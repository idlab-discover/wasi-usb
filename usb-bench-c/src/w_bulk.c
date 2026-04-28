/*
 * w_bulk.c — W-bulk benchmark: SCSI READ/WRITE throughput via libusb.
 *
 * Thesis conditions served by this single source file:
 *   C1  native-libusb   compiled for host with system libusb
 *   C2  wasi-libusb     compiled for wasm32-wasip2 with libusb-wasi.a
 *
 * The "one source, two targets" claim: the only difference between C1 and C2
 * is the build system (CMake toolchain). This file includes <libusb.h> and
 * calls standard libusb_* functions — the WASI variant links against
 * libusb-wasi.a which internally uses the component:usb WIT interface.
 *
 * Usage:
 *   w_bulk [output.csv] [VID:PID] [iterations] [blocks_per_iter]
 *          [--condition <name>] [--mode read|write|rw] [--random]
 *
 * Defaults:
 *   output.csv      bench_w_bulk.csv
 *   VID:PID         0951:1666  (Kingston DataTraveler)
 *   iterations      100
 *   blocks_per_iter 128   (64 KiB @ 512 B/block)
 *   condition       BENCH_CONDITION macro (set by CMake)
 *   mode            read  (safe default, no writes)
 *   random          0     (always start from LBA 0)
 *
 * Transfer modes:
 *   read   — SCSI READ(10) only (default, backward compatible)
 *   write  — SCSI WRITE(10) only; writes a fixed 0x5A pattern
 *            ⚠ overwrites data at LBA 0..blocks_per_iter; use a test device
 *   rw     — SCSI READ(10) then WRITE(10) same data back (safe,
 *            measures combined RTT, data unchanged)
 *
 * Random I/O:
 *   --random picks a uniformly distributed start LBA per iteration instead of
 *   always reading from LBA 0, exercising seek overhead in the flash controller.
 */

#include "csv.h"

/* ── Same include for C1 and C2 ─────────────────────────────────────────── */
#include <libusb.h>

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* ---------------------------------------------------------------------------
 * Compile-time defaults (overridden by CMake via -DBENCH_CONDITION=...)
 * ------------------------------------------------------------------------- */
#ifndef BENCH_CONDITION
#  define BENCH_CONDITION "native-libusb"
#endif

#define DEFAULT_VID         0x0951u
#define DEFAULT_PID         0x1666u
#define DEFAULT_ITERATIONS  100
#define DEFAULT_BLOCKS      128    /* 64 KiB @ 512 B/block */
#define TIMEOUT_MS          10000  /* libusb transfer timeout */

/* ---------------------------------------------------------------------------
 * Bulk-Only-Transport (BOT) helpers
 *
 * These implement §5 of the "Universal Serial Bus Mass Storage Class –
 * Bulk Only Transport" specification (revision 1.0).
 * ------------------------------------------------------------------------- */

static uint32_t g_bot_tag = 1;

static uint32_t bot_next_tag(void)
{
    return g_bot_tag++;
}

/**
 * bot_send_cbw — build and write a 31-byte Command Block Wrapper.
 * Returns 0 on success, -1 on error.
 */
static int bot_send_cbw(libusb_device_handle *h,
                        uint8_t              ep_out,
                        uint32_t             tag,
                        uint32_t             data_len,
                        int                  dir_in,
                        const uint8_t       *cbwcb,
                        uint8_t              cbwcb_len)
{
    uint8_t cbw[31];
    memset(cbw, 0, sizeof(cbw));

    /* dCBWSignature = 'USBC' (0x43425355 little-endian) */
    cbw[0] = 0x55; cbw[1] = 0x53; cbw[2] = 0x42; cbw[3] = 0x43;

    /* dCBWTag (little-endian) */
    cbw[4] = (uint8_t)(tag);
    cbw[5] = (uint8_t)(tag >> 8);
    cbw[6] = (uint8_t)(tag >> 16);
    cbw[7] = (uint8_t)(tag >> 24);

    /* dCBWDataTransferLength (little-endian) */
    cbw[8]  = (uint8_t)(data_len);
    cbw[9]  = (uint8_t)(data_len >> 8);
    cbw[10] = (uint8_t)(data_len >> 16);
    cbw[11] = (uint8_t)(data_len >> 24);

    /* bmCBWFlags: bit7=1 → IN, bit7=0 → OUT */
    cbw[12] = dir_in ? 0x80 : 0x00;
    cbw[13] = 0;             /* bCBWLUN = 0 */
    cbw[14] = cbwcb_len;     /* bCBWCBLength */
    memcpy(&cbw[15], cbwcb, cbwcb_len);

    int transferred = 0;
    int r = libusb_bulk_transfer(h, ep_out, cbw, 31, &transferred, TIMEOUT_MS);
    if (r != 0 || transferred != 31) {
        fprintf(stderr, "CBW write failed: r=%d transferred=%d\n", r, transferred);
        return -1;
    }
    return 0;
}

/**
 * bot_recv_csw — read and validate the 13-byte Command Status Wrapper.
 * Returns 0 on success, -1 on error.
 */
static int bot_recv_csw(libusb_device_handle *h,
                        uint8_t               ep_in,
                        uint32_t              expected_tag)
{
    uint8_t csw[13];
    int transferred = 0, total = 0;

    while (total < 13) {
        int r = libusb_bulk_transfer(h, ep_in,
                                     csw + total, 13 - total,
                                     &transferred, TIMEOUT_MS);
        if (r != 0 || transferred == 0) {
            fprintf(stderr, "CSW read failed: r=%d transferred=%d total=%d\n",
                    r, transferred, total);
            return -1;
        }
        total += transferred;
    }

    uint32_t sig = (uint32_t)csw[0]
                 | ((uint32_t)csw[1] << 8)
                 | ((uint32_t)csw[2] << 16)
                 | ((uint32_t)csw[3] << 24);
    if (sig != 0x53425355u) {
        fprintf(stderr, "Invalid CSW signature: 0x%08x\n", sig);
        return -1;
    }

    uint32_t tag = (uint32_t)csw[4]
                 | ((uint32_t)csw[5] << 8)
                 | ((uint32_t)csw[6] << 16)
                 | ((uint32_t)csw[7] << 24);
    if (tag != expected_tag) {
        fprintf(stderr, "CSW tag mismatch: got %u expected %u\n", tag, expected_tag);
        return -1;
    }

    if (csw[12] != 0) {
        fprintf(stderr, "CSW failure status: %u\n", csw[12]);
        return -1;
    }

    return 0;
}

/* ---------------------------------------------------------------------------
 * SCSI commands
 * ------------------------------------------------------------------------- */

/**
 * scsi_test_unit_ready — SCSI TEST UNIT READY (0x00).
 * Returns 0 on success, -1 on failure.
 */
static int scsi_test_unit_ready(libusb_device_handle *h,
                                 uint8_t ep_in,
                                 uint8_t ep_out)
{
    uint8_t cdb[6] = {0x00, 0x00, 0x00, 0x00, 0x00, 0x00};
    uint32_t tag = bot_next_tag();
    if (bot_send_cbw(h, ep_out, tag, 0, 0, cdb, 6) != 0) return -1;
    return bot_recv_csw(h, ep_in, tag);
}

/**
 * scsi_read_capacity — SCSI READ CAPACITY (10) (0x25).
 * Fills *block_size_out and *num_blocks_out.
 * Returns 0 on success, -1 on failure.
 */
static int scsi_read_capacity(libusb_device_handle *h,
                               uint8_t               ep_in,
                               uint8_t               ep_out,
                               uint32_t             *block_size_out,
                               uint32_t             *num_blocks_out)
{
    uint8_t cdb[10] = {0x25, 0x00, 0x00, 0x00, 0x00,
                        0x00, 0x00, 0x00, 0x00, 0x00};
    uint32_t tag = bot_next_tag();

    if (bot_send_cbw(h, ep_out, tag, 8, 1, cdb, 10) != 0) return -1;

    uint8_t data[8];
    int transferred = 0;
    int r = libusb_bulk_transfer(h, ep_in, data, 8, &transferred, TIMEOUT_MS);
    if (r != 0 || transferred < 8) {
        fprintf(stderr, "READ CAPACITY data read failed: r=%d transferred=%d\n",
                r, transferred);
        return -1;
    }

    if (bot_recv_csw(h, ep_in, tag) != 0) return -1;

    uint32_t last_lba  = ((uint32_t)data[0] << 24) | ((uint32_t)data[1] << 16)
                       | ((uint32_t)data[2] <<  8) |  (uint32_t)data[3];
    uint32_t block_len = ((uint32_t)data[4] << 24) | ((uint32_t)data[5] << 16)
                       | ((uint32_t)data[6] <<  8) |  (uint32_t)data[7];

    if (block_size_out)  *block_size_out  = block_len;
    if (num_blocks_out)  *num_blocks_out  = last_lba + 1u;
    return 0;
}

/**
 * scsi_read10 — SCSI READ (10) (0x28).
 * Reads `count` logical blocks starting at `lba` into `buf`.
 * `buf` must be at least count * block_size bytes.
 * Returns 0 on success, -1 on failure.
 */
static int scsi_read10(libusb_device_handle *h,
                        uint8_t               ep_in,
                        uint8_t               ep_out,
                        uint32_t              lba,
                        uint16_t              count,
                        uint32_t              block_size,
                        uint8_t              *buf)
{
    uint32_t data_len = (uint32_t)count * block_size;

    /* READ (10) CDB — SBC-3 §5.8 */
    uint8_t cdb[10] = {
        0x28,                                           /* OPERATION CODE   */
        0x00,                                           /* flags            */
        (uint8_t)(lba >> 24), (uint8_t)(lba >> 16),   /* LBA (MSB..LSB)   */
        (uint8_t)(lba >>  8), (uint8_t)(lba),
        0x00,                                           /* GROUP NUMBER     */
        (uint8_t)(count >> 8), (uint8_t)(count),       /* TRANSFER LENGTH  */
        0x00                                            /* CONTROL          */
    };

    uint32_t tag = bot_next_tag();
    if (bot_send_cbw(h, ep_out, tag, data_len, 1, cdb, 10) != 0) return -1;

    /* Read data in chunks until all data_len bytes are received */
    uint32_t total = 0;
    while (total < data_len) {
        int transferred = 0;
        int r = libusb_bulk_transfer(h, ep_in,
                                     buf + total,
                                     (int)(data_len - total),
                                     &transferred,
                                     TIMEOUT_MS);
        if (r != 0 || transferred == 0) {
            fprintf(stderr, "READ(10) data chunk failed: r=%d transferred=%d\n",
                    r, transferred);
            return -1;
        }
        total += (uint32_t)transferred;
    }

    return bot_recv_csw(h, ep_in, tag);
}

/**
 * scsi_write10 — SCSI WRITE (10) (0x2A).
 * Writes `count` logical blocks starting at `lba` from `buf`.
 * `buf` must be exactly count * block_size bytes.
 * ⚠ Overwrites on-device data; use on a dedicated test device.
 * Returns 0 on success, -1 on failure.
 */
static int scsi_write10(libusb_device_handle *h,
                         uint8_t               ep_in,
                         uint8_t               ep_out,
                         uint32_t              lba,
                         uint16_t              count,
                         uint32_t              block_size,
                         const uint8_t        *buf)
{
    uint32_t data_len = (uint32_t)count * block_size;

    /* WRITE (10) CDB — SBC-3 §5.26 */
    uint8_t cdb[10] = {
        0x2A,                                            /* OPERATION CODE  */
        0x00,                                            /* flags           */
        (uint8_t)(lba >> 24), (uint8_t)(lba >> 16),    /* LBA (MSB..LSB)  */
        (uint8_t)(lba >>  8), (uint8_t)(lba),
        0x00,                                            /* GROUP NUMBER    */
        (uint8_t)(count >> 8), (uint8_t)(count),        /* TRANSFER LENGTH */
        0x00                                             /* CONTROL         */
    };

    uint32_t tag = bot_next_tag();
    /* dir_in = 0 → OUT direction */
    if (bot_send_cbw(h, ep_out, tag, data_len, 0, cdb, 10) != 0) return -1;

    /* Send data in one bulk-OUT transfer */
    int transferred = 0;
    int r = libusb_bulk_transfer(h, ep_out, (uint8_t *)buf, (int)data_len,
                                  &transferred, TIMEOUT_MS);
    if (r != 0 || (uint32_t)transferred != data_len) {
        fprintf(stderr, "WRITE(10) data phase failed: r=%d transferred=%d\n",
                r, transferred);
        return -1;
    }

    return bot_recv_csw(h, ep_in, tag);
}

/* ---------------------------------------------------------------------------
 * Bulk transfer mode
 * ------------------------------------------------------------------------- */
typedef enum { MODE_READ = 0, MODE_WRITE = 1, MODE_RW = 2 } bulk_mode_t;

/* ---------------------------------------------------------------------------
 * Device enumeration
 * ------------------------------------------------------------------------- */

/**
 * Open the first device matching VID:PID that has a mass-storage BOT
 * interface (class=0x08, protocol=0x50).  Sets ep_in/ep_out/iface_num.
 * Returns a claimed device handle, or NULL on failure.
 */
static libusb_device_handle *open_mass_storage(libusb_context *ctx,
                                                uint16_t        vid,
                                                uint16_t        pid,
                                                uint8_t        *ep_in_out,
                                                uint8_t        *ep_out_out,
                                                uint8_t        *iface_out)
{
    libusb_device **devlist = NULL;
    ssize_t ndev = libusb_get_device_list(ctx, &devlist);
    if (ndev < 0) {
        fprintf(stderr, "libusb_get_device_list failed: %s\n",
                libusb_error_name((int)ndev));
        return NULL;
    }

    libusb_device_handle *handle = NULL;

    for (ssize_t i = 0; i < ndev && !handle; i++) {
        struct libusb_device_descriptor ddesc;
        if (libusb_get_device_descriptor(devlist[i], &ddesc) != 0) continue;
        if (ddesc.idVendor != vid || ddesc.idProduct != pid) continue;

        struct libusb_config_descriptor *cfg = NULL;
        if (libusb_get_active_config_descriptor(devlist[i], &cfg) != 0) continue;

        uint8_t iface_num = 0;
        uint8_t ep_in  = 0;
        uint8_t ep_out = 0;
        int found_iface = 0;

        for (int j = 0; j < cfg->bNumInterfaces && !found_iface; j++) {
            const struct libusb_interface_descriptor *idesc =
                &cfg->interface[j].altsetting[0];

            if (idesc->bInterfaceClass    != 0x08 ||
                idesc->bInterfaceProtocol != 0x50) continue;

            iface_num = idesc->bInterfaceNumber;
            for (int k = 0; k < idesc->bNumEndpoints; k++) {
                const struct libusb_endpoint_descriptor *ep = &idesc->endpoint[k];
                if ((ep->bmAttributes & LIBUSB_TRANSFER_TYPE_MASK)
                        != LIBUSB_TRANSFER_TYPE_BULK) continue;

                if (ep->bEndpointAddress & LIBUSB_ENDPOINT_IN)
                    ep_in  = ep->bEndpointAddress;
                else
                    ep_out = ep->bEndpointAddress;
            }
            found_iface = 1;
        }

        libusb_free_config_descriptor(cfg);
        if (!found_iface) continue;

        if (libusb_open(devlist[i], &handle) != 0) { handle = NULL; continue; }

        libusb_set_auto_detach_kernel_driver(handle, 1);

        if (libusb_claim_interface(handle, iface_num) != 0) {
            fprintf(stderr, "claim_interface(%u) failed\n", iface_num);
            libusb_close(handle);
            handle = NULL;
            continue;
        }

        *ep_in_out  = ep_in;
        *ep_out_out = ep_out;
        *iface_out  = iface_num;
    }

    libusb_free_device_list(devlist, 1);
    return handle;
}

/* ---------------------------------------------------------------------------
 * main
 *
 * Same entry point for native (C1) and wasm32-wasip2 (C2).
 * The wasm build's standard-library adapter maps main() to wasi:cli/run.
 * ------------------------------------------------------------------------- */
int main(int argc, char **argv)
{
    /* ── Parse arguments ─────────────────────────────────────────────────── */
    const char *output     = (argc > 1) ? argv[1] : "bench_w_bulk.csv";
    const char *vid_pid_s  = (argc > 2 && argv[2][0] != '-') ? argv[2] : "0951:1666";
    int         iters      = (argc > 3 && argv[3][0] != '-') ? atoi(argv[3]) : DEFAULT_ITERATIONS;
    int         blocks     = (argc > 4 && argv[4][0] != '-') ? atoi(argv[4]) : DEFAULT_BLOCKS;
    const char *condition  = BENCH_CONDITION; /* overridden below if provided */
    bulk_mode_t mode       = MODE_READ;
    int         random_lba = 0;

    /* Parse VID:PID (hex, colon-separated) */
    uint16_t vid = DEFAULT_VID;
    uint16_t pid = DEFAULT_PID;
    {
        const char *colon = strchr(vid_pid_s, ':');
        if (!colon) {
            fprintf(stderr, "expected VID:PID in hex, e.g. 0951:1666\n");
            return 1;
        }
        vid = (uint16_t)strtoul(vid_pid_s, NULL, 16);
        pid = (uint16_t)strtoul(colon + 1,  NULL, 16);
    }

    for (int i = 1; i < argc; i++) {
        if (i + 1 < argc && strcmp(argv[i], "--condition") == 0) {
            condition = argv[i + 1];
        }
        if (i + 1 < argc && strcmp(argv[i], "--mode") == 0) {
            if      (strcmp(argv[i+1], "write") == 0) mode = MODE_WRITE;
            else if (strcmp(argv[i+1], "rw")    == 0) mode = MODE_RW;
            /* else: keep MODE_READ */
        }
        if (strcmp(argv[i], "--random") == 0) {
            random_lba = 1;
        }
    }

    if (iters <= 0 || blocks <= 0) {
        fprintf(stderr, "iterations and blocks_per_iter must be > 0\n");
        return 1;
    }

    /* Seed RNG for random LBA selection */
    srand((unsigned int)time(NULL));

    /* ── Open CSV ────────────────────────────────────────────────────────── */
    FILE *csv = bench_csv_open(output);
    if (!csv) { perror("bench_csv_open"); return 1; }

    /* ── libusb init ─────────────────────────────────────────────────────── */
    libusb_context *ctx = NULL;
    if (libusb_init(&ctx) != 0) {
        fprintf(stderr, "libusb_init failed\n");
        bench_csv_close(csv);
        return 1;
    }

    /* ── Find and open device ────────────────────────────────────────────── */
    uint8_t ep_in = 0, ep_out = 0, iface_num = 0;
    libusb_device_handle *handle = open_mass_storage(ctx, vid, pid,
                                                      &ep_in, &ep_out,
                                                      &iface_num);
    if (!handle) {
        fprintf(stderr, "Device %04x:%04x not found or failed to open\n", vid, pid);
        libusb_exit(ctx);
        bench_csv_close(csv);
        return 1;
    }

    /* ── BOT Reset ───────────────────────────────────────────────────────── */
    /* Class-specific control transfer — ignore errors, some devices omit it */
    unsigned char dummy[1] = {0};
    libusb_control_transfer(handle,
        LIBUSB_REQUEST_TYPE_CLASS | LIBUSB_RECIPIENT_INTERFACE,
        0xFF,         /* BOT Bulk-Only Mass Storage Reset */
        0,            /* wValue */
        iface_num,    /* wIndex = interface number */
        dummy, 0,     /* no data */
        1000);

    /* ── TEST UNIT READY ─────────────────────────────────────────────────── */
    if (scsi_test_unit_ready(handle, ep_in, ep_out) != 0) {
        fprintf(stderr, "TEST UNIT READY failed\n");
        /* Non-fatal — some drives accept commands anyway */
    }

    /* ── READ CAPACITY ───────────────────────────────────────────────────── */
    uint32_t block_size = 0, num_blocks = 0;
    if (scsi_read_capacity(handle, ep_in, ep_out, &block_size, &num_blocks) != 0) {
        fprintf(stderr, "READ CAPACITY failed\n");
        libusb_release_interface(handle, iface_num);
        libusb_close(handle);
        libusb_exit(ctx);
        bench_csv_close(csv);
        return 1;
    }

    uint64_t bytes_per_iter = (uint64_t)blocks * block_size;
    uint8_t *buf = malloc(bytes_per_iter);
    if (!buf) {
        perror("malloc");
        libusb_release_interface(handle, iface_num);
        libusb_close(handle);
        libusb_exit(ctx);
        bench_csv_close(csv);
        return 1;
    }

    static const char *mode_names[] = {"read", "write", "rw"};
    fprintf(stderr,
            "▶ %04x:%04x  block_size=%u B  num_blocks=%u  "
            "%d×%d blocks = %" PRIu64 " KiB each\n",
            vid, pid, block_size, num_blocks,
            iters, blocks, bytes_per_iter / 1024);
    fprintf(stderr,
            "  condition=%s  workload=bulk  mode=%s  random=%d  "
            "iterations=%d  → %s\n",
            condition, mode_names[mode], random_lba, iters, output);

    /* Pre-built write buffer: fixed 0x5A pattern */
    memset(buf, 0x5A, bytes_per_iter);

    /* ── Measurement loop ────────────────────────────────────────────────── */
    uint64_t total_bytes = 0;
    uint64_t wall_start  = bench_now_ns();

    for (int iter = 0; iter < iters; iter++) {
        /* Choose LBA: sequential or random */
        uint32_t lba = 0;
        if (random_lba && num_blocks > (uint32_t)blocks) {
            uint32_t range = num_blocks - (uint32_t)blocks;
            lba = (uint32_t)((uint64_t)rand() * range / ((uint64_t)RAND_MAX + 1));
        }

        bench_rusage_t ru_before, ru_after;
        bench_rusage_now(&ru_before);

        /* ── timed section ── */
        uint64_t t0 = bench_now_ns();
        int rc;
        if (mode == MODE_READ) {
            rc = scsi_read10(handle, ep_in, ep_out, lba,
                              (uint16_t)blocks, block_size, buf);
        } else if (mode == MODE_WRITE) {
            rc = scsi_write10(handle, ep_in, ep_out, lba,
                               (uint16_t)blocks, block_size, buf);
        } else { /* MODE_RW */
            rc = scsi_read10(handle, ep_in, ep_out, lba,
                              (uint16_t)blocks, block_size, buf);
            if (rc == 0)
                rc = scsi_write10(handle, ep_in, ep_out, lba,
                                   (uint16_t)blocks, block_size, buf);
        }
        uint64_t duration_ns = bench_now_ns() - t0;
        /* ── end timed section ── */

        bench_rusage_now(&ru_after);

        if (rc != 0) {
            fprintf(stderr, "%s failed at iteration %d — aborting\n",
                    mode == MODE_WRITE ? "WRITE(10)" : "READ(10)", iter);
            break;
        }

        total_bytes += bytes_per_iter;

        /* Build notes: mode + lba */
        char notes_buf[64];
        snprintf(notes_buf, sizeof(notes_buf),
                 "mode=%s lba=%u", mode_names[mode], lba);

        uint64_t gmem = bench_guest_mem_bytes();
        bench_row_t row = {
            .condition       = condition,
            .workload        = "bulk",
            .iteration       = (uint32_t)iter,
            .bytes           = bytes_per_iter,
            .duration_ns     = duration_ns,
            .user_cpu_us     = ru_after.user_us  - ru_before.user_us,
            .sys_cpu_us      = ru_after.sys_us   - ru_before.sys_us,
            .rss_peak_kb     = ru_after.rss_peak_kb,
            .guest_mem_bytes = gmem,
            .has_guest_mem   = (gmem != 0),
            .checksum_hex    = NULL,
            .notes           = notes_buf,
        };
        bench_csv_write(csv, &row);

        if (iter % 10 == 0 || iter == iters - 1) {
            double mb_s = (bytes_per_iter / 1048576.0) / (duration_ns / 1e9);
            fprintf(stderr, "  [%3d/%d]  %6.1f MB/s  RTT=%" PRIu64 " µs  lba=%u\n",
                    iter, iters, mb_s, duration_ns / 1000u, lba);
        }
    }

    /* ── Summary ─────────────────────────────────────────────────────────── */
    uint64_t wall_ns  = bench_now_ns() - wall_start;
    double   avg_mb_s = (total_bytes / 1048576.0) / (wall_ns / 1e9);
    fprintf(stderr,
            "✓ Done  total=%.1f MiB  avg=%.1f MB/s  → %s\n",
            total_bytes / 1048576.0, avg_mb_s, output);

    /* ── Cleanup ─────────────────────────────────────────────────────────── */
    free(buf);
    libusb_release_interface(handle, iface_num);
    libusb_close(handle);
    libusb_exit(ctx);
    bench_csv_close(csv);
    return 0;
}

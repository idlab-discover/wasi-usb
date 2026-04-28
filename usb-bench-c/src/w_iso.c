/*
 * w_iso.c — W-iso benchmark: isochronous UVC streaming on a Logitech Brio.
 *
 * Activates the UVC video-streaming interface (alt-setting 1), then issues
 * a stream of isochronous IN transfers and records the per-transfer RTT and
 * cumulative throughput.
 *
 * Thesis conditions served by this single source file:
 *   C1  native-libusb   compiled for host with system libusb
 *   C2  wasi-libusb     compiled for wasm32-wasip2 with libusb-wasi.a
 *
 * Usage:
 *   w_iso [output.csv] [VID:PID] [iterations]
 *         [--iface N] [--alt N] [--ep 0xNN] [--pkt-size N] [--num-pkts N]
 *         [--condition <name>]
 *
 * Defaults:
 *   output.csv   bench_w_iso.csv
 *   VID:PID      046d:086c  (Logitech Brio 4K)
 *   iterations   200
 *   iface        1
 *   alt          1
 *   ep           0x81
 *   pkt-size     1024
 *   num-pkts     32
 *   condition    BENCH_CONDITION macro ("native-libusb" or "wasi-libusb")
 *
 * Isochronous transfers in libusb are asynchronous.  Each iteration:
 *   1. Allocate a transfer with num_pkts ISO packets.
 *   2. Fill and submit it.
 *   3. Run the event loop until the completion callback fires.
 *   4. Sum the actual_length fields across packets.
 *   5. Record RTT, bytes, and resource usage.
 */

#include "csv.h"

#include <libusb.h>

#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* ── Compile-time defaults ───────────────────────────────────────────────── */
#ifndef BENCH_CONDITION
#  define BENCH_CONDITION "native-libusb"
#endif

#define DEFAULT_OUTPUT     "bench_w_iso.csv"
#define DEFAULT_VID        0x046du   /* Logitech */
#define DEFAULT_PID        0x086cu   /* Brio 4K  */
#define DEFAULT_ITERATIONS 200u
#define DEFAULT_IFACE      1u
#define DEFAULT_ALT        1u
#define DEFAULT_EP         0x81u
#define DEFAULT_PKT_SIZE   1024u
#define DEFAULT_NUM_PKTS   32u

/* ── ISO completion callback ────────────────────────────────────────────── */
/*
 * The callback is called from libusb_handle_events_completed().
 * We store a pointer to a volatile int "completed" flag in user_data.
 * Setting it to 1 makes the event loop in main() exit.
 */
static void LIBUSB_CALL iso_callback(struct libusb_transfer *xfer)
{
    volatile int *completed = (volatile int *)xfer->user_data;
    *completed = 1;
}

/* ── Helpers ─────────────────────────────────────────────────────────────── */

static void die(const char *msg)
{
    fprintf(stderr, "fatal: %s\n", msg);
    exit(1);
}

static void parse_vid_pid(const char *s, uint16_t *vid, uint16_t *pid)
{
    unsigned v = 0, p = 0;
    if (sscanf(s, "%x:%x", &v, &p) != 2)
        die("expected VID:PID in hex, e.g. 046d:086c");
    *vid = (uint16_t)v;
    *pid = (uint16_t)p;
}

static int parse_uint_arg(const char *s)
{
    char *end = NULL;
    long v = strtol(s, &end, 0); /* base 0: accepts 0x prefix */
    return (end != s && v >= 0) ? (int)v : -1;
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(int argc, char **argv)
{
    const char *output     = (argc > 1) ? argv[1] : DEFAULT_OUTPUT;
    const char *vid_pid    = (argc > 2) ? argv[2] : "046d:086c";
    uint32_t    iterations = (argc > 3) ? (uint32_t)atoi(argv[3]) : DEFAULT_ITERATIONS;
    const char *condition  = BENCH_CONDITION;

    uint8_t  iface    = DEFAULT_IFACE;
    uint8_t  alt      = DEFAULT_ALT;
    uint8_t  ep       = DEFAULT_EP;
    uint32_t pkt_size = DEFAULT_PKT_SIZE;
    uint32_t num_pkts = DEFAULT_NUM_PKTS;

    for (int i = 1; i < argc; i++) {
        if (i + 1 < argc) {
            int v = parse_uint_arg(argv[i + 1]);
            if      (strcmp(argv[i], "--iface")     == 0 && v >= 0) iface    = (uint8_t)v;
            else if (strcmp(argv[i], "--alt")       == 0 && v >= 0) alt      = (uint8_t)v;
            else if (strcmp(argv[i], "--ep")        == 0 && v >= 0) ep       = (uint8_t)v;
            else if (strcmp(argv[i], "--pkt-size")  == 0 && v >= 0) pkt_size = (uint32_t)v;
            else if (strcmp(argv[i], "--num-pkts")  == 0 && v >= 0) num_pkts = (uint32_t)v;
            else if (strcmp(argv[i], "--condition") == 0) condition = argv[i + 1];
        }
    }

    uint16_t vid, pid;
    parse_vid_pid(vid_pid, &vid, &pid);

    uint32_t buf_size = pkt_size * num_pkts;

    /* ── libusb init ─────────────────────────────────────────────────────── */
    libusb_context *ctx = NULL;
    if (libusb_init(&ctx) != 0)
        die("libusb_init failed");

    libusb_device_handle *handle =
        libusb_open_device_with_vid_pid(ctx, vid, pid);
    if (!handle) {
        fprintf(stderr, "fatal: device %04x:%04x not found\n", vid, pid);
        libusb_exit(ctx);
        return 1;
    }

    /* Detach kernel UVC driver if present (Linux) */
    if (libusb_kernel_driver_active(handle, iface) == 1)
        libusb_detach_kernel_driver(handle, iface);

    /* Claim the video-streaming interface at alt-setting 0 first */
    int rc = libusb_claim_interface(handle, iface);
    if (rc != 0) {
        fprintf(stderr, "fatal: libusb_claim_interface(%u): %s\n",
                iface, libusb_error_name(rc));
        libusb_close(handle);
        libusb_exit(ctx);
        return 1;
    }

    /* Switch to the alt-setting that has ISO endpoints (alt >= 1) */
    rc = libusb_set_interface_alt_setting(handle, iface, alt);
    if (rc != 0) {
        fprintf(stderr, "fatal: libusb_set_interface_alt_setting(%u,%u): %s\n",
                iface, alt, libusb_error_name(rc));
        libusb_release_interface(handle, iface);
        libusb_close(handle);
        libusb_exit(ctx);
        return 1;
    }

    /* ── CSV logger ─────────────────────────────────────────────────────── */
    FILE *csv = bench_csv_open(output);
    if (!csv)
        die("bench_csv_open failed");

    fprintf(stderr,
            "▶ %04x:%04x  workload=iso  iface=%u  alt=%u  ep=0x%02x  "
            "pkt=%u B  num_pkts=%u  buf=%u B\n",
            vid, pid, iface, alt, ep, pkt_size, num_pkts, buf_size);
    fprintf(stderr,
            "  iterations=%u  condition=%s  → %s\n",
            iterations, condition, output);

    /* ── Allocate ISO transfer and data buffer ───────────────────────────── */
    uint8_t *buf = (uint8_t *)malloc(buf_size);
    if (!buf)
        die("malloc failed");

    struct libusb_transfer *xfer =
        libusb_alloc_transfer((int)num_pkts);
    if (!xfer)
        die("libusb_alloc_transfer failed");

    /* ── Benchmark loop ──────────────────────────────────────────────────── */
    uint64_t total_bytes = 0;

    for (uint32_t iter = 0; iter < iterations; ++iter) {
        bench_rusage_t ru_before, ru_after;
        bench_rusage_now(&ru_before);

        /* Arm the transfer */
        volatile int completed = 0;
        libusb_fill_iso_transfer(
            xfer,
            handle,
            ep,
            buf,
            (int)buf_size,
            (int)num_pkts,
            iso_callback,
            (void *)&completed,
            5000 /* timeout ms */
        );
        libusb_set_iso_packet_lengths(xfer, pkt_size);

        /* ── timed section ─────────────────────────────────────────────── */
        uint64_t t0 = bench_now_ns();

        rc = libusb_submit_transfer(xfer);
        if (rc != 0) {
            fprintf(stderr,
                    "  [%4u/%u] libusb_submit_transfer: %s\n",
                    iter, iterations, libusb_error_name(rc));
            continue;
        }

        /* Pump the event loop until the callback sets completed = 1 */
        while (!completed) {
            rc = libusb_handle_events_completed(ctx, (int *)&completed);
            if (rc != 0 && rc != LIBUSB_ERROR_INTERRUPTED) {
                fprintf(stderr,
                        "  [%4u/%u] libusb_handle_events_completed: %s\n",
                        iter, iterations, libusb_error_name(rc));
                break;
            }
        }

        uint64_t duration_ns = bench_now_ns() - t0;
        /* ── end timed section ─────────────────────────────────────────── */

        bench_rusage_now(&ru_after);

        /* Check transfer status */
        if (xfer->status != LIBUSB_TRANSFER_COMPLETED &&
            xfer->status != LIBUSB_TRANSFER_TIMED_OUT) {
            fprintf(stderr,
                    "  [%4u/%u] transfer status: %d\n",
                    iter, iterations, (int)xfer->status);
            continue;
        }

        /* Sum actual bytes received across all ISO packets */
        uint64_t bytes_received = 0;
        for (uint32_t p = 0; p < num_pkts; ++p) {
            struct libusb_iso_packet_descriptor *pkt =
                &xfer->iso_packet_desc[p];
            if (pkt->status == LIBUSB_TRANSFER_COMPLETED)
                bytes_received += pkt->actual_length;
        }
        total_bytes += bytes_received;

        bench_row_t row = {0};
        row.condition    = condition;
        row.workload     = "iso";
        row.iteration    = iter;
        row.bytes        = bytes_received;
        row.duration_ns  = duration_ns;
        row.user_cpu_us  = ru_after.user_us  - ru_before.user_us;
        row.sys_cpu_us   = ru_after.sys_us   - ru_before.sys_us;
        row.rss_peak_kb  = ru_after.rss_peak_kb;
        row.guest_mem_bytes = bench_guest_mem_bytes();
        row.has_guest_mem   = 0;
#ifdef __wasm__
        row.has_guest_mem   = 1;
#endif
        bench_csv_write(csv, &row);

        if (iter % 20 == 0 || iter == iterations - 1) {
            double mb_s = (bytes_received / 1048576.0) / (duration_ns / 1e9);
            fprintf(stderr,
                    "  [%4u/%u]  bytes=%" PRIu64 "  RTT=%llu µs  "
                    "tput=%.1f MB/s\n",
                    iter, iterations,
                    bytes_received,
                    (unsigned long long)(duration_ns / 1000u),
                    mb_s);
        }
    }

    /* ── Summary ─────────────────────────────────────────────────────────── */
    fprintf(stderr,
            "✓ Done  iterations=%u  total=%.1f MiB  → %s\n",
            iterations, total_bytes / 1048576.0, output);

    /* ── Cleanup ─────────────────────────────────────────────────────────── */
    libusb_free_transfer(xfer);
    free(buf);

    /* Switch back to alt-setting 0 (releases ISO bandwidth) */
    libusb_set_interface_alt_setting(handle, iface, 0);
    libusb_release_interface(handle, iface);
    libusb_close(handle);
    libusb_exit(ctx);
    bench_csv_close(csv);

    return 0;
}

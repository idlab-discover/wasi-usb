/*
 * w_int.c — W-int benchmark: USB interrupt-transfer RTT on a PS5 DualSense.
 *
 * Polls the interrupt-IN endpoint (0x81) of interface 0 and records the
 * host-side RTT (nanoseconds) for each HID input report.
 *
 * Thesis conditions served by this single source file:
 *   C1  native-libusb   compiled for host with system libusb
 *   C2  wasi-libusb     compiled for wasm32-wasip2 with libusb-wasi.a
 *
 * Usage:
 *   w_int [output.csv] [VID:PID] [iterations] [--condition <name>]
 *
 * Defaults:
 *   output.csv   bench_w_int.csv
 *   VID:PID      054c:0ce6  (PS5 DualSense)
 *   iterations   1000
 *   condition    BENCH_CONDITION macro ("native-libusb" or "wasi-libusb")
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

#define DEFAULT_OUTPUT     "bench_w_int.csv"
#define DEFAULT_VID        0x054cu   /* Sony */
#define DEFAULT_PID        0x0ce6u   /* PS5 DualSense */
#define DEFAULT_ITERATIONS 1000u

/* HID interface/endpoint geometry for the PS5 DualSense */
#define IFACE              0u
#define EP_IN              0x81u     /* interrupt IN */
#define PKT_SIZE           64u       /* max packet size for this endpoint */
#define TIMEOUT_MS         500u

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
        die("expected VID:PID in hex, e.g. 054c:0ce6");
    *vid = (uint16_t)v;
    *pid = (uint16_t)p;
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(int argc, char **argv)
{
    const char *output     = (argc > 1) ? argv[1] : DEFAULT_OUTPUT;
    const char *vid_pid    = (argc > 2) ? argv[2] : "054c:0ce6";
    uint32_t    iterations = (argc > 3) ? (uint32_t)atoi(argv[3]) : DEFAULT_ITERATIONS;
    const char *condition  = BENCH_CONDITION;

    for (int i = 1; i < argc - 1; i++) {
        if (strcmp(argv[i], "--condition") == 0)
            condition = argv[i + 1];
    }

    uint16_t vid, pid;
    parse_vid_pid(vid_pid, &vid, &pid);

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

    /* Detach the kernel HID driver if active (Linux) */
    if (libusb_kernel_driver_active(handle, IFACE) == 1) {
        int rc = libusb_detach_kernel_driver(handle, IFACE);
        if (rc != 0) {
            fprintf(stderr,
                    "warning: libusb_detach_kernel_driver: %s\n",
                    libusb_error_name(rc));
        }
    }

    int rc = libusb_claim_interface(handle, IFACE);
    if (rc != 0) {
        fprintf(stderr, "fatal: libusb_claim_interface(%u): %s\n",
                IFACE, libusb_error_name(rc));
        libusb_close(handle);
        libusb_exit(ctx);
        return 1;
    }

    /* ── CSV logger ─────────────────────────────────────────────────────── */
    FILE *csv = bench_csv_open(output);
    if (!csv)
        die("bench_csv_open failed");

    fprintf(stderr,
            "▶ %04x:%04x  workload=int  pkt=%u B  "
            "iterations=%u  condition=%s  → %s\n",
            vid, pid, PKT_SIZE, iterations, condition, output);

    /* ── Benchmark loop ──────────────────────────────────────────────────── */
    uint8_t buf[PKT_SIZE];

    for (uint32_t iter = 0; iter < iterations; ++iter) {
        bench_rusage_t ru_before, ru_after;
        bench_rusage_now(&ru_before);

        /* ── timed section ─────────────────────────────────────────────── */
        uint64_t t0 = bench_now_ns();

        int transferred = 0;
        int n = libusb_interrupt_transfer(
            handle,
            EP_IN,
            buf,
            (int)PKT_SIZE,
            &transferred,
            TIMEOUT_MS
        );

        uint64_t duration_ns = bench_now_ns() - t0;
        /* ── end timed section ─────────────────────────────────────────── */

        bench_rusage_now(&ru_after);

        if (n != 0) {
            fprintf(stderr,
                    "  [%4u/%u] libusb_interrupt_transfer error: %s\n",
                    iter, iterations, libusb_error_name(n));
            continue;
        }

        bench_row_t row = {0};
        row.condition    = condition;
        row.workload     = "int";
        row.iteration    = iter;
        row.bytes        = (uint64_t)transferred;
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

        if (iter % 100 == 0 || iter == iterations - 1) {
            fprintf(stderr,
                    "  [%4u/%u]  RTT=%llu µs  bytes=%d\n",
                    iter, iterations,
                    (unsigned long long)(duration_ns / 1000u),
                    transferred);
        }
    }

    /* ── Cleanup ─────────────────────────────────────────────────────────── */
    bench_csv_close(csv);
    libusb_release_interface(handle, IFACE);
    libusb_close(handle);
    libusb_exit(ctx);

    fprintf(stderr,
            "✓ Done  iterations=%u  → %s\n",
            iterations, output);
    return 0;
}

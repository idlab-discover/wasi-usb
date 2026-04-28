/*
 * w_ctrl.c — W-ctrl benchmark: USB control-transfer RTT on an Arduino Nano.
 *
 * Sends GET_DESCRIPTOR(Device) to the device N times and records the RTT
 * (nanoseconds) and resource usage for each transfer.
 *
 * Compiles identically for C1 (native-libusb) and C2 (wasi-libusb):
 *   C1: cmake -B build-native && cmake --build build-native
 *   C2: cmake -B build-wasi -DCMAKE_TOOLCHAIN_FILE=toolchain-wasi.cmake \
 *             && cmake --build build-wasi
 *
 * Usage:
 *   w_ctrl [output.csv] [VID:PID] [iterations]
 *   Defaults: bench_w_ctrl.csv  2341:8057  1000
 */

#include <libusb.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "csv.h"

/* ── Default parameters ──────────────────────────────────────────────────── */
#define DEFAULT_OUTPUT     "bench_w_ctrl.csv"
#define DEFAULT_VID        0x2341u   /* Arduino Nano 33 BLE */
#define DEFAULT_PID        0x8057u
#define DEFAULT_ITERATIONS 1000u

/* GET_DESCRIPTOR(Device): bmRequestType=0x80, bRequest=0x06, wValue=0x0100 */
#define GET_DESC_REQ_TYPE  0x80u
#define GET_DESC_REQUEST   0x06u
#define GET_DESC_VALUE     0x0100u
#define GET_DESC_INDEX     0u
#define DEVICE_DESC_LEN    18

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
        die("expected VID:PID in hex, e.g. 2341:8057");
    *vid = (uint16_t)v;
    *pid = (uint16_t)p;
}

/* ── Main ─────────────────────────────────────────────────────────────────── */

int main(int argc, char **argv)
{
    const char *output     = (argc > 1) ? argv[1] : DEFAULT_OUTPUT;
    const char *vid_pid    = (argc > 2) ? argv[2] : "2341:8057";
    uint32_t    iterations = (argc > 3) ? (uint32_t)atoi(argv[3]) : DEFAULT_ITERATIONS;

    uint16_t vid, pid;
    parse_vid_pid(vid_pid, &vid, &pid);

    /* ── libusb init ─────────────────────────────────────────────────────── */
    libusb_context *ctx = NULL;
    int rc = libusb_init(&ctx);
    if (rc != 0)
        die("libusb_init failed");

    libusb_device_handle *handle = libusb_open_device_with_vid_pid(ctx, vid, pid);
    if (!handle) {
        fprintf(stderr, "fatal: device %04x:%04x not found\n", vid, pid);
        libusb_exit(ctx);
        return 1;
    }

    /* ── CSV logger ─────────────────────────────────────────────────────── */
    FILE *csv = bench_csv_open(output);
    if (!csv)
        die("bench_csv_open failed");

    fprintf(stderr,
            "▶ %04x:%04x  workload=ctrl  iterations=%u  "
            "condition=" BENCH_CONDITION "  → %s\n",
            vid, pid, iterations, output);

    /* ── Benchmark loop ──────────────────────────────────────────────────── */
    uint8_t desc_buf[DEVICE_DESC_LEN];

    for (uint32_t iter = 0; iter < iterations; ++iter) {
        bench_rusage_t ru_before, ru_after;
        bench_rusage_now(&ru_before);

        /* ── timed section ─────────────────────────────────────────────── */
        uint64_t t0 = bench_now_ns();

        int n = libusb_control_transfer(
            handle,
            GET_DESC_REQ_TYPE,
            GET_DESC_REQUEST,
            GET_DESC_VALUE,
            GET_DESC_INDEX,
            desc_buf,
            (uint16_t)DEVICE_DESC_LEN,
            500 /* timeout ms */
        );

        uint64_t duration_ns = bench_now_ns() - t0;
        /* ── end timed section ─────────────────────────────────────────── */

        bench_rusage_now(&ru_after);

        if (n < 0) {
            fprintf(stderr,
                    "  [%4u/%u] libusb_control_transfer error: %s\n",
                    iter, iterations,
                    libusb_error_name(n));
            continue;
        }

        bench_row_t row = {0};
        row.condition    = BENCH_CONDITION;
        row.workload     = "ctrl";
        row.iteration    = iter;
        row.bytes        = (uint64_t)n;
        row.duration_ns  = duration_ns;
        row.user_cpu_us  = ru_after.user_us  - ru_before.user_us;
        row.sys_cpu_us   = ru_after.sys_us   - ru_before.sys_us;
        row.rss_peak_kb  = ru_after.rss_peak_kb;
        row.guest_mem_bytes = bench_guest_mem_bytes();
        row.has_guest_mem   = 0; /* native: emit empty column */
#ifdef __wasm__
        row.has_guest_mem   = 1;
#endif
        bench_csv_write(csv, &row);

        if (iter % 100 == 0 || iter == iterations - 1) {
            fprintf(stderr,
                    "  [%4u/%u]  RTT=%llu µs  bytes=%d\n",
                    iter, iterations,
                    (unsigned long long)(duration_ns / 1000u),
                    n);
        }
    }

    bench_csv_close(csv);
    libusb_close(handle);
    libusb_exit(ctx);

    fprintf(stderr,
            "✓ Done  iterations=%u  → %s\n",
            iterations, output);
    return 0;
}

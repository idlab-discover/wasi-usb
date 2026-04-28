/* Shared CSV logger for the WASI-USB benchmark suite.
 *
 * Schema (must stay in sync with usb-bench-rs/src/csv.rs):
 *   timestamp_iso,condition,workload,iteration,bytes,duration_ns,
 *   user_cpu_us,sys_cpu_us,rss_peak_kb,guest_mem_bytes,checksum_hex,notes
 *
 * Empty fields are written as the empty string. Strings are CSV-quoted only
 * when they contain a comma, double-quote, or newline.
 */

#ifndef BENCH_CSV_H
#define BENCH_CSV_H

#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    const char *condition;     /* e.g. "native-libusb" */
    const char *workload;      /* e.g. "bulk" */
    uint32_t    iteration;
    uint64_t    bytes;
    uint64_t    duration_ns;
    uint64_t    user_cpu_us;
    uint64_t    sys_cpu_us;
    uint64_t    rss_peak_kb;
    uint64_t    guest_mem_bytes;  /* 0 → empty for native cells (set has_guest_mem = 0) */
    int         has_guest_mem;    /* 0: write empty string; 1: write number */
    const char *checksum_hex;     /* NULL → empty */
    const char *notes;            /* NULL → empty */
} bench_row_t;

/* Open output CSV at `path`, appending. Writes the header if the file is new
 * or empty. Returns NULL on error (caller checks errno). */
FILE *bench_csv_open(const char *path);

/* Write one row and flush. Caller fills timestamp by giving NULL — we fill
 * an ISO-8601 UTC timestamp like "2026-04-26T12:34:56Z". */
void bench_csv_write(FILE *f, const bench_row_t *row);

/* Close. Safe on NULL. */
void bench_csv_close(FILE *f);

/* ---------------------------------------------------------------------------
 * Time + resource helpers (used by every benchmark)
 * ------------------------------------------------------------------------- */

/* Monotonic ns since some unspecified epoch. Uses CLOCK_MONOTONIC. */
uint64_t bench_now_ns(void);

typedef struct {
    uint64_t user_us;
    uint64_t sys_us;
    uint64_t rss_peak_kb;  /* ru_maxrss; on Linux it is in KiB */
} bench_rusage_t;

/* Snapshot getrusage(RUSAGE_SELF). Returns 0 on success, -1 on failure or
 * when getrusage is unavailable (then *out is zeroed). */
int bench_rusage_now(bench_rusage_t *out);

/* Guest linear-memory size in bytes. On wasm32 builds returns the current
 * memory.size in bytes; on native returns 0. */
uint64_t bench_guest_mem_bytes(void);

#ifdef __cplusplus
}
#endif

#endif /* BENCH_CSV_H */

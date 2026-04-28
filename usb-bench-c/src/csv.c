/* Shared CSV logger — implementation. See csv.h. */

#include "csv.h"

#include <errno.h>
#include <string.h>
#include <time.h>

#if !defined(__wasm__) && !defined(__wasi__)
#  include <sys/resource.h>
#  define HAS_GETRUSAGE 1
#else
#  define HAS_GETRUSAGE 0
#endif

/* ---------------------------------------------------------------------------
 * Helpers
 * ------------------------------------------------------------------------- */

static int file_is_empty(FILE *f)
{
    long pos = ftell(f);
    if (fseek(f, 0, SEEK_END) != 0) return 1;
    long end = ftell(f);
    if (fseek(f, pos, SEEK_SET) != 0) return 1;
    return end == 0;
}

/* CSV-escape: returns 1 if the field needs quoting. Caller writes the
 * escaped form via fputs() with surrounding quotes; embedded quotes doubled. */
static int field_needs_quote(const char *s)
{
    if (!s) return 0;
    for (; *s; ++s) {
        if (*s == ',' || *s == '"' || *s == '\n' || *s == '\r') return 1;
    }
    return 0;
}

static void write_field(FILE *f, const char *s)
{
    if (!s) return;  /* empty */
    if (!field_needs_quote(s)) {
        fputs(s, f);
        return;
    }
    fputc('"', f);
    for (; *s; ++s) {
        if (*s == '"') fputc('"', f);
        fputc(*s, f);
    }
    fputc('"', f);
}

static void iso_timestamp(char *buf, size_t len)
{
    /* Wallclock UTC. CLOCK_REALTIME is portable across native + WASI. */
    struct timespec ts;
    if (clock_gettime(CLOCK_REALTIME, &ts) != 0) {
        snprintf(buf, len, "1970-01-01T00:00:00Z");
        return;
    }
    time_t secs = (time_t)ts.tv_sec;
    struct tm tm_utc;
#if defined(_WIN32)
    gmtime_s(&tm_utc, &secs);
#else
    gmtime_r(&secs, &tm_utc);
#endif
    strftime(buf, len, "%Y-%m-%dT%H:%M:%SZ", &tm_utc);
}

/* ---------------------------------------------------------------------------
 * Public API
 * ------------------------------------------------------------------------- */

FILE *bench_csv_open(const char *path)
{
    FILE *f = fopen(path, "ab+");
    if (!f) return NULL;
    if (file_is_empty(f)) {
        fputs(
            "timestamp_iso,condition,workload,iteration,bytes,duration_ns,"
            "user_cpu_us,sys_cpu_us,rss_peak_kb,guest_mem_bytes,"
            "checksum_hex,notes\n",
            f);
        fflush(f);
    }
    return f;
}

void bench_csv_write(FILE *f, const bench_row_t *row)
{
    if (!f || !row) return;

    char ts[32];
    iso_timestamp(ts, sizeof(ts));

    write_field(f, ts);                    fputc(',', f);
    write_field(f, row->condition);        fputc(',', f);
    write_field(f, row->workload);         fputc(',', f);
    fprintf(f, "%u,",  row->iteration);
    fprintf(f, "%llu,", (unsigned long long)row->bytes);
    fprintf(f, "%llu,", (unsigned long long)row->duration_ns);
    fprintf(f, "%llu,", (unsigned long long)row->user_cpu_us);
    fprintf(f, "%llu,", (unsigned long long)row->sys_cpu_us);
    fprintf(f, "%llu,", (unsigned long long)row->rss_peak_kb);
    if (row->has_guest_mem) {
        fprintf(f, "%llu", (unsigned long long)row->guest_mem_bytes);
    }
    fputc(',', f);
    write_field(f, row->checksum_hex);     fputc(',', f);
    write_field(f, row->notes);
    fputc('\n', f);
    fflush(f);
}

void bench_csv_close(FILE *f)
{
    if (f) fclose(f);
}

/* ---------------------------------------------------------------------------
 * Time + resource helpers
 * ------------------------------------------------------------------------- */

uint64_t bench_now_ns(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) return 0;
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

int bench_rusage_now(bench_rusage_t *out)
{
    if (!out) return -1;
    out->user_us = 0;
    out->sys_us = 0;
    out->rss_peak_kb = 0;

#if HAS_GETRUSAGE
    struct rusage r;
    if (getrusage(RUSAGE_SELF, &r) != 0) return -1;
    out->user_us = (uint64_t)r.ru_utime.tv_sec * 1000000ULL + (uint64_t)r.ru_utime.tv_usec;
    out->sys_us  = (uint64_t)r.ru_stime.tv_sec * 1000000ULL + (uint64_t)r.ru_stime.tv_usec;
    /* On Linux ru_maxrss is in KiB; on macOS it is in bytes. We assume Linux
     * since this is the reference platform; analyze.py can normalize if a
     * macOS run sneaks in. */
    out->rss_peak_kb = (uint64_t)r.ru_maxrss;
    return 0;
#else
    /* WASI: no process-wide resource accounting. Caller logs zeros and the
     * host-side harness fills CPU/memory via wasmtime instrumentation. */
    return -1;
#endif
}

uint64_t bench_guest_mem_bytes(void)
{
#if defined(__wasm__) || defined(__wasi__)
    /* memory.size returns the size of memory 0 in pages of 65536 bytes. */
    return (uint64_t)__builtin_wasm_memory_size(0) * 65536ULL;
#else
    return 0;
#endif
}

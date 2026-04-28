//! Shared CSV logger for the WASI-USB benchmark suite.
//!
//! Schema (must stay in sync with `usb-bench-c/src/csv.{c,h}`):
//! ```text
//! timestamp_iso,condition,workload,iteration,bytes,duration_ns,
//! user_cpu_us,sys_cpu_us,rss_peak_kb,guest_mem_bytes,checksum_hex,notes
//! ```
//!
//! Empty fields are written as the empty string. Fields containing
//! `,`, `"`, `\n`, or `\r` are CSV-quoted with embedded `"` doubled.

use std::fs::OpenOptions;
use std::io::{BufWriter, Result as IoResult, Write};
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// One row in the benchmark CSV. Matches the C `bench_row_t`.
#[derive(Debug, Clone)]
pub struct Row {
    pub condition: String,
    pub workload: String,
    pub iteration: u32,
    pub bytes: u64,
    pub duration_ns: u64,
    pub user_cpu_us: u64,
    pub sys_cpu_us: u64,
    pub rss_peak_kb: u64,
    /// `None` for native cells (column is left empty in CSV).
    pub guest_mem_bytes: Option<u64>,
    pub checksum_hex: Option<String>,
    pub notes: Option<String>,
}

impl Row {
    pub fn new(condition: impl Into<String>, workload: impl Into<String>) -> Self {
        Self {
            condition: condition.into(),
            workload: workload.into(),
            iteration: 0,
            bytes: 0,
            duration_ns: 0,
            user_cpu_us: 0,
            sys_cpu_us: 0,
            rss_peak_kb: 0,
            guest_mem_bytes: None,
            checksum_hex: None,
            notes: None,
        }
    }
}

/// Append-mode CSV writer. Writes the header on first open of an empty file.
pub struct Logger {
    writer: BufWriter<std::fs::File>,
}

impl Logger {
    pub fn open(path: &Path) -> IoResult<Self> {
        let needs_header = match std::fs::metadata(path) {
            Ok(m) => m.len() == 0,
            Err(_) => true,
        };
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        let mut writer = BufWriter::new(f);
        if needs_header {
            writeln!(
                writer,
                "timestamp_iso,condition,workload,iteration,bytes,duration_ns,\
                 user_cpu_us,sys_cpu_us,rss_peak_kb,guest_mem_bytes,\
                 checksum_hex,notes"
            )?;
        }
        Ok(Self { writer })
    }

    pub fn write(&mut self, row: &Row) -> IoResult<()> {
        let ts = iso_timestamp();
        let guest_mem = row
            .guest_mem_bytes
            .map(|v| v.to_string())
            .unwrap_or_default();
        let checksum = row.checksum_hex.as_deref().unwrap_or("");
        let notes = row.notes.as_deref().unwrap_or("");
        writeln!(
            self.writer,
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            csv_escape(&ts),
            csv_escape(&row.condition),
            csv_escape(&row.workload),
            row.iteration,
            row.bytes,
            row.duration_ns,
            row.user_cpu_us,
            row.sys_cpu_us,
            row.rss_peak_kb,
            guest_mem,
            csv_escape(checksum),
            csv_escape(notes),
        )?;
        self.writer.flush()
    }
}

/// CSV-escape a field. Returns either the original string or a quoted form
/// with internal `"` doubled.
fn csv_escape(s: &str) -> std::borrow::Cow<'_, str> {
    if s.bytes().any(|b| matches!(b, b',' | b'"' | b'\n' | b'\r')) {
        std::borrow::Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

/// ISO-8601 UTC timestamp like "2026-04-26T12:34:56Z". Dependency-free so it
/// also compiles cleanly to wasm32-wasip2.
fn iso_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d) = epoch_days_to_ymd((secs / 86400) as i64);
    let rem = secs % 86400;
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y, mo, d, h, mi, s
    )
}

/// Howard Hinnant's civil-from-days algorithm.
/// `z` = days since 1970-01-01.
fn epoch_days_to_ymd(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Time + resource helpers (used by every benchmark binary)
// ---------------------------------------------------------------------------

/// Monotonic timestamp wrapper. Use the difference between two snapshots to
/// get the elapsed `Duration` for a measurement loop.
pub fn now() -> Instant {
    Instant::now()
}

/// Snapshot of `getrusage(RUSAGE_SELF)` on native. On WASM targets this
/// returns zeros — the host harness records CPU stats around wasmtime calls.
#[derive(Debug, Default, Clone, Copy)]
pub struct Rusage {
    pub user_us: u64,
    pub sys_us: u64,
    pub rss_peak_kb: u64,
}

impl Rusage {
    pub fn snapshot() -> Self {
        rusage_impl()
    }

    /// `self - earlier` per field, saturating at 0 to avoid wrap on RSS
    /// (which is a high-water mark and never decreases anyway).
    pub fn delta(&self, earlier: &Rusage) -> Rusage {
        Rusage {
            user_us: self.user_us.saturating_sub(earlier.user_us),
            sys_us: self.sys_us.saturating_sub(earlier.sys_us),
            rss_peak_kb: self.rss_peak_kb.max(earlier.rss_peak_kb),
        }
    }
}

#[cfg(all(unix, not(target_family = "wasm")))]
fn rusage_impl() -> Rusage {
    let mut r: libc::rusage = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut r) };
    if ok != 0 {
        return Rusage::default();
    }
    let user_us = (r.ru_utime.tv_sec as u64) * 1_000_000 + (r.ru_utime.tv_usec as u64);
    let sys_us = (r.ru_stime.tv_sec as u64) * 1_000_000 + (r.ru_stime.tv_usec as u64);
    // On Linux ru_maxrss is in KiB; on macOS it is in bytes. We assume Linux.
    let rss_peak_kb = r.ru_maxrss as u64;
    Rusage {
        user_us,
        sys_us,
        rss_peak_kb,
    }
}

#[cfg(any(not(unix), target_family = "wasm"))]
fn rusage_impl() -> Rusage {
    Rusage::default()
}

/// Guest linear-memory size in bytes. Returns `Some(bytes)` on wasm32,
/// `None` on native.
pub fn guest_mem_bytes() -> Option<u64> {
    #[cfg(target_family = "wasm")]
    {
        // memory.size returns the number of 64 KiB pages.
        let pages = core::arch::wasm32::memory_size(0) as u64;
        Some(pages * 65536)
    }
    #[cfg(not(target_family = "wasm"))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamp_format_is_well_shaped() {
        let s = iso_timestamp();
        // "YYYY-MM-DDTHH:MM:SSZ" → length 20
        assert_eq!(s.len(), 20, "got {s:?}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[13..14], ":");
        assert_eq!(&s[16..17], ":");
        assert_eq!(&s[19..], "Z");
    }

    #[test]
    fn epoch_days_known_dates() {
        // 1970-01-01 → day 0
        assert_eq!(epoch_days_to_ymd(0), (1970, 1, 1));
        // 2000-02-29 (leap day) → day 11_016
        assert_eq!(epoch_days_to_ymd(11016), (2000, 2, 29));
        // 2026-04-26 → 20_569 days after 1970-01-01
        assert_eq!(epoch_days_to_ymd(20569), (2026, 4, 26));
    }

    #[test]
    fn csv_escape_passthrough() {
        assert_eq!(csv_escape("hello").as_ref(), "hello");
        assert_eq!(csv_escape("native-libusb").as_ref(), "native-libusb");
    }

    #[test]
    fn csv_escape_quotes_embedded_separators() {
        assert_eq!(csv_escape("a,b").as_ref(), "\"a,b\"");
        assert_eq!(csv_escape("she said \"hi\"").as_ref(), "\"she said \"\"hi\"\"\"");
        assert_eq!(csv_escape("line1\nline2").as_ref(), "\"line1\nline2\"");
    }

    #[test]
    fn logger_writes_header_and_row() -> IoResult<()> {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "bench_csv_test_{}.csv",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut logger = Logger::open(&path)?;
            let mut row = Row::new("native-rusb", "bulk");
            row.iteration = 0;
            row.bytes = 1_048_576;
            row.duration_ns = 12_345_678;
            row.user_cpu_us = 100;
            row.sys_cpu_us = 50;
            row.rss_peak_kb = 8192;
            row.checksum_hex = Some("deadbeef".into());
            logger.write(&row)?;
        }

        let content = std::fs::read_to_string(&path)?;
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("timestamp_iso,"));
        assert!(lines[1].contains(",native-rusb,bulk,0,1048576,12345678,"));
        assert!(lines[1].ends_with(",deadbeef,"));

        let _ = std::fs::remove_file(&path);
        Ok(())
    }
}

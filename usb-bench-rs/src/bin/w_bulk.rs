//! W-bulk benchmark — SCSI READ/WRITE throughput on a USB mass-storage stick.
//!
//! Measures the three key axes for the bulk workload:
//!   • **Throughput** (MB/s) — `bytes / duration_ns * 1e9 / 1e6`
//!   • **RTT** (µs) — `duration_ns / 1000` per SCSI call
//!   • **Resource usage** — CPU-time and RSS-peak via `getrusage(RUSAGE_SELF)`
//!     (zeros on WASM — host harness fills those columns from wasmtime metrics)
//!
//! Output: unified CSV via `usb_bench_rs::csv::Logger` (same schema as C bench).
//!
//! # Conditions served by this binary
//! | Build target        | Condition | Default `--condition` |
//! |---------------------|-----------|-----------------------|
//! | native host         | C3        | `native-rusb`         |
//! | wasm32-wasip2       | C5        | `wasi-raw-wit`        |
//!
//! **Thesis claim**: the same binary source (this file) + the same
//! `ScsiDevice<T>` (mass_storage.rs) compile for both targets without a
//! single line of change. Only the transport `T` differs:
//!   C3: `NativeTransport` (rusb)   vs   C5: `WasmTransport` (wit-bindgen)
//!
//! # Usage
//! ```text
//! w_bulk [output.csv] [VID:PID] [iterations] [blocks_per_iter]
//!        [--condition <name>] [--mode read|write|rw] [--random]
//!
//! Defaults:
//!   output.csv       bench_w_bulk.csv
//!   VID:PID          0951:1666  (Kingston DataTraveler)
//!   iterations       100
//!   blocks_per_iter  128        (64 KiB @ 512 B/block)
//!   condition        native-rusb  (native) | wasi-raw-wit  (wasm)
//!   mode             read        (safe, no writes)
//!   random           false       (always start from LBA 0)
//! ```
//!
//! # Transfer modes
//! - `read`  — SCSI READ(10) only (default; backward compatible)
//! - `write` — SCSI WRITE(10) only; writes a fixed 0x5A pattern
//!             ⚠️ overwrites data at LBA 0..blocks_per_iter; use a test device
//! - `rw`    — SCSI READ(10) then WRITE(10) of the same data (safe);
//!             measures combined round-trip; data on device unchanged
//!
//! # Random I/O
//! With `--random`, each iteration picks a uniformly distributed start LBA
//! (aligned to `blocks_per_iter`) instead of always reading from LBA 0.
//! Uses a simple LCG seeded from the system clock so runs are reproducible
//! when re-run in the same second (sufficient for thesis comparisons).

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Result};

use usb_bench_rs::csv::{self, Logger, Row, Rusage};
use usb_bench_rs::mass_storage::ScsiDevice;

// ---------------------------------------------------------------------------
// Transport selection — the ONLY cfg-gated section in this file
// ---------------------------------------------------------------------------
// C3 (native) and C4 (wasm32-wasip2 + c4-rusb-wasi feature) → rusb NativeTransport
#[cfg(any(not(target_family = "wasm"), feature = "c4-rusb-wasi"))]
use usb_bench_rs::bulk_only::NativeTransport as ActiveTransport;

// C5 (wasm32-wasip2, raw WIT, no c4-rusb-wasi feature) → WasmTransport
#[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
use usb_bench_rs::bulk_only::WasmTransport as ActiveTransport;

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cfg = Config::from_args(std::env::args().collect())?;

    let mut logger = Logger::open(Path::new(&cfg.output))?;

    // Open device and initialise SCSI layer.
    // `ActiveTransport::open` is the only call that differs between C3/C5 —
    // and it's hidden behind the type alias above, so even this line is shared.
    let transport = ActiveTransport::open(cfg.vid, cfg.pid)?;
    run_benchmark(transport, &cfg, &mut logger)
}

// ---------------------------------------------------------------------------
// Shared benchmark loop (identical for native and wasm)
// ---------------------------------------------------------------------------

fn run_benchmark(
    transport: ActiveTransport,
    cfg: &Config,
    logger: &mut Logger,
) -> Result<()> {
    let mut scsi = ScsiDevice::new(transport)?;

    let block_size = scsi.block_size();
    let num_blocks = scsi.num_blocks();
    let bytes_per_iter = cfg.blocks_per_iter as u64 * block_size as u64;

    eprintln!(
        "▶ {vid:04x}:{pid:04x}  block_size={block_size} B  \
         num_blocks={num}  each iter: {n_blocks}×{block_size} = {kb} KiB",
        vid = cfg.vid,
        pid = cfg.pid,
        num = num_blocks,
        n_blocks = cfg.blocks_per_iter,
        kb = bytes_per_iter / 1024,
    );
    eprintln!(
        "  condition={cond}  workload=bulk  mode={mode}  random={rnd}  \
         iterations={it}  → {out}",
        cond = cfg.condition,
        mode = cfg.mode.as_str(),
        rnd = cfg.random,
        it = cfg.iterations,
        out = cfg.output,
    );

    // Pre-built write buffer: fixed 0x5A pattern (used for --mode write)
    let write_buf: Vec<u8> = vec![0x5Au8; bytes_per_iter as usize];

    // Simple LCG for reproducible pseudo-random LBA selection.
    // Seed: lower 32 bits of nanosecond timestamp (good enough for thesis runs).
    let mut lcg_state: u64 = {
        #[cfg(not(target_family = "wasm"))]
        { std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos() as u64 }
        #[cfg(target_family = "wasm")]
        { 0xDEAD_BEEF_u64 }
    };
    let lcg_next = |state: &mut u64| -> u32 {
        // LCG parameters from Knuth / Numerical Recipes
        *state = state.wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((*state >> 33) & 0xFFFF_FFFF) as u32
    };

    let mut total_bytes: u64 = 0;
    let loop_start = Instant::now();

    for iter in 0..cfg.iterations {
        // Choose LBA: sequential (LBA 0) or random
        let lba: u32 = if cfg.random && num_blocks > cfg.blocks_per_iter as u32 {
            let range = num_blocks - cfg.blocks_per_iter as u32;
            lcg_next(&mut lcg_state) % range
        } else {
            0
        };

        // Snapshot resource usage before transfer (zeros on wasm)
        let ru_before = Rusage::snapshot();

        // ── timed section ────────────────────────────────────────────────────
        let t0 = Instant::now();
        let data = match cfg.mode {
            BulkMode::Read => {
                scsi.read_blocks(lba, cfg.blocks_per_iter)?
            }
            BulkMode::Write => {
                scsi.write_blocks(lba, cfg.blocks_per_iter, &write_buf)?;
                write_buf.clone()
            }
            BulkMode::ReadWrite => {
                scsi.readwrite_blocks(lba, cfg.blocks_per_iter)?
            }
        };
        let duration_ns = t0.elapsed().as_nanos() as u64;
        // ── end timed section ────────────────────────────────────────────────

        // Snapshot resource usage after transfer
        let ru_after = Rusage::snapshot();
        let delta = ru_after.delta(&ru_before);

        total_bytes += data.len() as u64;

        // Build CSV row
        let mode_note = format!("mode={} lba={lba}", cfg.mode.as_str());
        let mut row = Row::new(cfg.condition.as_str(), "bulk");
        row.iteration = iter;
        row.bytes = data.len() as u64;
        row.duration_ns = duration_ns;
        row.user_cpu_us = delta.user_us;
        row.sys_cpu_us = delta.sys_us;
        row.rss_peak_kb = delta.rss_peak_kb;
        row.guest_mem_bytes = csv::guest_mem_bytes();
        row.notes = Some(mode_note.clone());
        logger.write(&row)?;

        // Progress every 10 iterations
        if iter % 10 == 0 || iter == cfg.iterations - 1 {
            let mb_s = (data.len() as f64 / 1_048_576.0)
                / (duration_ns as f64 / 1_000_000_000.0);
            let rtt_us = duration_ns / 1_000;
            eprintln!(
                "  [{iter:3}/{it}]  {mb_s:6.1} MB/s  RTT={rtt_us} µs  lba={lba}",
                it = cfg.iterations
            );
        }
    }

    let wall_secs = loop_start.elapsed().as_secs_f64();
    let avg_mb_s = (total_bytes as f64 / 1_048_576.0) / wall_secs;
    eprintln!(
        "✓ Done  total={total:.1} MiB  avg={avg:.1} MB/s  wall={wall:.2} s  → {out}",
        total = total_bytes as f64 / 1_048_576.0,
        avg = avg_mb_s,
        wall = wall_secs,
        out = cfg.output,
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Transfer mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BulkMode {
    Read,
    Write,
    ReadWrite,
}

impl BulkMode {
    fn as_str(self) -> &'static str {
        match self {
            BulkMode::Read      => "read",
            BulkMode::Write     => "write",
            BulkMode::ReadWrite => "rw",
        }
    }
}

// ---------------------------------------------------------------------------
// CLI configuration
// ---------------------------------------------------------------------------

struct Config {
    output: String,
    vid: u16,
    pid: u16,
    iterations: u32,
    blocks_per_iter: u16,
    condition: String,
    mode: BulkMode,
    random: bool,
}

impl Config {
    fn from_args(args: Vec<String>) -> Result<Self> {
        // Positional arguments (all optional with defaults):
        //   1: output CSV path
        //   2: VID:PID hex
        //   3: iterations
        //   4: blocks per iteration
        //   Named: --condition <name>  (may appear anywhere)
        //   Named: --mode read|write|rw
        //   Flag:  --random
        let output = args
            .get(1)
            .map(String::as_str)
            .unwrap_or("bench_w_bulk.csv")
            .to_owned();
        let vid_pid = args.get(2).map(String::as_str).unwrap_or("0951:1666");
        let iterations: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
        let blocks_per_iter: u16 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(128);

        // --condition flag: explicit name overrides the cfg-based default
        let condition = args
            .windows(2)
            .find(|w| w[0] == "--condition")
            .map(|w| w[1].clone())
            .unwrap_or_else(|| default_condition().to_owned());

        // --mode flag
        let mode = args
            .windows(2)
            .find(|w| w[0] == "--mode")
            .map(|w| match w[1].as_str() {
                "write" => BulkMode::Write,
                "rw"    => BulkMode::ReadWrite,
                _       => BulkMode::Read,
            })
            .unwrap_or(BulkMode::Read);

        // --random flag (presence = true)
        let random = args.iter().any(|a| a == "--random");

        let (vid, pid) = parse_vid_pid(vid_pid)?;

        if blocks_per_iter == 0 {
            return Err(anyhow!("blocks_per_iter must be > 0"));
        }

        Ok(Self {
            output,
            vid,
            pid,
            iterations,
            blocks_per_iter,
            condition,
            mode,
            random,
        })
    }
}

/// Default condition label — compile-time constant that reflects the actual
/// transport used. Can always be overridden with `--condition <name>`.
fn default_condition() -> &'static str {
    #[cfg(not(target_family = "wasm"))]
    { "native-rusb" }
    #[cfg(all(target_family = "wasm", feature = "c4-rusb-wasi"))]
    { "wasi-rusb" }
    #[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
    { "wasi-raw-wit" }
}

fn parse_vid_pid(s: &str) -> Result<(u16, u16)> {
    let (v, p) = s
        .split_once(':')
        .ok_or_else(|| anyhow!("expected VID:PID (hex), got {s:?}"))?;
    let vid = u16::from_str_radix(v.trim(), 16)
        .map_err(|e| anyhow!("invalid VID {v:?}: {e}"))?;
    let pid = u16::from_str_radix(p.trim(), 16)
        .map_err(|e| anyhow!("invalid PID {p:?}: {e}"))?;
    Ok((vid, pid))
}

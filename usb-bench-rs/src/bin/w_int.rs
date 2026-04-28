//! W-int benchmark — USB interrupt-transfer RTT on a PS5 DualSense controller.
//!
//! Polls the interrupt-IN endpoint and records the per-transfer RTT, capturing
//! jitter in the host-side scheduling of USB interrupt reports.
//!
//! # Conditions served by this binary
//! | Build target        | Condition | Default `--condition` |
//! |---------------------|-----------|-----------------------|
//! | native host         | C3        | `native-rusb`         |
//! | wasm32-wasip2       | C5        | `wasi-raw-wit`        |
//!
//! Default device: PS5 DualSense `054c:0ce6` (interface 0, endpoint 0x81,
//! 64-byte HID input report).  Override VID:PID on the command line.
//!
//! # Usage
//! ```text
//! w_int [output.csv] [VID:PID] [iterations] [--condition <name>]
//!
//! Defaults:
//!   output.csv       bench_w_int.csv
//!   VID:PID          054c:0ce6  (PS5 DualSense)
//!   iterations       1000
//!   condition        native-rusb  (native) | wasi-raw-wit  (wasm)
//! ```

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, Result};

use usb_bench_rs::csv::{self, Logger, Row, Rusage};

// ---------------------------------------------------------------------------
// Transport selection — the ONLY cfg-gated section in this file
// ---------------------------------------------------------------------------

// C3 (native) and C4 (wasm32-wasip2 + c4-rusb-wasi) → rusb-based native impl
#[cfg(any(not(target_family = "wasm"), feature = "c4-rusb-wasi"))]
use native::IntDevice as ActiveDevice;

// C5 (wasm32-wasip2, raw WIT) → WIT-based wasm impl
#[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
use wasm::IntDevice as ActiveDevice;

// ---------------------------------------------------------------------------
// Native transport (rusb) — C3 (native) and C4 (wasm + c4-rusb-wasi)
// ---------------------------------------------------------------------------

#[cfg(any(not(target_family = "wasm"), feature = "c4-rusb-wasi"))]
mod native {
    use anyhow::{anyhow, Result};
    use rusb::{GlobalContext, UsbContext};
    use std::time::Duration;

    // Default endpoint + packet geometry for PS5 DualSense
    const IFACE: u8 = 0;
    const EP_IN: u8 = 0x81;
    const PKT_SIZE: usize = 64;

    pub struct IntDevice {
        handle: rusb::DeviceHandle<GlobalContext>,
        pub packet_size: usize,
    }

    impl IntDevice {
        pub fn open(vid: u16, pid: u16) -> Result<Self> {
            let ctx = GlobalContext::default();
            let handle = ctx
                .open_device_with_vid_pid(vid, pid)
                .ok_or_else(|| anyhow!("device {:04x}:{:04x} not found", vid, pid))?;

            // Detach kernel HID driver if active (necessary on Linux)
            if handle.kernel_driver_active(IFACE).unwrap_or(false) {
                handle.detach_kernel_driver(IFACE)?;
            }
            handle.claim_interface(IFACE)?;

            Ok(IntDevice { handle, packet_size: PKT_SIZE })
        }

        /// Single interrupt-IN read → returns the report bytes.
        pub fn poll(&mut self) -> Result<Vec<u8>> {
            let mut buf = vec![0u8; self.packet_size];
            let n = self
                .handle
                .read_interrupt(EP_IN, &mut buf, Duration::from_millis(500))?;
            buf.truncate(n);
            Ok(buf)
        }
    }

    impl Drop for IntDevice {
        fn drop(&mut self) {
            let _ = self.handle.release_interface(IFACE);
        }
    }
}

// ---------------------------------------------------------------------------
// WASM transport (raw WIT bindings) — C5 only
// ---------------------------------------------------------------------------

#[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
mod wasm {
    use anyhow::{anyhow, Result};

    // WIT types from the central bindings module (one generate! per binary).
    use usb_bench_rs::bindings::component::usb::configuration::ConfigValue;
    use usb_bench_rs::bindings::component::usb::device::{list_devices, DeviceHandle};
    use usb_bench_rs::bindings::component::usb::transfers::{
        await_transfer, TransferOptions, TransferSetup, TransferType,
    };

    const IFACE: u8 = 0;
    const EP_IN: u8 = 0x81;
    const PKT_SIZE: u32 = 64;

    pub struct IntDevice {
        handle: DeviceHandle,
        pub packet_size: usize,
    }

    impl IntDevice {
        pub fn open(vid: u16, pid: u16) -> Result<Self> {
            let devices =
                list_devices().map_err(|e| anyhow!("list_devices: {:?}", e))?;
            let (dev, _, _) = devices
                .into_iter()
                .find(|(_, d, _)| d.vendor_id == vid && d.product_id == pid)
                .ok_or_else(|| anyhow!("device {:04x}:{:04x} not found", vid, pid))?;
            let handle = dev.open().map_err(|e| anyhow!("open: {:?}", e))?;
            handle
                .set_configuration(ConfigValue::Value(1))
                .map_err(|e| anyhow!("set_configuration: {:?}", e))?;
            if handle.kernel_driver_active(IFACE).unwrap_or(false) {
                let _ = handle.detach_kernel_driver(IFACE);
            }
            handle
                .claim_interface(IFACE)
                .map_err(|e| anyhow!("claim_interface: {:?}", e))?;
            Ok(IntDevice { handle, packet_size: PKT_SIZE as usize })
        }

        pub fn poll(&mut self) -> Result<Vec<u8>> {
            let setup = TransferSetup {
                bm_request_type: 0,
                b_request: 0,
                w_value: 0,
                w_index: 0,
            };
            let opts = TransferOptions {
                endpoint: EP_IN,
                timeout_ms: 500,
                stream_id: 0,
                iso_packets: 0,
            };
            let xfer = self
                .handle
                .new_transfer(TransferType::Interrupt, setup, PKT_SIZE, opts)
                .map_err(|e| anyhow!("new_transfer: {:?}", e))?;
            xfer.submit_transfer(&[])
                .map_err(|e| anyhow!("submit_transfer: {:?}", e))?;
            let result =
                await_transfer(&xfer).map_err(|e| anyhow!("await_transfer: {:?}", e))?;
            Ok(result.data)
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cfg = Config::from_args(std::env::args().collect())?;
    let mut logger = Logger::open(Path::new(&cfg.output))?;
    let device = ActiveDevice::open(cfg.vid, cfg.pid)?;
    run_benchmark(device, &cfg, &mut logger)
}

// ---------------------------------------------------------------------------
// Shared benchmark loop (identical for native and wasm)
// ---------------------------------------------------------------------------

fn run_benchmark(
    mut device: ActiveDevice,
    cfg: &Config,
    logger: &mut Logger,
) -> Result<()> {
    let pkt = device.packet_size;
    eprintln!(
        "▶ {vid:04x}:{pid:04x}  workload=int  pkt={pkt} B  \
         iterations={it}  condition={cond}  → {out}",
        vid = cfg.vid,
        pid = cfg.pid,
        it = cfg.iterations,
        cond = cfg.condition,
        out = cfg.output,
    );

    let loop_start = Instant::now();

    for iter in 0..cfg.iterations {
        let ru_before = Rusage::snapshot();

        // ── timed section ────────────────────────────────────────────────────
        let t0 = Instant::now();
        let data = device.poll()?;
        let duration_ns = t0.elapsed().as_nanos() as u64;
        // ── end timed section ────────────────────────────────────────────────

        let ru_after = Rusage::snapshot();
        let delta = ru_after.delta(&ru_before);

        let mut row = Row::new(cfg.condition.as_str(), "int");
        row.iteration = iter;
        row.bytes = data.len() as u64;
        row.duration_ns = duration_ns;
        row.user_cpu_us = delta.user_us;
        row.sys_cpu_us = delta.sys_us;
        row.rss_peak_kb = delta.rss_peak_kb;
        row.guest_mem_bytes = csv::guest_mem_bytes();
        logger.write(&row)?;

        if iter % 100 == 0 || iter == cfg.iterations - 1 {
            let rtt_us = duration_ns / 1_000;
            eprintln!(
                "  [{iter:4}/{it}]  RTT={rtt_us} µs  bytes={}",
                data.len(),
                it = cfg.iterations,
            );
        }
    }

    let wall_secs = loop_start.elapsed().as_secs_f64();
    eprintln!(
        "✓ Done  iterations={}  wall={wall:.2} s  → {out}",
        cfg.iterations,
        wall = wall_secs,
        out = cfg.output,
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI configuration
// ---------------------------------------------------------------------------

struct Config {
    output: String,
    vid: u16,
    pid: u16,
    iterations: u32,
    condition: String,
}

impl Config {
    fn from_args(args: Vec<String>) -> Result<Self> {
        let output = args
            .get(1)
            .map(String::as_str)
            .unwrap_or("bench_w_int.csv")
            .to_owned();
        let vid_pid = args.get(2).map(String::as_str).unwrap_or("054c:0ce6");
        let iterations: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1000);
        let condition = args
            .windows(2)
            .find(|w| w[0] == "--condition")
            .map(|w| w[1].clone())
            .unwrap_or_else(|| default_condition().to_owned());
        let (vid, pid) = parse_vid_pid(vid_pid)?;
        Ok(Self { output, vid, pid, iterations, condition })
    }
}

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

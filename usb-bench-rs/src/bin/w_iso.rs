//! W-iso benchmark — isochronous throughput on a Logitech Brio webcam.
//!
//! Submits isochronous IN transfers on the UVC Video Streaming interface and
//! records sustained throughput (bytes/s) and per-transfer latency.
//!
//! Native path uses `libusb1-sys` directly because `rusb 0.9` does not expose
//! a synchronous isochronous API; the raw ISO async submit+await loop is
//! implemented here behind the `IsoDevice` abstraction so that `run_benchmark`
//! is 100% shared between native and WASM.
//!
//! # Conditions served by this binary
//! | Build target        | Condition | Default `--condition` |
//! |---------------------|-----------|-----------------------|
//! | native host         | C3        | `native-rusb`         |
//! | wasm32-wasip2       | C5        | `wasi-raw-wit`        |
//!
//! Default device: Logitech Brio `046d:086c` (VS interface 1, alt 1,
//! endpoint 0x81, packet stride 1024 B, 32 packets per transfer ≈ 32 KiB).
//!
//! # Usage
//! ```text
//! w_iso [output.csv] [VID:PID] [iterations] [--condition <name>]
//!       [--iface N] [--alt N] [--ep 0xNN] [--pkt-size N] [--num-pkts N]
//!
//! Defaults:
//!   output.csv       bench_w_iso.csv
//!   VID:PID          046d:086c  (Logitech Brio)
//!   iterations       200
//!   condition        native-rusb  (native) | wasi-raw-wit  (wasm)
//!   --iface          1
//!   --alt            1
//!   --ep             0x81
//!   --pkt-size       1024
//!   --num-pkts       32
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
use native::IsoDevice as ActiveDevice;

// C5 (wasm32-wasip2, raw WIT) → WIT-based wasm impl
#[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
use wasm::IsoDevice as ActiveDevice;

// ---------------------------------------------------------------------------
// Native transport — libusb1-sys ISO async submit+await — C3 and C4
// ---------------------------------------------------------------------------

#[cfg(any(not(target_family = "wasm"), feature = "c4-rusb-wasi"))]
mod native {
    use anyhow::{anyhow, Result};
    use libusb1_sys::constants::*;
    use libusb1_sys::*;
    use std::os::raw::{c_int, c_void};

    // ── Callback set by libusb when the transfer completes ──────────────────
    extern "system" fn iso_cb(xfer: *mut libusb_transfer) {
        // `user_data` points to the `completed` c_int on the caller's stack.
        // The callback runs synchronously inside `libusb_handle_events_*`,
        // which is called from the same thread, so there is no data race.
        unsafe {
            let completed = (*xfer).user_data as *mut c_int;
            *completed = 1;
        }
    }

    // ── Helper: turn a libusb error code into an anyhow::Error ──────────────
    fn check(rc: c_int, ctx: &str) -> Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(anyhow!("{ctx}: libusb error {rc}"))
        }
    }

    pub struct IsoDevice {
        ctx: *mut libusb_context,
        handle: *mut libusb_device_handle,
        iface: c_int,
        ep_in: u8,
        pub packet_size: u32,
        pub num_packets: u32,
    }

    impl IsoDevice {
        pub fn open(
            vid: u16,
            pid: u16,
            iface: u8,
            alt: u8,
            ep_in: u8,
            packet_size: u32,
            num_packets: u32,
        ) -> Result<Self> {
            unsafe {
                let mut ctx: *mut libusb_context = std::ptr::null_mut();
                check(libusb_init(&mut ctx), "libusb_init")?;

                let handle = libusb_open_device_with_vid_pid(ctx, vid, pid);
                if handle.is_null() {
                    libusb_exit(ctx);
                    return Err(anyhow!("device {:04x}:{:04x} not found", vid, pid));
                }

                // Detach kernel driver if active (video4linux / uvcvideo on Linux)
                if libusb_kernel_driver_active(handle, iface as c_int) == 1 {
                    let _ = libusb_detach_kernel_driver(handle, iface as c_int);
                }

                check(libusb_set_configuration(handle, 1), "set_configuration")?;
                check(libusb_claim_interface(handle, iface as c_int), "claim_interface")?;
                check(
                    libusb_set_interface_alt_setting(handle, iface as c_int, alt as c_int),
                    "set_interface_alt_setting",
                )?;

                Ok(IsoDevice {
                    ctx,
                    handle,
                    iface: iface as c_int,
                    ep_in,
                    packet_size,
                    num_packets,
                })
            }
        }

        /// Submit one ISO IN transfer and wait for it.
        /// Returns the total number of bytes actually transferred.
        pub fn capture(&self) -> Result<u64> {
            let total = (self.packet_size * self.num_packets) as usize;
            let mut buf = vec![0u8; total];
            let mut completed: c_int = 0;

            unsafe {
                let xfer = libusb_alloc_transfer(self.num_packets as c_int);
                if xfer.is_null() {
                    return Err(anyhow!("libusb_alloc_transfer failed (OOM)"));
                }

                libusb_fill_iso_transfer(
                    xfer,
                    self.handle,
                    self.ep_in,
                    buf.as_mut_ptr(),
                    total as c_int,
                    self.num_packets as c_int,
                    iso_cb,
                    &mut completed as *mut c_int as *mut c_void,
                    5_000, // timeout ms
                );
                libusb_set_iso_packet_lengths(xfer, self.packet_size);

                let rc = libusb_submit_transfer(xfer);
                if rc != 0 {
                    libusb_free_transfer(xfer);
                    return Err(anyhow!("libusb_submit_transfer: {rc}"));
                }

                // Pump the event loop until the callback sets `completed = 1`.
                while completed == 0 {
                    libusb_handle_events_completed(self.ctx, &mut completed);
                }

                // Transfer status check
                let status = (*xfer).status;
                let actual = (*xfer).actual_length as u64;
                libusb_free_transfer(xfer);

                if status != LIBUSB_TRANSFER_COMPLETED as c_int
                    && status != LIBUSB_TRANSFER_TIMED_OUT as c_int
                {
                    return Err(anyhow!("iso transfer failed with status {status}"));
                }

                Ok(actual)
            }
        }
    }

    impl Drop for IsoDevice {
        fn drop(&mut self) {
            unsafe {
                let _ = libusb_release_interface(self.handle, self.iface);
                libusb_close(self.handle);
                libusb_exit(self.ctx);
            }
        }
    }

    // Send + Sync are needed so `run_benchmark` can be called generically.
    // Safety: IsoDevice is used from a single thread only.
    unsafe impl Send for IsoDevice {}
    unsafe impl Sync for IsoDevice {}
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
    use usb_bench_rs::bindings::component::usb::errors::LibusbError;
    use usb_bench_rs::bindings::component::usb::transfers::{
        await_transfer, TransferOptions, TransferSetup, TransferType,
    };

    pub struct IsoDevice {
        handle: DeviceHandle,
        iface: u8,
        ep_in: u8,
        pub packet_size: u32,
        pub num_packets: u32,
    }

    impl IsoDevice {
        pub fn open(
            vid: u16,
            pid: u16,
            iface: u8,
            alt: u8,
            ep_in: u8,
            packet_size: u32,
            num_packets: u32,
        ) -> Result<Self> {
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
            if handle.kernel_driver_active(iface).unwrap_or(false) {
                let _ = handle.detach_kernel_driver(iface);
            }
            handle
                .claim_interface(iface)
                .map_err(|e| anyhow!("claim_interface: {:?}", e))?;
            handle
                .set_interface_altsetting(iface, alt)
                .map_err(|e| anyhow!("set_interface_altsetting: {:?}", e))?;
            Ok(IsoDevice {
                handle,
                iface,
                ep_in,
                packet_size,
                num_packets,
            })
        }

        pub fn capture(&self) -> Result<u64> {
            let total = self.packet_size * self.num_packets;
            let setup = TransferSetup {
                bm_request_type: 0,
                b_request: 0,
                w_value: 0,
                w_index: 0,
            };
            let opts = TransferOptions {
                endpoint: self.ep_in,
                timeout_ms: 5_000,
                stream_id: 0,
                iso_packets: self.num_packets,
            };
            let xfer = self
                .handle
                .new_transfer(TransferType::Isochronous, setup, total, opts)
                .map_err(|e| anyhow!("new_transfer: {:?}", e))?;
            xfer.submit_transfer(&[])
                .map_err(|e| anyhow!("submit_transfer: {:?}", e))?;
            // Timeout is not fatal for ISO — the macOS UVC driver holds the
            // interface and all transfers time out with 0 bytes.  Accept it
            // the same way the native path accepts LIBUSB_TRANSFER_TIMED_OUT.
            let result = match await_transfer(&xfer) {
                Ok(r) => r,
                Err(LibusbError::Timeout) => return Ok(0),
                Err(e) => return Err(anyhow!("await_transfer: {:?}", e)),
            };
            // Sum actual lengths of individual ISO packets
            let actual: u64 = result
                .packets
                .iter()
                .map(|p| p.actual_length as u64)
                .sum();
            Ok(actual)
        }
    }

    impl Drop for IsoDevice {
        fn drop(&mut self) {
            let _ = self.handle.release_interface(self.iface);
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cfg = Config::from_args(std::env::args().collect())?;
    let mut logger = Logger::open(Path::new(&cfg.output))?;
    let device = ActiveDevice::open(
        cfg.vid,
        cfg.pid,
        cfg.iface,
        cfg.alt,
        cfg.ep_in,
        cfg.packet_size,
        cfg.num_packets,
    )?;
    run_benchmark(device, &cfg, &mut logger)
}

// ---------------------------------------------------------------------------
// Shared benchmark loop (identical for native and wasm)
// ---------------------------------------------------------------------------

fn run_benchmark(
    device: ActiveDevice,
    cfg: &Config,
    logger: &mut Logger,
) -> Result<()> {
    let bytes_per_iter = device.packet_size as u64 * device.num_packets as u64;
    eprintln!(
        "▶ {vid:04x}:{pid:04x}  workload=iso  iface={iface} alt={alt} \
         ep=0x{ep:02x}  pkt={pkt}×{npkt}={kib} KiB",
        vid = cfg.vid,
        pid = cfg.pid,
        iface = cfg.iface,
        alt = cfg.alt,
        ep = cfg.ep_in,
        pkt = device.packet_size,
        npkt = device.num_packets,
        kib = bytes_per_iter / 1024,
    );
    eprintln!(
        "  condition={cond}  iterations={it}  → {out}",
        cond = cfg.condition,
        it = cfg.iterations,
        out = cfg.output,
    );

    let mut total_bytes: u64 = 0;
    let loop_start = Instant::now();

    for iter in 0..cfg.iterations {
        let ru_before = Rusage::snapshot();

        // ── timed section ────────────────────────────────────────────────────
        let t0 = Instant::now();
        let actual_bytes = device.capture()?;
        let duration_ns = t0.elapsed().as_nanos() as u64;
        // ── end timed section ────────────────────────────────────────────────

        let ru_after = Rusage::snapshot();
        let delta = ru_after.delta(&ru_before);

        total_bytes += actual_bytes;

        let mut row = Row::new(cfg.condition.as_str(), "iso");
        row.iteration = iter;
        row.bytes = actual_bytes;
        row.duration_ns = duration_ns;
        row.user_cpu_us = delta.user_us;
        row.sys_cpu_us = delta.sys_us;
        row.rss_peak_kb = delta.rss_peak_kb;
        row.guest_mem_bytes = csv::guest_mem_bytes();
        logger.write(&row)?;

        if iter % 20 == 0 || iter == cfg.iterations - 1 {
            let mb_s = (actual_bytes as f64 / 1_048_576.0)
                / (duration_ns as f64 / 1_000_000_000.0);
            let rtt_us = duration_ns / 1_000;
            eprintln!(
                "  [{iter:3}/{it}]  {mb_s:6.1} MB/s  RTT={rtt_us} µs  bytes={actual_bytes}",
                it = cfg.iterations,
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
// CLI configuration
// ---------------------------------------------------------------------------

struct Config {
    output: String,
    vid: u16,
    pid: u16,
    iterations: u32,
    condition: String,
    iface: u8,
    alt: u8,
    ep_in: u8,
    packet_size: u32,
    num_packets: u32,
}

impl Config {
    fn from_args(args: Vec<String>) -> Result<Self> {
        let output = args
            .get(1)
            .map(String::as_str)
            .unwrap_or("bench_w_iso.csv")
            .to_owned();
        let vid_pid = args.get(2).map(String::as_str).unwrap_or("046d:086c");
        let iterations: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(200);

        let condition = named_arg(&args, "--condition")
            .unwrap_or_else(|| default_condition().to_owned());

        let iface: u8 = named_arg(&args, "--iface")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let alt: u8 = named_arg(&args, "--alt")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        let ep_in: u8 = named_arg(&args, "--ep")
            .and_then(|s| parse_hex_u8(&s).ok())
            .unwrap_or(0x81);
        let packet_size: u32 = named_arg(&args, "--pkt-size")
            .and_then(|s| s.parse().ok())
            .unwrap_or(1024);
        let num_packets: u32 = named_arg(&args, "--num-pkts")
            .and_then(|s| s.parse().ok())
            .unwrap_or(32);

        let (vid, pid) = parse_vid_pid(vid_pid)?;
        Ok(Self {
            output,
            vid,
            pid,
            iterations,
            condition,
            iface,
            alt,
            ep_in,
            packet_size,
            num_packets,
        })
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

fn named_arg(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
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

fn parse_hex_u8(s: &str) -> Result<u8> {
    let s = s.trim_start_matches("0x").trim_start_matches("0X");
    u8::from_str_radix(s, 16).map_err(|e| anyhow!("invalid hex byte {s:?}: {e}"))
}

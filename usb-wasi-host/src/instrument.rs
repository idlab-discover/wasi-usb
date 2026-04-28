// Copyright (c) 2026 IDLab Discover
// SPDX-License-Identifier: MIT

//! Lightweight call-tracing for WASI-USB host overhead analysis.
//!
//! Provides [`CallTrace`], a RAII guard that records the wall-clock duration
//! of a host-side USB operation and logs it on drop.  On Linux it additionally
//! captures the delta of voluntary and non-voluntary context switches by
//! reading `/proc/self/status`, giving a proxy for OS scheduling pressure
//! during each WIT host call.
//!
//! # Activation
//!
//! This module uses `log::info!` with a dedicated target so it can be
//! filtered independently of the normal host log output:
//!
//! ```text
//! # show only trace lines, suppress everything else:
//! RUST_LOG=wasi_usb_trace=info usb-wasi-host -c component.wasm ...
//!
//! # show traces AND normal info:
//! RUST_LOG=wasi_usb_trace=info,usb_wasi_host=info usb-wasi-host ...
//! ```
//!
//! # Log format
//!
//! Each call produces **one line** on `stderr`:
//!
//! ```text
//! [INFO  wasi_usb_trace] op=submit_transfer dur_us=42 ctx_vol_delta=0 ctx_nvol_delta=1 xfer_type=Bulk len=65536 dir=In
//! [INFO  wasi_usb_trace] op=await_transfer  dur_us=18 ctx_vol_delta=1 ctx_nvol_delta=0 actual_len=65536
//! ```
//!
//! Fields:
//! - `op`             — WIT method name
//! - `dur_us`         — host-side wall-clock duration in **microseconds**
//! - `ctx_vol_delta`  — delta voluntary context switches (Linux only)
//! - `ctx_nvol_delta` — delta non-voluntary context switches (Linux only)
//! - remainder        — caller-supplied `detail` string (key=value pairs)
//!
//! # Usage
//!
//! ```rust
//! use crate::instrument::CallTrace;
//!
//! fn submit_transfer(&mut self, ...) {
//!     let _t = CallTrace::enter("submit_transfer")
//!         .detail(&format!("xfer_type={:?} len={} dir={:?}", xfer_type, len, dir));
//!     // ... existing implementation ...
//! }  // _t drops here → log line emitted
//! ```
//!
//! # Performance impact
//!
//! When `RUST_LOG` does not include `wasi_usb_trace`, the `log::info!` call
//! is gated by a fast atomic level-check (no allocation, no I/O).  The
//! `Instant::now()` call in `enter()` always runs (~20 ns on modern hardware),
//! but this is negligible compared to USB transfer latencies (≥10 µs).
//!
//! On Linux, reading `/proc/self/status` costs ~5–15 µs per call (one
//! `open(2)` + `read(2)` + `close(2)`).  This is done only when the log
//! target is active.  If measuring sub-10-µs operations you may want to
//! disable ctx-switch tracking by compiling with `--cfg no_ctx_switches`.

use std::time::Instant;

// ── Public API ────────────────────────────────────────────────────────────────

/// RAII call-trace guard.  Drop it to emit the log line.
///
/// Construct via [`CallTrace::enter`].  Optionally chain [`CallTrace::detail`]
/// to append structured key=value information to the log line.
pub struct CallTrace {
    op: &'static str,
    started: Instant,
    detail: String,
    #[cfg(target_os = "linux")]
    ctx_start: Option<CtxSwitches>,
}

impl CallTrace {
    /// Start timing `op`.  The returned guard emits a log line on drop.
    #[inline]
    pub fn enter(op: &'static str) -> Self {
        Self {
            op,
            started: Instant::now(),
            detail: String::new(),
            #[cfg(target_os = "linux")]
            ctx_start: CtxSwitches::read(),
        }
    }

    /// Append a detail string (typically `"key=val key2=val2"`).
    /// Builder-style: consumes and returns `self` for chaining.
    #[inline]
    pub fn detail(mut self, kv: &str) -> Self {
        if !self.detail.is_empty() {
            self.detail.push(' ');
        }
        self.detail.push_str(kv);
        self
    }
}

impl Drop for CallTrace {
    fn drop(&mut self) {
        // Fast path: if the target is not enabled, skip everything.
        if !log::log_enabled!(target: "wasi_usb_trace", log::Level::Info) {
            return;
        }

        let dur_us = self.started.elapsed().as_micros();

        #[cfg(target_os = "linux")]
        if let Some(start) = self.ctx_start {
            if let Some(end) = CtxSwitches::read() {
                log::info!(target: "wasi_usb_trace",
                    "op={} dur_us={} ctx_vol_delta={} ctx_nvol_delta={} {}",
                    self.op,
                    dur_us,
                    end.voluntary.saturating_sub(start.voluntary),
                    end.nonvoluntary.saturating_sub(start.nonvoluntary),
                    self.detail,
                );
                return;
            }
        }

        log::info!(target: "wasi_usb_trace",
            "op={} dur_us={} {}",
            self.op, dur_us, self.detail,
        );
    }
}

// ── Linux context-switch helper ───────────────────────────────────────────────

#[cfg(target_os = "linux")]
#[derive(Copy, Clone)]
struct CtxSwitches {
    voluntary: u64,
    nonvoluntary: u64,
}

#[cfg(target_os = "linux")]
impl CtxSwitches {
    /// Parse `/proc/self/status` for `voluntary_ctxt_switches` and
    /// `nonvoluntary_ctxt_switches`.  Returns `None` if unavailable.
    fn read() -> Option<Self> {
        // Only read when the trace target is actually enabled, to avoid the
        // ~10 µs /proc overhead on every call during non-tracing runs.
        if !log::log_enabled!(target: "wasi_usb_trace", log::Level::Info) {
            return None;
        }

        let s = std::fs::read_to_string("/proc/self/status").ok()?;
        let mut vol: Option<u64> = None;
        let mut nvol: Option<u64> = None;

        for line in s.lines() {
            if vol.is_none() {
                if let Some(v) = line.strip_prefix("voluntary_ctxt_switches:") {
                    vol = v.trim().parse().ok();
                }
            }
            if nvol.is_none() {
                if let Some(v) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
                    nvol = v.trim().parse().ok();
                }
            }
            if vol.is_some() && nvol.is_some() {
                break;
            }
        }

        Some(Self {
            voluntary: vol?,
            nonvoluntary: nvol?,
        })
    }
}

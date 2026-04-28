//! USB Bulk-Only-Transport (BOT) abstraction.
//!
//! `BulkTransport` is the target-independent trait. Concrete implementations
//! are cfg-gated so the same `mass_storage::ScsiDevice<T>` generic compiles
//! for *both* runtimes without modification — the "one source, two targets"
//! claim of the thesis.
//!
//! | cfg                                         | impl             | condition |
//! |---------------------------------------------|------------------|-----------|
//! | not(target_family=wasm)                     | `NativeTransport`| C3        |
//! | target_family=wasm + feature=c4-rusb-wasi   | `NativeTransport`| C4        |
//! | target_family=wasm (no c4-rusb-wasi feature)| `WasmTransport`  | C5        |

use anyhow::Result;

// ---------------------------------------------------------------------------
// Transport trait
// ---------------------------------------------------------------------------

/// Minimal CBW→[data]→CSW abstraction over a USB Bulk-Only-Transport device.
///
/// Both directions are exposed:
/// - `command_in`  — device→host data (e.g. SCSI READ)
/// - `command_out` — host→device data or no data (e.g. SCSI TEST UNIT READY)
pub trait BulkTransport {
    /// Send a Command Block Wrapper with IN direction, receive `transfer_len`
    /// bytes of data, validate the Command Status Wrapper.
    /// Returns the data bytes (length ≤ transfer_len).
    fn command_in(&mut self, cbwcb: &[u8], transfer_len: u32) -> Result<Vec<u8>>;

    /// Send a Command Block Wrapper with OUT direction, optionally write
    /// `data`, validate the Command Status Wrapper.
    fn command_out(&mut self, cbwcb: &[u8], data: Option<&[u8]>) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Native (rusb) implementation — C3 (native) and C4 (wasm + c4-rusb-wasi)
// ---------------------------------------------------------------------------

#[cfg(any(not(target_family = "wasm"), feature = "c4-rusb-wasi"))]
mod native;

#[cfg(any(not(target_family = "wasm"), feature = "c4-rusb-wasi"))]
pub use native::NativeTransport;

// ---------------------------------------------------------------------------
// WASM (wit-bindgen) implementation — C5 raw WIT path
// ---------------------------------------------------------------------------

#[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
mod wasm;

#[cfg(all(target_family = "wasm", not(feature = "c4-rusb-wasi")))]
pub use wasm::WasmTransport;

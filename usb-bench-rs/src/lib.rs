//! Shared benchmark harness library for the WASI-USB thesis evaluation.
//!
//! # Modules
//! - [`csv`] — unified CSV logger (schema shared with C bench)
//! - [`bulk_only`] — `BulkTransport` trait + `NativeTransport` (rusb, C3) /
//!   `WasmTransport` (wit-bindgen, C5 — added in F3)
//! - [`mass_storage`] — target-independent `ScsiDevice<T: BulkTransport>`

pub mod bulk_only;
pub mod csv;
pub mod mass_storage;

/// WIT bindings for the `guest` world (wasm32-wasip2 only).
/// All wasm transport modules use these types instead of calling
/// `wit_bindgen::generate!` themselves — each binary must only have one
/// `component-type` custom section, which lives here in the library.
#[cfg(target_family = "wasm")]
pub mod bindings;

//! Central WIT binding generation for the wasm32-wasip2 target.
//!
//! `wit_bindgen::generate!` embeds a `component-type` custom section into the
//! wasm object it is compiled into.  If multiple compilation units in the same
//! binary each call `generate!` for the same world, the linker concatenates
//! those custom sections and the component toolchain rejects the result as
//! malformed.
//!
//! Solution: call `generate!` exactly **once**, here, in the library crate.
//! All wasm transport modules (`bulk_only/wasm.rs`, `w_ctrl`, `w_int`,
//! `w_iso`) import from `crate::bindings` (or `usb_bench_rs::bindings`)
//! instead of calling `generate!` themselves.

wit_bindgen::generate!({
    world: "guest",
    path: "../wit",
    generate_all,
});

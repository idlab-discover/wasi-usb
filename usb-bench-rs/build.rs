use std::path::PathBuf;

fn main() {
    // For C4 (wasm32-wasip2 + c4-rusb-wasi feature): pass cguest_component_type.o
    // as a linker arg so that wasm-component-ld can resolve the component:usb/* WIT
    // imports that come from libusb-wasi.a (via cguest.o's proper import annotations).
    //
    // Without this custom section, wasm-component-ld cannot encode the component
    // because it sees `env::component_usb_*` imports with no matching WIT world.
    //
    // This mirrors what rusb-wasi/examples/wasi-workload/build.rs does.
    if std::env::var("CARGO_CFG_TARGET_OS") == Ok("wasi".into()) {
        // For C4: pass usb-bench-c/bindings/guest_component_type.o as linker arg.
        //
        // This object provides the "component-type:cguest" custom section which
        // tells wasm-component-ld how to map the cguest.o WIT bindings to the
        // component:usb/* interfaces. It uses component:usb/*@0.2.1 (matching
        // libusb-wasi.a), with no wasi:cli/run version constraint — compatible
        // with Rust 1.93.1's wasm32-wasip2 target (@0.2.0).
        //
        // Do NOT use rusb-wasi's cguest_component_type.o; it requires
        // wasi:cli/run@0.2.5 which Rust 1.93.1 does not provide.
        let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
        let obj = manifest
            .parent().unwrap()   // wasi-usb/
            .join("usb-bench-c/bindings/guest_component_type.o");
        if obj.exists() {
            println!("cargo:rustc-link-arg={}", obj.display());
        }
    }
}

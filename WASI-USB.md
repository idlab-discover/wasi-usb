# wasi-usb (Host Runtime)

This repository contains the host runtime implementation and WIT definitions for the **WASI-USB** proposal. It bridges the gap between WebAssembly (Wasm) guests and the underlying Operating System's USB APIs.

## Role in the Project
The `wasi-usb` host is responsible for executing Wasm components and handling their USB-related requests. When a WebAssembly guest (compiled against `libusb-wasi` or `rusb-wasi`) initiates a USB operation, it calls a WASI-USB interface method. The host runtime intercepts this call and securely forwards it to the native OS using libusb/rusb.

## Compilation and Execution

The host is written in Rust. You can compile it using Cargo:

```bash
cargo build --release
```

Detailed instructions on how to compile specific workloads and execute them via this host can be found in [COMPILING.md](./COMPILING.md). 

## WIT Interfaces
This repository also contains the `wit` directory, which defines the standard WASI-USB interfaces (e.g., `component:usb/transfers@0.2.1`) used by the guest libraries.
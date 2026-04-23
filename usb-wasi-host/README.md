# usb-wasi-host

This directory contains the **WASI-USB Host Runtime** — a Wasmtime-based runner that provides WebAssembly components with safe, capability-based access to USB hardware.

Architecture: **dumb host, smart guest**. The host exposes only generic USB primitives (control/bulk/interrupt/isochronous transfers, device enumeration, hotplug). All protocol-specific logic (UVC, MJPEG reassembly, etc.) lives in the guest component.

## Key Features

- **WASI-USB Interface**: Full control, bulk, interrupt, and isochronous transfer support including USB 3.0 Bulk Streams.
- **Capability-Based Security**: Strictly enforces device allow-lists/deny-lists for guest components.
- **Async Execution**: Fully utilises Wasmtime 31.0.0 async component model for non-blocking I/O.
- **Canonical WIT**: Implements `component:usb@0.2.1` as defined in `../wit/` (mirror of the canonical version).

## Building

```bash
cargo build --release
```

The resulting binary will be located at `target/release/usb-wasi-host` (relative to `wasi-usb/`).

## Usage

```
Usage: usb-wasi-host [OPTIONS] --component-path <COMPONENT_PATH>

Options:
  -c, --component-path <COMPONENT_PATH>  Path to the .wasm component
  -d, --usb-devices <USB_DEVICES>        USB devices (format VID:PID) to allow or deny
  -u, --use-allow-list                   Treat the -d list as an allow-list (default is deny)
  -l, --debug_level <DEBUG_LEVEL>        Log level (trace, debug, info, warn, error) [default: info]
  -h, --help                             Print help
```

### Examples

#### List USB Devices (lsusb)
```bash
sudo target/release/usb-wasi-host \
    --component-path ../usb-wasm/out/lsusb.wasm
```

#### Webcam Demo (UVC capture)
```bash
sudo target/release/usb-wasi-host \
    --component-path ../usb-wasm/out/webcam.wasm
```

> **Note**: `sudo` is required on Linux/macOS to claim physical USB interfaces unless appropriate `udev` rules are configured.

## Project Structure

- `src/main.rs`: Entry point, CLI parsing, and all WIT interface implementations.
- `src/usb_backend.rs`: OS-specific USB logic (via `libusb`).
- `../wit/`: WIT definitions — exact mirror of the canonical `component:usb@0.2.1` package in `wasi-usb/wit/`.

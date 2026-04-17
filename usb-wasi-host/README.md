# usb-wasi-host

This directory contains the **WASI-USB Host Runtime**, a specialized Wasmtime-based runner that provides WebAssembly components with safe, capability-based access to USB hardware.

## Key Features

- **WASI-USB Interface**: Implements full control, bulk, interrupt, and isochronous transfer support.
- **Computer Vision Acceleration**: Provides optimized host-side UVC streaming and YOLOv8 inference resources to bypass the overhead of raw MJPEG/RGB processing in the Wasm sandbox.
- **Capability-Based Security**: Strictly enforces device allow-lists/deny-lists for guest components.
- **Async Execution**: Fully utilizes Wasmtime 31.0.0 async component model for non-blocking I/O.

## Building

A release version of the runtime can be built by running the following command in this folder:

```bash
cargo build --release
```

The resulting binary will be located at `../../target/release/usb-wasi-host`.

## Usage

The host runtime accepts a path to a compiled WASM component and various configuration flags.

```bash
Usage: usb-wasi-host [OPTIONS] --component-path <COMPONENT_PATH>

Options:
  -c, --component-path <COMPONENT_PATH>  Path to the .wasm component
  -d, --usb-devices <USB_DEVICES>        USB devices (format VID:PID) to allow or deny
  -u, --use-allow-list                   Treat the -d list as an allow-list (default is deny)
  -l, --debug_level <DEBUG_LEVEL>        Log level (trace, debug, info, warn, error) [default: info]
      --enable-yolo                      Enable YOLOv8 inference acceleration and timing logs
  -h, --help                             Print help
```

### Examples

#### 1. List USB Devices (lsusb)
```bash
sudo ../../target/release/usb-wasi-host \
    --component-path ../../usb-wasm/out/lsusb.wasm
```

#### 2. Real-time YOLOv8 Inference (Terminal Only)
The YOLO demo uses a pre-composed component that links the webcam-cv source and the detector sink.

```bash
sudo ../../target/release/usb-wasi-host \
    --component-path ../../usb-wasm/out/yolo-terminal-composed.wasm \
    --enable-yolo -- yolov8n.onnx
```

> [!NOTE]
> `sudo` is required on Linux/macOS to allow the host to claim physical USB interfaces unless appropriate `udev` rules are configured.

## Project Structure

- `src/main.rs`: Entry point, CLI parsing, and WASI interface implementations.
- `src/usb_backend.rs`: OS-specific USB logic (via `libusb`).
- `../wit/`: Interface definitions (WIT) shared between host and guest.
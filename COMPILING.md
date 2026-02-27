# Compiling and Benchmarking WASI-USB

This document provides detailed instructions for compiling and running USB examples and workloads natively and within the WASI-USB environment.

## 1. Local Environments

| Library | Directory | Description |
| :--- | :--- | :--- |
| **libusb-vanilla** | `libusb-vanilla/` | Official, unmodified libusb (Baseline). |
| **libusb-wasi** | `libusb-wasi/` | C implementation of libusb with WASI support (Leroy). |
| **rusb-wasi** | `rusb-wasi/` | Rust implementation (rusb) targeting WASI-USB. |
| **wasi-usb-host** | `wasi-usb/usb-wasi-host/` | Host-side implementation to run WASI components. |

---

## 2. Compilation

### Automated build
Run the master script to build all benchmarking versions across all 5 targets:
```bash
./benchmarks/build_all.sh
```

---

## 3. Benchmarking Suite

There are two main trajectories for benchmarking your WASI-USB setup:

### Scenario 1: Theoretical Benchmarking (Loopback)
This scenario measures pure overhead using simple echo/loopback payloads. It requires a dedicated test device (e.g., a Raspberry Pi Pico or ESP32) flashed with a loopback/vendor firmware that blindly returns incoming data. **Do not run these on Mass Storage Devices.**

### Scenario 2: Real-World Application Benchmarking (Mass Storage)
This scenario measures the execution time of functional workloads (like reading files) on a real USB flash drive. You can benchmark this by prepending the `time` command to the [Mass Storage I/O commands](#c-mass-storage-io-read_device--rusb_workload) in Section 4.C.

By executing these scripts with `time`, you measure the exact wall-clock overhead ("real" time) WASI adds to a full end-to-end USB transaction sequence (setup, SCSI MBR parsing, filesystem traversal, array allocations, ...).

**Examples (Native vs WASI)**:
```bash
# C Native
sudo time ./benchmarks/c/utils/read_device_leroy_native <VID> <PID>

# Rust WASI
cd wasi-usb/usb-wasi-host && sudo time cargo run --release -- --component-path ../../benchmarks/rust/utils/rusb_workload.component.wasm
```
---

### A. Latency (Ping-Pong RTT) - *Scenario 1 Only*
Measures the time for a request-response cycle. Requires a loopback device.

| Variant | Target | Command |
| :--- | :--- | :--- |
| **Vanilla C** | Native | `sudo ./benchmarks/c/latency_vanilla_native <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <SIZE_BYTES> [ITERATIONS] [VARIANT]` |
| **Leroy C** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run --release -- --component-path ../../benchmarks/c/latency_leroy.component.wasm <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <SIZE_BYTES> [ITERATIONS] [VARIANT]` |
| **rusb (Rust)** | Native | `sudo ./benchmarks/rust/latency_native <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <SIZE_BYTES> [ITERATIONS] [VARIANT]` |
| **rusb (Rust)** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run --release -- --component-path ../../benchmarks/rust/latency_rusb.component.wasm <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <SIZE_BYTES> [ITERATIONS] [VARIANT]` |

### B. Throughput (Bulk Read/Write)
Measures the pure data transfer rate (MB/s) using Mass Storage SCSI Read/Write commands. Requires a Mass Storage Device (USB stick).

| Variant | Target | Command |
| :--- | :--- | :--- |
| **Vanilla C** | Native | `sudo ./benchmarks/c/throughput_vanilla_native <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <START_LBA> <SIZE_MB> <RUNS> [VARIANT]` |
| **Leroy C** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run --release -- --component-path ../../benchmarks/c/throughput_leroy.component.wasm <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <START_LBA> <SIZE_MB> <RUNS> [VARIANT]` |
| **rusb (Rust)** | Native | `sudo ./benchmarks/rust/throughput_native <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <START_LBA> <SIZE_MB> <RUNS> [VARIANT]` |
| **rusb (Rust)** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run --release -- --component-path ../../benchmarks/rust/throughput_rusb.component.wasm <VID> <PID> <INTERFACE> <EP_OUT> <EP_IN> <START_LBA> <SIZE_MB> <RUNS> [VARIANT]` |


---

## 4. Utility Workloads

These tools are for device discovery and functional I/O testing (Read/Write).

### A. Device Discovery (`lsusb`)
Lists all connected USB devices and their descriptors.

| Variant | Target | Command |
| :--- | :--- | :--- |
| **Vanilla C** | Native | `sudo ./benchmarks/c/utils/lsusb_vanilla_native` |
| **Leroy C** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run -- --component-path ../../benchmarks/c/utils/lsusb.component.wasm` |
| **rusb (Rust)** | Native | `sudo ./benchmarks/rust/utils/lsusb_native` |
| **rusb (Rust)** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run -- --component-path ../../benchmarks/rust/utils/lsusb_rusb.component.wasm` |

### B. Raw Bulk I/O Loopback (`usb_io`)
Sends and receives raw data. Ideal for loopback devices (e.g. VID:PID `abcd:1234`).

| Variant | Target | Command |
| :--- | :--- | :--- |
| **Vanilla C** | Native | `sudo ./benchmarks/c/utils/usb_io_vanilla_native <VID> <PID> <INTERFACE> <OUT_EP> <IN_EP> [MSG]` |
| **Leroy C** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run -- --component-path ../../benchmarks/c/utils/usb_io.component.wasm <VID> <PID> <INTERFACE> <OUT_EP> <IN_EP> [MSG]` |
| **rusb (Rust)** | Native | `sudo ./benchmarks/rust/utils/usb_io_native <VID> <PID> <OUT_EP> <IN_EP> [MSG]` |
| **rusb (Rust)** | WASI | `cd wasi-usb/usb-wasi-host && sudo cargo run -- --component-path ../../benchmarks/rust/utils/usb_io_rusb.component.wasm <VID> <PID> <OUT_EP> <IN_EP> [MSG]` |

### C. Mass Storage I/O (`read_device` / `rusb_workload`)
Performs functional tests on USB drives (FAT filesystem).

| Variant | Target | Mode | Command |
| :--- | :--- | :--- | :--- |
| **Vanilla C** | Native | Read | `sudo ./benchmarks/c/utils/read_device_vanilla_native <VID> <PID>` |
| **Vanilla C** | Native | **Write** | `sudo ./benchmarks/c/utils/read_device_vanilla_native --write-test` |
| **Leroy C** | WASI | Read | `cd wasi-usb/usb-wasi-host && sudo cargo run -- --component-path ../../benchmarks/c/utils/read_device.component.wasm` |
| **Leroy C** | WASI | **Write** | `cd wasi-usb/usb-wasi-host && sudo cargo run -- --component-path ../../benchmarks/c/utils/read_device.component.wasm -- --write-test` |
| **rusb (Rust)** | Native | Read/Write | `sudo ./benchmarks/rust/utils/rusb_workload_native` |
| **rusb (Rust)** | WASI | Read/Write | `cd wasi-usb/usb-wasi-host && sudo cargo run -- --component-path ../../benchmarks/rust/utils/rusb_workload.component.wasm` |

> [!CAUTION]
> **Write tests** will modify Sector 0 (MBR) or create/delete `test.txt`. Always use a sacrificial USB drive.

---

---

## 4. Troubleshooting

- **LIBUSB_ERROR_ACCESS**: 
  Direct USB access requires root. **Always use `sudo`**.
- **error: could not find \`Cargo.toml\`**:
  WASI execution must happen from the `wasi-usb/usb-wasi-host` folder.
- **SCSI Command Failed**: 
  Unmount the USB drive first if using filesystem-level tests.

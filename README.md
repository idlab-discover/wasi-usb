# WASI USB

A proposed [WebAssembly System Interface](https://github.com/WebAssembly/WASI) API for USB hardware access.

### Current Phase

WASI USB is currently in Phase 2.

### Contributors & Champions

- **Merlijn Sebrechts** (Champion)
- **Michiel Van Kenhove**
- **Friedrich Vandenberghe**
- **Sibren Wieme** (IDLab Discover - host implementation, benchmarking)

### Portability Criteria

| Platform | Architecture | Reference Hardware |
|----------|--------------|--------------------|
| Linux    | amd64        |                    |
| Linux    | aarch64      | Raspberry Pi 4      |
| macOS    | aarch64      | MacBook Pro M3 MAX |

## Repository layout

```
wasi-usb/
├── wit/                    # WIT source of truth — component:usb@0.2.1
│   ├── device.wit          # USB device management + hotplug
│   ├── transfers.wit       # Transfer types, options, results
│   ├── descriptors.wit     # Device/configuration/interface/endpoint descriptors
│   ├── configuration.wit   # Configuration values
│   ├── errors.wit          # libusb error codes
│   ├── hotplug.wit         # Hotplug events
│   └── world.wit           # host / guest / cguest / webcam-guest worlds
├── usb-wasi-host/          # wasmtime-based host binary
├── usb-wasi-guest/         # Rust guest library + examples
│   └── examples/           # All demo components
│       ├── webcam/         # UVC webcam capture (sub-crate)
│       ├── mass-storage/   # FAT32 mass storage (sub-crate)
│       ├── ps5-maze/       # Pacman maze — PS5/Xbox (sub-crate)
│       ├── xbox-maze/      # Pacman maze — Xbox (sub-crate)
│       ├── enumerate-devices-go/  # Go/tinygo device lister (sub-dir)
│       ├── lsusb.rs        # Detailed USB device listing
│       ├── enumerate-devices-rust.rs
│       ├── control.rs      # Control transfer example
│       ├── ping.rs         # Bulk OUT/IN echo
│       ├── streams-test.rs # USB 3.0 bulk streams validation
│       ├── xbox.rs         # Xbox controller reader
│       └── identity.rs     # Trivial device lister
├── usb-wasi-cguest/        # Pre-built C bindings for benchmark components
├── Justfile                # Build + run recipes for all guests
└── libusb/                 # libusb git submodule
```

## Quick start

```bash
# Build the host
just build-host

# Webcam demo (Logitech Brio 100 or any UVC webcam, sudo required)
mkdir -p out
just webcam
# Frames are written to out/latest.jpg; open in Preview and press ENTER to refresh

# List USB devices
just lsusb

# USB 3.0 bulk streams validation
just streams-test 0781 5581 0 0x02 0x81
```

## Examples

| Command | Description |
|---------|-------------|
| `just webcam` | UVC capture → `out/latest.jpg` |
| `just lsusb` | Detailed device listing |
| `just enumerate-devices-rust` | Compact device list |
| `just control` | Control transfer to Arduino |
| `just xbox` | Xbox One S controller reader |
| `just streams-test <vid> <pid> <iface> <ep_out> <ep_in>` | Bulk streams test |
| `just mass-storage tree` | FAT32 directory tree |
| `just ps5-maze` | Pacman controlled by PS5/Xbox |
| `just build-all` | Build everything |

## Benchmarking

The repository includes a full 5-condition benchmark suite (C1–C5) covering native and WASI USB access in both C and Rust.

See **[BENCHMARKING.md](./BENCHMARKING.md)** for:
- The complete benchmark matrix (conditions × workloads)
- Build instructions for all conditions including C4 (rusb → WASM via WIT)
- Run and analysis instructions
- Technical details of the C4 implementation

```bash
just bench-build   # build all 5 conditions
just bench-smoke   # quick sanity run (1 iteration per cell)
just bench-run     # full measurement round
```

## WIT design notes

- `flags event { arrived; left; }` — bitflags, not an enum. Check `Event::ARRIVED` / `Event::LEFT`.
- `await-transfer(borrow<transfer>) -> result<transfer-result, libusb-error>` — borrow semantics; returns `TransferResult { data, packets }`.
- No `await-iso-transfer` — isochronous packets are in `TransferResult.packets`.
- `enable-hotplug` returns `result<_, libusb-error>` — no pollable.

## API walk-through

See [`WASI-USB.md`](./WASI-USB.md) for the full API documentation.

## References & acknowledgements

Many thanks for valuable feedback, work and advice from:
- Warre Dujardin
- Wouter Hennen
- Robbe Leroy

This work has been partially supported by the ELASTIC project, which received funding from the Smart Networks and Services Joint Undertaking (SNS JU) under the European Union's Horizon Europe research and innovation programme under Grant Agreement No 101139067.

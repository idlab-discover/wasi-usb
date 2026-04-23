# wasi-usb Justfile
# Usage: just <recipe>
# Requires: just, cargo, cargo-component, wasm32-wasip2 toolchain, wasm-tools

HOST := "./usb-wasi-host/target/release/usb-wasi-host"
GUEST_TARGET := "wasm32-wasip2"
GUEST_OUT := "usb-wasi-guest/target/wasm32-wasip2/release/examples"
SUB_OUT := "usb-wasi-guest/target/wasm32-wasip2/release"

# ── Host ────────────────────────────────────────────────────────────────────

build-host:
    cd usb-wasi-host && cargo build --release

# ── Webcam (sub-crate with WASI filesystem + UVC) ───────────────────────────

build-webcam:
    mkdir -p out
    cargo build --target {{GUEST_TARGET}} --release -p webcam \
        --manifest-path usb-wasi-guest/examples/webcam/Cargo.toml
    cp {{SUB_OUT}}/webcam.wasm out/webcam.wasm

webcam: build-host build-webcam
    mkdir -p out
    sudo {{HOST}} -c out/webcam.wasm

# ── Generic example builder ─────────────────────────────────────────────────

build-example name:
    mkdir -p out
    cargo build --example {{name}} --target {{GUEST_TARGET}} --release \
        --manifest-path usb-wasi-guest/Cargo.toml
    cp {{GUEST_OUT}}/{{name}}.wasm out/{{name}}.wasm

# ── Per-example run recipes ─────────────────────────────────────────────────

lsusb: build-host (build-example "lsusb")
    sudo {{HOST}} -c out/lsusb.wasm

enumerate-devices-rust: build-host (build-example "enumerate-devices-rust")
    sudo {{HOST}} -c out/enumerate-devices-rust.wasm

control: build-host (build-example "control")
    sudo {{HOST}} -c out/control.wasm

identity: build-host (build-example "identity")
    sudo {{HOST}} -c out/identity.wasm

xbox: build-host (build-example "xbox")
    sudo {{HOST}} -c out/xbox.wasm

ping *args: build-host (build-example "ping")
    sudo {{HOST}} -c out/ping.wasm -- {{args}}

streams-test *args: build-host (build-example "streams-test")
    sudo {{HOST}} -c out/streams-test.wasm -- {{args}}

# ── Sub-crate guests ─────────────────────────────────────────────────────────

build-mass-storage:
    mkdir -p out
    cargo build --target {{GUEST_TARGET}} --release -p mass-storage \
        --manifest-path usb-wasi-guest/examples/mass-storage/Cargo.toml
    cp {{SUB_OUT}}/mass-storage.wasm out/mass-storage.wasm

mass-storage *args: build-host build-mass-storage
    sudo {{HOST}} -c out/mass-storage.wasm -- {{args}}

build-ps5-maze:
    mkdir -p out
    cargo build --target {{GUEST_TARGET}} --release -p ps5-maze \
        --manifest-path usb-wasi-guest/examples/ps5-maze/Cargo.toml
    cp {{SUB_OUT}}/ps5_maze.wasm out/ps5-maze.wasm

ps5-maze: build-host build-ps5-maze
    sudo {{HOST}} -c out/ps5-maze.wasm

build-xbox-maze:
    mkdir -p out
    cargo build --target {{GUEST_TARGET}} --release -p xbox-maze \
        --manifest-path usb-wasi-guest/examples/xbox-maze/Cargo.toml
    cp {{SUB_OUT}}/xbox_maze.wasm out/xbox-maze.wasm

xbox-maze: build-host build-xbox-maze
    sudo {{HOST}} -c out/xbox-maze.wasm

# ── Go guest (requires tinygo + wit-bindgen-go) ──────────────────────────────

build-enumerate-devices-go:
    cd usb-wasi-guest/examples/enumerate-devices-go && ./build.sh

enumerate-devices-go: build-host build-enumerate-devices-go
    sudo {{HOST}} -c usb-wasi-guest/examples/enumerate-devices-go/out/main.component.wasm

# ── Build all guests ─────────────────────────────────────────────────────────

build-all: build-host build-webcam \
    (build-example "lsusb") \
    (build-example "enumerate-devices-rust") \
    (build-example "control") \
    (build-example "identity") \
    (build-example "xbox") \
    (build-example "ping") \
    (build-example "streams-test") \
    build-mass-storage build-ps5-maze build-xbox-maze

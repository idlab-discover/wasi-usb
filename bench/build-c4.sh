#!/usr/bin/env bash
# build-c4.sh — Cross-compile rusb benchmarks to wasm32-wasip2 using
#               libusb-wasi.a (Robbe Leroy's WIT-backed libusb build).
#
# Volgt de aanpak uit libusb1-sys/README.md "Cross-Compiling" sectie:
# PKG_CONFIG_LIBDIR + PKG_CONFIG_ALLOW_CROSS sturen libusb1-sys's build.rs
# zodat het linkt tegen libusb-wasi.a i.p.v. vendored libusb te bouwen.
#
# Resultaat (C4 condition — wasi-rusb):
#   usb-bench-rs/target-wasi-rusb/wasm32-wasip2/release/w_{bulk,ctrl,int,iso}.wasm
#
# Gebruik:
#   bash bench/build-c4.sh
#   just bench-build   (roept dit script ook aan)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYSROOT="${ROOT}/sysroot-wasi"
BENCH_RS="${ROOT}/usb-bench-rs"

# Sanity-check: libusb-wasi.a aanwezig?
if [[ ! -f "${ROOT}/libusb/libusb-wasi.a" ]]; then
    echo "ERROR: ${ROOT}/libusb/libusb-wasi.a niet gevonden." >&2
    echo "       Bouw Robbe's libusb-wasi eerst (zie libusb/BUILDING_WASI.md)." >&2
    exit 1
fi

# Sanity-check: guest_component_type.o aanwezig?
GUEST_OBJ="${ROOT}/usb-bench-c/bindings/guest_component_type.o"
if [[ ! -f "${GUEST_OBJ}" ]]; then
    echo "ERROR: ${GUEST_OBJ} niet gevonden." >&2
    echo "       Run: cd usb-bench-c && cmake --build build-wasi" >&2
    exit 1
fi

echo "══════════════════════════════════════════════════"
echo "  C4 build: rusb → libusb-wasi.a → WIT"
echo "  Sysroot : ${SYSROOT}"
echo "  Output  : ${BENCH_RS}/target-wasi-rusb/wasm32-wasip2/release/"
echo "══════════════════════════════════════════════════"

# Stuur pkg-config naar onze WASI-sysroot (wijst naar libusb-wasi.a, NIET naar
# de host-libusb). PKG_CONFIG_ALLOW_CROSS=1 is vereist door pkg-config-rs.
export PKG_CONFIG_LIBDIR="${SYSROOT}/usr/lib/pkgconfig"
export PKG_CONFIG_SYSROOT_DIR="${SYSROOT}"
export PKG_CONFIG_ALLOW_CROSS=1

# GEEN guest_component_type.o nodig voor Rust-binaries:
# usb-bench-rs/src/bindings.rs roept wit_bindgen::generate! aan en levert
# de component-type custom section. guest_component_type.o is alleen nodig
# voor de C-binaries (C2) die geen wit-bindgen hebben.

cd "${BENCH_RS}"
cargo build --release --bins \
    --target wasm32-wasip2 \
    --features c4-rusb-wasi \
    --target-dir target-wasi-rusb

echo ""
echo "✓ C4 binaries:"
ls -lh target-wasi-rusb/wasm32-wasip2/release/w_*.wasm 2>/dev/null || \
    echo "  (geen .wasm gevonden — build mislukt?)"

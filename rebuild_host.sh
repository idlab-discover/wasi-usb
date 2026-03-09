#!/bin/bash
set -e
echo "Cleaning environment..."
unset PKG_CONFIG_PATH
unset LIBUSB_STATIC
unset LIBUSB_DIR
unset CC
unset CFLAGS
unset LDFLAGS
unset CARGO_TARGET_DIR
export LIBUSB_DIR=/Users/sibrenwieme/Documents/Masterproef/wasi-usb/libusb

echo "Cleaning cargo..."
cargo clean

echo "Building host..."
cargo build --release

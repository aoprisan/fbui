#!/bin/sh
# Build the bare-metal viewer and the raw kernel8.img a Pi's firmware boots.
# Needs: rustup target add aarch64-unknown-none; rustup component add llvm-tools
set -eu
cd "$(dirname "$0")/.."
cargo build --release
elf=target/aarch64-unknown-none/release/doc-viewer-rpi3
objcopy=$(ls "$(rustc --print sysroot)"/lib/rustlib/*/bin/llvm-objcopy | head -n1)
"$objcopy" -O binary "$elf" target/kernel8.img
echo "built $elf"
echo "built target/kernel8.img ($(wc -c < target/kernel8.img) bytes)"

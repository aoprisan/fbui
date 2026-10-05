#!/bin/sh
# Run the viewer in QEMU's Raspberry Pi 3B model. This terminal is the UART:
# type arrows, PgUp/PgDn, space, +/-, w, p, Esc, Enter. Ctrl-A X quits.
# Add `-display none` (or run headless via scripts/qemu_drive.py) without a GUI.
set -eu
cd "$(dirname "$0")/.."
[ -f target/kernel8.img ] || scripts/build.sh
exec qemu-system-aarch64 -M raspi3b -kernel target/kernel8.img -serial mon:stdio "$@"

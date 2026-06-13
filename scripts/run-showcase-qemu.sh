#!/usr/bin/env bash
#
# Run the `showcase` all-widgets app inside a Linux guest (e.g. QEMU on macOS —
# see fbui-platform/docs/qemu.md for the host-side launch). This is the graphical
# sibling of scripts/qemu-test.sh: where that runs the platform's device tests and
# the `echo` demo, this builds and runs the full widget showcase.
#
# Run it *inside the Linux guest*, as root, from a real text VT (press
# Ctrl-Alt-F2 in the QEMU window — not an SSH/serial session, which has no
# console for KD_GRAPHICS):
#
#   sudo ./scripts/run-showcase-qemu.sh            # debug build
#   sudo ./scripts/run-showcase-qemu.sh --release  # optimized build
#
# It needs a DRM device with a connected connector: boot QEMU with
# `-device virtio-gpu-pci` (preferred) or `-vga std`, plus `-device usb-kbd` and
# `-device usb-tablet` for input. In the app: Tab/arrows move focus, Space/Enter
# activate, drag the slider, Esc quits (the VT guard restores text mode on exit).
#
# NOTE: like the rest of the QEMU tooling here, this was written but not executed
# from the development container (no qemu/kvm/DRM there); treat your first run as
# the validation step.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

log()  { printf '\033[1;34m[run-showcase]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[run-showcase]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[run-showcase]\033[0m %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run as root (sudo $0 $*) — the platform opens DRM and input nodes"

# Print the DRM driver behind a card node (e.g. "virtio_gpu", "bochs-drm").
card_driver() {
  local name; name="$(basename "$1")"
  basename "$(readlink -f "/sys/class/drm/$name/device/driver" 2>/dev/null || true)" 2>/dev/null || true
}

# Same probe as scripts/qemu-test.sh, so a missing GPU/input device is an obvious
# diagnostic rather than a cryptic open() failure deep in the platform.
probe() {
  log "kernel: $(uname -srm)"
  if ls /dev/dri/card* >/dev/null 2>&1; then
    for node in /dev/dri/card*; do log "DRM: $node  driver=$(card_driver "$node")"; done
  else
    warn "no /dev/dri — add -device virtio-gpu-pci or -vga std to QEMU (will try fbdev)"
  fi
  [ -c /dev/fb0 ] && log "fbdev: /dev/fb0 present" || warn "no /dev/fb0 (fbdev fallback unavailable)"
  if ls /dev/input/event* >/dev/null 2>&1; then
    log "input: $(ls /dev/input/event* | tr '\n' ' ')"
  else
    warn "no /dev/input/event* — add -device usb-kbd / -device usb-tablet (app starts but won't accept input)"
  fi
}

probe

tty -s || warn "stdin is not a terminal — the app needs a real VT for KD_GRAPHICS; use Ctrl-Alt-F2 in the QEMU window, not SSH/serial."

# Build via the companion script, which prints the binary path on stdout. Pass
# through any args (e.g. --release).
bin="$("$ROOT/scripts/build-showcase.sh" "$@")"

log "starting showcase — Tab/arrows to move, Space/Enter to activate, Esc to quit"
exec "$bin"

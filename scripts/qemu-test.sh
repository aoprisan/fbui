#!/usr/bin/env bash
#
# Guest-side helper for testing fbui-platform inside a Linux VM (e.g. QEMU on
# macOS — see fbui-platform/docs/qemu.md for the host-side launch).
#
# Run it *inside the Linux guest*, as root:
#
#   sudo ./scripts/qemu-test.sh smoke   # load vkms/uinput, run device tests
#   sudo ./scripts/qemu-test.sh demo    # run the software-cursor echo example
#   sudo ./scripts/qemu-test.sh probe   # just report what hardware is present
#
# `smoke` validates DRM modeset + page-flip plumbing and evdev normalization
# without needing a graphical window; `demo` needs a real VT (Ctrl-Alt-F2 in the
# QEMU display, not an SSH/pty session).
#
# NOTE: this script was written but could not be executed from the development
# container (no qemu/kvm/DRM there); you are the first to run it. It uses only
# standard tooling, but treat the first run as the validation step.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

log()  { printf '\033[1;34m[qemu-test]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[qemu-test]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[qemu-test]\033[0m %s\n' "$*" >&2; exit 1; }

need_root() {
  [ "$(id -u)" -eq 0 ] || die "run as root (sudo $0 $*)"
}

# Find the cargo binary even under sudo (which often resets PATH/HOME).
find_cargo() {
  if command -v cargo >/dev/null 2>&1; then command -v cargo; return; fi
  for c in "$HOME/.cargo/bin/cargo" "${SUDO_USER:+/home/$SUDO_USER/.cargo/bin/cargo}" /root/.cargo/bin/cargo; do
    [ -x "$c" ] && { echo "$c"; return; }
  done
  die "cargo not found; install Rust in the guest (https://rustup.rs)"
}

# Print the DRM driver behind a card node (e.g. "vkms", "virtio_gpu", "bochs-drm").
card_driver() {
  local node="$1" name
  name="$(basename "$node")"
  basename "$(readlink -f "/sys/class/drm/$name/device/driver" 2>/dev/null || true)" 2>/dev/null || true
}

# Choose the card to test: prefer VKMS (deterministic, what CI uses), else the
# first available card. Echoes the node path, or nothing if there is no DRM.
pick_card() {
  local first="" node drv
  for node in /dev/dri/card*; do
    [ -e "$node" ] || continue
    [ -z "$first" ] && first="$node"
    drv="$(card_driver "$node")"
    if [ "$drv" = "vkms" ]; then echo "$node"; return; fi
  done
  [ -n "$first" ] && echo "$first"
}

cmd_probe() {
  log "kernel: $(uname -srm)"
  if ls /dev/dri/card* >/dev/null 2>&1; then
    for node in /dev/dri/card*; do
      log "DRM: $node  driver=$(card_driver "$node")"
    done
  else
    warn "no /dev/dri — no DRM device (add -device virtio-gpu-pci or -vga std to QEMU)"
  fi
  [ -e /dev/uinput ] && log "uinput: present" || warn "no /dev/uinput (modprobe uinput)"
  [ -c /dev/fb0 ] && log "fbdev: /dev/fb0 present" || warn "no /dev/fb0 (fbdev fallback unavailable)"
}

cmd_smoke() {
  need_root smoke
  local cargo; cargo="$(find_cargo)"

  log "loading virtual kernel drivers (vkms, uinput)"
  modprobe vkms 2>/dev/null || warn "could not load vkms (built-in already? not in this kernel?)"
  modprobe uinput 2>/dev/null || warn "could not load uinput"

  # Wait for the card node the driver creates.
  for _ in $(seq 1 20); do ls /dev/dri/card* >/dev/null 2>&1 && break; sleep 0.5; done

  cmd_probe

  local card; card="$(pick_card)"
  if [ -z "$card" ]; then
    warn "no DRM card available; the DRM present test will fail its open()."
    warn "boot QEMU with a GPU device, or ensure vkms loaded."
  else
    log "using DRM card: $card (driver=$(card_driver "$card"))"
  fi

  log "building + running the #[ignore]d device integration tests"
  FBUI_DRM_CARD="${card:-/dev/dri/card0}" \
    "$cargo" test -p fbui-platform --test integration -- --ignored --nocapture
}

cmd_demo() {
  need_root demo
  local cargo; cargo="$(find_cargo)"
  cmd_probe
  if ! tty -s; then
    warn "stdin is not a terminal — the echo demo needs a real VT for KD_GRAPHICS."
    warn "in the QEMU graphical window press Ctrl-Alt-F2, log in, and run this there."
  fi
  log "starting the echo example (move the mouse, type, Esc to quit)"
  "$cargo" run -p fbui-platform --example echo
}

case "${1:-smoke}" in
  smoke) cmd_smoke ;;
  demo)  cmd_demo ;;
  probe) cmd_probe ;;
  -h|--help|help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//' ;;
  *) die "usage: $0 [smoke|demo|probe]" ;;
esac

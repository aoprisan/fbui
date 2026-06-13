#!/usr/bin/env bash
#
# Host-side launcher for the fbui visual echo demo under QEMU (macOS/arm64).
#
# It boots a Debian arm64 guest with HVF acceleration, shares this working tree
# in read-only over virtio-9p, and opens a graphical (cocoa) window. The echo
# binary is NOT built here — build it first on the host, in Docker:
#
#   docker run --rm --platform linux/arm64 -v "$PWD":/src -w /src \
#     -e CARGO_TARGET_DIR=/src/target/linux-arm64 rust:slim-bookworm \
#     cargo build --release -p fbui-platform --example echo
#
# Then run this script. On first boot cloud-init autologins root on tty1; in the
# QEMU window run `./run-demo.sh`, move the mouse / type, press Esc to quit.
#
#   scripts/qemu/launch-demo.sh           # boot (reuses cached image if present)
#   scripts/qemu/launch-demo.sh --fresh   # discard the guest disk and re-provision
#
# Serial console (incl. cloud-init progress) is logged to the cache dir; watch it
# with `tail -f "$FBUI_QEMU_CACHE/serial.log"`.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CACHE="${FBUI_QEMU_CACHE:-$HOME/.cache/fbui-qemu}"

# Use the *generic* image, not genericcloud: the genericcloud kernel is trimmed
# (no DRM/virtio-gpu, no 9p), which the graphical demo needs. Primary mirror first
# (the cloud.debian.org geo-redirect can resolve to a mirror that stalls on full
# GETs from some networks); cloud.debian.org is the fallback.
IMG_URLS=(
  "https://saimei.ftp.acc.umu.se/images/cloud/bookworm/latest/debian-12-generic-arm64.qcow2"
  "https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-arm64.qcow2"
)
BASE="$CACHE/debian-bookworm-generic-arm64.qcow2"
DISK="$CACHE/disk.qcow2"
SEED="$CACHE/seed.iso"
SERIAL="$CACHE/serial.log"
QEMU_SHARE="$(brew --prefix qemu)/share/qemu"
CODE_FW="$QEMU_SHARE/edk2-aarch64-code.fd"
VARS_TMPL="$QEMU_SHARE/edk2-arm-vars.fd"
VARS_FW="$CACHE/edk2-vars.fd"
BINREL="target/linux-arm64/release/examples/echo"

log()  { printf '\033[1;34m[launch-demo]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[launch-demo]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31m[launch-demo]\033[0m %s\n' "$*" >&2; exit 1; }

FRESH=0
[ "${1:-}" = "--fresh" ] && FRESH=1

command -v qemu-system-aarch64 >/dev/null || die "qemu-system-aarch64 not found (brew install qemu)"
[ -f "$CODE_FW" ] || die "EFI firmware not found: $CODE_FW"
[ -x "$ROOT/$BINREL" ] || die "echo binary missing: $ROOT/$BINREL — build it in Docker first (see header)."

mkdir -p "$CACHE"

# 1. base cloud image (downloaded once)
if [ ! -f "$BASE" ]; then
  log "downloading Debian bookworm arm64 cloud image (~326 MB)…"
  ok=0
  for url in "${IMG_URLS[@]}"; do
    log "  trying $url"
    if curl -fL --retry 3 --retry-delay 2 -C - -o "$BASE.part" "$url"; then ok=1; break; fi
    warn "  mirror failed, trying next"
  done
  [ "$ok" -eq 1 ] || die "could not download the cloud image from any mirror"
  mv "$BASE.part" "$BASE"
else
  log "using cached base image: $BASE"
fi

# 2. per-run disk (copy-on-first-use; --fresh re-provisions)
if [ ! -f "$DISK" ] || [ "$FRESH" -eq 1 ]; then
  log "creating fresh guest disk from base"
  cp "$BASE" "$DISK"
  qemu-img resize "$DISK" +4G >/dev/null
else
  log "reusing guest disk: $DISK (pass --fresh to reset)"
fi

# 3. writable EFI vars store (pflash needs a writable copy of the template)
[ -f "$VARS_FW" ] || cp "$VARS_TMPL" "$VARS_FW"

# 4. cloud-init NoCloud seed ISO (label must be CIDATA)
log "building cloud-init seed"
SEEDDIR="$(mktemp -d)"
trap 'rm -rf "$SEEDDIR"' EXIT
cp "$HERE/cloud-init/user-data" "$SEEDDIR/user-data"
cp "$HERE/cloud-init/meta-data" "$SEEDDIR/meta-data"
rm -f "$SEED"
hdiutil makehybrid -quiet -iso -joliet -default-volume-name CIDATA -o "$SEED" "$SEEDDIR"

: > "$SERIAL"
log "booting QEMU — a graphical window will open."
log "watch progress:  tail -f \"$SERIAL\""
log "in the window:    log in is automatic on tty1; run  ./run-demo.sh"

exec qemu-system-aarch64 \
  -machine virt -accel hvf -cpu host -smp 4 -m 2048 \
  -drive if=pflash,format=raw,readonly=on,file="$CODE_FW" \
  -drive if=pflash,format=raw,file="$VARS_FW" \
  -device virtio-gpu-pci \
  -device qemu-xhci -device usb-kbd -device usb-mouse \
  -netdev user,id=net0 -device virtio-net-pci,netdev=net0 \
  -fsdev local,id=fbui_fs,path="$ROOT",security_model=mapped-xattr,readonly=on \
  -device virtio-9p-pci,fsdev=fbui_fs,mount_tag=fbui \
  -drive if=virtio,format=qcow2,file="$DISK" \
  -drive if=virtio,format=raw,file="$SEED" \
  -display cocoa \
  -serial "file:$SERIAL"

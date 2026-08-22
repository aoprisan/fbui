#!/usr/bin/env bash
#
# Build a bootable Linux ISO that runs an fbui app as a kiosk — no distro, no
# init system, no display server: GRUB → kernel → a ~10 MB initramfs whose
# /init loads a handful of display/input modules and execs the app on the
# console. The default app is the `showcase` example, built statically against
# musl with the bundled Inter font, so the image needs no libc and no fonts.
#
#   ./scripts/iso/build-iso.sh                 # build showcase + ISO
#   ./scripts/iso/build-iso.sh --app BIN       # package a different static binary
#   ./scripts/iso/build-iso.sh --out PATH      # ISO output path
#
# Environment:
#   FBUI_ISO_CACHE   kernel package cache (default ~/.cache/fbui-iso)
#   FBUI_ISO_KERNEL  kernel version, e.g. 7.0.0-30-generic (default: newest
#                    linux-image-unsigned-*-generic in the apt archive)
#
# Host requirements (Debian/Ubuntu): the kernel comes from the host's apt
# archive, and the image is assembled with standard packaging tools:
#
#   apt install grub-pc-bin grub-efi-amd64-bin xorriso mtools cpio zstd \
#               kmod busybox-static
#
# The result boots on BIOS and UEFI (grub-mkrescue hybrid ISO) — in QEMU
# (scripts/iso/test-iso.sh) or from a USB stick:  dd if=… of=/dev/sdX bs=4M
#
# Display strategy: the kernel has simpledrm built in, so the firmware
# framebuffer (UEFI GOP, or the VESA mode GRUB sets on BIOS) is a DRM device
# with zero modules — that's the works-everywhere baseline fbui drives. On top
# of that the initramfs carries virtio-gpu + bochs (VMs) and USB HID + PS/2
# mouse (real input devices); everything is insmod'ed best-effort.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

log() { printf '\033[1;34m[build-iso]\033[0m %s\n' "$*" >&2; }
warn() { printf '\033[1;33m[build-iso]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[build-iso]\033[0m %s\n' "$*" >&2; exit 1; }

CACHE="${FBUI_ISO_CACHE:-$HOME/.cache/fbui-iso}"
KVER="${FBUI_ISO_KERNEL:-}"
OUT="$ROOT/target/iso/fbui-showcase.iso"
APP=""

while [ $# -gt 0 ]; do
  case "$1" in
    --app) APP="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    -h|--help|help) sed -n '2,36p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

# ---------- tool checks ----------------------------------------------------

need() { command -v "$1" >/dev/null 2>&1 || die "missing tool: $1 — $2"; }
need grub-mkrescue "apt install grub-pc-bin grub-efi-amd64-bin"
need xorriso       "apt install xorriso"
need mformat       "apt install mtools (grub-mkrescue needs it for the EFI image)"
need cpio          "apt install cpio"
need zstd          "apt install zstd (kernel modules are .ko.zst)"
need depmod        "apt install kmod"
need dpkg-deb      "this script fetches the kernel from a Debian/Ubuntu archive"
need apt-get       "this script fetches the kernel from a Debian/Ubuntu archive"

BUSYBOX="$(command -v busybox || true)"
[ -n "$BUSYBOX" ] || die "missing tool: busybox — apt install busybox-static"
file -L "$BUSYBOX" | grep -q "statically linked" \
  || die "$BUSYBOX is dynamically linked — install busybox-static"

# ---------- 1. the app: a static binary ------------------------------------

if [ -z "$APP" ]; then
  log "building showcase (release, x86_64-unknown-linux-musl, bundled font)"
  rustup target add x86_64-unknown-linux-musl >&2 2>/dev/null || true
  cargo build -p fbui --example showcase --release \
    --target x86_64-unknown-linux-musl --features platform,bundled-font >&2
  APP="$ROOT/target/x86_64-unknown-linux-musl/release/examples/showcase"
fi
[ -x "$APP" ] || die "app binary not found: $APP"
# A dynamic binary would need its interpreter + libs in the initramfs; refuse.
file -L "$APP" | grep -Eq "statically linked|static-pie" \
  || die "$APP is not statically linked — build with --target x86_64-unknown-linux-musl"
log "app: $APP ($(du -h "$APP" | cut -f1))"

# ---------- 2. the kernel: image + modules from the apt archive ------------

if [ -z "$KVER" ]; then
  KVER="$(apt-cache search --names-only '^linux-image-unsigned-[0-9.]+-[0-9]+-generic$' \
    | awk '{print $1}' | sed 's/^linux-image-unsigned-//' | sort -V | tail -1)"
  [ -n "$KVER" ] || die "no linux-image-unsigned-*-generic in the apt archive (set FBUI_ISO_KERNEL)"
fi
log "kernel: $KVER"

mkdir -p "$CACHE"
EXTRACT="$CACHE/extract-$KVER"
if [ ! -f "$EXTRACT/boot/vmlinuz-$KVER" ]; then
  ( cd "$CACHE"
    for pkg in "linux-image-unsigned-$KVER" "linux-modules-$KVER"; do
      ls "${pkg}_"*.deb >/dev/null 2>&1 && continue
      log "downloading $pkg"
      apt-get download "$pkg" >&2
    done )
  mkdir -p "$EXTRACT"
  for deb in "$CACHE/linux-image-unsigned-${KVER}_"*.deb "$CACHE/linux-modules-${KVER}_"*.deb; do
    log "extracting $(basename "$deb")"
    dpkg-deb -x "$deb" "$EXTRACT"
  done
fi
MODDIR="$EXTRACT/lib/modules/$KVER"
[ -d "$MODDIR" ] || die "modules not found under $EXTRACT"
# The packages don't ship modules.dep (depmod runs at install time); generate it.
[ -f "$MODDIR/modules.dep" ] || { log "running depmod"; depmod -b "$EXTRACT" "$KVER"; }

# ---------- 3. the initramfs -----------------------------------------------

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/dev" "$STAGE/proc" "$STAGE/sys" "$STAGE/lib/modules"

cp "$BUSYBOX" "$STAGE/bin/busybox"
cp "$APP" "$STAGE/bin/app"

# Optional hardware drivers, insmod'ed best-effort at boot. simpledrm (built
# in) already covers any machine whose firmware hands over a framebuffer;
# these add VM-native display/input and USB/PS2 input on real hardware.
WANT_MODULES=(
  kernel/drivers/gpu/drm/virtio/virtio-gpu.ko.zst
  kernel/drivers/gpu/drm/tiny/bochs.ko.zst
  kernel/drivers/virtio/virtio_input.ko.zst
  kernel/drivers/hid/hid-generic.ko.zst
  kernel/drivers/hid/usbhid/usbhid.ko.zst
  kernel/drivers/input/mouse/psmouse.ko.zst
)

# modules.dep lines are `mod: dep…` with the full transitive dependency list;
# loading deps right-to-left, then the module, is a valid insmod order. Emit
# each module once, numbered so /init can just insmod them in glob order.
declare -A SEEN=()
N=0
stage_module() {
  local rel="$1"
  [ -n "${SEEN[$rel]:-}" ] && return 0
  SEEN[$rel]=1
  local src="$MODDIR/$rel"
  [ -f "$src" ] || { warn "module not in this kernel, skipping: $rel"; return 0; }
  local base; base="$(basename "$rel" .zst)"     # foo.ko.zst -> foo.ko
  N=$((N + 1))
  zstd -q -d -f "$src" -o "$STAGE/lib/modules/$(printf '%02d' "$N")-$base"
}
for want in "${WANT_MODULES[@]}"; do
  line="$(grep -F "$want:" "$MODDIR/modules.dep" || true)"
  if [ -z "$line" ]; then warn "module not in this kernel, skipping: $want"; continue; fi
  deps="${line#*:}"
  for ((i = $(echo $deps | wc -w); i >= 1; i--)); do
    stage_module "$(echo $deps | cut -d' ' -f$i)"
  done
  stage_module "$want"
done
log "staged $N kernel module(s)"

cat > "$STAGE/init" <<'INIT'
#!/bin/busybox sh
# PID 1 in the initramfs: mount the API filesystems, load the optional
# display/input modules, wait for a display device, run the app.
/bin/busybox --install -s /bin
mount -t devtmpfs devtmpfs /dev
mount -t proc proc /proc
mount -t sysfs sysfs /sys

for m in /lib/modules/*.ko; do
  [ -e "$m" ] || break
  insmod "$m" 2>/dev/null || true
done

# Give the display driver a moment to probe; fbui itself falls back
# DRM -> fbdev and retries connectors, so this is just boot-time politeness.
i=0
while [ ! -e /dev/dri/card0 ] && [ ! -e /dev/fb0 ] && [ $i -lt 50 ]; do
  sleep 0.1; i=$((i + 1))
done
[ -e /dev/dri/card0 ] || [ -e /dev/fb0 ] || echo "[init] no display device appeared" >&2

echo "[init] starting app" >&2
/bin/app
rc=$?
echo "[init] app exited with status $rc" >&2

# `fbui.shell` on the kernel command line drops to a debug shell instead of
# powering off — handy with a serial console or in a VM.
if grep -qw fbui.shell /proc/cmdline; then
  echo "[init] fbui.shell: dropping to a shell" >&2
  exec sh
fi
poweroff -f
INIT
chmod +x "$STAGE/init"

mkdir -p "$(dirname "$OUT")"
INITRAMFS="$STAGE.initramfs.img"
( cd "$STAGE" && find . | cpio -o -H newc --quiet | gzip -9 ) > "$INITRAMFS"
log "initramfs: $(du -h "$INITRAMFS" | cut -f1)"

# ---------- 4. the ISO -----------------------------------------------------

ISODIR="$STAGE.iso"
mkdir -p "$ISODIR/boot/grub"
cp "$EXTRACT/boot/vmlinuz-$KVER" "$ISODIR/boot/vmlinuz"
mv "$INITRAMFS" "$ISODIR/boot/initramfs.img"
trap 'rm -rf "$STAGE" "$ISODIR"' EXIT

# Console goes to serial only: nothing ever writes to the VT, so the deferred
# fbcon never takes over and the app owns the display even without a VT guard.
# GRUB sets a video mode before boot; on BIOS that VESA framebuffer (and on
# UEFI the GOP framebuffer) becomes the built-in simpledrm device.
cat > "$ISODIR/boot/grub/grub.cfg" <<'GRUBCFG'
insmod all_video
set default=0
set timeout=1
menuentry "fbui showcase" {
    set gfxpayload=1280x800x32,1024x768x32,auto
    linux /boot/vmlinuz console=ttyS0,115200 loglevel=3
    initrd /boot/initramfs.img
}
menuentry "fbui showcase (debug shell on exit, verbose)" {
    set gfxpayload=1280x800x32,1024x768x32,auto
    linux /boot/vmlinuz console=ttyS0,115200 fbui.shell
    initrd /boot/initramfs.img
}
GRUBCFG

log "assembling ISO (grub-mkrescue)"
grub-mkrescue -o "$OUT" "$ISODIR" -- -volid FBUI_SHOWCASE >&2 2>&1 \
  || die "grub-mkrescue failed"

log "ISO: $OUT ($(du -h "$OUT" | cut -f1))"
log "test it:   ./scripts/iso/test-iso.sh"
log "USB stick: dd if=$OUT of=/dev/sdX bs=4M oflag=sync   (destroys /dev/sdX)"
echo "$OUT"

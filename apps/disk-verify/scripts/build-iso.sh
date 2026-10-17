#!/usr/bin/env bash
# Build a bootable x86_64 ISO (BIOS + UEFI, USB-stick or CD) that boots
# straight into disk-verify: a stock distribution kernel, a busybox initramfs,
# and the app as a static musl binary. No X11, no Wayland, no root filesystem.
#
#   apps/disk-verify/scripts/build-iso.sh [out.iso]
#
# Needs: rustup target x86_64-unknown-linux-musl, grub-mkrescue (grub-pc-bin
# and grub-efi-amd64-bin for both firmware types), xorriso, mtools, cpio,
# busybox-static, depmod, zstd, and a kernel with its modules. On Ubuntu:
#
#   apt install grub-pc-bin grub-efi-amd64-bin grub-common xorriso mtools \
#       cpio busybox-static kmod zstd linux-image-generic
#
# KERNEL=/boot/vmlinuz-x MODULES=/lib/modules/x override the kernel. It needs
# DEVTMPFS and a DRM framebuffer for the firmware's mode (SIMPLEDRM, or
# SYSFB_SIMPLEFB + an fbdev) built in — every mainstream distro kernel has both.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
out=$(realpath -m "${1:-$root/target/disk-verify.iso}")
target=x86_64-unknown-linux-musl

if [ -z "${KERNEL:-}" ]; then
    KERNEL=$(ls -1 /boot/vmlinuz-* 2>/dev/null | sort -V | tail -1 || true)
fi
[ -n "$KERNEL" ] && [ -r "$KERNEL" ] || { echo "no readable kernel (set KERNEL=)" >&2; exit 1; }
kver=${KERNEL##*/vmlinuz-}
MODULES=${MODULES:-/lib/modules/$kver}
[ -d "$MODULES" ] || { echo "no modules at $MODULES (set MODULES=)" >&2; exit 1; }
busybox=$(command -v busybox) || { echo "busybox-static not installed" >&2; exit 1; }
file -L "$busybox" 2>/dev/null | grep -q 'statically linked' ||
    echo "warning: $busybox may not be static" >&2

echo "== building disk-verify ($target)"
(cd "$root" && cargo build --release --target "$target" -p fbui-disk-verify)
bin="$root/target/$target/release/disk-verify"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
rootfs="$work/rootfs"
iso="$work/iso"

echo "== initramfs (kernel $kver)"
mkdir -p "$rootfs"/{bin,sbin,etc,proc,sys,dev,tmp,root} "$iso/boot/grub"
cp "$busybox" "$rootfs/bin/busybox"
for applet in $("$rootfs/bin/busybox" --list); do
    [ -e "$rootfs/bin/$applet" ] || ln -s busybox "$rootfs/bin/$applet"
done
install -m 755 "$bin" "$rootfs/bin/disk-verify"
install -m 755 "$here/iso/init" "$rootfs/init"
cp "$here/iso/modules" "$rootfs/etc/modules"

# Kernel modules: the storage/input drivers the kernel builds as modules,
# with everything they depend on (closure over modules.dep), decompressed so
# busybox's modprobe can load them.
moddir="$rootfs/lib/modules/$kver"
mkdir -p "$moddir"
cp "$MODULES"/modules.{order,builtin,builtin.modinfo} "$moddir/" 2>/dev/null || true
declare -A seen
add_module() { # $1: path relative to $MODULES, as modules.dep lists it
    local rel=$1
    [ -n "${seen[$rel]:-}" ] && return
    seen[$rel]=1
    local dst="$moddir/${rel%.zst}"
    dst=${dst%.xz}
    mkdir -p "$(dirname "$dst")"
    case $rel in
        *.zst) zstd -qdc "$MODULES/$rel" > "$dst" ;;
        *.xz) xz -dc "$MODULES/$rel" > "$dst" ;;
        *) cp "$MODULES/$rel" "$dst" ;;
    esac
    local deps
    deps=$(grep -m1 "^$rel:" "$MODULES/modules.dep" | cut -d: -f2)
    for d in $deps; do add_module "$d"; done
}
# Whole driver directories (for the modalias pass), plus the named modules.
for dir in drivers/ata drivers/nvme/host drivers/usb/storage drivers/hid/usbhid \
           drivers/input/mouse drivers/message/fusion drivers/scsi/vmw_pvscsi.ko \
           drivers/hid/hid-generic.ko; do
    grep -o "^kernel/$dir[^:]*" "$MODULES/modules.dep" | while read -r m; do echo "$m"; done
done > "$work/mods"
while read -r m; do add_module "$m"; done < "$work/mods"
while read -r name; do
    rel=$(grep -m1 -E "/${name//[-_]/[-_]}\.ko(\.zst|\.xz)?:" "$MODULES/modules.dep" | cut -d: -f1 || true)
    [ -n "$rel" ] && add_module "$rel"
done < "$here/iso/modules"
depmod -b "$rootfs" "$kver"

(cd "$rootfs" && find . | cpio -o -H newc --quiet | gzip -9) > "$iso/boot/initrd.img"
cp "$KERNEL" "$iso/boot/vmlinuz"
cp "$here/iso/grub.cfg" "$iso/boot/grub/grub.cfg"

echo "== iso"
mkdir -p "$(dirname "$out")"
grub-mkrescue -o "$out" "$iso" -- -volid DISKVERIFY 2>&1 | grep -v '^xorriso\|^Drive\|^Media\|^libisofs\|^Added\|^Writing\|^ISO image\|^$\|UPDATE' || true
[ -s "$out" ] || { echo "grub-mkrescue produced nothing" >&2; exit 1; }
echo "== wrote $out ($(du -h "$out" | cut -f1)): initramfs $(du -h "$iso/boot/initrd.img" | cut -f1), app $(du -h "$bin" | cut -f1)"

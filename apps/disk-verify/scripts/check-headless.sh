#!/usr/bin/env bash
# disk-verify with no display, tty or root: build a 64 MiB zeroed image with
# one corrupted sector, then drive the real app through flows/verify.txt (two
# scans, skips, input validation) and a lint pass. Exit 0 means it held.
#
#   apps/disk-verify/scripts/check-headless.sh
set -euo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
cd "$root"
cargo build -p fbui-disk-verify
app=target/debug/disk-verify

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
img="$tmp/disk.img"
truncate -s 64M "$img"
# Sector 5000, byte 10: "XYZ" — the one sector the flow expects to fail.
printf 'XYZ' | dd of="$img" bs=1 seek=$((5000 * 512 + 10)) conv=notrunc status=none

echo "== flow"
timeout 120 env FBUI_BACKEND=headless FBUI_REPLAY_SPEED=max \
    FBUI_REPLAY="$here/flows/verify.txt" "$app" "$img"

echo "== lint"
out=$(timeout 60 env FBUI_BACKEND=headless FBUI_LINT=1 FBUI_REPLAY=/dev/null \
    FBUI_REPLAY_EXIT=1 FBUI_REPLAY_TREE=/dev/null "$app" "$img" 2>&1)
echo "$out"
! echo "$out" | grep -q "fbui: lint:"

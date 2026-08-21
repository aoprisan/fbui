#!/usr/bin/env bash
#
# Boot the ISO from scripts/iso/build-iso.sh headless under QEMU and prove the
# app is actually drawing: wait for fbui's display line on the serial console,
# then take a QMP screendump and convert it to PNG.
#
#   ./scripts/iso/test-iso.sh                  # boot, screenshot, power off
#   ./scripts/iso/test-iso.sh --gpu virtio     # virtio-gpu instead of VGA/simpledrm
#   ./scripts/iso/test-iso.sh --uefi           # boot via OVMF instead of BIOS
#   ./scripts/iso/test-iso.sh --iso PATH       # non-default ISO path
#   ./scripts/iso/test-iso.sh --keep           # leave the VM running (QMP socket stays up)
#
# Artifacts land in target/iso/test/: serial.log, screen.ppm, screen.png.
# Uses KVM when /dev/kvm is available, otherwise falls back to TCG (slower;
# the boot-marker timeout below accounts for that).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

log() { printf '\033[1;34m[test-iso]\033[0m %s\n' "$*" >&2; }
die() { printf '\033[1;31m[test-iso]\033[0m %s\n' "$*" >&2; exit 1; }

ISO="$ROOT/target/iso/fbui-showcase.iso"
GPU=std
KEEP=0
UEFI=0
while [ $# -gt 0 ]; do
  case "$1" in
    --iso) ISO="$2"; shift 2 ;;
    --gpu) GPU="$2"; shift 2 ;;
    --uefi) UEFI=1; shift ;;
    --keep) KEEP=1; shift ;;
    -h|--help|help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

command -v qemu-system-x86_64 >/dev/null || die "qemu-system-x86_64 not found (apt install qemu-system-x86)"
command -v python3 >/dev/null || die "python3 not found (used for QMP + PPM->PNG)"
[ -f "$ISO" ] || die "ISO not found: $ISO — run ./scripts/iso/build-iso.sh first"

RUN="$ROOT/target/iso/test"
mkdir -p "$RUN"
SERIAL="$RUN/serial.log"
QMP="$RUN/qmp.sock"
rm -f "$SERIAL" "$QMP" "$RUN/screen.ppm" "$RUN/screen.png" "$RUN/qemu.log"

ACCEL=tcg
[ -w /dev/kvm ] && ACCEL=kvm
case "$GPU" in
  std)    GPU_ARGS=(-vga std) ;;
  virtio) GPU_ARGS=(-vga none -device virtio-gpu-pci) ;;
  *) die "unknown --gpu: $GPU (std | virtio)" ;;
esac

FW_ARGS=()
if [ "$UEFI" -eq 1 ]; then
  OVMF_CODE=/usr/share/OVMF/OVMF_CODE_4M.fd
  [ -f "$OVMF_CODE" ] || die "OVMF not found: $OVMF_CODE (apt install ovmf)"
  cp /usr/share/OVMF/OVMF_VARS_4M.fd "$RUN/ovmf-vars.fd"
  FW_ARGS=(-drive "if=pflash,format=raw,readonly=on,file=$OVMF_CODE"
           -drive "if=pflash,format=raw,file=$RUN/ovmf-vars.fd")
fi

log "booting $ISO (accel=$ACCEL, gpu=$GPU, firmware=$([ "$UEFI" -eq 1 ] && echo uefi || echo bios))"
qemu-system-x86_64 \
  -machine q35 -accel "$ACCEL" -smp 2 -m 1024 \
  "${GPU_ARGS[@]}" "${FW_ARGS[@]}" \
  -device virtio-keyboard-pci -device virtio-tablet-pci \
  -cdrom "$ISO" -boot d \
  -display none \
  -serial "file:$SERIAL" \
  -qmp "unix:$QMP,server=on,wait=off" \
  </dev/null >"$RUN/qemu.log" 2>&1 &
QEMU_PID=$!
[ "$KEEP" -eq 1 ] || trap 'kill "$QEMU_PID" 2>/dev/null || true' EXIT

# fbui prints "[platform] display WxH … via Drm/Fbdev …" once the display is
# up — that, not just a kernel banner, is the success marker.
MARKER='\[platform\] display'
DEADLINE=$(( $(date +%s) + 180 ))
log "waiting for the app to bring up the display (serial: $SERIAL)"
while ! grep -q "$MARKER" "$SERIAL" 2>/dev/null; do
  kill -0 "$QEMU_PID" 2>/dev/null || { tail -20 "$SERIAL" >&2 || true; die "QEMU exited before the display came up"; }
  [ "$(date +%s)" -lt "$DEADLINE" ] || { tail -20 "$SERIAL" >&2 || true; die "timed out waiting for '$MARKER'"; }
  sleep 1
done
grep "$MARKER" "$SERIAL" | head -1 >&2

# Let the first frames land, then grab the display over QMP.
sleep 3
log "taking screendump"
python3 - "$QMP" "$RUN/screen.ppm" "$KEEP" <<'PY'
import json, socket, sys, time
qmp_path, ppm, keep = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
s = socket.socket(socket.AF_UNIX)
s.connect(qmp_path)
f = s.makefile("rw")

def cmd(c, **args):
    f.write(json.dumps({"execute": c, **({"arguments": args} if args else {})}) + "\n")
    f.flush()
    while True:  # skip async events until the command's return/error shows up
        r = json.loads(f.readline())
        if "return" in r or "error" in r:
            return r

json.loads(f.readline())          # greeting
cmd("qmp_capabilities")
r = cmd("screendump", filename=ppm)
if "error" in r:
    sys.exit(f"screendump failed: {r['error']}")
time.sleep(1)                     # dump is written asynchronously
if not keep:
    cmd("quit")
PY

[ -s "$RUN/screen.ppm" ] || die "screendump produced no file"

# PPM (P6) -> PNG with just the stdlib, so the check needs no image packages.
python3 - "$RUN/screen.ppm" "$RUN/screen.png" <<'PY'
import struct, sys, zlib
raw = open(sys.argv[1], "rb").read()
tok, i = [], 0
while len(tok) < 4:               # magic, width, height, maxval
    j = raw.index(b"\n", i) if raw[i:i+1] == b"#" else i
    if raw[i:i+1] == b"#": i = j + 1; continue
    j = i
    while raw[j:j+1] not in b" \t\r\n": j += 1
    tok.append(raw[i:j]); i = j + 1
    while raw[i:i+1] in b" \t\r\n" and len(tok) < 4: i += 1
assert tok[0] == b"P6", "not a binary PPM"
w, h = int(tok[1]), int(tok[2])
pix = raw[i:i + w * h * 3]
rows = b"".join(b"\x00" + pix[y*w*3:(y+1)*w*3] for y in range(h))
def chunk(t, d): c = t + d; return struct.pack(">I", len(d)) + c + struct.pack(">I", zlib.crc32(c))
png = (b"\x89PNG\r\n\x1a\n"
       + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(rows, 6)) + chunk(b"IEND", b""))
open(sys.argv[2], "wb").write(png)
print(f"{sys.argv[2]}: {w}x{h}")
PY

log "OK — screenshot: $RUN/screen.png"
[ "$KEEP" -eq 1 ] && log "VM left running (pid $QEMU_PID, QMP: $QMP)"
exit 0

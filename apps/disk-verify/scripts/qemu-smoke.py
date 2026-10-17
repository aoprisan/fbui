#!/usr/bin/env python3
"""Boot the disk-verify ISO in QEMU against a known disk and drive a scan.

    apps/disk-verify/scripts/qemu-smoke.py target/disk-verify.iso OUTDIR [--uefi] [--keep]

Builds OUTDIR/test.img (256 MiB of 0xa5 with the 4 bytes at sector 300000,
offset 100 overwritten), attaches it through blkdebug so a read of sector
400000 fails with EIO, boots the ISO with no display, types into the UI over
the QEMU monitor (pattern a5, skip 200000+1000, Start), waits for the scan, and
writes screenshots: OUTDIR/boot.png, OUTDIR/done.png, OUTDIR/menu.png (after Esc).

Then it checks the surface map's pixels: it must hold both a wrong-bytes cell
and an unreadable cell. Exit 0 means the whole path — firmware, kernel,
initramfs drivers, DRM, evdev, the scan — worked. `--uefi` boots through OVMF
(`OVMF` env var, default /usr/share/ovmf/OVMF.fd) instead of SeaBIOS.
Stdlib only; no KVM needed.
"""

import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import zlib

SECTOR = 512
SIZE = 256 << 20
BAD_SECTOR, BAD_OFFSET, BAD_BYTES = 300000, 100, b"BAD!"
EIO_SECTOR = 400000
# The map's region in the 1024x768 layout, and the colours the app uses.
MAP = (412, 54, 1008, 400)
WRONG = (0xE3, 0xB3, 0x41)
# The dark theme's surface_alt: a pending cell. Seeing it means the app is up.
PENDING = (42, 47, 58)


def make_disk(path):
    chunk = b"\xa5" * (1 << 20)
    with open(path, "wb") as f:
        for _ in range(SIZE // len(chunk)):
            f.write(chunk)
        f.seek(BAD_SECTOR * SECTOR + BAD_OFFSET)
        f.write(BAD_BYTES)


class Monitor:
    def __init__(self, path):
        self.path = path

    def cmd(self, line):
        s = socket.socket(socket.AF_UNIX)
        s.connect(self.path)
        s.settimeout(1.0)
        time.sleep(0.1)
        try:
            s.recv(65536)
        except OSError:
            pass
        s.sendall((line + "\n").encode())
        time.sleep(0.25)
        s.close()

    def keys(self, *names):
        for n in names:
            self.cmd("sendkey " + n)

    def type(self, text):
        names = {"-": "minus", "+": "shift-equal", ",": "comma", " ": "spc"}
        self.keys(*[names.get(c, c) for c in text])

    def shot(self, png):
        ppm = png[:-4] + ".ppm"
        if os.path.exists(ppm):
            os.remove(ppm)
        self.cmd("screendump " + ppm)
        for _ in range(50):
            if os.path.exists(ppm) and os.path.getsize(ppm) > 0:
                break
            time.sleep(0.1)
        time.sleep(0.3)
        w, h, px = read_ppm(ppm)
        write_png(png, w, h, px)
        os.remove(ppm)
        return w, h, px


def read_ppm(path):
    data = open(path, "rb").read()
    fields, i = [], 0
    while len(fields) < 4:
        while data[i : i + 1].isspace():
            i += 1
        j = i
        while not data[j : j + 1].isspace():
            j += 1
        fields.append(data[i:j])
        i = j
    w, h = int(fields[1]), int(fields[2])
    return w, h, data[i + 1 : i + 1 + w * h * 3]


def write_png(path, w, h, rgb):
    raw = b"".join(b"\x00" + rgb[y * w * 3 : (y + 1) * w * 3] for y in range(h))

    def chunk(tag, body):
        c = struct.pack(">I", len(body)) + tag + body
        return c + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF)

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw, 6)))
        f.write(chunk(b"IEND", b""))


def map_colours(w, px):
    found = set()
    x0, y0, x1, y1 = MAP
    for y in range(y0, y1):
        row = y * w * 3
        for x in range(x0, x1):
            found.add(tuple(px[row + x * 3 : row + x * 3 + 3]))
    return found


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    iso, out = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    os.makedirs(out, exist_ok=True)
    img = os.path.join(out, "test.img")
    make_disk(img)
    tmp = tempfile.mkdtemp()
    conf = os.path.join(tmp, "blkdebug.conf")
    with open(conf, "w") as f:
        f.write(f'[inject-error]\nevent = "read_aio"\nerrno = "5"\nsector = "{EIO_SECTOR}"\n')
    mon = os.path.join(tmp, "mon.sock")
    firmware = []
    if "--uefi" in sys.argv:
        firmware = ["-bios", os.environ.get("OVMF", "/usr/share/ovmf/OVMF.fd")]
    qemu = subprocess.Popen(
        ["qemu-system-x86_64"] + firmware + [
            "-m", "1024", "-smp", "2",
            "-cdrom", iso, "-boot", "d",
            "-device", "ahci,id=ahci",
            "-drive", f"id=d0,file=blkdebug:{conf}:{img},if=none,format=raw",
            "-device", "ide-hd,drive=d0,bus=ahci.0",
            "-device", "qemu-xhci", "-device", "usb-tablet",
            "-vga", "std", "-display", "none",
            "-monitor", f"unix:{mon},server,nowait",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        m = Monitor(mon)
        for _ in range(100):
            if os.path.exists(mon):
                break
            time.sleep(0.1)
        # Wait for the UI: the map fills with pending cells.
        deadline = time.time() + 180
        while True:
            time.sleep(5)
            w, h, px = m.shot(os.path.join(out, "boot.png"))
            if w >= 1024 and PENDING in map_colours(w, px):
                break
            if time.time() > deadline:
                print("qemu-smoke: the UI never came up; see boot.png")
                return 1
        # Focus: disk list → pattern → skips → Start.
        m.keys("tab", "tab", "ctrl-a")
        m.type("a5")
        m.keys("tab")
        m.type("200000+1000")
        m.keys("tab", "spc")
        time.sleep(20)
        w, h, px = m.shot(os.path.join(out, "done.png"))
        colours = map_colours(w, px)
        wrong = WRONG in colours
        # The danger red: strongly red, weakly green/blue.
        unreadable = any(r > 180 and g < 110 and b < 110 for r, g, b in colours)
        m.keys("esc")
        time.sleep(3)
        m.shot(os.path.join(out, "menu.png"))
        print(f"qemu-smoke: wrong-bytes cell {'found' if wrong else 'MISSING'}, "
              f"unreadable cell {'found' if unreadable else 'MISSING'}; shots in {out}")
        return 0 if wrong and unreadable else 1
    finally:
        qemu.kill()
        qemu.wait()
        if "--keep" not in sys.argv:
            os.remove(img)


if __name__ == "__main__":
    sys.exit(main())

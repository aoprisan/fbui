#!/usr/bin/env python3
"""Boot the viewer in QEMU's raspi3b machine headless, type keys into its
UART, and screendump the framebuffer after each step — CI's (and your)
way to see the bare-metal build working without a Pi.

    scripts/qemu_drive.py ELF OUT_DIR [STEP ...]

A STEP is `name=keys`, keys being a comma list of: enter, esc, left, right,
up, down, pgup, pgdn, home, end, space, or literal characters. Each step
waits for the screen to settle, then writes OUT_DIR/<name>.ppm (and .png if
Pillow is around). The UART log goes to OUT_DIR/serial.log. Exit status is
non-zero if the guest panics or never prints its banner.
"""
import os, socket, subprocess, sys, tempfile, time

KEYS = {
    "enter": b"\r", "esc": b"\x1b", "left": b"\x1b[D", "right": b"\x1b[C",
    "up": b"\x1b[A", "down": b"\x1b[B", "pgup": b"\x1b[5~", "pgdn": b"\x1b[6~",
    "home": b"\x1b[H", "end": b"\x1b[F", "space": b" ",
}

def connect(path, timeout=10):
    end = time.time() + timeout
    while True:
        try:
            s = socket.socket(socket.AF_UNIX); s.connect(path); return s
        except OSError:
            if time.time() > end: raise
            time.sleep(0.05)

def main():
    elf, out = sys.argv[1], sys.argv[2]
    steps = [a.split("=", 1) for a in sys.argv[3:]] or [["boot", ""]]
    os.makedirs(out, exist_ok=True)
    tmp = tempfile.mkdtemp()
    ser_p, mon_p = os.path.join(tmp, "ser"), os.path.join(tmp, "mon")
    qemu = subprocess.Popen([
        "qemu-system-aarch64", "-M", "raspi3b", "-kernel", elf, "-display", "none",
        "-chardev", f"socket,id=ser,path={ser_p},server=on,wait=off", "-serial", "chardev:ser",
        "-monitor", f"unix:{mon_p},server,nowait",
    ])
    log = open(os.path.join(out, "serial.log"), "wb")
    ser, mon = connect(ser_p), connect(mon_p)
    ser.setblocking(False); mon.settimeout(2)
    text = b""

    def pump(seconds):
        nonlocal text
        end = time.time() + seconds
        while time.time() < end:
            try:
                d = ser.recv(4096)
                if d: log.write(d); log.flush(); text += d
            except BlockingIOError:
                time.sleep(0.05)
            if b"PANIC" in text:
                return

    def monitor(cmd):
        mon.sendall(cmd.encode() + b"\n")
        time.sleep(0.3)
        try: mon.recv(65536)
        except socket.timeout: pass

    try:
        monitor("")
        t0 = time.time()
        while b"documents;" not in text and time.time() - t0 < 60 and b"PANIC" not in text:
            pump(0.2)
        settle = float(os.environ.get("SETTLE", "4"))
        pump(settle)
        for name, keys in steps:
            for k in filter(None, keys.split(",")):
                ser.sendall(KEYS.get(k, k.encode()))
                pump(0.3)
            pump(settle)
            path = os.path.join(out, name + ".ppm")
            monitor(f"screendump {path}")
            try:
                from PIL import Image
                Image.open(path).save(path[:-4] + ".png")
            except Exception:
                pass
            print("shot", path, flush=True)
            if b"PANIC" in text:
                break
    finally:
        qemu.kill()
    sys.stdout.write(text.decode(errors="replace"))
    ok = b"documents;" in text and b"PANIC" not in text
    sys.exit(0 if ok else 1)

main()

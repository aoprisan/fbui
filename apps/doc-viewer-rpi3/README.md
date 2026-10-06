# doc-viewer-rpi3 — fbui with no operating system

The fbui [document viewer](../doc-viewer) booted straight on a Raspberry
Pi 3's Cortex-A53: no Linux, no firmware services beyond handing over a
framebuffer. Rust from the reset vector up — about 500 lines of board code
(`src/`) on top of `fbui-bare`.

| | |
|---|---|
| boot | `src/boot.rs`: park cores 1–3, EL3/EL2 → EL1, FPU on, stack, `.bss` |
| memory | `src/mmu.rs`: identity map, caches on (required — with the MMU off, unaligned accesses fault); `linked_list_allocator` heap |
| display | `src/mailbox.rs`: 1024×768×32 framebuffer from the VideoCore; `Framebuffer::flush` cleans the data cache over damaged rows |
| input | `src/uart.rs`: PL011 at 115200 8N1; a terminal's bytes and ANSI escapes become fbui keys |
| time / idle | `src/timer.rs`: generic timer for `now_ms`; `wfi` woken by the UART or a timer compare — an idle viewer uses **0% CPU** |

The documents are compiled into the image (`include_bytes!` of the fbui-doc
fixtures); swap in your own in `src/main.rs`.

## Run it in QEMU

```sh
rustup target add aarch64-unknown-none
rustup component add llvm-tools
scripts/build.sh            # → target/kernel8.img
scripts/run.sh              # this terminal is the Pi's UART
```

Headless, scripted (what CI and agents use): boots, types keys, and saves a
screendump after each step.

```sh
scripts/qemu_drive.py target/kernel8.img /tmp/shots \
    boot= open=enter next=right zoom=+,down,down library=esc
```

## Run it on a Pi 3

Copy `target/kernel8.img` to a FAT32 SD card that has the Raspberry Pi
firmware (`bootcode.bin`, `start.elf`, `fixup.dat`), with a `config.txt`:

```
arm_64bit=1
enable_uart=1
```

Connect a USB-serial adapter to GPIO 14/15 (115200 8N1) and drive the viewer
from a terminal. The QEMU model is what this was verified on; the real-board
path (cache cleaning, BGR pixel order, UART clock) is written to the
hardware's documentation but has not been run on a physical Pi yet — see
NOSTD.md.

## Keys

→ / PgDn / Space next page (Space scrolls first) · ← / PgUp previous ·
↑ ↓ scroll · Home / End · `+` `-` zoom · `w` fit width · `p` fit page ·
Esc library · Enter open.

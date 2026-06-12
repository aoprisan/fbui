# Testing fbui-platform under QEMU (incl. on macOS)

`fbui-platform` is Linux-only by construction — it speaks DRM/KMS, fbdev, evdev,
and the VT subsystem, all of which are Linux kernel interfaces. It cannot run
natively on macOS or Windows. The portable way to exercise it on those hosts (and
a useful CI/dev target on Linux too) is a **Linux guest under QEMU**, which is
exactly the `QEMU -vga std` row of the Phase 0/1 test matrix.

QEMU gives you two things the platform needs:

- a virtual GPU that exposes a real DRM device (`/dev/dri/card0`) — either
  `-vga std` (the **bochs-drm** driver) or `-device virtio-gpu-pci` (the
  **virtio_gpu** driver, preferred);
- virtual input (`-device usb-kbd`, `-device usb-tablet`) that shows up as
  `/dev/input/event*` for the evdev backend.

For a headless smoke test you don't even need the GPU device — load **VKMS**
(the kernel's virtual KMS driver) inside the guest and run the integration tests.

> **Heads-up:** these recipes were written but **not executed** from the
> development container that produced this branch (it has no qemu, no `/dev/kvm`,
> and no kernel-module support). They use only standard QEMU invocations; treat
> your first run as the validating one, and please file corrections.

---

## 0. Prerequisites

On the macOS host:

```sh
brew install qemu          # provides qemu-system-aarch64 / -x86_64 + firmware
```

You need a Linux guest with Rust installed. Easiest is a distro cloud image
(Debian/Ubuntu) plus `rustup`, or any guest where you can `git clone` this repo
and run `cargo`. Inside the guest you'll run as **root** (the platform opens DRM
and input nodes; `noseat` needs root or the `video`+`input` groups).

---

## 1. Apple Silicon Macs (fast, via the HVF hypervisor)

Use an **arm64** Linux guest. Example with a Debian arm64 qcow2 image:

```sh
qemu-system-aarch64 \
  -machine virt -accel hvf -cpu host -smp 4 -m 2048 \
  -bios "$(brew --prefix qemu)/share/qemu/edk2-aarch64-code.fd" \
  -device virtio-gpu-pci \
  -device qemu-xhci -device usb-kbd -device usb-tablet \
  -display cocoa \
  -drive if=virtio,file=debian-arm64.qcow2,format=qcow2 \
  -serial mon:stdio
```

- `-device virtio-gpu-pci` → `/dev/dri/card0` via the `virtio_gpu` driver.
- `-device usb-tablet` → **absolute** pointer events (exercises
  `PointerMotionAbsolute`). Use `usb-mouse` instead for relative motion.
- `-display cocoa` opens the guest screen in a macOS window. `-serial mon:stdio`
  gives you a serial console + the QEMU monitor in your terminal.

## 2. Intel Macs (or forcing x86_64)

Use an **x86_64** guest. `-vga std` gives you the bochs-drm path from the spike's
matrix; `-device virtio-gpu-pci` works too.

```sh
qemu-system-x86_64 \
  -accel hvf -cpu host -smp 4 -m 2048 \
  -vga std \
  -device qemu-xhci -device usb-kbd -device usb-tablet \
  -display cocoa \
  -drive if=virtio,file=ubuntu-amd64.qcow2,format=qcow2 \
  -serial mon:stdio
```

(On Linux hosts substitute `-accel kvm`. Without any accelerator QEMU falls back
to TCG emulation — correct but slow; fine for the smoke test.)

---

## 3. Run the tests in the guest

Clone the repo and check out the branch, then use the helper script
(`scripts/qemu-test.sh`):

```sh
git clone <repo-url> fbui && cd fbui
git checkout claude/phase-q-wbadze

# what hardware did QEMU expose?
sudo ./scripts/qemu-test.sh probe

# headless: load vkms + uinput, run the device integration tests
sudo ./scripts/qemu-test.sh smoke
```

`smoke` runs the `#[ignore]`d tests (`drm_vkms_present_cycle`,
`evdev_uinput_keystroke`) that validate DRM modeset + page-flip events and evdev
key normalization. It prefers a VKMS card when present and otherwise tests
whatever DRM card QEMU gave you.

### The visual demo

To see the software cursor + keystroke echo, run the example from a **real text
VT** (not the serial console or an SSH session — it needs the console for
`KD_GRAPHICS`):

1. In the QEMU graphical window, press **Ctrl-Alt-F2** and log in.
2. Run:
   ```sh
   sudo ./scripts/qemu-test.sh demo
   # or directly:
   sudo cargo run -p fbui-platform --example echo
   ```
3. Move the mouse — the arrow cursor follows; type — colored cells appear;
   press **Esc** to quit. On exit (and on Ctrl-C, or even a panic) the console is
   restored to text mode — that's the VT guard doing its job.

---

## 4. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `could not bring up the platform: open /dev/dri/card0 …` | No GPU device. Add `-device virtio-gpu-pci` (or `-vga std`); or run `smoke` which loads VKMS. |
| Falls back to fbdev / "no connected connector" | bochs/virtio expose a connector only with a GPU device attached; check `probe` output and QEMU `-vga`/`-device` flags. |
| `DRM master required` | Run as **root**, from the **active VT**, and make sure no display manager owns the console. |
| Cursor doesn't move | You used `usb-mouse` (relative) with a guest that needs a pointer warp, or no `-usb`/`-device qemu-xhci` controller. Prefer `usb-tablet`. |
| Demo over SSH does nothing/errors on `KD_GRAPHICS` | The demo needs a real VT; use the QEMU window (Ctrl-Alt-F2), not SSH/serial. Use `smoke` for headless validation instead. |
| Keyboard "stuck" after a hard kill | `kill -9` can't be trapped (Phase 0 NOTES); switch VT (Ctrl-Alt-F1/F2) or run `reset`. Normal exit/panic restores automatically. |

---

## 5. What this does and doesn't prove

- **Proves:** DRM dumb-buffer modeset, vsynced page-flip events, the back-buffer
  /stride/age contract, evdev key + pointer normalization, the VT guard's
  graphics-mode + restore behaviour, and the event loop end to end.
- **Doesn't prove:** real tear-free scanout timing (VKMS has no physical
  scanout; virtio/bochs in a window are close but not a panel), the
  libinput/libseat/xkbcommon backends (install `libinput-dev libseat-dev
  libxkbcommon-dev` in the guest and build with `--features libinput,libseat,xkbcommon`
  to exercise those), and anything HiDPI/touch-hardware specific.

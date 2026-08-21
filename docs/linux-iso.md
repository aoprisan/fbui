# Packaging an fbui app as a bootable Linux ISO

An assessment of shipping an fbui app as a **self-contained bootable ISO** — a
kiosk appliance image with no distro, no init system, no display server — plus
the working prototype that came out of it: `scripts/iso/build-iso.sh` builds a
**31 MB** hybrid BIOS+UEFI ISO that boots straight into the `showcase` example,
and `scripts/iso/test-iso.sh` proves it headlessly under QEMU.

**Verdict: this works, and it is small.** fbui's design assumptions line up
almost exactly with what a bare initramfs provides: the platform layer needs
only `/dev/dri` (or `/dev/fb0`) and `/dev/input`, the renderer is pure CPU with
a bundled font, and a statically linked musl binary needs no userspace at all
beyond a shell to exec it. Boot-to-UI is GRUB's 1 s menu timeout plus roughly a
kernel boot (~2 s under KVM-class hardware; ~40 s under emulated TCG).

## What the image contains

```
GRUB (hybrid BIOS + UEFI, grub-mkrescue)
 └─ vmlinuz            stock Ubuntu generic kernel, unmodified   (~15 MB)
     └─ initramfs.img  gzip cpio                                 (~3 MB)
         ├─ /init          busybox sh script (PID 1)
         ├─ /bin/busybox   static busybox (mount, insmod, poweroff)
         ├─ /bin/app       showcase, static musl + bundled Inter (3.9 MB)
         └─ /lib/modules/  8 modules: virtio-gpu, bochs, virtio_input,
                           hid, hid-generic, usbhid, psmouse (+deps)
```

`/init` mounts `devtmpfs`/`proc`/`sysfs`, best-effort `insmod`s the modules,
waits up to 5 s for a display node, and execs the app. On app exit it powers
off (Esc in the showcase = shutdown); `fbui.shell` on the kernel command line
drops to a busybox shell instead, and the second GRUB entry selects that plus
verbose boot.

## Design decisions

**Display: simpledrm is the zero-module baseline.** The Ubuntu generic kernel
has `CONFIG_DRM_SIMPLEDRM=y` + `CONFIG_SYSFB_SIMPLEFB=y` built in, so whatever
framebuffer the firmware hands over — UEFI GOP, or the VESA mode GRUB sets via
`gfxpayload` on BIOS — appears as a real DRM device with dumb buffers and
*zero modules loaded*. fbui's DRM backend drives it as-is. That covers any
modern UEFI machine even where no native DRM driver ships in the image;
unaccelerated scanout costs fbui nothing, since rendering is CPU-side anyway.
The bundled `virtio-gpu`/`bochs` modules give VMs a native KMS device on top.

**Console: serial-only, so nothing fights over the display.** The kernel
command line says `console=ttyS0` and *not* `console=tty0`; with Ubuntu's
`FRAMEBUFFER_CONSOLE_DEFERRED_TAKEOVER=y`, fbcon never takes over a display
nothing prints to. The VT guard finds no VT to take (`/dev/console` is the
serial port), logs one line, and continues disabled — exactly the graceful
path `fbui-platform` already had. There is no VT switching in an appliance,
so losing the guard costs nothing; the app still owns the display via DRM
master. App stderr lands on serial, which doubles as the test harness's
success signal.

**App: static musl, bundled font.** Built with
`--target x86_64-unknown-linux-musl --features platform,bundled-font`
(release, LTO, stripped): a 3.9 MB static-pie binary with Inter compiled in.
No libc, no `/usr/share/fonts`, no loader — the whole userspace argument for
a distro disappears. The build refuses a dynamically linked `--app` binary,
since it would silently fail to exec as PID 1's child.

**Kernel: stock and signed, from the host's apt archive.** `apt-get download
linux-image-unsigned-*-generic linux-modules-*` + `dpkg-deb -x` + `depmod -b`
— no kernel build, no custom config to maintain, security updates for free by
rebuilding. Module dependency order is resolved at *build* time from
`modules.dep` (each staged module decompressed from `.ko.zst` and numbered),
so `/init` needs only dumb `insmod` in glob order — no modprobe, no
depmod, and no module-decompression support needed in busybox.

### Alternatives considered

| Approach | Why not (here) |
|---|---|
| Distro live-ISO (debian-live, archiso) | Hundreds of MB and a full init system to then disable; fbui needs none of it. |
| Alpine netboot kernel + modloop | Attractive (smaller `-virt` kernel), but the CDN was unreachable from this dev environment (proxy 403) and it adds a second package ecosystem; the apt route uses tools already on the build host. |
| Buildroot / Yocto | The right answer for a shipping embedded *product* (pinned custom kernel config with virtio/simpledrm built in, BSPs, reproducibility, update story), but a heavyweight external toolchain — overkill for assessing feasibility and for demo images. The initramfs layout prototyped here transfers to it directly. |
| Unikernel-style (kernel + app, no busybox) | Works (`rdinit=/bin/app`) but loses module loading, the display-wait, and the debug shell for ~2 MB — poor trade. |

## Verified

All checks run by `scripts/iso/test-iso.sh` in QEMU (`q35`, TCG in the dev
container — no `/dev/kvm`; the script auto-uses KVM where present), with
`-display none`: the success signal is fbui's own
`[platform] display 1280x800 Xrgb8888 via DrmDumb (2 buffers)` line on serial,
followed by a QMP `screendump` converted to PNG showing the fully rendered
showcase UI.

- **BIOS boot, `-vga std`** — GRUB VESA mode → simpledrm/bochs → renders. ✅
- **BIOS boot, `-vga none -device virtio-gpu-pci`** — no firmware
  framebuffer at all, so this proves the staged `virtio-gpu.ko` path. ✅
- **UEFI boot (OVMF, `--uefi`)** — GRUB EFI → GOP → simpledrm → renders. ✅
- **Input end-to-end** — QMP-injected virtio-tablet motion moved the composited
  software cursor and two injected Tab presses moved the focus ring
  (`virtio_input` → evdev → gesture/focus machinery). ✅
- Artifacts land in `target/iso/test/` (`serial.log`, `screen.png`).

## Not verified / limitations

- **Real hardware.** The `dd`-to-USB path is stock grub-mkrescue hybrid ISO and
  should boot BIOS or UEFI machines, but no physical boot was run. On UEFI
  hardware the simpledrm baseline should light up any panel the firmware does;
  native DRM drivers (i915/amdgpu/…) are *not* in the image, so there is no
  mode-setting beyond the firmware mode and no display hotplug. Add the
  relevant modules to `WANT_MODULES` if a device needs them.
- **Secure Boot must be off.** grub-mkrescue's GRUB is unsigned (no shim
  chain), even though the kernel itself is Ubuntu's.
- **Build host is Debian/Ubuntu-shaped**: the kernel comes from the host's apt
  archive (`apt-get download`, `dpkg-deb`). Other hosts can pre-seed
  `FBUI_ISO_CACHE` with the two `.deb`s or swap in any kernel + modules pair
  via `FBUI_ISO_KERNEL`.
- **Appliance niceties are out of scope**: no persistence, no network, no
  watchdog/respawn (the app runs once; exit powers off), no A/B update story.
  For a product, graduate the same layout to Buildroot/Yocto.
- The showcase quits on **Esc** — a plugged-in keyboard can shut the kiosk
  down; a real appliance app would not bind that.

## Usage

```sh
./scripts/iso/build-iso.sh                    # build app + ISO -> target/iso/
./scripts/iso/build-iso.sh --app path/to/bin  # package another *static* binary
./scripts/iso/test-iso.sh                     # BIOS + VGA smoke test, screenshot
./scripts/iso/test-iso.sh --gpu virtio        # virtio-gpu module path
./scripts/iso/test-iso.sh --uefi              # OVMF firmware
./scripts/iso/test-iso.sh --keep              # leave the VM + QMP socket up
dd if=target/iso/fbui-showcase.iso of=/dev/sdX bs=4M oflag=sync   # USB stick
```

Build-host packages (Debian/Ubuntu):
`grub-pc-bin grub-efi-amd64-bin xorriso mtools cpio zstd kmod busybox-static`
(+ `qemu-system-x86 ovmf python3` for the test script), and the
`x86_64-unknown-linux-musl` Rust target.

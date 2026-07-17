//! [`TermDisplay`]: the [`Display`] impl that presents into a terminal.
//!
//! The render layer sees a perfectly ordinary single-buffered display: a
//! normal-RAM shadow buffer in `Xrgb8888` with a fixed stride, `age` = 1 after
//! the first present (the buffer keeps last frame's contents, so partial
//! redraw works exactly as on hardware). `present(damage)` is where pixels
//! become escape bytes:
//!
//! * **Kitty mode** — the frame lives in the terminal as one *base* image.
//!   Damage is transmitted as small *patch* images placed above the base at
//!   the damaged pixel offset, so bytes-on-the-wire scale with the damage,
//!   not the screen (the whole point over SSH). When patches pile up or the
//!   damage is most of the surface anyway, the base is retransmitted and the
//!   patches deleted — the base image id ping-pongs between two slots so the
//!   new frame is always placed before the old one is removed (no flicker).
//! * **Cells mode** — the damaged cells are re-emitted as truecolor
//!   half-blocks; untouched cells aren't written at all.
//!
//! Terminal resize is a *hotplug*: [`reconfigure`](Display::reconfigure)
//! re-reads the kernel winsize on the event loop's existing poll cadence and
//! reports a new [`DisplayInfo`], driving the same
//! `on_display_changed` path as an HDMI mode change.

use std::os::unix::io::{AsRawFd, BorrowedFd};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::display::{BackendKind, Display, DisplayInfo, Frame};
use crate::error::Result;
use crate::format::PixelFormat;
use crate::geom::{Rect, Size};

use super::encode;
use super::{Shared, TermProtocol, TtyGuard, WinSize};

/// Patch image ids start here; ids 1 and 2 are the base slots.
const FIRST_PATCH_ID: u32 = 16;
/// Consolidate (full retransmit + patch flush) when this many patches live.
const MAX_PATCHES: usize = 48;

pub struct TermDisplay {
    shared: Arc<Shared>,
    /// Owns the raw-mode + screen state; restores the terminal on drop.
    _guard: TtyGuard,
    protocol: TermProtocol,
    info: DisplayInfo,
    cols: u32,
    rows: u32,
    shadow: Vec<u8>,
    /// Our own normal-RAM allocation, so the stride is simply `w * 4` — the
    /// "never compute a stride" invariant is about *kernel-mapped* buffers
    /// with driver-chosen pitch; here we are the producer and report this
    /// stride through `Frame::stride` for everyone else to use.
    stride: usize,
    /// False until the first present (and again after a resize): the next
    /// present transmits/paints the full surface.
    presented_once: bool,
    /// Which base slot (image id 1 or 2) is currently on screen.
    base_slot: bool,
    next_patch_id: u32,
    live_patches: Vec<u32>,
    next_z: i32,
    /// Reused scratch buffers (escape bytes / extracted RGB), so a present
    /// allocates nothing in steady state.
    out: Vec<u8>,
    rgb: Vec<u8>,
}

impl TermDisplay {
    pub(crate) fn new(
        shared: Arc<Shared>,
        guard: TtyGuard,
        protocol: TermProtocol,
        ws: WinSize,
    ) -> Self {
        let (cell_w, cell_h) = (
            shared.cell_w.load(Ordering::Relaxed),
            shared.cell_h.load(Ordering::Relaxed),
        );
        let size = Self::surface_size(protocol, ws.cols, ws.rows, cell_w, cell_h);
        let stride = size.w as usize * 4;
        let info = DisplayInfo {
            size,
            format: PixelFormat::Xrgb8888,
            refresh_mhz: 0,
            buffers: 1,
            backend: BackendKind::Terminal,
        };
        TermDisplay {
            shared,
            _guard: guard,
            protocol,
            info,
            cols: ws.cols,
            rows: ws.rows,
            shadow: vec![0; stride * size.h as usize],
            stride,
            presented_once: false,
            base_slot: false,
            next_patch_id: FIRST_PATCH_ID,
            live_patches: Vec::new(),
            next_z: 1,
            out: Vec::new(),
            rgb: Vec::new(),
        }
    }

    /// Cell geometry, read from the [`Shared`] single source of truth.
    fn cell_px(&self) -> (u32, u32) {
        (
            self.shared.cell_w.load(Ordering::Relaxed).max(1),
            self.shared.cell_h.load(Ordering::Relaxed).max(1),
        )
    }

    fn surface_size(
        protocol: TermProtocol,
        cols: u32,
        rows: u32,
        cell_w: u32,
        cell_h: u32,
    ) -> Size {
        match protocol {
            TermProtocol::Kitty => Size::new(cols * cell_w, rows * cell_h),
            TermProtocol::Cells => Size::new(cols, rows * 2),
        }
    }

    /// Which pixel protocol this display is speaking (for logs and tests).
    pub fn protocol(&self) -> TermProtocol {
        self.protocol
    }

    fn write_out(&mut self) -> Result<()> {
        let r = super::write_all(self.shared.fd.as_raw_fd(), &self.out)
            .map_err(|e| crate::error::Error::io("tty write", e));
        self.out.clear();
        r
    }

    fn present_kitty(&mut self, damage: &[Rect]) {
        let surface = Rect::from_size(self.info.size);
        let damage_area: u64 = damage.iter().map(|r| r.w as u64 * r.h as u64).sum();
        let full = !self.presented_once
            || damage_area * 2 >= self.info.size.area()
            || self.live_patches.len() + damage.len() > MAX_PATCHES;

        if full {
            let new_base = if self.base_slot { 1 } else { 2 };
            let old_base = if self.base_slot { 2 } else { 1 };
            self.base_slot = !self.base_slot;
            encode::rgb_of_rect_into(&mut self.rgb, &self.shadow, self.stride, surface);
            encode::csi_goto(&mut self.out, 1, 1);
            encode::kitty_transmit(
                &mut self.out,
                &self.rgb,
                surface.w,
                surface.h,
                encode::KittyPlacement::base(new_base),
            );
            // New frame is on screen; now drop the stale layers beneath/above.
            if self.presented_once {
                encode::kitty_delete(&mut self.out, old_base);
            }
            for id in self.live_patches.drain(..) {
                encode::kitty_delete(&mut self.out, id);
            }
            self.next_z = 1;
            self.next_patch_id = FIRST_PATCH_ID;
            self.presented_once = true;
            return;
        }

        let (cell_w, cell_h) = self.cell_px();
        for &rect in damage {
            encode::rgb_of_rect_into(&mut self.rgb, &self.shadow, self.stride, rect);
            let col = rect.x as u32 / cell_w;
            let row = rect.y as u32 / cell_h;
            let id = self.next_patch_id;
            self.next_patch_id += 1;
            let z = self.next_z;
            self.next_z += 1;
            encode::csi_goto(&mut self.out, row + 1, col + 1);
            encode::kitty_transmit(
                &mut self.out,
                &self.rgb,
                rect.w,
                rect.h,
                encode::KittyPlacement {
                    id,
                    z,
                    x_off: rect.x as u32 % cell_w,
                    y_off: rect.y as u32 % cell_h,
                },
            );
            self.live_patches.push(id);
        }
    }

    fn present_cells(&mut self, damage: &[Rect]) {
        let full = Rect::from_size(self.info.size);
        if !self.presented_once {
            encode::cells_emit(
                &mut self.out,
                &self.shadow,
                self.stride,
                self.info.size.h,
                full,
            );
            self.presented_once = true;
            return;
        }
        for &rect in damage {
            encode::cells_emit(
                &mut self.out,
                &self.shadow,
                self.stride,
                self.info.size.h,
                rect,
            );
        }
    }
}

impl Display for TermDisplay {
    fn info(&self) -> DisplayInfo {
        self.info
    }

    fn begin_frame(&mut self) -> Result<Option<Frame<'_>>> {
        Ok(Some(Frame {
            buffer: &mut self.shadow,
            stride: self.stride,
            size: self.info.size,
            format: self.info.format,
            // The shadow persists across presents, so after the first frame
            // the buffer always holds exactly the last-presented contents.
            age: if self.presented_once { 1 } else { 0 },
        }))
    }

    fn present(&mut self, damage: &[Rect]) -> Result<()> {
        let clamped: Vec<Rect> = damage
            .iter()
            .map(|r| r.clamp_to(self.info.size))
            .filter(|r| !r.is_empty())
            .collect();
        if clamped.is_empty() && self.presented_once {
            return Ok(());
        }
        match self.protocol {
            TermProtocol::Kitty => self.present_kitty(&clamped),
            TermProtocol::Cells => self.present_cells(&clamped),
        }
        self.write_out()
    }

    fn present_fd(&self) -> Option<BorrowedFd<'_>> {
        None // writes complete synchronously; the loop paces us like fbdev
    }

    fn dispatch_present(&mut self) -> Result<bool> {
        Ok(false)
    }

    fn reconfigure(&mut self) -> Result<Option<DisplayInfo>> {
        let ws = super::winsize(self.shared.fd.as_raw_fd())?;
        // A resize (or terminal font-size change) may change the cell pixel
        // size too; re-derive it when the kernel reports the text area in
        // pixels, and publish it for the mouse scaler.
        let (cell_w, cell_h) = match self.protocol {
            TermProtocol::Cells => (1, 2),
            TermProtocol::Kitty if ws.x_px > 0 && ws.y_px > 0 => {
                ((ws.x_px / ws.cols).max(1), (ws.y_px / ws.rows).max(1))
            }
            TermProtocol::Kitty => self.cell_px(),
        };
        if ws.cols == self.cols && ws.rows == self.rows && (cell_w, cell_h) == self.cell_px() {
            return Ok(None);
        }
        self.cols = ws.cols;
        self.rows = ws.rows;
        self.shared.cell_w.store(cell_w, Ordering::Relaxed);
        self.shared.cell_h.store(cell_h, Ordering::Relaxed);
        let size = Self::surface_size(self.protocol, ws.cols, ws.rows, cell_w, cell_h);
        self.stride = size.w as usize * 4;
        self.shadow = vec![0; self.stride * size.h as usize];
        self.info.size = size;
        self.presented_once = false; // next present repaints everything
        self.base_slot = false;
        self.next_patch_id = FIRST_PATCH_ID;
        self.live_patches.clear();
        self.next_z = 1;
        // Old placements are sized for the old grid; clear the slate now so
        // nothing stale lingers while the app rebuilds its layout.
        encode::kitty_delete_all(&mut self.out);
        self.out.extend_from_slice(b"\x1b[2J");
        self.write_out()?;
        Ok(Some(self.info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::TermSetup;
    use std::os::unix::io::{FromRawFd, OwnedFd};

    /// Open a pty pair; returns (master, slave).
    fn pty() -> (OwnedFd, OwnedFd) {
        let mut m: libc::c_int = -1;
        let mut s: libc::c_int = -1;
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        ws.ws_col = 20;
        ws.ws_row = 10;
        // SAFETY: openpty fills the two fds and applies the winsize.
        let r =
            unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), &ws) };
        assert_eq!(r, 0, "openpty failed");
        // SAFETY: fresh fds we own.
        unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) }
    }

    /// Continuously drains the pty master on a thread. A full kitty frame is
    /// bigger than the kernel pty buffer, so a synchronous write-then-read
    /// pattern would deadlock; the pump keeps the slave's writes flowing.
    struct Pump {
        buf: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    impl Pump {
        fn start(master: OwnedFd) -> Pump {
            let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let out = buf.clone();
            std::thread::spawn(move || {
                let fd = master.as_raw_fd();
                loop {
                    let mut pfd = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: poll then read on the pty master we own.
                    let p = unsafe { libc::poll(&mut pfd, 1, 5_000) };
                    if p <= 0 {
                        break; // 5 s of silence: the test is over
                    }
                    let mut chunk = [0u8; 4096];
                    let n = unsafe {
                        libc::read(fd, chunk.as_mut_ptr() as *mut libc::c_void, chunk.len())
                    };
                    if n <= 0 {
                        break; // EIO: slave side fully closed
                    }
                    out.lock().unwrap().extend_from_slice(&chunk[..n as usize]);
                }
            });
            Pump { buf }
        }

        /// Wait until the stream goes quiet, then take everything captured.
        fn take(&self) -> String {
            let mut last = usize::MAX;
            loop {
                let len = self.buf.lock().unwrap().len();
                if len == last {
                    break;
                }
                last = len;
                std::thread::sleep(std::time::Duration::from_millis(40));
            }
            let bytes = std::mem::take(&mut *self.buf.lock().unwrap());
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }

    fn test_setup(protocol: TermProtocol) -> TermSetup {
        TermSetup {
            protocol,
            query_pixel_size: false,
            global_restore: false,
            fallback_cell_px: (8, 16),
        }
    }

    #[test]
    fn cells_display_reports_two_pixels_per_cell_and_presents_damage() {
        let (master, slave) = pty();
        let pump = Pump::start(master);
        let (mut disp, _input) =
            crate::term::open_pair_on(slave, &test_setup(TermProtocol::Cells)).unwrap();
        assert_eq!(disp.info().size, Size::new(20, 20));
        assert_eq!(disp.info().backend, BackendKind::Terminal);
        let s = pump.take();
        assert!(s.contains("\x1b[?1049h"), "enters alt screen: {s:?}");
        assert!(s.contains("\x1b[?25l"), "hides cursor");
        assert!(s.contains("\x1b[?1006h"), "SGR mouse on");

        // First frame: paint everything red-ish; expect a full-cell emit.
        {
            let frame = disp.begin_frame().unwrap().unwrap();
            assert_eq!(frame.age, 0);
            for px in frame.buffer.chunks_exact_mut(4) {
                px.copy_from_slice(&[0, 0, 200, 0]); // b,g,r,x
            }
        }
        disp.present(&[Rect::new(0, 0, 20, 20)]).unwrap();
        let first = pump.take();
        assert!(first.contains("38;2;200;0;0"), "truecolor fg: {first:?}");
        assert_eq!(first.matches('▀').count(), 20 * 10);

        // Second frame: damage one pixel row -> exactly one cell row re-emitted.
        {
            let frame = disp.begin_frame().unwrap().unwrap();
            assert_eq!(frame.age, 1);
        }
        disp.present(&[Rect::new(3, 4, 5, 1)]).unwrap();
        let second = pump.take();
        assert_eq!(second.matches('▀').count(), 5, "{second:?}");
        assert!(
            second.contains("\x1b[3;4H"),
            "cursor to cell row 3, col 4: {second:?}"
        );
    }

    #[test]
    fn kitty_display_full_then_patch_then_consolidate() {
        let (master, slave) = pty();
        let pump = Pump::start(master);
        let (mut disp, _input) =
            crate::term::open_pair_on(slave, &test_setup(TermProtocol::Kitty)).unwrap();
        // 20 cols x 10 rows at the 8x16 fallback cell.
        assert_eq!(disp.info().size, Size::new(160, 160));
        pump.take();

        disp.begin_frame().unwrap();
        disp.present(&[Rect::from_size(disp.info().size)]).unwrap();
        let full = pump.take();
        assert!(
            full.contains("a=T,f=24,s=160,v=160,i=2,"),
            "base in slot 2: {full:?}"
        );
        assert!(
            !full.contains("a=d,d=I"),
            "nothing to delete on the first frame"
        );

        // Small damage -> a patch image at the right cell + pixel offset.
        disp.begin_frame().unwrap();
        disp.present(&[Rect::new(13, 21, 6, 3)]).unwrap();
        let patch = pump.take();
        // cell col = 13/8 = 1 -> CSI col 2; row = 21/16 = 1 -> CSI row 2.
        assert!(patch.contains("\x1b[2;2H"), "{patch:?}");
        assert!(patch.contains("s=6,v=3,i=16,"), "first patch id: {patch:?}");
        assert!(patch.contains("X=5,Y=5"), "pixel offset in cell: {patch:?}");

        // Big damage -> consolidation: new base in the other slot, old base
        // and the patch deleted.
        disp.begin_frame().unwrap();
        disp.present(&[Rect::new(0, 0, 160, 120)]).unwrap();
        let consolidated = pump.take();
        assert!(
            consolidated.contains("i=1,"),
            "base flipped to slot 1: {consolidated:?}"
        );
        assert!(consolidated.contains("a=d,d=I,i=2"), "old base deleted");
        assert!(consolidated.contains("a=d,d=I,i=16"), "patch deleted");
    }

    #[test]
    fn resize_reconfigures_and_clears() {
        let (master, slave) = pty();
        let slave_raw = slave.as_raw_fd();
        let pump = Pump::start(master);
        let (mut disp, _input) =
            crate::term::open_pair_on(slave, &test_setup(TermProtocol::Cells)).unwrap();
        pump.take();
        assert!(disp.reconfigure().unwrap().is_none(), "no change yet");

        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        ws.ws_col = 30;
        ws.ws_row = 12;
        // SAFETY: TIOCSWINSZ with a winsize struct on the pty.
        assert_eq!(unsafe { libc::ioctl(slave_raw, libc::TIOCSWINSZ, &ws) }, 0);

        let info = disp.reconfigure().unwrap().expect("resize detected");
        assert_eq!(info.size, Size::new(30, 24));
        let bytes = pump.take();
        assert!(
            bytes.contains("\x1b[2J"),
            "screen cleared on resize: {bytes:?}"
        );
    }

    // ---- a minimal kitty-graphics "terminal" for the equivalence test ----

    /// One transmitted image and where it was placed.
    struct PlacedImage {
        id: u32,
        col: u32,
        row: u32, // 0-based anchor cell
        x_off: u32,
        y_off: u32,
        z: i32,
        w: u32,
        h: u32,
        rgb: Vec<u8>,
        seq: usize, // placement order breaks z ties (later wins)
    }

    fn b64_decode(s: &str) -> Vec<u8> {
        const AL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let val = |c: u8| AL.iter().position(|&a| a == c).unwrap() as u32;
        let s: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
        let mut out = Vec::new();
        for chunk in s.chunks(4) {
            let mut n = 0u32;
            for (i, &c) in chunk.iter().enumerate() {
                n |= val(c) << (18 - 6 * i);
            }
            out.push((n >> 16) as u8);
            if chunk.len() > 2 {
                out.push((n >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(n as u8);
            }
        }
        out
    }

    /// Interpret a captured escape stream the way a kitty terminal would:
    /// track cursor moves, accumulate chunked `a=T` transmits as placed
    /// images (a retransmitted id replaces its predecessor), honor
    /// `a=d,d=I` / `d=A` deletes.
    fn kitty_interpret(stream: &str, images: &mut Vec<PlacedImage>, seq: &mut usize) {
        let (mut cur_col, mut cur_row) = (1u32, 1u32);
        // A chunked transmit in flight: (image skeleton, base64 so far).
        let mut pending: Option<(PlacedImage, String)> = None;
        let mut rest = stream;
        while let Some(esc) = rest.find('\x1b') {
            rest = &rest[esc + 1..];
            if let Some(after) = rest.strip_prefix('[') {
                // CSI: only cursor moves matter here.
                if let Some(end) = after.find(|c: char| c.is_ascii_alphabetic()) {
                    if after.as_bytes()[end] == b'H' {
                        let mut it = after[..end].split(';');
                        cur_row = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
                        cur_col = it.next().and_then(|v| v.parse().ok()).unwrap_or(1);
                    }
                    rest = &after[end + 1..];
                }
                continue;
            }
            let Some(apc) = rest.strip_prefix("_G") else {
                continue;
            };
            let st = apc.find("\x1b\\").expect("unterminated APC");
            let body = &apc[..st];
            rest = &apc[st + 2..];
            let (ctrl, payload) = body.split_once(';').unwrap_or((body, ""));
            let keys: std::collections::HashMap<&str, &str> = ctrl
                .split(',')
                .filter_map(|kv| kv.split_once('='))
                .collect();
            let more = keys.get("m") == Some(&"1");

            if pending.is_some() {
                // Continuation chunk: payload only.
                let (_, b64) = pending.as_mut().unwrap();
                b64.push_str(payload);
                if !more {
                    let (mut img, b64) = pending.take().unwrap();
                    img.rgb = b64_decode(&b64);
                    images.push(img);
                }
                continue;
            }
            match keys.get("a").copied() {
                Some("T") => {
                    let get = |k: &str| keys.get(k).and_then(|v| v.parse::<i64>().ok());
                    let id = get("i").expect("i=") as u32;
                    images.retain(|i| i.id != id); // retransmit replaces
                    *seq += 1;
                    let mut img = PlacedImage {
                        id,
                        col: cur_col - 1,
                        row: cur_row - 1,
                        x_off: get("X").unwrap_or(0) as u32,
                        y_off: get("Y").unwrap_or(0) as u32,
                        z: get("z").unwrap_or(0) as i32,
                        w: get("s").expect("s=") as u32,
                        h: get("v").expect("v=") as u32,
                        rgb: Vec::new(),
                        seq: *seq,
                    };
                    if more {
                        pending = Some((img, payload.to_string()));
                    } else {
                        img.rgb = b64_decode(payload);
                        images.push(img);
                    }
                }
                Some("d") => match keys.get("d").copied() {
                    Some("I") => {
                        let id: u32 = keys.get("i").unwrap().parse().unwrap();
                        images.retain(|i| i.id != id);
                    }
                    Some("A") => images.clear(),
                    _ => {}
                },
                _ => {}
            }
        }
        assert!(pending.is_none(), "stream ended mid-transmit");
    }

    /// Composite the placed images (z order, then placement order) into an
    /// RGB surface of `w`×`h` pixels at `cell_w`×`cell_h` px per cell.
    fn composite(images: &[PlacedImage], w: u32, h: u32, cell_w: u32, cell_h: u32) -> Vec<u8> {
        let mut order: Vec<&PlacedImage> = images.iter().collect();
        order.sort_by_key(|i| (i.z, i.seq));
        let mut out = vec![0u8; (w * h * 3) as usize];
        for img in order {
            let ox = img.col * cell_w + img.x_off;
            let oy = img.row * cell_h + img.y_off;
            for y in 0..img.h {
                for x in 0..img.w {
                    let (dx, dy) = (ox + x, oy + y);
                    if dx >= w || dy >= h {
                        continue;
                    }
                    let src = ((y * img.w + x) * 3) as usize;
                    let dst = ((dy * w + dx) * 3) as usize;
                    out[dst..dst + 3].copy_from_slice(&img.rgb[src..src + 3]);
                }
            }
        }
        out
    }

    /// The repo invariant: the fast path (damage as patch placements) must
    /// composite to *exactly* the pixels the slow path (full retransmit)
    /// produces from the same shadow.
    #[test]
    fn kitty_patches_composite_identically_to_a_full_retransmit() {
        let (master, slave) = pty();
        let pump = Pump::start(master);
        let (mut disp, _input) =
            crate::term::open_pair_on(slave, &test_setup(TermProtocol::Kitty)).unwrap();
        let size = disp.info().size; // 160x160 at the 8x16 fallback cell
        pump.take();

        let mut images: Vec<PlacedImage> = Vec::new();
        let mut seq = 0usize;

        // Frame 1: a gradient, full present.
        {
            let frame = disp.begin_frame().unwrap().unwrap();
            for y in 0..size.h {
                for x in 0..size.w {
                    let i = (y as usize * frame.stride) + x as usize * 4;
                    frame.buffer[i] = (x * 3) as u8; // B
                    frame.buffer[i + 1] = (y * 5) as u8; // G
                    frame.buffer[i + 2] = (x ^ y) as u8; // R
                }
            }
        }
        disp.present(&[Rect::from_size(size)]).unwrap();
        kitty_interpret(&pump.take(), &mut images, &mut seq);

        // Frame 2: two small edits at awkward (non-cell-aligned) offsets,
        // presented as patches.
        let damage = [Rect::new(13, 21, 11, 7), Rect::new(100, 3, 5, 40)];
        {
            let frame = disp.begin_frame().unwrap().unwrap();
            for r in &damage {
                for y in r.y..r.bottom() {
                    for x in r.x..r.right() {
                        let i = (y as usize * frame.stride) + x as usize * 4;
                        frame.buffer[i] = 250;
                        frame.buffer[i + 1] = (x * 7) as u8;
                        frame.buffer[i + 2] = (y * 11) as u8;
                    }
                }
            }
        }
        disp.present(&damage).unwrap();
        kitty_interpret(&pump.take(), &mut images, &mut seq);
        let fast = composite(&images, size.w, size.h, 8, 16);

        // Frame 3: force the slow path (full retransmit) of the SAME shadow.
        disp.begin_frame().unwrap();
        disp.present(&[Rect::from_size(size)]).unwrap();
        kitty_interpret(&pump.take(), &mut images, &mut seq);
        let slow = composite(&images, size.w, size.h, 8, 16);

        assert_eq!(
            fast, slow,
            "patch placements must composite byte-for-byte to the full frame"
        );
    }

    #[test]
    fn drop_restores_the_terminal() {
        let (master, slave) = pty();
        let pump = Pump::start(master);
        {
            let (_disp, _input) =
                crate::term::open_pair_on(slave, &test_setup(TermProtocol::Cells)).unwrap();
            pump.take();
        }
        let bytes = pump.take();
        assert!(
            bytes.contains("\x1b[?1049l"),
            "leaves alt screen: {bytes:?}"
        );
        assert!(bytes.contains("\x1b[?25h"), "shows cursor");
        assert!(bytes.contains("\x1b[?1003l"), "mouse reporting off");
        assert!(bytes.contains("a=d,d=A"), "kitty images deleted");
    }
}

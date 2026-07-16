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
    cell_w: u32,
    cell_h: u32,
    shadow: Vec<u8>,
    stride: usize,
    /// False until the first present (and again after a resize): the next
    /// present transmits/paints the full surface.
    presented_once: bool,
    /// Which base slot (image id 1 or 2) is currently on screen.
    base_slot: bool,
    next_patch_id: u32,
    live_patches: Vec<u32>,
    next_z: i32,
    /// Reused escape-byte scratch, so a present allocates nothing in steady
    /// state.
    out: Vec<u8>,
}

impl TermDisplay {
    pub(crate) fn new(
        shared: Arc<Shared>,
        guard: TtyGuard,
        protocol: TermProtocol,
        ws: WinSize,
        cell_px: (u32, u32),
    ) -> Self {
        let (cell_w, cell_h) = cell_px;
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
            cell_w,
            cell_h,
            shadow: vec![0; stride * size.h as usize],
            stride,
            presented_once: false,
            base_slot: false,
            next_patch_id: FIRST_PATCH_ID,
            live_patches: Vec::new(),
            next_z: 1,
            out: Vec::new(),
        }
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
        let fd = self.shared.fd.as_raw_fd();
        let mut buf = self.out.as_slice();
        while !buf.is_empty() {
            // SAFETY: write(2) on our blocking tty fd with an in-bounds buffer.
            let n = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
            if n > 0 {
                buf = &buf[n as usize..];
            } else {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(crate::error::Error::io("tty write", err));
            }
        }
        self.out.clear();
        Ok(())
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
            let rgb = encode::rgb_of_rect(&self.shadow, self.stride, surface);
            encode::csi_goto(&mut self.out, 1, 1);
            encode::kitty_transmit(
                &mut self.out,
                &rgb,
                surface.w,
                surface.h,
                encode::KittyPlacement {
                    id: new_base,
                    z: 0,
                    x_off: 0,
                    y_off: 0,
                },
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

        for &rect in damage {
            let rgb = encode::rgb_of_rect(&self.shadow, self.stride, rect);
            let col = rect.x as u32 / self.cell_w;
            let row = rect.y as u32 / self.cell_h;
            let id = self.next_patch_id;
            self.next_patch_id += 1;
            let z = self.next_z;
            self.next_z += 1;
            encode::csi_goto(&mut self.out, row + 1, col + 1);
            encode::kitty_transmit(
                &mut self.out,
                &rgb,
                rect.w,
                rect.h,
                encode::KittyPlacement {
                    id,
                    z,
                    x_off: rect.x as u32 % self.cell_w,
                    y_off: rect.y as u32 % self.cell_h,
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
        if ws.cols == self.cols && ws.rows == self.rows {
            return Ok(None);
        }
        self.cols = ws.cols;
        self.rows = ws.rows;
        let size = Self::surface_size(self.protocol, ws.cols, ws.rows, self.cell_w, self.cell_h);
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

//! End-to-end test of the terminal backend through its *public* API, against
//! a real pty pair — the terminal-emulator side is played by the test. Unlike
//! the DRM/uinput integration tests this needs no privileges and no kernel
//! modules, so it runs everywhere `cargo test` does.

#![cfg(feature = "term")]

use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use fbui_platform::display::Display as _;
use fbui_platform::input::InputSource as _;
use fbui_platform::term::{open_pair_on, TermProtocol, TermSetup};
use fbui_platform::{keysym, Button, InputEvent, KeyState, Point, Rect};

/// Open a pty pair with a known size; returns (master, slave).
fn pty(cols: u16, rows: u16) -> (OwnedFd, OwnedFd) {
    let mut m: libc::c_int = -1;
    let mut s: libc::c_int = -1;
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    ws.ws_col = cols;
    ws.ws_row = rows;
    // SAFETY: openpty fills the two fds and applies the winsize.
    let r = unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), &ws) };
    assert_eq!(r, 0, "openpty failed");
    // SAFETY: fresh fds we own.
    unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) }
}

/// The fake terminal emulator: drains everything the app writes (so big
/// presents can't fill the pty buffer and deadlock) and lets the test type.
struct FakeTerminal {
    master: Arc<OwnedFd>,
    captured: Arc<Mutex<Vec<u8>>>,
}

impl FakeTerminal {
    fn start(master: OwnedFd) -> Self {
        let master = Arc::new(master);
        let captured = Arc::new(Mutex::new(Vec::new()));
        let (fd_arc, out) = (master.clone(), captured.clone());
        std::thread::spawn(move || {
            let fd = fd_arc.as_raw_fd();
            loop {
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: poll+read on the master we hold via Arc.
                if unsafe { libc::poll(&mut pfd, 1, 5_000) } <= 0 {
                    break;
                }
                let mut buf = [0u8; 4096];
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                if n <= 0 {
                    break;
                }
                out.lock().unwrap().extend_from_slice(&buf[..n as usize]);
            }
        });
        FakeTerminal { master, captured }
    }

    /// Wait for output to go quiet, then return everything captured so far.
    fn take_output(&self) -> String {
        let mut last = usize::MAX;
        loop {
            let len = self.captured.lock().unwrap().len();
            if len == last {
                break;
            }
            last = len;
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        String::from_utf8_lossy(&std::mem::take(&mut *self.captured.lock().unwrap())).into_owned()
    }

    /// "Type" bytes at the application and give the pty a moment to relay.
    fn send(&self, bytes: &[u8]) {
        let fd = self.master.as_raw_fd();
        let mut rest = bytes;
        while !rest.is_empty() {
            // SAFETY: write(2) to the pty master.
            let n = unsafe { libc::write(fd, rest.as_ptr() as *const libc::c_void, rest.len()) };
            assert!(n > 0, "pty write failed");
            rest = &rest[n as usize..];
        }
        // The kernel relays master->slave through the line discipline; give it
        // a beat so the slave poll sees the bytes.
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
}

fn setup(protocol: TermProtocol) -> TermSetup {
    TermSetup {
        protocol,
        query_pixel_size: false,
        global_restore: false,
        fallback_cell_px: (8, 16),
    }
}

#[test]
fn full_cycle_render_type_click_teardown() {
    let (master, slave) = pty(40, 12);
    let term = FakeTerminal::start(master);

    {
        let (mut display, mut input) =
            open_pair_on(slave, &setup(TermProtocol::Cells)).expect("bring-up");

        // Cells mode: 40x12 cells = 40x24 pixels.
        let info = display.info();
        assert_eq!((info.size.w, info.size.h), (40, 24));

        // The app took the terminal over.
        let boot = term.take_output();
        assert!(boot.contains("\x1b[?1049h"), "alt screen: {boot:?}");
        assert!(boot.contains("\x1b[?1006h"), "SGR mouse: {boot:?}");

        // Render a frame and present it.
        {
            let frame = display.begin_frame().unwrap().expect("buffer");
            for px in frame.buffer.chunks_exact_mut(4) {
                px.copy_from_slice(&[255, 0, 0, 0]); // solid blue
            }
        }
        display.present(&[Rect::new(0, 0, 40, 24)]).unwrap();
        let painted = term.take_output();
        assert!(painted.contains("38;2;0;0;255"), "blue pixels: {painted:?}");

        // Type "hi", press Enter, arrow up, click at cell (10, 6).
        term.send(b"hi\r\x1b[A\x1b[<0;10;6M\x1b[<0;10;6m");
        let mut events = Vec::new();
        input.dispatch(&mut |e| events.push(e)).unwrap();

        let pressed: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                InputEvent::Key(k) if k.state == KeyState::Pressed => Some(k.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(pressed.len(), 4, "{events:?}");
        assert_eq!(pressed[0].utf8.as_deref(), Some("h"));
        assert_eq!(pressed[1].utf8.as_deref(), Some("i"));
        assert_eq!(pressed[2].keysym, keysym::RETURN);
        assert_eq!(pressed[3].keysym, keysym::UP);

        let click: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                InputEvent::PointerButton { button, state } => Some((*button, *state)),
                _ => None,
            })
            .collect();
        assert_eq!(
            click,
            vec![
                (Button::Left, KeyState::Pressed),
                (Button::Left, KeyState::Released)
            ]
        );
        // Cell (10,6) 1-based, 1x2-pixel cells -> pixel (9, 11).
        let motion = events.iter().find_map(|e| match e {
            InputEvent::PointerMotionAbsolute { position } => Some(*position),
            _ => None,
        });
        assert_eq!(motion, Some(Point::new(9, 11)));
    } // drop display + input

    let teardown = term.take_output();
    assert!(
        teardown.contains("\x1b[?1049l"),
        "back to main screen: {teardown:?}"
    );
    assert!(
        teardown.contains("\x1b[?25h"),
        "cursor shown again: {teardown:?}"
    );
}

#[test]
fn kitty_mode_full_frame_reaches_the_terminal_intact() {
    let (master, slave) = pty(30, 10);
    let term = FakeTerminal::start(master);
    let (mut display, _input) = open_pair_on(slave, &setup(TermProtocol::Kitty)).expect("bring-up");
    term.take_output();

    let info = display.info();
    assert_eq!((info.size.w, info.size.h), (240, 160)); // 30x10 cells at 8x16

    display.begin_frame().unwrap();
    display.present(&[Rect::new(0, 0, 240, 160)]).unwrap();
    let out = term.take_output();
    assert!(
        out.contains("\x1b_Ga=T,f=24,s=240,v=160,"),
        "kitty transmit: {out:?}"
    );
    assert!(out.contains("\x1b\\"), "APC terminated");
}

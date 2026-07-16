//! [`TermInput`]: terminal bytes in, normalized [`InputEvent`]s out.
//!
//! A terminal is just another input device: keys arrive as UTF-8 or escape
//! sequences, the mouse as SGR reports (mode 1006, upgraded to pixel
//! coordinates by mode 1016 where supported). The parser is a plain state
//! machine over a byte buffer — resumable across reads, so a sequence split
//! by a slow SSH link parses identically to one that arrived whole — and is
//! unit-tested byte-for-byte with no tty anywhere.
//!
//! What terminals *don't* give us (without opting into the kitty keyboard
//! protocol) is key-release events, so every key is synthesized as a press
//! immediately followed by a release — the shape the widget layer already
//! handles for auto-repeating evdev keys. Terminal responses (cursor
//! position, device attributes, size reports, APC replies) are recognized and
//! swallowed so they can never leak into a text field.

use std::os::unix::io::{AsRawFd, RawFd};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crate::error::Result;
use crate::geom::{Point, Size};
use crate::input::{
    keysym, AxisSource, Button, InputEvent, InputSource, KeyEvent, KeyState, Keysym, Modifiers,
};

use super::Shared;

/// X11 keysym offset for Unicode codepoints beyond Latin-1.
const UNICODE_KEYSYM: u32 = 0x0100_0000;

pub struct TermInput {
    shared: Arc<Shared>,
    parser: Parser,
}

impl TermInput {
    pub(crate) fn new(shared: Arc<Shared>, _surface: Size) -> Self {
        TermInput {
            shared,
            parser: Parser::default(),
        }
    }
}

impl InputSource for TermInput {
    fn fds(&self) -> Vec<RawFd> {
        vec![self.shared.fd.as_raw_fd()]
    }

    fn dispatch(&mut self, sink: &mut dyn FnMut(InputEvent)) -> Result<()> {
        // Bytes the display swallowed while querying the terminal come first.
        let pending = std::mem::take(&mut *self.shared.pending_input.lock().unwrap());
        if !pending.is_empty() {
            self.parser.feed(&pending, &self.scaler(), sink);
        }

        let fd = self.shared.fd.as_raw_fd();
        loop {
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: zero-timeout poll to test readability of our own fd.
            let ready = unsafe { libc::poll(&mut pfd, 1, 0) };
            if ready <= 0 || pfd.revents & libc::POLLIN == 0 {
                break;
            }
            let mut buf = [0u8; 1024];
            // SAFETY: read into a stack buffer; the fd is blocking but poll
            // just said readable, so this returns without blocking.
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                let err = std::io::Error::last_os_error();
                if n < 0 && err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                break; // EOF / EIO: the terminal went away; the loop will notice
            }
            self.parser.feed(&buf[..n as usize], &self.scaler(), sink);
        }
        self.parser.end_of_batch(sink);
        Ok(())
    }
}

impl TermInput {
    fn scaler(&self) -> MouseScale {
        MouseScale {
            pixels: self.shared.mouse_pixels.load(Ordering::Relaxed),
            cell_w: self.shared.cell_w.load(Ordering::Relaxed).max(1),
            cell_h: self.shared.cell_h.load(Ordering::Relaxed).max(1),
        }
    }
}

/// How SGR mouse coordinates map to surface pixels.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MouseScale {
    /// Mode 1016 active: reports are already 1-based pixels.
    pub pixels: bool,
    pub cell_w: u32,
    pub cell_h: u32,
}

impl MouseScale {
    fn to_pixels(self, x: u32, y: u32) -> Point {
        if self.pixels {
            Point::new(x.saturating_sub(1) as i32, y.saturating_sub(1) as i32)
        } else {
            // Center of the reported cell, so a click lands inside it.
            Point::new(
                (x.saturating_sub(1) * self.cell_w + self.cell_w / 2) as i32,
                (y.saturating_sub(1) * self.cell_h + self.cell_h / 2) as i32,
            )
        }
    }
}

/// Resumable escape-sequence parser. Bytes go in via [`feed`](Parser::feed);
/// complete tokens come out as events; an incomplete tail is kept for the
/// next read. [`end_of_batch`](Parser::end_of_batch) resolves the classic
/// lone-ESC ambiguity: a bare `0x1b` with nothing after it in the batch *is*
/// the Escape key.
#[derive(Default)]
pub(crate) struct Parser {
    buf: Vec<u8>,
    /// Last reported mouse position, to synthesize motion before a click and
    /// suppress duplicate no-move reports.
    last_mouse: Option<Point>,
}

/// One parse step: how many bytes were consumed (0 = need more data).
enum Step {
    Consumed(usize),
    NeedMore,
}

impl Parser {
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        scale: &MouseScale,
        sink: &mut dyn FnMut(InputEvent),
    ) {
        self.buf.extend_from_slice(bytes);
        loop {
            if self.buf.is_empty() {
                return;
            }
            match self.step(scale, sink) {
                Step::Consumed(n) => {
                    self.buf.drain(..n);
                }
                Step::NeedMore => return,
            }
        }
    }

    /// The read loop found no more bytes: a buffered lone ESC is the Escape
    /// key, not the start of a sequence.
    pub(crate) fn end_of_batch(&mut self, sink: &mut dyn FnMut(InputEvent)) {
        if self.buf == [0x1b] {
            self.buf.clear();
            key_press(sink, keysym::ESCAPE, None, Modifiers::empty());
        }
    }

    fn step(&mut self, scale: &MouseScale, sink: &mut dyn FnMut(InputEvent)) -> Step {
        let buf = &self.buf;
        if buf[0] != 0x1b {
            return self.plain_byte(sink);
        }
        if buf.len() < 2 {
            return Step::NeedMore;
        }
        match buf[1] {
            b'[' => self.csi(scale, sink),
            b'O' => {
                // SS3: exactly one final byte.
                if buf.len() < 3 {
                    return Step::NeedMore;
                }
                ss3_key(buf[2], sink);
                Step::Consumed(3)
            }
            // String sequences (OSC/DCS/APC/PM/SOS): swallow to BEL or ST.
            b']' | b'P' | b'_' | b'^' | b'X' => match string_sequence_end(buf) {
                Some(end) => Step::Consumed(end),
                None => Step::NeedMore,
            },
            0x1b => {
                // ESC ESC: the first one is a real Escape keypress.
                key_press(sink, keysym::ESCAPE, None, Modifiers::empty());
                Step::Consumed(1)
            }
            b => {
                // Alt+key: ESC prefixing a plain byte.
                if let Some((sym, utf8, mut mods)) = plain_key(b) {
                    mods |= Modifiers::ALT;
                    key_press(sink, sym, utf8.as_deref(), mods);
                }
                Step::Consumed(2)
            }
        }
    }

    /// A byte outside any escape: a control key or the start of UTF-8 text.
    fn plain_byte(&mut self, sink: &mut dyn FnMut(InputEvent)) -> Step {
        let b = self.buf[0];
        if b < 0x80 {
            if let Some((sym, utf8, mods)) = plain_key(b) {
                key_press(sink, sym, utf8.as_deref(), mods);
            }
            return Step::Consumed(1);
        }
        // UTF-8 multibyte: wait for the full scalar.
        let need = match b {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return Step::Consumed(1), // stray continuation byte
        };
        if self.buf.len() < need {
            return Step::NeedMore;
        }
        match std::str::from_utf8(&self.buf[..need]) {
            Ok(s) => {
                let c = s.chars().next().unwrap();
                key_press(sink, char_keysym(c), Some(s), Modifiers::empty());
                Step::Consumed(need)
            }
            Err(_) => Step::Consumed(1), // malformed: drop the lead byte
        }
    }

    /// CSI: `ESC [ (params) final`. Params are digits, `;`, and the SGR-mouse
    /// `<` marker; finals are `0x40..=0x7E`.
    fn csi(&mut self, scale: &MouseScale, sink: &mut dyn FnMut(InputEvent)) -> Step {
        let buf = &self.buf;
        let mut i = 2;
        let mouse = buf.get(2) == Some(&b'<');
        if mouse {
            i = 3;
        }
        let mut params: Vec<u32> = Vec::with_capacity(4);
        let mut cur: Option<u32> = None;
        while i < buf.len() {
            let b = buf[i];
            match b {
                b'0'..=b'9' => {
                    cur = Some(cur.unwrap_or(0).saturating_mul(10) + (b - b'0') as u32);
                    i += 1;
                }
                b';' | b':' => {
                    params.push(cur.take().unwrap_or(0));
                    i += 1;
                }
                0x40..=0x7E => {
                    if let Some(v) = cur.take() {
                        params.push(v);
                    }
                    let consumed = i + 1;
                    if mouse {
                        self.sgr_mouse(&params, b, scale, sink);
                    } else {
                        csi_key(&params, b, sink);
                    }
                    return Step::Consumed(consumed);
                }
                _ => {
                    // Intermediate/private bytes we don't understand: skip them.
                    i += 1;
                }
            }
        }
        Step::NeedMore
    }

    /// SGR mouse report: `CSI < b ; x ; y (M|m)`.
    fn sgr_mouse(
        &mut self,
        params: &[u32],
        final_byte: u8,
        scale: &MouseScale,
        sink: &mut dyn FnMut(InputEvent),
    ) {
        let (&b, &x, &y) = match params {
            [b, x, y, ..] => (b, x, y),
            _ => return,
        };
        let pos = scale.to_pixels(x, y);
        let moved = self.last_mouse != Some(pos);
        self.last_mouse = Some(pos);

        if b & 64 != 0 {
            // Wheel. Positive vertical = content scrolls up (wheel away).
            if moved {
                sink(InputEvent::PointerMotionAbsolute { position: pos });
            }
            let (h, v) = match b & 3 {
                0 => (0.0, 1.0),
                1 => (0.0, -1.0),
                2 => (1.0, 0.0),
                _ => (-1.0, 0.0),
            };
            sink(InputEvent::PointerAxis {
                horizontal: h,
                vertical: v,
                source: AxisSource::Wheel,
            });
            return;
        }
        if b & 32 != 0 {
            // Motion (with or without a held button).
            if moved {
                sink(InputEvent::PointerMotionAbsolute { position: pos });
            }
            return;
        }
        let button = match b & 3 {
            0 => Button::Left,
            1 => Button::Middle,
            2 => Button::Right,
            _ => return, // release marker in non-SGR encodings; not used here
        };
        // Make sure the press lands where the terminal says it is.
        if moved {
            sink(InputEvent::PointerMotionAbsolute { position: pos });
        }
        let state = if final_byte == b'M' {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        sink(InputEvent::PointerButton { button, state });
    }
}

/// Emit a key as press + release (terminals report no release of their own).
fn key_press(sink: &mut dyn FnMut(InputEvent), sym: Keysym, utf8: Option<&str>, mods: Modifiers) {
    sink(InputEvent::Key(KeyEvent {
        code: 0,
        keysym: sym,
        utf8: utf8.map(str::to_owned),
        state: KeyState::Pressed,
        modifiers: mods,
    }));
    sink(InputEvent::Key(KeyEvent {
        code: 0,
        keysym: sym,
        utf8: None, // matches the evdev keymap: text only on the press
        state: KeyState::Released,
        modifiers: mods,
    }));
}

/// A decoded key: keysym, the text it types (if any), and its modifiers.
type DecodedKey = (Keysym, Option<String>, Modifiers);

/// Decode a single non-escape byte: control keys, then printable ASCII.
fn plain_key(b: u8) -> Option<DecodedKey> {
    match b {
        0x0d | 0x0a => Some((keysym::RETURN, None, Modifiers::empty())),
        0x09 => Some((keysym::TAB, None, Modifiers::empty())),
        0x7f | 0x08 => Some((keysym::BACKSPACE, None, Modifiers::empty())),
        0x1b => Some((keysym::ESCAPE, None, Modifiers::empty())),
        // Ctrl+letter (Ctrl+A .. Ctrl+Z, minus the aliases above).
        0x01..=0x1a => Some((Keysym((b'a' + b - 1) as u32), None, Modifiers::CTRL)),
        0x00 | 0x1c..=0x1f => None, // rare Ctrl combos nothing above needs
        _ => {
            let c = b as char;
            let mods = if c.is_ascii_uppercase() {
                Modifiers::SHIFT
            } else {
                Modifiers::empty()
            };
            Some((Keysym(b as u32), Some(c.to_string()), mods))
        }
    }
}

/// Keysym for a decoded character, X11 convention.
fn char_keysym(c: char) -> Keysym {
    let cp = c as u32;
    if cp < 0x100 {
        Keysym(cp)
    } else {
        Keysym(UNICODE_KEYSYM + cp)
    }
}

/// `CSI 1;m X` style modifiers: the parameter is 1 + a bitfield.
fn csi_modifiers(param: Option<u32>) -> Modifiers {
    let m = param.unwrap_or(1).saturating_sub(1);
    let mut mods = Modifiers::empty();
    if m & 1 != 0 {
        mods |= Modifiers::SHIFT;
    }
    if m & 2 != 0 {
        mods |= Modifiers::ALT;
    }
    if m & 4 != 0 {
        mods |= Modifiers::CTRL;
    }
    if m & 8 != 0 {
        mods |= Modifiers::LOGO;
    }
    mods
}

/// Non-mouse CSI finals: navigation keys, the `~` family, and terminal
/// responses (swallowed).
fn csi_key(params: &[u32], final_byte: u8, sink: &mut dyn FnMut(InputEvent)) {
    let mods = csi_modifiers(params.get(1).copied());
    let sym = match final_byte {
        b'A' => Some(keysym::UP),
        b'B' => Some(keysym::DOWN),
        b'C' => Some(keysym::RIGHT),
        b'D' => Some(keysym::LEFT),
        b'H' => Some(keysym::HOME),
        b'F' => Some(keysym::END),
        b'Z' => {
            key_press(sink, keysym::TAB, None, Modifiers::SHIFT);
            return;
        }
        b'~' => {
            let mods = csi_modifiers(params.get(1).copied());
            let sym = match params.first().copied().unwrap_or(0) {
                1 | 7 => Some(keysym::HOME),
                2 => Some(keysym::INSERT),
                3 => Some(keysym::DELETE),
                4 | 8 => Some(keysym::END),
                5 => Some(keysym::PAGE_UP),
                6 => Some(keysym::PAGE_DOWN),
                11..=15 => Some(Keysym(keysym::F1.0 + (params[0] - 11))),
                17..=21 => Some(Keysym(keysym::F6.0 + (params[0] - 17))),
                23 => Some(keysym::F11),
                24 => Some(keysym::F12),
                _ => None,
            };
            if let Some(sym) = sym {
                key_press(sink, sym, None, mods);
            }
            return;
        }
        // Responses: size reports (t), cursor position (R), device
        // attributes (c), mode reports (y, $y arrives as unknown-final skip),
        // kitty keyboard (u — we never enable it, but swallow defensively).
        b't' | b'R' | b'c' | b'y' | b'u' | b'n' => None,
        _ => None,
    };
    if let Some(sym) = sym {
        key_press(sink, sym, None, mods);
    }
}

/// SS3 finals (application-mode cursor keys, F1–F4).
fn ss3_key(final_byte: u8, sink: &mut dyn FnMut(InputEvent)) {
    let sym = match final_byte {
        b'A' => Some(keysym::UP),
        b'B' => Some(keysym::DOWN),
        b'C' => Some(keysym::RIGHT),
        b'D' => Some(keysym::LEFT),
        b'H' => Some(keysym::HOME),
        b'F' => Some(keysym::END),
        b'P' => Some(keysym::F1),
        b'Q' => Some(keysym::F2),
        b'R' => Some(keysym::F3),
        b'S' => Some(keysym::F4),
        _ => None,
    };
    if let Some(sym) = sym {
        key_press(sink, sym, None, Modifiers::empty());
    }
}

/// Length of a complete string sequence (`ESC ] … BEL` / `ESC ] … ESC \` and
/// the DCS/APC/PM/SOS cousins), or `None` while unterminated.
fn string_sequence_end(buf: &[u8]) -> Option<usize> {
    let mut i = 2;
    while i < buf.len() {
        match buf[i] {
            0x07 => return Some(i + 1),
            0x1b if buf.get(i + 1) == Some(&b'\\') => return Some(i + 2),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cells_scale() -> MouseScale {
        MouseScale {
            pixels: false,
            cell_w: 1,
            cell_h: 2,
        }
    }

    fn pixel_scale() -> MouseScale {
        MouseScale {
            pixels: true,
            cell_w: 8,
            cell_h: 16,
        }
    }

    fn parse(scale: MouseScale, bytes: &[u8]) -> Vec<InputEvent> {
        let mut p = Parser::default();
        let mut out = Vec::new();
        p.feed(bytes, &scale, &mut |e| out.push(e));
        p.end_of_batch(&mut |e| out.push(e));
        out
    }

    fn pressed_keys(events: &[InputEvent]) -> Vec<(Keysym, Option<String>, Modifiers)> {
        events
            .iter()
            .filter_map(|e| match e {
                InputEvent::Key(k) if k.state == KeyState::Pressed => {
                    Some((k.keysym, k.utf8.clone(), k.modifiers))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn text_keys_press_and_release_with_utf8_on_press_only() {
        let evs = parse(cells_scale(), b"aB");
        assert_eq!(evs.len(), 4);
        let keys = pressed_keys(&evs);
        assert_eq!(
            keys[0],
            (Keysym('a' as u32), Some("a".into()), Modifiers::empty())
        );
        assert_eq!(
            keys[1],
            (Keysym('B' as u32), Some("B".into()), Modifiers::SHIFT)
        );
        match &evs[1] {
            InputEvent::Key(k) => {
                assert_eq!(k.state, KeyState::Released);
                assert!(k.utf8.is_none());
            }
            other => panic!("expected key release, got {other:?}"),
        }
    }

    #[test]
    fn utf8_multibyte_decodes_even_split_across_reads() {
        let scale = cells_scale();
        let mut p = Parser::default();
        let mut out = Vec::new();
        let bytes = "é€".as_bytes(); // 2-byte and 3-byte scalars
        p.feed(&bytes[..1], &scale, &mut |e| out.push(e));
        assert!(out.is_empty(), "half a scalar produces nothing");
        p.feed(&bytes[1..4], &scale, &mut |e| out.push(e));
        p.feed(&bytes[4..], &scale, &mut |e| out.push(e));
        let keys = pressed_keys(&out);
        assert_eq!(keys[0].1.as_deref(), Some("é"));
        assert_eq!(keys[0].0, Keysym(0xE9)); // Latin-1 keysym
        assert_eq!(keys[1].1.as_deref(), Some("€"));
        assert_eq!(keys[1].0, Keysym(UNICODE_KEYSYM + '€' as u32));
    }

    #[test]
    fn control_and_editing_keys() {
        let keys = pressed_keys(&parse(cells_scale(), b"\r\t\x7f\x03"));
        assert_eq!(keys[0].0, keysym::RETURN);
        assert!(
            keys[0].1.is_none(),
            "Enter carries no text, like the evdev keymap"
        );
        assert_eq!(keys[1].0, keysym::TAB);
        assert_eq!(keys[2].0, keysym::BACKSPACE);
        assert_eq!(keys[3], (Keysym('c' as u32), None, Modifiers::CTRL));
    }

    #[test]
    fn arrows_navigation_and_modifiers() {
        let keys = pressed_keys(&parse(
            cells_scale(),
            b"\x1b[A\x1b[1;5C\x1b[H\x1b[3~\x1b[5~\x1b[Z\x1bOP\x1b[24~",
        ));
        assert_eq!(keys[0], (keysym::UP, None, Modifiers::empty()));
        assert_eq!(keys[1], (keysym::RIGHT, None, Modifiers::CTRL));
        assert_eq!(keys[2].0, keysym::HOME);
        assert_eq!(keys[3].0, keysym::DELETE);
        assert_eq!(keys[4].0, keysym::PAGE_UP);
        assert_eq!(keys[5], (keysym::TAB, None, Modifiers::SHIFT));
        assert_eq!(keys[6].0, keysym::F1);
        assert_eq!(keys[7].0, keysym::F12);
    }

    #[test]
    fn lone_esc_is_escape_but_sequence_start_is_not() {
        let keys = pressed_keys(&parse(cells_scale(), b"\x1b"));
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].0, keysym::ESCAPE);

        // ESC that begins a split sequence must NOT fire Escape.
        let scale = cells_scale();
        let mut p = Parser::default();
        let mut out = Vec::new();
        p.feed(b"\x1b[", &scale, &mut |e| out.push(e));
        p.end_of_batch(&mut |e| out.push(e));
        assert!(out.is_empty(), "{out:?}");
        p.feed(b"B", &scale, &mut |e| out.push(e));
        assert_eq!(pressed_keys(&out)[0].0, keysym::DOWN);
    }

    #[test]
    fn alt_key_prefix() {
        let keys = pressed_keys(&parse(cells_scale(), b"\x1bx"));
        assert_eq!(
            keys[0],
            (Keysym('x' as u32), Some("x".into()), Modifiers::ALT)
        );
    }

    #[test]
    fn sgr_mouse_click_synthesizes_motion_then_button() {
        // Press left at cell (5, 3), release at the same spot.
        let evs = parse(cells_scale(), b"\x1b[<0;5;3M\x1b[<0;5;3m");
        match evs.as_slice() {
            [InputEvent::PointerMotionAbsolute { position }, InputEvent::PointerButton {
                button: Button::Left,
                state: KeyState::Pressed,
            }, InputEvent::PointerButton {
                button: Button::Left,
                state: KeyState::Released,
            }] => {
                // Cell (5,3) 1-based -> px (4, 2*2+1) with 1x2 cells.
                assert_eq!(*position, Point::new(4, 5));
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn sgr_mouse_pixel_mode_passes_coordinates_through() {
        let evs = parse(pixel_scale(), b"\x1b[<35;101;51M");
        match evs.as_slice() {
            [InputEvent::PointerMotionAbsolute { position }] => {
                assert_eq!(*position, Point::new(100, 50));
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn sgr_wheel_maps_direction() {
        let evs = parse(pixel_scale(), b"\x1b[<64;10;10M\x1b[<65;10;10M");
        let axes: Vec<f64> = evs
            .iter()
            .filter_map(|e| match e {
                InputEvent::PointerAxis {
                    vertical,
                    source: AxisSource::Wheel,
                    ..
                } => Some(*vertical),
                _ => None,
            })
            .collect();
        assert_eq!(axes, vec![1.0, -1.0]);
    }

    #[test]
    fn duplicate_motion_reports_are_suppressed() {
        let evs = parse(pixel_scale(), b"\x1b[<35;9;9M\x1b[<35;9;9M\x1b[<35;10;9M");
        let motions = evs
            .iter()
            .filter(|e| matches!(e, InputEvent::PointerMotionAbsolute { .. }))
            .count();
        assert_eq!(motions, 2);
    }

    #[test]
    fn terminal_responses_are_swallowed() {
        let evs = parse(
            cells_scale(),
            b"\x1b[4;600;800t\x1b[?62;c\x1b[12;40R\x1b]0;title\x07\x1b_Gi=1;OK\x1b\\\x1bP+r\x1b\\",
        );
        assert!(evs.is_empty(), "responses leaked as events: {evs:?}");
    }

    #[test]
    fn esc_esc_is_one_escape_then_sequence() {
        let keys = pressed_keys(&parse(cells_scale(), b"\x1b\x1b[A"));
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].0, keysym::ESCAPE);
        assert_eq!(keys[1].0, keysym::UP);
    }
}

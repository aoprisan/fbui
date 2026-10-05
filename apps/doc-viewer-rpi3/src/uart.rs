//! The PL011 UART: a log for us, a keyboard for the viewer. A serial
//! terminal sends keys as bytes and ANSI escape sequences; [`Keys`] turns
//! them into fbui keys.

use core::fmt;
use core::ptr::{read_volatile, write_volatile};

use fbui_widgets::event::Key;

const BASE: usize = 0x3F20_1000;
const DR: usize = BASE;
const FR: usize = BASE + 0x18;
const IBRD: usize = BASE + 0x24;
const FBRD: usize = BASE + 0x28;
const LCRH: usize = BASE + 0x2C;
const CR: usize = BASE + 0x30;
const IMSC: usize = BASE + 0x38;
const ICR: usize = BASE + 0x44;
const FR_RXFE: u32 = 1 << 4;
const FR_TXFF: u32 = 1 << 5;

const GPFSEL1: usize = 0x3F20_0004;
const GPPUD: usize = 0x3F20_0094;
const GPPUDCLK0: usize = 0x3F20_0098;

fn rd(a: usize) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}
fn wr(a: usize, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) }
}

/// 115200 8N1 on GPIO 14/15, receive interrupts on (they wake `wfi`).
pub fn init() {
    wr(CR, 0);
    // GPIO 14/15 → ALT0 (TXD0/RXD0), pulls off.
    let mut sel = rd(GPFSEL1);
    sel &= !((7 << 12) | (7 << 15));
    sel |= (4 << 12) | (4 << 15);
    wr(GPFSEL1, sel);
    wr(GPPUD, 0);
    for _ in 0..150 {
        core::hint::spin_loop();
    }
    wr(GPPUDCLK0, (1 << 14) | (1 << 15));
    for _ in 0..150 {
        core::hint::spin_loop();
    }
    wr(GPPUDCLK0, 0);
    wr(ICR, 0x7FF);
    // 48 MHz UART clock: 48e6 / (16 * 115200) = 26.0417.
    wr(IBRD, 26);
    wr(FBRD, 3);
    wr(LCRH, (3 << 5) | (1 << 4)); // 8 bits, FIFOs on
    wr(IMSC, (1 << 4) | (1 << 6)); // RX + RX timeout interrupts
    wr(CR, (1 << 0) | (1 << 8) | (1 << 9)); // UARTEN, TXE, RXE
}

pub fn put(b: u8) {
    while rd(FR) & FR_TXFF != 0 {
        core::hint::spin_loop();
    }
    wr(DR, b as u32);
}

pub fn get() -> Option<u8> {
    if rd(FR) & FR_RXFE != 0 {
        // Drained: clear the receive-timeout interrupt so `wfi` can sleep.
        wr(ICR, 1 << 6);
        return None;
    }
    Some(rd(DR) as u8)
}

pub fn has_data() -> bool {
    rd(FR) & FR_RXFE == 0
}

pub struct Writer;

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            if b == b'\n' {
                put(b'\r');
            }
            put(b);
        }
        Ok(())
    }
}

#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        let _ = writeln!($crate::uart::Writer, $($arg)*);
    }};
}

/// Terminal bytes → keys. A lone ESC is only known to be Escape (not the
/// start of a sequence) once a little time passes with nothing after it.
#[derive(Default)]
pub struct Keys {
    seq: [u8; 8],
    len: usize,
    esc_at: u64,
}

/// How long a lone ESC waits for the rest of a sequence.
pub const ESC_TIMEOUT_MS: u64 = 40;

impl Keys {
    pub fn pending_escape(&self) -> bool {
        self.len > 0
    }

    /// Feed one byte at time `now`; returns a key when one completes.
    pub fn feed(&mut self, b: u8, now: u64) -> Option<Key> {
        if self.len == 0 {
            return match b {
                0x1B => {
                    self.seq[0] = b;
                    self.len = 1;
                    self.esc_at = now;
                    None
                }
                b'\r' | b'\n' => Some(Key::Enter),
                b'\t' => Some(Key::Tab),
                0x7F | 0x08 => Some(Key::Backspace),
                b' ' => Some(Key::Space),
                0x21..=0x7E => Some(Key::Char(b as char)),
                _ => None,
            };
        }
        if self.len < self.seq.len() {
            self.seq[self.len] = b;
            self.len += 1;
        }
        let s = &self.seq[..self.len];
        let key = match s {
            [0x1B, b'[' | b'O'] => return None,
            [0x1B, b'[' | b'O', b'A'] => Some(Key::Up),
            [0x1B, b'[' | b'O', b'B'] => Some(Key::Down),
            [0x1B, b'[' | b'O', b'C'] => Some(Key::Right),
            [0x1B, b'[' | b'O', b'D'] => Some(Key::Left),
            [0x1B, b'[' | b'O', b'H'] => Some(Key::Home),
            [0x1B, b'[' | b'O', b'F'] => Some(Key::End),
            [0x1B, b'[', d @ b'0'..=b'9', b'~'] => match d {
                b'1' | b'7' => Some(Key::Home),
                b'4' | b'8' => Some(Key::End),
                b'3' => Some(Key::Delete),
                b'5' => Some(Key::PageUp),
                b'6' => Some(Key::PageDown),
                _ => None,
            },
            [0x1B, b'[', b'0'..=b'9'] => return None,
            // ESC + something else: Escape, then that key.
            // ESC + anything else: the user pressed Escape (Alt-combos are
            // not mapped); the second byte is dropped.
            [0x1B, _] => Some(Key::Escape),
            _ => None,
        };
        self.len = 0;
        key
    }

    /// A lone ESC that timed out.
    pub fn tick(&mut self, now: u64) -> Option<Key> {
        if self.len == 1 && now.saturating_sub(self.esc_at) >= ESC_TIMEOUT_MS {
            self.len = 0;
            return Some(Key::Escape);
        }
        None
    }
}

//! Input session **record & replay** (feature `platform`).
//!
//! Recording captures the normalized [`InputEvent`] stream — the exact
//! boundary between the platform and the app — with millisecond timestamps,
//! to a small line-oriented text file. Replaying feeds those events back
//! through the *same* runner paths as live input (gesture recognition,
//! kinetic scrolling, `App::update`, everything), optionally faster than real
//! time, optionally capturing a PNG of the end state and exiting.
//!
//! That turns "here's how to reproduce the bug" into an artifact:
//!
//! ```sh
//! FBUI_RECORD=flow.rec  ./kiosk-app        # record a live session
//! FBUI_REPLAY=flow.rec  ./kiosk-app        # watch it happen again
//! FBUI_BACKEND=term FBUI_REPLAY=flow.rec FBUI_REPLAY_SPEED=max \
//!     FBUI_REPLAY_SHOT=end.png ./kiosk-app # regression-check it in CI
//! ```
//!
//! The format is deliberately plain text (one event per line, `#` comments,
//! unknown lines skipped) so a test flow can be written by hand or edited in
//! a code review. See `docs/record-replay.md` for the grammar.
//!
//! Replay determinism has one honest caveat: animations (kinetic coasts,
//! tweens) advance by real frame `dt`, so mid-flight pixels can differ
//! between runs; *settled* end states — what [`FBUI_REPLAY_SHOT`] captures —
//! are stable because events are delivered at the same logical positions.
//! A perpetually animated UI is captured after a bounded settle window rather
//! than blocking an unattended replay forever.
//!
//! [`FBUI_REPLAY_SHOT`]: crate::run()

use std::fmt::Write as _;
use std::io::{BufWriter, Write as _};
use std::time::{Duration, Instant};

use fbui_platform::{AxisSource, Button, InputEvent, KeyEvent, KeyState, Keysym, Modifiers, Point};

/// File magic + format version on the first line.
const HEADER: &str = "fbui-rec 1";

// ---- serialization ---------------------------------------------------------

fn state_token(s: KeyState) -> &'static str {
    match s {
        KeyState::Pressed => "p",
        KeyState::Released => "r",
        KeyState::Repeated => "a",
    }
}

fn parse_state(t: &str) -> Option<KeyState> {
    match t {
        "p" => Some(KeyState::Pressed),
        "r" => Some(KeyState::Released),
        "a" => Some(KeyState::Repeated),
        _ => None,
    }
}

fn hex_of(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn bytes_of_hex(s: &str) -> Option<Vec<u8>> {
    // Byte-wise, not str-sliced: a hand-edited file with a non-ASCII byte in
    // the hex field must parse to None (skip the line), never panic on a
    // char boundary.
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    b.chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

/// One event as a line body (without the leading `@ms`), or `None` for events
/// that don't round-trip (none today; future variants degrade gracefully).
pub(crate) fn event_line(ev: &InputEvent) -> Option<String> {
    let mut s = String::new();
    match ev {
        InputEvent::Key(k) => {
            let _ = write!(
                s,
                "k 0x{:x} {} {}",
                k.keysym.0,
                state_token(k.state),
                k.modifiers.bits()
            );
            if let Some(text) = &k.utf8 {
                let _ = write!(s, " u{}", hex_of(text.as_bytes()));
            }
        }
        InputEvent::PointerMotion { dx, dy } => {
            let _ = write!(s, "d {dx} {dy}");
        }
        InputEvent::PointerMotionAbsolute { position } => {
            let _ = write!(s, "m {} {}", position.x, position.y);
        }
        InputEvent::PointerButton { button, state } => {
            let b = match button {
                Button::Left => "l".to_string(),
                Button::Middle => "m".to_string(),
                Button::Right => "r".to_string(),
                Button::Other(c) => c.to_string(),
            };
            let _ = write!(s, "b {b} {}", state_token(*state));
        }
        InputEvent::PointerAxis {
            horizontal,
            vertical,
            source,
        } => {
            let src = match source {
                AxisSource::Wheel => "w",
                AxisSource::Finger => "f",
                AxisSource::Continuous => "c",
            };
            let _ = write!(s, "s {horizontal} {vertical} {src}");
        }
        InputEvent::TouchDown { slot, position } => {
            let _ = write!(s, "td {slot} {} {}", position.x, position.y);
        }
        InputEvent::TouchMotion { slot, position } => {
            let _ = write!(s, "tm {slot} {} {}", position.x, position.y);
        }
        InputEvent::TouchUp { slot } => {
            let _ = write!(s, "tu {slot}");
        }
        InputEvent::TouchCancel => s.push_str("tc"),
        // `InputEvent` is non_exhaustive: skip anything this version doesn't
        // know rather than corrupting the file.
        _ => return None,
    }
    Some(s)
}

/// Parse one line body back into an event. `None` for malformed or unknown
/// lines (they are skipped with a warning, so old players read new files).
pub(crate) fn parse_event(body: &str) -> Option<InputEvent> {
    let mut it = body.split_ascii_whitespace();
    let tag = it.next()?;
    let ev = match tag {
        "k" => {
            let sym = it.next()?.strip_prefix("0x")?;
            let keysym = Keysym(u32::from_str_radix(sym, 16).ok()?);
            let state = parse_state(it.next()?)?;
            let mods = Modifiers::from_bits_truncate(it.next()?.parse().ok()?);
            let utf8 = match it.next() {
                Some(tok) => {
                    let hex = tok.strip_prefix('u')?;
                    Some(String::from_utf8(bytes_of_hex(hex)?).ok()?)
                }
                None => None,
            };
            InputEvent::Key(KeyEvent {
                code: 0,
                keysym,
                utf8,
                state,
                modifiers: mods,
            })
        }
        "d" => InputEvent::PointerMotion {
            dx: it.next()?.parse().ok()?,
            dy: it.next()?.parse().ok()?,
        },
        "m" => InputEvent::PointerMotionAbsolute {
            position: Point::new(it.next()?.parse().ok()?, it.next()?.parse().ok()?),
        },
        "b" => {
            let button = match it.next()? {
                "l" => Button::Left,
                "m" => Button::Middle,
                "r" => Button::Right,
                other => Button::Other(other.parse().ok()?),
            };
            InputEvent::PointerButton {
                button,
                state: parse_state(it.next()?)?,
            }
        }
        "s" => {
            let horizontal = it.next()?.parse().ok()?;
            let vertical = it.next()?.parse().ok()?;
            let source = match it.next()? {
                "w" => AxisSource::Wheel,
                "f" => AxisSource::Finger,
                "c" => AxisSource::Continuous,
                _ => return None,
            };
            InputEvent::PointerAxis {
                horizontal,
                vertical,
                source,
            }
        }
        "td" | "tm" => {
            let slot = it.next()?.parse().ok()?;
            let position = Point::new(it.next()?.parse().ok()?, it.next()?.parse().ok()?);
            if tag == "td" {
                InputEvent::TouchDown { slot, position }
            } else {
                InputEvent::TouchMotion { slot, position }
            }
        }
        "tu" => InputEvent::TouchUp {
            slot: it.next()?.parse().ok()?,
        },
        "tc" => InputEvent::TouchCancel,
        _ => return None,
    };
    Some(ev)
}

// ---- recorder ---------------------------------------------------------------

/// Appends each input event to the recording file as it happens. Flushed per
/// event: a recording of the session that crashed is exactly the artifact you
/// want, so it must survive the crash.
pub(crate) struct Recorder {
    out: BufWriter<std::fs::File>,
    start: Instant,
}

impl Recorder {
    pub(crate) fn create(path: &std::path::Path, size: (u32, u32)) -> std::io::Result<Self> {
        let mut out = BufWriter::new(std::fs::File::create(path)?);
        writeln!(out, "{HEADER} {}x{}", size.0, size.1)?;
        Ok(Recorder {
            out,
            start: Instant::now(),
        })
    }

    pub(crate) fn record(&mut self, ev: &InputEvent) {
        let Some(body) = event_line(ev) else { return };
        let ms = self.start.elapsed().as_millis();
        // Recording failures must never take down the app; the kiosk log sees it.
        if writeln!(self.out, "@{ms} {body}")
            .and_then(|_| self.out.flush())
            .is_err()
        {
            eprintln!("fbui: recording write failed; further events may be lost");
        }
    }
}

// ---- replayer ---------------------------------------------------------------

/// A loaded recording being played back on the wall clock (scaled by `speed`).
pub(crate) struct Replayer {
    events: std::collections::VecDeque<(u64, InputEvent)>,
    start: Instant,
    /// Wall-time multiplier (2.0 = twice as fast). `f64::INFINITY` = "max":
    /// everything is due immediately.
    speed: f64,
    /// Surface size the recording was made on, for a mismatch warning.
    pub(crate) recorded_size: Option<(u32, u32)>,
}

impl Replayer {
    pub(crate) fn load(path: &std::path::Path, speed: f64) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text, speed).map_err(std::io::Error::other)
    }

    pub(crate) fn parse(text: &str, speed: f64) -> Result<Self, String> {
        let mut lines = text.lines();
        let header = lines.next().unwrap_or_default();
        let mut parts = header.split_ascii_whitespace();
        if (parts.next(), parts.next()) != (Some("fbui-rec"), Some("1")) {
            return Err(format!("not an fbui-rec v1 file (header {header:?})"));
        }
        let recorded_size = parts.next().and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some((w.parse().ok()?, h.parse().ok()?))
        });

        let mut events = std::collections::VecDeque::new();
        let mut last_ms = 0u64;
        for (n, line) in lines.enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((at, body)) = line.split_once(' ') else {
                continue;
            };
            let Some(ms) = at.strip_prefix('@').and_then(|v| v.parse::<u64>().ok()) else {
                eprintln!("fbui: replay: skipping line {} (bad timestamp)", n + 2);
                continue;
            };
            match parse_event(body) {
                Some(ev) => {
                    // Keep the stream monotonic even if the file was hand-edited.
                    last_ms = last_ms.max(ms);
                    events.push_back((last_ms, ev));
                }
                None => eprintln!("fbui: replay: skipping line {} (unknown event)", n + 2),
            }
        }
        Ok(Replayer {
            events,
            start: Instant::now(),
            speed,
            recorded_size,
        })
    }

    /// Milliseconds of recording time that have elapsed on the (scaled) clock.
    fn elapsed_rec_ms(&self) -> u64 {
        if self.speed.is_infinite() {
            return u64::MAX;
        }
        (self.start.elapsed().as_secs_f64() * 1000.0 * self.speed) as u64
    }

    /// Pop every event whose timestamp has been reached, with its recorded
    /// time (the runner drives its replay clock from it).
    pub(crate) fn due_events(&mut self) -> Vec<(u64, InputEvent)> {
        self.due_at(self.elapsed_rec_ms())
    }

    /// The scheduling core, on an explicit clock so tests need no sleeping.
    /// Events move out — nothing is cloned or revisited.
    pub(crate) fn due_at(&mut self, rec_ms: u64) -> Vec<(u64, InputEvent)> {
        let mut out = Vec::new();
        while let Some((ms, _)) = self.events.front() {
            if *ms > rec_ms {
                break;
            }
            out.push(self.events.pop_front().expect("front just checked"));
        }
        out
    }

    pub(crate) fn finished(&self) -> bool {
        self.events.is_empty()
    }

    /// Wall-clock time until the next event is due (zero if overdue).
    pub(crate) fn next_due_in(&self) -> Option<Duration> {
        let (ms, _) = self.events.front()?;
        if self.speed.is_infinite() {
            return Some(Duration::ZERO);
        }
        let due_wall = *ms as f64 / 1000.0 / self.speed;
        Some(Duration::from_secs_f64(
            (due_wall - self.start.elapsed().as_secs_f64()).max(0.0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(ev: InputEvent) {
        let line = event_line(&ev).expect("serializable");
        let back = parse_event(&line).expect("parseable");
        // InputEvent has no PartialEq; compare via the canonical line form.
        assert_eq!(event_line(&back).unwrap(), line, "{ev:?}");
    }

    #[test]
    fn every_event_kind_round_trips() {
        round_trip(InputEvent::Key(KeyEvent {
            code: 0,
            keysym: Keysym(0xFF0D),
            utf8: None,
            state: KeyState::Pressed,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
        }));
        round_trip(InputEvent::Key(KeyEvent {
            code: 0,
            keysym: Keysym('é' as u32),
            utf8: Some("é".into()),
            state: KeyState::Repeated,
            modifiers: Modifiers::empty(),
        }));
        round_trip(InputEvent::PointerMotion { dx: -3.5, dy: 0.25 });
        round_trip(InputEvent::PointerMotionAbsolute {
            position: Point::new(640, 360),
        });
        round_trip(InputEvent::PointerButton {
            button: Button::Left,
            state: KeyState::Pressed,
        });
        round_trip(InputEvent::PointerButton {
            button: Button::Other(0x118),
            state: KeyState::Released,
        });
        round_trip(InputEvent::PointerAxis {
            horizontal: 0.0,
            vertical: -1.0,
            source: AxisSource::Wheel,
        });
        round_trip(InputEvent::TouchDown {
            slot: 2,
            position: Point::new(10, 20),
        });
        round_trip(InputEvent::TouchMotion {
            slot: 2,
            position: Point::new(11, 21),
        });
        round_trip(InputEvent::TouchUp { slot: 2 });
        round_trip(InputEvent::TouchCancel);
    }

    #[test]
    fn utf8_with_spaces_and_multibyte_survives() {
        let line = event_line(&InputEvent::Key(KeyEvent {
            code: 0,
            keysym: Keysym(' ' as u32),
            utf8: Some(" ".into()),
            state: KeyState::Pressed,
            modifiers: Modifiers::empty(),
        }))
        .unwrap();
        match parse_event(&line) {
            Some(InputEvent::Key(k)) => assert_eq!(k.utf8.as_deref(), Some(" ")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn replayer_schedules_by_timestamp_and_finishes() {
        let rec = "fbui-rec 1 100x50\n\
                   # a comment\n\
                   @0 m 5 5\n\
                   @100 b l p\n\
                   @100 b l r\n\
                   garbage line\n\
                   @250 k 0xff0d p 0\n";
        let mut r = Replayer::parse(rec, 1.0).unwrap();
        assert_eq!(r.recorded_size, Some((100, 50)));
        assert_eq!(r.due_at(0).len(), 1, "the @0 motion");
        assert_eq!(r.due_at(99).len(), 0);
        assert_eq!(r.due_at(150).len(), 2, "both button events at @100");
        assert!(!r.finished());
        assert_eq!(r.due_at(10_000).len(), 1);
        assert!(r.finished());
    }

    #[test]
    fn non_monotonic_timestamps_are_clamped() {
        let rec = "fbui-rec 1\n@50 m 1 1\n@20 m 2 2\n@60 m 3 3\n";
        let mut r = Replayer::parse(rec, 1.0).unwrap();
        // The hand-edited @20 plays at @50 (never before an earlier event).
        assert_eq!(r.due_at(49).len(), 0);
        assert_eq!(r.due_at(50).len(), 2);
        assert_eq!(r.due_at(60).len(), 1);
    }

    #[test]
    fn bad_header_is_rejected() {
        assert!(Replayer::parse("not a recording\n@0 m 1 1\n", 1.0).is_err());
    }

    #[test]
    fn malformed_hex_token_is_skipped_not_a_panic() {
        // 4 bytes (passes the even-length check), but the '€' means byte
        // offsets fall inside a multibyte char — must be None, not a panic.
        assert!(parse_event("k 0x20 p 0 u€5").is_none());
        assert!(parse_event("k 0x20 p 0 uzz").is_none());
    }

    #[test]
    fn max_speed_makes_everything_due() {
        let rec = "fbui-rec 1\n@999999 m 1 1\n";
        let mut r = Replayer::parse(rec, f64::INFINITY).unwrap();
        assert_eq!(r.next_due_in(), Some(Duration::ZERO));
        assert_eq!(r.due_events().len(), 1);
        assert!(r.finished());
    }
}

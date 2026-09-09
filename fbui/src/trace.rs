//! `FBUI_TRACE`: one line per notable event, so an author can read *why*
//! something happened without a debugger.
//!
//! ```text
//! @0      start   headless 1024x600 scale=1
//! @120    input   tap 96,148 → Button #inc
//! @120    msg     Inc
//! @121    mutate  1 op, damage 1 rect / 56608 px²
//! @137    frame   paint=0.8ms rects=1
//! @1000   timer   Tick
//! @2500   expect  #count text "3"  ok
//! ```
//!
//! The point is the causal chain: input → message → mutation → damage →
//! frame. "The button did nothing" becomes a one-line diagnosis — no `msg`
//! line after the `input` line means the callback is missing; a `msg` with no
//! `mutate` means `update` matched the wrong arm.
//!
//! It is text so it can be grepped and diffed, buffered and flushed per
//! frame, and entirely absent unless `FBUI_TRACE` is set.

use std::io::Write;
use std::path::Path;

/// Where trace lines go.
enum Sink {
    Stderr,
    File(std::io::BufWriter<std::fs::File>),
}

/// The trace writer. Constructed from `FBUI_TRACE` (`-` for stderr).
pub(crate) struct Trace {
    sink: Sink,
}

impl Trace {
    /// Build from the environment, or `None` when `FBUI_TRACE` is unset. A
    /// requested-but-unopenable trace is a hard error, like the other
    /// `FBUI_*` toggles: silently running untraced is worse than stopping.
    pub(crate) fn from_env() -> std::io::Result<Option<Self>> {
        let Some(spec) = std::env::var_os("FBUI_TRACE") else {
            return Ok(None);
        };
        let spec = spec.to_string_lossy().to_string();
        let sink = if spec == "-" || spec.is_empty() {
            Sink::Stderr
        } else {
            Sink::File(std::io::BufWriter::new(std::fs::File::create(Path::new(
                &spec,
            ))?))
        };
        Ok(Some(Trace { sink }))
    }

    /// Write one line: `@<ms>\t<kind>\t<detail>`.
    pub(crate) fn line(&mut self, ms: u64, kind: &str, detail: &str) {
        let _ = match &mut self.sink {
            Sink::Stderr => writeln!(std::io::stderr(), "@{ms}\t{kind}\t{detail}"),
            Sink::File(f) => writeln!(f, "@{ms}\t{kind}\t{detail}"),
        };
    }

    /// Flush the buffer — called once per frame, so a crash loses at most one
    /// frame's worth of trace.
    pub(crate) fn flush(&mut self) {
        let _ = match &mut self.sink {
            Sink::Stderr => std::io::stderr().flush(),
            Sink::File(f) => f.flush(),
        };
    }
}

/// Describe an input event the way a trace line should read: the gesture and
/// where it landed, not the raw kernel codes.
pub(crate) fn input_detail(ev: &fbui_platform::InputEvent) -> Option<String> {
    use fbui_platform::{InputEvent, KeyState};
    Some(match ev {
        InputEvent::Key(k) => {
            let what = match &k.utf8 {
                Some(t) if !t.trim().is_empty() => format!("{t:?}"),
                _ => format!("keysym 0x{:x}", k.keysym.0),
            };
            let state = match k.state {
                KeyState::Pressed => "down",
                KeyState::Released => "up",
                KeyState::Repeated => "repeat",
            };
            format!("key {what} {state}")
        }
        InputEvent::PointerButton { state, .. } => {
            format!("button {}", if state.is_down() { "down" } else { "up" })
        }
        InputEvent::PointerAxis { vertical, .. } => format!("wheel {vertical}"),
        InputEvent::TouchDown { .. } => "touch down".into(),
        InputEvent::TouchUp { .. } => "touch up".into(),
        // Motion is the noisiest event by far and says nothing on its own;
        // the press that follows carries the position and the target.
        InputEvent::PointerMotion { .. }
        | InputEvent::PointerMotionAbsolute { .. }
        | InputEvent::TouchMotion { .. } => return None,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbui_platform::{Button, InputEvent, KeyEvent, KeyState, Keysym, Modifiers};

    #[test]
    fn input_details_read_like_a_person_would_describe_them() {
        let key = |utf8: Option<&str>, state| {
            InputEvent::Key(KeyEvent {
                code: 0,
                keysym: Keysym(0xff0d),
                utf8: utf8.map(str::to_string),
                state,
                modifiers: Modifiers::empty(),
            })
        };
        assert_eq!(
            input_detail(&key(Some("a"), KeyState::Pressed)).as_deref(),
            Some("key \"a\" down")
        );
        assert_eq!(
            input_detail(&key(None, KeyState::Released)).as_deref(),
            Some("key keysym 0xff0d up")
        );
        assert_eq!(
            input_detail(&InputEvent::PointerButton {
                button: Button::Left,
                state: KeyState::Pressed
            })
            .as_deref(),
            Some("button down")
        );
        // Motion is dropped: it is the noisiest event and says nothing alone.
        assert!(input_detail(&InputEvent::PointerMotion { dx: 1.0, dy: 1.0 }).is_none());
    }
}

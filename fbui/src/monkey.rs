//! Deterministic **monkey testing** (feature `platform`): seeded input chaos
//! with a built-in reproducer.
//!
//! `FBUI_MONKEY=<seed>` synthesizes a pseudo-random — but fully
//! seed-deterministic — input session (taps, drags, long-presses, cancelled
//! touches, mouse clicks, wheel scrolls, focus/navigation keys, text entry)
//! and plays it through the *replay* path: the same gesture recognition,
//! kinetic scrolling, and `App::update` a real user exercises. Before the
//! first event fires, the whole session is written out as an ordinary
//! recording (see [`crate::record`]), so the moment the monkey finds a panic
//! you already hold the artifact that reproduces it:
//!
//! ```sh
//! FBUI_BACKEND=term FBUI_MONKEY=42 FBUI_MONKEY_EVENTS=5000 ./kiosk-app
//! # → fbui: monkey: seed 42, 5000 events on 1280x800; script saved to
//! #   fbui-monkey-42.rec — reproduce with FBUI_REPLAY=fbui-monkey-42.rec
//! #   … app panics somewhere around event 3127 …
//! FBUI_BACKEND=term FBUI_REPLAY=fbui-monkey-42.rec ./kiosk-app  # same crash
//! ```
//!
//! Because the script is saved *first* and then replayed verbatim,
//! reproducibility does not depend on the generator staying stable across
//! fbui versions: the `.rec` file is the ground truth, and it can be trimmed
//! down by hand in an editor to a minimal reproducer (the format is plain
//! text, one event per line).
//!
//! The generator never emits Escape (the runner's quit key) and every gesture
//! it starts is finished — a session ends with no held key, button, or touch
//! contact — so a monkey run always ends because the budget ran out, never
//! because the monkey quit the app early. See `docs/monkey-testing.md`.

use std::fmt::Write as _;

use fbui_platform::{
    keysym, AxisSource, Button, InputEvent, KeyEvent, KeyState, Keysym, Modifiers, Point,
};

use crate::record::event_line;

/// The printable-text pool. Deliberately includes multibyte UTF-8 (Latin-1
/// supplement, CJK, an emoji) so text shaping and byte-vs-char handling get
/// stressed, not just ASCII.
const ALPHABET: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 .,-_!?éßñ€漢🦀";

/// Navigation/editing keysyms the monkey presses (Escape excluded: it must
/// never quit the run). These all map to semantic widget keys in the runner.
const NAV_KEYS: [Keysym; 12] = [
    keysym::TAB,
    keysym::RETURN,
    keysym::LEFT,
    keysym::RIGHT,
    keysym::UP,
    keysym::DOWN,
    keysym::HOME,
    keysym::END,
    keysym::PAGE_UP,
    keysym::PAGE_DOWN,
    keysym::BACKSPACE,
    keysym::DELETE,
];

/// SplitMix64: tiny, seedable, and good enough to scatter events — chosen so
/// the monkey needs no dependency and its streams are stable per seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..n`. The modulo bias is irrelevant at monkey scale.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    /// Uniform in `lo..=hi`.
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }
}

/// The session under construction: a monotonic millisecond clock plus the
/// event list. Every action helper leaves the input devices "at rest".
struct Gen {
    rng: Rng,
    w: u64,
    h: u64,
    ms: u64,
    events: Vec<(u64, InputEvent)>,
}

impl Gen {
    fn push(&mut self, ev: InputEvent) {
        self.events.push((self.ms, ev));
    }

    /// Advance the clock by a uniform `lo..=hi` milliseconds.
    fn wait(&mut self, lo: u64, hi: u64) {
        self.ms += self.rng.range(lo, hi);
    }

    fn point(&mut self) -> Point {
        Point::new(self.rng.below(self.w) as i32, self.rng.below(self.h) as i32)
    }

    /// A tap: touch down, brief hold, up. The bread and butter of kiosk input.
    fn tap(&mut self) {
        let p = self.point();
        self.push(InputEvent::TouchDown {
            slot: 0,
            position: p,
        });
        self.wait(40, 90);
        self.push(InputEvent::TouchUp { slot: 0 });
    }

    /// A drag/swipe between two random points with per-step jitter. Short
    /// step times make some of these fast enough to register as flings, so
    /// kinetic scrolling gets exercised too.
    fn drag(&mut self) {
        let a = self.point();
        let b = self.point();
        self.push(InputEvent::TouchDown {
            slot: 0,
            position: a,
        });
        let steps = self.rng.range(4, 10);
        for i in 1..=steps {
            self.wait(8, 24);
            let t = i as f64 / steps as f64;
            let jx = self.rng.range(0, 6) as i32 - 3;
            let jy = self.rng.range(0, 6) as i32 - 3;
            let x = (a.x as f64 + (b.x - a.x) as f64 * t) as i32 + jx;
            let y = (a.y as f64 + (b.y - a.y) as f64 * t) as i32 + jy;
            self.push(InputEvent::TouchMotion {
                slot: 0,
                position: Point::new(x.clamp(0, self.w as i32 - 1), y.clamp(0, self.h as i32 - 1)),
            });
        }
        self.wait(8, 40);
        self.push(InputEvent::TouchUp { slot: 0 });
    }

    /// A press held long enough for the gesture recognizer's long-press to
    /// fire between the down and the up (the replay clock advances to each
    /// event's timestamp, so this works even at `FBUI_REPLAY_SPEED=max`).
    fn long_press(&mut self) {
        let p = self.point();
        self.push(InputEvent::TouchDown {
            slot: 0,
            position: p,
        });
        self.wait(600, 900);
        self.push(InputEvent::TouchUp { slot: 0 });
    }

    /// A drag that the device abandons (palm rejection, hotplug): ends in
    /// `TouchCancel` instead of an up, exercising every widget's cancel path.
    fn cancelled_drag(&mut self) {
        let a = self.point();
        self.push(InputEvent::TouchDown {
            slot: 0,
            position: a,
        });
        for _ in 0..self.rng.range(2, 5) {
            self.wait(8, 24);
            let p = self.point();
            self.push(InputEvent::TouchMotion {
                slot: 0,
                position: p,
            });
        }
        self.wait(8, 40);
        self.push(InputEvent::TouchCancel);
    }

    /// A mouse click: absolute move, left down, left up — the pointer path,
    /// distinct from the touch path.
    fn click(&mut self) {
        let p = self.point();
        self.push(InputEvent::PointerMotionAbsolute { position: p });
        self.wait(20, 60);
        self.push(InputEvent::PointerButton {
            button: Button::Left,
            state: KeyState::Pressed,
        });
        self.wait(40, 90);
        self.push(InputEvent::PointerButton {
            button: Button::Left,
            state: KeyState::Released,
        });
    }

    /// Wheel notches at a random position, either direction.
    fn wheel(&mut self) {
        let p = self.point();
        self.push(InputEvent::PointerMotionAbsolute { position: p });
        let dir = if self.rng.below(2) == 0 { 1.0 } else { -1.0 };
        for _ in 0..self.rng.range(1, 4) {
            self.wait(30, 70);
            self.push(InputEvent::PointerAxis {
                horizontal: 0.0,
                vertical: dir,
                source: AxisSource::Wheel,
            });
        }
    }

    fn key(&mut self, sym: Keysym, utf8: Option<&str>, mods: Modifiers) {
        for state in [KeyState::Pressed, KeyState::Released] {
            self.push(InputEvent::Key(KeyEvent {
                code: 0,
                keysym: sym,
                utf8: utf8.map(str::to_owned),
                state,
                modifiers: mods,
            }));
            if state == KeyState::Pressed {
                self.wait(30, 80);
            }
        }
    }

    /// A navigation/editing key. Tab sometimes carries Shift, so focus walks
    /// backward as well as forward.
    fn nav_key(&mut self) {
        let sym = NAV_KEYS[self.rng.below(NAV_KEYS.len() as u64) as usize];
        let mods = if sym == keysym::TAB && self.rng.below(100) < 30 {
            Modifiers::SHIFT
        } else {
            Modifiers::empty()
        };
        self.key(sym, None, mods);
    }

    /// A printable character, delivered with its UTF-8 like a real keymap.
    fn text(&mut self) {
        let chars: Vec<char> = ALPHABET.chars().collect();
        let c = chars[self.rng.below(chars.len() as u64) as usize];
        let cp = c as u32;
        // X11 convention: Latin-1 keysyms are the codepoint; everything else
        // lives at 0x01000000 + codepoint. The runner keys off utf8 anyway.
        let sym = Keysym(if cp < 0x100 { cp } else { 0x0100_0000 + cp });
        self.key(sym, Some(&c.to_string()), Modifiers::empty());
    }
}

/// Generate a monkey session as a complete `.rec` v1 file: at least
/// `min_events` input events (the closing action finishes past the budget)
/// scattered over a `size.0 × size.1` screen, entirely determined by `seed`.
pub(crate) fn script(seed: u64, min_events: usize, size: (u32, u32)) -> String {
    let mut g = Gen {
        rng: Rng(seed),
        w: size.0.max(1) as u64,
        h: size.1.max(1) as u64,
        ms: 0,
        events: Vec::with_capacity(min_events + 16),
    };

    while g.events.len() < min_events.max(1) {
        g.wait(15, 120);
        // Weights sum to 100; touch-heavy, because kiosks are.
        match g.rng.below(100) {
            0..=25 => g.tap(),
            26..=41 => g.drag(),
            42..=47 => g.long_press(),
            48..=50 => g.cancelled_drag(),
            51..=62 => g.click(),
            63..=72 => g.wheel(),
            73..=89 => g.nav_key(),
            _ => g.text(),
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "fbui-rec 1 {}x{}", size.0, size.1);
    let _ = writeln!(
        out,
        "# fbui monkey session — seed {seed}, {} events on {}x{}",
        g.events.len(),
        size.0,
        size.1
    );
    let _ = writeln!(
        out,
        "# reproduce with: FBUI_REPLAY=<this file>  (see docs/record-replay.md)"
    );
    for (ms, ev) in &g.events {
        let body = event_line(ev).expect("the monkey only generates recordable events");
        let _ = writeln!(out, "@{ms} {body}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{parse_event, Replayer};

    const SIZE: (u32, u32) = (1280, 800);

    /// Every `@`-line of `text`, parsed through the real replay parser.
    fn events_of(text: &str) -> Vec<(u64, InputEvent)> {
        let mut r = Replayer::parse(text, f64::INFINITY).expect("valid rec file");
        r.due_events()
    }

    #[test]
    fn same_seed_same_script_different_seed_different_script() {
        let a = script(7, 300, SIZE);
        let b = script(7, 300, SIZE);
        let c = script(8, 300, SIZE);
        assert_eq!(a, b, "a seed must fully determine the session");
        assert_ne!(a, c, "different seeds must diverge");
    }

    #[test]
    fn script_is_a_valid_recording_and_meets_the_event_budget() {
        let text = script(42, 500, SIZE);
        let line_count = text.lines().filter(|l| l.starts_with('@')).count();
        let events = events_of(&text);
        assert_eq!(
            events.len(),
            line_count,
            "every generated line must parse (nothing skipped)"
        );
        assert!(events.len() >= 500, "budget honored: {}", events.len());
        // Each line must round-trip through the recording grammar verbatim.
        for l in text.lines().filter(|l| l.starts_with('@')) {
            let body = l.split_once(' ').unwrap().1;
            assert!(parse_event(body).is_some(), "unparseable line {l:?}");
        }
    }

    #[test]
    fn all_positions_are_on_screen() {
        let (w, h) = (SIZE.0 as i32, SIZE.1 as i32);
        for (_, ev) in events_of(&script(1, 800, SIZE)) {
            let p = match ev {
                InputEvent::PointerMotionAbsolute { position } => position,
                InputEvent::TouchDown { position, .. } => position,
                InputEvent::TouchMotion { position, .. } => position,
                _ => continue,
            };
            assert!(
                (0..w).contains(&p.x) && (0..h).contains(&p.y),
                "off-screen event at {},{}",
                p.x,
                p.y
            );
        }
    }

    #[test]
    fn timestamps_are_strictly_monotonic_enough_to_replay() {
        let mut last = 0u64;
        for (ms, _) in events_of(&script(3, 600, SIZE)) {
            assert!(ms >= last, "clock went backwards: {ms} after {last}");
            last = ms;
        }
    }

    /// The session must leave every input device at rest: no held touch
    /// contact, mouse button, or key, and no touch-down while a contact is
    /// already active (the monkey models a single slot-0 finger).
    #[test]
    fn every_gesture_is_balanced_and_nothing_is_left_held() {
        let mut touch_down = false;
        let mut button_down = false;
        let mut keys_down = 0i32;
        for (_, ev) in events_of(&script(9, 1000, SIZE)) {
            match ev {
                InputEvent::TouchDown { .. } => {
                    assert!(!touch_down, "touch down while a contact is active");
                    touch_down = true;
                }
                InputEvent::TouchMotion { .. } => assert!(touch_down, "motion with no contact"),
                InputEvent::TouchUp { .. } | InputEvent::TouchCancel => {
                    assert!(touch_down, "up/cancel with no contact");
                    touch_down = false;
                }
                InputEvent::PointerButton { state, .. } => {
                    match state {
                        KeyState::Pressed => {
                            assert!(!button_down, "button pressed twice");
                            button_down = true;
                        }
                        KeyState::Released => {
                            assert!(button_down, "release with no press");
                            button_down = false;
                        }
                        KeyState::Repeated => {}
                    };
                }
                InputEvent::Key(k) => match k.state {
                    KeyState::Pressed => keys_down += 1,
                    KeyState::Released => keys_down -= 1,
                    KeyState::Repeated => {}
                },
                _ => {}
            }
        }
        assert!(!touch_down, "session ends with a held touch");
        assert!(!button_down, "session ends with a held button");
        assert_eq!(keys_down, 0, "session ends with a held key");
    }

    /// Escape is the runner's quit key; the monkey must never press it, or a
    /// plain `FBUI_REPLAY` of the saved script would end the app early
    /// instead of reproducing the crash.
    #[test]
    fn the_monkey_never_presses_escape() {
        for (_, ev) in events_of(&script(5, 1000, SIZE)) {
            if let InputEvent::Key(k) = ev {
                assert_ne!(k.keysym, keysym::ESCAPE);
            }
        }
    }

    #[test]
    fn a_degenerate_screen_still_generates_in_bounds() {
        // 1×1: everything lands on the only pixel; must not panic or divide
        // by zero.
        for (_, ev) in events_of(&script(2, 100, (1, 1))) {
            if let InputEvent::TouchDown { position, .. } = ev {
                assert_eq!((position.x, position.y), (0, 0));
            }
        }
    }
}

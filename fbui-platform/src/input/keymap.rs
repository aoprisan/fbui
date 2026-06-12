//! Keymap translation: evdev keycode + modifier state → keysym + text.
//!
//! Two implementations behind one [`Keymap`] type:
//!
//! * with the `xkbcommon` feature, a real `xkb_state` driven from the system
//!   layout (XKB env vars / config), giving correct international layouts,
//!   dead keys, and compose;
//! * without it, a built-in US-QWERTY table — enough to bring a device up and
//!   type ASCII, which is all the Phase 1 echo demo needs.
//!
//! Both update internal modifier latches as keys go down/up and report the
//! [`Modifiers`] active for each event.

use super::{Keysym, Modifiers};

/// Translates raw key events for one keyboard.
pub struct Keymap {
    inner: Inner,
    mods: Modifiers,
}

enum Inner {
    #[cfg(feature = "xkbcommon")]
    Xkb(XkbKeymap),
    Builtin,
}

/// Result of translating one key transition.
pub struct Translated {
    pub keysym: Keysym,
    pub utf8: Option<String>,
    pub modifiers: Modifiers,
}

impl Keymap {
    /// Build the best keymap available: xkbcommon from the environment if the
    /// feature is on and it initializes, else the built-in US table.
    pub fn new() -> Self {
        #[cfg(feature = "xkbcommon")]
        {
            if let Some(xkb) = XkbKeymap::from_env() {
                return Keymap {
                    inner: Inner::Xkb(xkb),
                    mods: Modifiers::empty(),
                };
            }
        }
        Keymap {
            inner: Inner::Builtin,
            mods: Modifiers::empty(),
        }
    }

    /// Currently-latched modifiers.
    pub fn modifiers(&self) -> Modifiers {
        self.mods
    }

    /// Feed a key transition (evdev keycode, pressed?) and get its keysym/text.
    /// `pressed` is true for press or repeat.
    pub fn key(&mut self, code: u32, pressed: bool) -> Translated {
        // Track modifier latches first so the returned `modifiers` reflect this
        // event (e.g. Shift down toggles SHIFT immediately).
        if let Some(m) = modifier_for(code) {
            self.mods.set(m, pressed);
        }
        if pressed && code == KEY_CAPSLOCK {
            self.mods.toggle(Modifiers::CAPS);
        }

        match &mut self.inner {
            #[cfg(feature = "xkbcommon")]
            Inner::Xkb(xkb) => xkb.key(code, pressed, &mut self.mods),
            Inner::Builtin => {
                let (keysym, utf8) = builtin_translate(code, self.mods);
                Translated {
                    keysym,
                    utf8: if pressed { utf8 } else { None },
                    modifiers: self.mods,
                }
            }
        }
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Self::new()
    }
}

// evdev keycodes we care about for modifier tracking (`<linux/input-event-codes.h>`).
const KEY_LEFTSHIFT: u32 = 42;
const KEY_RIGHTSHIFT: u32 = 54;
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;
const KEY_LEFTMETA: u32 = 125;
const KEY_RIGHTMETA: u32 = 126;
const KEY_CAPSLOCK: u32 = 58;

fn modifier_for(code: u32) -> Option<Modifiers> {
    match code {
        KEY_LEFTSHIFT | KEY_RIGHTSHIFT => Some(Modifiers::SHIFT),
        KEY_LEFTCTRL | KEY_RIGHTCTRL => Some(Modifiers::CTRL),
        KEY_LEFTALT | KEY_RIGHTALT => Some(Modifiers::ALT),
        KEY_LEFTMETA | KEY_RIGHTMETA => Some(Modifiers::LOGO),
        _ => None,
    }
}

/// Built-in US-QWERTY translation. Returns the keysym and, for printable keys,
/// the text produced under the current modifiers.
fn builtin_translate(code: u32, mods: Modifiers) -> (Keysym, Option<String>) {
    use super::keysym::*;

    // Named non-text keys first.
    let named = match code {
        14 => Some(BACKSPACE),
        15 => Some(TAB),
        28 | 96 => Some(RETURN), // enter, keypad enter
        1 => Some(ESCAPE),
        111 => Some(DELETE),
        102 => Some(HOME),
        107 => Some(END),
        105 => Some(LEFT),
        103 => Some(UP),
        106 => Some(RIGHT),
        108 => Some(DOWN),
        _ => None,
    };
    if let Some(k) = named {
        // Enter/Tab/Backspace produce control characters in a terminal sense, but
        // the toolkit handles them as keysyms; emit no UTF-8.
        return (k, None);
    }

    // Printable keys: pick base vs shifted glyph.
    let shift = mods.contains(Modifiers::SHIFT) ^ mods.contains(Modifiers::CAPS);
    // Caps only affects letters; for non-letters use SHIFT alone.
    let shift_punct = mods.contains(Modifiers::SHIFT);

    let ch: Option<char> = match code {
        // number row
        2 => Some(if shift_punct { '!' } else { '1' }),
        3 => Some(if shift_punct { '@' } else { '2' }),
        4 => Some(if shift_punct { '#' } else { '3' }),
        5 => Some(if shift_punct { '$' } else { '4' }),
        6 => Some(if shift_punct { '%' } else { '5' }),
        7 => Some(if shift_punct { '^' } else { '6' }),
        8 => Some(if shift_punct { '&' } else { '7' }),
        9 => Some(if shift_punct { '*' } else { '8' }),
        10 => Some(if shift_punct { '(' } else { '9' }),
        11 => Some(if shift_punct { ')' } else { '0' }),
        12 => Some(if shift_punct { '_' } else { '-' }),
        13 => Some(if shift_punct { '+' } else { '=' }),
        // qwerty rows (letters honor caps XOR shift)
        16 => Some(letter('q', shift)),
        17 => Some(letter('w', shift)),
        18 => Some(letter('e', shift)),
        19 => Some(letter('r', shift)),
        20 => Some(letter('t', shift)),
        21 => Some(letter('y', shift)),
        22 => Some(letter('u', shift)),
        23 => Some(letter('i', shift)),
        24 => Some(letter('o', shift)),
        25 => Some(letter('p', shift)),
        26 => Some(if shift_punct { '{' } else { '[' }),
        27 => Some(if shift_punct { '}' } else { ']' }),
        30 => Some(letter('a', shift)),
        31 => Some(letter('s', shift)),
        32 => Some(letter('d', shift)),
        33 => Some(letter('f', shift)),
        34 => Some(letter('g', shift)),
        35 => Some(letter('h', shift)),
        36 => Some(letter('j', shift)),
        37 => Some(letter('k', shift)),
        38 => Some(letter('l', shift)),
        39 => Some(if shift_punct { ':' } else { ';' }),
        40 => Some(if shift_punct { '"' } else { '\'' }),
        41 => Some(if shift_punct { '~' } else { '`' }),
        43 => Some(if shift_punct { '|' } else { '\\' }),
        44 => Some(letter('z', shift)),
        45 => Some(letter('x', shift)),
        46 => Some(letter('c', shift)),
        47 => Some(letter('v', shift)),
        48 => Some(letter('b', shift)),
        49 => Some(letter('n', shift)),
        50 => Some(letter('m', shift)),
        51 => Some(if shift_punct { '<' } else { ',' }),
        52 => Some(if shift_punct { '>' } else { '.' }),
        53 => Some(if shift_punct { '?' } else { '/' }),
        57 => Some(' '),
        _ => None,
    };

    match ch {
        // The keysym for a Latin-1 char equals its codepoint (X11 convention).
        Some(c) => (Keysym(c as u32), Some(c.to_string())),
        None => (super::keysym::NONE, None),
    }
}

fn letter(base: char, upper: bool) -> char {
    if upper {
        base.to_ascii_uppercase()
    } else {
        base
    }
}

// ---- xkbcommon path -------------------------------------------------------

#[cfg(feature = "xkbcommon")]
struct XkbKeymap {
    state: xkbcommon::xkb::State,
}

#[cfg(feature = "xkbcommon")]
impl XkbKeymap {
    fn from_env() -> Option<Self> {
        use xkbcommon::xkb;
        let ctx = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        // Empty names => xkbcommon reads RULES/MODEL/LAYOUT from env, else default.
        let keymap =
            xkb::Keymap::new_from_names(&ctx, "", "", "", "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)?;
        let state = xkb::State::new(&keymap);
        Some(XkbKeymap { state })
    }

    fn key(&mut self, code: u32, pressed: bool, mods: &mut Modifiers) -> Translated {
        use xkbcommon::xkb;
        // xkb keycodes are evdev codes + 8.
        let xkb_code: xkb::Keycode = (code + 8).into();
        let keysym = self.state.key_get_one_sym(xkb_code);
        let utf8 = if pressed {
            let s = self.state.key_get_utf8(xkb_code);
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        } else {
            None
        };
        let dir = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        self.state.update_key(xkb_code, dir);
        // Mirror xkb's modifier view into our flags for consumers.
        mods.set(
            Modifiers::SHIFT,
            self.state
                .mod_name_is_active(xkb::MOD_NAME_SHIFT, xkb::STATE_MODS_EFFECTIVE),
        );
        mods.set(
            Modifiers::CTRL,
            self.state
                .mod_name_is_active(xkb::MOD_NAME_CTRL, xkb::STATE_MODS_EFFECTIVE),
        );
        mods.set(
            Modifiers::ALT,
            self.state
                .mod_name_is_active(xkb::MOD_NAME_ALT, xkb::STATE_MODS_EFFECTIVE),
        );
        mods.set(
            Modifiers::LOGO,
            self.state
                .mod_name_is_active(xkb::MOD_NAME_LOGO, xkb::STATE_MODS_EFFECTIVE),
        );
        mods.set(
            Modifiers::CAPS,
            self.state
                .mod_name_is_active(xkb::MOD_NAME_CAPS, xkb::STATE_MODS_EFFECTIVE),
        );
        Translated {
            keysym: Keysym(keysym.raw()),
            utf8,
            modifiers: *mods,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercase_letter() {
        let mut km = Keymap::new();
        let t = km.key(30, true); // 'a'
        assert_eq!(t.utf8.as_deref(), Some("a"));
    }

    #[test]
    fn shifted_letter_is_upper() {
        let mut km = Keymap::new();
        km.key(KEY_LEFTSHIFT, true);
        let t = km.key(30, true);
        assert_eq!(t.utf8.as_deref(), Some("A"));
        assert!(t.modifiers.contains(Modifiers::SHIFT));
    }

    #[test]
    fn shifted_number_is_symbol() {
        let mut km = Keymap::new();
        km.key(KEY_LEFTSHIFT, true);
        let t = km.key(2, true); // '1' -> '!'
        assert_eq!(t.utf8.as_deref(), Some("!"));
    }

    #[test]
    fn caps_xor_shift_for_letters() {
        let mut km = Keymap::new();
        km.key(KEY_CAPSLOCK, true); // caps on
        assert_eq!(km.key(30, true).utf8.as_deref(), Some("A"));
        km.key(KEY_LEFTSHIFT, true); // caps + shift -> lowercase
        assert_eq!(km.key(30, true).utf8.as_deref(), Some("a"));
    }

    #[test]
    fn enter_has_keysym_no_text() {
        let mut km = Keymap::new();
        let t = km.key(28, true);
        assert_eq!(t.keysym, super::super::keysym::RETURN);
        assert!(t.utf8.is_none());
    }

    #[test]
    fn release_produces_no_text() {
        let mut km = Keymap::new();
        let t = km.key(30, false);
        assert!(t.utf8.is_none());
    }
}

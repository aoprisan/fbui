//! Widget-level input events, in **logical** coordinates.
//!
//! The platform layer speaks physical pixels and raw keysyms; the umbrella
//! runner (or any embedder) translates those into these logical, toolkit-shaped
//! events before they reach a widget. Keeping this enum independent of
//! `fbui-platform` is what lets the widget layer be tested headlessly.

use fbui_render::geom::Point;

/// Which pointer/mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

/// Keyboard modifier state at the time of a key event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

/// A semantic key. Printable input arrives as [`Key::Char`] (with the resolved
/// character); everything else is a named editing/navigation key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Delete,
    Enter,
    Tab,
    Escape,
    Space,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Unknown,
}

/// An input event delivered to a widget.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// A pointer button went down at `pos`.
    PointerDown { pos: Point, button: PointerButton },
    /// A pointer button was released at `pos`.
    PointerUp { pos: Point, button: PointerButton },
    /// The pointer moved to `pos`.
    PointerMove { pos: Point },
    /// The pointer left this widget (hover ended).
    PointerLeave,
    /// A scroll wheel / two-finger scroll, in logical pixels.
    Scroll {
        pos: Point,
        delta_x: f32,
        delta_y: f32,
    },
    /// A recognized tap (quick press-and-release in place). Delivered in addition
    /// to the raw `PointerDown`/`PointerUp` for widgets that want tap semantics.
    Tap { pos: Point },
    /// A recognized long-press (contact held in place past the timeout).
    LongPress { pos: Point },
    /// A drag was released with momentum — `velocity` is in logical pixels per
    /// second. Scrollable widgets turn this into kinetic (coasting) scroll.
    Fling {
        pos: Point,
        velocity_x: f32,
        velocity_y: f32,
    },
    /// A key changed state. `text` is the committed string for printable keys.
    Key {
        key: Key,
        pressed: bool,
        mods: Modifiers,
    },
    /// This widget just gained keyboard focus.
    FocusGained,
    /// This widget just lost keyboard focus.
    FocusLost,
    /// The popup this widget owns (see [`Ui::open_popup`]) was dismissed by
    /// the [`Ui`] — an outside click/tap, or a popup opening over it — rather
    /// than closed by the widget itself. Sync internal state (`open = false`)
    /// and damage; the popup entry is already gone, so calling
    /// `close_popup` again is a harmless no-op.
    ///
    /// [`Ui`]: crate::Ui
    /// [`Ui::open_popup`]: crate::Ui::open_popup
    PopupDismissed,
}

impl Event {
    /// The pointer position carried by this event, if any (for hit-testing).
    pub fn pointer_pos(&self) -> Option<Point> {
        match self {
            Event::PointerDown { pos, .. }
            | Event::PointerUp { pos, .. }
            | Event::PointerMove { pos }
            | Event::Scroll { pos, .. }
            | Event::Tap { pos }
            | Event::LongPress { pos }
            | Event::Fling { pos, .. } => Some(*pos),
            _ => None,
        }
    }
}

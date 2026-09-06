# Text editing: `TextInput`, `TextArea`, and the clipboard

fbui has two editable text widgets. **`TextInput`** is the single-line field
(a name, a URL, a PIN); **`TextArea`** is the multi-line box (notes, an
address, a message) with word-wrapped paragraphs and a vertical scroll that
follows the caret. Both share one editing core, so every key below means the
same thing in each — and both are headless, deterministic widgets: pure
state machines fed `Event`s, testable without a display.

```rust
use fbui::widgets::{TextArea, TextInput};

ui.add_child(root, TextInput::new().placeholder("your name").on_change(Msg::Name));
ui.add_child(root, TextArea::new().rows(4).placeholder("notes").on_change(Msg::Notes));
```

`on_change` fires with the whole text after every edit (typing, deletion,
cut, paste) and never for a pure caret move. `text()` reads the value,
`set_text` replaces it (via `Ui::with`), and `select` / `select_all` /
`selection()` expose the selected byte range for an app that wants to act on
it (a "copy to serial" button, say).

## Keys

| Key | Action |
|---|---|
| printable, `Space` | insert at the caret, replacing the selection |
| `Backspace` / `Delete` | delete the selection, else one character back / forward |
| `Ctrl+Backspace` / `Ctrl+Delete` | delete the selection, else one *word* back / forward |
| `Left` / `Right` | move one character; with a selection, collapse onto its edge |
| `Ctrl+Left` / `Ctrl+Right` | move one word |
| `Home` / `End` | `TextInput`: start / end of the text. `TextArea`: start / end of the caret's *visual* line |
| `Ctrl+Home` / `Ctrl+End` | start / end of the whole text |
| `Up` / `Down` (`TextArea`) | move one visual line, keeping the column (the goal column survives a shorter line in between) |
| `PageUp` / `PageDown` (`TextArea`) | move one viewport height |
| `Enter` (`TextArea`) | insert a line break |
| `Shift` + any move | extend the selection instead of collapsing it |
| `Ctrl+A` | select all |
| `Ctrl+C` / `Ctrl+X` | copy / cut the selection to the clipboard |
| `Ctrl+V` | paste (a `TextInput` flattens line breaks to spaces) |
| `Tab` | never inserted: it moves focus, as everywhere in fbui |

`Enter` in a `TextInput` is left unhandled so it bubbles to the ancestors —
a form can act on it. Other `Ctrl` chords are ignored rather than typed
(`Ctrl+Q` inserts nothing). Chord letters are matched case-insensitively, so
Caps Lock doesn't break them.

Words are runs of alphanumeric characters (plus `_`); punctuation runs are
their own words, so `Ctrl+Right` from the start of `foo.bar` stops after
`foo`, then after `.`.

## Pointer and touch

- **Click / tap** places the caret at the nearest glyph boundary — across
  wrapped lines in a `TextArea`, and past the visible edge of a scrolled
  `TextInput`.
- **Drag** selects from the press point to the pointer (the widget captures
  the pointer, so a drag can leave the box).
- **Long-press** (touch, or a held mouse button) selects the word under it —
  there is no double-click gesture on a touch panel, so this is the
  word-select affordance.
- **Wheel** over a `TextArea` scrolls it; at its top or bottom the wheel
  bubbles to an enclosing `ScrollView`, so a page with a text box in it
  still scrolls as a page.

## The clipboard

There is **no system clipboard** on an fbui target: no display server
means no X selection and no Wayland data device. The `Ui` therefore owns
the one clipboard the process has — a plain `String`:

- `Ctrl+C` / `Ctrl+X` in any text widget write it, `Ctrl+V` reads it, so
  text moves freely between fields (and between a `TextInput` and a
  `TextArea`).
- The app reads it with `Ui::clipboard()` and installs text with
  `Ui::set_clipboard(..)`. That is the bridge to anything outside the
  process — the remote console, a serial link, a file, a QR code — fbui does
  not guess what that should be.
- A custom widget gets the same access through `EventCtx::clipboard()` /
  `EventCtx::set_clipboard(..)`.

## `TextArea` sizing and scrolling

`rows(n)` sets the box height in lines (default 4); the box never grows
with its content — it scrolls, showing a thin thumb on the right when there
is more than fits. `grow(f)` lets it take the remaining space in a column
instead, with `rows` as the minimum. Text wraps at the box width at word
boundaries (falling back to per-glyph for a word wider than the box).

Every edit or caret move re-shapes the paragraph with cosmic-text; for the
few hundred characters a kiosk text box holds that is well inside a frame,
and the area repaints only its own box. It is not a code editor: there is
no undo stack, no syntax anything, and no scroll-blit fast path for the
scroll (a scrolled repaint re-rasterizes the visible lines).

## Backends and keys

The widget layer only sees `Key::Char('c')` with `mods.ctrl`; the runner
makes every backend produce that. The built-in evdev keymap reports the
letter directly; xkbcommon reports the control character (`\x03`) and the
terminal backend reports the bare keysym with no text, so the runner falls
back to the keysym when there is no printable text. A `.rec` recording
carries the modifier bits, so a replayed session cuts and pastes exactly as
the live one did.

## Not here (yet)

- **IME / composed input** — explicitly out of scope per `PLAN.md`; the
  `Key::Char` path carries only committed characters.
- **Double-click** word selection — the gesture recognizer has no
  multi-tap; long-press covers touch, and a mouse user can drag.
- **Undo / redo**, rich text, and a horizontal-scroll `TextArea` (no-wrap).

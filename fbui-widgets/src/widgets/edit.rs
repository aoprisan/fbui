//! The editing state machine shared by [`TextInput`](super::TextInput) and
//! [`TextArea`](super::TextArea): a `String` with a caret and a selection
//! anchor, plus the key semantics every text field agrees on — character and
//! word deletion, word jumps, select-all, and the clipboard chords. Pure and
//! font-free (line-aware moves that need glyph geometry stay in the widgets),
//! so the shortcut table is unit-tested here once and can't drift between the
//! two widgets.

use crate::event::{Key, Modifiers};

/// What applying a key to an [`EditState`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Applied {
    /// The key meant something to the editor (navigation, editing, clipboard).
    pub handled: bool,
    /// The text changed (fire `on_change`).
    pub changed: bool,
    /// The caret or selection moved without changing the text.
    pub moved: bool,
}

impl Applied {
    const IGNORED: Applied = Applied {
        handled: false,
        changed: false,
        moved: false,
    };
    const CHANGED: Applied = Applied {
        handled: true,
        changed: true,
        moved: true,
    };
    const MOVED: Applied = Applied {
        handled: true,
        changed: false,
        moved: true,
    };
    const CONSUMED: Applied = Applied {
        handled: true,
        changed: false,
        moved: false,
    };
}

/// Text + caret + selection anchor. Byte offsets are always char boundaries.
#[derive(Debug, Clone, Default)]
pub(crate) struct EditState {
    pub text: String,
    /// The caret: the boundary edits happen at.
    pub cursor: usize,
    /// The other end of the selection (== `cursor` when nothing is selected).
    pub anchor: usize,
}

impl EditState {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let end = text.len();
        EditState {
            text,
            cursor: end,
            anchor: end,
        }
    }

    /// Replace the text, keeping the caret in range.
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.clamp_boundary(self.cursor);
        self.anchor = self.clamp_boundary(self.anchor);
    }

    fn clamp_boundary(&self, i: usize) -> usize {
        let mut i = i.min(self.text.len());
        while !self.text.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// The selected byte range, ordered.
    pub fn selection(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    pub fn selected_text(&self) -> &str {
        let (a, b) = self.selection();
        &self.text[a..b]
    }

    /// Delete the selection, if any; returns whether anything was deleted.
    pub fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.selection();
        self.text.replace_range(a..b, "");
        self.cursor = a;
        self.anchor = a;
        true
    }

    /// Insert at the caret, replacing any selection.
    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.anchor = self.cursor;
    }

    /// Move the caret; `extend` keeps the anchor (Shift held), otherwise the
    /// selection collapses to the new position.
    pub fn move_cursor(&mut self, to: usize, extend: bool) {
        self.cursor = self.clamp_boundary(to);
        if !extend {
            self.anchor = self.cursor;
        }
    }

    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Select `a..b` (either order), caret at `b`.
    pub fn select(&mut self, a: usize, b: usize) {
        self.anchor = self.clamp_boundary(a);
        self.cursor = self.clamp_boundary(b);
    }

    pub fn prev_boundary(&self, i: usize) -> usize {
        self.text[..i]
            .char_indices()
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    pub fn next_boundary(&self, i: usize) -> usize {
        self.text[i..]
            .char_indices()
            .nth(1)
            .map(|(idx, _)| i + idx)
            .unwrap_or(self.text.len())
    }

    /// Start of the word before `i`: skip whitespace backwards, then the run
    /// of same-class characters (word characters, or punctuation) before it.
    pub fn prev_word(&self, i: usize) -> usize {
        let mut chars: Vec<(usize, char)> = self.text[..i].char_indices().collect();
        while let Some(&(_, c)) = chars.last() {
            if c.is_whitespace() {
                chars.pop();
            } else {
                break;
            }
        }
        let Some(&(_, first)) = chars.last() else {
            return 0;
        };
        let class = char_class(first);
        let mut start = i;
        while let Some(&(idx, c)) = chars.last() {
            if char_class(c) == class {
                start = idx;
                chars.pop();
            } else {
                break;
            }
        }
        if start == i {
            // Only whitespace before us: land on the start.
            chars.last().map_or(0, |&(idx, c)| idx + c.len_utf8())
        } else {
            start
        }
    }

    /// End of the word after `i`: skip the run of same-class characters, then
    /// any whitespace following it (the caret lands at the next word's start,
    /// the way Ctrl+Right behaves in most editors).
    pub fn next_word(&self, i: usize) -> usize {
        let mut it = self.text[i..].char_indices().peekable();
        let Some(&(_, first)) = it.peek() else {
            return self.text.len();
        };
        let mut end = i;
        if first.is_whitespace() {
            while let Some(&(idx, c)) = it.peek() {
                if c.is_whitespace() && c != '\n' {
                    end = i + idx + c.len_utf8();
                    it.next();
                } else {
                    break;
                }
            }
            if let Some(&(idx, c)) = it.peek() {
                if c == '\n' {
                    return i + idx;
                }
            }
        }
        let Some(&(_, first)) = it.peek() else {
            return end.max(i);
        };
        let class = char_class(first);
        while let Some(&(idx, c)) = it.peek() {
            if char_class(c) == class {
                end = i + idx + c.len_utf8();
                it.next();
            } else {
                break;
            }
        }
        while let Some(&(idx, c)) = it.peek() {
            if c.is_whitespace() && c != '\n' {
                end = i + idx + c.len_utf8();
                it.next();
            } else {
                break;
            }
        }
        end
    }

    /// The word around `i` (for a double-click / long-press select).
    pub fn word_at(&self, i: usize) -> (usize, usize) {
        let i = self.clamp_boundary(i);
        let class_at = |idx: usize| self.text[idx..].chars().next().map(char_class);
        let class = class_at(i)
            .or_else(|| {
                if i > 0 {
                    class_at(self.prev_boundary(i))
                } else {
                    None
                }
            })
            .unwrap_or(CharClass::Space);
        let mut a = i;
        while a > 0 {
            let p = self.prev_boundary(a);
            if class_at(p) == Some(class) {
                a = p;
            } else {
                break;
            }
        }
        let mut b = i;
        while b < self.text.len() && class_at(b) == Some(class) {
            b = self.next_boundary(b);
        }
        (a, b)
    }

    /// Apply the shared key table. `multiline` admits `Enter` as a newline and
    /// keeps line breaks in pasted text (a single-line field flattens them to
    /// spaces). Line-aware keys (`Up`/`Down`/`Home`/`End`/paging) are left to
    /// the caller, which owns the glyph geometry; `Home`/`End` here are the
    /// whole-text moves a single-line field wants.
    pub fn apply(
        &mut self,
        key: Key,
        mods: Modifiers,
        multiline: bool,
        clipboard: &mut String,
    ) -> Applied {
        let extend = mods.shift;
        if mods.ctrl {
            return match key {
                Key::Char(c) => match c.to_ascii_lowercase() {
                    'a' => {
                        self.select_all();
                        Applied::MOVED
                    }
                    'c' => {
                        if self.has_selection() {
                            *clipboard = self.selected_text().to_string();
                        }
                        Applied::CONSUMED
                    }
                    'x' => {
                        if self.has_selection() {
                            *clipboard = self.selected_text().to_string();
                            self.delete_selection();
                            Applied::CHANGED
                        } else {
                            Applied::CONSUMED
                        }
                    }
                    'v' => {
                        if clipboard.is_empty() {
                            return Applied::CONSUMED;
                        }
                        let paste = if multiline {
                            clipboard.clone()
                        } else {
                            flatten_lines(clipboard)
                        };
                        self.insert(&paste);
                        Applied::CHANGED
                    }
                    _ => Applied::IGNORED,
                },
                Key::Left => {
                    let to = self.prev_word(self.cursor);
                    self.move_cursor(to, extend);
                    Applied::MOVED
                }
                Key::Right => {
                    let to = self.next_word(self.cursor);
                    self.move_cursor(to, extend);
                    Applied::MOVED
                }
                Key::Backspace => {
                    if !self.delete_selection() && self.cursor > 0 {
                        let to = self.prev_word(self.cursor);
                        self.text.replace_range(to..self.cursor, "");
                        self.cursor = to;
                        self.anchor = to;
                    }
                    Applied::CHANGED
                }
                Key::Delete => {
                    if !self.delete_selection() && self.cursor < self.text.len() {
                        let to = self.next_word(self.cursor);
                        self.text.replace_range(self.cursor..to, "");
                        self.anchor = self.cursor;
                    }
                    Applied::CHANGED
                }
                Key::Home => {
                    self.move_cursor(0, extend);
                    Applied::MOVED
                }
                Key::End => {
                    self.move_cursor(self.text.len(), extend);
                    Applied::MOVED
                }
                _ => Applied::IGNORED,
            };
        }
        match key {
            Key::Char(c) => {
                self.insert(&c.to_string());
                Applied::CHANGED
            }
            Key::Space => {
                self.insert(" ");
                Applied::CHANGED
            }
            Key::Enter if multiline => {
                self.insert("\n");
                Applied::CHANGED
            }
            Key::Backspace => {
                if !self.delete_selection() && self.cursor > 0 {
                    let prev = self.prev_boundary(self.cursor);
                    self.text.replace_range(prev..self.cursor, "");
                    self.cursor = prev;
                    self.anchor = prev;
                }
                Applied::CHANGED
            }
            Key::Delete => {
                if !self.delete_selection() && self.cursor < self.text.len() {
                    let next = self.next_boundary(self.cursor);
                    self.text.replace_range(self.cursor..next, "");
                    self.anchor = self.cursor;
                }
                Applied::CHANGED
            }
            Key::Left => {
                if self.has_selection() && !extend {
                    // Collapse onto the selection's start, like every editor.
                    let (a, _) = self.selection();
                    self.move_cursor(a, false);
                } else {
                    let to = self.prev_boundary(self.cursor);
                    self.move_cursor(to, extend);
                }
                Applied::MOVED
            }
            Key::Right => {
                if self.has_selection() && !extend {
                    let (_, b) = self.selection();
                    self.move_cursor(b, false);
                } else {
                    let to = self.next_boundary(self.cursor);
                    self.move_cursor(to, extend);
                }
                Applied::MOVED
            }
            Key::Home if !multiline => {
                self.move_cursor(0, extend);
                Applied::MOVED
            }
            Key::End if !multiline => {
                self.move_cursor(self.text.len(), extend);
                Applied::MOVED
            }
            _ => Applied::IGNORED,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Word,
    Punct,
    Space,
}

fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// Collapse line breaks to single spaces for a single-line field.
fn flatten_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut pending_space = false;
    for c in s.chars() {
        if c == '\n' || c == '\r' {
            pending_space = true;
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: true,
            ..Modifiers::default()
        }
    }

    fn ctrl_shift() -> Modifiers {
        Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
        }
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn typing_and_deleting_at_the_caret() {
        let mut e = EditState::default();
        let mut cb = String::new();
        e.apply(Key::Char('a'), Modifiers::default(), false, &mut cb);
        e.apply(Key::Char('b'), Modifiers::default(), false, &mut cb);
        e.apply(Key::Left, Modifiers::default(), false, &mut cb);
        e.apply(Key::Char('x'), Modifiers::default(), false, &mut cb);
        assert_eq!(e.text, "axb");
        e.apply(Key::Backspace, Modifiers::default(), false, &mut cb);
        assert_eq!(e.text, "ab");
        e.apply(Key::Delete, Modifiers::default(), false, &mut cb);
        assert_eq!(e.text, "a");
    }

    #[test]
    fn word_jumps_and_word_deletion() {
        let mut e = EditState::new("alpha beta  gamma");
        assert_eq!(e.prev_word(e.text.len()), 12);
        assert_eq!(e.prev_word(12), 6);
        assert_eq!(e.prev_word(6), 0);
        assert_eq!(e.next_word(0), 6);
        assert_eq!(e.next_word(6), 12);
        assert_eq!(e.next_word(12), 17);
        let mut cb = String::new();
        e.apply(Key::Backspace, ctrl(), false, &mut cb);
        assert_eq!(e.text, "alpha beta  ");
        e.move_cursor(0, false);
        e.apply(Key::Delete, ctrl(), false, &mut cb);
        assert_eq!(e.text, "beta  ");
    }

    #[test]
    fn punctuation_is_its_own_word_class() {
        let e = EditState::new("foo.bar");
        assert_eq!(e.next_word(0), 3);
        assert_eq!(e.next_word(3), 4);
        assert_eq!(e.word_at(1), (0, 3));
        assert_eq!(e.word_at(5), (4, 7));
        assert_eq!(e.word_at(3), (3, 4));
    }

    #[test]
    fn clipboard_round_trip_and_select_all() {
        let mut e = EditState::new("hello world");
        let mut cb = String::new();
        e.select(0, 5);
        assert_eq!(
            e.apply(Key::Char('C'), ctrl(), false, &mut cb),
            Applied::CONSUMED
        );
        assert_eq!(cb, "hello");
        e.move_cursor(e.text.len(), false);
        e.apply(Key::Space, Modifiers::default(), false, &mut cb);
        e.apply(Key::Char('v'), ctrl(), false, &mut cb);
        assert_eq!(e.text, "hello world hello");
        e.apply(Key::Char('a'), ctrl(), false, &mut cb);
        assert_eq!(e.selection(), (0, e.text.len()));
        e.apply(Key::Char('x'), ctrl(), false, &mut cb);
        assert_eq!(e.text, "");
        assert_eq!(cb, "hello world hello");
    }

    #[test]
    fn single_line_paste_flattens_line_breaks() {
        let mut e = EditState::default();
        let mut cb = "one\ntwo\r\nthree\n".to_string();
        e.apply(Key::Char('v'), ctrl(), false, &mut cb);
        assert_eq!(e.text, "one two three");
        let mut m = EditState::default();
        m.apply(Key::Char('v'), ctrl(), true, &mut cb);
        assert_eq!(m.text, "one\ntwo\r\nthree\n");
    }

    #[test]
    fn shift_extends_and_ctrl_shift_selects_words() {
        let mut e = EditState::new("alpha beta");
        let mut cb = String::new();
        e.move_cursor(0, false);
        e.apply(Key::Right, shift(), false, &mut cb);
        e.apply(Key::Right, shift(), false, &mut cb);
        assert_eq!(e.selected_text(), "al");
        e.apply(Key::Right, ctrl_shift(), false, &mut cb);
        assert_eq!(e.selected_text(), "alpha ");
        // A plain arrow collapses onto the selection edge.
        e.apply(Key::Left, Modifiers::default(), false, &mut cb);
        assert_eq!(e.cursor, 0);
        assert!(!e.has_selection());
    }

    #[test]
    fn other_ctrl_chords_are_not_typed() {
        let mut e = EditState::new("x");
        let mut cb = String::new();
        assert_eq!(
            e.apply(Key::Char('q'), ctrl(), false, &mut cb),
            Applied::IGNORED
        );
        assert_eq!(e.text, "x");
        assert_eq!(
            e.apply(Key::Enter, Modifiers::default(), false, &mut cb),
            Applied::IGNORED
        );
        assert_eq!(
            e.apply(Key::Enter, Modifiers::default(), true, &mut cb),
            Applied::CHANGED
        );
        assert_eq!(e.text, "x\n");
    }

    #[test]
    fn multibyte_boundaries_are_respected() {
        let mut e = EditState::new("héllo wörld");
        let mut cb = String::new();
        e.apply(Key::Left, ctrl(), false, &mut cb);
        assert_eq!(e.cursor, "héllo ".len());
        e.apply(Key::Backspace, Modifiers::default(), false, &mut cb);
        assert_eq!(e.text, "héllowörld");
        e.set_text("é");
        e.move_cursor(1, false); // inside the 2-byte char
        assert_eq!(e.cursor, 0);
    }
}

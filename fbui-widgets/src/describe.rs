//! [`Describe`]: what a widget says about itself in words.
//!
//! [`Ui::inspect`](crate::Ui::inspect) has always reported a node's type,
//! bounds and focus state. It could not say what a label *reads* or what an
//! input *holds*, which is the one thing a reader of text — an agent, a CI
//! job, a flow script, an operator on the remote console — needs most.
//! [`Widget::describe`](crate::Widget::describe) fills that gap: each widget
//! reports its user-visible content and state as ordered key/value pairs.
//!
//! It is called **only from `inspect`**, never on the paint or event path, so
//! it costs nothing in a normal frame.
//!
//! ## What to report
//!
//! * [`text`](Describe::text) — what a person would *read off* the widget:
//!   a label's text, a button's caption, an input's current content. This is
//!   what `expect … text` and `Kind "text"` flow references match against, so
//!   exactly one widget's-worth of user-visible text belongs here.
//! * [`value`](Describe::value) — the primary value of a widget that has one
//!   (a slider's position, a progress fraction).
//! * [`flag`](Describe::flag) / [`prop`](Describe::prop) — everything else a
//!   reader needs to know the widget's state: `checked`, `open`, `selected`,
//!   `offset`. Report *state*; skip styling that is at its default, or the
//!   dump becomes unreadable and the interesting line stops standing out.

use std::fmt::Display;

/// An ordered set of key/value pairs describing one widget.
///
/// Keys are `&'static str` so a description costs one `String` per value and
/// nothing per key. Insertion order is preserved and is the order a tree dump
/// prints, so a widget should report its most identifying property first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Describe {
    pairs: Vec<(&'static str, String)>,
}

/// The key [`Describe::text`] writes under — the widget's user-visible text.
pub const TEXT: &str = "text";
/// The key [`Describe::value`] writes under.
pub const VALUE: &str = "value";

impl Describe {
    pub fn new() -> Self {
        Describe::default()
    }

    /// The primary user-visible text of this widget (a label's words, a
    /// button's caption, a text field's content). Recorded under `text`.
    pub fn text(&mut self, s: &str) {
        self.set(TEXT, s.to_string());
    }

    /// The primary value of this widget (a slider position, a fraction).
    pub fn value(&mut self, v: impl Display) {
        self.set(VALUE, v.to_string());
    }

    /// A boolean state flag, recorded as `true` / `false`.
    pub fn flag(&mut self, key: &'static str, on: bool) {
        self.set(key, if on { "true" } else { "false" }.to_string());
    }

    /// Any other named property.
    pub fn prop(&mut self, key: &'static str, v: impl Display) {
        self.set(key, v.to_string());
    }

    /// Last write wins, in the position of the first: a widget that reports a
    /// key twice (a wrapper delegating to its inner state, say) gets one pair,
    /// not a duplicate that would make `expect` ambiguous.
    fn set(&mut self, key: &'static str, value: String) {
        match self.pairs.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.pairs.push((key, value)),
        }
    }

    /// The value reported under `key`, if any.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The user-visible text, if the widget reported any.
    pub fn text_value(&self) -> Option<&str> {
        self.get(TEXT)
    }

    /// Every pair, in the order the widget reported them.
    pub fn pairs(&self) -> &[(&'static str, String)] {
        &self.pairs
    }

    /// Whether the widget reported nothing at all.
    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// Consume into the pair list.
    pub fn into_pairs(self) -> Vec<(&'static str, String)> {
        self.pairs
    }
}

/// Format a float the way a tree dump should read it: `50` rather than `50`
/// dressed as `50.000000`, and `0.5` rather than `0.5000000001`.
///
/// Descriptions are compared by flow scripts (`expect #vol value 50`), so a
/// stable, short, round-trippable rendering matters more than full precision.
pub fn num(v: f32) -> String {
    if v == v.trunc() && v.abs() < 1e9 {
        format!("{}", v as i64)
    } else {
        // Two decimals is enough to tell UI values apart and keeps a dump
        // diffable across runs where the last float bit wobbles.
        let s = format!("{v:.2}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_keep_insertion_order_and_dedupe_by_key() {
        let mut d = Describe::new();
        d.text("hello");
        d.flag("checked", true);
        d.prop("rows", 3);
        d.text("goodbye");
        assert_eq!(
            d.pairs(),
            &[
                ("text", "goodbye".to_string()),
                ("checked", "true".to_string()),
                ("rows", "3".to_string()),
            ]
        );
        assert_eq!(d.text_value(), Some("goodbye"));
        assert_eq!(d.get("checked"), Some("true"));
        assert_eq!(d.get("missing"), None);
    }

    #[test]
    fn numbers_render_short_and_stable() {
        assert_eq!(num(50.0), "50");
        assert_eq!(num(-3.0), "-3");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(0.333_333), "0.33");
        assert_eq!(num(1.0 / 3.0 * 3.0), "1");
    }
}

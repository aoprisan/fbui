//! Flow scripts: a UI interaction written as steps and expectations
//! (`fbui-rec 2`), parsed and resolved here so every executor means the same
//! thing by it.
//!
//! ```text
//! fbui-rec 2
//! tap #add
//! type "milk"
//! key Enter
//! wait settle
//! expect #count text "1 item"
//! shot end.png
//! ```
//!
//! ## Why the meaning lives in the widget layer
//!
//! A flow runs three ways — in-process against a [`Ui`](crate::Ui) under
//! `cargo test`, through the runner under `FBUI_REPLAY`, and against a live
//! device over the remote console — and the three must agree, or a flow that
//! passes in CI proves nothing about the device. So everything that decides
//! *meaning* is here: parsing, resolving a reference against the live tree,
//! and evaluating an expectation. What differs per executor is only how an
//! [`Act`] is delivered — as a widget [`Event`](crate::Event), as a platform
//! input event, or as an HTTP request — and each of those is a dozen lines.
//!
//! ## Resolution happens at execution time
//!
//! A reference is resolved against [`Ui::inspect`](crate::Ui::inspect) *at
//! the moment its step runs*, never at parse time, so a flow follows the
//! layout: `tap #inc` lands on the button wherever it has moved to. A
//! reference that resolves to nothing — or to a widget that is not
//! [`visible`](crate::InspectNode::visible) — fails the step and prints the
//! tree, because that is the most common authoring mistake and it deserves
//! the most useful error.

use std::fmt;
use std::path::PathBuf;

use fbui_render::geom::Point;

use crate::event::{Key, Modifiers};
use crate::tree::InspectNode;

/// The header of a v2 flow file.
pub const HEADER_V2: &str = "fbui-rec 2";

// ---- references ------------------------------------------------------------

/// How a step names the widget it acts on.
#[derive(Debug, Clone, PartialEq)]
pub enum Ref {
    /// `#name`, or `#screen/field` scoped by named ancestors.
    Name(String),
    /// `Kind` or `Kind "text"` — the first match in tree order.
    Kind { kind: String, text: Option<String> },
    /// `@x,y` — logical coordinates, for the rare case with nothing to name.
    At(Point),
}

impl fmt::Display for Ref {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ref::Name(n) => write!(f, "#{n}"),
            Ref::Kind { kind, text: None } => write!(f, "{kind}"),
            Ref::Kind {
                kind,
                text: Some(t),
            } => write!(f, "{kind} {t:?}"),
            Ref::At(p) => write!(f, "@{},{}", p.x, p.y),
        }
    }
}

/// What a reference resolved to.
#[derive(Debug, Clone, Copy)]
pub enum Target<'a> {
    /// A widget in the live tree.
    Node(&'a InspectNode),
    /// A bare coordinate (`@x,y`), which names no widget.
    Point(Point),
}

impl Target<'_> {
    /// Where a pointer action lands: a widget's centre, or the point itself.
    pub fn point(&self) -> Point {
        match self {
            Target::Node(n) => {
                Point::new(n.bounds.x + n.bounds.w / 2.0, n.bounds.y + n.bounds.h / 2.0)
            }
            Target::Point(p) => *p,
        }
    }

    pub fn node(&self) -> Option<&InspectNode> {
        match self {
            Target::Node(n) => Some(n),
            Target::Point(_) => None,
        }
    }
}

/// Every widget a reference matches, in tree order. Used both to resolve
/// (the first match wins) and to report an ambiguous reference.
pub fn matches<'a>(root: &'a InspectNode, r: &Ref) -> Vec<&'a InspectNode> {
    match r {
        Ref::At(_) => Vec::new(),
        Ref::Name(name) => {
            // A path (`form/field`) matches when the earlier segments appear,
            // in order, among the node's named ancestors — the same rule
            // `Ui::find` applies, evaluated here against a snapshot.
            let (path, leaf) = match name.rsplit_once('/') {
                Some((p, l)) => (p.split('/').collect::<Vec<_>>(), l),
                None => (Vec::new(), name.as_str()),
            };
            let mut out = Vec::new();
            collect_named(root, leaf, &path, &mut Vec::new(), &mut out);
            out
        }
        Ref::Kind { kind, text } => root
            .iter()
            .filter(|n| {
                n.kind == *kind && text.as_deref().is_none_or(|t| n.text.as_deref() == Some(t))
            })
            .collect(),
    }
}

/// The deepest visible widget containing `p` that has a name — "what did the
/// user just touch?". The recorder annotates raw taps with it, and the remote
/// executor uses it to name a coordinate.
pub fn name_at(root: &InspectNode, p: Point) -> Option<&str> {
    fn walk<'a>(n: &'a InspectNode, p: Point, best: &mut Option<&'a str>) {
        if !n.visible || !n.bounds.contains_point(p) {
            return;
        }
        if let Some(name) = &n.name {
            *best = Some(name.as_str()); // deeper wins
        }
        for c in &n.children {
            walk(c, p, best);
        }
    }
    let mut best = None;
    walk(root, p, &mut best);
    best
}

fn collect_named<'a>(
    node: &'a InspectNode,
    leaf: &str,
    path: &[&str],
    ancestors: &mut Vec<&'a str>,
    out: &mut Vec<&'a InspectNode>,
) {
    if node.name.as_deref() == Some(leaf) && path_matches(path, ancestors) {
        out.push(node);
    }
    let named = node.name.is_some();
    if named {
        ancestors.push(node.name.as_deref().unwrap_or_default());
    }
    for c in &node.children {
        collect_named(c, leaf, path, ancestors, out);
    }
    if named {
        ancestors.pop();
    }
}

/// Whether `path` appears as an in-order subsequence of the named `ancestors`
/// (outermost first).
fn path_matches(path: &[&str], ancestors: &[&str]) -> bool {
    let mut want = path.iter();
    let mut next = want.next();
    for got in ancestors {
        if next == Some(got) {
            next = want.next();
        }
    }
    next.is_none()
}

// ---- steps -----------------------------------------------------------------

/// A named key with its modifiers, as `key Ctrl+C` writes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeySpec {
    pub key: Key,
    pub mods: Modifiers,
}

/// What an expectation asserts about a widget (or about the whole tree).
#[derive(Debug, Clone, PartialEq)]
pub enum Expect {
    /// `expect #x <key> <value>` / `expect #x checked` — a described property
    /// equals a value (a bare flag means `"true"`).
    Prop { key: String, value: String },
    /// `expect Label "Hi"` — something matches the reference.
    Exists,
    /// `expect #dialog absent` — nothing matches it.
    Absent,
    /// `expect #x visible` — it matches *and* is on screen.
    Visible,
    /// `expect #x hidden` — it matches but is clipped/off-surface.
    Hidden,
    /// `expect #x focused`.
    Focused,
    /// `expect no-lints` — the lint pass found nothing.
    NoLints,
}

/// One line of a flow.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Tap(Ref),
    LongPress(Ref),
    Press(Ref),
    Move(Ref),
    Release(Option<Ref>),
    Drag {
        target: Ref,
        dx: f32,
        dy: f32,
        fast: bool,
    },
    Wheel {
        target: Ref,
        notches: f32,
    },
    Type(String),
    Key(KeySpec),
    WaitSettle,
    WaitMs(u64),
    Expect {
        target: Option<Ref>,
        what: Expect,
    },
    Shot(PathBuf),
    Tree(PathBuf),
    /// A raw v1 recording line (`@ms <body>`), kept verbatim: only the runner
    /// can replay a platform-level event, so the in-process harness rejects it
    /// rather than pretending.
    Raw {
        at_ms: u64,
        body: String,
    },
}

/// A parsed flow: its steps with the source line each came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Script {
    pub steps: Vec<Step>,
    /// Source line number of each step, parallel to `steps`.
    pub lines: Vec<usize>,
    /// The original text of each step, for failure messages.
    pub sources: Vec<String>,
    /// Surface size the flow was authored against, if the header said.
    pub size: Option<(u32, u32)>,
    /// Whether any step is a raw v1 event line.
    pub has_raw: bool,
}

impl Script {
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// A parse error, with the line it happened on.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Whether `text` is a v2 flow (as opposed to a v1 recording), by header.
///
/// Leading blank lines and comments are skipped, so a flow can open with the
/// note that says how to run it — which is where such a note belongs.
pub fn is_v2(text: &str) -> bool {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut it = l.split_ascii_whitespace();
            (it.next(), it.next()) == (Some("fbui-rec"), Some("2"))
        })
        .unwrap_or(false)
}

/// One whitespace-separated token, remembering whether it was quoted (so
/// `expect #x checked` and `expect #x "checked"` can be told apart).
#[derive(Debug, Clone, PartialEq)]
struct Tok {
    text: String,
    quoted: bool,
}

/// Split a line into tokens, honouring `"quoted strings"` (with `\"` and
/// `\\`) and stripping a trailing `#` comment outside quotes.
fn tokenize(line: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if c == '"' {
            chars.next();
            let mut s = String::new();
            loop {
                match chars.next() {
                    None => return Err("unterminated quoted string".into()),
                    Some('"') => break,
                    Some('\\') => match chars.next() {
                        Some('n') => s.push('\n'),
                        Some('t') => s.push('\t'),
                        Some(e) => s.push(e),
                        None => return Err("trailing backslash".into()),
                    },
                    Some(c) => s.push(c),
                }
            }
            out.push(Tok {
                text: s,
                quoted: true,
            });
            continue;
        }
        // A `#` starts a comment only when it opens a token *and* the token
        // is not the first one — `tap #inc` must not be read as a comment.
        let mut s = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                break;
            }
            s.push(c);
            chars.next();
        }
        if s.starts_with('#') && !out.is_empty() && !out[0].text.is_empty() && is_comment_pos(&out)
        {
            break;
        }
        if s.starts_with('#') && out.is_empty() {
            break; // a whole-line comment
        }
        out.push(Tok {
            text: s,
            quoted: false,
        });
    }
    Ok(out)
}

/// A `#token` is a comment rather than a name when the verb before it does not
/// take a reference there — in practice, when the line already has its
/// reference. Keeping this narrow means `tap #inc  # the plus button` works.
fn is_comment_pos(so_far: &[Tok]) -> bool {
    match so_far[0].text.as_str() {
        "tap" | "long-press" | "press" | "move" | "release" | "drag" | "wheel" | "expect" => {
            so_far.len() > 1
        }
        _ => true,
    }
}

fn parse_ref(toks: &[Tok], at: &mut usize) -> Result<Ref, String> {
    let t = toks.get(*at).ok_or("expected a widget reference")?;
    *at += 1;
    if let Some(name) = t.text.strip_prefix('#') {
        if name.is_empty() {
            return Err("`#` needs a name".into());
        }
        return Ok(Ref::Name(name.to_string()));
    }
    if let Some(coords) = t.text.strip_prefix('@') {
        let (x, y) = coords
            .split_once(',')
            .ok_or("`@` needs `x,y` coordinates")?;
        let x: f32 = x.trim().parse().map_err(|_| "bad x coordinate")?;
        let y: f32 = y.trim().parse().map_err(|_| "bad y coordinate")?;
        return Ok(Ref::At(Point::new(x, y)));
    }
    if t.quoted {
        return Err(format!(
            "{:?} is a bare string; a reference is #name, Kind \"text\", or @x,y",
            t.text
        ));
    }
    // `Kind` optionally followed by a quoted text.
    let text = match toks.get(*at) {
        Some(next) if next.quoted => {
            *at += 1;
            Some(next.text.clone())
        }
        _ => None,
    };
    Ok(Ref::Kind {
        kind: t.text.clone(),
        text,
    })
}

fn parse_key(spec: &str) -> Result<KeySpec, String> {
    let mut mods = Modifiers::default();
    let mut parts: Vec<&str> = spec.split('+').collect();
    // A lone `+` is the plus character, not an empty modifier list.
    if spec == "+" {
        parts = vec!["+"];
    }
    let name = parts.pop().ok_or("expected a key name")?;
    for m in parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            other => return Err(format!("unknown modifier {other:?}")),
        }
    }
    let key = match name {
        "Enter" | "Return" => Key::Enter,
        "Tab" => Key::Tab,
        "Escape" | "Esc" => Key::Escape,
        "Space" => Key::Space,
        "Backspace" => Key::Backspace,
        "Delete" | "Del" => Key::Delete,
        "Home" => Key::Home,
        "End" => Key::End,
        "Left" => Key::Left,
        "Right" => Key::Right,
        "Up" => Key::Up,
        "Down" => Key::Down,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        other => {
            let mut cs = other.chars();
            match (cs.next(), cs.next()) {
                (Some(c), None) => Key::Char(c),
                _ => return Err(format!("unknown key {other:?}")),
            }
        }
    };
    Ok(KeySpec { key, mods })
}

/// Parse a duration token: `500ms`, `0.5s`, or a bare number of milliseconds.
fn parse_duration_ms(t: &str) -> Result<u64, String> {
    if let Some(v) = t.strip_suffix("ms") {
        return v.trim().parse().map_err(|_| format!("bad duration {t:?}"));
    }
    if let Some(v) = t.strip_suffix('s') {
        let secs: f64 = v
            .trim()
            .parse()
            .map_err(|_| format!("bad duration {t:?}"))?;
        return Ok((secs * 1000.0).round() as u64);
    }
    t.parse().map_err(|_| format!("bad duration {t:?}"))
}

/// The words that name tree state rather than a described property.
const STATE_WORDS: &[&str] = &["absent", "present", "visible", "hidden", "focused"];

fn parse_expect(toks: &[Tok]) -> Result<(Option<Ref>, Expect), String> {
    if toks.len() == 2 && toks[1].text == "no-lints" {
        return Ok((None, Expect::NoLints));
    }
    let mut at = 1;
    let target = parse_ref(toks, &mut at)?;
    let rest = &toks[at..];
    match rest.len() {
        0 => Ok((Some(target), Expect::Exists)),
        1 if STATE_WORDS.contains(&rest[0].text.as_str()) && !rest[0].quoted => {
            let what = match rest[0].text.as_str() {
                "absent" => Expect::Absent,
                "present" => Expect::Exists,
                "visible" => Expect::Visible,
                "hidden" => Expect::Hidden,
                _ => Expect::Focused,
            };
            Ok((Some(target), what))
        }
        // A bare word is a flag: `expect #done checked` means `checked=true`.
        1 => Ok((
            Some(target),
            Expect::Prop {
                key: rest[0].text.clone(),
                value: "true".into(),
            },
        )),
        2 => Ok((
            Some(target),
            Expect::Prop {
                key: rest[0].text.clone(),
                value: rest[1].text.clone(),
            },
        )),
        _ => Err("expect takes a reference and at most `<key> <value>`".into()),
    }
}

/// Parse a v2 flow. A v1 recording body (`@ms …`) may be mixed in; those lines
/// are kept verbatim as [`Step::Raw`].
pub fn parse(text: &str) -> Result<Script, ParseError> {
    let mut script = Script::default();
    let mut seen_header = false;
    for (i, raw) in text.lines().enumerate() {
        let lineno = i + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let err = |m: String| ParseError {
            line: lineno,
            message: m,
        };
        // Header (anywhere before the first step, in practice line 1).
        if !seen_header && line.starts_with("fbui-rec") {
            let mut it = line.split_ascii_whitespace();
            it.next();
            match it.next() {
                Some("2") => {}
                Some(v) => return Err(err(format!("this parser reads fbui-rec 2, not {v:?}"))),
                None => return Err(err("fbui-rec header needs a version".into())),
            }
            script.size = it.next().and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            });
            seen_header = true;
            continue;
        }
        // A raw v1 event line passes through untouched.
        if let Some(rest) = line.strip_prefix('@') {
            // `@x,y` is a reference, not a timestamp — but only a verb line
            // starts with one, so a leading `@` here is always a timestamp.
            let (ms, body) = rest
                .split_once(char::is_whitespace)
                .ok_or_else(|| err("a raw event line is `@<ms> <body>`".into()))?;
            let at_ms = ms
                .parse()
                .map_err(|_| err(format!("bad timestamp {ms:?}")))?;
            script.has_raw = true;
            script.steps.push(Step::Raw {
                at_ms,
                body: body.trim().to_string(),
            });
            script.lines.push(lineno);
            script.sources.push(line.to_string());
            continue;
        }

        let toks = tokenize(line).map_err(err)?;
        if toks.is_empty() {
            continue;
        }
        let mut at = 1;
        let step = match toks[0].text.as_str() {
            "tap" => Step::Tap(parse_ref(&toks, &mut at).map_err(err)?),
            "long-press" => Step::LongPress(parse_ref(&toks, &mut at).map_err(err)?),
            "press" => Step::Press(parse_ref(&toks, &mut at).map_err(err)?),
            "move" => Step::Move(parse_ref(&toks, &mut at).map_err(err)?),
            "release" => {
                if toks.len() > 1 {
                    Step::Release(Some(parse_ref(&toks, &mut at).map_err(err)?))
                } else {
                    Step::Release(None)
                }
            }
            "drag" => {
                let target = parse_ref(&toks, &mut at).map_err(err)?;
                let (mut dx, mut dy, mut fast) = (0.0f32, 0.0f32, false);
                for t in &toks[at..] {
                    let (k, v) = t.text.split_once('=').unwrap_or((t.text.as_str(), ""));
                    match k {
                        "dx" => dx = v.parse().map_err(|_| err(format!("bad dx {v:?}")))?,
                        "dy" => dy = v.parse().map_err(|_| err(format!("bad dy {v:?}")))?,
                        "fast" => fast = v.is_empty() || v == "true",
                        other => return Err(err(format!("unknown drag option {other:?}"))),
                    }
                }
                if dx == 0.0 && dy == 0.0 {
                    return Err(err("drag needs dx= or dy=".into()));
                }
                Step::Drag {
                    target,
                    dx,
                    dy,
                    fast,
                }
            }
            "wheel" => {
                let target = parse_ref(&toks, &mut at).map_err(err)?;
                let n = toks
                    .get(at)
                    .ok_or_else(|| err("wheel needs a notch count".into()))?;
                Step::Wheel {
                    target,
                    notches: n
                        .text
                        .parse()
                        .map_err(|_| err(format!("bad notch count {:?}", n.text)))?,
                }
            }
            "type" => {
                let t = toks
                    .get(1)
                    .ok_or_else(|| err("type needs a string".into()))?;
                Step::Type(t.text.clone())
            }
            "key" => {
                let t = toks.get(1).ok_or_else(|| err("key needs a name".into()))?;
                Step::Key(parse_key(&t.text).map_err(err)?)
            }
            "wait" => {
                let t = toks
                    .get(1)
                    .ok_or_else(|| err("wait needs `settle` or a duration".into()))?;
                if t.text == "settle" {
                    Step::WaitSettle
                } else {
                    Step::WaitMs(parse_duration_ms(&t.text).map_err(err)?)
                }
            }
            "expect" => {
                let (target, what) = parse_expect(&toks).map_err(err)?;
                Step::Expect { target, what }
            }
            "shot" => Step::Shot(PathBuf::from(
                &toks
                    .get(1)
                    .ok_or_else(|| err("shot needs a path".into()))?
                    .text,
            )),
            "tree" => Step::Tree(PathBuf::from(
                &toks
                    .get(1)
                    .ok_or_else(|| err("tree needs a path".into()))?
                    .text,
            )),
            other => return Err(err(format!("unknown step {other:?}"))),
        };
        script.steps.push(step);
        script.lines.push(lineno);
        script.sources.push(line.to_string());
    }
    Ok(script)
}

/// `Kind "text"` references in `script` that match more than one widget in
/// `tree`. Ambiguity is fine while authoring (`tap Button "OK"` finds the one
/// you meant), but in a committed flow the next layout change can flip which
/// match wins — so the lint pass says so.
pub fn ambiguous_refs(script: &Script, tree: &InspectNode) -> Vec<String> {
    let mut out = Vec::new();
    for (i, step) in script.steps.iter().enumerate() {
        let Some(r @ Ref::Kind { .. }) = step_ref(step) else {
            continue;
        };
        let n = matches(tree, r).len();
        if n > 1 {
            out.push(format!(
                "ambiguous-ref: line {}: {} matches {n} widgets; the next layout \
                 change may flip which one wins",
                script.lines[i], r
            ));
        }
    }
    out
}

/// The widget a step acts on, if it names one.
fn step_ref(step: &Step) -> Option<&Ref> {
    match step {
        Step::Tap(r)
        | Step::LongPress(r)
        | Step::Press(r)
        | Step::Move(r)
        | Step::Release(Some(r))
        | Step::Drag { target: r, .. }
        | Step::Wheel { target: r, .. } => Some(r),
        Step::Expect {
            target: Some(r), ..
        } => Some(r),
        _ => None,
    }
}

// ---- execution -------------------------------------------------------------

/// One thing an executor must actually do, with every reference already
/// resolved to logical coordinates. This is the entire contract between the
/// shared meaning of a flow and the three ways of delivering it.
#[derive(Debug, Clone, PartialEq)]
pub enum Act {
    Tap(Point),
    LongPress(Point),
    Press(Point),
    Move(Point),
    /// `None` lifts wherever the pointer already is.
    Release(Option<Point>),
    Drag {
        from: Point,
        dx: f32,
        dy: f32,
        fast: bool,
    },
    Wheel {
        at: Point,
        notches: f32,
    },
    Text(String),
    Key(KeySpec),
    /// Let animations finish (bounded) before continuing.
    WaitSettle,
    WaitMs(u64),
    /// Write a settled screenshot / tree dump.
    Shot(PathBuf),
    Tree(PathBuf),
    /// A raw v1 event line the runner must parse and replay itself.
    Raw {
        at_ms: u64,
        body: String,
    },
}

/// A step that did not hold: the line, its text, and what actually happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    pub line: usize,
    pub source: String,
    pub message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}\n  {}", self.line, self.source, self.message)
    }
}

/// What the executor needs to see of the live UI to resolve and assert.
#[derive(Debug, Clone, Copy)]
pub struct Snapshot<'a> {
    /// The tree as [`Ui::inspect`](crate::Ui::inspect) reports it now.
    pub tree: Option<&'a InspectNode>,
    /// Findings from the lint pass, already rendered (see `expect no-lints`).
    pub lints: &'a [String],
}

impl<'a> Snapshot<'a> {
    pub fn new(tree: Option<&'a InspectNode>) -> Self {
        Snapshot { tree, lints: &[] }
    }

    pub fn with_lints(mut self, lints: &'a [String]) -> Self {
        self.lints = lints;
        self
    }
}

/// Walks a [`Script`], resolving references and evaluating expectations
/// against the live tree, and hands the driver one [`Act`] at a time.
///
/// The executor never touches a clock: pacing (and how long a settle waits)
/// belongs to the driver, which is what lets the same flow run at replay
/// speed on a device and as fast as frames render in a test.
pub struct Executor {
    script: Script,
    next: usize,
    failures: Vec<Failure>,
    /// Expectations evaluated since the last drain, for a trace: the source
    /// line, its text, and whether it held.
    checks: Vec<(usize, String, bool)>,
    /// Stop at the first failure rather than cascading through steps that
    /// were only ever going to fail because an earlier one did.
    stopped: bool,
}

impl Executor {
    pub fn new(script: Script) -> Self {
        Executor {
            script,
            next: 0,
            failures: Vec::new(),
            checks: Vec::new(),
            stopped: false,
        }
    }

    pub fn script(&self) -> &Script {
        &self.script
    }

    /// Every step has run (or the flow stopped at a failure).
    pub fn is_done(&self) -> bool {
        self.stopped || self.next >= self.script.steps.len()
    }

    pub fn failures(&self) -> &[Failure] {
        &self.failures
    }

    /// Expectations evaluated since the last call: `(line, text, held)`. A
    /// driver drains these into its trace, so `FBUI_TRACE` shows each
    /// assertion as it is checked rather than only the one that failed.
    pub fn take_checks(&mut self) -> Vec<(usize, String, bool)> {
        std::mem::take(&mut self.checks)
    }

    /// How far through the flow we are, for progress messages.
    pub fn position(&self) -> (usize, usize) {
        (self.next, self.script.steps.len())
    }

    /// Whether the next step is `expect no-lints`, so a driver can run the
    /// lint pass only when a step actually asks for it rather than on every
    /// step of every flow.
    pub fn wants_lints(&self) -> bool {
        matches!(
            self.script.steps.get(self.next),
            Some(Step::Expect {
                what: Expect::NoLints,
                ..
            })
        )
    }

    /// The next thing the driver must do, consuming steps that need nothing
    /// from it (expectations). `None` means the flow is finished or stopped.
    pub fn advance(&mut self, snap: Snapshot<'_>) -> Option<Act> {
        while !self.is_done() {
            let i = self.next;
            self.next += 1;
            let expectation = matches!(self.script.steps[i], Step::Expect { .. });
            match self.act_for(i, snap) {
                Ok(Some(act)) => return Some(act),
                Ok(None) => {
                    // An expectation that held.
                    self.checks
                        .push((self.script.lines[i], self.script.sources[i].clone(), true));
                    continue;
                }
                Err(f) => {
                    if expectation {
                        self.checks.push((f.line, f.source.clone(), false));
                    }
                    self.failures.push(f);
                    self.stopped = true;
                    return None;
                }
            }
        }
        None
    }

    fn fail(&self, i: usize, message: String) -> Failure {
        Failure {
            line: self.script.lines[i],
            source: self.script.sources[i].clone(),
            message,
        }
    }

    fn act_for(&self, i: usize, snap: Snapshot<'_>) -> Result<Option<Act>, Failure> {
        let step = self.script.steps[i].clone();
        // A step that acts on a widget needs it present *and* on screen: a
        // tap that lands on a scrolled-away row is the classic silent
        // false-pass, so it is a failure here instead.
        let point = |r: &Ref| -> Result<Point, Failure> {
            let t = self.resolve(i, r, snap.tree)?;
            if let Some(n) = t.node() {
                if !n.visible {
                    return Err(self.fail(
                        i,
                        format!("{r} is in the tree but not on screen (clipped or off-surface)"),
                    ));
                }
            }
            Ok(t.point())
        };
        Ok(Some(match step {
            Step::Tap(r) => Act::Tap(point(&r)?),
            Step::LongPress(r) => Act::LongPress(point(&r)?),
            Step::Press(r) => Act::Press(point(&r)?),
            Step::Move(r) => Act::Move(point(&r)?),
            Step::Release(Some(r)) => Act::Release(Some(point(&r)?)),
            Step::Release(None) => Act::Release(None),
            Step::Drag {
                target,
                dx,
                dy,
                fast,
            } => Act::Drag {
                from: point(&target)?,
                dx,
                dy,
                fast,
            },
            Step::Wheel { target, notches } => Act::Wheel {
                at: point(&target)?,
                notches,
            },
            Step::Type(t) => Act::Text(t),
            Step::Key(k) => Act::Key(k),
            Step::WaitSettle => Act::WaitSettle,
            Step::WaitMs(ms) => Act::WaitMs(ms),
            Step::Shot(p) => Act::Shot(p),
            Step::Tree(p) => Act::Tree(p),
            Step::Raw { at_ms, body } => Act::Raw { at_ms, body },
            Step::Expect { target, what } => {
                self.check(i, target.as_ref(), &what, snap)?;
                return Ok(None);
            }
        }))
    }

    fn resolve<'a>(
        &self,
        i: usize,
        r: &Ref,
        tree: Option<&'a InspectNode>,
    ) -> Result<Target<'a>, Failure> {
        if let Ref::At(p) = r {
            return Ok(Target::Point(*p));
        }
        let root =
            tree.ok_or_else(|| self.fail(i, format!("{r} did not resolve: the tree is empty")))?;
        match matches(root, r).first() {
            Some(n) => Ok(Target::Node(n)),
            None => Err(self.fail(i, format!("{r} matches no widget"))),
        }
    }

    fn check(
        &self,
        i: usize,
        target: Option<&Ref>,
        what: &Expect,
        snap: Snapshot<'_>,
    ) -> Result<(), Failure> {
        if let Expect::NoLints = what {
            return if snap.lints.is_empty() {
                Ok(())
            } else {
                Err(self.fail(
                    i,
                    format!(
                        "{} lint finding(s):\n    {}",
                        snap.lints.len(),
                        snap.lints.join("\n    ")
                    ),
                ))
            };
        }
        let r = target.expect("only no-lints has no reference");
        let found = snap.tree.map(|t| matches(t, r)).unwrap_or_default();
        if let Expect::Absent = what {
            return if found.is_empty() {
                Ok(())
            } else {
                Err(self.fail(i, format!("{r} is present ({} match(es))", found.len())))
            };
        }
        let Some(node) = found.first() else {
            return Err(self.fail(i, format!("{r} matches no widget")));
        };
        match what {
            Expect::Exists => Ok(()),
            Expect::Visible if node.visible => Ok(()),
            Expect::Visible => Err(self.fail(i, format!("{r} is present but not on screen"))),
            Expect::Hidden if !node.visible => Ok(()),
            Expect::Hidden => Err(self.fail(i, format!("{r} is on screen"))),
            Expect::Focused if node.focused => Ok(()),
            Expect::Focused => Err(self.fail(i, format!("{r} is not focused"))),
            Expect::Prop { key, value } => match node.prop(key) {
                Some(got) if got == value => Ok(()),
                Some(got) => Err(self.fail(i, format!("{r} {key}={got:?}, expected {value:?}"))),
                None => Err(self.fail(
                    i,
                    format!(
                        "{r} ({}) reports no {key:?}; it reports {}",
                        node.kind,
                        described_keys(node)
                    ),
                )),
            },
            Expect::Absent | Expect::NoLints => unreachable!("handled above"),
        }
    }
}

fn described_keys(n: &InspectNode) -> String {
    let mut keys: Vec<&str> = Vec::new();
    if n.text.is_some() {
        keys.push("text");
    }
    keys.extend(n.props.iter().map(|(k, _)| *k));
    if keys.is_empty() {
        "nothing".to_string()
    } else {
        keys.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steps(text: &str) -> Vec<Step> {
        parse(text).expect("parses").steps
    }

    #[test]
    fn the_header_selects_the_version() {
        assert!(is_v2("fbui-rec 2\ntap #a\n"));
        assert!(is_v2("\n\n  fbui-rec 2 1024x600\n"));
        assert!(is_v2("# how to run this\n\nfbui-rec 2\ntap #a\n"));
        assert!(!is_v2("fbui-rec 1 1024x600\n@0 m 1 1\n"));
        assert!(!is_v2(""));
        assert_eq!(
            parse("fbui-rec 2 800x480\n").unwrap().size,
            Some((800, 480))
        );
        assert!(parse("fbui-rec 1\n").is_err());
    }

    #[test]
    fn actions_parse_with_every_reference_form() {
        assert_eq!(
            steps("fbui-rec 2\ntap #inc\n"),
            vec![Step::Tap(Ref::Name("inc".into()))]
        );
        assert_eq!(
            steps("tap Button \"Submit\"\n"),
            vec![Step::Tap(Ref::Kind {
                kind: "Button".into(),
                text: Some("Submit".into())
            })]
        );
        assert_eq!(
            steps("tap @240,112\n"),
            vec![Step::Tap(Ref::At(Point::new(240.0, 112.0)))]
        );
        assert_eq!(
            steps("tap #form/name\n"),
            vec![Step::Tap(Ref::Name("form/name".into()))]
        );
    }

    #[test]
    fn a_trailing_comment_is_not_a_reference() {
        assert_eq!(
            steps("tap #inc   # the plus button\n"),
            vec![Step::Tap(Ref::Name("inc".into()))]
        );
        assert_eq!(steps("# a whole-line comment\n"), vec![]);
    }

    #[test]
    fn drags_wheels_typing_and_keys_parse() {
        assert_eq!(
            steps("drag #list dy=-200 fast\n"),
            vec![Step::Drag {
                target: Ref::Name("list".into()),
                dx: 0.0,
                dy: -200.0,
                fast: true
            }]
        );
        assert_eq!(
            steps("wheel #list -3\n"),
            vec![Step::Wheel {
                target: Ref::Name("list".into()),
                notches: -3.0
            }]
        );
        assert_eq!(
            steps("type \"milk and eggs\"\n"),
            vec![Step::Type("milk and eggs".into())]
        );
        assert_eq!(
            steps("key Ctrl+C\n"),
            vec![Step::Key(KeySpec {
                key: Key::Char('C'),
                mods: Modifiers {
                    ctrl: true,
                    ..Default::default()
                }
            })]
        );
        assert_eq!(
            steps("key Shift+Tab\n"),
            vec![Step::Key(KeySpec {
                key: Key::Tab,
                mods: Modifiers {
                    shift: true,
                    ..Default::default()
                }
            })]
        );
        assert!(parse("key Hyper+X\n").is_err());
        assert!(parse("drag #a\n").is_err(), "a drag needs a direction");
    }

    #[test]
    fn waits_accept_both_spellings() {
        assert_eq!(steps("wait settle\n"), vec![Step::WaitSettle]);
        assert_eq!(steps("wait 500ms\n"), vec![Step::WaitMs(500)]);
        assert_eq!(steps("wait 0.5s\n"), vec![Step::WaitMs(500)]);
    }

    #[test]
    fn expectations_parse_in_all_their_forms() {
        let ex = |s: &str| match &steps(s)[0] {
            Step::Expect { target, what } => (target.clone(), what.clone()),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            ex("expect #count text \"1 item\"\n"),
            (
                Some(Ref::Name("count".into())),
                Expect::Prop {
                    key: "text".into(),
                    value: "1 item".into()
                }
            )
        );
        assert_eq!(
            ex("expect #done checked\n"),
            (
                Some(Ref::Name("done".into())),
                Expect::Prop {
                    key: "checked".into(),
                    value: "true".into()
                }
            )
        );
        assert_eq!(
            ex("expect #vol value 50\n"),
            (
                Some(Ref::Name("vol".into())),
                Expect::Prop {
                    key: "value".into(),
                    value: "50".into()
                }
            )
        );
        assert_eq!(ex("expect #dialog absent\n").1, Expect::Absent);
        assert_eq!(ex("expect #dialog visible\n").1, Expect::Visible);
        assert_eq!(ex("expect #dialog focused\n").1, Expect::Focused);
        assert_eq!(ex("expect Label \"Thanks, Ann!\"\n").1, Expect::Exists);
        assert_eq!(ex("expect no-lints\n"), (None, Expect::NoLints));
    }

    #[test]
    fn raw_v1_lines_pass_through_untouched() {
        let s = parse("fbui-rec 2\n@120 m 5 5\ntap #a\n").unwrap();
        assert!(s.has_raw);
        assert_eq!(
            s.steps[0],
            Step::Raw {
                at_ms: 120,
                body: "m 5 5".into()
            }
        );
        assert_eq!(s.steps[1], Step::Tap(Ref::Name("a".into())));
    }

    #[test]
    fn errors_name_the_line() {
        let e = parse("fbui-rec 2\ntap #inc\nfrobnicate #x\n").unwrap_err();
        assert_eq!(e.line, 3);
        assert!(e.message.contains("frobnicate"), "{}", e.message);
        let e = parse("type \"unterminated\n").unwrap_err();
        assert!(e.message.contains("unterminated"), "{}", e.message);
    }

    /// `name_at` reports the *deepest* named widget under a point, which is
    /// what "what did the user just touch?" means — a tap inside a named row
    /// inside a named list is the row.
    #[test]
    fn name_at_finds_the_deepest_named_widget() {
        use fbui_render::geom::Rect;
        let leaf = |name: Option<&str>, r: Rect, visible: bool| InspectNode {
            id: "x".into(),
            kind: "Button".into(),
            name: name.map(str::to_string),
            text: None,
            props: Vec::new(),
            bounds: r,
            focusable: false,
            focused: false,
            hovered: false,
            visible,
            overlay: None,
            children: Vec::new(),
        };
        let mut root = leaf(Some("page"), Rect::new(0.0, 0.0, 100.0, 100.0), true);
        let mut list = leaf(Some("list"), Rect::new(0.0, 0.0, 100.0, 50.0), true);
        list.children
            .push(leaf(Some("row"), Rect::new(0.0, 0.0, 100.0, 20.0), true));
        // A clipped child must not answer for a point inside its bounds.
        list.children
            .push(leaf(Some("gone"), Rect::new(0.0, 20.0, 100.0, 20.0), false));
        root.children.push(list);

        assert_eq!(name_at(&root, Point::new(10.0, 10.0)), Some("row"));
        assert_eq!(name_at(&root, Point::new(10.0, 30.0)), Some("list"));
        assert_eq!(name_at(&root, Point::new(10.0, 70.0)), Some("page"));
        assert_eq!(name_at(&root, Point::new(200.0, 10.0)), None);
    }

    #[test]
    fn quoted_strings_keep_spaces_and_escapes() {
        assert_eq!(
            steps("type \"a \\\"b\\\" c\"\n"),
            vec![Step::Type("a \"b\" c".into())]
        );
    }
}

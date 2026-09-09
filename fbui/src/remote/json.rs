//! Hand-rolled JSON *emission* for the remote console (`/tree`) and the
//! Prometheus text format for `/metrics`.
//!
//! Deliberately not a JSON library: the console only ever writes JSON (input
//! injection arrives as query parameters), and the documents are small and
//! fully under our control, so a serializer dependency buys nothing.

use fbui_widgets::InspectNode;

use super::hub::MetricsSnapshot;

/// Escape `s` into a JSON string literal body (no surrounding quotes).
pub(crate) fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The `/tree` document: `{"scale":N,"tree":{...}}`, where each node carries
/// `kind` (the widget type), an optional app-assigned `name`, `id`, `bounds`
/// (`[x,y,w,h]`, logical px), the focus/visibility flags, the widget's own
/// `text` and `props` from [`Widget::describe`](fbui_widgets::Widget::describe),
/// an optional `overlay` rect, and `children`. `scale` converts logical bounds
/// to the device pixels of `/screen.png`. Public so a custom embedder can
/// serve the same document the built-in runner does.
pub fn tree_json(root: &InspectNode, scale: f32) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("{{\"scale\":{scale},\"tree\":"));
    node_json(root, &mut out);
    out.push('}');
    out
}

fn node_json(n: &InspectNode, out: &mut String) {
    out.push_str(&format!(
        "{{\"id\":\"{}\",\"kind\":\"{}\",\"bounds\":[{},{},{},{}],\
         \"focusable\":{},\"focused\":{},\"hovered\":{},\"visible\":{}",
        escape_json(&n.id),
        escape_json(&n.kind),
        n.bounds.x,
        n.bounds.y,
        n.bounds.w,
        n.bounds.h,
        n.focusable,
        n.focused,
        n.hovered,
        n.visible,
    ));
    if let Some(name) = &n.name {
        out.push_str(&format!(",\"name\":\"{}\"", escape_json(name)));
    }
    if let Some(text) = &n.text {
        out.push_str(&format!(",\"text\":\"{}\"", escape_json(text)));
    }
    if !n.props.is_empty() {
        out.push_str(",\"props\":{");
        for (i, (k, v)) in n.props.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!("\"{}\":\"{}\"", escape_json(k), escape_json(v)));
        }
        out.push('}');
    }
    if let Some(o) = n.overlay {
        out.push_str(&format!(",\"overlay\":[{},{},{},{}]", o.x, o.y, o.w, o.h));
    }
    out.push_str(",\"children\":[");
    for (i, c) in n.children.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        node_json(c, out);
    }
    out.push_str("]}");
}

/// The `/metrics` document, in the Prometheus text exposition format — what a
/// fleet's scraper expects.
pub(crate) fn metrics_text(m: &MetricsSnapshot) -> String {
    format!(
        "# HELP fbui_frames_total Frames presented since start.\n\
         # TYPE fbui_frames_total counter\n\
         fbui_frames_total {}\n\
         # HELP fbui_input_events_total Input events delivered since start.\n\
         # TYPE fbui_input_events_total counter\n\
         fbui_input_events_total {}\n\
         # HELP fbui_paint_milliseconds Paint plus copy-out cost of the last frame.\n\
         # TYPE fbui_paint_milliseconds gauge\n\
         fbui_paint_milliseconds {}\n\
         # HELP fbui_paint_milliseconds_max Worst frame since start.\n\
         # TYPE fbui_paint_milliseconds_max gauge\n\
         fbui_paint_milliseconds_max {}\n\
         # HELP fbui_uptime_seconds Seconds since the app started.\n\
         # TYPE fbui_uptime_seconds counter\n\
         fbui_uptime_seconds {:.3}\n\
         # HELP fbui_surface_pixels Surface size in device pixels.\n\
         # TYPE fbui_surface_pixels gauge\n\
         fbui_surface_pixels{{axis=\"width\"}} {}\n\
         fbui_surface_pixels{{axis=\"height\"}} {}\n\
         # HELP fbui_remote_clients Open remote-console connections.\n\
         # TYPE fbui_remote_clients gauge\n\
         fbui_remote_clients {}\n\
         # HELP fbui_remote_watchers Connections watching the frame stream.\n\
         # TYPE fbui_remote_watchers gauge\n\
         fbui_remote_watchers {}\n",
        m.frames,
        m.input_events,
        m.paint_ms_last,
        m.paint_ms_max,
        m.uptime_s,
        m.width,
        m.height,
        m.clients,
        m.watchers,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use fbui_render::geom::Rect;

    fn leaf(kind: &str) -> InspectNode {
        InspectNode {
            id: "WidgetId(1v1)".into(),
            kind: kind.into(),
            name: None,
            text: None,
            props: Vec::new(),
            bounds: Rect::new(1.0, 2.0, 3.0, 4.0),
            focusable: true,
            focused: false,
            hovered: false,
            visible: true,
            overlay: None,
            children: Vec::new(),
        }
    }

    #[test]
    fn escapes_json_specials() {
        assert_eq!(escape_json("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
        assert_eq!(escape_json("\u{1}"), "\\u0001");
    }

    #[test]
    fn tree_json_shape() {
        let mut root = leaf("Container");
        root.overlay = Some(Rect::new(5.0, 6.0, 7.0, 8.0));
        root.children.push(leaf("Button"));
        root.children.push(leaf("Label"));
        let j = tree_json(&root, 2.0);
        assert!(j.starts_with("{\"scale\":2,\"tree\":{"), "{j}");
        assert!(j.contains("\"kind\":\"Container\""));
        assert!(j.contains("\"overlay\":[5,6,7,8]"));
        assert!(j.contains("\"bounds\":[1,2,3,4]"));
        assert!(j.contains("\"visible\":true"));
        // Two children, comma-separated.
        assert!(j.contains("\"kind\":\"Button\"") && j.contains("\"kind\":\"Label\""));
        assert_eq!(j.matches("\"children\":[").count(), 3);
    }

    /// A named, described widget carries its name, its text and its props —
    /// the whole point of the `describe` pass reaching the console.
    #[test]
    fn tree_json_carries_names_text_and_props() {
        let mut n = leaf("Checkbox");
        n.name = Some("agree".into());
        n.text = Some("I \"agree\"".into());
        n.props = vec![("checked", "true".into())];
        n.visible = false;
        let j = tree_json(&n, 1.0);
        assert!(j.contains("\"name\":\"agree\""), "{j}");
        assert!(j.contains("\"text\":\"I \\\"agree\\\"\""), "{j}");
        assert!(j.contains("\"props\":{\"checked\":\"true\"}"), "{j}");
        assert!(j.contains("\"visible\":false"), "{j}");
    }

    /// The reader and the writer must agree: whatever `tree_json` emits,
    /// `parse_tree` must reconstruct — that is what makes `fbui-ctl` resolve
    /// a flow against a live device the same way the runner would.
    #[test]
    fn a_tree_document_round_trips() {
        let mut root = leaf("Container");
        root.name = Some("page".into());
        root.props = vec![("direction", "column".into())];
        root.overlay = Some(Rect::new(5.0, 6.0, 7.0, 8.0));

        let mut child = leaf("Checkbox");
        child.name = Some("agree".into());
        child.text = Some("I \"agree\" — now\n".into());
        child.props = vec![("checked", "true".into())];
        child.visible = false;
        child.focused = true;
        root.children.push(child);
        root.children.push(leaf("Label"));

        let doc = tree_json(&root, 1.0);
        let back = parse_tree(&doc).expect("parses");
        assert_eq!(format!("{back:?}"), format!("{root:?}"));
    }

    #[test]
    fn a_malformed_document_is_none_not_a_panic() {
        assert!(parse_tree("").is_none());
        assert!(parse_tree("{\"scale\":1}").is_none(), "no tree");
        assert!(
            parse_tree("{\"scale\":1,\"tree\":{}}").is_none(),
            "no bounds"
        );
        assert!(parse_tree("{\"tree\":[1,2").is_none());
    }

    #[test]
    fn metrics_text_is_prometheus_shaped() {
        let m = MetricsSnapshot {
            frames: 7,
            paint_ms_last: 1.5,
            ..Default::default()
        };
        let t = metrics_text(&m);
        assert!(t.contains("fbui_frames_total 7\n"));
        assert!(t.contains("fbui_paint_milliseconds 1.5\n"));
        assert!(t.contains("# TYPE fbui_frames_total counter\n"));
    }
}

// ---- parsing back --------------------------------------------------------
//
// The console only ever *writes* JSON, with one exception: `fbui-ctl` reads
// `/tree` to resolve a flow's `#name` and `Kind "text"` references against the
// live device. That needs the document turned back into `InspectNode`s, so
// here is a minimal reader for exactly the document `tree_json` writes — no
// dependency, and a round-trip test keeps the two halves honest.

/// A parsed JSON value. Only what the tree document contains.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    fn as_f32(&self) -> Option<f32> {
        match self {
            Json::Num(n) => Some(*n as f32),
            _ => None,
        }
    }

    fn as_bool(&self) -> bool {
        matches!(self, Json::Bool(true))
    }

    fn as_arr(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            _ => &[],
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] as char).is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn value(&mut self) -> Option<Json> {
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.eat(b'}').is_some() {
                    return Some(Json::Obj(out));
                }
                loop {
                    self.ws();
                    let Json::Str(k) = self.string()? else {
                        return None;
                    };
                    self.eat(b':')?;
                    out.push((k, self.value()?));
                    self.ws();
                    if self.eat(b',').is_some() {
                        continue;
                    }
                    self.eat(b'}')?;
                    return Some(Json::Obj(out));
                }
            }
            b'[' => {
                self.i += 1;
                let mut out = Vec::new();
                if self.eat(b']').is_some() {
                    return Some(Json::Arr(out));
                }
                loop {
                    out.push(self.value()?);
                    self.ws();
                    if self.eat(b',').is_some() {
                        continue;
                    }
                    self.eat(b']')?;
                    return Some(Json::Arr(out));
                }
            }
            b'"' => self.string(),
            b't' => self.literal("true", Json::Bool(true)),
            b'f' => self.literal("false", Json::Bool(false)),
            b'n' => self.literal("null", Json::Null),
            _ => self.number(),
        }
    }

    fn literal(&mut self, word: &str, v: Json) -> Option<Json> {
        if self.b[self.i..].starts_with(word.as_bytes()) {
            self.i += word.len();
            Some(v)
        } else {
            None
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.i;
        while self
            .b
            .get(self.i)
            .is_some_and(|c| matches!(c, b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'))
        {
            self.i += 1;
        }
        std::str::from_utf8(&self.b[start..self.i])
            .ok()?
            .parse()
            .ok()
            .map(Json::Num)
    }

    fn string(&mut self) -> Option<Json> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return Some(Json::Str(out)),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    match e {
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
                        }
                        other => out.push(other as char),
                    }
                }
                // Multi-byte UTF-8 passes through byte by byte; the document
                // is valid UTF-8, so re-decoding at the end would work too,
                // but this keeps escapes and raw text in one pass.
                _ => {
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    self.i = start + len;
                    out.push_str(std::str::from_utf8(self.b.get(start..self.i)?).ok()?);
                }
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Parse the `/tree` document back into an [`InspectNode`], or `None` when it
/// is malformed or has no tree. This is what lets `fbui-ctl` resolve a flow's
/// references against a live device.
pub fn parse_tree(doc: &str) -> Option<InspectNode> {
    let v = Parser {
        b: doc.as_bytes(),
        i: 0,
    }
    .value()?;
    node_from(v.get("tree")?)
}

fn node_from(v: &Json) -> Option<InspectNode> {
    let b = v.get("bounds")?.as_arr();
    if b.len() != 4 {
        return None;
    }
    let props = match v.get("props") {
        Some(Json::Obj(pairs)) => pairs
            .iter()
            .filter_map(|(k, val)| {
                // Keys are `&'static str` in the node, and these came off the
                // wire — leak-free by interning the handful of known keys and
                // falling back to a leak for anything a newer server sent.
                Some((intern(k), val.as_str()?.to_string()))
            })
            .collect(),
        _ => Vec::new(),
    };
    Some(InspectNode {
        id: v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        kind: v.get("kind").and_then(|x| x.as_str())?.to_string(),
        name: v.get("name").and_then(|x| x.as_str()).map(str::to_string),
        text: v.get("text").and_then(|x| x.as_str()).map(str::to_string),
        props,
        bounds: fbui_render::geom::Rect::new(
            b[0].as_f32()?,
            b[1].as_f32()?,
            b[2].as_f32()?,
            b[3].as_f32()?,
        ),
        focusable: v.get("focusable").is_some_and(|x| x.as_bool()),
        focused: v.get("focused").is_some_and(|x| x.as_bool()),
        hovered: v.get("hovered").is_some_and(|x| x.as_bool()),
        // Older servers did not send `visible`; assume on screen rather than
        // failing every reference against them.
        visible: v.get("visible").map(|x| x.as_bool()).unwrap_or(true),
        overlay: v.get("overlay").and_then(|o| {
            let r = o.as_arr();
            (r.len() == 4).then(|| {
                fbui_render::geom::Rect::new(
                    r[0].as_f32().unwrap_or(0.0),
                    r[1].as_f32().unwrap_or(0.0),
                    r[2].as_f32().unwrap_or(0.0),
                    r[3].as_f32().unwrap_or(0.0),
                )
            })
        }),
        children: v
            .get("children")
            .map(|c| c.as_arr().iter().filter_map(node_from).collect())
            .unwrap_or_default(),
    })
}

/// `InspectNode::props` keys are `&'static str`. Every key a widget in this
/// workspace reports is known here; anything else came from a newer server, so
/// it is leaked once — bounded by the number of distinct unknown keys, which
/// in practice is zero.
fn intern(k: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "value",
        "checked",
        "on",
        "open",
        "selected",
        "options",
        "tabs",
        "rows",
        "cursor",
        "selection",
        "placeholder",
        "offset",
        "content",
        "viewport",
        "first_visible",
        "direction",
        "gap",
        "padding",
        "variant",
        "pressed",
        "wrap",
        "bold",
        "min",
        "max",
        "percent",
        "shown",
        "running",
        "modal",
        "dismiss_on_scrim",
        "depth",
        "top",
        "transitioning",
        "count",
        "source",
        "frame",
        "fit",
        "series",
        "samples",
        "range",
        "date",
        "view",
        "layer",
        "items",
    ];
    KNOWN
        .iter()
        .find(|k2| **k2 == k)
        .copied()
        .unwrap_or_else(|| Box::leak(k.to_string().into_boxed_str()))
}

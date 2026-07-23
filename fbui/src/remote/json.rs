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
/// `id`, `name`, `bounds` (`[x,y,w,h]`, logical px), the focus flags, an
/// optional `overlay` rect, and `children`. `scale` converts logical bounds to
/// the device pixels of `/screen.png`. Public so a custom embedder can serve
/// the same document the built-in runner does.
pub fn tree_json(root: &InspectNode, scale: f32) -> String {
    let mut out = String::with_capacity(1024);
    out.push_str(&format!("{{\"scale\":{scale},\"tree\":"));
    node_json(root, &mut out);
    out.push('}');
    out
}

fn node_json(n: &InspectNode, out: &mut String) {
    out.push_str(&format!(
        "{{\"id\":\"{}\",\"name\":\"{}\",\"bounds\":[{},{},{},{}],\
         \"focusable\":{},\"focused\":{},\"hovered\":{}",
        escape_json(&n.id),
        escape_json(&n.name),
        n.bounds.x,
        n.bounds.y,
        n.bounds.w,
        n.bounds.h,
        n.focusable,
        n.focused,
        n.hovered,
    ));
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

    fn leaf(name: &str) -> InspectNode {
        InspectNode {
            id: "WidgetId(1v1)".into(),
            name: name.into(),
            bounds: Rect::new(1.0, 2.0, 3.0, 4.0),
            focusable: true,
            focused: false,
            hovered: false,
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
        assert!(j.contains("\"name\":\"Container\""));
        assert!(j.contains("\"overlay\":[5,6,7,8]"));
        assert!(j.contains("\"bounds\":[1,2,3,4]"));
        // Two children, comma-separated.
        assert!(j.contains("\"name\":\"Button\"") && j.contains("\"name\":\"Label\""));
        assert_eq!(j.matches("\"children\":[").count(), 3);
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

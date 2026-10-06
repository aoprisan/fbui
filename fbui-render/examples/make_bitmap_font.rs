//! Rasterize a TTF/OTF into `.fbf` bitmap fonts (see `fbui_render::text::bitmap`).
//!
//! ```sh
//! cargo run -p fbui-render --example make_bitmap_font -- \
//!     fbui-render/fonts/Inter-Regular.ttf fbui-render/fonts/Inter 12,16,20,24
//! # writes fbui-render/fonts/Inter-12.fbf … Inter-24.fbf
//! ```
//!
//! Options (after the three positional arguments):
//!
//! * `--chars ascii|latin1` — the base set (default `latin1`: printable ASCII,
//!   U+00A0–U+00FF, and common punctuation and arrows);
//! * `--extra "…"` — more characters to include;
//! * `--family NAME`, `--bold`, `--italic` — what the font says about itself
//!   (the family defaults to the font's own name).
//!
//! Each glyph is shaped and rasterized exactly as fbui's outline text path
//! does it (cosmic-text + swash, unhinted, pen at a whole pixel), so text set
//! in the result sits where outline text would, minus kerning.

use std::sync::Arc;

use cosmic_text::{fontdb, Attrs, Buffer, FontSystem, Metrics, Shaping, SwashCache, SwashContent};
use fbui_render::text::BitmapFontWriter;

/// Beyond Latin-1: what a UI commonly prints.
const PUNCTUATION: &str = "–—‘’‚“”„†•…‰‹›€™←↑→↓↔✓−≤≥≈∞";

fn main() {
    if let Err(e) = run() {
        eprintln!("make_bitmap_font: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        return Err(
            "usage: make_bitmap_font FONT.ttf OUT_PREFIX PX[,PX...] [--chars ascii|latin1] \
                    [--extra CHARS] [--family NAME] [--bold] [--italic]"
                .into(),
        );
    }
    let (font_path, prefix) = (&args[0], &args[1]);
    let sizes: Vec<f32> = args[2]
        .split(',')
        .map(|s| s.trim().parse().map_err(|_| format!("bad size {s:?}")))
        .collect::<Result<_, _>>()?;
    let mut chars = String::new();
    let mut base = "latin1".to_string();
    let (mut family, mut bold, mut italic) = (None, false, false);
    let mut it = args[3..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--chars" => base = it.next().ok_or("--chars needs a value")?.clone(),
            "--extra" => chars.push_str(it.next().ok_or("--extra needs a value")?),
            "--family" => family = Some(it.next().ok_or("--family needs a value")?.clone()),
            "--bold" => bold = true,
            "--italic" => italic = true,
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    let mut set: Vec<char> = (' '..='~').collect();
    match base.as_str() {
        "ascii" => {}
        "latin1" => {
            set.extend('\u{a0}'..='\u{ff}');
            set.extend(PUNCTUATION.chars());
        }
        other => return Err(format!("unknown --chars {other:?} (ascii or latin1)")),
    }
    set.extend(chars.chars());
    set.sort_unstable();
    set.dedup();

    let data = std::fs::read(font_path).map_err(|e| format!("{font_path}: {e}"))?;
    let mut db = fontdb::Database::new();
    db.load_font_source(fontdb::Source::Binary(Arc::new(data)));
    let face = db.faces().next().ok_or("no font face in the file")?;
    let name = face
        .families
        .first()
        .map(|f| f.0.clone())
        .unwrap_or_default();
    db.set_sans_serif_family(name.clone());
    let family = family.unwrap_or(name);
    let mut fs = FontSystem::new_with_locale_and_db("en-US".into(), db);
    let mut swash = SwashCache::new();

    for px in sizes {
        let (ascent, descent) = metrics(&mut fs, px);
        let mut w = BitmapFontWriter::new(&family, px, ascent, descent)
            .bold(bold)
            .italic(italic);
        let mut missing = Vec::new();
        for &ch in &set {
            let mut buf = Buffer::new(&mut fs, Metrics::new(px, px * 1.25));
            let mut s = [0u8; 4];
            buf.set_text(
                ch.encode_utf8(&mut s),
                &Attrs::new(),
                Shaping::Advanced,
                None,
            );
            buf.shape_until_scroll(&mut fs, false);
            let Some(run) = buf.layout_runs().next() else {
                continue;
            };
            let [g] = run.glyphs else {
                // A character that shapes to several glyphs (or none) has no
                // single-glyph form; skip it.
                missing.push(ch);
                continue;
            };
            if g.glyph_id == 0 && ch != ' ' {
                missing.push(ch);
                continue;
            }
            let phys = g.physical((0.0, 0.0), 1.0);
            match swash.get_image_uncached(&mut fs, phys.cache_key) {
                Some(img) if img.content == SwashContent::Mask => {
                    let p = img.placement;
                    w.glyph(
                        ch,
                        g.w,
                        (p.left + phys.x) as i16,
                        (p.top - phys.y) as i16,
                        p.width as u16,
                        p.height as u16,
                        &img.data,
                    );
                }
                // Blank glyphs (space) and anything without a coverage mask.
                _ => {
                    w.glyph(ch, g.w, 0, 0, 0, 0, &[]);
                }
            }
        }
        let bytes = w.finish();
        let out = format!("{prefix}-{px}.fbf");
        std::fs::write(&out, &bytes).map_err(|e| format!("{out}: {e}"))?;
        eprintln!(
            "{out}: {} glyphs, {} bytes{}",
            set.len() - missing.len(),
            bytes.len(),
            if missing.is_empty() {
                String::new()
            } else {
                format!(", not in the font: {}", missing.iter().collect::<String>())
            }
        );
    }
    Ok(())
}

/// The face's ascent and descent at `px`, as cosmic-text lays lines out.
fn metrics(fs: &mut FontSystem, px: f32) -> (f32, f32) {
    let mut buf = Buffer::new(fs, Metrics::new(px, px * 1.25));
    buf.set_text("Hg", &Attrs::new(), Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);
    buf.lines
        .first()
        .and_then(|l| l.layout_opt())
        .and_then(|ls| ls.first())
        .map_or((px * 0.8, px * 0.2), |l| (l.max_ascent, l.max_descent))
}

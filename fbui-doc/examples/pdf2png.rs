//! Render a PDF page (or decode a PNG/JPEG) to a PNG on the host — the
//! quickest way to see what the no_std renderer makes of a file.
//!
//! ```sh
//! cargo run -p fbui-doc --features std --example pdf2png -- in.pdf out.png [page] [zoom]
//! ```
//!
//! Non-embedded fonts render with fbui-render's bundled Inter.

use fbui_doc::pdf::{Document, RenderOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: pdf2png <in.pdf|png|jpg> <out.png> [page=1] [zoom=1.5]");
        std::process::exit(2);
    }
    let bytes = std::fs::read(&args[1])?;
    let page: usize = args.get(3).map_or(Ok(1), |s| s.parse())?;
    let zoom: f32 = args.get(4).map_or(Ok(1.5), |s| s.parse())?;
    let t0 = std::time::Instant::now();
    let pixmap = match fbui_doc::sniff(&bytes) {
        fbui_doc::Format::Pdf => {
            let mut doc = Document::parse(bytes)?;
            doc.set_fallback_font(&include_bytes!("../../fbui-render/fonts/Inter-Regular.ttf")[..]);
            eprintln!("{} pages, title {:?}", doc.page_count(), doc.title());
            doc.render_page(
                page.saturating_sub(1),
                &RenderOptions {
                    zoom,
                    ..Default::default()
                },
            )?
        }
        _ => fbui_doc::raster::decode(&bytes, 1 << 26)?,
    };
    eprintln!(
        "rendered {}x{} in {:?}",
        pixmap.width(),
        pixmap.height(),
        t0.elapsed()
    );
    let mut rgba = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
    }
    image::save_buffer(
        &args[2],
        &rgba,
        pixmap.width(),
        pixmap.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(())
}

//! Headless example: render the sample settings page to a PNG.
//!
//! No device, no platform feature — proof that the render layer stands alone.
//! Run it anywhere:
//!
//! ```text
//! cargo run -p fbui-render --example settings_png -- /tmp/settings.png
//! ```
//!
//! It also demonstrates fractional HiDPI: pass a scale as the second argument to
//! render the same logical layout at, say, 2× device resolution.

use fbui_render::sample::settings_page;
use fbui_render::{FontContext, Scale, Surface};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "settings.png".to_string());
    let scale_factor: f32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1.0);

    // Logical layout size; the device surface is this times the scale factor.
    let (lw, lh) = (480.0f32, 360.0f32);
    let scale = Scale::new(scale_factor);
    let (dw, dh) = (
        (lw * scale.factor()).round() as u32,
        (lh * scale.factor()).round() as u32,
    );

    let mut fonts = FontContext::new();
    let mut surface = Surface::new(dw, dh, scale);
    surface.paint(|p| settings_page(p, &mut fonts, lw, lh, None));

    surface
        .pixmap()
        .save_png(&path)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
    eprintln!("wrote {path} ({dw}x{dh}, scale {})", scale.factor());
}

//! A memory-budget gate for small targets: a 320×240 RGB565 UI (the
//! minimal widget set, one font) must stay inside a heap an MCU can give it.
//! Measured with a counting allocator — the numbers NOSTD.md quotes come from
//! here. One test, measuring each configuration in turn: the allocator
//! counts the whole process, so parallel tests would pollute each other.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use fbui_bare::{App, FbInfo, Framebuffer, Input, Runner};
use fbui_render::TargetFormat;
use fbui_widgets::event::Key;
use fbui_widgets::widgets::{Button, Container, Label, ProgressBar};
use fbui_widgets::Ui;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let n = LIVE.fetch_add(l.size(), Relaxed) + l.size();
        PEAK.fetch_max(n, Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Relaxed);
        System.dealloc(p, l)
    }
}

#[global_allocator]
static A: Counting = Counting;

const FONT: &[u8] = include_bytes!("../../fbui-render/fonts/Inter-Regular.ttf");
const FBF: [&[u8]; 4] = [
    include_bytes!("../../fbui-render/fonts/Inter-12.fbf"),
    include_bytes!("../../fbui-render/fonts/Inter-16.fbf"),
    include_bytes!("../../fbui-render/fonts/Inter-20.fbf"),
    include_bytes!("../../fbui-render/fonts/Inter-24.fbf"),
];

/// How the counter gets its font.
#[derive(Clone, Copy, PartialEq)]
enum Fonts {
    /// The TTF copied into the heap.
    Owned,
    /// The TTF used in place.
    Static,
    /// Pre-rasterized bitmap fonts: no shaping engine at all.
    Bitmap,
}

#[derive(Clone)]
enum Msg {
    Inc,
}

struct Counter {
    n: u32,
    label: Option<fbui_widgets::WidgetId>,
    fonts: Fonts,
}

impl App for Counter {
    type Message = Msg;
    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(Container::column().fill().padding(12.0).gap(8.0));
        self.label = Some(ui.add_child(root, Label::new("Count: 0").size(20.0)));
        ui.add_child(root, ProgressBar::new(0.3));
        let b = ui.add_child(root, Button::new("Increment").on_press(|| Msg::Inc));
        ui.focus(Some(b));
    }
    fn update(&mut self, _: Msg, ui: &mut Ui<Msg>) {
        self.n += 1;
        let t = format!("Count: {}", self.n);
        if let Some(id) = self.label {
            ui.with::<Label, _>(id, move |l| l.set_text(t));
        }
    }
    fn static_fonts(&self) -> Vec<&'static [u8]> {
        match self.fonts {
            Fonts::Static => vec![FONT],
            _ => Vec::new(),
        }
    }
    fn fonts(&self) -> Vec<Vec<u8>> {
        match self.fonts {
            Fonts::Owned => vec![FONT.to_vec()],
            _ => Vec::new(),
        }
    }
    fn bitmap_fonts(&self) -> Vec<fbui_render::text::BitmapFont> {
        match self.fonts {
            Fonts::Bitmap => FBF
                .iter()
                .map(|f| fbui_render::text::BitmapFont::from_bytes(f).unwrap())
                .collect(),
            _ => Vec::new(),
        }
    }
}

struct Panel(Vec<u8>);
impl Framebuffer for Panel {
    fn info(&self) -> FbInfo {
        FbInfo {
            width: 320,
            height: 240,
            stride: 640,
            format: TargetFormat::Rgb565,
        }
    }
    fn pixels(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

struct Footprint {
    live_kib: usize,
    peak_kib: usize,
    screen: Vec<u8>,
}

/// Build the counter, press its button three times, and report the heap the
/// runner used (the framebuffer itself is the board's, not counted).
fn measure(fonts: Fonts, band_rows: Option<u32>) -> Footprint {
    let mut fb = Panel(vec![0; 640 * 240]);
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let app = Counter {
        n: 0,
        label: None,
        fonts,
    };
    let mut r = match band_rows {
        Some(rows) => Runner::new_banded(app, fb.info(), 1.0, rows),
        None => Runner::new(app, fb.info(), 1.0),
    };
    let mut now = 0;
    for _ in 0..3 {
        r.frame(&mut fb, now);
        now += 16;
        r.handle(Input::KeyTap(Key::Enter), now);
    }
    r.frame(&mut fb, now + 16);
    assert_eq!(r.app().n, 3, "the button worked");
    let f = Footprint {
        live_kib: (LIVE.load(Relaxed) - base) / 1024,
        peak_kib: (PEAK.load(Relaxed) - base) / 1024,
        screen: fb.0.clone(),
    };
    drop(r);
    f
}

#[test]
fn a_small_ui_fits_a_small_heap() {
    // The baseline: the font copied into the heap (~300 KiB) and a
    // whole-screen 4-byte shadow (300 KiB).
    let owned = measure(Fonts::Owned, None);
    eprintln!(
        "320x240, heap font, whole-screen shadow: live {} KiB, peak {} KiB",
        owned.live_kib, owned.peak_kib
    );
    assert!(
        owned.peak_kib < 2 * 1024,
        "peak {} KiB over 2 MiB",
        owned.peak_kib
    );

    // The font used in place: the copy is gone, the pixels are the same.
    let fixed = measure(Fonts::Static, None);
    eprintln!(
        "320x240, static font, whole-screen shadow: live {} KiB, peak {} KiB",
        fixed.live_kib, fixed.peak_kib
    );
    assert!(
        fixed.peak_kib + 250 < owned.peak_kib,
        "the font copy is gone"
    );
    assert!(fixed.screen == owned.screen);

    // And painted through a 16-row band: the small-MCU configuration.
    let banded = measure(Fonts::Static, Some(16));
    eprintln!(
        "320x240, static font, 16-row band: live {} KiB, peak {} KiB",
        banded.live_kib, banded.peak_kib
    );
    assert!(
        banded.peak_kib < 300,
        "banded peak {} KiB over the 300 KiB budget",
        banded.peak_kib
    );
    assert!(banded.screen == owned.screen, "banding changed the pixels");

    // Bitmap fonts: no shaping engine, glyph rasterizer or glyph cache.
    let bitmap = measure(Fonts::Bitmap, Some(16));
    eprintln!(
        "320x240, bitmap fonts, 16-row band: live {} KiB, peak {} KiB",
        bitmap.live_kib, bitmap.peak_kib
    );
    // Measured: 45 KiB live, 97 KiB peak (the transient is the first,
    // full-screen frame).
    assert!(
        bitmap.peak_kib < 128,
        "bitmap-font peak {} KiB over the 128 KiB budget",
        bitmap.peak_kib
    );
    let lit = |s: &[u8]| s.chunks_exact(2).filter(|p| p != &[0, 0]).count();
    let (a, b) = (lit(&bitmap.screen), lit(&banded.screen));
    assert!(
        a.abs_diff(b) * 20 < b,
        "the same UI, drawn: {a} vs {b} lit pixels"
    );
}

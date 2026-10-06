//! A memory-budget gate for small targets: a 320×240 RGB565 UI (the
//! minimal widget set, one font) must stay inside a heap an MCU with
//! external RAM can give it. Measured with a counting allocator — the
//! numbers NOSTD.md quotes come from here.

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

#[derive(Clone)]
enum Msg {
    Inc,
}

struct Counter {
    n: u32,
    label: Option<fbui_widgets::WidgetId>,
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
    fn fonts(&self) -> Vec<Vec<u8>> {
        vec![FONT.to_vec()]
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

#[test]
fn a_small_ui_fits_a_small_heap() {
    let mut fb = Panel(vec![0; 640 * 240]);
    let base = LIVE.load(Relaxed);
    PEAK.store(base, Relaxed);
    let mut r = Runner::new(Counter { n: 0, label: None }, fb.info(), 1.0);
    let mut now = 0;
    for _ in 0..3 {
        r.frame(&mut fb, now);
        now += 16;
        r.handle(Input::KeyTap(Key::Enter), now);
    }
    r.frame(&mut fb, now + 16);
    assert_eq!(r.app().n, 3, "the button worked");
    let live = LIVE.load(Relaxed) - base;
    let peak = PEAK.load(Relaxed) - base;
    eprintln!(
        "320x240 UI: live {} KiB, peak {} KiB",
        live / 1024,
        peak / 1024
    );
    // The font (~300 KiB) is copied into the heap by `App::fonts`; the shadow
    // surface is 300 KiB; the rest is cosmic-text, layout and the tree.
    assert!(
        peak < 2 * 1024 * 1024,
        "peak heap {} KiB over the 2 MiB budget",
        peak / 1024
    );
}

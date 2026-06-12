//! Scrolling cost: the Phase 5 perf gate (PLAN §4).
//!
//! Two ways to advance a long [`List`] one scroll step, on the same warm shadow:
//!
//! * **`scroll_full_repaint`** — re-rasterize every visible row (what a scroll
//!   cost before Phase 5: damage the whole viewport, shape and draw each row).
//! * **`scroll_blit`** — the fast path: shift the already-drawn rows in place and
//!   re-rasterize only the one row band that scrolled into view.
//!
//! The property we protect against regression is the *ratio*: `scroll_blit`
//! should be markedly cheaper, because its work is proportional to the exposed
//! strip, not the viewport. The absolute numbers are only a "Pi gate" on
//! Pi-class hardware.

use criterion::{criterion_group, criterion_main, Criterion};
use fbui_render::geom::{Point, Size};
use fbui_render::{Scale, Surface, TargetFormat};
use fbui_widgets::event::Event;
use fbui_widgets::widgets::{Container, List};
use fbui_widgets::{Theme, Ui, WidgetId};

const W: u32 = 480;
const H: u32 = 800;

#[derive(Clone)]
enum Msg {}

/// A list filling an 480×800 surface, painted once so the shadow is warm and the
/// benchmark measures only the incremental scroll.
fn warm_list() -> (Ui<Msg>, WidgetId, Surface, Vec<u8>) {
    let mut ui = Ui::<Msg>::new(Size::new(W as f32, H as f32), Scale::ONE, Theme::dark());
    let root = ui.set_root(Container::column().fill());
    let rows: Vec<String> = (0..5000).map(|i| format!("Contact {i:04}")).collect();
    let list = ui.add_child(root, List::new(rows));
    ui.layout_now();

    let mut surface = Surface::new(W, H, Scale::ONE);
    let stride = (W * 4) as usize;
    let mut scanout = vec![0u8; stride * H as usize];
    ui.paint(&mut surface);
    let _ = surface.present_to_buffer(&mut scanout, stride, TargetFormat::Xrgb8888, 0);
    (ui, list, surface, scanout)
}

fn wheel(ui: &mut Ui<Msg>, list: WidgetId, dy: f32) {
    let b = ui.bounds(list).unwrap();
    ui.event(Event::Scroll {
        pos: Point::new(b.x + 10.0, b.y + 10.0),
        delta_x: 0.0,
        delta_y: dy,
    });
}

fn bench_scroll(c: &mut Criterion) {
    let stride = (W * 4) as usize;

    c.bench_function("scroll_full_repaint", |b| {
        let (mut ui, list, mut surface, mut scanout) = warm_list();
        let mut dir = 8.0f32;
        b.iter(|| {
            // Move a little, then mark the whole list dirty so every visible row
            // is re-rasterized (the pre-Phase-5 scroll cost).
            wheel(&mut ui, list, dir);
            ui.request_paint(list);
            ui.paint(&mut surface);
            let d = surface.present_to_buffer(&mut scanout, stride, TargetFormat::Xrgb8888, 1);
            std::hint::black_box(&d);
            // Ping-pong so we stay mid-list and always actually move.
            dir = -dir;
        });
    });

    c.bench_function("scroll_blit", |b| {
        let (mut ui, list, mut surface, mut scanout) = warm_list();
        let mut dir = 8.0f32;
        b.iter(|| {
            // Same step, but let the scroll-blit fast path shift the rows and
            // repaint only the exposed strip.
            wheel(&mut ui, list, dir);
            ui.paint(&mut surface);
            let d = surface.present_to_buffer(&mut scanout, stride, TargetFormat::Xrgb8888, 1);
            std::hint::black_box(&d);
            dir = -dir;
        });
    });
}

criterion_group!(benches, bench_scroll);
criterion_main!(benches);

//! Repaint benchmarks for the Phase 2 perf gate (PLAN §4).
//!
//! Two cases, the two that matter for a CPU renderer on weak hardware:
//!
//! * **full frame at 1080p** — the worst case (resize, theme switch, resume):
//!   paint the whole settings page and copy it all out. Gate: < 16 ms.
//! * **small damage** — the common case (a toggle flips): the static page is
//!   already in the shadow; repaint one row's toggle and copy out only that span.
//!   Gate: < 5 ms on a Pi-class CPU.
//!
//! These run on whatever host CI provides; the absolute numbers are only a "Pi"
//! gate on Pi-class hardware, but the relative cost (small-damage ≪ full-frame)
//! is the property we're protecting against regression.

use criterion::{criterion_group, criterion_main, Criterion};
use fbui_render::geom::Rect;
use fbui_render::sample::settings_page;
use fbui_render::{Color, FontContext, Scale, Surface, TargetFormat};

fn bench_full_frame(c: &mut Criterion) {
    let (w, h) = (1920u32, 1080u32);
    let mut fonts = FontContext::new();
    let mut surface = Surface::new(w, h, Scale::ONE);
    let stride = (w * 4) as usize;
    let mut scanout = vec![0u8; stride * h as usize];

    c.bench_function("full_frame_1080p", |b| {
        b.iter(|| {
            surface.repaint_full(|p| settings_page(p, &mut fonts, w as f32, h as f32, None));
            // Age 0 => whole-surface copy-out.
            let damage = surface.present_to_buffer(&mut scanout, stride, TargetFormat::Xrgb8888, 0);
            std::hint::black_box(&damage);
        });
    });
}

fn bench_small_damage(c: &mut Criterion) {
    let (w, h) = (1920u32, 1080u32);
    let mut fonts = FontContext::new();
    let mut surface = Surface::new(w, h, Scale::ONE);
    let stride = (w * 4) as usize;
    let mut scanout = vec![0u8; stride * h as usize];

    // Lay the static page down once and present it, so the shadow is warm and
    // the benchmark measures only the incremental repaint.
    surface.repaint_full(|p| settings_page(p, &mut fonts, w as f32, h as f32, None));
    let _ = surface.present_to_buffer(&mut scanout, stride, TargetFormat::Xrgb8888, 0);

    // A small toggle-sized region near the right edge of the first row.
    let toggle = Rect::new(w as f32 - 120.0, 90.0, 60.0, 32.0);
    let mut on = false;

    c.bench_function("small_damage_toggle", |b| {
        b.iter(|| {
            on = !on;
            let color = if on {
                Color::rgb(0x4c, 0x8d, 0xff)
            } else {
                Color::rgb(0x3a, 0x40, 0x4e)
            };
            surface.paint(|p| p.fill_rounded_rect(toggle, 16.0, color));
            // Age 1 => only this frame's damage is copied out.
            let damage = surface.present_to_buffer(&mut scanout, stride, TargetFormat::Xrgb8888, 1);
            std::hint::black_box(&damage);
        });
    });
}

criterion_group!(benches, bench_full_frame, bench_small_damage);
criterion_main!(benches);

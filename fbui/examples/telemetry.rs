//! A live telemetry dashboard: the HMI instrument widgets under streaming load.
//!
//! Two [`Gauge`]s (zoned coolant dial, load dial) and a two-series streaming
//! [`Chart`] with a [`Chart::sparkline`] row, all fed at 10 Hz from a single
//! repeating timer. Every reading flows through [`Ui::stream`], so the big
//! chart advances by a scroll-blit — a per-row `memmove` plus a few
//! re-rasterized columns — instead of a full repaint; run with `FBUI_HUD=1` to
//! watch the paint cost stay flat. Between ticks the app sleeps in `poll`.
//!
//! ```text
//! cargo run -p fbui --example telemetry --features platform
//! ```
//! Esc quits.

use std::time::Duration;

use fbui::render::Color;
use fbui::widgets::{Align, Chart, Container, Gauge, Label};
use fbui::{App, Proxy, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    Sample,
}

#[derive(Default)]
struct Dashboard {
    t: u64,
    temp: Option<WidgetId>,
    load: Option<WidgetId>,
    chart: Option<WidgetId>,
    spark: Option<WidgetId>,
}

/// Deterministic pseudo-telemetry: smooth waves with a little LCG jitter.
fn signals(t: u64) -> (f32, f32, f32) {
    let x = t as f32 * 0.1;
    let jitter = ((t
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407)
        >> 33)
        % 1000) as f32
        / 1000.0
        - 0.5;
    let temp = 62.0 + (x * 0.11).sin() * 14.0 + (x * 0.73).sin() * 3.0 + jitter * 2.0;
    let load = 45.0 + (x * 0.23).sin() * 30.0 + jitter * 6.0;
    let net = 2.0 + (x * 0.4).cos().abs() * 3.0 + jitter;
    (temp, load.clamp(0.0, 100.0), net)
}

impl App for Dashboard {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(Container::column().fill().padding(20.0).gap(14.0));
        ui.add_child(root, Label::new("Telemetry").size(24.0).bold());

        let dials = ui.add_child(root, Container::row().gap(14.0).align(Align::Center));
        self.temp = Some(
            ui.add_child(
                dials,
                Gauge::new(20.0, 110.0)
                    .zone(80.0, Color::rgb(0x34, 0xd3, 0x99))
                    .zone(95.0, Color::rgb(0xfb, 0xbf, 0x24))
                    .zone(110.0, Color::rgb(0xef, 0x44, 0x44))
                    .label("°C coolant")
                    .preferred_size(170.0, 140.0),
            ),
        );
        self.load = Some(
            ui.add_child(
                dials,
                Gauge::new(0.0, 100.0)
                    .label("% load")
                    .preferred_size(170.0, 140.0),
            ),
        );
        let side = ui.add_child(dials, Container::column().gap(6.0).grow(1.0));
        ui.add_child(side, Label::new("throughput").size(12.0));
        self.spark = Some(ui.add_child(side, Chart::sparkline().preferred_size(160.0, 36.0)));

        ui.add_child(root, Label::new("coolant °C / % load").size(12.0));
        self.chart = Some(
            ui.add_child(
                root,
                Chart::new()
                    .fixed_range(0.0, 110.0)
                    .fill(true)
                    .time_grid_every(20)
                    .sample_width(2.0),
            ),
        );
    }

    fn on_start(&mut self, proxy: Proxy<Msg>) {
        proxy.send_every(Duration::from_millis(100), Msg::Sample);
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Sample => {
                self.t += 1;
                let (temp, load, net) = signals(self.t);
                if let Some(id) = self.temp {
                    ui.stream(id, |g: &mut Gauge| g.update(temp));
                }
                if let Some(id) = self.load {
                    ui.stream(id, |g: &mut Gauge| g.update(load));
                }
                if let Some(id) = self.chart {
                    ui.stream(id, |c: &mut Chart| c.push(&[temp, load]));
                }
                if let Some(id) = self.spark {
                    ui.stream(id, |c: &mut Chart| c.push_one(net));
                }
            }
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Dashboard::default()) {
        eprintln!("telemetry: {e}");
        std::process::exit(1);
    }
}

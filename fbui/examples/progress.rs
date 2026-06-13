//! Driving the UI from a background thread via a [`Proxy`].
//!
//! Demonstrates the framework's cross-thread wakeup primitive: [`App::on_start`]
//! hands out a [`Proxy`], a worker thread runs a long job *off* the UI thread and
//! posts progress with `proxy.send(..)`, and the runner delivers each message to
//! [`App::update`] — so the UI stays responsive while the work runs. This is the
//! shape an app uses to drive its UI from a separate worker (or an IPC reader)
//! without the framework knowing what the work is.
//!
//! ```text
//! cargo run -p fbui --example progress --features platform
//! ```
//! Esc quits.

use std::thread;
use std::time::Duration;

use fbui::widgets::{Align, Container, Label, ProgressBar};
use fbui::{App, Proxy, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    Progress(u8),
    Done,
}

#[derive(Default)]
struct Work {
    label: Option<WidgetId>,
    bar: Option<WidgetId>,
}

impl App for Work {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(
            Container::column()
                .fill()
                .padding(24.0)
                .gap(16.0)
                .align(Align::Center),
        );
        ui.add_child(root, Label::new("Working…").size(28.0).bold());
        self.label = Some(ui.add_child(root, Label::new("0%").size(48.0)));
        // A fixed-width row so the bar has a definite length to fill.
        let row = ui.add_child(root, Container::row().width(320.0));
        self.bar = Some(ui.add_child(row, ProgressBar::new(0.0)));
    }

    fn on_start(&mut self, proxy: Proxy<Msg>) {
        // The long-running job lives off the UI thread; a real app would do I/O
        // here (e.g. read progress from an IPC socket). The framework just ferries
        // each message back to `update` and repaints.
        thread::spawn(move || {
            for pct in 1..=100 {
                thread::sleep(Duration::from_millis(40));
                if !proxy.send(Msg::Progress(pct)) {
                    return; // UI exited; stop working.
                }
            }
            let _ = proxy.send(Msg::Done);
        });
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        let (text, fraction) = match msg {
            Msg::Progress(p) => (format!("{p}%"), p as f32 / 100.0),
            Msg::Done => ("Done".to_string(), 1.0),
        };
        if let Some(id) = self.label {
            ui.with::<Label, _>(id, |l| l.set_text(text));
        }
        if let Some(id) = self.bar {
            ui.with::<ProgressBar, _>(id, |b| b.set_fraction(fraction));
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Work::default()) {
        eprintln!("progress: {e}");
        std::process::exit(1);
    }
}

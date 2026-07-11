//! App timers without threads: [`Proxy::send_after`] / [`Proxy::send_every`].
//!
//! A clock label ticks once a second from a repeating timer, a button arms a
//! one-shot "ping" three seconds out, and another button cancels the tick via
//! its [`fbui::Timer`] handle. No worker threads, no polling: between
//! deadlines the event loop sleeps in `poll`, so the app burns ~0% CPU while
//! the countdown runs — and exactly 0 once everything is cancelled.
//!
//! ```text
//! cargo run -p fbui --example timer --features platform
//! ```
//! Esc quits.

use std::time::Duration;

use fbui::widgets::{Align, Button, Container, Label, Toasts};
use fbui::{App, Proxy, Timer, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    Tick,
    ArmPing,
    Ping,
    StopClock,
}

#[derive(Default)]
struct Clock {
    seconds: u64,
    label: Option<WidgetId>,
    toasts: Option<WidgetId>,
    proxy: Option<Proxy<Msg>>,
    ticker: Option<Timer>,
}

impl App for Clock {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(
            Container::column()
                .fill()
                .padding(24.0)
                .gap(16.0)
                .align(Align::Center),
        );
        ui.add_child(root, Label::new("Uptime").size(28.0).bold());
        self.label = Some(ui.add_child(root, Label::new("0 s").size(48.0)));
        let row = ui.add_child(root, Container::row().gap(12.0));
        ui.add_child(row, Button::new("Ping me in 3 s").on_press(|| Msg::ArmPing));
        ui.add_child(row, Button::new("Stop clock").on_press(|| Msg::StopClock));
        self.toasts = Some(ui.add_child(root, Toasts::new()));
    }

    fn on_start(&mut self, proxy: Proxy<Msg>) {
        // One repeating deadline drives the clock; keep the handle so the
        // "Stop clock" button can cancel it.
        self.ticker = Some(proxy.send_every(Duration::from_secs(1), Msg::Tick));
        self.proxy = Some(proxy);
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Tick => {
                self.seconds += 1;
                let text = format!("{} s", self.seconds);
                if let Some(label) = self.label {
                    ui.with::<Label, _>(label, |l| l.set_text(text));
                }
            }
            Msg::ArmPing => {
                // A one-shot; dropping the returned handle detaches it (the
                // message still arrives) — we don't need to cancel this one.
                if let Some(proxy) = &self.proxy {
                    drop(proxy.send_after(Duration::from_secs(3), Msg::Ping));
                }
            }
            Msg::Ping => {
                if let Some(toasts) = self.toasts {
                    ui.with::<Toasts, _>(toasts, |t| {
                        t.push(fbui::widgets::ToastKind::Info, "Ping!")
                    });
                }
            }
            Msg::StopClock => {
                if let Some(t) = self.ticker.take() {
                    t.cancel();
                }
            }
        }
    }
}

fn main() -> fbui::Result<()> {
    fbui::run(Clock::default())
}

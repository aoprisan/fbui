//! A 10 000-row list — the windowing stress test from the Phase 3 exit criteria.
//! Only the visible rows are painted, so scrolling stays at refresh regardless of
//! length.
//!
//! ```text
//! cargo run -p fbui --example big_list --features platform
//! ```
//! Scroll the wheel or drag; click or arrow-key to select. Esc quits.

use fbui::widgets::{Container, Label, List};
use fbui::{App, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    Selected(usize),
}

#[derive(Default)]
struct Big {
    status: Option<WidgetId>,
}

impl App for Big {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(Container::column().fill().padding(12.0).gap(8.0));
        ui.add_child(root, Label::new("10,000 rows").size(20.0).bold());
        self.status = Some(ui.add_child(
            root,
            Label::new("select a row").color(ui.theme().palette.muted),
        ));

        let rows: Vec<String> = (0..10_000).map(|i| format!("Row #{i:05}")).collect();
        ui.add_child(root, List::new(rows).on_select(Msg::Selected));
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        let Msg::Selected(i) = msg;
        if let Some(id) = self.status {
            ui.with::<Label, _>(id, |l| l.set_text(format!("selected row #{i:05}")));
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Big::default()) {
        eprintln!("big_list: {e}");
        std::process::exit(1);
    }
}

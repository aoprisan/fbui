//! The overlay layer: a modal [`Dialog`], a [`Select`] dropdown, and
//! [`Toasts`] — everything that floats above the page.
//!
//! ```text
//! cargo run -p fbui --example overlay --features platform
//! ```
//!
//! * **Select** — click (or focus + Enter) to drop the menu over the content
//!   below; pick an option and a toast confirms it.
//! * **Erase…** — opens a modal dialog: the page dims, clicks land on the
//!   scrim, Tab cycles only inside the card, Esc or Cancel dismisses,
//!   Erase fires a toast.
//! * Toasts stack bottom-center and fade out on their own.

use fbui::widgets::{Button, Container, Dialog, Label, Select, Stack, ToastKind, Toasts};
use fbui::{App, Ui, WidgetId};

#[derive(Clone)]
enum Msg {
    PickedQuality(usize),
    OpenDialog,
    CloseDialog,
    Erase,
}

const QUALITIES: [&str; 4] = ["Low", "Medium", "High", "Ultra"];

#[derive(Default)]
struct Overlay {
    stack: Option<WidgetId>,
    dialog: Option<WidgetId>,
    toasts: Option<WidgetId>,
}

impl App for Overlay {
    type Message = Msg;

    fn build(&mut self, ui: &mut Ui<Msg>) {
        // A full-screen Stack: the page is child 0; dialogs are pushed on top;
        // the toast host floats above it all.
        let stack = ui.set_root(Stack::new());
        self.stack = Some(stack);

        let page = ui.add_child(stack, Container::column().padding(24.0).gap(14.0));
        ui.add_child(page, Label::new("Overlay demo").size(26.0).bold());

        let muted = ui.theme().palette.muted;
        ui.add_child(page, Label::new("Render quality").color(muted));
        ui.add_child(
            page,
            Select::new(QUALITIES)
                .selected(1)
                .on_change(Msg::PickedQuality),
        );

        // Content below the select, so the open menu visibly floats over it.
        ui.add_child(page, Label::new("Danger zone").color(muted));
        let row = ui.add_child(page, Container::row().gap(10.0));
        ui.add_child(
            row,
            Button::new("Erase everything…")
                .danger()
                .on_press(|| Msg::OpenDialog),
        );

        self.toasts = Some(ui.add_child(stack, Toasts::new()));
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::PickedQuality(i) => self.toast(
                ui,
                ToastKind::Info,
                format!("Quality set to {}", QUALITIES[i]),
            ),
            Msg::OpenDialog => self.open_dialog(ui),
            Msg::CloseDialog => self.close_dialog(ui),
            Msg::Erase => {
                self.close_dialog(ui);
                self.toast(ui, ToastKind::Error, "Everything erased");
            }
        }
    }
}

impl Overlay {
    fn toast(&self, ui: &mut Ui<Msg>, kind: ToastKind, text: impl Into<String>) {
        if let Some(id) = self.toasts {
            ui.with::<Toasts, _>(id, |t| t.push(kind, text));
        }
    }

    fn open_dialog(&mut self, ui: &mut Ui<Msg>) {
        if self.dialog.is_some() {
            return;
        }
        let Some(stack) = self.stack else { return };
        let surface = ui.theme().palette.surface;
        let dialog = ui.add_child(stack, Dialog::new().on_dismiss(|| Msg::CloseDialog));
        let card = ui.add_child(
            dialog,
            Container::column()
                .padding(20.0)
                .gap(14.0)
                .background(surface, 12.0),
        );
        ui.add_child(card, Label::new("Erase everything?").size(20.0).bold());
        ui.add_child(
            card,
            Label::new("This cannot be undone. Esc or a click outside cancels."),
        );
        let buttons = ui.add_child(card, Container::row().gap(10.0));
        ui.add_child(
            buttons,
            Button::new("Cancel")
                .secondary()
                .on_press(|| Msg::CloseDialog),
        );
        ui.add_child(
            buttons,
            Button::new("Erase").danger().on_press(|| Msg::Erase),
        );
        // Land keyboard focus inside the modal so Tab cycles the card's
        // buttons and Esc reaches the dialog.
        ui.focus_first(dialog);
        self.dialog = Some(dialog);
    }

    fn close_dialog(&mut self, ui: &mut Ui<Msg>) {
        if let Some(dialog) = self.dialog.take() {
            ui.remove(dialog);
        }
    }
}

fn main() {
    if let Err(e) = fbui::run(Overlay::default()) {
        eprintln!("overlay: {e}");
        std::process::exit(1);
    }
}

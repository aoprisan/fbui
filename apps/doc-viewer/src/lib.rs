//! # fbui-doc-viewer — a document viewer for a machine with no OS
//!
//! PDF pages (through `fbui-doc`'s subset renderer) and PNG/JPEG images,
//! with a library screen, paging, zoom and panning. Everything here is
//! `no_std + alloc` and board-independent: a board crate hands [`Viewer`] the
//! documents (`include_bytes!`, a flash partition, an SD card read) and runs
//! it with [`fbui_bare::run`]. The same app runs on the host through
//! [`fbui_bare::Runner`] — that's how the tests drive it.
//!
//! The UI uses only fbui-widgets' **minimal set** (`Container`, `Label`,
//! `Button`, `List`, `ProgressBar`) plus one custom widget,
//! [`page_view::PageView`].
//!
//! Keys (any board input that maps to them):
//!
//! | key | action |
//! |---|---|
//! | → / PageDown / Space | next page (Space scrolls first) |
//! | ← / PageUp / Backspace | previous page |
//! | ↑ / ↓ | scroll |
//! | Home / End | first / last page |
//! | `+` / `-` | zoom in / out |
//! | `w` | fit width (the default) |
//! | `p` | fit whole page |
//! | `o` / Esc | library ↔ document |
//! | Enter | (library) open the highlighted document |

#![no_std]

extern crate alloc;

pub mod page_view;

use alloc::borrow::Cow;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fbui_bare::App;
use fbui_doc::pdf::{Document, RenderOptions};
use fbui_doc::tiny_skia::{self, Pixmap};
use fbui_doc::Format;
// `f32::round` & co. live in std; on a pure no_std build this trait
// supplies them (with std in the graph the inherent methods win).
use fbui_render::geom::Rect;
#[allow(unused_imports)]
use fbui_render::math::F32Ext;
use fbui_render::Image;
use fbui_widgets::event::Key;
use fbui_widgets::widgets::{Align, Button, Container, Label, List, ProgressBar};
use fbui_widgets::{Theme, Ui, WidgetId};

use page_view::{PageView, MARGIN};

/// Zoom steps for `+`/`-`, as multiples of "fit width".
const ZOOM_STEPS: &[f32] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];
/// Logical px one arrow press scrolls.
const LINE: f32 = 60.0;

/// A document the board makes available.
pub struct Entry {
    pub name: String,
    pub data: Cow<'static, [u8]>,
}

impl Entry {
    pub fn new(name: impl Into<String>, data: impl Into<Cow<'static, [u8]>>) -> Self {
        Entry {
            name: name.into(),
            data: data.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    FitWidth,
    FitPage,
    /// A multiple of fit-width.
    Step(usize),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Library,
    Highlight(usize),
    Open(usize),
    OpenHighlighted,
    Prev,
    Next,
    ZoomIn,
    ZoomOut,
    Fit(Zoom),
}

enum Loaded {
    Pdf(Document),
    Image(Rc<Pixmap>),
}

/// What's on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Library,
    Document,
}

pub struct Viewer {
    entries: Vec<Entry>,
    fallback_font: Option<Cow<'static, [u8]>>,
    /// The interface font: borrowed (compiled in) fonts are used in place.
    ui_font: Cow<'static, [u8]>,
    loaded: Option<(usize, Loaded)>,
    page: usize,
    zoom: Zoom,
    highlighted: usize,
    screen: Screen,
    /// Upper bound for one page raster, in pixels.
    pub max_pixels: u64,
    ids: Ids,
}

#[derive(Default)]
struct Ids {
    title: Option<WidgetId>,
    status: Option<WidgetId>,
    zoom: Option<WidgetId>,
    progress: Option<WidgetId>,
    body: Option<WidgetId>,
    page: Option<WidgetId>,
    list: Option<WidgetId>,
}

impl Viewer {
    /// A viewer over `entries`. `ui_font` renders the interface; it is also
    /// the fallback for PDF fonts that aren't embedded.
    pub fn new(entries: Vec<Entry>, ui_font: impl Into<Cow<'static, [u8]>>) -> Self {
        let ui_font = ui_font.into();
        Viewer {
            entries,
            fallback_font: Some(ui_font.clone()),
            ui_font,
            loaded: None,
            page: 0,
            zoom: Zoom::FitWidth,
            highlighted: 0,
            screen: Screen::Library,
            max_pixels: 12 * 1024 * 1024,
            ids: Ids::default(),
        }
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// Current page (0-based) and page count of the open document.
    pub fn position(&self) -> Option<(usize, usize)> {
        self.loaded
            .as_ref()
            .map(|(_, l)| (self.page, page_count(l)))
    }

    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    fn set_label(&self, ui: &mut Ui<Msg>, id: Option<WidgetId>, text: &str) {
        if let Some(id) = id {
            let t = text.to_string();
            ui.with::<Label, _>(id, move |l| l.set_text(t));
        }
    }

    fn show_library(&mut self, ui: &mut Ui<Msg>) {
        self.screen = Screen::Library;
        let Some(body) = self.ids.body else { return };
        for c in ui.child_ids(body) {
            ui.remove(c);
        }
        self.ids.page = None;
        let rows: Vec<String> = self
            .entries
            .iter()
            .map(|e| {
                let kind = match fbui_doc::sniff(&e.data) {
                    Format::Pdf => "PDF",
                    Format::Png => "PNG",
                    Format::Jpeg => "JPEG",
                    Format::Unknown => "?",
                };
                format!(
                    "{}   ·   {kind}, {} KiB",
                    e.name,
                    e.data.len().div_ceil(1024)
                )
            })
            .collect();
        let list = ui.add_named(
            body,
            "library",
            List::new(rows).row_height(44.0).on_select(Msg::Highlight),
        );
        self.ids.list = Some(list);
        ui.focus(Some(list));
        self.set_label(ui, self.ids.title, "Library");
        let hint = if self.entries.is_empty() {
            "No documents".to_string()
        } else {
            format!(
                "{} documents — ↑↓ to choose, Enter to open",
                self.entries.len()
            )
        };
        self.set_label(ui, self.ids.status, &hint);
        self.set_label(ui, self.ids.zoom, "");
        if let Some(p) = self.ids.progress {
            ui.with::<ProgressBar, _>(p, |b| b.set_fraction(0.0));
        }
    }

    fn open(&mut self, index: usize, ui: &mut Ui<Msg>) {
        let Some(entry) = self.entries.get(index) else {
            return;
        };
        let loaded = match fbui_doc::sniff(&entry.data) {
            Format::Pdf => Document::parse(entry.data.clone()).map(|mut d| {
                if let Some(f) = &self.fallback_font {
                    d.set_fallback_font(f.clone());
                }
                Loaded::Pdf(d)
            }),
            Format::Png | Format::Jpeg => fbui_doc::raster::decode(&entry.data, self.max_pixels)
                .map(|p| Loaded::Image(Rc::new(p)))
                .map_err(|_| fbui_doc::pdf::Error::Corrupt("image")),
            Format::Unknown => Err(fbui_doc::pdf::Error::Unsupported("file type")),
        };
        let name = entry.name.clone();
        match loaded {
            Ok(l) => {
                self.loaded = Some((index, l));
                self.page = 0;
                self.zoom = Zoom::FitWidth;
                self.show_document(ui);
            }
            Err(e) => {
                let msg = format!("{name}: {e}");
                self.set_label(ui, self.ids.status, &msg);
            }
        }
    }

    fn show_document(&mut self, ui: &mut Ui<Msg>) {
        self.screen = Screen::Document;
        let Some(body) = self.ids.body else { return };
        if self.ids.page.is_none() {
            for c in ui.child_ids(body) {
                ui.remove(c);
            }
            self.ids.list = None;
            self.ids.page = Some(ui.add_named(body, "page", PageView::new()));
            ui.focus(None);
        }
        self.render(ui, false);
    }

    /// Render the current page at the current zoom into the page view.
    fn render(&mut self, ui: &mut Ui<Msg>, keep_y: bool) {
        let Some(page_id) = self.ids.page else { return };
        // The viewport is only known after layout.
        ui.layout_now();
        let view = ui.bounds(page_id).unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        let scale = ui.scale().factor();
        let avail_w = (view.w - 2.0 * MARGIN).max(32.0);
        let avail_h = (view.h - 2.0 * MARGIN).max(32.0);
        let Some((index, loaded)) = &self.loaded else {
            return;
        };
        let (pw, ph) = match loaded {
            Loaded::Pdf(d) => d
                .page(self.page)
                .map(|p| p.size())
                .unwrap_or((612.0, 792.0)),
            Loaded::Image(p) => (p.width() as f32 / scale, p.height() as f32 / scale),
        };
        let fit_w = avail_w / pw;
        let fit_p = fit_w.min(avail_h / ph);
        // Logical px per page unit.
        let z = match self.zoom {
            Zoom::FitWidth => fit_w,
            Zoom::FitPage => fit_p,
            Zoom::Step(i) => fit_w * ZOOM_STEPS[i.min(ZOOM_STEPS.len() - 1)],
        };
        let name = self.entries[*index].name.clone();
        let count = page_count(loaded);
        let image = match loaded {
            Loaded::Pdf(d) => {
                let opts = RenderOptions {
                    zoom: z * scale,
                    max_pixels: self.max_pixels,
                    ..Default::default()
                };
                d.render_page(self.page, &opts).map_err(|e| e.to_string())
            }
            Loaded::Image(p) => scale_pixmap(p, z).ok_or_else(|| "image too large".to_string()),
        };
        // 100% = a PDF point per logical pixel, or an image pixel per device
        // pixel.
        let percent = (z * 100.0).round() as i32;
        let (image, status) = match image {
            Ok(pm) => (Some(Rc::new(Image::from_pixmap(pm))), String::new()),
            Err(e) => (None, format!("page {}: {e}", self.page + 1)),
        };
        ui.with::<PageView, _>(page_id, move |v| v.set_image(image, keep_y));
        self.set_label(ui, self.ids.title, &name);
        let pos = format!("{} / {count}", self.page + 1);
        let status = if status.is_empty() { pos } else { status };
        self.set_label(ui, self.ids.status, &status);
        let zl = match self.zoom {
            Zoom::FitWidth => format!("{percent}% (width)"),
            Zoom::FitPage => format!("{percent}% (page)"),
            Zoom::Step(_) => format!("{percent}%"),
        };
        self.set_label(ui, self.ids.zoom, &zl);
        if let Some(p) = self.ids.progress {
            let f = (self.page + 1) as f32 / count.max(1) as f32;
            ui.with::<ProgressBar, _>(p, move |b| b.set_fraction(f));
        }
    }

    fn go(&mut self, page: usize, ui: &mut Ui<Msg>, at_bottom: bool) {
        let Some((_, l)) = &self.loaded else { return };
        let count = page_count(l);
        if page >= count || page == self.page {
            return;
        }
        self.page = page;
        self.render(ui, false);
        if at_bottom {
            if let Some(id) = self.ids.page {
                ui.with::<PageView, _>(id, |v| v.to_bottom());
            }
        }
    }

    fn zoom_by(&mut self, dir: i32, ui: &mut Ui<Msg>) {
        let cur = match self.zoom {
            Zoom::Step(i) => i as i32,
            // Both fit modes step from the 1.0 (= fit width) rung.
            _ => ZOOM_STEPS.iter().position(|&s| s == 1.0).unwrap_or(2) as i32,
        };
        let i = (cur + dir).clamp(0, ZOOM_STEPS.len() as i32 - 1) as usize;
        self.zoom = Zoom::Step(i);
        self.render(ui, true);
    }

    fn pan(&mut self, ui: &mut Ui<Msg>, dy: f32) -> bool {
        let Some(id) = self.ids.page else {
            return false;
        };
        ui.with::<PageView, _>(id, |v| v.pan(0.0, dy))
            .unwrap_or(false)
    }

    fn page_view<R>(&self, ui: &mut Ui<Msg>, f: impl FnOnce(&mut PageView) -> R) -> Option<R> {
        ui.with::<PageView, _>(self.ids.page?, f)
    }
}

fn page_count(l: &Loaded) -> usize {
    match l {
        Loaded::Pdf(d) => d.page_count(),
        Loaded::Image(_) => 1,
    }
}

/// Scale a decoded image by `z` (bilinear), for zooming PNG/JPEG documents.
fn scale_pixmap(src: &Pixmap, z: f32) -> Option<Pixmap> {
    let w = (src.width() as f32 * z).round().max(1.0) as u32;
    let h = (src.height() as f32 * z).round().max(1.0) as u32;
    let mut out = Pixmap::new(w, h)?;
    let paint = tiny_skia::PixmapPaint {
        quality: tiny_skia::FilterQuality::Bilinear,
        ..Default::default()
    };
    let ts = tiny_skia::Transform::from_scale(
        w as f32 / src.width() as f32,
        h as f32 / src.height() as f32,
    );
    out.draw_pixmap(0, 0, src.as_ref(), &paint, ts, None);
    Some(out)
}

impl App for Viewer {
    type Message = Msg;

    fn theme(&self) -> Theme {
        Theme::dark()
    }

    fn static_fonts(&self) -> Vec<&'static [u8]> {
        match self.ui_font {
            Cow::Borrowed(f) => alloc::vec![f],
            Cow::Owned(_) => Vec::new(),
        }
    }

    fn fonts(&self) -> Vec<Vec<u8>> {
        match &self.ui_font {
            Cow::Borrowed(_) => Vec::new(),
            Cow::Owned(f) => alloc::vec![f.clone()],
        }
    }

    fn build(&mut self, ui: &mut Ui<Msg>) {
        let pal = ui.theme().palette.clone();
        let root = ui.set_root(Container::column().fill());
        let bar = ui.add_child(
            root,
            Container::row()
                .padding(6.0)
                .gap(6.0)
                .align(Align::Center)
                .background(pal.surface, 0.0),
        );
        ui.add_named(
            bar,
            "library-button",
            Button::new("Files").secondary().on_press(|| Msg::Library),
        );
        ui.add_named(
            bar,
            "prev",
            Button::new("<").secondary().on_press(|| Msg::Prev),
        );
        ui.add_named(
            bar,
            "next",
            Button::new(">").secondary().on_press(|| Msg::Next),
        );
        ui.add_named(
            bar,
            "zoom-out",
            Button::new("-").secondary().on_press(|| Msg::ZoomOut),
        );
        ui.add_named(
            bar,
            "zoom-in",
            Button::new("+").secondary().on_press(|| Msg::ZoomIn),
        );
        ui.add_named(
            bar,
            "fit",
            Button::new("Fit")
                .secondary()
                .on_press(|| Msg::Fit(Zoom::FitWidth)),
        );
        let title_box = ui.add_child(bar, Container::column().grow(1.0).shrink());
        self.ids.title = Some(ui.add_named(title_box, "title", Label::new("Library").bold()));
        self.ids.status = Some(ui.add_named(
            title_box,
            "status",
            Label::new("").size(12.0).color(pal.muted),
        ));
        self.ids.zoom = Some(ui.add_named(bar, "zoom", Label::new("").size(12.0).color(pal.muted)));
        ui.add_named(
            bar,
            "open",
            Button::new("Open").on_press(|| Msg::OpenHighlighted),
        );
        // ProgressBar grows along its parent's main axis: give it a row.
        let strip = ui.add_child(root, Container::row().height(4.0));
        self.ids.progress = Some(ui.add_named(strip, "progress", ProgressBar::new(0.0)));
        self.ids.body = Some(ui.add_child(root, Container::column().grow(1.0).shrink()));
        self.show_library(ui);
        // One document: skip the library.
        if self.entries.len() == 1 {
            self.open(0, ui);
        }
    }

    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        match msg {
            Msg::Library => match self.screen {
                Screen::Library if self.loaded.is_some() => self.show_document(ui),
                _ => self.show_library(ui),
            },
            Msg::Highlight(i) => {
                self.highlighted = i;
                if let Some(e) = self.entries.get(i) {
                    let s = format!("Enter to open {}", e.name);
                    self.set_label(ui, self.ids.status, &s);
                }
            }
            Msg::Open(i) => self.open(i, ui),
            Msg::OpenHighlighted => {
                if self.screen == Screen::Library {
                    self.open(self.highlighted, ui);
                }
            }
            Msg::Prev if self.screen == Screen::Document => {
                self.go(self.page.wrapping_sub(1), ui, false)
            }
            Msg::Next if self.screen == Screen::Document => self.go(self.page + 1, ui, false),
            Msg::ZoomIn if self.screen == Screen::Document => self.zoom_by(1, ui),
            Msg::ZoomOut if self.screen == Screen::Document => self.zoom_by(-1, ui),
            Msg::Fit(z) if self.screen == Screen::Document => {
                self.zoom = z;
                self.render(ui, true);
            }
            _ => {}
        }
    }

    fn on_key(&mut self, key: Key, ui: &mut Ui<Msg>) -> bool {
        if self.screen == Screen::Library {
            return match key {
                Key::Enter => {
                    self.open(self.highlighted, ui);
                    true
                }
                Key::Escape | Key::Char('o') => {
                    if self.loaded.is_some() {
                        self.show_document(ui);
                    }
                    true
                }
                _ => false,
            };
        }
        let screen_h = self.page_view(ui, |v| v.viewport().h).unwrap_or(300.0) * 0.9;
        match key {
            Key::Right | Key::PageDown => self.go(self.page + 1, ui, false),
            Key::Left | Key::PageUp | Key::Backspace => {
                self.go(self.page.wrapping_sub(1), ui, false)
            }
            Key::Space => {
                // Read on: scroll a screen, then turn the page.
                if self.page_view(ui, |v| v.at_bottom()).unwrap_or(true) {
                    self.go(self.page + 1, ui, false);
                } else {
                    self.pan(ui, screen_h);
                }
            }
            Key::Down => {
                self.pan(ui, LINE);
            }
            Key::Up => {
                if self.page_view(ui, |v| v.at_top()).unwrap_or(false) && self.page > 0 {
                    self.go(self.page - 1, ui, true);
                } else {
                    self.pan(ui, -LINE);
                }
            }
            Key::Home => self.go(0, ui, false),
            Key::End => {
                let last = self.position().map_or(0, |(_, n)| n.saturating_sub(1));
                self.go(last, ui, false);
            }
            Key::Char('+') | Key::Char('=') => self.zoom_by(1, ui),
            Key::Char('-') => self.zoom_by(-1, ui),
            Key::Char('w') => self.update(Msg::Fit(Zoom::FitWidth), ui),
            Key::Char('p') => self.update(Msg::Fit(Zoom::FitPage), ui),
            Key::Escape | Key::Char('o') => self.show_library(ui),
            _ => return false,
        }
        true
    }
}

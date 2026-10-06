//! [`PageView`] — the one custom widget the viewer needs: a rendered page
//! that pans in both directions (drag, wheel, keys), centred when it is
//! narrower than the viewport. `ScrollView` scrolls vertically only and lays
//! out children; a page is a single bitmap, so a small leaf widget is both
//! simpler and cheaper.

use alloc::rc::Rc;
use core::any::Any;

use fbui_render::geom::{Point, Rect, Size};
use fbui_render::{Color, FontContext, Image};
use fbui_widgets::ctx::{EventCtx, PaintCtx};
use fbui_widgets::describe::Describe;
use fbui_widgets::event::{Event, PointerButton};
use fbui_widgets::style::{self, Style};
use fbui_widgets::widget::{AvailableSize, KnownDims, Widget};
use fbui_widgets::Theme;

/// Gap around the page, logical px.
pub const MARGIN: f32 = 12.0;

pub struct PageView {
    image: Option<Rc<Image>>,
    /// Pan offset of the page's top-left, logical px (≥ 0).
    offset: Point,
    bounds: Rect,
    scale: f32,
    drag: Option<Point>,
}

impl Default for PageView {
    fn default() -> Self {
        Self::new()
    }
}

impl PageView {
    pub fn new() -> Self {
        PageView {
            image: None,
            offset: Point::new(0.0, 0.0),
            bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            scale: 1.0,
            drag: None,
        }
    }

    /// Show `image` (device pixels). `keep_y`: keep the vertical position
    /// as a fraction (a zoom), else start at the top (a new page).
    pub fn set_image(&mut self, image: Option<Rc<Image>>, keep_y: bool) {
        let frac = if keep_y {
            let max = self.max().1;
            if max > 0.0 {
                self.offset.y / max
            } else {
                0.0
            }
        } else {
            0.0
        };
        self.image = image;
        let (mx, my) = self.max();
        self.offset = Point::new((self.offset.x).min(mx), (frac * my).clamp(0.0, my));
        if !keep_y {
            self.offset.x = 0.0;
        }
    }

    /// Size of the page in logical px.
    fn page_size(&self) -> Size {
        match &self.image {
            Some(i) => Size::new(
                i.width() as f32 / self.scale,
                i.height() as f32 / self.scale,
            ),
            None => Size::new(0.0, 0.0),
        }
    }

    fn max(&self) -> (f32, f32) {
        let p = self.page_size();
        (
            (p.w + 2.0 * MARGIN - self.bounds.w).max(0.0),
            (p.h + 2.0 * MARGIN - self.bounds.h).max(0.0),
        )
    }

    /// The viewport size in logical px, as last laid out.
    pub fn viewport(&self) -> Size {
        Size::new(self.bounds.w, self.bounds.h)
    }

    /// Pan by `(dx, dy)`; returns whether anything moved.
    pub fn pan(&mut self, dx: f32, dy: f32) -> bool {
        let (mx, my) = self.max();
        let new = Point::new(
            (self.offset.x + dx).clamp(0.0, mx),
            (self.offset.y + dy).clamp(0.0, my),
        );
        let moved = (new.x - self.offset.x).abs() > 0.01 || (new.y - self.offset.y).abs() > 0.01;
        self.offset = new;
        moved
    }

    pub fn at_top(&self) -> bool {
        self.offset.y <= 0.01
    }

    pub fn at_bottom(&self) -> bool {
        self.offset.y >= self.max().1 - 0.01
    }

    /// Jump to the bottom (paging backwards lands at the end of a page).
    pub fn to_bottom(&mut self) {
        self.offset.y = self.max().1;
    }
}

impl<Msg: 'static> Widget<Msg> for PageView {
    fn layout_style(&self, _theme: &Theme) -> Style {
        // Fill whatever the column leaves over; never push the toolbar away.
        Style {
            flex_grow: 1.0,
            flex_shrink: 1.0,
            flex_basis: style::length(0.0),
            size: taffy::Size {
                width: style::percent(1.0),
                height: style::auto(),
            },
            min_size: style::size(0.0, 0.0),
            ..Style::default()
        }
    }

    fn measure(
        &mut self,
        _: &mut FontContext,
        _: &Theme,
        _: KnownDims,
        _: AvailableSize,
    ) -> Option<Size> {
        Some(Size::new(0.0, 0.0))
    }

    fn placed(&mut self, bounds: Rect, scale: fbui_render::Scale) {
        self.bounds = bounds;
        self.scale = scale.factor();
        let (mx, my) = self.max();
        self.offset = Point::new(self.offset.x.min(mx), self.offset.y.min(my));
    }

    fn clips(&self) -> bool {
        true
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let bg = ctx.theme().palette.bg;
        let page = self.page_size();
        let p = ctx.painter();
        p.push_clip(b);
        p.fill_rect(b, bg);
        if let Some(img) = &self.image {
            // Centre horizontally when the page is narrower than the view.
            let x = if page.w + 2.0 * MARGIN <= b.w {
                b.x + (b.w - page.w) / 2.0
            } else {
                b.x + MARGIN - self.offset.x
            };
            let y = b.y + MARGIN - self.offset.y;
            // A soft drop shadow, then the paper.
            let shadow = Rect::new(x + 2.0, y + 3.0, page.w, page.h);
            p.fill_rect(shadow, Color::rgba(0, 0, 0, 0x50));
            p.draw_image(img, Point::new(x, y));
        }
        p.pop_clip();
    }

    fn event(&mut self, ctx: &mut EventCtx<Msg>) {
        match ctx.event().clone() {
            Event::PointerDown {
                pos,
                button: PointerButton::Left,
            } => {
                self.drag = Some(pos);
                ctx.capture_pointer();
                ctx.set_handled();
            }
            Event::PointerMove { pos } => {
                if let Some(last) = self.drag {
                    if self.pan(last.x - pos.x, last.y - pos.y) {
                        ctx.request_paint();
                    }
                    self.drag = Some(pos);
                    ctx.set_handled();
                }
            }
            Event::PointerUp { .. } => {
                if self.drag.take().is_some() {
                    ctx.release_pointer();
                    ctx.set_handled();
                }
            }
            Event::Scroll {
                delta_x, delta_y, ..
            } => {
                if self.pan(delta_x, delta_y) {
                    ctx.request_paint();
                }
                ctx.set_handled();
            }
            _ => {}
        }
    }

    fn describe(&self, out: &mut Describe) {
        if let Some(i) = &self.image {
            out.prop("page", alloc::format!("{}x{}", i.width(), i.height()));
        }
        out.prop(
            "offset",
            alloc::format!("{},{}", self.offset.x as i32, self.offset.y as i32),
        );
    }

    fn debug_name(&self) -> &'static str {
        "PageView"
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

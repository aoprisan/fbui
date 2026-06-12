//! [`ImageView`] — blits a decoded image.
//!
//! v1 draws the image 1:1 at its natural size, top-left-aligned and clipped to
//! the widget bounds (object-fit scaling is a later refinement, since the Phase 2
//! painter blits images unscaled).

use std::any::Any;
use std::rc::Rc;

use fbui_render::geom::{Point, Size};
use fbui_render::{FontContext, Image};

use crate::ctx::PaintCtx;
use crate::style::Style;
use crate::theme::Theme;
use crate::widget::{AvailableSize, KnownDims, Widget};

/// A widget that displays a raster [`Image`]. The image is `Rc`-shared so the
/// same decoded bitmap can back several widgets cheaply.
pub struct ImageView {
    image: Rc<Image>,
}

impl ImageView {
    pub fn new(image: Image) -> Self {
        ImageView {
            image: Rc::new(image),
        }
    }

    pub fn shared(image: Rc<Image>) -> Self {
        ImageView { image }
    }
}

impl<Msg: 'static> Widget<Msg> for ImageView {
    fn layout_style(&self, _theme: &Theme) -> Style {
        Style::default()
    }

    fn measure(
        &mut self,
        _fonts: &mut FontContext,
        _theme: &Theme,
        _known: KnownDims,
        _available: AvailableSize,
    ) -> Option<Size> {
        Some(self.image.size())
    }

    fn paint(&self, ctx: &mut PaintCtx) {
        let b = ctx.bounds();
        let img = self.image.clone();
        let p = ctx.painter();
        p.push_clip(b);
        p.draw_image(&img, Point::new(b.x, b.y));
        p.pop_clip();
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

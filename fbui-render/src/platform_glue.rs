//! The one bridge to `fbui-platform`, behind the `platform` feature.
//!
//! Core `fbui-render` knows nothing about DRM or fbdev. This module — compiled
//! only when a downstream actually wants to drive a display — adds a single
//! method that runs the standard frame cycle against a `Display`:
//! `begin_frame` → copy the shadow's damaged spans into the back buffer →
//! `present` the damaged regions. It is the glue PLAN/PHASE1 promised, kept
//! deliberately thin so the headless core stays the tested default.

use fbui_platform::{Display, Frame, PixelFormat, Rect as PRect, Result};

use crate::copyout::TargetFormat;
use crate::geom::IRect;
use crate::surface::Surface;

impl Surface {
    /// Render the pending damage to `display` for one frame.
    ///
    /// Returns `Ok(true)` if a frame was presented, `Ok(false)` if there was
    /// nothing to draw or no back buffer was free yet (a previous present is
    /// still in flight — wait on the display's `present_fd` and retry).
    ///
    /// The back buffer's [`age`](fbui_platform::Frame::age) drives buffer-age
    /// repaint: an aged buffer is brought current by unioning recent damage.
    pub fn present(&mut self, display: &mut dyn Display) -> Result<bool> {
        if self.is_clean() {
            return Ok(false);
        }

        // Scope the frame borrow: we must finish writing the back buffer and drop
        // the `Frame` (which borrows `display`) before calling `present`.
        let damage = {
            let Some(frame) = display.begin_frame()? else {
                return Ok(false);
            };
            let format = map_format(frame.format);
            let age = frame.age;
            let stride = frame.stride;
            self.present_to_buffer(&mut frame.buffer[..], stride, format, age)
        };

        let prects: Vec<PRect> = damage.iter().copied().map(to_platform_rect).collect();
        display.present(&prects)?;
        Ok(true)
    }

    /// Copy the pending damage into a back buffer the event loop already
    /// acquired, returning the damage as platform rects.
    ///
    /// This is the form to call from inside a
    /// [`PlatformHandler::render`](fbui_platform::PlatformHandler::render): the
    /// loop owns the `Display` and hands you the [`Frame`], so you can't take a
    /// `&mut Display`. Paint into the surface first, then call this to blit out.
    pub fn copy_into_frame(&mut self, frame: &mut Frame<'_>) -> Vec<PRect> {
        let format = map_format(frame.format);
        let stride = frame.stride;
        let age = frame.age;
        let damage = self.present_to_buffer(&mut frame.buffer[..], stride, format, age);
        damage.into_iter().map(to_platform_rect).collect()
    }
}

/// Map the platform's pixel format to the renderer's copy-out format. RGB565 and
/// the two 32-bit layouts are the formats both layers agree on.
fn map_format(format: PixelFormat) -> TargetFormat {
    match format {
        PixelFormat::Xrgb8888 => TargetFormat::Xrgb8888,
        PixelFormat::Argb8888 => TargetFormat::Argb8888,
        PixelFormat::Rgb565 => TargetFormat::Rgb565,
        // `PixelFormat` is non-exhaustive; default any future format to the
        // 32-bit native path rather than fail to build downstream.
        _ => TargetFormat::Xrgb8888,
    }
}

/// Damage rects are byte-identical in shape between the two layers; this just
/// changes the type.
fn to_platform_rect(r: IRect) -> PRect {
    PRect::new(r.x, r.y, r.w, r.h)
}

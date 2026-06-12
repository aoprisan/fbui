//! On-device example (feature `platform`): drive the sample settings page onto a
//! real `fbui_platform::Display` through the event loop, with damage-tracked
//! incremental repaints.
//!
//! Run from a real text VT as root (or with `video`+`input` groups):
//!
//! ```text
//! cargo run -p fbui-render --example present --features platform
//! ```
//!
//! Up/Down move a highlight between rows; only the two changed toggles repaint,
//! so the copy-out is a few small spans, not the whole screen. Esc quits.

use fbui_platform::{
    keysym, Flow, Frame, InputEvent, KeyState, Platform, PlatformConfig, PlatformHandler, Rect,
};
use fbui_render::sample::settings_page;
use fbui_render::{FontContext, Scale, Surface};

const ROWS: usize = 6;

struct App {
    surface: Surface,
    fonts: FontContext,
    logical_w: f32,
    logical_h: f32,
    selected: usize,
    /// Repaint the whole page next frame (first frame, or after a resume).
    full: bool,
}

impl App {
    fn new(width: u32, height: u32) -> Self {
        App {
            surface: Surface::new(width, height, Scale::ONE),
            fonts: FontContext::new(),
            logical_w: width as f32,
            logical_h: height as f32,
            selected: 0,
            full: true,
        }
    }
}

impl PlatformHandler for App {
    fn on_input(&mut self, event: InputEvent) -> Flow {
        if let InputEvent::Key(k) = event {
            if k.state != KeyState::Pressed {
                return Flow::Continue;
            }
            match k.keysym {
                keysym::ESCAPE => return Flow::Exit,
                // Arrow keysyms aren't in the platform's small built-in table, so
                // accept j/k as well for keyboards routed through the US fallback.
                _ => {
                    if let Some(t) = k.utf8.as_deref() {
                        match t {
                            "k" => self.selected = self.selected.saturating_sub(1),
                            "j" => self.selected = (self.selected + 1).min(ROWS - 1),
                            _ => return Flow::Continue,
                        }
                        return Flow::Redraw;
                    }
                }
            }
        }
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame<'_>) -> Vec<Rect> {
        let (w, h) = (self.logical_w, self.logical_h);
        let sel = self.selected;
        // Always repaint the full page here for simplicity of the demo; the
        // surface's damage tracker still bounds the copy-out to what changed
        // versus the previous frame's shadow on a partial buffer age.
        if self.full {
            self.full = false;
            let fonts = &mut self.fonts;
            self.surface
                .repaint_full(|p| settings_page(p, fonts, w, h, Some(sel)));
        } else {
            let fonts = &mut self.fonts;
            self.surface
                .paint(|p| settings_page(p, fonts, w, h, Some(sel)));
        }
        self.surface.copy_into_frame(frame)
    }

    fn on_session(&mut self, active: bool) {
        if active {
            // Back buffers hold unknown contents after a VT switch; force a full
            // repaint on the next frame.
            self.full = true;
        }
    }
}

fn main() {
    let platform = match Platform::new(&PlatformConfig::default()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("present: could not bring up the platform: {e}");
            eprintln!("        (run from a real text VT as root or with video+input groups)");
            std::process::exit(1);
        }
    };
    let size = platform.info().size;
    eprintln!(
        "[present] {}x{} — j/k to move selection, Esc to quit",
        size.w, size.h
    );
    let mut app = App::new(size.w, size.h);
    if let Err(e) = platform.run(&mut app) {
        eprintln!("present: run error: {e}");
        std::process::exit(1);
    }
}

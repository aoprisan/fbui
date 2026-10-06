//! The bare-metal `Runner`, driven step by step the way a board's loop does:
//! input mapping (scale and rotation), damage copy-out, the idle rule, and
//! the `Timers` queue's interplay with `next_deadline`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use fbui_bare::{App, FbInfo, Framebuffer, Input, Rotation, Runner, Timer, Timers};
use fbui_render::geom::IRect;
use fbui_render::TargetFormat;
use fbui_widgets::event::Key;
use fbui_widgets::widgets::{Button, Container, Label};
use fbui_widgets::{Ui, WidgetId};

const FONT: &[u8] = include_bytes!("../../fbui-render/fonts/Inter-Regular.ttf");

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Pressed,
    Tick(u32),
    Fire(&'static str),
}

/// A button at the top of a column, plus a log of every message `update`
/// saw (shared, so a test can read it while the runner owns the app).
#[derive(Default)]
struct Probe {
    log: Rc<RefCell<Vec<Msg>>>,
    button: Option<WidgetId>,
    label: Option<WidgetId>,
    /// Armed in `on_start`, before the runner has seen any time.
    start_timer: Option<(Duration, Msg)>,
    timers: Option<Timers<Msg>>,
    /// Keys `on_key` swallows.
    eat: Option<Key>,
}

impl App for Probe {
    type Message = Msg;
    fn build(&mut self, ui: &mut Ui<Msg>) {
        let root = ui.set_root(Container::column().fill().padding(4.0).gap(4.0));
        let b = ui.add_child(root, Button::new("Go").on_press(|| Msg::Pressed));
        self.label = Some(ui.add_child(root, Label::new("0")));
        ui.focus(Some(b));
        self.button = Some(b);
    }
    fn on_start(&mut self, timers: Timers<Msg>) {
        if let Some((d, m)) = self.start_timer.take() {
            let _ = timers.send_after(d, m);
        }
        self.timers = Some(timers);
    }
    fn update(&mut self, msg: Msg, ui: &mut Ui<Msg>) {
        if let (Msg::Tick(n), Some(id)) = (&msg, self.label) {
            let t = n.to_string();
            ui.with::<Label, _>(id, move |l| l.set_text(t));
        }
        self.log.borrow_mut().push(msg);
    }
    fn on_key(&mut self, key: Key, _: &mut Ui<Msg>) -> bool {
        self.eat == Some(key)
    }
    fn fonts(&self) -> Vec<Vec<u8>> {
        vec![FONT.to_vec()]
    }
}

/// A RAM framebuffer with a padded stride (rows wider than the panel), which
/// records every `flush`.
struct Ram {
    w: u32,
    h: u32,
    stride: usize,
    px: Vec<u8>,
    flushed: Vec<Vec<IRect>>,
}

const PAD: usize = 12;
const FILL: u8 = 0xA5;

impl Ram {
    fn new(w: u32, h: u32) -> Self {
        let stride = w as usize * 4 + PAD;
        Ram {
            w,
            h,
            stride,
            px: vec![FILL; stride * h as usize],
            flushed: Vec::new(),
        }
    }
    /// XRGB8888 little-endian: B, G, R, X.
    fn rgb(&self, x: u32, y: u32) -> [u8; 3] {
        let o = y as usize * self.stride + x as usize * 4;
        [self.px[o + 2], self.px[o + 1], self.px[o]]
    }
}

impl Framebuffer for Ram {
    fn info(&self) -> FbInfo {
        FbInfo {
            width: self.w,
            height: self.h,
            stride: self.stride,
            format: TargetFormat::Xrgb8888,
        }
    }
    fn pixels(&mut self) -> &mut [u8] {
        &mut self.px
    }
    fn flush(&mut self, damage: &[IRect]) {
        self.flushed.push(damage.to_vec());
    }
}

fn probe() -> (Probe, Rc<RefCell<Vec<Msg>>>) {
    let p = Probe::default();
    let log = p.log.clone();
    (p, log)
}

fn tap(r: &mut Runner<Probe>, x: f32, y: f32, now: u64) {
    r.handle(Input::PointerDown { x, y }, now);
    r.handle(Input::PointerUp { x, y }, now + 20);
}

fn button_center(r: &mut Runner<Probe>) -> (f32, f32) {
    let id = r.app().button.unwrap();
    let b = r.ui().bounds(id).expect("button laid out");
    (b.x + b.w / 2.0, b.y + b.h / 2.0)
}

// --- the basic loop --------------------------------------------------------

#[test]
fn first_frame_fills_the_panel_then_the_runner_idles() {
    let (app, _) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);

    assert!(r.frame(&mut fb, 0), "first frame paints");
    assert_eq!(fb.flushed.last().unwrap(), &vec![IRect::new(0, 0, 120, 80)]);
    // Stride is never computed: the padding past each row stays untouched.
    for y in 0..80 {
        let row_end = y * fb.stride + 120 * 4;
        assert!(fb.px[row_end..row_end + PAD].iter().all(|&b| b == FILL));
    }

    assert!(!r.frame(&mut fb, 16), "nothing changed: no frame");
    assert_eq!(r.next_deadline(16), None, "idle sleeps until input");
}

#[test]
fn invalidate_recopies_the_whole_panel() {
    let (app, _) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    r.frame(&mut fb, 0);
    fb.px.fill(0); // a boot splash or panic screen drew over us
    r.invalidate();
    assert!(r.frame(&mut fb, 16));
    assert_eq!(fb.flushed.last().unwrap(), &vec![IRect::new(0, 0, 120, 80)]);
    assert_ne!(fb.rgb(0, 0), [0, 0, 0], "the theme background is back");
}

#[test]
fn key_tap_presses_the_focused_button_and_on_key_can_swallow() {
    let (mut app, log) = probe();
    app.eat = Some(Key::Escape);
    let fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);

    r.handle(Input::KeyTap(Key::Escape), 0);
    assert!(log.borrow().is_empty(), "on_key consumed Escape");
    r.handle(Input::KeyTap(Key::Enter), 10);
    assert_eq!(*log.borrow(), vec![Msg::Pressed]);
}

#[test]
fn pointer_input_is_scaled_to_logical_coordinates() {
    let (app, log) = probe();
    let mut fb = Ram::new(240, 160);
    let mut r = Runner::new(app, fb.info(), 2.0);
    r.frame(&mut fb, 0);
    let (cx, cy) = button_center(&mut r);
    // Device pixels are twice the logical ones.
    tap(&mut r, cx * 2.0, cy * 2.0, 100);
    assert_eq!(*log.borrow(), vec![Msg::Pressed]);
}

// --- rotation ---------------------------------------------------------------

#[test]
fn rotation_lays_out_portrait_and_copies_out_turned() {
    let (app, _) = probe();
    let mut fb = Ram::new(120, 80); // a landscape panel...
    let mut r = Runner::new(app, fb.info(), 1.0).with_rotation(Rotation::Rot90);
    assert_eq!(r.rotation(), Rotation::Rot90);
    // ...stood on its edge: the UI is portrait.
    assert_eq!(r.ui().size().w, 80.0);
    assert_eq!(r.ui().size().h, 120.0);

    assert!(r.frame(&mut fb, 0));
    // Flush rects are in panel space.
    assert_eq!(fb.flushed.last().unwrap(), &vec![IRect::new(0, 0, 120, 80)]);

    // Every panel pixel is the surface pixel the rotation maps onto it.
    let s = r.surface();
    let (sw, sh) = (s.width(), s.height());
    assert_eq!((sw, sh), (80, 120));
    let pm = s.pixmap();
    for sy in 0..sh {
        for sx in 0..sw {
            let c = pm.pixel(sx, sy).unwrap();
            let (px, py) = Rotation::Rot90.map_pixel(sx, sy, sw, sh);
            assert_eq!(
                fb.rgb(px, py),
                [c.red(), c.green(), c.blue()],
                "surface ({sx},{sy}) -> panel ({px},{py})"
            );
        }
    }
    // The stride padding is still untouched.
    for y in 0..80 {
        let row_end = y * fb.stride + 120 * 4;
        assert!(fb.px[row_end..row_end + PAD].iter().all(|&b| b == FILL));
    }
}

#[test]
fn rotated_pointer_input_maps_back_into_the_ui() {
    for rot in [Rotation::Rot90, Rotation::Rot180, Rotation::Rot270] {
        let (app, log) = probe();
        let (pw, ph) = (120.0, 80.0);
        let mut fb = Ram::new(120, 80);
        let mut r = Runner::new(app, fb.info(), 1.0).with_rotation(rot);
        r.frame(&mut fb, 0);
        let (ux, uy) = button_center(&mut r);
        let (uw, uh) = (r.ui().size().w, r.ui().size().h);

        // Where that UI point lands on the panel, worked out by hand for each
        // clockwise quarter turn.
        let (px, py) = match rot {
            Rotation::Rot90 => (pw - uy, ux),
            Rotation::Rot180 => (uw - ux, uh - uy),
            Rotation::Rot270 => (uy, ph - ux),
            Rotation::Rot0 => unreachable!(),
        };
        // The unrotated reading of that panel point misses the button, so
        // the test can tell the mapping happened.
        let id = r.app().button.unwrap();
        let b = r.ui().bounds(id).unwrap();
        assert!(
            !b.contains_point(fbui_render::geom::Point::new(px, py)),
            "{rot:?}"
        );

        tap(&mut r, px, py, 100);
        assert_eq!(*log.borrow(), vec![Msg::Pressed], "{rot:?}");
    }
}

#[test]
fn rotating_at_runtime_relays_out_and_repaints_everything() {
    let (app, _) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    r.frame(&mut fb, 0);
    assert!(!r.frame(&mut fb, 16));

    r.set_rotation(Rotation::Rot180);
    assert_eq!(r.ui().size().w, 120.0, "a half turn keeps the shape");
    assert!(r.frame(&mut fb, 32), "the new orientation is painted");
    assert_eq!(fb.flushed.last().unwrap(), &vec![IRect::new(0, 0, 120, 80)]);

    r.set_rotation(Rotation::Rot180);
    assert!(!r.frame(&mut fb, 48), "same rotation again is a no-op");
}

// --- RGB565 dithering ------------------------------------------------------

/// An RGB565 panel with a padded stride, recording flushes.
struct Ram565 {
    w: u32,
    h: u32,
    stride: usize,
    px: Vec<u8>,
    flushed: Vec<Vec<IRect>>,
}

impl Ram565 {
    fn new(w: u32, h: u32) -> Self {
        let stride = w as usize * 2 + PAD;
        Ram565 {
            w,
            h,
            stride,
            px: vec![FILL; stride * h as usize],
            flushed: Vec::new(),
        }
    }
    fn at(&self, x: u32, y: u32) -> u16 {
        let o = y as usize * self.stride + x as usize * 2;
        u16::from_le_bytes([self.px[o], self.px[o + 1]])
    }
    /// The distinct values in the panel's bottom-left 4×4 block — one whole
    /// Bayer period, and in the root container's padding (flat background)
    /// whichever way the UI is turned.
    fn corner_values(&self) -> Vec<u16> {
        let mut v: Vec<u16> = (self.h - 4..self.h)
            .flat_map(|y| (0..4).map(move |x| (x, y)))
            .map(|(x, y)| self.at(x, y))
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

impl Framebuffer for Ram565 {
    fn info(&self) -> FbInfo {
        FbInfo {
            width: self.w,
            height: self.h,
            stride: self.stride,
            format: TargetFormat::Rgb565,
        }
    }
    fn pixels(&mut self) -> &mut [u8] {
        &mut self.px
    }
    fn flush(&mut self, damage: &[IRect]) {
        self.flushed.push(damage.to_vec());
    }
}

/// Split RGB565 into its 5/6/5-bit channels.
fn channels(v: u16) -> [i32; 3] {
    [
        (v >> 11) as i32,
        ((v >> 5) & 0x3f) as i32,
        (v & 0x1f) as i32,
    ]
}

#[test]
fn rgb565_panels_are_dithered_by_default_and_32_bit_ones_are_not() {
    let (app, _) = probe();
    assert!(!Runner::new(app, Ram::new(120, 80).info(), 1.0).dither());

    let (app, _) = probe();
    let mut fb = Ram565::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    assert!(r.dither());
    r.frame(&mut fb, 0);
    // The dark theme's background (#14161b) has no exact RGB565 value, so the
    // flat area becomes a pattern of the neighbouring values.
    let dithered = fb.corner_values();
    assert!(
        dithered.len() > 1,
        "flat background is dithered: {dithered:x?}"
    );
    for y in 0..80 {
        let row_end = y * fb.stride + 120 * 2;
        assert!(fb.px[row_end..row_end + PAD].iter().all(|&b| b == FILL));
    }

    // Off, the same area is the plain truncation: one value, and each
    // dithered value is within one step of it per channel.
    let (app, _) = probe();
    let mut plain_fb = Ram565::new(120, 80);
    let mut plain = Runner::new(app, plain_fb.info(), 1.0);
    plain.set_dither(false);
    plain.frame(&mut plain_fb, 0);
    let flat = plain_fb.corner_values();
    assert_eq!(flat.len(), 1, "undithered background is flat: {flat:x?}");
    let base = channels(flat[0]);
    for v in dithered {
        let c = channels(v);
        assert!(
            (0..3).all(|i| (c[i] - base[i]).abs() <= 1),
            "{v:04x} vs {:04x}",
            flat[0]
        );
    }
}

#[test]
fn dithering_survives_rotation_and_toggling_repaints_the_panel() {
    let (app, _) = probe();
    let mut fb = Ram565::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0).with_rotation(Rotation::Rot90);
    assert!(r.dither(), "the rotated surface keeps the setting");
    r.frame(&mut fb, 0);
    assert!(fb.corner_values().len() > 1);
    assert!(!r.frame(&mut fb, 16));

    r.set_dither(false);
    assert!(r.frame(&mut fb, 32), "the new copy-out reaches the panel");
    assert_eq!(fb.flushed.last().unwrap(), &vec![IRect::new(0, 0, 120, 80)]);
    assert_eq!(fb.corner_values().len(), 1);

    r.set_dither(false);
    assert!(!r.frame(&mut fb, 48), "unchanged setting: nothing to do");
    // And a later rotation doesn't turn it back on.
    r.set_rotation(Rotation::Rot0);
    r.frame(&mut fb, 64);
    assert_eq!(fb.corner_values().len(), 1);
}

// --- timers -----------------------------------------------------------------

fn timers(r: &Runner<Probe>) -> Timers<Msg> {
    r.app().timers.clone().expect("on_start ran")
}

#[test]
fn send_after_fires_on_the_first_frame_at_its_deadline_and_wakes_the_board() {
    let (app, log) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    r.frame(&mut fb, 1_000);
    assert_eq!(r.next_deadline(1_000), None);

    let _t = timers(&r).send_after(Duration::from_millis(500), Msg::Tick(1));
    // The board may sleep exactly until the timer, not forever.
    assert_eq!(r.next_deadline(1_000), Some(1_500));

    assert!(!r.frame(&mut fb, 1_499), "not due yet");
    assert!(log.borrow().is_empty());

    assert!(r.frame(&mut fb, 1_500), "the update it caused paints now");
    assert_eq!(*log.borrow(), vec![Msg::Tick(1)]);
    assert_eq!(r.next_deadline(1_500), None, "one-shot is gone; idle again");
}

#[test]
fn timers_armed_before_any_clock_reading_count_from_the_first() {
    let (mut app, log) = probe();
    app.start_timer = Some((Duration::from_millis(100), Msg::Fire("start")));
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    // The board clock started long before the runner did.
    r.frame(&mut fb, 50_000);
    assert!(
        log.borrow().is_empty(),
        "did not fire early on a late clock"
    );
    assert_eq!(r.next_deadline(50_000), Some(50_100));
    r.frame(&mut fb, 50_100);
    assert_eq!(*log.borrow(), vec![Msg::Fire("start")]);
}

#[test]
fn send_every_repeats_fixed_delay_and_cancel_stops_it() {
    let (app, log) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    r.frame(&mut fb, 0);
    let t: Timer = timers(&r).send_every(Duration::from_millis(100), Msg::Tick(7));

    r.frame(&mut fb, 100);
    r.frame(&mut fb, 200);
    assert_eq!(log.borrow().len(), 2);
    // A long stall: one delivery on catch-up, re-armed a period after now.
    r.frame(&mut fb, 1_000);
    assert_eq!(log.borrow().len(), 3);
    // (The tick's repaint happened inside that frame, so only the timer is
    // left to wake for.)
    assert_eq!(r.next_deadline(1_000), Some(1_100));

    t.cancel();
    assert_eq!(r.next_deadline(1_000), None);
    r.frame(&mut fb, 5_000);
    assert_eq!(log.borrow().len(), 3);
}

#[test]
fn messages_fire_in_deadline_order_and_dropping_a_handle_detaches() {
    let (app, log) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    r.frame(&mut fb, 0);
    let q = timers(&r);
    drop(q.send_after(Duration::from_millis(30), Msg::Fire("c")));
    let _ = q.send_after(Duration::from_millis(10), Msg::Fire("a"));
    q.send_after(Duration::from_millis(20), Msg::Fire("b"))
        .cancel();
    let _ = q.send_after(Duration::from_millis(10), Msg::Fire("a2"));

    r.frame(&mut fb, 100);
    assert_eq!(
        *log.borrow(),
        vec![Msg::Fire("a"), Msg::Fire("a2"), Msg::Fire("c")]
    );
}

#[test]
fn send_from_board_code_is_delivered_on_the_next_frame() {
    let (app, log) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    r.frame(&mut fb, 0);
    // A card-detect interrupt flagged something; the board loop posts it.
    r.timers().send(Msg::Fire("card"));
    assert_eq!(r.next_deadline(40), Some(40), "wake now, don't sleep");
    r.frame(&mut fb, 40);
    assert_eq!(*log.borrow(), vec![Msg::Fire("card")]);
}

#[test]
fn a_pending_timer_does_not_shorten_the_animation_frame_period() {
    let (app, _) = probe();
    let mut fb = Ram::new(120, 80);
    let mut r = Runner::new(app, fb.info(), 1.0);
    r.frame(&mut fb, 0);
    let _t = timers(&r).send_after(Duration::from_secs(10), Msg::Tick(1));
    // A gesture in flight wants frame-rate wake-ups; the far timer must not
    // push that out...
    r.handle(Input::PointerDown { x: 1.0, y: 1.0 }, 0);
    let d = r.next_deadline(0).unwrap();
    assert!(
        d < 100,
        "frame-rate deadline while a gesture is active: {d}"
    );
    // ...and once the UI settles, the timer bounds the idle sleep.
    r.handle(Input::PointerUp { x: 1.0, y: 1.0 }, 20);
    r.frame(&mut fb, 20);
    r.frame(&mut fb, 1_000); // past the long-press window: gesture over
    assert_eq!(r.next_deadline(1_000), Some(10_000));
}

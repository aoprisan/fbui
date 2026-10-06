//! The viewer driven exactly as a board drives it — `fbui_bare::Runner`
//! over a RAM framebuffer — with the fbui-doc fixtures as its documents.

use fbui_bare::{FbInfo, Framebuffer, Input, Runner};
use fbui_doc_viewer::{Entry, Screen, Viewer, Zoom};
use fbui_render::TargetFormat;
use fbui_widgets::event::Key;

const W: u32 = 640;
const H: u32 = 480;
const FONT: &[u8] = include_bytes!("../../../fbui-render/fonts/Inter-Regular.ttf");

struct Ram(Vec<u8>);

impl Framebuffer for Ram {
    fn info(&self) -> FbInfo {
        // Padded rows, as real controllers do: stride is never width * 4.
        FbInfo {
            width: W,
            height: H,
            stride: W as usize * 4 + 64,
            format: TargetFormat::Xrgb8888,
        }
    }
    fn pixels(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

fn fixture(name: &str) -> Entry {
    let path = format!(
        "{}/../../fbui-doc/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    Entry::new(name, std::fs::read(path).unwrap())
}

struct Rig {
    runner: Runner<Viewer>,
    fb: Ram,
    now: u64,
}

impl Rig {
    fn new(entries: Vec<Entry>) -> Rig {
        let fb = Ram(vec![0; (W as usize * 4 + 64) * H as usize]);
        let runner = Runner::new(Viewer::new(entries, FONT), fb.info(), 1.0);
        let mut rig = Rig { runner, fb, now: 0 };
        rig.settle();
        rig
    }

    fn key(&mut self, k: Key) {
        self.now += 16;
        self.runner.handle(Input::KeyTap(k), self.now);
        self.settle();
    }

    fn settle(&mut self) {
        for _ in 0..20 {
            self.now += 16;
            self.runner.frame(&mut self.fb, self.now);
        }
    }

    fn tree(&mut self) -> String {
        self.runner.ui().inspect_text()
    }

    /// The framebuffer pixel at (x, y) as RGB.
    fn px(&self, x: u32, y: u32) -> (u8, u8, u8) {
        let i = y as usize * (W as usize * 4 + 64) + x as usize * 4;
        (self.fb.0[i + 2], self.fb.0[i + 1], self.fb.0[i])
    }

    /// White pixels in the page area — "is a page on screen".
    fn paper(&self) -> usize {
        (60..H)
            .step_by(4)
            .flat_map(|y| (0..W).step_by(4).map(move |x| (x, y)))
            .filter(|&(x, y)| self.px(x, y) == (255, 255, 255))
            .count()
    }
}

#[test]
fn library_then_paging_then_back() {
    let mut rig = Rig::new(vec![
        fixture("fpdf2.pdf"),
        fixture("reportlab.pdf"),
        fixture("swatch.png"),
    ]);
    assert_eq!(rig.runner.app().screen(), Screen::Library);
    let tree = rig.tree();
    assert!(tree.contains("3 documents"), "{tree}");
    assert_eq!(rig.paper(), 0, "no page in the library");

    rig.key(Key::Enter);
    assert_eq!(rig.runner.app().screen(), Screen::Document);
    assert_eq!(rig.runner.app().position(), Some((0, 2)));
    assert!(rig.paper() > 2000, "page 1 is on screen");
    assert!(rig.tree().contains("1 / 2"));

    rig.key(Key::Right);
    assert_eq!(rig.runner.app().position(), Some((1, 2)));
    assert!(rig.tree().contains("2 / 2"));
    // Past the end: stays.
    rig.key(Key::PageDown);
    assert_eq!(rig.runner.app().position(), Some((1, 2)));
    rig.key(Key::Home);
    assert_eq!(rig.runner.app().position(), Some((0, 2)));

    // Back to the library, pick the PNG (third row).
    rig.key(Key::Escape);
    assert_eq!(rig.runner.app().screen(), Screen::Library);
    for _ in 0..3 {
        rig.key(Key::Down);
    }
    rig.key(Key::Enter);
    assert_eq!(rig.runner.app().position(), Some((0, 1)));
    assert!(rig.tree().contains("swatch.png"));
}

#[test]
fn zoom_steps_and_fit_modes() {
    let mut rig = Rig::new(vec![fixture("reportlab.pdf")]);
    // A single document opens straight away.
    assert_eq!(rig.runner.app().screen(), Screen::Document);
    assert_eq!(rig.runner.app().zoom(), Zoom::FitWidth);
    rig.key(Key::Char('+'));
    assert_eq!(rig.runner.app().zoom(), Zoom::Step(3));
    rig.key(Key::Char('-'));
    rig.key(Key::Char('-'));
    assert_eq!(rig.runner.app().zoom(), Zoom::Step(1));
    rig.key(Key::Char('p'));
    assert_eq!(rig.runner.app().zoom(), Zoom::FitPage);
    assert!(rig.tree().contains("(page)"));
    rig.key(Key::Char('w'));
    assert_eq!(rig.runner.app().zoom(), Zoom::FitWidth);
}

#[test]
fn scrolling_then_space_turns_the_page() {
    let mut rig = Rig::new(vec![fixture("fpdf2.pdf")]);
    // Fit-width page 1 is taller than the screen: Space scrolls first…
    rig.key(Key::Space);
    assert_eq!(rig.runner.app().position(), Some((0, 2)));
    // …and turns the page once at the bottom.
    let mut presses = 1;
    while rig.runner.app().position() == Some((0, 2)) && presses < 10 {
        rig.key(Key::Space);
        presses += 1;
    }
    assert_eq!(rig.runner.app().position(), Some((1, 2)));
    assert!(
        presses > 2,
        "the page should take more than one screen ({presses})"
    );
    // Up at the top of page 2 goes back to the end of page 1.
    rig.key(Key::Up);
    assert_eq!(rig.runner.app().position(), Some((0, 2)));
}

#[test]
fn a_broken_document_reports_instead_of_crashing() {
    let mut rig = Rig::new(vec![
        Entry::new("garbage.pdf", b"%PDF-1.4 nothing here".to_vec()),
        Entry::new("notes.txt", b"plain text".to_vec()),
    ]);
    rig.key(Key::Enter);
    assert_eq!(rig.runner.app().screen(), Screen::Library);
    assert!(
        rig.tree().contains("garbage.pdf: pdf has no pages"),
        "{}",
        rig.tree()
    );
    rig.key(Key::Down);
    rig.key(Key::Down);
    rig.key(Key::Enter);
    assert!(
        rig.tree().contains("notes.txt: unsupported"),
        "{}",
        rig.tree()
    );
}

#[test]
fn idle_costs_no_frames() {
    let mut rig = Rig::new(vec![fixture("reportlab.pdf")]);
    rig.now += 16;
    assert!(
        !rig.runner.frame(&mut rig.fb, rig.now),
        "nothing changed, nothing painted"
    );
    assert_eq!(
        rig.runner.next_deadline(rig.now),
        None,
        "idle: sleep until input"
    );
}

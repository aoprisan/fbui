//! Drive the viewer on the host — same `fbui_bare::Runner` path a board
//! uses, with a RAM framebuffer — and save what the "screen" shows after a
//! scripted key sequence. The headless way to see a change to the app.
//!
//! ```sh
//! cargo run -p fbui-doc-viewer --features std --example shots -- OUT_DIR [FILE...]
//! ```
//!
//! With no files it loads the fbui-doc test fixtures.

use fbui_bare::{FbInfo, Framebuffer, Input, Runner};
use fbui_doc_viewer::{Entry, Viewer};
use fbui_render::TargetFormat;
use fbui_widgets::event::Key;

const W: u32 = 800;
const H: u32 = 600;

struct Ram(Vec<u8>);

impl Framebuffer for Ram {
    fn info(&self) -> FbInfo {
        FbInfo {
            width: W,
            height: H,
            stride: W as usize * 4,
            format: TargetFormat::Xrgb8888,
        }
    }
    fn pixels(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

fn save(fb: &Ram, path: &std::path::Path) {
    // XRGB8888 little-endian: B, G, R, X.
    let rgb: Vec<u8> =
        fb.0.chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect();
    image::save_buffer(path, &rgb, W, H, image::ExtendedColorType::Rgb8).unwrap();
    eprintln!("wrote {}", path.display());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = std::path::PathBuf::from(args.first().expect("usage: shots OUT_DIR [FILE...]"));
    std::fs::create_dir_all(&out).unwrap();
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fbui-doc/tests/fixtures");
    let files: Vec<String> = if args.len() > 1 {
        args[1..].to_vec()
    } else {
        ["fpdf2.pdf", "reportlab.pdf"]
            .iter()
            .map(|f| format!("{fixtures}/{f}"))
            .collect()
    };
    let entries = files
        .iter()
        .map(|f| {
            let name = std::path::Path::new(f)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            Entry::new(name, std::fs::read(f).unwrap())
        })
        .collect();
    let font: &'static [u8] = include_bytes!("../../../fbui-render/fonts/Inter-Regular.ttf");
    let mut fb = Ram(vec![0; (W * H * 4) as usize]);
    let mut now = 0u64;
    let mut runner = Runner::new(Viewer::new(entries, font), fb.info(), 1.0);
    let mut step = |runner: &mut Runner<Viewer>, fb: &mut Ram, keys: &[Key], name: &str| {
        for &k in keys {
            now += 16;
            let t = std::time::Instant::now();
            runner.handle(Input::KeyTap(k), now);
            eprintln!("{k:?}: {:?}", t.elapsed());
        }
        for _ in 0..30 {
            now += 16;
            runner.frame(fb, now);
        }
        save(fb, &out.join(name));
    };
    step(&mut runner, &mut fb, &[], "1-library.png");
    step(&mut runner, &mut fb, &[Key::Enter], "2-open.png");
    step(&mut runner, &mut fb, &[Key::Right], "3-page2.png");
    step(
        &mut runner,
        &mut fb,
        &[Key::Char('+'), Key::Char('+'), Key::Down, Key::Down],
        "4-zoomed.png",
    );
    step(&mut runner, &mut fb, &[Key::Char('p')], "5-fit-page.png");
    step(
        &mut runner,
        &mut fb,
        &[Key::Escape, Key::Down, Key::Down, Key::Enter],
        "6-second-doc.png",
    );
}

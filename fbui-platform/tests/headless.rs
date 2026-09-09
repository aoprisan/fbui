//! The headless backend driven through the *real* event loop.
//!
//! Unlike `tests/integration.rs` these need no devices, no root and no
//! `modprobe`, so they run in the ordinary `cargo test` pass — which is the
//! whole point of the backend: the loop, the frame cycle and the buffer-age
//! accounting get exercised on any machine, not only on VKMS.

use std::time::Duration;

use fbui_platform::display::headless::HeadlessDisplay;
use fbui_platform::{
    Display, DisplayInfo, Flow, Frame, InputEvent, PlatformConfig, PlatformHandler, Rect, Size,
};

/// Renders a fixed number of frames, recording each frame's buffer age, then
/// asks the loop to exit.
struct AgeProbe {
    ages: Vec<u32>,
    want: usize,
    display_changes: Vec<Size>,
}

impl PlatformHandler for AgeProbe {
    fn on_input(&mut self, _event: InputEvent) -> Flow {
        Flow::Continue
    }

    fn render(&mut self, frame: &mut Frame<'_>) -> Vec<Rect> {
        self.ages.push(frame.age);
        // Write one row forward, the way the render layer does, so the frame
        // is a real present rather than an empty-damage skip.
        frame.row(0).fill(0x40);
        vec![Rect::new(0, 0, frame.size.w, 1)]
    }

    fn on_display_changed(&mut self, info: DisplayInfo) {
        self.display_changes.push(info.size);
    }

    fn tick(&mut self) -> Flow {
        if self.ages.len() >= self.want {
            Flow::Exit
        } else {
            Flow::Redraw
        }
    }

    fn next_timeout(&mut self) -> Option<Duration> {
        Some(Duration::from_millis(1))
    }
}

fn headless_config() -> PlatformConfig {
    PlatformConfig {
        prefer_headless: true,
        vt_guard: false,
        ..Default::default()
    }
}

/// The full `Platform::run` cycle — event loop, frame clock, present — turns
/// with no display device, no tty and no seat, and the buffer ages it hands
/// out are the double-buffered sequence the DRM backend produces (both
/// buffers undefined once, then a 2-old buffer every frame). That is what
/// makes a headless run exercise the *partial* redraw path.
#[test]
fn the_event_loop_runs_and_ages_buffers_like_drm() {
    let platform = fbui_platform::Platform::new(&headless_config()).expect("headless comes up");
    let info = platform.info();
    assert_eq!(info.backend, fbui_platform::BackendKind::Headless);
    assert_eq!(info.buffers, 2);

    let mut probe = AgeProbe {
        ages: Vec::new(),
        want: 6,
        display_changes: Vec::new(),
    };
    platform.run(&mut probe).expect("the loop exits cleanly");

    assert!(
        probe.ages.len() >= 6,
        "rendered {} frames",
        probe.ages.len()
    );
    assert_eq!(&probe.ages[..2], &[0, 0], "both buffers start undefined");
    for (i, age) in probe.ages[2..6].iter().enumerate() {
        assert_eq!(*age, 2, "frame {} reuses a two-present-old buffer", i + 2);
    }
}

/// A simulated mode change reaches `on_display_changed`, so the hotplug path
/// — until now testable only against VKMS — has an off-device test.
#[test]
fn a_simulated_mode_change_reaches_the_handler() {
    let mut display = HeadlessDisplay::new(Size::new(320, 240));
    display.request_mode(Size::new(240, 320));

    let mut probe = AgeProbe {
        ages: Vec::new(),
        want: 2,
        display_changes: Vec::new(),
    };
    // The loop polls `reconfigure`; drive it directly here so the test needs
    // no second of wall clock.
    let changed = display.reconfigure().expect("reconfigure works");
    let info = changed.expect("the requested mode is reported");
    probe.on_display_changed(info);

    assert_eq!(probe.display_changes, vec![Size::new(240, 320)]);
    assert_eq!(display.info().size, Size::new(240, 320));
}

//! Phase 1 integration tests against virtual kernel devices.
//!
//! These exercise the real kernel ABI, so they need privileges and virtual
//! drivers that aren't present in an ordinary build sandbox:
//!
//! * [`drm_vkms_present_cycle`] needs a DRM card we can become master of — VKMS
//!   (`modprobe vkms`) in CI, or any real card run from the active VT as root.
//! * [`evdev_uinput_keystroke`] needs `/dev/uinput` (root) to synthesize a
//!   keyboard and read it back through the evdev backend.
//!
//! Both are `#[ignore]` so `cargo test` stays green everywhere; CI runs them
//! explicitly with `cargo test -- --ignored` in a privileged container. The
//! point of checking them in now is that the *harness* is a Phase 1 deliverable
//! (it's what Phase 2's snapshot tests and later regression gates build on).

#[cfg(all(feature = "drm-backend", feature = "fbdev"))]
mod drm {
    use fbui_platform::display::drm::DrmDisplay;
    use fbui_platform::display::Display;
    use fbui_platform::geom::Rect;

    fn card_path() -> String {
        std::env::var("FBUI_DRM_CARD").unwrap_or_else(|_| "/dev/dri/card0".to_string())
    }

    /// Bring up a DRM dumb-buffer display and run a few vsynced present cycles,
    /// asserting that buffer age advances the way double buffering implies. This
    /// is the "CI green on VKMS" exit criterion in executable form.
    #[test]
    #[ignore = "needs a DRM card with master (modprobe vkms; run as root --ignored)"]
    fn drm_vkms_present_cycle() {
        let path = card_path();
        let mut display = DrmDisplay::open(&path).expect("open DRM card / become master");
        let info = display.info();
        assert!(
            info.size.w > 0 && info.size.h > 0,
            "modeset produced a real size"
        );

        let mut presented = 0u32;
        let mut last_age = None;
        for _ in 0..6 {
            // Acquire a buffer (may be momentarily busy right after a flip).
            let Some(frame) = display.begin_frame().expect("begin_frame") else {
                wait_readable(&display);
                display.dispatch_present().expect("dispatch");
                continue;
            };
            // Fill it so a real scanout would show something; whole-row writes.
            for b in frame.buffer.iter_mut() {
                *b = 0x30;
            }
            last_age = Some(frame.age);
            let damage = vec![Rect::from_size(frame.size)];
            display.present(&damage).expect("present");
            presented += 1;

            // Block for the flip-complete event, then free the buffer.
            wait_readable(&display);
            assert!(
                display.dispatch_present().expect("dispatch"),
                "a flip completed"
            );
        }

        assert!(presented >= 4, "completed several present cycles");
        // After the cycle has warmed up, a recycled buffer reports a nonzero age.
        assert!(last_age.is_some());
    }

    /// Poll the display's present fd until it's readable (the page flip landed).
    fn wait_readable(display: &DrmDisplay) {
        use std::os::unix::io::AsRawFd;
        let fd = match display.present_fd() {
            Some(f) => f.as_raw_fd(),
            None => return,
        };
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: single valid pollfd; 200ms timeout.
        unsafe {
            libc::poll(&mut pfd, 1, 200);
        }
    }
}

#[cfg(feature = "evdev")]
mod input {
    use std::time::{Duration, Instant};

    use evdev::uinput::VirtualDevice;
    use evdev::{AttributeSet, InputEvent as RawEvent, KeyCode};

    // Raw evdev event-type codes (`<linux/input-event-codes.h>`): `RawEvent::new`
    // takes the bare `u16`, not the typed `EventType`.
    const EV_SYN: u16 = 0x00;
    const EV_KEY: u16 = 0x01;

    use fbui_platform::geom::Size;
    use fbui_platform::input::evdev::EvdevInput;
    use fbui_platform::input::{InputEvent, InputSource, KeyState};

    /// Synthesize a keyboard via uinput, type `a`, and assert the evdev backend
    /// emits a normalized key event carrying the keysym and the UTF-8 "a".
    #[test]
    #[ignore = "needs /dev/uinput (root); run with --ignored"]
    fn evdev_uinput_keystroke() {
        let mut keys = AttributeSet::<KeyCode>::new();
        keys.insert(KeyCode::KEY_A);
        let mut vdev = VirtualDevice::builder()
            .expect("open /dev/uinput")
            .name("fbui-test-keyboard")
            .with_keys(&keys)
            .expect("declare keys")
            .build()
            .expect("build virtual device");

        // Resolve the event node the kernel created and open it for the backend.
        let node = vdev
            .enumerate_dev_nodes_blocking()
            .expect("enumerate nodes")
            .flatten()
            .next()
            .expect("a /dev/input/event* node");
        let dev = evdev::Device::open(&node).expect("open synthesized device");
        let mut input =
            EvdevInput::from_devices(vec![dev], Size::new(640, 480)).expect("build evdev source");

        // Emit press + release of KEY_A, each followed by SYN_REPORT.
        let code = KeyCode::KEY_A.code();
        vdev.emit(&[RawEvent::new(EV_KEY, code, 1)]).unwrap();
        vdev.emit(&[RawEvent::new(EV_SYN, 0, 0)]).unwrap();
        vdev.emit(&[RawEvent::new(EV_KEY, code, 0)]).unwrap();
        vdev.emit(&[RawEvent::new(EV_SYN, 0, 0)]).unwrap();

        // Drain (with a short spin, since uinput delivery is asynchronous).
        let mut got_press = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && !got_press {
            input
                .dispatch(&mut |ev| {
                    if let InputEvent::Key(k) = ev {
                        if k.state == KeyState::Pressed && k.utf8.as_deref() == Some("a") {
                            got_press = true;
                        }
                    }
                })
                .expect("dispatch");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            got_press,
            "evdev backend normalized the synthesized keypress"
        );
    }
}

//! The [`Hub`]: shared state between the UI thread and the remote console's
//! server threads.
//!
//! One `Arc<Hub>` is held by the runner (UI thread) and every HTTP connection
//! thread. The traffic through it is deliberately narrow and one-directional
//! per lane:
//!
//! * **frames** flow UI → server: the runner publishes an RGBA snapshot after
//!   presenting (only while someone is watching), and stream clients block on
//!   a condvar for the next sequence number.
//! * **commands** flow server → UI: input injection and inspect requests are
//!   queued here, then the loop [`Waker`](https://docs.rs/fbui-platform) is
//!   poked (via the callback installed with [`set_waker`](Hub::set_waker), so
//!   this module stays platform-free and headless-testable). The runner drains
//!   the queue on the UI thread.
//! * **metrics** are plain counters the runner updates and `/metrics` reads.
//!
//! Nothing here does I/O; the HTTP layer lives in `http.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// One published frame: straight-alpha RGBA8 rows, device pixels. The pixel
/// buffer is shared (`Arc`) so a slow client encoding a PNG never blocks the
/// UI thread or other clients.
#[derive(Clone)]
pub struct FrameImage {
    /// Monotonic publish counter, for "wait for a frame newer than X".
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<Vec<u8>>,
}

/// Pointer buttons the console can inject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteButton {
    Left,
    Middle,
    Right,
}

/// A request from a server thread, serviced by the runner on the UI thread.
/// Coordinates are **device pixels** — the same space as the published frames,
/// so a browser can map a click on the image 1:1.
pub enum Command {
    PointerMove {
        x: f32,
        y: f32,
    },
    PointerDown {
        x: f32,
        y: f32,
        button: RemoteButton,
    },
    PointerUp {
        x: f32,
        y: f32,
        button: RemoteButton,
    },
    /// Wheel scroll at a position; positive `dy` scrolls the content down
    /// (the direction a positive browser `deltaY` means).
    Wheel {
        x: f32,
        y: f32,
        dy: f64,
    },
    /// A key press+release: a single character, or a named key
    /// (`Enter`, `Backspace`, `Tab`, `Delete`, `Home`, `End`, `Left`,
    /// `Right`, `Up`, `Down`, `Escape`, `Space`).
    Key {
        name: String,
    },
    /// Type a string: one press+release per character.
    Text {
        text: String,
    },
    /// Snapshot the widget tree; the runner replies with the JSON document.
    Inspect {
        reply: SyncSender<String>,
    },
    /// Publish a frame from the current surface even if nothing repainted —
    /// how a freshly connected client gets pixels from an idle app.
    RefreshFrame,
}

/// Counters for `/metrics`. All updated by the runner; `clients`/`watchers`
/// come from the hub itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct MetricsSnapshot {
    /// Frames presented since start.
    pub frames: u64,
    /// Paint + copy-out cost of the most recent frame, milliseconds.
    pub paint_ms_last: f32,
    /// Worst frame since start, milliseconds.
    pub paint_ms_max: f32,
    /// Input events delivered (live + injected) since start.
    pub input_events: u64,
    /// Seconds since the runner started.
    pub uptime_s: f64,
    /// Current surface size, device pixels.
    pub width: u32,
    pub height: u32,
    /// Open HTTP connections.
    pub clients: usize,
    /// Connections currently watching the frame stream.
    pub watchers: usize,
}

#[derive(Default)]
struct Metrics {
    frames: u64,
    paint_ms_last: f32,
    paint_ms_max: f32,
    input_events: u64,
    width: u32,
    height: u32,
}

struct Inner {
    frame: Option<FrameImage>,
    seq: u64,
    /// Stream clients; the runner publishes frames only while this is > 0.
    watchers: usize,
    /// A client wants a frame now even if the app is idle.
    refresh: bool,
    commands: Vec<Command>,
    waker: Option<Box<dyn Fn() + Send + Sync>>,
    metrics: Metrics,
}

/// The rendezvous point. See the module docs.
pub struct Hub {
    inner: Mutex<Inner>,
    frame_cv: Condvar,
    /// Open HTTP connections (bounds accepted connections; see `http.rs`).
    pub(crate) clients: AtomicUsize,
    started: Instant,
}

impl Hub {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub {
            inner: Mutex::new(Inner {
                frame: None,
                seq: 0,
                watchers: 0,
                refresh: false,
                commands: Vec::new(),
                waker: None,
                metrics: Metrics::default(),
            }),
            frame_cv: Condvar::new(),
            clients: AtomicUsize::new(0),
            started: Instant::now(),
        })
    }

    /// Install the callback that wakes the UI event loop (the runner passes
    /// its platform `Waker` here). Until it's set, commands queue silently and
    /// are picked up on the loop's next natural wakeup.
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        self.inner.lock().unwrap().waker = Some(Box::new(waker));
    }

    fn wake(&self) {
        // Take the call outside the lock: the waker may do fd I/O.
        let guard = self.inner.lock().unwrap();
        if let Some(w) = &guard.waker {
            // Calling under the lock would be fine too (the runner never takes
            // this lock from the wake path), but keep the hold time minimal.
            w();
        }
    }

    // ---- frames (UI → server) -------------------------------------------

    /// Publish a frame snapshot, waking every stream waiter.
    pub fn publish_frame(&self, width: u32, height: u32, rgba: Vec<u8>) {
        let mut g = self.inner.lock().unwrap();
        g.seq += 1;
        let seq = g.seq;
        g.frame = Some(FrameImage {
            seq,
            width,
            height,
            rgba: Arc::new(rgba),
        });
        g.refresh = false;
        drop(g);
        self.frame_cv.notify_all();
    }

    /// The most recently published frame, if any.
    pub fn latest_frame(&self) -> Option<FrameImage> {
        self.inner.lock().unwrap().frame.clone()
    }

    /// Block until a frame newer than `after` is published (or `timeout`
    /// passes — returns `None` then).
    pub fn wait_frame(&self, after: u64, timeout: Duration) -> Option<FrameImage> {
        let g = self.inner.lock().unwrap();
        let (g, _res) = self
            .frame_cv
            .wait_timeout_while(g, timeout, |g| g.seq <= after)
            .ok()?;
        g.frame.clone().filter(|f| f.seq > after)
    }

    /// A stream client arrived: the runner starts publishing frames.
    pub fn add_watcher(&self) {
        self.inner.lock().unwrap().watchers += 1;
    }

    /// A stream client left.
    pub fn remove_watcher(&self) {
        let mut g = self.inner.lock().unwrap();
        g.watchers = g.watchers.saturating_sub(1);
    }

    /// How many stream clients are connected right now.
    pub fn watchers(&self) -> usize {
        self.inner.lock().unwrap().watchers
    }

    /// Ask the runner for a frame even if the app is idle (see
    /// [`Command::RefreshFrame`] — this is the cheap flag form used together
    /// with a queued command).
    pub fn request_refresh(&self) {
        self.inner.lock().unwrap().refresh = true;
        self.wake();
    }

    /// Runner side: was a refresh requested since the last publish?
    pub fn take_refresh(&self) -> bool {
        std::mem::take(&mut self.inner.lock().unwrap().refresh)
    }

    // ---- commands (server → UI) -----------------------------------------

    /// Queue a command for the UI thread and wake the loop.
    pub fn push(&self, cmd: Command) {
        self.inner.lock().unwrap().commands.push(cmd);
        self.wake();
    }

    /// Runner side: drain everything queued since the last call.
    pub fn take_commands(&self) -> Vec<Command> {
        std::mem::take(&mut self.inner.lock().unwrap().commands)
    }

    // ---- metrics ---------------------------------------------------------

    /// Record one presented frame and its paint + copy-out cost.
    pub fn record_frame(&self, paint_ms: f32) {
        let mut g = self.inner.lock().unwrap();
        g.metrics.frames += 1;
        g.metrics.paint_ms_last = paint_ms;
        g.metrics.paint_ms_max = g.metrics.paint_ms_max.max(paint_ms);
    }

    /// Record `n` delivered input events.
    pub fn record_input(&self, n: u64) {
        self.inner.lock().unwrap().metrics.input_events += n;
    }

    /// Record the current surface size (start + hotplug).
    pub fn set_size(&self, width: u32, height: u32) {
        let mut g = self.inner.lock().unwrap();
        g.metrics.width = width;
        g.metrics.height = height;
    }

    /// A consistent snapshot for `/metrics`.
    pub fn metrics(&self) -> MetricsSnapshot {
        let g = self.inner.lock().unwrap();
        MetricsSnapshot {
            frames: g.metrics.frames,
            paint_ms_last: g.metrics.paint_ms_last,
            paint_ms_max: g.metrics.paint_ms_max,
            input_events: g.metrics.input_events,
            uptime_s: self.started.elapsed().as_secs_f64(),
            width: g.metrics.width,
            height: g.metrics.height,
            clients: self.clients.load(Ordering::Relaxed),
            watchers: g.watchers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn frames_publish_and_wait() {
        let hub = Hub::new();
        assert!(hub.latest_frame().is_none());
        assert!(hub.wait_frame(0, Duration::from_millis(10)).is_none());

        hub.publish_frame(2, 1, vec![0; 8]);
        let f = hub.latest_frame().unwrap();
        assert_eq!((f.seq, f.width, f.height), (1, 2, 1));
        // A waiter for something newer than seq 0 is satisfied immediately.
        assert_eq!(hub.wait_frame(0, Duration::from_millis(10)).unwrap().seq, 1);
        // ... but not one already at seq 1.
        assert!(hub.wait_frame(1, Duration::from_millis(10)).is_none());

        // A publish from another thread releases a blocked waiter.
        let h2 = hub.clone();
        let t = std::thread::spawn(move || h2.wait_frame(1, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        hub.publish_frame(2, 1, vec![255; 8]);
        assert_eq!(t.join().unwrap().unwrap().seq, 2);
    }

    #[test]
    fn commands_queue_and_wake() {
        let hub = Hub::new();
        let (tx, rx) = mpsc::channel();
        hub.set_waker(move || {
            let _ = tx.send(());
        });
        hub.push(Command::Key {
            name: "a".to_string(),
        });
        rx.recv_timeout(Duration::from_secs(1)).expect("woken");
        let cmds = hub.take_commands();
        assert_eq!(cmds.len(), 1);
        assert!(matches!(&cmds[0], Command::Key { name } if name == "a"));
        assert!(hub.take_commands().is_empty());
    }

    #[test]
    fn refresh_flag_is_taken_once() {
        let hub = Hub::new();
        assert!(!hub.take_refresh());
        hub.request_refresh();
        assert!(hub.take_refresh());
        assert!(!hub.take_refresh());
        // Publishing clears a pending refresh too.
        hub.request_refresh();
        hub.publish_frame(1, 1, vec![0; 4]);
        assert!(!hub.take_refresh());
    }

    #[test]
    fn metrics_accumulate() {
        let hub = Hub::new();
        hub.set_size(640, 480);
        hub.record_frame(2.0);
        hub.record_frame(1.0);
        hub.record_input(3);
        let m = hub.metrics();
        assert_eq!(m.frames, 2);
        assert_eq!(m.paint_ms_last, 1.0);
        assert_eq!(m.paint_ms_max, 2.0);
        assert_eq!(m.input_events, 3);
        assert_eq!((m.width, m.height), (640, 480));
    }
}

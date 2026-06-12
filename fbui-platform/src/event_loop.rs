//! The event loop: one `calloop` loop multiplexing every fd the platform owns.
//!
//! Per the plan, a single loop drives the DRM page-flip fd (the frame clock),
//! the input fds, the VT-switch self-pipe, the seat session fd, and a frame
//! timer for the fbdev path. Frame pacing falls out of it: we render only when
//! there's damage *and* a buffer is free, and otherwise the loop sleeps in
//! `poll`, so an idle UI burns ~0% CPU.
//!
//! The app implements [`PlatformHandler`]; everything device-facing is hidden
//! behind it:
//!
//! ```ignore
//! struct MyApp { /* ... */ }
//! impl PlatformHandler for MyApp {
//!     fn on_input(&mut self, ev: InputEvent) -> Flow { /* ... */ Flow::Redraw }
//!     fn render(&mut self, frame: &mut Frame<'_>) -> Vec<Rect> { /* paint */ vec![] }
//! }
//! platform.run(&mut MyApp { .. })?;
//! ```

use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, RawFd};
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop as Calloop, Interest, Mode, PostAction};

use crate::display::{Display, Frame};
use crate::error::{Error, Result};
use crate::geom::Rect;
use crate::input::{InputEvent, InputSource};
use crate::seat::{Seat, SessionEvent};
use crate::vt::{VtEvent, VtGuard};

/// What the app wants to happen after handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Nothing changed on screen; keep waiting.
    Continue,
    /// Schedule a repaint.
    Redraw,
    /// Tear down and return from [`run`](super::Platform::run).
    Exit,
}

/// The application side of the platform. The loop calls these; the app never
/// touches a device fd directly.
pub trait PlatformHandler {
    /// Handle one normalized input event.
    fn on_input(&mut self, event: InputEvent) -> Flow;

    /// Paint the current frame into `frame.buffer` (sequential, whole-row
    /// writes — see [`Frame`]). Return the damage rectangles produced, which the
    /// backend uses to bound copy-out. Returning an empty slice means "nothing
    /// changed" and the present is skipped.
    fn render(&mut self, frame: &mut Frame<'_>) -> Vec<Rect>;

    /// The session became active (`true`, VT switched to us) or inactive
    /// (`false`, switched away). Default: ignore.
    fn on_session(&mut self, active: bool) {
        let _ = active;
    }

    /// Called once per loop wakeup after events are drained, for app-driven
    /// animation/timers. Return [`Flow::Redraw`] to keep animating. Default: idle.
    fn tick(&mut self) -> Flow {
        Flow::Continue
    }
}

/// An `AsFd` wrapper around a borrowed raw fd, so we can hand `calloop` a poll
/// target whose real owner (the display / input source) lives in [`LoopState`].
///
/// Safe to use here because the loop and its sources are dropped at the end of
/// [`run`] while the owning devices are still alive.
struct PollFd(RawFd);

impl AsFd for PollFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        // SAFETY: the fd is owned by a device in `LoopState`, which outlives the
        // loop; `calloop` only ever polls it, never closes it.
        unsafe { BorrowedFd::borrow_raw(self.0) }
    }
}

/// Everything the loop threads through its source callbacks.
struct LoopState<'h> {
    display: Box<dyn Display>,
    inputs: Vec<Box<dyn InputSource>>,
    seat: Box<dyn Seat>,
    vt: VtGuard,
    handler: &'h mut dyn PlatformHandler,
    /// Session/VT currently ours? When false we neither render nor hold master.
    active: bool,
    /// A repaint is owed.
    needs_redraw: bool,
    /// Set by [`Flow::Exit`]; the loop stops on the next turn.
    exit: bool,
}

impl LoopState<'_> {
    /// Render + present if there's a reason to and a buffer is free.
    fn try_render(&mut self) -> Result<()> {
        if !self.active || !self.needs_redraw {
            return Ok(());
        }
        let Some(mut frame) = self.display.begin_frame()? else {
            // No free buffer yet (flip in flight); we'll retry on flip-complete.
            return Ok(());
        };
        // `frame` borrows `display`; its last use here ends the borrow (NLL),
        // freeing `display` for the `present` call below.
        let damage = self.handler.render(&mut frame);
        if damage.is_empty() {
            self.needs_redraw = false;
            return Ok(());
        }
        self.display.present(&damage)?;
        self.needs_redraw = false;
        Ok(())
    }

    /// Drain input from every source, feeding the handler.
    fn on_input_ready(&mut self) -> Result<()> {
        // Collect first to avoid borrowing `self` mutably twice (sources +
        // handler). Events are cheap and batches are small.
        let mut events: Vec<InputEvent> = Vec::new();
        for src in &mut self.inputs {
            src.dispatch(&mut |ev| events.push(ev))?;
        }
        for ev in events {
            match self.handler.on_input(ev) {
                Flow::Continue => {}
                Flow::Redraw => self.needs_redraw = true,
                Flow::Exit => self.exit = true,
            }
        }
        self.try_render()
    }

    /// A page-flip completed: free the buffer and paint the next frame if owed.
    fn on_display_ready(&mut self) -> Result<()> {
        self.display.dispatch_present()?;
        self.try_render()
    }

    /// A cooperative VT switch: suspend on release (then ack), resume on acquire.
    fn on_vt_ready(&mut self) -> Result<()> {
        let mut events = Vec::new();
        self.vt
            .drain_switches(|e| events.push(e))
            .map_err(|e| Error::io("vt drain", e))?;
        for e in events {
            match e {
                VtEvent::Release => {
                    self.active = false;
                    self.display.suspend()?;
                    self.handler.on_session(false);
                    self.vt
                        .ack_release()
                        .map_err(|e| Error::io("vt ack_release", e))?;
                }
                VtEvent::Acquire => {
                    self.vt
                        .ack_acquire()
                        .map_err(|e| Error::io("vt ack_acquire", e))?;
                    self.display.resume()?;
                    self.active = true;
                    self.needs_redraw = true;
                    self.handler.on_session(true);
                }
            }
        }
        self.try_render()
    }

    /// A seat (libseat) session change — same suspend/resume, manager-driven.
    fn on_seat_ready(&mut self) -> Result<()> {
        let mut events = Vec::new();
        self.seat.dispatch(&mut |e| events.push(e))?;
        for e in events {
            match e {
                SessionEvent::Deactivate => {
                    self.active = false;
                    self.display.suspend()?;
                    self.handler.on_session(false);
                }
                SessionEvent::Activate => {
                    self.display.resume()?;
                    self.active = true;
                    self.needs_redraw = true;
                    self.handler.on_session(true);
                }
            }
        }
        self.try_render()
    }
}

/// Drive `handler` until it asks to exit. Consumes the platform's device set.
///
/// This is the body of [`Platform::run`](super::Platform::run); it's a free
/// function so the borrow of `handler` and the move of the devices stay local.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_loop(
    display: Box<dyn Display>,
    inputs: Vec<Box<dyn InputSource>>,
    seat: Box<dyn Seat>,
    vt: VtGuard,
    handler: &mut dyn PlatformHandler,
) -> Result<()> {
    // Snapshot the fds before the devices move into `LoopState` (fd numbers are
    // stable across the move).
    let display_fd: Option<RawFd> = display.present_fd().map(|f| f.as_raw_fd());
    let input_fds: Vec<RawFd> = inputs.iter().flat_map(|s| s.fds()).collect();
    let vt_fd: Option<RawFd> = vt.switch_fd();
    let seat_fd: Option<RawFd> = seat.session_fd();
    // fbdev has no flip fd, so pace it with a timer instead.
    let needs_timer = display_fd.is_none();

    let mut event_loop: Calloop<LoopState> =
        Calloop::try_new().map_err(|e| Error::io("calloop new", std::io::Error::other(e)))?;
    let handle = event_loop.handle();

    let mut state = LoopState {
        display,
        inputs,
        seat,
        vt,
        handler,
        active: true,
        needs_redraw: true, // force the first frame
        exit: false,
    };

    if let Some(fd) = display_fd {
        handle
            .insert_source(
                Generic::new(PollFd(fd), Interest::READ, Mode::Level),
                |_, _, st: &mut LoopState| {
                    st.on_display_ready().map_err(into_io)?;
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| Error::io("insert display source", std::io::Error::other(e.error)))?;
    }
    for fd in input_fds {
        handle
            .insert_source(
                Generic::new(PollFd(fd), Interest::READ, Mode::Level),
                |_, _, st: &mut LoopState| {
                    st.on_input_ready().map_err(into_io)?;
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| Error::io("insert input source", std::io::Error::other(e.error)))?;
    }
    if let Some(fd) = vt_fd {
        handle
            .insert_source(
                Generic::new(PollFd(fd), Interest::READ, Mode::Level),
                |_, _, st: &mut LoopState| {
                    st.on_vt_ready().map_err(into_io)?;
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| Error::io("insert vt source", std::io::Error::other(e.error)))?;
    }
    if let Some(fd) = seat_fd {
        handle
            .insert_source(
                Generic::new(PollFd(fd), Interest::READ, Mode::Level),
                |_, _, st: &mut LoopState| {
                    st.on_seat_ready().map_err(into_io)?;
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| Error::io("insert seat source", std::io::Error::other(e.error)))?;
    }
    if needs_timer {
        let refresh = Duration::from_micros(16_666); // ~60 Hz pacing for fbdev
        handle
            .insert_source(
                Timer::from_duration(refresh),
                move |_, _, st: &mut LoopState| {
                    let _ = st.try_render();
                    TimeoutAction::ToDuration(refresh)
                },
            )
            .map_err(|e| Error::io("insert timer", std::io::Error::other(e.error)))?;
    }

    // Initial paint before we start sleeping in poll.
    state.try_render()?;

    loop {
        if state.exit {
            break;
        }
        // Let the handler advance any animation.
        match state.handler.tick() {
            Flow::Redraw => state.needs_redraw = true,
            Flow::Exit => state.exit = true,
            Flow::Continue => {}
        }
        state.try_render()?;
        // Block until at least one fd is ready (or the fbdev timer fires). The
        // timeout bounds latency for the tick() animation hook.
        event_loop
            .dispatch(Some(Duration::from_millis(16)), &mut state)
            .map_err(|e| Error::io("calloop dispatch", std::io::Error::other(e)))?;
    }
    Ok(())
}

/// Bridge our [`Error`] into the `io::Error` that calloop callbacks must return.
fn into_io(e: Error) -> std::io::Error {
    e.into()
}

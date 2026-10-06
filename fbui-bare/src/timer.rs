//! Deferred and repeating messages with no OS: the bare-metal counterpart of
//! `fbui::Proxy::send_after` / `send_every`.
//!
//! The queue is a plain `Vec` of millisecond deadlines on the board's clock —
//! timer counts in a UI are tiny, so a scan beats a heap. It never ticks: the
//! [`Runner`](crate::Runner) folds the earliest deadline into
//! [`next_deadline`](crate::Runner::next_deadline), so [`Board::wait`] sleeps
//! exactly until the next timer is due and a pending timer costs nothing until
//! it fires (fbui's idle rule). There are no threads here, so the shared state
//! is an `Rc<RefCell<_>>`, not a mutex.
//!
//! Time is the clock the runner was last handed (`now_ms` of the latest
//! [`Runner::handle`](crate::Runner::handle) / [`frame`](crate::Runner::frame)).
//! Timers armed before the runner has seen any time — in [`App::build`] or
//! [`App::on_start`] — count from the first reading, so a board whose clock
//! starts at boot doesn't see them fire early.
//!
//! [`Board::wait`]: crate::Board::wait
//! [`App::build`]: crate::App::build
//! [`App::on_start`]: crate::App::on_start

use alloc::rc::{Rc, Weak};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::time::Duration;

/// A cancellation handle for a message armed with [`Timers::send_after`] or
/// [`Timers::send_every`].
///
/// Call [`cancel`](Timer::cancel) to stop the delivery (a no-op if the
/// one-shot already fired). **Dropping the handle does *not* cancel** — it
/// detaches, leaving the timer to fire, exactly as in the hosted runner; hold
/// on to the handle only if you may need to cancel.
pub struct Timer {
    id: u64,
    queue: Weak<RefCell<dyn CancelSink>>,
}

impl Timer {
    /// Cancel the scheduled delivery. No-op if it already fired (one-shot) or
    /// the runner is gone.
    pub fn cancel(self) {
        if let Some(q) = self.queue.upgrade() {
            q.borrow_mut().cancel(self.id);
        }
    }
}

/// The type-erased cancel half, so [`Timer`] needn't be generic over the
/// app's message type.
trait CancelSink {
    fn cancel(&mut self, id: u64);
}

struct Queue<M> {
    entries: Vec<Entry<M>>,
    next_id: u64,
    /// The latest board time the runner reported; `None` until the first.
    now: Option<u64>,
}

struct Entry<M> {
    id: u64,
    /// Board milliseconds — or, while `Queue::now` is `None`, the delay from
    /// the first clock reading.
    due: u64,
    /// `Some` = repeating with this period in ms (fixed delay), `None` =
    /// one-shot.
    period: Option<u64>,
    msg: M,
}

impl<M> CancelSink for Queue<M> {
    fn cancel(&mut self, id: u64) {
        self.entries.retain(|e| e.id != id);
    }
}

/// A handle for delivering the app's own messages later: a clock tick, a
/// sensor poll, a timeout. Cheap to clone; the app receives one in
/// [`App::on_start`](crate::App::on_start) and keeps it, and board code can
/// get another from [`Runner::timers`](crate::Runner::timers).
///
/// Messages are delivered through `App::update` at the start of the next
/// [`Runner::frame`](crate::Runner::frame) at or after their deadline, in
/// deadline order.
pub struct Timers<M> {
    inner: Rc<RefCell<Queue<M>>>,
}

impl<M> Clone for Timers<M> {
    fn clone(&self) -> Self {
        Timers {
            inner: self.inner.clone(),
        }
    }
}

impl<M: Clone + 'static> Timers<M> {
    pub(crate) fn new() -> Self {
        Timers {
            inner: Rc::new(RefCell::new(Queue {
                entries: Vec::new(),
                next_id: 0,
                now: None,
            })),
        }
    }

    /// Deliver `msg` on the next frame — a way for code outside `update` (or
    /// `update` itself) to queue work for after the current step.
    pub fn send(&self, msg: M) {
        let _ = self.schedule(0, None, msg);
    }

    /// Deliver `msg` once, `delay` from now. Sub-millisecond precision is
    /// dropped (the board clock is in milliseconds).
    pub fn send_after(&self, delay: Duration, msg: M) -> Timer {
        self.schedule(millis(delay), None, msg)
    }

    /// Deliver `msg` every `period`, the first time one `period` from now,
    /// until the returned [`Timer`] is cancelled. Repeats are **fixed delay**:
    /// a stalled loop gets one delivery on catch-up, never a burst. A period
    /// under a millisecond is treated as one.
    pub fn send_every(&self, period: Duration, msg: M) -> Timer {
        let p = millis(period).max(1);
        self.schedule(p, Some(p), msg)
    }

    fn schedule(&self, delay: u64, period: Option<u64>, msg: M) -> Timer {
        let mut q = self.inner.borrow_mut();
        let id = q.next_id;
        q.next_id += 1;
        let due = q.now.unwrap_or(0).saturating_add(delay);
        q.entries.push(Entry {
            id,
            due,
            period,
            msg,
        });
        drop(q);
        let erased: Rc<RefCell<dyn CancelSink>> = self.inner.clone();
        Timer {
            id,
            queue: Rc::downgrade(&erased),
        }
    }

    /// Record the board time. The first reading anchors timers armed before
    /// any time was known.
    pub(crate) fn set_now(&self, now: u64) {
        let mut q = self.inner.borrow_mut();
        if q.now.is_none() {
            for e in &mut q.entries {
                e.due = e.due.saturating_add(now);
            }
        }
        // The board clock is monotonic; never let a stray reading move
        // deadlines backwards.
        q.now = Some(q.now.map_or(now, |prev| prev.max(now)));
    }

    /// The earliest pending deadline, in board milliseconds. Only meaningful
    /// after [`set_now`](Self::set_now), which the runner always calls first.
    pub(crate) fn next_due(&self) -> Option<u64> {
        self.inner.borrow().entries.iter().map(|e| e.due).min()
    }

    /// Pop every message due at `now`, in deadline order (ties in the order
    /// they were armed). One-shots are removed; a repeating entry re-arms at
    /// `now + period`.
    pub(crate) fn take_due(&self, now: u64) -> Vec<M> {
        let mut q = self.inner.borrow_mut();
        let mut due: Vec<(u64, u64, M)> = Vec::new();
        let mut i = 0;
        while i < q.entries.len() {
            let e = &mut q.entries[i];
            if e.due > now {
                i += 1;
                continue;
            }
            match e.period {
                Some(p) => {
                    due.push((e.due, e.id, e.msg.clone()));
                    e.due = now.saturating_add(p);
                    i += 1;
                }
                None => {
                    let e = q.entries.remove(i);
                    due.push((e.due, e.id, e.msg));
                }
            }
        }
        due.sort_by_key(|(d, id, _)| (*d, *id));
        due.into_iter().map(|(_, _, m)| m).collect()
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

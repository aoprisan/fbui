//! The runner's timer queue: deadlines behind [`Proxy::send_after`] /
//! [`Proxy::send_every`].
//!
//! A plain `Vec` of deadlines behind a mutex — timer counts in a UI are tiny,
//! so a scan beats a heap. The queue never ticks: the runner asks
//! [`next_due`](TimerQueue::next_due) how long the event loop may sleep and
//! collects ripe messages with [`take_due`](TimerQueue::take_due) when it
//! wakes, so a pending timer costs zero CPU until it fires (the idle-burns-0%
//! rule). Std-only and headless — everything here unit-tests without a
//! device.
//!
//! [`Proxy::send_after`]: crate::Proxy::send_after
//! [`Proxy::send_every`]: crate::Proxy::send_every

use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

/// A cancellation handle for a scheduled [`Proxy::send_after`] /
/// [`Proxy::send_every`] message.
///
/// Call [`cancel`](Timer::cancel) to stop the delivery (a no-op if the
/// one-shot already fired). **Dropping the handle does *not* cancel** — it
/// detaches, leaving the timer to fire; hold on to the handle only if you may
/// need to cancel. `Send`, so a worker thread can cancel a timer the UI
/// thread armed (and vice versa).
///
/// [`Proxy::send_after`]: crate::Proxy::send_after
/// [`Proxy::send_every`]: crate::Proxy::send_every
pub struct Timer {
    id: u64,
    queue: Weak<Mutex<dyn CancelSink + Send>>,
}

impl Timer {
    /// Cancel the scheduled delivery. No-op if it already fired (one-shot) or
    /// the app has exited.
    pub fn cancel(self) {
        if let Some(q) = self.queue.upgrade() {
            if let Ok(mut q) = q.lock() {
                q.cancel(self.id);
            }
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
}

struct Entry<M> {
    id: u64,
    due: Instant,
    /// `Some` = repeating with this period (fixed-delay), `None` = one-shot.
    period: Option<Duration>,
    msg: M,
}

impl<M> CancelSink for Queue<M> {
    fn cancel(&mut self, id: u64) {
        self.entries.retain(|e| e.id != id);
    }
}

/// The shared deadline queue: one per running app, cloned into every
/// [`Proxy`](crate::Proxy) and drained by the runner.
pub(crate) struct TimerQueue<M> {
    inner: Arc<Mutex<Queue<M>>>,
}

impl<M> Clone for TimerQueue<M> {
    fn clone(&self) -> Self {
        TimerQueue {
            inner: self.inner.clone(),
        }
    }
}

impl<M: Send + 'static> TimerQueue<M> {
    pub fn new() -> Self {
        TimerQueue {
            inner: Arc::new(Mutex::new(Queue {
                entries: Vec::new(),
                next_id: 0,
            })),
        }
    }

    /// Schedule `msg` for `due` (repeating every `period` if `Some`),
    /// returning the cancel handle.
    pub fn schedule(&self, due: Instant, period: Option<Duration>, msg: M) -> Timer {
        let mut q = self.inner.lock().expect("timer queue poisoned");
        let id = q.next_id;
        q.next_id += 1;
        q.entries.push(Entry {
            id,
            due,
            period,
            msg,
        });
        drop(q);
        let erased: Arc<Mutex<dyn CancelSink + Send>> = self.inner.clone();
        Timer {
            id,
            queue: Arc::downgrade(&erased),
        }
    }

    /// The earliest pending deadline — how long the event loop may sleep.
    pub fn next_due(&self) -> Option<Instant> {
        let q = self.inner.lock().expect("timer queue poisoned");
        q.entries.iter().map(|e| e.due).min()
    }

    /// Pop every message due at `now`, in deadline order. One-shots are
    /// removed; a repeating entry clones its message and reschedules at
    /// `now + period` — **fixed delay**, so a stalled loop delivers one
    /// message per period on catch-up, never a burst.
    pub fn take_due(&self, now: Instant) -> Vec<M>
    where
        M: Clone,
    {
        let mut q = self.inner.lock().expect("timer queue poisoned");
        let mut due: Vec<(Instant, M)> = Vec::new();
        let mut i = 0;
        while i < q.entries.len() {
            if q.entries[i].due <= now {
                match q.entries[i].period {
                    Some(p) => {
                        due.push((q.entries[i].due, q.entries[i].msg.clone()));
                        q.entries[i].due = now + p;
                        i += 1;
                    }
                    None => {
                        let e = q.entries.remove(i);
                        due.push((e.due, e.msg));
                    }
                }
            } else {
                i += 1;
            }
        }
        due.sort_by_key(|(d, _)| *d);
        due.into_iter().map(|(_, m)| m).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Instant {
        Instant::now()
    }

    #[test]
    fn fires_in_deadline_order() {
        let t0 = base();
        let q = TimerQueue::<&'static str>::new();
        let _b = q.schedule(t0 + Duration::from_millis(20), None, "b");
        let _a = q.schedule(t0 + Duration::from_millis(10), None, "a");
        assert_eq!(q.next_due(), Some(t0 + Duration::from_millis(10)));
        assert!(q.take_due(t0).is_empty(), "nothing ripe yet");
        assert_eq!(q.take_due(t0 + Duration::from_millis(25)), vec!["a", "b"]);
        assert_eq!(q.next_due(), None, "one-shots are gone");
    }

    #[test]
    fn cancel_before_due_suppresses_delivery() {
        let t0 = base();
        let q = TimerQueue::<u32>::new();
        let keep = q.schedule(t0 + Duration::from_millis(10), None, 1);
        let drop_me = q.schedule(t0 + Duration::from_millis(10), None, 2);
        drop_me.cancel();
        let _ = keep; // dropping the handle detaches; the timer still fires
        assert_eq!(q.take_due(t0 + Duration::from_millis(10)), vec![1]);
    }

    #[test]
    fn periodic_reschedules_fixed_delay_without_bursts() {
        let t0 = base();
        let q = TimerQueue::<u32>::new();
        let handle = q.schedule(
            t0 + Duration::from_millis(10),
            Some(Duration::from_millis(10)),
            7,
        );
        // A long stall covers many periods: exactly one delivery, re-armed
        // one period after *now* (fixed delay, no catch-up burst).
        let late = t0 + Duration::from_millis(100);
        assert_eq!(q.take_due(late), vec![7]);
        assert_eq!(q.next_due(), Some(late + Duration::from_millis(10)));
        // Cancel stops the repetition.
        handle.cancel();
        assert_eq!(q.next_due(), None);
        assert!(q.take_due(late + Duration::from_secs(1)).is_empty());
    }

    #[test]
    fn timer_handle_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Timer>();
    }
}

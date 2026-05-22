use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use tokio::sync::broadcast;

/// Result of a successful [`crate::pty::PtySession::subscribe`] call.
///
/// `snapshot` is an opaque VT escape sequence representing the terminal
/// state at the moment of subscription. Feed it to a VT100/xterm-compatible
/// emulator (xterm.js, ghostty-web, etc.) before processing `stream` bytes.
///
/// `stream` yields bytes that arrived strictly *after* the snapshot was
/// captured — no gap, no overlap. `broadcast::error::RecvError::Lagged(_)`
/// means the subscriber fell behind the broadcast capacity; recover by
/// dropping this `Subscription` and calling `subscribe()` again — the new
/// snapshot reflects current state and the new receiver starts clean.
/// `RecvError::Closed` means the session has exited.
///
/// While a `Subscription` is alive, the worker treats this client as a
/// "primary" attached emulator: backend auto-replies to terminal queries
/// (DA, XTVERSION, DSR, etc.) are suppressed so the client's emulator
/// can answer instead. The primary count returns to zero when this
/// Subscription is dropped.
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
    _primary_guard: PrimaryGuard,
}

impl Subscription {
    /// Construct a Subscription, incrementing `primary_count` and binding
    /// the matching decrement to this value's drop.
    ///
    /// `pub(crate)` because external callers receive Subscriptions from
    /// [`crate::pty::PtySession::subscribe`]; they never construct them
    /// directly. The constructor encapsulates the count-increment
    /// invariant so internal call sites cannot forget it.
    pub(crate) fn new(
        snapshot: Bytes,
        stream: broadcast::Receiver<Bytes>,
        primary_count: Arc<AtomicUsize>,
    ) -> Self {
        primary_count.fetch_add(1, Ordering::Relaxed);
        Self {
            snapshot,
            stream,
            _primary_guard: PrimaryGuard(primary_count),
        }
    }
}

/// RAII guard that decrements the primary-attached counter on drop.
pub(crate) struct PrimaryGuard(Arc<AtomicUsize>);

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    #[test]
    fn new_increments_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let _sub = Subscription::new(Bytes::new(), rx, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drop_decrements_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let sub = Subscription::new(Bytes::new(), rx, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn multiple_subscriptions_share_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx1) = broadcast::channel::<Bytes>(1);
        let (_tx2, rx2) = broadcast::channel::<Bytes>(1);
        let sub1 = Subscription::new(Bytes::new(), rx1, counter.clone());
        let sub2 = Subscription::new(Bytes::new(), rx2, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        drop(sub1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub2);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}

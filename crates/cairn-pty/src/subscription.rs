use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use tokio::sync::broadcast;

pub use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::ClientId;
use crate::ghostty::{Command, Envelope};

/// Result of a successful [`crate::PtySession::subscribe`] call.
///
/// `snapshot` is an opaque VT escape sequence representing the terminal
/// state at the moment of subscription. Feed it to a VT100/xterm-compatible
/// emulator (xterm.js, ghostty-web, etc.) before processing further bytes.
///
/// Use [`Subscription::recv`] (async) or [`Subscription::try_recv`]
/// (non-blocking) to pull bytes that arrived strictly *after* the snapshot
/// was captured — no gap, no overlap.
///
/// **Session-end semantics.** `recv()`/`try_recv()` return
/// [`RecvError::Closed`] / [`TryRecvError::Closed`] when the underlying
/// session has ended. The worker guarantees that the broadcast sender is
/// dropped within a bounded delay of the child being reaped — after a
/// brief drain to flush any output the kernel had queued on the master
/// PTY at exit. That drain is essential on Linux, where the SIGCHLD reap
/// frequently wins the race against `EPOLLHUP` propagation on the master
/// FD; without it, subscribers would either block indefinitely on a
/// still-open broadcast or lose the child's final bytes.
///
/// `RecvError::Lagged(n)` means the subscriber fell behind the broadcast
/// capacity; recover by dropping this `Subscription` and calling
/// `subscribe()` again — the new snapshot reflects current state and the
/// new receiver starts clean.
///
/// While a `Subscription` is alive, the worker treats this client as a
/// "primary" attached emulator: backend auto-replies to terminal queries
/// (DA, XTVERSION, DSR, etc.) are suppressed so the client's emulator can
/// answer instead. The primary count returns to zero when this Subscription
/// is dropped.
///
/// Dropping the Subscription also sends `Command::Detach` to the worker so
/// it can clear the leader seat if this client held it.
pub struct Subscription {
    pub snapshot: Bytes,
    stream: broadcast::Receiver<Bytes>,
    _guard: SubscriptionGuard,
}

impl Subscription {
    /// Construct a `Subscription`, incrementing `primary_count` and
    /// binding both decrement and detach-notification to drop.
    pub(crate) fn new(
        snapshot: Bytes,
        stream: broadcast::Receiver<Bytes>,
        primary_count: Arc<AtomicUsize>,
        client_id: ClientId,
        cmd_tx: flume::Sender<Envelope>,
    ) -> Self {
        primary_count.fetch_add(1, Ordering::Relaxed);
        Self {
            snapshot,
            stream,
            _guard: SubscriptionGuard {
                client_id,
                primary_count,
                cmd_tx,
            },
        }
    }

    /// Yield the next chunk of session output. Blocks until output is
    /// available, the subscriber lags, or the session ends.
    pub async fn recv(&mut self) -> Result<Bytes, RecvError> {
        self.stream.recv().await
    }

    /// Non-blocking variant of [`recv`]. Returns `Empty` if no chunk is
    /// available right now and the session is still live; returns `Closed`
    /// if the session has ended.
    pub fn try_recv(&mut self) -> Result<Bytes, TryRecvError> {
        self.stream.try_recv()
    }
}

/// RAII guard combining two on-drop responsibilities:
///   1. Decrement the worker's primary-attached counter.
///   2. Send `Command::Detach` so the worker can clear the leader
///      seat if this client held it.
struct SubscriptionGuard {
    client_id: ClientId,
    primary_count: Arc<AtomicUsize>,
    cmd_tx: flume::Sender<Envelope>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.primary_count.fetch_sub(1, Ordering::Relaxed);
        // Best-effort. If `cmd_tx` is closed, the worker has already
        // shut down and there's no leader state to clear. trace_id is
        // None because Drop has no meaningful caller span to propagate.
        let _ = self.cmd_tx.send(Envelope {
            cmd: Command::Detach {
                client_id: self.client_id,
            },
            trace_id: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    fn dummy_channel() -> flume::Sender<Envelope> {
        let (tx, _rx) = flume::unbounded::<Envelope>();
        tx
    }

    #[test]
    fn new_increments_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let _sub = Subscription::new(
            Bytes::new(),
            rx,
            counter.clone(),
            ClientId::from_u64(0),
            dummy_channel(),
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drop_decrements_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let sub = Subscription::new(
            Bytes::new(),
            rx,
            counter.clone(),
            ClientId::from_u64(0),
            dummy_channel(),
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn multiple_subscriptions_share_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx1) = broadcast::channel::<Bytes>(1);
        let (_tx2, rx2) = broadcast::channel::<Bytes>(1);
        let sub1 = Subscription::new(
            Bytes::new(),
            rx1,
            counter.clone(),
            ClientId::from_u64(0),
            dummy_channel(),
        );
        let sub2 = Subscription::new(
            Bytes::new(),
            rx2,
            counter.clone(),
            ClientId::from_u64(1),
            dummy_channel(),
        );
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        drop(sub1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub2);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn drop_sends_detach_command() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let (cmd_tx, cmd_rx) = flume::unbounded::<Envelope>();
        let client_id = ClientId::from_u64(0);
        let sub = Subscription::new(Bytes::new(), rx, counter, client_id, cmd_tx);
        drop(sub);
        let received = cmd_rx.try_recv().expect("Detach should have been sent");
        match received.cmd {
            Command::Detach { client_id: id } => assert_eq!(id, client_id),
            other => panic!("expected Detach, got {:?}", std::mem::discriminant(&other)),
        }
    }

    /// `recv()` returns `Closed` once the broadcast sender is dropped — the
    /// signal the worker emits during its post-exit drain.
    #[tokio::test]
    async fn recv_returns_closed_when_broadcast_sender_dropped() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (bcast_tx, bcast_rx) = broadcast::channel::<Bytes>(4);
        let mut sub = Subscription::new(
            Bytes::new(),
            bcast_rx,
            counter,
            ClientId::from_u64(0),
            dummy_channel(),
        );
        drop(bcast_tx);
        let err = sub.recv().await.unwrap_err();
        assert!(matches!(err, RecvError::Closed));
    }
}

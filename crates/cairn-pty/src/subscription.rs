use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use tokio::sync::broadcast;

use crate::ClientId;
use crate::ghostty::Command;

/// Result of a successful [`crate::pty::PtySession::subscribe`] call.
///
/// `snapshot` is an opaque VT escape sequence representing the
/// terminal state at the moment of subscription. Feed it to a
/// VT100/xterm-compatible emulator (xterm.js, ghostty-web, etc.)
/// before processing `stream` bytes.
///
/// `stream` yields bytes that arrived strictly *after* the snapshot
/// was captured — no gap, no overlap.
/// `broadcast::error::RecvError::Lagged(_)` means the subscriber fell
/// behind the broadcast capacity; recover by dropping this
/// `Subscription` and calling `subscribe()` again — the new snapshot
/// reflects current state and the new receiver starts clean.
/// `RecvError::Closed` means the session has exited.
///
/// While a `Subscription` is alive, the worker treats this client as
/// a "primary" attached emulator: backend auto-replies to terminal
/// queries (DA, XTVERSION, DSR, etc.) are suppressed so the client's
/// emulator can answer instead. The primary count returns to zero
/// when this Subscription is dropped.
///
/// Dropping the Subscription also sends `Command::Detach` to the
/// worker so it can clear the leader seat if this client held it.
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
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
        cmd_tx: flume::Sender<Command>,
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
}

/// RAII guard combining two on-drop responsibilities:
///   1. Decrement the worker's primary-attached counter.
///   2. Send `Command::Detach` so the worker can clear the leader
///      seat if this client held it.
pub(crate) struct SubscriptionGuard {
    client_id: ClientId,
    primary_count: Arc<AtomicUsize>,
    cmd_tx: flume::Sender<Command>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.primary_count.fetch_sub(1, Ordering::Relaxed);
        // Best-effort. If `cmd_tx` is closed, the worker has already
        // shut down and there's no leader state to clear.
        let _ = self.cmd_tx.send(Command::Detach {
            client_id: self.client_id,
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

    fn dummy_channel() -> flume::Sender<Command> {
        let (tx, _rx) = flume::unbounded::<Command>();
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
        let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
        let client_id = ClientId::from_u64(0);
        let sub = Subscription::new(
            Bytes::new(),
            rx,
            counter,
            client_id,
            cmd_tx,
        );
        drop(sub);
        let received = cmd_rx.try_recv().expect("Detach should have been sent");
        match received {
            Command::Detach { client_id: id } => assert_eq!(id, client_id),
            other => panic!("expected Detach, got {:?}", std::mem::discriminant(&other)),
        }
    }
}

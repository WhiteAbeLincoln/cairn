use std::collections::HashSet;
use std::pin::Pin;

use futures::Stream;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio_stream::wrappers::ReceiverStream;

use cairn_protocol::cairn::daemon::types::SessionEvent;

use crate::daemon::Daemon;
use crate::handlers::sessions::list_all;
use crate::registry::{RegistryEvent, session_info};

/// `sessions.watch-sessions`: server-push session-list subscription. The
/// first item is always a full `Snapshot`; afterwards each bus notification
/// (create/rename/restart/exit/attach/detach) is coalesced into a batch of
/// `Upsert`/`Removed` events built from fresh state, so slow sends never
/// carry stale data.
///
/// Plain `fn`, not `async fn`: everything here (subscribing, cloning the
/// handle, spawning the background task) is synchronous — the only `.await`
/// points live inside the spawned task's own future, which this function
/// never awaits itself.
pub fn watch_sessions(
    d: &Daemon,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = Vec<SessionEvent>> + Send + 'static>>> {
    // Subscribe BEFORE building the snapshot — nothing falls in the gap
    // between listing sessions and the first delta.
    let mut events = d.registry.subscribe_events();
    // Cloned (cheap: both fields are `Arc`) so the task can outlive this call.
    let daemon = d.clone();
    let (tx, out) = tokio::sync::mpsc::channel::<Vec<SessionEvent>>(2);

    tokio::spawn(async move {
        let snapshot = list_all(&daemon).await;
        if tx
            .send(vec![SessionEvent::Snapshot(snapshot)])
            .await
            .is_err()
        {
            return;
        }

        loop {
            // Race the bus against the subscriber hanging up. `tx.closed()`
            // resolves once wRPC drops its receiver (client disconnect); a
            // later `tx.send` failing (the old behavior) only happens to
            // notice this on the NEXT bus event, so a disconnect during a
            // quiet period leaves the task (and its bus receiver) parked
            // indefinitely. Both arms are cancel-safe: `broadcast::Receiver::
            // recv` and `mpsc::Sender::closed` document no lost state on
            // cancellation, and `recv_batch`'s try_recv drain runs entirely
            // after its one await point resolves, so there's no partial
            // batch to lose if `tx.closed()` wins the race instead.
            tokio::select! {
                _ = tx.closed() => return,
                outcome = recv_batch(&mut events) => match outcome {
                    BatchOutcome::Events(raw) => {
                        let batch = coalesce(&daemon, raw).await;
                        // A batch can be empty in principle (all events deduped
                        // away is impossible today, but stay defensive) — don't
                        // send a vacuous update.
                        if !batch.is_empty() && tx.send(batch).await.is_err() {
                            return;
                        }
                    }
                    BatchOutcome::Lagged => {
                        // Resync: the subscriber fell behind the bus; a fresh
                        // snapshot is cheaper and simpler than reasoning about
                        // which deltas were missed.
                        let snapshot = list_all(&daemon).await;
                        if tx
                            .send(vec![SessionEvent::Snapshot(snapshot)])
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    // Defense in depth, not a reachable path today: this task
                    // holds `daemon` (and, through it, the registry `Arc` that
                    // owns the bus sender), so the sender can't actually drop
                    // while this loop is running. Shutdown is process exit,
                    // not a bus close. Exit cleanly if this ever fires anyway.
                    BatchOutcome::Closed => return,
                },
            }
        }
    });

    Ok(Box::pin(ReceiverStream::new(out)))
}

/// Outcome of draining one round of bus events.
enum BatchOutcome {
    Events(Vec<RegistryEvent>),
    /// The subscriber fell behind (bus ring overflowed); caller should resync.
    Lagged,
    /// All senders dropped — the bus (and the daemon) is going away.
    Closed,
}

/// Block for one event, then non-blockingly drain the backlog into a single
/// batch — natural coalescing while the previous stream send was in flight.
async fn recv_batch(events: &mut tokio::sync::broadcast::Receiver<RegistryEvent>) -> BatchOutcome {
    let first = match events.recv().await {
        Ok(ev) => ev,
        Err(RecvError::Lagged(_)) => return BatchOutcome::Lagged,
        Err(RecvError::Closed) => return BatchOutcome::Closed,
    };
    let mut batch = vec![first];
    loop {
        match events.try_recv() {
            Ok(ev) => batch.push(ev),
            Err(TryRecvError::Empty) => break,
            // The bus closed mid-drain: process what we have now: the next
            // call to `recv_batch` will see `Closed` again and end the task.
            Err(TryRecvError::Closed) => break,
            // Lagged mid-drain: discard the partial batch, the caller resyncs
            // with a full snapshot instead of a partial delta set.
            Err(TryRecvError::Lagged(_)) => return BatchOutcome::Lagged,
        }
    }
    BatchOutcome::Events(batch)
}

/// Dedupe a raw batch of bus events by session id (first occurrence in
/// arrival order wins — `resolve()` below always reflects current truth, so
/// which occurrence is picked doesn't change the result), then resolve each
/// unique id to a fresh `Upsert`/`Removed`.
async fn coalesce(daemon: &Daemon, raw: Vec<RegistryEvent>) -> Vec<SessionEvent> {
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(raw.len());
    for ev in raw {
        let id = match &ev {
            RegistryEvent::Changed { id } | RegistryEvent::Removed { id } => id.clone(),
        };
        if !seen.insert(id.clone()) {
            continue; // already handled earlier in this batch
        }
        match ev {
            RegistryEvent::Changed { id } => match daemon.registry.resolve(&id) {
                Some(entry) => out.push(SessionEvent::Upsert(session_info(&entry).await)),
                // No session-removal op exists yet, but a resolve miss on a
                // `Changed` id future-proofs against one landing later.
                None => out.push(SessionEvent::Removed(id)),
            },
            RegistryEvent::Removed { id } => out.push(SessionEvent::Removed(id)),
        }
    }
    out
}

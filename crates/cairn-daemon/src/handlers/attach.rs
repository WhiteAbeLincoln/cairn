use std::pin::Pin;

use cairn_pty::TermSize;
use futures::{Stream, StreamExt as _};
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;
use tracing::Instrument as _;

use cairn_protocol::cairn::daemon::types::{
    AttachInit, ClientEvent, Error as WireError, ServerEvent,
};

use crate::daemon::Daemon;
use crate::handlers::wire_exit;

type ServerEvents = Pin<Box<dyn Stream<Item = Vec<ServerEvent>> + Send + 'static>>;
type ClientEvents = Pin<Box<dyn Stream<Item = Vec<ClientEvent>> + Send + 'static>>;

/// `sessions.attach`: the bidirectional bridge. Resolve, subscribe, emit a
/// `snapshot`, then bridge client-events × broadcast × kick onto the outbound
/// `server-event` stream. Errors are in-band (`server-event::error`).
pub async fn attach(
    d: &Daemon,
    id: String,
    init: AttachInit,
    events: ClientEvents,
) -> ServerEvents {
    let Some(entry) = d.registry.resolve(&id) else {
        return once_error("session.not_found", &format!("no such session: {id}"));
    };
    let client_id = d.registry.mint_client_id();
    let handle = entry.handle();

    // Leader-wins: the first interactive attacher claims the empty seat + sets
    // size; followers get NotLeader (ignored). Read-only attaches don't claim.
    if !init.no_stdin {
        let _ = handle
            .resize(
                client_id,
                TermSize {
                    cols: init.cols,
                    rows: init.rows,
                },
            )
            .await;
    }

    let sub = match handle.subscribe(client_id).await {
        Ok(s) => s,
        Err(e) => return once_error("pty.backend", &format!("subscribe failed: {e}")),
    };
    let (mut kick_rx, guard) = entry.attach(client_id);

    tracing::info!(
        session_id = %id,
        client_id = %client_id,
        no_stdin = init.no_stdin,
        "client attached"
    );

    let attach_span = tracing::info_span!(
        "attach",
        session_id = %id,
        client_id = %client_id,
    );

    let no_stdin = init.no_stdin;
    // Capacity is deliberately small: this channel exists only because wRPC's
    // StreamEncoder eagerly polls the returned Stream into an unbounded
    // internal BytesMut, so without a bounded intermediary the encoder would
    // drain the entire broadcast ring into heap memory.  The capacity limits
    // how far ahead the encoder can get; a slow client triggers broadcast
    // Lagged sooner, which is the desired outcome (kick it, trust the
    // snapshot).
    let (tx, out) = tokio::sync::mpsc::channel::<Vec<ServerEvent>>(2);

    tokio::spawn(
        async move {
            // Hold the attach guard (deregisters on drop) and the Subscription
            // (clears leadership + primary-count on drop) for the task's lifetime.
            let _guard = guard;
            let mut sub = sub;
            let mut events = events;

            if tx
                .send(vec![ServerEvent::Snapshot(sub.snapshot.clone())])
                .await
                .is_err()
            {
                return; // client already gone
            }

            loop {
                tokio::select! {
                    ev = events.next() => match ev {
                        Some(batch) => {
                            for e in batch {
                                match e {
                                    ClientEvent::Input(b) if !no_stdin => {
                                        if handle.write(client_id, b).await.is_err() { return; }
                                    }
                                    ClientEvent::Resize((c, r)) => {
                                        let _ = handle.resize(client_id, TermSize { cols: c, rows: r }).await;
                                    }
                                    ClientEvent::Detach => {
                                        tracing::info!("client detached");
                                        return;
                                    }
                                    _ => {} // Input while no_stdin: ignore
                                }
                            }
                        }
                        None => {
                            tracing::info!("client disconnected");
                            return;
                        }
                    },
                    out_chunk = sub.recv() => match out_chunk {
                        Ok(bytes) => {
                            // Race the send against kick so an operator-initiated
                            // kick isn't blocked by a slow client's full channel.
                            let msg = vec![ServerEvent::Output(bytes)];
                            tokio::select! {
                                res = tx.send(msg) => {
                                    if res.is_err() { return; }
                                }
                                _ = &mut kick_rx => {
                                    tracing::info!("client kicked");
                                    let _ = tx.try_send(vec![ServerEvent::Error(WireError {
                                        code: cairn_protocol::error_codes::CLIENT_KICKED.to_string(),
                                        message: "detached by operator".to_string(),
                                    })]);
                                    return;
                                }
                            }
                        }
                        Err(RecvError::Lagged(_)) => {
                            tracing::warn!("client lagged");
                            let _ = tx.try_send(vec![ServerEvent::Error(WireError {
                                code: cairn_protocol::error_codes::CLIENT_LAGGED.to_string(),
                                message: "client fell behind output; reattach for a fresh snapshot".to_string(),
                            })]);
                            return;
                        }
                        Err(RecvError::Closed) => {
                            tracing::info!("session ended under attached client");
                            let exit = wire_exit(handle.wait().await);
                            let _ = tx.try_send(vec![ServerEvent::Exited(exit)]);
                            return;
                        }
                    },
                    _ = &mut kick_rx => {
                        tracing::info!("client kicked");
                        let _ = tx.try_send(vec![ServerEvent::Error(WireError {
                            code: cairn_protocol::error_codes::CLIENT_KICKED.to_string(),
                            message: "detached by operator".to_string(),
                        })]);
                        return;
                    }
                }
            }
        }
        .instrument(attach_span),
    );

    Box::pin(ReceiverStream::new(out))
}

/// A one-element stream carrying a single `server-event::error`, then close.
fn once_error(code: &str, message: &str) -> ServerEvents {
    let err = ServerEvent::Error(WireError {
        code: code.to_string(),
        message: message.to_string(),
    });
    Box::pin(futures::stream::once(async move { vec![err] }))
}

//! WebSocket transport: an axum HTTP server whose `/ws` route upgrades to a
//! WebSocket speaking one of two wire protocols, negotiated via
//! `Sec-WebSocket-Protocol` (see [`WsMode`]):
//!
//! - **One-shot** (default, and explicitly as `cairn-oneshot-v0`): the
//!   connection carries exactly one wRPC invocation — the stock
//!   `wrpc-websockets` model, kept for dedicated high-throughput streams
//!   (attach/logs/send) and third-party clients.
//! - **Muxed** (`cairn-mux-v0`): one persistent connection carries many
//!   concurrent invocations on logical channels — see
//!   [`super::ws_mux`].
//!
//! This is the browser-facing primary transport. `bind`/`run` own the TCP
//! accept loop (delegated to `axum::serve`); the HTTP routing, upgrade
//! handshake, and subprotocol negotiation live in [`crate::serve::http`],
//! which hands successful upgrades back here via [`serve_upgraded`].
//!
//! ## Why only `wrpc_websockets::split`
//!
//! The unpublished `wrpc-websockets` crate is built against a newer,
//! source-incompatible `wrpc-transport` than the published 0.29 the daemon
//! links, so its `Client`/`Invoke` types cannot cross into our code. Its
//! `split` function, however, only touches `tokio-websockets`/`futures`/
//! `tokio-util` — it turns a `WebSocketStream` into `AsyncRead`/`AsyncWrite`
//! halves — so it is version-agnostic and safe to reuse. We wrap it in
//! [`crate::ws::split`] (which adds eager flushing, required for streaming over
//! a buffered WebSocket sink) and drive the halves through our own
//! (published-0.29) `frame::Server`.

use std::net::SocketAddr;
use std::sync::Arc;

use futures::stream::{StreamExt as _, select_all};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wrpc_transport::frame::{Accept, AcceptError};

use crate::serve::ConnCtx;
use crate::serve::auth::Authenticator;
use crate::serve::http::{HttpState, OriginPolicy, SpaState};

use super::super::ListenerId;

pub(in crate::serve) struct BoundWebSocketListener {
    id: ListenerId,
    listener: tokio::net::TcpListener,
    origins: OriginPolicy,
    drain_timeout: std::time::Duration,
    /// Bound port, captured right after bind so a configured port of `0`
    /// still reports the real port in `/cairn.json` (see `serve::mod`'s
    /// `CairnJsonInfo` assembly).
    pub(in crate::serve) bound_addr: SocketAddr,
}

pub(super) async fn bind(
    id: ListenerId,
    addr: SocketAddr,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<BoundWebSocketListener> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    tracing::info!(listener = %id, %bound_addr, "WS listening");

    Ok(BoundWebSocketListener {
        id,
        listener,
        origins: OriginPolicy::new(cfg.ws_origins.clone()),
        drain_timeout: cfg.shutdown_grace,
        bound_addr,
    })
}

impl BoundWebSocketListener {
    /// `spa` carries this listener's `/cairn.json` facts and (only when
    /// `--web-ui` attaches SPA routes here) the resolved asset source.
    ///
    /// `ui_origin_ports` are the bound ports of the daemon's dedicated
    /// `--web-ui=host:port` listeners; their same-host origins are folded into
    /// this listener's [`OriginPolicy`] so the SPA served there can dial `/ws`
    /// cross-origin without a manual `--ws-origin`.
    pub(in crate::serve) async fn run(
        self,
        daemon: crate::daemon::Daemon,
        auth: Authenticator,
        shutdown: CancellationToken,
        spa: Arc<SpaState>,
        ui_origin_ports: Vec<u16>,
    ) -> anyhow::Result<()> {
        let conns = TaskTracker::new();
        let origins = self.origins.with_ui_ports(ui_origin_ports);
        let state = Arc::new(HttpState::new(
            daemon,
            auth,
            shutdown.clone(),
            origins,
            self.id.clone(),
            conns.clone(),
            spa,
        ));
        let app =
            crate::serve::http::router(state).into_make_service_with_connect_info::<SocketAddr>();

        // axum's graceful shutdown stops accepting and waits for in-flight
        // request handlers. Our `/ws` handler returns 101 immediately and
        // hands the long-lived connection to a tracked task, so we drain those
        // tasks separately below.
        let graceful = {
            let shutdown = shutdown.clone();
            async move { shutdown.cancelled().await }
        };
        axum::serve(self.listener, app)
            .with_graceful_shutdown(graceful)
            .await?;

        // Accept loop stopped. Cooperative drain: each connection task observes
        // `shutdown` and returns, so give them a bounded window to finish before
        // we give up (stragglers are dropped when the runtime tears down).
        conns.close();
        if tokio::time::timeout(self.drain_timeout, conns.wait())
            .await
            .is_err()
        {
            tracing::warn!(listener = %self.id, "WS connections did not drain in time");
        }
        Ok(())
    }
}

/// Wire protocol selected for an upgraded `/ws` connection during the
/// `Sec-WebSocket-Protocol` negotiation in [`crate::serve::http`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WsMode {
    /// One invocation per socket (the default; also `cairn-oneshot-v0`).
    OneShot,
    /// Many invocations muxed over one socket (`cairn-mux-v0`).
    Mux,
}

/// Take over a completed HTTP upgrade and serve wRPC over it in the
/// negotiated [`WsMode`]. Spawned (tracked) from the `/ws` handler once the
/// 101 response is queued.
///
/// The whole body races `shutdown` so a connection stalled mid-upgrade (e.g. a
/// client that never finishes the handshake during daemon shutdown) cannot
/// wedge the drain.
pub(crate) async fn serve_upgraded(
    on_upgrade: hyper::upgrade::OnUpgrade,
    ctx: ConnCtx,
    peer: SocketAddr,
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
    mode: WsMode,
) {
    let served = shutdown.clone();
    let work = async move {
        let upgraded = match on_upgrade.await {
            Ok(upgraded) => upgraded,
            Err(error) => {
                tracing::debug!(%peer, %error, "WS upgrade did not complete");
                return;
            }
        };

        // hyper's `Upgraded` is `Send` but not `Sync`; wrapping it in `TokioIo`
        // and splitting yields halves that ARE `Send + Sync` (the `futures`
        // split guards the shared stream with a `BiLock`), satisfying the frame
        // `Server`'s bounds. See [`crate::ws::split`].
        let io = hyper_util::rt::TokioIo::new(upgraded);
        let ws = wrpc_websockets::tokio_websockets::ServerBuilder::new().serve(io);
        let result = match mode {
            WsMode::OneShot => {
                let (tx, rx) = crate::ws::split(ws);
                serve_one(ctx, tx, rx, daemon, served).await
            }
            WsMode::Mux => super::ws_mux::serve_mux(ctx, ws, daemon, served).await,
        };
        if let Err(error) = result {
            tracing::debug!(%peer, error = %error, "WS connection ended with error");
        }
    };

    tokio::select! {
        _ = shutdown.cancelled() => {}
        () = work => {}
    }
}

/// A one-shot [`Accept`] implementation: yields the already-established WebSocket
/// stream once, then parks. The frame server calls `accept` a single time to
/// route the connection's lone invocation.
struct OneShot<I, O> {
    conn: std::sync::Mutex<Option<(ConnCtx, O, I)>>,
}

impl<I, O> OneShot<I, O> {
    fn new(ctx: ConnCtx, tx: O, rx: I) -> Self {
        Self {
            conn: std::sync::Mutex::new(Some((ctx, tx, rx))),
        }
    }
}

impl<I, O> Accept for &OneShot<I, O>
where
    I: AsyncRead + Send + Sync + Unpin + 'static,
    O: AsyncWrite + Send + Sync + Unpin + 'static,
{
    type Context = ConnCtx;
    type Outgoing = O;
    type Incoming = I;

    async fn accept(&self) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let taken = {
            // Recover from a poisoned lock rather than panicking: the only
            // writer is `new`, so the inner value is always well-formed.
            let mut guard = self.conn.lock().unwrap_or_else(|p| p.into_inner());
            guard.take()
        };
        match taken {
            Some(conn) => Ok(conn),
            // Already consumed: park forever so the frame server's accept future
            // stays pending (it is re-armed after routing the one invocation).
            None => std::future::pending().await,
        }
    }
}

/// Serve exactly one wRPC invocation over an established byte-stream pair.
///
/// Mirrors [`crate::serve::wrpc::run_wrpc_server`] but for the single-invocation
/// WebSocket model: route the one incoming call into the handler registry, then
/// run that invocation's future to completion (this is where a long-lived
/// `attach` stream lives) or until shutdown.
async fn serve_one<I, O>(
    ctx: ConnCtx,
    tx: O,
    rx: I,
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) -> anyhow::Result<()>
where
    I: AsyncRead + Send + Sync + Unpin + 'static,
    O: AsyncWrite + Send + Sync + Unpin + 'static,
{
    let server: wrpc_transport::Server<ConnCtx, I, O, ()> = wrpc_transport::Server::new();
    let invocations = cairn_protocol::serve(&server, daemon).await?;
    let mut invocations = select_all(
        invocations
            .into_iter()
            .map(|(instance, name, stream)| stream.map(move |res| (instance, name, res))),
    );

    let acceptor = OneShot::new(ctx, tx, rx);

    // Route the connection's single invocation into a handler channel.
    tokio::select! {
        _ = shutdown.cancelled() => return Ok(()),
        result = server.accept(&acceptor) => {
            match result {
                Ok(()) => {}
                Err(AcceptError::Send(_)) => anyhow::bail!("wRPC handler channel closed"),
                Err(error) => {
                    tracing::debug!(%error, "WS invocation rejected");
                    return Ok(());
                }
            }
        }
    }

    // Run the connection's single routed invocation to completion. A long-lived
    // `attach` stream lives inside this future; a unary call resolves quickly.
    tokio::select! {
        _ = shutdown.cancelled() => {}
        item = invocations.next() => match item {
            Some((instance, name, Ok(fut))) => {
                if let Err(error) = fut.await {
                    tracing::debug!(%error, %instance, %name, "WS invocation future errored");
                }
            }
            Some((instance, name, Err(error))) => {
                tracing::debug!(%error, %instance, %name, "WS invocation failed");
            }
            None => {}
        }
    }

    Ok(())
}

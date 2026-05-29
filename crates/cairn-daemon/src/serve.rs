//! UDS + WebTransport listeners + wRPC server wiring: `ConnCtx`,
//! `PeerCredListener`, `AuthenticatedWtAccept`, `bind_with_cleanup`,
//! `serve()`, and graceful `drain_sessions`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::sync::CancellationToken;
use wrpc_transport::frame::Accept;

/// Per-connection context handed to every `Handler` method.
#[derive(Clone, Debug)]
pub struct ConnCtx {
    pub identity: crate::identity::Identity,
}

/// A `UnixListener` whose `accept` captures `SO_PEERCRED` into `ConnCtx`
/// before splitting the stream.
pub struct PeerCredListener(pub tokio::net::UnixListener);

impl Accept for &PeerCredListener {
    type Context = ConnCtx;
    type Outgoing = OwnedWriteHalf;
    type Incoming = OwnedReadHalf;

    async fn accept(&self) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let (stream, _addr) = self.0.accept().await?;
        let identity = match stream.peer_cred().ok().map(|c| c.uid()) {
            Some(uid) => crate::identity::Identity::Unix {
                uid,
                username: None,
            },
            None => crate::identity::Identity::Anonymous,
        };
        let (rx, tx) = stream.into_split();
        Ok((ConnCtx { identity }, tx, rx))
    }
}

// ── WebTransport Accept adapter ───────────────────────────────────────────

/// Wraps a `wrpc_transport_web::Client` (which implements `Accept<Context=()>`)
/// and injects a pre-authenticated `ConnCtx` as the context, so the same
/// `Handler<ConnCtx>` impl works for both UDS and WT connections.
struct AuthenticatedWtAccept {
    inner: wrpc_transport_web::Client,
    ctx: ConnCtx,
}

impl Accept for &AuthenticatedWtAccept {
    type Context = ConnCtx;
    type Outgoing = wtransport::SendStream;
    type Incoming = wtransport::RecvStream;

    async fn accept(&self) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let ((), tx, rx) = Accept::accept(&self.inner).await?;
        Ok((self.ctx.clone(), tx, rx))
    }
}

// ── Public entry point ────────────────────────────────────────────────────

/// Bind the daemon socket(s), pump the wRPC accept/serve loops, and block
/// until `shutdown` is cancelled. On shutdown: drain all sessions
/// (SIGTERM + grace), abort the accept/pump tasks, then remove the socket
/// file.
pub async fn serve(
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    // At least one listener must be configured.
    if daemon.cfg.listeners.is_empty() {
        anyhow::bail!("no listeners configured");
    }

    let auth_chain = Arc::new(daemon.build_auth_chain()?);
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // ── UDS listener ─────────────────────────────────────────────────────

    // Find the Unix listener config, if any.
    let unix_path = daemon.cfg.listeners.iter().find_map(|l| match l {
        crate::listen::ListenerConfig::Unix(p) => Some(p.clone()),
        _ => None,
    });

    let listener = if let Some(ref path) = unix_path {
        let l = bind_with_cleanup(path, &daemon.cfg)?;
        tracing::info!(socket = %path.display(), "UDS listening");
        Some(l)
    } else {
        None
    };

    let srv = Arc::new(wrpc_transport::Server::default());

    // Only wire up the accept loop when we have a UDS listener.
    if let Some(listener) = listener {
        let pl = Arc::new(PeerCredListener(listener));
        let srv = Arc::clone(&srv);
        let shutdown = shutdown.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    res = srv.accept(pl.as_ref()) => {
                        if res.is_err() { break; }
                    }
                }
            }
        }));
    }

    let invocations = cairn_protocol::serve(srv.as_ref(), daemon.clone()).await?;

    tasks.push(tokio::spawn(async move {
        use futures::stream::{StreamExt as _, select_all};
        let mut invocations = select_all(
            invocations
                .into_iter()
                .map(|(i, n, s)| s.map(move |r| (i, n, r))),
        );
        while let Some((_i, _n, res)) = invocations.next().await {
            if let Ok(fut) = res {
                tokio::spawn(fut);
            }
        }
    }));

    // ── WebTransport listener ────────────────────────────────────────────

    let wt_addr = daemon.cfg.listeners.iter().find_map(|l| match l {
        crate::listen::ListenerConfig::WebTransport(addr) => Some(*addr),
        _ => None,
    });

    if let Some(addr) = wt_addr {
        let (tls, cert_path, key_path) = resolve_tls(&daemon.cfg)?;
        let rt_dir = crate::config::runtime_dir();
        std::fs::create_dir_all(&rt_dir)?;
        tls.export_hash(&rt_dir.join("cert-hash"))?;

        let identity = wtransport::Identity::load_pemfiles(&cert_path, &key_path)
            .await
            .map_err(|e| anyhow::anyhow!("loading TLS identity: {e}"))?;

        let config = wtransport::ServerConfig::builder()
            .with_bind_address(addr)
            .with_identity(identity)
            .keep_alive_interval(Some(std::time::Duration::from_secs(15)))
            .max_idle_timeout(Some(daemon.cfg.wt_idle_timeout))
            .map_err(|e| anyhow::anyhow!("invalid idle timeout: {e}"))?
            .build();

        let endpoint = wtransport::Endpoint::server(config)?;
        let bound_addr = endpoint.local_addr()?;
        tracing::info!(%bound_addr, hash = %tls.spki_hash_hex(), "WT listening");

        tasks.push(tokio::spawn({
            let daemon = daemon.clone();
            let auth_chain = Arc::clone(&auth_chain);
            let shutdown = shutdown.clone();
            async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        incoming = endpoint.accept() => {
                            let request = match incoming.await {
                                Ok(req) => req,
                                Err(e) => {
                                    tracing::debug!(error = %e, "WT session request error");
                                    continue;
                                }
                            };
                            let conn = match request.accept().await {
                                Ok(c) => c,
                                Err(e) => {
                                    tracing::debug!(error = %e, "WT connection accept error");
                                    continue;
                                }
                            };
                            let daemon = daemon.clone();
                            let auth_chain = Arc::clone(&auth_chain);
                            let shutdown = shutdown.clone();
                            tokio::spawn(async move {
                                serve_wt_connection(conn, &auth_chain, daemon, shutdown).await;
                            });
                        }
                    }
                }
            }
        }));
    }

    // ── Shutdown ─────────────────────────────────────────────────────────

    shutdown.cancelled().await;
    drain_sessions(&daemon, daemon.cfg.shutdown_grace).await;
    for t in &tasks {
        t.abort();
    }
    // Clean up the socket file on shutdown.
    if let Some(ref path) = unix_path {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

// ── Socket lifecycle ──────────────────────────────────────────────────────

/// Create (or recover) the socket file with correct permissions.
///
/// - Creates the parent directory if needed, chmoding it to `dir_mode` only if
///   we created it (so we don't stomp an admin-managed dir).
/// - Probes a pre-existing socket: live → bail; connection-refused → unlink.
/// - Binds and chmods the socket to `socket_mode`.
fn bind_with_cleanup(
    path: &Path,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;

    if let Some(parent) = path.parent() {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        if created {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(cfg.dir_mode))?;
        }
    }

    if path.exists() {
        // Probe: a live daemon means refuse; connection-refused means stale.
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => anyhow::bail!("a daemon is already listening on {}", path.display()),
            Err(_) => {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(cfg.socket_mode))?;
    Ok(listener)
}

// ── Graceful drain ────────────────────────────────────────────────────────

/// Signal all live sessions with SIGTERM and wait up to `grace` for each to
/// exit. Sessions that survive (e.g. they ignore SIGTERM) are dropped; the
/// individual `kill` handler arms a SIGKILL escalation separately if requested
/// — this path is the daemon-shutdown backstop.
async fn drain_sessions(daemon: &crate::daemon::Daemon, grace: std::time::Duration) {
    let entries = daemon.registry.list();
    // Send SIGTERM to all sessions (ignore errors — session may already be gone).
    for e in &entries {
        let _ = e
            .handle()
            .signal(
                nix::sys::signal::Signal::SIGTERM,
                Some("daemon shutting down".into()),
            )
            .await;
    }
    // Wait for each with a shared timeout budget.
    let waits = entries.iter().map(|e| {
        let h = e.handle();
        async move {
            let _ = tokio::time::timeout(grace, h.wait()).await;
        }
    });
    futures::future::join_all(waits).await;
    // Dropping the registry's Arcs (on daemon teardown) is the final SIGKILL backstop.
}

// ── WebTransport per-connection handler ───────────────────────────────────

/// Handle a single authenticated WebTransport connection: authenticate via the
/// auth chain, wrap the connection's bidirectional streams into a per-connection
/// wRPC server, register handlers, and pump invocations until the connection
/// closes or the daemon shuts down.
async fn serve_wt_connection(
    conn: wtransport::Connection,
    auth_chain: &crate::auth::AuthChain,
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) {
    let peer_addr = conn.remote_address();
    let ctx = crate::auth::AuthContext {
        peer_addr,
        token: None,
    };

    let identity = match auth_chain.try_transport(&ctx).await {
        Ok(id) => id,
        Err(crate::auth::AuthError::NotApplicable) => {
            tracing::warn!(%peer_addr, "no auth backend accepted the WT connection");
            return;
        }
        Err(crate::auth::AuthError::Rejected(reason)) => {
            tracing::warn!(%peer_addr, %reason, "WT connection rejected");
            return;
        }
    };

    tracing::info!(%peer_addr, identity = ?identity, "WT connection authenticated");
    let conn_ctx = ConnCtx { identity };

    let acceptor = Arc::new(AuthenticatedWtAccept {
        inner: wrpc_transport_web::Client::from(conn),
        ctx: conn_ctx,
    });

    // Per-connection wRPC server with ConnCtx context type and WT stream
    // handler for graceful shutdown.
    let srv: Arc<
        wrpc_transport::Server<
            ConnCtx,
            wtransport::RecvStream,
            wtransport::SendStream,
            wrpc_transport_web::ConnHandler,
        >,
    > = Arc::new(wrpc_transport::Server::new());

    let accept_task = tokio::spawn({
        let srv = Arc::clone(&srv);
        let acceptor = Arc::clone(&acceptor);
        let shutdown = shutdown.clone();
        async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    res = srv.accept(acceptor.as_ref()) => {
                        if res.is_err() { break; }
                    }
                }
            }
        }
    });

    // Register the same Handler<ConnCtx> for this per-connection server
    // and pump the resulting invocation streams.
    match cairn_protocol::serve(srv.as_ref(), daemon).await {
        Ok(invocations) => {
            use futures::stream::{StreamExt as _, select_all};
            let mut merged = select_all(
                invocations
                    .into_iter()
                    .map(|(i, n, s)| s.map(move |r| (i, n, r))),
            );
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    item = merged.next() => {
                        match item {
                            Some((_i, _n, Ok(fut))) => { tokio::spawn(fut); }
                            Some((_i, _n, Err(_))) => {}
                            None => break,
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!(%peer_addr, error = %e, "WT invocation serve setup failed");
        }
    }

    accept_task.abort();
}

// ── TLS resolution ───────────────────────────────────────────────────────

/// Resolve the TLS configuration for the WebTransport listener.
///
/// Returns the `TlsConfig` and the filesystem paths to cert and key PEM files.
/// If the user provided `--wt-cert` and `--wt-key`, those paths are used.
/// Otherwise a self-signed certificate is generated (or reused) under
/// `crate::config::runtime_dir()/tls/`.
fn resolve_tls(
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<(crate::tls::TlsConfig, PathBuf, PathBuf)> {
    match (&cfg.wt_cert, &cfg.wt_key) {
        (Some(cert), Some(key)) => {
            let tls = crate::tls::TlsConfig::from_pem_files(cert, key)?;
            Ok((tls, cert.clone(), key.clone()))
        }
        (None, None) => {
            let tls_dir = crate::config::runtime_dir().join("tls");
            let tls = crate::tls::TlsConfig::self_signed(&tls_dir)?;
            Ok((tls, tls_dir.join("cert.pem"), tls_dir.join("key.pem")))
        }
        _ => anyhow::bail!("--wt-cert and --wt-key must both be provided, or both omitted"),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

pub(crate) fn username_for(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accept_yields_peer_uid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let pl = PeerCredListener(listener);

        let connect = tokio::spawn(async move {
            let _c = tokio::net::UnixStream::connect(&path).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let (ctx, _tx, _rx) = (&pl).accept().await.unwrap();
        match &ctx.identity {
            crate::identity::Identity::Unix { uid, username } => {
                assert_eq!(*uid, nix_geteuid());
                // Username is resolved lazily in whoami, not at accept time.
                assert!(username.is_none());
            }
            other => panic!("expected Unix identity, got {other:?}"),
        }
        connect.await.unwrap();
    }

    fn nix_geteuid() -> u32 {
        nix::unistd::geteuid().as_raw()
    }
}

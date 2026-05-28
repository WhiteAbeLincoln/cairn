//! UDS listener + wRPC server wiring: `ConnCtx`, `PeerCredListener`,
//! `bind_with_cleanup`, `serve()`, and graceful `drain_sessions`.

use std::sync::Arc;

use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf, UCred};
use tokio_util::sync::CancellationToken;
use wrpc_transport::frame::Accept;

/// Per-connection context handed to every `Handler` method. On UDS the peer
/// credentials identify the caller (for `whoami` and audit). The future WT
/// transport will fill the same shape with the authenticated token identity.
#[derive(Clone, Copy, Debug)]
pub struct ConnCtx {
    pub peer: Option<UCred>,
}

/// A `UnixListener` whose `accept` captures `SO_PEERCRED` into `ConnCtx`
/// before splitting the stream.
pub struct PeerCredListener(pub tokio::net::UnixListener);

impl Accept for &PeerCredListener {
    type Context = ConnCtx;
    type Outgoing = OwnedWriteHalf;
    type Incoming = OwnedReadHalf;

    async fn accept(
        &self,
    ) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer = stream.peer_cred().ok();
        let (rx, tx) = stream.into_split();
        Ok((ConnCtx { peer }, tx, rx))
    }
}

// ── Public entry point ────────────────────────────────────────────────────

/// Bind the daemon socket, pump the wRPC accept/serve loops, and block until
/// `shutdown` is cancelled. On shutdown: drain all sessions (SIGTERM + grace),
/// abort the accept/pump tasks, then remove the socket file.
pub async fn serve(
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let listener = bind_with_cleanup(&daemon.cfg)?;
    tracing::info!(socket = %daemon.cfg.socket_path.display(), "listening");
    let srv = Arc::new(wrpc_transport::Server::default());
    let pl = Arc::new(PeerCredListener(listener));

    let accept = tokio::spawn({
        let srv = Arc::clone(&srv);
        let pl = Arc::clone(&pl);
        let shutdown = shutdown.clone();
        async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    res = srv.accept(pl.as_ref()) => {
                        if res.is_err() { break; }
                    }
                }
            }
        }
    });

    let invocations = cairn_protocol::serve(srv.as_ref(), daemon.clone()).await?;

    let pump = tokio::spawn(async move {
        use futures::stream::{select_all, StreamExt as _};
        let mut invocations =
            select_all(invocations.into_iter().map(|(i, n, s)| s.map(move |r| (i, n, r))));
        while let Some((_i, _n, res)) = invocations.next().await {
            if let Ok(fut) = res {
                tokio::spawn(fut);
            }
        }
    });

    shutdown.cancelled().await;
    drain_sessions(&daemon, daemon.cfg.shutdown_grace).await;
    accept.abort();
    pump.abort();
    let _ = std::fs::remove_file(&daemon.cfg.socket_path);
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
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;

    if let Some(parent) = cfg.socket_path.parent() {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        if created {
            std::fs::set_permissions(
                parent,
                std::fs::Permissions::from_mode(cfg.dir_mode),
            )?;
        }
    }

    if cfg.socket_path.exists() {
        // Probe: a live daemon means refuse; connection-refused means stale.
        match std::os::unix::net::UnixStream::connect(&cfg.socket_path) {
            Ok(_) => anyhow::bail!(
                "a daemon is already listening on {}",
                cfg.socket_path.display()
            ),
            Err(_) => {
                let _ = std::fs::remove_file(&cfg.socket_path);
            }
        }
    }

    let listener = tokio::net::UnixListener::bind(&cfg.socket_path)?;
    std::fs::set_permissions(
        &cfg.socket_path,
        std::fs::Permissions::from_mode(cfg.socket_mode),
    )?;
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
        let _ = e.handle().signal(nix::sys::signal::Signal::SIGTERM).await;
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
        assert_eq!(ctx.peer.unwrap().uid(), nix_geteuid());
        connect.await.unwrap();
    }

    fn nix_geteuid() -> u32 {
        nix::unistd::geteuid().as_raw()
    }
}

//! Transport listener orchestration and shared wRPC serving.
//!
//! Each transport owns its full lifecycle: bind, accept, authenticate,
//! and run a wRPC server. `serve()` spawns transport tasks, waits for
//! shutdown, drains sessions, and cleans up.

use std::fmt;
use std::path::PathBuf;

use anyhow::Context as _;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

mod auth;
mod transport;
mod wrpc;

/// Per-connection context handed to every `Handler` method.
#[derive(Clone, Debug)]
pub struct ConnCtx {
    pub identity: crate::identity::Identity,
}

/// Bind the configured daemon listeners, accept raw transport connections, and
/// serve the cairn wRPC protocol until `shutdown` is cancelled.
pub async fn serve(
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
    auth_chain: Option<crate::auth::AuthChain>,
) -> anyhow::Result<()> {
    if daemon.cfg.listeners.is_empty() {
        anyhow::bail!("no listeners configured");
    }

    let auth = auth::Authenticator::new(&daemon, auth_chain)?;
    let mut tasks = JoinSet::new();

    let spawned =
        transport::bind_and_spawn(&daemon.cfg, &daemon, &auth, &shutdown, &mut tasks).await?;

    // Wait for shutdown or a transport task to fail.
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            result = tasks.join_next(), if !tasks.is_empty() => {
                match result {
                    Some(Ok(Ok(()))) => {
                        if !shutdown.is_cancelled() {
                            anyhow::bail!("transport task exited unexpectedly");
                        }
                    }
                    Some(Ok(Err(error))) => return Err(error).context("transport task failed"),
                    Some(Err(error)) if error.is_cancelled() => {}
                    Some(Err(error)) => return Err(error).context("transport task panicked"),
                    None => break,
                }
            }
        }
    }

    // Graceful shutdown: drain sessions, then abort remaining tasks.
    drain_sessions(&daemon, daemon.cfg.shutdown_grace).await;
    tasks.shutdown().await;

    for path in &spawned.unix_paths {
        let _ = std::fs::remove_file(path);
    }

    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct ListenerId {
    index: usize,
    label: String,
}

impl ListenerId {
    fn new(index: usize, listener: &crate::listen::ListenerConfig) -> Self {
        let label = match listener {
            crate::listen::ListenerConfig::Unix(path) => {
                format!("unix://{}", path.display())
            }
            crate::listen::ListenerConfig::WebTransport(addr) => {
                format!("https://{addr}")
            }
        };

        Self { index, label }
    }
}

impl fmt::Display for ListenerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} #{}", self.label, self.index)
    }
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

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[tokio::test]
    async fn serve_binds_multiple_unix_listeners() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.sock");
        let path_b = dir.path().join("b.sock");
        let cfg = crate::config::DaemonConfig {
            listeners: vec![
                crate::listen::ListenerConfig::Unix(path_a.clone()),
                crate::listen::ListenerConfig::Unix(path_b.clone()),
            ],
            ..crate::config::DaemonConfig::default()
        };
        let daemon = crate::daemon::Daemon::new(cfg).unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone(), None));

        wait_for_path(&path_a).await;
        wait_for_path(&path_b).await;

        for path in [&path_a, &path_b] {
            let client = wrpc_transport::unix::Client::from(path.to_path_buf());
            let info = cairn_protocol::client::cairn::daemon::meta::version(&client, (), None)
                .await
                .unwrap();
            assert_eq!(info.protocol, "cairn:daemon@0.1.0");
        }

        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert!(!path_a.exists());
        assert!(!path_b.exists());
    }

    async fn wait_for_path(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("path did not appear in time: {}", path.display());
    }
}

//! Transport listener orchestration and shared wRPC serving.
//!
//! Each transport owns its full lifecycle: bind, accept, authenticate,
//! and run a wRPC server. `serve()` spawns transport tasks, waits for
//! shutdown, drains sessions, and cleans up.

use std::fmt;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub(crate) mod assets;
mod auth;
pub(crate) mod cairn_json;
mod http;
pub(crate) mod transport;
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

    // holds guards for transports that require cleanup (e.g. UnixListenerGuard removes socket file on drop)
    let _spawned =
        transport::bind_and_spawn(&daemon.cfg, &daemon, &auth, &shutdown, &mut tasks).await?;

    // Wait for shutdown or a transport task to fail.
    let mut transport_error = None;
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            result = tasks.join_next(), if !tasks.is_empty() => {
                match result {
                    Some(Ok(Ok(()))) => {
                        if !shutdown.is_cancelled() {
                            transport_error = Some(anyhow::anyhow!("transport task exited unexpectedly"));
                            break;
                        }
                    }
                    Some(Ok(Err(error))) => {
                        transport_error = Some(error.context("transport task failed"));
                        break;
                    }
                    Some(Err(error)) if error.is_cancelled() => {}
                    Some(Err(error)) => {
                        transport_error = Some(
                            anyhow::Error::from(error).context("transport task panicked"),
                        );
                        break;
                    }
                    None => break,
                }
            }
        }
    }

    // Always drain sessions, even after a transport failure — managed
    // processes deserve the SIGTERM grace path regardless of why we're
    // shutting down.
    drain_sessions(&daemon, daemon.cfg.shutdown_grace).await;
    drain_proxies(&daemon).await;
    tasks.shutdown().await;

    match transport_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Drain live proxy sessions with a bounded grace period. hudsucker's
/// graceful shutdown waits unboundedly for in-flight connections to finish,
/// so a wedged exchange (a synthetic response whose interceptor never sends
/// `ResponseEnd`, or a child ignoring SIGTERM) can otherwise hang daemon
/// shutdown forever. If `grace` elapses first we log and move on — the
/// subsequent `ProxySession` drop (token-cancel) and `tasks.shutdown()` force
/// teardown of whatever's left.
async fn drain_proxies(daemon: &crate::daemon::Daemon) {
    let proxies: Vec<_> = daemon
        .registry
        .list()
        .into_iter()
        .filter_map(|entry| entry.proxy())
        .collect();
    if proxies.is_empty() {
        return;
    }
    let grace = daemon.cfg.shutdown_grace;
    let completed = drain_with_timeout(proxies.iter().map(|proxy| proxy.shutdown()), grace).await;
    if !completed {
        tracing::warn!(
            grace_secs = grace.as_secs_f64(),
            proxy_count = proxies.len(),
            "proxy shutdown did not complete within the shutdown grace period; \
             proceeding with forced teardown"
        );
    }
}

/// Await `shutdowns` concurrently, bounded by `grace`. Returns `true` if every
/// future resolved within the grace period, `false` if `grace` elapsed with
/// futures still pending (in which case the caller should proceed and let a
/// forcible teardown path take over — this helper never blocks past `grace`).
async fn drain_with_timeout(
    shutdowns: impl IntoIterator<Item = impl std::future::Future<Output = ()>>,
    grace: std::time::Duration,
) -> bool {
    tokio::time::timeout(grace, futures::future::join_all(shutdowns))
        .await
        .is_ok()
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
            crate::listen::ListenerConfig::WebSocket(addr) => {
                format!("ws://{addr}")
            }
        };

        Self { index, label }
    }

    /// A `ListenerId` for the dedicated `--web-ui=host:port` listener, which
    /// isn't part of `cfg.listeners` and so has no `ListenerConfig` to derive
    /// a label from.
    fn web_ui(index: usize, addr: std::net::SocketAddr) -> Self {
        Self {
            index,
            label: format!("web-ui://{addr}"),
        }
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

    #[tokio::test(start_paused = true)]
    async fn drain_with_timeout_completes_when_all_shutdowns_finish() {
        let grace = std::time::Duration::from_secs(1);
        let completed = drain_with_timeout(
            vec![futures::future::ready(()), futures::future::ready(())],
            grace,
        )
        .await;
        assert!(
            completed,
            "expected drain to report completion, not a timeout"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_with_timeout_times_out_on_a_stuck_future() {
        let grace = std::time::Duration::from_millis(50);
        let completed = drain_with_timeout(vec![futures::future::pending::<()>()], grace).await;
        assert!(
            !completed,
            "a future that never resolves must not block the drain past `grace`"
        );
    }
}

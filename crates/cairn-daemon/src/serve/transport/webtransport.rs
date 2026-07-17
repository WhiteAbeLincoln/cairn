use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use wrpc_transport::Accept;

use crate::serve::ConnCtx;
use crate::serve::auth::Authenticator;
use crate::serve::transport::TransportListener;
use crate::serve::wrpc::run_wrpc_server;

use super::super::ListenerId;

pub(in crate::serve) struct BoundWebTransportListener {
    id: ListenerId,
    endpoint: wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>,
    connect_timeout: std::time::Duration,
    pending_limit: Arc<Semaphore>,
    /// `/cairn.json` facts for this listener, captured right after bind (see
    /// `serve::mod`'s `CairnJsonInfo` assembly).
    pub(in crate::serve) cairn_json: crate::serve::cairn_json::WtInfo,
}

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

pub(super) async fn bind(
    id: ListenerId,
    addr: SocketAddr,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<BoundWebTransportListener> {
    let (tls, cert_path, key_path) = resolve_tls(cfg)?;
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
        .max_idle_timeout(Some(cfg.wt_idle_timeout))
        .map_err(|e| anyhow::anyhow!("invalid idle timeout: {e}"))?
        .build();

    let endpoint = wtransport::Endpoint::server(config)?;
    let bound_addr = endpoint.local_addr()?;
    tracing::info!(
        listener = %id,
        %bound_addr,
        hash = %tls.spki_hash_hex(),
        "WT listening"
    );

    // `--wt-cert`/`--wt-key` (a user-supplied cert) omits the pinned hash
    // from `/cairn.json`: only the self-signed path needs out-of-band
    // pinning to be trusted by WebTransport clients.
    let cert_hash = if cfg.wt_tls.is_some() {
        None
    } else {
        Some(tls.spki_hash_hex())
    };

    Ok(BoundWebTransportListener {
        id,
        endpoint,
        connect_timeout: cfg.wt_connect_timeout,
        pending_limit: Arc::new(Semaphore::new(cfg.wt_max_pending)),
        cairn_json: crate::serve::cairn_json::WtInfo {
            port: bound_addr.port(),
            cert_hash,
        },
    })
}

impl TransportListener for BoundWebTransportListener {
    async fn run(
        self,
        daemon: crate::daemon::Daemon,
        auth: Authenticator,
        shutdown: CancellationToken,
    ) -> anyhow::Result<()> {
        let mut connections = JoinSet::new();

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,

                incoming = self.endpoint.accept() => {
                    let permit = match self.pending_limit.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            tracing::warn!(
                                listener = %self.id,
                                "WT pending connection limit reached, dropping"
                            );
                            continue;
                        }
                    };

                    let id = self.id.clone();
                    let connect_timeout = self.connect_timeout;
                    let auth = auth.clone();
                    let daemon = daemon.clone();
                    let shutdown = shutdown.clone();
                    connections.spawn(async move {
                        let conn = match accept_connection(connect_timeout, incoming).await {
                            Ok(conn) => conn,
                            Err(error) => {
                                tracing::debug!(
                                    listener = %id,
                                    error = %error,
                                    "WT connection accept failed"
                                );
                                return;
                            }
                        };

                        let peer_addr = conn.remote_address();
                        let identity = match auth.authenticate_network(peer_addr).await {
                            Ok(id) => id,
                            Err(error) => {
                                tracing::warn!(
                                    listener = %id,
                                    %peer_addr,
                                    %error,
                                    "WT connection rejected"
                                );
                                return;
                            }
                        };

                        tracing::info!(%peer_addr, ?identity, "WT connection authenticated");
                        drop(permit);
                        let ctx = ConnCtx { identity };
                        let acceptor = AuthenticatedWtAccept {
                            inner: wrpc_transport_web::Client::from(conn),
                            ctx,
                        };

                        if let Err(error) = run_wrpc_server::<
                            _,
                            wtransport::RecvStream,
                            wtransport::SendStream,
                            wrpc_transport_web::ConnHandler,
                        >(acceptor, daemon, shutdown).await {
                            tracing::debug!(%peer_addr, error = %error, "WT connection ended with error");
                        }
                    });
                }

                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result
                        && !error.is_cancelled()
                    {
                        tracing::error!(error = %error, "WT connection task failed");
                    }
                }
            }
        }

        // Cooperative drain: give connection tasks time to notice shutdown
        // and exit cleanly before forcefully aborting stragglers.
        let drain = tokio::time::timeout(self.connect_timeout, async {
            while connections.join_next().await.is_some() {}
        });
        if drain.await.is_err() {
            tracing::warn!(listener = %self.id, "WT connections did not drain in time, aborting");
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        }
        Ok(())
    }
}

async fn accept_connection(
    timeout: std::time::Duration,
    incoming: wtransport::endpoint::IncomingSession,
) -> anyhow::Result<wtransport::Connection> {
    let request = tokio::time::timeout(timeout, incoming)
        .await
        .map_err(|_| anyhow::anyhow!("WT session request timed out"))?
        .map_err(|e| anyhow::anyhow!("WT session request error: {e}"))?;

    let conn = tokio::time::timeout(timeout, request.accept())
        .await
        .map_err(|_| anyhow::anyhow!("WT connection accept timed out"))?
        .map_err(|e| anyhow::anyhow!("WT connection accept error: {e}"))?;

    Ok(conn)
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
    match &cfg.wt_tls {
        Some(id) => {
            let tls = crate::tls::TlsConfig::from_pem_files(&id.cert, &id.key)?;
            Ok((tls, id.cert.clone(), id.key.clone()))
        }
        None => {
            let tls_dir = crate::config::runtime_dir().join("tls");
            let tls = crate::tls::TlsConfig::self_signed(&tls_dir)?;
            Ok((tls, tls_dir.join("cert.pem"), tls_dir.join("key.pem")))
        }
    }
}

//! The dedicated `--web-ui=host:port` HTTP listener: serves only the SPA and
//! `/cairn.json` (no `/ws`, no wRPC, no auth — the contents are public). Bind
//! owns the TCP accept loop like the `ws://` listener does; HTTP routing
//! lives in `crate::serve::http::ui_router`.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::serve::http::SpaState;

use super::super::ListenerId;

pub(in crate::serve) struct BoundUiListener {
    id: ListenerId,
    listener: tokio::net::TcpListener,
}

pub(in crate::serve) async fn bind(
    id: ListenerId,
    addr: SocketAddr,
) -> anyhow::Result<BoundUiListener> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound_addr = listener.local_addr()?;
    tracing::info!(listener = %id, %bound_addr, "web UI listening");
    Ok(BoundUiListener { id, listener })
}

impl BoundUiListener {
    pub(in crate::serve) async fn run(
        self,
        shutdown: CancellationToken,
        spa: Arc<SpaState>,
    ) -> anyhow::Result<()> {
        let app = crate::serve::http::ui_router(spa).into_make_service();

        // Static asset requests are short-lived (no upgrade/hijack like
        // `/ws`), so axum's own graceful shutdown (stop accepting, let
        // in-flight handlers finish) is sufficient — no separate connection
        // draining needed here.
        let graceful = {
            let shutdown = shutdown.clone();
            async move { shutdown.cancelled().await }
        };
        axum::serve(self.listener, app)
            .with_graceful_shutdown(graceful)
            .await?;

        tracing::debug!(listener = %self.id, "web UI listener stopped");
        Ok(())
    }
}

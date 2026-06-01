//! `Daemon` struct — `Clone`-able root handle that impls both generated
//! `Handler` traits by delegating to `handlers::*`.

use std::sync::Arc;

use cairn_protocol::cairn::daemon::types::{
    CallContext, Error as WireError, SessionInfo, SessionSpec, Signal,
};
use cairn_protocol::exports::cairn::daemon::meta::VersionInfo;

use crate::config::DaemonConfig;
use crate::registry::SessionRegistry;
use crate::serve::ConnCtx;
use crate::{handlers, handlers::sessions as sess};

/// The daemon root: a cheaply-cloneable handle that carries the registry and
/// resolved config. Handed to `cairn_protocol::serve` as the Handler impl.
#[derive(Clone)]
pub struct Daemon {
    pub registry: Arc<SessionRegistry>,
    pub cfg: Arc<DaemonConfig>,
}

impl Daemon {
    pub fn new(cfg: DaemonConfig) -> Self {
        Self {
            registry: Arc::new(SessionRegistry::new()),
            cfg: Arc::new(cfg),
        }
    }

    /// Build the auth chain from the configured backend names.
    pub fn build_auth_chain(&self) -> anyhow::Result<crate::auth::AuthChain> {
        let mut backends: Vec<Box<dyn crate::auth::AuthBackend>> = Vec::new();
        for name in &self.cfg.auth_backends {
            match name.as_str() {
                "none" => backends.push(Box::new(crate::auth::none::NoneBackend)),
                "tailscale" => {
                    backends.push(Box::new(
                        crate::auth::tailscale::TailscaleBackend::new()
                            .map_err(|e| anyhow::anyhow!("tailscale auth backend init: {e}"))?,
                    ));
                }
                other => anyhow::bail!("unknown auth backend: {other:?}"),
            }
        }
        if backends.is_empty() {
            anyhow::bail!("at least one --auth backend is required");
        }
        Ok(crate::auth::AuthChain::new(backends))
    }
}

// ── sessions::Handler<ConnCtx> ────────────────────────────────────────────

impl cairn_protocol::exports::cairn::daemon::sessions::Handler<ConnCtx> for Daemon {
    async fn list_all(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        Ok(sess::list_all(self).await)
    }

    async fn inspect(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
    ) -> anyhow::Result<Result<SessionInfo, WireError>> {
        Ok(sess::inspect(self, id).await)
    }

    async fn create(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        spec: SessionSpec,
    ) -> anyhow::Result<Result<SessionInfo, WireError>> {
        Ok(sess::create(self, spec).await)
    }

    async fn rename(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
        new_name: String,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::rename(self, id, new_name).await)
    }

    async fn restart(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
        force: bool,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::restart(self, id, force).await)
    }

    async fn kill(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
        sig: Signal,
        grace_ms: Option<u32>,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::kill(self, id, sig, grace_ms).await)
    }

    async fn kick(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
        client: Option<String>,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::kick(self, id, client).await)
    }

    // ── Deferred streaming operations (Plan 3) ───────────────────────────

    async fn wait(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<
                dyn std::future::Future<Output = cairn_protocol::cairn::daemon::types::ExitStatus>
                    + Send
                    + 'static,
            >,
        >,
    > {
        crate::handlers::wait::wait(self, id).await
    }

    async fn logs(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
        window: cairn_protocol::cairn::daemon::types::LogWindow,
        follow: bool,
    ) -> anyhow::Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>,
    > {
        crate::handlers::logs::logs(self, id, window, follow).await
    }

    async fn attach(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
        init: cairn_protocol::cairn::daemon::types::AttachInit,
        events: std::pin::Pin<
            Box<
                dyn futures::Stream<Item = Vec<cairn_protocol::cairn::daemon::types::ClientEvent>>
                    + Send
                    + 'static,
            >,
        >,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = Vec<cairn_protocol::cairn::daemon::types::ServerEvent>>
                    + Send
                    + 'static,
            >,
        >,
    > {
        Ok(crate::handlers::attach::attach(self, id, init, events).await)
    }

    async fn send(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        id: String,
        chunks: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(crate::handlers::send::send(self, id, chunks).await)
    }
}

// ── meta::Handler<ConnCtx> ────────────────────────────────────────────────

impl cairn_protocol::exports::cairn::daemon::meta::Handler<ConnCtx> for Daemon {
    async fn version(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
    ) -> anyhow::Result<VersionInfo> {
        Ok(handlers::meta::version())
    }

    async fn authenticate(
        &self,
        _ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
        token: String,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(handlers::meta::authenticate(token))
    }

    async fn whoami(
        &self,
        ctx: ConnCtx,
        _call_ctx: Option<CallContext>,
    ) -> anyhow::Result<Result<String, WireError>> {
        Ok(handlers::meta::whoami(&ctx))
    }
}

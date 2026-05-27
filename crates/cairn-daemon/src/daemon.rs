//! `Daemon` struct — `Clone`-able root handle that impls both generated
//! `Handler` traits by delegating to `handlers::*`.

use std::sync::Arc;

use cairn_protocol::cairn::daemon::types::{Error as WireError, SessionInfo, SessionSpec, Signal};
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
        Self { registry: Arc::new(SessionRegistry::new()), cfg: Arc::new(cfg) }
    }
}

// ── sessions::Handler<ConnCtx> ────────────────────────────────────────────

impl cairn_protocol::exports::cairn::daemon::sessions::Handler<ConnCtx> for Daemon {
    async fn list_all(&self, _ctx: ConnCtx) -> anyhow::Result<Vec<SessionInfo>> {
        Ok(sess::list_all(self).await)
    }

    async fn inspect(
        &self,
        _ctx: ConnCtx,
        id: String,
    ) -> anyhow::Result<Result<SessionInfo, WireError>> {
        Ok(sess::inspect(self, id).await)
    }

    async fn create(
        &self,
        _ctx: ConnCtx,
        spec: SessionSpec,
    ) -> anyhow::Result<Result<SessionInfo, WireError>> {
        Ok(sess::create(self, spec).await)
    }

    async fn rename(
        &self,
        _ctx: ConnCtx,
        id: String,
        new_name: String,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::rename(self, id, new_name).await)
    }

    async fn restart(
        &self,
        _ctx: ConnCtx,
        id: String,
        force: bool,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::restart(self, id, force).await)
    }

    async fn kill(
        &self,
        _ctx: ConnCtx,
        id: String,
        sig: Signal,
        grace_ms: Option<u32>,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::kill(self, id, sig, grace_ms).await)
    }

    async fn kick(
        &self,
        _ctx: ConnCtx,
        id: String,
        client: Option<String>,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(sess::kick(self, id, client).await)
    }

    // ── Deferred streaming operations (Plan 3) ───────────────────────────

    async fn wait(
        &self,
        _ctx: ConnCtx,
        id: String,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = cairn_protocol::cairn::daemon::types::ExitStatus,
                    > + Send
                    + 'static,
            >,
        >,
    > {
        crate::handlers::wait::wait(self, id).await
    }

    async fn logs(
        &self,
        _ctx: ConnCtx,
        _id: String,
        _window: cairn_protocol::cairn::daemon::types::LogWindow,
        _follow: bool,
    ) -> anyhow::Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>,
    > {
        unimplemented!("served in Plan 3")
    }

    async fn attach(
        &self,
        _ctx: ConnCtx,
        _id: String,
        _init: cairn_protocol::cairn::daemon::types::AttachInit,
        _events: std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = Vec<cairn_protocol::cairn::daemon::types::ClientEvent>,
                    > + Send
                    + 'static,
            >,
        >,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = Vec<cairn_protocol::cairn::daemon::types::ServerEvent>,
                    > + Send
                    + 'static,
            >,
        >,
    > {
        unimplemented!("served in Plan 3")
    }

    async fn send(
        &self,
        _ctx: ConnCtx,
        id: String,
        chunks: std::pin::Pin<
            Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>,
        >,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(crate::handlers::send::send(self, id, chunks).await)
    }
}

// ── meta::Handler<ConnCtx> ────────────────────────────────────────────────

impl cairn_protocol::exports::cairn::daemon::meta::Handler<ConnCtx> for Daemon {
    async fn version(&self, _ctx: ConnCtx) -> anyhow::Result<VersionInfo> {
        Ok(handlers::meta::version())
    }

    async fn authenticate(
        &self,
        _ctx: ConnCtx,
        token: String,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(handlers::meta::authenticate(token))
    }

    async fn whoami(&self, ctx: ConnCtx) -> anyhow::Result<Result<String, WireError>> {
        Ok(handlers::meta::whoami(&ctx))
    }
}

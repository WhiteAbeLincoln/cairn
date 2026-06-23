//! `Daemon` struct — `Clone`-able root handle that impls both generated
//! `Handler` traits by delegating to `handlers::*`.

use std::sync::Arc;

use cairn_protocol::cairn::daemon::types::{
    CallContext, Error as WireError, SessionInfo, SessionSpec, Signal,
};
use cairn_protocol::exports::cairn::daemon::meta::VersionInfo;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use tracing::Instrument;

use crate::config::{AuthBackendKind, DaemonConfig};
use crate::registry::SessionRegistry;
use crate::serve::ConnCtx;
use crate::telemetry::link_remote_context;
use crate::{handlers, handlers::sessions as sess};

/// The daemon root: a cheaply-cloneable handle that carries the registry and
/// resolved config. Handed to `cairn_protocol::serve` as the Handler impl.
#[derive(Clone)]
pub struct Daemon {
    pub registry: Arc<SessionRegistry>,
    pub cfg: Arc<DaemonConfig>,
}

impl Daemon {
    pub fn new(cfg: DaemonConfig) -> anyhow::Result<Self> {
        let has_network = cfg.listeners.iter().any(|l| !l.is_unix());
        let has_exposed = cfg.listeners.iter().any(|l| !l.is_loopback());
        let has_unix = cfg.listeners.iter().any(|l| l.is_unix());

        if has_exposed && cfg.auth_backends.is_empty() {
            anyhow::bail!(
                "non-loopback listener configured but no --auth backend specified; \
                 authentication is required for externally-reachable transports"
            );
        }

        if !has_network && !cfg.auth_backends.is_empty() {
            tracing::warn!("--auth has no effect without a network listener");
        }
        if !has_unix && (cfg.dir_mode != 0o700 || cfg.socket_mode != 0o600) {
            tracing::warn!("--dir-mode / --socket-mode have no effect without a unix:// listener");
        }
        if cfg.listeners.iter().any(|l| l.is_wt()) && cfg.wt_tls.is_none() {
            tracing::warn!(
                "https:// (WebTransport) listener configured but \
                 --wt-cert / --wt-key not set; using self-signed certificate"
            );
        }

        Ok(Self {
            registry: Arc::new(SessionRegistry::new()),
            cfg: Arc::new(cfg),
        })
    }

    /// Build the auth chain from the configured backend kinds.
    /// Returns `None` for UDS-only configurations that require no auth chain.
    pub fn build_auth_chain(&self) -> anyhow::Result<Option<crate::auth::AuthChain>> {
        let has_network = self.cfg.listeners.iter().any(|l| !l.is_unix());
        if !has_network {
            return Ok(None);
        }
        if self.cfg.auth_backends.is_empty() {
            return Ok(None);
        }
        let mut backends: Vec<Box<dyn crate::auth::AuthBackend>> = Vec::new();
        for kind in &self.cfg.auth_backends {
            match kind {
                AuthBackendKind::Tailscale => {
                    backends.push(Box::new(
                        crate::auth::tailscale::TailscaleBackend::new()
                            .map_err(|e| anyhow::anyhow!("tailscale auth backend init: {e}"))?,
                    ));
                }
            }
        }
        Ok(Some(crate::auth::AuthChain::new(backends)))
    }
}

// ── sessions::Handler<ConnCtx> ────────────────────────────────────────────

impl cairn_protocol::exports::cairn::daemon::sessions::Handler<ConnCtx> for Daemon {
    async fn list_all(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
    ) -> anyhow::Result<Vec<SessionInfo>> {
        let span = tracing::info_span!("rpc", method = "sessions.list_all");
        link_remote_context(&span, &call_ctx);
        Ok(sess::list_all(self).instrument(span).await)
    }

    async fn inspect(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
    ) -> anyhow::Result<Result<SessionInfo, WireError>> {
        let span = tracing::info_span!("rpc", method = "sessions.inspect");
        link_remote_context(&span, &call_ctx);
        Ok(sess::inspect(self, id).instrument(span).await)
    }

    async fn create(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        spec: SessionSpec,
    ) -> anyhow::Result<Result<SessionInfo, WireError>> {
        let span = tracing::info_span!("rpc", method = "sessions.create");
        link_remote_context(&span, &call_ctx);
        Ok(sess::create(self, spec).instrument(span).await)
    }

    async fn rename(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
        new_name: String,
    ) -> anyhow::Result<Result<(), WireError>> {
        let span = tracing::info_span!("rpc", method = "sessions.rename");
        link_remote_context(&span, &call_ctx);
        Ok(sess::rename(self, id, new_name).instrument(span).await)
    }

    async fn restart(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
        force: bool,
    ) -> anyhow::Result<Result<(), WireError>> {
        let span = tracing::info_span!("rpc", method = "sessions.restart");
        link_remote_context(&span, &call_ctx);
        Ok(sess::restart(self, id, force).instrument(span).await)
    }

    async fn kill(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
        sig: Signal,
        grace_ms: Option<u32>,
    ) -> anyhow::Result<Result<(), WireError>> {
        let span = tracing::info_span!("rpc", method = "sessions.kill");
        link_remote_context(&span, &call_ctx);
        Ok(sess::kill(self, id, sig, grace_ms).instrument(span).await)
    }

    async fn kick(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
        client: Option<String>,
    ) -> anyhow::Result<Result<(), WireError>> {
        let span = tracing::info_span!("rpc", method = "sessions.kick");
        link_remote_context(&span, &call_ctx);
        Ok(sess::kick(self, id, client).instrument(span).await)
    }

    // ── streaming operations ───────────────────────────

    async fn wait(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
    ) -> anyhow::Result<BoxFuture<'static, cairn_protocol::cairn::daemon::types::ExitStatus>> {
        let span = tracing::info_span!("rpc", method = "sessions.wait");
        link_remote_context(&span, &call_ctx);
        crate::handlers::wait::wait(self, id).instrument(span).await
    }

    async fn logs(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
        window: cairn_protocol::cairn::daemon::types::LogWindow,
        follow: bool,
    ) -> anyhow::Result<BoxStream<'static, Vec<bytes::Bytes>>> {
        let span = tracing::info_span!("rpc", method = "sessions.logs");
        link_remote_context(&span, &call_ctx);
        let _enter = span.enter();
        crate::handlers::logs::logs(self, id, window, follow).await
    }

    async fn attach(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
        init: cairn_protocol::cairn::daemon::types::AttachInit,
        events: BoxStream<'static, Vec<cairn_protocol::cairn::daemon::types::ClientEvent>>,
    ) -> anyhow::Result<BoxStream<'static, Vec<cairn_protocol::cairn::daemon::types::ServerEvent>>>
    {
        let span = tracing::info_span!("rpc", method = "sessions.attach");
        link_remote_context(&span, &call_ctx);
        let _enter = span.enter();
        Ok(crate::handlers::attach::attach(self, id, init, events).await)
    }

    async fn send(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        id: String,
        chunks: BoxStream<'static, Vec<bytes::Bytes>>,
    ) -> anyhow::Result<Result<(), WireError>> {
        let span = tracing::info_span!("rpc", method = "sessions.send");
        link_remote_context(&span, &call_ctx);
        let _enter = span.enter();
        Ok(crate::handlers::send::send(self, id, chunks).await)
    }
}

// ── meta::Handler<ConnCtx> ────────────────────────────────────────────────

impl cairn_protocol::exports::cairn::daemon::meta::Handler<ConnCtx> for Daemon {
    async fn version(
        &self,
        _ctx: ConnCtx,
        call_ctx: Option<CallContext>,
    ) -> anyhow::Result<VersionInfo> {
        let span = tracing::info_span!("rpc", method = "meta.version");
        link_remote_context(&span, &call_ctx);
        let _enter = span.enter();
        Ok(handlers::meta::version())
    }

    async fn authenticate(
        &self,
        ctx: ConnCtx,
        call_ctx: Option<CallContext>,
        token: String,
    ) -> anyhow::Result<Result<(), WireError>> {
        let span = tracing::info_span!("rpc", method = "meta.authenticate");
        link_remote_context(&span, &call_ctx);
        let _enter = span.enter();
        Ok(handlers::meta::authenticate(&ctx, token))
    }

    async fn whoami(
        &self,
        ctx: ConnCtx,
        call_ctx: Option<CallContext>,
    ) -> anyhow::Result<Result<String, WireError>> {
        let span = tracing::info_span!("rpc", method = "meta.whoami");
        link_remote_context(&span, &call_ctx);
        let _enter = span.enter();
        Ok(handlers::meta::whoami(&ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthBackendKind;
    use crate::listen::ListenerConfig;

    #[test]
    fn new_rejects_exposed_listener_without_auth() {
        let cfg = DaemonConfig {
            listeners: vec![ListenerConfig::WebTransport(
                "0.0.0.0:9443".parse().unwrap(),
            )],
            ..DaemonConfig::default()
        };
        let err = Daemon::new(cfg)
            .err()
            .expect("should reject non-loopback listener without auth");
        assert!(
            err.to_string().contains("--auth"),
            "expected --auth hint, got: {err}"
        );
    }

    #[test]
    fn new_accepts_uds_without_auth() {
        let cfg = DaemonConfig::default();
        assert!(Daemon::new(cfg).is_ok());
    }

    #[test]
    fn new_accepts_loopback_wt_without_auth() {
        let cfg = DaemonConfig {
            listeners: vec![ListenerConfig::WebTransport(
                "127.0.0.1:9443".parse().unwrap(),
            )],
            ..DaemonConfig::default()
        };
        assert!(Daemon::new(cfg).is_ok());
    }

    #[test]
    fn new_accepts_exposed_listener_with_auth() {
        let cfg = DaemonConfig {
            listeners: vec![ListenerConfig::WebTransport(
                "0.0.0.0:9443".parse().unwrap(),
            )],
            auth_backends: vec![AuthBackendKind::Tailscale],
            ..DaemonConfig::default()
        };
        assert!(Daemon::new(cfg).is_ok());
    }
}

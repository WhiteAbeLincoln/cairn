use std::sync::Arc;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ListenerId;
use super::assets::Assets;
use super::auth::Authenticator;
use super::cairn_json::CairnJsonInfo;
use super::http::SpaState;

pub(super) mod unix;
pub(super) mod web_ui;
pub(super) mod websocket;
pub(super) mod webtransport;

pub(super) struct SpawnedTransports {
    _unix_guards: Vec<unix::UnixListenerGuard>,
}

pub trait TransportListener {
    fn run(
        self,
        daemon: crate::daemon::Daemon,
        auth: Authenticator,
        shutdown: CancellationToken,
    ) -> impl futures::Future<Output = anyhow::Result<()>> + Send + 'static;
}

/// Bind every configured listener and spawn its serve task.
///
/// `ws://` listeners are bound eagerly (a plain TCP bind, cheap and
/// order-independent) but their `run()` — which builds their HTTP state — is
/// deferred until every listener in `cfg.listeners` has been bound. That's
/// because `/cairn.json` needs facts (bound port, WT cert hash) from *every*
/// listener, including ones that appear later in the list than a given
/// `ws://` entry. Unix and WebTransport listeners don't serve `/cairn.json`
/// themselves, so binding and spawning them immediately is fine — we just
/// capture the bits of WT info `/cairn.json` needs along the way.
pub(super) async fn bind_and_spawn(
    cfg: &crate::config::DaemonConfig,
    daemon: &crate::daemon::Daemon,
    auth: &Authenticator,
    shutdown: &CancellationToken,
    tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<SpawnedTransports> {
    let mut unix_guards = Vec::new();
    let mut pending_ws: Vec<websocket::BoundWebSocketListener> = Vec::new();
    let mut ws_port: Option<u16> = None;
    let mut wt_info: Option<super::cairn_json::WtInfo> = None;

    for (index, listener_cfg) in cfg.listeners.iter().enumerate() {
        let id = ListenerId::new(index, listener_cfg);

        match listener_cfg {
            crate::listen::ListenerConfig::Unix(path) => {
                let (bound, guard) = unix::bind(id, path.clone(), cfg)?;
                unix_guards.push(guard);

                let daemon = daemon.clone();
                let auth = auth.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(bound.run(daemon, auth, shutdown));
            }
            crate::listen::ListenerConfig::WebTransport(addr) => {
                let bound = webtransport::bind(id, *addr, cfg).await?;
                // First WT listener wins (a dual-stack `localhost:PORT`
                // expansion shares one configured port across two addresses,
                // so that common case is unaffected — though NOT for port 0,
                // where each bind gets its own ephemeral port and only the
                // first is reported; genuinely distinct WT listeners are an
                // edge case the design spec doesn't cover).
                if wt_info.is_none() {
                    wt_info = Some(bound.cairn_json.clone());
                }

                let daemon = daemon.clone();
                let auth = auth.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(bound.run(daemon, auth, shutdown));
            }
            crate::listen::ListenerConfig::WebSocket(addr) => {
                let bound = websocket::bind(id, *addr, cfg).await?;
                if ws_port.is_none() {
                    ws_port = Some(bound.bound_addr.port());
                }
                pending_ws.push(bound);
            }
        }
    }

    let cairn_json = Arc::new(CairnJsonInfo {
        ws_port,
        wt: wt_info,
    });

    // Resolve SPA assets once (not per listener) if `--web-ui` is set at all.
    let assets: Option<Arc<Assets>> = match &cfg.web_ui {
        Some(_) => Some(Arc::new(Assets::resolve(cfg.web_dir.as_deref())?)),
        None => None,
    };
    let attach_to_ws = matches!(cfg.web_ui, Some(crate::config::WebUiMode::Attach));

    for bound in pending_ws {
        let daemon = daemon.clone();
        let auth = auth.clone();
        let shutdown = shutdown.clone();
        let spa = Arc::new(SpaState {
            assets: if attach_to_ws { assets.clone() } else { None },
            cairn_json: cairn_json.clone(),
            is_ws_listener: true,
        });
        tasks.spawn(bound.run(daemon, auth, shutdown, spa));
    }

    // The dedicated `--web-ui=host:port` listener isn't part of `--listen`;
    // bind and spawn it separately, once assets/`CairnJsonInfo` are ready.
    if let Some(crate::config::WebUiMode::Dedicated(addrs)) = &cfg.web_ui {
        for (i, addr) in addrs.iter().enumerate() {
            let id = ListenerId::web_ui(cfg.listeners.len() + i, *addr);
            let bound = web_ui::bind(id, *addr).await?;
            let shutdown = shutdown.clone();
            let spa = Arc::new(SpaState {
                assets: assets.clone(),
                cairn_json: cairn_json.clone(),
                is_ws_listener: false,
            });
            tasks.spawn(bound.run(shutdown, spa));
        }
    }

    Ok(SpawnedTransports {
        _unix_guards: unix_guards,
    })
}

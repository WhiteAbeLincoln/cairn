use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ListenerId;
use super::auth::Authenticator;

pub(super) mod unix;
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

pub(super) async fn bind_and_spawn(
    cfg: &crate::config::DaemonConfig,
    daemon: &crate::daemon::Daemon,
    auth: &Authenticator,
    shutdown: &CancellationToken,
    tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<SpawnedTransports> {
    let mut unix_guards = Vec::new();

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

                let daemon = daemon.clone();
                let auth = auth.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(bound.run(daemon, auth, shutdown));
            }
            crate::listen::ListenerConfig::WebSocket(addr) => {
                let bound = websocket::bind(id, *addr, cfg).await?;

                let daemon = daemon.clone();
                let auth = auth.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(bound.run(daemon, auth, shutdown));
            }
        }
    }

    Ok(SpawnedTransports {
        _unix_guards: unix_guards,
    })
}

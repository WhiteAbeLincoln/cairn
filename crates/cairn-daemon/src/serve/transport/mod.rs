use std::path::PathBuf;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ListenerId;
use super::auth::Authenticator;

pub(super) mod unix;
pub(super) mod webtransport;

pub(super) struct SpawnedTransports {
    pub(super) unix_paths: Vec<PathBuf>,
}

pub(super) async fn bind_and_spawn(
    cfg: &crate::config::DaemonConfig,
    daemon: &crate::daemon::Daemon,
    auth: &Authenticator,
    shutdown: &CancellationToken,
    tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<SpawnedTransports> {
    let mut unix_paths = Vec::new();

    for (index, listener_cfg) in cfg.listeners.iter().enumerate() {
        let id = ListenerId::new(index, listener_cfg);

        match listener_cfg {
            crate::listen::ListenerConfig::Unix(path) => {
                let bound = unix::bind(id, path.clone(), cfg)?;
                unix_paths.push(bound.path().to_path_buf());
                let daemon = daemon.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(async move { bound.run(daemon, shutdown).await });
            }
            crate::listen::ListenerConfig::WebTransport(addr) => {
                let bound = webtransport::bind(id, *addr, cfg).await?;
                let daemon = daemon.clone();
                let auth = auth.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(async move { bound.run(daemon, auth, shutdown).await });
            }
        }
    }

    Ok(SpawnedTransports { unix_paths })
}

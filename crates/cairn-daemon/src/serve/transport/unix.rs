use std::path::{Path, PathBuf};

use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::sync::CancellationToken;
use wrpc_transport::frame::Accept;

use crate::serve::ConnCtx;
use crate::serve::auth::Authenticator;
use crate::serve::transport::TransportListener;
use crate::serve::wrpc::run_wrpc_server;

use super::super::ListenerId;

/// A `UnixListener` whose `accept` captures `SO_PEERCRED` into `ConnCtx`
/// before splitting the stream.
struct PeerCredListener(pub tokio::net::UnixListener);

impl Accept for &PeerCredListener {
    type Context = ConnCtx;
    type Outgoing = OwnedWriteHalf;
    type Incoming = OwnedReadHalf;

    async fn accept(&self) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let (stream, _addr) = self.0.accept().await?;
        let identity = match stream.peer_cred().ok().map(|c| c.uid()) {
            Some(uid) => crate::identity::Identity::Unix {
                uid,
                // username is resolved lazily in whoami
                username: None,
            },
            None => crate::identity::Identity::Anonymous,
        };
        let (rx, tx) = stream.into_split();
        Ok((ConnCtx { identity }, tx, rx))
    }
}

impl TransportListener for PeerCredListener {
    fn run(
        self,
        daemon: crate::daemon::Daemon,
        _auth: Authenticator,
        shutdown: CancellationToken,
    ) -> impl futures::Future<Output = anyhow::Result<()>> + Send + 'static {
        run_wrpc_server::<_, OwnedReadHalf, OwnedWriteHalf, ()>(self, daemon, shutdown)
    }
}

pub(super) struct UnixListenerGuard {
    path: PathBuf,
}

impl Drop for UnixListenerGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path) {
            tracing::error!(
                socket = %self.path.display(),
                error = %error,
                "failed to remove daemon socket file during shutdown"
            );
        }
    }
}

pub(super) fn bind(
    id: ListenerId,
    path: PathBuf,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<(impl TransportListener, UnixListenerGuard)> {
    let listener = bind_with_cleanup(&path, cfg)?;
    tracing::info!(listener = %id, socket = %path.display(), "UDS listening");
    Ok((PeerCredListener(listener), UnixListenerGuard { path }))
}

// ── Socket lifecycle ──────────────────────────────────────────────────────

/// Create (or recover) the socket file with correct permissions.
///
/// - Creates the parent directory if needed, chmoding it to `dir_mode` only if
///   we created it (so we don't stomp an admin-managed dir).
/// - Probes a pre-existing socket: live -> bail; connection-refused -> unlink.
/// - Binds and chmods the socket to `socket_mode`.
fn bind_with_cleanup(
    path: &Path,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};

    if let Some(parent) = path.parent() {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        if created {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(cfg.dir_mode))?;
        }
    }

    if path.exists() {
        let file_type = std::fs::symlink_metadata(path)?.file_type();
        if !file_type.is_socket() {
            anyhow::bail!(
                "refusing to remove non-socket path while binding daemon socket: {}",
                path.display()
            );
        }

        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => anyhow::bail!("a daemon is already listening on {}", path.display()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(path)?;
            }
            Err(error) => {
                anyhow::bail!(
                    "failed to probe existing daemon socket {}: {error}",
                    path.display()
                );
            }
        }
    }

    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(cfg.socket_mode))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accept_yields_peer_uid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let pl = PeerCredListener(listener);

        let connect = tokio::spawn(async move {
            let _c = tokio::net::UnixStream::connect(&path).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let (ctx, _tx, _rx) = (&pl).accept().await.unwrap();
        match &ctx.identity {
            crate::identity::Identity::Unix { uid, username } => {
                assert_eq!(*uid, nix::unistd::geteuid().as_raw());
                // Username is resolved lazily in whoami, not at accept time.
                assert!(username.is_none());
            }
            other => panic!("expected Unix identity, got {other:?}"),
        }
        connect.await.unwrap();
    }
}

//! UDS listener + wRPC server wiring.

use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf, UCred};
use wrpc_transport::frame::Accept;

/// Per-connection context handed to every `Handler` method. On UDS the peer
/// credentials identify the caller (for `whoami` and audit). The future WT
/// transport will fill the same shape with the authenticated token identity.
#[derive(Clone, Copy, Debug)]
pub struct ConnCtx {
    pub peer: Option<UCred>,
}

/// A `UnixListener` whose `accept` captures `SO_PEERCRED` into `ConnCtx`
/// before splitting the stream.
pub struct PeerCredListener(pub tokio::net::UnixListener);

impl Accept for &PeerCredListener {
    type Context = ConnCtx;
    type Outgoing = OwnedWriteHalf;
    type Incoming = OwnedReadHalf;

    async fn accept(
        &self,
    ) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer = stream.peer_cred().ok();
        let (rx, tx) = stream.into_split();
        Ok((ConnCtx { peer }, tx, rx))
    }
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
        assert_eq!(ctx.peer.unwrap().uid(), nix_geteuid());
        connect.await.unwrap();
    }

    fn nix_geteuid() -> u32 {
        // SAFETY: geteuid is always safe.
        unsafe { libc::geteuid() }
    }
}

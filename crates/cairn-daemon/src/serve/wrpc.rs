use anyhow::Context as _;
use futures::stream::{StreamExt as _, select_all};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use wrpc_transport::frame::Accept;

use super::ConnCtx;

pub(in crate::serve) async fn run_wrpc_server<A, I, O, H>(
    acceptor: A,
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) -> anyhow::Result<()>
where
    for<'a> &'a A: Accept<Context = ConnCtx, Incoming = I, Outgoing = O>,
    A: Send + Sync + 'static,
    I: AsyncRead + Send + Sync + Unpin + 'static,
    O: AsyncWrite + Send + Sync + Unpin + 'static,
    H: wrpc_transport::frame::ConnHandler<I, O> + Send + Sync + 'static,
{
    let server: wrpc_transport::Server<ConnCtx, I, O, H> = wrpc_transport::Server::new();
    let invocations = cairn_protocol::serve(&server, daemon).await?;
    let mut invocations = select_all(
        invocations
            .into_iter()
            .map(|(instance, name, stream)| stream.map(move |res| (instance, name, res))),
    );

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,

            result = server.accept(&acceptor) => {
                result.context("wRPC accept failed")?;
            }

            item = invocations.next() => {
                match item {
                    Some((_instance, _name, Ok(fut))) => {
                        tokio::spawn(fut);
                    }
                    Some((instance, name, Err(error))) => {
                        return Err(error).with_context(|| {
                            format!("wRPC invocation failed for {instance}#{name}")
                        });
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

pub(in crate::serve) struct AuthenticatedWtAccept {
    pub(in crate::serve) inner: wrpc_transport_web::Client,
    pub(in crate::serve) ctx: ConnCtx,
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

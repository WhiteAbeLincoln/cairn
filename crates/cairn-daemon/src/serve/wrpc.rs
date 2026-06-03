use futures::stream::{StreamExt as _, select_all};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use wrpc_transport::frame::{Accept, AcceptError};

use super::ConnCtx;

pub(super) async fn run_wrpc_server<A, I, O, H>(
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

    // server.accept() is not cancel-safe — it has internal await points
    // where dropping loses accepted connections or deadlocks the handler
    // registry mutex. Pin it so it survives across select! iterations
    // rather than being dropped and recreated each time.
    let mut accepting = std::pin::pin!(server.accept(&acceptor));

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,

            result = &mut accepting => {
                match result {
                    Ok(()) => {}
                    // Handler channel closed — server internals are broken.
                    Err(AcceptError::Send(_)) => {
                        anyhow::bail!("wRPC handler channel closed");
                    }
                    Err(error) => {
                        tracing::debug!(%error, "wRPC connection rejected");
                    }
                }
                accepting.set(server.accept(&acceptor));
            }

            item = invocations.next() => {
                match item {
                    Some((_instance, _name, Ok(fut))) => {
                        tokio::spawn(fut);
                    }
                    Some((instance, name, Err(error))) => {
                        tracing::debug!(%error, %instance, %name, "wRPC invocation failed");
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

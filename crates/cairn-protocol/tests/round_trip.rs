//! End-to-end round-trip tests for the `cairn-protocol` bindings.
//!
//! Each test sets up a wRPC server on a tempdir Unix socket with stub
//! `Handler` implementations and asserts that a wRPC client can invoke
//! the relevant operations and receive the expected values back.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use futures::stream::select_all;
use futures::stream::StreamExt as _;
use tempfile::TempDir;

use cairn_protocol as bindings;

/// Resources held by an active harness. The temp dir is preserved
/// here so the socket path stays valid for the lifetime of the test;
/// dropping the harness shuts the server down.
struct Harness {
    socket_path: PathBuf,
    _tmp: TempDir,
    server_task: tokio::task::JoinHandle<()>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn unix_client(&self) -> wrpc_transport::unix::Client<std::path::PathBuf> {
        wrpc_transport::unix::Client::from(self.socket_path.clone())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server_task.abort();
        self.accept_task.abort();
    }
}

/// Spawn a wRPC unix-domain-socket server hosting the given `handler`.
///
/// `handler` must implement the `Handler` traits for every interface
/// in the `cairn:daemon@0.1.0` world that any test calls. The harness
/// returns once the server is bound and accepting connections.
async fn spawn_server<H>(handler: H) -> anyhow::Result<Harness>
where
    H: bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr>
        + bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr>
        + Clone
        + Send
        + Sync
        + 'static,
{
    let tmp = TempDir::new().context("failed to create temp dir")?;
    let socket_path = tmp.path().join("cairn-protocol-test.sock");

    let listener = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind on {}", socket_path.display()))?;

    let srv = Arc::new(wrpc_transport::Server::default());

    let accept_task = tokio::spawn({
        let srv = Arc::clone(&srv);
        async move {
            loop {
                if srv.accept(&listener).await.is_err() {
                    break;
                }
            }
        }
    });

    let invocations = bindings::serve(srv.as_ref(), handler)
        .await
        .context("bindings::serve failed")?;

    let server_task = tokio::spawn(async move {
        let mut invocations = select_all(
            invocations
                .into_iter()
                .map(|(instance, name, invocations)| {
                    invocations.map(move |res| (instance, name, res))
                }),
        );
        while let Some((_instance, _name, res)) = invocations.next().await {
            if let Ok(fut) = res {
                tokio::spawn(fut);
            }
        }
    });

    Ok(Harness {
        socket_path,
        _tmp: tmp,
        server_task,
        accept_task,
    })
}

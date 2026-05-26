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

#[tokio::test]
async fn meta_version_round_trips_record_fields() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn authenticate(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _token: String,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn whoami(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Result<String, bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn version(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<bindings::exports::cairn::daemon::meta::VersionInfo> {
            Ok(bindings::exports::cairn::daemon::meta::VersionInfo {
                daemon: "cairn-test-daemon/0.1.0".to_string(),
                protocol: "cairn:daemon@0.1.0".to_string(),
            })
        }
    }

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
        // All sessions methods must be implemented to satisfy the trait bound on
        // `spawn_server`, but only the operation under test (here: `version`,
        // which lives in `meta` — sessions methods are entirely unused) sees
        // real bodies. The rest panic via `unimplemented!()` so any accidental
        // invocation is loud rather than silent.
        async fn list_all(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Vec<bindings::cairn::daemon::types::SessionInfo>> {
            unimplemented!("not exercised by this test")
        }

        async fn inspect(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
        ) -> anyhow::Result<Result<bindings::cairn::daemon::types::SessionInfo, bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn create(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _spec: bindings::cairn::daemon::types::SessionSpec,
        ) -> anyhow::Result<Result<bindings::cairn::daemon::types::SessionInfo, bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn rename(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _new_name: String,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn restart(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _force: bool,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn kill(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _sig: bindings::cairn::daemon::types::Signal,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn kick(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _client: Option<String>,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn wait(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
        ) -> anyhow::Result<std::pin::Pin<Box<dyn std::future::Future<Output = bindings::cairn::daemon::types::ExitStatus> + Send + 'static>>> {
            unimplemented!("not exercised by this test")
        }

        async fn logs(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _window: bindings::cairn::daemon::types::LogWindow,
            _follow: bool,
        ) -> anyhow::Result<std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>> {
            unimplemented!("not exercised by this test")
        }

        async fn attach(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _init: bindings::cairn::daemon::types::AttachInit,
            _events: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bindings::cairn::daemon::types::ClientEvent>> + Send + 'static>>,
        ) -> anyhow::Result<std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bindings::cairn::daemon::types::ServerEvent>> + Send + 'static>>> {
            unimplemented!("not exercised by this test")
        }

        async fn send(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _chunks: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }
    }

    let harness = spawn_server(Stub).await.expect("spawn_server");

    let info = bindings::client::cairn::daemon::meta::version(&harness.unix_client(), ())
        .await
        .expect("version invocation");

    assert_eq!(info.daemon, "cairn-test-daemon/0.1.0");
    assert_eq!(info.protocol, "cairn:daemon@0.1.0");
}

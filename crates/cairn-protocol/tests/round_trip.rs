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
            _grace_ms: Option<u32>,
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

fn sample_session_info(id: &str) -> bindings::cairn::daemon::types::SessionInfo {
    use bindings::cairn::daemon::types::{SessionInfo, SessionSpec};

    SessionInfo {
        id: id.to_string(),
        name: Some("test".to_string()),
        pid: Some(42),
        cols: 80,
        rows: 24,
        attached_clients: vec!["client-a".to_string(), "client-b".to_string()],
        created_at_unix_ms: 1_000_000_000_000,
        exit: None,
        spec: SessionSpec {
            name: Some("test".to_string()),
            command: vec!["/bin/echo".to_string(), "hi".to_string()],
            env: vec![("FOO".to_string(), "bar".to_string())],
            env_inherit: true,
            workdir: Some("/tmp".to_string()),
            tty: true,
            stdin: true,
            idle_timeout_secs: None,
            scrollback_lines: 1000,
        },
    }
}

#[tokio::test]
async fn sessions_list_all_round_trips_two_entries_with_optional_fields() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn list_all(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Vec<bindings::cairn::daemon::types::SessionInfo>> {
            Ok(vec![
                sample_session_info("01900000-0000-7000-8000-000000000001"),
                sample_session_info("01900000-0000-7000-8000-000000000002"),
            ])
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
            _grace_ms: Option<u32>,
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
            unimplemented!("not exercised by this test")
        }
    }

    let harness = spawn_server(Stub).await.expect("spawn_server");

    let result = bindings::client::cairn::daemon::sessions::list_all(&harness.unix_client(), ())
        .await
        .expect("list_all invocation");

    assert_eq!(result.len(), 2);

    // PartialEq is not generated for SessionInfo / SessionSpec by wit-bindgen-wrpc,
    // so we assert per-field to cover the full type graph: nested record, options,
    // list<string>, list<tuple<string,string>>, and scalar types.

    // --- first entry ---
    assert_eq!(result[0].id, "01900000-0000-7000-8000-000000000001");
    assert_eq!(result[0].name, Some("test".to_string()));
    assert_eq!(result[0].pid, Some(42));
    assert_eq!(result[0].cols, 80);
    assert_eq!(result[0].rows, 24);
    assert_eq!(result[0].attached_clients, vec!["client-a".to_string(), "client-b".to_string()]);
    assert_eq!(result[0].created_at_unix_ms, 1_000_000_000_000);
    assert!(result[0].exit.is_none());
    // nested SessionSpec
    assert_eq!(result[0].spec.name, Some("test".to_string()));
    assert_eq!(result[0].spec.command, vec!["/bin/echo".to_string(), "hi".to_string()]);
    assert_eq!(result[0].spec.env, vec![("FOO".to_string(), "bar".to_string())]);
    assert!(result[0].spec.env_inherit);
    assert_eq!(result[0].spec.workdir, Some("/tmp".to_string()));
    assert!(result[0].spec.tty);
    assert!(result[0].spec.stdin);
    assert!(result[0].spec.idle_timeout_secs.is_none());
    assert_eq!(result[0].spec.scrollback_lines, 1000);

    // --- second entry (same shape, different id) ---
    assert_eq!(result[1].id, "01900000-0000-7000-8000-000000000002");
    assert_eq!(result[1].name, Some("test".to_string()));
    assert_eq!(result[1].pid, Some(42));
    assert_eq!(result[1].cols, 80);
    assert_eq!(result[1].rows, 24);
    assert_eq!(result[1].attached_clients, vec!["client-a".to_string(), "client-b".to_string()]);
    assert_eq!(result[1].created_at_unix_ms, 1_000_000_000_000);
    assert!(result[1].exit.is_none());
    // nested SessionSpec
    assert_eq!(result[1].spec.name, Some("test".to_string()));
    assert_eq!(result[1].spec.command, vec!["/bin/echo".to_string(), "hi".to_string()]);
    assert_eq!(result[1].spec.env, vec![("FOO".to_string(), "bar".to_string())]);
    assert!(result[1].spec.env_inherit);
    assert_eq!(result[1].spec.workdir, Some("/tmp".to_string()));
    assert!(result[1].spec.tty);
    assert!(result[1].spec.stdin);
    assert!(result[1].spec.idle_timeout_secs.is_none());
    assert_eq!(result[1].spec.scrollback_lines, 1000);
}

#[tokio::test]
async fn meta_authenticate_round_trips_error_variant() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn authenticate(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            token: String,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            if token == "valid-token" {
                Ok(Ok(()))
            } else {
                Ok(Err(bindings::cairn::daemon::types::Error {
                    code: "auth.invalid_token".to_string(),
                    message: "supplied token did not match".to_string(),
                }))
            }
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
            unimplemented!("not exercised by this test")
        }
    }

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
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
            _grace_ms: Option<u32>,
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

    // Success path.
    let ok = bindings::client::cairn::daemon::meta::authenticate(
        &harness.unix_client(),
        (),
        "valid-token",
    )
    .await
    .expect("authenticate invocation (ok)");
    assert!(ok.is_ok(), "expected Ok(_), got {ok:?}");

    // Failure path.
    let err = bindings::client::cairn::daemon::meta::authenticate(
        &harness.unix_client(),
        (),
        "wrong-token",
    )
    .await
    .expect("authenticate invocation (err)");
    let err = err.expect_err("expected error variant");
    assert_eq!(err.code, "auth.invalid_token");
    assert_eq!(err.message, "supplied token did not match");
}

#[tokio::test]
async fn sessions_kill_round_trips_grace_ms() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn kill(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            id: String,
            sig: bindings::cairn::daemon::types::Signal,
            grace_ms: Option<u32>,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            let named = matches!(sig, bindings::cairn::daemon::types::Signal::Named(_));
            Ok(Err(bindings::cairn::daemon::types::Error {
                code: "echo".to_string(),
                message: format!("id={id} named={named} grace={grace_ms:?}"),
            }))
        }

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
            unimplemented!("not exercised by this test")
        }
    }

    let harness = spawn_server(Stub).await.expect("spawn_server");
    let sig = bindings::cairn::daemon::types::Signal::Named(
        bindings::cairn::daemon::types::SignalName::Term,
    );
    let res = bindings::client::cairn::daemon::sessions::kill(
        &harness.unix_client(),
        (),
        "dev",
        &sig,
        Some(5000u32),
    )
    .await
    .expect("kill invocation");
    let err = res.expect_err("stub echoes via Err");
    assert_eq!(err.message, "id=dev named=true grace=Some(5000)");
}

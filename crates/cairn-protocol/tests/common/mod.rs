//! Shared test support for the `cairn-protocol` round-trip tests.
//!
//! [`StubHandler`] implements both daemon `Handler` traits once, here. A test
//! wires up only the operation(s) it exercises via the `on_*` builder methods;
//! every other operation falls back to `unimplemented!`, so individual tests
//! never have to spell out the methods they don't use.
//!
//! When a new test needs to stub an operation not yet covered, add an `on_*`
//! builder + field for it (the streaming ops — wait/logs/attach/send — are
//! intentionally left unstubbed until a test needs them).

#![allow(dead_code)] // builder methods are used à la carte by individual tests

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use futures::stream::{StreamExt as _, select_all};
use tempfile::TempDir;

use cairn_protocol as bindings;

use bindings::cairn::daemon::types::{Error, SessionInfo, SessionSpec, Signal};
use bindings::exports::cairn::daemon::meta::VersionInfo;

/// Per-connection context the wRPC unix-socket server hands to handlers.
type Ctx = tokio::net::unix::SocketAddr;

// ── Server harness ───────────────────────────────────────────────────────

/// Resources held by an active harness. The temp dir is preserved here so the
/// socket path stays valid for the lifetime of the test; dropping the harness
/// shuts the server down.
pub struct Harness {
    socket_path: PathBuf,
    _tmp: TempDir,
    server_task: tokio::task::JoinHandle<()>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl Harness {
    pub fn unix_client(&self) -> wrpc_transport::unix::Client<PathBuf> {
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
/// Returns once the server is bound and accepting connections.
pub async fn spawn_server<H>(handler: H) -> anyhow::Result<Harness>
where
    H: bindings::exports::cairn::daemon::sessions::Handler<Ctx>
        + bindings::exports::cairn::daemon::meta::Handler<Ctx>
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
        let mut invocations = select_all(invocations.into_iter().map(
            |(instance, name, invocations)| invocations.map(move |res| (instance, name, res)),
        ));
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

// ── Fixtures ─────────────────────────────────────────────────────────────

/// A fully-populated `SessionInfo` for exercising the value-type graph
/// (nested record, options, `list<string>`, `list<tuple<string,string>>`).
pub fn sample_session_info(id: &str) -> SessionInfo {
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

// ── Configurable stub handler ────────────────────────────────────────────

type VersionFn = Arc<dyn Fn(Ctx) -> VersionInfo + Send + Sync>;
type ListAllFn = Arc<dyn Fn(Ctx) -> Vec<SessionInfo> + Send + Sync>;
type AuthenticateFn = Arc<dyn Fn(Ctx, String) -> Result<(), Error> + Send + Sync>;
type KillFn = Arc<dyn Fn(Ctx, String, Signal, Option<u32>) -> Result<(), Error> + Send + Sync>;

/// A `Handler` whose operations are individually configurable. Unset
/// operations panic via `unimplemented!`, so a test only wires up the
/// method(s) it actually calls.
#[derive(Clone, Default)]
pub struct StubHandler {
    version: Option<VersionFn>,
    list_all: Option<ListAllFn>,
    authenticate: Option<AuthenticateFn>,
    kill: Option<KillFn>,
}

impl StubHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_version(mut self, f: impl Fn(Ctx) -> VersionInfo + Send + Sync + 'static) -> Self {
        self.version = Some(Arc::new(f));
        self
    }

    pub fn on_list_all(
        mut self,
        f: impl Fn(Ctx) -> Vec<SessionInfo> + Send + Sync + 'static,
    ) -> Self {
        self.list_all = Some(Arc::new(f));
        self
    }

    pub fn on_authenticate(
        mut self,
        f: impl Fn(Ctx, String) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Self {
        self.authenticate = Some(Arc::new(f));
        self
    }

    pub fn on_kill(
        mut self,
        f: impl Fn(Ctx, String, Signal, Option<u32>) -> Result<(), Error> + Send + Sync + 'static,
    ) -> Self {
        self.kill = Some(Arc::new(f));
        self
    }
}

impl bindings::exports::cairn::daemon::sessions::Handler<Ctx> for StubHandler {
    async fn list_all(&self, ctx: Ctx) -> anyhow::Result<Vec<SessionInfo>> {
        match &self.list_all {
            Some(f) => Ok(f(ctx)),
            None => unimplemented!("sessions.list-all not stubbed in this test"),
        }
    }

    async fn kill(
        &self,
        ctx: Ctx,
        id: String,
        sig: Signal,
        grace_ms: Option<u32>,
    ) -> anyhow::Result<Result<(), Error>> {
        match &self.kill {
            Some(f) => Ok(f(ctx, id, sig, grace_ms)),
            None => unimplemented!("sessions.kill not stubbed in this test"),
        }
    }

    async fn inspect(&self, _ctx: Ctx, _id: String) -> anyhow::Result<Result<SessionInfo, Error>> {
        unimplemented!("sessions.inspect not stubbed in this test")
    }

    async fn create(
        &self,
        _ctx: Ctx,
        _spec: SessionSpec,
    ) -> anyhow::Result<Result<SessionInfo, Error>> {
        unimplemented!("sessions.create not stubbed in this test")
    }

    async fn rename(
        &self,
        _ctx: Ctx,
        _id: String,
        _new_name: String,
    ) -> anyhow::Result<Result<(), Error>> {
        unimplemented!("sessions.rename not stubbed in this test")
    }

    async fn restart(
        &self,
        _ctx: Ctx,
        _id: String,
        _force: bool,
    ) -> anyhow::Result<Result<(), Error>> {
        unimplemented!("sessions.restart not stubbed in this test")
    }

    async fn kick(
        &self,
        _ctx: Ctx,
        _id: String,
        _client: Option<String>,
    ) -> anyhow::Result<Result<(), Error>> {
        unimplemented!("sessions.kick not stubbed in this test")
    }

    async fn wait(
        &self,
        _ctx: Ctx,
        _id: String,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<
                dyn std::future::Future<Output = bindings::cairn::daemon::types::ExitStatus>
                    + Send
                    + 'static,
            >,
        >,
    > {
        unimplemented!("sessions.wait not stubbed in this test")
    }

    async fn logs(
        &self,
        _ctx: Ctx,
        _id: String,
        _window: bindings::cairn::daemon::types::LogWindow,
        _follow: bool,
    ) -> anyhow::Result<
        std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>,
    > {
        unimplemented!("sessions.logs not stubbed in this test")
    }

    async fn attach(
        &self,
        _ctx: Ctx,
        _id: String,
        _init: bindings::cairn::daemon::types::AttachInit,
        _events: std::pin::Pin<
            Box<
                dyn futures::Stream<Item = Vec<bindings::cairn::daemon::types::ClientEvent>>
                    + Send
                    + 'static,
            >,
        >,
    ) -> anyhow::Result<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = Vec<bindings::cairn::daemon::types::ServerEvent>>
                    + Send
                    + 'static,
            >,
        >,
    > {
        unimplemented!("sessions.attach not stubbed in this test")
    }

    async fn send(
        &self,
        _ctx: Ctx,
        _id: String,
        _chunks: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>,
    ) -> anyhow::Result<Result<(), Error>> {
        unimplemented!("sessions.send not stubbed in this test")
    }
}

impl bindings::exports::cairn::daemon::meta::Handler<Ctx> for StubHandler {
    async fn version(&self, ctx: Ctx) -> anyhow::Result<VersionInfo> {
        match &self.version {
            Some(f) => Ok(f(ctx)),
            None => unimplemented!("meta.version not stubbed in this test"),
        }
    }

    async fn authenticate(&self, ctx: Ctx, token: String) -> anyhow::Result<Result<(), Error>> {
        match &self.authenticate {
            Some(f) => Ok(f(ctx, token)),
            None => unimplemented!("meta.authenticate not stubbed in this test"),
        }
    }

    async fn whoami(&self, _ctx: Ctx) -> anyhow::Result<Result<String, Error>> {
        unimplemented!("meta.whoami not stubbed in this test")
    }
}

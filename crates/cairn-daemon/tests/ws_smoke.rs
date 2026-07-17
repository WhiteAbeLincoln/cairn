//! Smoke tests: daemon with a `ws://` listener, clients connect over WebSocket.
//!
//! Validates the browser-facing transport end to end: an axum HTTP server binds
//! a TCP port, `/ws` upgrades to a WebSocket carrying exactly one wRPC
//! invocation, and a Rust `wrpc-websockets` client (see `common::WsClient`)
//! round-trips `meta.version` and a full `attach`. Also covers `/healthz`,
//! origin validation, and graceful-shutdown draining.

mod common;

use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use cairn_protocol::cairn::daemon::types::{AttachInit, ClientEvent, ServerEvent, SessionSpec};
use cairn_protocol::client::cairn::daemon as api;
use common::DaemonHarness;
use futures::StreamExt as _;

fn session_spec(name: &str, cmd: &[&str]) -> SessionSpec {
    SessionSpec {
        name: Some(name.to_string()),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        env: vec![],
        env_inherit: true,
        workdir: None,
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 100,
    }
}

async fn next_batch(
    stream: &mut (impl futures::Stream<Item = Vec<ServerEvent>> + Unpin),
) -> Vec<ServerEvent> {
    tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

// ── version ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_version_round_trip() {
    let harness = DaemonHarness::start_with_ws().await;
    let client = harness.ws_client();

    let info = api::meta::version(&client, (), None)
        .await
        .expect("version via WS");
    assert!(
        info.daemon.starts_with("cairn-daemon/"),
        "unexpected daemon version: {}",
        info.daemon
    );
    assert_eq!(info.protocol, "cairn:daemon@0.1.0");
}

// ── attach ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_attach_round_trip() {
    let harness = DaemonHarness::start_with_ws().await;
    let client = harness.ws_client();

    // Create a `cat` session over WS: input is echoed straight back.
    let info = api::sessions::create(&client, (), None, &session_spec("wsattach", &["cat"]))
        .await
        .expect("create invoke")
        .expect("create returned a session");

    // Client-event channel feeding the attach's upstream.
    let (tx, rx) = futures::channel::mpsc::unbounded::<Vec<ClientEvent>>();
    let events: Pin<Box<dyn futures::Stream<Item = Vec<ClientEvent>> + Send>> = Box::pin(rx);
    let init = AttachInit {
        cols: 80,
        rows: 24,
        no_stdin: false,
    };

    let (mut server, io) = api::sessions::attach(&client, (), None, &info.id, &init, events)
        .await
        .expect("attach invoke");

    // The transport io future pumps both directions; drive it in the background.
    let io_task = tokio::spawn(async move {
        if let Some(f) = io {
            let _ = f.await;
        }
    });

    // The attach stream opens with a snapshot. (Poll a few batches rather than
    // asserting on exactly the first one, to stay robust under parallel load.)
    let mut saw_snapshot = false;
    for _ in 0..20 {
        if next_batch(&mut server)
            .await
            .iter()
            .any(|e| matches!(e, ServerEvent::Snapshot(_)))
        {
            saw_snapshot = true;
            break;
        }
    }
    assert!(saw_snapshot, "attach must emit a Snapshot event");

    // Send input; `cat` echoes it back as Output over the same WS stream.
    tx.unbounded_send(vec![ClientEvent::Input(bytes::Bytes::from_static(
        b"hello-ws\n",
    ))])
    .expect("queue input event");
    let mut saw_echo = false;
    for _ in 0..20 {
        for ev in next_batch(&mut server).await {
            if let ServerEvent::Output(b) = ev
                && b.windows(8).any(|w| w == b"hello-ws")
            {
                saw_echo = true;
            }
        }
        if saw_echo {
            break;
        }
    }
    assert!(
        saw_echo,
        "input should be echoed back over the WS attach stream"
    );

    // Detach ends the stream.
    tx.unbounded_send(vec![ClientEvent::Detach])
        .expect("queue detach event");
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while server.next().await.is_some() {}
    })
    .await;
    assert!(ended.is_ok(), "attach stream should end after Detach");

    io_task.abort();
}

// ── coexistence ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn uds_and_ws_coexist() {
    let harness = DaemonHarness::start_with_ws().await;

    let uds = harness.client();
    let uds_info = api::meta::version(&uds, (), None)
        .await
        .expect("version via UDS");

    let ws = harness.ws_client();
    let ws_info = api::meta::version(&ws, (), None)
        .await
        .expect("version via WS");

    // Same daemon behind both transports.
    assert_eq!(uds_info.daemon, ws_info.daemon);
    assert_eq!(uds_info.protocol, ws_info.protocol);
}

// ── /healthz ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ws_healthz_returns_ok() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET /healthz HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();
    let response = String::from_utf8_lossy(&buf);

    assert!(
        response.starts_with("HTTP/1.1 200"),
        "healthz should return 200, got: {response}"
    );
    assert!(response.contains("ok"), "healthz body should be 'ok'");
}

// ── identity (tailscale-serve headers) ─────────────────────────────────────────

/// Like `common::WsClient`, but attaches extra headers to the WS upgrade
/// request — used to exercise `TransportContext::Http`'s header-based auth
/// backends (`tailscale-serve`) end to end.
struct WsClientWithHeaders {
    uri: String,
    headers: Vec<(http::HeaderName, http::HeaderValue)>,
}

impl wrpc_transport::Invoke for WsClientWithHeaders {
    type Context = ();
    type Outgoing = wrpc_transport::frame::Outgoing;
    type Incoming = wrpc_transport::frame::Incoming;

    async fn invoke<P>(
        &self,
        (): Self::Context,
        instance: &str,
        func: &str,
        params: bytes::Bytes,
        paths: impl AsRef<[P]> + Send,
    ) -> anyhow::Result<(Self::Outgoing, Self::Incoming)>
    where
        P: AsRef<[Option<usize>]> + Send + Sync,
    {
        let mut builder = wrpc_websockets::tokio_websockets::ClientBuilder::new().uri(&self.uri)?;
        for (name, value) in &self.headers {
            builder = builder.add_header(name.clone(), value.clone())?;
        }
        let (ws, _resp) = builder.connect().await?;
        let (tx, rx) = cairn_daemon::ws::split(ws);
        wrpc_transport::frame::invoke(tx, rx, instance, func, params, paths).await
    }
}

/// End-to-end: a WS upgrade carrying `Tailscale-User-*` headers from loopback,
/// against a daemon configured with `--auth tailscale-serve`, must resolve to
/// that Tailscale identity — proving `TransportContext::Http` reaches the real
/// `AuthChain` and the result lands on `ConnCtx` (the same struct WT identities
/// flow through) rather than just being computed and discarded.
#[tokio::test]
async fn ws_tailscale_serve_header_identifies_caller() {
    let harness = DaemonHarness::start_with_ws_tailscale_serve().await;
    let addr = harness.ws_addr.unwrap();

    let client = WsClientWithHeaders {
        uri: format!("ws://{addr}/ws"),
        headers: vec![
            (
                http::HeaderName::from_static("tailscale-user-login"),
                http::HeaderValue::from_static("alice@example.com"),
            ),
            (
                http::HeaderName::from_static("tailscale-user-name"),
                http::HeaderValue::from_static("Alice Architect"),
            ),
        ],
    };

    let who = api::meta::whoami(&client, (), None)
        .await
        .expect("whoami invoke")
        .expect("whoami result");
    assert_eq!(who, "Alice Architect");
}

// ── origin validation ──────────────────────────────────────────────────────────

/// Attempt the `/ws` upgrade with an optional `Origin` header. Returns `Ok` if
/// the handshake reached `101 Switching Protocols`, `Err` otherwise (e.g. a 403
/// origin rejection before upgrade).
async fn ws_connect(addr: SocketAddr, origin: Option<&str>) -> Result<(), String> {
    let mut builder = wrpc_websockets::tokio_websockets::ClientBuilder::new()
        .uri(&format!("ws://{addr}/ws"))
        .map_err(|e| e.to_string())?;
    if let Some(origin) = origin {
        builder = builder
            .add_header(
                http::header::ORIGIN,
                http::HeaderValue::from_str(origin).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
    }
    builder
        .connect()
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn ws_origin_same_host_allowed() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();
    // tokio-websockets sets Host to the URI authority; a matching Origin passes.
    let origin = format!("http://{addr}");
    assert!(
        ws_connect(addr, Some(&origin)).await.is_ok(),
        "same-host origin must be allowed"
    );
}

#[tokio::test]
async fn ws_origin_absent_allowed() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();
    assert!(
        ws_connect(addr, None).await.is_ok(),
        "absent origin must be allowed (non-browser client)"
    );
}

#[tokio::test]
async fn ws_origin_allowlisted_allowed() {
    let harness =
        DaemonHarness::start_with_ws_origins(vec!["http://app.example".to_string()]).await;
    let addr = harness.ws_addr.unwrap();
    assert!(
        ws_connect(addr, Some("http://app.example")).await.is_ok(),
        "allowlisted origin must be allowed"
    );
}

#[tokio::test]
async fn ws_origin_mismatch_rejected() {
    let harness = DaemonHarness::start_with_ws().await;
    let addr = harness.ws_addr.unwrap();
    assert!(
        ws_connect(addr, Some("http://evil.example")).await.is_err(),
        "mismatched origin must be rejected before upgrade"
    );
}

// ── graceful shutdown ──────────────────────────────────────────────────────────

/// With an open WebSocket connection, cancelling the daemon must let `serve()`
/// return cleanly (draining the connection task) rather than hang.
#[tokio::test]
async fn ws_graceful_shutdown_drains_open_connection() {
    use cairn_daemon::config::DaemonConfig;
    use cairn_daemon::daemon::Daemon;
    use cairn_daemon::listen::ListenerConfig;
    use cairn_daemon::serve::serve;
    use tokio_util::sync::CancellationToken;

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let ws_addr = SocketAddr::from(([127, 0, 0, 1], port));

    let cfg = DaemonConfig {
        listeners: vec![ListenerConfig::WebSocket(ws_addr)],
        shutdown_grace: Duration::from_millis(300),
        ..DaemonConfig::default()
    };
    let daemon = Daemon::new(cfg).expect("daemon config valid");
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(serve(daemon, shutdown.clone(), None));

    // Wait for the listener to accept.
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(ws_addr).await.is_ok() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Hold an upgraded WebSocket open across shutdown.
    let (ws, _resp) = wrpc_websockets::tokio_websockets::ClientBuilder::new()
        .uri(&format!("ws://{ws_addr}/ws"))
        .unwrap()
        .connect()
        .await
        .expect("ws connect");

    shutdown.cancel();

    let joined = tokio::time::timeout(Duration::from_secs(5), task).await;
    let serve_result = joined
        .expect("serve() must return promptly after shutdown, not hang")
        .expect("serve task should not panic");
    assert!(
        serve_result.is_ok(),
        "serve() should shut down cleanly: {serve_result:?}"
    );

    drop(ws);
}

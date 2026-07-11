//! Integration tests for `--web-ui`/`--web-dir` SPA serving and `/cairn.json`.
//!
//! No real SPA exists yet (that's a later task), so these tests build small
//! fixture directories on the fly and pass them via `--web-dir`, which works
//! without the `web-ui` compile-time embed feature — exactly the
//! configuration this test suite always runs under.

mod common;

use std::net::SocketAddr;
use std::path::Path;

use cairn_daemon::config::{DaemonConfig, WebUiMode, WtTlsIdentity};
use cairn_daemon::daemon::Daemon;
use cairn_daemon::listen::ListenerConfig;
use cairn_daemon::serve::serve;
use cairn_daemon::tls::TlsConfig;
use tokio_util::sync::CancellationToken;

/// Write a minimal SPA fixture (`index.html` + one static asset) to `dir`.
fn write_fixture_spa(dir: &Path) {
    std::fs::write(
        dir.join("index.html"),
        b"<html><body>cairn spa</body></html>",
    )
    .unwrap();
    std::fs::write(dir.join("app.js"), b"console.log('cairn')").unwrap();
}

/// A running daemon (background task) that shuts down and aborts on drop.
struct TestDaemon {
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl TestDaemon {
    /// Spawn `serve()` for `cfg`. Assumes `cfg` is valid (`Daemon::new`
    /// succeeds) — tests for the invalid-config path call `Daemon::new`
    /// directly instead of going through this helper.
    async fn spawn(cfg: DaemonConfig) -> Self {
        let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone(), None));
        Self { shutdown, task }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.task.abort();
    }
}

/// A parsed raw HTTP/1.1 response.
struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Issue a plain `GET {path}` over a fresh TCP connection to `addr` and parse
/// the response. No HTTP client crate needed for these tests' small surface.
async fn http_get(addr: SocketAddr, path: &str) -> HttpResponse {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();

    let sep = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response should have a header/body separator");
    let header_block = std::str::from_utf8(&buf[..sep]).unwrap();
    let body = buf[sep + 4..].to_vec();

    let mut lines = header_block.split("\r\n");
    let status_line = lines.next().unwrap();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (k, v) = l.split_once(':').unwrap();
            (k.trim().to_string(), v.trim().to_string())
        })
        .collect();

    HttpResponse {
        status,
        headers,
        body,
    }
}

/// Generate a self-signed WT cert in a per-test tempdir and return the
/// `WtTlsIdentity` pointing at it, mirroring `common::DaemonHarness`'s
/// pattern (never rely on the shared global `runtime_dir()` in tests, to
/// avoid cross-test races). Because this passes explicit `wt_cert`/`wt_key`,
/// the daemon treats it as a *user-supplied* cert, so `/cairn.json` omits
/// `certHash` for it (see `cairn_json::render`'s hash-omission rule) — the
/// reverse case (self-signed -> hash present) is covered by pure unit tests
/// in `serve::cairn_json`, which don't need a real bind and so can't race on
/// the shared directory.
fn wt_tls_identity(tls_dir: &Path) -> (WtTlsIdentity, String) {
    let tls = TlsConfig::self_signed(tls_dir).unwrap();
    let cert_hash = tls.spki_hash_hex();
    (
        WtTlsIdentity {
            cert: tls_dir.join("cert.pem"),
            key: tls_dir.join("key.pem"),
        },
        cert_hash,
    )
}

// ── flag validation ─────────────────────────────────────────────────────────

#[test]
fn bare_web_ui_without_ws_listener_is_rejected_at_construction() {
    let cfg = DaemonConfig {
        listeners: vec![ListenerConfig::WebTransport("127.0.0.1:0".parse().unwrap())],
        web_ui: Some(WebUiMode::Attach),
        ..DaemonConfig::default()
    };
    let err = Daemon::new(cfg)
        .err()
        .expect("bare --web-ui needs a ws:// listener");
    assert!(err.to_string().contains("ws://"));
}

// ── serving form 1: bare --web-ui attaches to the ws:// listener ───────────

#[tokio::test]
async fn bare_web_ui_serves_spa_on_ws_listener() {
    let fixture = tempfile::tempdir().unwrap();
    write_fixture_spa(fixture.path());

    let ws_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let cfg = DaemonConfig {
        listeners: vec![ListenerConfig::WebSocket(ws_addr)],
        web_ui: Some(WebUiMode::Attach),
        web_dir: Some(fixture.path().to_path_buf()),
        ..DaemonConfig::default()
    };
    let _daemon = TestDaemon::spawn(cfg).await;
    common::wait_for_tcp(ws_addr).await;

    let index = http_get(ws_addr, "/").await;
    assert_eq!(index.status, 200);
    assert_eq!(index.body, b"<html><body>cairn spa</body></html>");

    let asset = http_get(ws_addr, "/app.js").await;
    assert_eq!(asset.status, 200);
    assert_eq!(asset.body, b"console.log('cairn')");
    assert_eq!(asset.header("content-type"), Some("text/javascript"));

    // `/ws` stays reserved for the wRPC upgrade even with SPA routes
    // attached — a plain GET (no upgrade headers) must not fall back to the
    // SPA; it should hit the ws_upgrade handler and get rejected.
    let ws_route = http_get(ws_addr, "/ws").await;
    assert_ne!(
        ws_route.body, index.body,
        "/ws must not be shadowed by the SPA fallback"
    );

    // Unknown path falls back to index.html (client-side routing).
    let unknown = http_get(ws_addr, "/sessions/abc123").await;
    assert_eq!(unknown.status, 200);
    assert_eq!(unknown.body, index.body);
}

// ── serving form 2: --web-ui=host:port is a dedicated, SPA-only listener ──

#[tokio::test]
async fn dedicated_web_ui_serves_spa_without_ws_listener() {
    let fixture = tempfile::tempdir().unwrap();
    write_fixture_spa(fixture.path());
    let tls_dir = tempfile::tempdir().unwrap();
    let (wt_tls, _cert_hash) = wt_tls_identity(tls_dir.path());

    let wt_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let ui_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let cfg = DaemonConfig {
        // Valid with only a WebTransport listener configured (no ws://).
        listeners: vec![ListenerConfig::WebTransport(wt_addr)],
        wt_tls: Some(wt_tls),
        web_ui: Some(WebUiMode::Dedicated(vec![ui_addr])),
        web_dir: Some(fixture.path().to_path_buf()),
        ..DaemonConfig::default()
    };
    let _daemon = TestDaemon::spawn(cfg).await;
    common::wait_for_tcp(ui_addr).await;

    let index = http_get(ui_addr, "/").await;
    assert_eq!(index.status, 200);
    assert_eq!(index.body, b"<html><body>cairn spa</body></html>");

    let asset = http_get(ui_addr, "/app.js").await;
    assert_eq!(asset.status, 200);
    assert_eq!(asset.body, b"console.log('cairn')");

    // Unknown path (including "/ws", which doesn't exist on this listener at
    // all) falls back to index.html.
    let ws_route = http_get(ui_addr, "/ws").await;
    assert_eq!(ws_route.status, 200);
    assert_eq!(ws_route.body, index.body);
}

// ── /cairn.json contents per listener combination ──────────────────────────

#[tokio::test]
async fn cairn_json_ws_only() {
    let ws_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let cfg = DaemonConfig {
        listeners: vec![ListenerConfig::WebSocket(ws_addr)],
        ..DaemonConfig::default()
    };
    let _daemon = TestDaemon::spawn(cfg).await;
    common::wait_for_tcp(ws_addr).await;

    let resp = http_get(ws_addr, "/cairn.json").await;
    assert_eq!(resp.status, 200);
    assert_eq!(
        resp.header("access-control-allow-origin"),
        Some("*"),
        "/cairn.json must be servable cross-origin"
    );
    let doc: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    assert_eq!(doc, serde_json::json!({"endpoints": {"websocket": "/ws"}}));
}

#[tokio::test]
async fn cairn_json_ws_and_wt() {
    let tls_dir = tempfile::tempdir().unwrap();
    let (wt_tls, _cert_hash) = wt_tls_identity(tls_dir.path());

    let ws_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let wt_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let cfg = DaemonConfig {
        listeners: vec![
            ListenerConfig::WebSocket(ws_addr),
            ListenerConfig::WebTransport(wt_addr),
        ],
        wt_tls: Some(wt_tls),
        ..DaemonConfig::default()
    };
    let _daemon = TestDaemon::spawn(cfg).await;
    common::wait_for_tcp(ws_addr).await;

    let resp = http_get(ws_addr, "/cairn.json").await;
    assert_eq!(resp.status, 200);
    let doc: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    let endpoints = doc.get("endpoints").unwrap();
    assert_eq!(endpoints.get("websocket").unwrap(), "/ws");
    let wt = endpoints.get("webtransport").unwrap();
    assert_eq!(
        wt.get("url").unwrap(),
        &serde_json::json!(format!("https://127.0.0.1:{}", wt_addr.port()))
    );
    // Explicit `--wt-cert`/`--wt-key` (this test's `wt_tls_identity` helper)
    // is the user-supplied-cert path: no pinned hash is published.
    assert!(
        wt.get("certHash").is_none(),
        "user-supplied cert must omit certHash, got: {wt}"
    );
}

#[tokio::test]
async fn cairn_json_wt_and_dedicated_ui() {
    let tls_dir = tempfile::tempdir().unwrap();
    let (wt_tls, _cert_hash) = wt_tls_identity(tls_dir.path());
    let fixture = tempfile::tempdir().unwrap();
    write_fixture_spa(fixture.path());

    let wt_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let ui_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let cfg = DaemonConfig {
        listeners: vec![ListenerConfig::WebTransport(wt_addr)],
        wt_tls: Some(wt_tls),
        web_ui: Some(WebUiMode::Dedicated(vec![ui_addr])),
        web_dir: Some(fixture.path().to_path_buf()),
        ..DaemonConfig::default()
    };
    let _daemon = TestDaemon::spawn(cfg).await;
    common::wait_for_tcp(ui_addr).await;

    let resp = http_get(ui_addr, "/cairn.json").await;
    assert_eq!(resp.status, 200);
    assert_eq!(resp.header("access-control-allow-origin"), Some("*"));
    let doc: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    let endpoints = doc.get("endpoints").unwrap();
    // No ws:// listener configured at all, so no `websocket` key — the
    // dedicated listener has no `/ws` of its own to point at.
    assert!(
        endpoints.get("websocket").is_none(),
        "no ws:// listener configured, so websocket must be absent, got: {endpoints}"
    );
    let wt = endpoints.get("webtransport").unwrap();
    assert_eq!(
        wt.get("url").unwrap(),
        &serde_json::json!(format!("https://127.0.0.1:{}", wt_addr.port()))
    );
}

#[tokio::test]
async fn cairn_json_dedicated_ui_reports_absolute_ws_url_when_ws_listener_exists() {
    let fixture = tempfile::tempdir().unwrap();
    write_fixture_spa(fixture.path());

    let ws_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let ui_addr: SocketAddr = ([127, 0, 0, 1], common::free_tcp_port()).into();
    let cfg = DaemonConfig {
        listeners: vec![ListenerConfig::WebSocket(ws_addr)],
        web_ui: Some(WebUiMode::Dedicated(vec![ui_addr])),
        web_dir: Some(fixture.path().to_path_buf()),
        ..DaemonConfig::default()
    };
    let _daemon = TestDaemon::spawn(cfg).await;
    common::wait_for_tcp(ui_addr).await;

    let resp = http_get(ui_addr, "/cairn.json").await;
    let doc: serde_json::Value = serde_json::from_slice(&resp.body).unwrap();
    let endpoints = doc.get("endpoints").unwrap();
    assert_eq!(
        endpoints.get("websocket").unwrap(),
        &serde_json::json!(format!("ws://127.0.0.1:{}/ws", ws_addr.port()))
    );
}

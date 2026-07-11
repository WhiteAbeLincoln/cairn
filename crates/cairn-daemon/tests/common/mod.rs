//! Test harness for `cairn-daemon` integration tests.
//!
//! `DaemonHarness::start()` runs the real `Daemon` via `serve()` on a tempdir
//! socket and returns once the socket file appears. `Drop` cancels the daemon
//! and aborts the task. `client()` returns a wRPC unix client bound to that
//! socket.
//!
//! `start_with_wt()` additionally starts a WebTransport listener on a free
//! port, exposing the bound address so integration tests can connect over QUIC.

#![allow(dead_code)] // fields and methods are used à la carte by individual tests

use std::net::SocketAddr;
use std::path::PathBuf;

use cairn_daemon::{
    auth::{self, none::NoneBackend},
    config::{AuthBackendKind, DaemonConfig},
    daemon::Daemon,
    listen::ListenerConfig,
    serve::serve,
};
use tokio_util::sync::CancellationToken;

pub struct DaemonHarness {
    pub socket_path: PathBuf,
    /// Bound address of the WebTransport listener, if started with `start_with_wt()`.
    pub wt_addr: Option<SocketAddr>,
    /// Bound address of the WebSocket listener, if started with `start_with_ws*()`.
    pub ws_addr: Option<SocketAddr>,
    /// Hex-encoded SHA-256 cert hash for WebTransport client-side pinning,
    /// set when started with `start_with_wt()`.
    pub cert_hash: Option<String>,
    _tmp: tempfile::TempDir,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl DaemonHarness {
    pub async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");
        let cfg = DaemonConfig {
            listeners: vec![ListenerConfig::Unix(socket_path.clone())],
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone(), None));

        // Poll until the socket file appears; serve() binds before accepting.
        for _ in 0..100 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(socket_path.exists(), "daemon socket did not appear in time");

        Self {
            socket_path,
            wt_addr: None,
            ws_addr: None,
            cert_hash: None,
            _tmp: tmp,
            shutdown,
            task,
        }
    }

    /// Start a daemon with both UDS and WebTransport listeners.
    ///
    /// Pre-generates a self-signed TLS cert in the tempdir and configures the
    /// daemon to use it, so the test knows the cert hash for client-side pinning.
    /// The WT listener binds to a free high port on 127.0.0.1.
    pub async fn start_with_wt() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");
        let tls_dir = tmp.path().join("tls");

        // Generate the cert in advance so we can pass explicit paths to the
        // daemon and know the hash for client-side pinning.
        let tls = cairn_daemon::tls::TlsConfig::self_signed(&tls_dir).unwrap();
        let cert_hash = tls.spki_hash_hex();
        let cert_path = tls_dir.join("cert.pem");
        let key_path = tls_dir.join("key.pem");

        let port = free_port();
        let wt_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], port));

        let cfg = DaemonConfig {
            listeners: vec![
                ListenerConfig::Unix(socket_path.clone()),
                ListenerConfig::WebTransport(wt_addr),
            ],
            auth_backends: vec![AuthBackendKind::Tailscale],
            wt_tls: Some(cairn_daemon::config::WtTlsIdentity {
                cert: cert_path,
                key: key_path,
            }),
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
        let shutdown = CancellationToken::new();

        let test_chain = auth::AuthChain::new(vec![Box::new(NoneBackend)]);
        let task = tokio::spawn(serve(daemon, shutdown.clone(), Some(test_chain)));

        // Poll until the UDS socket file appears — the WT endpoint binds at
        // roughly the same time.
        for _ in 0..100 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(socket_path.exists(), "daemon socket did not appear in time");

        // Give the WT endpoint a moment to finish binding.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Self {
            socket_path,
            wt_addr: Some(wt_addr),
            ws_addr: None,
            cert_hash: Some(cert_hash),
            _tmp: tmp,
            shutdown,
            task,
        }
    }

    /// Start a daemon with both UDS and a loopback WebTransport listener,
    /// but **no auth backends**. The daemon should allow anonymous loopback
    /// connections without requiring `--auth`.
    pub async fn start_with_wt_no_auth() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");
        let tls_dir = tmp.path().join("tls");

        let tls = cairn_daemon::tls::TlsConfig::self_signed(&tls_dir).unwrap();
        let cert_hash = tls.spki_hash_hex();
        let cert_path = tls_dir.join("cert.pem");
        let key_path = tls_dir.join("key.pem");

        let port = free_port();
        let wt_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], port));

        let cfg = DaemonConfig {
            listeners: vec![
                ListenerConfig::Unix(socket_path.clone()),
                ListenerConfig::WebTransport(wt_addr),
            ],
            wt_tls: Some(cairn_daemon::config::WtTlsIdentity {
                cert: cert_path,
                key: key_path,
            }),
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
        let shutdown = CancellationToken::new();

        let task = tokio::spawn(serve(daemon, shutdown.clone(), None));

        for _ in 0..100 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(socket_path.exists(), "daemon socket did not appear in time");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        Self {
            socket_path,
            wt_addr: Some(wt_addr),
            ws_addr: None,
            cert_hash: Some(cert_hash),
            _tmp: tmp,
            shutdown,
            task,
        }
    }

    /// Start a daemon with both UDS and a loopback WebSocket listener, no auth
    /// backends and no extra allowed origins. Mirrors the local dev setup
    /// `--listen ws://127.0.0.1:0` with anonymous loopback access.
    pub async fn start_with_ws() -> Self {
        Self::start_with_ws_origins(vec![]).await
    }

    /// Like [`start_with_ws`](Self::start_with_ws) but with an explicit
    /// `ws_origins` allowlist for exercising origin validation.
    pub async fn start_with_ws_origins(ws_origins: Vec<String>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");

        let ws_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], free_tcp_port()));
        let cfg = DaemonConfig {
            listeners: vec![
                ListenerConfig::Unix(socket_path.clone()),
                ListenerConfig::WebSocket(ws_addr),
            ],
            ws_origins,
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone(), None));

        for _ in 0..100 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(socket_path.exists(), "daemon socket did not appear in time");
        wait_for_tcp(ws_addr).await;
        // Sanity: the daemon must still be serving after both listeners came up
        // (catches e.g. a lost port-binding race against a parallel test).
        assert!(!task.is_finished(), "daemon died during startup");

        Self {
            socket_path,
            wt_addr: None,
            ws_addr: Some(ws_addr),
            cert_hash: None,
            _tmp: tmp,
            shutdown,
            task,
        }
    }

    /// Start a daemon with UDS + a loopback WebSocket listener and the
    /// `tailscale-serve` auth backend (no whois backend, so it needs no
    /// running `tailscaled`). Exercises `TransportContext::Http` end to end:
    /// a client that sends `Tailscale-User-*` upgrade headers should be
    /// identified as that Tailscale user.
    pub async fn start_with_ws_tailscale_serve() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");

        let ws_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], free_tcp_port()));
        let cfg = DaemonConfig {
            listeners: vec![
                ListenerConfig::Unix(socket_path.clone()),
                ListenerConfig::WebSocket(ws_addr),
            ],
            auth_backends: vec![AuthBackendKind::TailscaleServe],
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone(), None));

        for _ in 0..100 {
            if socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(socket_path.exists(), "daemon socket did not appear in time");
        wait_for_tcp(ws_addr).await;

        Self {
            socket_path,
            wt_addr: None,
            ws_addr: Some(ws_addr),
            cert_hash: None,
            _tmp: tmp,
            shutdown,
            task,
        }
    }

    pub fn client(&self) -> wrpc_transport::unix::Client<PathBuf> {
        wrpc_transport::unix::Client::from(self.socket_path.clone())
    }

    /// A wRPC client that dials the WebSocket listener (one socket per invocation).
    pub fn ws_client(&self) -> WsClient {
        WsClient::new(self.ws_addr.expect("harness started without a WS listener"))
    }
}

/// A wRPC [`Invoke`](wrpc_transport::Invoke) client over the daemon's `ws://`
/// transport. Each invocation opens a fresh WebSocket carrying one call, the
/// browser SDK's model. Built on the version-agnostic `wrpc_websockets::split`
/// plus the published `wrpc_transport::frame::invoke`.
#[derive(Clone)]
pub struct WsClient {
    uri: String,
}

impl WsClient {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            uri: format!("ws://{addr}/ws"),
        }
    }
}

impl wrpc_transport::Invoke for WsClient {
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
        let (ws, _resp) = wrpc_websockets::tokio_websockets::ClientBuilder::new()
            .uri(&self.uri)?
            .connect()
            .await?;
        // `cairn_daemon::ws::split`: eager-flush the write half (so streamed
        // client events reach the daemon promptly over the buffered WS sink)
        // and drain the read half on drop (so closing after a response doesn't
        // RST the connection with the server's EOF sentinel unread).
        let (tx, rx) = cairn_daemon::ws::split(ws);
        wrpc_transport::frame::invoke(tx, rx, instance, func, params, paths).await
    }
}

impl Drop for DaemonHarness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.task.abort();
    }
}

/// Find a free UDP port by binding to :0 and reading the assigned port.
/// There's a small TOCTOU window but it works reliably for tests.
fn free_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Find a free TCP port by binding to :0 and reading the assigned port.
/// Small TOCTOU window; adequate for tests.
fn free_tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Poll until a TCP connection to `addr` succeeds (the listener is accepting).
async fn wait_for_tcp(addr: SocketAddr) {
    for _ in 0..200 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("TCP listener did not accept in time: {addr}");
}

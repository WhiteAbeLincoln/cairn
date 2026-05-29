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

use cairn_daemon::{config::DaemonConfig, daemon::Daemon, listen::ListenerConfig, serve::serve};
use tokio_util::sync::CancellationToken;

pub struct DaemonHarness {
    pub socket_path: PathBuf,
    /// Bound address of the WebTransport listener, if started with `start_with_wt()`.
    pub wt_addr: Option<SocketAddr>,
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
        let daemon = Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone()));

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
        let _tls = cairn_daemon::tls::TlsConfig::self_signed(&tls_dir).unwrap();
        let cert_path = tls_dir.join("cert.pem");
        let key_path = tls_dir.join("key.pem");

        let port = free_port();
        let wt_addr: SocketAddr = SocketAddr::from(([127, 0, 0, 1], port));

        let cfg = DaemonConfig {
            listeners: vec![
                ListenerConfig::Unix(socket_path.clone()),
                ListenerConfig::WebTransport(wt_addr),
            ],
            wt_cert: Some(cert_path),
            wt_key: Some(key_path),
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone()));

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
            _tmp: tmp,
            shutdown,
            task,
        }
    }

    pub fn client(&self) -> wrpc_transport::unix::Client<PathBuf> {
        wrpc_transport::unix::Client::from(self.socket_path.clone())
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

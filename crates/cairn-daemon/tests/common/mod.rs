//! Test harness for `cairn-daemon` integration tests.
//!
//! `DaemonHarness::start()` runs the real `Daemon` via `serve()` on a tempdir
//! socket and returns once the socket file appears. `Drop` cancels the daemon
//! and aborts the task. `client()` returns a wRPC unix client bound to that
//! socket.

#![allow(dead_code)] // fields and methods are used à la carte by individual tests

use std::path::PathBuf;

use cairn_daemon::{config::DaemonConfig, daemon::Daemon, listen::ListenerConfig, serve::serve};
use tokio_util::sync::CancellationToken;

pub struct DaemonHarness {
    pub socket_path: PathBuf,
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

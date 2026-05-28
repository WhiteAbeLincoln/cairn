//! Shared harness for `cairn` binary integration tests.
//!
//! Spins an in-process `cairn-daemon` on a tempdir UDS, then runs the real
//! `cairn` client binary against it. Tests get a `Harness` with helpers to
//! create sessions, call wRPC ops directly (for setup/assertion), and exec
//! the binary.
#![allow(dead_code)] // not every test uses every helper

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use cairn_daemon::config::DaemonConfig;
use cairn_daemon::daemon::Daemon;
use cairn_protocol::cairn::daemon::types::{SessionInfo, SessionSpec};
use cairn_protocol::client::cairn::daemon as api;
use tokio_util::sync::CancellationToken;

pub struct Harness {
    pub daemon: Daemon,
    pub socket: PathBuf,
    pub shutdown: CancellationToken,
    serve: tokio::task::JoinHandle<()>,
    _tmp: tempfile::TempDir,
}

impl Harness {
    /// Start a fresh daemon on a tempdir socket. Waits up to 2 s for the
    /// socket to appear.
    pub async fn start() -> anyhow::Result<Self> {
        let tmp = tempfile::tempdir()?;
        let socket = tmp.path().join("cairn.sock");
        let cfg = DaemonConfig { socket_path: socket.clone(), ..Default::default() };
        let daemon = Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let serve = {
            let daemon = daemon.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let _ = cairn_daemon::serve::serve(daemon, shutdown).await;
            })
        };
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        anyhow::ensure!(socket.exists(), "daemon socket was not created in time");
        Ok(Self { daemon, socket, shutdown, serve, _tmp: tmp })
    }

    /// Convenience: a spec for a session running `cmd args...`.
    pub fn spec(cmd: &[&str], name: Option<&str>) -> SessionSpec {
        SessionSpec {
            name: name.map(str::to_string),
            command: cmd.iter().map(|s| (*s).to_string()).collect(),
            env: vec![],
            env_inherit: true,
            workdir: None,
            tty: true,
            stdin: true,
            idle_timeout_secs: None,
            scrollback_lines: 100,
        }
    }

    /// Create a session through the registry, returning its `SessionInfo`.
    pub async fn create(&self, spec: SessionSpec) -> anyhow::Result<SessionInfo> {
        self.daemon
            .registry
            .create(spec, &self.daemon.cfg.default_shell)
            .await
            .map_err(|e| anyhow::anyhow!("create: {e:?}"))
    }

    /// Run the real `cairn` binary against this daemon with the given args.
    /// `stdin` is fed as bytes; stdout/stderr are captured.
    pub fn run(&self, args: &[&str], stdin: &[u8]) -> std::io::Result<Output> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.arg("--daemon")
            .arg(format!("unix://{}", self.socket.display()))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        use std::io::Write;
        if !stdin.is_empty() {
            child.stdin.as_mut().unwrap().write_all(stdin)?;
        }
        drop(child.stdin.take());
        child.wait_with_output()
    }

    /// Background variant of `run` — for follow/wait tests.
    pub fn spawn_run(&self, args: &[&str]) -> std::io::Result<std::process::Child> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.arg("--daemon")
            .arg(format!("unix://{}", self.socket.display()))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn()
    }

    pub fn client(&self) -> wrpc_transport::unix::Client<PathBuf> {
        wrpc_transport::unix::Client::from(self.socket.clone())
    }

    pub async fn list_all(&self) -> anyhow::Result<Vec<SessionInfo>> {
        api::sessions::list_all(&self.client(), ())
            .await
            .map_err(|e| anyhow::anyhow!("list_all: {e}"))
    }

    pub async fn inspect(&self, id: &str) -> anyhow::Result<SessionInfo> {
        let r = api::sessions::inspect(&self.client(), (), id)
            .await
            .map_err(|e| anyhow::anyhow!("inspect: {e}"))?;
        r.map_err(|e| anyhow::anyhow!("inspect wire-err: {} {}", e.code, e.message))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        // Best-effort: the serve task drops naturally when its socket closes.
        // We don't `.await` here (Drop is sync), and tokio will join on runtime
        // shutdown.
        let _ = &self.serve;
    }
}

/// Extract stdout from `Output` as a UTF-8 `String`.
pub fn stdout_str(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Extract stderr from `Output` as a UTF-8 `String`.
pub fn stderr_str(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

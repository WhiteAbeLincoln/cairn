//! End-to-end: the real `cairn attach` binary against an in-process daemon,
//! driven through a pty. Asserts input is echoed, the detach key exits cleanly,
//! and the session survives the detach.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cairn_daemon::config::DaemonConfig;
use cairn_daemon::daemon::Daemon;
use cairn_protocol::cairn::daemon::types::SessionSpec;
use tokio_util::sync::CancellationToken;

fn cat_spec() -> SessionSpec {
    SessionSpec {
        name: Some("itest".to_string()),
        command: vec!["cat".to_string()],
        env: vec![],
        env_inherit: true,
        workdir: None,
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 100,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_echoes_input_then_detach_keeps_session_alive() -> anyhow::Result<()> {
    // ---- in-process daemon on a tempdir socket ----
    let tmp = tempfile::tempdir()?;
    let sock = tmp.path().join("cairn.sock");
    let mut cfg = DaemonConfig::default();
    cfg.socket_path = sock.clone();
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
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(sock.exists(), "daemon socket was not created");

    // ---- create a `cat` session (echoes input) ----
    let info = daemon
        .registry
        .create(cat_spec(), &daemon.cfg.default_shell)
        .await
        .expect("create cat session");
    let id = info.id.clone();

    // ---- spawn `cairn attach <id>` wired to a pty ----
    let pty = nix::pty::openpty(None, None)?;
    let master_fd = pty.master.as_raw_fd();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.arg("--daemon")
        .arg(format!("unix://{}", sock.display()))
        .arg("attach")
        .arg(&id)
        // SAFETY: dup the slave for each of stdin/stdout/stderr.
        .stdin(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave.as_raw_fd())) })
        .stdout(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave.as_raw_fd())) })
        .stderr(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave.as_raw_fd())) });
    let mut child = cmd.spawn()?;
    drop(pty.slave); // the parent keeps only the master

    // ---- reader thread for the master fd ----
    let mut master = unsafe { std::fs::File::from_raw_fd(libc::dup(master_fd)) };
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    {
        let mut rd = master.try_clone()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match rd.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    // ---- give the client time to attach + receive the snapshot ----
    std::thread::sleep(Duration::from_millis(700));

    // ---- write input; expect `cat` to echo it back ----
    master.write_all(b"hello\n")?;
    master.flush()?;
    assert!(
        wait_for(&out_rx, b"hello", Duration::from_secs(3)),
        "attach should echo typed input back to the terminal"
    );

    // ---- send the default detach sequence (ctrl-q, ctrl-q) ----
    master.write_all(&[0x11, 0x11])?;
    master.flush()?;

    // ---- the client should exit cleanly ----
    let status = wait_child(&mut child, Duration::from_secs(5));
    assert_eq!(status.and_then(|s| s.code()), Some(0), "client should exit 0 on detach");

    // ---- the session must still be alive ----
    let entry = daemon.registry.resolve(&id).expect("session must survive detach");
    assert!(
        entry.handle().try_exit_status().is_none(),
        "child must still be running after the client detaches"
    );

    shutdown.cancel();
    let _ = serve.await;
    Ok(())
}

/// Drain the reader channel until `needle` is seen or the deadline passes.
fn wait_for(rx: &mpsc::Receiver<Vec<u8>>, needle: &[u8], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut acc = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                acc.extend_from_slice(&chunk);
                if acc.windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    false
}

/// Poll `try_wait` until the child exits or the deadline passes.
fn wait_child(child: &mut std::process::Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    let _ = child.kill();
    None
}

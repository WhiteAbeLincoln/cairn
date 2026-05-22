//! Integration tests for GhosttyPty spawn / wait / kill lifecycle.

use cairn_core::pty::{GhosttyPty, SpawnOptions, TermSize};

#[tokio::test]
async fn spawn_true_exits_cleanly() {
    let cmd = std::process::Command::new("true");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let status = pty.wait().await;
    assert!(status.success(), "expected `true` to exit 0, got {:?}", status);
}

#[tokio::test]
async fn kill_terminates_long_running_child() {
    // `sleep 60` would block the test runner — kill should make wait() return.
    let cmd = std::process::Command::new("sleep");
    let mut cmd = cmd;
    cmd.arg("60");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Brief delay so the child is actually running before we signal it.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    pty.kill().expect("kill");
    let status = pty.wait().await;
    assert!(!status.success(), "expected non-zero exit after kill, got {:?}", status);
}

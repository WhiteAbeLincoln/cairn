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

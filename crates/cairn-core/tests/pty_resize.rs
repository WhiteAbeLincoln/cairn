//! Integration tests for GhosttyPty resize semantics.

use cairn_core::pty::{GhosttyPty, PtySession, SpawnOptions, TermSize};

#[tokio::test]
async fn resize_updates_size_query() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("5");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    assert_eq!(pty.size().await.unwrap(), TermSize { cols: 80, rows: 24 });

    pty.resize(TermSize {
        cols: 120,
        rows: 40,
    })
    .await
    .expect("resize");
    assert_eq!(
        pty.size().await.unwrap(),
        TermSize {
            cols: 120,
            rows: 40
        }
    );

    pty.kill().expect("kill");
    let _ = pty.wait().await;
}

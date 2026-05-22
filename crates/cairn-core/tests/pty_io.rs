//! Integration tests for GhosttyPty subscribe / write / scrollback I/O.

use cairn_core::pty::{GhosttyPty, PtySession, SpawnOptions, TermSize};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Read from the subscription stream until either the deadline elapses or
/// the accumulated bytes contain the needle. Returns the accumulated bytes.
async fn read_until_contains(
    sub: &mut cairn_core::pty::Subscription,
    needle: &[u8],
    deadline: Duration,
) -> Vec<u8> {
    let mut acc = sub.snapshot.to_vec();
    if acc.windows(needle.len()).any(|w| w == needle) {
        return acc;
    }
    let read = async {
        loop {
            match sub.stream.recv().await {
                Ok(chunk) => {
                    acc.extend_from_slice(&chunk);
                    if acc.windows(needle.len()).any(|w| w == needle) {
                        return acc;
                    }
                }
                Err(RecvError::Closed) => return acc,
                Err(RecvError::Lagged(_)) => continue,
            }
        }
    };
    tokio::time::timeout(deadline, read)
        .await
        .unwrap_or_else(|_| vec![])
}

#[tokio::test]
async fn echo_output_is_broadcast_to_subscribers() {
    let mut cmd = std::process::Command::new("printf");
    cmd.arg("hello-cairn");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe().await.expect("subscribe");
    let bytes = read_until_contains(&mut sub, b"hello-cairn", Duration::from_secs(2)).await;
    assert!(
        bytes.windows(b"hello-cairn".len()).any(|w| w == b"hello-cairn"),
        "did not see 'hello-cairn' in PTY output; got {:?}",
        std::str::from_utf8(&bytes).unwrap_or("<non-utf8>")
    );

    // Process should have exited; wait so the test doesn't leak the worker.
    let _ = pty.wait().await;
}

#[tokio::test]
async fn size_reports_configured_dimensions() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("5");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 132, rows: 50 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let size = pty.size().await.expect("size");
    assert_eq!(size, TermSize { cols: 132, rows: 50 });
    pty.kill().expect("kill");
    let _ = pty.wait().await;
}

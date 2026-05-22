//! Integration tests for GhosttyPty subscribe / write / scrollback I/O.

use bytes::Bytes;
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

#[tokio::test]
async fn write_delivers_bytes_to_child_stdin() {
    // `cat` echoes its stdin back to stdout. We write a line; it should
    // come back through the subscription stream.
    let cmd = std::process::Command::new("cat");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe().await.expect("subscribe");
    pty.write(Bytes::from_static(b"ping-cairn\n"))
        .await
        .expect("write");

    let bytes = read_until_contains(&mut sub, b"ping-cairn", Duration::from_secs(2)).await;
    assert!(
        bytes.windows(b"ping-cairn".len()).any(|w| w == b"ping-cairn"),
        "did not see echoed 'ping-cairn'; got {:?}",
        std::str::from_utf8(&bytes).unwrap_or("<non-utf8>")
    );

    pty.kill().expect("kill");
    let _ = pty.wait().await;
}

#[tokio::test]
async fn spawn_succeeds_with_terminal_attached() {
    // Regression guard: when libghostty-vt's Terminal is wired into the
    // worker, spawning and basic broadcast must still work. Behavioral
    // change comes in Task 14 (snapshot via Formatter).
    let mut cmd = std::process::Command::new("printf");
    cmd.arg("vt-attached");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let mut sub = pty.subscribe().await.expect("subscribe");
    let bytes = read_until_contains(&mut sub, b"vt-attached", Duration::from_secs(2)).await;
    assert!(
        bytes.windows(b"vt-attached".len()).any(|w| w == b"vt-attached"),
        "did not see 'vt-attached'"
    );
    let _ = pty.wait().await;
}

#[tokio::test]
async fn late_subscriber_sees_prior_output_in_snapshot() {
    // First subscriber starts immediately; second subscribes after the child
    // has finished writing. The second's snapshot should contain the same
    // visible content (text "late-join-marker") as the first saw via stream.
    let mut cmd = std::process::Command::new("printf");
    cmd.arg("late-join-marker");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub1 = pty.subscribe().await.expect("subscribe-1");
    let bytes1 = read_until_contains(&mut sub1, b"late-join-marker", Duration::from_secs(2)).await;
    assert!(bytes1
        .windows(b"late-join-marker".len())
        .any(|w| w == b"late-join-marker"));

    // Wait for child exit so subsequent reads return Closed promptly.
    let _ = pty.wait().await;

    let sub2 = pty.subscribe().await.expect("subscribe-2");
    // The snapshot bytes are an opaque VT escape stream; we don't try to
    // parse them, but the literal text 'late-join-marker' (printed by
    // printf) should still appear somewhere in the encoded screen since
    // libghostty-vt's Formatter emits literal characters for printable cells.
    assert!(
        sub2.snapshot
            .windows(b"late-join-marker".len())
            .any(|w| w == b"late-join-marker"),
        "snapshot missing 'late-join-marker'; got {:?}",
        std::str::from_utf8(&sub2.snapshot).unwrap_or("<non-utf8>")
    );
}

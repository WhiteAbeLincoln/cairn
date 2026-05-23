//! Integration tests for GhosttyPty subscribe / write / scrollback I/O.

use bytes::Bytes;
use cairn_pty::{ClientId, GhosttyPty, PtySession, SpawnOptions, TermSize};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Read from the subscription stream until either the deadline elapses or
/// the accumulated bytes contain the needle. Returns the accumulated bytes.
async fn read_until_contains(
    sub: &mut cairn_pty::Subscription,
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
    let mut cmd = tokio::process::Command::new("printf");
    cmd.arg("hello-cairn");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
    let bytes = read_until_contains(&mut sub, b"hello-cairn", Duration::from_secs(2)).await;
    assert!(
        bytes
            .windows(b"hello-cairn".len())
            .any(|w| w == b"hello-cairn"),
        "did not see 'hello-cairn' in PTY output; got {:?}",
        std::str::from_utf8(&bytes).unwrap_or("<non-utf8>")
    );

    // Process should have exited; wait so the test doesn't leak the worker.
    let _ = pty.wait().await;
}

#[tokio::test]
async fn size_reports_configured_dimensions() {
    let mut cmd = tokio::process::Command::new("sleep");
    cmd.arg("5");
    let opts = SpawnOptions::new(cmd).with_size(TermSize {
        cols: 132,
        rows: 50,
    });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let size = pty.size().await.expect("size");
    assert_eq!(
        size,
        TermSize {
            cols: 132,
            rows: 50
        }
    );
    pty.kill().expect("kill");
    let _ = pty.wait().await;
}

#[tokio::test]
async fn write_delivers_bytes_to_child_stdin() {
    // `cat` echoes its stdin back to stdout. We write a line; it should
    // come back through the subscription stream.
    let cmd = tokio::process::Command::new("cat");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
    pty.write(ClientId::from_u64(0), Bytes::from_static(b"ping-cairn\n"))
        .await
        .expect("write");

    let bytes = read_until_contains(&mut sub, b"ping-cairn", Duration::from_secs(2)).await;
    assert!(
        bytes
            .windows(b"ping-cairn".len())
            .any(|w| w == b"ping-cairn"),
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
    let mut cmd = tokio::process::Command::new("printf");
    cmd.arg("vt-attached");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let mut sub = pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
    let bytes = read_until_contains(&mut sub, b"vt-attached", Duration::from_secs(2)).await;
    assert!(
        bytes
            .windows(b"vt-attached".len())
            .any(|w| w == b"vt-attached"),
        "did not see 'vt-attached'"
    );
    let _ = pty.wait().await;
}

#[tokio::test]
async fn late_subscriber_sees_prior_output_in_snapshot() {
    // First subscriber starts immediately; second subscribes after the child
    // has finished writing. The second's snapshot should contain the same
    // visible content (text "late-join-marker") as the first saw via stream.
    let mut cmd = tokio::process::Command::new("printf");
    cmd.arg("late-join-marker");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub1 = pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe-1");
    let bytes1 = read_until_contains(&mut sub1, b"late-join-marker", Duration::from_secs(2)).await;
    assert!(
        bytes1
            .windows(b"late-join-marker".len())
            .any(|w| w == b"late-join-marker")
    );

    // Wait for child exit so subsequent reads return Closed promptly.
    let _ = pty.wait().await;

    let sub2 = pty.subscribe(ClientId::from_u64(1)).await.expect("subscribe-2");
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

#[tokio::test]
async fn da1_query_gets_response_without_client() {
    // Verifies the count == 0 path end-to-end: with no subscriber
    // attached, libghostty's default DA1 reply (\x1b[?62;22c) flows
    // through the PTY to the child, the child's `read` returns the
    // reply bytes, and we observe a non-zero reply length.
    //
    // Race-free: we wait for the child to exit BEFORE subscribing,
    // so the entire query/reply roundtrip happens with count == 0.
    // The post-exit snapshot contains the child's final stdout
    // (`reply-len=N`) which we parse.
    let script = r#"
        printf '\033[c'
        read -r -n 32 -t 1 reply
        printf 'reply-len=%d\n' "${#reply}"
    "#;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Wait for the child to finish the whole script. After this, the
    // snapshot captures the final terminal state including the
    // `reply-len=N` line.
    let _ = pty.wait().await;

    let sub = pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
    let text = std::str::from_utf8(&sub.snapshot).unwrap_or("<non-utf8>");
    assert!(
        text.contains("reply-len="),
        "missing reply-len marker in snapshot: {text}"
    );
    assert!(
        !text.contains("reply-len=0"),
        "expected non-zero reply length (terminal responded to DA1), got: {text}"
    );
}

#[tokio::test]
async fn da1_query_suppressed_when_client_attached() {
    // Verifies the count >= 1 path end-to-end: with a Subscription held
    // during the query, the worker's gate drops libghostty's default
    // DA1 reply, the child's `read` times out, and reply-len=0 lands
    // in the snapshot.
    //
    // The leading `sleep 0.2` ensures our subscribe call lands before
    // the child issues its DA1 query, so count == 1 at query time.
    let script = r#"
        sleep 0.2
        printf '\033[c'
        read -r -n 32 -t 1 reply
        printf 'reply-len=%d\n' "${#reply}"
    "#;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Subscribe immediately — this is the "primary client" whose presence
    // suppresses backend auto-replies. Hold the Subscription across the
    // child's entire run by keeping `_sub` alive until after `wait`.
    let _sub = pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");

    let _ = pty.wait().await;

    // `_sub` is still alive here (primary_count == 2 once sub2 lands) and
    // outlives this function. That's fine: the snapshot we read below was
    // captured by the worker while the child was running, with count == 1
    // suppressing the DA1 reply — exactly what we want to observe.
    let sub2 = pty.subscribe(ClientId::from_u64(1)).await.expect("subscribe-2");
    let text = std::str::from_utf8(&sub2.snapshot).unwrap_or("<non-utf8>");
    assert!(
        text.contains("reply-len="),
        "missing reply-len marker in snapshot: {text}"
    );
    assert!(
        text.contains("reply-len=0"),
        "expected reply-len=0 (gate suppressed backend DA1 reply), got: {text}"
    );
}

#[tokio::test]
async fn subscribers_observe_close_on_child_exit() {
    let cmd = tokio::process::Command::new("true");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe(ClientId::from_u64(0)).await.expect("subscribe");
    let _ = pty.wait().await;

    // Loop draining anything still in the channel, then assert we eventually
    // get Closed (not Lagged, not data).
    let saw_close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match sub.stream.recv().await {
                Ok(_) => continue,
                Err(RecvError::Closed) => return true,
                Err(RecvError::Lagged(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(
        saw_close,
        "subscribers did not observe Closed after child exit"
    );
}

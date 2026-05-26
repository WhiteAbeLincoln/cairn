//! Integration tests for GhosttyPty spawn / wait / kill lifecycle.

use bytes::Bytes;
use cairn_pty::{ClientId, GhosttyPty, PtyError, PtySession, SpawnOptions, TermSize};
use tokio::sync::broadcast::error::RecvError;

#[tokio::test]
async fn spawn_true_exits_cleanly() {
    let cmd = tokio::process::Command::new("true");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let status = pty.wait().await;
    assert!(
        status.success(),
        "expected `true` to exit 0, got {:?}",
        status
    );
}

#[tokio::test]
async fn kill_terminates_long_running_child() {
    // `sleep 60` would block the test runner — kill should make wait() return.
    let cmd = tokio::process::Command::new("sleep");
    let mut cmd = cmd;
    cmd.arg("60");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Brief delay so the child is actually running before we signal it.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    pty.kill().expect("kill");
    let status = pty.wait().await;
    assert!(
        !status.success(),
        "expected non-zero exit after kill, got {:?}",
        status
    );
}

#[tokio::test]
async fn drop_kills_running_child() {
    use std::time::Duration;

    let mut cmd = tokio::process::Command::new("sleep");
    cmd.arg("60");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Subscribe BEFORE drop so we have a stream to observe Closed on.
    // RecvError::Closed arrives when the broadcast sender is dropped, which
    // happens inside the forwarder task once the child exits. If Drop does
    // NOT kill the child, the forwarder will be waiting for PTY EOF for the
    // full 60 seconds of the sleep.
    let mut sub = pty
        .subscribe(ClientId::from_u64(0))
        .await
        .expect("subscribe");

    // Brief delay so the child is actually running.
    tokio::time::sleep(Duration::from_millis(50)).await;

    drop(pty);

    // The broadcast stream should close within a couple of seconds (kill →
    // child exits → reader EOF → forwarder nulls bcast_tx → subscribers see
    // Closed). Without Drop killing the child, this loop would wait 60 s.
    let saw_close = tokio::time::timeout(Duration::from_secs(5), async {
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
    assert!(saw_close, "stream did not close within 5 s after drop");
}

#[tokio::test]
async fn wait_returns_status_with_code_and_timestamp() {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg("exit 7");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let before = now_ms();
    let status = pty.wait().await;
    let after = now_ms();

    assert_eq!(status.code(), Some(7));
    assert!(
        status.unix_ms() >= before && status.unix_ms() <= after,
        "exit timestamp {} not within [{before}, {after}]",
        status.unix_ms()
    );
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[tokio::test]
async fn write_after_exit_returns_closed() {
    use std::time::Duration;

    // Spawn a child that exits immediately.
    let cmd = tokio::process::Command::new("true");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Subscribe before the child exits so we can deterministically wait
    // for teardown (broadcast Close signal).
    let mut sub = pty
        .subscribe(ClientId::from_u64(0))
        .await
        .expect("subscribe");

    let _ = pty.wait().await;

    // Wait for the broadcast to close. This is the same signal the forwarder
    // task emits when it nulls the writer on EOF, so by the time we observe
    // Closed here, the writer is None and write() will return PtyError::Closed.
    // This replaces a fragile fixed sleep with an event-driven barrier.
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
    assert!(saw_close, "broadcast did not close after child exit");

    let result = pty
        .write(ClientId::from_u64(0), Bytes::from_static(b"hello"))
        .await;
    assert!(
        matches!(result, Err(PtyError::Closed)),
        "expected Closed after child exit, got {:?}",
        result
    );
}

#[tokio::test]
async fn try_exit_status_is_none_before_exit_and_some_after() {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg("sleep 100");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    assert!(pty.try_exit_status().is_none(), "should be running");

    pty.kill().expect("kill");
    let _ = pty.wait().await; // ensure exit published
    assert!(pty.try_exit_status().is_some(), "should be exited");
}

#[tokio::test]
async fn size_returns_cached_value_after_exit() {
    let cmd = tokio::process::Command::new("true");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 100, rows: 40 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let _ = pty.wait().await; // `true` exits immediately

    let size = pty.size().await.expect("size should still work post-exit");
    assert_eq!(size, TermSize { cols: 100, rows: 40 });
}

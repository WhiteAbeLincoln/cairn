//! Integration test for multi-client election against a real PTY.
//!
//! Drives a real `/bin/cat` through the production worker path and
//! verifies that ClientId-aware resize, leader election, and detach
//! work end-to-end. The bulk of correctness lives in the mock-driven
//! worker tests (`src/ghostty/worker.rs::tests`); this test guards
//! against breakage at the real-PTY layer (kernel scheduling, actual
//! tokio::process::Child interactions, etc.).

use std::time::Duration;

use bytes::Bytes;
use cairn_pty::{ClientId, GhosttyPty, PtyError, PtySession, SpawnOptions, TermSize};

#[tokio::test]
async fn two_clients_resize_election_against_real_pty() {
    let cmd = tokio::process::Command::new("/bin/cat");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    let _sub_a = pty.subscribe(a).await.expect("subscribe a");
    pty.resize(a, TermSize { cols: 100, rows: 30 })
        .await
        .expect("a's first resize promotes a to leader");

    let _sub_b = pty.subscribe(b).await.expect("subscribe b");
    let err = pty
        .resize(b, TermSize { cols: 120, rows: 40 })
        .await
        .expect_err("b is not leader");
    match err {
        PtyError::NotLeader { requester, current } => {
            assert_eq!(requester, b);
            assert_eq!(current, Some(a));
        }
        other => panic!("expected NotLeader, got {other:?}"),
    }

    // b types — claims leadership.
    pty.write(b, Bytes::from_static(b"hello"))
        .await
        .expect("b writes");
    // Now b can resize.
    pty.resize(b, TermSize { cols: 120, rows: 40 })
        .await
        .expect("b should now be leader");

    pty.kill().expect("kill");
}

#[tokio::test]
async fn leader_seat_clears_on_subscription_drop_against_real_pty() {
    let cmd = tokio::process::Command::new("/bin/cat");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    let sub_a = pty.subscribe(a).await.expect("subscribe a");
    pty.resize(a, TermSize { cols: 100, rows: 30 })
        .await
        .expect("a is leader");

    drop(sub_a);
    // Give worker a moment to process Detach.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // b can now claim the empty seat.
    pty.resize(b, TermSize { cols: 110, rows: 35 })
        .await
        .expect("b claims empty seat");

    pty.kill().expect("kill");
}

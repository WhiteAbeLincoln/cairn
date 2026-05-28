use std::time::Duration;

use cairn_pty::{ClientId, GhosttyPty, PtySession, SpawnOptions};

#[tokio::test]
async fn signal_term_kills_child() {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg("sleep 100");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    pty.signal(nix::sys::signal::Signal::SIGTERM).await.expect("signal");

    let status = tokio::time::timeout(Duration::from_secs(5), pty.wait())
        .await
        .expect("child should exit after SIGTERM");
    assert_eq!(status.signal(), Some(15), "child should die from SIGTERM");
}

#[tokio::test]
async fn inject_writes_to_pty_without_claiming_leadership() {
    let cmd = tokio::process::Command::new("cat");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    // A types — becomes leader.
    pty.write(a, bytes::Bytes::from_static(b"hi\n")).await.expect("a writes");

    // Inject from no client identity — must NOT promote anyone.
    pty.inject(bytes::Bytes::from_static(b"yo\n")).await.expect("inject");

    // A is still the leader: B's resize is rejected as NotLeader(current = A).
    let err = pty
        .resize(b, cairn_pty::TermSize { cols: 100, rows: 30 })
        .await
        .expect_err("b should not be leader");
    match err {
        cairn_pty::PtyError::NotLeader { current, .. } => assert_eq!(current, Some(a)),
        other => panic!("expected NotLeader, got {other:?}"),
    }
}

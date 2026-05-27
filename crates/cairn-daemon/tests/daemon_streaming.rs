//! Direct handler tests for the streaming session operations:
//! wait, send (Tasks 2–3).

use cairn_daemon::{config::DaemonConfig, daemon::Daemon};
use cairn_protocol::cairn::daemon::types::SessionSpec;

fn test_daemon() -> Daemon {
    Daemon::new(DaemonConfig::default())
}

async fn create(daemon: &Daemon, name: &str, cmd: &[&str]) -> cairn_protocol::cairn::daemon::types::SessionInfo {
    let spec = SessionSpec {
        name: Some(name.to_string()),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        env: vec![],
        env_inherit: true,
        workdir: None,
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 100,
    };
    daemon.registry.create(spec, &daemon.cfg.default_shell).await.unwrap()
}

// ── wait ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn wait_resolves_with_exit_code() {
    let daemon = test_daemon();
    let info = create(&daemon, "w", &["sh", "-c", "exit 7"]).await;
    let fut = cairn_daemon::handlers::wait::wait(&daemon, info.id.clone())
        .await
        .expect("wait setup");
    let exit = fut.await;
    assert_eq!(exit.code, Some(7));
}

#[tokio::test]
async fn wait_unknown_is_err() {
    let daemon = test_daemon();
    assert!(cairn_daemon::handlers::wait::wait(&daemon, "nope".into()).await.is_err());
}

// ── send ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn send_injects_into_session() {
    let daemon = test_daemon();
    let info = create(&daemon, "s", &["cat"]).await;

    // Observer: subscribe directly via the registry handle so we can read echo.
    let entry = daemon.registry.resolve(&info.id).unwrap();
    let cid = daemon.registry.mint_client_id();
    let mut sub = entry.handle().subscribe(cid).await.unwrap();

    // send "ping\n" as one chunk.
    let chunks = futures::stream::iter(vec![vec![bytes::Bytes::from_static(b"ping\n")]]);
    let res = cairn_daemon::handlers::send::send(&daemon, info.id.clone(), Box::pin(chunks)).await;
    assert!(res.is_ok());

    // cat echoes; the broadcast should carry "ping" within a short window.
    let saw = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sub.stream.recv().await {
                Ok(b) if b.windows(4).any(|w| w == b"ping") => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(saw, "injected bytes should be echoed by cat");
}

#[tokio::test]
async fn send_unknown_is_not_found() {
    let chunks = futures::stream::iter(Vec::<Vec<bytes::Bytes>>::new());
    let daemon = test_daemon();
    let err = cairn_daemon::handlers::send::send(&daemon, "nope".into(), Box::pin(chunks))
        .await
        .expect_err("not found");
    assert_eq!(err.code, "session.not_found");
}

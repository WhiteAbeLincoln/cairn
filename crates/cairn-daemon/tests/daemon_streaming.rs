//! Direct handler tests for the streaming session operations:
//! wait, send (Tasks 2–3), logs (Task 4), attach (Task 5).

use cairn_daemon::{config::DaemonConfig, daemon::Daemon};
use cairn_protocol::cairn::daemon::types::{LogWindow, SessionSpec};
use futures::StreamExt as _;

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

// ── logs ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn logs_without_follow_emits_snapshot_then_closes() {
    let daemon = test_daemon();
    // A session that prints something so the snapshot is non-trivial.
    let info = create(&daemon, "l", &["sh", "-c", "printf hello; sleep 100"]).await;

    // The child's `printf` may not have been read into the VT buffer yet. Each
    // `logs` call (no follow) takes a fresh snapshot and must terminate on its
    // own; poll until the snapshot is non-empty (bounded) rather than guessing
    // with a fixed sleep.
    let mut bytes = Vec::new();
    for _ in 0..40 {
        let mut stream = cairn_daemon::handlers::logs::logs(
            &daemon, info.id.clone(), LogWindow::All, false,
        ).await.expect("logs");
        let mut buf = Vec::new();
        let collected = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(batch) = stream.next().await {
                for chunk in batch { buf.extend_from_slice(&chunk); }
            }
        }).await;
        assert!(collected.is_ok(), "logs without follow must terminate");
        if !buf.is_empty() {
            bytes = buf;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!bytes.is_empty(), "snapshot should contain the printed output");
}

#[tokio::test]
async fn logs_unknown_is_err() {
    let daemon = test_daemon();
    assert!(cairn_daemon::handlers::logs::logs(&daemon, "nope".into(), LogWindow::All, false).await.is_err());
}

// ── attach ────────────────────────────────────────────────────────────────

use cairn_protocol::cairn::daemon::types::{AttachInit, ClientEvent, ServerEvent};

fn attach_init() -> AttachInit { AttachInit { cols: 80, rows: 24, no_stdin: false } }

// Drain the next server-event batch within a timeout, flattened.
async fn next_events(s: &mut (impl futures::Stream<Item = Vec<ServerEvent>> + Unpin)) -> Vec<ServerEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(2), s.next())
        .await.ok().flatten().unwrap_or_default()
}

#[tokio::test]
async fn attach_first_event_is_snapshot() {
    let daemon = test_daemon();
    let info = create(&daemon, "a", &["cat"]).await;
    let events = futures::stream::pending::<Vec<ClientEvent>>(); // no client input
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let first = next_events(&mut out).await;
    assert!(matches!(first.first(), Some(ServerEvent::Snapshot(_))), "first event must be Snapshot");
}

#[tokio::test]
async fn attach_input_is_echoed_as_output() {
    let daemon = test_daemon();
    let info = create(&daemon, "a2", &["cat"]).await;
    // Send one Input batch then keep the stream open (pending).
    let events = futures::stream::once(async {
        vec![ClientEvent::Input(bytes::Bytes::from_static(b"hey\n"))]
    }).chain(futures::stream::pending());
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;
    // cat echoes "hey"; look for an Output event containing it.
    let mut saw = false;
    for _ in 0..10 {
        for ev in next_events(&mut out).await {
            if let ServerEvent::Output(b) = ev
                && b.windows(3).any(|w| w == b"hey")
            {
                saw = true;
            }
        }
        if saw { break; }
    }
    assert!(saw, "input should be echoed back as Output");
}

#[tokio::test]
async fn attach_detach_event_ends_stream() {
    let daemon = test_daemon();
    let info = create(&daemon, "a3", &["cat"]).await;
    let events = futures::stream::once(async { vec![ClientEvent::Detach] });
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;
    // After Detach the stream must end (next() yields None within the timeout).
    let ended = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while out.next().await.is_some() {}
    }).await;
    assert!(ended.is_ok(), "stream should end after Detach");
}

#[tokio::test]
async fn attach_emits_exited_when_child_dies() {
    let daemon = test_daemon();
    let info = create(&daemon, "a4", &["sh", "-c", "sleep 100"]).await;
    let events = futures::stream::pending::<Vec<ClientEvent>>();
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;
    // Kill via the registry handle; the bridge should emit Exited then end.
    daemon.registry.resolve(&info.id).unwrap().handle().signal(libc::SIGKILL).await.unwrap();
    let mut saw_exit = false;
    for _ in 0..20 {
        for ev in next_events(&mut out).await {
            if matches!(ev, ServerEvent::Exited(_)) { saw_exit = true; }
        }
        if saw_exit { break; }
    }
    assert!(saw_exit, "bridge should emit Exited when the child dies");
}

#[tokio::test]
async fn attach_unknown_session_yields_error_event() {
    let daemon = test_daemon();
    let events = futures::stream::pending::<Vec<ClientEvent>>();
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, "nope".into(), attach_init(), Box::pin(events),
    ).await;
    let first = next_events(&mut out).await;
    assert!(matches!(first.first(), Some(ServerEvent::Error(_))), "unknown id -> Error event");
}

#[tokio::test]
async fn kick_emits_kicked_event_then_ends() {
    let daemon = test_daemon();
    let info = create(&daemon, "a6", &["cat"]).await;
    let events = futures::stream::pending::<Vec<ClientEvent>>();
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;

    // attach() registers the client synchronously before returning, so kick finds it.
    cairn_daemon::handlers::sessions::kick(&daemon, info.id.clone(), None)
        .await
        .expect("kick");

    // The bridge must emit Error{client.kicked} and then end the stream.
    let mut saw_kicked = false;
    let ended = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match out.next().await {
                Some(batch) => {
                    for ev in batch {
                        if let ServerEvent::Error(e) = ev
                            && e.code == cairn_protocol::error_codes::CLIENT_KICKED
                        {
                            saw_kicked = true;
                        }
                    }
                }
                None => break,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "kick should end the attached stream");
    assert!(saw_kicked, "kick should emit a client.kicked error event before ending");
}

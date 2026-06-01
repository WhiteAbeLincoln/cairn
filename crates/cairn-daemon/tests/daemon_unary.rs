//! Integration tests for unary session operations:
//! create / list / inspect / rename / restart / kill / kick.

mod common;

use bindings::cairn::daemon::types::{SessionSpec, Signal, SignalName};
use cairn_protocol as bindings;
use common::DaemonHarness;

fn spec(name: &str, cmd: &[&str]) -> SessionSpec {
    SessionSpec {
        name: Some(name.to_string()),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        env: vec![],
        env_inherit: true,
        workdir: None,
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 100,
    }
}

// ── create / list / inspect ───────────────────────────────────────────────

#[tokio::test]
async fn create_then_list_then_inspect() {
    let h = DaemonHarness::start().await;
    let client = h.client();

    let created = bindings::client::cairn::daemon::sessions::create(
        &client,
        (),
        None,
        &spec("dev", &["sleep", "100"]),
    )
    .await
    .expect("create invocation")
    .expect("create result");
    assert_eq!(created.name, Some("dev".to_string()));
    // session has no exit yet
    assert!(created.exit.is_none());

    let listed = bindings::client::cairn::daemon::sessions::list_all(&client, (), None)
        .await
        .expect("list_all invocation");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let got = bindings::client::cairn::daemon::sessions::inspect(&client, (), None, &created.id)
        .await
        .expect("inspect invocation")
        .expect("inspect result");
    assert_eq!(got.id, created.id);
    assert_eq!(got.name, Some("dev".to_string()));
}

#[tokio::test]
async fn inspect_unknown_is_not_found() {
    let h = DaemonHarness::start().await;
    let err =
        bindings::client::cairn::daemon::sessions::inspect(&h.client(), (), None, "no-such-id")
            .await
            .expect("inspect invocation")
            .expect_err("should be not found");
    assert_eq!(err.code, "session.not_found");
}

#[tokio::test]
async fn duplicate_name_is_rejected() {
    let h = DaemonHarness::start().await;
    let client = h.client();

    bindings::client::cairn::daemon::sessions::create(
        &client,
        (),
        None,
        &spec("dev", &["sleep", "100"]),
    )
    .await
    .expect("first create")
    .expect("first create ok");

    let err = bindings::client::cairn::daemon::sessions::create(
        &client,
        (),
        None,
        &spec("dev", &["sleep", "100"]),
    )
    .await
    .expect("second create invocation")
    .expect_err("should be dup name");
    assert_eq!(err.code, "session.name_in_use");
}

// ── kill ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn kill_term_stops_session() {
    let h = DaemonHarness::start().await;
    let client = h.client();

    let created = bindings::client::cairn::daemon::sessions::create(
        &client,
        (),
        None,
        &spec("dev", &["sleep", "100"]),
    )
    .await
    .expect("create")
    .expect("create ok");

    let sig = Signal::Named(SignalName::Term);
    bindings::client::cairn::daemon::sessions::kill(&client, (), None, &created.id, &sig, None)
        .await
        .expect("kill invocation")
        .expect("kill ok");

    // Poll inspect until exit is populated (SIGTERM should stop `sleep 100`).
    let mut exited = false;
    for _ in 0..50 {
        let got =
            bindings::client::cairn::daemon::sessions::inspect(&client, (), None, &created.id)
                .await
                .expect("inspect")
                .expect("inspect ok");
        if got.exit.is_some() {
            exited = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(exited, "session should have exited after SIGTERM");
}

#[tokio::test]
async fn kill_with_grace_escalates_to_sigkill() {
    let h = DaemonHarness::start().await;
    let client = h.client();

    // Shell that ignores SIGTERM; only SIGKILL can stop it.
    let created = bindings::client::cairn::daemon::sessions::create(
        &client,
        (),
        None,
        &spec("stubborn", &["sh", "-c", "trap '' TERM; sleep 100"]),
    )
    .await
    .expect("create")
    .expect("create ok");

    let sig = Signal::Named(SignalName::Term);
    // grace_ms = 300 ms — SIGTERM is ignored, SIGKILL fires after 300 ms.
    bindings::client::cairn::daemon::sessions::kill(
        &client,
        (),
        None,
        &created.id,
        &sig,
        Some(300),
    )
    .await
    .expect("kill invocation")
    .expect("kill ok");

    let mut exited = false;
    for _ in 0..60 {
        let got =
            bindings::client::cairn::daemon::sessions::inspect(&client, (), None, &created.id)
                .await
                .expect("inspect")
                .expect("inspect ok");
        if got.exit.is_some() {
            exited = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        exited,
        "escalation should have SIGKILLed the stubborn session"
    );
}

// ── rename + restart ──────────────────────────────────────────────────────

#[tokio::test]
async fn rename_and_restart() {
    let h = DaemonHarness::start().await;
    let client = h.client();

    let created = bindings::client::cairn::daemon::sessions::create(
        &client,
        (),
        None,
        &spec("old", &["sleep", "100"]),
    )
    .await
    .expect("create")
    .expect("create ok");

    // Rename old → new.
    bindings::client::cairn::daemon::sessions::rename(&client, (), None, &created.id, "new")
        .await
        .expect("rename")
        .expect("rename ok");

    let got = bindings::client::cairn::daemon::sessions::inspect(&client, (), None, "new")
        .await
        .expect("inspect by new name")
        .expect("inspect ok");
    assert_eq!(got.name, Some("new".to_string()));

    // restart while running without force → session.running error.
    let err =
        bindings::client::cairn::daemon::sessions::restart(&client, (), None, &created.id, false)
            .await
            .expect("restart invocation")
            .expect_err("should reject running");
    assert_eq!(err.code, "session.running");

    // with force → ok, same id.
    bindings::client::cairn::daemon::sessions::restart(&client, (), None, &created.id, true)
        .await
        .expect("force restart invocation")
        .expect("force restart ok");

    // still resolves under the same id.
    let after = bindings::client::cairn::daemon::sessions::inspect(&client, (), None, &created.id)
        .await
        .expect("inspect after restart")
        .expect("inspect ok");
    assert_eq!(after.id, created.id);
}

//! End-to-end round-trip tests for the `cairn-protocol` bindings.
//!
//! Each test stands up a wRPC server on a tempdir Unix socket with a
//! [`StubHandler`] configured for just the operation under test, then asserts
//! that a wRPC client invocation returns the expected values. The handler
//! boilerplate (and the `unimplemented!` fallbacks for unexercised ops) lives
//! once in [`common`].

mod common;

use cairn_protocol as bindings;
use common::{StubHandler, sample_session_info, spawn_server};

#[tokio::test]
async fn meta_version_round_trips_record_fields() {
    let stub = StubHandler::new().on_version(|_ctx| {
        bindings::exports::cairn::daemon::meta::VersionInfo {
            daemon: "cairn-test-daemon/0.1.0".to_string(),
            protocol: "cairn:daemon@0.1.0".to_string(),
        }
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let info = bindings::client::cairn::daemon::meta::version(&harness.unix_client(), ())
        .await
        .expect("version invocation");

    assert_eq!(info.daemon, "cairn-test-daemon/0.1.0");
    assert_eq!(info.protocol, "cairn:daemon@0.1.0");
}

#[tokio::test]
async fn sessions_list_all_round_trips_two_entries_with_optional_fields() {
    let stub = StubHandler::new().on_list_all(|_ctx| {
        vec![
            sample_session_info("01900000-0000-7000-8000-000000000001"),
            sample_session_info("01900000-0000-7000-8000-000000000002"),
        ]
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let result = bindings::client::cairn::daemon::sessions::list_all(&harness.unix_client(), ())
        .await
        .expect("list_all invocation");

    assert_eq!(result.len(), 2);

    // PartialEq is not generated for SessionInfo / SessionSpec by wit-bindgen-wrpc,
    // so we assert per-field to cover the full type graph: nested record, options,
    // list<string>, list<tuple<string,string>>, and scalar types.

    // --- first entry ---
    assert_eq!(result[0].id, "01900000-0000-7000-8000-000000000001");
    assert_eq!(result[0].name, Some("test".to_string()));
    assert_eq!(result[0].pid, Some(42));
    assert_eq!(result[0].cols, 80);
    assert_eq!(result[0].rows, 24);
    assert_eq!(
        result[0].attached_clients,
        vec!["client-a".to_string(), "client-b".to_string()]
    );
    assert_eq!(result[0].created_at_unix_ms, 1_000_000_000_000);
    assert!(result[0].exit.is_none());
    // nested SessionSpec
    assert_eq!(result[0].spec.name, Some("test".to_string()));
    assert_eq!(
        result[0].spec.command,
        vec!["/bin/echo".to_string(), "hi".to_string()]
    );
    assert_eq!(result[0].spec.env, vec![("FOO".to_string(), "bar".to_string())]);
    assert!(result[0].spec.env_inherit);
    assert_eq!(result[0].spec.workdir, Some("/tmp".to_string()));
    assert!(result[0].spec.tty);
    assert!(result[0].spec.stdin);
    assert!(result[0].spec.idle_timeout_secs.is_none());
    assert_eq!(result[0].spec.scrollback_lines, 1000);

    // --- second entry (same shape, different id) ---
    assert_eq!(result[1].id, "01900000-0000-7000-8000-000000000002");
    assert_eq!(result[1].name, Some("test".to_string()));
    assert_eq!(result[1].pid, Some(42));
    assert_eq!(result[1].cols, 80);
    assert_eq!(result[1].rows, 24);
    assert_eq!(
        result[1].attached_clients,
        vec!["client-a".to_string(), "client-b".to_string()]
    );
    assert_eq!(result[1].created_at_unix_ms, 1_000_000_000_000);
    assert!(result[1].exit.is_none());
    // nested SessionSpec
    assert_eq!(result[1].spec.name, Some("test".to_string()));
    assert_eq!(
        result[1].spec.command,
        vec!["/bin/echo".to_string(), "hi".to_string()]
    );
    assert_eq!(result[1].spec.env, vec![("FOO".to_string(), "bar".to_string())]);
    assert!(result[1].spec.env_inherit);
    assert_eq!(result[1].spec.workdir, Some("/tmp".to_string()));
    assert!(result[1].spec.tty);
    assert!(result[1].spec.stdin);
    assert!(result[1].spec.idle_timeout_secs.is_none());
    assert_eq!(result[1].spec.scrollback_lines, 1000);
}

#[tokio::test]
async fn meta_authenticate_round_trips_error_variant() {
    let stub = StubHandler::new().on_authenticate(|_ctx, token| {
        if token == "valid-token" {
            Ok(())
        } else {
            Err(bindings::cairn::daemon::types::Error {
                code: "auth.invalid_token".to_string(),
                message: "supplied token did not match".to_string(),
            })
        }
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    // Success path.
    let ok =
        bindings::client::cairn::daemon::meta::authenticate(&harness.unix_client(), (), "valid-token")
            .await
            .expect("authenticate invocation (ok)");
    assert!(ok.is_ok(), "expected Ok(_), got {ok:?}");

    // Failure path.
    let err =
        bindings::client::cairn::daemon::meta::authenticate(&harness.unix_client(), (), "wrong-token")
            .await
            .expect("authenticate invocation (err)");
    let err = err.expect_err("expected error variant");
    assert_eq!(err.code, "auth.invalid_token");
    assert_eq!(err.message, "supplied token did not match");
}

#[tokio::test]
async fn sessions_kill_round_trips_grace_ms() {
    // Echo the received params back through the error channel so the client can
    // assert they crossed the wire intact (exercises `signal` + `option<u32>`).
    let stub = StubHandler::new().on_kill(|_ctx, id, sig, grace_ms| {
        let named = matches!(sig, bindings::cairn::daemon::types::Signal::Named(_));
        Err(bindings::cairn::daemon::types::Error {
            code: "echo".to_string(),
            message: format!("id={id} named={named} grace={grace_ms:?}"),
        })
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let sig = bindings::cairn::daemon::types::Signal::Named(
        bindings::cairn::daemon::types::SignalName::Term,
    );
    let res = bindings::client::cairn::daemon::sessions::kill(
        &harness.unix_client(),
        (),
        "dev",
        &sig,
        Some(5000u32),
    )
    .await
    .expect("kill invocation");
    let err = res.expect_err("stub echoes via Err");
    assert_eq!(err.message, "id=dev named=true grace=Some(5000)");
}

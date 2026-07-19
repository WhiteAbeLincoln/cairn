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
use futures::StreamExt as _;

#[tokio::test]
async fn meta_version_round_trips_record_fields() {
    let stub =
        StubHandler::new().on_version(|_ctx| bindings::exports::cairn::daemon::meta::VersionInfo {
            daemon: "cairn-test-daemon/0.1.0".to_string(),
            protocol: "cairn:daemon@0.1.0".to_string(),
        });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let info = bindings::client::cairn::daemon::meta::version(&harness.unix_client(), (), None)
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

    let result =
        bindings::client::cairn::daemon::sessions::list_all(&harness.unix_client(), (), None)
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
    assert_eq!(
        result[0].spec.env,
        vec![("FOO".to_string(), "bar".to_string())]
    );
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
    assert_eq!(
        result[1].spec.env,
        vec![("FOO".to_string(), "bar".to_string())]
    );
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
    let ok = bindings::client::cairn::daemon::meta::authenticate(
        &harness.unix_client(),
        (),
        None,
        "valid-token",
    )
    .await
    .expect("authenticate invocation (ok)");
    assert!(ok.is_ok(), "expected Ok(_), got {ok:?}");

    // Failure path.
    let err = bindings::client::cairn::daemon::meta::authenticate(
        &harness.unix_client(),
        (),
        None,
        "wrong-token",
    )
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
        None,
        "dev",
        &sig,
        Some(5000u32),
    )
    .await
    .expect("kill invocation");
    let err = res.expect_err("stub echoes via Err");
    assert_eq!(err.message, "id=dev named=true grace=Some(5000)");
}

#[tokio::test]
async fn http_proxy_intercept_round_trips_streamed_actions_and_raw_request_data() {
    use bindings::client::cairn::daemon::http_proxy as client_proxy;
    use bindings::exports::cairn::daemon::http_proxy as server_proxy;

    let stub = StubHandler::new().on_proxy_intercept(|_ctx, id, mut actions| {
        Box::pin(futures::stream::once(async move {
            let valid_actions = matches!(actions.next().await.as_deref(), Some([
                server_proxy::InterceptorAction::ResponseStart(server_proxy::ResponseStart {
                    id: 77,
                    head: server_proxy::ResponseHead { status: 202, .. },
                }),
                server_proxy::InterceptorAction::ResponseBody(server_proxy::BodyChunk { id: 77, bytes }),
                server_proxy::InterceptorAction::ResponseEnd(77),
            ]) if bytes.as_ref() == b"streamed");
            if !valid_actions {
                return vec![server_proxy::InterceptorEvent::Error(
                    bindings::cairn::daemon::types::Error {
                        code: "test.invalid_actions".to_string(),
                        message: "interceptor actions changed in transit".to_string(),
                    },
                )];
            }
            vec![server_proxy::InterceptorEvent::Request(
                server_proxy::InterceptedRequest {
                    id: 77,
                    head: server_proxy::RequestHead {
                        method: "POST".to_string(),
                        uri: format!("https://api.example.com/{id}"),
                        version: "HTTP/2".to_string(),
                        headers: vec![(
                            "x-audit".to_string(),
                            bytes::Bytes::from_static(&[0, 255]),
                        )],
                    },
                    body: bytes::Bytes::from_static(b"request body"),
                },
            )]
        }))
    });
    let harness = spawn_server(stub).await.expect("spawn_server");
    let actions: std::pin::Pin<
        Box<dyn futures::Stream<Item = Vec<client_proxy::InterceptorAction>> + Send>,
    > = Box::pin(futures::stream::once(async {
        vec![
            client_proxy::InterceptorAction::ResponseStart(client_proxy::ResponseStart {
                id: 77,
                head: client_proxy::ResponseHead {
                    status: 202,
                    version: "HTTP/2".to_string(),
                    headers: vec![(
                        "content-type".to_string(),
                        bytes::Bytes::from_static(b"text/event-stream"),
                    )],
                },
            }),
            client_proxy::InterceptorAction::ResponseBody(client_proxy::BodyChunk {
                id: 77,
                bytes: bytes::Bytes::from_static(b"streamed"),
            }),
            client_proxy::InterceptorAction::ResponseEnd(77),
        ]
    }));

    let (mut events, io) = bindings::client::cairn::daemon::http_proxy::intercept(
        &harness.unix_client(),
        (),
        None,
        "audit",
        actions,
    )
    .await
    .expect("intercept invocation");
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }
    let batch = events.next().await.expect("interceptor event batch");
    let client_proxy::InterceptorEvent::Request(request) = &batch[0] else {
        panic!("expected request event");
    };
    assert_eq!(request.id, 77);
    assert_eq!(request.head.method, "POST");
    assert_eq!(request.head.uri, "https://api.example.com/audit");
    assert_eq!(request.head.version, "HTTP/2");
    assert_eq!(request.head.headers[0].1.as_ref(), &[0, 255]);
    assert_eq!(request.body.as_ref(), b"request body");
}

/// Exercises every `interceptor-action` variant on the actions stream
/// (client -> server), including `Forward` and `Fail` which the original
/// intercept test above never sent. The server-side stub validates each
/// decoded action field-by-field and reports pass/fail back through a single
/// `InterceptorEvent::Error` (mirroring the echo-via-error-channel pattern
/// above), so a decode mismatch surfaces as a clear client-side assertion
/// rather than a silent server-task panic.
#[tokio::test]
async fn http_proxy_intercept_round_trips_all_interceptor_action_variants() {
    use bindings::client::cairn::daemon::http_proxy as client_proxy;
    use bindings::exports::cairn::daemon::http_proxy as server_proxy;

    let stub = StubHandler::new().on_proxy_intercept(|_ctx, _id, mut actions| {
        Box::pin(futures::stream::once(async move {
            let batch = actions.next().await.unwrap_or_default();
            let check = || -> Result<(), String> {
                if batch.len() != 5 {
                    return Err(format!(
                        "expected 5 actions, got {}: {batch:?}",
                        batch.len()
                    ));
                }
                match &batch[0] {
                    server_proxy::InterceptorAction::Forward(id) if *id == 11 => {}
                    other => return Err(format!("action[0]: expected Forward(11), got {other:?}")),
                }
                match &batch[1] {
                    server_proxy::InterceptorAction::ResponseStart(rs)
                        if rs.id == 12
                            && rs.head.status == 200
                            && rs.head.version == "HTTP/1.1" => {}
                    other => {
                        return Err(format!("action[1]: expected ResponseStart, got {other:?}"));
                    }
                }
                match &batch[2] {
                    server_proxy::InterceptorAction::ResponseBody(bc)
                        if bc.id == 12 && bc.bytes.as_ref() == b"chunk" => {}
                    other => {
                        return Err(format!("action[2]: expected ResponseBody, got {other:?}"));
                    }
                }
                match &batch[3] {
                    server_proxy::InterceptorAction::ResponseEnd(id) if *id == 12 => {}
                    other => {
                        return Err(format!(
                            "action[3]: expected ResponseEnd(12), got {other:?}"
                        ));
                    }
                }
                match &batch[4] {
                    server_proxy::InterceptorAction::Fail((id, err))
                        if *id == 13 && err.code == "upstream.reset" => {}
                    other => return Err(format!("action[4]: expected Fail, got {other:?}")),
                }
                Ok(())
            };
            match check() {
                Ok(()) => vec![server_proxy::InterceptorEvent::Error(
                    bindings::cairn::daemon::types::Error {
                        code: "test.ok".to_string(),
                        message: "all five interceptor-action variants observed in order"
                            .to_string(),
                    },
                )],
                Err(message) => vec![server_proxy::InterceptorEvent::Error(
                    bindings::cairn::daemon::types::Error {
                        code: "test.mismatch".to_string(),
                        message,
                    },
                )],
            }
        }))
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let actions: std::pin::Pin<
        Box<dyn futures::Stream<Item = Vec<client_proxy::InterceptorAction>> + Send>,
    > = Box::pin(futures::stream::once(async {
        vec![
            client_proxy::InterceptorAction::Forward(11),
            client_proxy::InterceptorAction::ResponseStart(client_proxy::ResponseStart {
                id: 12,
                head: client_proxy::ResponseHead {
                    status: 200,
                    version: "HTTP/1.1".to_string(),
                    headers: vec![],
                },
            }),
            client_proxy::InterceptorAction::ResponseBody(client_proxy::BodyChunk {
                id: 12,
                bytes: bytes::Bytes::from_static(b"chunk"),
            }),
            client_proxy::InterceptorAction::ResponseEnd(12),
            client_proxy::InterceptorAction::Fail((
                13,
                bindings::cairn::daemon::types::Error {
                    code: "upstream.reset".to_string(),
                    message: "peer reset connection".to_string(),
                },
            )),
        ]
    }));

    let (mut events, io) = bindings::client::cairn::daemon::http_proxy::intercept(
        &harness.unix_client(),
        (),
        None,
        "sess",
        actions,
    )
    .await
    .expect("intercept invocation");
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }

    let batch = events.next().await.expect("event batch");
    let client_proxy::InterceptorEvent::Error(err) = &batch[0] else {
        panic!("expected error event, got {batch:?}");
    };
    assert_eq!(err.code, "test.ok", "stub reported: {}", err.message);
}

/// Exercises every `interceptor-event` variant on the events stream
/// (server -> client): `Request`, `Cancelled`, and `Error` (the original
/// intercept test above only ever emitted `Request`).
#[tokio::test]
async fn http_proxy_intercept_round_trips_all_interceptor_event_variants() {
    use bindings::client::cairn::daemon::http_proxy as client_proxy;
    use bindings::exports::cairn::daemon::http_proxy as server_proxy;

    let stub = StubHandler::new().on_proxy_intercept(|_ctx, _id, _actions| {
        Box::pin(futures::stream::once(async {
            vec![
                server_proxy::InterceptorEvent::Request(server_proxy::InterceptedRequest {
                    id: 21,
                    head: server_proxy::RequestHead {
                        method: "GET".to_string(),
                        uri: "https://api.example.com/data".to_string(),
                        version: "HTTP/1.1".to_string(),
                        headers: vec![],
                    },
                    body: bytes::Bytes::new(),
                }),
                server_proxy::InterceptorEvent::Cancelled(22),
                server_proxy::InterceptorEvent::Error(bindings::cairn::daemon::types::Error {
                    code: "proxy.cancelled".to_string(),
                    message: "client dropped".to_string(),
                }),
            ]
        }))
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let actions: std::pin::Pin<
        Box<dyn futures::Stream<Item = Vec<client_proxy::InterceptorAction>> + Send>,
    > = Box::pin(futures::stream::empty());

    let (mut events, io) = bindings::client::cairn::daemon::http_proxy::intercept(
        &harness.unix_client(),
        (),
        None,
        "sess",
        actions,
    )
    .await
    .expect("intercept invocation");
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }

    let batch = events.next().await.expect("event batch");
    assert_eq!(batch.len(), 3);

    match &batch[0] {
        client_proxy::InterceptorEvent::Request(req) => {
            assert_eq!(req.id, 21);
            assert_eq!(req.head.method, "GET");
            assert_eq!(req.head.uri, "https://api.example.com/data");
            assert!(req.body.is_empty());
        }
        other => panic!("batch[0]: expected Request event, got {other:?}"),
    }
    match &batch[1] {
        client_proxy::InterceptorEvent::Cancelled(id) => assert_eq!(*id, 22),
        other => panic!("batch[1]: expected Cancelled event, got {other:?}"),
    }
    match &batch[2] {
        client_proxy::InterceptorEvent::Error(err) => {
            assert_eq!(err.code, "proxy.cancelled");
            assert_eq!(err.message, "client dropped");
        }
        other => panic!("batch[2]: expected Error event, got {other:?}"),
    }
}

/// The wire design promises header values are opaque bytes (not necessarily
/// UTF-8) and that headers are carried as an ordered list rather than a map,
/// so repeated header names must survive distinctly rather than being
/// deduplicated or coerced to text. This pins both guarantees across a real
/// intercept round trip.
#[tokio::test]
async fn http_proxy_intercept_round_trips_non_utf8_and_duplicate_header_values() {
    use bindings::client::cairn::daemon::http_proxy as client_proxy;
    use bindings::exports::cairn::daemon::http_proxy as server_proxy;

    let stub = StubHandler::new().on_proxy_intercept(|_ctx, _id, _actions| {
        Box::pin(futures::stream::once(async {
            vec![server_proxy::InterceptorEvent::Request(
                server_proxy::InterceptedRequest {
                    id: 1,
                    head: server_proxy::RequestHead {
                        method: "GET".to_string(),
                        uri: "https://example.com".to_string(),
                        version: "HTTP/1.1".to_string(),
                        headers: vec![
                            (
                                "x-forwarded-for".to_string(),
                                bytes::Bytes::from_static(&[0xFF, 0xFE]),
                            ),
                            (
                                "x-forwarded-for".to_string(),
                                bytes::Bytes::from_static(b"10.0.0.1"),
                            ),
                            (
                                "x-forwarded-for".to_string(),
                                bytes::Bytes::from_static(&[0xFF, 0xFE]),
                            ),
                        ],
                    },
                    body: bytes::Bytes::new(),
                },
            )]
        }))
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let actions: std::pin::Pin<
        Box<dyn futures::Stream<Item = Vec<client_proxy::InterceptorAction>> + Send>,
    > = Box::pin(futures::stream::empty());

    let (mut events, io) = bindings::client::cairn::daemon::http_proxy::intercept(
        &harness.unix_client(),
        (),
        None,
        "sess",
        actions,
    )
    .await
    .expect("intercept invocation");
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }

    let batch = events.next().await.expect("event batch");
    let client_proxy::InterceptorEvent::Request(request) = &batch[0] else {
        panic!("expected request event, got {batch:?}");
    };

    assert_eq!(
        request.head.headers.len(),
        3,
        "duplicate names must not collapse"
    );
    assert_eq!(request.head.headers[0].0, "x-forwarded-for");
    assert_eq!(request.head.headers[0].1.as_ref(), &[0xFF, 0xFE]);
    assert_eq!(request.head.headers[1].0, "x-forwarded-for");
    assert_eq!(request.head.headers[1].1.as_ref(), b"10.0.0.1");
    assert_eq!(request.head.headers[2].0, "x-forwarded-for");
    assert_eq!(request.head.headers[2].1.as_ref(), &[0xFF, 0xFE]);
    assert!(
        std::str::from_utf8(&request.head.headers[0].1).is_err(),
        "test fixture must actually be invalid UTF-8"
    );
}

/// `watch` (a stream RPC with no client -> server stream input) delivered in
/// a single batch containing every `observation-event` variant, so a
/// reordering in the WIT enum or a copy-paste swap in the daemon's
/// wire-mapping functions would show up as a decode mismatch here.
#[tokio::test]
async fn http_proxy_watch_round_trips_all_observation_event_variants() {
    use bindings::client::cairn::daemon::http_proxy as client_proxy;
    use bindings::exports::cairn::daemon::http_proxy as server_proxy;

    let stub = StubHandler::new().on_proxy_watch(|_ctx, _id| {
        Box::pin(futures::stream::once(async {
            vec![
                server_proxy::ObservationEvent::Snapshot(vec![server_proxy::ObservedExchange {
                    id: 1,
                    request: server_proxy::RequestHead {
                        method: "GET".to_string(),
                        uri: "https://api.example.com/snap".to_string(),
                        version: "HTTP/1.1".to_string(),
                        headers: vec![],
                    },
                    request_body: server_proxy::ObservedBody {
                        bytes: bytes::Bytes::new(),
                        total_bytes: 0,
                        truncated: false,
                        complete: true,
                    },
                    response: None,
                    response_body: server_proxy::ObservedBody {
                        bytes: bytes::Bytes::new(),
                        total_bytes: 0,
                        truncated: false,
                        complete: false,
                    },
                    started_at_unix_ms: 1_700_000_000_000,
                    completed_at_unix_ms: None,
                    failure: None,
                }]),
                server_proxy::ObservationEvent::RequestStart(server_proxy::RequestStarted {
                    id: 2,
                    head: server_proxy::RequestHead {
                        method: "POST".to_string(),
                        uri: "https://api.example.com/data".to_string(),
                        version: "HTTP/1.1".to_string(),
                        headers: vec![(
                            "content-type".to_string(),
                            bytes::Bytes::from_static(b"application/json"),
                        )],
                    },
                    unix_ms: 1_700_000_000_100,
                }),
                server_proxy::ObservationEvent::RequestBody(server_proxy::BodyChunk {
                    id: 2,
                    bytes: bytes::Bytes::from_static(b"payload"),
                }),
                server_proxy::ObservationEvent::RequestEnd(server_proxy::BodyEnded {
                    id: 2,
                    total_bytes: 7,
                    truncated: false,
                }),
                server_proxy::ObservationEvent::ResponseStart(server_proxy::ResponseStarted {
                    id: 2,
                    head: server_proxy::ResponseHead {
                        status: 201,
                        version: "HTTP/1.1".to_string(),
                        headers: vec![],
                    },
                }),
                server_proxy::ObservationEvent::ResponseBody(server_proxy::BodyChunk {
                    id: 2,
                    bytes: bytes::Bytes::from_static(b"created"),
                }),
                server_proxy::ObservationEvent::ResponseEnd(server_proxy::BodyEnded {
                    id: 2,
                    total_bytes: 7,
                    truncated: false,
                }),
                server_proxy::ObservationEvent::Completed((2, 1_700_000_000_200)),
                server_proxy::ObservationEvent::Failed(server_proxy::ExchangeFailed {
                    id: 3,
                    error: bindings::cairn::daemon::types::Error {
                        code: "upstream.timeout".to_string(),
                        message: "gateway timed out".to_string(),
                    },
                }),
                server_proxy::ObservationEvent::Error(bindings::cairn::daemon::types::Error {
                    code: "proxy.internal".to_string(),
                    message: "observer channel closed".to_string(),
                }),
            ]
        }))
    });
    let harness = spawn_server(stub).await.expect("spawn_server");

    let (mut observations, io) = bindings::client::cairn::daemon::http_proxy::watch(
        &harness.unix_client(),
        (),
        None,
        "sess",
    )
    .await
    .expect("watch invocation");
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }

    let batch = observations.next().await.expect("observation batch");
    assert_eq!(batch.len(), 10);

    match &batch[0] {
        client_proxy::ObservationEvent::Snapshot(exchanges) => {
            assert_eq!(exchanges.len(), 1);
            assert_eq!(exchanges[0].id, 1);
            assert_eq!(exchanges[0].request.uri, "https://api.example.com/snap");
            assert!(exchanges[0].response.is_none());
        }
        other => panic!("batch[0]: expected Snapshot, got {other:?}"),
    }
    match &batch[1] {
        client_proxy::ObservationEvent::RequestStart(start) => {
            assert_eq!(start.id, 2);
            assert_eq!(start.head.method, "POST");
            assert_eq!(start.head.headers[0].1.as_ref(), b"application/json");
            assert_eq!(start.unix_ms, 1_700_000_000_100);
        }
        other => panic!("batch[1]: expected RequestStart, got {other:?}"),
    }
    match &batch[2] {
        client_proxy::ObservationEvent::RequestBody(chunk) => {
            assert_eq!(chunk.id, 2);
            assert_eq!(chunk.bytes.as_ref(), b"payload");
        }
        other => panic!("batch[2]: expected RequestBody, got {other:?}"),
    }
    match &batch[3] {
        client_proxy::ObservationEvent::RequestEnd(end) => {
            assert_eq!(end.id, 2);
            assert_eq!(end.total_bytes, 7);
            assert!(!end.truncated);
        }
        other => panic!("batch[3]: expected RequestEnd, got {other:?}"),
    }
    match &batch[4] {
        client_proxy::ObservationEvent::ResponseStart(start) => {
            assert_eq!(start.id, 2);
            assert_eq!(start.head.status, 201);
        }
        other => panic!("batch[4]: expected ResponseStart, got {other:?}"),
    }
    match &batch[5] {
        client_proxy::ObservationEvent::ResponseBody(chunk) => {
            assert_eq!(chunk.id, 2);
            assert_eq!(chunk.bytes.as_ref(), b"created");
        }
        other => panic!("batch[5]: expected ResponseBody, got {other:?}"),
    }
    match &batch[6] {
        client_proxy::ObservationEvent::ResponseEnd(end) => {
            assert_eq!(end.id, 2);
            assert_eq!(end.total_bytes, 7);
            assert!(!end.truncated);
        }
        other => panic!("batch[6]: expected ResponseEnd, got {other:?}"),
    }
    match &batch[7] {
        client_proxy::ObservationEvent::Completed((id, unix_ms)) => {
            assert_eq!(*id, 2);
            assert_eq!(*unix_ms, 1_700_000_000_200);
        }
        other => panic!("batch[7]: expected Completed, got {other:?}"),
    }
    match &batch[8] {
        client_proxy::ObservationEvent::Failed(failed) => {
            assert_eq!(failed.id, 3);
            assert_eq!(failed.error.code, "upstream.timeout");
            assert_eq!(failed.error.message, "gateway timed out");
        }
        other => panic!("batch[8]: expected Failed, got {other:?}"),
    }
    match &batch[9] {
        client_proxy::ObservationEvent::Error(err) => {
            assert_eq!(err.code, "proxy.internal");
            assert_eq!(err.message, "observer channel closed");
        }
        other => panic!("batch[9]: expected Error, got {other:?}"),
    }
}

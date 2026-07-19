//! Golden-file tests for cross-language wire-format compatibility.
//!
//! Each test encodes a known value using the Rust wRPC codec, writes the raw
//! bytes to `tests/golden/<name>.bin`, and writes a JSON sidecar to
//! `tests/golden/<name>.json` describing the expected decoded value (using
//! camelCase field names matching the TypeScript types).
//!
//! The TypeScript side (`cairn-web/src/lib/client/golden.test.ts`) reads those
//! `.bin` files, decodes them with the TypeScript codec, and compares against
//! the `.json` sidecar. If a `.bin` file does not yet exist the TS test is
//! skipped gracefully.
//!
//! Re-running this test regenerates the golden files (they are idempotent for
//! stable inputs). Commit the generated `.bin`/`.json` files together with any
//! codec changes.

use std::path::PathBuf;
use std::sync::LazyLock;

use bytes::BytesMut;
use cairn_protocol::cairn::daemon::types::{
    Error, ExitStatus, HttpProxySpec, HttpRoute, SessionSpec,
};
use cairn_protocol::exports::cairn::daemon::http_proxy::{
    InterceptorAction, ObservationEvent, ObservedBody, ObservedExchange, RequestHead,
    RequestStarted, ResponseHead,
};
use cairn_protocol::exports::cairn::daemon::meta::VersionInfo;

// ── Concrete writer type used to satisfy the Encode<W> bound ────────────────
//
// The generated `Encoder<W>` struct holds W in phantom data only; the
// `encode` method writes directly into a `BytesMut` and never touches W.
// `wrpc_transport::frame::Outgoing` implements `wrpc_transport::Index<Self>`
// and satisfies all required bounds, so we use it as the phantom type.
type PhantomWriter = wrpc_transport::frame::Outgoing;

// ── Helpers ──────────────────────────────────────────────────────────────────

static GOLDEN_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden"));

/// Encode a value using its generated `tokio_util::codec::Encoder` impl.
///
/// The encoder is `Default`-constructed; `encode` is called synchronously into
/// a `BytesMut`. No async writer is involved — the phantom `W` type is only
/// present to satisfy the `Encode<W>` bound on the generated impl.
fn encode_to_bytes<T, E>(value: T) -> Vec<u8>
where
    E: tokio_util::codec::Encoder<T, Error = std::io::Error> + Default,
{
    let mut buf = BytesMut::new();
    let mut enc = E::default();
    enc.encode(value, &mut buf).expect("encoding must not fail");
    buf.to_vec()
}

/// Write `<name>.bin` and `<name>.json` into the golden directory.
fn write_golden(name: &str, bytes: &[u8], json: &str) {
    let dir = &*GOLDEN_DIR;
    std::fs::create_dir_all(dir).expect("create golden dir");

    let bin_path = dir.join(format!("{name}.bin"));
    let json_path = dir.join(format!("{name}.json"));

    std::fs::write(&bin_path, bytes)
        .unwrap_or_else(|e| panic!("write {}: {e}", bin_path.display()));
    std::fs::write(&json_path, json)
        .unwrap_or_else(|e| panic!("write {}: {e}", json_path.display()));
}

// ── Concrete Encoder type aliases ─────────────────────────────────────────────
//
// The `Encode<W>` impls name their `Encoder` from a private sub-module.
// These type aliases surface them without exposing the module path.

type VersionInfoEncoder = <VersionInfo as wrpc_transport::Encode<PhantomWriter>>::Encoder;
type ErrorEncoder = <Error as wrpc_transport::Encode<PhantomWriter>>::Encoder;
type ExitStatusEncoder = <ExitStatus as wrpc_transport::Encode<PhantomWriter>>::Encoder;
type SessionSpecEncoder = <SessionSpec as wrpc_transport::Encode<PhantomWriter>>::Encoder;
type ObservedExchangeEncoder = <ObservedExchange as wrpc_transport::Encode<PhantomWriter>>::Encoder;
type InterceptorActionEncoder =
    <InterceptorAction as wrpc_transport::Encode<PhantomWriter>>::Encoder;
type ObservationEventEncoder = <ObservationEvent as wrpc_transport::Encode<PhantomWriter>>::Encoder;

// ── Tests ────────────────────────────────────────────────────────────────────

/// `version-info` record: two strings, no optional fields.
#[test]
fn golden_version_info() {
    let value = VersionInfo {
        daemon: "cairn-daemon/0.1.0".to_string(),
        protocol: "cairn:daemon@0.1.0".to_string(),
    };
    let bytes = encode_to_bytes::<VersionInfo, VersionInfoEncoder>(value);
    let json = r#"{"daemon":"cairn-daemon/0.1.0","protocol":"cairn:daemon@0.1.0"}"#;
    write_golden("version-info", &bytes, json);
}

/// `error` record: code + message strings.
/// WIT name is `error`; TypeScript type is `CairnError`.
#[test]
fn golden_cairn_error() {
    let value = Error {
        code: "session.not_found".to_string(),
        message: "no session with that id".to_string(),
    };
    let bytes = encode_to_bytes::<Error, ErrorEncoder>(value);
    let json = r#"{"code":"session.not_found","message":"no session with that id"}"#;
    write_golden("cairn-error", &bytes, json);
}

/// `exit-status` with code=0 and all optional fields absent.
#[test]
fn golden_exit_status_clean() {
    let value = ExitStatus {
        code: Some(0),
        signal: None,
        unix_ms: 1_718_300_000_000,
        reason: None,
    };
    let bytes = encode_to_bytes::<ExitStatus, ExitStatusEncoder>(value);
    let json = r#"{"code":0,"signal":null,"unixMs":1718300000000,"reason":null}"#;
    write_golden("exit-status-clean", &bytes, json);
}

/// `exit-status` with a signal and reason — exercises the `Some` branches.
#[test]
fn golden_exit_status_killed() {
    let value = ExitStatus {
        code: None,
        signal: Some(9),
        unix_ms: 1_718_300_001_234,
        reason: Some("killed by user".to_string()),
    };
    let bytes = encode_to_bytes::<ExitStatus, ExitStatusEncoder>(value);
    let json = r#"{"code":null,"signal":9,"unixMs":1718300001234,"reason":"killed by user"}"#;
    write_golden("exit-status-killed", &bytes, json);
}

/// `session-spec` record: name option, command list, env list of tuples,
/// bool fields, workdir option, idle-timeout option, scrollback-lines u32,
/// and HTTP proxy option.
#[test]
fn golden_session_spec() {
    let value = SessionSpec {
        name: Some("my-session".to_string()),
        command: vec![
            "bash".to_string(),
            "-c".to_string(),
            "echo hello".to_string(),
        ],
        env: vec![
            ("FOO".to_string(), "bar".to_string()),
            ("BAZ".to_string(), "qux".to_string()),
        ],
        env_inherit: true,
        workdir: Some("/home/user/project".to_string()),
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 10_000,
        http_proxy: Some(HttpProxySpec {
            routes: vec![HttpRoute {
                methods: vec!["POST".to_string()],
                host: Some("api.example.com".to_string()),
                path_prefix: Some("/v1/audit".to_string()),
            }],
        }),
    };
    let bytes = encode_to_bytes::<SessionSpec, SessionSpecEncoder>(value);
    let json = r#"{"name":"my-session","command":["bash","-c","echo hello"],"env":[["FOO","bar"],["BAZ","qux"]],"envInherit":true,"workdir":"/home/user/project","tty":true,"stdin":true,"idleTimeoutSecs":null,"scrollbackLines":10000,"httpProxy":{"routes":[{"methods":["POST"],"host":"api.example.com","pathPrefix":"/v1/audit"}]}}"#;
    write_golden("session-spec", &bytes, json);
}

// ── http-proxy interface ────────────────────────────────────────────────────
//
// `header` (`tuple<string, list<u8>>`) values and body chunks are opaque
// bytes, not necessarily UTF-8 text. No TS decoder for these types exists yet
// (unlike the `types` fixtures above), so there is no established JSON
// convention to match; sidecars below represent a `list<u8>` field as a plain
// JSON array of byte values (0-255) — the most direct, unambiguous encoding
// of raw bytes available in JSON. Variant payloads use the `{"tag","val"}`
// shape the TS side already uses for `types` variants (see
// `cairn-web/src/lib/protocol/types.ts`), with case names camelCased the same
// way multi-word field names are camelCased elsewhere in this file.

/// `observed-exchange` record: nested `request-head` with duplicate header
/// names and a non-UTF-8 header value, an empty request body, a present
/// `response`, a truncated response body, and a present `failure` — the
/// fields the interceptor/observer design most depends on getting right.
#[test]
fn golden_observed_exchange() {
    let value = ObservedExchange {
        id: 42,
        request: RequestHead {
            method: "GET".to_string(),
            uri: "https://a/x".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![
                ("h".to_string(), bytes::Bytes::from_static(&[0, 255])),
                ("h".to_string(), bytes::Bytes::from_static(b"ok")),
            ],
        },
        request_body: ObservedBody {
            bytes: bytes::Bytes::new(),
            total_bytes: 0,
            truncated: false,
            complete: true,
        },
        response: Some(ResponseHead {
            status: 404,
            version: "HTTP/1.1".to_string(),
            headers: vec![],
        }),
        response_body: ObservedBody {
            bytes: bytes::Bytes::from_static(b"no"),
            total_bytes: 2,
            truncated: true,
            complete: false,
        },
        started_at_unix_ms: 1_700_000_000_000,
        completed_at_unix_ms: Some(1_700_000_000_050),
        failure: Some(Error {
            code: "upstream.timeout".to_string(),
            message: "gateway timed out".to_string(),
        }),
    };
    let bytes = encode_to_bytes::<ObservedExchange, ObservedExchangeEncoder>(value);
    let json = r#"{"id":42,"request":{"method":"GET","uri":"https://a/x","version":"HTTP/1.1","headers":[["h",[0,255]],["h",[111,107]]]},"requestBody":{"bytes":[],"totalBytes":0,"truncated":false,"complete":true},"response":{"status":404,"version":"HTTP/1.1","headers":[]},"responseBody":{"bytes":[110,111],"totalBytes":2,"truncated":true,"complete":false},"startedAtUnixMs":1700000000000,"completedAtUnixMs":1700000000050,"failure":{"code":"upstream.timeout","message":"gateway timed out"}}"#;
    write_golden("observed-exchange", &bytes, json);
}

/// `interceptor-action::fail` — the variant missing from `round_trip.rs`'s
/// coverage until this PR. Non-trivial payload: `tuple<exchange-id, error>`.
#[test]
fn golden_interceptor_action_fail() {
    let value = InterceptorAction::Fail((
        99,
        Error {
            code: "proxy.upstream_error".to_string(),
            message: "connection reset".to_string(),
        },
    ));
    let bytes = encode_to_bytes::<InterceptorAction, InterceptorActionEncoder>(value);
    let json =
        r#"{"tag":"fail","val":[99,{"code":"proxy.upstream_error","message":"connection reset"}]}"#;
    write_golden("interceptor-action-fail", &bytes, json);
}

/// `observation-event::request-start` — non-trivial payload: a
/// `request-started` record nesting a `request-head` (with a header) plus a
/// `unix-ms` timestamp.
#[test]
fn golden_observation_event_request_start() {
    let value = ObservationEvent::RequestStart(RequestStarted {
        id: 5,
        head: RequestHead {
            method: "POST".to_string(),
            uri: "https://api.example.com/v1/items".to_string(),
            version: "HTTP/1.1".to_string(),
            headers: vec![(
                "content-type".to_string(),
                bytes::Bytes::from_static(b"application/json"),
            )],
        },
        unix_ms: 1_700_000_000_777,
    });
    let bytes = encode_to_bytes::<ObservationEvent, ObservationEventEncoder>(value);
    let json = r#"{"tag":"requestStart","val":{"id":5,"head":{"method":"POST","uri":"https://api.example.com/v1/items","version":"HTTP/1.1","headers":[["content-type",[97,112,112,108,105,99,97,116,105,111,110,47,106,115,111,110]]]},"unixMs":1700000000777}}"#;
    write_golden("observation-event-request-start", &bytes, json);
}

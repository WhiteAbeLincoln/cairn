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
use cairn_protocol::cairn::daemon::types::{Error, ExitStatus, SessionSpec};
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
/// bool fields, workdir option, idle-timeout option, scrollback-lines u32.
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
    };
    let bytes = encode_to_bytes::<SessionSpec, SessionSpecEncoder>(value);
    let json = r#"{"name":"my-session","command":["bash","-c","echo hello"],"env":[["FOO","bar"],["BAZ","qux"]],"envInherit":true,"workdir":"/home/user/project","tty":true,"stdin":true,"idleTimeoutSecs":null,"scrollbackLines":10000}"#;
    write_golden("session-spec", &bytes, json);
}

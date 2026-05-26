# cairn-protocol Crate Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Establish the `cairn-protocol` crate that holds the `cairn:daemon@0.1.0` WIT schema and emits Rust type bindings via `wit-bindgen-wrpc`, with an in-process wRPC round-trip test fixture proving the codegen actually serves and consumes representative messages.

**Architecture:** New workspace crate at `crates/cairn-protocol/`. A single WIT package (`cairn:daemon@0.1.0`) covers the `types`, `sessions`, and `meta` interfaces from `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md`. The `wit_bindgen_wrpc::generate!{}` procedural macro produces Rust traits and client functions at compile time. The crate's `tests/round_trip.rs` spawns a stub wRPC server on a tempdir Unix domain socket, connects with the wRPC unix client, and asserts that representative operations round-trip values cleanly. The stub server lives in test code only — production daemon implementation is the next plan.

**Tech Stack:** Rust 2024 (edition matches workspace), Tokio (current-thread suffices for tests), `wit-bindgen-wrpc` 0.10 (latest published on crates.io as of 2026-05-26), `wrpc-transport` 0.28 (latest published; `net` feature for Unix sockets), `anyhow` 1, `tempfile` 3, `tokio` 1, `futures` 0.3, `cargo-nextest` for running tests.

**Out of scope for this plan (future plans):**
- The `cairn-daemon` binary, session registry, real handler implementations.
- WebTransport transport, auth tokens, signal handling, listener wiring.
- TypeScript client, `jco types` build step.
- CLI integration (wiring `cairn-client/src/cli.rs` to actually invoke daemon ops).
- WASM plugin host.
- Updates to `docs/architecture/pty-session/` documents.

**Repo conventions to follow** (from `~/.claude/CLAUDE.md`):
- Use `cargo nextest run -p cairn-protocol` for tests, not `cargo test`.
- Tests assert behavior, not the existence of types/fields/constants the compiler already guarantees.
- No `unwrap` / `expect` / `panic!` in non-test code. Test code may use them as the framework's intended failure path.
- Existing comments are not to be removed unless directed; new code follows the "comment only when WHY is non-obvious" rule.

---

## File structure

```
crates/cairn-protocol/
├── Cargo.toml
├── src/
│   └── lib.rs          # wit_bindgen_wrpc::generate!{} and re-exports
├── wit/
│   └── cairn.wit       # cairn:daemon@0.1.0 package
└── tests/
    └── round_trip.rs   # stub server + in-process round-trip assertions
```

Each file has one job: `Cargo.toml` declares deps, `lib.rs` runs the macro and re-exports the generated module, `wit/cairn.wit` is the schema source of truth, `tests/round_trip.rs` validates that the schema produces usable Rust code through the wRPC server/client path.

The workspace root `Cargo.toml` already uses `members = ["crates/*"]`, so a new crate under `crates/` is picked up automatically with no edit to the root manifest needed.

---

## Task 1: Bootstrap the `cairn-protocol` crate skeleton

**Files:**
- Create: `crates/cairn-protocol/Cargo.toml`
- Create: `crates/cairn-protocol/src/lib.rs`

- [ ] **Step 1: Write `crates/cairn-protocol/Cargo.toml`**

```toml
[package]
name = "cairn-protocol"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
authors.workspace = true
description = "WIT schema and generated bindings for the cairn daemon wire protocol"

[dependencies]
anyhow = { version = "1", default-features = false, features = ["std"] }
bytes = { version = "1", default-features = false }
futures = { version = "0.3", default-features = false }
tokio = { version = "1", default-features = false }
wit-bindgen-wrpc = { version = "0.10", default-features = false }
wrpc-transport = { version = "0.28", default-features = false, features = ["net"] }

[dev-dependencies]
tempfile = { version = "3", default-features = false }
tokio = { version = "1", default-features = false, features = ["fs", "macros", "net", "rt"] }
```

- [ ] **Step 2: Write `crates/cairn-protocol/src/lib.rs`**

```rust
//! `cairn:daemon@0.1.0` wire protocol bindings.
//!
//! WIT schema lives at `wit/cairn.wit`. This module runs the
//! `wit-bindgen-wrpc` codegen and re-exports the generated symbols.
//! See `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md`
//! for the design rationale.
```

The `lib.rs` is intentionally empty for this task — we add the codegen macro in Task 3 once the WIT file exists.

- [ ] **Step 3: Verify the workspace picks up the new crate**

Run: `cargo check -p cairn-protocol`
Expected: succeeds with no errors and no warnings.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-protocol/Cargo.toml crates/cairn-protocol/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(cairn-protocol): bootstrap crate skeleton

Empty lib.rs and Cargo.toml staked out for the WIT-schema crate.
WIT file and codegen wiring follow in subsequent commits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Write the WIT schema file

**Files:**
- Create: `crates/cairn-protocol/wit/cairn.wit`

- [ ] **Step 1: Create the `wit/` directory and write `cairn.wit`**

The schema is the one in `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md` § "WIT interface surface". Copy it verbatim:

```wit
package cairn:daemon@0.1.0;

interface types {
    type session-id = string;     // UUIDv7
    type client-id = string;

    record session-spec {
        name: option<string>,
        command: list<string>,
        env: list<tuple<string, string>>,
        env-inherit: bool,
        workdir: option<string>,
        tty: bool,
        stdin: bool,
        idle-timeout-secs: option<u64>,
        scrollback-lines: u32,
    }

    record session-info {
        id: session-id,
        name: option<string>,
        pid: option<u32>,
        cols: u16,
        rows: u16,
        attached-clients: list<client-id>,
        created-at-unix-ms: u64,
        exit: option<exit-status>,
        spec: session-spec,
    }

    record exit-status {
        code: option<s32>,
        signal: option<u8>,
        unix-ms: u64,
    }

    variant signal {
        named(signal-name),
        numbered(u8),
    }

    enum signal-name {
        hup, int, quit, ill, trap, abrt, bus, fpe, kill,
        usr1, segv, usr2, pipe, alrm, term, chld, cont,
        stop, tstp, ttin, ttou, urg, xcpu, xfsz, vtalrm,
        prof, winch, io, sys,
    }

    variant log-window {
        tail(u32),
        since-unix-ms(u64),
        all,
    }

    record attach-init {
        cols: u16,
        rows: u16,
        no-stdin: bool,
    }

    variant client-event {
        input(list<u8>),
        resize(tuple<u16, u16>),
        detach,
    }

    variant server-event {
        snapshot(list<u8>),
        output(list<u8>),
        exited(exit-status),
        error(error),
    }

    record error {
        code: string,
        message: string,
    }
}

interface sessions {
    use types.{
        session-id, client-id, session-spec, session-info, signal,
        log-window, attach-init, client-event, server-event,
        exit-status, error,
    };

    list-all: func() -> list<session-info>;
    inspect:  func(id: session-id) -> result<session-info, error>;

    create:  func(spec: session-spec) -> result<session-info, error>;
    rename:  func(id: session-id, new-name: string) -> result<_, error>;
    restart: func(id: session-id, force: bool) -> result<_, error>;
    kill:    func(id: session-id, sig: signal) -> result<_, error>;
    kick:    func(id: session-id, client: option<client-id>) -> result<_, error>;

    wait:    func(id: session-id) -> future<exit-status>;

    logs:    func(id: session-id, window: log-window, follow: bool)
             -> stream<list<u8>>;

    attach:  func(id: session-id, init: attach-init,
                  events: stream<client-event>) -> stream<server-event>;

    send:    func(id: session-id, chunks: stream<list<u8>>)
             -> result<_, error>;
}

interface meta {
    use types.{error};

    record version-info {
        daemon: string,
        protocol: string,    // "cairn:daemon@0.1.0"
    }

    authenticate: func(token: string) -> result<_, error>;

    whoami:  func() -> result<string, error>;
    version: func() -> version-info;
}

world daemon {
    export sessions;
    export meta;
}
```

Notes vs the spec sketch:
- The spec used `signal: signal` as a parameter name inside the `kill` signature; `signal` is a reserved-feeling name in many languages and clashes with the type. Renamed the parameter to `sig`.
- `meta` interface now imports `error` from `types`, since `authenticate` and `whoami` both reference it.

- [ ] **Step 2: Verify the file parses (no compile-time check yet; we add codegen in Task 3)**

Run: `ls -la crates/cairn-protocol/wit/`
Expected: shows `cairn.wit` of non-trivial size.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-protocol/wit/cairn.wit
git commit -m "$(cat <<'EOF'
feat(cairn-protocol): add cairn:daemon@0.1.0 WIT schema

Schema matches the design in docs/superpowers/specs/2026-05-26-daemon-protocol-design.md
modulo two cleanups: kill's parameter renamed signal -> sig to avoid
clashing with the type name, and meta now imports `error` since
authenticate/whoami both return result<_, error>.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Wire `wit-bindgen-wrpc` codegen

**Files:**
- Modify: `crates/cairn-protocol/src/lib.rs`

- [ ] **Step 1: Add the codegen macro invocation to `src/lib.rs`**

Replace the contents of `crates/cairn-protocol/src/lib.rs` with:

```rust
//! `cairn:daemon@0.1.0` wire protocol bindings.
//!
//! WIT schema lives at `wit/cairn.wit`. The `wit-bindgen-wrpc` macro
//! below produces Rust trait definitions for server-side `Handler`
//! impls plus free functions for client-side invocations. See
//! `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md`
//! for the design rationale.

wit_bindgen_wrpc::generate!({
    world: "daemon",
    with: {
        "cairn:daemon/types@0.1.0": generate,
        "cairn:daemon/sessions@0.1.0": generate,
        "cairn:daemon/meta@0.1.0": generate,
    },
});
```

The path strings match the `wrpc-examples:hello/handler` pattern from `wrpc/examples/rust/hello-unix-server/src/main.rs:18-21`, adapted to the `cairn:daemon@0.1.0` package version we declared in WIT.

- [ ] **Step 2: Run `cargo check` to confirm codegen succeeds**

Run: `cargo check -p cairn-protocol`
Expected: succeeds. The macro reads `wit/cairn.wit` and emits Rust into the build output.

If the build fails because the macro can't locate the WIT directory, check that `wit/cairn.wit` exists relative to the crate root — `wit-bindgen-wrpc` looks for `wit/` by default.

If the build fails on `future<T>` codegen, fall back to `stream<exit-status>` for `wait` and update both the WIT and the spec doc; record the change in a follow-up commit. The user has confirmed wasip3 features are acceptable, but if the macro's preview3 support has a regression, the simplest workaround is using `stream`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-protocol/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(cairn-protocol): wire wit-bindgen-wrpc codegen for the daemon world

generate! macro reads wit/cairn.wit and produces Rust traits +
client functions for cairn:daemon@0.1.0. Verified by cargo check.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add the round-trip test harness

The smoke test pattern: tempdir socket → spawn wRPC server task with stub `Handler` impls → invoke from a wRPC unix client → assert returned value matches what the stub produced. The harness lives in `tests/round_trip.rs` so the stub `Handler` impls are out of the library binary entirely.

**Files:**
- Create: `crates/cairn-protocol/tests/round_trip.rs`

- [ ] **Step 1: Write the harness scaffolding**

This first revision spawns a server but registers no operations yet; that's fine — we add operations one at a time across the remaining tasks. The pattern mirrors `wrpc/examples/rust/hello-unix-server/src/main.rs:44-105`.

```rust
//! End-to-end round-trip tests for the `cairn-protocol` bindings.
//!
//! Each test sets up a wRPC server on a tempdir Unix socket with stub
//! `Handler` implementations and asserts that a wRPC client can invoke
//! the relevant operations and receive the expected values back.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use futures::stream::StreamExt as _;
use futures::stream::select_all;
use tempfile::TempDir;

use cairn_protocol as bindings;

/// Resources held by an active harness. The temp dir is preserved
/// here so the socket path stays valid for the lifetime of the test;
/// dropping the harness shuts the server down.
struct Harness {
    socket_path: PathBuf,
    _tmp: TempDir,
    server_task: tokio::task::JoinHandle<()>,
    accept_task: tokio::task::JoinHandle<()>,
}

impl Harness {
    fn unix_client(&self) -> wrpc_transport::unix::Client {
        wrpc_transport::unix::Client::from(self.socket_path.as_path())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server_task.abort();
        self.accept_task.abort();
    }
}

/// Spawn a wRPC unix-domain-socket server hosting the given `handler`.
///
/// `handler` must implement the `Handler` traits for every interface
/// in the `cairn:daemon@0.1.0` world that any test calls. The harness
/// returns once the server is bound and accepting connections.
async fn spawn_server<H>(handler: H) -> anyhow::Result<Harness>
where
    H: bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr>
        + bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr>
        + Clone
        + Send
        + Sync
        + 'static,
{
    let tmp = TempDir::new().context("failed to create temp dir")?;
    let socket_path = tmp.path().join("cairn-protocol-test.sock");

    let listener = tokio::net::UnixListener::bind(&socket_path)
        .with_context(|| format!("failed to bind on {}", socket_path.display()))?;

    let srv = Arc::new(wrpc_transport::Server::default());

    let accept_task = tokio::spawn({
        let srv = Arc::clone(&srv);
        async move {
            loop {
                if srv.accept(&listener).await.is_err() {
                    // Listener errors are swallowed — tests drive the
                    // server to shutdown via Harness::drop, which
                    // aborts this task.
                    break;
                }
            }
        }
    });

    let invocations = bindings::serve(srv.as_ref(), handler)
        .await
        .context("bindings::serve failed")?;

    let server_task = tokio::spawn(async move {
        let mut invocations = select_all(
            invocations
                .into_iter()
                .map(|(instance, name, invocations)| {
                    invocations.map(move |res| (instance, name, res))
                }),
        );
        while let Some((_instance, _name, res)) = invocations.next().await {
            if let Ok(fut) = res {
                tokio::spawn(fut);
            }
        }
    });

    Ok(Harness {
        socket_path,
        _tmp: tmp,
        server_task,
        accept_task,
    })
}
```

- [ ] **Step 2: Run `cargo check --tests -p cairn-protocol` to confirm scaffolding compiles**

Run: `cargo check --tests -p cairn-protocol`
Expected: succeeds. (No tests yet; this verifies the harness file alone compiles. The trait bounds on `spawn_server` only become checkable once a concrete `handler` is supplied — that happens in Task 5.)

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-protocol/tests/round_trip.rs
git commit -m "$(cat <<'EOF'
test(cairn-protocol): add round-trip Harness scaffolding

Spawns a wRPC server on a tempdir Unix socket. Stub Handler impls
follow in subsequent commits; the harness alone is the scaffolding
shared across all wire-protocol tests.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: First round-trip test — `meta.version`

The simplest unary operation: no arguments, returns a record with two strings. This exercises the basic invocation path, record encoding/decoding, and string round-trips.

**Files:**
- Modify: `crates/cairn-protocol/tests/round_trip.rs`

- [ ] **Step 1: Write the test BEFORE the stub impl exists, so the trait-bound failure is the first signal**

Append to `tests/round_trip.rs`:

```rust
#[tokio::test]
async fn meta_version_round_trips_record_fields() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn authenticate(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _token: String,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn whoami(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Result<String, bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }

        async fn version(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<bindings::exports::cairn::daemon::meta::VersionInfo> {
            Ok(bindings::exports::cairn::daemon::meta::VersionInfo {
                daemon: "cairn-test-daemon/0.1.0".to_string(),
                protocol: "cairn:daemon@0.1.0".to_string(),
            })
        }
    }

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
        // The sessions interface has 11 methods. They must all be
        // present to satisfy the trait bound on `spawn_server`, but
        // only `version` is exercised by this test — every sessions
        // method below panics if invoked.
        //
        // Method list (from wit/cairn.wit):
        //   list, inspect, create, rename, restart, kill, kick,
        //   wait, logs, attach, send
        //
        // Iterative-discovery workflow: write the empty impl block,
        // run `cargo check --tests -p cairn-protocol`, copy each
        // missing-method signature from the compiler output into
        // the block with `unimplemented!("not exercised by this test")`
        // as the body. Repeat until cargo check passes.
        //
        // For the streaming methods (`wait` returns `future<T>`;
        // `logs`, `attach`, `send` involve `stream<T>` in params
        // and/or returns) the generated signatures wrap values in
        // `Pin<Box<dyn Stream<Item = ...> + Send>>` and similar.
        // The pattern is documented in
        // /Users/abe/Projects/wrpc/examples/rust/streams-quic-server/src/main.rs:43-50.
        async fn list(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Vec<bindings::cairn::daemon::types::SessionInfo>> {
            unimplemented!("not exercised by this test")
        }
        // ... the other 10 methods filled in via the iterative workflow above.
    }

    let harness = spawn_server(Stub).await.expect("spawn_server");

    let info = bindings::client::cairn::daemon::meta::version(&harness.unix_client(), ())
        .await
        .expect("version invocation");

    assert_eq!(info.daemon, "cairn-test-daemon/0.1.0");
    assert_eq!(info.protocol, "cairn:daemon@0.1.0");
}
```

The test asserts behavior (the value passed by the stub equals the value received by the client) — not "the type exists" or "a field is named X". This is the key thing the harness is proving: a value produced on one side of the wire arrives unchanged on the other.

The exact module paths under `bindings::` depend on how `wit-bindgen-wrpc` lays out generated code. The pattern from `wrpc/examples/rust/hello-unix-server/src/main.rs:30` is:

```rust
bindings::exports::wrpc_examples::hello::handler::Handler<Ctx>
```

So for us the trait path is `bindings::exports::cairn::daemon::meta::Handler<Ctx>` and the type path for `VersionInfo` is `bindings::exports::cairn::daemon::meta::VersionInfo`. Verify by running `cargo doc -p cairn-protocol --open` if the paths are off.

The client-side invocation pattern from `wrpc/examples/rust/hello-unix-client/src/main.rs:24`:

```rust
bindings::wrpc_examples::hello::handler::hello(&wrpc, ()).await
```

So for us it's `bindings::client::cairn::daemon::meta::version(&client, ())`. Note: client-side path is `client::cairn::daemon::meta` (a second `generate!` invocation in `src/lib.rs` against a `daemon-client` world that *imports* the interfaces — needed because the macro only emits client-invocation functions for imports); server-side path is `exports::cairn::daemon::meta` (from the original `daemon` world that *exports*).

- [ ] **Step 2: Run the test, expect a compile error if the path/trait names are off; fix paths until it compiles AND the test panics on `unimplemented!()` from `sessions::Handler` methods the wRPC plumbing somehow exercises (it shouldn't, but worth knowing)**

Run: `cargo nextest run -p cairn-protocol meta_version_round_trips_record_fields --nocapture`
Expected: PASS — the stub returns the canned `VersionInfo`; the client receives identical strings.

If you get a compile error about the trait path, run `cargo doc -p cairn-protocol --no-deps --open` and use the actual generated path. The structure follows the WIT package's namespace (`cairn::daemon`) plus the interface name (`meta`, `sessions`, `types`).

If you get `unimplemented!()` panics from any `sessions::Handler` method, the wRPC server is registering all interfaces in the world for serving — that's expected. The test should not trigger them; if it does, investigate the invocation routing.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-protocol/tests/round_trip.rs
git commit -m "$(cat <<'EOF'
test(cairn-protocol): assert meta.version round-trips record fields

First behavioral round-trip through the wRPC server/client path.
A stub handler returns a known VersionInfo; the test asserts the
client receives the same strings on both fields. Validates basic
unary invocation, record codec, and string transport over UDS.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Round-trip test — `sessions.list-all` with a non-empty result

This exercises a more interesting type graph: `Vec<SessionInfo>`, nested `Option<...>` and `Option<ExitStatus>`, plus a populated `SessionSpec`. Catches encoder/decoder bugs on optional and nested-record paths.

**Files:**
- Modify: `crates/cairn-protocol/tests/round_trip.rs`

- [ ] **Step 1: Add a helper that constructs a representative `SessionInfo`**

Append to `tests/round_trip.rs`:

```rust
fn sample_session_info(id: &str) -> bindings::cairn::daemon::types::SessionInfo {
    use bindings::cairn::daemon::types::{SessionInfo, SessionSpec};

    SessionInfo {
        id: id.to_string(),
        name: Some("test".to_string()),
        pid: Some(42),
        cols: 80,
        rows: 24,
        attached_clients: vec!["client-a".to_string(), "client-b".to_string()],
        created_at_unix_ms: 1_000_000_000_000,
        exit: None,
        spec: SessionSpec {
            name: Some("test".to_string()),
            command: vec!["/bin/echo".to_string(), "hi".to_string()],
            env: vec![("FOO".to_string(), "bar".to_string())],
            env_inherit: true,
            workdir: Some("/tmp".to_string()),
            tty: true,
            stdin: true,
            idle_timeout_secs: None,
            scrollback_lines: 1000,
        },
    }
}
```

The helper fixes the values once; both the stub and the test assertion read from the same source. If the WIT codegen renames a field (e.g. kebab-case `created-at-unix-ms` to snake_case `created_at_unix_ms`), the compiler points at the helper and the test passes once it's fixed.

- [ ] **Step 2: Add the test**

Append:

```rust
#[tokio::test]
async fn sessions_list_round_trips_two_entries_with_optional_fields() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn list(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Vec<bindings::cairn::daemon::types::SessionInfo>> {
            Ok(vec![sample_session_info("01900000-0000-7000-8000-000000000001"),
                    sample_session_info("01900000-0000-7000-8000-000000000002")])
        }
        // The other 10 sessions methods: `unimplemented!("not exercised by this test")`
        // for each. Iterative-discovery workflow: run `cargo check --tests -p cairn-protocol`
        // and the compiler enumerates the missing method signatures. Method names
        // (from wit/cairn.wit): inspect, create, rename, restart, kill, kick, wait,
        // logs, attach, send. Streaming-method signature patterns are documented in
        // /Users/abe/Projects/wrpc/examples/rust/streams-quic-server/src/main.rs:43-50.
    }

    impl bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn authenticate(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _token: String,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }
        async fn whoami(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Result<String, bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }
        async fn version(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<bindings::exports::cairn::daemon::meta::VersionInfo> {
            unimplemented!("not exercised by this test")
        }
    }

    let harness = spawn_server(Stub).await.expect("spawn_server");

    let result = bindings::client::cairn::daemon::sessions::list_all(&harness.unix_client(), ())
        .await
        .expect("list invocation");

    assert_eq!(result.len(), 2);
    assert_eq!(result[0], sample_session_info("01900000-0000-7000-8000-000000000001"));
    assert_eq!(result[1], sample_session_info("01900000-0000-7000-8000-000000000002"));
}
```

The `assert_eq!` between two `SessionInfo` values requires the generated types to derive `PartialEq`. The wit-bindgen-wrpc macro does this by default for plain data types — verify by reading the generated code via `cargo doc` if the assertion doesn't compile.

If `PartialEq` is not derived, assert per-field equality instead:

```rust
assert_eq!(result[0].id, "01900000-0000-7000-8000-000000000001");
assert_eq!(result[0].spec.command, vec!["/bin/echo".to_string(), "hi".to_string()]);
assert_eq!(result[0].spec.env, vec![("FOO".to_string(), "bar".to_string())]);
// ...
```

- [ ] **Step 3: Run the test**

Run: `cargo nextest run -p cairn-protocol sessions_list_round_trips_two_entries_with_optional_fields --nocapture`
Expected: PASS.

- [ ] **Step 4: Run the entire test suite**

Run: `cargo nextest run -p cairn-protocol`
Expected: 2 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-protocol/tests/round_trip.rs
git commit -m "$(cat <<'EOF'
test(cairn-protocol): assert sessions.list-all round-trips nested records

Exercises Vec<SessionInfo>, nested SessionSpec, optional fields, and
list-of-tuples (env). Catches codec bugs that wouldn't surface from
the flat record in meta.version.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Round-trip test — `meta.authenticate` failure path

The `result<_, error>` return type is the protocol's standard error envelope, used by `inspect`, `create`, `rename`, `restart`, `kill`, `kick`, `send`, `whoami`, and `authenticate`. Validating it once on the simplest signature lets every other operation rely on the same codec.

**Files:**
- Modify: `crates/cairn-protocol/tests/round_trip.rs`

- [ ] **Step 1: Add the test**

Append:

```rust
#[tokio::test]
async fn meta_authenticate_round_trips_error_variant() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn authenticate(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            token: String,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            if token == "valid-token" {
                Ok(Ok(()))
            } else {
                Ok(Err(bindings::cairn::daemon::types::Error {
                    code: "auth.invalid_token".to_string(),
                    message: "supplied token did not match".to_string(),
                }))
            }
        }
        async fn whoami(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Result<String, bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }
        async fn version(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<bindings::exports::cairn::daemon::meta::VersionInfo> {
            unimplemented!("not exercised by this test")
        }
    }

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn list(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
        ) -> anyhow::Result<Vec<bindings::cairn::daemon::types::SessionInfo>> {
            unimplemented!("not exercised by this test")
        }
        // The other 10 sessions methods: `unimplemented!("not exercised by this test")`
        // for each. Iterative-discovery workflow: run `cargo check --tests -p cairn-protocol`
        // and the compiler enumerates the missing method signatures. Method names
        // (from wit/cairn.wit): inspect, create, rename, restart, kill, kick, wait,
        // logs, attach, send. Streaming-method signature patterns are documented in
        // /Users/abe/Projects/wrpc/examples/rust/streams-quic-server/src/main.rs:43-50.
    }

    let harness = spawn_server(Stub).await.expect("spawn_server");

    // Success path.
    let ok = bindings::client::cairn::daemon::meta::authenticate(
        &harness.unix_client(),
        (),
        "valid-token",
    )
    .await
    .expect("authenticate invocation (ok)");
    assert!(ok.is_ok(), "expected Ok(_), got {ok:?}");

    // Failure path.
    let err = bindings::client::cairn::daemon::meta::authenticate(
        &harness.unix_client(),
        (),
        "wrong-token",
    )
    .await
    .expect("authenticate invocation (err)");
    let err = err.expect_err("expected error variant");
    assert_eq!(err.code, "auth.invalid_token");
    assert_eq!(err.message, "supplied token did not match");
}
```

This test verifies two things: (1) `result<_, error>` decodes into Rust's `Result<(), Error>`, and (2) both arms of the result survive the round-trip with their fields intact.

- [ ] **Step 2: Run the test**

Run: `cargo nextest run -p cairn-protocol meta_authenticate_round_trips_error_variant --nocapture`
Expected: PASS.

- [ ] **Step 3: Run the entire suite**

Run: `cargo nextest run -p cairn-protocol`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-protocol/tests/round_trip.rs
git commit -m "$(cat <<'EOF'
test(cairn-protocol): assert meta.authenticate round-trips both result arms

Exercises the result<_, error> envelope shared by ~half the
sessions/meta surface. Validates that Ok and Err variants both
encode and decode with their payloads intact.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Sanity sweep — no clippy warnings, no dead deps, full test pass

A final scrub before declaring the crate stable.

- [ ] **Step 1: Run clippy**

Run: `cargo clippy -p cairn-protocol --tests -- -D warnings`
Expected: no errors. If there are warnings, fix them inline (most likely candidates: unused imports in `tests/round_trip.rs` since each test stubs a different subset, or `Drop` impl noise).

- [ ] **Step 2: Run the test suite one more time**

Run: `cargo nextest run -p cairn-protocol`
Expected: all 3 tests pass.

- [ ] **Step 3: Verify the workspace builds end-to-end**

Run: `cargo check --workspace`
Expected: succeeds. (Confirms the new crate didn't introduce a workspace-level dep conflict.)

- [ ] **Step 4: Commit any fixes**

If clippy/build adjustments were needed:

```bash
git add -A
git commit -m "$(cat <<'EOF'
chore(cairn-protocol): clippy/build cleanup

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

If nothing changed, skip the commit.

---

## Spec coverage check

Mapping spec sections to plan tasks:

| Spec section | Covered by |
|---|---|
| Summary (single WIT schema as source of truth) | Tasks 2, 3 |
| WIT interface surface (types, sessions, meta, world) | Task 2 |
| Codegen (`wit-bindgen-wrpc-rust` for daemon) | Task 3 |
| Validation that the wire round-trips | Tasks 5, 6, 7 |
| UDS transport | Tasks 4-7 (via `wrpc-transport::unix`) |
| Plugin API, TS client, Auth, Streaming semantics, WebTransport | **Future plans** — explicitly out of scope here |
| Updates to pty-session docs | **Future plan** — doc hygiene |

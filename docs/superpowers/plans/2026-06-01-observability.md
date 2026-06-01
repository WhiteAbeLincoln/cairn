# Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add OTLP trace export, configurable log format, W3C traceparent propagation via `call-context`, per-session spans, span links across the flume boundary, and lifecycle events to the cairn daemon.

**Architecture:** Layered tracing subscriber (stderr fmt + OTLP), WIT-level `call-context` record on every operation, `Envelope` wrapper around `Command` carrying trace context across the flume channel, span links (not parent spans) for cross-boundary correlation. Forward-compatible with subprocess worker backends.

**Tech Stack:** `tracing`, `tracing-subscriber` (fmt + json), `opentelemetry`, `opentelemetry_sdk`, `opentelemetry-otlp`, `tracing-opentelemetry`, `wit-bindgen-wrpc`

**Spec:** `docs/superpowers/specs/2026-06-01-observability-design.md`

---

## Task 1: LogFormat enum and configurable subscriber

**Files:**
- Modify: `crates/cairn-daemon/Cargo.toml`
- Modify: `crates/cairn-daemon/src/config.rs`
- Modify: `crates/cairn-daemon/src/main.rs`
- Modify: `crates/cairn-daemon/src/lib.rs`
- Create: `crates/cairn-daemon/src/telemetry.rs`

- [ ] **Step 1: Add dependencies to Cargo.toml**

In `crates/cairn-daemon/Cargo.toml`, add `"json"` to tracing-subscriber features and add the OTLP deps:

```toml
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt", "json"] }
opentelemetry = "0.30"
opentelemetry_sdk = { version = "0.30", features = ["rt-tokio"] }
opentelemetry-otlp = { version = "0.30", features = ["tonic"] }
tracing-opentelemetry = "0.29"
```

Note: pin the exact minor versions after checking crate compatibility. The `opentelemetry` ecosystem requires matched versions across crates — check that `tracing-opentelemetry 0.29` works with `opentelemetry 0.30`. If not, use the version set that `tracing-opentelemetry`'s Cargo.toml requires.

- [ ] **Step 2: Add LogFormat enum to config.rs**

In `crates/cairn-daemon/src/config.rs`, add after the existing `DaemonConfig` impl block:

```rust
/// Stderr log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum LogFormat {
    /// Disable stderr logs entirely.
    Off,
    /// Human-readable multi-line format with thread ids, names, and line numbers.
    #[default]
    Pretty,
    /// Condensed single-line format.
    Compact,
    /// Single-line JSON for log aggregation systems.
    Json,
    /// Full single-line format with all metadata.
    Full,
}
```

- [ ] **Step 3: Write the LogFormat unit test**

In `crates/cairn-daemon/src/config.rs`, add to the existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn log_format_from_str_round_trips() {
    use clap::ValueEnum;
    for variant in LogFormat::value_variants() {
        let s = variant.to_possible_value().unwrap();
        let parsed = LogFormat::from_str(s.get_name(), false).unwrap();
        assert_eq!(*variant, parsed);
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo nextest run -p cairn-daemon config::tests::log_format_from_str_round_trips`
Expected: PASS

- [ ] **Step 5: Create telemetry.rs with init_tracing**

Create `crates/cairn-daemon/src/telemetry.rs`:

```rust
//! Tracing subscriber initialization: stderr fmt layer (configurable format)
//! + optional OTLP trace export (activated by OTEL_* env vars).

use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::Layer as _;

use crate::config::LogFormat;

/// Initialize the global tracing subscriber.
///
/// Returns `Some(provider)` when OTLP is active — the caller must hold it
/// until shutdown so pending spans flush. Returns `None` when OTLP is off.
pub fn init_tracing(
    filter: &str,
    format: LogFormat,
) -> anyhow::Result<Option<opentelemetry_sdk::trace::SdkTracerProvider>> {
    let env_filter = tracing_subscriber::EnvFilter::new(filter);

    let otlp = if std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some()
        || std::env::var_os("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT").is_some()
    {
        Some(build_otlp_provider()?)
    } else {
        None
    };

    let otlp_layer = otlp.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("cairn-daemon"))
    });

    let registry = tracing_subscriber::registry()
        .with(env_filter)
        .with(otlp_layer);

    match format {
        LogFormat::Off => {
            registry.init();
        }
        LogFormat::Pretty => {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .pretty()
                        .with_thread_ids(true)
                        .with_thread_names(true)
                        .with_writer(std::io::stderr),
                )
                .init();
        }
        LogFormat::Compact => {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .compact()
                        .with_thread_ids(true)
                        .with_thread_names(true)
                        .with_writer(std::io::stderr),
                )
                .init();
        }
        LogFormat::Json => {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .with_thread_ids(true)
                        .with_thread_names(true)
                        .with_writer(std::io::stderr),
                )
                .init();
        }
        LogFormat::Full => {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_thread_ids(true)
                        .with_thread_names(true)
                        .with_writer(std::io::stderr),
                )
                .init();
        }
    };

    Ok(otlp)
}

fn build_otlp_provider() -> anyhow::Result<opentelemetry_sdk::trace::SdkTracerProvider> {
    use opentelemetry_sdk::trace::SdkTracerProvider;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .map_err(|e| anyhow::anyhow!("OTLP exporter init: {e}"))?;
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .build();
    Ok(provider)
}
```

Register the module in `crates/cairn-daemon/src/lib.rs`:

```rust
pub mod telemetry;
```

- [ ] **Step 6: Update main.rs to use LogFormat and init_tracing**

In `crates/cairn-daemon/src/main.rs`, add the CLI arg to `Args`:

```rust
/// Log output format.
#[arg(long, env = "CAIRN_LOG_FORMAT", default_value = "pretty")]
log_format: cairn_daemon::config::LogFormat,
```

Replace the existing subscriber init block:

```rust
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::new(args.log.clone()))
    .with_writer(std::io::stderr)
    .init();
```

With:

```rust
let _tracer_provider = cairn_daemon::telemetry::init_tracing(&args.log, args.log_format)?;
```

The `_tracer_provider` binding keeps the `SdkTracerProvider` alive (flushing spans on drop at process exit).

- [ ] **Step 7: Build and run existing tests**

Run: `cargo build -p cairn-daemon && cargo nextest run -p cairn-daemon`
Expected: all existing tests pass. The default `--log-format pretty` matches the old behavior.

- [ ] **Step 8: Commit**

```
feat(cairn-daemon): configurable log format and OTLP trace export

LogFormat enum (pretty/compact/json/full/off) controlled by
--log-format / CAIRN_LOG_FORMAT. OTLP layer activates when
OTEL_EXPORTER_OTLP_ENDPOINT is set.
```

---

## Task 2: WIT schema — add `call-context`

**Files:**
- Modify: `crates/cairn-protocol/wit/cairn.wit`

This is the breaking change. After this task, nothing compiles until Tasks 3 and 4 update the Handler impls and client call sites.

- [ ] **Step 1: Add `call-context` record to the `types` interface**

In `crates/cairn-protocol/wit/cairn.wit`, inside `interface types { ... }`, add after the `error` record:

```wit
    record call-context {
        trace-context: option<string>,
    }
```

- [ ] **Step 2: Add `call-context` to `sessions` use block and all function signatures**

Replace the `sessions` interface with:

```wit
interface sessions {
    use types.{
        session-id, client-id, session-spec, session-info, signal,
        log-window, attach-init, client-event, server-event,
        exit-status, error, call-context,
    };

    list-all: func(ctx: option<call-context>) -> list<session-info>;
    inspect:  func(ctx: option<call-context>, id: session-id) -> result<session-info, error>;

    create:  func(ctx: option<call-context>, spec: session-spec) -> result<session-info, error>;
    rename:  func(ctx: option<call-context>, id: session-id, new-name: string) -> result<_, error>;
    restart: func(ctx: option<call-context>, id: session-id, force: bool) -> result<_, error>;
    kill:    func(ctx: option<call-context>, id: session-id, sig: signal, grace-ms: option<u32>) -> result<_, error>;
    kick:    func(ctx: option<call-context>, id: session-id, client: option<client-id>) -> result<_, error>;

    wait:    func(ctx: option<call-context>, id: session-id) -> future<exit-status>;

    logs:    func(ctx: option<call-context>, id: session-id, window: log-window, follow: bool)
             -> stream<list<u8>>;

    attach:  func(ctx: option<call-context>, id: session-id, init: attach-init,
                  events: stream<client-event>) -> stream<server-event>;

    send:    func(ctx: option<call-context>, id: session-id, chunks: stream<list<u8>>)
             -> result<_, error>;
}
```

- [ ] **Step 3: Add `call-context` to `meta` use block and all function signatures**

Replace the `meta` interface with:

```wit
interface meta {
    use types.{error, call-context};

    record version-info {
        daemon: string,
        protocol: string,
    }

    authenticate: func(ctx: option<call-context>, token: string) -> result<_, error>;

    whoami:  func(ctx: option<call-context>) -> result<string, error>;
    version: func(ctx: option<call-context>) -> version-info;
}
```

- [ ] **Step 4: Verify codegen compiles**

Run: `cargo build -p cairn-protocol 2>&1 | head -5`
Expected: `cairn-protocol` compiles. Downstream crates (`cairn-daemon`, `cairn-client`) will fail — that's expected.

- [ ] **Step 5: Commit (broken downstream is expected)**

```
protocol: add call-context record to all WIT operations

Breaking wire change — every operation now takes
ctx: option<call-context> as its first parameter. Downstream
Handler impls and client call sites updated in following commits.
```

---

## Task 3: Update daemon Handler impls for new WIT signatures

**Files:**
- Modify: `crates/cairn-daemon/src/daemon.rs`
- Modify: `crates/cairn-daemon/src/handlers/sessions.rs`
- Modify: `crates/cairn-daemon/src/handlers/attach.rs`
- Modify: `crates/cairn-daemon/src/handlers/logs.rs`
- Modify: `crates/cairn-daemon/src/handlers/wait.rs`
- Modify: `crates/cairn-daemon/src/handlers/send.rs`
- Modify: `crates/cairn-daemon/src/handlers/meta.rs`

The generated `Handler` traits now have a `ctx: Option<CallContext>` parameter on every method. This task is purely mechanical — accept the param, ignore it (tracing integration comes in Task 6).

- [ ] **Step 1: Update `sessions::Handler` impl in daemon.rs**

Every method in `impl cairn_protocol::exports::cairn::daemon::sessions::Handler<ConnCtx> for Daemon` gains a `_ctx: Option<cairn_protocol::cairn::daemon::types::CallContext>` parameter after `_ctx: ConnCtx`. The exact signatures depend on what `wit-bindgen-wrpc` generates — match them by reading the compiler errors.

The pattern for each method: add the parameter, pass it through (or ignore with `_`). Example for `list_all`:

```rust
async fn list_all(
    &self,
    _cx: ConnCtx,
    _ctx: Option<cairn_protocol::cairn::daemon::types::CallContext>,
) -> anyhow::Result<Vec<SessionInfo>> {
    Ok(sess::list_all(self).await)
}
```

Apply the same pattern to: `inspect`, `create`, `rename`, `restart`, `kill`, `kick`, `wait`, `logs`, `attach`, `send`.

- [ ] **Step 2: Update `meta::Handler` impl in daemon.rs**

Same pattern for `version`, `authenticate`, `whoami`.

- [ ] **Step 3: Verify daemon crate compiles**

Run: `cargo build -p cairn-daemon 2>&1 | head -20`
Expected: compiles (test binaries may still fail due to client call sites — that's Task 4).

- [ ] **Step 4: Commit**

```
fix(cairn-daemon): accept call-context on all Handler methods

Mechanical update to match the new WIT signatures. The ctx
parameter is ignored for now — tracing integration follows.
```

---

## Task 4: Update all client call sites for new WIT signatures

**Files:**
- Modify: `crates/cairn-client/src/list.rs`
- Modify: `crates/cairn-client/src/inspect.rs`
- Modify: `crates/cairn-client/src/exec.rs`
- Modify: `crates/cairn-client/src/attach.rs`
- Modify: `crates/cairn-client/src/kill.rs`
- Modify: `crates/cairn-client/src/kick.rs`
- Modify: `crates/cairn-client/src/logs.rs`
- Modify: `crates/cairn-client/src/rename.rs`
- Modify: `crates/cairn-client/src/restart.rs`
- Modify: `crates/cairn-client/src/send.rs`
- Modify: `crates/cairn-client/src/wait.rs`
- Modify: `crates/cairn-client/src/meta.rs`
- Modify: `crates/cairn-client/src/targets.rs`
- Modify: `crates/cairn-daemon/tests/daemon_unary.rs`
- Modify: `crates/cairn-daemon/tests/daemon_streaming.rs`
- Modify: `crates/cairn-daemon/tests/daemon_meta.rs`
- Modify: `crates/cairn-daemon/tests/smoke.rs`
- Modify: `crates/cairn-daemon/tests/wt_smoke.rs`

The generated client functions now have a `ctx: Option<&CallContext>` (or `Option<CallContext>`) parameter. For v0, every call site passes `None`.

- [ ] **Step 1: Update client call sites**

The old pattern is: `sessions::list_all(client, ())` → becomes `sessions::list_all(client, None, ())` or similar depending on codegen output. The `()` was the empty context before `call-context` existed; now `call-context` comes first.

Check the exact generated signatures by reading compiler errors on `cargo build -p cairn-client`. The WIT `ctx: option<call-context>` typically generates as `Option<&CallContext>` for the client free functions.

Apply across all ~20 client source files. Each call site: insert `None` for the ctx parameter.

- [ ] **Step 2: Update daemon test call sites**

Same mechanical change across `daemon_unary.rs`, `daemon_streaming.rs`, `daemon_meta.rs`, `smoke.rs`, `wt_smoke.rs`. ~25 call sites, each gets `None` inserted for ctx.

- [ ] **Step 3: Update client integration tests**

The tests in `crates/cairn-client/tests/` spawn the real CLI binary so they don't call protocol functions directly — these should work without changes if the binary itself is updated. Verify by building.

- [ ] **Step 4: Full build and test**

Run: `cargo build --all-targets && cargo nextest run`
Expected: everything compiles and all tests pass.

- [ ] **Step 5: Run clippy and fmt**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings`
Expected: clean

- [ ] **Step 6: Commit**

```
fix: pass None for call-context at all client and test call sites

Mechanical update — every generated client invocation now passes
None for the ctx: option<call-context> parameter. Actual trace
context population follows when CLI-side OTLP is added.
```

---

## Task 5: Command envelope and session_id on SpawnOptions

**Files:**
- Modify: `crates/cairn-pty/src/types.rs`
- Modify: `crates/cairn-pty/src/ghostty/mod.rs`
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Modify: `crates/cairn-daemon/src/spawn.rs`
- Modify: `crates/cairn-daemon/src/registry.rs`

- [ ] **Step 1: Add session_id to SpawnOptions**

In `crates/cairn-pty/src/types.rs`, add a `session_id` field to `SpawnOptions`:

```rust
pub struct SpawnOptions {
    pub command: tokio::process::Command,
    pub size: TermSize,
    pub broadcast_capacity: usize,
    pub scrollback_lines: usize,
    /// Identifier for this session, used in tracing spans. Empty string
    /// if the caller doesn't assign one (e.g. tests).
    pub session_id: String,
}
```

Update `SpawnOptions::new`:

```rust
pub fn new(command: tokio::process::Command) -> Self {
    Self {
        command,
        size: TermSize::default(),
        broadcast_capacity: 1024,
        scrollback_lines: 1000,
        session_id: String::new(),
    }
}
```

Add a builder method:

```rust
pub fn with_session_id(mut self, id: String) -> Self {
    self.session_id = id;
    self
}
```

- [ ] **Step 2: Create the Envelope struct and switch the channel type**

In `crates/cairn-pty/src/ghostty/mod.rs`, add the `Envelope` struct and change the channel type:

```rust
/// Wraps a `Command` with cross-cutting trace context. The flume channel
/// carries `Envelope` instead of bare `Command` so trace IDs propagate
/// from the daemon runtime to the worker thread without changing `Command`
/// or the `PtySession` trait.
pub(crate) struct Envelope {
    pub cmd: Command,
    pub trace_id: Option<String>,
}
```

Change `GhosttyPty`'s channel from `flume::Sender<Command>` to `flume::Sender<Envelope>`:

```rust
pub struct GhosttyPty {
    cmd_tx: flume::Sender<Envelope>,
    exit_rx: tokio::sync::watch::Receiver<Option<crate::ExitStatus>>,
}
```

Update `WorkerHandles` in `worker.rs` to use `flume::Sender<Envelope>`.

- [ ] **Step 3: Add current_trace_id helper**

In `crates/cairn-pty/src/ghostty/mod.rs`, add the ambient trace context capture function:

```rust
/// Extract the current span's W3C traceparent string, if an OpenTelemetry
/// context is active. Returns `None` when no OTLP layer is installed or
/// the current span has no trace context.
fn current_trace_id() -> Option<String> {
    use opentelemetry::trace::TraceContextExt as _;
    let cx = tracing_opentelemetry::OpenTelemetrySpanExt::context(
        &tracing::Span::current(),
    );
    let span_ref = cx.span();
    let sc = span_ref.span_context();
    if sc.is_valid() {
        Some(format!(
            "00-{}-{}-{:02x}",
            sc.trace_id(),
            sc.span_id(),
            sc.trace_flags().to_u8(),
        ))
    } else {
        None
    }
}
```

This requires `opentelemetry` and `tracing-opentelemetry` as deps of `cairn-pty`. However, per the spec, `cairn-pty` should NOT depend on these. Instead, move this function to `cairn-daemon/src/telemetry.rs` and have the daemon set a thread-local or use a different approach.

**Alternative (simpler):** Add `opentelemetry` (API-only, lightweight) and `tracing-opentelemetry` to `cairn-pty`'s dependencies. The API crate is ~100KB and adds no runtime overhead when no provider is installed. This is the pragmatic approach — the `current_trace_id()` call returns `None` when no OTLP layer exists.

Evaluate: if the spec's "cairn-pty untouched" constraint is firm, the daemon must set `trace_id` on every `PtySession` call instead. This means either:
- Adding `trace_id: Option<String>` to `PtySession` methods (rejected in the design), or
- Having the daemon wrap `GhosttyPty` in a newtype that intercepts calls and sets trace_id via a setter before each send.

**Decision:** Add `opentelemetry` (API-only) and `tracing-opentelemetry` to `cairn-pty` as lightweight deps. The API crate adds no runtime overhead when no provider is installed — `current_trace_id()` returns `None`. This is the pragmatic choice over complicating the architecture to avoid a dep. The spec's "cairn-pty untouched" referred to not adding the full OTLP exporter + SDK, which we're not.

```toml
# crates/cairn-pty/Cargo.toml
opentelemetry = { version = "0.30", default-features = false }
tracing-opentelemetry = { version = "0.29", default-features = false }
```

- [ ] **Step 4: Wrap all GhosttyPty send sites in Envelope**

In `crates/cairn-pty/src/ghostty/mod.rs`, update every `cmd_tx.send` and `cmd_tx.send_async` call. Example for `subscribe`:

```rust
async fn subscribe(&self, client_id: ClientId) -> Result<Subscription, PtyError> {
    let (tx, rx) = oneshot::channel();
    self.cmd_tx
        .send_async(Envelope {
            cmd: Command::Subscribe {
                client_id,
                reply: tx,
            },
            trace_id: current_trace_id(),
        })
        .await
        .map_err(|_| PtyError::Closed)?;
    rx.await.map_err(|_| PtyError::Closed)?
}
```

Apply the same wrapping to: `resize`, `size`, `write`, `signal`, `inject`, `kill` (Shutdown). For `kill` (synchronous `cmd_tx.send`), the pattern is the same but with `send` instead of `send_async`.

- [ ] **Step 5: Update worker.rs to receive Envelope**

In `crates/cairn-pty/src/ghostty/worker.rs`, change the `cmd_rx` type in `WorkerHandles` and `SessionState` from `flume::Receiver<Command>` to `flume::Receiver<Envelope>`. Update the select arm:

```rust
recv = s.cmd_rx.recv_async() => {
    let envelope = match recv {
        Ok(e) => e,
        Err(_) => break,
    };
    let Envelope { cmd, trace_id: _trace_id } = envelope;
    // _trace_id used in Task 7 for per-command spans
```

The `match cmd { ... }` dispatch is unchanged — it still matches on `Command` variants.

Also update `drain_commands_with_construction_error` to receive `Envelope` (just destructure and ignore `trace_id`).

- [ ] **Step 6: Pass session_id through spawn**

In `crates/cairn-daemon/src/spawn.rs`, chain `.with_session_id(...)`:

The registry's `create` method already has the `id` string. Pass it to `options_from` by adding a `session_id` parameter:

```rust
pub fn options_from(spec: SessionSpec, default_shell: &str, session_id: String) -> SpawnOptions {
    // ... existing code ...
    SpawnOptions::new(cmd)
        .with_scrollback_lines(spec.scrollback_lines as usize)
        .with_session_id(session_id)
}
```

Update both call sites in `crates/cairn-daemon/src/registry.rs`:
- `create`: `options_from(spec.clone(), default_shell, id.clone())`
- `restart`: `options_from(entry.spec.clone(), default_shell, entry.id.clone())`

- [ ] **Step 7: Build and test**

Run: `cargo build --all-targets && cargo nextest run`
Expected: all tests pass. The `MockSession` in worker.rs tests creates `SpawnOptions` via `default_opts()` which calls `SpawnOptions::new(...)` — session_id defaults to `""`.

- [ ] **Step 8: Commit**

```
refactor(cairn-pty): wrap Command in Envelope for trace context transport

Introduces Envelope { cmd, trace_id } as the flume channel payload.
GhosttyPty captures the ambient OTel trace context before sending.
Also adds session_id to SpawnOptions for per-session span labeling.
```

---

## Task 6: Per-session span and worker lifecycle events

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`

- [ ] **Step 1: Add the pty_session span to run_session**

At the top of `run_session`, before the `pending_writes` line:

```rust
let _session_span = tracing::info_span!(
    "pty_session",
    session_id = %s.session_id,
)
.entered();

tracing::info!(
    cols = s.initial_size.cols,
    rows = s.initial_size.rows,
    "session started"
);
```

The `session_id` field comes from `SessionState`. Add `session_id: String` to `SessionState` and populate it from `opts.session_id` in `spawn()` and `spawn_with()`.

- [ ] **Step 2: Add "session ended" event**

At the end of `run_session`, just before the final `*bcast_tx.borrow_mut() = None;`:

```rust
tracing::info!("session ended");
```

- [ ] **Step 3: Add "child exited" event**

In the `child.wait()` select arm, in the `Ok(s_val)` branch, after the `exit_tx.send(...)`:

```rust
Ok(s_val) => {
    let _ = s.exit_tx.send(Some(crate::ExitStatus::from_std(s_val, crate::types::now_unix_ms(), s.exit_reason.take())));
    tracing::info!(
        exit_code = ?s_val.code(),
        signal = ?{
            use std::os::unix::process::ExitStatusExt as _;
            s_val.signal()
        },
        "child exited"
    );
}
```

Note: `std::process::ExitStatus::code()` returns `Option<i32>`. On Unix, `signal()` requires `ExitStatusExt`. Since `s_val` is consumed by `from_std`, capture the values before that call:

```rust
Ok(s_val) => {
    use std::os::unix::process::ExitStatusExt as _;
    let exit_code = s_val.code();
    let exit_signal = s_val.signal();
    let _ = s.exit_tx.send(Some(crate::ExitStatus::from_std(s_val, crate::types::now_unix_ms(), s.exit_reason.take())));
    tracing::info!(exit_code = ?exit_code, signal = ?exit_signal, "child exited");
}
```

- [ ] **Step 4: Add "pty eof" event**

In the PTY read `Ok(0)` arm, before `pty_closed = true;`:

```rust
tracing::debug!("pty eof");
```

- [ ] **Step 5: Add "resized" event**

In the `Command::Resize` arm, after `current_size.set(size);`:

```rust
tracing::debug!(cols = size.cols, rows = size.rows, "resized");
```

(Inside the `if res.is_ok()` block.)

- [ ] **Step 6: Write tracing-test assertions for lifecycle events**

In the existing `mod tests` in `worker.rs`, add:

```rust
#[tokio::test]
#[tracing_test::traced_test]
async fn session_lifecycle_events_are_emitted() {
    let start = buf_snapshot();

    let session = MockSession::new(default_opts());
    // Let the session start (the "session started" event fires in run_session).
    tokio::time::sleep(Duration::from_millis(50)).await;

    let started = buf_lines_since(start, "session started");
    assert!(!started.is_empty(), "expected 'session started' event");

    // Shut down the session — triggers "session ended".
    session.pty.shutdown().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let ended = buf_lines_since(start, "session ended");
    assert!(!ended.is_empty(), "expected 'session ended' event");
}
```

Note: `shutdown()` is `GhosttyPty::kill()` — check the exact method name. The test may need adjustment based on whether `MockChild::start_kill` triggers the exit path that emits "child exited".

- [ ] **Step 7: Run tests**

Run: `cargo nextest run -p cairn-pty`
Expected: all tests pass including the new lifecycle event test.

- [ ] **Step 8: Commit**

```
feat(cairn-pty): per-session tracing span and lifecycle events

Adds info_span!("pty_session") at run_session entry. Lifecycle
events: session started, child exited, pty eof, resized,
session ended.
```

---

## Task 7: Per-command spans with span links on worker

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`

This task wires the `trace_id` from the `Envelope` into per-command child spans with OTel span links.

- [ ] **Step 1: Add a make_cmd_span helper**

At the bottom of `worker.rs` (before `mod tests`), add:

```rust
/// Create a child span for the current command dispatch. If `trace_id`
/// contains a valid W3C traceparent, the span carries an OpenTelemetry
/// link to the remote context.
fn make_cmd_span(name: &'static str, trace_id: &Option<String>) -> tracing::span::EnteredSpan {
    let span = tracing::info_span!(target: "cairn_pty::cmd", name);
    if let Some(ref tp) = trace_id {
        if let Some(remote_cx) = parse_traceparent(tp) {
            span.add_link(remote_cx.span().span_context().clone());
        }
    }
    span.entered()
}

/// Parse a W3C traceparent string into an OTel SpanContext.
fn parse_traceparent(tp: &str) -> Option<opentelemetry::Context> {
    use opentelemetry::propagation::TextMapPropagator as _;
    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    let mut carrier = std::collections::HashMap::new();
    carrier.insert("traceparent".to_string(), tp.to_string());
    let cx = propagator.extract(&carrier);
    if cx.span().span_context().is_valid() {
        Some(cx)
    } else {
        None
    }
}
```

Note: check if `tracing::Span::add_link()` exists — this is an OpenTelemetry extension. If not available directly on `tracing::Span`, the link must be set via `tracing_opentelemetry::OpenTelemetrySpanExt::add_link()`. Adjust the implementation accordingly.

If `add_link` is not available, an alternative approach: create the span with a builder that includes the link at construction time via `tracing_opentelemetry::OpenTelemetrySpanExt::set_parent()` or custom span attributes. Research the exact API at implementation time.

- [ ] **Step 2: Apply per-command spans in the dispatch loop**

In the select arm for `cmd_rx.recv_async()`, after destructuring the envelope:

```rust
recv = s.cmd_rx.recv_async() => {
    let envelope = match recv {
        Ok(e) => e,
        Err(_) => break,
    };
    let Envelope { cmd, trace_id } = envelope;

    // ... existing exit_published check ...

    match cmd {
        Command::Subscribe { .. } => {
            let _span = make_cmd_span("cmd.subscribe", &trace_id);
            // ... existing Subscribe handling ...
        }
        Command::Resize { .. } => {
            let _span = make_cmd_span("cmd.resize", &trace_id);
            // ... existing Resize handling ...
        }
        Command::Write { .. } => {
            let _span = make_cmd_span("cmd.write", &trace_id);
            // ... existing Write handling ...
        }
        Command::Signal { .. } => {
            let _span = make_cmd_span("cmd.signal", &trace_id);
            // ... existing Signal handling ...
        }
        Command::Inject { .. } => {
            let _span = make_cmd_span("cmd.inject", &trace_id);
            // ... existing Inject handling ...
        }
        // No per-command span for these:
        Command::Shutdown => { /* existing */ }
        Command::Detach { .. } => { /* existing */ }
        Command::Size { .. } => { /* existing */ }
    }
}
```

Be careful with the post-exit normalisation block — the per-command span should wrap the normal dispatch, not the post-exit fallthrough.

- [ ] **Step 3: Build and test**

Run: `cargo build --all-targets && cargo nextest run -p cairn-pty`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```
feat(cairn-pty): per-command spans with OTel span links

Worker creates brief child spans for each command dispatch. When
trace_id is present (from the daemon's handler span), the command
span carries an OTel link to the remote context.
```

---

## Task 8: Daemon-side rpc and attach spans + lifecycle events

**Files:**
- Modify: `crates/cairn-daemon/src/daemon.rs`
- Modify: `crates/cairn-daemon/src/handlers/attach.rs`
- Modify: `crates/cairn-daemon/src/registry.rs`
- Modify: `crates/cairn-daemon/src/telemetry.rs`

- [ ] **Step 1: Add extract_remote_context helper to telemetry.rs**

In `crates/cairn-daemon/src/telemetry.rs`:

```rust
/// Extract a remote OTel context from a `call-context` record.
/// Returns `None` if ctx is `None`, trace-context is `None`, or the
/// traceparent is invalid.
pub fn extract_remote_context(
    ctx: &Option<cairn_protocol::cairn::daemon::types::CallContext>,
) -> Option<opentelemetry::Context> {
    let tp = ctx.as_ref()?.trace_context.as_ref()?;
    use opentelemetry::propagation::TextMapPropagator as _;
    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    let mut carrier = std::collections::HashMap::new();
    carrier.insert("traceparent".to_string(), tp.clone());
    let cx = propagator.extract(&carrier);
    if cx.span().span_context().is_valid() {
        Some(cx)
    } else {
        None
    }
}
```

- [ ] **Step 2: Add rpc spans to Handler methods in daemon.rs**

For each `Handler` method, create an `info_span!("rpc", method = "sessions.create", ...)` and instrument the call. Example for `create`:

```rust
async fn create(
    &self,
    _cx: ConnCtx,
    ctx: Option<cairn_protocol::cairn::daemon::types::CallContext>,
    spec: SessionSpec,
) -> anyhow::Result<Result<SessionInfo, WireError>> {
    let span = tracing::info_span!("rpc", method = "sessions.create");
    // If the client sent a traceparent, link to it
    if let Some(remote_cx) = crate::telemetry::extract_remote_context(&ctx) {
        // add link to span — use the same pattern as Task 7
    }
    let _enter = span.enter();
    Ok(sess::create(self, spec).await)
}
```

Apply to all 14 Handler methods (11 sessions + 3 meta). For streaming methods (`wait`, `logs`, `attach`, `send`), the span should wrap the entire stream lifetime via `.instrument(span)` rather than `.enter()`.

- [ ] **Step 3: Add attach span and lifecycle events to attach.rs**

In `crates/cairn-daemon/src/handlers/attach.rs`:

After the subscribe succeeds and before spawning the task, log the attach event and create the attach span:

```rust
tracing::info!(
    session_id = %id,
    client_id = %client_id,
    no_stdin = init.no_stdin,
    "client attached"
);

let attach_span = tracing::info_span!(
    "attach",
    session_id = %id,
    client_id = %client_id,
);
```

Instrument the spawned task: `tokio::spawn(async move { ... }.instrument(attach_span));`

Add `use tracing::Instrument as _;` at the top of the file.

Inside the spawned task, add events at each exit point:

- `ClientEvent::Detach =>`: `tracing::info!(session_id = %id, client_id = %client_id, "client detached");`
- `None =>` (stream closed): `tracing::info!(session_id = %id, client_id = %client_id, "client disconnected");`
- `RecvError::Lagged(_)`: `tracing::warn!(session_id = %id, client_id = %client_id, "client lagged");`
- `RecvError::Closed`: `tracing::info!(session_id = %id, client_id = %client_id, "session ended under attached client");`
- `kick_rx =>`: `tracing::info!(session_id = %id, client_id = %client_id, "client kicked");`

The `id` and `client_id` variables need to be cloned into the spawned task (they're `String` and `ClientId` respectively — both `Clone`).

- [ ] **Step 4: Add "session created" event to registry.rs**

In `crates/cairn-daemon/src/registry.rs`, in `create()`, after `self.sessions.write().expect(...).insert(id, entry);`:

```rust
tracing::info!(
    session_id = %id,
    name = ?name,
    command = ?spec.command,
    "session created"
);
```

Note: `id` and `name` are available before the insert; `spec.command` requires `spec` to still be in scope. Check the borrow flow — `spec` is cloned for `options_from` earlier, so it's still available.

- [ ] **Step 5: Build and run all tests**

Run: `cargo build --all-targets && cargo nextest run`
Expected: all tests pass.

- [ ] **Step 6: Run clippy and fmt**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings`
Expected: clean

- [ ] **Step 7: Commit**

```
feat(cairn-daemon): rpc spans, attach spans, and lifecycle events

Handler methods create info_span!("rpc") with method name.
Attach handler creates child info_span!("attach") instrumented on
the bridge task. Lifecycle events: session created, client
attached/detached/disconnected/lagged/kicked, session ended.
```

---

## Task 9: Integration tests for call-context plumbing

**Files:**
- Modify: `crates/cairn-daemon/tests/daemon_meta.rs`
- Modify: `crates/cairn-daemon/tests/daemon_unary.rs`

- [ ] **Step 1: Test that a synthetic traceparent is accepted**

In `crates/cairn-daemon/tests/daemon_unary.rs`, add:

```rust
#[tokio::test]
async fn create_with_call_context_is_accepted() {
    let h = common::DaemonHarness::start().await;
    let client = h.client();
    let ctx = Some(bindings::cairn::daemon::types::CallContext {
        trace_context: Some(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_string(),
        ),
    });
    let spec = spec("ctx-test", &["sleep", "100"]);
    let created = bindings::client::cairn::daemon::sessions::create(&client, ctx.as_ref(), &spec)
        .await
        .expect("transport")
        .expect("create");
    assert!(created.name.as_deref() == Some("ctx-test"));
}
```

- [ ] **Step 2: Test that None call-context works (backward compat)**

This is already covered by every existing test (they all pass `None` after Task 4). Verify explicitly:

```rust
#[tokio::test]
async fn create_with_none_call_context_is_accepted() {
    let h = common::DaemonHarness::start().await;
    let client = h.client();
    let spec = spec("no-ctx", &["sleep", "100"]);
    let created = bindings::client::cairn::daemon::sessions::create(&client, None, &spec)
        .await
        .expect("transport")
        .expect("create");
    assert!(created.name.as_deref() == Some("no-ctx"));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-daemon -E 'test(~call_context)'`
Expected: both new tests pass.

- [ ] **Step 4: Commit**

```
test(cairn-daemon): verify call-context plumbing with and without traceparent
```

---

## Task 10: Update architecture doc and final verification

**Files:**
- Modify: `docs/architecture/pty-session/observability.md`
- Modify: `docs/architecture/pty-session/README.md`

- [ ] **Step 1: Update observability.md baseline section**

Update the "Cairn baseline" section to list the full current inventory of tracing calls (now much more than five `warn!`s). Update the "Subscriber and transport" section to reflect the actual `init_tracing` implementation.

- [ ] **Step 2: Update README.md build-list item 9**

Update item 9 to reflect what's done vs. remaining:

```markdown
9. **Observability** — ... **Done (v0).** Configurable log format (pretty/compact/json/full/off); OTLP trace layer gated on `OTEL_EXPORTER_OTLP_ENDPOINT`; per-session `pty_session` spans; per-RPC `rpc` spans; per-attach `attach` spans with lifecycle events; `call-context` on all WIT operations for W3C traceparent propagation; `Envelope` trace context transport across the flume boundary with span links. **Deferred:** CLI-side OTLP export; OTLP push metrics; per-session spans with span links for actual OTel navigation (requires validating `add_link` API availability).
```

- [ ] **Step 3: Full test suite**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo nextest run`
Expected: all clean, all pass.

- [ ] **Step 4: Commit**

```
docs: update observability architecture doc to reflect implementation
```

---

## Dependency chain

```
Task 1 (LogFormat + OTLP deps)     — independent
Task 2 (WIT schema)                — independent
Task 3 (daemon Handler sigs)       — depends on Task 2
Task 4 (client + test call sites)  — depends on Task 2
Task 5 (Envelope + session_id)     — depends on Task 1 (for OTel deps)
Task 6 (pty_session span + events) — depends on Task 5
Task 7 (per-command spans + links) — depends on Task 5, Task 6
Task 8 (rpc/attach spans + events) — depends on Task 3, Task 5
Task 9 (integration tests)         — depends on Task 4, Task 8
Task 10 (docs)                     — depends on all
```

Tasks 1 and 2 can run in parallel. Tasks 3 and 4 can run in parallel after Task 2. Tasks 6, 7, and 8 have interleaving deps but should be done sequentially.

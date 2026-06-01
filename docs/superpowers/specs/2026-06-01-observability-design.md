# Observability: OTLP Traces, Structured Events, and Cross-Boundary Correlation

Design spec for cairn observability (build-list item #9). Covers the
tracing subscriber stack, OTLP export, WIT-level trace context
propagation, span topology across the flume channel boundary, and
lifecycle events.

Reference: `docs/architecture/pty-session/observability.md`.

---

## 1. Subscriber stack

The daemon's `main.rs` builds a layered `tracing_subscriber::Registry`
with two optional layers.

### Stderr fmt layer

Controlled by `--log-format` / `CAIRN_LOG_FORMAT`:

| Format    | Description                                          |
| --------- | ---------------------------------------------------- |
| `pretty`  | Multi-line, thread ids+names, line numbers (default) |
| `compact` | Condensed single-line                                |
| `json`    | Single-line JSON for log aggregation                 |
| `full`    | Single-line with all metadata                        |
| `off`     | Disable stderr logs entirely                         |

`LogFormat` lives in `config.rs` with a `clap::ValueEnum` derive.

Filter directive from `--log` / `CAIRN_LOG`
(default `"info,cairn_daemon=info,cairn_pty=info"`).

### OTLP trace layer

Activated when `OTEL_EXPORTER_OTLP_ENDPOINT` or
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` is set in the environment. No
dedicated CLI flag — all configuration via standard `OTEL_*` env vars
(protocol, headers, resource attributes, sampling).

Uses `tracing-opentelemetry` bridge with `opentelemetry-otlp` batch
exporter (tonic/gRPC). The `SdkTracerProvider` guard is held for the
daemon's lifetime so pending spans flush on shutdown.

Subscriber init moves to a dedicated function:

```rust
fn init_tracing(
    log: &str,
    format: LogFormat,
) -> anyhow::Result<Option<opentelemetry_sdk::trace::SdkTracerProvider>>
```

Returns `Some(provider)` when OTLP is active (caller holds it until
shutdown), `None` otherwise.

### New dependencies (cairn-daemon only)

- `opentelemetry` (API)
- `opentelemetry_sdk`
- `opentelemetry-otlp` (with `tonic` feature for gRPC)
- `tracing-opentelemetry`

`cairn-pty` and `cairn-client` are untouched.

---

## 2. WIT schema: `call-context`

A new `call-context` record carries cross-cutting per-call metadata.
Every operation takes `ctx: option<call-context>` as its first
parameter. Clients that don't care about tracing pass `None`.

```wit
record call-context {
    trace-context: option<string>,  // W3C traceparent
}
```

`call-context` is defined in the `types` interface alongside the
other shared records.

Concrete changes to `wit/cairn.wit`:

```wit
interface sessions {
    use types.{
        session-id, client-id, session-spec, session-info, signal,
        log-window, attach-init, client-event, server-event,
        exit-status, error, call-context,
    };

    list-all: func(ctx: option<call-context>) -> list<session-info>;
    inspect:  func(ctx: option<call-context>, id: session-id) -> result<session-info, error>;
    create:   func(ctx: option<call-context>, spec: session-spec) -> result<session-info, error>;
    rename:   func(ctx: option<call-context>, id: session-id, new-name: string) -> result<_, error>;
    restart:  func(ctx: option<call-context>, id: session-id, force: bool) -> result<_, error>;
    kill:     func(ctx: option<call-context>, id: session-id, sig: signal, grace-ms: option<u32>) -> result<_, error>;
    kick:     func(ctx: option<call-context>, id: session-id, client: option<client-id>) -> result<_, error>;
    wait:     func(ctx: option<call-context>, id: session-id) -> future<exit-status>;
    logs:     func(ctx: option<call-context>, id: session-id, window: log-window, follow: bool) -> stream<list<u8>>;
    attach:   func(ctx: option<call-context>, id: session-id, init: attach-init, events: stream<client-event>) -> stream<server-event>;
    send:     func(ctx: option<call-context>, id: session-id, chunks: stream<list<u8>>) -> result<_, error>;
}

interface meta {
    use types.{error, call-context};

    authenticate: func(ctx: option<call-context>, token: string) -> result<_, error>;
    whoami:       func(ctx: option<call-context>) -> result<string, error>;
    version:      func(ctx: option<call-context>) -> version-info;
}
```

This is a breaking wire change. Acceptable at `@0.1.0` (pre-stable).

### Daemon side

Each `Handler` method extracts the traceparent string. If present, a
span link is created from the handler's `rpc` span to the remote
client's span context. A helper centralizes parsing:

```rust
fn extract_remote_context(ctx: &Option<CallContext>) -> Option<otel::Context>
```

Uses `opentelemetry::propagation::TextMapPropagator` with the W3C
`TraceContextPropagator`.

### Client side (v0)

The CLI passes `None` for `call-context`. When CLI-side OTLP is added
later, it injects its current span's traceparent here.

---

## 3. Span topology

Three span scopes connected by links:

### `pty_session` span (worker thread)

Created at the top of `run_session`. Lives for the session's entire
lifetime. Fields: `session_id`. The session id flows from the registry
(`SessionRegistry::create` generates a UUIDv7) through `SpawnOptions`
(new `session_id: String` field) into the worker. All worker events
(resize, child exit, leader election, PTY EOF) are children.

### `rpc` span (daemon handler)

Created in each `Handler` method. Lives for the duration of the RPC
(short for unary, long for `attach`/`logs`). Fields: `session_id`,
`client_id`, `method`. If `call-context.trace-context` is present, the
span carries a link to the remote client's span context.

### `attach` span (daemon attach task)

Child of the `rpc` span, created when `attach::attach()` spawns its
bridge task via `tokio::spawn(...).instrument(span)`. Lives for the
client's attach duration. Fields: `session_id`, `client_id`. Attach
lifecycle events are children.

### Cross-boundary: handler -> worker

The `Command` enum is wrapped in an `Envelope`:

```rust
struct Envelope {
    cmd: Command,
    trace_id: Option<String>,
}
```

`GhosttyPty`'s `PtySession` impl captures the ambient span context
(`Span::current()` -> extract traceparent) before constructing the
`Envelope`. The `PtySession` trait signatures do not change — trace
context is a transport concern.

On the worker side, when dispatching a command with a `trace_id`, the
worker creates a brief child span of `pty_session` (e.g. `cmd.resize`,
`cmd.write`, `cmd.subscribe`) with a **span link** to the handler's
context. The span lives only for that command's processing.

This design is forward-compatible with the subprocess worker backend:
`trace_id` is a `String` that serializes over any IPC wire. The
subprocess extracts it and creates the same span link.

### Cross-boundary: client -> daemon

The `call-context.trace-context` field carries the client's
traceparent. The daemon handler creates a link from its `rpc` span.

### Link chain (once CLI OTLP exists)

```
CLI span <-link- daemon rpc span <-link- worker cmd.* span (child of pty_session)
```

All three are independent trace trees, correlated by span links and
shared `session_id` / `client_id` fields. Tempo and Jaeger render links
as navigable references.

---

## 4. Lifecycle events

All events use structured field syntax. No PTY byte payloads are ever
logged.

### Worker events (children of `pty_session` span)

| Event           | Level   | Fields                 | Location in `worker.rs`             |
| --------------- | ------- | ---------------------- | ----------------------------------- |
| session started | `info`  | `cols`, `rows`         | Top of `run_session`                |
| resized         | `debug` | `cols`, `rows`         | After successful `Command::Resize`  |
| child exited    | `info`  | `exit_code`, `signal`  | `child.wait()` select arm, Ok path  |
| pty eof         | `debug` | —                      | PTY read `Ok(0)` arm               |
| session ended   | `info`  | —                      | End of `run_session`, final teardown|

Leader promotion / vacation events already exist at `info` level.

### Daemon handler events

| Event                          | Level  | Fields                                  | Location                        |
| ------------------------------ | ------ | --------------------------------------- | ------------------------------- |
| session created                | `info` | `session_id`, `name`, `command`         | `registry.rs` after insertion   |
| client attached                | `info` | `session_id`, `client_id`, `no_stdin`   | `attach.rs` after subscribe     |
| client detached                | `info` | `session_id`, `client_id`               | `attach.rs` on Detach event     |
| client disconnected            | `info` | `session_id`, `client_id`               | `attach.rs` on stream close     |
| client lagged                  | `warn` | `session_id`, `client_id`               | `attach.rs` on RecvError::Lagged|
| client kicked                  | `info` | `session_id`, `client_id`               | `attach.rs` on kick_rx          |
| session ended under client     | `info` | `session_id`, `client_id`               | `attach.rs` on RecvError::Closed|

### Events NOT added (already covered)

- Auth failure — logged by WT auth chain in `serve.rs`
- Spawn failure — logged as `warn!` in `registry.rs`
- Protocol errors — wRPC logs internally

---

## 5. Command envelope

The flume channel type changes from `Command` to `Envelope`:

```rust
pub(crate) struct Envelope {
    pub cmd: Command,
    pub trace_id: Option<String>,
}
```

### Sender side (GhosttyPty)

Each `PtySession` method captures the ambient trace context before
sending:

```rust
fn current_trace_id() -> Option<String>
```

Extracts the trace-id + span-id from the current OTel span context if
the OTLP layer is active, returns `None` otherwise. This keeps the
`PtySession` trait signatures unchanged — trace context is captured
implicitly from the calling task's span.

### Receiver side (worker)

The `run_session` select loop receives `Envelope { cmd, trace_id }`.
Before the `match cmd` dispatch, it extracts `trace_id` once. For
commands that warrant a per-command span (`Subscribe`, `Resize`,
`Write`, `Signal`, `Inject`), a child span of `pty_session` is created
with a span link to the extracted context:

```rust
let Envelope { cmd, trace_id } = envelope;
// For applicable commands:
let _cmd_span = make_cmd_span("cmd.resize", &trace_id);
```

`Detach`, `Shutdown`, and `Size` skip the per-command span.

---

## 6. Testing strategy

### Unit tests (no OTLP collector needed)

- **LogFormat parsing** — each variant round-trips through
  `clap::ValueEnum`. In `config.rs` test module.
- **call-context plumbing** — daemon integration test passes
  `call-context: some(...)` with a synthetic traceparent, verifies
  the handler accepts it without error. `None` verifies
  backward-compat. Uses `DaemonHarness`.
- **Envelope construction** — worker test verifies `trace_id` is
  carried through the flume channel. Uses the existing `MockSession`
  harness.

### Integration tests (tracing-test)

- **Lifecycle events** — the existing `buf_snapshot()` /
  `buf_lines_since()` pattern in `worker.rs` extends to cover new
  events: "session started", "resized", "child exited",
  "session ended". Same approach for attach handler events via daemon
  integration tests.
- **Span structure** — `tracing-test` assertions that `pty_session`
  carries `session_id`, `rpc` carries `method`, `attach` carries
  `client_id`.

### Manual verification

Spin up a local Jaeger or Grafana Tempo, set
`OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317`, run
`cairn-daemon`, exercise operations, verify traces with span links
are navigable between handler and session spans.

### Not tested

- OTLP export correctness — OpenTelemetry SDK's responsibility.
- CLI-side traceparent injection — deferred with CLI OTLP.

---

## 7. Deferred work

- **CLI-side OTLP export.** The `cairn` CLI does not get an OTLP layer
  in this iteration. Long-lived operations like `attach` would benefit
  from client-side spans in Tempo, but the startup latency cost for
  short-lived commands needs investigation. The plumbing is ready:
  `call-context.trace-context` is `None` for now; when CLI OTLP is
  added, it injects its current span's traceparent.

- **Baggage propagation.** W3C tracebaggage could carry `session_name`,
  `user`, etc. across the wire. Not needed until there's a multi-hop
  topology (reverse proxy -> daemon -> subprocess worker).

- **Per-session log files.** Operators who want per-session filtering
  use `session_id` field queries in their log aggregator, or
  `--log-format json` piped to `jq`. File-based per-session logs
  (zmx-style) are not planned.

- **Metrics.** OTLP push metrics via `opentelemetry-otlp`. Same
  env-var activation pattern as traces. Deferred until there's a
  concrete need.

---

## Files modified

| File | Change |
| ---- | ------ |
| `crates/cairn-daemon/Cargo.toml` | Add `"json"` to tracing-subscriber features; add opentelemetry, opentelemetry_sdk, opentelemetry-otlp, tracing-opentelemetry deps |
| `crates/cairn-daemon/src/config.rs` | Add `LogFormat` enum with `clap::ValueEnum` |
| `crates/cairn-daemon/src/main.rs` | Add `--log-format` arg; replace subscriber init with `init_tracing()` |
| `crates/cairn-daemon/src/tracing.rs` | New module: `init_tracing()`, `extract_remote_context()`, `current_trace_id()` helpers |
| `crates/cairn-daemon/src/daemon.rs` | Update `Handler` impls: accept `call-context`, create `rpc` spans with links |
| `crates/cairn-daemon/src/handlers/attach.rs` | Add `attach` span, lifecycle events (attached/detached/lagged/kicked/disconnected/ended) |
| `crates/cairn-daemon/src/handlers/sessions.rs` | Pass trace context through to PtySession calls |
| `crates/cairn-daemon/src/registry.rs` | Add "session created" event |
| `crates/cairn-protocol/wit/cairn.wit` | Add `call-context` record; add `ctx` param to all operations |
| `crates/cairn-pty/src/ghostty/mod.rs` | Change channel type to `Envelope`; capture ambient trace context in PtySession impl |
| `crates/cairn-pty/src/ghostty/worker.rs` | Receive `Envelope`; add `pty_session` span; add per-command spans with links; add lifecycle events |
| `crates/cairn-pty/src/types.rs` | Add `session_id: String` to `SpawnOptions` |
| `docs/architecture/pty-session/observability.md` | Update to reflect implemented state |

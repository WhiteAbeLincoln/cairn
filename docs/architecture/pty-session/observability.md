# Observability

How operators see what cairn's PTY session layer is doing — logs,
spans, metrics, and ad-hoc state inspection. Sibling docs:
[[pty-lifecycle]], [[client-attach-and-election]], [[backpressure]],
[[resize-semantics]], [[query-response-delegation]],
[[external-protocol]], [[authentication]], [[daemon-process-model]],
[[error-recovery]], [[testing]], [[configuration]].

zmx is the reference. Where cairn already diverges (it always does,
because `tracing` is structured and `std.log` is not), the divergence
is called out explicitly.

---

## zmx baseline

zmx routes every log call through Zig's stdlib `std.log` facility,
replacing the default sink via `std_options.logFn` (`src/main.zig:17–29`)
with a process-global `log_system: LogSystem` (`src/main.zig:15`)
defined in `src/log.zig:3–102`. `LogSystem` is a thread-safe file
appender with these properties:

- **Sink**: single file, opened in append mode (`src/log.zig:17–28`).
  Default path is `<socket_dir>/logs/zmx.log` for the foreground CLI
  and `<socket_dir>/logs/<session_name>.log` for the daemon child
  (`src/main.zig:87–89`, `src/main.zig:827–838`). The daemon
  re-`init`s the log system *after* `fork()` to redirect to the
  per-session file (`src/main.zig:838`); this also closes the
  parent's fd so the original `zmx.log` isn't held open across the
  daemonisation boundary (`src/main.zig:782`).
- **Format**: `[{ms_epoch}] [{LEVEL}] ({scope}): <format>` with a
  trailing newline (`src/log.zig:57–78`). Scope is the Zig
  `@tagName(.enum_literal)` — almost always `.default` because zmx
  doesn't use scoped loggers. **Not structured** — payload is a
  format string with positional args; key/value pairs are encoded
  by convention as `name={...}` inside the format string
  (e.g. `"client connected fd={d} total={d}"`,
  `src/main.zig:2541–2544`).
- **Rotation**: when `current_size >= max_size` (5 MiB,
  `src/log.zig:7`), the file is renamed to `<path>.old` and a new
  empty file is opened (`src/log.zig:82–101`). Only one generation
  is kept; older rotations are clobbered.
- **Locking**: per-process `std.Thread.Mutex` (`src/log.zig:5`,
  `:43`). Multi-writer is not coordinated across processes — the CLI
  and the daemon write to *different* files post-fork, so the only
  cross-process contention is the brief window before
  re-initialisation.
- **Permissions**: configurable via `ZMX_LOG_MODE`
  (`src/main.zig:487–490`), default `0o640`.
- **Level filter**: compile-time `.debug`. There is no runtime knob.

What gets logged, by level (from the call sites surveyed in
`src/main.zig`):

| Level   | Events                                                                                                                                                                      |
| ------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `info`  | Daemon start/stop (`main.zig:2461`, `:612`), session create (`:773`), socket-path announcements (`:176`, `:219`, `:443`), client connect/disconnect (`:2541`, `:635`), leader change (`:644`), PTY spawn (`:728`), client detach (`:1033`), SIGTERM (`:2515`), shell exit (`:2559`), task completion (`:2584`), kill flow (`:1047`, `:1053`) |
| `warn`  | Recoverable failures: socket-file delete (`:857`), child-exit-code parse (`util.zig:345`), terminal-state format/alloc (`util.zig:570`, `:583`, `:624`, `:632`), SIGWINCH send (`main.zig:1170`), PTY write (`:2617`), `connect` retries (`:764`)               |
| `err`   | Unrecoverable per-call failures: `execvpe`/`execve` (`:681`, `:695`), child setup (`:721`), daemon read (`:1415`, `:2392`), session-unresponsive paths in CLI subcommands (`:1716`, `:1750`, `:1865`, `:1923`, `:2069`, `:2159`, `:2256`) |
| `debug` | Hot-path tracing: buffering PTY input (`:888`, `:898`), serialize terminal state (`:954`), init/resize cells (`:998`, `:1029`), run-command length (`:1158`), per-poll diagnostics (`:1219`, `:2646`) |

`debug` is compile-time on by default (`src/main.zig:19`), and the
`buffering pty input data={x}` calls (`:888`, `:898`) **log raw PTY
input bytes** — see Privacy below.

## Cairn baseline

`tracing` is on the dependency graph at the workspace root
(`Cargo.toml:14`) and re-exported into `cairn-pty`
(`crates/cairn-pty/Cargo.toml:12`). No `tracing_subscriber` is wired
anywhere in the workspace today — events flow into the global
dispatcher and, with no subscriber installed, are silently dropped.
Installing a subscriber is a host-process responsibility (the future
`cairn` daemon binary, `cairn-cli`, or a test harness); the worker
library deliberately stays neutral.

Today's full inventory of `tracing` calls in the PTY layer
(`crates/cairn-pty/src/pty/ghostty/worker.rs`):

- `worker.rs:217` — `error!` on `Terminal::new` failure.
- `worker.rs:229` — `error!` on `on_pty_write` callback install failure.
- `worker.rs:339` — `warn!` when `child.start_kill()` fails on shutdown.
- `worker.rs:410` — `warn!` when `child.wait()` fails (synthetic exit 1).
- `worker.rs:442` — `warn!` when flushing a PtyWriteFn response to the master fails.

Every call uses field syntax (`error = ?e` or `error = %e`), so the
events are already structured — `tracing-subscriber` with `fmt::layer`
will emit them as `error=...` key/value or JSON depending on layer
config. No `info`/`debug` events, no spans, no
[[backpressure]]/[[client-attach-and-election]]/[[resize-semantics]]
observability surface yet. **Currently we log only the error-recovery
paths inside the worker; everything else is dark.**

## Recommended event surface

The lift from "library that emits five `warn!`s" to "operable system"
is concentrated in two pieces: (a) per-session spans so every event
carries a session id, and (b) explicit info-level events at session
lifecycle boundaries. Concrete recommendations below; they slot into
the existing `run_session` task in `worker.rs:200–427`.

**Spans.** `run_session` should open a top-level
`info_span!("pty_session", session_id, child_pid)` `.entered()` for
the lifetime of the task; `child_pid` is recorded via
`Span::current().record` once `pty_process::Command::spawn` returns.
Same-task awaits inherit the span automatically. Cross-task wRPC
attach handlers in the host crate should open `pty_client` child
spans with `client_id`, `peer_addr`, `auth_method`, `transport`
(`uds` or `wt`), plus wRPC's `instance` + `name` on the invocation
span itself (see [[client-attach-and-election]], [[authentication]],
[[external-protocol]]).

**Events to add**, mapping zmx's `info`/`warn` set to cairn:

- Session spawned — `info!(cmd, args, child_pid, cols, rows)` inside
  `GhosttyPty::spawn` after the child is alive. zmx parallel
  `main.zig:728`.
- Session ended — `info!(reason, runtime_ms)` in the teardown block
  (`worker.rs:419–427`); `reason ∈ {child_exit, eof, shutdown_cmd,
  drop}` matching [[pty-lifecycle]].
- Child exited — `info!(exit_code, signal, runtime_ms)` in the
  `child.wait()` arm (`worker.rs:406–415`). zmx parallel
  `main.zig:2559`.
- Client attach / detach — `info!(client_id, auth_method,
  transport, client_type)` in the wRPC attach handler. zmx parallels
  `main.zig:2541`, `:635`.
- Leader transition — `info!(prev, next, reason, "leader changed")`
  in the dispatcher for [[query-response-delegation]]. zmx parallel
  `main.zig:644`.
- Resize — `debug!(rows, cols)` in `Command::Resize`
  (`worker.rs:372–387`). See [[resize-semantics]].
- Backpressure / lag — `warn!(client_id, lag_frames)` where
  `RecvError::Lagged(n)` surfaces on the receive side. Today
  `worker.rs:289` is a fire-and-forget `let _ = tx.send(chunk)`. See
  [[backpressure]].
- Auth failure — `warn!(peer_addr, reason)` in the
  `meta.authenticate` handler (or, for UDS, on `SO_PEERCRED` mismatch
  at accept). See [[authentication]].
- Protocol error — `warn!(client_id, instance, name, kind)` whenever
  a wRPC invocation surfaces a decode or transport error. See
  [[external-protocol]].

## Privacy: byte-level tracing

PTY traffic carries passwords (typed against terminal echo-off),
agent keys, OAuth tokens, JWT bearer tokens scrolled in `curl -v`
output, and arbitrary secrets that happen to land on stderr.
**Logging bytes is a security incident waiting to happen.** zmx
already trips this wire at `main.zig:888` and `:898` — debug logs
contain hex-encoded PTY input, and the default log level is debug.

Cairn's stance should be:

- No byte-level events at `info` or `debug`. Reserve `trace` for
  hex/length-prefixed payload logging, and document explicitly that
  enabling `trace` for `cairn_pty::pty=trace` exposes PTY contents.
- Add a build-time feature `unsafe-trace-bytes` (default off) gating
  even the `trace!` macro on payload bytes, so a release build cannot
  emit them no matter how the operator configures `RUST_LOG`.
- Where lengths are useful for debugging without payload (the common
  case), emit `debug!(bytes = data.len(), "vt_write")` not the bytes
  themselves.

## Subscriber and transport

The library deliberately does not install a subscriber. The host
binary (the daemon) should install `tracing_subscriber::fmt` with:

- `EnvFilter::from_default_env().with_default("info,cairn_pty::pty=info")`.
- For dev: pretty layer to stderr.
- For production: JSON layer to a file under
  `${XDG_STATE_HOME:-$HOME/.local/state}/cairn/cairn.log`, with a
  rotation policy. `tracing-appender`'s `RollingFileAppender` (daily +
  size-bounded) is the obvious fit; it sidesteps zmx's bespoke
  rotator (`src/log.zig:82–101`).
- When run under systemd, default to stderr with no timestamp prefix
  (journald supplies one) and rely on the unit's `StandardError=journal`.

The library should not depend on `tracing-subscriber` — only the
host binary picks the transport.

## Metrics

A separate surface from logs; the daemon process is the natural
owner. Minimum useful set:

- Gauges: `cairn_sessions_total`, `cairn_clients_total`,
  `cairn_clients_per_session{session_id}` (avoid high-cardinality —
  consider quantiles instead),
  `cairn_scrollback_bytes{session_id}`.
- Counters: `cairn_pty_bytes_in_total`, `cairn_pty_bytes_out_total`,
  `cairn_client_lag_events_total`, `cairn_auth_failures_total`,
  `cairn_protocol_errors_total`.
- Histograms: `cairn_child_runtime_seconds`,
  `cairn_resize_latency_ms`, `cairn_snapshot_serialize_ms`
  (per `format_snapshot`, `worker.rs:356`).

Recommended exposure: Prometheus text format at
`GET /metrics` on a small HTTP listener bound alongside the wRPC
endpoints (loopback by default, gated by [[authentication]]).
OpenTelemetry is
plausible but adds a heavy dep tree for a self-hosted daemon; defer
unless an integration partner demands it.

## State inspection (debug endpoint)

Operators occasionally need to ask "what is session X actually
doing?" without attaching a real client. zmx answers this by
attaching and inspecting; cairn can do better because sessions
already live as named objects in a registry. Recommendation:

- `GET /debug/sessions` → list `{session_id, child_pid, cols, rows,
  attached_clients, leader_id, scrollback_lines, pending_writes,
  created_at, last_resize_at}`.
- `GET /debug/sessions/{id}/state` → the same plus the serialised
  scrollback (large; cap at request param or stream).
- `GET /debug/sessions/{id}/clients` → per-client `{id, peer_addr,
  attached_since, lag_frames, last_recv_at}`.

The endpoint is admin-only ([[authentication]]) and disabled unless
`CAIRN_DEBUG_ENDPOINT=1` (or equivalent in [[configuration]]).
A separate CLI (`cairn inspect <id>`) is a thin wrapper over the
HTTP surface — easier to operate than embedding a debug REPL.

## Open Questions

- **Span propagation across the `flume` channel.** `cmd_rx.recv_async`
  in `worker.rs:309` consumes commands from other tasks. Should the
  sender attach its `tracing::Span` to the `Command` enum so that
  "client X requested resize" shows the client's span as the parent
  of the resize event?
- **PII redaction at trace level.** Even with `unsafe-trace-bytes`
  gated, an operator may turn it on once and forget. Should the
  `trace!` payload emitter run a simple regex against common secret
  shapes (`AKIA...`, `ssh-rsa ...`, `Bearer ey...`) and zero them?
- **Per-session vs. shared log file.** zmx writes one log per session
  (`main.zig:838`); cairn's single-daemon model wants one file with
  `session_id` as a span field. Confirm operators prefer the spans
  approach over `tail -f .../sessions/<id>.log`.
- **Metrics cardinality.** Tagging counters by `session_id` explodes
  Prometheus cardinality for long-lived daemons. Drop the label and
  rely on logs for per-session detail, or accept the cost?
- **OpenTelemetry traces vs. logs.** `tracing-opentelemetry` makes
  spans into OTel traces "for free", but for a daemon with one
  long-running task per session, do trace exports add value?
- **Log rotation strategy.** `tracing-appender` daily + 7-day
  retention vs. zmx-style 5 MiB + one generation (`src/log.zig:7`,
  `:82–101`). Daily aligns with journald; size-based is friendlier to
  high-traffic sessions. Likely both, configurable.
- **Cross-process correlation.** If the daemon spawns helper
  processes ([[query-response-delegation]] envisioned an external
  delegator), `trace_id` as an env var with W3C traceparent semantics?
- **Debug-endpoint dump format.** Scrollback can be megabytes:
  streaming NDJSON, `text/plain` with a truncation header? Resolve
  once [[terminal-state-and-replay]] settles the schema.

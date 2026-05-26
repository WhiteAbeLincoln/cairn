# Error and Recovery Model

How cairn classifies failures and what it does when each fires. The
taxonomy starts at `PtyError` (`crates/cairn-pty/src/pty/error.rs:8-20`)
and extends outward to wRPC transport errors, daemon-level resource
exhaustion, and the worker thread itself. zmx is the reference for the
pre-wRPC failure modes. Adjacent: [[pty-lifecycle]] (teardown
ordering), [[backpressure]] (lag-as-error), [[external-protocol]] (wire
errors), [[client-attach-and-election]] (re-attach),
[[daemon-process-model]] (resource caps), [[observability]] (logging).

## Failure taxonomy

`PtyError` has three variants (`error.rs:8-20`):

| Variant | Meaning | Recoverable? |
|---|---|---|
| `PtyError::Closed` | Session has exited; commands cannot run | No — session is terminal |
| `PtyError::Io { source }` | `std::io::Error` from PTY read/write or worker thread spawn | Depends on `ErrorKind` |
| `PtyError::Backend { source }` | Opaque error from libghostty-vt or `pty_process` | Usually fatal to the session |

`Closed` is the steady-state error of a session whose child has exited
(`worker.rs:314-333`). It is also the only error the public handle
fabricates: every channel-send failure on `GhosttyPty` collapses to it
(`pty/ghostty/mod.rs:60-62, 90-121`), because from the caller's side a
closed command channel means "the worker is gone".

`Backend` is the escape hatch. libghostty-vt allocation/construction/
`format_alloc` errors and `pty_process` errors are all wrapped behind
`source: Box<dyn Error + Send + Sync>` (`worker.rs:71-79, 84-92, 94-102,
132-140, 224-228, 489-496`). Callers treat these generically; advanced
consumers can downcast.

## Per-failure handling

### Child crash, external kill, PTY EIO, EOF

These four collapse into one path. Detection: `child.wait()` resolves,
or `pty.read` returns `Ok(0)` (slave fds closed), or `pty.read` returns
`Err(_)`. The select races all three (`worker.rs:265-415`); whichever
fires first publishes the final status to `exit_tx`
(`worker.rs:279, 408, 411`).

Action: log + publish exit status, **but keep the worker alive**. The
EOF and read-err arms set `pty_closed = true`, drop `bcast_tx` so live
subscribers see `RecvError::Closed` (`worker.rs:275-276, 297-298`), and
inline-await `child.wait()` if not already done. The child-wait-first
arm publishes status and lets the loop continue draining buffered
output (`worker.rs:398-415`). This is the "post-exit normalisation"
mode described in [[pty-lifecycle]].

User-visible: `wait()` returns the real `ExitStatus`. Subsequent
`write`/`resize`/`size` return `PtyError::Closed`. `subscribe` still
succeeds but the stream is pre-closed (`worker.rs:362-368`). Late
attaches get a snapshot of the final screen — divergence from zmx,
which destroys the session on child EOF
(`zmx/src/main.zig:2551-2560`) so the next attach gets a fresh PTY.
The read-error path *swallows* the underlying `io::Error` — by design
("EIO on a PTY master with a dead slave is morally an EOF") but worth
flagging if we ever need to distinguish "child crashed" from "PTY
went away" in telemetry. See Open Questions.

### PTY write failure (Command::Write)

Detection: `pty.write_all(&data).await` returns `Err(io::Error)`
(`worker.rs:392`).

Action: converted via `PtyError::from(io::Error)` → `PtyError::Io`
(`error.rs:22-26`) and sent back via the oneshot reply
(`worker.rs:393`). The session does **not** tear down on a single
write failure — the next read iteration will observe EIO/EOF naturally
if the PTY is truly dead, and an `EINTR`-style transient is fair game
for the caller to retry. zmx takes the same view
(`zmx/src/main.zig:2615-2619`): log + clear the output buffer, keep
the loop running.

### VT terminal callback errors (`on_pty_write`)

Two sites. **Installation** failure during construction
(`worker.rs:224-232`) is treated like any other init error: the worker
drains queued commands with a synthetic `PtyError::Backend` via
`drain_commands_with_construction_error` (`worker.rs:451-472`) and
exits; the `init_tx`/`init_rx` handshake (`worker.rs:60, 168-179`)
surfaces it synchronously to `GhosttyPty::spawn()`. **Runtime**
failure only shows up when draining queued query replies
(`flush_pending_writes`, `worker.rs:435-446`): the offending chunk is
dropped with a `warn!` log and the loop continues. Worst case is a
DA1 reply going missing, which the client can re-query.

### Worker thread panic

**Pre-init panic** (before `init_tx.send`): the parent's
`init_rx.recv()` returns `Err(_)`, matched at `worker.rs:171-178` and
surfaced as `PtyError::Backend` wrapping `"worker thread exited before
PTY was ready"`. **Post-init panic** (inside `run_session`): the
`LocalSet` drops, `cmd_rx` is destroyed, all subsequent `cmd_tx` sends
collapse to `PtyError::Closed` (`pty/ghostty/mod.rs:60-121`). `exit_tx`
also drops; `wait()` (`pty/ghostty/mod.rs:67-82`) observes
`rx.changed()` returning `Err` and falls back to
`synthetic_exit_status(1)` (`worker.rs:505-508`), so callers see a
synthesised failing exit rather than a phantom success.

No panic recovery — the session is dead. The daemon's session
registry should treat the next failed command as "session ended
unexpectedly" and remove the entry. zmx has no equivalent because a
panic in its session loop kills the OS process, which the parent-shell
`wait` observes directly.

### Construction errors during spawn

Every step from `pty_process::Pty::new()` through `builder.spawn(&pts)`
is fallible (`worker.rs:71-140`); each error arm runs
`init_tx.send(Err(e))` and returns. The parent's `init_rx.recv()`
(`worker.rs:168-179`) surfaces the error directly from
`GhosttyPty::spawn()` — the only codepath where `PtyError::Io` or
`PtyError::Backend` surfaces from `spawn()` itself.

### Abrupt client disconnect (TCP RST, tab closed, network blip)

Inside the worker, a dropped subscriber is **invisible**:
`broadcast::Sender::send` succeeds regardless of receiver count
(`worker.rs:288-290`, `let _ = tx.send(chunk)`). The transport task
drops its `Receiver`; no worker-side cleanup, no log line; the session
runs on. If the disconnected client was the
[[client-attach-and-election|leader]], the seat stays vacant until
another qualifying input lands (zmx pattern, `zmx/src/main.zig:622-631`,
which uses explicit `closeClient` on POLLHUP/POLLERR/POLLNVAL/EOF at
`zmx/src/main.zig:2650, 2657, 2698, 2713`). The reconnecting client
calls `subscribe()` again and pays one snapshot serialisation cost
(`worker.rs:481-498`); see [[terminal-state-and-replay]].

### Malformed wire protocol message

The wRPC transport fails to decode an inbound invocation against
the WIT schema ([[external-protocol]]). zmx's equivalent — unknown
tag — falls through to a `_` arm that logs and ignores
(`zmx/src/main.zig:2685-2688`) so old daemons stay forward-compatible.
cairn relies on wRPC's instance + function-name dispatch for
forward-compat (unknown function = ignore-with-error response), but a
malformed *value* in a known function indicates a client bug. Close
the underlying stream / connection, log once at the handler task,
let the client reconnect. Per-stream isolation means one bad client
never poisons another's subscription.

### Wire size and codec errors

QUIC streams (WT) and UDS connections both have implementation
limits on message size that wRPC surfaces as transport errors.
Close the stream / connection; same isolation as malformed-protocol.
Per-stream sizing for the attach output is governed by libghostty
snapshot serialisation cost (tens of KiB initial, smaller per
steady-state `server-event::output` element; see [[backpressure]]).

### Lagging client

`broadcast::Receiver::recv()` returns `RecvError::Lagged(n)`. The
worker is oblivious (`worker.rs:288-290`); detection lives in the
attach handler task. Kick the wRPC stream, force the client to
reconnect with a fresh `sessions.attach` invocation; its first
`server-event::snapshot` resynchronises. Full discussion in
[[backpressure]]. **A lag-kick is not a session failure** — must not
show up in any session-failure metric ([[observability]]).

### Network partition (heartbeat)

For WebTransport the QUIC layer has its own idle timeout, but
application-level liveness still wants a heartbeat. Specific
mechanism TBD (see [[external-protocol]] open questions). For UDS
attach connections, TCP-keepalive-style detection or an
application heartbeat. Cadence and threshold are operator-tunable
([[configuration]]). zmx has no equivalent — its blocking-poll UDS
attaches see RST or HUP immediately on peer death.

### Daemon-level resource exhaustion (FDs, memory, max sessions)

Lives *above* the `PtySession` layer. **FD exhaustion** on
`pty_process::Pty::new()` surfaces as `PtyError::Backend`
(`worker.rs:71-79`); the session-manager callsite of
`GhosttyPty::spawn()` decides whether to retry after reaping idle
sessions or reject with a 503-like rejection frame. **`max_sessions`
cap** is enforced by the session registry before calling `spawn()`;
hitting it returns a daemon-level error, not a `PtyError`. **OOM** is
not caught — Rust aborts, systemd/launchd restarts the daemon, clients
reconnect.

### Daemon SIGTERM / shutdown

The daemon's signal handler drops all `Arc<dyn PtySession>`s.
`GhosttyPty::drop()` (`pty/ghostty/mod.rs:124-131`) calls `kill()`,
which enqueues `Command::Shutdown`; the worker's `Shutdown` arm calls
`child.start_kill()` (immediate SIGKILL on Unix), awaits
`child.wait()`, and breaks the loop (`worker.rs:335-354`).

Divergence: zmx escalates SIGHUP → 500 ms → SIGKILL
(`zmx/src/main.zig:1046-1061`), reasoning that shells frequently ignore
SIGTERM. cairn has no grace period today ([[pty-lifecycle]] open
question) — so no `~/.bash_logout`, no shell history flush.

## Error signalling to clients

| Source | wRPC signal |
|---|---|
| Post-exit op (`PtyError::Closed`) | `Err(types::Error { code, message })` returned by the failing operation |
| `write()` returning `PtyError::Io` | `Err(types::Error { … })` on `sessions.send` |
| Session terminated mid-stream | `server-event::exited(exit-status)` then stream end |
| Lag-kick | Stream closed by handler; reconnect path is a fresh `sessions.attach` |
| Malformed inbound | Close `1002` |
| Oversize frame | Close `1009` |
| Heartbeat timeout | Close `1011` |
| Daemon shutdown | Close `1001` (going away) |

In-band errors ride the request/response envelope so callers correlate
to the originating call. Transport-level failures use Close frames so
the client knows to reconnect rather than retry the same request.

## Logging policy

- `error!`: VT terminal construction failure (`worker.rs:217, 229`).
- `warn!`: `child.start_kill()` failure on shutdown
  (`worker.rs:339-343`), `child.wait()` error before falling back to
  synthetic exit (`worker.rs:410`), `flush_pending_writes` PTY write
  failure (`worker.rs:441-444`).
- No log: lag-kicks, normal client disconnects, normal child exit.
  Routine events; per-occurrence logging would swamp the daemon under
  healthy multi-client use. Metric counterparts in [[observability]].

## Open questions

- **EIO vs. EOF vs. crash distinction.** `worker.rs:294-305` collapses
  read errors into the EOF path without preserving the original
  `io::Error`. Combined with `synthetic_exit_status(1)` from
  `worker.rs:505-508` being indistinguishable from a real `exit 1`,
  the daemon can't tell "child crashed" from "PTY went away" from
  "wait itself failed". A side-channel `ExitReason` enum alongside
  `ExitStatus` would unblock telemetry. ([[pty-lifecycle]] tracks the
  synthetic-exit half of this.)
- **Pre-exit `wait()` error budget.** If `child.wait()` flaps (rare
  but possible under fd-table corruption), the select keeps re-arming
  the branch. Cap retries?
- **Reconnect grace window.** Should the session registry hold a
  closing session for N seconds after the last subscriber drops, to
  absorb network blips without losing post-exit Subscribe capability?
  See [[daemon-process-model]] and [[configuration]].
- **Per-client flap detection.** A backgrounded tab flapping through
  lag-kick → reconnect → lag-kick is healthy session-side but bad UX.
  Should the transport track per-client reconnect rate and downgrade
  to "rate-limited" rather than re-kicking silently?
  ([[backpressure]] §"backgrounded-tab case".)
- **Panic safety.** A panic inside the `LocalSet` future tears the
  worker down invisibly. Wrap `run_session` with a panic hook so the
  daemon at least logs the location?
- **Init-failure shape.** All construction failures collapse to
  `PtyError::Backend` or `PtyError::Io`. A caller wanting to retry on
  `ENOMEM` but reject on `ENOENT` cannot do so without downcasting.
  Worth a `PtyError::Spawn { kind }` variant?

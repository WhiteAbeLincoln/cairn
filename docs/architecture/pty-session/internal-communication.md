# Internal Communication and Concurrency

How data and commands move *inside* a single cairn daemon process. The wire
protocol to clients lives in [[external-protocol]]; this document stops at the
WebSocket boundary and the public `PtySession` trait.

Scope: threading model, channel topology, single-writer invariants, and the
`Send`/`Sync` constraints that `libghostty-vt` imposes. Closely related:
[[pty-lifecycle]], [[terminal-state-and-replay]], [[resize-semantics]],
[[query-response-delegation]], [[backpressure]], [[daemon-process-model]].

## zmx baseline

zmx runs **one OS process per session**. `attach` calls `Daemon.ensureSession`
(`main.zig:738`), which `fork()`s on cache-miss (`main.zig:777`); the child
`setsid`s, `forkpty`s the user's shell (`main.zig:700`), and enters
`daemonLoop` (`main.zig:2460`). Routing between sessions is done **by the
filesystem**: each session owns a unique Unix socket under
`$XDG_RUNTIME_DIR/zmx/`, so there is no in-process "find the session" step
(`main.zig:572` documents this explicitly).

The daemon itself is single-threaded — one `posix.poll()` loop multiplexes
`server_sock_fd` (`main.zig:2481`), `pty_fd` (`main.zig:2491`), a
self-pipe for SIGTERM (`main.zig:2497`), and every connected client fd with
`POLL.OUT` gated on a per-client `has_pending_output` flag
(`main.zig:2499-2509`).

PTY → client distribution is a fan-out *append* into each client's
`write_buf: std.ArrayList(u8)` (`main.zig:2599-2608`), drained on `POLL.OUT`.
Client → PTY input is queued into `daemon.pty_write_buf` via `queuePtyInput`
(`main.zig:879`); the single PTY-writer invariant holds trivially because the
loop is the only writer. The libghostty `Terminal` lives on `daemonLoop`'s
stack (`main.zig:2469`); no other thread can reach it.

zmx pays for its simplicity with a process-per-session boundary and gets the
`!Send` discipline for free because there is only one thread.

## cairn: threading model

cairn embeds `libghostty-vt` in the same daemon process as the WebSocket
server, so it cannot hide behind `fork()`. The constraint that drives
everything is in `libghostty-vt/src/lib.rs:19-23`:

> All `libghostty-vt` objects are **not** thread-safe, and have been marked
> `!Send + !Sync` accordingly. The expectation is for them to be managed by a
> single thread, that may communicate with other threads via channels.

cairn picks the channel option. Each session gets a dedicated OS thread named
`cairn-pty-session` (`crates/cairn-pty/src/pty/ghostty/worker.rs:62`)
hosting a `current_thread` tokio runtime (`worker.rs:53-55`) running a
`LocalSet` inside `block_on` (`worker.rs:65-66`). The single-thread guarantee
makes `!Send` futures and `Rc<RefCell<…>>` legal inside the worker.

The handle exposed to the outside world is `GhosttyPty`
(`pty/ghostty/mod.rs:41`), which holds only `Send + Sync` channel endpoints
(`flume::Sender<Command>` and `watch::Receiver<Option<ExitStatus>>`,
`mod.rs:42-43`). The trait `PtySession` (`pty/session.rs:13`) is declared
`Send + Sync` so a single `Arc<dyn PtySession>` can be shared across the
daemon's tokio worker threads. A compile-time assertion at
`pty/mod.rs:140-143` pins this: any `!Send` field added to `GhosttyPty`
breaks the build.

## Channels and message types

Inside one session there are exactly three channel kinds:

| Direction | Channel | Purpose |
|-----------|---------|---------|
| outside → worker | `flume::unbounded::<Command>` (`worker.rs:33`) | Method calls become messages |
| worker → all subscribers | `tokio::sync::broadcast::<Bytes>` (`worker.rs:235`) | PTY-output fan-out |
| worker → callers awaiting exit | `tokio::sync::watch::<Option<ExitStatus>>` (`worker.rs:34`) | Latch-style "child is done" |

The command enum (`pty/ghostty/mod.rs:20-36`) covers every public trait method
plus shutdown:

```
Subscribe { reply: oneshot::Sender<Result<Subscription, PtyError>> }
Resize    { size, reply }
Size      { reply }
Write     { data, reply }
Shutdown
```

Each variant carries a `oneshot::Sender` for its reply, so the public async
trait impl (`mod.rs:87-122`) is uniform: send command, await oneshot. `flume`
is chosen over `tokio::sync::mpsc` because `send_async` works from any tokio
runtime and `recv_async` integrates with the `LocalSet` task without
requiring the channel itself to be local.

## Output fan-out: read loop to attached clients

The session task is a single `tokio::select!` (`worker.rs:265-416`) with
three arms: PTY readable (`s.pty.read`), external command
(`s.cmd_rx.recv_async`), and child exit (`s.child.wait`).

On the read arm, bytes from the kernel are (1) fed to the VT emulator via
`terminal.borrow_mut().vt_write(&chunk)` (`worker.rs:287`), updating screen
state used later by `subscribe`'s snapshot (see
[[terminal-state-and-replay]]); (2) broadcast to all current subscribers via
`tx.send(chunk)` (`worker.rs:288-290`); (3) followed by
`flush_pending_writes` (`worker.rs:292`), which drains any bytes the VT
emitted in response to a query (DA1/DSR/etc.) — see
[[query-response-delegation]].

Each `Subscribe` reply pairs a one-shot `snapshot: Bytes` (the formatted VT
state at subscribe time) with a fresh `broadcast::Receiver<Bytes>`
(`worker.rs:355-371`). The receiver yields *only* bytes broadcast strictly
after the snapshot — no duplication, no gap. Subscribe runs on the worker
thread, so it is atomic with respect to the next PTY read.

Contrast with zmx: zmx broadcasts by appending to per-client `write_buf`s
inside the same loop iteration that read the PTY. cairn delegates
per-subscriber buffering to `tokio::sync::broadcast`, a ring of size
`broadcast_capacity` (default 1024, `types.rs:36`) shared across receivers.
Slow subscribers observe `RecvError::Lagged(n)` and must resubscribe to
resync via a fresh snapshot (`pty/subscription.rs:13`). This pushes per-client
buffering off the worker thread; back-pressure consequences live in
[[backpressure]].

## Client input → PTY

`PtySession::write` (`session.rs:32`) sends `Command::Write { data, reply }`
to the worker (`mod.rs:114-121`). The cmd arm calls
`s.pty.write_all(&data).await` directly (`worker.rs:391-394`); no
intermediate queue.

This is the **single-writer-to-PTY invariant**. Both client input and
VT-emitted query responses are written by the same tokio task, sequentially:
client input via cmd arm → `pty.write_all`; query responses via
`terminal.vt_write`'s callback (`worker.rs:224-227`), which pushes into
`pending_writes`; the read arm drains via `flush_pending_writes`
(`worker.rs:435-446`) before yielding back to `select!`. Since
`tokio::select!` polls one arm at a time, these writes are serialized
without an explicit mutex. zmx achieves the same invariant via its single
poll loop and `pty_write_buf`.

The `Rc<RefCell<…>>` choices are deliberate, not lazy: `Terminal` is `!Send`
so it cannot live in an `Arc`, and the `LocalSet` guarantees no other task
runs concurrently with the worker — `borrow_mut()` panics could only fire if
`vt_write` recursed into itself (it doesn't). See the comment at
`worker.rs:204-206`. The pending-writes `VecDeque` is similarly local
because it only ever exists between a `vt_write` call and the next `await`
point on the same task.

## Backpressure inside the daemon

Three queues are on the hot path:

- **`flume::unbounded` command channel** (`worker.rs:33`): unbounded. Callers
  (WebSocket tasks) are themselves rate-limited by network reads, so runaway
  producers are unlikely. A bounded variant would force `write()` callers to
  block or fail; we currently choose blocking implicitly.
- **`broadcast::channel(capacity)`** (`worker.rs:235`): bounded ring, default
  1024 (`types.rs:36`). Overflow drops *oldest* unread bytes per lagging
  subscriber; subscribers recover by resubscribing.
- **`pending_writes: VecDeque<Bytes>`**: unbounded in theory, bounded in
  practice by VT query-response sizes (tens of bytes).

PTY-write back-pressure is delegated to the kernel: `s.pty.write_all().await`
suspends if the kernel buffer is full. zmx's analogue (`pty_write_buf` capped
at 256 KiB, `main.zig:872`) drops *new* bytes on overflow — a different
trade-off cairn does not make. See [[backpressure]] for the client edge.

## Daemon-level orchestration (speculative)

The current code ships only the per-session worker; the routing layer between
WebSocket connections and `GhosttyPty` instances is not yet in tree.
Speculating from zmx and the trait shape:

- A `SessionRegistry` (likely `dashmap` or
  `Arc<Mutex<HashMap<SessionId, Arc<dyn PtySession>>>>`) maps session IDs to
  handles. Because `Arc<dyn PtySession>` is `Send + Sync`, the lock is held
  only across map operations, never across PTY I/O.
- WebSocket handlers on the main tokio multi-thread runtime look up the
  handle, then call `subscribe()` / `write()` / `resize()` without holding
  the registry lock. The flume channel handles the cross-thread hop into
  the per-session worker.
- Session creation calls `GhosttyPty::spawn`, which blocks the caller until
  the worker thread has finished PTY setup (`worker.rs:166-179`) — matching
  zmx's behaviour where `ensureSession` does not return until the daemon is
  up.

zmx avoids this layer by handing routing to the filesystem — a luxury cairn
cannot share because all sessions must live in the WebSocket process so
browsers can attach. See [[daemon-process-model]].

## Send/Sync summary

| Type | Send/Sync | Lives where |
|------|-----------|-------------|
| `libghostty_vt::Terminal` | `!Send + !Sync` | worker thread only |
| `pending_writes: Rc<RefCell<VecDeque<Bytes>>>` | `!Send + !Sync` | worker thread only |
| `pty_process::Pty` | `Send` | worker thread (held by select) |
| `bcast_tx: broadcast::Sender<Bytes>` | `Send + Sync` | worker; clones in `Subscription` |
| `flume::Sender<Command>` | `Send + Sync` | every `GhosttyPty` |
| `watch::Receiver<Option<ExitStatus>>` | `Send + Sync` | every `GhosttyPty` |
| `GhosttyPty` | `Send + Sync` | freely shared via `Arc` |

The `!Send` types are exactly the ones whose APIs are inherently
single-threaded; every channel that crosses the thread boundary is `Send +
Sync`. The composition is sound because every `!Send` value is constructed
*inside* the worker thread (`worker.rs:210` for `Terminal`, `Rc::default()`
for `pending_writes`) and never escapes.

## Open Questions

- **Should the command channel be bounded?** Unbounded `flume` is simple but
  lets a misbehaving client grow worker memory. A bounded channel with
  `send_async` back-pressure would propagate naturally to WebSocket reads —
  but could deadlock if the worker is itself waiting on a slow
  `pty.write_all`. Pick one based on measured behaviour in [[backpressure]].
- **Where does the registry live?** `dashmap` keyed by session ID is the
  obvious answer, but cross-cutting concerns ([[authentication]],
  [[observability]], [[client-attach-and-election]]) may want a richer type.
- **Leader election inside the worker?** zmx tracks `leader_client_fd`
  (`main.zig:583`) so only the leader's resize / non-echo input applies.
  cairn treats all `Write`s identically and applies the last `Resize` (see
  [[resize-semantics]]). If we adopt leader election, state most likely
  lives in the registry layer with the worker remaining a "dumb" executor.
- **Snapshot atomicity vs. PtyWriteFn responses.** `format_snapshot` runs
  inside `borrow()`, not `borrow_mut()` (`worker.rs:356`). Any `vt_write`
  queued by formatting (none expected, worth confirming with libghostty
  maintainers) would deadlock on the `RefCell`.
- **Worker-thread panic propagation.** A panic unwinds the OS thread,
  dropping `cmd_rx`/`bcast_tx` and surfacing `PtyError::Closed` — but
  `exit_rx.watch` may never publish, so `wait()` falls back to a synthetic
  exit status (`mod.rs:78-81`). Should panic produce a distinct variant?
  See [[error-recovery]].

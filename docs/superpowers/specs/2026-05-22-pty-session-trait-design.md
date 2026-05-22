# PtySession Trait Design

**Status:** Approved design, ready for implementation planning
**Date:** 2026-05-22
**Location:** `crates/cairn-pty/src/pty.rs` (package: `cairn-types` per
current Cargo.toml — package rename appears to be in flight; spec uses
module path, not package name)

## Purpose

Provide a trait-based abstraction for wrapping interactive TUI programs
(claude-code's TUI, bash shells, vim, etc.) such that a cairn instance can:

- Render the TUI live in browser-based emulators (xterm.js, ghostty-web) over
  WebSockets, with multiple concurrent viewers per session.
- Forward keyboard input from any viewer back to the program.
- Provide a clean snapshot to late-joining clients (so they see current
  screen state without replaying the entire session).
- Answer terminal queries (DA1/DA2, DSR, DECRQM, OSC color queries, etc.)
  authoritatively, regardless of how many clients are connected.
- Scale to many concurrent sessions (low hundreds today; design supports
  growth via threading-model swap).

This spec covers only the PTY/terminal layer. Higher-level adapter
abstractions, the WebSocket layer, the bridge-protocol MITM, and frontend
work are out of scope.

## The Trait

```rust
use bytes::Bytes;
use tokio::sync::broadcast;

#[async_trait::async_trait]
pub trait PtySession: Send + Sync {
    /// Current terminal size in cells. Reports the kernel's TIOCGWINSZ value
    /// (what the child process actually sees).
    async fn size(&self) -> Result<TermSize, PtyError>;

    /// Resize the terminal grid. Updates the VT emulator's grid, calls
    /// Pty::resize (TIOCSWINSZ, which also delivers SIGWINCH to the child).
    /// Both happen atomically inside one command dispatch.
    /// Multi-client coordination is the caller's concern; last call wins.
    async fn resize(&self, size: TermSize) -> Result<(), PtyError>;

    /// Atomically: take a snapshot of current terminal state AND register
    /// a live stream of subsequent output.
    ///
    /// The snapshot is opaque VT escape bytes suitable for replay into any
    /// VT100/xterm-compatible emulator. The receiver yields bytes that
    /// arrived strictly after the snapshot was captured — no gap, no
    /// overlap. (This is the answer to the snapshot-vs-subscribe race.)
    async fn subscribe(&self) -> Result<Subscription, PtyError>;

    /// Write bytes to the PTY master (becomes the child's stdin).
    /// Concurrent calls from multiple tasks serialize at byte boundaries
    /// via the session's command channel.
    async fn write(&self, data: Bytes) -> Result<(), PtyError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

pub struct Subscription {
    /// VT escape sequence representing the terminal state at the moment of
    /// subscription. Format is opaque — clients should treat it as a byte
    /// stream to feed into their local emulator before processing live bytes.
    pub snapshot: Bytes,
    /// Live stream of subsequent PTY output.
    ///
    /// `RecvError::Lagged(_)` means the subscriber fell behind the broadcast
    /// capacity. To recover, drop the subscription and call `subscribe()`
    /// again — the new snapshot reflects current state and the new receiver
    /// starts clean.
    ///
    /// `RecvError::Closed` means the session has exited.
    pub stream: broadcast::Receiver<Bytes>,
}

#[derive(Debug, snafu::Snafu)]
pub enum PtyError {
    #[snafu(display("pty session has exited"))]
    Closed,
    #[snafu(display("pty io: {source}"))]
    Io { source: std::io::Error },
    #[snafu(display("terminal backend error: {source}"))]
    Backend {
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },
}

impl From<std::io::Error> for PtyError {
    fn from(source: std::io::Error) -> Self {
        Self::Io { source }
    }
}
```

### Design discipline: keep the trait backend-agnostic

The trait must remain implementable by alternative backends (test fakes,
future implementations we don't know about). Concrete rules:

- No libghostty-vt types in any method signature.
- No pty-process types in any method signature.
- `PtyError::Backend` is the only place implementor errors surface, and
  they're wrapped opaquely. Callers handle generically; advanced consumers
  can downcast if they need to.
- `Subscription::snapshot` is documented as opaque VT bytes, not as
  Formatter output.

## Concrete Implementation: GhosttyPty

Sole production implementor at launch. Uses:

- [`libghostty-vt`](https://docs.rs/libghostty-vt/0.1.1) for VT parsing,
  screen state, snapshot serialization, and authoritative responses to
  terminal queries.
- [`pty-process`](https://docs.rs/pty-process) (Unix-only) for PTY
  open/spawn with controlling-TTY setup and tokio-native async I/O.

### Lifecycle (not on the trait — concrete-only)

```rust
pub struct GhosttyPty { op_tx: flume::Sender<Op> }

pub struct SpawnOptions {
    pub command: std::process::Command,  // argv, env, cwd
    pub size: TermSize,
    pub broadcast_capacity: usize,       // default 1024
}

pub struct ExitHandle { rx: oneshot::Receiver<std::process::ExitStatus> }

impl GhosttyPty {
    pub fn spawn(opts: SpawnOptions) -> Result<(Self, ExitHandle), PtyError>;
    pub fn kill(&self) -> Result<(), PtyError>;
}

impl ExitHandle {
    pub async fn wait(self) -> std::process::ExitStatus;
}
```

`spawn` is sync because it spins up the session thread before returning;
no async work needed at construction. It returns the handle plus a
separate `ExitHandle` so a supervisor task can await child exit without
holding a reference to the trait object.

### Threading model

**One dedicated OS thread per session**, running a current-thread tokio
runtime with a `LocalSet`. All per-session work — PTY reads, PTY writes,
VT parsing, command dispatch, child wait — runs on this single thread as
cooperative tokio tasks/branches.

```
┌─────────────────────────────────────────────────────────────┐
│ Main tokio runtime (#cores worker threads, fixed)           │
│   ├── WS task for client 1A (session 1)                     │
│   ├── WS task for client 1B (session 1)                     │
│   ├── WS task for client 2A (session 2)                     │
│   └── …N WS tasks total, all on the shared workers          │
└──────────┬──────────────────┬───────────────────────────────┘
           │ flume op_tx      │ flume op_tx
           ▼                  ▼
   ┌──────────────────┐  ┌──────────────────┐
   │ Session thread 1 │  │ Session thread 2 │   …N session threads
   │ • current_thread │  │                  │
   │   tokio rt       │  │                  │
   │ • LocalSet       │  │                  │
   │ • Single task    │  │                  │
   │   select! over:  │  │                  │
   │   - pty.read     │  │                  │
   │   - op_rx        │  │                  │
   │   - child.wait   │  │                  │
   │ • Rc<RefCell<    │  │                  │
   │     Terminal>>   │  │                  │
   │ • broadcast::    │  │                  │
   │     Sender       │  │                  │
   │ • pty_process::  │  │                  │
   │     Pty + Child  │  │                  │
   └──────────────────┘  └──────────────────┘
```

**Total OS threads** = `#cores (tokio main) + N (sessions)`. WS clients are
tokio tasks (~600 bytes each) on the main runtime, not threads. At 200
active sessions on an 8-core box, ~208 threads — well within Linux/macOS
comfortable range.

### Why this works

- libghostty-vt's `Terminal`/`RenderState`/`Formatter` are `!Send + !Sync`,
  so they must be pinned to a single thread.
- A current-thread tokio runtime + `LocalSet` provides both async I/O and
  a `!Send`-friendly task executor on that thread.
- `pty_process::Pty` implements `AsyncRead`, `AsyncWrite`, and `AsRawFd`,
  so PTY reads and writes are native tokio futures (no dedicated I/O
  threads).
- `pty_process::Command::spawn(&pts)` returns a standard
  `tokio::process::Child`, whose `wait()` is reactor-driven (epoll on
  Linux via `pidfd`, kqueue `EVFILT_PROC` on macOS — both handled by
  tokio internally). No dedicated waiter thread.
- A single `select!` loop on the session thread multiplexes PTY readable,
  external commands, and child exit. Each branch's future is dropped when
  another branch wins, releasing borrows cleanly.
- Shared mutable state via `Rc<RefCell<Terminal>>` is safe because the
  task runs on one thread. `borrow_mut()` is held only across await-free
  sync blocks — no contention, no panic risk.

### Internal Op enum

```rust
enum Op {
    Subscribe { reply: oneshot::Sender<Result<Subscription, PtyError>> },
    Resize    { size: TermSize, reply: oneshot::Sender<Result<(), PtyError>> },
    Size      { reply: oneshot::Sender<Result<TermSize, PtyError>> },
    Write     { data: Bytes, reply: oneshot::Sender<Result<(), PtyError>> },
    Shutdown,
}
```

### Session-thread sketch

```rust
std::thread::Builder::new()
    .name("cairn-pty-session".into())
    .spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, run_session(opts, op_rx, exit_tx));
    })?;

async fn run_session(
    opts: SpawnOptions,
    op_rx: flume::Receiver<Op>,
    exit_tx: oneshot::Sender<std::process::ExitStatus>,
) {
    // 1. Open the PTY pair (Unix openpty under the hood)
    let (mut pty, pts) = pty_process::open()?;
    pty.resize(opts.size.into()).ok();

    // 2. Spawn the child attached to the slave; returns tokio::process::Child
    let mut child = pty_process::Command::from(opts.command).spawn(&pts)?;
    drop(pts);  // parent doesn't need the slave fd; child holds its own dup

    // 3. !Send VT state, broadcast tx, all live on this thread
    let terminal = Rc::new(RefCell::new(make_terminal(opts.size, /* PtyWriteFn */)?));
    let (bcast_tx, _) = broadcast::channel(opts.broadcast_capacity);
    let mut current_size = opts.size;

    // 4. Single task, three event sources via select!
    let mut buf = vec![0u8; 65536];
    let mut wait_fut = Box::pin(child.wait());

    loop {
        tokio::select! {
            // ── PTY readable
            res = pty.read(&mut buf) => match res {
                Ok(0) => {
                    // EOF — child closed slave. Drain exit status, then break.
                    if let Ok(status) = (&mut wait_fut).await {
                        let _ = exit_tx.send(status);
                    }
                    break;
                }
                Ok(n) => {
                    let chunk = Bytes::copy_from_slice(&buf[..n]);
                    terminal.borrow_mut().vt_write(&chunk).ok();
                    let _ = bcast_tx.send(chunk);
                }
                Err(_) => break,
            },

            // ── External command
            Ok(op) = op_rx.recv_async() => match op {
                Op::Subscribe { reply } => {
                    let snapshot = format_vt_snapshot(&terminal.borrow());
                    let _ = reply.send(Ok(Subscription {
                        snapshot,
                        stream: bcast_tx.subscribe(),
                    }));
                }
                Op::Resize { size, reply } => {
                    // cell_width_px / cell_height_px are 0 because we don't
                    // know the client's font metrics and they only matter for
                    // pixel-precise mouse reporting and graphics. Revisit if
                    // we add a way for clients to report cell pixel size.
                    let res = pty.resize(size.into())
                        .map_err(PtyError::Io)
                        .and_then(|()| terminal.borrow_mut()
                            .resize(size.cols, size.rows, 0, 0)
                            .map_err(|e| PtyError::Backend(Box::new(e))));
                    if res.is_ok() { current_size = size; }
                    let _ = reply.send(res);
                }
                Op::Size { reply } => {
                    // Cached from the most recent resize (or initial opts.size).
                    // pty_process::Pty doesn't expose a get_size() shortcut, and
                    // we always set the size ourselves, so caching is authoritative.
                    let _ = reply.send(Ok(current_size));
                }
                Op::Write { data, reply } => {
                    let res = pty.write_all(&data).await.map_err(PtyError::Io);
                    let _ = reply.send(res);
                }
                Op::Shutdown => { let _ = child.kill().await; break; }
            },

            // ── Child exited independently
            status = &mut wait_fut => {
                if let Ok(s) = status { let _ = exit_tx.send(s); }
                break;
            },
        }
    }

    // Teardown: dropping bcast_tx closes the broadcast — existing subscribers
    // observe RecvError::Closed. Dropping op_rx (via end-of-function) closes
    // the command channel — subsequent trait calls return PtyError::Closed.
}
```

`format_vt_snapshot` uses `libghostty_vt::fmt::Formatter` with `Format::VT`
to produce a self-contained escape stream representing current screen +
scrollback. Bounded by `cols × (rows + scrollback_rows)`.

### Why one task, not split read/write/dispatch tasks

`pty_process::Pty::into_split()` exists for parallel read+write across
tasks. We don't need it:

- Reads happen continuously (TUI emits constantly).
- Writes happen rarely (only when a viewer types or libghostty-vt
  generates a query response).
- During a write, the reader is briefly paused inside the `select!`. The
  kernel's PTY buffer holds incoming bytes; nothing is lost.

`select!` cancellation handles the borrow: when an op or child-wait
branch fires, the pending `pty.read(&mut buf)` future is dropped,
releasing its `&mut pty` borrow before any write or resize runs.

Avoiding split also sidesteps the awkward fact that `Pty::resize` is on
`&Pty` but `into_split()` consumes `Pty`. With split, you'd need to dup
the master fd before splitting and ioctl-resize through the dup.

### Terminal query handling

libghostty-vt's `terminal` module exposes callback traits for VT queries:

- `PtyWriteFn` — writes responses back into the PTY
- `EnquiryFn`, `DeviceAttributesFn`, `SizeFn`, `ColorSchemeFn`,
  `XtversionFn`, `TitleChanged`, `BellFn`

These are wired during `Terminal::new(...)` construction.

`PtyWriteFn` fires synchronously inside `terminal.vt_write()`, but
`pty.write_all` is async. The bridge is a small in-thread queue:

```rust
let pending_writes: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();
// PtyWriteFn closure pushes onto pending_writes.
// After each terminal.vt_write(&chunk), drain pending_writes
// and pty.write_all each one before returning to select!.
```

This means the session thread itself responds to queries — authoritative,
single response per query, regardless of how many viewers are connected.

This is the architectural reason `GhosttyPty` (not a raw-byte pipe) is the
right backend: query responses must come from the server-side terminal,
not from browser emulators. Browsers responding to queries would cause
duplicate responses with multiple clients, no response with zero clients,
and inconsistent behavior across emulator implementations.

## Subscription Mechanics

The snapshot-vs-subscribe race:

- `scrollback()` then `add_reader()` → bytes between calls are missed.
- `add_reader()` then `scrollback()` → bytes are duplicated.

Resolution: combined into one atomic `subscribe()` call. Inside the
session thread (single-threaded execution), the dispatcher:

1. Calls `Formatter::format_alloc` against the current `Terminal` state →
   `snapshot: Bytes`.
2. Calls `bcast_tx.subscribe()` → fresh `broadcast::Receiver`.
3. Returns both.

`tokio::sync::broadcast` guarantees the receiver only sees messages sent
*after* it was created. Since the snapshot and the subscribe happen on the
same thread with no `await` between them, there's no gap (no bytes arrive
between them) and no overlap (the receiver doesn't see anything the
snapshot already covered).

## Lifecycle and Process Death

Two paths into shutdown:

1. **Child exits on its own.** Either `pty.read` returns `Ok(0)` (EOF on
   master after the child closes the slave) or `child.wait()` resolves
   first. In either case the session task drains the exit status into
   `exit_tx` and breaks. Dropping `bcast_tx` closes the broadcast;
   existing subscribers observe `RecvError::Closed`.
2. **Caller invokes `kill()`.** Sends `Op::Shutdown`, which calls
   `child.kill().await` and breaks. Teardown is otherwise identical.

After teardown the session thread exits. Subsequent trait calls fail with
`PtyError::Closed` because `op_tx.send_async()` returns
`flume::SendError`.

`ExitHandle::wait` is a thin wrapper around the oneshot receiver, so
supervisors can `select!` on session exit alongside other work.

## PTY Backend: pty-process (Unix-only)

This is a change from the earlier `portable-pty` choice. The reason:

`portable-pty` forces three threads per session — a blocking PTY reader,
a blocking PTY writer, and a blocking child-waiter — because its
abstraction must accommodate Windows ConPTY's non-async surface. On Unix
that overhead is unnecessary: PTY master fds can be `O_NONBLOCK`, and
child exit can be polled via `pidfd`/`kqueue`.

`pty-process`:

- Unix-only (Linux + macOS + BSDs). Cairn doesn't target Windows.
- `Pty` implements `AsyncRead`, `AsyncWrite`, `AsRawFd`, `AsFd` — drops
  into tokio with no I/O threads.
- `Command::spawn(&pts)` returns a `tokio::process::Child` — drops into
  tokio with no waiter thread.
- `pty.resize(Size)` takes `&self`, doesn't conflict with active reads.

Per-session thread count drops from 3 (portable-pty) to 1 (pty-process).

### What we give up vs portable-pty

| portable-pty feature | Loss in pty-process | Impact |
|---|---|---|
| Windows / ConPTY backend | Gone | Cairn doesn't target Windows. |
| `MasterPty` trait + pluggable backends | Gone — single Unix backend | We never used the trait abstraction. |
| `CommandBuilder` with its own argv/env/cwd API | Replaced by `std::process::Command` directly | Wash — std::Command is more familiar. |
| Custom `Child` trait wrapping platform handles | Replaced by `tokio::process::Child` | Net win — async wait/kill native to tokio. |
| `try_clone_reader()` returning `Box<dyn Read>` | Replaced by `AsyncRead`/`AsyncWrite` on `Pty` | Net win — typed I/O. |

No genuine losses given the Unix-only constraint.

### Alternatives considered

- **`portable-pty`** — initial choice. Rejected after discovering the
  3-thread cost.
- **`nix::pty` + manual fork/exec + ioctls** — would also collapse to 1
  thread, but requires hand-rolling the controlling-TTY dance
  (`setsid`, `TIOCSCTTY`, `dup2` of slave fd into stdin/stdout/stderr)
  and async-signal-safe fork-exec. `pty-process` packages this correctly.
  Rejected as YAGNI.

## Out of Scope

Items deliberately excluded from this design:

- **Multi-client resize coordination** (min-of-all, controller-client,
  etc.) — trait is policy-free; coordination lives in a higher abstraction
  if/when needed.
- **Writer arbitration** beyond byte-boundary serialization — no writer
  lock, no takeover. Multiple tabs typing concurrently is acceptable.
- **Higher-level "agent harness adapter" trait** distinguishing TUI
  adapters from headless adapters (claude headless, PI RPC, etc., which
  use `tokio::process::Command` directly, not `PtySession`).
- **Session persistence / resume across cairn restarts** — sessions die
  when the process dies.
- **Bridge-protocol integration** (claude-code MITM) — separate concern;
  `PtySession` is just the TUI wrapper.
- **Session recording / asciinema replay** — could be added later as a
  tee on the reader branch without changing the trait.
- **Frontend / WebSocket layer** — outside cairn-pty.
- **Shared-thread session pool** — designed in (1-thread-per-session is
  an implementation detail of `GhosttyPty`, the trait doesn't constrain
  it), but not implemented. Migration path if scale demands it: hash
  sessions to a fixed pool of `LocalSet`-hosting threads.
- **Windows support** — explicitly out. WSL2 users get Linux semantics
  via the Linux kernel.

## Migration / Future Considerations

- If session count exceeds ~thousands per process, sharding into a fixed
  actor pool (sessions distributed across M threads by session-id hash)
  is the next step. Trait surface doesn't change; only
  `GhosttyPty::spawn`'s internals do.
- If libghostty-vt grows a `Send` Terminal type in a future version, the
  threading model can be simplified accordingly (the LocalSet can be
  removed; tasks become normal tokio tasks).
- A future adapter trait may sit above both `PtySession` (for TUI
  adapters) and direct `tokio::process::Command` use (for headless
  adapters), abstracting both as "agent harness adapters."
- If Windows support ever becomes a requirement, the path is: introduce
  a `WindowsPty` impl behind the same trait, using ConPTY directly or
  reintroducing `portable-pty` for that backend only. The trait is
  designed to admit this.

## Cargo Dependencies

Already present in `crates/cairn-pty/Cargo.toml`:

- `libghostty-vt = "0.1.1"`
- `tokio = { version = "1.52", features = ["full"] }` (`full` includes
  `process`, `net`, `io-util` — all needed)

Already present via workspace inheritance:

- `snafu` (workspace error convention — used for `PtyError`)

New dependencies to add:

```toml
async-trait = "0.1"
bytes = "1"
flume = "0.12"           # sync↔async channel for command dispatch
pty-process = { version = "0.4", features = ["async"] }
```

Removed (was in the earlier portable-pty plan):

- `portable-pty` — replaced by `pty-process` per the rationale above.

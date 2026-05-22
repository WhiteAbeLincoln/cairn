# PtySession Trait Design

**Status:** Approved design, ready for implementation planning
**Date:** 2026-05-22
**Location:** `crates/cairn-core/src/pty.rs` (package: `cairn-types` per
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
    /// MasterPty::resize (TIOCSWINSZ, which also delivers SIGWINCH to the
    /// child). All three happen atomically inside one command dispatch.
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

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("pty session has exited")]
    Closed,
    #[error("pty io: {0}")]
    Io(#[from] std::io::Error),
    #[error("terminal backend error: {0}")]
    Backend(Box<dyn std::error::Error + Send + Sync + 'static>),
}
```

### Design discipline: keep the trait backend-agnostic

The trait must remain implementable by alternative backends (test fakes,
future implementations we don't know about). Concrete rules:

- No libghostty-vt types in any method signature.
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
- [`portable-pty`](https://docs.rs/portable-pty) for cross-platform PTY
  spawning and child process management.

### Lifecycle (not on the trait — concrete-only)

```rust
pub struct GhosttyPty { cmd_tx: flume::Sender<Command> }

pub struct SpawnOptions {
    pub command: std::process::Command,  // argv, env, cwd
    pub size: TermSize,
    pub broadcast_capacity: usize,       // default 1024
}

impl GhosttyPty {
    pub fn spawn(opts: SpawnOptions) -> Result<Self, PtyError>;
    pub async fn wait(&self) -> ExitStatus;
    pub fn kill(&self) -> Result<(), PtyError>;
}
```

`spawn` is sync because it spins up the session thread before returning;
no async work needed at construction.

### Threading model

**One dedicated OS thread per session**, running a current-thread tokio
runtime with a `LocalSet`. This collapses what would otherwise be a
PTY-reader thread and a VT-actor thread into one.

```
┌─────────────────────────────────────────────────────────────┐
│ Main tokio runtime (#cores worker threads, fixed)           │
│   ├── WS task for client 1A (session 1)                     │
│   ├── WS task for client 1B (session 1)                     │
│   ├── WS task for client 2A (session 2)                     │
│   └── …N WS tasks total, all on the shared workers          │
└──────────┬──────────────────┬───────────────────────────────┘
           │ flume cmd_tx     │ flume cmd_tx
           ▼                  ▼
   ┌──────────────────┐  ┌──────────────────┐
   │ Session thread 1 │  │ Session thread 2 │   …N session threads
   │ • current_thread │  │                  │
   │   tokio rt       │  │                  │
   │ • LocalSet:      │  │                  │
   │   - PTY reader   │  │                  │
   │     (AsyncFd)    │  │                  │
   │   - Cmd loop     │  │                  │
   │ • Rc<RefCell<    │  │                  │
   │     Terminal>>   │  │                  │
   │ • broadcast::    │  │                  │
   │     Sender       │  │                  │
   │ • MasterPty +    │  │                  │
   │     child handle │  │                  │
   └──────────────────┘  └──────────────────┘
```

**Total OS threads** = `#cores (tokio main) + N (sessions)`. WS clients are
tokio tasks (~600 bytes each) on the main runtime, not threads. At 200
active sessions on an 8-core box, ~208 threads — well within Linux/macOS
comfortable range.

### Why this works

- libghostty-vt's `Terminal`/`RenderState`/`Formatter` are `!Send + !Sync`,
  so they must be pinned to a single thread.
- A current-thread tokio runtime + `LocalSet` provides both async I/O
  (`AsyncFd` for the PTY) and a `!Send`-friendly task executor on that
  thread.
- The PTY reader task and the command dispatcher task both run on the same
  OS thread. When the reader awaits `readable()`, the dispatcher runs.
  When the dispatcher awaits a command, the reader runs.
- Shared mutable state via `Rc<RefCell<Terminal>>` is safe because both
  tasks execute on one thread. `borrow_mut()` is held only across
  await-free sync blocks — no contention, no panic risk.

### Internal Command enum

```rust
enum Command {
    Subscribe { reply: oneshot::Sender<Result<Subscription, PtyError>> },
    Resize    { size: TermSize, reply: oneshot::Sender<Result<(), PtyError>> },
    Size      { reply: oneshot::Sender<Result<TermSize, PtyError>> },
    Write     { data: Bytes, reply: oneshot::Sender<Result<(), PtyError>> },
    Shutdown,
}
```

### Session-thread sketch

```rust
std::thread::spawn(move || {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()?;
    let local = tokio::task::LocalSet::new();

    local.block_on(&rt, async move {
        let terminal = Rc::new(RefCell::new(Terminal::new(opts)?));
        let (bcast_tx, _) = broadcast::channel(opts.broadcast_capacity);
        let bcast_tx = Rc::new(bcast_tx);

        // PTY reader as a local !Send task
        let t = terminal.clone();
        let tx = bcast_tx.clone();
        tokio::task::spawn_local(async move {
            let async_fd = AsyncFd::new(pty_pair.master.as_raw_fd())?;
            let mut buf = vec![0u8; 65536];
            loop {
                let mut g = async_fd.readable().await?;
                let n = match g.try_io(|fd| read(fd.as_raw_fd(), &mut buf)) {
                    Ok(Ok(n)) if n > 0 => n,
                    Ok(Ok(_)) => break,        // EOF
                    Ok(Err(e)) => return Err(e),
                    Err(_) => continue,         // WouldBlock
                };
                let chunk = Bytes::copy_from_slice(&buf[..n]);
                t.borrow_mut().vt_write(&chunk)?;
                let _ = tx.send(chunk);
            }
            Ok::<_, io::Error>(())
        });

        // Command dispatcher
        while let Ok(cmd) = cmd_rx.recv_async().await {
            match cmd {
                Command::Subscribe { reply } => {
                    let snapshot = format_vt_snapshot(&terminal.borrow())?;
                    let stream = bcast_tx.subscribe();
                    let _ = reply.send(Ok(Subscription { snapshot, stream }));
                }
                Command::Resize { size, reply } => {
                    // cell_width_px / cell_height_px are 0 because we don't
                    // know the client's font metrics and they only matter for
                    // pixel-precise mouse reporting and graphics. Revisit if
                    // we add a way for clients to report cell pixel size.
                    terminal.borrow_mut().resize(size.cols, size.rows, 0, 0)?;
                    pty_pair.master.resize(PtySize { cols: size.cols, rows: size.rows, ..Default::default() })?;
                    let _ = reply.send(Ok(()));
                }
                Command::Size { reply } => {
                    let pty_size = pty_pair.master.get_size()?;
                    let _ = reply.send(Ok(TermSize { cols: pty_size.cols, rows: pty_size.rows }));
                }
                Command::Write { data, reply } => {
                    let res = pty_writer.write_all(&data).map_err(Into::into);
                    let _ = reply.send(res);
                }
                Command::Shutdown => break,
            }
        }
        // Teardown: drop bcast_tx (subscribers get Closed),
        // close cmd channel, kill child, exit thread.
    });
});
```

`format_vt_snapshot` uses `libghostty_vt::fmt::Formatter` with `Format::VT`
to produce a self-contained escape stream representing current screen +
scrollback. Bounded by `cols × (rows + scrollback_rows)`.

### Terminal query handling

libghostty-vt's `terminal` module exposes callback traits for VT queries:

- `PtyWriteFn` — writes responses back into the PTY
- `EnquiryFn`, `DeviceAttributesFn`, `SizeFn`, `ColorSchemeFn`,
  `XtversionFn`, `TitleChanged`, `BellFn`

These are wired during `Terminal::new(...)` construction. `PtyWriteFn`
gets a handle that writes to the session's PTY master writer (same channel
as `Command::Write`). This means the session thread itself responds to
queries — authoritative, single response per query, regardless of how many
viewers are connected.

This is the architectural reason `GhosttyPty` (not a raw-byte pipe) is the
right backend: query responses must come from the server-side terminal,
not from browser emulators. Browsers responding to queries would cause
duplicate responses with multiple clients, no response with zero clients,
and inconsistent behavior across emulator implementations.

## Subscription Mechanics

The snapshot-vs-subscribe race the user flagged:
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
same thread with no interleaving, there's no gap (no bytes arrive between
them) and no overlap (the receiver doesn't see anything the snapshot
already covered).

## Lifecycle and Process Death

When the child exits, the PTY master returns EOF on read. The reader task:

1. Stops looping.
2. Drops `bcast_tx` (existing subscribers get `RecvError::Closed`).
3. Signals the dispatcher to break out (close the command channel).
4. Signals `wait()` with the child's `ExitStatus`.

Subsequent calls to trait methods return `PtyError::Closed`.

`kill()` sends `Command::Shutdown`, which terminates the child and
triggers the same teardown.

## PTY Backend Choice: `portable-pty`

Rationale:

- Cross-platform (Linux, macOS, Windows via ConPTY). Cairn runs on
  developer machines and servers; cross-platform is cheap insurance.
- Mature and well-maintained (used by wezterm, zellij).
- Owns child process spawning via `MasterPty::spawn_command`.
- Exposes `as_raw_fd()` on Unix, enabling `AsyncFd` integration.
- Provides `get_size()`, `resize()` — no need to cache size ourselves.

Alternative considered: `nix::pty`. Lower-level, Unix-only, more manual
child management. Rejected for cross-platform reasons.

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
- **Session recording / asciinema replay** — could be added later as a tee
  on the reader task without changing the trait.
- **Frontend / WebSocket layer** — outside cairn-core.
- **Shared-thread session pool** — designed in (1-thread-per-session is an
  implementation detail of `GhosttyPty`, the trait doesn't constrain it),
  but not implemented. Migration path if scale demands it: hash sessions
  to a fixed pool of `LocalSet`-hosting threads.

## Migration / Future Considerations

- If session count exceeds ~thousands per process, sharding into a fixed
  actor pool (sessions distributed across M threads by session-id hash) is
  the next step. Trait surface doesn't change; only `GhosttyPty::spawn`'s
  internals do.
- If libghostty-vt grows a `Send` Terminal type in a future version, the
  threading model can be simplified accordingly.
- A future adapter trait may sit above both `PtySession` (for TUI
  adapters) and direct `tokio::process::Command` use (for headless
  adapters), abstracting both as "agent harness adapters."

## Cargo Dependencies

Already present in `crates/cairn-core/Cargo.toml`:
- `libghostty-vt = "0.1.1"`
- `tokio = { version = "1.52", features = ["full"] }` (`full` includes
  `net`, which provides `AsyncFd` on Unix)

New dependencies to add:
```toml
async-trait = "0.1"
bytes = "1"
flume = "0.12"           # sync↔async channel for command dispatch
portable-pty = "0.9"     # PTY abstraction + child spawn
thiserror = "2"
```

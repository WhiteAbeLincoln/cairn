# PtySession Trait Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the `PtySession` trait + the `GhosttyPty` concrete implementation in `crates/cairn-core/`, per `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`.

**Architecture:** Single dedicated OS thread per session running a current-thread tokio runtime + `LocalSet`. The thread hosts the `!Send` libghostty-vt `Terminal`, a PTY reader task using `AsyncFd`, and a command dispatcher task. External callers hold a `Send + Sync` `Arc<GhosttyPty>` and talk to the worker via a `flume` channel.

**Tech Stack:** Rust 2024, tokio 1.52 (current-thread runtime + LocalSet), `libghostty-vt = 0.1.1`, `portable-pty = 0.9`, `flume = 0.12`, `async-trait`, `bytes`, `snafu` (workspace error convention).

---

## File Structure

```
crates/cairn-core/
├── Cargo.toml                    (modified: add deps)
├── src/
│   ├── lib.rs                    (modified: re-export pty surface)
│   └── pty/
│       ├── mod.rs                (new: declare submodules, public re-exports — REPLACES existing pty.rs)
│       ├── error.rs              (new: PtyError)
│       ├── types.rs              (new: TermSize, SpawnOptions)
│       ├── subscription.rs       (new: Subscription)
│       ├── session.rs            (new: PtySession trait)
│       ├── ghostty/
│       │   ├── mod.rs            (new: GhosttyPty + Command enum + public API)
│       │   └── worker.rs         (new: session thread bootstrap, reader task, dispatcher loop)
└── tests/
    ├── pty_lifecycle.rs          (new: spawn / wait / kill integration)
    ├── pty_io.rs                 (new: subscribe / write / scrollback integration)
    └── pty_resize.rs             (new: resize semantics)
```

The existing placeholder file `crates/cairn-core/src/pty.rs` (currently `trait PtySession {}`) gets deleted in Task 2 and replaced by the `pty/` directory with `mod.rs`.

**Notes on the package name:** the package is currently `cairn-types` (in `crates/cairn-core/`). Use `cargo test -p cairn-types` and `cargo build -p cairn-types` throughout this plan.

---

## Phase 1 — Dependencies and Type Foundations

### Task 1: Add dependencies

**Files:**
- Modify: `crates/cairn-core/Cargo.toml`

- [ ] **Step 1: Add new dependencies**

Replace the `[dependencies]` block in `crates/cairn-core/Cargo.toml` with:

```toml
[dependencies]
serde.workspace = true
chrono.workspace = true
serde_json.workspace = true
snafu.workspace = true
tracing.workspace = true
libghostty-vt = { version = "0.1.1" }
tokio = { version = "1.52", features = ["full"] }
async-trait = "0.1"
bytes = "1"
flume = "0.12"
portable-pty = "0.9"

[dev-dependencies]
tokio = { version = "1.52", features = ["full", "test-util", "macros"] }
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p cairn-types`
Expected: clean build, all new crates downloaded and compiled.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/Cargo.toml Cargo.lock
git commit -m "Add deps for PtySession trait (portable-pty, flume, async-trait, bytes)"
```

---

### Task 2: PtyError type + module layout switch

**Files:**
- Delete: `crates/cairn-core/src/pty.rs` (the existing placeholder)
- Create: `crates/cairn-core/src/pty/mod.rs`
- Create: `crates/cairn-core/src/pty/error.rs`

- [ ] **Step 1: Delete the existing placeholder file**

```bash
git rm crates/cairn-core/src/pty.rs
```

- [ ] **Step 2: Create the module entry with failing tests**

Create `crates/cairn-core/src/pty/mod.rs`:

```rust
//! Pseudo-terminal session abstraction.
//!
//! See `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`.

mod error;

pub use error::PtyError;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn error_from_io_error() {
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "boom");
        let err: PtyError = io_err.into();
        assert!(matches!(err, PtyError::Io { .. }));
    }

    #[test]
    fn error_closed_is_constructible() {
        let err = PtyError::Closed;
        assert_eq!(format!("{err}"), "pty session has exited");
    }
}
```

- [ ] **Step 3: Run the test (expect failure)**

Run: `cargo test -p cairn-types pty::tests`
Expected: FAIL — `PtyError` does not exist (module not yet created).

- [ ] **Step 4: Create the error module**

Create `crates/cairn-core/src/pty/error.rs`:

```rust
use snafu::Snafu;

/// Errors surfaced by a [`crate::pty::PtySession`].
///
/// `Backend` is an opaque escape hatch for implementor-specific errors
/// (e.g. libghostty-vt's `error::Error`). Callers handle generically;
/// advanced consumers can downcast via the inner trait object.
#[derive(Debug, Snafu)]
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

- [ ] **Step 5: Run the test (expect pass)**

Run: `cargo test -p cairn-types pty::tests`
Expected: PASS (both tests green).

- [ ] **Step 6: Commit**

```bash
git add -A crates/cairn-core/src/pty.rs crates/cairn-core/src/pty/
git commit -m "Switch pty to module directory layout; add PtyError"
```

---

### Task 3: TermSize and SpawnOptions

**Files:**
- Create: `crates/cairn-core/src/pty/types.rs`
- Modify: `crates/cairn-core/src/pty/mod.rs`

- [ ] **Step 1: Write failing tests**

Replace the `#[cfg(test)] mod tests` block in `crates/cairn-core/src/pty/mod.rs` with:

```rust
mod error;
mod types;

pub use error::PtyError;
pub use types::{SpawnOptions, TermSize};

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn error_from_io_error() {
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "boom");
        let err: PtyError = io_err.into();
        assert!(matches!(err, PtyError::Io { .. }));
    }

    #[test]
    fn error_closed_is_constructible() {
        let err = PtyError::Closed;
        assert_eq!(format!("{err}"), "pty session has exited");
    }

    #[test]
    fn termsize_is_copy_and_eq() {
        let a = TermSize { cols: 80, rows: 24 };
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn spawn_options_default_capacity() {
        let opts = SpawnOptions::new(std::process::Command::new("true"));
        assert_eq!(opts.broadcast_capacity, 1024);
        assert_eq!(opts.size, TermSize { cols: 80, rows: 24 });
    }

    #[test]
    fn spawn_options_builder_size() {
        let opts = SpawnOptions::new(std::process::Command::new("true"))
            .with_size(TermSize { cols: 120, rows: 40 });
        assert_eq!(opts.size, TermSize { cols: 120, rows: 40 });
    }

    #[test]
    fn spawn_options_builder_capacity() {
        let opts = SpawnOptions::new(std::process::Command::new("true"))
            .with_broadcast_capacity(64);
        assert_eq!(opts.broadcast_capacity, 64);
    }
}
```

- [ ] **Step 2: Run tests (expect failure)**

Run: `cargo test -p cairn-types pty::tests`
Expected: FAIL — `TermSize` and `SpawnOptions` not defined.

- [ ] **Step 3: Create the types module**

Create `crates/cairn-core/src/pty/types.rs`:

```rust
/// Terminal grid size in cells. Matches the kernel TIOCGWINSZ representation
/// of cols (width) and rows (height).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

/// Options for spawning a new PTY session.
///
/// Construct via [`SpawnOptions::new`] with a configured [`std::process::Command`].
/// `std::process::Command` (not `tokio::process::Command`) because
/// `portable-pty::SlavePty::spawn_command` expects the std variant.
pub struct SpawnOptions {
    pub command: std::process::Command,
    pub size: TermSize,
    pub broadcast_capacity: usize,
}

impl SpawnOptions {
    pub fn new(command: std::process::Command) -> Self {
        Self {
            command,
            size: TermSize::default(),
            broadcast_capacity: 1024,
        }
    }

    pub fn with_size(mut self, size: TermSize) -> Self {
        self.size = size;
        self
    }

    pub fn with_broadcast_capacity(mut self, capacity: usize) -> Self {
        self.broadcast_capacity = capacity;
        self
    }
}
```

- [ ] **Step 4: Run tests (expect pass)**

Run: `cargo test -p cairn-types pty::tests`
Expected: PASS (all six tests green).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/mod.rs crates/cairn-core/src/pty/types.rs
git commit -m "Add TermSize and SpawnOptions with builder API"
```

---

### Task 4: Subscription type

**Files:**
- Create: `crates/cairn-core/src/pty/subscription.rs`
- Modify: `crates/cairn-core/src/pty/mod.rs`

- [ ] **Step 1: Write failing test**

Update `crates/cairn-core/src/pty/mod.rs` — replace the `mod`/`pub use` lines and append a test:

```rust
mod error;
mod subscription;
mod types;

pub use error::PtyError;
pub use subscription::Subscription;
pub use types::{SpawnOptions, TermSize};

#[cfg(test)]
mod tests {
    // ... keep existing tests above ...

    #[test]
    fn subscription_constructs_from_parts() {
        use bytes::Bytes;
        use tokio::sync::broadcast;

        let (tx, rx) = broadcast::channel::<Bytes>(4);
        let snap = Bytes::from_static(b"\x1b[2J");
        let sub = Subscription {
            snapshot: snap.clone(),
            stream: rx,
        };
        assert_eq!(sub.snapshot, snap);
        drop(tx); // explicit drop so test asserts type accepts a Receiver
    }
}
```

(Keep all the existing tests from Task 3 above this new one.)

- [ ] **Step 2: Run tests (expect failure)**

Run: `cargo test -p cairn-types pty::tests::subscription_constructs_from_parts`
Expected: FAIL — `Subscription` does not exist.

- [ ] **Step 3: Create the subscription module**

Create `crates/cairn-core/src/pty/subscription.rs`:

```rust
use bytes::Bytes;
use tokio::sync::broadcast;

/// Result of a successful [`crate::pty::PtySession::subscribe`] call.
///
/// `snapshot` is an opaque VT escape sequence representing the terminal
/// state at the moment of subscription. Feed it to a VT100/xterm-compatible
/// emulator (xterm.js, ghostty-web, etc.) before processing `stream` bytes.
///
/// `stream` yields bytes that arrived strictly *after* the snapshot was
/// captured — no gap, no overlap. `broadcast::error::RecvError::Lagged(_)`
/// means the subscriber fell behind the broadcast capacity; recover by
/// dropping this `Subscription` and calling `subscribe()` again — the new
/// snapshot reflects current state and the new receiver starts clean.
/// `RecvError::Closed` means the session has exited.
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
}
```

- [ ] **Step 4: Run tests (expect pass)**

Run: `cargo test -p cairn-types pty::tests`
Expected: PASS (all tests green).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/mod.rs crates/cairn-core/src/pty/subscription.rs
git commit -m "Add Subscription type bundling snapshot + broadcast receiver"
```

---

## Phase 2 — The Trait

### Task 5: PtySession trait

**Files:**
- Create: `crates/cairn-core/src/pty/session.rs`
- Modify: `crates/cairn-core/src/pty/mod.rs`

- [ ] **Step 1: Write failing test**

Update `crates/cairn-core/src/pty/mod.rs`:

```rust
mod error;
mod session;
mod subscription;
mod types;

pub use error::PtyError;
pub use session::PtySession;
pub use subscription::Subscription;
pub use types::{SpawnOptions, TermSize};
```

Add a new integration-style unit test at the bottom of the `tests` module:

```rust
    #[test]
    fn pty_session_is_object_safe() {
        // Compile-time check that PtySession is object-safe.
        // (If the trait grows generic methods or returns Self by value,
        // this line will fail to compile.)
        fn _assert_dyn(_: &dyn PtySession) {}
    }

    struct StubSession;

    #[async_trait::async_trait]
    impl PtySession for StubSession {
        async fn size(&self) -> Result<TermSize, PtyError> {
            Ok(TermSize { cols: 1, rows: 1 })
        }
        async fn resize(&self, _: TermSize) -> Result<(), PtyError> {
            Ok(())
        }
        async fn subscribe(&self) -> Result<Subscription, PtyError> {
            use bytes::Bytes;
            use tokio::sync::broadcast;
            let (_tx, rx) = broadcast::channel(1);
            Ok(Subscription {
                snapshot: Bytes::new(),
                stream: rx,
            })
        }
        async fn write(&self, _: bytes::Bytes) -> Result<(), PtyError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stub_session_implements_trait() {
        let s = StubSession;
        let size = s.size().await.unwrap();
        assert_eq!(size, TermSize { cols: 1, rows: 1 });
    }
```

- [ ] **Step 2: Run tests (expect failure)**

Run: `cargo test -p cairn-types pty::tests`
Expected: FAIL — `PtySession` not defined.

- [ ] **Step 3: Create the trait**

Create `crates/cairn-core/src/pty/session.rs`:

```rust
use bytes::Bytes;

use super::{PtyError, Subscription, TermSize};

/// A live pseudo-terminal session wrapping a child process.
///
/// Implementations are `Send + Sync` so they can be shared across many
/// async tasks (e.g. WebSocket handlers, each holding `Arc<dyn PtySession>`).
///
/// See `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`
/// for the design rationale.
#[async_trait::async_trait]
pub trait PtySession: Send + Sync {
    /// Current terminal size in cells. Reports the kernel's TIOCGWINSZ value
    /// (what the child process actually sees).
    async fn size(&self) -> Result<TermSize, PtyError>;

    /// Resize the terminal grid. Updates the VT emulator's grid and the
    /// kernel-level PTY size (TIOCSWINSZ, which delivers SIGWINCH to the
    /// child). All updates happen atomically inside one command dispatch.
    /// Multi-client coordination is the caller's concern; last call wins.
    async fn resize(&self, size: TermSize) -> Result<(), PtyError>;

    /// Atomically take a snapshot of current terminal state AND register
    /// a live stream of subsequent output. See [`Subscription`] for
    /// the contract on what the returned snapshot and stream represent.
    async fn subscribe(&self) -> Result<Subscription, PtyError>;

    /// Write bytes to the PTY master (becomes the child's stdin).
    /// Concurrent calls from multiple tasks serialize at byte boundaries
    /// via the session's command channel.
    async fn write(&self, data: Bytes) -> Result<(), PtyError>;
}
```

- [ ] **Step 4: Run tests (expect pass)**

Run: `cargo test -p cairn-types pty::tests`
Expected: PASS (all tests green).

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/mod.rs crates/cairn-core/src/pty/session.rs
git commit -m "Add PtySession async trait with size/resize/subscribe/write"
```

---

## Phase 3 — GhosttyPty Skeleton and Process Spawn

### Task 6: GhosttyPty struct and Command enum

**Files:**
- Create: `crates/cairn-core/src/pty/ghostty/mod.rs`
- Modify: `crates/cairn-core/src/pty/mod.rs`

- [ ] **Step 1: Write failing test**

Update `crates/cairn-core/src/pty/mod.rs` to add the ghostty module and re-export:

```rust
mod error;
mod ghostty;
mod session;
mod subscription;
mod types;

pub use error::PtyError;
pub use ghostty::GhosttyPty;
pub use session::PtySession;
pub use subscription::Subscription;
pub use types::{SpawnOptions, TermSize};
```

Append a test to the existing `tests` module:

```rust
    #[test]
    fn ghostty_pty_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GhosttyPty>();
    }
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types pty::tests::ghostty_pty_is_send_sync`
Expected: FAIL — `GhosttyPty` not defined.

- [ ] **Step 3: Create the ghostty module with placeholder struct**

Create `crates/cairn-core/src/pty/ghostty/mod.rs`:

```rust
//! `libghostty-vt`-backed [`PtySession`] implementation.
//!
//! Runs one dedicated OS thread per session hosting a current-thread tokio
//! runtime + `LocalSet`. The thread owns the `!Send` `Terminal`, the PTY
//! master fd, and the broadcast sender. External callers reach it via a
//! `flume` command channel.
//!
//! [`PtySession`]: super::PtySession

mod worker;

use bytes::Bytes;
use tokio::sync::{broadcast, oneshot};

use super::{PtyError, Subscription, TermSize};

/// Commands the public API sends to the session worker thread.
pub(super) enum Command {
    Subscribe {
        reply: oneshot::Sender<Result<Subscription, PtyError>>,
    },
    Resize {
        size: TermSize,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Size {
        reply: oneshot::Sender<Result<TermSize, PtyError>>,
    },
    Write {
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Shutdown,
}

/// Handle to a running PTY session.
///
/// Construct via [`GhosttyPty::spawn`]. Cheap to clone via `Arc`; the
/// session keeps running until the child exits or [`GhosttyPty::kill`]
/// is called.
pub struct GhosttyPty {
    cmd_tx: flume::Sender<Command>,
}

impl GhosttyPty {
    // Lifecycle methods filled in in later tasks.
}
```

Create the empty worker submodule file `crates/cairn-core/src/pty/ghostty/worker.rs`:

```rust
//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

// (Implementation lands in later tasks.)
```

- [ ] **Step 4: Run test (expect pass)**

Run: `cargo test -p cairn-types pty::tests::ghostty_pty_is_send_sync`
Expected: PASS — `flume::Sender` is `Send + Sync`, so `GhosttyPty` inherits it.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/mod.rs crates/cairn-core/src/pty/ghostty/mod.rs crates/cairn-core/src/pty/ghostty/worker.rs
git commit -m "Add GhosttyPty skeleton with Command enum and worker module stub"
```

---

### Task 7: Spawn a child process (proof of life)

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/mod.rs`
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Create: `crates/cairn-core/tests/pty_lifecycle.rs`

This task does the minimum to spawn a child via `portable-pty` and `wait()` for it. No reader task, no Terminal, no commands yet — just proving the worker thread bootstraps and a child exits.

- [ ] **Step 1: Write failing integration test**

Create `crates/cairn-core/tests/pty_lifecycle.rs`:

```rust
//! Integration tests for GhosttyPty spawn / wait / kill lifecycle.

use cairn_types::pty::{GhosttyPty, SpawnOptions, TermSize};

#[tokio::test]
async fn spawn_true_exits_cleanly() {
    let cmd = std::process::Command::new("true");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let status = pty.wait().await;
    assert!(status.success(), "expected `true` to exit 0, got {:?}", status);
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_lifecycle`
Expected: FAIL — `GhosttyPty::spawn` and `GhosttyPty::wait` not implemented.

- [ ] **Step 3: Implement worker bootstrap and lifecycle plumbing**

Replace `crates/cairn-core/src/pty/ghostty/worker.rs` with:

```rust
//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

use std::sync::Arc;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::oneshot;

use super::Command;
use crate::pty::{PtyError, SpawnOptions};

/// State shared between the worker thread's setup phase and the caller.
pub(super) struct WorkerHandles {
    pub cmd_tx: flume::Sender<Command>,
    pub exit_rx: tokio::sync::watch::Receiver<Option<ExitStatus>>,
}

pub use std::process::ExitStatus;

/// Spawn the dedicated OS thread that owns the PTY and runs the dispatcher.
///
/// Returns the channels external callers use to interact with the session.
pub(super) fn spawn(opts: SpawnOptions) -> Result<WorkerHandles, PtyError> {
    let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
    let (exit_tx, exit_rx) = tokio::sync::watch::channel::<Option<ExitStatus>>(None);

    // Synchronously open the PTY and spawn the child on this thread so spawn
    // errors surface to the caller rather than getting buried in the worker.
    let pty_system = native_pty_system();
    let pty_pair = pty_system
        .openpty(PtySize {
            rows: opts.size.rows,
            cols: opts.size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;

    // Translate std::process::Command into portable_pty::CommandBuilder.
    // portable-pty wants its own builder type; we copy program + args + env.
    let mut builder = CommandBuilder::new(opts.command.get_program());
    for arg in opts.command.get_args() {
        builder.arg(arg);
    }
    for (k, v) in opts.command.get_envs() {
        if let Some(v) = v {
            builder.env(k, v);
        } else {
            builder.env_remove(k);
        }
    }
    if let Some(cwd) = opts.command.get_current_dir() {
        builder.cwd(cwd);
    }

    let mut child = pty_pair
        .slave
        .spawn_command(builder)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;

    // The slave side can be dropped after spawn — the child holds its own
    // open fd to it. Keeping it open in the parent prevents EOF detection.
    drop(pty_pair.slave);

    let master = Arc::new(std::sync::Mutex::new(pty_pair.master));

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");

            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                // For now: just drain commands until Shutdown or channel close.
                // (Reader task, dispatcher, etc. are added in later tasks.)
                while let Ok(cmd) = cmd_rx.recv_async().await {
                    match cmd {
                        Command::Shutdown => break,
                        // Other commands are not yet handled — reply with Closed
                        // so callers get a clear error in this skeleton stage.
                        Command::Subscribe { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Resize { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Size { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Write { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                    }
                }
            });

            // Wait for the child to exit (synchronous, after the runtime ends).
            let status = child.wait().unwrap_or_else(|_| {
                // If wait fails (already reaped, etc.), fabricate a generic exit.
                ExitStatus::default()
            });
            let _ = exit_tx.send(Some(status));
            drop(master);
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}
```

Replace `crates/cairn-core/src/pty/ghostty/mod.rs` with:

```rust
//! `libghostty-vt`-backed [`PtySession`] implementation.
//!
//! Runs one dedicated OS thread per session hosting a current-thread tokio
//! runtime + `LocalSet`. The thread owns the `!Send` `Terminal`, the PTY
//! master fd, and the broadcast sender. External callers reach it via a
//! `flume` command channel.
//!
//! [`PtySession`]: super::PtySession

mod worker;

use bytes::Bytes;
use tokio::sync::oneshot;

use super::{PtyError, SpawnOptions, Subscription, TermSize};

pub use worker::ExitStatus;

/// Commands the public API sends to the session worker thread.
pub(super) enum Command {
    Subscribe {
        reply: oneshot::Sender<Result<Subscription, PtyError>>,
    },
    Resize {
        size: TermSize,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Size {
        reply: oneshot::Sender<Result<TermSize, PtyError>>,
    },
    Write {
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Shutdown,
}

/// Handle to a running PTY session.
///
/// Construct via [`GhosttyPty::spawn`]. Send + Sync — share across tasks.
pub struct GhosttyPty {
    cmd_tx: flume::Sender<Command>,
    exit_rx: tokio::sync::watch::Receiver<Option<ExitStatus>>,
}

impl GhosttyPty {
    /// Spawn a child process inside a new PTY session.
    pub fn spawn(opts: SpawnOptions) -> Result<Self, PtyError> {
        let handles = worker::spawn(opts)?;
        Ok(Self {
            cmd_tx: handles.cmd_tx,
            exit_rx: handles.exit_rx,
        })
    }

    /// Wait for the child to exit. Returns the exit status.
    ///
    /// Multiple calls are safe; all resolve once the child exits.
    pub async fn wait(&self) -> ExitStatus {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(status) = *rx.borrow() {
                return status;
            }
            // changed() returns Err only when the sender is dropped, which
            // happens after a final `Some(status)` is sent — so loop back.
            if rx.changed().await.is_err() {
                return rx.borrow().unwrap_or_default();
            }
        }
    }
}
```

- [ ] **Step 4: Run test (expect pass)**

Run: `cargo test -p cairn-types --test pty_lifecycle`
Expected: PASS — `true` exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/mod.rs crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_lifecycle.rs
git commit -m "GhosttyPty spawns child via portable-pty and reports exit status"
```

---

### Task 8: Implement kill()

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/mod.rs`
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Modify: `crates/cairn-core/tests/pty_lifecycle.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-core/tests/pty_lifecycle.rs`:

```rust
#[tokio::test]
async fn kill_terminates_long_running_child() {
    // `sleep 60` would block the test runner — kill should make wait() return.
    let cmd = std::process::Command::new("sleep");
    let mut cmd = cmd;
    cmd.arg("60");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Brief delay so the child is actually running before we signal it.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    pty.kill().expect("kill");
    let status = pty.wait().await;
    assert!(!status.success(), "expected non-zero exit after kill, got {:?}", status);
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_lifecycle kill_terminates_long_running_child`
Expected: FAIL — `pty.kill()` does not exist.

- [ ] **Step 3: Implement kill via Command::Shutdown plus child kill**

The trick: `Command::Shutdown` only breaks the dispatcher; the child also needs to receive a kill signal. Easiest approach is to give the worker access to a `Child` clone via the shutdown handler.

Update `crates/cairn-core/src/pty/ghostty/worker.rs` — wrap `child` in a way the dispatcher can reach it. Replace the section starting `let mut child = ...` through the end of the `spawn(move || ...)` closure with:

```rust
    let mut child = pty_pair
        .slave
        .spawn_command(builder)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;

    drop(pty_pair.slave);

    // Take a killer handle now; portable_pty::Child exposes a separate
    // killer that's safe to clone across threads.
    let child_killer = child.clone_killer();

    let master = Arc::new(std::sync::Mutex::new(pty_pair.master));

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");

            let local = tokio::task::LocalSet::new();
            let mut killer = child_killer;
            local.block_on(&rt, async move {
                while let Ok(cmd) = cmd_rx.recv_async().await {
                    match cmd {
                        Command::Shutdown => {
                            // Best-effort kill; reaping happens after the runtime ends.
                            let _ = killer.kill();
                            break;
                        }
                        Command::Subscribe { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Resize { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Size { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Write { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                    }
                }
            });

            let status = child.wait().unwrap_or_else(|_| ExitStatus::default());
            let _ = exit_tx.send(Some(status));
            drop(master);
        })
        .map_err(|e| PtyError::Io { source: e })?;
```

Add the `kill` method to `crates/cairn-core/src/pty/ghostty/mod.rs`. Inside `impl GhosttyPty { ... }`:

```rust
    /// Send a kill signal to the child and tear down the session.
    /// `wait()` will resolve shortly after.
    pub fn kill(&self) -> Result<(), PtyError> {
        self.cmd_tx
            .send(Command::Shutdown)
            .map_err(|_| PtyError::Closed)
    }
```

- [ ] **Step 4: Run tests (expect pass)**

Run: `cargo test -p cairn-types --test pty_lifecycle`
Expected: PASS — both lifecycle tests green.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/mod.rs crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_lifecycle.rs
git commit -m "Implement GhosttyPty::kill via Shutdown command + portable-pty killer"
```

---

## Phase 4 — PTY Reader and VT State

### Task 9: PTY reader task broadcasts raw bytes

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Modify: `crates/cairn-core/src/pty/ghostty/mod.rs`
- Create: `crates/cairn-core/tests/pty_io.rs`

This task adds the reader task that pumps PTY output into a broadcast channel. Subscribers still use `Command::Subscribe`, but for this task only we implement subscribe enough to expose the broadcast receiver; the snapshot stays empty until Task 11 wires libghostty-vt.

- [ ] **Step 1: Write failing integration test**

Create `crates/cairn-core/tests/pty_io.rs`:

```rust
//! Integration tests for GhosttyPty subscribe / write / scrollback I/O.

use bytes::Bytes;
use cairn_types::pty::{GhosttyPty, PtySession, SpawnOptions, TermSize};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

/// Read from the subscription stream until either the deadline elapses or
/// the accumulated bytes contain the needle. Returns the accumulated bytes.
async fn read_until_contains(
    sub: &mut cairn_types::pty::Subscription,
    needle: &[u8],
    deadline: Duration,
) -> Vec<u8> {
    let mut acc = sub.snapshot.to_vec();
    if acc.windows(needle.len()).any(|w| w == needle) {
        return acc;
    }
    let read = async {
        loop {
            match sub.stream.recv().await {
                Ok(chunk) => {
                    acc.extend_from_slice(&chunk);
                    if acc.windows(needle.len()).any(|w| w == needle) {
                        return acc;
                    }
                }
                Err(RecvError::Closed) => return acc,
                Err(RecvError::Lagged(_)) => continue,
            }
        }
    };
    tokio::time::timeout(deadline, read).await.unwrap_or(acc)
}

#[tokio::test]
async fn echo_output_is_broadcast_to_subscribers() {
    let mut cmd = std::process::Command::new("printf");
    cmd.arg("hello-cairn");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe().await.expect("subscribe");
    let bytes = read_until_contains(&mut sub, b"hello-cairn", Duration::from_secs(2)).await;
    assert!(
        bytes.windows(b"hello-cairn".len()).any(|w| w == b"hello-cairn"),
        "did not see 'hello-cairn' in PTY output; got {:?}",
        std::str::from_utf8(&bytes).unwrap_or("<non-utf8>")
    );

    // Process should have exited; wait so the test doesn't leak the worker.
    let _ = pty.wait().await;
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_io`
Expected: FAIL — `subscribe()` returns `PtyError::Closed`.

- [ ] **Step 3: Add reader task and broadcast wiring**

Replace `crates/cairn-core/src/pty/ghostty/worker.rs` with:

```rust
//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs the PTY reader task and the command dispatcher on a `LocalSet`.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::Arc;

use bytes::Bytes;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tokio::sync::broadcast;

use super::Command;
use crate::pty::{PtyError, SpawnOptions, Subscription, TermSize};

pub use std::process::ExitStatus;

pub(super) struct WorkerHandles {
    pub cmd_tx: flume::Sender<Command>,
    pub exit_rx: tokio::sync::watch::Receiver<Option<ExitStatus>>,
}

pub(super) fn spawn(opts: SpawnOptions) -> Result<WorkerHandles, PtyError> {
    let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
    let (exit_tx, exit_rx) = tokio::sync::watch::channel::<Option<ExitStatus>>(None);

    let pty_system = native_pty_system();
    let pty_pair = pty_system
        .openpty(PtySize {
            rows: opts.size.rows,
            cols: opts.size.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;

    let mut builder = CommandBuilder::new(opts.command.get_program());
    for arg in opts.command.get_args() {
        builder.arg(arg);
    }
    for (k, v) in opts.command.get_envs() {
        if let Some(v) = v {
            builder.env(k, v);
        } else {
            builder.env_remove(k);
        }
    }
    if let Some(cwd) = opts.command.get_current_dir() {
        builder.cwd(cwd);
    }

    let mut child = pty_pair
        .slave
        .spawn_command(builder)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;

    drop(pty_pair.slave);

    let child_killer = child.clone_killer();
    let mut reader = pty_pair
        .master
        .try_clone_reader()
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
    let master = pty_pair.master;
    let broadcast_capacity = opts.broadcast_capacity;

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build current-thread runtime");

            let local = tokio::task::LocalSet::new();
            let mut killer = child_killer;

            local.block_on(&rt, async move {
                let (bcast_tx, _) = broadcast::channel::<Bytes>(broadcast_capacity);
                let bcast_tx = Rc::new(bcast_tx);
                let master = Rc::new(master);

                // PTY reader task. portable-pty's reader is std::io::Read on a
                // raw fd; spawn_blocking it to avoid blocking the LocalSet.
                let bcast_tx_for_reader = bcast_tx.clone();
                let reader_task = tokio::task::spawn_local(async move {
                    // Run the blocking read loop on tokio's blocking pool;
                    // forward chunks back via a non-blocking channel.
                    let (chunk_tx, chunk_rx) = flume::unbounded::<Bytes>();
                    let _ = std::thread::Builder::new()
                        .name("cairn-pty-reader".into())
                        .spawn(move || {
                            let mut buf = vec![0u8; 65536];
                            loop {
                                match reader.read(&mut buf) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        let chunk = Bytes::copy_from_slice(&buf[..n]);
                                        if chunk_tx.send(chunk).is_err() {
                                            break;
                                        }
                                    }
                                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                                    Err(_) => break,
                                }
                            }
                        });

                    while let Ok(chunk) = chunk_rx.recv_async().await {
                        let _ = bcast_tx_for_reader.send(chunk);
                    }
                });

                // Command dispatcher loop.
                while let Ok(cmd) = cmd_rx.recv_async().await {
                    match cmd {
                        Command::Shutdown => {
                            let _ = killer.kill();
                            break;
                        }
                        Command::Subscribe { reply } => {
                            // Transient: snapshot is empty until Task 14 wires
                            // in the Formatter. Live stream already works.
                            let sub = Subscription {
                                snapshot: Bytes::new(),
                                stream: bcast_tx.subscribe(),
                            };
                            let _ = reply.send(Ok(sub));
                        }
                        Command::Resize { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Size { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                        Command::Write { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                        }
                    }
                }

                drop(reader_task);
            });

            let status = child.wait().unwrap_or_else(|_| ExitStatus::default());
            let _ = exit_tx.send(Some(status));
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}
```

Add the `PtySession` impl to `crates/cairn-core/src/pty/ghostty/mod.rs` (append below the existing `impl GhosttyPty { ... }`):

```rust
#[async_trait::async_trait]
impl super::PtySession for GhosttyPty {
    async fn size(&self) -> Result<TermSize, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Size { reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn resize(&self, size: TermSize) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Resize { size, reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn subscribe(&self) -> Result<Subscription, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Subscribe { reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn write(&self, data: Bytes) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Write { data, reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }
}
```

- [ ] **Step 4: Run tests (expect pass)**

Run: `cargo test -p cairn-types`
Expected: PASS — all unit tests + lifecycle + io tests green. The `printf` child's output is broadcast and the subscriber sees `hello-cairn`.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/mod.rs crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_io.rs
git commit -m "Reader thread broadcasts PTY bytes; PtySession trait impl on GhosttyPty"
```

---

### Task 10: Implement size() via MasterPty::get_size

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Modify: `crates/cairn-core/tests/pty_io.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-core/tests/pty_io.rs`:

```rust
#[tokio::test]
async fn size_reports_configured_dimensions() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("5");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 132, rows: 50 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let size = pty.size().await.expect("size");
    assert_eq!(size, TermSize { cols: 132, rows: 50 });
    pty.kill().expect("kill");
    let _ = pty.wait().await;
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_io size_reports_configured_dimensions`
Expected: FAIL — `Size` command still returns `Closed`.

- [ ] **Step 3: Implement Size dispatch**

In `crates/cairn-core/src/pty/ghostty/worker.rs`, replace the `Command::Size { reply }` arm in the dispatcher loop with:

```rust
                        Command::Size { reply } => {
                            let result = master
                                .get_size()
                                .map(|s| TermSize {
                                    cols: s.cols,
                                    rows: s.rows,
                                })
                                .map_err(|e| PtyError::Backend { source: Box::new(e) });
                            let _ = reply.send(result);
                        }
```

- [ ] **Step 4: Run test (expect pass)**

Run: `cargo test -p cairn-types --test pty_io`
Expected: PASS — `size()` returns 132×50.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_io.rs
git commit -m "Implement GhosttyPty::size via MasterPty::get_size"
```

---

### Task 11: Implement write() to PTY master

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Modify: `crates/cairn-core/tests/pty_io.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-core/tests/pty_io.rs`:

```rust
#[tokio::test]
async fn write_delivers_bytes_to_child_stdin() {
    // `cat` echoes its stdin back to stdout. We write a line; it should
    // come back through the subscription stream.
    let cmd = std::process::Command::new("cat");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe().await.expect("subscribe");
    pty.write(Bytes::from_static(b"ping-cairn\n"))
        .await
        .expect("write");

    let bytes = read_until_contains(&mut sub, b"ping-cairn", Duration::from_secs(2)).await;
    assert!(
        bytes.windows(b"ping-cairn".len()).any(|w| w == b"ping-cairn"),
        "did not see echoed 'ping-cairn'; got {:?}",
        std::str::from_utf8(&bytes).unwrap_or("<non-utf8>")
    );

    pty.kill().expect("kill");
    let _ = pty.wait().await;
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_io write_delivers_bytes_to_child_stdin`
Expected: FAIL — `Write` command returns `Closed`.

- [ ] **Step 3: Hold a writer and implement Write dispatch**

In `crates/cairn-core/src/pty/ghostty/worker.rs`, inside the `local.block_on(...)` block, add a writer setup right after `let bcast_tx = Rc::new(bcast_tx);`:

```rust
                let writer = master
                    .take_writer()
                    .map_err(|e| PtyError::Backend { source: Box::new(e) });
                let writer = match writer {
                    Ok(w) => Rc::new(std::cell::RefCell::new(w)),
                    Err(e) => {
                        // Surface the error to anyone who tries to write,
                        // but don't crash the worker — the reader can still run.
                        tracing::error!(error = %e, "failed to take PTY writer");
                        let _ = exit_tx.send(Some(ExitStatus::default()));
                        return;
                    }
                };
```

Replace the `Command::Write { reply, .. }` arm with:

```rust
                        Command::Write { data, reply } => {
                            use std::io::Write;
                            let result = writer
                                .borrow_mut()
                                .write_all(&data)
                                .and_then(|_| writer.borrow_mut().flush())
                                .map_err(PtyError::from);
                            let _ = reply.send(result);
                        }
```

You'll also need `use std::cell::RefCell;` at the top of the file (or just keep the inline `std::cell::RefCell`).

- [ ] **Step 4: Run test (expect pass)**

Run: `cargo test -p cairn-types --test pty_io`
Expected: PASS — `cat` echo round-trip works.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_io.rs
git commit -m "Implement GhosttyPty::write via PTY master writer"
```

---

### Task 12: Implement resize()

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Create: `crates/cairn-core/tests/pty_resize.rs`

- [ ] **Step 1: Write failing test**

Create `crates/cairn-core/tests/pty_resize.rs`:

```rust
//! Integration tests for GhosttyPty resize semantics.

use cairn_types::pty::{GhosttyPty, PtySession, SpawnOptions, TermSize};

#[tokio::test]
async fn resize_updates_size_query() {
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("5");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    assert_eq!(pty.size().await.unwrap(), TermSize { cols: 80, rows: 24 });

    pty.resize(TermSize { cols: 120, rows: 40 })
        .await
        .expect("resize");
    assert_eq!(pty.size().await.unwrap(), TermSize { cols: 120, rows: 40 });

    pty.kill().expect("kill");
    let _ = pty.wait().await;
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_resize`
Expected: FAIL — `Resize` command returns `Closed`.

- [ ] **Step 3: Implement Resize dispatch**

In `crates/cairn-core/src/pty/ghostty/worker.rs`, replace the `Command::Resize { reply, .. }` arm with:

```rust
                        Command::Resize { size, reply } => {
                            let result = master
                                .resize(PtySize {
                                    rows: size.rows,
                                    cols: size.cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                })
                                .map_err(|e| PtyError::Backend { source: Box::new(e) });
                            let _ = reply.send(result);
                        }
```

(libghostty-vt's `Terminal::resize` is wired in Task 14 once the Terminal is in scope. Until then, resize only updates the kernel side, which still satisfies this test.)

- [ ] **Step 4: Run test (expect pass)**

Run: `cargo test -p cairn-types --test pty_resize`
Expected: PASS — size reports the new dimensions.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_resize.rs
git commit -m "Implement GhosttyPty::resize (kernel side; VT side added in Task 14)"
```

---

## Phase 5 — Wire In libghostty-vt

### Task 13: Feed PTY bytes into a Terminal instance

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`

This task adds the libghostty-vt `Terminal` to the worker thread and feeds it the same bytes the broadcast sees. The terminal's state isn't queried yet (Task 14 uses Formatter for snapshots); this is just plumbing.

> **API note:** libghostty-vt's exact constructor and `vt_write` signature may differ from the sketch below. Consult <https://docs.rs/libghostty-vt/0.1.1/libghostty_vt/struct.Terminal.html> while implementing. The shape (construct → feed bytes → query state) is correct; argument types may need tweaking.

> **Note on TDD shape:** Task 13 has no new externally-observable behavior on
> its own — Terminal state is internal until Task 14 exposes it via Subscribe's
> snapshot. The "test" here is just a regression check that wiring the
> Terminal doesn't break existing functionality. Task 14 is the one that
> actually drives the Terminal wiring through observable behavior.

- [ ] **Step 1: Write regression guard test**

Append to `crates/cairn-core/tests/pty_io.rs`:

```rust
#[tokio::test]
async fn spawn_succeeds_with_terminal_attached() {
    // Regression guard: when libghostty-vt's Terminal is wired into the
    // worker, spawning and basic broadcast must still work. Behavioral
    // change comes in Task 14 (snapshot via Formatter).
    let mut cmd = std::process::Command::new("printf");
    cmd.arg("vt-attached");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let mut sub = pty.subscribe().await.expect("subscribe");
    let bytes = read_until_contains(&mut sub, b"vt-attached", Duration::from_secs(2)).await;
    assert!(
        bytes.windows(b"vt-attached".len()).any(|w| w == b"vt-attached"),
        "did not see 'vt-attached'"
    );
    let _ = pty.wait().await;
}
```

- [ ] **Step 2: Confirm test passes before changes**

Run: `cargo test -p cairn-types --test pty_io spawn_succeeds_with_terminal_attached`
Expected: PASS (no code changes yet — the test merely captures current behavior so the Step 3 changes can be verified non-regressive).

- [ ] **Step 3: Add Terminal to the worker**

In `crates/cairn-core/src/pty/ghostty/worker.rs`, inside the `local.block_on(...)` block, add the Terminal alongside the writer setup. After `let writer = match writer { ... }` insert:

```rust
                // Owned VT state for this session. Terminal is !Send + !Sync
                // and stays pinned to this thread (the LocalSet guarantees it).
                //
                // Construction signature per docs.rs/libghostty-vt/0.1.1. If
                // the actual API differs, adjust here and in the vt_write call
                // below — the shape (build → feed bytes → query) is stable.
                let terminal = match libghostty_vt::Terminal::new(
                    libghostty_vt::TerminalOptions::default()
                        .cols(opts.size.cols)
                        .rows(opts.size.rows),
                ) {
                    Ok(t) => Rc::new(std::cell::RefCell::new(t)),
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to construct libghostty-vt Terminal");
                        let _ = exit_tx.send(Some(ExitStatus::default()));
                        return;
                    }
                };
```

Replace the reader task body so it routes received chunks through the Terminal before broadcasting. Find the inner `while let Ok(chunk) = chunk_rx.recv_async().await` loop and change it to:

```rust
                    let terminal_for_reader = terminal.clone();
                    while let Ok(chunk) = chunk_rx.recv_async().await {
                        // Feed the VT parser so its screen state stays current.
                        // borrow_mut() is held only across this synchronous call —
                        // never across an .await — so the dispatcher's borrow_mut
                        // calls in command handlers can't collide.
                        if let Err(e) = terminal_for_reader.borrow_mut().vt_write(&chunk) {
                            tracing::warn!(error = ?e, "vt_write failed");
                        }
                        let _ = bcast_tx_for_reader.send(chunk);
                    }
```

Note: the `terminal` `Rc` is also needed by the dispatcher loop in Task 14; that wiring is added there.

- [ ] **Step 4: Run all tests**

Run: `cargo test -p cairn-types`
Expected: PASS — all existing tests still green; Terminal is attached but its state isn't yet observed externally.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_io.rs
git commit -m "Attach libghostty-vt Terminal to PTY reader; feed bytes via vt_write"
```

---

### Task 14: Snapshot via Formatter in subscribe()

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Modify: `crates/cairn-core/tests/pty_io.rs`

This task wires the Formatter so `Subscription::snapshot` reflects current Terminal state, and also extends `Resize` to update the Terminal grid in lockstep with the kernel.

> **API note:** see <https://docs.rs/libghostty-vt/0.1.1/libghostty_vt/fmt/struct.Formatter.html>. The Formatter uses `format_alloc(allocator)` or `format_buf(buf)`; either is fine — the test pins down the externally-observable behavior.

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-core/tests/pty_io.rs`:

```rust
#[tokio::test]
async fn late_subscriber_sees_prior_output_in_snapshot() {
    // First subscriber starts immediately; second subscribes after the child
    // has finished writing. The second's snapshot should contain the same
    // visible content (text "late-join-marker") as the first saw via stream.
    let mut cmd = std::process::Command::new("printf");
    cmd.arg("late-join-marker");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub1 = pty.subscribe().await.expect("subscribe-1");
    let bytes1 = read_until_contains(&mut sub1, b"late-join-marker", Duration::from_secs(2)).await;
    assert!(bytes1
        .windows(b"late-join-marker".len())
        .any(|w| w == b"late-join-marker"));

    // Wait for child exit so subsequent reads return Closed promptly.
    let _ = pty.wait().await;

    let sub2 = pty.subscribe().await.expect("subscribe-2");
    // The snapshot bytes are an opaque VT escape stream; we don't try to
    // parse them, but the literal text 'late-join-marker' (printed by
    // printf) should still appear somewhere in the encoded screen since
    // libghostty-vt's Formatter emits literal characters for printable cells.
    assert!(
        sub2.snapshot
            .windows(b"late-join-marker".len())
            .any(|w| w == b"late-join-marker"),
        "snapshot missing 'late-join-marker'; got {:?}",
        std::str::from_utf8(&sub2.snapshot).unwrap_or("<non-utf8>")
    );
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_io late_subscriber_sees_prior_output_in_snapshot`
Expected: FAIL — snapshot is currently empty.

- [ ] **Step 3: Wire Formatter into Subscribe and Resize into Terminal**

In `crates/cairn-core/src/pty/ghostty/worker.rs`, inside the dispatcher loop, replace the `Command::Subscribe { reply }` arm with:

```rust
                        Command::Subscribe { reply } => {
                            // Atomic: format current Terminal state, then
                            // subscribe to subsequent bytes. tokio::broadcast
                            // guarantees the receiver only sees messages sent
                            // after creation, so no overlap with the snapshot.
                            let snapshot = match format_snapshot(&terminal.borrow()) {
                                Ok(bytes) => bytes,
                                Err(e) => {
                                    let _ = reply.send(Err(e));
                                    continue;
                                }
                            };
                            let sub = Subscription {
                                snapshot,
                                stream: bcast_tx.subscribe(),
                            };
                            let _ = reply.send(Ok(sub));
                        }
```

Replace the `Command::Resize { size, reply }` arm with:

```rust
                        Command::Resize { size, reply } => {
                            // Update VT state and kernel side together so no
                            // partial state is observable to subscribers.
                            if let Err(e) = terminal
                                .borrow_mut()
                                .resize(size.cols, size.rows, 0, 0)
                            {
                                let _ = reply.send(Err(PtyError::Backend {
                                    source: Box::new(e),
                                }));
                                continue;
                            }
                            let result = master
                                .resize(PtySize {
                                    rows: size.rows,
                                    cols: size.cols,
                                    pixel_width: 0,
                                    pixel_height: 0,
                                })
                                .map_err(|e| PtyError::Backend { source: Box::new(e) });
                            let _ = reply.send(result);
                        }
```

Add a `format_snapshot` helper at the bottom of `crates/cairn-core/src/pty/ghostty/worker.rs`:

```rust
/// Serialize the current Terminal state as a self-contained VT escape
/// sequence stream. Clients feed this to their local emulator (xterm.js,
/// ghostty-web, etc.) to reconstruct the visible screen + scrollback.
///
/// Bounded by `cols × (rows + scrollback_rows)`, independent of session length.
fn format_snapshot(terminal: &libghostty_vt::Terminal) -> Result<Bytes, PtyError> {
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};

    let opts = FormatterOptions::default().format(Format::VT);
    let formatter = Formatter::new(terminal, opts)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
    let vec = formatter
        .format_alloc(libghostty_vt::alloc::Allocator::default())
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
    Ok(Bytes::from(vec.to_vec()))
}
```

> **Implementer note:** if `FormatterOptions::default().format(Format::VT)` isn't the exact builder syntax in 0.1.1, the closest equivalent works. The behavioral contract — "the output bytes contain literal printable characters from the screen" — is what the test pins down.

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-types`
Expected: PASS — late subscriber's snapshot contains `late-join-marker`.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_io.rs
git commit -m "Atomic snapshot + subscribe via libghostty-vt Formatter; resize updates VT grid"
```

---

### Task 15: Wire PtyWriteFn for terminal queries

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Modify: `crates/cairn-core/tests/pty_io.rs`

The Terminal needs a callback (`PtyWriteFn`) that writes response bytes back into the PTY master. The callback and the `Command::Write` handler share the same writer via `Rc<RefCell<...>>`.

> **API note:** `libghostty_vt::terminal::PtyWriteFn` and friends are callbacks installed at `Terminal::new` via `TerminalOptions`. Consult the docs while implementing; the test pins down the contract: a program that asks DA1 should receive a response without any client attached.

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-core/tests/pty_io.rs`:

```rust
#[tokio::test]
async fn da1_query_gets_response_without_client() {
    // Smallest reproducible TTY query test: launch a shell script that sends
    // ESC[c (DA1) and then reads one byte from stdin (the response). If the
    // server responds, the read succeeds within the timeout and we see the
    // sentinel 'ok' printed. If no response, the read blocks and the test
    // times out.
    //
    // Note: depends on `sh` being on PATH (true on Linux/macOS).
    let script = r#"
        printf '\033[c'
        read -r -n 32 -t 1 reply
        printf 'reply-len=%d\n' "${#reply}"
    "#;
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe().await.expect("subscribe");
    let bytes = read_until_contains(&mut sub, b"reply-len=", Duration::from_secs(3)).await;
    let text = std::str::from_utf8(&bytes).unwrap_or("<non-utf8>");
    assert!(text.contains("reply-len="), "missing reply-len marker: {text}");
    // The reply length must be >0 — meaning the script received the DA1 response.
    assert!(
        text.contains("reply-len=") && !text.contains("reply-len=0"),
        "expected non-zero reply length (terminal responded to DA1), got: {text}"
    );

    let _ = pty.wait().await;
}
```

- [ ] **Step 2: Run test (expect failure)**

Run: `cargo test -p cairn-types --test pty_io da1_query_gets_response_without_client`
Expected: FAIL — without `PtyWriteFn`, nothing answers the DA1 query, so `read` times out and the script prints `reply-len=0`.

- [ ] **Step 3: Install PtyWriteFn**

In `crates/cairn-core/src/pty/ghostty/worker.rs`, modify Terminal construction. The writer is constructed *before* the Terminal so it can be captured by the callback:

```rust
                // Writer FIRST so the Terminal's PtyWriteFn can capture it.
                let writer = master
                    .take_writer()
                    .map_err(|e| PtyError::Backend { source: Box::new(e) });
                let writer = match writer {
                    Ok(w) => Rc::new(std::cell::RefCell::new(w)),
                    Err(e) => {
                        tracing::error!(error = %e, "failed to take PTY writer");
                        let _ = exit_tx.send(Some(ExitStatus::default()));
                        return;
                    }
                };

                let writer_for_callback = writer.clone();
                let pty_write_fn = move |bytes: &[u8]| {
                    use std::io::Write;
                    if let Err(e) = writer_for_callback.borrow_mut().write_all(bytes) {
                        tracing::warn!(error = %e, "PtyWriteFn failed to write response");
                    }
                };

                let terminal = match libghostty_vt::Terminal::new(
                    libghostty_vt::TerminalOptions::default()
                        .cols(opts.size.cols)
                        .rows(opts.size.rows)
                        .pty_write_fn(pty_write_fn),
                ) {
                    Ok(t) => Rc::new(std::cell::RefCell::new(t)),
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to construct libghostty-vt Terminal");
                        let _ = exit_tx.send(Some(ExitStatus::default()));
                        return;
                    }
                };
```

> **Implementer note:** the exact name of `TerminalOptions::pty_write_fn` and the closure signature depend on the 0.1.1 API. The trait is `libghostty_vt::terminal::PtyWriteFn`. If a `Fn(&[u8])` closure can't be installed directly, wrap it in a small newtype that implements the trait.

- [ ] **Step 4: Run test (expect pass)**

Run: `cargo test -p cairn-types --test pty_io da1_query_gets_response_without_client`
Expected: PASS — script reports non-zero reply length.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_io.rs
git commit -m "Wire libghostty-vt PtyWriteFn so terminal queries get authoritative responses"
```

---

## Phase 6 — Cleanup and Robustness

### Task 16: Process death closes subscribers

**Files:**
- Modify: `crates/cairn-core/src/pty/ghostty/worker.rs`
- Modify: `crates/cairn-core/tests/pty_io.rs`

When the child exits and the PTY reader sees EOF, existing subscribers should observe `RecvError::Closed` (caused by `bcast_tx` being dropped).

- [ ] **Step 1: Write failing test**

Append to `crates/cairn-core/tests/pty_io.rs`:

```rust
#[tokio::test]
async fn subscribers_observe_close_on_child_exit() {
    let cmd = std::process::Command::new("true");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let mut sub = pty.subscribe().await.expect("subscribe");
    let _ = pty.wait().await;

    // Loop draining anything still in the channel, then assert we eventually
    // get Closed (not Lagged, not data).
    let saw_close = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match sub.stream.recv().await {
                Ok(_) => continue,
                Err(RecvError::Closed) => return true,
                Err(RecvError::Lagged(_)) => continue,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(saw_close, "subscribers did not observe Closed after child exit");
}
```

- [ ] **Step 2: Run test (expect failure or pass)**

Run: `cargo test -p cairn-types --test pty_io subscribers_observe_close_on_child_exit`
Expected: This may already PASS if `bcast_tx` is dropped naturally when the worker async block ends. If so, keep the test as regression coverage and skip Step 3. If it FAILs (subscribers never see Closed), continue to Step 3.

- [ ] **Step 3 (only if Step 2 failed): Explicitly close broadcast on reader EOF**

In `crates/cairn-core/src/pty/ghostty/worker.rs`, after the dispatcher loop and reader task complete, drop the broadcast sender explicitly:

```rust
                // Explicit teardown: drop broadcast sender so subscribers
                // see Closed promptly even if the dispatcher exits first.
                drop(bcast_tx);
                drop(reader_task);
```

- [ ] **Step 4: Run test (expect pass)**

Run: `cargo test -p cairn-types --test pty_io`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-core/src/pty/ghostty/worker.rs crates/cairn-core/tests/pty_io.rs
git commit -m "Subscribers observe Closed when child exits"
```

---

### Task 17: Operations after exit return Closed

**Files:**
- Modify: `crates/cairn-core/tests/pty_lifecycle.rs`

This is a regression test, no new code — the existing `cmd_tx` disconnect handling already returns `PtyError::Closed` when the worker drops the receiver. We pin the behavior.

- [ ] **Step 1: Write test**

Append to `crates/cairn-core/tests/pty_lifecycle.rs`:

```rust
use bytes::Bytes;
use cairn_types::pty::{PtyError, PtySession};

#[tokio::test]
async fn write_after_exit_returns_closed() {
    let cmd = std::process::Command::new("true");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let _ = pty.wait().await;

    // Give the worker a moment to fully tear down its channel.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let result = pty.write(Bytes::from_static(b"x")).await;
    assert!(
        matches!(result, Err(PtyError::Closed)),
        "expected Closed, got {:?}",
        result
    );
}
```

- [ ] **Step 2: Run test**

Run: `cargo test -p cairn-types --test pty_lifecycle write_after_exit_returns_closed`
Expected: PASS — `cmd_tx` channel is closed once the worker thread exits, so `send_async` errors and we map it to `Closed`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-core/tests/pty_lifecycle.rs
git commit -m "Regression: operations after exit return PtyError::Closed"
```

---

## Phase 7 — Re-exports and Public Surface

### Task 18: Re-export pty module at crate root

**Files:**
- Modify: `crates/cairn-core/src/lib.rs`

- [ ] **Step 1: Update lib.rs**

Replace the contents of `crates/cairn-core/src/lib.rs` with:

```rust
//! `cairn-core` (package: `cairn-types`)
//!
//! Core types and abstractions for cairn agent harnessing.

pub mod pty;
```

- [ ] **Step 2: Verify integration tests still pick up the path**

Run: `cargo test -p cairn-types`
Expected: PASS — all tests green (they already import via `cairn_types::pty::...`).

- [ ] **Step 3: Verify docs build**

Run: `cargo doc -p cairn-types --no-deps`
Expected: clean doc build, no warnings about unresolved links.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-core/src/lib.rs
git commit -m "Expose pty module at crate root"
```

---

## Phase 8 — Final Verification

### Task 19: Full test sweep + clippy + fmt

**Files:** none (verification only)

- [ ] **Step 1: Run the full test suite**

Run: `cargo test -p cairn-types --all-features`
Expected: PASS — every test green.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy -p cairn-types --all-targets -- -D warnings`
Expected: clean (no warnings, no errors).

- [ ] **Step 3: Run rustfmt**

Run: `cargo fmt -p cairn-types --check`
Expected: clean. If not, run `cargo fmt -p cairn-types` and commit the format fixes:

```bash
git add crates/cairn-core/src/
git commit -m "cargo fmt"
```

- [ ] **Step 4: Manual smoke test (optional but recommended)**

Write a tiny example in `crates/cairn-core/examples/echo.rs`:

```rust
use cairn_types::pty::{GhosttyPty, PtySession, SpawnOptions};

#[tokio::main]
async fn main() {
    let mut cmd = std::process::Command::new("bash");
    cmd.arg("-i");
    let opts = SpawnOptions::new(cmd);
    let pty = GhosttyPty::spawn(opts).expect("spawn");
    let mut sub = pty.subscribe().await.expect("subscribe");
    println!("snapshot length: {}", sub.snapshot.len());
    pty.write(bytes::Bytes::from_static(b"echo hello-from-cairn\n"))
        .await
        .expect("write");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut total = 0usize;
    while let Ok(chunk) = sub.stream.try_recv() {
        total += chunk.len();
    }
    println!("received {total} bytes from bash");
    pty.kill().expect("kill");
}
```

Run: `cargo run -p cairn-types --example echo`
Expected: prints a non-zero snapshot length, prints a byte count after the `echo` round-trip.

- [ ] **Step 5: Commit the example (if you added it)**

```bash
git add crates/cairn-core/examples/echo.rs
git commit -m "Add echo example demonstrating GhosttyPty usage"
```

---

## Spec Coverage Checklist

After implementing all tasks, verify every spec section has an implementation:

- [x] `PtySession` trait with `size`, `resize`, `subscribe`, `write` — Tasks 5, 9–14.
- [x] `Subscription { snapshot, stream }` atomic — Task 14.
- [x] `PtyError::{Closed, Io, Backend}` — Task 2.
- [x] `TermSize`, `SpawnOptions` — Task 3.
- [x] `GhosttyPty::{spawn, wait, kill}` lifecycle — Tasks 7, 8.
- [x] Single dedicated thread + current-thread runtime + LocalSet — Task 7+.
- [x] PTY reader collapsed onto the session thread — Task 9, 13.
- [x] `Rc<RefCell<Terminal>>` pattern (no borrow across await) — Task 13, 14.
- [x] Snapshot via `Formatter` — Task 14.
- [x] `MasterPty::get_size` for size queries — Task 10.
- [x] `Terminal::resize` + `MasterPty::resize` in lockstep — Task 14.
- [x] `PtyWriteFn` for query responses — Task 15.
- [x] Subscribers see `Closed` on child exit — Task 16.
- [x] Operations after exit return `PtyError::Closed` — Task 17.
- [x] `portable-pty = 0.9` cross-platform — Task 1, 7+.

Out-of-scope (per spec, not in this plan): multi-client resize policy, writer arbitration, higher-level harness adapter, session persistence, bridge integration, recording, frontend, shared-thread session pool.

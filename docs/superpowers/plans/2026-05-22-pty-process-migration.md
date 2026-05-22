# pty-process Migration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate the existing `GhosttyPty` implementation from `portable-pty` (3 OS threads per session) to `pty-process` (1 OS thread per session) without changing the public API or behavior, per the updated spec at `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`.

**Architecture:** Replace the three-thread topology (cairn-pty-waiter, cairn-pty-reader, cairn-pty-session) with a single session thread running a current-thread tokio runtime + `LocalSet`. Inside the LocalSet, a single task multiplexes PTY reads, external commands, and child exit via `tokio::select!`. The PTY itself becomes a `pty_process::Pty` that implements `AsyncRead`/`AsyncWrite`; the child becomes a `tokio::process::Child` whose `wait()` is reactor-driven.

**Tech Stack:** Rust 2024, tokio 1.52 (current-thread runtime + LocalSet), `libghostty-vt = 0.1.1`, `pty-process = 0.4` (replaces `portable-pty = 0.9`), `flume = 0.12`, `async-trait`, `bytes`, `snafu`.

---

## Pre-flight

This plan migrates an existing, fully-tested implementation. The contract is:

1. Every existing test in `crates/cairn-pty/tests/` must continue to pass at the end of every task.
2. The public API (`pty::PtySession`, `pty::GhosttyPty`, `pty::SpawnOptions`, etc.) does not change.
3. The `pty::ghostty::ExitStatus` re-export changes type (from `portable_pty::ExitStatus` to `std::process::ExitStatus`), but the methods callers use (`.success()`) are identical.

You will be working in `crates/cairn-pty/`. Run all commands from the repo root unless noted.

## File Structure

```
crates/cairn-pty/
├── Cargo.toml                                 (modified: dep swap)
├── src/pty/
│   ├── types.rs                               (modified: comment update only)
│   └── ghostty/
│       ├── mod.rs                             (modified: ExitStatus re-export)
│       └── worker.rs                          (rewritten: single-thread topology)
└── tests/                                     (unchanged; act as the contract)
```

Only `worker.rs` undergoes substantive change. The trait, error type, types, subscription, and tests all stay byte-identical.

## Pre-flight verification

- [ ] **Step 1: Confirm current state is green**

```bash
cargo test -p cairn-pty
```

Expected: all tests pass.

- [ ] **Step 2: Confirm starting branch**

```bash
git status
git log --oneline -1
```

Expected: clean working tree on `feature/pty-session` (or current working branch), head at the most recent pty-related commit.

If the working tree is not clean, stop and resolve before proceeding.

---

## Task 1: Swap the PTY dependency

**Files:**
- Modify: `crates/cairn-pty/Cargo.toml`

**Rationale:** `pty-process` provides Unix-native async PTY I/O and integrates with `tokio::process::Child` for reactor-driven wait. Adding it alongside portable-pty is fine — we don't remove portable-pty until Task 5, after the rewrite compiles and tests pass on the new path.

- [ ] **Step 1: Add `pty-process` to `[dependencies]`**

Open `crates/cairn-pty/Cargo.toml` and add a single line in the `[dependencies]` block, immediately after the existing `portable-pty` line:

```toml
pty-process = { version = "0.4", features = ["async"] }
```

The block should look like (existing `portable-pty = "0.9"` is intentionally left in place for this task):

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
pty-process = { version = "0.4", features = ["async"] }
```

- [ ] **Step 2: Verify the crate still compiles**

```bash
cargo check -p cairn-pty
```

Expected: success (no errors, possibly an "unused dependency" warning for `pty-process` which we will resolve in Task 3).

- [ ] **Step 3: Verify all tests still pass**

```bash
cargo test -p cairn-pty
```

Expected: all tests pass (same as pre-flight).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/Cargo.toml
git commit -m "$(cat <<'EOF'
Add pty-process dependency alongside portable-pty

Staged migration: introduce pty-process so worker.rs can be rewritten to
use Unix-native async PTY I/O without breaking compilation. portable-pty
is removed in a later task once the rewrite is green.
EOF
)"
```

---

## Task 2: Update `ExitStatus` re-export to `std::process::ExitStatus`

**Files:**
- Modify: `crates/cairn-pty/src/pty/ghostty/worker.rs`

**Rationale:** `pty_process::Command::spawn` returns a `tokio::process::Child`, whose `wait()` resolves to `std::process::ExitStatus`. We change the public re-export to match. The methods callers use (`.success()`) are identical on both types, so the existing tests are source-compatible.

This task does NOT yet rewrite the worker — only the type alias. The worker still uses `portable-pty` internally for now.

- [ ] **Step 1: Replace the re-export and the synthetic exit-status construction site**

Open `crates/cairn-pty/src/pty/ghostty/worker.rs`.

Replace line 16:

```rust
pub use portable_pty::ExitStatus;
```

with:

```rust
pub use std::process::ExitStatus;
```

The synthetic exit status used on `child.wait()` failure (currently `ExitStatus::with_exit_code(1)` around line 105 in worker.rs and similar in `ghostty/mod.rs` around line 79) is a portable-pty-specific constructor. We replace it with the Unix-extension constructor.

Find this line in `worker.rs`:

```rust
            let status = child.wait().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "child wait failed; reporting synthetic exit code 1");
                ExitStatus::with_exit_code(1)
            });
```

Replace with:

```rust
            let status = child.wait().unwrap_or_else(|e| {
                tracing::warn!(error = %e, "child wait failed; reporting synthetic exit code 1");
                synthetic_exit_status(1)
            });
```

Then add this helper function at the bottom of `worker.rs` (marked `pub(super)` so `mod.rs` can call it):

```rust
/// Construct a synthetic `std::process::ExitStatus` with the given exit code.
///
/// Used when `child.wait()` itself fails (rare; usually wait failures imply
/// the parent has lost track of the child, not that the child is healthy).
/// We surface this as a failing exit so callers see the session as broken.
#[cfg(unix)]
pub(super) fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}
```

Now find the matching call site in `crates/cairn-pty/src/pty/ghostty/mod.rs` (around line 79):

```rust
            if rx.changed().await.is_err() {
                return rx
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| ExitStatus::with_exit_code(1));
            }
```

Replace with:

```rust
            if rx.changed().await.is_err() {
                return rx
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| worker::synthetic_exit_status(1));
            }
```

No new function in `mod.rs` is needed — the helper lives in `worker.rs` and is reachable via the existing `mod worker;` declaration.

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p cairn-pty
```

Expected: success. (If you see "no method `with_exit_code` on `ExitStatus`" you missed a call site — grep for `with_exit_code` and replace it with the helper.)

- [ ] **Step 3: Verify all tests still pass**

```bash
cargo test -p cairn-pty
```

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/src/pty/ghostty/
git commit -m "$(cat <<'EOF'
Switch ExitStatus re-export to std::process::ExitStatus

Replaces portable_pty::ExitStatus::with_exit_code with a Unix
ExitStatusExt::from_raw helper. Method surface (.success(), exit code
access) is unchanged, so tests stay source-compatible.
EOF
)"
```

---

## Task 3: Rewrite `worker.rs` for `pty-process`

**Files:**
- Modify: `crates/cairn-pty/src/pty/ghostty/worker.rs` (substantive rewrite)

This is the load-bearing task. It collapses three threads into one and replaces the portable-pty surface with pty-process. The strategy: rewrite the file in place, then verify by running the existing test suite (which encodes all the behavior we need to preserve).

The rewrite has five logical pieces:

1. PTY open + child spawn via `pty_process::open()` and `pty_process::Command`.
2. Eliminate the `cairn-pty-reader` thread: PTY reads happen as an async branch in the LocalSet.
3. Eliminate the `cairn-pty-waiter` thread: `tokio::process::Child::wait()` is awaited in the same LocalSet.
4. Replace the blocking writer with `pty_process::Pty`'s `AsyncWrite`.
5. Wire `PtyWriteFn` callback via a pending-writes queue, drained after each `vt_write`.

All five changes land in one rewrite of `worker.rs`. Multiple sub-steps follow.

- [ ] **Step 1: Replace the file contents wholesale**

Open `crates/cairn-pty/src/pty/ghostty/worker.rs` and replace its entire contents with:

```rust
//! Session worker thread: bootstraps the current-thread tokio runtime,
//! runs a single LocalSet task that multiplexes PTY I/O, command dispatch,
//! and child exit via tokio::select!.
//!
//! See docs/superpowers/specs/2026-05-22-pty-session-trait-design.md for
//! the architectural rationale (single thread per session, Unix-only,
//! pty-process for AsyncRead/AsyncWrite and tokio::process::Child).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;

use super::Command;
use crate::pty::{PtyError, SpawnOptions, Subscription, TermSize};

pub use std::process::ExitStatus;

/// State shared between the worker thread's setup phase and the caller.
pub(super) struct WorkerHandles {
    pub cmd_tx: flume::Sender<Command>,
    pub exit_rx: tokio::sync::watch::Receiver<Option<ExitStatus>>,
}

/// Spawn the dedicated OS thread that owns the PTY and runs the dispatcher.
///
/// Returns the channels external callers use to interact with the session.
pub(super) fn spawn(opts: SpawnOptions) -> Result<WorkerHandles, PtyError> {
    let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
    let (exit_tx, exit_rx) = tokio::sync::watch::channel::<Option<ExitStatus>>(None);

    // Clamp to at least 1: broadcast::channel(0) panics, and capacity is just
    // a tuning knob — silently promoting 0 → 1 is more forgiving than erroring.
    let broadcast_capacity = opts.broadcast_capacity.max(1);
    let initial_size = opts.size;
    let scrollback_lines = opts.scrollback_lines;

    // Synchronously open the PTY and spawn the child on this thread so spawn
    // errors surface to the caller rather than getting buried in the worker.
    let (pty, pts) = pty_process::open().map_err(|e| PtyError::Io { source: e })?;

    pty.resize(pty_process::Size::new(initial_size.rows, initial_size.cols))
        .map_err(|e| PtyError::Io { source: e })?;

    // Translate std::process::Command into pty_process::Command. pty-process
    // wraps tokio::process::Command and uses a builder API, so we copy program
    // + args + env + cwd by hand.
    let mut builder = pty_process::Command::new(opts.command.get_program());
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
        builder.current_dir(cwd);
    }

    let child = builder
        .spawn(pts)
        .map_err(|e| PtyError::Io { source: e })?;

    // Build the runtime on this (parent) thread so construction failures
    // surface to the caller via spawn() rather than panicking in the worker.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    std::thread::Builder::new()
        .name("cairn-pty-session".into())
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run_session(SessionState {
                pty,
                child,
                cmd_rx,
                exit_tx,
                broadcast_capacity,
                initial_size,
                scrollback_lines,
            }));
        })
        .map_err(|e| PtyError::Io { source: e })?;

    Ok(WorkerHandles { cmd_tx, exit_rx })
}

struct SessionState {
    pty: pty_process::Pty,
    child: tokio::process::Child,
    cmd_rx: flume::Receiver<Command>,
    exit_tx: tokio::sync::watch::Sender<Option<ExitStatus>>,
    broadcast_capacity: usize,
    initial_size: TermSize,
    scrollback_lines: usize,
}

/// Main session loop. Runs inside the LocalSet on the dedicated thread.
///
/// Single tokio::select! across:
///   - pty.read(...)               (PTY readable → vt_write + broadcast)
///   - cmd_rx.recv_async()         (external commands → dispatch)
///   - child.wait()                (child exit → publish status + tear down)
async fn run_session(mut s: SessionState) {
    // Pending writes from the libghostty-vt PtyWriteFn callback. The callback
    // is synchronous (fires inside terminal.vt_write); pty.write_all is async.
    // We queue bytes in the callback and drain them on the same task after
    // each vt_write call. Rc<RefCell<...>> is safe because the LocalSet is
    // single-threaded; borrow_mut is held only across sync code.
    let pending_writes: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();

    // Construct the VT emulator. The PtyWriteFn closure captures a clone of
    // pending_writes and pushes; the main loop drains and forwards to pty.
    let mut terminal = match Terminal::new(TerminalOptions {
        cols: s.initial_size.cols,
        rows: s.initial_size.rows,
        max_scrollback: s.scrollback_lines,
    }) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = ?e, "failed to construct libghostty-vt Terminal");
            drain_commands_with_construction_error(&s.cmd_rx);
            return;
        }
    };

    let pending_for_cb = pending_writes.clone();
    if let Err(e) = terminal.on_pty_write(move |_term, data| {
        pending_for_cb
            .borrow_mut()
            .push_back(Bytes::copy_from_slice(data));
    }) {
        tracing::error!(error = ?e, "failed to install PtyWriteFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }
    let terminal = Rc::new(RefCell::new(terminal));

    let (bcast_tx, _) = broadcast::channel::<Bytes>(s.broadcast_capacity);
    // Option so the EOF/exit path can drop the sender promptly, surfacing
    // RecvError::Closed to existing subscribers even if cmd_rx is still alive.
    let bcast_tx: Rc<RefCell<Option<broadcast::Sender<Bytes>>>> =
        Rc::new(RefCell::new(Some(bcast_tx)));

    // Cached size; updated on every successful resize. pty_process::Pty has
    // no get_size shortcut and we always set the size ourselves, so caching
    // is authoritative.
    let mut current_size = s.initial_size;

    let mut buf = vec![0u8; 65536];
    // Track whether we have already published the exit status, to keep
    // behavior identical when EOF on the PTY fires before SIGCHLD propagates.
    // Used as the guard on the `child.wait()` select branch so we never
    // poll wait twice.
    let mut exit_published = false;

    loop {
        // tokio::select! creates each branch's future fresh per iteration.
        // The `&mut self` borrows that pty.read / child.wait require are
        // local to a single iteration — when one branch wins, select! drops
        // the others before running the matched arm, releasing borrows so
        // the arm can call &mut methods on the same object freely.
        tokio::select! {
            // ── PTY readable
            res = s.pty.read(&mut buf) => match res {
                Ok(0) => {
                    // EOF — child closed slave. If we haven't already
                    // published exit status from the wait branch, await it
                    // here. wait() returning Ok after EOF is essentially
                    // instant because the child is already a zombie.
                    if !exit_published {
                        if let Ok(status) = s.child.wait().await {
                            let _ = s.exit_tx.send(Some(status));
                        }
                    }
                    break;
                }
                Ok(n) => {
                    let chunk = Bytes::copy_from_slice(&buf[..n]);
                    // borrow_mut is held only across these sync calls — never
                    // across an .await — so no LocalSet task collision risk.
                    terminal.borrow_mut().vt_write(&chunk);
                    if let Some(tx) = bcast_tx.borrow().as_ref() {
                        let _ = tx.send(chunk);
                    }
                    // Flush any queued PtyWriteFn responses (DA1, DSR, etc.).
                    flush_pending_writes(&pending_writes, &mut s.pty).await;
                }
                Err(_) => break,
            },

            // ── External command
            recv = s.cmd_rx.recv_async() => {
                let cmd = match recv {
                    Ok(c) => c,
                    Err(_) => break, // all GhosttyPty handles dropped
                };
                if exit_published {
                    // Post-exit normalisation: reply Closed to everything except
                    // Shutdown (no-op) and Subscribe (still returns final state).
                    match cmd {
                        Command::Shutdown => break,
                        Command::Subscribe { .. } => {} // fall through to normal handler
                        Command::Resize { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                            continue;
                        }
                        Command::Size { reply } => {
                            let _ = reply.send(Err(PtyError::Closed));
                            continue;
                        }
                        Command::Write { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                            continue;
                        }
                    }
                }
                match cmd {
                    Command::Shutdown => {
                        // Best-effort kill; the child's wait will resolve
                        // shortly after the signal lands.
                        if let Err(e) = s.child.start_kill() {
                            tracing::warn!(
                                error = %e,
                                "failed to signal child on shutdown; \
                                 it may have already exited"
                            );
                        }
                        // Await wait here so we publish status before
                        // teardown. select! has already dropped the
                        // wait-branch future for this iteration, so s.child
                        // is freely borrowable.
                        if !exit_published {
                            if let Ok(status) = s.child.wait().await {
                                let _ = s.exit_tx.send(Some(status));
                            }
                        }
                        break;
                    }
                    Command::Subscribe { reply } => {
                        let snapshot = match format_snapshot(&terminal.borrow()) {
                            Ok(bytes) => bytes,
                            Err(e) => { let _ = reply.send(Err(e)); continue; }
                        };
                        let stream = match bcast_tx.borrow().as_ref() {
                            Some(tx) => tx.subscribe(),
                            None => {
                                // Session post-exit: produce a stream that
                                // immediately closes on first recv.
                                let (tmp_tx, rx) = broadcast::channel::<Bytes>(1);
                                drop(tmp_tx);
                                rx
                            }
                        };
                        let _ = reply.send(Ok(Subscription { snapshot, stream }));
                    }
                    Command::Resize { size, reply } => {
                        let res = (|| -> Result<(), PtyError> {
                            terminal
                                .borrow_mut()
                                .resize(size.cols, size.rows, 0, 0)
                                .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
                            s.pty
                                .resize(pty_process::Size::new(size.rows, size.cols))
                                .map_err(|e| PtyError::Io { source: e })?;
                            Ok(())
                        })();
                        if res.is_ok() {
                            current_size = size;
                        }
                        let _ = reply.send(res);
                    }
                    Command::Size { reply } => {
                        let _ = reply.send(Ok(current_size));
                    }
                    Command::Write { data, reply } => {
                        let res = s.pty.write_all(&data).await.map_err(PtyError::from);
                        let _ = reply.send(res);
                    }
                }
            },

            // ── Child exited (independently of EOF on the PTY master).
            // Guarded by `if !exit_published` so the branch is dormant once
            // exit has been reported; tokio::select! skips the branch on
            // subsequent iterations without polling s.child again.
            status = s.child.wait(), if !exit_published => {
                match status {
                    Ok(s_val) => { let _ = s.exit_tx.send(Some(s_val)); }
                    Err(e) => {
                        tracing::warn!(error = %e, "child wait failed; reporting synthetic exit code 1");
                        let _ = s.exit_tx.send(Some(synthetic_exit_status(1)));
                    }
                }
                exit_published = true;
                // Don't break yet — let the PTY drain any final buffered output
                // via the read branch. The next Ok(0) will exit the loop.
            },
        }
    }

    // Teardown:
    //  - drop bcast_tx → existing subscribers observe RecvError::Closed.
    //  - cmd_rx falls out of scope when SessionState drops → cmd_tx sends fail
    //    on the GhosttyPty side, which we map to PtyError::Closed.
    *bcast_tx.borrow_mut() = None;
}

/// Drain queued PtyWriteFn output to the PTY master.
///
/// Called after every successful `terminal.vt_write` in case the VT parsed a
/// query (DA1/DSR/DECRQM/...) and produced a response. Drains are short and
/// synchronous most of the time; only blocks if the kernel write buffer is
/// full, which is rare for query responses (tens of bytes).
async fn flush_pending_writes(
    pending: &Rc<RefCell<VecDeque<Bytes>>>,
    pty: &mut pty_process::Pty,
) {
    loop {
        let chunk = pending.borrow_mut().pop_front();
        let Some(chunk) = chunk else { return; };
        if let Err(e) = pty.write_all(&chunk).await {
            tracing::warn!(error = %e, "PtyWriteFn flush failed; dropping response");
            return;
        }
    }
}

/// Reply Closed (via Backend wrapping a synthetic IO error) to any commands
/// the caller has queued before they discover the worker has failed to start.
/// Called from the Terminal-construction error paths.
fn drain_commands_with_construction_error(cmd_rx: &flume::Receiver<Command>) {
    let make_err = || PtyError::Backend {
        source: Box::new(std::io::Error::other("VT terminal construction failed")),
    };
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Shutdown => {}
            Command::Subscribe { reply } => {
                let _ = reply.send(Err(make_err()));
            }
            Command::Resize { reply, .. } => {
                let _ = reply.send(Err(make_err()));
            }
            Command::Size { reply } => {
                let _ = reply.send(Err(make_err()));
            }
            Command::Write { reply, .. } => {
                let _ = reply.send(Err(make_err()));
            }
        }
    }
}

/// Serialize the current Terminal state as a self-contained VT escape
/// sequence stream. Clients feed this to their local emulator (xterm.js,
/// ghostty-web, etc.) to reconstruct the visible screen + scrollback.
///
/// `None` is passed to `format_alloc` so libghostty uses its own default (C)
/// allocator; the returned bytes are immediately copied into a `bytes::Bytes`,
/// and the libghostty allocation is freed on drop.
fn format_snapshot(terminal: &libghostty_vt::Terminal) -> Result<Bytes, PtyError> {
    use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};

    let opts = FormatterOptions {
        format: Format::Vt,
        trim: false,
        unwrap: false,
    };
    let mut formatter = Formatter::new(terminal, opts)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
    let vt_bytes = formatter
        .format_alloc(None::<&libghostty_vt::alloc::Allocator<()>>)
        .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
    Ok(Bytes::copy_from_slice(&vt_bytes))
}

/// Construct a synthetic `std::process::ExitStatus` with the given exit code.
///
/// Used when `child.wait()` itself fails (rare). We surface this as a failing
/// exit so callers see the session as broken rather than reporting success.
#[cfg(unix)]
pub(super) fn synthetic_exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}
```

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p cairn-pty 2>&1 | tail -40
```

Expected: success. If compilation fails:
- Missing imports → add the import the compiler suggests.
- `pty_process::Command::env` taking the wrong key/value types → wrap `k`/`v` in `OsStr::new(...)`.
- `pty_process::Pty::resize` returning a different error type than `std::io::Error` → adapt the `.map_err` to handle the actual return type (likely `rustix::io::Errno` or similar).

If a fix takes more than two iterations to find, stop and ask the user — the dep version may have shifted.

- [ ] **Step 3: Run the lifecycle integration tests**

```bash
cargo test -p cairn-pty --test pty_lifecycle 2>&1 | tail -40
```

Expected: all three tests pass:
- `spawn_true_exits_cleanly`
- `kill_terminates_long_running_child`
- `write_after_exit_returns_closed`

The third test is the most timing-sensitive; if it fails, the post-exit normalisation in the dispatcher branch isn't firing. Recheck the `exit_published` gate and the `bcast_tx` Option-nulling on teardown.

- [ ] **Step 4: Run the I/O integration tests**

```bash
cargo test -p cairn-pty --test pty_io 2>&1 | tail -40
```

Expected: all seven tests pass:
- `echo_output_is_broadcast_to_subscribers`
- `size_reports_configured_dimensions`
- `write_delivers_bytes_to_child_stdin`
- `spawn_succeeds_with_terminal_attached`
- `late_subscriber_sees_prior_output_in_snapshot`
- `da1_query_gets_response_without_client`  ← critical: validates PtyWriteFn queue-flush
- `subscribers_observe_close_on_child_exit`

If `da1_query_gets_response_without_client` fails (the test reads `reply-len=0` instead of a non-zero length), the `flush_pending_writes` call after `vt_write` is not running or the queue is empty. Trace the issue:

1. Add `tracing::debug!("vt_write {} bytes, pending after = {}", chunk.len(), pending_writes.borrow().len())` after the vt_write call.
2. Run with `RUST_LOG=cairn_pty=debug cargo test --test pty_io da1_query_gets_response_without_client -- --nocapture`.
3. If `pending after = 0` always, the PtyWriteFn callback isn't being installed correctly — verify the `on_pty_write` call succeeded.
4. If `pending after > 0` but bytes don't reach the child, the `pty.write_all` in flush_pending_writes is failing silently.

- [ ] **Step 5: Run the resize test**

```bash
cargo test -p cairn-pty --test pty_resize 2>&1 | tail -20
```

Expected: `resize_updates_size_query` passes.

If the new `Size` size is reported but doesn't match what the kernel sees, the test may expose that we cached `current_size` but the kernel ioctl failed. Confirm `s.pty.resize(...)` returned Ok before updating `current_size`.

- [ ] **Step 6: Run unit tests inside cairn-pty**

```bash
cargo test -p cairn-pty --lib 2>&1 | tail -30
```

Expected: all `pty::tests::*` cases pass, including the `ghostty_pty_is_send_sync()` compile check.

- [ ] **Step 7: Run the full workspace**

```bash
cargo test --workspace 2>&1 | tail -30
```

Expected: green across the workspace.

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-pty/src/pty/ghostty/worker.rs
git commit -m "$(cat <<'EOF'
Rewrite session worker for pty-process (single-thread topology)

Collapses the previous three-thread topology (cairn-pty-waiter,
cairn-pty-reader, cairn-pty-session) into a single session thread
running current_thread tokio + LocalSet. PTY reads and writes use
pty_process::Pty's AsyncRead/AsyncWrite directly; child exit is
awaited via tokio::process::Child::wait() in the same select!.

PtyWriteFn (DA1/DSR responses) is wired through a per-task pending-
writes queue, drained after each vt_write call.
EOF
)"
```

---

## Task 4: Update doc comment in `types.rs`

**Files:**
- Modify: `crates/cairn-pty/src/pty/types.rs:18-19`

The comment about why we use `std::process::Command` still references portable-pty.

- [ ] **Step 1: Replace the comment**

Open `crates/cairn-pty/src/pty/types.rs` and find:

```rust
/// Construct via [`SpawnOptions::new`] with a configured [`std::process::Command`].
/// `std::process::Command` (not `tokio::process::Command`) because
/// `portable-pty::SlavePty::spawn_command` expects the std variant.
```

Replace with:

```rust
/// Construct via [`SpawnOptions::new`] with a configured [`std::process::Command`].
/// `std::process::Command` (not `tokio::process::Command`) because callers
/// configure argv/env/cwd here and the worker translates field-by-field into
/// `pty_process::Command` at spawn time.
```

- [ ] **Step 2: Verify compilation and tests**

```bash
cargo check -p cairn-pty && cargo test -p cairn-pty
```

Expected: green.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/src/pty/types.rs
git commit -m "$(cat <<'EOF'
Update SpawnOptions doc comment to reference pty-process

Comment still mentioned the portable-pty SlavePty::spawn_command rationale.
The actual reason we take std::process::Command remains the same (callers
configure argv/env/cwd via the familiar std API), but the downstream
translation target is now pty_process::Command.
EOF
)"
```

---

## Task 5: Drop `portable-pty` dependency

**Files:**
- Modify: `crates/cairn-pty/Cargo.toml`

Now that the rewrite is green, remove the no-longer-used dep.

- [ ] **Step 1: Remove the line**

Open `crates/cairn-pty/Cargo.toml` and delete:

```toml
portable-pty = "0.9"
```

- [ ] **Step 2: Verify compilation**

```bash
cargo check -p cairn-pty 2>&1 | tail -10
```

Expected: success. If you see `unresolved import portable_pty::...` somewhere, you missed a reference in Task 3 — grep for `portable_pty` and resolve it:

```bash
grep -rn 'portable[_-]pty' crates/cairn-pty/
```

The output should be empty (no remaining references).

- [ ] **Step 3: Verify all tests still pass**

```bash
cargo test -p cairn-pty 2>&1 | tail -20
```

Expected: all tests pass.

- [ ] **Step 4: Verify workspace lockfile no longer depends on portable-pty**

```bash
grep portable-pty Cargo.lock | head -5
```

Expected: empty output (or only transitive references unrelated to cairn-pty). If `cairn-pty` still pulls portable-pty transitively, run `cargo update -p portable-pty --precise '0.0.0'` won't help — instead `cargo update` to refresh the lock.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
Drop portable-pty dependency

Migration to pty-process is complete; portable-pty is no longer used by
cairn-pty. Lockfile updated to reflect the removal.
EOF
)"
```

---

## Task 6: Smoke-test the example

**Files:**
- None modified.

Verify the example binary still works end-to-end. The example exercises spawn, subscribe, write, and kill — the full happy path.

- [ ] **Step 1: Build the example**

```bash
cargo build -p cairn-pty --example echo 2>&1 | tail -10
```

Expected: builds clean.

- [ ] **Step 2: Run the example**

```bash
cargo run -p cairn-pty --example echo 2>&1 | tail -20
```

Expected output:
- A `snapshot length:` line with a non-zero number (the initial VT snapshot is non-empty even for a fresh bash session).
- A `received N bytes from bash` line where N > 0 (bash echoed our command and its output).
- The example exits cleanly after `kill`.

If the example hangs, kill it with Ctrl-C and check whether `pty.kill()` actually signalled the child. The most likely cause is that the dispatcher's Shutdown arm isn't awaiting `wait_fut`, so the thread doesn't exit.

If the example panics, the panic message will identify which expect() failed; trace it back to the missing setup.

- [ ] **Step 3: Final formatting + lint**

```bash
cargo fmt -p cairn-pty
cargo clippy -p cairn-pty --all-targets 2>&1 | tail -40
```

Expected: fmt produces no diff (the rewrite was already formatted); clippy is clean except for any pre-existing warnings not introduced by this migration.

If clippy flags new lints in worker.rs (e.g., unused `tracing` import, `clippy::let_underscore_future` on the `let _ = exit_tx.send(...)` calls), fix them inline.

- [ ] **Step 4: Commit any cleanup**

If steps 2 or 3 produced changes:

```bash
git add -A
git commit -m "$(cat <<'EOF'
Address fmt/clippy after pty-process migration

Mechanical cleanup; no behavioral changes beyond what Task 3 introduced.
EOF
)"
```

If no changes resulted, skip this commit.

---

## Verification checklist (end of plan)

- [ ] `cargo test -p cairn-pty` is green.
- [ ] `cargo test --workspace` is green.
- [ ] `cargo build -p cairn-pty --example echo` succeeds.
- [ ] `grep -rn 'portable[_-]pty' crates/cairn-pty/` returns nothing.
- [ ] `cargo clippy -p cairn-pty --all-targets` is clean for any new warnings.
- [ ] Thread inventory inspection (manual sanity check): in a debugger, on a running session, only one thread named `cairn-pty-session` exists. The `cairn-pty-reader` and `cairn-pty-waiter` threads from the previous architecture are gone.
- [ ] `git log` shows one commit per task above (six commits total, or fewer if Task 6 Step 4 was a no-op).

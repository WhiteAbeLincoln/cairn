# libghostty Callback Set Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Install the libghostty-vt embedder callbacks needed to gate
backend query auto-replies when a client emulator is attached, brand
cairn's XTVERSION identity, and cover the Tier-2 queries libghostty
silently drops. Expose attach state to clients via a `Subscription`
RAII guard.

**Architecture:** A shared `Arc<AtomicUsize>` ("primary count") lives
on the worker thread and is read by libghostty callbacks inside
`vt_write` to decide whether the backend should answer terminal
queries. The same `Arc` is cloned into a private guard on each
`Subscription`; the guard increments on construction (inside the
worker's `Command::Subscribe` arm) and decrements on drop. Three new
callbacks are installed (`on_xtversion`, `on_size`, `on_color_scheme`)
and the existing `on_pty_write` is gated. `on_device_attributes` is
intentionally not installed — libghostty's DA1/DA2/DA3 defaults are
acceptable and the gate on `on_pty_write` covers suppression.

**Tech Stack:** Rust 2024, tokio (current_thread runtime), flume,
libghostty-vt 0.1.1, pty-process 0.4, bytes 1.

**Spec:** `docs/superpowers/specs/2026-05-22-libghostty-callbacks-design.md`

---

## File Structure

- **Modify:** `crates/cairn-pty/src/subscription.rs` — add
  `PrimaryGuard` and `Subscription::new` constructor.
- **Modify:** `crates/cairn-pty/src/ghostty/worker.rs` — construct
  `primary_count`, convert `current_size` to `Rc<Cell<_>>`, install
  callbacks, increment counter in `Command::Subscribe`.
- **Modify:** `crates/cairn-pty/src/lib.rs` — update two internal
  tests (`subscription_constructs_from_parts`, `StubSession::subscribe`)
  to use `Subscription::new`.
- **Modify:** `crates/cairn-pty/tests/pty_io.rs` — rewrite
  `da1_query_gets_response_without_client` to be race-free; add new
  `da1_query_suppressed_when_client_attached` test.
- **Create:** `crates/cairn-pty/tests/callback_gating.rs` — unit-level
  callback gating tests against a bare `Terminal`.

---

### Task 1: Make existing DA1 test race-free

The current `da1_query_gets_response_without_client` test
(`tests/pty_io.rs:161-194`) subscribes before the child script issues
its DA1 query. After Task 4 wires the counter into Subscribe, this
race becomes a real bug source: if subscribe wins (likely), count == 1
during the query, the gate suppresses the reply, the script's `read`
times out, and the test fails. Rewrite the test to subscribe *after*
the child exits, reading the result from the snapshot. The test still
passes today (libghostty's defaults reply via `on_pty_write` regardless
of subscriber count) and will continue to pass after the gate is added
(because count == 0 at the time the script issues the query — no
subscribers exist yet).

**Files:**
- Modify: `crates/cairn-pty/tests/pty_io.rs:161-194`

- [ ] **Step 1: Replace the existing test body**

Replace the entire `da1_query_gets_response_without_client` function
body (lines 161-194) with:

```rust
#[tokio::test]
async fn da1_query_gets_response_without_client() {
    // Verifies the count == 0 path end-to-end: with no subscriber
    // attached, libghostty's default DA1 reply (\x1b[?62;22c) flows
    // through the PTY to the child, the child's `read` returns the
    // reply bytes, and we observe a non-zero reply length.
    //
    // Race-free: we wait for the child to exit BEFORE subscribing,
    // so the entire query/reply roundtrip happens with count == 0.
    // The post-exit snapshot contains the child's final stdout
    // (`reply-len=N`) which we parse.
    let script = r#"
        printf '\033[c'
        read -r -n 32 -t 1 reply
        printf 'reply-len=%d\n' "${#reply}"
    "#;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Wait for the child to finish the whole script. After this, the
    // snapshot captures the final terminal state including the
    // `reply-len=N` line.
    let _ = pty.wait().await;

    let sub = pty.subscribe().await.expect("subscribe");
    let text = std::str::from_utf8(&sub.snapshot).unwrap_or("<non-utf8>");
    assert!(
        text.contains("reply-len="),
        "missing reply-len marker in snapshot: {text}"
    );
    assert!(
        !text.contains("reply-len=0"),
        "expected non-zero reply length (terminal responded to DA1), got: {text}"
    );
}
```

- [ ] **Step 2: Run the rewritten test**

Run: `cargo test --package cairn-pty --test pty_io da1_query_gets_response_without_client`
Expected: 1 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/pty_io.rs
git commit -m "$(cat <<'EOF'
Make existing DA1 test race-free ahead of callback gating

Subscribe after child exit and read from the post-exit snapshot so
the test no longer depends on whether subscribe lands before the
child issues its DA1 query. Same assertion semantics; removes a race
that becomes load-bearing once Subscribe increments the primary
count.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Add PrimaryGuard and Subscription::new constructor

Introduce the RAII counter primitive. Add an internal constructor that
increments the counter and wraps it in a guard whose drop decrements.
Update the two in-crate tests that construct `Subscription` literals.

**Files:**
- Modify: `crates/cairn-pty/src/subscription.rs`
- Modify: `crates/cairn-pty/src/lib.rs` (test code only)

- [ ] **Step 1: Write the failing unit tests for the constructor and guard**

Append the following test module to `crates/cairn-pty/src/subscription.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    #[test]
    fn new_increments_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let _sub = Subscription::new(Bytes::new(), rx, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drop_decrements_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let sub = Subscription::new(Bytes::new(), rx, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn multiple_subscriptions_share_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx1) = broadcast::channel::<Bytes>(1);
        let (_tx2, rx2) = broadcast::channel::<Bytes>(1);
        let sub1 = Subscription::new(Bytes::new(), rx1, counter.clone());
        let sub2 = Subscription::new(Bytes::new(), rx2, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        drop(sub1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub2);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --package cairn-pty --lib subscription::tests`
Expected: compile error — `Subscription::new` does not exist.

- [ ] **Step 3: Implement PrimaryGuard and Subscription::new**

Replace the full contents of `crates/cairn-pty/src/subscription.rs` with:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
///
/// While a `Subscription` is alive, the worker treats this client as a
/// "primary" attached emulator: backend auto-replies to terminal queries
/// (DA, XTVERSION, DSR, etc.) are suppressed so the client's emulator
/// can answer instead. The primary count returns to zero when this
/// Subscription is dropped.
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
    _primary_guard: PrimaryGuard,
}

impl Subscription {
    /// Construct a Subscription, incrementing `primary_count` and binding
    /// the matching decrement to this value's drop.
    ///
    /// `pub(crate)` because external callers receive Subscriptions from
    /// [`crate::pty::PtySession::subscribe`]; they never construct them
    /// directly. The constructor encapsulates the count-increment
    /// invariant so internal call sites cannot forget it.
    pub(crate) fn new(
        snapshot: Bytes,
        stream: broadcast::Receiver<Bytes>,
        primary_count: Arc<AtomicUsize>,
    ) -> Self {
        primary_count.fetch_add(1, Ordering::Relaxed);
        Self {
            snapshot,
            stream,
            _primary_guard: PrimaryGuard(primary_count),
        }
    }
}

/// RAII guard that decrements the primary-attached counter on drop.
pub(crate) struct PrimaryGuard(Arc<AtomicUsize>);

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    #[test]
    fn new_increments_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let _sub = Subscription::new(Bytes::new(), rx, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drop_decrements_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let sub = Subscription::new(Bytes::new(), rx, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn multiple_subscriptions_share_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx1) = broadcast::channel::<Bytes>(1);
        let (_tx2, rx2) = broadcast::channel::<Bytes>(1);
        let sub1 = Subscription::new(Bytes::new(), rx1, counter.clone());
        let sub2 = Subscription::new(Bytes::new(), rx2, counter.clone());
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        drop(sub1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub2);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }
}
```

- [ ] **Step 4: Update lib.rs tests that construct Subscription literals**

The existing in-crate tests at `crates/cairn-pty/src/lib.rs` construct
`Subscription { snapshot, stream }` literally — which no longer
compiles because of the new private `_primary_guard` field.

Update `subscription_constructs_from_parts` at `crates/cairn-pty/src/lib.rs:86-98`:

Replace this block:
```rust
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
```

With this:
```rust
    #[test]
    fn subscription_constructs_from_parts() {
        use bytes::Bytes;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use tokio::sync::broadcast;

        let (tx, rx) = broadcast::channel::<Bytes>(4);
        let snap = Bytes::from_static(b"\x1b[2J");
        let counter = Arc::new(AtomicUsize::new(0));
        let sub = Subscription::new(snap.clone(), rx, counter);
        assert_eq!(sub.snapshot, snap);
        drop(tx); // explicit drop so test asserts type accepts a Receiver
    }
```

Update `StubSession::subscribe` at `crates/cairn-pty/src/lib.rs:118-126`:

Replace this block:
```rust
        async fn subscribe(&self) -> Result<Subscription, PtyError> {
            use bytes::Bytes;
            use tokio::sync::broadcast;
            let (_tx, rx) = broadcast::channel(1);
            Ok(Subscription {
                snapshot: Bytes::new(),
                stream: rx,
            })
        }
```

With this:
```rust
        async fn subscribe(&self) -> Result<Subscription, PtyError> {
            use bytes::Bytes;
            use std::sync::Arc;
            use std::sync::atomic::AtomicUsize;
            use tokio::sync::broadcast;
            let (_tx, rx) = broadcast::channel(1);
            Ok(Subscription::new(
                Bytes::new(),
                rx,
                Arc::new(AtomicUsize::new(0)),
            ))
        }
```

- [ ] **Step 5: Run all tests to verify everything passes**

Run: `cargo test --package cairn-pty`
Expected: all previously passing tests still pass; the three new
subscription tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-pty/src/subscription.rs crates/cairn-pty/src/lib.rs
git commit -m "$(cat <<'EOF'
Subscription RAII guard for the primary-attached counter

Add PrimaryGuard and Subscription::new(snapshot, stream, counter) so
construction increments and drop decrements a shared Arc<AtomicUsize>.
The counter primitive is wired but not yet read by anything — that
arrives with the callback gating in a follow-up.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Convert worker's current_size to Rc<Cell<TermSize>>

The `on_size` callback (to be installed in Task 7) needs to read the
current cell grid synchronously from inside `vt_write`. Today
`current_size` is a plain `TermSize` local. Convert it to
`Rc<Cell<TermSize>>` now so subsequent tasks can capture clones without
restructuring. Same idiom as `pending_writes`; sound because the
LocalSet is single-threaded and no `.await` is held across `get`/`set`.

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`

- [ ] **Step 1: Make the conversion**

In `crates/cairn-pty/src/ghostty/worker.rs`:

Locate line 244 (inside `run_session`):
```rust
    // Cached size; updated on every successful resize. pty_process::Pty has
    // no get_size shortcut and we always set the size ourselves, so caching
    // is authoritative.
    let mut current_size = s.initial_size;
```

Replace with:
```rust
    // Cached size; updated on every successful resize. pty_process::Pty has
    // no get_size shortcut and we always set the size ourselves, so caching
    // is authoritative. Wrapped in Rc<Cell<_>> so the on_size libghostty
    // callback (installed below) can capture a clone and read it
    // synchronously inside vt_write.
    let current_size: Rc<Cell<TermSize>> = Rc::new(Cell::new(s.initial_size));
```

Then update the `Command::Resize` arm at lines 383-385 — locate:
```rust
                        if res.is_ok() {
                            current_size = size;
                        }
                        let _ = reply.send(res);
```

Replace with:
```rust
                        if res.is_ok() {
                            current_size.set(size);
                        }
                        let _ = reply.send(res);
```

And the `Command::Size` arm at line 389 — locate:
```rust
                    Command::Size { reply } => {
                        let _ = reply.send(Ok(current_size));
                    }
```

Replace with:
```rust
                    Command::Size { reply } => {
                        let _ = reply.send(Ok(current_size.get()));
                    }
```

Then add `Cell` to the imports near the top of the file. Locate line 9:
```rust
use std::cell::RefCell;
```

Replace with:
```rust
use std::cell::{Cell, RefCell};
```

- [ ] **Step 2: Run tests to verify no regressions**

Run: `cargo test --package cairn-pty`
Expected: all tests pass (this is a pure refactor of how the size is
stored; semantics unchanged).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/src/ghostty/worker.rs
git commit -m "$(cat <<'EOF'
Convert worker current_size to Rc<Cell<TermSize>> for callback capture

Lays groundwork for the on_size libghostty callback, which needs to
read the cell grid synchronously inside vt_write. Pure refactor;
existing size/resize semantics unchanged.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Worker constructs primary_count and increments on Subscribe

Add the `Arc<AtomicUsize>` to the worker. Increment it inside the
`Command::Subscribe` arm and construct the Subscription via the new
constructor. No callbacks read the counter yet — that comes in Tasks
5-8.

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`

- [ ] **Step 1: Add imports**

In `crates/cairn-pty/src/ghostty/worker.rs`, locate line 11:
```rust
use std::rc::Rc;
```

Replace with:
```rust
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
```

- [ ] **Step 2: Construct primary_count in run_session**

In `crates/cairn-pty/src/ghostty/worker.rs`, find the `pending_writes`
construction around line 206:
```rust
    let pending_writes: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();
```

Immediately after that line, add:
```rust

    // Shared counter of "primary attached" subscribers. Incremented in
    // the Command::Subscribe arm; decremented by the PrimaryGuard inside
    // each Subscription on drop. Read by libghostty callbacks
    // (installed below) to decide whether to emit backend replies.
    // Atomic (not Cell) so it can be cloned into Subscriptions, which
    // are Send.
    let primary_count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
```

- [ ] **Step 3: Use Subscription::new in the Command::Subscribe arm**

Locate `Command::Subscribe` at around line 355-371:
```rust
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
```

Replace the final `reply.send` line with the `Subscription::new` call:
```rust
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
                        let sub = Subscription::new(snapshot, stream, primary_count.clone());
                        let _ = reply.send(Ok(sub));
                    }
```

- [ ] **Step 4: Run tests to verify no regressions**

Run: `cargo test --package cairn-pty`
Expected: all tests pass. No behavior visible to callers yet; this just
wires the increment.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/src/ghostty/worker.rs
git commit -m "$(cat <<'EOF'
Worker increments primary_count on Subscribe

Adds the Arc<AtomicUsize> to run_session and threads it through
Command::Subscribe via Subscription::new. The counter is now live but
no libghostty callback reads it yet — that arrives task by task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Gate on_pty_write on primary_count

Add the counter check to the existing `on_pty_write` callback. This is
the load-bearing gate: with `primary_count >= 1`, libghostty's
automatic replies (DA1/DA2/DA3/DSR/DECRQM/XTVERSION) get dropped
instead of queued, so attached client emulators are the sole
answerers.

Two test layers:
- Unit tests in a new `tests/callback_gating.rs` exercise the callback
  in isolation against a bare `Terminal`.
- An integration test in `tests/pty_io.rs` verifies the gate
  end-to-end through the worker.

**Files:**
- Create: `crates/cairn-pty/tests/callback_gating.rs`
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Modify: `crates/cairn-pty/tests/pty_io.rs`

- [ ] **Step 1: Write the failing unit test**

Create `crates/cairn-pty/tests/callback_gating.rs` with the following
content (no `use` lines from other tests required; this is a fresh
test binary):

```rust
//! Unit-level tests of the libghostty-vt callbacks. Construct a bare
//! Terminal (no PTY, no worker), install the gated callbacks against
//! a hand-rolled Arc<AtomicUsize>, feed VT bytes via vt_write, and
//! assert what reaches the pending-writes queue.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};

/// Build a Terminal with on_pty_write gated on the supplied counter.
/// Returns the terminal and the pending-writes queue the callback
/// pushes into.
fn build_terminal_with_pty_gate(
    counter: Arc<AtomicUsize>,
) -> (
    Terminal<'static, 'static>,
    Rc<RefCell<VecDeque<Bytes>>>,
) {
    let pending: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();
    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");
    let pending_cb = pending.clone();
    let pc = counter.clone();
    term.on_pty_write(move |_t, data| {
        if pc.load(Ordering::Relaxed) == 0 {
            pending_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
        }
    })
    .expect("on_pty_write");
    (term, pending)
}

#[test]
fn on_pty_write_emits_da1_reply_when_no_primary() {
    let counter = Arc::new(AtomicUsize::new(0));
    let (mut term, pending) = build_terminal_with_pty_gate(counter);
    term.vt_write(b"\x1b[c"); // DA1
    let chunks: Vec<_> = pending.borrow_mut().drain(..).collect();
    assert_eq!(chunks.len(), 1, "expected one reply chunk, got {chunks:?}");
    assert_eq!(
        chunks[0].as_ref(),
        b"\x1b[?62;22c",
        "DA1 wire reply mismatch"
    );
}

#[test]
fn on_pty_write_suppresses_da1_reply_when_primary_attached() {
    let counter = Arc::new(AtomicUsize::new(1));
    let (mut term, pending) = build_terminal_with_pty_gate(counter);
    term.vt_write(b"\x1b[c"); // DA1
    assert!(
        pending.borrow().is_empty(),
        "expected no reply when count >= 1, got {:?}",
        pending.borrow()
    );
}

#[test]
fn on_pty_write_gates_decrqm_reply() {
    let counter = Arc::new(AtomicUsize::new(0));
    let (mut term, pending) = build_terminal_with_pty_gate(counter.clone());

    // Count == 0: DECRQM reply queued.
    term.vt_write(b"\x1b[?7$p");
    assert_eq!(pending.borrow().len(), 1, "DECRQM reply missing at count 0");
    pending.borrow_mut().clear();

    // Count == 1: DECRQM reply suppressed.
    counter.store(1, Ordering::Relaxed);
    term.vt_write(b"\x1b[?7$p");
    assert!(
        pending.borrow().is_empty(),
        "DECRQM reply leaked at count 1: {:?}",
        pending.borrow()
    );
}
```

- [ ] **Step 2: Run the unit tests to verify they fail or compile**

Run: `cargo test --package cairn-pty --test callback_gating`

Expected: tests compile (everything they need is already public on
`Terminal`); they pass *for the count == 0 cases* and pass for the
count == 1 cases too — because the gate logic is inside the test's own
callback closure, not the worker's. These tests document the desired
gating contract and serve as the model for the worker's installation.

(The actual gate gets installed in the worker in Step 3 below. Yes,
this means the tests don't "fail then pass" in the classic TDD shape
— they're documentation/contract tests for the callback pattern.)

- [ ] **Step 3: Install the gated on_pty_write in the worker**

In `crates/cairn-pty/src/ghostty/worker.rs`, locate the existing
`on_pty_write` installation at lines 223-232:

```rust
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
```

Replace with:

```rust
    let pending_for_cb = pending_writes.clone();
    let pc_for_pty_write = primary_count.clone();
    if let Err(e) = terminal.on_pty_write(move |_term, data| {
        // When a primary client (Subscription holder) is attached, the
        // client emulator is the authoritative answerer for queries
        // libghostty's parser would otherwise auto-reply to (DA1, DA2,
        // DA3, DSR cursor, DECRQM, XTVERSION). Suppressing the backend
        // reply here is the load-bearing half of query delegation; the
        // other half — broadcasting the original query bytes to the
        // client — happens unconditionally in the PTY-read arm.
        if pc_for_pty_write.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            pending_for_cb
                .borrow_mut()
                .push_back(Bytes::copy_from_slice(data));
        }
    }) {
        tracing::error!(error = ?e, "failed to install PtyWriteFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }
```

- [ ] **Step 4: Add the integration test for suppression**

In `crates/cairn-pty/tests/pty_io.rs`, append the following test
function:

```rust
#[tokio::test]
async fn da1_query_suppressed_when_client_attached() {
    // Verifies the count >= 1 path end-to-end: with a Subscription held
    // during the query, the worker's gate drops libghostty's default
    // DA1 reply, the child's `read` times out, and reply-len=0 lands
    // in the snapshot.
    //
    // The leading `sleep 0.2` ensures our subscribe call lands before
    // the child issues its DA1 query, so count == 1 at query time.
    let script = r#"
        sleep 0.2
        printf '\033[c'
        read -r -n 32 -t 1 reply
        printf 'reply-len=%d\n' "${#reply}"
    "#;
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg(script);
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 80, rows: 24 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    // Subscribe immediately — this is the "primary client" whose presence
    // suppresses backend auto-replies. Hold the Subscription across the
    // child's entire run by keeping `_sub` alive until after `wait`.
    let _sub = pty.subscribe().await.expect("subscribe");

    let _ = pty.wait().await;

    // Drop _sub by ending its scope after subscribing the late observer.
    let sub2 = pty.subscribe().await.expect("subscribe-2");
    let text = std::str::from_utf8(&sub2.snapshot).unwrap_or("<non-utf8>");
    assert!(
        text.contains("reply-len="),
        "missing reply-len marker in snapshot: {text}"
    );
    assert!(
        text.contains("reply-len=0"),
        "expected reply-len=0 (gate suppressed backend DA1 reply), got: {text}"
    );
}
```

- [ ] **Step 5: Run all tests to verify the gate is live**

Run: `cargo test --package cairn-pty`
Expected: all tests pass, including the new
`da1_query_suppressed_when_client_attached`.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-pty/tests/callback_gating.rs \
        crates/cairn-pty/src/ghostty/worker.rs \
        crates/cairn-pty/tests/pty_io.rs
git commit -m "$(cat <<'EOF'
Gate on_pty_write on primary_count

libghostty 0.1.1 auto-replies to DA1/DA2/DA3/DSR/DECRQM/XTVERSION via
on_pty_write today. Dropping those bytes when a primary client is
attached is the load-bearing half of query delegation — the broadcast
of the original query bytes (unchanged at worker.rs:288-290) is the
other half.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Install on_xtversion

Override libghostty's default `"libghostty"` XTVERSION reply with
`"cairn <CARGO_PKG_VERSION>"`. Same gate semantics as the other
callbacks — when a primary is attached, return None and let the
client emulator answer.

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Modify: `crates/cairn-pty/tests/callback_gating.rs`

- [ ] **Step 1: Add the unit test**

Append to `crates/cairn-pty/tests/callback_gating.rs`:

```rust
/// Build a Terminal with on_pty_write gated AND on_xtversion gated +
/// overridden. Returns the terminal and the pending-writes queue.
fn build_terminal_with_xtversion_override(
    counter: Arc<AtomicUsize>,
) -> (
    Terminal<'static, 'static>,
    Rc<RefCell<VecDeque<Bytes>>>,
) {
    let pending: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();
    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");

    let pending_cb = pending.clone();
    let pc_pty = counter.clone();
    term.on_pty_write(move |_t, data| {
        if pc_pty.load(Ordering::Relaxed) == 0 {
            pending_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
        }
    })
    .expect("on_pty_write");

    const XTVERSION_REPLY: &str = concat!("cairn ", env!("CARGO_PKG_VERSION"));
    let pc_xt = counter.clone();
    term.on_xtversion(move |_t| {
        if pc_xt.load(Ordering::Relaxed) == 0 {
            Some(XTVERSION_REPLY)
        } else {
            None
        }
    })
    .expect("on_xtversion");

    (term, pending)
}

#[test]
fn on_xtversion_overrides_default_when_no_primary() {
    let counter = Arc::new(AtomicUsize::new(0));
    let (mut term, pending) = build_terminal_with_xtversion_override(counter);
    term.vt_write(b"\x1b[>q"); // XTVERSION query
    let chunks: Vec<_> = pending.borrow_mut().drain(..).collect();
    assert_eq!(chunks.len(), 1, "expected one reply, got {chunks:?}");
    let reply = std::str::from_utf8(&chunks[0]).unwrap_or("<non-utf8>");
    assert!(
        reply.contains("cairn "),
        "reply should brand as cairn, got {reply:?}"
    );
    assert!(
        reply.contains(env!("CARGO_PKG_VERSION")),
        "reply should include the crate version, got {reply:?}"
    );
    assert!(
        !reply.contains("libghostty"),
        "default libghostty fingerprint leaked: {reply:?}"
    );
}

#[test]
fn on_xtversion_suppressed_when_primary_attached() {
    let counter = Arc::new(AtomicUsize::new(1));
    let (mut term, pending) = build_terminal_with_xtversion_override(counter);
    term.vt_write(b"\x1b[>q");
    assert!(
        pending.borrow().is_empty(),
        "expected no XTVERSION reply with count == 1, got {:?}",
        pending.borrow()
    );
}
```

- [ ] **Step 2: Run unit tests to verify the new ones pass**

Run: `cargo test --package cairn-pty --test callback_gating`
Expected: all five tests in this file pass (three from Task 5 + two
new ones from this task).

- [ ] **Step 3: Install on_xtversion in the worker**

In `crates/cairn-pty/src/ghostty/worker.rs`, locate the
`on_pty_write` installation completed in Task 5 (ends with `}` after
the `tracing::error!` block). Immediately after that closing `}`,
before the line `let terminal = Rc::new(RefCell::new(terminal));`,
insert the following:

```rust
    // Override libghostty's default XTVERSION reply ("libghostty") with
    // "cairn <version>". Gated on primary_count so attached client
    // emulators take over the response when present.
    const XTVERSION_REPLY: &str = concat!("cairn ", env!("CARGO_PKG_VERSION"));
    let pc_for_xtversion = primary_count.clone();
    if let Err(e) = terminal.on_xtversion(move |_term| {
        if pc_for_xtversion.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            Some(XTVERSION_REPLY)
        } else {
            None
        }
    }) {
        tracing::error!(error = ?e, "failed to install XtversionFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --package cairn-pty`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/tests/callback_gating.rs \
        crates/cairn-pty/src/ghostty/worker.rs
git commit -m "$(cat <<'EOF'
Override XTVERSION with cairn identity

Inferiors that query \x1b[>q now see "cairn <pkg_version>" instead of
libghostty's default "libghostty" string. Gated on primary_count so
attached client emulators take over when present.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Install on_size

XTWINOPS size queries (CSI 14/16/18 t) are silently dropped by
libghostty today because no `on_size` callback is installed. Add one
that reports the cached cell grid with synthetic pixel dimensions
(non-zero defaults to avoid divide-by-zero in image-protocol code).

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Modify: `crates/cairn-pty/tests/callback_gating.rs`

- [ ] **Step 1: Add the unit test**

Append to `crates/cairn-pty/tests/callback_gating.rs`:

```rust
use std::cell::Cell;
use cairn_pty::TermSize;

const DEFAULT_CELL_WIDTH_PX: u32 = 10;
const DEFAULT_CELL_HEIGHT_PX: u32 = 20;

/// Build a Terminal with on_pty_write + on_size gated. on_size reads
/// `size` for the cell grid and reports the synthetic pixel defaults.
fn build_terminal_with_size_callback(
    counter: Arc<AtomicUsize>,
    size: Rc<Cell<TermSize>>,
) -> (
    Terminal<'static, 'static>,
    Rc<RefCell<VecDeque<Bytes>>>,
) {
    use libghostty_vt::terminal::SizeReportSize;

    let pending: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();
    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");

    let pending_cb = pending.clone();
    let pc_pty = counter.clone();
    term.on_pty_write(move |_t, data| {
        if pc_pty.load(Ordering::Relaxed) == 0 {
            pending_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
        }
    })
    .expect("on_pty_write");

    let pc_size = counter.clone();
    let size_cb = size.clone();
    term.on_size(move |_t| {
        if pc_size.load(Ordering::Relaxed) == 0 {
            let s = size_cb.get();
            Some(SizeReportSize {
                rows: s.rows,
                columns: s.cols,
                cell_width: DEFAULT_CELL_WIDTH_PX,
                cell_height: DEFAULT_CELL_HEIGHT_PX,
            })
        } else {
            None
        }
    })
    .expect("on_size");

    (term, pending)
}

#[test]
fn on_size_reports_cell_grid_when_no_primary() {
    let counter = Arc::new(AtomicUsize::new(0));
    let size = Rc::new(Cell::new(TermSize { cols: 132, rows: 50 }));
    let (mut term, pending) = build_terminal_with_size_callback(counter, size);

    // CSI 18 t — report text area in chars. Wire form of reply:
    // CSI 8 ; rows ; cols t.
    term.vt_write(b"\x1b[18t");

    let chunks: Vec<_> = pending.borrow_mut().drain(..).collect();
    assert!(!chunks.is_empty(), "expected at least one reply chunk");
    let joined: Vec<u8> = chunks.iter().flat_map(|c| c.iter().copied()).collect();
    assert_eq!(
        joined.as_slice(),
        b"\x1b[8;50;132t",
        "unexpected XTWINOPS 18t reply"
    );
}

#[test]
fn on_size_suppressed_when_primary_attached() {
    let counter = Arc::new(AtomicUsize::new(1));
    let size = Rc::new(Cell::new(TermSize { cols: 80, rows: 24 }));
    let (mut term, pending) = build_terminal_with_size_callback(counter, size);
    term.vt_write(b"\x1b[18t");
    assert!(
        pending.borrow().is_empty(),
        "expected no reply with count == 1, got {:?}",
        pending.borrow()
    );
}
```

- [ ] **Step 2: Run the unit tests**

Run: `cargo test --package cairn-pty --test callback_gating`
Expected: previous tests still pass; both new tests pass.

- [ ] **Step 3: Install on_size in the worker**

In `crates/cairn-pty/src/ghostty/worker.rs`, find the `on_xtversion`
installation completed in Task 6 (ends with the `tracing::error!` and
`return;` block). Immediately after that closing brace, insert:

```rust
    // Answer XTWINOPS size queries (CSI 14/16/18 t). libghostty has no
    // default for these — without the callback they're silently
    // dropped. Pixel dimensions are placeholders; the backend has no
    // font. Real pixel sizes come from the client emulator once
    // attached. Non-zero defaults avoid divide-by-zero in image
    // protocol code paths.
    const DEFAULT_CELL_WIDTH_PX: u32 = 10;
    const DEFAULT_CELL_HEIGHT_PX: u32 = 20;
    let pc_for_size = primary_count.clone();
    let current_size_for_cb = current_size.clone();
    if let Err(e) = terminal.on_size(move |_term| {
        if pc_for_size.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            let size = current_size_for_cb.get();
            Some(libghostty_vt::terminal::SizeReportSize {
                rows: size.rows,
                columns: size.cols,
                cell_width: DEFAULT_CELL_WIDTH_PX,
                cell_height: DEFAULT_CELL_HEIGHT_PX,
            })
        } else {
            None
        }
    }) {
        tracing::error!(error = ?e, "failed to install SizeFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --package cairn-pty`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/tests/callback_gating.rs \
        crates/cairn-pty/src/ghostty/worker.rs
git commit -m "$(cat <<'EOF'
Install on_size callback for XTWINOPS queries

libghostty silently drops CSI 14/16/18 t without this callback. Now
the backend answers cell-grid queries from the cached current_size,
with synthetic non-zero pixel dimensions to keep image-protocol code
paths happy. Gated on primary_count.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8: Install on_color_scheme

Install the callback as an explicit `None` policy. libghostty drops
the query by default anyway; making the policy explicit means a
future change to "delegate to any attached observer" lives in one
place.

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Modify: `crates/cairn-pty/tests/callback_gating.rs`

- [ ] **Step 1: Add the unit test**

Append to `crates/cairn-pty/tests/callback_gating.rs`:

```rust
/// Build a Terminal with the on_color_scheme policy installed.
fn build_terminal_with_color_scheme(
    counter: Arc<AtomicUsize>,
) -> (
    Terminal<'static, 'static>,
    Rc<RefCell<VecDeque<Bytes>>>,
) {
    let pending: Rc<RefCell<VecDeque<Bytes>>> = Rc::default();
    let mut term = Terminal::new(TerminalOptions {
        cols: 80,
        rows: 24,
        max_scrollback: 100,
    })
    .expect("Terminal::new");

    let pending_cb = pending.clone();
    let pc_pty = counter.clone();
    term.on_pty_write(move |_t, data| {
        if pc_pty.load(Ordering::Relaxed) == 0 {
            pending_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
        }
    })
    .expect("on_pty_write");

    term.on_color_scheme(|_t| None).expect("on_color_scheme");

    (term, pending)
}

#[test]
fn on_color_scheme_never_replies_regardless_of_count() {
    for count in [0, 1] {
        let counter = Arc::new(AtomicUsize::new(count));
        let (mut term, pending) = build_terminal_with_color_scheme(counter);
        term.vt_write(b"\x1b[?996n"); // color scheme query
        assert!(
            pending.borrow().is_empty(),
            "expected no reply at count={count}, got {:?}",
            pending.borrow()
        );
    }
}
```

- [ ] **Step 2: Run unit tests**

Run: `cargo test --package cairn-pty --test callback_gating`
Expected: all tests pass.

- [ ] **Step 3: Install on_color_scheme in the worker**

In `crates/cairn-pty/src/ghostty/worker.rs`, after the `on_size`
installation completed in Task 7, insert:

```rust
    // Color scheme has no honest backend answer (no theme). Returning
    // None unconditionally is the documented policy. The callback is
    // installed (rather than left unset) so future changes to delegate
    // to attached observers live in one place.
    if let Err(e) = terminal.on_color_scheme(|_term| None) {
        tracing::error!(error = ?e, "failed to install ColorSchemeFn callback");
        drain_commands_with_construction_error(&s.cmd_rx);
        return;
    }
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --package cairn-pty`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/tests/callback_gating.rs \
        crates/cairn-pty/src/ghostty/worker.rs
git commit -m "$(cat <<'EOF'
Install on_color_scheme as explicit None policy

libghostty already drops CSI ?996n without a callback. Installing the
callback with an unconditional None makes the policy explicit and
gives future delegate-to-observer changes a single anchor point.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 9: Verify with clippy and fmt; full test sweep

Ensure no warnings, formatting, or full-suite regressions.

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --package cairn-pty --all-targets -- -D warnings`
Expected: no warnings.

If warnings appear, fix them in place. Common candidates:
- Unused imports (the `use` line for `Ordering` if you fully-qualify
  inside the closure)
- Needless `clone()` calls
- Long type signatures that clippy suggests aliasing

- [ ] **Step 2: Run fmt**

Run: `cargo fmt --package cairn-pty`
Expected: no changes (the code blocks in this plan are already
formatted). If there are changes, inspect them and commit if benign.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test --package cairn-pty`
Expected: all tests pass — including:
- `subscription::tests::new_increments_counter`
- `subscription::tests::drop_decrements_counter`
- `subscription::tests::multiple_subscriptions_share_counter`
- All eight tests in `tests/callback_gating.rs`
- The rewritten `da1_query_gets_response_without_client`
- The new `da1_query_suppressed_when_client_attached`
- All existing pty_io / pty_lifecycle / pty_resize tests

- [ ] **Step 4: Commit any fixups**

If clippy or fmt produced changes:
```bash
git add -A
git commit -m "$(cat <<'EOF'
Cleanup: clippy / fmt after callback installation

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

If neither produced changes, this task records the verification only —
no commit needed.

---

## Self-Review Notes

**Spec coverage:**
- Counter primitive (`Arc<AtomicUsize>`) → Task 4.
- Subscription RAII guard → Task 2.
- `on_pty_write` gating → Task 5.
- `on_xtversion` override + gate → Task 6.
- `on_size` Tier-2 coverage + gate → Task 7.
- `on_color_scheme` explicit policy → Task 8.
- `current_size` as `Rc<Cell<_>>` → Task 3.
- Unit-level callback gating tests → Tasks 5-8 build them up
  incrementally.
- End-to-end integration test for the `count >= 1` path → Task 5.
- Existing `da1_query_gets_response_without_client` made race-free →
  Task 1.
- `on_device_attributes` intentionally not installed (per revised
  spec) → no task; documented in the "Out-of-scope" section of the
  spec.

**Type consistency:**
- `PrimaryGuard` is the same name throughout (Task 2 spec text, Task
  2 implementation, Task 4 worker integration).
- `Subscription::new` signature `(snapshot: Bytes, stream:
  broadcast::Receiver<Bytes>, primary_count: Arc<AtomicUsize>) -> Self`
  is identical between Task 2's implementation and the call sites in
  Task 4 and the updated lib.rs tests.
- `DEFAULT_CELL_WIDTH_PX = 10`, `DEFAULT_CELL_HEIGHT_PX = 20` match
  between the spec, the unit test (Task 7), and the worker
  installation (Task 7).
- `XTVERSION_REPLY` uses `concat!("cairn ", env!("CARGO_PKG_VERSION"))`
  in both the unit test (Task 6) and the worker (Task 6).
- `Ordering::Relaxed` is used everywhere atomic operations appear.

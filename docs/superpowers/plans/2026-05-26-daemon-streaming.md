# Daemon Streaming Implementation Plan (cairn-daemon: attach / logs / send / wait)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the four streaming `sessions` operations in `cairn-daemon` — `attach` (bidi), `logs` (server-stream), `send` (client-stream), `wait` (future) — replacing the `unimplemented!("served in Plan 3")` stubs, and make the attached-client set real (so `kick`, `list`, `inspect` reflect attachers).

**Architecture:** Each streaming op is a thin handler over the `cairn-pty` `PtySession` surface. `attach` runs a per-connection bridge: subscribe → emit a `snapshot` server-event → then a spawned task `select!`s over (inbound `client-event` batches ⨯ the broadcast receiver ⨯ a `kick` oneshot), feeding a `tokio::sync::mpsc` whose `ReceiverStream` is the outbound `server-event` stream. An RAII `AttachGuard` registers the client in `SessionEntry.attached` and removes it on drop; the `Subscription` it holds clears leadership (via the worker's `Command::Detach`) and decrements the primary-count on drop. `logs` is the output-only sibling. `send` injects each chunk (non-promoting). `wait` returns a boxed future over `PtySession::wait()`.

**Tech Stack:** Rust 2024, tokio (mpsc, broadcast, select), `tokio-stream` (`ReceiverStream`), `futures` (`StreamExt`), `cairn-pty` (`PtySession`), `cairn-protocol` (generated `Handler` + value types).

**Spec:** `docs/superpowers/specs/2026-05-26-daemon-binary-design.md` (§ attach bridge, logs, send, wait). Depends on merged Plan 1 + Plan 2 (`cairn-daemon` core).

---

## Key facts to build against (read first)

- **Generated server-side `Handler` signatures** are pinned by `crates/cairn-protocol/tests/common/mod.rs` (the `StubHandler`). The daemon impls already exist in `crates/cairn-daemon/src/daemon.rs` with the four streaming methods as `unimplemented!("served in Plan 3")` — you replace those bodies. The signatures (context type is `crate::serve::ConnCtx`):
  - `attach(&self, ConnCtx, id: String, init: AttachInit, events: Pin<Box<dyn Stream<Item = Vec<ClientEvent>> + Send + 'static>>) -> anyhow::Result<Pin<Box<dyn Stream<Item = Vec<ServerEvent>> + Send + 'static>>>`
  - `logs(&self, ConnCtx, id: String, window: LogWindow, follow: bool) -> anyhow::Result<Pin<Box<dyn Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>>`
  - `send(&self, ConnCtx, id: String, chunks: Pin<Box<dyn Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>) -> anyhow::Result<Result<(), Error>>`
  - `wait(&self, ConnCtx, id: String) -> anyhow::Result<Pin<Box<dyn Future<Output = ExitStatus> + Send + 'static>>>` (the `ExitStatus` here is the **wire** `cairn_protocol::cairn::daemon::types::ExitStatus`).
- **`list<u8>` value payloads generate as `bytes::Bytes`.** Confirmed by `common/mod.rs`'s `logs` item type `Vec<bytes::Bytes>`. Therefore `ClientEvent::Input(Bytes)`, `ServerEvent::Output(Bytes)`, `ServerEvent::Snapshot(Bytes)` — and the broadcast channel already carries `Bytes`, and `PtySession::write`/`inject` take `Bytes` — so **no byte conversions are needed**. CONFIRM by reading the generated variants; if any payload is `Vec<u8>` instead, convert with `Bytes::from(_)` / `_.to_vec()`.
- **Variant shapes** (from `wit/cairn.wit`): `ClientEvent::{Input(Bytes), Resize((u16,u16)), Detach}`; `ServerEvent::{Snapshot(Bytes), Output(Bytes), Exited(ExitStatus), Error(Error)}`; `LogWindow::{Tail(u32), All}` (the `Since` variant was removed earlier).
- **`cairn-pty` surface** (merged): `PtySession` (object-safe, `Arc<dyn>`) with `subscribe(ClientId) -> Result<Subscription, PtyError>`, `write(ClientId, Bytes)`, `resize(ClientId, TermSize)`, `inject(Bytes)`, `wait() -> ExitStatus` (the cairn-pty struct with `.code()/.signal()/.unix_ms()`), `try_exit_status() -> Option<ExitStatus>`. `Subscription { snapshot: Bytes, stream: broadcast::Receiver<Bytes> }` (dropping it sends `Command::Detach` → clears leader if held, and decrements primary-count). `subscribe` does NOT claim leadership; `write`/`resize` promote on user-input/empty-seat.
- **Testing approach:** test the handler functions **directly** (call `handlers::attach::attach(&daemon, …)` etc. with `futures::stream::iter(...)` inputs against a real `GhosttyPty` session) and drain/inspect the returned stream/result. This exercises the real bridge logic without depending on the wRPC client's streaming-invocation API (which the unary tests already proved at the wire level). One subprocess smoke test covers the binary end-to-end.

---

## Task 1: Registry attach-tracking (`AttachGuard`) + shared `wire_exit` + deps

**Files:**
- Modify: `crates/cairn-daemon/src/registry.rs`
- Modify: `crates/cairn-daemon/src/handlers/mod.rs`
- Modify: `crates/cairn-daemon/Cargo.toml`
- Test: inline `#[cfg(test)]` in `registry.rs`

- [ ] **Step 1: Add deps**

In `crates/cairn-daemon/Cargo.toml` `[dependencies]`: `tokio-stream = "0.1"`. (`futures`, `tokio` with `sync`/`macros`/`time`, `bytes` are already present.)

- [ ] **Step 2: Write failing test**

```rust
#[tokio::test]
async fn attach_registers_then_guard_drop_removes() {
    let reg = SessionRegistry::new();
    let info = reg.create(spec(Some("dev")), "/bin/sh").await.expect("create");
    let entry = reg.resolve(&info.id).expect("resolve");
    let cid = reg.mint_client_id();

    let (_kick_rx, guard) = entry.attach(cid);
    assert_eq!(entry.attached_ids(), vec![cid.to_string()]);
    drop(guard);
    assert!(entry.attached_ids().is_empty());
}
```

(Add a small read helper `attached_ids()` on `SessionEntry` returning `Vec<String>` of the attached client ids, used by the test and by `session_info`.)

- [ ] **Step 3: Run — verify fail**

Run: `cargo test -p cairn-daemon registry::tests::attach_registers_then_guard_drop_removes`
Expected: FAIL (no `attach`/`AttachGuard`/`attached_ids`).

- [ ] **Step 4: Implement in `registry.rs`**

Add (near `AttachHandle`), using `tokio::sync::oneshot`:

```rust
/// RAII guard: removes the client from the entry's attached-set on drop.
pub struct AttachGuard {
    entry: Arc<SessionEntry>,
    client_id: ClientId,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.entry
            .attached
            .lock()
            .expect("attached lock")
            .remove(&self.client_id);
    }
}

impl SessionEntry {
    /// Register an attached client. Returns the kick receiver (fired by the
    /// `kick` op) and an RAII guard that deregisters on drop.
    pub fn attach(self: &Arc<Self>, client_id: ClientId) -> (oneshot::Receiver<()>, AttachGuard) {
        let (kick_tx, kick_rx) = oneshot::channel();
        self.attached
            .lock()
            .expect("attached lock")
            .insert(client_id, AttachHandle { kick: kick_tx });
        (kick_rx, AttachGuard { entry: Arc::clone(self), client_id })
    }

    /// Rendered ids of currently-attached clients (for list/inspect).
    pub fn attached_ids(&self) -> Vec<String> {
        self.attached
            .lock()
            .expect("attached lock")
            .keys()
            .map(|c| c.to_string())
            .collect()
    }
}
```

Refactor `session_info` to use `entry.attached_ids()` for `attached_clients` (instead of inlining the lock). Keep `now_unix_ms`/the rest unchanged.

- [ ] **Step 5: Add `wire_exit` to `handlers/mod.rs`**

```rust
pub mod attach;
pub mod logs;
pub mod meta;
pub mod send;
pub mod sessions;
pub mod wait;

use cairn_protocol::cairn::daemon::types::ExitStatus as WireExit;

/// Map a `cairn_pty::ExitStatus` to the wire `exit-status` record.
pub fn wire_exit(st: cairn_pty::ExitStatus) -> WireExit {
    WireExit {
        code: st.code(),
        signal: st.signal().map(|s| s as u8),
        unix_ms: st.unix_ms(),
    }
}
```

(Create empty `handlers/attach.rs`, `logs.rs`, `send.rs`, `wait.rs` so the module list compiles; they're filled in Tasks 2–5.)

- [ ] **Step 6: Run test + build**

Run: `cargo test -p cairn-daemon registry::` then `cargo build -p cairn-daemon`
Expected: PASS / compiles.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-daemon/src/registry.rs crates/cairn-daemon/src/handlers/mod.rs crates/cairn-daemon/Cargo.toml
git commit -m "feat(cairn-daemon): attach-tracking guard + wire_exit helper"
```

---

## Task 2: `wait` handler

**Files:**
- Modify: `crates/cairn-daemon/src/handlers/wait.rs`, `src/daemon.rs`
- Test: `crates/cairn-daemon/tests/daemon_streaming.rs` (new)

- [ ] **Step 1: Write failing direct test**

Create `crates/cairn-daemon/tests/daemon_streaming.rs` with a small helper to build a `Daemon` and a `SessionSpec`, then:

```rust
#[tokio::test]
async fn wait_resolves_with_exit_code() {
    let daemon = test_daemon();
    let info = create(&daemon, "w", &["sh", "-c", "exit 7"]).await;
    let fut = cairn_daemon::handlers::wait::wait(&daemon, info.id.clone())
        .await
        .expect("wait setup");
    let exit = fut.await;
    assert_eq!(exit.code, Some(7));
}

#[tokio::test]
async fn wait_unknown_is_err() {
    let daemon = test_daemon();
    assert!(cairn_daemon::handlers::wait::wait(&daemon, "nope".into()).await.is_err());
}
```

(Provide `test_daemon()` and `create(...)` test helpers in this file — `test_daemon()` builds `Daemon::new(DaemonConfig::default())` with a tempdir socket path it never binds (handlers don't need the socket); `create` calls `daemon.registry.create(spec, &daemon.cfg.default_shell).await.unwrap()`.)

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon --test daemon_streaming wait_`
Expected: FAIL.

- [ ] **Step 3: Implement `handlers/wait.rs`**

```rust
use std::future::Future;
use std::pin::Pin;

use cairn_protocol::cairn::daemon::types::ExitStatus as WireExit;

use crate::daemon::Daemon;
use crate::handlers::wire_exit;

/// `sessions.wait`: resolve the session, return a future that yields its exit
/// status. No in-band error channel (a bare `future<exit-status>`), so a
/// resolve miss is an outer transport error.
pub async fn wait(
    d: &Daemon,
    id: String,
) -> anyhow::Result<Pin<Box<dyn Future<Output = WireExit> + Send + 'static>>> {
    let entry = d
        .registry
        .resolve(&id)
        .ok_or_else(|| anyhow::anyhow!("session.not_found: {id}"))?;
    let handle = entry.handle();
    Ok(Box::pin(async move { wire_exit(handle.wait().await) }))
}
```

- [ ] **Step 4: Wire into `daemon.rs`**

Replace the `wait` method's `unimplemented!("served in Plan 3")` with:

```rust
    async fn wait(
        &self,
        _ctx: ConnCtx,
        id: String,
    ) -> anyhow::Result<std::pin::Pin<Box<dyn std::future::Future<Output = WireExit> + Send + 'static>>> {
        crate::handlers::wait::wait(self, id).await
    }
```

(Match the exact return type already present in the stub; `WireExit`/`ExitStatus` import as the stub had it.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p cairn-daemon --test daemon_streaming wait_`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon/src/handlers/wait.rs crates/cairn-daemon/src/daemon.rs crates/cairn-daemon/tests/daemon_streaming.rs
git commit -m "feat(cairn-daemon): implement sessions.wait"
```

---

## Task 3: `send` handler

**Files:**
- Modify: `crates/cairn-daemon/src/handlers/send.rs`, `src/daemon.rs`
- Test: `crates/cairn-daemon/tests/daemon_streaming.rs`

- [ ] **Step 1: Write failing direct test**

Verifies injected bytes reach the child AND that `send` does not claim leadership (a fresh client's resize still fails NotLeader after a prior leader, unaffected by the send). Simpler behavioral check: inject to a `cat` session and observe the echo via a `subscribe`.

```rust
#[tokio::test]
async fn send_injects_into_session() {
    use futures::StreamExt as _;
    let daemon = test_daemon();
    let info = create(&daemon, "s", &["cat"]).await;

    // Observer: subscribe directly via the registry handle so we can read echo.
    let entry = daemon.registry.resolve(&info.id).unwrap();
    let cid = daemon.registry.mint_client_id();
    let mut sub = entry.handle().subscribe(cid).await.unwrap();

    // send "ping\n" as one chunk.
    let chunks = futures::stream::iter(vec![vec![bytes::Bytes::from_static(b"ping\n")]]);
    let res = cairn_daemon::handlers::send::send(&daemon, info.id.clone(), Box::pin(chunks)).await;
    assert!(res.is_ok());

    // cat echoes; the broadcast should carry "ping" within a short window.
    let saw = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match sub.stream.recv().await {
                Ok(b) if b.windows(4).any(|w| w == b"ping") => return true,
                Ok(_) => continue,
                Err(_) => return false,
            }
        }
    }).await.unwrap_or(false);
    assert!(saw, "injected bytes should be echoed by cat");
}

#[tokio::test]
async fn send_unknown_is_not_found() {
    let chunks = futures::stream::iter(Vec::<Vec<bytes::Bytes>>::new());
    let daemon = test_daemon();
    let err = cairn_daemon::handlers::send::send(&daemon, "nope".into(), Box::pin(chunks))
        .await
        .expect_err("not found");
    assert_eq!(err.code, "session.not_found");
}
```

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon --test daemon_streaming send_`
Expected: FAIL.

- [ ] **Step 3: Implement `handlers/send.rs`**

```rust
use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;
use futures::StreamExt as _;

use cairn_protocol::cairn::daemon::types::Error as WireError;

use crate::daemon::Daemon;
use crate::error::{to_wire, DaemonError};

/// `sessions.send`: inject each streamed chunk into the PTY without claiming
/// leadership. Empty stream is a no-op. Returns once the input stream ends.
pub async fn send(
    d: &Daemon,
    id: String,
    mut chunks: Pin<Box<dyn Stream<Item = Vec<Bytes>> + Send + 'static>>,
) -> Result<(), WireError> {
    let entry = d.registry.resolve(&id).ok_or_else(|| DaemonError::NotFound.to_wire())?;
    let handle = entry.handle();
    while let Some(batch) = chunks.next().await {
        for chunk in batch {
            handle.inject(chunk).await.map_err(to_wire)?;
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Wire into `daemon.rs`**

Replace the `send` stub body:

```rust
    async fn send(
        &self,
        _ctx: ConnCtx,
        id: String,
        chunks: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>,
    ) -> anyhow::Result<Result<(), WireError>> {
        Ok(crate::handlers::send::send(self, id, chunks).await)
    }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p cairn-daemon --test daemon_streaming send_`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon/src/handlers/send.rs crates/cairn-daemon/src/daemon.rs crates/cairn-daemon/tests/daemon_streaming.rs
git commit -m "feat(cairn-daemon): implement sessions.send (non-promoting injection)"
```

---

## Task 4: `logs` handler

**Files:**
- Modify: `crates/cairn-daemon/src/handlers/logs.rs`, `src/daemon.rs`
- Test: `crates/cairn-daemon/tests/daemon_streaming.rs`

- [ ] **Step 1: Write failing direct test**

```rust
#[tokio::test]
async fn logs_without_follow_emits_snapshot_then_closes() {
    use futures::StreamExt as _;
    let daemon = test_daemon();
    // A session that prints something so the snapshot is non-trivial.
    let info = create(&daemon, "l", &["sh", "-c", "printf hello; sleep 100"]).await;
    // Give the child a moment to emit.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut stream = cairn_daemon::handlers::logs::logs(
        &daemon, info.id.clone(), LogWindow::All, false,
    ).await.expect("logs");

    // Collect everything; without follow it must terminate on its own.
    let mut bytes = Vec::new();
    let collected = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(batch) = stream.next().await {
            for chunk in batch { bytes.extend_from_slice(&chunk); }
        }
    }).await;
    assert!(collected.is_ok(), "logs without follow must terminate");
    assert!(!bytes.is_empty(), "snapshot should contain the printed output");
}

#[tokio::test]
async fn logs_unknown_is_err() {
    let daemon = test_daemon();
    assert!(cairn_daemon::handlers::logs::logs(&daemon, "nope".into(), LogWindow::All, false).await.is_err());
}
```

(Import `use cairn_protocol::cairn::daemon::types::LogWindow;` in the test.)

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon --test daemon_streaming logs_`
Expected: FAIL.

- [ ] **Step 3: Implement `handlers/logs.rs`**

Output-only sibling of `attach`: subscribe, emit the snapshot (windowed best-effort), then if `follow` forward the broadcast until close. Uses an mpsc + spawned task + `ReceiverStream`.

```rust
use std::pin::Pin;

use bytes::Bytes;
use futures::Stream;
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;

use cairn_protocol::cairn::daemon::types::LogWindow;

use crate::daemon::Daemon;

/// `sessions.logs`: emit the buffered snapshot (windowed best-effort), then, if
/// `follow`, the live output until the session closes. Output-only: no input,
/// no leader, raw `Bytes` chunks (no server-event tagging).
pub async fn logs(
    d: &Daemon,
    id: String,
    window: LogWindow,
    follow: bool,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = Vec<Bytes>> + Send + 'static>>> {
    let entry = d
        .registry
        .resolve(&id)
        .ok_or_else(|| anyhow::anyhow!("session.not_found: {id}"))?;
    let handle = entry.handle();
    let client_id = d.registry.mint_client_id();
    let sub = handle
        .subscribe(client_id)
        .await
        .map_err(|e| anyhow::anyhow!("subscribe failed: {e}"))?;

    let snapshot = apply_window(sub.snapshot.clone(), &window);
    let mut rx = sub.stream;
    let (tx, out) = tokio::sync::mpsc::channel::<Vec<Bytes>>(32);

    tokio::spawn(async move {
        // Keep `sub`'s guard alive for the task's lifetime by moving the
        // receiver's owner: hold `sub` (it owns the Subscription guard).
        let _keepalive = (); // `rx` was moved out of sub above; see note below.
        if tx.send(vec![snapshot]).await.is_err() {
            return;
        }
        if !follow {
            return;
        }
        loop {
            match rx.recv().await {
                Ok(b) => {
                    if tx.send(vec![b]).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => continue, // logs tolerates gaps; keep following
                Err(RecvError::Closed) => return,      // session ended
            }
        }
    });

    Ok(Box::pin(ReceiverStream::new(out)))
}

/// Apply a `log-window` to the snapshot bytes (best-effort, line-based).
fn apply_window(snapshot: Bytes, window: &LogWindow) -> Bytes {
    match window {
        LogWindow::All => snapshot,
        LogWindow::Tail(n) => tail_lines(&snapshot, *n as usize),
    }
}

fn tail_lines(bytes: &[u8], n: usize) -> Bytes {
    if n == 0 {
        return Bytes::new();
    }
    // Index of the start of the last `n` lines.
    let mut newlines = 0usize;
    let mut start = bytes.len();
    for i in (0..bytes.len()).rev() {
        if bytes[i] == b'\n' {
            newlines += 1;
            if newlines > n {
                start = i + 1;
                break;
            }
            start = i;
        } else {
            start = i;
        }
    }
    Bytes::copy_from_slice(&bytes[start..])
}
```

**Important fix for the keepalive:** moving `rx = sub.stream` drops the rest of `sub` (including its leadership/primary-count guard) immediately. Instead, keep the whole `Subscription` alive in the task and call `sub.stream.recv()` on the field. Adjust: don't destructure — move `sub` into the task and use `sub.stream.recv()`; read `sub.snapshot` before moving. Concretely:

```rust
    let snapshot = apply_window(sub.snapshot.clone(), &window);
    let (tx, out) = tokio::sync::mpsc::channel::<Vec<Bytes>>(32);
    tokio::spawn(async move {
        let mut sub = sub; // hold the Subscription (its guard) for the task's life
        if tx.send(vec![snapshot]).await.is_err() { return; }
        if !follow { return; }
        loop {
            match sub.stream.recv().await {
                Ok(b) => { if tx.send(vec![b]).await.is_err() { return; } }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
        }
    });
```

Use this second form (drop the `let mut rx = sub.stream;` line and the `_keepalive` placeholder).

- [ ] **Step 4: Wire into `daemon.rs`**

Replace the `logs` stub body:

```rust
    async fn logs(
        &self,
        _ctx: ConnCtx,
        id: String,
        window: LogWindow,
        follow: bool,
    ) -> anyhow::Result<std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>> {
        crate::handlers::logs::logs(self, id, window, follow).await
    }
```

(Import `LogWindow` in `daemon.rs` as the stub had it.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p cairn-daemon --test daemon_streaming logs_`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon/src/handlers/logs.rs crates/cairn-daemon/src/daemon.rs crates/cairn-daemon/tests/daemon_streaming.rs
git commit -m "feat(cairn-daemon): implement sessions.logs (snapshot + optional follow)"
```

---

## Task 5: `attach` bridge

The bidi bridge. **Files:** `crates/cairn-daemon/src/handlers/attach.rs`, `src/daemon.rs`; tests in `tests/daemon_streaming.rs`.

- [ ] **Step 1: Write failing direct tests**

```rust
use futures::StreamExt as _;
use cairn_protocol::cairn::daemon::types::{AttachInit, ClientEvent, ServerEvent};

fn attach_init() -> AttachInit { AttachInit { cols: 80, rows: 24, no_stdin: false } }

// Drain the next server-event batch within a timeout, flattened.
async fn next_events(s: &mut (impl futures::Stream<Item = Vec<ServerEvent>> + Unpin)) -> Vec<ServerEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(2), s.next())
        .await.ok().flatten().unwrap_or_default()
}

#[tokio::test]
async fn attach_first_event_is_snapshot() {
    let daemon = test_daemon();
    let info = create(&daemon, "a", &["cat"]).await;
    let events = futures::stream::pending::<Vec<ClientEvent>>(); // no client input
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let first = next_events(&mut out).await;
    assert!(matches!(first.first(), Some(ServerEvent::Snapshot(_))), "first event must be Snapshot");
}

#[tokio::test]
async fn attach_input_is_echoed_as_output() {
    let daemon = test_daemon();
    let info = create(&daemon, "a", &["cat"]).await;
    // Send one Input batch then keep the stream open (pending).
    let events = futures::stream::once(async {
        vec![ClientEvent::Input(bytes::Bytes::from_static(b"hey\n"))]
    }).chain(futures::stream::pending());
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;
    // cat echoes "hey"; look for an Output event containing it.
    let mut saw = false;
    for _ in 0..10 {
        for ev in next_events(&mut out).await {
            if let ServerEvent::Output(b) = ev {
                if b.windows(3).any(|w| w == b"hey") { saw = true; }
            }
        }
        if saw { break; }
    }
    assert!(saw, "input should be echoed back as Output");
}

#[tokio::test]
async fn attach_detach_event_ends_stream() {
    let daemon = test_daemon();
    let info = create(&daemon, "a", &["cat"]).await;
    let events = futures::stream::once(async { vec![ClientEvent::Detach] });
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;
    // After Detach the stream must end (next() yields None within the timeout).
    let ended = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while out.next().await.is_some() {}
    }).await;
    assert!(ended.is_ok(), "stream should end after Detach");
}

#[tokio::test]
async fn attach_emits_exited_when_child_dies() {
    let daemon = test_daemon();
    let info = create(&daemon, "a", &["sh", "-c", "sleep 100"]).await;
    let events = futures::stream::pending::<Vec<ClientEvent>>();
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;
    // Kill via the registry handle; the bridge should emit Exited then end.
    daemon.registry.resolve(&info.id).unwrap().handle().signal(libc::SIGKILL).await.unwrap();
    let mut saw_exit = false;
    for _ in 0..20 {
        for ev in next_events(&mut out).await {
            if matches!(ev, ServerEvent::Exited(_)) { saw_exit = true; }
        }
        if saw_exit { break; }
    }
    assert!(saw_exit, "bridge should emit Exited when the child dies");
}

#[tokio::test]
async fn attach_unknown_session_yields_error_event() {
    let daemon = test_daemon();
    let events = futures::stream::pending::<Vec<ClientEvent>>();
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, "nope".into(), attach_init(), Box::pin(events),
    ).await;
    let first = next_events(&mut out).await;
    assert!(matches!(first.first(), Some(ServerEvent::Error(_))), "unknown id -> Error event");
}
```

(Add `libc = "0.2"` to `cairn-daemon` `[dev-dependencies]` if not already reachable in tests; it's a normal dep so it is.)

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon --test daemon_streaming attach_`
Expected: FAIL.

- [ ] **Step 3: Implement `handlers/attach.rs`**

```rust
use std::pin::Pin;

use bytes::Bytes;
use cairn_pty::TermSize;
use futures::{Stream, StreamExt as _};
use tokio::sync::broadcast::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;

use cairn_protocol::cairn::daemon::types::{AttachInit, ClientEvent, Error as WireError, ServerEvent};

use crate::daemon::Daemon;
use crate::handlers::wire_exit;

type ServerEvents = Pin<Box<dyn Stream<Item = Vec<ServerEvent>> + Send + 'static>>;
type ClientEvents = Pin<Box<dyn Stream<Item = Vec<ClientEvent>> + Send + 'static>>;

/// `sessions.attach`: the bidirectional bridge. Resolve, subscribe, emit a
/// `snapshot`, then bridge client-events ⨯ broadcast ⨯ kick onto the outbound
/// `server-event` stream. Errors are in-band (`server-event::error`).
pub async fn attach(d: &Daemon, id: String, init: AttachInit, events: ClientEvents) -> ServerEvents {
    let Some(entry) = d.registry.resolve(&id) else {
        return once_error("session.not_found", &format!("no such session: {id}"));
    };
    let client_id = d.registry.mint_client_id();
    let handle = entry.handle();

    // Leader-wins: the first interactive attacher claims the empty seat + sets
    // size; followers get NotLeader (ignored). Read-only attaches don't claim.
    if !init.no_stdin {
        let _ = handle.resize(client_id, TermSize { cols: init.cols, rows: init.rows }).await;
    }

    let sub = match handle.subscribe(client_id).await {
        Ok(s) => s,
        Err(e) => return once_error("pty.backend", &format!("subscribe failed: {e}")),
    };
    let (_kick_rx, guard) = entry.attach(client_id);
    let mut kick_rx = _kick_rx;

    let no_stdin = init.no_stdin;
    let (tx, out) = tokio::sync::mpsc::channel::<Vec<ServerEvent>>(32);

    tokio::spawn(async move {
        // Hold the attach guard (deregisters on drop) and the Subscription
        // (clears leadership + primary-count on drop) for the task's lifetime.
        let _guard = guard;
        let mut sub = sub;
        let mut events = events;

        if tx.send(vec![ServerEvent::Snapshot(sub.snapshot.clone())]).await.is_err() {
            return; // client already gone
        }

        loop {
            tokio::select! {
                ev = events.next() => match ev {
                    Some(batch) => {
                        for e in batch {
                            match e {
                                ClientEvent::Input(b) if !no_stdin => {
                                    if handle.write(client_id, b).await.is_err() { return; }
                                }
                                ClientEvent::Resize((c, r)) => {
                                    let _ = handle.resize(client_id, TermSize { cols: c, rows: r }).await;
                                }
                                ClientEvent::Detach => return,
                                _ => {} // Input while no_stdin: ignore
                            }
                        }
                    }
                    None => return, // client closed the inbound stream
                },
                out_chunk = sub.stream.recv() => match out_chunk {
                    Ok(bytes) => {
                        if tx.send(vec![ServerEvent::Output(bytes)]).await.is_err() { return; }
                    }
                    Err(RecvError::Lagged(_)) => return, // lag-kick: close -> client reattaches fresh
                    Err(RecvError::Closed) => {
                        // Child exited. wait() resolves immediately now.
                        let exit = wire_exit(handle.wait().await);
                        let _ = tx.send(vec![ServerEvent::Exited(exit)]).await;
                        return;
                    }
                },
                _ = &mut kick_rx => return, // evicted by the `kick` op
            }
        }
    });

    Box::pin(ReceiverStream::new(out))
}

/// A one-element stream carrying a single `server-event::error`, then close.
fn once_error(code: &str, message: &str) -> ServerEvents {
    let err = ServerEvent::Error(WireError { code: code.to_string(), message: message.to_string() });
    Box::pin(futures::stream::once(async move { vec![err] }))
}
```

(Note the `let mut kick_rx = _kick_rx;` is to allow `&mut kick_rx` in `select!`; name it `kick_rx` directly — `let (kick_rx, guard) = entry.attach(client_id); let mut kick_rx = kick_rx;` or `let (mut kick_rx, guard) = ...`. Use `let (mut kick_rx, guard) = entry.attach(client_id);` and drop the `_kick_rx` alias.)

- [ ] **Step 4: Wire into `daemon.rs`**

Replace the `attach` stub body (note: returns `Ok(...)` — attach errors are in-band, so the outer Result is always `Ok`):

```rust
    async fn attach(
        &self,
        _ctx: ConnCtx,
        id: String,
        init: AttachInit,
        events: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<ClientEvent>> + Send + 'static>>,
    ) -> anyhow::Result<std::pin::Pin<Box<dyn futures::Stream<Item = Vec<ServerEvent>> + Send + 'static>>> {
        Ok(crate::handlers::attach::attach(self, id, init, events).await)
    }
```

(Import `AttachInit`, `ClientEvent`, `ServerEvent` in `daemon.rs` as the stub had them.)

- [ ] **Step 5: Run tests**

Run: `cargo test -p cairn-daemon --test daemon_streaming attach_`
Expected: all PASS.

- [ ] **Step 6: Full crate test + clippy**

Run: `cargo test -p cairn-daemon && cargo clippy -p cairn-daemon --all-targets`
Expected: all PASS; clippy clean. (Confirm the unary `kick` integration test, if any populates `attached`, still behaves — the attached-set is now real.)

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-daemon/src/handlers/attach.rs crates/cairn-daemon/src/daemon.rs crates/cairn-daemon/tests/daemon_streaming.rs
git commit -m "feat(cairn-daemon): implement the sessions.attach bidi bridge"
```

---

## Task 6: Subprocess smoke test

Proves the actual `cairn-daemon` **binary** starts, binds, and serves a real wRPC client (unary `version`) — the zmx-BATS analog. (Streaming-over-the-wire is covered behaviorally by the direct handler tests; this proves packaging/startup/signal handling.)

**Files:** `crates/cairn-daemon/tests/smoke.rs` (new)

- [ ] **Step 1: Write the test**

```rust
//! Subprocess smoke test: spawn the real `cairn-daemon` binary and hit it.

use std::time::Duration;

#[tokio::test]
async fn binary_starts_and_serves_version() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("cairn").join("cairn.sock");

    // `CARGO_BIN_EXE_cairn-daemon` is set by cargo for integration tests.
    let bin = env!("CARGO_BIN_EXE_cairn-daemon");
    let mut child = tokio::process::Command::new(bin)
        .env("CAIRN_SOCKET", &socket)
        .env("CAIRN_LOG", "warn")
        .kill_on_drop(true)
        .spawn()
        .expect("spawn cairn-daemon");

    // Wait for the socket to appear.
    let mut ready = false;
    for _ in 0..100 {
        if socket.exists() { ready = true; break; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ready, "daemon did not create its socket");

    let client = wrpc_transport::unix::Client::from(socket.clone());
    let info = cairn_protocol::client::cairn::daemon::meta::version(&client, ())
        .await
        .expect("version invocation");
    assert!(info.daemon.starts_with("cairn-daemon/"));

    // Graceful shutdown via SIGTERM.
    let pid = child.id().expect("pid");
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p cairn-daemon --test smoke`
Expected: PASS. (If `CARGO_BIN_EXE_cairn-daemon` isn't set, confirm the `[[bin]]` name is `cairn-daemon` in `Cargo.toml` — the env var is `CARGO_BIN_EXE_<bin-name>`.)

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-daemon/tests/smoke.rs
git commit -m "test(cairn-daemon): subprocess smoke test for the daemon binary"
```

---

## Task 7: README build-list checkoff

**Files:** `docs/architecture/pty-session/README.md`

- [ ] **Step 1: Mark item 4 done**

In the "What needs to be built" list, update item 4 ("Daemon binary") to reflect that the UDS daemon (meta + unary + streaming) is implemented across Plans 2–3, keeping the deferred-work note (WebTransport, idle-timeout enforcement, max-session cap, metrics/debug endpoint, TOML config loader remain). Leave items 6 (auth — WT path), 8 (backpressure policy beyond lag-close), 9 (observability beyond tracing-to-stderr), 10 (broader daemon tests) as still-open as appropriate.

- [ ] **Step 2: Commit**

```bash
git add docs/architecture/pty-session/README.md
git commit -m "docs(pty-session): check off the UDS daemon binary (Plans 2-3)"
```

---

## Self-review checklist (run before handing off)

- [ ] **Spec coverage:** `attach` (bridge: snapshot-first, leader-claim on non-`no_stdin`, input→write, resize, detach→end, lag→close, exit→Exited, kick→end, unknown→in-band Error), `logs` (snapshot + windowed + optional follow), `send` (non-promoting inject), `wait` (future). Attached-set is now real (`kick`/`list`/`inspect` reflect attachers).
- [ ] **No placeholders:** the `logs` keepalive has TWO code blocks — use the SECOND (hold the whole `Subscription` in the task; do not destructure `sub.stream` out early, which would drop the leadership/primary-count guard). The attach `kick_rx` binding: use `let (mut kick_rx, guard) = entry.attach(client_id);`.
- [ ] **`Bytes` vs `Vec<u8>`:** verified the `list<u8>` payloads generate as `bytes::Bytes` (so no conversions). If the real generated type differs, convert at the boundaries.
- [ ] **Send + 'static:** every spawned task captures only `Send + 'static` values (`Arc<dyn PtySession>`, `Subscription`, the boxed input stream, `AttachGuard`, `oneshot::Receiver`); the returned `ReceiverStream` is `Send`.
- [ ] **Drop semantics:** when the outbound stream is dropped (client disconnect) or the loop returns (detach/kick/exit/lag), the task ends → `AttachGuard` drop deregisters from `attached`, and `Subscription` drop clears leadership + primary-count. Confirm no lock held across `.await` in any handler (locks are only taken inside `registry`/guard `Drop`, all sync).
- [ ] `cargo test -p cairn-daemon` (unit + `daemon_meta` + `daemon_unary` + `daemon_streaming` + `smoke`) green; `cargo clippy -p cairn-daemon --all-targets` clean; whole workspace builds.

When this lands, the UDS daemon is feature-complete for v0. Remaining roadmap (separate efforts): the `cairn` CLI client wiring (item 7), and the WebTransport transport + token auth (the deferred follow-up).

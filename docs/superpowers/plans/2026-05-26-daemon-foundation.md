# Daemon Foundation Implementation Plan (cairn-pty extensions + protocol kill-grace)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `cairn-pty` with the surface the daemon needs (signal delivery, blind input injection, exit timestamps, non-blocking exit peek, post-exit size) and add the `grace-ms` parameter to the protocol's `kill` operation — the prerequisites for the daemon binary (Plan 2).

**Architecture:** `cairn-pty` keeps its dedicated-thread `GhosttyPty` worker; we add two `Command` variants (`Signal`, `Inject`), change the worker's exit `watch` channel to carry an `ExitInfo { status, unix_ms }` stamped at exit-detection time, and promote `signal`/`inject`/`wait`/`try_exit_status` onto the `PtySession` trait so the daemon can hold `Arc<dyn PtySession>`. Signal delivery uses `libc::killpg` against the child's process group (the child is a session/group leader after `pty-process`'s `setsid`). The `kill` WIT op gains an optional `grace-ms` so the daemon (Plan 2) owns escalation.

**Tech Stack:** Rust 2024, tokio current-thread runtime + `LocalSet`, `flume` command channel, `tokio::sync::watch`/`broadcast`, `libghostty-vt`, `pty-process`, `libc`, `wit-bindgen-wrpc`.

**Spec:** `docs/superpowers/specs/2026-05-26-daemon-binary-design.md` (§ "cairn-pty changes" and § "Schema change: kill gains grace-ms").

---

## File structure

- `crates/cairn-protocol/wit/cairn.wit` — `kill` gains `grace-ms: option<u32>`.
- `crates/cairn-protocol/tests/round_trip.rs` — update the 3 `kill` stub signatures; add a `kill` round-trip test for the new param.
- `crates/cairn-pty/Cargo.toml` — add `libc`.
- `crates/cairn-pty/src/ghostty/mod.rs` — `Command::{Signal, Inject}`; `GhosttyPty` exit type, `wait()` → `ExitInfo`, `try_exit_status`, `signal`, `inject`; `ExitInfo` type + re-export.
- `crates/cairn-pty/src/ghostty/worker.rs` — exit channel carries `ExitInfo`; `Signal`/`Inject` arms; post-exit `Size` returns cached; drain handles new variants; stamp timestamp at every exit publish.
- `crates/cairn-pty/src/ghostty/process.rs` — `ChildProcess::id()` + production impl.
- `crates/cairn-pty/src/session.rs` — `PtySession` gains `signal`, `inject`, `wait`, `try_exit_status`.
- `crates/cairn-pty/src/lib.rs` — re-export `ExitInfo`.
- `crates/cairn-pty/tests/pty_lifecycle.rs` — update existing `wait()` call sites to `.status`.
- `crates/cairn-pty/tests/pty_signal.rs` — new: signal + inject integration tests against real children.

---

## Task 1: `kill` gains `grace-ms` in the WIT + round-trip test

**Files:**
- Modify: `crates/cairn-protocol/wit/cairn.wit`
- Modify: `crates/cairn-protocol/tests/round_trip.rs` (3 `kill` stub signatures)
- Test: `crates/cairn-protocol/tests/round_trip.rs` (new test)

- [ ] **Step 1: Edit the WIT**

In `crates/cairn-protocol/wit/cairn.wit`, change the `kill` signature in `interface sessions`:

```wit
    kill:    func(id: session-id, sig: signal, grace-ms: option<u32>) -> result<_, error>;
```

- [ ] **Step 2: Update the three existing `kill` stub signatures**

In `crates/cairn-protocol/tests/round_trip.rs` there are three `impl ... sessions::Handler for Stub` blocks (around lines 179, 321, 530). In each, change the `kill` method signature to add the parameter:

```rust
        async fn kill(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            _id: String,
            _sig: bindings::cairn::daemon::types::Signal,
            _grace_ms: Option<u32>,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            unimplemented!("not exercised by this test")
        }
```

(Leave the two stubs that aren't exercised as `unimplemented!`. Only the new test below gets a real body.)

- [ ] **Step 3: Write the failing round-trip test**

Append to `crates/cairn-protocol/tests/round_trip.rs`. This proves the new `option<u32>` parameter encodes across the wire, mirroring the existing `meta_authenticate_round_trips_error_variant` pattern. Provide a real `kill` body in this test's stub that echoes the received `grace_ms` back through the error message so we can assert it arrived.

```rust
#[tokio::test]
async fn sessions_kill_round_trips_grace_ms() {
    #[derive(Clone)]
    struct Stub;

    impl bindings::exports::cairn::daemon::sessions::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn kill(
            &self,
            _ctx: tokio::net::unix::SocketAddr,
            id: String,
            sig: bindings::cairn::daemon::types::Signal,
            grace_ms: Option<u32>,
        ) -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> {
            // Echo what we received back through the error channel so the
            // client can assert the params crossed the wire intact.
            let named = matches!(sig, bindings::cairn::daemon::types::Signal::Named(_));
            Ok(Err(bindings::cairn::daemon::types::Error {
                code: "echo".to_string(),
                message: format!("id={id} named={named} grace={grace_ms:?}"),
            }))
        }
        // All other sessions methods: unimplemented!("not exercised by this test")
        // (copy the unimplemented stubs from the other tests in this file)
        async fn list_all(&self, _c: tokio::net::unix::SocketAddr)
            -> anyhow::Result<Vec<bindings::cairn::daemon::types::SessionInfo>> { unimplemented!() }
        async fn inspect(&self, _c: tokio::net::unix::SocketAddr, _id: String)
            -> anyhow::Result<Result<bindings::cairn::daemon::types::SessionInfo, bindings::cairn::daemon::types::Error>> { unimplemented!() }
        async fn create(&self, _c: tokio::net::unix::SocketAddr, _s: bindings::cairn::daemon::types::SessionSpec)
            -> anyhow::Result<Result<bindings::cairn::daemon::types::SessionInfo, bindings::cairn::daemon::types::Error>> { unimplemented!() }
        async fn rename(&self, _c: tokio::net::unix::SocketAddr, _id: String, _n: String)
            -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> { unimplemented!() }
        async fn restart(&self, _c: tokio::net::unix::SocketAddr, _id: String, _f: bool)
            -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> { unimplemented!() }
        async fn kick(&self, _c: tokio::net::unix::SocketAddr, _id: String, _cl: Option<String>)
            -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> { unimplemented!() }
        async fn wait(&self, _c: tokio::net::unix::SocketAddr, _id: String)
            -> anyhow::Result<std::pin::Pin<Box<dyn std::future::Future<Output = bindings::cairn::daemon::types::ExitStatus> + Send + 'static>>> { unimplemented!() }
        async fn logs(&self, _c: tokio::net::unix::SocketAddr, _id: String, _w: bindings::cairn::daemon::types::LogWindow, _f: bool)
            -> anyhow::Result<std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>> { unimplemented!() }
        async fn attach(&self, _c: tokio::net::unix::SocketAddr, _id: String, _i: bindings::cairn::daemon::types::AttachInit, _e: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bindings::cairn::daemon::types::ClientEvent>> + Send + 'static>>)
            -> anyhow::Result<std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bindings::cairn::daemon::types::ServerEvent>> + Send + 'static>>> { unimplemented!() }
        async fn send(&self, _c: tokio::net::unix::SocketAddr, _id: String, _ch: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<bytes::Bytes>> + Send + 'static>>)
            -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> { unimplemented!() }
    }

    impl bindings::exports::cairn::daemon::meta::Handler<tokio::net::unix::SocketAddr> for Stub {
        async fn authenticate(&self, _c: tokio::net::unix::SocketAddr, _t: String)
            -> anyhow::Result<Result<(), bindings::cairn::daemon::types::Error>> { unimplemented!() }
        async fn whoami(&self, _c: tokio::net::unix::SocketAddr)
            -> anyhow::Result<Result<String, bindings::cairn::daemon::types::Error>> { unimplemented!() }
        async fn version(&self, _c: tokio::net::unix::SocketAddr)
            -> anyhow::Result<bindings::exports::cairn::daemon::meta::VersionInfo> { unimplemented!() }
    }

    let harness = spawn_server(Stub).await.expect("spawn_server");
    let sig = bindings::cairn::daemon::types::Signal::Named(
        bindings::cairn::daemon::types::SignalName::Term,
    );
    let res = bindings::client::cairn::daemon::sessions::kill(
        &harness.unix_client(), (), "dev", &sig, Some(5000u32),
    )
    .await
    .expect("kill invocation");
    let err = res.expect_err("stub echoes via Err");
    assert_eq!(err.message, "id=dev named=true grace=Some(5000)");
}
```

- [ ] **Step 4: Run the test — verify it fails first if you skipped the WIT edit, then passes**

Run: `cargo test -p cairn-protocol --test round_trip sessions_kill_round_trips_grace_ms -- --nocapture`
Expected: PASS (and the whole file still compiles, proving the three stub signatures match the regenerated bindings).

- [ ] **Step 5: Run the full protocol test suite**

Run: `cargo test -p cairn-protocol`
Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-protocol/wit/cairn.wit crates/cairn-protocol/tests/round_trip.rs
git commit -m "feat(protocol): add grace-ms to sessions.kill for daemon-owned escalation"
```

---

## Task 2: Add `libc` to cairn-pty

**Files:**
- Modify: `crates/cairn-pty/Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `crates/cairn-pty/Cargo.toml` `[dependencies]`, add (alphabetically near the other small deps):

```toml
libc = "0.2"
```

- [ ] **Step 2: Verify it resolves**

Run: `cargo check -p cairn-pty`
Expected: compiles (no usage yet).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/Cargo.toml
git commit -m "build(cairn-pty): add libc for process-group signal delivery"
```

---

## Task 3: `ExitInfo` type + worker stamps exit timestamp

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Modify: `crates/cairn-pty/src/ghostty/mod.rs`
- Modify: `crates/cairn-pty/src/lib.rs`
- Modify: `crates/cairn-pty/tests/pty_lifecycle.rs`
- Test: `crates/cairn-pty/tests/pty_lifecycle.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/cairn-pty/tests/pty_lifecycle.rs` (it already spawns real children via `GhosttyPty::spawn`):

```rust
#[tokio::test]
async fn wait_returns_exit_info_with_code_and_timestamp() {
    use cairn_pty::{GhosttyPty, SpawnOptions};
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg("exit 7");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let info = pty.wait().await;
    let after = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;

    assert_eq!(info.status.code(), Some(7));
    assert!(info.unix_ms >= before && info.unix_ms <= after,
        "exit timestamp {} not within [{before}, {after}]", info.unix_ms);
}
```

- [ ] **Step 2: Run it — verify it fails to compile**

Run: `cargo test -p cairn-pty --test pty_lifecycle wait_returns_exit_info_with_code_and_timestamp`
Expected: FAIL — `wait()` returns `ExitStatus` (no `.status`/`.unix_ms` fields).

- [ ] **Step 3: Define `ExitInfo` and stamp it in the worker**

In `crates/cairn-pty/src/ghostty/worker.rs`, just below `pub use std::process::ExitStatus;` (line 24), add:

```rust
/// Child exit status plus the wall-clock time exit was detected.
///
/// The timestamp is captured by the worker at exit-detection time so the
/// daemon can report `exit-status.unix-ms` for a session that exited while no
/// one was awaiting it.
#[derive(Debug, Clone, Copy)]
pub struct ExitInfo {
    pub status: ExitStatus,
    pub unix_ms: u64,
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn exit_info(status: ExitStatus) -> ExitInfo {
    ExitInfo { status, unix_ms: now_unix_ms() }
}
```

Change every exit-channel type and publish site in `worker.rs` from `ExitStatus` to `ExitInfo`:

- `WorkerHandles.exit_rx` field (line ~29): `tokio::sync::watch::Receiver<Option<ExitInfo>>`.
- `spawn` and `spawn_with` `watch::channel::<Option<ExitStatus>>(None)` (lines ~38, ~208) → `watch::channel::<Option<ExitInfo>>(None)`.
- `SessionState.exit_tx` (line ~246): `tokio::sync::watch::Sender<Option<ExitInfo>>`.
- The four `s.exit_tx.send(Some(status))` / synthetic publish sites — wrap with `exit_info(...)`:
  - EOF arm (line ~429): `let _ = s.exit_tx.send(Some(exit_info(status)));`
  - Read-error arm (line ~451): `let _ = s.exit_tx.send(Some(exit_info(status)));`
  - Shutdown arm (line ~502): `let _ = s.exit_tx.send(Some(exit_info(status)));`
  - `child.wait()` arm (lines ~614, ~617): `Ok(s_val) => { let _ = s.exit_tx.send(Some(exit_info(s_val))); }` and `Err(_) => { ... s.exit_tx.send(Some(exit_info(synthetic_exit_status(1)))); }`

- [ ] **Step 4: Update `GhosttyPty` to carry/return `ExitInfo`**

In `crates/cairn-pty/src/ghostty/mod.rs`:
- Re-export near the top: `pub use worker::{ExitInfo, ExitStatus};` (replace the existing `pub use worker::ExitStatus;`).
- Change the field (line ~53): `exit_rx: tokio::sync::watch::Receiver<Option<ExitInfo>>,`.
- Change `wait()` (lines ~89-104) to return `ExitInfo`:

```rust
    pub async fn wait(&self) -> ExitInfo {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(info) = *rx.borrow() {
                return info;
            }
            if rx.changed().await.is_err() {
                return (*rx.borrow())
                    .unwrap_or_else(|| worker::exit_info(worker::synthetic_exit_status(1)));
            }
        }
    }
```

(Make `exit_info` and `synthetic_exit_status` reachable: they're already `pub(super)`/module-private in `worker`; mark `exit_info` `pub(super)`.)

In `crates/cairn-pty/src/lib.rs`, change line 14 to re-export both:

```rust
pub use ghostty::{ExitInfo, ExitStatus};
```

- [ ] **Step 5: Fix existing `wait()` call sites in tests**

In `crates/cairn-pty/tests/pty_lifecycle.rs`, find every existing `.wait().await` whose result is used as an `ExitStatus` (e.g. `assert!(status.success())`, `status.code()`), and insert `.status`:
- `let status = pty.wait().await;` → `let status = pty.wait().await.status;`
(Search the file for `wait().await` and adjust each call site that touches `ExitStatus` methods.)

- [ ] **Step 6: Run the new test + the existing lifecycle suite**

Run: `cargo test -p cairn-pty --test pty_lifecycle`
Expected: all PASS, including `wait_returns_exit_info_with_code_and_timestamp`.

- [ ] **Step 7: Run the worker unit tests (they use the `watch` channel via mocks)**

Run: `cargo test -p cairn-pty`
Expected: all PASS. (The worker's `tests` module `MockChild` uses `watch::<Option<ExitStatus>>`; update those two `watch` types and the `*self.exit_rx.borrow()` reads if the compiler flags them — the mock publishes `synthetic_exit_status(0)`, so wrap with `exit_info(...)` there too.)

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-pty/src crates/cairn-pty/tests/pty_lifecycle.rs
git commit -m "feat(cairn-pty): carry exit timestamp via ExitInfo on the exit channel"
```

---

## Task 4: `try_exit_status` (non-blocking exit peek)

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/mod.rs`
- Test: `crates/cairn-pty/tests/pty_lifecycle.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn try_exit_status_is_none_before_exit_and_some_after() {
    use cairn_pty::{GhosttyPty, SpawnOptions};
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg("sleep 100");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    assert!(pty.try_exit_status().is_none(), "should be running");

    pty.kill().expect("kill");
    let _ = pty.wait().await; // ensure exit published
    assert!(pty.try_exit_status().is_some(), "should be exited");
}
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p cairn-pty --test pty_lifecycle try_exit_status_is_none_before_exit_and_some_after`
Expected: FAIL — `try_exit_status` does not exist.

- [ ] **Step 3: Implement on `GhosttyPty`**

In `crates/cairn-pty/src/ghostty/mod.rs`, add an inherent method (near `wait`):

```rust
    /// Non-blocking peek at the exit state. `None` while running.
    pub fn try_exit_status(&self) -> Option<ExitInfo> {
        *self.exit_rx.borrow()
    }
```

- [ ] **Step 4: Run it**

Run: `cargo test -p cairn-pty --test pty_lifecycle try_exit_status_is_none_before_exit_and_some_after`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/src/ghostty/mod.rs crates/cairn-pty/tests/pty_lifecycle.rs
git commit -m "feat(cairn-pty): add try_exit_status non-blocking exit peek"
```

---

## Task 5: `ChildProcess::id()` + `Command::Signal` + worker signal arm

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/process.rs`
- Modify: `crates/cairn-pty/src/ghostty/mod.rs`
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Test: `crates/cairn-pty/tests/pty_signal.rs` (new)

- [ ] **Step 1: Write the failing integration test**

Create `crates/cairn-pty/tests/pty_signal.rs`:

```rust
use std::os::unix::process::ExitStatusExt as _;
use std::time::Duration;

use cairn_pty::{ClientId, GhosttyPty, PtySession, SpawnOptions};

#[tokio::test]
async fn signal_term_kills_child() {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.arg("-c").arg("sleep 100");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    // SIGTERM == 15 on Linux and macOS.
    pty.signal(15).await.expect("signal");

    let info = tokio::time::timeout(Duration::from_secs(5), pty.wait())
        .await
        .expect("child should exit after SIGTERM");
    assert_eq!(info.status.signal(), Some(15), "child should die from SIGTERM");
}
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p cairn-pty --test pty_signal signal_term_kills_child`
Expected: FAIL — `PtySession::signal` does not exist (Task 7 adds the trait method; for now this won't compile, which is the expected failing state).

- [ ] **Step 3: Add `id()` to `ChildProcess`**

In `crates/cairn-pty/src/ghostty/process.rs`, add to the trait (after `start_kill`):

```rust
    /// PID of the child, or `None` once it has been reaped.
    fn id(&self) -> Option<u32>;
```

And to the production impl `impl ChildProcess for tokio::process::Child`:

```rust
    fn id(&self) -> Option<u32> {
        tokio::process::Child::id(self)
    }
```

- [ ] **Step 4: Add the `Signal` command variant**

In `crates/cairn-pty/src/ghostty/mod.rs`, add to `enum Command`:

```rust
    /// Deliver `sig` to the child's process group. Not leader-gated.
    Signal {
        sig: i32,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
```

- [ ] **Step 5: Handle `Signal` in the worker**

In `crates/cairn-pty/src/ghostty/worker.rs`, add a new arm inside the main `match cmd` block (alongside `Command::Write`):

```rust
                    Command::Signal { sig, reply } => {
                        let res = match s.child.id() {
                            Some(pid) => {
                                // The child is a session/process-group leader
                                // (pty-process setsid's it), so its pid is the
                                // pgid. Signal the whole group.
                                let rc = unsafe { libc::killpg(pid as libc::pid_t, sig) };
                                if rc == 0 {
                                    Ok(())
                                } else {
                                    Err(PtyError::from(std::io::Error::last_os_error()))
                                }
                            }
                            // Already reaped — desired state reached.
                            None => Ok(()),
                        };
                        let _ = reply.send(res);
                    }
```

In the **post-exit normalisation** block (the `if exit_published { match cmd { ... } }` near line 467), add:

```rust
                        Command::Signal { reply, .. } => {
                            // Child already dead — the requested state is reached.
                            let _ = reply.send(Ok(()));
                            continue;
                        }
```

In `drain_commands_with_construction_error` (near line 661), add:

```rust
            Command::Signal { reply, .. } => {
                let _ = reply.send(Err(make_err()));
            }
```

In the worker's `tests` module, add `id()` to `impl ChildProcess for MockChild`:

```rust
        fn id(&self) -> Option<u32> {
            None // mock has no real pid; signal arm treats None as no-op
        }
```

- [ ] **Step 6: Add `GhosttyPty::signal` inherent method**

In `crates/cairn-pty/src/ghostty/mod.rs`:

```rust
    /// Deliver a signal (libc number) to the child's process group.
    pub async fn signal(&self, sig: i32) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Signal { sig, reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }
```

(The `PtySession::signal` trait method is added in Task 7; this inherent method backs it. The Task-5 test calls `PtySession::signal`, so it stays red until Task 7 — that's expected; run the worker unit tests now to confirm the worker compiles.)

- [ ] **Step 7: Confirm the worker compiles and its unit tests pass**

Run: `cargo test -p cairn-pty --lib`
Expected: PASS (worker unit tests compile with the new `Signal` arm and `MockChild::id`).

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-pty/src crates/cairn-pty/tests/pty_signal.rs
git commit -m "feat(cairn-pty): add Command::Signal delivering to the child process group"
```

---

## Task 6: `Command::Inject` (blind, non-promoting write)

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/mod.rs`
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Test: `crates/cairn-pty/tests/pty_signal.rs`

- [ ] **Step 1: Write the failing integration test**

Append to `crates/cairn-pty/tests/pty_signal.rs`:

```rust
#[tokio::test]
async fn inject_writes_to_pty_without_claiming_leadership() {
    let mut cmd = tokio::process::Command::new("cat");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    // A types — becomes leader.
    pty.write(a, bytes::Bytes::from_static(b"hi\n")).await.expect("a writes");

    // Inject from no client identity — must NOT promote anyone.
    pty.inject(bytes::Bytes::from_static(b"yo\n")).await.expect("inject");

    // A is still the leader: B's resize is rejected as NotLeader(current = A).
    let err = pty
        .resize(b, cairn_pty::TermSize { cols: 100, rows: 30 })
        .await
        .expect_err("b should not be leader");
    match err {
        cairn_pty::PtyError::NotLeader { current, .. } => assert_eq!(current, Some(a)),
        other => panic!("expected NotLeader, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p cairn-pty --test pty_signal inject_writes_to_pty_without_claiming_leadership`
Expected: FAIL — `PtySession::inject` does not exist (added in Task 7); the worker `Command::Inject` arm is missing.

- [ ] **Step 3: Add the `Inject` command variant**

In `crates/cairn-pty/src/ghostty/mod.rs`, add to `enum Command`:

```rust
    /// Write to the PTY with no client identity and no leader promotion.
    /// Backs `cairn send`.
    Inject {
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
```

- [ ] **Step 4: Handle `Inject` in the worker**

In `crates/cairn-pty/src/ghostty/worker.rs`, add an arm in the main `match cmd` block:

```rust
                    Command::Inject { data, reply } => {
                        // No leader election: this is identity-less injection.
                        let res = s.pty.write_all(&data).await.map_err(PtyError::from);
                        let _ = reply.send(res);
                    }
```

In the post-exit block, add:

```rust
                        Command::Inject { reply, .. } => {
                            let _ = reply.send(Err(PtyError::Closed));
                            continue;
                        }
```

In `drain_commands_with_construction_error`, add:

```rust
            Command::Inject { reply, .. } => {
                let _ = reply.send(Err(make_err()));
            }
```

- [ ] **Step 5: Add `GhosttyPty::inject` inherent method**

In `crates/cairn-pty/src/ghostty/mod.rs`:

```rust
    /// Write bytes to the PTY without claiming leadership (backs `send`).
    pub async fn inject(&self, data: Bytes) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Inject { data, reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }
```

- [ ] **Step 6: Confirm the worker compiles + unit tests pass**

Run: `cargo test -p cairn-pty --lib`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-pty/src
git commit -m "feat(cairn-pty): add Command::Inject for non-promoting input injection"
```

---

## Task 7: Promote `signal`/`inject`/`wait`/`try_exit_status` onto `PtySession`

**Files:**
- Modify: `crates/cairn-pty/src/session.rs`
- Modify: `crates/cairn-pty/src/ghostty/mod.rs` (trait impl)
- Test: `crates/cairn-pty/tests/pty_signal.rs` (Tasks 5 & 6 tests now go green)

- [ ] **Step 1: Add the trait methods**

In `crates/cairn-pty/src/session.rs`, add to `trait PtySession` (import `ExitInfo`):

```rust
    /// Deliver a signal (libc number) to the child's process group. Not
    /// leader-gated. `Ok(())` if the child has already exited.
    async fn signal(&self, sig: i32) -> Result<(), PtyError>;

    /// Write bytes to the PTY with no client identity and no leader
    /// promotion. Backs `cairn send`.
    async fn inject(&self, data: Bytes) -> Result<(), PtyError>;

    /// Resolve when the child exits, returning status + exit timestamp.
    async fn wait(&self) -> ExitInfo;

    /// Non-blocking peek at exit state; `None` while running.
    fn try_exit_status(&self) -> Option<ExitInfo>;
```

Update the `use` line: `use super::{ClientId, ExitInfo, PtyError, Subscription, TermSize};`.

- [ ] **Step 2: Add the trait impls on `GhosttyPty`**

In `crates/cairn-pty/src/ghostty/mod.rs`, inside `impl super::PtySession for GhosttyPty`, forward to the inherent methods from Tasks 3–6:

```rust
    async fn signal(&self, sig: i32) -> Result<(), PtyError> {
        GhosttyPty::signal(self, sig).await
    }

    async fn inject(&self, data: bytes::Bytes) -> Result<(), PtyError> {
        GhosttyPty::inject(self, data).await
    }

    async fn wait(&self) -> super::ExitInfo {
        GhosttyPty::wait(self).await
    }

    fn try_exit_status(&self) -> Option<super::ExitInfo> {
        GhosttyPty::try_exit_status(self)
    }
```

- [ ] **Step 3: Run the signal + inject integration tests (now green)**

Run: `cargo test -p cairn-pty --test pty_signal`
Expected: PASS — both `signal_term_kills_child` and `inject_writes_to_pty_without_claiming_leadership`.

- [ ] **Step 4: Run the whole cairn-pty suite**

Run: `cargo test -p cairn-pty`
Expected: all PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/src
git commit -m "feat(cairn-pty): expose signal/inject/wait/try_exit_status on PtySession"
```

---

## Task 8: Post-exit `Size` returns the cached size

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`
- Modify: `crates/cairn-pty/src/session.rs` (doc only)
- Test: `crates/cairn-pty/tests/pty_lifecycle.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/cairn-pty/tests/pty_lifecycle.rs`:

```rust
#[tokio::test]
async fn size_returns_cached_value_after_exit() {
    use cairn_pty::{GhosttyPty, PtySession, SpawnOptions, TermSize};
    let mut cmd = tokio::process::Command::new("/bin/true");
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: 100, rows: 40 });
    let pty = GhosttyPty::spawn(opts).expect("spawn");

    let _ = pty.wait().await; // /bin/true exits immediately

    let size = pty.size().await.expect("size should still work post-exit");
    assert_eq!(size, TermSize { cols: 100, rows: 40 });
}
```

- [ ] **Step 2: Run it — verify it fails**

Run: `cargo test -p cairn-pty --test pty_lifecycle size_returns_cached_value_after_exit`
Expected: FAIL — post-exit `size()` returns `Err(PtyError::Closed)`.

- [ ] **Step 3: Relax the post-exit `Size` arm**

In `crates/cairn-pty/src/ghostty/worker.rs`, in the post-exit normalisation block, change the `Command::Size` arm from returning `Closed` to returning the cached size:

```rust
                        Command::Size { reply } => {
                            // In-memory read, no kernel call — safe post-exit.
                            let _ = reply.send(Ok(current_size.get()));
                            continue;
                        }
```

- [ ] **Step 4: Update the trait doc**

In `crates/cairn-pty/src/session.rs`, append to the `size` doc comment:

```rust
    /// ... Post-exit, returns the last-applied size rather than an error.
```

- [ ] **Step 5: Run it**

Run: `cargo test -p cairn-pty --test pty_lifecycle size_returns_cached_value_after_exit`
Expected: PASS.

- [ ] **Step 6: Run the full suite + a clippy pass**

Run: `cargo test -p cairn-pty && cargo clippy -p cairn-pty --all-targets`
Expected: tests PASS; clippy clean.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-pty/src
git commit -m "feat(cairn-pty): return cached size post-exit instead of Closed"
```

---

## Self-review checklist (run before handing off)

- [ ] **Spec coverage:** every item in the spec's "cairn-pty changes" table maps to a task — `signal` (T5/T7), `inject` (T6/T7), `wait`→`ExitInfo` (T3/T7), `try_exit_status` (T4/T7), post-exit `Size` (T8), exit timestamp (T3). `kill` `grace-ms` WIT change (T1). ✓
- [ ] **No placeholders:** every code step has complete code; every run step has an exact command + expected outcome.
- [ ] **Type consistency:** `ExitInfo { status, unix_ms }` used identically in worker, `GhosttyPty`, trait, and tests; `signal(i32)`, `inject(Bytes)`, `wait() -> ExitInfo`, `try_exit_status() -> Option<ExitInfo>` consistent across `session.rs` and `mod.rs`.
- [ ] `cargo test -p cairn-pty && cargo test -p cairn-protocol` both green at the end.

When Plan 1 is complete and merged, proceed to **Plan 2 — Daemon binary** (`2026-05-26-daemon-binary.md`).

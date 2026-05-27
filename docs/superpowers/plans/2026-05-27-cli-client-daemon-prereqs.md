# CLI Client — Daemon Prerequisites Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the three small daemon-side changes the interactive CLI client depends on, before building the client itself.

**Architecture:** Three independent edits to existing crates: (1) a shared error-code contract in `cairn-protocol` so the client can distinguish a recoverable lag-kick from a terminal operator-kick; (2) the daemon's attach bridge emits those distinct terminal events before closing; (3) the registry infers a default session name when the client omits one. A fourth task adds a regression test locking the already-correct "explicit env overrides inherited" behavior.

**Tech Stack:** Rust, tokio, `wit-bindgen-wrpc` generated bindings, `cargo-nextest`.

Spec: `docs/superpowers/specs/2026-05-27-cli-client-interactive-attach-design.md` (the "Daemon-side changes" section).

---

### Task 1: Shared error-code contract in `cairn-protocol`

**Files:**
- Modify: `crates/cairn-protocol/src/lib.rs`

These string constants are the contract between the daemon's attach bridge (Task 2) and the client's reconnect logic (separate client plan). Putting them in the shared protocol crate keeps both sides off hard-coded literals. No standalone test — a const-exists test would assert shape, not behavior; the value is exercised by Task 2's behavior test.

- [ ] **Step 1: Add the `error_codes` module**

Append to `crates/cairn-protocol/src/lib.rs` (after the existing `pub mod client { … }` block):

```rust
/// Machine-stable `types::error.code` values that carry protocol meaning beyond
/// a human message. Both the daemon (producer) and clients (consumers) reference
/// these instead of hard-coding the strings.
pub mod error_codes {
    /// Emitted on the `attach` stream when an operator `kick` evicts the client.
    /// Terminal: the client must NOT auto-reconnect.
    pub const CLIENT_KICKED: &str = "client.kicked";

    /// Emitted on the `attach` stream when the client is dropped for lagging
    /// behind the output broadcast. Recoverable: the client SHOULD reconnect and
    /// repaint from a fresh snapshot.
    pub const CLIENT_LAGGED: &str = "client.lagged";
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p cairn-protocol`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-protocol/src/lib.rs
git commit -m "feat(cairn-protocol): add client.kicked/client.lagged error-code contract

The CLI client auto-reconnects on transient attach-stream loss, so it must
distinguish a recoverable lag-kick from a terminal operator kick. Define the
codes once in the shared crate so daemon and client agree."
```

---

### Task 2: Attach bridge emits distinct kicked/lagged terminal events

**Files:**
- Modify: `crates/cairn-daemon/src/handlers/attach.rs:71-84` (the lag + kick `select!` arms)
- Test: `crates/cairn-daemon/tests/daemon_streaming.rs:226-247` (replace `kick_ends_attached_stream`)

Today both the lag arm (`handlers/attach.rs:75`) and the kick arm (`:83`) `return` with no final event, so the client can't tell them apart on the wire. Emit a distinct `server-event::error` first.

- [ ] **Step 1: Rewrite the test to assert the kicked event**

Replace the existing `kick_ends_attached_stream` test (`daemon_streaming.rs:226-247`) with:

```rust
#[tokio::test]
async fn kick_emits_kicked_event_then_ends() {
    let daemon = test_daemon();
    let info = create(&daemon, "a6", &["cat"]).await;
    let events = futures::stream::pending::<Vec<ClientEvent>>();
    let mut out = cairn_daemon::handlers::attach::attach(
        &daemon, info.id.clone(), attach_init(), Box::pin(events),
    ).await;
    let _snapshot = next_events(&mut out).await;

    // attach() registers the client synchronously before returning, so kick finds it.
    cairn_daemon::handlers::sessions::kick(&daemon, info.id.clone(), None)
        .await
        .expect("kick");

    // The bridge must emit Error{client.kicked} and then end the stream.
    let mut saw_kicked = false;
    let ended = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match out.next().await {
                Some(batch) => {
                    for ev in batch {
                        if let ServerEvent::Error(e) = ev
                            && e.code == cairn_protocol::error_codes::CLIENT_KICKED
                        {
                            saw_kicked = true;
                        }
                    }
                }
                None => break,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "kick should end the attached stream");
    assert!(saw_kicked, "kick should emit a client.kicked error event before ending");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p cairn-daemon kick_emits_kicked_event_then_ends`
Expected: FAIL — `saw_kicked` is false (the bridge emits no kicked event yet).

- [ ] **Step 3: Implement the kicked + lagged events**

In `crates/cairn-daemon/src/handlers/attach.rs`, change the lag and kick arms of the `select!` loop (currently lines 71-84). Replace:

```rust
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
```

with:

```rust
                out_chunk = sub.stream.recv() => match out_chunk {
                    Ok(bytes) => {
                        if tx.send(vec![ServerEvent::Output(bytes)]).await.is_err() { return; }
                    }
                    Err(RecvError::Lagged(_)) => {
                        // lag-kick: tell the client this is recoverable so it reattaches fresh.
                        let _ = tx.send(vec![ServerEvent::Error(WireError {
                            code: cairn_protocol::error_codes::CLIENT_LAGGED.to_string(),
                            message: "client fell behind output; reattach for a fresh snapshot".to_string(),
                        })]).await;
                        return;
                    }
                    Err(RecvError::Closed) => {
                        // Child exited. wait() resolves immediately now.
                        let exit = wire_exit(handle.wait().await);
                        let _ = tx.send(vec![ServerEvent::Exited(exit)]).await;
                        return;
                    }
                },
                _ = &mut kick_rx => {
                    // evicted by the `kick` op: terminal, client must not reconnect.
                    let _ = tx.send(vec![ServerEvent::Error(WireError {
                        code: cairn_protocol::error_codes::CLIENT_KICKED.to_string(),
                        message: "detached by operator".to_string(),
                    })]).await;
                    return;
                }
```

(`WireError` is already imported at `attach.rs:8`. `cairn_protocol` is a direct dependency.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p cairn-daemon kick_emits_kicked_event_then_ends`
Expected: PASS.

- [ ] **Step 5: Run the full attach test module to check for regressions**

Run: `cargo nextest run -p cairn-daemon --test daemon_streaming`
Expected: all pass (the other attach tests drain to `None` and tolerate the extra event).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon/src/handlers/attach.rs crates/cairn-daemon/tests/daemon_streaming.rs
git commit -m "feat(cairn-daemon): emit client.kicked/client.lagged before closing attach

A bare stream-close can't tell an auto-reconnecting client whether it was
operator-kicked (stay gone) or lag-kicked (reconnect). Emit a distinct
terminal error event for each so the client's reconnect rule is unambiguous."
```

---

### Task 3: Infer a default session name from the command

**Files:**
- Modify: `crates/cairn-daemon/src/registry.rs:134-160` (the `create` method) + add a helper
- Test: `crates/cairn-daemon/tests/` via the existing `registry.rs` unit-test module (add one test)

When `spec.name` is `None`, the daemon names the session `{basename}-{suffix}`, where `basename` is the file stem of the command (or default shell), and `suffix` is the **last 6 hex chars** of the session's UUIDv7 — its random tail, not the leading timestamp bits. Always appended (no instance counting); on the astronomically rare collision the suffix is extended.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/cairn-daemon/src/registry.rs` (after `duplicate_live_name_is_rejected`):

```rust
    #[tokio::test]
    async fn create_without_name_infers_basename_and_hex_suffix() {
        let reg = SessionRegistry::new();
        // spec(None) uses command ["sleep", "100"], so the basename is "sleep".
        let info = reg.create(spec(None), "/bin/sh").await.expect("create");
        let name = info.name.expect("a name should be inferred");
        let suffix = name
            .strip_prefix("sleep-")
            .unwrap_or_else(|| panic!("expected 'sleep-' prefix, got {name}"));
        assert_eq!(suffix.len(), 6, "suffix should be 6 hex chars: {name}");
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()), "suffix not hex: {name}");
        // The suffix is the tail of the session id's hex digits.
        let hex: String = info.id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        assert_eq!(suffix, &hex[hex.len() - 6..], "suffix must be the id's hex tail");
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo nextest run -p cairn-daemon create_without_name_infers_basename_and_hex_suffix`
Expected: FAIL — `info.name` is `None` (no inference yet), so `.expect("a name should be inferred")` panics.

- [ ] **Step 3: Implement inference in `create`**

In `crates/cairn-daemon/src/registry.rs`, replace the body of `create` (lines 134-160) with this version (mint the id first, then decide the name):

```rust
    /// Spawn a new session. Rejects an explicit name already used by a live
    /// session; infers `{basename}-{hex-tail}` when no name is given.
    pub async fn create(
        &self,
        spec: SessionSpec,
        default_shell: &str,
    ) -> Result<SessionInfo, DaemonError> {
        let id = uuid::Uuid::now_v7().to_string();
        let name = match &spec.name {
            Some(n) => {
                if self.resolve(n).is_some() {
                    return Err(DaemonError::NameInUse);
                }
                Some(n.clone())
            }
            None => Some(self.inferred_unique_name(&spec, default_shell, &id)),
        };
        let opts = options_from(spec.clone(), default_shell);
        let handle = GhosttyPty::spawn(opts).map_err(|_| DaemonError::SpawnFailed)?;
        let pid = None; // pid surfaced via inspect later if cairn-pty exposes it; None for v0
        let entry = Arc::new(SessionEntry {
            id: id.clone(),
            created_at_unix_ms: now_unix_ms(),
            spec: spec.clone(),
            name: Mutex::new(name),
            running: RwLock::new(Running { handle: Arc::new(handle), pid }),
            attached: Mutex::new(HashMap::new()),
        });
        // Build SessionInfo before inserting — lock is dropped before .await.
        let info = session_info(&entry).await;
        self.sessions.write().expect("sessions lock").insert(id, entry);
        Ok(info)
    }

    /// `{basename}-{hex-tail}`. `basename` is the command's file stem (or the
    /// default shell's); the suffix is the last 6 hex digits of `id` (UUIDv7's
    /// random tail — the leading digits are a shared millisecond timestamp).
    /// Always appended; extends the tail on the rare collision with a live name.
    fn inferred_unique_name(&self, spec: &SessionSpec, default_shell: &str, id: &str) -> String {
        let prog = spec.command.first().map(String::as_str).unwrap_or(default_shell);
        let base = std::path::Path::new(prog)
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("session");
        let hex: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        for len in 6..=hex.len() {
            let candidate = format!("{base}-{}", &hex[hex.len() - len..]);
            if self.resolve(&candidate).is_none() {
                return candidate;
            }
        }
        // Exhausted the whole hex tail (impossible in practice): fall back to the id.
        format!("{base}-{id}")
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo nextest run -p cairn-daemon create_without_name_infers_basename_and_hex_suffix`
Expected: PASS.

- [ ] **Step 5: Run the registry + unary suites to check for regressions**

Run: `cargo nextest run -p cairn-daemon registry && cargo nextest run -p cairn-daemon --test daemon_unary`
Expected: all pass (explicit-name behavior is unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon/src/registry.rs
git commit -m "feat(cairn-daemon): infer {basename}-{hex-tail} name when none is given

A session created without a name (e.g. \`cairn run bash\`) gets a stable,
readable, unique handle for \`list\`/attach instead of being unnamed. The
suffix is the UUIDv7 random tail (not the shared timestamp prefix), always
appended so no instance-counting is needed."
```

---

### Task 4: Lock "explicit env overrides inherited" with a behavior test

**Files:**
- Test: `crates/cairn-daemon/tests/daemon_streaming.rs` (add one test)

`spawn.rs:options_from` builds the child env with `tokio::process::Command`, where `cmd.env(k, v)` overrides the inherited value — so explicit-wins is already correct. This test pins that decision against a future regression (e.g. someone applying inherited env *after* explicit). No production code changes.

- [ ] **Step 1: Write the test**

Add to `crates/cairn-daemon/tests/daemon_streaming.rs` (the `SessionSpec` import is already present at the top). Place it after the `send` tests:

```rust
// ── env precedence ──────────────────────────────────────────────────────────

#[tokio::test]
async fn explicit_env_overrides_inherited() {
    // `CARGO_PKG_NAME` is set in the test process env by cargo, so with
    // env_inherit the child would inherit it ("cairn-daemon"). The explicit
    // spec.env value must win.
    let daemon = test_daemon();
    let spec = SessionSpec {
        name: Some("envprec".to_string()),
        command: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf '%s' \"$CARGO_PKG_NAME\"".to_string(),
        ],
        env: vec![("CARGO_PKG_NAME".to_string(), "overridden".to_string())],
        env_inherit: true,
        workdir: None,
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 100,
    };
    let info = daemon.registry.create(spec, &daemon.cfg.default_shell).await.unwrap();
    let entry = daemon.registry.resolve(&info.id).unwrap();
    let cid = daemon.registry.mint_client_id();
    let mut sub = entry.handle().subscribe(cid).await.unwrap();

    // The child's output may land in the snapshot (it runs immediately) or the
    // live stream — check both, and confirm the inherited value never appears.
    let mut buf = sub.snapshot.to_vec();
    let saw = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if buf.windows(10).any(|w| w == b"overridden") {
                return true;
            }
            match sub.stream.recv().await {
                Ok(b) => buf.extend_from_slice(&b),
                Err(_) => return buf.windows(10).any(|w| w == b"overridden"),
            }
        }
    })
    .await
    .unwrap_or(false);

    assert!(saw, "explicit spec.env must override the inherited CARGO_PKG_NAME");
    assert!(
        !buf.windows(12).any(|w| w == b"cairn-daemon"),
        "inherited value must not leak through when explicitly overridden"
    );
}
```

- [ ] **Step 2: Run the test to verify it passes (behavior is already correct)**

Run: `cargo nextest run -p cairn-daemon explicit_env_overrides_inherited`
Expected: PASS — confirms explicit-wins.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-daemon/tests/daemon_streaming.rs
git commit -m "test(cairn-daemon): lock explicit-env-overrides-inherited precedence

We committed to 'explicit wins over inherited' for session env; the current
std::process::Command-based merge already does this. Pin it so a future
change to the merge order is caught."
```

---

## Self-Review

**Spec coverage** (spec "Daemon-side changes" section):
1. Distinct kicked/lagged events — Task 1 (consts) + Task 2 (emit + test). ✓
2. Explicit env overrides inherited — already correct in `spawn.rs`; Task 4 locks it; the misleading `cli.rs` doc-comment fix lives in the client plan's cli.rs task. ✓
3. Default-name inference `{basename}-{last-6-hex}` — Task 3. ✓

**Placeholder scan:** none — every step shows full code/commands and expected output.

**Type consistency:** `cairn_protocol::error_codes::{CLIENT_KICKED, CLIENT_LAGGED}` used identically in Task 1 (def), Task 2 (handler + test). `inferred_unique_name` defined and called in Task 3. `ServerEvent::Error(WireError { code, message })` matches the existing `once_error` shape at `handlers/attach.rs:92-95`.

**Note for the executor:** these tasks land on `main` work already in progress on branch `feat/cli-client-interactive-attach` (the spec commit is there). Run them on that branch so the client plan builds on top.

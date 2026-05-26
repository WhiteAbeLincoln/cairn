# Daemon Core Implementation Plan (cairn-daemon: registry, serve, unary ops)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the `cairn-daemon` binary serving the `meta` interface and the **unary** `sessions` operations (list-all/inspect/create/rename/restart/kill/kick) over a Unix Domain Socket, backed by an in-process session registry of `cairn-pty` `GhosttyPty` workers. (The streaming ops — attach/logs/send/wait — are Plan 3.)

**Architecture:** Approach A from the design spec — a `Daemon { registry, cfg }` (Clone) whose thin `Handler` impls delegate into `handlers::*`. The registry maps UUIDv7 ids to `Arc<SessionEntry>` (handle behind `RwLock<Running>`, name behind `Mutex`, attached-set behind `Mutex`); never holds a lock across `.await`. A custom `Accept` impl reads `SO_PEERCRED` into a `ConnCtx` so `whoami` reports the real peer uid. `kill` delivers a signal and, given a grace, arms a detached daemon-side escalation task.

**Tech Stack:** Rust 2024, tokio multi-thread runtime, `wrpc-transport` (net), `cairn-protocol` (generated `Handler` traits + client free-fns), `cairn-pty` (`PtySession`/`GhosttyPty`), `clap`, `tracing`/`tracing-subscriber`, `uuid` (v7), `futures`, `tokio-util` (`CancellationToken`).

**Spec:** `docs/superpowers/specs/2026-05-26-daemon-binary-design.md`. Depends on the merged Plan 1 foundation (`PtySession::{signal,inject,wait,try_exit_status}`, `ExitStatus` struct, `sessions.kill` `grace-ms`).

---

## File structure

```
crates/cairn-daemon/
  Cargo.toml          # bin "cairn-daemon" + lib
  src/
    main.rs           # arg parse -> tracing init -> build Daemon -> run serve(); SIGTERM/SIGINT
    lib.rs            # pub mod {config, daemon, registry, serve, spawn, signal, error, handlers}
    config.rs         # DaemonConfig (flags + CAIRN_* env + XDG defaults)
    error.rs          # PtyError + DaemonError -> types::Error { code, message }
    signal.rs         # protocol Signal -> libc i32 (name resolution, numbered passthrough)
    spawn.rs          # SessionSpec -> cairn_pty::SpawnOptions
    registry.rs       # SessionRegistry, SessionEntry, Running, resolution, ClientId minting
    serve.rs          # ConnCtx + PeerCredListener, bind_with_cleanup, accept loop, pump, drain, serve()
    daemon.rs         # Daemon struct; impl sessions::Handler + meta::Handler (delegate to handlers::*)
    handlers/
      mod.rs
      meta.rs         # version, whoami, authenticate
      sessions.rs     # list_all, inspect, create, rename, restart, kill, kick
  tests/
    common/mod.rs     # test harness: spawn the real Daemon on a tempdir socket; client helpers
    daemon_meta.rs    # version / whoami / authenticate
    daemon_unary.rs   # create/list/inspect/rename/restart/kill/kick round-trips
```

Streaming handlers (`handlers/attach.rs`, `logs.rs`, `send.rs`, `wait.rs`) and their wiring land in Plan 3.

---

## Task 1: Crate scaffold

**Files:**
- Create: `crates/cairn-daemon/Cargo.toml`
- Create: `crates/cairn-daemon/src/lib.rs`
- Create: `crates/cairn-daemon/src/main.rs`

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "cairn-daemon"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
authors.workspace = true
description = "The cairn session-manager daemon"

[[bin]]
name = "cairn-daemon"
path = "src/main.rs"

[lib]
name = "cairn_daemon"
path = "src/lib.rs"

[dependencies]
cairn-pty = { path = "../cairn-pty" }
cairn-protocol = { path = "../cairn-protocol" }
wrpc-transport.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "net", "signal", "macros", "sync", "time"] }
tokio-util = { version = "0.7", features = ["rt"] }
futures.workspace = true
anyhow.workspace = true
clap.workspace = true
tracing.workspace = true
tracing-subscriber = { version = "0.3", features = ["env-filter", "fmt"] }
uuid = { version = "1", features = ["v7"] }
bytes.workspace = true

[dev-dependencies]
tempfile = { version = "3", default-features = false }
tokio = { workspace = true, features = ["rt-multi-thread", "net", "macros", "time", "fs"] }
```

- [ ] **Step 2: Create `src/lib.rs`**

```rust
//! The cairn session-manager daemon: serves the `cairn:daemon@0.1.0` wRPC
//! surface over a Unix domain socket against an in-process session registry.

pub mod config;
pub mod daemon;
pub mod error;
pub mod handlers;
pub mod registry;
pub mod serve;
pub mod signal;
pub mod spawn;
```

- [ ] **Step 3: Create a minimal `src/main.rs` (fleshed out in Task 9)**

```rust
fn main() -> anyhow::Result<()> {
    // Real entrypoint wired in Task 9 (arg parse -> tracing -> serve).
    Ok(())
}
```

- [ ] **Step 4: Create placeholder module files so `lib.rs` compiles**

Create empty `src/config.rs`, `src/error.rs`, `src/signal.rs`, `src/spawn.rs`, `src/registry.rs`, `src/serve.rs`, `src/daemon.rs`, and `src/handlers/mod.rs` (with `pub mod meta; pub mod sessions;` — and create empty `src/handlers/meta.rs`, `src/handlers/sessions.rs`). Each empty for now (filled by later tasks). `mod.rs` content:

```rust
pub mod meta;
pub mod sessions;
```

- [ ] **Step 5: Verify it builds**

Run: `cargo build -p cairn-daemon`
Expected: compiles (empty modules).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon Cargo.lock
git commit -m "feat(cairn-daemon): scaffold crate (lib + bin, empty modules)"
```

---

## Task 2: `DaemonConfig`

**Files:**
- Modify: `crates/cairn-daemon/src/config.rs`
- Test: `crates/cairn-daemon/src/config.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_octal_accepts_0o_and_bare() {
        assert_eq!(parse_octal_mode("0o750").unwrap(), 0o750);
        assert_eq!(parse_octal_mode("750").unwrap(), 0o750);
        assert!(parse_octal_mode("nonsense").is_err());
    }

    #[test]
    fn defaults_are_conservative() {
        let c = DaemonConfig::default();
        assert_eq!(c.dir_mode, 0o700);
        assert_eq!(c.socket_mode, 0o600);
        assert_eq!(c.shutdown_grace, std::time::Duration::from_secs(5));
        assert!(c.socket_path.ends_with("cairn/cairn.sock"));
    }
}
```

- [ ] **Step 2: Run — verify fail (compile error: items missing)**

Run: `cargo test -p cairn-daemon config::`
Expected: FAIL.

- [ ] **Step 3: Implement `config.rs`**

```rust
//! Daemon configuration: defaults < CAIRN_* env < CLI flags.

use std::path::PathBuf;
use std::time::Duration;

/// Resolved daemon configuration.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
    pub dir_mode: u32,
    pub socket_mode: u32,
    pub shutdown_grace: Duration,
    pub default_shell: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            dir_mode: 0o700,
            socket_mode: 0o600,
            shutdown_grace: Duration::from_secs(5),
            default_shell: default_shell(),
        }
    }
}

/// `$XDG_RUNTIME_DIR/cairn/cairn.sock` on Linux, `$TMPDIR/cairn/cairn.sock`
/// otherwise. The `cairn/` parent is daemon-owned so `dir_mode` governs it.
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn").join("cairn.sock")
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Parse an octal file mode, accepting `0o750` or bare `750`.
pub fn parse_octal_mode(s: &str) -> Result<u32, std::num::ParseIntError> {
    let digits = s.strip_prefix("0o").unwrap_or(s);
    u32::from_str_radix(digits, 8)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-daemon config::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-daemon/src/config.rs
git commit -m "feat(cairn-daemon): add DaemonConfig with XDG defaults and octal mode parsing"
```

---

## Task 3: Error mapping (`error.rs`)

**Files:**
- Modify: `crates/cairn-daemon/src/error.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cairn_pty::{ClientId, PtyError};

    #[test]
    fn pty_errors_map_to_stable_codes() {
        assert_eq!(to_wire(PtyError::Closed).code, "session.closed");
        assert_eq!(
            to_wire(PtyError::NotLeader { requester: ClientId::from_u64(0), current: None }).code,
            "resize.not_leader"
        );
    }

    #[test]
    fn daemon_errors_map_to_stable_codes() {
        assert_eq!(DaemonError::NotFound.to_wire().code, "session.not_found");
        assert_eq!(DaemonError::NameInUse.to_wire().code, "session.name_in_use");
        assert_eq!(DaemonError::Running.to_wire().code, "session.running");
        assert_eq!(DaemonError::InvalidSignal.to_wire().code, "signal.invalid");
    }
}
```

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon error::`
Expected: FAIL.

- [ ] **Step 3: Implement `error.rs`**

```rust
//! Mapping from internal errors to the wire `types::error` envelope, with
//! machine-stable `code` strings the CLI can branch on.

use cairn_pty::PtyError;
use cairn_protocol::cairn::daemon::types::Error as WireError;

/// Daemon-level (non-PtyError) failures.
#[derive(Debug, Clone, Copy)]
pub enum DaemonError {
    NotFound,
    NameInUse,
    Running,
    SpawnFailed,
    InvalidSignal,
}

impl DaemonError {
    pub fn to_wire(self) -> WireError {
        let (code, message) = match self {
            DaemonError::NotFound => ("session.not_found", "no such session"),
            DaemonError::NameInUse => ("session.name_in_use", "a live session already has that name"),
            DaemonError::Running => ("session.running", "session is still running (use --force)"),
            DaemonError::SpawnFailed => ("session.spawn_failed", "failed to spawn the session"),
            DaemonError::InvalidSignal => ("signal.invalid", "unknown or out-of-range signal"),
        };
        WireError { code: code.to_string(), message: message.to_string() }
    }
}

/// Map a `PtyError` to the wire envelope.
pub fn to_wire(err: PtyError) -> WireError {
    let (code, message) = match &err {
        PtyError::Closed => ("session.closed", "session has exited".to_string()),
        PtyError::NotLeader { .. } => ("resize.not_leader", err.to_string()),
        PtyError::Io { .. } => ("pty.io", err.to_string()),
        PtyError::Backend { .. } => ("pty.backend", err.to_string()),
    };
    WireError { code: code.to_string(), message }
}
```

(Note: `message` for the `Closed` arm is built as a `String`; the others reuse `err.to_string()`. Adjust the match so both arms produce `String` — wrap the literal in `.to_string()` as shown.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-daemon error::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-daemon/src/error.rs
git commit -m "feat(cairn-daemon): map PtyError/DaemonError to stable wire error codes"
```

---

## Task 4: Signal translation (`signal.rs`)

**Files:**
- Modify: `crates/cairn-daemon/src/signal.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::{Signal, SignalName};

    #[test]
    fn named_term_resolves_to_libc_sigterm() {
        assert_eq!(to_libc(&Signal::Named(SignalName::Term)).unwrap(), libc::SIGTERM);
        assert_eq!(to_libc(&Signal::Named(SignalName::Kill)).unwrap(), libc::SIGKILL);
        assert_eq!(to_libc(&Signal::Named(SignalName::Int)).unwrap(), libc::SIGINT);
    }

    #[test]
    fn numbered_passes_through() {
        assert_eq!(to_libc(&Signal::Numbered(9)).unwrap(), 9);
    }

    #[test]
    fn numbered_zero_is_invalid() {
        assert!(to_libc(&Signal::Numbered(0)).is_err());
    }
}
```

(Add `libc = "0.2"` to `cairn-daemon`'s `[dependencies]` in this task — it's needed for the `libc::SIG*` constants.)

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon signal::`
Expected: FAIL.

- [ ] **Step 3: Implement `signal.rs`**

Map every `SignalName` to its local libc constant (resolving the Linux/BSD numbering divergence at the daemon's own libc). The numbered variant passes through, rejecting 0.

```rust
//! Translate the protocol `signal` to a local libc signal number. Named
//! signals resolve against THIS host's libc (so SIGUSR1 etc. are correct on
//! both Linux and BSD); the numbered variant is an as-is escape hatch.

use cairn_protocol::cairn::daemon::types::{Signal, SignalName};

use crate::error::DaemonError;

pub fn to_libc(sig: &Signal) -> Result<i32, DaemonError> {
    match sig {
        Signal::Numbered(0) => Err(DaemonError::InvalidSignal),
        Signal::Numbered(n) => Ok(i32::from(*n)),
        Signal::Named(name) => Ok(named_to_libc(*name)),
    }
}

fn named_to_libc(name: SignalName) -> i32 {
    match name {
        SignalName::Hup => libc::SIGHUP,
        SignalName::Int => libc::SIGINT,
        SignalName::Quit => libc::SIGQUIT,
        SignalName::Ill => libc::SIGILL,
        SignalName::Trap => libc::SIGTRAP,
        SignalName::Abrt => libc::SIGABRT,
        SignalName::Bus => libc::SIGBUS,
        SignalName::Fpe => libc::SIGFPE,
        SignalName::Kill => libc::SIGKILL,
        SignalName::Usr1 => libc::SIGUSR1,
        SignalName::Segv => libc::SIGSEGV,
        SignalName::Usr2 => libc::SIGUSR2,
        SignalName::Pipe => libc::SIGPIPE,
        SignalName::Alrm => libc::SIGALRM,
        SignalName::Term => libc::SIGTERM,
        SignalName::Chld => libc::SIGCHLD,
        SignalName::Cont => libc::SIGCONT,
        SignalName::Stop => libc::SIGSTOP,
        SignalName::Tstp => libc::SIGTSTP,
        SignalName::Ttin => libc::SIGTTIN,
        SignalName::Ttou => libc::SIGTTOU,
        SignalName::Urg => libc::SIGURG,
        SignalName::Xcpu => libc::SIGXCPU,
        SignalName::Xfsz => libc::SIGXFSZ,
        SignalName::Vtalrm => libc::SIGVTALRM,
        SignalName::Prof => libc::SIGPROF,
        SignalName::Winch => libc::SIGWINCH,
        SignalName::Io => libc::SIGIO,
        SignalName::Sys => libc::SIGSYS,
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-daemon signal::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-daemon/src/signal.rs crates/cairn-daemon/Cargo.toml
git commit -m "feat(cairn-daemon): translate protocol Signal to local libc signal number"
```

---

## Task 5: Spec → SpawnOptions (`spawn.rs`)

**Files:**
- Modify: `crates/cairn-daemon/src/spawn.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::SessionSpec;

    fn base_spec() -> SessionSpec {
        SessionSpec {
            name: None, command: vec![], env: vec![], env_inherit: true,
            workdir: None, tty: true, stdin: true, idle_timeout_secs: None,
            scrollback_lines: 500,
        }
    }

    #[test]
    fn empty_command_uses_default_shell() {
        let opts = options_from(base_spec(), "/bin/zsh");
        let std = opts.command.as_std();
        assert_eq!(std.get_program(), std::ffi::OsStr::new("/bin/zsh"));
        assert_eq!(opts.scrollback_lines, 500);
    }

    #[test]
    fn explicit_command_and_env_are_applied() {
        let mut spec = base_spec();
        spec.command = vec!["echo".into(), "hi".into()];
        spec.env = vec![("FOO".into(), "bar".into())];
        spec.workdir = Some("/tmp".into());
        let opts = options_from(spec, "/bin/sh");
        let std = opts.command.as_std();
        assert_eq!(std.get_program(), std::ffi::OsStr::new("echo"));
        let args: Vec<_> = std.get_args().collect();
        assert_eq!(args, vec![std::ffi::OsStr::new("hi")]);
        assert_eq!(std.get_current_dir(), Some(std::path::Path::new("/tmp")));
    }
}
```

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon spawn::`
Expected: FAIL.

- [ ] **Step 3: Implement `spawn.rs`**

```rust
//! Build a `cairn_pty::SpawnOptions` from a wire `session-spec`.

use cairn_pty::SpawnOptions;
use cairn_protocol::cairn::daemon::types::SessionSpec;

/// Translate a `session-spec` into spawn options. An empty `command` falls
/// back to `default_shell`. `env-inherit=false` clears the inherited env.
pub fn options_from(spec: SessionSpec, default_shell: &str) -> SpawnOptions {
    let mut argv = spec.command.into_iter();
    let program = argv.next().unwrap_or_else(|| default_shell.to_string());

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(argv);
    if !spec.env_inherit {
        cmd.env_clear();
    }
    for (k, v) in spec.env {
        cmd.env(k, v);
    }
    if let Some(dir) = spec.workdir {
        cmd.current_dir(dir);
    }

    let opts = SpawnOptions::new(cmd).with_scrollback_lines(spec.scrollback_lines as usize);
    opts
}
```

(Note: `tty`/`stdin`/`idle_timeout_secs`/`name` are handled at the registry/daemon level, not by `SpawnOptions`. Initial size is the `SpawnOptions` default 80×24; a client's first attach resizes it in Plan 3.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p cairn-daemon spawn::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-daemon/src/spawn.rs
git commit -m "feat(cairn-daemon): build SpawnOptions from a session-spec"
```

---

## Task 6: Session registry (`registry.rs`)

**Files:**
- Modify: `crates/cairn-daemon/src/registry.rs`
- Test: inline `#[cfg(test)]` (real `GhosttyPty` spawning `/bin/sleep`/`/bin/true`)

- [ ] **Step 1: Write failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::SessionSpec;

    fn spec(name: Option<&str>) -> SessionSpec {
        SessionSpec {
            name: name.map(str::to_string),
            command: vec!["sleep".into(), "100".into()],
            env: vec![], env_inherit: true, workdir: None,
            tty: true, stdin: true, idle_timeout_secs: None, scrollback_lines: 100,
        }
    }

    #[tokio::test]
    async fn create_then_resolve_by_name_and_id() {
        let reg = SessionRegistry::new();
        let info = reg.create(spec(Some("dev")), "/bin/sh").expect("create");
        let by_name = reg.resolve("dev").expect("by name");
        let by_id = reg.resolve(&info.id).expect("by id");
        assert_eq!(by_name.id, info.id);
        assert_eq!(by_id.id, info.id);
        assert!(reg.resolve("nope").is_none());
    }

    #[tokio::test]
    async fn duplicate_live_name_is_rejected() {
        let reg = SessionRegistry::new();
        reg.create(spec(Some("dev")), "/bin/sh").expect("first");
        let err = reg.create(spec(Some("dev")), "/bin/sh").expect_err("dup");
        assert!(matches!(err, crate::error::DaemonError::NameInUse));
    }

    #[tokio::test]
    async fn mint_client_id_is_monotonic() {
        let reg = SessionRegistry::new();
        let a = reg.mint_client_id();
        let b = reg.mint_client_id();
        assert_ne!(a, b);
    }
}
```

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon registry::`
Expected: FAIL.

- [ ] **Step 3: Implement `registry.rs`**

```rust
//! In-process session registry. Holds `Arc<SessionEntry>` keyed by UUIDv7 id;
//! resolution is by exact live name then exact id. Locking discipline: never
//! hold an entry lock across `.await`, never hold two entry locks at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use cairn_pty::{ClientId, GhosttyPty, PtySession};
use cairn_protocol::cairn::daemon::types::{ExitStatus as WireExit, SessionInfo, SessionSpec};
use tokio::sync::oneshot;

use crate::error::DaemonError;
use crate::spawn::options_from;

/// The swappable runtime state of a session (replaced on restart).
struct Running {
    handle: Arc<dyn PtySession>,
    pid: Option<u32>,
}

/// One client's attach registration; `kick` fires `kick` to evict it.
pub struct AttachHandle {
    pub kick: oneshot::Sender<()>,
}

/// A registry entry: stable identity + swappable handle + daemon-side metadata.
pub struct SessionEntry {
    pub id: String,
    pub created_at_unix_ms: u64,
    pub spec: SessionSpec,
    name: Mutex<Option<String>>,
    running: RwLock<Running>,
    pub attached: Mutex<HashMap<ClientId, AttachHandle>>,
}

impl SessionEntry {
    /// Clone the current session handle out of the lock (held only for the clone).
    pub fn handle(&self) -> Arc<dyn PtySession> {
        self.running.read().expect("running lock").handle.clone()
    }

    pub fn pid(&self) -> Option<u32> {
        self.running.read().expect("running lock").pid
    }

    pub fn name(&self) -> Option<String> {
        self.name.lock().expect("name lock").clone()
    }

    fn set_name(&self, new: String) {
        *self.name.lock().expect("name lock") = Some(new);
    }

    fn swap_running(&self, handle: Arc<dyn PtySession>, pid: Option<u32>) {
        *self.running.write().expect("running lock") = Running { handle, pid };
    }
}

pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, Arc<SessionEntry>>>,
    next_client_id: AtomicU64,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            next_client_id: AtomicU64::new(0),
        }
    }

    pub fn mint_client_id(&self) -> ClientId {
        ClientId::from_u64(self.next_client_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Resolve by exact live name first, then by exact id.
    pub fn resolve(&self, key: &str) -> Option<Arc<SessionEntry>> {
        let map = self.sessions.read().expect("sessions lock");
        if let Some(entry) = map.values().find(|e| e.name().as_deref() == Some(key)) {
            return Some(entry.clone());
        }
        map.get(key).cloned()
    }

    pub fn list(&self) -> Vec<Arc<SessionEntry>> {
        self.sessions.read().expect("sessions lock").values().cloned().collect()
    }

    /// Spawn a new session. Rejects a name already used by a live session.
    pub fn create(&self, spec: SessionSpec, default_shell: &str) -> Result<SessionInfo, DaemonError> {
        if let Some(name) = &spec.name {
            if self.resolve(name).is_some() {
                return Err(DaemonError::NameInUse);
            }
        }
        let opts = options_from(spec.clone(), default_shell);
        let handle = GhosttyPty::spawn(opts).map_err(|_| DaemonError::SpawnFailed)?;
        let pid = None; // pid surfaced via inspect later if cairn-pty exposes it; None for v0
        let id = uuid::Uuid::now_v7().to_string();
        let entry = Arc::new(SessionEntry {
            id: id.clone(),
            created_at_unix_ms: now_unix_ms(),
            spec: spec.clone(),
            name: Mutex::new(spec.name.clone()),
            running: RwLock::new(Running { handle: Arc::new(handle), pid }),
            attached: Mutex::new(HashMap::new()),
        });
        let info = futures::executor::block_on(session_info(&entry));
        self.sessions.write().expect("sessions lock").insert(id, entry);
        Ok(info)
    }

    pub fn rename(&self, key: &str, new_name: String) -> Result<(), DaemonError> {
        let entry = self.resolve(key).ok_or(DaemonError::NotFound)?;
        if let Some(existing) = self.resolve(&new_name) {
            if existing.id != entry.id {
                return Err(DaemonError::NameInUse);
            }
        }
        entry.set_name(new_name);
        Ok(())
    }

    /// Re-spawn under the same id/name; rejects a still-running session unless `force`.
    pub fn restart(&self, key: &str, force: bool, default_shell: &str) -> Result<(), DaemonError> {
        let entry = self.resolve(key).ok_or(DaemonError::NotFound)?;
        if entry.handle().try_exit_status().is_none() && !force {
            return Err(DaemonError::Running);
        }
        let opts = options_from(entry.spec.clone(), default_shell);
        let handle = GhosttyPty::spawn(opts).map_err(|_| DaemonError::SpawnFailed)?;
        entry.swap_running(Arc::new(handle), None); // old handle dropped -> Drop kills old child
        Ok(())
    }
}

pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build the wire `SessionInfo` for an entry (size via `size()`, exit via the
/// non-blocking `try_exit_status()`).
pub async fn session_info(entry: &SessionEntry) -> SessionInfo {
    let handle = entry.handle();
    let (cols, rows) = handle.size().await.map(|s| (s.cols, s.rows)).unwrap_or((0, 0));
    let exit = handle.try_exit_status().map(|st| WireExit {
        code: st.code(),
        signal: st.signal().map(|s| s as u8),
        unix_ms: st.unix_ms(),
    });
    let attached_clients = entry
        .attached
        .lock()
        .expect("attached lock")
        .keys()
        .map(|c| c.to_string())
        .collect();
    SessionInfo {
        id: entry.id.clone(),
        name: entry.name(),
        pid: entry.pid(),
        cols,
        rows,
        attached_clients,
        created_at_unix_ms: entry.created_at_unix_ms,
        exit,
        spec: entry.spec.clone(),
    }
}
```

(Note on `create` using `futures::executor::block_on(session_info(...))`: `size()` is async. `create` is called from an async handler, so prefer making `create` async and `.await`ing `session_info`. **Adjust `create` to `async fn` and `.await` `session_info`** — the `block_on` shown is a placeholder; the handler in Task 8 calls `create` from async context, so make `create` async to avoid nested runtimes. The test wraps in `#[tokio::test]` and `.await`s.)

- [ ] **Step 4: Refine `create` to be async**

Change `pub fn create(...)` to `pub async fn create(...)` and replace the `block_on` line with `let info = session_info(&entry).await;`. Update the Step-1 tests to `reg.create(...).await`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p cairn-daemon registry::`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon/src/registry.rs
git commit -m "feat(cairn-daemon): session registry with create/resolve/rename/restart"
```

---

## Task 7: `ConnCtx` + `PeerCredListener` (`serve.rs` part 1)

**Files:**
- Modify: `crates/cairn-daemon/src/serve.rs`
- Test: inline `#[cfg(test)]`

- [ ] **Step 1: Write failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wrpc_transport::frame::Accept as _;

    #[tokio::test]
    async fn accept_yields_peer_uid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let pl = PeerCredListener(listener);

        let connect = tokio::spawn(async move {
            let _c = tokio::net::UnixStream::connect(&path).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let (ctx, _tx, _rx) = (&pl).accept().await.unwrap();
        assert_eq!(ctx.peer.unwrap().uid(), nix_geteuid());
        connect.await.unwrap();
    }

    fn nix_geteuid() -> u32 {
        // SAFETY: geteuid is always safe.
        unsafe { libc::geteuid() }
    }
}
```

- [ ] **Step 2: Run — verify fail**

Run: `cargo test -p cairn-daemon serve::tests::accept_yields_peer_uid`
Expected: FAIL.

- [ ] **Step 3: Implement `ConnCtx` + `PeerCredListener`**

```rust
//! UDS listener + wRPC server wiring.

use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf, UCred};
use wrpc_transport::frame::Accept;

/// Per-connection context handed to every `Handler` method. On UDS the peer
/// credentials identify the caller (for `whoami` and audit). The future WT
/// transport will fill the same shape with the authenticated token identity.
#[derive(Clone, Copy, Debug)]
pub struct ConnCtx {
    pub peer: Option<UCred>,
}

/// A `UnixListener` whose `accept` captures `SO_PEERCRED` into `ConnCtx`
/// before splitting the stream.
pub struct PeerCredListener(pub tokio::net::UnixListener);

impl Accept for &PeerCredListener {
    type Context = ConnCtx;
    type Outgoing = OwnedWriteHalf;
    type Incoming = OwnedReadHalf;

    async fn accept(
        &self,
    ) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer = stream.peer_cred().ok();
        let (rx, tx) = stream.into_split();
        Ok((ConnCtx { peer }, tx, rx))
    }
}
```

(Confirm the exact import path of the `Accept` trait and `UCred` against the merged code — `wrpc_transport::frame::Accept` per the 0.28 source, `tokio::net::unix::UCred`. If `Accept` is re-exported elsewhere, adjust the `use`.)

- [ ] **Step 4: Run test**

Run: `cargo test -p cairn-daemon serve::tests::accept_yields_peer_uid`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-daemon/src/serve.rs
git commit -m "feat(cairn-daemon): ConnCtx + PeerCredListener capturing SO_PEERCRED"
```

---

## Task 8: `Daemon` + `meta`/unary handlers + `serve()`

This is the integration task: wire `Daemon`, the `Handler` impls, socket hygiene, the accept loop + invocation pump, graceful drain, and a test harness, then drive everything through real wRPC client calls.

**Files:**
- Modify: `crates/cairn-daemon/src/daemon.rs`, `src/serve.rs`, `src/handlers/meta.rs`, `src/handlers/sessions.rs`, `src/main.rs`
- Test: `crates/cairn-daemon/tests/common/mod.rs`, `tests/daemon_meta.rs`, `tests/daemon_unary.rs`

- [ ] **Step 1: `Daemon` struct (`daemon.rs`)**

```rust
use std::sync::Arc;

use crate::config::DaemonConfig;
use crate::registry::SessionRegistry;

#[derive(Clone)]
pub struct Daemon {
    pub registry: Arc<SessionRegistry>,
    pub cfg: Arc<DaemonConfig>,
}

impl Daemon {
    pub fn new(cfg: DaemonConfig) -> Self {
        Self { registry: Arc::new(SessionRegistry::new()), cfg: Arc::new(cfg) }
    }
}
```

- [ ] **Step 2: `meta` handlers (`handlers/meta.rs`)**

```rust
use cairn_protocol::cairn::daemon::types::Error as WireError;
use cairn_protocol::exports::cairn::daemon::meta::VersionInfo;

use crate::daemon::Daemon;
use crate::serve::ConnCtx;

pub fn version() -> VersionInfo {
    VersionInfo {
        daemon: concat!("cairn-daemon/", env!("CARGO_PKG_VERSION")).to_string(),
        protocol: "cairn:daemon@0.1.0".to_string(),
    }
}

/// UDS is pre-authenticated by the kernel; first-message auth is a WT concern.
pub fn authenticate(_token: String) -> Result<(), WireError> {
    Ok(())
}

/// The peer uid (resolved to a username when possible), from `SO_PEERCRED`.
pub fn whoami(ctx: &ConnCtx) -> Result<String, WireError> {
    let uid = ctx.peer.map(|c| c.uid());
    Ok(match uid {
        Some(uid) => username_for(uid).unwrap_or_else(|| uid.to_string()),
        None => "unknown".to_string(),
    })
}

fn username_for(uid: u32) -> Option<String> {
    // getpwuid_r via libc; return None on any failure (fall back to numeric).
    // Keep this small and dependency-free.
    None // v0: report numeric uid; richer lookup is a later refinement
}
```

(For v0, `username_for` returns `None` so `whoami` reports the numeric uid — no extra dependency. A `getpwuid_r` lookup can be added later without changing the interface.)

- [ ] **Step 3: unary `sessions` handlers (`handlers/sessions.rs`)**

Each is thin: act on the registry, map errors. `kill` additionally arms daemon-side escalation.

```rust
use std::time::Duration;

use cairn_protocol::cairn::daemon::types::{Error as WireError, SessionInfo, SessionSpec, Signal};

use crate::daemon::Daemon;
use crate::error::{to_wire, DaemonError};
use crate::registry::session_info;
use crate::signal::to_libc;

pub async fn list_all(d: &Daemon) -> Vec<SessionInfo> {
    let entries = d.registry.list();
    // Fan out size() concurrently — one round-trip latency regardless of count.
    futures::future::join_all(entries.iter().map(|e| session_info(e))).await
}

pub async fn inspect(d: &Daemon, id: String) -> Result<SessionInfo, WireError> {
    let entry = d.registry.resolve(&id).ok_or_else(|| DaemonError::NotFound.to_wire())?;
    Ok(session_info(&entry).await)
}

pub async fn create(d: &Daemon, spec: SessionSpec) -> Result<SessionInfo, WireError> {
    d.registry
        .create(spec, &d.cfg.default_shell)
        .await
        .map_err(DaemonError::to_wire)
}

pub async fn rename(d: &Daemon, id: String, new_name: String) -> Result<(), WireError> {
    d.registry.rename(&id, new_name).map_err(DaemonError::to_wire)
}

pub async fn restart(d: &Daemon, id: String, force: bool) -> Result<(), WireError> {
    d.registry.restart(&id, force, &d.cfg.default_shell).map_err(DaemonError::to_wire)
}

pub async fn kick(d: &Daemon, id: String, client: Option<String>) -> Result<(), WireError> {
    let entry = d.registry.resolve(&id).ok_or_else(|| DaemonError::NotFound.to_wire())?;
    let mut attached = entry.attached.lock().expect("attached lock");
    // Fire the kick signal(s); the attach bridge (Plan 3) exits and self-removes.
    // In the core slice `attached` is always empty (no bridge populates it yet),
    // so this is a no-op success — which is correct.
    match client {
        Some(cid) => {
            // ClientId is Copy; find the one whose id renders to `cid`.
            let key = attached.keys().find(|c| c.to_string() == cid).copied();
            if let Some(k) = key {
                if let Some(h) = attached.remove(&k) {
                    let _ = h.kick.send(());
                }
            }
        }
        None => {
            for (_id, h) in attached.drain() {
                let _ = h.kick.send(());
            }
        }
    }
    Ok(())
}

pub async fn kill(d: &Daemon, id: String, sig: Signal, grace_ms: Option<u32>) -> Result<(), WireError> {
    let entry = d.registry.resolve(&id).ok_or_else(|| DaemonError::NotFound.to_wire())?;
    let signum = to_libc(&sig).map_err(DaemonError::to_wire)?;
    let handle = entry.handle();
    handle.signal(signum).await.map_err(to_wire)?;

    if let Some(g) = grace_ms {
        // Daemon-owned escalation: independent of client liveness.
        let handle = entry.handle();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(g as u64)).await;
            if handle.try_exit_status().is_none() {
                let _ = handle.signal(libc::SIGKILL).await;
            }
        });
    }
    Ok(())
}
```

(Note: `kick` shows `remove_entry_matching` as pseudo — implement client matching by comparing `ClientId::to_string()` to `cid`; iterate the map, find the matching key, remove it. The streaming attach bridge that populates `attached` lands in Plan 3, so until then `attached` is empty and `kick` is a no-op success — which is correct.)

- [ ] **Step 4: `Handler` impls on `Daemon` (`daemon.rs`)**

Implement `sessions::Handler<ConnCtx>` and `meta::Handler<ConnCtx>` for `Daemon`, each method a one-liner into `handlers::*`. The streaming methods (`wait`/`logs`/`attach`/`send`) return `unimplemented!("Plan 3")` for now — they compile but aren't served until Plan 3. Example shape:

```rust
impl cairn_protocol::exports::cairn::daemon::sessions::Handler<ConnCtx> for Daemon {
    async fn list_all(&self, _ctx: ConnCtx) -> anyhow::Result<Vec<SessionInfo>> {
        Ok(handlers::sessions::list_all(self).await)
    }
    async fn kill(&self, _ctx: ConnCtx, id: String, sig: Signal, grace_ms: Option<u32>)
        -> anyhow::Result<Result<(), WireError>> {
        Ok(handlers::sessions::kill(self, id, sig, grace_ms).await)
    }
    // inspect/create/rename/restart/kick: same delegation shape, wrapping the
    // handler's Result in Ok(...).
    // wait/logs/attach/send: unimplemented!("served in Plan 3") for now.
}
impl cairn_protocol::exports::cairn::daemon::meta::Handler<ConnCtx> for Daemon {
    async fn version(&self, _ctx: ConnCtx) -> anyhow::Result<VersionInfo> {
        Ok(handlers::meta::version())
    }
    async fn authenticate(&self, _ctx: ConnCtx, token: String) -> anyhow::Result<Result<(), WireError>> {
        Ok(handlers::meta::authenticate(token))
    }
    async fn whoami(&self, ctx: ConnCtx) -> anyhow::Result<Result<String, WireError>> {
        Ok(handlers::meta::whoami(&ctx))
    }
}
```

- [ ] **Step 5: `serve()` + socket hygiene + pump + drain (`serve.rs` part 2)**

```rust
use std::sync::Arc;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub async fn serve(daemon: crate::daemon::Daemon, shutdown: CancellationToken) -> anyhow::Result<()> {
    let listener = bind_with_cleanup(&daemon.cfg)?;
    let srv = Arc::new(wrpc_transport::Server::default());
    let pl = Arc::new(PeerCredListener(listener));

    let accept = tokio::spawn({
        let srv = Arc::clone(&srv);
        let pl = Arc::clone(&pl);
        let shutdown = shutdown.clone();
        async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    res = srv.accept(pl.as_ref()) => { if res.is_err() { break } }
                }
            }
        }
    });

    let invocations = cairn_protocol::serve(srv.as_ref(), daemon.clone()).await?;
    let pump = tokio::spawn(async move {
        use futures::stream::{select_all, StreamExt as _};
        let mut invocations = select_all(
            invocations.into_iter().map(|(i, n, s)| s.map(move |r| (i, n, r))),
        );
        while let Some((_i, _n, res)) = invocations.next().await {
            if let Ok(fut) = res { tokio::spawn(fut); }
        }
    });

    shutdown.cancelled().await;
    drain_sessions(&daemon, daemon.cfg.shutdown_grace).await;
    accept.abort();
    pump.abort();
    let _ = std::fs::remove_file(&daemon.cfg.socket_path);
    Ok(())
}

fn bind_with_cleanup(cfg: &crate::config::DaemonConfig) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(parent) = cfg.socket_path.parent() {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        if created {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(cfg.dir_mode))?;
        }
    }
    if cfg.socket_path.exists() {
        // Probe: a live daemon means refuse; connection-refused means stale.
        match std::os::unix::net::UnixStream::connect(&cfg.socket_path) {
            Ok(_) => anyhow::bail!("a daemon is already listening on {}", cfg.socket_path.display()),
            Err(_) => { let _ = std::fs::remove_file(&cfg.socket_path); }
        }
    }
    let listener = tokio::net::UnixListener::bind(&cfg.socket_path)?;
    std::fs::set_permissions(&cfg.socket_path, std::fs::Permissions::from_mode(cfg.socket_mode))?;
    Ok(listener)
}

async fn drain_sessions(daemon: &crate::daemon::Daemon, grace: std::time::Duration) {
    let entries = daemon.registry.list();
    for e in &entries {
        let _ = e.handle().signal(libc::SIGTERM).await;
    }
    let waits = entries.iter().map(|e| {
        let h = e.handle();
        async move { let _ = tokio::time::timeout(grace, h.wait()).await; }
    });
    futures::future::join_all(waits).await;
    // Dropping the registry's Arcs (on daemon teardown) is the SIGKILL backstop.
}
```

(`bind_with_cleanup` needs `libc` already in deps from Task 4. Confirm `cairn_protocol::serve` is the generated server entrypoint — same one `round_trip.rs` uses.)

- [ ] **Step 6: Real `main.rs`**

```rust
use clap::Parser;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(version, about = "The cairn session-manager daemon")]
struct Args {
    #[arg(long, env = "CAIRN_SOCKET")]
    socket: Option<std::path::PathBuf>,
    #[arg(long, env = "CAIRN_DIR_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    dir_mode: Option<u32>,
    #[arg(long, env = "CAIRN_SOCKET_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    socket_mode: Option<u32>,
    #[arg(long, env = "CAIRN_SHUTDOWN_GRACE", value_parser = humantime::parse_duration)]
    shutdown_grace: Option<std::time::Duration>,
    #[arg(long, env = "CAIRN_DEFAULT_SHELL")]
    default_shell: Option<String>,
    #[arg(long, env = "CAIRN_LOG", default_value = "info,cairn_daemon=info,cairn_pty=info")]
    log: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(args.log.clone()))
        .with_writer(std::io::stderr)
        .init();

    let mut cfg = cairn_daemon::config::DaemonConfig::default();
    if let Some(p) = args.socket { cfg.socket_path = p; }
    if let Some(m) = args.dir_mode { cfg.dir_mode = m; }
    if let Some(m) = args.socket_mode { cfg.socket_mode = m; }
    if let Some(g) = args.shutdown_grace { cfg.shutdown_grace = g; }
    if let Some(s) = args.default_shell { cfg.default_shell = s; }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        let daemon = cairn_daemon::daemon::Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let sig = shutdown.clone();
        tokio::spawn(async move {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = term.recv() => {},
            }
            sig.cancel();
        });
        cairn_daemon::serve::serve(daemon, shutdown).await
    })
}
```

(Add `humantime = "2"` to `cairn-daemon` deps for `--shutdown-grace`. Add `cairn_daemon::serve`, `::daemon`, `::config` to `lib.rs` exports — already there.)

- [ ] **Step 7: Test harness (`tests/common/mod.rs`)**

A harness that runs the real `Daemon` via `serve()` on a tempdir socket and hands back a `cairn_protocol::client` unix client.

```rust
#![allow(dead_code)]
use std::path::PathBuf;
use cairn_daemon::{config::DaemonConfig, daemon::Daemon, serve::serve};
use tokio_util::sync::CancellationToken;

pub struct DaemonHarness {
    pub socket_path: PathBuf,
    _tmp: tempfile::TempDir,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl DaemonHarness {
    pub async fn start() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");
        let mut cfg = DaemonConfig::default();
        cfg.socket_path = socket_path.clone();
        let daemon = Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone()));
        // Wait for the socket to appear (serve binds before accepting).
        for _ in 0..100 {
            if socket_path.exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Self { socket_path, _tmp: tmp, shutdown, task }
    }

    pub fn client(&self) -> wrpc_transport::unix::Client<PathBuf> {
        wrpc_transport::unix::Client::from(self.socket_path.clone())
    }
}

impl Drop for DaemonHarness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.task.abort();
    }
}
```

- [ ] **Step 8: `meta` integration tests (`tests/daemon_meta.rs`)**

```rust
mod common;
use common::DaemonHarness;
use cairn_protocol as bindings;

#[tokio::test]
async fn version_reports_daemon_and_protocol() {
    let h = DaemonHarness::start().await;
    let info = bindings::client::cairn::daemon::meta::version(&h.client(), ()).await.unwrap();
    assert!(info.daemon.starts_with("cairn-daemon/"));
    assert_eq!(info.protocol, "cairn:daemon@0.1.0");
}

#[tokio::test]
async fn whoami_returns_caller_uid() {
    let h = DaemonHarness::start().await;
    let who = bindings::client::cairn::daemon::meta::whoami(&h.client(), ()).await.unwrap().unwrap();
    let uid = unsafe { libc::geteuid() }.to_string();
    assert_eq!(who, uid); // v0 reports numeric uid
}

#[tokio::test]
async fn authenticate_is_ok_on_uds() {
    let h = DaemonHarness::start().await;
    let r = bindings::client::cairn::daemon::meta::authenticate(&h.client(), (), "ignored").await.unwrap();
    assert!(r.is_ok());
}
```

(Add `libc = "0.2"` to `cairn-daemon` `[dev-dependencies]` for the uid assert, or reuse the normal dep.)

- [ ] **Step 9: unary `sessions` integration tests (`tests/daemon_unary.rs`)**

```rust
mod common;
use common::DaemonHarness;
use cairn_protocol as bindings;
use bindings::cairn::daemon::types::{SessionSpec, Signal, SignalName};

fn spec(name: &str, cmd: &[&str]) -> SessionSpec {
    SessionSpec {
        name: Some(name.to_string()),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        env: vec![], env_inherit: true, workdir: None,
        tty: true, stdin: true, idle_timeout_secs: None, scrollback_lines: 100,
    }
}

#[tokio::test]
async fn create_then_list_then_inspect() {
    let h = DaemonHarness::start().await;
    let created = bindings::client::cairn::daemon::sessions::create(&h.client(), (), &spec("dev", &["sleep", "100"]))
        .await.unwrap().unwrap();
    assert_eq!(created.name, Some("dev".to_string()));

    let listed = bindings::client::cairn::daemon::sessions::list_all(&h.client(), ()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, created.id);

    let got = bindings::client::cairn::daemon::sessions::inspect(&h.client(), (), &created.id)
        .await.unwrap().unwrap();
    assert_eq!(got.id, created.id);
}

#[tokio::test]
async fn inspect_unknown_is_not_found() {
    let h = DaemonHarness::start().await;
    let err = bindings::client::cairn::daemon::sessions::inspect(&h.client(), (), "nope")
        .await.unwrap().unwrap_err();
    assert_eq!(err.code, "session.not_found");
}

#[tokio::test]
async fn duplicate_name_is_rejected() {
    let h = DaemonHarness::start().await;
    let _ = bindings::client::cairn::daemon::sessions::create(&h.client(), (), &spec("dev", &["sleep", "100"]))
        .await.unwrap().unwrap();
    let err = bindings::client::cairn::daemon::sessions::create(&h.client(), (), &spec("dev", &["sleep", "100"]))
        .await.unwrap().unwrap_err();
    assert_eq!(err.code, "session.name_in_use");
}

#[tokio::test]
async fn kill_term_stops_session() {
    let h = DaemonHarness::start().await;
    let created = bindings::client::cairn::daemon::sessions::create(&h.client(), (), &spec("dev", &["sleep", "100"]))
        .await.unwrap().unwrap();
    let sig = Signal::Named(SignalName::Term);
    bindings::client::cairn::daemon::sessions::kill(&h.client(), (), &created.id, &sig, None)
        .await.unwrap().unwrap();
    // Poll inspect until exit is populated.
    let mut exited = false;
    for _ in 0..50 {
        let got = bindings::client::cairn::daemon::sessions::inspect(&h.client(), (), &created.id)
            .await.unwrap().unwrap();
        if got.exit.is_some() { exited = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(exited, "session should have exited after SIGTERM");
}

#[tokio::test]
async fn kill_with_grace_escalates_to_sigkill() {
    let h = DaemonHarness::start().await;
    // Shell that ignores SIGTERM; only SIGKILL stops it.
    let created = bindings::client::cairn::daemon::sessions::create(
        &h.client(), (), &spec("stubborn", &["sh", "-c", "trap '' TERM; sleep 100"]),
    ).await.unwrap().unwrap();
    let sig = Signal::Named(SignalName::Term);
    bindings::client::cairn::daemon::sessions::kill(&h.client(), (), &created.id, &sig, Some(300))
        .await.unwrap().unwrap();
    let mut exited = false;
    for _ in 0..50 {
        let got = bindings::client::cairn::daemon::sessions::inspect(&h.client(), (), &created.id)
            .await.unwrap().unwrap();
        if got.exit.is_some() { exited = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(exited, "escalation should have SIGKILLed the stubborn session");
}

#[tokio::test]
async fn rename_and_restart() {
    let h = DaemonHarness::start().await;
    let created = bindings::client::cairn::daemon::sessions::create(&h.client(), (), &spec("old", &["sleep", "100"]))
        .await.unwrap().unwrap();
    bindings::client::cairn::daemon::sessions::rename(&h.client(), (), &created.id, "new")
        .await.unwrap().unwrap();
    let got = bindings::client::cairn::daemon::sessions::inspect(&h.client(), (), "new")
        .await.unwrap().unwrap();
    assert_eq!(got.name, Some("new".to_string()));

    // restart while running without force -> error
    let err = bindings::client::cairn::daemon::sessions::restart(&h.client(), (), &created.id, false)
        .await.unwrap().unwrap_err();
    assert_eq!(err.code, "session.running");
    // with force -> ok, same id
    bindings::client::cairn::daemon::sessions::restart(&h.client(), (), &created.id, true)
        .await.unwrap().unwrap();
}
```

- [ ] **Step 10: Build, test, clippy**

Run: `cargo test -p cairn-daemon && cargo clippy -p cairn-daemon --all-targets`
Expected: all PASS; clippy clean. Fix any signature mismatches against the regenerated `Handler` trait / client free-fn arities (e.g. whether client calls take `&id`/`&sig` by reference — match what the bindings expect, mirroring `cairn-protocol/tests/common/mod.rs`).

- [ ] **Step 11: Commit**

```bash
git add crates/cairn-daemon
git commit -m "feat(cairn-daemon): serve meta + unary session ops over UDS with peer-cred whoami"
```

---

## Self-review checklist (run before handing off)

- [ ] **Spec coverage (core slice):** crate scaffold, `DaemonConfig` (incl. `--dir-mode`/`--socket-mode`), error codes, signal translation, `SpawnOptions` mapping, registry (create/resolve/rename/restart, name uniqueness, exited-lingers), `ConnCtx`/`PeerCredListener` peer-cred `whoami`, socket hygiene (chmod-if-created dir + always-chmod socket + stale probe), graceful drain (TERM→grace), and all unary `meta`+`sessions` handlers incl. daemon-owned `kill` escalation. Streaming (attach/logs/send/wait) is explicitly deferred to Plan 3 (their `Handler` methods are `unimplemented!` for now).
- [ ] **No placeholders:** every code step has complete code; the two spots marked "adjust"/"pseudo" (`create` → async; `kick` client-match) have explicit instructions — resolve them concretely during implementation.
- [ ] **Type consistency:** `Daemon` is `Handler<ConnCtx>` (not `SocketAddr`); the `serve` `Server` is therefore `Server<ConnCtx, OwnedReadHalf, OwnedWriteHalf>`; client free-fn call arities match `cairn-protocol/tests/common/mod.rs`.
- [ ] **Locking discipline:** no entry lock held across `.await`; `handle()` clones the `Arc` out under a brief read lock, then awaits on the clone.
- [ ] `cargo test -p cairn-daemon` green; `cargo clippy -p cairn-daemon --all-targets` clean; whole workspace still builds.

When this lands, proceed to **Plan 3 — daemon streaming** (`attach` bridge, `logs`/`send`/`wait`, subprocess smoke test, and the README build-list checkoff).

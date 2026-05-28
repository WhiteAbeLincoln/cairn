# CLI client — non-interactive commands implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the eleven non-interactive `cairn` subcommands (`list`, `inspect`, `rename`, `restart`, `kill`, `kick`, `send`, `logs`, `wait`, `whoami`, `version`) per `docs/superpowers/specs/2026-05-27-cli-client-non-interactive-commands-design.md`, plus the forward-looking connection-layer refactor that keeps the future WebTransport addition a single-file change inside `connect.rs`.

**Architecture:** One file per command in `crates/cairn-client/src/`; one shared `targets.rs` for `SessionTargets`/`SessionTarget` resolution; `connect.rs` becomes a small enum so adding transports is mechanical. Plain-text output only — the `--output {plain,json,jsonl,wide}` enum and `list --filter` stay deferred behind their existing `cli.rs` TODOs. Multi-session ops resolve client-side via one `sessions.list_all` per command and run per-target work concurrently; best-effort on per-target errors.

**Tech Stack:** Rust 2024, tokio, `wit-bindgen-wrpc`-generated client functions from `cairn-protocol`, `globset = "0.4"`, `strip-ansi-escapes = "0.2"`, `time = "0.3"`, existing in-process daemon harness pattern from `tests/attach_pty.rs`.

---

## File map

**Create**
- `crates/cairn-client/src/targets.rs` — resolver
- `crates/cairn-client/src/meta.rs` — `whoami`, `version`
- `crates/cairn-client/src/list.rs`
- `crates/cairn-client/src/inspect.rs`
- `crates/cairn-client/src/rename.rs`
- `crates/cairn-client/src/restart.rs`
- `crates/cairn-client/src/send.rs`
- `crates/cairn-client/src/kick.rs`
- `crates/cairn-client/src/kill.rs`
- `crates/cairn-client/src/wait.rs`
- `crates/cairn-client/src/logs.rs`
- `crates/cairn-client/tests/common/mod.rs` — shared daemon harness
- `crates/cairn-client/tests/non_interactive.rs` — integration tests

**Modify**
- `crates/cairn-client/src/connect.rs` — `Endpoint` enum, `label()`
- `crates/cairn-client/src/attach.rs` — `endpoint.label()` instead of `endpoint.path().display()`
- `crates/cairn-client/src/cli.rs` — drop `conflicts_with = "timeout"` on `--no-wait`
- `crates/cairn-client/src/main.rs` — register modules, dispatch new commands; `resolve_target` moves out
- `crates/cairn-client/Cargo.toml` — add `globset`, `strip-ansi-escapes`, `time`

---

## Task 1: Refactor `connect.rs` — `Endpoint` enum and `label()`

Pure refactor; no behavior change. The two existing tests in `connect.rs` and `attach_pty.rs` both still pass.

**Files:**
- Modify: `crates/cairn-client/src/connect.rs`
- Modify: `crates/cairn-client/src/attach.rs`
- Modify: `crates/cairn-client/src/main.rs`

- [ ] **Step 1: Rewrite `connect.rs`**

Replace the entire file content with the enum-shaped version. The body of `from_uri` is structurally identical; only `Endpoint` itself changes shape.

```rust
//! Daemon endpoint resolution. v0 supports only the unix-socket transport.
//!
//! Only this file may name `wrpc_transport::*` types. Every other module in
//! `cairn-client` touches the wRPC backend through `Endpoint::client()`'s
//! return value generically — the value is passed straight into the generated
//! `cairn_protocol::client::*` functions, which are themselves
//! `<C: wrpc_transport::Invoke>`. When a second transport (e.g. WebTransport)
//! lands, the only edits are inside this file: add an `Endpoint` variant, add
//! a `Client` enum variant, write the forwarding `Invoke` impl.

use std::path::PathBuf;

use anyhow::{Result, bail};

/// The wRPC client type for the local unix-socket transport. Cheap to clone
/// (holds only the socket path); each invocation opens a fresh connection.
///
/// Today's `Client` is just the UDS client. When WebTransport lands, this
/// alias becomes an enum wrapper with a forwarding `wrpc_transport::Invoke`
/// impl — the public `Endpoint::client()` API stays unchanged.
pub type Client = wrpc_transport::unix::Client<PathBuf>;

/// A resolved daemon endpoint. v0 has only the `Unix` variant; future
/// transports add variants alongside it.
#[derive(Debug)]
pub enum Endpoint {
    Unix(PathBuf),
}

impl Endpoint {
    /// Resolve from `--daemon` / `CAIRN_DAEMON` (already read by clap) or the
    /// platform default socket.
    pub fn resolve(daemon: Option<&str>) -> Result<Self> {
        match daemon {
            None => Ok(Self::Unix(default_socket())),
            Some(s) => Self::from_uri(s),
        }
    }

    fn from_uri(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix("unix://") {
            if rest.is_empty() {
                bail!("`--daemon unix://` has no socket path");
            }
            return Ok(Self::Unix(PathBuf::from(rest)));
        }
        if s.starts_with('/') {
            return Ok(Self::Unix(PathBuf::from(s)));
        }
        if s.starts_with("ws://") || s.starts_with("wss://") {
            bail!("remote transports (WebTransport) are not yet supported; v0 is unix-socket only");
        }
        bail!("unrecognized --daemon endpoint {s:?} (expected `unix:///path/to/cairn.sock`)");
    }

    /// Human-readable label used in error messages. Avoids leaking
    /// transport-specific accessors (a future `Wt` variant has no `Path`).
    pub fn label(&self) -> String {
        match self {
            Self::Unix(p) => format!("unix://{}", p.display()),
        }
    }

    pub fn client(&self) -> Client {
        match self {
            Self::Unix(p) => wrpc_transport::unix::Client::from(p.clone()),
        }
    }
}

/// `$XDG_RUNTIME_DIR/cairn/cairn.sock`, else `$TMPDIR/cairn/cairn.sock`, else
/// `/tmp/cairn/cairn.sock` — identical to the daemon's default.
fn default_socket() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn").join("cairn.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_resolves_to_unix_with_cairn_sock_suffix() {
        match Endpoint::resolve(None).unwrap() {
            Endpoint::Unix(p) => assert!(p.ends_with("cairn/cairn.sock"), "got {p:?}"),
        }
    }

    #[test]
    fn unix_uri_yields_its_path() {
        match Endpoint::resolve(Some("unix:///run/cairn/x.sock")).unwrap() {
            Endpoint::Unix(p) => assert_eq!(p, Path::new("/run/cairn/x.sock")),
        }
    }

    #[test]
    fn bare_absolute_path_is_accepted() {
        match Endpoint::resolve(Some("/tmp/y.sock")).unwrap() {
            Endpoint::Unix(p) => assert_eq!(p, Path::new("/tmp/y.sock")),
        }
    }

    #[test]
    fn websocket_endpoints_are_rejected() {
        let err = Endpoint::resolve(Some("wss://host:443")).unwrap_err();
        assert!(err.to_string().contains("not yet supported"), "got {err}");
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!(Endpoint::resolve(Some("http://host")).is_err());
    }

    #[test]
    fn label_renders_unix_uri() {
        let ep = Endpoint::resolve(Some("/tmp/y.sock")).unwrap();
        assert_eq!(ep.label(), "unix:///tmp/y.sock");
    }
}
```

- [ ] **Step 2: Update `attach.rs` callsite**

The current code reads `endpoint.path().display()`. Replace with `endpoint.label()`. There is exactly one such site; locate it with:

```bash
grep -n 'endpoint\.path\|\.path()\.display' crates/cairn-client/src/attach.rs
```

Replace each occurrence:

```rust
// before
anyhow::anyhow!("cannot reach cairn-daemon at {}: {e}", endpoint.path().display())
// after
anyhow::anyhow!("cannot reach cairn-daemon at {}: {e}", endpoint.label())
```

- [ ] **Step 3: Update `main.rs` callsite**

Same change in `resolve_target` at `main.rs:75-78`:

```rust
let mut all = sessions::list_all(&client, ())
    .await
    .map_err(|e| anyhow::anyhow!("cannot reach cairn-daemon at {}: {e}", endpoint.label()))?;
```

- [ ] **Step 4: Build + run all existing tests**

```bash
cargo nextest run -p cairn
```

Expected: all tests pass, including the new `label_renders_unix_uri` unit test and the existing `attach_pty.rs` integration test.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-client/src/connect.rs crates/cairn-client/src/attach.rs crates/cairn-client/src/main.rs
git commit -m "refactor(cairn-client): make Endpoint an enum so adding transports stays a single-file change"
```

---

## Task 2: Add deps + extract resolver into `targets.rs`

The interactive spec's `resolve_target` helper in `main.rs:72-87` moves into `targets.rs` and is generalized to also handle `SessionTargets`. Adds `globset` for pattern matching.

**Files:**
- Modify: `crates/cairn-client/Cargo.toml`
- Create: `crates/cairn-client/src/targets.rs`
- Modify: `crates/cairn-client/src/main.rs`

- [ ] **Step 1: Add `globset` to `Cargo.toml`**

Append under `[dependencies]` in `crates/cairn-client/Cargo.toml`, alphabetically between `futures` and `humantime`:

```toml
globset = { version = "0.4", default-features = false }
```

- [ ] **Step 2: Write failing unit tests for the resolver (drives interface)**

Create `crates/cairn-client/src/targets.rs` with the test module first (test-first; production code lands in step 4):

```rust
//! Resolve `SessionTarget` / `SessionTargets` against the daemon's `list_all`.
//! One `list_all` call per command; literal name-or-id match, glob match
//! against names (`*`, `?`, `[`), `--latest`, and `--all`. Per-token misses
//! inside a positional list are collected, not fatal.

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::SessionInfo;

use crate::cli::{SessionTarget, SessionTargets};
use crate::connect::Endpoint;

/// A session that the user wants to operate on.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub id: String,
    pub name: Option<String>,
    pub info: SessionInfo,
}

/// Outcome of resolving a `SessionTargets` set.
#[derive(Debug, Default)]
pub struct ResolvedMany {
    /// Sessions that matched, in stable first-occurrence order, de-duplicated.
    pub matched: Vec<ResolvedTarget>,
    /// Positional-list tokens (literal or glob) that matched nothing; surfaced
    /// per-target by the calling command, not fatal here.
    pub unresolved: Vec<String>,
}

pub async fn resolve_one(ep: &Endpoint, t: &SessionTarget) -> Result<ResolvedTarget> {
    let sessions = list_all(ep).await?;
    resolve_one_in(&sessions, t)
}

pub async fn resolve_many(ep: &Endpoint, t: &SessionTargets) -> Result<ResolvedMany> {
    let sessions = list_all(ep).await?;
    Ok(resolve_many_in(&sessions, t))
}

// ── Pure-logic core (tested below via fixtures, no daemon) ────────────────

fn resolve_one_in(sessions: &[SessionInfo], t: &SessionTarget) -> Result<ResolvedTarget> {
    if t.latest {
        let latest = sessions
            .iter()
            .max_by_key(|s| s.created_at_unix_ms)
            .ok_or_else(|| anyhow::anyhow!("no sessions to operate on"))?;
        return Ok(into_target(latest));
    }
    if let Some(s) = &t.session {
        return match find_literal(sessions, s) {
            Some(info) => Ok(into_target(info)),
            None => Err(anyhow::anyhow!("no session matches {s}")),
        };
    }
    anyhow::bail!("no session specified")
}

fn resolve_many_in(sessions: &[SessionInfo], t: &SessionTargets) -> ResolvedMany {
    let mut matched: Vec<ResolvedTarget> = Vec::new();
    let mut unresolved: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let push = |sess: &SessionInfo,
                matched: &mut Vec<ResolvedTarget>,
                seen: &mut std::collections::HashSet<String>| {
        if seen.insert(sess.id.clone()) {
            matched.push(into_target(sess));
        }
    };

    if t.latest {
        if let Some(latest) = sessions.iter().max_by_key(|s| s.created_at_unix_ms) {
            push(latest, &mut matched, &mut seen);
        }
    } else if t.all {
        for s in sessions.iter().filter(|s| s.exit.is_none()) {
            push(s, &mut matched, &mut seen);
        }
    } else {
        for tok in &t.sessions {
            if is_glob(tok) {
                let any = match build_glob(tok) {
                    Ok(matcher) => {
                        let mut hit = false;
                        for s in sessions {
                            if let Some(name) = &s.name
                                && matcher.is_match(name)
                            {
                                push(s, &mut matched, &mut seen);
                                hit = true;
                            }
                        }
                        hit
                    }
                    Err(_) => false,
                };
                if !any {
                    unresolved.push(tok.clone());
                }
            } else {
                match find_literal(sessions, tok) {
                    Some(info) => push(info, &mut matched, &mut seen),
                    None => unresolved.push(tok.clone()),
                }
            }
        }
    }
    ResolvedMany { matched, unresolved }
}

fn into_target(s: &SessionInfo) -> ResolvedTarget {
    ResolvedTarget { id: s.id.clone(), name: s.name.clone(), info: s.clone() }
}

fn find_literal<'a>(sessions: &'a [SessionInfo], tok: &str) -> Option<&'a SessionInfo> {
    sessions.iter().find(|s| s.name.as_deref() == Some(tok)).or_else(|| sessions.iter().find(|s| s.id == tok))
}

fn is_glob(tok: &str) -> bool {
    tok.contains('*') || tok.contains('?') || tok.contains('[')
}

fn build_glob(pat: &str) -> Result<globset::GlobMatcher, globset::Error> {
    Ok(globset::Glob::new(pat)?.compile_matcher())
}

async fn list_all(ep: &Endpoint) -> Result<Vec<SessionInfo>> {
    use cairn_protocol::client::cairn::daemon::sessions;
    let client = ep.client();
    sessions::list_all(&client, ())
        .await
        .map_err(|e| anyhow::anyhow!("cannot reach cairn-daemon at {}: {e}", ep.label()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_protocol::cairn::daemon::types::{ExitStatus, SessionSpec};

    fn spec() -> SessionSpec {
        SessionSpec {
            name: None,
            command: vec![],
            env: vec![],
            env_inherit: true,
            workdir: None,
            tty: false,
            stdin: false,
            idle_timeout_secs: None,
            scrollback_lines: 1000,
        }
    }

    fn s(id: &str, name: Option<&str>, created: u64, exited: bool) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            name: name.map(str::to_string),
            pid: None,
            cols: 80,
            rows: 24,
            attached_clients: vec![],
            created_at_unix_ms: created,
            exit: if exited {
                Some(ExitStatus { code: Some(0), signal: None, unix_ms: created + 1 })
            } else {
                None
            },
            spec: spec(),
        }
    }

    fn many(tokens: &[&str], latest: bool, all: bool) -> SessionTargets {
        SessionTargets {
            sessions: tokens.iter().map(|s| (*s).to_string()).collect(),
            latest,
            all,
        }
    }

    #[test]
    fn one_latest_picks_max_created_at() {
        let xs = vec![s("a", Some("old"), 10, false), s("b", Some("new"), 20, false)];
        let t = SessionTarget { session: None, latest: true };
        let got = resolve_one_in(&xs, &t).unwrap();
        assert_eq!(got.id, "b");
    }

    #[test]
    fn one_literal_matches_name_then_id() {
        let xs = vec![s("a", Some("bash"), 1, false), s("b", Some("zsh"), 2, false)];
        let t = SessionTarget { session: Some("bash".into()), latest: false };
        assert_eq!(resolve_one_in(&xs, &t).unwrap().id, "a");
        let t = SessionTarget { session: Some("b".into()), latest: false };
        assert_eq!(resolve_one_in(&xs, &t).unwrap().id, "b");
    }

    #[test]
    fn one_unmatched_literal_errors_with_token() {
        let xs = vec![s("a", Some("bash"), 1, false)];
        let t = SessionTarget { session: Some("zsh".into()), latest: false };
        let err = resolve_one_in(&xs, &t).unwrap_err().to_string();
        assert!(err.contains("zsh"), "got {err}");
    }

    #[test]
    fn many_all_excludes_exited() {
        let xs = vec![s("a", Some("live"), 1, false), s("b", Some("dead"), 2, true)];
        let r = resolve_many_in(&xs, &many(&[], false, true));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);
        assert!(r.unresolved.is_empty());
    }

    #[test]
    fn many_glob_matches_names_only() {
        let xs = vec![
            s("a", Some("bash-3f"), 1, false),
            s("b", Some("bash-7c"), 2, false),
            s("c", Some("zsh-99"), 3, false),
            s("d", None, 4, false), // no name -> not matched by a glob
        ];
        let r = resolve_many_in(&xs, &many(&["bash-*"], false, false));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn many_dedups_literal_and_overlapping_glob() {
        let xs = vec![s("a", Some("bash-3f"), 1, false), s("b", Some("bash-7c"), 2, false)];
        let r = resolve_many_in(&xs, &many(&["bash-3f", "bash-*"], false, false));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]); // a appears once, even though both expressions match it
    }

    #[test]
    fn many_unresolved_tokens_collected_not_fatal() {
        let xs = vec![s("a", Some("bash-3f"), 1, false)];
        let r = resolve_many_in(&xs, &many(&["bash-3f", "ghost", "z-*"], false, false));
        assert_eq!(r.matched.len(), 1);
        assert_eq!(r.unresolved, vec!["ghost", "z-*"]);
    }

    #[test]
    fn many_latest_returns_max_only() {
        let xs = vec![s("a", Some("old"), 1, false), s("b", Some("new"), 2, false)];
        let r = resolve_many_in(&xs, &many(&[], true, false));
        let ids: Vec<&str> = r.matched.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["b"]);
    }
}
```

- [ ] **Step 3: Run tests — they should fail because `targets` is not yet declared in `main.rs`**

```bash
cargo nextest run -p cairn targets
```

Expected: compile error — `targets` module not declared.

- [ ] **Step 4: Wire the module into `main.rs`**

Add the module declaration alongside the existing ones in `crates/cairn-client/src/main.rs`:

```rust
mod attach;
mod cli;
mod connect;
mod detach;
mod exec;
mod signals;
mod targets;
mod terminal;
```

Also replace the existing `resolve_target` helper (`main.rs:72-87`) so the attach-dispatch arm goes through `targets::resolve_one`:

```rust
Command::Attach { session, no_stdin, detach_keys } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    let target = targets::resolve_one(&endpoint, session).await?;
    let opts = AttachOptions {
        no_stdin: *no_stdin,
        detach_keys: DetachKeys::parse_or_default(detach_keys.as_deref())
            .map_err(|e| anyhow::anyhow!(e))?,
    };
    attach::run(&endpoint, &target.id, opts).await
}
```

Remove the now-dead `resolve_target` function from `main.rs`.

- [ ] **Step 5: Run all tests**

```bash
cargo nextest run -p cairn
```

Expected: all unit tests in `targets::tests` pass; the existing `attach_pty.rs` integration test still passes (the attach dispatch arm now routes through `targets::resolve_one`).

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/targets.rs crates/cairn-client/src/main.rs crates/cairn-client/Cargo.toml
git commit -m "feat(cairn-client): shared SessionTargets resolver (literal + glob + --latest + --all)"
```

---

## Task 3: `cli.rs` — drop `conflicts_with` from `--no-wait`

**Files:**
- Modify: `crates/cairn-client/src/cli.rs`

- [ ] **Step 1: Edit the `--no-wait` clap attribute and its docstring**

Find the field at `cli.rs:173-176`:

```rust
        /// Don't wait for the session(s) to actually exit; return as
        /// soon as the signal has been dispatched. Mutually exclusive
        /// with `--timeout`.
        #[clap(long, conflicts_with = "timeout")]
        no_wait: bool,
```

Replace with:

```rust
        /// Don't wait for the session(s) to actually exit; return as
        /// soon as the signal has been dispatched. Independent of
        /// `--timeout`: `--no-wait --timeout 5s` dispatches the signal,
        /// returns immediately, and lets the daemon escalate to SIGKILL
        /// after the grace period.
        #[clap(long)]
        no_wait: bool,
```

- [ ] **Step 2: Run the existing clap verifier**

```bash
cargo nextest run -p cairn verify_cli
```

Expected: `verify_cli` still passes (clap's `debug_assert` validates the schema).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/cli.rs
git commit -m "fix(cairn-client): --no-wait and --timeout are orthogonal on cairn kill"
```

---

## Task 4: Shared test harness in `tests/common/mod.rs`

Extract the in-process-daemon setup pattern from `tests/attach_pty.rs` so every new integration test can reuse it.

**Files:**
- Create: `crates/cairn-client/tests/common/mod.rs`

- [ ] **Step 1: Write the harness**

```rust
//! Shared harness for `cairn` binary integration tests.
//!
//! Spins an in-process `cairn-daemon` on a tempdir UDS, then runs the real
//! `cairn` client binary against it. Tests get a `Harness` with helpers to
//! create sessions, call wRPC ops directly (for setup/assertion), and exec
//! the binary.
#![allow(dead_code)] // not every test uses every helper

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use cairn_daemon::config::DaemonConfig;
use cairn_daemon::daemon::Daemon;
use cairn_protocol::cairn::daemon::types::{SessionInfo, SessionSpec};
use cairn_protocol::client::cairn::daemon as api;
use tokio_util::sync::CancellationToken;

pub struct Harness {
    pub daemon: Daemon,
    pub socket: PathBuf,
    pub shutdown: CancellationToken,
    serve: tokio::task::JoinHandle<()>,
    _tmp: tempfile::TempDir,
}

impl Harness {
    /// Start a fresh daemon on a tempdir socket. Waits up to 2 s for the
    /// socket to appear.
    pub async fn start() -> anyhow::Result<Self> {
        let tmp = tempfile::tempdir()?;
        let socket = tmp.path().join("cairn.sock");
        let mut cfg = DaemonConfig::default();
        cfg.socket_path = socket.clone();
        let daemon = Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let serve = {
            let daemon = daemon.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move {
                let _ = cairn_daemon::serve::serve(daemon, shutdown).await;
            })
        };
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        anyhow::ensure!(socket.exists(), "daemon socket was not created in time");
        Ok(Self { daemon, socket, shutdown, serve, _tmp: tmp })
    }

    /// Convenience: a spec for a session running `cmd args...`.
    pub fn spec(cmd: &[&str], name: Option<&str>) -> SessionSpec {
        SessionSpec {
            name: name.map(str::to_string),
            command: cmd.iter().map(|s| (*s).to_string()).collect(),
            env: vec![],
            env_inherit: true,
            workdir: None,
            tty: true,
            stdin: true,
            idle_timeout_secs: None,
            scrollback_lines: 100,
        }
    }

    /// Create a session through the registry, returning its `SessionInfo`.
    pub async fn create(&self, spec: SessionSpec) -> anyhow::Result<SessionInfo> {
        self.daemon
            .registry
            .create(spec, &self.daemon.cfg.default_shell)
            .await
            .map_err(|e| anyhow::anyhow!("create: {e:?}"))
    }

    /// Run the real `cairn` binary against this daemon with the given args.
    /// `stdin` is fed as bytes; stdout/stderr are captured.
    pub fn run(&self, args: &[&str], stdin: &[u8]) -> std::io::Result<Output> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.arg("--daemon")
            .arg(format!("unix://{}", self.socket.display()))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn()?;
        use std::io::Write;
        if !stdin.is_empty() {
            child.stdin.as_mut().unwrap().write_all(stdin)?;
        }
        drop(child.stdin.take());
        child.wait_with_output()
    }

    /// Background variant of `run` — for follow/wait tests.
    pub fn spawn_run(&self, args: &[&str]) -> std::io::Result<std::process::Child> {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
        cmd.arg("--daemon")
            .arg(format!("unix://{}", self.socket.display()))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.spawn()
    }

    pub fn client(&self) -> wrpc_transport::unix::Client<PathBuf> {
        wrpc_transport::unix::Client::from(self.socket.clone())
    }

    pub async fn list_all(&self) -> anyhow::Result<Vec<SessionInfo>> {
        api::sessions::list_all(&self.client(), ())
            .await
            .map_err(|e| anyhow::anyhow!("list_all: {e}"))
    }

    pub async fn inspect(&self, id: &str) -> anyhow::Result<SessionInfo> {
        let r = api::sessions::inspect(&self.client(), (), id)
            .await
            .map_err(|e| anyhow::anyhow!("inspect: {e}"))?;
        r.map_err(|e| anyhow::anyhow!("inspect wire-err: {} {}", e.code, e.message))
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        // Best-effort: the serve task drops naturally when its socket closes.
        // We don't `.await` here (Drop is sync), and tokio will join on runtime
        // shutdown.
        let _ = &self.serve;
    }
}

/// Extract stdout from `Output` as a UTF-8 `String`.
pub fn stdout_str(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// Extract stderr from `Output` as a UTF-8 `String`.
pub fn stderr_str(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
```

- [ ] **Step 2: Write a smoke test in the integration test file**

Create `crates/cairn-client/tests/non_interactive.rs`:

```rust
//! Integration tests for the non-interactive `cairn` commands. Each test
//! spins a fresh in-process daemon (via `common::Harness`) and invokes the
//! real `cairn` binary against it.

mod common;

use common::{Harness, stderr_str, stdout_str};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harness_smoke() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // No sessions yet — list_all is non-fatal and returns empty.
    let xs = h.list_all().await?;
    assert!(xs.is_empty(), "fresh daemon has no sessions; got {xs:?}");
    Ok(())
}
```

- [ ] **Step 3: Run the smoke test**

```bash
cargo nextest run -p cairn --test non_interactive harness_smoke
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-client/tests/common/mod.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "test(cairn-client): shared daemon harness for non-interactive integration tests"
```

---

## Task 5: `meta.rs` — `whoami` + `version`

**Files:**
- Create: `crates/cairn-client/src/meta.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write a failing integration test for `whoami` and `version`**

Append to `crates/cairn-client/tests/non_interactive.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whoami_prints_identity_and_exits_zero() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["whoami"], b"")?;
    assert!(out.status.success(), "exit: {:?} stderr: {}", out.status, stderr_str(&out));
    assert!(!stdout_str(&out).trim().is_empty(), "whoami stdout should be non-empty");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_prints_client_and_daemon_rows_exit_zero() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["version"], b"")?;
    assert!(out.status.success(), "exit: {:?} stderr: {}", out.status, stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("cairn"), "missing client row: {stdout}");
    assert!(stdout.contains("daemon"), "missing daemon row: {stdout}");
    assert!(stdout.contains("cairn:daemon@0.1.0"), "missing protocol id: {stdout}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn version_with_unreachable_daemon_still_exits_zero() -> anyhow::Result<()> {
    // Don't start a daemon; point the client at a non-existent socket.
    let tmp = tempfile::tempdir()?;
    let bad = tmp.path().join("does-not-exist.sock");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .arg("--daemon")
        .arg(format!("unix://{}", bad.display()))
        .arg("version")
        .output()?;
    assert!(out.status.success(), "version must exit 0 even when daemon is down");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cairn"), "client row missing: {stdout}");
    assert!(stdout.contains("unreachable"), "daemon row missing 'unreachable': {stdout}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn whoami_with_unreachable_daemon_exits_nonzero() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let bad = tmp.path().join("does-not-exist.sock");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_cairn"))
        .arg("--daemon")
        .arg(format!("unix://{}", bad.display()))
        .arg("whoami")
        .output()?;
    assert!(!out.status.success(), "whoami is a connectivity probe; must exit non-zero on unreachable");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("cannot reach") || stderr.contains("error"), "stderr should describe the failure: {stderr}");
    Ok(())
}
```

- [ ] **Step 2: Verify the tests fail at compile or with the existing 'not implemented' bail**

```bash
cargo nextest run -p cairn --test non_interactive whoami_prints
```

Expected: FAIL — the current `main.rs` dispatch arm bails with `this command is not implemented yet…`.

- [ ] **Step 3: Implement `meta.rs`**

Create `crates/cairn-client/src/meta.rs`:

```rust
//! `cairn whoami` and `cairn version`.

use anyhow::Result;
use cairn_protocol::client::cairn::daemon::meta;

use crate::connect::Endpoint;

pub async fn whoami(endpoint: &Endpoint) -> Result<i32> {
    let client = endpoint.client();
    match meta::whoami(&client, ()).await {
        Ok(Ok(identity)) => {
            println!("{identity}");
            Ok(0)
        }
        Ok(Err(e)) => {
            eprintln!("error: {}: {}", e.code, e.message);
            Ok(1)
        }
        Err(e) => {
            eprintln!("cannot reach cairn-daemon at {}: {e}", endpoint.label());
            Ok(1)
        }
    }
}

pub async fn version(endpoint: &Endpoint) -> Result<i32> {
    println!("cairn {}", env!("CARGO_PKG_VERSION"));
    let client = endpoint.client();
    match meta::version(&client, ()).await {
        Ok(v) => println!("daemon: {} · {}", v.daemon, v.protocol),
        Err(e) => println!("daemon: unreachable: {e}"),
    }
    Ok(0)
}
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add the module and the dispatch arms. After `mod targets;` add:

```rust
mod meta;
```

Then in the `match &cli.command` block, add arms before the fallthrough `_ =>`:

```rust
Command::Whoami => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    meta::whoami(&endpoint).await
}
Command::Version => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    meta::version(&endpoint).await
}
```

- [ ] **Step 5: Run the tests**

```bash
cargo nextest run -p cairn --test non_interactive whoami version
```

Expected: all four tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/meta.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn whoami and cairn version"
```

---

## Task 6: `list.rs`

**Files:**
- Create: `crates/cairn-client/src/list.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write failing integration tests**

Append to `crates/cairn-client/tests/non_interactive.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_on_empty_registry_says_no_sessions() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["list"], b"")?;
    assert!(out.status.success(), "exit: {:?} stderr: {}", out.status, stderr_str(&out));
    assert!(stdout_str(&out).contains("no sessions"), "got {}", stdout_str(&out));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_shows_each_session_name() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    h.create(Harness::spec(&["cat"], Some("alpha"))).await?;
    h.create(Harness::spec(&["cat"], Some("bravo"))).await?;

    let out = h.run(&["list"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("alpha"), "alpha row missing: {stdout}");
    assert!(stdout.contains("bravo"), "bravo row missing: {stdout}");
    Ok(())
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo nextest run -p cairn --test non_interactive list_on_empty list_shows
```

Expected: FAIL with `not implemented yet`.

- [ ] **Step 3: Implement `list.rs`**

Create `crates/cairn-client/src/list.rs`:

```rust
//! `cairn list` / `cairn ls`: plain-text table of sessions ordered by
//! `created_at_unix_ms`.

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::SessionInfo;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::connect::Endpoint;

pub async fn run(endpoint: &Endpoint) -> Result<i32> {
    let client = endpoint.client();
    let mut sessions = match sessions::list_all(&client, ()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot reach cairn-daemon at {}: {e}", endpoint.label());
            return Ok(1);
        }
    };
    if sessions.is_empty() {
        println!("no sessions");
        return Ok(0);
    }
    sessions.sort_by_key(|s| s.created_at_unix_ms);
    let rows: Vec<[String; 5]> = sessions.iter().map(render_row).collect();
    print_table(&["NAME", "ID", "SIZE", "CLIENTS", "STATE"], &rows);
    Ok(0)
}

fn render_row(s: &SessionInfo) -> [String; 5] {
    let name = s.name.clone().unwrap_or_else(|| "(unnamed)".into());
    let short_id = s.id.chars().take(12).collect::<String>();
    let size = format!("{}x{}", s.cols, s.rows);
    let clients = s.attached_clients.len().to_string();
    let state = match &s.exit {
        None => "running".to_string(),
        Some(e) => match (e.code, e.signal) {
            (Some(c), _) => format!("exited code={c}"),
            (_, Some(sig)) => format!("exited signal={sig}"),
            _ => "exited".into(),
        },
    };
    [truncate(&name, 40), short_id, size, clients, state]
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn print_table(headers: &[&str], rows: &[[String; 5]]) {
    let widths: Vec<usize> = (0..headers.len())
        .map(|i| {
            let cell_max = rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0);
            cell_max.max(headers[i].chars().count())
        })
        .collect();
    let mut line = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            line.push_str("  ");
        }
        line.push_str(h);
        for _ in h.chars().count()..widths[i] {
            line.push(' ');
        }
    }
    println!("{line}");
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            for _ in cell.chars().count()..widths[i] {
                line.push(' ');
            }
        }
        println!("{line}");
    }
}
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add `mod list;` and a dispatch arm:

```rust
Command::List => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    list::run(&endpoint).await
}
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn --test non_interactive list_
```

Expected: both `list_*` tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/list.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn list (plain-text table)"
```

---

## Task 7: `inspect.rs`

**Files:**
- Modify: `crates/cairn-client/Cargo.toml` — add `time`
- Create: `crates/cairn-client/src/inspect.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Add `time` to `Cargo.toml`**

Under `[dependencies]`:

```toml
time = { version = "0.3", default-features = false, features = ["formatting", "std"] }
```

- [ ] **Step 2: Write a failing integration test**

Append to `tests/non_interactive.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspect_renders_command_and_workdir() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h.create(Harness::spec(&["cat"], Some("ins"))).await?;

    let out = h.run(&["inspect", "ins"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains(&info.id), "id missing: {stdout}");
    assert!(stdout.contains("cat"), "command missing: {stdout}");
    assert!(stdout.contains("running"), "state missing: {stdout}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inspect_unknown_target_errors_and_exits_one() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["inspect", "no-such"], b"")?;
    assert!(!out.status.success(), "should exit non-zero");
    let stderr = stderr_str(&out);
    assert!(stderr.contains("no-such"), "stderr missing token: {stderr}");
    Ok(())
}
```

- [ ] **Step 3: Verify the tests fail**

```bash
cargo nextest run -p cairn --test non_interactive inspect_
```

Expected: FAIL.

- [ ] **Step 4: Implement `inspect.rs`**

Create `crates/cairn-client/src/inspect.rs`:

```rust
//! `cairn inspect`: render all known metadata for a single session as an
//! aligned key/value block.

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::SessionInfo;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(endpoint: &Endpoint, target: &SessionTarget) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let client = endpoint.client();
    let info = match sessions::inspect(&client, (), &resolved.id).await {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => {
            eprintln!("error: {}: {}", e.code, e.message);
            return Ok(1);
        }
        Err(e) => {
            eprintln!("cannot reach cairn-daemon at {}: {e}", endpoint.label());
            return Ok(1);
        }
    };
    print_block(&info);
    Ok(0)
}

fn print_block(s: &SessionInfo) {
    let rows: Vec<(&str, String)> = vec![
        ("id", s.id.clone()),
        ("name", s.name.clone().unwrap_or_else(|| "(unnamed)".into())),
        ("pid", s.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into())),
        ("state", state_of(s)),
        ("size", format!("{}x{}", s.cols, s.rows)),
        ("created_at", rfc3339_or_raw(s.created_at_unix_ms)),
        ("command", shell_quote_join(&s.spec.command)),
        ("workdir", s.spec.workdir.clone().unwrap_or_else(|| "(daemon default)".into())),
        ("tty", s.spec.tty.to_string()),
        ("stdin", s.spec.stdin.to_string()),
        ("env_inherit", s.spec.env_inherit.to_string()),
        ("idle_timeout", s.spec.idle_timeout_secs.map(|t| format!("{t}s")).unwrap_or_else(|| "none".into())),
        ("scrollback_lines", s.spec.scrollback_lines.to_string()),
        ("attached_clients", attached_str(&s.attached_clients)),
    ];
    let key_width = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    for (k, v) in rows {
        println!("{:<width$}  {}", k, v, width = key_width);
    }
}

fn state_of(s: &SessionInfo) -> String {
    match &s.exit {
        None => "running".into(),
        Some(e) => {
            let when = rfc3339_or_raw(e.unix_ms);
            match (e.code, e.signal) {
                (Some(c), _) => format!("exited code={c} at {when}"),
                (_, Some(sig)) => format!("exited signal={sig} at {when}"),
                _ => format!("exited at {when}"),
            }
        }
    }
}

fn rfc3339_or_raw(unix_ms: u64) -> String {
    let secs = (unix_ms / 1000) as i64;
    let nanos = ((unix_ms % 1000) * 1_000_000) as u32;
    let Ok(odt) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return unix_ms.to_string();
    };
    let odt = odt.replace_nanosecond(nanos).unwrap_or(odt);
    odt.format(&time::format_description::well_known::Rfc3339).unwrap_or_else(|_| unix_ms.to_string())
}

fn shell_quote_join(words: &[String]) -> String {
    if words.is_empty() {
        return "(daemon default)".into();
    }
    words
        .iter()
        .map(|w| if needs_quoting(w) { format!("'{}'", w.replace('\'', r"'\''")) } else { w.clone() })
        .collect::<Vec<_>>()
        .join(" ")
}

fn needs_quoting(w: &str) -> bool {
    w.is_empty() || w.chars().any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '=' | ':' | ',')))
}

fn attached_str(ids: &[String]) -> String {
    if ids.is_empty() {
        "0".into()
    } else {
        format!("{} ({})", ids.len(), ids.join(", "))
    }
}
```

- [ ] **Step 5: Wire dispatch in `main.rs`**

Add `mod inspect;` and:

```rust
Command::Inspect { session } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    inspect::run(&endpoint, session).await
}
```

- [ ] **Step 6: Run tests**

```bash
cargo nextest run -p cairn --test non_interactive inspect_
```

Expected: both pass.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-client/Cargo.toml crates/cairn-client/src/inspect.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn inspect (key/value session metadata)"
```

---

## Task 8: `rename.rs`

**Files:**
- Create: `crates/cairn-client/src/rename.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write failing integration tests**

Append to `tests/non_interactive.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rename_changes_the_session_name() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h.create(Harness::spec(&["cat"], Some("before"))).await?;

    let out = h.run(&["rename", "before", "--to", "after"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    let fresh = h.inspect(&info.id).await?;
    assert_eq!(fresh.name.as_deref(), Some("after"));
    Ok(())
}
```

- [ ] **Step 2: Verify failure**

```bash
cargo nextest run -p cairn --test non_interactive rename_
```

Expected: FAIL.

- [ ] **Step 3: Implement `rename.rs`**

```rust
//! `cairn rename <target> --to <new-name>`.

use anyhow::Result;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(endpoint: &Endpoint, target: &SessionTarget, new_name: &str) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let client = endpoint.client();
    match sessions::rename(&client, (), &resolved.id, &new_name.to_string()).await {
        Ok(Ok(())) => Ok(0),
        Ok(Err(e)) => {
            eprintln!("error: {}: {}", e.code, e.message);
            Ok(1)
        }
        Err(e) => {
            eprintln!("cannot reach cairn-daemon at {}: {e}", endpoint.label());
            Ok(1)
        }
    }
}
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add `mod rename;` and:

```rust
Command::Rename { session, new_name } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    rename::run(&endpoint, session, new_name).await
}
```

- [ ] **Step 5: Run test**

```bash
cargo nextest run -p cairn --test non_interactive rename_
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/rename.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn rename"
```

---

## Task 9: `restart.rs`

**Files:**
- Create: `crates/cairn-client/src/restart.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write a failing integration test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn restart_force_replaces_the_child_process() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // sh -c 'while true; do sleep 1; done' — a stable, restartable child.
    let info = h.create(Harness::spec(&["sh", "-c", "while true; do sleep 1; done"], Some("loopy"))).await?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let before = h.inspect(&info.id).await?.pid;

    let out = h.run(&["restart", "loopy", "--force"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let after = h.inspect(&info.id).await?.pid;
    assert!(before.is_some() && after.is_some() && before != after, "pid before={before:?} after={after:?}");
    Ok(())
}
```

- [ ] **Step 2: Verify failure**

```bash
cargo nextest run -p cairn --test non_interactive restart_
```

Expected: FAIL.

- [ ] **Step 3: Implement `restart.rs`**

```rust
//! `cairn restart <target> [--force]`.

use anyhow::Result;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(endpoint: &Endpoint, target: &SessionTarget, force: bool) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let client = endpoint.client();
    match sessions::restart(&client, (), &resolved.id, force).await {
        Ok(Ok(())) => Ok(0),
        Ok(Err(e)) => {
            eprintln!("error: {}: {}", e.code, e.message);
            Ok(1)
        }
        Err(e) => {
            eprintln!("cannot reach cairn-daemon at {}: {e}", endpoint.label());
            Ok(1)
        }
    }
}
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add `mod restart;` and:

```rust
Command::Restart { session, force } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    restart::run(&endpoint, session, *force).await
}
```

- [ ] **Step 5: Run test**

```bash
cargo nextest run -p cairn --test non_interactive restart_
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/restart.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn restart"
```

---

## Task 10: `send.rs`

**Files:**
- Create: `crates/cairn-client/src/send.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write a failing unit test for the argv-to-chunk helper**

Inside the still-to-be-created `send.rs`, the helper is `argv_to_chunk`. We'll write the unit tests inline in step 3. First, the integration tests in `tests/non_interactive.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_argv_joins_with_spaces_and_appends_newline() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // `cat` echoes whatever it receives on stdin to the PTY.
    let info = h.create(Harness::spec(&["cat"], Some("snd"))).await?;
    let out = h.run(&["send", "snd", "hello", "world"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));

    // Read the session's transcript via the logs op; assert it saw "hello world\n".
    // Allow up to 2 s for the daemon to round-trip the input.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let info = h.inspect(&info.id).await?;
        let _ = info; // (we read logs, not inspect, but inspect proves the session is still alive)
        let logs = read_snapshot(&h, "snd").await?;
        if logs.contains("hello world") {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("transcript never contained 'hello world'; got: {logs:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn send_stdin_streams_raw_bytes_no_trailing_newline() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h.create(Harness::spec(&["cat"], Some("raw"))).await?;
    let out = h.run(&["send", "raw"], b"abc")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let logs = read_snapshot(&h, "raw").await?;
    assert!(logs.contains("abc"), "expected 'abc' in transcript: {logs:?}");
    assert!(!logs.contains("abc\n"), "stdin path must not append a newline: {logs:?}");
    Ok(())
}

/// Drain the `logs(All, follow=false)` snapshot of `target` into a string.
async fn read_snapshot(h: &Harness, target: &str) -> anyhow::Result<String> {
    use cairn_protocol::cairn::daemon::types::LogWindow;
    use cairn_protocol::client::cairn::daemon::sessions;
    use futures::StreamExt as _;

    // Resolve target via list_all (test helper, not the production resolver).
    let xs = h.list_all().await?;
    let id = xs
        .iter()
        .find(|s| s.name.as_deref() == Some(target))
        .map(|s| s.id.clone())
        .ok_or_else(|| anyhow::anyhow!("no session named {target}"))?;
    let (mut stream, io) = sessions::logs(&h.client(), (), &id, &LogWindow::All, false)
        .await
        .map_err(|e| anyhow::anyhow!("logs: {e}"))?;
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }
    let mut out = Vec::new();
    while let Some(batch) = stream.next().await {
        for chunk in batch {
            out.extend_from_slice(&chunk);
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}
```

- [ ] **Step 2: Verify the tests fail**

```bash
cargo nextest run -p cairn --test non_interactive send_
```

Expected: FAIL.

- [ ] **Step 3: Implement `send.rs` with inline unit tests**

```rust
//! `cairn send <target> [-r/--raw] [input...]`: inject bytes into a session.
//! Argv form joins with single spaces and appends `\n` unless `--raw`.
//! Stdin form streams 8 KiB chunks raw.

use anyhow::Result;
use bytes::Bytes;
use cairn_protocol::client::cairn::daemon::sessions;
use futures::Stream;
use tokio::io::AsyncReadExt as _;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

const CHUNK_SIZE: usize = 8 * 1024;

pub async fn run(
    endpoint: &Endpoint,
    target: &SessionTarget,
    raw: bool,
    input: &[String],
) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let chunks: std::pin::Pin<Box<dyn Stream<Item = Vec<Bytes>> + Send>> = if input.is_empty() {
        Box::pin(stdin_stream())
    } else {
        let chunk = argv_to_chunk(input, raw);
        Box::pin(futures::stream::iter(vec![vec![chunk]]))
    };
    let client = endpoint.client();
    // `sessions::send` returns `Result<(Result<(), Error>, Option<io_future>)>`;
    // the io future drives the underlying transport and must be spawned.
    match sessions::send(&client, (), &resolved.id, chunks).await {
        Ok((wire, io)) => {
            if let Some(io) = io {
                tokio::spawn(async move {
                    let _ = io.await;
                });
            }
            match wire {
                Ok(()) => Ok(0),
                Err(e) => {
                    eprintln!("error: {}: {}", e.code, e.message);
                    Ok(1)
                }
            }
        }
        Err(e) => {
            eprintln!("cannot reach cairn-daemon at {}: {e}", endpoint.label());
            Ok(1)
        }
    }
}

fn argv_to_chunk(words: &[String], raw: bool) -> Bytes {
    let mut s = words.join(" ");
    if !raw {
        s.push('\n');
    }
    Bytes::from(s.into_bytes())
}

fn stdin_stream() -> impl Stream<Item = Vec<Bytes>> + Send {
    async_stream::stream! {
        let mut stdin = tokio::io::stdin();
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => yield vec![Bytes::copy_from_slice(&buf[..n])],
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_joined_with_space_and_newline() {
        let got = argv_to_chunk(&["hello".into(), "world".into()], false);
        assert_eq!(&got[..], b"hello world\n");
    }

    #[test]
    fn argv_raw_omits_trailing_newline() {
        let got = argv_to_chunk(&["hello".into()], true);
        assert_eq!(&got[..], b"hello");
    }

    #[test]
    fn argv_empty_in_raw_is_empty_chunk() {
        let got = argv_to_chunk(&[], true);
        assert!(got.is_empty());
    }
}
```

Then add the missing dependency to `Cargo.toml` under `[dependencies]`:

```toml
async-stream = "0.3"
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add `mod send;` and:

```rust
Command::Send { session, raw, input } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    send::run(&endpoint, session, *raw, input).await
}
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn --test non_interactive send_
cargo nextest run -p cairn send::tests
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/Cargo.toml crates/cairn-client/src/send.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn send (argv or piped stdin)"
```

---

## Task 11: `kick.rs`

**Files:**
- Create: `crates/cairn-client/src/kick.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write a failing integration test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_all_on_empty_resolution_exits_two() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["kick", "--all"], b"")?;
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr_str(&out));
    assert!(stderr_str(&out).contains("no sessions matched"), "stderr: {}", stderr_str(&out));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_named_session_returns_zero() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h.create(Harness::spec(&["cat"], Some("kk"))).await?;
    let out = h.run(&["kick", "kk"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    Ok(())
}
```

- [ ] **Step 2: Verify failure**

```bash
cargo nextest run -p cairn --test non_interactive kick_
```

Expected: FAIL.

- [ ] **Step 3: Implement `kick.rs`**

```rust
//! `cairn kick` / `cairn detach`: detach attached clients without killing the
//! session. Multi-target; idempotent on `session.not_found` (the desired
//! terminal state is already true).

use anyhow::Result;
use cairn_protocol::client::cairn::daemon::sessions;
use futures::stream::{FuturesUnordered, StreamExt as _};

use crate::cli::SessionTargets;
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(endpoint: &Endpoint, sessions_arg: &SessionTargets, client_filter: Option<&str>) -> Result<i32> {
    let resolved = match targets::resolve_many(endpoint, sessions_arg).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let mut exit_code = 0;
    for t in &resolved.unresolved {
        eprintln!("error: {t}: no session matches");
        exit_code = 1;
    }
    if resolved.matched.is_empty() {
        eprintln!("no sessions matched");
        return Ok(2);
    }
    let client = endpoint.client();
    let mut tasks = FuturesUnordered::new();
    for t in &resolved.matched {
        let client = client.clone();
        let id = t.id.clone();
        let token = t.name.clone().unwrap_or_else(|| t.id.clone());
        let client_filter = client_filter.map(|s| s.to_string());
        tasks.push(async move {
            let result = sessions::kick(&client, (), &id, client_filter).await;
            (token, result)
        });
    }
    while let Some((token, result)) = tasks.next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) if e.code == "session.not_found" => {
                tracing::info!(target = %token, "kick: session already gone (no-op success)");
            }
            Ok(Err(e)) => {
                eprintln!("error: {}: {}: {}", token, e.code, e.message);
                exit_code = 1;
            }
            Err(e) => {
                eprintln!("error: {token}: {e}");
                exit_code = 1;
            }
        }
    }
    Ok(exit_code)
}
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add `mod kick;` and:

```rust
Command::Kick { sessions, client } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    kick::run(&endpoint, sessions, client.as_deref()).await
}
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn --test non_interactive kick_
```

Expected: both PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/kick.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn kick / detach (multi-target, idempotent)"
```

---

## Task 12: `kill.rs`

The complex multi-target command. `--no-wait` and `--timeout` are orthogonal.

**Files:**
- Create: `crates/cairn-client/src/kill.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write failing integration tests**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_blocks_until_session_exits() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h.create(Harness::spec(&["sleep", "30"], Some("zzz"))).await?;
    let start = std::time::Instant::now();
    let out = h.run(&["kill", "zzz"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(start.elapsed() < std::time::Duration::from_secs(5), "kill took too long: {:?}", start.elapsed());

    let after = h.inspect(&info.id).await?;
    assert!(after.exit.is_some(), "session should be exited; got {:?}", after.exit);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_no_wait_with_timeout_returns_immediately_then_daemon_escalates() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // bash -c 'trap "" TERM; sleep 30' ignores TERM, so SIGKILL escalation is the only way out.
    let info = h.create(Harness::spec(&["bash", "-c", "trap '' TERM; sleep 30"], Some("nope"))).await?;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let start = std::time::Instant::now();
    let out = h.run(&["kill", "--no-wait", "--timeout", "1s", "nope"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    assert!(start.elapsed() < std::time::Duration::from_millis(700), "should return ~immediately, took {:?}", start.elapsed());

    // Wait until the daemon-side escalation fires.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let info = h.inspect(&info.id).await?;
        if info.exit.is_some() {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("session never exited after daemon escalation; info={info:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kill_all_on_empty_registry_exits_two() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let out = h.run(&["kill", "--all"], b"")?;
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr_str(&out).contains("no sessions matched"));
    Ok(())
}
```

- [ ] **Step 2: Verify failure**

```bash
cargo nextest run -p cairn --test non_interactive kill_
```

Expected: FAIL.

- [ ] **Step 3: Implement `kill.rs` with the helpful unit test**

```rust
//! `cairn kill`: signal one-or-more sessions, optionally wait for exit and
//! optionally arm a daemon-side SIGKILL escalation.

use std::time::Duration;

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::{Signal as WireSignal, SignalName as WireSignalName};
use cairn_protocol::client::cairn::daemon::sessions;
use futures::stream::{FuturesUnordered, StreamExt as _};

use crate::cli::{SessionTargets, Signal, SignalName};
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(
    endpoint: &Endpoint,
    sessions_arg: &SessionTargets,
    signal: Signal,
    no_wait: bool,
    timeout: Option<Duration>,
) -> Result<i32> {
    let resolved = match targets::resolve_many(endpoint, sessions_arg).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let mut exit_code = 0;
    for t in &resolved.unresolved {
        eprintln!("error: {t}: no session matches");
        exit_code = 1;
    }
    if resolved.matched.is_empty() {
        eprintln!("no sessions matched");
        return Ok(2);
    }
    let grace_ms = grace_ms(timeout);
    let wire_sig = into_wire_signal(signal);
    let client = endpoint.client();
    let mut tasks = FuturesUnordered::new();
    for t in &resolved.matched {
        let client = client.clone();
        let id = t.id.clone();
        let token = t.name.clone().unwrap_or_else(|| t.id.clone());
        let wire_sig = wire_sig.clone();
        tasks.push(async move {
            let sig_result = sessions::kill(&client, (), &id, &wire_sig, grace_ms).await;
            let wait_result = if no_wait {
                Ok(())
            } else {
                // `wait` returns (future, Option<io_future>); drive both.
                match sessions::wait(&client, (), &id).await {
                    Ok((future, io)) => {
                        if let Some(io) = io {
                            tokio::spawn(async move {
                                let _ = io.await;
                            });
                        }
                        future.await;
                        Ok(())
                    }
                    Err(e) => Err(anyhow::anyhow!("{e}")),
                }
            };
            (token, sig_result, wait_result)
        });
    }
    while let Some((token, sig_result, wait_result)) = tasks.next().await {
        match sig_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                eprintln!("error: {}: {}: {}", token, e.code, e.message);
                exit_code = 1;
                continue;
            }
            Err(e) => {
                eprintln!("error: {token}: {e}");
                exit_code = 1;
                continue;
            }
        }
        if let Err(e) = wait_result {
            eprintln!("error: {token}: wait: {e}");
            exit_code = 1;
        }
    }
    Ok(exit_code)
}

fn grace_ms(timeout: Option<Duration>) -> Option<u32> {
    timeout.map(|d| u32::try_from(d.as_millis()).unwrap_or(u32::MAX))
}

fn into_wire_signal(sig: Signal) -> WireSignal {
    match sig {
        Signal::Named(name) => WireSignal::Named(into_wire_name(name)),
        Signal::Number(n) => WireSignal::Numbered(n),
    }
}

fn into_wire_name(name: SignalName) -> WireSignalName {
    use SignalName::*;
    match name {
        Hup => WireSignalName::Hup,
        Int => WireSignalName::Int,
        Quit => WireSignalName::Quit,
        Ill => WireSignalName::Ill,
        Trap => WireSignalName::Trap,
        Abrt => WireSignalName::Abrt,
        Bus => WireSignalName::Bus,
        Fpe => WireSignalName::Fpe,
        Kill => WireSignalName::Kill,
        Usr1 => WireSignalName::Usr1,
        Segv => WireSignalName::Segv,
        Usr2 => WireSignalName::Usr2,
        Pipe => WireSignalName::Pipe,
        Alrm => WireSignalName::Alrm,
        Term => WireSignalName::Term,
        Chld => WireSignalName::Chld,
        Cont => WireSignalName::Cont,
        Stop => WireSignalName::Stop,
        Tstp => WireSignalName::Tstp,
        Ttin => WireSignalName::Ttin,
        Ttou => WireSignalName::Ttou,
        Urg => WireSignalName::Urg,
        Xcpu => WireSignalName::Xcpu,
        Xfsz => WireSignalName::Xfsz,
        Vtalrm => WireSignalName::Vtalrm,
        Prof => WireSignalName::Prof,
        Winch => WireSignalName::Winch,
        Io => WireSignalName::Io,
        Sys => WireSignalName::Sys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grace_ms_none_when_no_timeout() {
        assert_eq!(grace_ms(None), None);
    }

    #[test]
    fn grace_ms_round_trips_small_durations() {
        assert_eq!(grace_ms(Some(Duration::from_millis(1500))), Some(1500));
    }

    #[test]
    fn grace_ms_saturates_at_u32_max() {
        // 100 days in ms is way past u32::MAX (~49 days). Saturate, don't drop.
        let huge = Duration::from_secs(100 * 24 * 60 * 60);
        assert_eq!(grace_ms(Some(huge)), Some(u32::MAX));
    }
}
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add `mod kill;` and:

```rust
Command::Kill { signal, no_wait, timeout, sessions } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    kill::run(&endpoint, sessions, *signal, *no_wait, *timeout).await
}
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn --test non_interactive kill_
cargo nextest run -p cairn kill::tests
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/kill.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn kill (multi-target, --no-wait orthogonal to --timeout)"
```

---

## Task 13: `wait.rs`

**Files:**
- Create: `crates/cairn-client/src/wait.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Write failing integration tests**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_returns_child_exit_code() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // `sh -c 'exit 7'` will exit with code 7 immediately.
    let _ = h.create(Harness::spec(&["sh", "-c", "exit 7"], Some("seven"))).await?;
    let out = h.run(&["wait", "seven"], b"")?;
    assert_eq!(out.status.code(), Some(7), "stderr: {}", stderr_str(&out));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wait_timeout_elapsed_exits_124_session_alive() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let info = h.create(Harness::spec(&["sleep", "30"], Some("slow"))).await?;
    let out = h.run(&["wait", "--timeout", "300ms", "slow"], b"")?;
    assert_eq!(out.status.code(), Some(124), "stderr: {}", stderr_str(&out));
    let after = h.inspect(&info.id).await?;
    assert!(after.exit.is_none(), "session must still be alive after a wait timeout; got {:?}", after.exit);
    Ok(())
}
```

- [ ] **Step 2: Verify failure**

```bash
cargo nextest run -p cairn --test non_interactive wait_
```

Expected: FAIL.

- [ ] **Step 3: Implement `wait.rs`**

```rust
//! `cairn wait <target> [--timeout T]`: block until exit, propagate the
//! child's exit code (`128+signal` if killed), or exit 124 on timeout.

use std::time::Duration;

use anyhow::Result;
use cairn_protocol::cairn::daemon::types::ExitStatus;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::cli::SessionTarget;
use crate::connect::Endpoint;
use crate::targets;

const TIMEOUT_EXIT_CODE: i32 = 124;

pub async fn run(endpoint: &Endpoint, target: &SessionTarget, timeout: Option<Duration>) -> Result<i32> {
    let resolved = match targets::resolve_one(endpoint, target).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let client = endpoint.client();
    // `sessions::wait` returns `Result<(future, Option<io_future>)>`: the
    // first future yields the ExitStatus, the second drives the underlying
    // transport and must be spawned for the call to make progress.
    let (future, io) = match sessions::wait(&client, (), &resolved.id).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    if let Some(io) = io {
        tokio::spawn(async move {
            let _ = io.await;
        });
    }

    let status = match timeout {
        Some(d) => match tokio::time::timeout(d, future).await {
            Ok(s) => s,
            Err(_) => return Ok(TIMEOUT_EXIT_CODE),
        },
        None => future.await,
    };
    Ok(exit_code_of(&status))
}

fn exit_code_of(status: &ExitStatus) -> i32 {
    if let Some(c) = status.code {
        return c;
    }
    if let Some(s) = status.signal {
        return 128 + i32::from(s);
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(code: Option<i32>, signal: Option<u8>) -> ExitStatus {
        ExitStatus { code, signal, unix_ms: 0 }
    }

    #[test]
    fn exit_code_uses_code_when_present() {
        assert_eq!(exit_code_of(&st(Some(7), None)), 7);
    }

    #[test]
    fn exit_code_uses_128_plus_signal_when_killed() {
        assert_eq!(exit_code_of(&st(None, Some(9))), 137);
    }

    #[test]
    fn exit_code_falls_back_to_one() {
        assert_eq!(exit_code_of(&st(None, None)), 1);
    }
}
```

- [ ] **Step 4: Wire dispatch in `main.rs`**

Add `mod wait;` and:

```rust
Command::Wait { session, timeout } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    wait::run(&endpoint, session, *timeout).await
}
```

- [ ] **Step 5: Run tests**

```bash
cargo nextest run -p cairn --test non_interactive wait_
cargo nextest run -p cairn wait::tests
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-client/src/wait.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn wait (propagate child code, --timeout=124)"
```

---

## Task 14: `logs.rs`

**Files:**
- Modify: `crates/cairn-client/Cargo.toml` — add `strip-ansi-escapes`
- Create: `crates/cairn-client/src/logs.rs`
- Modify: `crates/cairn-client/src/main.rs`
- Modify: `crates/cairn-client/tests/non_interactive.rs`

- [ ] **Step 1: Add `strip-ansi-escapes` to `Cargo.toml`**

Under `[dependencies]`, alphabetically:

```toml
strip-ansi-escapes = "0.2"
```

- [ ] **Step 2: Write failing integration tests**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_single_session_prints_snapshot() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h.create(Harness::spec(&["sh", "-c", "echo hello-from-the-pty"], Some("lg"))).await?;
    // Give the child time to print.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let out = h.run(&["logs", "lg"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("hello-from-the-pty"), "missing line: {stdout}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_strip_removes_ansi_escapes() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    // Print a red 'X' then reset.
    let _ = h
        .create(Harness::spec(&["sh", "-c", "printf '\\x1b[31mX\\x1b[0m\\n'"], Some("ansi")))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let out = h.run(&["logs", "--strip", "ansi"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains('X'), "missing X: {stdout:?}");
    assert!(!stdout.contains('\u{1b}'), "ANSI escape still present: {stdout:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_prefix_prepends_name_per_line() -> anyhow::Result<()> {
    let h = Harness::start().await?;
    let _ = h
        .create(Harness::spec(&["sh", "-c", "echo a-line && echo b-line"], Some("p")))
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let out = h.run(&["logs", "--prefix", "--strip", "p"], b"")?;
    assert!(out.status.success(), "stderr: {}", stderr_str(&out));
    let stdout = stdout_str(&out);
    assert!(stdout.contains("p: a-line"), "expected prefixed 'a-line': {stdout:?}");
    assert!(stdout.contains("p: b-line"), "expected prefixed 'b-line': {stdout:?}");
    Ok(())
}
```

- [ ] **Step 3: Verify failure**

```bash
cargo nextest run -p cairn --test non_interactive logs_
```

Expected: FAIL.

- [ ] **Step 4: Implement `logs.rs`**

```rust
//! `cairn logs <targets> [-f] [-n N] [--prefix] [--strip]`.

use anyhow::Result;
use bytes::Bytes;
use cairn_protocol::cairn::daemon::types::LogWindow;
use cairn_protocol::client::cairn::daemon::sessions;
use futures::stream::StreamExt as _;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc;

use crate::cli::SessionTargets;
use crate::connect::Endpoint;
use crate::targets;

pub async fn run(
    endpoint: &Endpoint,
    sessions_arg: &SessionTargets,
    strip: bool,
    prefix: bool,
    follow: bool,
    tail: Option<usize>,
) -> Result<i32> {
    let resolved = match targets::resolve_many(endpoint, sessions_arg).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return Ok(1);
        }
    };
    let mut exit_code = 0;
    for t in &resolved.unresolved {
        eprintln!("error: {t}: no session matches");
        exit_code = 1;
    }
    if resolved.matched.is_empty() {
        eprintln!("no sessions matched");
        return Ok(2);
    }

    let window = tail.map(|n| LogWindow::Tail(n as u32)).unwrap_or(LogWindow::All);
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);

    let client = endpoint.client();
    let mut stream_tasks = Vec::new();
    for t in &resolved.matched {
        let id = t.id.clone();
        let label = display_label(&t.name, &t.id);
        let out_tx = out_tx.clone();
        let client = client.clone();
        let window = window.clone();

        stream_tasks.push(tokio::spawn(async move {
            let result = sessions::logs(&client, (), &id, &window, follow).await;
            let (mut stream, io) = match result {
                Ok(pair) => pair,
                Err(e) => {
                    let line = format!("error: {label}: {e}\n");
                    let _ = out_tx.send(line.into_bytes()).await;
                    return 1i32;
                }
            };
            if let Some(io) = io {
                tokio::spawn(async move {
                    let _ = io.await;
                });
            }
            let mut buf = LineBuffer::new(if prefix { Some(label.clone()) } else { None });
            while let Some(batch) = stream.next().await {
                for chunk in batch {
                    let bytes: &[u8] = &chunk;
                    let bytes = if strip {
                        Bytes::from(strip_ansi_escapes::strip(bytes))
                    } else {
                        Bytes::copy_from_slice(bytes)
                    };
                    for piece in buf.feed(&bytes) {
                        if out_tx.send(piece).await.is_err() {
                            return 0;
                        }
                    }
                }
            }
            if let Some(rest) = buf.flush() {
                let _ = out_tx.send(rest).await;
            }
            0
        }));
    }
    drop(out_tx); // close the channel once all stream tasks finish

    // Writer: drain into stdout until all senders close.
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(batch) = out_rx.recv().await {
            if stdout.write_all(&batch).await.is_err() {
                break;
            }
        }
        let _ = stdout.flush().await;
    });

    // Wait for all per-target tasks, then the writer.
    for t in stream_tasks {
        if let Ok(code) = t.await
            && code != 0
        {
            exit_code = code;
        }
    }
    let _ = writer.await;
    Ok(exit_code)
}

fn display_label(name: &Option<String>, id: &str) -> String {
    match name {
        Some(n) => n.clone(),
        None => id.chars().take(8).collect(),
    }
}

/// Re-emit bytes line-by-line, optionally prefixing each completed line with
/// `<label>: `. Partial trailing line is buffered and flushed on stream end.
pub struct LineBuffer {
    prefix: Option<String>,
    partial: Vec<u8>,
}

impl LineBuffer {
    pub fn new(prefix: Option<String>) -> Self {
        Self { prefix, partial: Vec::new() }
    }

    /// Emit zero-or-more byte buffers ready for stdout.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut cursor = 0;
        for (i, b) in bytes.iter().enumerate() {
            if *b == b'\n' {
                self.partial.extend_from_slice(&bytes[cursor..=i]);
                out.push(self.take_line());
                cursor = i + 1;
            }
        }
        if cursor < bytes.len() {
            self.partial.extend_from_slice(&bytes[cursor..]);
        }
        out
    }

    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.partial.is_empty() {
            return None;
        }
        Some(self.take_line())
    }

    fn take_line(&mut self) -> Vec<u8> {
        let line = std::mem::take(&mut self.partial);
        match &self.prefix {
            None => line,
            Some(p) => {
                let mut prefixed = Vec::with_capacity(p.len() + 2 + line.len());
                prefixed.extend_from_slice(p.as_bytes());
                prefixed.extend_from_slice(b": ");
                prefixed.extend_from_slice(&line);
                prefixed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linebuf_no_prefix_passthrough() {
        let mut b = LineBuffer::new(None);
        let out = b.feed(b"hello\nworld\n");
        assert_eq!(out, vec![b"hello\n".to_vec(), b"world\n".to_vec()]);
        assert!(b.flush().is_none());
    }

    #[test]
    fn linebuf_prefix_per_complete_line() {
        let mut b = LineBuffer::new(Some("x".into()));
        let out = b.feed(b"a\nb\n");
        assert_eq!(out, vec![b"x: a\n".to_vec(), b"x: b\n".to_vec()]);
    }

    #[test]
    fn linebuf_partial_buffered_until_next_newline() {
        let mut b = LineBuffer::new(Some("x".into()));
        let first = b.feed(b"hel");
        assert!(first.is_empty());
        let second = b.feed(b"lo\n");
        assert_eq!(second, vec![b"x: hello\n".to_vec()]);
    }

    #[test]
    fn linebuf_flush_emits_unterminated_tail() {
        let mut b = LineBuffer::new(Some("x".into()));
        let _ = b.feed(b"trail");
        let last = b.flush().expect("partial line should flush");
        assert_eq!(last, b"x: trail".to_vec());
    }
}
```

- [ ] **Step 5: Wire dispatch in `main.rs`**

Add `mod logs;` and:

```rust
Command::Logs { sessions, strip, prefix, follow, tail } => {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    logs::run(&endpoint, sessions, *strip, *prefix, *follow, *tail).await
}
```

- [ ] **Step 6: Run tests**

```bash
cargo nextest run -p cairn --test non_interactive logs_
cargo nextest run -p cairn logs::tests
```

Expected: all PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/cairn-client/Cargo.toml crates/cairn-client/src/logs.rs crates/cairn-client/src/main.rs crates/cairn-client/tests/non_interactive.rs
git commit -m "feat(cairn-client): cairn logs (multi-session, --strip, --prefix, --tail, --follow)"
```

---

## Task 15: Remove the catch-all bail in `main.rs`

With all eleven commands wired, the `_ => anyhow::bail!("this command is not implemented yet…")` arm at `main.rs:64-66` is dead.

**Files:**
- Modify: `crates/cairn-client/src/main.rs`

- [ ] **Step 1: Delete the catch-all arm**

Open `main.rs`. Locate the `match &cli.command` block. The current `_ => anyhow::bail!(...)` arm should be removed; clap's `enum Command` is exhaustive, and every variant is now handled. After removal the `match` reads exhaustively (verify with `cargo check`).

- [ ] **Step 2: Build + run the whole test suite to confirm exhaustiveness and no regressions**

```bash
cargo nextest run -p cairn
```

Expected: all tests pass; no `match`-non-exhaustive errors.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/main.rs
git commit -m "chore(cairn-client): drop the catch-all 'not implemented' arm; every subcommand is wired"
```

---

## Self-review summary

After writing this plan, I checked it against the spec:

**Spec coverage:**
- Connection refactor — Task 1 ✓
- Shared resolver (`targets.rs`) — Task 2 ✓
- `cli.rs` edit (`--no-wait` conflicts_with) — Task 3 ✓
- `whoami`, `version` — Task 5 ✓
- `list` — Task 6 ✓
- `inspect` — Task 7 ✓
- `rename` — Task 8 ✓
- `restart` — Task 9 ✓
- `send` — Task 10 ✓
- `kick` — Task 11 ✓
- `kill` (incl. orthogonal `--no-wait`/`--timeout`) — Task 12 ✓
- `wait` (incl. exit 124) — Task 13 ✓
- `logs` (multi-session, prefix, strip, tail, follow) — Task 14 ✓
- Catch-all removal — Task 15 ✓
- Integration-test harness — Task 4 ✓
- Per-command unit tests embedded in their respective tasks ✓

**Placeholder scan:** no `TBD`/`TODO`/`fill in`/`appropriate error handling` strings in any task body. Every "TODO" reference is a citation of the existing `cli.rs` TODOs that are deferred *by design*.

**Type consistency:** `ResolvedTarget`/`ResolvedMany` shape used by Task 2 matches every consumer in Tasks 7-14. `Signal`/`SignalName` (CLI types) and `WireSignal`/`WireSignalName` (protocol types) are mapped exactly once, in `kill.rs::into_wire_signal`. The `LineBuffer` API surfaces (`new`, `feed`, `flush`) are consistent between definition and tests.

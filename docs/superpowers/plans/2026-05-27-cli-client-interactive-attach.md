# CLI Client — Interactive Attach Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the interactive half of the `cairn` CLI client — `cairn attach`, `cairn exec`, `cairn run` — over a Unix Domain Socket, with raw-terminal I/O, configurable detach keys, SIGWINCH-driven resize, the detach-camp signal model, and auto-reconnect-with-snapshot.

**Architecture:** A dedicated `std::thread` does blocking reads on `STDIN_FILENO` and ships bytes over a channel to an async driver; everything else (SIGWINCH, termination signals, the wRPC `attach` bidi stream, the reconnect loop) is tokio. Raw mode via `nix` termios + a `Drop` guard. The driver owns a `tokio::select!` loop and a reconnect-with-backoff loop. `exec`/`run` build a `SessionSpec`, create the session, then hand the new id to the same driver.

**Tech Stack:** Rust, tokio (multi-thread), `wit-bindgen-wrpc` generated client, `wrpc_transport::unix::Client`, `nix` (termios + winsize ioctl), `futures::channel::mpsc`, `cargo-nextest`.

Spec: `docs/superpowers/specs/2026-05-27-cli-client-interactive-attach-design.md`.

**Prerequisite:** the daemon-prereqs plan (`docs/superpowers/plans/2026-05-27-cli-client-daemon-prereqs.md`) must be completed first — this plan depends on `cairn_protocol::error_codes::{CLIENT_KICKED, CLIENT_LAGGED}` and the daemon emitting them.

**Confirmed generated client signatures** (from `cargo expand -p cairn-protocol`, so later tasks can rely on them):

```rust
// cairn_protocol::client::cairn::daemon::sessions
pub fn attach<'a, C: Invoke>(
    wrpc: &'a C, cx: C::Context, id: &'a str, init: &'a AttachInit,
    events: Pin<Box<dyn Stream<Item = Vec<ClientEvent>> + Send>>,
) -> impl Future<Output = anyhow::Result<(
    Pin<Box<dyn Stream<Item = Vec<ServerEvent>> + Send>>,
    Option<impl Future<Output = anyhow::Result<()>> + Send + 'static>,  // io pump, must be driven
)>>;

pub fn create<'a, C: Invoke>(wrpc: &'a C, cx: C::Context, spec: &'a SessionSpec)
    -> impl Future<Output = anyhow::Result<Result<SessionInfo, Error>>>;   // unary, no io future

pub fn list_all<'a, C: Invoke>(wrpc: &'a C, cx: C::Context)
    -> impl Future<Output = anyhow::Result<Vec<SessionInfo>>>;             // unary, no io future
```

For the `unix` transport `C::Context = ()`. Types live at `cairn_protocol::cairn::daemon::types::{AttachInit, ClientEvent, ServerEvent, SessionSpec, SessionInfo}`; `ClientEvent::{Input(Bytes), Resize((u16,u16)), Detach}`; `ServerEvent::{Snapshot(Bytes), Output(Bytes), Exited(ExitStatus), Error(Error)}`.

**No-panic rule:** per the project's error-handling convention, production code in `crates/cairn-client/src` must not use `unwrap`/`expect`/`panic!`/`unreachable!`/`todo!`. Return errors. Test code is exempt.

---

### Task 1: Add client dependencies

**Files:**
- Modify: `crates/cairn-client/Cargo.toml`

- [ ] **Step 1: Add runtime + dev dependencies**

Replace the `[dependencies]` block in `crates/cairn-client/Cargo.toml` (keep the `[package]` block above it untouched) and add a `[dev-dependencies]` block:

```toml
[dependencies]
clap.workspace = true
clap_complete = { version = "4.6" }
anyhow = { workspace = true, features = ["backtrace"] }
strum = { version = "0.28", features = ["derive"] }
humantime = { version = "2.3" }
url = { version = "2.5" }
tracing.workspace = true
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
bytes.workspace = true
futures.workspace = true
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "signal", "sync", "time"] }
nix = { version = "0.29", features = ["term", "ioctl"] }
cairn-protocol = { path = "../cairn-protocol" }
wrpc-transport.workspace = true

[dev-dependencies]
cairn-daemon = { path = "../cairn-daemon" }
tempfile = "3"
libc = "0.2"
tokio-util = "0.7"
tokio = { workspace = true, features = ["rt-multi-thread", "macros", "time", "net"] }
```

- [ ] **Step 2: Verify the existing stub still builds**

Run: `cargo build -p cairn`
Expected: builds clean (the stub `main.rs`/`cli.rs` are unaffected; new deps are unused for now).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/Cargo.toml
git commit -m "build(cairn-client): add deps for interactive attach (tokio, nix, protocol client)"
```

---

### Task 2: Connection layer (`connect.rs`)

**Files:**
- Create: `crates/cairn-client/src/connect.rs`
- Modify: `crates/cairn-client/src/main.rs` (add `mod connect;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-client/src/connect.rs` with the test module first (implementation in Step 3):

```rust
//! Daemon endpoint resolution. v0 supports only the unix-socket transport.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// The wRPC client type for the local unix-socket transport. Cheap to clone
/// (holds only the socket path); each invocation opens a fresh connection.
pub type Client = wrpc_transport::unix::Client<PathBuf>;

/// A resolved daemon endpoint.
pub struct Endpoint {
    path: PathBuf,
}

impl Endpoint {
    /// Resolve from `--daemon` / `CAIRN_DAEMON` (already read by clap) or the
    /// platform default socket.
    pub fn resolve(daemon: Option<&str>) -> Result<Self> {
        match daemon {
            None => Ok(Self { path: default_socket() }),
            Some(s) => Self::from_uri(s),
        }
    }

    fn from_uri(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix("unix://") {
            if rest.is_empty() {
                bail!("`--daemon unix://` has no socket path");
            }
            return Ok(Self { path: PathBuf::from(rest) });
        }
        if s.starts_with('/') {
            return Ok(Self { path: PathBuf::from(s) });
        }
        if s.starts_with("ws://") || s.starts_with("wss://") {
            bail!("remote transports (WebTransport) are not yet supported; v0 is unix-socket only");
        }
        bail!("unrecognized --daemon endpoint {s:?} (expected `unix:///path/to/cairn.sock`)");
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn client(&self) -> Client {
        wrpc_transport::unix::Client::from(self.path.clone())
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

    #[test]
    fn default_socket_ends_with_cairn_sock() {
        let ep = Endpoint::resolve(None).unwrap();
        assert!(ep.path().ends_with("cairn/cairn.sock"), "got {:?}", ep.path());
    }

    #[test]
    fn unix_uri_yields_its_path() {
        let ep = Endpoint::resolve(Some("unix:///run/cairn/x.sock")).unwrap();
        assert_eq!(ep.path(), Path::new("/run/cairn/x.sock"));
    }

    #[test]
    fn bare_absolute_path_is_accepted() {
        let ep = Endpoint::resolve(Some("/tmp/y.sock")).unwrap();
        assert_eq!(ep.path(), Path::new("/tmp/y.sock"));
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
}
```

Add to the top of `crates/cairn-client/src/main.rs` (after `mod cli;`):

```rust
mod connect;
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo nextest run -p cairn connect`
Expected: PASS (this module is self-contained; tests pass immediately). If any fail, fix `from_uri`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/connect.rs crates/cairn-client/src/main.rs
git commit -m "feat(cairn-client): resolve daemon endpoint (unix-socket only in v0)"
```

---

### Task 3: Detach-key parsing (`detach.rs`)

**Files:**
- Create: `crates/cairn-client/src/detach.rs`
- Modify: `crates/cairn-client/src/main.rs` (add `mod detach;`)

Parse the docker-style `--detach-keys` spec into a sequence of keys, each carrying both its raw control byte and its canonical Kitty CSI-u encoding (for matching in Task 4).

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-client/src/detach.rs`:

```rust
//! Detach-key sequence parsing and matching.
//!
//! `--detach-keys` is a comma-separated list of `ctrl-<char>` or single-char
//! tokens (docker-style), e.g. `ctrl-q,ctrl-q` or `ctrl-a,d`. Each key is
//! recognized in TWO encodings: the raw control byte, and the Kitty CSI-u
//! `\x1b[<code>;<mods>u` form — because an inferior program inside the session
//! can flip the outer terminal into Kitty mode via passthrough, after which
//! keystrokes arrive as CSI-u rather than raw bytes.

/// One key in the detach sequence, with both byte encodings precomputed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachKey {
    raw: u8,
    csiu: Vec<u8>,
}

/// A parsed detach-key sequence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachKeys {
    keys: Vec<DetachKey>,
}

impl DetachKeys {
    /// Parse a comma-separated detach-key spec.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut keys = Vec::new();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err(format!("empty key in detach sequence {spec:?}"));
            }
            keys.push(DetachKey::parse(token)?);
        }
        if keys.is_empty() {
            return Err("detach sequence is empty".to_string());
        }
        Ok(Self { keys })
    }

    /// Parse the spec, defaulting to `ctrl-q,ctrl-q` when none is given.
    pub fn parse_or_default(spec: Option<&str>) -> Result<Self, String> {
        Self::parse(spec.unwrap_or("ctrl-q,ctrl-q"))
    }

    pub(crate) fn keys(&self) -> &[DetachKey] {
        &self.keys
    }
}

impl DetachKey {
    pub(crate) fn raw(&self) -> u8 {
        self.raw
    }
    pub(crate) fn csiu(&self) -> &[u8] {
        &self.csiu
    }

    fn parse(token: &str) -> Result<Self, String> {
        if let Some(rest) = token.strip_prefix("ctrl-") {
            let c = single_ascii(rest, token)?;
            let lower = c.to_ascii_lowercase();
            let code = lower as u32;
            Ok(DetachKey {
                raw: (lower as u8) & 0x1f,
                csiu: format!("\x1b[{code};5u").into_bytes(), // mods 5 = ctrl
            })
        } else {
            let c = single_ascii(token, token)?;
            let code = c as u32;
            Ok(DetachKey {
                raw: c as u8,
                csiu: format!("\x1b[{code}u").into_bytes(), // unmodified
            })
        }
    }
}

fn single_ascii(s: &str, token: &str) -> Result<char, String> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii() => Ok(c),
        (Some(_), None) => Err(format!("detach key {token:?} must be ASCII")),
        _ => Err(format!("detach key {token:?} must be a single char or ctrl-<char>")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_ctrl_q_sequence() {
        let keys = DetachKeys::parse("ctrl-q,ctrl-q").unwrap();
        assert_eq!(keys.keys().len(), 2);
        assert_eq!(keys.keys()[0].raw(), 0x11); // 'q' & 0x1f
        assert_eq!(keys.keys()[0].csiu(), b"\x1b[113;5u"); // 'q' = 113
    }

    #[test]
    fn parses_mixed_ctrl_and_literal() {
        let keys = DetachKeys::parse("ctrl-a,d").unwrap();
        assert_eq!(keys.keys()[0].raw(), 0x01);
        assert_eq!(keys.keys()[0].csiu(), b"\x1b[97;5u"); // 'a' = 97
        assert_eq!(keys.keys()[1].raw(), b'd');
        assert_eq!(keys.keys()[1].csiu(), b"\x1b[100u"); // 'd' = 100, unmodified
    }

    #[test]
    fn ctrl_is_case_insensitive() {
        let keys = DetachKeys::parse("ctrl-Q").unwrap();
        assert_eq!(keys.keys()[0].raw(), 0x11);
        assert_eq!(keys.keys()[0].csiu(), b"\x1b[113;5u");
    }

    #[test]
    fn rejects_empty_and_malformed_tokens() {
        assert!(DetachKeys::parse("ctrl-q,,ctrl-q").is_err()); // empty token
        assert!(DetachKeys::parse("ctrl-").is_err()); // no char after ctrl-
        assert!(DetachKeys::parse("ab").is_err()); // two-char literal
        assert!(DetachKeys::parse("").is_err()); // empty spec
    }
}
```

Add to `crates/cairn-client/src/main.rs`:

```rust
mod detach;
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo nextest run -p cairn detach::tests`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/detach.rs crates/cairn-client/src/main.rs
git commit -m "feat(cairn-client): parse docker-style detach-key sequences (raw + CSI-u)"
```

---

### Task 4: Detach-key matcher (`detach.rs`)

**Files:**
- Modify: `crates/cairn-client/src/detach.rs` (add `Matcher` + tests)

A streaming matcher that detects the detach sequence in either encoding, withholding partial matches and flushing them as input on mismatch.

- [ ] **Step 1: Write the failing tests**

Append to `crates/cairn-client/src/detach.rs` (above the existing `#[cfg(test)]` module, add the `Matcher`; then add the tests inside the test module):

Add this implementation after the `DetachKey` impl:

```rust
enum Step {
    Full,
    Partial,
    NotPrefix,
}

/// Streaming matcher: feed input bytes, get back the bytes to forward to the
/// session and whether the detach sequence completed.
pub struct Matcher {
    keys: Vec<DetachKey>,
    withheld: Vec<u8>,
}

impl Matcher {
    pub fn new(keys: DetachKeys) -> Self {
        Self { keys: keys.keys, withheld: Vec::new() }
    }

    /// Feed `input`; append forwardable bytes to `out`. Returns true when the
    /// detach sequence has completed (bytes after the sequence in this call are
    /// dropped — detach ends the stream anyway).
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> bool {
        for &b in input {
            self.withheld.push(b);
            loop {
                match self.try_match() {
                    Step::Full => {
                        self.withheld.clear();
                        return true;
                    }
                    Step::Partial => break,
                    Step::NotPrefix => {
                        // The front byte can't begin a match: release it as input.
                        out.push(self.withheld.remove(0));
                        if self.withheld.is_empty() {
                            break;
                        }
                    }
                }
            }
        }
        false
    }

    /// Match the key sequence against the front of `withheld`. At each position
    /// the next byte selects the encoding: `0x1b` => Kitty CSI-u, else raw byte.
    fn try_match(&self) -> Step {
        let buf = &self.withheld;
        let mut j = 0;
        for key in &self.keys {
            if j >= buf.len() {
                return Step::Partial;
            }
            if buf[j] == 0x1b {
                let need = key.csiu();
                let avail = &buf[j..];
                let n = avail.len().min(need.len());
                if avail[..n] != need[..n] {
                    return Step::NotPrefix;
                }
                if n < need.len() {
                    return Step::Partial;
                }
                j += need.len();
            } else {
                if buf[j] != key.raw() {
                    return Step::NotPrefix;
                }
                j += 1;
            }
        }
        Step::Full
    }
}
```

Note: `Matcher::new` moves `keys.keys`; since `DetachKeys.keys` is a private field accessed within the same module, this compiles. (`keys` field is `Vec<DetachKey>`.)

Add these tests inside the existing `#[cfg(test)] mod tests`:

```rust
    fn feed_all(spec: &str, input: &[u8]) -> (Vec<u8>, bool) {
        let mut m = Matcher::new(DetachKeys::parse(spec).unwrap());
        let mut out = Vec::new();
        let detached = m.feed(input, &mut out);
        (out, detached)
    }

    #[test]
    fn raw_sequence_detaches_and_forwards_nothing() {
        let (out, detached) = feed_all("ctrl-q,ctrl-q", &[0x11, 0x11]);
        assert!(detached);
        assert!(out.is_empty());
    }

    #[test]
    fn partial_then_mismatch_flushes_withheld_bytes() {
        let mut m = Matcher::new(DetachKeys::parse("ctrl-q,ctrl-q").unwrap());
        let mut out = Vec::new();
        // First ctrl-q is withheld (could start the sequence).
        assert!(!m.feed(&[0x11], &mut out));
        assert!(out.is_empty());
        // A non-ctrl-q breaks it: both bytes are released as input.
        assert!(!m.feed(&[b'x'], &mut out));
        assert_eq!(out, vec![0x11, b'x']);
    }

    #[test]
    fn csiu_sequence_detaches() {
        let (out, detached) = feed_all("ctrl-q,ctrl-q", b"\x1b[113;5u\x1b[113;5u");
        assert!(detached, "Kitty CSI-u encoding of ctrl-q,ctrl-q should detach");
        assert!(out.is_empty());
    }

    #[test]
    fn mixed_csiu_then_raw_detaches() {
        // ctrl-a as CSI-u, then literal `d` as a raw byte.
        let (out, detached) = feed_all("ctrl-a,d", b"\x1b[97;5ud");
        assert!(detached);
        assert!(out.is_empty());
    }

    #[test]
    fn non_matching_escape_sequence_is_forwarded() {
        // An up-arrow (\x1b[A) shares the \x1b[ prefix with CSI-u but isn't a
        // detach key — it must be forwarded intact.
        let (out, detached) = feed_all("ctrl-q,ctrl-q", b"\x1b[A");
        assert!(!detached);
        assert_eq!(out, b"\x1b[A");
    }

    #[test]
    fn lone_ctrl_q_inside_a_run_does_not_detach() {
        let (out, detached) = feed_all("ctrl-q,ctrl-q", b"a\x11b");
        assert!(!detached);
        assert_eq!(out, b"a\x11b");
    }
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo nextest run -p cairn detach::tests`
Expected: PASS. If `mixed_csiu_then_raw_detaches` or `non_matching_escape_sequence_is_forwarded` fail, re-check `try_match`'s encoding branch.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/detach.rs
git commit -m "feat(cairn-client): streaming detach matcher (raw + CSI-u, partial flush)"
```

---

### Task 5: Terminal control (`terminal.rs`)

**Files:**
- Create: `crates/cairn-client/src/terminal.rs`
- Modify: `crates/cairn-client/src/main.rs` (add `mod terminal;`)

Raw-mode guard, window-size query, screen clear, and a stdout writer. Hard to unit-test (needs a real TTY); validated by the integration harness (Task 11). Verify via build here.

- [ ] **Step 1: Write the module**

Create `crates/cairn-client/src/terminal.rs`:

```rust
//! Local-terminal control: raw mode (with RAII restore), window size, output.

use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd};

use nix::sys::termios::{self, SetArg, SpecialCharacterIndices, Termios};

/// RAII guard that puts stdin into raw mode and restores it on drop. If stdin
/// is not a TTY, this is a no-op guard (output still streams; no raw munging).
pub struct RawGuard {
    original: Option<Termios>,
}

impl RawGuard {
    pub fn engage() -> io::Result<Self> {
        let stdin = io::stdin();
        // tcgetattr fails (ENOTTY) when stdin isn't a terminal — degrade.
        let original = match termios::tcgetattr(stdin.as_fd()) {
            Ok(t) => t,
            Err(_) => return Ok(Self { original: None }),
        };
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw); // clears ISIG/IEXTEN/ICANON/ECHO; sets VMIN=1/VTIME=0
        // Be explicit about the one-byte-read discipline regardless of libc.
        raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw)
            .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
        Ok(Self { original: Some(original) })
    }

    pub fn is_raw(&self) -> bool {
        self.original.is_some()
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if let Some(orig) = &self.original {
            let stdin = io::stdin();
            let _ = termios::tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, orig);
            // RIS: reset the outer terminal out of alt-screen / mouse / paste modes
            // the inferior may have set.
            let mut out = io::stdout();
            let _ = out.write_all(b"\x1bc");
            let _ = out.flush();
        }
    }
}

nix::ioctl_read_bad!(tiocgwinsz, nix::libc::TIOCGWINSZ, nix::libc::winsize);

/// Current terminal size as `(cols, rows)`, or `None` when stdout isn't a TTY.
pub fn window_size() -> Option<(u16, u16)> {
    let mut ws: nix::libc::winsize = unsafe { std::mem::zeroed() };
    let fd = io::stdout().as_raw_fd();
    // SAFETY: `ws` is a valid, writable winsize for the duration of the call.
    let rc = unsafe { tiocgwinsz(fd, &mut ws) };
    match rc {
        Ok(_) if ws.ws_col > 0 => Some((ws.ws_col, ws.ws_row)),
        _ => None,
    }
}

/// Clear the screen and home the cursor — a clean canvas before snapshot replay.
pub fn clear_screen(out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"\x1b[2J\x1b[H")?;
    out.flush()
}

/// Write session output to stdout. Blocking write; fine for a TTY (the daemon's
/// lag-kick handles a slow consumer). Errors are swallowed — a broken stdout
/// surfaces as the stream/transport ending elsewhere.
pub fn write_stdout(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}
```

Add to `crates/cairn-client/src/main.rs`:

```rust
mod terminal;
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p cairn`
Expected: builds (warnings about unused functions are fine until wired in later tasks). If `nix` reports a missing item, confirm the `["term", "ioctl"]` features from Task 1 are present.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/terminal.rs crates/cairn-client/src/main.rs
git commit -m "feat(cairn-client): raw-mode guard, window-size query, stdout writer"
```

---

### Task 6: Signal streams (`signals.rs`)

**Files:**
- Create: `crates/cairn-client/src/signals.rs`
- Modify: `crates/cairn-client/src/main.rs` (add `mod signals;`)

The detach-camp model: SIGWINCH drives resize; the termination set causes a clean detach. (SIGPIPE is already set to `SIG_IGN` by the Rust std runtime, so we don't touch it. SIGTSTP/CONT keep default disposition — deferred.)

- [ ] **Step 1: Write the module**

Create `crates/cairn-client/src/signals.rs`:

```rust
//! Signal handling for `attach`. Per the detach-camp model (matching
//! zmx/dtach/abduco/shpool): only SIGWINCH is forwarded (as a resize); every
//! other client-received signal triggers a clean detach. Nothing is forwarded
//! to the child.

use std::io;

use tokio::signal::unix::{Signal, SignalKind, signal};

/// The set of signals that, when delivered to the client, mean "detach". We
/// trap them (rather than letting them default-kill the process) so the
/// terminal is restored cleanly before exit.
pub struct Termination {
    int: Signal,
    term: Signal,
    quit: Signal,
    hup: Signal,
    usr1: Signal,
    usr2: Signal,
}

impl Termination {
    pub fn install() -> io::Result<Self> {
        Ok(Self {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            quit: signal(SignalKind::quit())?,
            hup: signal(SignalKind::hangup())?,
            usr1: signal(SignalKind::user_defined1())?,
            usr2: signal(SignalKind::user_defined2())?,
        })
    }

    /// Resolves when any termination signal is received. Cancel-safe.
    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.int.recv() => {}
            _ = self.term.recv() => {}
            _ = self.quit.recv() => {}
            _ = self.hup.recv() => {}
            _ = self.usr1.recv() => {}
            _ = self.usr2.recv() => {}
        }
    }
}

/// A SIGWINCH stream for resize handling.
pub fn window_changes() -> io::Result<Signal> {
    signal(SignalKind::window_change())
}
```

Add to `crates/cairn-client/src/main.rs`:

```rust
mod signals;
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p cairn`
Expected: builds (unused-code warnings ok).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/signals.rs crates/cairn-client/src/main.rs
git commit -m "feat(cairn-client): signal streams (SIGWINCH resize, termination=detach)"
```

---

### Task 7: The attach driver (`attach.rs`)

**Files:**
- Create: `crates/cairn-client/src/attach.rs`
- Modify: `crates/cairn-client/src/main.rs` (add `mod attach;`)

The heart: the stdin reader thread, the reconnect-with-backoff loop, and the `tokio::select!` I/O loop. Validated by the integration harness (Task 11); verify build here.

- [ ] **Step 1: Write the module**

Create `crates/cairn-client/src/attach.rs`:

```rust
//! The interactive attach driver: bridges the local terminal to a session's
//! `attach` bidi stream, with auto-reconnect-and-repaint on transient loss.

use std::future::pending;
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use futures::channel::mpsc;

use cairn_protocol::cairn::daemon::types::{AttachInit, ClientEvent, ServerEvent};
use cairn_protocol::client::cairn::daemon::sessions;
use cairn_protocol::error_codes;

use crate::connect::Endpoint;
use crate::detach::{DetachKeys, Matcher};
use crate::signals::{Termination, window_changes};
use crate::terminal::{self, RawGuard};

pub struct AttachOptions {
    pub no_stdin: bool,
    pub detach_keys: DetachKeys,
}

enum Outcome {
    Detached,
    Exited { code: Option<i32>, signal: Option<u8> },
    Fatal(String),
    Reconnect,
}

/// Attach to `id` and run until detach, child-exit, fatal error, or giving up
/// on reconnect. Returns the process exit code.
pub async fn run(endpoint: &Endpoint, id: &str, opts: AttachOptions) -> Result<i32> {
    let client = endpoint.client();
    let guard = RawGuard::engage()?;
    if !std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        eprintln!("cairn: stdout is not a terminal; output will include raw escape sequences");
    }
    let _ = guard.is_raw(); // held for the whole attach; restored on drop

    let mut stdin_rx = if opts.no_stdin { None } else { Some(spawn_stdin_reader()?) };
    let mut matcher = Matcher::new(opts.detach_keys.clone());
    let mut term = Termination::install()?;
    let mut winch = window_changes()?;

    let budget = reconnect_budget();
    let mut backoff = Duration::from_millis(100);
    let mut deadline: Option<Instant> = None;

    loop {
        let (cols, rows) = terminal::window_size().unwrap_or((80, 24));
        let init = AttachInit { cols, rows, no_stdin: opts.no_stdin };
        let (mut events_tx, events_rx) = mpsc::channel::<Vec<ClientEvent>>(64);
        let events: std::pin::Pin<Box<dyn futures::Stream<Item = Vec<ClientEvent>> + Send>> =
            Box::pin(events_rx);

        let outcome = match sessions::attach(&client, (), id, &init, events).await {
            Err(_e) => Outcome::Reconnect, // couldn't establish the stream
            Ok((mut server, io)) => {
                deadline = None; // connected: reset the give-up clock
                backoff = Duration::from_millis(100);
                {
                    let mut out = std::io::stdout().lock();
                    let _ = terminal::clear_screen(&mut out);
                }
                // The io future pumps the transport (both directions). Drive it
                // concurrently with the select loop; if it ends, the connection
                // is gone.
                let io_fut = async move {
                    if let Some(f) = io {
                        let _ = f.await;
                    } else {
                        pending::<()>().await
                    }
                };
                tokio::pin!(io_fut);

                loop {
                    tokio::select! {
                        _ = &mut io_fut => break Outcome::Reconnect,

                        maybe = server.next() => match maybe {
                            Some(batch) => {
                                if let Some(o) = handle_server_batch(batch) {
                                    break o;
                                }
                            }
                            None => break Outcome::Reconnect,
                        },

                        _ = term.recv() => {
                            let _ = events_tx.send(vec![ClientEvent::Detach]).await;
                            break Outcome::Detached;
                        }

                        _ = winch.recv() => {
                            if let Some((c, r)) = terminal::window_size() {
                                let _ = events_tx.try_send(vec![ClientEvent::Resize((c, r))]);
                            }
                        }

                        chunk = recv_stdin(&mut stdin_rx) => match chunk {
                            Some(bytes) => {
                                let mut forward = Vec::new();
                                let detached = matcher.feed(&bytes, &mut forward);
                                if !forward.is_empty()
                                    && events_tx
                                        .send(vec![ClientEvent::Input(Bytes::from(forward))])
                                        .await
                                        .is_err()
                                {
                                    break Outcome::Reconnect;
                                }
                                if detached {
                                    let _ = events_tx.send(vec![ClientEvent::Detach]).await;
                                    break Outcome::Detached;
                                }
                            }
                            None => {
                                // stdin EOF: stop forwarding, keep streaming output.
                                stdin_rx = None;
                            }
                        }
                    }
                }
            }
        };

        match outcome {
            Outcome::Detached => return Ok(0),
            Outcome::Exited { code, signal } => return Ok(exit_code(code, signal)),
            Outcome::Fatal(msg) => {
                eprintln!("cairn: {msg}");
                return Ok(1);
            }
            Outcome::Reconnect => {} // fall through to backoff
        }

        if !endpoint.path().exists() {
            eprintln!(
                "cairn: connection lost (daemon socket {} is gone)",
                endpoint.path().display()
            );
            return Ok(1);
        }
        let now = Instant::now();
        let dl = *deadline.get_or_insert(now + budget);
        if budget != Duration::ZERO && now >= dl {
            eprintln!("cairn: connection lost (gave up reconnecting after {:?})", budget);
            return Ok(1);
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(2));
    }
}

/// Apply one batch of server events to the terminal; return an `Outcome` if the
/// batch is terminal (exit / fatal error / recoverable lag).
fn handle_server_batch(batch: Vec<ServerEvent>) -> Option<Outcome> {
    for ev in batch {
        match ev {
            ServerEvent::Snapshot(b) | ServerEvent::Output(b) => terminal::write_stdout(&b),
            ServerEvent::Exited(st) => {
                return Some(Outcome::Exited { code: st.code, signal: st.signal });
            }
            ServerEvent::Error(e) => {
                if e.code == error_codes::CLIENT_LAGGED {
                    return Some(Outcome::Reconnect);
                }
                return Some(Outcome::Fatal(format!("{}: {}", e.code, e.message)));
            }
        }
    }
    None
}

/// Await the next stdin chunk, or pend forever when there's no stdin source.
async fn recv_stdin(rx: &mut Option<tokio::sync::mpsc::Receiver<Bytes>>) -> Option<Bytes> {
    match rx {
        Some(r) => r.recv().await, // None on channel close (EOF / reader gone)
        None => pending().await,
    }
}

/// Dedicated blocking-read thread on STDIN_FILENO. Never sets O_NONBLOCK on fd 0
/// (that flag is shared with the parent shell). Leaked at process exit.
fn spawn_stdin_reader() -> std::io::Result<tokio::sync::mpsc::Receiver<Bytes>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(64);
    std::thread::Builder::new()
        .name("cairn-stdin".to_string())
        .spawn(move || {
            use std::io::Read;
            let mut stdin = std::io::stdin();
            let mut buf = [0u8; 4096];
            loop {
                match stdin.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if tx.blocking_send(Bytes::copy_from_slice(&buf[..n])).is_err() {
                            break; // driver gone
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        })?;
    Ok(rx)
}

fn exit_code(code: Option<i32>, signal: Option<u8>) -> i32 {
    code.unwrap_or_else(|| signal.map(|s| 128 + s as i32).unwrap_or(1))
}

/// Reconnect give-up budget from `CAIRN_RECONNECT_TIMEOUT` (humantime; `0`/`off`
/// = retry forever; default 30s).
fn reconnect_budget() -> Duration {
    match std::env::var("CAIRN_RECONNECT_TIMEOUT") {
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("off") => Duration::ZERO,
        Ok(v) => humantime::parse_duration(&v).unwrap_or(Duration::from_secs(30)),
        Err(_) => Duration::from_secs(30),
    }
}
```

Add to `crates/cairn-client/src/main.rs`:

```rust
mod attach;
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p cairn`
Expected: builds. Likely fix-ups during execution: the `io` future's lifetime when moved into `io_fut` (if the borrow checker objects, the cause is `init`/`client` lifetimes — keep `init` and `client` alive for the whole iteration, which this structure does). If `events_tx.send` needs `SinkExt`, it's imported. Confirm `ServerEvent::Exited`'s field names are `code`/`signal` (they are — wire `exit-status`).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/attach.rs crates/cairn-client/src/main.rs
git commit -m "feat(cairn-client): interactive attach driver (select loop + reconnect)"
```

---

### Task 8: env-file / `-e` merge (`exec.rs`)

**Files:**
- Create: `crates/cairn-client/src/exec.rs` (the `merge_env` fn + tests; the rest in Task 9)
- Modify: `crates/cairn-client/src/main.rs` (add `mod exec;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/cairn-client/src/exec.rs`:

```rust
//! `cairn exec` / `cairn run`: build a SessionSpec, create the session, then
//! (unless detached) attach to it.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Merge `--env-file` files (lowest precedence, applied in order) with `-e`
/// args (highest; `KEY=VALUE` sets, bare `KEY` copies from the client env).
/// Returns the explicit env list for the spec; the daemon overlays it on the
/// inherited env (explicit wins).
pub fn merge_env(env_files: &[PathBuf], env_args: &[String]) -> Result<Vec<(String, String)>> {
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for path in env_files {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading --env-file {}", path.display()))?;
        for (i, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .with_context(|| format!("{}:{}: expected KEY=VALUE", path.display(), i + 1))?;
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    for item in env_args {
        match item.split_once('=') {
            Some((k, v)) => {
                map.insert(k.to_string(), v.to_string());
            }
            None => {
                // bare KEY: pass through from the client env if it's set (docker parity).
                if let Some(v) = std::env::var_os(item) {
                    map.insert(item.clone(), v.to_string_lossy().into_owned());
                }
            }
        }
    }
    Ok(map.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_env_file_skipping_comments_and_blanks() {
        let f = write_tmp("# comment\n\nFOO=bar\nBAZ=qux\n");
        let env = merge_env(&[f.path().to_path_buf()], &[]).unwrap();
        assert!(env.contains(&("FOO".to_string(), "bar".to_string())));
        assert!(env.contains(&("BAZ".to_string(), "qux".to_string())));
    }

    #[test]
    fn dash_e_overrides_env_file() {
        let f = write_tmp("FOO=from_file\n");
        let env = merge_env(&[f.path().to_path_buf()], &["FOO=from_flag".to_string()]).unwrap();
        assert!(env.contains(&("FOO".to_string(), "from_flag".to_string())));
        assert!(!env.contains(&("FOO".to_string(), "from_file".to_string())));
    }

    #[test]
    fn bare_key_copies_from_client_env_when_set() {
        // CARGO_PKG_NAME is set during `cargo test` -> "cairn".
        let env = merge_env(&[], &["CARGO_PKG_NAME".to_string()]).unwrap();
        assert!(env.iter().any(|(k, v)| k == "CARGO_PKG_NAME" && v == "cairn"));
    }

    #[test]
    fn bare_key_absent_is_skipped() {
        let env = merge_env(&[], &["CAIRN_DEFINITELY_UNSET_VAR_XYZ".to_string()]).unwrap();
        assert!(env.iter().all(|(k, _)| k != "CAIRN_DEFINITELY_UNSET_VAR_XYZ"));
    }

    #[test]
    fn malformed_env_file_line_errors() {
        let f = write_tmp("NOEQUALS\n");
        assert!(merge_env(&[f.path().to_path_buf()], &[]).is_err());
    }
}
```

Add to `crates/cairn-client/src/main.rs`:

```rust
mod exec;
```

Add `tempfile` to dev-deps if not already present (it was added in Task 1).

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo nextest run -p cairn exec::tests`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/exec.rs crates/cairn-client/src/main.rs
git commit -m "feat(cairn-client): merge --env-file and -e into the session env list"
```

---

### Task 9: exec/run session creation + dispatch (`exec.rs`)

**Files:**
- Modify: `crates/cairn-client/src/exec.rs` (add `run_exec`)

- [ ] **Step 1: Add the `run_exec` function**

Append to `crates/cairn-client/src/exec.rs` (after `merge_env`, before the test module):

```rust
use cairn_protocol::cairn::daemon::types::SessionSpec;
use cairn_protocol::client::cairn::daemon::sessions;

use crate::attach::{self, AttachOptions};
use crate::cli::{Cli, ExecArgs};
use crate::connect::Endpoint;
use crate::detach::DetachKeys;

/// Shared body for `exec` (default `-it` off) and `run` (default `-it` on).
pub async fn run_exec(
    cli: &Cli,
    args: &ExecArgs,
    default_tty: bool,
    default_interactive: bool,
) -> Result<i32> {
    let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
    let client = endpoint.client();

    let tty = args.tty_with_default(default_tty);
    let stdin = args.interactive_with_default(default_interactive);
    let workdir = match &args.workdir {
        Some(w) => Some(w.to_string_lossy().into_owned()),
        None => std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned()),
    };

    let spec = SessionSpec {
        name: args.name.clone(),
        command: args.command.clone(),
        env: merge_env(&args.env_file, &args.env)?,
        env_inherit: !args.no_inherit_env,
        workdir,
        tty,
        stdin,
        idle_timeout_secs: args.timeout.map(|d| d.as_secs()),
        scrollback_lines: 1000,
    };

    let info = match sessions::create(&client, (), &spec).await.context("create session")? {
        Ok(info) => info,
        Err(e) => {
            eprintln!("cairn: create failed: {}: {}", e.code, e.message);
            return Ok(1);
        }
    };

    let label = info.name.clone().unwrap_or_else(|| info.id.clone());
    if args.detach {
        println!("{label}");
        eprintln!("cairn: session created detached; attach with `cairn attach {label}`");
        return Ok(0);
    }

    let opts = AttachOptions {
        no_stdin: !stdin,
        detach_keys: DetachKeys::parse_or_default(args.detach_keys.as_deref())
            .map_err(|e| anyhow::anyhow!(e))?,
    };
    attach::run(&endpoint, &info.id, opts).await
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p cairn`
Expected: builds. (`ExecArgs::tty_with_default`/`interactive_with_default` exist at `cli.rs:436-458`; the `#[allow(dead_code)]` on that impl can be removed in Task 10 once used.)

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-client/src/exec.rs
git commit -m "feat(cairn-client): exec/run build a SessionSpec, create, then attach"
```

---

### Task 10: Remove `--sig-proxy`, fix env docs, wire up `main.rs` dispatch

**Files:**
- Modify: `crates/cairn-client/src/cli.rs` (remove `--sig-proxy` ×2; fix env-file doc; soften attach help; drop the now-unneeded `#[allow(dead_code)]`)
- Modify: `crates/cairn-client/src/main.rs` (runtime + tracing + dispatch)

- [ ] **Step 1: Remove `--sig-proxy` from the `Attach` subcommand**

In `crates/cairn-client/src/cli.rs`, delete the `sig_proxy` field and its doc-comment from the `Attach` variant (currently lines 97-114, the block starting `/// Forward signals received by the client...` through the `sig_proxy: bool,` with its `#[clap(...)]`). The `Attach` variant should end after the `detach_keys` field:

```rust
    Attach {
        #[command(flatten)]
        session: SessionTarget,
        /// Don't forward client stdin to the session. ...
        #[clap(long)]
        no_stdin: bool,
        /// Key sequence that detaches the client ...
        #[clap(long)]
        detach_keys: Option<String>,
    },
```

- [ ] **Step 2: Remove `--sig-proxy` from `ExecArgs`**

In `crates/cairn-client/src/cli.rs`, delete the `sig_proxy` field and its doc-comment from `ExecArgs` (currently lines 408-421, the block starting `/// Forward signals received by the client...` through its `pub sig_proxy: bool,`).

- [ ] **Step 3: Fix the misleading env-file doc-comment**

In `crates/cairn-client/src/cli.rs`, the `env_file` doc-comment (lines 365-370) currently says inherited values override `-e`. Replace its second paragraph so it matches the committed precedence (explicit overrides inherited):

```rust
    /// Load environment variables from a dotenv-style file.
    /// Lines of the form `KEY=VALUE`; `#` comments and blank lines
    /// are ignored. Repeatable.
    ///
    /// Lowest precedence: values from `--env-file` are overridden by
    /// `-e` flags, and all explicitly-set variables override the
    /// inherited environment.
    #[clap(long)]
    pub env_file: Vec<PathBuf>,
```

- [ ] **Step 4: Soften the attach help text**

In `crates/cairn-client/src/cli.rs`, change the `Attach` doc line (line 76) from `/// Requires the client to have an interactive terminal.` to:

```rust
    /// Best used from an interactive terminal; with stdout redirected it
    /// streams raw output (script-style capture) instead.
```

- [ ] **Step 5: Remove the now-unneeded `#[allow(dead_code)]` on the ExecArgs helper impl**

In `crates/cairn-client/src/cli.rs`, delete the `#[allow(dead_code)]` attribute (line 435) above `impl ExecArgs` — the helpers are used by `exec::run_exec` now.

- [ ] **Step 6: Rewrite `main.rs` to set up the runtime and dispatch**

Replace the body of `crates/cairn-client/src/main.rs` (keeping the `mod` lines added in earlier tasks) with:

```rust
use clap::{CommandFactory, Parser};

mod attach;
mod cli;
mod connect;
mod detach;
mod exec;
mod signals;
mod terminal;

use attach::AttachOptions;
use cli::{Cli, Command, SessionTarget};
use connect::Endpoint;
use detach::DetachKeys;

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    // Completion generation needs no runtime and no daemon.
    if let Command::Completion { shell } = args.command {
        let mut cmd = Cli::command();
        let bin_name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
        return Ok(());
    }

    init_tracing(args.verbose);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let code = rt.block_on(dispatch(args))?;
    std::process::exit(code);
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("CAIRN_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

async fn dispatch(cli: Cli) -> anyhow::Result<i32> {
    match &cli.command {
        Command::Attach { session, no_stdin, detach_keys } => {
            let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
            let id = resolve_target(&endpoint, session).await?;
            let opts = AttachOptions {
                no_stdin: *no_stdin,
                detach_keys: DetachKeys::parse_or_default(detach_keys.as_deref())
                    .map_err(|e| anyhow::anyhow!(e))?,
            };
            attach::run(&endpoint, &id, opts).await
        }
        Command::Exec(args) => exec::run_exec(&cli, args, false, false).await,
        Command::Run(args) => exec::run_exec(&cli, args, true, true).await,
        Command::Completion { .. } => Ok(0), // handled before the runtime
        _ => anyhow::bail!(
            "this command is not implemented yet; the interactive-attach milestone covers attach/exec/run"
        ),
    }
}

/// Resolve a single-session target to a concrete session id. `--latest` is
/// resolved client-side via `list_all` (it has no wire representation).
async fn resolve_target(endpoint: &Endpoint, target: &SessionTarget) -> anyhow::Result<String> {
    use cairn_protocol::client::cairn::daemon::sessions;
    if target.latest {
        let client = endpoint.client();
        let mut all = sessions::list_all(&client, ())
            .await
            .map_err(|e| anyhow::anyhow!("cannot reach cairn-daemon at {}: {e}", endpoint.path().display()))?;
        all.sort_by_key(|s| s.created_at_unix_ms);
        let latest = all.last().ok_or_else(|| anyhow::anyhow!("no sessions to attach to"))?;
        Ok(latest.id.clone())
    } else if let Some(s) = &target.session {
        Ok(s.clone())
    } else {
        anyhow::bail!("no session specified")
    }
}
```

- [ ] **Step 7: Verify the CLI builds and parses**

Run: `cargo build -p cairn && cargo nextest run -p cairn cli::tests::verify_cli`
Expected: builds; `verify_cli` (clap's `debug_assert`) passes with `--sig-proxy` removed.

- [ ] **Step 8: Smoke-check help and an error path**

Run: `cargo run -p cairn -- attach --help`
Expected: help shows `--no-stdin` and `--detach-keys`, no `--sig-proxy`.

Run: `cargo run -p cairn -- --daemon wss://x:443 attach foo`
Expected: exits non-zero printing `remote transports (WebTransport) are not yet supported`.

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-client/src/cli.rs crates/cairn-client/src/main.rs
git commit -m "feat(cairn-client): wire attach/exec/run dispatch; drop --sig-proxy

Removes the --sig-proxy flag (v0 commits to the detach-camp signal model;
transparent forwarding is future work) and fixes the env-file doc to match
the explicit-overrides-inherited precedence."
```

---

### Task 11: Integration — PTY harness

**Files:**
- Create: `crates/cairn-client/tests/attach_pty.rs`

End-to-end: an in-process daemon over a tempdir UDS, a real `cairn attach` subprocess wired to a pty, driving input/output and the detach sequence. This is the integration checkpoint — expect to iterate on timing/fd handling during execution.

- [ ] **Step 1: Write the harness test**

Create `crates/cairn-client/tests/attach_pty.rs`:

```rust
//! End-to-end: the real `cairn attach` binary against an in-process daemon,
//! driven through a pty. Asserts input is echoed, the detach key exits cleanly,
//! and the session survives the detach.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use cairn_daemon::config::DaemonConfig;
use cairn_daemon::daemon::Daemon;
use cairn_protocol::cairn::daemon::types::SessionSpec;
use tokio_util::sync::CancellationToken;

fn cat_spec() -> SessionSpec {
    SessionSpec {
        name: Some("itest".to_string()),
        command: vec!["cat".to_string()],
        env: vec![],
        env_inherit: true,
        workdir: None,
        tty: true,
        stdin: true,
        idle_timeout_secs: None,
        scrollback_lines: 100,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attach_echoes_input_then_detach_keeps_session_alive() -> anyhow::Result<()> {
    // ---- in-process daemon on a tempdir socket ----
    let tmp = tempfile::tempdir()?;
    let sock = tmp.path().join("cairn.sock");
    let mut cfg = DaemonConfig::default();
    cfg.socket_path = sock.clone();
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
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(sock.exists(), "daemon socket was not created");

    // ---- create a `cat` session (echoes input) ----
    let info = daemon.registry.create(cat_spec(), &daemon.cfg.default_shell).await?;
    let id = info.id.clone();

    // ---- spawn `cairn attach <id>` wired to a pty ----
    let pty = nix::pty::openpty(None, None)?;
    let master_fd = pty.master.as_raw_fd();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_cairn"));
    cmd.arg("--daemon")
        .arg(format!("unix://{}", sock.display()))
        .arg("attach")
        .arg(&id)
        // SAFETY: dup the slave for each of stdin/stdout/stderr.
        .stdin(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave.as_raw_fd())) })
        .stdout(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave.as_raw_fd())) })
        .stderr(unsafe { Stdio::from_raw_fd(libc::dup(pty.slave.as_raw_fd())) });
    let mut child = cmd.spawn()?;
    drop(pty.slave); // the parent keeps only the master

    // ---- reader thread for the master fd ----
    let mut master = unsafe { std::fs::File::from_raw_fd(libc::dup(master_fd)) };
    let (out_tx, out_rx) = mpsc::channel::<Vec<u8>>();
    {
        let mut rd = master.try_clone()?;
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match rd.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if out_tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    // ---- give the client time to attach + receive the snapshot ----
    std::thread::sleep(Duration::from_millis(700));

    // ---- write input; expect `cat` to echo it back ----
    master.write_all(b"hello\n")?;
    master.flush()?;
    assert!(
        wait_for(&out_rx, b"hello", Duration::from_secs(3)),
        "attach should echo typed input back to the terminal"
    );

    // ---- send the default detach sequence (ctrl-q, ctrl-q) ----
    master.write_all(&[0x11, 0x11])?;
    master.flush()?;

    // ---- the client should exit cleanly ----
    let status = wait_child(&mut child, Duration::from_secs(5));
    assert_eq!(status.and_then(|s| s.code()), Some(0), "client should exit 0 on detach");

    // ---- the session must still be alive ----
    let entry = daemon.registry.resolve(&id).expect("session must survive detach");
    assert!(
        entry.handle().try_exit_status().is_none(),
        "child must still be running after the client detaches"
    );

    shutdown.cancel();
    let _ = serve.await;
    Ok(())
}

/// Drain the reader channel until `needle` is seen or the deadline passes.
fn wait_for(rx: &mpsc::Receiver<Vec<u8>>, needle: &[u8], timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let mut acc = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => {
                acc.extend_from_slice(&chunk);
                if acc.windows(needle.len()).any(|w| w == needle) {
                    return true;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        }
    }
    false
}

/// Poll `try_wait` until the child exits or the deadline passes.
fn wait_child(child: &mut std::process::Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    let _ = child.kill();
    None
}
```

- [ ] **Step 2: Run the harness**

Run: `cargo nextest run -p cairn --test attach_pty -- --nocapture`
Expected: PASS. Likely iteration points if it fails:
- If the client never echoes: confirm the io future actually pumps the inbound `events` stream (this is the one wRPC assumption flagged in the plan header). If not, the fix is to also `tokio::spawn` the io future in `attach.rs` in addition to selecting on it, or to drive input on a separate task.
- If the detach doesn't exit: confirm `ctrl-q,ctrl-q` (`0x11 0x11`) reaches the client before any byte breaks the partial match.
- If `child.code()` is `None` (signal): the client may have been killed by the pty closing — increase the post-write delay.

- [ ] **Step 3: Run the full client test suite**

Run: `cargo nextest run -p cairn`
Expected: all unit tests + the harness pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-client/tests/attach_pty.rs
git commit -m "test(cairn-client): pty harness for attach echo + detach + session survival"
```

---

## Self-Review

**Spec coverage:**
- Scope (attach/exec/run over UDS; others deferred) — Task 10 dispatch (`_ => bail`). ✓
- Connection layer, UDS-only, ws rejected — Task 2. ✓
- `cli.rs` edits (remove `--sig-proxy`; soften help) — Task 10. ✓
- Terminal guard (raw mode, RIS, size, clear, non-TTY degrade) — Task 5 + the stdout warning in Task 7. ✓
- Attach driver (persistent stdin thread; per-connection events stream; select loop; outcome classification; reconnect-with-backoff + `CAIRN_RECONNECT_TIMEOUT`; socket-gone immediate give-up) — Task 7. ✓
- Detach keys (docker-style parse; raw + CSI-u matcher; partial flush) — Tasks 3-4. ✓
- Signals (SIGPIPE already ignored by std; SIGWINCH=resize; termination=detach; SIGTSTP/CONT default) — Task 6 + Task 7 usage. ✓
- exec/run (SessionSpec build; env merge with explicit>inherited doc; workdir=cwd; scrollback 1000; `--detach`) — Tasks 8-9. ✓
- Exit codes (detach 0; child code / 128+sig; fatal 1; give-up 1) — Task 7 `exit_code` + outcome handling. ✓
- Daemon-side prereqs (`error_codes`, kicked/lagged, name inference, env regression) — the separate prereqs plan; consumed here in Task 7 (`error_codes::CLIENT_LAGGED`). ✓
- Testing (pure-logic units for parse/matcher/merge/connect; PTY harness) — Tasks 2,3,4,8,11. ✓

**Placeholder scan:** none — every code step is complete. The two "likely fix-up" notes (Task 7 Step 2, Task 11 Step 2) are real execution-time verification of the one unconfirmed wRPC assumption (does the single io future pump the inbound events stream), not placeholders.

**Type consistency:** `Endpoint`/`Client`/`.client()`/`.path()` consistent across Tasks 2, 7, 9, 10. `DetachKeys::{parse, parse_or_default, keys}`, `DetachKey::{raw, csiu}`, `Matcher::{new, feed}` consistent across Tasks 3, 4, 7, 9, 10. `AttachOptions { no_stdin, detach_keys }` + `attach::run(&Endpoint, &str, AttachOptions) -> Result<i32>` consistent across Tasks 7, 9, 10. `merge_env(&[PathBuf], &[String])` and `run_exec(&Cli, &ExecArgs, bool, bool)` consistent across Tasks 8, 9, 10. `ServerEvent::Exited` fields `code: Option<i32>` / `signal: Option<u8>` match the wire `exit-status`.

**No-panic audit:** production code (`connect.rs`, `terminal.rs`, `signals.rs`, `attach.rs`, `exec.rs`, `main.rs`) uses `?` / `map_err` / `let _ =` / `unwrap_or*` — no `unwrap`/`expect`/`panic!`/`unreachable!`. Test code uses `unwrap`/`expect`/`assert` freely (exempt).

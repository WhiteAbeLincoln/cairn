# PTY Multi-Client Semantics Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add multi-client coordination to `cairn-pty`: an opaque `ClientId`, leader election by most-recent user input, leader-only resize gated by a typed error, and a vte-driven input classifier that distinguishes intentional interactions from terminal back-chatter.

**Architecture:** All changes live in the `cairn-pty` library crate. The libghostty `Terminal` stays private to the worker (not modeled as a client). `ClientId` is a transport-agnostic opaque newtype with an infallible constructor that maps caller counter values into the non-zero range internally. Election state (`leader: Option<ClientId>`, `last_input_at: Option<Instant>`) lives as locals inside `run_session`. `Subscription` drop sends `Command::Detach` back through the worker's flume channel to clear the seat when the leader disconnects. The classifier uses the `vte` crate's `Perform` trait — we own the policy, vte owns the state machine.

**Tech Stack:** Rust, tokio (current_thread runtime + LocalSet), `flume` channels, `tokio::sync::broadcast`, `libghostty-vt` 0.1.1, `vte` (new), `tracing` + `tracing-test` (dev), `snafu` for errors, `async-trait`.

**Spec:** `docs/superpowers/specs/2026-05-22-pty-multi-client-semantics-design.md`.

---

## Conventions used throughout this plan

- **Run from repo root** (`/Users/abe/Projects/cairn`) unless stated.
- **Cargo invocation:** `nix develop --command cargo ...` is the project pattern (visible in `examples/echo.rs`). All cargo commands below use this prefix.
- **Test scope:** `cargo test -p cairn-pty` runs all unit + integration tests in the crate. Use `--lib` to scope to unit tests, `--test <name>` for a specific integration file.
- **TDD discipline:** every code-writing task starts with a failing test and ends with a passing one, then a commit.
- **Commit style:** small, focused, conventional-ish messages. Each task ends with its own commit.

---

### Task 1: Add `vte` and `tracing-test` dependencies

**Files:**
- Modify: `crates/cairn-pty/Cargo.toml`

- [ ] **Step 1: Add `vte` to runtime dependencies and `tracing-test` to dev dependencies**

Edit `crates/cairn-pty/Cargo.toml`. The current `[dependencies]` and `[dev-dependencies]` blocks are:

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
pty-process = { version = "0.4", features = ["async"] }

[dev-dependencies]
tokio = { version = "1.52", features = ["full", "test-util", "macros"] }
```

Change to:

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
pty-process = { version = "0.4", features = ["async"] }
vte = "0.13"

[dev-dependencies]
tokio = { version = "1.52", features = ["full", "test-util", "macros"] }
tracing-test = "0.2"
```

- [ ] **Step 2: Verify deps resolve**

Run: `nix develop --command cargo build -p cairn-pty`
Expected: build succeeds. (No code uses `vte` or `tracing-test` yet; we're just confirming the deps resolve.)

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/Cargo.toml
git commit -m "Add vte and tracing-test deps for multi-client semantics"
```

---

### Task 2: Create `ClientId` type with tests

**Files:**
- Create: `crates/cairn-pty/src/client_id.rs`
- Modify: `crates/cairn-pty/src/lib.rs:5` (add `mod client_id;` next to existing module declarations, and add `pub use client_id::ClientId;` next to existing `pub use` statements)

- [ ] **Step 1: Write the failing test file**

Create `crates/cairn-pty/src/client_id.rs`:

```rust
//! Opaque client identity used to track per-attached-client state in
//! `PtySession` (leader election, detach notifications). Caller-supplied
//! and transport-agnostic — the library does only equality comparisons.
//!
//! See `docs/superpowers/specs/2026-05-22-pty-multi-client-semantics-design.md`.

use std::fmt;
use std::num::NonZeroU64;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ClientId(NonZeroU64);

impl ClientId {
    /// Construct a `ClientId` from a daemon counter value.
    ///
    /// The library adds 1 internally so the underlying `NonZeroU64` is
    /// never zero. The daemon may start its counter at 0; the returned
    /// id is opaque.
    ///
    /// # Panics
    ///
    /// Panics if `value == u64::MAX`. At 1M attaches per second this
    /// would take ~584,500 years; in debug builds Rust's overflow check
    /// fires at the `+ 1`, in release builds the `NonZeroU64` invariant
    /// fires on the wrapped result. Both are the desired behavior —
    /// reaching this case means something has gone catastrophically
    /// wrong upstream.
    pub fn from_u64(value: u64) -> Self {
        ClientId(
            NonZeroU64::new(value + 1)
                .expect("ClientId from u64::MAX is unsupported"),
        )
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_u64_zero_maps_to_one() {
        let id = ClientId::from_u64(0);
        assert_eq!(format!("{id}"), "1");
    }

    #[test]
    fn from_u64_preserves_uniqueness() {
        let a = ClientId::from_u64(0);
        let b = ClientId::from_u64(1);
        let c = ClientId::from_u64(0);
        assert_ne!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn display_renders_underlying_value() {
        let id = ClientId::from_u64(41);
        assert_eq!(format!("{id}"), "42");
    }

    #[test]
    fn is_copy_and_hashable() {
        use std::collections::HashSet;
        let a = ClientId::from_u64(0);
        let b = a; // Copy
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
    }
}
```

- [ ] **Step 2: Wire the module into `lib.rs`**

In `crates/cairn-pty/src/lib.rs`, find the existing module declarations (around line 5):

```rust
mod error;
mod ghostty;
mod session;
mod subscription;
mod types;
```

Add `mod client_id;` so the block becomes:

```rust
mod client_id;
mod error;
mod ghostty;
mod session;
mod subscription;
mod types;
```

Find the existing `pub use` block (around line 11):

```rust
pub use error::PtyError;
pub use ghostty::ExitStatus;
pub use ghostty::GhosttyPty;
pub use session::PtySession;
pub use subscription::Subscription;
pub use types::{SpawnOptions, TermSize};
```

Add `pub use client_id::ClientId;`:

```rust
pub use client_id::ClientId;
pub use error::PtyError;
pub use ghostty::ExitStatus;
pub use ghostty::GhosttyPty;
pub use session::PtySession;
pub use subscription::Subscription;
pub use types::{SpawnOptions, TermSize};
```

- [ ] **Step 3: Run the tests**

Run: `nix develop --command cargo test -p cairn-pty --lib client_id::`
Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/src/client_id.rs crates/cairn-pty/src/lib.rs
git commit -m "Add opaque ClientId newtype for multi-client coordination"
```

---

### Task 3: Add `PtyError::NotLeader` variant

**Files:**
- Modify: `crates/cairn-pty/src/error.rs`

- [ ] **Step 1: Write a failing test**

Add at the bottom of `crates/cairn-pty/src/error.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientId;

    #[test]
    fn not_leader_display_includes_requester_and_current() {
        let err = PtyError::NotLeader {
            requester: ClientId::from_u64(0),
            current: Some(ClientId::from_u64(1)),
        };
        let msg = format!("{err}");
        assert!(msg.contains("1"), "should mention requester id 1, got: {msg}");
        assert!(msg.contains("2"), "should mention current leader id 2, got: {msg}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `nix develop --command cargo test -p cairn-pty --lib error::tests::not_leader`
Expected: compile error — `NotLeader` variant does not exist.

- [ ] **Step 3: Add the variant**

In `crates/cairn-pty/src/error.rs`, replace the existing enum body:

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

    #[snafu(display("resize rejected: client {requester} is not the leader (current: {current:?})"))]
    NotLeader {
        requester: crate::ClientId,
        current: Option<crate::ClientId>,
    },
}

impl From<std::io::Error> for PtyError {
    fn from(source: std::io::Error) -> Self {
        Self::Io { source }
    }
}
```

(Keep the existing `#[cfg(test)] mod tests` block from Step 1 at the bottom.)

- [ ] **Step 4: Run to verify the test passes**

Run: `nix develop --command cargo test -p cairn-pty --lib error::tests::not_leader`
Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/src/error.rs
git commit -m "Add PtyError::NotLeader for rejected non-leader resizes"
```

---

### Task 4: Create `is_user_input` classifier with all unit tests

**Files:**
- Create: `crates/cairn-pty/src/ghostty/input_classifier.rs`
- Modify: `crates/cairn-pty/src/ghostty/mod.rs` (add `mod input_classifier;` near the other module decl)

- [ ] **Step 1: Write the full test suite first (will fail to compile)**

Create `crates/cairn-pty/src/ghostty/input_classifier.rs`:

```rust
//! Classify a write payload as "user input" or "terminal back-chatter."
//!
//! Used by the worker to decide whether a write from a non-leader
//! client should promote that client to leader. Mirrors zmx's
//! `util.isUserInput` (`zmx/src/util.zig:446-477`) with one deliberate
//! divergence: mouse press/release/scroll/drag DO qualify as user
//! input. See the spec ("Divergences from zmx") for the rationale.

use vte::{Params, Parser, Perform};

/// Returns true if any byte or recognized escape sequence in `data`
/// could only have come from intentional human interaction (typing,
/// clicking, scrolling) and not from terminal-emitted back-chatter
/// (mouse motion, focus events, query replies).
pub(crate) fn is_user_input(data: &[u8]) -> bool {
    let mut classifier = Classifier::default();
    let mut parser = Parser::new();
    for &b in data {
        parser.advance(&mut classifier, b);
    }
    classifier.found
}

#[derive(Default)]
struct Classifier {
    found: bool,
}

impl Perform for Classifier {
    fn print(&mut self, _c: char) {
        self.found = true;
    }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, 0x08 | 0x09 | 0x0A | 0x0D | 0x7F) {
            self.found = true;
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &Params,
        intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        match (intermediates, action) {
            // Focus in/out — terminal back-chatter, not user input.
            (b"", 'I') | (b"", 'O') => {}

            // SGR mouse press / release / scroll / drag / motion.
            (b"<", 'M') | (b"<", 'm') => {
                let button = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let motion = button & 32 != 0;
                let scroll = button & 64 != 0;
                let button_held = (button & 0b11) != 0b11;
                // Promote unless this is a pure motion event with no
                // button held and no scroll. Drag (motion + button) and
                // scroll both qualify.
                if !motion || button_held || scroll {
                    self.found = true;
                }
            }

            // Kitty keyboard protocol.
            (b"", 'u') => self.found = true,

            // Legacy modified-key sequences: CSI 1 ; <mod> <final>.
            (b"", a)
                if matches!(a, 'A' | 'B' | 'C' | 'D' | 'F' | 'H' | 'P' | 'Q' | 'R' | 'S') =>
            {
                let first = params.iter().next().and_then(|p| p.first().copied());
                let mod_param = params.iter().nth(1).and_then(|p| p.first().copied());
                if first == Some(1) && mod_param.is_some_and(|m| m >= 2) {
                    self.found = true;
                }
            }

            // Everything else (DA1/DA2 replies, DSR, DECRQM, etc.) is
            // not user input.
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell: bool) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
    fn hook(
        &mut self,
        _params: &Params,
        _intermediates: &[u8],
        _ignore: bool,
        _action: char,
    ) {
    }
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::is_user_input;

    #[test]
    fn empty_payload_is_not_user_input() {
        assert!(!is_user_input(b""));
    }

    #[test]
    fn printable_ascii_is_user_input() {
        assert!(is_user_input(b"a"));
        assert!(is_user_input(b"hello"));
    }

    #[test]
    fn carriage_return_is_user_input() {
        assert!(is_user_input(b"\r"));
    }

    #[test]
    fn backspace_is_user_input() {
        assert!(is_user_input(b"\x08"));
        assert!(is_user_input(b"\x7F"));
    }

    #[test]
    fn tab_and_newline_are_user_input() {
        assert!(is_user_input(b"\t"));
        assert!(is_user_input(b"\n"));
    }

    #[test]
    fn ctrl_up_arrow_is_user_input() {
        // ESC [ 1 ; 5 A
        assert!(is_user_input(b"\x1b[1;5A"));
    }

    #[test]
    fn kitty_modified_key_is_user_input() {
        // ESC [ 97 ; 5 u (Ctrl-a in kitty protocol)
        assert!(is_user_input(b"\x1b[97;5u"));
    }

    #[test]
    fn da1_query_is_not_user_input() {
        // ESC [ c — the shell would send this; classifier sees the
        // shape and refuses to promote on it.
        assert!(!is_user_input(b"\x1b[c"));
    }

    #[test]
    fn da1_reply_is_not_user_input() {
        // ESC [ ? 6 2 ; 2 2 c
        assert!(!is_user_input(b"\x1b[?62;22c"));
    }

    #[test]
    fn focus_events_are_not_user_input() {
        assert!(!is_user_input(b"\x1b[I"));
        assert!(!is_user_input(b"\x1b[O"));
    }

    #[test]
    fn mouse_press_is_user_input() {
        // ESC [ < 0 ; 10 ; 20 M  (button 0 press at col 10 row 20)
        assert!(is_user_input(b"\x1b[<0;10;20M"));
    }

    #[test]
    fn mouse_release_is_user_input() {
        assert!(is_user_input(b"\x1b[<0;10;20m"));
    }

    #[test]
    fn mouse_right_button_is_user_input() {
        assert!(is_user_input(b"\x1b[<2;10;20M"));
    }

    #[test]
    fn mouse_scroll_is_user_input() {
        // Button 64 = scroll up, 65 = scroll down.
        assert!(is_user_input(b"\x1b[<64;10;20M"));
        assert!(is_user_input(b"\x1b[<65;10;20M"));
    }

    #[test]
    fn mouse_drag_is_user_input() {
        // Button 32 = motion flag + button 0 held (drag).
        assert!(is_user_input(b"\x1b[<32;10;20M"));
    }

    #[test]
    fn mouse_motion_only_is_not_user_input() {
        // Button 35 = motion flag (32) + button-none (3).
        assert!(!is_user_input(b"\x1b[<35;10;20M"));
    }

    #[test]
    fn x10_mouse_is_user_input() {
        // ESC [ M <btn> <col> <row>  (button 0 press)
        assert!(is_user_input(b"\x1b[M\x20\x20\x20"));
    }

    #[test]
    fn disjunctive_payload_promotes() {
        // Motion-only followed by 'a' — qualifies because 'a' qualifies.
        assert!(is_user_input(b"\x1b[<35;10;20Ma"));
    }
}
```

- [ ] **Step 2: Wire the module into `ghostty/mod.rs`**

In `crates/cairn-pty/src/ghostty/mod.rs`, find the existing module declarations at the top:

```rust
mod process;
mod worker;
```

Add `input_classifier`:

```rust
mod input_classifier;
mod process;
mod worker;
```

- [ ] **Step 3: Run the tests**

Run: `nix develop --command cargo test -p cairn-pty --lib input_classifier::`
Expected: all 18 tests pass.

If `x10_mouse_is_user_input` fails: vte 0.13 might dispatch X10 mouse differently. If so, add this pre-filter as the first lines of `is_user_input` (before constructing the parser):

```rust
// X10 mouse: ESC [ M <btn> <col> <row>. vte may consume the three
// trailing bytes inside its state machine differently across
// versions; this pre-check covers it explicitly.
if data.len() >= 4 && data[0] == 0x1b && data[1] == b'[' && data[2] == b'M' {
    return true;
}
```

Re-run the tests until they all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/src/ghostty/input_classifier.rs crates/cairn-pty/src/ghostty/mod.rs
git commit -m "Add vte-driven is_user_input classifier"
```

---

### Task 5: Update `PtySession` trait signatures

**Files:**
- Modify: `crates/cairn-pty/src/session.rs`

- [ ] **Step 1: Replace the trait body**

In `crates/cairn-pty/src/session.rs`, replace the entire file contents with:

```rust
use bytes::Bytes;

use super::{ClientId, PtyError, Subscription, TermSize};

/// A live pseudo-terminal session wrapping a child process.
///
/// Implementations are `Send + Sync` so they can be shared across many
/// async tasks (e.g. WebSocket handlers, each holding `Arc<dyn PtySession>`).
///
/// See `docs/superpowers/specs/2026-05-22-pty-multi-client-semantics-design.md`
/// for the multi-client coordination model (leader election, NotLeader
/// errors, ClientId semantics).
#[async_trait::async_trait]
pub trait PtySession: Send + Sync {
    /// Current terminal size in cells. Reports the kernel's TIOCGWINSZ
    /// value (what the child process actually sees).
    async fn size(&self) -> Result<TermSize, PtyError>;

    /// Resize the terminal grid. Only honored when `client_id` is the
    /// current leader. Returns `PtyError::NotLeader` otherwise. A
    /// resize from any client promotes them to leader if the seat is
    /// empty.
    async fn resize(&self, client_id: ClientId, size: TermSize) -> Result<(), PtyError>;

    /// Atomically snapshot current terminal state AND register a live
    /// stream of subsequent output. Subscribing does not claim
    /// leadership; only `write` or `resize` calls promote. See
    /// [`Subscription`] for the snapshot/stream contract.
    async fn subscribe(&self, client_id: ClientId) -> Result<Subscription, PtyError>;

    /// Write bytes to the PTY master (becomes the child's stdin).
    /// Bytes that pass the user-input classifier promote `client_id`
    /// to leader if it isn't already. Concurrent calls from multiple
    /// tasks serialize at byte boundaries via the session's command
    /// channel.
    async fn write(&self, client_id: ClientId, data: Bytes) -> Result<(), PtyError>;
}
```

- [ ] **Step 2: Verify the codebase fails to compile (expected)**

Run: `nix develop --command cargo build -p cairn-pty`
Expected: compile errors in `ghostty/mod.rs` (GhosttyPty trait impl mismatch), `lib.rs` (StubSession mismatch). These get fixed in the next tasks.

- [ ] **Step 3: Commit the broken state**

We commit the broken state so the trait change is its own reviewable diff. Subsequent tasks restore compile.

```bash
git add crates/cairn-pty/src/session.rs
git commit -m "Add ClientId arg to PtySession::write/resize/subscribe"
```

---

### Task 6: Update `Command` enum and add `Detach` variant

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/mod.rs`

- [ ] **Step 1: Update `Command` enum**

In `crates/cairn-pty/src/ghostty/mod.rs`, find the existing `Command` enum (lines 21-37):

```rust
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
```

Replace with (note: visibility changes from `pub(super)` to `pub(crate)` so `subscription.rs` can hold a `flume::Sender<Command>`):

```rust
pub(crate) enum Command {
    Subscribe {
        client_id: ClientId,
        reply: oneshot::Sender<Result<Subscription, PtyError>>,
    },
    Resize {
        client_id: ClientId,
        size: TermSize,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Size {
        reply: oneshot::Sender<Result<TermSize, PtyError>>,
    },
    Write {
        client_id: ClientId,
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    /// Sent by `SubscriptionGuard::drop`. Worker checks if `client_id`
    /// is the current leader and clears the seat if so.
    Detach { client_id: ClientId },
    Shutdown,
}
```

- [ ] **Step 2: Update imports at the top of `ghostty/mod.rs`**

Find the existing `use` block:

```rust
use bytes::Bytes;
use tokio::sync::oneshot;

use super::{PtyError, SpawnOptions, Subscription, TermSize};
```

Change `super::{...}` to include `ClientId`:

```rust
use bytes::Bytes;
use tokio::sync::oneshot;

use super::{ClientId, PtyError, SpawnOptions, Subscription, TermSize};
```

- [ ] **Step 3: Build to verify compile errors are now in expected places**

Run: `nix develop --command cargo build -p cairn-pty`
Expected: compile errors are now scoped to:
- `ghostty/mod.rs` GhosttyPty trait impl (missing client_id in Command construction)
- `ghostty/worker.rs` (pattern matches on Command don't cover Detach; Subscribe/Write/Resize match arms don't bind client_id)
- `lib.rs` (StubSession trait impl signature mismatch)

Don't try to fix everything yet; the next tasks scope the work.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/src/ghostty/mod.rs
git commit -m "Thread ClientId through Command enum; add Detach variant"
```

---

### Task 7: Update `GhosttyPty` trait impl to thread `client_id` through

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/mod.rs`

- [ ] **Step 1: Update the impl block**

In `crates/cairn-pty/src/ghostty/mod.rs`, find the existing `impl super::PtySession for GhosttyPty` block (around line 98) and replace its three methods (`resize`, `subscribe`, `write`) — `size` is unchanged:

```rust
#[async_trait::async_trait]
impl super::PtySession for GhosttyPty {
    async fn size(&self) -> Result<super::TermSize, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Size { reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn resize(&self, client_id: ClientId, size: super::TermSize) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Resize {
                client_id,
                size,
                reply: tx,
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn subscribe(&self, client_id: ClientId) -> Result<Subscription, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Subscribe {
                client_id,
                reply: tx,
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn write(&self, client_id: ClientId, data: bytes::Bytes) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Write {
                client_id,
                data,
                reply: tx,
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }
}
```

- [ ] **Step 2: Build to verify GhosttyPty compiles**

Run: `nix develop --command cargo build -p cairn-pty`
Expected: errors remaining are in `worker.rs` (Command match arms) and `lib.rs` (StubSession). GhosttyPty itself should compile.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/src/ghostty/mod.rs
git commit -m "Forward ClientId from GhosttyPty trait impl through Command channel"
```

---

### Task 8: Update `SubscriptionGuard` to carry `client_id` and send `Detach` on drop

**Files:**
- Modify: `crates/cairn-pty/src/subscription.rs`

- [ ] **Step 1: Rewrite `subscription.rs`**

Replace the entire contents of `crates/cairn-pty/src/subscription.rs` with:

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::Bytes;
use tokio::sync::broadcast;

use crate::ClientId;
use crate::ghostty::Command;

/// Result of a successful [`crate::pty::PtySession::subscribe`] call.
///
/// `snapshot` is an opaque VT escape sequence representing the
/// terminal state at the moment of subscription. Feed it to a
/// VT100/xterm-compatible emulator (xterm.js, ghostty-web, etc.)
/// before processing `stream` bytes.
///
/// `stream` yields bytes that arrived strictly *after* the snapshot
/// was captured — no gap, no overlap.
/// `broadcast::error::RecvError::Lagged(_)` means the subscriber fell
/// behind the broadcast capacity; recover by dropping this
/// `Subscription` and calling `subscribe()` again — the new snapshot
/// reflects current state and the new receiver starts clean.
/// `RecvError::Closed` means the session has exited.
///
/// While a `Subscription` is alive, the worker treats this client as
/// a "primary" attached emulator: backend auto-replies to terminal
/// queries (DA, XTVERSION, DSR, etc.) are suppressed so the client's
/// emulator can answer instead. The primary count returns to zero
/// when this Subscription is dropped.
///
/// Dropping the Subscription also sends `Command::Detach` to the
/// worker so it can clear the leader seat if this client held it.
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
    _guard: SubscriptionGuard,
}

impl Subscription {
    /// Construct a `Subscription`, incrementing `primary_count` and
    /// binding both decrement and detach-notification to drop.
    pub(crate) fn new(
        snapshot: Bytes,
        stream: broadcast::Receiver<Bytes>,
        primary_count: Arc<AtomicUsize>,
        client_id: ClientId,
        cmd_tx: flume::Sender<Command>,
    ) -> Self {
        primary_count.fetch_add(1, Ordering::Relaxed);
        Self {
            snapshot,
            stream,
            _guard: SubscriptionGuard {
                client_id,
                primary_count,
                cmd_tx,
            },
        }
    }
}

/// RAII guard combining two on-drop responsibilities:
///   1. Decrement the worker's primary-attached counter.
///   2. Send `Command::Detach` so the worker can clear the leader
///      seat if this client held it.
pub(crate) struct SubscriptionGuard {
    client_id: ClientId,
    primary_count: Arc<AtomicUsize>,
    cmd_tx: flume::Sender<Command>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.primary_count.fetch_sub(1, Ordering::Relaxed);
        // Best-effort. If `cmd_tx` is closed, the worker has already
        // shut down and there's no leader state to clear.
        let _ = self.cmd_tx.send(Command::Detach {
            client_id: self.client_id,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::broadcast;

    fn dummy_channel() -> flume::Sender<Command> {
        let (tx, _rx) = flume::unbounded::<Command>();
        tx
    }

    #[test]
    fn new_increments_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let _sub = Subscription::new(
            Bytes::new(),
            rx,
            counter.clone(),
            ClientId::from_u64(0),
            dummy_channel(),
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn drop_decrements_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let sub = Subscription::new(
            Bytes::new(),
            rx,
            counter.clone(),
            ClientId::from_u64(0),
            dummy_channel(),
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn multiple_subscriptions_share_counter() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx1) = broadcast::channel::<Bytes>(1);
        let (_tx2, rx2) = broadcast::channel::<Bytes>(1);
        let sub1 = Subscription::new(
            Bytes::new(),
            rx1,
            counter.clone(),
            ClientId::from_u64(0),
            dummy_channel(),
        );
        let sub2 = Subscription::new(
            Bytes::new(),
            rx2,
            counter.clone(),
            ClientId::from_u64(1),
            dummy_channel(),
        );
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        drop(sub1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(sub2);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn drop_sends_detach_command() {
        let counter = Arc::new(AtomicUsize::new(0));
        let (_tx, rx) = broadcast::channel::<Bytes>(1);
        let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
        let client_id = ClientId::from_u64(0);
        let sub = Subscription::new(
            Bytes::new(),
            rx,
            counter,
            client_id,
            cmd_tx,
        );
        drop(sub);
        let received = cmd_rx.try_recv().expect("Detach should have been sent");
        match received {
            Command::Detach { client_id: id } => assert_eq!(id, client_id),
            other => panic!("expected Detach, got {:?}", std::mem::discriminant(&other)),
        }
    }
}
```

- [ ] **Step 2: Run the subscription tests**

Run: `nix develop --command cargo test -p cairn-pty --lib subscription::`
Expected: 4 tests pass. (Other unrelated build failures from `worker.rs` and `lib.rs` may still block compile. If so, jump ahead — but the test will eventually pass once the rest of the crate compiles.)

If the crate doesn't compile, the test command will fail with build errors before tests run. That's OK — continue with the next tasks; we'll re-run this test in Task 11 after the crate builds.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/src/subscription.rs
git commit -m "Rename PrimaryGuard to SubscriptionGuard; add ClientId + detach"
```

---

### Task 9: Update `StubSession` in `lib.rs` to match new trait

**Files:**
- Modify: `crates/cairn-pty/src/lib.rs`

- [ ] **Step 1: Update the StubSession impl**

In `crates/cairn-pty/src/lib.rs`, find the existing `StubSession` block (around lines 108-134):

```rust
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
    async fn write(&self, _: bytes::Bytes) -> Result<(), PtyError> {
        Ok(())
    }
}
```

Replace with:

```rust
struct StubSession;

#[async_trait::async_trait]
impl PtySession for StubSession {
    async fn size(&self) -> Result<TermSize, PtyError> {
        Ok(TermSize { cols: 1, rows: 1 })
    }
    async fn resize(&self, _: ClientId, _: TermSize) -> Result<(), PtyError> {
        Ok(())
    }
    async fn subscribe(&self, _: ClientId) -> Result<Subscription, PtyError> {
        use bytes::Bytes;
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        use tokio::sync::broadcast;
        let (_tx, rx) = broadcast::channel(1);
        let (cmd_tx, _cmd_rx) = flume::unbounded();
        Ok(Subscription::new(
            Bytes::new(),
            rx,
            Arc::new(AtomicUsize::new(0)),
            ClientId::from_u64(0),
            cmd_tx,
        ))
    }
    async fn write(&self, _: ClientId, _: bytes::Bytes) -> Result<(), PtyError> {
        Ok(())
    }
}
```

- [ ] **Step 2: Update the `subscription_constructs_from_parts` test**

In the same file, find the test (around line 86):

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

Update the `Subscription::new` call site:

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
    let (cmd_tx, _cmd_rx) = flume::unbounded();
    let sub = Subscription::new(snap.clone(), rx, counter, ClientId::from_u64(0), cmd_tx);
    assert_eq!(sub.snapshot, snap);
    drop(tx); // explicit drop so test asserts type accepts a Receiver
}
```

- [ ] **Step 3: Update the `stub_session_implements_trait` test if it references methods**

The test (around line 135) calls `s.size().await` which is unchanged. No update needed.

- [ ] **Step 4: Commit (build will still fail in `worker.rs` — that's expected)**

```bash
git add crates/cairn-pty/src/lib.rs
git commit -m "Update StubSession + subscription test for new trait signatures"
```

---

### Task 10: Restore worker compile — handle new Command variants and Subscribe construction

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`

This task only restores the worker to a *compiling* state. Election logic is added in Task 12. For now, every match arm has to bind `client_id` (we'll discard it via `_`) and a no-op `Detach` arm has to exist.

- [ ] **Step 1: Update imports**

In `crates/cairn-pty/src/ghostty/worker.rs`, find the `use` block near the top (around lines 9-21):

```rust
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};
use tokio::sync::broadcast;

use super::Command;
use super::process::{ChildProcess, Pty};
use crate::{PtyError, SpawnOptions, Subscription, TermSize};
```

Add `ClientId`:

```rust
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use bytes::Bytes;
use libghostty_vt::{Terminal, TerminalOptions};
use tokio::sync::broadcast;

use super::Command;
use super::process::{ChildProcess, Pty};
use crate::{ClientId, PtyError, SpawnOptions, Subscription, TermSize};
```

- [ ] **Step 2: Pass `cmd_tx` clone into `run_session` and update both spawn paths**

In `worker::spawn` (around lines 64-66), find:

```rust
std::thread::Builder::new()
    .name("cairn-pty-session".into())
    .spawn(move || {
```

The closure captures `cmd_rx` and other state but does NOT currently capture `cmd_tx`. We need to clone `cmd_tx` and move it in.

Find the line where `cmd_tx`/`cmd_rx` are created (around line 35):

```rust
let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
```

Below it, add the clone:

```rust
let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
let cmd_tx_for_worker = cmd_tx.clone();
```

Then in the thread closure (around line 67), the move-closure captures `cmd_rx`, `exit_tx`, `opts`, etc. — add `cmd_tx_for_worker` to the move. Inside the closure, `run_session` is called with `SessionState { ... }` — add `cmd_tx: cmd_tx_for_worker` to that construction.

Find this block (around line 154):

```rust
run_session(SessionState {
    pty,
    child,
    cmd_rx,
    exit_tx,
    broadcast_capacity,
    initial_size,
    scrollback_lines,
})
.await;
```

Add `cmd_tx`:

```rust
run_session(SessionState {
    pty,
    child,
    cmd_rx,
    cmd_tx: cmd_tx_for_worker,
    exit_tx,
    broadcast_capacity,
    initial_size,
    scrollback_lines,
})
.await;
```

Do the same for `worker::spawn_with` (the test-only variant). Around line 203, find:

```rust
let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
```

Add the clone below:

```rust
let (cmd_tx, cmd_rx) = flume::unbounded::<Command>();
let cmd_tx_for_worker = cmd_tx.clone();
```

In the closure (around line 218), the `run_session(SessionState { ... })` call also needs `cmd_tx`:

```rust
run_session(SessionState {
    pty,
    child,
    cmd_rx,
    cmd_tx: cmd_tx_for_worker,
    exit_tx,
    broadcast_capacity,
    initial_size,
    scrollback_lines,
})
.await;
```

- [ ] **Step 3: Add `cmd_tx` field to `SessionState`**

Find the struct definition (around lines 236-244):

```rust
struct SessionState<P: Pty, C: ChildProcess> {
    pty: P,
    child: C,
    cmd_rx: flume::Receiver<Command>,
    exit_tx: tokio::sync::watch::Sender<Option<ExitStatus>>,
    broadcast_capacity: usize,
    initial_size: TermSize,
    scrollback_lines: usize,
}
```

Add `cmd_tx`:

```rust
struct SessionState<P: Pty, C: ChildProcess> {
    pty: P,
    child: C,
    cmd_rx: flume::Receiver<Command>,
    cmd_tx: flume::Sender<Command>,
    exit_tx: tokio::sync::watch::Sender<Option<ExitStatus>>,
    broadcast_capacity: usize,
    initial_size: TermSize,
    scrollback_lines: usize,
}
```

- [ ] **Step 4: Update the Subscribe handler to pass the new args to `Subscription::new`**

Inside `run_session`, find the `Command::Subscribe` arm (around lines 491-508):

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

Update the pattern to bind `client_id` and the `Subscription::new` call to include the new args:

```rust
Command::Subscribe { client_id, reply } => {
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
    let sub = Subscription::new(
        snapshot,
        stream,
        primary_count.clone(),
        client_id,
        s.cmd_tx.clone(),
    );
    let _ = reply.send(Ok(sub));
}
```

- [ ] **Step 5: Update the other Command match arms to bind `client_id` (discarded for now)**

In the post-exit normalisation block (around lines 450-468), update the patterns to bind the new fields:

Existing:

```rust
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
```

Replace with (the `Detach` arm is a no-op post-exit, and `Subscribe { .. }` already uses `..`):

```rust
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
    Command::Detach { .. } => continue, // no-op post-exit
}
```

- [ ] **Step 6: Update the active Resize / Write match arms to bind `client_id` (discarded for now)**

Find the active Resize arm (around lines 509-524):

```rust
Command::Resize { size, reply } => {
    let res = (|| -> Result<(), PtyError> {
        terminal
            .borrow_mut()
            .resize(size.cols, size.rows, 0, 0)
            .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
        s.pty
            .set_size(size)
            .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
        Ok(())
    })();
    if res.is_ok() {
        current_size.set(size);
    }
    let _ = reply.send(res);
}
```

Change to bind `client_id` (we'll *use* it in Task 12 — for now it's `_client_id`):

```rust
Command::Resize { client_id: _client_id, size, reply } => {
    let res = (|| -> Result<(), PtyError> {
        terminal
            .borrow_mut()
            .resize(size.cols, size.rows, 0, 0)
            .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
        s.pty
            .set_size(size)
            .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
        Ok(())
    })();
    if res.is_ok() {
        current_size.set(size);
    }
    let _ = reply.send(res);
}
```

Find the Write arm (around lines 528-531):

```rust
Command::Write { data, reply } => {
    let res = s.pty.write_all(&data).await.map_err(PtyError::from);
    let _ = reply.send(res);
}
```

Change to:

```rust
Command::Write { client_id: _client_id, data, reply } => {
    let res = s.pty.write_all(&data).await.map_err(PtyError::from);
    let _ = reply.send(res);
}
```

- [ ] **Step 7: Add `Detach` arm to the active command handler (no-op for now)**

Right after the Write arm (still inside the `match cmd` block), add:

```rust
Command::Detach { client_id: _client_id } => {
    // No-op for now; Task 12 adds leader-vacation logic here.
}
```

- [ ] **Step 8: Update `drain_commands_with_construction_error` to handle Detach**

Find the function (around lines 591-612):

```rust
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
```

Update the Subscribe pattern to `..` (it now has a `client_id` field), and add the Detach arm:

```rust
fn drain_commands_with_construction_error(cmd_rx: &flume::Receiver<Command>) {
    let make_err = || PtyError::Backend {
        source: Box::new(std::io::Error::other("VT terminal construction failed")),
    };
    while let Ok(cmd) = cmd_rx.try_recv() {
        match cmd {
            Command::Shutdown => {}
            Command::Subscribe { reply, .. } => {
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
            Command::Detach { .. } => {}
        }
    }
}
```

- [ ] **Step 9: Build to confirm the worker compiles**

Run: `nix develop --command cargo build -p cairn-pty`
Expected: success. Existing tests/examples in `tests/` and `examples/` still fail to compile because their call sites pass the old signature.

- [ ] **Step 10: Commit**

```bash
git add crates/cairn-pty/src/ghostty/worker.rs
git commit -m "Restore worker compile after Command shape changes"
```

---

### Task 11: Migrate `echo` example and integration test call sites

**Files:**
- Modify: `crates/cairn-pty/examples/echo.rs`
- Modify: `crates/cairn-pty/tests/pty_io.rs`
- Modify: `crates/cairn-pty/tests/pty_lifecycle.rs`
- Modify: `crates/cairn-pty/tests/pty_resize.rs`

- [ ] **Step 1: Update `examples/echo.rs`**

In `crates/cairn-pty/examples/echo.rs`, find:

```rust
use cairn_pty::{GhosttyPty, PtySession, SpawnOptions};
```

Add `ClientId`:

```rust
use cairn_pty::{ClientId, GhosttyPty, PtySession, SpawnOptions};
```

Find:

```rust
let pty = GhosttyPty::spawn(opts).expect("spawn");
let mut sub = pty.subscribe().await.expect("subscribe");
println!("snapshot length: {}", sub.snapshot.len());
pty.write(bytes::Bytes::from_static(b"echo hello-from-cairn\n"))
    .await
    .expect("write");
```

Change to:

```rust
let pty = GhosttyPty::spawn(opts).expect("spawn");
let client = ClientId::from_u64(0);
let mut sub = pty.subscribe(client).await.expect("subscribe");
println!("snapshot length: {}", sub.snapshot.len());
pty.write(client, bytes::Bytes::from_static(b"echo hello-from-cairn\n"))
    .await
    .expect("write");
```

- [ ] **Step 2: Update `tests/pty_io.rs`**

In `crates/cairn-pty/tests/pty_io.rs`, find the import block near the top:

```rust
use cairn_pty::{GhosttyPty, PtySession, SpawnOptions};
```

(or whatever the exact import line is — adjust accordingly). Add `ClientId`:

```rust
use cairn_pty::{ClientId, GhosttyPty, PtySession, SpawnOptions};
```

Then run a search-and-replace pass. For each call site:

- `pty.subscribe().await` → `pty.subscribe(ClientId::from_u64(0)).await`
- `pty.write(<X>)` → `pty.write(ClientId::from_u64(0), <X>)`
- `pty.resize(<X>)` → `pty.resize(ClientId::from_u64(0), <X>)`

If multiple distinct clients appear in a test (e.g., two subscribers), use different counter values (`from_u64(0)`, `from_u64(1)`) — but the existing integration tests are single-client, so `from_u64(0)` everywhere is fine.

Verify by running: `grep -n 'subscribe\|\.write\|\.resize' crates/cairn-pty/tests/pty_io.rs` — every match should now include a `ClientId::from_u64`.

- [ ] **Step 3: Update `tests/pty_lifecycle.rs`**

Apply the same import update and call-site search-and-replace as Step 2.

- [ ] **Step 4: Update `tests/pty_resize.rs`**

Apply the same import update and call-site search-and-replace as Step 2.

- [ ] **Step 5: Build and run all existing tests**

Run: `nix develop --command cargo test -p cairn-pty`
Expected: all tests pass — both unit tests (including the new ClientId, classifier, NotLeader, subscription tests) and the existing integration tests (`pty_io`, `pty_lifecycle`, `pty_resize`).

If any test fails, it's likely a missed call site. Use `grep -n 'subscribe\|\.write\|\.resize' crates/cairn-pty/tests/*.rs crates/cairn-pty/examples/*.rs` to find anything missed.

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-pty/examples/echo.rs crates/cairn-pty/tests/pty_io.rs crates/cairn-pty/tests/pty_lifecycle.rs crates/cairn-pty/tests/pty_resize.rs
git commit -m "Migrate existing call sites to thread ClientId through"
```

---

### Task 12: Implement election logic in the worker

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs`

- [ ] **Step 1: Add the classifier import**

In `crates/cairn-pty/src/ghostty/worker.rs`, find the existing `use super::process::...` line (around line 20) and add the classifier import next to it:

```rust
use super::Command;
use super::input_classifier::is_user_input;
use super::process::{ChildProcess, Pty};
```

- [ ] **Step 2: Add the election state locals inside `run_session`**

Find the loop-local declarations near the top of `run_session` (just before `let mut buf = vec![0u8; 65536];` around line 382). After the existing locals (terminal, current_size, bcast_tx initialization, etc.), and before `let mut buf`, add:

```rust
let mut leader: Option<ClientId> = None;
let mut last_input_at: Option<std::time::Instant> = None;
```

- [ ] **Step 3: Replace the Resize arm with election logic**

Find the current Resize arm (modified in Task 10 to bind `_client_id`). Replace its entire body with:

```rust
Command::Resize { client_id, size, reply } => {
    // Election: empty seat promotes; non-leader rejects.
    match leader {
        None => {
            leader = Some(client_id);
            tracing::info!(
                target: "cairn_pty::election",
                client_id = %client_id,
                cause = "resize",
                previous = ?None::<ClientId>,
                "leader promoted"
            );
        }
        Some(current) if current == client_id => {
            // Already the leader; apply.
        }
        Some(current) => {
            let _ = reply.send(Err(PtyError::NotLeader {
                requester: client_id,
                current: Some(current),
            }));
            continue;
        }
    }

    let res = (|| -> Result<(), PtyError> {
        terminal
            .borrow_mut()
            .resize(size.cols, size.rows, 0, 0)
            .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
        s.pty
            .set_size(size)
            .map_err(|e| PtyError::Backend { source: Box::new(e) })?;
        Ok(())
    })();
    if res.is_ok() {
        current_size.set(size);
    }
    let _ = reply.send(res);
}
```

- [ ] **Step 4: Replace the Write arm with classifier + election logic**

Find the current Write arm (modified in Task 10 to bind `_client_id`). Replace with:

```rust
Command::Write { client_id, data, reply } => {
    if is_user_input(&data) {
        last_input_at = Some(std::time::Instant::now());
        if leader != Some(client_id) {
            let previous = leader;
            leader = Some(client_id);
            tracing::info!(
                target: "cairn_pty::election",
                client_id = %client_id,
                cause = "input",
                previous = ?previous,
                "leader promoted"
            );
        }
    }
    let res = s.pty.write_all(&data).await.map_err(PtyError::from);
    let _ = reply.send(res);
}
```

- [ ] **Step 5: Replace the Detach arm with vacation logic**

Find the Detach arm (added as a no-op in Task 10). Replace with:

```rust
Command::Detach { client_id } => {
    if leader == Some(client_id) {
        tracing::info!(
            target: "cairn_pty::election",
            client_id = %client_id,
            "leader vacated"
        );
        leader = None;
    }
}
```

- [ ] **Step 6: Silence the unused `last_input_at` warning**

Rust will warn that `last_input_at` is written but never read (it's only used for tracing in a later step). Add an underscore prefix to suppress the warning, OR add a brief tracing emit. Use underscore for now:

Find the line you added in Step 2:

```rust
let mut last_input_at: Option<std::time::Instant> = None;
```

Change to:

```rust
let mut _last_input_at: Option<std::time::Instant> = None;
```

And update the line in the Write arm (Step 4) accordingly:

```rust
_last_input_at = Some(std::time::Instant::now());
```

(We keep the field name with the underscore for now; if/when a future task uses it for tracing or handoff, the underscore can be dropped.)

- [ ] **Step 7: Build to confirm it compiles**

Run: `nix develop --command cargo build -p cairn-pty`
Expected: success.

- [ ] **Step 8: Run all existing tests to confirm no regressions**

Run: `nix develop --command cargo test -p cairn-pty`
Expected: all existing tests pass. (No new election tests yet — those come in Task 13.)

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-pty/src/ghostty/worker.rs
git commit -m "Implement leader election + detach vacation in worker"
```

---

### Task 13: Add MockSession election helpers and election unit tests (part 1: resize)

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs` (the existing `#[cfg(test)] mod tests` block)

- [ ] **Step 1: Add helpers to the `MockSession` struct**

In `crates/cairn-pty/src/ghostty/worker.rs`, find the `MockSession` impl (around line 750). Add these helper methods alongside `feed`, `recv_write`, and `assert_no_write_within`:

```rust
async fn write_as(
    &self,
    client_id: ClientId,
    data: &'static [u8],
) -> Result<(), PtyError> {
    use crate::session::PtySession;
    self.pty.write(client_id, Bytes::from_static(data)).await
}

async fn resize_as(
    &self,
    client_id: ClientId,
    size: TermSize,
) -> Result<(), PtyError> {
    use crate::session::PtySession;
    self.pty.resize(client_id, size).await
}

async fn subscribe_as(
    &self,
    client_id: ClientId,
) -> Result<Subscription, PtyError> {
    use crate::session::PtySession;
    self.pty.subscribe(client_id).await
}
```

- [ ] **Step 2: Add resize-based election tests**

In the same `mod tests` block (after the existing tests), add these tests:

```rust
#[tokio::test]
async fn resize_from_empty_seat_promotes_to_leader() {
    let session = MockSession::new(default_opts());
    let client = ClientId::from_u64(0);
    session
        .resize_as(client, TermSize { cols: 100, rows: 30 })
        .await
        .expect("first resize should succeed and promote");
    // A second resize from the same client must succeed (leader is established).
    session
        .resize_as(client, TermSize { cols: 110, rows: 35 })
        .await
        .expect("leader's subsequent resize should succeed");
}

#[tokio::test]
async fn non_leader_resize_returns_not_leader_error() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    session
        .resize_as(a, TermSize { cols: 100, rows: 30 })
        .await
        .expect("a's resize promotes a to leader");

    let err = session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .expect_err("b is not leader");
    match err {
        PtyError::NotLeader { requester, current } => {
            assert_eq!(requester, b);
            assert_eq!(current, Some(a));
        }
        other => panic!("expected NotLeader, got {other:?}"),
    }
}

#[tokio::test]
async fn leader_vacates_when_subscription_drops() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    let sub_a = session.subscribe_as(a).await.expect("subscribe a");
    session
        .resize_as(a, TermSize { cols: 100, rows: 30 })
        .await
        .expect("a becomes leader");

    drop(sub_a);
    // Give the worker a chance to process the Detach.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // b should now be able to resize (seat is empty, b is promoted).
    session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .expect("b should claim empty seat");
}

#[tokio::test]
async fn non_leader_detach_does_not_clear_leader() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    let _sub_a = session.subscribe_as(a).await.expect("subscribe a");
    let sub_b = session.subscribe_as(b).await.expect("subscribe b");
    session
        .resize_as(a, TermSize { cols: 100, rows: 30 })
        .await
        .expect("a becomes leader");

    drop(sub_b);
    tokio::time::sleep(Duration::from_millis(50)).await;

    // a should still be leader: b's resize attempt fails.
    let err = session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .unwrap_err();
    assert!(matches!(err, PtyError::NotLeader { .. }));
    // and a's own resize still succeeds.
    session
        .resize_as(a, TermSize { cols: 120, rows: 40 })
        .await
        .expect("a still leader after b detaches");
}
```

- [ ] **Step 3: Run the new tests**

Run: `nix develop --command cargo test -p cairn-pty --lib ghostty::worker::tests`
Expected: all tests pass (existing + 4 new).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/src/ghostty/worker.rs
git commit -m "Add MockSession helpers and resize-based election tests"
```

---

### Task 14: Add write-based and classifier-driven election tests

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs` (same test module)

- [ ] **Step 1: Add the tests**

Append to the same `mod tests` block:

```rust
#[tokio::test]
async fn first_user_input_promotes_to_leader() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    // a types — becomes leader.
    session.write_as(a, b"hello").await.expect("a writes");

    // b's resize must fail because a is leader.
    let err = session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .unwrap_err();
    assert!(matches!(err, PtyError::NotLeader { current: Some(_), .. }));
}

#[tokio::test]
async fn most_recent_user_input_steals_leader() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    session.write_as(a, b"hello").await.expect("a writes");
    session.write_as(b, b"world").await.expect("b writes");

    // b is now leader; a's resize must fail.
    let err = session
        .resize_as(a, TermSize { cols: 110, rows: 35 })
        .await
        .unwrap_err();
    match err {
        PtyError::NotLeader { requester, current } => {
            assert_eq!(requester, a);
            assert_eq!(current, Some(b));
        }
        other => panic!("expected NotLeader, got {other:?}"),
    }
}

#[tokio::test]
async fn mouse_click_promotes() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    // a sends an SGR mouse press — should promote.
    session
        .write_as(a, b"\x1b[<0;10;20M")
        .await
        .expect("mouse press");

    let err = session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .unwrap_err();
    assert!(matches!(err, PtyError::NotLeader { .. }));
}

#[tokio::test]
async fn mouse_motion_does_not_promote() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    // a sends mouse motion only (button 35 = motion + no button held).
    session
        .write_as(a, b"\x1b[<35;10;20M")
        .await
        .expect("mouse motion");

    // No leader was established; b's resize should succeed (empty seat).
    session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .expect("b claims empty seat");
}

#[tokio::test]
async fn focus_event_does_not_promote() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    session
        .write_as(a, b"\x1b[I")
        .await
        .expect("focus in");
    session
        .write_as(a, b"\x1b[O")
        .await
        .expect("focus out");

    // No leader: b can claim the seat.
    session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .expect("b claims empty seat");
}

#[tokio::test]
async fn da_reply_passthrough_does_not_promote() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    // a sends what looks like a DA1 reply — should NOT promote.
    session
        .write_as(a, b"\x1b[?62;22c")
        .await
        .expect("DA reply");

    session
        .resize_as(b, TermSize { cols: 110, rows: 35 })
        .await
        .expect("b claims empty seat");
}
```

- [ ] **Step 2: Run the new tests**

Run: `nix develop --command cargo test -p cairn-pty --lib ghostty::worker::tests`
Expected: all tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/src/ghostty/worker.rs
git commit -m "Add write-based and classifier-driven election tests"
```

---

### Task 15: Add tracing-based assertion test

**Files:**
- Modify: `crates/cairn-pty/src/ghostty/worker.rs` (same test module)

- [ ] **Step 1: Add the tracing test**

Append to the same `mod tests` block:

```rust
#[tokio::test]
#[tracing_test::traced_test]
async fn leader_input_after_promotion_does_not_re_emit_event() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);

    // First write promotes.
    session.write_as(a, b"hello").await.expect("write 1");
    // Second write from same client should NOT emit a new promotion event.
    session.write_as(a, b"world").await.expect("write 2");

    // Count promotion log lines mentioning client_id=1 (ClientId::from_u64(0) Display value).
    let logs = logs_contain("leader promoted");
    assert!(logs, "promotion event should fire at least once");

    // Capture all log lines and ensure only ONE promotion line exists.
    // tracing_test exposes `logs_assert` for richer queries.
    tracing_test::internal::logs_assert(|lines| {
        let promotions = lines
            .iter()
            .filter(|l| l.contains("leader promoted"))
            .count();
        if promotions == 1 {
            Ok(())
        } else {
            Err(format!("expected 1 promotion event, got {promotions}"))
        }
    });
}

#[tokio::test]
#[tracing_test::traced_test]
async fn leader_vacation_emits_event() {
    let session = MockSession::new(default_opts());
    let a = ClientId::from_u64(0);

    let sub_a = session.subscribe_as(a).await.expect("subscribe a");
    session.write_as(a, b"x").await.expect("a becomes leader");

    drop(sub_a);
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(
        logs_contain("leader vacated"),
        "vacation event should have been emitted"
    );
}
```

Note on the API: `tracing_test::traced_test` macro injects a subscriber. `logs_contain(substr)` returns a `bool`. `tracing_test::internal::logs_assert(closure)` runs a custom assertion. If the `internal` module is not exposed in your `tracing-test` version, fall back to manually counting using a `Vec` collected through `logs_contain`. Adjust syntax if the version's API differs; the spec's intent is captured by the assertion semantics, not the exact function name.

- [ ] **Step 2: Run the new tests**

Run: `nix develop --command cargo test -p cairn-pty --lib ghostty::worker::tests::leader_input_after_promotion_does_not_re_emit_event`
Run: `nix develop --command cargo test -p cairn-pty --lib ghostty::worker::tests::leader_vacation_emits_event`
Expected: both pass. If `tracing_test::internal::logs_assert` isn't available, replace with a simpler `assert!(logs_contain("leader promoted"))` and rely on visual inspection / a fixed count via `logs_contain` iterations.

- [ ] **Step 3: Run all tests once more to confirm nothing regresses**

Run: `nix develop --command cargo test -p cairn-pty`
Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/src/ghostty/worker.rs
git commit -m "Add tracing-test assertions for election events"
```

---

### Task 16: Add real-PTY integration test

**Files:**
- Create: `crates/cairn-pty/tests/pty_multi_client.rs`

- [ ] **Step 1: Write the integration test file**

Create `crates/cairn-pty/tests/pty_multi_client.rs`:

```rust
//! Integration test for multi-client election against a real PTY.
//!
//! Drives a real `/bin/cat` through the production worker path and
//! verifies that ClientId-aware resize, leader election, and detach
//! work end-to-end. The bulk of correctness lives in the mock-driven
//! worker tests (`src/ghostty/worker.rs::tests`); this test guards
//! against breakage at the real-PTY layer (kernel scheduling, actual
//! tokio::process::Child interactions, etc.).

use std::time::Duration;

use bytes::Bytes;
use cairn_pty::{ClientId, GhosttyPty, PtyError, PtySession, SpawnOptions, TermSize};

#[tokio::test]
async fn two_clients_resize_election_against_real_pty() {
    let mut cmd = tokio::process::Command::new("/bin/cat");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    let _sub_a = pty.subscribe(a).await.expect("subscribe a");
    pty.resize(a, TermSize { cols: 100, rows: 30 })
        .await
        .expect("a's first resize promotes a to leader");

    let _sub_b = pty.subscribe(b).await.expect("subscribe b");
    let err = pty
        .resize(b, TermSize { cols: 120, rows: 40 })
        .await
        .expect_err("b is not leader");
    match err {
        PtyError::NotLeader { requester, current } => {
            assert_eq!(requester, b);
            assert_eq!(current, Some(a));
        }
        other => panic!("expected NotLeader, got {other:?}"),
    }

    // b types — claims leadership.
    pty.write(b, Bytes::from_static(b"hello"))
        .await
        .expect("b writes");
    // Now b can resize.
    pty.resize(b, TermSize { cols: 120, rows: 40 })
        .await
        .expect("b should now be leader");

    pty.kill().expect("kill");
}

#[tokio::test]
async fn leader_seat_clears_on_subscription_drop_against_real_pty() {
    let cmd = tokio::process::Command::new("/bin/cat");
    let pty = GhosttyPty::spawn(SpawnOptions::new(cmd)).expect("spawn");

    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    let sub_a = pty.subscribe(a).await.expect("subscribe a");
    pty.resize(a, TermSize { cols: 100, rows: 30 })
        .await
        .expect("a is leader");

    drop(sub_a);
    // Give worker a moment to process Detach.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // b can now claim the empty seat.
    pty.resize(b, TermSize { cols: 110, rows: 35 })
        .await
        .expect("b claims empty seat");

    pty.kill().expect("kill");
}
```

- [ ] **Step 2: Run the integration tests**

Run: `nix develop --command cargo test -p cairn-pty --test pty_multi_client`
Expected: 2 tests pass.

If the tests fail with PTY allocation errors on the test runner, that's environment-related, not a code bug — try once more, and if reproducible, document the environment requirement (linux/macOS with PTY support).

- [ ] **Step 3: Run the entire test suite one final time**

Run: `nix develop --command cargo test -p cairn-pty`
Expected: all tests pass — unit (client_id, error, classifier, subscription, worker), integration (`pty_io`, `pty_lifecycle`, `pty_resize`, `pty_multi_client`).

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/tests/pty_multi_client.rs
git commit -m "Add real-PTY integration test for multi-client election"
```

---

### Task 17: Final sweep — clippy, fmt, doc check

**Files:** All modified above.

- [ ] **Step 1: Run clippy**

Run: `nix develop --command cargo clippy -p cairn-pty --all-targets -- -D warnings`
Expected: no warnings.

If warnings appear, fix them inline (most likely candidates: unused variable warnings on `_last_input_at`, unused imports in test modules).

- [ ] **Step 2: Run rustfmt**

Run: `nix develop --command cargo fmt -p cairn-pty`
Expected: no changes (or auto-fixes formatting). If changes, review them and commit as part of the final commit below.

- [ ] **Step 3: Build docs**

Run: `nix develop --command cargo doc -p cairn-pty --no-deps`
Expected: docs build with no warnings.

- [ ] **Step 4: Commit any final fixes**

```bash
git add -A crates/cairn-pty/
git diff --cached --stat
git commit -m "Final cleanup: clippy + fmt + doc fixes" || echo "nothing to commit"
```

(The `|| echo "nothing to commit"` handles the case where the previous steps left nothing to do.)

---

## Acceptance criteria

When this plan is complete:

- `cargo test -p cairn-pty` passes all tests (existing + new).
- `cargo clippy -p cairn-pty --all-targets -- -D warnings` succeeds.
- The `PtySession` trait requires a `ClientId` argument on `subscribe`, `resize`, and `write`.
- A non-leader's `resize` call returns `PtyError::NotLeader { requester, current }`.
- The first `write` with bytes that pass `is_user_input` promotes the writer to leader; subsequent qualifying writes from other clients steal the seat.
- The first `resize` while no leader exists promotes the resizer.
- Dropping a `Subscription` while the dropper is the leader vacates the seat; non-leader detaches do not.
- Mouse motion, focus events, and DA replies do not promote, but mouse clicks/scrolls/drags do.
- `tracing` events fire on every promotion and vacation under the `cairn_pty::election` target.
- `git log --oneline` shows one commit per task (~17 commits, each a small reviewable change).

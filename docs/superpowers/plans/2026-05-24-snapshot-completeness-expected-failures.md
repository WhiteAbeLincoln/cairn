# Snapshot Completeness Expected-Failure Tests Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pin every distinct failure mode of cairn's current `format_snapshot` as a checked-in integration test, with no production-code changes. The tests live in a new file `crates/cairn-pty/tests/snapshot_completeness.rs`; the only Cargo change is one `[dev-dependencies]` line.

**Architecture:** New external integration test file alongside existing `pty_io.rs`, `pty_lifecycle.rs`, etc. A small harness in the same file spawns a raw-mode PTY child (a shell that runs `stty` to disable canonical mode / echo / output post-processing, then `exec cat`), so writes to the session arrive at the embedded emulator exactly as sent — once each, no `\r\n` translation, no kernel ECHO duplication. Each test writes a canned VT setup, waits for a printable sentinel to echo back (proving the emulator has absorbed the setup), drops its setup-watcher subscription, resubscribes to capture the post-setup snapshot, replays the snapshot into a fresh `libghostty_vt::Terminal`, and asserts receiver state. Most assertions panic today and are wrapped in `#[should_panic(expected = "<unique substring>")]`; one is a "tripwire" that passes today but is designed to fail under a partial fix.

**Tech Stack:** Rust, tokio integration tests (`#[tokio::test]`), `libghostty-vt` 0.1.1 (added as dev-dep), `bytes`, `flume`, existing `cairn-pty` public API.

**Spec:** `docs/superpowers/specs/2026-05-24-snapshot-completeness-expected-failures-design.md`.

---

## Conventions used throughout this plan

- **Run from repo root** (`/Users/abe/Projects/cairn`) unless stated.
- **Cargo invocation:** `nix develop --command cargo nextest run ...` — `cargo-nextest` is in the dev shell (see `flake.nix:86` toolchain), and the user's CLAUDE.md mandates nextest over `cargo test` where configured. For builds (no test run), use `nix develop --command cargo build`.
- **Test scope:** `cargo nextest run -p cairn-pty --test snapshot_completeness` runs only this new integration test file. Individual tests can be selected with `--test snapshot_completeness <substring>`.
- **TDD discipline:** each test task starts by writing the test, then runs it to verify it produces the expected outcome (panic-with-expected-substring for broken-today tests; clean pass for tripwire tests), then commits.
- **Commit style:** small, focused. Conventional prefixes (`test:`, `chore:`) consistent with the existing repo history.
- **No production code changes** — `crates/cairn-pty/src/` must remain untouched at the end of this plan.
- **The single harness rebuild from Task 1 is reused by every subsequent task.** Tasks 2-14 do not modify the harness; they only add tests.

---

### Task 1: Add `libghostty-vt` dev-dep, scaffold the test file with full harness, and implement test #1 (bracketed paste)

**Files:**
- Modify: `crates/cairn-pty/Cargo.toml`
- Create: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Add `libghostty-vt` to `[dev-dependencies]`**

Open `crates/cairn-pty/Cargo.toml`. The current `[dev-dependencies]` block is:

```toml
[dev-dependencies]
tokio = { version = "1.52", features = ["full", "test-util", "macros"] }
tracing-test = "0.2"
```

Append one line so it becomes:

```toml
[dev-dependencies]
tokio = { version = "1.52", features = ["full", "test-util", "macros"] }
tracing-test = "0.2"
libghostty-vt = "0.1.1"
```

`libghostty-vt` is already a regular dependency of `cairn-pty` (`crates/cairn-pty/Cargo.toml:14`); duplicating it as a dev-dep lets external integration tests in `tests/` use `libghostty_vt::Terminal` directly without forcing a re-export from cairn-pty's public API.

- [ ] **Step 2: Verify the workspace still builds**

Run: `nix develop --command cargo build -p cairn-pty`
Expected: builds cleanly. Cargo will note that `libghostty-vt` resolves to the same version already in the lockfile, so this is a metadata-only change.

- [ ] **Step 3: Create the test file with the harness module and test #1**

Create `crates/cairn-pty/tests/snapshot_completeness.rs` with the following content:

```rust
//! Expected-to-fail tests pinning every distinct way cairn's current
//! `format_snapshot` loses state that a freshly-attached client needs to
//! reconstruct the source emulator faithfully.
//!
//! Each test sets up a piece of state on a real cairn session, takes a
//! snapshot through the public `Subscription` API, replays it into a fresh
//! `libghostty_vt::Terminal`, and asserts the receiver matches the source.
//!
//! Most tests fail today; those are wrapped in
//! `#[should_panic(expected = "<unique substring>")]` so CI is green while
//! the gap is open, and start failing the moment the gap closes. A small
//! number of tests are "tripwires": they pass today but assert behavior
//! that a partial fix would silently regress, and they omit
//! `#[should_panic]` so the regression surfaces immediately.
//!
//! See docs/superpowers/specs/2026-05-24-snapshot-completeness-expected-failures-design.md
//! for the full taxonomy and the future-fix flip workflow.

use bytes::Bytes;
use cairn_pty::{ClientId, GhosttyPty, PtySession, SpawnOptions, Subscription, TermSize};
use libghostty_vt::terminal::Mode;
use libghostty_vt::{Terminal, TerminalOptions};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

// ─── Harness ────────────────────────────────────────────────────────────────

const COLS: u16 = 80;
const ROWS: u16 = 24;
const READ_DEADLINE: Duration = Duration::from_secs(5);
const READY_MARKER: &[u8] = b"__READY__";

/// Spawn a session whose slave PTY is in raw mode: no canonical line
/// buffering, no input echo, no output post-processing. This guarantees
/// the inner emulator sees exactly the bytes we write via `pty.write()` —
/// once each, no `\r\n` translation, no duplicate kernel-ECHO copy.
///
/// The shell script prints `__READY__` *after* `stty` has taken effect; we
/// subscribe and wait for that marker before returning so subsequent writes
/// don't race with terminal-mode setup.
async fn spawn_raw_session() -> GhosttyPty {
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args([
        "-c",
        "stty -icanon -echo -opost -icrnl 2>/dev/null; printf '__READY__'; exec cat",
    ]);
    let opts = SpawnOptions::new(cmd).with_size(TermSize { cols: COLS, rows: ROWS });
    let pty = GhosttyPty::spawn(opts).expect("spawn raw cat session");

    let mut sub = pty
        .subscribe(ClientId::from_u64(0))
        .await
        .expect("subscribe for ready marker");
    let _ = read_until_contains(&mut sub, READY_MARKER, READ_DEADLINE).await;
    drop(sub);
    pty
}

/// Drain `sub.snapshot` and subsequent `sub.stream` chunks until the
/// accumulated bytes contain `needle` or the deadline elapses. Same shape
/// as the helper in `tests/pty_io.rs`; duplicated rather than shared because
/// Cargo integration tests are separate compilation units.
async fn read_until_contains(
    sub: &mut Subscription,
    needle: &[u8],
    deadline: Duration,
) -> Vec<u8> {
    let mut acc = sub.snapshot.to_vec();
    if windows_contain(&acc, needle) {
        return acc;
    }
    let read = async {
        loop {
            match sub.stream.recv().await {
                Ok(chunk) => {
                    acc.extend_from_slice(&chunk);
                    if windows_contain(&acc, needle) {
                        return acc;
                    }
                }
                Err(RecvError::Closed) => return acc,
                Err(RecvError::Lagged(_)) => continue,
            }
        }
    };
    tokio::time::timeout(deadline, read).await.unwrap_or_default()
}

fn windows_contain(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Subscribe (`sub1`), write `setup` bytes, wait for `sentinel` to echo back
/// — by which point the emulator has absorbed every preceding byte too —
/// then drop `sub1` and return a fresh `sub2` whose `.snapshot` reflects
/// post-setup state.
///
/// `setup` must contain `sentinel` somewhere near the end so "we saw sentinel"
/// implies "the emulator has processed everything we sent".
async fn write_setup_and_resubscribe(
    pty: &GhosttyPty,
    setup: &[u8],
    sentinel: &[u8],
) -> Subscription {
    let client = ClientId::from_u64(0);
    let mut sub = pty.subscribe(client).await.expect("subscribe pre-setup");
    pty.write(client, Bytes::copy_from_slice(setup))
        .await
        .expect("write setup bytes");
    let _ = read_until_contains(&mut sub, sentinel, READ_DEADLINE).await;
    drop(sub);
    pty.subscribe(client).await.expect("resubscribe post-setup")
}

/// Build a fresh receiver `Terminal` and feed it the snapshot bytes. This is
/// the consumer side of cairn's snapshot contract — the same VT emulator core
/// that the eventual cairn ghostty-web client will use to replay snapshots.
fn replay_into_receiver(snapshot: &Bytes) -> Terminal<'static, 'static> {
    let mut term = Terminal::new(TerminalOptions {
        cols: COLS,
        rows: ROWS,
        max_scrollback: 1000,
    })
    .expect("construct receiver Terminal");
    term.vt_write(snapshot);
    term
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[tokio::test]
#[should_panic(expected = "bracketed paste not preserved")]
async fn snapshot_preserves_bracketed_paste_mode() {
    // Failure mode: DECSET 2004 (bracketed paste) is not preserved across
    //   snapshot.
    // Impact: editors/REPLs depend on bracketed paste to distinguish typed
    //   input from pasted input; without it, paste delivers raw keystrokes
    //   that may trigger keybindings (e.g., `:` opening vim's command line
    //   for every colon in a pasted block).
    // Why this fails today: `format_snapshot` runs the libghostty formatter
    //   with default `FormatterTerminalExtra` (all flags false). The `modes`
    //   flag, which emits DECSET sequences for non-default modes, is off, so
    //   the snapshot never contains `\x1b[?2004h`.

    let pty = spawn_raw_session().await;
    let sub = write_setup_and_resubscribe(&pty, b"\x1b[?2004h_BP_SENT_", b"_BP_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    assert!(
        receiver.mode(Mode::BRACKETED_PASTE).expect("mode query"),
        "bracketed paste not preserved",
    );
}
```

- [ ] **Step 4: Run the new test and verify it currently panics with the expected substring**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_bracketed_paste_mode`
Expected: PASS — `#[should_panic(expected = "bracketed paste not preserved")]` catches the assertion panic.

If the test instead **fails** with "test did not panic", the gap isn't actually present with current defaults — drop the `#[should_panic(...)]` line, re-run, expect PASS. (This reclassifies the test as a "tripwire" per the spec; it's a fine outcome.)

If the test fails with "panic did not contain expected string", the panic message mismatched — check that the `assert!`'s message literal matches the `expected = "..."` substring exactly.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-pty/Cargo.toml crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "$(cat <<'EOF'
test: scaffold snapshot-completeness tests with raw-mode harness

Pin gap #1 (bracketed paste) and add the harness that the remaining
13 tests will reuse. Raw-mode shell child (stty -icanon -echo -opost
-icrnl; exec cat) guarantees the inner emulator sees exactly the
bytes we write, so tests can craft cursor / mode / charset state
without fighting the kernel's line discipline.
EOF
)"
```

---

### Task 2: Test #2 — application cursor keys (DECCKM)

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test to the file**

Add the following at the bottom of `crates/cairn-pty/tests/snapshot_completeness.rs`:

```rust
#[tokio::test]
#[should_panic(expected = "application cursor keys mode not preserved")]
async fn snapshot_preserves_application_cursor_keys() {
    // Failure mode: DECSET 1 (DECCKM, application cursor keys) is not
    //   preserved across snapshot.
    // Impact: DECCKM controls whether the arrow keys send `\x1bOA`/`B`/`C`/`D`
    //   (application mode) or `\x1b[A`/`B`/`C`/`D` (normal mode). TUIs like
    //   vim and readline-based shells switch into application mode on entry;
    //   if a client attaches afterward, arrow keys send the wrong sequences
    //   and cursor movement misbehaves.
    // Why this fails today: same as #1 — `extra.modes` is off so the
    //   snapshot emits no DECSET sequence.

    let pty = spawn_raw_session().await;
    let sub = write_setup_and_resubscribe(&pty, b"\x1b[?1h_DECCKM_SENT_", b"_DECCKM_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    assert!(
        receiver.mode(Mode::DECCKM).expect("mode query"),
        "application cursor keys mode not preserved",
    );
}
```

- [ ] **Step 2: Run the test and verify expected-panic**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_application_cursor_keys`
Expected: PASS via `#[should_panic]`.

If "test did not panic": drop the `#[should_panic]` line — gap closed by current defaults.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin DECCKM gap in snapshot completeness suite"
```

---

### Task 3: Test #3 — focus event mode

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "focus event mode not preserved")]
async fn snapshot_preserves_focus_event_mode() {
    // Failure mode: DECSET 1004 (focus events) is not preserved across
    //   snapshot.
    // Impact: programs that watch for FocusIn / FocusOut (`\x1b[I` / `\x1b[O`)
    //   — e.g., vim's `:autoread`, tmux session activity tracking — miss
    //   focus transitions on the reattaching client because mode 1004 is
    //   not active on the receiver.
    // Why this fails today: same as #1 — `extra.modes` is off.

    let pty = spawn_raw_session().await;
    let sub = write_setup_and_resubscribe(&pty, b"\x1b[?1004h_FOCUS_SENT_", b"_FOCUS_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    assert!(
        receiver.mode(Mode::FOCUS_EVENT).expect("mode query"),
        "focus event mode not preserved",
    );
}
```

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_focus_event_mode`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin focus-event-mode gap in snapshot completeness suite"
```

---

### Task 4: Test #4 — alt-screen active at snapshot time

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "alt screen not active on receiver")]
async fn snapshot_preserves_alt_screen_when_active() {
    // Failure mode: when the source is in the alternate screen (DECSET
    //   1049) at snapshot time, the snapshot does NOT switch the receiver
    //   into the alt screen. The alt-screen content is rendered onto the
    //   receiver's MAIN screen instead, irreversibly corrupting it.
    // Impact: catastrophic for the common case of "vim / htop is running
    //   when a new client attaches" — the receiver's main-screen buffer
    //   gets overwritten with the alt-screen rendering, and when the
    //   program later exits alt-screen (DECRST 1049) on the live stream,
    //   the receiver swaps to a now-empty main screen (no shell scrollback,
    //   no prompt).
    // Why this fails today: `extra.modes` is off, so the snapshot never
    //   contains `\x1b[?1049h`. The receiver stays on its main screen.

    let pty = spawn_raw_session().await;
    let sub = write_setup_and_resubscribe(
        &pty,
        b"\x1b[?1049hALT_VISIBLE_SENT_",
        b"ALT_VISIBLE_SENT_",
    )
    .await;
    let receiver = replay_into_receiver(&sub.snapshot);

    use libghostty_vt::ffi::GhosttyTerminalScreen_GHOSTTY_TERMINAL_SCREEN_ALTERNATE as ALTERNATE;
    assert_eq!(
        receiver.active_screen().expect("active screen"),
        ALTERNATE,
        "alt screen not active on receiver",
    );
}
```

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_alt_screen_when_active`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin alt-screen-active gap in snapshot completeness suite"
```

---

### Task 5: Test #5 — alt-screen content does not leak after exit

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "alt-screen content leaked into receiver main screen")]
async fn snapshot_does_not_leak_alt_screen_content_after_exit() {
    // Failure mode: source enters alt screen, writes something, exits alt
    //   screen, writes something on main. Snapshot may either (a) include
    //   alt-screen content as if it were main-screen content, or (b)
    //   re-emit it via a partial DECSET cycle, leaving a stale ALT_MARK
    //   on the receiver's main buffer.
    // Impact: scrollback / main-buffer content visible on the receiver
    //   includes data the source never had on its main screen — a
    //   correctness violation visible to the user as ghost text.
    // Why this fails today: depends on what the current default formatter
    //   emits when the source has both screens populated. Mirrors zmx's
    //   round-trip test at `zmx/src/util.zig:1190-1209`.

    let pty = spawn_raw_session().await;
    let sub = write_setup_and_resubscribe(
        &pty,
        b"\x1b[?1049hALT_MARK\x1b[?1049lMAIN_MARK_SENT_",
        b"MAIN_MARK_SENT_",
    )
    .await;
    let receiver = replay_into_receiver(&sub.snapshot);

    use libghostty_vt::ffi::GhosttyTerminalScreen_GHOSTTY_TERMINAL_SCREEN_PRIMARY as PRIMARY;
    assert_eq!(
        receiver.active_screen().expect("active screen"),
        PRIMARY,
        "receiver not on main screen after alt cycle",
    );

    // Scan the receiver's visible viewport for ALT_MARK. If found, that
    // content has leaked from the source's alt buffer onto the receiver's
    // main buffer.
    let needle = b"ALT_MARK";
    let mut leaked = false;
    for y in 0..ROWS {
        for x in 0..(COLS - needle.len() as u16) {
            let mut window = Vec::with_capacity(needle.len());
            for dx in 0..needle.len() as u16 {
                let p = libghostty_vt::terminal::Point::Viewport(
                    libghostty_vt::terminal::PointCoordinate { x: x + dx, y },
                );
                let r = match receiver.grid_ref(p) {
                    Ok(r) => r,
                    Err(_) => break,
                };
                let cell = match r.cell() {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let cp = cell.codepoint().unwrap_or(0);
                window.push(cp as u8);
            }
            if window.as_slice() == needle {
                leaked = true;
                break;
            }
        }
        if leaked {
            break;
        }
    }
    assert!(
        !leaked,
        "alt-screen content leaked into receiver main screen",
    );
}
```

Notes:
- This test imports `libghostty_vt::terminal::{Point, PointCoordinate}` inline. If those names differ in the safe wrapper, adjust to match — they should be at `libghostty_vt::terminal::Point` per `terminal.rs:388`.
- The viewport scan is `O(rows * cols * needle_len)` ≈ 24 × 72 × 8 ≈ 14K reads, well within tolerance for a one-shot test.

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_does_not_leak_alt_screen_content_after_exit`
Expected: PASS via `#[should_panic]`. If "test did not panic", current defaults already isolate the screens; drop `#[should_panic]` — reclassifies as a tripwire which is still useful (would catch a future regression that *introduces* the leak).

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin alt-screen-leak gap in snapshot completeness suite"
```

---

### Task 6: Test #6 — cursor position preserved

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "cursor position not preserved")]
async fn snapshot_preserves_cursor_position() {
    // Failure mode: CUP (`\x1b[<row>;<col>H`) is not preserved across
    //   snapshot. Receiver's cursor lands wherever the printed content
    //   ended, not at the source's saved CUP position.
    // Impact: next keystroke from the reattaching client renders at the
    //   wrong column — visible artifact in shell prompts mid-edit, in
    //   readline command-line builders, and anywhere a TUI relies on
    //   "current cursor is here".
    // Why this fails today: `extra.screen.cursor` is off, so the snapshot
    //   emits no CUP at all. Receiver cursor is wherever the last cell
    //   write left it.
    //
    // Setup design: `hello`, advance two lines, CUP to (10, 20) (1-indexed),
    //   print sentinel `*`, then `\b` so the source cursor lands at (9, 4)
    //   when expressed as 0-indexed (col 19, row 9) — note: cursor_x is
    //   COLUMN (0-indexed), cursor_y is ROW (0-indexed). `\b` rolls back
    //   one column; without the sentinel + `\b` trick we'd have no
    //   echo-able byte to wait on, so we synchronize then rewind.

    let pty = spawn_raw_session().await;
    let setup = b"hello\r\n\x1b[10;20H*\x08";
    let sub = write_setup_and_resubscribe(&pty, setup, b"*").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    let cx = receiver.cursor_x().expect("cursor_x");
    let cy = receiver.cursor_y().expect("cursor_y");
    assert!(
        cx == 19 && cy == 9,
        "cursor position not preserved (expected (19, 9), got ({cx}, {cy}))",
    );
}
```

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_cursor_position`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin cursor-position gap in snapshot completeness suite"
```

---

### Task 7: Test #7 — current SGR style preserved

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "current SGR style not preserved")]
async fn snapshot_preserves_current_sgr_style() {
    // Failure mode: the SGR attributes active at the source cursor (bold,
    //   red, italic, …) are not preserved across snapshot. Receiver's
    //   cursor sits with default style, so the next character printed
    //   renders in default attributes instead of inheriting the active
    //   style.
    // Impact: visible artifact in shells where a colored prompt segment
    //   ends with the cursor positioned mid-segment — next keystrokes
    //   render in white-on-black instead of the intended color, and stay
    //   that way until the program issues a fresh SGR.
    // Why this fails today: `extra.screen.style` is off; the snapshot
    //   emits no SGR-restoration sequence for the cursor.

    let pty = spawn_raw_session().await;
    // Bold + red FG, cursor home, sentinel (overwrites READY at top-left),
    // then `\b` to leave cursor adjacent to the styled sentinel.
    let setup = b"\x1b[1;31m\x1b[H_SGR_SENT_\x08";
    let sub = write_setup_and_resubscribe(&pty, setup, b"_SGR_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    let style = receiver.cursor_style().expect("cursor_style");

    use libghostty_vt::style::{PaletteIndex, StyleColor};
    let is_bold_red = style.bold && style.fg_color == StyleColor::Palette(PaletteIndex::RED);
    assert!(
        is_bold_red,
        "current SGR style not preserved (bold={}, fg_color={:?})",
        style.bold,
        style.fg_color,
    );
}
```

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_current_sgr_style`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin current-SGR-style gap in snapshot completeness suite"
```

---

### Task 8: Test #8 — active hyperlink at cursor preserved

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "active hyperlink not preserved")]
async fn snapshot_preserves_active_hyperlink() {
    // Failure mode: an OSC 8 hyperlink open mid-stream (no closing
    //   `\x1b]8;;\x1b\\`) at snapshot time is not preserved. Cells printed
    //   to the receiver after attach lose their hyperlink annotation.
    // Impact: link-aware terminals show the linked text as plain text
    //   after attach; clicks no longer open the URL.
    // Why this fails today: `extra.screen.hyperlink` is off and
    //   `extra.modes` (which would emit any required mode state) is also
    //   off. The snapshot emits no OSC 8 sequence to re-open the link.
    //
    // Readback: we print a sentinel character INSIDE the open link and
    // inspect that cell on the receiver via grid_ref → cell.has_hyperlink().

    let pty = spawn_raw_session().await;
    // CUP home, open hyperlink, print one char inside the link, leave the
    // link open. `LINK_SENT_` is used as the readback marker.
    let setup = b"\x1b[H\x1b]8;;https://example.com\x1b\\LINK_SENT_";
    let sub = write_setup_and_resubscribe(&pty, setup, b"LINK_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);

    // The sentinel was printed from (0, 0). Inspect cell (0, 0) — its
    // codepoint should be 'L' and it should carry a hyperlink.
    let p = libghostty_vt::terminal::Point::Viewport(
        libghostty_vt::terminal::PointCoordinate { x: 0, y: 0 },
    );
    let gref = receiver.grid_ref(p).expect("grid_ref");
    let cell = gref.cell().expect("cell");
    let has_link = cell.has_hyperlink().unwrap_or(false);
    assert!(
        has_link,
        "active hyperlink not preserved (cell at 0,0 has_hyperlink={has_link})",
    );
}
```

Notes on uncertain readback: if `cell.has_hyperlink()` always returns `false` against a vt-only stream (e.g., libghostty only tracks hyperlinks via its own state machine that the snapshot bypasses), this test may need to fall back to byte-pattern matching on `&sub.snapshot` for the literal `\x1b]8;;https://example.com\x1b\\` sequence. Switch the readback only if `cell.has_hyperlink()` is structurally unable to answer for the receiver; keep the same panic message.

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_active_hyperlink`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin active-hyperlink gap in snapshot completeness suite"
```

---

### Task 9: Test #9 — working directory (OSC 7) preserved

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "working directory not preserved")]
async fn snapshot_preserves_working_directory() {
    // Failure mode: OSC 7 (working-directory hint) is not preserved across
    //   snapshot. Receiver's `pwd()` returns an empty string regardless of
    //   what the source set.
    // Impact: terminal integrations that use OSC 7 — "new tab here",
    //   prompt PWD display, file-drop relative-path resolution — fail
    //   silently on attach.
    // Why this fails today: `extra.pwd` is off.

    let pty = spawn_raw_session().await;
    let setup = b"\x1b]7;file:///home/abe/projects\x1b\\_PWD_SENT_";
    let sub = write_setup_and_resubscribe(&pty, setup, b"_PWD_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    let pwd = receiver.pwd().expect("pwd query");
    assert!(
        pwd == "/home/abe/projects",
        "working directory not preserved (got {pwd:?})",
    );
}
```

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_working_directory`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin OSC-7 PWD gap in snapshot completeness suite"
```

---

### Task 10: Test #10 — scrolling region (DECSTBM) preserved

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "scrolling region not preserved")]
async fn snapshot_preserves_scrolling_region() {
    // Failure mode: DECSTBM (`\x1b[<top>;<bot>r`) — the top/bottom scroll
    //   margins — is not preserved across snapshot. Receiver has the
    //   default full-screen scroll region regardless of source state.
    // Impact: TUIs that set a scroll region for a "status bar at bottom"
    //   layout (e.g., older mc/htop variants) clobber the status row on
    //   the receiver — content scrolls past it instead of being held
    //   below the region.
    // Why this fails today: `extra.scrolling_region` is off.
    //
    // Readback strategy: libghostty-vt 0.1.1's safe API doesn't expose
    // the active scroll region directly. We probe behaviorally — print
    // `(rows - 5)` newlines after positioning inside what *should* be
    // the scroll region. If the region is honored, content scrolls within
    // rows 5..=20; if not, content scrolls across the whole screen and
    // line 21+ shows up. After replay, we check that the source's setup
    // sequence's effects survived by reading the cell at row 4 col 0 —
    // it should be empty because the scroll region top is at row 5.

    let pty = spawn_raw_session().await;
    // DECSTBM(5,20), then print sentinel at row 5 col 0 via CUP, then go
    // to the bottom of the region and emit several LF to force scrolling
    // inside the region. After the scroll, the cell at row 4 col 0
    // (outside the region) MUST still be the post-READY space if the
    // region was honored.
    let setup = b"\x1b[5;20r\x1b[5;1H_REGION_SENT_\x1b[20;1H\n\n\n\n\n";
    let sub = write_setup_and_resubscribe(&pty, setup, b"_REGION_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);

    // If the snapshot preserved DECSTBM, replaying it onto the receiver
    // will replay the scroll region too. We then issue ONE more LF
    // through the receiver and watch where the scroll happens: with the
    // region honored, row 4 stays untouched. Probe by writing an LF and
    // re-checking via grid_ref that row 4 col 0 is still its original
    // codepoint.
    let p_before = libghostty_vt::terminal::Point::Viewport(
        libghostty_vt::terminal::PointCoordinate { x: 0, y: 3 },
    );
    let before_cp = receiver
        .grid_ref(p_before)
        .expect("grid_ref before")
        .cell()
        .expect("cell before")
        .codepoint()
        .unwrap_or(0);

    // We can't mutate the receiver after construction here without &mut,
    // so make a fresh receiver and re-feed the snapshot followed by a
    // single CUP+LF probe to force-scroll inside-or-outside the region.
    let mut probe = libghostty_vt::Terminal::new(libghostty_vt::TerminalOptions {
        cols: COLS,
        rows: ROWS,
        max_scrollback: 1000,
    })
    .expect("probe receiver");
    probe.vt_write(&sub.snapshot);
    // Position at bottom of intended region and emit a scroll-trigger LF.
    probe.vt_write(b"\x1b[20;1H\n");
    let after_cp = probe
        .grid_ref(libghostty_vt::terminal::Point::Viewport(
            libghostty_vt::terminal::PointCoordinate { x: 0, y: 3 },
        ))
        .expect("grid_ref after")
        .cell()
        .expect("cell after")
        .codepoint()
        .unwrap_or(0);

    // If the region was honored on the receiver, row 4 (y=3) is OUTSIDE
    // the [5..20] region and should not have been disturbed by the
    // extra LF. Equality means the region was preserved; inequality
    // means it wasn't.
    assert!(
        before_cp == after_cp,
        "scrolling region not preserved (row 4 col 0 changed: {before_cp:#x} → {after_cp:#x})",
    );
}
```

Notes:
- If libghostty's safe API later grows a direct accessor for DECSTBM margins, replace the dual-receiver probe with a direct query and update the panic message to match. The behavior under test ("region preserved across snapshot") doesn't change.
- The `_REGION_SENT_` sentinel is printed at row 5 col 0 inside the region — its presence isn't asserted but it guarantees we wait until the emulator has processed DECSTBM before resubscribing.

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_scrolling_region`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin DECSTBM scrolling-region gap in snapshot completeness suite"
```

---

### Task 11: Test #11 — Kitty keyboard flags preserved

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "kitty keyboard flags not preserved")]
async fn snapshot_preserves_kitty_keyboard_flags() {
    // Failure mode: Kitty keyboard protocol flags pushed via `CSI > <flags> u`
    //   are not preserved across snapshot. Receiver reports default flags
    //   (0) regardless of what the source pushed.
    // Impact: Kitty-keyboard-aware programs (Helix, Neovim with kitty
    //   keyboard, etc.) lose modifier disambiguation on the receiver;
    //   Ctrl+key, Shift+key, and unmodified key all collapse into the
    //   same sequence.
    // Why this fails today: `extra.keyboard` is off (and `extra.screen.
    //   kitty_keyboard` likewise), so the snapshot emits no Kitty
    //   protocol push.
    //
    // Source pushes flags = 5 (disambiguate + report event types). The
    // receiver should report kitty_keyboard_flags().bits() == 5.

    let pty = spawn_raw_session().await;
    let setup = b"\x1b[>5u_KKBD_SENT_";
    let sub = write_setup_and_resubscribe(&pty, setup, b"_KKBD_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    let flags = receiver.kitty_keyboard_flags().expect("kitty flags");
    let bits = flags.bits();
    assert!(
        bits == 5,
        "kitty keyboard flags not preserved (expected 5, got {bits})",
    );
}
```

Notes:
- `kitty_keyboard_flags()` returns a `KittyKeyFlags` bitflags type per `terminal.rs:318-323`. `.bits()` extracts the u8.
- If libghostty's flag encoding uses a different value for "disambiguate + report event types" than 5, adjust the source byte to match and assert the same emitted value. The point of the test is round-trip equality, not the specific flag.

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_kitty_keyboard_flags`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin kitty-keyboard-flags gap in snapshot completeness suite"
```

---

### Task 12: Test #12 — charset designation (DEC special graphics) preserved

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "charset designation not preserved")]
async fn snapshot_preserves_charset_designation() {
    // Failure mode: G0 character set designation (e.g., `\x1b(0` for DEC
    //   special graphics) is not preserved across snapshot. Receiver
    //   prints `lqqqk` as ASCII letters instead of as the line-drawing
    //   corner/horizontal characters.
    // Impact: TUIs using box-drawing for borders (mc, ncurses dialogs,
    //   classic htop) render ASCII garbage on the receiver until the
    //   program re-designates the charset.
    // Why this fails today: `extra.screen.charsets` is off.
    //
    // We designate G0 as DEC special graphics, position cursor at row 1
    // col 1, and print `lqqqk` — five box-drawing characters in DEC
    // graphics, plus the sentinel `_CHARSET_SENT_` (printed in DEC
    // graphics too, but the sentinel is ASCII letters not in the
    // remapped range so they should display unchanged).
    //
    // Readback: cell (0, 0) on the receiver should have a codepoint that
    // maps to the upper-left corner of a DEC line-drawing box, which in
    // Unicode is U+250C (┌). If charset designation was lost, the cell
    // codepoint is just 'l' (0x6c).

    let pty = spawn_raw_session().await;
    let setup = b"\x1b[H\x1b(0lqqqk_CHARSET_SENT_";
    let sub = write_setup_and_resubscribe(&pty, setup, b"_CHARSET_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);

    let p = libghostty_vt::terminal::Point::Viewport(
        libghostty_vt::terminal::PointCoordinate { x: 0, y: 0 },
    );
    let cell = receiver.grid_ref(p).expect("grid_ref").cell().expect("cell");
    let cp = cell.codepoint().unwrap_or(0);
    assert!(
        cp == 0x250c,
        "charset designation not preserved (cell 0,0 codepoint {cp:#x}, expected 0x250c U+250C ┌)",
    );
}
```

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_preserves_charset_designation`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: pin charset-designation gap in snapshot completeness suite"
```

---

### Task 13: Test #13 — synchronized output (DECSET 2026) does not leak (TRIPWIRE)

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test (no `#[should_panic]`)**

```rust
#[tokio::test]
async fn snapshot_does_not_leak_synchronized_output_mode() {
    // Tripwire: this test currently PASSES because `format_snapshot` emits
    // no DECSET sequences at all (`extra.modes` is off), so the
    // synchronized-output mode (DECSET 2026) the source is holding is
    // simply absent from the snapshot — there's nothing to leak yet.
    //
    // It will start FAILING the moment a partial fix flips
    // `extra.modes = true` without also implementing the toggle-off /
    // restore dance that zmx performs at `util.zig:488-491`. zmx
    // temporarily clears mode 2026 *before* formatting and restores it
    // after, specifically because a client that attaches while 2026 is
    // held will defer rendering until its local timeout fires — visible
    // to the user as a blank or flickering screen on attach.
    //
    // Impact when it fires: every attach to a session whose program is
    // mid-synchronized-update (modern TUIs that batch redraws) shows a
    // blank screen until the receiver's 2026 timeout (typically 150ms).

    let pty = spawn_raw_session().await;
    let setup = b"\x1b[?2026h_SYNCOUT_SENT_";
    let sub = write_setup_and_resubscribe(&pty, setup, b"_SYNCOUT_SENT_").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    let leaked = receiver.mode(Mode::SYNC_OUTPUT).expect("mode query");
    assert!(
        !leaked,
        "synchronized output (DECSET 2026) leaked into receiver — partial fix turned modes on without the toggle dance",
    );
}
```

- [ ] **Step 2: Run and verify (this test should PASS cleanly today)**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_does_not_leak_synchronized_output_mode`
Expected: PASS cleanly (no panic, no `should_panic`). Today the snapshot emits no mode-set for 2026 so the assertion holds.

If the test instead **fails** at this step, then the gap is already broken today (not just forward-only). Add `#[should_panic(expected = "synchronized output (DECSET 2026) leaked into receiver")]` above the test and rerun; it should then pass via the panic catch.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "test: add DECSET-2026 tripwire to snapshot completeness suite"
```

---

### Task 14: Test #14 — cursor position correct with scrollback present

**Files:**
- Modify: `crates/cairn-pty/tests/snapshot_completeness.rs`

- [ ] **Step 1: Append the test**

```rust
#[tokio::test]
#[should_panic(expected = "cursor position not preserved with scrollback")]
async fn snapshot_cursor_position_correct_with_scrollback() {
    // Failure mode: when the source has scrollback (rows pushed out of the
    //   active area), the snapshot interleaves scrollback rows + visible
    //   rows without zmx's two-phase split (`\x1b[2J\x1b[H\x1b[0m` between
    //   phases — see `zmx/src/util.zig:498-533`). Any CUP emitted at the
    //   end lands relative to wherever the receiver's viewport ended up
    //   after the scrollback rows pushed content down — not relative to
    //   the source's intended row.
    // Impact: this is the gnarliest case — the receiver's cursor is on
    //   the wrong physical row, sometimes off the visible viewport
    //   entirely, and subsequent text from the source lands in the wrong
    //   place. Mirrors zmx's regression test at `util.zig:1097-1135`.
    // Why this fails today: two-phase serialization is upstream-blocked
    //   on `libghostty-vt 0.1.1`'s C ABI exposing a `Selection` parameter
    //   to the formatter. Today the snapshot emits no CUP at all
    //   (`extra.screen.cursor` is off), so the receiver's cursor lands
    //   wherever the last cell write left it — which is essentially
    //   guaranteed to be wrong.
    //
    // Setup: 48 rows of `lineNN\r\n` (2 × ROWS, enough to overflow into
    //   scrollback), then CUP to row 5 col 10 (1-indexed; 0-indexed
    //   (9, 4)), then sentinel `*` plus `\x08` to land cursor at (9, 4).

    let pty = spawn_raw_session().await;
    let mut setup = Vec::with_capacity(8 * 48 + 16);
    for i in 0..48u32 {
        setup.extend_from_slice(format!("line{i:02}\r\n").as_bytes());
    }
    setup.extend_from_slice(b"\x1b[5;10H*\x08");

    let sub = write_setup_and_resubscribe(&pty, &setup, b"*").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    let cx = receiver.cursor_x().expect("cursor_x");
    let cy = receiver.cursor_y().expect("cursor_y");
    assert!(
        cx == 9 && cy == 4,
        "cursor position not preserved with scrollback (expected (9, 4), got ({cx}, {cy}))",
    );
}
```

- [ ] **Step 2: Run and verify**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness snapshot_cursor_position_correct_with_scrollback`
Expected: PASS via `#[should_panic]`.

- [ ] **Step 3: Run the entire suite end-to-end**

Run: `nix develop --command cargo nextest run -p cairn-pty --test snapshot_completeness`
Expected: all 14 tests pass — 13 via `#[should_panic]`, 1 (the tripwire) cleanly.

If any test FAILS at this stage, decide whether (a) the gap isn't actually broken today (drop `#[should_panic]` and reclassify as tripwire) or (b) the assertion / `expected = ...` substring is wrong (fix the test). Don't merge with a failing test.

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-pty/tests/snapshot_completeness.rs
git commit -m "$(cat <<'EOF'
test: pin scrollback-cursor gap and complete snapshot-completeness suite

Closes the test-side of step 3 of the pty-session "what needs to be
built" list. 13 broken-today tests pinned via #[should_panic] and 1
forward-looking tripwire covering the DECSET 2026 leak that a partial
fix would introduce. Each test names one distinct gap so when
libghostty-vt is patched and format_snapshot is rewritten, the suite
serves as the punch list — should_panic flips remove themselves as
gaps close.
EOF
)"
```

---

## Self-Review (run after writing the plan, fix issues inline)

**Spec coverage check:**
- ✅ Spec requirement: "One representative test per `FormatterTerminalExtra` flag" — Tasks 2-12 cover modes / screen.cursor / screen.style / screen.hyperlink / pwd / scrolling_region / keyboard / screen.charsets via tests #1-#12. ✓
- ✅ Spec requirement: "tripwire test for DECSET 2026" — Task 13. ✓
- ✅ Spec requirement: "scrollback cursor case" — Task 14. ✓
- ✅ Spec requirement: "External integration test at `crates/cairn-pty/tests/snapshot_completeness.rs`" — Task 1 creates this path. ✓
- ✅ Spec requirement: "Add `libghostty-vt = "0.1.1"` to `[dev-dependencies]`" — Task 1, Step 1. ✓
- ✅ Spec requirement: "harness helpers: `spawn_*`, `read_until_contains`, `write_setup_and_resubscribe`, `replay_into_receiver`" — Task 1, Step 3. The spec named `spawn_cat_session`; the plan renames to `spawn_raw_session` because raw-mode is required for deterministic-cursor tests (#6 and #14). Semantics match. ✓
- ✅ Spec requirement: "every test uses `#[should_panic(expected = "<unique substring>")]` for broken-today, omitted for tripwires" — every task explicitly specifies the annotation and the substring. ✓
- ✅ Spec requirement: "Comment block on each test: Failure mode / Impact / Why this fails today (or Tripwire / Impact / What trips it)" — every test in the plan has this block. ✓

**Placeholder scan:** no TBDs / TODOs / "implement later" / "similar to Task N" omissions. Every test's full source is present.

**Type consistency:**
- `Mode::BRACKETED_PASTE`, `Mode::DECCKM`, `Mode::FOCUS_EVENT`, `Mode::SYNC_OUTPUT` all match the public constants at `libghostty-vt-0.1.1/src/terminal.rs:518-558`.
- `cursor_x()` / `cursor_y()` / `cursor_style()` / `pwd()` / `kitty_keyboard_flags()` / `active_screen()` / `mode()` / `grid_ref()` — all match the public API at `libghostty-vt-0.1.1/src/terminal.rs:240-378`.
- `Point::Viewport(PointCoordinate { x, y })` matches `terminal.rs:388-398`.
- `Cell::codepoint()`, `Cell::has_hyperlink()` match `libghostty-vt-0.1.1/src/screen.rs:157-186`.
- `Style::bold`, `Style::fg_color`, `StyleColor::Palette(PaletteIndex)`, `PaletteIndex::RED` match `libghostty-vt-0.1.1/src/style.rs:30-103`.
- `KittyKeyFlags::bits()` is the standard `bitflags` accessor.
- `ffi::GhosttyTerminalScreen_GHOSTTY_TERMINAL_SCREEN_ALTERNATE` / `_PRIMARY` match `libghostty-vt-sys-0.1.1/src/bindings.rs:917-921`. Since `libghostty_vt` re-exports `libghostty_vt_sys as ffi` (`lib.rs:35`), the import path resolves.

No drift between tasks. Plan is internally consistent.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-24-snapshot-completeness-expected-failures.md`. Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?

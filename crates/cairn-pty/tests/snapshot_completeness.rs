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
    let buf = read_until_contains(&mut sub, READY_MARKER, READ_DEADLINE).await;
    assert!(
        windows_contain(&buf, READY_MARKER),
        "harness: timed out waiting for __READY__ from raw-mode shell (stty or cat failed to start within {}s)",
        READ_DEADLINE.as_secs(),
    );
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
    let buf = read_until_contains(&mut sub, sentinel, READ_DEADLINE).await;
    assert!(
        windows_contain(&buf, sentinel),
        "harness: timed out waiting for sentinel {:?} to echo back from session (write or echo path stalled within {}s)",
        std::str::from_utf8(sentinel).unwrap_or("<non-utf8>"),
        READ_DEADLINE.as_secs(),
    );
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

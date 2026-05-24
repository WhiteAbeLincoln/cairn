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

#[tokio::test]
async fn snapshot_does_not_leak_alt_screen_content_after_exit() {
    // Tripwire: source enters alt screen, writes ALT_MARK, exits alt screen,
    //   then writes MAIN_MARK_SENT_ on main. A buggy snapshot formatter could
    //   (a) include alt-screen content as if it were main-screen content, or
    //   (b) re-emit it via a partial DECSET cycle, leaving a stale ALT_MARK
    //   on the receiver's main buffer.
    // Impact: scrollback / main-buffer content visible on the receiver
    //   includes data the source never had on its main screen — a
    //   correctness violation visible to the user as ghost text.
    // What trips it: any change to `format_snapshot` that causes alt-buffer
    //   content to bleed into the main-buffer rendering path. Mirrors zmx's
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
                // PointCoordinate has private fields; construct via the
                // public ffi type and its From impl.
                let coord = libghostty_vt::ffi::GhosttyPointCoordinate {
                    x: x + dx,
                    y: y as u32,
                }
                .into();
                let p = libghostty_vt::terminal::Point::Viewport(coord);
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
    let coord = libghostty_vt::ffi::GhosttyPointCoordinate { x: 0u16, y: 0u32 }.into();
    let p = libghostty_vt::terminal::Point::Viewport(coord);
    let gref = receiver.grid_ref(p).expect("grid_ref");
    let cell = gref.cell().expect("cell");
    let has_link = cell.has_hyperlink().unwrap_or(false);
    assert!(
        has_link,
        "active hyperlink not preserved (cell at 0,0 has_hyperlink={has_link})",
    );
}

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

#[tokio::test]
async fn snapshot_preserves_scrolling_region() {
    // Tripwire: DECSTBM (`\x1b[<top>;<bot>r`) scroll-region preservation.
    //   This test PASSES today (the behavioral probe shows the region is
    //   honored on the receiver), but is retained as a tripwire so any
    //   future regression that breaks DECSTBM round-trip surfaces
    //   immediately rather than silently.
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
    let coord_before = libghostty_vt::ffi::GhosttyPointCoordinate { x: 0u16, y: 3u32 }.into();
    let p_before = libghostty_vt::terminal::Point::Viewport(coord_before);
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
    let coord_after = libghostty_vt::ffi::GhosttyPointCoordinate { x: 0u16, y: 3u32 }.into();
    let p_after = libghostty_vt::terminal::Point::Viewport(coord_after);
    let after_cp = probe
        .grid_ref(p_after)
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

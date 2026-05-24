# Snapshot Completeness — Expected-Failure Tests

Partial step 3 of the [pty-session "What needs to be built"](../../architecture/pty-session/README.md#what-needs-to-be-built)
list. The full step ports zmx's two-phase serializer and turns on
libghostty's `FormatterTerminalExtra` flags; this spec covers **only**
the test scaffolding that pins each distinct gap. No production code
changes.

Background:
[terminal-state-and-replay.md](../../architecture/pty-session/terminal-state-and-replay.md)
documents the contract under test and enumerates the missing extras.
The fix itself is blocked on `libghostty-vt 0.1.1` not exposing
`FormatterTerminalExtra` (Rust wrapper hardcodes all extras to
defaults) and the C ABI not exposing a `Selection` parameter on the
formatter (needed for zmx's two-phase split). A follow-up spec will
cover the fork / upstream patch and the rewrite of `format_snapshot`;
this spec stops at "every gap is a failing test."

## Goals

- Capture every distinct way cairn's current `format_snapshot` loses
  replayable state, as a checked-in test that exercises the real
  production path.
- Each test asserts the **correct** (post-fix) receiver state. The
  majority fail today and are wrapped in
  `#[should_panic(expected = "<unique substring>")]` so they pass
  while the gap is open (the assertion panics, `should_panic` catches
  it) and *start failing* the moment the gap closes — prompting
  whoever lands the fix to flip the annotation off rather than
  letting the test sit in `#[ignore]` limbo.
- A small number of tests are **tripwires**: they pass today (the
  defect isn't yet observable with current defaults) but assert
  behavior that a partial fix would silently regress. Those tests
  omit `#[should_panic]` and fail loudly the moment the regression
  appears.
- Stay strictly test-only: no changes to `crates/cairn-pty/src/`, no
  new public API, no production-code refactor.
- Use cairn's public `PtySession` / `Subscription` API for snapshot
  acquisition — the gap is in cairn's observable output, not in any
  internal helper.

## Non-goals

- **Fixing any of the failures.** This spec is exclusively about
  pinning them. The cargo-patch of `libghostty-vt`, the rewrite of
  `format_snapshot`, the upstream PR, and the two-phase scrollback
  split are all out of scope.
- **Exhaustive matrix of every DEC/ANSI mode.** One representative
  test per `FormatterTerminalExtra` flag (and one per
  failure-class-not-tied-to-an-extra, like the synchronized-output
  leak and the scrollback cursor case) is enough. We are not building
  a libghostty conformance suite.
- **Mocking the receiver.** The receiver is a fresh
  `libghostty_vt::Terminal`, which is also what the eventual cairn
  ghostty-web client will use to replay snapshots. Tests against the
  contract's actual consumer — not a parallel implementation.
- **Refactoring `pty_io.rs` or any other existing test file.** New
  tests live in a new file.

## Architecture

### File placement

New external integration test file:

```
crates/cairn-pty/tests/snapshot_completeness.rs
```

Sibling to `pty_io.rs`, `pty_lifecycle.rs`, `pty_multi_client.rs`,
`pty_resize.rs`. External integration tests are the right fit because
the snapshot is part of cairn-pty's public API surface
(`PtySession::subscribe()` → `Subscription.snapshot`); no
`pub(crate)` access is needed.

### Cargo.toml change

A single line in `crates/cairn-pty/Cargo.toml` under
`[dev-dependencies]`:

```toml
libghostty-vt = "0.1.1"
```

`libghostty-vt` is already a regular dependency of `cairn-pty` for the
production worker; adding it under `[dev-dependencies]` lets external
integration tests in `tests/` construct a fresh `Terminal` to use as
the snapshot's receiver. No re-export from the library's public API.

### Test harness

All harness lives in `tests/snapshot_completeness.rs` — no helper
module under `src/`. Four small helpers:

1. **`spawn_cat_session() -> GhosttyPty`** — spawns `cat` at a fixed
   80×24 size. `cat` echoes its stdin to stdout, which is how we get
   our canned VT bytes into cairn's embedded emulator through the real
   PTY path. Each test calls this fresh so failures isolate.

2. **`read_until_contains(sub: &mut Subscription, needle: &[u8]) -> Vec<u8>`**
   — drains `sub.snapshot` and subsequent `sub.stream` chunks until
   the accumulated bytes contain `needle` or a deadline elapses. Same
   shape as the helper already in `pty_io.rs`; duplicated rather than
   shared because integration-test crates don't share helpers without
   an inline module (and the duplication is ~25 lines).

3. **`write_setup_and_resubscribe(pty, setup, sentinel) -> Subscription`**
   — the load-bearing helper. Subscribes once (`sub1`), writes
   `setup` to the session, waits via `read_until_contains` on `sub1`
   for `sentinel` (proving the worker's emulator has absorbed the
   setup bytes), drops `sub1`, and returns a fresh `sub2` whose
   `snapshot` reflects post-setup state. `sentinel` is passed
   explicitly so OSC-using tests can pair the OSC with a printable
   companion byte sequence that `cat` echoes verbatim.

4. **`replay_into_receiver(snapshot: &Bytes) -> Terminal<'static, 'static>`**
   — builds a fresh 80×24 `libghostty_vt::Terminal`, `vt_write`s the
   snapshot bytes, returns it. The receiver is then queried via
   libghostty-vt's safe API (`mode()`, `cursor_x()`, `cursor_y()`,
   `active_screen()`, `pwd()`, `cursor_style()`,
   `kitty_keyboard_flags()`, `grid_ref()` etc.) to assert state.

### Test structure

A **broken-today** test:

```rust
#[tokio::test]
#[should_panic(expected = "<unique substring of this test's assertion>")]
async fn snapshot_preserves_<failure_mode>() {
    // Failure mode: <what's lost across snapshot>.
    // Impact: <user-observable consequence>.
    // Why this fails today: <which FormatterTerminalExtra flag or
    //   format_snapshot behavior is missing>.

    let pty = spawn_cat_session().await;
    let sub = write_setup_and_resubscribe(&pty, b"<setup>", b"<sentinel>").await;
    let receiver = replay_into_receiver(&sub.snapshot);
    assert!(
        receiver.<query>() == <source state>,
        "<unique panic message matching #[should_panic(expected=)]>"
    );
}
```

The `expected = "..."` string pins the panic to the test's own
assertion. If libghostty errors during `vt_write` or `replay_into_receiver`
panics for an unrelated reason, `should_panic` rejects the test and CI
goes red — exactly what we want.

A **tripwire** test is identical except it omits `#[should_panic]`
and adds a comment explaining what partial fix would trip it:

```rust
#[tokio::test]
async fn snapshot_does_not_leak_<regression>() {
    // Tripwire: this test currently passes because
    // <reason — usually "format_snapshot emits no DECSET sequences at all today">.
    // It will start failing the moment a partial fix flips
    // <which extra/flag> on without also doing <which additional work>.

    // … same body shape as a broken-today test, minus the should_panic.
}
```

The comment block above each test follows a fixed template:
**Failure mode** / **Impact** / **Why this fails today** for
broken-today tests, **Tripwire** / **Impact** / **What trips it** for
tripwire tests.

## The 14 tests

Each row gives the failure mode and the `FormatterTerminalExtra` flag
(or other gap) that a future fix would need to flip on.

Most tests in this batch fail *today* — they're wrapped in
`#[should_panic(...)]` and the panic disappears when the gap closes.
A small minority document gaps that aren't broken today but **would
silently regress under a partial fix**. Those tests omit
`#[should_panic]` and assert the desired behavior directly: they pass
today, and they're tripwires that fire the moment a partial fix
introduces the regression. The "Today?" column distinguishes the two.

| # | Test name | Today? | Source setup | Receiver assertion | Underlying gap |
|---|---|---|---|---|---|
| 1 | `snapshot_preserves_bracketed_paste_mode` | broken | `\x1b[?2004h` + printable sentinel | `Mode::BRACKETED_PASTE` is set on receiver | `extra.modes = true` |
| 2 | `snapshot_preserves_application_cursor_keys` | broken | `\x1b[?1h` + sentinel | `Mode::DECCKM` is set | `extra.modes = true` |
| 3 | `snapshot_preserves_focus_event_mode` | broken | `\x1b[?1004h` + sentinel | `Mode::FOCUS_EVENT` is set | `extra.modes = true` |
| 4 | `snapshot_preserves_alt_screen_when_active` | broken | `\x1b[?1049h` + `ALT_VISIBLE\r\n` | `active_screen() == ALTERNATE` AND `ALT_VISIBLE` is visible on receiver | `extra.modes = true` |
| 5 | `snapshot_does_not_leak_alt_screen_content_after_exit` | broken | `\x1b[?1049h` + `ALT_MARK\r\n` + `\x1b[?1049l` + `MAIN_MARK\r\n` | Receiver's main screen contains `MAIN_MARK` and NOT `ALT_MARK` | `extra.screen.<all>` (whichever flag controls inactive-screen suppression) |
| 6 | `snapshot_preserves_cursor_position` | broken | `hello\r\n\x1b[10;20H` + sentinel | `(cursor_x(), cursor_y()) == (19, 9)` | `extra.screen.cursor = true` |
| 7 | `snapshot_preserves_current_sgr_style` | broken | `\x1b[1;31m` + `\x1b[H` + sentinel | `cursor_style()` has bold + red FG | `extra.screen.style = true` |
| 8 | `snapshot_preserves_active_hyperlink` | broken | `\x1b]8;;https://example.com\x1b\\` + sentinel char (which becomes the linked cell) | Cell at cursor or just before cursor carries a hyperlink id (queried via `grid_ref`; fallback: snapshot byte-pattern match for OSC 8) | `extra.screen.hyperlink = true` |
| 9 | `snapshot_preserves_working_directory` | broken | `\x1b]7;file:///home/abe/projects\x1b\\` + sentinel | `pwd() == "/home/abe/projects"` | `extra.pwd = true` |
| 10 | `snapshot_preserves_scrolling_region` | broken | `\x1b[5;20r` + sentinel | Scrolling region rows 5–20 on receiver (verified via `mode(ORIGIN)` plus a content probe if direct readback isn't available) | `extra.scrolling_region = true` |
| 11 | `snapshot_preserves_kitty_keyboard_flags` | broken | `\x1b[>5u` + sentinel | `kitty_keyboard_flags()` matches source | `extra.keyboard = true` (and/or `extra.screen.kitty_keyboard`) |
| 12 | `snapshot_preserves_charset_designation` | broken | `\x1b(0` + `lqqqk\r\n` + sentinel | Receiver renders box-drawing chars for `lqqqk` (verified via `grid_ref` cell read) | `extra.screen.charsets = true` |
| 13 | `snapshot_does_not_leak_synchronized_output_mode` | tripwire | `\x1b[?2026h` + sentinel | `Mode::SYNC_OUTPUT` is **not** set on receiver | DECSET 2026 toggle dance around format (zmx `util.zig:488-491`); regression appears once `extra.modes = true` is enabled without the dance |
| 14 | `snapshot_cursor_position_correct_with_scrollback` | broken | N rows of `line<i>\r\n` where N ≥ 2 × ROWS, then `\x1b[5;10H` + sentinel | `cursor_x()/cursor_y() == (9, 4)` AND the cursor lies on the receiver's visible viewport | Two-phase serialization (zmx `util.zig:498-533`); upstream-blocked on `Selection` support in the libghostty-vt C ABI |

**Today? column legend:**

- **broken** — currently incorrect; the assertion panics today. Wrap
  in `#[should_panic(expected = "...")]`. Flips to a real failure
  when the gap closes.
- **tripwire** — currently correct, but a partial fix would make it
  regress. **Omit** `#[should_panic]`; the test passes today and
  starts failing the moment the regression is introduced.

If a test labelled "broken" turns out to pass at merge time (i.e. the
gap isn't actually broken with current defaults), drop its
`#[should_panic]` annotation — it then automatically reclassifies as a
"tripwire" test, which is fine.

### Notes on the uncertain three

- **#8 (hyperlink readback)** — libghostty-vt 0.1.1's safe API exposes
  `cursor_style()` returning a `Style`; whether `Style` carries
  hyperlink id is unverified at spec time. If not, fall back to
  printing a sentinel char while the link is active and reading the
  cell back through `grid_ref`. If even that's not enough, the test
  becomes a snapshot-byte-pattern assertion for `\x1b]8;;https://example.com\x1b\\`,
  with a comment that this is the weakest of the available checks.
- **#10 (scroll region readback)** — if `Mode::ORIGIN` plus
  `cursor_x/y` isn't enough to assert margins, the test sends rows of
  content that would scroll inside vs. outside the region and reads
  back affected cells via `grid_ref` to infer the region.
- **#11 (kitty keyboard)** — `kitty_keyboard_flags()` returns the
  current top-of-stack flags. If push/pop semantics interfere, the
  test pushes a unique flag combination and asserts the receiver
  reports the same combination.

The fallbacks are implementation choices, not spec changes. Each test
ships with whichever readback works.

## Wire-format vs. round-trip assertions

Every test asserts on **receiver state** (round-trip), not on
**snapshot byte patterns**, with the single exception noted in #8 if
no state readback path exists. Rationale documented in the
brainstorming transcript: a byte-pattern match passes even when the
right escape sequence appears in the wrong context (e.g., inside an
alt-screen block that's later unwound), and tightly couples tests to
libghostty's specific escape-sequence choices. Round-trip into a fresh
`Terminal` tests the actual cairn-snapshot contract: "fed to a
VT-compliant emulator, the bytes reconstruct source state."

The receiver `Terminal` is the consumer side of cairn's snapshot
contract — same emulator core the ghostty-web client will use — not a
parallel implementation of `format_snapshot`.

## Future-fix flip workflow

When the follow-up spec lands and `format_snapshot` is rewritten to
turn on the appropriate extras (and/or to do the two-phase split for
test #14):

1. Run `cargo nextest run -p cairn-pty --test snapshot_completeness`.
2. **Broken-today tests** whose gap the fix closes will fail with
   "test did not panic" — that's `#[should_panic]` signaling success.
   Delete the `#[should_panic(...)]` attribute on each.
3. **Tripwire tests** whose tripwire condition the fix introduces
   will fail with a real assertion failure — meaning the partial fix
   did create the predicted regression. Address the regression in the
   fix (don't touch the tripwire test).
4. Re-run; broken-today tests now pass on their merits, tripwire
   tests still pass (because the regression was avoided).

Tests whose gaps the fix doesn't close keep their original shape.
The file is the punch list.

## Testing strategy

- The tests **are** the testing strategy. No tests-of-tests.
- One pass through `cargo nextest run -p cairn-pty --test snapshot_completeness`
  before merging. Every test must currently pass: broken-today tests
  by panicking with their expected substring, tripwire tests by
  asserting cleanly.
- If a **broken-today** test doesn't panic at merge time: either (a)
  the gap isn't real with current defaults, (b) the assertion is
  wrong, or (c) `should_panic`'s `expected` string doesn't match the
  actual panic. If (a), drop the `#[should_panic]` and reclassify as
  tripwire. If (b) or (c), fix the test. Don't paper over.
- If a **tripwire** test fails at merge time: the regression it
  predicts already exists — meaning a gap we thought was forward-only
  is actually broken today. Reclassify by adding `#[should_panic]`
  with a substring matching the panic.

## Open questions

None blocking. Two design choices we've already made but worth
flagging for future readers:

1. **Why `should_panic` rather than `#[ignore]`?** Ignored tests
   don't run, so they don't signal when the underlying behavior
   changes. `should_panic` tests run on every invocation, panic
   today, and start *failing* the moment a fix lands — which is
   exactly the signal we want.

2. **Why duplicate `read_until_contains` from `pty_io.rs`?**
   External integration tests in Cargo are separate compilation
   units; sharing helpers requires either a `tests/common/mod.rs`
   module or a workspace test-helper crate. Both are bigger
   refactors than the 25-line duplication, and the helper is
   stable. If a third test file needs the same helper, extract
   then.

## Out of band

- The follow-up spec (libghostty-vt fork / cargo-patch + rewritten
  `format_snapshot` + two-phase split) will reference this file by
  test number when describing what each `should_panic` flip
  validates.
- The
  [terminal-state-and-replay.md](../../architecture/pty-session/terminal-state-and-replay.md)
  Open Questions section already enumerates the gaps; no edit needed
  there as part of this work.

# Bugs

## ~~1. `whoami` always returns 501~~ FIXED

Implemented passwd lookup via `nix::unistd::User::from_uid()`.

## ~~2. Version output isn't helpful~~ FIXED

Reformatted to `daemon: cairn-daemon/0.1.0 (protocol cairn:daemon@0.1.0)`.

## ~~3. Daemon logs get stuck / no "listening at" event~~ FIXED

Added `tracing::info!` after socket bind in `serve()`.

## ~~4. Kick message not visible on kicked client~~ FIXED

`RawGuard::drop` sent RIS (`\x1bc`) which cleared the terminal *after* the
message was printed — wiping it. Fix: explicitly drop the guard before
printing the fatal message.

## ~~5. Global args mixed in with subcommand help~~ FIXED

Added `help_heading = "Global options"` to `--daemon`, `--token`,
`--verbose`, `--color` so they group separately in subcommand `--help`.

## ~~6. Attached clients list unsorted in inspect output~~ FIXED

Sort client IDs before displaying.

## pid line in cairn inspect is empty

Cairn inspect always shows a - for the pid row.

## (Low Priority) typing in the terminal is has perceptible lag

Not obviously choppy but uncomfortable. Don't know how to evaluate this.
Will likely be significantly worse over a remote connection which we want to
avoid.

## starting an interactive program with exec locks up 

Start fish or zsh with cairn exec (not cairn run): `cairn exec "$(which fish)"`.
Fish hangs for a bit, then logs
"warning: fish could not read response to Primary Device Attribute query after waiting for 10 seconds."
before the prompt (as expected, this is the same issue zmx documents).

However, after this we can't disconnect using ctrl-q ctrl-q, keyboard input doesn't work (expected without a pty).
The cairn client is unresponsive to keystrokes (but responds when an external `cairn kill` occurs)

## log message to client when process is killed

If the process is externally killed (through `cairn kill`), log a message on
attached clients similar to `cairn kick` - how feasible is this?

## ~~7. Esc in vim requires two key-presses~~ FIXED

The detach matcher withheld bare `0x1b` as a partial CSI-u prefix match,
blocking until the next keystroke disambiguated it. Added a 50ms flush
timeout — if no follow-up byte arrives, withheld bytes are released as
normal input. Kitty-aware terminals are unaffected (Esc arrives as a
multi-byte CSI-u sequence resolved in one pass).

## 8. Attach on running session doesn't place cursor correctly

**Known cairn-pty snapshot gap.** The daemon sends `clear_screen()` then the
snapshot bytes, but `format_snapshot` doesn't emit a CUP sequence to restore
the cursor position. The client sees the screen content replayed from (0,0)
but the cursor lands wherever the last character was written, not where the
session's actual cursor is. Alt-screen, application-cursor-keys, and
scrolling-region gaps compound this for TUI apps. Pinned by
`snapshot_preserves_cursor_position` (and 10 other `#[should_panic]` tests)
in `cairn-pty/tests/snapshot_completeness.rs`.

## 9. Ghostty + Terminal.app cross-terminal rendering

**Multi-client size disagreement + snapshot gaps.** The leader-wins resize
model means only the first interactive client controls the PTY size. When
Ghostty (leader) is 77x24 and Terminal.app attaches second, it gets
`NotLeader` on resize — the PTY stays 77x24 regardless of Terminal.app's
window. Vim won't re-render because it never receives a SIGWINCH (the PTY
size didn't change). Color differences stem from SGR style not being
preserved in the snapshot (also a known failing test). Text reflow is
fundamentally a size-mismatch problem. Hardest bug — involves design
decisions around multi-client size negotiation, not just implementation gaps.

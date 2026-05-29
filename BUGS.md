# Bugs

## Misleading error message after app close (not an error)

2026-05-29T06:41:43.374099Z ERROR wrpc_transport_web: ingress failed err=Custom { kind: NotConnected, error: ConnectionLost(ApplicationClosed(ApplicationClose { error_code: 0, reason: b"" })) }

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

## version command should show which transport is being used

User might not know if it's set in a config file or env var
(not the case right now).

## whoami should show the authentication method

tailscale, jwt, local unix

## Attach on running session doesn't place cursor correctly

**Known cairn-pty snapshot gap.** The daemon sends `clear_screen()` then the
snapshot bytes, but `format_snapshot` doesn't emit a CUP sequence to restore
the cursor position. The client sees the screen content replayed from (0,0)
but the cursor lands wherever the last character was written, not where the
session's actual cursor is. Alt-screen, application-cursor-keys, and
scrolling-region gaps compound this for TUI apps. Pinned by
`snapshot_preserves_cursor_position` (and 10 other `#[should_panic]` tests)
in `cairn-pty/tests/snapshot_completeness.rs`.

## (Low Priority) 50ms post-reap drain in cairn-pty worker feels hacky

`crates/cairn-pty/src/ghostty/worker.rs` reaps the child via a
`child.wait()` arm racing `pty.read()` in tokio::select!, then drains the
master PTY for up to 50ms (`EXIT_DRAIN`) before dropping the broadcast
sender. This works in practice but the timeout constant is a heuristic,
not a guarantee — on a sufficiently slow / loaded system the kernel
might still have queued slave-side output past the window, and we'd
lose those tail bytes from the live broadcast (they still flow into
terminal state for future snapshots).

The principled alternative (zmx-style: reap only after `pty.read`
returns EOF/Err, no race, no timeout) is documented on the
`pty-exit-hang-fix-zmx-style` branch. It doesn't work today because
tokio's `AsyncFd` doesn't propagate POLLHUP-only events on Linux
master PTYs — the `readable` future stays pending forever when the
slave closes with no further data. zmx avoids this because its
`posix.poll` call asks for `POLL.HUP` in the events bitmap explicitly.

Revisit if/when we move off tokio's `AsyncFd` (e.g. to nix/rustix
with custom epoll registration that includes POLLHUP), or if upstream
tokio exposes a HUP interest. Until then the drain stays. Not blocking
anything; low priority unless we observe lost bytes in the wild.

## Ghostty + Terminal.app cross-terminal rendering

**Multi-client size disagreement + snapshot gaps.** The leader-wins resize
model means only the first interactive client controls the PTY size. When
Ghostty (leader) is 77x24 and Terminal.app attaches second, it gets
`NotLeader` on resize — the PTY stays 77x24 regardless of Terminal.app's
window. Vim won't re-render because it never receives a SIGWINCH (the PTY
size didn't change). Color differences stem from SGR style not being
preserved in the snapshot (also a known failing test). Text reflow is
fundamentally a size-mismatch problem. Hardest bug — involves design
decisions around multi-client size negotiation, not just implementation gaps.

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

## Attach on running session doesn't place cursor correctly

**Known cairn-pty snapshot gap.** The daemon sends `clear_screen()` then the
snapshot bytes, but `format_snapshot` doesn't emit a CUP sequence to restore
the cursor position. The client sees the screen content replayed from (0,0)
but the cursor lands wherever the last character was written, not where the
session's actual cursor is. Alt-screen, application-cursor-keys, and
scrolling-region gaps compound this for TUI apps. Pinned by
`snapshot_preserves_cursor_position` (and 10 other `#[should_panic]` tests)
in `cairn-pty/tests/snapshot_completeness.rs`.

## `--daemon wt://` rejects hostnames

`connect.rs::parse_wt()` parses the address as `SocketAddr`, which only
accepts IP:port. `--daemon wt://myhost.ts.net:4433` fails with a parse
error. Needs `tokio::net::lookup_host()` to resolve DNS, keeping the
original hostname as the `host` field for TLS SNI.

## Tailscale auth backend not implemented on Linux

`TailscaleBackend::new()` bails on non-macOS. The Tailscale LocalAPI on
Linux listens on a Unix domain socket (`/var/run/tailscale/tailscaled.sock`),
which needs a hyper UDS connector instead of the TCP `HttpConnector`.

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

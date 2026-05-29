# Exit Reason Tracking

Track *why* a session exited and surface that reason to attached interactive
clients so the user isn't left staring at a reset terminal.

## Motivation

When `cairn kill` terminates a session, every attached client sees the
terminal reset and the process exit — but nothing explains what happened.
The same problem will apply to future exit triggers (idle timeout, daemon
shutdown, restart). The exit status alone (code/signal) doesn't distinguish
"the process decided to exit" from "an operator killed it."

## Design

### Reason string on the signal path

`PtySession::signal` gains an `Option<String>` reason parameter. The pty
worker stashes the most recent reason in a `Mutex<Option<String>>`. When
the child exits, `ExitStatus` is built with whatever reason is stashed at
that point. Last-writer-wins handles the grace-escalation case (SIGTERM
reason is overwritten by the SIGKILL escalation reason).

`ExitStatus` in cairn-pty gains a `reason: Option<String>` field. The pty
layer treats it as an opaque pass-through.

### Wire protocol

The WIT `exit-status` record gains:

```wit
record exit-status {
    code: option<s32>,
    signal: option<u8>,
    unix-ms: u64,
    reason: option<string>,
}
```

### Daemon call sites

| Call site | Reason |
|---|---|
| `sessions::kill()` | `"killed by operator"` |
| Grace escalation (SIGKILL after timeout) | `"killed by operator (escalated to SIGKILL)"` |
| `drain_sessions()` on shutdown | `"daemon shutting down"` |
| Self-exit (no signal called) | `None` |

The daemon owns the reason vocabulary. The client never interprets the
string — it just prints it.

### Client display

In `attach.rs`, the `Outcome::Exited` arm checks:
- If `raw_mode` (interactive TTY session) and `reason.is_some()`:
  drop `RawGuard` first, then print `cairn: <reason>` to stderr.
- Otherwise: silent, as today.

This follows the same pattern as the kick message fix — drop the guard
before printing so RIS doesn't wipe the message. Non-interactive clients
(`cairn wait`, piped `cairn exec`) see only the exit code.

## Changes by crate

### cairn-pty

- `ExitStatus`: add `reason: Option<String>` field and accessor.
- `ExitStatus::from_std`: accept `reason: Option<String>` parameter.
- `ExitStatus::synthetic`: set reason to `None`.
- `PtySession::signal`: add `reason: Option<String>` parameter.
- `GhosttyPty::signal` and `Command::Signal`: carry the reason.
- Worker: stash reason in `Mutex<Option<String>>` on signal, read it at
  exit-detection time.

### cairn-protocol

- WIT `exit-status`: add `reason: option<string>`.

### cairn-daemon

- `wire_exit`: thread reason through to wire type.
- `sessions::kill`: pass reason to `handle.signal()`.
- Grace escalation task: pass escalation reason.
- `drain_sessions`: pass shutdown reason.

### cairn-client

- `Outcome::Exited`: add `reason: Option<String>`.
- `handle_server_batch`: extract reason from `ServerEvent::Exited`.
- Exit handling: print reason to stderr when interactive.

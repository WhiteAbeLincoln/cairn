# Client Attach and Leader Election

Companion to [[pty-lifecycle]], [[terminal-state-and-replay]], and
[[query-response-delegation]]. This doc covers **what happens when a client
joins or leaves an existing PTY session**, and **how zmx picks the single
"leader" among N attached clients**. zmx is the reference; cairn
([[daemon-process-model]]) matches its semantics over wRPC (transports:
UDS for local CLI, WebTransport for browser and remote CLI) so all
client types can attach uniformly. The attach flow runs through the WIT
`sessions.attach` operation — a bidirectional stream pair of
`client-event`s flowing inbound and `server-event`s flowing outbound,
documented in [[external-protocol]]. See also [[web-vs-cli-clients]].

---

## Attach handshake (zmx)

The flow is driven by the client sending an `Init` message right after
connect:

1. Client opens the session's Unix socket (`src/main.zig:2284-2303`
   `clientLoop`), sets `O_NONBLOCK`, and buffers `Init` with its current
   TTY size (`getTerminalSize` → `ipc.Resize { rows, cols }`,
   `src/main.zig:2302`, `src/ipc.zig:38-49`).
2. Daemon `accept()`s the fd and appends a `Client` to `daemon.clients`
   (`src/main.zig:2526-2545`). No state is sent yet.
3. On `Init` (`src/main.zig:933-1000` `handleInit`):
   - If the daemon already has PTY output **and** a prior client has
     attached (`has_pty_output && has_had_client`,
     `src/main.zig:947`), it serializes the libghostty terminal via
     `util.serializeTerminalState` (`src/util.zig:479-586`) and sends
     it as `Output`. The snapshot is **scrollback + visible viewport**
     in two phases (`src/util.zig:510-567`): plain scrollback text,
     then `\x1b[2J\x1b[H\x1b[0m`, then the visible screen with modes,
     cursor, keyboard flags, scrolling region, and pwd.
     `synchronized_output` is forced off during snapshot
     (`src/util.zig:488-491`); OSC 133;A is rewritten with `redraw=0`
     (`src/util.zig:957`, `src/main.zig:2597`) so the outer terminal
     doesn't clear prompt lines on resize.
   - On the **very first attach** (`!has_had_client`) no snapshot is
     sent — shell startup chatter (DA1/DA2 etc.) flows naturally
     instead. The guard at `src/main.zig:944-946` exists explicitly to
     avoid stepping on shell init.
   - If no leader is set, this client becomes leader via `setLeader`
     (`src/main.zig:971-973`, `src/main.zig:643-650`).
   - If this client is leader, the daemon applies `TIOCSWINSZ` and
     resizes the libghostty `Terminal` (`src/main.zig:976-998`).
     `has_had_client` and `has_terminal_client` both flip true.

After the handshake, the client just relays bytes both ways
(`Input`, `Output`) and answers `Resize` requests from the daemon.

## "Terminal client" vs. tail-only client

`has_terminal_client` (`src/main.zig:592`,
`src/main.zig:996`) is **only flipped true when a client sends `Init`**.
zmx supports `zmx run`, which connects to read output (the "tail" mode in
`src/main.zig:1364-1500` `tail`) without sending `Init`. Those connections
appear in `daemon.clients` and receive broadcast output but are **not**
counted as terminal clients.

The flag gates one behaviour: synthetic DA1/DA2 (Device Attributes)
replies. At `src/main.zig:2566-2576`, when PTY output passes through
the headless libghostty emulator and `!has_terminal_client`, the daemon
itself answers DA queries via `util.respondToDeviceAttributes`. This
exists so `fish` (which blocks ~10s for DA1) starts cleanly in a
tail-only session. Once any real attach lands the flag stays sticky and
the daemon stops synthesising; replies come from attached clients via
their forwarded `Input`. The flag is **never cleared** — even after
every terminal client detaches, the daemon won't resume synthesising.
See [[query-response-delegation]].

## Leader election

> "Leader selection by most recent user input."

Confirmed. The daemon holds a single `leader_client_fd: ?i32`
(`src/main.zig:583`) and updates it in three places:

| Trigger                                    | Site                            | New leader                    |
| ------------------------------------------ | ------------------------------- | ----------------------------- |
| First `Init` while no leader               | `handleInit`, `main.zig:971-973`| The attaching client          |
| Any `Resize` while no leader               | `handleResize`, `main.zig:1010-1011` | The resizing client       |
| `Input` from non-leader that is **user input** | `handleInput`, `main.zig:897-910` | The typing client          |
| Leader disconnects                         | `closeClient`, `main.zig:624-631` | None (cleared to `null`)    |

The non-leader-input case is the heart of the policy:

```zig
// main.zig:897
pub fn handleInput(self: *Daemon, client: *Client, payload: []const u8) !void {
    if (self.leader_client_fd == client.socket_fd) {
        self.queuePtyInput(payload);
        return;
    }
    if (util.isUserInput(payload)) {
        try self.setLeader(client);
        self.queuePtyInput(payload);
    }
}
```

Mouse events (`CSI M`, `CSI <`) and focus events (`CSI I`, `CSI O`) are
explicitly **excluded** from "user input" (`src/util.zig:461-466`).
Bare query responses from the underlying terminal would also fail the
test, because `isUserInput` only returns true for `print`, kitty/legacy
modified-key sequences, or `CR/LF/Tab/Backspace`
(`src/util.zig:446-477`). The intent: a user actively typing claims the
session, but the terminal app whispering DA replies and mouse positions
does not.

There is **no debounce** — the very first qualifying byte from a
non-leader switches leadership atomically. `setLeader`
(`src/main.zig:643-650`) immediately sends a `Resize` request back to
the new leader; the client answers with its current TTY size
(`main.zig:2407-2417`) and the daemon retunes the PTY + libghostty grid
to match.

When the leader detaches, the seat is vacated, not handed off: no
"runner-up" promotion. The next qualifying input (or `Resize`/`Init`)
from any remaining client claims it. If only tail-only clients remain,
the seat stays empty until a future terminal client attaches.

## What being leader changes

Only two things are gated by leader identity in zmx:

1. **Resize source.** `handleResize` is a no-op for non-leaders
   (`src/main.zig:1014`). Only the leader's window size drives the
   shared PTY's `TIOCSWINSZ` and the libghostty grid. See
   [[resize-semantics]] for the full picture.
2. **`Switch` delivery target.** `handleSwitch`
   (`src/main.zig:912-931`) sends the `Switch` IPC message only to the
   current leader, because at most one user is interactively driving
   the session and only that user's client should be redirected.

That's the whole list. **Output is broadcast unconditionally** to every
attached client (`src/main.zig:2599-2608`, `Output` to all of
`daemon.clients`), including tail-only clients. **Input is accepted
unconditionally** from every attached client — both leader and non-leader
input is written to the PTY (`handleInput` queues bytes either way).
Mode tracking (mouse, focus, alt-screen, etc.) lives entirely in the
single shared libghostty `Terminal` and is therefore independent of who
the leader is.

## Multi-write implications

Because every connected client can write to the PTY at any time, the
shell sees an interleaved byte stream from N possible humans. zmx accepts
this — concurrent typing is rare in practice, and the daemon's PTY write
buffer (`PTY_WRITE_BUF_MAX = 256 KiB`, `src/main.zig:872`) just
serialises whatever arrives.

The consequence that matters for cairn: **query responses are also
"input"**. When the shell emits `CSI c` (DA1), every attached terminal
client's local emulator may reply, and all of those replies arrive at
the daemon as ordinary `Input`, get concatenated, and are written to
the PTY. zmx's mitigation is the `has_terminal_client` short-circuit
(daemon answers itself until a real client is present) plus an implicit
assumption that in practice only one ghostty-on-the-other-end is doing
the replying. Cairn cannot inherit that assumption: with multiple
browser clients attached simultaneously the duplicate-reply problem is
the common case, not the corner case. See
[[query-response-delegation]] for the proposed policy.

---

## Proposed adaptation for cairn

Mirror zmx's policy with these changes; nothing here breaks the
`PtySession` trait at `crates/cairn-pty/src/pty/session.rs:13-33`:

- **Attach == `subscribe()`.** Already atomic per
  `crates/cairn-pty/src/pty/subscription.rs:16-19` and the
  `Command::Subscribe` arm at
  `crates/cairn-pty/src/pty/ghostty/worker.rs:355-371`. Verify
  `format_snapshot` emits scrollback + viewport with zmx's OSC 133
  and synchronized-output hygiene; port the workarounds if missing.
  See [[terminal-state-and-replay]].
- **First-attach snapshot is kept, not suppressed.** zmx skips the
  snapshot for the very first attach because in zmx the spawning
  client *is* the first attacher: shell startup is in flight as the
  client opens the socket, and a snapshot at that moment would
  duplicate or fight the bytes the client is about to receive live.
  Cairn's primary use case inverts that assumption — processes are
  spawned headlessly and may run unattended for hours (AI agent
  workflows, background automation) before any client looks at them.
  Suppressing the snapshot on first attach would throw away the
  accumulated state the user opened the terminal to see. Cairn keeps
  the snapshot on every `subscribe()` call; the worker does not
  track a `has_had_client` equivalent.
- **Leader state on the session.** Add `leader: Option<ClientId>` and
  `last_input_at: Option<Instant>` to the worker's `SessionState`.
  `ClientId` is assigned by whichever wRPC-attach handler owns the
  client's stream pair ([[internal-communication]]); the PTY worker
  doesn't distinguish CLI vs. browser, nor UDS vs. WT.
- **`Command::Write { client_id, ... }`.** Threading a client id
  through writes is mandatory — otherwise the worker has no way to
  apply `isUserInput` and promote leadership.
- **Resize is leader-only.** `Command::Resize` should reject non-leader
  resizes. Today's doc says last-write-wins, which is more permissive
  than zmx. See [[resize-semantics]].
- **Headless Terminal stays internal to the worker — not modeled as
  a client.** zmx's daemon owns a `term` that participates in the
  same dispatch loop as real client sockets (one unified collection
  of attached endpoints, the internal `term` exempt from leader
  election). Cairn diverges: the libghostty `Terminal` in
  `crates/cairn-pty/src/ghostty/worker.rs:run_session` is a private
  `Rc<RefCell<Terminal>>` owned by the worker thread. It doesn't go
  through `Command::Subscribe` or `Command::Write`; PTY output flows
  through `terminal.vt_write` directly inside the worker's `select!`
  arm, and the `on_pty_write` callback queues responses to
  `pending_writes` for the worker to drain in the same iteration.
  This keeps the `subscribe()` snapshot atomicity contract
  structural (snapshot + stream-start happen in one synchronous
  block) rather than requiring coordination across a feedback edge.
  The "is any real client attached" signal that zmx derives from
  `has_terminal_client` is provided in cairn by the `primary_count`
  atomic, incremented on `Subscribe` and decremented by
  `SubscriptionGuard::drop` — no synthetic local-client identity is
  needed. If we ever want multiple internal observers (log capture,
  telemetry), an observer abstraction can be added then; YAGNI for
  one.
- **Tail-only == `subscribe()` without `write()`.** No new flag
  needed; the daemon's equivalent of `has_terminal_client` just
  becomes "has any client ever issued a write".

---

## Open Questions

1. **Snapshot/broadcast race.** The worker reads the libghostty grid
   while PTY output is still flowing through the same emulator. The
   trait promises "no gap, no overlap" — verify under load. See
   [[testing]].
2. **Leader ping-pong.** Two browser tabs racing for input flap
   leadership on every keystroke. zmx accepts this. Probably fine for
   writes; **possibly worth debouncing resizes** so a 30-second war
   between phone and laptop doesn't thrash the PTY size. See
   [[resize-semantics]].
3. **Handoff on graceful detach.** zmx clears the leader and waits for
   the next input. For long-lived web sessions where the only human
   locks their screen, nothing can resize until they return. Should
   cairn promote the most-recent-non-leader using `last_input_at`?
4. **Headless Terminal and DA replies.** Resolved by the
   architecture choice above: the headless `Terminal` is internal to
   the worker, not a client, and its query auto-replies are gated by
   the `primary_count` atomic — when any real client is attached
   (`primary_count > 0`), the backend stays silent and attached
   client emulators answer. See [[query-response-delegation]].
5. **Auth interaction.** A read-only viewer must not claim leadership
   by sending input. The election check must sit behind a write
   authorization check. See [[authentication]] and
   [[external-protocol]].
6. **Observability.** Mirror zmx's `setting new leader` log
   (`src/main.zig:644`) as a tracing event with `client_id` and
   `cause` (`init`/`resize`/`input`/`detach`). See [[observability]].
7. **Stuck leader recovery.** A half-open WT or UDS leader blocks
   everyone else's resizes until the underlying carrier detects the
   gap (QUIC idle timeout for WT; TCP-keepalive-style detection for
   UDS attaches). zmx has the same problem but its blocking-poll
   UDS attaches fail faster. Heartbeat-driven kick vs. explicit
   "steal" command? See [[backpressure]] and [[error-recovery]].

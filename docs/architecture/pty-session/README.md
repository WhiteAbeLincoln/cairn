# Cairn PTY Session Manager — Architecture Overview

This directory holds the design documents for cairn's PTY session manager — the subsystem that runs terminal processes inside persistent sessions, lets multiple clients attach/detach, and survives client disconnects without losing the running process or its state.

The design is closely modelled on [zmx](https://github.com/neurosnap/zmx), a single-binary Zig tool that solves the same problem for local CLI use. Cairn keeps the architecture but adapts it for two material differences: WebSocket transport (so browser clients like ghostty-web can attach) and a single-daemon process model (so all sessions are reachable from one HTTP endpoint).

Each topic below has its own document. This file is the entry point — read it to get the picture; consult the per-topic docs when you need to make or review a decision in that area.

## What we're building

A subsystem with these capabilities, mirroring zmx feature-for-feature:

- Spawn a child process inside a PTY and keep it running independently of any attached client
- Allow N clients to attach to the same session simultaneously
- Restore prior screen state and scrollback when a client (re)attaches
- Embed a headless [libghostty-vt](https://crates.io/crates/libghostty-vt) emulator on the server so the session can render its own scrollback, answer some terminal queries when no client is attached, and feed authoritative state to clients on reattach
- Persist sessions across daemon restarts? — not in v0 (see [Non-goals](#non-goals))
- Send input to a session without attaching (`cairn send`), print scrollback (`cairn history`), kill (`cairn kill`), list (`cairn list`)

Explicitly **not** a multiplexer: no windows, tabs, or splits. zmx makes the same choice (zmx README and `[[testing]]`).

## Design baseline: zmx

Concrete shape — verified via source citations across the topic docs:

- **One OS process per session.** zmx's `attach` command forks a daemon for that session (`zmx/src/main.zig:777`, [[daemon-process-model]]). Loss of one session does not affect others.
- **Unix domain sockets for transport**, one per session, in `$XDG_RUNTIME_DIR/zmx/` or fallbacks (`zmx/src/main.zig:504-516`). Authentication is filesystem DAC — directory mode `0o750`, no peer-credential check ([[authentication]]).
- **Single-threaded `posix.poll()` loop** drives PTY reads, client I/O, signals, and the embedded ghostty emulator (`zmx/src/main.zig:572`, [[internal-communication]]).
- **Embedded ghostty-vt** runs continuously for state-tracking and scrollback. zmx hardcodes DA1/DA2 replies for the no-client case (`zmx/src/util.zig:142-180`) and gates them on a sticky `has_terminal_client` flag set on first `Init` and never reset (`zmx/src/main.zig:592, 996`, [[query-response-delegation]]).
- **Leader election by most-recent user input.** A single `leader_client_fd` is updated when any non-leader sends a byte stream that passes `util.isUserInput` (which excludes mouse `CSI M`/`CSI <` and focus `CSI I`/`CSI O` so terminal back-chatter doesn't steal leadership). No debounce. Cleared, not handed off, when the leader disconnects (`zmx/src/main.zig:583, 624-631, 897-910, 1010-1011`, [[client-attach-and-election]]).
- **Leader-wins for resize**, broadcast-to-all for output, write-from-anyone for input. Non-leader resizes are silently dropped at `zmx/src/main.zig:1014`. Output goes to every client unconditionally (`:2599-2608`). All clients can write input ([[resize-semantics]], [[client-attach-and-election]]).
- **First attach gets no snapshot** to avoid clobbering shell startup; subsequent attaches get full state via `serializeTerminalState` (two-phase: scrollback first plain, then `\x1b[2J\x1b[H\x1b[0m`, then visible viewport with modes/cursor/keyboard/pwd) (`zmx/src/util.zig:479-586`, [[terminal-state-and-replay]]).
- **Wire format**: 8-byte header (`tag: u8`, `len: u32`), 14 message tags, frozen byte-size assertions in tests (`zmx/src/ipc.zig:33-36, 255-280`). No request/response correlation; pseudo-RPC is drain-until-tag-matches ([[external-protocol]]).
- **No idle timeout** on sessions. Session lives as long as the child does (`zmx/README.md:464-466`, [[configuration]]).
- **Per-client `write_buf` is unbounded** — slow clients can OOM the daemon. This is a real flaw inherited only at our peril ([[backpressure]]).

## Deliberate divergences

Where cairn intentionally moves away from zmx:

### 1. Transport: WebSocket instead of Unix sockets

Browser clients can't open Unix sockets. Cairn binds an HTTP server (loopback only by default) and serves both an HTTP control plane (for `list`, `info`, `kill`, `history` — non-streaming ops) and WebSocket endpoints for attach. CLI clients use the same transport, optionally with a Unix-socket fast path for local users ([[external-protocol]], [[transports]], [[web-vs-cli-clients]]).

WebSocket is the v0 carrier for browser and remote CLI. WebTransport (HTTP/3 over QUIC) is considered for a future revision when Safari ships stable support and the connection-migration UX win on mobile is measurably worth the deployment cost — see [[transports]] for the comparison, including why the "multi-stream + datagrams" pitch around WebTransport doesn't apply to our terminal-session traffic.

### 2. Single daemon process (v0)

All cairn sessions live in one daemon process, not one process per session.

Why this is the v0 shape:
- `libghostty-vt` instances are `!Send + !Sync` (per `lib.rs:19-23`), so each session needs its own OS thread regardless of process model. One daemon with N threads is simpler than N processes for v0.
- Browser clients need a single addressable HTTP+WS endpoint. With one daemon, the listener and the session workers share an address space — no IPC layer needed.
- Less code to ship, simpler tests, simpler observability.

Cairn's model: dedicated OS thread per session running a tokio `current_thread` runtime + `LocalSet` (the existing `crates/cairn-pty/src/pty/ghostty/worker.rs` implementation). The daemon-level executor on a separate runtime owns the HTTP/WS listeners and routes attached clients to per-session command channels ([[internal-communication]], [[daemon-process-model]]).

**v0 cost, not a load-bearing commitment**: a daemon crash takes down all sessions today, where zmx's per-session model isolates failures. [[worker-backends]] documents the migration path — four backends (in-process, local-subprocess, local-VM, remote) sharing the same `Command`-channel abstraction. The discipline that keeps the migration mechanical is: the `Command` enum is the only API to a session worker, and daemon-level code never touches the emulator or PTY directly. Until then: keep the daemon supervisor-managed (systemd / launchd) so it restarts; sessions reconnect via `cairn attach` to a freshly-spawned daemon would *not* recover their PTYs ([[error-recovery]]).

### 3. Authentication is explicit, not filesystem-based

Loopback-only bind + per-daemon bearer token in `$XDG_RUNTIME_DIR/cairn/token` (mode `0o600`), plus strict `Origin` allow-list for browser CSWSH defense.

The browser `WebSocket` constructor cannot set arbitrary headers, but the token also must not appear in the URL — query/path tokens leak to access logs, browser history, Referer headers, screenshots, and any network appliance along the path. Cairn uses **first-message authentication**: the WS opens unauthenticated, the client sends a `Hello { token, … }` frame as its first message, and the server closes the connection (policy violation, code 1008) if no valid `Hello` arrives within a short deadline (~5s). The same code path works for browser and CLI; the token never appears in URL, HTTP headers, or access logs. Origin allow-list still applies for browser CSWSH defense.

Optional Unix-socket fallback for CLI inherits zmx's filesystem-DAC model — no token check on that transport ([[authentication]]).

### 4. Post-exit normalisation

When a child exits, cairn keeps the worker alive serving `Subscribe` against the final snapshot (`worker.rs:283-300`) until the last `GhosttyPty` handle drops. zmx tears down the socket immediately on EOF (`zmx/src/main.zig:2557-2560`). This buys cairn a window for clients to fetch final output and exit status without racing the teardown — important for browser clients that may reconnect after the child died ([[pty-lifecycle]], [[error-recovery]]).

### 5. Library-first, then binaries

`cairn-pty` is currently just a Rust library exposing `GhosttyPty`. The daemon binary, CLI client binary, and web frontend are TBD. zmx is a single binary that fronts everything. We will likely have a `cairn-daemon` and `cairn` (CLI) binary in separate crates, both depending on `cairn-pty`.

### Smaller divergences worth knowing

- **Scrollback default**: cairn 1000 lines (`pty/types.rs:36`); zmx 10,000,000 (`zmx/src/main.zig:473`). Four orders of magnitude apart. Cairn's default looks low — revisit before going public ([[configuration]]).
- **Snapshot completeness**: cairn's `format_snapshot` uses default `FormatterTerminalExtra` — missing `modes`, `scrolling_region`, `pwd`, `keyboard`, `screen: .all`. Round-trip will lose bracketed_paste, application_cursor, etc. zmx's two-phase serialization is the model to copy ([[terminal-state-and-replay]]).
- **Library callbacks installed**: cairn currently installs only `on_pty_write`. Missing `on_device_attributes`, `on_xtversion`, `on_color_scheme`, `on_size`. Tier-2 queries are silently dropped today ([[query-response-delegation]]).
- **Backpressure**: cairn already uses `tokio::sync::broadcast` (bounded by message count). Better than zmx's unbounded per-client `ArrayList`. But the worker drops `tx.send()` errors today (`worker.rs:289`) — no `Lagged` handling, no per-client transport-level backpressure yet ([[backpressure]]).
- **Kill semantics**: zmx escalates SIGHUP → 500ms → SIGKILL (`zmx/src/main.zig:1046-1061`). Cairn's `child.start_kill` is unconditional SIGKILL today ([[pty-lifecycle]], [[error-recovery]]).
- **First-attach snapshot**: zmx suppresses the snapshot for the very first attach because in zmx the spawning client *is* the first attacher, so the snapshot would clobber shell startup output the client is about to render anyway. Cairn deliberately keeps the snapshot on first attach — its primary use case is long-running headless processes (AI agent management, background automation) that may run for hours before any client attaches. Suppressing the snapshot in that scenario would throw away exactly the state the user opened the terminal to see ([[client-attach-and-election]]).

## Non-goals (v0)

- **Multiplexer features**: windows, tabs, splits. Sessions are single-pane.
- **Multi-user / multi-tenant**: one daemon per user. No shared sessions across users in v0.
- **Persistence across daemon restart**: when the daemon dies, sessions die. Recovery is a possible future direction; not in v0 ([[daemon-process-model]]).
- **Cross-machine remoting**: loopback only by default. Remote access is a deployment concern, not a v0 feature ([[authentication]]).
- **Delta replay** for client reattach: every reattach is a fresh snapshot + live stream. No transcript or sequence-numbered diff stream ([[external-protocol]], [[terminal-state-and-replay]]).
- **Config hot-reload**: changes require daemon restart ([[configuration]]).

## What cairn has today

Verified by reading `crates/cairn-pty/src/pty/`:

- `GhosttyPty` library type — spawn, write, resize, size, subscribe, shutdown
- Dedicated OS thread + tokio `current_thread` runtime + `LocalSet` per session
- libghostty-vt `Terminal` per session, fed all PTY output, snapshot on Subscribe
- `tokio::sync::broadcast` for fan-out, `flume::Sender<Command>` for in
- Single-writer-to-PTY by construction (both Command::Write and `on_pty_write` callback funnel through the same select! task)
- Post-exit normalisation: Subscribe still serves snapshot, other commands return `PtyError::Closed`
- Test coverage at the worker level: `tests/pty_io.rs` (222 lines), `tests/pty_lifecycle.rs` (117), `tests/pty_resize.rs` (30)

## What needs to be built

In approximate dependency order:

1. **Complete the libghostty callback set** — wire `on_device_attributes`, `on_xtversion`, `on_color_scheme`, `on_size`. Gate all of them (including `on_pty_write`) on a primary-attached flag ([[query-response-delegation]]).
2. **Multi-client semantics** — extend `Subscribe` with a client identity, track leader by most-recent input, gate resize to leader-only ([[client-attach-and-election]]).
3. **Snapshot completeness** — port zmx's two-phase serialization with full `FormatterTerminalExtra` ([[terminal-state-and-replay]]).
4. **Daemon binary** — HTTP+WebSocket listener, session registry, routing layer between client connections and per-session workers ([[daemon-process-model]], [[internal-communication]]).
5. **Wire protocol** — binary WebSocket frames with msgpack body and a one-byte version prefix; message types Hello/Welcome/Attach/Snapshot/Output/Input/Resize/Ping/Pong/Error/Bye/Detach ([[external-protocol]]).
6. **Authentication** — bearer-token + Origin checks; Unix socket fallback ([[authentication]]).
7. **CLI client binary** — termios raw mode, Ctrl+\ detach detection, SIGWINCH propagation, signal forwarding ([[web-vs-cli-clients]]).
8. **Backpressure policy** — per-client transport backpressure via `Sink::poll_ready`, lag → close → reconnect-with-snapshot ([[backpressure]]).
9. **Observability** — `tracing` subscriber installation, per-session spans, debug endpoint ([[observability]]).
10. **Daemon-level tests** — subprocess + in-process WS client harness ([[testing]]).

## Topic docs

| File | Topic | Key finding |
|---|---|---|
| [[pty-lifecycle]] | PTY allocation, child spawn, exit detection, cleanup, post-exit normalisation | cairn keeps the worker alive after child exit for final-snapshot delivery; zmx doesn't |
| [[terminal-state-and-replay]] | libghostty state, snapshot serialization, alt-screen, scrollback | cairn uses default `FormatterTerminalExtra` — loses modes/pwd/keyboard on roundtrip |
| [[query-response-delegation]] | DA1/DA2/DSR/XTVERSION reply gating between backend and clients | cairn currently installs only `on_pty_write`; Tier-2 queries silently dropped |
| [[internal-communication]] | Channels, worker threads, concurrency invariants | Single-writer-to-PTY is structural; `Rc<RefCell<>>` is sound because of `LocalSet` |
| [[external-protocol]] | WebSocket wire protocol, message types, framing, versioning | Binary frames + msgpack + version prefix; HTTP control plane for non-streaming ops |
| [[transports]] | WS vs WebTransport vs others; mobile UX; rejected alternatives | WS primary for v0; WT's multi-stream / datagrams pitch doesn't apply to terminal sessions |
| [[client-attach-and-election]] | Attach handshake, leader selection by most-recent-input | zmx's `has_terminal_client` is sticky and never reset — known limitation to fix |
| [[resize-semantics]] | Multi-client size negotiation, leader-wins, initial size, pixel-size | zmx is unambiguously leader-wins; cairn already has the mechanism, needs the policy |
| [[backpressure]] | Slow clients, broadcast lag, browser-tab throttling | cairn's bounded broadcast is better than zmx's unbounded per-client `ArrayList` |
| [[authentication]] | Bearer tokens, Origin checks, loopback binding | Biggest divergence: filesystem DAC doesn't work over WebSocket |
| [[daemon-process-model]] | Single daemon, listener placement, session enumeration | cairn fate-shares all sessions on daemon crash — accepted v0 cost |
| [[worker-backends]] | In-process / subprocess / VM / remote backends; the spawner pattern | Same `Command`-channel everywhere; transport and connection direction are the only axes |
| [[error-recovery]] | Failure modes and policies | `PtyError` taxonomy is narrow on purpose; EOF and EIO collapse into one path |
| [[web-vs-cli-clients]] | Termios setup, signal forwarding, browser reconnect, OSC 52 | Same wire protocol; the difference is what each client does at its edges |
| [[configuration]] | Per-session and daemon-level config surface, layered loading | cairn's scrollback default of 1000 is too low; revisit before public release |
| [[observability]] | Logging, tracing, metrics, debug endpoint | `tracing` is on the dep graph but no subscriber installed |
| [[testing]] | Test strategy across worker, daemon, wire protocol, browser | cairn needs a WS client harness analogous to zmx's BATS tests over `nc -U` |

## Cross-cutting open questions

Issues that surfaced in multiple docs and likely need a holistic decision:

1. **Idle-session TTL.** Should a session with zero attached clients eventually die? zmx says no. Cairn could expose a per-session knob. Touches [[pty-lifecycle]], [[configuration]], [[daemon-process-model]].

2. **Leader-election placement.** Does the worker track leader, or does the daemon-level orchestrator? Worker is simpler (state stays with PTY); daemon is more honest (the worker doesn't know about transports). Touches [[internal-communication]], [[client-attach-and-election]].

3. **Snapshot completeness vs. session-create-time cost.** The two-phase serialization needs to capture more state to be lossless. How expensive does that get at 10M-line scrollback? Touches [[terminal-state-and-replay]], [[backpressure]].

4. **Web-friendly auth UX.** Tokens via URL query are leaky (logs, browser history). Cookies need same-origin enforcement. We may need an OAuth-style flow for embedded contexts. Touches [[authentication]], [[web-vs-cli-clients]].

5. **Privacy at trace level.** Byte-level tracing leaks passwords. zmx logs raw input bytes at debug. Cairn should gate this behind a build-time feature or runtime confirmation. Touches [[observability]], [[authentication]].

6. **Reconnect semantics for half-open WebSockets.** Browser network blips can produce zombie attached connections that the server hasn't reaped yet. How do we detect-and-replace, and how does this interact with leader election? Touches [[client-attach-and-election]], [[error-recovery]], [[backpressure]].

7. **Session ID model.** Name-based like zmx (`cairn attach dev`)? UUID + optional name (`cairn attach --name dev`)? Path-style with hierarchy? Affects URL design for browser clients. Touches [[daemon-process-model]], [[external-protocol]].

## Reading order

If you want the architecture in one sitting, read in this order:

1. This file (you're here)
2. [[daemon-process-model]] — overall topology
3. [[worker-backends]] — how that topology extends to subprocess / VM / remote
4. [[pty-lifecycle]] — the unit of work
5. [[internal-communication]] — how data moves inside the daemon
6. [[external-protocol]] — how it moves to clients
7. [[client-attach-and-election]] — the multi-client model
8. [[query-response-delegation]] — the headless-emulator decision
9. [[terminal-state-and-replay]] — what state we maintain and replay

Then the policy docs as needed: [[authentication]], [[transports]], [[backpressure]], [[resize-semantics]], [[error-recovery]], [[web-vs-cli-clients]], [[configuration]], [[observability]], [[testing]].

# Cairn CLI client — interactive attach design

## Status

Design. Defines the first slice of the `cairn` CLI client binary
(`crates/cairn-client`): the **connection layer** plus the **interactive**
commands — `cairn attach`, `cairn exec`, `cairn run` — over a Unix Domain
Socket. This is the interactive half of item 7 ("CLI client binary") of
`docs/architecture/pty-session/README.md`. The non-interactive commands
(`list`/`kill`/`send`/`logs`/`wait`/`inspect`/`rename`/`restart`/`kick`/
`whoami`/`version`) are a deliberately-separate follow-up spec.

Builds on:
- `docs/superpowers/specs/2026-05-26-daemon-binary-design.md` (the daemon this
  client drives; the `attach` bridge semantics are defined there).
- `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md` (the wire
  protocol / generated client functions).
- `docs/architecture/pty-session/web-vs-cli-clients.md` (the termios / signal /
  reconnect shape this client implements) and `client-attach-and-election.md`,
  `backpressure.md`, `error-recovery.md`.

The CLI surface already exists and is fully designed in `crates/cairn-client/src/cli.rs`
(clap derive, 15 subcommands). `main.rs` is a stub that only handles `completion`.
This spec wires up the interactive subset and makes two edits to `cli.rs` (below).

## Scope (v0)

- **Commands: `attach`, `exec`, `run` only.** Plus the shared connection layer
  they all need. Every other subcommand returns from the existing stub / is
  deferred to the non-interactive follow-up spec.
- **Transport: UDS only.** `--daemon unix:///path` or the platform default
  socket. `ws://` / `wss://` (and `--token`) return a clear "remote transports
  not yet supported in v0" error. Matches the daemon's UDS-only v0.
- **Signal model: the detach-camp consensus.** The attach client traps only
  SIGWINCH (→ resize); every other client-received signal leads to a *clean
  detach* (restore terminal, drop connection, session survives). Nothing is
  forwarded to the child. This matches zmx, dtach, abduco, and shpool (all four
  verified from source). See [Signal handling](#signal-handling).
- **Reconnect-with-snapshot on transient loss.** An unexpected stream end
  (transport drop or a daemon lag-kick) transparently re-attaches and repaints
  from a fresh snapshot, rather than ejecting the user. See
  [The attach driver](#the-attach-driver).
- **No daemon auto-spawn.** The daemon is service-managed (matching the daemon
  spec); a missing daemon is a clear error, not an auto-`ensureSession`.
- **Deferred:** transparent signal forwarding (`--sig-proxy`), WebTransport +
  token auth, daemon auto-spawn, SIGTSTP/CONT suspend-resume hygiene, exotic
  Kitty-CSI-u variants, bracketed-paste-aware detach suppression. See
  [Deferred / future work](#deferred--future-work).

### `cli.rs` edits

1. **Remove `--sig-proxy`** from both `Attach` (`cli.rs:106-114`) and `ExecArgs`
   (`cli.rs:413-421`). v0 commits to the detach-camp model; the transparent
   forwarding the flag implied is recorded as future work, not shipped inert.
2. Soften the `Attach` help text "Requires the client to have an interactive
   terminal" (`cli.rs:76`) — see [Non-TTY behavior](#non-tty-behavior); we
   degrade gracefully instead of hard-requiring a TTY.

## Architecture (Approach A: dedicated stdin thread + tokio reactor)

The client is async (tokio + the wRPC futures), but raw-terminal input must not
go through tokio's stdio adapters: those wrap fd 0 in a blocking thread pool and
mask `EAGAIN`, making detach-hotkey latency unpredictable
(`web-vs-cli-clients.md`). So a single dedicated `std::thread` does blocking
reads on `STDIN_FILENO` and ships bytes over an `mpsc` to the async driver;
everything else (SIGWINCH, the termination signals, the wRPC streams, the
reconnect loop) is tokio.

Crucially this **never sets `O_NONBLOCK` on fd 0** — that flag lives on the
shared open file description and would leak to the parent shell after exit,
breaking its subsequent reads. Rejected alternatives: `tokio::io::unix::AsyncFd`
on a non-blocking stdin (the `O_NONBLOCK`-leak footgun + the adapter the doc
warns against); `crossterm`'s event stream (it parses input into key/mouse
*events*, but a passthrough multiplexer client must forward raw bytes verbatim —
escape sequences, bracketed paste, mouse reports, Kitty protocol — so
re-encoding parsed events loses fidelity).

The stdin reader thread is spawned once, feeds the driver across reconnects, and
is simply leaked at process exit (the process is dying; a blocking `read()` on
fd 0 can't be cleanly cancelled, and there's nothing to clean up).

### Module layout

```
crates/cairn-client/src/
  main.rs       # parse CLI, ignore SIGPIPE, build tokio runtime, dispatch.
                #   `completion` already handled (main.rs:10-15).
  cli.rs        # exists. Edits: remove --sig-proxy; soften attach help.
  connect.rs    # Endpoint: --daemon URI -> wrpc_transport::unix::Client; default
                #   socket resolution; ws/wss + token -> "not yet supported".
  terminal.rs   # RawGuard (nix termios save/restore + RIS), is-a-tty checks,
                #   TIOCGWINSZ size query, screen-clear helper, stdout writer.
  detach.rs     # detach-key parse (docker-style) + matcher (raw byte + Kitty CSI-u).
  signals.rs    # SIGPIPE ignore; SIGWINCH + termination-signal streams; classification.
  attach.rs     # the attach driver: event sources, select loop, reconnect.
  exec.rs       # ExecArgs -> SessionSpec; create; optionally hand off to attach.
```

`main.rs` builds a multi-thread tokio runtime (like `cairn-daemon`'s `main.rs:48`)
and maps `-v/-vv/-vvv` to a `tracing_subscriber` `EnvFilter` on stderr (never
stdout — stdout is the session's output channel).

### Connection layer (`connect.rs`)

```rust
enum Endpoint { Unix(PathBuf) }            // v0: the only variant that resolves

fn resolve(cli: &Cli) -> Result<Endpoint>  // --daemon / CAIRN_DAEMON / default
```

- **Default** (no `--daemon`): `$XDG_RUNTIME_DIR/cairn/cairn.sock`, else
  `$TMPDIR/cairn/cairn.sock`, else `/tmp/cairn/cairn.sock` — byte-identical to
  the daemon (`config.rs:30-36`) and webtest (`examples/webtest.rs:37-46`).
- `unix:///abs/path` or a bare absolute path → `Endpoint::Unix`. Build clients
  with `wrpc_transport::unix::Client::from(path)` (the proven pattern —
  `webtest.rs:35,49`, `smoke.rs:30`). The client is cheap to clone and holds
  only the path; **each wRPC invocation opens a fresh UDS connection**, so
  `attach` holds exactly one connection for the session's lifetime and a
  reconnect is a brand-new invocation.
- `ws://` / `wss://` → `Err` ("remote transports (WebTransport) are not yet
  supported; v0 is unix-socket only"). `--token` with a unix endpoint is ignored
  (already documented in `cli.rs:30-35`).
- **Connect failure** on the first call: a clear message
  (`cannot reach cairn-daemon at <path>: <io error> — is it running?`), exit
  non-zero. No auto-spawn.

A small `Client` factory (clone of the path-based client) is threaded into the
driver so reconnects mint fresh connections.

### Terminal guard (`terminal.rs`)

`RawGuard` owns the terminal-state side effects with RAII so every exit path
(detach, child-exit, error, trapped signal, panic) restores cleanly.

```rust
struct RawGuard { original: Option<Termios> }   // None => stdin wasn't a TTY

impl RawGuard {
    fn new() -> Self {                           // tcgetattr(STDIN); if it fails,
        // stdin is not a TTY -> passthrough mode, no raw, no restore.
        // else: save `original`, apply cfmakeraw + VMIN=1/VTIME=0 via TCSANOW.
    }
}
impl Drop for RawGuard {                          // only if `original` is Some:
    fn drop(&mut self) {
        // tcsetattr(STDIN, TCSAFLUSH, &original)  // discards pending input
        // write STDOUT: "\x1bc" (RIS)             // reset alt-screen/mouse/bracketed-paste
    }
}
```

- `cfmakeraw` already clears `ISIG` and `IEXTEN`, so Ctrl-C / Ctrl-Z / Ctrl-\\ /
  Ctrl-V all pass through as raw bytes — no per-key termios overrides needed
  (this is *why* typed control chars never become client signals; see
  [Signal handling](#signal-handling)).
- **Size query**: `TIOCGWINSZ` (via `nix::libc` ioctl) on stdout → `(cols, rows)`.
  Used for `attach-init` and on every SIGWINCH. Pixel dimensions are read but
  unused in v0 (the `Resize` wire type carries only `(cols, rows)`).
- **Clean canvas**: before the first attach, and before painting each
  post-reconnect snapshot, write `\x1b[2J\x1b[H` so snapshot replay doesn't
  overlay stale screen content (`web-vs-cli-clients.md` bootstrap step 4).
- **stdout writer**: a small buffered writer that does direct blocking
  `write(2)` on fd 1 (no tokio stdio adapter), flushing per server-event batch.
  A blocking write to a slow *pipe* could stall the driver loop; for a TTY it is
  negligible. v0 accepts this — the daemon's lag-kick (`backpressure.md`) is the
  real backpressure mechanism. Flagged in [Open questions](#open-questions).

## The attach driver

The heart. Defined in `attach.rs`; reused verbatim by `exec`/`run`'s non-detached
path. The wire op is `sessions.attach(id, init: attach-init, events: stream<client-event>)
-> stream<server-event>` (`wit/cairn.wit:100-101`); the daemon side is defined in
the daemon spec and implemented in `crates/cairn-daemon/src/handlers/attach.rs`.

### Generated client signature (to confirm against codegen)

Following the established wit-bindgen-wrpc shapes — `logs` (server-stream)
returns `(stream, Option<io_future>)` (`webtest.rs:309-316`) and `send`
(client-stream) takes an input `Stream` arg (`webtest.rs:344-347`) — the bidi
`attach` client function is expected to be:

```rust
cairn_protocol::client::cairn::daemon::sessions::attach(
    &client, (), &id, &init,
    events: impl Stream<Item = Vec<ClientEvent>> + Send + 'static,
) -> anyhow::Result<(
    impl Stream<Item = Vec<ServerEvent>> + Send,   // server events
    Option<impl Future<Output = anyhow::Result<()>> + Send>,  // io pump, must be driven
)>
```

The exact generic bounds are confirmed against the generated code during
implementation; the driver is written against this shape.

### State model

**Persistent** (constructed once, survives every reconnect):

- The cloneable UDS `Client` factory + the **resolved** `session_id`. `attach`
  takes a concrete `session-id`; the daemon resolves name-or-id
  (`daemon spec, Identity & resolution`). `--latest` has no wire representation,
  so the client resolves it up front via `sessions.list_all`, picks the max
  `created_at_unix_ms`, and uses that id.
- One `RawGuard` (held for the whole command).
- The detach-key `Matcher` (carries partial-match state).
- The stdin reader thread → `mpsc::Receiver<Bytes>`.
- The SIGWINCH stream and the termination-signal streams (`signals.rs`).

**Per-connection** (rebuilt on each reconnect):

- `attach-init { cols, rows, no_stdin }` from the current `TIOCGWINSZ`.
- A fresh `mpsc::channel::<Vec<ClientEvent>>`; its `Receiver` (as a
  `ReceiverStream`) is the `events` arg to `attach`. The driver pushes
  `ClientEvent`s into the `Sender`.
- The returned `(server_stream, io_future)`; the `io_future` is `tokio::spawn`ed.

### Select loop

After invoking `attach`, the driver writes the clean-canvas clear, then enters
`tokio::select!`:

| Arm | Action |
|---|---|
| stdin `Bytes` (from the reader thread) | Feed the detach `Matcher`. It emits `Input(bytes)` to forward (possibly after flushing a withheld partial-match prefix) → push `ClientEvent::Input` on the events channel. On a full detach match → push `ClientEvent::Detach`, set outcome `Detached`, break. Dropped entirely if `no_stdin`. |
| SIGWINCH | Re-query `TIOCGWINSZ` → push `ClientEvent::Resize((cols, rows))`. |
| termination signal (INT/TERM/QUIT/HUP/USR1/USR2) | Set outcome `Detached`, break (the `Drop` guard restores the terminal; session survives). See [Signal handling](#signal-handling). |
| `server_stream.next()` → `Some(batch)` | For each `ServerEvent`: `Snapshot`/`Output` → write bytes to stdout (+ flush). `Exited(status)` → outcome `Exited(status)`, break. `Error(e)` → if `e.code == CLIENT_LAGGED` → outcome `Reconnect`, break; else outcome `Fatal(e)`, break. |
| `server_stream.next()` → `None` | Stream ended with no terminal event → outcome `Reconnect`, break (transport drop). |
| spawned `io_future` resolves | Transport finished → outcome `Reconnect`, break. |

### Outcome classification & reconnect

```
Detached            -> restore (Drop), exit 0
Exited(status)      -> restore, exit with status.code, or 128+signal if signal-killed
Fatal(error)        -> restore, eprintln mapped message, exit non-zero (1)
Reconnect           -> if !give_up: backoff, re-attach, repaint; else exit non-zero (1)
```

`Reconnect` is the [reconnect-with-snapshot decision](#scope-v0): exponential
backoff (~100ms → 2s cap), **silent** so it doesn't corrupt the rendered display
(the fresh snapshot repaints on success; the backoff and the give-up budget both
reset on a successful reattach). Give up — restore the terminal and exit non-zero
with `connection lost` — when either:

- the socket path has vanished (the daemon is gone — immediate, not subject to
  the budget); or
- the **retry budget** is exhausted: a maximum total elapsed time spent
  reconnecting since the last successful attach, configurable via
  `CAIRN_RECONNECT_TIMEOUT` (a humantime duration; default `30s`; `0` / `off`
  retries indefinitely). Expressed as elapsed time rather than a raw retry count
  so the bound is intuitive under the capped exponential backoff.

The `CLIENT_LAGGED` vs `CLIENT_KICKED` distinction is what makes reconnect-safe:
both a lag-kick and an operator `cairn kick` currently close the stream with *no
final event* (`handlers/attach.rs:75,83`), so they are indistinguishable on the
wire — auto-reconnect would silently defeat `cairn kick`. This spec adds distinct
terminal events on the daemon side (see [Daemon-side changes](#daemon-side-changes))
so the client rule is clean: `Error{client.lagged}` → reconnect; `Error{client.kicked}`
(and every other error) → terminal; bare `None` / transport error → reconnect.

## Detach-key parsing & matching (`detach.rs`)

`--detach-keys` (default `ctrl-q,ctrl-q`, `cli.rs:95-96,406-407`) is parsed at
startup (before going raw, so a parse error is a clean CLI error):

- Split on `,`. Each token is either `ctrl-<c>` → byte `ascii(c) & 0x1f`
  (`ctrl-q` → `0x11`, `ctrl-a` → `0x01`), or a single literal char → its byte
  (`d` → `0x64`). Produce a `Vec<DetachKey>` where each `DetachKey` carries the
  expected raw byte and the key's `(unicode_code, ctrl)` for CSI-u matching.
- Reject empty / malformed tokens with a clear message.

**Matcher** — docker/tmux model, recognizing **two encodings** at each sequence
position, because an inferior program inside the session can flip the *outer*
terminal into Kitty keyboard mode via transparent passthrough (it writes
`CSI > flags u`, those bytes flow `child → PTY → daemon → output → client →
outer terminal`), after which the user's keystrokes — including the detach key —
arrive as CSI-u sequences, not raw control bytes. zmx handles exactly this
(`util.zig:354-357,880-941`); `web-vs-cli-clients.md`: "must detect both — a
terminal in Kitty mode never delivers the raw 0x1C."

- **(a) Raw control byte**: `b == seq[matched].raw_byte` (fast path).
- **(b) Canonical Kitty CSI-u *press*** `\x1b[<code>;<mods>u`, where `mods = 1 +
  4·ctrl` (ctrl-q → `\x1b[113;5u`), tolerating and ignoring an optional
  `:event-type` suffix. When a position could match CSI-u and the stream begins
  `\x1b[`, buffer a bounded in-flight event (up to ~16 bytes through the
  terminating `u`); parse `code;mods`; if it matches → advance; if it parses to a
  non-matching key event, or never terminates within the bound, or isn't CSI-u →
  flush the buffered bytes as `Input` and reset.

Matching both forms unconditionally is safe (the two byte-strings never collide),
so the matcher does **not** track the terminal's mode-stack state.

On a position mismatch, the withheld partial-match prefix is flushed as `Input`
(it was real keystrokes), `matched` resets to 0, and the current byte is re-tested
against `seq[0]`. Consequence: mid-partial-match, bytes are briefly withheld from
the session until the next keystroke disambiguates — one keystroke of latency on
a rare prefix, the standard docker tradeoff; no flush-timeout in v0. The multi-key
default also makes accidental triggers inside a paste unlikely.

Residual edge cases deferred: exotic modifier / alternate-key codes, the
associated-text flag (16), shift/alt-combined detach keys, and bracketed-paste
suppression. Default `ctrl-q,ctrl-q` is fully covered.

## Signal handling (`signals.rs`)

v0 commits to the **detach-camp consensus** verified across all four reference
tools — zmx (`main.zig` `clientLoop`, WINCH self-pipe only), dtach (`attach.c`,
WINCH + HUP/TERM/INT/QUIT→restore-and-`die`), abduco (`abduco.c`, WINCH only),
shpool (`attach.rs:435`, WINCH only). None forward client-received signals to the
session; killing/closing the client *is* a detach and the session survives. cairn
matches this because it has an explicit detach key as the primary "leave but keep
running" mechanism, and its headline use case is long-running headless jobs that
must survive client teardown.

The one refinement over abduco/shpool (and matching dtach): we **trap** the
termination signals rather than letting them default-kill the process, purely so
the terminal is restored cleanly before exit-as-detach.

| Signal | v0 behavior |
|---|---|
| SIGPIPE | Ignored process-wide at startup (matches zmx `main.zig:78`) — don't die when a write races a closed fd/socket. |
| SIGWINCH | `tokio::signal::unix` stream → resize arm (re-query size, send `ClientEvent::Resize`). |
| SIGINT, SIGTERM, SIGQUIT, SIGHUP, SIGUSR1, SIGUSR2 | Trapped → **graceful detach**: break the driver loop so `RawGuard::drop` restores the terminal, then exit 0. Nothing forwarded to the child; session survives. |
| SIGTSTP, SIGCONT | **Default disposition** (deferred). Typed Ctrl-Z is already forwarded to the child as byte `0x1a` (raw mode), and the intended return-to-shell path is the detach key, so the only exposure is an *external* `kill -TSTP`, which would leave the terminal raw while frozen (SIGSTOP is uncatchable and unfixable regardless). Full suspend/resume hygiene is future work. |
| SIGKILL, SIGSTOP | Uncatchable. SIGKILL on the client force-detaches (the daemon reaps the dropped connection); session survives. |

Why typed control chars don't double-up: in raw mode `ISIG` is off, so typed
Ctrl-C/Z/\\ are *bytes* forwarded to the PTY (the daemon-side line discipline
turns them into the child's signals). The client therefore only ever receives
INT/TERM/QUIT as *external* signals — there is no typed-vs-external collision.

The transparent / `--sig-proxy` model (forward client-received signals to the
child via `sessions.kill`, with a SIGHUP=detach carve-out) is recorded as
[future work](#deferred--future-work), not shipped.

## `exec` / `run` (`exec.rs`)

Both build a `SessionSpec` from `ExecArgs` and differ only in `-i`/`-t` defaults
(`exec` off/off, `run` on/on), resolved via the existing helpers
`interactive_with_default` / `tty_with_default` (`cli.rs:436-458`):

```rust
SessionSpec {
    name: args.name,                          // None -> daemon infers (see below)
    command: args.command,                    // empty -> daemon default shell
    env: merge_env(&args)?,                    // env-file(s) then -e; see below
    env_inherit: !args.no_inherit_env,
    workdir: args.workdir.or_else(cwd),        // else the client's current_dir()
    tty: args.tty_with_default(default),       // exec:false run:true
    stdin: args.interactive_with_default(default),
    idle_timeout_secs: args.timeout.map(|d| d.as_secs()),
    scrollback_lines: 1000,                    // no ExecArgs flag; daemon/config default
}
```

- **env merge** (`merge_env`): parse each `--env-file` in order (dotenv:
  `KEY=VALUE`, skip `#`-comments and blank lines), then apply `-e` (`KEY=VALUE`
  sets; bare `KEY` copies from the client's environment if present). Later `-e`
  overrides earlier. The result is the explicit `env` list. **Precedence rule
  (daemon-side, see below): explicit overrides inherited.**
- **workdir**: `--workdir`, else the client's `std::env::current_dir()`
  (`cli.rs:351-355` — client cwd on the same machine as the daemon).

Then `sessions.create(spec)` (`webtest.rs:228` is the call shape). On
`Ok(Err(e))` → print `e.code: e.message`, exit non-zero.

- **`--detach`** (`cli.rs:343-344`): print the new session's name + id and a
  `cairn attach <name>` hint, exit 0. Do **not** attach. (The session is still
  created with its tty/stdin per the command default, so a later attach is
  interactive — mirroring `docker run -dit`.)
- **Otherwise**: hand the created session's id to the attach driver with
  `no_stdin = !stdin_resolved` and `detach_keys` from `args`.

## Daemon-side changes

This spec touches the daemon in three small, bounded places. All are in
`crates/cairn-daemon` (+ a shared const in `crates/cairn-protocol`).

### 1. Distinct terminal events for kick vs lag (`handlers/attach.rs`)

So the auto-reconnecting client can tell an operator kick (stay gone) from a
lag-kick (reconnect):

- The lag arm (`handlers/attach.rs:75`, `Err(RecvError::Lagged(_)) => return`)
  emits `ServerEvent::Error { code: CLIENT_LAGGED, .. }` **before** returning.
- The kick arm (`handlers/attach.rs:83`, `_ = &mut kick_rx => return`) emits
  `ServerEvent::Error { code: CLIENT_KICKED, .. }` **before** returning.

Add the contract as shared consts in `cairn-protocol` (e.g.
`error_codes::{CLIENT_KICKED, CLIENT_LAGGED}` = `"client.kicked"` /
`"client.lagged"`) so neither side hard-codes the strings. The existing
`kick_ends_attached_stream` test (`daemon_streaming.rs:226-247`) still passes — it
drains to `None`, now consuming the one extra `Error` event first.

### 2. env precedence: explicit overrides inherited

When `env_inherit` is true, the child's environment is the inherited process
environment **overlaid by** the spec's explicit `env` (explicit wins on key
collision). This resolves the contradiction between the two `cli.rs` doc-comments
(`cli.rs:358-378`). Lives where `SessionSpec` → `SpawnOptions` env is constructed
(`cairn-daemon/src/spawn.rs`); confirm/adjust the current merge order there.

### 3. Default name inference

When `create` receives `spec.name == None`, the daemon infers
`{basename}-{suffix}`:

- `basename` = file-stem of `spec.command[0]` (e.g. `/usr/bin/bash` → `bash`), or
  the basename of the default shell when `command` is empty.
- `suffix` = the **last 6 hex chars** of the session's UUIDv7 (its random tail —
  *not* the leading bits, which are the millisecond timestamp and are nearly
  identical for sessions created close together). E.g. `bash-3f9ac2`.
- Always appended (no instance-counting). On the astronomically rare suffix
  collision with an existing live name, extend the slice by more hex chars until
  unique — collision-driven only, not per-prefix counting.

Lives in `registry.create` (`registry.rs`), where the id is minted and name
uniqueness is already enforced.

## Exit codes

| Outcome | Code |
|---|---|
| Detach (key or trapped signal) | 0 |
| Child exited normally | the child's exit code |
| Child killed by signal | `128 + signal` (shell convention) |
| Fatal in-band error (`client.kicked`, `session.not_found`, `pty.backend`, …) | 1 |
| Daemon unreachable / reconnect give-up | 1 |
| CLI usage / parse error | clap's 2 |

## Non-TTY behavior

Honors `web-vs-cli-clients.md`'s "degrade gracefully" guidance rather than
hard-failing (hence the `cli.rs:76` help-text softening):

- **stdin not a TTY** (`tcgetattr` fails): no raw mode, no detach-key handling.
  Still stream output; forward piped stdin as `Input` if present (`cat | cairn
  attach`); resize uses the initial size only.
- **stdout not a TTY**: warn on stderr (`stdout is not a terminal; output will
  include raw escape sequences`), then proceed dumping `Output` bytes verbatim —
  `script(1)`-style capture (`cairn attach foo > log.txt`).

## Testing

Per the project's test discipline — assert behavior through the real interface,
never restate structure.

**Pure-logic units** (the highest-value, fully-deterministic pieces):
- detach `Matcher`: raw-byte match; Kitty-CSI-u match (`\x1b[113;5u` → detach for
  `ctrl-q`); partial-match-then-mismatch flushes the withheld bytes as `Input`;
  a `0x11` *inside* a longer non-matching run does not falsely detach; multi-key
  sequence requires both keys.
- `--detach-keys` parsing: `ctrl-q,ctrl-q`, `ctrl-a,d`, literals; malformed
  tokens error.
- `merge_env`: env-file then `-e` precedence; bare `KEY` passthrough; comments.
- `connect::resolve`: default path, `unix://`, `ws://` → error.
- signal → action classification.

**Handler-level** (extend `daemon_streaming.rs`): the kick arm now emits
`Error{client.kicked}` then ends; default-name inference yields `{basename}-{6hex}`
and is unique; env precedence (explicit wins) observed through a created session.

**Integration — PTY harness** (the zmx-BATS analog, `web-vs-cli-clients.md`):
spawn the real `cairn-daemon` on a tempdir socket; `openpty` (`nix::pty`); launch
the `cairn` binary with its stdio wired to the pty slave; drive input bytes; read
rendered output; send the detach sequence; assert clean exit **and** that the
session survives (`inspect` still shows it running). Plus `exec`/`run`
create-then-attach. The reconnect / lag paths are hard to force deterministically
— best-effort.

## Deferred / future work

Recorded here and in `README.md`'s build list / `web-vs-cli-clients.md` open
questions:

- **Non-interactive commands** (`list`/`kill`/`send`/`logs`/`wait`/`inspect`/
  `rename`/`restart`/`kick`/`whoami`/`version`) — the follow-up CLI spec.
- **Transparent signal forwarding** (`--sig-proxy`): forward client-received
  INT/TERM/QUIT/USR1/USR2 to the child via `sessions.kill` (stay attached, exit
  on child-exit), with a SIGHUP=detach carve-out to protect headless sessions.
  The divergent-from-prior-art model; deferred behind the detach-camp default.
- **WebTransport / `ws` transport** + bearer-token first-message auth (the
  `--daemon ws://…` / `--token` paths in `cli.rs`).
- **Daemon auto-spawn** (zmx's `ensureSession`) for a zero-setup UX.
- **SIGTSTP/SIGCONT suspend-resume hygiene** (vim/less pattern).
- **Exotic Kitty-CSI-u variants** (event-types, alternate keys, associated-text
  flag, shift/alt-combined detach keys) and **bracketed-paste-aware** detach
  suppression.
- **Resize pixel dimensions** (XTWINOPS `CSI 14/16 t`) in the `Resize` frame.

## Open questions

1. **Blocking stdout writes vs a slow pipe.** Direct `write(2)` on fd 1 can stall
   the driver loop if stdout is a slow consumer (not a TTY). v0 relies on the
   daemon's lag-kick for backpressure; a dedicated output task is the escape
   hatch if this bites.
2. **`exec`/`run` with `tty: false`.** A non-TTY session is still a PTY-backed
   worker; confirm the interactive attach path degrades sensibly when the session
   itself was created without a controlling TTY.
3. **`-e KEY` passthrough scope.** v0 copies a bare `-e KEY` from the client env.
   Confirm this is wanted (docker does it) vs requiring `KEY=VALUE` only.

(The reconnect give-up policy is resolved: a `CAIRN_RECONNECT_TIMEOUT`-bounded
elapsed-time budget, default `30s` — see [the reconnect
section](#outcome-classification--reconnect).)

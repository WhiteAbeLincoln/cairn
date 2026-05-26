# Daemon Process Model

How session-owning processes are launched, discovered, and torn down.
zmx is the reference; cairn diverges deliberately because the WebTransport
remote carrier and embedded `libghostty-vt` make zmx's per-session-process
shape inappropriate. Closely related: [[pty-lifecycle]],
[[internal-communication]], [[external-protocol]],
[[client-attach-and-election]], [[error-recovery]].

## zmx baseline: one daemon per session

There is no single zmx daemon. Each session is its own OS process,
spawned lazily from a foreground CLI invocation. The pattern lives
inside `Daemon.ensureSession`
(`/Users/abe/Projects/zmx/src/main.zig:738`):

1. The CLI builds a socket path under `socket_dir`
   (`main.zig:172`, `504`).
2. `ensureSession` stats the socket file
   (`socket.zig:46` → `socket.sessionExists`).
3. If the socket exists and `connect()` succeeds, the CLI is "client #2";
   it does **not** fork. If the file is there but `connect()` returns
   `ConnectionRefused`, the socket is deleted as stale
   (`main.zig:756`, `socket.zig:39`) and the create path runs.
4. On create, the *foreground* process `bind()`s and `listen()`s on the
   socket (`socket.zig:57–72`), then a single `posix.fork()`
   (`main.zig:777`).
5. The child `posix.setsid()`s (`main.zig:780`), redirects stdio to
   `/dev/null` (`main.zig:789–805`), closes inherited fds 3–63 except
   the listening socket and the socket-dir fd (`main.zig:818–825`),
   `forkpty()`s the shell (`main.zig:700`), and enters `daemonLoop`
   (`main.zig:2460`).

This is a *single* `fork()` plus `setsid()` — not the classical
double-fork. The parent CLI returns after a 10 ms sleep
(`main.zig:865`) and `connect()`s as a regular client.

The implication that matters for cairn: **the listening socket is
created by the parent, not the daemon**. The parent `bind`s and
hands the bound fd to the forked child by inheritance, sidestepping
any race where the CLI would otherwise try to attach before the
daemon binds.

## Socket / listener placement

zmx resolves `socket_dir` (`main.zig:504–516`) by precedence:
`$ZMX_DIR`, then `$XDG_RUNTIME_DIR/zmx`, then `$TMPDIR/zmx-$UID`
falling back to `/tmp/zmx-$UID`. Directory is `mkdir`d at `0750`
(`main.zig:474`, `524`); each session is a file inside it whose name
equals the session name (`socket.zig:81–94`). The kernel limit on
`sockaddr_un.path` (108 on Linux, 104 on macOS — `socket.zig:77`)
bounds session-name length, computed dynamically by `maxSessionNameLen`
(`socket.zig:115`). No separate registry file: the directory listing
**is** the registry.

## Session enumeration / discovery

`zmx list` is implemented as a directory iteration, not a query against
any central process. `util.get_session_entries`
(`/Users/abe/Projects/zmx/src/util.zig:31`) opens `socket_dir`, iterates
entries, filters to Unix-socket nodes via `fstatat`
(`socket.zig:46`), then for each candidate sends an `Info` IPC
(`util.zig:53`, `ipc.probeSession`) to read pid, client count, command,
cwd. If a probe returns `ConnectionRefused`, the socket file is unlinked
(`util.zig:67–69`); any other error leaves the file alone — a busy daemon
can miss the probe timeout, and deleting its socket would permanently
orphan it (`util.zig:64–66`).

Filesystem-as-registry is crash-safe and lock-free for readers, but
enumeration scales linearly and touches every socket — unsuitable as
a UI hot path. zmx accepts that because the CLI is the only consumer.

## Startup model: on-demand, never service-managed

zmx is on-demand. No systemd unit, no launchd plist, no "zmx daemon"
subcommand. The first `zmx attach <name>` creates its process;
subsequent invocations connect to the existing socket
(`main.zig:738–870`). Every CLI invocation is potentially a daemon
parent.

## Shutdown

A zmx session daemon exits when:

- The shell under the PTY exits (EOF on the master,
  `main.zig:2557–2560`). `daemonLoop` breaks; the deferred block at
  `main.zig:849–859` runs `handleKill`, waits on the child, closes
  the socket fd, and unlinks the socket file.
- A client sends a `Kill` IPC (`main.zig:1046`). `handleKill` calls
  `shutdown()`, then `SIGHUP`s the process group, sleeps 500 ms, then
  `SIGKILL`s it (`main.zig:1054–1060`).
- The last client disconnects **only if** that session was opened
  with `shutdown_on_last` semantics (`main.zig:622–641`). Normal
  `attach` sessions are sticky — losing the last client does not kill
  them; that is the whole point of session persistence.
- A signal handler trips the self-pipe (`main.zig:58`,
  `main.zig:2729`). The poll loop wakes and the daemon shuts down
  cleanly.

## What happens when the daemon dies with live sessions

In zmx, **sessions ARE daemons**, so "the daemon died" and "the
session died" are the same event. The PTY master closes, the shell
gets `SIGHUP` from the kernel because its controlling tty went away,
the socket file is left behind, and the next `zmx list` or `zmx attach`
deletes it as stale (`util.zig:67`, `main.zig:756`). There are no
zombie sessions or orphaned PTYs in a single-session sense, but a
kernel-killed daemon (e.g. OOM) does leave a stale socket file — the
next operation cleans it up.

## Cairn: single long-running daemon, sessions as objects

Cairn cannot copy zmx's process-per-session shape. The WebTransport
remote carrier needs an HTTP/3 endpoint (port, TLS keys for the web
client) — duplicating that per session is absurd. Add that
`libghostty-vt` is `!Send + !Sync` (see [[internal-communication]])
and the embedded emulator dictates one host process anyway.

The design is therefore **one cairn daemon process per user, holding
all of that user's sessions** as `Arc<dyn PtySession>` values
(`crates/cairn-pty/src/pty/session.rs`). Each session owns its own
dedicated OS thread running a `current_thread` tokio runtime
(`crates/cairn-pty/src/pty/ghostty/worker.rs:62`), isolating
`libghostty-vt`'s thread affinity from the rest of the daemon.

Listener: the daemon binds two endpoints —

- A WebTransport (HTTP/3 over QUIC) endpoint on `127.0.0.1:PORT` (or
  a configured non-loopback address); browser and remote CLI clients
  both connect via wRPC over `wrpc-transport-web`.
- A Unix socket at `$XDG_RUNTIME_DIR/cairn/cairn.sock` (Linux) or
  `$TMPDIR/cairn/cairn.sock` (macOS) for local CLI clients, also speaking
  wRPC (`wrpc-transport`'s `net` feature).

Both surfaces feed the same in-process session registry through wRPC
`Handler` impls of `cairn:daemon@0.1.0` (`crates/cairn-protocol/`).
There is no separate HTTP control plane; every operation —
list-all, kill, exec, attach, send, logs — is a wRPC invocation on
one of these endpoints (see [[external-protocol]], [[transports]]).

## Session id / name model

zmx uses bare string names (`zmx attach dev` opens or creates `dev`)
because the directory is the namespace and the name is the
socket-file basename (`socket.zig:12–29`).

Cairn should use **UUIDv7 id + optional name**. Rationale:

- The web UI lists sessions in a sidebar; URLs are
  `/s/<uuid>` and survive renames.
- Names are human labels, mutable, and not required to be unique
  across a daemon's lifetime (or across users).
- UUIDv7's time-ordered prefix gives free chronological sort for
  enumeration without a separate `created_at` index.
- CLI accepts either: `cairn attach dev` resolves the *most recent
  unkilled* session named `dev`; `cairn attach 0193…` matches by id
  prefix.

## Multi-user

Per-user daemon. Each user runs their own cairn process under their
own uid, binds their own port (or unix socket under their own
`$XDG_RUNTIME_DIR/cairn/`), and owns their own PTYs. Mirrors zmx's
uid-stamped `socket_dir` (`/tmp/zmx-$UID`, `main.zig:513`) and avoids
the security questions a system-wide daemon would raise by holding
other users' PTY masters. A system-wide daemon is out of scope; if
multi-tenant hosting is ever wanted, that belongs in a separate
front-door process that spawns per-user cairn instances.

## Crash recovery and the central tradeoff

This is where cairn's choice has the highest cost.

zmx: each session is its own process. If the supervising shell or the
session daemon crashes, *only that session* is lost; siblings keep
running. There is nothing to recover because there is no shared
state.

cairn: one daemon holds N sessions. If the daemon crashes, **all N
sessions die simultaneously** — the PTY masters close, the shells
under them receive `SIGHUP`, and the libghostty terminal state is
gone. This is the price of in-process emulator embedding.

Mitigations (tracked in [[error-recovery]]):

- Run as a `systemd --user` / `launchd` user service with
  `Restart=on-failure`. Crashes restart the daemon promptly; the
  sessions inside are lost but the daemon comes back.
- Catch single-session worker panics at the supervisor boundary
  (`tokio::task::JoinError` must not propagate to the listener task),
  surface as a `SessionDead` event, and keep the rest of the daemon
  up.
- Ghostty terminal state is in-memory only; PTY output replay across
  daemon restarts is intentionally **not** supported — serialising
  `libghostty-vt`'s internal grid is not a supported operation. See
  [[terminal-state-and-replay]].

Cairn buys multi-client web attach with this fate-sharing **in v0**.
The single-daemon shape is a simplicity choice, not a load-bearing
architectural commitment: a routing front-end + one process per
session would recover zmx-style isolation while keeping the shared
listener endpoints and per-session `libghostty-vt` thread-affinity intact.
See [Multi-daemon migration path](#multi-daemon-migration-path) for
the discipline that keeps this option open.

## Multi-daemon migration path

> **See also**: [[worker-backends]] for the broader story. The
> "local-subprocess worker" backend described here is one of four
> backends (in-process, local-subprocess, local-VM, remote) that
> share the same `Command`-channel abstraction. The discipline
> below is what keeps all of them reachable.

If we later need durability against frontend-process crashes (most
likely in WT/UDS-accept/auth routing code, which is statistically larger
than session-worker code), the path is **a routing frontend that
holds the WT and UDS listeners and auth, plus one session-daemon
process per session communicating with the frontend over a local
socket**.
This is zmx-with-a-C&C-node: each session keeps its own PTY and
`libghostty-vt` in its own process, but clients still talk to one
endpoint.

What this would buy beyond single-daemon:

- Frontend-process crash (the statistically common case in early
  development) does not kill sessions; clients reconnect to the
  restarted frontend.
- A `libghostty-vt` segfault or memory corruption in one session is
  bounded to that process — siblings keep running.
- Memory leaks accumulate against one session rather than the
  daemon.

What this would cost:

- An IPC layer between frontend and session-daemons. wRPC over UDS
  is the natural choice — the WIT schema in
  `crates/cairn-protocol/` ([[external-protocol]]) already describes
  the operations; the trusted boundary lets us drop the
  `meta.authenticate` step on the session-daemon side.
- Session-daemon lifecycle: when does the process exit? Same triggers
  as today (child exit + post-exit-normalisation drain, explicit kill,
  idle timeout) but enforced from within the session process.
- Discovery on frontend restart: enumerate sockets under
  `$XDG_RUNTIME_DIR/cairn/sessions/`, probe each for liveness, prune
  stale entries. zmx already does this (`util.zig:31, 67`).
- Cross-session operations (`list`) become fanout instead of an
  in-memory registry lookup.
- Multi-process tests are noticeably harder than single-process.
- Logs scatter; session-id propagation must be disciplined.

### Invariants to preserve in v0 to keep the migration mechanical

The migration is roughly 2-3 weeks of mechanical work **if** the
following are true. They are true today; the discipline is to keep
them true.

1. **The `Command` enum is the only API to a session worker.**
   Anything daemon-level that wants something from a session goes
   through `cmd_tx`. No reaching into worker state, no shared
   `Arc<SessionState>`, no shortcut channels that bypass the command
   queue. This is already the shape of
   `crates/cairn-pty/src/pty/ghostty/mod.rs` — keep it that way. The
   `Command` enum becomes the IPC schema directly.
2. **Sessions are opaque from the daemon's perspective.** The
   registry holds `(session_id, Arc<dyn PtySession>)` and nothing
   else. No cross-session state mutates session internals.
3. **Stable session ID from day 1.** UUIDv7 + optional name (already
   the plan). Multi-daemon's per-session socket files will be named
   by ID; any in-process reference should already be using ID, not a
   pointer or thread handle.
4. **`Subscription` stays small and serialisable.** Currently
   `{ snapshot: Bytes, stream: broadcast::Receiver<Bytes> }`
   (`pty/subscription.rs`). The snapshot serialises trivially; the
   stream becomes "frontend bridges PTY-output frames from the
   session-daemon socket to attached wRPC clients" — same shape,
   different transport.
5. **Daemon-level mutable state is not session-state.** Leader
   election (see [[client-attach-and-election]]) belongs in the
   frontend, which sees client identities. Auth tokens (see
   [[authentication]]) live in the frontend. The session worker
   should not learn anything about transports.

### What would kill the migration path

Two patterns to refuse in code review even when convenient:

- **Calling `terminal.borrow_mut()` from daemon-level code.**
  Snapshot generation, state inspection, anything that touches the
  emulator must happen on the worker thread. The
  `libghostty-vt` `!Send + !Sync` constraint already blocks
  cross-thread access at compile time; the analogous discipline for
  multi-daemon is "no out-of-worker access at all." A future "let the
  daemon peek at terminal state for `list`" feature becomes a
  `Command::SerializeSnapshot`, not a borrow.
- **Reading PTY output bytes from daemon-level code without going
  through `Subscribe`.** E.g., a "tail to file" feature implemented by
  tapping the worker. Make it a subscription with a daemon-side
  consumer instead.

The pragmatic outcome: single-daemon now, with the option to migrate
preserved by API discipline rather than by speculative abstractions.

## Daemon lifecycle summary

| Phase     | zmx                                      | cairn                                                  |
| --------- | ---------------------------------------- | ------------------------------------------------------ |
| Startup   | First `attach` / `run` forks            | `systemd --user` / `launchctl` / manual `cairn serve` |
| Discovery | `readdir(socket_dir)` + IPC probe       | `sessions.list-all` wRPC call over UDS or WT           |
| Process   | One per session                          | One per user, all sessions in-process                  |
| Shutdown  | Shell EOF, `Kill` IPC, signal           | Signal, `cairn shutdown`, never on last-session-exit   |
| Crash blast radius | One session                     | All sessions for that user                             |

## Open questions

- **Auto-spawn on attach for unknown names?** zmx auto-creates on
  attach-by-name. Cairn's UUID model can't auto-create on attach (an
  unknown UUID is just unknown), but should `cairn attach <name>`
  auto-create when `<name>` does not resolve? See
  [[client-attach-and-election]].
- **First-touch daemon spawn?** Should `cairn` CLI auto-spawn the
  daemon on the first command if no listener is bound, or require
  `systemd --user enable cairn` first?
- **Idle shutdown?** Exit when holding zero sessions for N minutes,
  or stay up forever? Affects [[configuration]] and the systemd unit
  type.
- **Local UDS vs loopback WT for CLI?** Loopback WebTransport works
  for the CLI in principle (`wtransport` has a Rust client), but UDS
  avoids the TLS handshake and gives `SO_PEERCRED` auth for free.
  Keep the UDS path unless it proves to be dead weight.
- **Cross-user attach?** Per-user daemons today; pairing-style
  cross-user attach is out of scope for v1 but worth flagging before
  the per-user assumption ossifies.
- **Restart surfacing?** Ensure [[observability]] exposes "daemon
  was restarted at T" so clients can interpret the session gap
  correctly.

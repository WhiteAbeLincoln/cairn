# PTY lifecycle

How a PTY-backed session is born, lives, and dies in cairn — with zmx
as the reference. Scope: the master/slave pair, the child process,
and the per-session loop. Higher-level concerns live elsewhere — see
[[daemon-process-model]], [[client-attach-and-election]],
[[internal-communication]], [[external-protocol]].

## Creation trigger

**zmx** creates a session lazily, from the *attach* path. `attach`
(`src/main.zig:1935`) calls `Daemon.ensureSession()`
(`src/main.zig:1941`), which probes the unix socket on disk. If no
socket exists (or the existing one is stale, `src/main.zig:746–769`),
the foreground process `fork()`s, the child `setsid()`s, redirects
stdio to `/dev/null`, runs `spawnPty()`, and enters `daemonLoop`
(`src/main.zig:777–862`). The original foreground process then
`connect()`s as client #1. The session daemon **is** the PTY owner —
one OS process per session, no separate registry.

**cairn** is the inverse. `GhosttyPty::spawn(opts)`
(`crates/cairn-pty/src/pty/ghostty/mod.rs:48–54`) is an *eager*,
caller-driven constructor that returns a `Send + Sync` handle. The
PTY lifecycle is **not** coupled to attach: the cairn daemon
([[daemon-process-model]]) calls `GhosttyPty::spawn` when it decides
a session should exist, and clients later attach via the wRPC
`sessions.attach` operation ([[client-attach-and-election]]). zmx ties "session exists" to "PTY
master exists" at the process level; cairn keeps the PTY as an
object the host process holds inside an `Arc<dyn PtySession>`
(`crates/cairn-pty/src/pty/session.rs`), making session registry,
persistence policy, and PTY ownership orthogonal.

## PTY allocation

**zmx** uses BSD `forkpty(3)` exclusively (`src/cross.zig:27–32`
declares the macOS/Linux/FreeBSD shims). One call allocates the
master, opens a slave, `fork()`s, makes the child the controlling
terminal owner, and `dup2`s the slave to fds 0/1/2 — returning
`master_fd` and child pid to the parent (`src/main.zig:710`). The
parent then switches the master to `O_NONBLOCK`
(`src/main.zig:731–732`) so its `poll()` loop won't block on reads.

**cairn** splits allocation and spawn. `pty_process::Pty::new()`
opens the master via `posix_openpt`/`grantpt`/`unlockpt` and wraps
the fd in `tokio::io::unix::AsyncFd` (`worker.rs:71–79`); `pty.pts()`
returns a `Pts` slave handle (`worker.rs:84–92`); the child is then
spawned via `pty_process::Command::spawn(&pts)` (`worker.rs:104–140`),
which does `fork`/`setsid`/`TIOCSCTTY`/`dup2(slave, 0/1/2)`/`execvp`
in the child.

A macOS wart documented at `worker.rs:81–83`: the very first
`TIOCSWINSZ` on the master fails with `ENOTTY` until the slave has
been opened at least once, so cairn calls `pts()` before `resize()`.
zmx dodges the issue by passing the initial winsize into `forkpty`
itself (`src/main.zig:702–710`).

Both are Unix-only; `cfg(unix)` gates appear in cairn (`worker.rs:504`).

## Child spawn

**zmx**'s `execChild()` (`src/main.zig:654–697`) runs inside the
forked child: restores `SIGPIPE` to `SIG_DFL` (parent ignores SIGPIPE
for socket safety, child shell pipelines need default semantics),
sets `ZMX_SESSION` in the env (`src/main.zig:667–673`), then
`execve`s the user's command or a login shell (`argv[0] = "-bash"`)
detected via `util.detectShell()` (`src/main.zig:685–696`).

**cairn** never calls `exec` directly — it constructs a
`pty_process::Command` by copying program/args/env/cwd off the
caller-supplied `tokio::process::Command` via `as_std()`
(`worker.rs:115–129`). One documented limitation: `std` exposes env
overrides through `get_envs()` but does **not** expose whether
`env_clear()` was called, so any `env_clear` on the input command
is silently lost (`worker.rs:108–114`).

A critical detail at `worker.rs:142–147`: after `spawn(&pts)` the
worker **drops** its copy of the slave handle. The child holds its
own `dup`'d fd, so the TTY stays alive — but the master only sees
EOF when *all* slave fds close. Keeping the parent's slave around
would deadlock post-exit cleanup. zmx avoids this by construction:
`forkpty(3)` closes the slave in the parent automatically.

## The session loop

**zmx**'s `daemonLoop` (`src/main.zig:2460–2718`) is a single `poll(2)`
loop multiplexing: the server accept socket, the PTY master fd
(POLL.IN, plus POLL.OUT when input is buffered), a self-pipe woken
by `SIGTERM` (`src/main.zig:2497, 2720`), and one fd per attached
client. PTY output is fed into `ghostty_vt.Terminal.vtStream()` for
state tracking (`src/main.zig:2475, 2563`) and broadcast to every
client (`src/main.zig:2599–2608`). Leader input is queued onto
`pty_write_buf` (`src/main.zig:879–895`) and drained on POLLOUT
(`src/main.zig:2613–2625`); see [[backpressure]].

**cairn**'s equivalent is `run_session`
(`worker.rs:200–427`), structurally identical at a higher abstraction:
a single `tokio::select!` across `pty.read(...)`,
`cmd_rx.recv_async()`, and `child.wait()`. The select fires on a
dedicated worker thread's current-thread tokio runtime + `LocalSet`
(`worker.rs:62–67`), needed because `libghostty_vt::Terminal` is
`!Send`. Each chunk read off the master is `vt_write`d into the
emulator and forwarded on the broadcast (`worker.rs:283–293`). The
VT can synthesize responses to queries (DA1/DSR/...) via the
`PtyWriteFn` callback; cairn queues those into a `pending_writes`
`VecDeque` and drains them back to the master after every read
(`worker.rs:206–232, 292, 435–446`) — see
[[query-response-delegation]] and [[terminal-state-and-replay]].

## Child exit detection

This is where the two designs make the most divergent choices.

**zmx** detects exit *only* via EOF on the master: `n == 0` from
`posix.read(pty_fd, ...)` triggers `break :daemon_loop`
(`src/main.zig:2548–2560`). No `waitpid` runs inside the loop — the
final `posix.waitpid(self.pid, 0)` in the `defer` at
`src/main.zig:853` reaps the zombie *after* loop exit. SIGCHLD is
not installed. This works because the daemon's only job is the PTY;
when the child closes its slave, reads return 0, the loop ends.

**cairn** races EOF against `child.wait()` explicitly
(`worker.rs:265–415`). Either branch can fire first:

- If `pty.read` returns `Ok(0)` (`worker.rs:267–282`), the worker
  sets `pty_closed = true` to disable the read branch, drops the
  broadcast sender, and awaits `child.wait()` inline to publish
  status to the watch channel.
- If `child.wait()` resolves first (`worker.rs:406–415`, guarded
  by `if !exit_published`), status is published but the loop
  **keeps running**. The PTY may still hold buffered output, and
  the comment at `worker.rs:398–405` is explicit: drain `pty.read`
  until EOF before tearing down.

`child.wait()` reaps the zombie as a side effect (tokio's `wait`
calls `waitpid` internally). zmx defers the reap. cairn's
`synthetic_exit_status(1)` (`worker.rs:505–508`) covers the rare
case where `wait()` itself errors — zmx has no such fallback because
it never observes the wait result during normal operation.

## Final-output drain and persistence

After the child exits, output may still be in the kernel's PTY
buffer.

**zmx** doesn't actively drain — by the time the loop sees `n == 0`,
all data has already been delivered (read came up empty). The daemon
process then runs the `defer` block at `src/main.zig:849–859`:
`handleKill` sends SIGHUP/SIGKILL to the process group (harmless
even though the child is already dead — it cleans up descendants),
`deinit` frees client lists, `close(pty_fd)` closes the master,
`waitpid` reaps the child, the listener socket is closed, and the
socket file is `unlink`ed. **The session disappears immediately on
child exit** — no scrollback preservation; the next `zmx attach`
gets a fresh PTY.

**cairn** does the opposite. After `child.wait` resolves, the loop
keeps spinning so further `pty.read` calls flush buffered bytes
(`worker.rs:398–406`). Once read returns 0, `bcast_tx` is dropped
and live subscribers observe `RecvError::Closed` (`worker.rs:275–276`).
The worker thread itself **does not exit** — it enters "post-exit
normalisation" mode (`worker.rs:314–333`): `Resize`/`Size`/`Write`
reply `PtyError::Closed`, while `Subscribe` still works, returning
a snapshot of the final terminal state plus an immediately-closing
stream (`worker.rs:355–371`). Late attaches can still see "what
happened". The worker only truly exits when `cmd_rx` disconnects —
i.e. every `GhosttyPty` clone has been dropped (`worker.rs:309–313`).

So **cairn's session persists indefinitely after child exit**, for
as long as the surrounding daemon holds a handle. No built-in idle
timeout, no TTL — the daemon's session registry is the authoritative
lifetime owner. See [[error-recovery]] for reconnect interactions.

## Destruction triggers

Triggers that begin teardown:

| Trigger | zmx | cairn |
|---|---|---|
| Child exits naturally | `read == 0` → loop exit (`main.zig:2557–2560`) | `child.wait` or EOF → exit published, loop continues serving Subscribe |
| Explicit `kill` command | `.Kill` IPC → `break :daemon_loop` (`main.zig:2677–2679`) then `handleKill` SIGHUP→500 ms→SIGKILL (`main.zig:1046–1061`) | `GhosttyPty::kill()` enqueues `Command::Shutdown` → `child.start_kill()` (SIGKILL by default) → await wait → `break` (`worker.rs:335–354`) |
| Handle dropped | n/a (no Rust handle) | `Drop for GhosttyPty` calls `kill()` (`mod.rs:124–131`); test at `tests/pty_lifecycle.rs:42–77` |
| Daemon SIGTERM | self-pipe wake → `break :daemon_loop` (`main.zig:2513–2520`) | upstream daemon drops its `Arc<dyn PtySession>`s; cascades to `Drop` |
| Last client disconnect | configurable: `closeClient(... shutdown_on_last=true)` is supported but the attach path passes `false` (`main.zig:622–641`) | unrelated — see [[client-attach-and-election]] |

zmx's SIGHUP→500 ms→SIGKILL escalation (`src/main.zig:1049–1051`)
is softer than cairn's `child.start_kill` (Unix: immediate SIGKILL,
no grace). zmx's rationale: shells frequently ignore SIGTERM.

## Cleanup ordering

**zmx** (`src/main.zig:849–859`, `defer`-driven):

1. `handleKill` — `running = false`, signal child
2. `deinit` — free client list, write buf, socket path
3. `close(pty_fd)` (master)
4. `waitpid(pid, 0)` — reap zombie
5. `close(server_sock_fd)`
6. `dir.deleteFile(session_name)` — unlink socket file
7. terminal & vt_stream drop via the inner `defer`
   (`src/main.zig:2474–2476`)

**cairn** (`worker.rs:419–426` + Drop chain):

1. Loop exits (via `Shutdown`, `cmd_rx` disconnect, or post-EOF break)
2. `bcast_tx` → `None` → subscribers observe `Closed`
3. `SessionState` drops → `cmd_rx`, `pty` (master closed via
   `AsyncFd` drop), `child` (no zombie — `wait` was awaited),
   `exit_tx` (sender drop signals the watch channel)
4. `LocalSet`/runtime drop on thread join
5. `Terminal` drop releases the libghostty allocator

Both orderings line up (broadcast → master fd → child → emulator),
but cairn rides RAII while zmx uses hand-written `defer`s. cairn's
`Drop for GhosttyPty` (`mod.rs:124–131`) is the safety net for
callers who forget to `kill()`.

## Open questions

- **Idle-session reaping.** cairn's "persists after child exit"
  policy has no built-in TTL. Should the surrounding session manager
  (not the PTY layer) drop sessions whose child exited > N minutes
  ago with no live subscribers? See [[daemon-process-model]],
  [[configuration]].
- **Grace period on kill.** Should cairn match zmx's
  SIGHUP→500 ms→SIGKILL escalation (`main.zig:1053–1060`)?
  `child.start_kill()` is currently unconditional SIGKILL.
- **`env_clear` lossiness.** `worker.rs:108–114`:
  `tokio::process::Command::env_clear()` is invisible through
  `as_std().get_envs()`. Should `SpawnOptions` expose env explicitly?
  See [[configuration]].
- **Synthetic exit status.** When `child.wait()` itself errors
  (`worker.rs:409–412`), cairn fabricates exit code 1. Should this
  be distinguishable from a real exit 1 (e.g. `PtyError::WaitFailed`)?
  See [[error-recovery]].
- **Multi-platform.** Both implementations are Unix-only; cairn's
  `cfg(unix)` gates (`worker.rs:504`) hint at a future ConPTY backend.
- **Handle sharing.** `GhosttyPty` is not `Clone`, so drop-kills-
  child is unambiguous. If the daemon shares handles via
  `Arc<GhosttyPty>`, cleanup is gated on the last `Arc` drop —
  confirm that matches the session manager. See
  [[daemon-process-model]].
- **Post-exit Subscribe stream.** `worker.rs:362–368` returns a
  pre-closed broadcast for post-exit subscribers. Should it also
  emit a synthetic exit marker for ghostty-web/xterm.js to render
  "process exited" UI? See [[web-vs-cli-clients]].

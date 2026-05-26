# Cairn daemon binary (UDS) design

## Status

Design. Defines the `cairn-daemon` binary that serves the `cairn:daemon@0.1.0`
wRPC surface over a Unix Domain Socket, holds all of a user's PTY sessions in an
in-process registry, and drives them through the existing `cairn-pty`
`PtySession` workers. This is item 4 ("Daemon binary") of
`docs/architecture/pty-session/README.md`.

Builds on:
- `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md` (the wire protocol / schema).
- `docs/architecture/pty-session/*` (the subsystem architecture; `daemon-process-model.md`,
  `internal-communication.md`, `client-attach-and-election.md`, `error-recovery.md` are the load-bearing ones here).

## Scope (v0)

- **Transport: UDS only.** The Unix-socket path is proven today
  (`crates/cairn-protocol/tests/round_trip.rs` already stands up a real
  `wrpc_transport::Server` over a socket and drives it from
  `wrpc_transport::unix::Client`). WebTransport — and the TLS / bearer-token /
  first-message-auth machinery it needs — is a separate follow-up spec. Listener
  and auth seams are structured so WT drops in as a second listener.
- **Full operation surface.** All 14 operations (`meta`: version/whoami/authenticate;
  `sessions`: list-all/inspect/create/rename/restart/kill/kick/wait/logs/attach/send)
  are designed and implemented over UDS.
- **Startup: foreground, service-managed.** A `cairn-daemon` binary that runs in
  the foreground, logs to stderr, and handles SIGTERM/SIGINT for graceful
  shutdown. `systemd --user` / `launchd` supervises it. No auto-spawn-on-first-CLI
  in v0.
- **Faithful signal delivery + graceful escalation.** `kill` delivers the
  requested signal; an optional grace escalates to SIGKILL — owned by the daemon,
  not the client.
- **Deferred (see [Deferred / future work](#deferred--future-work)):** idle-timeout
  enforcement, max-session cap, time-windowed `logs`, WebTransport, metrics/debug
  endpoints, a TOML config loader.

## Architecture (Approach A: thin handler + rich `SessionEntry` registry)

The runtime topology is fixed by `internal-communication.md`: a multi-thread
tokio runtime owns the UDS listener and runs wRPC `Handler` tasks; each session
stays behind its existing dedicated-thread `GhosttyPty` worker, reached only via
the `flume` `Command` channel. The daemon never touches the emulator or PTY
directly — the `Command` enum remains the only API to a worker, preserving the
multi-daemon migration discipline in `daemon-process-model.md`.

Handlers are thin: resolve a session, call a trait method, map the result. Each
streaming operation gets its own module; the bidi `attach` bridge is the only
substantial piece of logic.

Rejected: a per-session daemon-side supervisor/actor task (that boundary is
exactly what the multi-daemon migration adds later — YAGNI now) and flat parallel
maps (`handle` and `metadata` can desync; `restart`/`rename` get awkward).

### Crate layout

New crate `crates/cairn-daemon`, **lib + bin** (the lib makes `Daemon` and
`serve()` importable so integration tests can drive the real handlers
in-process, exactly as `round_trip.rs` does):

```
crates/cairn-daemon/
  Cargo.toml          # bin "cairn-daemon" + lib
                      # deps: cairn-pty, cairn-protocol, wrpc-transport (net),
                      #   tokio (rt-multi-thread/net/signal/macros), futures,
                      #   async-stream, anyhow, clap, tracing, tracing-subscriber,
                      #   uuid (v7), nix (signals + peer creds), bytes,
                      #   tokio-util (CancellationToken)
  src/
    main.rs           # parse args -> init tracing -> build Daemon -> serve(); SIGTERM/SIGINT
    lib.rs            # pub re-exports for the test harness
    config.rs         # DaemonConfig (flags + CAIRN_* env + XDG defaults)
    daemon.rs         # Daemon (Clone): Arc<SessionRegistry> + Arc<DaemonConfig>.
                      #   Carries the two Handler impls; each method delegates to handlers::*
    registry.rs       # SessionRegistry, SessionEntry, resolution, ClientId minting
    serve.rs          # PeerCredListener + ConnCtx, socket hygiene, accept loop,
                      #   invocation pump, graceful drain
    spawn.rs          # SessionSpec -> cairn_pty::SpawnOptions
    signal.rs         # protocol signal -> libc int (name resolution, Linux/BSD divergence)
    error.rs          # PtyError + daemon errors -> types::Error { code, message }
    handlers/
      mod.rs
      meta.rs         # version, whoami, authenticate
      sessions.rs     # unary: list_all, inspect, create, rename, restart, kill, kick
      attach.rs       # bidi attach bridge
      logs.rs         # server-stream
      send.rs         # client-stream
      wait.rs         # future
```

## Session registry & data model

```rust
pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, Arc<SessionEntry>>>, // key = session id (UUIDv7)
    next_client_id: AtomicU64,
}

pub struct SessionEntry {
    pub id: String,                 // UUIDv7 (server-assigned; chronological sort for free)
    pub created_at_unix_ms: u64,
    pub spec: SessionSpec,          // original create spec — drives restart + inspect.spec
    name: Mutex<Option<String>>,    // rename mutates; uniqueness enforced by the registry
    running: RwLock<Running>,       // handle + pid; both swapped atomically on restart
    attached: Mutex<HashMap<ClientId, AttachHandle>>, // powers list/inspect + kick
}

struct Running { handle: Arc<dyn PtySession>, pid: Option<u32> }
struct AttachHandle { kick: oneshot::Sender<()> } // fired by `kick` to evict one bridge
```

There is **no `exit` field and no `size` cache** — both are read straight from
the worker (see [cairn-pty changes](#cairn-pty-changes)). Exit state lives in the
worker's `watch` (`try_exit_status()` / `wait()`); the protocol's
`exit-status.unix-ms` comes from a timestamp the worker stamps at exit. Size
comes from `size()`, which the worker now answers even post-exit. Keeping a
single source of truth per fact avoids sync drift.

### Concurrency discipline

Three independent locks rather than one combined mutex, because the fields are
mutated by different operations at very different rates and **no operation needs
an atomic multi-field update**. A combined lock would couple unrelated hot paths
(every keystroke clones the handle out of `running`; that must not serialize
against `attach`/`detach` mutating the client set, or against `rename`).

- `running` is read-mostly (cloned on every `subscribe`/`write`/`resize`/`signal`,
  written only by the rare `restart`) → `RwLock`.
- `name`, `attached` → `Mutex` (low contention; string swap / map mutation only).

**Invariant that makes the multi-lock entry safe:** *never hold an entry lock
across an `.await`, and never hold two entry locks at once.* Every operation does
lock → clone-or-mutate the in-memory value → unlock → then `.await` the async
work on the clone. `list`/`inspect` read each lock independently and tolerate a
slightly torn snapshot (it is a status view; nothing depends on a consistent
cross-field read). With that rule there is no lock-ordering or deadlock surface.

The only would-be cross-field coupling is removed by construction: **`attached`
is maintained exclusively by the attach bridges** (insert-on-start,
remove-on-end via an RAII guard). `restart` only swaps `running`; `kick` only
fires the `oneshot` and lets the bridge exit and self-remove. So no operation
ever needs two of these locks together, and `restart` cannot race a concurrent
attach over the client set.

### Identity & resolution

- **ClientId:** `ClientId::from_u64(next_client_id.fetch_add(1, Relaxed))`. The
  daemon owns identity; the worker only does equality on it for leader election.
- **Resolution** (`resolve(key) -> Option<Arc<SessionEntry>>`): exact live-name
  match first, then exact id — the contract in `cli.rs:478-485`. Single-target
  ops pass one already-resolved name-or-id. `--latest`/`--all`/globs never reach
  the daemon — see [Bulk / selector resolution](#bulk--selector-resolution).

### Lifecycle semantics

- **Name uniqueness** is enforced only across *live* sessions; `create`/`rename`
  reject a name currently in use.
- **Exited sessions linger** in the registry (visible in `list` with `exit` set,
  still serving `subscribe`/`logs` against the final snapshot — the
  post-exit-normalization divergence in `pty-lifecycle.md`). With idle-reaping
  deferred, they persist until daemon restart. Known v0 behavior, tied to the
  deferred idle-timeout work.
- **`restart`** rebuilds `SpawnOptions` from `spec`, spawns a fresh `GhosttyPty`,
  swaps `Running` under the same id/name/created_at, and drops the old handle
  (its `Drop` kills the old child; `force` gates restarting a still-running
  session). Attached clients' broadcast streams close, so they reconnect. Exit
  state resets for free — the new handle has a fresh `exit_rx`.

### Bulk / selector resolution

`--latest`, `--all`, and name globs (`SessionTargets` in `cli.rs`) are resolved
**client-side**: the client calls `list-all`, filters/selects, then issues one
independent per-target unary call. This deliberately stays out of the WIT, and
it is *not* the same decision as `kill`'s grace. `kill` escalation is a durable
multi-step policy that must survive client death, so it lives server-side; bulk
resolution is stateless fan-out — globs and `--latest` are CLI input sugar (the
web UI selects from its rendered list and never parses globs), and a client that
dies mid-fan-out leaves no half-applied state.

The list→act gap is a TOCTOU window, but benign for these ops: a session created
after "all" was named simply was not part of "all", and a target that vanished
in the gap is already in the desired state. Two requirements keep batch behavior
correct and consistent across clients:

1. **Per-target calls are independent and each returns its own result.** A batch
   is N unary RPCs, not one aggregate RPC; the daemon already returns a per-call
   `result<_, error>`.
2. **The client aggregates per-target outcomes — a batch never collapses to a
   single error.** Each target's result is reported individually; the command
   exits non-zero only if one or more targets *genuinely* failed. A per-target
   `not_found` in a `--all`/glob batch (a session that vanished in the gap) is
   treated as benign, not a failure, and never aborts the rest of the batch. A
   single explicitly-named target still surfaces `not_found` as an error — that
   is a typo, not a race. (In v0 exited sessions linger in the registry, so a
   batch target going `not_found` is rare until reaping lands.)

Escape hatch if this ever needs to move server-side (a third client that needs
glob matching, or atomic bulk semantics): add a `target` selector variant to the
unary ops and have bulk ops return `list<result>`. Out of scope for v0 — the
benign-drift / CLI-sugar analysis says the WIT expansion is not warranted yet.

## cairn-pty changes

These promote everything the daemon needs onto the `PtySession` trait so the
registry stays on backend-agnostic `Arc<dyn PtySession>`:

| Addition | Why | Mechanism |
|---|---|---|
| `async fn signal(&self, sig: i32) -> Result<(), PtyError>` | `kill` + shutdown drain | new `Command::Signal`; worker sends to the child's process group (`nix::sys::signal::killpg` / negative pid) |
| `async fn inject(&self, data: Bytes) -> Result<(), PtyError>` | `send` | new `Command::Inject`; writes to the PTY with **no** client identity and **no** leader promotion |
| `async fn wait(&self) -> ExitStatus` | `sessions.wait`, escalation timeout | promote the existing `GhosttyPty::wait()` onto the trait |
| `fn try_exit_status(&self) -> Option<ExitStatus>` | `list`/`inspect` exit field | `*self.exit_rx.borrow()` — sync, non-blocking, no command |
| post-exit `Command::Size` returns `Ok(current_size)` | `list`/`inspect` size of exited sessions | relax the post-exit normalization arm — it is an in-memory read, not a kernel call; `Resize`/`Write` keep returning `Closed`. Trait doc gains: "post-exit, returns the last-applied size." |
| `ExitStatus` gains an exit-timestamp field | protocol `exit-status.unix-ms` | stamped where exit is detected (`child.wait` / EOF / synthetic arms) |

`signal`/`inject` are leader-irrelevant. `inject` exists specifically so `send`
does **not** steal the interactive leader's seat: `write(client_id, …)` promotes
on user-input (most-recent-input-wins), so a background `cairn send` routed
through `write` would yank leadership from an attached human and trigger a
spurious resize. `inject` is identity-less blind injection — the correct
semantics for "send characters without attaching."

## wRPC server wiring

### Peer credentials via a custom `Accept`

`wrpc_transport::Server<C, I, O>` is generic over the context type `C`;
`Server::accept` takes `impl Accept<Context = C, …>`, and that `C` is exactly the
`Ctx` the generated `Handler<Ctx>` receives. `Accept` is a public trait, so the
daemon provides its own listener that reads `peer_cred()` off the `UnixStream`
**before** splitting it:

```rust
#[derive(Clone, Copy, Debug)]
pub struct ConnCtx { pub peer: Option<tokio::net::unix::UCred> } // uid/gid/pid

struct PeerCredListener(tokio::net::UnixListener);

impl Accept for &PeerCredListener {
    type Context = ConnCtx;
    type Outgoing = tokio::net::unix::OwnedWriteHalf;
    type Incoming = tokio::net::unix::OwnedReadHalf;
    async fn accept(&self) -> io::Result<(ConnCtx, Self::Outgoing, Self::Incoming)> {
        let (stream, _addr) = self.0.accept().await?;
        let peer = stream.peer_cred().ok();   // one getsockopt; Linux + macOS
        let (rx, tx) = stream.into_split();
        Ok((ConnCtx { peer }, tx, rx))
    }
}
```

The `Server` becomes `Server<ConnCtx, OwnedReadHalf, OwnedWriteHalf>`; `Daemon`
implements `Handler<ConnCtx>`. No wRPC fork, no custom accept loop — still
`srv.accept(&peer_listener)`. UDS opens a fresh connection per invocation, so
every call independently carries its peer cred. This is also the context-shape
the future WT transport fills with the authenticated token identity.

### `Daemon` + `serve()`

`Daemon { registry: Arc<SessionRegistry>, cfg: Arc<DaemonConfig> }`, `#[derive(Clone)]`
(two `Arc`s). `serve()` mirrors the `round_trip.rs` shape:

```rust
pub async fn serve(daemon: Daemon, cfg: Arc<DaemonConfig>, shutdown: CancellationToken) -> Result<()> {
    let listener = bind_with_cleanup(&cfg)?;                       // socket hygiene below
    let srv = Arc::new(wrpc_transport::Server::default());
    let accept = tokio::spawn(accept_loop(srv.clone(), listener, shutdown.clone()));
    let invocations = cairn_protocol::serve(srv.as_ref(), daemon.clone()).await?;
    let pump = tokio::spawn(invocation_pump(invocations, shutdown.clone()));
    shutdown.cancelled().await;
    drain_sessions(&daemon, cfg.shutdown_grace).await;            // graceful drain below
    // abort accept/pump, unlink socket
}
```

`invocation_pump` is the `select_all` + `tokio::spawn(fut)` per invocation from
`round_trip.rs:78-90` — one task per in-flight call, so a long-lived `attach`
never blocks other operations. Each spawned call is wrapped in a `tracing` span
(see [Observability](#observability)).

### Two-layer return convention

Generated handlers return `anyhow::Result<Result<T, types::Error>>`. The **inner**
`Err(types::Error)` is a normal domain outcome the CLI renders; the **outer**
`anyhow::Err` is reserved for genuine transport/internal breakage and surfaces as
a wRPC-level failure. Almost everything is `Ok(Ok(..))` / `Ok(Err(types::Error{..}))`.

### Error mapping (`error.rs`)

`PtyError` and daemon errors map to `types::Error { code, message }` with
machine-stable `code` strings the CLI can branch on:

| Source | `code` |
|---|---|
| resolve miss | `session.not_found` |
| name already live | `session.name_in_use` |
| `restart` while running, no `--force` | `session.running` |
| `GhosttyPty::spawn` failure | `session.spawn_failed` |
| bad signal name/number | `signal.invalid` |
| `PtyError::Closed` on write/resize | `session.closed` |
| `PtyError::NotLeader` | `resize.not_leader` |
| `PtyError::Io` / `Backend` | `pty.io` / `pty.backend` |

## Operation semantics

### `meta`

- `version` → `{ daemon: "cairn-daemon/<CARGO_PKG_VERSION>", protocol: "cairn:daemon@0.1.0" }`.
- `whoami` → the peer uid from `ConnCtx`, resolved to a username when possible
  (`nix::unistd::User::from_uid`), else the numeric uid.
- `authenticate(token)` → `Ok(Ok(()))` no-op on UDS; the kernel already
  authenticated via filesystem DAC. "Must authenticate before other calls"
  gating is a WebTransport-only concern.

### Unary `sessions`

All thin: `resolve` → act → map. Notable ones:

- `create(spec)` → validate name free → `spawn::options_from(spec, &cfg)` (empty
  `spec.command` → `cfg.default_shell`) → `GhosttyPty::spawn` → insert →
  `SessionInfo`.
- `list_all` → enumerate; build each `SessionInfo` from cached fields +
  `handle.size()` + `handle.try_exit_status()`. The `size()` calls fan out
  concurrently (`futures::future::join_all`), so list latency is ~one flume
  round-trip regardless of session count — each session is its own worker thread.
- `restart(id, force)` → if `try_exit_status().is_none() && !force` →
  `Err(session.running)`; else spawn fresh from `spec`, swap `Running`, `Ok`.
- `kill(id, sig, grace-ms)` → resolve → `signal::to_libc(sig)` → capture the
  handle `Arc` (so a concurrent `restart` cannot redirect it) → `handle.signal(n)`.
  If `grace-ms` is `Some(g)`, spawn a detached daemon task:
  `sleep(g); if handle.try_exit_status().is_none() { handle.signal(SIGKILL) }`.
  Returns `Ok` once the first signal is dispatched. Escalation lives **in the
  daemon**, durable against client death/network blips; re-issuing `kill` is
  idempotent. (See [Schema change](#schema-change-kill-gains-grace-ms).)
- `kick(id, client)` → fire one (or all) `AttachHandle.kick` oneshots; the bridge
  exits and self-removes from `attached`.
- `rename(id, new_name)` → uniqueness check → swap `name`.
- `inspect(id)` → one `SessionInfo` or `Err(session.not_found)`.

### `attach` (bidi bridge — `handlers/attach.rs`)

Generated signature delivers `events: Stream<Item = Vec<ClientEvent>>` and
returns `Stream<Item = Vec<ServerEvent>>` (wRPC batches stream elements into
`Vec`s).

```rust
async fn attach(daemon, _ctx, id, init, mut events) -> Result<ServerEventStream> {
    let Some(entry) = daemon.registry.resolve(&id) else {
        return Ok(once(vec![ServerEvent::Error(not_found())]));   // in-band, clean
    };
    let client_id = daemon.registry.mint_client_id();
    let handle = entry.handle();                                  // clone Arc out of RwLock
    if !init.no_stdin {                                           // leader-wins: first attacher
        let _ = handle.resize(client_id, init_size).await;        //   claims the empty seat + sets size;
    }                                                            //   followers get NotLeader (ignored)
    let sub = handle.subscribe(client_id).await?;                 // atomic snapshot+stream; no promote
    let guard = entry.track_attach(client_id);                   // RAII: removes from `attached` on drop
    let mut kick = guard.kick_rx();

    Ok(async_stream::stream! {
        let _guard = guard;                                       // held for the stream's lifetime
        yield vec![ServerEvent::Snapshot(sub.snapshot)];          // cairn keeps the snapshot on 1st attach
        let mut rx = sub.stream;
        loop {
          tokio::select! {
            ev = events.next() => match ev {
                Some(batch) => for e in batch { match e {
                    ClientEvent::Input(b)  if !init.no_stdin =>
                        { if handle.write(client_id, Bytes::from(b)).await.is_err() { return } }
                    ClientEvent::Resize((c, r)) =>
                        { let _ = handle.resize(client_id, TermSize { cols: c, rows: r }).await; } // NotLeader ignored
                    ClientEvent::Detach => return,
                    _ => {}
                }},
                None => return,                                   // client closed inbound
            },
            out = rx.recv() => match out {
                Ok(bytes)             => yield vec![ServerEvent::Output(bytes)],
                Err(Lagged(_))        => return,                  // lag-kick: close -> client reattaches fresh
                Err(Closed)           => { yield vec![ServerEvent::Exited(exit_status(&handle))]; return } // child exited
            },
            _ = &mut kick => return,                              // `kick` op evicted us
          }
        }
    })
    // on drop: the Subscription guard sends Command::Detach (worker clears leader
    // if we held it) and `_guard` removes us from `attached`. Both automatic.
}
```

Properties:
- **First attach keeps the snapshot** (cairn's divergence from zmx — primary use
  case is long-running headless processes).
- **Leader-wins:** the first non-`no_stdin` attacher's init-resize claims the
  empty seat and sets PTY size; followers' resizes return `NotLeader` and are
  ignored (they letterbox client-side). `no_stdin` (read-only) attaches never
  claim leadership.
- **Leader cleanup is automatic:** dropping the `Subscription` fires the existing
  `Command::Detach`, which clears the seat if this client was leader. The bridge
  does nothing special.
- **Late attach after exit works for free:** `subscribe()` returns a pre-closed
  stream, so the loop emits snapshot → `Exited` → ends.
- **Lag → kick → reconnect:** `RecvError::Lagged` closes the stream; the client
  reattaches and resnapshots (`backpressure.md`). A lag-kick is **not** a session
  failure.
- Input writes are awaited inline in the `select!` loop (the shape
  `backpressure.md` recommends). If pathological input backpressure ever stalls
  output, the escape hatch is splitting into two tasks; v0 keeps one loop.

### `logs` (`handlers/logs.rs`)

An output-only sibling of `attach` over the same `subscribe()`: emit the snapshot
bytes, then if `follow` forward the broadcast until close; without `follow`,
close after the snapshot. Item type is raw `Vec<Bytes>` (no input, no leader, no
snapshot/output tagging). `log-window` is best-effort against the VT snapshot:
`tail(n)` = last *n* lines of the snapshot text, `all` = the whole snapshot.
There is no `since` variant (removed from schema + CLI; cairn keeps no
timestamped transcript — see [Deferred](#deferred--future-work)).

### `send` (`handlers/send.rs`)

Client-streaming `Stream<Vec<Bytes>>` → `handle.inject(chunk)` per element.
Resolve miss → `Ok(Err(session.not_found))`. Empty stream → `Ok(Ok(()))`. Uses
`inject` (non-promoting) so it never steals the interactive leader's seat.

### `wait` (`handlers/wait.rs`)

`Box::pin(async move { map_exit(handle.wait().await) })`; wRPC resolves the single
value. Resolve miss → outer `anyhow::Err` (the future has no in-band error
channel). `cairn_pty::ExitStatus` → `types::ExitStatus { code, signal, unix_ms }`.

### Per-shape error signaling

`attach` → in-band `server-event::error` then close. `send` → inner
`Ok(Err(types::Error))`. `logs`/`wait` have no in-band error channel (raw bytes /
bare future) → resolve miss returns the outer `anyhow::Err`. Unary ops → inner
`Err(types::Error)`.

### Schema change: `kill` gains `grace-ms`

To make escalation daemon-owned rather than a fragile client RPC sequence:

```wit
kill: func(id: session-id, sig: signal, grace-ms: option<u32>) -> result<_, error>;
```

`none` = deliver `sig` only; `Some(g)` = deliver `sig`, then SIGKILL after `g`
milliseconds if still alive. The CLI maps `--timeout d` → `grace = d`, default /
`--no-wait` → `none`, and uses `sessions.wait` to observe exit. The web UI gets
the same behavior by calling the same op. The daemon-shutdown drain reuses the
same primitive, so escalation has one implementation. This is a v0-acceptable
wire edit (nothing shipped; wRPC dispatch is by name); the `round_trip.rs` `kill`
stub signature updates with it.

## Config, startup, shutdown

### `DaemonConfig` (flags + `CAIRN_*` env + XDG defaults; precedence CLI > env > default)

| Field | Flag | Env | Default |
|---|---|---|---|
| socket path | `--socket` | `CAIRN_SOCKET` | `$XDG_RUNTIME_DIR/cairn/cairn.sock` (Linux) / `$TMPDIR/cairn/cairn.sock` (macOS) |
| dir mode | `--dir-mode` | `CAIRN_DIR_MODE` | `0o700` |
| socket mode | `--socket-mode` | `CAIRN_SOCKET_MODE` | `0o600` |
| shutdown grace | `--shutdown-grace` | `CAIRN_SHUTDOWN_GRACE` | `5s` |
| default shell | `--default-shell` | `CAIRN_DEFAULT_SHELL` / `$SHELL` | `/bin/sh` |
| log filter | `--log` | `CAIRN_LOG` / `RUST_LOG` | `info,cairn_daemon=info,cairn_pty=info` |

`dir_mode`/`socket_mode` exist for the system-service / shared-unix-group
deployment (cf. zmx's `ZMX_DIR_MODE`); e.g.
`--socket /srv/cairn/cairn.sock --dir-mode 0750 --socket-mode 0660`. No TOML
loader in v0 (`configuration.md`'s layered model is deferred).

The macOS default uses a `cairn/` subdirectory (matching Linux) so `dir_mode`
governs a daemon-owned parent on both platforms, never the OS-managed
`$TMPDIR`/`$XDG_RUNTIME_DIR` grandparent.

### Socket hygiene (`bind_with_cleanup`)

1. `mkdir` the socket's parent dir. **`chmod` it to `dir_mode` only if this call
   created it** — a pre-existing dir (`$TMPDIR`, `/var/run`, an operator-prepared
   shared dir) keeps its mode.
2. If the socket path exists, probe-connect: a *live* answer means another daemon
   owns it → refuse to start with a clear error; connection-refused → unlink the
   stale file and proceed (zmx pattern).
3. Bind the `UnixListener`; `chmod` the socket file to `socket_mode` (we always
   create it, so we always own its mode).
4. Unlink the socket on shutdown.

### Graceful shutdown

`main.rs` installs a `tokio::signal` SIGTERM/SIGINT handler that trips a
`CancellationToken`. `serve()` awaits it, then `drain_sessions`: `signal(SIGTERM)`
every session → `join_all` their `wait()` under `shutdown_grace` → drop all
entries (the `GhosttyPty::drop` SIGKILL backstop catches stragglers) → unlink
socket → exit. Same `signal()`+`wait()` primitive as `kill` escalation.

## Observability

`main.rs` installs `tracing_subscriber::fmt` to stderr with an `EnvFilter` from
`CAIRN_LOG`/`RUST_LOG` (default above); the library stays subscriber-neutral
(`observability.md`). The invocation pump wraps each spawned call in a span
carrying `instance`/`name`/`peer_uid` (from `ConnCtx`); `attach` opens a child
span with `client_id`. **No payload bytes are ever logged** at any level the
daemon controls. Prometheus `/metrics` and the `/debug/sessions` endpoint are
deferred.

## Security / trust model

- **Default (per-user) deployment:** UDS at `0o600` inside a `0o700` `cairn/`
  dir. Authentication is filesystem DAC; the peer is necessarily the owner.
  `whoami` reports that uid via `SO_PEERCRED`/`getpeereid` (`ConnCtx`).
- **Shared-group deployment** (`--dir-mode`/`--socket-mode` relaxed to a group):
  the trust domain widens from "the user" to "anyone who can `connect(2)`."
  Authorization stays **flat** — any connector can drive any session, exactly as
  zmx documents. This is intended for a shared service but is a real change from
  the per-user default, so operators must opt in explicitly via the mode knobs.
- `authenticate` is a UDS no-op; token auth arrives with the WebTransport
  transport.

## Testing

Harness: reuse the `round_trip.rs` `spawn_server` shape, but serve the **real**
`Daemon` (registry + handlers) on a tempdir UDS and drive it with
`cairn_protocol::client::*`. Real PTYs and real children (`cat`, `sh`, `sleep`, a
`trap`-ignoring shell); no mocking; the broadcast/stream is the sync barrier (the
`read_until_contains` pattern from `pty_io.rs`), not fixed sleeps. One file per
axis (`tests/daemon_*.rs`):

- **unary:** create→list→inspect round-trip; `not_found`, `name_in_use`;
  `restart` (running-without-force errors; with-force → new pid, same id);
  `rename`.
- **attach:** first event is `Snapshot`; `Input` echoes back as `Output`;
  `Detach` ends the stream; child exit → `Exited`; **late attach after exit →
  snapshot + `Exited`**.
- **send:** `inject` reaches the child **and does not steal leadership** — attach
  a leader, `send` from a separate call, assert the leader/size is unchanged.
- **logs:** `tail(n)` vs `all`; `follow` vs close-after-snapshot.
- **wait:** `sh -c 'exit 7'` → exit code 7.
- **kill:** `kill(TERM)` stops a normal shell; `kill(TERM, grace)` on
  `sh -c 'trap "" TERM; sleep 100'` escalates to SIGKILL after the grace;
  named-vs-numbered signal mapping.
- **whoami:** returns the caller's uid (via `ConnCtx`).
- One **subprocess smoke test** (the zmx-BATS analog): spawn the real
  `cairn-daemon` binary, run the `cairn` CLI against it — catches
  packaging/path/signal issues.
- WebTransport tests: out of scope (deferred with the WT transport).

## Deferred / future work

Recorded in `README.md`'s build list as well:

- **Idle-timeout enforcement** (`session-spec.idle-timeout-secs` is stored but
  inert) — also the reaper that removes lingering exited sessions.
- **Max-concurrent-sessions cap** + per-session memory accounting.
- **Time-windowed `logs`** (`since`) — needs a timestamped transcript cairn does
  not keep.
- **WebTransport transport** + bearer-token first-message auth + TLS / self-signed
  cert generation + Origin handling.
- **Prometheus `/metrics`** + `/debug/sessions` endpoint.
- **TOML config file** (`configuration.md`'s layered loader).

## Open questions

1. **Auto-spawn on first CLI command.** Deferred; the daemon is service-managed
   in v0. Revisit if the zero-setup UX proves important (`daemon-process-model.md`).
2. **uid → username resolution dependency.** `nix::unistd::User::from_uid` vs a
   dedicated `uzers`-style crate vs reporting the numeric uid. Numeric uid is the
   zero-dependency floor.
3. **Lingering-exited-session pressure.** Without reaping, exited sessions
   accumulate until restart. Acceptable for v0 but the first thing the idle-timeout
   work should address.
4. **`killpg` vs single-pid signal.** Signaling the child's process group catches
   the whole job (zmx's choice); confirm it does not over-signal in edge cases
   where the child has re-parented descendants.

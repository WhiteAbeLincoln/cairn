# Worker Backends

Where session workers run — host thread, subprocess, local VM, remote
machine — and how each backend plugs into the same daemon-side
abstraction. This doc extends the migration roadmap sketched in
[[daemon-process-model]] with a unified model that covers all the
sandboxing strengths cairn wants to support.

The discipline established in [[daemon-process-model]]'s
"Multi-daemon migration path" is the architectural lever this doc
builds on: every worker, regardless of location, talks the same
`Command`-channel wire format. The transport differs; the message
shapes do not.

## The session is the constant

A **session** is the unit that holds:

- One PTY (master + slave)
- One libghostty-vt `Terminal` instance
- One child process running inside the PTY
- All the on_pty_write / on_device_attributes / etc. callbacks from
  [[query-response-delegation]]
- A reply path back to whoever asked for it

A session is **1:1 with the child process**: it exists from spawn to
child exit (plus the post-exit normalisation window from
[[pty-lifecycle]]), then terminates. Sessions are never reused for
new jobs.

The libghostty `Terminal` lives **in the session**, not in the
daemon. This is the corollary to the v1 migration discipline:
daemon-level code never reaches into libghostty state; it asks the
session for what it needs via Commands. Putting the emulator in the
session also moves the largest C dependency (and the most likely
source of segfaults) out of the daemon's address space.

## Backends at a glance

| Backend | Where the session runs | Transport | Connection direction | Sandbox strength |
|---|---|---|---|---|
| **In-process** | Daemon thread | tokio channels | n/a | None (dev) |
| **Local subprocess** | Forked child of daemon | Unix socket | Daemon → session | seccomp / landlock |
| **Local VM** | Guest agent in Firecracker / Hypervisor.framework microVM | vsock | Daemon → session (daemon boots VM) | Hypervisor |
| **Remote** | Process on a different machine | wRPC over WebTransport | Spawner → daemon (outbound) | Physical / network |

All four implement the same `PtySession` trait
(`crates/cairn-pty/src/pty/session.rs`). The daemon never branches
on backend type for session operations — it sends a `Command`, the
session handles it, the response comes back. Branching is confined
to **construction** (how do we create a session?) and **lifecycle
ownership** (who tears it down?).

## The spawner pattern

For in-process and local-subprocess backends, the daemon can create
sessions directly: just fork. For local VM and remote backends, that
doesn't work — a VM has to be booted, a remote machine has to be
contacted. The daemon needs an intermediary.

A **spawner** is a long-lived service that creates sessions on demand:

```
Daemon  ←→  Spawner (long-lived)
              │
              ├─ fork → Session A (one job, exits when child does)
              ├─ fork → Session B
              └─ fork → Session C
```

The spawner is **not** a session and does **not** hold session state.
Its job is creating session processes and reporting their lifecycle.
Each session it creates is 1:1 with one child process, exactly as in
v1 — the spawner just exists to make session creation reachable from
the daemon when forking won't do it.

The session abstraction is unchanged. Whether the session was forked
by the daemon (v1) or by a spawner (v2/v3) is invisible to the
session itself.

### Why spawners aren't always present

A spawner is only useful when session creation requires more setup
than a `fork()`:

| Backend | Spawner needed? | Why |
|---|---|---|
| In-process | No | Just a `tokio::task::spawn` |
| Local subprocess | No | Daemon forks directly |
| Local VM | Yes | A VM must be booted and its guest agent contacted before a session can exist |
| Remote | Yes | A long-lived process must exist on the remote machine to accept session-creation requests |

For local VM and remote, the spawner is the entry point through which
the daemon reaches a foreign address space. The session that gets
forked inside is the same shape as any other session.

## Topology axis 1: who initiates the connection

The spawner pattern has two natural variants depending on who owns
the spawner's lifecycle:

### Daemon-initiated (used by Local VM)

The daemon boots the VM as part of session creation. The VM contains
a small `cairn-vm-agent` that, on boot, opens a vsock listener and
waits for the daemon to send `CreateSession`. The daemon then spawns
*one* session in *that* VM and runs it until the child exits, at which
point the VM is torn down.

In this configuration the spawner and the session are bundled — the
VM contains both, but conceptually:

- The vm-agent is the spawner (creates the session)
- The session is what the vm-agent forks inside the VM

For VM-per-session policy (max isolation), this collapses into "the
VM hosts one session and dies." For VM-pool policy (warm VMs reused),
the same vm-agent stays alive and creates multiple sessions over
time, mirroring the remote spawner.

Connection direction: **daemon → spawner**, because the daemon
controls VM existence. The daemon `connect()`s vsock when the VM
boots.

### Spawner-initiated (used by Remote)

A `cairn-spawner` binary runs on the remote machine, configured with
the daemon's URL and a worker token. It connects **outbound** to the
daemon (CI-runner style), registers, and idles waiting for work
assignments.

The daemon never opens connections to the remote machine — the remote
machine reaches out. This works through NAT, firewalls, and
restrictive corporate networks. It also means the daemon's inventory
of available spawners updates dynamically as workers come and go.

Connection direction: **spawner → daemon**, because the daemon does
not own the spawner's lifecycle.

### The pattern is the same; the carrier differs

| Aspect | Local VM | Remote |
|---|---|---|
| Spawner process | `cairn-vm-agent` (inside guest) | `cairn-spawner` (on remote machine) |
| Transport | vsock | wRPC over WebTransport |
| Initiated by | Daemon (boots VM, connects vsock) | Spawner (registers outbound) |
| Spawner lifecycle | Tied to VM lifecycle | Independent of daemon |
| Trust establishment | Daemon controls VM boot → implicit trust | mTLS / pre-shared token at registration |

The session protocol (`CreateSession`, `Input`, `Output`, `Resize`,
`Kill`, `Exit`) is byte-for-byte identical across both. Only the
spawner-management protocol (`Register`, `Heartbeat`, capacity
reporting) differs slightly, because in the daemon-initiated case the
daemon already knows what it boots and doesn't need a registration
handshake.

## Topology axis 2: 1:1 vs pool

Independent of transport, a spawner can be:

- **Single-tenant (1:1)**: one session per spawner lifetime. The
  spawner is created to host this specific session and terminates when
  the session ends. Maximises isolation. The "VM-per-session" policy
  fits here.
- **Multi-tenant (pool)**: the spawner takes one session, runs it to
  completion, then accepts the next. Each session is still 1:1 with a
  child process inside the spawner, but the spawner survives across
  many sessions. Maximises throughput / amortises VM-boot cost.

A pooled spawner can host **multiple concurrent sessions** if its
machine has capacity — the spawner reports a concurrency limit at
registration, and the daemon respects it. Each session is its own
process, forked by the spawner; libghostty's `!Send + !Sync` is fine
because each session has its own process and threads.

Choice axes are independent: a 1:1 VM uses daemon-initiated connection
+ single-tenant; a remote runner uses spawner-initiated + multi-tenant;
mixed configurations are possible.

## Backend descriptions

### In-process thread (v0)

Today's `crates/cairn-pty/src/pty/ghostty/worker.rs`. Session runs
on a dedicated OS thread inside the daemon process, in a tokio
`current_thread` runtime + `LocalSet`. No IPC.

Use: development, library embedding (e.g., a desktop app that
embeds cairn-pty directly), tests.

Crash blast radius: any session crash (worker thread panic) is caught
at the supervisor boundary, but libghostty-vt segfaults or memory
corruption take down the daemon process. Not suitable for production
with untrusted child processes.

See `pty/ghostty/worker.rs:62-152` for the spawn path.

### Local subprocess (v1)

The "zmx-style" backend: daemon forks a `cairn-session-worker` binary
per session. The binary runs the same logic as today's
`worker.rs`, but as its own process communicating with the daemon
over a Unix-domain socket.

Properties:

- 1:1 session per process. Same as zmx (`zmx/src/main.zig:777`).
- IPC: Unix socket per session, named by session ID, under
  `$XDG_RUNTIME_DIR/cairn/workers/`. Permissions `0o600`.
- Sandbox: cheap and host-local. `seccomp-bpf` filter (Linux) or
  Landlock (Linux 5.13+) applied before exec'ing the child. macOS
  has fewer options — sandbox-exec is deprecated but still functional.
- Crash isolation: libghostty segfault is bounded to one session.
  Daemon survives; other sessions unaffected.
- Lifecycle: daemon owns the process. Daemon SIGKILLs on shutdown,
  or sends graceful `Command::Shutdown`.

This is the minimum-cost step up from v0. Crash isolation alone is a
significant win.

### Local VM (v2)

Daemon boots a Firecracker (Linux) or Hypervisor.framework (macOS)
microVM per session, runs a `cairn-vm-agent` inside, and creates
exactly one session in that VM.

Properties:

- 1:1 VM per session is the default policy. Pooled VMs (warm reuse)
  are an optimisation, not a starting point.
- Transport: vsock between host and guest. Same `Command`/Output
  envelope as Unix socket.
- Connection direction: daemon-initiated. Daemon boots the VM,
  vsock-connects to the vm-agent, sends `CreateSession`.
- Sandbox: hypervisor-level. Kernel-grade isolation. Suitable for
  untrusted code (the agent-sandboxing use case from
  `crates/cairn-pty/Cargo.toml`'s likely future direction).
- Workspace: virtiofs / shared-folder mount. See Open Questions —
  this is where most of the design work lives.
- Cold-start cost: Firecracker ~100ms boot; macOS Hypervisor.framework
  similar. For interactive use this is invisible; for high-frequency
  job spawning, snapshot-based fast-restore is the standard mitigation
  (not v0).

Lifecycle: daemon owns the VM. VM dies when session ends (default) or
is held warm in a pool (configurable).

### Remote (v3)

A `cairn-spawner` runs on a separate machine, connects outbound to
the daemon's WebTransport endpoint, registers with capabilities and
capacity, and accepts session-creation requests.

Properties:

- Multi-tenant pooled spawner. One spawner can host many concurrent
  sessions (one process each).
- Transport: wRPC over WebTransport. Same wRPC stack as browser
  clients (see [[external-protocol]]), reused for daemon ↔ spawner.
- Connection direction: spawner-initiated. Critical for NAT / firewall
  traversal.
- Sandbox: physical and network separation. The remote machine itself
  is the boundary. Combine with subprocess-level sandboxing (seccomp)
  for defense in depth.
- Workspace: the limiting factor. See Open Questions; tentatively
  "workspace-as-git-repo" or "workspace-on-spawner-side" for v0;
  rsync / shared-filesystem strategies require more design.
- Use cases:
  - Bigger / dedicated hardware for agent execution
  - Different OS or architecture from the daemon
  - Pooled compute for automation bursts (Jira webhook fires → spawn
    job on next-available remote spawner)
  - Hosted-cairn deployments where users bring their own runners

Auth: separate token type from client tokens (see
[[authentication]]). Worker tokens are longer-lived, per-spawner,
revocable. Probably mTLS on top of token auth for defense-in-depth
if exposed beyond loopback.

## Wire format

All session-direction messages reuse the `sessions.*` operations
from [[external-protocol]]'s `cairn:daemon@0.1.0` WIT schema. The
session-channel is the same regardless of transport.

Spawner-management messages are new but small:

| Message | Direction | Purpose |
|---|---|---|
| `Register` | Spawner → daemon | Initial handshake (spawner-initiated case only); advertises capabilities (`os`, `arch`, tags, concurrency limit) and presents worker token |
| `Welcome` | Daemon → spawner | Accept registration, assign spawner ID |
| `Heartbeat` | Spawner ↔ daemon | Liveness and capacity updates; intervals ~10s |
| `CreateSession` | Daemon → spawner | Request to fork a new session; carries spawn params (command, env, cwd, size, scrollback) and session ID |
| `SessionCreated` | Spawner → daemon | Ack with session ID + spawner's internal handle |
| `SessionExited` | Spawner → daemon | Child exited; carries exit status and session ID |
| `KillSpawner` | Daemon → spawner | Graceful shutdown; spawner SIGHUPs all its sessions then exits |

`Register` is omitted in the daemon-initiated case (local VM): the
daemon already knows what it booted, so the vm-agent skips the
handshake and goes straight to listening for `CreateSession`.

`Heartbeat` is needed in both directions for the remote case (network
blips); in the local-VM case, vsock liveness is detectable directly,
but a low-frequency heartbeat is still useful for capacity reporting.

## Spawner failure modes

What happens when each kind of unit dies:

- **Session inside spawner crashes**: spawner detects via SIGCHLD,
  sends `SessionExited` (with synthetic exit if it can't determine
  status), then is ready for the next session. Other sessions in the
  same spawner unaffected.
- **Spawner disconnects** (network blip, process crash, machine
  reboot): daemon marks all its sessions as `spawner-disconnected`.
  After a configurable grace period (default short — sessions
  shouldn't survive their spawner indefinitely), sessions are torn
  down; clients receive disconnect events. If the spawner reconnects
  before grace expiry, sessions are revived (this requires the
  spawner to preserve session state across the gap — easy when the
  gap is purely network, harder when the spawner restarted).
- **Daemon dies**: spawners detect via dropped connection. For remote
  spawners, they should hold their sessions for a configurable
  grace period waiting for daemon reconnect (clients are also
  disconnected; the session has no consumer either way). For local VM
  spawners, daemon death implies the host process is gone — the VMs
  may keep running but become unreachable. Either tear them down or
  preserve them as zombie VMs the next daemon instance can adopt
  (out of scope for v0).

## Mapping to the roadmap

| Phase | Backends available | Spawner pattern | Reused transport infrastructure |
|---|---|---|---|
| v0 | In-process | No spawner needed | n/a |
| v1 | + Local subprocess | Daemon as implicit spawner | Unix socket IPC layer |
| v2 | + Local VM | Daemon-initiated spawner via vsock | + vsock + serialization |
| v3 | + Remote | Spawner-initiated registration over WT | + reuses wRPC stack from client connections |

Each phase reuses the prior phase's infrastructure; nothing has to be
redesigned. The session abstraction is stable across all of them.

## Open Questions

1. **Workspace transport for remote spawners.** The limiting factor
   for adoption. Default "workspace is a git repo, cloned by the
   agent" works for code-only tasks; doesn't cover non-code workspaces
   or live file editing. Defer until v3 lands and we have concrete
   user demand to choose between rsync-sync, workspace-on-spawner-side,
   and shared-filesystem strategies.
2. **Spawner pool policy.** Tag-matching, least-loaded, locality
   preference, priority — design exists; specifics need pinning when
   v3 lands. For v2 (local VM) the daemon is the only spawner, so
   policy is trivial.
3. **Warm VM pools.** For v2, default policy is VM-per-session. Pool
   reuse cuts boot latency by ~10x but complicates cleanup correctness
   (sanitising a used VM is non-trivial — must reset filesystem,
   network state, env vars, dotfiles). Probably v2.5.
4. **Cross-spawner session migration.** Could a long-running session
   migrate from one spawner to another (e.g., if a spawner is going
   away for maintenance)? Requires PTY+libghostty state replication.
   Almost certainly out of scope, but worth flagging that we're
   intentionally not designing for it.
5. **Spawner-side authentication of the daemon.** mTLS is the
   standard answer for remote; pre-shared keys with constant-time
   compare also work for v0. Decision needed before v3 ships. See
   [[authentication]].
6. **Concurrent-session limits on local VM spawners.** Does a single
   VM host multiple sessions (pooled) or one (single-tenant)? Affects
   spawner ↔ session topology inside the guest. Default single-tenant
   keeps the model uniform with remote pooled spawners (each session
   is still 1:1 with a process), at the cost of per-session VM boot.
7. **Backend selection policy.** Which backend does a given session
   use? Per-session config (`sandbox: vm | subprocess | remote`)? Per
   trigger source (interactive shells → host, automation triggers →
   remote pool)? Both? Likely a small policy DSL belongs in
   [[configuration]] once backends actually exist.
8. **Spawner discovery for clients.** A user wants to see "where is
   my session running?" UI-side concern, but the daemon needs to
   expose spawner identity in session metadata. Hooks into
   [[observability]].

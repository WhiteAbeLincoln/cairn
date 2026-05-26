# Configuration

What can be tuned on a cairn session and on the cairn daemon as a
whole, where each knob lives in code today, and how the layered
loading story should work. Scope: settings the operator or programmatic
caller can change. Adjacent docs: [[pty-lifecycle]] for idle/exit
policy, [[backpressure]] for capacity sizing, [[daemon-process-model]]
for listen address, [[authentication]] for credential material,
[[observability]] for log destination.

## Per-session configuration today

cairn's per-session config surface is `SpawnOptions`
(`crates/cairn-pty/src/pty/types.rs:21-28`): `command`, `size`,
`broadcast_capacity`, `scrollback_lines`. Constructed via
`SpawnOptions::new(command)` (`types.rs:31-38`) with builder-style
`with_*` setters. Defaults live in `new`.

### Command (program + args + env + cwd)

The entire process specification rides inside
`tokio::process::Command`. The worker reads it back at spawn time via
`as_std()` and translates argv, env, and cwd into a
`pty_process::Command` (`worker.rs:115-129`): `get_program()` +
`get_args()` for argv, `get_envs()` `(K, Option<V>)` pairs into
`builder.env(...)` or `env_remove(...)`, and `get_current_dir()` into
`current_dir(...)`.

There is **no default command** — the caller supplies one on every
`spawn`. No `$SHELL` fallback at the PTY layer, no login-shell argv0,
no implicit `$HOME` cwd. Those policies belong above the `PtySession`
trait; `PtySession` is a transport, not a shell-spawning utility
([[pty-lifecycle]]).

Documented limitation at `worker.rs:108-114`: `std::process::Command`
exposes env overrides via `get_envs()` but does **not** expose
whether `env_clear()` was called. Callers wanting a hermetic env
cannot reach it through the current adapter — the std API hides the
bit. Fix requires either a new adapter or a richer `SpawnOptions::env`
mode (extend / replace / clear).

### Initial terminal size

`TermSize` (`types.rs:1-13`) is two `u16`s. Default `80 × 24`
(`types.rs:9-13`). The worker applies the size to the kernel PTY at
`worker.rs:94-102` and to the libghostty emulator at
`worker.rs:210-214`. Subsequent resizes go through the session's
`resize` command ([[resize-semantics]]).

### Scrollback line count

`SpawnOptions::scrollback_lines` (`types.rs:25-27`) bounds libghostty's
history retention. Default `1000` (`types.rs:36`), passed to
`TerminalOptions.max_scrollback` at `worker.rs:213`.

**Divergence from zmx.** zmx defaults to `10_000_000` lines
(`zmx/src/main.zig:473`, also `util.zig:1003,1035`) — four orders of
magnitude larger. The value was bumped in commit `d9e8cab` ("feat:
increase max scrollback") closing github.com/neurosnap/zmx/issues/9;
the commit message gives no quantitative rationale beyond "increase".
cairn's default minimises worst-case memory per session
([[backpressure]]); zmx errs on the side of "never lose user history".
Neither is reachable below the `SpawnOptions` API today — no env var,
flag, or file feeds it.

### Broadcast capacity

`SpawnOptions::broadcast_capacity` (`types.rs:24`) sizes the
`tokio::sync::broadcast` ring fanning PTY output to subscribers.
Default `1024` messages (`types.rs:35`), clamped to ≥1 at
`worker.rs:38` because `broadcast::channel(0)` panics. Units are
**messages**, not bytes — each message is one `pty.read()` worth of
output, up to 64 KiB. See [[backpressure]] for how this knob
interacts with slow clients.

### Idle timeout, maximum lifetime, kill-on-last-detach

**cairn has none of these today.** Nothing in `SpawnOptions`, the
worker, or `GhosttyPty` watches wall clock or subscriber count. The
only natural exit is the child process exiting (`worker.rs`
`child.wait()` arm) — see [[pty-lifecycle]]. zmx is identical: no idle
timer, no max lifetime, no "die on last detach" at session granularity.
zmx's `-d` flag on `run` (`zmx/src/main.zig:186-191`) detaches the
*calling client*, not the session.

These are session-policy decisions that belong above the `PtySession`
trait — in whatever supervisor owns the `Arc<dyn PtySession>` map.
Flagged here so the design doc treats them as *explicit non-features*
rather than oversights.

## Daemon-level configuration today

There is **no cairn daemon yet** — `cairn-pty` is a library. The
daemon-level knobs below are forward-looking, listed so the design
doc has a target shape.

zmx's daemon-level configuration is the `Cfg` struct
(`zmx/src/main.zig:467-535`):

- `socket_dir` — resolved from `ZMX_DIR` → `XDG_RUNTIME_DIR/zmx` →
  `TMPDIR/zmx-{uid}` → `/tmp/zmx-{uid}` (`main.zig:504-517`).
- `log_dir` — always `{socket_dir}/logs` (`main.zig:479`).
- `max_scrollback` — hardcoded `10_000_000`, **not** env-or-flag
  configurable (`main.zig:473`).
- `dir_mode` / `log_mode` — octal `0o750` / `0o640`, settable via
  `ZMX_DIR_MODE` / `ZMX_LOG_MODE` (`main.zig:482-490`).

The full zmx env surface (`zmx/README.md:179-188`,
`main.zig:1348-1355`): `SHELL` (default shell, resolved at
`util.zig:637-639`, fallback `/bin/sh`); `ZMX_DIR`, `XDG_RUNTIME_DIR`,
`TMPDIR` (socket dir precedence); `ZMX_SESSION` (injected into child
env at `main.zig:669-673`); `ZMX_SESSION_PREFIX`; `ZMX_DIR_MODE`,
`ZMX_LOG_MODE`.

zmx has **no config file** — `grep` across `zmx/src/` for `toml`,
`yaml`, `kdl`, or `XDG_CONFIG` returns nothing. All configuration is
env vars plus per-invocation CLI flags. The README explicitly defends
this posture (`zmx/README.md:464-466`): "Every configuration option
is a burden for us maintainers."

For cairn the daemon-level surface is necessarily larger because the
process serves two listeners (UDS for local CLI, WebTransport for
remote and browser). Minimum the daemon must know:

- **WT listen address** — `host:port` for the WebTransport (HTTP/3)
  listener; defaults to loopback ([[daemon-process-model]],
  [[transports]]).
- **UDS path** — defaults to `$XDG_RUNTIME_DIR/cairn/cairn.sock` on
  Linux, `$TMPDIR/cairn/cairn.sock` on macOS ([[transports]]).
- **Token store location** — for client auth on the WT path
  ([[authentication]]). Plausible default: `$XDG_RUNTIME_DIR/cairn/token`.
- **TLS material** — cert + key paths for the WT endpoint (QUIC
  requires TLS). For loopback, generate a self-signed cert at
  daemon start and pin via `serverCertificateHashes` in the
  browser. For non-loopback, defer to a real cert + reverse proxy.
- **Max concurrent sessions** — registry cap. Default `unlimited`,
  but the knob lets operators bound a misbehaving caller.
- **Per-session memory ceiling** — scrollback × broadcast ring; an
  aggregate cap belongs at the daemon level ([[backpressure]]).
- **Log level / format / destination** — `tracing` initialisation
  ([[observability]]).
- **File descriptor budget** — each session consumes ~3 fds (master,
  signal pipe, broadcast wakers) plus 1 per attached client. Document
  the formula; don't try to enforce in cairn.

None of these are wired today.

## Loading mechanism: recommended layering

cairn should adopt the conventional layered model — defaults < file
< env vars < CLI flags < programmatic override. Each later layer
only sets the fields it cares about; the earlier layer fills the rest.

```
SpawnOptions defaults
  <- daemon-wide defaults from config file
    <- env vars (CAIRN_*)
      <- CLI flags from the daemon binary
        <- per-call programmatic override (Rust API)
```

Concrete recommendations:

- **Format: TOML.** Same family as `Cargo.toml` and `rustfmt.toml`;
  the `toml` crate is already in most cairn callers' ecosystems.
  KDL is unfamiliar to most Rust shops; YAML's edge cases (Norway,
  anchors) are not worth importing.
- **Location: `$XDG_CONFIG_HOME/cairn/config.toml`**, falling back to
  `$HOME/.config/cairn/config.toml`. The daemon also accepts
  `--config <path>` to override.
- **Env prefix: `CAIRN_`** — `CAIRN_LISTEN`, `CAIRN_LOG`,
  `CAIRN_CONFIG`, `CAIRN_SCROLLBACK_LINES`, etc. Mirrors zmx's
  `ZMX_*` convention.
- **Precedence: CLI > env > file > defaults.** Standard ordering;
  matches `tracing-subscriber`'s `RUST_LOG` semantics.
- **Programmatic override always wins.** When a Rust caller passes a
  `SpawnOptions` to `GhosttyPty::spawn`, those values are final —
  no daemon-level layer reaches in to "correct" them. Preserves
  library use.
- **Per-session inline override at create time.** The
  `sessions.create(spec)` invocation's `session-spec` record carries
  command, env, workdir, scrollback, timeout, etc. — these layer on
  top of daemon defaults the same way CLI flags do for the daemon
  ([[external-protocol]]).

## What cairn already gets right vs. what's missing

Right: per-session config is one struct with explicit defaults and
builder setters (`types.rs:30-53`), trivially merge-able once higher
layers exist. Defaults are conservative on memory (scrollback 1000)
rather than retention (zmx 10M) — easy to raise per-session, hard to
recover from an OOM. No global state — every session has its own
`SpawnOptions`, so a multi-tenant daemon can vary defaults per caller
without thread-local hacks.

Missing: no file loader, no env loader, no CLI parser —
`SpawnOptions::new` is the *only* way values reach the worker today.
No `env_clear` semantics (`worker.rs:108-114`). No `scrollback_lines`
env override — operators who want zmx-like 10M retention must thread
it through every call site. No daemon-level `Cfg` analogue.

## Open questions

- **Default command policy.** Should the cairn daemon default to
  `$SHELL` + login-shell argv0, matching zmx's `execChild` path
  (`zmx/src/main.zig:685-694`)? Or refuse to spawn without an
  explicit command, pushing the policy to the API client? Browser
  callers (ghostty-web) and CLI callers want different things.
- **Idle timeout shape.** Per-session, per-daemon, or both? Wall
  time, "no client attached" time, or "no PTY output" time? See
  [[pty-lifecycle]] — must compose with that doc's shutdown story.
- **`scrollback_lines` upper bound.** zmx's 10M × one session is
  fine; × 1000 sessions is 10 GiB of VT state. Should the daemon
  cap the per-session value, or trust callers?
- **Hot reload.** Reload on SIGHUP, or is "restart the daemon"
  acceptable given the session-survival story in [[pty-lifecycle]]?
- **Per-attach inline config in the protocol.** Which fields may a
  *new-session* attach frame ([[external-protocol]]) set? `size` and
  `command` clearly; `scrollback` feels operator-controlled — a
  malicious client requesting 10M lines could exhaust memory.
  Whitelist, not blacklist.
- **Auth material loading.** Token files, OIDC issuers, mTLS roots —
  in `config.toml` alongside everything else, or a sidecar
  `auth.toml` for permissions reasons? See [[authentication]].
- **Listen-address default.** Loopback-only (`127.0.0.1`) fails
  closed but breaks the LAN/SSH case zmx leans into
  (`zmx/README.md:389-441`). Document the LAN-exposure flag
  prominently ([[daemon-process-model]]).
- **CLI vs. config file scope.** zmx is CLI-only and defends it
  (`zmx/README.md:464-466`). cairn needs a file because auth/TLS
  material doesn't fit on a command line — but once a file exists,
  the temptation to grow it is real. Establish a written principle
  for what belongs in the file vs. stays CLI-only before the file
  ships.

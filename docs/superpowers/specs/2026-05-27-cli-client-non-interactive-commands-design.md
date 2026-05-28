# Cairn CLI client — non-interactive commands design

## Status

Design. Defines the non-interactive half of the `cairn` CLI client binary
(`crates/cairn-client`): the eleven commands that finish out item 7 ("CLI
client binary") of `docs/architecture/pty-session/README.md`. The
**interactive** half — `attach`, `exec`, `run`, and the shared connection /
terminal-guard / detach-matcher / signal-handling layer — landed in
`docs/superpowers/specs/2026-05-27-cli-client-interactive-attach-design.md`
and is already shipped.

Commands covered here: `list` (alias `ls`), `inspect`, `rename`, `restart`,
`kill`, `kick` (alias `detach`), `send`, `logs`, `wait`, `whoami`, `version`.

Builds on:
- `docs/superpowers/specs/2026-05-26-daemon-binary-design.md` — the daemon
  this client drives. Every wire op below is already implemented:
  `sessions::{list_all, inspect, rename, restart, kill, kick, send, wait,
  logs}` and `meta::{whoami, version}`.
- `docs/superpowers/specs/2026-05-26-daemon-protocol-design.md` — the wire
  protocol / generated client functions.
- `docs/superpowers/specs/2026-05-27-cli-client-interactive-attach-design.md`
  — the connection layer, error-surface conventions, and exit-code base set
  this spec extends.

The CLI surface for these commands already exists in
`crates/cairn-client/src/cli.rs` (the clap derive structure shipped with
the interactive spec). `main.rs` dispatches `attach`/`exec`/`run`/
`completion` today and bails on every other subcommand
(`main.rs:64-66`). This spec wires up the remaining eleven, refactors the
connection layer to make the future WebTransport addition a single-file
change, and makes one small `cli.rs` correction.

## Scope (v0)

- **Plain-text output only.** The shared `--output {plain,json,jsonl,wide}`
  value-enum that the existing TODOs in `cli.rs:126-136,224-226` envision
  is **deferred**. So is `list --filter`. Scripts that need machine-readable
  state today should call wRPC directly (the protocol-crate example
  `webtest.rs` demonstrates that path).
- **UDS only**, inheriting the interactive spec's transport constraint.
  The connection-layer refactor below is the seam that makes WebTransport
  a future single-file change inside `connect.rs`.
- **Multi-target resolution is client-side** via one `sessions.list_all`
  call per command. Literal name/uuid resolution mirrors the daemon's
  `registry.resolve` precedence (name first, then id). Globs (`*`, `?`,
  `[`) match against session **names**. `--latest` picks the highest
  `created_at_unix_ms`; `--all` is every session with `exit.is_none()`.
- **Best-effort multi-target semantics.** Per-target errors print to
  stderr and contribute a non-zero exit code, but other targets in the
  same invocation still get their work done. Empty resolution from
  `--all` or a positional list with no matches is exit 2.
- **Default `kill` blocks until the targeted sessions have actually
  exited and been reaped** (matches the existing cli.rs docstring at
  `cli.rs:163`). `--no-wait` returns immediately after dispatching the
  signal; `--timeout T` is independent of `--no-wait` and arms the
  *daemon-side* SIGKILL escalation after the grace period.
- **Deferred:** structured output (`--output`), `list --filter`,
  `--color` actually emitting color, `cairn version` returning non-zero
  on unreachable daemon, ANSI stripping for snapshot replay in
  interactive paths, `logs --since <timestamp>` (needs a timestamped
  transcript in the daemon). See
  [Deferred / future work](#deferred--future-work).

### `cli.rs` edits

One small correction; everything else in `cli.rs` is already in its final
v0 shape and is wired through unchanged.

1. **Drop `conflicts_with = "timeout"` from `--no-wait`** (`cli.rs:175`)
   and rewrite the trailing "Mutually exclusive with `--timeout`" sentence
   in its help text. `--no-wait` and `--timeout` are orthogonal: the
   former controls *whether the client blocks*, the latter arms the
   *daemon-side* SIGKILL escalation. A user can want both ("ensure it
   dies, but don't wait around"); the spec's `kill.rs` honors that
   combination.

## Connection layer — forward-looking refactor

The wRPC generated client functions are already trait-generic
(`pub fn list_all<'a, C: wrpc_transport::Invoke>(...)` etc.), so the
protocol crate needs no changes. The seam that matters lives entirely
in `connect.rs`. Today's file has two structural shapes that would force
WebTransport changes outside it:

- `pub struct Endpoint { path: PathBuf }` — a struct can't grow
  alternative variants without rippling.
- `Endpoint::path() -> &Path` — used purely for human-readable error
  messages. WT endpoints have no filesystem path.

### Refactor in this spec

Three small edits to `connect.rs` plus two grep-fixable callsites:

1. `struct Endpoint { path: PathBuf }` → `enum Endpoint { Unix(PathBuf) }`.
   `resolve()` and `from_uri()` are unchanged structurally; their bodies
   construct `Endpoint::Unix(...)`. Adding a variant later is a localized
   match-arm edit.
2. Replace `Endpoint::path() -> &Path` with
   `Endpoint::label() -> String` (e.g. `"unix:///run/cairn/cairn.sock"`).
   Two callsites: `attach.rs`'s reconnect/connect-failure error message
   and the resolver that moves out of `main.rs:72-87` into
   `targets.rs`. Pure-string change; both are user-facing diagnostics.
3. Document the discipline at the top of `connect.rs`: **only this file
   may name `wrpc_transport::*` types**. Every other module touches the
   wRPC backend through `Endpoint::client()`'s return value generically
   — the value is passed straight into the generated
   `cairn_protocol::client::*` functions, which are themselves
   `<C: wrpc_transport::Invoke>`.

The existing `pub type Client = wrpc_transport::unix::Client<PathBuf>`
alias stays for v0. Nothing outside `connect.rs` *names* it today
(callers do `let client = endpoint.client();` and let inference handle
the type — verified across `attach.rs`, `exec.rs`, `main.rs`), and the
docs above pin that as the rule.

### Future WT addition (single file: `connect.rs`)

When `ws://` / `wss://` lands, the only edits are inside `connect.rs`:

1. Add `Endpoint::Wt { url: url::Url, token: SecretString }`. Replace
   the existing `bail!` arm in `from_uri()` with the parse + construct.
2. Replace the `pub type Client` alias with an opaque wrapper enum and
   write the forwarding `Invoke` impl that dispatches by variant:

   ```rust
   pub enum Client {
       Unix(wrpc_transport::unix::Client<PathBuf>),
       Wt(wrpc_transport::wt::Client<...>),
   }
   impl wrpc_transport::Invoke for Client {
       type Context = ();        // or an enum, if a variant requires one
       type Outgoing = ...;      // futures::future::Either, or pin-box
       type Incoming = ...;
       fn invoke<P>(&self, cx, instance, func, params, paths)
           -> impl Future<Output = anyhow::Result<(Self::Outgoing, Self::Incoming)>> + Send
       where P: AsRef<[Option<usize>]> + Send + Sync
       {
           async move {
               match self {
                   Client::Unix(c) => c.invoke(cx, instance, func, params, paths).await,
                   Client::Wt(c)   => c.invoke(cx, instance, func, params, paths).await,
               }
           }
       }
   }
   ```
3. Extend `Endpoint::client()` to construct the right variant per
   `match self`. Pre-call `meta.authenticate(token)` happens inside this
   constructor for the `Wt` arm so every other module stays oblivious to
   the first-message auth dance.

The `Invoke` forwarding impl is the only nontrivial code, and it never
leaves `connect.rs`. Daemon-side WT work (the listener + the
`authenticate` gate) is daemon scope and out-of-spec here.

## Shared resolver (`targets.rs`)

The same name-or-glob expansion is needed by every multi-session
command and (in a degenerate form) by every single-session command.
One module owns it; the interactive spec's `resolve_target` helper in
`main.rs:72-87` moves here and is generalized.

```rust
pub struct ResolvedTarget {
    pub id: String,            // session UUID
    pub name: Option<String>,  // for prefix output / error messages
    pub info: SessionInfo,     // for inspect-renders / kill-state checks
}

pub async fn resolve_one(ep: &Endpoint, t: &SessionTarget) -> Result<ResolvedTarget>;

pub struct ResolvedMany {
    pub matched: Vec<ResolvedTarget>,
    pub unresolved: Vec<String>,   // tokens with no match (positional list only)
}
pub async fn resolve_many(ep: &Endpoint, t: &SessionTargets) -> Result<ResolvedMany>;
```

**Algorithm — one `sessions.list_all` call regardless of input:**

`resolve_one`:
- `--latest`: pick max `created_at_unix_ms` from `list_all`; error
  `no sessions to operate on` if empty (preserves the message
  currently in `main.rs:80`).
- Literal `session: Some(s)`: match against `list_all` first by exact
  name, then by exact id; error `no session matches {s}` if neither.
  Mirrors the daemon's `registry.resolve` precedence and gives a clean
  client-side error instead of waiting on the wire `session.not_found`.

`resolve_many` — single `list_all`, then for each token:
- **Glob heuristic**: a token containing `*`, `?`, or `[` is compiled
  with `globset::Glob` and matched against session **names**. Sessions
  without a name are skipped for glob matches (uuids don't glob
  meaningfully).
- **Literal**: exact name then exact id, same as `resolve_one`.
- **`--latest`**: same as `resolve_one`.
- **`--all`**: every session with `exit.is_none()`. Exited sessions
  are filtered because the canonical `--all` use case is "kill
  everything still running" (mirrors `docker kill $(docker ps -q)`).
- **De-duplicate** the union (a session matching both a literal and an
  overlapping glob shouldn't be operated on twice). Stable order:
  preserve first-occurrence position so per-target output is
  predictable.
- **Empty `matched` set**: the command exits 2 with `no sessions
  matched`. Catches typos in literal lists and dead globs.
- **Per-token misses** (a literal in a positional list that didn't
  resolve, when other tokens did): collected into `unresolved` and
  surfaced by the calling command as `error: <token>: no session
  matches`, contributing to the non-zero exit code but not aborting
  the other targets.

**Glob library**: `globset = "0.4"`. Compile glob → matcher once and
apply per session. Already transitively present in the workspace
dep-graph via `tracing`/`ignore`; the spec adds it as an explicit
direct dependency of `cairn-client`.

## Per-command behavior

Grouped by shape. Every command resolves `endpoint = Endpoint::resolve(
cli.daemon.as_deref())?` in `main.rs` and dispatches to its module.

### Metadata (no target)

**`cairn whoami` (`meta.rs::whoami`)**

- Calls `meta::whoami(client, ())`. Prints the daemon-returned identity
  string + newline.
- Wire `Ok(Err(WireError))`: `eprintln!("error: {}: {}", e.code, e.message);
  exit 1`.
- Connect failure: `cannot reach cairn-daemon at <label>: <e>`; exit 1.
  `whoami` doubles as the connectivity probe and is the one place
  non-zero on unreachable is the right behavior.

**`cairn version` (`meta.rs::version`)**

- Always prints the client first: `cairn <CARGO_PKG_VERSION>`.
- Then calls `meta::version(...)`. On success:
  `daemon: <v> · <protocol>`. On failure (including transport):
  `daemon: unreachable: <err>`. Exit 0 in either case — the question
  was "what version am I", and the daemon's status is informational.
  Connectivity probing is `whoami`'s job, not `version`'s.

### Read-only (single target)

**`cairn list` / `cairn ls` (`list.rs`)**

- `sessions::list_all(client, ())`. No filters in v0; the `list --filter`
  TODO at `cli.rs:126-136` survives untouched.
- Plain table to stdout, sorted by `created_at_unix_ms` ascending.
  Columns: `NAME  ID  SIZE  CLIENTS  STATE`. State is `running`, or
  `exited code=N` / `exited signal=N`. Dynamic column widths from
  longest entry per column, capped at 40 for `NAME` (truncate with
  `…`).
- Empty list: a single line `no sessions`. Exit 0 — distinct from
  `kill --all` (where empty *is* an error). No-sessions is a normal
  state, not a query failure.

**`cairn inspect` (`inspect.rs`)**

- `resolve_one`, then `sessions::inspect(client, (), &id)` (a fresh
  inspect call rather than reusing the stale `list_all` snapshot, so
  the output reflects current daemon state).
- Plain key-value block, one field per line, two-column aligned.
  Fields in order:
  `id`, `name`, `pid` (`-` if `None`), `state`, `size`, `created_at`,
  `command` (joined with shell-quoted args), `workdir`, `tty`, `stdin`,
  `env_inherit`, `idle_timeout`, `scrollback_lines`, `attached_clients`
  (count + comma-joined ids).
- `state` derives from `exit`: `running` /
  `exited code=N at <RFC3339>` / `exited signal=N at <RFC3339>`.
- `created_at` rendered RFC3339 from `unix_ms` via the `time` crate
  (already in the workspace dep-graph).
- Wire error → `error: <code>: <msg>`; exit 1.

### Mutators

**`cairn rename` (`rename.rs`)**

- `resolve_one` → `sessions::rename(client, (), &id, &new_name)`.
- Silent on success; exit 0. Mirrors `mv`.
- Wire error → `error: <code>: <msg>` (name collision is whatever code
  the daemon emits today); exit 1.

**`cairn restart` (`restart.rs`)**

- `resolve_one` → `sessions::restart(client, (), &id, force)`.
- Silent on success. The daemon assigns a new PID and the user can
  `cairn inspect` after if they care. The wire op returns
  `result<_, error>` with no payload, so there's nothing to print on
  success.
- Common error: restarting a live session without `--force` →
  `error: <code>: <msg>`; exit 1.

**`cairn kill` (`kill.rs`) — multi-target**

The complex one. Per the question answered during brainstorming,
`--no-wait` and `--timeout` are orthogonal:

```
let grace_ms = args.timeout.map(|d| u32::try_from(d.as_millis()).unwrap_or(u32::MAX));
// grace_ms is honored regardless of --no-wait, so "ensure it dies but
// don't make me wait" is expressible as `--no-wait --timeout T`.
// Saturating on overflow rather than dropping to None: a humantime
// value above u32 milliseconds (~49 days) is absurd input but still
// expresses intent — clamp it instead of silently disabling escalation.

for target in resolve_many(...).matched {
    spawn(async move {
        sessions::kill(client, (), &id, &signal, grace_ms).await?;
        if !args.no_wait {
            // Wire wait is `future<exit-status>`; drive it to completion.
            sessions::wait(client, (), &id).await?.await;
        }
    });
}
join_all(...).await;
```

- `--signal` (`cli.rs:170`) defaults to `TERM`. The numeric form
  (`--signal 9`) and the named form (`SIGINT`/`INT`/`int`) both round-trip
  through the existing `Signal` parser and reach the daemon as the
  WIT `signal` variant.
- Per-target failures print to stderr and contribute exit 1. All
  targets succeeding → exit 0. Empty `matched` → exit 2.
- Quiet on success (POSIX `kill` convention).

**`cairn kick` / `cairn detach` (`kick.rs`)**

- `resolve_many`; for each `id` call `sessions::kick(client, (), &id,
  args.client.clone())` in parallel.
- `--client` is interpreted per-target by the daemon (operators
  typically pair it with a single target; the multi-target use case is
  `--all` with no `--client`).
- **Idempotent on `session.not_found`**: a race between resolve and
  kick (the session exited on its own) is logged at `info` and not
  counted as a per-target error — it's the desired terminal state
  anyway. All other errors → stderr + exit 1.

**`cairn send` (`send.rs`)**

- `resolve_one` for the target.
- **Source selection**:
  - `args.input` non-empty: build a single `Bytes` chunk by joining
    argv with `' '`, append `\n` unless `--raw`. Emit one-batch stream.
  - `args.input` empty: stream stdin in 8 KiB chunks (raw, no
    transformation) until EOF.
- `sessions::send(client, (), &id, chunks_stream).await?`. Returns once
  the input stream ends.
- Quiet on success. Wire error → `error: <code>: <msg>`; exit 1.

### Streaming

**`cairn logs` (`logs.rs`) — multi-target with interleaving**

- `resolve_many`; for each target open
  `sessions::logs(client, (), &id, &window, args.follow)` where
  `window = args.tail.map(LogWindow::Tail).unwrap_or(LogWindow::All)`.
- Drive each `(stream, io_future)` pair on its own task; spawn each
  `io_future` (matches the proven `webtest.rs:309-316` pattern). One
  mpsc channel funnels per-session output into a single writer task
  that owns stdout, so prefix-mode never tears a line across sessions.
- **`--strip`**: ANSI-escape removal via the `strip-ansi-escapes`
  crate. Applied *before* prefix.
- **`--prefix`**: prepend `<name>: ` to every newline-terminated line.
  A small per-stream `LineBuffer` accumulates a trailing partial line
  and emits the prefix on the next `\n` (or on stream end). Sessions
  with no name use the first 8 hex of their id.
- **No `--follow`**: each per-target stream emits one snapshot batch
  then ends; writer exits when all per-target tasks finish.
- **`--follow`**: streams emit snapshot + live output; the command
  runs until all sessions exit (each `logs` stream ends on session
  close per `handlers/logs.rs:53`) or until SIGINT/SIGTERM.
- **SIGINT / SIGTERM**: a `tokio::signal` trap drops the streams and
  exits cleanly. We don't go raw, so no terminal-state restore is
  needed.
- Per-stream errors → stderr `error: <name>: <code>: <msg>`; exit 1
  if any. All-success exit 0.

**`cairn wait` (`wait.rs`) — single target**

- `resolve_one` → `sessions::wait(client, (), &id)` returns
  `future<exit-status>`. Await it.
- Exit-code propagation mirrors `attach`:
  - `code: Some(n)` → exit `n`.
  - `signal: Some(s)` → exit `128 + s`.
  - Both `None` (shouldn't happen) → exit 1.
- **`--timeout`** (`cli.rs:198`): `tokio::time::timeout(d, future)`;
  on elapse, **exit 124** (GNU `timeout` convention) without killing
  the session. The cli.rs docstring at `cli.rs:196-197` promises "a
  distinct non-zero code without killing the session" — 124 is the
  obvious choice and is recognized across the shell-scripting
  ecosystem.
- Wire/transport error mid-wait → `error: <e>`; exit 1.

## Non-TTY / scripted usage

The non-interactive commands have no termios/raw-mode concerns at all —
the only output stream that could be a TTY is `cairn logs --follow`,
and even there we never go raw (no input forwarding, no resize
handling). So:

- `list`, `inspect`, `whoami`, `version`: plain text either way. No
  color logic in v0 (the `--color` flag from the interactive spec
  exists globally but is treated as `never` everywhere here; emitting
  color is recorded as future work).
- `cairn logs` with stdout piped/redirected: emits bytes verbatim
  including ANSI sequences from the snapshot. `--strip` is the
  script-friendly toggle. Unlike `cairn attach`'s redirect path
  (`web-vs-cli-clients.md` "script-style capture"), `logs` does **not**
  warn on stderr — piping logs to a file is a deliberate, normal
  workflow.
- All commands respect `NO_COLOR` by being uncolored regardless.

## Exit codes

Single binary-wide table that extends the interactive spec's set:

| Outcome | Code |
|---|---|
| Success | 0 |
| `cairn list` empty | 0 |
| `cairn version` with daemon unreachable | 0 (informational) |
| `cairn wait --timeout` elapsed | 124 (GNU `timeout` convention) |
| Multi-target command with empty resolution (incl. `--all` matching nothing) | 2 |
| CLI usage / parse error | 2 (clap's default) |
| Multi-target command with ≥1 per-target failure | 1 |
| Single-target wire error (`session.not_found`, etc.) | 1 |
| `whoami` with daemon unreachable | 1 (connectivity probe) |
| Any other transport / I/O error on a daemon-requiring command | 1 |

Interactive-spec codes (child exit, `128+signal`, detach) keep their
meanings for `attach`/`exec`/`run`.

## Error surface

Single error sink across the binary, consistent with the interactive
spec:

- Wire `Err(WireError { code, message })` → stderr as
  `error: <code>: <message>`. Code identifiers are stable
  (`cairn-protocol::error_codes::*` for the ones that carry protocol
  meaning).
- Transport / I/O errors → stderr as `error: <context>: <e>`, where
  `<context>` is the command name (`list`, `kill`, etc.) so users see
  the operation that failed.
- Per-target failures in multi-target ops include the target token:
  `error: bash-3f9ac2: session.not_found: …`.
- No stack traces; the `tracing` layer at `-vvv` is the diagnostic
  channel.

## Daemon-side changes

**None.** Every wire op needed by these commands already ships:
`sessions::{list_all, inspect, rename, restart, kill, kick, send, wait,
logs}` and `meta::{whoami, version}`. `kill` already accepts the
optional `grace_ms` daemon-side escalation (`handlers/sessions.rs:63-89`);
`wait` already returns a `future<exit-status>` (`handlers/wait.rs:12`).

The interactive spec's three daemon edits (the `client.kicked` /
`client.lagged` distinction, explicit-env-overrides-inherited
precedence, default-name inference) are already landed and benefit this
spec implicitly.

## Testing

Per the project's test discipline — assert behavior through the real
interface, never restate structure.

### Pure-logic units (highest-value, deterministic)

- `targets::resolve_many`: literal-vs-glob dispatch; `--all` excludes
  sessions with `exit.is_some()`; de-dup of literal + overlapping glob;
  unresolved-tokens list captures only positional-list misses (not
  `--all` empties). Driven by a `Vec<SessionInfo>` fixture; no daemon.
- `kill::grace_ms_conversion`: `Duration` → `Option<u32>` (None when
  `--timeout` absent), saturating at `u32::MAX` for absurdly large
  humantime inputs.
- `send::argv_to_chunk`: argv joined with single space, `\n` appended
  iff not `--raw`; multi-word, embedded quotes, empty-arg edges.
- `logs::LineBuffer`: prefix prepended once per newline-terminated
  line; trailing partial line buffered; flush on stream end.
- `logs::strip_ansi`: a couple of fixtures around the
  `strip-ansi-escapes` wiring (delegate; we're asserting we plumbed it,
  not re-testing the crate).

### Integration — through the real daemon

A small per-test helper spawns `cairn-daemon` on a tempdir socket (same
shape as the interactive spec's PTY harness, minus the pty wiring) and
invokes the `cairn` binary against it.

- `list`: create N sessions via the protocol client; `cairn list` and
  assert the table contains each name (substring match, never column
  position).
- `inspect`: create a session with known env/workdir; `cairn inspect`
  renders those facts in the key-value block.
- `rename`: `cairn rename old --to new`; `list_all` then shows `new`.
- `restart`: spawn a known short-lived child; `cairn restart --force
  <id>`; `inspect` shows a different `pid`.
- `kill` default (block-until-exit): spawn `sleep 30`; `cairn kill <id>`
  returns within seconds and the session is `exited`.
- `kill --no-wait --timeout 1s`: spawn `sleep 30`; command returns
  near-instantly; after ~1.5s the session is `exited` (daemon
  escalated). This proves the orthogonality fix.
- `kill --all` on an empty registry: exit 2 with `no sessions matched`.
- `kick`: attach a `logs --follow` client; `cairn kick --all`; the
  streaming client sees stream end with a `client.kicked` error.
- `send`: argv mode → session sees `<argv joined> \n`; stdin mode
  (`echo -n raw | cairn send <id>`) → exact bytes, no trailing newline.
- `logs`: a session emits known output; `cairn logs <id>` (no follow)
  prints the snapshot and exits; `cairn logs --tail 1 <id>` emits the
  last line only; `cairn logs --strip <id>` removes inserted ANSI codes.
- `logs --prefix` with two sessions: each line carries the correct
  `<name>: ` prefix, no inter-session tearing.
- `wait`: short-lived child → propagates the child's exit code;
  signal-killed → `128 + signal`; `--timeout` elapsed → exit 124, session
  still alive.
- `whoami`: returns non-empty (the daemon's UDS identity string).
- `version`: prints two lines; daemon line carries `daemon` and
  `protocol` from the wire `version-info`.
- `version` with a wrong `--daemon` path: client line still prints;
  daemon line says `unreachable: …`; exit 0.

### Non-coverage (deliberate)

- No tests asserting "this clap subcommand exists" or "this flag is
  parsed" — `Cli::command().debug_assert()` already covers those
  structurally.
- No tests on `--help` text content.
- No tests restating the per-target task count, the join strategy, or
  any other implementation-shape detail.

## Deferred / future work

- **Structured output** — the shared `--output {plain,json,jsonl,wide}`
  enum across `list` and `inspect` (the existing TODOs at
  `cli.rs:126-136,224-226`). Settle the format once and apply
  consistently.
- **`list --filter`** — repeatable `--filter key=value` per the
  existing TODO comment.
- **`--color` actually emitting color** — the global flag is parsed
  but ignored everywhere outside the interactive paths in v0.
- **`logs --since <timestamp>`** — needs a timestamped transcript in
  the daemon, called out in the daemon-binary spec's deferred set.
- **Color/style for `list`/`inspect`** — running vs. exited rows,
  attached-clients highlight, etc.
- **`cairn ps`** as a `list` alias — keep `ls` (already present) and
  evaluate whether `ps` is worth a second alias once people use this.

## Open questions

1. **`kick` idempotency boundary.** This spec treats `session.not_found`
   from a race between resolve and kick as a no-op success. Does the
   same apply to a `kill` that races the session into `exited`? Current
   plan: yes for the signal phase (the session is already gone, so the
   user's intent is satisfied); no for the `wait` phase if it returns a
   transport error (that's a real client problem, not a benign race).
2. **`globset` matching semantics.** Defaulting to case-sensitive,
   `Unicode`-disabled, no negation. If anyone wants `--filter
   '!exited'` semantics it's a separate flag and a separate spec
   (the structured-output / filtering work).
3. **`send` chunk size.** 8 KiB is a reasonable middle ground for the
   stdin-pipe path; if anyone runs into the daemon's input-channel
   backpressure under heavy `send` traffic, this is the knob.

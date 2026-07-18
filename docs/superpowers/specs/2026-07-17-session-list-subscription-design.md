# Session List Subscription (server-push `watch-sessions`)

Replace the web UI's 5-second `list-all` poll with a daemon-push subscription:
one long-lived `watch-sessions` RPC whose stream opens with a full snapshot and
then carries per-session updates as they happen. First half of issue #11; the
second half (multiplexing wRPC invocations over a single WebSocket) is split
into its own ticket and deliberately not addressed here — the subscription is
one persistent connection under today's connection-per-invocation transport,
exactly like `attach` and `logs` already are.

## Background: why wRPC dials per invocation

Investigated as part of this ticket (it shapes what "one subscription" costs).
wRPC's wire format has no invocation ID — frames are `{path, data}` where
`path` indexes into a *single call's* value tree; the connection boundary is
the invocation identity. SPEC.md mandates "a single bidirectional byte stream
per wRPC invocation" (for TCP: "client MUST establish a new connection … per
each invocation"). The design inherits from NATS, where minting a
per-invocation channel is nearly free; on QUIC/WebTransport it is `open_bi()`
on a shared connection; WebSocket is the degenerate case where minting a
channel means dialing. Upstream has no plan to change this (PR #1382 recently
*entrenched* the model by making the framing mandatory). Consequence for this
spec: a subscription costs one held-open WebSocket, which is fine — and it
*replaces* both the 5s poll and the 15s health probe, so steady-state
connection churn drops to zero without any transport work.

## Decisions (with rationale)

| Decision | Choice | Rationale |
|---|---|---|
| Push triggers | Structural changes only: created, renamed, restarted, exited, attach, detach | `cols`/`rows` changes (leader resizes) would be chatty and the list page doesn't show them. The fields stay in `session-info` (CLI `list`/`inspect` and the web detail page display them; tmux's `list-sessions` shows dimensions too) — a pushed event just carries dimensions as-of the last structural change. |
| Event payload | Full `session-info` snapshot per changed session | The UI displays most of the record anyway; fine-grained field diffs are hard to maintain. |
| Stream shape | Snapshot-then-deltas: first event is always the full list | One RPC does everything; no race window between a separate `list-all` and the delta stream; reconnect recovery = resubscribe. Mirrors `attach` (snapshot first, then output). |
| Consumers | Web UI only | CLI keeps one-shot `list`; `cairn list --watch` becomes trivial follow-up once the interface exists. |
| Subscription lifetime | App-lifetime, doubles as the liveness signal | Stream death = disconnected. Replaces the 15s `version()` probe *and* the 5s poll: one persistent WS per tab. |
| Daemon change detection | Dirty-id bus in the registry + per-handle exit watchers | Single choke point (the registry already mediates every structural mutation except exit); future mutation paths can't silently forget to emit; lag→snapshot gives a clean slow-subscriber story. Rejected: level-triggered version counter + per-subscriber diff (O(all sessions) per wake, and structural-only filtering needs field-level diff logic); handler-layer emission (scattered, easy to miss attach-guard drops and exits). |
| Bus payload | `enum RegistryEvent { Changed { id }, Removed { id } }` | Ids, not snapshots: `session_info()` is async but two emission points (`AttachGuard::drop`, `rename`) are sync — the watch handler does the async snapshot-building. An enum (not a bare id struct) so removal is explicit rather than inferred from a failed resolve; future ops (session GC, `close`/`remove` RPC, idle reaping) slot in. |
| WIT name | `watch-sessions` | `watch` alone is too generic. |
| Protocol version | Stays `cairn:daemon@0.1.0` | Purely additive; the daemon serves the web bundle, so skew is transient. |

## Protocol (WIT)

In `interface types`:

```wit
variant session-event {
    snapshot(list<session-info>),
    upsert(session-info),
    removed(session-id),
}
```

In `interface sessions`:

```wit
watch-sessions: func(ctx: option<call-context>) -> stream<session-event>;
```

- The first event on the stream is always `snapshot`.
- `removed` is reserved now even though nothing emits it yet (no
  session-removal op exists): appending a variant case later changes the wire
  format for existing clients, so reserving it up front is cheap insurance.
- `stream<variant>` over the wire is already proven by `attach`'s
  `stream<server-event>`.

Web protocol layer: mirror `session-event` in `wit.ts`/`types.ts`; add
`DaemonClient.watchSessions()` returning an async iterable of typed events,
following the `logs()` pattern (transport held open for the stream's
lifetime, closed when the iterator finishes).

## Daemon: registry event bus

`SessionRegistry` owns a `tokio::sync::broadcast::Sender<RegistryEvent>`
(capacity 64 — it carries only ids, and overflow is recoverable via the
handler's `Lagged` path, so there is no reason to size it generously).

Emission points:

- `create()`, `rename()`, `restart()` — send `Changed` at the end of the
  existing registry methods.
- Attach/detach — `SessionEntry` holds a clone of the sender;
  `SessionEntry::attach()` and `AttachGuard::drop` send `Changed`. Both are
  sync sends (no await), respecting the locking discipline (never hold an
  entry lock across `.await`).
- Exit — new machinery: `create()` and `restart()` spawn a watcher task per
  spawned handle that awaits `handle.wait()` then sends `Changed`. On restart
  the old handle's watcher fires once as the old child dies (harmless,
  coalesced by the handler) and a new watcher is spawned for the new handle.

Send failures (no subscribers) are ignored — broadcast's normal idle state.

## Daemon: `watch-sessions` handler

New handler in `handlers/`, registered like `logs`. Per subscriber:

1. Subscribe to the bus **before** snapshotting, so no change falls in the
   gap between snapshot and first delta.
2. Build the full list via the existing `session_info` fan-out
   (`join_all`, one round-trip latency) and send `snapshot`.
3. Loop: `recv()` one event, then `try_recv()` until empty to drain the
   backlog, dedupe ids — natural coalescing while the previous stream send
   was in flight. For each `Changed` id: resolve → build fresh
   `session_info` → send `upsert`; a resolve miss sends `removed`
   (future-proofing). For `Removed`: send `removed`.
4. On `broadcast::error::RecvError::Lagged`: rebuild and send a full
   `snapshot` — the self-healing path for slow subscribers.

Snapshots are built at send time, so coalesced events always carry fresh
state — no ordering hazards. The handler exits when the stream send fails
(client gone) or on daemon shutdown, following the same drain pattern
`attach`/`logs` use (confirm exact mechanism during planning).

## Web UI

- **`SessionListEngine`** becomes event-applying: a `Map<id, SessionInfo>`
  with `applyEvent(ev)` — `snapshot` replaces all, `upsert` sets, `removed`
  deletes. `loading` is true until the first snapshot. The polled `refresh()`
  and its in-flight coalescing go away.
- **`ReconnectController`** is reworked from probe-based to
  stream-supervision: instead of `probe: () => Promise<void>` on a 15s
  interval, it takes a long-lived `run(onUp: () => void): Promise<void>` —
  establish `watchSessions()`, call `onUp` when the first snapshot arrives
  (status → `connected`), feed events to the engine, return when the stream
  ends or errors (status → `reconnecting`, existing full-jitter backoff,
  retry). Resubscribing re-delivers a snapshot, so reconnect recovery is
  free; `sessions.svelte.ts`'s refresh-on-reconnect listener goes away.
- **Deleted:** the 5s `setInterval` in `+page.svelte`; the `version()` probe
  wiring in `connection.svelte.ts` (the `version` RPC itself stays in the
  protocol); the steady-state re-probe machinery in `reconnect.ts`.
- **Untouched:** `<cairn-terminal>` (builds a bare `DaemonClient`; never used
  the controller — its liveness is the attach stream itself); the CLI; every
  other RPC still dials per call until the muxing ticket.

Steady state per tab: exactly one persistent WebSocket (plus any active
attach), zero churn.

## Error handling and edge cases

- **Client stops reading** (backgrounded tab): the wRPC stream send blocks →
  bus backlog grows → `Lagged` → full snapshot resync when the client
  resumes.
- **Daemon restart:** stream dies → controller backoff → resubscribe → fresh
  snapshot.
- **Endpoint switch** (`forgetEndpoint`): controller stop closes the WS; the
  daemon handler sees the send failure and exits.
- **No subscribers:** bus sends are no-ops; exit-watcher tasks (one per
  running session) are the only standing cost.

## Testing

- **Registry unit tests:** bus emits `Changed` on create/rename/restart and
  on attach-guard drop.
- **Daemon integration (`DaemonHarness`):** subscribe → snapshot; create →
  upsert; rename → upsert with new name; kill → upsert carrying exit status
  (exercises the exit watcher end-to-end); a second subscriber gets its own
  snapshot.
- **Web unit tests:** engine event application; controller state transitions
  with a fake `run`; `protocol.test.ts` round-trip for `watchSessions` over
  the in-process transport (existing pattern).

All tests assert observable behavior through real interfaces (RPC in, events
out) — no shape-only assertions.

## Out of scope

- Multiplexing wRPC invocations over a single WebSocket (split into its own
  ticket; this spec's subscription rides one dedicated connection).
- `cairn list --watch` (follow-up once the interface exists).
- Any emitter for `removed` (reserved in the wire format only).
- Session removal / GC semantics.

## Addendum: rebase onto `cairn-mux-v0` (post-#14)

The mux ticket landed first, so two assumptions above changed when this work
was rebased onto it:

- **Transport.** The subscription no longer costs a dedicated WebSocket: over
  WS endpoints `watchSessions` dials the `control` role (a channel on the
  persistent muxed connection), exactly as the mux spec's follow-ups
  anticipated. Cancellation composes unchanged — the channel transport's
  `close()` RSTs only that channel — and mux-socket death fails the riding
  watch channel in the same event turn, so "stream settled" and "connection
  died" coincide by construction. `attach`/`logs`/`send` keep dedicated
  one-shot sockets, per the mux spec.
- **Probe retirement became probe retuning.** This spec retired the 15s
  `version()` probe outright; the mux spec kept a probe because a *pending*
  stream cannot distinguish "no session changes" from silent path death
  (NAT drop, network switch — invisible to the browser), and a wedged
  establishment would park the controller in `connecting` forever. The
  run-based `ReconnectController` therefore keeps stream supervision as the
  primary signal and adds two backstops: an establishment deadline
  (`upTimeoutMs`, default 10s — the first snapshot must arrive or the attempt
  is declared down) and a steady-state `watchdog` probe (default 30s interval,
  10s timeout — `version()` over the mux) that aborts the run on failure.
  Steady-state RPC traffic on a healthy, quiet connection is one tiny probe
  per 30s instead of the old 5s poll + 15s probe.

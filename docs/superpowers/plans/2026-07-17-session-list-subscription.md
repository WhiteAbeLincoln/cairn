# Session List Subscription Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Spec:** `docs/superpowers/specs/2026-07-17-session-list-subscription-design.md` — read it first; it carries the rationale for every decision below.

**Goal:** Replace the web UI's 5-second `list-all` poll (and the 15s `version()` health probe) with a daemon-push `watch-sessions` subscription: snapshot first, then per-session updates on structural changes.

**Architecture:** A `broadcast` dirty-id bus inside `SessionRegistry` (plus per-handle exit-watcher tasks) feeds a new streaming `watch-sessions` RPC handler that builds fresh `session-info` snapshots per changed id. The web UI's `SessionListEngine` becomes event-applying, and `ReconnectController` switches from periodic probing to supervising the long-lived watch stream.

**Tech Stack:** Rust (tokio, wit-bindgen-wrpc, cargo-nextest), WIT, TypeScript (Svelte 5, vitest, `@bytecodealliance/wrpc`).

## Global Constraints

- No `unwrap`/`expect`/`panic!` in production code (tests exempt). Existing `expect("... lock")` on `std::sync::Mutex`/`RwLock` in `registry.rs` is the established local pattern — match it for lock acquisition only, nothing else.
- Locking discipline (registry.rs module doc): never hold an entry lock across `.await`, never hold two entry locks at once. Bus sends are sync and must happen after locks are released or under the same brief lock scope already present.
- Push triggers are **structural only**: created, renamed, restarted, exited, attach, detach. Resizes must NOT emit.
- Do not remove existing comments; keep comments up to date with changed code.
- Tests assert behavior through real interfaces, never structure. Run Rust tests with `cargo nextest run`, web tests with `npm test` in `cairn-web/`.
- All commits from the worktree root on branch `worktree-feat+session-list-subscription`. Run `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` before each Rust commit; `npx biome check` runs via pre-commit for web files.
- The daemon serves the web bundle; protocol version string stays `cairn:daemon@0.1.0` (additive change).

---

### Task 1: Registry event bus + exit watchers

**Files:**
- Modify: `crates/cairn-daemon/src/registry.rs`

**Interfaces (produced, relied on by Task 2):**

```rust
/// Session-lifecycle notifications. Carries ids, not snapshots: two emission
/// points (`AttachGuard::drop`, `rename`) are sync, and `session_info()` is
/// async — the watch handler resolves ids to fresh snapshots itself.
#[derive(Debug, Clone)]
pub enum RegistryEvent {
    /// The session's `session-info` changed structurally
    /// (created / renamed / restarted / exited / attach / detach).
    Changed { id: String },
    /// Reserved: no emitter yet (no session-removal op exists).
    Removed { id: String },
}

impl SessionRegistry {
    /// Subscribe to session-lifecycle events. Capacity is small (64) —
    /// overflow is recoverable: receivers treat `Lagged` as "resync".
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<RegistryEvent>;
}
```

**Outcome:** Every structural mutation emits `RegistryEvent::Changed` on a `broadcast` channel owned by `SessionRegistry` (capacity 64, created in `SessionRegistry::new()`):

- `create()` — send after the entry is inserted into the map (so a subscriber resolving the id finds it).
- `rename()` — send after `set_name`.
- `restart()` — send after `swap_running`.
- Attach/detach — `SessionEntry` gains a clone of the `broadcast::Sender` (new field, threaded in at entry construction); `SessionEntry::attach()` sends after inserting into the attached map, `AttachGuard::drop` sends after removing. Both are sync sends.
- Exit — `create()` and `restart()` spawn a watcher task per spawned handle:
  clone the `Arc<dyn PtySession>` and sender, `handle.wait().await`, then send
  `Changed`. `PtySession::wait(&self) -> ExitStatus` already exists
  (`crates/cairn-pty/src/session.rs:54`). On restart the old handle's watcher
  fires once as the old child dies (harmless; the handler coalesces) and a new
  watcher is spawned for the new handle. Note: `tokio::spawn` requires an
  ambient runtime — all production callers are RPC handlers (runtime present),
  and registry tests are `#[tokio::test]`.

All sends ignore errors (`let _ = …`) — no subscribers is broadcast's normal idle state. Resize paths must not emit (nothing in the registry handles resize today; just don't add one).

**Acceptance criteria:**

- New unit tests in `registry.rs`'s `mod tests` (behavior, not shape):
  - subscribe → `create` → receive `Changed` with the created id.
  - `rename` and force-`restart` each produce a `Changed`.
  - `SessionEntry::attach` then guard drop produce two `Changed` events.
  - create a short-lived session (`["sh", "-c", "exit 0"]`) → a `Changed`
    arrives after exit without any further registry call (use
    `tokio::time::timeout` around `recv()`, generous e.g. 5s).
- `cargo nextest run -p cairn-daemon` passes; clippy/fmt clean.
- Commit: `feat(daemon): registry session-lifecycle event bus with exit watchers`

---

### Task 2: WIT `watch-sessions` + daemon handler + integration tests

**Files:**
- Modify: `crates/cairn-protocol/wit/cairn.wit`
- Create: `crates/cairn-daemon/src/handlers/watch.rs`
- Modify: `crates/cairn-daemon/src/handlers/mod.rs` (add `pub mod watch;`)
- Modify: `crates/cairn-daemon/src/daemon.rs` (new trait method)
- Test: `crates/cairn-daemon/tests/daemon_streaming.rs` (append a `watch-sessions` section)

**Interfaces:**
- Consumes (Task 1): `SessionRegistry::subscribe_events()`, `RegistryEvent`.
- Produces (wire contract, consumed by Task 3): the WIT below. Case ORDER is the wire format — snapshot, upsert, removed, exactly.

In `interface types` (after `server-event`):

```wit
variant session-event {
    snapshot(list<session-info>),
    upsert(session-info),
    removed(session-id),
}
```

In `interface sessions` (after `list-all`/`inspect`, alongside the other streaming ops), and add `session-event` to the interface's `use types.{...}` list:

```wit
watch-sessions: func(ctx: option<call-context>) -> stream<session-event>;
```

wit-bindgen-wrpc regenerates at build: `cairn_protocol::cairn::daemon::types::SessionEvent` (enum with `Snapshot(Vec<SessionInfo>)`, `Upsert(SessionInfo)`, `Removed(String)`) and a required `watch_sessions` method on the sessions `Handler` trait — the daemon won't compile until implemented (that's the forcing function; the CLI's client world just gains an unused free function, no CLI changes).

**Outcome:** `handlers/watch.rs` exposes (mirroring `logs.rs`'s shape — spawned task + `ReceiverStream`, channel capacity 2):

```rust
pub async fn watch_sessions(
    d: &Daemon,
) -> anyhow::Result<Pin<Box<dyn Stream<Item = Vec<SessionEvent>> + Send + 'static>>>
```

Handler behavior, per the spec (this ordering is load-bearing):

1. `subscribe_events()` **before** building the snapshot — nothing falls in the gap.
2. Build the full list exactly like `handlers/sessions.rs::list_all` (registry `list()` + concurrent `session_info` fan-out) and send `vec![SessionEvent::Snapshot(list)]`.
3. Loop: `recv().await` one event, then `try_recv()` until empty, dedupe ids (`HashSet`). For each `Changed` id: `resolve()` → `session_info().await` → `Upsert`; resolve miss → `Removed` (future-proofing). For `Removed`: emit `Removed`. Send the coalesced group as ONE `Vec<SessionEvent>` batch.
4. `RecvError::Lagged` → rebuild and send a fresh `Snapshot` (resync), continue.
5. Exit the task when the outbound `mpsc` send fails (client gone) or the bus returns `Closed`.

Wire into `daemon.rs` following the `logs` method exactly (tracing span `method = "sessions.watch_sessions"`, `link_remote_context`, delegate).

**Acceptance criteria:**

- Direct handler tests in `daemon_streaming.rs` (same style as the `logs` section there — `test_daemon()`, `create()` helpers, `StreamExt::next` with `tokio::time::timeout`):
  - First item is `Snapshot` containing exactly the pre-existing sessions.
  - After `registry.create(...)`, an `Upsert` with the new session's id arrives.
  - After `rename`, an `Upsert` carries the new name.
  - A session run as `["sh", "-c", "exit 3"]` yields an `Upsert` whose
    `exit` is `Some` with `code == Some(3)` — exercises Task 1's exit watcher
    through the full pipeline.
  - Two concurrent subscribers each receive their own `Snapshot` and both see
    the same subsequent `Upsert`.
- Full `cargo nextest run` passes (proves CLI + protocol still build); clippy/fmt clean.
- Commit: `feat(protocol,daemon): watch-sessions server-push subscription`

---

### Task 3: Web protocol layer — `watchSessions()`

**Files:**
- Modify: `cairn-web/src/lib/protocol/types.ts`, `cairn-web/src/lib/protocol/wit.ts`, `cairn-web/src/lib/protocol/client.ts`
- Check: `cairn-web/src/lib/protocol/index.ts` (or wherever `$lib/protocol` re-exports live) — export `SessionEvent`
- Test: `cairn-web/src/lib/protocol/protocol.test.ts`

**Interfaces:**
- Consumes (Task 2): the `session-event` wire variant, case order snapshot/upsert/removed.
- Produces (consumed by Tasks 4–5):

```ts
// types.ts
/** `types.session-event` — events on a watch-sessions stream. */
export type SessionEvent =
    | { tag: 'snapshot'; val: SessionInfo[] }
    | { tag: 'upsert'; val: SessionInfo }
    | { tag: 'removed'; val: SessionId };

// client.ts — yields individual events (batches flattened)
async *watchSessions(): AsyncIterable<SessionEvent>
```

**Outcome:**

- `wit.ts`: descriptor mirroring the WIT — case order is the wire contract:

```ts
/** `types.session-event`. */
export const sessionEvent: Type = t.variant({
    snapshot: t.list(sessionInfo),
    upsert: sessionInfo,
    removed: t.string,
});
```

- `client.ts`: `watchSessions()` follows the `logs()` pattern (dial, `invoke`
  with `[t.option(wit.callContext)]` params / `[t.stream(wit.sessionEvent)]`
  result, iterate `results[0]`, `closeTransport` in `finally`), flattening each
  wire batch and yielding typed events. No error-wrapping map is needed
  (`session-event` carries no `error` case), so the raw decoded variants cast
  directly — mirror `attach`'s `RawVariant` handling minus `toServerEvent`.
  Like `logs`, await `done` after the stream ends.

**Acceptance criteria:**

- `protocol.test.ts` round-trip in the existing in-process style: a fake
  daemon serves a `snapshot` batch then an `upsert` batch; the client yields
  three typed events in order (snapshot with 2 sessions, then the upsert) and
  the transport closes afterward. Follow the file's existing serve/accept
  helpers.
- `npm test` and `npm run check` pass in `cairn-web/`.
- Commit: `feat(web): watchSessions protocol client`

---

### Task 4: Event-applying engine + stream-supervising controller

**Files:**
- Rewrite: `cairn-web/src/lib/stores/sessionListEngine.ts` + `sessionListEngine.test.ts`
- Rework: `cairn-web/src/lib/stores/reconnect.ts` + `reconnect.test.ts`

**Interfaces:**
- Consumes (Task 3): `SessionEvent`, `SessionInfo` from `$lib/protocol`.
- Produces (consumed by Task 5):

```ts
// sessionListEngine.ts — still framework-free; now also exports a singleton
// so both connection.svelte.ts and sessions.svelte.ts can share it without
// an import cycle (connection must not import sessions.svelte.ts).
export class SessionListEngine {
    get sessions(): SessionInfo[];       // sorted: createdAtUnixMs asc, id tiebreak
    get loading(): boolean;              // true until the first snapshot ever applied
    subscribe(listener: () => void): () => void;
    applyEvent(ev: SessionEvent): void;  // snapshot=replace all, upsert=set, removed=delete
    reset(): void;                       // back to empty + loading (endpoint switch)
}
export const sessionListEngine: SessionListEngine;

// reconnect.ts — probe/steadyIntervalMs replaced by run
export interface ReconnectControllerOptions {
    /** One connection attempt: establish the watch stream, call `onUp()` when
     *  live (first event received), and stay pending while healthy. Returning
     *  OR throwing both mean "down" — the controller schedules a retry. */
    run: (onUp: () => void) => Promise<void>;
    backoff?: BackoffOptions;
    schedule?: (fn: () => void, ms: number) => unknown;
    clearSchedule?: (handle: unknown) => void;
}
```

**Outcome:**

- Engine: internal `Map<SessionId, SessionInfo>`; the polled `refresh()`, its
  `#inflight` coalescing, and the `error` field are deleted (connection-level
  errors are `ReconnectController`'s to report now — check `error`'s consumers
  in Task 5). Sorting is deterministic (`createdAtUnixMs` ascending, id as
  tiebreak) — an improvement over today's arbitrary HashMap order, and stable
  across upserts. `bigint` comparison: `a < b ? -1 : a > b ? 1 : …`.
- Controller: same `ConnectionStatus` shape and listener semantics (Task 5
  depends on `ConnectionIndicator.svelte` continuing to work unmodified).
  `start()` → `connecting` → invoke `run(onUp)`. `onUp` → reset attempt
  counter, status `connected` (transition-only notify, as today). `run`
  settles (resolve or reject) → `reconnecting` with full-jitter backoff →
  scheduled retry. `stop()` cancels the pending timer and suppresses
  transitions from a still-pending `run` (keep the `#stopped` guard pattern).
  `steadyIntervalMs` and the probe loop are gone. Keep `backoffDelay` and its
  tests untouched.

**Acceptance criteria:**

- Engine tests (rewrite `sessionListEngine.test.ts`; the old refresh/coalesce
  tests describe deleted behavior — remove them): snapshot replaces; upsert
  inserts new id and replaces existing id; removed deletes; unknown-id removed
  is a no-op; ordering is by `createdAtUnixMs` regardless of event arrival
  order; `loading` flips false on first snapshot and stays false after
  `reset()`+snapshot cycle… (`reset()` returns `loading` to true until the
  next snapshot); listeners fire per applied event.
- Controller tests (rework the probe-based cases in `reconnect.test.ts`; keep
  the `backoffDelay` cases): `onUp` → `connected`; `run` rejection before
  `onUp` → `reconnecting` with attempt 1; resolve-after-up → `reconnecting`
  then a scheduled retry that calls `run` again; recovery resets the attempt
  counter; `stop()` prevents both retry and status transitions. Use the
  injectable `schedule`/`clearSchedule` seams (no fake timers).
- `npm test` passes.
- Commit: `feat(web): event-applying session engine + stream-supervising reconnect`

---

### Task 5: Wire the stores, delete poll + probe

**Files:**
- Modify: `cairn-web/src/lib/stores/connection.svelte.ts`
- Modify: `cairn-web/src/lib/stores/sessions.svelte.ts`
- Modify: `cairn-web/src/routes/sessions/+page.svelte`
- Check-and-fix consumers: `cairn-web/src/lib/components/SessionList.svelte` (drops the `error` prop if the engine no longer supplies one — connection state already renders via `ConnectionIndicator`), anything else `npm run check` flags.

**Interfaces:**
- Consumes: `sessionListEngine` singleton + reworked `ReconnectController` (Task 4), `client.watchSessions()` (Task 3).

**Outcome:**

- `connection.svelte.ts` `connectWith()` builds the controller with a watch
  `run` in place of the `version()` probe (the `version` RPC itself stays in
  the protocol client for the CLI/debugging):

```ts
const next = new ReconnectController({
    // The watch stream is both the data feed and the liveness signal: its
    // death (resolve or reject) is what "disconnected" means now.
    run: async (onUp) => {
        let live = false;
        for await (const ev of c.watchSessions()) {
            if (!live) {
                live = true;
                onUp();
            }
            sessionListEngine.applyEvent(ev);
        }
    },
});
```

- `forgetEndpoint()` (and `connectWith` on endpoint switch) also call
  `sessionListEngine.reset()` so a stale list never shows under a new daemon.
- `sessions.svelte.ts`: drop `refreshSessions()` and the
  `onConnectionStatusChange` refresh hook; the runes wrapper just mirrors the
  singleton engine's `sessions`/`loading` into `$state` via
  `engine.subscribe`. If nothing else uses `onConnectionStatusChange`
  afterward, remove it from `connection.svelte.ts` too.
- `+page.svelte`: delete the `setInterval` poll and the `refreshSessions`
  import — the page becomes purely declarative.
- `<cairn-terminal>` (`CairnTerminalElement.svelte`) is untouched.

**Acceptance criteria:**

- `npm test`, `npm run check`, and `npm run build` all pass in `cairn-web/`.
- `cargo nextest run` (workspace) still green.
- Manual smoke (documented in the commit/PR body, per the repo's verify
  habit): `cargo run -p cairn-daemon -- --listen ws://127.0.0.1:8080 --web-ui`
  … open the UI, then from a shell `cargo run -p cairn -- run sleep 60` —
  the session appears in the list without a reload; `cargo run -p cairn --
  kill <name>` flips it to exited within a beat; DevTools network tab shows
  ONE persistent `/ws` connection and no 5s/15s churn.
- Commit: `feat(web): drive session list from watch-sessions push`

---

## Self-review notes (already applied)

- Spec coverage: bus + watchers (Task 1), WIT + handler + Lagged resync (Task 2), client (Task 3), engine + controller (Task 4), wiring + deletions + smoke (Task 5). "Out of scope" items from the spec have no tasks, deliberately.
- The spec's `Lagged → snapshot` path has a handler-level implementation (Task 2 step 4) but no automated test — forcing 64 events of backlog deterministically isn't worth the flake risk; noted here rather than silently dropped.
- Type consistency: `RegistryEvent`/`subscribe_events` (Rust), `SessionEvent`/`watchSessions`/`applyEvent`/`run(onUp)` (TS) are each defined once above and referenced identically across tasks.

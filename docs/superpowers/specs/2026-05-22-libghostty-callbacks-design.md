# libghostty Callback Set — Design

Step 1 of the [pty-session "What needs to be built"](../../architecture/pty-session/README.md#what-needs-to-be-built)
list: install the libghostty-vt embedder callbacks needed to (a) gate
backend auto-replies to terminal queries when a client emulator is
attached, (b) brand cairn's XTVERSION identity, and (c) cover the
Tier-2 queries libghostty silently drops by default. Surface the
"primary attached" state to clients via `subscribe()`'s Subscription
RAII guard.

Background: [query-response-delegation.md](../../architecture/pty-session/query-response-delegation.md)
explains the two-emulator problem (headless backend vs. client emulator)
and proposes the counter design. **This spec corrects the tier
classification in that doc** based on an empirical probe of
libghostty-vt 0.1.1 — see § "Empirical libghostty behavior" below.

## Goals

- `fish`, `vim`, `tmux`, and other apps that probe terminal capabilities
  continue to be answered when running inside a cairn session with no
  attached primary client (today already works for Tier-1 queries; add
  Tier-2 coverage for XTWINOPS).
- When a primary client *is* attached (i.e. has an active
  `Subscription`), the backend stops emitting auto-replies so the
  client's emulator can answer with authoritative font/theme/feature
  info.
- XTVERSION identifies cairn rather than the embedded library
  (`libghostty`).
- The mechanism is in place for step 2 (multi-client semantics) to
  attach client identity without restructuring callbacks.

## Non-goals

- Per-client primary/observer distinction (step 2).
- First-attach snapshot suppression (step 2).
- Leader election or resize gating (step 2).
- Snapshot completeness — modes, pwd, keyboard, scrolling region (step 3).
- Timeout-fallback for the "client received query, then detached"
  detach race. Query-response-delegation.md § "Trust the client, or
  timeout-fallback?" recommends ship pure delegation first; revisit
  only if real workloads hit it.
- `on_enquiry` / `on_bell` / `on_title_changed`. Out of scope per the
  step 1 bullet; enquiry is "silence is benign for every modern shell"
  per the open question in query-response-delegation.md.
- Overriding DA1/DA2/DA3 wire bytes via `on_device_attributes`.
  libghostty's defaults already match zmx for DA1 (`\x1b[?62;22c`) and
  are sufficient for DA2/DA3. The gate on `on_pty_write` suppresses
  them when a client is attached without needing the override
  callback. Revisit if a workload requires claiming additional DA
  features.

## Empirical libghostty behavior

Probed `libghostty-vt 0.1.1` directly with a minimal test program
(install only `on_pty_write`, feed each query, observe what bytes the
callback receives). Findings:

| Query | Wire | Library behavior |
|---|---|---|
| DA1 (`\x1b[c`) | `\x1b[?62;22c` | **Auto-reply via `on_pty_write`** |
| DA2 (`\x1b[>c`) | `\x1b[>1;0;0c` | **Auto-reply via `on_pty_write`** |
| DA3 (`\x1b[=c`) | `\x1bP!|00000000\x1b\\` | **Auto-reply via `on_pty_write`** |
| DSR cursor (`\x1b[6n`) | `\x1b[1;1R` | **Auto-reply via `on_pty_write`** |
| DECRQM (`\x1b[?7$p`) | `\x1b[?7;1$y` | **Auto-reply via `on_pty_write`** |
| XTVERSION (`\x1b[>q`) | `\x1bP>\|libghostty\x1b\\` | **Auto-reply via `on_pty_write`** |
| XTWINOPS (`\x1b[14t/16t/18t`) | — | **Silently dropped** without `on_size` |
| Color scheme (`\x1b[?996n`) | — | **Silently dropped** without `on_color_scheme` |
| ENQ (`0x05`) | — | **Silently dropped** without `on_enquiry` |

This contradicts the original
[query-response-delegation.md § "libghostty-vt's callback surface"](../../architecture/pty-session/query-response-delegation.md#libghostty-vts-callback-surface)
classification that puts DA1/DA2/XTVERSION in Tier 2 ("require an
explicit embedder callback or the query is silently dropped"). In
libghostty 0.1.1 they are all Tier 1. The architectural doc should be
updated in a follow-up; this spec proceeds from the empirical reality.

**Consequence:** Gating `on_pty_write` on `primary_count == 0` is by
itself sufficient to suppress every auto-reply (DA1/DA2/DA3/DSR/DECRQM/
XTVERSION) when a primary client is attached. The Tier-2 callbacks
(`on_size`, `on_color_scheme`) close coverage gaps unrelated to the
gate.

## Architecture

One shared counter, three new callbacks (plus the existing
`on_pty_write` with added gating logic), one RAII guard on
Subscription.

### The counter

```rust
let primary_count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
```

`Arc<AtomicUsize>` — not `Rc<Cell<usize>>` as the original architecture
doc suggested — because `Subscription` (which holds the decrement
guard) is `Send` (its `broadcast::Receiver<Bytes>` is `Send`), so any
field it carries must be `Send`. `Rc<Cell<_>>` is `!Send`.

All loads and stores use `Ordering::Relaxed`. The callbacks fire
synchronously inside `vt_write` on the worker thread and only need a
best-effort view of the count. The attach/detach race windows
documented in query-response-delegation.md § "Race conditions on
transition" are accepted as bounded to at most one query straddling
the transition; stronger orderings would not eliminate them.

### Callbacks

| Callback          | `count == 0`                                          | `count >= 1` |
|-------------------|-------------------------------------------------------|--------------|
| `on_pty_write`    | push bytes to `pending_writes` (existing behavior)    | drop         |
| `on_xtversion`    | return `Some("cairn <CARGO_PKG_VERSION>")`            | return `None`|
| `on_size`         | return `Some(SizeReportSize { ... })` from cached size| return `None`|
| `on_color_scheme` | return `None` (no honest backend answer)              | return `None`|

`on_device_attributes` is **not installed**. libghostty's defaults
already match zmx for DA1 and are acceptable for DA2/DA3; the gate on
`on_pty_write` covers query suppression. If a future workload needs to
override the DA wire bytes, install it as a follow-up; the gating
infrastructure is in place.

#### `on_pty_write` — gating only

The current callback (worker.rs:223-228) unconditionally queues bytes
into `pending_writes`. The change adds a single counter check:

```rust
let pending_for_cb = pending_writes.clone();
let pc = primary_count.clone();
terminal.on_pty_write(move |_term, data| {
    if pc.load(Ordering::Relaxed) == 0 {
        pending_for_cb.borrow_mut().push_back(Bytes::copy_from_slice(data));
    }
    // else: drop. The client emulator(s) will answer; if the backend
    // also wrote, the shell would see two replies.
})?;
```

This single gate suppresses libghostty's auto-replies for DA1, DA2,
DA3, DSR cursor, DECRQM, and XTVERSION when a primary is attached.
Deferral to the client works because every PTY-read chunk is also
broadcast to subscribers verbatim (worker.rs:288-290); the client
emulator parses the original query and emits its own reply upstream as
Input.

#### `on_xtversion` — override + gate

```rust
const XTVERSION_REPLY: &str = concat!("cairn ", env!("CARGO_PKG_VERSION"));

let pc = primary_count.clone();
terminal.on_xtversion(move |_term| {
    if pc.load(Ordering::Relaxed) == 0 {
        Some(XTVERSION_REPLY)
    } else {
        None
    }
})?;
```

Returning `Some` overrides libghostty's default `"libghostty"` string;
returning `None` lets the client emulator answer. Neither `"cairn"`
nor `"libghostty"` is recognised by tools that gate behavior on
XTVERSION fingerprints (they look for `"iTerm2"`, `"kitty"`,
`"ghostty"`, etc.) — both fall into the same generic codepath — so
identifying as our own product is the honest choice.

#### `on_size` — close XTWINOPS gap

Reports the current cell grid with synthetic pixel dimensions. The
backend has no font, so pixel dims are placeholders.

```rust
const DEFAULT_CELL_WIDTH_PX: u32 = 10;
const DEFAULT_CELL_HEIGHT_PX: u32 = 20;

let pc = primary_count.clone();
let cs = current_size.clone();
terminal.on_size(move |_term| {
    if pc.load(Ordering::Relaxed) == 0 {
        let size = cs.get();
        Some(SizeReportSize {
            rows: size.rows,
            columns: size.cols,
            cell_width: DEFAULT_CELL_WIDTH_PX,
            cell_height: DEFAULT_CELL_HEIGHT_PX,
        })
    } else {
        None
    }
})?;
```

Non-zero pixel defaults avoid divide-by-zero footguns documented in
the ghostling-rs comments around Kitty graphics placement.
Applications that need real pixel sizes get them from the client
emulator once attached.

#### `on_color_scheme` — explicit silent policy

```rust
terminal.on_color_scheme(|_term| None)?;
```

Always `None`. There is no honest backend answer to "what is the
user's color scheme?" — a wrong guess locks the inferior into the
wrong codepath for the session; silence lets the client (when
attached) provide the answer or shells fall back to a feature probe.
Installing the callback rather than leaving it unset makes the policy
explicit: a future change to "delegate to any attached observer"
lives in one place.

### Subscription RAII guard

`Subscription` keeps its public `snapshot` and `stream` fields and
gains a private guard:

```rust
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
    _primary_guard: PrimaryGuard,
}

pub(crate) struct PrimaryGuard(Arc<AtomicUsize>);

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
```

`PrimaryGuard` is crate-private. Consumers cannot construct or extract
it, which forces decrement-on-drop. The increment happens inside the
worker's `Command::Subscribe` arm immediately before the reply is
sent; the Subscription is observably "primary attached" the moment
the caller receives it.

Public API impact: callers that destructured `Subscription` with
`let Subscription { snapshot, stream } = sub` would break. Two
in-tree call sites need to be updated:

- `crates/cairn-pty/src/lib.rs:92-95`
  (`subscription_constructs_from_parts` test) — constructs a
  Subscription literal.
- `crates/cairn-pty/src/lib.rs:122-126` (`StubSession::subscribe`) —
  constructs a Subscription literal.

These get a `pub(crate) Subscription::new(snapshot, stream, counter)`
constructor that also handles the increment. External callers never
construct a Subscription directly (they receive it from
`PtySession::subscribe`).

### Worker integration

Inside `crates/cairn-pty/src/ghostty/worker.rs::run_session`
(`worker.rs:200-417`):

1. After `pending_writes` (`worker.rs:206`), construct
   `primary_count`.
2. Replace `let mut current_size = s.initial_size;` (`worker.rs:244`)
   with `let current_size: Rc<Cell<TermSize>> = Rc::new(Cell::new(s.initial_size));`.
   - `Command::Size` reads via `current_size.get()`.
   - `Command::Resize` writes via `current_size.set(size)` on the
     success path.
   - `on_size` callback captures a clone, reads on each XTWINOPS query.
   - `Rc<Cell<_>>` is sound here for the same reason as
     `pending_writes`: the LocalSet is single-threaded and no `.await`
     is held across `borrow_mut`/`get`/`set`.
3. Install the four callbacks as a single builder chain on the
   `Terminal` (libghostty-vt 0.1.1 returns `&mut Self` from each
   `on_*`, so the chain ends with one `?`).
4. In the `Command::Subscribe` arm (`worker.rs:355-371`), increment
   `primary_count` and construct a `PrimaryGuard` before sending the
   reply.

### Edge cases (already handled by existing structure)

- **Post-exit Subscribe.** Worker continues serving `Subscribe` after
  child exit (`worker.rs:319`). Step 1 increments the counter on that
  path too; the Subscription's drop decrements an atomic that nobody
  reads. Harmless.
- **Subscriber outlives worker.** The cloned `Arc` keeps the
  allocation alive until the last Subscription drops, at which point
  it frees. No worker-side state to leak.
- **Callback installation failure.** Same fall-through to
  `drain_commands_with_construction_error` as today's `on_pty_write`
  installation at `worker.rs:228-231`.

## Test plan

### Unit tests — `crates/cairn-pty/tests/callback_gating.rs`

Construct a `Terminal` directly (no PTY, no worker), install the
callbacks against hand-rolled `Arc<AtomicUsize>` and
`Rc<RefCell<VecDeque<Bytes>>>`, and assert behavior by feeding query
bytes with `vt_write`. The structure mirrors the libghostty-vt module
example at `terminal.rs:62-107`.

Cases:

- **`on_pty_write` gating — DA1.** Feed `\x1b[c`. Assert
  `pending_writes` receives `\x1b[?62;22c` (libghostty's default) when
  count == 0. Increment counter, feed again; assert nothing appears.
- **`on_pty_write` gating — DECRQM.** Feed `\x1b[?7$p`. Assert reply
  `\x1b[?7;1$y` arrives when count == 0; nothing when count >= 1.
  Same shape for DSR cursor (`\x1b[6n` → `\x1b[1;1R`).
- **`on_xtversion` override + gate.** Feed `\x1b[>q`. Assert reply
  appears in `pending_writes` and its byte payload contains `cairn `
  followed by `env!("CARGO_PKG_VERSION")` (not the default
  `libghostty`) when count == 0; nothing when count >= 1. The exact
  DCS framing (`\x1bP>|...\x1b\\`) is libghostty's responsibility;
  the test asserts on the payload contents, not the wrapper.
- **`on_size` — close gap.** Feed `\x1b[18t` (text area in chars).
  Assert reply `\x1b[8;<rows>;<cols>t` appears with the fake
  `current_size` when count == 0; nothing when count >= 1.
- **`on_color_scheme` — always None.** Feed `\x1b[?996n` with count
  at 0 and at 1; assert nothing in `pending_writes` either way (the
  callback returns None, so libghostty emits no reply via its own
  internal path either).

Each test increments/decrements the atomic by hand and re-feeds the
query. Fast, deterministic, no PTY required.

### Integration test — extend `crates/cairn-pty/tests/pty_io.rs`

The existing `da1_query_gets_response_without_client` test
(`tests/pty_io.rs:161-194`) already exercises the count == 0 path
end-to-end: it spawns a shell script that issues DA1 and `read`s the
reply; the test asserts a non-zero reply length. Today this passes
because libghostty's defaults reply via `on_pty_write` and the test
subscribes *after* spawn (the query and reply complete before the
subscribe lands, while count == 0). After step 1, the same race
holds, but the assertion still passes.

Add a complementary test `da1_query_suppressed_when_client_attached`
that exercises the count >= 1 path:

1. Spawn `GhosttyPty` with a child script: `printf '\033[c'; read -r -n 32 -t 1 reply; printf 'reply-len=%d\n' "${#reply}"`.
2. **Subscribe before any input flows.** The cleanest way is to write
   the child as `sleep 0.5; printf '\033[c'; read ...` so the
   subscribe call wins the race. The first 0.5s delay lets the test
   subscribe and ensures `primary_count == 1` when the script issues
   the query.
3. Assert the stream eventually contains `reply-len=0` — meaning
   the script did *not* receive a backend reply, because the gate
   suppressed it.

This complements the existing test without rewriting it. Together
they verify both branches of the gate end-to-end.

## Open questions deferred to step 2

- When the *last* primary detaches, should the gate reset, or stay
  sticky like zmx? (query-response-delegation.md open question #2.)
  Today's design *does* reset (the counter goes back to 0), which is
  the more permissive behavior; if step 2 measurements show this
  causes re-probing thrash, switch to sticky.
- Per-query-class gating: should `on_color_scheme` delegate to any
  attached observer (not just primary)? (query-response-delegation.md
  open question #1.) Currently moot — we return `None` regardless.
- Timeout-fallback for the detach race (query-response-delegation.md
  § "Trust the client, or timeout-fallback?"). Ship pure delegation
  first; instrument and revisit.
- Updating the architecture doc's Tier 1 / Tier 2 classification to
  match the empirical findings in this spec.

## Out-of-scope cleanup considered and rejected

- Generalising `pending_writes` and `primary_count` into a single
  `Callbacks` struct. Premature — step 2 will add more callback state
  (client identity, leader id) and the right shape isn't visible yet.
- Replacing `format_snapshot` with the full `FormatterTerminalExtra`
  field set. That's step 3.
- Adding observability counters for delegation decisions
  (query-response-delegation.md § "Cross-references → observability").
  Belongs with the broader observability work item #9 in the README.
- Installing `on_device_attributes`. libghostty's DA defaults are
  acceptable and the gate suppresses them on client attach. Adding
  the override callback can wait until a workload requires claiming
  specific DA features.

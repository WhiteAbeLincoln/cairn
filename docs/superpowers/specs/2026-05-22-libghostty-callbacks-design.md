# libghostty Callback Set — Design

Step 1 of the [pty-session "What needs to be built"](../../architecture/pty-session/README.md#what-needs-to-be-built)
list: wire the four missing libghostty-vt embedder callbacks
(`on_device_attributes`, `on_xtversion`, `on_color_scheme`, `on_size`),
gate them along with the existing `on_pty_write` on a primary-attached
counter, and surface that counter to clients via `subscribe()`'s
Subscription RAII guard.

Background: [query-response-delegation.md](../../architecture/pty-session/query-response-delegation.md)
explains the two-emulator problem (headless backend vs. client emulator)
and proposes the counter design we adopt here.

## Goals

- `fish`, `vim`, `tmux`, and other apps that probe terminal capabilities
  no longer hang when running inside a cairn session with no attached
  primary client.
- When a primary client *is* attached (i.e. has an active
  `Subscription`), the backend stops answering Tier-2 queries so the
  client's emulator can answer with authoritative font/theme/feature
  info.
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

## Architecture

One shared counter, five callbacks, one RAII guard on Subscription.

### The counter

```rust
let primary_count: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
```

`Arc<AtomicUsize>` — not `Rc<Cell<usize>>` as the doc originally
suggested — because `Subscription` (which holds the decrement guard) is
`Send` (its `broadcast::Receiver<Bytes>` is `Send`), so any field it
carries must be `Send`. `Rc<Cell<_>>` is `!Send`.

All loads and stores use `Ordering::Relaxed`. The callbacks fire
synchronously inside `vt_write` on the worker thread and only need a
best-effort view of the count. The attach/detach race windows
documented in query-response-delegation.md § "Race conditions on
transition" are accepted as bounded to at most one query straddling the
transition; stronger orderings would not eliminate them.

### The five callbacks

All installed during worker startup as a single builder chain on the
`Terminal`. Each captures a clone of `primary_count`; `on_pty_write`
additionally captures `pending_writes`; `on_size` additionally captures
the new shared `current_size`.

| Callback              | When `count == 0`                                     | When `count >= 1` |
|-----------------------|-------------------------------------------------------|-------------------|
| `on_pty_write`        | push bytes to `pending_writes` (existing behavior)    | drop              |
| `on_device_attributes`| return `Some(DeviceAttributes { ... })` (see below)   | return `None`     |
| `on_xtversion`        | return `Some("cairn <CARGO_PKG_VERSION>")`            | return `None`     |
| `on_size`             | return `Some(SizeReportSize { ... })` from cached size| return `None`     |
| `on_color_scheme`     | return `None` (no honest backend answer)              | return `None`     |

#### DA1 / DA2 / DA3 — `on_device_attributes`

Matches the upstream libghostty-vt
[`ghostling-rs` example](https://github.com/Uzaaft/libghostty-rs/blob/master/example/ghostling_rs/src/main.rs)
verbatim:

```rust
DeviceAttributes {
    primary: PrimaryDeviceAttributes::new(
        ConformanceLevel::VT220,
        [
            DeviceAttributeFeature::COLUMNS_132,
            DeviceAttributeFeature::SELECTIVE_ERASE,
            DeviceAttributeFeature::ANSI_COLOR,
        ],
    ),
    secondary: SecondaryDeviceAttributes {
        device_type: DeviceType::VT220,
        firmware_version: 1,
        rom_cartridge: 0,
    },
    tertiary: TertiaryDeviceAttributes::default(),
}
```

Wire form for DA1: `\x1b[?62;1;6;22c`. zmx uses `\x1b[?62;22c` (just
ANSI_COLOR); we claim two more features that libghostty's VT220
emulation actually supports. Wire form for DA2: `\x1b[>1;1;0c` (zmx
uses fw=10; either is arbitrary).

Cost of changing: trivial — change the feature array or the firmware
version constant. The values are policy and revisitable.

#### `on_xtversion`

```rust
const XTVERSION: &str = concat!("cairn ", env!("CARGO_PKG_VERSION"));
move |_term| {
    if primary_count.load(Ordering::Relaxed) == 0 {
        Some(XTVERSION)
    } else {
        None
    }
}
```

zmx does not install this callback (its inferior queries silently drop
into libghostty's unhandled Tier-2 path). `"cairn 0.1.0"` is a free
choice; the version updates automatically with the crate.

#### `on_size`

Reports the current cell grid with synthetic pixel dimensions:

```rust
const DEFAULT_CELL_WIDTH_PX: u32 = 10;
const DEFAULT_CELL_HEIGHT_PX: u32 = 20;

move |_term| {
    if primary_count.load(Ordering::Relaxed) == 0 {
        let size = current_size.get();
        Some(SizeReportSize {
            rows: size.rows,
            columns: size.cols,
            cell_width: DEFAULT_CELL_WIDTH_PX,
            cell_height: DEFAULT_CELL_HEIGHT_PX,
        })
    } else {
        None
    }
}
```

The pixel dimensions are placeholders — the backend has no font and no
authoritative cell metrics. Applications that need real pixel sizes
(image-aware shells, sixel users) will get them from the client's
emulator once attached. Non-zero defaults avoid the divide-by-zero
footguns documented in query-response-delegation.md and in the
ghostling-rs source comments around Kitty graphics placement.

#### `on_color_scheme`

```rust
move |_term| -> Option<ColorScheme> { None }
```

Returned `None` regardless of `primary_count` because there is no
honest backend answer to "what is the user's color scheme?". A wrong
guess locks the inferior into the wrong code path for the session;
silence lets shells fall back to a feature probe. Installing the
callback (rather than leaving it unset) is explicit policy: a future
change to "delegate to any attached observer" lives in one place.

### Subscription RAII guard

`Subscription` keeps its public `snapshot` and `stream` fields and
gains a private guard:

```rust
pub struct Subscription {
    pub snapshot: Bytes,
    pub stream: broadcast::Receiver<Bytes>,
    _primary_guard: PrimaryGuard,
}

struct PrimaryGuard(Arc<AtomicUsize>);

impl Drop for PrimaryGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
```

`PrimaryGuard` is crate-private. Consumers cannot construct or extract
it, which forces decrement-on-drop. The increment happens inside the
worker's `Command::Subscribe` arm immediately before the reply is sent;
the Subscription is observably "primary attached" the moment the
caller receives it.

Public API impact: callers that destructured `Subscription` with
`let Subscription { snapshot, stream } = sub` would break. No current
in-tree call site does this; only the in-tree example uses the type
and it accesses fields by name. Documenting the change is sufficient.

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
     is held across `borrow_mut`.
3. Install the five callbacks as a single builder chain on the
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

- **`on_pty_write` — gated.** Feed a DECRQM query (`\x1b[?7$p`,
  matching the libghostty example). Assert bytes appear in
  `pending_writes` when count == 0; assert nothing appears when count
  >= 1.
- **`on_device_attributes` — gated.** Feed `\x1b[c` (DA1). Assert
  exact wire reply `\x1b[?62;1;6;22c` appears in `pending_writes` when
  count == 0; nothing when count >= 1. Same for DA2 (`\x1b[>c` →
  `\x1b[>1;1;0c`) and DA3 (`\x1b[=c`).
- **`on_xtversion` — gated.** Feed `\x1b[>q`. Assert the reply
  appears in `pending_writes` and contains the substring `cairn `
  followed by the crate version (via `env!("CARGO_PKG_VERSION")`)
  when count == 0; nothing when count >= 1. The exact DCS framing
  (`\x1bP>|...\x1b\\`) is libghostty's responsibility; the test
  asserts on the payload contents, not the wrapper.
- **`on_size` — gated.** Feed `\x1b[18t`. Assert reply
  `\x1b[8;<rows>;<cols>t` with the fake size when count == 0; nothing
  when count >= 1.
- **`on_color_scheme` — always None.** Feed `\x1b[?996n` with count
  at 0 and at 1; assert nothing in `pending_writes` either way.

Each test increments/decrements the atomic by hand and re-feeds the
query. Fast, deterministic, no PTY required.

### Integration test — `crates/cairn-pty/tests/pty_callbacks.rs`

One end-to-end test verifies that the counter wiring is live and the
gate flips correctly through the full worker:

1. Spawn `GhosttyPty` with `cat` as the child. `cat` echoes stdin to
   stdout, giving a roundtrip path for any bytes the worker writes
   to the PTY.
2. Subscribe (count goes 0 → 1). Keep the Subscription as the
   observer.
3. Send `\x1b[c` (DA1) via `pty.write(...)`. The query reaches
   `cat`'s stdin, `cat` echoes it to stdout, the worker reads it
   and feeds it to `vt_write`; `on_device_attributes` fires and —
   because count == 1 — returns `None`. The flush path therefore
   has nothing to write.
4. Assert the subscriber stream yields exactly the echoed query
   bytes (`\x1b[c`) and nothing else. In particular, the canned
   reply `\x1b[?62;1;6;22c` must not appear.
5. Drop the Subscription (count goes 1 → 0). Subscribe again; assert
   the second subscribe still succeeds (counter accounting is sound
   across attach cycles).

This test does not assert the count == 0 path produces a canned
reply at the worker level. That path is fully exercised by the unit
tests at the Terminal level; the worker just chains the same
callbacks. End-to-end verification of "no subscriber attached →
canned reply reaches the inferior" requires either time-based
synchronisation (write the query, sleep, subscribe, read scrollback
from the snapshot) or a non-primary observer attachment that step 1
deliberately does not introduce. Step 2 adds that observer mode and
takes over the assertion.

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

## Out-of-scope cleanup considered and rejected

- Generalising `pending_writes` and `primary_count` into a single
  `Callbacks` struct. Premature — step 2 will add more callback state
  (client identity, leader id) and the right shape isn't visible yet.
- Replacing `format_snapshot` with the full `FormatterTerminalExtra`
  field set. That's step 3.
- Adding observability counters for delegation decisions
  (query-response-delegation.md § "Cross-references → observability").
  Belongs with the broader observability work item #9 in the README.

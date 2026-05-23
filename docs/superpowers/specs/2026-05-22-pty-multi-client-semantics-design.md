# Multi-Client Semantics — Design

Step 2 of the [pty-session "What needs to be built"](../../architecture/pty-session/README.md#what-needs-to-be-built)
list. Extends `cairn-pty` so the worker can coordinate multiple clients
sharing a single PTY session: per-connection identity, leader election
by most-recent user input, and leader-only resize.

Background: [client-attach-and-election.md](../../architecture/pty-session/client-attach-and-election.md)
documents zmx's reference behavior and the proposed adaptation for
cairn. This spec specifies the concrete `cairn-pty` API and worker
changes that implement it.

## Goals

- Multiple clients can attach to one PTY session and share its output
  via `subscribe()`.
- Any client can write input; the most recent client to send bytes that
  pass the user-input classifier becomes the *leader*.
- Only the leader's `resize()` calls are honored; non-leader resize
  calls fail with a typed error.
- An empty leader seat can be claimed by either the first input *or*
  the first resize from any client.
- The leader seat is vacated when the leader's `Subscription` drops.
  The next qualifying input promotes a new leader.
- Terminal back-chatter (mouse motion, focus, query replies) does not
  promote anyone — only intentional human interaction does.

## Non-goals

- **First-attach snapshot suppression.** zmx suppresses the snapshot
  on the very first attach because in zmx the spawning client is the
  first attacher; shell startup is in flight and a snapshot would
  clobber bytes the client is about to render live. Cairn's primary
  use case inverts that assumption — processes are spawned headlessly
  and may run for hours before any client looks at them. Every
  `subscribe()` returns the full snapshot. See the divergence note in
  [README.md](../../architecture/pty-session/README.md) and the
  proposed adaptation in
  [client-attach-and-election.md](../../architecture/pty-session/client-attach-and-election.md).
- **Leader handoff to most-recent non-leader on detach.** The seat is
  vacated, not handed off. Open question 3 in
  client-attach-and-election.md; defer until a workload demands it.
- **Stuck-leader recovery via heartbeat.** Half-open WebSocket leaders
  blocking everyone else's resizes is a transport/daemon concern (open
  question 7, step 8); not solved here.
- **Cross-reconnect identity preservation.** A WebSocket reconnect is
  a fresh `ClientId` from the library's perspective. Sticky identity
  across reconnect is a daemon-layer concern (auth token, session
  cookie), not a library concern.
- **Snapshot completeness.** Porting zmx's two-phase snapshot with
  full `FormatterTerminalExtra` is step 3.
- **Daemon binary, wire protocol, authentication, CLI client.** Steps
  4-7. This spec is library-only.

## Architecture: headless Terminal stays internal to the worker

zmx's daemon owns a `term` that participates in the same dispatch
loop as real client sockets — one unified collection of attached
endpoints, the internal `term` exempt from leader election by
identity. The proposed-adaptation section in
[client-attach-and-election.md](../../architecture/pty-session/client-attach-and-election.md)
originally suggested mirroring this in cairn: give the libghostty
`Terminal` a stable `ClientId`, route its writes through the same
command channel as real clients, mark it non-electing.

This spec **does not** adopt that shape. Instead the headless
`Terminal` stays where it is today — a private
`Rc<RefCell<Terminal>>` owned by the worker thread, fed PTY output
via `terminal.vt_write` inside the `select!` arm, emitting query
responses through `on_pty_write` into `pending_writes` for the
worker to drain in the same iteration.

Reasoning:

1. **Snapshot atomicity is structural, not negotiated.** The
   `PtySession::subscribe` contract requires "no gap, no overlap"
   between the snapshot and the live stream. With the Terminal
   inside the worker, the Subscribe handler grabs
   `terminal.borrow().format_snapshot()` and creates a
   `broadcast::Receiver` in the same `select!` arm — before any
   further PTY byte is read. Treating the Terminal as a client
   would require a feedback round-trip (Subscribe → worker →
   observer → snapshot → reply), opening a window where PTY bytes
   could arrive between snapshot generation and stream
   subscription.
2. **No identity is needed for state-tracking that has no external
   API.** The Terminal doesn't get `Subscribe`d to, never sends
   `Write` commands, never resizes itself. Modeling it as a client
   would be naming-for-uniformity, not a real boundary.
3. **`!Send + !Sync` pins everything to one thread anyway.**
   Splitting the Terminal into a separate "observer" doesn't enable
   any threading flexibility; the observer would have to live on
   the same LocalSet as the worker.
4. **"Is any real client attached" is already a clean signal.** The
   `primary_count: Arc<AtomicUsize>` (incremented in
   `Command::Subscribe`, decremented by `SubscriptionGuard::drop`)
   already gates the headless Terminal's auto-replies in
   `on_pty_write`, `on_xtversion`, and `on_size`. No synthetic
   local-client identity is needed to drive this.
5. **YAGNI.** A second internal observer (log capture, telemetry)
   would justify the observer abstraction. We have one, and no
   concrete proposal for more. When/if that lands, the
   architecture can be revisited with a real driver.

Consequence: there is no `ClientId::LOCAL_BACKEND` reserved id.
`ClientId::from_u64` is infallible — it adds 1 internally to keep
the underlying `NonZeroU64` non-zero — so daemons can start their
counter at 0 with no convention to remember. If a future spec moves
the Terminal out of the worker (or introduces another internal
observer), that spec can introduce a reservation and bump the
offset at that point.

## Public API

### New type: `ClientId`

`crates/cairn-pty/src/client_id.rs` (new file, re-exported from
`lib.rs`):

```rust
use std::fmt;
use std::num::NonZeroU64;

#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ClientId(NonZeroU64);

impl ClientId {
    /// Construct a `ClientId` from a daemon counter value.
    ///
    /// The library adds 1 internally so the underlying `NonZeroU64`
    /// is never zero. The daemon may start its counter at 0; the
    /// returned id is opaque.
    ///
    /// # Panics
    ///
    /// Panics if `value == u64::MAX`. At 1M attaches per second this
    /// would take ~584,500 years; in debug builds Rust's overflow
    /// check fires at the `+ 1`, in release builds the `NonZeroU64`
    /// invariant fires on the wrapped result. Both are the desired
    /// behavior — reaching this case means something has gone
    /// catastrophically wrong upstream.
    pub fn from_u64(value: u64) -> Self {
        ClientId(
            NonZeroU64::new(value + 1)
                .expect("ClientId from u64::MAX is unsupported"),
        )
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

Notes:

- Inner field is private. `ClientId` is opaque from outside the crate.
- `Copy + Eq + Hash` because the worker's only operation is equality
  comparison against `leader`, and the daemon will key tables by
  `ClientId`.
- No reserved internal ids. The headless `Terminal` in the worker is
  not modeled as a client (see the "Architecture" section below), so
  there's nothing internal that needs a stable id.
- Daemons that need to round-trip a displayed value back into a
  `ClientId` (e.g., future `cairn kill <id>` CLI) will use the same
  `from_u64` since `Display` shows the underlying `NonZeroU64` value
  directly. No separate constructor needed.

### Trait change: `PtySession`

`crates/cairn-pty/src/session.rs`:

```rust
#[async_trait::async_trait]
pub trait PtySession: Send + Sync {
    async fn size(&self) -> Result<TermSize, PtyError>;

    /// Resize the terminal grid. Only honored when `client_id` is the
    /// current leader. Returns `PtyError::NotLeader` otherwise. A
    /// resize from any client promotes them to leader if the seat is
    /// empty.
    async fn resize(&self, client_id: ClientId, size: TermSize) -> Result<(), PtyError>;

    /// Subscribe to terminal output. Returns a snapshot of current
    /// terminal state (the accumulated screen + scrollback) atomically
    /// with the start of the live byte stream. Subscribing does not
    /// claim leadership; only `write` or `resize` calls can promote.
    async fn subscribe(&self, client_id: ClientId) -> Result<Subscription, PtyError>;

    /// Write bytes to the PTY. Bytes that pass the user-input
    /// classifier promote `client_id` to leader if it isn't already.
    async fn write(&self, client_id: ClientId, data: Bytes) -> Result<(), PtyError>;
}
```

Existing callers (tests, examples, the `StubSession` in `lib.rs`)
update to pass `ClientId::from_u64(0)` (or any value) at every call
site. About 15-20 line edits.

### New error variant: `PtyError::NotLeader`

`crates/cairn-pty/src/error.rs`:

```rust
#[error("resize rejected: client {requester} is not the leader (current: {current:?})")]
NotLeader {
    requester: ClientId,
    current: Option<ClientId>,
},
```

`current` is `Option` because the type honestly mirrors the worker's
`leader: Option<ClientId>` field shape. In practice, when this error
is returned, `current` is always `Some` (the empty-seat case promotes
instead of rejecting).

## Worker state and command shape

### `Command` enum changes

`crates/cairn-pty/src/ghostty/mod.rs`:

```rust
pub(super) enum Command {
    Subscribe {
        client_id: ClientId,
        reply: oneshot::Sender<Result<Subscription, PtyError>>,
    },
    Resize {
        client_id: ClientId,
        size: TermSize,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Size {
        reply: oneshot::Sender<Result<TermSize, PtyError>>,
    },
    Write {
        client_id: ClientId,
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    /// Sent by `SubscriptionGuard::drop`. Worker checks if `client_id`
    /// is the current leader and clears the seat if so.
    Detach {
        client_id: ClientId,
    },
    Shutdown,
}
```

`Detach` is new — the mechanism by which `Subscription` drop
propagates into the worker without holding a lock on `leader`.

### Loop-local state additions in `run_session`

```rust
// Pre-existing locals: pending_writes, primary_count, terminal,
// current_size, bcast_tx, buf, exit_published, pty_closed.

// NEW:
let mut leader: Option<ClientId> = None;
let mut last_input_at: Option<std::time::Instant> = None;
```

Plain locals — not behind `Rc<RefCell<>>` because they are only
mutated from `select!` arm bodies, all of which run on the same
LocalSet task.

### `SubscriptionGuard` (replaces `PrimaryGuard`)

`crates/cairn-pty/src/subscription.rs`:

```rust
pub(crate) struct SubscriptionGuard {
    client_id: ClientId,
    primary_count: Arc<AtomicUsize>,
    cmd_tx: flume::Sender<Command>,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        self.primary_count.fetch_sub(1, Ordering::Relaxed);
        // Best-effort detach. If cmd_tx is closed, the worker has
        // already shut down and there's no leader state to clear.
        let _ = self.cmd_tx.send(Command::Detach {
            client_id: self.client_id,
        });
    }
}
```

`Subscription` holds a `SubscriptionGuard` in place of the existing
`PrimaryGuard`. The constructor takes `client_id` and a clone of
`cmd_tx` alongside the existing snapshot, stream, and primary count.

### Worker plumbing for the detach channel

`worker::spawn` and `worker::spawn_with` keep their own clone of
`cmd_tx` and pass it into `run_session` so it can hand out
`SubscriptionGuard`s on each `Command::Subscribe`. flume is FIFO
unbounded, so a `Detach` following its `Subscribe` is delivered in
order. No deadlock risk: drop sends a non-blocking message; the worker
processes it on the next `select!` iteration.

## Election rules

State: `leader: Option<ClientId>`, `last_input_at: Option<Instant>`.

```
Subscribe { client_id }       — no effect on leader
Detach { client_id }          — if leader == Some(client_id): clear
Size                          — no effect on leader
Shutdown                      — terminal

Write { client_id, data }:
    is_user = is_user_input(&data)
    if is_user:
        last_input_at = Some(Instant::now())
        if leader != Some(client_id):
            previous = leader
            leader = Some(client_id)
            tracing::info!(
                target: "cairn_pty::election",
                client_id = %client_id,
                cause = "input",
                previous = ?previous,
                "leader promoted"
            )
    pty.write_all(&data).await; reply

Resize { client_id, size }:
    match leader:
        None:
            leader = Some(client_id)
            tracing::info!(
                target: "cairn_pty::election",
                client_id = %client_id,
                cause = "resize",
                previous = ?None::<ClientId>,
                "leader promoted"
            )
            apply
        Some(id) if id == client_id:
            apply
        Some(other):
            reply Err(NotLeader { requester: client_id, current: Some(other) })

    apply ≡ terminal.resize(...) + pty.set_size(...) + current_size.set(...)
```

On `Detach { client_id }` where `leader == Some(client_id)`:

```rust
tracing::info!(
    target: "cairn_pty::election",
    client_id = %client_id,
    "leader vacated",
);
leader = None;
```

### Key properties

1. **No debounce on input-based promotion.** First qualifying byte
   from any non-leader switches the seat atomically. Matches zmx.
2. **Promotion is silent to the demoted client.** They learn they're
   no longer leader on their next `resize` call. The daemon layer can
   surface this to clients via the wire protocol later.
3. **`Subscribe` never promotes.** A read-only viewer never claims
   leadership through subscription alone.
4. **`Resize` while seat is empty always promotes,** regardless of
   byte content. Resizes are unambiguous user actions.
5. **Subscription drop is the only way to vacate.** No idle timeout.
6. **`last_input_at`** is updated on every input-classified write,
   leader or not. Useful for tracing/debug; doesn't gate behavior in
   v0.

## `is_user_input` classifier

`crates/cairn-pty/src/ghostty/input_classifier.rs` (new file, private
to the `ghostty` submodule). Add `vte` to `cairn-pty/Cargo.toml`
(pinned to a recent stable version at implementation time).

### API

```rust
/// Classify a write payload as "user input" or "terminal back-chatter."
///
/// Used by the worker to decide whether a write from a non-leader
/// client should promote that client to leader. The classifier
/// mirrors zmx's `util.isUserInput` (`zmx/src/util.zig:446-477`)
/// with one deliberate divergence: mouse press/release/scroll/drag
/// events DO qualify as user input. See "Divergences" below.
pub(crate) fn is_user_input(data: &[u8]) -> bool;
```

### Qualifying inputs (returns `true`)

- Printable ASCII: `0x20..=0x7E`.
- Keyboard control bytes: `\r` (0x0D), `\n` (0x0A), `\t` (0x09),
  `\x08` (Backspace), `\x7F` (DEL).
- Kitty keyboard protocol sequences: `ESC [ ... u`.
- Legacy modified-key sequences: `ESC [ 1 ; <mod> <final>` where
  `final ∈ { A B C D F H P Q R S }` and `<mod> >= 2`.
- SGR mouse, press / release / scroll / drag:
  `ESC [ < <button> ; <col> ; <row> M|m` where either
  `(button & 32) == 0` (no motion) or `(button & 32) != 0 &&
  has_button` (drag).
- X10 mouse: `ESC [ M <b> <c> <r>`. (Mode 1000 doesn't track
  motion; we accept any X10 byte. X10 dispatch shape under `vte`
  requires verification at implementation time — see "Open at
  implementation time" below.)

### Non-qualifying (returns `false`)

- SGR mouse motion-only: `(button & 32) != 0 && !has_button`.
- Focus in/out: `ESC [ I`, `ESC [ O`.
- DA1 / DA2 / DA3 responses: `ESC [ ? <params> c`, `ESC [ > <params> c`,
  DCS form for DA3.
- DSR cursor reports: `ESC [ <row> ; <col> R`.
- DECRQM mode reports: `ESC [ ? <mode> ; <value> $ y`.
- Any other CSI / DCS / OSC sequence not in the qualifying list.

### Disjunctive over the payload

If the payload contains *any* qualifying byte or sequence, the whole
payload is user input. Matches zmx and is robust to clients that
batch keystrokes.

### Edge cases

- Empty payload returns `false`. The worker still applies the
  (zero-byte) write to the PTY for symmetry; no promotion happens.
- A truncated escape sequence at end of payload is non-qualifying
  (the `vte` state machine waits for the final byte). In practice
  WebSocket framing keeps a keystroke in one frame; the cost is one
  extra keypress to claim leadership in the rare split case.
- Non-printable, non-keyboard control bytes without escape framing
  (`\x01..=\x07`) do not qualify.

### Implementation: `vte`-driven `Perform`

```rust
use vte::{Params, Parser, Perform};

#[derive(Default)]
struct Classifier { found: bool }

impl Perform for Classifier {
    fn print(&mut self, _c: char) { self.found = true; }

    fn execute(&mut self, byte: u8) {
        if matches!(byte, 0x08 | 0x09 | 0x0A | 0x0D | 0x7F) {
            self.found = true;
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        match (intermediates, action) {
            (b"", 'I') | (b"", 'O') => {} // focus
            (b"<", 'M') | (b"<", 'm') => {
                let button = params.iter().next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let motion = button & 32 != 0;
                let has_button = (button & 0b11) != 0b11 || (button & 64) != 0;
                if !motion || has_button {
                    self.found = true;
                }
            }
            (b"", 'u') => self.found = true,
            (b"", a) if matches!(a, 'A'|'B'|'C'|'D'|'F'|'H'|'P'|'Q'|'R'|'S') => {
                let first = params.iter().next().and_then(|p| p.first().copied());
                let mod_param = params.iter().nth(1).and_then(|p| p.first().copied());
                if first == Some(1) && mod_param.is_some_and(|m| m >= 2) {
                    self.found = true;
                }
            }
            _ => {} // DA, DSR, DECRQM, etc.
        }
    }

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell: bool) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

pub(crate) fn is_user_input(data: &[u8]) -> bool {
    let mut classifier = Classifier::default();
    let mut parser = Parser::new();
    for &b in data { parser.advance(&mut classifier, b); }
    classifier.found
}
```

~40 lines including the trait impl. `vte` handles the state machine;
we own the classification policy.

### Why not libghostty's parser

libghostty 0.1.1's Rust bindings expose only `osc::Parser` (OSC
sequences) and the encoders (`key`, `mouse`, `focus`). The full VT
state machine lives in `terminal/Parser.zig` and is not exposed via
the C FFI. zmx uses it directly because zmx is Zig and links the
full Zig API. From Rust today, `vte` is the canonical alternative
(used by alacritty, ratatui consumers, etc.).

### Open at implementation time

- X10 mouse (`\x1b[M\x20\x20\x20`) dispatch shape under `vte`: most
  versions emit `csi_dispatch` with action `'M'` and no
  intermediates, with the three following bytes consumed as
  parser-internal state. Verify with a unit test before relying on
  the match arm; if the dispatch shape differs, add a small
  pre-filter that detects the `\x1b[M` prefix.
- Exact `vte` version pin: pick the most recent stable at the time
  the spec is implemented; the API used here is stable across
  recent major versions.

## Testing

### Layer 1: `is_user_input` unit tests

Inline `#[cfg(test)] mod tests` in `input_classifier.rs`. Pinned
cases:

```rust
b""                  => false,
b"a"                 => true,
b"\r"                => true,
b"\x08"              => true,  // backspace
b"\x1b[1;5A"         => true,  // Ctrl-Up
b"\x1b[97;5u"        => true,  // kitty Ctrl-a
b"\x1b[c"            => false, // DA1 query
b"\x1b[?62;22c"      => false, // DA1 reply
b"\x1b[I"            => false, // focus in
b"\x1b[O"            => false, // focus out

// Mouse:
b"\x1b[<0;10;20M"    => true,  // press button 0
b"\x1b[<0;10;20m"    => true,  // release button 0
b"\x1b[<2;10;20M"    => true,  // press right button
b"\x1b[<64;10;20M"   => true,  // scroll up
b"\x1b[<65;10;20M"   => true,  // scroll down
b"\x1b[<32;10;20M"   => true,  // drag (motion + button 0)
b"\x1b[<35;10;20M"   => false, // motion only
b"\x1b[M\x20\x20\x20" => true, // X10 mouse (verified via vte)

// Disjunctive:
b"\x1b[<35;10;20Ma"  => true,  // motion-only followed by 'a'
```

### Layer 2: Election worker tests

Extend the existing `mod tests` in
`crates/cairn-pty/src/ghostty/worker.rs`. The `MockSession` harness
gains `write_as(client_id, bytes)` and `resize_as(client_id, size)`
helpers. Test cases (each one `#[tokio::test]`):

- `resize_from_no_client_promotes_to_leader`
- `first_user_input_promotes_to_leader`
- `non_leader_resize_returns_not_leader_error`
- `most_recent_user_input_steals_leader`
- `mouse_motion_does_not_promote`
- `mouse_click_promotes`
- `focus_event_does_not_promote`
- `da_reply_passthrough_does_not_promote`
- `leader_vacates_when_subscription_drops`
- `non_leader_detach_does_not_clear_leader`
- `leader_input_after_promotion_does_not_re_promote`

Tracing assertions use `tracing_test::traced_test` (add to
`dev-dependencies`).

### Layer 3: Integration test against a real PTY

New file `crates/cairn-pty/tests/pty_multi_client.rs`, exercising
election against a real `/bin/cat`:

```rust
#[tokio::test]
async fn two_clients_resize_election_against_real_pty() {
    let pty = GhosttyPty::spawn(SpawnOptions::new(Command::new("/bin/cat"))).unwrap();
    let a = ClientId::from_u64(0);
    let b = ClientId::from_u64(1);

    let _sub_a = pty.subscribe(a).await.unwrap();
    pty.resize(a, TermSize { cols: 100, rows: 30 }).await.unwrap();

    let _sub_b = pty.subscribe(b).await.unwrap();
    let err = pty.resize(b, TermSize { cols: 120, rows: 40 }).await.unwrap_err();
    assert!(matches!(err, PtyError::NotLeader { .. }));

    pty.write(b, Bytes::from_static(b"hello\n")).await.unwrap();
    pty.resize(b, TermSize { cols: 120, rows: 40 }).await.unwrap();
}
```

One real-PTY test for the end-to-end flow. The bulk of correctness
lives in the mock-driven worker tests where we don't have to deal
with kernel scheduling.

### Layer 4: Migration smoke

Existing tests (`pty_io.rs`, `pty_lifecycle.rs`, `pty_resize.rs`),
the `echo` example, and the `StubSession` in `lib.rs` update
mechanically: every call site picks a `ClientId::from_u64(0)` and
threads it. ~15-20 line edits across the four files. Test scope is
unchanged.

## Divergences from zmx

Recorded here so reviewers see them in one place. Each is justified
upstream in the relevant architecture doc.

1. **Headless Terminal stays internal to the worker, not modeled as a
   client.** zmx's daemon has a unified collection of attached
   endpoints with its internal `term` participating via the same
   dispatch path as socket clients. Cairn keeps the `Terminal`
   private to the worker. See the "Architecture" section above.
2. **First-attach snapshot is kept, not suppressed.** Cairn's primary
   use case is long-running headless processes attached to later. See
   [client-attach-and-election.md].
3. **Mouse press / release / scroll / drag promote to leader.** zmx
   excludes all mouse bytes via `isUserInput`. Cairn classifies
   intentional mouse interactions as user input because clicking is
   a clear presence signal in browser-attached clients. Motion-only
   events remain excluded to avoid leadership flapping under mode
   1003.
4. **Non-leader resize returns a typed error (`NotLeader`) instead
   of silently succeeding.** zmx's silent drop matches its
   single-binary CLI; cairn is a library and an honest error is
   appropriate for a programmatic caller.

## Open questions deferred to later steps

- **Leader handoff to most-recent non-leader on detach.** Open
  question 3 in client-attach-and-election.md. Defer until a real
  workload demands it (long-lived web sessions where the only human
  locks their screen).
- **Stuck-leader recovery via heartbeat.** Open question 7. Belongs in
  step 8 (backpressure / transport health), not the library.
- **`is_user_input` as a `pub` API for the daemon.** Punt to step 6
  (auth / read-only viewers) — the daemon may want to pre-emptively
  classify input before forwarding to the library.

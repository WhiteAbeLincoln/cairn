# Backpressure and the slow-client problem

How cairn handles clients that can't drain PTY output as fast as the
child produces it. Scope: the fanout from the PTY master read loop to
subscribers, the in-process queue between the worker and each
subscriber, and policy when a queue grows. Adjacent docs:
[[terminal-state-and-replay]] (snapshot a kicked client receives on
reconnect), [[external-protocol]] (wRPC stream framing),
[[query-response-delegation]] (leader-as-slow-client),
[[client-attach-and-election]] (reconnect flow).

## The problem

A child can produce output orders of magnitude faster than a client
can render it (`yes`, `cat /dev/urandom | xxd`, a verbose build).
Three categories of slow client matter:

- **Backgrounded browser tabs.** Browsers throttle JS to ~1 Hz when a
  tab is hidden. The WebTransport layer in the browser may keep
  ACKing QUIC packets for a while, but the JS reader pulling chunks
  off the bidirectional stream into the ghostty-vt WASM emulator won't
  run; the receive buffer fills, QUIC flow-control closes the
  stream's credit window, and the daemon's wRPC sink stops draining.
- **Network blips.** Remote clients over wifi see multi-second RTT
  spikes; frames queue server-side during the spike.
- **Genuinely slow renderers.** First-paint xterm.js on a low-end
  device, or a debugger-paused web client.

In all three cases the daemon must decide what to do with bytes
produced *while one client is unable to receive*, without penalising
the other clients or running out of memory.

## What cairn does today

cairn fans output via `tokio::sync::broadcast`, created in
`run_session` at `crates/cairn-pty/src/pty/ghostty/worker.rs:235`:

```rust
let (bcast_tx, _) = broadcast::channel::<Bytes>(s.broadcast_capacity);
```

Every PTY read produces a `Bytes` chunk fed to the libghostty emulator
and then broadcast (`worker.rs:283–290`). Subscribers are created in
the `Subscribe` arm (`worker.rs:355–371`) and returned wrapped in a
[`Subscription`](../../../crates/cairn-pty/src/pty/subscription.rs)
alongside a freshly serialised snapshot.

`tokio::sync::broadcast` is a **bounded, lossless-or-lagged** channel.
Each receiver has its own logical cursor over a shared ring of
`broadcast_capacity` slots. When a receiver falls more than `capacity`
messages behind the tail, the next `recv()` returns
`RecvError::Lagged(n)` and the cursor jumps to the current tail.
Other receivers are *not* affected by one slow consumer — that is the
whole reason for picking `broadcast` over an mpsc per client.

The tuning knob is `SpawnOptions::broadcast_capacity`, defaulting to
`1024` (`crates/cairn-pty/src/pty/types.rs:35`) and clamped to a
minimum of 1 in `worker.rs:38` because `broadcast::channel(0)` panics.
The unit is **messages, not bytes**, and one "message" is whatever
`Bytes` chunk a single `pty.read()` produced — up to 65 536 bytes
given the worker's buffer at `worker.rs:246`. The default ring
therefore holds anywhere from a few KiB to ~64 MiB depending on read
granularity. That asymmetry matters when sizing: the knob has units
the operator does not directly see.

**Today the worker itself does nothing on lag.** It calls
`tx.send(chunk)` and discards the error (`worker.rs:289`,
`let _ = tx.send(chunk)`). Lag detection is the *subscriber's*
responsibility — the
[`Subscription`](../../../crates/cairn-pty/src/pty/subscription.rs)
doc-comment instructs callers that on `RecvError::Lagged` they should
drop the `Subscription` and call `subscribe()` again. There is no
per-client byte cap, send timeout, or eviction logic in the worker;
policy is delegated to whatever transport task holds the receiver.
The fanout loop in `worker.rs:265–306` never blocks on a single
client.

## Policy options

Four shapes for "what to do when client X can't keep up":

1. **Drop the client (forcible kick).** Transport observes `Lagged`,
   closes the wRPC stream (or for UDS, the per-attach connection).
   Client reconnects and gets a fresh snapshot via a new `attach`
   invocation. Predictable; relies on the snapshot path being
   cheap and correct ([[terminal-state-and-replay]]).
2. **Drop frames silently.** Skip past lagged messages without telling
   the client. The libghostty state on the *client* diverges from the
   server's — VT is not idempotent and missed bytes cannot be
   reconstructed by replaying later bytes. Acceptable only if paired
   with an immediate fresh snapshot, at which point it is just (1)
   with worse UX.
3. **Per-client bounded buffer with drop-on-overrun.** Convert the
   shared ring into a per-client mpsc with its own cap. Preserves
   state until the cap is hit, then devolves to (1) or (2). Costs
   `O(clients × capacity)` memory in the worst case.
4. **Per-client transport-level backpressure.** Let wRPC stream
   credit (QUIC flow control on WT, the underlying socket on UDS)
   propagate into the consumer task — the task awaits the next stream
   send; if the carrier can't take more bytes, it parks. *Other*
   clients are unaffected because each runs its own consumer over the
   shared broadcast. The parked task's cursor still falls behind, so
   this reduces to (1)/(2) once the ring overflows — transport-level
   backpressure only helps for *briefly* slow renderers.

Recommendation: layer (4) for the common case and (1) as the fallback.
This maps naturally onto the existing architecture: each attach
invocation runs a tight `select!` between `receiver.recv().await` and
yielding the next `server-event` onto its outbound stream; on
`Lagged`, the task closes the stream and the client reconnects fresh.

## How zmx handles this

zmx is single-threaded, manual-poll, and uses a per-client growable
`std::ArrayList(u8)` for outbound bytes. The PTY read path appends
encoded IPC messages to every client's `write_buf`
(`zmx/src/main.zig:2599–2608`); the `POLLOUT` arm drains as much as
the socket will take and slices the consumed prefix off
(`zmx/src/main.zig:2693–2710`). **There is no per-client buffer cap.**
A slow client makes its `write_buf` grow until either the socket
unwedges or zmx OOMs. zmx caps only the *opposite* direction — bytes
destined for the PTY's stdin (`PTY_WRITE_BUF_MAX = 256 * 1024`,
`zmx/src/main.zig:872`, `queuePtyInput` at `:879–895`) — because
"shell stops reading" is the failure mode zmx actually encounters.
The slow-client problem is left for the operator to discover via OOM.

cairn's `broadcast` ring is a *strict improvement*: memory is bounded
by `broadcast_capacity × max_chunk_size` regardless of how many
clients attach, and one slow client cannot starve another because each
has an independent cursor. zmx's mpsc-per-client topology cannot offer
the same guarantee without imposing per-client caps.

## Drift-then-resync as the load-bearing pattern

The libghostty emulator on the daemon side is *authoritative* for
visible screen state — see [[terminal-state-and-replay]]. Because a
fresh `subscribe()` returns a self-contained VT sequence representing
current state (`worker.rs:481–498`, `format_snapshot`), the cost of
kicking a lagged client is one snapshot serialisation plus a
reconnect. Qualitatively cheaper than tmux's "give the client a
perfect byte-stream replay" model — no per-client replay buffer. The
natural cairn policy is therefore:

> When a client lags, kick it. Trust the snapshot.

The implication for `broadcast_capacity` is that it only needs to
absorb *transient* bursts between "PTY emits a flurry" and "a healthy
client's attach task drains it" — sub-second, not seconds.
Operators with chronically slow clients should *reduce* the ring to
fail faster, not enlarge it to paper over the problem.

## Memory bounds and DoS

The worker's `broadcast` is bounded *per session* by
`broadcast_capacity`. Each subscriber adds a `Receiver` (cursor +
wakeup state) but no new byte storage. Per-client storage lives
*outside* the worker, in the transport task that holds the receiver —
that is where the unbounded-queue DoS vector lives.

The wRPC transport layer must therefore both:

- Await the next stream send so carrier-level backpressure (QUIC
  flow control on WT, socket buffer fill on UDS) caps in-flight
  bytes, **and**
- Treat `RecvError::Lagged` as irrecoverable: close the stream, let
  the client reconnect, do not attempt to bridge the gap.

Per-session caps are enforced by the broadcast ring. Per-client caps
must be enforced by the transport. Both layers are required.

## Interaction with wRPC streaming ([[external-protocol]])

The `attach` operation returns a `stream<server-event>` per WIT
(`crates/cairn-protocol/wit/cairn.wit`). wRPC carries that over a
QUIC bidirectional stream (WebTransport) or a UDS connection — both
provide flow control under the hood. Recommend:

- Per-attach loop reads from the broadcast receiver and yields the
  next `server-event::output(bytes)` onto the outbound stream. This
  naturally couples client RX speed to per-task consumer speed.
- A `max_buffered_bytes_per_client` ceiling well below the ring's
  byte capacity; crossing it triggers stream close.
- Per-element size sized for libghostty snapshots (tens of KiB
  on first attach; steady-state output frames are tens to hundreds
  of bytes).

## The backgrounded-tab case specifically

Browsers keep ACKing QUIC packets for a while after a tab is hidden —
the receive buffer fills, then QUIC flow control closes the stream's
credit window. The server-side wRPC stream sees its outbound
operations park awaiting credit. With the "await the next send"
pattern, the consumer task parks, its broadcast cursor stops
advancing, the ring fills, and the next `recv()` returns `Lagged`.
We close the stream. When the tab is foregrounded, the TS client
opens a new `attach` invocation and the resnapshot flow
([[terminal-state-and-replay]]) repaints.

Two operator-tunable thresholds to expose ([[configuration]]):

- `max_buffered_bytes_per_client` — hard ceiling for the transport
  send queue. Triggers WS close + reconnect.
- `client_send_timeout` — per-`send().await` deadline. Defends against
  a socket that's been `Pending` too long with no protocol-level error.

## Interaction with leader query replies ([[query-response-delegation]])

[[query-response-delegation]] routes DA1/DSR/DECRQM through the
*leader* client. If the leader is slow, its query replies lag too —
and a stalled DA1 can wedge an *other* client that is
mid-initialisation. Treat "leader cannot drain within the
query-response deadline" identically to other lag: kick the leader,
elect a new one ([[client-attach-and-election]]), let the backend's
libghostty synthesise replies directly. The leader's role is an
optimisation, not a correctness requirement; when it becomes a
liability, demote it.

## Open questions

- **Should the worker observe lag at all?** Today it is oblivious
  (`let _ = tx.send(chunk)`). Tracking per-receiver lag via
  `tx.receiver_count()` + `tx.len()` and surfacing it in
  [[observability]] would let operators see lag before clients
  silently drop frames.
- **Units of `broadcast_capacity`.** Messages-not-bytes makes the
  default of 1024 ambiguous. Is the right unit messages, KiB, or a
  *time-window of PTY output*? A millisecond-budget would self-tune
  to the child's throughput.
- **Per-client byte cap location.** Belongs in the wRPC transport
  layer (the attach-handler task that yields `server-event`s), but
  the worker can't *enforce* it — should the
  `PtySession` trait grow a `subscribe_bounded(max_bytes)` variant
  proxying the broadcast receiver through an mpsc with a byte budget?
- **Snapshot cost on lag-kick.** `format_snapshot` serialises full
  scrollback (`worker.rs:481–498`). If a flapping client triggers
  repeated full snapshots, is the cost bounded? Cap snapshot
  scrollback separately from `scrollback_lines`?
- **Leader demotion deadline.** What is the actual threshold before a
  slow leader is dethroned? Tied to the query-response timeout in
  [[query-response-delegation]], or independent?
- **Test strategy.** Slow-client behaviour is timing-dependent.
  Belongs in [[testing]] as integration scenarios with a deliberately
  throttled wRPC stream peer (the TS client or a Rust peer that
  delays stream reads).
- **Error taxonomy ([[error-recovery]]).** A lag-kick is *not* a
  session-level failure — keep "client kicked for lag" out of any
  session-failure metric.

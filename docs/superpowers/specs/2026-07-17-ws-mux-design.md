# WebSocket Multiplexing (`cairn-mux-v0`) — Design

**Date:** 2026-07-17
**Issue:** [#11](https://github.com/WhiteAbeLincoln/cairn/issues/11) (problem 1 of 3; session-list
subscriptions and health-probe removal are follow-up work)

## Problem

The web UI's wRPC transport opens a **new WebSocket per RPC invocation** — TCP + TLS + upgrade
handshake, one call, teardown. The session list page alone produces ~15 connections/min from
three sources (5 s `list-all` poll, 15 s `version` health probe, on-reconnect refresh). This
discards the core benefit of WebSockets: one persistent bidirectional channel carrying many
messages.

### Why wRPC works this way (prior art)

This is structural in wRPC, not an oversight. The wire format
([SPEC.md](https://github.com/bytecodealliance/wrpc/blob/main/SPEC.md)) is a per-stream header
`{instance, name}` followed by frames `{path: list<u32>, data: list<u8>}` — **no invocation ID
exists**. The `path` multiplexes the async sub-values of a *single* call (`stream<T>`/`future<T>`
params and results); invocation-level multiplexing is delegated to the transport. The spec states
"transports provide a single bidirectional byte stream per wRPC invocation" and, for TCP, that the
"client MUST establish a new connection to that socket per each invocation."

The design comes from wRPC's NATS heritage (wasmCloud), where a per-invocation channel is a nearly
free subject + reply inbox. QUIC/WebTransport get the same cheaply via `open_bi()` on a shared
connection — which is why cairn's existing `wtDialer` does not have this problem. TCP/UDS/WebSocket
are the degenerate case where "mint a channel" = "dial a connection." Upstream has no plan to
change this: [PR #1382](https://github.com/bytecodealliance/wrpc/pull/1382) made the
one-stream-per-invocation framing mandatory and removed the `Index` trait, entrenching the model.

WebTransport is not an escape hatch for cairn: it is QUIC/UDP, which does not traverse
`tailscale serve` (a TCP/TLS proxy — the scenario from issue #5), and Safari support is shaky.
WebSocket is the transport that works everywhere, so it is the one worth fixing.

### The seams that make this feasible without forking

Both endpoints expose a clean per-invocation byte-stream abstraction:

- **JS:** `@bytecodealliance/wrpc`'s entire external contract is the 3-method `Transport`
  interface (`read`/`write`/`closeWrite`); `invoke()` never touches a socket. The value codec,
  intra-invocation `Mux`, and session logic are all transport-agnostic and reused unchanged. The
  dial-per-call behavior lives in cairn's own `wsDialer` (`cairn-web/src/lib/protocol/ws.ts`),
  and `wtDialer` already demonstrates the cached-connection + channel-per-dial shape.
- **Rust:** the daemon's WS path implements `wrpc_transport::frame::Accept`
  (`serve/transport/websocket.rs`), whose `accept()` yields one `(Context, AsyncWrite, AsyncRead)`
  triple per invocation. Today a `OneShot` acceptor yields the raw socket once; the daemon already
  runs the persistent accept-loop model for UDS/WebTransport (`serve/wrpc.rs`).

So the work is a small cairn-owned channel-mux protocol implemented symmetrically in TS and Rust.
No changes to `wrpc-transport` (published 0.29), the vendored JS package, `cairn-protocol`, the
WIT schema, handlers, or the registry.

## Goals

- A single persistent WebSocket carries all unary RPC traffic from the web UI.
- Cancellation of an individual call without disturbing the connection.
- Connection liveness that survives idle periods behind NAT/proxies (`tailscale serve`).
- The one-shot protocol remains fully supported for dedicated high-throughput sockets.

## Non-goals (explicitly out of scope)

- **Session-list subscriptions** (issue #11 problem 2) — separate design; the 5 s `list-all` poll
  remains for now, riding the mux.
- **Health-probe removal** (issue #11 problem 3) — the probe remains, riding the mux (see
  Liveness).
- Per-channel flow control, channel priorities, server-initiated channels — deferred to a future
  `cairn-mux-v1` if the anticipated plugin/high-throughput scenarios materialize.
- Old-daemon compatibility fallback in the client — nothing is published yet; a failed
  negotiation is just a failed connection.

## Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Protocol selection | `Sec-WebSocket-Protocol: cairn-mux-v0` on the existing `/ws` | RFC 6455's purpose-built mechanism; no discovery (`/cairn.json`) changes; version negotiation built into the name |
| One-shot protocol | Kept, served when no subprotocol offered | Future split: low-priority muxed events vs. dedicated sockets for high-throughput streams |
| Mux scope | Unary calls + `wait` on the mux; `attach`/`logs`/`send` on dedicated one-shot sockets | PTY throughput never contends with control traffic; mux flow control stays trivial |
| Mux protocol | Minimal cairn-owned framing (below) | yamux's only maintained JS impl drags in libp2p's interface stack — a bigger shim than this whole protocol; upstream contribution contradicts wrpc's direction |
| Flow control | Socket-level only | Mux carries small frames only; bounded per-channel buffers + whole-socket read pause on the daemon |
| Liveness | Daemon WS pings (~30 s) + client `version()` probe over the mux (~30 s) | Pings keep NAT paths warm and detect dead clients; the probe detects a wedged daemon and silent path death (browsers cannot send WS pings) |

## Wire protocol: `cairn-mux-v0`

### Negotiation

The client opens `/ws` offering `cairn-mux-v0` in `Sec-WebSocket-Protocol`. The daemon echoes it
in the 101 response and serves muxed mode. A client offering no subprotocol gets the existing
one-shot protocol unchanged. There is no client-side fallback: if the daemon does not echo, the
browser fails the connection and the reconnect machinery reports it like any outage.

### Framing

Every WS **binary message** is exactly one mux frame (WS message boundaries delimit frames — no
length prefix):

```
[channel_id: u32 big-endian][flags: u8][payload: 0..N bytes]
```

- Flags: bit 0 = `FIN`, bit 1 = `RST`, bits 2–7 reserved (non-zero ⇒ protocol error).
- Text frames are a protocol error in muxed mode. The one-shot empty-text EOF sentinel does not
  exist here; `FIN` replaces it, per channel per direction.
- `FIN` may accompany a non-empty payload (receiver processes the payload, then marks EOF) or be
  a standalone empty-payload frame.

### Channels

- Only the client opens channels. Ids start at 1, strictly increasing within a connection, never
  reused. Id 0 is reserved for future control use (a frame addressed to it is a protocol error).
- A frame whose id exceeds the highest previously seen opens that channel.
- Each channel carries exactly one wRPC invocation, byte-identical to a one-shot socket's
  payload: client writes the invocation and sends `FIN` when its write side completes (wRPC
  `closeWrite`); daemon writes results and sends `FIN` when done. Both sides FIN'd ⇒ channel
  closed and forgotten.
- `RST` from either side aborts the channel immediately (payload ignored, both directions dead)
  without touching the socket. The client uses it to cancel a call; the daemon uses it for
  channels it cannot serve.

### Tolerance rule

`RST` and in-flight data can cross on the wire. Frames addressed to a non-live channel with
id ≤ the highest-seen id are **silently ignored** — never an error. Only malformed traffic is a
protocol error: reserved flag bits, text frames, oversized frames, channel id 0.

### Limits, flow control, errors

- Max frame payload: 1 MiB (larger ⇒ protocol error).
- Max concurrent channels: 256 per connection; the daemon `RST`s channels beyond the limit
  rather than killing the socket.
- Flow control is socket-level: client-side `bufferedAmount` backpressure (as today); daemon-side
  each channel has a bounded inbound buffer and a full buffer **pauses reads of the whole socket**
  (TCP backpressure). Head-of-line blocking is accepted: the mux carries small unary traffic only.
- A protocol error tears down the whole connection; everything else is contained to its channel.

### Liveness

The daemon sends WS protocol-level pings every ~30 s on muxed connections (browsers auto-pong;
no JS involved). The client keeps the existing `ReconnectController` `version()` probe, riding
the mux at a relaxed 30 s interval (was 15 s) — near-zero cost on the persistent socket, and it
detects failures pings cannot: a wedged daemon, and silent path death invisible to the browser.

## Daemon design (Rust)

**Negotiation in `serve/http.rs`.** The `/ws` upgrade handler additionally parses
`Sec-WebSocket-Protocol`; if the offer list contains `cairn-mux-v0`, the 101 echoes it and the
connection goes to the muxed serve path. Otherwise the existing one-shot path runs untouched.
Auth and origin policy run once at upgrade, as today; every channel inherits the connection's
`ConnCtx`.

**New mux module (e.g. `serve/transport/ws_mux.rs`).** Operates on the
`tokio_websockets::WebSocketStream` at *message* granularity — it does **not** use
`crate::ws::split()`, which byte-splits and erases the message boundaries the framing relies on.

- **One reader task:** parses frames, routes payloads into per-channel bounded buffers, opens new
  channels, applies the tolerance rule, enforces limits. A full channel buffer suspends the read
  loop (whole-socket pause).
- **One writer task:** sole consumer of an mpsc of outbound frames from all channels; writes to
  the WS sink with flush-after-write (same rationale as `FlushOnWrite`); owns the ping interval.
  Single-writer ⇒ no sink locking, natural serialization.
- **Logical channel streams:** per channel, an `AsyncRead` half (drains the inbound buffer, EOF on
  peer FIN) and an `AsyncWrite` half (chunks writes into ≤ 1 MiB frames; `poll_shutdown` sends
  FIN; dropping an un-FIN'd writer sends RST so aborted handlers clean up their channel).

**Accept seam.** `MuxAcceptor` implements `wrpc_transport::frame::Accept`, receiving
`(ConnCtx, writer, reader)` triples from the reader task via a channel — the shape `OneShot`
fakes today, yielded repeatedly. The serve loop mirrors `serve/wrpc.rs::run_wrpc_server`:
`server.accept(&acceptor)` in a loop, each routed invocation spawned as a tracked task, all
cancelled on shutdown under the existing `TaskTracker` drain.

## Web client design (TS)

**New `wsMuxDialer(url)`** (sibling of `ws.ts`), shaped like `wtDialer`: one lazily-opened cached
WebSocket via `new WebSocket(url, ['cairn-mux-v0'])`; on open, verifies
`ws.protocol === 'cairn-mux-v0'` and fails otherwise. Each dial allocates the next channel id and
returns a `Transport`:

- `read()` — pulls from the channel's `Chan<Uint8Array>`, fed by the shared `onmessage` demux;
  peer FIN closes the Chan (EOF), peer RST closes it with an error.
- `write(bytes)` — prepends the 5-byte header; reuses `ws.ts`'s `bufferedAmount` backpressure.
- `closeWrite()` — sends an empty-payload FIN frame.
- `close()` — sends RST if the channel is not already complete and frees the table entry.
  (`DaemonClient`'s existing `finally` blocks call this, so cancellation composes unchanged.)

**Socket lifecycle.** `onclose`/`onerror` fails every live channel's Chan with a connection
error, clears the table, and forgets the socket (the `wtDialer` forget pattern); the next dial
redials. All in-flight invocations fail together — the socket is the health signal. The dialer
exposes an `onDown` hook so the connection layer flips to "reconnecting" immediately rather than
waiting for the next probe.

**`DaemonClient` routing.** Constructor takes two dialers, `control` and `streams` (`streams`
defaults to `control`, preserving existing constructors/tests). Unary calls and `wait`
(long-lived but tiny) use `control`; `attach`, `logs` (scrollback dumps are bulky), and `send`
use `streams`. Nothing else in `client.ts` changes.

**Wiring (`connection.svelte.ts`).** WS endpoints: `control = wsMuxDialer(url)`,
`streams = wsDialer(url)`. WebTransport endpoints: both roles stay `wtDialer` (QUIC already
multiplexes). `ReconnectController` keeps its probe (over the mux, 30 s) plus the `onDown` hook
triggering an immediate re-probe.

**Net effect:** one persistent `/ws` connection for everything, plus one dedicated one-shot
socket per active attach/logs/send.

## Failure matrix

| Failure | Blast radius | Observed as |
|---|---|---|
| Handler error / malformed wRPC payload on a channel | That channel (RST) | One call rejects; socket and other calls unaffected |
| Client cancels a call (`transport.close()`) | That channel (RST) | Daemon handler's writes fail; its task ends |
| Mux protocol violation | Whole connection | All in-flight calls fail; client redials |
| Socket drop / daemon restart | Whole connection | All in-flight calls fail together; `onDown` ⇒ immediate reconnect status |
| Daemon shutdown | Whole connection | Tracked tasks cancelled inside the existing drain window |

## Testing

Behavioral tests through real interfaces (per repo test discipline):

**Rust** — the mux serve path driven over `tokio::io::duplex` with hand-written raw frames:

- Two interleaved invocations on one connection both complete correctly.
- FIN half-close delivers results; RST mid-call terminates that channel's handler without
  disturbing a concurrent call.
- Stale-frame tolerance (RST/data race) is silent.
- Each protocol violation (reserved flags, text frame, oversized frame, id 0) closes the
  connection; channel-limit overflow RSTs the new channel and leaves the socket alive.

Daemon integration tests alongside the existing harness: a real WS listener with a
`tokio-websockets` client negotiating `cairn-mux-v0` running concurrent `version` + `list-all`
over one socket; and a regression test that a no-subprotocol client still gets one-shot behavior.

**TS** — unit tests for `wsMuxDialer` against a mock WebSocket: header framing, demux routes
payloads to the correct channel's Transport, FIN ⇒ EOF, RST ⇒ error, socket drop fails all live
channels, `close()` emits RST. End-to-end through the same in-process harness that covers the
one-shot `DaemonClient` path.

## Follow-ups (tracked separately)

- Session-list subscription (`stream<session-event>` with full-snapshot events) replacing the
  5 s poll — rides this mux.
- Health-probe retirement/retuning once subscriptions provide ambient traffic.
- `cairn-mux-v1` if per-channel flow control, priorities, or server-initiated channels become
  necessary (plugin scenarios).

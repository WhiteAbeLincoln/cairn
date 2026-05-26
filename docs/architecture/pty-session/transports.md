# Client transports

How clients reach the cairn daemon. This doc is about the carrier — UDS, QUIC/WebTransport — not the wire format. The wire format (wRPC over WIT-defined interfaces) is transport-agnostic by design and is documented in [[external-protocol]].

Scope: client ↔ daemon. The worker ↔ daemon transport and the host ↔ spawner transport are separate concerns covered in [[worker-backends]].

## What the three client types need

| Client | Latency | Disconnect frequency | Recovery requirement |
|---|---|---|---|
| Local CLI (same machine) | ~0ms | ~never | n/a |
| Remote CLI (laptop, desktop, server) | 10-200ms | occasional | required |
| Browser desktop | 10-100ms | tab background, sleep, page reload | required, mostly invisible |
| Browser mobile | 10-2000ms (spiky) | frequent: backgrounding, sleep, network change | required, invisible, fast |

The important observation: **mobile and desktop browsers face the same correctness problem, just at different frequencies.** Both require reconnect-as-first-class. We're already designing for that ([[client-attach-and-election]]). Mobile just exercises the reconnect path harder.

The properties mobile *additionally* wants are connection migration (network handoff without reconnect), aggressive disconnect detection, and graceful handling of input typed during offline windows. Some of these are transport properties (QUIC connection migration), and some are design properties any transport must support (heartbeats, buffered-input UX).

## v0 transports: UDS (local) and WebTransport (remote)

### Unix Domain Socket — local CLI

The local CLI opens a fresh UDS connection per invocation against `$XDG_RUNTIME_DIR/cairn/cairn.sock` on Linux or `$TMPDIR/cairn/cairn.sock` on macOS. Socket mode `0o600`, parent directory mode `0o700` (both configurable — see the daemon binary design spec).

- **Carrier**: wRPC's UDS transport (`wrpc_transport::unix`). The framed-stream spec applies — one connection per invocation — but on a loopback socket this is microseconds.
- **Auth**: filesystem DAC (user only sees their own socket) plus `SO_PEERCRED` (Linux) / `getpeereid` (macOS) to record the invoking uid for audit. No bearer token.
- **Connection lifecycle**: fresh connection per CLI invocation; long-running operations (`attach`) hold the connection until the operation ends.

### WebTransport over HTTP/3 — browser and remote CLI

All remote clients — browsers and off-machine CLI invocations — share a single HTTP/3 endpoint.

- **Listener**: `127.0.0.1:<port>` by default; configurable to a non-loopback address for remote and LAN access.
- **Carrier**: wRPC's `transport-web` crate (`wrpc_transport_web`), which wraps `wtransport` (QUIC via quinn underneath). The `Client` type wraps a `wtransport::Connection`; each wRPC invocation opens a bidirectional stream via `Connection::open_bi()` (`crates/transport-web/src/lib.rs:97-103`). Stream open is a single QUIC SETTINGS frame — no per-invocation handshake cost.
- **Auth**: bearer token via the first invocation on each connection — a `meta.authenticate(token)` call that the daemon requires before serving any other interface. This is not a preference; it is forced by the browser API. The `WebTransport` constructor in JS only accepts `allowPooling`, `congestionControl`, `requireUnreliable`, and `serverCertificateHashes` — there is no `Authorization` header path from the browser. URL-embedded tokens leak to access logs; cookies require same-origin setup we do not want for v0. First-message auth is the only mechanism that works for the browser, and the remote CLI uses the same path for uniformity.
- **Connection lifecycle**: browser holds one `Connection` per page-session; remote CLI opens one `Connection` per process invocation. Multiple wRPC operations interleave on QUIC streams within the same connection.

**Why WebTransport, not WebSocket:**

1. wRPC has `crates/transport-web` as a first-class WebTransport carrier. There is no upstream wRPC WebSocket transport; building one would be a significant upstream contribution rather than a one-line `Cargo.toml` dependency.
2. QUIC stream multiplexing lets multiple concurrent invocations share one connection without head-of-line blocking or per-call handshake overhead.
3. QUIC connection migration preserves the session across IP changes (wifi → cellular), eliminating a common mobile interruption without any application-layer reconnect.
4. Safari 26.4 ships stable WebTransport — the last browser holdout. Universal browser coverage is no longer a concern.

## Mobile-friendliness, regardless of transport

These are non-negotiable for the browser client:

1. **Reconnect-as-fresh-attach is the only mode.** Every reconnect re-runs authentication then `sessions.attach` and receives a fresh `server-event::snapshot`. No "resume from offset" complexity; no sequence-number replay. The session state on the daemon survives the disconnect ([[pty-lifecycle]]).
2. **Heartbeats with quick failure detection.** Ping every 30s while a session is in the foreground, every 5min when idle. Declare dead in ≤60s; immediately reconnect.
3. **Page Visibility API integration.** When the tab is foregrounded, immediately probe; if no response within 2s, reconnect. Do not wait for the heartbeat timer.
4. **Reconnect backoff with a low cap.** Immediate, 1s, 2s, 5s, 30s. Never longer — users will give up.
5. **Buffered input is not auto-replayed.** Keystrokes typed during an offline window are shown to the user with an option to send or drop, never auto-sent. The inferior's prompt state may have changed in the interim.
6. **Server-side session lifetime is decoupled from client lifetime.** Already the design — the session keeps running with no client attached ([[client-attach-and-election]]).

These properties sit at the application layer, not the transport layer. They apply identically over WebTransport.

## Transport options, compared

| Transport | Browser | CLI | Connection migration | Maturity | Verdict |
|---|---|---|---|---|---|
| **Unix socket** | ❌ | ✅ local only | n/a | mature | **Primary v0 — local CLI** |
| **WebTransport (HTTP/3 / QUIC)** | ✅ | ✅ | ✅ QUIC-native | stable (Safari 26.4) | **Primary v0 — remote** |
| **WebSocket (TCP/TLS)** | ✅ | ✅ | ❌ | mature | Considered; rejected — no wRPC transport |
| **TCP/TLS (standalone)** | ❌ | ✅ | ❌ | mature | Future, additive if WT unworkable |
| **WebRTC data channels** | ✅ | painful | ✅ ICE restart | mature for video | Wrong abstraction |
| **SSE + POST** | ✅ | ❌ | ❌ | mature | Awkward split, half-duplex |
| **mosh-style UDP** | ❌ | possible | ✅ | tool-specific | Inspiration only |

### WebRTC, SSE, raw TCP, mosh — rejected

- **WebRTC data channels** are designed for peer-to-peer with NAT traversal (STUN/TURN/ICE signaling). For client-server, the abstraction is wrong-shaped and the complexity is enormous.
- **SSE + POST** splits server-to-client (SSE) from client-to-server (POST), giving up ordering between the two directions. Half-duplex shape; awkward for interactive use.
- **Raw TCP** for the CLI would save a small amount of framing overhead but loses code reuse with the browser path and has no browser support. Kept in the table as a future additive fallback if WebTransport proves unworkable in UDP-blocked deployments.
- **mosh** is a brilliant transport for high-latency interactive use (predictive local echo, UDP roaming). Not browser-compatible and tightly coupled to mosh's own terminal model. Inspiration for predictive-echo design if we ever go there; not adoptable as a library.

### WebSocket — considered and rejected

WebSocket would work functionally. The blocker is wRPC: there is no `wrpc_transport_ws` crate. The existing `crates/transport-web` is WebTransport-native. Building a WebSocket transport for wRPC would be a meaningful upstream contribution, not a configuration choice, so it is not the right path for v0.

## Not in v0

- **Standalone TCP/TLS transport.** Additive later if WebTransport proves unworkable in some deployment (UDP blocked, HTTP/3 disabled by policy). No schema change required; the wRPC wire format is transport-agnostic.
- **Connection-pooling agent (`cairn-agent`).** Could amortize the QUIC+TLS handshake on remote CLI by multiplexing process invocations over one long-lived connection. Out of scope for v0.
- **mTLS / client-certificate auth.** Relevant if the daemon ever listens on a public interface; tracked in [[authentication]].

## Open questions

1. **Rate-limiting unauthenticated connections.** First-message auth means the daemon accepts a WT connection and allocates session state before knowing whether the peer has a valid token. Need a connection-establishment rate limiter and a deadline (~5s) for the first frame, after which the connection is closed.
2. **Reconnect input buffering UX.** When the connection drops mid-typing, the user has a queue of unsent keystrokes. Show them, let them confirm? Auto-drop? Show as a translucent overlay? This is a UX decision, not architectural, but it shapes the protocol (see [[external-protocol]] for the `client-event` message shape).
3. **Service workers for backgrounded-tab keepalive.** Browsers throttle JS in hidden tabs. A service worker can maintain the WebTransport connection across tab states; worth a prototype before committing — service workers have their own lifecycle complexity.

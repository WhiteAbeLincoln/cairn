# Client transports

How clients reach the cairn daemon. This doc is about the carrier — TCP, UDP, WebSocket, etc. — not the wire format. The wire format (msgpack-tagged binary frames with a version prefix) is documented in [[external-protocol]] and is transport-agnostic by design.

Scope: client ↔ daemon. The worker ↔ daemon transport and the host ↔ spawner transport are separate concerns covered in [[worker-backends]].

## What the three client types need

| Client | Latency | Disconnect frequency | Recovery requirement |
|---|---|---|---|
| Local CLI (same machine) | ~0ms | ~never | n/a |
| Remote CLI (laptop, desktop, server) | 10-200ms | occasional | required |
| Browser desktop | 10-100ms | tab background, sleep, page reload | required, mostly invisible |
| Browser mobile | 10-2000ms (spiky) | frequent: backgrounding, sleep, network change | required, invisible, fast |

The important observation: **mobile and desktop browsers face the same correctness problem, just at different frequencies.** Both require reconnect-as-first-class. We're already designing for that ([[client-attach-and-election]]). Mobile just exercises the reconnect path harder.

The properties mobile *additionally* wants are connection migration (network handoff without reconnect), aggressive disconnect detection, and graceful handling of input typed during offline windows. None of these are inherently transport features — they're design properties that any transport can or can't support.

## Recommendation: WebSocket as the v0 primary, Unix-socket fast path for local CLI

For all clients, **WebSocket over HTTPS** carries the [[external-protocol]] framing:

- **Local CLI**: prefers Unix socket at `$XDG_RUNTIME_DIR/cairn/control.sock` (filesystem-DAC auth from [[authentication]]). WebSocket fallback for portability or when running CLI from outside a Unix environment. Identical wire format on both — the framing doesn't change.
- **Remote CLI**: WebSocket over TLS. Token auth via first-message ([[authentication]]). Same client code as the browser, less the browser-specific UI plumbing.
- **Browser (desktop or mobile)**: WebSocket over HTTPS. Token auth via first-message.

Reasoning, in priority order:

1. **Universal browser support.** Every browser version we care about supports WS. No "graceful degradation" path needed.
2. **Mature ecosystem.** `tokio-tungstenite` on the daemon side, native `WebSocket` in browsers, `tungstenite` for the CLI.
3. **Reaches everywhere.** WS rides on HTTPS; every corporate proxy, mobile carrier, and CDN handles it. UDP-based transports do not.
4. **Single ordered stream matches our PTY model.** The byte stream from daemon to client is inherently one ordered sequence; multi-stream transports buy us nothing for that (see "Multi-stream and datagrams" below).
5. **Mobile-friendliness is mostly a design problem, not a transport problem.** Properly designed reconnect handling closes ~95% of the gap to "fancier" transports.

## Mobile-friendliness, regardless of transport

These are non-negotiable for the browser client:

1. **Reconnect-as-fresh-attach is the only mode.** Every reconnect re-runs `Hello` + `Attach { session_id }` and gets a fresh snapshot. No "resume from offset" complexity; no sequence-number replay. The session state on the daemon survives the disconnect ([[pty-lifecycle]]).
2. **Heartbeats with quick failure detection.** Ping every 30s while a session is in the foreground, every 5min when idle. Declare dead in ≤60s; immediately reconnect.
3. **Page Visibility API integration.** When the tab is foregrounded, immediately ping; if no pong within 2s, reconnect. Don't wait for the heartbeat timer.
4. **Reconnect backoff with a low cap.** Immediate, 1s, 2s, 5s, 30s. Never longer — users will give up.
5. **Buffered input is not auto-replayed.** Keystrokes typed during an offline window are shown to the user with an option to send or drop, never auto-sent. The inferior's prompt state may have changed in the interim.
6. **Server-side session lifetime is decoupled from client lifetime.** Already the design — the session keeps running with no client attached ([[client-attach-and-election]]).

These work identically over WebSocket and WebTransport. They're properties of the application, not the transport.

## Transport options, compared

| Transport | Browser | CLI | Mobile-friendly | Maturity | Verdict |
|---|---|---|---|---|---|
| **Unix socket** | ❌ | ✅ local only | n/a | mature | **Local-CLI fast path** |
| **WebSocket (TCP/TLS)** | ✅ | ✅ | needs design | mature | **Primary v0** |
| **WebTransport (HTTP/3 over QUIC)** | ⚠️ | ⚠️ | better (migration) | nascent | Future, optional |
| **WebRTC data channels** | ✅ | painful | yes (ICE restart) | mature for video | Wrong abstraction |
| **SSE + POST** | ✅ | ❌ | better auto-reconnect | mature | Awkward split |
| **Raw TCP** | ❌ | ✅ remote | poor | mature | No reuse with browser |
| **mosh-style UDP** | ❌ | possible | excellent | tool-specific | Inspiration only |

### WebRTC, SSE, raw TCP, mosh — rejected briefly

- **WebRTC data channels** are designed for peer-to-peer with NAT traversal (STUN/TURN/ICE signaling). For client-server, the abstraction is wrong-shaped and the complexity is enormous. Skip.
- **SSE + POST** would split server-to-client (SSE) from client-to-server (POST), giving up ordering between the two directions. Half-duplex shape; awkward for interactive use. Skip.
- **Raw TCP** for the CLI would save a small amount of WS framing overhead but lose code reuse with the browser path. Marginal gain, real cost. Skip.
- **mosh** is a brilliant transport for high-latency interactive use (predictive local echo, UDP roaming). Not browser-compatible and tightly coupled to mosh's own terminal model. Inspiration for predictive-echo design if we ever go there; not adoptable as a library.

## WebTransport: consider for later, not v0

WebTransport (HTTP/3 over QUIC over UDP) is the only "modern" alternative worth taking seriously.

### What it would buy

One thing of substance: **connection migration on network handoff.** QUIC Connection IDs survive an IP change, so when a phone walks from wifi to cellular the session stays connected. With WS, the TCP connection drops and the client reconnects (fast, but visible as a ~1-second hang).

That's a real mobile UX improvement. It is *not* a correctness improvement — we still need full reconnect logic for backgrounding, sleep, and dead links — but it removes one common interruption.

### What it would *not* buy

The "multiple streams plus datagrams" pitch sometimes attached to WebTransport doesn't apply to our architecture. Working through it explicitly because it will come up again:

**PTY data stream + separate control stream** (claim: priority, ordering isolation, backpressure isolation). For our traffic profile:

- *Priority during bulk output*: we send PTY output in chunks of one kernel read (~65KB max). Between chunks, control messages slot in immediately. Worst-case stall of a resize message: ~5ms on LTE, hundreds of ms only on pathological 3G. If this ever matters, the targeted fix is to cap chunk size to ~16KB (drops worst case by 4×) or add app-layer priority queues — both cheaper than introducing two-stream coordination code.
- *Ordering between control and data*: sometimes you want them ordered (apply resize before continuing to render), sometimes not (re-auth shouldn't wait for output). Single-stream with independent message dispatch handles both correctly. Multi-stream gives you the "independent" case for free but doesn't help the "ordered" case.
- *Backpressure isolation*: if a client is too slow to drain data, the right response is to kick them and reconnect them with a fresh snapshot ([[backpressure]]). Once that policy is in place, the "control still flows while data is queued" argument is moot — we don't let the queue grow that big.

**Datagrams for heartbeats**: wrong. Heartbeats need reliability. The whole point of a heartbeat is "I'm still here"; if a heartbeat is lost without retransmit, it looks identical to a real disconnect. You'd have to widen the dead-detection threshold to absorb routine UDP loss, which makes failure detection *slower*. Reliable heartbeats on the control channel are what every robust protocol does.

**Datagrams for telemetry / focus events**: tolerable either way. Loss is OK because the next event supersedes. At our event rates (a few per second tops, ~50 bytes each), the savings from skipping retransmit are unmeasurable. Reliable messages are fine.

**Datagrams for mosh-style predictive echo acks**: wrong. Mosh's acks aren't "did this datagram arrive" — they're authoritative state-sync messages that the client reconciles its speculative state against. Losing an ack causes permanent state divergence between client and server. Reliable delivery is *required* if we ever build this. (And predictive echo is app-layer complexity we're not doing in v0 anyway; the right place for it is the client-side emulator, not the transport.)

### Transport-pluggability lever

Same shape as worker backends ([[worker-backends]]). The [[external-protocol]] wire format is transport-agnostic: msgpack-tagged frames, version prefix, opaque PTY bytes. Adding WebTransport later is additive — daemon binds an HTTP/3 listener alongside the existing WS endpoint; client capability-negotiates ("I support WT") in `Hello`; same frames flow over whichever carrier won.

This means we don't have to design for WT now to keep the option open. The carrier-pluggability falls out of the existing message-shape design.

### When to revisit

Conservatively:

- **Safari ships stable WebTransport.** Without this we exclude iOS browser users from any WT-specific UX benefit. Verify current state when revisiting.
- **Measured production evidence** that mobile-handoff session drops are a real complaint. If users don't notice or don't care, the work isn't worth it.
- **Hosting/CDN story matures.** Many corporate networks and some mobile carriers block or throttle UDP. The deployment shapes we care about need to support HTTP/3 end-to-end.

Until then, single-stream WS with proper reconnect handling is the right design.

## Open questions

1. **Service workers for keeping the WS alive in backgrounded tabs.** Browsers throttle JS in hidden tabs, which can stall WS handling and cause spurious heartbeat timeouts. A service worker can keep the connection alive across tab states. Worth a prototype before committing — service workers have their own lifecycle complexity.
2. **Reconnect input buffering UX.** When the connection drops mid-typing, the user has a queue of unsent keystrokes. Show them, let them confirm? Auto-drop? Show as a translucent overlay that fades? This is a UX decision, not architectural, but it shapes the protocol (do we need a `BufferedInput` message type?).
3. **Capability negotiation in `Hello`.** When we add a second transport, the protocol-version byte in [[external-protocol]] needs to be reconciled with transport-specific capabilities. Likely a `capabilities: [...]` array in `Hello` rather than fattening the version byte.
4. **mTLS for non-loopback remote CLI.** Once the daemon listens on a non-loopback address, bearer token alone isn't enough — we need transport-layer auth too. Affects [[authentication]] and any "expose cairn to my LAN" deployment.
5. **Predictive local echo.** A v2+ design. Doesn't require transport changes — works over single-stream WS via app-layer sequence numbers and reconciliation against authoritative bytes. Worth a focused doc if/when we go there.

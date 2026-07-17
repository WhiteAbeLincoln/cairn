# Cairn Web UI v2

A browser interface for managing Cairn PTY sessions: session list, create,
inspect, and a ghostty-compatible terminal attach. Second attempt — the first
(`feat/web-ui`, spec `2026-06-13-web-ui-design.md`) failed on a hand-written
wRPC codec that could not interoperate with the daemon and exhibited a
sustained daemon CPU spike (~600%) while a web client was connected.

## What changed since v1

- The wRPC project now ships an official JavaScript SDK
  (`@bytecodealliance/wrpc`, in the wrpc repo under `js/`): dependency-free ES
  modules, runtime WIT type descriptors, `invoke()`/`accept()` over any
  byte-duplex transport, full `stream`/`future` multiplexing. The entire
  hand-written codec layer from v1 is deleted from the design.
- The wrpc repo also gained a `wrpc-websockets` Rust crate (client `Invoke`
  impl + `split()` helper that adapts a WebSocket into the byte-stream halves
  `wrpc_transport::frame` expects).
- Lesson applied: wire interop with a real daemon is proven **first**, before
  any UI work.

## Decisions (with rationale)

| Decision | Choice | Rationale |
|---|---|---|
| Browser transport | WebSocket primary, WebTransport secondary | WT still needs a feature flag in Safari and cannot traverse `tailscale serve`/reverse proxies (QUIC/UDP). WS works everywhere and proxies cleanly. WT stays for direct-to-tailnet-IP use. |
| SPA serving | Daemon-served *and* standalone static | Daemon serving gives one-URL mobile UX behind `tailscale serve`; the build remains plain static files deployable anywhere. |
| Auth (v1) | Network identity + `tailscale serve` identity headers | No login UI. Loopback → anonymous (unchanged). `--auth tailscale` whois for direct connections (unchanged). New: trust `Tailscale-User-*` headers from loopback peers when enabled. `meta.authenticate` stays unimplemented. |
| JS tooling | Node LTS + Vite + SvelteKit (`adapter-static`), Svelte 5, TypeScript, vitest | v1's Deno choice hit SvelteKit/Vite compat issues. Boring wins. |
| Terminal | `@wterm/dom` + `@wterm/ghostty` (0.3.x) | DOM rendering (native selection, virtual keyboard on mobile), libghostty-vt WASM core matching the daemon's server-side parser. |
| Daemon HTTP stack | axum | Idiomatic tower ecosystem; WS upgrade, static serving, and header extraction are all first-class. |
| Isolation | Web serving lives in cairn-daemon (not a separate binary) | Re-evaluate later; tracked in architecture docs. |

## Daemon changes

### Listener schemes

`--listen` scheme names the transport, in the form a client dials:

| `--listen` value | Transport |
|---|---|
| `unix://path`, bare path, `unix` | UDS (unchanged) |
| `https://host:port` | WebTransport/QUIC (unchanged — the WebTransport spec dials `https://` URLs; a `wt://` scheme was tried historically and reverted) |
| `ws://host:port` | **New.** WebSocket: an axum HTTP server with wRPC at `/ws` and `/healthz` |
| `wss://host:port` | Reserved for future daemon-terminated TLS WS; rejected with a clear error (unchanged) |

Multiple `--listen` flags compose, as today. The `ws://` listener is plain
HTTP by design: its intended postures are loopback-behind-`tailscale-serve`
(remote access, real certs, identity headers) or plain localhost (dev).

### WebSocket wRPC endpoint

Each accepted WebSocket at `/ws` carries exactly **one wRPC invocation**
(the JS SDK's model: one duplex byte stream per invocation, sub-streams
multiplexed inside via the frame protocol). Server side follows the
`wrpc-websockets` loopback-test pattern: `wrpc_websockets::split(ws)` →
`wrpc_transport::frame::Server::accept(tx, rx)`, reusing the daemon's
existing generic serve loop (`serve/wrpc.rs`). Unary RPCs open-and-close;
`attach`/`logs`/`wait` hold their connection for the stream's lifetime.
Binary frames carry data; an empty text frame is the EOF sentinel
(the `wrpc-websockets` convention).

### Web UI serving (orthogonal to transports)

- `--web-ui` (bare): attaches SPA routes to every `ws://` listener's HTTP
  server. Error at startup if no `ws://` listener exists.
- `--web-ui=host:port`: binds a dedicated HTTP listener serving only the SPA
  (no `/ws`); in this form SPA routes attach only to that listener, not to any
  `ws://` listeners. Valid with only a `https://` (WT) listener — the page
  always travels over HTTP/TCP, but the wRPC transport it then uses can be
  WebTransport.
- `--web-dir <path>`: serve SPA assets from a directory instead of the
  embedded copy (works with both forms, and without the embed feature).
- SPA assets are embedded at compile time behind a new cargo feature
  `web-ui` (off by default so `cargo build` never requires npm; enabled for
  release/nix builds). Unknown paths fall back to `index.html` (SPA routing).

### `/cairn.json` — server-defined client config

Served alongside the SPA (and on `ws://` listeners regardless of `--web-ui`):
a JSON document telling web clients what to dial. v1 contents:

```json
{
  "endpoints": {
    "websocket": "/ws",
    "webtransport": { "url": "https://host:4433", "certHash": "<hex SPKI>" }
  }
}
```

Each key present only when that listener is configured. This also delivers the
WT `serverCertificateHashes` value that previously required manual copy-paste
from `runtime_dir()/cert-hash`. `/cairn.json` is served with
`Access-Control-Allow-Origin: *` (its contents are public — endpoints and a
cert fingerprint), so a standalone-hosted UI can bootstrap from a pasted
daemon URL instead of manual per-field configuration. The name is deliberately a general
"daemon-to-web-client config" document — future occupants include plugin
manifests (which web-component bundles to load).

### Identity & auth

- New `TransportContext::Http { peer_addr, headers }` variant; the existing
  `AuthChain` runs at WebSocket upgrade time (and the identity attaches to the
  connection, as WT does today).
- New `tailscale-serve` auth backend (`--auth tailscale-serve`): reads
  `Tailscale-User-Login` (+ display name) from upgrade headers. Trusted
  **only when `peer_addr` is loopback** — only the local `tailscaled` should
  originate these. Composes with the existing chain (first success wins).
- Unchanged fallback: no chain + loopback → `Identity::Anonymous`; no chain +
  non-loopback → rejected.
- **Origin validation (cross-site WebSocket hijacking defense).** Because
  loopback peers get anonymous access, any website open in a local browser
  could otherwise dial `ws://localhost:4180/ws` and drive sessions. WebSocket
  upgrades (and `/ws` only — not the SPA or `/cairn.json`) enforce: requests
  with an `Origin` header must match one of the daemon's own serving origins
  (the request's `Host`) or an entry in a repeatable `--ws-origin <origin>`
  allowlist (used when the SPA is hosted standalone); requests without
  `Origin` (non-browser clients) pass. Mismatched origins are rejected before
  the upgrade completes.

### Dependency changes

- Bump workspace pins: `wrpc-transport 0.28 → 0.29`, `wrpc-transport-web
  0.2 → 0.3`, plus whatever `wit-bindgen-wrpc` bump those require. The JS SDK
  implements the current frame protocol (version byte 0) — interop is
  validated by test, not assumed.
- Add `wrpc-websockets` as a **git dependency pinned to a commit**
  (unpublished on crates.io). The JS SDK is pinned to the same wrpc commit.
- `axum` (and `tokio-websockets` transitively) behind the new listener.

## Web client — `cairn-web/`

Lives at the repo root (sibling of `crates/`). Node + Vite + SvelteKit.

```
cairn-web/src/lib/
├── protocol/          # plain TS, zero framework deps — the future plugin API surface
│   ├── wit.ts         # t.* type descriptors mirroring cairn.wit (~100 lines of data)
│   ├── types.ts       # TS interfaces mirroring WIT types (SessionInfo, ServerEvent, …)
│   ├── transport.ts   # Dialer = () => Promise<Transport>; one duplex per invocation
│   ├── ws.ts          # WebSocket dialer (native WebSocket → Transport adapter)
│   ├── wt.ts          # WebTransport dialer (one session, one bidi stream per invocation)
│   └── client.ts      # DaemonClient: typed sessions.* / meta.* methods over invoke()
├── stores/            # thin Svelte 5 runes wrappers: connection, session list, session
└── components/        # Terminal.svelte, SessionList, SessionDetail, CreateSession, …
```

- **All encoding belongs to the SDK.** `wit.ts` is declarative descriptors
  (`t.record`, `t.variant`, `t.stream`, …) — data, not code.
- **`DaemonClient` is stateless per invocation** and takes a `Dialer`.
  Endpoint selection: fetch `/cairn.json` → prefer same-origin WS → fall back
  to WT → else a manual endpoint screen for standalone static hosting
  (persisted in localStorage, overridable via `?endpoint=`). The manual screen
  accepts a daemon base URL — the client fetches that host's `/cairn.json`
  (CORS-open, above) — or a direct `ws://`/`wss://` URL; a WT endpoint needs
  the cert-hash field only when the daemon uses its self-signed cert.
- **Reconnect lives in the stores layer**, not `DaemonClient`: dial failures
  and dropped attach streams flip connection state; exponential backoff with
  jitter capped at 10s; on recovery, re-fetch the session list and re-attach
  active terminals.
- **Routes:** `/sessions` (list), `/sessions/:id` (detail + terminal),
  `/sessions/new` (create form). Same views, fields, and responsive behavior
  as the v1 spec (cards on narrow viewports, full-viewport terminal on
  mobile, stacked create form with collapsed advanced section).
- **`<cairn-terminal>` web component:** `Terminal.svelte` compiled via
  Svelte `customElement`. Contract as in the v1 spec: attributes
  `session-id`/`endpoint`/`font-size`/`font-family`, properties
  `.client`/`.sessionId`, events `cairn-attached`/`cairn-detached`/
  `cairn-exited`. Shared-client mode (`.client`) and standalone mode
  (`endpoint`).
- **SDK dependency:** vendored into `cairn-web/vendor/` as an `npm pack`
  tarball from the wrpc checkout (npm cannot install a git dependency from a
  repo subdirectory, and the package lives at `wrpc/js/`). A note in the
  vendor dir records the source commit, which matches the daemon's
  `wrpc-websockets` git pin. Revisit once the package is published to npm.

## Terminal & attach flow

1. Mount → measure container → `client.attach(id, {cols, rows, noStdin},
   clientEvents)` over a dedicated WS connection (or WT bidi stream), so the
   PTY is right-sized from the first byte.
2. First server event is always `snapshot` (daemon guarantee) → initial
   screen. `output` batches append. `exited` → exit-status overlay. In-band
   `error` (`client.kicked`, `client.lagged`) → toast/overlay with reattach
   action.
3. Keystrokes → `input`; `ResizeObserver` → debounced (~100ms) `resize`;
   unmount/navigation → push `detach`, close the event queue, ending the
   invocation and the socket.

**Backpressure rules (the CPU-spike lesson).** The daemon already throttles
its side (cap-2 channel feeding the wRPC stream encoder, `attach.rs`).
Client-side: no polling loops anywhere — everything awaits SDK async
iterables; socket writes respect backpressure (await `bufferedAmount` drain)
rather than spin. The v1 spike was never root-caused, so it is treated as a
regression class to test for, not an assumption that the SDK fixes it.

## Testing

1. **Wire interop gate (first milestone, before any UI):** integration suite
   driving the official JS SDK against a real `cairn-daemon` over `ws://` —
   every unary method, `logs`, `wait`, and a full `attach` round-trip
   (snapshot → input → echoed output → detach).
2. **Daemon integration tests** (existing `DaemonHarness` pattern): WS bind +
   upgrade, identity-header auth paths (loopback trust, non-loopback
   rejection), `/cairn.json` contents per listener combination, SPA fallback,
   `--web-ui` flag validation errors.
3. **Client unit tests** (vitest): stores, reconnect/backoff state machine,
   endpoint selection logic.
4. **CPU regression check:** in the JS↔daemon integration suite, assert the
   daemon process sits at ~idle CPU while a web client stays attached to a
   quiet session for a sampling window.
5. **Browser E2E:** manual for v1 (chrome-devtools during development); not
   CI-automated.

## Out of scope for v1

- Plugin loader / manifest / discovery UI (groundwork only: `/cairn.json`,
  `<cairn-terminal>`, framework-free `protocol/` layer)
- Token auth (`meta.authenticate` remains unimplemented) and any login UI
- Daemon-terminated TLS for WS (`wss://`)
- Multi-session views (splits, tabs)
- Logs-only view; session restart UI
- Automated browser E2E in CI

# cairn-web

A browser interface for [Cairn](../README.md) PTY sessions: session list,
create, inspect, and a ghostty-compatible terminal attach — plus a standalone
`<cairn-terminal>` web component for embedding a single terminal in any page.

Node + Vite + SvelteKit (`adapter-static`), Svelte 5, TypeScript, Vitest. See
`docs/superpowers/specs/2026-07-11-web-ui-v2-design.md` for the full design
rationale.

## Development

```sh
npm install
npm run dev             # vite dev server
npm run build            # production SPA build -> build/
npm run build:element    # <cairn-terminal> web component bundle -> dist-element/
npm run check             # svelte-kit sync + svelte-check (types)
npm test                  # vitest (unit tests, plain TS + Svelte stores)
npm run test:integration  # vitest against a real cairn-daemon (see tests/integration/)
```

## Deployment: daemon-served vs standalone

The SPA build (`npm run build` -> `build/`) is plain static files. It can be
served two ways:

### 1. Daemon-served (the common case)

The daemon serves the SPA itself, alongside its own `/ws` WebSocket endpoint,
on the same origin — no CORS, no origin allowlisting needed for same-origin
browser traffic:

```sh
cairn-daemon \
  --listen ws://127.0.0.1:8080 \
  --web-ui \
  --web-dir /path/to/cairn-web/build
```

Bare `--web-ui` attaches the SPA (and `/cairn.json`) to every `ws://`
listener's own HTTP server; error at startup if none exist. Use
`--web-ui=host:port` instead to bind a *dedicated* HTTP listener serving only
the SPA (no `/ws` on that port) — valid even with only a WebTransport
(`https://`) listener configured, e.g.:

```sh
cairn-daemon \
  --listen https://127.0.0.1:4433 \
  --web-ui=127.0.0.1:5173 \
  --web-dir /path/to/cairn-web/build
```

If you pair a dedicated `--web-ui=host:port` listener with a `ws://` listener,
the SPA (served on the UI port) reaches `/ws` cross-origin — a different port
on the same host. The daemon auto-allows that origin: an `Origin` whose host
matches the `/ws` request's own `Host` and whose port is a dedicated UI
listener's port passes the allowlist with **no `--ws-origin` needed**. This
holds only for the daemon's own UI listener ports on the same host; a UI hosted
elsewhere (different host, or standalone static hosting — see below) still
requires `--ws-origin`.

`--web-dir` works with or without the daemon's `web-ui` cargo feature
(compiled-in asset embed); `web-dir` is the simplest way to iterate on the
build without rebuilding the daemon binary.

### 2. Standalone static hosting

The same `build/` output can be hosted anywhere static files are served
(nginx, an S3 bucket, `python3 -m http.server`, a CDN) with **no daemon
involvement in serving the page**. On load, the app tries, in order:

1. `?endpoint=<ws-url>` query override.
2. Same-origin `/cairn.json` (present only if *this* origin happens to also
   be a daemon — usually not, for a standalone deployment).
3. A previously-persisted endpoint (`localStorage`).
4. The manual-entry screen (see below).

Because the standalone build's origin is *not* the daemon's origin, the
browser's WebSocket connection to the daemon is cross-origin. The daemon
enforces an Origin allowlist on `/ws` upgrades specifically to prevent
malicious pages from silently driving sessions over a loopback-trusted
connection — **you must pass `--ws-origin`** naming the origin(s) the
standalone build is served from:

```sh
cairn-daemon \
  --listen ws://127.0.0.1:8080 \
  --ws-origin https://cairn-ui.example \
  --ws-origin http://localhost:5500   # e.g. a local static-file preview
```

`/cairn.json` itself is always served with `Access-Control-Allow-Origin: *`
(its contents — endpoint URLs and a cert fingerprint — are not sensitive),
so the manual-entry screen's "Daemon URL" bootstrap works cross-origin
without any allowlisting; only the actual `/ws` WebSocket upgrade is origin
-checked. WebTransport connections aren't subject to this allowlist at all
(no `Origin` enforcement on that path today).

### Manual endpoint screen

Shown whenever discovery doesn't resolve an endpoint (no same-origin
`/cairn.json`, nothing persisted). Three independent ways to connect,
whichever succeeds is saved to `localStorage` for next time:

- **Daemon URL** — a `host:port` (or full `http(s)://` URL) the daemon's
  `ws://` listener (or dedicated `--web-ui=host:port` listener) is reachable
  on. Its `/cairn.json` is fetched and the normal WS-preferred/WT-fallback
  selection applies, resolved against *that* origin.
- **Direct WebSocket URL** — skip discovery, dial a `ws://`/`wss://` URL
  directly.
- **WebTransport endpoint** — an `https://` URL, with an optional cert-hash
  field. The hash is only needed for a daemon presenting its
  daemon-generated self-signed certificate (find it in that daemon's
  `/cairn.json` under `endpoints.webtransport.certHash`, or in the hex
  contents of `cert-hash` under the daemon's runtime directory —
  `$XDG_RUNTIME_DIR/cairn` or `$TMPDIR/cairn`). Leave it blank for a
  CA-signed certificate.

A "Change endpoint" control (next to the connection indicator, once
connected) clears the persisted endpoint and returns to this screen — useful
for pointing the same standalone build at a different daemon. `?endpoint=`
always takes priority over everything above, including a persisted value.

## `<cairn-terminal>` web component

`Terminal.svelte` (the same component the SvelteKit app uses for
`/sessions/:id`) is additionally compiled to a standalone custom element,
`<cairn-terminal>`, via a separate Vite build target — `npm run
build:element`, entry point `src/lib/webcomponent/CairnTerminalElement.svelte`,
config `vite.element.config.ts`. It has zero SvelteKit/routing dependency and
can be dropped into any HTML page.

### Build output

```sh
npm run build:element
```

produces `dist-element/`:

- `cairn-terminal.js` — the component (ES module). Self-registers
  `customElements.define('cairn-terminal', ...)` on load — no manual
  registration call needed. Ghostty's WASM core is bundled inline as a
  base64 data URL (Vite's library-mode asset inlining), so there is no
  separate `.wasm` file to host or configure MIME types for.
- `cairn-terminal.css` — wterm's terminal-rendering stylesheet (grid layout,
  cursor, selection). This is a plain (non-Svelte) stylesheet import, so it
  ships as a companion file rather than being inlined into the JS; include it
  as a `<link>` alongside the script tag. (The component's *own* styles —
  layout, overlays, buttons — are embedded in the JS and self-inject at
  runtime, no separate file needed for those.)

A consuming page needs both files:

```html
<script type="module" src="/path/to/cairn-terminal.js"></script>
<link rel="stylesheet" href="/path/to/cairn-terminal.css" />
```

`<cairn-terminal>` renders into the light DOM (no shadow root) — this is
required so the plain `cairn-terminal.css` (which targets the document, not a
specific shadow tree) actually applies to the terminal's grid. Give it an
explicit size (e.g. `height: 400px` or a flex/grid item) — like
`Terminal.svelte`, it fills whatever box its container provides.

### Usage — standalone mode

For simple embeds without an existing daemon connection: set `endpoint`
directly on the element. Scheme selects the transport, matching the daemon's
own `--listen` convention (`ws://`/`wss://` = WebSocket, `https://` =
WebTransport):

```html
<cairn-terminal
  session-id="01936f8a-1234-7abc-9def-0123456789ab"
  endpoint="ws://localhost:8080/ws"
  style="height: 400px; display: block"
></cairn-terminal>
```

For a WebTransport endpoint with a self-signed certificate, build a
`DaemonClient` yourself (with `wtDialer(url, certHash)`) and use shared-client
mode instead — the bare `endpoint` attribute has no cert-hash field (see
below).

### Usage — shared-client mode

When the embedding code already holds a `DaemonClient` (or any object
duck-typed to its `attach()` method — see `src/lib/protocol/client.ts` and
`src/lib/terminal/attachController.ts`'s `AttachClient` interface) — e.g. a
future plugin sharing the core app's own connection — pass it directly
instead of letting the element dial its own:

```js
const el = document.createElement('cairn-terminal');
el.client = existingDaemonClient; // an instance of DaemonClient from '$lib/protocol'
el.sessionId = '01936f8a-1234-7abc-9def-0123456789ab';
el.style.height = '400px';
container.appendChild(el);
```

`.client` takes priority over the `endpoint` attribute if both are set. This
mode has no build-time dependency of its own beyond `cairn-terminal.js`/
`.css` — it's a plain property assignment — but constructing a real
`DaemonClient` today means importing `cairn-web`'s protocol layer
(`src/lib/protocol/`), which isn't yet published as a standalone package (see
the design spec's "Out of scope" — a published protocol package is future
plugin-ecosystem groundwork).

### Attributes / properties / events

| Attribute | Property | Type | Description |
|---|---|---|---|
| `session-id` | `.sessionId` | string | Required. The session's id to attach to. |
| `endpoint` | `.endpoint` | string | Standalone mode: `ws://`/`wss://`/`https://` daemon URL. Ignored if `.client` is set. |
| `font-size` | `.fontSize` | number | Terminal font size in px. |
| `font-family` | `.fontFamily` | string | CSS font-family string. |
| — | `.client` | `DaemonClient` | Shared-client mode: an already-connected client. Practically usable only as a property (a `DaemonClient` instance can't round-trip through an HTML attribute string) — though Svelte's custom-element wrapper still lists `client` in `observedAttributes` for every declared prop, so a literal `client="..."` attribute is technically observed too; it just can't carry anything useful. |

| Event | `detail` | Fires when |
|---|---|---|
| `cairn-attached` | `{ sessionId }` | The attach stream established (first snapshot received). |
| `cairn-detached` | `{ sessionId, reason }` | The attach ended — `reason` is `"unmount"` (element removed from the DOM; fires on removal regardless of whether the session had already exited), `"disconnected[:message]"` (stream dropped unexpectedly), or `"error:<code>"` (an in-band `kicked`/`lagged` event from the daemon). |
| `cairn-exited` | `ExitStatus` (`{ code?, signal?, unixMs, reason? }`) | The session's process exited. |

Changing `session-id` (or switching between shared/standalone mode) after
the element is already attached forces a clean re-attach (a fresh
`AttachController`, not an in-place reconfiguration).

## Origin allowlist quick reference

| Scenario | Need `--ws-origin`? |
|---|---|
| Daemon-served SPA (page and `/ws` on the same origin) | No — same-origin requests are always allowed. |
| Dedicated `--web-ui=host:port` listener + a `ws://` listener on the same host | No — the UI listener's origin (same host, its own port) is auto-allowed on `/ws`. |
| Standalone-hosted SPA, different origin than the daemon | Yes — add the standalone build's serving origin(s). |
| Plain-HTML page embedding `<cairn-terminal endpoint=...>`, different origin than the daemon | Yes — same rule; the browser sends the page's origin on the WebSocket upgrade regardless of whether a full SvelteKit app or a bare custom element initiated it. |
| Non-browser clients (the `cairn` CLI, WebTransport) | N/A — no `Origin` header (CLI/UDS) or no origin enforcement on this path (WebTransport) today. |

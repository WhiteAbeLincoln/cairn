# Cairn Web UI

A statically-served web interface for managing Cairn PTY sessions, built with a
modular architecture that supports future plugin UI components.

## Goals

- Provide session management (list, create, inspect, kill, rename) and terminal
  attach from a browser
- Mobile-friendly from day one — Cairn's core value prop includes phone-based
  access to remote agents
- Statically deployable — no server-side rendering, serve from any CDN or file
  server
- Modular component boundaries that align with the plugin system vision (Web
  Components, composable `<cairn-terminal>`)
- Framework-agnostic API layer so future plugins don't depend on the core app's
  UI framework

## Stack

| Layer | Choice | Rationale |
|---|---|---|
| Framework | Svelte 5 + SvelteKit | Compiles away (minimal runtime), `customElement` compilation to Web Components, file-based routing, `adapter-static` for static output |
| Routing | SvelteKit `adapter-static` | HTML5 History API, clean URLs (`/sessions/:id`), static fallback page for SPA |
| JS Runtime | Deno | Dev/build only. Deno 2.x runs Vite + SvelteKit via Node compat. May need Kit version pin (known regression in 2.21.1 with Deno builds) |
| Transport | WebTransport (browser API) | Direct connection to daemon's existing WebTransport listener. ~85% browser support (Chrome, Edge, Firefox, Safari). No fallback transport in v1 |
| Wire Protocol | wRPC, hand-written TypeScript codec | 12 methods across 2 WIT interfaces — small enough to hand-write. Codec encapsulated behind `DaemonClient` so it can be swapped for codegen later as WIT surface grows |
| Terminal | wterm with `@wterm/ghostty` | DOM-based rendering — native text selection, browser find, screen reader support. libghostty-vt WASM backend for full VT compliance, matching the daemon's server-side parser. Swappable via `<cairn-terminal>` abstraction |
| Auth (v1) | Tailscale identity | No login UI. Daemon's `authenticate_network(peer_addr)` handles it. Auth abstraction in the client layer for future token-based auth |

## Project Structure

`cairn-web/` lives at the repo root as a sibling to `crates/`, since it's a
Deno/Node project, not a Rust crate.

```
cairn-web/
├── src/
│   ├── lib/
│   │   ├── client/              # DaemonClient (plain TypeScript, zero framework deps)
│   │   │   ├── client.ts        # WebTransport connection, reconnection, lifecycle
│   │   │   ├── sessions.ts      # sessions interface methods
│   │   │   ├── meta.ts          # meta interface methods
│   │   │   ├── auth.ts          # auth abstraction (Tailscale no-op now, token later)
│   │   │   ├── codec.ts         # wRPC frame encoding/decoding
│   │   │   └── types.ts         # TypeScript types mirroring WIT
│   │   ├── stores/              # Thin Svelte 5 runes wrappers
│   │   │   ├── sessions.svelte.ts
│   │   │   └── connection.svelte.ts
│   │   └── components/          # Svelte components
│   │       ├── Terminal.svelte   # wraps wterm, compilable to <cairn-terminal> WC
│   │       ├── SessionList.svelte
│   │       ├── SessionDetail.svelte
│   │       └── CreateSession.svelte
│   ├── routes/
│   │   ├── +layout.svelte       # app shell, connection init, responsive layout
│   │   ├── +page.svelte         # redirects to /sessions
│   │   └── sessions/
│   │       ├── +page.svelte     # session list
│   │       ├── [id]/
│   │       │   └── +page.svelte # session detail + terminal
│   │       └── new/
│   │           └── +page.svelte # create session form
│   └── app.html
├── static/
├── svelte.config.js
├── vite.config.ts
├── tsconfig.json
├── deno.json
└── package.json
```

### Key Boundaries

**`lib/client/`** — Zero framework dependencies. Plain TypeScript. Owns the
WebTransport connection, auth negotiation, reconnection logic, and typed methods
mirroring the WIT `sessions` and `meta` interfaces. This is the API surface
that future plugins will use. Could be published as a standalone npm package.

**`lib/stores/`** — Thin Svelte 5 runes wrappers (~10 lines each) that adapt
`DaemonClient` methods into reactive `$state` primitives for the SvelteKit app.
Internal to the core UI, not part of the plugin API.

**`lib/components/`** — Svelte components. `Terminal.svelte` wraps wterm and can
be compiled to a `<cairn-terminal>` custom element via Svelte's `customElement`
option.

**`routes/`** — SvelteKit file-based routing. Minimal logic, delegates to
components.

## Architecture

### Hybrid: Service Core + Svelte Wrappers

A framework-agnostic `DaemonClient` class (plain TypeScript) handles
WebTransport and exposes the WIT API. Thin Svelte wrapper stores adapt the
service into reactive primitives for the SvelteKit app. Plugins get the raw
service; the core app gets reactive convenience.

```
Routes (SvelteKit pages)
  │
  ▼
Svelte Stores (lib/stores/)
  useSessionList()  useSession(id)  connection
  │
  ▼
DaemonClient (lib/client/)
  Framework-agnostic TypeScript
  sessions.list()  sessions.attach()  meta.version()
  │
  ▼
WebTransport + wRPC
  │
  ▼
cairn-daemon (wRPC server)
```

### Connection Lifecycle

1. App mounts — `+layout.svelte` initializes `DaemonClient`
2. `DaemonClient` opens WebTransport to daemon endpoint
   - Endpoint URL from `CAIRN_DAEMON_URL` env var (baked at build time via
     Vite's `import.meta.env`) or overridable at runtime via `?endpoint=` query
     param (useful for development)
   - `serverCertificateHashes` for self-signed TLS (hash exported by daemon to
     `$TMPDIR/cairn/cert-hash`, entered manually or via query param for dev)
3. Auth abstraction runs:
   - v1 (Tailscale): no-op — network identity is sufficient
   - Future: call `meta.authenticate(token)`
4. Connection state exposed as `ConnectionState`:
   `'connecting' | 'connected' | 'error' | 'disconnected'`
5. On disconnect: auto-reconnect with exponential backoff
   (100ms → 200ms → 400ms → ... capped at 10s)
6. On reconnect: re-fetch session list, re-attach active terminals

### Streaming Operations

WIT `stream` and `future` types map to browser APIs:

| WIT Type | Browser Type | Used By |
|---|---|---|
| `stream<server-event>` | `ReadableStream<ServerEvent>` | attach (output) |
| `stream<client-event>` | `WritableStream<ClientEvent>` | attach (input) |
| `stream<list<u8>>` | `ReadableStream<Uint8Array>` | logs |
| `future<exit-status>` | `Promise<ExitStatus>` | wait |
| Non-streaming RPCs | `Promise<T>` | list, inspect, create, rename, kill, kick |

## `<cairn-terminal>` Web Component

`Terminal.svelte` compiles to a `<cairn-terminal>` custom element. Two usage
modes:

**Shared client mode** (core app and plugins with DaemonClient access):
```js
const el = document.createElement('cairn-terminal');
el.client = daemonClient;
el.sessionId = '01936f8a-...';
container.appendChild(el);
```

**Standalone mode** (simple embeds without existing connection):
```html
<cairn-terminal
  session-id="01936f8a-..."
  endpoint="https://daemon.tailnet:4433"
></cairn-terminal>
```

### Attributes (HTML-friendly, string-based)

| Attribute | Description |
|---|---|
| `session-id` | Required. UUIDv7 of the session to attach |
| `endpoint` | Daemon WebTransport URL (optional if `.client` property set) |
| `font-size` | Terminal font size in px |
| `font-family` | CSS font-family string |

### Properties (JS-friendly, typed)

| Property | Type | Description |
|---|---|---|
| `.client` | `DaemonClient` | Shared connection instance (skips internal connection) |
| `.sessionId` | `string` | Same as `session-id` attribute |

### Events

| Event | Detail | Fires When |
|---|---|---|
| `cairn-attached` | `{ sessionId }` | Attach stream established |
| `cairn-detached` | `{ sessionId, reason }` | Detach (navigation, explicit, error) |
| `cairn-exited` | `ExitStatus` | Session process exits |

### Behavior

- If `.client` is set, uses it. Otherwise creates its own `DaemonClient` from
  the `endpoint` attribute.
- Initializes wterm with `@wterm/ghostty` backend on `connectedCallback`.
- Attaches to session, feeds `snapshot` + `output` server events to wterm.
- Sends keystrokes (`input`) and container resize (`resize`) events back.
- Uses `ResizeObserver` on host element for resize events.
- On `disconnectedCallback`: sends `detach`, tears down wterm instance.

## UI Views

### Session List — `/sessions`

Table/card view of all sessions from `sessions.list-all()`.

Each entry shows:
- Status indicator (running / exited with code 0 / exited with non-zero)
- Session name (or truncated command if unnamed)
- Command basename
- Attached client count
- Relative creation time
- Exit code (if exited)

Click navigates to `/sessions/:id`. "+ New Session" button links to
`/sessions/new`.

**Responsive**: card layout on narrow viewports with tap-friendly targets,
table rows on wider screens.

### Session Detail — `/sessions/:id`

Terminal view with session metadata.

- Header: back link, session name, action buttons (Kill, Rename)
- Terminal: `<cairn-terminal>` fills available viewport height
- Metadata bar below terminal: PID, created time, attached client count
- If session has exited: overlay on terminal with exit status and exit code

**Responsive**: terminal goes full-viewport on mobile with minimal chrome.
Touch input triggers virtual keyboard via wterm's DOM rendering.

### Create Session — `/sessions/new`

Form for `sessions.create()`.

Primary fields (always visible):
- Name (optional)
- Command (required)
- Working directory (optional)

Advanced section (collapsed by default):
- Environment variables (key=value pairs)
- Inherit environment (checkbox, default true)
- Scrollback lines (number)
- Idle timeout (seconds, optional)
- TTY (checkbox, default true)
- Stdin (checkbox, default true)

On create: navigates to `/sessions/:id` for the new session.

**Responsive**: stacked layout, large touch targets, advanced section stays
collapsed.

## Plugin Integration (Future — Not Built in v1)

v1 establishes the patterns that plugins will use:

1. **`DaemonClient` as the plugin API surface** — plugins receive a
   `DaemonClient` instance (plain TS class). No Svelte dependency required.
   Works in any framework or vanilla JS Web Component.

2. **`<cairn-terminal>` as a composable primitive** — plugins embed it like any
   HTML element. Two modes: pass `.client` (shared connection) or set `endpoint`
   (standalone connection).

3. **Custom element registration** — core registers `cairn-terminal`. Future
   plugins register their own (`cairn-chat`, `cairn-cost-dashboard`, etc.),
   prefixed with `cairn-` or the plugin name.

v1 does **not** build:
- Plugin loader / manifest system
- Plugin route mounting
- Plugin discovery UI
- Inter-plugin communication

## wRPC TypeScript Codec

Hand-written TypeScript implementation of wRPC frame encoding/decoding, guided
by the `wrpc-transport-web` Rust crate. Covers:

- wRPC frame serialization/deserialization
- WIT type mapping (records, variants, enums, options, results, lists)
- Stream and future wire format
- WebTransport bidirectional stream management for attach/logs/send

The codec is encapsulated inside `DaemonClient` — the public API is typed
TypeScript methods, not raw frames. This allows swapping to a codegen-based
implementation later as the WIT surface grows for plugins, without changing the
`DaemonClient` API.

The wRPC JS/TS ecosystem is immature, particularly for WebTransport. This codec
work may be worth upstreaming to the wRPC project once stabilized.

## Responsive Design

Mobile-first from the start. Key considerations:

- **Viewport**: proper `<meta name="viewport">` for no zoom-fighting on mobile
  Safari
- **Session list**: card layout on narrow screens, table on wide
- **Terminal**: full-viewport on mobile, minimal surrounding chrome
- **Touch input**: wterm's DOM rendering triggers virtual keyboard on tap
- **Navigation**: bottom nav or hamburger on mobile, sidebar/topbar on desktop
- **Create form**: stacked layout, large touch targets
- **Connection indicator**: always visible, compact on mobile

## Out of Scope for v1

- Plugin infrastructure (loader, routes, discovery, inter-plugin communication)
- Settings / configuration UI
- Multi-session views (split panes, tabs)
- WebSocket fallback transport
- Token-based authentication UI (auth abstraction exists, login flow does not)
- Logs-only view (streaming `sessions.logs()` without terminal attach)
- Session restart UI

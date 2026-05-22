# Authentication and Authorization

This is the area where cairn diverges most sharply from zmx. zmx leans
entirely on POSIX filesystem permissions for both authentication and
authorization; cairn cannot, because a browser cannot present a file
descriptor to a Unix socket as proof of identity. Everything else in
[[external-protocol]], [[client-attach-and-election]], and
[[internal-communication]] assumes the connection is already trusted —
this doc is about how it becomes trusted.

## zmx baseline: filesystem permissions are the auth model

zmx has **no explicit authentication code**. There is no token, no
handshake credential, no `SO_PEERCRED` check on `accept()`. The accept
path at `src/main.zig:2526-2545` `accept(server_sock_fd, null, null, ...)`
deliberately passes a `null` `sockaddr` and never reads peer credentials —
once the TCP-equivalent of "ESTABLISHED" lands, the new fd is appended
to `daemon.clients` and the protocol begins.

Authentication is delegated to the kernel via the socket directory:

| Layer                | Mechanism                                                                                              | Source                                              |
|----------------------|--------------------------------------------------------------------------------------------------------|-----------------------------------------------------|
| Directory location   | `$XDG_RUNTIME_DIR/zmx/` (default), or `$ZMX_DIR`, or `$TMPDIR/zmx-$UID`                                 | `Cfg.socketDir` at `src/main.zig:504-517`           |
| Directory mode       | `0o750` (owner full, group read+exec, world none); overridable via `$ZMX_DIR_MODE`                     | `Cfg.dir_mode` at `src/main.zig:474`, `mkdir:524`   |
| Socket file mode     | Inherited from process `umask`. zmx never `chmod`s or sets `umask` itself                              | (absence — `grep umask\|chmod` finds nothing)       |
| Session-name safety  | Reject `/`, NUL, `.`, `..` so a malicious name can't escape the socket directory                       | `getSeshName` at `src/socket.zig:18-27`             |

`$XDG_RUNTIME_DIR` on Linux is created by systemd-logind as
`/run/user/$UID` mode `0o700`, owned by the user — the surrounding
directory is **already** unreachable to other users. zmx's `0o750`
inside that runtime dir is belt-and-suspenders. The `/tmp/zmx-$UID`
fallback is the weak link: `0o750` is the *only* gate because `/tmp`
itself is world-readable.

The practical model is: **if your UID can `connect(2)` the socket, you
are zmx**. There is no separate identity or revokable credential. Any
process running as the daemon's user can attach to any session, send
`Kill` (`ipc.zig:13`, `main.zig:2675-2680`), inject `Write`
(path-traversal-checked at `main.zig:2092-2100` but otherwise
unrestricted), or read scrollback. Root can always attach (POSIX DAC).
There is no second factor.

### Authorization in zmx: there isn't any

Once attached, every client can do everything. `handleMessage`
(`main.zig:2660-2700`) dispatches purely on `Tag`; the only stateful
gating is **leader election** (see [[client-attach-and-election]] and
[[resize-semantics]]), which is about *who controls the size*, not
*who is permitted to send what*. Non-leaders can still send `Input`,
`Kill`, `DetachAll`, `Write`, `Run`. zmx is explicitly single-user,
single-trust-domain.

## Why this doesn't port

Move the listener from `AF_UNIX` to `AF_INET` (loopback, ws://) and the
kernel's authorization story evaporates:

1. **No file-permission gate.** TCP listens are not protected by a path
   the kernel can DAC-check; any local process — of any user — can
   `connect(2)` to `127.0.0.1:PORT`. Including user-namespaced
   containers and processes running as `nobody`.
2. **No `SO_PEERCRED` analogue.** That `getsockopt` works only on
   `AF_UNIX`; TCP gives us peer IP and port, nothing about UID or PID.
3. **Browsers are now in scope.** Any HTTP page the user visits can
   attempt `new WebSocket("ws://localhost:PORT/...")` — cross-site
   WebSocket hijacking (CSWSH). Same-origin policy does **not** apply
   to WebSockets; CORS doesn't either. The browser will happily make
   the connection and forward any cookies for `localhost` it has.
4. **The `Hello` frame from [[external-protocol]] arrives *after* the
   socket is open.** By that point a malicious peer has already opened
   the WS and (without auth) is a client.

## Cairn v0 scheme

A single mechanism that covers both browser and CLI clients, with a
Unix-socket fallback for CLI when present.

### Bind: loopback only

The cairn HTTP/WS listener binds **only** to `127.0.0.1` (and `::1`),
never to `0.0.0.0`. This is a hard rule, not a configurable. Exposing
the daemon on a routable interface is out of scope for v0 and would
need mTLS / a reverse proxy — see Open Questions.

### Token: per-daemon bearer token

On first start the daemon generates 32 bytes from a CSPRNG and writes
them base64url-encoded to `$XDG_RUNTIME_DIR/cairn/token` (or
`$XDG_STATE_HOME/cairn/token`, falling back to
`$HOME/.local/state/cairn/token`). The containing directory is created
`0o700` and the file `0o600`. The same `$XDG_RUNTIME_DIR/cairn/`
directory mirrors zmx's `0o750`-style gate (`src/main.zig:474`),
tightened to `0o700` because we are the only writer and there is no
Unix-socket sharing story to preserve.

Token presentation: **first-message authentication** for all clients.

The WebSocket connects unauthenticated. The client's first frame must
be a `Hello { token, protocol_version, … }` message (see
[[external-protocol]]). The server processes no other message types
until the `Hello` is validated. If no valid `Hello` arrives within
~5 seconds the server closes the connection with WS close code
`1008` (policy violation). Comparison uses `subtle::ConstantTimeEq`;
on failure the daemon emits a single rate-limited log line and
closes — no error message body, so probing learns nothing.

The same code path serves both clients:

- **CLI**: reads `$XDG_RUNTIME_DIR/cairn/token` at startup, opens the
  WS, sends `Hello { token, … }` as the first frame. If the file is
  missing or unreadable the CLI refuses to attempt connection — no
  auto-prompting.
- **Browser (ghostty-web)**: the daemon serves a launcher HTML page
  at `http://127.0.0.1:PORT/` over the same listener; the page reads
  the token from `sessionStorage` (populated by an earlier one-shot
  paste or by a `#token=…` URL fragment on first load — fragments are
  client-only and never transmitted to the server). The page then
  opens the WS and sends `Hello { token, … }` as the first frame.

### Why not the URL?

This is the question the browser's missing-headers limitation pushes
on every WebSocket designer. Rejected alternatives, with rationale:

- **`?token=` query parameter or `/attach/<token>` path segment**: the
  token rides in the request URL, which leaks to access logs, browser
  history, browser address-bar autocomplete, `Referer` headers on any
  navigation away from the page, screenshots, screen-share sessions,
  intermediate proxy logs, load-balancer logs, and network appliances
  (IDS/IPS/WAFs) that inspect URLs. Even loopback HTTPS terminates at
  the daemon's listener; logs and client-side leaks happen outside that
  protection. Strip-on-log mitigates the daemon's own logs but nothing
  else. **Rejected.**
- **HTTP Basic Auth in the URL (`wss://user:pass@host`)**: deprecated
  in modern browsers; Chromium ignores it for subresource and
  WebSocket loads. **Rejected.**
- **Cookies**: browsers send same-origin cookies on the WS upgrade
  request automatically. Works cleanly when the page is served by the
  same origin as the WS endpoint, and we already plan that for the
  launcher HTML. Costs: ties browser auth to a specific deployment
  topology (cross-origin embedders of ghostty-web don't get cookies);
  exposes the token to all JS on the page unless `HttpOnly` (and
  `HttpOnly` cookies aren't readable from JS for the WS handshake on
  some configurations); CSRF surface grows. **Viable but not chosen
  as default**: first-message has the same security guarantees with
  fewer deployment constraints.
- **`Sec-WebSocket-Protocol` abuse**: the browser's `WebSocket`
  constructor lets you set this header via the second argument
  (`new WebSocket(url, [token])`). Server reads it on upgrade. Works
  in browsers and is used by Kubernetes' `kubectl exec` and similar
  tools. Costs: mis-uses a protocol-negotiation header for credential
  carriage; the value lands in some HTTP access logs (curl, Apache
  `LogFormat` `%{Sec-WebSocket-Protocol}i`); the server must echo
  a value back. **Viable backup** if a deployment ever forbids
  application-layer auth, but first-message wins on clarity.
- **TLS client certificates (mTLS)**: cryptographically the cleanest
  answer; UX for browser provisioning is poor. Out of scope for v0
  local-only deployment. Re-examine if remote exposure becomes a
  requirement.

First-message auth has costs of its own — the server must briefly
hold an unauthenticated-but-accepted connection, which is a small DoS
surface. Mitigations: a per-IP connection cap, the 5-second `Hello`
deadline, and refusing all message types other than `Hello` until
authenticated.

### Origin verification (browsers)

The daemon **must** validate `Origin` on every WS upgrade:

- Accept: `null` (file:// or sandboxed contexts), or `http://127.0.0.1:PORT`
  exactly matching our own listener, or an explicitly allow-listed
  origin from config (see [[configuration]]).
- Reject everything else with HTTP 403 *before* the WS handshake
  completes.

CORS does not protect WS. CSWSH protection is `Origin` + token. Without
this check, a token leaked into a browser session (e.g. via a copy-paste
into a URL bar that ended up in browser history) could be replayed
from any tab the user is logged into; with this check, only pages
served by the cairn daemon itself can initiate the handshake.

### Constant-time comparison and rotation

Token comparison uses `subtle::ConstantTimeEq` (or equivalent). On
SIGHUP the daemon re-reads the token file; if it changed, currently
attached clients keep their sessions (the auth check happened at
`Hello` time), but new connections are gated against the new value.
Full rotation is "restart the daemon," which severs everything — that
matches zmx's "restart wipes sessions" mental model.

### Unix-socket fallback for CLI

Where the platform supports it (Linux, macOS) the daemon **also**
exposes a Unix-domain socket at `$XDG_RUNTIME_DIR/cairn/control.sock`
mode `0o600`. CLI clients prefer this path when present, and on it the
token requirement is **waived** — the kernel has already done the
auth, exactly per zmx. The WS+token path is then strictly the browser
story plus a portability fallback.

This is the dual-transport mode hinted at in [[web-vs-cli-clients]].
The trust model becomes:

| Transport       | Auth                                                            | Equivalent to                |
|-----------------|-----------------------------------------------------------------|------------------------------|
| `AF_UNIX` (CLI) | Filesystem DAC                                                  | zmx today                    |
| WS (browser)    | `Origin` + bearer token (first-message) + loopback              | net-new                      |
| WS (CLI)        | Bearer token (first-message) + loopback                         | for non-Unix platforms       |

### Authorization stays flat (v0)

Once authenticated, a client can do everything its zmx counterpart
could. We deliberately do **not** introduce per-tag scopes, read-only
attaches, or session ACLs in v0. Two reasons:

1. The single-user trust domain is preserved verbatim from zmx — any
   process that can read the token file already runs as the user and
   can read PTY state via `/proc/$PID/fd/` anyway.
2. Adding scope strings to `Hello.capabilities`
   (`docs/architecture/pty-session/external-protocol.md` message table)
   is forward-compatible. We can introduce read-only browser links
   later via signed per-session capability tokens (see Open Questions).

## Concrete recommendation for v0

1. Loopback-only bind (`127.0.0.1` + `::1`).
2. Bearer token in `$XDG_RUNTIME_DIR/cairn/token` mode `0o600`.
3. `Origin` allow-list with the daemon's own URL as the sole default.
4. Unix-socket fallback at `$XDG_RUNTIME_DIR/cairn/control.sock`
   mode `0o600`; CLI prefers it; no token required on that transport.
5. **First-message authentication** on the WS path: server accepts the
   upgrade unauthenticated, waits up to 5 seconds for a `Hello { token, … }`
   frame, processes no other message types until validated, closes with
   WS code 1008 on failure or timeout. Same code path for browser and
   CLI clients. Token never appears in URL, request headers, or access
   logs.
6. Constant-time comparison; never log the token or the `Hello`
   payload at any trace level (see [[observability]]).

This buys zmx-equivalent local-user safety, closes CSWSH, and keeps
the door open to per-session capabilities without a wire break — new
fields ride the msgpack maps described in [[external-protocol]].

## Open Questions

1. **Browser token storage between sessions.** First-message auth
   solves the wire-leak problem, but the browser still has to hold
   the token long enough to send it on each reconnect. Options:
   `sessionStorage` (cleared on tab close, requires re-paste);
   `localStorage` (persists, XSS-readable); cookie set by a one-shot
   `POST /login` endpoint that takes the token and binds it to the
   browser (no XSS read if `HttpOnly`, but the JS that opens the WS
   can't read the cookie back to put it in the `Hello` frame — would
   need server-side session identity). The launcher HTML can support
   either via a small config knob. Worth pinning before the v1 wire
   freeze in [[external-protocol]].
2. **Per-session capability tokens.** A read-only share-link
   (`wss://.../session/:id?cap=<signed>`) is appealing for
   collaboration but requires a signing key, expiry semantics, and a
   revocation list. Defer past v0; reserve a `cap` field shape in
   `Attach`.
3. **Remote exposure.** mTLS over `wss://` is the standard answer, but
   provisioning client certs has poor UX. If we ever need this,
   probably terminate TLS at a reverse proxy (Caddy, Tailscale Funnel)
   and keep the daemon loopback-only. Affects [[daemon-process-model]].
4. **Multi-user.** Explicit non-goal for v0. A future "shared dev
   server" mode would need per-user tokens, an identity map, and
   per-session ACLs — a different doc.
5. **Token file watch vs. SIGHUP.** Polling the token file with
   `inotify` / `kqueue` is more responsive than SIGHUP but adds a
   filesystem dependency the daemon otherwise doesn't have. Interacts
   with [[observability]] (auth-event logging) and [[error-recovery]].
6. **Pre-auth resource cost.** An unauthenticated TCP peer can still
   open and abandon connections, costing accept-queue slots. Do we
   need per-IP connection limits even on loopback? Interacts with
   [[backpressure]].
7. **Test posture.** [[testing]] needs an "auth disabled" mode for
   the in-process integration harness — or do we always mint a
   throwaway token? The latter is closer to production but slower.

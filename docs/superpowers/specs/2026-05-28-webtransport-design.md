# WebTransport support for cairn-daemon and cairn-client

## Status

Design. Adds a WebTransport listener to the daemon and WebTransport endpoint support to the CLI client, enabling remote access over QUIC/HTTP/3. Builds on the transport and auth decisions in `2026-05-26-daemon-protocol-design.md`.

## Summary

cairn-daemon gains an optional WebTransport (HTTP/3 over QUIC) listener alongside the existing Unix domain socket. cairn-client gains the ability to connect to a remote daemon via a `wt://` or `https://` endpoint. Authentication is pluggable: v0 ships Tailscale (LocalAPI `whois`) and `none` (loopback-only) backends, with SSH-key and JWT-token backends designed into the trait for v1.

## Scope

**In v0:**
- Daemon WebTransport listener via `wrpc-transport-web` + `wtransport`
- TLS certificate handling: user-provided PEM or auto-generated self-signed
- Pluggable auth backend trait with `tailscale` and `none` implementations
- CLI WebTransport client via `wrpc-transport-web`
- Unified `ConnCtx` / `Identity` across UDS and WT transports
- Cert hash export for CLI pinning

**Out of v0 (designed into the trait, implemented later):**
- SSH-key auth backend (client-signed JWT verified against `authorized_keys`)
- Daemon-issued JWT token backend (for web UI, scripts, CI)
- Embedded Tailscale via `tailscale-rs` (pending crate stabilization)
- Web UI / TypeScript client

## Transport

### Carrier

wRPC's `wrpc-transport-web` crate (v0.2.0), which wraps `wtransport` (QUIC via quinn). The `Client` type wraps a `wtransport::Connection` and implements `wrpc_transport::Invoke`. On the server side, the same `Client` type implements `wrpc_transport::frame::Accept`, yielding `((), SendStream, RecvStream)` triples — one per wRPC invocation.

Each wRPC invocation opens a bidirectional QUIC stream via `Connection::open_bi()`. Multiple concurrent invocations (e.g. an `attach` stream alongside a `list-all` call) multiplex over independent streams with per-stream flow control — no head-of-line blocking between operations.

### Listener configuration

Transport listeners are individually selectable via `--listen`, which accepts multiple values:

```
cairn-daemon                                              # default: unix socket at default path
cairn-daemon --listen wt://0.0.0.0:4433                   # WT only, no UDS
cairn-daemon --listen unix --listen wt://0.0.0.0:4433     # both
cairn-daemon --listen unix:///custom/path                  # UDS at custom path
```

If no `--listen` is provided, the daemon defaults to `unix` (UDS at the platform default path), preserving current behavior. If any `--listen` is provided, only those listeners are active — there is no implicit UDS. At least one listener is required; the daemon exits with an error if the resolved set is empty.

| Flag | Env | Default | Description |
|---|---|---|---|
| `--listen` | `CAIRN_LISTEN` | `unix` | Repeatable. `unix` or `unix:///path` for UDS, `wt://host:port` for WebTransport. |
| `--wt-cert` | `CAIRN_WT_CERT` | none | PEM certificate file path (requires a `wt://` listener) |
| `--wt-key` | `CAIRN_WT_KEY` | none | PEM private key file path (requires a `wt://` listener) |
| `--auth` | `CAIRN_AUTH` | `none` | Comma-separated auth backends, tried in order: `tailscale`, `none`. v1 adds `ssh-key`, `token`. |
| `--wt-connect-timeout` | `CAIRN_WT_CONNECT_TIMEOUT` | `30s` | Time to wait for the first wRPC invocation before closing the connection. |
| `--auth-timeout` | `CAIRN_AUTH_TIMEOUT` | `5s` | Time to wait for a `meta.authenticate` first-message when `FirstMessage` backends are in the chain. Transport-agnostic — applies to any future transport (e.g. WebSocket) that uses first-message auth. |
| `--wt-idle-timeout` | `CAIRN_WT_IDLE_TIMEOUT` | `5m` | Close WT connections with no active streams after this duration. |

UDS-specific flags (`--socket-mode`, `--dir-mode`) only apply when a `unix` listener is active; the daemon warns and ignores them otherwise. WT-specific flags (`--wt-cert`, `--wt-key`) only apply when a `wt://` listener is active.

When a `wt://` listener is configured without `--wt-cert`/`--wt-key`, the daemon generates a self-signed Ed25519 certificate valid for 14 days (within the browser `serverCertificateHashes` limit). The cert and key are written to `$XDG_RUNTIME_DIR/cairn/tls/` and reused across restarts until expiry. The SPKI hash is written to `$XDG_RUNTIME_DIR/cairn/cert-hash` for CLI pinning.

When `--wt-cert`/`--wt-key` are provided (e.g. from `tailscale cert`), the daemon uses those directly. The SPKI hash is still exported.

### Client endpoint resolution

`connect.rs` gains a `WebTransport` variant in the `Endpoint` enum:

```
cairn --daemon wt://hostname:port list          # explicit WT
cairn --daemon https://hostname:port list       # alias for wt://
cairn --daemon /path/to/sock list               # UDS (unchanged)
cairn list                                      # UDS default (unchanged)
```

The `Client` type alias becomes an enum:

```rust
pub enum Client {
    Unix(wrpc_transport::unix::Client<PathBuf>),
    WebTransport(wrpc_transport_web::Client),
}
```

with a forwarding `wrpc_transport::Invoke` impl so protocol call sites remain unchanged.

### Client TLS trust

When connecting via WT, the CLI resolves trust in order:

1. **System trust store** — if the server cert chains to a system-trusted CA (e.g. Tailscale-provisioned cert for `*.ts.net`), connect normally.
2. **Pinned cert hash** — `--cert-hash <hex>` or `CAIRN_CERT_HASH` env var. Used for self-signed certs. The CLI passes this to `wtransport::ClientConfig` via `server_certificate_hashes`.
3. **Hash file** — if neither of the above, and the endpoint is `localhost`/`127.0.0.1`/`::1`, the CLI reads `$XDG_RUNTIME_DIR/cairn/cert-hash` automatically (same-machine WT testing without flags).

## Authentication

### Design: pluggable auth backend chain

Multiple auth backends can be active simultaneously. The daemon holds an ordered list of backends and tries each in sequence — first success wins. This lets a future daemon accept SSH-key auth from CLI users and JWT-token auth from web UI users without separate instances or flag changes.

The daemon defines an auth trait:

```rust
pub trait AuthBackend: Send + Sync + 'static {
    /// Resolve the identity of a WebTransport connection.
    /// Returns `Ok(identity)` on success, `Err(AuthError::NotApplicable)` to
    /// pass to the next backend in the chain, or `Err(AuthError::Rejected(reason))`
    /// to hard-fail the connection (e.g. Tailscale recognizes the IP but the
    /// node is not authorized).
    async fn authenticate(&self, ctx: &AuthContext) -> Result<Identity, AuthError>;

    /// Whether this backend can resolve identity from the connection alone
    /// (transport-level) or needs a first-message token (application-level).
    fn phase(&self) -> AuthPhase;
}

pub enum AuthPhase {
    /// Resolves at connection accept time using peer address or TLS info.
    /// Tailscale and `none` are transport-level.
    Transport,
    /// Requires the client to send a `meta.authenticate(token)` first-message.
    /// SSH-key and JWT-token are application-level.
    FirstMessage,
}

pub enum AuthError {
    /// This backend doesn't apply (e.g. IP not on tailnet). Try the next one.
    NotApplicable,
    /// Hard rejection — don't try further backends, close the connection.
    Rejected(String),
}
```

**Resolution flow per connection:**

1. QUIC handshake completes. Build `AuthContext` with `peer_addr`.
2. Try all `AuthPhase::Transport` backends in order. First `Ok(identity)` wins → done.
3. If none succeeded and `AuthPhase::FirstMessage` backends exist, wait for a `meta.authenticate(token)` call (deadline controlled by `--auth-timeout`, default 5s). Populate `AuthContext.token`.
4. Try all `FirstMessage` backends in order. First `Ok(identity)` wins → done.
5. If all backends returned `NotApplicable`, reject the connection.
6. At any point, `AuthError::Rejected` immediately closes the connection.

`AuthContext` provides the information backends need to resolve identity:

```rust
pub struct AuthContext {
    /// The peer's source address (used by Tailscale whois).
    pub peer_addr: SocketAddr,
    /// The first-message token, if the client sent one (used by JWT backends in v1).
    pub token: Option<String>,
}
```

`Identity` is the unified output:

```rust
pub enum Identity {
    /// Unix peer credentials (UDS transport).
    Unix { uid: u32, username: Option<String> },
    /// Tailscale-resolved identity.
    Tailscale { login: String, display_name: String, node: String },
    /// JWT-authenticated identity (v1).
    Token { subject: String },
    /// SSH-key-authenticated identity (v1).
    SshKey { fingerprint: String, comment: Option<String> },
    /// No authentication (loopback-only or UDS).
    Anonymous,
}

impl Identity {
    /// The human-readable label returned by `whoami`.
    pub fn display_name(&self) -> &str { ... }
}
```

### ConnCtx unification

`ConnCtx` changes from a UDS-specific struct to a transport-agnostic one:

```rust
pub struct ConnCtx {
    pub identity: Identity,
}
```

UDS connections populate `Identity::Unix { uid, username }` from `SO_PEERCRED`. WT connections populate the identity from whichever auth backend is active. The `Handler` impls and `whoami` handler operate on `Identity` uniformly.

### v0 backends

#### `--auth=tailscale`

Resolves identity by calling the Tailscale LocalAPI over its Unix socket:

```
GET /localapi/v0/whois?addr=<peer_ip:peer_port>
```

Returns the peer's Tailscale `UserProfile` (login name, display name) and `Node` (hostname). The daemon makes this call once per WT connection at accept time — before any wRPC invocations are served.

Requirements:
- `tailscaled` must be running on the same machine
- The daemon process must have access to the Tailscale socket (same user or root)
- The WT listener should be bound to the machine's Tailscale IP (100.x.y.z) or `0.0.0.0`

Connections from IPs that Tailscale doesn't recognize are rejected.

**LocalAPI trait boundary.** The Tailscale interaction is behind its own internal trait so the implementation can be swapped from LocalAPI HTTP calls to embedded `tailscale-rs` when that crate stabilizes, without changing the auth backend interface.

#### `--auth=none`

Accepts all connections. `whoami` returns `"anonymous"`. Intended for:
- Loopback-only WT testing during development
- Deployments where the network layer (VPN, firewall) provides sufficient access control

When `--auth=none` is active and `--wt-listen` binds a non-loopback address, the daemon logs a warning at startup.

### v1 backends (trait-designed, not implemented)

#### SSH-key backend

For remote CLI without Tailscale. The client mints a short-lived JWT (`exp: now + 60s`) with `sub` set to the key comment or username, signed with the user's SSH private key (Ed25519 → EdDSA). The daemon verifies the signature against `~/.config/cairn/authorized_keys`. Identity is `Identity::SshKey { fingerprint, comment }`.

Onboarding: `cairn auth add-key ~/.ssh/id_ed25519.pub` appends to the authorized_keys file.

The `AuthContext.token` field exists in v0 to support this — the client sends the JWT as the first-message `meta.authenticate(token)` call, and the auth backend receives it.

#### JWT token backend

For web UI, scripts, and CI. The daemon generates an Ed25519 signing key on first run, stored at `~/.config/cairn/signing-key`. `cairn token issue --name "abe-browser"` mints a long-lived JWT signed with this key. The user pastes the JWT into the web UI. Identity is `Identity::Token { subject }`.

Multi-user: each issued token has a distinct `sub` claim. Revocation via `cairn token revoke <id>` (backed by a small revocation list).

## Daemon serve loop changes

`serve.rs` currently runs a single accept loop for the UDS listener. With `--listen`, it spins up one accept loop per configured listener, all under the same `CancellationToken`:

```
serve()
├── [if unix]  UDS accept loop  →  ConnCtx { identity: Unix { uid, username } }
│              └── wRPC Server (UDS) → invocation streams
├── [if wt]    WT accept loop   →  AuthBackend chain → ConnCtx { identity: ... }
│              └── wRPC Server (WT) → invocation streams
├── invocation pump (merges all streams via select_all)
└── shutdown → drain sessions, abort all loops, cleanup
```

Each transport has its own `wrpc_transport::Server` instance (the type parameters differ: UDS carries `OwnedWriteHalf`/`OwnedReadHalf`, WT carries `SendStream`/`RecvStream`). Both produce invocation streams via `cairn_protocol::serve()` against the same `Daemon` handler. The invocation pump merges all streams with `select_all` — the `Handler` trait impls receive `ConnCtx` and don't care which transport produced it.

Either or both loops may be absent depending on which `--listen` values were provided. The structure is the same regardless of the combination.

The WT accept loop wraps each accepted connection: QUIC handshake completes → auth backend chain resolves identity → `wrpc_transport_web::Client` (which impls `Accept`) is used to accept individual wRPC invocations within the connection. A per-connection task maps the `()` context from the WT `Accept` impl to the pre-resolved `ConnCtx` before feeding invocations to the pump.

### WT-specific lifecycle

- **Connection-level auth**: identity is resolved once per WT connection, not per invocation. All invocations within a connection share the same identity.
- **Connection timeout** (`--wt-connect-timeout`, default 30s): if no wRPC invocation arrives within this deadline after connection open, the connection is closed.
- **Idle timeout** (`--wt-idle-timeout`, default 5m): connections with no active streams for this duration are closed.
- **Graceful shutdown**: on SIGTERM, the daemon stops accepting new WT connections, drains active sessions (same as UDS path), then closes the QUIC endpoint.

## Wire protocol interaction

The `meta.authenticate(token)` WIT operation remains in the schema. Whether the client must call it depends on which auth backends are active:

- **Transport-level backends only** (e.g. `--auth=tailscale` or `--auth=none`): identity is resolved at connection accept time. The client does not need to call `meta.authenticate`. If called, it's a no-op.
- **FirstMessage backends in the chain** (e.g. `--auth=tailscale,token`): if no transport-level backend succeeded, the daemon holds the connection open and waits for a `meta.authenticate(token)` call (deadline controlled by `--auth-timeout`, default 5s). Other wRPC invocations received before authentication completes are rejected with an `unauthenticated` error code. Once `authenticate` populates the token and a `FirstMessage` backend succeeds, subsequent invocations proceed normally.
- **Mixed chains** (e.g. `--auth=tailscale,ssh-key`): Tailscale clients authenticate at the transport level and never need to call `authenticate`. Non-Tailscale clients fall through to the `FirstMessage` phase and must send a token.

This two-phase design means the same daemon can serve both Tailscale-authenticated web UI users and SSH-key-authenticated CLI users without configuration changes.

## Dependencies

### Workspace additions (`Cargo.toml`)

```toml
wrpc-transport-web = { version = "0.2", default-features = false }
wtransport = { version = "0.6", default-features = false }
rustls = { version = "0.23", default-features = false }
rcgen = { version = "0.13", default-features = false }  # self-signed cert generation
```

### cairn-daemon

```toml
wrpc-transport-web.workspace = true
wtransport.workspace = true
rustls.workspace = true
rcgen.workspace = true
hyper = { version = "1", features = ["client", "http1"] }  # Tailscale LocalAPI calls
hyper-util = { version = "0.1", features = ["tokio"] }
```

### cairn-client

```toml
wrpc-transport-web.workspace = true
wtransport.workspace = true
rustls.workspace = true
```

## Testing

### Unit tests

- `auth::tailscale` — mock the LocalAPI HTTP response, verify identity extraction and error cases (unknown IP, tailscaled unreachable)
- `auth::none` — verify all connections produce `Identity::Anonymous`, verify non-loopback warning
- `connect.rs` — endpoint parsing for `wt://`, `https://`, cert-hash resolution order
- `tls.rs` — self-signed cert generation, hash export, expiry check, PEM loading
- `ConnCtx` / `Identity` — `display_name()` for each variant

### Integration tests

- Daemon starts with `--wt-listen 127.0.0.1:0 --auth=none`, client connects via WT, runs `version` — verifies the full wRPC-over-QUIC path.
- Same setup but with `create` + `attach` — verifies streaming over WT.
- UDS and WT clients connected simultaneously — verifies both accept loops coexist.
- Client connects with wrong cert hash — verifies TLS rejection.
- `--auth=tailscale` with a mocked LocalAPI — verifies identity flows through to `whoami`.

### Manual testing

- `cairn-daemon --wt-listen 0.0.0.0:4433 --auth=tailscale --wt-cert /path/to/cert --wt-key /path/to/key`
- From another Tailscale machine: `cairn --daemon wt://cairn-host.ts.net:4433 list`

## Open questions

1. **wtransport version compatibility.** `wrpc-transport-web` 0.2.0 depends on `wtransport ^0.6.1`, but `wtransport` is at 0.7.1 on crates.io. Need to verify whether `wrpc-transport-web` works with 0.7.x or if we're pinned to 0.6.x.
2. **Tailscale LocalAPI socket path.** Varies by platform: `/var/run/tailscale/tailscaled.sock` on Linux, `127.0.0.1:41112` (TCP) on macOS. Need platform detection or a `--tailscale-socket` override flag.
3. **Self-signed cert rotation.** When the 14-day self-signed cert expires, should the daemon auto-regenerate on startup, or require a manual `cairn cert regenerate`? Auto-regenerate is simpler but changes the hash, breaking pinned clients.
4. **QUIC transport config tuning.** Default quinn connection/stream flow control windows are likely fine, but may need adjustment for high-throughput PTY output. Defer until measured.

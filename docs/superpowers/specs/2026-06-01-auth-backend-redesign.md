# Auth Backend Redesign

## Problem

The current auth system has a `NoneBackend` that accepts all connections as
anonymous. When combined with real backends (`--auth none,tailscale`), it
silently swallows connections before the real backend runs, or acts as a
fallback-to-anonymous that undermines the security of the real backend.

More fundamentally, UDS and network transports have different identity models
that the current flat auth chain conflates:

- **UDS**: identity comes from the OS via `SO_PEERCRED`. No authentication is
  needed — filesystem permissions gate access.
- **Network transports** (WebTransport, future WebSocket/QUIC/iroh): identity
  must be established by an auth backend. Different transports provide different
  material (peer IP, public key, client cert) that different backends consume.

The auth interface needs to separate these concerns and model the transport-auth
relationship as a discriminated union rather than a grab-bag of optional fields.

## Design

### Core principle

UDS resolves identity from the OS, never the auth chain. The auth chain is
exclusively for network transports. `NoneBackend` is removed entirely.

### Transport context

Each transport variant carries exactly the material available on that transport.
No optional fields for "maybe this transport has it." The transport layer that
accepts the connection constructs the appropriate variant.

```rust
// auth/mod.rs

pub enum TransportContext {
    WebTransport { peer_addr: SocketAddr },
    // future variants as transports are added:
    // WebSocket { peer_addr: SocketAddr },
    // Quic { peer_addr: SocketAddr },
    // Iroh { peer_key: PublicKey },
}

pub struct AuthContext {
    pub transport: TransportContext,
    /// First-message token, transport-independent. Populated after
    /// `meta.authenticate(token)` is called by the client.
    pub token: Option<String>,
}
```

Adding a new transport means adding a variant to `TransportContext`. The
compiler flags every backend's match as non-exhaustive, forcing a conscious
decision about whether the backend applies to the new transport.

If a transport later gains optional capabilities (e.g. QUIC/WT support mTLS
client certs), that's an `Option` on the specific variant — it's optional
because *that transport* optionally provides it.

### Auth backend trait

Unchanged from today. Backends inspect `ctx.transport` and return
`NotApplicable` for transports they don't support. The `phase()` method
remains — it tells the connection handler when to invoke the backend
(at connection accept vs after the first-message token).

```rust
pub enum AuthPhase {
    Transport,
    FirstMessage,
}

pub enum AuthError {
    NotApplicable,
    Rejected(String),
}

pub trait AuthBackend: Send + Sync {
    fn authenticate(
        &self,
        ctx: &AuthContext,
    ) -> Pin<Box<dyn Future<Output = Result<Identity, AuthError>> + Send + '_>>;

    fn phase(&self) -> AuthPhase;
}
```

`AuthChain` is also unchanged: tries backends in order within each phase,
first success wins, hard rejection short-circuits.

### Backend self-selection pattern

Backends match on the transport variant to extract what they need:

```rust
// TailscaleBackend — works on any transport with peer_addr
match &ctx.transport {
    TransportContext::WebTransport { peer_addr } => self.whois(peer_addr).await,
    // future: WebSocket, Quic variants would also match here
}

// future: IrohKeyBackend — only works on iroh
match &ctx.transport {
    TransportContext::Iroh { peer_key } => self.verify(peer_key).await,
    _ => Err(AuthError::NotApplicable),
}
```

This means all backends share one flat `--auth` list. There is no per-listener
auth configuration. Backends determine their own applicability from the
connection context, which the existing chain model already supports.

### CLI surface

`AuthBackendKind::None` is removed. `--auth` has no default value — it is only
required when a network listener is configured.

```
# UDS only (default) — no --auth needed
cairn-daemon

# Network listener — --auth is required
cairn-daemon --listen https://0.0.0.0:9443 --auth tailscale

# Both — --auth applies to network listener only, UDS uses peer creds
cairn-daemon --listen unix --listen https://0.0.0.0:9443 --auth tailscale
```

### Validation chokepoint: `Daemon::new`

Validation lives in `Daemon::new(cfg) -> Result<Self>`, not on `DaemonConfig`
or in any conversion trait. This is the single chokepoint that every config
source (CLI args today, config file tomorrow) must pass through. There is no
valid `Daemon` with an invalid config, and you can't forget to validate because
construction is the only entry point.

`From<Args> for DaemonConfig` stays infallible — it's pure field mapping. A
future config file parser would also produce a `DaemonConfig` that gets
validated at `Daemon::new`.

Hard errors (in `Daemon::new`):
- Network listener configured + `auth` is empty

Soft warnings (also in `Daemon::new`, after tracing is initialized):
- `--auth` provided but no network listener configured
- `--dir-mode`/`--socket-mode` without a UDS listener
- WT listener without `--wt-cert`/`--wt-key`

The auth=none + non-loopback WT warning is removed (the condition can no longer
arise). The existing `DaemonConfig::warn_on_misconfig()` is removed — its
contents move into `Daemon::new`.

### `build_auth_chain`

Returns `Option<AuthChain>` — `None` when no network listeners are configured,
`Some(chain)` when they are. The empty-backends case is caught by
`Daemon::new` before this is ever called.

### Serve wiring

`serve_wt_connection` constructs the `AuthContext` with the transport variant:

```rust
let ctx = AuthContext {
    transport: TransportContext::WebTransport {
        peer_addr: conn.remote_address(),
    },
    token: None,
};
```

UDS path (`PeerCredListener::accept`) is completely unchanged — it resolves
`Identity::Unix` from `SO_PEERCRED` and never touches the auth chain.

## File-level changes

| File | Change |
|---|---|
| `auth/mod.rs` | Add `TransportContext` enum. Replace `AuthContext` fields. |
| `auth/none.rs` | Keep as public module, not wired to CLI. Used by test harness. Update doc comment. |
| `auth/tailscale.rs` | Match on `ctx.transport` to extract `peer_addr`. |
| `config/mod.rs` | Remove `AuthBackendKind::None`. Remove `DaemonConfig::warn_on_misconfig()`. |
| `config/args.rs` | Remove `default_value = "none"` from `--auth`. |
| `daemon.rs` | `Daemon::new` becomes fallible (`-> Result<Self>`), absorbs validation and warnings. `build_auth_chain` returns `Option<AuthChain>`. |
| `main.rs` | Remove `cfg.warn_on_misconfig()` call (moved into `Daemon::new`). Pass `None` to `serve()`. |
| `serve.rs` | Add `auth_chain: Option<AuthChain>` parameter. Construct `AuthContext` with `TransportContext::WebTransport`. |
| `identity.rs` | No changes. `Anonymous` stays for UDS `peer_cred()` fallback. |

## Test impact

- Update `auth::none::tests` and `auth::tests` chain tests to use `TransportContext` in test fixtures.
- `auth::tailscale::tests` — minimal changes (unit tests exercise `http_get_unix`/`parse_whois_response`, not `AuthContext`).
- Integration tests (`daemon_meta`, `daemon_streaming`) — UDS-only, unaffected.
- WT smoke tests — test harness injects a `NoneBackend` chain via the `serve()` parameter, bypassing `build_auth_chain`. This avoids a tailscaled dependency in automated tests while still exercising the WT transport path.

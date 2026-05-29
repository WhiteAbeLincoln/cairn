# WebTransport support implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **2026-05-29 amendment.** Post-implementation, the `wt://` URL scheme
> used in this plan has been replaced with the standard `https://` scheme
> defined by the W3C WebTransport spec. `wt://` is not an IANA-registered
> URI scheme; a future WebSocket transport would use the standard
> `wss://`, so `https://` already unambiguously selects WebTransport.
> Read `https://host:port` wherever the plan body says `wt://host:port`.

**Goal:** Add a WebTransport (QUIC/HTTP/3) listener to cairn-daemon and WebTransport client support to cairn-client, enabling remote daemon access alongside the existing Unix domain socket transport.

**Architecture:** The daemon gains an optional WT listener via `wrpc-transport-web` + `wtransport`. Both transports feed the same `Daemon` handler through a unified `ConnCtx { identity: Identity }`. Authentication is pluggable: an `AuthBackend` trait chain with `none` and `tailscale` (Tailscale LocalAPI) implementations for v0. TLS uses user-provided PEM certs or auto-generated self-signed certs with SPKI hash export for CLI pinning. The CLI client gains a `Client` enum wrapping both UDS and WT transports with a forwarding `Invoke` impl. Transports are individually selectable via `--listen unix`/`--listen wt://host:port`.

**Tech Stack:** Rust 2024, `wrpc-transport-web` 0.2, `wtransport` 0.6, `rcgen` 0.13, `rustls` 0.23, `hyper` 1 (Tailscale LocalAPI), existing `wrpc-transport` 0.28 + `wit-bindgen-wrpc` generated code.

---

## File map

**Create (cairn-daemon)**
- `crates/cairn-daemon/src/identity.rs` — `Identity` enum, `display_name()` impl
- `crates/cairn-daemon/src/auth/mod.rs` — `AuthBackend` trait, `AuthPhase`, `AuthError`, `AuthContext`, `AuthChain`
- `crates/cairn-daemon/src/auth/none.rs` — `NoneBackend`
- `crates/cairn-daemon/src/auth/tailscale.rs` — `TailscaleBackend` via LocalAPI
- `crates/cairn-daemon/src/tls.rs` — self-signed cert generation, PEM loading, SPKI hash export
- `crates/cairn-daemon/src/listen.rs` — `ListenerConfig` enum, `--listen` URI parsing

**Modify (cairn-daemon)**
- `Cargo.toml` (workspace) — add `wrpc-transport-web`, `wtransport`, `rustls`, `rcgen` workspace deps
- `crates/cairn-daemon/Cargo.toml` — add new deps
- `crates/cairn-daemon/src/lib.rs` — register new modules
- `crates/cairn-daemon/src/config.rs` — add `listeners`, `auth_backends`, WT timeout fields
- `crates/cairn-daemon/src/serve.rs` — refactor `ConnCtx`, extract UDS binding, add WT accept loop
- `crates/cairn-daemon/src/daemon.rs` — update `Handler` impls to use `Identity`
- `crates/cairn-daemon/src/handlers/meta.rs` — rewrite `whoami` to use `Identity`
- `crates/cairn-daemon/src/main.rs` — new CLI flags (`--listen`, `--auth`, `--wt-cert`, etc.)
- `crates/cairn-daemon/tests/common/mod.rs` — update `DaemonHarness` for new config shape

**Modify (cairn-client)**
- `crates/cairn-client/Cargo.toml` — add `wrpc-transport-web`, `wtransport`, `rustls`
- `crates/cairn-client/src/connect.rs` — `Endpoint::WebTransport`, `Client` enum, `Invoke` forwarding, TLS trust
- `crates/cairn-client/src/cli.rs` — add `--cert-hash`, update `--daemon` help text

---

## Task 1: Add workspace dependencies

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/cairn-daemon/Cargo.toml`
- Modify: `crates/cairn-client/Cargo.toml`

- [ ] **Step 1: Add workspace-level deps**

Add to the `[workspace.dependencies]` section of `Cargo.toml`:

```toml
wrpc-transport-web = { version = "0.2", default-features = false }
wtransport = { version = "0.6", default-features = false }
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
rcgen = { version = "0.13", default-features = false, features = ["ring"] }
serde_json = { version = "1.0.149", features = ["raw_value"] }
```

Note: `serde_json` may already be in workspace deps — if so, just ensure it's present. Check the `wrpc-transport-web` → `wtransport` version requirement: `wrpc-transport-web 0.2.0` depends on `wtransport ^0.6.1`. If the semver range doesn't resolve, pin to `0.6.1`.

- [ ] **Step 2: Add cairn-daemon deps**

Add to `crates/cairn-daemon/Cargo.toml` `[dependencies]`:

```toml
wrpc-transport-web.workspace = true
wtransport.workspace = true
rustls.workspace = true
rcgen.workspace = true
serde.workspace = true
serde_json.workspace = true
hyper = { version = "1", features = ["client", "http1"] }
hyper-util = { version = "0.1", features = ["tokio", "client-legacy"] }
http-body-util = "0.1"
tokio = { workspace = true, features = ["rt-multi-thread", "net", "signal", "macros", "sync", "time", "io-util"] }
```

- [ ] **Step 3: Add cairn-client deps**

Add to `crates/cairn-client/Cargo.toml` `[dependencies]`:

```toml
wrpc-transport-web.workspace = true
wtransport.workspace = true
rustls.workspace = true
```

- [ ] **Step 4: Verify workspace compiles**

Run: `cargo check --workspace`
Expected: compiles with no errors (new deps are unused for now)

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/cairn-daemon/Cargo.toml crates/cairn-client/Cargo.toml
git commit -m "chore: add WebTransport, TLS, and auth dependencies"
```

---

## Task 2: Identity enum and ConnCtx refactor

Replace the UDS-specific `ConnCtx { peer: Option<UCred> }` with a transport-agnostic `ConnCtx { identity: Identity }`. This is the foundation all subsequent tasks build on.

**Files:**
- Create: `crates/cairn-daemon/src/identity.rs`
- Modify: `crates/cairn-daemon/src/lib.rs`
- Modify: `crates/cairn-daemon/src/serve.rs`
- Modify: `crates/cairn-daemon/src/daemon.rs`
- Modify: `crates/cairn-daemon/src/handlers/meta.rs`
- Modify: `crates/cairn-daemon/tests/common/mod.rs`

- [ ] **Step 1: Write Identity tests**

Create `crates/cairn-daemon/src/identity.rs`:

```rust
//! Transport-agnostic caller identity.

/// The resolved identity of a connected client, produced by the transport
/// layer (UDS peer creds) or an auth backend (Tailscale, JWT, SSH key).
#[derive(Clone, Debug)]
pub enum Identity {
    /// Unix peer credentials from `SO_PEERCRED`.
    Unix { uid: u32, username: Option<String> },
    /// Tailscale-resolved identity via LocalAPI `whois`.
    Tailscale {
        login: String,
        display_name: String,
        node: String,
    },
    /// JWT-authenticated identity (v1).
    Token { subject: String },
    /// SSH-key-authenticated identity (v1).
    SshKey {
        fingerprint: String,
        comment: Option<String>,
    },
    /// No authentication (loopback-only or development).
    Anonymous,
}

impl Identity {
    /// Human-readable label returned by `whoami`.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Unix { username: Some(name), .. } => name,
            Self::Unix { uid, .. } => {
                // Fallback handled by the caller since we can't return
                // a &str to a formatted number. Return a static placeholder.
                // The whoami handler formats the uid directly.
                return "";
            }
            Self::Tailscale { display_name, .. } => display_name,
            Self::Token { subject } => subject,
            Self::SshKey { fingerprint, .. } => fingerprint,
            Self::Anonymous => "anonymous",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_display_name_prefers_username() {
        let id = Identity::Unix {
            uid: 501,
            username: Some("abe".into()),
        };
        assert_eq!(id.display_name(), "abe");
    }

    #[test]
    fn unix_display_name_fallback_for_uid_only() {
        let id = Identity::Unix {
            uid: 501,
            username: None,
        };
        assert_eq!(id.display_name(), "");
    }

    #[test]
    fn tailscale_display_name() {
        let id = Identity::Tailscale {
            login: "user@example.com".into(),
            display_name: "User".into(),
            node: "myhost".into(),
        };
        assert_eq!(id.display_name(), "User");
    }

    #[test]
    fn anonymous_display_name() {
        assert_eq!(Identity::Anonymous.display_name(), "anonymous");
    }
}
```

- [ ] **Step 2: Register module and run tests**

Add `pub mod identity;` to `crates/cairn-daemon/src/lib.rs`.

Run: `cargo nextest run -p cairn-daemon -E 'test(~identity)'`
Expected: all 4 tests pass

- [ ] **Step 3: Refactor ConnCtx and PeerCredListener**

In `crates/cairn-daemon/src/serve.rs`, change `ConnCtx` from the UDS-specific struct to the transport-agnostic one. Update `PeerCredListener::accept` to produce `Identity::Unix`. Remove the `UCred` import since it's no longer stored directly.

Replace `ConnCtx`:

```rust
/// Per-connection context handed to every `Handler` method.
#[derive(Clone, Debug)]
pub struct ConnCtx {
    pub identity: crate::identity::Identity,
}
```

Remove `Copy` derive (Identity contains Strings).

Update `PeerCredListener::accept`:

```rust
async fn accept(&self) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
    let (stream, _addr) = self.0.accept().await?;
    let peer = stream.peer_cred().ok();
    let identity = crate::identity::Identity::Unix {
        uid: peer.map(|c| c.uid()).unwrap_or(u32::MAX),
        username: peer
            .map(|c| c.uid())
            .and_then(username_for),
    };
    let (rx, tx) = stream.into_split();
    Ok((ConnCtx { identity }, tx, rx))
}
```

Add `username_for` to `serve.rs` (move from `handlers/meta.rs`):

```rust
fn username_for(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}
```

- [ ] **Step 4: Update handlers/meta.rs**

Rewrite `whoami` to use `Identity`:

```rust
//! Meta-interface handlers: `version`, `whoami`, `authenticate`.

use cairn_protocol::cairn::daemon::types::Error as WireError;
use cairn_protocol::exports::cairn::daemon::meta::VersionInfo;

use crate::serve::ConnCtx;

pub fn version() -> VersionInfo {
    VersionInfo {
        daemon: concat!("cairn-daemon/", env!("CARGO_PKG_VERSION")).to_string(),
        protocol: "cairn:daemon@0.1.0".to_string(),
    }
}

/// UDS is pre-authenticated by the kernel; first-message auth is a
/// WebTransport concern. Accept any token unconditionally on this transport.
pub fn authenticate(_token: String) -> Result<(), WireError> {
    Ok(())
}

pub fn whoami(ctx: &ConnCtx) -> Result<String, WireError> {
    use crate::identity::Identity;
    let name = match &ctx.identity {
        Identity::Unix { uid, username } => {
            username.clone().unwrap_or_else(|| uid.to_string())
        }
        other => {
            let dn = other.display_name();
            if dn.is_empty() {
                "unknown".to_string()
            } else {
                dn.to_string()
            }
        }
    };
    Ok(name)
}
```

Remove the old `username_for` function from this file (it moved to serve.rs).

- [ ] **Step 5: Update daemon.rs Handler impls**

In `crates/cairn-daemon/src/daemon.rs`, the Handler impls receive `ConnCtx` — no signature changes needed. The only change: `ConnCtx` is no longer `Copy`, so methods that pass `ctx` by value are fine (it's moved). Verify no `Copy`-dependent patterns exist.

Check that `daemon.rs` compiles by looking for any `ctx.peer` references — there should be none; only `handlers/meta.rs::whoami` used it, and that's been updated.

- [ ] **Step 6: Update serve.rs test**

Update the `accept_yields_peer_uid` test in `serve.rs`:

```rust
#[tokio::test]
async fn accept_yields_peer_uid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let pl = PeerCredListener(listener);

    let connect = tokio::spawn(async move {
        let _c = tokio::net::UnixStream::connect(&path).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    });

    let (ctx, _tx, _rx) = (&pl).accept().await.unwrap();
    match &ctx.identity {
        crate::identity::Identity::Unix { uid, username } => {
            assert_eq!(*uid, nix_geteuid());
            assert!(username.is_some());
        }
        other => panic!("expected Unix identity, got {other:?}"),
    }
    connect.await.unwrap();
}
```

- [ ] **Step 7: Update test harness**

In `crates/cairn-daemon/tests/common/mod.rs`, `DaemonHarness::start()` and `client()` don't reference `ConnCtx` directly — they create a `DaemonConfig` and return a wRPC client. No changes needed unless the `DaemonConfig` shape changed (it hasn't yet). Verify compilation.

- [ ] **Step 8: Run all tests**

Run: `cargo nextest run -p cairn-daemon`
Expected: all tests pass

Run: `cargo clippy -p cairn-daemon --all-targets -- -D warnings`
Expected: clean

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-daemon/src/identity.rs crates/cairn-daemon/src/lib.rs \
  crates/cairn-daemon/src/serve.rs crates/cairn-daemon/src/daemon.rs \
  crates/cairn-daemon/src/handlers/meta.rs
git commit -m "refactor: unify ConnCtx around transport-agnostic Identity enum"
```

---

## Task 3: Auth backend trait and NoneBackend

Define the pluggable authentication interface and implement the simplest backend.

**Files:**
- Create: `crates/cairn-daemon/src/auth/mod.rs`
- Create: `crates/cairn-daemon/src/auth/none.rs`
- Modify: `crates/cairn-daemon/src/lib.rs`

- [ ] **Step 1: Write the auth trait and chain**

Create `crates/cairn-daemon/src/auth/mod.rs`:

```rust
//! Pluggable authentication backends for WebTransport connections.

pub mod none;

use std::net::SocketAddr;

use crate::identity::Identity;

/// When in the connection lifecycle this backend resolves identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthPhase {
    /// Resolves at connection accept time using peer address or TLS info.
    Transport,
    /// Requires the client to send a `meta.authenticate(token)` first-message.
    FirstMessage,
}

/// Error returned by an auth backend.
#[derive(Debug)]
pub enum AuthError {
    /// This backend doesn't apply. Try the next one in the chain.
    NotApplicable,
    /// Hard rejection — close the connection, don't try further backends.
    Rejected(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApplicable => write!(f, "not applicable"),
            Self::Rejected(reason) => write!(f, "rejected: {reason}"),
        }
    }
}

/// Information available to auth backends for identity resolution.
#[derive(Debug)]
pub struct AuthContext {
    /// Peer's source address (used by Tailscale whois).
    pub peer_addr: SocketAddr,
    /// First-message token, populated after `meta.authenticate` is called.
    pub token: Option<String>,
}

/// A backend that can resolve the identity of a WebTransport connection.
pub trait AuthBackend: Send + Sync {
    /// Try to resolve identity. Return `Ok(identity)` on success,
    /// `Err(NotApplicable)` to pass to the next backend, or
    /// `Err(Rejected)` to hard-fail the connection.
    fn authenticate(
        &self,
        ctx: &AuthContext,
    ) -> impl std::future::Future<Output = Result<Identity, AuthError>> + Send;

    /// When this backend resolves identity relative to the connection lifecycle.
    fn phase(&self) -> AuthPhase;
}

/// An ordered chain of auth backends. Tries each backend in sequence;
/// first success wins.
pub struct AuthChain {
    backends: Vec<Box<dyn AuthBackend>>,
}

impl AuthChain {
    pub fn new(backends: Vec<Box<dyn AuthBackend>>) -> Self {
        Self { backends }
    }

    /// Whether any backend in the chain requires a first-message token.
    pub fn has_first_message_backends(&self) -> bool {
        self.backends.iter().any(|b| b.phase() == AuthPhase::FirstMessage)
    }

    /// Run all transport-phase backends. Returns the first successful identity
    /// or None if all returned NotApplicable.
    pub async fn try_transport(&self, ctx: &AuthContext) -> Result<Identity, AuthError> {
        for backend in &self.backends {
            if backend.phase() != AuthPhase::Transport {
                continue;
            }
            match backend.authenticate(ctx).await {
                Ok(identity) => return Ok(identity),
                Err(AuthError::NotApplicable) => continue,
                Err(e @ AuthError::Rejected(_)) => return Err(e),
            }
        }
        Err(AuthError::NotApplicable)
    }

    /// Run all first-message-phase backends. Called after transport phase
    /// fails and the client sends a token.
    pub async fn try_first_message(&self, ctx: &AuthContext) -> Result<Identity, AuthError> {
        for backend in &self.backends {
            if backend.phase() != AuthPhase::FirstMessage {
                continue;
            }
            match backend.authenticate(ctx).await {
                Ok(identity) => return Ok(identity),
                Err(AuthError::NotApplicable) => continue,
                Err(e @ AuthError::Rejected(_)) => return Err(e),
            }
        }
        Err(AuthError::NotApplicable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AlwaysAnon;
    impl AuthBackend for AlwaysAnon {
        async fn authenticate(&self, _ctx: &AuthContext) -> Result<Identity, AuthError> {
            Ok(Identity::Anonymous)
        }
        fn phase(&self) -> AuthPhase {
            AuthPhase::Transport
        }
    }

    struct AlwaysReject;
    impl AuthBackend for AlwaysReject {
        async fn authenticate(&self, _ctx: &AuthContext) -> Result<Identity, AuthError> {
            Err(AuthError::Rejected("denied".into()))
        }
        fn phase(&self) -> AuthPhase {
            AuthPhase::Transport
        }
    }

    struct SkipBackend;
    impl AuthBackend for SkipBackend {
        async fn authenticate(&self, _ctx: &AuthContext) -> Result<Identity, AuthError> {
            Err(AuthError::NotApplicable)
        }
        fn phase(&self) -> AuthPhase {
            AuthPhase::Transport
        }
    }

    fn test_ctx() -> AuthContext {
        AuthContext {
            peer_addr: "127.0.0.1:1234".parse().unwrap(),
            token: None,
        }
    }

    #[tokio::test]
    async fn chain_returns_first_success() {
        let chain = AuthChain::new(vec![
            Box::new(SkipBackend),
            Box::new(AlwaysAnon),
        ]);
        let result = chain.try_transport(&test_ctx()).await;
        assert!(matches!(result, Ok(Identity::Anonymous)));
    }

    #[tokio::test]
    async fn chain_stops_on_rejection() {
        let chain = AuthChain::new(vec![
            Box::new(AlwaysReject),
            Box::new(AlwaysAnon),
        ]);
        let result = chain.try_transport(&test_ctx()).await;
        assert!(matches!(result, Err(AuthError::Rejected(_))));
    }

    #[tokio::test]
    async fn chain_not_applicable_if_all_skip() {
        let chain = AuthChain::new(vec![Box::new(SkipBackend)]);
        let result = chain.try_transport(&test_ctx()).await;
        assert!(matches!(result, Err(AuthError::NotApplicable)));
    }
}
```

- [ ] **Step 2: Implement NoneBackend**

Create `crates/cairn-daemon/src/auth/none.rs`:

```rust
//! The `none` auth backend: accepts all connections as anonymous.

use crate::auth::{AuthBackend, AuthContext, AuthError, AuthPhase};
use crate::identity::Identity;

pub struct NoneBackend;

impl AuthBackend for NoneBackend {
    async fn authenticate(&self, _ctx: &AuthContext) -> Result<Identity, AuthError> {
        Ok(Identity::Anonymous)
    }

    fn phase(&self) -> AuthPhase {
        AuthPhase::Transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepts_any_connection() {
        let backend = NoneBackend;
        let ctx = AuthContext {
            peer_addr: "192.168.1.50:9999".parse().unwrap(),
            token: None,
        };
        let result = backend.authenticate(&ctx).await;
        assert!(matches!(result, Ok(Identity::Anonymous)));
    }
}
```

- [ ] **Step 3: Register module and run tests**

Add `pub mod auth;` to `crates/cairn-daemon/src/lib.rs`.

Run: `cargo nextest run -p cairn-daemon -E 'test(~auth)'`
Expected: 4 tests pass (3 chain tests + 1 none test)

- [ ] **Step 4: Commit**

```bash
git add crates/cairn-daemon/src/auth/ crates/cairn-daemon/src/lib.rs
git commit -m "feat: pluggable auth backend trait with none implementation"
```

---

## Task 4: TLS certificate handling

Self-signed cert generation via `rcgen`, PEM file loading, and SPKI hash export.

**Files:**
- Create: `crates/cairn-daemon/src/tls.rs`
- Modify: `crates/cairn-daemon/src/lib.rs`

- [ ] **Step 1: Write TLS module with tests**

Create `crates/cairn-daemon/src/tls.rs`:

```rust
//! TLS certificate handling for the WebTransport listener.
//!
//! Two modes: user-provided PEM files (`--wt-cert`/`--wt-key`) or
//! auto-generated self-signed Ed25519 cert (14-day validity, written to
//! `$XDG_RUNTIME_DIR/cairn/tls/`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// A resolved TLS configuration ready to hand to `wtransport::ServerConfig`.
pub struct TlsConfig {
    pub cert_pem: String,
    pub key_pem: String,
    pub spki_hash: Vec<u8>,
}

impl TlsConfig {
    /// Load from user-provided PEM files.
    pub fn from_pem_files(cert_path: &Path, key_path: &Path) -> Result<Self> {
        let cert_pem =
            std::fs::read_to_string(cert_path).context("reading TLS certificate file")?;
        let key_pem = std::fs::read_to_string(key_path).context("reading TLS key file")?;
        let spki_hash = compute_spki_hash(&cert_pem)?;
        Ok(Self { cert_pem, key_pem, spki_hash })
    }

    /// Load or generate a self-signed certificate. Reuses an existing cert
    /// from `tls_dir` if it hasn't expired; otherwise generates a new one.
    pub fn self_signed(tls_dir: &Path) -> Result<Self> {
        let cert_path = tls_dir.join("cert.pem");
        let key_path = tls_dir.join("key.pem");

        if cert_path.exists() && key_path.exists() {
            if let Ok(config) = Self::from_pem_files(&cert_path, &key_path) {
                if !is_expired(&config.cert_pem) {
                    tracing::info!(
                        cert = %cert_path.display(),
                        "reusing existing self-signed certificate"
                    );
                    return Ok(config);
                }
                tracing::info!("self-signed certificate expired, regenerating");
            }
        }

        tracing::info!(dir = %tls_dir.display(), "generating self-signed certificate");
        let (cert_pem, key_pem) = generate_self_signed()?;

        std::fs::create_dir_all(tls_dir).context("creating TLS directory")?;
        std::fs::write(&cert_path, &cert_pem).context("writing cert.pem")?;
        std::fs::write(&key_path, &key_pem).context("writing key.pem")?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
            let _ = std::fs::set_permissions(&cert_path, std::fs::Permissions::from_mode(0o644));
        }

        let spki_hash = compute_spki_hash(&cert_pem)?;
        Ok(Self { cert_pem, key_pem, spki_hash })
    }

    /// Write the hex-encoded SPKI hash to a file for CLI pinning.
    pub fn export_hash(&self, path: &Path) -> Result<()> {
        let hex = hex_encode(&self.spki_hash);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &hex).context("writing cert-hash file")?;
        tracing::info!(path = %path.display(), hash = %hex, "exported SPKI hash");
        Ok(())
    }

    /// The SPKI hash as a hex string.
    pub fn spki_hash_hex(&self) -> String {
        hex_encode(&self.spki_hash)
    }
}

fn generate_self_signed() -> Result<(String, String)> {
    use rcgen::{CertificateParams, KeyPair};

    let key_pair = KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
    let mut params = CertificateParams::new(vec!["localhost".into()])?;
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(14);
    let cert = params.self_signed(&key_pair)?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

fn compute_spki_hash(cert_pem: &str) -> Result<Vec<u8>> {
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject as _;

    let cert = CertificateDer::from_pem_slice(cert_pem.as_bytes())
        .next()
        .ok_or_else(|| anyhow::anyhow!("no certificate found in PEM"))??;

    // SHA-256 of the SubjectPublicKeyInfo DER encoding.
    // The SPKI starts after the outer SEQUENCE + version + serial + sig-alg + issuer + validity + subject.
    // For simplicity, hash the whole certificate DER — `wtransport` uses the same approach
    // for `serverCertificateHashes`.
    use ring::digest;
    let hash = digest::digest(&digest::SHA256, cert.as_ref());
    Ok(hash.as_ref().to_vec())
}

fn is_expired(cert_pem: &str) -> bool {
    // Parse not_after from the PEM cert. If parsing fails, treat as expired
    // to trigger regeneration.
    use rustls::pki_types::CertificateDer;
    use rustls::pki_types::pem::PemObject as _;

    let Ok(Some(Ok(cert))) = CertificateDer::from_pem_slice(cert_pem.as_bytes()).next().map(Some) else {
        return true;
    };

    // Use x509-parser or a simple DER walk. For now, treat parse failure as expired.
    // The rcgen cert has a 14-day window; checking file mtime is a simpler proxy.
    false // TODO: implement proper expiry check during implementation
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_generates_valid_pem() {
        let (cert, key) = generate_self_signed().unwrap();
        assert!(cert.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(key.starts_with("-----BEGIN PRIVATE KEY-----"));
    }

    #[test]
    fn spki_hash_is_32_bytes() {
        let (cert, _) = generate_self_signed().unwrap();
        let hash = compute_spki_hash(&cert).unwrap();
        assert_eq!(hash.len(), 32, "SHA-256 should produce 32 bytes");
    }

    #[test]
    fn self_signed_reuses_unexpired_cert() {
        let dir = tempfile::tempdir().unwrap();
        let c1 = TlsConfig::self_signed(dir.path()).unwrap();
        let c2 = TlsConfig::self_signed(dir.path()).unwrap();
        assert_eq!(c1.spki_hash, c2.spki_hash, "should reuse the same cert");
    }

    #[test]
    fn export_and_read_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = TlsConfig::self_signed(dir.path()).unwrap();
        let hash_path = dir.path().join("cert-hash");
        cfg.export_hash(&hash_path).unwrap();
        let read_back = std::fs::read_to_string(&hash_path).unwrap();
        assert_eq!(read_back, cfg.spki_hash_hex());
    }

    #[test]
    fn pem_loading_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = generate_self_signed().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, &cert).unwrap();
        std::fs::write(&key_path, &key).unwrap();
        let cfg = TlsConfig::from_pem_files(&cert_path, &key_path).unwrap();
        assert_eq!(cfg.cert_pem, cert);
        assert_eq!(cfg.spki_hash.len(), 32);
    }
}
```

Note: the `ring` crate is pulled in transitively by `rustls` and `rcgen`. If it's not available directly, use `rustls`'s re-export or add `ring` as a workspace dep. The `time` crate is already used by `rcgen` — check if it needs to be added to `cairn-daemon`'s deps. Adjust the `compute_spki_hash` implementation based on what `wtransport` actually uses for `serverCertificateHashes` — check `wtransport`'s source during implementation.

- [ ] **Step 2: Register module and run tests**

Add `pub mod tls;` to `crates/cairn-daemon/src/lib.rs`.

Run: `cargo nextest run -p cairn-daemon -E 'test(~tls)'`
Expected: all 5 tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-daemon/src/tls.rs crates/cairn-daemon/src/lib.rs
git commit -m "feat: TLS cert generation, PEM loading, and SPKI hash export"
```

---

## Task 5: Listener configuration and daemon CLI

Replace `--socket` with `--listen` and add WT-specific flags. Update `DaemonConfig` to hold the new fields.

**Files:**
- Create: `crates/cairn-daemon/src/listen.rs`
- Modify: `crates/cairn-daemon/src/lib.rs`
- Modify: `crates/cairn-daemon/src/config.rs`
- Modify: `crates/cairn-daemon/src/main.rs`
- Modify: `crates/cairn-daemon/tests/common/mod.rs`
- Modify: `crates/cairn-daemon/tests/smoke.rs`

- [ ] **Step 1: Write listener config with tests**

Create `crates/cairn-daemon/src/listen.rs`:

```rust
//! Listener configuration parsed from `--listen` URIs.

use std::net::SocketAddr;
use std::path::PathBuf;

/// A resolved listener endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerConfig {
    /// Unix domain socket at the given path.
    Unix(PathBuf),
    /// WebTransport (HTTP/3 over QUIC) at the given address.
    WebTransport(SocketAddr),
}

/// Parse a `--listen` value into a `ListenerConfig`.
pub fn parse_listener(s: &str) -> anyhow::Result<ListenerConfig> {
    if s == "unix" {
        return Ok(ListenerConfig::Unix(crate::config::default_socket_path()));
    }
    if let Some(rest) = s.strip_prefix("unix://") {
        if rest.is_empty() {
            anyhow::bail!("`--listen unix://` requires a socket path");
        }
        return Ok(ListenerConfig::Unix(PathBuf::from(rest)));
    }
    if let Some(rest) = s.strip_prefix("wt://") {
        let addr: SocketAddr = rest
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid wt:// address {rest:?}: {e}"))?;
        return Ok(ListenerConfig::WebTransport(addr));
    }
    if s.starts_with('/') {
        return Ok(ListenerConfig::Unix(PathBuf::from(s)));
    }
    anyhow::bail!(
        "unrecognized --listen value {s:?}; expected `unix`, `unix:///path`, or `wt://host:port`"
    )
}

impl ListenerConfig {
    pub fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }

    pub fn is_wt(&self) -> bool {
        matches!(self, Self::WebTransport(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn bare_unix_uses_default_path() {
        let cfg = parse_listener("unix").unwrap();
        match cfg {
            ListenerConfig::Unix(p) => assert!(p.ends_with("cairn/cairn.sock"), "got {p:?}"),
            _ => panic!("expected Unix"),
        }
    }

    #[test]
    fn unix_uri_extracts_path() {
        let cfg = parse_listener("unix:///run/cairn/x.sock").unwrap();
        assert_eq!(cfg, ListenerConfig::Unix(Path::new("/run/cairn/x.sock").into()));
    }

    #[test]
    fn wt_uri_extracts_socket_addr() {
        let cfg = parse_listener("wt://0.0.0.0:4433").unwrap();
        assert_eq!(
            cfg,
            ListenerConfig::WebTransport("0.0.0.0:4433".parse().unwrap())
        );
    }

    #[test]
    fn bare_path_is_unix() {
        let cfg = parse_listener("/tmp/cairn.sock").unwrap();
        assert_eq!(cfg, ListenerConfig::Unix(Path::new("/tmp/cairn.sock").into()));
    }

    #[test]
    fn empty_unix_uri_rejected() {
        assert!(parse_listener("unix://").is_err());
    }

    #[test]
    fn invalid_wt_addr_rejected() {
        assert!(parse_listener("wt://not-an-addr").is_err());
    }

    #[test]
    fn unknown_scheme_rejected() {
        assert!(parse_listener("http://host:80").is_err());
    }
}
```

- [ ] **Step 2: Register module and run tests**

Add `pub mod listen;` to `crates/cairn-daemon/src/lib.rs`.

Run: `cargo nextest run -p cairn-daemon -E 'test(~listen)'`
Expected: all 7 tests pass

- [ ] **Step 3: Update DaemonConfig**

In `crates/cairn-daemon/src/config.rs`, add listener and WT fields. Keep the existing UDS fields for backwards compat with the `DaemonConfig::default()` pattern used in tests:

```rust
use std::path::PathBuf;
use std::time::Duration;

use crate::listen::ListenerConfig;

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Active listeners. Defaults to `[ListenerConfig::Unix(default_socket_path())]`.
    pub listeners: Vec<ListenerConfig>,
    // UDS-specific
    pub dir_mode: u32,
    pub socket_mode: u32,
    // WT-specific
    pub wt_cert: Option<PathBuf>,
    pub wt_key: Option<PathBuf>,
    pub wt_connect_timeout: Duration,
    pub wt_idle_timeout: Duration,
    // Auth
    pub auth_backends: Vec<String>,
    pub auth_timeout: Duration,
    // General
    pub shutdown_grace: Duration,
    pub default_shell: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listeners: vec![ListenerConfig::Unix(default_socket_path())],
            dir_mode: 0o700,
            socket_mode: 0o600,
            wt_cert: None,
            wt_key: None,
            wt_connect_timeout: Duration::from_secs(30),
            wt_idle_timeout: Duration::from_secs(300),
            auth_backends: vec!["none".into()],
            auth_timeout: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(5),
            default_shell: default_shell(),
        }
    }
}
```

Remove the old `socket_path` field. Any code that referenced `cfg.socket_path` needs to find the unix listener in `cfg.listeners` — update `serve.rs`'s `bind_with_cleanup` call site and the socket cleanup in `serve()`. The `config.rs` tests also need updating.

Update `config.rs` tests:

```rust
#[test]
fn defaults_are_conservative() {
    let c = DaemonConfig::default();
    assert_eq!(c.dir_mode, 0o700);
    assert_eq!(c.socket_mode, 0o600);
    assert_eq!(c.shutdown_grace, Duration::from_secs(5));
    assert_eq!(c.wt_connect_timeout, Duration::from_secs(30));
    assert_eq!(c.wt_idle_timeout, Duration::from_secs(300));
    assert_eq!(c.auth_timeout, Duration::from_secs(5));
    assert!(c.listeners.len() == 1);
    assert!(c.listeners[0].is_unix());
}
```

- [ ] **Step 4: Update main.rs CLI args**

Replace the `--socket` flag with `--listen` and add WT/auth flags:

```rust
#[derive(Parser)]
#[command(version, about = "The cairn session-manager daemon")]
struct Args {
    /// Transport listeners. Repeatable. `unix` or `unix:///path` for UDS,
    /// `wt://host:port` for WebTransport. Default: `unix`.
    #[arg(
        long,
        env = "CAIRN_LISTEN",
        value_delimiter = ',',
        value_parser = cairn_daemon::listen::parse_listener,
    )]
    listen: Vec<cairn_daemon::listen::ListenerConfig>,

    /// Octal permission mode for the UDS parent directory.
    #[arg(long, env = "CAIRN_DIR_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    dir_mode: Option<u32>,

    /// Octal permission mode for the UDS socket file.
    #[arg(long, env = "CAIRN_SOCKET_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    socket_mode: Option<u32>,

    /// PEM certificate file for WebTransport TLS.
    #[arg(long, env = "CAIRN_WT_CERT")]
    wt_cert: Option<std::path::PathBuf>,

    /// PEM private key file for WebTransport TLS.
    #[arg(long, env = "CAIRN_WT_KEY")]
    wt_key: Option<std::path::PathBuf>,

    /// Time to wait for the first wRPC invocation on a WT connection.
    #[arg(long, env = "CAIRN_WT_CONNECT_TIMEOUT", value_parser = humantime::parse_duration)]
    wt_connect_timeout: Option<std::time::Duration>,

    /// Close WT connections idle for this duration.
    #[arg(long, env = "CAIRN_WT_IDLE_TIMEOUT", value_parser = humantime::parse_duration)]
    wt_idle_timeout: Option<std::time::Duration>,

    /// Comma-separated auth backends, tried in order.
    #[arg(long, env = "CAIRN_AUTH", value_delimiter = ',', default_value = "none")]
    auth: Vec<String>,

    /// Time to wait for a first-message `meta.authenticate` call.
    #[arg(long, env = "CAIRN_AUTH_TIMEOUT", value_parser = humantime::parse_duration)]
    auth_timeout: Option<std::time::Duration>,

    /// How long to wait for sessions to exit on shutdown.
    #[arg(long, env = "CAIRN_SHUTDOWN_GRACE", value_parser = humantime::parse_duration)]
    shutdown_grace: Option<std::time::Duration>,

    /// Default shell for sessions that don't specify a command.
    #[arg(long, env = "CAIRN_DEFAULT_SHELL")]
    default_shell: Option<String>,

    /// `tracing-subscriber` filter directive.
    #[arg(
        long,
        env = "CAIRN_LOG",
        default_value = "info,cairn_daemon=info,cairn_pty=info"
    )]
    log: String,
}
```

Update the config construction in `main()`. Include validation warnings from the spec: warn if `--auth=none` with a non-loopback WT listener, warn if UDS-specific flags are set without a unix listener:

```rust
let mut cfg = cairn_daemon::config::DaemonConfig::default();
if !args.listen.is_empty() {
    cfg.listeners = args.listen;
}
if let Some(m) = args.dir_mode { cfg.dir_mode = m; }
if let Some(m) = args.socket_mode { cfg.socket_mode = m; }
if let Some(p) = args.wt_cert { cfg.wt_cert = Some(p); }
if let Some(p) = args.wt_key { cfg.wt_key = Some(p); }
if let Some(d) = args.wt_connect_timeout { cfg.wt_connect_timeout = d; }
if let Some(d) = args.wt_idle_timeout { cfg.wt_idle_timeout = d; }
cfg.auth_backends = args.auth;
if let Some(d) = args.auth_timeout { cfg.auth_timeout = d; }
if let Some(g) = args.shutdown_grace { cfg.shutdown_grace = g; }
if let Some(s) = args.default_shell { cfg.default_shell = s; }

// Validation warnings
let has_unix = cfg.listeners.iter().any(|l| l.is_unix());
let has_wt = cfg.listeners.iter().any(|l| l.is_wt());

if !has_unix && (args.dir_mode.is_some() || args.socket_mode.is_some()) {
    tracing::warn!("--dir-mode/--socket-mode ignored: no unix listener configured");
}
if !has_wt && (args.wt_cert.is_some() || args.wt_key.is_some()) {
    tracing::warn!("--wt-cert/--wt-key ignored: no wt:// listener configured");
}
if cfg.auth_backends == ["none"] {
    if let Some(addr) = cfg.listeners.iter().find_map(|l| match l {
        cairn_daemon::listen::ListenerConfig::WebTransport(a) if !a.ip().is_loopback() => Some(a),
        _ => None,
    }) {
        tracing::warn!(
            %addr,
            "--auth=none with non-loopback WT listener: connections will not be authenticated"
        );
    }
}
```

- [ ] **Step 5: Update serve.rs for new config shape**

`serve.rs` currently reads `daemon.cfg.socket_path`. Change it to find the unix listener from `daemon.cfg.listeners`:

In `serve()`, replace the `bind_with_cleanup(&daemon.cfg)` call with a lookup:

```rust
let unix_path = daemon.cfg.listeners.iter().find_map(|l| match l {
    crate::listen::ListenerConfig::Unix(p) => Some(p.clone()),
    _ => None,
});

if let Some(ref path) = unix_path {
    let listener = bind_with_cleanup(path, &daemon.cfg)?;
    // ... existing accept loop setup ...
}
```

Update `bind_with_cleanup` to take a `&Path` instead of a `&DaemonConfig`:

```rust
fn bind_with_cleanup(
    socket_path: &std::path::Path,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<tokio::net::UnixListener> {
```

Update the socket cleanup at shutdown:

```rust
if let Some(ref path) = unix_path {
    let _ = std::fs::remove_file(path);
}
```

- [ ] **Step 6: Update test harness and smoke test**

In `tests/common/mod.rs`, `DaemonHarness::start()` creates a `DaemonConfig` with a custom `socket_path`. Update to use the new `listeners` field:

```rust
let cfg = DaemonConfig {
    listeners: vec![ListenerConfig::Unix(socket_path.clone())],
    ..DaemonConfig::default()
};
```

Add `use cairn_daemon::listen::ListenerConfig;` to the imports.

In `tests/smoke.rs`, replace `CAIRN_SOCKET` with `CAIRN_LISTEN`:

```rust
.env("CAIRN_LISTEN", format!("unix://{}", socket.display()))
```

- [ ] **Step 7: Run all tests**

Run: `cargo nextest run -p cairn-daemon`
Expected: all tests pass

Run: `cargo clippy -p cairn-daemon --all-targets -- -D warnings`

- [ ] **Step 8: Commit**

```bash
git add crates/cairn-daemon/src/listen.rs crates/cairn-daemon/src/lib.rs \
  crates/cairn-daemon/src/config.rs crates/cairn-daemon/src/main.rs \
  crates/cairn-daemon/src/serve.rs \
  crates/cairn-daemon/tests/common/mod.rs crates/cairn-daemon/tests/smoke.rs
git commit -m "feat: --listen flag for individually selectable transports"
```

---

## Task 6: Daemon WebTransport accept loop

Wire the WT listener into `serve.rs`: bind a QUIC endpoint, accept connections, authenticate via the auth chain, and feed wRPC invocations into the pump.

**Files:**
- Modify: `crates/cairn-daemon/src/serve.rs`
- Modify: `crates/cairn-daemon/src/daemon.rs`

- [ ] **Step 1: Build the auth chain from config**

Add a helper to `daemon.rs` that constructs the `AuthChain` from `cfg.auth_backends`:

```rust
use crate::auth::{self, AuthChain};

impl Daemon {
    pub fn build_auth_chain(&self) -> anyhow::Result<AuthChain> {
        let mut backends: Vec<Box<dyn auth::AuthBackend>> = Vec::new();
        for name in &self.cfg.auth_backends {
            match name.as_str() {
                "none" => backends.push(Box::new(auth::none::NoneBackend)),
                "tailscale" => {
                    // Tailscale backend is added in Task 8
                    anyhow::bail!("tailscale auth backend not yet implemented");
                }
                other => anyhow::bail!("unknown auth backend: {other:?}"),
            }
        }
        if backends.is_empty() {
            anyhow::bail!("at least one --auth backend is required");
        }
        Ok(AuthChain::new(backends))
    }
}
```

- [ ] **Step 2: Add WT listener setup to serve()**

Add a function to `serve.rs` that builds the `wtransport::Endpoint` (server) from the config:

```rust
use std::net::SocketAddr;

async fn bind_wt(
    addr: SocketAddr,
    tls: &crate::tls::TlsConfig,
    idle_timeout: std::time::Duration,
) -> anyhow::Result<wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>> {
    use wtransport::ServerConfig;
    use wtransport::tls::Certificate;

    let cert = Certificate::new(
        vec![rustls::pki_types::CertificateDer::from(
            rustls::pki_types::pem::PemObject::from_pem_slice(tls.cert_pem.as_bytes())
                .next()
                .ok_or_else(|| anyhow::anyhow!("invalid cert PEM"))??,
        )],
        rustls::pki_types::PrivateKeyDer::from_pem_slice(tls.key_pem.as_bytes())
            .next()
            .ok_or_else(|| anyhow::anyhow!("invalid key PEM"))??,
    );

    let config = ServerConfig::builder()
        .with_bind_address(addr)
        .with_certificate(cert)
        .keep_alive_interval(Some(std::time::Duration::from_secs(15)))
        .max_idle_timeout(Some(idle_timeout))
        .expect("valid idle timeout")
        .build();

    let endpoint = wtransport::Endpoint::server(config)?;
    Ok(endpoint)
}
```

Note: the exact `wtransport` / `Certificate` API may differ — check the `wtransport 0.6` docs during implementation. The `ServerConfig::builder()` API is the entry point; adapt method names as needed.

- [ ] **Step 3: Add the WT accept loop**

Add a function that runs the per-connection WT serve loop:

```rust
/// Accept a single WT connection, authenticate it, and serve wRPC invocations.
async fn serve_wt_connection(
    conn: wtransport::Connection,
    auth_chain: &crate::auth::AuthChain,
    daemon: crate::daemon::Daemon,
    auth_timeout: std::time::Duration,
    shutdown: CancellationToken,
) {
    let peer_addr = conn.remote_address();
    let ctx = crate::auth::AuthContext {
        peer_addr,
        token: None,
    };

    // Phase 1: try transport-level auth
    let identity = match auth_chain.try_transport(&ctx).await {
        Ok(id) => id,
        Err(crate::auth::AuthError::NotApplicable) => {
            // Phase 2 would go here for FirstMessage backends (v1).
            // For v0, no FirstMessage backends exist, so reject.
            tracing::warn!(%peer_addr, "no auth backend accepted the connection");
            return;
        }
        Err(crate::auth::AuthError::Rejected(reason)) => {
            tracing::warn!(%peer_addr, %reason, "connection rejected by auth backend");
            return;
        }
    };

    tracing::info!(%peer_addr, identity = ?identity, "WT connection authenticated");
    let conn_ctx = ConnCtx { identity };

    // Wrap the connection for wRPC
    let wt_client = wrpc_transport_web::Client::from(conn);
    let wt_srv: Arc<wrpc_transport_web::Server> =
        Arc::new(wrpc_transport::Server::default());

    // Accept loop for this connection's bidirectional streams
    let accept_task = tokio::spawn({
        let wt_srv = Arc::clone(&wt_srv);
        let shutdown = shutdown.clone();
        let wt_client = wt_client.clone();
        async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    res = wt_srv.accept(&wt_client) => {
                        if res.is_err() { break; }
                    }
                }
            }
        }
    });

    // Serve invocations for this connection
    match cairn_protocol::serve(wt_srv.as_ref(), daemon.clone()).await {
        Ok(invocations) => {
            use futures::stream::{StreamExt as _, select_all};
            let mut invocations = select_all(
                invocations
                    .into_iter()
                    .map(|(i, n, s)| s.map(move |r| (i, n, r))),
            );
            // Note: conn_ctx needs to be injected into each invocation.
            // The exact mechanism depends on how cairn_protocol::serve()
            // threads the context. Since the WT Server has Context = (),
            // we may need to modify the Handler impl or wrap invocations.
            // Implementation will need to resolve this — see spec note about
            // "per-connection task maps () context to pre-resolved ConnCtx."
            while let Some((_i, _n, res)) = invocations.next().await {
                if let Ok(fut) = res {
                    tokio::spawn(fut);
                }
            }
        }
        Err(e) => {
            tracing::error!(%peer_addr, error = %e, "failed to set up WT invocation serve");
        }
    }

    accept_task.abort();
}
```

**Important implementation note:** The `cairn_protocol::serve()` function is generated by `wit-bindgen-wrpc` and calls `Handler<Ctx>` where `Ctx` matches the server's `Accept::Context`. The WT `Server` has `Context = ()`, but our `Handler` is `impl Handler<ConnCtx>`. Resolving this requires one of:

1. Implement `Handler<()>` for a wrapper that injects `ConnCtx` (adapter pattern)
2. Create a custom `Accept` wrapper for `wrpc_transport_web::Client` that yields `ConnCtx` instead of `()`, then use a `Server<ConnCtx, SendStream, RecvStream, ConnHandler>`
3. Add a second `Handler<()>` impl with a `ConnCtx` stored in the `Daemon` (not clean — Daemon is shared)

Option 2 is cleanest. Create a wrapper struct:

```rust
/// Wraps a `wrpc_transport_web::Client` to inject a pre-resolved `ConnCtx`
/// into the wRPC Accept context, so the same `Handler<ConnCtx>` works for
/// both UDS and WT.
struct AuthenticatedWtAccept {
    inner: wrpc_transport_web::Client,
    ctx: ConnCtx,
}

impl Accept for &AuthenticatedWtAccept {
    type Context = ConnCtx;
    type Outgoing = wtransport::SendStream;
    type Incoming = wtransport::RecvStream;

    async fn accept(&self) -> std::io::Result<(ConnCtx, Self::Outgoing, Self::Incoming)> {
        let ((), tx, rx) = self.inner.accept().await?;
        Ok((self.ctx.clone(), tx, rx))
    }
}
```

Then in `serve_wt_connection`, use `AuthenticatedWtAccept` instead of raw `wrpc_transport_web::Client`, and create a `wrpc_transport::Server::default()` with matching type params.

The implementer should verify this approach compiles against the actual `wrpc_transport::Server` type constraints. The `Server` type is `Server<Ctx, Tx, Rx, H>` — check if `H` (the ConnHandler) needs to match. If `wrpc_transport_web::ConnHandler` is required, it can be specified as the type parameter.

- [ ] **Step 4: Wire the WT outer loop into serve()**

Update `serve()` to conditionally start the WT endpoint alongside UDS. The structure becomes:

```rust
pub async fn serve(
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    let auth_chain = Arc::new(daemon.build_auth_chain()?);
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();

    // ── UDS listener ────────────────────────────────────────────────
    let unix_path = daemon.cfg.listeners.iter().find_map(|l| match l {
        crate::listen::ListenerConfig::Unix(p) => Some(p.clone()),
        _ => None,
    });

    if let Some(ref path) = unix_path {
        let listener = bind_with_cleanup(path, &daemon.cfg)?;
        tracing::info!(socket = %path.display(), "UDS listening");
        let uds_srv = Arc::new(wrpc_transport::Server::default());
        let pl = Arc::new(PeerCredListener(listener));

        // UDS accept loop
        tasks.push(tokio::spawn({
            let srv = Arc::clone(&uds_srv);
            let pl = Arc::clone(&pl);
            let shutdown = shutdown.clone();
            async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        res = srv.accept(pl.as_ref()) => {
                            if res.is_err() { break; }
                        }
                    }
                }
            }
        }));

        // UDS invocation pump
        let invocations = cairn_protocol::serve(uds_srv.as_ref(), daemon.clone()).await?;
        tasks.push(tokio::spawn(pump_invocations(invocations)));
    }

    // ── WebTransport listener ───────────────────────────────────────
    let wt_addr = daemon.cfg.listeners.iter().find_map(|l| match l {
        crate::listen::ListenerConfig::WebTransport(addr) => Some(*addr),
        _ => None,
    });

    if let Some(addr) = wt_addr {
        let tls = resolve_tls(&daemon.cfg)?;
        let hash_path = runtime_dir().join("cert-hash");
        tls.export_hash(&hash_path)?;

        let endpoint = bind_wt(addr, &tls, daemon.cfg.wt_idle_timeout).await?;
        tracing::info!(%addr, hash = %tls.spki_hash_hex(), "WT listening");

        tasks.push(tokio::spawn({
            let daemon = daemon.clone();
            let auth_chain = Arc::clone(&auth_chain);
            let shutdown = shutdown.clone();
            async move {
                loop {
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        incoming = endpoint.accept() => {
                            let conn = match incoming.await.accept().await {
                                Ok(conn) => conn,
                                Err(e) => {
                                    tracing::debug!(error = %e, "WT accept error");
                                    continue;
                                }
                            };
                            let daemon = daemon.clone();
                            let auth_chain = Arc::clone(&auth_chain);
                            let shutdown = shutdown.clone();
                            tokio::spawn(async move {
                                serve_wt_connection(
                                    conn,
                                    &auth_chain,
                                    daemon,
                                    shutdown,
                                ).await;
                            });
                        }
                    }
                }
            }
        }));
    }

    if tasks.is_empty() {
        anyhow::bail!("no listeners configured; provide at least one --listen value");
    }

    // ── Wait for shutdown ───────────────────────────────────────────
    shutdown.cancelled().await;
    drain_sessions(&daemon, daemon.cfg.shutdown_grace).await;
    for task in &tasks {
        task.abort();
    }
    if let Some(ref path) = unix_path {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}
```

Add helper functions:

```rust
fn resolve_tls(cfg: &crate::config::DaemonConfig) -> anyhow::Result<crate::tls::TlsConfig> {
    match (&cfg.wt_cert, &cfg.wt_key) {
        (Some(cert), Some(key)) => crate::tls::TlsConfig::from_pem_files(cert, key),
        (None, None) => crate::tls::TlsConfig::self_signed(&runtime_dir().join("tls")),
        _ => anyhow::bail!("--wt-cert and --wt-key must both be provided, or both omitted"),
    }
}

fn runtime_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    base.join("cairn")
}

async fn pump_invocations(
    invocations: Vec<(
        &str, &str,
        impl futures::Stream<Item = anyhow::Result<impl std::future::Future<Output = ()> + Send>> + Unpin,
    )>,
) {
    // Implementation mirrors the existing pump. The exact type depends on what
    // cairn_protocol::serve returns — use the same pattern as the current code.
    use futures::stream::{StreamExt as _, select_all};
    let mut merged = select_all(
        invocations
            .into_iter()
            .map(|(i, n, s)| s.map(move |r| (i, n, r))),
    );
    while let Some((_i, _n, res)) = merged.next().await {
        if let Ok(fut) = res {
            tokio::spawn(fut);
        }
    }
}
```

Note: the `pump_invocations` function signature will need adjustment based on the actual return type of `cairn_protocol::serve()`. The existing inline code in `serve()` already handles this — extract it into a helper. The `&str` lifetimes may require owned strings or `'static` bounds; adjust during implementation.

- [ ] **Step 5: Verify UDS still works**

Run: `cargo nextest run -p cairn-daemon`
Expected: all existing tests pass (no WT tests yet — the WT path is only activated when a `wt://` listener is configured)

- [ ] **Step 6: Commit**

```bash
git add crates/cairn-daemon/src/serve.rs crates/cairn-daemon/src/daemon.rs
git commit -m "feat: WebTransport accept loop with auth chain integration"
```

---

## Task 7: Client WebTransport endpoint and Client enum

Add the `WebTransport` variant to `Endpoint` and create a `Client` enum with a forwarding `Invoke` impl.

**Files:**
- Modify: `crates/cairn-client/src/connect.rs`
- Modify: `crates/cairn-client/src/cli.rs`

- [ ] **Step 1: Write new endpoint parsing tests**

Add tests to `connect.rs` for WT URIs:

```rust
#[test]
fn wt_uri_yields_webtransport_endpoint() {
    let ep = Endpoint::resolve(Some("wt://192.168.1.10:4433"), None).unwrap();
    assert!(matches!(ep, Endpoint::WebTransport { .. }));
    assert_eq!(ep.label(), "wt://192.168.1.10:4433");
}

#[test]
fn https_uri_aliases_to_wt() {
    let ep = Endpoint::resolve(Some("https://myhost.ts.net:4433"), None).unwrap();
    assert!(matches!(ep, Endpoint::WebTransport { .. }));
}

#[test]
fn wt_is_gone_always_returns_false() {
    let ep = Endpoint::resolve(Some("wt://192.168.1.10:4433"), None).unwrap();
    assert!(!ep.is_gone());
}
```

- [ ] **Step 2: Run tests to see them fail**

Run: `cargo nextest run -p cairn -E 'test(~wt_uri)'`
Expected: FAIL — `Endpoint::WebTransport` doesn't exist yet

- [ ] **Step 3: Implement Endpoint::WebTransport and Client enum**

Rewrite `connect.rs`:

```rust
//! Daemon endpoint resolution and multi-transport client.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Result, bail};

/// A resolved daemon endpoint.
#[derive(Debug)]
pub enum Endpoint {
    Unix(PathBuf),
    WebTransport {
        addr: SocketAddr,
        host: String,
        cert_hash: Option<String>,
    },
}

impl Endpoint {
    /// Resolve from `--daemon` / `CAIRN_DAEMON` or the platform default socket.
    /// `cert_hash` is from `--cert-hash` / `CAIRN_CERT_HASH`.
    pub fn resolve(daemon: Option<&str>, cert_hash: Option<String>) -> Result<Self> {
        match daemon {
            None => Ok(Self::Unix(default_socket())),
            Some(s) => Self::from_uri(s, cert_hash),
        }
    }

    fn from_uri(s: &str, cert_hash: Option<String>) -> Result<Self> {
        if let Some(rest) = s.strip_prefix("unix://") {
            if rest.is_empty() {
                bail!("`--daemon unix://` has no socket path");
            }
            return Ok(Self::Unix(PathBuf::from(rest)));
        }
        if s.starts_with('/') {
            return Ok(Self::Unix(PathBuf::from(s)));
        }
        if let Some(rest) = s.strip_prefix("wt://") {
            return Self::parse_wt(rest, cert_hash);
        }
        if let Some(rest) = s.strip_prefix("https://") {
            return Self::parse_wt(rest, cert_hash);
        }
        if s.starts_with("ws://") || s.starts_with("wss://") {
            bail!("WebSocket transport is not supported; use wt:// for WebTransport");
        }
        bail!("unrecognized --daemon endpoint {s:?}");
    }

    fn parse_wt(host_port: &str, cert_hash: Option<String>) -> Result<Self> {
        // Try parsing as SocketAddr directly
        let addr: SocketAddr = host_port
            .parse()
            .or_else(|_| {
                // If it has a hostname, resolve it. For now, require host:port format.
                bail!("invalid WebTransport address {host_port:?}; expected host:port")
            })?;
        let host = host_port
            .rsplit_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| addr.ip().to_string());

        // Auto-load cert hash from file for localhost endpoints
        let cert_hash = cert_hash.or_else(|| {
            if addr.ip().is_loopback() {
                let hash_path = runtime_dir().join("cert-hash");
                std::fs::read_to_string(&hash_path).ok()
            } else {
                None
            }
        });

        Ok(Self::WebTransport { addr, host, cert_hash })
    }

    pub fn label(&self) -> String {
        match self {
            Self::Unix(p) => format!("unix://{}", p.display()),
            Self::WebTransport { addr, .. } => format!("wt://{addr}"),
        }
    }

    pub fn is_gone(&self) -> bool {
        match self {
            Self::Unix(p) => !p.exists(),
            Self::WebTransport { .. } => false,
        }
    }

    pub async fn client(&self) -> Result<Client> {
        match self {
            Self::Unix(p) => Ok(Client::Unix(wrpc_transport::unix::Client::from(p.clone()))),
            Self::WebTransport { addr, host, cert_hash } => {
                let config = build_wt_client_config(host, cert_hash.as_deref())?;
                let url = format!("https://{addr}");
                let endpoint = wtransport::Endpoint::client(config)?;
                let conn = endpoint
                    .connect(&url)
                    .await
                    .map_err(|e| anyhow::anyhow!("WebTransport connect to {addr}: {e}"))?;
                Ok(Client::WebTransport(wrpc_transport_web::Client::from(conn)))
            }
        }
    }
}

fn build_wt_client_config(
    _host: &str,
    cert_hash: Option<&str>,
) -> Result<wtransport::ClientConfig> {
    use wtransport::ClientConfig;

    let mut builder = ClientConfig::builder().with_bind_default();

    if let Some(hash_hex) = cert_hash {
        let hash_bytes: Vec<u8> = (0..hash_hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hash_hex[i..i + 2], 16))
            .collect::<Result<_, _>>()
            .map_err(|e| anyhow::anyhow!("invalid cert-hash hex: {e}"))?;
        // wtransport uses server_certificate_hashes for pinning.
        // Check wtransport 0.6 API for exact method name during implementation.
        builder = builder.with_server_certificate_hashes(vec![
            wtransport::tls::Sha256Digest::new(hash_bytes.try_into().map_err(|_| {
                anyhow::anyhow!("cert-hash must be 32 bytes (64 hex chars)")
            })?),
        ]);
    } else {
        builder = builder.with_native_certs();
    }

    Ok(builder.build())
}

fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn")
}

fn default_socket() -> PathBuf {
    runtime_dir().join("cairn.sock")
}

/// Multi-transport wRPC client. Forwards `Invoke` to the inner client.
pub enum Client {
    Unix(wrpc_transport::unix::Client<PathBuf>),
    WebTransport(wrpc_transport_web::Client),
}
```

Note: the exact `wtransport::ClientConfig` builder API (method names, chain order) should be verified against `wtransport 0.6` docs during implementation. The `with_server_certificate_hashes`, `with_native_certs`, and `Sha256Digest` types may have different names.

- [ ] **Step 4: Implement `Invoke` for `Client` enum**

The `wrpc_transport::Invoke` trait needs a forwarding impl. Check the trait signature in `wrpc-transport 0.28`:

```rust
impl wrpc_transport::Invoke for Client {
    type Context = ();
    type Outgoing = Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>;
    type Incoming = Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>;

    async fn invoke<P>(
        &self,
        cx: Self::Context,
        instance: &str,
        func: &str,
        params: bytes::Bytes,
        paths: impl AsRef<[P]> + Send,
    ) -> anyhow::Result<(Self::Outgoing, Self::Incoming)>
    where
        P: AsRef<[Option<usize>]> + Send + Sync,
    {
        match self {
            Self::Unix(c) => {
                let (tx, rx) = c.invoke(cx, instance, func, params, paths).await?;
                Ok((Box::new(tx), Box::new(rx)))
            }
            Self::WebTransport(c) => {
                let (tx, rx) = c.invoke(cx, instance, func, params, paths).await?;
                Ok((Box::new(tx), Box::new(rx)))
            }
        }
    }
}
```

**Critical note:** The `Invoke` trait's associated types (`Outgoing`, `Incoming`) differ between the UDS and WT clients. Type-erasing to `Box<dyn AsyncWrite>` / `Box<dyn AsyncRead>` is one approach. However, the `cairn_protocol::client::*` generated functions may require specific type bounds. **Check the generated function signatures first** — if they're generic over `C: Invoke`, the boxed approach works. If they require specific stream types, a different strategy is needed (e.g., separate `invoke_*` call sites per transport, behind a match).

An alternative that avoids type erasure: keep the match at the call site. Instead of `endpoint.client()` returning a unified `Client`, have the call site match on `Endpoint` and call the protocol functions directly:

```rust
// In each command module, instead of:
//   let client = endpoint.client();
//   cairn_protocol::client::..::version(&client, ()).await
// Do:
//   endpoint.invoke(|c| cairn_protocol::client::..::version(c, ())).await
```

The implementer should try the enum approach first; fall back to a macro or helper if type erasure doesn't work with the generated code.

- [ ] **Step 5: Update cli.rs**

Add `--cert-hash` global option to `crates/cairn-client/src/cli.rs`:

```rust
/// SPKI hash of the server certificate for WebTransport pinning.
/// Used when connecting to a daemon with a self-signed certificate.
#[clap(
    long,
    env = "CAIRN_CERT_HASH",
    global = true,
    help_heading = "Global options"
)]
pub cert_hash: Option<String>,
```

Update the `--daemon` help text: replace the `ws://`/`wss://` references with `wt://`/`https://`.

- [ ] **Step 6: Update Endpoint::resolve call sites**

Grep for `Endpoint::resolve` in `main.rs` and other command modules. Update to pass `cert_hash`:

```rust
let endpoint = Endpoint::resolve(cli.daemon.as_deref(), cli.cert_hash.clone())?;
```

If `client()` is now async, update call sites to `.await`.

- [ ] **Step 7: Update existing connect.rs tests**

Update tests for the new `resolve` signature (now takes a second `cert_hash` arg):

```rust
#[test]
fn default_resolves_to_unix_with_cairn_sock_suffix() {
    match Endpoint::resolve(None, None).unwrap() {
        Endpoint::Unix(p) => assert!(p.ends_with("cairn/cairn.sock"), "got {p:?}"),
        _ => panic!("expected Unix"),
    }
}
```

Update all other tests similarly — add `None` as the second argument.

- [ ] **Step 8: Run tests**

Run: `cargo nextest run -p cairn`
Expected: all tests pass (UDS path unchanged, WT tests pass)

Run: `cargo clippy -p cairn --all-targets -- -D warnings`

- [ ] **Step 9: Commit**

```bash
git add crates/cairn-client/src/connect.rs crates/cairn-client/src/cli.rs \
  crates/cairn-client/src/main.rs
git commit -m "feat: WebTransport client endpoint with cert-hash pinning"
```

---

## Task 8: Tailscale auth backend

Implement the `tailscale` auth backend using the Tailscale LocalAPI `whois` endpoint.

**Files:**
- Create: `crates/cairn-daemon/src/auth/tailscale.rs`
- Modify: `crates/cairn-daemon/src/auth/mod.rs`
- Modify: `crates/cairn-daemon/src/daemon.rs`

- [ ] **Step 1: Write the Tailscale backend with tests**

Create `crates/cairn-daemon/src/auth/tailscale.rs`:

```rust
//! Tailscale auth backend: resolves identity via the LocalAPI `whois` endpoint.

use crate::auth::{AuthBackend, AuthContext, AuthError, AuthPhase};
use crate::identity::Identity;

/// Resolves peer identity by calling the Tailscale LocalAPI.
pub struct TailscaleBackend {
    /// LocalAPI base URL. Platform-dependent:
    /// - Linux: `http://localhost/localapi/v0` over Unix socket at `/var/run/tailscale/tailscaled.sock`
    /// - macOS: `http://127.0.0.1:41112/localapi/v0` over TCP
    client: TailscaleClient,
}

impl TailscaleBackend {
    pub fn new() -> anyhow::Result<Self> {
        let client = TailscaleClient::new()?;
        Ok(Self { client })
    }
}

impl AuthBackend for TailscaleBackend {
    async fn authenticate(&self, ctx: &AuthContext) -> Result<Identity, AuthError> {
        match self.client.whois(&ctx.peer_addr).await {
            Ok(info) => Ok(Identity::Tailscale {
                login: info.login,
                display_name: info.display_name,
                node: info.node,
            }),
            Err(WhoisError::NotFound) => Err(AuthError::NotApplicable),
            Err(WhoisError::Forbidden(reason)) => Err(AuthError::Rejected(reason)),
            Err(WhoisError::Unavailable(e)) => {
                tracing::warn!(error = %e, "tailscale LocalAPI unavailable");
                Err(AuthError::NotApplicable)
            }
        }
    }

    fn phase(&self) -> AuthPhase {
        AuthPhase::Transport
    }
}

struct WhoisInfo {
    login: String,
    display_name: String,
    node: String,
}

enum WhoisError {
    NotFound,
    Forbidden(String),
    Unavailable(String),
}

/// Thin HTTP client for the Tailscale LocalAPI.
struct TailscaleClient {
    /// On macOS: TCP to 127.0.0.1:41112. On Linux: Unix socket.
    base_url: String,
}

impl TailscaleClient {
    fn new() -> anyhow::Result<Self> {
        let base_url = if cfg!(target_os = "macos") {
            "http://127.0.0.1:41112/localapi/v0".to_string()
        } else {
            // Linux: use Unix socket. The HTTP client needs a Unix socket connector.
            "http://localhost/localapi/v0".to_string()
        };
        Ok(Self { base_url })
    }

    async fn whois(&self, addr: &std::net::SocketAddr) -> Result<WhoisInfo, WhoisError> {
        let url = format!("{}/whois?addr={addr}", self.base_url);

        // Use hyper for the HTTP request. On Linux, this needs a Unix socket
        // connector; on macOS, standard TCP.
        let body = self.http_get(&url).await.map_err(WhoisError::Unavailable)?;

        let json: serde_json::Value =
            serde_json::from_str(&body).map_err(|e| WhoisError::Unavailable(e.to_string()))?;

        let user_profile = json
            .get("UserProfile")
            .ok_or(WhoisError::NotFound)?;
        let node = json
            .get("Node")
            .and_then(|n| n.get("ComputedName"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown")
            .to_string();
        let login = user_profile
            .get("LoginName")
            .and_then(|v| v.as_str())
            .ok_or(WhoisError::NotFound)?
            .to_string();
        let display_name = user_profile
            .get("DisplayName")
            .and_then(|v| v.as_str())
            .unwrap_or(&login)
            .to_string();

        Ok(WhoisInfo { login, display_name, node })
    }

    async fn http_get(&self, url: &str) -> Result<String, String> {
        // Implementation depends on platform:
        // macOS: standard TCP HTTP GET to 127.0.0.1:41112
        // Linux: HTTP over Unix socket at /var/run/tailscale/tailscaled.sock
        //
        // Use hyper with appropriate connector. During implementation,
        // check if a simpler approach (tokio::net::UnixStream + raw HTTP)
        // works to avoid the full hyper client setup.
        //
        // For now, use reqwest or hyper — the exact HTTP client setup is
        // an implementation detail.
        todo!("implement HTTP GET to Tailscale LocalAPI")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_whois_response() {
        let json = serde_json::json!({
            "Node": {
                "ComputedName": "myhost",
            },
            "UserProfile": {
                "LoginName": "user@example.com",
                "DisplayName": "Test User",
            }
        });

        let user_profile = json.get("UserProfile").unwrap();
        let login = user_profile.get("LoginName").unwrap().as_str().unwrap();
        let display_name = user_profile.get("DisplayName").unwrap().as_str().unwrap();
        let node = json.get("Node").unwrap().get("ComputedName").unwrap().as_str().unwrap();

        assert_eq!(login, "user@example.com");
        assert_eq!(display_name, "Test User");
        assert_eq!(node, "myhost");
    }

    #[test]
    fn parse_whois_missing_user_profile() {
        let json: serde_json::Value = serde_json::json!({
            "Node": { "ComputedName": "myhost" }
        });
        assert!(json.get("UserProfile").is_none());
    }

    #[tokio::test]
    async fn backend_returns_not_applicable_on_unavailable() {
        // When the LocalAPI is unreachable, the backend should return
        // NotApplicable so the chain can try the next backend.
        // This test verifies the error mapping, not the actual HTTP call.
        let err = WhoisError::Unavailable("connection refused".into());
        assert!(matches!(
            map_whois_error(err),
            Err(AuthError::NotApplicable)
        ));
    }
}

fn map_whois_error(e: WhoisError) -> Result<Identity, AuthError> {
    match e {
        WhoisError::NotFound => Err(AuthError::NotApplicable),
        WhoisError::Forbidden(reason) => Err(AuthError::Rejected(reason)),
        WhoisError::Unavailable(_) => Err(AuthError::NotApplicable),
    }
}
```

- [ ] **Step 2: Register module and update daemon.rs**

Add `pub mod tailscale;` to `crates/cairn-daemon/src/auth/mod.rs`.

Update `Daemon::build_auth_chain` in `daemon.rs`:

```rust
"tailscale" => {
    backends.push(Box::new(
        auth::tailscale::TailscaleBackend::new()
            .map_err(|e| anyhow::anyhow!("tailscale auth backend init: {e}"))?,
    ));
}
```

- [ ] **Step 3: Run tests**

Run: `cargo nextest run -p cairn-daemon -E 'test(~tailscale)'`
Expected: parsing tests pass. The `todo!()` in `http_get` is only reached at runtime, not in these tests.

- [ ] **Step 4: Implement the HTTP client**

Fill in `TailscaleClient::http_get()`. On macOS (TCP):

```rust
async fn http_get(&self, url: &str) -> Result<String, String> {
    use http_body_util::BodyExt as _;
    use hyper::Request;
    use hyper_util::client::legacy::Client;
    use hyper_util::rt::TokioExecutor;

    let client = Client::builder(TokioExecutor::new())
        .build_http::<http_body_util::Empty<bytes::Bytes>>();

    let req = Request::get(url)
        .body(http_body_util::Empty::new())
        .map_err(|e| e.to_string())?;

    let resp = client.request(req).await.map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("LocalAPI returned {}", resp.status()));
    }

    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .to_bytes();

    String::from_utf8(body.to_vec()).map_err(|e| e.to_string())
}
```

For Linux (Unix socket), use `hyper_util`'s Unix socket support or a raw `tokio::net::UnixStream` HTTP request. This is platform-specific — use `#[cfg(target_os = "...")]` to select the transport. The macOS TCP path covers the user's primary platform.

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-daemon/src/auth/tailscale.rs crates/cairn-daemon/src/auth/mod.rs \
  crates/cairn-daemon/src/daemon.rs
git commit -m "feat: Tailscale auth backend via LocalAPI whois"
```

---

## Task 9: Integration tests

End-to-end test: daemon with WT listener, client connects via WT, exercises basic operations.

**Files:**
- Modify: `crates/cairn-daemon/tests/common/mod.rs`
- Create: `crates/cairn-daemon/tests/wt_smoke.rs`

- [ ] **Step 1: Extend DaemonHarness for WT**

Update `tests/common/mod.rs` to support starting a daemon with both UDS and WT listeners:

```rust
use cairn_daemon::listen::ListenerConfig;

pub struct DaemonHarness {
    pub socket_path: PathBuf,
    pub wt_addr: Option<std::net::SocketAddr>,
    pub cert_hash: Option<String>,
    _tmp: tempfile::TempDir,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl DaemonHarness {
    pub async fn start() -> Self {
        Self::start_with_listeners(vec![], vec!["none".into()]).await
    }

    pub async fn start_with_listeners(
        extra_listeners: Vec<ListenerConfig>,
        auth: Vec<String>,
    ) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");

        let mut listeners = vec![ListenerConfig::Unix(socket_path.clone())];
        listeners.extend(extra_listeners);

        let cfg = DaemonConfig {
            listeners,
            auth_backends: auth,
            ..DaemonConfig::default()
        };
        let daemon = Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone()));

        // Poll until UDS socket appears
        for _ in 0..100 {
            if socket_path.exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(socket_path.exists(), "daemon socket did not appear in time");

        Self {
            socket_path,
            wt_addr: None,
            cert_hash: None,
            _tmp: tmp,
            shutdown,
            task,
        }
    }

    pub fn client(&self) -> wrpc_transport::unix::Client<PathBuf> {
        wrpc_transport::unix::Client::from(self.socket_path.clone())
    }
}
```

The WT harness variant is more complex — it needs to start a WT listener on port 0, discover the bound port, and provide the cert hash for client pinning. The exact setup depends on how `wtransport::Endpoint::server` reports its bound address. Implement during this task:

```rust
impl DaemonHarness {
    pub async fn start_with_wt() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let socket_path = tmp.path().join("cairn").join("cairn.sock");
        let tls_dir = tmp.path().join("tls");

        let tls = cairn_daemon::tls::TlsConfig::self_signed(&tls_dir).unwrap();
        let cert_hash = tls.spki_hash_hex();

        let cfg = DaemonConfig {
            listeners: vec![
                ListenerConfig::Unix(socket_path.clone()),
                ListenerConfig::WebTransport("127.0.0.1:0".parse().unwrap()),
            ],
            auth_backends: vec!["none".into()],
            ..DaemonConfig::default()
        };

        // ... start daemon, discover bound WT port ...
        todo!("discover bound WT port from the endpoint");
    }
}
```

Note: discovering the bound port when using `127.0.0.1:0` requires `wtransport::Endpoint::local_addr()` or similar. Check the API during implementation. If not available, use a fixed port for tests (less ideal).

- [ ] **Step 2: Write WT smoke test**

Create `crates/cairn-daemon/tests/wt_smoke.rs`:

```rust
//! Smoke test: daemon with WT listener, client connects via WebTransport.

mod common;

use common::DaemonHarness;

#[tokio::test]
async fn wt_version_round_trip() {
    let harness = DaemonHarness::start_with_wt().await;
    let wt_addr = harness.wt_addr.expect("WT addr");
    let cert_hash = harness.cert_hash.as_ref().expect("cert hash");

    // Build a WT client with cert hash pinning
    let config = wtransport::ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes(vec![/* parse cert_hash */])
        .build();
    let endpoint = wtransport::Endpoint::client(config).unwrap();
    let conn = endpoint
        .connect(format!("https://{wt_addr}"))
        .await
        .unwrap();
    let client = wrpc_transport_web::Client::from(conn);

    let info = cairn_protocol::client::cairn::daemon::meta::version(&client, ())
        .await
        .expect("version via WT");
    assert!(info.daemon.starts_with("cairn-daemon/"));
}
```

- [ ] **Step 3: Run the test**

Run: `cargo nextest run -p cairn-daemon -E 'test(~wt_)'`
Expected: PASS — full round-trip over QUIC/WebTransport

- [ ] **Step 4: Add UDS+WT coexistence test**

```rust
#[tokio::test]
async fn uds_and_wt_coexist() {
    let harness = DaemonHarness::start_with_wt().await;

    // UDS client
    let uds_client = harness.client();
    let uds_info = cairn_protocol::client::cairn::daemon::meta::version(&uds_client, ())
        .await
        .expect("version via UDS");

    // WT client (same setup as above)
    // ...
    let wt_info = /* ... */;

    assert_eq!(uds_info.daemon, wt_info.daemon);
}
```

- [ ] **Step 5: Commit**

```bash
git add crates/cairn-daemon/tests/
git commit -m "test: WebTransport integration tests with DaemonHarness"
```

---

## Task 10: Update cairn-client integration tests and existing tests

Ensure all existing tests compile and pass with the new `Endpoint::resolve` signature and `Client` enum.

**Files:**
- Modify: `crates/cairn-client/tests/` (any files that use `Endpoint::resolve`)
- Modify: `crates/cairn-client/src/main.rs` (update `client()` call sites if now async)

- [ ] **Step 1: Grep for all Endpoint::resolve call sites**

Run: `grep -rn "Endpoint::resolve" crates/cairn-client/`

Update each call to pass the `cert_hash` parameter (usually `None` for UDS tests).

- [ ] **Step 2: Update client() calls if async**

If `Endpoint::client()` became async (for WT connection setup), update all call sites to `.await`. For UDS, the connection is still synchronous (just stores a path), so this may require making `client()` async only for the WT variant, or making the whole method async.

- [ ] **Step 3: Run full test suite**

Run: `cargo nextest run --workspace`
Expected: all tests pass

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore: update tests for WebTransport-aware Endpoint and Client"
```

---

## Implementation notes

### wrpc-transport-web API verification

The code in this plan uses API names from the upstream `wrpc-transport-web 0.2.0` and `wtransport 0.6` crates. Several method names (`with_server_certificate_hashes`, `Sha256Digest`, `with_native_certs`, `Certificate::new`, `ServerConfig::builder`) are based on the research and may differ in the actual crate versions. **Verify each against the real API docs/source during implementation.** The plan captures the intent; the exact method names are secondary.

### Context mapping for WT serve loop (Task 6)

The most architecturally complex piece is injecting `ConnCtx` into wRPC invocations on the WT side. The plan describes the `AuthenticatedWtAccept` wrapper approach. If this doesn't type-check against `wrpc_transport::Server`, alternative approaches:

1. **Separate `Handler` impl:** Create a `WtDaemon` wrapper that implements `Handler<()>` and stores the `ConnCtx` per-connection. Delegates to the real handler methods.
2. **Skip cairn_protocol::serve for WT:** Manually match on wRPC instance+function names and dispatch to handler methods. More boilerplate but full control.
3. **Thread-local / task-local context:** Store `ConnCtx` in a `tokio::task_local!` and have the handler read it. Fragile but minimal code change.

The implementer should try approach (1) from the plan first, then fall back.

### Dependency version pins

If `wrpc-transport-web 0.2.0` requires `wtransport ^0.6.1` and `wrpc-transport ^0.28.3`, verify these don't conflict with the workspace's `wrpc-transport = "0.28"`. If they do, the workspace dep may need a version bump.

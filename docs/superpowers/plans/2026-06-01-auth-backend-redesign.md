# Auth Backend Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Separate UDS identity (OS peer creds) from network auth (backend chain), remove `NoneBackend`, model transport-specific auth material as a discriminated union (`TransportContext`), and validate config in `Daemon::new`.

**Architecture:** `AuthContext` gains a `TransportContext` enum whose variants carry transport-specific material. Auth backends match on the variant to self-select. UDS identity resolution stays in `PeerCredListener` and never enters the auth chain. `Daemon::new` becomes the validation chokepoint.

**Tech Stack:** Rust, clap (ValueEnum), anyhow, tracing, cargo-nextest

**Spec:** `docs/superpowers/specs/2026-06-01-auth-backend-redesign.md`

---

### Task 1: Add `TransportContext` and update `AuthContext`

**Files:**
- Modify: `crates/cairn-daemon/src/auth/mod.rs`

- [ ] **Step 1: Write test for `TransportContext` in `AuthContext`**

Add a test to `auth::tests` that constructs an `AuthContext` with the new `TransportContext::WebTransport` variant and passes it through the chain. This replaces the existing `test_ctx()` helper.

In `crates/cairn-daemon/src/auth/mod.rs`, replace the `test_ctx` function and update all three existing chain tests:

```rust
fn test_ctx() -> AuthContext {
    AuthContext {
        transport: TransportContext::WebTransport {
            peer_addr: "127.0.0.1:1234".parse().unwrap(),
        },
        token: None,
    }
}
```

The three test bodies (`chain_returns_first_success`, `chain_stops_on_rejection`, `chain_not_applicable_if_all_skip`) stay the same — they call `test_ctx()` and assert chain behaviour.

- [ ] **Step 2: Update `AuthContext` and add `TransportContext`**

In `crates/cairn-daemon/src/auth/mod.rs`:

Replace the `use std::net::SocketAddr;` import section and `AuthContext` struct with:

```rust
use std::net::SocketAddr;

use crate::identity::Identity;

/// Transport-specific connection material, produced by the listener that
/// accepted the connection. Each variant carries exactly the material
/// available on that transport.
#[derive(Debug)]
pub enum TransportContext {
    WebTransport { peer_addr: SocketAddr },
}

/// Information available to auth backends for identity resolution.
/// Created by the transport layer, enriched by the first-message phase.
#[derive(Debug)]
pub struct AuthContext {
    pub transport: TransportContext,
    pub token: Option<String>,
}
```

Remove the old `AuthContext` struct (the one with `peer_addr: SocketAddr` and `token: Option<String>` as flat fields).

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo nextest run -p cairn-daemon -E 'test(~auth::tests)'`

Expected: the 3 chain tests pass. Other tests will fail (tailscale, serve) — that's expected, we fix those in later tasks.

- [ ] **Step 4: Commit**

```
git add crates/cairn-daemon/src/auth/mod.rs
git commit -m "refactor(auth): add TransportContext enum, update AuthContext"
```

---

### Task 2: Update `TailscaleBackend` to match on `TransportContext`

**Files:**
- Modify: `crates/cairn-daemon/src/auth/tailscale.rs`

- [ ] **Step 1: Update `authenticate` to extract `peer_addr` from transport variant**

In `crates/cairn-daemon/src/auth/tailscale.rs`, replace the `AuthBackend` impl:

```rust
impl AuthBackend for TailscaleBackend {
    fn authenticate(
        &self,
        ctx: &AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>>
    {
        match &ctx.transport {
            TransportContext::WebTransport { peer_addr } => {
                let peer_addr = *peer_addr;
                Box::pin(self.do_authenticate(peer_addr))
            }
        }
    }

    fn phase(&self) -> AuthPhase {
        AuthPhase::Transport
    }
}
```

Add `TransportContext` to the use statement at the top of the file:

```rust
use crate::auth::{AuthBackend, AuthContext, AuthError, AuthPhase, TransportContext};
```

Remove the now-unused `use crate::identity::Identity;` import (it's used indirectly through `do_authenticate`'s return type, but check — if `Identity` is referenced directly elsewhere in the file, keep it). The `Identity` type is used in `do_authenticate` return position, so it stays.

- [ ] **Step 2: Run the tailscale unit tests**

Run: `cargo nextest run -p cairn-daemon -E 'test(~tailscale)'`

Expected: PASS — the tailscale unit tests exercise `http_get_unix`, `http_get_tcp`, and `parse_whois_response`, not `AuthBackend::authenticate` directly.

- [ ] **Step 3: Commit**

```
git add crates/cairn-daemon/src/auth/tailscale.rs
git commit -m "refactor(auth): TailscaleBackend matches on TransportContext"
```

---

### Task 3: Decouple `NoneBackend` from CLI (keep for tests)

`NoneBackend` stays as a public module — it's a valid `AuthBackend` impl that
tests use directly. It's just not wired to any `AuthBackendKind` variant, so
users can't select it from the CLI.

**Files:**
- Modify: `crates/cairn-daemon/src/auth/none.rs`

- [ ] **Step 1: Update the module doc comment**

In `crates/cairn-daemon/src/auth/none.rs`, update the module doc:

```rust
//! The `none` auth backend: accepts all connections as anonymous.
//!
//! Not selectable via `--auth`. Used by the test harness to provide an auth
//! chain for WT smoke tests that exercise transport, not authentication.
```

- [ ] **Step 2: Update the test to use `TransportContext`**

In `crates/cairn-daemon/src/auth/none.rs`, update the test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepts_any_connection() {
        let backend = NoneBackend;
        let ctx = AuthContext {
            transport: crate::auth::TransportContext::WebTransport {
                peer_addr: "192.168.1.50:9999".parse().unwrap(),
            },
            token: None,
        };
        let result = backend.authenticate(&ctx).await;
        assert!(matches!(result, Ok(Identity::Anonymous)));
    }
}
```

- [ ] **Step 3: Run auth tests**

Run: `cargo nextest run -p cairn-daemon -E 'test(~auth::)'`

Expected: PASS — all auth tests pass, including the updated `none::tests`.

- [ ] **Step 4: Commit**

```
git add crates/cairn-daemon/src/auth/none.rs
git commit -m "refactor(auth): decouple NoneBackend from CLI, keep for tests"
```

---

### Task 4: Remove `AuthBackendKind::None` from config and CLI

**Files:**
- Modify: `crates/cairn-daemon/src/config/mod.rs`
- Modify: `crates/cairn-daemon/src/config/args.rs`

- [ ] **Step 1: Remove the `None` variant from `AuthBackendKind`**

In `crates/cairn-daemon/src/config/mod.rs`, replace the enum:

```rust
/// Which authentication backend to enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AuthBackendKind {
    /// Authenticate via the Tailscale LocalAPI (whois).
    Tailscale,
}
```

- [ ] **Step 2: Update the `DaemonConfig` default**

In `crates/cairn-daemon/src/config/mod.rs`, change the `auth_backends` default from `vec![AuthBackendKind::None]` to `vec![]`:

```rust
auth_backends: vec![],
```

- [ ] **Step 3: Remove `default_value` from the `--auth` CLI arg**

In `crates/cairn-daemon/src/config/args.rs`, update the `auth` field's `#[arg]` attribute. Remove `default_value = "none"` and update the doc comment:

```rust
    /// Authentication backends for network listeners. Repeat or comma-separate.
    /// Required when a network listener (https://) is configured.
    #[arg(
        long,
        env = "CAIRN_AUTH",
        value_delimiter = ',',
        value_enum
    )]
    pub auth: Vec<AuthBackendKind>,
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`

Expected: compile errors in `daemon.rs` (`build_auth_chain` still references `AuthBackendKind::None`). That's expected — we fix it in the next task.

- [ ] **Step 5: Commit**

```
git add crates/cairn-daemon/src/config/
git commit -m "refactor(config): remove AuthBackendKind::None, no default auth"
```

---

### Task 5: Make `Daemon::new` the validation chokepoint

**Files:**
- Modify: `crates/cairn-daemon/src/daemon.rs`
- Modify: `crates/cairn-daemon/src/config/mod.rs`
- Modify: `crates/cairn-daemon/src/main.rs`

- [ ] **Step 1: Write tests for `Daemon::new` validation**

In `crates/cairn-daemon/src/daemon.rs`, add a `#[cfg(test)]` module at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthBackendKind;
    use crate::listen::ListenerConfig;

    #[test]
    fn new_rejects_network_listener_without_auth() {
        let cfg = DaemonConfig {
            listeners: vec![ListenerConfig::WebTransport(
                "127.0.0.1:9443".parse().unwrap(),
            )],
            ..DaemonConfig::default()
        };
        let err = Daemon::new(cfg).unwrap_err();
        assert!(
            err.to_string().contains("--auth"),
            "expected --auth hint, got: {err}"
        );
    }

    #[test]
    fn new_accepts_uds_without_auth() {
        let cfg = DaemonConfig::default(); // UDS only, empty auth
        assert!(Daemon::new(cfg).is_ok());
    }

    #[test]
    fn new_accepts_network_listener_with_auth() {
        // We can't actually init TailscaleBackend in unit tests (no tailscaled),
        // but Daemon::new only validates config, it doesn't build the chain.
        let cfg = DaemonConfig {
            listeners: vec![ListenerConfig::WebTransport(
                "127.0.0.1:9443".parse().unwrap(),
            )],
            auth_backends: vec![AuthBackendKind::Tailscale],
            ..DaemonConfig::default()
        };
        assert!(Daemon::new(cfg).is_ok());
    }
}
```

- [ ] **Step 2: Make `Daemon::new` fallible with validation and warnings**

In `crates/cairn-daemon/src/daemon.rs`, replace the `new` method:

```rust
pub fn new(cfg: DaemonConfig) -> anyhow::Result<Self> {
    let has_network = cfg.listeners.iter().any(|l| !l.is_unix());
    let has_unix = cfg.listeners.iter().any(|l| l.is_unix());

    // Hard errors.
    if has_network && cfg.auth_backends.is_empty() {
        anyhow::bail!(
            "network listener configured but no --auth backend specified; \
             authentication is required for non-UDS transports"
        );
    }

    // Soft warnings.
    if !has_network && !cfg.auth_backends.is_empty() {
        tracing::warn!("--auth has no effect without a network listener");
    }
    if !has_unix && (cfg.dir_mode != 0o700 || cfg.socket_mode != 0o600) {
        tracing::warn!(
            "--dir-mode / --socket-mode have no effect without a unix:// listener"
        );
    }
    if cfg.listeners.iter().any(|l| l.is_wt()) {
        if cfg.wt_cert.is_none() || cfg.wt_key.is_none() {
            tracing::warn!(
                "https:// (WebTransport) listener configured but \
                 --wt-cert / --wt-key not set"
            );
        }
    }

    Ok(Self {
        registry: Arc::new(SessionRegistry::new()),
        cfg: Arc::new(cfg),
    })
}
```

- [ ] **Step 3: Remove `warn_on_misconfig` from `DaemonConfig`**

In `crates/cairn-daemon/src/config/mod.rs`, delete the entire `impl DaemonConfig` block containing `warn_on_misconfig`.

- [ ] **Step 4: Update `main.rs`**

In `crates/cairn-daemon/src/main.rs`, remove the `cfg.warn_on_misconfig();` line and add `?` to `Daemon::new`:

```rust
let cfg: DaemonConfig = args.into();

let rt = tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()?;
rt.block_on(async {
    let daemon = cairn_daemon::daemon::Daemon::new(cfg)?;
```

- [ ] **Step 5: Run the new validation tests**

Run: `cargo nextest run -p cairn-daemon -E 'test(~daemon::tests)'`

Expected: all 3 tests PASS.

- [ ] **Step 6: Commit**

```
git add crates/cairn-daemon/src/daemon.rs crates/cairn-daemon/src/config/mod.rs crates/cairn-daemon/src/main.rs
git commit -m "refactor(daemon): Daemon::new validates config, absorbs warnings"
```

---

### Task 6: Update `build_auth_chain` and `serve` wiring

`serve()` gains an optional `auth_chain` parameter so tests can inject a
`NoneBackend`-based chain without going through `build_auth_chain` (which
would require tailscaled). Production code passes `None` and the chain is
built from config as before.

**Files:**
- Modify: `crates/cairn-daemon/src/daemon.rs`
- Modify: `crates/cairn-daemon/src/serve.rs`
- Modify: `crates/cairn-daemon/src/main.rs`

- [ ] **Step 1: Update `build_auth_chain` to return `Option<AuthChain>`**

In `crates/cairn-daemon/src/daemon.rs`, replace the `build_auth_chain` method:

```rust
/// Build the auth chain from the configured backend kinds.
///
/// Returns `None` when no network listeners are configured (UDS-only mode).
pub fn build_auth_chain(&self) -> anyhow::Result<Option<crate::auth::AuthChain>> {
    let has_network = self.cfg.listeners.iter().any(|l| !l.is_unix());
    if !has_network {
        return Ok(None);
    }
    let mut backends: Vec<Box<dyn crate::auth::AuthBackend>> = Vec::new();
    for kind in &self.cfg.auth_backends {
        match kind {
            AuthBackendKind::Tailscale => {
                backends.push(Box::new(
                    crate::auth::tailscale::TailscaleBackend::new()
                        .map_err(|e| anyhow::anyhow!("tailscale auth backend init: {e}"))?,
                ));
            }
        }
    }
    Ok(Some(crate::auth::AuthChain::new(backends)))
}
```

Add the `AuthBackendKind` import at the top of the file:

```rust
use crate::config::{AuthBackendKind, DaemonConfig};
```

(Replace the existing `use crate::config::DaemonConfig;` line.)

- [ ] **Step 2: Add `auth_chain` parameter to `serve()`**

In `crates/cairn-daemon/src/serve.rs`, change the signature of `serve()`:

```rust
pub async fn serve(
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
    auth_chain: Option<crate::auth::AuthChain>,
) -> anyhow::Result<()> {
```

Replace the `auth_chain` construction at the top of the function body:

```rust
let auth_chain = match auth_chain {
    Some(chain) => Some(Arc::new(chain)),
    None => daemon.build_auth_chain()?.map(Arc::new),
};
```

Then in the WT listener block (inside `if let Some(addr) = wt_addr`), the auth
chain must exist for WT. Add at the top of that block:

```rust
if let Some(addr) = wt_addr {
    let auth_chain = auth_chain
        .clone()
        .expect("WT listener requires auth chain; validated by Daemon::new");
```

The two `Arc::clone(&auth_chain)` sites inside the WT spawn now reference
the local `auth_chain` binding which is `Arc<AuthChain>` — no changes needed
to those lines.

- [ ] **Step 3: Update `main.rs` to pass `None`**

In `crates/cairn-daemon/src/main.rs`, update the `serve` call:

```rust
cairn_daemon::serve::serve(daemon, shutdown, None).await
```

- [ ] **Step 4: Update `AuthContext` construction in `serve_wt_connection`**

In `crates/cairn-daemon/src/serve.rs`, in the `serve_wt_connection` function,
replace:

```rust
let ctx = crate::auth::AuthContext {
    peer_addr,
    token: None,
};
```

with:

```rust
let ctx = crate::auth::AuthContext {
    transport: crate::auth::TransportContext::WebTransport { peer_addr },
    token: None,
};
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo build`

Expected: compiles. Integration tests will fail until we update the harness in the next task.

- [ ] **Step 6: Commit**

```
git add crates/cairn-daemon/src/daemon.rs crates/cairn-daemon/src/serve.rs crates/cairn-daemon/src/main.rs
git commit -m "refactor(serve): auth_chain parameter, build_auth_chain returns Option, TransportContext in AuthContext"
```

---

### Task 7: Fix test harness and integration tests

The test harness injects a `NoneBackend`-based auth chain via the new
`serve()` parameter. This lets WT smoke tests run without tailscaled —
they exercise the transport, not authentication.

**Files:**
- Modify: `crates/cairn-daemon/tests/common/mod.rs`
- Modify: `crates/cairn-daemon/tests/daemon_streaming.rs`

- [ ] **Step 1: Update `DaemonHarness::start()` for fallible `Daemon::new`**

In `crates/cairn-daemon/tests/common/mod.rs`, in the `start()` method, change:

```rust
let daemon = Daemon::new(cfg);
let shutdown = CancellationToken::new();
let task = tokio::spawn(serve(daemon, shutdown.clone()));
```

to:

```rust
let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
let shutdown = CancellationToken::new();
let task = tokio::spawn(serve(daemon, shutdown.clone(), None));
```

- [ ] **Step 2: Update `DaemonHarness::start_with_wt()` to inject `NoneBackend`**

In `crates/cairn-daemon/tests/common/mod.rs`, in `start_with_wt()`, the config
has a WT listener but no `auth_backends` (empty by default). `Daemon::new`
would reject this, so we need to either add a backend to the config or bypass
validation. Since we want to use `NoneBackend` (not Tailscale), we set
`auth_backends` to contain Tailscale to pass validation, then override the
chain at `serve()` time.

Actually, simpler: `Daemon::new` validates that network listeners have auth
backends. We need to satisfy that. But we don't want to actually *use*
Tailscale. The cleanest approach: add `AuthBackendKind::Tailscale` to satisfy
`Daemon::new` validation, then pass a `NoneBackend` chain to `serve()` which
takes precedence over `build_auth_chain`.

Add the import at the top:

```rust
use cairn_daemon::{
    auth::{self, none::NoneBackend},
    config::{AuthBackendKind, DaemonConfig},
    daemon::Daemon,
    listen::ListenerConfig,
    serve::serve,
};
```

Update the config and serve call in `start_with_wt()`:

```rust
let cfg = DaemonConfig {
    listeners: vec![
        ListenerConfig::Unix(socket_path.clone()),
        ListenerConfig::WebTransport(wt_addr),
    ],
    auth_backends: vec![AuthBackendKind::Tailscale],
    wt_cert: Some(cert_path),
    wt_key: Some(key_path),
    ..DaemonConfig::default()
};
let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
let shutdown = CancellationToken::new();

let test_chain = auth::AuthChain::new(vec![Box::new(NoneBackend)]);
let task = tokio::spawn(serve(daemon, shutdown.clone(), Some(test_chain)));
```

- [ ] **Step 3: Update `test_daemon()` in `daemon_streaming.rs`**

In `crates/cairn-daemon/tests/daemon_streaming.rs`, change:

```rust
fn test_daemon() -> Daemon {
    Daemon::new(DaemonConfig::default())
}
```

to:

```rust
fn test_daemon() -> Daemon {
    Daemon::new(DaemonConfig::default()).expect("test daemon config should be valid")
}
```

- [ ] **Step 4: Run full test suite**

Run: `cargo nextest run -p cairn-daemon`

Expected: all tests PASS. UDS tests pass as before. WT smoke tests pass
using the injected `NoneBackend` chain — no tailscaled required.

- [ ] **Step 5: Commit**

```
git add crates/cairn-daemon/tests/
git commit -m "test: inject NoneBackend chain for WT smoke tests, update harness for fallible Daemon::new"
```

---

### Task 8: Final verification and cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --all-targets -- -D warnings`

Expected: clean.

- [ ] **Step 2: Run fmt check**

Run: `cargo fmt --check`

Expected: clean.

- [ ] **Step 3: Run full test suite**

Run: `cargo nextest run -p cairn-daemon`

Expected: all 82 tests pass (same count — `none::tests` is updated, not deleted; WT smoke tests use the injected `NoneBackend` chain).

- [ ] **Step 4: Verify the binary rejects missing auth for WT**

Run: `cargo run -p cairn-daemon -- --listen https://127.0.0.1:9443 2>&1`

Expected: error message containing `--auth` and `network listener`.

- [ ] **Step 5: Verify the binary accepts UDS-only without auth**

Run: `timeout 1 cargo run -p cairn-daemon 2>&1 || true`

Expected: starts normally (UDS listening), no auth error. The `timeout` kills it after 1 second.

- [ ] **Step 6: Commit any fixups, then run full build one more time**

```
cargo build && cargo clippy --all-targets -- -D warnings && cargo nextest run -p cairn-daemon
```

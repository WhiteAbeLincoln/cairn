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

### Task 3: Delete `NoneBackend`

**Files:**
- Delete: `crates/cairn-daemon/src/auth/none.rs`
- Modify: `crates/cairn-daemon/src/auth/mod.rs`

- [ ] **Step 1: Remove the `none` module declaration**

In `crates/cairn-daemon/src/auth/mod.rs`, remove the line:

```rust
pub mod none;
```

- [ ] **Step 2: Delete the file**

```
rm crates/cairn-daemon/src/auth/none.rs
```

- [ ] **Step 3: Verify auth tests still pass**

Run: `cargo nextest run -p cairn-daemon -E 'test(~auth::)'`

Expected: PASS — the chain tests use inline `AlwaysAnon`/`AlwaysReject`/`SkipBackend` structs, not `NoneBackend`. The `none::tests` module is gone.

- [ ] **Step 4: Commit**

```
git add -A crates/cairn-daemon/src/auth/
git commit -m "refactor(auth): delete NoneBackend"
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

**Files:**
- Modify: `crates/cairn-daemon/src/daemon.rs`
- Modify: `crates/cairn-daemon/src/serve.rs`

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

- [ ] **Step 2: Update `serve()` to handle `Option<AuthChain>`**

In `crates/cairn-daemon/src/serve.rs`, replace the `auth_chain` line at the top of `serve()`:

```rust
let auth_chain = daemon.build_auth_chain()?.map(Arc::new);
```

Then in the WT listener block (inside `if let Some(addr) = wt_addr`), the auth chain must exist for WT. Replace the `Arc::clone(&auth_chain)` usage at the two sites inside the WT block. Wrap the WT block in an auth chain check:

```rust
if let Some(addr) = wt_addr {
    let auth_chain = auth_chain
        .clone()
        .expect("WT listener requires auth chain; validated by Daemon::new");
```

Then change the two `Arc::clone(&auth_chain)` inside the WT spawn to just `Arc::clone(&auth_chain)` — they reference the local `auth_chain` binding which is now `Arc<AuthChain>`.

- [ ] **Step 3: Update `AuthContext` construction in `serve_wt_connection`**

In `crates/cairn-daemon/src/serve.rs`, in the `serve_wt_connection` function, replace:

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

- [ ] **Step 4: Build and run full test suite**

Run: `cargo build && cargo nextest run -p cairn-daemon`

Expected: all tests PASS. The UDS integration tests (`daemon_unary`, `daemon_streaming`, `daemon_meta`) never touch the auth chain. The WT integration tests (`wt_smoke`) use `DaemonConfig::default()` which has `auth_backends: vec![]`, but `start_with_wt()` in the test harness builds a config with a WT listener — this will fail because auth_backends is empty. We fix that in the next task.

- [ ] **Step 5: Commit**

```
git add crates/cairn-daemon/src/daemon.rs crates/cairn-daemon/src/serve.rs
git commit -m "refactor(serve): build_auth_chain returns Option, AuthContext uses TransportContext"
```

---

### Task 7: Fix test harness and integration tests

**Files:**
- Modify: `crates/cairn-daemon/tests/common/mod.rs`
- Modify: `crates/cairn-daemon/tests/daemon_streaming.rs`

- [ ] **Step 1: Update `DaemonHarness::start()` for fallible `Daemon::new`**

In `crates/cairn-daemon/tests/common/mod.rs`, in the `start()` method, change:

```rust
let daemon = Daemon::new(cfg);
```

to:

```rust
let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
```

- [ ] **Step 2: Update `DaemonHarness::start_with_wt()` for fallible `Daemon::new` and auth**

In `crates/cairn-daemon/tests/common/mod.rs`, in `start_with_wt()`, the config now needs an auth backend since it has a WT listener. The WT smoke tests use `NoneBackend` implicitly via the default — but `NoneBackend` is gone. Since these tests run over localhost and Tailscale isn't available in CI, we need a test-only approach.

The WT smoke tests currently authenticate via the `NoneBackend` default. With it gone, the WT tests need a backend that will accept the connection. The `auth_chain` in `serve()` must return *some* identity. Add `AuthBackendKind::Tailscale` to the config — but Tailscale won't be running in CI so the WT smoke tests will need to be gated. However, looking at the current WT tests, they connect but the auth chain with `NoneBackend` always succeeds. We need to replace this.

The simplest fix: the test harness constructs a `Daemon` whose `auth_chain` will accept localhost connections. Since we can't use Tailscale in tests, and `NoneBackend` is gone, we need a test-only auth backend.

Actually, re-reading `serve()` more carefully: `build_auth_chain` is called in `serve()`, not `Daemon::new`. And `Daemon` has a public `registry` and `cfg`. We can create the `Daemon` with the auth_backends list empty (for UDS-only tests) or non-empty (for WT tests). But for WT tests, `build_auth_chain` will try to construct `TailscaleBackend::new()` which probes for `tailscaled` and will fail in CI.

The right fix: make `build_auth_chain` a method that can be overridden in tests, or inject the auth chain from outside. But that's a larger refactor. The simpler approach for now: make the WT smoke tests conditional on Tailscale availability (they already need a real tailscaled to authenticate).

Update `start_with_wt()`:

```rust
let cfg = DaemonConfig {
    listeners: vec![
        ListenerConfig::Unix(socket_path.clone()),
        ListenerConfig::WebTransport(wt_addr),
    ],
    auth_backends: vec![cairn_daemon::config::AuthBackendKind::Tailscale],
    wt_cert: Some(cert_path),
    wt_key: Some(key_path),
    ..DaemonConfig::default()
};
let daemon = Daemon::new(cfg).expect("test daemon config should be valid");
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

- [ ] **Step 4: Run UDS integration tests**

Run: `cargo nextest run -p cairn-daemon -E 'test(~daemon_unary) | test(~daemon_streaming) | test(~daemon_meta) | test(~smoke::binary)'`

Expected: all PASS — these are UDS-only and don't touch auth.

- [ ] **Step 5: Run WT smoke tests**

Run: `cargo nextest run -p cairn-daemon -E 'test(~wt_smoke)'`

Expected: if tailscaled is running locally, PASS. If not, the tests will fail at `TailscaleBackend::new()` in `build_auth_chain`. This is the expected new behaviour — WT tests require a real auth backend. If they fail, that's correct for CI without tailscaled.

- [ ] **Step 6: Commit**

```
git add crates/cairn-daemon/tests/
git commit -m "test: update harness for fallible Daemon::new and auth redesign"
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

Expected: all UDS tests pass (82 minus the deleted `none::tests::accepts_any_connection` = 81, minus any WT tests that now require tailscaled). Check the count and note any expected WT failures.

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

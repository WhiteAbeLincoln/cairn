# Serve Transport Refactor

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate three regressions introduced by the current `serve/` refactor (mpsc channel, `SingleUnixAccept` mutex, `AcceptedConnection` enum dispatch) while keeping the good structural extractions (auth.rs, unix bind/cleanup, ListenerId).

**Architecture:** Extract a generic `run_wrpc_server` function that encapsulates the wRPC accept-loop + invocation-pump pattern. Each transport owns its full lifecycle — UDS uses `PeerCredListener` (which already implements `wrpc_transport::frame::Accept`) directly with one shared wRPC server per listener; WT runs its own `endpoint.accept()` loop and spawns a per-connection `run_wrpc_server`. `serve()` binds all transports, spawns their tasks into a `JoinSet`, and handles shutdown/drain/cleanup. No mpsc channel, no enum dispatch, no Mutex.

**Tech Stack:** Rust, tokio, wrpc-transport, wtransport, wrpc-transport-web

---

## File map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/cairn-daemon/src/serve/wrpc.rs` | **Rewrite** | Generic `run_wrpc_server<A, I, O, H>` — the only shared wRPC code. Keeps `AuthenticatedWtAccept`. Deletes `serve_connection`, `SingleUnixAccept`, `AcceptMode`. |
| `crates/cairn-daemon/src/serve/transport/unix.rs` | **Modify** | Add `pub(super) async fn run(...)` that binds the `PeerCredListener` and calls `run_wrpc_server`. Remove `AcceptedUnixConnection` and `BoundUnixListener::accept()` (no longer used — the `Accept` impl on `PeerCredListener` handles this). Keep `BoundUnixListener` for bind/cleanup state. |
| `crates/cairn-daemon/src/serve/transport/webtransport.rs` | **Modify** | Add `pub(super) async fn run(...)` that loops on `endpoint.accept()`, authenticates, and spawns per-connection `run_wrpc_server`. Remove `AcceptedWebTransportConnection` and `BoundWebTransportListener::accept()`. Keep `BoundWebTransportListener` for bind state. |
| `crates/cairn-daemon/src/serve/transport/mod.rs` | **Rewrite** | Delete `BoundListener` enum, `AcceptedConnection` enum, `bind_all()`. Replace with a thin `bind_and_spawn()` that matches on `ListenerConfig`, binds, and spawns the transport's `run()` into the caller's `JoinSet`. |
| `crates/cairn-daemon/src/serve/mod.rs` | **Rewrite** | `serve()` becomes: build auth → bind+spawn transports via `transport::bind_and_spawn()` → `shutdown.cancelled().await` → drain → join/abort tasks → cleanup. Delete `ListenerEvent`, `run_listener`, `handle_connection`, `handle_accept_error`, `handle_connection_result`, `shutdown_listener_tasks`, `shutdown_connection_tasks`. |
| `crates/cairn-daemon/src/serve/auth.rs` | **Modify (minor)** | Delete `PeerMaterial` enum and `log_auth_failure`. Auth for UDS is done inside `PeerCredListener::accept()` (peer creds, no chain needed). Auth for WT is done inside the WT `run()` function directly using `Authenticator::authenticate_network()`. Make `authenticate_network` pub(super). |

## What gets deleted (and why)

| Symbol | File | Why |
|--------|------|-----|
| `ListenerEvent` | mod.rs | mpsc channel gone; transports handle their own accept loops |
| `run_listener` | mod.rs | replaced by per-transport `run()` functions |
| `handle_connection` | mod.rs | auth + dispatch logic moves into each transport's `run()` |
| `handle_accept_error` | mod.rs | each transport logs its own errors |
| `handle_connection_result` | mod.rs | `JoinSet` drain handles this inline |
| `shutdown_listener_tasks` | mod.rs | folded into `serve()` shutdown block |
| `shutdown_connection_tasks` | mod.rs | only WT has connection tasks; it manages its own `JoinSet` |
| `serve_connection` | wrpc.rs | enum dispatch replaced by direct generic calls |
| `SingleUnixAccept` | wrpc.rs | UDS uses `PeerCredListener` which already impls `Accept` |
| `AcceptMode` | wrpc.rs | UDS and WT both naturally loop; no mode flag needed |
| `BoundListener` enum | transport/mod.rs | transports own their lifecycle; no shared enum |
| `AcceptedConnection` enum | transport/mod.rs | same |
| `bind_all()` | transport/mod.rs | replaced by `bind_and_spawn()` |
| `AcceptedUnixConnection` | unix.rs | `PeerCredListener` handles accept+split internally |
| `BoundUnixListener::accept()` | unix.rs | same |
| `AcceptedWebTransportConnection` | webtransport.rs | WT `run()` handles accept inline |
| `BoundWebTransportListener::accept()` | webtransport.rs | same |
| `PeerMaterial` | auth.rs | UDS auth is inside `PeerCredListener`; WT calls `authenticate_network` directly |
| `log_auth_failure` | auth.rs | each transport logs with its own context |

---

### Task 1: Extract `run_wrpc_server` and delete regressions from `wrpc.rs`

This is the foundation — the single generic function both transports will call.

**Files:**
- Rewrite: `crates/cairn-daemon/src/serve/wrpc.rs`

- [ ] **Step 1: Rewrite wrpc.rs**

Replace the entire file. Keep `AuthenticatedWtAccept` (WT needs it). The new `run_wrpc_server` is the existing `run_server` but without `AcceptMode` — both transports loop naturally.

```rust
use anyhow::Context as _;
use futures::stream::{StreamExt as _, select_all};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;
use wrpc_transport::frame::Accept;

use super::ConnCtx;

pub(super) async fn run_wrpc_server<A, I, O, H>(
    acceptor: A,
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
) -> anyhow::Result<()>
where
    for<'a> &'a A: Accept<Context = ConnCtx, Incoming = I, Outgoing = O>,
    A: Send + Sync + 'static,
    I: AsyncRead + Send + Sync + Unpin + 'static,
    O: AsyncWrite + Send + Sync + Unpin + 'static,
    H: wrpc_transport::frame::ConnHandler<I, O> + Send + Sync + 'static,
{
    let server: wrpc_transport::Server<ConnCtx, I, O, H> = wrpc_transport::Server::new();
    let invocations = cairn_protocol::serve(&server, daemon).await?;
    let mut invocations = select_all(
        invocations
            .into_iter()
            .map(|(instance, name, stream)| stream.map(move |res| (instance, name, res))),
    );

    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,

            result = server.accept(&acceptor) => {
                result.context("wRPC accept failed")?;
            }

            item = invocations.next() => {
                match item {
                    Some((_instance, _name, Ok(fut))) => {
                        tokio::spawn(fut);
                    }
                    Some((instance, name, Err(error))) => {
                        return Err(error).with_context(|| {
                            format!("wRPC invocation failed for {instance}#{name}")
                        });
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

pub(super) struct AuthenticatedWtAccept {
    pub(super) inner: wrpc_transport_web::Client,
    pub(super) ctx: ConnCtx,
}

impl Accept for &AuthenticatedWtAccept {
    type Context = ConnCtx;
    type Outgoing = wtransport::SendStream;
    type Incoming = wtransport::RecvStream;

    async fn accept(&self) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let ((), tx, rx) = Accept::accept(&self.inner).await?;
        Ok((self.ctx.clone(), tx, rx))
    }
}
```

- [ ] **Step 2: Verify it compiles in isolation**

This file has no callers yet (the old callers are being rewritten in subsequent tasks), so it won't compile with the full crate. Verify syntax only:

```
cargo check -p cairn-daemon 2>&1 | head -40
```

Expect errors about unused imports and dead code — that's fine at this stage. Confirm no errors *within* `wrpc.rs` itself (syntax, trait bound issues). If there are trait bound errors, fix them before proceeding.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-daemon/src/serve/wrpc.rs
git commit -m "refactor(serve): extract generic run_wrpc_server, delete SingleUnixAccept and AcceptMode

The old wrpc.rs had serve_connection (enum dispatch), SingleUnixAccept
(Mutex<Option<UnixStream>>), and AcceptMode to paper over the fact that
UDS and WT have different connection models. Replace with a single
generic run_wrpc_server that both transports call directly with their
own Accept impls. No mode flag needed — both naturally loop."
```

---

### Task 2: Give `unix.rs` its own `run()` that uses `PeerCredListener` directly

**Files:**
- Modify: `crates/cairn-daemon/src/serve/transport/unix.rs`

- [ ] **Step 1: Add `run()`, remove `AcceptedUnixConnection` and `BoundUnixListener::accept()`**

The `PeerCredListener` already implements `wrpc_transport::frame::Accept` — it accepts from the `UnixListener`, extracts peer creds into `ConnCtx`, and splits the stream. This is the correct UDS model: one shared wRPC server per listener.

Remove `AcceptedUnixConnection` (no longer produced). Remove `BoundUnixListener::accept()` (the `Accept` impl on `PeerCredListener` replaces it). Add a `run()` method on `BoundUnixListener` that wraps the listener in `PeerCredListener` and calls `run_wrpc_server`.

The file should look like:

```rust
use std::path::{Path, PathBuf};

use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio_util::sync::CancellationToken;
use wrpc_transport::frame::Accept;

use crate::serve::ConnCtx;
use crate::serve::wrpc::run_wrpc_server;

use super::super::ListenerId;

/// A `UnixListener` whose `accept` captures `SO_PEERCRED` into `ConnCtx`
/// before splitting the stream.
pub struct PeerCredListener(pub tokio::net::UnixListener);

impl Accept for &PeerCredListener {
    type Context = ConnCtx;
    type Outgoing = OwnedWriteHalf;
    type Incoming = OwnedReadHalf;

    async fn accept(&self) -> std::io::Result<(Self::Context, Self::Outgoing, Self::Incoming)> {
        let (stream, _addr) = self.0.accept().await?;
        let identity = match stream.peer_cred().ok().map(|c| c.uid()) {
            Some(uid) => crate::identity::Identity::Unix {
                uid,
                username: None,
            },
            None => crate::identity::Identity::Anonymous,
        };
        let (rx, tx) = stream.into_split();
        Ok((ConnCtx { identity }, tx, rx))
    }
}

pub(in crate::serve) struct BoundUnixListener {
    pub(super) id: ListenerId,
    path: PathBuf,
    listener: tokio::net::UnixListener,
}

pub(super) fn bind(
    id: ListenerId,
    path: PathBuf,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<BoundUnixListener> {
    let listener = bind_with_cleanup(&path, cfg)?;
    tracing::info!(listener = %id, socket = %path.display(), "UDS listening");
    Ok(BoundUnixListener { id, path, listener })
}

impl BoundUnixListener {
    pub(super) async fn run(
        self,
        daemon: crate::daemon::Daemon,
        shutdown: CancellationToken,
    ) -> anyhow::Result<()> {
        let acceptor = PeerCredListener(self.listener);
        run_wrpc_server::<_, OwnedReadHalf, OwnedWriteHalf, ()>(
            acceptor,
            daemon,
            shutdown,
        )
        .await
    }

    pub(super) fn cleanup(&self) {
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    listener = %self.id,
                    socket = %self.path.display(),
                    error = %error,
                    "failed to remove UDS socket"
                );
            }
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

// ── Socket lifecycle ──────────────────────────────────────────────────────

/// Create (or recover) the socket file with correct permissions.
///
/// - Creates the parent directory if needed, chmoding it to `dir_mode` only if
///   we created it (so we don't stomp an admin-managed dir).
/// - Probes a pre-existing socket: live -> bail; connection-refused -> unlink.
/// - Binds and chmods the socket to `socket_mode`.
fn bind_with_cleanup(
    path: &Path,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{FileTypeExt as _, PermissionsExt as _};

    if let Some(parent) = path.parent() {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        if created {
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(cfg.dir_mode))?;
        }
    }

    if path.exists() {
        let file_type = std::fs::symlink_metadata(path)?.file_type();
        if !file_type.is_socket() {
            anyhow::bail!(
                "refusing to remove non-socket path while binding daemon socket: {}",
                path.display()
            );
        }

        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => anyhow::bail!("a daemon is already listening on {}", path.display()),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(path)?;
            }
            Err(error) => {
                anyhow::bail!(
                    "failed to probe existing daemon socket {}: {error}",
                    path.display()
                );
            }
        }
    }

    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(cfg.socket_mode))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                assert_eq!(*uid, nix::unistd::geteuid().as_raw());
                // Username is resolved lazily in whoami, not at accept time.
                assert!(username.is_none());
            }
            other => panic!("expected Unix identity, got {other:?}"),
        }
        connect.await.unwrap();
    }
}
```

Note: `run()` takes `self` by value (not `&self`) because it consumes the `UnixListener` to wrap it in `PeerCredListener`. This is correct — once a listener is running, you don't need the `BoundUnixListener` struct anymore. The cleanup path uses the `path` stored before spawning. The `path()` accessor is added so `serve()` can track paths for cleanup after shutdown.

- [ ] **Step 2: Commit**

```bash
git add crates/cairn-daemon/src/serve/transport/unix.rs
git commit -m "refactor(serve/unix): add run() using PeerCredListener directly

BoundUnixListener::run() wraps the listener in PeerCredListener and
calls run_wrpc_server — no SingleUnixAccept, no Mutex, no per-connection
overhead. PeerCredListener already implements Accept correctly for UDS."
```

---

### Task 3: Give `webtransport.rs` its own `run()` with per-connection wRPC servers

**Files:**
- Modify: `crates/cairn-daemon/src/serve/transport/webtransport.rs`

- [ ] **Step 1: Add `run()`, remove `AcceptedWebTransportConnection` and `BoundWebTransportListener::accept()`**

The WT transport needs per-connection wRPC servers (a WT connection multiplexes bidirectional streams; `wrpc_transport_web::Client` adapts this into the `Accept` trait). The `run()` method loops on `endpoint.accept()`, authenticates each connection, then spawns a per-connection `run_wrpc_server` task.

Auth uses `Authenticator::authenticate_network()` directly — no `PeerMaterial` indirection.

The file should look like:

```rust
use std::net::SocketAddr;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::serve::ConnCtx;
use crate::serve::auth::{AuthFailure, Authenticator};
use crate::serve::wrpc::{AuthenticatedWtAccept, run_wrpc_server};

use super::super::ListenerId;

pub(in crate::serve) struct BoundWebTransportListener {
    pub(super) id: ListenerId,
    endpoint: wtransport::Endpoint<wtransport::endpoint::endpoint_side::Server>,
    connect_timeout: std::time::Duration,
}

pub(super) async fn bind(
    id: ListenerId,
    addr: SocketAddr,
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<BoundWebTransportListener> {
    let (tls, cert_path, key_path) = super::super::resolve_tls(cfg)?;
    let rt_dir = crate::config::runtime_dir();
    std::fs::create_dir_all(&rt_dir)?;
    tls.export_hash(&rt_dir.join("cert-hash"))?;

    let identity = wtransport::Identity::load_pemfiles(&cert_path, &key_path)
        .await
        .map_err(|e| anyhow::anyhow!("loading TLS identity: {e}"))?;

    let config = wtransport::ServerConfig::builder()
        .with_bind_address(addr)
        .with_identity(identity)
        .keep_alive_interval(Some(std::time::Duration::from_secs(15)))
        .max_idle_timeout(Some(cfg.wt_idle_timeout))
        .map_err(|e| anyhow::anyhow!("invalid idle timeout: {e}"))?
        .build();

    let endpoint = wtransport::Endpoint::server(config)?;
    let bound_addr = endpoint.local_addr()?;
    tracing::info!(
        listener = %id,
        %bound_addr,
        hash = %tls.spki_hash_hex(),
        "WT listening"
    );

    Ok(BoundWebTransportListener {
        id,
        endpoint,
        connect_timeout: cfg.wt_connect_timeout,
    })
}

impl BoundWebTransportListener {
    pub(super) async fn run(
        self,
        daemon: crate::daemon::Daemon,
        auth: Authenticator,
        shutdown: CancellationToken,
    ) -> anyhow::Result<()> {
        let mut connections = JoinSet::new();

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,

                incoming = self.endpoint.accept() => {
                    let conn = match self.accept_connection(incoming).await {
                        Ok(conn) => conn,
                        Err(error) => {
                            tracing::debug!(
                                listener = %self.id,
                                error = %error,
                                "WT connection accept failed"
                            );
                            continue;
                        }
                    };

                    let peer_addr = conn.remote_address();
                    let identity = match auth.authenticate_network(peer_addr).await {
                        Ok(id) => id,
                        Err(error) => {
                            tracing::warn!(
                                listener = %self.id,
                                %peer_addr,
                                %error,
                                "WT connection rejected"
                            );
                            continue;
                        }
                    };

                    tracing::info!(%peer_addr, ?identity, "WT connection authenticated");
                    let ctx = ConnCtx { identity };
                    let acceptor = AuthenticatedWtAccept {
                        inner: wrpc_transport_web::Client::from(conn),
                        ctx,
                    };

                    let daemon = daemon.clone();
                    let shutdown = shutdown.clone();
                    connections.spawn(async move {
                        if let Err(error) = run_wrpc_server::<
                            _,
                            wtransport::RecvStream,
                            wtransport::SendStream,
                            wrpc_transport_web::ConnHandler,
                        >(acceptor, daemon, shutdown).await {
                            tracing::debug!(%peer_addr, error = %error, "WT connection ended with error");
                        }
                    });
                }

                result = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = result {
                        if !error.is_cancelled() {
                            tracing::error!(error = %error, "WT connection task failed");
                        }
                    }
                }
            }
        }

        connections.abort_all();
        while connections.join_next().await.is_some() {}
        Ok(())
    }

    async fn accept_connection(
        &self,
        incoming: wtransport::endpoint::IncomingSession,
    ) -> anyhow::Result<wtransport::Connection> {
        let request = tokio::time::timeout(self.connect_timeout, incoming)
            .await
            .map_err(|_| anyhow::anyhow!("WT session request timed out"))?
            .map_err(|e| anyhow::anyhow!("WT session request error: {e}"))?;

        let conn = tokio::time::timeout(self.connect_timeout, request.accept())
            .await
            .map_err(|_| anyhow::anyhow!("WT connection accept timed out"))?
            .map_err(|e| anyhow::anyhow!("WT connection accept error: {e}"))?;

        Ok(conn)
    }
}
```

Note: `run()` takes `self` by value — it owns the endpoint for its lifetime. The `JoinSet` for connection tasks is local to `run()`, not shared with `serve()`.

The `AuthFailure` import may need adjusting depending on what Task 5 does to auth.rs — if `authenticate_network` returns `AuthFailure`, the import is needed; if it returns a different error type, adjust. See Task 5.

- [ ] **Step 2: Commit**

```bash
git add crates/cairn-daemon/src/serve/transport/webtransport.rs
git commit -m "refactor(serve/wt): add run() with per-connection wRPC servers

BoundWebTransportListener::run() owns the endpoint accept loop and
spawns per-connection run_wrpc_server tasks into a local JoinSet.
Auth calls Authenticator::authenticate_network() directly — no
PeerMaterial indirection."
```

---

### Task 4: Simplify `transport/mod.rs` — delete enums, add `bind_and_spawn()`

**Files:**
- Rewrite: `crates/cairn-daemon/src/serve/transport/mod.rs`

- [ ] **Step 1: Rewrite transport/mod.rs**

Delete `BoundListener`, `AcceptedConnection`, and `bind_all()`. Replace with `bind_and_spawn()` that matches on `ListenerConfig`, binds the transport, and spawns its `run()` into the caller's `JoinSet`. Returns a list of cleanup closures (just the unix socket paths that need removal on shutdown).

```rust
use std::path::PathBuf;

use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::ListenerId;
use super::auth::Authenticator;

pub(super) mod unix;
pub(super) mod webtransport;

pub(super) struct SpawnedTransports {
    pub(super) unix_paths: Vec<PathBuf>,
}

pub(super) async fn bind_and_spawn(
    cfg: &crate::config::DaemonConfig,
    daemon: &crate::daemon::Daemon,
    auth: &Authenticator,
    shutdown: &CancellationToken,
    tasks: &mut JoinSet<anyhow::Result<()>>,
) -> anyhow::Result<SpawnedTransports> {
    let mut unix_paths = Vec::new();

    for (index, listener_cfg) in cfg.listeners.iter().enumerate() {
        let id = ListenerId::new(index, listener_cfg);

        match listener_cfg {
            crate::listen::ListenerConfig::Unix(path) => {
                let bound = unix::bind(id, path.clone(), cfg)?;
                unix_paths.push(bound.path().to_path_buf());
                let daemon = daemon.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(async move { bound.run(daemon, shutdown).await });
            }
            crate::listen::ListenerConfig::WebTransport(addr) => {
                let bound = webtransport::bind(id, *addr, cfg).await?;
                let daemon = daemon.clone();
                let auth = auth.clone();
                let shutdown = shutdown.clone();
                tasks.spawn(async move { bound.run(daemon, auth, shutdown).await });
            }
        }
    }

    Ok(SpawnedTransports { unix_paths })
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/cairn-daemon/src/serve/transport/mod.rs
git commit -m "refactor(serve/transport): replace enum dispatch with bind_and_spawn()

Each transport binds and spawns its own run() task — no BoundListener
enum, no AcceptedConnection enum, no shared accept path."
```

---

### Task 5: Simplify `auth.rs` — remove `PeerMaterial`, expose `authenticate_network`

**Files:**
- Modify: `crates/cairn-daemon/src/serve/auth.rs`

- [ ] **Step 1: Rewrite auth.rs**

With the new architecture:
- UDS auth is handled entirely inside `PeerCredListener::accept()` (peer creds → `Identity`). The `Authenticator` is not involved.
- WT auth calls `Authenticator::authenticate_network()` directly with the peer address.

So `PeerMaterial`, the top-level `authenticate()` method that matched on it, and `log_auth_failure()` are all dead code. Remove them. Make `authenticate_network` `pub(in crate::serve)`.

```rust
use std::fmt;
use std::sync::Arc;

use super::ListenerId;

#[derive(Clone)]
pub(super) struct Authenticator {
    chain: Option<Arc<crate::auth::AuthChain>>,
    auth_timeout: std::time::Duration,
}

impl Authenticator {
    pub(super) fn new(
        daemon: &crate::daemon::Daemon,
        auth_chain: Option<crate::auth::AuthChain>,
    ) -> anyhow::Result<Self> {
        let chain = match auth_chain {
            Some(chain) => Some(Arc::new(chain)),
            None => daemon.build_auth_chain()?.map(Arc::new),
        };

        Ok(Self {
            chain,
            auth_timeout: daemon.cfg.auth_timeout,
        })
    }

    pub(in crate::serve) async fn authenticate_network(
        &self,
        peer_addr: std::net::SocketAddr,
    ) -> Result<crate::identity::Identity, AuthFailure> {
        let chain = self.chain.as_ref().ok_or(AuthFailure::NoBackend)?;
        let ctx = crate::auth::AuthContext {
            transport: crate::auth::TransportContext::WebTransport { peer_addr },
            token: None,
        };

        let result = tokio::time::timeout(self.auth_timeout, chain.try_transport(&ctx))
            .await
            .map_err(|_| AuthFailure::TimedOut)?;

        match result {
            Ok(identity) => Ok(identity),
            Err(crate::auth::AuthError::NotApplicable) => Err(AuthFailure::NoBackend),
            Err(crate::auth::AuthError::Rejected(reason)) => Err(AuthFailure::Rejected(reason)),
        }
    }
}

#[derive(Debug)]
pub(in crate::serve) enum AuthFailure {
    NoBackend,
    Rejected(String),
    TimedOut,
}

impl fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoBackend => write!(f, "no auth backend accepted the connection"),
            Self::Rejected(reason) => write!(f, "connection rejected: {reason}"),
            Self::TimedOut => write!(f, "authentication timed out"),
        }
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add crates/cairn-daemon/src/serve/auth.rs
git commit -m "refactor(serve/auth): remove PeerMaterial, expose authenticate_network

UDS auth lives in PeerCredListener::accept(). WT auth calls
authenticate_network() directly. The PeerMaterial enum that unified
them added indirection without value."
```

---

### Task 6: Rewrite `serve()` in `mod.rs`

**Files:**
- Rewrite: `crates/cairn-daemon/src/serve/mod.rs`

- [ ] **Step 1: Rewrite mod.rs**

`serve()` becomes straightforward: build auth → bind+spawn all transports → wait for shutdown → drain sessions → join tasks → clean up sockets. No mpsc, no event loop, no connection dispatch.

```rust
//! Transport listener orchestration and shared wRPC serving.
//!
//! Each transport owns its full lifecycle: bind, accept, authenticate,
//! and run a wRPC server. `serve()` spawns transport tasks, waits for
//! shutdown, drains sessions, and cleans up.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub(crate) mod auth;
mod transport;
pub(crate) mod wrpc;

/// Per-connection context handed to every `Handler` method.
#[derive(Clone, Debug)]
pub struct ConnCtx {
    pub identity: crate::identity::Identity,
}

/// Bind the configured daemon listeners, accept raw transport connections, and
/// serve the cairn wRPC protocol until `shutdown` is cancelled.
pub async fn serve(
    daemon: crate::daemon::Daemon,
    shutdown: CancellationToken,
    auth_chain: Option<crate::auth::AuthChain>,
) -> anyhow::Result<()> {
    if daemon.cfg.listeners.is_empty() {
        anyhow::bail!("no listeners configured");
    }

    let auth = auth::Authenticator::new(&daemon, auth_chain)?;
    let mut tasks = JoinSet::new();

    let spawned = transport::bind_and_spawn(
        &daemon.cfg,
        &daemon,
        &auth,
        &shutdown,
        &mut tasks,
    )
    .await?;

    // Wait for shutdown or a transport task to fail.
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => break,
            result = tasks.join_next(), if !tasks.is_empty() => {
                match result {
                    Some(Ok(Ok(()))) => {
                        if !shutdown.is_cancelled() {
                            anyhow::bail!("transport task exited unexpectedly");
                        }
                    }
                    Some(Ok(Err(error))) => return Err(error).context("transport task failed"),
                    Some(Err(error)) if error.is_cancelled() => {}
                    Some(Err(error)) => return Err(error).context("transport task panicked"),
                    None => break,
                }
            }
        }
    }

    // Graceful shutdown: drain sessions, then abort remaining tasks.
    drain_sessions(&daemon, daemon.cfg.shutdown_grace).await;
    tasks.shutdown().await;

    for path in &spawned.unix_paths {
        let _ = std::fs::remove_file(path);
    }

    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct ListenerId {
    index: usize,
    label: String,
}

impl ListenerId {
    fn new(index: usize, listener: &crate::listen::ListenerConfig) -> Self {
        let label = match listener {
            crate::listen::ListenerConfig::Unix(path) => {
                format!("unix://{}", path.display())
            }
            crate::listen::ListenerConfig::WebTransport(addr) => {
                format!("https://{addr}")
            }
        };

        Self { index, label }
    }
}

impl fmt::Display for ListenerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} #{}", self.label, self.index)
    }
}

// ── Graceful drain ────────────────────────────────────────────────────────

/// Signal all live sessions with SIGTERM and wait up to `grace` for each to
/// exit. Sessions that survive (e.g. they ignore SIGTERM) are dropped; the
/// individual `kill` handler arms a SIGKILL escalation separately if requested
/// — this path is the daemon-shutdown backstop.
async fn drain_sessions(daemon: &crate::daemon::Daemon, grace: std::time::Duration) {
    let entries = daemon.registry.list();
    // Send SIGTERM to all sessions (ignore errors — session may already be gone).
    for e in &entries {
        let _ = e
            .handle()
            .signal(
                nix::sys::signal::Signal::SIGTERM,
                Some("daemon shutting down".into()),
            )
            .await;
    }
    // Wait for each with a shared timeout budget.
    let waits = entries.iter().map(|e| {
        let h = e.handle();
        async move {
            let _ = tokio::time::timeout(grace, h.wait()).await;
        }
    });
    futures::future::join_all(waits).await;
    // Dropping the registry's Arcs (on daemon teardown) is the final SIGKILL backstop.
}

// ── TLS resolution ───────────────────────────────────────────────────────

/// Resolve the TLS configuration for the WebTransport listener.
///
/// Returns the `TlsConfig` and the filesystem paths to cert and key PEM files.
/// If the user provided `--wt-cert` and `--wt-key`, those paths are used.
/// Otherwise a self-signed certificate is generated (or reused) under
/// `crate::config::runtime_dir()/tls/`.
fn resolve_tls(
    cfg: &crate::config::DaemonConfig,
) -> anyhow::Result<(crate::tls::TlsConfig, PathBuf, PathBuf)> {
    match (&cfg.wt_cert, &cfg.wt_key) {
        (Some(cert), Some(key)) => {
            let tls = crate::tls::TlsConfig::from_pem_files(cert, key)?;
            Ok((tls, cert.clone(), key.clone()))
        }
        (None, None) => {
            let tls_dir = crate::config::runtime_dir().join("tls");
            let tls = crate::tls::TlsConfig::self_signed(&tls_dir)?;
            Ok((tls, tls_dir.join("cert.pem"), tls_dir.join("key.pem")))
        }
        _ => anyhow::bail!("--wt-cert and --wt-key must both be provided, or both omitted"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[tokio::test]
    async fn serve_binds_multiple_unix_listeners() {
        let dir = tempfile::tempdir().unwrap();
        let path_a = dir.path().join("a.sock");
        let path_b = dir.path().join("b.sock");
        let cfg = crate::config::DaemonConfig {
            listeners: vec![
                crate::listen::ListenerConfig::Unix(path_a.clone()),
                crate::listen::ListenerConfig::Unix(path_b.clone()),
            ],
            ..crate::config::DaemonConfig::default()
        };
        let daemon = crate::daemon::Daemon::new(cfg).unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(daemon, shutdown.clone(), None));

        wait_for_path(&path_a).await;
        wait_for_path(&path_b).await;

        for path in [&path_a, &path_b] {
            let client = wrpc_transport::unix::Client::from(path.to_path_buf());
            let info = cairn_protocol::client::cairn::daemon::meta::version(&client, (), None)
                .await
                .unwrap();
            assert_eq!(info.protocol, "cairn:daemon@0.1.0");
        }

        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert!(!path_a.exists());
        assert!(!path_b.exists());
    }

    async fn wait_for_path(path: &Path) {
        for _ in 0..100 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("path did not appear in time: {}", path.display());
    }
}
```

Note: the `use auth::Authenticator;` import in the old file is no longer needed at module scope since we pass `auth` through `bind_and_spawn`. Module visibility of `auth` and `wrpc` changes to `pub(crate)` so that `transport/webtransport.rs` can import from them via `crate::serve::wrpc` and `crate::serve::auth`.

- [ ] **Step 2: Build and fix any remaining compilation issues**

```
cargo check -p cairn-daemon 2>&1
```

Likely issues to watch for:
- Visibility: `wrpc::run_wrpc_server` and `wrpc::AuthenticatedWtAccept` need to be reachable from `transport/webtransport.rs`. The `pub(super)` in wrpc.rs makes them visible to `serve/mod.rs` but not to `serve/transport/webtransport.rs`. If needed, widen to `pub(in crate::serve)`.
- Same for `auth::Authenticator` and `auth::AuthFailure` — `transport/webtransport.rs` needs them.
- The `ListenerId` visibility — `transport/mod.rs` and submodules reference it. Ensure `pub(crate)` or `pub(in crate::serve)` as appropriate.

Fix any issues found. The code shown above uses `pub(in crate::serve)` where cross-submodule access is needed; verify this compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/cairn-daemon/src/serve/
git commit -m "refactor(serve): rewrite serve() — no mpsc, no enum dispatch

serve() now: build auth, bind+spawn transports, wait for shutdown,
drain, cleanup. Each transport owns its lifecycle. The mpsc channel,
ListenerEvent, and all the handler/shutdown helper functions are gone."
```

---

### Task 7: Run the full test suite and fix issues

**Files:**
- Possibly any file touched above (fix-up)

- [ ] **Step 1: Run unit tests for cairn-daemon**

```
cargo nextest run -p cairn-daemon
```

All existing tests should pass, including:
- `serve::tests::serve_binds_multiple_unix_listeners` — the integration test in mod.rs
- `serve::transport::unix::tests::accept_yields_peer_uid` — the PeerCredListener unit test

- [ ] **Step 2: Run the full integration test suite**

```
cargo nextest run
```

The integration tests in `crates/cairn-daemon/tests/` (smoke, daemon_meta, daemon_streaming, daemon_unary, wt_smoke) exercise the full daemon via `DaemonHarness` which calls `serve()` directly. These tests should pass without changes since the public API (`serve(daemon, shutdown, auth_chain)`) is unchanged.

- [ ] **Step 3: Run clippy and fmt**

```
cargo clippy --all-targets -- -D warnings && cargo fmt --check
```

Fix any warnings. Common ones to expect:
- Unused imports from deleted code paths
- `#[allow(dead_code)]` that's no longer needed

- [ ] **Step 4: Commit any fixes**

Only if changes were needed:

```bash
git add -u
git commit -m "fix: address clippy/test issues from serve refactor"
```

---

### Summary of what changed and why

**Before (regressions):**
```
serve()
├── mpsc channel ← all transports funnel accepted connections here
├── run_listener() per transport ← sends ListenerEvent over channel
├── handle_connection() ← receives from channel, dispatches
│   └── wrpc::serve_connection() ← matches AcceptedConnection enum
│       ├── Unix → SingleUnixAccept (Mutex<Option<Stream>>) + AcceptMode::OneInvocation
│       └── WT → AuthenticatedWtAccept + AcceptMode::UntilShutdown
```

**After (clean):**
```
serve()
├── transport::bind_and_spawn() ← binds all, spawns into JoinSet
│   ├── unix::BoundUnixListener::run()
│   │   └── PeerCredListener (impl Accept) → run_wrpc_server()
│   └── wt::BoundWebTransportListener::run()
│       └── endpoint.accept() loop → authenticate → AuthenticatedWtAccept → run_wrpc_server()
├── select! { shutdown | task failure }
└── drain + cleanup
```

Each transport owns its accept loop, auth, and wRPC lifecycle. The only shared code is `run_wrpc_server` — the genuine common pattern.

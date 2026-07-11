//! Shared axum HTTP surface for the WebSocket transport.
//!
//! This module owns the axum [`Router`] and its request handling. Today it
//! exposes:
//!
//! - `GET /healthz` — an unauthenticated liveness probe.
//! - `GET /ws` — a WebSocket upgrade that carries exactly one wRPC invocation
//!   (the browser SDK's connection-per-call model). The upgrade is gated by
//!   [`OriginPolicy`] and the shared network [`Authenticator`] before the 101
//!   response is sent; the upgraded stream is then handed to
//!   [`super::transport::websocket::serve_upgraded`].
//!
//! Later tasks extend this router with static SPA serving and `/cairn.json`.
//! Keep new HTTP routes here so the transport module stays focused on binding
//! and connection lifecycle.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::ConnCtx;
use super::ListenerId;
use super::auth::Authenticator;

/// The GUID from RFC 6455 §4.2.2 used to derive the `Sec-WebSocket-Accept`
/// value from the client's `Sec-WebSocket-Key`.
const WS_ACCEPT_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Shared state handed to every HTTP handler for one WebSocket listener.
pub(crate) struct HttpState {
    daemon: crate::daemon::Daemon,
    auth: Authenticator,
    shutdown: CancellationToken,
    origins: OriginPolicy,
    listener: ListenerId,
    /// Tracks the per-connection wRPC serve tasks spawned off successful
    /// upgrades, so the listener can drain them on graceful shutdown.
    conns: TaskTracker,
}

impl HttpState {
    pub(crate) fn new(
        daemon: crate::daemon::Daemon,
        auth: Authenticator,
        shutdown: CancellationToken,
        origins: OriginPolicy,
        listener: ListenerId,
        conns: TaskTracker,
    ) -> Self {
        Self {
            daemon,
            auth,
            shutdown,
            origins,
            listener,
            conns,
        }
    }
}

/// Build the axum router for a WebSocket listener.
pub(crate) fn router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Handle a `GET /ws` WebSocket upgrade.
///
/// Validates the upgrade headers, applies origin and peer-address gating,
/// completes the RFC 6455 handshake by hand (so the upgraded stream can be
/// driven by `tokio-websockets` rather than axum's own socket type), and spawns
/// the wRPC serve task before returning `101 Switching Protocols`.
async fn ws_upgrade(
    State(state): State<Arc<HttpState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut req: axum::extract::Request,
) -> Response {
    let headers = req.headers();
    if !is_websocket_upgrade(headers) {
        return (StatusCode::BAD_REQUEST, "expected a WebSocket upgrade").into_response();
    }

    let origin = header_str(headers, header::ORIGIN);
    let host = header_str(headers, header::HOST);
    let identity = match authorize(&state.auth, &state.origins, peer, origin, host).await {
        Ok(identity) => identity,
        Err(Rejection::Origin) => {
            tracing::warn!(
                listener = %state.listener,
                origin = origin.unwrap_or(""),
                "WS upgrade rejected: origin not allowed"
            );
            return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
        }
        Err(Rejection::Unauthorized(reason)) => {
            tracing::warn!(
                listener = %state.listener,
                %peer,
                %reason,
                "WS upgrade rejected: unauthorized"
            );
            return (StatusCode::FORBIDDEN, "unauthorized").into_response();
        }
    };

    let Some(key) = headers.get(header::SEC_WEBSOCKET_KEY).cloned() else {
        return (StatusCode::BAD_REQUEST, "missing Sec-WebSocket-Key").into_response();
    };
    let accept = sec_websocket_accept(key.as_bytes());

    // Take ownership of the pending upgrade before we commit to a 101. hyper
    // (via axum::serve's upgrade-aware connection) stashes this in the request
    // extensions; without it we can't take over the stream.
    let Some(on_upgrade) = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() else {
        return (StatusCode::BAD_REQUEST, "connection is not upgradable").into_response();
    };

    let ctx = ConnCtx { identity };
    tracing::info!(listener = %state.listener, %peer, identity = ?ctx.identity, "WS connection authenticated");

    let daemon = state.daemon.clone();
    let shutdown = state.shutdown.clone();
    state
        .conns
        .spawn(super::transport::websocket::serve_upgraded(
            on_upgrade, ctx, peer, daemon, shutdown,
        ));

    match Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_ACCEPT, accept)
        .body(Body::empty())
    {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(%error, "failed to build WS upgrade response");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

/// Why a WebSocket upgrade was refused before the handshake completed.
#[derive(Debug)]
enum Rejection {
    /// The `Origin` header did not match the request host or the allowlist.
    Origin,
    /// The peer failed network authentication (parity with the WT listener).
    Unauthorized(String),
}

/// Apply the pre-upgrade gate: origin validation followed by peer-address
/// authentication. Split out from the handler so the policy is unit-testable
/// without a live socket.
async fn authorize(
    auth: &Authenticator,
    origins: &OriginPolicy,
    peer: SocketAddr,
    origin: Option<&str>,
    host: Option<&str>,
) -> Result<crate::identity::Identity, Rejection> {
    if !origins.allows(origin, host) {
        return Err(Rejection::Origin);
    }
    auth.authenticate_network(peer)
        .await
        .map_err(|error| Rejection::Unauthorized(error.to_string()))
}

// ── Origin validation ──────────────────────────────────────────────────────

/// Decides whether a WebSocket upgrade's `Origin` header is acceptable.
///
/// Browsers always send `Origin` on cross-context WebSocket connects, so this
/// is the daemon's CSRF/DNS-rebinding guard. Non-browser clients omit `Origin`
/// entirely and are always allowed through (auth still applies).
#[derive(Clone)]
pub(crate) struct OriginPolicy {
    allowlist: Vec<String>,
}

impl OriginPolicy {
    pub(crate) fn new(allowlist: Vec<String>) -> Self {
        Self { allowlist }
    }

    /// - Absent `Origin` → allowed (non-browser client).
    /// - `Origin` whose authority equals the request `Host` → allowed (same origin).
    /// - `Origin` present in the configured allowlist → allowed.
    /// - anything else → rejected.
    fn allows(&self, origin: Option<&str>, host: Option<&str>) -> bool {
        let Some(origin) = origin else {
            return true;
        };
        if self.allowlist.iter().any(|allowed| allowed == origin) {
            return true;
        }
        match (origin_authority(origin), host) {
            (Some(authority), Some(host)) => authority.eq_ignore_ascii_case(host),
            _ => false,
        }
    }
}

/// Extract the `host[:port]` authority from an origin like `https://host:port`.
fn origin_authority(origin: &str) -> Option<&str> {
    origin
        .split_once("://")
        .map(|(_scheme, authority)| authority)
}

// ── Handshake helpers ────────────────────────────────────────────────────────

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    header_contains(headers, header::CONNECTION, "upgrade")
        && header_contains(headers, header::UPGRADE, "websocket")
        && header_eq(headers, header::SEC_WEBSOCKET_VERSION, "13")
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn header_contains(headers: &HeaderMap, name: header::HeaderName, needle: &str) -> bool {
    header_str(headers, name).is_some_and(|v| v.to_ascii_lowercase().contains(needle))
}

fn header_eq(headers: &HeaderMap, name: header::HeaderName, expected: &str) -> bool {
    header_str(headers, name).is_some_and(|v| v.eq_ignore_ascii_case(expected))
}

/// Compute the RFC 6455 `Sec-WebSocket-Accept` response value:
/// `base64(sha1(key + GUID))`.
fn sec_websocket_accept(key: &[u8]) -> String {
    use base64::Engine as _;

    let mut input = Vec::with_capacity(key.len() + WS_ACCEPT_GUID.len());
    input.extend_from_slice(key);
    input.extend_from_slice(WS_ACCEPT_GUID);
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &input);
    base64::engine::general_purpose::STANDARD.encode(digest.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(allow: &[&str]) -> OriginPolicy {
        OriginPolicy::new(allow.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn absent_origin_is_allowed() {
        assert!(policy(&[]).allows(None, Some("127.0.0.1:8080")));
    }

    #[test]
    fn same_host_origin_is_allowed() {
        assert!(policy(&[]).allows(Some("http://127.0.0.1:8080"), Some("127.0.0.1:8080")));
        // Host without an explicit port (default 80/443) matches a portless origin.
        assert!(policy(&[]).allows(Some("https://cairn.example"), Some("cairn.example")));
    }

    #[test]
    fn allowlisted_origin_is_allowed() {
        let p = policy(&["https://app.example"]);
        assert!(p.allows(Some("https://app.example"), Some("127.0.0.1:8080")));
    }

    #[test]
    fn mismatched_origin_is_rejected() {
        assert!(!policy(&[]).allows(Some("http://evil.example"), Some("127.0.0.1:8080")));
        // Allowlist entries must match exactly, including scheme/port.
        let p = policy(&["https://app.example"]);
        assert!(!p.allows(Some("http://app.example"), Some("127.0.0.1:8080")));
    }

    // ── Pre-upgrade gate (origin + peer-address auth) ────────────────────────

    fn test_authenticator() -> Authenticator {
        let daemon = crate::daemon::Daemon::new(crate::config::DaemonConfig::default())
            .expect("default daemon config is valid");
        // No auth chain: loopback → anonymous, non-loopback → rejected.
        Authenticator::new(&daemon, None).expect("authenticator")
    }

    #[tokio::test]
    async fn loopback_without_chain_is_anonymous() {
        let auth = test_authenticator();
        let identity = authorize(
            &auth,
            &policy(&[]),
            "127.0.0.1:5000".parse().unwrap(),
            None,
            None,
        )
        .await
        .expect("loopback should be allowed");
        assert!(matches!(identity, crate::identity::Identity::Anonymous));
    }

    #[tokio::test]
    async fn non_loopback_without_chain_is_rejected() {
        let auth = test_authenticator();
        let result = authorize(
            &auth,
            &policy(&[]),
            "203.0.113.7:5000".parse().unwrap(),
            None,
            None,
        )
        .await;
        assert!(
            matches!(result, Err(Rejection::Unauthorized(_))),
            "non-loopback peer without an auth chain must be rejected"
        );
    }

    #[tokio::test]
    async fn bad_origin_is_rejected_before_auth() {
        let auth = test_authenticator();
        let result = authorize(
            &auth,
            &policy(&[]),
            "127.0.0.1:5000".parse().unwrap(),
            Some("http://evil.example"),
            Some("127.0.0.1:8080"),
        )
        .await;
        assert!(matches!(result, Err(Rejection::Origin)));
    }
}

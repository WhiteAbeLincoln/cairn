//! Shared axum HTTP surface for the `ws://` and dedicated web-ui listeners.
//!
//! [`router`] builds the full surface for a `ws://` listener:
//!
//! - `GET /healthz` — an unauthenticated liveness probe.
//! - `GET /ws` — a WebSocket upgrade that carries exactly one wRPC invocation
//!   (the browser SDK's connection-per-call model). The upgrade is gated by
//!   [`OriginPolicy`] and the shared network [`Authenticator`] before the 101
//!   response is sent; the upgraded stream is then handed to
//!   [`super::transport::websocket::serve_upgraded`].
//! - `GET /cairn.json` — always present, regardless of `--web-ui`.
//! - SPA fallback (unknown paths -> `index.html`) — only when `--web-ui`
//!   attaches SPA routes to this listener.
//!
//! [`ui_router`] builds the smaller surface for the dedicated
//! `--web-ui=host:port` listener: `/cairn.json` and the SPA fallback, and
//! nothing else (no `/ws`, no `/healthz`) — see the design spec's "Web UI
//! serving" section.
//!
//! Keep new HTTP routes here so the transport modules stay focused on binding
//! and connection lifecycle.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::ConnCtx;
use super::ListenerId;
use super::assets::Assets;
use super::auth::Authenticator;
use super::cairn_json::{self, CairnJsonInfo};

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
    /// `/cairn.json` + SPA-fallback state, shared with [`ui_router`].
    spa: Arc<SpaState>,
}

impl HttpState {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        daemon: crate::daemon::Daemon,
        auth: Authenticator,
        shutdown: CancellationToken,
        origins: OriginPolicy,
        listener: ListenerId,
        conns: TaskTracker,
        spa: Arc<SpaState>,
    ) -> Self {
        Self {
            daemon,
            auth,
            shutdown,
            origins,
            listener,
            conns,
            spa,
        }
    }
}

/// Build the axum router for a `ws://` listener: `/healthz`, `/ws`, plus the
/// always-present `/cairn.json` and (only when SPA assets were attached to
/// this listener) the SPA fallback.
pub(crate) fn router(state: Arc<HttpState>) -> Router {
    let core = Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_upgrade))
        .with_state(state.clone());
    core.merge(ui_router(state.spa.clone()))
}

/// Build the axum router for the dedicated `--web-ui=host:port` listener:
/// only `/cairn.json` and the SPA fallback, no `/ws`/`/healthz`.
pub(crate) fn ui_router(state: Arc<SpaState>) -> Router {
    let mut router = Router::new().route("/cairn.json", get(cairn_json_handler));
    if state.assets.is_some() {
        router = router.fallback(get(spa_fallback));
    }
    router.with_state(state)
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

// ── /cairn.json + SPA fallback ───────────────────────────────────────────

/// State shared by `/cairn.json` and the SPA fallback across both the
/// `ws://` listener's router and the dedicated UI listener's router.
pub(crate) struct SpaState {
    /// `None` when this listener doesn't serve the SPA (no `--web-ui`, or a
    /// `ws://` listener when `--web-ui=host:port` attaches only to the
    /// dedicated listener instead).
    pub(crate) assets: Option<Arc<Assets>>,
    pub(crate) cairn_json: Arc<CairnJsonInfo>,
    /// `true` for a `ws://` listener's own HTTP server (its `/cairn.json`
    /// reports the relative `"/ws"`); `false` for the dedicated UI listener
    /// (no `/ws` of its own — see [`cairn_json::render`]).
    pub(crate) is_ws_listener: bool,
}

async fn cairn_json_handler(State(state): State<Arc<SpaState>>, headers: HeaderMap) -> Response {
    let host = cairn_json::request_host(&headers);
    let doc = cairn_json::render(&state.cairn_json, state.is_ws_listener, &host);
    let mut response = axum::Json(doc).into_response();
    // The contents are public (endpoints + a cert fingerprint) so a
    // standalone-hosted UI can bootstrap from a pasted daemon URL.
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    response
}

/// SPA fallback: unknown paths serve `index.html` for client-side routing.
/// Only registered as a route when `state.assets` is `Some`; the `None`
/// branch below is an unreachable-in-practice defensive fallback.
async fn spa_fallback(State(state): State<Arc<SpaState>>, uri: Uri) -> Response {
    let Some(assets) = &state.assets else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let rel_path = uri.path().trim_start_matches('/');
    let asset = if rel_path.is_empty() {
        None
    } else {
        assets.get(rel_path)
    }
    .or_else(|| assets.index());

    match asset {
        Some(asset) => ([(header::CONTENT_TYPE, asset.content_type)], asset.body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
    let identity = match authorize(&state.auth, &state.origins, peer, headers).await {
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

/// Apply the pre-upgrade gate: origin validation followed by the real auth
/// chain (network address + upgrade headers). Split out from the handler so
/// the policy is unit-testable without a live socket.
async fn authorize(
    auth: &Authenticator,
    origins: &OriginPolicy,
    peer: SocketAddr,
    headers: &HeaderMap,
) -> Result<crate::identity::Identity, Rejection> {
    let origin = header_str(headers, header::ORIGIN);
    let host = header_str(headers, header::HOST);
    if !origins.allows(origin, host) {
        return Err(Rejection::Origin);
    }
    auth.authenticate_http(peer, headers.clone())
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

    // ── Pre-upgrade gate (origin + auth chain) ───────────────────────────────

    /// Build a `HeaderMap` from `(name, value)` pairs — used to stand in for
    /// upgrade request headers without a live socket.
    fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                http::HeaderName::from_bytes(name.as_bytes()).expect("valid header name"),
                http::HeaderValue::from_str(value).expect("valid header value"),
            );
        }
        map
    }

    fn test_authenticator() -> Authenticator {
        let daemon = crate::daemon::Daemon::new(crate::config::DaemonConfig::default())
            .expect("default daemon config is valid");
        // No auth chain: loopback → anonymous, non-loopback → rejected.
        Authenticator::new(&daemon, None).expect("authenticator")
    }

    /// An authenticator whose chain is just the `tailscale-serve` backend —
    /// isolates the identity-matrix tests from the whois backend (which
    /// requires a running `tailscaled`).
    fn test_authenticator_with_tailscale_serve() -> Authenticator {
        let daemon = crate::daemon::Daemon::new(crate::config::DaemonConfig::default())
            .expect("default daemon config is valid");
        let chain = crate::auth::AuthChain::new(vec![Box::new(
            crate::auth::tailscale_serve::TailscaleServeBackend::new(),
        )]);
        Authenticator::new(&daemon, Some(chain)).expect("authenticator")
    }

    #[tokio::test]
    async fn loopback_without_chain_is_anonymous() {
        let auth = test_authenticator();
        let identity = authorize(
            &auth,
            &policy(&[]),
            "127.0.0.1:5000".parse().unwrap(),
            &HeaderMap::new(),
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
            &HeaderMap::new(),
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
        let headers = header_map(&[
            ("origin", "http://evil.example"),
            ("host", "127.0.0.1:8080"),
        ]);
        let result = authorize(
            &auth,
            &policy(&[]),
            "127.0.0.1:5000".parse().unwrap(),
            &headers,
        )
        .await;
        assert!(matches!(result, Err(Rejection::Origin)));
    }

    // ── tailscale-serve identity matrix (acceptance criteria) ────────────────

    #[tokio::test]
    async fn tailscale_serve_header_from_loopback_is_identified() {
        let auth = test_authenticator_with_tailscale_serve();
        let headers = header_map(&[("tailscale-user-login", "alice@example.com")]);
        let identity = authorize(
            &auth,
            &policy(&[]),
            "127.0.0.1:5000".parse().unwrap(),
            &headers,
        )
        .await
        .expect("loopback peer with a valid header should be identified");
        assert!(matches!(
            identity,
            crate::identity::Identity::Tailscale { ref login, .. } if login == "alice@example.com"
        ));
    }

    #[tokio::test]
    async fn tailscale_serve_header_from_non_loopback_is_ignored() {
        let auth = test_authenticator_with_tailscale_serve();
        let headers = header_map(&[("tailscale-user-login", "alice@example.com")]);
        let result = authorize(
            &auth,
            &policy(&[]),
            "203.0.113.7:5000".parse().unwrap(),
            &headers,
        )
        .await;
        assert!(
            matches!(result, Err(Rejection::Unauthorized(_))),
            "a non-loopback peer's tailscale-serve header must be ignored (fall through the chain), \
             got: {result:?}"
        );
    }

    // The other two rows of the acceptance identity matrix — no chain +
    // loopback → anonymous, no chain + non-loopback → rejected — are already
    // covered above by `loopback_without_chain_is_anonymous` and
    // `non_loopback_without_chain_is_rejected`.

    // ── /cairn.json + SPA fallback (handler-level, no live socket) ──────────

    fn spa_state(assets: Option<Assets>, is_ws_listener: bool) -> Arc<SpaState> {
        Arc::new(SpaState {
            assets: assets.map(Arc::new),
            cairn_json: Arc::new(CairnJsonInfo::default()),
            is_ws_listener,
        })
    }

    #[tokio::test]
    async fn cairn_json_sets_cors_header() {
        let state = spa_state(None, true);
        let response = cairn_json_handler(State(state), HeaderMap::new()).await;
        assert_eq!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );
    }

    #[tokio::test]
    async fn spa_fallback_serves_index_for_unknown_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"<html>spa</html>").unwrap();
        let assets = Assets::resolve(Some(dir.path())).unwrap();
        let state = spa_state(Some(assets), true);

        let uri: Uri = "/some/unknown/route".parse().unwrap();
        let response = spa_fallback(State(state), uri).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"<html>spa</html>");
    }

    #[tokio::test]
    async fn spa_fallback_serves_exact_asset() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"<html>spa</html>").unwrap();
        std::fs::write(dir.path().join("app.js"), b"console.log(1)").unwrap();
        let assets = Assets::resolve(Some(dir.path())).unwrap();
        let state = spa_state(Some(assets), true);

        let uri: Uri = "/app.js".parse().unwrap();
        let response = spa_fallback(State(state), uri).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("text/javascript")
        );
    }

    #[tokio::test]
    async fn spa_fallback_without_assets_is_not_found() {
        let state = spa_state(None, true);
        let uri: Uri = "/anything".parse().unwrap();
        let response = spa_fallback(State(state), uri).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

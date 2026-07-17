//! `tailscale-serve` auth backend: trusts identity headers injected by a
//! *local* `tailscale serve` reverse proxy.
//!
//! `tailscale serve` runs alongside `tailscaled` on the same host and proxies
//! tailnet traffic to a local backend, adding `Tailscale-User-*` headers that
//! identify the tailnet user who made the request
//! (<https://tailscale.com/docs/concepts/tailscale-identity>):
//!
//! - `Tailscale-User-Login` — the tailnet login name (e.g. `alice@example.com`).
//! - `Tailscale-User-Name` — the display name (e.g. `Alice Architect`).
//!
//! These are populated only for user identities (not tagged devices) and only
//! for Serve traffic — Funnel (public internet) traffic never carries them.
//! `tailscale serve` also strips any of these headers present on the
//! *incoming* tailnet request before adding its own, so a tailnet peer cannot
//! spoof them by hand *through the proxy*.
//!
//! That guarantee only holds for traffic that actually went through the
//! proxy. Nothing stops a peer from connecting to cairn-daemon directly and
//! setting these headers itself — `tailscale serve` isn't in a position to
//! strip anything it never saw. The proxy always forwards to a **loopback**
//! backend address, so the one property cairn-daemon *can* check is: did this
//! connection arrive from loopback? A non-loopback peer presenting these
//! headers is, by construction, not `tailscale serve` — it's either a
//! misconfigured client or an active spoofing attempt — so this backend
//! authenticates ONLY when `peer_addr` is loopback and otherwise ignores the
//! headers, falling through to the next backend in the chain.

use std::net::SocketAddr;

use http::HeaderMap;

use crate::auth::{AuthBackend, AuthContext, AuthError, AuthPhase, TransportContext};
use crate::identity::Identity;

const LOGIN_HEADER: &str = "Tailscale-User-Login";
const NAME_HEADER: &str = "Tailscale-User-Name";

/// Unlike the whois-based [`super::tailscale::TailscaleBackend`], `tailscale
/// serve` doesn't hand us a per-request node identifier — so identities
/// resolved here carry a fixed marker in place of a real computed node name.
const NODE_MARKER: &str = "tailscale-serve";

pub struct TailscaleServeBackend;

impl TailscaleServeBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TailscaleServeBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthBackend for TailscaleServeBackend {
    fn authenticate(
        &self,
        ctx: &AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>>
    {
        // No I/O here, but the trait's future is tied to `&self`'s lifetime,
        // not `ctx`'s (see TailscaleBackend for the same pattern) — resolve
        // synchronously before boxing so nothing borrows `ctx`.
        let result = match &ctx.transport {
            TransportContext::Http { peer_addr, headers } => from_headers(*peer_addr, headers),
            TransportContext::WebTransport { .. } => Err(AuthError::NotApplicable),
        };
        Box::pin(async move { result })
    }

    fn phase(&self) -> AuthPhase {
        AuthPhase::Transport
    }
}

/// Resolve identity from `tailscale serve` headers. Returns `NotApplicable`
/// (fall through the chain) whenever the login header is missing or the peer
/// isn't loopback.
fn from_headers(peer_addr: SocketAddr, headers: &HeaderMap) -> Result<Identity, AuthError> {
    if !peer_addr.ip().is_loopback() {
        return Err(AuthError::NotApplicable);
    }

    let login = header_value(headers, LOGIN_HEADER).ok_or(AuthError::NotApplicable)?;
    let display_name = header_value(headers, NAME_HEADER).unwrap_or_else(|| login.clone());

    Ok(Identity::Tailscale {
        login,
        display_name,
        node: NODE_MARKER.to_string(),
    })
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?.to_str().ok().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn http_ctx(peer_addr: SocketAddr, headers: HeaderMap) -> AuthContext {
        AuthContext {
            transport: TransportContext::Http { peer_addr, headers },
            token: None,
        }
    }

    #[tokio::test]
    async fn loopback_with_headers_resolves_identity() {
        let backend = TailscaleServeBackend::new();
        let headers = header_map(&[
            (LOGIN_HEADER, "alice@example.com"),
            (NAME_HEADER, "Alice Architect"),
        ]);
        let result = backend
            .authenticate(&http_ctx("127.0.0.1:9000".parse().unwrap(), headers))
            .await;
        match result {
            Ok(Identity::Tailscale {
                login,
                display_name,
                ..
            }) => {
                assert_eq!(login, "alice@example.com");
                assert_eq!(display_name, "Alice Architect");
            }
            other => panic!("expected Tailscale identity, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn loopback_ipv6_with_headers_resolves_identity() {
        let backend = TailscaleServeBackend::new();
        let headers = header_map(&[(LOGIN_HEADER, "alice@example.com")]);
        let result = backend
            .authenticate(&http_ctx("[::1]:9000".parse().unwrap(), headers))
            .await;
        assert!(matches!(result, Ok(Identity::Tailscale { .. })));
    }

    #[tokio::test]
    async fn missing_display_name_falls_back_to_login() {
        let backend = TailscaleServeBackend::new();
        let headers = header_map(&[(LOGIN_HEADER, "alice@example.com")]);
        let result = backend
            .authenticate(&http_ctx("127.0.0.1:9000".parse().unwrap(), headers))
            .await
            .expect("loopback with login header should resolve");
        assert!(
            matches!(result, Identity::Tailscale { ref display_name, .. } if display_name == "alice@example.com")
        );
    }

    #[tokio::test]
    async fn non_loopback_with_headers_is_not_applicable() {
        let backend = TailscaleServeBackend::new();
        let headers = header_map(&[(LOGIN_HEADER, "alice@example.com")]);
        let result = backend
            .authenticate(&http_ctx("203.0.113.7:9000".parse().unwrap(), headers))
            .await;
        assert!(
            matches!(result, Err(AuthError::NotApplicable)),
            "a non-loopback peer's tailscale-serve headers must be ignored, not trusted"
        );
    }

    #[tokio::test]
    async fn loopback_without_headers_is_not_applicable() {
        let backend = TailscaleServeBackend::new();
        let result = backend
            .authenticate(&http_ctx(
                "127.0.0.1:9000".parse().unwrap(),
                HeaderMap::new(),
            ))
            .await;
        assert!(matches!(result, Err(AuthError::NotApplicable)));
    }

    #[tokio::test]
    async fn web_transport_context_is_not_applicable() {
        let backend = TailscaleServeBackend::new();
        let ctx = AuthContext {
            transport: TransportContext::WebTransport {
                peer_addr: "127.0.0.1:9000".parse().unwrap(),
            },
            token: None,
        };
        let result = backend.authenticate(&ctx).await;
        assert!(matches!(result, Err(AuthError::NotApplicable)));
    }
}

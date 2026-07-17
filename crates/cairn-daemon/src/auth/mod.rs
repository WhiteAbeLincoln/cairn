//! Pluggable authentication backends for network-facing connections
//! (WebTransport and WebSocket).

pub mod none;
pub mod tailscale;
pub mod tailscale_serve;

use std::net::SocketAddr;

use http::HeaderMap;

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

/// Transport-specific connection material, produced by the listener that
/// accepted the connection. Each variant carries exactly the material
/// available on that transport.
#[derive(Debug)]
pub enum TransportContext {
    WebTransport {
        peer_addr: SocketAddr,
    },
    /// A WebSocket upgrade request. `headers` are the raw upgrade request
    /// headers — carried through so backends like `tailscale-serve` can read
    /// proxy-injected identity headers without the transport layer knowing
    /// about any particular backend's header names.
    Http {
        peer_addr: SocketAddr,
        headers: HeaderMap,
    },
}

/// Information available to auth backends for identity resolution.
/// Created by the transport layer, enriched by the first-message phase.
#[derive(Debug)]
pub struct AuthContext {
    pub transport: TransportContext,
    pub token: Option<String>,
}

/// A backend that can resolve the identity of a connection, given whatever
/// [`TransportContext`] its transport was accepted on.
pub trait AuthBackend: Send + Sync {
    fn authenticate(
        &self,
        ctx: &AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>>;

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

    /// Run all transport-phase backends. First success wins.
    pub async fn try_transport(&self, ctx: &AuthContext) -> Result<Identity, AuthError> {
        self.run_phase(AuthPhase::Transport, ctx).await
    }

    /// Run all first-message-phase backends.
    pub async fn try_first_message(&self, ctx: &AuthContext) -> Result<Identity, AuthError> {
        self.run_phase(AuthPhase::FirstMessage, ctx).await
    }

    async fn run_phase(&self, phase: AuthPhase, ctx: &AuthContext) -> Result<Identity, AuthError> {
        for backend in self.backends.iter().filter(|b| b.phase() == phase) {
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
        fn authenticate(
            &self,
            _ctx: &AuthContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>,
        > {
            Box::pin(async { Ok(Identity::Anonymous) })
        }
        fn phase(&self) -> AuthPhase {
            AuthPhase::Transport
        }
    }

    struct AlwaysReject;
    impl AuthBackend for AlwaysReject {
        fn authenticate(
            &self,
            _ctx: &AuthContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>,
        > {
            Box::pin(async { Err(AuthError::Rejected("denied".into())) })
        }
        fn phase(&self) -> AuthPhase {
            AuthPhase::Transport
        }
    }

    struct SkipBackend;
    impl AuthBackend for SkipBackend {
        fn authenticate(
            &self,
            _ctx: &AuthContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>,
        > {
            Box::pin(async { Err(AuthError::NotApplicable) })
        }
        fn phase(&self) -> AuthPhase {
            AuthPhase::Transport
        }
    }

    fn test_ctx() -> AuthContext {
        AuthContext {
            transport: TransportContext::WebTransport {
                peer_addr: "127.0.0.1:1234".parse().unwrap(),
            },
            token: None,
        }
    }

    #[tokio::test]
    async fn chain_returns_first_success() {
        let chain = AuthChain::new(vec![Box::new(SkipBackend), Box::new(AlwaysAnon)]);
        let result = chain.try_transport(&test_ctx()).await;
        assert!(matches!(result, Ok(Identity::Anonymous)));
    }

    #[tokio::test]
    async fn chain_stops_on_rejection() {
        let chain = AuthChain::new(vec![Box::new(AlwaysReject), Box::new(AlwaysAnon)]);
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

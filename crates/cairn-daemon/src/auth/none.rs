//! The `none` auth backend: accepts all connections as anonymous.

use crate::auth::{AuthBackend, AuthContext, AuthError, AuthPhase};
use crate::identity::Identity;

pub struct NoneBackend;

impl AuthBackend for NoneBackend {
    fn authenticate(
        &self,
        _ctx: &AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>>
    {
        Box::pin(async { Ok(Identity::Anonymous) })
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
            transport: crate::auth::TransportContext::WebTransport {
                peer_addr: "192.168.1.50:9999".parse().unwrap(),
            },
            token: None,
        };
        let result = backend.authenticate(&ctx).await;
        assert!(matches!(result, Ok(Identity::Anonymous)));
    }
}

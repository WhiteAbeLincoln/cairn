use std::fmt;
use std::sync::Arc;

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

    pub(super) async fn authenticate_network(
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
pub(super) enum AuthFailure {
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

//! Daemon ownership and environment wiring for per-session HTTP proxies.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use cairn_http_proxy::{ProxyAuthority, ProxySession, ProxySessionConfig, Route};
use cairn_protocol::cairn::daemon::types::HttpProxySpec;

#[derive(Clone, Debug)]
pub struct ProxyEnvironment {
    pub proxy_url: String,
    pub ca_path: PathBuf,
    pub bundle_path: PathBuf,
}

pub struct ProxyManager {
    runtime_parent: PathBuf,
    authority: Mutex<Option<Arc<ProxyAuthority>>>,
}

impl ProxyManager {
    pub fn new(runtime_parent: PathBuf) -> Self {
        Self {
            runtime_parent,
            authority: Mutex::new(None),
        }
    }

    pub async fn start(
        &self,
        spec: &HttpProxySpec,
    ) -> anyhow::Result<(Arc<ProxySession>, ProxyEnvironment)> {
        let authority = self.authority()?;
        let config = ProxySessionConfig {
            routes: spec
                .routes
                .iter()
                .map(|route| Route {
                    methods: route.methods.clone(),
                    host: route.host.clone(),
                    path_prefix: route.path_prefix.clone(),
                })
                .collect(),
            ..ProxySessionConfig::default()
        };
        let proxy = Arc::new(ProxySession::start(&authority, config).await?);
        let environment = ProxyEnvironment {
            proxy_url: proxy.proxy_url(),
            ca_path: authority.ca_path().to_path_buf(),
            bundle_path: authority.bundle_path().to_path_buf(),
        };
        Ok((proxy, environment))
    }

    fn authority(&self) -> anyhow::Result<Arc<ProxyAuthority>> {
        let mut authority = lock_recover(&self.authority);
        if let Some(authority) = authority.as_ref() {
            return Ok(Arc::clone(authority));
        }
        let created = Arc::new(ProxyAuthority::create(&self.runtime_parent)?);
        *authority = Some(Arc::clone(&created));
        Ok(created)
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

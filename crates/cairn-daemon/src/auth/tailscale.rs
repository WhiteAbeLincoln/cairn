//! Tailscale auth backend: resolves identity via the LocalAPI `whois` endpoint.

use std::net::SocketAddr;

use crate::auth::{AuthBackend, AuthContext, AuthError, AuthPhase};
use crate::identity::Identity;

type HttpClient = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    http_body_util::Empty<bytes::Bytes>,
>;

pub struct TailscaleBackend {
    base_url: String,
    client: HttpClient,
}

impl TailscaleBackend {
    pub fn new() -> anyhow::Result<Self> {
        let base_url = if cfg!(target_os = "macos") {
            "http://127.0.0.1:41112".to_string()
        } else {
            // TODO: Linux uses a Unix domain socket at /var/run/tailscale/tailscaled.sock
            // which requires hyper-util's UDS connector. For now, fall through to TCP.
            "http://localhost".to_string()
        };
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http();
        Ok(Self { base_url, client })
    }
}

impl AuthBackend for TailscaleBackend {
    fn authenticate(
        &self,
        ctx: &AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>>
    {
        // SocketAddr is Copy; pull it out so the future only borrows `self`.
        let peer_addr = ctx.peer_addr;
        Box::pin(self.do_authenticate(peer_addr))
    }

    fn phase(&self) -> AuthPhase {
        AuthPhase::Transport
    }
}

impl TailscaleBackend {
    async fn do_authenticate(&self, peer_addr: SocketAddr) -> Result<Identity, AuthError> {
        match self.whois(&peer_addr).await {
            Ok(info) => Ok(Identity::Tailscale {
                login: info.login,
                display_name: info.display_name,
                node: info.node,
            }),
            Err(WhoisError::NotFound) => Err(AuthError::NotApplicable),
            Err(WhoisError::Forbidden(reason)) => Err(AuthError::Rejected(reason)),
            Err(WhoisError::Unavailable(e)) => {
                tracing::warn!(error = %e, "tailscale LocalAPI unavailable");
                Err(AuthError::NotApplicable)
            }
        }
    }

    async fn whois(&self, addr: &SocketAddr) -> Result<WhoisInfo, WhoisError> {
        let url = format!("{}/localapi/v0/whois?addr={addr}", self.base_url);
        let body = self.http_get(&url).await?;
        parse_whois_response(&body)
    }

    async fn http_get(&self, url: &str) -> Result<String, WhoisError> {
        use http_body_util::BodyExt as _;

        let req = hyper::Request::get(url)
            .body(http_body_util::Empty::new())
            .map_err(|e| WhoisError::Unavailable(e.to_string()))?;

        let resp =
            self.client
                .request(req)
                .await
                .map_err(|e: hyper_util::client::legacy::Error| {
                    WhoisError::Unavailable(e.to_string())
                })?;

        if resp.status() == hyper::StatusCode::NOT_FOUND {
            return Err(WhoisError::NotFound);
        }
        if resp.status() == hyper::StatusCode::FORBIDDEN {
            return Err(WhoisError::Forbidden("access denied by tailscaled".into()));
        }
        if !resp.status().is_success() {
            return Err(WhoisError::Unavailable(format!(
                "LocalAPI returned {}",
                resp.status()
            )));
        }

        let body = resp
            .into_body()
            .collect()
            .await
            .map_err(|e: hyper::Error| WhoisError::Unavailable(e.to_string()))?
            .to_bytes();

        String::from_utf8(body.to_vec()).map_err(|e| WhoisError::Unavailable(e.to_string()))
    }
}

struct WhoisInfo {
    login: String,
    display_name: String,
    node: String,
}

#[derive(Debug)]
enum WhoisError {
    NotFound,
    Forbidden(String),
    Unavailable(String),
}

fn parse_whois_response(body: &str) -> Result<WhoisInfo, WhoisError> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| WhoisError::Unavailable(e.to_string()))?;

    let user_profile = json.get("UserProfile").ok_or(WhoisError::NotFound)?;
    let node = json
        .get("Node")
        .and_then(|n| n.get("ComputedName"))
        .and_then(|n| n.as_str())
        .unwrap_or("unknown")
        .to_string();
    let login = user_profile
        .get("LoginName")
        .and_then(|v| v.as_str())
        .ok_or(WhoisError::NotFound)?
        .to_string();
    let display_name = user_profile
        .get("DisplayName")
        .and_then(|v| v.as_str())
        .unwrap_or(&login)
        .to_string();

    Ok(WhoisInfo {
        login,
        display_name,
        node,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_whois_response() {
        let body = r#"{
            "Node": { "ComputedName": "myhost" },
            "UserProfile": {
                "LoginName": "user@example.com",
                "DisplayName": "Test User"
            }
        }"#;
        let info = parse_whois_response(body).unwrap();
        assert_eq!(info.login, "user@example.com");
        assert_eq!(info.display_name, "Test User");
        assert_eq!(info.node, "myhost");
    }

    #[test]
    fn parse_whois_missing_display_name_falls_back_to_login() {
        let body = r#"{
            "Node": { "ComputedName": "myhost" },
            "UserProfile": { "LoginName": "user@example.com" }
        }"#;
        let info = parse_whois_response(body).unwrap();
        assert_eq!(info.display_name, "user@example.com");
    }

    #[test]
    fn parse_whois_missing_user_profile_returns_not_found() {
        let body = r#"{ "Node": { "ComputedName": "myhost" } }"#;
        assert!(matches!(
            parse_whois_response(body),
            Err(WhoisError::NotFound)
        ));
    }

    #[test]
    fn parse_whois_missing_node_uses_unknown() {
        let body = r#"{ "UserProfile": { "LoginName": "user@example.com" } }"#;
        let info = parse_whois_response(body).unwrap();
        assert_eq!(info.node, "unknown");
    }

    #[test]
    fn parse_invalid_json_returns_unavailable() {
        assert!(matches!(
            parse_whois_response("not json"),
            Err(WhoisError::Unavailable(_))
        ));
    }
}

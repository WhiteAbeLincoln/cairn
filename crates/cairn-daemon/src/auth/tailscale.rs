//! Tailscale auth backend: resolves identity via the LocalAPI `whois` endpoint.
//!
//! The Tailscale LocalAPI is reachable two different ways depending on the
//! install method (NOT the host OS — both methods exist on both platforms):
//!
//! - **Unix socket** (`tailscaled` installed via package manager / Homebrew /
//!   tarball): authorisation is by socket-file permissions / SO_PEERCRED, so
//!   no `Authorization` header is sent. The default path is
//!   `/var/run/tailscale/tailscaled.sock`; `$CAIRN_TAILSCALE_SOCKET`
//!   overrides for non-default installs.
//! - **Localhost TCP** (macOS Tailscale GUI app from the App Store or
//!   standalone): a localhost listener whose port and a `sameuserproof`
//!   token are written under `/Library/Tailscale/`. The token is sent as
//!   HTTP Basic auth.
//!
//! Detection is by probing at startup, not by `#[cfg(target_os)]` — a future
//! Linux GUI install or an existing Homebrew `tailscaled` on macOS both Just
//! Work without code changes.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::auth::{AuthBackend, AuthContext, AuthError, AuthPhase, TransportContext};
use crate::identity::Identity;

type HttpClient = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    http_body_util::Empty<bytes::Bytes>,
>;

/// How to reach the Tailscale LocalAPI on this host. The two variants share
/// the request / response shape but use very different connection plumbing —
/// the TCP client piggybacks on hyper-util's pooled `Client`; the Unix path
/// opens a fresh `UnixStream` per request and drives a one-shot
/// `hyper::client::conn::http1`.
enum LocalApi {
    Tcp {
        base_url: String,
        token: String,
        client: Box<HttpClient>,
    },
    Unix {
        socket_path: PathBuf,
    },
}

pub struct TailscaleBackend {
    api: LocalApi,
}

/// Standard filesystem locations the LocalAPI may live at, probed in order:
///   1. `$CAIRN_TAILSCALE_SOCKET` (explicit override)
///   2. `/var/run/tailscale/tailscaled.sock` — Linux default, also Homebrew
///      `tailscaled` on macOS
///   3. `/var/run/tailscaled.socket` — older / alt macOS install layouts
///   4. `/Library/Tailscale/ipnport` — macOS App Store / standalone GUI
const IPNPORT_PATH: &str = "/Library/Tailscale/ipnport";
const TAILSCALED_SOCKET_CANDIDATES: &[&str] = &[
    "/var/run/tailscale/tailscaled.sock",
    "/var/run/tailscaled.socket",
];

/// Credentials for the GUI-app TCP install.
struct IpnportCreds {
    port: u16,
    token: String,
}

impl TailscaleBackend {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            api: detect_local_api()?,
        })
    }
}

/// Probe the filesystem for an available LocalAPI endpoint. Returns the
/// first match in priority order; errors with a list of paths checked if
/// nothing was found, so the user knows where we looked.
fn detect_local_api() -> anyhow::Result<LocalApi> {
    if let Some(raw) = std::env::var_os("CAIRN_TAILSCALE_SOCKET") {
        let path = PathBuf::from(raw);
        if !path.exists() {
            anyhow::bail!(
                "CAIRN_TAILSCALE_SOCKET points to {} which does not exist",
                path.display()
            );
        }
        return Ok(LocalApi::Unix { socket_path: path });
    }

    for candidate in TAILSCALED_SOCKET_CANDIDATES {
        let p = Path::new(candidate);
        if p.exists() {
            return Ok(LocalApi::Unix {
                socket_path: p.to_path_buf(),
            });
        }
    }

    // ipnport is a symlink whose *target name* is the port number — the
    // target itself doesn't exist as a file, so Path::exists() (which follows
    // symlinks) returns false. Check for the symlink with symlink_metadata().
    if std::fs::symlink_metadata(IPNPORT_PATH).is_ok() {
        let creds = read_ipnport_creds()?;
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http();
        return Ok(LocalApi::Tcp {
            base_url: format!("http://127.0.0.1:{}", creds.port),
            token: creds.token,
            client: Box::new(client),
        });
    }

    let socket_list = TAILSCALED_SOCKET_CANDIDATES.join(", ");
    anyhow::bail!(
        "tailscale LocalAPI not found. Checked: $CAIRN_TAILSCALE_SOCKET, {socket_list}, \
         {IPNPORT_PATH}. Is tailscaled running?"
    );
}

/// Read LocalAPI port + auth token from `/Library/Tailscale/` (macOS GUI app).
///
/// The GUI app writes:
/// - `/Library/Tailscale/ipnport` — symlink whose target is the port number
/// - `/Library/Tailscale/sameuserproof-{port}` — file containing the auth token
///
/// The proof file is `root:admin 0640`, so this works for any process whose
/// effective user is in the `admin` group (the common case today). Under a
/// future multi-user model the master process runs as root, so access is free.
fn read_ipnport_creds() -> anyhow::Result<IpnportCreds> {
    use std::fs;

    let target = fs::read_link(IPNPORT_PATH)
        .map_err(|e| anyhow::anyhow!("cannot read {IPNPORT_PATH}: {e} (is Tailscale running?)"))?;
    let port: u16 = target
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("ipnport symlink target is not UTF-8"))?
        .parse()
        .map_err(|e| anyhow::anyhow!("ipnport symlink target is not a valid port: {e}"))?;

    let proof_path = format!("/Library/Tailscale/sameuserproof-{port}");
    let token = fs::read_to_string(&proof_path)
        .map_err(|e| {
            anyhow::anyhow!(
                "cannot read {proof_path}: {e} \
                 (daemon user may need to be in the admin group)"
            )
        })?
        .trim()
        .to_string();

    Ok(IpnportCreds { port, token })
}

impl AuthBackend for TailscaleBackend {
    fn authenticate(
        &self,
        ctx: &AuthContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Identity, AuthError>> + Send + '_>>
    {
        // SocketAddr is Copy; pull it out so the future only borrows `self`.
        // whois only needs the peer address, so this backend is
        // transport-agnostic: it resolves identity for direct (non-proxied)
        // connections over either WebTransport or WebSocket from a tailnet
        // IP, ignoring any HTTP headers on the latter.
        let peer_addr = match ctx.transport {
            TransportContext::WebTransport { peer_addr } => peer_addr,
            TransportContext::Http { peer_addr, .. } => peer_addr,
        };
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
        let path_and_query = format!("/localapi/v0/whois?addr={addr}");
        let body = match &self.api {
            LocalApi::Tcp {
                base_url,
                token,
                client,
            } => http_get_tcp(client, base_url, &path_and_query, token).await?,
            LocalApi::Unix { socket_path } => http_get_unix(socket_path, &path_and_query).await?,
        };
        parse_whois_response(&body)
    }
}

async fn http_get_tcp(
    client: &HttpClient,
    base_url: &str,
    path_and_query: &str,
    token: &str,
) -> Result<String, WhoisError> {
    use base64::Engine as _;
    use http_body_util::BodyExt as _;

    let url = format!("{base_url}{path_and_query}");
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!(":{token}"));
    let req = hyper::Request::get(url)
        .header("Authorization", format!("Basic {encoded}"))
        .body(http_body_util::Empty::new())
        .map_err(|e| WhoisError::Unavailable(e.to_string()))?;

    let resp = client
        .request(req)
        .await
        .map_err(|e: hyper_util::client::legacy::Error| WhoisError::Unavailable(e.to_string()))?;

    classify_status(resp.status(), FORBIDDEN_HINT_TCP)?;
    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e: hyper::Error| WhoisError::Unavailable(e.to_string()))?
        .to_bytes();
    String::from_utf8(body.to_vec()).map_err(|e| WhoisError::Unavailable(e.to_string()))
}

async fn http_get_unix(socket: &Path, path_and_query: &str) -> Result<String, WhoisError> {
    use http_body_util::{BodyExt as _, Empty};
    use hyper::client::conn::http1;
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| WhoisError::Unavailable(format!("connecting {}: {e}", socket.display())))?;
    let (mut sender, conn) = http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| WhoisError::Unavailable(format!("http handshake: {e}")))?;

    // Drive the connection in the background. It ends when the response is
    // fully consumed and `sender` is dropped at the end of this function.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!(error = %e, "tailscale UDS conn ended");
        }
    });

    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(path_and_query)
        // tailscaled's LocalAPI validates the Host header against an exact
        // allowlist; "local-tailscaled.sock" is the canonical value. Anything
        // else returns 403 from a Host check that long pre-empts the
        // operator/permission gates, which is misleading.
        .header(hyper::header::HOST, "local-tailscaled.sock")
        .body(Empty::<bytes::Bytes>::new())
        .map_err(|e| WhoisError::Unavailable(e.to_string()))?;

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| WhoisError::Unavailable(e.to_string()))?;
    classify_status(resp.status(), FORBIDDEN_HINT_UNIX)?;

    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| WhoisError::Unavailable(e.to_string()))?
        .to_bytes();
    String::from_utf8(body.to_vec()).map_err(|e| WhoisError::Unavailable(e.to_string()))
}

/// Hint appended to a `403 Forbidden` from the UDS LocalAPI. tailscaled grants
/// `/whois` only to uid 0 or the configured operator, so a non-root cairn-daemon
/// usually needs the operator set explicitly.
const FORBIDDEN_HINT_UNIX: &str = "the user running cairn-daemon needs Tailscale operator permission \
     (run `sudo tailscale set --operator=<user>` on the daemon host)";

/// Hint appended to a `403 Forbidden` from the TCP LocalAPI. The standalone
/// macOS GUI app rejects when the sameuserproof token is stale or unreadable.
const FORBIDDEN_HINT_TCP: &str = "the sameuserproof token may be stale or unreadable \
     (the daemon user typically needs to be in the `admin` group)";

fn classify_status(status: hyper::StatusCode, forbidden_hint: &str) -> Result<(), WhoisError> {
    if status == hyper::StatusCode::NOT_FOUND {
        return Err(WhoisError::NotFound);
    }
    if status == hyper::StatusCode::FORBIDDEN {
        return Err(WhoisError::Forbidden(format!(
            "access denied by tailscaled — {forbidden_hint}"
        )));
    }
    if !status.is_success() {
        return Err(WhoisError::Unavailable(format!(
            "LocalAPI returned {status}"
        )));
    }
    Ok(())
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

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_round_trip_returns_whois_body() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tailscaled.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        // Stand-in tailscaled: accept one connection, drain the request bytes,
        // hand back a canned whois response, then close to signal EOF.
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let body = r#"{"Node":{"ComputedName":"myhost"},"UserProfile":{"LoginName":"user@example.com","DisplayName":"Test User"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let body = http_get_unix(&socket_path, "/localapi/v0/whois?addr=100.64.0.1:12345")
            .await
            .unwrap();
        let info = parse_whois_response(&body).unwrap();
        assert_eq!(info.login, "user@example.com");
        assert_eq!(info.display_name, "Test User");
        assert_eq!(info.node, "myhost");

        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_404_maps_to_not_found() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tailscaled.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
            stream.write_all(resp.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let err = http_get_unix(&socket_path, "/localapi/v0/whois?addr=100.64.0.1:12345")
            .await
            .unwrap_err();
        assert!(matches!(err, WhoisError::NotFound));

        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_403_includes_operator_hint() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        use tokio::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("tailscaled.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
            stream.write_all(resp.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });

        let err = http_get_unix(&socket_path, "/localapi/v0/whois?addr=100.64.0.1:12345")
            .await
            .unwrap_err();
        match err {
            WhoisError::Forbidden(reason) => {
                assert!(
                    reason.contains("tailscale set --operator"),
                    "expected operator hint, got: {reason}"
                );
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }

        server.await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_missing_socket_is_unavailable() {
        let err = http_get_unix(
            std::path::Path::new("/nonexistent/tailscaled.sock"),
            "/localapi/v0/whois?addr=100.64.0.1:12345",
        )
        .await
        .unwrap_err();
        assert!(matches!(err, WhoisError::Unavailable(_)));
    }
}

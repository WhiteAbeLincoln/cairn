//! Daemon endpoint resolution and multi-transport client.
//!
//! Only this file may name `wrpc_transport::*` types. Every other module in
//! `cairn-client` touches the wRPC backend through `Endpoint::client()`'s
//! return value generically — the value is passed straight into the generated
//! `cairn_protocol::client::*` functions, which are themselves
//! `<C: wrpc_transport::Invoke>`. When a second transport (e.g. WebTransport)
//! lands, the only edits are inside this file: add an `Endpoint` variant, add
//! a `Client` enum variant, write the forwarding `Invoke` impl.

use std::path::PathBuf;

use anyhow::{Result, bail};
use wrpc_transport::Invoke;

/// Multi-transport wRPC client.
#[derive(Clone)]
pub enum Client {
    Unix(wrpc_transport::unix::Client<PathBuf>),
    WebTransport(wrpc_transport_web::Client),
}

impl Invoke for Client {
    type Context = ();
    type Outgoing = wrpc_transport::frame::Outgoing;
    type Incoming = wrpc_transport::frame::Incoming;

    async fn invoke<P>(
        &self,
        cx: Self::Context,
        instance: &str,
        func: &str,
        params: bytes::Bytes,
        paths: impl AsRef<[P]> + Send,
    ) -> anyhow::Result<(Self::Outgoing, Self::Incoming)>
    where
        P: AsRef<[Option<usize>]> + Send + Sync,
    {
        match self {
            Self::Unix(c) => c.invoke(cx, instance, func, params, paths).await,
            Self::WebTransport(c) => c.invoke(cx, instance, func, params, paths).await,
        }
    }
}

/// A resolved daemon endpoint. `Unix` is the local unix-socket transport;
/// `WebTransport` is the QUIC-based remote transport.
///
/// WebTransport endpoints use the standard `https://` URL scheme (per
/// W3C WebTransport). Path and query are passed through to the server's
/// H3 `:path`; fragment is rejected as it has no on-wire meaning.
#[derive(Debug)]
pub enum Endpoint {
    Unix(PathBuf),
    WebTransport {
        url: url::Url,
        cert_hash: Option<String>,
    },
}

impl Endpoint {
    /// Resolve from `--daemon` / `CAIRN_DAEMON` (already read by clap) or the
    /// platform default socket. `cert_hash` is used for WebTransport
    /// self-signed certificate pinning.
    pub fn resolve(daemon: Option<&str>, cert_hash: Option<String>) -> Result<Self> {
        match daemon {
            None => Ok(Self::Unix(default_socket())),
            Some(s) => Self::from_uri(s, cert_hash),
        }
    }

    fn from_uri(s: &str, cert_hash: Option<String>) -> Result<Self> {
        // Bare absolute path is a shorthand for unix:// — not a URI form, so
        // it's the one input we can't hand to `url::Url::parse`.
        if s.starts_with('/') {
            return Ok(Self::Unix(PathBuf::from(s)));
        }

        let url = url::Url::parse(s)
            .map_err(|e| anyhow::anyhow!("invalid --daemon endpoint {s:?}: {e}"))?;

        match url.scheme() {
            "unix" => {
                let path = url.path();
                if path.is_empty() {
                    bail!("`--daemon {s}` has no socket path");
                }
                Ok(Self::Unix(PathBuf::from(path)))
            }
            "https" => Self::parse_https(&url, cert_hash),
            "ws" | "wss" => {
                bail!("WebSocket transport is not supported")
            }
            other => bail!(
                "unrecognized --daemon scheme {other:?} in {s:?} (expected `unix://` or `https://`)"
            ),
        }
    }

    fn parse_https(url: &url::Url, cert_hash: Option<String>) -> Result<Self> {
        let host = url
            .host()
            .ok_or_else(|| anyhow::anyhow!("WebTransport endpoint {url} is missing a host"))?;
        if url.port().is_none() {
            bail!("WebTransport endpoint {url} is missing a port");
        }
        if url.fragment().is_some() {
            bail!("WebTransport endpoint {url} must not include a fragment (not sent on the wire)");
        }

        // Auto-load cert hash from file for loopback endpoints. Hostnames
        // other than `localhost` are not resolved here; users pointing at a
        // hostname that happens to resolve to loopback need to pass
        // `--cert-hash` explicitly.
        let is_loopback = match host {
            url::Host::Domain(d) => d.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        };
        let cert_hash = cert_hash.or_else(|| {
            if is_loopback {
                let hash_path = runtime_dir().join("cert-hash");
                std::fs::read_to_string(&hash_path)
                    .ok()
                    .map(|s| s.trim().to_string())
            } else {
                None
            }
        });

        Ok(Self::WebTransport {
            url: url.clone(),
            cert_hash,
        })
    }

    /// Human-readable label used in error messages. Avoids leaking
    /// transport-specific accessors (a future `Wt` variant has no `Path`).
    pub fn label(&self) -> String {
        match self {
            Self::Unix(p) => format!("unix://{}", p.display()),
            Self::WebTransport { url, .. } => url.to_string(),
        }
    }

    /// True if the endpoint's underlying resource is known-gone without even
    /// attempting a connection. For the Unix transport this is "the socket
    /// path no longer exists" — used by the reconnect loop to give up
    /// immediately when the daemon has been torn down. WebTransport always
    /// returns `false` (you only learn it's gone by failing to connect).
    pub fn is_gone(&self) -> bool {
        match self {
            Self::Unix(p) => !p.exists(),
            Self::WebTransport { .. } => false,
        }
    }

    /// Build a wRPC client for this endpoint. For UDS this is cheap (just
    /// stores the path). For WebTransport this opens a QUIC connection.
    pub async fn client(&self) -> Result<Client> {
        match self {
            Self::Unix(p) => Ok(Client::Unix(wrpc_transport::unix::Client::from(p.clone()))),
            Self::WebTransport { url, cert_hash } => {
                let config = build_wt_client_config(url.as_str(), cert_hash.as_deref())?;
                let endpoint = wtransport::Endpoint::client(config)
                    .map_err(|e| anyhow::anyhow!("creating WT endpoint: {e}"))?;
                let conn = endpoint
                    .connect(url.as_str())
                    .await
                    .map_err(|e| anyhow::anyhow!("WebTransport connect to {url}: {e}"))?;
                Ok(Client::WebTransport(wrpc_transport_web::Client::from(conn)))
            }
        }
    }
}

fn build_wt_client_config(
    _host: &str,
    cert_hash: Option<&str>,
) -> Result<wtransport::ClientConfig> {
    use wtransport::ClientConfig;

    let builder = ClientConfig::builder().with_bind_default();

    let config = if let Some(hash_hex) = cert_hash {
        let hash_bytes: [u8; 32] = hex_decode(hash_hex)?;
        builder
            .with_server_certificate_hashes(vec![wtransport::tls::Sha256Digest::new(hash_bytes)])
            .build()
    } else {
        builder.with_native_certs().build()
    };

    Ok(config)
}

fn hex_decode(hex: &str) -> Result<[u8; 32]> {
    let hex = hex.trim();
    if hex.len() != 64 {
        bail!(
            "cert-hash must be 64 hex chars (32 bytes), got {}",
            hex.len()
        );
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|e| anyhow::anyhow!("invalid cert-hash hex at position {}: {e}", i * 2))?;
    }
    Ok(bytes)
}

/// `$XDG_RUNTIME_DIR/cairn`, else `$TMPDIR/cairn`, else `/tmp/cairn` —
/// identical to the daemon's runtime directory.
fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn")
}

/// `$XDG_RUNTIME_DIR/cairn/cairn.sock`, else `$TMPDIR/cairn/cairn.sock`, else
/// `/tmp/cairn/cairn.sock` — identical to the daemon's default.
fn default_socket() -> PathBuf {
    runtime_dir().join("cairn.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_resolves_to_unix_with_cairn_sock_suffix() {
        match Endpoint::resolve(None, None).unwrap() {
            Endpoint::Unix(p) => assert!(p.ends_with("cairn/cairn.sock"), "got {p:?}"),
            _ => panic!("expected Unix"),
        }
    }

    #[test]
    fn unix_uri_yields_its_path() {
        match Endpoint::resolve(Some("unix:///run/cairn/x.sock"), None).unwrap() {
            Endpoint::Unix(p) => assert_eq!(p, Path::new("/run/cairn/x.sock")),
            _ => panic!("expected Unix"),
        }
    }

    #[test]
    fn bare_absolute_path_is_accepted() {
        match Endpoint::resolve(Some("/tmp/y.sock"), None).unwrap() {
            Endpoint::Unix(p) => assert_eq!(p, Path::new("/tmp/y.sock")),
            _ => panic!("expected Unix"),
        }
    }

    #[test]
    fn websocket_endpoints_are_rejected() {
        let err = Endpoint::resolve(Some("wss://host:443"), None).unwrap_err();
        assert!(err.to_string().contains("not supported"), "got {err}");
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!(Endpoint::resolve(Some("http://host"), None).is_err());
    }

    #[test]
    fn label_renders_unix_uri() {
        let ep = Endpoint::resolve(Some("/tmp/y.sock"), None).unwrap();
        assert_eq!(ep.label(), "unix:///tmp/y.sock");
    }

    #[test]
    fn https_uri_yields_webtransport_endpoint() {
        let ep = Endpoint::resolve(Some("https://192.168.1.10:4433"), None).unwrap();
        assert!(matches!(ep, Endpoint::WebTransport { .. }));
        assert_eq!(ep.label(), "https://192.168.1.10:4433/");
    }

    #[test]
    fn https_uri_accepts_hostname() {
        let ep = Endpoint::resolve(Some("https://myhost.ts.net:4433"), None).unwrap();
        assert!(matches!(ep, Endpoint::WebTransport { .. }));
        assert_eq!(ep.label(), "https://myhost.ts.net:4433/");
    }

    #[test]
    fn https_uri_accepts_bracketed_ipv6() {
        let ep = Endpoint::resolve(Some("https://[2001:db8::1]:4433"), None).unwrap();
        assert_eq!(ep.label(), "https://[2001:db8::1]:4433/");
    }

    #[test]
    fn https_uri_accepts_path() {
        // https://example.com:4999/wt is the canonical WebTransport example
        // from the W3C spec. The path is sent as the H3 :path pseudo-header.
        let ep = Endpoint::resolve(Some("https://example.com:4999/wt"), None).unwrap();
        assert_eq!(ep.label(), "https://example.com:4999/wt");
    }

    #[test]
    fn https_uri_rejects_missing_port() {
        assert!(Endpoint::resolve(Some("https://myhost.ts.net"), None).is_err());
    }

    #[test]
    fn https_uri_rejects_fragment() {
        assert!(Endpoint::resolve(Some("https://myhost.ts.net:4433#x"), None).is_err());
    }

    #[test]
    fn https_is_gone_always_returns_false() {
        let ep = Endpoint::resolve(Some("https://192.168.1.10:4433"), None).unwrap();
        assert!(!ep.is_gone());
    }

    #[test]
    fn https_with_cert_hash() {
        let hash = "a".repeat(64);
        let ep = Endpoint::resolve(Some("https://10.0.0.1:4433"), Some(hash.clone())).unwrap();
        match ep {
            Endpoint::WebTransport { cert_hash, .. } => assert_eq!(cert_hash, Some(hash)),
            _ => panic!("expected WebTransport"),
        }
    }

    #[test]
    fn hex_decode_valid() {
        let hex = "aa".repeat(32);
        let bytes = hex_decode(&hex).unwrap();
        assert!(bytes.iter().all(|&b| b == 0xaa));
    }

    #[test]
    fn hex_decode_wrong_length() {
        assert!(hex_decode("aabb").is_err());
    }
}

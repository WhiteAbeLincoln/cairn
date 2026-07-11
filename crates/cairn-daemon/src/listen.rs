//! Listener configuration parsed from `--listen` URIs.

use std::net::SocketAddr;
use std::path::PathBuf;

/// A resolved listener endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerConfig {
    Unix(PathBuf),
    WebTransport(SocketAddr),
    WebSocket(SocketAddr),
}

/// Parse a `--listen` value into one or more [`ListenerConfig`]s.
///
/// Accepted forms:
/// - `unix` — use the default socket path
/// - `unix:///absolute/path` — explicit UDS path
/// - `https://host:port` — WebTransport listener (the W3C
///   WebTransport URL scheme)
/// - `ws://host:port` — WebSocket listener (the browser-facing
///   HTTP/WebSocket transport; `wss://` TLS termination is handled
///   out of band, e.g. by `tailscale serve`)
/// - `/absolute/path` — bare path, treated as `unix://`
///
/// When the host is `localhost`, the listener expands to both
/// `127.0.0.1` and `[::1]` (dual-stack loopback). Explicit IP
/// addresses bind to exactly what was requested.
pub fn parse_listener(s: &str) -> anyhow::Result<Vec<ListenerConfig>> {
    if s == "unix" {
        return Ok(vec![ListenerConfig::Unix(
            crate::config::default_socket_path(),
        )]);
    }
    if s.starts_with('/') {
        return Ok(vec![ListenerConfig::Unix(PathBuf::from(s))]);
    }

    let url =
        url::Url::parse(s).map_err(|e| anyhow::anyhow!("invalid --listen value {s:?}: {e}"))?;

    match url.scheme() {
        "unix" => {
            let path = url.path();
            if path.is_empty() {
                anyhow::bail!("`--listen {s}` requires a socket path");
            }
            Ok(vec![ListenerConfig::Unix(PathBuf::from(path))])
        }
        "https" => Ok(parse_host_port(&url, s)?
            .into_iter()
            .map(ListenerConfig::WebTransport)
            .collect()),
        "ws" => Ok(parse_host_port(&url, s)?
            .into_iter()
            .map(ListenerConfig::WebSocket)
            .collect()),
        other => anyhow::bail!(
            "unrecognized --listen scheme {other:?} in {s:?}; expected `unix`, `unix:///path`, `https://host:port`, or `ws://host:port`"
        ),
    }
}

/// Resolve the `host:port` authority of a network `--listen` URL into one or
/// more socket addresses. `localhost` expands to dual-stack loopback
/// (`127.0.0.1` and `[::1]`); explicit IP literals bind exactly what was asked.
fn parse_host_port(url: &url::Url, s: &str) -> anyhow::Result<Vec<SocketAddr>> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("--listen {s:?} is missing a host"))?;
    let port = url
        .port()
        .ok_or_else(|| anyhow::anyhow!("--listen {s:?} is missing a port"))?;

    if host == "localhost" {
        let v4: SocketAddr = ([127, 0, 0, 1], port).into();
        let v6: SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();
        Ok(vec![v4, v6])
    } else {
        let addr: SocketAddr = format!("{host}:{port}").parse().map_err(|e| {
            anyhow::anyhow!("--listen {s:?} must be IP:port or localhost:port: {e}")
        })?;
        Ok(vec![addr])
    }
}

impl ListenerConfig {
    pub fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }
    pub fn is_wt(&self) -> bool {
        matches!(self, Self::WebTransport(_))
    }
    pub fn is_ws(&self) -> bool {
        matches!(self, Self::WebSocket(_))
    }
    pub fn is_loopback(&self) -> bool {
        match self {
            Self::Unix(_) => true,
            Self::WebTransport(addr) | Self::WebSocket(addr) => addr.ip().is_loopback(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_unix_uses_default_path() {
        let cfgs = parse_listener("unix").unwrap();
        assert_eq!(cfgs.len(), 1);
        assert!(cfgs[0].is_unix());
        if let ListenerConfig::Unix(p) = &cfgs[0] {
            assert!(p.ends_with("cairn/cairn.sock"));
        }
    }

    #[test]
    fn unix_uri_extracts_path() {
        let cfgs = parse_listener("unix:///tmp/my.sock").unwrap();
        assert_eq!(
            cfgs,
            vec![ListenerConfig::Unix(PathBuf::from("/tmp/my.sock"))]
        );
    }

    #[test]
    fn https_uri_extracts_socket_addr() {
        let cfgs = parse_listener("https://127.0.0.1:9443").unwrap();
        assert_eq!(
            cfgs,
            vec![ListenerConfig::WebTransport(
                "127.0.0.1:9443".parse().unwrap()
            )]
        );
    }

    #[test]
    fn localhost_expands_to_dual_stack() {
        let cfgs = parse_listener("https://localhost:4433").unwrap();
        assert_eq!(cfgs.len(), 2);
        assert_eq!(
            cfgs[0],
            ListenerConfig::WebTransport(([127, 0, 0, 1], 4433).into())
        );
        assert_eq!(
            cfgs[1],
            ListenerConfig::WebTransport(([0, 0, 0, 0, 0, 0, 0, 1], 4433).into())
        );
        assert!(cfgs.iter().all(|c| c.is_loopback()));
    }

    #[test]
    fn ws_uri_extracts_socket_addr() {
        let cfgs = parse_listener("ws://127.0.0.1:8080").unwrap();
        assert_eq!(
            cfgs,
            vec![ListenerConfig::WebSocket("127.0.0.1:8080".parse().unwrap())]
        );
        assert!(cfgs[0].is_ws());
    }

    #[test]
    fn ws_localhost_expands_to_dual_stack() {
        let cfgs = parse_listener("ws://localhost:8080").unwrap();
        assert_eq!(
            cfgs,
            vec![
                ListenerConfig::WebSocket(([127, 0, 0, 1], 8080).into()),
                ListenerConfig::WebSocket(([0, 0, 0, 0, 0, 0, 0, 1], 8080).into()),
            ]
        );
        assert!(cfgs.iter().all(|c| c.is_loopback()));
    }

    #[test]
    fn ws_hostname_rejected() {
        // Arbitrary hostnames (not localhost) still require an IP literal.
        assert!(parse_listener("ws://myhost.example:8080").is_err());
    }

    #[test]
    fn wss_scheme_rejected() {
        // TLS is terminated out of band; the daemon only speaks plaintext `ws`.
        assert!(parse_listener("wss://localhost:8080").is_err());
    }

    #[test]
    fn bare_path_is_unix() {
        let cfgs = parse_listener("/var/run/cairn.sock").unwrap();
        assert_eq!(
            cfgs,
            vec![ListenerConfig::Unix(PathBuf::from("/var/run/cairn.sock"))]
        );
    }

    #[test]
    fn empty_unix_uri_rejected() {
        assert!(parse_listener("unix://").is_err());
    }

    #[test]
    fn invalid_https_addr_rejected() {
        assert!(parse_listener("https://not-an-addr").is_err());
    }

    #[test]
    fn https_hostname_rejected() {
        // Arbitrary hostnames (not localhost) still require an IP literal.
        assert!(parse_listener("https://myhost.example:9443").is_err());
    }

    #[test]
    fn unknown_scheme_rejected() {
        assert!(parse_listener("tcp://localhost:1234").is_err());
    }
}

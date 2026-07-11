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
        Ok(dual_stack_loopback(port))
    } else {
        let addr: SocketAddr = format!("{host}:{port}").parse().map_err(|e| {
            anyhow::anyhow!("--listen {s:?} must be IP:port or localhost:port: {e}")
        })?;
        Ok(vec![addr])
    }
}

/// Parse a bare `host:port` string (no URL scheme) into one or more socket
/// addresses — used by `--web-ui=host:port`, which names a dedicated HTTP
/// listener rather than a `--listen` transport. Applies the same `localhost`
/// dual-stack expansion as `--listen ws://`/`https://` for consistency.
pub fn parse_addr_spec(s: &str) -> anyhow::Result<Vec<SocketAddr>> {
    if let Ok(addr) = s.parse::<SocketAddr>() {
        return Ok(vec![addr]);
    }
    let (host, port) = s
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("{s:?} must be HOST:PORT"))?;
    if host != "localhost" {
        anyhow::bail!("{s:?} must be IP:port or localhost:port");
    }
    let port: u16 = port
        .parse()
        .map_err(|e| anyhow::anyhow!("{s:?} has an invalid port: {e}"))?;
    Ok(dual_stack_loopback(port))
}

/// The two loopback addresses `localhost` expands to for a given port.
fn dual_stack_loopback(port: u16) -> Vec<SocketAddr> {
    vec![
        ([127, 0, 0, 1], port).into(),
        ([0, 0, 0, 0, 0, 0, 0, 1], port).into(),
    ]
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

    // ── --web-ui=host:port target parsing ────────────────────────────────

    #[test]
    fn addr_spec_accepts_ip_port() {
        assert_eq!(
            parse_addr_spec("127.0.0.1:5173").unwrap(),
            vec!["127.0.0.1:5173".parse().unwrap()]
        );
    }

    #[test]
    fn addr_spec_accepts_ipv6_literal() {
        assert_eq!(
            parse_addr_spec("[::1]:5173").unwrap(),
            vec!["[::1]:5173".parse().unwrap()]
        );
    }

    #[test]
    fn addr_spec_localhost_expands_to_dual_stack() {
        let addrs = parse_addr_spec("localhost:5173").unwrap();
        assert_eq!(
            addrs,
            vec![
                ([127, 0, 0, 1], 5173).into(),
                ([0, 0, 0, 0, 0, 0, 0, 1], 5173).into(),
            ]
        );
    }

    #[test]
    fn addr_spec_rejects_arbitrary_hostname() {
        assert!(parse_addr_spec("myhost.example:5173").is_err());
    }

    #[test]
    fn addr_spec_rejects_missing_port() {
        assert!(parse_addr_spec("127.0.0.1").is_err());
    }

    #[test]
    fn addr_spec_rejects_invalid_port() {
        assert!(parse_addr_spec("127.0.0.1:notaport").is_err());
    }
}

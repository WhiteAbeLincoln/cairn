//! Listener configuration parsed from `--listen` URIs.

use std::net::SocketAddr;
use std::path::PathBuf;

/// A resolved listener endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerConfig {
    Unix(PathBuf),
    WebTransport(SocketAddr),
}

/// Parse a `--listen` value into one or more [`ListenerConfig`]s.
///
/// Accepted forms:
/// - `unix` — use the default socket path
/// - `unix:///absolute/path` — explicit UDS path
/// - `https://host:port` — WebTransport listener (the W3C
///   WebTransport URL scheme; a future WebSocket listener would
///   use `wss://`)
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
        "https" => {
            let host = url
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("--listen {s:?} is missing a host"))?;
            let port = url
                .port()
                .ok_or_else(|| anyhow::anyhow!("--listen {s:?} is missing a port"))?;

            if host == "localhost" {
                let v4: SocketAddr = ([127, 0, 0, 1], port).into();
                let v6: SocketAddr = ([0, 0, 0, 0, 0, 0, 0, 1], port).into();
                Ok(vec![
                    ListenerConfig::WebTransport(v4),
                    ListenerConfig::WebTransport(v6),
                ])
            } else {
                let addr: SocketAddr = format!("{host}:{port}").parse().map_err(|e| {
                    anyhow::anyhow!("--listen {s:?} must be IP:port or localhost:port: {e}")
                })?;
                Ok(vec![ListenerConfig::WebTransport(addr)])
            }
        }
        other => anyhow::bail!(
            "unrecognized --listen scheme {other:?} in {s:?}; expected `unix`, `unix:///path`, or `https://host:port`"
        ),
    }
}

impl ListenerConfig {
    pub fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }
    pub fn is_wt(&self) -> bool {
        matches!(self, Self::WebTransport(_))
    }
    pub fn is_loopback(&self) -> bool {
        match self {
            Self::Unix(_) => true,
            Self::WebTransport(addr) => addr.ip().is_loopback(),
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

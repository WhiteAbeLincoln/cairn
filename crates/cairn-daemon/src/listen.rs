//! Listener configuration parsed from `--listen` URIs.

use std::net::SocketAddr;
use std::path::PathBuf;

/// A resolved listener endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListenerConfig {
    Unix(PathBuf),
    WebTransport(SocketAddr),
}

/// Parse a `--listen` value into a [`ListenerConfig`].
///
/// Accepted forms:
/// - `unix` — use the default socket path
/// - `unix:///absolute/path` — explicit UDS path
/// - `https://host:port` — WebTransport listener (the W3C
///   WebTransport URL scheme; a future WebSocket listener would
///   use `wss://`)
/// - `/absolute/path` — bare path, treated as `unix://`
pub fn parse_listener(s: &str) -> anyhow::Result<ListenerConfig> {
    if s == "unix" {
        return Ok(ListenerConfig::Unix(crate::config::default_socket_path()));
    }
    if s.starts_with('/') {
        return Ok(ListenerConfig::Unix(PathBuf::from(s)));
    }

    let url =
        url::Url::parse(s).map_err(|e| anyhow::anyhow!("invalid --listen value {s:?}: {e}"))?;

    match url.scheme() {
        "unix" => {
            let path = url.path();
            if path.is_empty() {
                anyhow::bail!("`--listen {s}` requires a socket path");
            }
            Ok(ListenerConfig::Unix(PathBuf::from(path)))
        }
        "https" => {
            // WebTransport listeners bind to a socket; the URL form is
            // only used here as a transport selector. A `host:port` with
            // a hostname requires DNS to bind, which we don't do — IP
            // literals only.
            let host = url
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("--listen {s:?} is missing a host"))?;
            let port = url
                .port()
                .ok_or_else(|| anyhow::anyhow!("--listen {s:?} is missing a port"))?;
            let addr: SocketAddr = format!("{host}:{port}")
                .parse()
                .map_err(|e| anyhow::anyhow!("--listen {s:?} must be IP:port (no DNS): {e}"))?;
            Ok(ListenerConfig::WebTransport(addr))
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
        let l = parse_listener("unix").unwrap();
        assert!(l.is_unix());
        if let ListenerConfig::Unix(p) = l {
            assert!(p.ends_with("cairn/cairn.sock"));
        }
    }

    #[test]
    fn unix_uri_extracts_path() {
        let l = parse_listener("unix:///tmp/my.sock").unwrap();
        assert_eq!(l, ListenerConfig::Unix(PathBuf::from("/tmp/my.sock")));
    }

    #[test]
    fn https_uri_extracts_socket_addr() {
        let l = parse_listener("https://127.0.0.1:9443").unwrap();
        assert_eq!(
            l,
            ListenerConfig::WebTransport("127.0.0.1:9443".parse().unwrap())
        );
    }

    #[test]
    fn bare_path_is_unix() {
        let l = parse_listener("/var/run/cairn.sock").unwrap();
        assert_eq!(
            l,
            ListenerConfig::Unix(PathBuf::from("/var/run/cairn.sock"))
        );
    }

    #[test]
    fn empty_unix_uri_rejected() {
        assert!(parse_listener("unix://").is_err());
    }

    #[test]
    fn invalid_https_addr_rejected() {
        // Hostnames (no DNS-at-bind-time) and bare paths are not valid.
        assert!(parse_listener("https://not-an-addr").is_err());
    }

    #[test]
    fn https_hostname_rejected() {
        // The daemon binds to a socket; hostnames would need DNS, which
        // we don't do at listen time.
        assert!(parse_listener("https://myhost.example:9443").is_err());
    }

    #[test]
    fn unknown_scheme_rejected() {
        assert!(parse_listener("tcp://localhost:1234").is_err());
    }
}

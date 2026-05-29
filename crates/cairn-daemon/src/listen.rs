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
/// - `wt://host:port` — WebTransport listener
/// - `/absolute/path` — bare path, treated as `unix://`
pub fn parse_listener(s: &str) -> anyhow::Result<ListenerConfig> {
    if s == "unix" {
        return Ok(ListenerConfig::Unix(crate::config::default_socket_path()));
    }
    if let Some(rest) = s.strip_prefix("unix://") {
        if rest.is_empty() {
            anyhow::bail!("`--listen unix://` requires a socket path");
        }
        return Ok(ListenerConfig::Unix(PathBuf::from(rest)));
    }
    if let Some(rest) = s.strip_prefix("wt://") {
        let addr: SocketAddr = rest
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid wt:// address {rest:?}: {e}"))?;
        return Ok(ListenerConfig::WebTransport(addr));
    }
    if s.starts_with('/') {
        return Ok(ListenerConfig::Unix(PathBuf::from(s)));
    }
    anyhow::bail!(
        "unrecognized --listen value {s:?}; expected `unix`, `unix:///path`, or `wt://host:port`"
    )
}

impl ListenerConfig {
    pub fn is_unix(&self) -> bool {
        matches!(self, Self::Unix(_))
    }
    pub fn is_wt(&self) -> bool {
        matches!(self, Self::WebTransport(_))
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
    fn wt_uri_extracts_socket_addr() {
        let l = parse_listener("wt://127.0.0.1:9443").unwrap();
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
    fn invalid_wt_addr_rejected() {
        assert!(parse_listener("wt://not-an-addr").is_err());
    }

    #[test]
    fn unknown_scheme_rejected() {
        assert!(parse_listener("tcp://localhost:1234").is_err());
    }
}

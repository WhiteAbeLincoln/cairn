//! Daemon endpoint resolution. v0 supports only the unix-socket transport.
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

/// The wRPC client type for the local unix-socket transport. Cheap to clone
/// (holds only the socket path); each invocation opens a fresh connection.
///
/// Today's `Client` is just the UDS client. When WebTransport lands, this
/// alias becomes an enum wrapper with a forwarding `wrpc_transport::Invoke`
/// impl — the public `Endpoint::client()` API stays unchanged.
pub type Client = wrpc_transport::unix::Client<PathBuf>;

/// A resolved daemon endpoint. v0 has only the `Unix` variant; future
/// transports add variants alongside it.
#[derive(Debug)]
pub enum Endpoint {
    Unix(PathBuf),
}

impl Endpoint {
    /// Resolve from `--daemon` / `CAIRN_DAEMON` (already read by clap) or the
    /// platform default socket.
    pub fn resolve(daemon: Option<&str>) -> Result<Self> {
        match daemon {
            None => Ok(Self::Unix(default_socket())),
            Some(s) => Self::from_uri(s),
        }
    }

    fn from_uri(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix("unix://") {
            if rest.is_empty() {
                bail!("`--daemon unix://` has no socket path");
            }
            return Ok(Self::Unix(PathBuf::from(rest)));
        }
        if s.starts_with('/') {
            return Ok(Self::Unix(PathBuf::from(s)));
        }
        if s.starts_with("ws://") || s.starts_with("wss://") {
            bail!("remote transports (WebTransport) are not yet supported; v0 is unix-socket only");
        }
        bail!("unrecognized --daemon endpoint {s:?} (expected `unix:///path/to/cairn.sock`)");
    }

    /// Human-readable label used in error messages. Avoids leaking
    /// transport-specific accessors (a future `Wt` variant has no `Path`).
    pub fn label(&self) -> String {
        match self {
            Self::Unix(p) => format!("unix://{}", p.display()),
        }
    }

    pub fn client(&self) -> Client {
        match self {
            Self::Unix(p) => wrpc_transport::unix::Client::from(p.clone()),
        }
    }
}

/// `$XDG_RUNTIME_DIR/cairn/cairn.sock`, else `$TMPDIR/cairn/cairn.sock`, else
/// `/tmp/cairn/cairn.sock` — identical to the daemon's default.
fn default_socket() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn").join("cairn.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_resolves_to_unix_with_cairn_sock_suffix() {
        match Endpoint::resolve(None).unwrap() {
            Endpoint::Unix(p) => assert!(p.ends_with("cairn/cairn.sock"), "got {p:?}"),
        }
    }

    #[test]
    fn unix_uri_yields_its_path() {
        match Endpoint::resolve(Some("unix:///run/cairn/x.sock")).unwrap() {
            Endpoint::Unix(p) => assert_eq!(p, Path::new("/run/cairn/x.sock")),
        }
    }

    #[test]
    fn bare_absolute_path_is_accepted() {
        match Endpoint::resolve(Some("/tmp/y.sock")).unwrap() {
            Endpoint::Unix(p) => assert_eq!(p, Path::new("/tmp/y.sock")),
        }
    }

    #[test]
    fn websocket_endpoints_are_rejected() {
        let err = Endpoint::resolve(Some("wss://host:443")).unwrap_err();
        assert!(err.to_string().contains("not yet supported"), "got {err}");
    }

    #[test]
    fn unknown_scheme_is_rejected() {
        assert!(Endpoint::resolve(Some("http://host")).is_err());
    }

    #[test]
    fn label_renders_unix_uri() {
        let ep = Endpoint::resolve(Some("/tmp/y.sock")).unwrap();
        assert_eq!(ep.label(), "unix:///tmp/y.sock");
    }
}

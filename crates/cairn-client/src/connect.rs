//! Daemon endpoint resolution. v0 supports only the unix-socket transport.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

/// The wRPC client type for the local unix-socket transport. Cheap to clone
/// (holds only the socket path); each invocation opens a fresh connection.
pub type Client = wrpc_transport::unix::Client<PathBuf>;

/// A resolved daemon endpoint.
#[derive(Debug)]
pub struct Endpoint {
    path: PathBuf,
}

impl Endpoint {
    /// Resolve from `--daemon` / `CAIRN_DAEMON` (already read by clap) or the
    /// platform default socket.
    pub fn resolve(daemon: Option<&str>) -> Result<Self> {
        match daemon {
            None => Ok(Self { path: default_socket() }),
            Some(s) => Self::from_uri(s),
        }
    }

    fn from_uri(s: &str) -> Result<Self> {
        if let Some(rest) = s.strip_prefix("unix://") {
            if rest.is_empty() {
                bail!("`--daemon unix://` has no socket path");
            }
            return Ok(Self { path: PathBuf::from(rest) });
        }
        if s.starts_with('/') {
            return Ok(Self { path: PathBuf::from(s) });
        }
        if s.starts_with("ws://") || s.starts_with("wss://") {
            bail!("remote transports (WebTransport) are not yet supported; v0 is unix-socket only");
        }
        bail!("unrecognized --daemon endpoint {s:?} (expected `unix:///path/to/cairn.sock`)");
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn client(&self) -> Client {
        wrpc_transport::unix::Client::from(self.path.clone())
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

    #[test]
    fn default_socket_ends_with_cairn_sock() {
        let ep = Endpoint::resolve(None).unwrap();
        assert!(ep.path().ends_with("cairn/cairn.sock"), "got {:?}", ep.path());
    }

    #[test]
    fn unix_uri_yields_its_path() {
        let ep = Endpoint::resolve(Some("unix:///run/cairn/x.sock")).unwrap();
        assert_eq!(ep.path(), Path::new("/run/cairn/x.sock"));
    }

    #[test]
    fn bare_absolute_path_is_accepted() {
        let ep = Endpoint::resolve(Some("/tmp/y.sock")).unwrap();
        assert_eq!(ep.path(), Path::new("/tmp/y.sock"));
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
}

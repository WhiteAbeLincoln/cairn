//! Meta-interface handlers: `version`, `whoami`, `authenticate`.

use cairn_protocol::cairn::daemon::types::Error as WireError;
use cairn_protocol::exports::cairn::daemon::meta::VersionInfo;

use crate::serve::ConnCtx;

/// Return the daemon build version and protocol identifier.
pub fn version() -> VersionInfo {
    VersionInfo {
        daemon: concat!("cairn-daemon/", env!("CARGO_PKG_VERSION")).to_string(),
        protocol: "cairn:daemon@0.1.0".to_string(),
    }
}

/// UDS is pre-authenticated by the kernel; first-message auth is a WebTransport
/// concern. Accept any token unconditionally on this transport.
pub fn authenticate(_token: String) -> Result<(), WireError> {
    Ok(())
}

/// The peer uid (resolved to a username when possible), from `SO_PEERCRED`.
pub fn whoami(ctx: &ConnCtx) -> Result<String, WireError> {
    let uid = ctx.peer.map(|c| c.uid());
    Ok(match uid {
        Some(uid) => username_for(uid).unwrap_or_else(|| uid.to_string()),
        None => "unknown".to_string(),
    })
}

/// Attempt to resolve a uid to a login name via the local passwd database.
/// Returns `None` on any failure — callers fall back to the numeric uid string.
///
/// v0: not implemented; richer lookup (getpwuid_r) can be added later without
/// changing the interface.
fn username_for(_uid: u32) -> Option<String> {
    None
}

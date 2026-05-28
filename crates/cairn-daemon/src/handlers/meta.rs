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

/// The peer identity from `SO_PEERCRED`. v0 reports the numeric uid (see
/// [`username_for`] for the deferred name-resolution hook).
pub fn whoami(ctx: &ConnCtx) -> Result<String, WireError> {
    let uid = ctx.peer.map(|c| c.uid());
    Ok(match uid {
        Some(uid) => username_for(uid).unwrap_or_else(|| uid.to_string()),
        None => "unknown".to_string(),
    })
}

fn username_for(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}

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

/// UDS is pre-authenticated by the kernel by user id. Network transports can
/// currently authenticate at connection setup; first-message authentication
/// (for example JWT tokens) is not implemented yet.
pub fn authenticate(ctx: &ConnCtx, _token: String) -> Result<(), WireError> {
    if matches!(ctx.identity, crate::identity::Identity::Unix { .. }) {
        return Ok(());
    }

    Err(WireError {
        code: "unimplemented".to_string(),
        message: "first-message authentication is not implemented yet".to_string(),
    })
}

pub fn whoami(ctx: &ConnCtx) -> Result<String, WireError> {
    use crate::identity::Identity;
    let name = match &ctx.identity {
        Identity::Unix { uid, username } => username
            .clone()
            .unwrap_or_else(|| username_for(*uid).unwrap_or_else(|| uid.to_string())),
        other => {
            let dn = other.display_name();
            if dn.is_empty() {
                "unknown".to_string()
            } else {
                dn.to_string()
            }
        }
    };
    Ok(name)
}

fn username_for(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|u| u.name)
}

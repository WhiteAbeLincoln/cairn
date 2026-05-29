//! Transport-agnostic caller identity.

/// The resolved identity of a connected client, produced by the transport
/// layer (UDS peer creds) or an auth backend (Tailscale, JWT, SSH key).
#[derive(Clone, Debug)]
pub enum Identity {
    /// Unix peer credentials from `SO_PEERCRED`.
    Unix { uid: u32, username: Option<String> },
    /// Tailscale-resolved identity via LocalAPI `whois`.
    Tailscale {
        login: String,
        display_name: String,
        node: String,
    },
    /// JWT-authenticated identity (v1).
    Token { subject: String },
    /// SSH-key-authenticated identity (v1).
    SshKey {
        fingerprint: String,
        comment: Option<String>,
    },
    /// No authentication (loopback-only or development).
    Anonymous,
}

impl Identity {
    /// Human-readable label returned by `whoami`.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Unix {
                username: Some(name),
                ..
            } => name,
            Self::Unix { .. } => "",
            Self::Tailscale { display_name, .. } => display_name,
            Self::Token { subject } => subject,
            Self::SshKey { fingerprint, .. } => fingerprint,
            Self::Anonymous => "anonymous",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_display_name_prefers_username() {
        let id = Identity::Unix {
            uid: 501,
            username: Some("abe".into()),
        };
        assert_eq!(id.display_name(), "abe");
    }

    #[test]
    fn unix_display_name_fallback_for_uid_only() {
        let id = Identity::Unix {
            uid: 501,
            username: None,
        };
        assert_eq!(id.display_name(), "");
    }

    #[test]
    fn tailscale_display_name() {
        let id = Identity::Tailscale {
            login: "user@example.com".into(),
            display_name: "User".into(),
            node: "myhost".into(),
        };
        assert_eq!(id.display_name(), "User");
    }

    #[test]
    fn anonymous_display_name() {
        assert_eq!(Identity::Anonymous.display_name(), "anonymous");
    }
}

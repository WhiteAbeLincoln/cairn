//! Mapping from internal errors to the wire `types::error` envelope, with
//! machine-stable `code` strings the CLI can branch on.

use cairn_protocol::cairn::daemon::types::Error as WireError;
use cairn_pty::PtyError;

/// Daemon-level (non-PtyError) failures.
#[derive(Debug, Clone, Copy)]
pub enum DaemonError {
    NotFound,
    NameInUse,
    Running,
    SpawnFailed,
    InvalidSignal,
}

impl DaemonError {
    pub fn to_wire(self) -> WireError {
        let (code, message) = match self {
            DaemonError::NotFound => ("session.not_found", "no such session"),
            DaemonError::NameInUse => (
                "session.name_in_use",
                "a live session already has that name",
            ),
            DaemonError::Running => ("session.running", "session is still running (use --force)"),
            DaemonError::SpawnFailed => ("session.spawn_failed", "failed to spawn the session"),
            DaemonError::InvalidSignal => ("signal.invalid", "unknown or out-of-range signal"),
        };
        WireError {
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

/// Map a `PtyError` to the wire envelope.
pub fn to_wire(err: PtyError) -> WireError {
    let (code, message) = match &err {
        PtyError::Closed => ("session.closed", "session has exited".to_string()),
        PtyError::NotLeader { .. } => ("resize.not_leader", err.to_string()),
        PtyError::Io { .. } => ("pty.io", err.to_string()),
        PtyError::Backend { .. } => ("pty.backend", err.to_string()),
    };
    WireError {
        code: code.to_string(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_pty::{ClientId, PtyError};

    #[test]
    fn pty_errors_map_to_stable_codes() {
        assert_eq!(to_wire(PtyError::Closed).code, "session.closed");
        assert_eq!(
            to_wire(PtyError::NotLeader {
                requester: ClientId::from_u64(0),
                current: None
            })
            .code,
            "resize.not_leader"
        );
    }

    #[test]
    fn daemon_errors_map_to_stable_codes() {
        assert_eq!(DaemonError::NotFound.to_wire().code, "session.not_found");
        assert_eq!(DaemonError::NameInUse.to_wire().code, "session.name_in_use");
        assert_eq!(DaemonError::Running.to_wire().code, "session.running");
        assert_eq!(DaemonError::InvalidSignal.to_wire().code, "signal.invalid");
    }
}

use snafu::Snafu;

/// Errors surfaced by a [`crate::pty::PtySession`].
///
/// `Backend` is an opaque escape hatch for implementor-specific errors
/// (e.g. libghostty-vt's `error::Error`). Callers handle generically;
/// advanced consumers can downcast via the inner trait object.
#[derive(Debug, Snafu)]
pub enum PtyError {
    #[snafu(display("pty session has exited"))]
    Closed,

    #[snafu(display("pty io: {source}"))]
    Io { source: std::io::Error },

    #[snafu(display("terminal backend error: {source}"))]
    Backend {
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[snafu(display("resize rejected: client {requester} is not the leader (current: {current:?})"))]
    NotLeader {
        requester: crate::ClientId,
        current: Option<crate::ClientId>,
    },
}

impl From<std::io::Error> for PtyError {
    fn from(source: std::io::Error) -> Self {
        Self::Io { source }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ClientId;

    #[test]
    fn not_leader_display_includes_requester_and_current() {
        let err = PtyError::NotLeader {
            requester: ClientId::from_u64(0),
            current: Some(ClientId::from_u64(1)),
        };
        let msg = format!("{err}");
        assert!(msg.contains("1"), "should mention requester id 1, got: {msg}");
        assert!(msg.contains("2"), "should mention current leader id 2, got: {msg}");
    }
}

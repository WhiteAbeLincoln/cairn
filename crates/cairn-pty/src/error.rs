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
}

impl From<std::io::Error> for PtyError {
    fn from(source: std::io::Error) -> Self {
        Self::Io { source }
    }
}

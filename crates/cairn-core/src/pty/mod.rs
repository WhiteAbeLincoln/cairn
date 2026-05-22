//! Pseudo-terminal session abstraction.
//!
//! See `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`.

mod error;

pub use error::PtyError;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn error_from_io_error() {
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "boom");
        let err: PtyError = io_err.into();
        assert!(matches!(err, PtyError::Io { .. }));
    }

    #[test]
    fn error_closed_is_constructible() {
        let err = PtyError::Closed;
        assert_eq!(format!("{err}"), "pty session has exited");
    }
}

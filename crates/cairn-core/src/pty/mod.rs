//! Pseudo-terminal session abstraction.
//!
//! See `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`.

mod error;
mod types;

pub use error::PtyError;
pub use types::{SpawnOptions, TermSize};

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

    #[test]
    fn termsize_is_copy_and_eq() {
        let a = TermSize { cols: 80, rows: 24 };
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn spawn_options_default_capacity() {
        let opts = SpawnOptions::new(std::process::Command::new("true"));
        assert_eq!(opts.broadcast_capacity, 1024);
        assert_eq!(opts.size, TermSize { cols: 80, rows: 24 });
    }

    #[test]
    fn spawn_options_builder_size() {
        let opts = SpawnOptions::new(std::process::Command::new("true"))
            .with_size(TermSize { cols: 120, rows: 40 });
        assert_eq!(opts.size, TermSize { cols: 120, rows: 40 });
    }

    #[test]
    fn spawn_options_builder_capacity() {
        let opts = SpawnOptions::new(std::process::Command::new("true"))
            .with_broadcast_capacity(64);
        assert_eq!(opts.broadcast_capacity, 64);
    }
}

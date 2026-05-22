//! Pseudo-terminal session abstraction.
//!
//! See `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`.

mod error;
mod ghostty;
mod session;
mod subscription;
mod types;

pub use error::PtyError;
pub use ghostty::GhosttyPty;
pub use session::PtySession;
pub use subscription::Subscription;
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

    #[test]
    fn subscription_constructs_from_parts() {
        use bytes::Bytes;
        use tokio::sync::broadcast;

        let (tx, rx) = broadcast::channel::<Bytes>(4);
        let snap = Bytes::from_static(b"\x1b[2J");
        let sub = Subscription {
            snapshot: snap.clone(),
            stream: rx,
        };
        assert_eq!(sub.snapshot, snap);
        drop(tx); // explicit drop so test asserts type accepts a Receiver
    }

    #[test]
    fn pty_session_is_object_safe() {
        // Compile-time check that PtySession is object-safe.
        // (If the trait grows generic methods or returns Self by value,
        // this line will fail to compile.)
        fn _assert_dyn(_: &dyn PtySession) {}
    }

    struct StubSession;

    #[async_trait::async_trait]
    impl PtySession for StubSession {
        async fn size(&self) -> Result<TermSize, PtyError> {
            Ok(TermSize { cols: 1, rows: 1 })
        }
        async fn resize(&self, _: TermSize) -> Result<(), PtyError> {
            Ok(())
        }
        async fn subscribe(&self) -> Result<Subscription, PtyError> {
            use bytes::Bytes;
            use tokio::sync::broadcast;
            let (_tx, rx) = broadcast::channel(1);
            Ok(Subscription {
                snapshot: Bytes::new(),
                stream: rx,
            })
        }
        async fn write(&self, _: bytes::Bytes) -> Result<(), PtyError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stub_session_implements_trait() {
        let s = StubSession;
        let size = s.size().await.unwrap();
        assert_eq!(size, TermSize { cols: 1, rows: 1 });
    }

    #[test]
    fn ghostty_pty_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<GhosttyPty>();
    }
}

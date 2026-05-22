use bytes::Bytes;

use super::{PtyError, Subscription, TermSize};

/// A live pseudo-terminal session wrapping a child process.
///
/// Implementations are `Send + Sync` so they can be shared across many
/// async tasks (e.g. WebSocket handlers, each holding `Arc<dyn PtySession>`).
///
/// See `docs/superpowers/specs/2026-05-22-pty-session-trait-design.md`
/// for the design rationale.
#[async_trait::async_trait]
pub trait PtySession: Send + Sync {
    /// Current terminal size in cells. Reports the kernel's TIOCGWINSZ value
    /// (what the child process actually sees).
    async fn size(&self) -> Result<TermSize, PtyError>;

    /// Resize the terminal grid. Updates the VT emulator's grid and the
    /// kernel-level PTY size (TIOCSWINSZ, which delivers SIGWINCH to the
    /// child). All updates happen atomically inside one command dispatch.
    /// Multi-client coordination is the caller's concern; last call wins.
    async fn resize(&self, size: TermSize) -> Result<(), PtyError>;

    /// Atomically take a snapshot of current terminal state AND register
    /// a live stream of subsequent output. See [`Subscription`] for
    /// the contract on what the returned snapshot and stream represent.
    async fn subscribe(&self) -> Result<Subscription, PtyError>;

    /// Write bytes to the PTY master (becomes the child's stdin).
    /// Concurrent calls from multiple tasks serialize at byte boundaries
    /// via the session's command channel.
    async fn write(&self, data: Bytes) -> Result<(), PtyError>;
}

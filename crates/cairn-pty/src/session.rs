use bytes::Bytes;

use super::{ClientId, PtyError, Subscription, TermSize};

/// A live pseudo-terminal session wrapping a child process.
///
/// Implementations are `Send + Sync` so they can be shared across many
/// async tasks (e.g. WebSocket handlers, each holding `Arc<dyn PtySession>`).
///
/// See `docs/superpowers/specs/2026-05-22-pty-multi-client-semantics-design.md`
/// for the multi-client coordination model (leader election, NotLeader
/// errors, ClientId semantics).
#[async_trait::async_trait]
pub trait PtySession: Send + Sync {
    /// Current terminal size in cells. Reports the kernel's TIOCGWINSZ
    /// value (what the child process actually sees).
    async fn size(&self) -> Result<TermSize, PtyError>;

    /// Resize the terminal grid. Only honored when `client_id` is the
    /// current leader. Returns `PtyError::NotLeader` otherwise. A
    /// resize from any client promotes them to leader if the seat is
    /// empty.
    async fn resize(&self, client_id: ClientId, size: TermSize) -> Result<(), PtyError>;

    /// Atomically snapshot current terminal state AND register a live
    /// stream of subsequent output. Subscribing does not claim
    /// leadership; only `write` or `resize` calls promote. See
    /// [`Subscription`] for the snapshot/stream contract.
    async fn subscribe(&self, client_id: ClientId) -> Result<Subscription, PtyError>;

    /// Write bytes to the PTY master (becomes the child's stdin).
    /// Bytes that pass the user-input classifier promote `client_id`
    /// to leader if it isn't already. Concurrent calls from multiple
    /// tasks serialize at byte boundaries via the session's command
    /// channel.
    async fn write(&self, client_id: ClientId, data: Bytes) -> Result<(), PtyError>;
}

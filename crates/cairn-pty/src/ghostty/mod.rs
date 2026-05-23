//! `libghostty-vt`-backed [`PtySession`] implementation.
//!
//! Runs one dedicated OS thread per session hosting a current-thread tokio
//! runtime + `LocalSet`. The thread owns the `!Send` `Terminal`, the PTY
//! master fd, and the broadcast sender. External callers reach it via a
//! `flume` command channel.
//!
//! [`PtySession`]: super::PtySession

mod input_classifier;
mod process;
mod worker;

use bytes::Bytes;
use tokio::sync::oneshot;

use super::{PtyError, SpawnOptions, Subscription, TermSize};

pub use worker::ExitStatus;

/// Commands the public API sends to the session worker thread.
pub(super) enum Command {
    Subscribe {
        reply: oneshot::Sender<Result<Subscription, PtyError>>,
    },
    Resize {
        size: TermSize,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Size {
        reply: oneshot::Sender<Result<TermSize, PtyError>>,
    },
    Write {
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Shutdown,
}

/// Handle to a running PTY session.
///
/// Construct via [`GhosttyPty::spawn`]. Send + Sync — share across tasks.
pub struct GhosttyPty {
    cmd_tx: flume::Sender<Command>,
    exit_rx: tokio::sync::watch::Receiver<Option<ExitStatus>>,
}

impl GhosttyPty {
    /// Spawn a child process inside a new PTY session.
    pub fn spawn(opts: SpawnOptions) -> Result<Self, PtyError> {
        let handles = worker::spawn(opts)?;
        Ok(Self {
            cmd_tx: handles.cmd_tx,
            exit_rx: handles.exit_rx,
        })
    }

    /// Construct a `GhosttyPty` from a `WorkerHandles` pair returned by
    /// `spawn_with`. Used by tests that inject mock `Pty`/`ChildProcess`
    /// implementations; the fields are private so tests inside `worker.rs`
    /// call this rather than writing a struct literal.
    #[cfg(test)]
    pub(in crate::ghostty) fn from_handles(handles: worker::WorkerHandles) -> Self {
        Self {
            cmd_tx: handles.cmd_tx,
            exit_rx: handles.exit_rx,
        }
    }

    /// Send a kill signal to the child and tear down the session.
    /// `wait()` will resolve shortly after.
    pub fn kill(&self) -> Result<(), PtyError> {
        self.cmd_tx
            .send(Command::Shutdown)
            .map_err(|_| PtyError::Closed)
    }

    /// Wait for the child to exit. Returns the exit status.
    ///
    /// Multiple calls are safe; all resolve once the child exits.
    pub async fn wait(&self) -> ExitStatus {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(status) = *rx.borrow() {
                return status;
            }
            // changed() returns Err only when the sender is dropped. Normally
            // that happens after a final `Some(status)` is sent, so we loop
            // back and the next borrow returns it. If the worker panicked
            // before publishing, the borrow is None — fall back to a
            // synthetic failing status so callers don't see a phantom success.
            if rx.changed().await.is_err() {
                return (*rx.borrow()).unwrap_or_else(|| worker::synthetic_exit_status(1));
            }
        }
    }
}

#[async_trait::async_trait]
impl super::PtySession for GhosttyPty {
    async fn size(&self) -> Result<super::TermSize, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Size { reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn resize(&self, size: super::TermSize) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Resize { size, reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn subscribe(&self) -> Result<Subscription, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Subscribe { reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn write(&self, data: bytes::Bytes) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Command::Write { data, reply: tx })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }
}

impl Drop for GhosttyPty {
    fn drop(&mut self) {
        // Best-effort kill on drop so dropped handles don't leak the child.
        // Failure means the session already shut down (channel closed), which
        // is fine — there's nothing to clean up.
        let _ = self.kill();
    }
}

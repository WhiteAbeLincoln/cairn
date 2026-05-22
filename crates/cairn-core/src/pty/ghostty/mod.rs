//! `libghostty-vt`-backed [`PtySession`] implementation.
//!
//! Runs one dedicated OS thread per session hosting a current-thread tokio
//! runtime + `LocalSet`. The thread owns the `!Send` `Terminal`, the PTY
//! master fd, and the broadcast sender. External callers reach it via a
//! `flume` command channel.
//!
//! [`PtySession`]: super::PtySession

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

    /// Wait for the child to exit. Returns the exit status.
    ///
    /// Multiple calls are safe; all resolve once the child exits.
    pub async fn wait(&self) -> ExitStatus {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(status) = rx.borrow().clone() {
                return status;
            }
            // changed() returns Err only when the sender is dropped, which
            // happens after a final `Some(status)` is sent — so loop back.
            if rx.changed().await.is_err() {
                return rx
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| ExitStatus::with_exit_code(1));
            }
        }
    }
}

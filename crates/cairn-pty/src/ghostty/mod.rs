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

use super::{ClientId, PtyError, SpawnOptions, Subscription, TermSize};

/// Wraps a [`Command`] with the caller's current OTel trace context, so
/// the worker can create span links that bridge the async→thread boundary.
pub(crate) struct Envelope {
    pub cmd: Command,
    pub trace_id: Option<String>,
}

/// Extract the current OTel span's trace-id + span-id as a W3C `traceparent`
/// string, or `None` if no valid OTel context is active.
fn current_trace_id() -> Option<String> {
    use opentelemetry::trace::TraceContextExt as _;
    let cx = tracing_opentelemetry::OpenTelemetrySpanExt::context(&tracing::Span::current());
    let span_ref = cx.span();
    let sc = span_ref.span_context();
    if sc.is_valid() {
        Some(format!(
            "00-{}-{}-{:02x}",
            sc.trace_id(),
            sc.span_id(),
            sc.trace_flags().to_u8(),
        ))
    } else {
        None
    }
}

/// Commands the public API sends to the session worker thread.
pub(crate) enum Command {
    Subscribe {
        client_id: ClientId,
        reply: oneshot::Sender<Result<Subscription, PtyError>>,
    },
    Resize {
        client_id: ClientId,
        size: TermSize,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Size {
        reply: oneshot::Sender<Result<TermSize, PtyError>>,
    },
    Write {
        client_id: ClientId,
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    /// Sent by `SubscriptionGuard::drop`. Worker checks if `client_id`
    /// is the current leader and clears the seat if so.
    Detach {
        client_id: ClientId,
    },
    /// Deliver `sig` to the child's process group. Not leader-gated.
    Signal {
        sig: nix::sys::signal::Signal,
        reason: Option<String>,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    /// Write to the PTY with no client identity and no leader promotion.
    /// Backs `cairn send`.
    Inject {
        data: Bytes,
        reply: oneshot::Sender<Result<(), PtyError>>,
    },
    Shutdown,
}

/// Handle to a running PTY session.
///
/// Construct via [`GhosttyPty::spawn`]. Send + Sync — share across tasks.
pub struct GhosttyPty {
    cmd_tx: flume::Sender<Envelope>,
    exit_rx: tokio::sync::watch::Receiver<Option<crate::ExitStatus>>,
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
            .send(Envelope {
                cmd: Command::Shutdown,
                trace_id: current_trace_id(),
            })
            .map_err(|_| PtyError::Closed)
    }

    /// Wait for the child to exit. Returns the exit status.
    ///
    /// Multiple calls are safe; all resolve once the child exits.
    pub async fn wait(&self) -> crate::ExitStatus {
        let mut rx = self.exit_rx.clone();
        loop {
            if let Some(status) = rx.borrow().clone() {
                return status;
            }
            // changed() returns Err only when the sender is dropped. Normally
            // that happens after a final `Some(status)` is sent, so we loop
            // back and the next borrow returns it. If the worker panicked
            // before publishing, the borrow is None — fall back to a
            // synthetic failing status so callers don't see a phantom success.
            if rx.changed().await.is_err() {
                return rx.borrow().clone().unwrap_or_else(|| {
                    crate::ExitStatus::synthetic(1, crate::types::now_unix_ms())
                });
            }
        }
    }

    /// Non-blocking peek at the exit state. `None` while the child is running.
    pub fn try_exit_status(&self) -> Option<crate::ExitStatus> {
        self.exit_rx.borrow().clone()
    }

    /// Deliver a signal to the child's process group.
    pub async fn signal(
        &self,
        sig: nix::sys::signal::Signal,
        reason: Option<String>,
    ) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Envelope {
                cmd: Command::Signal {
                    sig,
                    reason,
                    reply: tx,
                },
                trace_id: current_trace_id(),
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    /// Write bytes to the PTY without claiming leadership (backs `send`).
    pub async fn inject(&self, data: Bytes) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Envelope {
                cmd: Command::Inject { data, reply: tx },
                trace_id: current_trace_id(),
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }
}

#[async_trait::async_trait]
impl super::PtySession for GhosttyPty {
    async fn size(&self) -> Result<super::TermSize, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Envelope {
                cmd: Command::Size { reply: tx },
                trace_id: current_trace_id(),
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn resize(&self, client_id: ClientId, size: super::TermSize) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Envelope {
                cmd: Command::Resize {
                    client_id,
                    size,
                    reply: tx,
                },
                trace_id: current_trace_id(),
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn subscribe(&self, client_id: ClientId) -> Result<Subscription, PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Envelope {
                cmd: Command::Subscribe {
                    client_id,
                    reply: tx,
                },
                trace_id: current_trace_id(),
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn write(&self, client_id: ClientId, data: bytes::Bytes) -> Result<(), PtyError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send_async(Envelope {
                cmd: Command::Write {
                    client_id,
                    data,
                    reply: tx,
                },
                trace_id: current_trace_id(),
            })
            .await
            .map_err(|_| PtyError::Closed)?;
        rx.await.map_err(|_| PtyError::Closed)?
    }

    async fn signal(
        &self,
        sig: nix::sys::signal::Signal,
        reason: Option<String>,
    ) -> Result<(), PtyError> {
        GhosttyPty::signal(self, sig, reason).await
    }

    async fn inject(&self, data: bytes::Bytes) -> Result<(), PtyError> {
        GhosttyPty::inject(self, data).await
    }

    async fn wait(&self) -> crate::ExitStatus {
        GhosttyPty::wait(self).await
    }

    fn try_exit_status(&self) -> Option<crate::ExitStatus> {
        GhosttyPty::try_exit_status(self)
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

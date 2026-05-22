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
use tokio::sync::{broadcast, oneshot};

use super::{PtyError, Subscription, TermSize};

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
/// Construct via [`GhosttyPty::spawn`]. Cheap to clone via `Arc`; the
/// session keeps running until the child exits or [`GhosttyPty::kill`]
/// is called.
pub struct GhosttyPty {
    cmd_tx: flume::Sender<Command>,
}

impl GhosttyPty {
    // Lifecycle methods filled in in later tasks.
}

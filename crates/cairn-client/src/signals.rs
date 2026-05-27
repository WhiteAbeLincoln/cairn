//! Signal handling for `attach`. Per the detach-camp model (matching
//! zmx/dtach/abduco/shpool): only SIGWINCH is forwarded (as a resize); every
//! other client-received signal triggers a clean detach. Nothing is forwarded
//! to the child.

use std::io;

use tokio::signal::unix::{Signal, SignalKind, signal};

/// The set of signals that, when delivered to the client, mean "detach". We
/// trap them (rather than letting them default-kill the process) so the
/// terminal is restored cleanly before exit.
pub struct Termination {
    int: Signal,
    term: Signal,
    quit: Signal,
    hup: Signal,
    usr1: Signal,
    usr2: Signal,
}

impl Termination {
    pub fn install() -> io::Result<Self> {
        Ok(Self {
            int: signal(SignalKind::interrupt())?,
            term: signal(SignalKind::terminate())?,
            quit: signal(SignalKind::quit())?,
            hup: signal(SignalKind::hangup())?,
            usr1: signal(SignalKind::user_defined1())?,
            usr2: signal(SignalKind::user_defined2())?,
        })
    }

    /// Resolves when any termination signal is received. Cancel-safe.
    pub async fn recv(&mut self) {
        tokio::select! {
            _ = self.int.recv() => {}
            _ = self.term.recv() => {}
            _ = self.quit.recv() => {}
            _ = self.hup.recv() => {}
            _ = self.usr1.recv() => {}
            _ = self.usr2.recv() => {}
        }
    }
}

/// A SIGWINCH stream for resize handling.
pub fn window_changes() -> io::Result<Signal> {
    signal(SignalKind::window_change())
}

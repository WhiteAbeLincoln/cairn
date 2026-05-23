//! Trait abstractions over the PTY master fd and the child process so the
//! session worker can be exercised against mock implementations in tests.
//!
//! Both traits use native async fn (Rust 1.75+); we deliberately avoid
//! `async_trait`'s `Box<dyn Future>` overhead because the worker is on the
//! hot path for every PTY-master byte. Trait objects are not needed —
//! callers (e.g. `SessionState<P, C>`) are generic and monomorphise.
//!
//! The `Pty` trait's `set_size` is named to avoid collision with
//! `pty_process::Pty::resize`. The `read` / `write_all` trait methods
//! collide with `tokio::io::AsyncReadExt` / `AsyncWriteExt`; the
//! production impls disambiguate by calling those Ext traits with fully
//! qualified syntax.

use std::process::ExitStatus;

use crate::TermSize;

/// PTY master fd abstraction — async read/write plus a sync size setter.
///
/// Production: `impl Pty for pty_process::Pty` wraps the existing async I/O
/// and resize methods. Test: a mock backed by tokio mpsc channels lets the
/// test feed bytes the worker will see on read and observe what the worker
/// writes via `write_all`.
pub(super) trait Pty: Send + 'static {
    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()>;
    fn set_size(&self, size: TermSize) -> std::io::Result<()>;
}

/// Child process abstraction — async wait + sync kill.
///
/// Production: `impl ChildProcess for tokio::process::Child` forwards to
/// the inherent methods. Test: a mock holds a tokio watch channel; the
/// test signals exit by sending Some(status), and `wait()` resolves when
/// the watched value is non-None.
pub(super) trait ChildProcess: Send + 'static {
    async fn wait(&mut self) -> std::io::Result<ExitStatus>;
    fn start_kill(&mut self) -> std::io::Result<()>;
}

// ─── Production impls ─────────────────────────────────────────────────

impl Pty for pty_process::Pty {
    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use tokio::io::AsyncReadExt;
        // Disambiguate from our own trait method by calling through the
        // extension trait explicitly.
        AsyncReadExt::read(self, buf).await
    }

    async fn write_all(&mut self, data: &[u8]) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        AsyncWriteExt::write_all(self, data).await
    }

    fn set_size(&self, size: TermSize) -> std::io::Result<()> {
        // Convert TermSize to pty_process::Size; wrap pty_process's error
        // type into io::Error so the trait surface is io-typed throughout.
        pty_process::Pty::resize(self, pty_process::Size::new(size.rows, size.cols))
            .map_err(std::io::Error::other)
    }
}

impl ChildProcess for tokio::process::Child {
    async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        tokio::process::Child::wait(self).await
    }

    fn start_kill(&mut self) -> std::io::Result<()> {
        tokio::process::Child::start_kill(self)
    }
}

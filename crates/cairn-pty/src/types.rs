/// Outcome of a finished session: the child's exit status plus the wall-clock
/// time (Unix epoch ms) the exit was detected. The timestamp is captured by the
/// worker at exit-detection time so a caller that was not waiting can still
/// report when the session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitStatus {
    code: Option<i32>,
    signal: Option<i32>,
    unix_ms: u64,
}

impl ExitStatus {
    /// Exit code if the child exited normally.
    pub fn code(&self) -> Option<i32> { self.code }
    /// Terminating signal number if the child was killed by a signal.
    pub fn signal(&self) -> Option<i32> { self.signal }
    /// Wall-clock time (Unix epoch ms) the exit was detected.
    pub fn unix_ms(&self) -> u64 { self.unix_ms }
    /// True iff the child exited with code 0.
    pub fn success(&self) -> bool { self.code == Some(0) }

    /// Build from the std exit status the child reports, stamping `unix_ms`.
    pub(crate) fn from_std(status: std::process::ExitStatus, unix_ms: u64) -> Self {
        use std::os::unix::process::ExitStatusExt;
        Self { code: status.code(), signal: status.signal(), unix_ms }
    }

    /// Synthetic status for the "wait itself failed" fallback.
    pub(crate) fn synthetic(code: i32, unix_ms: u64) -> Self {
        Self { code: Some(code), signal: None, unix_ms }
    }
}

/// Current Unix epoch time in milliseconds (saturating to 0 before the epoch).
pub(crate) fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Terminal grid size in cells. Matches the kernel TIOCGWINSZ representation
/// of cols (width) and rows (height).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

/// Options for spawning a new PTY session.
///
/// Construct via [`SpawnOptions::new`] with a configured
/// [`tokio::process::Command`]. The worker translates the command's argv,
/// env, and cwd into a `pty_process::Command` at spawn time, reading those
/// fields via `tokio::process::Command::as_std()`.
pub struct SpawnOptions {
    pub command: tokio::process::Command,
    pub size: TermSize,
    pub broadcast_capacity: usize,
    /// Maximum scrollback lines retained by the VT emulator. The snapshot
    /// returned by `subscribe()` includes these rows. Default: 1000.
    pub scrollback_lines: usize,
}

impl SpawnOptions {
    pub fn new(command: tokio::process::Command) -> Self {
        Self {
            command,
            size: TermSize::default(),
            broadcast_capacity: 1024,
            scrollback_lines: 1000,
        }
    }

    pub fn with_size(mut self, size: TermSize) -> Self {
        self.size = size;
        self
    }

    pub fn with_broadcast_capacity(mut self, capacity: usize) -> Self {
        self.broadcast_capacity = capacity;
        self
    }

    pub fn with_scrollback_lines(mut self, lines: usize) -> Self {
        self.scrollback_lines = lines;
        self
    }
}

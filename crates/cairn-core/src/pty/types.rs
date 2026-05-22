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

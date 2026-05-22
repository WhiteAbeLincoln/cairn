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
/// Construct via [`SpawnOptions::new`] with a configured [`std::process::Command`].
/// `std::process::Command` (not `tokio::process::Command`) because
/// `portable-pty::SlavePty::spawn_command` expects the std variant.
pub struct SpawnOptions {
    pub command: std::process::Command,
    pub size: TermSize,
    pub broadcast_capacity: usize,
    /// Maximum scrollback lines retained by the VT emulator. The snapshot
    /// returned by `subscribe()` includes these rows. Default: 1000.
    pub scrollback_lines: usize,
}

impl SpawnOptions {
    pub fn new(command: std::process::Command) -> Self {
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

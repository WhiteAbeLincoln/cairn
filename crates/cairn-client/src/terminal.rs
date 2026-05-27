//! Local-terminal control: raw mode (with RAII restore), window size, output.

use std::io::{self, Write};
use std::os::fd::{AsFd, AsRawFd};

use nix::sys::termios::{self, SetArg, SpecialCharacterIndices, Termios};

/// RAII guard that puts stdin into raw mode and restores it on drop. If stdin
/// is not a TTY, this is a no-op guard (output still streams; no raw munging).
pub struct RawGuard {
    original: Option<Termios>,
}

impl RawGuard {
    pub fn engage() -> io::Result<Self> {
        let stdin = io::stdin();
        // tcgetattr fails (ENOTTY) when stdin isn't a terminal — degrade.
        let original = match termios::tcgetattr(stdin.as_fd()) {
            Ok(t) => t,
            Err(_) => return Ok(Self { original: None }),
        };
        let mut raw = original.clone();
        termios::cfmakeraw(&mut raw); // clears ISIG/IEXTEN/ICANON/ECHO; sets VMIN=1/VTIME=0
        // Be explicit about the one-byte-read discipline regardless of libc.
        raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
        raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
        termios::tcsetattr(stdin.as_fd(), SetArg::TCSANOW, &raw)
            .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
        Ok(Self { original: Some(original) })
    }

    pub fn is_raw(&self) -> bool {
        self.original.is_some()
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        if let Some(orig) = &self.original {
            let stdin = io::stdin();
            let _ = termios::tcsetattr(stdin.as_fd(), SetArg::TCSAFLUSH, orig);
            // RIS: reset the outer terminal out of alt-screen / mouse / paste modes
            // the inferior may have set.
            let mut out = io::stdout();
            let _ = out.write_all(b"\x1bc");
            let _ = out.flush();
        }
    }
}

nix::ioctl_read_bad!(tiocgwinsz, nix::libc::TIOCGWINSZ, nix::libc::winsize);

/// Current terminal size as `(cols, rows)`, or `None` when stdout isn't a TTY.
pub fn window_size() -> Option<(u16, u16)> {
    let mut ws: nix::libc::winsize = unsafe { std::mem::zeroed() };
    let fd = io::stdout().as_raw_fd();
    // SAFETY: `ws` is a valid, writable winsize for the duration of the call.
    let rc = unsafe { tiocgwinsz(fd, &mut ws) };
    match rc {
        Ok(_) if ws.ws_col > 0 => Some((ws.ws_col, ws.ws_row)),
        _ => None,
    }
}

/// Clear the screen and home the cursor — a clean canvas before snapshot replay.
pub fn clear_screen(out: &mut impl Write) -> io::Result<()> {
    out.write_all(b"\x1b[2J\x1b[H")?;
    out.flush()
}

/// Write session output to stdout. Blocking write; fine for a TTY (the daemon's
/// lag-kick handles a slow consumer). Errors are swallowed — a broken stdout
/// surfaces as the stream/transport ending elsewhere.
pub fn write_stdout(bytes: &[u8]) {
    let mut out = io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

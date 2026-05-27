//! Daemon configuration: defaults < CAIRN_* env < CLI flags.

use std::path::PathBuf;
use std::time::Duration;

/// Resolved daemon configuration.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: PathBuf,
    pub dir_mode: u32,
    pub socket_mode: u32,
    pub shutdown_grace: Duration,
    pub default_shell: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            dir_mode: 0o700,
            socket_mode: 0o600,
            shutdown_grace: Duration::from_secs(5),
            default_shell: default_shell(),
        }
    }
}

/// `$XDG_RUNTIME_DIR/cairn/cairn.sock` on Linux, `$TMPDIR/cairn/cairn.sock`
/// otherwise. The `cairn/` parent is daemon-owned so `dir_mode` governs it.
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn").join("cairn.sock")
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

/// Parse an octal file mode, accepting `0o750` or bare `750`.
pub fn parse_octal_mode(s: &str) -> Result<u32, std::num::ParseIntError> {
    let digits = s.strip_prefix("0o").unwrap_or(s);
    u32::from_str_radix(digits, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_octal_accepts_0o_and_bare() {
        assert_eq!(parse_octal_mode("0o750").unwrap(), 0o750);
        assert_eq!(parse_octal_mode("750").unwrap(), 0o750);
        assert!(parse_octal_mode("nonsense").is_err());
    }

    #[test]
    fn defaults_are_conservative() {
        let c = DaemonConfig::default();
        assert_eq!(c.dir_mode, 0o700);
        assert_eq!(c.socket_mode, 0o600);
        assert_eq!(c.shutdown_grace, std::time::Duration::from_secs(5));
        assert!(c.socket_path.ends_with("cairn/cairn.sock"));
    }
}

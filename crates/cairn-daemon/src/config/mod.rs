//! Daemon configuration: defaults < CAIRN_* env < CLI flags.

mod args;

pub use args::Args;

use std::path::PathBuf;
use std::time::Duration;

use crate::listen::ListenerConfig;

/// Which authentication backend to enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum AuthBackendKind {
    /// Authenticate via the Tailscale LocalAPI (whois).
    Tailscale,
}

/// Controls the stderr log output format.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    /// Human-friendly coloured output (default).
    #[default]
    Pretty,
    /// One-line-per-event, no ANSI colours.
    Compact,
    /// Newline-delimited JSON objects.
    Json,
    /// The `tracing-subscriber` "full" format (like compact but with span context).
    Full,
    /// Suppress stderr logging entirely.
    Off,
}

/// Resolved daemon configuration.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub listeners: Vec<ListenerConfig>,
    pub dir_mode: u32,
    pub socket_mode: u32,
    pub wt_cert: Option<PathBuf>,
    pub wt_key: Option<PathBuf>,
    pub wt_connect_timeout: Duration,
    pub wt_idle_timeout: Duration,
    pub auth_backends: Vec<AuthBackendKind>,
    pub auth_timeout: Duration,
    pub shutdown_grace: Duration,
    pub default_shell: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listeners: vec![ListenerConfig::Unix(default_socket_path())],
            dir_mode: 0o700,
            socket_mode: 0o600,
            wt_cert: None,
            wt_key: None,
            wt_connect_timeout: Duration::from_secs(30),
            wt_idle_timeout: Duration::from_secs(300),
            auth_backends: vec![],
            auth_timeout: Duration::from_secs(5),
            shutdown_grace: Duration::from_secs(5),
            default_shell: default_shell(),
        }
    }
}

/// The daemon's runtime directory: `$XDG_RUNTIME_DIR/cairn` or
/// `$TMPDIR/cairn` or `/tmp/cairn`.
pub fn runtime_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("TMPDIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("cairn")
}

/// `$XDG_RUNTIME_DIR/cairn/cairn.sock` on Linux, `$TMPDIR/cairn/cairn.sock`
/// otherwise. The `cairn/` parent is daemon-owned so `dir_mode` governs it.
pub fn default_socket_path() -> PathBuf {
    runtime_dir().join("cairn.sock")
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
    fn log_format_round_trips_through_value_enum() {
        use clap::ValueEnum;
        for variant in LogFormat::value_variants() {
            let name = variant
                .to_possible_value()
                .map(|v| v.get_name().to_owned())
                .unwrap_or_default();
            let parsed = LogFormat::from_str(&name, /* ignore_case */ true);
            assert_eq!(parsed, Ok(*variant), "round-trip failed for {name:?}");
        }
    }

    #[test]
    fn defaults_are_conservative() {
        let c = DaemonConfig::default();
        assert_eq!(c.dir_mode, 0o700);
        assert_eq!(c.socket_mode, 0o600);
        assert_eq!(c.shutdown_grace, std::time::Duration::from_secs(5));
        assert_eq!(c.listeners.len(), 1);
        assert!(c.listeners[0].is_unix());
    }
}

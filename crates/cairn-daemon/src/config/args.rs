//! CLI argument definitions for `cairn-daemon`.

use clap::Parser;

use super::{AuthBackendKind, DaemonConfig, LogFormat};
use crate::listen::{self, ListenerConfig};

#[derive(Parser)]
#[command(version, about = "The cairn session-manager daemon")]
pub struct Args {
    /// Listener endpoints. Repeat or comma-separate. Examples:
    ///   unix                    — default UDS path
    ///   unix:///path/to.sock    — explicit UDS path
    ///   https://0.0.0.0:9443    — WebTransport listener (W3C scheme)
    ///   /path/to.sock           — bare path treated as unix://
    #[arg(
        long,
        env = "CAIRN_LISTEN",
        value_delimiter = ',',
        value_parser = listen::parse_listener
    )]
    pub listen: Vec<ListenerConfig>,

    /// Octal permission mode for the socket parent directory (e.g. 700 or 0o700).
    #[arg(long, env = "CAIRN_DIR_MODE", value_parser = super::parse_octal_mode)]
    pub dir_mode: Option<u32>,

    /// Octal permission mode for the socket file itself.
    #[arg(long, env = "CAIRN_SOCKET_MODE", value_parser = super::parse_octal_mode)]
    pub socket_mode: Option<u32>,

    /// TLS certificate file for the WebTransport listener.
    #[arg(long, env = "CAIRN_WT_CERT")]
    pub wt_cert: Option<std::path::PathBuf>,

    /// TLS private key file for the WebTransport listener.
    #[arg(long, env = "CAIRN_WT_KEY")]
    pub wt_key: Option<std::path::PathBuf>,

    /// WebTransport connection timeout (e.g. "30s", "1m").
    #[arg(long, env = "CAIRN_WT_CONNECT_TIMEOUT", value_parser = humantime::parse_duration)]
    pub wt_connect_timeout: Option<std::time::Duration>,

    /// WebTransport idle timeout (e.g. "5m", "300s").
    #[arg(long, env = "CAIRN_WT_IDLE_TIMEOUT", value_parser = humantime::parse_duration)]
    pub wt_idle_timeout: Option<std::time::Duration>,

    /// Authentication backends for network listeners. Repeat or comma-separate.
    /// Required when a network listener (https://) is configured.
    #[arg(long, env = "CAIRN_AUTH", value_delimiter = ',', value_enum)]
    pub auth: Vec<AuthBackendKind>,

    /// Timeout for authentication handshakes (e.g. "5s").
    #[arg(long, env = "CAIRN_AUTH_TIMEOUT", value_parser = humantime::parse_duration)]
    pub auth_timeout: Option<std::time::Duration>,

    /// How long to wait for sessions to exit on daemon shutdown (e.g. "5s", "500ms").
    #[arg(long, env = "CAIRN_SHUTDOWN_GRACE", value_parser = humantime::parse_duration)]
    pub shutdown_grace: Option<std::time::Duration>,

    /// Default shell for sessions that don't specify a command.
    #[arg(long, env = "CAIRN_DEFAULT_SHELL")]
    pub default_shell: Option<String>,

    /// `tracing-subscriber` filter directive.
    #[arg(
        long,
        env = "CAIRN_LOG",
        default_value = "info,cairn_daemon=info,cairn_pty=info"
    )]
    pub log: String,

    /// Stderr log format (pretty, compact, json, full, off).
    #[arg(long, env = "CAIRN_LOG_FORMAT", default_value = "pretty", value_enum)]
    pub log_format: LogFormat,
}

impl From<Args> for DaemonConfig {
    fn from(args: Args) -> Self {
        let mut cfg = DaemonConfig::default();

        if !args.listen.is_empty() {
            cfg.listeners = args.listen;
        }
        if let Some(m) = args.dir_mode {
            cfg.dir_mode = m;
        }
        if let Some(m) = args.socket_mode {
            cfg.socket_mode = m;
        }
        if let Some(p) = args.wt_cert {
            cfg.wt_cert = Some(p);
        }
        if let Some(p) = args.wt_key {
            cfg.wt_key = Some(p);
        }
        if let Some(t) = args.wt_connect_timeout {
            cfg.wt_connect_timeout = t;
        }
        if let Some(t) = args.wt_idle_timeout {
            cfg.wt_idle_timeout = t;
        }
        cfg.auth_backends = args.auth;
        if let Some(t) = args.auth_timeout {
            cfg.auth_timeout = t;
        }
        if let Some(g) = args.shutdown_grace {
            cfg.shutdown_grace = g;
        }
        if let Some(s) = args.default_shell {
            cfg.default_shell = s;
        }

        cfg
    }
}

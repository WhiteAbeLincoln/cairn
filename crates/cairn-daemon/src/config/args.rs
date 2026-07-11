//! CLI argument definitions for `cairn-daemon`.

use clap::Parser;

use super::{AuthBackendKind, DaemonConfig, LogFormat, WebUiMode};
use crate::listen::{self, ListenerConfig};

/// Sentinel `default_missing_value` for `--web-ui` given with no `=value`
/// (bare form). Not a valid `host:port` string, so it can't collide with a
/// real dedicated-listener target.
const WEB_UI_ATTACH: &str = "attach";

#[derive(Parser)]
#[command(version, about = "The cairn session-manager daemon")]
pub struct Args {
    // extra whitespace here is necessary for the cli args
    // to render properly
    /// Listener endpoints. Repeat or comma-separate. Examples:
    ///
    /// - `unix`: default UDS path
    ///
    /// - `unix:///path/to.sock`: explicit UDS path
    ///
    /// - `https://0.0.0.0:9443`: WebTransport listener (W3C scheme)
    ///
    /// - `ws://127.0.0.1:8080`: WebSocket listener (browser-facing)
    ///
    /// - `/path/to.sock`: bare path treated as unix://
    #[arg(
        long,
        env = "CAIRN_LISTEN",
        value_delimiter = ',',
        value_parser = listen::parse_listener
    )]
    pub listen: Vec<Vec<ListenerConfig>>,

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

    /// Maximum concurrent WebTransport connections in the accept/auth
    /// handshake phase. Excess connections are dropped immediately.
    #[arg(long, env = "CAIRN_WT_MAX_PENDING")]
    pub wt_max_pending: Option<usize>,

    /// Additional allowed WebSocket `Origin` values (e.g. `https://cairn.example`).
    /// Repeat or comma-separate. The request's own `Host`-derived origin is always
    /// allowed; this extends the allowlist for cross-origin browser clients.
    #[arg(long, env = "CAIRN_WS_ORIGIN", value_delimiter = ',')]
    pub ws_origin: Vec<String>,

    /// Serve the SPA web UI. Bare `--web-ui` attaches SPA routes (and
    /// `/cairn.json`) to every `ws://` listener — an error at startup if none
    /// exist. `--web-ui=host:port` instead binds a dedicated HTTP listener
    /// that serves only the SPA (no `/ws`); valid even with only a
    /// WebTransport listener configured.
    #[arg(
        long,
        env = "CAIRN_WEB_UI",
        num_args = 0..=1,
        default_missing_value = WEB_UI_ATTACH,
        value_name = "HOST:PORT"
    )]
    pub web_ui: Option<String>,

    /// Serve SPA assets from this directory instead of the compiled-in embed
    /// (works with or without the `web-ui` compile-time embed feature).
    #[arg(long, env = "CAIRN_WEB_DIR")]
    pub web_dir: Option<std::path::PathBuf>,

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

impl TryFrom<Args> for DaemonConfig {
    type Error = anyhow::Error;

    fn try_from(args: Args) -> anyhow::Result<Self> {
        let mut cfg = DaemonConfig::default();

        let listeners: Vec<ListenerConfig> = args.listen.into_iter().flatten().collect();
        if !listeners.is_empty() {
            cfg.listeners = listeners;
        }
        if let Some(m) = args.dir_mode {
            cfg.dir_mode = m;
        }
        if let Some(m) = args.socket_mode {
            cfg.socket_mode = m;
        }
        cfg.wt_tls = match (args.wt_cert, args.wt_key) {
            (Some(cert), Some(key)) => Some(super::WtTlsIdentity { cert, key }),
            (None, None) => None,
            _ => anyhow::bail!("--wt-cert and --wt-key must both be provided, or both omitted"),
        };
        if let Some(t) = args.wt_connect_timeout {
            cfg.wt_connect_timeout = t;
        }
        if let Some(t) = args.wt_idle_timeout {
            cfg.wt_idle_timeout = t;
        }
        if let Some(n) = args.wt_max_pending {
            cfg.wt_max_pending = n;
        }
        cfg.auth_backends = args.auth;
        if !args.ws_origin.is_empty() {
            cfg.ws_origins = args.ws_origin;
        }
        cfg.web_ui = match args.web_ui.as_deref() {
            None => None,
            Some(WEB_UI_ATTACH) => Some(WebUiMode::Attach),
            Some(target) => Some(WebUiMode::Dedicated(listen::parse_addr_spec(target)?)),
        };
        cfg.web_dir = args.web_dir;
        if let Some(t) = args.auth_timeout {
            cfg.auth_timeout = t;
        }
        if let Some(g) = args.shutdown_grace {
            cfg.shutdown_grace = g;
        }
        if let Some(s) = args.default_shell {
            cfg.default_shell = s;
        }

        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[test]
    fn auth_flag_accepts_tailscale_serve_alone() {
        let args = Args::try_parse_from(["cairn-daemon", "--auth", "tailscale-serve"])
            .expect("--auth tailscale-serve should parse");
        assert_eq!(args.auth, vec![AuthBackendKind::TailscaleServe]);
    }

    #[test]
    fn auth_flag_accepts_tailscale_and_tailscale_serve_together() {
        let args = Args::try_parse_from(["cairn-daemon", "--auth", "tailscale,tailscale-serve"])
            .expect("comma-separated --auth should parse both backends");
        assert_eq!(
            args.auth,
            vec![AuthBackendKind::Tailscale, AuthBackendKind::TailscaleServe]
        );
    }

    // ── --web-ui ──────────────────────────────────────────────────────────

    #[test]
    fn web_ui_absent_by_default() {
        let cfg: DaemonConfig = Args::try_parse_from(["cairn-daemon"])
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(cfg.web_ui, None);
    }

    #[test]
    fn web_ui_bare_attaches() {
        let cfg: DaemonConfig = Args::try_parse_from(["cairn-daemon", "--web-ui"])
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(cfg.web_ui, Some(WebUiMode::Attach));
    }

    #[test]
    fn web_ui_with_target_is_dedicated() {
        let cfg: DaemonConfig = Args::try_parse_from(["cairn-daemon", "--web-ui=127.0.0.1:5173"])
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(
            cfg.web_ui,
            Some(WebUiMode::Dedicated(vec![
                "127.0.0.1:5173".parse().unwrap()
            ]))
        );
    }

    #[test]
    fn web_ui_with_invalid_target_errors() {
        let args = Args::try_parse_from(["cairn-daemon", "--web-ui=not-a-target"]).unwrap();
        let result: anyhow::Result<DaemonConfig> = args.try_into();
        assert!(result.is_err(), "malformed --web-ui target should error");
    }

    #[test]
    fn web_dir_parses_as_path() {
        let cfg: DaemonConfig = Args::try_parse_from(["cairn-daemon", "--web-dir", "/tmp/spa"])
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(cfg.web_dir, Some(std::path::PathBuf::from("/tmp/spa")));
    }
}

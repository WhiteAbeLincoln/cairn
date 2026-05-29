//! The cairn session-manager daemon entry point.
//!
//! Parses CLI flags (with `CAIRN_*` env overrides), initialises tracing to
//! stderr, builds a `Daemon`, sets up SIGTERM/ctrl-c shutdown via a
//! `CancellationToken`, and runs `serve()`.

use clap::Parser;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(version, about = "The cairn session-manager daemon")]
struct Args {
    /// Listener endpoints. Repeat or comma-separate. Examples:
    ///   unix                  — default UDS path
    ///   unix:///path/to.sock  — explicit UDS path
    ///   wt://0.0.0.0:9443     — WebTransport listener
    ///   /path/to.sock         — bare path treated as unix://
    #[arg(
        long,
        env = "CAIRN_LISTEN",
        value_delimiter = ',',
        value_parser = cairn_daemon::listen::parse_listener
    )]
    listen: Vec<cairn_daemon::listen::ListenerConfig>,

    /// Octal permission mode for the socket parent directory (e.g. 700 or 0o700).
    #[arg(long, env = "CAIRN_DIR_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    dir_mode: Option<u32>,

    /// Octal permission mode for the socket file itself.
    #[arg(long, env = "CAIRN_SOCKET_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    socket_mode: Option<u32>,

    /// TLS certificate file for the WebTransport listener.
    #[arg(long, env = "CAIRN_WT_CERT")]
    wt_cert: Option<std::path::PathBuf>,

    /// TLS private key file for the WebTransport listener.
    #[arg(long, env = "CAIRN_WT_KEY")]
    wt_key: Option<std::path::PathBuf>,

    /// WebTransport connection timeout (e.g. "30s", "1m").
    #[arg(long, env = "CAIRN_WT_CONNECT_TIMEOUT", value_parser = humantime::parse_duration)]
    wt_connect_timeout: Option<std::time::Duration>,

    /// WebTransport idle timeout (e.g. "5m", "300s").
    #[arg(long, env = "CAIRN_WT_IDLE_TIMEOUT", value_parser = humantime::parse_duration)]
    wt_idle_timeout: Option<std::time::Duration>,

    /// Authentication backends. Repeat or comma-separate (e.g. "none", "token").
    #[arg(
        long,
        env = "CAIRN_AUTH",
        value_delimiter = ',',
        default_value = "none"
    )]
    auth: Vec<String>,

    /// Timeout for authentication handshakes (e.g. "5s").
    #[arg(long, env = "CAIRN_AUTH_TIMEOUT", value_parser = humantime::parse_duration)]
    auth_timeout: Option<std::time::Duration>,

    /// How long to wait for sessions to exit on daemon shutdown (e.g. "5s", "500ms").
    #[arg(long, env = "CAIRN_SHUTDOWN_GRACE", value_parser = humantime::parse_duration)]
    shutdown_grace: Option<std::time::Duration>,

    /// Default shell for sessions that don't specify a command.
    #[arg(long, env = "CAIRN_DEFAULT_SHELL")]
    default_shell: Option<String>,

    /// `tracing-subscriber` filter directive.
    #[arg(
        long,
        env = "CAIRN_LOG",
        default_value = "info,cairn_daemon=info,cairn_pty=info"
    )]
    log: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(args.log.clone()))
        .with_writer(std::io::stderr)
        .init();

    let mut cfg = cairn_daemon::config::DaemonConfig::default();

    // If the user supplied explicit listeners, replace the default.
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

    // Validation warnings: flag mismatches that aren't hard errors yet.
    let has_wt = cfg.listeners.iter().any(|l| l.is_wt());
    let has_unix = cfg.listeners.iter().any(|l| l.is_unix());

    if !has_unix && (cfg.dir_mode != 0o700 || cfg.socket_mode != 0o600) {
        tracing::warn!("--dir-mode / --socket-mode have no effect without a unix:// listener");
    }
    if has_wt {
        // Warn if WT is configured but cert/key are missing (not fatal — WT isn't wired yet).
        if cfg.wt_cert.is_none() || cfg.wt_key.is_none() {
            tracing::warn!("wt:// listener configured but --wt-cert / --wt-key not set");
        }
        // Warn if auth=none is used with a non-loopback WT listener.
        let auth_none = cfg.auth_backends.iter().any(|b| b == "none");
        if auth_none {
            use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
            let has_non_loopback_wt = cfg.listeners.iter().any(|l| match l {
                cairn_daemon::listen::ListenerConfig::WebTransport(addr) => {
                    let ip = addr.ip();
                    ip != IpAddr::V4(Ipv4Addr::LOCALHOST)
                        && ip != IpAddr::V6(Ipv6Addr::LOCALHOST)
                        && !addr.ip().is_loopback()
                }
                _ => false,
            });
            if has_non_loopback_wt {
                tracing::warn!(
                    "auth=none with a non-loopback wt:// listener exposes the daemon without authentication"
                );
            }
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let daemon = cairn_daemon::daemon::Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let sig = shutdown.clone();
        // Install the SIGTERM handler before spawning so an install failure
        // propagates out of `main` rather than panicking in a detached task
        // (which would silently leave the daemon deaf to SIGTERM).
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = term.recv() => {},
            }
            sig.cancel();
        });
        cairn_daemon::serve::serve(daemon, shutdown).await
    })
}

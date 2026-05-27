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
    /// Path for the Unix domain socket.
    #[arg(long, env = "CAIRN_SOCKET")]
    socket: Option<std::path::PathBuf>,

    /// Octal permission mode for the socket parent directory (e.g. 700 or 0o700).
    #[arg(long, env = "CAIRN_DIR_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    dir_mode: Option<u32>,

    /// Octal permission mode for the socket file itself.
    #[arg(long, env = "CAIRN_SOCKET_MODE", value_parser = cairn_daemon::config::parse_octal_mode)]
    socket_mode: Option<u32>,

    /// How long to wait for sessions to exit on daemon shutdown (e.g. "5s", "500ms").
    #[arg(long, env = "CAIRN_SHUTDOWN_GRACE", value_parser = humantime::parse_duration)]
    shutdown_grace: Option<std::time::Duration>,

    /// Default shell for sessions that don't specify a command.
    #[arg(long, env = "CAIRN_DEFAULT_SHELL")]
    default_shell: Option<String>,

    /// `tracing-subscriber` filter directive.
    #[arg(long, env = "CAIRN_LOG", default_value = "info,cairn_daemon=info,cairn_pty=info")]
    log: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(args.log.clone()))
        .with_writer(std::io::stderr)
        .init();

    let mut cfg = cairn_daemon::config::DaemonConfig::default();
    if let Some(p) = args.socket {
        cfg.socket_path = p;
    }
    if let Some(m) = args.dir_mode {
        cfg.dir_mode = m;
    }
    if let Some(m) = args.socket_mode {
        cfg.socket_mode = m;
    }
    if let Some(g) = args.shutdown_grace {
        cfg.shutdown_grace = g;
    }
    if let Some(s) = args.default_shell {
        cfg.default_shell = s;
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async {
        let daemon = cairn_daemon::daemon::Daemon::new(cfg);
        let shutdown = CancellationToken::new();
        let sig = shutdown.clone();
        tokio::spawn(async move {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to install SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = term.recv() => {},
            }
            sig.cancel();
        });
        cairn_daemon::serve::serve(daemon, shutdown).await
    })
}

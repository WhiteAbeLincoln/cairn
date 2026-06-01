//! The cairn session-manager daemon entry point.
//!
//! Parses CLI flags (with `CAIRN_*` env overrides), initialises tracing to
//! stderr, builds a `Daemon`, sets up SIGTERM/ctrl-c shutdown via a
//! `CancellationToken`, and runs `serve()`.

use clap::Parser;
use tokio_util::sync::CancellationToken;

use cairn_daemon::config::{Args, DaemonConfig};

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // Hold the provider until process exit so OTLP spans are flushed.
    // Extract tracing args before the move into DaemonConfig.
    let _otel_provider = cairn_daemon::telemetry::init_tracing(&args.log, args.log_format)?;

    let cfg: DaemonConfig = args.into();
    cfg.warn_on_misconfig();

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

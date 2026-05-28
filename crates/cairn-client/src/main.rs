use clap::{CommandFactory, Parser};

mod attach;
mod cli;
mod connect;
mod detach;
mod exec;
mod signals;
mod terminal;

use attach::AttachOptions;
use cli::{Cli, Command, SessionTarget};
use connect::Endpoint;
use detach::DetachKeys;

fn main() -> anyhow::Result<()> {
    let args = Cli::parse();

    // Completion generation needs no runtime and no daemon.
    if let Command::Completion { shell } = args.command {
        let mut cmd = Cli::command();
        let bin_name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
        return Ok(());
    }

    init_tracing(args.verbose);

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let code = rt.block_on(dispatch(args))?;
    std::process::exit(code);
}

fn init_tracing(verbose: u8) {
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("CAIRN_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

async fn dispatch(cli: Cli) -> anyhow::Result<i32> {
    match &cli.command {
        Command::Attach { session, no_stdin, detach_keys } => {
            let endpoint = Endpoint::resolve(cli.daemon.as_deref())?;
            let id = resolve_target(&endpoint, session).await?;
            let opts = AttachOptions {
                no_stdin: *no_stdin,
                detach_keys: DetachKeys::parse_or_default(detach_keys.as_deref())
                    .map_err(|e| anyhow::anyhow!(e))?,
            };
            attach::run(&endpoint, &id, opts).await
        }
        Command::Exec(args) => exec::run_exec(&cli, args, false, false).await,
        Command::Run(args) => exec::run_exec(&cli, args, true, true).await,
        Command::Completion { .. } => Ok(0), // handled before the runtime
        _ => anyhow::bail!(
            "this command is not implemented yet; the interactive-attach milestone covers attach/exec/run"
        ),
    }
}

/// Resolve a single-session target to a concrete session id. `--latest` is
/// resolved client-side via `list_all` (it has no wire representation).
async fn resolve_target(endpoint: &Endpoint, target: &SessionTarget) -> anyhow::Result<String> {
    use cairn_protocol::client::cairn::daemon::sessions;
    if target.latest {
        let client = endpoint.client();
        let mut all = sessions::list_all(&client, ())
            .await
            .map_err(|e| anyhow::anyhow!("cannot reach cairn-daemon at {}: {e}", endpoint.label()))?;
        all.sort_by_key(|s| s.created_at_unix_ms);
        let latest = all.last().ok_or_else(|| anyhow::anyhow!("no sessions to attach to"))?;
        Ok(latest.id.clone())
    } else if let Some(s) = &target.session {
        Ok(s.clone())
    } else {
        anyhow::bail!("no session specified")
    }
}

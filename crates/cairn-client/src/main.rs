use clap::{CommandFactory, Parser};

mod attach;
mod cli;
mod connect;
mod detach;
mod exec;
mod inspect;
mod kick;
mod kill;
mod list;
mod logs;
mod meta;
mod rename;
mod restart;
mod send;
mod signals;
mod targets;
mod terminal;
mod wait;

use attach::AttachOptions;
use cli::{Cli, Command, SessionTarget};
use connect::{Client, Endpoint};
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

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
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
    // `Attach` keeps its own client lifecycle (reconnect loop), so it only
    // needs the endpoint. All other commands build the client once here and
    // share it, avoiding a redundant QUIC handshake on WebTransport.
    match &cli.command {
        Command::Attach {
            session,
            no_stdin,
            detach_keys,
        } => {
            let endpoint = Endpoint::resolve(cli.daemon.as_deref(), cli.cert_hash.clone())?;
            // attach needs a client for the initial target resolution but
            // builds its own inside the reconnect loop — resolve the target
            // with a dedicated client here.
            let client = endpoint.client().await?;
            let target = targets::resolve_one(&client, session).await?;
            let opts = AttachOptions {
                no_stdin: *no_stdin,
                detach_keys: DetachKeys::parse_or_default(detach_keys.as_deref())
                    .map_err(|e| anyhow::anyhow!(e))?,
                pty: target.info.spec.tty,
            };
            attach::run(&endpoint, &target.id, opts).await
        }
        Command::Exec(args) => {
            let endpoint = Endpoint::resolve(cli.daemon.as_deref(), cli.cert_hash.clone())?;
            let client = endpoint.client().await?;
            exec::run_exec(args, false, false, &endpoint, &client).await
        }
        Command::Run(args) => {
            let endpoint = Endpoint::resolve(cli.daemon.as_deref(), cli.cert_hash.clone())?;
            let client = endpoint.client().await?;
            exec::run_exec(args, true, true, &endpoint, &client).await
        }
        _ => {
            // All remaining commands share a single client.
            let endpoint = Endpoint::resolve(cli.daemon.as_deref(), cli.cert_hash.clone())?;
            let client = endpoint.client().await?;
            dispatch_with_client(&cli, &client).await
        }
    }
}

async fn dispatch_with_client(cli: &Cli, client: &Client) -> anyhow::Result<i32> {
    match &cli.command {
        Command::Whoami => meta::whoami(client).await,
        Command::Version => meta::version(client).await,
        Command::List => list::run(client).await,
        Command::Inspect { session } => inspect::run(client, session).await,
        Command::Rename { session, new_name } => rename::run(client, session, new_name).await,
        Command::Restart { session, force } => restart::run(client, session, *force).await,
        Command::Send { latest, raw, args } => {
            // Split the positional vector into (session-selector, input).
            // `required_unless_present = "latest"` on `args` guarantees
            // the non-`--latest` branch sees at least one element.
            let (target, input) = if *latest {
                (
                    SessionTarget {
                        session: None,
                        latest: true,
                    },
                    args.as_slice(),
                )
            } else {
                let (s, rest) = args
                    .split_first()
                    .expect("clap `required_unless_present(latest)` guarantees args is non-empty");
                (
                    SessionTarget {
                        session: Some(s.clone()),
                        latest: false,
                    },
                    rest,
                )
            };
            send::run(client, &target, *raw, input).await
        }
        Command::Kick {
            sessions,
            client: client_filter,
        } => kick::run(client, sessions, client_filter.as_deref()).await,
        Command::Kill {
            signal,
            no_wait,
            timeout,
            sessions,
        } => kill::run(client, sessions, *signal, *no_wait, *timeout).await,
        Command::Wait { session, timeout } => wait::run(client, session, *timeout).await,
        Command::Logs {
            sessions,
            strip,
            prefix,
            follow,
            tail,
        } => logs::run(client, sessions, *strip, *prefix, *follow, *tail).await,
        // These are handled in `dispatch` before reaching here.
        Command::Attach { .. }
        | Command::Exec(_)
        | Command::Run(_)
        | Command::Completion { .. } => Ok(0),
    }
}

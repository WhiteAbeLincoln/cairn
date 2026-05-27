use clap::{CommandFactory, Parser};

mod cli;
mod connect;
mod detach;
mod signals;
mod terminal;

fn main() -> anyhow::Result<()> {
    let args = cli::Cli::parse();

    // Handle completion generation up front: it produces output and
    // exits without needing to reach the daemon.
    if let cli::Command::Completion { shell } = args.command {
        let mut cmd = cli::Cli::command();
        let bin_name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, bin_name, &mut std::io::stdout());
        return Ok(());
    }

    eprintln!("args: {args:#?}");

    Ok(())
}

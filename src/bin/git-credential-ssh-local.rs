use std::fmt::Debug;

use clap::Parser as _;
use color_eyre::eyre;
use ssh_local_exec::args::ConnectAddress;

/// Git credential helper to retrieving and storing credentials on the SSH local host
#[derive(Debug, clap::Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(flatten)]
    connect_address: ConnectAddress,
    /// Git credential helper command to execute
    #[clap(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, clap::Subcommand)]
pub enum Command {
    /// Returns a matching credential from remote server, if any exists
    Get,
    /// Store the credential to remote server, if applicable to helper
    Store,
    /// Remove a matching credential from remote server, if any, from the helper's storage
    Erase,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let Args {
        connect_address,
        command: credential_command,
    } = Args::parse();

    let command = "git".to_string();
    let args = match credential_command {
        Command::Get => &["credential", "fill"],
        Command::Store => &["credential", "approve"],
        Command::Erase => &["credential", "reject"],
    };
    let args = args.iter().copied().map(String::from).collect();

    ssh_local_exec::client::main(&connect_address, command, args).await?;

    Ok(())
}

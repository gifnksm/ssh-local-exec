use std::{fmt::Debug, process::ExitCode};

use clap::Parser as _;
use color_eyre::eyre;
use ssh_local_exec::args::ConnectAddress;

/// Execute a command on the SSH local host
#[derive(Debug, clap::Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(flatten)]
    connect_address: ConnectAddress,
    /// Command to execute
    command: String,
    /// Command arguments
    args: Vec<String>,
}

#[tokio::main]
async fn main() -> eyre::Result<ExitCode> {
    color_eyre::install()?;
    ssh_local_exec::log::install()?;

    let Args {
        connect_address,
        command,
        args,
    } = Args::parse();

    let exit_code = ssh_local_exec::client::main(&connect_address, command, args).await?;

    Ok(exit_code)
}

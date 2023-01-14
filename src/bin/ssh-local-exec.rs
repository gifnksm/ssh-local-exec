use std::fmt::Debug;

use clap::Parser as _;
use color_eyre::eyre;
use ssh_local_exec::args::RemoteEndpoint;

/// Execute a command on the SSH local host
#[derive(Debug, clap::Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(flatten)]
    remote_endpoint: RemoteEndpoint,
    /// Command to execute
    command: String,
    /// Command arguments
    args: Vec<String>,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let Args {
        remote_endpoint,
        command,
        args,
    } = Args::parse();

    ssh_local_exec::client::main(&remote_endpoint, command, args).await?;

    Ok(())
}

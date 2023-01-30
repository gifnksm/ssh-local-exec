use clap::Parser as _;
use color_eyre::eyre;
use ssh_local_exec::args::ListenAddress;

/// Server for executing commands on the SSH local host
#[derive(Debug, clap::Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(flatten)]
    listen_address: ListenAddress,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    ssh_local_exec::log::install()?;

    let Args { listen_address } = Args::parse();

    ssh_local_exec::server::main(&listen_address).await?;

    Ok(())
}

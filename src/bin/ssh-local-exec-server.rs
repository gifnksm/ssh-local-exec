use clap::Parser as _;
use color_eyre::eyre;
use ssh_local_exec::args::LocalEndpoint;

/// Server for executing commands on the SSH local host
#[derive(Debug, clap::Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(flatten)]
    local_endpoint: LocalEndpoint,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    color_eyre::install()?;
    tracing_subscriber::fmt::init();

    let Args { local_endpoint } = Args::parse();

    ssh_local_exec::server::main(&local_endpoint).await?;

    Ok(())
}

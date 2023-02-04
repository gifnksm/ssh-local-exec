use std::io;

use color_eyre::eyre::eyre;
use tracing_error::ErrorLayer;
use tracing_subscriber::{prelude::*, EnvFilter};

pub fn install() -> color_eyre::eyre::Result<()> {
    use std::env;
    if env::var_os("RUST_LOG").is_none() {
        env::set_var("RUST_LOG", "info");
    }

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .with_target(false)
        .finish()
        .with(ErrorLayer::default());
    subscriber.try_init().map_err(|e| eyre!(e))?;

    Ok(())
}

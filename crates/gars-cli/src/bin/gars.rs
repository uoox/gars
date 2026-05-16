use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use gars_memory::{GarsPaths, migration_hint};
use gars_server::{ServerOptions, serve};

#[derive(Parser)]
#[command(
    name = "gars",
    version,
    about = "gars background service with local REST API"
)]
struct Cli {
    #[arg(short = 'c', long = "config", visible_alias = "c")]
    config: Option<PathBuf>,
    #[arg(long, env = "GARS_HOME")]
    home: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let cli = Cli::parse();
    let paths = GarsPaths::resolve(cli.home)?;
    if let Some(hint) = migration_hint(&paths) {
        eprintln!("{hint}");
    }
    paths.ensure()?;
    let config_path = cli.config.unwrap_or_else(|| paths.config.clone());
    serve(ServerOptions { paths, config_path }).await
}

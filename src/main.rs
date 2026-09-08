mod app;
mod bootstrap;
mod cli;
mod config;
mod mpv;
mod playlist;
mod sidebar;
mod ui;
mod utility;

use anyhow::Result;
use clap::Parser;
use tracing::{debug, info};

#[tokio::main]
async fn main() -> Result<()> {
    let _guard = bootstrap::init_tracing();
    info!("ytrs starting");
    let args = cli::Cli::parse();
    debug!(?args, "parsed CLI args");
    if let Some(mut app) = bootstrap::build_app_from_cli(&args) {
        info!("running from CLI subcommand");
        app.run().await?;
        return Ok(());
    }
    info!("no subcommand, entering interactive mode");
    bootstrap::run_interactive(args).await
}

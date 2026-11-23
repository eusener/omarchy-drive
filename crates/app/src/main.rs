mod cli;
mod daemon;
mod ipc;
mod providers;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("OMARCHY_DRIVE_LOG")
                .unwrap_or_else(|_| "info,omarchy_drive=info".into()),
        )
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    cli::run(cli::Cli::parse()).await
}

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
    let cli = cli::Cli::parse();
    if !cli.is_daemon() {
        // Comandos de terminal: comportamento Unix padrão (`status | head`
        // encerra em silêncio). O daemon mantém SIGPIPE ignorado para que um
        // cliente que feche o socket não o derrube.
        // SAFETY: chamado no início do processo, antes de qualquer escrita.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
    cli::run(cli).await
}

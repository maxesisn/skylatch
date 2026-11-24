use clap::Parser;
use skylatch_agent::{
    cli::{Cli, Command},
    hub,
};
use tracing_subscriber::EnvFilter;

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let mut cli = Cli::parse();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    if let Some(Command::Join(join)) = cli.command.take() {
        return join.run().await;
    }
    tokio::select! {
        result = hub::run(cli) => result?,
        _ = shutdown_signal() => tracing::info!("shutting down"),
    }
    Ok(())
}

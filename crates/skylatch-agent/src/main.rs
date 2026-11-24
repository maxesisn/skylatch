use clap::Parser;
use skylatch_agent::{
    cli::{Cli, Command},
    hub,
    service::ServiceAction,
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

fn log_to_stderr() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Cli { agent, command } = Cli::parse();
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    match command {
        Some(Command::Service(service)) => {
            // `service run` logs to a file next to its configuration.
            if !matches!(service.action, ServiceAction::Run(_)) {
                log_to_stderr();
            }
            service.run()
        }
        command => {
            log_to_stderr();
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    if let Some(Command::Join(join)) = command {
                        return join.run().await;
                    }
                    tokio::select! {
                        result = hub::run(agent) => result?,
                        _ = shutdown_signal() => tracing::info!("shutting down"),
                    }
                    Ok(())
                })
        }
    }
}

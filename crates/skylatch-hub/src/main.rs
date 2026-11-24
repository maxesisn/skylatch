use clap::Parser;
use skylatch_hub::{router, AppState, HubConfig};
use std::net::SocketAddr;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

/// Skylatch relay hub. Terminate TLS in front of it (reverse proxy) to serve WSS on 443.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Address to listen on.
    #[arg(long, env = "SKYLATCH_LISTEN", default_value = "127.0.0.1:8080")]
    listen: SocketAddr,

    /// Account as `name=token`. Repeat or comma-separate for several accounts.
    #[arg(
        long = "account",
        env = "SKYLATCH_ACCOUNTS",
        value_delimiter = ',',
        required = true
    )]
    accounts: Vec<String>,

    /// Heartbeat interval announced to hosts, in seconds.
    #[arg(long, env = "SKYLATCH_HEARTBEAT_SECS", default_value_t = 15)]
    heartbeat_secs: u64,
}

fn parse_account(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((name, token)) if !name.is_empty() && !token.is_empty() => {
            Ok((name.to_string(), token.to_string()))
        }
        _ => Err(format!(
            "invalid account `{}`: expected name=token",
            s.split('=').next().unwrap_or("")
        )),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args = Args::parse();
    let accounts = args
        .accounts
        .iter()
        .map(|a| parse_account(a))
        .collect::<Result<Vec<_>, _>>()?;
    let mut config = HubConfig::new(accounts);
    config.heartbeat_interval = Duration::from_secs(args.heartbeat_secs.max(1));
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!(addr = %listener.local_addr()?, "skylatch-hub listening");
    axum::serve(listener, router(AppState::new(config)))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

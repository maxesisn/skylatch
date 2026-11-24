use clap::Parser;
use skylatch_hub::{router, AppState, HubConfig, HubMode};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

/// Skylatch relay hub. Terminate TLS in front of it (reverse proxy) to serve WSS on 443.
#[derive(Parser)]
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
        hide_env_values = true
    )]
    accounts: Vec<String>,

    /// Password login account as `username=password`. Repeat the flag for several users.
    /// Alternatively set the comma-delimited `SKYLATCH_USERS` environment variable.
    #[arg(long = "user")]
    users: Vec<String>,

    /// Store password hashes, client sessions, and revocable device credentials as private JSON.
    #[arg(long, env = "SKYLATCH_STATE_FILE")]
    state_file: Option<PathBuf>,

    /// Heartbeat interval announced to hosts, in seconds.
    #[arg(long, env = "SKYLATCH_HEARTBEAT_SECS", default_value_t = 15)]
    heartbeat_secs: u64,

    /// `relay` carries session traffic through the hub; `signaling-only` only issues direct-connect credentials and hints.
    #[arg(long, env = "SKYLATCH_HUB_MODE", default_value = "relay")]
    mode: HubMode,
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
    let users = if args.users.is_empty() {
        match std::env::var("SKYLATCH_USERS") {
            Ok(raw) if !raw.is_empty() => raw.split(',').map(str::to_owned).collect(),
            Ok(_) | Err(std::env::VarError::NotPresent) => vec![],
            Err(err) => return Err(err.into()),
        }
    } else {
        args.users
    };
    let users = users
        .iter()
        .map(|s| {
            s.split_once('=')
                .filter(|(name, password)| !name.is_empty() && !password.is_empty())
                .map(|(name, password)| (name.into(), password.into()))
                .ok_or("invalid --user: expected username=password")
        })
        .collect::<Result<Vec<_>, _>>()?;
    if accounts.is_empty()
        && users.is_empty()
        && !args.state_file.as_ref().is_some_and(|path| path.is_file())
    {
        return Err("configure --account, --user, or an existing --state-file".into());
    }
    let mut config = HubConfig::new(accounts);
    config.heartbeat_interval = Duration::from_secs(args.heartbeat_secs.max(1));
    config.mode = args.mode;
    let state = AppState::with_auth(config, args.state_file, users)?;
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!(addr = %listener.local_addr()?, mode = args.mode.as_str(), "skylatch-hub listening");
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

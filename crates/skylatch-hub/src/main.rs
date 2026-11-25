use clap::Parser;
use skylatch_hub::{router, AppState, HubConfig, HubMode};
use skylatch_proto::mesh::is_valid_peer_url;
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

    /// EasyTier peer URL announced by `/api/v1/info`, e.g. `tcp://public.example.com:11010`. Repeat or comma-separate.
    #[arg(long = "easytier-peer", env = "SKYLATCH_EASYTIER_PEERS", value_delimiter = ',', value_parser = parse_peer)]
    easytier_peers: Vec<String>,

    /// Let anyone create a password account with `POST /api/v1/signup`.
    #[arg(long, env = "SKYLATCH_ALLOW_SIGNUP")]
    allow_signup: bool,

    /// Most distinct devices one account may enroll or register. Unlimited when unset.
    #[arg(long, env = "SKYLATCH_MAX_DEVICES_PER_ACCOUNT", value_parser = clap::value_parser!(u32).range(1..))]
    max_devices_per_account: Option<u32>,
}

fn parse_peer(s: &str) -> Result<String, String> {
    let peer = s.trim();
    if is_valid_peer_url(peer) {
        Ok(peer.to_string())
    } else {
        Err(format!(
            "invalid EasyTier peer `{peer}`: expected scheme://host:port"
        ))
    }
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
        && !args.allow_signup
        && !args.state_file.as_ref().is_some_and(|path| path.is_file())
    {
        return Err(
            "configure --account, --user, --allow-signup, or an existing --state-file".into(),
        );
    }
    let mut config = HubConfig::new(accounts);
    config.heartbeat_interval = Duration::from_secs(args.heartbeat_secs.max(1));
    config.mode = args.mode;
    config.easytier_peers = args.easytier_peers;
    config.allow_signup = args.allow_signup;
    config.max_devices_per_account = args.max_devices_per_account;
    let state = AppState::with_auth(config, args.state_file, users)?;
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    tracing::info!(addr = %listener.local_addr()?, mode = args.mode.as_str(), "skylatch-hub listening");
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

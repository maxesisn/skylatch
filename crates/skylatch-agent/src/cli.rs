//! Command line and derived configuration.

use crate::join::{config_path, load, Join};
use clap::{ArgGroup, Parser, Subcommand, ValueEnum};
use skylatch_proto::control::{DeviceFormFactor, DirectEndpoint, EndpointKind};
use skylatch_proto::credentials::is_token;
use std::path::PathBuf;
use url::Url;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum FormFactorArg {
    Auto,
    Desktop,
    Laptop,
    Mini,
    Vm,
    Server,
}

impl FormFactorArg {
    pub fn resolve(self, detected: DeviceFormFactor) -> DeviceFormFactor {
        match self {
            Self::Auto => detected,
            Self::Desktop => DeviceFormFactor::Desktop,
            Self::Laptop => DeviceFormFactor::Laptop,
            Self::Mini => DeviceFormFactor::Mini,
            Self::Vm => DeviceFormFactor::Vm,
            Self::Server => DeviceFormFactor::Server,
        }
    }
}

/// Skylatch host agent.
///
/// Keeps one outbound WebSocket to `skylatch-hub` and, when a client opens a session, bridges
/// its `NSL1` stream to the local `skylatch-host-engine`.
#[derive(Parser)]
#[command(version, about, args_conflicts_with_subcommands = true)]
#[command(group(ArgGroup::new("engine").args(["engine_bin", "engine_url"])))]
pub struct Cli {
    /// Hub URL: `wss://hub.example.com/ws/host`, or a base URL such as `ws://127.0.0.1:8080`
    /// (`/ws/host` is appended when the path is empty).
    #[arg(long, env = "SKYLATCH_HUB", value_parser = parse_hub_url)]
    pub hub: Option<Url>,

    /// Account or device token.
    #[arg(long, env = "SKYLATCH_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Stable device id (default: derived from the hostname).
    #[arg(long, env = "SKYLATCH_DEVICE_ID")]
    pub device_id: Option<String>,

    /// Display name (default: the hostname).
    #[arg(long, env = "SKYLATCH_DEVICE_NAME")]
    pub name: Option<String>,

    /// Override the detected hardware form factor.
    #[arg(long, value_enum, default_value = "auto")]
    pub form_factor: FormFactorArg,

    /// Physical room or workstation group.
    #[arg(long, value_name = "NAME")]
    pub room: Option<String>,

    /// Stable desk or rack slot index.
    #[arg(long, value_name = "N")]
    pub slot: Option<u32>,

    /// Path to `skylatch-host-engine`. The agent starts it on demand, bound to loopback only.
    #[arg(long, env = "SKYLATCH_ENGINE_BIN")]
    pub engine_bin: Option<PathBuf>,

    /// Attach to an already running local engine, e.g. `ws://127.0.0.1:17900`.
    #[arg(long, value_parser = parse_engine_url)]
    pub engine_url: Option<Url>,

    /// Loopback port for the engine started with `--engine-bin` (default: an ephemeral port).
    #[arg(long, requires = "engine_bin")]
    pub engine_port: Option<u16>,

    /// Direct endpoint advertised to clients as `lan=host:port` or `overlay=host:port`. Repeatable.
    #[arg(long = "direct-endpoint", value_name = "KIND=HOST:PORT", value_parser = parse_endpoint)]
    pub direct_endpoints: Vec<DirectEndpoint>,

    /// Listen for authenticated NSL1 connections. Port 0 disables unless --enable-direct is set.
    #[arg(long, default_value_t = 0)]
    pub direct_port: u16,

    /// Enable direct connections, using an ephemeral port when --direct-port is not set.
    #[arg(long)]
    pub enable_direct: bool,

    /// Treat --token as a per-device token for direct authentication, never as an account token.
    #[arg(long, requires = "token")]
    pub device_token: bool,

    /// Capture frame rate passed to the engine (10-120).
    #[arg(long, requires = "engine_bin", value_parser = clap::value_parser!(u32).range(10..=120))]
    pub fps: Option<u32>,

    /// Minimum bitrate in bits per second passed to the engine.
    #[arg(long, requires = "engine_bin")]
    pub bitrate_min: Option<u64>,

    /// Starting bitrate in bits per second passed to the engine.
    #[arg(long, requires = "engine_bin")]
    pub bitrate_start: Option<u64>,

    /// Maximum bitrate in bits per second passed to the engine.
    #[arg(long, requires = "engine_bin")]
    pub bitrate_max: Option<u64>,

    /// Capture backend passed to the engine (for example `x11`, `wayland`, `macos`, `windows`).
    #[arg(long, requires = "engine_bin")]
    pub backend: Option<String>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Enroll this workstation and save its private device credentials.
    Join(Join),
}

pub(crate) struct DaemonCredentials {
    pub hub: Url,
    pub token: String,
    pub device_token: bool,
}

impl Cli {
    pub(crate) fn resolve(&mut self) -> Result<DaemonCredentials, Box<dyn std::error::Error>> {
        let saved = if self.hub.is_some() && self.token.is_some() {
            None
        } else {
            match config_path() {
                Ok(path) => load(&path)?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                Err(err) => return Err(err.into()),
            }
        };
        let mut from_disk = false;
        if let Some(saved) = saved {
            let saved_hub = parse_hub_url(&saved.hub_url)?;
            if let Some(hub) = &self.hub {
                if hub != &saved_hub && self.token.is_none() {
                    return Err("an explicit --token is required for a different hub".into());
                }
            }
            self.hub.get_or_insert(saved_hub);
            if self.token.is_none() {
                if !is_token(&saved.device_token) {
                    return Err("invalid saved device credentials; run skylatch-agent join".into());
                }
                if self
                    .device_id
                    .as_ref()
                    .is_some_and(|id| id != &saved.device_id)
                {
                    return Err(
                        "a saved device token cannot be used for a different --device-id".into(),
                    );
                }
                from_disk = true;
                self.token = Some(saved.device_token);
                self.device_id = Some(saved.device_id);
            }
            self.name = self.name.take().or(Some(saved.name));
            self.room = self.room.take().or(saved.room);
            self.slot = self.slot.or(saved.slot);
        }
        let hub = self
            .hub
            .take()
            .ok_or("provide --hub or run skylatch-agent join")?;
        let token = self
            .token
            .take()
            .ok_or("provide --token or run skylatch-agent join")?;
        if self.device_token && !is_token(&token) {
            return Err("a device token must have 64 hex digits".into());
        }
        Ok(DaemonCredentials {
            hub,
            token,
            device_token: from_disk || self.device_token,
        })
    }

    /// Device id, falling back to a sanitised hostname.
    pub fn resolved_device_id(&self) -> String {
        match &self.device_id {
            Some(id) => id.clone(),
            None => sanitize_id(&hostname()),
        }
    }

    pub fn resolved_name(&self) -> String {
        self.name.clone().unwrap_or_else(hostname)
    }

    /// Extra arguments for the engine process (everything except bind/port).
    pub fn engine_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        let mut push = |flag: &str, value: Option<String>| {
            if let Some(v) = value {
                args.push(flag.to_string());
                args.push(v);
            }
        };
        push("--fps", self.fps.map(|v| v.to_string()));
        push("--bitrate-min", self.bitrate_min.map(|v| v.to_string()));
        push("--bitrate-start", self.bitrate_start.map(|v| v.to_string()));
        push("--bitrate-max", self.bitrate_max.map(|v| v.to_string()));
        push("--backend", self.backend.clone());
        args
    }
}

pub(crate) fn hostname() -> String {
    let h = gethostname::gethostname()
        .to_string_lossy()
        .trim()
        .to_string();
    if h.is_empty() {
        "skylatch-host".into()
    } else {
        h
    }
}

/// Lowercases and replaces anything outside `[a-z0-9._-]` with `-`.
pub fn sanitize_id(raw: &str) -> String {
    let id: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if id.is_empty() {
        "skylatch-host".into()
    } else {
        id
    }
}

/// Parses the hub URL, appending `/ws/host` when no path is given.
pub fn parse_hub_url(raw: &str) -> Result<Url, String> {
    let mut url = Url::parse(raw).map_err(|e| format!("invalid hub URL: {e}"))?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err("hub URL must start with ws:// or wss://".into());
    }
    if url.host_str().is_none() {
        return Err("hub URL has no host".into());
    }
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/ws/host");
    }
    Ok(url)
}

fn parse_engine_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid engine URL: {e}"))?;
    if url.scheme() != "ws" || url.host_str().is_none() {
        return Err("engine URL must look like ws://127.0.0.1:PORT".into());
    }
    Ok(url)
}

/// Parses `lan=host:port` / `overlay=host:port`.
pub fn parse_endpoint(raw: &str) -> Result<DirectEndpoint, String> {
    let (kind, addr) = raw
        .split_once('=')
        .ok_or("expected KIND=HOST:PORT, e.g. lan=192.0.2.10:17900")?;
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "lan" => EndpointKind::Lan,
        "overlay" => EndpointKind::Overlay,
        other => {
            return Err(format!(
                "unknown endpoint kind `{other}` (use lan or overlay)"
            ))
        }
    };
    let (host, port) = addr
        .trim()
        .rsplit_once(':')
        .ok_or("expected HOST:PORT after `=`")?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return Err("endpoint host is empty".into());
    }
    let port: u16 = port.parse().map_err(|_| "invalid endpoint port")?;
    if port == 0 {
        return Err("endpoint port must be nonzero".into());
    }
    Ok(DirectEndpoint {
        kind,
        host: host.to_string(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_url_gets_default_path() {
        assert_eq!(
            parse_hub_url("ws://127.0.0.1:8080").unwrap().as_str(),
            "ws://127.0.0.1:8080/ws/host"
        );
        assert_eq!(
            parse_hub_url("wss://hub.example.com/").unwrap().as_str(),
            "wss://hub.example.com/ws/host"
        );
        assert_eq!(
            parse_hub_url("wss://hub.example.com/custom/host")
                .unwrap()
                .path(),
            "/custom/host"
        );
        assert!(parse_hub_url("http://hub.example.com").is_err());
        assert!(parse_hub_url("nonsense").is_err());
    }

    #[test]
    fn endpoints_parse() {
        let e = parse_endpoint("lan=192.0.2.10:17900").unwrap();
        assert_eq!(e.kind, EndpointKind::Lan);
        assert_eq!((e.host.as_str(), e.port), ("192.0.2.10", 17900));
        let e = parse_endpoint("overlay=[2001:db8::3]:17900").unwrap();
        assert_eq!(e.kind, EndpointKind::Overlay);
        assert_eq!(e.host, "2001:db8::3");
        assert!(parse_endpoint("wan=192.0.2.4:5").is_err());
        assert!(parse_endpoint("lan=192.0.2.4").is_err());
        assert!(parse_endpoint("lan=192.0.2.4:0").is_err());
        assert!(parse_endpoint("lan=:80").is_err());
    }

    #[test]
    fn ids_are_sanitised() {
        assert_eq!(sanitize_id("Demo MacBook Pro"), "demo-macbook-pro");
        assert_eq!(sanitize_id("  "), "skylatch-host");
    }
}

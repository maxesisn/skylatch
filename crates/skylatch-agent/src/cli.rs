//! Command line and derived configuration.

use clap::{ArgGroup, Parser};
use skylatch_proto::control::{DirectEndpoint, EndpointKind};
use std::path::PathBuf;
use url::Url;

/// Skylatch host agent.
///
/// Keeps one outbound WebSocket to `skylatch-hub` and, when a client opens a session, bridges
/// its `NSL1` stream to the local `skylatch-host-engine`.
#[derive(Debug, Clone, Parser)]
#[command(version, about)]
#[command(group(ArgGroup::new("engine").required(true).args(["engine_bin", "engine_url"])))]
pub struct Cli {
    /// Hub URL: `wss://hub.example.com/ws/host`, or a base URL such as `ws://127.0.0.1:8080`
    /// (`/ws/host` is appended when the path is empty).
    #[arg(long, env = "SKYLATCH_HUB", value_parser = parse_hub_url)]
    pub hub: Url,

    /// Account token.
    #[arg(long, env = "SKYLATCH_TOKEN", hide_env_values = true)]
    pub token: String,

    /// Stable device id (default: derived from the hostname).
    #[arg(long, env = "SKYLATCH_DEVICE_ID")]
    pub device_id: Option<String>,

    /// Display name (default: the hostname).
    #[arg(long, env = "SKYLATCH_DEVICE_NAME")]
    pub name: Option<String>,

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
}

impl Cli {
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

fn hostname() -> String {
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
        .ok_or("expected KIND=HOST:PORT, e.g. lan=192.168.1.10:17900")?;
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
        let e = parse_endpoint("lan=192.168.1.10:17900").unwrap();
        assert_eq!(e.kind, EndpointKind::Lan);
        assert_eq!((e.host.as_str(), e.port), ("192.168.1.10", 17900));
        let e = parse_endpoint("overlay=[fd00::3]:17900").unwrap();
        assert_eq!(e.kind, EndpointKind::Overlay);
        assert_eq!(e.host, "fd00::3");
        assert!(parse_endpoint("wan=1.2.3.4:5").is_err());
        assert!(parse_endpoint("lan=1.2.3.4").is_err());
        assert!(parse_endpoint("lan=1.2.3.4:0").is_err());
        assert!(parse_endpoint("lan=:80").is_err());
    }

    #[test]
    fn ids_are_sanitised() {
        assert_eq!(sanitize_id("Max's MacBook Pro"), "max-s-macbook-pro");
        assert_eq!(sanitize_id("  "), "skylatch-host");
    }
}

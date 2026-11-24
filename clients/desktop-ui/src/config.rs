// SPDX-License-Identifier: Apache-2.0

//! Persisted local configuration plus command-line / environment overrides.
//!
//! The JSON file lives in the platform's per-user config directory:
//!
//! | OS      | Path                                                  |
//! | ------- | ----------------------------------------------------- |
//! | Windows | `%APPDATA%\Skylatch\config.json`                      |
//! | macOS   | `~/Library/Application Support/Skylatch/config.json`  |
//! | Linux   | `$XDG_CONFIG_HOME/skylatch/config.json` (`~/.config`) |
//!
//! Precedence at start-up: command-line flags > environment > file. The overrides are never
//! written back to the file; only the in-window settings drawer and the theme / view toggles
//! persist.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// What is stored on disk.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hub_url: String,
    pub token: String,
    pub prefer_direct: bool,
    pub dark_mode: bool,
    pub shelf_mode: bool,
    /// Let other devices take over this machine: the launcher enrolls it as a host and keeps a
    /// local `skylatch-agent` running while a hub is configured.
    pub host_enabled: bool,
    /// Room this machine joins when the launcher enrolls it (empty: the hub's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_room: Option<String>,
    /// Optional absolute path of the native stream client.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_bin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            hub_url: String::new(),
            token: String::new(),
            prefer_direct: true,
            dark_mode: false,
            shelf_mode: false,
            host_enabled: true,
            host_room: None,
            client_bin: None,
        }
    }
}

// The token must never end up in logs or panic messages.
impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("hub_url", &self.hub_url)
            .field(
                "token",
                &if self.token.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .field("prefer_direct", &self.prefer_direct)
            .field("dark_mode", &self.dark_mode)
            .field("shelf_mode", &self.shelf_mode)
            .field("host_enabled", &self.host_enabled)
            .field("host_room", &self.host_room)
            .field("client_bin", &self.client_bin)
            .finish()
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Parse(serde_json::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(e) => write!(f, "config I/O error: {e}"),
            ConfigError::Parse(e) => write!(f, "config file is not valid JSON: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    Windows,
    MacOs,
    Linux,
}

impl Platform {
    pub fn current() -> Platform {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Linux
        }
    }
}

/// Environment lookup abstraction so precedence rules are testable without touching the
/// process environment.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// Config file location for `platform`, or `None` when the home directory is unknown.
pub fn path_for(platform: Platform, env: Env) -> Option<PathBuf> {
    match platform {
        Platform::Windows => {
            env("APPDATA").map(|d| PathBuf::from(d).join("Skylatch").join("config.json"))
        }
        Platform::MacOs => env("HOME").map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("Skylatch")
                .join("config.json")
        }),
        Platform::Linux => env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|d| d.join("skylatch").join("config.json")),
    }
}

pub fn default_path() -> Option<PathBuf> {
    path_for(Platform::current(), &process_env)
}

/// Reads the file; a missing file is the default configuration.
pub fn load(path: &Path) -> Result<Config, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(ConfigError::Parse),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(ConfigError::Io(e)),
    }
}

/// Writes the file atomically (temporary file + rename). On Unix the file is private to the
/// user because it holds the hub token.
pub fn save(path: &Path, cfg: &Config) -> Result<(), ConfigError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(ConfigError::Io)?;
    }
    let json = serde_json::to_string_pretty(cfg).map_err(ConfigError::Parse)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(ConfigError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .map_err(ConfigError::Io)?;
    }
    std::fs::rename(&tmp, path).map_err(ConfigError::Io)
}

// ───────────────────────────── command line ─────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cli {
    pub hub: Option<String>,
    pub token: Option<String>,
    pub demo: bool,
    pub dark: bool,
    pub shelf: bool,
    pub help: bool,
    pub version: bool,
}

pub fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{what} needs a value"))
        };
        match name {
            "-h" | "--help" => cli.help = true,
            "-V" | "--version" => cli.version = true,
            "--demo" => cli.demo = true,
            "--dark" => cli.dark = true,
            "--shelf" => cli.shelf = true,
            "--hub" => cli.hub = Some(value("--hub")?),
            "--token" => cli.token = Some(value("--token")?),
            other => return Err(format!("unknown option `{other}` (see --help)")),
        }
    }
    Ok(cli)
}

// ───────────────────────────── effective settings ─────────────────────────────

/// Start-up settings after applying flags and environment on top of the file.
#[derive(Clone, PartialEq)]
pub struct Effective {
    pub hub_url: String,
    pub token: String,
    pub prefer_direct: bool,
    pub dark: bool,
    pub shelf: bool,
    pub client_bin: Option<String>,
    /// `--demo`: show the built-in fleet even when a hub is configured. This flag is the only
    /// way to get demo data.
    pub demo: bool,
}

impl fmt::Debug for Effective {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Effective")
            .field("hub_url", &self.hub_url)
            .field(
                "token",
                &if self.token.is_empty() {
                    ""
                } else {
                    "<redacted>"
                },
            )
            .field("prefer_direct", &self.prefer_direct)
            .field("dark", &self.dark)
            .field("shelf", &self.shelf)
            .field("client_bin", &self.client_bin)
            .field("demo", &self.demo)
            .finish()
    }
}

impl Effective {
    /// A hub is configured and live polling should run.
    pub fn live(&self) -> bool {
        !self.demo && !self.hub_url.trim().is_empty() && !self.token.is_empty()
    }

    /// Neither a hub nor `--demo`: the first-run onboarding is shown, never a fake fleet.
    pub fn unconfigured(&self) -> bool {
        !self.demo && !self.live()
    }

    /// The built-in demo fleet (`--demo`).
    #[cfg(test)]
    pub fn demo_only() -> Effective {
        Effective {
            demo: true,
            ..Effective::from_config(&Config::default())
        }
    }

    pub fn from_config(cfg: &Config) -> Effective {
        Effective {
            hub_url: cfg.hub_url.clone(),
            token: cfg.token.clone(),
            prefer_direct: cfg.prefer_direct,
            dark: cfg.dark_mode,
            shelf: cfg.shelf_mode,
            client_bin: cfg.client_bin.clone(),
            demo: false,
        }
    }
}

pub fn resolve(cfg: &Config, cli: &Cli, env: Env) -> Effective {
    let mut e = Effective::from_config(cfg);
    if let Some(h) = cli.hub.clone().or_else(|| env("SKYLATCH_HUB")) {
        e.hub_url = h;
    }
    if let Some(t) = cli.token.clone().or_else(|| env("SKYLATCH_TOKEN")) {
        e.token = t;
    }
    if let Some(c) = env("SKYLATCH_CLIENT") {
        e.client_bin = Some(c);
    }
    e.dark |= cli.dark;
    e.shelf |= cli.shelf;
    e.demo = cli.demo;
    e
}

// ───────────────────────────── hub URL helpers ─────────────────────────────

/// Turns what a user types into a `ws://` / `wss://` base URL: `https://` becomes `wss://`,
/// `http://` becomes `ws://`, a bare host defaults to `wss://`, trailing slashes are dropped.
pub fn normalize_hub_url(input: &str) -> String {
    let s = input.trim();
    let url = if let Some(rest) = s.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = s.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if s.starts_with("wss://") || s.starts_with("ws://") {
        s.to_string()
    } else if s.is_empty() {
        String::new()
    } else {
        format!("wss://{s}")
    };
    url.trim_end_matches('/').to_string()
}

/// `true` when `url` (already normalised) names a host.
pub fn hub_url_valid(url: &str) -> bool {
    url.strip_prefix("wss://")
        .or_else(|| url.strip_prefix("ws://"))
        .is_some_and(|rest| !rest.trim_matches('/').is_empty())
}

/// Short `host[:port]` form for the header pill.
pub fn hub_host(url: &str) -> String {
    let rest = url
        .trim_start_matches("wss://")
        .trim_start_matches("ws://")
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = rest.split('/').next().unwrap_or("");
    if host.chars().count() > 26 {
        let head: String = host.chars().take(25).collect();
        format!("{head}…")
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn sample() -> Config {
        Config {
            hub_url: "wss://hub.example.com".into(),
            token: "secret-token".into(),
            prefer_direct: false,
            dark_mode: true,
            shelf_mode: true,
            host_enabled: false,
            host_room: Some("办公室".into()),
            client_bin: Some("/opt/skylatch/skylatch-client".into()),
        }
    }

    #[test]
    fn config_json_round_trip() {
        let cfg = sample();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        for key in [
            "hub_url",
            "token",
            "prefer_direct",
            "dark_mode",
            "shelf_mode",
            "host_enabled",
            "host_room",
            "client_bin",
        ] {
            assert!(
                json.contains(&format!("\"{key}\"")),
                "{key} missing in {json}"
            );
        }
        assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), cfg);
        // optional override is omitted when unset
        let plain = serde_json::to_string(&Config::default()).unwrap();
        assert!(!plain.contains("client_bin"));
        assert!(!plain.contains("host_room"), "an unset room is omitted");
        assert_eq!(
            serde_json::from_str::<Config>(&plain).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let cfg: Config = serde_json::from_str(r#"{"hub_url":"wss://h.example"}"#).unwrap();
        assert_eq!(cfg.hub_url, "wss://h.example");
        assert!(cfg.prefer_direct, "prefer_direct defaults to true");
        assert!(cfg.host_enabled, "hosting this machine defaults to on");
        assert!(
            !cfg.dark_mode && !cfg.shelf_mode && cfg.token.is_empty() && cfg.client_bin.is_none()
        );
    }

    #[test]
    fn save_and_load_use_a_file() {
        let dir = std::env::temp_dir().join(format!(
            "skylatch-ui-cfg-{}-{}",
            std::process::id(),
            line!()
        ));
        let path = dir.join("nested").join("config.json");
        assert_eq!(
            load(&path).unwrap(),
            Config::default(),
            "missing file is the default config"
        );
        save(&path, &sample()).unwrap();
        assert_eq!(load(&path).unwrap(), sample());
        // overwriting works and leaves no temporary file behind
        let mut next = sample();
        next.dark_mode = false;
        save(&path, &next).unwrap();
        assert_eq!(load(&path).unwrap(), next);
        assert!(!path.with_extension("json.tmp").exists());
        std::fs::write(&path, "{not json").unwrap();
        assert!(matches!(load(&path), Err(ConfigError::Parse(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_enabled_is_persisted_and_defaults_on() {
        assert!(Config::default().host_enabled);
        let dir = std::env::temp_dir().join(format!("skylatch-ui-host-cfg-{}", std::process::id()));
        let path = dir.join("config.json");
        let off = Config {
            host_enabled: false,
            ..Config::default()
        };
        save(&path, &off).unwrap();
        assert!(!load(&path).unwrap().host_enabled);
        save(&path, &Config::default()).unwrap();
        assert!(load(&path).unwrap().host_enabled);
        // a config written before this setting existed keeps hosting on
        std::fs::write(&path, r#"{"hub_url":"wss://h.example","token":"t"}"#).unwrap();
        assert!(load(&path).unwrap().host_enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_room_is_persisted_and_optional() {
        assert_eq!(Config::default().host_room, None);
        let dir = std::env::temp_dir().join(format!("skylatch-ui-room-cfg-{}", std::process::id()));
        let path = dir.join("config.json");
        let cfg = Config {
            host_room: Some("客厅工作台".into()),
            ..Config::default()
        };
        save(&path, &cfg).unwrap();
        assert_eq!(
            load(&path).unwrap().host_room.as_deref(),
            Some("客厅工作台")
        );
        // a config written before this setting existed has no room
        std::fs::write(&path, r#"{"hub_url":"wss://h.example","token":"t"}"#).unwrap();
        assert_eq!(load(&path).unwrap().host_room, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let text = format!("{:?}", sample());
        assert!(!text.contains("secret-token") && text.contains("<redacted>"));
        let eff = resolve(&sample(), &Cli::default(), &|_| None);
        assert!(!format!("{eff:?}").contains("secret-token"));
    }

    #[test]
    fn config_paths_follow_each_platform_convention() {
        let win = env_of(&[("APPDATA", r"D:\Roaming")]);
        let p = path_for(Platform::Windows, &|k| win.get(k).cloned()).unwrap();
        assert!(p.ends_with(Path::new("Skylatch").join("config.json")));
        assert!(p.starts_with(r"D:\Roaming"));

        let mac = env_of(&[("HOME", "/mock/user")]);
        let p = path_for(Platform::MacOs, &|k| mac.get(k).cloned()).unwrap();
        assert_eq!(
            p,
            PathBuf::from("/mock/user/Library/Application Support/Skylatch/config.json")
        );

        let xdg = env_of(&[("XDG_CONFIG_HOME", "/tmp/xdg"), ("HOME", "/mock/home")]);
        let p = path_for(Platform::Linux, &|k| xdg.get(k).cloned()).unwrap();
        assert_eq!(p, PathBuf::from("/tmp/xdg/skylatch/config.json"));
        let home = env_of(&[("HOME", "/mock/home")]);
        let p = path_for(Platform::Linux, &|k| home.get(k).cloned()).unwrap();
        assert_eq!(p, PathBuf::from("/mock/home/.config/skylatch/config.json"));
        assert_eq!(path_for(Platform::Linux, &|_| None), None);
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn command_line_flags_parse() {
        let cli = parse_args(&args(&[
            "--hub",
            "wss://a.example",
            "--token=abc",
            "--demo",
            "--dark",
            "--shelf",
        ]))
        .unwrap();
        assert_eq!(cli.hub.as_deref(), Some("wss://a.example"));
        assert_eq!(cli.token.as_deref(), Some("abc"));
        assert!(cli.demo && cli.dark && cli.shelf && !cli.help && !cli.version);
        assert!(parse_args(&args(&["-h"])).unwrap().help);
        assert!(parse_args(&args(&["--version"])).unwrap().version);
        assert!(parse_args(&args(&["--hub"])).is_err(), "missing value");
        assert!(
            parse_args(&args(&["--bogus"]))
                .unwrap_err()
                .contains("--bogus")
        );
    }

    #[test]
    fn precedence_is_flags_then_environment_then_file() {
        let file = sample();
        let env = env_of(&[
            ("SKYLATCH_HUB", "wss://env.example"),
            ("SKYLATCH_TOKEN", "env-token"),
            ("SKYLATCH_CLIENT", "/env/client"),
        ]);
        let lookup = |k: &str| env.get(k).cloned();

        let from_env = resolve(&file, &Cli::default(), &lookup);
        assert_eq!(from_env.hub_url, "wss://env.example");
        assert_eq!(from_env.token, "env-token");
        assert_eq!(from_env.client_bin.as_deref(), Some("/env/client"));
        assert!(
            !from_env.prefer_direct && from_env.dark && from_env.shelf,
            "file values stay for the rest"
        );

        let cli = Cli {
            hub: Some("wss://flag.example".into()),
            token: Some("flag-token".into()),
            ..Cli::default()
        };
        let from_flags = resolve(&file, &cli, &lookup);
        assert_eq!(
            (from_flags.hub_url.as_str(), from_flags.token.as_str()),
            ("wss://flag.example", "flag-token")
        );

        let from_file = resolve(&file, &Cli::default(), &|_| None);
        assert_eq!(from_file.hub_url, "wss://hub.example.com");
        assert_eq!(from_file.client_bin, file.client_bin);
        assert!(from_file.live());
    }

    #[test]
    fn live_needs_hub_and_token_and_demo_flag_wins() {
        let mut eff = Effective::from_config(&sample());
        assert!(eff.live());
        eff.demo = true;
        assert!(!eff.live(), "--demo forces the built-in fleet");
        assert!(!Effective::demo_only().live());
        assert!(Effective::demo_only().demo && !Effective::demo_only().unconfigured());
        assert!(
            Effective::from_config(&Config::default()).unconfigured(),
            "no hub and no --demo: onboarding, not a fake fleet"
        );
        assert!(!Effective::from_config(&sample()).unconfigured());
        let half = Config {
            hub_url: "wss://h.example".into(),
            ..Config::default()
        };
        assert!(!Effective::from_config(&half).live(), "token missing");
        let cli = Cli {
            demo: true,
            ..Cli::default()
        };
        assert!(resolve(&sample(), &cli, &|_| None).demo);
    }

    #[test]
    fn hub_urls_are_normalised_for_the_client() {
        assert_eq!(
            normalize_hub_url("  https://hub.example.com/ "),
            "wss://hub.example.com"
        );
        assert_eq!(
            normalize_hub_url("http://hub.example.com:8080/x/"),
            "ws://hub.example.com:8080/x"
        );
        assert_eq!(
            normalize_hub_url("hub.example.com"),
            "wss://hub.example.com"
        );
        assert_eq!(
            normalize_hub_url("wss://hub.example.com//"),
            "wss://hub.example.com"
        );
        assert_eq!(normalize_hub_url("ws://127.0.0.1:9"), "ws://127.0.0.1:9");
        assert_eq!(normalize_hub_url("   "), "");
        assert!(hub_url_valid("wss://hub.example.com"));
        assert!(
            !hub_url_valid("wss://") && !hub_url_valid("") && !hub_url_valid("hub.example.com")
        );
        assert_eq!(
            hub_host("wss://hub.example.com:8443/path"),
            "hub.example.com:8443"
        );
        assert!(
            hub_host(&format!("wss://{}", "a".repeat(60)))
                .chars()
                .count()
                <= 26
        );
    }
}

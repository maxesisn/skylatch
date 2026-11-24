//! Enrollment and per-user agent configuration.

use crate::cli::{hostname, parse_hub_url, sanitize_id};
use clap::Args;
use serde::{Deserialize, Serialize};
use skylatch_proto::credentials::{is_token, write_private_file};
use skylatch_proto::onboarding::{CodeEnrollment, EnrollRequest, PasswordEnrollment};
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use url::Url;

#[derive(Args)]
pub struct Join {
    /// Public HTTP(S) or WS(S) address of the hub.
    hub_url: String,
    /// Single-use pairing code, such as 482-917.
    #[arg(long, conflicts_with = "username")]
    code: Option<String>,
    /// Hub username. When no code is supplied, prompts for any missing credentials.
    #[arg(long)]
    username: Option<String>,
    #[arg(long)]
    device_id: Option<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    room: Option<String>,
    #[arg(long)]
    slot: Option<u32>,
    /// Write the configuration to this file instead of the per-user one.
    #[arg(long, value_name = "PATH", conflicts_with = "system")]
    config: Option<PathBuf>,
    /// Write the configuration read by the Windows service (`%ProgramData%\Skylatch\agent.json`),
    /// readable only by SYSTEM and Administrators. Run from an elevated prompt.
    #[arg(long)]
    system: bool,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct SavedConfig {
    pub hub_url: String,
    pub device_id: String,
    pub name: String,
    pub device_token: String,
    pub room: Option<String>,
    pub slot: Option<u32>,
}

pub(crate) fn config_path() -> io::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let root = env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("Skylatch"));
    #[cfg(target_os = "macos")]
    let root = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support/Skylatch"));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let root = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".config"))
        })
        .map(|p| p.join("skylatch"));
    root.map(|p| p.join("agent.json")).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cannot find user configuration directory",
        )
    })
}

/// Configuration of the Windows service: `%ProgramData%\Skylatch\agent.json`.
pub fn system_config_path() -> PathBuf {
    system_config_path_in(env::var_os("ProgramData").map(PathBuf::from))
}

fn system_config_path_in(program_data: Option<PathBuf>) -> PathBuf {
    program_data
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Skylatch")
        .join("agent.json")
}

/// The file `join` writes: an explicit path, the service configuration, or the per-user one.
pub(crate) fn join_target(explicit: Option<PathBuf>, system: bool) -> io::Result<PathBuf> {
    match (explicit, system) {
        (Some(path), _) => Ok(path),
        (None, true) if cfg!(windows) => Ok(system_config_path()),
        (None, true) => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--system is only available on Windows; use --config PATH",
        )),
        (None, false) => config_path(),
    }
}

pub(crate) fn load(path: &Path) -> io::Result<Option<SavedConfig>> {
    match std::fs::read(path) {
        Ok(data) => Ok(Some(serde_json::from_slice(&data)?)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn join_url(address: &str) -> Result<Url, String> {
    let mut url = Url::parse(address).map_err(|_| "invalid hub URL")?;
    let scheme = match url.scheme() {
        "http" | "ws" => "ws",
        "https" | "wss" => "wss",
        _ => return Err("hub URL must use HTTP(S) or WS(S)".into()),
    };
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("hub URL must not contain credentials, a query or a fragment".into());
    }
    url.set_scheme(scheme).map_err(|_| "invalid hub scheme")?;
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with("/ws/host") {
        path.to_owned()
    } else {
        format!("{path}/ws/host")
    };
    url.set_path(&path);
    parse_hub_url(url.as_str())
}

fn prompt(label: &str) -> io::Result<String> {
    let mut stderr = io::stderr().lock();
    write!(stderr, "{label}: ")?;
    stderr.flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim_end_matches(['\r', '\n']).to_owned())
}

impl Join {
    pub async fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let hub_url = join_url(&self.hub_url)?;
        let path = join_target(self.config, self.system)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        #[cfg(windows)]
        if self.system {
            crate::service::restrict_to_admins(parent).map_err(|err| {
                format!(
                    "cannot restrict {} to SYSTEM and Administrators ({err}); run from an elevated prompt",
                    parent.display()
                )
            })?;
        }
        let device_id = self.device_id.unwrap_or_else(|| sanitize_id(&hostname()));
        let name = self.name.unwrap_or_else(hostname);
        let request = if let Some(code) = self.code {
            EnrollRequest::Code(CodeEnrollment {
                code,
                device_id: device_id.clone(),
                name: name.clone(),
            })
        } else {
            let username = match self.username {
                Some(username) => username,
                None => prompt("Username")?,
            };
            let password = match env::var("SKYLATCH_PASSWORD") {
                Ok(password) => password,
                Err(env::VarError::NotPresent) if io::stdin().is_terminal() => {
                    rpassword::prompt_password("Password: ")?
                }
                Err(env::VarError::NotPresent) => prompt("Password")?,
                Err(err) => return Err(err.into()),
            };
            EnrollRequest::Password(PasswordEnrollment {
                username,
                password,
                device_id: device_id.clone(),
                name: name.clone(),
            })
        };
        let result = skylatch_client_core::enroll(hub_url.as_str(), &request).await?;
        drop(request);
        if result.device_id != device_id || !is_token(&result.device_token) {
            return Err("hub returned invalid enrollment credentials".into());
        }
        let config = SavedConfig {
            hub_url: hub_url.into(),
            device_id,
            name,
            device_token: result.device_token,
            room: self.room,
            slot: self.slot,
        };
        write_private_file(&path, &serde_json::to_vec_pretty(&config)?)?;
        if self.system {
            println!(
                "Device enrolled for {}. The SkylatchAgent service connects within a few seconds.",
                result.account
            );
        } else {
            println!(
                "Device enrolled for {}. Start skylatch-agent to connect.",
                result.account
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_configuration_lives_under_program_data() {
        assert_eq!(
            system_config_path_in(Some(PathBuf::from(if cfg!(windows) {
                r"D:\Data"
            } else {
                "/data"
            }))),
            PathBuf::from(if cfg!(windows) { r"D:\Data" } else { "/data" })
                .join("Skylatch")
                .join("agent.json")
        );
        let fallback = PathBuf::from(r"C:\ProgramData")
            .join("Skylatch")
            .join("agent.json");
        assert_eq!(system_config_path_in(None), fallback);
        assert_eq!(
            system_config_path_in(Some(PathBuf::from("relative"))),
            fallback
        );
    }

    #[test]
    fn join_target_prefers_an_explicit_path() {
        let explicit = PathBuf::from("custom.json");
        assert_eq!(
            join_target(Some(explicit.clone()), false).unwrap(),
            explicit
        );
        assert_eq!(join_target(Some(explicit.clone()), true).unwrap(), explicit);
        if cfg!(windows) {
            assert_eq!(join_target(None, true).unwrap(), system_config_path());
        } else {
            assert_eq!(
                join_target(None, true).unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
    }
}

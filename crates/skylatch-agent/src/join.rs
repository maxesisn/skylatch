//! Enrollment and per-user agent configuration.

use crate::cli::{hostname, parse_hub_url, sanitize_id};
use clap::Args;
use serde::{Deserialize, Serialize};
use skylatch_client_core::mesh_vault::{open_mesh_invite, split_join_code, InviteSecret};
use skylatch_mesh::service::{is_elevated, CommandSpec};
use skylatch_mesh::{
    detect_conflicts, find_binaries, install::ACCEPTED_VERSION, write_managed_profiles,
    ManagedWriteOptions, MeshManager, MeshPaths,
};
use skylatch_proto::credentials::{is_token, write_private_file};
use skylatch_proto::mesh::MeshInviteCiphertext;
use skylatch_proto::onboarding::{CodeEnrollment, EnrollRequest, PasswordEnrollment};
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use url::Url;

#[derive(Args)]
pub struct Join {
    /// Public HTTP(S) or WS(S) address of the hub.
    hub_url: String,
    /// Single-use pairing code, such as 482-917, optionally followed by the mesh invite secret
    /// (482-917-XXXX-XXXX-XXXX-XXXX-XXXX) to also join the invited EasyTier network.
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

/// Splits `--code` into what is sent to the hub and the invite secret that stays on this machine.
/// A plain pairing code is sent exactly as typed.
fn split_code(raw: String) -> Result<(String, Option<InviteSecret>), &'static str> {
    match split_join_code(&raw) {
        Ok((code, Some(secret))) => Ok((code, Some(secret))),
        Ok((_, None)) => Ok((raw, None)),
        Err(_) if raw.trim().len() <= 7 => Ok((raw, None)),
        Err(_) => Err("the mesh invite part of the join code is malformed; check for typos"),
    }
}

/// Decrypts the invite, writes its owner-only EasyTier profile into the Skylatch-managed
/// directory next to `config_path`, and starts the managed service when that is safe.
/// Returns the status line to print; it never contains the network secret.
async fn join_mesh(
    hub_url: &Url,
    config_path: &Path,
    hostname: &str,
    invite: &MeshInviteCiphertext,
    secret: &InviteSecret,
) -> Result<String, Box<dyn std::error::Error>> {
    let profile = open_mesh_invite(invite, secret)?;
    let hub_peers = match skylatch_client_core::hub_info(hub_url.as_str()).await {
        Ok(info) => info.easytier_peers,
        Err(err) => {
            tracing::warn!(%err, "cannot read the hub's EasyTier peers");
            vec![]
        }
    };
    let app_dir = config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let paths = MeshPaths::new(app_dir);
    let launch = write_managed_profiles(
        &paths.profiles_dir(),
        std::slice::from_ref(&profile),
        std::slice::from_ref(&profile.id),
        &hub_peers,
        &ManagedWriteOptions::new(hostname),
    )?;
    let manager = MeshManager::system(paths.clone());
    manager.bind_managed(&launch, None, false, Some(hub_url.as_str()))?;
    let saved = format!(
        "Mesh network \"{}\" ({}) saved to {}",
        profile.name,
        profile.subnet,
        paths.profiles_dir().display()
    );
    let Some(core) = find_binaries(&[]).core else {
        return Ok(format!(
            "{saved}; install EasyTier {ACCEPTED_VERSION} to connect."
        ));
    };
    let conflicts = detect_conflicts();
    if !conflicts.is_empty() {
        let titles: Vec<&str> = conflicts.iter().map(|c| c.title.as_str()).collect();
        return Ok(format!("{saved}; not started: {}.", titles.join("; ")));
    }
    if is_elevated() {
        manager.apply(&core)?;
        return Ok(format!(
            "{saved}; the Skylatch EasyTier service is running."
        ));
    }
    let command = CommandSpec {
        program: core.to_string_lossy().into_owned(),
        args: launch.args(),
    };
    let elevate = if cfg!(windows) { "" } else { "sudo " };
    Ok(format!(
        "{saved}; start it as administrator: {elevate}{command}"
    ))
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
        let mut invite_secret = None;
        let request = if let Some(code) = self.code {
            let (code, secret) = split_code(code)?;
            invite_secret = secret;
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
            hub_url: hub_url.to_string(),
            device_id,
            name: name.clone(),
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
        match (invite_secret, &result.mesh_invite) {
            (Some(secret), Some(invite)) => {
                let status = join_mesh(&hub_url, &path, &name, invite, &secret)
                    .await
                    .map_err(|err| format!("the mesh network was not set up: {err}"))?;
                println!("{status}");
            }
            (Some(_), None) => {
                eprintln!(
                    "The hub returned no mesh invite for this code; only the device was enrolled."
                )
            }
            (None, _) => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_codes_reach_the_hub_unchanged() {
        for raw in ["482-917", " 482-917 ", "482917", "12"] {
            let (code, secret) = split_code(raw.into()).unwrap();
            assert_eq!(code, raw);
            assert!(secret.is_none());
        }
        let secret = InviteSecret::generate().unwrap();
        let joined = skylatch_client_core::mesh_vault::format_join_code("482-917", &secret);
        let (code, parsed) = split_code(joined.to_lowercase()).unwrap();
        assert_eq!(code, "482-917");
        assert_eq!(parsed.unwrap().display(), secret.display());
        assert!(split_code("482-917-ABCD".into()).is_err());
    }

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

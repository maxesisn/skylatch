//! EasyTier integration shared by the Skylatch desktop client and host agent: environment
//! detection, Skylatch-managed TOML profiles, runtime status and the lifecycle of the managed
//! EasyTier system service.
//!
//! No EasyTier code is linked. Everything goes through standard EasyTier TOML files, the
//! `easytier-core` / `easytier-cli` executables and the operating system's service manager.

pub mod detect;
pub mod install;
pub mod manager;
mod process;
pub mod service;
pub mod status;
pub mod toml_config;

pub use detect::{
    detect_conflicts, detect_presence, find_binaries, ConflictKind, ConflictSeverity,
    EasyTierBinaries, EasyTierPresence, MeshConflict,
};
pub use install::{install_or_download_spec, DownloadSpec, EasyTierTarget};
pub use manager::{MeshBindingMode, MeshManager, MeshState};
pub use status::{query_status, MeshPeerStatus, MeshRouteKind, MeshRuntimeStatus};
pub use toml_config::{
    import_easytier_toml, profile_toml, write_managed_profiles, ManagedLaunch, ManagedWriteOptions,
};

use std::path::PathBuf;

/// RPC portal of Skylatch's managed `easytier-core`, next to EasyTier's default `15888` so an
/// independent EasyTier on the same machine keeps its own portal.
pub const MANAGED_RPC_PORTAL: &str = "127.0.0.1:15889";
/// Portal `easytier-core` and `easytier-cli` use when none is given.
pub const EASYTIER_DEFAULT_RPC_PORTAL: &str = "127.0.0.1:15888";
/// Present in every directory whose `net-*.toml` files Skylatch owns.
pub const MANAGED_MARKER: &str = ".skylatch-managed";

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("invalid EasyTier TOML: {0}")]
    Toml(String),
    #[error("EasyTier TOML has no {0}")]
    MissingField(&'static str),
    #[error(transparent)]
    Profile(#[from] skylatch_proto::mesh::MeshError),
    #[error("profile `{0}` is not among the supplied profiles")]
    UnknownProfile(String),
    #[error("profile `{0}` conflicts with the managed network `{1}` (same subnet or listen port)")]
    ManagedConflict(String, String),
    #[error("{} is not a Skylatch-managed EasyTier directory", .0.display())]
    NotManaged(PathBuf),
    #[error("invalid mesh state file: {0}")]
    State(String),
    #[error(transparent)]
    Service(#[from] service::ServiceError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Where Skylatch keeps its local mesh files: `mesh-state.json` and the managed `easytier/`
/// directory of `net-<profile id>.toml` files, both under the Skylatch application directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshPaths {
    pub app_dir: PathBuf,
}

impl MeshPaths {
    pub fn new(app_dir: impl Into<PathBuf>) -> Self {
        Self {
            app_dir: app_dir.into(),
        }
    }

    /// The per-user Skylatch directory, the same one `skylatch-agent join` writes `agent.json` to.
    pub fn user_default() -> Option<Self> {
        user_app_dir().map(Self::new)
    }

    pub fn state_file(&self) -> PathBuf {
        self.app_dir.join("mesh-state.json")
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.app_dir.join("easytier")
    }
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

fn user_app_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_dir("APPDATA").map(|p| p.join("Skylatch"))
    } else if cfg!(target_os = "macos") {
        env_dir("HOME").map(|p| p.join("Library/Application Support/Skylatch"))
    } else {
        env_dir("XDG_CONFIG_HOME")
            .or_else(|| env_dir("HOME").map(|p| p.join(".config")))
            .map(|p| p.join("skylatch"))
    }
}

/// The machine-wide Skylatch directory used by system services.
pub fn system_app_dir() -> PathBuf {
    if cfg!(windows) {
        env_dir("ProgramData")
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("Skylatch")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/Skylatch")
    } else {
        PathBuf::from("/etc/skylatch")
    }
}

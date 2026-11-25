//! Local binding state (`mesh-state.json`) and the managed network's lifecycle.
//!
//! The state file never holds secrets: network secrets live only in the encrypted vault and in
//! the owner-only `net-*.toml` files EasyTier reads.

use crate::service::{spawn_user_process, ServiceError, ServiceManager};
use crate::toml_config::{is_managed_dir, managed_profile_ids, profile_file_name, ManagedLaunch};
use crate::{MeshError, MeshPaths, MANAGED_MARKER, MANAGED_RPC_PORTAL};
use serde::{Deserialize, Serialize};
use skylatch_proto::credentials::write_private_file;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum MeshBindingMode {
    /// EasyTier, if any, runs on its own; Skylatch does not rewrite its profiles.
    #[default]
    Unbound,
    Managed {
        active_profile_ids: Vec<String>,
        /// Vault version the profiles were written from; `None` for pairing-code invites.
        vault_version: Option<u64>,
        no_tun: bool,
        rpc_portal: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshState {
    #[serde(default)]
    pub binding: MeshBindingMode,
    #[serde(default)]
    pub last_hub_url: Option<String>,
}

pub struct MeshManager {
    paths: MeshPaths,
    service: Option<ServiceManager>,
}

impl MeshManager {
    pub fn new(paths: MeshPaths, service: Option<ServiceManager>) -> Self {
        Self { paths, service }
    }

    /// A manager for `paths` using this operating system's service manager.
    pub fn system(paths: MeshPaths) -> Self {
        Self::new(paths, ServiceManager::system())
    }

    pub fn paths(&self) -> &MeshPaths {
        &self.paths
    }

    pub fn load_state(&self) -> Result<MeshState, MeshError> {
        match fs::read(self.paths.state_file()) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|e| MeshError::State(e.to_string()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(MeshState::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save_state(&self, state: &MeshState) -> Result<(), MeshError> {
        fs::create_dir_all(&self.paths.app_dir)?;
        let json = serde_json::to_vec_pretty(state).map_err(|e| MeshError::State(e.to_string()))?;
        write_private_file(&self.paths.state_file(), &json)?;
        Ok(())
    }

    /// Records that Skylatch manages the profiles of `launch`.
    pub fn bind_managed(
        &self,
        launch: &ManagedLaunch,
        vault_version: Option<u64>,
        no_tun: bool,
        hub_url: Option<&str>,
    ) -> Result<MeshState, MeshError> {
        let mut state = self.load_state()?;
        state.binding = MeshBindingMode::Managed {
            active_profile_ids: launch.profile_ids.clone(),
            vault_version,
            no_tun,
            rpc_portal: launch.rpc_portal.clone(),
        };
        if let Some(url) = hub_url {
            state.last_hub_url = Some(url.into());
        }
        self.save_state(&state)?;
        Ok(state)
    }

    /// The launch for every profile in the managed directory.
    pub fn launch(&self) -> Result<ManagedLaunch, MeshError> {
        let rpc_portal = match self.load_state()?.binding {
            MeshBindingMode::Managed { rpc_portal, .. } => rpc_portal,
            MeshBindingMode::Unbound => MANAGED_RPC_PORTAL.into(),
        };
        Ok(ManagedLaunch::scan(
            &self.paths.profiles_dir(),
            &rpc_portal,
        )?)
    }

    fn service(&self) -> Result<&ServiceManager, MeshError> {
        self.service
            .as_ref()
            .ok_or(MeshError::Service(ServiceError::Unsupported))
    }

    /// Installs (or replaces) and starts the managed EasyTier service with every managed profile.
    pub fn apply(&self, easytier_core: &Path) -> Result<(), MeshError> {
        let launch = self.launch()?;
        Ok(self.service()?.install_and_start(easytier_core, &launch)?)
    }

    /// Runs the managed profiles as a user process; only for bindings in no-TUN mode.
    pub fn start_user_mode(&self, easytier_core: &Path) -> Result<std::process::Child, MeshError> {
        match self.load_state()?.binding {
            MeshBindingMode::Managed { no_tun: true, .. } => {}
            _ => {
                return Err(MeshError::Service(ServiceError::NeedsElevation(
                    self.service()?.install_plan(easytier_core, &self.launch()?),
                )))
            }
        }
        let log = self.paths.profiles_dir().join("easytier.log");
        Ok(spawn_user_process(easytier_core, &self.launch()?, &log)?)
    }

    /// Unlinks the network from the Skylatch account: the running service and its TOML files stay,
    /// so the machine keeps its mesh connectivity on its own.
    pub fn detach_to_independent(&self) -> Result<MeshState, MeshError> {
        let mut state = self.load_state()?;
        state.binding = MeshBindingMode::Unbound;
        self.save_state(&state)?;
        Ok(state)
    }

    /// Stops and removes the managed service, then deletes the managed `net-*.toml` files. Any
    /// EasyTier Skylatch did not install is left alone. Nothing is deleted while the service
    /// cannot be stopped.
    pub fn stop_and_clear_managed(&self) -> Result<MeshState, MeshError> {
        if let Some(service) = &self.service {
            if service.is_installed() {
                service.stop_and_uninstall()?;
            }
        }
        let dir = self.paths.profiles_dir();
        if is_managed_dir(&dir) {
            for id in managed_profile_ids(&dir)? {
                fs::remove_file(dir.join(profile_file_name(&id)))?;
            }
            fs::remove_file(dir.join(MANAGED_MARKER))?;
            let _ = fs::remove_file(dir.join("easytier.log"));
            let _ = fs::remove_dir(&dir);
        }
        self.detach_to_independent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::Recorder;
    use crate::service::ServicePlatform;
    use crate::toml_config::{write_managed_profiles, ManagedWriteOptions};
    use skylatch_proto::mesh::MeshVault;
    use std::sync::Arc;

    struct Fixture {
        manager: MeshManager,
        recorder: Arc<Recorder>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.manager.paths.app_dir);
        }
    }

    fn fixture(elevated: bool) -> Fixture {
        let dir = std::env::temp_dir().join(format!(
            "skylatch-manager-{}",
            skylatch_proto::credentials::random_token().unwrap()
        ));
        let recorder = Arc::new(Recorder {
            elevated,
            ..Default::default()
        });
        let service =
            ServiceManager::with_executor(ServicePlatform::Launchd, Box::new(recorder.clone()));
        Fixture {
            manager: MeshManager::new(MeshPaths::new(dir), Some(service)),
            recorder,
        }
    }

    fn bind(manager: &MeshManager) -> (MeshVault, ManagedLaunch) {
        let vault = MeshVault::new_default().unwrap();
        let ids = vec![vault.profiles[0].id.clone()];
        let launch = write_managed_profiles(
            &manager.paths.profiles_dir(),
            &vault.profiles,
            &ids,
            &[],
            &ManagedWriteOptions::new("host"),
        )
        .unwrap();
        manager
            .bind_managed(&launch, Some(3), false, Some("https://hub.example.com"))
            .unwrap();
        (vault, launch)
    }

    #[test]
    fn state_file_has_no_secrets_and_detach_keeps_the_network() {
        let f = fixture(true);
        assert_eq!(f.manager.load_state().unwrap(), MeshState::default());
        let (vault, launch) = bind(&f.manager);
        let saved = fs::read_to_string(f.manager.paths.state_file()).unwrap();
        assert!(!saved.contains(&vault.profiles[0].network_secret));
        assert!(saved.contains("\"mode\": \"managed\""));
        assert_eq!(f.manager.launch().unwrap(), launch);

        f.manager.apply(Path::new("/opt/et/easytier-core")).unwrap();
        let state = f.manager.detach_to_independent().unwrap();
        assert_eq!(state.binding, MeshBindingMode::Unbound);
        assert_eq!(
            state.last_hub_url.as_deref(),
            Some("https://hub.example.com")
        );
        assert!(launch.config_files[0].is_file());
        assert!(f
            .recorder
            .log
            .lock()
            .unwrap()
            .iter()
            .all(|l| !l.starts_with("remove")));
    }

    #[test]
    fn stop_and_clear_removes_only_managed_files() {
        let f = fixture(true);
        let (_, launch) = bind(&f.manager);
        let foreign = f.manager.paths.app_dir.join("my-easytier.toml");
        fs::write(&foreign, "x").unwrap();
        f.manager.apply(Path::new("/opt/et/easytier-core")).unwrap();
        let state = f.manager.stop_and_clear_managed().unwrap();
        assert_eq!(state.binding, MeshBindingMode::Unbound);
        assert!(!launch.config_files[0].exists());
        assert!(!f.manager.paths.profiles_dir().exists());
        assert!(foreign.exists());
        assert!(f
            .recorder
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("remove /Library/LaunchDaemons/io.github.maxesisn.skylatch")));
    }

    #[test]
    fn unprivileged_stop_keeps_everything() {
        let f = fixture(false);
        let (_, launch) = bind(&f.manager);
        f.recorder
            .present
            .lock()
            .unwrap()
            .push(ServicePlatform::Launchd.definition_path().unwrap());
        assert!(matches!(
            f.manager.stop_and_clear_managed(),
            Err(MeshError::Service(ServiceError::NeedsElevation(_)))
        ));
        assert!(launch.config_files[0].is_file());
        assert!(matches!(
            f.manager.load_state().unwrap().binding,
            MeshBindingMode::Managed { .. }
        ));
        assert!(matches!(
            f.manager
                .start_user_mode(Path::new("/opt/et/easytier-core")),
            Err(MeshError::Service(ServiceError::NeedsElevation(_)))
        ));
    }
}

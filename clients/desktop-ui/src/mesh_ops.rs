// SPDX-License-Identifier: Apache-2.0

//! The work behind the 连接与组网 drawer that must not run on the UI thread: EasyTier
//! detection, the vault's hub requests and its Argon2id unlock, and changes to this machine's
//! managed EasyTier. Every function here blocks or is async; callers run them through
//! [`run_blocking`] or [`crate::live::run_async`].
//!
//! System-service changes need administrator rights. The launcher then runs itself once more
//! as `--mesh-helper <op>` through the platform's own elevation prompt (macOS authorization
//! dialog, Windows UAC, polkit); the user-mode, no-TUN path needs no rights at all.

use crate::live;
use crate::mesh_ui::{Cell, NetStatus, Probe, UnlockMode, Vault, VaultEvent, VaultSession};
use skylatch_client_core::mesh_vault::{self, VaultKey};
use skylatch_client_core::{MeshVault, MeshVaultEnvelope, VaultPutError};
use skylatch_mesh::install::{EasyTierTarget, system_install_dir};
use skylatch_mesh::service::{ServiceError, ServiceManager};
use skylatch_mesh::toml_config::instance_name;
use skylatch_mesh::{
    EasyTierBinaries, EasyTierPresence, ManagedWriteOptions, MeshBindingMode, MeshError,
    MeshManager, MeshPaths, MeshState, detect_conflicts, detect_presence, find_binaries,
    install_or_download_spec, query_status, write_managed_profiles,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};
use zeroize::Zeroizing;

/// Runs `f` on a short-lived worker thread; the answer arrives on the returned channel.
pub fn run_blocking<T, F>(f: F) -> Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("skylatch-mesh".into())
        .spawn(move || {
            let _ = tx.send(f());
        });
    rx
}

/// This user's Skylatch directory; `None` without a home directory.
pub fn paths() -> Option<MeshPaths> {
    MeshPaths::user_default()
}

/// Where the no-TUN path installs EasyTier without administrator rights.
fn user_bin_dir(paths: &MeshPaths) -> PathBuf {
    paths.app_dir.join("easytier-bin")
}

fn pid_file(paths: &MeshPaths) -> PathBuf {
    paths.app_dir.join("mesh-user.pid")
}

fn binaries(paths: &MeshPaths) -> EasyTierBinaries {
    find_binaries(&[user_bin_dir(paths)])
}

pub fn load_state(paths: &MeshPaths) -> MeshState {
    MeshManager::new(paths.clone(), None)
        .load_state()
        .unwrap_or_default()
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The hostname EasyTier announces for this machine.
pub fn local_hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname NUL-terminates on success.
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            let name = String::from_utf8_lossy(&buf[..end]).to_string();
            return name.trim_end_matches(".local").to_string();
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "skylatch-host".into())
}

// ───────────────────────────── detection ─────────────────────────────

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

#[cfg(not(unix))]
fn pid_alive(pid: i32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
}

fn user_pid(paths: &MeshPaths) -> Option<i32> {
    std::fs::read_to_string(pid_file(paths))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|pid| pid_alive(*pid))
}

/// One detection round: presence, live topology, conflicts and the local binding. Read-only:
/// external instances are only queried through `easytier-cli`.
pub fn probe(paths: Option<&MeshPaths>) -> Probe {
    let bins = paths.map(binaries).unwrap_or_else(|| find_binaries(&[]));
    let own: Vec<MeshPaths> = paths.into_iter().cloned().collect();
    let presence = detect_presence(&own, &bins);
    let state = paths.map(load_state).unwrap_or_default();
    let managed_ids = paths
        .and_then(|p| skylatch_mesh::toml_config::managed_profile_ids(&p.profiles_dir()).ok())
        .unwrap_or_default();
    let bound = matches!(state.binding, MeshBindingMode::Managed { .. });
    let cell = Cell::resolve(&presence, bound, &managed_ids, bins.core.is_some());
    let query = |portal: &str, instance: Option<&str>| match &bins.cli {
        Some(cli) => query_status(cli, Some(portal), instance).map_err(|e| e.to_string()),
        None => Err("找不到 easytier-cli".to_string()),
    };
    let networks = match (&presence, cell) {
        (
            EasyTierPresence::ManagedRunning {
                rpc_portal,
                active_profile_ids,
                ..
            },
            _,
        ) => active_profile_ids
            .iter()
            .map(|id| NetStatus {
                profile_id: Some(id.clone()),
                status: query(rpc_portal, Some(&instance_name(id))),
            })
            .collect(),
        (EasyTierPresence::ExternalConfigServer { rpc_portal, .. }, c) if c.is_external() => {
            vec![NetStatus {
                profile_id: None,
                status: query(rpc_portal, None),
            }]
        }
        (
            EasyTierPresence::ExternalGuiOrManual {
                rpc_portal: Some(rpc_portal),
                ..
            },
            c,
        ) if c.is_external() => vec![NetStatus {
            profile_id: None,
            status: query(rpc_portal, None),
        }],
        _ => vec![],
    };
    Probe {
        user_process: paths.and_then(user_pid).is_some(),
        presence,
        cell,
        networks,
        conflicts: detect_conflicts(),
        state,
        managed_ids,
    }
}

// ───────────────────────────── vault ─────────────────────────────

/// A new vault with one default network, sealed with `password` but not uploaded yet.
pub fn new_vault(password: &str) -> Result<VaultSession, String> {
    let vault = MeshVault::new_default().map_err(|e| e.to_string())?;
    let key = mesh_vault::generate_dek().map_err(|e| e.to_string())?;
    let envelope =
        mesh_vault::seal_vault(&vault, &key, password, 0, now_unix()).map_err(|e| e.to_string())?;
    Ok(VaultSession {
        envelope,
        vault,
        key,
    })
}

fn describe_vault_error(e: &mesh_vault::MeshVaultError) -> String {
    use mesh_vault::MeshVaultError as E;
    match e {
        E::WrongPasswordOrCorrupted => "密码不正确，无法解开云端组网配置".into(),
        E::WrongKeyOrCorrupted => "恢复码与这份云端组网配置不匹配".into(),
        E::InvalidRecoveryKey => "这不是 Skylatch 恢复码（应以 SLK1- 开头）".into(),
        E::RecoveryKeyChecksum => "恢复码校验失败，请检查是否抄错".into(),
        other => other.to_string(),
    }
}

/// Opens `envelope` with the account password or a recovery key (Argon2id: off the UI thread).
pub fn unlock(
    envelope: &MeshVaultEnvelope,
    secret: &str,
    mode: UnlockMode,
) -> Result<VaultSession, String> {
    let (vault, key) = match mode {
        UnlockMode::Password => {
            mesh_vault::open_vault(envelope, secret).map_err(|e| describe_vault_error(&e))?
        }
        UnlockMode::Recovery => {
            let key =
                mesh_vault::parse_recovery_key(secret).map_err(|e| describe_vault_error(&e))?;
            let vault = mesh_vault::open_vault_with_recovery_key(envelope, &key)
                .map_err(|e| describe_vault_error(&e))?;
            (vault, key)
        }
    };
    Ok(VaultSession {
        envelope: envelope.clone(),
        vault,
        key,
    })
}

/// The vault right after a password sign-in: opened with the password, or a fresh default
/// vault sealed with it when the hub has none yet.
async fn vault_after_sign_in(hub: &str, token: &str, password: &str) -> Vault {
    match skylatch_client_core::get_mesh_vault(hub, token).await {
        Ok(Some(envelope)) => match unlock(&envelope, password, UnlockMode::Password) {
            Ok(session) => Vault::Open(session),
            Err(_) => Vault::Locked(envelope),
        },
        Ok(None) => match new_vault(password) {
            Ok(session) => Vault::Open(session),
            Err(e) => Vault::Failed(e),
        },
        Err(e) => Vault::Failed(live::describe_error(&e)),
    }
}

/// A finished password sign-in or signup.
pub struct SignedIn {
    pub token: String,
    pub username: String,
    pub vault: Vault,
}

/// Signs in (or signs up) and unlocks the account's vault with the same password, which is
/// dropped (and wiped) at the end.
pub async fn sign_in(
    hub: String,
    username: String,
    password: Zeroizing<String>,
    signup: bool,
) -> Result<SignedIn, String> {
    let response = if signup {
        skylatch_client_core::signup(&hub, &username, &password)
            .await
            .map_err(|e| crate::hub_ui::describe_signup_error(&e))?
    } else {
        skylatch_client_core::login(&hub, &username, &password)
            .await
            .map_err(|e| crate::onboard::describe_login_error(&e))?
    };
    let vault = vault_after_sign_in(&hub, &response.token, &password).await;
    Ok(SignedIn {
        token: response.token,
        username,
        vault,
    })
}

pub async fn fetch_vault(hub: String, token: String) -> VaultEvent {
    match skylatch_client_core::get_mesh_vault(&hub, &token).await {
        Ok(envelope) => VaultEvent::Loaded(envelope),
        Err(e) => VaultEvent::Failed(live::describe_error(&e)),
    }
}

/// Seals `vault` into `base` (same data key and password wrap) and stores it if the hub still
/// holds `base.version`.
pub async fn push_vault(
    hub: String,
    token: String,
    base: MeshVaultEnvelope,
    key: VaultKey,
    vault: MeshVault,
) -> VaultEvent {
    let sealed = match mesh_vault::reseal_vault(&base, &vault, &key, now_unix()) {
        Ok(sealed) => sealed,
        Err(e) => return VaultEvent::Failed(describe_vault_error(&e)),
    };
    match skylatch_client_core::put_mesh_vault(&hub, &token, base.version, &sealed).await {
        Ok(saved) => VaultEvent::Saved(saved),
        Err(VaultPutError::Conflict(current)) => VaultEvent::Conflict(current.map(|e| *e)),
        Err(VaultPutError::Client(e)) => VaultEvent::Failed(live::describe_error(&e)),
    }
}

// ───────────────────────────── this machine's EasyTier ─────────────────────────────

/// Writes the selected networks into the managed directory and records the binding.
pub struct ApplyRequest {
    pub paths: MeshPaths,
    pub vault: MeshVault,
    pub selected: Vec<String>,
    pub hub_peers: Vec<String>,
    pub no_tun: bool,
    pub vault_version: Option<u64>,
    pub hub_url: String,
    /// Also (re)start the managed EasyTier afterwards.
    pub start: bool,
}

fn mesh_error(e: MeshError) -> String {
    match e {
        MeshError::ManagedConflict(a, b) => {
            format!("组网 {a} 与本机已有的托管组网 {b} 网段或端口相同")
        }
        MeshError::Profile(p) => crate::mesh_ui::describe_mesh_error(&p),
        other => other.to_string(),
    }
}

pub fn apply_local(req: ApplyRequest) -> Result<String, String> {
    if req.selected.is_empty() {
        return Err("请至少勾选一个本机要加入的组网".into());
    }
    let mut opts = ManagedWriteOptions::new(local_hostname());
    opts.no_tun = req.no_tun;
    let launch = write_managed_profiles(
        &req.paths.profiles_dir(),
        &req.vault.profiles,
        &req.selected,
        &req.hub_peers,
        &opts,
    )
    .map_err(mesh_error)?;
    let manager = MeshManager::system(req.paths.clone());
    manager
        .bind_managed(&launch, req.vault_version, req.no_tun, Some(&req.hub_url))
        .map_err(mesh_error)?;
    if !req.start {
        return Ok(format!(
            "已写入本机 {} 个托管组网",
            launch.profile_ids.len()
        ));
    }
    start(&req.paths)
}

fn stop_user_process(paths: &MeshPaths) {
    if let Some(pid) = user_pid(paths) {
        #[cfg(unix)]
        // SAFETY: plain signal to a process Skylatch started and recorded itself.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .output();
        for _ in 0..30 {
            if !pid_alive(pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let _ = std::fs::remove_file(pid_file(paths));
}

/// Starts (or restarts) the managed networks: a user process in no-TUN mode, the system
/// service otherwise.
pub fn start(paths: &MeshPaths) -> Result<String, String> {
    let manager = MeshManager::system(paths.clone());
    let no_tun = matches!(
        manager.load_state().map_err(mesh_error)?.binding,
        MeshBindingMode::Managed { no_tun: true, .. }
    );
    if no_tun {
        let core = binaries(paths)
            .core
            .ok_or("找不到 easytier-core，请先安装 EasyTier")?;
        // the system service would run the same networks a second time
        if ServiceManager::system().is_some_and(|s| s.is_installed()) {
            stop(paths)?;
        }
        stop_user_process(paths);
        let child = manager.start_user_mode(&core).map_err(mesh_error)?;
        std::fs::write(pid_file(paths), child.id().to_string()).map_err(|e| e.to_string())?;
        return Ok("已以无 TUN 模式启动托管组网".into());
    }
    let core = system_core().ok_or("找不到 easytier-core，请先安装 EasyTier")?;
    stop_user_process(paths);
    match manager.apply(&core) {
        Ok(()) => Ok("托管 EasyTier 服务已启动".into()),
        Err(MeshError::Service(ServiceError::NeedsElevation(_))) => {
            elevate("apply", &paths.app_dir, Some(&core))?;
            Ok("托管 EasyTier 服务已启动".into())
        }
        Err(e) => Err(mesh_error(e)),
    }
}

/// `easytier-core` for the system service: never one inside a user-writable directory.
fn system_core() -> Option<PathBuf> {
    let found = find_binaries(&[]).core?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    (!home.is_some_and(|h| found.starts_with(h))).then_some(found)
}

/// Stops the managed networks but keeps their files (ManagedStopped).
pub fn stop(paths: &MeshPaths) -> Result<String, String> {
    if user_pid(paths).is_some() {
        stop_user_process(paths);
        return Ok("托管组网已停止".into());
    }
    let Some(service) = ServiceManager::system() else {
        return Err("此系统不支持托管 EasyTier 服务".into());
    };
    if !service.is_installed() {
        return Ok("托管组网没有在运行".into());
    }
    match service.stop_and_uninstall() {
        Ok(()) => Ok("托管组网已停止".into()),
        Err(ServiceError::NeedsElevation(_)) => {
            elevate("stop", &paths.app_dir, None)?;
            Ok("托管组网已停止".into())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// 仅退出 Skylatch：EasyTier keeps running and its files stay, only the binding goes.
pub fn detach(paths: &MeshPaths) -> Result<String, String> {
    MeshManager::system(paths.clone())
        .detach_to_independent()
        .map_err(mesh_error)?;
    let _ = std::fs::remove_file(pid_file(paths));
    Ok("EasyTier 已转为独立运行，不再随 Skylatch 账号同步".into())
}

/// Stops and removes the managed EasyTier and its files.
pub fn stop_and_clear(paths: &MeshPaths) -> Result<String, String> {
    stop_user_process(paths);
    match MeshManager::system(paths.clone()).stop_and_clear_managed() {
        Ok(_) => Ok("已停用并清除本机 EasyTier 组网".into()),
        Err(MeshError::Service(ServiceError::NeedsElevation(_))) => {
            elevate("stop-clear", &paths.app_dir, None)?;
            Ok("已停用并清除本机 EasyTier 组网".into())
        }
        Err(e) => Err(mesh_error(e)),
    }
}

/// `cloud` with every network this machine runs replaced by its local managed TOML: what
/// 用本机覆盖云端 uploads. The hub's peers are folded back into `use_hub_peer`.
pub fn local_vault(
    paths: &MeshPaths,
    cloud: &MeshVault,
    hub_peers: &[String],
) -> Result<MeshVault, String> {
    let dir = paths.profiles_dir();
    let ids = skylatch_mesh::toml_config::managed_profile_ids(&dir).map_err(|e| e.to_string())?;
    let mut vault = cloud.clone();
    for id in ids {
        let file = dir.join(skylatch_mesh::toml_config::profile_file_name(&id));
        let text = std::fs::read_to_string(&file).map_err(|e| e.to_string())?;
        let local = skylatch_mesh::import_easytier_toml(&text, None).map_err(mesh_error)?;
        let use_hub_peer = local.custom_peers.iter().any(|p| hub_peers.contains(p));
        let custom_peers: Vec<String> = local
            .custom_peers
            .into_iter()
            .filter(|p| !hub_peers.contains(p))
            .collect();
        let name = cloud
            .profile(&id)
            .map_or(local.name.clone(), |p| p.name.clone());
        let profile = skylatch_client_core::MeshProfile {
            id: id.clone(),
            name,
            custom_peers,
            use_hub_peer,
            ..local
        };
        match vault.profiles.iter_mut().find(|p| p.id == id) {
            Some(slot) => *slot = profile,
            None => vault.profiles.push(profile),
        }
    }
    vault
        .validate()
        .map_err(|e| crate::mesh_ui::describe_mesh_error(&e))?;
    Ok(vault)
}

/// Re-binds Skylatch's own profiles that were detached earlier.
pub fn rebind(paths: &MeshPaths, hub_url: &str) -> Result<String, String> {
    let manager = MeshManager::system(paths.clone());
    let launch = manager.launch().map_err(mesh_error)?;
    if launch.profile_ids.is_empty() {
        return Err("本机没有可接入的 Skylatch 组网文件".into());
    }
    manager
        .bind_managed(&launch, None, false, Some(hub_url))
        .map_err(mesh_error)?;
    Ok("已接入 Skylatch 协作管理".into())
}

/// Downloads the official EasyTier release (`curl`), checks its SHA-256 and installs it: into
/// this user's directory for no-TUN mode, through the elevation prompt for the system service.
pub fn install(paths: &MeshPaths, no_tun: bool) -> Result<String, String> {
    if no_tun {
        if binaries(paths).core.is_some() {
            return Ok("EasyTier 已安装".into());
        }
        install_to(&user_bin_dir(paths))?;
        return Ok("EasyTier 已安装到用户目录".into());
    }
    if system_core().is_some() {
        return Ok("EasyTier 已安装".into());
    }
    elevate("install", &paths.app_dir, None)?;
    Ok("EasyTier 已安装".into())
}

fn install_to(dir: &Path) -> Result<(), String> {
    let target = EasyTierTarget::current().ok_or("这台机器的系统或架构没有官方 EasyTier 构建")?;
    let mut spec = install_or_download_spec(target, None);
    spec.install_dir = dir.to_path_buf();
    spec.install_with(|url| {
        let out = std::process::Command::new("curl")
            .args(["-fsSL", "--max-time", "600", url])
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(out.stdout)
    })
    .map_err(|e| format!("安装 EasyTier 失败：{e}"))?;
    Ok(())
}

// ───────────────────────────── elevation ─────────────────────────────

/// Single-quoted for a POSIX shell.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The helper invocation: this executable with `--mesh-helper <op> <app dir> [core]`.
pub fn helper_args(op: &str, app_dir: &Path, core: Option<&Path>) -> Vec<String> {
    let mut args = vec![
        "--mesh-helper".to_string(),
        op.to_string(),
        app_dir.to_string_lossy().into_owned(),
    ];
    if let Some(core) = core {
        args.push(core.to_string_lossy().into_owned());
    }
    args
}

/// The AppleScript that asks for an administrator password and runs `exe args`.
pub fn applescript(exe: &Path, args: &[String]) -> String {
    let mut cmd = sh_quote(&exe.to_string_lossy());
    for a in args {
        cmd.push(' ');
        cmd.push_str(&sh_quote(a));
    }
    let escaped = cmd.replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{escaped}\" with administrator privileges")
}

fn elevate(op: &str, app_dir: &Path, core: Option<&Path>) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let args = helper_args(op, app_dir, core);
    let output = if cfg!(target_os = "macos") {
        std::process::Command::new("osascript")
            .args(["-e", &applescript(&exe, &args)])
            .output()
    } else if cfg!(windows) {
        let list = args
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(",");
        std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "$p = Start-Process -FilePath '{}' -ArgumentList {list} -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
                    exe.to_string_lossy().replace('\'', "''")
                ),
            ])
            .output()
    } else {
        std::process::Command::new("pkexec")
            .arg(&exe)
            .args(&args)
            .output()
    }
    .map_err(|e| format!("无法请求管理员权限：{e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&output.stderr);
    if err.contains("-128") || err.contains("canceled") || err.contains("cancelled") {
        return Err("已取消管理员授权，没有做任何改动".into());
    }
    Err(format!(
        "管理员操作失败：{}",
        err.lines().last().unwrap_or("未知错误").trim()
    ))
}

/// `--mesh-helper <op> <app dir> [core]`, run with administrator rights by [`elevate`].
/// Returns the exit code, or `None` when `args` are not a helper invocation.
pub fn run_helper(args: &[String]) -> Option<i32> {
    if args.first().map(String::as_str) != Some("--mesh-helper") {
        return None;
    }
    let (Some(op), Some(dir)) = (args.get(1), args.get(2)) else {
        eprintln!("usage: skylatch-ui --mesh-helper <op> <app dir> [easytier-core]");
        return Some(2);
    };
    let paths = MeshPaths::new(dir);
    let manager = MeshManager::system(paths.clone());
    let result: Result<(), String> = match op.as_str() {
        "apply" => args
            .get(3)
            .ok_or_else(|| "missing easytier-core".to_string())
            .and_then(|core| manager.apply(Path::new(core)).map_err(|e| e.to_string())),
        "stop" => ServiceManager::system()
            .ok_or_else(|| "unsupported".to_string())
            .and_then(|s| s.stop_and_uninstall().map_err(|e| e.to_string())),
        "stop-clear" => manager
            .stop_and_clear_managed()
            .map(drop)
            .map_err(|e| e.to_string()),
        "install" => EasyTierTarget::current()
            .ok_or_else(|| "unsupported target".to_string())
            .and_then(|t| install_to(&system_install_dir(t))),
        other => Err(format!("unknown helper operation {other}")),
    };
    match result {
        Ok(()) => Some(0),
        Err(e) => {
            eprintln!("skylatch-ui --mesh-helper {op}: {e}");
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_elevated_helper_is_this_executable_with_quoted_arguments() {
        let args = helper_args(
            "apply",
            Path::new("/Users/max/Library/Application Support/Skylatch"),
            Some(Path::new("/usr/local/bin/easytier-core")),
        );
        assert_eq!(
            args,
            [
                "--mesh-helper",
                "apply",
                "/Users/max/Library/Application Support/Skylatch",
                "/usr/local/bin/easytier-core"
            ]
        );
        let script = applescript(Path::new("/Applications/Sky latch.app/x\"y"), &args);
        assert!(script.starts_with("do shell script \"'/Applications/Sky latch.app/x\\\"y'"));
        assert!(script.contains("'/Users/max/Library/Application Support/Skylatch'"));
        assert!(script.ends_with("with administrator privileges"));
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(run_helper(&["--demo".into()]), None);
        assert_eq!(run_helper(&["--mesh-helper".into()]), Some(2));
    }

    fn temp_paths() -> MeshPaths {
        MeshPaths::new(std::env::temp_dir().join(format!(
            "skylatch-ui-mesh-{}",
            skylatch_proto::credentials::random_token().unwrap()
        )))
    }

    #[test]
    fn writing_detaching_and_clearing_touch_only_the_temp_directory() {
        let paths = temp_paths();
        let mut vault = MeshVault::new_default().unwrap();
        vault.profiles.push(vault.next_isolated_defaults().unwrap());
        let mut ids: Vec<String> = vault.profiles.iter().map(|p| p.id.clone()).collect();
        ids.sort();
        let msg = apply_local(ApplyRequest {
            paths: paths.clone(),
            vault: vault.clone(),
            selected: ids.clone(),
            hub_peers: vec!["tcp://hub.example.com:11010".into()],
            no_tun: true,
            vault_version: Some(2),
            hub_url: "wss://hub.example.com".into(),
            start: false,
        })
        .unwrap();
        assert!(msg.contains('2'));
        let state = load_state(&paths);
        assert!(matches!(
            &state.binding,
            MeshBindingMode::Managed { active_profile_ids, no_tun: true, vault_version: Some(2), .. }
                if *active_profile_ids == ids
        ));
        let saved = std::fs::read_to_string(paths.state_file()).unwrap();
        assert!(!saved.contains(&vault.profiles[0].network_secret));

        let p = probe(Some(&paths));
        assert_eq!(p.managed_ids.len(), 2);
        assert!(p.bound() && p.no_tun() && !p.user_process);
        assert_eq!(p.vault_version(), Some(2));

        assert!(
            apply_local(ApplyRequest {
                paths: paths.clone(),
                vault: vault.clone(),
                selected: vec![],
                hub_peers: vec![],
                no_tun: false,
                vault_version: None,
                hub_url: String::new(),
                start: false,
            })
            .is_err()
        );

        detach(&paths).unwrap();
        assert_eq!(load_state(&paths).binding, MeshBindingMode::Unbound);
        assert!(paths.profiles_dir().join("net-default.toml").is_file());
        rebind(&paths, "wss://hub.example.com").unwrap();
        assert!(matches!(
            load_state(&paths).binding,
            MeshBindingMode::Managed { .. }
        ));
        stop_and_clear(&paths).unwrap();
        assert!(!paths.profiles_dir().exists());
        assert_eq!(load_state(&paths).binding, MeshBindingMode::Unbound);
        let _ = std::fs::remove_dir_all(&paths.app_dir);
    }

    #[test]
    fn a_new_vault_opens_with_its_password_or_its_recovery_key() {
        let s = new_vault("correct horse battery").unwrap();
        assert_eq!(s.envelope.version, 0);
        let opened = unlock(&s.envelope, "correct horse battery", UnlockMode::Password).unwrap();
        assert_eq!(opened.vault, s.vault);
        assert!(
            unlock(&s.envelope, "wrong", UnlockMode::Password)
                .err()
                .unwrap()
                .contains("密码")
        );
        let code = mesh_vault::format_recovery_key(&s.key);
        assert_eq!(
            unlock(&s.envelope, &code, UnlockMode::Recovery)
                .unwrap()
                .vault,
            s.vault
        );
        assert!(
            unlock(&s.envelope, "SLK1-XXXX", UnlockMode::Recovery)
                .err()
                .unwrap()
                .contains("恢复码")
        );
    }
}

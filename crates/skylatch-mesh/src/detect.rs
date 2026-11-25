//! Local EasyTier binaries, running instances, and software known to conflict with them.

use crate::process::{list_processes, ProcessInfo};
use crate::toml_config::{is_managed_dir, managed_profile_ids};
use crate::{install, status, MeshPaths, EASYTIER_DEFAULT_RPC_PORTAL, MANAGED_RPC_PORTAL};
use serde::Serialize;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EasyTierBinaries {
    pub core: Option<PathBuf>,
    pub cli: Option<PathBuf>,
}

fn exe(base: &str) -> String {
    format!("{base}{}", std::env::consts::EXE_SUFFIX)
}

fn search_dirs(extra: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = extra.to_vec();
    if let Some(target) = install::EasyTierTarget::current() {
        dirs.push(install::system_install_dir(target));
    }
    dirs.push(crate::system_app_dir().join("easytier"));
    if let Some(user) = MeshPaths::user_default() {
        dirs.push(user.profiles_dir());
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(dir) = std::env::var_os(var) {
                dirs.push(PathBuf::from(dir).join("EasyTier"));
            }
        }
    } else {
        for dir in [
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/usr/bin",
            "/opt/easytier",
        ] {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs
}

/// Finds `easytier-core` and `easytier-cli` in `extra_dirs`, Skylatch's install and managed
/// directories, `PATH`, and the usual install locations, in that order.
pub fn find_binaries(extra_dirs: &[PathBuf]) -> EasyTierBinaries {
    let dirs = search_dirs(extra_dirs);
    let find = |name: &str| {
        let name = exe(name);
        dirs.iter().map(|d| d.join(&name)).find(|p| p.is_file())
    };
    EasyTierBinaries {
        core: find("easytier-core"),
        cli: find("easytier-cli"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EasyTierPresence {
    /// No EasyTier binary and no running instance.
    NotInstalled,
    /// EasyTier is installed but nothing runs and Skylatch manages no profiles.
    InstalledIdle { easytier_core: PathBuf },
    ManagedRunning {
        rpc_portal: String,
        profiles_dir: PathBuf,
        active_profile_ids: Vec<String>,
    },
    ManagedStopped {
        profiles_dir: PathBuf,
        configured_profile_ids: Vec<String>,
    },
    /// `easytier-core --config-server`: configured remotely, read-only for Skylatch.
    ExternalConfigServer { rpc_portal: String, summary: String },
    ExternalGuiOrManual {
        rpc_portal: Option<String>,
        overlay_ips: Vec<String>,
        summary: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstanceKind {
    Managed {
        profiles_dir: PathBuf,
        ids: Vec<String>,
    },
    ConfigServer(String),
    Manual,
    Gui,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Instance {
    pub pid: u32,
    pub kind: InstanceKind,
    pub rpc_portal: Option<String>,
}

fn base_name(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    name.strip_suffix(".exe")
        .unwrap_or(name)
        .to_ascii_lowercase()
}

fn process_base(process: &ProcessInfo) -> String {
    match process.cmd.first() {
        Some(argv0) if process.name.is_empty() => base_name(argv0),
        _ => base_name(&process.name),
    }
}

/// Values of `long`/`short` in `args`: `--opt v`, `--opt=v`, `-o v`, and the extra values clap
/// accepts after multi-value options such as `-c a.toml b.toml`.
fn option_values(args: &[String], long: &str, short: Option<&str>, multi: bool) -> Vec<String> {
    let mut values = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if let Some(value) = arg.strip_prefix(long).and_then(|v| v.strip_prefix('=')) {
            values.push(value.to_string());
        } else if arg == long || short == Some(arg.as_str()) {
            while i < args.len() && !args[i].starts_with('-') {
                values.push(args[i].clone());
                i += 1;
                if !multi {
                    break;
                }
            }
        }
    }
    values
}

/// Redacts the user key in a config-server URL such as `tcp://host:22020/<key>`.
fn redact_config_server(server: &str) -> String {
    match server.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.split('/').next().unwrap_or(rest);
            format!("{scheme}://{host}")
        }
        None => "official config server".into(),
    }
}

fn classify_core(args: &[String]) -> (InstanceKind, Option<String>) {
    let rpc_portal = option_values(args, "--rpc-portal", Some("-r"), false)
        .pop()
        .map(|portal| match portal.parse::<u16>() {
            Ok(port) => format!("127.0.0.1:{port}"),
            Err(_) => portal,
        });
    if let Some(server) = option_values(args, "--config-server", Some("-w"), false).pop() {
        return (
            InstanceKind::ConfigServer(redact_config_server(&server)),
            rpc_portal,
        );
    }
    let files = option_values(args, "--config-file", Some("-c"), true);
    let managed = files.iter().map(PathBuf::from).find_map(|file| {
        let dir = file.parent()?.to_path_buf();
        is_managed_dir(&dir).then_some(dir)
    });
    let kind = match managed {
        Some(profiles_dir) => {
            let mut ids: Vec<String> = files
                .iter()
                .filter_map(|f| Path::new(f).file_name()?.to_str().map(String::from))
                .filter_map(|n| Some(n.strip_prefix("net-")?.strip_suffix(".toml")?.to_string()))
                .collect();
            ids.sort();
            InstanceKind::Managed { profiles_dir, ids }
        }
        None => InstanceKind::Manual,
    };
    (kind, rpc_portal)
}

pub(crate) fn instances(processes: &[ProcessInfo]) -> Vec<Instance> {
    let mut found: Vec<Instance> = processes
        .iter()
        .filter_map(|process| {
            let base = process_base(process);
            if base.starts_with("easytier-gui") {
                return Some(Instance {
                    pid: process.pid,
                    kind: InstanceKind::Gui,
                    rpc_portal: None,
                });
            }
            if base != "easytier-core" {
                return None;
            }
            let (kind, rpc_portal) = classify_core(process.cmd.get(1..).unwrap_or_default());
            Some(Instance {
                pid: process.pid,
                kind,
                rpc_portal,
            })
        })
        .collect();
    found.sort_by_key(|i| i.pid);
    found
}

/// The overlay addresses and hostname reported by an instance's RPC portal.
pub(crate) type NodeLookup<'a> = &'a dyn Fn(&str) -> Option<status::MeshRuntimeStatus>;

pub(crate) fn classify(
    instances: &[Instance],
    managed_dirs: &[PathBuf],
    core: Option<&Path>,
    lookup: NodeLookup<'_>,
) -> EasyTierPresence {
    if let Some((instance, profiles_dir, ids)) = instances.iter().find_map(|i| match &i.kind {
        InstanceKind::Managed { profiles_dir, ids } => Some((i, profiles_dir, ids)),
        _ => None,
    }) {
        return EasyTierPresence::ManagedRunning {
            rpc_portal: instance
                .rpc_portal
                .clone()
                .unwrap_or_else(|| EASYTIER_DEFAULT_RPC_PORTAL.into()),
            profiles_dir: profiles_dir.clone(),
            active_profile_ids: ids.clone(),
        };
    }
    let describe = |instance: &Instance, what: String| {
        let portal = instance
            .rpc_portal
            .as_deref()
            .unwrap_or(EASYTIER_DEFAULT_RPC_PORTAL);
        let node = (instance.kind != InstanceKind::Gui || instance.rpc_portal.is_some())
            .then(|| lookup(portal))
            .flatten();
        let overlay_ips: Vec<String> = node.iter().filter_map(|n| n.local_ipv4.clone()).collect();
        let mut summary = format!("{what} (PID {})", instance.pid);
        if let Some(node) = &node {
            if !node.local_hostname.is_empty() {
                summary.push_str(&format!(", hostname {}", node.local_hostname));
            }
            for ip in &overlay_ips {
                summary.push_str(&format!(", {ip}"));
            }
            summary.push_str(&format!(", {} nodes", node.peers.len()));
        }
        (portal.to_string(), overlay_ips, summary, node.is_some())
    };
    if let Some((instance, server)) = instances.iter().find_map(|i| match &i.kind {
        InstanceKind::ConfigServer(server) => Some((i, server)),
        _ => None,
    }) {
        let (rpc_portal, _, summary, _) =
            describe(instance, format!("easytier-core configured by {server}"));
        return EasyTierPresence::ExternalConfigServer {
            rpc_portal,
            summary,
        };
    }
    if let Some(instance) = instances.first() {
        let what = if instance.kind == InstanceKind::Gui {
            "easytier-gui"
        } else {
            "easytier-core"
        };
        let (portal, overlay_ips, summary, answered) = describe(instance, what.into());
        return EasyTierPresence::ExternalGuiOrManual {
            rpc_portal: (answered || instance.kind != InstanceKind::Gui).then_some(portal),
            overlay_ips,
            summary,
        };
    }
    if let Some(dir) = managed_dirs.iter().find(|d| is_managed_dir(d)) {
        return EasyTierPresence::ManagedStopped {
            profiles_dir: dir.clone(),
            configured_profile_ids: managed_profile_ids(dir).unwrap_or_default(),
        };
    }
    match core {
        Some(core) => EasyTierPresence::InstalledIdle {
            easytier_core: core.to_path_buf(),
        },
        None => EasyTierPresence::NotInstalled,
    }
}

/// Detects the EasyTier state of this machine. `paths` are the Skylatch directories whose
/// managed profiles count; running instances are recognized from their command lines, and
/// external ones are queried read-only through `easytier-cli`.
pub fn detect_presence(paths: &[MeshPaths], binaries: &EasyTierBinaries) -> EasyTierPresence {
    let managed: Vec<PathBuf> = paths.iter().map(MeshPaths::profiles_dir).collect();
    let lookup = |portal: &str| {
        let cli = binaries.cli.as_deref()?;
        status::query_status(cli, Some(portal), None).ok()
    };
    classify(
        &instances(&list_processes()),
        &managed,
        binaries.core.as_deref(),
        &lookup,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictKind {
    /// Tailscale on Windows removes Wintun adapters it does not own when it starts or updates.
    TailscaleWintun,
    /// A Clash / mihomo TUN captures EasyTier's underlay traffic and the overlay routes.
    ClashTun,
    /// An EasyTier instance Skylatch does not manage runs on this machine.
    ExternalEasyTier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictSeverity {
    Warning,
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MeshConflict {
    pub kind: ConflictKind,
    pub title: String,
    pub detail: String,
    pub severity: ConflictSeverity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Os {
    Windows,
    Macos,
    Linux,
}

impl Os {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::Macos
        } else {
            Self::Linux
        }
    }
}

fn is_clash_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.contains("mihomo") || name.contains("clash")
}

/// mihomo's default TUN and fake-IP range, `198.18.0.0/15`.
fn is_fake_ip(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v4) if v4.octets()[0] == 198 && v4.octets()[1] & 0xfe == 18)
}

pub(crate) fn conflicts_from(
    os: Os,
    processes: &[ProcessInfo],
    interfaces: &[(String, IpAddr)],
) -> Vec<MeshConflict> {
    let mut conflicts = Vec::new();
    let names: Vec<String> = processes.iter().map(process_base).collect();

    if os == Os::Windows
        && (names
            .iter()
            .any(|n| n == "tailscaled" || n == "tailscale-ipn")
            || interfaces
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("Tailscale")))
    {
        conflicts.push(MeshConflict {
            kind: ConflictKind::TailscaleWintun,
            title: "Tailscale 可能清理 EasyTier 的 Wintun 网卡".into(),
            detail: "Tailscale 与 EasyTier 都使用 Wintun 驱动。Tailscale 服务启动或升级时会清理不属于它的 Wintun 适配器，EasyTier 网卡可能因此消失；出现后重启 Skylatch 托管的 EasyTier 服务即可恢复。".into(),
            severity: ConflictSeverity::Warning,
        });
    }

    let clash_process = processes
        .iter()
        .zip(&names)
        .find(|(_, name)| is_clash_name(name))
        .map(|(p, name)| format!("进程 {name} (PID {})", p.pid));
    let clash_tun = interfaces
        .iter()
        .find(|(name, ip)| {
            let lower = name.to_ascii_lowercase();
            ["meta", "mihomo", "clash"]
                .iter()
                .any(|p| lower.starts_with(p))
                || is_fake_ip(ip)
        })
        .map(|(name, ip)| format!("网卡 {name} ({ip})"));
    let evidence: Vec<String> = clash_process.into_iter().chain(clash_tun).collect();
    if !evidence.is_empty() {
        conflicts.push(MeshConflict {
            kind: ConflictKind::ClashTun,
            title: "检测到 Clash / mihomo，TUN 模式会接管 EasyTier 流量".into(),
            detail: format!(
                "{}。若开启了 TUN，请把 EasyTier 组网网段和对等节点地址加入绕过（如 route-exclude-address），或为本机组网启用无 TUN 模式（no_tun）。",
                evidence.join("、")
            ),
            severity: ConflictSeverity::Warning,
        });
    }

    for instance in instances(processes) {
        let what = match &instance.kind {
            InstanceKind::Managed { .. } => continue,
            InstanceKind::ConfigServer(server) => {
                format!("由 {server} 远程下发配置的 easytier-core")
            }
            InstanceKind::Manual => "手动启动的 easytier-core".into(),
            InstanceKind::Gui => "EasyTier GUI".into(),
        };
        let portal = instance.rpc_portal.as_deref();
        let same_portal = portal == Some(MANAGED_RPC_PORTAL);
        conflicts.push(MeshConflict {
            kind: ConflictKind::ExternalEasyTier,
            title: "本机已有非 Skylatch 管理的 EasyTier".into(),
            detail: format!(
                "{what}（PID {}，RPC {}）正在运行。Skylatch 不会改动它；托管服务使用独立 RPC {MANAGED_RPC_PORTAL} 与各组网自己的监听端口，请确认两边的网段和监听端口不重叠。{}",
                instance.pid,
                portal.unwrap_or(EASYTIER_DEFAULT_RPC_PORTAL),
                if same_portal { "它占用了托管服务的 RPC 端口，必须先改掉其中一个。" } else { "" }
            ),
            severity: if same_portal {
                ConflictSeverity::Blocking
            } else {
                ConflictSeverity::Warning
            },
        });
    }
    conflicts
}

/// Known conflicts on this machine, from running processes and network interfaces.
pub fn detect_conflicts() -> Vec<MeshConflict> {
    let interfaces: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|i| (i.name, i.addr.ip()))
        .collect();
    conflicts_from(Os::current(), &list_processes(), &interfaces)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::parse_status;

    fn process(pid: u32, cmd: &str) -> ProcessInfo {
        let cmd: Vec<String> = cmd.split(' ').map(String::from).collect();
        ProcessInfo {
            pid,
            name: base_name(&cmd[0]),
            cmd,
        }
    }

    const CONFIG_SERVER: &str = "/usr/local/bin/easytier-core --config-server tcp://config.example.com:22020/SECRETKEY --machine-id lab-mac --hostname lab-mac";

    fn node(_: &str) -> Option<status::MeshRuntimeStatus> {
        parse_status(
            r#"[{"hostname":"me","cost":"Local","ipv4":"10.126.126.3"},{"hostname":"pve","cost":"relay(2)"}]"#,
            Some(r#"{"hostname":"me","ipv4_addr":"10.126.126.3/24"}"#),
        )
        .ok()
    }

    #[test]
    fn command_line_options() {
        let args: Vec<String> =
            "-c a.toml b.toml --rpc-portal=1.2.3.4:5 -r 15999 --config-file c.toml --no-tun"
                .split(' ')
                .map(String::from)
                .collect();
        assert_eq!(
            option_values(&args, "--config-file", Some("-c"), true),
            ["a.toml", "b.toml", "c.toml"]
        );
        assert_eq!(
            option_values(&args, "--rpc-portal", Some("-r"), false),
            ["1.2.3.4:5", "15999"]
        );
        assert_eq!(classify_core(&args).1.as_deref(), Some("127.0.0.1:15999"));
    }

    #[test]
    fn config_server_instances_are_external_and_redacted() {
        let found = instances(&[process(371, CONFIG_SERVER), process(1, "/sbin/launchd")]);
        assert_eq!(found.len(), 1);
        let presence = classify(&found, &[], None, &node);
        let EasyTierPresence::ExternalConfigServer {
            rpc_portal,
            summary,
        } = presence
        else {
            panic!("{presence:?}");
        };
        assert_eq!(rpc_portal, EASYTIER_DEFAULT_RPC_PORTAL);
        assert!(
            summary.contains("tcp://config.example.com:22020"),
            "{summary}"
        );
        assert!(summary.contains("10.126.126.3"), "{summary}");
        assert!(!summary.contains("SECRETKEY"));
    }

    #[test]
    fn managed_instances_are_recognized_by_their_marker() {
        let dir = std::env::temp_dir().join(format!(
            "skylatch-detect-{}",
            skylatch_proto::credentials::random_token().unwrap()
        ));
        crate::toml_config::ensure_managed_dir(&dir).unwrap();
        std::fs::write(dir.join("net-lab.toml"), "").unwrap();
        let file = dir.join("net-lab.toml");
        let managed = ProcessInfo {
            pid: 9,
            name: "easytier-core".into(),
            cmd: vec![
                "easytier-core".into(),
                "--rpc-portal".into(),
                MANAGED_RPC_PORTAL.into(),
                "-c".into(),
                file.to_string_lossy().into_owned(),
            ],
        };
        let found = instances(&[managed.clone(), process(371, CONFIG_SERVER)]);
        assert_eq!(
            classify(&found, &[], None, &node),
            EasyTierPresence::ManagedRunning {
                rpc_portal: MANAGED_RPC_PORTAL.into(),
                profiles_dir: dir.clone(),
                active_profile_ids: vec!["lab".into()],
            }
        );
        assert_eq!(
            classify(&[], std::slice::from_ref(&dir), None, &node),
            EasyTierPresence::ManagedStopped {
                profiles_dir: dir.clone(),
                configured_profile_ids: vec!["lab".into()],
            }
        );
        let conflicts = conflicts_from(Os::Macos, &[managed], &[]);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn manual_gui_and_absent() {
        let manual = instances(&[process(
            5,
            "C:\\EasyTier\\easytier-core.exe -c C:\\my\\home.toml",
        )]);
        let presence = classify(&manual, &[], None, &node);
        assert!(
            matches!(
                &presence,
                EasyTierPresence::ExternalGuiOrManual { rpc_portal: Some(_), overlay_ips, .. }
                    if overlay_ips == &["10.126.126.3"]
            ),
            "{presence:?}"
        );

        let gui = instances(&[process(
            6,
            "/Applications/easytier-gui.app/Contents/MacOS/easytier-gui",
        )]);
        assert!(matches!(
            classify(&gui, &[], None, &node),
            EasyTierPresence::ExternalGuiOrManual {
                rpc_portal: None,
                ..
            }
        ));
        assert_eq!(
            classify(&[], &[], None, &node),
            EasyTierPresence::NotInstalled
        );
        assert_eq!(
            classify(
                &[],
                &[],
                Some(Path::new("/usr/local/bin/easytier-core")),
                &node
            ),
            EasyTierPresence::InstalledIdle {
                easytier_core: "/usr/local/bin/easytier-core".into()
            }
        );
    }

    #[test]
    fn conflicts_need_evidence() {
        let quiet = [process(1, "/sbin/launchd"), process(2, "/usr/bin/ssh")];
        let lan: Vec<(String, IpAddr)> = vec![("en0".into(), "192.168.1.20".parse().unwrap())];
        assert!(conflicts_from(Os::Windows, &quiet, &lan).is_empty());

        let tailscale = [process(3, "C:\\Tailscale\\tailscaled.exe")];
        let kinds = |c: Vec<MeshConflict>| c.into_iter().map(|c| c.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds(conflicts_from(Os::Windows, &tailscale, &[])),
            [ConflictKind::TailscaleWintun]
        );
        assert!(conflicts_from(Os::Macos, &tailscale, &[]).is_empty());

        let meta: Vec<(String, IpAddr)> = vec![("Meta".into(), "198.18.0.1".parse().unwrap())];
        assert_eq!(
            kinds(conflicts_from(Os::Windows, &quiet, &meta)),
            [ConflictKind::ClashTun]
        );
        let utun: Vec<(String, IpAddr)> = vec![("utun4".into(), "198.19.0.1".parse().unwrap())];
        assert_eq!(
            kinds(conflicts_from(Os::Macos, &quiet, &utun)),
            [ConflictKind::ClashTun]
        );
        let verge = [process(
            4,
            "/Applications/ClashVerge.app/Contents/MacOS/verge-mihomo",
        )];
        assert_eq!(
            kinds(conflicts_from(Os::Macos, &verge, &[])),
            [ConflictKind::ClashTun]
        );

        let external = conflicts_from(Os::Macos, &[process(371, CONFIG_SERVER)], &[]);
        assert_eq!(external.len(), 1);
        assert_eq!(external[0].kind, ConflictKind::ExternalEasyTier);
        assert_eq!(external[0].severity, ConflictSeverity::Warning);
        assert!(!external[0].detail.contains("SECRETKEY"));
        let squatter = conflicts_from(
            Os::Linux,
            &[process(8, "easytier-core -r 15889 -c /etc/et.toml")],
            &[],
        );
        assert_eq!(squatter[0].severity, ConflictSeverity::Blocking);
    }
}

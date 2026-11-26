//! Standard EasyTier `config.toml` generation, the Skylatch-managed profile directory, and import
//! of TOML written by the official GUI or by hand.
//!
//! Address assignment: with `dhcp = true`, `easytier-core` takes the subnet of any peer that
//! already has an address and moves to its lowest free host, re-picking on conflicts. With no
//! addressed peer it falls back to `10.126.126.0/24`, so `dhcp` alone cannot keep a network in
//! its profile subnet. Every generated file therefore also sets a seed `ipv4` inside the profile
//! subnet: the node brings its TUN up with it and announces it, which anchors the network to the
//! profile subnet, and DHCP then settles each node on a conflict-free address there.

use crate::{MeshError, MANAGED_MARKER, MANAGED_RPC_PORTAL};
use serde::Serialize;
use skylatch_proto::credentials::write_private_file;
use skylatch_proto::mesh::{
    is_valid_peer_url, parse_private_subnet, MeshProfile, MeshVault, VAULT_SCHEMA_VERSION,
};
use std::fs;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
struct ConfigToml<'a> {
    instance_name: String,
    hostname: &'a str,
    ipv4: String,
    dhcp: bool,
    listeners: Vec<String>,
    network_identity: NetworkIdentity<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    peer: Vec<Peer<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<Flags>,
}

#[derive(Serialize)]
struct NetworkIdentity<'a> {
    network_name: &'a str,
    network_secret: &'a str,
}

#[derive(Serialize)]
struct Peer<'a> {
    uri: &'a str,
}

#[derive(Serialize)]
struct Flags {
    no_tun: bool,
}

/// `instance_name` of a profile, also accepted by `easytier-cli -n`.
pub fn instance_name(profile_id: &str) -> String {
    format!("skylatch-{profile_id}")
}

pub fn profile_file_name(profile_id: &str) -> String {
    format!("net-{profile_id}.toml")
}

/// A stable host number in `1..=253` so fresh nodes rarely start on the same seed address.
fn seed_host(hostname: &str) -> u8 {
    let hash = hostname.bytes().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(b)).wrapping_mul(0x0100_0193)
    });
    (hash % 253) as u8 + 1
}

/// Hub peers first when the profile follows the hub, then the profile's own, without duplicates.
pub fn merged_peers(profile: &MeshProfile, hub_peers: &[String]) -> Vec<String> {
    let hub = hub_peers
        .iter()
        .filter(|_| profile.use_hub_peer)
        .filter(|p| is_valid_peer_url(p));
    let mut peers: Vec<String> = Vec::new();
    for peer in hub.chain(&profile.custom_peers) {
        if !peers.contains(peer) {
            peers.push(peer.clone());
        }
    }
    peers
}

/// The EasyTier `config.toml` for one profile.
pub fn profile_toml(
    profile: &MeshProfile,
    hub_peers: &[String],
    hostname: &str,
    no_tun: bool,
) -> Result<String, MeshError> {
    profile.validate()?;
    let network = parse_private_subnet(&profile.subnet).ok_or(MeshError::MissingField("subnet"))?;
    let hostname = match hostname.trim() {
        "" => "skylatch-host",
        name => name,
    };
    let [a, b, c, _] = network.octets();
    let peers = merged_peers(profile, hub_peers);
    let config = ConfigToml {
        instance_name: instance_name(&profile.id),
        hostname,
        ipv4: format!("{}/24", Ipv4Addr::new(a, b, c, seed_host(hostname))),
        dhcp: true,
        listeners: vec![
            format!("tcp://0.0.0.0:{}", profile.listen_port),
            format!("udp://0.0.0.0:{}", profile.listen_port),
        ],
        network_identity: NetworkIdentity {
            network_name: &profile.network_name,
            network_secret: &profile.network_secret,
        },
        peer: peers.iter().map(|uri| Peer { uri }).collect(),
        flags: no_tun.then_some(Flags { no_tun: true }),
    };
    let body = toml::to_string(&config).map_err(|e| MeshError::Toml(e.to_string()))?;
    Ok(format!(
        "# Managed by Skylatch (mesh profile `{}`); edits are overwritten.\n\
         # `ipv4` is only a seed inside {}: `dhcp = true` moves this node to a free address there.\n\
         {body}",
        profile.id, profile.subnet
    ))
}

/// How [`write_managed_profiles`] writes and what the launch uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedWriteOptions {
    pub hostname: String,
    pub no_tun: bool,
    pub rpc_portal: String,
    /// Also delete managed files of profiles not passed in `profiles`. Without it they are kept.
    pub exclusive: bool,
}

impl ManagedWriteOptions {
    pub fn new(hostname: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            no_tun: false,
            rpc_portal: MANAGED_RPC_PORTAL.into(),
            exclusive: false,
        }
    }
}

/// The `easytier-core` invocation for every profile in a managed directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedLaunch {
    pub rpc_portal: String,
    pub profile_ids: Vec<String>,
    pub config_files: Vec<PathBuf>,
}

impl ManagedLaunch {
    pub fn scan(dir: &Path, rpc_portal: &str) -> io::Result<Self> {
        let profile_ids = managed_profile_ids(dir)?;
        Ok(Self {
            rpc_portal: rpc_portal.into(),
            config_files: profile_ids
                .iter()
                .map(|id| dir.join(profile_file_name(id)))
                .collect(),
            profile_ids,
        })
    }

    /// Arguments after the `easytier-core` path.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec!["--rpc-portal".to_string(), self.rpc_portal.clone()];
        for file in &self.config_files {
            args.push("-c".into());
            args.push(file.to_string_lossy().into_owned());
        }
        args
    }
}

/// Profile ids of the `net-<id>.toml` files in `dir`, sorted.
pub fn managed_profile_ids(dir: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let id = name.strip_prefix("net-")?.strip_suffix(".toml")?;
            (!id.is_empty()).then(|| id.to_string())
        })
        .collect();
    ids.sort();
    Ok(ids)
}

pub fn is_managed_dir(dir: &Path) -> bool {
    dir.join(MANAGED_MARKER).is_file()
}

/// Creates `dir` as a Skylatch-managed directory, refusing any non-empty directory it does not own.
pub(crate) fn ensure_managed_dir(dir: &Path) -> Result<(), MeshError> {
    if is_managed_dir(dir) {
        return Ok(());
    }
    match fs::read_dir(dir).map(|mut entries| entries.next().is_some()) {
        Ok(true) => return Err(MeshError::NotManaged(dir.to_path_buf())),
        Ok(false) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => create_private_dir(dir)?,
        Err(e) => return Err(e.into()),
    }
    write_private_file(
        &dir.join(MANAGED_MARKER),
        b"EasyTier profiles managed by Skylatch.\n",
    )?;
    Ok(())
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)
}

fn overlaps(subnet_a: Option<Ipv4Addr>, port_a: Option<u16>, profile: &MeshProfile) -> bool {
    subnet_a.is_some() && subnet_a == parse_private_subnet(&profile.subnet)
        || port_a == Some(profile.listen_port)
}

/// Writes `net-<id>.toml` (owner-only) for each selected profile into the Skylatch-managed `dir`
/// and returns the launch covering every profile now in it. Managed files of profiles in
/// `profiles` that are not selected are removed; other existing files are kept unless
/// `opts.exclusive`, and must not share a subnet or listen port with the selection.
pub fn write_managed_profiles(
    dir: &Path,
    profiles: &[MeshProfile],
    selected_ids: &[String],
    hub_peers: &[String],
    opts: &ManagedWriteOptions,
) -> Result<ManagedLaunch, MeshError> {
    let selected = selected_ids
        .iter()
        .map(|id| {
            profiles
                .iter()
                .find(|p| &p.id == id)
                .ok_or_else(|| MeshError::UnknownProfile(id.clone()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if !selected.is_empty() {
        MeshVault {
            schema_version: VAULT_SCHEMA_VERSION,
            profiles: selected.iter().map(|p| (*p).clone()).collect(),
        }
        .validate()?;
    }
    let rendered = selected
        .iter()
        .map(|p| {
            Ok((
                p.id.as_str(),
                profile_toml(p, hub_peers, &opts.hostname, opts.no_tun)?,
            ))
        })
        .collect::<Result<Vec<_>, MeshError>>()?;

    ensure_managed_dir(dir)?;
    let mut stale = Vec::new();
    for id in managed_profile_ids(dir)? {
        if selected_ids.contains(&id) {
            continue;
        }
        let path = dir.join(profile_file_name(&id));
        if opts.exclusive || profiles.iter().any(|p| p.id == id) {
            stale.push(path);
            continue;
        }
        let kept = parse_fields(&fs::read_to_string(&path)?)?;
        let (subnet, port) = (kept.subnet(), kept.listen_port());
        if let Some(clash) = selected.iter().find(|p| overlaps(subnet, port, p)) {
            return Err(MeshError::ManagedConflict(clash.id.clone(), id));
        }
    }
    for path in stale {
        fs::remove_file(path)?;
    }
    for (id, toml) in rendered {
        write_private_file(&dir.join(profile_file_name(id)), toml.as_bytes())?;
    }
    Ok(ManagedLaunch::scan(dir, &opts.rpc_portal)?)
}

#[derive(Default)]
struct ParsedToml {
    instance_name: Option<String>,
    network_name: Option<String>,
    network_secret: Option<String>,
    ipv4: Option<String>,
    listeners: Vec<String>,
    peers: Vec<String>,
}

fn parse_fields(text: &str) -> Result<ParsedToml, MeshError> {
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| MeshError::Toml(e.message().to_string()))?;
    let string = |value: Option<&toml::Value>| {
        value
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    let identity = table.get("network_identity");
    let strings = |value: Option<&toml::Value>| -> Vec<String> {
        value
            .and_then(toml::Value::as_array)
            .map(|items| items.iter().filter_map(|v| string(Some(v))).collect())
            .unwrap_or_default()
    };
    Ok(ParsedToml {
        instance_name: string(table.get("instance_name")),
        network_name: string(identity.and_then(|i| i.get("network_name"))),
        network_secret: identity
            .and_then(|i| i.get("network_secret"))
            .and_then(toml::Value::as_str)
            .map(String::from),
        ipv4: string(table.get("ipv4")),
        listeners: strings(table.get("listeners")),
        peers: table
            .get("peer")
            .and_then(toml::Value::as_array)
            .map(|peers| peers.iter().filter_map(|p| string(p.get("uri"))).collect())
            .unwrap_or_default(),
    })
}

impl ParsedToml {
    /// The private `/24` holding `ipv4`; an address without a prefix counts as `/24`.
    fn subnet(&self) -> Option<Ipv4Addr> {
        let ipv4 = self.ipv4.as_deref()?;
        let (addr, prefix) = ipv4.split_once('/').unwrap_or((ipv4, "24"));
        let [a, b, c, _] = addr.trim().parse::<Ipv4Addr>().ok()?.octets();
        parse_private_subnet(&format!("{a}.{b}.{c}.0/{}", prefix.trim()))
    }

    /// Port of the first TCP/UDP listener: `tcp://0.0.0.0:11010`, `udp://[::]:11010` or `11010`.
    fn listen_port(&self) -> Option<u16> {
        self.listeners.iter().find_map(|listener| {
            let port = match listener.split_once("://") {
                Some(("tcp" | "udp", rest)) => rest.trim_end_matches('/').rsplit_once(':')?.1,
                Some(_) => return None,
                None => listener.as_str(),
            };
            port.parse().ok().filter(|p| *p != 0)
        })
    }
}

/// Converts an EasyTier TOML (official GUI export or hand-written) into a new profile. A missing,
/// non-private or non-`/24` address, or a subnet or listen port already used in `existing_vault`,
/// is replaced by the vault's next isolated defaults; so is the profile id. The hub peer is used
/// only when the file lists no peers of its own.
pub fn import_easytier_toml(
    toml_text: &str,
    existing_vault: Option<&MeshVault>,
) -> Result<MeshProfile, MeshError> {
    let parsed = parse_fields(toml_text)?;
    let network_name = parsed
        .network_name
        .clone()
        .ok_or(MeshError::MissingField("[network_identity] network_name"))?;
    let network_secret = parsed
        .network_secret
        .clone()
        .ok_or(MeshError::MissingField("[network_identity] network_secret"))?;
    let empty = MeshVault {
        schema_version: VAULT_SCHEMA_VERSION,
        profiles: vec![],
    };
    let vault = existing_vault.unwrap_or(&empty);
    let defaults = vault.next_isolated_defaults()?;
    let subnet = parsed
        .subnet()
        .filter(|net| {
            vault
                .profiles
                .iter()
                .all(|p| parse_private_subnet(&p.subnet) != Some(*net))
        })
        .map_or(defaults.subnet, |net| format!("{net}/24"));
    let listen_port = parsed
        .listen_port()
        .filter(|port| vault.profiles.iter().all(|p| p.listen_port != *port))
        .unwrap_or(defaults.listen_port);
    let mut custom_peers: Vec<String> = Vec::new();
    for peer in parsed.peers.iter().filter(|p| is_valid_peer_url(p)) {
        if !custom_peers.contains(peer) {
            custom_peers.push(peer.clone());
        }
    }
    let name: String = parsed
        .instance_name
        .unwrap_or_else(|| network_name.clone())
        .chars()
        .take(64)
        .collect();
    let profile = MeshProfile {
        id: defaults.id,
        name,
        subnet,
        listen_port,
        network_name,
        network_secret,
        use_hub_peer: custom_peers.is_empty(),
        custom_peers,
    };
    profile.validate()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, third: u8, port: u16) -> MeshProfile {
        MeshProfile {
            id: id.into(),
            name: format!("Net {id}"),
            subnet: format!("10.126.{third}.0/24"),
            listen_port: port,
            network_name: format!("skylatch-{id}"),
            network_secret: format!("secret-{id}-0123456789abcdef"),
            custom_peers: vec!["tcp://own.example.com:11010".into()],
            use_hub_peer: true,
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "skylatch-mesh-{}",
            skylatch_proto::credentials::random_token().unwrap()
        ));
        dir
    }

    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn generated_toml_is_standard_easytier() {
        let hub = vec![
            "tcp://hub.example.com:11010".to_string(),
            "tcp://own.example.com:11010".to_string(),
        ];
        let text = profile_toml(&profile("lab", 2, 11011), &hub, "lab-vm", false).unwrap();
        let table: toml::Table = text.parse().unwrap();
        assert_eq!(table["instance_name"].as_str(), Some("skylatch-lab"));
        assert_eq!(table["hostname"].as_str(), Some("lab-vm"));
        assert_eq!(table["dhcp"].as_bool(), Some(true));
        let ipv4 = table["ipv4"].as_str().unwrap();
        assert!(
            ipv4.starts_with("10.126.2.") && ipv4.ends_with("/24"),
            "{ipv4}"
        );
        assert_eq!(
            table["listeners"].as_array().unwrap(),
            &vec![
                toml::Value::from("tcp://0.0.0.0:11011"),
                toml::Value::from("udp://0.0.0.0:11011")
            ]
        );
        let identity = &table["network_identity"];
        assert_eq!(identity["network_name"].as_str(), Some("skylatch-lab"));
        assert_eq!(
            identity["network_secret"].as_str(),
            Some("secret-lab-0123456789abcdef")
        );
        let peers: Vec<_> = table["peer"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["uri"].as_str().unwrap())
            .collect();
        assert_eq!(
            peers,
            ["tcp://hub.example.com:11010", "tcp://own.example.com:11010"]
        );
        assert!(table.get("flags").is_none());

        let mut own_only = profile("lab", 2, 11011);
        own_only.use_hub_peer = false;
        let table: toml::Table = profile_toml(&own_only, &hub, "", true)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(table["peer"].as_array().unwrap().len(), 1);
        assert_eq!(table["hostname"].as_str(), Some("skylatch-host"));
        assert_eq!(table["flags"]["no_tun"].as_bool(), Some(true));
    }

    #[test]
    fn seed_hosts_are_valid_and_stable() {
        for name in ["", "a", "maxclaw-mac", "很长的主机名-with-unicode"] {
            let host = seed_host(name);
            assert!((1..=253).contains(&host));
            assert_eq!(host, seed_host(name));
        }
    }

    #[test]
    fn managed_directory_holds_only_selected_profiles() {
        let dir = temp_dir();
        let _cleanup = Cleanup(dir.clone());
        let profiles = vec![profile("a", 1, 11010), profile("b", 2, 11011)];
        let opts = ManagedWriteOptions::new("host");
        let launch =
            write_managed_profiles(&dir, &profiles, &["a".into(), "b".into()], &[], &opts).unwrap();
        assert_eq!(launch.profile_ids, ["a", "b"]);
        assert_eq!(
            launch.args(),
            [
                "--rpc-portal".to_string(),
                MANAGED_RPC_PORTAL.into(),
                "-c".into(),
                dir.join("net-a.toml").to_string_lossy().into_owned(),
                "-c".into(),
                dir.join("net-b.toml").to_string_lossy().into_owned(),
            ]
        );
        assert!(is_managed_dir(&dir));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&dir.join("net-a.toml")), 0o600);
        }

        let launch = write_managed_profiles(&dir, &profiles, &["b".into()], &[], &opts).unwrap();
        assert_eq!(launch.profile_ids, ["b"]);

        let joined = vec![profile("c", 3, 11012)];
        let launch = write_managed_profiles(&dir, &joined, &["c".into()], &[], &opts).unwrap();
        assert_eq!(launch.profile_ids, ["b", "c"]);

        let clash = vec![profile("d", 2, 11020)];
        assert!(matches!(
            write_managed_profiles(&dir, &clash, &["d".into()], &[], &opts),
            Err(MeshError::ManagedConflict(new, old)) if new == "d" && old == "b"
        ));
        let clash = vec![profile("d", 9, 11012)];
        assert!(matches!(
            write_managed_profiles(&dir, &clash, &["d".into()], &[], &opts),
            Err(MeshError::ManagedConflict(_, old)) if old == "c"
        ));
        assert!(matches!(
            write_managed_profiles(&dir, &clash, &["zz".into()], &[], &opts),
            Err(MeshError::UnknownProfile(_))
        ));

        let exclusive = ManagedWriteOptions {
            exclusive: true,
            ..opts.clone()
        };
        let launch = write_managed_profiles(&dir, &clash, &["d".into()], &[], &exclusive).unwrap();
        assert_eq!(launch.profile_ids, ["d"]);
    }

    #[test]
    fn foreign_directories_are_never_written() {
        let dir = temp_dir();
        let _cleanup = Cleanup(dir.clone());
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("my-network.toml"), "instance_name = \"mine\"").unwrap();
        let profiles = vec![profile("a", 1, 11010)];
        assert!(matches!(
            write_managed_profiles(
                &dir,
                &profiles,
                &["a".into()],
                &[],
                &ManagedWriteOptions::new("h")
            ),
            Err(MeshError::NotManaged(_))
        ));
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    const GUI_EXPORT: &str = r#"
hostname = "office-desktop"
instance_name = "home"
instance_id = "4bab4e15-f48c-400f-82e5-d14f8c2324b5"
ipv4 = "10.144.3.7/24"
dhcp = false
listeners = ["tcp://0.0.0.0:11020", "udp://0.0.0.0:11020", "wg://0.0.0.0:11021"]
rpc_portal = "0.0.0.0:0"

[network_identity]
network_name = "home-net"
network_secret = "a-long-enough-secret"

[[peer]]
uri = "tcp://public.easytier.top:11010"

[[peer]]
uri = "tcp://public.easytier.top:11010"

[flags]
enable_kcp_proxy = true
"#;

    #[test]
    fn imports_official_gui_toml() {
        let profile = import_easytier_toml(GUI_EXPORT, None).unwrap();
        assert_eq!(profile.name, "home");
        assert_eq!(profile.subnet, "10.144.3.0/24");
        assert_eq!(profile.listen_port, 11020);
        assert_eq!(profile.network_name, "home-net");
        assert_eq!(profile.network_secret, "a-long-enough-secret");
        assert_eq!(profile.custom_peers, ["tcp://public.easytier.top:11010"]);
        assert!(!profile.use_hub_peer);

        let mut vault = MeshVault::new_default().unwrap();
        vault.profiles[0].subnet = "10.144.3.0/24".into();
        vault.profiles[0].listen_port = 11020;
        let profile = import_easytier_toml(GUI_EXPORT, Some(&vault)).unwrap();
        assert_eq!(profile.subnet, "10.126.1.0/24");
        assert_eq!(profile.listen_port, 11010);
        vault.profiles.push(profile);
        vault.validate().unwrap();
    }

    #[test]
    fn import_fills_missing_address_and_port() {
        let text =
            "[network_identity]\nnetwork_name = \"bare\"\nnetwork_secret = \"0123456789abcdef\"\n";
        let profile = import_easytier_toml(text, None).unwrap();
        assert_eq!(profile.name, "bare");
        assert_eq!(profile.subnet, "10.126.1.0/24");
        assert_eq!(profile.listen_port, 11010);
        assert!(profile.use_hub_peer);

        let public = text.replace(
            "[network_identity]",
            "ipv4 = \"8.8.8.8\"\nlisteners = [\"11030\"]\n[network_identity]",
        );
        let profile = import_easytier_toml(&public, None).unwrap();
        assert_eq!(profile.subnet, "10.126.1.0/24");
        assert_eq!(profile.listen_port, 11030);

        assert!(matches!(
            import_easytier_toml("instance_name = \"x\"", None),
            Err(MeshError::MissingField(_))
        ));
        assert!(matches!(
            import_easytier_toml("not toml [", None),
            Err(MeshError::Toml(_))
        ));
        let short = text.replace("0123456789abcdef", "short");
        assert!(matches!(
            import_easytier_toml(&short, None),
            Err(MeshError::Profile(_))
        ));
    }

    #[test]
    fn generated_toml_round_trips_through_import() {
        let original = profile("lab", 7, 11017);
        let text = profile_toml(&original, &[], "h", false).unwrap();
        let imported = import_easytier_toml(&text, None).unwrap();
        assert_eq!(imported.subnet, original.subnet);
        assert_eq!(imported.listen_port, original.listen_port);
        assert_eq!(imported.network_secret, original.network_secret);
        assert_eq!(imported.custom_peers, original.custom_peers);
    }
}

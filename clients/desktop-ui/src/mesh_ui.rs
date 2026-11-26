// SPDX-License-Identifier: Apache-2.0

//! The 虚拟组网 tab without GPUI: which of the six EasyTier situations this machine is in,
//! the polling rule, the peer table, the unlocked end-to-end encrypted vault and its editable
//! draft, and the dialogs (sign-out guard, TOML import, sync conflict).
//!
//! Secrets stay in memory only: the vault key is a zeroizing [`VaultKey`], the unlock text is
//! wiped when it is submitted, and nothing here implements `Debug` over secrets.

use ring::digest;
use skylatch_client_core::mesh_vault::{self, InviteSecret, VaultKey};
use skylatch_client_core::{MeshInviteCiphertext, MeshProfile, MeshVault, MeshVaultEnvelope};
use skylatch_mesh::{
    ConflictKind, ConflictSeverity, EasyTierPresence, MeshBindingMode, MeshConflict, MeshRouteKind,
    MeshRuntimeStatus, MeshState,
};
use skylatch_proto::mesh::{MeshError, VAULT_SCHEMA_VERSION, is_valid_peer_url};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// How often the open 虚拟组网 tab asks `easytier-cli` for the live topology.
pub const POLL_EVERY: Duration = Duration::from_secs(3);
/// Largest TOML accepted by the import dialog.
pub const MAX_IMPORT_LEN: usize = 16 * 1024;
pub const MAX_PROFILE_NAME_LEN: usize = 64;
pub const MAX_PEERS_TEXT_LEN: usize = 2048;
pub const MAX_UNLOCK_LEN: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DrawerTab {
    Server,
    Mesh,
}

/// The six situations the tab distinguishes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cell {
    NotInstalled,
    InstalledIdle,
    ExternalConfigServer,
    ExternalManual,
    ManagedRunning,
    ManagedStopped,
}

impl Cell {
    /// The cell of a detection result. `bound` is whether `mesh-state.json` says Skylatch
    /// manages the network and `managed_ids` the profiles in the managed directory: a detached
    /// network that keeps running counts as independent, and Skylatch's own stopped profiles
    /// win over an external instance that runs next to them.
    pub fn resolve(
        presence: &EasyTierPresence,
        bound: bool,
        managed_ids: &[String],
        has_core: bool,
    ) -> Cell {
        let idle = if has_core {
            Cell::InstalledIdle
        } else {
            Cell::NotInstalled
        };
        match presence {
            EasyTierPresence::NotInstalled => Cell::NotInstalled,
            EasyTierPresence::InstalledIdle { .. } => Cell::InstalledIdle,
            EasyTierPresence::ManagedRunning { .. } if bound => Cell::ManagedRunning,
            EasyTierPresence::ManagedRunning { .. } => Cell::ExternalManual,
            EasyTierPresence::ManagedStopped { .. } if bound => Cell::ManagedStopped,
            EasyTierPresence::ManagedStopped { .. } => idle,
            EasyTierPresence::ExternalConfigServer { .. }
            | EasyTierPresence::ExternalGuiOrManual { .. }
                if bound && !managed_ids.is_empty() =>
            {
                Cell::ManagedStopped
            }
            EasyTierPresence::ExternalConfigServer { .. } => Cell::ExternalConfigServer,
            EasyTierPresence::ExternalGuiOrManual { .. } => Cell::ExternalManual,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Cell::NotInstalled => "本机未安装 EasyTier",
            Cell::InstalledIdle => "EasyTier 已安装，未运行",
            Cell::ExternalConfigServer => "外部 config-server 托管（只读复用）",
            Cell::ExternalManual => "外部独立运行（未绑定）",
            Cell::ManagedRunning => "Skylatch 托管运行中",
            Cell::ManagedStopped => "Skylatch 托管已停止",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Cell::NotInstalled => "安装官方 EasyTier 2.6.4 后，Skylatch 按云端组网配置托管它",
            Cell::InstalledIdle => "本机有 EasyTier，但没有在运行，也没有 Skylatch 托管的组网",
            Cell::ExternalConfigServer => {
                "配置由远端 config-server 下发，Skylatch 只读取状态，不改写配置"
            }
            Cell::ExternalManual => "这是你自己的 EasyTier，Skylatch 不会动它的配置和进程",
            Cell::ManagedRunning => "按云端组网配置运行，改动保存后自动同步到本机",
            Cell::ManagedStopped => "本机已有 Skylatch 托管的组网配置，当前没有运行",
        }
    }

    pub fn is_external(self) -> bool {
        matches!(self, Cell::ExternalConfigServer | Cell::ExternalManual)
    }

    pub fn is_managed(self) -> bool {
        matches!(self, Cell::ManagedRunning | Cell::ManagedStopped)
    }
}

/// The small dot next to the 虚拟组网 tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabDot {
    Unknown,
    Connected,
    External,
    Missing,
    Warning,
}

/// Conflicts worth a strip in `cell`: an external EasyTier is the state card itself while it is
/// the one shown.
pub fn shown_conflicts(cell: Cell, conflicts: &[MeshConflict]) -> Vec<&MeshConflict> {
    conflicts
        .iter()
        .filter(|c| !(cell.is_external() && c.kind == ConflictKind::ExternalEasyTier))
        .collect()
}

pub fn tab_dot(cell: Option<Cell>, conflicts: &[MeshConflict], failed: bool) -> TabDot {
    let Some(cell) = cell else {
        return TabDot::Unknown;
    };
    if failed
        || cell == Cell::ManagedStopped
        || shown_conflicts(cell, conflicts)
            .iter()
            .any(|c| c.severity == ConflictSeverity::Blocking || cell.is_managed())
    {
        return TabDot::Warning;
    }
    match cell {
        Cell::ManagedRunning => TabDot::Connected,
        Cell::ExternalConfigServer | Cell::ExternalManual => TabDot::External,
        _ => TabDot::Missing,
    }
}

/// The presence summary of `skylatch-mesh` (`easytier-core configured by tcp://cfg:22020
/// (PID 371), hostname h, ...`) as one short line: where the configuration comes from and the
/// process.
pub fn summary_line(summary: &str) -> String {
    let (what, rest) = summary.split_once(" (PID ").unwrap_or((summary, ""));
    let pid = rest.split(')').next().filter(|p| !p.is_empty());
    let source = match what.strip_prefix("easytier-core configured by ") {
        Some(server) => format!("配置来源 {server}"),
        None => what.to_string(),
    };
    match pid {
        Some(pid) => format!("{source}，PID {pid}"),
        None => source,
    }
}

/// One detection round, made off the UI thread.
pub struct Probe {
    pub presence: EasyTierPresence,
    pub cell: Cell,
    /// Live status of each network: one per managed profile, or the external instance.
    pub networks: Vec<NetStatus>,
    pub conflicts: Vec<MeshConflict>,
    pub state: MeshState,
    pub managed_ids: Vec<String>,
    /// The managed profiles run as a user process Skylatch started (no-TUN mode).
    pub user_process: bool,
}

pub struct NetStatus {
    pub profile_id: Option<String>,
    pub status: Result<MeshRuntimeStatus, String>,
}

impl Probe {
    pub fn bound(&self) -> bool {
        matches!(self.state.binding, MeshBindingMode::Managed { .. })
    }

    pub fn no_tun(&self) -> bool {
        matches!(
            self.state.binding,
            MeshBindingMode::Managed { no_tun: true, .. }
        )
    }

    pub fn vault_version(&self) -> Option<u64> {
        match &self.state.binding {
            MeshBindingMode::Managed { vault_version, .. } => *vault_version,
            MeshBindingMode::Unbound => None,
        }
    }
}

/// Whether the polling thread should run a detection round now. A round runs when the drawer
/// opens (for the tab's dot) and every [`POLL_EVERY`] while the 虚拟组网 tab is on screen;
/// never while the drawer is closed.
pub fn poll_due(
    open: bool,
    tab: DrawerTab,
    last: Option<Instant>,
    inflight: bool,
    now: Instant,
) -> bool {
    if !open || inflight {
        return false;
    }
    match last {
        None => true,
        Some(at) => tab == DrawerTab::Mesh && now.saturating_duration_since(at) >= POLL_EVERY,
    }
}

// ───────────────────────────── peer table ─────────────────────────────

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RouteBadge {
    P2p,
    Relay,
    Other(String),
}

impl RouteBadge {
    pub fn label(&self) -> String {
        match self {
            RouteBadge::P2p => "P2P".into(),
            RouteBadge::Relay => "中继".into(),
            RouteBadge::Other(s) if s.is_empty() => "未知".into(),
            RouteBadge::Other(s) => s.clone(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PeerRow {
    pub hostname: String,
    pub ip: String,
    pub route: RouteBadge,
    pub latency: String,
}

pub fn latency_text(ms: Option<f64>) -> String {
    match ms {
        Some(ms) if ms < 10.0 => format!("{ms:.1} ms"),
        Some(ms) => format!("{} ms", ms.round() as u64),
        None => "—".into(),
    }
}

/// The other nodes of a network, in the order `easytier-cli` lists them.
pub fn peer_rows(status: &MeshRuntimeStatus) -> Vec<PeerRow> {
    status
        .peers
        .iter()
        .filter(|p| p.route != MeshRouteKind::Local)
        .map(|p| PeerRow {
            hostname: if p.hostname.is_empty() {
                p.id.as_deref()
                    .map_or("未命名节点".to_string(), |id| format!("节点 {id}"))
            } else {
                p.hostname.clone()
            },
            ip: p.ipv4.clone().unwrap_or_else(|| "—".into()),
            route: match &p.route {
                MeshRouteKind::P2p => RouteBadge::P2p,
                MeshRouteKind::Relay(_) => RouteBadge::Relay,
                MeshRouteKind::Other(s) => RouteBadge::Other(s.clone()),
                MeshRouteKind::Local => RouteBadge::Other(String::new()),
            },
            latency: latency_text(p.latency_ms),
        })
        .collect()
}

/// `(P2P, relayed)` counts of a network.
pub fn route_counts(rows: &[PeerRow]) -> (usize, usize) {
    (
        rows.iter().filter(|r| r.route == RouteBadge::P2p).count(),
        rows.iter().filter(|r| r.route == RouteBadge::Relay).count(),
    )
}

// ───────────────────────────── vault ─────────────────────────────

/// An unlocked vault: the envelope it came from (version `0` before the first upload), the
/// decrypted networks and the data key, which is wiped when the session ends.
#[derive(Clone)]
pub struct VaultSession {
    pub envelope: MeshVaultEnvelope,
    pub vault: MeshVault,
    pub key: VaultKey,
}

impl VaultSession {
    pub fn uploaded(&self) -> bool {
        self.envelope.version > 0
    }
}

pub enum Vault {
    /// Not asked yet (no hub, or the hub just changed).
    Unknown,
    Loading,
    /// The hub has no vault and no password was entered this session.
    Absent,
    /// The hub holds a vault this session cannot read yet.
    Locked(MeshVaultEnvelope),
    Open(VaultSession),
    Failed(String),
}

impl Vault {
    pub fn session(&self) -> Option<&VaultSession> {
        match self {
            Vault::Open(s) => Some(s),
            _ => None,
        }
    }

    pub fn session_mut(&mut self) -> Option<&mut VaultSession> {
        match self {
            Vault::Open(s) => Some(s),
            _ => None,
        }
    }

    /// The cloud already holds an encrypted mesh configuration for the account.
    pub fn cloud_has_config(&self) -> bool {
        match self {
            Vault::Locked(_) => true,
            Vault::Open(s) => s.uploaded(),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnlockMode {
    Password,
    Recovery,
}

/// What a background vault request reports.
pub enum VaultEvent {
    Loaded(Option<MeshVaultEnvelope>),
    Opened(VaultSession),
    /// The hub stored the vault and answered with the new envelope.
    Saved(MeshVaultEnvelope),
    /// `409 Conflict`: the hub's current envelope (`None`: it was deleted).
    Conflict(Option<MeshVaultEnvelope>),
    Failed(String),
}

/// SHA-256 of the stored ciphertext, as four groups of four hex digits: what the hub keeps.
pub fn fingerprint(envelope: &MeshVaultEnvelope) -> String {
    let d = digest::digest(&digest::SHA256, envelope.payload_ct_b64.as_bytes());
    d.as_ref()[..8]
        .chunks(2)
        .map(|c| format!("{:02X}{:02X}", c[0], c[1]))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` in groups of `n` characters separated by spaces, so long secrets wrap on screen.
pub fn grouped(text: &str, n: usize) -> String {
    text.chars()
        .collect::<Vec<_>>()
        .chunks(n.max(1))
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Bytes of ciphertext the hub stores for the vault.
pub fn ciphertext_len(envelope: &MeshVaultEnvelope) -> usize {
    let b64 = envelope.payload_ct_b64.trim_end_matches('=');
    b64.len() * 3 / 4
}

pub fn recovery_key(session: &VaultSession) -> String {
    mesh_vault::format_recovery_key(&session.key)
}

/// One network as the tab edits it.
#[derive(Clone)]
pub struct ProfileDraft {
    pub profile: MeshProfile,
    /// Custom peers, one per line (commas and spaces also separate).
    pub peers_text: String,
    /// This machine joins the network.
    pub join: bool,
}

/// The editable copy of the vault.
#[derive(Clone)]
pub struct Draft {
    pub items: Vec<ProfileDraft>,
    pub dirty: bool,
    /// The network whose card is expanded (several networks) and whose fields take text.
    pub focus: Option<String>,
}

impl Draft {
    /// `joined` are the profiles this machine runs; with none recorded, a single network is
    /// joined by default.
    pub fn from_vault(vault: &MeshVault, joined: Option<&[String]>) -> Draft {
        let single = vault.profiles.len() == 1;
        Draft {
            items: vault
                .profiles
                .iter()
                .map(|p| ProfileDraft {
                    peers_text: p.custom_peers.join("\n"),
                    join: joined.map_or(single, |ids| ids.contains(&p.id)),
                    profile: p.clone(),
                })
                .collect(),
            dirty: false,
            focus: vault.profiles.first().map(|p| p.id.clone()),
        }
    }

    pub fn item(&self, id: &str) -> Option<&ProfileDraft> {
        self.items.iter().find(|i| i.profile.id == id)
    }

    pub fn item_mut(&mut self, id: &str) -> Option<&mut ProfileDraft> {
        self.items.iter_mut().find(|i| i.profile.id == id)
    }

    fn shell(&self) -> MeshVault {
        MeshVault {
            schema_version: VAULT_SCHEMA_VERSION,
            profiles: self.items.iter().map(|i| i.profile.clone()).collect(),
        }
    }

    /// Adds an isolated network on the next free subnet and port; returns its id.
    pub fn add_isolated(&mut self) -> Result<String, String> {
        let profile = self
            .shell()
            .next_isolated_defaults()
            .map_err(|e| describe_mesh_error(&e))?;
        let id = profile.id.clone();
        self.items.push(ProfileDraft {
            profile,
            peers_text: String::new(),
            join: false,
        });
        self.focus = Some(id.clone());
        self.dirty = true;
        Ok(id)
    }

    pub fn add_imported(&mut self, profile: MeshProfile) {
        self.focus = Some(profile.id.clone());
        self.items.push(ProfileDraft {
            peers_text: profile.custom_peers.join("\n"),
            profile,
            join: false,
        });
        self.dirty = true;
    }

    /// Deletes a network; the vault always keeps at least one.
    pub fn remove(&mut self, id: &str) -> Result<(), String> {
        if self.items.len() <= 1 {
            return Err("至少保留一个组网".into());
        }
        let before = self.items.len();
        self.items.retain(|i| i.profile.id != id);
        if self.items.len() == before {
            return Err("没有这个组网".into());
        }
        if self.focus.as_deref() == Some(id) {
            self.focus = self.items.first().map(|i| i.profile.id.clone());
        }
        self.dirty = true;
        Ok(())
    }

    pub fn toggle_join(&mut self, id: &str) {
        if let Some(item) = self.item_mut(id) {
            item.join = !item.join;
            self.dirty = true;
        }
    }

    pub fn toggle_hub_peer(&mut self, id: &str) {
        if let Some(item) = self.item_mut(id) {
            item.profile.use_hub_peer = !item.profile.use_hub_peer;
            self.dirty = true;
        }
    }

    pub fn joined(&self) -> Vec<String> {
        self.items
            .iter()
            .filter(|i| i.join)
            .map(|i| i.profile.id.clone())
            .collect()
    }

    /// The vault to seal: peers parsed, everything validated.
    pub fn to_vault(&self) -> Result<MeshVault, String> {
        let mut vault = self.shell();
        for (profile, item) in vault.profiles.iter_mut().zip(&self.items) {
            profile.name = profile.name.trim().to_string();
            profile.custom_peers = parse_peers(&item.peers_text)
                .map_err(|bad| format!("「{}」的节点地址无效：{bad}", item.profile.name))?;
        }
        vault.validate().map_err(|e| describe_mesh_error(&e))?;
        Ok(vault)
    }
}

/// Peer URLs separated by lines, commas or spaces; the first invalid one is the error.
pub fn parse_peers(text: &str) -> Result<Vec<String>, String> {
    let mut peers: Vec<String> = Vec::new();
    for peer in text
        .split(|c: char| c.is_whitespace() || c == ',' || c == '，')
        .filter(|p| !p.is_empty())
    {
        if !is_valid_peer_url(peer) {
            return Err(peer.to_string());
        }
        if !peers.iter().any(|p| p == peer) {
            peers.push(peer.to_string());
        }
    }
    Ok(peers)
}

pub fn describe_mesh_error(e: &MeshError) -> String {
    match e {
        MeshError::Empty => "至少保留一个组网".into(),
        MeshError::TooManyProfiles => "组网数量已达上限".into(),
        MeshError::InvalidProfile { reason, .. } if reason.starts_with("name") => {
            "组网名称须为 1 到 64 个字符".into()
        }
        MeshError::InvalidProfile { reason, .. } if reason.starts_with("custom peers") => {
            "节点地址须为 scheme://host 形式，最多 32 个".into()
        }
        MeshError::InvalidProfile { reason, .. } => format!("组网配置无效：{reason}"),
        MeshError::DuplicateId(_) => "组网标识重复".into(),
        MeshError::SubnetOverlap(..) => "两个组网使用了相同的网段".into(),
        MeshError::PortConflict(..) => "两个组网使用了相同的监听端口".into(),
        MeshError::Exhausted => "没有可用的网段或端口了".into(),
        MeshError::UnsupportedSchema(v) => format!("不支持的组网配置版本 {v}"),
        MeshError::Random => "系统随机数源不可用".into(),
    }
}

// ───────────────────────────── pairing invites ─────────────────────────────

/// A fresh invite secret and `profile` sealed with it, for a pairing code.
pub fn seal_invite(profile: &MeshProfile) -> Result<(InviteSecret, MeshInviteCiphertext), String> {
    let secret = InviteSecret::generate().map_err(|e| e.to_string())?;
    let invite = mesh_vault::seal_mesh_invite(profile, &secret).map_err(|e| e.to_string())?;
    Ok((secret, invite))
}

// ───────────────────────────── dialogs ─────────────────────────────

/// Signing out or switching accounts while Skylatch manages this machine's network needs a
/// decision about that network first.
pub fn needs_guard(binding: &MeshBindingMode) -> bool {
    matches!(binding, MeshBindingMode::Managed { .. })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GuardChoice {
    /// Only sign out of Skylatch; EasyTier keeps running on its own.
    KeepIndependent,
    /// Also stop and remove this machine's managed EasyTier.
    StopAndClear,
}

/// What waits behind the guard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountAction {
    SignOut,
    /// The server tab's submit (another hub or account).
    Submit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ImportIntro {
    Plain,
    /// 接入 Skylatch 协作管理 of an independent EasyTier.
    Adopt,
}

pub struct ImportDialog {
    pub text: String,
    pub intro: ImportIntro,
    pub preview: Option<Result<MeshProfile, String>>,
}

impl ImportDialog {
    pub fn new(intro: ImportIntro) -> ImportDialog {
        ImportDialog {
            text: String::new(),
            intro,
            preview: None,
        }
    }

    /// Re-parses the text against the networks already in `existing`.
    pub fn refresh(&mut self, existing: Option<&MeshVault>) {
        self.preview = (!self.text.trim().is_empty()).then(|| {
            skylatch_mesh::import_easytier_toml(&self.text, existing).map_err(|e| match e {
                skylatch_mesh::MeshError::MissingField(f) => format!("缺少 {f}"),
                skylatch_mesh::MeshError::Toml(m) => format!("不是有效的 TOML：{m}"),
                skylatch_mesh::MeshError::Profile(e) => describe_mesh_error(&e),
                other => other.to_string(),
            })
        });
    }

    pub fn ready(&self) -> Option<&MeshProfile> {
        self.preview.as_ref()?.as_ref().ok()
    }
}

/// `网络名 x，2 个节点，10.126.2.0/24，端口 11011`.
pub fn import_summary(p: &MeshProfile) -> String {
    format!(
        "网络名 {}，{} 个节点，{}，端口 {}",
        p.network_name,
        p.custom_peers.len(),
        p.subnet,
        p.listen_port
    )
}

pub enum SyncConflict {
    /// `409 Conflict` while saving: someone else changed the cloud copy first.
    Remote { current: Option<MeshVaultEnvelope> },
    /// This machine runs profiles written from an older cloud version.
    LocalDiffers { local: u64, cloud: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyncChoice {
    UseCloud,
    UseLocal,
    KeepIndependent,
}

/// The managed network was written from a cloud version other than `cloud`.
pub fn local_differs(binding: &MeshBindingMode, cloud: u64) -> Option<u64> {
    match binding {
        MeshBindingMode::Managed {
            vault_version: Some(local),
            ..
        } if cloud > 0 && *local != cloud => Some(*local),
        _ => None,
    }
}

pub enum Modal {
    Guard(AccountAction),
    Import(ImportDialog),
    Conflict(SyncConflict),
}

/// A text box of the tab or its dialogs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MeshField {
    Unlock,
    Name,
    Peers,
    Import,
}

/// Everything the 虚拟组网 tab shows and edits.
pub struct MeshUi {
    pub tab: DrawerTab,
    pub field: Option<MeshField>,
    pub probe: Option<Probe>,
    pub probe_at: Option<Instant>,
    pub probe_rx: Option<Receiver<Probe>>,
    /// The network whose peers the table shows.
    pub peer_net: usize,
    pub vault: Vault,
    pub vault_rx: Option<Receiver<VaultEvent>>,
    pub draft: Option<Draft>,
    pub unlock_mode: UnlockMode,
    pub unlock_text: String,
    /// The no-TUN choice of the install card.
    pub no_tun: bool,
    pub audit_open: bool,
    pub reveal: bool,
    pub modal: Option<Modal>,
    /// A local EasyTier operation on its way, with what it is doing.
    pub op: Option<(&'static str, Receiver<Result<String, String>>)>,
    /// Status line of the tab: text and whether it is an error.
    pub notice: Option<(String, bool)>,
    /// The cloud version a local-differs conflict was already offered for.
    pub offered_for: Option<u64>,
    /// What a copy button just copied, for its 已复制 flash.
    pub copied: Option<(String, Instant)>,
    /// A sign-out or switch waiting for its EasyTier decision to finish.
    pub after_op: Option<AccountAction>,
    /// The vault on its way to the hub, applied to this machine once it is stored.
    pub pending_save: Option<MeshVault>,
}

impl MeshUi {
    pub fn new() -> MeshUi {
        MeshUi {
            tab: DrawerTab::Server,
            field: None,
            probe: None,
            probe_at: None,
            probe_rx: None,
            peer_net: 0,
            vault: Vault::Unknown,
            vault_rx: None,
            draft: None,
            unlock_mode: UnlockMode::Password,
            unlock_text: String::new(),
            no_tun: false,
            audit_open: false,
            reveal: false,
            modal: None,
            op: None,
            notice: None,
            offered_for: None,
            copied: None,
            after_op: None,
            pending_save: None,
        }
    }

    pub fn cell(&self) -> Option<Cell> {
        self.probe.as_ref().map(|p| p.cell)
    }

    pub fn dot(&self) -> TabDot {
        let probe = self.probe.as_ref();
        tab_dot(
            self.cell(),
            probe.map(|p| p.conflicts.as_slice()).unwrap_or_default(),
            probe.is_some_and(|p| p.networks.iter().any(|n| n.status.is_err())),
        )
    }

    pub fn busy(&self) -> bool {
        self.op.is_some() || self.vault_rx.is_some()
    }

    /// Forgets the account's vault (sign-out, another hub): the key is wiped on drop.
    pub fn forget_vault(&mut self) {
        self.vault = Vault::Unknown;
        self.vault_rx = None;
        self.draft = None;
        self.wipe_unlock();
        self.reveal = false;
        self.audit_open = false;
        self.offered_for = None;
        self.pending_save = None;
        if matches!(self.modal, Some(Modal::Conflict(_) | Modal::Import(_))) {
            self.modal = None;
        }
    }

    /// Takes a newly opened vault and rebuilds the draft from it.
    pub fn open(&mut self, session: VaultSession) {
        let joined =
            self.probe
                .as_ref()
                .filter(|p| p.bound())
                .and_then(|p| match &p.state.binding {
                    MeshBindingMode::Managed {
                        active_profile_ids, ..
                    } => Some(active_profile_ids.clone()),
                    MeshBindingMode::Unbound => None,
                });
        self.draft = Some(Draft::from_vault(&session.vault, joined.as_deref()));
        self.vault = Vault::Open(session);
    }

    /// Follows the networks this machine runs while the draft has no edits of its own.
    pub fn sync_joins(&mut self) {
        let Some(probe) = self.probe.as_ref() else {
            return;
        };
        let MeshBindingMode::Managed {
            active_profile_ids, ..
        } = &probe.state.binding
        else {
            return;
        };
        if let Some(d) = self.draft.as_mut().filter(|d| !d.dirty) {
            for item in &mut d.items {
                item.join = active_profile_ids.contains(&item.profile.id);
            }
        }
    }

    /// Overwrites the unlock text before it is dropped.
    pub fn wipe_unlock(&mut self) {
        let len = self.unlock_text.len();
        self.unlock_text.clear();
        self.unlock_text.extend(std::iter::repeat_n('\0', len));
        self.unlock_text.clear();
    }

    /// The text of a box with its length limit.
    pub fn buf(&mut self, field: MeshField) -> Option<(&mut String, usize)> {
        match field {
            MeshField::Unlock => Some((&mut self.unlock_text, MAX_UNLOCK_LEN)),
            MeshField::Import => match &mut self.modal {
                Some(Modal::Import(d)) => Some((&mut d.text, MAX_IMPORT_LEN)),
                _ => None,
            },
            MeshField::Name | MeshField::Peers => {
                let draft = self.draft.as_mut()?;
                let id = draft.focus.clone()?;
                let item = draft.items.iter_mut().find(|i| i.profile.id == id)?;
                Some(if field == MeshField::Name {
                    (&mut item.profile.name, MAX_PROFILE_NAME_LEN)
                } else {
                    (&mut item.peers_text, MAX_PEERS_TEXT_LEN)
                })
            }
        }
    }

    pub fn text_of(&self, field: MeshField) -> Option<&str> {
        match field {
            MeshField::Unlock => Some(&self.unlock_text),
            MeshField::Import => match &self.modal {
                Some(Modal::Import(d)) => Some(&d.text),
                _ => None,
            },
            MeshField::Name | MeshField::Peers => {
                let draft = self.draft.as_ref()?;
                let item = draft.item(draft.focus.as_deref()?)?;
                Some(if field == MeshField::Name {
                    &item.profile.name
                } else {
                    &item.peers_text
                })
            }
        }
    }

    /// A box changed: the draft is dirty, the import preview follows the text.
    pub fn edited(&mut self, field: MeshField) {
        match field {
            MeshField::Name | MeshField::Peers => {
                if let Some(d) = self.draft.as_mut() {
                    d.dirty = true;
                }
            }
            MeshField::Import => {
                let vault = self.draft.as_ref().and_then(|d| d.to_vault().ok());
                if let Some(Modal::Import(d)) = &mut self.modal {
                    d.refresh(vault.as_ref());
                }
            }
            MeshField::Unlock => {}
        }
        self.notice = None;
    }

    pub fn say(&mut self, text: impl Into<String>, error: bool) {
        self.notice = Some((text.into(), error));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_mesh::{MeshPeerStatus, MeshRuntimeStatus};
    use std::path::PathBuf;

    fn presences() -> Vec<EasyTierPresence> {
        vec![
            EasyTierPresence::NotInstalled,
            EasyTierPresence::InstalledIdle {
                easytier_core: PathBuf::from("/usr/local/bin/easytier-core"),
            },
            EasyTierPresence::ExternalConfigServer {
                rpc_portal: "127.0.0.1:15888".into(),
                summary: "easytier-core configured by tcp://cfg:22020 (PID 371)".into(),
            },
            EasyTierPresence::ExternalGuiOrManual {
                rpc_portal: None,
                overlay_ips: vec![],
                summary: "easytier-gui (PID 9)".into(),
            },
            EasyTierPresence::ManagedRunning {
                rpc_portal: "127.0.0.1:15889".into(),
                profiles_dir: PathBuf::from("/x/easytier"),
                active_profile_ids: vec!["default".into()],
            },
            EasyTierPresence::ManagedStopped {
                profiles_dir: PathBuf::from("/x/easytier"),
                configured_profile_ids: vec!["default".into()],
            },
        ]
    }

    #[test]
    fn the_six_cells_follow_detection_and_binding() {
        let ids = vec!["default".to_string()];
        let bound: Vec<Cell> = presences()
            .iter()
            .map(|p| Cell::resolve(p, true, &[], true))
            .collect();
        assert_eq!(
            bound,
            [
                Cell::NotInstalled,
                Cell::InstalledIdle,
                Cell::ExternalConfigServer,
                Cell::ExternalManual,
                Cell::ManagedRunning,
                Cell::ManagedStopped,
            ]
        );
        let p = presences();
        // detached: the network keeps running on its own, stopped files are just files
        assert_eq!(
            Cell::resolve(&p[4], false, &ids, true),
            Cell::ExternalManual
        );
        assert_eq!(Cell::resolve(&p[5], false, &ids, true), Cell::InstalledIdle);
        assert_eq!(Cell::resolve(&p[5], false, &ids, false), Cell::NotInstalled);
        // Skylatch's own stopped profiles win over an external instance next to them
        assert_eq!(Cell::resolve(&p[2], true, &ids, true), Cell::ManagedStopped);
        assert_eq!(Cell::resolve(&p[3], true, &ids, true), Cell::ManagedStopped);
        assert_eq!(
            Cell::resolve(&p[2], false, &ids, true),
            Cell::ExternalConfigServer
        );
        for cell in bound {
            assert!(!cell.title().is_empty() && !cell.detail().is_empty());
            assert_eq!(
                [cell.is_external(), cell.is_managed()]
                    .iter()
                    .filter(|b| **b)
                    .count(),
                usize::from(!matches!(cell, Cell::NotInstalled | Cell::InstalledIdle))
            );
        }
        assert_eq!(
            Cell::ExternalConfigServer.title(),
            "外部 config-server 托管（只读复用）"
        );
        assert_eq!(Cell::ExternalManual.title(), "外部独立运行（未绑定）");
    }

    fn conflict(kind: ConflictKind, severity: ConflictSeverity) -> MeshConflict {
        MeshConflict {
            kind,
            title: "t".into(),
            detail: "d".into(),
            severity,
        }
    }

    #[test]
    fn the_tab_dot_tells_connected_external_missing_and_warnings_apart() {
        let external = conflict(ConflictKind::ExternalEasyTier, ConflictSeverity::Warning);
        let clash = conflict(ConflictKind::ClashTun, ConflictSeverity::Warning);
        assert_eq!(tab_dot(None, &[], false), TabDot::Unknown);
        assert_eq!(
            tab_dot(Some(Cell::ManagedRunning), &[], false),
            TabDot::Connected
        );
        assert_eq!(
            tab_dot(
                Some(Cell::ExternalConfigServer),
                std::slice::from_ref(&external),
                false
            ),
            TabDot::External
        );
        assert_eq!(
            tab_dot(Some(Cell::NotInstalled), &[], false),
            TabDot::Missing
        );
        assert_eq!(
            tab_dot(Some(Cell::InstalledIdle), &[], false),
            TabDot::Missing
        );
        assert_eq!(
            tab_dot(Some(Cell::ManagedStopped), &[], false),
            TabDot::Warning
        );
        assert_eq!(
            tab_dot(
                Some(Cell::ManagedRunning),
                std::slice::from_ref(&clash),
                false
            ),
            TabDot::Warning
        );
        assert_eq!(
            tab_dot(Some(Cell::ManagedRunning), &[], true),
            TabDot::Warning
        );
        assert_eq!(
            tab_dot(Some(Cell::NotInstalled), &[clash], false),
            TabDot::Missing
        );
        let blocking = conflict(ConflictKind::ExternalEasyTier, ConflictSeverity::Blocking);
        assert_eq!(
            tab_dot(Some(Cell::NotInstalled), &[blocking], false),
            TabDot::Warning
        );
        // the external instance is the state card itself
        assert!(shown_conflicts(Cell::ExternalManual, std::slice::from_ref(&external)).is_empty());
        assert_eq!(shown_conflicts(Cell::ManagedRunning, &[external]).len(), 1);
    }

    #[test]
    fn presence_summaries_become_one_short_line() {
        assert_eq!(
            summary_line(
                "easytier-core configured by tcp://47.95.10.205:22020 (PID 371), hostname maxclaw-mac, 10.126.126.3, 13 nodes"
            ),
            "配置来源 tcp://47.95.10.205:22020，PID 371"
        );
        assert_eq!(summary_line("easytier-gui (PID 9)"), "easytier-gui，PID 9");
        assert_eq!(summary_line("x"), "x");
    }

    #[test]
    fn polling_runs_only_while_the_drawer_is_open() {
        let t0 = Instant::now();
        let later = t0 + Duration::from_millis(3100);
        let soon = t0 + Duration::from_millis(1000);
        assert!(!poll_due(false, DrawerTab::Mesh, None, false, t0));
        assert!(!poll_due(false, DrawerTab::Mesh, Some(t0), false, later));
        // one round when the drawer opens, for the dot
        assert!(poll_due(true, DrawerTab::Server, None, false, t0));
        assert!(!poll_due(true, DrawerTab::Server, Some(t0), false, later));
        assert!(poll_due(true, DrawerTab::Mesh, Some(t0), false, later));
        assert!(!poll_due(true, DrawerTab::Mesh, Some(t0), false, soon));
        assert!(!poll_due(true, DrawerTab::Mesh, Some(t0), true, later));
    }

    fn peer(host: &str, ip: Option<&str>, route: MeshRouteKind, ms: Option<f64>) -> MeshPeerStatus {
        MeshPeerStatus {
            id: Some("42".into()),
            hostname: host.into(),
            ipv4: ip.map(String::from),
            route,
            latency_ms: ms,
            loss_rate: None,
            tunnel_proto: None,
            nat_type: None,
        }
    }

    #[test]
    fn the_peer_table_has_badges_latencies_and_no_local_row() {
        let status = MeshRuntimeStatus {
            local_hostname: "maxclaw-mac".into(),
            local_ipv4: Some("10.126.126.3".into()),
            nat_type: Some("PortRestricted".into()),
            version: Some("2.6.4".into()),
            peers: vec![
                peer(
                    "maxclaw-mac",
                    Some("10.126.126.3"),
                    MeshRouteKind::Local,
                    None,
                ),
                peer("ecs2", None, MeshRouteKind::P2p, Some(5.49)),
                peer(
                    "pve",
                    Some("10.126.126.10"),
                    MeshRouteKind::Relay(2),
                    Some(33.2),
                ),
                peer("", None, MeshRouteKind::Other("Tunnel".into()), None),
            ],
        };
        let rows = peer_rows(&status);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].route.label(), "P2P");
        assert_eq!(rows[0].latency, "5.5 ms");
        assert_eq!(rows[0].ip, "—");
        assert_eq!(rows[1].route.label(), "中继");
        assert_eq!(rows[1].latency, "33 ms");
        assert_eq!(rows[2].hostname, "节点 42");
        assert_eq!(rows[2].route.label(), "Tunnel");
        assert_eq!(route_counts(&rows), (1, 1));
        assert_eq!(latency_text(Some(5.4)), "5.4 ms");
        assert_eq!(latency_text(None), "—");
    }

    fn session() -> VaultSession {
        let vault = MeshVault::new_default().unwrap();
        let key = mesh_vault::generate_dek().unwrap();
        let envelope = mesh_vault::seal_vault(&vault, &key, "correct horse", 0, 1).unwrap();
        VaultSession {
            envelope,
            vault,
            key,
        }
    }

    #[test]
    fn several_isolated_networks_are_added_edited_validated_and_removed() {
        let s = session();
        let mut d = Draft::from_vault(&s.vault, None);
        assert_eq!(d.items.len(), 1);
        assert!(d.items[0].join, "a single network is joined by default");
        assert!(d.remove("default").is_err());

        let vm = d.add_isolated().unwrap();
        assert!(d.dirty);
        assert_eq!(d.focus.as_deref(), Some(vm.as_str()));
        let added = &d.item(&vm).unwrap().profile;
        assert_eq!(added.subnet, "10.126.2.0/24");
        assert_eq!(added.listen_port, 11011);
        assert!(!d.item(&vm).unwrap().join);
        d.toggle_join(&vm);
        assert_eq!(d.joined(), vec!["default".to_string(), vm.clone()]);
        d.toggle_hub_peer(&vm);
        assert!(!d.item(&vm).unwrap().profile.use_hub_peer);

        d.item_mut(&vm).unwrap().profile.name = " VM 隔离网 ".into();
        d.item_mut(&vm).unwrap().peers_text =
            "tcp://a.example:11010\n tcp://a.example:11010, udp://b:1".into();
        let vault = d.to_vault().unwrap();
        assert_eq!(vault.profiles[1].name, "VM 隔离网");
        assert_eq!(
            vault.profiles[1].custom_peers,
            vec!["tcp://a.example:11010".to_string(), "udp://b:1".to_string()]
        );

        d.item_mut(&vm).unwrap().peers_text = "not a peer".into();
        assert!(d.to_vault().unwrap_err().contains("not"));
        d.item_mut(&vm).unwrap().peers_text.clear();
        d.item_mut(&vm).unwrap().profile.name = "  ".into();
        assert!(d.to_vault().unwrap_err().contains("名称"));
        d.item_mut(&vm).unwrap().profile.name = "VM".into();
        d.item_mut(&vm).unwrap().profile.subnet = "10.126.1.0/24".into();
        assert!(d.to_vault().unwrap_err().contains("网段"));

        d.remove(&vm).unwrap();
        assert_eq!(d.focus.as_deref(), Some("default"));
        assert_eq!(d.items.len(), 1);

        let joined = vec![vm];
        let d = Draft::from_vault(&vault, Some(&joined));
        assert_eq!(d.joined(), joined);
    }

    #[test]
    fn the_audit_shows_a_ciphertext_fingerprint_and_the_recovery_key() {
        let s = session();
        let fp = fingerprint(&s.envelope);
        assert_eq!(fp.len(), 19);
        assert_eq!(fp.split(' ').count(), 4);
        assert!(ciphertext_len(&s.envelope) > 16);
        assert_eq!(grouped("abcdefghij", 4), "abcd efgh ij");
        let key = recovery_key(&s);
        assert!(key.starts_with("SLK1-"));
        let parsed = mesh_vault::parse_recovery_key(&key).unwrap();
        assert_eq!(
            mesh_vault::open_vault_with_recovery_key(&s.envelope, &parsed).unwrap(),
            s.vault
        );
        assert!(!s.uploaded());
        assert!(Vault::Locked(s.envelope.clone()).cloud_has_config());
        assert!(!Vault::Open(s).cloud_has_config());
    }

    const GUI_EXPORT: &str = r#"
instance_name = "home"
hostname = "nas"
ipv4 = "10.144.144.5/24"
dhcp = false
listeners = ["tcp://0.0.0.0:11010", "udp://0.0.0.0:11010"]

[network_identity]
network_name = "home-net"
network_secret = "0123456789abcdef0123"

[[peer]]
uri = "tcp://public.easytier.top:11010"

[[peer]]
uri = "udp://relay.example.com:11010"
"#;

    #[test]
    fn the_import_dialog_previews_and_avoids_existing_networks() {
        let s = session();
        let mut ui = MeshUi::new();
        ui.open(s);
        ui.modal = Some(Modal::Import(ImportDialog::new(ImportIntro::Plain)));
        ui.field = Some(MeshField::Import);
        let (buf, max) = ui.buf(MeshField::Import).unwrap();
        assert_eq!(max, MAX_IMPORT_LEN);
        buf.push_str("not = [toml");
        ui.edited(MeshField::Import);
        let Some(Modal::Import(d)) = &ui.modal else {
            panic!()
        };
        assert!(d.ready().is_none());
        assert!(
            d.preview
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap_err()
                .contains("TOML")
        );

        let (buf, _) = ui.buf(MeshField::Import).unwrap();
        buf.clear();
        buf.push_str(GUI_EXPORT);
        ui.edited(MeshField::Import);
        let Some(Modal::Import(d)) = &ui.modal else {
            panic!()
        };
        let p = d.ready().unwrap();
        assert_eq!(p.network_name, "home-net");
        assert_eq!(p.custom_peers.len(), 2);
        // 11010 is the default network's port: the import moves to the next free one
        assert_eq!(p.listen_port, 11011);
        assert_eq!(p.subnet, "10.144.144.0/24");
        assert_eq!(
            import_summary(p),
            "网络名 home-net，2 个节点，10.144.144.0/24，端口 11011"
        );

        let p = p.clone();
        ui.draft.as_mut().unwrap().add_imported(p);
        assert_eq!(ui.draft.as_ref().unwrap().items.len(), 2);
        assert!(ui.draft.as_ref().unwrap().to_vault().is_ok());

        let mut missing = ImportDialog::new(ImportIntro::Adopt);
        missing.text = "[network_identity]\nnetwork_name = \"x\"\n".into();
        missing.refresh(None);
        assert!(
            missing
                .preview
                .unwrap()
                .unwrap_err()
                .contains("network_secret")
        );
    }

    #[test]
    fn signing_out_is_guarded_only_while_skylatch_manages_the_network() {
        assert!(!needs_guard(&MeshBindingMode::Unbound));
        let managed = MeshBindingMode::Managed {
            active_profile_ids: vec!["default".into()],
            vault_version: Some(3),
            no_tun: false,
            rpc_portal: skylatch_mesh::MANAGED_RPC_PORTAL.into(),
        };
        assert!(needs_guard(&managed));
        assert_eq!(local_differs(&managed, 3), None);
        assert_eq!(local_differs(&managed, 4), Some(3));
        assert_eq!(local_differs(&managed, 0), None);
        assert_eq!(local_differs(&MeshBindingMode::Unbound, 4), None);
        let invite = MeshBindingMode::Managed {
            active_profile_ids: vec!["vm".into()],
            vault_version: None,
            no_tun: true,
            rpc_portal: skylatch_mesh::MANAGED_RPC_PORTAL.into(),
        };
        assert!(needs_guard(&invite));
        assert_eq!(local_differs(&invite, 4), None);
    }

    #[test]
    fn invites_open_only_with_their_secret() {
        let s = session();
        let (secret, invite) = seal_invite(&s.vault.profiles[0]).unwrap();
        let join = mesh_vault::format_join_code("482-917", &secret);
        let (code, parsed) = mesh_vault::split_join_code(&join).unwrap();
        assert_eq!(code, "482-917");
        let opened = mesh_vault::open_mesh_invite(&invite, &parsed.unwrap()).unwrap();
        assert_eq!(opened, s.vault.profiles[0]);
        let other = InviteSecret::generate().unwrap();
        assert!(mesh_vault::open_mesh_invite(&invite, &other).is_err());
    }

    #[test]
    fn text_boxes_edit_the_focused_network_and_the_unlock_text_is_wiped() {
        let mut ui = MeshUi::new();
        assert!(ui.buf(MeshField::Name).is_none());
        ui.open(session());
        let vm = ui.draft.as_mut().unwrap().add_isolated().unwrap();
        ui.draft.as_mut().unwrap().dirty = false;
        ui.buf(MeshField::Name).unwrap().0.push('机');
        ui.edited(MeshField::Name);
        assert!(ui.draft.as_ref().unwrap().dirty);
        assert!(ui.text_of(MeshField::Name).unwrap().ends_with("机"));
        assert!(
            ui.draft
                .as_ref()
                .unwrap()
                .item(&vm)
                .unwrap()
                .profile
                .name
                .ends_with("机")
        );
        ui.buf(MeshField::Peers).unwrap().0.push_str("tcp://x:1");
        assert_eq!(ui.text_of(MeshField::Peers), Some("tcp://x:1"));

        ui.unlock_text = "secret password".into();
        ui.wipe_unlock();
        assert!(ui.unlock_text.is_empty());
        ui.forget_vault();
        assert!(ui.draft.is_none() && matches!(ui.vault, Vault::Unknown));
    }
}

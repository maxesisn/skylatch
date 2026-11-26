// SPDX-License-Identifier: Apache-2.0

//! What the 连接与组网 drawer and its dialogs do: switching tabs, the background rounds of
//! EasyTier detection (only while the drawer is open), the vault's unlock and sync, local
//! EasyTier operations, and the guard in front of signing out. Views are in `hub_view.rs`,
//! `mesh_view.rs` and `mesh_modal.rs`; the rules in [`crate::mesh_ui`].

use crate::hub_ui::AccountMode;
use crate::mesh_ops::{self, ApplyRequest};
use crate::mesh_ui::{
    self, AccountAction, Cell, DrawerTab, GuardChoice, ImportDialog, ImportIntro, MeshField, Modal,
    SyncChoice, SyncConflict, Vault, VaultEvent, VaultSession,
};
use crate::{Field, Proto, config, live};
use gpui::{ClipboardItem, Context, Keystroke};
use skylatch_client_core::MeshVault;
use skylatch_mesh::MeshPaths;
use std::sync::mpsc::TryRecvError;
use std::time::Instant;
use zeroize::Zeroizing;

impl Proto {
    pub(crate) fn set_drawer_tab(&mut self, tab: DrawerTab) {
        if self.mesh.tab == tab {
            return;
        }
        self.marked = None;
        self.mesh.tab = tab;
        self.mesh.field = None;
        if tab == DrawerTab::Mesh
            && self
                .mesh
                .probe_at
                .is_some_and(|t| t.elapsed() >= std::time::Duration::from_secs(1))
        {
            self.mesh.probe_at = None;
        }
        self.kick();
    }

    pub(crate) fn close_drawer(&mut self) {
        self.marked = None;
        self.drawer.open = false;
        self.mesh.field = None;
        self.kick();
    }

    pub(crate) fn set_account_mode(&mut self, mode: AccountMode) {
        self.marked = None;
        self.drawer.account = mode;
        self.drawer.field = if mode == AccountMode::Token {
            Field::Token
        } else {
            Field::User
        };
        self.drawer.error = None;
    }

    pub(crate) fn copy_text(&mut self, what: &str, text: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        self.mesh.copied = Some((what.to_string(), Instant::now()));
        self.kick();
    }

    // ───────────── background work ─────────────

    pub(crate) fn fetch_hub_info(&mut self, url: &str) {
        let url = config::normalize_hub_url(url);
        if !self.hubinfo.wants(&url) {
            return;
        }
        let hub = url.clone();
        let rx = live::run_async(async move {
            skylatch_client_core::hub_info(&hub)
                .await
                .map_err(|e| live::describe_error(&e))
        });
        self.hubinfo.start(url, rx);
    }

    pub(crate) fn fetch_vault(&mut self) {
        if !self.eff.live() || self.eff.token.is_empty() {
            self.mesh.vault = Vault::Unknown;
            return;
        }
        let t = self.live_target();
        self.mesh.vault = Vault::Loading;
        self.mesh.vault_rx = Some(live::run_async(mesh_ops::fetch_vault(t.url, t.token)));
    }

    fn start_probe(&mut self) {
        let paths = self.mesh_paths.clone();
        self.mesh.probe_at = Some(Instant::now());
        self.mesh.probe_rx = Some(mesh_ops::run_blocking(move || {
            mesh_ops::probe(paths.as_ref())
        }));
    }

    fn run_op(
        &mut self,
        what: &'static str,
        f: impl FnOnce(MeshPaths) -> Result<String, String> + Send + 'static,
    ) {
        let Some(paths) = self.mesh_paths.clone() else {
            self.mesh.say("找不到本机的 Skylatch 配置目录", true);
            return;
        };
        self.mesh.notice = None;
        self.mesh.op = Some((what, mesh_ops::run_blocking(move || f(paths))));
        self.kick();
    }

    /// Applies finished background work of the drawer; returns whether anything changed.
    pub(crate) fn pump_mesh(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = self.hubinfo.poll();

        if mesh_ui::poll_due(
            self.drawer.open,
            self.mesh.tab,
            self.mesh.probe_at,
            self.mesh.probe_rx.is_some() || self.mesh.op.is_some(),
            Instant::now(),
        ) {
            self.start_probe();
        }
        let probed = self.mesh.probe_rx.as_ref().map(|rx| rx.try_recv());
        match probed {
            Some(Ok(probe)) => {
                self.mesh.probe_rx = None;
                if self.mesh.peer_net >= probe.networks.len() {
                    self.mesh.peer_net = 0;
                }
                if probe.cell != Cell::ManagedRunning && probe.cell != Cell::ManagedStopped {
                    self.mesh.no_tun = self.mesh.no_tun || probe.no_tun();
                }
                self.mesh.probe = Some(probe);
                self.mesh.sync_joins();
                self.offer_local_conflict();
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.mesh.probe_rx = None;
                changed = true;
            }
            _ => {}
        }

        let event = self.mesh.vault_rx.as_ref().map(|rx| rx.try_recv());
        match event {
            Some(Ok(event)) => {
                self.mesh.vault_rx = None;
                self.on_vault_event(event);
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.mesh.vault_rx = None;
                if matches!(self.mesh.vault, Vault::Loading) {
                    self.mesh.vault = Vault::Failed("请求未能完成".into());
                }
                changed = true;
            }
            _ => {}
        }

        let op = self.mesh.op.as_ref().map(|(_, rx)| rx.try_recv());
        match op {
            Some(Ok(result)) => {
                self.mesh.op = None;
                self.on_op_done(result, cx);
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.mesh.op = None;
                self.on_op_done(Err("操作未能完成".into()), cx);
                changed = true;
            }
            _ => {}
        }

        let login = self.drawer.login.as_ref().map(|(_, rx)| rx.try_recv());
        match login {
            Some(Ok(outcome)) => {
                let (url, _) = self.drawer.login.take().expect("sign-in in flight");
                self.drawer.busy = false;
                match outcome {
                    Ok(signed) => {
                        self.drawer.open = false;
                        self.drawer.error = None;
                        self.mesh_user = Some(signed.username.clone());
                        self.adopt_hub(url, signed.token, Some(signed.vault), cx);
                        self.say("已登录，正在加载设备…", false);
                    }
                    Err(e) => self.drawer.error = Some(e),
                }
                changed = true;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                self.drawer.login = None;
                self.drawer.busy = false;
                self.drawer.error = Some("登录请求未能完成，请重试".into());
                changed = true;
            }
            _ => {}
        }

        if self
            .mesh
            .copied
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() > std::time::Duration::from_secs(2))
        {
            self.mesh.copied = None;
            changed = true;
        }
        changed
    }

    /// Requests whose answers the pump waits for.
    pub(crate) fn mesh_busy(&self) -> bool {
        self.mesh.busy()
            || self.mesh.probe_rx.is_some()
            || self.drawer.login.is_some()
            || self.hubinfo.pending.is_some()
    }

    fn on_vault_event(&mut self, event: VaultEvent) {
        match event {
            VaultEvent::Loaded(Some(envelope)) => self.mesh.vault = Vault::Locked(envelope),
            VaultEvent::Loaded(None) => self.mesh.vault = Vault::Absent,
            VaultEvent::Opened(session) => {
                self.mesh.field = None;
                self.mesh.open(session);
                self.mesh
                    .say("已解锁云端组网配置，密钥只保存在内存中", false);
                self.offer_local_conflict();
            }
            VaultEvent::Saved(envelope) => {
                let saved = self.mesh.pending_save.take();
                let version = envelope.version;
                if let (Some(s), Some(vault)) = (self.mesh.vault.session_mut(), saved) {
                    s.envelope = envelope;
                    s.vault = vault.clone();
                    if let Some(d) = self.mesh.draft.as_mut() {
                        d.dirty = false;
                    }
                    self.mesh.offered_for = Some(version);
                    self.mesh.say(format!("已加密同步到云端 v{version}"), false);
                    if self.mesh.cell().is_some_and(Cell::is_managed) {
                        self.apply_to_machine(vault, Some(version));
                    }
                }
            }
            VaultEvent::Conflict(current) => {
                self.mesh.modal = Some(Modal::Conflict(SyncConflict::Remote { current }));
            }
            VaultEvent::Failed(e) => {
                self.mesh.pending_save = None;
                if matches!(self.mesh.vault, Vault::Loading) {
                    self.mesh.vault = Vault::Failed(e);
                } else {
                    self.mesh.say(e, true);
                }
            }
        }
    }

    fn on_op_done(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        // the binding and the processes just changed: the next round starts from scratch
        self.mesh.probe = None;
        self.mesh.probe_at = None;
        let after = self.mesh.after_op.take();
        match result {
            Ok(msg) => {
                self.mesh.say(msg, false);
                if let Some(action) = after {
                    self.mesh.modal = None;
                    match action {
                        AccountAction::SignOut => self.sign_out_now(),
                        AccountAction::Submit => {
                            self.drawer.guard_passed = true;
                            self.save_and_connect(cx);
                        }
                    }
                }
            }
            Err(e) => {
                if after.is_some() {
                    self.mesh.modal = None;
                    self.drawer.error = Some(e.clone());
                }
                self.mesh.say(e, true);
            }
        }
    }

    /// Offers the three-way choice once per cloud version when this machine runs an older one.
    fn offer_local_conflict(&mut self) {
        let (Some(probe), Some(session)) = (&self.mesh.probe, self.mesh.vault.session()) else {
            return;
        };
        let cloud = session.envelope.version;
        if self.mesh.modal.is_some() || self.mesh.offered_for == Some(cloud) || !probe.bound() {
            return;
        }
        if let Some(local) = mesh_ui::local_differs(&probe.state.binding, cloud) {
            self.mesh.offered_for = Some(cloud);
            self.mesh.modal = Some(Modal::Conflict(SyncConflict::LocalDiffers { local, cloud }));
        }
    }

    // ───────────── vault ─────────────

    pub(crate) fn mesh_unlock(&mut self) {
        if self.mesh.vault_rx.is_some() {
            return;
        }
        if self.mesh.unlock_text.is_empty() {
            self.mesh.say("请输入账号密码或恢复码", true);
            return;
        }
        let secret = Zeroizing::new(self.mesh.unlock_text.clone());
        self.mesh.wipe_unlock();
        self.marked = None;
        let rx = match &self.mesh.vault {
            Vault::Locked(envelope) => {
                let envelope = envelope.clone();
                let mode = self.mesh.unlock_mode;
                mesh_ops::run_blocking(move || match mesh_ops::unlock(&envelope, &secret, mode) {
                    Ok(s) => VaultEvent::Opened(s),
                    Err(e) => VaultEvent::Failed(e),
                })
            }
            Vault::Absent => mesh_ops::run_blocking(move || match mesh_ops::new_vault(&secret) {
                Ok(s) => VaultEvent::Opened(s),
                Err(e) => VaultEvent::Failed(e),
            }),
            _ => return,
        };
        self.mesh.notice = None;
        self.mesh.vault_rx = Some(rx);
    }

    pub(crate) fn save_mesh(&mut self) {
        if self.mesh.vault_rx.is_some() {
            return;
        }
        let (Some(session), Some(draft)) = (self.mesh.vault.session(), self.mesh.draft.as_ref())
        else {
            return;
        };
        let vault = match draft.to_vault() {
            Ok(v) => v,
            Err(e) => {
                self.mesh.say(e, true);
                return;
            }
        };
        if !self.eff.live() {
            self.mesh.say("连接 Hub 后才能同步到云端", true);
            return;
        }
        let t = self.live_target();
        let (base, key) = (session.envelope.clone(), session.key.clone());
        self.mesh.pending_save = Some(vault.clone());
        self.mesh.notice = None;
        self.mesh.vault_rx = Some(live::run_async(mesh_ops::push_vault(
            t.url, t.token, base, key, vault,
        )));
    }

    /// Writes `vault`'s networks this machine joins into the managed directory and restarts them.
    fn apply_to_machine(&mut self, vault: MeshVault, version: Option<u64>) {
        let Some(probe) = &self.mesh.probe else {
            return;
        };
        let selected = self
            .mesh
            .draft
            .as_ref()
            .map(|d| d.joined())
            .unwrap_or_default();
        let req_no_tun = probe.no_tun();
        let start = probe.cell == Cell::ManagedRunning;
        let hub_peers = self.hubinfo.peers();
        let hub_url = config::normalize_hub_url(&self.eff.hub_url);
        self.run_op("正在把组网写入本机", move |paths| {
            mesh_ops::apply_local(ApplyRequest {
                paths,
                vault,
                selected,
                hub_peers,
                no_tun: req_no_tun,
                vault_version: version,
                hub_url,
                start,
            })
        });
    }

    pub(crate) fn add_profile(&mut self) {
        let Some(draft) = self.mesh.draft.as_mut() else {
            return;
        };
        match draft.add_isolated() {
            Ok(_) => {
                self.mesh.field = None;
                self.mesh
                    .say("已新增隔离网络，网段与端口自动避开已有组网", false);
            }
            Err(e) => self.mesh.say(e, true),
        }
    }

    pub(crate) fn remove_profile(&mut self, id: &str) {
        let Some(draft) = self.mesh.draft.as_mut() else {
            return;
        };
        if let Err(e) = draft.remove(id) {
            self.mesh.say(e, true);
        }
        self.mesh.field = None;
    }

    pub(crate) fn toggle_join(&mut self, id: &str) {
        if let Some(d) = self.mesh.draft.as_mut() {
            d.toggle_join(id);
        }
    }

    pub(crate) fn toggle_hub_peer(&mut self, id: &str) {
        if let Some(d) = self.mesh.draft.as_mut() {
            d.toggle_hub_peer(id);
        }
    }

    pub(crate) fn focus_profile(&mut self, id: &str, field: MeshField) {
        if let Some(d) = self.mesh.draft.as_mut() {
            d.focus = Some(id.to_string());
            self.mesh.field = Some(field);
        }
    }

    pub(crate) fn expand_profile(&mut self, id: &str) {
        self.marked = None;
        self.mesh.field = None;
        if let Some(d) = self.mesh.draft.as_mut() {
            d.focus = if d.focus.as_deref() == Some(id) {
                None
            } else {
                Some(id.to_string())
            };
        }
    }

    // ───────────── this machine's EasyTier ─────────────

    pub(crate) fn mesh_install(&mut self) {
        let Some(draft) = self.mesh.draft.as_mut() else {
            self.mesh.say("先解锁云端组网配置", true);
            return;
        };
        if draft.joined().is_empty() && draft.items.len() == 1 {
            draft.items[0].join = true;
        }
        let selected = draft.joined();
        if selected.is_empty() {
            self.mesh.say("请先在下方勾选本机要加入的组网", true);
            return;
        }
        let vault = match draft.to_vault() {
            Ok(v) => v,
            Err(e) => {
                self.mesh.say(e, true);
                return;
            }
        };
        let uploaded = self
            .mesh
            .vault
            .session()
            .is_some_and(VaultSession::uploaded);
        let dirty = self.mesh.draft.as_ref().is_some_and(|d| d.dirty);
        let version = self
            .mesh
            .vault
            .session()
            .filter(|s| s.uploaded())
            .map(|s| s.envelope.version);
        let no_tun = self.mesh.no_tun;
        let hub_peers = self.hubinfo.peers();
        let hub_url = config::normalize_hub_url(&self.eff.hub_url);
        self.run_op("正在安装并启动托管组网", move |paths| {
            mesh_ops::install(&paths, no_tun)?;
            mesh_ops::apply_local(ApplyRequest {
                paths,
                vault,
                selected,
                hub_peers,
                no_tun,
                vault_version: version,
                hub_url,
                start: true,
            })
        });
        if (!uploaded || dirty) && self.eff.live() {
            self.save_mesh();
        }
    }

    pub(crate) fn mesh_run(&mut self, on: bool) {
        if on {
            self.run_op("正在启动托管组网", |p| mesh_ops::start(&p));
        } else {
            self.run_op("正在停止托管组网", |p| mesh_ops::stop(&p));
        }
    }

    pub(crate) fn mesh_toggle_no_tun(&mut self) {
        let (Some(probe), Some(session)) = (&self.mesh.probe, self.mesh.vault.session()) else {
            self.mesh.say("先解锁云端组网配置", true);
            return;
        };
        let skylatch_mesh::MeshBindingMode::Managed {
            active_profile_ids, ..
        } = &probe.state.binding
        else {
            return;
        };
        if let Some(missing) = active_profile_ids
            .iter()
            .find(|id| session.vault.profile(id).is_none())
        {
            self.mesh.say(
                format!("云端配置里没有本机的组网 {missing}，请先同步"),
                true,
            );
            return;
        }
        let req = (
            session.vault.clone(),
            active_profile_ids.clone(),
            !probe.no_tun(),
            probe.vault_version(),
            probe.cell == Cell::ManagedRunning,
        );
        let hub_peers = self.hubinfo.peers();
        let hub_url = config::normalize_hub_url(&self.eff.hub_url);
        self.run_op("正在切换 TUN 模式", move |paths| {
            mesh_ops::apply_local(ApplyRequest {
                paths,
                vault: req.0,
                selected: req.1,
                hub_peers,
                no_tun: req.2,
                vault_version: req.3,
                hub_url,
                start: req.4,
            })
        });
    }

    pub(crate) fn mesh_detach(&mut self) {
        self.run_op("正在解除绑定", |p| mesh_ops::detach(&p));
    }

    pub(crate) fn mesh_adopt(&mut self) {
        let own = self
            .mesh
            .probe
            .as_ref()
            .is_some_and(|p| !p.managed_ids.is_empty());
        if own {
            let hub = config::normalize_hub_url(&self.eff.hub_url);
            self.run_op("正在接入 Skylatch 协作管理", move |p| {
                mesh_ops::rebind(&p, &hub)
            });
        } else {
            self.open_import(ImportIntro::Adopt);
        }
    }

    // ───────────── dialogs ─────────────

    pub(crate) fn open_import(&mut self, intro: ImportIntro) {
        if self.mesh.draft.is_none() {
            self.mesh.say("先解锁云端组网配置，再导入", true);
            return;
        }
        self.marked = None;
        self.mesh.modal = Some(Modal::Import(ImportDialog::new(intro)));
        self.mesh.field = Some(MeshField::Import);
        self.kick();
    }

    pub(crate) fn confirm_import(&mut self) {
        let Some(Modal::Import(d)) = &self.mesh.modal else {
            return;
        };
        let Some(profile) = d.ready().cloned() else {
            return;
        };
        let name = profile.name.clone();
        if let Some(draft) = self.mesh.draft.as_mut() {
            draft.add_imported(profile);
        }
        self.close_modal();
        self.mesh
            .say(format!("已导入「{name}」，保存后加密同步到云端"), false);
    }

    pub(crate) fn close_modal(&mut self) {
        if self.mesh.op.is_some() && matches!(self.mesh.modal, Some(Modal::Guard(_))) {
            return;
        }
        self.marked = None;
        if matches!(self.mesh.modal, Some(Modal::Import(_))) {
            self.mesh.field = None;
        }
        self.mesh.modal = None;
        self.kick();
    }

    /// Whether Skylatch manages this machine's EasyTier right now.
    pub(crate) fn guard_needed(&self) -> bool {
        match (&self.mesh.probe, &self.mesh_paths) {
            (Some(p), _) => mesh_ui::needs_guard(&p.state.binding),
            (None, Some(paths)) => mesh_ui::needs_guard(&mesh_ops::load_state(paths).binding),
            (None, None) => false,
        }
    }

    /// 退出账号: guarded while Skylatch manages this machine's EasyTier.
    pub(crate) fn sign_out(&mut self) {
        if self.guard_needed() {
            self.marked = None;
            self.mesh.modal = Some(Modal::Guard(AccountAction::SignOut));
            self.kick();
            return;
        }
        self.sign_out_now();
    }

    pub(crate) fn resolve_guard(&mut self, choice: GuardChoice) {
        let Some(Modal::Guard(action)) = self.mesh.modal else {
            return;
        };
        if self.mesh.op.is_some() {
            return;
        }
        self.mesh.after_op = Some(action);
        match choice {
            GuardChoice::KeepIndependent => self
                .run_op("正在把 EasyTier 转为独立运行", |p| {
                    mesh_ops::detach(&p)
                }),
            GuardChoice::StopAndClear => self.run_op("正在停用并清除本机 EasyTier", |p| {
                mesh_ops::stop_and_clear(&p)
            }),
        }
        if self.mesh.op.is_none() {
            self.mesh.after_op = None;
        }
    }

    pub(crate) fn resolve_conflict(&mut self, choice: SyncChoice) {
        let Some(Modal::Conflict(conflict)) = self.mesh.modal.take() else {
            return;
        };
        self.mesh.pending_save = None;
        match (choice, conflict) {
            (SyncChoice::UseCloud, SyncConflict::Remote { current: Some(env) }) => {
                let Some(session) = self.mesh.vault.session() else {
                    return;
                };
                match skylatch_client_core::mesh_vault::open_vault_with_recovery_key(
                    &env,
                    &session.key,
                ) {
                    Ok(vault) => {
                        let s = VaultSession {
                            envelope: env.clone(),
                            vault: vault.clone(),
                            key: session.key.clone(),
                        };
                        self.mesh.open(s);
                        self.mesh.offered_for = Some(env.version);
                        self.mesh.say(format!("已采用云端 v{}", env.version), false);
                        if self.mesh.cell().is_some_and(Cell::is_managed) {
                            self.apply_to_machine(vault, Some(env.version));
                        }
                    }
                    Err(_) => {
                        self.mesh.vault = Vault::Locked(env);
                        self.mesh.draft = None;
                        self.mesh.say("云端配置的密钥已更换，请重新解锁", true);
                    }
                }
            }
            (SyncChoice::UseCloud, SyncConflict::Remote { current: None }) => {
                if let Some(s) = self.mesh.vault.session_mut() {
                    s.envelope.version = 0;
                }
                self.mesh.say("云端配置已被删除，保存后会重新创建", true);
            }
            (SyncChoice::UseCloud, SyncConflict::LocalDiffers { cloud, .. }) => {
                if let Some(vault) = self.mesh.vault.session().map(|s| s.vault.clone()) {
                    self.apply_to_machine(vault, Some(cloud));
                }
            }
            (SyncChoice::UseLocal, SyncConflict::Remote { current }) => {
                let version = current.as_ref().map_or(0, |e| e.version);
                if let Some(s) = self.mesh.vault.session_mut() {
                    s.envelope.version = version;
                }
                self.save_mesh();
            }
            (SyncChoice::UseLocal, SyncConflict::LocalDiffers { .. }) => {
                let (Some(paths), Some(session)) =
                    (self.mesh_paths.clone(), self.mesh.vault.session())
                else {
                    return;
                };
                match mesh_ops::local_vault(&paths, &session.vault, &self.hubinfo.peers()) {
                    Ok(vault) => {
                        let joined =
                            self.mesh
                                .probe
                                .as_ref()
                                .and_then(|p| match &p.state.binding {
                                    skylatch_mesh::MeshBindingMode::Managed {
                                        active_profile_ids,
                                        ..
                                    } => Some(active_profile_ids.clone()),
                                    _ => None,
                                });
                        let mut draft = mesh_ui::Draft::from_vault(&vault, joined.as_deref());
                        draft.dirty = true;
                        self.mesh.draft = Some(draft);
                        self.save_mesh();
                    }
                    Err(e) => self.mesh.say(e, true),
                }
            }
            (SyncChoice::KeepIndependent, _) => {
                if self.mesh.cell().is_some_and(Cell::is_managed) {
                    self.run_op("正在转为独立运行", |p| mesh_ops::detach(&p));
                } else {
                    self.mesh.say("本机保持现状，没有同步", false);
                }
            }
        }
        self.kick();
    }

    /// Keys while a dialog is open; it takes them all.
    pub(crate) fn mesh_modal_key(&mut self, ks: &Keystroke) -> bool {
        let Some(modal) = &self.mesh.modal else {
            return false;
        };
        match (modal, ks.key.as_str()) {
            (_, "escape") => self.close_modal(),
            (Modal::Guard(_), "enter") => self.resolve_guard(GuardChoice::KeepIndependent),
            (Modal::Import(_), "enter") => {
                self.marked = None;
                self.put_text(crate::Target::Mesh(MeshField::Import), None, "\n");
            }
            (Modal::Import(_), "backspace") => {
                self.marked = None;
                if let Some((buf, _)) = self.mesh.buf(MeshField::Import) {
                    buf.pop();
                }
                self.mesh.edited(MeshField::Import);
            }
            (Modal::Import(_), "v") if ks.modifiers.secondary() => {}
            (Modal::Import(_), _) => return false,
            _ => {}
        }
        true
    }

    /// Keys of the 虚拟组网 tab.
    pub(crate) fn mesh_tab_key(&mut self, ks: &Keystroke) -> bool {
        let field = self.mesh.field.filter(|f| self.mesh.text_of(*f).is_some());
        match ks.key.as_str() {
            "enter" => match field {
                Some(MeshField::Unlock) => {
                    self.marked = None;
                    self.mesh_unlock();
                }
                Some(MeshField::Peers) => {
                    self.marked = None;
                    self.put_text(crate::Target::Mesh(MeshField::Peers), None, "\n");
                }
                _ => {}
            },
            "tab" => {
                self.marked = None;
                self.mesh.field = match field {
                    Some(MeshField::Name) => Some(MeshField::Peers),
                    Some(MeshField::Peers) => Some(MeshField::Name),
                    other => other,
                };
            }
            "backspace" => {
                self.marked = None;
                if let Some(f) = field
                    && let Some((buf, _)) = self.mesh.buf(f)
                {
                    buf.pop();
                    self.mesh.edited(f);
                }
            }
            "v" if ks.modifiers.secondary() => {}
            _ => return false,
        }
        true
    }
}

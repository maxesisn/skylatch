// SPDX-License-Identifier: Apache-2.0

//! The 虚拟组网 tab of the drawer: this machine's EasyTier state and what can honestly be
//! done about it, known conflicts, the live peer table, and the end-to-end encrypted cloud
//! configuration with its networks, audit and recovery key. State and rules live in
//! [`crate::mesh_ui`]; the actions are methods of [`Proto`] in `mesh_actions.rs`.

use crate::hub_view::{Tone, card, dot, label, para, section, tag};
use crate::mesh_ui::{self, Cell, MeshField, PeerRow, RouteBadge, UnlockMode, Vault, VaultSession};
use crate::theme::{Pal, ui};
use crate::{Proto, Target, clip, mtxt, rgba, segmented, txt};
use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, FontWeight, Hsla, SharedString, div, px};
use std::time::Duration;

const PEER_ROW_H: f32 = 26.0;
/// Rows of the peer table before it scrolls.
const PEER_ROWS: usize = 12;
const COPIED_FLASH: Duration = Duration::from_millis(1800);

fn cell_color(cell: Cell, pal: &Pal) -> (Hsla, bool) {
    match cell {
        Cell::ManagedRunning => (rgba(pal.green, 1.0), false),
        Cell::ManagedStopped => (rgba(pal.warn, 1.0), false),
        Cell::ExternalConfigServer | Cell::ExternalManual => (rgba(pal.blue, 1.0), false),
        Cell::NotInstalled | Cell::InstalledIdle => (rgba(pal.muted, 0.9), true),
    }
}

/// One fact of the state card: a small label over its value.
fn fact(name: &'static str, value: impl Into<SharedString>, pal: &Pal) -> gpui::Div {
    div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap(px(2.0))
        .child(label(name, pal))
        .child(
            mtxt(value, 12.0, rgba(pal.ink, 1.0))
                .overflow_hidden()
                .text_ellipsis(),
        )
}

fn route_badge(route: &RouteBadge, pal: &Pal) -> gpui::Div {
    let color = match route {
        RouteBadge::P2p => rgba(pal.green, 1.0),
        RouteBadge::Relay => rgba(pal.blue, 1.0),
        RouteBadge::Other(_) => rgba(pal.muted, 1.0),
    };
    div()
        .h(px(ui::KBD_H))
        .w(px(44.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(px(ui::RAD_KBD))
        .bg(color.opacity(0.13))
        .child(txt(route.label(), 10.5, color).font_weight(FontWeight::MEDIUM))
}

fn peer_row(i: usize, row: &PeerRow, pal: &Pal) -> AnyElement {
    div()
        .h(px(PEER_ROW_H))
        .px(px(8.0))
        .flex()
        .flex_none()
        .items_center()
        .gap(px(10.0))
        .rounded(px(ui::RAD_INNER))
        .when(i % 2 == 1, |d| d.bg(rgba(pal.wash, 0.45)))
        .child(
            div().flex_1().min_w(px(0.0)).overflow_hidden().child(
                txt(row.hostname.clone(), 12.0, rgba(pal.ink, 1.0))
                    .overflow_hidden()
                    .text_ellipsis(),
            ),
        )
        .child(div().w(px(112.0)).flex_none().child(mtxt(
            row.ip.clone(),
            11.5,
            rgba(pal.muted, 1.0),
        )))
        .child(route_badge(&row.route, pal))
        .child(
            div()
                .w(px(58.0))
                .flex_none()
                .flex()
                .justify_end()
                .child(mtxt(row.latency.clone(), 11.5, rgba(pal.ink, 0.9))),
        )
        .into_any_element()
}

fn strip(title: &str, detail: &str, color: Hsla, pal: &Pal) -> AnyElement {
    div()
        .w_full()
        .px(px(12.0))
        .py(px(9.0))
        .flex()
        .flex_col()
        .gap(px(3.0))
        .rounded(px(ui::RAD_FIELD))
        .border_1()
        .border_color(color.opacity(0.35))
        .bg(color.opacity(0.07))
        .child(txt(title.to_string(), 12.0, color).font_weight(FontWeight::MEDIUM))
        .child(para(
            detail.trim_end_matches('。').to_string(),
            11.0,
            rgba(pal.muted, 1.0),
        ))
        .into_any_element()
}

impl Proto {
    fn copied_now(&self, what: &str) -> bool {
        self.mesh
            .copied
            .as_ref()
            .is_some_and(|(w, at)| w == what && at.elapsed() < COPIED_FLASH)
    }

    fn copy_btn(
        &self,
        id: &'static str,
        what: String,
        text: String,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let done = self.copied_now(&what);
        let wash = rgba(pal.wash, 1.0);
        div()
            .id(SharedString::from(format!("{id}-{what}")))
            .debug_selector(move || id.to_string())
            .h(px(ui::TAG_H))
            .px(px(7.0))
            .flex()
            .flex_none()
            .items_center()
            .rounded(px(ui::RAD_INNER))
            .border_1()
            .border_color(rgba(pal.hair, 1.0))
            .cursor_pointer()
            .hover(move |s| s.bg(wash))
            .child(txt(
                if done { "已复制 ✓" } else { "复制" },
                10.5,
                rgba(pal.accent, 1.0),
            ))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                this.copy_text(&what, &text, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    fn state_card(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let muted = rgba(pal.muted, 1.0);
        let Some(probe) = &self.mesh.probe else {
            return card(pal)
                .debug_selector(|| "mesh-state".to_string())
                .child(txt("正在检测本机 EasyTier…", 12.0, muted))
                .into_any_element();
        };
        let cell = probe.cell;
        let (color, hollow) = cell_color(cell, pal);
        let status = probe
            .networks
            .get(
                self.mesh
                    .peer_net
                    .min(probe.networks.len().saturating_sub(1)),
            )
            .and_then(|n| n.status.as_ref().ok());
        let version = status.and_then(|s| s.version.clone());
        let mut body = card(pal)
            .debug_selector(|| "mesh-state".to_string())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(dot(color, hollow))
                    .child(
                        txt(cell.title(), 13.0, rgba(pal.ink, 1.0))
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(div().flex_1())
                    .children(
                        (cell == Cell::ManagedRunning && probe.user_process)
                            .then(|| tag("用户进程", rgba(pal.accent, 1.0))),
                    )
                    .children(version.map(|v| {
                        tag(
                            format!("EasyTier {}", v.split('-').next().unwrap_or(&v)),
                            muted,
                        )
                    })),
            )
            .child(para(cell.detail(), 11.5, muted));
        let summary = match &probe.presence {
            skylatch_mesh::EasyTierPresence::ExternalConfigServer { summary, .. }
            | skylatch_mesh::EasyTierPresence::ExternalGuiOrManual { summary, .. }
                if cell.is_external() =>
            {
                Some(mesh_ui::summary_line(summary))
            }
            _ => None,
        };
        if let Some(line) = summary {
            body = body.child(mtxt(clip(&line, 58), 11.0, muted));
        }
        if let Some(s) = status {
            let rows = mesh_ui::peer_rows(s);
            let (p2p, relay) = mesh_ui::route_counts(&rows);
            body = body.child(
                div()
                    .pt(px(4.0))
                    .flex()
                    .gap(px(10.0))
                    .child(fact(
                        "本机虚拟 IP",
                        s.local_ipv4.clone().unwrap_or_else(|| "未分配".into()),
                        pal,
                    ))
                    .child(fact(
                        "NAT 类型",
                        s.nat_type.clone().unwrap_or_else(|| "未知".into()),
                        pal,
                    ))
                    .child(fact(
                        "在线节点",
                        format!("{}（P2P {p2p}，中继 {relay}）", rows.len()),
                        pal,
                    )),
            );
        }
        if let Some((what, _)) = &self.mesh.op {
            body = body.child(txt(format!("{what}…"), 11.5, rgba(pal.blue, 1.0)));
        }
        let idle = self.mesh.op.is_none();
        let open = self.mesh.vault.session().is_some();
        match cell {
            Cell::NotInstalled | Cell::InstalledIdle => {
                let synced = self.mesh.vault.cloud_has_config();
                if synced {
                    body = body.child(
                        txt(
                            "检测到账号已同步组网配置，一键安装并加入",
                            12.0,
                            rgba(pal.accent, 1.0),
                        )
                        .font_weight(FontWeight::MEDIUM),
                    );
                }
                let primary = match (cell, synced) {
                    (_, true) => "一键安装并加入",
                    (Cell::NotInstalled, false) => "一键安装并托管组网",
                    _ => "一键托管组网",
                };
                body = body
                    .child(self.check(
                        "mesh-no-tun",
                        "mesh-no-tun".into(),
                        self.mesh.no_tun,
                        "无 TUN 模式（--no-tun，无需管理员权限）",
                        idle,
                        pal,
                        cx,
                        |t, _| t.mesh.no_tun = !t.mesh.no_tun,
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(self.btn(
                                "mesh-install",
                                "mesh-install".into(),
                                primary,
                                Tone::Primary,
                                idle && open,
                                pal,
                                cx,
                                |t, _| t.mesh_install(),
                            ))
                            .child(self.btn(
                                "mesh-import",
                                "mesh-import".into(),
                                "导入已有 TOML",
                                Tone::Plain,
                                idle && open,
                                pal,
                                cx,
                                |t, _| t.open_import(mesh_ui::ImportIntro::Plain),
                            )),
                    );
                if !open {
                    body = body.child(txt("先在下方解锁云端组网配置，再安装或导入", 11.0, muted));
                }
            }
            Cell::ExternalConfigServer => {}
            Cell::ExternalManual => {
                body = body.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(self.btn(
                            "mesh-adopt",
                            "mesh-adopt".into(),
                            "接入 Skylatch 协作管理",
                            Tone::Plain,
                            idle,
                            pal,
                            cx,
                            |t, _| t.mesh_adopt(),
                        ))
                        .child(txt("导入并确认冲突，不会强停现有进程", 11.0, muted)),
                );
            }
            Cell::ManagedRunning | Cell::ManagedStopped => {
                let running = cell == Cell::ManagedRunning;
                body = body
                    .child(
                        div()
                            .pt(px(2.0))
                            .flex()
                            .items_center()
                            .gap(px(18.0))
                            .child(self.check(
                                "mesh-run",
                                "mesh-run".into(),
                                running,
                                "启用组网",
                                idle,
                                pal,
                                cx,
                                move |t, _| t.mesh_run(!running),
                            ))
                            .child(self.check(
                                "mesh-managed-no-tun",
                                "mesh-managed-no-tun".into(),
                                probe.no_tun(),
                                "无 TUN 模式（--no-tun）",
                                idle && open,
                                pal,
                                cx,
                                |t, _| t.mesh_toggle_no_tun(),
                            )),
                    )
                    .child(self.check(
                        "mesh-detach",
                        "mesh-detach".into(),
                        false,
                        "解除绑定（转为独立运行，EasyTier 保持运行）",
                        idle,
                        pal,
                        cx,
                        |t, _| t.mesh_detach(),
                    ));
            }
        }
        body.into_any_element()
    }

    fn conflict_strips(&self, pal: &Pal) -> Vec<AnyElement> {
        let Some(probe) = &self.mesh.probe else {
            return vec![];
        };
        mesh_ui::shown_conflicts(probe.cell, &probe.conflicts)
            .into_iter()
            .map(|c| {
                let color = match c.severity {
                    skylatch_mesh::ConflictSeverity::Blocking => rgba(pal.warn, 1.0),
                    skylatch_mesh::ConflictSeverity::Warning => rgba(pal.warn, 0.85),
                };
                strip(&c.title, &c.detail, color, pal)
            })
            .collect()
    }

    fn peers_section(&self, pal: &Pal, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(probe) = &self.mesh.probe else {
            return vec![];
        };
        if probe.networks.is_empty() {
            return vec![];
        }
        let muted = rgba(pal.muted, 1.0);
        let at = self.mesh.peer_net.min(probe.networks.len() - 1);
        let mut out = vec![];
        let mut head = section("实时节点", pal);
        if probe.networks.len() > 1 {
            let names: Vec<String> = probe
                .networks
                .iter()
                .map(|n| {
                    let id = n.profile_id.clone().unwrap_or_default();
                    self.mesh
                        .vault
                        .session()
                        .and_then(|s| s.vault.profile(&id))
                        .map_or(id, |p| p.name.clone())
                })
                .collect();
            let mut seg = segmented("peer-net-seg", pal);
            for (i, name) in names.iter().enumerate() {
                seg = seg.child(self.seg_btn(
                    ("peer-net", i),
                    clip(name, 10),
                    i == at,
                    pal,
                    cx,
                    move |t| t.mesh.peer_net = i,
                ));
            }
            head = head.child(div().flex_1()).child(seg);
        } else {
            head = head
                .child(div().flex_1())
                .child(txt("每 3 秒刷新", 10.5, muted));
        }
        out.push(head.into_any_element());
        match &probe.networks[at].status {
            Err(e) => out.push(
                txt(format!("无法读取节点：{}", clip(e, 48)), 11.5, muted).into_any_element(),
            ),
            Ok(s) => {
                let rows = mesh_ui::peer_rows(s);
                let local = div()
                    .h(px(PEER_ROW_H))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .rounded(px(ui::RAD_INNER))
                    .bg(rgba(pal.accent, 0.07))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .overflow_hidden()
                            .child(
                                div().min_w(px(0.0)).overflow_hidden().child(
                                    txt(s.local_hostname.clone(), 12.0, rgba(pal.ink, 1.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .overflow_hidden()
                                        .text_ellipsis(),
                                ),
                            )
                            .child(crate::local_chip(pal)),
                    )
                    .child(div().w(px(112.0)).flex_none().child(mtxt(
                        s.local_ipv4.clone().unwrap_or_else(|| "—".into()),
                        11.5,
                        rgba(pal.ink, 1.0),
                    )))
                    .child(div().w(px(44.0)).flex_none())
                    .child(
                        div()
                            .w(px(58.0))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .child(txt(s.nat_type.clone().unwrap_or_default(), 10.0, muted)),
                    );
                let header = div()
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_1().child(label("主机", pal)))
                    .child(div().w(px(112.0)).flex_none().child(label("虚拟 IP", pal)))
                    .child(div().w(px(44.0)).flex_none().child(label("路由", pal)))
                    .child(
                        div()
                            .w(px(58.0))
                            .flex_none()
                            .flex()
                            .justify_end()
                            .child(label("延迟", pal)),
                    );
                let list_h = rows.len().min(PEER_ROWS) as f32 * PEER_ROW_H;
                out.push(
                    div()
                        .debug_selector(|| "mesh-peers".to_string())
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(header)
                        .child(local)
                        .child(
                            div()
                                .id("mesh-peer-list")
                                .h(px(list_h))
                                .overflow_y_scroll()
                                .child(div().flex().flex_col().children(
                                    rows.iter().enumerate().map(|(i, r)| peer_row(i, r, pal)),
                                )),
                        )
                        .children(
                            rows.is_empty()
                                .then(|| txt("还没有其他节点在线", 11.5, muted).px(px(8.0))),
                        )
                        .into_any_element(),
                );
            }
        }
        out
    }

    fn unlock_card(&self, pal: &Pal, cx: &mut Context<Self>, create: bool) -> AnyElement {
        let mode = if create {
            UnlockMode::Password
        } else {
            self.mesh.unlock_mode
        };
        let busy = self.mesh.vault_rx.is_some();
        let mut c = card(pal).debug_selector(|| "mesh-unlock".to_string()).child(para(
            if create {
                "账号还没有云端组网配置。输入本账号的登录密码，生成只有你的设备能解开的加密组网配置"
            } else {
                "账号已有端到端加密的组网配置。输入账号密码或恢复码解锁，只在内存中保留解密密钥"
            },
            11.5,
            rgba(pal.muted, 1.0),
        ));
        if !create {
            c = c.child(
                segmented("unlock-seg", pal)
                    .child(self.seg_btn(
                        "unlock-pw",
                        "账号密码",
                        mode == UnlockMode::Password,
                        pal,
                        cx,
                        |t| {
                            t.mesh.unlock_mode = UnlockMode::Password;
                            t.mesh.wipe_unlock();
                        },
                    ))
                    .child(self.seg_btn(
                        "unlock-rk",
                        "恢复码",
                        mode == UnlockMode::Recovery,
                        pal,
                        cx,
                        |t| {
                            t.mesh.unlock_mode = UnlockMode::Recovery;
                            t.mesh.wipe_unlock();
                        },
                    )),
            );
        }
        let shown = if mode == UnlockMode::Password {
            crate::onboard_view::mask(&self.mesh.unlock_text)
        } else {
            self.mesh.unlock_text.clone()
        };
        c.child(
            div()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_1().min_w(px(0.0)).child(self.text_box(
                    "mesh-unlock-field",
                    "mesh-unlock-field".into(),
                    Target::Mesh(MeshField::Unlock),
                    shown,
                    if mode == UnlockMode::Password {
                        "账号密码"
                    } else {
                        "SLK1-..."
                    },
                    pal,
                    cx,
                    |t| t.mesh.field = Some(MeshField::Unlock),
                )))
                .child(self.btn(
                    "mesh-unlock-go",
                    "mesh-unlock-go".into(),
                    if busy {
                        "解锁中…"
                    } else if create {
                        "创建"
                    } else {
                        "解锁"
                    },
                    Tone::Primary,
                    !busy,
                    pal,
                    cx,
                    |t, _| t.mesh_unlock(),
                )),
        )
        .into_any_element()
    }

    /// The editable fields of one network.
    fn profile_fields(
        &self,
        id: &str,
        single: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(item) = self.mesh.draft.as_ref().and_then(|d| d.item(id)) else {
            return vec![];
        };
        let hub_peers = self.hubinfo.peers();
        let mut out = vec![];
        let focus_id = id.to_string();
        out.push(label("名称", pal).into_any_element());
        out.push(self.text_box(
            SharedString::from(format!("mesh-name-{id}")),
            "mesh-name".into(),
            Target::Mesh(MeshField::Name),
            item.profile.name.clone(),
            "组网名称",
            pal,
            cx,
            move |t| t.focus_profile(&focus_id, MeshField::Name),
        ));
        let (a, b) = (id.to_string(), id.to_string());
        let mut toggles = div()
            .flex()
            .items_center()
            .gap(px(18.0))
            .child(self.check(
                SharedString::from(format!("mesh-join-{id}")),
                format!("mesh-join-{id}"),
                item.join,
                "本机加入",
                true,
                pal,
                cx,
                move |t, _| t.toggle_join(&a),
            ))
            .child(self.check(
                SharedString::from(format!("mesh-hubpeer-{id}")),
                format!("mesh-hubpeer-{id}"),
                item.profile.use_hub_peer,
                "叠加 Hub 配套打洞节点",
                true,
                pal,
                cx,
                move |t, _| t.toggle_hub_peer(&b),
            ));
        if !single {
            let c = id.to_string();
            toggles = toggles.child(div().flex_1()).child(self.btn(
                SharedString::from(format!("mesh-del-{id}")),
                format!("mesh-del-{id}"),
                "删除",
                Tone::Danger,
                true,
                pal,
                cx,
                move |t, _| t.remove_profile(&c),
            ));
        }
        out.push(toggles.into_any_element());
        if item.profile.use_hub_peer && !hub_peers.is_empty() {
            out.push(
                txt(
                    clip(&crate::hub_ui::peers_line(&hub_peers), 64),
                    11.0,
                    rgba(pal.muted, 1.0),
                )
                .into_any_element(),
            );
        }
        let focus_id = id.to_string();
        out.push(
            label(
                "自定义节点（每行一个，例如 tcp://public.example.com:11010）",
                pal,
            )
            .into_any_element(),
        );
        let focused = self.mesh.draft.as_ref().and_then(|d| d.focus.as_deref()) == Some(id);
        out.push(self.text_area(
            "mesh-peers-field",
            Target::Mesh(MeshField::Peers),
            if focused { &item.peers_text } else { "" },
            "可留空",
            64.0,
            pal,
            cx,
            move |t| t.focus_profile(&focus_id, MeshField::Peers),
        ));
        out
    }

    fn profile_card(
        &self,
        i: usize,
        id: &str,
        single: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.mesh.draft.as_ref().and_then(|d| d.item(id)) else {
            return div().into_any_element();
        };
        let expanded =
            single || self.mesh.draft.as_ref().and_then(|d| d.focus.as_deref()) == Some(id);
        let muted = rgba(pal.muted, 1.0);
        let p = &item.profile;
        let toggle_id = id.to_string();
        let join_id = id.to_string();
        let accent = rgba(pal.accent, 1.0);
        let head = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .when(!single, |d| {
                d.child(self.check(
                    ("mesh-pick", i),
                    format!("mesh-pick-{i}"),
                    item.join,
                    "",
                    true,
                    pal,
                    cx,
                    move |t, _| t.toggle_join(&join_id),
                ))
            })
            .child(div().flex_1().min_w(px(0.0)).overflow_hidden().child(
                txt(clip(&p.name, 18), 12.5, rgba(pal.ink, 1.0)).font_weight(FontWeight::MEDIUM),
            ))
            .child(mtxt(p.subnet.clone(), 11.0, muted))
            .child(mtxt(format!(":{}", p.listen_port), 11.0, muted))
            .when(!single, |d| {
                d.child(
                    div()
                        .id(("mesh-expand", i))
                        .debug_selector(move || format!("mesh-expand-{i}"))
                        .px(px(6.0))
                        .h(px(ui::MINI_H))
                        .flex()
                        .items_center()
                        .rounded(px(ui::RAD_INNER))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                        .child(txt(if expanded { "收起" } else { "编辑" }, 11.0, accent))
                        .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                            this.expand_profile(&toggle_id);
                            cx.notify();
                        })),
                )
            });
        card(pal)
            .debug_selector(move || format!("mesh-profile-{i}"))
            .when(item.join, |d| d.border_color(rgba(pal.accent, 0.45)))
            .child(head)
            .when(expanded, |d| {
                d.children(self.profile_fields(id, single, pal, cx))
            })
            .into_any_element()
    }

    fn audit_section(
        &self,
        s: &VaultSession,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let muted = rgba(pal.muted, 1.0);
        let ink = rgba(pal.ink, 1.0);
        let accent = rgba(pal.accent, 1.0);
        let open = self.mesh.audit_open;
        let mut out = vec![
            div()
                .id("mesh-audit-toggle")
                .debug_selector(|| "mesh-audit-toggle".to_string())
                .flex()
                .items_center()
                .gap(px(6.0))
                .cursor_pointer()
                .child(txt(if open { "▾" } else { "▸" }, 11.0, accent))
                .child(txt("加密审计与恢复码", 12.0, accent).font_weight(FontWeight::MEDIUM))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    this.mesh.audit_open = !this.mesh.audit_open;
                    if !this.mesh.audit_open {
                        this.mesh.reveal = false;
                    }
                    cx.notify();
                }))
                .into_any_element(),
        ];
        if !open {
            return out;
        }
        let reveal = self.mesh.reveal;
        let env = &s.envelope;
        let row = |name: &'static str, value: String| {
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(div().w(px(84.0)).flex_none().child(label(name, pal)))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_hidden()
                        .child(mtxt(value, 11.5, ink).overflow_hidden().text_ellipsis()),
                )
        };
        let mut c = card(pal)
            .debug_selector(|| "mesh-audit".to_string())
            .child(row(
                "云端信封",
                if s.uploaded() {
                    format!(
                        "版本 v{}，{} 字节密文",
                        env.version,
                        mesh_ui::ciphertext_len(env)
                    )
                } else {
                    format!("尚未上传，{} 字节密文", mesh_ui::ciphertext_len(env))
                },
            ))
            .child(row(
                "密文指纹",
                format!("SHA-256 {}", mesh_ui::fingerprint(env)),
            ))
            .child(row("加密方式", "ChaCha20-Poly1305 + Argon2id".to_string()))
            .child(para(
                "Hub 只保存上面这份密文；网络密钥只在你的设备上用账号密码或恢复码解开",
                11.0,
                muted,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label("明文网络凭据（仅本机内存）", pal))
                    .child(
                        div()
                            .id("mesh-reveal")
                            .debug_selector(|| "mesh-reveal".to_string())
                            .cursor_pointer()
                            .child(txt(if reveal { "隐藏" } else { "显示" }, 11.0, accent))
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.mesh.reveal = !this.mesh.reveal;
                                cx.notify();
                            })),
                    ),
            );
        for p in &s.vault.profiles {
            let secret = if reveal {
                mesh_ui::grouped(&p.network_secret, 8)
            } else {
                crate::onboard_view::mask(&p.network_secret)
                    .chars()
                    .take(24)
                    .collect()
            };
            c = c.child(
                div()
                    .pt(px(2.0))
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(txt(p.name.clone(), 11.5, ink).font_weight(FontWeight::MEDIUM))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .w(px(84.0))
                                    .flex_none()
                                    .child(label("network_name", pal)),
                            )
                            .child(
                                div().flex_1().min_w(px(0.0)).overflow_hidden().child(
                                    mtxt(p.network_name.clone(), 11.0, ink)
                                        .overflow_hidden()
                                        .text_ellipsis(),
                                ),
                            )
                            .child(self.copy_btn(
                                "copy-name",
                                format!("name-{}", p.id),
                                p.network_name.clone(),
                                pal,
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .w(px(84.0))
                                    .flex_none()
                                    .child(label("network_secret", pal)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .child(mtxt(secret, 11.0, ink).whitespace_normal()),
                            )
                            .child(self.copy_btn(
                                "copy-secret",
                                format!("secret-{}", p.id),
                                p.network_secret.clone(),
                                pal,
                                cx,
                            )),
                    ),
            );
        }
        let recovery = mesh_ui::recovery_key(s);
        let shown = if reveal {
            recovery.clone()
        } else {
            format!("SLK1-{}", "••••-".repeat(4).trim_end_matches('-'))
        };
        c = c.child(
            div()
                .pt(px(4.0))
                .flex()
                .flex_col()
                .gap(px(4.0))
                .child(label("恢复码（忘记密码时用它解锁，请离线保存）", pal))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .px(px(8.0))
                                .py(px(6.0))
                                .rounded(px(ui::RAD_INNER))
                                .bg(rgba(pal.wash, 1.0))
                                .child(mtxt(shown, 11.0, rgba(pal.ink, 1.0)).whitespace_normal()),
                        )
                        .child(self.copy_btn(
                            "copy-recovery",
                            "recovery".into(),
                            recovery,
                            pal,
                            cx,
                        )),
                ),
        );
        out.push(c.into_any_element());
        out
    }

    fn vault_section(&self, pal: &Pal, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let muted = rgba(pal.muted, 1.0);
        let mut head = section("云端组网配置", pal).child(tag("端到端加密", rgba(pal.green, 1.0)));
        if let Some(s) = self.mesh.vault.session() {
            head = head.child(div().flex_1()).child(if s.uploaded() {
                tag(format!("云端 v{}", s.envelope.version), muted)
            } else {
                tag("尚未上传", rgba(pal.warn, 1.0))
            });
        }
        let mut out = vec![head.into_any_element()];
        match &self.mesh.vault {
            Vault::Unknown => out.push(
                txt("连接 Hub 并登录后，组网配置会加密同步到账号", 11.5, muted).into_any_element(),
            ),
            Vault::Loading => {
                out.push(txt("正在读取云端组网配置…", 11.5, muted).into_any_element())
            }
            Vault::Failed(e) => out.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(txt(
                        format!("无法读取：{}", clip(e, 34)),
                        11.5,
                        rgba(pal.warn, 1.0),
                    ))
                    .child(self.btn(
                        "mesh-vault-retry",
                        "mesh-vault-retry".into(),
                        "重试",
                        Tone::Plain,
                        true,
                        pal,
                        cx,
                        |t, _| t.fetch_vault(),
                    ))
                    .into_any_element(),
            ),
            Vault::Absent => out.push(self.unlock_card(pal, cx, true)),
            Vault::Locked(_) => out.push(self.unlock_card(pal, cx, false)),
            Vault::Open(s) => {
                let Some(draft) = &self.mesh.draft else {
                    return out;
                };
                let single = draft.items.len() == 1;
                if !single {
                    out.push(
                        txt(
                            format!(
                                "{} 个隔离网络，本机加入 {} 个；勾选决定这台电脑加入哪些",
                                draft.items.len(),
                                draft.joined().len()
                            ),
                            11.5,
                            muted,
                        )
                        .into_any_element(),
                    );
                }
                let ids: Vec<String> = draft.items.iter().map(|i| i.profile.id.clone()).collect();
                for (i, id) in ids.iter().enumerate() {
                    out.push(self.profile_card(i, id, single, pal, cx));
                }
                let busy = self.mesh.busy();
                let can_save = !busy && (draft.dirty || !s.uploaded());
                let local = self.mesh.cell().is_some_and(Cell::is_managed);
                out.push(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.0))
                        .child(self.btn(
                            "mesh-save",
                            "mesh-save".into(),
                            if self.mesh.vault_rx.is_some() {
                                "同步中…"
                            } else if local {
                                "保存并同步"
                            } else {
                                "保存到云端"
                            },
                            Tone::Primary,
                            can_save,
                            pal,
                            cx,
                            |t, _| t.save_mesh(),
                        ))
                        .child(self.btn(
                            "mesh-add",
                            "mesh-add".into(),
                            "+ 新增隔离网络",
                            Tone::Plain,
                            !busy,
                            pal,
                            cx,
                            |t, _| t.add_profile(),
                        ))
                        .child(self.btn(
                            "mesh-import-2",
                            "mesh-import-2".into(),
                            "导入 TOML",
                            Tone::Plain,
                            !busy,
                            pal,
                            cx,
                            |t, _| t.open_import(mesh_ui::ImportIntro::Plain),
                        ))
                        .into_any_element(),
                );
                if self.mesh.cell().is_some_and(Cell::is_external) {
                    out.push(
                        para(
                            "本机 EasyTier 由外部管理：保存只更新云端密文，不改动本机配置",
                            11.0,
                            muted,
                        )
                        .into_any_element(),
                    );
                }
                out.extend(self.audit_section(s, pal, cx));
            }
        }
        out
    }

    pub(crate) fn mesh_tab(&self, pal: &Pal, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mut out = vec![self.state_card(pal, cx)];
        out.extend(self.conflict_strips(pal));
        out.extend(self.peers_section(pal, cx));
        out.extend(self.vault_section(pal, cx));
        if let Some((text, error)) = &self.mesh.notice {
            out.push(
                para(
                    text.clone(),
                    11.5,
                    if *error {
                        rgba(pal.warn, 1.0)
                    } else {
                        rgba(pal.green, 1.0)
                    },
                )
                .into_any_element(),
            );
        }
        out
    }
}

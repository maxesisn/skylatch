// SPDX-License-Identifier: Apache-2.0

//! Dialogs of the 连接与组网 drawer: the sign-out guard while Skylatch manages this machine's
//! EasyTier, the EasyTier TOML import and the cloud-versus-local sync conflict.

use crate::hub_view::{Tone, para};
use crate::mesh_ui::{
    self, AccountAction, GuardChoice, ImportIntro, MeshField, Modal, SyncChoice, SyncConflict,
};
use crate::theme::{Pal, ui};
use crate::{Proto, Target, mtxt, rgba, txt};
use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, div, px};

const MODAL_W: f32 = 480.0;

fn shell(pal: &Pal) -> gpui::Div {
    div()
        .w(px(MODAL_W))
        .px(px(24.0))
        .py(px(22.0))
        .flex()
        .flex_col()
        .gap(px(12.0))
        .rounded(px(ui::RAD_PANEL))
        .bg(rgba(pal.bar, 1.0))
        .border_1()
        .border_color(rgba(pal.hair, 1.0))
        .shadow_md()
        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
            cx.stop_propagation()
        })
}

fn title_row(title: &'static str, pal: &Pal) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(txt(title, 17.0, rgba(pal.ink, 1.0)).font_weight(FontWeight::SEMIBOLD))
        .child(mtxt("Esc", 10.0, rgba(pal.muted, 1.0)))
}

impl Proto {
    /// A choice card: the default one is outlined in the accent colour.
    #[allow(clippy::too_many_arguments)]
    fn choice(
        &self,
        id: &'static str,
        title: &'static str,
        detail: &'static str,
        primary: bool,
        danger: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Proto) + 'static,
    ) -> AnyElement {
        let color = if danger {
            rgba(pal.warn, 1.0)
        } else if primary {
            rgba(pal.accent, 1.0)
        } else {
            rgba(pal.ink, 1.0)
        };
        let border = if primary {
            rgba(pal.accent, 0.8)
        } else if danger {
            rgba(pal.warn, 0.35)
        } else {
            rgba(pal.hair, 1.0)
        };
        let hover = if danger {
            rgba(pal.warn, 0.08)
        } else {
            rgba(pal.accent, 0.08)
        };
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .w_full()
            .px(px(14.0))
            .py(px(11.0))
            .flex()
            .flex_col()
            .gap(px(4.0))
            .rounded(px(ui::RAD_CTL))
            .border_1()
            .border_color(border)
            .bg(if primary {
                rgba(pal.accent, 0.06)
            } else {
                rgba(pal.surface, 1.0)
            })
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(txt(title, 13.0, color).font_weight(FontWeight::SEMIBOLD))
                    .when(primary, |d| {
                        d.child(crate::hub_view::tag("推荐", rgba(pal.accent, 1.0)))
                    }),
            )
            .child(para(detail, 11.5, rgba(pal.muted, 1.0)))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                on(this);
                cx.notify();
            }))
            .into_any_element()
    }

    fn guard_card(&self, action: AccountAction, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.mesh.op.as_ref().map(|(w, _)| *w);
        let lead = match action {
            AccountAction::SignOut => "退出账号前，请决定本机由 Skylatch 托管的 EasyTier 怎么办",
            AccountAction::Submit => {
                "切换 Hub 或账号前，请决定本机由 Skylatch 托管的 EasyTier 怎么办"
            }
        };
        shell(pal)
            .id("mesh-guard")
            .debug_selector(|| "mesh-guard".to_string())
            .child(title_row(
                match action {
                    AccountAction::SignOut => "退出账号",
                    AccountAction::Submit => "切换 Hub 账号",
                },
                pal,
            ))
            .child(para(lead, 12.0, rgba(pal.muted, 1.0)))
            .child(self.choice(
                "guard-keep",
                "仅退出 Skylatch，保留 EasyTier 独立运行",
                "组网保持连通，配置文件留在本机，之后不再随账号同步；以后可再接入",
                true,
                false,
                pal,
                cx,
                |t| t.resolve_guard(GuardChoice::KeepIndependent),
            ))
            .child(self.choice(
                "guard-clear",
                "同时停用并清除本机 EasyTier 组网",
                "停止托管的 EasyTier，删除 Skylatch 写入的组网配置；本机将离开这些虚拟网络",
                false,
                true,
                pal,
                cx,
                |t| t.resolve_guard(GuardChoice::StopAndClear),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(txt(
                        busy.map_or("Enter 选择推荐项，Esc 取消".to_string(), |w| {
                            format!("{w}…")
                        }),
                        11.0,
                        if busy.is_some() {
                            rgba(pal.blue, 1.0)
                        } else {
                            rgba(pal.muted, 1.0)
                        },
                    ))
                    .child(self.btn(
                        "guard-cancel",
                        "guard-cancel".into(),
                        "取消",
                        Tone::Plain,
                        busy.is_none(),
                        pal,
                        cx,
                        |t, _| t.close_modal(),
                    )),
            )
            .into_any_element()
    }

    fn import_card(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let Some(Modal::Import(d)) = &self.mesh.modal else {
            return div().into_any_element();
        };
        let muted = rgba(pal.muted, 1.0);
        let (preview, color) = match &d.preview {
            None => ("粘贴后实时校验并预览网络名与节点数".to_string(), muted),
            Some(Ok(p)) => (mesh_ui::import_summary(p), rgba(pal.green, 1.0)),
            Some(Err(e)) => (e.clone(), rgba(pal.warn, 1.0)),
        };
        let ready = d.ready().is_some();
        shell(pal)
            .id("mesh-import-modal")
            .debug_selector(|| "mesh-import-modal".to_string())
            .child(title_row(
                if d.intro == ImportIntro::Adopt {
                    "接入 Skylatch 协作管理"
                } else {
                    "导入 EasyTier TOML"
                },
                pal,
            ))
            .child(para(
                if d.intro == ImportIntro::Adopt {
                    "粘贴这台机器现有 EasyTier 的 TOML，导入为一个云端组网。原 EasyTier 保持运行，确认网段和端口不冲突后再启用托管"
                } else {
                    "粘贴官方 easytier-gui 导出或手写的 config.toml；网段、端口与已有组网重叠时自动换成空闲值"
                },
                12.0,
                muted,
            ))
            .child(self.text_area(
                "mesh-import-field",
                Target::Mesh(MeshField::Import),
                &d.text,
                "[network_identity]\nnetwork_name = \"...\"\nnetwork_secret = \"...\"",
                200.0,
                pal,
                cx,
                |t| t.mesh.field = Some(MeshField::Import),
            ))
            .child(
                div()
                    .debug_selector(|| "mesh-import-preview".to_string())
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(crate::hub_view::dot(color, d.preview.is_none()))
                    .child(para(preview, 11.5, color)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(self.btn(
                        "import-go",
                        "import-go".into(),
                        "导入",
                        Tone::Primary,
                        ready,
                        pal,
                        cx,
                        |t, _| t.confirm_import(),
                    ))
                    .child(self.btn(
                        "import-cancel",
                        "import-cancel".into(),
                        "取消",
                        Tone::Plain,
                        true,
                        pal,
                        cx,
                        |t, _| t.close_modal(),
                    ))
                    .child(crate::hint_row(&[("Ctrl+V", "粘贴"), ("Enter", "换行")], pal)),
            )
            .into_any_element()
    }

    fn conflict_card(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let Some(Modal::Conflict(c)) = &self.mesh.modal else {
            return div().into_any_element();
        };
        let lead = match c {
            SyncConflict::Remote { current: Some(e) } => format!(
                "保存时发现云端组网配置已被其他设备更新到 v{}，与本机的修改不一致",
                e.version
            ),
            SyncConflict::Remote { current: None } => "保存时发现云端组网配置已被删除".to_string(),
            SyncConflict::LocalDiffers { local, cloud } => {
                format!("本机运行的组网写自云端 v{local}，云端现在是 v{cloud}，两边配置不同")
            }
        };
        shell(pal)
            .id("mesh-conflict")
            .debug_selector(|| "mesh-conflict".to_string())
            .child(title_row("组网配置冲突", pal))
            .child(para(lead, 12.0, rgba(pal.muted, 1.0)))
            .child(self.choice(
                "conflict-cloud",
                "用云端替换本机",
                "采用云端最新的组网配置，本机的修改作废",
                true,
                false,
                pal,
                cx,
                |t| t.resolve_conflict(SyncChoice::UseCloud),
            ))
            .child(self.choice(
                "conflict-local",
                "用本机覆盖云端",
                "把本机这一份重新加密上传，覆盖云端的版本",
                false,
                false,
                pal,
                cx,
                |t| t.resolve_conflict(SyncChoice::UseLocal),
            ))
            .child(self.choice(
                "conflict-keep",
                "保持独立不同步",
                "本机 EasyTier 保持现状独立运行，不再随云端同步",
                false,
                false,
                pal,
                cx,
                |t| t.resolve_conflict(SyncChoice::KeepIndependent),
            ))
            .into_any_element()
    }

    /// The open dialog over a dimmed window.
    pub(crate) fn mesh_modal(&self, pal: &Pal, cx: &mut Context<Self>) -> Option<AnyElement> {
        let card = match self.mesh.modal.as_ref()? {
            Modal::Guard(action) => self.guard_card(*action, pal, cx),
            Modal::Import(_) => self.import_card(pal, cx),
            Modal::Conflict(_) => self.conflict_card(pal, cx),
        };
        Some(
            div()
                .id("mesh-scrim")
                .debug_selector(|| "mesh-scrim".to_string())
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(pal.ink, 0.30))
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e: &MouseDownEvent, window, cx| {
                        window.focus(&this.focus);
                        cx.stop_propagation();
                    }),
                )
                .child(card)
                .into_any_element(),
        )
    }
}

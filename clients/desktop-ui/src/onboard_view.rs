// SPDX-License-Identifier: Apache-2.0

//! Views of the first-run flow: the onboarding card on the empty L-desk, the "add the first
//! device" card, the `+ 添加设备` header button and the pairing modal. State and validation
//! live in [`crate::onboard`]; the actions are methods of [`Proto`] in `main.rs`.

use crate::header_icons::{Icon, Look};
use crate::host_mgr::{ChipKind, chip_look};
use crate::onboard::{self, OnbField, Tab};
use crate::scene::HEADER_H;
use crate::theme::{Pal, ui};
use crate::{Proto, Target, clip, hint_row, mtxt, paint_mark, rgba, segmented, txt};
use gpui::prelude::*;
use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, canvas, div, px,
};

const CARD_W: f32 = 440.0;

/// Frame shared by every card of this module.
fn card_shell(pal: &Pal, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .px(px(26.0))
        .py(px(24.0))
        .flex()
        .flex_col()
        .gap(px(11.0))
        .rounded(px(ui::RAD_PANEL))
        .bg(rgba(pal.bar, 1.0))
        .border_1()
        .border_color(rgba(pal.hair, 1.0))
        .shadow_md()
}

/// Centres `card` over the whole stage.
fn centered(card: impl IntoElement) -> AnyElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(card)
        .into_any_element()
}

pub(crate) fn mask(s: &str) -> String {
    "•".repeat(s.chars().count().min(48))
}

impl Proto {
    fn small_label(&self, text: &'static str, pal: &Pal) -> gpui::Div {
        txt(text, 11.0, rgba(pal.muted, 1.0))
    }

    /// One single-line input of the onboarding form.
    fn onb_input(
        &self,
        id: &'static str,
        field: OnbField,
        value: String,
        placeholder: &'static str,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.onb.field == field;
        let secret = matches!(
            field,
            OnbField::Password | OnbField::Token | OnbField::DirectToken
        );
        let preedit = self.preedit(Target::Onb(field), secret, 12.0, true, pal);
        let empty = value.is_empty() && preedit.is_none();
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .relative()
            .h(px(ui::FIELD_H))
            .w_full()
            .px(px(10.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(ui::RAD_FIELD))
            .border_1()
            .border_color(if focused {
                rgba(pal.accent, 1.0)
            } else {
                rgba(pal.hair, 1.0)
            })
            .bg(rgba(pal.canvas, 1.0))
            .cursor_text()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .justify_end()
                    .child(if empty {
                        mtxt(placeholder, 12.0, rgba(pal.muted, 0.7))
                    } else {
                        mtxt(value, 12.0, rgba(pal.ink, 1.0))
                    })
                    .children(preedit)
                    .when(focused, |d| {
                        d.child(
                            div()
                                .w(px(1.5))
                                .h(px(15.0))
                                .ml(px(1.0))
                                .flex_none()
                                .bg(rgba(pal.accent, 1.0)),
                        )
                    }),
            )
            .child(self.ime_anchor(Target::Onb(field), cx))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                this.marked = None;
                this.onb.field = field;
                cx.notify();
            }))
            .into_any_element()
    }

    /// The room of the machine that is about to join, in the add-device modal.
    fn pair_room_input(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let preedit = self.preedit(Target::Pair, false, 12.0, true, pal);
        let value = self.pair.room.clone();
        let empty = value.is_empty() && preedit.is_none();
        div()
            .id("pair-room")
            .debug_selector(|| "pair-room".to_string())
            .relative()
            .h(px(ui::FIELD_H))
            .w_full()
            .px(px(10.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(ui::RAD_FIELD))
            .border_1()
            .border_color(rgba(pal.accent, 1.0))
            .bg(rgba(pal.canvas, 1.0))
            .cursor_text()
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .justify_end()
                    .child(if empty {
                        mtxt("留空则由 Hub 决定，例如 办公室", 12.0, rgba(pal.muted, 0.7))
                    } else {
                        mtxt(value, 12.0, rgba(pal.ink, 1.0))
                    })
                    .children(preedit)
                    .child(
                        div()
                            .w(px(1.5))
                            .h(px(15.0))
                            .ml(px(1.0))
                            .flex_none()
                            .bg(rgba(pal.accent, 1.0)),
                    ),
            )
            .child(self.ime_anchor(Target::Pair, cx))
            .into_any_element()
    }

    fn solid_button(
        &self,
        id: &'static str,
        label: String,
        enabled: bool,
        pal: &Pal,
        on: impl Fn(&mut Proto, &mut Context<Proto>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let on_accent = rgba(pal.on_accent, 1.0);
        let hover = rgba(pal.accent.mix(pal.ink, 0.18), 1.0);
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .h(px(ui::FIELD_H))
            .px(px(18.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(ui::RAD_FIELD))
            .bg(rgba(pal.accent, 1.0))
            .opacity(if enabled { 1.0 } else { 0.45 })
            .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(hover)))
            .child(txt(label, 13.0, on_accent).font_weight(FontWeight::SEMIBOLD))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                if enabled {
                    on(this, cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn outline_button(
        &self,
        id: &'static str,
        label: &'static str,
        pal: &Pal,
        on: impl Fn(&mut Proto, &mut Context<Proto>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let wash = rgba(pal.wash, 1.0);
        div()
            .id(id)
            .debug_selector(|| id.to_string())
            .h(px(ui::FIELD_H))
            .px(px(16.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(ui::RAD_FIELD))
            .border_1()
            .border_color(rgba(pal.hair, 1.0))
            .bg(rgba(pal.surface, 1.0))
            .cursor_pointer()
            .hover(move |s| s.bg(wash))
            .child(txt(label, 13.0, rgba(pal.ink, 1.0)))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                on(this, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    // ───────────── onboarding ─────────────

    /// The empty-desk card: connect to a hub (password or account token) or connect directly.
    pub(crate) fn onboarding_card(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let ink = rgba(pal.ink, 1.0);
        let muted = rgba(pal.muted, 1.0);
        let accent = rgba(pal.accent, 1.0);
        let o = &self.onb;
        let mark_pal = *pal;

        let tabs = segmented("onb-tabs", pal)
            .child(self.seg_btn(
                "onb-tab-hub",
                "连接 Hub",
                o.tab == Tab::Hub,
                pal,
                cx,
                |t| t.onb.set_tab(Tab::Hub),
            ))
            .child(self.seg_btn(
                "onb-tab-direct",
                "直接连接",
                o.tab == Tab::Direct,
                pal,
                cx,
                |t| t.onb.set_tab(Tab::Direct),
            ));

        let mut form: Vec<AnyElement> = Vec::new();
        match (o.tab, o.token_mode) {
            (Tab::Hub, false) => {
                form.push(self.small_label("Hub 地址", pal).into_any_element());
                form.push(self.onb_input(
                    "onb-hub-url",
                    OnbField::HubUrl,
                    o.hub_url.clone(),
                    "https://hub.example.com",
                    pal,
                    cx,
                ));
                form.push(self.small_label("用户名", pal).into_any_element());
                form.push(self.onb_input(
                    "onb-user",
                    OnbField::Username,
                    o.username.clone(),
                    "用户名",
                    pal,
                    cx,
                ));
                form.push(self.small_label("密码", pal).into_any_element());
                form.push(self.onb_input(
                    "onb-pass",
                    OnbField::Password,
                    mask(&o.password),
                    "密码",
                    pal,
                    cx,
                ));
            }
            (Tab::Hub, true) => {
                form.push(self.small_label("Hub 地址", pal).into_any_element());
                form.push(self.onb_input(
                    "onb-hub-url",
                    OnbField::HubUrl,
                    o.hub_url.clone(),
                    "https://hub.example.com",
                    pal,
                    cx,
                ));
                form.push(self.small_label("账户 Token", pal).into_any_element());
                form.push(self.onb_input(
                    "onb-token",
                    OnbField::Token,
                    mask(&o.token),
                    crate::ONB_TOKEN_PLACEHOLDER,
                    pal,
                    cx,
                ));
            }
            (Tab::Direct, _) => {
                form.push(self.small_label("主机地址", pal).into_any_element());
                form.push(self.onb_input(
                    "onb-direct-url",
                    OnbField::DirectUrl,
                    o.direct_url.clone(),
                    "wss://host.example.com:7001",
                    pal,
                    cx,
                ));
                form.push(
                    self.small_label("直连 Token（可选）", pal)
                        .into_any_element(),
                );
                form.push(self.onb_input(
                    "onb-direct-token",
                    OnbField::DirectToken,
                    mask(&o.direct_token),
                    "64 位十六进制 Token",
                    pal,
                    cx,
                ));
            }
        }

        let signup_toggle = (o.tab == Tab::Hub && !o.token_mode).then(|| {
            div()
                .id("onb-signup-toggle")
                .debug_selector(|| "onb-signup-toggle".to_string())
                .cursor_pointer()
                .child(txt(
                    if o.signup {
                        "登录已有账号"
                    } else {
                        "注册新账号"
                    },
                    11.5,
                    accent,
                ))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    this.onb.signup = !this.onb.signup;
                    this.onb.error = None;
                    cx.notify();
                }))
        });
        let toggle = (o.tab == Tab::Hub).then(|| {
            div()
                .id("onb-token-toggle")
                .cursor_pointer()
                .child(txt(
                    if o.token_mode {
                        "改用用户名和密码登录"
                    } else {
                        "改为粘贴账户 Token"
                    },
                    11.5,
                    accent,
                ))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    let on = !this.onb.token_mode;
                    this.onb.set_token_mode(on);
                    cx.notify();
                }))
        });

        // Hub sign-in also decides whether this machine can be taken over by the others
        let host_opt = (o.tab == Tab::Hub).then(|| {
            let on = o.host_enabled;
            let on_accent = rgba(pal.on_accent, 1.0);
            div()
                .id("onb-host")
                .debug_selector(|| "onb-host".to_string())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(9.0))
                .cursor_pointer()
                .child(
                    div()
                        .size(px(15.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(ui::RAD_KBD))
                        .border_1()
                        .border_color(if on { accent } else { rgba(pal.hair, 1.0) })
                        .bg(if on { accent } else { rgba(pal.canvas, 1.0) })
                        .when(on, |d| d.child(txt("✓", 10.5, on_accent))),
                )
                .child(txt("允许远端接管本机", 11.5, ink))
                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                    this.marked = None;
                    let on = !this.onb.host_enabled;
                    this.onb.set_host_enabled(on);
                    cx.notify();
                }))
        });
        // where this machine should land on its first login (optional), beside the checkbox
        let host_room = (o.tab == Tab::Hub && o.host_enabled).then(|| {
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_none().child(self.small_label("本机房间", pal)))
                .child(div().flex_1().min_w(px(0.0)).child(self.onb_input(
                    "onb-host-room",
                    OnbField::HostRoom,
                    o.host_room.clone(),
                    "Home 或 办公室",
                    pal,
                    cx,
                )))
        });
        let host_row = host_opt.map(|opt| {
            div()
                .flex()
                .items_center()
                .gap(px(14.0))
                .child(opt)
                .children(host_room)
        });

        let (status, status_col) = match (&o.error, o.busy) {
            (Some(e), _) => (clip(e, 56), rgba(pal.warn, 1.0)),
            (None, true) if o.signup => ("正在注册…".to_string(), rgba(pal.blue, 1.0)),
            (None, true) => ("正在登录…".to_string(), rgba(pal.blue, 1.0)),
            (None, false) => (
                match o.tab {
                    Tab::Hub if o.token_mode => "Token 保存在本机用户配置目录".to_string(),
                    Tab::Hub => "密码只用于登录，不会被保存，仅保留返回的会话 Token".to_string(),
                    Tab::Direct => "无需 Hub，直接启动 skylatch-client --direct".to_string(),
                },
                muted,
            ),
        };

        let (submit_label, submit_id) = match (o.tab, o.token_mode, o.busy) {
            (Tab::Hub, false, true) => ("请稍候…", "onb-submit"),
            (Tab::Hub, false, false) if o.signup => ("注册并登录", "onb-submit"),
            (Tab::Hub, false, false) => ("登录", "onb-submit"),
            (Tab::Hub, true, _) => ("连接 Hub", "onb-submit"),
            (Tab::Direct, _, _) => ("直接连接", "onb-submit"),
        };

        centered(
            card_shell(pal, CARD_W)
                .id("onboarding-card")
                .debug_selector(|| "onboarding-card".to_string())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(
                            canvas(|_, _, _| (), move |b, _, w, _| paint_mark(w, b, &mark_pal))
                                .w(px(22.0))
                                .h(px(22.0)),
                        )
                        .child(
                            txt("开始使用 Skylatch", 17.0, ink).font_weight(FontWeight::SEMIBOLD),
                        ),
                )
                .child(txt(
                    "这张桌子还是空的 — 连接 Hub，或直接连接一台主机",
                    12.0,
                    muted,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(tabs)
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(14.0))
                                .children(signup_toggle)
                                .children(toggle),
                        ),
                )
                .children(form)
                .children(host_row)
                .child(
                    div()
                        .h(px(18.0))
                        .overflow_hidden()
                        .child(txt(status, 11.5, status_col)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .child(self.solid_button(
                            submit_id,
                            submit_label.to_string(),
                            !o.busy,
                            pal,
                            |t, cx| t.onb_submit(cx),
                            cx,
                        ))
                        .child(hint_row(
                            &[
                                ("Tab", "切换字段"),
                                ("Enter", "提交"),
                                (crate::PASTE_KEY, "粘贴"),
                            ],
                            pal,
                        )),
                ),
        )
    }

    // ───────────── pairing ─────────────

    /// `+ 添加设备`, always in the header while a hub is configured.
    pub(crate) fn add_device_button(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let wash = rgba(pal.wash, 1.0);
        div()
            .id("add-device")
            .debug_selector(|| "add-device".to_string())
            .h(px(ui::CTL_H))
            .px(px(12.0))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .rounded(px(ui::RAD_CTL))
            .border_1()
            .border_color(rgba(pal.hair, 1.0))
            .bg(rgba(pal.surface, 1.0))
            .cursor_pointer()
            .hover(move |s| s.bg(wash))
            .child(txt("+", 14.0, rgba(pal.muted, 1.0)))
            .child(txt("添加设备", 12.5, rgba(pal.ink, 1.0)).font_weight(FontWeight::MEDIUM))
            .on_click(cx.listener(|this, _e: &ClickEvent, window, cx| {
                window.focus(&this.focus);
                this.open_pair_modal();
                cx.notify();
            }))
            .into_any_element()
    }

    /// Header icon showing whether this machine can be taken over; a click switches it.
    pub(crate) fn host_chip(&self, pal: &Pal, cx: &mut Context<Self>) -> Option<AnyElement> {
        let host = self.host.as_ref()?;
        let (kind, _) = chip_look(self.stored.host_enabled, &host.status());
        let (look, dot) = match kind {
            ChipKind::Shared => (Look::On, Some((rgba(pal.green, 1.0), false))),
            ChipKind::Starting => (Look::Rest, Some((rgba(pal.blue, 1.0), true))),
            ChipKind::Problem => (Look::Rest, Some((rgba(pal.warn, 1.0), false))),
            ChipKind::Off => (Look::Dim, None),
        };
        Some(
            self.icon_btn("host-chip", Icon::Share, look, dot, pal, cx, |t| {
                t.set_host_enabled(!t.stored.host_enabled)
            }),
        )
    }

    /// Big code, validity badge, copyable command and the regenerate button; shared by the
    /// first-device card and the modal.
    fn pair_body(&self, pal: &Pal, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let ink = rgba(pal.ink, 1.0);
        let muted = rgba(pal.muted, 1.0);
        let warn = rgba(pal.warn, 1.0);
        let p = &self.pair;
        let live_code = p.code.as_ref().filter(|c| !c.expired());

        let big = match &p.code {
            Some(c) if !c.expired() => {
                mtxt(c.code.clone(), 46.0, ink).font_weight(FontWeight::SEMIBOLD)
            }
            Some(c) => mtxt(c.code.clone(), 46.0, rgba(pal.muted, 0.55)),
            None => mtxt("•••-•••", 46.0, rgba(pal.muted, 0.45)),
        };
        let (badge_text, badge_col) = match (&p.code, p.busy, &p.error) {
            (Some(c), _, _) if c.expired() => ("已过期，请换一码".to_string(), warn),
            (Some(c), _, _) => (
                format!("{} 分钟内有效，仅限使用一次", c.minutes_left().max(1)),
                muted,
            ),
            (None, true, _) => ("正在生成配对码…".to_string(), rgba(pal.blue, 1.0)),
            (None, false, Some(e)) => (clip(e, 52), warn),
            (None, false, None) => ("点击「换一码」获取配对码".to_string(), muted),
        };
        let command = match p.join_code() {
            Some(code) => onboard::join_command(
                &crate::config::normalize_hub_url(&self.eff.hub_url),
                &code,
                Some(&self.pair.room),
            ),
            None => "skylatch-agent join <hub> --code <code>".to_string(),
        };
        let suffix = p.invite_suffix().map(str::to_string);
        let mesh_name = p.mesh.as_ref().and_then(|id| {
            self.mesh
                .vault
                .session()
                .and_then(|s| s.vault.profile(id))
                .map(|pr| pr.name.clone())
        });
        let copied = p.copied_at.is_some();

        vec![
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(10.0))
                .py(px(16.0))
                .rounded(px(ui::RAD_CTL))
                .border_1()
                .border_color(rgba(pal.hair, 1.0))
                .bg(rgba(pal.canvas, 1.0))
                .child(big)
                .children(suffix.map(|sfx| {
                    div()
                        .debug_selector(|| "pair-invite".to_string())
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(3.0))
                        .child(mtxt(
                            format!("{}-{sfx}", p.code.as_ref().map_or("", |c| c.code.as_str())),
                            15.0,
                            if live_code.is_some() {
                                rgba(pal.accent, 1.0)
                            } else {
                                rgba(pal.muted, 0.55)
                            },
                        ))
                        .child(txt(
                            format!(
                                "扩展加入码：新设备自动加入「{}」",
                                mesh_name.clone().unwrap_or_default()
                            ),
                            11.0,
                            muted,
                        ))
                }))
                .child(
                    div()
                        .h(px(ui::TAG_H))
                        .px(px(8.0))
                        .flex()
                        .items_center()
                        .rounded(px(ui::RAD_INNER))
                        .bg(badge_col.opacity(0.10))
                        .child(txt(badge_text, 11.0, badge_col)),
                )
                .into_any_element(),
            div()
                .debug_selector(|| "pair-command".to_string())
                .min_h(px(ui::FIELD_H))
                .px(px(10.0))
                .py(px(7.0))
                .flex()
                .items_center()
                .overflow_hidden()
                .rounded(px(ui::RAD_FIELD))
                .bg(rgba(pal.wash, 1.0))
                .child(
                    mtxt(
                        command,
                        11.5,
                        if live_code.is_some() {
                            ink
                        } else {
                            rgba(pal.muted, 0.7)
                        },
                    )
                    .whitespace_normal()
                    .flex_1()
                    .min_w(px(0.0)),
                )
                .into_any_element(),
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(
                    self.solid_button(
                        "pair-copy",
                        if copied {
                            "已复制 ✓"
                        } else {
                            "复制命令"
                        }
                        .to_string(),
                        live_code.is_some(),
                        pal,
                        |t, cx| t.copy_join_command(cx),
                        cx,
                    ),
                )
                .child(self.outline_button(
                    "pair-again",
                    "换一码",
                    pal,
                    |t, _cx| t.request_code(),
                    cx,
                ))
                .into_any_element(),
        ]
    }

    /// Shown while the hub is reachable but no device is online yet.
    pub(crate) fn first_device_card(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let body = self.pair_body(pal, cx);
        centered(
            card_shell(pal, CARD_W)
                .id("first-device-card")
                .debug_selector(|| "first-device-card".to_string())
                .child(
                    txt("添加第一台设备", 18.0, rgba(pal.ink, 1.0))
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .child(txt(
                    "在要远程控制的电脑上运行下面的命令，设备上线后会自动出现在桌上",
                    12.0,
                    rgba(pal.muted, 1.0),
                ))
                .children(body),
        )
    }

    /// 新设备自动加入组网: offered once the account's mesh configuration is unlocked.
    fn pair_mesh_row(&self, pal: &Pal, cx: &mut Context<Self>) -> Option<AnyElement> {
        let vault = &self.mesh.vault.session()?.vault;
        if vault.profiles.is_empty() {
            return None;
        }
        let mut seg = segmented("pair-mesh-seg", pal).child(self.seg_btn(
            "pair-mesh-none",
            "不附带",
            self.pair.mesh.is_none(),
            pal,
            cx,
            |t| t.set_pair_mesh(None),
        ));
        for (i, p) in vault.profiles.iter().enumerate() {
            let id = p.id.clone();
            seg = seg.child(self.seg_btn(
                ("pair-mesh", i),
                clip(&p.name, 8),
                self.pair.mesh.as_deref() == Some(p.id.as_str()),
                pal,
                cx,
                move |t| t.set_pair_mesh(Some(id.clone())),
            ));
        }
        Some(
            div()
                .debug_selector(|| "pair-mesh".to_string())
                .flex()
                .flex_col()
                .gap(px(6.0))
                .child(self.small_label("新设备自动加入组网", pal))
                .child(seg)
                .into_any_element(),
        )
    }

    /// Centred modal over a dimmed stage; the header stays usable.
    pub(crate) fn pair_modal(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let muted = rgba(pal.muted, 1.0);
        let body = self.pair_body(pal, cx);
        let card = card_shell(pal, CARD_W)
            .id("pair-modal")
            .debug_selector(|| "pair-modal".to_string())
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| {
                cx.stop_propagation()
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        txt("添加设备", 18.0, rgba(pal.ink, 1.0)).font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        div()
                            .id("pair-close")
                            .px(px(8.0))
                            .h(px(ui::MINI_H))
                            .flex()
                            .items_center()
                            .rounded(px(ui::RAD_INNER))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                            .child(mtxt("Esc", 10.0, muted))
                            .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                this.close_pair_modal();
                                cx.notify();
                            })),
                    ),
            )
            .child(txt(
                "在新电脑上运行下面的命令（需已安装 skylatch-agent）",
                12.0,
                muted,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().flex_none().child(self.small_label("加入房间", pal)))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(self.pair_room_input(pal, cx)),
                    ),
            )
            .children(self.pair_mesh_row(pal, cx))
            .children(body)
            .children(self.pair.invite_suffix().is_some().then(|| {
                crate::hub_view::para(
                    "短码后缀只在新设备本地解密组网密钥，Hub 看不到它，也就拿不到组网凭据",
                    11.0,
                    muted,
                )
            }))
            .child(hint_row(
                &[
                    ("Enter", "复制命令"),
                    (crate::CODE_KEY, "换一码"),
                    (crate::PASTE_KEY, "粘贴"),
                    ("Esc", "关闭"),
                ],
                pal,
            ));

        div()
            .id("pair-scrim")
            .debug_selector(|| "pair-scrim".to_string())
            .absolute()
            .top(px(HEADER_H))
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
                    this.close_pair_modal();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .child(card)
            .into_any_element()
    }
}

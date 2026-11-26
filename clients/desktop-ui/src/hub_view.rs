// SPDX-License-Identifier: Apache-2.0

//! The 连接与组网 drawer: its frame, the 服务器与账号 | 虚拟组网 switch and the server tab,
//! plus the small controls the mesh tab and its dialogs share (buttons, checkboxes, text
//! boxes). State lives in [`crate::hub_ui`] and [`crate::mesh_ui`].

use crate::hub_ui::{self, AccountMode};
use crate::mesh_ui::{DrawerTab, TabDot};
use crate::theme::{Pal, ui};
use crate::{Field, HubState, Proto, Target, clip, config, hint_row, mtxt, rgba, segmented, txt};
use gpui::prelude::*;
use gpui::{AnyElement, ClickEvent, Context, ElementId, FontWeight, Hsla, SharedString, div, px};

pub(crate) const DRAWER_W: f32 = 480.0;
/// From this window width on the drawer takes its own column and the desk shrinks to the rest;
/// narrower windows get it as an overlay on the right edge.
const DOCK_MIN_W: f32 = 1040.0;
/// Left and right padding inside the drawer.
const DRAWER_PAD: f32 = 20.0;
/// Row height of a multi-line text box; boxes are sized to whole rows plus their padding.
const TEXT_LINE_H: f32 = 16.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tone {
    Primary,
    Plain,
    Danger,
}

/// Text that wraps inside its column.
pub(crate) fn para(s: impl Into<SharedString>, size: f32, color: Hsla) -> gpui::Div {
    txt(s, size, color).whitespace_normal()
}

/// A status dot; `hollow` for states that are not settled.
pub(crate) fn dot(color: Hsla, hollow: bool) -> gpui::Div {
    let d = div().size(px(6.0)).flex_none().rounded_full();
    if hollow {
        d.border_1().border_color(color)
    } else {
        d.bg(color)
    }
}

/// A small tag: text on a light wash of its colour.
pub(crate) fn tag(text: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .h(px(ui::TAG_H))
        .px(px(7.0))
        .flex()
        .flex_none()
        .items_center()
        .rounded(px(ui::RAD_INNER))
        .bg(color.opacity(0.12))
        .child(txt(text, 10.5, color).font_weight(FontWeight::MEDIUM))
}

/// A card inside the drawer.
pub(crate) fn card(pal: &Pal) -> gpui::Div {
    div()
        .w_full()
        .p(px(12.0))
        .flex()
        .flex_col()
        .gap(px(8.0))
        .rounded(px(ui::RAD_CTL))
        .border_1()
        .border_color(rgba(pal.hair, 1.0))
        .bg(rgba(pal.surface, 1.0))
}

/// A section title of the drawer body.
pub(crate) fn section(title: &'static str, pal: &Pal) -> gpui::Div {
    div()
        .pt(px(4.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .child(txt(title, 12.0, rgba(pal.ink, 1.0)).font_weight(FontWeight::SEMIBOLD))
}

pub(crate) fn label(text: impl Into<SharedString>, pal: &Pal) -> gpui::Div {
    txt(text, 11.0, rgba(pal.muted, 1.0))
}

pub(crate) fn tab_dot_color(dot: TabDot, pal: &Pal) -> (Hsla, bool) {
    match dot {
        TabDot::Unknown => (rgba(pal.muted, 0.8), true),
        TabDot::Connected => (rgba(pal.green, 1.0), false),
        TabDot::External => (rgba(pal.blue, 1.0), false),
        TabDot::Missing => (rgba(pal.muted, 0.9), true),
        TabDot::Warning => (rgba(pal.warn, 1.0), false),
    }
}

impl Proto {
    /// The drawer's area in window px: the full height below the header, on the right edge.
    pub(crate) fn drawer_rect(&self) -> crate::iso::Rect2 {
        let y = crate::scene::HEADER_H;
        crate::iso::Rect2 {
            x: self.vp.0 - DRAWER_W,
            y,
            w: DRAWER_W,
            h: (self.vp.1 - y).max(240.0),
        }
    }

    /// Whether the open drawer sits beside the desk instead of over it.
    pub(crate) fn drawer_docked(&self) -> bool {
        self.drawer.open && self.vp.0 >= DOCK_MIN_W
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn btn(
        &self,
        id: impl Into<ElementId>,
        selector: String,
        label: impl Into<SharedString>,
        tone: Tone,
        enabled: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Proto, &mut Context<Proto>) + 'static,
    ) -> AnyElement {
        let (bg, fg, border, hover) = match tone {
            Tone::Primary => (
                rgba(pal.accent, 1.0),
                rgba(pal.on_accent, 1.0),
                rgba(pal.accent, 1.0),
                rgba(pal.accent.mix(pal.ink, 0.18), 1.0),
            ),
            Tone::Plain => (
                rgba(pal.surface, 1.0),
                rgba(pal.ink, 1.0),
                rgba(pal.hair, 1.0),
                rgba(pal.wash, 1.0),
            ),
            Tone::Danger => (
                rgba(pal.warn, 0.08),
                rgba(pal.warn, 1.0),
                rgba(pal.warn, 0.45),
                rgba(pal.warn, 0.16),
            ),
        };
        let label: SharedString = label.into();
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .h(px(ui::FIELD_H))
            .px(px(14.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(px(ui::RAD_FIELD))
            .border_1()
            .border_color(border)
            .bg(bg)
            .opacity(if enabled { 1.0 } else { 0.45 })
            .when(enabled, |d| d.cursor_pointer().hover(move |s| s.bg(hover)))
            .child(txt(label, 12.5, fg).font_weight(if tone == Tone::Primary {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::NORMAL
            }))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                if enabled {
                    on(this, cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// A checkbox with its label; the whole row toggles.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check(
        &self,
        id: impl Into<ElementId>,
        selector: String,
        checked: bool,
        label: impl Into<SharedString>,
        enabled: bool,
        pal: &Pal,
        cx: &mut Context<Self>,
        on: impl Fn(&mut Proto, &mut Context<Proto>) + 'static,
    ) -> AnyElement {
        let accent = rgba(pal.accent, 1.0);
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .opacity(if enabled { 1.0 } else { 0.5 })
            .when(enabled, |d| d.cursor_pointer())
            .child(
                div()
                    .size(px(15.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(ui::RAD_KBD))
                    .border_1()
                    .border_color(if checked {
                        accent
                    } else {
                        rgba(pal.muted, 0.8)
                    })
                    .bg(if checked {
                        accent
                    } else {
                        rgba(pal.canvas, 1.0)
                    })
                    .when(checked, |d| {
                        d.child(txt("✓", 10.0, rgba(pal.on_accent, 1.0)))
                    }),
            )
            .child(txt(label, 12.0, rgba(pal.ink, 1.0)))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                if enabled {
                    on(this, cx);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    /// A one-line text box that hands its text to the input method while it is in use.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text_box(
        &self,
        id: impl Into<ElementId>,
        selector: String,
        target: Target,
        shown: String,
        placeholder: &'static str,
        pal: &Pal,
        cx: &mut Context<Self>,
        on_focus: impl Fn(&mut Proto) + 'static,
    ) -> AnyElement {
        let focused = self.active_target() == Some(target);
        let preedit = self.preedit(target, Self::is_secret(target), 12.0, true, pal);
        let empty = shown.is_empty() && preedit.is_none();
        div()
            .id(id)
            .debug_selector(move || selector.clone())
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
                    .when(focused, |d| d.justify_end())
                    .child(if empty {
                        mtxt(placeholder, 12.0, rgba(pal.muted, 0.7))
                    } else {
                        mtxt(shown, 12.0, rgba(pal.ink, 1.0))
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
            .child(self.ime_anchor(target, cx))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                this.marked = None;
                on_focus(this);
                cx.notify();
            }))
            .into_any_element()
    }

    /// A multi-line text box (peer lists, pasted TOML); Enter starts a new line.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn text_area(
        &self,
        id: &'static str,
        target: Target,
        text: &str,
        placeholder: &'static str,
        height: f32,
        pal: &Pal,
        cx: &mut Context<Self>,
        on_focus: impl Fn(&mut Proto) + 'static,
    ) -> AnyElement {
        let focused = self.active_target() == Some(target);
        let preedit = self.preedit(target, false, 11.5, true, pal);
        let ink = rgba(pal.ink, 1.0);
        let caret = rgba(pal.accent, 1.0);
        let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
        if text.is_empty() {
            lines.clear();
        }
        let last = lines.len().saturating_sub(1);
        let empty = text.is_empty() && preedit.is_none();
        let mut rows: Vec<AnyElement> = Vec::new();
        if empty {
            rows.push(
                div()
                    .relative()
                    .child(para(placeholder, 11.5, rgba(pal.muted, 0.7)))
                    .when(focused, |d| {
                        d.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .w(px(1.5))
                                .h(px(15.0))
                                .bg(caret),
                        )
                    })
                    .into_any_element(),
            );
        } else {
            let mut preedit = preedit;
            for (i, line) in lines.iter().enumerate() {
                let tail = i == last;
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .h(px(TEXT_LINE_H))
                        .child(mtxt(line.clone(), 11.5, ink))
                        .when(tail, |d| d.children(preedit.take()))
                        .when(tail && focused, |d| {
                            d.child(div().w(px(1.5)).h(px(14.0)).ml(px(1.0)).bg(caret))
                        })
                        .into_any_element(),
                );
            }
            if lines.is_empty() {
                rows.push(div().flex().children(preedit).into_any_element());
            }
        }
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .relative()
            .h(px(height))
            .w_full()
            .px(px(10.0))
            .py(px(7.0))
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
                    .id((id, 0usize))
                    .size_full()
                    .overflow_y_scroll()
                    .child(div().flex().flex_col().children(rows)),
            )
            .child(self.ime_anchor(target, cx))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                this.marked = None;
                on_focus(this);
                cx.notify();
            }))
            .into_any_element()
    }

    /// One segment of the drawer's switch; the 虚拟组网 one carries the mesh state's dot.
    fn drawer_tab(
        &self,
        id: &'static str,
        text: &'static str,
        tab: DrawerTab,
        dot_of: Option<TabDot>,
        pal: &Pal,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.mesh.tab == tab;
        let fg = rgba(if active { pal.ink } else { pal.muted }, 1.0);
        let lift = rgba(pal.raised, 0.5);
        div()
            .id(id)
            .debug_selector(move || id.to_string())
            .h(px(ui::SEG_IN_H))
            .flex_1()
            .px(px(12.0))
            .flex()
            .items_center()
            .justify_center()
            .gap(px(7.0))
            .rounded(px(ui::RAD_INNER))
            .cursor_pointer()
            .when(active, |d| d.bg(rgba(pal.raised, 1.0)).shadow_sm())
            .when(!active, move |d| d.hover(move |s| s.bg(lift)))
            .child(txt(text, 12.5, fg).font_weight(if active {
                FontWeight::MEDIUM
            } else {
                FontWeight::NORMAL
            }))
            .children(dot_of.map(|d| {
                let (c, hollow) = tab_dot_color(d, pal);
                dot(c, hollow)
            }))
            .on_click(cx.listener(move |this, _e: &ClickEvent, _w, cx| {
                this.set_drawer_tab(tab);
                cx.notify();
            }))
            .into_any_element()
    }

    fn hub_info_card(&self, pal: &Pal) -> Option<AnyElement> {
        let url = config::normalize_hub_url(&self.drawer.url);
        if !config::hub_url_valid(&url) {
            return None;
        }
        let muted = rgba(pal.muted, 1.0);
        let ink = rgba(pal.ink, 1.0);
        let body = card(pal).debug_selector(|| "hub-info".to_string());
        let Some(info) = self.hubinfo.for_url(&url) else {
            let text = if self.hubinfo.loading(&url) {
                "正在读取 Hub 信息…".to_string()
            } else if let Some(e) = self
                .hubinfo
                .error
                .as_ref()
                .filter(|_| self.hubinfo.url == url)
            {
                format!("无法读取 Hub 信息：{}", clip(e, 40))
            } else {
                "保存并连接后读取 Hub 的模式与配套打洞节点".to_string()
            };
            return Some(body.child(txt(text, 11.5, muted)).into_any_element());
        };
        let mode_color = if info.mode == "relay" {
            rgba(pal.green, 1.0)
        } else {
            rgba(pal.blue, 1.0)
        };
        let peers: Vec<AnyElement> = if info.easytier_peers.is_empty() {
            vec![txt("Hub 未声明配套打洞节点", 11.5, muted).into_any_element()]
        } else {
            info.easytier_peers
                .iter()
                .map(|p| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(dot(rgba(pal.accent, 0.9), false))
                        .child(mtxt(clip(p, 52), 11.5, ink))
                        .into_any_element()
                })
                .collect()
        };
        let signup = if info.allow_signup {
            tag("开放自助注册", rgba(pal.green, 1.0))
        } else {
            tag("仅限已有账号", muted)
        };
        Some(
            body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(tag(hub_ui::mode_label(&info.mode), mode_color))
                    .child(signup)
                    .child(div().flex_1())
                    .child(mtxt(format!("Hub v{}", info.version), 11.0, muted)),
            )
            .child(txt(hub_ui::mode_detail(&info.mode), 11.5, muted))
            .child(
                div()
                    .pt(px(2.0))
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(label("配套打洞节点（虚拟组网默认叠加）", pal))
                    .children(peers),
            )
            .children(
                info.max_devices_per_account
                    .map(|n| txt(format!("每个账号最多 {n} 台设备"), 11.0, muted)),
            )
            .into_any_element(),
        )
    }

    fn server_tab(&self, pal: &Pal, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let muted = rgba(pal.muted, 1.0);
        let accent = rgba(pal.accent, 1.0);
        let d = &self.drawer;
        let url = config::normalize_hub_url(&d.url);
        let allow_signup = self.hubinfo.for_url(&url).is_some_and(|i| i.allow_signup);
        let mode = d.account;
        let mut out: Vec<AnyElement> = vec![
            label("Hub 地址", pal).into_any_element(),
            self.text_box(
                "field-url",
                "field-url".into(),
                Target::Drawer(Field::Url),
                d.url.clone(),
                "wss://hub.example.com",
                pal,
                cx,
                |t| t.drawer.field = Field::Url,
            ),
        ];
        out.extend(self.hub_info_card(pal));

        let mut seg = segmented("account-seg", pal).child(self.seg_btn(
            "acct-login",
            "登录",
            mode == AccountMode::Login,
            pal,
            cx,
            |t| t.set_account_mode(AccountMode::Login),
        ));
        if allow_signup || mode == AccountMode::Signup {
            seg = seg.child(self.seg_btn(
                "acct-signup",
                "注册",
                mode == AccountMode::Signup,
                pal,
                cx,
                |t| t.set_account_mode(AccountMode::Signup),
            ));
        }
        seg = seg.child(self.seg_btn(
            "acct-token",
            "Token",
            mode == AccountMode::Token,
            pal,
            cx,
            |t| t.set_account_mode(AccountMode::Token),
        ));
        out.push(
            div()
                .pt(px(2.0))
                .flex()
                .items_center()
                .justify_between()
                .child(txt("账号", 12.0, rgba(pal.ink, 1.0)).font_weight(FontWeight::SEMIBOLD))
                .child(seg)
                .into_any_element(),
        );
        match mode {
            AccountMode::Login | AccountMode::Signup => {
                out.push(label("用户名", pal).into_any_element());
                out.push(self.text_box(
                    "field-user",
                    "field-user".into(),
                    Target::Drawer(Field::User),
                    d.user.clone(),
                    "用户名",
                    pal,
                    cx,
                    |t| t.drawer.field = Field::User,
                ));
                out.push(
                    label(
                        if mode == AccountMode::Signup {
                            "密码（至少 8 位，同时用于加密云端组网配置）"
                        } else {
                            "密码"
                        },
                        pal,
                    )
                    .into_any_element(),
                );
                out.push(self.text_box(
                    "field-pass",
                    "field-pass".into(),
                    Target::Drawer(Field::Pass),
                    crate::onboard_view::mask(&d.pass),
                    "密码",
                    pal,
                    cx,
                    |t| t.drawer.field = Field::Pass,
                ));
            }
            AccountMode::Token => {
                let token_text = if d.show_token {
                    d.token.clone()
                } else {
                    crate::onboard_view::mask(&d.token)
                };
                out.push(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(label("Token", pal))
                        .child(
                            div()
                                .id("token-eye")
                                .cursor_pointer()
                                .child(txt(
                                    if d.show_token { "隐藏" } else { "显示" },
                                    11.0,
                                    accent,
                                ))
                                .on_click(cx.listener(|this, _e: &ClickEvent, _w, cx| {
                                    this.drawer.show_token = !this.drawer.show_token;
                                    cx.notify();
                                })),
                        )
                        .into_any_element(),
                );
                out.push(self.text_box(
                    "field-token",
                    "field-token".into(),
                    Target::Drawer(Field::Token),
                    token_text,
                    crate::TOKEN_PLACEHOLDER,
                    pal,
                    cx,
                    |t| t.drawer.field = Field::Token,
                ));
            }
        }

        out.push(
            div()
                .pt(px(2.0))
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(self.check(
                    "prefer-direct",
                    "prefer-direct".into(),
                    d.prefer_direct,
                    "优先直连（取消勾选则只走 Hub 中继）",
                    true,
                    pal,
                    cx,
                    |t, _| t.drawer.prefer_direct = !t.drawer.prefer_direct,
                ))
                .children(self.host.is_some().then(|| {
                    self.check(
                        "drawer-host",
                        "drawer-host".into(),
                        self.stored.host_enabled,
                        "共享本机（允许其他设备接管这台电脑）",
                        true,
                        pal,
                        cx,
                        |t, _| t.set_host_enabled(!t.stored.host_enabled),
                    )
                }))
                .into_any_element(),
        );

        let status = match (&d.error, &self.hub, d.busy) {
            (Some(e), _, _) => (e.clone(), rgba(pal.warn, 1.0)),
            (None, _, true) => (
                if mode == AccountMode::Signup {
                    "正在注册并解锁云端组网配置…"
                } else {
                    "正在登录并解锁云端组网配置…"
                }
                .to_string(),
                rgba(pal.blue, 1.0),
            ),
            (None, HubState::Demo, _) => {
                ("当前为演示模式，填写后即可连接真实 Hub".to_string(), muted)
            }
            (None, HubState::Unconfigured, _) => (String::new(), muted),
            (None, HubState::Connecting, _) => ("正在连接…".to_string(), rgba(pal.blue, 1.0)),
            (None, HubState::Online { count }, _) => (
                match &self.mesh_user {
                    Some(u) => format!("已以 {u} 登录，{count} 台设备在线"),
                    None => format!("已连接，{count} 台设备在线"),
                },
                rgba(pal.green, 1.0),
            ),
            (None, HubState::Failed(m), _) => (m.clone(), rgba(pal.warn, 1.0)),
        };
        out.push(
            div()
                .h(px(18.0))
                .overflow_hidden()
                .child(txt(clip(&status.0, 60), 11.5, status.1))
                .into_any_element(),
        );
        let submit = match (mode, d.busy) {
            (_, true) => "请稍候…",
            (AccountMode::Login, _) => "登录并连接",
            (AccountMode::Signup, _) => "注册并连接",
            (AccountMode::Token, _) => "保存并连接",
        };
        let signed_in = !self.eff.hub_url.is_empty() || !self.eff.token.is_empty();
        out.push(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(self.btn(
                    "drawer-save",
                    "drawer-save".into(),
                    submit,
                    Tone::Primary,
                    !d.busy,
                    pal,
                    cx,
                    |t, cx| t.save_and_connect(cx),
                ))
                .child(self.btn(
                    "drawer-signout",
                    "drawer-signout".into(),
                    "退出账号",
                    Tone::Plain,
                    signed_in || self.eff.demo,
                    pal,
                    cx,
                    |t, _| t.sign_out(),
                ))
                .into_any_element(),
        );
        out.push(
            hint_row(
                &[("Tab", "切换字段"), ("Enter", "保存"), ("Esc", "关闭")],
                pal,
            )
            .into_any_element(),
        );
        out.push(
            para(
                "设置保存在本机用户配置目录；密码只用于登录和解锁云端组网配置，不会保存",
                10.5,
                muted,
            )
            .into_any_element(),
        );
        out.push(self.version_row(pal, cx));
        out
    }

    pub(crate) fn drawer_view(&self, pal: &Pal, cx: &mut Context<Self>) -> AnyElement {
        let hair = rgba(pal.hair, 1.0);
        let muted = rgba(pal.muted, 1.0);
        let body: Vec<AnyElement> = match self.mesh.tab {
            DrawerTab::Server => self.server_tab(pal, cx),
            DrawerTab::Mesh => self.mesh_tab(pal, cx),
        };
        let edge = gpui::BoxShadow {
            color: gpui::black().opacity(0.16),
            offset: gpui::point(px(-6.0), px(0.0)),
            blur_radius: px(22.0),
            spread_radius: px(0.0),
        };
        div()
            .id("hub-drawer")
            .debug_selector(|| "hub-drawer".to_string())
            .w(px(DRAWER_W))
            .flex_none()
            .flex()
            .flex_col()
            .when(self.drawer_docked(), |d| d.relative().h_full())
            .when(!self.drawer_docked(), |d| {
                d.absolute()
                    .right_0()
                    .top(px(crate::scene::HEADER_H))
                    .bottom_0()
            })
            .bg(rgba(pal.bar, 1.0))
            .border_l_1()
            .border_color(hair)
            .shadow(vec![edge])
            .occlude()
            .child(
                div()
                    .px(px(DRAWER_PAD))
                    .pt(px(18.0))
                    .pb(px(12.0))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .flex_none()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                txt("连接与组网", 14.0, rgba(pal.ink, 1.0))
                                    .font_weight(FontWeight::SEMIBOLD),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(2.0))
                                    .children(self.eff.live().then(|| {
                                        div()
                                            .id("hub-refresh")
                                            .debug_selector(|| "hub-refresh".to_string())
                                            .size(px(ui::MINI_H))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .rounded(px(ui::RAD_INNER))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                                            .child(txt("↻", 13.0, muted))
                                            .on_click(cx.listener(
                                                |this, _e: &ClickEvent, _w, cx| {
                                                    this.refresh();
                                                    cx.notify();
                                                },
                                            ))
                                    }))
                                    .child(
                                        div()
                                            .id("drawer-close")
                                            .debug_selector(|| "drawer-close".to_string())
                                            .px(px(8.0))
                                            .h(px(ui::MINI_H))
                                            .flex()
                                            .items_center()
                                            .rounded(px(ui::RAD_INNER))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(rgba(pal.wash, 1.0)))
                                            .child(mtxt("Esc", 10.0, muted))
                                            .on_click(cx.listener(
                                                |this, _e: &ClickEvent, _w, cx| {
                                                    this.close_drawer();
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        segmented("drawer-tabs", pal)
                            .w_full()
                            .child(self.drawer_tab(
                                "tab-server",
                                "服务器与账号",
                                DrawerTab::Server,
                                None,
                                pal,
                                cx,
                            ))
                            .child(self.drawer_tab(
                                "tab-mesh",
                                "虚拟组网",
                                DrawerTab::Mesh,
                                Some(self.mesh.dot()),
                                pal,
                                cx,
                            )),
                    ),
            )
            .child(
                div()
                    .id("drawer-body")
                    .debug_selector(|| "drawer-body".to_string())
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .px(px(DRAWER_PAD))
                    .pb(px(20.0))
                    .child(
                        div()
                            .w(px(DRAWER_W - 2.0 * DRAWER_PAD - 1.0))
                            .flex()
                            .flex_col()
                            .gap(px(9.0))
                            .children(body),
                    ),
            )
            .into_any_element()
    }
}

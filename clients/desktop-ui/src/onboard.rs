// SPDX-License-Identifier: Apache-2.0

//! Onboarding and pairing state that does not depend on GPUI: the form behind the empty-desk
//! card (hub sign-in, account token, direct connect) and the one-time pairing code shown when
//! another machine is added.
//!
//! Secrets are handled conservatively: the password only lives in the form until it is
//! submitted (it is wiped at that moment and never written anywhere), and nothing here
//! implements `Debug`.

use crate::config;
use crate::live;
use skylatch_client_core::ClientError;
use skylatch_proto::credentials::is_token;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

pub const MAX_URL_LEN: usize = 256;
pub const MAX_NAME_LEN: usize = 128;
pub const MAX_SECRET_LEN: usize = 512;
/// Longest room name typed into the launcher.
pub const MAX_ROOM_LEN: usize = 40;

/// Characters that may appear in a room name that ends up inside a quoted shell command:
/// control characters and the ones double quotes do not protect are left out.
pub fn room_char_ok(c: char) -> bool {
    !c.is_control() && !matches!(c, '"' | '`' | '$' | '\\' | '!')
}

/// A room name as the agent receives it: trimmed, cleaned and clipped; `None` when blank.
pub fn clean_room(text: &str) -> Option<String> {
    let room: String = text
        .chars()
        .filter(|c| room_char_ok(*c))
        .collect::<String>()
        .trim()
        .chars()
        .take(MAX_ROOM_LEN)
        .collect();
    let room = room.trim().to_string();
    (!room.is_empty()).then_some(room)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Hub,
    Direct,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnbField {
    HubUrl,
    Username,
    Password,
    Token,
    DirectUrl,
    DirectToken,
    /// Hub tab, while hosting this machine is on: the room it should join.
    HostRoom,
}

/// What the Hub tab asks the app to do.
#[derive(PartialEq, Eq)]
pub enum HubSubmit {
    /// Sign in with a password; only the returned session token is kept.
    Login {
        url: String,
        username: String,
        password: String,
    },
    /// Use an existing account token as is.
    Token { url: String, token: String },
}

/// Result of a background sign-in: the session token on success.
pub type LoginOutcome = Result<String, String>;

pub struct Onboarding {
    pub tab: Tab,
    /// Hub tab: paste an account token instead of signing in with a password.
    pub token_mode: bool,
    pub field: OnbField,
    pub hub_url: String,
    pub username: String,
    pub password: String,
    pub token: String,
    pub direct_url: String,
    pub direct_token: String,
    /// Hub tab: also let other devices take over this machine (default on).
    pub host_enabled: bool,
    /// Hub tab: room this machine joins (optional; blank keeps the hub's default).
    pub host_room: String,
    pub busy: bool,
    pub error: Option<String>,
    /// Pending background sign-in, with the hub URL it was started for.
    pub login: Option<(String, Receiver<LoginOutcome>)>,
}

impl Onboarding {
    pub fn new() -> Onboarding {
        Onboarding {
            tab: Tab::Hub,
            token_mode: false,
            field: OnbField::HubUrl,
            hub_url: String::new(),
            username: String::new(),
            password: String::new(),
            token: String::new(),
            direct_url: String::new(),
            direct_token: String::new(),
            host_enabled: true,
            host_room: String::new(),
            busy: false,
            error: None,
            login: None,
        }
    }

    /// Fields of the visible form, in Tab order.
    pub fn fields(&self) -> &'static [OnbField] {
        match (self.tab, self.token_mode, self.host_enabled) {
            (Tab::Hub, false, false) => &[OnbField::HubUrl, OnbField::Username, OnbField::Password],
            (Tab::Hub, false, true) => &[
                OnbField::HubUrl,
                OnbField::Username,
                OnbField::Password,
                OnbField::HostRoom,
            ],
            (Tab::Hub, true, false) => &[OnbField::HubUrl, OnbField::Token],
            (Tab::Hub, true, true) => &[OnbField::HubUrl, OnbField::Token, OnbField::HostRoom],
            (Tab::Direct, _, _) => &[OnbField::DirectUrl, OnbField::DirectToken],
        }
    }

    pub fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.field = self.fields()[0];
        self.error = None;
    }

    pub fn set_token_mode(&mut self, on: bool) {
        self.token_mode = on;
        self.field = if on {
            OnbField::Token
        } else {
            OnbField::Username
        };
        self.error = None;
    }

    pub fn move_focus(&mut self, dir: i32) {
        let list = self.fields();
        let at = list.iter().position(|f| *f == self.field).unwrap_or(0) as i32;
        self.field = list[(at + dir).rem_euclid(list.len() as i32) as usize];
    }

    /// Switches "let others take over this machine"; the room field goes with it.
    pub fn set_host_enabled(&mut self, on: bool) {
        self.host_enabled = on;
        if !self.fields().contains(&self.field) {
            self.field = self.fields()[0];
        }
    }

    /// The room to enroll this machine in: only while hosting is on, and only when typed.
    pub fn host_room_value(&self) -> Option<String> {
        if self.host_enabled {
            clean_room(&self.host_room)
        } else {
            None
        }
    }

    /// The text of one field.
    pub fn text_of(&self, field: OnbField) -> &str {
        match field {
            OnbField::HubUrl => &self.hub_url,
            OnbField::Username => &self.username,
            OnbField::Password => &self.password,
            OnbField::Token => &self.token,
            OnbField::DirectUrl => &self.direct_url,
            OnbField::DirectToken => &self.direct_token,
            OnbField::HostRoom => &self.host_room,
        }
    }

    /// The text of the focused field with its length limit.
    pub fn buf(&mut self) -> (&mut String, usize) {
        match self.field {
            OnbField::HubUrl => (&mut self.hub_url, MAX_URL_LEN),
            OnbField::Username => (&mut self.username, MAX_NAME_LEN),
            OnbField::Password => (&mut self.password, MAX_SECRET_LEN),
            OnbField::Token => (&mut self.token, MAX_SECRET_LEN),
            OnbField::DirectUrl => (&mut self.direct_url, MAX_URL_LEN),
            OnbField::DirectToken => (&mut self.direct_token, MAX_SECRET_LEN),
            OnbField::HostRoom => (&mut self.host_room, MAX_ROOM_LEN),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn type_text(&mut self, text: &str) {
        let (buf, max) = self.buf();
        for c in text.chars().filter(|c| !c.is_control()) {
            if buf.chars().count() < max {
                buf.push(c);
            }
        }
        self.error = None;
    }

    pub fn backspace(&mut self) {
        self.buf().0.pop();
        self.error = None;
    }

    /// Overwrites the password before it is dropped.
    pub fn wipe_password(&mut self) {
        let len = self.password.len();
        self.password.clear();
        self.password.extend(std::iter::repeat_n('\0', len));
        self.password.clear();
    }

    /// Validates the Hub tab.
    pub fn hub_submit(&self) -> Result<HubSubmit, String> {
        let url = config::normalize_hub_url(&self.hub_url);
        if !config::hub_url_valid(&url) {
            return Err("请填写有效的 Hub 地址，例如 https://hub.example.com".into());
        }
        if self.token_mode {
            let token = self.token.trim().to_string();
            if token.is_empty() {
                return Err("请粘贴账户 Token".into());
            }
            return Ok(HubSubmit::Token { url, token });
        }
        let username = self.username.trim().to_string();
        if username.is_empty() {
            return Err("请填写用户名".into());
        }
        if self.password.is_empty() {
            return Err("请填写密码".into());
        }
        Ok(HubSubmit::Login {
            url,
            username,
            password: self.password.clone(),
        })
    }

    /// Validates the Direct tab and returns `(url, optional token)`.
    pub fn direct_target(&self) -> Result<(String, Option<String>), String> {
        let url = self.direct_url.trim().trim_end_matches('/').to_string();
        if !(url.starts_with("ws://") || url.starts_with("wss://")) || !config::hub_url_valid(&url)
        {
            return Err("请填写以 ws:// 或 wss:// 开头的主机地址".into());
        }
        let token = self.direct_token.trim();
        if !token.is_empty() && !is_token(token) {
            return Err("直连 Token 应为 64 位十六进制字符".into());
        }
        Ok((url, (!token.is_empty()).then(|| token.to_string())))
    }
}

/// Arguments of the native client for a direct connection.
pub fn direct_args(url: &str, token: Option<&str>) -> Vec<String> {
    let mut args = vec!["--direct".to_string(), url.to_string()];
    if let Some(t) = token {
        args.push("--token".into());
        args.push(t.to_string());
    }
    args
}

/// One-line explanation of a failed sign-in.
pub fn describe_login_error(e: &ClientError) -> String {
    match e {
        ClientError::Hub { code, .. } if code == "401" => "用户名或密码不正确".into(),
        ClientError::Hub { code, .. } if code == "429" => "尝试过于频繁，请稍后再试".into(),
        other => live::describe_error(other),
    }
}

// ───────────────────────────── pairing ─────────────────────────────

/// A code minted by the hub.
#[derive(Clone, PartialEq, Eq)]
pub struct Code {
    pub code: String,
    pub expires: Instant,
}

impl Code {
    pub fn new(code: String, lifetime_secs: u64) -> Code {
        Code {
            code,
            expires: Instant::now() + Duration::from_secs(lifetime_secs),
        }
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }

    /// Whole minutes left, rounded up.
    pub fn minutes_left(&self) -> u64 {
        self.expires
            .saturating_duration_since(Instant::now())
            .as_secs()
            .div_ceil(60)
    }
}

pub type PairOutcome = Result<(String, u64), String>;

/// State of "add a device": the modal and the first-device card share it.
pub struct Pairing {
    pub open: bool,
    pub code: Option<Code>,
    pub busy: bool,
    pub error: Option<String>,
    pub copied_at: Option<Instant>,
    /// Bumped for every request and on sign-out; late answers of older requests are dropped.
    pub generation: u64,
    pub pending: Option<(u64, Receiver<PairOutcome>)>,
    /// (minutes left, expired) as last painted; the pump repaints when it changes.
    pub shown: (u64, bool),
    /// Room the joining machine should land in, as typed in the modal (blank: none).
    pub room: String,
}

impl Pairing {
    pub fn new() -> Pairing {
        Pairing {
            open: false,
            code: None,
            busy: false,
            error: None,
            copied_at: None,
            generation: 0,
            pending: None,
            shown: (u64::MAX, false),
            room: String::new(),
        }
    }

    /// Forgets everything (sign-out, hub change).
    pub fn reset(&mut self) {
        let generation = self.generation + 1;
        *self = Pairing::new();
        self.generation = generation;
    }
}

/// The command to run on the machine that should join; a non-blank `room` is appended as
/// `--room "<room>"`.
pub fn join_command(hub_url: &str, code: &str, room: Option<&str>) -> String {
    let base = format!("skylatch-agent join {hub_url} --code {code}");
    match room.and_then(clean_room) {
        Some(room) => format!("{base} --room \"{room}\""),
        None => base,
    }
}

/// One-line explanation of a failed pairing-code request.
pub fn describe_pairing_error(e: &ClientError) -> String {
    match e {
        ClientError::Hub { code, .. } if code == "401" => {
            "登录已失效，请重新登录或检查 Token".into()
        }
        ClientError::Hub { code, .. } if code == "429" => "请求过于频繁，请稍后再换一码".into(),
        other => live::describe_error(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosting_this_machine_is_on_by_default() {
        let o = Onboarding::new();
        assert!(o.host_enabled);
    }

    fn filled() -> Onboarding {
        let mut o = Onboarding::new();
        o.hub_url = "https://hub.example.com/".into();
        o.username = " alice ".into();
        o.password = "pw".into();
        o
    }

    #[test]
    fn hub_login_is_validated_and_normalised() {
        let o = filled();
        let HubSubmit::Login {
            url,
            username,
            password,
        } = o.hub_submit().unwrap()
        else {
            panic!("login expected")
        };
        assert_eq!(
            (url.as_str(), username.as_str(), password.as_str()),
            ("wss://hub.example.com", "alice", "pw")
        );
        let mut bad = filled();
        bad.hub_url.clear();
        assert!(bad.hub_submit().err().unwrap().contains("Hub 地址"));
        let mut bad = filled();
        bad.username = "  ".into();
        assert!(bad.hub_submit().err().unwrap().contains("用户名"));
        let mut bad = filled();
        bad.password.clear();
        assert!(bad.hub_submit().err().unwrap().contains("密码"));
    }

    #[test]
    fn account_token_mode_skips_the_password() {
        let mut o = filled();
        o.password.clear();
        o.set_token_mode(true);
        assert_eq!(o.field, OnbField::Token);
        assert!(o.hub_submit().err().unwrap().contains("Token"));
        o.token = "  abc  ".into();
        assert!(matches!(
            o.hub_submit().unwrap(),
            HubSubmit::Token { token, .. } if token == "abc"
        ));
        o.set_token_mode(false);
        assert_eq!(o.field, OnbField::Username);
    }

    #[test]
    fn direct_target_needs_a_websocket_url_and_a_hex_token() {
        let mut o = Onboarding::new();
        o.set_tab(Tab::Direct);
        assert_eq!(o.field, OnbField::DirectUrl);
        o.direct_url = "host.example.com:7001".into();
        assert!(o.direct_target().unwrap_err().contains("ws://"));
        o.direct_url = "wss://".into();
        assert!(o.direct_target().is_err());
        o.direct_url = " ws://host.example.com:7001/ ".into();
        assert_eq!(
            o.direct_target().unwrap(),
            ("ws://host.example.com:7001".to_string(), None)
        );
        o.direct_token = "xyz".into();
        assert!(o.direct_target().unwrap_err().contains("64"));
        let tok = "0123456789abcdef".repeat(4);
        o.direct_token = tok.clone();
        assert_eq!(o.direct_target().unwrap().1, Some(tok.clone()));
        assert_eq!(
            direct_args("ws://h:1", Some(&tok)),
            ["--direct", "ws://h:1", "--token", tok.as_str()]
        );
        assert_eq!(direct_args("ws://h:1", None), ["--direct", "ws://h:1"]);
    }

    #[test]
    fn typing_follows_the_focused_field() {
        let mut o = Onboarding::new();
        o.type_text("a\nb");
        assert_eq!(o.hub_url, "ab", "control characters are dropped");
        o.move_focus(1);
        o.type_text("bob");
        o.move_focus(1);
        o.type_text("pw!");
        o.backspace();
        assert_eq!((o.username.as_str(), o.password.as_str()), ("bob", "pw"));
        o.move_focus(1);
        assert_eq!(o.field, OnbField::HostRoom, "the room field comes last");
        o.type_text("办公室");
        assert_eq!(o.host_room, "办公室");
        o.move_focus(1);
        assert_eq!(o.field, OnbField::HubUrl, "focus wraps around");
        o.move_focus(-1);
        o.move_focus(-1);
        assert_eq!(o.field, OnbField::Password);
        o.wipe_password();
        assert!(o.password.is_empty());
        o.set_tab(Tab::Direct);
        o.type_text("ws://x");
        assert_eq!(o.direct_url, "ws://x");
    }

    #[test]
    fn join_command_and_errors_read_well() {
        assert_eq!(
            join_command("wss://hub.example.com", "482-917", None),
            "skylatch-agent join wss://hub.example.com --code 482-917"
        );
        assert_eq!(
            join_command("wss://hub.example.com", "482-917", Some("  ")),
            "skylatch-agent join wss://hub.example.com --code 482-917",
            "a blank room adds nothing"
        );
        assert_eq!(
            join_command("wss://hub.example.com", "482-917", Some(" 办公室 ")),
            "skylatch-agent join wss://hub.example.com --code 482-917 --room \"办公室\""
        );
        assert_eq!(
            join_command("wss://h", "1", Some("a\"$(x)`")),
            "skylatch-agent join wss://h --code 1 --room \"a(x)\"",
            "characters a quote does not protect are dropped"
        );
        let unauthorized = ClientError::Hub {
            code: "401".into(),
            message: "invalid or expired credentials".into(),
        };
        assert!(describe_login_error(&unauthorized).contains("密码"));
        assert!(describe_pairing_error(&unauthorized).contains("登录"));
        let limited = ClientError::Hub {
            code: "429".into(),
            message: String::new(),
        };
        assert!(describe_login_error(&limited).contains("频繁"));
        assert!(describe_login_error(&ClientError::InvalidUrl).contains("地址"));
    }

    #[test]
    fn the_room_field_follows_the_hosting_switch() {
        let mut o = Onboarding::new();
        assert!(o.fields().contains(&OnbField::HostRoom));
        o.host_room = "  客厅工作台 ".into();
        assert_eq!(o.host_room_value().as_deref(), Some("客厅工作台"));
        o.field = OnbField::HostRoom;
        o.set_host_enabled(false);
        assert!(!o.fields().contains(&OnbField::HostRoom));
        assert_eq!(o.field, OnbField::HubUrl, "focus leaves the hidden field");
        assert_eq!(o.host_room_value(), None, "no hosting: no room");
        o.set_host_enabled(true);
        o.host_room = " ".into();
        assert_eq!(o.host_room_value(), None, "blank means the hub default");
        o.host_room = "x".repeat(100);
        assert_eq!(o.host_room_value().unwrap().chars().count(), MAX_ROOM_LEN);
        o.set_token_mode(true);
        assert_eq!(
            o.fields(),
            &[OnbField::HubUrl, OnbField::Token, OnbField::HostRoom]
        );
    }

    #[test]
    fn codes_expire_and_pairing_resets_invalidate_old_requests() {
        let fresh = Code::new("482-917".into(), 600);
        assert!(!fresh.expired());
        assert_eq!(fresh.minutes_left(), 10);
        assert!(Code::new("111-111".into(), 0).expired());
        let mut p = Pairing::new();
        p.generation = 4;
        p.open = true;
        p.code = Some(fresh);
        p.reset();
        assert!(!p.open && p.code.is_none());
        assert_eq!(p.generation, 5);
    }
}

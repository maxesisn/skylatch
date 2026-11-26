// SPDX-License-Identifier: Apache-2.0

//! The 服务器与账号 tab of the 连接与组网 drawer, without GPUI: what `GET /api/v1/info` says
//! about the hub, and the account form (sign in, sign up when the hub allows it, or paste a
//! token).

use crate::config;
use crate::live;
use skylatch_client_core::{ClientError, HubInfo};
use std::sync::mpsc::{Receiver, TryRecvError};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AccountMode {
    Login,
    Signup,
    Token,
}

/// The hub description of one URL, fetched in the background.
pub struct HubInfoSlot {
    /// Normalised URL the answer (or the error) belongs to.
    pub url: String,
    pub info: Option<HubInfo>,
    pub error: Option<String>,
    pub pending: Option<(String, Receiver<Result<HubInfo, String>>)>,
}

impl HubInfoSlot {
    pub fn new() -> HubInfoSlot {
        HubInfoSlot {
            url: String::new(),
            info: None,
            error: None,
            pending: None,
        }
    }

    /// Nothing is known or on its way for `url`.
    pub fn wants(&self, url: &str) -> bool {
        config::hub_url_valid(url)
            && self.url != url
            && self.pending.as_ref().is_none_or(|(u, _)| u != url)
    }

    pub fn start(&mut self, url: String, rx: Receiver<Result<HubInfo, String>>) {
        self.pending = Some((url, rx));
    }

    /// Takes a finished answer; returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        let Some((url, rx)) = &self.pending else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("请求未能完成".to_string()),
        };
        self.url = url.clone();
        self.pending = None;
        match outcome {
            Ok(info) => {
                self.info = Some(info);
                self.error = None;
            }
            Err(e) => {
                self.info = None;
                self.error = Some(e);
            }
        }
        true
    }

    pub fn clear(&mut self) {
        *self = HubInfoSlot::new();
    }

    /// The description of `url`, once it arrived.
    pub fn for_url(&self, url: &str) -> Option<&HubInfo> {
        self.info.as_ref().filter(|_| self.url == url)
    }

    pub fn loading(&self, url: &str) -> bool {
        self.pending.as_ref().is_some_and(|(u, _)| u == url)
    }

    /// The EasyTier peers the hub operates, for the profiles that follow it.
    pub fn peers(&self) -> Vec<String> {
        self.info
            .as_ref()
            .map(|i| i.easytier_peers.clone())
            .unwrap_or_default()
    }
}

/// The hub's transport mode as the drawer shows it.
pub fn mode_label(mode: &str) -> &'static str {
    match mode {
        "relay" => "直连 + 中继",
        "signaling-only" => "纯信令模式",
        _ => "未知模式",
    }
}

/// One-line explanation of the hub's transport mode.
pub fn mode_detail(mode: &str) -> &'static str {
    match mode {
        "relay" => "优先直连，打洞失败时经 Hub 中继画面",
        "signaling-only" => "Hub 只做信令，画面须经直连或虚拟组网",
        _ => "Hub 未声明传输模式",
    }
}

/// The hub's EasyTier peers, as the drawer lists them.
pub fn peers_line(peers: &[String]) -> String {
    if peers.is_empty() {
        "Hub 未声明配套打洞节点".to_string()
    } else {
        format!("配套打洞节点：{}", peers.join("，"))
    }
}

/// What the account form asks the app to do.
pub enum AccountSubmit {
    /// Password sign-in or signup; only the returned session token is kept.
    Credentials {
        url: String,
        username: String,
        password: Zeroizing<String>,
        signup: bool,
    },
    Token {
        url: String,
        token: String,
    },
}

/// Validates the server tab's form.
pub fn account_submit(
    mode: AccountMode,
    url_text: &str,
    username: &str,
    password: &str,
    token: &str,
    allow_signup: bool,
) -> Result<AccountSubmit, String> {
    let url = config::normalize_hub_url(url_text);
    if !config::hub_url_valid(&url) {
        return Err("请填写有效的 Hub 地址，例如 wss://hub.example.com".into());
    }
    if mode == AccountMode::Token {
        let token = token.trim();
        if token.is_empty() {
            return Err("请填写 Hub Token".into());
        }
        return Ok(AccountSubmit::Token {
            url,
            token: token.to_string(),
        });
    }
    if mode == AccountMode::Signup && !allow_signup {
        return Err("该 Hub 未开放自助注册".into());
    }
    let username = username.trim();
    if username.is_empty() {
        return Err("请填写用户名".into());
    }
    if password.is_empty() {
        return Err("请填写密码".into());
    }
    if mode == AccountMode::Signup && password.chars().count() < 8 {
        return Err("注册密码至少 8 位".into());
    }
    Ok(AccountSubmit::Credentials {
        url,
        username: username.to_string(),
        password: Zeroizing::new(password.to_string()),
        signup: mode == AccountMode::Signup,
    })
}

/// One-line explanation of a failed signup.
pub fn describe_signup_error(e: &ClientError) -> String {
    match e {
        ClientError::Hub { code, .. } if code == "409" => "该用户名已被注册".into(),
        ClientError::Hub { code, .. } if code == "403" => "该 Hub 未开放自助注册".into(),
        ClientError::Hub { code, .. } if code == "400" => "用户名或密码不符合 Hub 的要求".into(),
        ClientError::Hub { code, .. } if code == "429" => "尝试过于频繁，请稍后再试".into(),
        other => live::describe_error(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(mode: &str, peers: &[&str], signup: bool) -> HubInfo {
        HubInfo {
            version: "0.1.0".into(),
            mode: mode.into(),
            easytier_peers: peers.iter().map(|p| p.to_string()).collect(),
            allow_signup: signup,
            max_devices_per_account: None,
        }
    }

    #[test]
    fn modes_and_peers_read_as_one_product() {
        assert_eq!(mode_label("relay"), "直连 + 中继");
        assert_eq!(mode_label("signaling-only"), "纯信令模式");
        assert_eq!(mode_label("x"), "未知模式");
        assert_eq!(
            peers_line(&["tcp://et.example.com:11010".into()]),
            "配套打洞节点：tcp://et.example.com:11010"
        );
        assert!(peers_line(&[]).contains("未声明"));
    }

    #[test]
    fn the_slot_keeps_the_answer_of_its_url_only() {
        let mut slot = HubInfoSlot::new();
        assert!(slot.wants("wss://a.example"));
        assert!(!slot.wants("not a url"));
        let (tx, rx) = std::sync::mpsc::channel();
        slot.start("wss://a.example".into(), rx);
        assert!(slot.loading("wss://a.example"));
        assert!(!slot.wants("wss://a.example"));
        assert!(!slot.poll());
        tx.send(Ok(info("relay", &["tcp://p:1"], true))).unwrap();
        assert!(slot.poll());
        assert!(slot.for_url("wss://a.example").unwrap().allow_signup);
        assert!(slot.for_url("wss://b.example").is_none());
        assert_eq!(slot.peers(), vec!["tcp://p:1".to_string()]);
        assert!(slot.wants("wss://b.example"));

        let (tx, rx) = std::sync::mpsc::channel::<Result<HubInfo, String>>();
        slot.start("wss://b.example".into(), rx);
        drop(tx);
        assert!(slot.poll());
        assert!(slot.info.is_none() && slot.error.is_some());
    }

    #[test]
    fn the_account_form_signs_in_signs_up_or_takes_a_token() {
        let Ok(AccountSubmit::Credentials {
            url,
            username,
            password,
            signup,
        }) = account_submit(
            AccountMode::Login,
            "https://hub.example.com/",
            " max ",
            "pw",
            "",
            false,
        )
        else {
            panic!("credentials expected");
        };
        assert_eq!(url, "wss://hub.example.com");
        assert_eq!(username, "max");
        assert_eq!(password.as_str(), "pw");
        assert!(!signup);

        let signup_closed = account_submit(
            AccountMode::Signup,
            "wss://h",
            "max",
            "longpassword",
            "",
            false,
        );
        assert_eq!(signup_closed.err().unwrap(), "该 Hub 未开放自助注册");
        let short = account_submit(AccountMode::Signup, "wss://h", "max", "short", "", true);
        assert!(short.err().unwrap().contains("8 位"));
        assert!(matches!(
            account_submit(
                AccountMode::Signup,
                "wss://h",
                "max",
                "longpassword",
                "",
                true
            ),
            Ok(AccountSubmit::Credentials { signup: true, .. })
        ));
        assert!(matches!(
            account_submit(AccountMode::Token, "wss://h", "", "", " t0k ", false),
            Ok(AccountSubmit::Token { token, .. }) if token == "t0k"
        ));
        assert!(account_submit(AccountMode::Token, "", "", "", "t", false).is_err());
        assert!(account_submit(AccountMode::Login, "wss://h", "", "pw", "", false).is_err());
        assert!(account_submit(AccountMode::Login, "wss://h", "u", "", "", false).is_err());
    }

    #[test]
    fn signup_errors_are_explained() {
        let hub = |code: &str| ClientError::Hub {
            code: code.into(),
            message: String::new(),
        };
        assert_eq!(describe_signup_error(&hub("409")), "该用户名已被注册");
        assert_eq!(describe_signup_error(&hub("403")), "该 Hub 未开放自助注册");
    }
}

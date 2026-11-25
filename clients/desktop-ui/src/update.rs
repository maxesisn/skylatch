// SPDX-License-Identifier: Apache-2.0

//! Self-update as the launcher shows it: a quiet check shortly after start-up, a header capsule
//! that downloads, verifies and hands over to the new build, and the drawer's manual check.
//!
//! [`Updater`] is the state machine; the network and the hand-off run on short-lived worker
//! threads that report back with [`Msg`] over a channel the view drains.

use crate::live::run_async;
use skylatch_client_core::updater::{
    self, ApplyOptions, ApplyOutcome, CheckOptions, DownloadProgress, InstallKind, UpdateError,
    UpdateRelease, VerifiedPackage,
};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

/// How long after start-up the quiet check runs.
pub const AUTO_CHECK_DELAY: Duration = Duration::from_secs(5);

/// `SKYLATCH_DISABLE_AUTO_UPDATE=1` turns the quiet start-up check off.
pub fn auto_check_enabled(env: &dyn Fn(&str) -> Option<String>) -> bool {
    !env("SKYLATCH_DISABLE_AUTO_UPDATE").is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
}

#[derive(Clone, Debug, Default)]
pub enum UpdateUiState {
    #[default]
    Idle,
    Checking,
    Available(UpdateRelease),
    Downloading {
        version: String,
        downloaded: u64,
        total: Option<u64>,
    },
    ReadyToApply(VerifiedPackage),
    ManualInstall {
        version: String,
        command_hint: String,
    },
    Failed {
        version: Option<String>,
        message: String,
    },
}

/// What a worker thread reports.
#[derive(Debug)]
pub enum Msg {
    Checked(Result<Option<UpdateRelease>, String>),
    Progress(DownloadProgress),
    Downloaded(Result<VerifiedPackage, String>),
    Applied(Result<ApplyOutcome, String>),
}

/// The header capsule: hidden while idle or checking.
#[derive(Clone, Debug, PartialEq)]
pub enum Capsule {
    Offer(String),
    Progress { text: String, frac: Option<f32> },
    Restart { text: String, busy: bool },
    Install(String),
    Retry,
}

#[derive(Default)]
pub struct Updater {
    pub state: UpdateUiState,
    /// the release found by the last check, kept for a retry after a failed download
    release: Option<UpdateRelease>,
    /// the last check found nothing newer
    up_to_date: bool,
    /// why the last quiet check failed (shown in the drawer only)
    note: Option<String>,
    /// the check under way was started without a click
    silent: bool,
    /// the hand-off to the new build is under way
    pub applying: bool,
    /// the new build takes over once this process exits
    pub restart: bool,
    rx: Option<Receiver<Msg>>,
}

impl Updater {
    /// Enters `Checking`; `false` when a check makes no sense now (one is under way, or an
    /// update is already in hand).
    pub fn begin_check(&mut self, silent: bool) -> bool {
        if !matches!(
            self.state,
            UpdateUiState::Idle | UpdateUiState::Failed { .. }
        ) {
            return false;
        }
        self.state = UpdateUiState::Checking;
        self.silent = silent;
        self.note = None;
        true
    }

    /// Enters `Downloading` from an offered release (or a failed download of one) and returns it.
    pub fn begin_download(&mut self) -> Option<UpdateRelease> {
        let release = match &self.state {
            UpdateUiState::Available(r) => r.clone(),
            UpdateUiState::Failed { .. } => self.release.clone()?,
            _ => return None,
        };
        self.state = UpdateUiState::Downloading {
            version: release.version.clone(),
            downloaded: 0,
            total: release.package.size,
        };
        Some(release)
    }

    /// The verified package to hand over to, once per click.
    pub fn begin_apply(&mut self) -> Option<VerifiedPackage> {
        match &self.state {
            UpdateUiState::ReadyToApply(p) if !self.applying => {
                self.applying = true;
                Some(p.clone())
            }
            _ => None,
        }
    }

    pub fn on_msg(&mut self, msg: Msg) {
        let version = self.version();
        match msg {
            Msg::Checked(result) => {
                if !matches!(self.state, UpdateUiState::Checking) {
                    return;
                }
                self.state = match result {
                    Ok(Some(release)) => {
                        self.up_to_date = false;
                        self.release = Some(release.clone());
                        UpdateUiState::Available(release)
                    }
                    Ok(None) => {
                        self.up_to_date = true;
                        UpdateUiState::Idle
                    }
                    Err(message) if self.silent => {
                        self.note = Some(message);
                        UpdateUiState::Idle
                    }
                    Err(message) => UpdateUiState::Failed {
                        version: None,
                        message,
                    },
                };
            }
            Msg::Progress(p) => {
                if let UpdateUiState::Downloading {
                    downloaded, total, ..
                } = &mut self.state
                {
                    *downloaded = p.downloaded_bytes;
                    *total = p.total_bytes.or(*total);
                }
            }
            Msg::Downloaded(result) => {
                if !matches!(self.state, UpdateUiState::Downloading { .. }) {
                    return;
                }
                self.state = match result {
                    Ok(package) => UpdateUiState::ReadyToApply(package),
                    Err(message) => UpdateUiState::Failed { version, message },
                };
            }
            Msg::Applied(result) => {
                if !self.applying {
                    return;
                }
                match result {
                    Ok(ApplyOutcome::RestartRequired) => self.restart = true,
                    Ok(ApplyOutcome::ManualInstall { command_hint, .. }) => {
                        self.applying = false;
                        self.state = UpdateUiState::ManualInstall {
                            version: version.unwrap_or_default(),
                            command_hint,
                        };
                    }
                    Err(message) => {
                        self.applying = false;
                        self.state = UpdateUiState::Failed { version, message };
                    }
                }
            }
        }
    }

    /// The version this state is about, if any.
    pub fn version(&self) -> Option<String> {
        match &self.state {
            UpdateUiState::Idle | UpdateUiState::Checking => None,
            UpdateUiState::Available(r) => Some(r.version.clone()),
            UpdateUiState::Downloading { version, .. }
            | UpdateUiState::ManualInstall { version, .. } => Some(version.clone()),
            UpdateUiState::ReadyToApply(p) => Some(p.version.clone()),
            UpdateUiState::Failed { version, .. } => version.clone(),
        }
    }

    pub fn capsule(&self) -> Option<Capsule> {
        Some(match &self.state {
            UpdateUiState::Idle | UpdateUiState::Checking => return None,
            UpdateUiState::Available(r) => Capsule::Offer(format!("更新至 v{}", r.version)),
            UpdateUiState::Downloading {
                downloaded, total, ..
            } => {
                let frac = total
                    .filter(|&t| t > 0)
                    .map(|t| (*downloaded as f32 / t as f32).clamp(0.0, 1.0));
                let text = match frac {
                    Some(f) => format!("下载中 {}%", (f * 100.0).floor() as u32),
                    None => format!("下载中 {:.1} MB", *downloaded as f32 / 1_048_576.0),
                };
                Capsule::Progress { text, frac }
            }
            UpdateUiState::ReadyToApply(p) => Capsule::Restart {
                text: if self.applying {
                    "正在重启…".to_string()
                } else {
                    format!("重启以应用 v{}", p.version)
                },
                busy: self.applying,
            },
            UpdateUiState::ManualInstall { version, .. } => {
                Capsule::Install(format!("安装 v{version}"))
            }
            UpdateUiState::Failed { .. } => Capsule::Retry,
        })
    }

    /// The drawer's one-line status of the last check.
    pub fn status_line(&self) -> String {
        match &self.state {
            UpdateUiState::Idle => match (&self.note, self.up_to_date) {
                (Some(note), _) => format!("检查更新失败：{note}"),
                (None, true) => "已是最新版本".to_string(),
                (None, false) => String::new(),
            },
            UpdateUiState::Checking => "正在检查更新…".to_string(),
            UpdateUiState::Available(r) => format!("发现新版本 v{}", r.version),
            UpdateUiState::Downloading { .. } => match self.capsule() {
                Some(Capsule::Progress { text, .. }) => text,
                _ => String::new(),
            },
            UpdateUiState::ReadyToApply(p) => {
                format!("v{} 已下载并通过校验，重启即可应用", p.version)
            }
            UpdateUiState::ManualInstall { version, .. } => {
                format!("v{version} 已下载，需在终端安装")
            }
            UpdateUiState::Failed { message, .. } => format!("更新失败：{message}"),
        }
    }

    /// The status line reports a failure.
    pub fn status_is_error(&self) -> bool {
        match self.state {
            UpdateUiState::Failed { .. } => true,
            UpdateUiState::Idle => self.note.is_some(),
            _ => false,
        }
    }

    /// The drawer's check button is usable.
    pub fn can_check(&self) -> bool {
        matches!(
            self.state,
            UpdateUiState::Idle | UpdateUiState::Failed { .. }
        )
    }

    pub fn listen(&mut self) -> Sender<Msg> {
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        tx
    }

    /// Applies everything the workers reported; `true` when something changed.
    pub fn pump(&mut self) -> bool {
        let mut msgs = Vec::new();
        let mut closed = false;
        if let Some(rx) = &self.rx {
            loop {
                match rx.try_recv() {
                    Ok(m) => msgs.push(m),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        closed = true;
                        break;
                    }
                }
            }
        }
        let mut changed = !msgs.is_empty();
        for m in msgs {
            self.on_msg(m);
        }
        if closed {
            self.rx = None;
            changed = true;
            // a worker that ended without an answer (its runtime never started)
            let lost = "更新任务中断".to_string();
            match self.state {
                UpdateUiState::Checking => self.on_msg(Msg::Checked(Err(lost))),
                UpdateUiState::Downloading { .. } => self.on_msg(Msg::Downloaded(Err(lost))),
                _ if self.applying && !self.restart => self.on_msg(Msg::Applied(Err(lost))),
                _ => {}
            }
        }
        changed
    }
}

/// Short, user-facing reason of an update failure.
pub fn describe(e: &UpdateError) -> String {
    match e {
        UpdateError::Request(_) | UpdateError::Status(_) => "网络错误".to_string(),
        UpdateError::InvalidChecksums(_)
        | UpdateError::ChecksumMissing(_)
        | UpdateError::ChecksumMismatch { .. } => "校验失败".to_string(),
        UpdateError::SignatureMismatch { .. } => "签名与当前安装不符".to_string(),
        UpdateError::UnsupportedPlatform => "此平台不支持自动更新".to_string(),
        UpdateError::NotAppBundle => "当前不是从应用包运行，无法自动替换".to_string(),
        UpdateError::MissingAsset(_) | UpdateError::InvalidVersion(_) => {
            "发布信息不完整".to_string()
        }
        UpdateError::Metadata(_) | UpdateError::InvalidAssetName(_) => "发布信息无效".to_string(),
        UpdateError::BadPackage(_) => "安装包不可用".to_string(),
        UpdateError::Command { program, .. } => format!("{program} 执行失败"),
        UpdateError::Io(e) => format!("文件读写失败（{}）", e.kind()),
    }
}

pub fn spawn_check(options: CheckOptions, tx: Sender<Msg>) {
    run_async(async move {
        let result = updater::check_for_update(options).await;
        let _ = tx.send(Msg::Checked(result.map_err(|e| describe(&e))));
    });
}

fn download_dir() -> PathBuf {
    std::env::temp_dir().join("skylatch-update")
}

pub fn spawn_download(release: UpdateRelease, tx: Sender<Msg>) {
    run_async(async move {
        let progress = tx.clone();
        let result = updater::download_and_verify(&release, &download_dir(), move |p| {
            let _ = progress.send(Msg::Progress(p));
        })
        .await;
        let _ = tx.send(Msg::Downloaded(result.map_err(|e| describe(&e))));
    });
}

/// Runs `stop_agent` (unless the package is installed by hand), then hands over to the new
/// build.
pub fn spawn_apply(
    package: VerifiedPackage,
    stop_agent: impl FnOnce() + Send + 'static,
    tx: Sender<Msg>,
) {
    std::thread::spawn(move || {
        if package.kind != InstallKind::LinuxDeb {
            stop_agent();
        }
        let result = updater::apply_update(&package, &ApplyOptions::default());
        let _ = tx.send(Msg::Applied(result.map_err(|e| describe(&e))));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_client_core::updater::Asset;

    fn release(version: &str, size: Option<u64>) -> UpdateRelease {
        let asset = |name: &str| Asset {
            name: name.into(),
            url: format!("https://example.test/{name}"),
            size,
        };
        UpdateRelease {
            version: version.into(),
            kind: InstallKind::MacApp,
            package: asset("Skylatch.dmg"),
            checksums: asset("SHA256SUMS"),
        }
    }

    fn package(version: &str, kind: InstallKind) -> VerifiedPackage {
        VerifiedPackage {
            path: PathBuf::from("/tmp/Skylatch.dmg"),
            version: version.into(),
            kind,
            sha256: "0".repeat(64),
        }
    }

    fn progress(downloaded_bytes: u64, total_bytes: Option<u64>) -> Msg {
        Msg::Progress(DownloadProgress {
            downloaded_bytes,
            total_bytes,
        })
    }

    #[test]
    fn idle_and_checking_hide_the_capsule() {
        let mut u = Updater::default();
        assert!(matches!(u.state, UpdateUiState::Idle));
        assert_eq!(u.capsule(), None);
        assert_eq!(u.status_line(), "");
        assert!(u.begin_check(true));
        assert!(matches!(u.state, UpdateUiState::Checking));
        assert_eq!(u.capsule(), None);
        assert!(!u.begin_check(false), "one check at a time");
        assert!(!u.can_check());
        assert_eq!(u.status_line(), "正在检查更新…");
    }

    #[test]
    fn a_check_without_news_returns_to_idle_and_says_so_in_the_drawer() {
        let mut u = Updater::default();
        u.begin_check(false);
        u.on_msg(Msg::Checked(Ok(None)));
        assert!(matches!(u.state, UpdateUiState::Idle));
        assert_eq!(u.capsule(), None);
        assert_eq!(u.status_line(), "已是最新版本");
        assert!(u.can_check() && !u.status_is_error());
    }

    #[test]
    fn a_quiet_check_that_fails_stays_hidden_and_a_manual_one_fails_loudly() {
        let mut u = Updater::default();
        u.begin_check(true);
        u.on_msg(Msg::Checked(Err("网络错误".into())));
        assert!(matches!(u.state, UpdateUiState::Idle));
        assert_eq!(u.capsule(), None, "never disturbs");
        assert_eq!(u.status_line(), "检查更新失败：网络错误");
        assert!(u.status_is_error());

        assert!(u.begin_check(false));
        assert_eq!(u.status_line(), "正在检查更新…");
        u.on_msg(Msg::Checked(Err("网络错误".into())));
        assert!(matches!(
            &u.state,
            UpdateUiState::Failed { version: None, message } if message == "网络错误"
        ));
        assert_eq!(u.capsule(), Some(Capsule::Retry));
        // nothing to download yet: a retry checks again
        assert!(u.begin_download().is_none());
        assert!(u.begin_check(false));
    }

    #[test]
    fn an_update_is_offered_downloaded_with_progress_and_made_ready() {
        let mut u = Updater::default();
        u.begin_check(true);
        u.on_msg(Msg::Checked(Ok(Some(release("0.2.0", Some(1000))))));
        assert!(matches!(&u.state, UpdateUiState::Available(r) if r.version == "0.2.0"));
        assert_eq!(u.capsule(), Some(Capsule::Offer("更新至 v0.2.0".into())));
        assert_eq!(u.status_line(), "发现新版本 v0.2.0");
        assert!(!u.begin_check(false), "an offer is not checked over");

        let r = u.begin_download().expect("the offered release");
        assert_eq!(r.version, "0.2.0");
        assert!(matches!(
            u.state,
            UpdateUiState::Downloading {
                downloaded: 0,
                total: Some(1000),
                ..
            }
        ));
        assert!(u.begin_download().is_none(), "one download at a time");
        assert_eq!(
            u.capsule(),
            Some(Capsule::Progress {
                text: "下载中 0%".into(),
                frac: Some(0.0)
            })
        );
        u.on_msg(progress(426, Some(1000)));
        assert_eq!(
            u.capsule(),
            Some(Capsule::Progress {
                text: "下载中 42%".into(),
                frac: Some(0.426)
            })
        );
        assert_eq!(u.status_line(), "下载中 42%");
        u.on_msg(Msg::Downloaded(Ok(package("0.2.0", InstallKind::MacApp))));
        assert!(matches!(&u.state, UpdateUiState::ReadyToApply(p) if p.version == "0.2.0"));
        assert_eq!(
            u.capsule(),
            Some(Capsule::Restart {
                text: "重启以应用 v0.2.0".into(),
                busy: false
            })
        );
        assert!(u.status_line().contains("v0.2.0"));
        // stray progress after the end changes nothing
        u.on_msg(progress(1, Some(2)));
        assert!(matches!(u.state, UpdateUiState::ReadyToApply(_)));
    }

    #[test]
    fn an_unknown_size_shows_megabytes() {
        let mut u = Updater::default();
        u.begin_check(true);
        u.on_msg(Msg::Checked(Ok(Some(release("0.2.0", None)))));
        u.begin_download();
        u.on_msg(progress(3 * 1_048_576 + 524_288, None));
        assert_eq!(
            u.capsule(),
            Some(Capsule::Progress {
                text: "下载中 3.5 MB".into(),
                frac: None
            })
        );
    }

    #[test]
    fn a_failed_download_offers_a_retry_of_the_same_release() {
        let mut u = Updater::default();
        u.begin_check(true);
        u.on_msg(Msg::Checked(Ok(Some(release("0.2.0", Some(10))))));
        u.begin_download();
        u.on_msg(Msg::Downloaded(Err("校验失败".into())));
        assert!(matches!(
            &u.state,
            UpdateUiState::Failed { version: Some(v), message } if v == "0.2.0" && message == "校验失败"
        ));
        assert_eq!(u.capsule(), Some(Capsule::Retry));
        assert_eq!(u.status_line(), "更新失败：校验失败");
        let again = u.begin_download().expect("retry the release");
        assert_eq!(again.version, "0.2.0");
        assert!(matches!(u.state, UpdateUiState::Downloading { .. }));
    }

    #[test]
    fn applying_restarts_once_or_falls_back_to_a_manual_install_or_a_failure() {
        let ready = || {
            let mut u = Updater::default();
            u.begin_check(true);
            u.on_msg(Msg::Checked(Ok(Some(release("0.2.0", None)))));
            u.begin_download();
            u.on_msg(Msg::Downloaded(Ok(package("0.2.0", InstallKind::LinuxDeb))));
            u
        };

        let mut u = ready();
        assert!(u.begin_apply().is_some());
        assert!(u.begin_apply().is_none(), "one hand-off per click");
        assert_eq!(
            u.capsule(),
            Some(Capsule::Restart {
                text: "正在重启…".into(),
                busy: true
            })
        );
        let tx = u.listen();
        tx.send(Msg::Applied(Ok(ApplyOutcome::RestartRequired)))
            .unwrap();
        drop(tx);
        assert!(u.pump());
        assert!(u.restart && u.applying);
        assert!(matches!(u.state, UpdateUiState::ReadyToApply(_)));

        let mut u = ready();
        u.begin_apply();
        u.on_msg(Msg::Applied(Ok(ApplyOutcome::ManualInstall {
            package_path: "/tmp/skylatch.deb".into(),
            command_hint: "sudo dpkg -i '/tmp/skylatch.deb'".into(),
        })));
        assert!(!u.applying);
        assert!(matches!(
            &u.state,
            UpdateUiState::ManualInstall { version, command_hint }
                if version == "0.2.0" && command_hint.starts_with("sudo dpkg -i")
        ));
        assert_eq!(u.capsule(), Some(Capsule::Install("安装 v0.2.0".into())));
        assert!(u.status_line().contains("终端"));
        assert!(!u.can_check());

        let mut u = ready();
        u.begin_apply();
        u.on_msg(Msg::Applied(Err("签名与当前安装不符".into())));
        assert!(!u.applying);
        assert!(matches!(&u.state, UpdateUiState::Failed { version: Some(v), .. } if v == "0.2.0"));
        assert_eq!(u.capsule(), Some(Capsule::Retry));
        // the download is retried, the verified file is fetched again
        assert!(u.begin_download().is_some());
    }

    #[test]
    fn late_reports_of_another_state_are_ignored() {
        let mut u = Updater::default();
        u.on_msg(Msg::Checked(Ok(Some(release("9.9.9", None)))));
        assert!(matches!(u.state, UpdateUiState::Idle));
        u.on_msg(Msg::Downloaded(Ok(package("9.9.9", InstallKind::MacApp))));
        assert!(matches!(u.state, UpdateUiState::Idle));
        u.on_msg(Msg::Applied(Ok(ApplyOutcome::RestartRequired)));
        assert!(!u.applying && !u.restart);
    }

    #[test]
    fn workers_report_through_the_channel() {
        let mut u = Updater::default();
        assert!(u.rx.is_none());
        u.begin_check(false);
        let tx = u.listen();
        assert!(u.rx.is_some());
        assert!(!u.pump());
        tx.send(Msg::Checked(Ok(None))).unwrap();
        drop(tx);
        assert!(u.pump());
        assert!(matches!(u.state, UpdateUiState::Idle));
        assert!(u.rx.is_none(), "the channel closes with the worker");

        // a worker that vanished without an answer
        u.begin_check(false);
        drop(u.listen());
        assert!(u.pump());
        assert!(matches!(u.state, UpdateUiState::Failed { .. }));
    }

    #[test]
    fn failures_are_explained_briefly() {
        assert_eq!(
            describe(&UpdateError::ChecksumMismatch {
                expected: "a".into(),
                actual: "b".into()
            }),
            "校验失败"
        );
        assert_eq!(describe(&UpdateError::Status(503)), "网络错误");
        assert_eq!(describe(&UpdateError::Request("dns".into())), "网络错误");
        assert!(describe(&UpdateError::NotAppBundle).contains("应用包"));
    }

    #[test]
    fn the_quiet_check_can_be_switched_off() {
        let env = |v: Option<&'static str>| move |_: &str| v.map(String::from);
        assert!(auto_check_enabled(&env(None)));
        assert!(auto_check_enabled(&env(Some("0"))));
        assert!(!auto_check_enabled(&env(Some("1"))));
        assert!(!auto_check_enabled(&env(Some(" true "))));
    }
}

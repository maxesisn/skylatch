// SPDX-License-Identifier: Apache-2.0

//! Live hub access: device discovery on a background thread, and launching the native
//! stream client for the selected device.
//!
//! The GPUI thread never blocks on the network. A worker thread owns a small tokio runtime,
//! calls [`skylatch_client_core::list_devices`] (and fetches the saved workspace layout next to
//! it) on start-up, on every manual refresh and every [`POLL_INTERVAL`], and sends the outcome
//! back over a channel that the UI drains. Saving a layout runs on its own short-lived thread
//! ([`save_layout`]).

use skylatch_client_core::{ClientError, WorkspaceLayout, get_layout, list_devices, put_layout};
use skylatch_proto::control::DeviceInfo;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// How often the device list is refreshed while a hub is configured.
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Hub endpoint and account token.
#[derive(Clone, PartialEq, Eq)]
pub struct HubTarget {
    pub url: String,
    pub token: String,
}

impl fmt::Debug for HubTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HubTarget")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .finish()
    }
}

enum Cmd {
    Configure(u64, Option<HubTarget>),
    Refresh,
    Quit,
}

/// Outcome of one discovery round.
#[derive(Debug)]
pub struct PollResult {
    pub devices: Result<Vec<DeviceInfo>, String>,
    /// The account's saved arrangement; `None` when it could not be fetched (the device list
    /// is still good then).
    pub layout: Option<WorkspaceLayout>,
}

/// Handle to the polling worker.
pub struct LiveHandle {
    tx: Sender<Cmd>,
    rx: Receiver<(u64, PollResult)>,
    generation: u64,
}

impl LiveHandle {
    /// Starts the worker; it stays idle until [`LiveHandle::configure`] gives it a hub.
    pub fn spawn() -> LiveHandle {
        let (tx, cmd_rx) = mpsc::channel();
        let (res_tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("skylatch-hub-poll".into())
            .spawn(move || worker(cmd_rx, res_tx))
            .expect("spawn hub polling thread");
        LiveHandle {
            tx,
            rx,
            generation: 0,
        }
    }

    /// Points the worker at a hub (or stops polling with `None`) and polls right away.
    /// Results of the previous configuration are dropped.
    pub fn configure(&mut self, target: Option<HubTarget>) {
        self.generation += 1;
        let _ = self.tx.send(Cmd::Configure(self.generation, target));
    }

    /// Polls now instead of waiting for the next interval.
    pub fn refresh(&self) {
        let _ = self.tx.send(Cmd::Refresh);
    }

    /// Next finished round for the current configuration, if any.
    pub fn try_recv(&self) -> Option<PollResult> {
        while let Ok((generation, result)) = self.rx.try_recv() {
            if generation == self.generation {
                return Some(result);
            }
        }
        None
    }

    /// Blocks up to `timeout` for the next result of the current configuration.
    #[cfg(test)]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<PollResult> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(left) {
                Ok((generation, result)) if generation == self.generation => return Some(result),
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
    }
}

impl Drop for LiveHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
    }
}

fn worker(cmd_rx: Receiver<Cmd>, res_tx: Sender<(u64, PollResult)>) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    let mut target: Option<HubTarget> = None;
    let mut generation = 0u64;
    let mut due = Instant::now();
    loop {
        let wait = if target.is_some() {
            due.saturating_duration_since(Instant::now())
        } else {
            Duration::from_secs(3600)
        };
        match cmd_rx.recv_timeout(wait) {
            Ok(Cmd::Configure(g, t)) => {
                generation = g;
                target = t;
                due = Instant::now();
            }
            Ok(Cmd::Refresh) => due = Instant::now(),
            Ok(Cmd::Quit) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let Some(t) = target.clone() else { continue };
        if Instant::now() < due {
            continue;
        }
        let (devices, layout) = rt.block_on(async {
            let devices = list_devices(&t.url, &t.token)
                .await
                .map_err(|e| describe_error(&e));
            let layout = match devices {
                Ok(_) => get_layout(&t.url, &t.token).await.ok(),
                Err(_) => None,
            };
            (devices, layout)
        });
        due = Instant::now() + POLL_INTERVAL;
        if res_tx
            .send((generation, PollResult { devices, layout }))
            .is_err()
        {
            return;
        }
    }
}

/// Runs one hub request on a short-lived worker thread with its own tokio runtime, so the UI
/// thread never waits on the network. The answer arrives on the returned channel; if the
/// runtime cannot start, the channel is closed without an answer.
pub fn run_async<T, F>(fut: F) -> Receiver<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = T> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("skylatch-hub-request".into())
        .spawn(move || {
            if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                let _ = tx.send(rt.block_on(fut));
            }
        });
    rx
}

/// Saves `layout` on the hub in the background (`PUT /api/v1/layout`). The answer, the layout
/// the hub stored or a one-line explanation, arrives on the returned channel; the UI thread
/// never waits for it.
pub fn save_layout(
    hub_url: &str,
    token: &str,
    layout: &WorkspaceLayout,
) -> Receiver<Result<WorkspaceLayout, String>> {
    let (url, token, layout) = (hub_url.to_string(), token.to_string(), layout.clone());
    run_async(async move {
        put_layout(&url, &token, &layout)
            .await
            .map_err(|e| describe_error(&e))
    })
}

/// One-line, user-facing explanation of a discovery failure.
pub fn describe_error(e: &ClientError) -> String {
    match e {
        ClientError::Hub { code, message }
            if code.contains("unauthor") || code.contains("auth") =>
        {
            format!("Token 无效或未授权（{code}: {message}）")
        }
        ClientError::Hub { code, message } => format!("Hub 拒绝了请求（{code}: {message}）"),
        ClientError::Timeout(what) => format!("Hub 响应超时（{what}）"),
        ClientError::InvalidUrl => "Hub 地址无效".to_string(),
        ClientError::InvalidArgument(what) => format!("参数无效：{what}"),
        ClientError::WebSocket(err) => {
            let text = err.to_string();
            if text.contains("401") || text.contains("403") {
                format!("Token 无效或未授权（{text}）")
            } else {
                format!("无法连接 Hub（{text}）")
            }
        }
        other => format!("Hub 通信失败（{other}）"),
    }
}

// ───────────────────────────── native client launch ─────────────────────────────

#[cfg(windows)]
const CLIENT_NAME: &str = "skylatch-client.exe";
#[cfg(not(windows))]
const CLIENT_NAME: &str = "skylatch-client";

#[cfg(windows)]
const CLIENT_REPO_PATH: [&str; 3] = ["clients", "windows", CLIENT_NAME];
#[cfg(target_os = "macos")]
const CLIENT_REPO_PATH: [&str; 3] = ["clients", "macos", CLIENT_NAME];
#[cfg(not(any(windows, target_os = "macos")))]
const CLIENT_REPO_PATH: [&str; 3] = ["clients", "linux", CLIENT_NAME];

/// Arguments of the native client for one device.
pub fn client_args(hub: &str, token: &str, device_id: &str, prefer_direct: bool) -> Vec<String> {
    let mut args: Vec<String> = ["--hub", hub, "--token", token, "--device", device_id]
        .iter()
        .map(|s| s.to_string())
        .collect();
    if !prefer_direct {
        args.push("--no-direct".into());
    }
    args
}

/// Finds the native viewer: an explicit path (settings or `SKYLATCH_CLIENT`) wins, then a
/// sibling of this executable, then the repository's `clients/<os>/` build output found by
/// walking up from the executable and from the working directory.
pub fn locate_client(
    explicit: Option<&str>,
    exe_dir: Option<&Path>,
    cwd: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(p) = explicit.map(str::trim).filter(|p| !p.is_empty()) {
        let path = PathBuf::from(p);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(format!("指定的 skylatch-client 不存在：{p}"))
        };
    }
    if let Some(dir) = exe_dir {
        let sibling = dir.join(CLIENT_NAME);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    for start in [exe_dir, cwd].into_iter().flatten() {
        for dir in start.ancestors().take(8) {
            let candidate = CLIENT_REPO_PATH
                .iter()
                .fold(dir.to_path_buf(), |acc, part| acc.join(part));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err(format!(
        "未找到 {CLIENT_NAME}（可在 Hub 设置或 SKYLATCH_CLIENT 中指定路径）"
    ))
}

/// Locates the client relative to the running process.
pub fn locate_client_here(explicit: Option<&str>) -> Result<PathBuf, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().ok();
    locate_client(explicit, exe_dir.as_deref(), cwd.as_deref())
}

/// Starts the client detached from this process.
pub fn spawn_client(exe: &Path, args: &[String]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = exe.parent().filter(|d| !d.as_os_str().is_empty()) {
        // the Windows client loads its core DLL from its own directory
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // this launcher has no console of its own: never let the client flash one
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn()?;
    // reap the child when it exits so it never lingers as a zombie
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skylatch-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn client_arguments_match_the_native_client_cli() {
        assert_eq!(
            client_args("wss://hub.example.com", "tok", "dev-1", true),
            [
                "--hub",
                "wss://hub.example.com",
                "--token",
                "tok",
                "--device",
                "dev-1"
            ]
        );
        let relay = client_args("wss://hub.example.com", "tok", "dev-1", false);
        assert_eq!(relay.last().map(String::as_str), Some("--no-direct"));
        assert_eq!(relay.len(), 7);
    }

    #[test]
    fn explicit_client_path_wins_and_must_exist() {
        let dir = temp_dir("explicit");
        let exe = dir.join("custom-viewer");
        std::fs::write(&exe, b"").unwrap();
        assert_eq!(locate_client(exe.to_str(), Some(&dir), None).unwrap(), exe);
        let missing = dir.join("nope");
        let err = locate_client(missing.to_str(), Some(&dir), None).unwrap_err();
        assert!(err.contains("不存在"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn client_is_found_next_to_the_executable_then_in_the_repo_layout() {
        let root = temp_dir("repo");
        let exe_dir = root
            .join("clients")
            .join("desktop-ui")
            .join("target")
            .join("release");
        std::fs::create_dir_all(&exe_dir).unwrap();
        // nothing built yet
        assert!(locate_client(None, Some(&exe_dir), None).is_err());
        // repo layout, found by walking up from the executable
        let repo_client = CLIENT_REPO_PATH.iter().fold(root.clone(), |a, p| a.join(p));
        std::fs::create_dir_all(repo_client.parent().unwrap()).unwrap();
        std::fs::write(&repo_client, b"").unwrap();
        assert_eq!(
            locate_client(None, Some(&exe_dir), None).unwrap(),
            repo_client
        );
        // ... and from the working directory when the executable lives elsewhere
        let elsewhere = temp_dir("elsewhere");
        assert_eq!(
            locate_client(None, Some(&elsewhere), Some(&root.join("clients"))).unwrap(),
            repo_client
        );
        // a sibling binary beats the repo layout
        let sibling = exe_dir.join(CLIENT_NAME);
        std::fs::write(&sibling, b"").unwrap();
        assert_eq!(locate_client(None, Some(&exe_dir), None).unwrap(), sibling);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn hub_errors_are_explained_in_one_line() {
        let unauthorized = ClientError::Hub {
            code: "unauthorized".into(),
            message: "bad token".into(),
        };
        assert!(describe_error(&unauthorized).contains("Token"));
        let other = ClientError::Hub {
            code: "not_found".into(),
            message: "no such device".into(),
        };
        assert!(describe_error(&other).contains("not_found"));
        assert!(describe_error(&ClientError::InvalidUrl).contains("地址"));
        assert!(describe_error(&ClientError::Timeout("device discovery")).contains("超时"));
        assert!(!describe_error(&unauthorized).contains('\n'));
    }

    #[test]
    fn target_debug_hides_the_token() {
        let t = HubTarget {
            url: "wss://hub.example.com".into(),
            token: "very-secret".into(),
        };
        assert!(!format!("{t:?}").contains("very-secret"));
    }

    #[test]
    fn unreachable_hub_is_reported_without_blocking_the_caller() {
        let mut live = LiveHandle::spawn();
        assert!(live.try_recv().is_none(), "idle until configured");
        let started = Instant::now();
        // nothing listens on the discard port of the loopback interface
        live.configure(Some(HubTarget {
            url: "ws://127.0.0.1:9".into(),
            token: "t".into(),
        }));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "configure never blocks"
        );
        let result = live
            .recv_timeout(Duration::from_secs(20))
            .expect("an error result arrives");
        let err = result.devices.expect_err("nothing is listening");
        assert!(err.contains("Hub"), "{err}");
        // a refresh polls again straight away
        live.refresh();
        assert!(live.recv_timeout(Duration::from_secs(20)).is_some());
        // switching to demo stops polling; stale results are dropped
        live.configure(None);
        assert!(live.recv_timeout(Duration::from_millis(300)).is_none());
    }
}

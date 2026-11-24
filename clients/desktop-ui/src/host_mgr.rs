// SPDX-License-Identifier: Apache-2.0

//! Managed local host: lets other devices take over *this* machine without any command line.
//!
//! While the launcher is signed in to a hub and hosting is switched on, a background
//! supervisor thread
//!
//! 1. locates `skylatch-agent` and `skylatch-host-engine` (next to this executable, else the
//!    repository build output),
//! 2. enrolls the machine once per hub: it mints a pairing code with the account token and runs
//!    `skylatch-agent join <hub> --code <code>`, so the agent itself writes its standard
//!    `agent.json` with a dedicated, revocable per-device token (no password is ever stored),
//! 3. keeps `skylatch-agent --engine-bin <engine> --enable-direct` running, restarting it with a
//!    growing delay if it dies, and stops it when hosting is switched off, the account signs
//!    out, or the launcher quits.
//!
//! The supervisor talks to the outside world through the [`Backend`] trait, so its behaviour is
//! tested here without spawning anything but a harmless sleeper.

use crate::config::{self, Platform};
use crate::live::HubTarget;
use skylatch_proto::credentials::is_token;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(windows)]
const AGENT_NAME: &str = "skylatch-agent.exe";
#[cfg(not(windows))]
const AGENT_NAME: &str = "skylatch-agent";

#[cfg(windows)]
const ENGINE_NAME: &str = "skylatch-host-engine.exe";
#[cfg(not(windows))]
const ENGINE_NAME: &str = "skylatch-host-engine";

/// The one phrase the user sees when the host components are not installed.
pub const MISSING_TEXT: &str = "未找到本机被控组件";

// ───────────────────────────── binaries and enrollment state ─────────────────────────────

/// The two programs a hosted machine needs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Binaries {
    pub agent: PathBuf,
    pub engine: PathBuf,
}

/// Finds both programs. Installed bundles keep them next to the launcher; a source checkout has
/// the agent in `target/release/` and the engine in `host/`, found by walking up from the
/// executable and from the working directory.
pub fn locate_binaries(exe_dir: Option<&Path>, cwd: Option<&Path>) -> Result<Binaries, String> {
    let find = |name: &str, repo_dirs: &[&[&str]]| -> Option<PathBuf> {
        if let Some(dir) = exe_dir {
            let sibling = dir.join(name);
            if sibling.is_file() {
                return Some(sibling);
            }
        }
        for start in [exe_dir, cwd].into_iter().flatten() {
            for dir in start.ancestors().take(8) {
                for parts in repo_dirs {
                    let candidate = parts.iter().fold(dir.to_path_buf(), |a, p| a.join(p));
                    let candidate = candidate.join(name);
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    };
    let agent = find(AGENT_NAME, &[&["target", "release"]]);
    let engine = find(ENGINE_NAME, &[&["host"], &["target", "release"]]);
    match (agent, engine) {
        (Some(agent), Some(engine)) => Ok(Binaries { agent, engine }),
        (agent, engine) => {
            let mut missing = Vec::new();
            if agent.is_none() {
                missing.push(AGENT_NAME);
            }
            if engine.is_none() {
                missing.push(ENGINE_NAME);
            }
            Err(format!("{MISSING_TEXT}（{}）", missing.join("、")))
        }
    }
}

/// Locates the programs relative to the running process.
pub fn locate_binaries_here() -> Result<Binaries, String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().ok();
    locate_binaries(exe_dir.as_deref(), cwd.as_deref())
}

/// Where `skylatch-agent join` saves its credentials: `agent.json` next to this launcher's own
/// `config.json`.
pub fn agent_json_path(platform: Platform, env: config::Env) -> Option<PathBuf> {
    config::path_for(platform, env).map(|p| p.with_file_name("agent.json"))
}

/// `agent.json` of this machine, where the platform keeps it.
pub fn agent_config_path() -> Option<PathBuf> {
    agent_json_path(Platform::current(), &config::process_env)
}

/// The hub device ID this machine enrolled with, when `agent.json` is for `hub_url`. This is how
/// the launcher knows which device on the desk is the machine it runs on.
pub fn local_device_id(path: &Path, hub_url: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Saved {
        hub_url: String,
        device_id: String,
    }
    let saved: Saved = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let id = saved.device_id.trim();
    (!id.is_empty() && hub_key(&saved.hub_url) == hub_key(hub_url)).then(|| id.to_string())
}

/// What the local `agent.json` says about a hub.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Enrollment {
    /// No credentials saved.
    Missing,
    /// Credentials for this hub.
    Current,
    /// Credentials for a different hub: this machine has to be enrolled again.
    OtherHub,
    /// The file exists but is not usable.
    Invalid,
}

/// Canonical form of a hub address, so `https://Hub.example.com/`, `wss://hub.example.com` and
/// the agent's `wss://hub.example.com/ws/host` compare equal.
pub fn hub_key(url: &str) -> String {
    let normalized = config::normalize_hub_url(url).to_ascii_lowercase();
    normalized
        .strip_suffix("/ws/host")
        .unwrap_or(&normalized)
        .trim_end_matches('/')
        .to_string()
}

pub fn enrollment_state(path: &Path, hub_url: &str) -> Enrollment {
    #[derive(serde::Deserialize)]
    struct Saved {
        hub_url: String,
        device_token: String,
    }
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Enrollment::Missing,
        Err(_) => return Enrollment::Invalid,
    };
    match serde_json::from_slice::<Saved>(&data) {
        Ok(s) if is_token(&s.device_token) => {
            if hub_key(&s.hub_url) == hub_key(hub_url) {
                Enrollment::Current
            } else {
                Enrollment::OtherHub
            }
        }
        _ => Enrollment::Invalid,
    }
}

// ───────────────────────────── status ─────────────────────────────

/// What the supervisor is doing, as shown in the header chip and the tray menu.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum HostStatus {
    /// Hosting is switched off.
    #[default]
    Off,
    /// Hosting is on but there is no hub to host for.
    Idle,
    /// Registering this machine with the hub.
    Enrolling,
    /// The agent is running.
    Running,
    /// Enrollment or the agent failed; trying again after a delay.
    Retrying(String),
    /// The agent or the engine is not installed.
    Missing(String),
}

impl fmt::Display for HostStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostStatus::Off => f.write_str("本机未共享"),
            HostStatus::Idle => f.write_str("本机被控：等待登录 Hub"),
            HostStatus::Enrolling => f.write_str("正在登记本机…"),
            HostStatus::Running => f.write_str("本机可被控"),
            HostStatus::Retrying(why) => write!(f, "本机被控异常：{why}"),
            HostStatus::Missing(why) => f.write_str(why),
        }
    }
}

/// How the header chip looks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChipKind {
    /// Green dot: remote machines can connect.
    Shared,
    /// Blue hollow dot: getting ready.
    Starting,
    /// Warning dot.
    Problem,
    /// Muted hollow dot: hosting is off.
    Off,
}

pub fn chip_look(enabled: bool, status: &HostStatus) -> (ChipKind, &'static str) {
    if !enabled {
        return (ChipKind::Off, "本机未共享");
    }
    match status {
        HostStatus::Running => (ChipKind::Shared, "本机可被控"),
        HostStatus::Retrying(_) => (ChipKind::Problem, "本机被控异常"),
        HostStatus::Missing(_) => (ChipKind::Problem, MISSING_TEXT),
        HostStatus::Off | HostStatus::Idle | HostStatus::Enrolling => {
            (ChipKind::Starting, "本机准备中…")
        }
    }
}

// ───────────────────────────── processes ─────────────────────────────

/// A running agent.
pub trait Process: Send {
    /// `true` once the process is gone.
    fn exited(&mut self) -> bool;
    /// Stops the process and everything it started. Safe to call twice.
    fn stop(&mut self);
}

/// Everything the supervisor needs from the machine.
pub trait Backend: Send + 'static {
    fn locate(&self) -> Result<Binaries, String>;
    /// Credentials for `hub_url` are saved already.
    fn enrolled(&self, hub_url: &str) -> bool;
    /// Registers this machine with the hub (blocking), in `room` when one is chosen.
    fn enroll(&self, bins: &Binaries, hub: &HubTarget, room: Option<&str>) -> Result<(), String>;
    fn start(&self, bins: &Binaries, room: Option<&str>) -> Result<Box<dyn Process>, String>;
}

/// Variables that would make the agent use the launcher's account token instead of its own
/// device credentials.
const SCRUBBED_ENV: [&str; 4] = [
    "SKYLATCH_HUB",
    "SKYLATCH_TOKEN",
    "SKYLATCH_DEVICE_ID",
    "SKYLATCH_DEVICE_NAME",
];

/// Arguments of `skylatch-agent join` for a one-time `code`.
pub fn join_args(hub_url: &str, code: &str, room: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "join".to_string(),
        hub_url.to_string(),
        "--code".to_string(),
        code.to_string(),
    ];
    args.extend(room_args(room));
    args
}

/// Arguments of the long-running agent.
pub fn run_args(engine: &str, room: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--engine-bin".to_string(),
        engine.to_string(),
        "--enable-direct".to_string(),
    ];
    args.extend(room_args(room));
    args
}

/// `--room <room>` for a non-blank room.
fn room_args(room: Option<&str>) -> Vec<String> {
    match room.map(str::trim).filter(|r| !r.is_empty()) {
        Some(room) => vec!["--room".to_string(), room.to_string()],
        None => Vec::new(),
    }
}

fn agent_command(bins: &Binaries, args: &[String]) -> Command {
    let mut cmd = Command::new(&bins.agent);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    if let Some(dir) = bins.agent.parent().filter(|d| !d.as_os_str().is_empty()) {
        cmd.current_dir(dir);
    }
    hide_console(&mut cmd);
    cmd
}

/// Never flash a console window: this launcher is a GUI program.
fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

/// A child process that is stopped together with everything it starts.
pub struct ChildProcess {
    child: Child,
    done: bool,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl ChildProcess {
    pub fn spawn(mut cmd: Command) -> std::io::Result<ChildProcess> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // its own process group: a terminal Ctrl-C aimed at the launcher leaves it alone,
            // and the whole group can be signalled on stop
            cmd.process_group(0);
        }
        let child = cmd.spawn()?;
        #[cfg(windows)]
        let job = job::Job::kill_on_close().filter(|j| j.assign(&child));
        Ok(ChildProcess {
            child,
            done: false,
            #[cfg(windows)]
            job,
        })
    }

    #[cfg(test)]
    pub fn id(&self) -> u32 {
        self.child.id()
    }
}

impl Process for ChildProcess {
    fn exited(&mut self) -> bool {
        self.done || !matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        if self.done {
            return;
        }
        self.done = true;
        #[cfg(windows)]
        {
            // closing the job terminates the agent and the engine it started
            self.job = None;
        }
        #[cfg(unix)]
        {
            // SAFETY: plain signal to a process group this struct created.
            let group = -(self.child.id() as i32);
            unsafe { libc::kill(group, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            // SAFETY: as above.
            unsafe { libc::kill(group, libc::SIGKILL) };
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(windows)]
mod job {
    //! A Windows job object that kills its processes when the last handle closes: the managed
    //! agent (and the engine it starts) can never outlive the launcher, even if it crashes.

    use std::os::windows::io::AsRawHandle;
    use std::process::Child;

    type Handle = *mut core::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        per_process_user_time_limit: i64,
        per_job_user_time_limit: i64,
        limit_flags: u32,
        minimum_working_set_size: usize,
        maximum_working_set_size: usize,
        active_process_limit: u32,
        affinity: usize,
        priority_class: u32,
        scheduling_class: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        counters: [u64; 6],
    }

    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io: IoCounters,
        process_memory_limit: usize,
        job_memory_limit: usize,
        peak_process_memory_used: usize,
        peak_job_memory_used: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *mut core::ffi::c_void, name: *const u16) -> Handle;
        fn SetInformationJobObject(
            job: Handle,
            class: i32,
            info: *const core::ffi::c_void,
            len: u32,
        ) -> i32;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;
    const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x2000;

    pub struct Job(Handle);

    // SAFETY: a job handle is just a kernel object reference, usable from any thread.
    unsafe impl Send for Job {}

    impl Job {
        pub fn kill_on_close() -> Option<Job> {
            // SAFETY: documented Win32 calls with valid arguments; the handle is closed in
            // `Drop`, or right here when configuring it fails.
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
                if handle.is_null() {
                    return None;
                }
                let mut limits = ExtendedLimits::default();
                limits.basic.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                    (&raw const limits).cast(),
                    size_of::<ExtendedLimits>() as u32,
                );
                if ok == 0 {
                    CloseHandle(handle);
                    return None;
                }
                Some(Job(handle))
            }
        }

        pub fn assign(&self, child: &Child) -> bool {
            // SAFETY: both handles are valid for the duration of the call.
            unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle().cast()) != 0 }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateJobObjectW` and is closed exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

// ───────────────────────────── the real machine ─────────────────────────────

/// How long `skylatch-agent join` may take.
const JOIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Runs on the real machine: real files, the hub over HTTP, real processes.
pub struct SystemBackend {
    agent_json: Option<PathBuf>,
}

impl SystemBackend {
    pub fn new() -> SystemBackend {
        SystemBackend {
            agent_json: agent_config_path(),
        }
    }
}

impl Backend for SystemBackend {
    fn locate(&self) -> Result<Binaries, String> {
        locate_binaries_here()
    }

    fn enrolled(&self, hub_url: &str) -> bool {
        self.agent_json
            .as_deref()
            .is_some_and(|p| enrollment_state(p, hub_url) == Enrollment::Current)
    }

    fn enroll(&self, bins: &Binaries, hub: &HubTarget, room: Option<&str>) -> Result<(), String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("无法启动网络运行时：{e}"))?;
        let code = rt
            .block_on(skylatch_client_core::create_pairing_code(
                &hub.url, &hub.token,
            ))
            .map_err(|e| crate::onboard::describe_pairing_error(&e))?
            .code;
        let mut cmd = agent_command(bins, &join_args(&hub.url, &code, room));
        cmd.stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("无法运行 join：{e}"))?;
        let deadline = Instant::now() + JOIN_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("登记本机超时".into());
                }
                Err(e) => return Err(format!("登记本机失败：{e}")),
            }
        };
        if status.success() {
            return Ok(());
        }
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            use std::io::Read;
            let mut buf = Vec::new();
            let _ = pipe.by_ref().take(4096).read_to_end(&mut buf);
            stderr = String::from_utf8_lossy(&buf).into_owned();
        }
        Err(describe_join_failure(&stderr))
    }

    fn start(&self, bins: &Binaries, room: Option<&str>) -> Result<Box<dyn Process>, String> {
        let engine = bins.engine.to_string_lossy().into_owned();
        let cmd = agent_command(bins, &run_args(&engine, room));
        ChildProcess::spawn(cmd)
            .map(|p| Box::new(p) as Box<dyn Process>)
            .map_err(|e| format!("无法启动 {AGENT_NAME}：{e}"))
    }
}

/// One-line reason for a failed `join`, from the agent's error output.
pub fn describe_join_failure(stderr: &str) -> String {
    if stderr.contains("already enrolled") || stderr.contains("409") {
        return "本机已在该 Hub 登记过，请先在 Hub 控制台吊销旧的设备令牌".into();
    }
    if stderr.contains("invalid or expired") || stderr.contains("401") {
        return "配对码无效或已过期".into();
    }
    let line = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("登记本机失败");
    let short: String = line.chars().take(100).collect();
    format!("登记本机失败：{short}")
}

// ───────────────────────────── supervisor ─────────────────────────────

/// Supervisor timings (shortened in tests).
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    /// How often a running agent is checked for having exited.
    pub poll: Duration,
    /// First delay before a retry; doubles up to `backoff_max`.
    pub backoff_initial: Duration,
    pub backoff_max: Duration,
    /// An agent that ran this long counts as healthy: the next failure starts at the first delay.
    pub stable_after: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            poll: Duration::from_millis(500),
            backoff_initial: Duration::from_secs(2),
            backoff_max: Duration::from_secs(60),
            stable_after: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Desired {
    enabled: bool,
    hub: Option<HubTarget>,
    /// Room the machine should join (enrollment and agent start).
    room: Option<String>,
}

enum Cmd {
    Configure(Desired),
    Shutdown,
}

/// How long [`HostManager::shutdown`] waits for the supervisor to stop the agent.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(8);

/// Handle to the supervisor thread. Dropping it (or [`HostManager::shutdown`]) stops the agent.
pub struct HostManager {
    tx: Sender<Cmd>,
    status: Arc<Mutex<HostStatus>>,
    stop: Arc<AtomicBool>,
    done: Mutex<Option<Receiver<()>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl HostManager {
    /// The real thing.
    pub fn spawn() -> HostManager {
        HostManager::with_backend(Box::new(SystemBackend::new()), Timing::default())
    }

    pub fn with_backend(backend: Box<dyn Backend>, timing: Timing) -> HostManager {
        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(HostStatus::Off));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let status = status.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("skylatch-host".into())
                .spawn(move || {
                    supervise(rx, backend, timing, &status, &stop);
                    let _ = done_tx.send(());
                })
                .expect("spawn host supervisor thread")
        };
        HostManager {
            tx,
            status,
            stop,
            done: Mutex::new(Some(done_rx)),
            thread: Mutex::new(Some(thread)),
        }
    }

    /// Declares what should be running: host this machine for `hub` (in `room`, when one is
    /// chosen), or nothing.
    pub fn configure(&self, enabled: bool, hub: Option<HubTarget>, room: Option<String>) {
        let _ = self.tx.send(Cmd::Configure(Desired { enabled, hub, room }));
    }

    pub fn status(&self) -> HostStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Stops the agent and the supervisor, then returns (bounded wait). Idempotent.
    pub fn shutdown(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::Shutdown);
        let done = self.done.lock().ok().and_then(|mut d| d.take());
        if let Some(done) = done {
            let finished = done.recv_timeout(SHUTDOWN_WAIT).is_ok();
            let thread = self.thread.lock().ok().and_then(|mut t| t.take());
            if finished && let Some(thread) = thread {
                let _ = thread.join();
            }
        }
    }
}

impl Drop for HostManager {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn set_status(status: &Mutex<HostStatus>, next: HostStatus) {
    if let Ok(mut s) = status.lock() {
        *s = next;
    }
}

fn supervise(
    rx: Receiver<Cmd>,
    backend: Box<dyn Backend>,
    timing: Timing,
    status: &Mutex<HostStatus>,
    stop: &AtomicBool,
) {
    let mut desired = Desired {
        enabled: false,
        hub: None,
        room: None,
    };
    let mut proc: Option<Box<dyn Process>> = None;
    let mut started_at = Instant::now();
    let mut backoff = timing.backoff_initial;
    let mut retry_at: Option<Instant> = None;
    let stop_proc = |proc: &mut Option<Box<dyn Process>>| {
        if let Some(mut p) = proc.take() {
            p.stop();
        }
    };

    loop {
        let wait = match retry_at {
            Some(at) if proc.is_none() => at
                .saturating_duration_since(Instant::now())
                .min(timing.poll),
            _ => timing.poll,
        };
        match rx.recv_timeout(wait) {
            Ok(Cmd::Configure(next)) => {
                // a new token for the same hub keeps the running agent: only the switch
                // position, the hub itself or the room restarts things
                let key = |d: &Desired| d.hub.as_ref().map(|h| hub_key(&h.url));
                if next.enabled != desired.enabled
                    || key(&next) != key(&desired)
                    || next.room != desired.room
                {
                    stop_proc(&mut proc);
                    retry_at = None;
                    backoff = timing.backoff_initial;
                }
                desired = next;
            }
            Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }

        let Some(hub) = desired.hub.clone().filter(|_| desired.enabled) else {
            stop_proc(&mut proc);
            set_status(
                status,
                if desired.enabled {
                    HostStatus::Idle
                } else {
                    HostStatus::Off
                },
            );
            continue;
        };

        if let Some(p) = proc.as_mut() {
            if !p.exited() {
                if started_at.elapsed() >= timing.stable_after {
                    backoff = timing.backoff_initial;
                }
                continue;
            }
            proc = None;
            set_status(
                status,
                HostStatus::Retrying("本机被控组件意外退出，正在重启".into()),
            );
            retry_at = Some(Instant::now() + backoff);
            backoff = (backoff * 2).min(timing.backoff_max);
            continue;
        }
        if retry_at.is_some_and(|at| Instant::now() < at) {
            continue;
        }
        retry_at = None;

        match launch(&*backend, &hub, desired.room.as_deref(), status, stop) {
            Launch::Started(p) => {
                proc = Some(p);
                started_at = Instant::now();
                set_status(status, HostStatus::Running);
            }
            Launch::Stopped => break,
            Launch::Failed(next) => {
                set_status(status, next);
                retry_at = Some(Instant::now() + backoff);
                backoff = (backoff * 2).min(timing.backoff_max);
            }
        }
    }
    stop_proc(&mut proc);
    set_status(status, HostStatus::Off);
}

enum Launch {
    Started(Box<dyn Process>),
    /// Shutdown was requested while preparing.
    Stopped,
    Failed(HostStatus),
}

fn launch(
    backend: &dyn Backend,
    hub: &HubTarget,
    room: Option<&str>,
    status: &Mutex<HostStatus>,
    stop: &AtomicBool,
) -> Launch {
    let bins = match backend.locate() {
        Ok(b) => b,
        Err(why) => return Launch::Failed(HostStatus::Missing(why)),
    };
    if !backend.enrolled(&hub.url) {
        set_status(status, HostStatus::Enrolling);
        if let Err(why) = backend.enroll(&bins, hub, room) {
            return Launch::Failed(HostStatus::Retrying(why));
        }
    }
    // never start an agent after the launcher asked to quit while enrollment was running
    if stop.load(Ordering::SeqCst) {
        return Launch::Stopped;
    }
    match backend.start(&bins, room) {
        Ok(p) => Launch::Started(p),
        Err(why) => Launch::Failed(HostStatus::Retrying(why)),
    }
}

// ───────────────────────────── tests ─────────────────────────────

/// A scripted machine, shared with the view tests in `main.rs`.
#[cfg(test)]
pub mod testing {
    use super::*;

    #[derive(Default)]
    pub struct FakeState {
        pub installed: bool,
        pub enrolled: bool,
        pub enroll_error: Option<String>,
        pub enroll_delay: Duration,
        pub enroll_calls: usize,
        pub starts: usize,
        pub stops: usize,
        /// the room each enrollment / agent start was asked for
        pub enroll_rooms: Vec<Option<String>>,
        pub start_rooms: Vec<Option<String>>,
        pub alive: Vec<Arc<AtomicBool>>,
    }

    /// Cloneable view of a [`FakeBackend`]'s state.
    #[derive(Clone, Default)]
    pub struct Fake(pub Arc<Mutex<FakeState>>);

    impl Fake {
        pub fn installed() -> Fake {
            let f = Fake::default();
            f.0.lock().unwrap().installed = true;
            f
        }

        pub fn backend(&self) -> Box<dyn Backend> {
            Box::new(FakeBackend(self.clone()))
        }

        pub fn running(&self) -> usize {
            let s = self.0.lock().unwrap();
            s.alive.iter().filter(|a| a.load(Ordering::SeqCst)).count()
        }

        /// Simulates the agent dying on its own.
        pub fn crash_all(&self) {
            for a in &self.0.lock().unwrap().alive {
                a.store(false, Ordering::SeqCst);
            }
        }
    }

    struct FakeBackend(Fake);
    struct FakeProcess {
        fake: Fake,
        alive: Arc<AtomicBool>,
    }

    impl Process for FakeProcess {
        fn exited(&mut self) -> bool {
            !self.alive.load(Ordering::SeqCst)
        }
        fn stop(&mut self) {
            if self.alive.swap(false, Ordering::SeqCst) {
                self.fake.0.lock().unwrap().stops += 1;
            }
        }
    }

    impl Backend for FakeBackend {
        fn locate(&self) -> Result<Binaries, String> {
            if self.0.0.lock().unwrap().installed {
                Ok(Binaries {
                    agent: PathBuf::from("agent"),
                    engine: PathBuf::from("engine"),
                })
            } else {
                Err(format!("{MISSING_TEXT}（agent）"))
            }
        }
        fn enrolled(&self, _hub_url: &str) -> bool {
            self.0.0.lock().unwrap().enrolled
        }
        fn enroll(
            &self,
            _bins: &Binaries,
            _hub: &HubTarget,
            room: Option<&str>,
        ) -> Result<(), String> {
            let delay = {
                let mut s = self.0.0.lock().unwrap();
                s.enroll_calls += 1;
                s.enroll_rooms.push(room.map(str::to_string));
                s.enroll_delay
            };
            std::thread::sleep(delay);
            let mut s = self.0.0.lock().unwrap();
            match s.enroll_error.clone() {
                Some(e) => Err(e),
                None => {
                    s.enrolled = true;
                    Ok(())
                }
            }
        }
        fn start(&self, _bins: &Binaries, room: Option<&str>) -> Result<Box<dyn Process>, String> {
            let alive = Arc::new(AtomicBool::new(true));
            let mut s = self.0.0.lock().unwrap();
            s.starts += 1;
            s.start_rooms.push(room.map(str::to_string));
            s.alive.push(alive.clone());
            Ok(Box::new(FakeProcess {
                fake: self.0.clone(),
                alive,
            }))
        }
    }

    pub const FAST: Timing = Timing {
        poll: Duration::from_millis(5),
        backoff_initial: Duration::from_millis(20),
        backoff_max: Duration::from_millis(80),
        stable_after: Duration::from_secs(5),
    };

    pub fn hub(url: &str) -> HubTarget {
        HubTarget {
            url: url.into(),
            token: "account-token".into(),
        }
    }

    /// Polls `cond` for up to five seconds.
    pub fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use std::collections::HashMap;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("skylatch-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn binaries_are_found_next_to_the_launcher_then_in_a_source_checkout() {
        let root = temp_dir("bins");
        let exe_dir = root.join("clients/desktop-ui/target/release");
        std::fs::create_dir_all(&exe_dir).unwrap();

        // nothing built: both programs are named in the message
        let err = locate_binaries(Some(&exe_dir), None).unwrap_err();
        assert!(err.starts_with(MISSING_TEXT), "{err}");
        assert!(
            err.contains(AGENT_NAME) && err.contains(ENGINE_NAME),
            "{err}"
        );

        // source checkout: agent in target/release, engine in host/
        let agent = root.join("target/release").join(AGENT_NAME);
        touch(&agent);
        let err = locate_binaries(Some(&exe_dir), None).unwrap_err();
        assert!(
            err.contains(ENGINE_NAME) && !err.contains(AGENT_NAME),
            "{err}"
        );
        let engine = root.join("host").join(ENGINE_NAME);
        touch(&engine);
        assert_eq!(
            locate_binaries(Some(&exe_dir), None).unwrap(),
            Binaries {
                agent: agent.clone(),
                engine: engine.clone()
            }
        );
        // ... also reachable from the working directory
        let elsewhere = temp_dir("bins-elsewhere");
        assert_eq!(
            locate_binaries(Some(&elsewhere), Some(&root.join("clients"))).unwrap(),
            Binaries { agent, engine }
        );

        // installed bundle: siblings win over the checkout
        let sibling_agent = exe_dir.join(AGENT_NAME);
        let sibling_engine = exe_dir.join(ENGINE_NAME);
        touch(&sibling_agent);
        touch(&sibling_engine);
        assert_eq!(
            locate_binaries(Some(&exe_dir), None).unwrap(),
            Binaries {
                agent: sibling_agent,
                engine: sibling_engine
            }
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn agent_credentials_live_next_to_the_launcher_config() {
        let win: HashMap<_, _> = [("APPDATA", r"D:\Roaming")].into();
        let p = agent_json_path(Platform::Windows, &|k| win.get(k).map(|v| v.to_string()));
        assert!(
            p.unwrap()
                .ends_with(Path::new("Skylatch").join("agent.json"))
        );
        let mac: HashMap<_, _> = [("HOME", "/mock/user")].into();
        let p = agent_json_path(Platform::MacOs, &|k| mac.get(k).map(|v| v.to_string()));
        assert_eq!(
            p.unwrap(),
            PathBuf::from("/mock/user/Library/Application Support/Skylatch/agent.json")
        );
        let none = agent_json_path(Platform::Linux, &|_| None);
        assert_eq!(none, None);
    }

    #[test]
    fn hub_addresses_compare_the_way_the_agent_stores_them() {
        assert_eq!(hub_key("https://Hub.Example.com/"), "wss://hub.example.com");
        assert_eq!(
            hub_key("wss://hub.example.com/ws/host"),
            "wss://hub.example.com"
        );
        assert_eq!(hub_key("http://127.0.0.1:8080"), "ws://127.0.0.1:8080");
        assert_ne!(hub_key("wss://a.example"), hub_key("wss://b.example"));
        assert_ne!(hub_key("wss://a.example/x"), hub_key("wss://a.example"));
    }

    #[test]
    fn enrollment_state_reads_the_agent_json() {
        let dir = temp_dir("enroll-state");
        let path = dir.join("agent.json");
        assert_eq!(
            enrollment_state(&path, "wss://hub.example.com"),
            Enrollment::Missing
        );
        let token = "ab".repeat(32);
        let saved = |hub: &str, token: &str| {
            format!(
                r#"{{"hub_url":"{hub}","device_id":"pc","name":"PC","device_token":"{token}","room":null,"slot":null}}"#
            )
        };
        std::fs::write(&path, saved("wss://hub.example.com/ws/host", &token)).unwrap();
        assert_eq!(
            enrollment_state(&path, "wss://hub.example.com"),
            Enrollment::Current
        );
        assert_eq!(
            enrollment_state(&path, "https://hub.example.com/"),
            Enrollment::Current
        );
        assert_eq!(
            enrollment_state(&path, "wss://other.example.com"),
            Enrollment::OtherHub
        );
        std::fs::write(&path, saved("wss://hub.example.com/ws/host", "short")).unwrap();
        assert_eq!(
            enrollment_state(&path, "wss://hub.example.com"),
            Enrollment::Invalid
        );
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(
            enrollment_state(&path, "wss://hub.example.com"),
            Enrollment::Invalid
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chip_and_join_messages_are_short_and_plain() {
        assert_eq!(
            chip_look(true, &HostStatus::Running),
            (ChipKind::Shared, "本机可被控")
        );
        assert_eq!(
            chip_look(false, &HostStatus::Running),
            (ChipKind::Off, "本机未共享")
        );
        assert_eq!(
            chip_look(true, &HostStatus::Enrolling).0,
            ChipKind::Starting
        );
        assert_eq!(
            chip_look(true, &HostStatus::Retrying("x".into())).0,
            ChipKind::Problem
        );
        assert_eq!(
            chip_look(true, &HostStatus::Missing(MISSING_TEXT.into())),
            (ChipKind::Problem, MISSING_TEXT)
        );
        assert!(describe_join_failure("Error: hub error 409 already enrolled").contains("吊销"));
        assert!(describe_join_failure("401 invalid or expired credentials").contains("过期"));
        let long = describe_join_failure(&format!("first\n{}\n", "x".repeat(500)));
        assert!(long.chars().count() < 130 && !long.contains('\n'), "{long}");
    }

    #[test]
    fn hosting_enrolls_once_runs_and_follows_the_switch() {
        let fake = Fake::installed();
        let m = HostManager::with_backend(fake.backend(), FAST);
        assert_eq!(m.status(), HostStatus::Off);
        m.configure(true, Some(hub("wss://hub.example.com")), None);
        wait_until("agent running", || m.status() == HostStatus::Running);
        {
            let s = fake.0.lock().unwrap();
            assert_eq!((s.enroll_calls, s.starts), (1, 1), "enrolled, then started");
        }
        assert_eq!(fake.running(), 1);

        m.configure(false, Some(hub("wss://hub.example.com")), None);
        wait_until("agent stopped", || m.status() == HostStatus::Off);
        assert_eq!(fake.running(), 0, "switching off stops the agent");

        m.configure(true, Some(hub("wss://hub.example.com")), None);
        wait_until("agent running again", || m.status() == HostStatus::Running);
        let s = fake.0.lock().unwrap();
        assert_eq!(s.enroll_calls, 1, "already enrolled: no second enrollment");
        assert_eq!(s.starts, 2);
    }

    #[test]
    fn a_crashed_agent_is_restarted_with_a_delay() {
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let m = HostManager::with_backend(fake.backend(), FAST);
        m.configure(true, Some(hub("wss://hub.example.com")), None);
        wait_until("first start", || fake.0.lock().unwrap().starts == 1);
        let crashed = Instant::now();
        fake.crash_all();
        wait_until("restart", || fake.0.lock().unwrap().starts == 2);
        assert!(
            crashed.elapsed() >= FAST.backoff_initial,
            "the restart waits for the back-off"
        );
        wait_until("running again", || m.status() == HostStatus::Running);
        // crashing right away again waits longer
        let crashed = Instant::now();
        fake.crash_all();
        wait_until("second restart", || fake.0.lock().unwrap().starts == 3);
        assert!(crashed.elapsed() >= FAST.backoff_initial * 2);
    }

    #[test]
    fn missing_components_are_reported_without_failing_and_found_later() {
        let fake = Fake::default();
        let m = HostManager::with_backend(fake.backend(), FAST);
        m.configure(true, Some(hub("wss://hub.example.com")), None);
        wait_until(
            "missing status",
            || matches!(m.status(), HostStatus::Missing(ref t) if t.starts_with(MISSING_TEXT)),
        );
        assert_eq!(fake.0.lock().unwrap().starts, 0);
        fake.0.lock().unwrap().installed = true;
        wait_until("running once installed", || {
            m.status() == HostStatus::Running
        });
    }

    #[test]
    fn a_failed_enrollment_is_retried_and_never_starts_the_agent() {
        let fake = Fake::installed();
        fake.0.lock().unwrap().enroll_error = Some("Hub 拒绝了请求".into());
        let m = HostManager::with_backend(fake.backend(), FAST);
        m.configure(true, Some(hub("wss://hub.example.com")), None);
        wait_until(
            "retry status",
            || matches!(m.status(), HostStatus::Retrying(ref why) if why.contains("拒绝")),
        );
        wait_until("second attempt", || {
            fake.0.lock().unwrap().enroll_calls >= 2
        });
        assert_eq!(fake.0.lock().unwrap().starts, 0);
        fake.0.lock().unwrap().enroll_error = None;
        wait_until("running", || m.status() == HostStatus::Running);
    }

    #[test]
    fn signing_out_or_changing_hub_stops_and_restarts_the_agent() {
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let m = HostManager::with_backend(fake.backend(), FAST);
        m.configure(true, Some(hub("wss://one.example.com")), None);
        wait_until("running", || m.status() == HostStatus::Running);
        m.configure(true, Some(hub("wss://two.example.com")), None);
        wait_until("restarted for the new hub", || {
            fake.0.lock().unwrap().starts == 2
        });
        assert_eq!(fake.running(), 1, "the old agent is gone");
        m.configure(true, None, None);
        wait_until("idle after sign-out", || m.status() == HostStatus::Idle);
        assert_eq!(fake.running(), 0);
    }

    #[test]
    fn shutdown_stops_the_agent_and_blocks_a_late_start() {
        let fake = Fake::installed();
        fake.0.lock().unwrap().enrolled = true;
        let m = HostManager::with_backend(fake.backend(), FAST);
        m.configure(true, Some(hub("wss://hub.example.com")), None);
        wait_until("running", || m.status() == HostStatus::Running);
        m.shutdown();
        assert_eq!(fake.running(), 0);
        assert_eq!(m.status(), HostStatus::Off);
        m.shutdown(); // idempotent

        // quitting while the machine is still being enrolled must not leave an agent behind
        let slow = Fake::installed();
        slow.0.lock().unwrap().enroll_delay = Duration::from_millis(150);
        let m = HostManager::with_backend(slow.backend(), FAST);
        m.configure(true, Some(hub("wss://hub.example.com")), None);
        wait_until("enrollment started", || {
            slow.0.lock().unwrap().enroll_calls == 1
        });
        m.shutdown();
        assert_eq!(slow.0.lock().unwrap().starts, 0);
        assert_eq!(slow.running(), 0);

        // dropping the handle is enough
        let dropped = Fake::installed();
        dropped.0.lock().unwrap().enrolled = true;
        {
            let m = HostManager::with_backend(dropped.backend(), FAST);
            m.configure(true, Some(hub("wss://hub.example.com")), None);
            wait_until("running", || dropped.running() == 1);
        }
        assert_eq!(dropped.running(), 0);
    }

    #[test]
    fn the_chosen_room_reaches_the_join_and_the_agent_command_lines() {
        assert_eq!(
            join_args("wss://hub.example.com", "482-917", Some("办公室")),
            [
                "join",
                "wss://hub.example.com",
                "--code",
                "482-917",
                "--room",
                "办公室"
            ]
        );
        assert_eq!(
            join_args("wss://hub.example.com", "482-917", None),
            ["join", "wss://hub.example.com", "--code", "482-917"]
        );
        assert_eq!(
            run_args("engine", Some(" 客厅工作台 ")),
            [
                "--engine-bin",
                "engine",
                "--enable-direct",
                "--room",
                "客厅工作台"
            ]
        );
        assert_eq!(
            run_args("engine", Some("  ")),
            ["--engine-bin", "engine", "--enable-direct"],
            "a blank room is not passed"
        );

        // the supervisor hands the room to the enrollment and to every start, and a new room
        // restarts the agent
        let fake = Fake::installed();
        let m = HostManager::with_backend(fake.backend(), FAST);
        m.configure(
            true,
            Some(hub("wss://hub.example.com")),
            Some("办公室".into()),
        );
        wait_until("running", || m.status() == HostStatus::Running);
        m.configure(
            true,
            Some(hub("wss://hub.example.com")),
            Some("书房".into()),
        );
        wait_until("restarted in the new room", || {
            fake.0.lock().unwrap().starts == 2
        });
        let s = fake.0.lock().unwrap();
        assert_eq!(s.enroll_rooms, [Some("办公室".to_string())]);
        assert_eq!(
            s.start_rooms,
            [Some("办公室".to_string()), Some("书房".to_string())]
        );
    }

    #[test]
    fn a_real_child_process_is_killed_on_stop() {
        #[cfg(windows)]
        let mut cmd = {
            let mut c = Command::new("ping");
            c.args(["-n", "60", "127.0.0.1"]);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = Command::new("sleep");
            c.arg("60");
            c
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console(&mut cmd);
        let mut p = ChildProcess::spawn(cmd).expect("spawn a sleeper");
        assert!(p.id() > 0);
        assert!(!p.exited(), "still sleeping");
        let started = Instant::now();
        p.stop();
        assert!(p.exited());
        assert!(started.elapsed() < Duration::from_secs(5));
        p.stop(); // twice is fine
    }
}

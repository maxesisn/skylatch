//! The Skylatch-managed EasyTier system service: one `easytier-core` running every managed
//! profile, as a macOS LaunchDaemon, a systemd unit or a Windows service. It only ever touches
//! its own service name and files, never an independently installed EasyTier.
//!
//! Changing the service needs administrator rights. Without them the operations return
//! [`ServiceError::NeedsElevation`] with the exact steps to run from an elevated prompt.

use crate::process::{command, output_with_timeout};
use crate::toml_config::ManagedLaunch;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const LAUNCHD_LABEL: &str = "io.github.maxesisn.skylatch.easytier";
pub const SYSTEMD_UNIT: &str = "skylatch-easytier.service";
pub const WINDOWS_SERVICE: &str = "SkylatchEasyTier";
const LAUNCHD_LOG: &str = "/var/log/skylatch-easytier.log";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServicePlatform {
    Launchd,
    Systemd,
    Windows,
}

impl ServicePlatform {
    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "macos" => Some(Self::Launchd),
            "linux" => Some(Self::Systemd),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }

    pub(crate) fn definition_path(self) -> Option<PathBuf> {
        match self {
            Self::Launchd => Some(PathBuf::from(format!(
                "/Library/LaunchDaemons/{LAUNCHD_LABEL}.plist"
            ))),
            Self::Systemd => Some(PathBuf::from(format!("/etc/systemd/system/{SYSTEMD_UNIT}"))),
            Self::Windows => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }
}

impl fmt::Display for CommandSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&shell_quote(&self.program))?;
        for arg in &self.args {
            write!(f, " {}", shell_quote(arg))?;
        }
        Ok(())
    }
}

/// One argument quoted for this platform's shell.
fn shell_quote(arg: &str) -> String {
    if cfg!(windows) {
        return windows_quote(arg);
    }
    if !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b))
    {
        arg.into()
    } else {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStep {
    Run {
        command: CommandSpec,
        allow_failure: bool,
    },
    WriteFile {
        path: PathBuf,
        contents: String,
    },
    RemoveFile(PathBuf),
    /// Polls `probe` until it fails, e.g. while launchd finishes an asynchronous `bootout`.
    WaitUntilGone(CommandSpec),
}

impl fmt::Display for ServiceStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Run {
                command,
                allow_failure,
            } => write!(
                f,
                "{command}{}",
                if *allow_failure { " || true" } else { "" }
            ),
            Self::WriteFile { path, .. } => write!(f, "write {}", path.display()),
            Self::RemoveFile(path) => write!(f, "remove {}", path.display()),
            Self::WaitUntilGone(probe) => write!(f, "wait until `{probe}` fails"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("administrator rights are required; run from an elevated prompt:\n{}", describe(.0))]
    NeedsElevation(Vec<ServiceStep>),
    #[error("`{command}` failed: {stderr}")]
    Command { command: String, stderr: String },
    #[error("the EasyTier service is not supported on this operating system")]
    Unsupported,
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub fn describe(steps: &[ServiceStep]) -> String {
    steps
        .iter()
        .map(|s| format!("  {s}"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct RunResult {
    pub success: bool,
    pub stderr: String,
}

/// Carries out service steps; [`SystemExecutor`] on real machines, a recorder in tests.
pub trait Executor: Send + Sync {
    fn is_elevated(&self) -> bool;
    fn run(&self, command: &CommandSpec) -> io::Result<RunResult>;
    fn write_file(&self, path: &Path, contents: &str) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn exists(&self, path: &Path) -> bool;
}

pub struct SystemExecutor;

impl Executor for SystemExecutor {
    fn is_elevated(&self) -> bool {
        is_elevated()
    }

    fn run(&self, spec: &CommandSpec) -> io::Result<RunResult> {
        let mut cmd = command(&spec.program);
        cmd.args(&spec.args);
        let output = output_with_timeout(cmd, COMMAND_TIMEOUT)?;
        Ok(RunResult {
            success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }

    fn write_file(&self, path: &Path, contents: &str) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// Whether this process runs as root or as an elevated Windows administrator.
pub fn is_elevated() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
        // SAFETY: the token handle is owned here and closed once; the output buffer is a
        // correctly sized TOKEN_ELEVATION.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            );
            CloseHandle(token);
            ok != 0 && elevation.TokenIsElevated != 0
        }
    }
    #[cfg(not(any(unix, windows)))]
    false
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn launchd_plist(program: &[String]) -> String {
    let args: String = program
        .iter()
        .map(|a| format!("        <string>{}</string>\n", xml_escape(a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{LAUNCHD_LOG}</string>
    <key>StandardErrorPath</key>
    <string>{LAUNCHD_LOG}</string>
</dict>
</plist>
"#
    )
}

fn systemd_quote(arg: &str) -> String {
    format!(
        "\"{}\"",
        arg.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}

fn systemd_unit(program: &[String]) -> String {
    let exec: Vec<String> = program.iter().map(|a| systemd_quote(a)).collect();
    format!(
        "[Unit]\nDescription=Skylatch-managed EasyTier\nWants=network-online.target\nAfter=network-online.target\n\n\
         [Service]\nExecStart={}\nRestart=on-failure\nRestartSec=5s\n\n\
         [Install]\nWantedBy=multi-user.target\n",
        exec.join(" ")
    )
}

/// One argument as `CommandLineToArgvW` reads it back.
fn windows_quote(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.into();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            c => {
                quoted.push_str(&"\\".repeat(backslashes));
                quoted.push(c);
                backslashes = 0;
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

pub struct ServiceManager {
    platform: ServicePlatform,
    executor: Box<dyn Executor>,
}

impl ServiceManager {
    pub fn system() -> Option<Self> {
        ServicePlatform::current().map(|p| Self::with_executor(p, Box::new(SystemExecutor)))
    }

    pub fn with_executor(platform: ServicePlatform, executor: Box<dyn Executor>) -> Self {
        Self { platform, executor }
    }

    pub fn platform(&self) -> ServicePlatform {
        self.platform
    }

    fn run(program: &str, args: &[&str], allow_failure: bool) -> ServiceStep {
        ServiceStep::Run {
            command: CommandSpec::new(program, args),
            allow_failure,
        }
    }

    fn removal_steps(&self) -> Vec<ServiceStep> {
        let target = format!("system/{LAUNCHD_LABEL}");
        match self.platform {
            ServicePlatform::Launchd => vec![
                Self::run("launchctl", &["bootout", &target], true),
                ServiceStep::WaitUntilGone(CommandSpec::new("launchctl", &["print", &target])),
            ],
            ServicePlatform::Systemd => {
                vec![Self::run(
                    "systemctl",
                    &["disable", "--now", SYSTEMD_UNIT],
                    true,
                )]
            }
            ServicePlatform::Windows => vec![
                Self::run("sc.exe", &["stop", WINDOWS_SERVICE], true),
                Self::run("sc.exe", &["delete", WINDOWS_SERVICE], true),
                ServiceStep::WaitUntilGone(CommandSpec::new("sc.exe", &["query", WINDOWS_SERVICE])),
            ],
        }
    }

    /// Replaces any previous managed service with one running `core` with `launch`.
    pub fn install_plan(&self, core: &Path, launch: &ManagedLaunch) -> Vec<ServiceStep> {
        let mut program = vec![core.to_string_lossy().into_owned()];
        program.extend(launch.args());
        let mut steps = self.removal_steps();
        match self.platform {
            ServicePlatform::Launchd => {
                let plist = self.platform.definition_path().unwrap_or_default();
                steps.push(ServiceStep::WriteFile {
                    path: plist.clone(),
                    contents: launchd_plist(&program),
                });
                steps.push(Self::run(
                    "launchctl",
                    &["bootstrap", "system", &plist.to_string_lossy()],
                    false,
                ));
            }
            ServicePlatform::Systemd => {
                steps.push(ServiceStep::WriteFile {
                    path: self.platform.definition_path().unwrap_or_default(),
                    contents: systemd_unit(&program),
                });
                steps.push(Self::run("systemctl", &["daemon-reload"], false));
                steps.push(Self::run(
                    "systemctl",
                    &["enable", "--now", SYSTEMD_UNIT],
                    false,
                ));
            }
            ServicePlatform::Windows => {
                let bin_path: Vec<String> = program.iter().map(|a| windows_quote(a)).collect();
                steps.push(Self::run(
                    "sc.exe",
                    &[
                        "create",
                        WINDOWS_SERVICE,
                        "binPath=",
                        &bin_path.join(" "),
                        "start=",
                        "auto",
                        "DisplayName=",
                        "Skylatch EasyTier",
                    ],
                    false,
                ));
                steps.push(Self::run(
                    "sc.exe",
                    &[
                        "failure",
                        WINDOWS_SERVICE,
                        "reset=",
                        "86400",
                        "actions=",
                        "restart/5000",
                    ],
                    true,
                ));
                steps.push(Self::run("sc.exe", &["start", WINDOWS_SERVICE], false));
            }
        }
        steps
    }

    pub fn uninstall_plan(&self) -> Vec<ServiceStep> {
        let mut steps = self.removal_steps();
        match self.platform {
            ServicePlatform::Launchd => {
                steps.extend(self.platform.definition_path().map(ServiceStep::RemoveFile));
            }
            ServicePlatform::Systemd => {
                steps.extend(self.platform.definition_path().map(ServiceStep::RemoveFile));
                steps.push(Self::run("systemctl", &["daemon-reload"], false));
            }
            ServicePlatform::Windows => {}
        }
        steps
    }

    pub fn is_installed(&self) -> bool {
        match self.platform.definition_path() {
            Some(path) => self.executor.exists(&path),
            None => self
                .executor
                .run(&CommandSpec::new("sc.exe", &["query", WINDOWS_SERVICE]))
                .is_ok_and(|r| r.success),
        }
    }

    fn execute(&self, steps: Vec<ServiceStep>) -> Result<(), ServiceError> {
        if !self.executor.is_elevated() {
            return Err(ServiceError::NeedsElevation(steps));
        }
        for step in &steps {
            match step {
                ServiceStep::Run {
                    command,
                    allow_failure,
                } => {
                    let result = self.executor.run(command)?;
                    if !result.success && !allow_failure {
                        return Err(ServiceError::Command {
                            command: command.to_string(),
                            stderr: result.stderr,
                        });
                    }
                }
                ServiceStep::WriteFile { path, contents } => {
                    self.executor.write_file(path, contents)?
                }
                ServiceStep::RemoveFile(path) => self.executor.remove_file(path)?,
                ServiceStep::WaitUntilGone(probe) => {
                    for _ in 0..50 {
                        if !self.executor.run(probe).is_ok_and(|r| r.success) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn install_and_start(
        &self,
        core: &Path,
        launch: &ManagedLaunch,
    ) -> Result<(), ServiceError> {
        self.execute(self.install_plan(core, launch))
    }

    pub fn stop_and_uninstall(&self) -> Result<(), ServiceError> {
        self.execute(self.uninstall_plan())
    }
}

/// Starts `easytier-core` as an ordinary user process, which works without administrator rights
/// only when every profile runs without a TUN device. Output goes to `log`.
pub fn spawn_user_process(
    core: &Path,
    launch: &ManagedLaunch,
    log: &Path,
) -> io::Result<std::process::Child> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)?;
    let mut cmd = command(core);
    cmd.args(launch.args())
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    pub(crate) struct Recorder {
        pub elevated: bool,
        pub log: Mutex<Vec<String>>,
        pub present: Mutex<Vec<PathBuf>>,
    }

    impl Executor for Arc<Recorder> {
        fn is_elevated(&self) -> bool {
            self.elevated
        }
        fn run(&self, command: &CommandSpec) -> io::Result<RunResult> {
            self.log.lock().unwrap().push(command.to_string());
            Ok(RunResult {
                success: !command.args.iter().any(|a| a == "print" || a == "query"),
                stderr: String::new(),
            })
        }
        fn write_file(&self, path: &Path, _: &str) -> io::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("write {}", path.display()));
            self.present.lock().unwrap().push(path.into());
            Ok(())
        }
        fn remove_file(&self, path: &Path) -> io::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("remove {}", path.display()));
            self.present.lock().unwrap().retain(|p| p != path);
            Ok(())
        }
        fn exists(&self, path: &Path) -> bool {
            self.present.lock().unwrap().iter().any(|p| p == path)
        }
    }

    fn launch() -> ManagedLaunch {
        ManagedLaunch {
            rpc_portal: crate::MANAGED_RPC_PORTAL.into(),
            profile_ids: vec!["a".into()],
            config_files: vec![PathBuf::from(
                "/Users/Jane Doe/Skylatch/easytier/net-a.toml",
            )],
        }
    }

    #[test]
    fn launchd_plan_replaces_only_its_own_daemon() {
        let recorder = Arc::new(Recorder {
            elevated: true,
            ..Default::default()
        });
        let manager =
            ServiceManager::with_executor(ServicePlatform::Launchd, Box::new(recorder.clone()));
        let plan = manager.install_plan(Path::new("/opt/et/easytier-core"), &launch());
        let ServiceStep::WriteFile { path, contents } = &plan[2] else {
            panic!("{plan:?}");
        };
        assert_eq!(
            path,
            Path::new("/Library/LaunchDaemons/io.github.maxesisn.skylatch.easytier.plist")
        );
        assert!(contents.contains("<string>/Users/Jane Doe/Skylatch/easytier/net-a.toml</string>"));
        assert!(contents.contains("<string>127.0.0.1:15889</string>"));
        manager
            .install_and_start(Path::new("/opt/et/easytier-core"), &launch())
            .unwrap();
        assert!(manager.is_installed());
        manager.stop_and_uninstall().unwrap();
        assert!(!manager.is_installed());
        let log = recorder.log.lock().unwrap();
        assert!(log.iter().all(|line| !line.contains("cc.maxng")));
        assert_eq!(
            log.iter()
                .filter(|l| l
                    .starts_with("launchctl bootout system/io.github.maxesisn.skylatch.easytier"))
                .count(),
            2
        );
        assert!(log.contains(
            &"launchctl bootstrap system /Library/LaunchDaemons/io.github.maxesisn.skylatch.easytier.plist".to_string()
        ));
    }

    #[test]
    fn unprivileged_callers_get_the_elevated_steps() {
        let recorder = Arc::new(Recorder::default());
        let manager =
            ServiceManager::with_executor(ServicePlatform::Systemd, Box::new(recorder.clone()));
        let err = manager
            .install_and_start(Path::new("/opt/skylatch/easytier/easytier-core"), &launch())
            .unwrap_err();
        let ServiceError::NeedsElevation(steps) = &err else {
            panic!("{err}");
        };
        assert!(recorder.log.lock().unwrap().is_empty());
        let ServiceStep::WriteFile { contents, .. } = &steps[1] else {
            panic!("{steps:?}");
        };
        assert!(contents.contains(
            "ExecStart=\"/opt/skylatch/easytier/easytier-core\" \"--rpc-portal\" \"127.0.0.1:15889\" \"-c\" \"/Users/Jane Doe/Skylatch/easytier/net-a.toml\""
        ));
        let text = err.to_string();
        assert!(
            text.contains("systemctl enable --now skylatch-easytier.service"),
            "{text}"
        );
    }

    #[test]
    fn windows_service_command_line_is_quoted() {
        let manager = ServiceManager::with_executor(
            ServicePlatform::Windows,
            Box::new(Arc::new(Recorder::default())),
        );
        let launch = ManagedLaunch {
            config_files: vec![PathBuf::from(
                r"C:\ProgramData\Skylatch\easytier\net-a.toml",
            )],
            ..launch()
        };
        let plan = manager.install_plan(
            Path::new(r"C:\Program Files\Skylatch\EasyTier\easytier-core.exe"),
            &launch,
        );
        let create = plan
            .iter()
            .find_map(|s| match s {
                ServiceStep::Run { command, .. } if command.args[0] == "create" => Some(command),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            create.args[3],
            r#""C:\Program Files\Skylatch\EasyTier\easytier-core.exe" --rpc-portal 127.0.0.1:15889 -c C:\ProgramData\Skylatch\easytier\net-a.toml"#
        );
        assert_eq!(windows_quote(r#"a "b"\"#), r#""a \"b\"\\""#);
    }
}

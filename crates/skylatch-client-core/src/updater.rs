//! Self-update: release discovery, verified download and platform hand-off.

use crate::http::http_runtime;
use reqwest::Client;
use ring::digest::{Context, SHA256};
use serde::Deserialize;
use std::{
    cmp::Ordering,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
    time::Duration,
};

const DEFAULT_LATEST_URL: &str = "https://api.github.com/repos/maxesisn/skylatch/releases/latest";
const DEFAULT_LIST_URL: &str = "https://api.github.com/repos/maxesisn/skylatch/releases";
const API_TIMEOUT: Duration = Duration::from_secs(30);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(60);
const SUMS_NAME: &str = "SHA256SUMS";
const MAC_SCRIPT: &str = r#"#!/bin/sh
pid=$1; target=$2; new=$3; old="$target.old"
i=0
while kill -0 "$pid" 2>/dev/null && [ "$i" -lt 60 ]; do sleep 0.5; i=$((i+1)); done
if kill -0 "$pid" 2>/dev/null; then rm -rf "$new" "$0"; exit 1; fi
rm -rf "$old"
if ! mv "$target" "$old"; then rm -rf "$new" "$0"; exit 1; fi
if mv "$new" "$target"; then rm -rf "$old"; else mv "$old" "$target"; fi
open -a "$target"
rm -f "$0"
"#;
const WIN_SCRIPT: &str = r#"param($OldPid, $Target, $Src, $Stage)
$ErrorActionPreference = 'Stop'
try {
  $deadline = (Get-Date).AddSeconds(30)
  while ((Get-Process -Id $OldPid -ErrorAction SilentlyContinue) -and ((Get-Date) -lt $deadline)) { Start-Sleep -Milliseconds 250 }
  if (Get-Process -Id $OldPid -ErrorAction SilentlyContinue) { exit 1 }
  foreach ($name in 'skylatch-agent', 'skylatch-host-engine') {
    Get-Process -Name $name -ErrorAction SilentlyContinue |
      Where-Object { $_.Path -and $_.Path.StartsWith($Target, [StringComparison]::OrdinalIgnoreCase) } |
      Stop-Process -Force
  }
  Start-Sleep -Milliseconds 500
  Copy-Item -Path (Join-Path $Src '*') -Destination $Target -Recurse -Force
  Start-Process -FilePath (Join-Path $Target 'skylatch-ui.exe') -WorkingDirectory $Target
} finally {
  Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
  Remove-Item -Force $PSCommandPath -ErrorAction SilentlyContinue
}
"#;

/// Version baked in by the release workflow, falling back to the crate version.
pub fn current_version() -> &'static str {
    option_env!("SKYLATCH_BUILD_VERSION")
        .filter(|s| !s.is_empty())
        .unwrap_or(env!("CARGO_PKG_VERSION"))
        .trim_start_matches('v')
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("update request failed: {0}")]
    Request(String),
    #[error("update server answered HTTP {0}")]
    Status(u16),
    #[error("invalid release metadata: {0}")]
    Metadata(String),
    #[error("invalid version {0:?}")]
    InvalidVersion(String),
    #[error("release has no {0}")]
    MissingAsset(String),
    #[error("self-update is not supported on this platform")]
    UnsupportedPlatform,
    #[error("unsafe asset name {0:?}")]
    InvalidAssetName(String),
    #[error("invalid {SUMS_NAME}: {0}")]
    InvalidChecksums(String),
    #[error("{SUMS_NAME} has no entry for {0}")]
    ChecksumMissing(String),
    #[error("SHA-256 mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("not running from an .app bundle")]
    NotAppBundle,
    #[error("new package signature does not match the installed app (installed {expected:?}, new {actual:?})")]
    SignatureMismatch {
        expected: Option<String>,
        actual: Option<String>,
    },
    #[error("unusable update package: {0}")]
    BadPackage(String),
    #[error("{program} failed: {detail}")]
    Command {
        program: &'static str,
        detail: String,
    },
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemVer {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Vec<Ident>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Ident {
    Num(u64),
    Text(String),
}

impl SemVer {
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let bad = || UpdateError::InvalidVersion(text.to_owned());
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split_once('+').map_or(text, |(core, _)| core);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let number = |part: &str| {
            let valid = !part.is_empty()
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'));
            if valid {
                part.parse::<u64>().map_err(|_| bad())
            } else {
                Err(bad())
            }
        };
        let mut parts = core.split('.');
        let (major, minor, patch) = match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(a), Some(b), Some(c), None) => (number(a)?, number(b)?, number(c)?),
            _ => return Err(bad()),
        };
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    {
                        Err(bad())
                    } else if id.bytes().all(|b| b.is_ascii_digit()) {
                        number(id).map(Ident::Num)
                    } else {
                        Ok(Ident::Text(id.to_owned()))
                    }
                })
                .collect::<Result<_, _>>()?,
        };
        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    MacApp,
    WindowsInstaller,
    WindowsPortable,
    LinuxDeb,
}

impl InstallKind {
    pub fn detect() -> Result<Self, UpdateError> {
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Ok(Self::MacApp)
        } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            let exe = std::env::current_exe()?;
            Ok(exe
                .parent()
                .map_or(Self::WindowsPortable, Self::windows_in_dir))
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            Ok(Self::LinuxDeb)
        } else {
            Err(UpdateError::UnsupportedPlatform)
        }
    }

    pub fn windows_in_dir(dir: &Path) -> Self {
        let has_uninstaller = fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
                name.starts_with("unins") && name.ends_with(".exe")
            })
        });
        if has_uninstaller {
            Self::WindowsInstaller
        } else {
            Self::WindowsPortable
        }
    }

    pub fn asset_name(self, version: &str) -> String {
        match self {
            Self::MacApp => format!("Skylatch-{version}-macos-arm64.dmg"),
            Self::WindowsInstaller => format!("Skylatch-{version}-windows-x64-setup.exe"),
            Self::WindowsPortable => format!("Skylatch-{version}-windows-x64-portable.zip"),
            Self::LinuxDeb => format!("skylatch-{version}-linux-x86_64.deb"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    #[serde(rename = "browser_download_url")]
    pub url: String,
    #[serde(default)]
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateRelease {
    pub version: String,
    pub kind: InstallKind,
    pub package: Asset,
    pub checksums: Asset,
}

#[derive(Debug, Default, Clone)]
pub struct CheckOptions {
    pub allow_prerelease: bool,
    /// Overrides `SKYLATCH_UPDATE_API_URL` and the GitHub default.
    pub api_url: Option<String>,
    /// Overrides [`current_version`].
    pub current_version: Option<String>,
    pub kind: Option<InstallKind>,
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReleaseDoc {
    One(Box<ReleaseJson>),
    Many(Vec<ReleaseJson>),
}

pub fn select_assets(
    kind: InstallKind,
    version: &str,
    assets: &[Asset],
) -> Result<(Asset, Asset), UpdateError> {
    let find = |name: String| {
        assets
            .iter()
            .find(|asset| asset.name == name)
            .cloned()
            .ok_or(UpdateError::MissingAsset(name))
    };
    Ok((find(kind.asset_name(version))?, find(SUMS_NAME.into())?))
}

fn pick_release(
    doc: ReleaseDoc,
    current: &SemVer,
    options: &CheckOptions,
    kind: InstallKind,
) -> Result<Option<UpdateRelease>, UpdateError> {
    let releases = match doc {
        ReleaseDoc::One(one) => vec![*one],
        ReleaseDoc::Many(many) => many,
    };
    let newest = releases
        .into_iter()
        .filter(|r| !r.draft && (options.allow_prerelease || !r.prerelease))
        .filter_map(|r| SemVer::parse(&r.tag_name).ok().map(|v| (v, r)))
        .filter(|(v, _)| options.allow_prerelease || !v.is_prerelease())
        .max_by(|a, b| a.0.cmp(&b.0));
    let Some((_, release)) = newest.filter(|(v, _)| v > current) else {
        return Ok(None);
    };
    let version = release.tag_name.trim_start_matches('v').to_owned();
    let (package, checksums) = select_assets(kind, &version, &release.assets)?;
    Ok(Some(UpdateRelease {
        version,
        kind,
        package,
        checksums,
    }))
}

fn client() -> Result<&'static Client, UpdateError> {
    static CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // Release assets redirect to a CDN; no credentials are ever attached here.
            Client::builder()
                .user_agent(format!("skylatch/{}", current_version()))
                .connect_timeout(Duration::from_secs(15))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| UpdateError::Request(e.clone()))
}

fn spawn_http<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, UpdateError>> + Send + 'static,
) -> impl std::future::Future<Output = Result<T, UpdateError>> {
    let handle = http_runtime()
        .map(|rt| rt.spawn(future))
        .map_err(|e| UpdateError::Request(e.to_string()));
    async move {
        handle?
            .await
            .map_err(|_| UpdateError::Request("update task aborted".into()))?
    }
}

fn request_error(error: reqwest::Error) -> UpdateError {
    UpdateError::Request(error.without_url().to_string())
}

async fn get(url: &str, timeout: Option<Duration>) -> Result<reqwest::Response, UpdateError> {
    let mut request = client()?.get(url);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    let response = request.send().await.map_err(request_error)?;
    match response.status() {
        s if s.is_success() => Ok(response),
        s => Err(UpdateError::Status(s.as_u16())),
    }
}

/// `Ok(None)` means the running build is already the newest eligible release.
pub async fn check_for_update(options: CheckOptions) -> Result<Option<UpdateRelease>, UpdateError> {
    let current = SemVer::parse(
        options
            .current_version
            .as_deref()
            .unwrap_or(current_version()),
    )?;
    let kind = match options.kind {
        Some(kind) => kind,
        None => InstallKind::detect()?,
    };
    let url = options
        .api_url
        .clone()
        .or_else(|| {
            std::env::var("SKYLATCH_UPDATE_API_URL")
                .ok()
                .filter(|u| !u.is_empty())
        })
        .unwrap_or_else(|| {
            if options.allow_prerelease {
                DEFAULT_LIST_URL
            } else {
                DEFAULT_LATEST_URL
            }
            .into()
        });
    let doc = spawn_http(async move {
        let response = get(&url, Some(API_TIMEOUT)).await?;
        let bytes = response.bytes().await.map_err(request_error)?;
        serde_json::from_slice::<ReleaseDoc>(&bytes)
            .map_err(|e| UpdateError::Metadata(e.to_string()))
    })
    .await?;
    pick_release(doc, &current, &options, kind)
}

/// Digest of `name` in `sha256sum` output, matched on the whole file name.
pub fn parse_checksums(text: &str, name: &str) -> Result<String, UpdateError> {
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some((digest, file)) = line.split_once(char::is_whitespace) else {
            return Err(UpdateError::InvalidChecksums(format!(
                "malformed line {line:?}"
            )));
        };
        let file = file.trim_start();
        if file.strip_prefix('*').unwrap_or(file) != name {
            continue;
        }
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(UpdateError::InvalidChecksums(format!(
                "bad digest for {name}"
            )));
        }
        return Ok(digest.to_ascii_lowercase());
    }
    Err(UpdateError::ChecksumMissing(name.to_owned()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadProgress {
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct VerifiedPackage {
    pub path: PathBuf,
    pub version: String,
    pub kind: InstallKind,
    pub sha256: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn download_and_verify(
    release: &UpdateRelease,
    dest_dir: &Path,
    on_progress: impl Fn(DownloadProgress) + Send + 'static,
) -> Result<VerifiedPackage, UpdateError> {
    let name = release.package.name.clone();
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return Err(UpdateError::InvalidAssetName(name));
    }
    fs::create_dir_all(dest_dir)?;
    let release = release.clone();
    let dest = dest_dir.join(&name);
    let part = dest_dir.join(format!("{name}.part"));
    spawn_http(async move {
        let sums = get(&release.checksums.url, Some(API_TIMEOUT)).await?;
        let sums = sums.text().await.map_err(request_error)?;
        let expected = parse_checksums(&sums, &name)?;
        let result = stream_to(&release.package.url, &part, &expected, on_progress).await;
        if result.is_err() {
            let _ = fs::remove_file(&part);
        }
        let actual = result?;
        fs::rename(&part, &dest)?;
        Ok(VerifiedPackage {
            path: dest,
            version: release.version,
            kind: release.kind,
            sha256: actual,
        })
    })
    .await
}

async fn stream_to(
    url: &str,
    part: &Path,
    expected: &str,
    on_progress: impl Fn(DownloadProgress),
) -> Result<String, UpdateError> {
    let mut response = get(url, None).await?;
    let total_bytes = response.content_length();
    let mut file = fs::File::create(part)?;
    let mut hash = Context::new(&SHA256);
    let mut downloaded_bytes = 0u64;
    while let Some(chunk) = tokio::time::timeout(CHUNK_TIMEOUT, response.chunk())
        .await
        .map_err(|_| UpdateError::Request("download stalled".into()))?
        .map_err(request_error)?
    {
        hash.update(&chunk);
        file.write_all(&chunk)?;
        downloaded_bytes += chunk.len() as u64;
        on_progress(DownloadProgress {
            downloaded_bytes,
            total_bytes,
        });
    }
    file.sync_all()?;
    drop(file);
    let actual = hex(hash.finish().as_ref());
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch {
            expected: expected.to_owned(),
            actual,
        });
    }
    Ok(actual)
}

#[derive(Debug, Default, Clone)]
pub struct ApplyOptions {
    /// Overrides `std::env::current_exe()`.
    pub current_exe: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// A detached helper finishes the swap once this process exits; stop `skylatch-agent` first.
    RestartRequired,
    ManualInstall {
        package_path: PathBuf,
        command_hint: String,
    },
}

pub fn apply_update(
    package: &VerifiedPackage,
    options: &ApplyOptions,
) -> Result<ApplyOutcome, UpdateError> {
    let exe = match &options.current_exe {
        Some(exe) => exe.clone(),
        None => std::env::current_exe()?,
    };
    match package.kind {
        InstallKind::MacApp => apply_macos(package, &exe),
        InstallKind::WindowsInstaller => {
            let mut setup = Command::new(&package.path);
            setup.args([
                "/SILENT",
                "/CLOSEAPPLICATIONS",
                "/RESTARTAPPLICATIONS",
                "/SUPPRESSMSGBOXES",
            ]);
            spawn_detached(setup)?;
            Ok(ApplyOutcome::RestartRequired)
        }
        InstallKind::WindowsPortable => apply_windows_portable(package, &exe),
        InstallKind::LinuxDeb => Ok(ApplyOutcome::ManualInstall {
            package_path: package.path.clone(),
            command_hint: format!("sudo dpkg -i '{}'", package.path.display()),
        }),
    }
}

fn spawn_detached(mut command: Command) -> Result<(), UpdateError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    #[cfg(windows)]
    {
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::os::windows::process::CommandExt::creation_flags(
            &mut command,
            DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
        );
    }
    command.spawn()?;
    Ok(())
}

fn run(
    program: &'static str,
    args: &[&std::ffi::OsStr],
) -> Result<std::process::Output, UpdateError> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(UpdateError::Command {
            program,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// `*.app` root when `exe` is `<app>/Contents/MacOS/<bin>`.
pub fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    (macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app")
        .then(|| app.to_owned())
}

/// `None` for ad-hoc or unsigned output (`TeamIdentifier=not set`).
pub fn team_identifier(codesign_output: &str) -> Option<String> {
    codesign_output
        .lines()
        .find_map(|line| line.trim().strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|id| !id.is_empty() && *id != "not set")
        .map(str::to_owned)
}

/// An installed app with a real Team ID only accepts a package from the same team.
pub fn check_team(installed: Option<&str>, incoming: Option<&str>) -> Result<(), UpdateError> {
    match installed {
        Some(team) if incoming != Some(team) => Err(UpdateError::SignatureMismatch {
            expected: Some(team.to_owned()),
            actual: incoming.map(str::to_owned),
        }),
        _ => Ok(()),
    }
}

fn app_team(app: &Path) -> Result<Option<String>, UpdateError> {
    let output = Command::new("codesign")
        .args(["-dv", "--verbose=4"])
        .arg(app)
        .output()?;
    let text = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && !text.contains("not signed at all") {
        return Err(UpdateError::Command {
            program: "codesign",
            detail: text.trim().to_owned(),
        });
    }
    Ok(team_identifier(&text))
}

struct Mount(PathBuf);

impl Mount {
    fn attach(dmg: &Path, point: PathBuf) -> Result<Self, UpdateError> {
        fs::create_dir_all(&point)?;
        let mount = Self(point);
        run(
            "hdiutil",
            &[
                "attach".as_ref(),
                "-nobrowse".as_ref(),
                "-readonly".as_ref(),
                "-mountpoint".as_ref(),
                mount.0.as_os_str(),
                dmg.as_os_str(),
            ],
        )?;
        Ok(mount)
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-force"])
            .arg(&self.0)
            .output();
        let _ = fs::remove_dir(&self.0);
    }
}

fn apply_macos(package: &VerifiedPackage, exe: &Path) -> Result<ApplyOutcome, UpdateError> {
    let target = app_bundle(exe).ok_or(UpdateError::NotAppBundle)?;
    let work = target.parent().ok_or(UpdateError::NotAppBundle)?;
    let pid = std::process::id();
    let staged = work.join(format!(".Skylatch-update-{pid}.app"));
    let script = work.join(format!(".skylatch-update-{pid}.sh"));
    {
        let download_dir = package.path.parent().unwrap_or(work);
        let mount = Mount::attach(
            &package.path,
            download_dir.join(format!(".skylatch-mnt-{pid}")),
        )?;
        let new_app = mount.0.join("Skylatch.app");
        if !new_app.is_dir() {
            return Err(UpdateError::BadPackage(
                "Skylatch.app not found in disk image".into(),
            ));
        }
        run(
            "codesign",
            &[
                "--verify".as_ref(),
                "--deep".as_ref(),
                "--strict".as_ref(),
                new_app.as_os_str(),
            ],
        )?;
        check_team(
            app_team(&target)?.as_deref(),
            app_team(&new_app)?.as_deref(),
        )?;
        let _ = fs::remove_dir_all(&staged);
        if let Err(error) = run("ditto", &[new_app.as_os_str(), staged.as_os_str()]) {
            let _ = fs::remove_dir_all(&staged);
            return Err(error);
        }
    }
    fs::write(&script, MAC_SCRIPT)?;
    let mut helper = Command::new("nohup");
    helper
        .args(["/bin/sh"])
        .arg(&script)
        .arg(pid.to_string())
        .arg(&target)
        .arg(&staged);
    spawn_detached(helper)?;
    Ok(ApplyOutcome::RestartRequired)
}

fn apply_windows_portable(
    package: &VerifiedPackage,
    exe: &Path,
) -> Result<ApplyOutcome, UpdateError> {
    let target = exe
        .parent()
        .ok_or_else(|| UpdateError::BadPackage("no install directory".into()))?;
    let pid = std::process::id();
    let stage = target.join(format!(".skylatch-update-{pid}"));
    let _ = fs::remove_dir_all(&stage);
    let mut expand = Command::new("powershell");
    expand
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Expand-Archive -LiteralPath $args[0] -DestinationPath $args[1] -Force",
        ])
        .arg(&package.path)
        .arg(&stage);
    let output = expand.output()?;
    if !output.status.success() {
        let _ = fs::remove_dir_all(&stage);
        return Err(UpdateError::Command {
            program: "powershell",
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    let source = [stage.clone()]
        .into_iter()
        .chain(fs::read_dir(&stage)?.flatten().map(|e| e.path()))
        .find(|dir| dir.join("skylatch-ui.exe").is_file());
    let Some(source) = source else {
        let _ = fs::remove_dir_all(&stage);
        return Err(UpdateError::BadPackage(
            "skylatch-ui.exe not found in archive".into(),
        ));
    };
    let script = stage.with_extension("ps1");
    fs::write(&script, WIN_SCRIPT)?;
    let mut helper = Command::new("powershell");
    helper
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .arg(pid.to_string())
        .arg(target)
        .arg(&source)
        .arg(&stage);
    spawn_detached(helper)?;
    Ok(ApplyOutcome::RestartRequired)
}

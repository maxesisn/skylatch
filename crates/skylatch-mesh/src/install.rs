//! The official EasyTier release Skylatch is tested with, and its verified installation.
//! Binaries are fetched at install time, never vendored.

use crate::detect::EasyTierBinaries;
use std::io::{self, Read};
use std::path::PathBuf;

pub const ACCEPTED_VERSION: &str = "2.6.4";
const RELEASES: &str = "https://github.com/EasyTier/EasyTier/releases/download";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EasyTierTarget {
    MacosAarch64,
    WindowsX86_64,
    LinuxX86_64,
}

impl EasyTierTarget {
    pub fn current() -> Option<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => Some(Self::MacosAarch64),
            ("windows", "x86_64") => Some(Self::WindowsX86_64),
            ("linux", "x86_64") => Some(Self::LinuxX86_64),
            _ => None,
        }
    }

    fn archive_dir(self) -> &'static str {
        match self {
            Self::MacosAarch64 => "easytier-macos-aarch64",
            Self::WindowsX86_64 => "easytier-windows-x86_64",
            Self::LinuxX86_64 => "easytier-linux-x86_64",
        }
    }

    /// SHA-256 of the official `v2.6.4` release archives, as published by GitHub.
    fn sha256(self) -> &'static str {
        match self {
            Self::MacosAarch64 => {
                "4be1882d1aa36d31c1d6ba0596f2cf8a097e371f8da124212324b2e0f8df7e4b"
            }
            Self::WindowsX86_64 => {
                "27af91e270e554709b048bd32327fefd2dfce5062ae1e8701af7550c6f525f84"
            }
            Self::LinuxX86_64 => "61b659eaedba658fa66fe47d17e1426cdd77e5d02fa15fed447bb4357c09dfd6",
        }
    }

    fn files(self) -> &'static [&'static str] {
        match self {
            Self::WindowsX86_64 => &[
                "easytier-core.exe",
                "easytier-cli.exe",
                "wintun.dll",
                "Packet.dll",
                "WinDivert64.sys",
            ],
            _ => &["easytier-core", "easytier-cli"],
        }
    }

    fn exe_suffix(self) -> &'static str {
        if self == Self::WindowsX86_64 {
            ".exe"
        } else {
            ""
        }
    }
}

/// Where Skylatch installs EasyTier for its system service: writable only by administrators.
pub fn system_install_dir(target: EasyTierTarget) -> PathBuf {
    match target {
        EasyTierTarget::MacosAarch64 => {
            PathBuf::from("/Library/Application Support/Skylatch/easytier")
        }
        EasyTierTarget::WindowsX86_64 => std::env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
            .join("Skylatch")
            .join("EasyTier"),
        EasyTierTarget::LinuxX86_64 => PathBuf::from("/opt/skylatch/easytier"),
    }
}

/// A mirror or test archive used instead of the official release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomSource {
    pub url: String,
    pub sha256: String,
    pub install_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadSpec {
    pub version: String,
    pub target: EasyTierTarget,
    pub url: String,
    pub sha256: String,
    /// Top-level directory inside the zip archive.
    pub archive_dir: String,
    pub files: Vec<String>,
    pub install_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("archive checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
    #[error("invalid EasyTier archive: {0}")]
    Archive(String),
    #[error("EasyTier archive lacks {0}")]
    MissingFile(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The download, checksum and install location of EasyTier for `target`.
pub fn install_or_download_spec(
    target: EasyTierTarget,
    custom: Option<CustomSource>,
) -> DownloadSpec {
    let archive_dir = target.archive_dir();
    let official = format!("{RELEASES}/v{ACCEPTED_VERSION}/{archive_dir}-v{ACCEPTED_VERSION}.zip");
    let (url, sha256, install_dir) = match custom {
        Some(source) => (
            source.url,
            source.sha256.to_ascii_lowercase(),
            source
                .install_dir
                .unwrap_or_else(|| system_install_dir(target)),
        ),
        None => (official, target.sha256().into(), system_install_dir(target)),
    };
    DownloadSpec {
        version: ACCEPTED_VERSION.into(),
        target,
        url,
        sha256,
        archive_dir: archive_dir.into(),
        files: target.files().iter().map(|f| f.to_string()).collect(),
        install_dir,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

impl DownloadSpec {
    pub fn binaries(&self) -> EasyTierBinaries {
        let suffix = self.target.exe_suffix();
        EasyTierBinaries {
            core: Some(self.install_dir.join(format!("easytier-core{suffix}"))),
            cli: Some(self.install_dir.join(format!("easytier-cli{suffix}"))),
        }
    }

    pub fn verify(&self, archive: &[u8]) -> Result<(), InstallError> {
        let actual = sha256_hex(archive);
        if actual != self.sha256 {
            return Err(InstallError::Checksum {
                expected: self.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }

    /// Verifies `archive` and extracts the EasyTier files into `install_dir`.
    pub fn install_from_archive(&self, archive: &[u8]) -> Result<EasyTierBinaries, InstallError> {
        self.verify(archive)?;
        let mut zip = zip::ZipArchive::new(io::Cursor::new(archive))
            .map_err(|e| InstallError::Archive(e.to_string()))?;
        let mut contents = Vec::new();
        for file in &self.files {
            let name = format!("{}/{file}", self.archive_dir);
            let mut entry = zip
                .by_name(&name)
                .map_err(|_| InstallError::MissingFile(name.clone()))?;
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            contents.push((file, bytes));
        }
        std::fs::create_dir_all(&self.install_dir)?;
        for (file, bytes) in contents {
            let path = self.install_dir.join(file);
            let tmp = self.install_dir.join(format!(".{file}.tmp"));
            std::fs::write(&tmp, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
            }
            std::fs::rename(&tmp, &path)?;
        }
        Ok(self.binaries())
    }

    /// Fetches `url` with `fetch` and installs it.
    pub fn install_with(
        &self,
        fetch: impl FnOnce(&str) -> io::Result<Vec<u8>>,
    ) -> Result<EasyTierBinaries, InstallError> {
        let archive = fetch(&self.url)?;
        self.install_from_archive(&archive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(dir: &str, files: &[&str]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        for file in files {
            zip.start_file(
                format!("{dir}/{file}"),
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(format!("binary {file}").as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn official_specs_point_at_the_accepted_release() {
        let mac = install_or_download_spec(EasyTierTarget::MacosAarch64, None);
        assert_eq!(
            mac.url,
            "https://github.com/EasyTier/EasyTier/releases/download/v2.6.4/easytier-macos-aarch64-v2.6.4.zip"
        );
        assert_eq!(mac.sha256.len(), 64);
        let win = install_or_download_spec(EasyTierTarget::WindowsX86_64, None);
        assert!(win.files.contains(&"wintun.dll".to_string()));
        assert!(win.binaries().core.unwrap().ends_with("easytier-core.exe"));
        let linux = install_or_download_spec(EasyTierTarget::LinuxX86_64, None);
        assert_eq!(linux.install_dir, PathBuf::from("/opt/skylatch/easytier"));
    }

    #[test]
    fn installs_only_verified_archives() {
        let dir = std::env::temp_dir().join(format!(
            "skylatch-install-{}",
            skylatch_proto::credentials::random_token().unwrap()
        ));
        let bytes = archive(
            "easytier-linux-x86_64",
            &["easytier-core", "easytier-cli", "easytier-web"],
        );
        let spec = install_or_download_spec(
            EasyTierTarget::LinuxX86_64,
            Some(CustomSource {
                url: "https://mirror.example.com/et.zip".into(),
                sha256: sha256_hex(&bytes).to_uppercase(),
                install_dir: Some(dir.clone()),
            }),
        );
        let mut requested = String::new();
        let installed = spec
            .install_with(|url| {
                requested = url.into();
                Ok(bytes.clone())
            })
            .unwrap();
        assert_eq!(requested, "https://mirror.example.com/et.zip");
        let core = installed.core.unwrap();
        assert_eq!(std::fs::read(&core).unwrap(), b"binary easytier-core");
        assert!(!dir.join("easytier-web").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&core).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }

        let mut tampered = bytes.clone();
        tampered[0] ^= 1;
        assert!(matches!(
            spec.install_from_archive(&tampered),
            Err(InstallError::Checksum { .. })
        ));
        let partial = archive("easytier-linux-x86_64", &["easytier-core"]);
        let partial_spec = DownloadSpec {
            sha256: sha256_hex(&partial),
            ..spec.clone()
        };
        assert!(matches!(
            partial_spec.install_from_archive(&partial),
            Err(InstallError::MissingFile(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

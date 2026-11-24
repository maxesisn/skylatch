// SPDX-License-Identifier: Apache-2.0

//! Embeds the Lock-Bolt icon (resource id 101) into `skylatch-ui.exe` on Windows.
//!
//! `skylatch-ui.rc` is compiled with the Windows SDK resource compiler, `rc.exe`: the one on
//! `PATH` (a Developer Command Prompt / `vcvars64.bat` puts it there) or else the newest one
//! under `Windows Kits\10\bin`. The resulting `.res` is handed to the MSVC linker. Other
//! platforms and non-MSVC targets are left alone.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=skylatch-ui.rc");
    println!("cargo:rerun-if-changed=../../assets/icon/skylatch.ico");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let res = out_dir.join("skylatch-ui.res");

    let Some(rc) = find_rc() else {
        println!(
            "cargo:warning=rc.exe (Windows SDK) not found: the application icon is not embedded"
        );
        return;
    };
    let status = Command::new(&rc)
        .current_dir(&manifest)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg("skylatch-ui.rc")
        .status();
    match status {
        Ok(s) if s.success() => {
            println!("cargo:rustc-link-arg-bins={}", res.display());
        }
        Ok(s) => panic!("rc.exe failed to compile skylatch-ui.rc ({s})"),
        Err(e) => panic!("could not run {}: {e}", rc.display()),
    }
}

/// `rc.exe` from `PATH`, else the newest x64 one of the installed Windows 10/11 SDKs.
fn find_rc() -> Option<PathBuf> {
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let candidate = dir.join("rc.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let roots = ["ProgramFiles(x86)", "ProgramFiles"]
        .into_iter()
        .filter_map(env::var_os)
        .map(|p| Path::new(&p).join("Windows Kits").join("10").join("bin"));
    for bin in roots {
        let Ok(entries) = std::fs::read_dir(&bin) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.join("x64").join("rc.exe").is_file())
            .collect();
        // SDK folders are named like 10.0.26100.0: a numeric sort puts the newest last
        versions.sort_by_key(|p| version_key(p));
        if let Some(newest) = versions.pop() {
            return Some(newest.join("x64").join("rc.exe"));
        }
    }
    None
}

fn version_key(dir: &Path) -> Vec<u32> {
    dir.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

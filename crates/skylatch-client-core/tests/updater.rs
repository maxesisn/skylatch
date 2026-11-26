use axum::{routing::get, Json, Router};
use ring::digest::{digest, SHA256};
use serde_json::{json, Value};
use skylatch_client_core::updater::{
    app_bundle, apply_update, check_for_update, check_for_update_with_notes, check_team,
    download_and_verify, parse_checksums, select_assets, team_identifier, ApplyOptions,
    ApplyOutcome, Asset, CheckOptions, DownloadProgress, InstallKind, SemVer, UpdateError,
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn v(text: &str) -> SemVer {
    SemVer::parse(text).unwrap()
}

fn sha(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("skylatch-updater-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn semver_orders_per_spec() {
    let ordered = [
        "0.9.9",
        "1.0.0-alpha",
        "1.0.0-alpha.1",
        "1.0.0-alpha.beta",
        "1.0.0-beta",
        "1.0.0-beta.2",
        "1.0.0-beta.11",
        "1.0.0-rc.1",
        "1.0.0",
        "1.0.1",
        "1.1.0",
        "2.0.0",
        "10.0.0",
    ];
    for pair in ordered.windows(2) {
        assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
    }
    assert_eq!(v("v1.2.3"), v("1.2.3"));
    assert_eq!(v("1.2.3+build.5"), v("1.2.3"));
    assert!(v("1.0.0-rc.1").is_prerelease() && !v("1.0.0").is_prerelease());
}

#[test]
fn semver_rejects_garbage() {
    for bad in [
        "",
        "1",
        "1.2",
        "1.2.3.4",
        "01.2.3",
        "1.2.x",
        "a.b.c",
        "1.2.3-",
        "1.2.3-a..b",
        "1.2.3-é",
        "vv1.2.3",
        "-1.2.3",
    ] {
        assert!(
            matches!(SemVer::parse(bad), Err(UpdateError::InvalidVersion(_))),
            "{bad}"
        );
    }
}

#[test]
fn checksums_parse_exactly() {
    let text = format!(
        "\n{DIGEST}  other-app.dmg\n{} *app.dmg\r\n{DIGEST}  my-app.dmg\n\n",
        "B".repeat(64)
    );
    assert_eq!(parse_checksums(&text, "app.dmg").unwrap(), "b".repeat(64));
    assert_eq!(parse_checksums(&text, "my-app.dmg").unwrap(), DIGEST);
    assert!(matches!(
        parse_checksums(&text, "pp.dmg"),
        Err(UpdateError::ChecksumMissing(_))
    ));
    assert!(matches!(
        parse_checksums("abc  app.dmg", "app.dmg"),
        Err(UpdateError::InvalidChecksums(_))
    ));
    let non_hex = format!("{}  app.dmg", "g".repeat(64));
    assert!(matches!(
        parse_checksums(&non_hex, "app.dmg"),
        Err(UpdateError::InvalidChecksums(_))
    ));
    assert!(matches!(
        parse_checksums("", "app.dmg"),
        Err(UpdateError::ChecksumMissing(_))
    ));
}

fn assets(version: &str) -> Vec<Asset> {
    [
        format!("Skylatch-{version}-macos-arm64.dmg"),
        format!("Skylatch-{version}-windows-x64-setup.exe"),
        format!("Skylatch-{version}-windows-x64-portable.zip"),
        format!("skylatch-{version}-linux-x86_64.deb"),
        format!("Skylatch-{version}-android-arm64.apk"),
        "SHA256SUMS".into(),
    ]
    .into_iter()
    .map(|name| Asset {
        url: format!("http://x/{name}"),
        name,
        size: None,
    })
    .collect()
}

#[test]
fn selects_asset_per_install_kind() {
    for (kind, name) in [
        (InstallKind::MacApp, "Skylatch-0.2.0-macos-arm64.dmg"),
        (
            InstallKind::WindowsInstaller,
            "Skylatch-0.2.0-windows-x64-setup.exe",
        ),
        (
            InstallKind::WindowsPortable,
            "Skylatch-0.2.0-windows-x64-portable.zip",
        ),
        (InstallKind::LinuxDeb, "skylatch-0.2.0-linux-x86_64.deb"),
        (InstallKind::AndroidApk, "Skylatch-0.2.0-android-arm64.apk"),
    ] {
        let (package, sums) = select_assets(kind, "0.2.0", &assets("0.2.0")).unwrap();
        assert_eq!(
            (package.name.as_str(), sums.name.as_str()),
            (name, "SHA256SUMS")
        );
    }
}

#[test]
fn missing_assets_fail_loudly() {
    let mut list = assets("0.2.0");
    list.retain(|a| a.name != "SHA256SUMS");
    assert!(matches!(
        select_assets(InstallKind::MacApp, "0.2.0", &list),
        Err(UpdateError::MissingAsset(n)) if n == "SHA256SUMS"
    ));
    let mut list = assets("0.2.0");
    list.retain(|a| !a.name.ends_with(".dmg"));
    assert!(matches!(
        select_assets(InstallKind::MacApp, "0.2.0", &list),
        Err(UpdateError::MissingAsset(_))
    ));
    assert!(select_assets(InstallKind::MacApp, "0.3.0", &assets("0.2.0")).is_err());
}

#[test]
fn windows_install_kind_follows_uninstaller() {
    let dir = temp_dir("kind");
    fs::write(dir.join("skylatch-ui.exe"), b"").unwrap();
    assert_eq!(
        InstallKind::windows_in_dir(&dir),
        InstallKind::WindowsPortable
    );
    fs::write(dir.join("unins000.exe"), b"").unwrap();
    assert_eq!(
        InstallKind::windows_in_dir(&dir),
        InstallKind::WindowsInstaller
    );
    fs::remove_file(dir.join("unins000.exe")).unwrap();
    fs::write(dir.join("UNINS001.EXE"), b"").unwrap();
    assert_eq!(
        InstallKind::windows_in_dir(&dir),
        InstallKind::WindowsInstaller
    );
    assert_eq!(
        InstallKind::windows_in_dir(&dir.join("missing")),
        InstallKind::WindowsPortable
    );
}

#[test]
fn macos_team_id_checks() {
    let signed = "Executable=/Applications/Skylatch.app/Contents/MacOS/skylatch\nAuthority=Developer ID Application: Bingchen Wu (7U57KTJH9C)\nTeamIdentifier=7U57KTJH9C\nSealed Resources version=2\n";
    let adhoc = "Signature=adhoc\nTeamIdentifier=not set\n";
    assert_eq!(team_identifier(signed).as_deref(), Some("7U57KTJH9C"));
    assert_eq!(team_identifier(adhoc), None);
    assert_eq!(team_identifier("no signature info"), None);

    assert!(check_team(Some("7U57KTJH9C"), Some("7U57KTJH9C")).is_ok());
    assert!(matches!(
        check_team(Some("7U57KTJH9C"), Some("OTHER12345")),
        Err(UpdateError::SignatureMismatch { .. })
    ));
    assert!(matches!(
        check_team(Some("7U57KTJH9C"), None),
        Err(UpdateError::SignatureMismatch { actual: None, .. })
    ));
    assert!(check_team(None, None).is_ok());
    assert!(check_team(None, Some("7U57KTJH9C")).is_ok());
}

#[test]
fn app_bundle_detection() {
    assert_eq!(
        app_bundle("/Applications/Skylatch.app/Contents/MacOS/skylatch-ui".as_ref()),
        Some(PathBuf::from("/Applications/Skylatch.app"))
    );
    assert_eq!(
        app_bundle("/work/target/release/skylatch-ui".as_ref()),
        None
    );
    assert_eq!(
        app_bundle("/Applications/Skylatch.app/Contents/skylatch-ui".as_ref()),
        None
    );
}

struct Server {
    base: String,
    package: Vec<u8>,
}

async fn serve(prerelease_tag: Option<&str>, tamper: bool) -> Server {
    let package: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let served = if tamper {
        let mut bytes = package.clone();
        bytes[1000] ^= 1;
        bytes
    } else {
        package.clone()
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let kind = InstallKind::LinuxDeb;
    let name = kind.asset_name("0.2.0");
    let sums = format!("{}  {name}\n{DIGEST}  {name}.sig\n", sha(&package));
    let release = |tag: &str, pre: bool, draft: bool| -> Value {
        json!({
            "tag_name": tag, "prerelease": pre, "draft": draft,
            "html_url": format!("{base}/releases/{tag}"), "body": format!("notes for {tag}"),
            "assets": assets(tag.trim_start_matches('v')).into_iter().map(|a| json!({
                "name": a.name, "size": 1, "browser_download_url": format!("{base}/dl/{}", a.name)
            })).collect::<Vec<_>>()
        })
    };
    let latest = release("v0.2.0", false, false);
    let mut all = vec![
        release("v0.1.5", false, false),
        latest.clone(),
        release("v9.9.9", false, true),
    ];
    if let Some(tag) = prerelease_tag {
        all.push(release(tag, true, false));
    }
    let app = Router::new()
        .route(
            "/latest",
            get({
                let latest = latest.clone();
                move || async move { Json(latest) }
            }),
        )
        .route("/list", get(move || async move { Json(all) }))
        .route("/dl/SHA256SUMS", get(move || async move { sums }))
        .route(&format!("/dl/{name}"), get(move || async move { served }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server { base, package }
}

fn options(server: &Server, path: &str, current: &str) -> CheckOptions {
    CheckOptions {
        api_url: Some(format!("{}{path}", server.base)),
        current_version: Some(current.into()),
        kind: Some(InstallKind::LinuxDeb),
        allow_prerelease: false,
    }
}

#[tokio::test]
async fn check_reports_new_current_and_prerelease() {
    let server = serve(Some("v0.3.0-rc.1"), false).await;
    let found = check_for_update(options(&server, "/latest", "0.1.0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.version, "0.2.0");
    assert_eq!(found.package.name, "skylatch-0.2.0-linux-x86_64.deb");

    let mut android = options(&server, "/latest", "0.1.0");
    android.kind = Some(InstallKind::AndroidApk);
    let (found, notes) = check_for_update_with_notes(android).await.unwrap().unwrap();
    assert_eq!(found.package.name, "Skylatch-0.2.0-android-arm64.apk");
    assert_eq!(found.checksums.name, "SHA256SUMS");
    assert_eq!(
        notes.url.as_deref(),
        Some(format!("{}/releases/v0.2.0", server.base).as_str())
    );
    assert_eq!(notes.body.as_deref(), Some("notes for v0.2.0"));

    assert_eq!(
        check_for_update(options(&server, "/latest", "v0.2.0"))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        check_for_update(options(&server, "/latest", "0.3.0"))
            .await
            .unwrap(),
        None
    );

    let list = check_for_update(options(&server, "/list", "0.1.0"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(list.version, "0.2.0", "draft and prerelease skipped");

    let mut pre = options(&server, "/list", "0.1.0");
    pre.allow_prerelease = true;
    assert_eq!(
        check_for_update(pre).await.unwrap().unwrap().version,
        "0.3.0-rc.1"
    );

    let missing = options(&server, "/nope", "0.1.0");
    assert!(matches!(
        check_for_update(missing).await,
        Err(UpdateError::Status(404))
    ));
}

#[tokio::test]
async fn download_streams_verifies_and_renames() {
    let server = serve(None, false).await;
    let release = check_for_update(options(&server, "/latest", "0.1.0"))
        .await
        .unwrap()
        .unwrap();
    let dir = temp_dir("ok");
    let seen = Arc::new(Mutex::new(Vec::<DownloadProgress>::new()));
    let sink = seen.clone();
    let package = download_and_verify(&release, &dir, move |p| sink.lock().unwrap().push(p))
        .await
        .unwrap();

    assert_eq!(fs::read(&package.path).unwrap(), server.package);
    assert_eq!(package.sha256, sha(&server.package));
    assert!(!dir.join("skylatch-0.2.0-linux-x86_64.deb.part").exists());
    let seen = seen.lock().unwrap();
    assert!(seen.len() > 1);
    assert!(seen
        .windows(2)
        .all(|w| w[0].downloaded_bytes < w[1].downloaded_bytes));
    assert_eq!(
        seen.last().unwrap().downloaded_bytes,
        server.package.len() as u64
    );
    assert_eq!(
        seen.last().unwrap().total_bytes,
        Some(server.package.len() as u64)
    );

    let outcome = apply_update(&package, &ApplyOptions::default()).unwrap();
    assert!(matches!(
        outcome,
        ApplyOutcome::ManualInstall { ref package_path, ref command_hint }
            if *package_path == package.path && command_hint.starts_with("sudo dpkg -i ")
    ));
}

#[tokio::test]
async fn tampered_download_is_rejected_and_cleaned() {
    let server = serve(None, true).await;
    let release = check_for_update(options(&server, "/latest", "0.1.0"))
        .await
        .unwrap()
        .unwrap();
    let dir = temp_dir("bad");
    let error = download_and_verify(&release, &dir, |_| {})
        .await
        .unwrap_err();
    assert!(
        matches!(error, UpdateError::ChecksumMismatch { .. }),
        "{error}"
    );
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[tokio::test]
async fn unsafe_asset_name_is_refused() {
    let mut release = {
        let server = serve(None, false).await;
        check_for_update(options(&server, "/latest", "0.1.0"))
            .await
            .unwrap()
            .unwrap()
    };
    release.package.name = "../evil".into();
    let error = download_and_verify(&release, &temp_dir("name"), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(error, UpdateError::InvalidAssetName(_)));
}

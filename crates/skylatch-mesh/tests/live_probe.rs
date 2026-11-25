//! Read-only probes of the real machine's EasyTier. Never starts, stops or reconfigures anything.
//!
//! `cargo test -p skylatch-mesh --test live_probe -- --ignored --nocapture`; set
//! `SKYLATCH_MESH_EXPECT` to a presence state such as `external_config_server` to assert it.

use skylatch_mesh::{
    detect_conflicts, detect_presence, find_binaries, profile_toml, query_status, EasyTierPresence,
    MeshPaths, MeshRouteKind,
};
use skylatch_proto::mesh::MeshVault;
use std::process::Command;

#[test]
#[ignore = "probes this machine's EasyTier"]
fn presence_status_and_conflicts() {
    let binaries = find_binaries(&[]);
    println!("binaries: {binaries:?}");
    let paths: Vec<MeshPaths> = MeshPaths::user_default().into_iter().collect();
    let presence = detect_presence(&paths, &binaries);
    println!(
        "presence: {}",
        serde_json::to_string_pretty(&presence).unwrap()
    );
    for conflict in detect_conflicts() {
        println!("conflict: {}", serde_json::to_string(&conflict).unwrap());
    }
    if let Ok(expected) = std::env::var("SKYLATCH_MESH_EXPECT") {
        let state = serde_json::to_value(&presence).unwrap()["state"].clone();
        assert_eq!(state, expected.as_str());
    }
    let portal = match &presence {
        EasyTierPresence::ManagedRunning { rpc_portal, .. }
        | EasyTierPresence::ExternalConfigServer { rpc_portal, .. } => Some(rpc_portal.clone()),
        EasyTierPresence::ExternalGuiOrManual { rpc_portal, .. } => rpc_portal.clone(),
        _ => None,
    };
    if let (Some(cli), Some(portal)) = (&binaries.cli, portal) {
        let status = query_status(cli, Some(&portal), None).unwrap();
        println!(
            "local {} {:?} nat {:?} version {:?}",
            status.local_hostname, status.local_ipv4, status.nat_type, status.version
        );
        for peer in &status.peers {
            println!(
                "  {:<20} {:<16} {:<10} {:>8} loss {:<6} {:?}",
                peer.hostname,
                peer.ipv4.as_deref().unwrap_or("-"),
                format!("{:?}", peer.route),
                peer.latency_ms.map_or("-".into(), |l| format!("{l:.2}ms")),
                peer.loss_rate.as_deref().unwrap_or("-"),
                peer.tunnel_proto
            );
        }
        assert!(status.local_ipv4.is_some());
        assert_eq!(
            status
                .peers
                .iter()
                .filter(|p| p.route == MeshRouteKind::Local)
                .count(),
            1
        );
    }
}

#[test]
#[ignore = "runs this machine's easytier-core --check-config"]
fn generated_toml_passes_easytier_check_config() {
    let Some(core) = find_binaries(&[]).core else {
        return;
    };
    let mut vault = MeshVault::new_default().unwrap();
    let mut second = vault.next_isolated_defaults().unwrap();
    second.custom_peers = vec!["udp://peer.example.com:11010".into()];
    vault.profiles.push(second);
    let dir = std::env::temp_dir().join(format!("skylatch-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut args = vec!["--check-config".to_string()];
    for (i, profile) in vault.profiles.iter().enumerate() {
        let text = profile_toml(
            profile,
            &["tcp://hub.example.com:11010".into()],
            "probe",
            i == 1,
        )
        .unwrap();
        let path = dir.join(format!("net-{}.toml", profile.id));
        std::fs::write(&path, text).unwrap();
        args.extend(["-c".into(), path.to_string_lossy().into_owned()]);
    }
    let output = Command::new(core).args(&args).output().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

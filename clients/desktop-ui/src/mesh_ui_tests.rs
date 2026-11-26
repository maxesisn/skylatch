// SPDX-License-Identifier: Apache-2.0

// Headless tests of the 连接与组网 drawer: both tabs, every EasyTier cell in both themes, the
// sign-out guard, the TOML import, the vault unlock and pairing codes that carry a mesh
// invite. Detection results are injected, so no test reads this machine's EasyTier.

use super::*;
use crate::mesh_ui::{
    Cell, DrawerTab, ImportIntro, MeshField, Modal, NetStatus, Probe, Vault, VaultSession,
};
use skylatch_mesh::{
    EasyTierPresence, MeshBindingMode, MeshPeerStatus, MeshRouteKind, MeshRuntimeStatus,
    MeshState,
};
use std::path::PathBuf;

fn status() -> MeshRuntimeStatus {
    let peer = |host: &str, ip: &str, route, ms| MeshPeerStatus {
        id: None,
        hostname: host.into(),
        ipv4: Some(ip.into()),
        route,
        latency_ms: ms,
        loss_rate: None,
        tunnel_proto: None,
        nat_type: None,
    };
    MeshRuntimeStatus {
        local_hostname: "studio".into(),
        local_ipv4: Some("10.126.1.20".into()),
        nat_type: Some("PortRestricted".into()),
        version: Some("2.6.4-8428a89d".into()),
        peers: vec![
            peer("studio", "10.126.1.20", MeshRouteKind::Local, None),
            peer("nas", "10.126.1.11", MeshRouteKind::P2p, Some(5.4)),
            peer("pve", "10.126.1.10", MeshRouteKind::Relay(2), Some(33.0)),
        ],
    }
}

fn managed_binding(no_tun: bool) -> MeshBindingMode {
    MeshBindingMode::Managed {
        active_profile_ids: vec!["default".into()],
        vault_version: Some(1),
        no_tun,
        rpc_portal: skylatch_mesh::MANAGED_RPC_PORTAL.into(),
    }
}

fn probe(presence: EasyTierPresence, bound: bool) -> Probe {
    let managed_ids = vec!["default".to_string()];
    let cell = Cell::resolve(&presence, bound, &managed_ids, true);
    let networks = match cell {
        Cell::NotInstalled | Cell::InstalledIdle | Cell::ManagedStopped => vec![],
        _ => vec![NetStatus {
            profile_id: cell.is_managed().then(|| "default".to_string()),
            status: Ok(status()),
        }],
    };
    Probe {
        presence,
        cell,
        networks,
        conflicts: vec![],
        state: MeshState {
            binding: if bound {
                managed_binding(false)
            } else {
                MeshBindingMode::Unbound
            },
            last_hub_url: None,
        },
        managed_ids,
        user_process: false,
    }
}

fn presences() -> Vec<(EasyTierPresence, bool)> {
    let dir = PathBuf::from("/tmp/skylatch/easytier");
    vec![
        (EasyTierPresence::NotInstalled, false),
        (
            EasyTierPresence::InstalledIdle {
                easytier_core: "/usr/local/bin/easytier-core".into(),
            },
            false,
        ),
        (
            EasyTierPresence::ExternalConfigServer {
                rpc_portal: "127.0.0.1:15888".into(),
                summary: "easytier-core configured by tcp://cfg.example.com:22020 (PID 371)"
                    .into(),
            },
            false,
        ),
        (
            EasyTierPresence::ExternalGuiOrManual {
                rpc_portal: Some("127.0.0.1:15888".into()),
                overlay_ips: vec![],
                summary: "easytier-core (PID 12)".into(),
            },
            false,
        ),
        (
            EasyTierPresence::ManagedRunning {
                rpc_portal: skylatch_mesh::MANAGED_RPC_PORTAL.into(),
                profiles_dir: dir.clone(),
                active_profile_ids: vec!["default".into()],
            },
            true,
        ),
        (
            EasyTierPresence::ManagedStopped {
                profiles_dir: dir,
                configured_profile_ids: vec!["default".into()],
            },
            true,
        ),
    ]
}

fn session() -> VaultSession {
    crate::mesh_ops::new_vault("correct horse battery").unwrap()
}

/// The drawer open on the 虚拟组网 tab with `probe` as the latest round (none is started).
fn on_mesh_tab(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext, probe: Probe) {
    view.update(cx, |v, cx| {
        v.open_drawer();
        v.set_drawer_tab(DrawerTab::Mesh);
        v.mesh.probe = Some(probe);
        v.mesh.probe_at = Some(Instant::now());
        cx.notify();
    });
    draw(cx);
}

/// Changes the view outside an event handler and lets it repaint.
fn change(view: &gpui::Entity<Proto>, cx: &mut VisualTestContext, f: impl FnOnce(&mut Proto)) {
    view.update(cx, |v, cx| {
        f(v);
        cx.notify();
    });
}

#[gpui::test]
fn the_drawer_switches_tabs_and_shows_what_the_hub_says(cx: &mut TestAppContext) {
    let hub = fake_hub();
    let (view, cx) = live_view(cx, &hub);
    view.update(cx, |v, _| {
        v.open_drawer();
        v.hubinfo.url = config::normalize_hub_url(&hub.ws_url());
        v.hubinfo.info = Some(skylatch_client_core::HubInfo {
            version: "0.1.0".into(),
            mode: "signaling-only".into(),
            easytier_peers: vec!["tcp://et.example.com:11010".into()],
            allow_signup: true,
            max_devices_per_account: None,
        });
        v.hubinfo.pending = None;
    });
    draw(cx);
    assert!(has_sel(cx, "hub-drawer") && has_sel(cx, "hub-info"));
    assert!(has_sel(cx, "account-seg"));
    assert_eq!(
        view.read_with(cx, |v, _| v.drawer.account),
        hub_ui::AccountMode::Token,
        "a configured token opens the Token segment"
    );
    change(&view, cx, |v| v.set_account_mode(hub_ui::AccountMode::Signup));
    assert!(has_sel(cx, "field-user") && has_sel(cx, "field-pass"));
    // a short signup password is refused before anything is sent
    view.update(cx, |v, _| {
        v.drawer.user = "max".into();
        v.drawer.pass = "short".into();
    });
    view.update(cx, |v, cx| v.save_and_connect(cx));
    assert!(view.read_with(cx, |v, _| v
        .drawer
        .error
        .as_deref()
        .is_some_and(|e| e.contains("8 位"))
        && !v.drawer.busy));

    click_sel(cx, "tab-mesh");
    assert_eq!(view.read_with(cx, |v, _| v.mesh.tab), DrawerTab::Mesh);
    assert!(has_sel(cx, "mesh-state"));
    click_sel(cx, "tab-server");
    assert_eq!(view.read_with(cx, |v, _| v.mesh.tab), DrawerTab::Server);
    cx.simulate_keystrokes("escape");
    assert!(!view.read_with(cx, |v, _| v.drawer.open));
}

// gpui keeps the debug bounds of earlier frames, so every situation gets a window of its own
// and only what is on screen is asserted, never what is missing.
#[gpui::test]
fn every_easytier_cell_renders_honest_actions_in_both_themes(cx: &mut TestAppContext) {
    let hub = fake_hub();
    for dark in [false, true] {
        for (presence, bound) in presences() {
            let (view, vcx) = live_view(cx, &hub);
            view.update(vcx, |v, _| {
                v.set_dark(dark);
                v.mesh.open(session());
            });
            let p = probe(presence, bound);
            let cell = p.cell;
            on_mesh_tab(&view, vcx, p);
            assert!(has_sel(vcx, "mesh-state"), "{cell:?}");
            let shown = ["mesh-install", "mesh-adopt", "mesh-run", "mesh-detach", "mesh-peers"]
                .map(|s| has_sel(vcx, s));
            let expected = match cell {
                Cell::NotInstalled | Cell::InstalledIdle => [true, false, false, false, false],
                // read-only: no action at all, only the live view
                Cell::ExternalConfigServer => [false, false, false, false, true],
                Cell::ExternalManual => [false, true, false, false, true],
                Cell::ManagedRunning => [false, false, true, true, true],
                Cell::ManagedStopped => [false, false, true, true, false],
            };
            assert_eq!(shown, expected, "{cell:?}, dark {dark}");
            assert!(has_sel(vcx, "mesh-save") && has_sel(vcx, "mesh-add"));
            if cell == Cell::ManagedRunning {
                // the audit shows the ciphertext fingerprint and the recovery key
                click_sel(vcx, "mesh-audit-toggle");
                assert!(has_sel(vcx, "mesh-audit"));
                click_sel(vcx, "copy-recovery");
                assert!(clipboard_text(vcx).is_some_and(|t| t.starts_with("SLK1-")));
            }
        }
    }
}

#[gpui::test]
fn several_networks_are_added_picked_and_saved_through_the_tab(cx: &mut TestAppContext) {
    let hub = fake_hub();
    let (view, cx) = live_view(cx, &hub);
    view.update(cx, |v, _| v.mesh.open(session()));
    on_mesh_tab(&view, cx, probe(EasyTierPresence::NotInstalled, false));
    assert!(has_sel(cx, "mesh-profile-0") && !has_sel(cx, "mesh-profile-1"));
    click_sel(cx, "mesh-add");
    assert!(has_sel(cx, "mesh-profile-1"));
    let id = view.read_with(cx, |v, _| {
        let d = v.mesh.draft.as_ref().unwrap();
        assert!(d.dirty);
        d.items[1].profile.id.clone()
    });
    // the new network's name takes Chinese from the input method
    view.update(cx, |v, _| v.focus_profile(&id, MeshField::Name));
    view.update(cx, |v, _| v.mesh.buf(MeshField::Name).unwrap().0.clear());
    draw(cx);
    ime_commit(&view, cx, "虚拟机");
    let name = view.read_with(cx, |v, _| v.mesh.text_of(MeshField::Name).map(String::from));
    assert_eq!(name.as_deref(), Some("虚拟机"));
    view.update(cx, |v, _| v.toggle_join(&id));
    assert_eq!(
        view.read_with(cx, |v, _| v.mesh.draft.as_ref().unwrap().joined().len()),
        2
    );
    // the fake hub has no vault endpoint: saving reports the failure and keeps the draft
    click_sel(cx, "mesh-save");
    pump_until(&view, cx, "the save attempt", |v| v.mesh.vault_rx.is_none());
    assert!(view.read_with(cx, |v, _| v.mesh.draft.as_ref().unwrap().dirty
        && v.mesh.notice.as_ref().is_some_and(|(_, err)| *err)));
    change(&view, cx, |v| v.remove_profile(&id));
    assert_eq!(
        view.read_with(cx, |v, _| v.mesh.draft.as_ref().unwrap().items.len()),
        1
    );
}

#[gpui::test]
fn signing_out_while_managed_asks_first_and_keeps_easytier_running(cx: &mut TestAppContext) {
    let hub = fake_hub();
    let (view, cx) = live_view(cx, &hub);
    let paths = skylatch_mesh::MeshPaths::new(std::env::temp_dir().join(format!(
        "skylatch-ui-guard-{}",
        skylatch_proto::credentials::random_token().unwrap()
    )));
    let s = session();
    crate::mesh_ops::apply_local(crate::mesh_ops::ApplyRequest {
        paths: paths.clone(),
        vault: s.vault.clone(),
        selected: vec!["default".into()],
        hub_peers: vec![],
        no_tun: true,
        vault_version: Some(1),
        hub_url: hub.ws_url(),
        start: false,
    })
    .unwrap();
    let mut p = probe(presences().remove(5).0, true);
    p.state.binding = managed_binding(true);
    view.update(cx, |v, _| {
        v.mesh_paths = Some(paths.clone());
        v.mesh.open(s);
    });
    on_mesh_tab(&view, cx, p);

    change(&view, cx, |v| v.sign_out());
    assert!(has_sel(cx, "mesh-guard"), "never a silent sign-out");
    assert!(view.read_with(cx, |v, _| v.eff.live()));
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |v, _| v.mesh.modal.is_none() && v.eff.live()));

    change(&view, cx, |v| v.sign_out());
    click_sel(cx, "guard-keep");
    pump_until(&view, cx, "the detach", |v| v.mesh.op.is_none());
    assert!(view.read_with(cx, |v, _| !v.eff.live() && v.onboarding_active()));
    assert_eq!(
        crate::mesh_ops::load_state(&paths).binding,
        MeshBindingMode::Unbound
    );
    assert!(
        paths.profiles_dir().join("net-default.toml").is_file(),
        "the network keeps its configuration"
    );
    assert!(view.read_with(cx, |v, _| matches!(v.mesh.vault, Vault::Unknown)));
    let _ = std::fs::remove_dir_all(&paths.app_dir);
}

#[gpui::test]
fn the_import_dialog_takes_pasted_toml_and_adds_a_network(cx: &mut TestAppContext) {
    let hub = fake_hub();
    let (view, cx) = live_view(cx, &hub);
    view.update(cx, |v, _| v.mesh.open(session()));
    on_mesh_tab(&view, cx, probe(EasyTierPresence::NotInstalled, false));
    click_sel(cx, "mesh-import");
    assert!(has_sel(cx, "mesh-import-modal"));
    assert_eq!(
        view.read_with(cx, |v, _| v.active_target()),
        Some(Target::Mesh(MeshField::Import))
    );
    set_clipboard(
        cx,
        "instance_name = \"lab\"\nipv4 = \"10.144.144.5/24\"\nlisteners = [\"tcp://0.0.0.0:11010\"]\n\n[network_identity]\nnetwork_name = \"lab-net\"\nnetwork_secret = \"0123456789abcdef0123\"\n\n[[peer]]\nuri = \"tcp://public.example.com:11010\"\n",
    );
    cx.simulate_keystrokes(PASTE);
    draw(cx);
    let (lines, ready) = view.read_with(cx, |v, _| match &v.mesh.modal {
        Some(Modal::Import(d)) => (d.text.lines().count(), d.ready().cloned()),
        _ => panic!("the import dialog is open"),
    });
    assert!(lines >= 8, "line breaks survive the paste");
    let ready = ready.expect("a valid preview");
    assert_eq!(ready.network_name, "lab-net");
    assert_eq!(ready.listen_port, 11011);
    // Enter is a new line inside the box, Esc would close it
    cx.simulate_keystrokes("enter");
    assert!(view.read_with(cx, |v, _| v.mesh.modal.is_some()));
    click_sel(cx, "import-go");
    let names: Vec<String> = view.read_with(cx, |v, _| {
        v.mesh
            .draft
            .as_ref()
            .unwrap()
            .items
            .iter()
            .map(|i| i.profile.network_name.clone())
            .collect()
    });
    assert_eq!(names.len(), 2);
    assert_eq!(names[1], "lab-net");
    assert!(view.read_with(cx, |v, _| v.mesh.modal.is_none()));

    // the adopt flow uses the same dialog with its own intro
    change(&view, cx, |v| v.open_import(ImportIntro::Adopt));
    assert!(has_sel(cx, "mesh-import-modal"));
    cx.simulate_keystrokes("escape");
    assert!(view.read_with(cx, |v, _| v.mesh.modal.is_none()));
}

#[gpui::test]
fn the_unlock_card_opens_the_vault_and_forgets_the_password(cx: &mut TestAppContext) {
    let hub = fake_hub();
    let (view, cx) = live_view(cx, &hub);
    let s = session();
    view.update(cx, |v, _| v.mesh.vault = Vault::Locked(s.envelope.clone()));
    on_mesh_tab(&view, cx, probe(EasyTierPresence::NotInstalled, false));
    assert!(has_sel(cx, "mesh-unlock"));
    click_sel(cx, "mesh-unlock-field");
    cx.simulate_input("wrong password");
    cx.simulate_keystrokes("enter");
    assert!(view.read_with(cx, |v, _| v.mesh.unlock_text.is_empty()));
    pump_until(&view, cx, "the failed unlock", |v| v.mesh.vault_rx.is_none());
    assert!(view.read_with(cx, |v, _| matches!(v.mesh.vault, Vault::Locked(_))
        && v.mesh.notice.as_ref().is_some_and(|(t, e)| *e && t.contains("密码"))));

    click_sel(cx, "mesh-unlock-field");
    cx.simulate_input("correct horse battery");
    cx.simulate_keystrokes("enter");
    pump_until(&view, cx, "the unlock", |v| v.mesh.vault.session().is_some());
    assert!(view.read_with(cx, |v, _| v.mesh.unlock_text.is_empty()
        && v.mesh.draft.as_ref().is_some_and(|d| d.items.len() == 1)));
    change(&view, cx, |_| {});
    assert!(has_sel(cx, "mesh-profile-0"));
}

#[gpui::test]
fn a_pairing_code_can_carry_a_sealed_mesh_invite(cx: &mut TestAppContext) {
    let hub = fake_hub();
    let (view, cx) = live_view(cx, &hub);
    let s = session();
    let profile = s.vault.profiles[0].clone();
    view.update(cx, |v, _| {
        v.hub = HubState::Online { count: 7 };
        v.apply_devices(fleet());
    });
    draw(cx);
    // without an unlocked vault there is nothing to offer
    click_sel(cx, "add-device");
    pump_until(&view, cx, "the plain code", |v| v.pair.code.is_some());
    assert!(!has_sel(cx, "pair-mesh"));
    view.update(cx, |v, _| v.close_pair_modal());

    change(&view, cx, |v| v.mesh.open(s));
    click_sel(cx, "add-device");
    pump_until(&view, cx, "a code", |v| v.pair.code.is_some());
    assert!(has_sel(cx, "pair-mesh"));
    view.update(cx, |v, _| v.set_pair_mesh(Some(profile.id.clone())));
    pump_until(&view, cx, "the code with an invite", |v| {
        v.pair.code.is_some() && v.pair.invite_suffix().is_some()
    });
    assert!(
        hub.log()
            .iter()
            .any(|l| l.starts_with("POST /api/v1/pairing-codes") && l.ends_with("mesh_invite")),
        "{:?}",
        hub.log()
    );
    draw(cx);
    assert!(has_sel(cx, "pair-invite"));
    let join = view.read_with(cx, |v, _| v.pair.join_code().unwrap());
    let (code, secret) = skylatch_client_core::mesh_vault::split_join_code(&join).unwrap();
    assert_eq!(code.len(), 7);
    assert!(secret.is_some(), "the suffix completes the code: {join}");
    click_sel(cx, "pair-copy");
    let clip = clipboard_text(cx).unwrap();
    assert!(clip.contains(&format!("--code {join}")), "{clip}");

    // back to no invite: a plain code again
    view.update(cx, |v, _| v.set_pair_mesh(None));
    pump_until(&view, cx, "the plain code again", |v| v.pair.code.is_some());
    assert!(view.read_with(cx, |v, _| v.pair.invite_suffix().is_none()
        && v.pair.join_code().is_some_and(|c| c.len() == 7)));
}

use skylatch_client_core::mesh_vault::{
    format_join_code, format_recovery_key, generate_dek, open_mesh_invite, open_vault,
    open_vault_with_recovery_key, parse_recovery_key, reseal_vault, rewrap_dek, seal_mesh_invite,
    seal_vault, split_join_code, InviteSecret, MeshVaultError,
};
use skylatch_client_core::{
    create_pairing_code, create_pairing_code_with_invite, delete_mesh_vault, enroll,
    get_mesh_vault, hub_info, login, put_mesh_vault, signup, ClientError, MeshVault,
    MeshVaultEnvelope, VaultPutError,
};
use skylatch_hub::{router, AppState, HubConfig};
use skylatch_proto::onboarding::{CodeEnrollment, EnrollRequest};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const PASSWORD: &str = "correct horse battery";

fn vault() -> MeshVault {
    let mut vault = MeshVault::new_default().unwrap();
    let lab = vault.next_isolated_defaults().unwrap();
    vault.profiles.push(lab);
    vault
}

fn flip(field: &str) -> String {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let mut bytes = engine.decode(field).unwrap();
    bytes[0] ^= 1;
    engine.encode(bytes)
}

#[test]
fn vault_round_trips_and_rejects_wrong_password_or_tampering() {
    let plain = vault();
    let dek = generate_dek().unwrap();
    let envelope = seal_vault(&plain, &dek, PASSWORD, 3, 1_700_000_000).unwrap();
    envelope.validate_shape().unwrap();
    assert_eq!(envelope.version, 3);
    assert_eq!(envelope.updated_at_unix, 1_700_000_000);
    assert_eq!(envelope.kdf, "argon2id-v1");
    let json = serde_json::to_string(&envelope).unwrap();
    assert!(!json.contains(&plain.profiles[0].network_secret));
    assert!(!json.contains(&plain.profiles[0].network_name));

    let (opened, key) = open_vault(&envelope, PASSWORD).unwrap();
    assert_eq!(opened, plain);
    assert_eq!(key.as_bytes(), dek.as_bytes());
    assert_eq!(format!("{key:?}"), "VaultKey(<redacted>)");

    assert_eq!(
        open_vault(&envelope, "incorrect password").unwrap_err(),
        MeshVaultError::WrongPasswordOrCorrupted
    );
    for tampered in [
        MeshVaultEnvelope {
            payload_ct_b64: flip(&envelope.payload_ct_b64),
            ..envelope.clone()
        },
        MeshVaultEnvelope {
            payload_nonce_b64: flip(&envelope.payload_nonce_b64),
            ..envelope.clone()
        },
        MeshVaultEnvelope {
            wrapped_dek_ct_b64: flip(&envelope.wrapped_dek_ct_b64),
            ..envelope.clone()
        },
        MeshVaultEnvelope {
            kdf_salt_b64: flip(&envelope.kdf_salt_b64),
            ..envelope.clone()
        },
    ] {
        assert_eq!(
            open_vault(&tampered, PASSWORD).unwrap_err(),
            MeshVaultError::WrongPasswordOrCorrupted
        );
    }
    let other = seal_vault(&plain, &generate_dek().unwrap(), PASSWORD, 1, 0).unwrap();
    let spliced = MeshVaultEnvelope {
        payload_ct_b64: other.payload_ct_b64,
        payload_nonce_b64: other.payload_nonce_b64,
        ..envelope.clone()
    };
    assert_eq!(
        open_vault(&spliced, PASSWORD).unwrap_err(),
        MeshVaultError::WrongPasswordOrCorrupted
    );
    assert_eq!(
        open_vault(
            &MeshVaultEnvelope {
                kdf: "scrypt".into(),
                ..envelope.clone()
            },
            PASSWORD
        )
        .unwrap_err(),
        MeshVaultError::UnsupportedKdf("scrypt".into())
    );
    assert!(matches!(
        open_vault(
            &MeshVaultEnvelope {
                payload_nonce_b64: "AAAA".into(),
                ..envelope
            },
            PASSWORD
        ),
        Err(MeshVaultError::MalformedEnvelope(_))
    ));
}

#[test]
fn seal_refuses_invalid_vaults() {
    let mut plain = vault();
    plain.profiles[1].listen_port = plain.profiles[0].listen_port;
    assert!(matches!(
        seal_vault(&plain, &generate_dek().unwrap(), PASSWORD, 1, 0),
        Err(MeshVaultError::InvalidVault(_))
    ));
}

#[test]
fn recovery_key_round_trips_and_catches_typos() {
    let plain = vault();
    let dek = generate_dek().unwrap();
    let envelope = seal_vault(&plain, &dek, PASSWORD, 1, 0).unwrap();
    let text = format_recovery_key(&dek);
    assert!(text.starts_with("SLK1-"));
    assert_eq!(text.len(), 4 + 14 * 5);
    assert!(text[5..]
        .split('-')
        .all(|group| group.len() == 4 && group.bytes().all(|b| b.is_ascii_alphanumeric())));

    let parsed = parse_recovery_key(&text).unwrap();
    assert_eq!(parsed.as_bytes(), dek.as_bytes());
    let relaxed = text.to_lowercase().replace('-', " ").replace('0', "o");
    assert_eq!(
        parse_recovery_key(&relaxed).unwrap().as_bytes(),
        dek.as_bytes()
    );
    assert_eq!(
        open_vault_with_recovery_key(&envelope, &parsed).unwrap(),
        plain
    );

    let symbols: Vec<char> = text.chars().collect();
    let position = text.rfind('-').unwrap() - 2;
    let mut typo = symbols.clone();
    typo[position] = if typo[position] == 'Z' { 'Y' } else { 'Z' };
    let typo: String = typo.into_iter().collect();
    assert_eq!(
        parse_recovery_key(&typo).unwrap_err(),
        MeshVaultError::RecoveryKeyChecksum
    );
    let mut swapped = symbols;
    let (a, b) = (6, 7);
    if swapped[a] != swapped[b] {
        swapped.swap(a, b);
        let swapped: String = swapped.into_iter().collect();
        assert_eq!(
            parse_recovery_key(&swapped).unwrap_err(),
            MeshVaultError::RecoveryKeyChecksum
        );
    }
    for bad in [
        "",
        "SLK1",
        "SLK2-0000",
        &text[..text.len() - 1],
        "SLK1-UUUU",
    ] {
        assert_eq!(
            parse_recovery_key(bad).unwrap_err(),
            MeshVaultError::InvalidRecoveryKey,
            "{bad}"
        );
    }
    assert_eq!(
        open_vault_with_recovery_key(&envelope, &generate_dek().unwrap()).unwrap_err(),
        MeshVaultError::WrongKeyOrCorrupted
    );
}

#[test]
fn password_change_rewraps_only_the_key() {
    let plain = vault();
    let dek = generate_dek().unwrap();
    let envelope = seal_vault(&plain, &dek, PASSWORD, 4, 10).unwrap();
    let rewrapped = rewrap_dek(&envelope, &dek, "a brand new password", 20).unwrap();
    assert_eq!(rewrapped.payload_ct_b64, envelope.payload_ct_b64);
    assert_eq!(rewrapped.payload_nonce_b64, envelope.payload_nonce_b64);
    assert_ne!(rewrapped.kdf_salt_b64, envelope.kdf_salt_b64);
    assert_ne!(rewrapped.wrapped_dek_ct_b64, envelope.wrapped_dek_ct_b64);
    assert_eq!(rewrapped.version, 4);
    assert_eq!(rewrapped.updated_at_unix, 20);
    assert_eq!(
        open_vault(&rewrapped, PASSWORD).unwrap_err(),
        MeshVaultError::WrongPasswordOrCorrupted
    );
    let (opened, key) = open_vault(&rewrapped, "a brand new password").unwrap();
    assert_eq!(opened, plain);
    assert_eq!(key.as_bytes(), dek.as_bytes());
    assert_eq!(
        rewrap_dek(&envelope, &generate_dek().unwrap(), "x", 0).unwrap_err(),
        MeshVaultError::WrongKeyOrCorrupted
    );

    let mut edited = plain.clone();
    edited.profiles[1].name = "Lab VMs".into();
    let resealed = reseal_vault(&rewrapped, &edited, &dek, 30).unwrap();
    assert_eq!(resealed.wrapped_dek_ct_b64, rewrapped.wrapped_dek_ct_b64);
    assert_ne!(resealed.payload_ct_b64, rewrapped.payload_ct_b64);
    assert_eq!(
        open_vault(&resealed, "a brand new password").unwrap().0,
        edited
    );
    assert_eq!(
        reseal_vault(&rewrapped, &edited, &generate_dek().unwrap(), 0).unwrap_err(),
        MeshVaultError::WrongKeyOrCorrupted
    );
}

#[test]
fn mesh_invite_needs_the_invite_secret() {
    let profile = vault().profiles.remove(1);
    let secret = InviteSecret::generate().unwrap();
    let invite = seal_mesh_invite(&profile, &secret).unwrap();
    invite.validate_shape().unwrap();
    assert!(!invite.ct_b64.contains(&profile.network_secret));
    assert_eq!(open_mesh_invite(&invite, &secret).unwrap(), profile);
    assert_eq!(format!("{secret:?}"), "InviteSecret(<redacted>)");

    let join = format_join_code("482-917", &secret);
    let (code, parsed) = split_join_code(&join.to_lowercase()).unwrap();
    assert_eq!(code, "482-917");
    let parsed = parsed.unwrap();
    assert_eq!(parsed.display(), secret.display());
    assert_eq!(open_mesh_invite(&invite, &parsed).unwrap(), profile);
    assert!(matches!(split_join_code("482-917"), Ok((c, None)) if c == "482-917"));
    for bad in ["48-2917", "482-917-", "482-917-ABCD", "482-917X", "€€€-917"] {
        assert!(split_join_code(bad).is_err(), "{bad}");
    }

    let other = InviteSecret::generate().unwrap();
    assert_eq!(
        open_mesh_invite(&invite, &other).unwrap_err(),
        MeshVaultError::WrongInviteSecretOrCorrupted
    );
    let tampered = skylatch_client_core::MeshInviteCiphertext {
        ct_b64: flip(&invite.ct_b64),
        ..invite
    };
    assert_eq!(
        open_mesh_invite(&tampered, &secret).unwrap_err(),
        MeshVaultError::WrongInviteSecretOrCorrupted
    );
}

struct Hub {
    url: String,
    task: JoinHandle<()>,
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn hub() -> Hub {
    let mut config = HubConfig::new(vec![]);
    config.allow_signup = true;
    config.easytier_peers = vec!["tcp://public.example.com:11010".into()];
    let state = AppState::with_auth(config, None, vec![]).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    Hub { url, task }
}

#[tokio::test]
async fn http_helpers_sync_the_vault_and_deliver_invites() {
    let hub = hub().await;
    let info = hub_info(&hub.url).await.unwrap();
    assert!(info.allow_signup);
    assert_eq!(info.easytier_peers, ["tcp://public.example.com:11010"]);

    let account = signup(&hub.url, "alice", PASSWORD).await.unwrap();
    assert!(matches!(
        signup(&hub.url, "alice", PASSWORD).await,
        Err(ClientError::Hub { code, message }) if code == "409" && message == "account already exists"
    ));
    let token = login(&hub.url, "alice", PASSWORD).await.unwrap().token;
    assert_eq!(token, account.token);

    assert!(get_mesh_vault(&hub.url, &token).await.unwrap().is_none());
    let plain = vault();
    let dek = generate_dek().unwrap();
    let sealed = seal_vault(&plain, &dek, PASSWORD, 1, 0).unwrap();
    let stored = put_mesh_vault(&hub.url, &token, 0, &sealed).await.unwrap();
    assert_eq!(stored.version, 1);
    let fetched = get_mesh_vault(&hub.url, &token).await.unwrap().unwrap();
    assert_eq!(fetched, stored);
    assert_eq!(open_vault(&fetched, PASSWORD).unwrap().0, plain);

    let mut edited = plain.clone();
    edited.profiles.pop();
    let update = reseal_vault(&fetched, &edited, &dek, 0).unwrap();
    let stored = put_mesh_vault(&hub.url, &token, 1, &update).await.unwrap();
    assert_eq!(stored.version, 2);
    match put_mesh_vault(&hub.url, &token, 1, &update).await {
        Err(VaultPutError::Conflict(Some(current))) => assert_eq!(*current, stored),
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert!(matches!(
        put_mesh_vault(&hub.url, "incorrect", 0, &update).await,
        Err(VaultPutError::Client(ClientError::Hub { code, .. })) if code == "401"
    ));
    delete_mesh_vault(&hub.url, &token).await.unwrap();
    assert!(get_mesh_vault(&hub.url, &token).await.unwrap().is_none());
    assert!(matches!(
        put_mesh_vault(&hub.url, &token, 2, &update).await,
        Err(VaultPutError::Conflict(None))
    ));

    let secret = InviteSecret::generate().unwrap();
    let invite = seal_mesh_invite(&plain.profiles[1], &secret).unwrap();
    let pair = create_pairing_code_with_invite(&hub.url, &token, &invite)
        .await
        .unwrap();
    let join = format_join_code(&pair.code, &secret);
    let (code, secret) = split_join_code(&join).unwrap();
    let credential = enroll(
        &hub.url,
        &EnrollRequest::Code(CodeEnrollment {
            code,
            device_id: "lab-vm".into(),
            name: "Lab VM".into(),
        }),
    )
    .await
    .unwrap();
    let received = credential.mesh_invite.expect("invite is released");
    assert_eq!(
        open_mesh_invite(&received, &secret.unwrap()).unwrap(),
        plain.profiles[1]
    );

    let pair = create_pairing_code(&hub.url, &token).await.unwrap();
    let credential = enroll(
        &hub.url,
        &EnrollRequest::Code(CodeEnrollment {
            code: pair.code,
            device_id: "plain".into(),
            name: "Plain host".into(),
        }),
    )
    .await
    .unwrap();
    assert!(credential.mesh_invite.is_none());
}

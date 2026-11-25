use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use skylatch_hub::{router, AppState, HubConfig, HubMode};
use skylatch_proto::control::{DeviceInfo, HostToHub, HubToHost};
use skylatch_proto::credentials::random_token;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const STATIC: &str = "example-static-account-token";
const OTHER: &str = "example-other-account-token";

struct Hub {
    origin: String,
    client: Client,
    task: JoinHandle<()>,
}

impl Hub {
    async fn start(path: Option<&Path>, tune: impl FnOnce(&mut HubConfig)) -> Self {
        let mut config = HubConfig::new(vec![
            ("demo".into(), STATIC.into()),
            ("other".into(), OTHER.into()),
        ]);
        tune(&mut config);
        let state = AppState::with_auth(config, path.map(Path::to_owned), vec![]).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        Self {
            origin,
            client: Client::new(),
            task,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> Response {
        let mut request = self.client.post(self.url(path)).json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send().await.unwrap()
    }

    async fn vault_get(&self, token: &str) -> Response {
        self.client
            .get(self.url("/api/v1/mesh-vault"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn vault_put(&self, token: &str, expected: u64, envelope: Value) -> Response {
        self.client
            .put(self.url("/api/v1/mesh-vault"))
            .bearer_auth(token)
            .json(&json!({"expected_version": expected, "envelope": envelope}))
            .send()
            .await
            .unwrap()
    }

    async fn vault_delete(&self, token: &str) -> StatusCode {
        self.client
            .delete(self.url("/api/v1/mesh-vault"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn host(&self, token: &str, device_id: &str) -> HubToHost {
        let ws_url = self.url("/ws/host").replacen("http", "ws", 1);
        let (mut socket, _) = connect_async(ws_url).await.unwrap();
        let device = DeviceInfo {
            device_id: device_id.into(),
            name: "Example host".into(),
            os: "linux".into(),
            host_version: "1".into(),
            monitors: 1,
            direct_endpoints: vec![],
            form_factor: Default::default(),
            monitor_list: vec![],
            room: None,
            slot: None,
            hardware_model: None,
            online: true,
            last_seen_unix: None,
        };
        let register = HostToHub::Register {
            token: token.into(),
            device,
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&register).unwrap().into(),
            ))
            .await
            .unwrap();
        let reply = timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        serde_json::from_str(reply.as_str()).unwrap()
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("skylatch-mesh-{}", random_token().unwrap()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn envelope(tag: char) -> Value {
    json!({
        "version": 99,
        "kdf": "argon2id-v1",
        "kdf_salt_b64": "AAAAAAAAAAAAAAAAAAAAAA==",
        "wrapped_dek_nonce_b64": "AAAAAAAAAAAAAAAA",
        "wrapped_dek_ct_b64": "A".repeat(64),
        "payload_nonce_b64": "AAAAAAAAAAAAAAAA",
        "payload_ct_b64": tag.to_string().repeat(24),
        "updated_at_unix": 0,
    })
}

async fn json_of(response: Response, status: StatusCode) -> Value {
    assert_eq!(response.status(), status);
    response.json().await.unwrap()
}

#[tokio::test]
async fn info_is_public_and_reflects_configuration() {
    let hub = Hub::start(None, |c| {
        c.mode = HubMode::SignalingOnly;
        c.easytier_peers = vec!["tcp://public.example.com:11010".into()];
        c.allow_signup = true;
        c.max_devices_per_account = Some(3);
    })
    .await;
    let info = json_of(
        hub.client
            .get(hub.url("/api/v1/info"))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(info["mode"], "signaling-only");
    assert_eq!(
        info["easytier_peers"],
        json!(["tcp://public.example.com:11010"])
    );
    assert_eq!(info["allow_signup"], true);
    assert_eq!(info["max_devices_per_account"], 3);
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));

    let plain = Hub::start(None, |_| {}).await;
    let info = json_of(
        plain
            .client
            .get(plain.url("/api/v1/info"))
            .send()
            .await
            .unwrap(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        info,
        json!({"version": env!("CARGO_PKG_VERSION"), "mode": "relay", "easytier_peers": [], "allow_signup": false})
    );
}

#[tokio::test]
async fn signup_requires_the_switch_and_unique_valid_names() {
    let closed = Hub::start(None, |_| {}).await;
    let body = json!({"username": "alice", "password": "long enough"});
    assert_eq!(
        closed
            .post("/api/v1/signup", None, body.clone())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );

    let hub = Hub::start(None, |c| c.allow_signup = true).await;
    let created = json_of(
        hub.post("/api/v1/signup", None, body.clone()).await,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(created["account"], "alice");
    let token = created["token"].as_str().unwrap();
    assert_eq!(
        hub.post("/api/v1/signup", None, body.clone())
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        hub.post(
            "/api/v1/signup",
            None,
            json!({"username": "demo", "password": "long enough"})
        )
        .await
        .status(),
        StatusCode::CONFLICT,
        "static account names are taken"
    );
    for bad in [
        json!({"username": "bob", "password": "short"}),
        json!({"username": "-bob", "password": "long enough"}),
        json!({"username": "b o b", "password": "long enough"}),
        json!({"username": "", "password": "long enough"}),
    ] {
        assert_eq!(
            hub.post("/api/v1/signup", None, bad).await.status(),
            StatusCode::BAD_REQUEST
        );
    }
    let login = json_of(hub.post("/api/v1/login", None, body).await, StatusCode::OK).await;
    assert_eq!(login["token"], token);
    assert_eq!(hub.vault_get(token).await.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn device_limit_applies_to_new_devices_only() {
    let hub = &Hub::start(None, |c| c.max_devices_per_account = Some(2)).await;
    let enroll = |id: &'static str| async move {
        let code = json_of(
            hub.post("/api/v1/pairing-codes", Some(STATIC), json!({}))
                .await,
            StatusCode::OK,
        )
        .await["code"]
            .as_str()
            .unwrap()
            .to_string();
        hub.post(
            "/api/v1/enroll",
            None,
            json!({"code": code, "device_id": id, "name": "Example host"}),
        )
        .await
    };
    let first = json_of(enroll("first").await, StatusCode::OK).await;
    assert!(matches!(
        hub.host(STATIC, "static-host").await,
        HubToHost::Registered { .. }
    ));
    assert_eq!(enroll("third").await.status(), StatusCode::FORBIDDEN);
    assert!(matches!(
        hub.host(STATIC, "another-static").await,
        HubToHost::Error { code, .. } if code == "device_limit"
    ));
    assert!(matches!(
        hub.host(first["device_token"].as_str().unwrap(), "first")
            .await,
        HubToHost::Registered { .. }
    ));
    assert!(matches!(
        hub.host(STATIC, "static-host").await,
        HubToHost::Registered { .. }
    ));
    assert!(matches!(
        hub.host(OTHER, "other-account").await,
        HubToHost::Registered { .. }
    ));
}

#[tokio::test]
async fn mesh_vault_is_versioned_per_account_and_survives_restarts() {
    let scratch = Scratch::new();
    let path = scratch.0.join("state.json");
    std::fs::write(
        &path,
        br#"{"users":{},"devices":{"demo":{"old-host":"0000000000000000000000000000000000000000000000000000000000000000"}}}"#,
    )
    .unwrap();
    let hub = Hub::start(Some(&path), |_| {}).await;

    assert_eq!(hub.vault_get(STATIC).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        hub.vault_put(STATIC, 1, envelope('A')).await.status(),
        StatusCode::CONFLICT
    );
    let first = json_of(
        hub.vault_put(STATIC, 0, envelope('A')).await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(first["version"], 1);
    assert!(first["updated_at_unix"].as_u64().unwrap() > 0);
    assert_eq!(
        json_of(hub.vault_get(STATIC).await, StatusCode::OK).await,
        first
    );
    assert_eq!(hub.vault_get(OTHER).await.status(), StatusCode::NOT_FOUND);

    let (a, b) = tokio::join!(
        hub.vault_put(STATIC, 1, envelope('B')),
        hub.vault_put(STATIC, 1, envelope('C')),
    );
    let (won, lost) = if a.status() == StatusCode::OK {
        (a, b)
    } else {
        (b, a)
    };
    let won = json_of(won, StatusCode::OK).await;
    let lost = json_of(lost, StatusCode::CONFLICT).await;
    assert_eq!(won["version"], 2);
    assert_eq!(lost["current"], won);

    assert_eq!(
        hub.vault_put(STATIC, 2, json!({"kdf": "argon2id-v1"}))
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut bad = envelope('D');
    bad["kdf"] = json!("pbkdf2");
    assert_eq!(
        hub.vault_put(STATIC, 2, bad).await.status(),
        StatusCode::BAD_REQUEST
    );
    let mut huge = envelope('D');
    huge["payload_ct_b64"] = json!("A".repeat(70 * 1024));
    assert_eq!(
        hub.vault_put(STATIC, 2, huge).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(
        hub.client
            .get(hub.url("/api/v1/mesh-vault"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    drop(hub);

    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        saved["devices"]["demo"]["old-host"].as_str().unwrap().len(),
        64
    );
    assert_eq!(saved["mesh_vaults"]["demo"], won);

    let hub = Hub::start(Some(&path), |_| {}).await;
    assert_eq!(
        json_of(hub.vault_get(STATIC).await, StatusCode::OK).await,
        won
    );
    assert_eq!(hub.vault_delete(STATIC).await, StatusCode::NO_CONTENT);
    assert_eq!(hub.vault_get(STATIC).await.status(), StatusCode::NOT_FOUND);
    assert_eq!(hub.vault_delete(STATIC).await, StatusCode::NO_CONTENT);
    let conflict = json_of(
        hub.vault_put(STATIC, 2, envelope('E')).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(conflict["current"], Value::Null);
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["mesh_vaults"], json!({}));
}

#[tokio::test]
async fn pairing_code_releases_its_mesh_invite_once() {
    let hub = Hub::start(None, |_| {}).await;
    let invite = json!({"nonce_b64": "AAAAAAAAAAAAAAAA", "ct_b64": "Q".repeat(40)});
    let code = json_of(
        hub.post(
            "/api/v1/pairing-codes",
            Some(STATIC),
            json!({"mesh_invite": invite}),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    let code = code["code"].as_str().unwrap();
    let enrolled = json_of(
        hub.post(
            "/api/v1/enroll",
            None,
            json!({"code": code, "device_id": "vm-1", "name": "VM"}),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(enrolled["mesh_invite"], invite);
    assert_eq!(
        hub.post(
            "/api/v1/enroll",
            None,
            json!({"code": code, "device_id": "vm-2", "name": "VM"}),
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );

    let plain = hub
        .client
        .post(hub.url("/api/v1/pairing-codes"))
        .bearer_auth(STATIC)
        .send()
        .await
        .unwrap();
    let code = json_of(plain, StatusCode::OK).await;
    let enrolled = json_of(
        hub.post(
            "/api/v1/enroll",
            None,
            json!({"code": code["code"], "device_id": "plain", "name": "Host"}),
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert!(enrolled.get("mesh_invite").is_none());

    for bad in [
        json!({"mesh_invite": {"nonce_b64": "AAAA", "ct_b64": "Q".repeat(40)}}),
        json!({"mesh_invite": {"nonce_b64": "AAAAAAAAAAAAAAAA", "ct_b64": "Q".repeat(17 * 1024)}}),
        json!({"unexpected": true}),
    ] {
        assert_eq!(
            hub.post("/api/v1/pairing-codes", Some(STATIC), bad)
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
}

#[tokio::test]
async fn enroll_is_rate_limited_per_client() {
    let hub = Hub::start(None, |_| {}).await;
    let body = json!({"code": "bad", "device_id": "x", "name": "x"});
    for _ in 0..30 {
        assert_eq!(
            hub.post("/api/v1/enroll", None, body.clone())
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(
        hub.post("/api/v1/enroll", None, body).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        hub.client
            .get(hub.url("/api/v1/info"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

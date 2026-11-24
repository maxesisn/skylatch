use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};
use skylatch_hub::{router, AppState, HubConfig};
use skylatch_proto::control::{DeviceInfo, HostToHub, HubToHost};
use skylatch_proto::credentials::{is_token, random_token};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

const USER: &str = "demo";
const PASSWORD: &str = "example passphrase for testing";
const LEGACY: &str = "example-static-account-token";
const OTHER: &str = "example-other-account-token";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Hub {
    origin: String,
    client: Client,
    task: JoinHandle<()>,
}

impl Hub {
    async fn start(path: Option<&Path>, seed_user: bool) -> Self {
        let config = HubConfig::new(vec![
            (USER.into(), LEGACY.into()),
            ("other".into(), OTHER.into()),
        ]);
        let users = seed_user
            .then(|| (USER.into(), PASSWORD.into()))
            .into_iter()
            .collect();
        let state = AppState::with_auth(config, path.map(Path::to_owned), users).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            origin,
            client,
            task,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }

    async fn login(&self, password: &str) -> Response {
        self.client
            .post(self.url("/api/v1/login"))
            .json(&json!({"username":USER,"password":password}))
            .send()
            .await
            .unwrap()
    }

    async fn list(&self, token: &str) -> Response {
        self.client
            .get(self.url("/api/v1/devices"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn new_code(&self, token: &str) -> Response {
        self.client
            .post(self.url("/api/v1/pairing-codes"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn enroll(&self, value: Value) -> Response {
        self.client
            .post(self.url("/api/v1/enroll"))
            .json(&value)
            .send()
            .await
            .unwrap()
    }

    async fn revoke(&self, token: &str, device: &str) -> StatusCode {
        self.client
            .delete(self.url(&format!("/api/v1/devices/{device}/token")))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn host(&self, token: &str, device_id: &str) -> (Socket, HubToHost) {
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
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&HostToHub::Register {
                    token: token.into(),
                    device,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let msg = timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        (socket, serde_json::from_str(msg.as_str()).unwrap())
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn ok(response: Response) -> Value {
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

#[tokio::test]
async fn login_pair_enroll_and_revoke_are_account_and_device_scoped() {
    let hub = Hub::start(None, true).await;
    assert_eq!(
        hub.login("incorrect").await.status(),
        StatusCode::UNAUTHORIZED
    );
    let login = ok(hub.login(PASSWORD).await).await;
    let session = login["token"].as_str().unwrap();
    assert_eq!(login["account"], USER);
    assert!(is_token(session));
    assert_eq!(ok(hub.list(session).await).await, json!({"devices":[]}));

    let pair = ok(hub.new_code(session).await).await;
    let code = pair["code"].as_str().unwrap();
    assert_eq!(pair["expires_in_secs"], 600);
    assert_eq!(code.len(), 7);
    assert!(code.bytes().enumerate().all(|(i, b)| if i == 3 {
        b == b'-'
    } else {
        b.is_ascii_digit()
    }));
    let (first, second) = tokio::join!(
        hub.enroll(json!({"code":code,"name":"Example host","device_id":"paired"})),
        hub.enroll(json!({"code":code,"name":"Another host","device_id":"concurrent"})),
    );
    let (accepted, denied) = if first.status().is_success() {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let accepted = ok(accepted).await;
    let id = accepted["device_id"].as_str().unwrap();
    let token = accepted["device_token"].as_str().unwrap();
    assert_eq!(accepted["account"], USER);
    assert!(is_token(token));
    assert_eq!(hub.list(token).await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(hub.new_code(token).await.status(), StatusCode::UNAUTHORIZED);
    assert!(
        matches!(hub.host(token, "wrong-id").await.1, HubToHost::Error { code, .. } if code == "unauthorized")
    );
    let (mut paired, reply) = hub.host(token, id).await;
    assert!(matches!(reply, HubToHost::Registered { .. }));
    let standalone = ok(hub.enroll(json!({"username":USER,"password":PASSWORD,"name":"Standalone","device_id":"standalone"})).await).await;
    let other_token = standalone["device_token"].as_str().unwrap();
    assert_ne!(token, other_token);
    let (_standalone, reply) = hub.host(other_token, "standalone").await;
    assert!(matches!(reply, HubToHost::Registered { .. }));
    assert_eq!(hub.revoke(OTHER, id).await, StatusCode::NOT_FOUND);
    assert_eq!(hub.revoke(session, id).await, StatusCode::NO_CONTENT);
    let closed = timeout(Duration::from_secs(3), paired.next())
        .await
        .unwrap();
    assert!(matches!(closed, Some(Ok(Message::Close(_))) | None));
    assert!(
        matches!(hub.host(token, id).await.1, HubToHost::Error { code, .. } if code == "unauthorized")
    );
    let visible = ok(hub.list(session).await).await;
    assert_eq!(visible["devices"].as_array().unwrap().len(), 1);
    assert_eq!(visible["devices"][0]["device_id"], "standalone");
    assert!(matches!(
        hub.host(LEGACY, "legacy").await.1,
        HubToHost::Registered { .. }
    ));
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn standalone_credentials_and_revocation_survive_hub_restart() {
    let dir =
        Scratch(std::env::temp_dir().join(format!("skylatch-test-{}", random_token().unwrap())));
    std::fs::create_dir(&dir.0).unwrap();
    let file = dir.0.join("state.json");
    let hub = Hub::start(Some(&file), true).await;
    let login = ok(hub.login(PASSWORD).await).await;
    let session = login["token"].as_str().unwrap();
    let enrolled = ok(hub.enroll(json!({"username":USER,"password":PASSWORD,"name":"Standalone","device_id":"persisted"})).await).await;
    let token = enrolled["device_token"].as_str().unwrap();
    let state = std::fs::read_to_string(&file).unwrap();
    assert!(state.contains("$argon2id$"));
    assert!(!state.contains(PASSWORD));
    assert!(!state.contains(token));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    drop(hub);
    let hub = Hub::start(Some(&file), false).await;
    assert_eq!(ok(hub.login(PASSWORD).await).await["token"], session);
    let (_registered, reply) = hub.host(token, "persisted").await;
    assert!(matches!(reply, HubToHost::Registered { .. }));
    assert_eq!(
        ok(hub.list(session).await).await["devices"][0]["device_id"],
        "persisted"
    );
    assert_eq!(
        hub.revoke(session, "persisted").await,
        StatusCode::NO_CONTENT
    );
    drop(hub);
    let hub = Hub::start(Some(&file), false).await;
    assert!(
        matches!(hub.host(token, "persisted").await.1, HubToHost::Error { code, .. } if code == "unauthorized")
    );
}

#[tokio::test]
async fn root_serves_self_contained_console() {
    let hub = Hub::start(None, false).await;
    let response = hub.client.get(hub.url("/")).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let html = response.text().await.unwrap();
    assert!((4_000..=16_000).contains(&html.len()));
    for text in [
        "Skylatch Hub",
        "Sign in",
        "+ Add Device",
        "skylatch-agent join",
        "Revoke",
    ] {
        assert!(html.contains(text));
    }
    assert!(!html.contains("<script src"));
}

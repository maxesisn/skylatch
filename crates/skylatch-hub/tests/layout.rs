use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use skylatch_hub::{router, AppState, HubConfig};
use skylatch_proto::credentials::random_token;
use skylatch_proto::layout::WorkspaceLayout;
use std::path::{Path, PathBuf};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const A: &str = "example-account-a-token";
const B: &str = "example-account-b-token";

struct Hub {
    origin: String,
    task: JoinHandle<()>,
}

impl Hub {
    async fn start(path: Option<&Path>) -> Self {
        let config = HubConfig::new(vec![("a".into(), A.into()), ("b".into(), B.into())]);
        let state = AppState::with_auth(config, path.map(Path::to_owned), vec![]).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let origin = format!("http://{}/api/v1/layout", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        Self { origin, task }
    }

    async fn get(&self, token: &str) -> reqwest::Response {
        Client::new()
            .get(&self.origin)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
    }

    async fn put(&self, token: &str, body: &Value) -> reqwest::Response {
        Client::new()
            .put(&self.origin)
            .bearer_auth(token)
            .json(body)
            .send()
            .await
            .unwrap()
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sample(name: &str) -> Value {
    json!({
        "version": 1,
        "rooms": [{"id": "r1", "name": name, "furniture": [
            {"id": "f1", "kind": "straight_desk", "grid_x": 1, "grid_y": 2}
        ]}],
        "placements": [{"device_id": "d1", "room_id": "r1", "furniture_id": "f1", "slot": 0}]
    })
}

#[tokio::test]
async fn fresh_hub_returns_empty_default_and_requires_auth() {
    let hub = Hub::start(None).await;
    let response = hub.get(A).await;
    assert_eq!(response.status(), StatusCode::OK);
    let layout: WorkspaceLayout = response.json().await.unwrap();
    assert_eq!(layout, WorkspaceLayout::default());
    assert_eq!(hub.get("wrong").await.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        hub.put("wrong", &sample("x")).await.status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn accounts_are_isolated() {
    let hub = Hub::start(None).await;
    let saved: Value = hub.put(A, &sample("Mine")).await.json().await.unwrap();
    assert_eq!(saved["rooms"][0]["name"], "Mine");
    assert_eq!(saved["rooms"][0]["furniture"][0]["facing"], "front_left");
    let other: WorkspaceLayout = hub.get(B).await.json().await.unwrap();
    assert_eq!(other, WorkspaceLayout::default());
    hub.put(B, &sample("Theirs")).await;
    let mine: Value = hub.get(A).await.json().await.unwrap();
    assert_eq!(mine["rooms"][0]["name"], "Mine");
}

#[tokio::test]
async fn invalid_layouts_are_rejected_with_400() {
    let hub = Hub::start(None).await;
    let mut dup = sample("x");
    dup["rooms"][0]["furniture"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id": "f1", "kind": "vm_rack", "grid_x": 0, "grid_y": 0}));
    let response = hub.put(A, &dup).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value = response.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("duplicate"));

    let mut far = sample("x");
    far["placements"][0]["du"] = json!(5.0);
    assert_eq!(hub.put(A, &far).await.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        hub.put(A, &json!({"rooms": "nope"})).await.status(),
        StatusCode::BAD_REQUEST
    );
    let after: WorkspaceLayout = hub.get(A).await.json().await.unwrap();
    assert_eq!(after, WorkspaceLayout::default());
}

#[tokio::test]
async fn oversized_body_is_refused() {
    let hub = Hub::start(None).await;
    let big = json!({"rooms": [{"id": "r", "name": "x".repeat(300 * 1024)}]});
    assert_eq!(
        hub.put(A, &big).await.status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
async fn layout_survives_restart_and_old_state_loads() {
    let dir =
        Scratch(std::env::temp_dir().join(format!("skylatch-test-{}", random_token().unwrap())));
    std::fs::create_dir(&dir.0).unwrap();
    let file = dir.0.join("state.json");
    // A state file written before layouts existed.
    std::fs::write(&file, r#"{"users":{},"devices":{}}"#).unwrap();
    {
        let hub = Hub::start(Some(&file)).await;
        let empty: WorkspaceLayout = hub.get(A).await.json().await.unwrap();
        assert_eq!(empty, WorkspaceLayout::default());
        assert_eq!(hub.put(A, &sample("Kept")).await.status(), StatusCode::OK);
    }
    let hub = Hub::start(Some(&file)).await;
    let kept: Value = hub.get(A).await.json().await.unwrap();
    assert_eq!(kept["rooms"][0]["name"], "Kept");
    let other: WorkspaceLayout = hub.get(B).await.json().await.unwrap();
    assert_eq!(other, WorkspaceLayout::default());
}

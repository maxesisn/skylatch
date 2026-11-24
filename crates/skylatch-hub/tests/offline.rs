//! Offline device persistence and the `signaling-only` mode.

use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use skylatch_hub::{router, AppState, HubConfig, HubMode};
use skylatch_proto::control::*;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

const TOKEN: &str = "example-static-account-token";

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Hub {
    origin: String,
    client: Client,
    task: JoinHandle<()>,
}

impl Hub {
    async fn start(path: Option<&Path>, mode: HubMode) -> Self {
        let mut config = HubConfig::new(vec![("demo".into(), TOKEN.into())]);
        config.mode = mode;
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

    async fn devices(&self, query: &str) -> Vec<Value> {
        let body: Value = self
            .client
            .get(self.url(&format!("/api/v1/devices{query}")))
            .bearer_auth(TOKEN)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        body["devices"].as_array().unwrap().clone()
    }

    async fn wait_online(&self, id: &str, online: bool) -> Value {
        timeout(Duration::from_secs(5), async {
            loop {
                let found = self
                    .devices("")
                    .await
                    .into_iter()
                    .find(|d| d["device_id"] == id && d["online"] == online);
                if let Some(d) = found {
                    return d;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("device state did not settle")
    }

    async fn host(&self, id: &str, name: &str, endpoints: bool) -> Socket {
        let (mut socket, _) = connect_async(self.url("/ws/host").replacen("http", "ws", 1))
            .await
            .unwrap();
        let device = DeviceInfo {
            device_id: id.into(),
            name: name.into(),
            os: "linux".into(),
            host_version: "1".into(),
            monitors: 1,
            direct_endpoints: vec![],
            form_factor: DeviceFormFactor::Mini,
            monitor_list: vec![],
            room: Some("Lab".into()),
            slot: Some(2),
            hardware_model: Some("Test Box".into()),
            online: true,
            last_seen_unix: None,
        };
        send(
            &mut socket,
            &HostToHub::Register {
                token: TOKEN.into(),
                device,
            },
        )
        .await;
        assert!(matches!(
            recv::<HubToHost>(&mut socket).await,
            HubToHost::Registered { .. }
        ));
        if endpoints {
            send(
                &mut socket,
                &HostToHub::DirectAccess {
                    key: "a".repeat(64),
                },
            )
            .await;
            send(
                &mut socket,
                &HostToHub::DirectEndpoints {
                    endpoints: vec![DirectEndpoint {
                        kind: EndpointKind::Lan,
                        host: "192.0.2.9".into(),
                        port: 7001,
                    }],
                },
            )
            .await;
            timeout(Duration::from_secs(5), async {
                while self.devices("").await[0]["direct_endpoints"]
                    .as_array()
                    .is_none_or(Vec::is_empty)
                {
                    sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
        }
        socket
    }

    async fn client(&self) -> Socket {
        let (mut socket, _) = connect_async(self.url("/ws/client").replacen("http", "ws", 1))
            .await
            .unwrap();
        send(
            &mut socket,
            &ClientToHub::Auth {
                token: TOKEN.into(),
            },
        )
        .await;
        assert!(matches!(
            recv::<HubToClient>(&mut socket).await,
            HubToClient::AuthOk { .. }
        ));
        socket
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
        let dir = std::env::temp_dir()
            .join(format!("skylatch-offline-{}", std::process::id()))
            .join(format!("{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir.join("state.json"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
    }
}

async fn send<T: serde::Serialize>(socket: &mut Socket, value: &T) {
    socket
        .send(Message::Text(serde_json::to_string(value).unwrap().into()))
        .await
        .unwrap();
}

async fn recv<T: serde::de::DeserializeOwned>(socket: &mut Socket) -> T {
    loop {
        let message = timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("timed out")
            .expect("closed")
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(text.as_str()).unwrap();
        }
    }
}

async fn connect(client: &mut Socket, id: &str) -> Vec<HubToClient> {
    send(
        client,
        &ClientToHub::ConnectDevice {
            device_id: id.into(),
        },
    )
    .await;
    let mut out = vec![recv(client).await];
    if matches!(out[0], HubToClient::DirectHint { .. }) {
        out.push(recv(client).await);
    }
    out
}

#[tokio::test]
async fn state_file_without_device_snapshots_still_loads() {
    let scratch = Scratch::new();
    std::fs::write(&scratch.0, r#"{"users":{},"devices":{}}"#).unwrap();
    let hub = Hub::start(Some(&scratch.0), HubMode::Relay).await;
    assert!(hub.devices("").await.is_empty());
}

#[tokio::test]
async fn disconnect_marks_offline_and_reconnect_restores_it() {
    let hub = Hub::start(None, HubMode::Relay).await;
    let host = hub.host("box", "Box", true).await;
    let live = hub.wait_online("box", true).await;
    assert!(live["last_seen_unix"].as_u64().unwrap() > 0);
    drop(host);
    let offline = hub.wait_online("box", false).await;
    assert_eq!(offline["direct_endpoints"], json!([]));
    assert_eq!(offline["name"], "Box");
    assert_eq!(offline["room"], "Lab");
    assert_eq!(offline["hardware_model"], "Test Box");
    assert!(hub.devices("?online_only=true").await.is_empty());
    let _host = hub.host("box", "Box renamed", false).await;
    assert_eq!(hub.wait_online("box", true).await["name"], "Box renamed");
}

#[tokio::test]
async fn list_puts_online_devices_first() {
    let hub = Hub::start(None, HubMode::Relay).await;
    drop(hub.host("a-offline", "Alpha", false).await);
    hub.wait_online("a-offline", false).await;
    let _live = hub.host("z-online", "Zulu", false).await;
    hub.wait_online("z-online", true).await;
    let ids: Vec<_> = hub
        .devices("")
        .await
        .into_iter()
        .map(|d| d["device_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(ids, ["z-online", "a-offline"]);
    let mut client = hub.client().await;
    send(&mut client, &ClientToHub::ListDevices).await;
    let HubToClient::Devices { devices } = recv(&mut client).await else {
        panic!("expected devices");
    };
    assert_eq!(
        devices.iter().map(|d| d.online).collect::<Vec<_>>(),
        [true, false]
    );
}

#[tokio::test]
async fn snapshots_survive_restart_as_offline_and_can_be_deleted() {
    let scratch = Scratch::new();
    let hub = Hub::start(Some(&scratch.0), HubMode::Relay).await;
    let host = hub.host("box", "Box", true).await;
    hub.wait_online("box", true).await;
    drop(hub);
    drop(host);
    let hub = Hub::start(Some(&scratch.0), HubMode::Relay).await;
    let restored = hub.wait_online("box", false).await;
    assert_eq!(restored["direct_endpoints"], json!([]));
    assert_eq!(restored["slot"], 2);
    assert!(restored["last_seen_unix"].as_u64().is_some());

    let mut client = hub.client().await;
    let reply = connect(&mut client, "box").await;
    assert!(matches!(&reply[0], HubToClient::Error { code, .. } if code == "device_offline"));
    let direct = hub
        .client
        .post(hub.url("/api/v1/direct-sessions"))
        .bearer_auth(TOKEN)
        .json(&json!({"device_id": "box"}))
        .send()
        .await
        .unwrap();
    assert_eq!(direct.status(), StatusCode::CONFLICT);
    assert_eq!(
        direct.json::<Value>().await.unwrap()["error"],
        "device_offline"
    );

    let delete = |id: &'static str| {
        hub.client
            .delete(hub.url(&format!("/api/v1/devices/{id}")))
            .bearer_auth(TOKEN)
            .send()
    };
    assert_eq!(
        delete("box").await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(delete("box").await.unwrap().status(), StatusCode::NOT_FOUND);
    assert!(hub.devices("").await.is_empty());
    let reply = connect(&mut client, "box").await;
    assert!(matches!(&reply[0], HubToClient::Error { code, .. } if code == "not_found"));
    drop(hub);
    let hub = Hub::start(Some(&scratch.0), HubMode::Relay).await;
    assert!(hub.devices("").await.is_empty());
}

#[tokio::test]
async fn online_devices_cannot_be_deleted() {
    let hub = Hub::start(None, HubMode::Relay).await;
    let _host = hub.host("box", "Box", false).await;
    hub.wait_online("box", true).await;
    let status = hub
        .client
        .delete(hub.url("/api/v1/devices/box"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn signaling_only_hints_but_never_relays() {
    let hub = Hub::start(None, HubMode::SignalingOnly).await;
    let health: Value = hub
        .client
        .get(hub.url("/healthz"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["mode"], "signaling-only");

    let mut host = hub.host("box", "Box", true).await;
    let mut client = hub.client().await;
    let reply = connect(&mut client, "box").await;
    assert!(matches!(&reply[0], HubToClient::DirectHint { endpoints, .. } if endpoints.len() == 1));
    assert!(matches!(&reply[1], HubToClient::Error { code, .. } if code == "relay_disabled"));
    assert!(
        timeout(Duration::from_millis(300), host.next())
            .await
            .is_err(),
        "host must not be told to open a session"
    );

    let ticket = hub
        .client
        .post(hub.url("/api/v1/direct-sessions"))
        .bearer_auth(TOKEN)
        .json(&json!({"device_id": "box"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ticket.status(), StatusCode::OK);
}

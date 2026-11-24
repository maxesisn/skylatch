//! End-to-end tests against a real listener.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use skylatch_hub::{router, AppState, HubConfig};
use skylatch_proto::control::*;
use skylatch_proto::nsl1::{Message as Nsl1, MonitorInfo};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start() -> std::net::SocketAddr {
    let cfg = HubConfig::new(vec![
        ("alice".into(), "tok-a".into()),
        ("bob".into(), "tok-b".into()),
    ]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(AppState::new(cfg)))
            .await
            .unwrap();
    });
    addr
}

async fn http_get(addr: std::net::SocketAddr, path: &str, token: Option<&str>) -> (u16, String) {
    let mut s = TcpStream::connect(addr).await.unwrap();
    let auth = token
        .map(|t| format!("Authorization: Bearer {t}\r\n"))
        .unwrap_or_default();
    let req = format!("GET {path} HTTP/1.1\r\nHost: test\r\n{auth}Connection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).await.unwrap();
    let status = raw.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

async fn send_json<T: serde::Serialize>(ws: &mut Ws, v: &T) {
    ws.send(Message::Text(serde_json::to_string(v).unwrap().into()))
        .await
        .unwrap();
}

async fn recv_json<T: serde::de::DeserializeOwned>(ws: &mut Ws) -> T {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out")
            .expect("closed")
            .expect("ws error");
        if let Message::Text(t) = m {
            return serde_json::from_str(t.as_str()).unwrap();
        }
    }
}

async fn recv_binary(ws: &mut Ws) -> Bytes {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timed out")
            .expect("closed")
            .expect("ws error");
        if let Message::Binary(b) = m {
            return b;
        }
    }
}

fn device(id: &str) -> DeviceInfo {
    DeviceInfo {
        device_id: id.into(),
        name: format!("Host {id}"),
        os: "linux".into(),
        host_version: "0.1.0".into(),
        monitors: 2,
        direct_endpoints: vec![],
        form_factor: DeviceFormFactor::Mini,
        monitor_list: vec![
            MonitorInfo {
                id: "landscape".into(),
                name: "Landscape display".into(),
                x: 0,
                y: 0,
                width: 5120,
                height: 2880,
                primary: true,
                active: true,
            },
            MonitorInfo {
                id: "portrait".into(),
                name: "Portrait display".into(),
                x: -1440,
                y: 120,
                width: 1440,
                height: 2560,
                primary: false,
                active: true,
            },
        ],
        room: Some("Office".into()),
        slot: Some(3),
        hardware_model: None,
    }
}

async fn register_host(addr: std::net::SocketAddr, token: &str, id: &str) -> Ws {
    let (mut ws, _) = connect_async(format!("ws://{addr}/ws/host")).await.unwrap();
    send_json(
        &mut ws,
        &HostToHub::Register {
            token: token.into(),
            device: device(id),
        },
    )
    .await;
    ws
}

#[tokio::test]
async fn healthz_and_auth() {
    let addr = start().await;
    let (status, body) = http_get(addr, "/healthz", None).await;
    assert_eq!(status, 200);
    assert!(body.contains("\"status\":\"ok\""));
    assert_eq!(http_get(addr, "/api/v1/devices", None).await.0, 401);
    assert_eq!(
        http_get(addr, "/api/v1/devices", Some("wrong")).await.0,
        401
    );
}

#[tokio::test]
async fn host_registration_is_listed_and_removed() {
    let addr = start().await;
    let mut host = register_host(addr, "tok-a", "box-1").await;
    let reply: HubToHost = recv_json(&mut host).await;
    assert!(matches!(
        reply,
        HubToHost::Registered {
            heartbeat_interval_secs: 15
        }
    ));

    send_json(&mut host, &HostToHub::Heartbeat { seq: 4 }).await;
    assert_eq!(
        recv_json::<HubToHost>(&mut host).await,
        HubToHost::HeartbeatAck { seq: 4 }
    );

    let endpoint = DirectEndpoint {
        kind: EndpointKind::Lan,
        host: "192.0.2.10".into(),
        port: 7001,
    };
    send_json(
        &mut host,
        &HostToHub::DirectEndpoints {
            endpoints: vec![endpoint.clone()],
        },
    )
    .await;
    // The heartbeat ack below orders the endpoint update before the listing.
    send_json(&mut host, &HostToHub::Heartbeat { seq: 5 }).await;
    recv_json::<HubToHost>(&mut host).await;

    let (status, body) = http_get(addr, "/api/v1/devices", Some("tok-a")).await;
    assert_eq!(status, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let devices: Vec<DeviceInfo> = serde_json::from_value(v["devices"].clone()).unwrap();
    let mut expected = device("box-1");
    expected.direct_endpoints = vec![endpoint];
    assert_eq!(devices, [expected]);

    // Another account sees nothing.
    let (_, body) = http_get(addr, "/api/v1/devices", Some("tok-b")).await;
    assert!(body.contains("\"devices\":[]"));

    drop(host);
    for _ in 0..50 {
        let (_, body) = http_get(addr, "/api/v1/devices", Some("tok-a")).await;
        if body.contains("\"devices\":[]") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("device was not removed after disconnect");
}

#[tokio::test]
async fn host_with_bad_token_is_rejected() {
    let addr = start().await;
    let mut host = register_host(addr, "nope", "box-1").await;
    let reply: HubToHost = recv_json(&mut host).await;
    assert!(matches!(reply, HubToHost::Error { ref code, .. } if code == "unauthorized"));
    let (_, body) = http_get(addr, "/api/v1/devices", Some("tok-a")).await;
    assert!(body.contains("\"devices\":[]"));
}

#[tokio::test]
async fn client_lists_connects_and_relays_both_ways() {
    let addr = start().await;
    let mut host = register_host(addr, "tok-a", "box-1").await;
    recv_json::<HubToHost>(&mut host).await;

    let (mut client, _) = connect_async(format!("ws://{addr}/ws/client"))
        .await
        .unwrap();
    send_json(
        &mut client,
        &ClientToHub::Auth {
            token: "tok-a".into(),
        },
    )
    .await;
    assert_eq!(
        recv_json::<HubToClient>(&mut client).await,
        HubToClient::AuthOk {
            account: "alice".into()
        }
    );
    send_json(&mut client, &ClientToHub::ListDevices).await;
    match recv_json::<HubToClient>(&mut client).await {
        HubToClient::Devices { devices } => assert_eq!(devices, [device("box-1")]),
        other => panic!("unexpected {other:?}"),
    }

    send_json(
        &mut client,
        &ClientToHub::ConnectDevice {
            device_id: "missing".into(),
        },
    )
    .await;
    assert!(matches!(
        recv_json::<HubToClient>(&mut client).await,
        HubToClient::Error { ref code, .. } if code == "not_found"
    ));

    send_json(
        &mut client,
        &ClientToHub::ConnectDevice {
            device_id: "box-1".into(),
        },
    )
    .await;
    assert!(matches!(
        recv_json::<HubToClient>(&mut client).await,
        HubToClient::DirectHint { .. }
    ));
    let session_id = match recv_json::<HubToClient>(&mut client).await {
        HubToClient::RelayStream { session_id, .. } => session_id,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(
        recv_json::<HubToHost>(&mut host).await,
        HubToHost::OpenSession {
            session_id: session_id.clone()
        }
    );

    // Host to client.
    let frame = Nsl1::Frame {
        generation: 1,
        sequence: 1,
        keyframe: true,
        width: 8,
        height: 8,
        capture_us: 1,
        data: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
    }
    .encode()
    .unwrap();
    host.send(Message::Binary(frame.clone())).await.unwrap();
    assert_eq!(recv_binary(&mut client).await, frame);

    // Client to host.
    let ack = Nsl1::Ack {
        generation: 1,
        sequence: 1,
        viewer_wait_us: Some(500),
    }
    .encode()
    .unwrap();
    client.send(Message::Binary(ack.clone())).await.unwrap();
    assert_eq!(recv_binary(&mut host).await, ack);

    // A second client is refused while the session is active.
    let (mut other, _) = connect_async(format!("ws://{addr}/ws/client"))
        .await
        .unwrap();
    send_json(
        &mut other,
        &ClientToHub::Auth {
            token: "tok-a".into(),
        },
    )
    .await;
    recv_json::<HubToClient>(&mut other).await;
    send_json(
        &mut other,
        &ClientToHub::ConnectDevice {
            device_id: "box-1".into(),
        },
    )
    .await;
    assert!(matches!(
        recv_json::<HubToClient>(&mut other).await,
        HubToClient::Error { ref code, .. } if code == "busy"
    ));

    // Closing the client closes the session on the host.
    drop(client);
    assert_eq!(
        recv_json::<HubToHost>(&mut host).await,
        HubToHost::CloseSession { session_id }
    );
}

#[tokio::test]
async fn client_with_bad_token_is_rejected() {
    let addr = start().await;
    let (mut client, _) = connect_async(format!("ws://{addr}/ws/client"))
        .await
        .unwrap();
    send_json(
        &mut client,
        &ClientToHub::Auth {
            token: "nope".into(),
        },
    )
    .await;
    assert!(matches!(
        recv_json::<HubToClient>(&mut client).await,
        HubToClient::Error { ref code, .. } if code == "unauthorized"
    ));
}

#[tokio::test]
async fn host_disconnect_closes_client_session() {
    let addr = start().await;
    let mut host = register_host(addr, "tok-a", "box-1").await;
    recv_json::<HubToHost>(&mut host).await;
    let (mut client, _) = connect_async(format!("ws://{addr}/ws/client"))
        .await
        .unwrap();
    send_json(
        &mut client,
        &ClientToHub::Auth {
            token: "tok-a".into(),
        },
    )
    .await;
    recv_json::<HubToClient>(&mut client).await;
    send_json(
        &mut client,
        &ClientToHub::ConnectDevice {
            device_id: "box-1".into(),
        },
    )
    .await;
    recv_json::<HubToClient>(&mut client).await;
    recv_json::<HubToClient>(&mut client).await;
    drop(host);
    assert!(matches!(
        recv_json::<HubToClient>(&mut client).await,
        HubToClient::SessionClosed { ref reason, .. } if reason == "host_disconnected"
    ));
}

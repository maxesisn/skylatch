//! End-to-end: real hub + real agent binary + mock local engine + WebSocket client.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use skylatch_hub::{router, AppState, HubConfig};
use skylatch_proto::control::*;
use skylatch_proto::nsl1::{InputEvent, InputRecord, Message as Nsl1, MonitorInfo};
use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    accept_async, accept_hdr_async, connect_async, MaybeTlsStream, WebSocketStream,
};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;
const WAIT: Duration = Duration::from_secs(10);

fn keyframe(sequence: u32) -> Bytes {
    Nsl1::Frame {
        generation: 1,
        sequence,
        keyframe: true,
        width: 64,
        height: 48,
        capture_us: 7,
        data: Bytes::from_static(&[0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65]),
    }
    .encode()
    .unwrap()
}

fn ack(sequence: u32) -> Bytes {
    Nsl1::Ack {
        generation: 1,
        sequence,
        viewer_wait_us: Some(1500),
    }
    .encode()
    .unwrap()
}

fn input() -> Bytes {
    Nsl1::Input {
        generation: 1,
        sequence: 0,
        record: InputRecord::new(InputEvent::KeyDown { usage: 4 }),
    }
    .encode()
    .unwrap()
}

fn monitor_list() -> Bytes {
    Nsl1::MonitorList {
        generation: 1,
        monitors: vec![MonitorInfo {
            id: "display-1".into(),
            name: "Example display".into(),
            x: -64,
            y: 0,
            width: 64,
            height: 48,
            primary: true,
            active: true,
        }],
    }
    .encode()
    .unwrap()
}

fn clipboard(text: &str) -> Bytes {
    Nsl1::ClipboardData {
        generation: 1,
        text: text.into(),
    }
    .encode()
    .unwrap()
}

/// A local engine stand-in: sends a keyframe on every `/stream` connection and records what it
/// receives.
struct MockEngine {
    addr: SocketAddr,
    connections: Arc<AtomicUsize>,
    uris: Arc<Mutex<Vec<String>>>,
    received: mpsc::UnboundedReceiver<Bytes>,
    closed: mpsc::UnboundedReceiver<()>,
}

#[allow(clippy::result_large_err)]
async fn mock_engine() -> MockEngine {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let uris = Arc::new(Mutex::new(Vec::new()));
    let (rx_tx, received) = mpsc::unbounded_channel();
    let (closed_tx, closed) = mpsc::unbounded_channel();
    let (conns, seen) = (connections.clone(), uris.clone());
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let (conns, seen) = (conns.clone(), seen.clone());
            let (rx_tx, closed_tx) = (rx_tx.clone(), closed_tx.clone());
            tokio::spawn(async move {
                let callback =
                    |req: &Request, mut resp: Response| -> Result<Response, ErrorResponse> {
                        assert_eq!(req.headers().get("sec-websocket-protocol").unwrap(), "nsl1");
                        seen.lock().unwrap().push(req.uri().to_string());
                        resp.headers_mut()
                            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("nsl1"));
                        Ok(resp)
                    };
                let mut ws = accept_hdr_async(stream, callback).await.unwrap();
                let n = conns.fetch_add(1, Ordering::SeqCst) as u32 + 1;
                ws.send(Message::Binary(keyframe(n))).await.unwrap();
                if n == 1 {
                    ws.send(Message::Binary(monitor_list())).await.unwrap();
                    ws.send(Message::Binary(clipboard("Copied on the host: café")))
                        .await
                        .unwrap();
                }
                while let Some(Ok(msg)) = ws.next().await {
                    match msg {
                        Message::Binary(b) => {
                            let _ = rx_tx.send(b);
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                let _ = closed_tx.send(());
            });
        }
    });
    MockEngine {
        addr,
        connections,
        uris,
        received,
        closed,
    }
}

fn spawn_agent(hub: SocketAddr, engine: SocketAddr) -> Child {
    Command::new(env!("CARGO_BIN_EXE_skylatch-agent"))
        .args(["--hub", &format!("ws://{hub}")])
        .args([
            "--token",
            "tok-a",
            "--device-id",
            "box-1",
            "--name",
            "Test Box",
        ])
        .args(["--engine-url", &format!("ws://{engine}")])
        .args(["--direct-endpoint", "lan=192.0.2.10:17900"])
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap()
}

async fn send_json<T: serde::Serialize>(ws: &mut Ws, v: &T) {
    ws.send(Message::Text(serde_json::to_string(v).unwrap().into()))
        .await
        .unwrap();
}

async fn recv_json<T: serde::de::DeserializeOwned>(ws: &mut Ws) -> T {
    loop {
        let m = timeout(WAIT, ws.next())
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
        let m = timeout(WAIT, ws.next())
            .await
            .expect("timed out")
            .expect("closed")
            .expect("ws error");
        if let Message::Binary(b) = m {
            return b;
        }
    }
}

#[tokio::test]
async fn client_session_is_relayed_through_hub_and_agent() {
    let cfg = HubConfig::new(vec![("alice".into(), "tok-a".into())]);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, router(AppState::new(cfg)))
            .await
            .unwrap();
    });
    let mut engine = mock_engine().await;
    let _agent = spawn_agent(hub, engine.addr);

    let (mut client, _) = connect_async(format!("ws://{hub}/ws/client"))
        .await
        .unwrap();
    send_json(
        &mut client,
        &ClientToHub::Auth {
            token: "tok-a".into(),
        },
    )
    .await;
    assert!(matches!(
        recv_json::<HubToClient>(&mut client).await,
        HubToClient::AuthOk { .. }
    ));

    // The agent registers asynchronously; poll the device list.
    let device = loop {
        send_json(&mut client, &ClientToHub::ListDevices).await;
        let HubToClient::Devices { devices } = recv_json(&mut client).await else {
            panic!("expected devices");
        };
        if let Some(d) = devices.into_iter().next() {
            break d;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(device.device_id, "box-1");
    assert_eq!(device.name, "Test Box");
    assert_eq!(device.os, std::env::consts::OS);
    assert_eq!(device.direct_endpoints.len(), 1);
    assert_eq!(device.direct_endpoints[0].port, 17900);
    // Idle: the engine has not been touched yet.
    assert_eq!(engine.connections.load(Ordering::SeqCst), 0);

    send_json(
        &mut client,
        &ClientToHub::ConnectDevice {
            device_id: "box-1".into(),
        },
    )
    .await;
    loop {
        match recv_json::<HubToClient>(&mut client).await {
            HubToClient::RelayStream { device_id, .. } => {
                assert_eq!(device_id, "box-1");
                break;
            }
            HubToClient::DirectHint { .. } => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    // Engine -> hub -> client: the keyframe.
    let frame = recv_binary(&mut client).await;
    assert_eq!(frame, keyframe(1));
    let Nsl1::Frame { keyframe: true, .. } = Nsl1::decode(&frame).unwrap() else {
        panic!("expected keyframe");
    };
    assert_eq!(
        engine.uris.lock().unwrap().as_slice(),
        ["/stream?take=1&clipboard=1"]
    );

    assert_eq!(recv_binary(&mut client).await, monitor_list());
    assert_eq!(
        recv_binary(&mut client).await,
        clipboard("Copied on the host: café")
    );

    // Client -> hub -> engine: ACK, input, monitor selection and clipboard.
    let select = Nsl1::SelectMonitor {
        generation: 1,
        id: "display-1".into(),
    }
    .encode()
    .unwrap();
    for expected in [
        ack(1),
        input(),
        select,
        clipboard("Copied on the viewer: 雪"),
    ] {
        client
            .send(Message::Binary(expected.clone()))
            .await
            .unwrap();
        assert_eq!(
            timeout(WAIT, engine.received.recv())
                .await
                .unwrap()
                .unwrap(),
            expected
        );
    }

    // Disconnect: the agent closes the engine socket.
    send_json(&mut client, &ClientToHub::Disconnect).await;
    timeout(WAIT, engine.closed.recv()).await.unwrap().unwrap();
}

type HostWs = WebSocketStream<TcpStream>;

/// Next non-heartbeat control message from the agent.
async fn next_host_msg(host: &mut HostWs) -> HostToHub {
    loop {
        if let Message::Text(t) = timeout(WAIT, host.next()).await.unwrap().unwrap().unwrap() {
            let m: HostToHub = serde_json::from_str(t.as_str()).unwrap();
            if !matches!(m, HostToHub::Heartbeat { .. }) {
                return m;
            }
        }
    }
}

async fn next_binary(host: &mut HostWs) -> Bytes {
    loop {
        if let Message::Binary(b) = timeout(WAIT, host.next()).await.unwrap().unwrap().unwrap() {
            return b;
        }
    }
}

#[tokio::test]
async fn keyframe_request_close_and_reconnect() {
    let hub_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hub = hub_listener.local_addr().unwrap();
    let mut engine = mock_engine().await;
    let _agent = spawn_agent(hub, engine.addr);

    // First registration.
    let (stream, _) = timeout(WAIT, hub_listener.accept()).await.unwrap().unwrap();
    let mut host = accept_async(stream).await.unwrap();
    let Message::Text(t) = host.next().await.unwrap().unwrap() else {
        panic!("expected register");
    };
    let HostToHub::Register { token, device } = serde_json::from_str(t.as_str()).unwrap() else {
        panic!("expected register");
    };
    assert_eq!(token, "tok-a");
    assert_eq!(device.device_id, "box-1");
    let text = |m: &HubToHost| Message::Text(serde_json::to_string(m).unwrap().into());
    host.send(text(&HubToHost::Registered {
        heartbeat_interval_secs: 1,
    }))
    .await
    .unwrap();

    host.send(text(&HubToHost::OpenSession {
        session_id: "s1".into(),
    }))
    .await
    .unwrap();
    assert_eq!(
        next_host_msg(&mut host).await,
        HostToHub::SessionReady {
            session_id: "s1".into()
        }
    );
    assert_eq!(next_binary(&mut host).await, keyframe(1));
    assert_eq!(next_binary(&mut host).await, monitor_list());
    assert_eq!(
        next_binary(&mut host).await,
        clipboard("Copied on the host: café")
    );

    // Keyframe request: the engine is reconnected and emits a fresh keyframe; hub session stays.
    host.send(text(&HubToHost::RequestKeyframe {
        session_id: "s1".into(),
    }))
    .await
    .unwrap();
    assert_eq!(next_binary(&mut host).await, keyframe(2));
    assert_eq!(engine.connections.load(Ordering::SeqCst), 2);
    timeout(WAIT, engine.closed.recv()).await.unwrap().unwrap(); // old link closed

    // Hub bytes still reach the new engine link.
    host.send(Message::Binary(ack(2))).await.unwrap();
    assert_eq!(
        timeout(WAIT, engine.received.recv())
            .await
            .unwrap()
            .unwrap(),
        ack(2)
    );

    // CloseSession closes the engine socket.
    host.send(text(&HubToHost::CloseSession {
        session_id: "s1".into(),
    }))
    .await
    .unwrap();
    timeout(WAIT, engine.closed.recv()).await.unwrap().unwrap();

    // A hub close makes the agent reconnect and register again.
    host.close(None).await.unwrap();
    let (stream, _) = timeout(WAIT, hub_listener.accept()).await.unwrap().unwrap();
    let mut host = accept_async(stream).await.unwrap();
    let Message::Text(t) = host.next().await.unwrap().unwrap() else {
        panic!("expected register");
    };
    assert!(matches!(
        serde_json::from_str::<HostToHub>(t.as_str()).unwrap(),
        HostToHub::Register { .. }
    ));
}

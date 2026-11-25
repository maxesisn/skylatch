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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        .args(["--form-factor", "mini", "--room", "Office", "--slot", "3"])
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
    let device = timeout(WAIT, async {
        loop {
            send_json(&mut client, &ClientToHub::ListDevices).await;
            let HubToClient::Devices { devices } = recv_json(&mut client).await else {
                panic!("expected devices");
            };
            if let Some(d) = devices.into_iter().next() {
                break d;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("agent did not register");
    assert_eq!(device.device_id, "box-1");
    assert_eq!(device.name, "Test Box");
    assert_eq!(device.os, std::env::consts::OS);
    assert_eq!(device.direct_endpoints.len(), 1);
    assert_eq!(device.direct_endpoints[0].port, 17900);
    assert_eq!(device.form_factor, DeviceFormFactor::Mini);
    assert_eq!(device.room.as_deref(), Some("Office"));
    assert_eq!(device.slot, Some(3));
    assert!(!device.monitor_list.is_empty());
    assert_eq!(device.monitors as usize, device.monitor_list.len());

    // The HTTP directory serves the same registration, including the complete monitor list.
    let response = timeout(WAIT, async {
        let mut stream = TcpStream::connect(hub).await.unwrap();
        stream.write_all(b"GET /api/v1/devices HTTP/1.1\r\nHost: test\r\nAuthorization: Bearer tok-a\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }).await.expect("device directory request timed out");
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    assert!(headers.starts_with("HTTP/1.1 200"));
    let listing: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(listing["devices"], serde_json::json!([device]));

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

    // A hub close makes the agent reconnect with the same device metadata.
    host.close(None).await.unwrap();
    let (stream, _) = timeout(WAIT, hub_listener.accept()).await.unwrap().unwrap();
    let mut host = accept_async(stream).await.unwrap();
    let Message::Text(t) = host.next().await.unwrap().unwrap() else {
        panic!("expected register");
    };
    assert_eq!(
        serde_json::from_str::<HostToHub>(t.as_str()).unwrap(),
        HostToHub::Register { token, device }
    );
}

struct ConfigDirectory(std::path::PathBuf);

impl ConfigDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "skylatch-test-{}",
            skylatch_proto::credentials::random_token().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_skylatch-agent"));
        command
            .env("RUST_LOG", "info")
            .env("NO_COLOR", "1")
            .env("HOME", &self.0)
            .env("APPDATA", &self.0)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("SKYLATCH_HUB")
            .env_remove("SKYLATCH_TOKEN")
            .env_remove("SKYLATCH_PASSWORD")
            .env_remove("SKYLATCH_ENGINE_BIN")
            .env_remove("SKYLATCH_DEVICE_NAME")
            .env_remove("SKYLATCH_DEVICE_ID")
            .kill_on_drop(true);
        command
    }

    fn path(&self) -> std::path::PathBuf {
        let dir = if cfg!(windows) {
            "Skylatch"
        } else if cfg!(target_os = "macos") {
            "Library/Application Support/Skylatch"
        } else {
            ".config/skylatch"
        };
        self.0.join(dir).join("agent.json")
    }
}

impl Drop for ConfigDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn join_saves_config_and_direct_listener_authenticates_and_streams() {
    use skylatch_client_core::{create_pairing_code, Event, SessionClient, TransportMode};
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Error};

    let cfg = HubConfig::new(vec![("demo".into(), "example-static-token".into())]);
    let state = AppState::with_auth(
        cfg,
        None,
        vec![("demo".into(), "example passphrase".into())],
    )
    .unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let hub = listener.local_addr().unwrap();
    let _hub_task = tokio::spawn(async move {
        axum::serve(listener, router(state)).await.unwrap();
    });
    let origin = format!("http://{hub}");
    let joined = ConfigDirectory::new();
    let child = joined
        .command()
        .args(["join", &origin, "--device-id", "first", "--name", "First"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = child;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"demo\nexample passphrase\n")
        .await
        .unwrap();
    let result = timeout(WAIT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let path = joined.path();
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(!saved.contains("example passphrase"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let config = ConfigDirectory::new();
    let pair = create_pairing_code(&origin, "example-static-token")
        .await
        .unwrap();
    let output = timeout(
        WAIT,
        config
            .command()
            .args([
                "join",
                &origin,
                "--code",
                &pair.code,
                "--device-id",
                "paired",
                "--name",
                "Paired",
                "--room",
                "Example room",
                "--slot",
                "2",
            ])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config.path()).unwrap()).unwrap();
    let token = config_json["device_token"].as_str().unwrap();
    assert_ne!(token, "example-static-token");
    assert_eq!(config_json["slot"], 2);

    let mut engine = mock_engine().await;
    let mut agent = config
        .command()
        .args([
            "--engine-url",
            &format!("ws://{}", engine.addr),
            "--enable-direct",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let device = timeout(WAIT, async {
        loop {
            let devices = skylatch_client_core::list_devices(&origin, "example-static-token")
                .await
                .unwrap();
            if let Some(device) = devices.into_iter().find(|d| d.device_id == "paired") {
                break device;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(device.name, "Paired");
    assert_eq!(device.room.as_deref(), Some("Example room"));
    assert_eq!(device.slot, Some(2));
    let port = if let Some(endpoint) = device.direct_endpoints.first() {
        for endpoint in &device.direct_endpoints {
            assert!(endpoint
                .host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| !ip.is_loopback()));
            assert_eq!(endpoint.port, device.direct_endpoints[0].port);
        }
        endpoint.port
    } else {
        // The listener remains usable from loopback on machines with no other IPv4 interfaces.
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(agent.stdout.take().unwrap()).lines();
        timeout(WAIT, async {
            while let Some(line) = lines.next_line().await.unwrap() {
                let line = line.replace("\x1b[0m", "").replace("\x1b[3m", "");
                if line.contains("direct listener enabled") {
                    if let Some((_, port)) = line.rsplit_once("port=") {
                        return port.trim().parse().unwrap();
                    }
                }
            }
            panic!("missing direct port");
        })
        .await
        .unwrap()
    };
    let direct = format!("ws://{}:{port}/stream", std::net::Ipv4Addr::LOCALHOST);
    let mut unauth = direct.as_str().into_client_request().unwrap();
    unauth
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("nsl1"));
    assert!(
        matches!(connect_async(unauth).await, Err(Error::Http(response)) if matches!(response.status().as_u16(), 401 | 503))
    );
    let mut authenticated = direct.as_str().into_client_request().unwrap();
    authenticated.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str(&format!("nsl1, bearer.{token}")).unwrap(),
    );
    let (mut socket, response) = timeout(WAIT, async {
        loop {
            match connect_async(authenticated.clone()).await {
                Err(Error::Http(response)) if response.status() == 503 => {
                    tokio::time::sleep(Duration::from_millis(30)).await
                }
                result => break result.unwrap(),
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(response.headers()["sec-websocket-protocol"], "nsl1");
    assert_eq!(recv_binary(&mut socket).await, keyframe(1));
    socket.send(Message::Binary(input())).await.unwrap();
    assert_eq!(
        timeout(WAIT, engine.received.recv())
            .await
            .unwrap()
            .unwrap(),
        input()
    );
    socket.close(None).await.unwrap();
    timeout(WAIT, engine.closed.recv()).await.unwrap().unwrap();

    if !device.direct_endpoints.is_empty() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = SessionClient::connect_hub(
            &origin,
            "example-static-token",
            "paired",
            true,
            move |event| {
                let _ = tx.send(event);
            },
        )
        .await
        .unwrap();
        assert_eq!(session.transport_mode(), TransportMode::Direct);
        assert!(matches!(
            timeout(WAIT, rx.recv()).await.unwrap(),
            Some(Event::Frame(_))
        ));

        let mut revoke = TcpStream::connect(hub).await.unwrap();
        revoke.write_all(b"DELETE /api/v1/devices/paired/token HTTP/1.1\r\nHost: example.test\r\nAuthorization: Bearer example-static-token\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut response = String::new();
        timeout(WAIT, revoke.read_to_string(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(response.starts_with("HTTP/1.1 204"));
        assert!(matches!(
            timeout(WAIT, rx.recv()).await.unwrap(),
            Some(Event::Disconnected(_))
        ));
        assert!(
            matches!(connect_async(authenticated).await, Err(Error::Http(response)) if response.status() == 503)
        );
    }
}

async fn record_request(
    axum::extract::State(seen): axum::extract::State<Arc<Mutex<Vec<String>>>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let (parts, body) = request.into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    seen.lock().unwrap().push(format!(
        "{} {:?} {}",
        parts.uri,
        parts.headers,
        String::from_utf8_lossy(&body)
    ));
    next.run(axum::extract::Request::from_parts(
        parts,
        axum::body::Body::from(body),
    ))
    .await
}

#[tokio::test]
async fn join_code_with_invite_secret_writes_the_managed_mesh_profile() {
    use skylatch_client_core::mesh_vault::{format_join_code, seal_mesh_invite, InviteSecret};
    use skylatch_client_core::{create_pairing_code_with_invite, MeshVault};

    let mut cfg = HubConfig::new(vec![("demo".into(), "example-static-token".into())]);
    cfg.easytier_peers = vec!["tcp://relay.example.com:11010".into()];
    let state = AppState::with_auth(cfg, None, vec![]).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let app = router(state).layer(axum::middleware::from_fn_with_state(
        seen.clone(),
        record_request,
    ));
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let _hub_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let vault = MeshVault::new_default().unwrap();
    let mut profile = vault.next_isolated_defaults().unwrap();
    profile.custom_peers = vec!["udp://own.example.com:11010".into()];
    let secret = InviteSecret::generate().unwrap();
    let invite = seal_mesh_invite(&profile, &secret).unwrap();
    let pair = create_pairing_code_with_invite(&origin, "example-static-token", &invite)
        .await
        .unwrap();
    let join_code = format_join_code(&pair.code, &secret);

    let config = ConfigDirectory::new();
    let output = timeout(
        WAIT,
        config
            .command()
            .args([
                "join",
                &origin,
                "--code",
                &join_code,
                "--device-id",
                "lab-vm",
                "--name",
                "Lab VM",
            ])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}{stderr}");

    let secret_symbols = secret.display().replace('-', "");
    for leak in [
        profile.network_secret.as_str(),
        &secret.display(),
        &secret_symbols,
    ] {
        assert!(
            !stdout.contains(leak) && !stderr.contains(leak),
            "{stdout}{stderr}"
        );
        for request in seen.lock().unwrap().iter() {
            assert!(!request.contains(leak), "hub saw a secret: {request}");
        }
    }
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|r| r.contains(&invite.ct_b64)));
    assert!(seen.iter().any(
        |r| r.contains("/api/v1/enroll") && r.contains(&format!("\"code\":\"{}\"", pair.code))
    ));
    assert!(seen.iter().any(|r| r.starts_with("/api/v1/info")));
    let status = stdout.lines().last().unwrap();
    assert!(
        status.starts_with(&format!(
            "Mesh network \"{}\" ({})",
            profile.name, profile.subnet
        )),
        "{stdout}"
    );

    let app_dir = config.path().parent().unwrap().to_path_buf();
    let toml_path = app_dir
        .join("easytier")
        .join(format!("net-{}.toml", profile.id));
    let written = std::fs::read_to_string(&toml_path).unwrap();
    assert!(written.contains(&profile.network_secret));
    assert!(written.contains("uri = \"tcp://relay.example.com:11010\""));
    assert!(written.contains("uri = \"udp://own.example.com:11010\""));
    assert!(written.contains(&format!("tcp://0.0.0.0:{}", profile.listen_port)));
    assert!(written.contains("dhcp = true"));
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(app_dir.join("mesh-state.json")).unwrap()).unwrap();
    assert_eq!(state["binding"]["mode"], "managed");
    assert_eq!(
        state["binding"]["active_profile_ids"][0],
        profile.id.as_str()
    );
    assert!(!state.to_string().contains(&profile.network_secret));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&toml_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let agent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(config.path()).unwrap()).unwrap();
    assert_eq!(agent["device_id"], "lab-vm");
}

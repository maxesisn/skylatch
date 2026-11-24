use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use skylatch_client_core::{
    ffi::{
        skylatch_connect_direct, skylatch_connect_hub, skylatch_destroy, skylatch_free_string,
        skylatch_list_devices_json, skylatch_proto_version, skylatch_select_monitor,
        skylatch_send_ack, skylatch_send_clipboard, skylatch_send_input,
        skylatch_set_sl1_callbacks, SkylatchCallbacks, SkylatchClient, SkylatchSl1Callbacks,
        SKYLATCH_ERR_INVALID, SKYLATCH_ERR_NOT_READY, SKYLATCH_ERR_NULL, SKYLATCH_ERR_TOO_LARGE,
        SKYLATCH_OK,
    },
    list_devices, ClientError, Event, Frame, SessionClient, TransportMode,
};
use skylatch_hub::{router, AppState, HubConfig};
use skylatch_proto::{
    control::{DeviceInfo, DirectEndpoint, EndpointKind, HostToHub, HubToHost},
    nsl1::{Cursor, InputEvent, InputRecord, Message, MonitorInfo, MAX_CLIPBOARD_DATA},
};
use std::{
    ffi::{c_char, c_void, CStr, CString},
    future::Future,
    net::Ipv4Addr,
    ptr,
    time::Duration,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    task::JoinHandle,
    time::timeout,
};
use tokio_tungstenite::{
    accept_hdr_async, connect_async,
    tungstenite::{
        handshake::server::{Request, Response},
        http::{header, HeaderValue},
        Message as WsMessage,
    },
    MaybeTlsStream, WebSocketStream,
};

const ACCOUNT: &str = "test-account";
const HUB_TOKEN: &str = "example-hub-token";
const DEVICE: &str = "test-device";
const GENERATION: u32 = 17;
const WAIT: u32 = 90;
const VIDEO: &[u8] = &[0, 0, 0, 1, 0x65, 0x01];

type HostSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn deadline<T>(future: impl Future<Output = T>) -> T {
    timeout(Duration::from_secs(5), future)
        .await
        .expect("test timed out")
}

async fn loopback() -> TcpListener {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap()
}

struct Hub {
    url: String,
    task: JoinHandle<()>,
}

impl Hub {
    async fn start() -> Self {
        let listener = loopback().await;
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let config = HubConfig::new(vec![(ACCOUNT.into(), HUB_TOKEN.into())]);
        let task = tokio::spawn(async move {
            axum::serve(listener, router(AppState::new(config)))
                .await
                .unwrap();
        });
        Self { url, task }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn device(endpoints: Vec<DirectEndpoint>) -> DeviceInfo {
    DeviceInfo {
        device_id: DEVICE.into(),
        name: "Example host".into(),
        os: "linux".into(),
        host_version: "0.1.0".into(),
        monitors: 2,
        direct_endpoints: endpoints,
        form_factor: Default::default(),
        monitor_list: vec![],
        room: None,
        slot: None,
        hardware_model: None,
    }
}

fn endpoint(port: u16) -> DirectEndpoint {
    DirectEndpoint {
        kind: EndpointKind::Lan,
        host: Ipv4Addr::LOCALHOST.to_string(),
        port,
    }
}

async fn register(hub: &Hub, device: DeviceInfo) -> HostSocket {
    let (mut ws, _) = deadline(connect_async(format!("{}/ws/host", hub.url)))
        .await
        .unwrap();
    host_send(
        &mut ws,
        HostToHub::Register {
            token: HUB_TOKEN.into(),
            device,
        },
    )
    .await;
    assert!(matches!(
        host_recv(&mut ws).await,
        HubToHost::Registered { .. }
    ));
    ws
}

async fn host_send(ws: &mut HostSocket, msg: HostToHub) {
    ws.send(WsMessage::Text(serde_json::to_string(&msg).unwrap().into()))
        .await
        .unwrap();
}

async fn host_recv(ws: &mut HostSocket) -> HubToHost {
    let msg = deadline(ws.next()).await.unwrap().unwrap();
    let WsMessage::Text(text) = msg else {
        panic!("expected hub control: {msg:?}")
    };
    serde_json::from_str(text.as_str()).unwrap()
}

async fn open_session(ws: &mut HostSocket) -> String {
    let HubToHost::OpenSession { session_id } = host_recv(ws).await else {
        panic!("missing open_session")
    };
    host_send(
        ws,
        HostToHub::SessionReady {
            session_id: session_id.clone(),
        },
    )
    .await;
    session_id
}

async fn assert_relay_closed(ws: &mut HostSocket, id: String) {
    assert_eq!(
        host_recv(ws).await,
        HubToHost::CloseSession { session_id: id }
    );
}

fn video() -> Message {
    Message::Frame {
        generation: GENERATION,
        sequence: 1,
        width: 640,
        height: 360,
        capture_us: 10,
        keyframe: true,
        data: Bytes::from_static(VIDEO),
    }
}

fn displays() -> Vec<MonitorInfo> {
    vec![MonitorInfo {
        id: "output-雪".into(),
        name: "Écran".into(),
        x: -640,
        y: 0,
        width: 640,
        height: 360,
        primary: false,
        active: true,
    }]
}

fn frame() -> Frame {
    Frame {
        generation: GENERATION,
        sequence: 1,
        width: 640,
        height: 360,
        capture_us: 10,
        is_keyframe: true,
        annex_b: Bytes::from_static(VIDEO),
    }
}

fn input() -> InputRecord {
    InputRecord::new(InputEvent::Wheel {
        axis: skylatch_proto::nsl1::WheelAxis::Horizontal,
        delta: -120,
        x: 0x1234,
        y: 0x5678,
    })
    .with_masks(5, 0x81)
}

async fn assert_wire<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<S>,
    expected: Message,
) {
    assert_eq!(
        deadline(ws.next()).await.unwrap().unwrap(),
        WsMessage::Binary(expected.encode().unwrap())
    );
}

async fn assert_outbound<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<S>,
) {
    let ack = Message::Ack {
        generation: GENERATION,
        sequence: 1,
        viewer_wait_us: Some(WAIT),
    };
    let input = Message::Input {
        generation: GENERATION,
        sequence: 0,
        record: input(),
    };
    for expected in [ack, input] {
        assert_wire(ws, expected).await;
    }
}

async fn assert_frame(events: &mut mpsc::UnboundedReceiver<Event>) {
    let Some(Event::Frame(actual)) = deadline(events.recv()).await else {
        panic!("expected frame callback")
    };
    assert_eq!(actual, frame());
}

#[allow(clippy::result_large_err)] // The WebSocket handshake callback has a fixed error type.
async fn start_direct(listener: TcpListener, token: Option<String>) -> WebSocketStream<TcpStream> {
    let (stream, _) = deadline(listener.accept()).await.unwrap();
    let expected = token.map_or_else(
        || "nsl1".to_string(),
        |token| format!("nsl1, bearer.{token}"),
    );
    deadline(accept_hdr_async(
        stream,
        |req: &Request, mut res: Response| {
            assert_eq!(
                req.uri().path_and_query().unwrap().as_str(),
                "/stream?take=1&clipboard=1"
            );
            assert_eq!(
                req.headers().get(header::SEC_WEBSOCKET_PROTOCOL).unwrap(),
                expected.as_str()
            );
            assert!(!req.headers().contains_key(header::ORIGIN));
            res.headers_mut().insert(
                header::SEC_WEBSOCKET_PROTOCOL,
                HeaderValue::from_static("nsl1"),
            );
            Ok(res)
        },
    ))
    .await
    .unwrap()
}

#[tokio::test]
async fn discovers_registered_devices_and_rejects_bad_auth() {
    let hub = Hub::start().await;
    let info = device(vec![endpoint(7001)]);
    let _host = register(&hub, info.clone()).await;
    let url = hub.url.replacen("ws://", "http://", 1);
    assert_eq!(
        deadline(list_devices(&url, HUB_TOKEN)).await.unwrap(),
        vec![info]
    );
    assert!(
        matches!(deadline(list_devices(&hub.url, "not-the-test-token")).await,
        Err(ClientError::Hub { code, .. }) if code == "unauthorized")
    );
}

#[tokio::test]
async fn hub_relay_delivers_callbacks_and_exact_ack_and_input() {
    let hub = Hub::start().await;
    let mut host = register(&hub, device(vec![])).await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = deadline(SessionClient::connect_hub(
        &hub.url,
        HUB_TOKEN,
        DEVICE,
        false,
        move |event| {
            let _ = tx.send(event);
        },
    ))
    .await
    .unwrap();
    assert_eq!(client.transport_mode(), TransportMode::Relay);
    assert!(matches!(
        client.send_input(input().encode().unwrap()),
        Err(skylatch_client_core::SendError::NotReady)
    ));
    let id = open_session(&mut host).await;
    let cursor = Cursor::Image {
        width: 2,
        height: 1,
        hot_x: 1,
        hot_y: 0,
        rgba: Bytes::from_static(&[0, 1, 2, 3, 4, 5, 6, 7]),
    };
    for msg in [
        video(),
        Message::Cursor {
            generation: GENERATION,
            cursor: cursor.clone(),
        },
        Message::Cursor {
            generation: GENERATION,
            cursor: Cursor::Reset,
        },
        Message::ClipboardNotice {
            generation: GENERATION,
        },
    ] {
        host.send(WsMessage::Binary(msg.encode().unwrap()))
            .await
            .unwrap();
    }
    assert_frame(&mut rx).await;
    assert!(matches!(deadline(rx.recv()).await, Some(Event::Cursor(actual)) if actual == cursor));
    assert!(matches!(
        deadline(rx.recv()).await,
        Some(Event::Cursor(Cursor::Reset))
    ));
    assert!(matches!(
        deadline(rx.recv()).await,
        Some(Event::ClipboardNotice)
    ));
    client.send_ack(GENERATION, 1, WAIT).unwrap();
    client.send_input(input().encode().unwrap()).unwrap();
    assert_outbound(&mut host).await;
    client.disconnect();
    assert_relay_closed(&mut host, id).await;
    assert!(matches!(
        deadline(rx.recv()).await,
        Some(Event::Disconnected(Ok(())))
    ));
    assert!(deadline(rx.recv()).await.is_none());
}

#[tokio::test]
async fn failed_direct_tcp_and_upgrade_keep_relay_usable() {
    let hub = Hub::start().await;
    let closed = loopback().await;
    let unreachable = endpoint(closed.local_addr().unwrap().port());
    let stalled = loopback().await;
    let stalled_endpoint = endpoint(stalled.local_addr().unwrap().port());
    drop(closed);
    // A listening port that never answers the WebSocket upgrade must not hold up fallback.
    let mut host = register(&hub, device(vec![unreachable, stalled_endpoint])).await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = deadline(SessionClient::connect_hub(
        &hub.url,
        HUB_TOKEN,
        DEVICE,
        true,
        move |event| {
            let _ = tx.send(event);
        },
    ))
    .await
    .unwrap();
    assert_eq!(client.transport_mode(), TransportMode::Relay);
    let id = open_session(&mut host).await;
    host.send(WsMessage::Binary(video().encode().unwrap()))
        .await
        .unwrap();
    assert_frame(&mut rx).await;
    drop(client);
    assert_relay_closed(&mut host, id).await;
}

#[tokio::test]
async fn successful_fast_path_closes_hub_relay_and_uses_direct_stream() {
    let hub = Hub::start().await;
    let listener = loopback().await;
    let mut host = register(
        &hub,
        device(vec![endpoint(listener.local_addr().unwrap().port())]),
    )
    .await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (client, mut direct) = tokio::join!(
        deadline(SessionClient::connect_hub(
            &hub.url,
            HUB_TOKEN,
            DEVICE,
            true,
            move |event| {
                let _ = tx.send(event);
            }
        )),
        start_direct(listener, None)
    );
    let client = client.unwrap();
    assert_eq!(client.transport_mode(), TransportMode::Direct);
    let HubToHost::OpenSession { session_id } = host_recv(&mut host).await else {
        panic!("missing open_session")
    };
    assert_relay_closed(&mut host, session_id).await;
    direct
        .send(WsMessage::Binary(video().encode().unwrap()))
        .await
        .unwrap();
    assert_frame(&mut rx).await;
    client.send_ack(GENERATION, 1, WAIT).unwrap();
    client.send_input(input().encode().unwrap()).unwrap();
    assert_outbound(&mut direct).await;
    drop(client);
    assert!(matches!(
        deadline(direct.next()).await,
        Some(Ok(WsMessage::Close(_)))
    ));
}

#[tokio::test]
async fn host_session_close_surfaces_reason() {
    let hub = Hub::start().await;
    let mut host = register(&hub, device(vec![])).await;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _client = deadline(SessionClient::connect_hub(
        &hub.url,
        HUB_TOKEN,
        DEVICE,
        true,
        move |event| {
            let _ = tx.send(event);
        },
    ))
    .await
    .unwrap();
    let session_id = open_session(&mut host).await;
    host_send(&mut host, HostToHub::SessionClosed { session_id }).await;
    assert!(
        matches!(deadline(rx.recv()).await, Some(Event::Disconnected(Err(ClientError::SessionClosed(reason)))) if reason == "host_closed")
    );
}

#[derive(Debug, PartialEq, Eq)]
enum NativeEvent {
    Frame(Frame),
    MonitorList(Vec<u8>),
    ClipboardData(Vec<u8>),
    Disconnected(Option<String>),
}

unsafe extern "C" fn native_frame(
    data: *mut c_void,
    generation: u32,
    sequence: u32,
    width: u16,
    height: u16,
    capture_us: u64,
    is_keyframe: bool,
    bytes: *const u8,
    len: usize,
) {
    let tx = unsafe { &*data.cast::<mpsc::UnboundedSender<NativeEvent>>() };
    let annex_b = Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(bytes, len) });
    let _ = tx.send(NativeEvent::Frame(Frame {
        generation,
        sequence,
        width,
        height,
        capture_us,
        is_keyframe,
        annex_b,
    }));
}

unsafe extern "C" fn native_disconnected(data: *mut c_void, error: *const c_char) {
    let tx = unsafe { &*data.cast::<mpsc::UnboundedSender<NativeEvent>>() };
    let error = (!error.is_null()).then(|| {
        unsafe { CStr::from_ptr(error) }
            .to_string_lossy()
            .into_owned()
    });
    let _ = tx.send(NativeEvent::Disconnected(error));
}

unsafe extern "C" fn native_monitors(data: *mut c_void, json: *const c_char, len: usize) {
    let tx = unsafe { &*data.cast::<mpsc::UnboundedSender<NativeEvent>>() };
    let bytes = unsafe { std::slice::from_raw_parts(json.cast::<u8>(), len) };
    let _ = tx.send(NativeEvent::MonitorList(bytes.to_vec()));
}

unsafe extern "C" fn native_clipboard(data: *mut c_void, text: *const c_char, len: usize) {
    let tx = unsafe { &*data.cast::<mpsc::UnboundedSender<NativeEvent>>() };
    let bytes = unsafe { std::slice::from_raw_parts(text.cast::<u8>(), len) };
    let _ = tx.send(NativeEvent::ClipboardData(bytes.to_vec()));
}

const NATIVE_CALLBACKS: SkylatchCallbacks = SkylatchCallbacks {
    on_frame: Some(native_frame),
    on_cursor: None,
    on_clipboard_notice: None,
    on_disconnected: Some(native_disconnected),
};

struct NativeHandle(*mut SkylatchClient);

impl Drop for NativeHandle {
    fn drop(&mut self) {
        unsafe { skylatch_destroy(self.0) };
    }
}

fn c_transaction(
    url: String,
    token: String,
    direct: bool,
    tx: mpsc::UnboundedSender<NativeEvent>,
    mut destroy: mpsc::Receiver<()>,
) -> JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let url = CString::new(url).unwrap();
        let token = CString::new(token).unwrap();
        let device_id = CString::new(DEVICE).unwrap();
        let data = ptr::from_ref(&tx).cast_mut().cast();
        let mut error = [1 as c_char; 256];
        unsafe {
            let client = if direct {
                skylatch_connect_direct(
                    url.as_ptr(),
                    token.as_ptr(),
                    &NATIVE_CALLBACKS,
                    data,
                    error.as_mut_ptr(),
                    error.len(),
                )
            } else {
                skylatch_connect_hub(
                    url.as_ptr(),
                    token.as_ptr(),
                    device_id.as_ptr(),
                    false,
                    &NATIVE_CALLBACKS,
                    data,
                    error.as_mut_ptr(),
                    error.len(),
                )
            };
            assert!(
                !client.is_null(),
                "{}",
                CStr::from_ptr(error.as_ptr()).to_string_lossy()
            );
            let handle = NativeHandle(client);
            assert_eq!(error[0], 0);
            // Waiting for the frame callback from the async side proves the generation has arrived.
            assert_eq!(destroy.blocking_recv(), Some(()));
            assert_eq!(skylatch_send_ack(client, 0, 1, WAIT), SKYLATCH_ERR_INVALID);
            assert_eq!(skylatch_send_ack(client, GENERATION, 1, WAIT), SKYLATCH_OK);
            assert_eq!(
                skylatch_send_input(client, input().encode().unwrap().as_ptr()),
                SKYLATCH_OK
            );
            // Wait for the server to actually see the queued packets before destroying.
            assert_eq!(destroy.blocking_recv(), Some(()));
            drop(handle);
        }
    })
}

#[tokio::test]
async fn direct_only_c_abi_uses_standalone_path_and_bearer_subprotocol() {
    let listener = loopback().await;
    let url = format!(
        "ws://{}/stream?take=1&clipboard=1",
        listener.local_addr().unwrap()
    );
    let token = "ab".repeat(32);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (action, control) = mpsc::channel(1);
    let task = c_transaction(url, token.clone(), true, tx, control);
    let mut ws = start_direct(listener, Some(token)).await;
    ws.send(WsMessage::Binary(video().encode().unwrap()))
        .await
        .unwrap();
    assert_eq!(deadline(rx.recv()).await, Some(NativeEvent::Frame(frame())));
    action.send(()).await.unwrap();
    assert_outbound(&mut ws).await;
    action.send(()).await.unwrap();
    assert!(matches!(
        deadline(ws.next()).await,
        Some(Ok(WsMessage::Close(_)))
    ));
    deadline(task).await.unwrap();
    assert_eq!(
        deadline(rx.recv()).await,
        Some(NativeEvent::Disconnected(None))
    );
    assert_eq!(deadline(rx.recv()).await, None);
}

#[tokio::test]
async fn hub_c_abi_discovery_connect_send_and_destroy() {
    let hub = Hub::start().await;
    let info = device(vec![]);
    let mut host = register(&hub, info.clone()).await;
    let url = CString::new(hub.url.clone()).unwrap();
    deadline(tokio::task::spawn_blocking(move || unsafe {
        let token = CString::new(HUB_TOKEN).unwrap();
        let mut error = [1 as c_char; 128];
        assert_eq!(skylatch_proto_version(), 1);
        assert_eq!(
            skylatch_send_input(ptr::null_mut(), ptr::null()),
            SKYLATCH_ERR_NULL
        );
        let json = skylatch_list_devices_json(
            url.as_ptr(),
            token.as_ptr(),
            error.as_mut_ptr(),
            error.len(),
        );
        assert!(
            !json.is_null(),
            "{}",
            CStr::from_ptr(error.as_ptr()).to_string_lossy()
        );
        assert_eq!(error[0], 0);
        let devices =
            serde_json::from_slice::<Vec<DeviceInfo>>(CStr::from_ptr(json).to_bytes()).unwrap();
        skylatch_free_string(json);
        assert_eq!(devices, vec![info]);
        assert!(skylatch_list_devices_json(
            url.as_ptr(),
            ptr::null(),
            error.as_mut_ptr(),
            error.len()
        )
        .is_null());
        assert_eq!(
            CStr::from_ptr(error.as_ptr()).to_str().unwrap(),
            "hub token is null"
        );
    }))
    .await
    .unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (action, control) = mpsc::channel(1);
    let task = c_transaction(hub.url.clone(), HUB_TOKEN.into(), false, tx, control);
    let id = open_session(&mut host).await;
    host.send(WsMessage::Binary(video().encode().unwrap()))
        .await
        .unwrap();
    assert_eq!(deadline(rx.recv()).await, Some(NativeEvent::Frame(frame())));
    action.send(()).await.unwrap();
    assert_outbound(&mut host).await;
    action.send(()).await.unwrap();
    assert_relay_closed(&mut host, id).await;
    deadline(task).await.unwrap();
    assert_eq!(
        deadline(rx.recv()).await,
        Some(NativeEvent::Disconnected(None))
    );
    assert_eq!(deadline(rx.recv()).await, None);
}

#[tokio::test]
async fn hub_relay_exchanges_sl1_through_separate_c_abi_callbacks() {
    let hub = Hub::start().await;
    let mut host = register(&hub, device(vec![])).await;
    let (legacy_tx, mut legacy) = mpsc::unbounded_channel();
    let (sl1_tx, mut sl1) = mpsc::unbounded_channel();
    let (ready, mut waiting) = mpsc::channel(1);
    let (commands, mut run) = mpsc::channel(1);
    let url = hub.url.clone();
    let host_clipboard = "host: café\0雪";
    let viewer_clipboard = "viewer: 雪\0café";
    let task = tokio::task::spawn_blocking(move || unsafe {
        let url = CString::new(url).unwrap();
        let token = CString::new(HUB_TOKEN).unwrap();
        let id = CString::new(DEVICE).unwrap();
        let client = skylatch_connect_hub(
            url.as_ptr(),
            token.as_ptr(),
            id.as_ptr(),
            false,
            &NATIVE_CALLBACKS,
            ptr::from_ref(&legacy_tx).cast_mut().cast(),
            ptr::null_mut(),
            0,
        );
        assert!(!client.is_null());
        let _handle = NativeHandle(client);
        assert_eq!(
            skylatch_send_clipboard(client, ptr::null(), 0),
            SKYLATCH_ERR_NOT_READY
        );
        ready.blocking_send(()).unwrap();
        // Register only after the old frame callback, so the test covers initial-list replay.
        assert_eq!(run.blocking_recv(), Some(()));
        let mut callbacks = SkylatchSl1Callbacks {
            on_monitor_list: Some(native_monitors),
            on_clipboard_data: Some(native_clipboard),
        };
        assert_eq!(
            skylatch_set_sl1_callbacks(
                client,
                &callbacks,
                ptr::from_ref(&sl1_tx).cast_mut().cast()
            ),
            SKYLATCH_OK
        );
        callbacks.on_clipboard_data = None;
        std::hint::black_box(callbacks); // Changing the caller's table must not change the copy.
        assert_eq!(run.blocking_recv(), Some(()));
        let monitor_id = CString::new("output-雪").unwrap();
        let bad = [0xffu8, 0];
        assert_eq!(
            skylatch_select_monitor(client, bad.as_ptr().cast()),
            SKYLATCH_ERR_INVALID
        );
        assert_eq!(
            skylatch_select_monitor(client, c"".as_ptr()),
            SKYLATCH_ERR_INVALID
        );
        assert_eq!(
            skylatch_send_clipboard(client, bad.as_ptr().cast(), 1),
            SKYLATCH_ERR_INVALID
        );
        let oversized = "x".repeat(MAX_CLIPBOARD_DATA + 1);
        assert_eq!(
            skylatch_send_clipboard(client, oversized.as_ptr().cast(), oversized.len()),
            SKYLATCH_ERR_TOO_LARGE
        );
        assert_eq!(
            skylatch_select_monitor(client, monitor_id.as_ptr()),
            SKYLATCH_OK
        );
        assert_eq!(
            skylatch_send_clipboard(
                client,
                viewer_clipboard.as_ptr().cast(),
                viewer_clipboard.len()
            ),
            SKYLATCH_OK
        );
        assert_eq!(skylatch_send_clipboard(client, ptr::null(), 0), SKYLATCH_OK);
        assert_eq!(run.blocking_recv(), Some(()));
        assert_eq!(
            skylatch_set_sl1_callbacks(client, ptr::null(), ptr::null_mut()),
            SKYLATCH_OK
        );
    });
    let id = open_session(&mut host).await;
    deadline(waiting.recv()).await.unwrap();
    for msg in [
        Message::MonitorList {
            generation: GENERATION,
            monitors: displays(),
        },
        video(),
    ] {
        host.send(WsMessage::Binary(msg.encode().unwrap()))
            .await
            .unwrap();
    }
    assert_eq!(
        deadline(legacy.recv()).await,
        Some(NativeEvent::Frame(frame()))
    );
    commands.send(()).await.unwrap();
    let Some(NativeEvent::MonitorList(json)) = deadline(sl1.recv()).await else {
        panic!("expected the C monitor callback");
    };
    assert_eq!(
        serde_json::from_slice::<Vec<MonitorInfo>>(&json).unwrap(),
        displays()
    );

    // The later incoming clipboard packet also advances the generation, which outgoing SL1 uses.
    let generation = GENERATION + 1;
    let incoming = Message::ClipboardData {
        generation,
        text: host_clipboard.into(),
    };
    host.send(WsMessage::Binary(incoming.encode().unwrap()))
        .await
        .unwrap();
    assert_eq!(
        deadline(sl1.recv()).await,
        Some(NativeEvent::ClipboardData(host_clipboard.as_bytes().into()))
    );
    commands.send(()).await.unwrap();
    for expected in [
        Message::SelectMonitor {
            generation,
            id: "output-雪".into(),
        },
        Message::ClipboardData {
            generation,
            text: viewer_clipboard.into(),
        },
        Message::ClipboardData {
            generation,
            text: String::new(),
        },
    ] {
        assert_wire(&mut host, expected).await;
    }
    commands.send(()).await.unwrap();
    assert_relay_closed(&mut host, id).await;
    deadline(task).await.unwrap();
    assert_eq!(deadline(sl1.recv()).await, None);
    assert_eq!(
        deadline(legacy.recv()).await,
        Some(NativeEvent::Disconnected(None))
    );
}

#[tokio::test]
async fn direct_session_exchanges_sl1_without_http() {
    let listener = loopback().await;
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (client, mut host) = tokio::join!(
        deadline(SessionClient::connect_direct(&url, None, move |event| {
            let _ = tx.send(event);
        })),
        start_direct(listener, None)
    );
    let client = client.unwrap();
    let text = "Copied directly: 雪";
    for message in [
        Message::MonitorList {
            generation: GENERATION,
            monitors: displays(),
        },
        Message::ClipboardData {
            generation: GENERATION,
            text: text.into(),
        },
    ] {
        host.send(WsMessage::Binary(message.encode().unwrap()))
            .await
            .unwrap();
    }
    assert!(
        matches!(deadline(rx.recv()).await, Some(Event::MonitorList(list)) if list == displays())
    );
    assert!(
        matches!(deadline(rx.recv()).await, Some(Event::ClipboardData(actual)) if actual == text)
    );
    client.select_monitor("output-雪").unwrap();
    client.send_clipboard("viewer reply").unwrap();
    assert_wire(
        &mut host,
        Message::SelectMonitor {
            generation: GENERATION,
            id: "output-雪".into(),
        },
    )
    .await;
    assert_wire(
        &mut host,
        Message::ClipboardData {
            generation: GENERATION,
            text: "viewer reply".into(),
        },
    )
    .await;
}

#[tokio::test]
async fn wss_sends_tls_before_the_websocket_upgrade() {
    use tokio::io::AsyncReadExt;

    let listener = loopback().await;
    let url = format!("wss://{}", listener.local_addr().unwrap());
    let peer = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut record = [0; 6];
        stream.read_exact(&mut record).await.unwrap();
        assert_eq!(record[0], 22, "TLS handshake record");
        assert_eq!(record[1], 3, "TLS record version major");
        assert_eq!(record[5], 1, "TLS ClientHello");
    };
    let (client, ()) =
        deadline(async { tokio::join!(SessionClient::connect_direct(&url, None, |_| {}), peer) })
            .await;
    assert!(matches!(client, Err(ClientError::WebSocket(_))));
}

#[tokio::test]
async fn onboarding_helpers_sign_in_and_create_code() {
    use skylatch_client_core::{create_pairing_code, enroll, login};
    use skylatch_proto::onboarding::{CodeEnrollment, EnrollRequest};

    let listener = loopback().await;
    let url = format!("ws://{}", listener.local_addr().unwrap());
    let state = AppState::with_auth(
        HubConfig::new(vec![]),
        None,
        vec![(ACCOUNT.into(), "example passphrase".into())],
    )
    .unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router(state)).await.unwrap() });
    let hub = Hub { url, task };
    let login = login(
        &format!("{}/ws/host", hub.url),
        ACCOUNT,
        "example passphrase",
    )
    .await
    .unwrap();
    assert_eq!(login.account, ACCOUNT);
    assert!(list_devices(&hub.url, &login.token)
        .await
        .unwrap()
        .is_empty());
    assert!(
        matches!(create_pairing_code(&hub.url, "incorrect").await, Err(ClientError::Hub { code, .. }) if code == "401")
    );
    let pair = create_pairing_code(&hub.url, &login.token).await.unwrap();
    assert_eq!(pair.expires_in_secs, 600);
    let request = EnrollRequest::Code(CodeEnrollment {
        code: pair.code,
        device_id: DEVICE.into(),
        name: "Example host".into(),
    });
    let credential = enroll(&hub.url, &request).await.unwrap();
    assert_eq!(credential.device_id, DEVICE);
    assert!(skylatch_proto::credentials::is_token(
        &credential.device_token
    ));
    assert!(
        matches!(enroll(&hub.url, &request).await, Err(ClientError::Hub { code, .. }) if code == "401")
    );
}

#[tokio::test]
async fn layout_round_trips_through_the_hub() {
    use skylatch_client_core::{
        get_layout, put_layout, DevicePlacement, Facing, FurnitureItem, FurnitureKind, RoomLayout,
        WorkspaceLayout,
    };
    let listener = loopback().await;
    let url = format!("http://{}", listener.local_addr().unwrap());
    let config = HubConfig::new(vec![(ACCOUNT.into(), HUB_TOKEN.into())]);
    let server = tokio::spawn(async move {
        axum::serve(listener, router(AppState::new(config)))
            .await
            .unwrap();
    });
    assert_eq!(
        get_layout(&url, HUB_TOKEN).await.unwrap(),
        WorkspaceLayout::default()
    );
    let layout = WorkspaceLayout {
        version: 1,
        rooms: vec![RoomLayout {
            id: "r1".into(),
            name: "Lab".into(),
            furniture: vec![FurnitureItem {
                id: "f1".into(),
                kind: FurnitureKind::VmRack,
                grid_x: 3,
                grid_y: -4,
                facing: Facing::FrontRight,
            }],
        }],
        placements: vec![DevicePlacement {
            device_id: DEVICE.into(),
            custom_name: Some("Rack PC".into()),
            room_id: "r1".into(),
            furniture_id: "f1".into(),
            slot: 2,
            du: Some(0.5),
            dv: None,
        }],
    };
    assert_eq!(put_layout(&url, HUB_TOKEN, &layout).await.unwrap(), layout);
    assert_eq!(get_layout(&url, HUB_TOKEN).await.unwrap(), layout);
    let mut bad = layout.clone();
    bad.rooms.push(bad.rooms[0].clone());
    assert!(matches!(
        put_layout(&url, HUB_TOKEN, &bad).await,
        Err(ClientError::Hub { code, .. }) if code == "400"
    ));
    assert!(matches!(
        get_layout(&url, "wrong").await,
        Err(ClientError::Hub { code, .. }) if code == "401"
    ));
    server.abort();
}

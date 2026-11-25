//! HTTP and WebSocket handlers.

use crate::api;
use crate::auth::{Auth, AuthError};
use crate::config::{HubConfig, HubMode};
use crate::ratelimit::Limits;
use crate::registry::{ClientOut, HostCommand, OpenError, Registry, CLIENT_QUEUE, HOST_QUEUE};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use skylatch_proto::control::{ClientToHub, DeviceInfo, HostToHub, HubToClient, HubToHost};
use skylatch_proto::credentials::is_token;
use skylatch_proto::mesh::{MESH_INVITE_CT_LIMIT, MESH_VAULT_BODY_LIMIT};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// How long a new socket may take to send its first message.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest accepted `PUT /api/v1/layout` body.
const LAYOUT_BODY_LIMIT: usize = 256 * 1024;

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<HubConfig>,
    pub registry: Arc<Registry>,
    pub(crate) auth: Arc<Auth>,
    pub(crate) limits: Arc<Limits>,
}

impl AppState {
    pub fn new(config: HubConfig) -> Self {
        Self::with_auth(config, None, vec![]).expect("empty in-memory credentials need no I/O")
    }

    /// Loads persisted credentials and creates or updates configured password users.
    pub fn with_auth(
        config: HubConfig,
        state_file: Option<PathBuf>,
        users: Vec<(String, String)>,
    ) -> std::io::Result<Self> {
        let auth = Auth::new(state_file, users, config.max_devices_per_account)?;
        Ok(Self {
            config: Arc::new(config),
            registry: Arc::new(Registry::new()),
            auth: Arc::new(auth),
            limits: Arc::default(),
        })
    }
}

/// Builds the hub router.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(api::console))
        .route("/api/v1/info", get(api::info))
        .route("/api/v1/login", post(api::login))
        .route("/api/v1/signup", post(api::signup))
        .route(
            "/api/v1/pairing-codes",
            post(api::create_code).layer(DefaultBodyLimit::max(MESH_INVITE_CT_LIMIT * 2)),
        )
        .route("/api/v1/enroll", post(api::enroll))
        .route("/api/v1/devices/{device_id}/token", delete(api::revoke))
        .route("/api/v1/devices/{device_id}", delete(api::delete_device))
        .route("/api/v1/direct-sessions", post(api::direct_session))
        .route(
            "/api/v1/layout",
            get(api::get_layout)
                .put(api::put_layout)
                .layer(DefaultBodyLimit::max(LAYOUT_BODY_LIMIT)),
        )
        .route(
            "/api/v1/mesh-vault",
            get(api::get_mesh_vault)
                .put(api::put_mesh_vault)
                .delete(api::delete_mesh_vault)
                .layer(DefaultBodyLimit::max(MESH_VAULT_BODY_LIMIT)),
        )
        .route("/healthz", get(healthz))
        .route("/api/v1/devices", get(list_devices))
        .route("/ws/host", get(ws_host))
        .route("/ws/client", get(ws_client))
        .with_state(state)
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
    mode: &'static str,
}

async fn healthz(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        mode: state.config.mode.as_str(),
    })
}

#[derive(serde::Deserialize)]
struct ListQuery {
    #[serde(default)]
    online_only: bool,
}

#[derive(Serialize)]
struct DeviceList {
    devices: Vec<DeviceInfo>,
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

async fn list_devices(
    State(state): State<AppState>,
    Query(query): Query<ListQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(account) = bearer(&headers).and_then(|t| state.auth.account(&state.config, t)) else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    };
    let mut devices = state.auth.devices(&state.registry, &account);
    if query.online_only {
        devices.retain(|d| d.online);
    }
    Json(DeviceList { devices }).into_response()
}

fn text<T: Serialize>(value: &T) -> Message {
    Message::Text(
        serde_json::to_string(value)
            .expect("control messages serialise")
            .into(),
    )
}

async fn ws_host(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_host(state, socket))
}

async fn ws_client(State(state): State<AppState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| handle_client(state, socket))
}

/// Reads the first text message within the handshake timeout.
async fn first_text(socket: &mut WebSocket) -> Option<String> {
    timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match socket.recv().await? {
                Ok(Message::Text(t)) => return Some(t.as_str().to_string()),
                Ok(Message::Ping(_) | Message::Pong(_)) => continue,
                _ => return None,
            }
        }
    })
    .await
    .ok()
    .flatten()
}

async fn handle_host(state: AppState, mut socket: WebSocket) {
    let reject = |code: &str, message: &str| {
        text(&HubToHost::Error {
            code: code.into(),
            message: message.into(),
        })
    };
    let Some(first) = first_text(&mut socket).await else {
        return;
    };
    let Ok(HostToHub::Register { token, device }) = serde_json::from_str(&first) else {
        let _ = socket.send(reject("invalid", "expected register")).await;
        return;
    };
    let device_id = device.device_id.clone();
    let (tx, mut rx) = mpsc::channel::<HostCommand>(HOST_QUEUE);
    let (account, conn_id, cancelled) =
        match state
            .auth
            .register_host(&state.config, &state.registry, &token, device, tx)
        {
            Ok(value) => value,
            Err(AuthError::Invalid) => {
                let _ = socket
                    .send(reject("invalid", "device_id must not be empty"))
                    .await;
                return;
            }
            Err(AuthError::DeviceLimit) => {
                let _ = socket
                    .send(reject("device_limit", "account device limit reached"))
                    .await;
                return;
            }
            Err(_) => {
                let _ = socket.send(reject("unauthorized", "bad token")).await;
                return;
            }
        };
    state
        .auth
        .remember_online(&state.registry, &account, &device_id);
    tracing::info!(%account, %device_id, "host registered");
    let pump = async {
        let interval = state.config.heartbeat_interval;
        let registered = text(&HubToHost::Registered {
            heartbeat_interval_secs: interval.as_secs().max(1),
        });
        if socket.send(registered).await.is_err() {
            return;
        }
        let idle_limit = interval * 3;
        loop {
            tokio::select! {
                incoming = timeout(idle_limit, socket.recv()) => {
                    let msg = match incoming {
                        Err(_) => { tracing::info!(%device_id, "host heartbeat timeout"); break; }
                        Ok(None | Some(Err(_))) => break,
                        Ok(Some(Ok(m))) => m,
                    };
                    state.registry.touch(&account, &device_id, conn_id);
                    match msg {
                        Message::Text(t) => match serde_json::from_str::<HostToHub>(t.as_str()) {
                            Ok(HostToHub::Heartbeat { seq }) => {
                                if socket.send(text(&HubToHost::HeartbeatAck { seq })).await.is_err() { break; }
                            }
                            Ok(HostToHub::DirectEndpoints { endpoints }) => {
                                state.registry.set_endpoints(&account, &device_id, conn_id, endpoints);
                                state.auth.remember_online(&state.registry, &account, &device_id);
                            }
                            Ok(HostToHub::Monitors { monitor_list, .. }) => {
                                state.registry.set_monitors(&account, &device_id, conn_id, monitor_list);
                                state.auth.remember_online(&state.registry, &account, &device_id);
                            }
                            Ok(HostToHub::DirectAccess { key }) => {
                                if is_token(&key) {
                                    state.registry.set_direct_key(&account, &device_id, conn_id, key);
                                }
                            }
                            Ok(HostToHub::SessionReady { session_id }) => {
                                tracing::debug!(%device_id, %session_id, "session ready");
                            }
                            Ok(HostToHub::SessionClosed { session_id }) => {
                                state.registry.close_session_from_host(&account, &device_id, &session_id);
                            }
                            Ok(HostToHub::Register { .. }) | Err(_) => {
                                let _ = socket.send(reject("invalid", "unexpected message")).await;
                            }
                        },
                        Message::Binary(data) => {
                            if !state.registry.relay_from_host(&account, &device_id, conn_id, data).await {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        Message::Ping(_) | Message::Pong(_) => {}
                    }
                }
                command = rx.recv() => {
                    let Some(command) = command else { break };
                    let sent = match command {
                        HostCommand::Json(m) => socket.send(text(&m)).await,
                        HostCommand::Binary(b) => socket.send(Message::Binary(b)).await,
                    };
                    if sent.is_err() { break; }
                }
            }
        }
    };
    tokio::select! {
        biased;
        _ = cancelled => {},
        _ = pump => {},
    }
    state
        .auth
        .host_disconnected(&state.registry, &account, &device_id, conn_id);
    let _ = timeout(
        Duration::from_millis(250),
        socket.send(Message::Close(None)),
    )
    .await;
    tracing::info!(%account, %device_id, "host disconnected");
}

fn client_error(code: &str, message: &str) -> Message {
    text(&HubToClient::Error {
        code: code.into(),
        message: message.into(),
    })
}

async fn handle_client(state: AppState, mut socket: WebSocket) {
    let Some(first) = first_text(&mut socket).await else {
        return;
    };
    let account = match serde_json::from_str::<ClientToHub>(&first) {
        Ok(ClientToHub::Auth { token }) => match state.auth.account(&state.config, &token) {
            Some(account) => account,
            None => {
                let _ = socket.send(client_error("unauthorized", "bad token")).await;
                return;
            }
        },
        _ => {
            let _ = socket.send(client_error("invalid", "expected auth")).await;
            return;
        }
    };
    if socket
        .send(text(&HubToClient::AuthOk {
            account: account.clone(),
        }))
        .await
        .is_err()
    {
        return;
    }
    let (out_tx, mut out_rx) = mpsc::channel::<ClientOut>(CLIENT_QUEUE);
    // (device_id, session_id) of the active session.
    let mut current: Option<(String, String)> = None;
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let msg = match incoming {
                    None | Some(Err(_)) => break,
                    Some(Ok(m)) => m,
                };
                match msg {
                    Message::Text(t) => {
                        let reply = match serde_json::from_str::<ClientToHub>(t.as_str()) {
                            Ok(ClientToHub::ListDevices) => vec![text(&HubToClient::Devices {
                                devices: state.auth.devices(&state.registry, &account),
                            })],
                            Ok(ClientToHub::ConnectDevice { device_id }) => {
                                if let Some((dev, sid)) = current.take() {
                                    state.registry.close_session(&account, &dev, &sid, true);
                                }
                                let live = state.registry.info(&account, &device_id);
                                if state.config.mode == HubMode::SignalingOnly && live.is_some() {
                                    let mut reply = Vec::new();
                                    if let Some(info) = live.filter(|d| !d.direct_endpoints.is_empty()) {
                                        reply.push(text(&HubToClient::DirectHint { device_id, endpoints: info.direct_endpoints }));
                                    }
                                    reply.push(client_error("relay_disabled", "this hub only brokers direct connections; connect to the device directly"));
                                    reply
                                } else {
                                match state.registry.open_session(&account, &device_id, out_tx.clone()) {
                                    Ok((session_id, endpoints)) => {
                                        current = Some((device_id.clone(), session_id.clone()));
                                        vec![
                                            text(&HubToClient::DirectHint { device_id: device_id.clone(), endpoints }),
                                            text(&HubToClient::RelayStream { session_id, device_id }),
                                        ]
                                    }
                                    Err(OpenError::NotFound) if state.auth.is_known(&account, &device_id) => {
                                        vec![client_error("device_offline", "device is offline")]
                                    }
                                    Err(OpenError::NotFound) => vec![client_error("not_found", "no such device")],
                                    Err(OpenError::Busy) => vec![client_error("busy", "device already has a session")],
                                    Err(OpenError::Unavailable) => vec![client_error("unavailable", "device is not reachable")],
                                }
                                }
                            }
                            Ok(ClientToHub::Disconnect) => {
                                if let Some((dev, sid)) = current.take() {
                                    state.registry.close_session(&account, &dev, &sid, true);
                                    vec![text(&HubToClient::SessionClosed { session_id: sid, reason: "client_disconnect".into() })]
                                } else {
                                    vec![]
                                }
                            }
                            Ok(ClientToHub::Auth { .. }) | Err(_) => vec![client_error("invalid", "unexpected message")],
                        };
                        for m in reply {
                            if socket.send(m).await.is_err() { break; }
                        }
                    }
                    Message::Binary(data) => {
                        if let Some((dev, sid)) = &current {
                            if !state.registry.relay_from_client(&account, dev, sid, data).await {
                                break;
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            out = out_rx.recv() => {
                let Some(out) = out else { break };
                let sent = match out {
                    ClientOut::Json(m) => {
                        if matches!(&m, HubToClient::SessionClosed { session_id, .. }
                            if current.as_ref().is_some_and(|(_, s)| s == session_id))
                        {
                            current = None;
                        }
                        socket.send(text(&m)).await
                    }
                    ClientOut::Binary(b) => socket.send(Message::Binary(b)).await,
                };
                if sent.is_err() { break; }
            }
        }
    }
    if let Some((dev, sid)) = current {
        state.registry.close_session(&account, &dev, &sid, true);
    }
}

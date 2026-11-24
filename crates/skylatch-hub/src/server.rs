//! HTTP and WebSocket handlers.

use crate::config::HubConfig;
use crate::registry::{ClientOut, HostCommand, OpenError, Registry, CLIENT_QUEUE, HOST_QUEUE};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use skylatch_proto::control::{ClientToHub, DeviceInfo, HostToHub, HubToClient, HubToHost};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// How long a new socket may take to send its first message.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<HubConfig>,
    pub registry: Arc<Registry>,
}

impl AppState {
    pub fn new(config: HubConfig) -> Self {
        Self {
            config: Arc::new(config),
            registry: Arc::new(Registry::new()),
        }
    }
}

/// Builds the hub router.
pub fn router(state: AppState) -> Router {
    Router::new()
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
}

async fn healthz() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

#[derive(Serialize)]
struct DeviceList {
    devices: Vec<DeviceInfo>,
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

async fn list_devices(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(account) = bearer(&headers).and_then(|t| state.config.account_for_token(t)) else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    };
    Json(DeviceList {
        devices: state.registry.list(account),
    })
    .into_response()
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
    let (account, device) = match serde_json::from_str::<HostToHub>(&first) {
        Ok(HostToHub::Register { token, device }) => match state.config.account_for_token(&token) {
            Some(account) if !device.device_id.is_empty() => (account.to_string(), device),
            Some(_) => {
                let _ = socket
                    .send(reject("invalid", "device_id must not be empty"))
                    .await;
                return;
            }
            None => {
                let _ = socket.send(reject("unauthorized", "bad token")).await;
                return;
            }
        },
        _ => {
            let _ = socket.send(reject("invalid", "expected register")).await;
            return;
        }
    };
    let device_id = device.device_id.clone();
    let (tx, mut rx) = mpsc::channel::<HostCommand>(HOST_QUEUE);
    let conn_id = state.registry.register(&account, device, tx);
    tracing::info!(%account, %device_id, "host registered");
    let interval = state.config.heartbeat_interval;
    let registered = text(&HubToHost::Registered {
        heartbeat_interval_secs: interval.as_secs().max(1),
    });
    if socket.send(registered).await.is_err() {
        state.registry.unregister(&account, &device_id, conn_id);
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
                        state.registry.relay_from_host(&account, &device_id, conn_id, data);
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
    state.registry.unregister(&account, &device_id, conn_id);
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
        Ok(ClientToHub::Auth { token }) => match state.config.account_for_token(&token) {
            Some(account) => account.to_string(),
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
                                devices: state.registry.list(&account),
                            })],
                            Ok(ClientToHub::ConnectDevice { device_id }) => {
                                if let Some((dev, sid)) = current.take() {
                                    state.registry.close_session(&account, &dev, &sid, true);
                                }
                                match state.registry.open_session(&account, &device_id, out_tx.clone()) {
                                    Ok((session_id, endpoints)) => {
                                        current = Some((device_id.clone(), session_id.clone()));
                                        vec![
                                            text(&HubToClient::DirectHint { device_id: device_id.clone(), endpoints }),
                                            text(&HubToClient::RelayStream { session_id, device_id }),
                                        ]
                                    }
                                    Err(OpenError::NotFound) => vec![client_error("not_found", "no such device")],
                                    Err(OpenError::Busy) => vec![client_error("busy", "device already has a session")],
                                    Err(OpenError::Unavailable) => vec![client_error("unavailable", "device is not reachable")],
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
                            state.registry.relay_from_client(&account, dev, sid, data);
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

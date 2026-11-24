//! Opt-in NSL1 listener. The engine itself remains on loopback.

use crate::engine::{EngineEvent, EngineSource};
use axum::extract::{
    ws::{Message, WebSocket, WebSocketUpgrade},
    State,
};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use skylatch_proto::control::{DirectEndpoint, EndpointKind};
use skylatch_proto::credentials::{constant_time_eq, valid_direct_ticket};
use skylatch_proto::nsl1::{HEADER_SIZE, MAX_PAYLOAD, SUBPROTOCOL};
use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, watch, Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;

#[derive(Clone)]
pub(crate) struct Direct {
    pub engine: Arc<Mutex<EngineSource>>,
    pub viewer: Arc<Semaphore>,
    pub key: watch::Receiver<Option<String>>,
    pub device_token: Option<Arc<str>>,
}

pub(crate) fn endpoints(port: u16) -> io::Result<Vec<DirectEndpoint>> {
    let mut endpoints = Vec::new();
    for interface in if_addrs::get_if_addrs()? {
        let IpAddr::V4(ip) = interface.ip() else {
            continue;
        };
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
            continue;
        }
        let name = interface.name.to_ascii_lowercase();
        let overlay = [
            "tun",
            "utun",
            "tap",
            "wg",
            "tailscale",
            "zerotier",
            "zt",
            "easytier",
            "wireguard",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
            || (ip.octets()[0] == 100 && (64..128).contains(&ip.octets()[1]));
        let endpoint = DirectEndpoint {
            kind: if overlay {
                EndpointKind::Overlay
            } else {
                EndpointKind::Lan
            },
            host: ip.to_string(),
            port,
        };
        if !endpoints.contains(&endpoint) {
            endpoints.push(endpoint);
        }
    }
    endpoints.sort_by_key(|endpoint| (endpoint.kind != EndpointKind::Lan, endpoint.host.clone()));
    Ok(endpoints)
}

pub(crate) async fn serve(listener: TcpListener, direct: Direct) -> io::Result<()> {
    axum::serve(
        listener,
        Router::new()
            .route("/stream", get(upgrade))
            .with_state(direct),
    )
    .await
}

async fn upgrade(
    State(mut direct): State<Direct>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(key) = direct.key.borrow_and_update().clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut protocols = headers
        .get_all(header::SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|header| header.to_str().ok())
        .flat_map(|header| header.split(',').map(str::trim));
    let mut nsl1 = false;
    let mut authorized = false;
    for value in &mut protocols {
        nsl1 |= value == SUBPROTOCOL;
        if let Some(ticket) = value.strip_prefix("bearer.") {
            authorized |= direct
                .device_token
                .as_deref()
                .is_some_and(|token| constant_time_eq(token.as_bytes(), ticket.as_bytes()))
                || valid_direct_ticket(&key, ticket);
        }
    }
    if !authorized {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if !nsl1 {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Ok(viewer) = direct.viewer.clone().try_acquire_owned() else {
        return StatusCode::CONFLICT.into_response();
    };
    let limit = HEADER_SIZE + MAX_PAYLOAD;
    ws.protocols([SUBPROTOCOL])
        .max_frame_size(limit)
        .max_message_size(limit)
        .on_upgrade(move |socket| bridge(socket, direct, viewer))
}

async fn bridge(mut socket: WebSocket, mut direct: Direct, _viewer: OwnedSemaphorePermit) {
    let (events, mut incoming) = mpsc::channel(64);
    let opening = async { direct.engine.lock().await.open_link(1, events).await };
    let link = tokio::select! {
        biased;
        _ = direct.key.changed() => return,
        result = opening => match result {
            Ok(link) => link,
            Err(err) => { tracing::warn!(%err, "cannot open direct engine stream"); return; }
        },
    };
    let stream = async {
        loop {
            tokio::select! {
                wire = socket.recv() => match wire {
                    Some(Ok(Message::Binary(data))) => if !link.send(data).await { break; },
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                    _ => break,
                },
                event = incoming.recv() => match event {
                    Some(EngineEvent::Binary(_, data)) => if socket.send(Message::Binary(data)).await.is_err() { break; },
                    _ => break,
                }
            }
        }
    };
    tokio::select! {
        biased;
        _ = direct.key.changed() => {},
        _ = stream => {},
    }
    let _ = timeout(
        Duration::from_millis(250),
        socket.send(Message::Close(None)),
    )
    .await;
    link.close().await;
}

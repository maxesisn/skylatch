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
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
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

const MESH_PREFIXES: &[&str] = &[
    "tun",
    "utun",
    "tap",
    "wg",
    "tailscale",
    "zerotier",
    "zt",
    "easytier",
    "wireguard",
];

fn is_mesh_name(name: &str) -> bool {
    MESH_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
        || name
            .strip_prefix("et")
            .is_some_and(|rest| !rest.starts_with('h'))
}

fn is_virtual_bridge(name: &str) -> bool {
    ["vethernet (wsl", "vethernet (default switch"]
        .iter()
        .chain(&["wsl", "vmnet", "vmware", "virtualbox"])
        .any(|marker| name.contains(marker))
        || (name.starts_with("veth") && !name.starts_with("vethernet"))
        || ["docker", "br-", "virbr", "vboxnet"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn is_ula(ip: Ipv6Addr) -> bool {
    ip.octets()[0] & 0xfe == 0xfc
}

fn is_unusable(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.is_link_local()
        }
        IpAddr::V6(ip) => {
            let seg = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.to_ipv4_mapped().is_some()
                || seg[0] & 0xffc0 == 0xfe80
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)
        }
    }
}

/// Endpoint kind and connect priority (lower first) for an interface address; `None` drops it.
fn classify(name: &str, ip: IpAddr) -> Option<(EndpointKind, u8)> {
    let name = name.to_ascii_lowercase();
    if is_unusable(ip) || (is_virtual_bridge(&name) && !is_mesh_name(&name)) {
        return None;
    }
    let overlay = is_mesh_name(&name)
        || match ip {
            IpAddr::V4(ip) => {
                let octets = ip.octets();
                (octets[0] == 100 && (64..128).contains(&octets[1]))
                    || octets[..3] == [10, 126, 126]
            }
            IpAddr::V6(ip) => is_ula(ip),
        };
    Some(match (overlay, ip) {
        (true, _) => (EndpointKind::Overlay, 1),
        (false, IpAddr::V4(_)) => (EndpointKind::Lan, 0),
        (false, IpAddr::V6(_)) => (EndpointKind::Lan, 2),
    })
}

fn rank_endpoints(
    addresses: impl IntoIterator<Item = (String, IpAddr)>,
    port: u16,
) -> Vec<DirectEndpoint> {
    let mut ranked: Vec<(u8, DirectEndpoint)> = Vec::new();
    for (name, ip) in addresses {
        let Some((kind, rank)) = classify(&name, ip) else {
            continue;
        };
        let endpoint = DirectEndpoint {
            kind,
            host: ip.to_string(),
            port,
        };
        if !ranked.iter().any(|(_, known)| *known == endpoint) {
            ranked.push((rank, endpoint));
        }
    }
    ranked.sort_by(|a, b| (a.0, &a.1.host).cmp(&(b.0, &b.1.host)));
    ranked.into_iter().map(|(_, endpoint)| endpoint).collect()
}

pub(crate) fn endpoints(port: u16) -> io::Result<Vec<DirectEndpoint>> {
    Ok(rank_endpoints(
        if_addrs::get_if_addrs()?
            .into_iter()
            .map(|interface| (interface.name.clone(), interface.ip())),
        port,
    ))
}

/// Binds `[::]:port` accepting IPv4 too, or `0.0.0.0:port` when the host has no IPv6 stack.
pub(crate) async fn bind(port: u16) -> io::Result<TcpListener> {
    match bind_dual_stack(port) {
        Ok(listener) => Ok(listener),
        Err(_) => TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await,
    }
}

fn bind_dual_stack(port: u16) -> io::Result<TcpListener> {
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(false)?;
    #[cfg(unix)]
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(name: &str, ip: &str) -> (String, IpAddr) {
        (name.into(), ip.parse().unwrap())
    }

    fn hosts(addresses: &[(&str, &str)]) -> Vec<(EndpointKind, String)> {
        rank_endpoints(addresses.iter().map(|(n, ip)| addr(n, ip)), 17915)
            .into_iter()
            .map(|e| (e.kind, e.host))
            .collect()
    }

    #[test]
    fn drops_virtual_bridges() {
        for name in [
            "vEthernet (WSL)",
            "vEthernet (WSL (Hyper-V firewall))",
            "vEthernet (Default Switch)",
            "WSL",
            "VMware Network Adapter VMnet8",
            "VMnet1",
            "VirtualBox Host-Only Network",
            "docker0",
            "br-3f9a1c2d",
            "veth1a2b3c",
            "virbr0",
            "vboxnet0",
        ] {
            assert_eq!(hosts(&[(name, "172.27.144.1")]), [], "{name}");
        }
    }

    #[test]
    fn keeps_physical_and_external_switch_lans() {
        for name in ["en0", "eth0", "Ethernet", "Wi-Fi", "vEthernet (External)"] {
            assert_eq!(
                hosts(&[(name, "192.168.1.20")]),
                [(EndpointKind::Lan, "192.168.1.20".into())],
                "{name}"
            );
        }
    }

    #[test]
    fn keeps_mesh_overlays() {
        for (name, ip) in [
            ("easytier0", "10.126.126.3"),
            ("Ethernet 3", "10.126.126.14"),
            ("et0", "10.126.126.9"),
            ("tailscale0", "100.101.102.103"),
            ("Tailscale", "100.64.0.7"),
            ("wg0", "10.8.0.2"),
            ("ZeroTier One [abc]", "172.28.0.5"),
            ("zt3jn", "172.28.0.6"),
            ("tun0", "10.9.0.2"),
            ("utun4", "100.100.1.1"),
            ("utun5", "fd7a:115c:a1e0::1"),
            ("easytier", "fd00::5"),
        ] {
            assert_eq!(hosts(&[(name, ip)])[0].0, EndpointKind::Overlay, "{name}");
        }
    }

    #[test]
    fn drops_unusable_addresses() {
        for (name, ip) in [
            ("en0", "169.254.10.1"),
            ("utun0", "fe80::1"),
            ("utun1", "169.254.1.1"),
            ("en0", "fe80::1c2d:3e4f"),
            ("en0", "::1"),
            ("lo0", "127.0.0.1"),
            ("en0", "ff02::1"),
            ("en0", "2001:db8::10"),
            ("en0", "::ffff:192.0.2.1"),
            ("en0", "::"),
        ] {
            assert_eq!(hosts(&[(name, ip)]), [], "{name} {ip}");
        }
    }

    #[test]
    fn orders_lan_then_overlay_then_global_v6_without_duplicates() {
        let found = hosts(&[
            ("en1", "2409:8a00::1"),
            ("easytier0", "10.126.126.3"),
            ("en0", "10.50.2.86"),
            ("en0", "10.50.2.86"),
            ("docker0", "172.17.0.1"),
            ("en0", "fe80::1"),
            ("utun2", "fd00::7"),
        ]);
        assert_eq!(
            found,
            [
                (EndpointKind::Lan, "10.50.2.86".into()),
                (EndpointKind::Overlay, "10.126.126.3".into()),
                (EndpointKind::Overlay, "fd00::7".into()),
                (EndpointKind::Lan, "2409:8a00::1".into()),
            ]
        );
    }

    #[tokio::test]
    async fn listener_accepts_ipv4_and_ipv6() {
        let listener = bind(0).await.unwrap();
        let local = listener.local_addr().unwrap();
        let mut targets = vec![SocketAddr::from((Ipv4Addr::LOCALHOST, local.port()))];
        if local.is_ipv6() {
            targets.push(SocketAddr::from((Ipv6Addr::LOCALHOST, local.port())));
        }
        for target in &targets {
            let (accepted, connected) =
                tokio::join!(listener.accept(), tokio::net::TcpStream::connect(target));
            accepted.unwrap();
            connected.unwrap();
        }
    }
}

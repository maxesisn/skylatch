//! WebSocket handshakes and hub control-plane exchanges.

use crate::ClientError;
use futures_util::{SinkExt, StreamExt};
use skylatch_proto::control::{ClientToHub, DeviceInfo, DirectEndpoint, HubToClient};
use skylatch_proto::nsl1::{HEADER_SIZE, MAX_PAYLOAD, SUBPROTOCOL};
use std::{net::Ipv6Addr, sync::Arc, time::Duration};
use tokio::{net::TcpStream, task::JoinSet, time::timeout};
use tokio_tungstenite::{
    client_async_tls_with_config, connect_async_tls_with_config,
    tungstenite::{
        client::IntoClientRequest,
        handshake::client::{Request, Response},
        http::{header, HeaderMap, HeaderValue, Uri},
        protocol::WebSocketConfig,
        Message,
    },
    Connector, MaybeTlsStream, WebSocketStream,
};

pub(crate) type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(250);
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);
const UPGRADE_TIMEOUT: Duration = Duration::from_millis(350);
const FAST_PATH_TIMEOUT: Duration = Duration::from_secs(1);
const PROBE_CONCURRENCY: usize = 8;
const STREAM_PATH: &str = "/stream?take=1&clipboard=1";

fn ws_config() -> WebSocketConfig {
    let limit = HEADER_SIZE + MAX_PAYLOAD;
    WebSocketConfig::default()
        .max_message_size(Some(limit))
        .max_frame_size(Some(limit))
}

async fn connect_socket(request: Request) -> Result<(Socket, Response), ClientError> {
    let connector = if request.uri().scheme_str() == Some("wss") {
        // Select rustls and its provider without changing a native caller's process-wide defaults.
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| ClientError::Unexpected("invalid default TLS protocol configuration"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Connector::Rustls(Arc::new(config))
    } else {
        Connector::Plain
    };
    // `disable_nagle` sets TCP_NODELAY before either the TLS or WebSocket handshake.
    Ok(connect_async_tls_with_config(request, Some(ws_config()), true, Some(connector)).await?)
}

fn parse_base(url: &str, accept_http: bool) -> Result<(&'static str, Uri), ClientError> {
    let uri: Uri = url.parse().map_err(|_| ClientError::InvalidUrl)?;
    let scheme = match uri.scheme_str() {
        Some("ws") => "ws",
        Some("wss") => "wss",
        Some("http") if accept_http => "ws",
        Some("https") if accept_http => "wss",
        _ => return Err(ClientError::InvalidUrl),
    };
    let authority = uri.authority().ok_or(ClientError::InvalidUrl)?;
    let host = authority.host();
    if host.is_empty()
        || authority.as_str().contains('@')
        || url.contains('#')
        || (authority.as_str() != host && !matches!(authority.port_u16(), Some(1..)))
    {
        return Err(ClientError::InvalidUrl);
    }
    Ok((scheme, uri))
}

async fn open_hub(url: &str, token: &str) -> Result<Socket, ClientError> {
    if token.is_empty() {
        return Err(ClientError::InvalidArgument("hub token is empty"));
    }
    let (scheme, uri) = parse_base(url, true)?;
    if uri.query().is_some() {
        return Err(ClientError::InvalidUrl);
    }
    let authority = uri.authority().ok_or(ClientError::InvalidUrl)?;
    let url = format!(
        "{scheme}://{authority}{}/ws/client",
        uri.path().trim_end_matches('/')
    );
    let (mut socket, _) = connect_socket(url.into_client_request()?).await?;
    send_control(
        &mut socket,
        &ClientToHub::Auth {
            token: token.into(),
        },
    )
    .await?;
    match recv_control(&mut socket).await? {
        HubToClient::AuthOk { .. } => Ok(socket),
        _ => Err(ClientError::Unexpected("expected auth_ok")),
    }
}

pub(crate) async fn send_control(
    socket: &mut Socket,
    message: &ClientToHub,
) -> Result<(), ClientError> {
    socket
        .send(Message::Text(serde_json::to_string(message)?.into()))
        .await?;
    Ok(())
}

pub(crate) fn decode_control(text: &str) -> Result<HubToClient, ClientError> {
    match serde_json::from_str(text)? {
        HubToClient::Error { code, message } => Err(ClientError::Hub { code, message }),
        HubToClient::SessionClosed { reason, .. } => Err(ClientError::SessionClosed(reason)),
        message => Ok(message),
    }
}

async fn recv_control(socket: &mut Socket) -> Result<HubToClient, ClientError> {
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => return decode_control(&text),
            Message::Ping(_) => socket.flush().await?,
            Message::Pong(_) => (),
            Message::Close(_) => break,
            _ => return Err(ClientError::Unexpected("expected hub control message")),
        }
    }
    Err(ClientError::Unexpected("hub closed during handshake"))
}

/// Lists the devices visible to a hub account using the WebSocket control plane.
/// Accepts `ws://`, `wss://`, `http://` and `https://` hub base URLs.
pub async fn list_devices(hub_url: &str, token: &str) -> Result<Vec<DeviceInfo>, ClientError> {
    let (mut socket, devices) = timeout(HANDSHAKE_TIMEOUT, async {
        let mut socket = open_hub(hub_url, token).await?;
        send_control(&mut socket, &ClientToHub::ListDevices).await?;
        match recv_control(&mut socket).await? {
            HubToClient::Devices { devices } => Ok((socket, devices)),
            _ => Err(ClientError::Unexpected("expected devices")),
        }
    })
    .await
    .map_err(|_| ClientError::Timeout("device discovery"))??;
    close(&mut socket, false).await;
    Ok(devices)
}

pub(crate) async fn open_relay(
    hub_url: &str,
    token: &str,
    device_id: &str,
) -> Result<(Socket, Vec<DirectEndpoint>), ClientError> {
    if device_id.is_empty() {
        return Err(ClientError::InvalidArgument("device ID is empty"));
    }
    timeout(HANDSHAKE_TIMEOUT, async {
        let mut socket = open_hub(hub_url, token).await?;
        send_control(
            &mut socket,
            &ClientToHub::ConnectDevice {
                device_id: device_id.into(),
            },
        )
        .await?;
        match recv_control(&mut socket).await? {
            HubToClient::DirectHint {
                device_id: id,
                endpoints,
            } if id == device_id => match recv_control(&mut socket).await? {
                HubToClient::RelayStream { device_id: id, .. } if id == device_id => {
                    Ok((socket, endpoints))
                }
                _ => Err(ClientError::Unexpected(
                    "expected relay_stream for selected device",
                )),
            },
            HubToClient::RelayStream { device_id: id, .. } if id == device_id => {
                Ok((socket, Vec::new()))
            }
            _ => Err(ClientError::Unexpected(
                "expected direct_hint or relay_stream for selected device",
            )),
        }
    })
    .await
    .map_err(|_| ClientError::Timeout("hub handshake"))?
}

pub(crate) async fn close(socket: &mut Socket, relay: bool) {
    let _ = timeout(CLOSE_TIMEOUT, async {
        if relay {
            let _ = send_control(socket, &ClientToHub::Disconnect).await;
        }
        let _ = socket.close(None).await;
    })
    .await;
}

fn direct_request(url: &str, token: Option<&str>) -> Result<Request, ClientError> {
    let (scheme, uri) = parse_base(url, false)?;
    if !matches!(uri.path(), "/" | "/stream") {
        return Err(ClientError::InvalidUrl);
    }
    let authority = uri.authority().ok_or(ClientError::InvalidUrl)?;
    let mut request = format!("{scheme}://{authority}{STREAM_PATH}").into_client_request()?;
    let protocols = if let Some(token) = token {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ClientError::InvalidArgument(
                "direct token must have 64 hex digits",
            ));
        }
        let mut value = HeaderValue::from_str(&format!("{SUBPROTOCOL}, bearer.{token}"))
            .map_err(|_| ClientError::InvalidArgument("invalid direct token"))?;
        value.set_sensitive(true);
        value
    } else {
        HeaderValue::from_static(SUBPROTOCOL)
    };
    request
        .headers_mut()
        .insert(header::SEC_WEBSOCKET_PROTOCOL, protocols);
    Ok(request)
}

fn endpoint_request(endpoint: &DirectEndpoint) -> Option<Request> {
    let host = endpoint.host.trim_start_matches('[').trim_end_matches(']');
    let host = if let Ok(ip) = host.parse::<Ipv6Addr>() {
        format!("[{ip}]")
    } else if !endpoint.host.is_empty()
        && endpoint
            .host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        endpoint.host.clone()
    } else {
        return None;
    };
    // Hints carry only addresses. Never forward the hub account token to a direct host.
    direct_request(&format!("ws://{host}:{}", endpoint.port), None).ok()
}

pub(crate) async fn open_direct(url: &str, token: Option<&str>) -> Result<Socket, ClientError> {
    let request = direct_request(url, token)?;
    timeout(HANDSHAKE_TIMEOUT, async {
        let (socket, response) = connect_socket(request).await?;
        verify_direct_protocol(response.headers())?;
        Ok(socket)
    })
    .await
    .map_err(|_| ClientError::Timeout("direct handshake"))?
}

fn verify_direct_protocol(headers: &HeaderMap) -> Result<(), ClientError> {
    if headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .is_some_and(|v| v == SUBPROTOCOL)
    {
        Ok(())
    } else {
        Err(ClientError::Unexpected("direct host did not select nsl1"))
    }
}

pub(crate) async fn probe_direct(endpoints: &[DirectEndpoint]) -> Option<Socket> {
    timeout(FAST_PATH_TIMEOUT, async {
        let mut requests = endpoints.iter().filter_map(endpoint_request);
        let mut probes = JoinSet::new();
        for request in requests.by_ref().take(PROBE_CONCURRENCY) {
            probes.spawn(tcp_probe(request));
        }
        while let Some(probe) = probes.join_next().await {
            if let Some(request) = requests.next() {
                probes.spawn(tcp_probe(request));
            }
            let Ok(Some((request, stream))) = probe else {
                continue;
            };
            // Probe concurrently, but only send one takeover request at a time. LAN and
            // overlay addresses can point to the same listener.
            let upgrade = client_async_tls_with_config(
                request,
                stream,
                Some(ws_config()),
                Some(Connector::Plain),
            );
            if let Ok(Ok((socket, response))) = timeout(UPGRADE_TIMEOUT, upgrade).await {
                if verify_direct_protocol(response.headers()).is_ok() {
                    return Some(socket);
                }
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

async fn tcp_probe(request: Request) -> Option<(Request, TcpStream)> {
    let host = request
        .uri()
        .host()?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let port = request.uri().port_u16()?;
    let stream = timeout(PROBE_TIMEOUT, TcpStream::connect((host, port)))
        .await
        .ok()?
        .ok()?;
    stream.set_nodelay(true).ok()?;
    Some((request, stream))
}

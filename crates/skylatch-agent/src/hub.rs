//! The outbound hub connection and the session bridge.

use crate::cli::{Cli, DaemonCredentials};
use crate::direct::{self, Direct};
use crate::engine::{EngineEvent, EngineSource, LocalLink};
use crate::hardware::detect_hardware_profile;
use futures_util::{SinkExt, StreamExt};
use skylatch_proto::control::{DeviceInfo, HostToHub, HubToHost};
use skylatch_proto::credentials::random_token;
use std::{io, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch, Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::{interval_at, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
enum ConnError {
    #[error("connect failed: {0}")]
    Connect(String),
    #[error("hub rejected registration: {code}: {message}")]
    Rejected { code: String, message: String },
    #[error("registration failed: {0}")]
    Register(String),
    #[error("hub closed the connection")]
    Closed,
    #[error("hub connection error: {0}")]
    Ws(String),
    #[error("hub heartbeat timed out")]
    HeartbeatTimeout,
}

struct Active {
    session_id: String,
    link: LocalLink,
    viewer: OwnedSemaphorePermit,
}

/// Runs until the process is stopped, reconnecting to the hub with capped exponential backoff.
pub async fn run(mut cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let credentials = cli.resolve()?;
    let direct_enabled = cli.enable_direct || cli.direct_port != 0;
    if direct_enabled
        && cli.engine_url.as_ref().is_some_and(|url| match url.host() {
            Some(url::Host::Ipv4(ip)) => !ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => !ip.is_loopback(),
            Some(url::Host::Domain(host)) => host != "localhost",
            None => true,
        })
    {
        return Err("the direct listener requires a loopback engine URL".into());
    }
    let listener = if direct_enabled {
        Some(direct::bind(cli.direct_port).await?)
    } else {
        None
    };

    // Probes can involve native display services or a short-lived child process.
    let hardware = tokio::task::spawn_blocking(detect_hardware_profile)
        .await
        .unwrap_or_default();
    let mut endpoints = cli.direct_endpoints.clone();
    if let Some(listener) = &listener {
        let port = listener.local_addr()?.port();
        match direct::endpoints(port) {
            Ok(detected) => {
                for endpoint in detected {
                    if !endpoints.contains(&endpoint) {
                        endpoints.push(endpoint);
                    }
                }
            }
            Err(err) => tracing::warn!(%err, "could not detect direct interfaces"),
        }
        tracing::info!(port, "direct listener enabled");
    }
    let device = DeviceInfo {
        device_id: cli.resolved_device_id(),
        name: cli.resolved_name(),
        os: std::env::consts::OS.to_string(),
        host_version: env!("CARGO_PKG_VERSION").to_string(),
        monitors: hardware.monitor_list.len().max(1) as u32,
        monitor_list: hardware.monitor_list,
        form_factor: cli.form_factor.resolve(hardware.form_factor),
        room: cli.room.clone(),
        slot: cli.slot,
        hardware_model: cli.model.clone().or(hardware.hardware_model),
        direct_endpoints: endpoints,
        online: true,
        last_seen_unix: None,
    };
    let engine = Arc::new(Mutex::new(match (&cli.engine_bin, &cli.engine_url) {
        (Some(bin), _) => EngineSource::spawn(bin.clone(), cli.engine_port, cli.engine_args()),
        (None, Some(url)) => EngineSource::attach(url.clone()),
        (None, None) => {
            let bin = format!("skylatch-host-engine{}", std::env::consts::EXE_SUFFIX);
            let sibling = std::env::current_exe()?.with_file_name(&bin);
            let bin = if sibling.is_file() {
                sibling
            } else {
                bin.into()
            };
            EngineSource::spawn(bin, None, vec![])
        }
    }));
    let viewer = Arc::new(Semaphore::new(1));
    if let Some(listener) = listener {
        let (key, watch) = watch::channel(None);
        let direct = Direct {
            engine: engine.clone(),
            viewer: viewer.clone(),
            key: watch,
            device_token: credentials
                .device_token
                .then(|| Arc::from(credentials.token.as_str())),
        };
        tokio::select! {
            _ = reconnect(&credentials, &device, &engine, &viewer, Some(&key)) => {},
            result = direct::serve(listener, direct) => result?,
        }
    } else {
        reconnect(&credentials, &device, &engine, &viewer, None).await;
    }
    Ok(())
}

async fn reconnect(
    credentials: &DaemonCredentials,
    device: &DeviceInfo,
    engine: &Mutex<EngineSource>,
    viewer: &Arc<Semaphore>,
    key: Option<&watch::Sender<Option<String>>>,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let mut registered = false;
        let result = connection(credentials, device, engine, viewer, key, &mut registered).await;
        if let Some(key) = key {
            key.send_replace(None);
        }
        match result {
            Ok(()) => tracing::info!("hub connection ended"),
            Err(err) => tracing::warn!("{err}"),
        }
        if registered {
            backoff = BACKOFF_MIN;
        }
        tracing::info!(retry_in = ?backoff, "reconnecting to hub");
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn json(msg: &HostToHub) -> Message {
    Message::Text(serde_json::to_string(msg).expect("serialisable").into())
}

async fn connection(
    credentials: &DaemonCredentials,
    device: &DeviceInfo,
    engine: &Mutex<EngineSource>,
    viewer: &Arc<Semaphore>,
    key: Option<&watch::Sender<Option<String>>>,
    registered: &mut bool,
) -> Result<(), ConnError> {
    tracing::info!(hub = %credentials.hub, "connecting to hub");
    let (ws, _) =
        tokio_tungstenite::connect_async_with_config(credentials.hub.as_str(), None, true)
            .await
            .map_err(|e| ConnError::Connect(e.to_string()))?;
    let (mut sink, mut stream) = ws.split();

    sink.send(json(&HostToHub::Register {
        token: credentials.token.clone(),
        device: device.clone(),
    }))
    .await
    .map_err(|e| ConnError::Register(e.to_string()))?;

    let heartbeat_secs = timeout(REGISTER_TIMEOUT, async {
        loop {
            match stream.next().await {
                Some(Ok(Message::Text(t))) => {
                    return match serde_json::from_str::<HubToHost>(t.as_str()) {
                        Ok(HubToHost::Registered {
                            heartbeat_interval_secs,
                        }) => Ok(heartbeat_interval_secs.max(1)),
                        Ok(HubToHost::Error { code, message }) => {
                            Err(ConnError::Rejected { code, message })
                        }
                        _ => Err(ConnError::Register("unexpected reply".into())),
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                Some(Ok(_)) | None => return Err(ConnError::Closed),
                Some(Err(e)) => return Err(ConnError::Register(e.to_string())),
            }
        }
    })
    .await
    .map_err(|_| ConnError::Register("timed out".into()))??;
    if let Some(online) = key {
        let key = random_token().map_err(|err: io::Error| ConnError::Register(err.to_string()))?;
        online.send_replace(Some(key.clone()));
        sink.send(json(&HostToHub::DirectAccess { key }))
            .await
            .map_err(|e| ConnError::Register(e.to_string()))?;
    }
    *registered = true;
    tracing::info!(device_id = %device.device_id, heartbeat_secs, "registered with hub");

    let period = Duration::from_secs(heartbeat_secs);
    let mut ticker = interval_at(Instant::now() + period, period);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seq = 0u64;
    let mut last_rx = Instant::now();
    let (events_tx, mut events_rx) = mpsc::channel::<EngineEvent>(64);
    let mut generation = 0u64;
    let mut active: Option<Active> = None;

    let result: Result<(), ConnError> = async {
        loop {
            tokio::select! {
                incoming = stream.next() => {
                    let msg = match incoming {
                        None => return Err(ConnError::Closed),
                        Some(Err(e)) => return Err(ConnError::Ws(e.to_string())),
                        Some(Ok(m)) => m,
                    };
                    last_rx = Instant::now();
                    match msg {
                        Message::Binary(data) => {
                            if let Some(a) = &active {
                                if !a.link.send(data).await {
                                    tracing::debug!("engine link gone while forwarding");
                                }
                            }
                        }
                        Message::Text(t) => {
                            let Ok(cmd) = serde_json::from_str::<HubToHost>(t.as_str()) else {
                                tracing::debug!("ignoring unknown hub message");
                                continue;
                            };
                            match cmd {
                                HubToHost::OpenSession { session_id } => {
                                    if let Some(old) = active.take() {
                                        old.link.close().await;
                                    }
                                    let Ok(viewer) = viewer.clone().try_acquire_owned() else {
                                        sink.send(json(&HostToHub::SessionClosed { session_id }))
                                            .await
                                            .map_err(|e| ConnError::Ws(e.to_string()))?;
                                        continue;
                                    };
                                    generation += 1;
                                    match engine.lock().await.open_link(generation, events_tx.clone()).await {
                                        Ok(link) => {
                                            tracing::info!(%session_id, "session open");
                                            active = Some(Active { session_id: session_id.clone(), link, viewer });
                                            sink.send(json(&HostToHub::SessionReady { session_id }))
                                                .await
                                                .map_err(|e| ConnError::Ws(e.to_string()))?;
                                        }
                                        Err(err) => {
                                            tracing::error!(%session_id, "cannot open session: {err}");
                                            sink.send(json(&HostToHub::SessionClosed { session_id }))
                                                .await
                                                .map_err(|e| ConnError::Ws(e.to_string()))?;
                                        }
                                    }
                                }
                                HubToHost::RequestKeyframe { session_id } => {
                                    // A fresh `/stream` connection makes the engine start with
                                    // SPS/PPS and an IDR frame; the hub session stays open.
                                    if active.as_ref().is_some_and(|a| a.session_id == session_id) {
                                        let old = active.take().expect("checked above");
                                        old.link.close().await;
                                        generation += 1;
                                        match engine.lock().await.open_link(generation, events_tx.clone()).await {
                                            Ok(link) => {
                                                tracing::debug!(%session_id, "keyframe requested");
                                                active = Some(Active { session_id, link, viewer: old.viewer });
                                            }
                                            Err(err) => {
                                                tracing::error!(%session_id, "keyframe restart failed: {err}");
                                                sink.send(json(&HostToHub::SessionClosed { session_id }))
                                                    .await
                                                    .map_err(|e| ConnError::Ws(e.to_string()))?;
                                            }
                                        }
                                    }
                                }
                                HubToHost::CloseSession { session_id } => {
                                    if active.as_ref().is_some_and(|a| a.session_id == session_id) {
                                        let old = active.take().expect("checked above");
                                        old.link.close().await;
                                        tracing::info!(%session_id, "session closed by hub");
                                    }
                                }
                                HubToHost::Error { code, message } => {
                                    tracing::warn!(%code, %message, "hub error");
                                }
                                HubToHost::Registered { .. } | HubToHost::HeartbeatAck { .. } => {}
                            }
                        }
                        Message::Close(_) => return Err(ConnError::Closed),
                        _ => {}
                    }
                }
                event = events_rx.recv() => {
                    match event {
                        Some(EngineEvent::Binary(gen, data)) => {
                            if active.as_ref().is_some_and(|a| a.link.generation == gen) {
                                sink.send(Message::Binary(data))
                                    .await
                                    .map_err(|e| ConnError::Ws(e.to_string()))?;
                            }
                        }
                        Some(EngineEvent::Closed(gen)) => {
                            if active.as_ref().is_some_and(|a| a.link.generation == gen) {
                                let old = active.take().expect("checked above");
                                tracing::warn!(session_id = %old.session_id, "engine closed the stream");
                                old.link.close().await;
                                sink.send(json(&HostToHub::SessionClosed { session_id: old.session_id }))
                                    .await
                                    .map_err(|e| ConnError::Ws(e.to_string()))?;
                            }
                        }
                        None => unreachable!("events_tx is held by this loop"),
                    }
                }
                _ = ticker.tick() => {
                    if last_rx.elapsed() > period * 3 {
                        return Err(ConnError::HeartbeatTimeout);
                    }
                    seq += 1;
                    sink.send(json(&HostToHub::Heartbeat { seq }))
                        .await
                        .map_err(|e| ConnError::Ws(e.to_string()))?;
                }
            }
        }
    }
    .await;

    if let Some(key) = key {
        key.send_replace(None);
    }
    if let Some(a) = active.take() {
        a.link.close().await;
    }
    let _ = sink.close().await;
    result
}

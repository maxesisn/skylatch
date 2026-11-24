//! The outbound hub connection and the session bridge.

use crate::cli::Cli;
use crate::engine::{EngineEvent, EngineSource, LocalLink};
use futures_util::{SinkExt, StreamExt};
use skylatch_proto::control::{DeviceInfo, HostToHub, HubToHost};
use std::time::Duration;
use tokio::sync::mpsc;
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
}

/// Runs until the process is stopped, reconnecting to the hub with capped exponential backoff.
pub async fn run(cli: Cli) {
    let device = DeviceInfo {
        device_id: cli.resolved_device_id(),
        name: cli.resolved_name(),
        os: std::env::consts::OS.to_string(),
        host_version: env!("CARGO_PKG_VERSION").to_string(),
        monitors: 1,
        direct_endpoints: cli.direct_endpoints.clone(),
    };
    let mut engine = match (&cli.engine_bin, &cli.engine_url) {
        (Some(bin), _) => EngineSource::spawn(bin.clone(), cli.engine_port, cli.engine_args()),
        (None, Some(url)) => EngineSource::attach(url.clone()),
        (None, None) => unreachable!("clap requires one engine source"),
    };
    let mut backoff = BACKOFF_MIN;
    loop {
        let mut registered = false;
        match connection(&cli, &device, &mut engine, &mut registered).await {
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
    cli: &Cli,
    device: &DeviceInfo,
    engine: &mut EngineSource,
    registered: &mut bool,
) -> Result<(), ConnError> {
    tracing::info!(hub = %cli.hub, "connecting to hub");
    let (ws, _) = tokio_tungstenite::connect_async_with_config(cli.hub.as_str(), None, true)
        .await
        .map_err(|e| ConnError::Connect(e.to_string()))?;
    let (mut sink, mut stream) = ws.split();

    sink.send(json(&HostToHub::Register {
        token: cli.token.clone(),
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
                                    generation += 1;
                                    match engine.open_link(generation, events_tx.clone()).await {
                                        Ok(link) => {
                                            tracing::info!(%session_id, "session open");
                                            active = Some(Active { session_id: session_id.clone(), link });
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
                                        match engine.open_link(generation, events_tx.clone()).await {
                                            Ok(link) => {
                                                tracing::debug!(%session_id, "keyframe requested");
                                                active = Some(Active { session_id, link });
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

    if let Some(a) = active.take() {
        a.link.close().await;
    }
    let _ = sink.close().await;
    result
}

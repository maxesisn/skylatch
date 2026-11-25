//! The outbound hub connection and the session bridge.

use crate::cli::{AgentArgs, DaemonCredentials};
use crate::direct::{self, Direct};
use crate::engine::{EngineEvent, EngineSource, LocalLink};
use crate::hardware::{detect_hardware_profile, detect_monitors};
use futures_util::{SinkExt, StreamExt};
use skylatch_proto::control::{DeviceInfo, DirectEndpoint, HostToHub, HubToHost};
use skylatch_proto::credentials::random_token;
use skylatch_proto::nsl1::MonitorInfo;
use std::{io, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch, Mutex, OwnedSemaphorePermit, Semaphore};
use tokio::time::{interval_at, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);
const TOPOLOGY_POLL: Duration = Duration::from_secs(5);

/// Display geometry and direct endpoints as currently observed on the host.
struct Observed {
    monitor_list: Vec<MonitorInfo>,
    /// `None` when interface enumeration failed; the last known endpoints stay advertised.
    direct_endpoints: Option<Vec<DirectEndpoint>>,
}

type Probe = Arc<dyn Fn() -> Observed + Send + Sync>;

impl Observed {
    /// Folds the observation into `device`, returning the control messages that announce changes.
    fn apply(self, device: &mut DeviceInfo) -> Vec<HostToHub> {
        let mut updates = Vec::new();
        if self.monitor_list != device.monitor_list {
            device.monitors = self.monitor_list.len().max(1) as u32;
            device.monitor_list = self.monitor_list;
            tracing::info!(monitors = device.monitors, "display topology changed");
            updates.push(HostToHub::Monitors {
                monitors: device.monitors,
                monitor_list: device.monitor_list.clone(),
            });
        }
        if let Some(endpoints) = self
            .direct_endpoints
            .filter(|e| *e != device.direct_endpoints)
        {
            device.direct_endpoints = endpoints;
            tracing::info!(
                count = device.direct_endpoints.len(),
                "direct endpoints changed"
            );
            updates.push(HostToHub::DirectEndpoints {
                endpoints: device.direct_endpoints.clone(),
            });
        }
        updates
    }
}

async fn observe(probe: &Probe) -> Option<Observed> {
    let probe = probe.clone();
    tokio::task::spawn_blocking(move || probe()).await.ok()
}

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
pub async fn run(mut cli: AgentArgs) -> Result<(), Box<dyn std::error::Error>> {
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
    let direct_port = match &listener {
        Some(listener) => {
            let port = listener.local_addr()?.port();
            tracing::info!(port, "direct listener enabled");
            Some(port)
        }
        None => None,
    };
    let static_endpoints = cli.direct_endpoints.clone();
    let probe: Probe = Arc::new(move || Observed {
        monitor_list: detect_monitors(),
        direct_endpoints: direct_port.map(|port| {
            let mut endpoints = static_endpoints.clone();
            match direct::endpoints(port) {
                Ok(detected) => {
                    for endpoint in detected {
                        if !endpoints.contains(&endpoint) {
                            endpoints.push(endpoint);
                        }
                    }
                }
                Err(err) => tracing::debug!(%err, "could not detect direct interfaces"),
            }
            endpoints
        }),
    });
    let mut device = DeviceInfo {
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
        direct_endpoints: cli.direct_endpoints.clone(),
        online: true,
        last_seen_unix: None,
    };
    if let Some(observed) = observe(&probe).await {
        observed.apply(&mut device);
    }
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
    #[cfg(windows)]
    let _follower = engine
        .lock()
        .await
        .follows_console()
        .then(|| crate::engine_win_session::Follower::spawn(engine.clone()));
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
            _ = reconnect(&credentials, device, &engine, &viewer, Some(&key), probe, TOPOLOGY_POLL) => {},
            result = direct::serve(listener, direct) => result?,
        }
    } else {
        reconnect(
            &credentials,
            device,
            &engine,
            &viewer,
            None,
            probe,
            TOPOLOGY_POLL,
        )
        .await;
    }
    Ok(())
}

async fn reconnect(
    credentials: &DaemonCredentials,
    mut device: DeviceInfo,
    engine: &Mutex<EngineSource>,
    viewer: &Arc<Semaphore>,
    key: Option<&watch::Sender<Option<String>>>,
    probe: Probe,
    poll: Duration,
) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let mut registered = false;
        if let Some(observed) = observe(&probe).await {
            observed.apply(&mut device);
        }
        let result = connection(
            credentials,
            &mut device,
            engine,
            viewer,
            key,
            &mut registered,
            &probe,
            poll,
        )
        .await;
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

#[allow(clippy::too_many_arguments)]
async fn connection(
    credentials: &DaemonCredentials,
    device: &mut DeviceInfo,
    engine: &Mutex<EngineSource>,
    viewer: &Arc<Semaphore>,
    key: Option<&watch::Sender<Option<String>>>,
    registered: &mut bool,
    probe: &Probe,
    poll: Duration,
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
    let mut topology = interval_at(Instant::now() + poll, poll);
    topology.set_missed_tick_behavior(MissedTickBehavior::Delay);
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
                                let switches = old.link.switches;
                                old.link.close().await;
                                let mut engine = engine.lock().await;
                                if engine.session_switches() != switches {
                                    // The engine moved to a new console session; resume the
                                    // same client session on it.
                                    generation += 1;
                                    match engine.open_link(generation, events_tx.clone()).await {
                                        Ok(link) => {
                                            tracing::info!(session_id = %old.session_id, "session resumed in the new console session");
                                            active = Some(Active { session_id: old.session_id, link, viewer: old.viewer });
                                            continue;
                                        }
                                        Err(err) => tracing::error!(session_id = %old.session_id, "cannot resume session: {err}"),
                                    }
                                } else {
                                    tracing::warn!(session_id = %old.session_id, "engine closed the stream");
                                }
                                drop(engine);
                                sink.send(json(&HostToHub::SessionClosed { session_id: old.session_id }))
                                    .await
                                    .map_err(|e| ConnError::Ws(e.to_string()))?;
                            }
                        }
                        None => unreachable!("events_tx is held by this loop"),
                    }
                }
                _ = topology.tick() => {
                    let Some(observed) = observe(probe).await else { continue };
                    for update in observed.apply(device) {
                        sink.send(json(&update))
                            .await
                            .map_err(|e| ConnError::Ws(e.to_string()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_proto::control::DeviceFormFactor;
    use std::sync::Mutex as StdMutex;
    use tokio::net::TcpListener;

    fn monitor(id: &str, x: i32, width: u16, height: u16) -> MonitorInfo {
        MonitorInfo {
            id: id.into(),
            name: id.into(),
            x,
            y: 0,
            width,
            height,
            primary: x == 0,
            active: true,
        }
    }

    fn endpoint(host: &str) -> DirectEndpoint {
        DirectEndpoint {
            kind: skylatch_proto::control::EndpointKind::Lan,
            host: host.into(),
            port: 7001,
        }
    }

    fn device() -> DeviceInfo {
        DeviceInfo {
            device_id: "box".into(),
            name: "Box".into(),
            os: "linux".into(),
            host_version: "1".into(),
            monitors: 1,
            monitor_list: vec![monitor("a", 0, 1920, 1080)],
            form_factor: DeviceFormFactor::Mini,
            room: None,
            slot: None,
            hardware_model: None,
            direct_endpoints: vec![endpoint("192.0.2.1")],
            online: true,
            last_seen_unix: None,
        }
    }

    fn observed(monitors: Vec<MonitorInfo>, endpoints: Option<Vec<DirectEndpoint>>) -> Observed {
        Observed {
            monitor_list: monitors,
            direct_endpoints: endpoints,
        }
    }

    #[test]
    fn unchanged_topology_sends_nothing() {
        let mut device = device();
        let same = observed(
            device.monitor_list.clone(),
            Some(device.direct_endpoints.clone()),
        );
        assert!(same.apply(&mut device).is_empty());
    }

    #[test]
    fn each_change_is_announced_once() {
        let mut device = device();
        let two = vec![monitor("a", 0, 1920, 1080), monitor("b", 1920, 1080, 1920)];
        let updates = observed(two.clone(), Some(vec![endpoint("192.0.2.2")])).apply(&mut device);
        assert_eq!(
            updates,
            [
                HostToHub::Monitors {
                    monitors: 2,
                    monitor_list: two.clone()
                },
                HostToHub::DirectEndpoints {
                    endpoints: vec![endpoint("192.0.2.2")]
                },
            ]
        );
        assert_eq!(device.monitors, 2);
        assert!(observed(two, Some(vec![endpoint("192.0.2.2")]))
            .apply(&mut device)
            .is_empty());
    }

    #[test]
    fn failed_endpoint_probe_keeps_advertised_endpoints() {
        let mut device = device();
        assert!(observed(device.monitor_list.clone(), None)
            .apply(&mut device)
            .is_empty());
        assert_eq!(device.direct_endpoints, [endpoint("192.0.2.1")]);
    }

    #[tokio::test]
    async fn connected_agent_reports_display_changes_to_the_hub() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let hub = listener.local_addr().unwrap();
        let (seen_tx, mut seen) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            while let Some(Ok(Message::Text(t))) = ws.next().await {
                match serde_json::from_str::<HostToHub>(t.as_str()).unwrap() {
                    HostToHub::Register { .. } => {
                        let ack = HubToHost::Registered {
                            heartbeat_interval_secs: 60,
                        };
                        ws.send(Message::Text(serde_json::to_string(&ack).unwrap().into()))
                            .await
                            .unwrap();
                    }
                    other => seen_tx.send(other).unwrap(),
                }
            }
        });

        let current = Arc::new(StdMutex::new(vec![monitor("a", 0, 1920, 1080)]));
        let probe: Probe = {
            let current = current.clone();
            Arc::new(move || observed(current.lock().unwrap().clone(), None))
        };
        let credentials = DaemonCredentials {
            hub: format!("ws://{hub}/ws/host").parse().unwrap(),
            token: "t".into(),
            device_token: false,
        };
        let engine = Mutex::new(EngineSource::attach("http://127.0.0.1:9".parse().unwrap()));
        let viewer = Arc::new(Semaphore::new(1));
        let mut device = device();
        let agent = async {
            let mut registered = false;
            connection(
                &credentials,
                &mut device,
                &engine,
                &viewer,
                None,
                &mut registered,
                &probe,
                Duration::from_millis(20),
            )
            .await
        };
        let driver = async {
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(seen.try_recv().is_err(), "no change, no message");
            let two = vec![monitor("a", 0, 1920, 1080), monitor("b", -1080, 1080, 1920)];
            *current.lock().unwrap() = two.clone();
            let msg = timeout(Duration::from_secs(5), seen.recv()).await.unwrap();
            assert_eq!(
                msg,
                Some(HostToHub::Monitors {
                    monitors: 2,
                    monitor_list: two
                })
            );
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert!(seen.try_recv().is_err(), "change is not repeated");
        };
        tokio::select! {
            _ = agent => panic!("connection ended"),
            _ = driver => {}
        }
    }
}

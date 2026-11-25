//! In-memory device registry and session table.

use crate::gate::{FrameGate, Verdict};
use bytes::Bytes;
use skylatch_proto::control::{DeviceInfo, DirectEndpoint, HubToClient, HubToHost};
use skylatch_proto::nsl1::MonitorInfo;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

/// Queue depth towards a client. When it is full the frame gate starts dropping.
pub const CLIENT_QUEUE: usize = 32;
/// Queue depth towards a host.
pub const HOST_QUEUE: usize = 256;
/// Bound reliable-queue waits even if a peer is no longer reading.
const RELAY_QUEUE_TIMEOUT: Duration = Duration::from_secs(5);

/// Something to write to a host socket.
#[derive(Debug)]
pub enum HostCommand {
    Json(HubToHost),
    Binary(Bytes),
}

/// Something to write to a client socket.
#[derive(Debug)]
pub enum ClientOut {
    Json(HubToClient),
    Binary(Bytes),
}

/// Why a session could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    NotFound,
    Busy,
    Unavailable,
}

struct Session {
    id: String,
    to_client: mpsc::Sender<ClientOut>,
    gate: FrameGate,
}

struct Device {
    info: DeviceInfo,
    conn_id: u64,
    tx: mpsc::Sender<HostCommand>,
    last_seen: Instant,
    session: Option<Session>,
    _cancel: Option<oneshot::Sender<()>>,
    enrolled: bool,
    direct_key: Option<String>,
}

type Key = (String, String);

/// Shared registry keyed by `(account, device_id)`.
#[derive(Default)]
pub struct Registry {
    devices: Mutex<HashMap<Key, Device>>,
    next_id: AtomicU64,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a process-unique id for connections and sessions.
    pub fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Device>> {
        self.devices.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Registers a host connection, replacing any previous one for the same device.
    pub fn register(&self, account: &str, info: DeviceInfo, tx: mpsc::Sender<HostCommand>) -> u64 {
        self.insert_host(account, info, tx, None, false)
    }

    pub(crate) fn register_host(
        &self,
        account: &str,
        info: DeviceInfo,
        tx: mpsc::Sender<HostCommand>,
        enrolled: bool,
    ) -> (u64, oneshot::Receiver<()>) {
        let (cancel, stopped) = oneshot::channel();
        (
            self.insert_host(account, info, tx, Some(cancel), enrolled),
            stopped,
        )
    }

    fn insert_host(
        &self,
        account: &str,
        mut info: DeviceInfo,
        tx: mpsc::Sender<HostCommand>,
        cancel: Option<oneshot::Sender<()>>,
        enrolled: bool,
    ) -> u64 {
        let conn_id = self.next_id();
        info.online = true;
        info.last_seen_unix = Some(unix_now());
        let key = (account.to_string(), info.device_id.clone());
        let old = self.lock().insert(
            key,
            Device {
                info,
                conn_id,
                tx,
                last_seen: Instant::now(),
                session: None,
                _cancel: cancel,
                enrolled,
                direct_key: None,
            },
        );
        if let Some(session) = old.and_then(|d| d.session) {
            notify_closed(&session, "host_replaced");
        }
        conn_id
    }

    pub(crate) fn revoke_host(&self, account: &str, device_id: &str) {
        let key = (account.to_owned(), device_id.to_owned());
        let mut map = self.lock();
        if map.get(&key).is_some_and(|d| d.enrolled) {
            if let Some(session) = map.remove(&key).and_then(|d| d.session) {
                notify_closed(&session, "token_revoked");
            }
        }
    }

    pub(crate) fn set_direct_key(&self, account: &str, device_id: &str, conn_id: u64, key: String) {
        if let Some(device) = self
            .lock()
            .get_mut(&(account.to_owned(), device_id.to_owned()))
            .filter(|d| d.conn_id == conn_id)
        {
            device.direct_key = Some(key);
        }
    }

    pub(crate) fn direct_access(
        &self,
        account: &str,
        device_id: &str,
    ) -> Option<(String, Vec<DirectEndpoint>)> {
        let map = self.lock();
        let device = map.get(&(account.to_owned(), device_id.to_owned()))?;
        if device.session.is_some() || device.info.direct_endpoints.is_empty() {
            return None;
        }
        Some((
            device.direct_key.clone()?,
            device.info.direct_endpoints.clone(),
        ))
    }

    /// Removes the device if `conn_id` is still its current connection.
    pub fn unregister(&self, account: &str, device_id: &str, conn_id: u64) -> Option<DeviceInfo> {
        let key = (account.to_string(), device_id.to_string());
        let mut map = self.lock();
        if map.get(&key).is_some_and(|d| d.conn_id == conn_id) {
            let device = map.remove(&key)?;
            if let Some(session) = &device.session {
                notify_closed(session, "host_disconnected");
            }
            return Some(device.info);
        }
        None
    }

    /// The live record of a connected device.
    pub fn info(&self, account: &str, device_id: &str) -> Option<DeviceInfo> {
        self.lock()
            .get(&(account.to_string(), device_id.to_string()))
            .map(|d| d.info.clone())
    }

    pub fn touch(&self, account: &str, device_id: &str, conn_id: u64) {
        if let Some(d) = self
            .lock()
            .get_mut(&(account.to_string(), device_id.to_string()))
        {
            if d.conn_id == conn_id {
                d.last_seen = Instant::now();
            }
        }
    }

    /// Seconds since the host was last heard from.
    pub fn idle_secs(&self, account: &str, device_id: &str) -> Option<u64> {
        self.lock()
            .get(&(account.to_string(), device_id.to_string()))
            .map(|d| d.last_seen.elapsed().as_secs())
    }

    pub fn set_endpoints(
        &self,
        account: &str,
        device_id: &str,
        conn_id: u64,
        endpoints: Vec<DirectEndpoint>,
    ) {
        if let Some(d) = self
            .lock()
            .get_mut(&(account.to_string(), device_id.to_string()))
        {
            if d.conn_id == conn_id {
                d.info.direct_endpoints = endpoints;
                d.info.last_seen_unix = Some(unix_now());
            }
        }
    }

    pub fn set_monitors(
        &self,
        account: &str,
        device_id: &str,
        conn_id: u64,
        monitor_list: Vec<MonitorInfo>,
    ) {
        if let Some(d) = self
            .lock()
            .get_mut(&(account.to_string(), device_id.to_string()))
        {
            if d.conn_id == conn_id {
                d.info.monitors = monitor_list.len().max(1) as u32;
                d.info.monitor_list = monitor_list;
                d.info.last_seen_unix = Some(unix_now());
            }
        }
    }

    /// Connected devices of `account`, sorted by id.
    pub fn list(&self, account: &str) -> Vec<DeviceInfo> {
        let mut out: Vec<DeviceInfo> = self
            .lock()
            .iter()
            .filter(|((a, _), _)| a == account)
            .map(|(_, d)| d.info.clone())
            .collect();
        out.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        out
    }

    /// Starts a session and tells the host. One session per host in v0.1.
    pub fn open_session(
        &self,
        account: &str,
        device_id: &str,
        to_client: mpsc::Sender<ClientOut>,
    ) -> Result<(String, Vec<DirectEndpoint>), OpenError> {
        let mut map = self.lock();
        let device = map
            .get_mut(&(account.to_string(), device_id.to_string()))
            .ok_or(OpenError::NotFound)?;
        if device.session.is_some() {
            return Err(OpenError::Busy);
        }
        let id = format!("s{:x}", self.next_id());
        device
            .tx
            .try_send(HostCommand::Json(HubToHost::OpenSession {
                session_id: id.clone(),
            }))
            .map_err(|_| OpenError::Unavailable)?;
        device.session = Some(Session {
            id: id.clone(),
            to_client,
            gate: FrameGate::new(),
        });
        Ok((id, device.info.direct_endpoints.clone()))
    }

    /// Ends `session_id`. Returns true if it was active. The host is told when `notify_host`.
    pub fn close_session(
        &self,
        account: &str,
        device_id: &str,
        session_id: &str,
        notify_host: bool,
    ) -> bool {
        let mut map = self.lock();
        let Some(device) = map.get_mut(&(account.to_string(), device_id.to_string())) else {
            return false;
        };
        if device.session.as_ref().map(|s| s.id.as_str()) != Some(session_id) {
            return false;
        }
        device.session = None;
        if notify_host {
            let _ = device
                .tx
                .try_send(HostCommand::Json(HubToHost::CloseSession {
                    session_id: session_id.to_string(),
                }));
        }
        true
    }

    /// Ends the session and tells the client why. Used when the host closes it.
    pub fn close_session_from_host(&self, account: &str, device_id: &str, session_id: &str) {
        let key = (account.to_string(), device_id.to_string());
        let mut map = self.lock();
        if let Some(device) = map.get_mut(&key) {
            if device.session.as_ref().map(|s| s.id.as_str()) == Some(session_id) {
                if let Some(session) = device.session.take() {
                    notify_closed(&session, "host_closed");
                }
            }
        }
    }

    /// Relays a binary message from the host. Only delta video is shed under pressure.
    /// Returns false if the receiver cannot accept a reliable message; end the connection.
    pub async fn relay_from_host(
        &self,
        account: &str,
        device_id: &str,
        conn_id: u64,
        data: Bytes,
    ) -> bool {
        let key = (account.to_string(), device_id.to_string());
        let (sender, session_id) = {
            let mut map = self.lock();
            let Some(device) = map.get_mut(&key).filter(|d| d.conn_id == conn_id) else {
                return true;
            };
            let Some(session) = device.session.as_mut() else {
                return true;
            };
            let congested = session.to_client.capacity() == 0;
            match session.gate.decide(&data, congested) {
                Verdict::ForwardReliable => (session.to_client.clone(), session.id.clone()),
                Verdict::Forward => {
                    let _ = session.to_client.try_send(ClientOut::Binary(data));
                    return true;
                }
                Verdict::Drop => return true,
                Verdict::DropAndRequestKeyframe => {
                    let _ = device
                        .tx
                        .try_send(HostCommand::Json(HubToHost::RequestKeyframe {
                            session_id: session.id.clone(),
                        }));
                    return true;
                }
            }
        };
        let permit = tokio::time::timeout(RELAY_QUEUE_TIMEOUT, sender.reserve()).await;
        // The target may have closed or changed sessions while waiting for room.
        let map = self.lock();
        let current = map.get(&key).and_then(|d| d.session.as_ref());
        if current.is_some_and(|s| s.id == session_id) {
            let Ok(Ok(permit)) = permit else { return false };
            permit.send(ClientOut::Binary(data));
        }
        true
    }

    /// Relays a binary message from the client to the host, without a video drop gate.
    /// Returns false if the receiver cannot accept the message; end the connection.
    pub async fn relay_from_client(
        &self,
        account: &str,
        device_id: &str,
        session_id: &str,
        data: Bytes,
    ) -> bool {
        let key = (account.to_string(), device_id.to_string());
        let sender = {
            let map = self.lock();
            let Some(device) = map.get(&key) else {
                return true;
            };
            if device.session.as_ref().map(|s| s.id.as_str()) != Some(session_id) {
                return true;
            }
            device.tx.clone()
        };
        let permit = tokio::time::timeout(RELAY_QUEUE_TIMEOUT, sender.reserve()).await;
        let map = self.lock();
        let current = map.get(&key).and_then(|d| d.session.as_ref());
        if current.is_some_and(|s| s.id == session_id) {
            let Ok(Ok(permit)) = permit else { return false };
            permit.send(HostCommand::Binary(data));
        }
        true
    }
}

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn notify_closed(session: &Session, reason: &str) {
    let _ = session
        .to_client
        .try_send(ClientOut::Json(HubToClient::SessionClosed {
            session_id: session.id.clone(),
            reason: reason.to_string(),
        }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use skylatch_proto::nsl1::Message;

    fn info(id: &str) -> DeviceInfo {
        DeviceInfo {
            device_id: id.into(),
            name: id.into(),
            os: "linux".into(),
            host_version: "0.1.0".into(),
            monitors: 1,
            direct_endpoints: vec![],
            form_factor: Default::default(),
            monitor_list: vec![],
            room: None,
            slot: None,
            hardware_model: None,
            online: true,
            last_seen_unix: None,
        }
    }

    fn frame(seq: u32, keyframe: bool) -> Bytes {
        Message::Frame {
            generation: 1,
            sequence: seq,
            keyframe,
            width: 1,
            height: 1,
            capture_us: 0,
            data: Bytes::from_static(&[0]),
        }
        .encode()
        .unwrap()
    }

    #[test]
    fn accounts_are_isolated_and_sorted() {
        let reg = Registry::new();
        let (tx, _rx) = mpsc::channel(4);
        reg.register("a", info("z"), tx.clone());
        reg.register("a", info("b"), tx.clone());
        reg.register("other", info("x"), tx);
        let ids: Vec<_> = reg.list("a").into_iter().map(|d| d.device_id).collect();
        assert_eq!(ids, ["b", "z"]);
        assert_eq!(reg.list("other").len(), 1);
        assert!(reg.list("nobody").is_empty());
    }

    #[test]
    fn stale_connection_cannot_unregister_replacement() {
        let reg = Registry::new();
        let (tx, _rx) = mpsc::channel(4);
        let old = reg.register("a", info("d"), tx.clone());
        let new = reg.register("a", info("d"), tx);
        assert_eq!(reg.list("a").len(), 1);
        assert!(reg.unregister("a", "d", old).is_none());
        assert!(reg.unregister("a", "d", new).is_some());
        assert!(reg.list("a").is_empty());
    }

    #[test]
    fn session_lifecycle_and_busy() {
        let reg = Registry::new();
        let (htx, mut hrx) = mpsc::channel(8);
        reg.register("a", info("d"), htx);
        let (ctx, _crx) = mpsc::channel(CLIENT_QUEUE);
        assert_eq!(
            reg.open_session("a", "missing", ctx.clone()).unwrap_err(),
            OpenError::NotFound
        );
        let (sid, _) = reg.open_session("a", "d", ctx.clone()).unwrap();
        assert!(matches!(
            hrx.try_recv().unwrap(),
            HostCommand::Json(HubToHost::OpenSession { .. })
        ));
        assert_eq!(
            reg.open_session("a", "d", ctx.clone()).unwrap_err(),
            OpenError::Busy
        );
        assert!(!reg.close_session("a", "d", "wrong", true));
        assert!(reg.close_session("a", "d", &sid, true));
        assert!(matches!(
            hrx.try_recv().unwrap(),
            HostCommand::Json(HubToHost::CloseSession { .. })
        ));
        assert!(reg.open_session("a", "d", ctx).is_ok());
    }

    #[tokio::test]
    async fn slow_client_loses_frames_and_host_is_asked_for_keyframe() {
        let reg = Registry::new();
        let (htx, mut hrx) = mpsc::channel(HOST_QUEUE);
        let conn = reg.register("a", info("d"), htx);
        let (ctx, mut crx) = mpsc::channel(2);
        reg.open_session("a", "d", ctx).unwrap();
        let _open = hrx.try_recv().unwrap();
        reg.relay_from_host("a", "d", conn, frame(1, true)).await;
        reg.relay_from_host("a", "d", conn, frame(2, false)).await;
        // Queue is full now.
        reg.relay_from_host("a", "d", conn, frame(3, false)).await;
        assert!(matches!(
            hrx.try_recv().unwrap(),
            HostCommand::Json(HubToHost::RequestKeyframe { .. })
        ));
        // Further deltas are dropped silently.
        reg.relay_from_host("a", "d", conn, frame(4, false)).await;
        assert!(hrx.try_recv().is_err());
        // Client drains; a keyframe flows again.
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(_)));
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(_)));
        reg.relay_from_host("a", "d", conn, frame(5, false)).await;
        assert!(crx.try_recv().is_err());
        reg.relay_from_host("a", "d", conn, frame(6, true)).await;
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(_)));
    }

    #[tokio::test]
    async fn sl1_and_keyframes_survive_a_full_video_queue() {
        let reg = Registry::new();
        let (htx, mut hrx) = mpsc::channel(HOST_QUEUE);
        let conn = reg.register("a", info("d"), htx);
        let (ctx, mut crx) = mpsc::channel(1);
        reg.open_session("a", "d", ctx).unwrap();
        let _open = hrx.try_recv().unwrap();
        let first = frame(1, true);
        assert!(reg.relay_from_host("a", "d", conn, first.clone()).await);
        assert!(reg.relay_from_host("a", "d", conn, frame(2, false)).await);
        assert!(matches!(
            hrx.try_recv().unwrap(),
            HostCommand::Json(HubToHost::RequestKeyframe { .. })
        ));

        let list = Message::MonitorList {
            generation: 1,
            monitors: vec![],
        }
        .encode()
        .unwrap();
        let clip = Message::ClipboardData {
            generation: 1,
            text: "copied: café\0雪".into(),
        }
        .encode()
        .unwrap();
        let key = frame(5, true);
        let final_delta = frame(6, false);
        for (expected, queued) in [
            (list.clone(), first),
            (clip.clone(), list),
            (key.clone(), clip),
        ] {
            let relay = reg.relay_from_host("a", "d", conn, expected);
            tokio::pin!(relay);
            assert!(
                futures_util::poll!(&mut relay).is_pending(),
                "reliable send must wait for room"
            );
            assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(data) if data == queued));
            assert!(relay.await);
        }
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(data) if data == key));
        assert!(
            reg.relay_from_host("a", "d", conn, final_delta.clone())
                .await
        );
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(data) if data == final_delta));
        assert!(crx.try_recv().is_err());
    }

    #[tokio::test]
    async fn viewer_sl1_waits_for_host_queue_without_dropping() {
        let reg = Registry::new();
        let (htx, mut hrx) = mpsc::channel(1);
        reg.register("a", info("d"), htx);
        let (ctx, _crx) = mpsc::channel(1);
        let (sid, _) = reg.open_session("a", "d", ctx).unwrap();
        let list = Message::SelectMonitor {
            generation: 1,
            id: "display".into(),
        }
        .encode()
        .unwrap();
        let clip = Message::ClipboardData {
            generation: 1,
            text: "viewer".into(),
        }
        .encode()
        .unwrap();
        let select = reg.relay_from_client("a", "d", &sid, list.clone());
        tokio::pin!(select);
        assert!(futures_util::poll!(&mut select).is_pending());
        assert!(matches!(
            hrx.try_recv().unwrap(),
            HostCommand::Json(HubToHost::OpenSession { .. })
        ));
        assert!(select.await);
        let copy = reg.relay_from_client("a", "d", &sid, clip.clone());
        tokio::pin!(copy);
        assert!(futures_util::poll!(&mut copy).is_pending());
        assert!(matches!(hrx.try_recv().unwrap(), HostCommand::Binary(data) if data == list));
        assert!(copy.await);
        assert!(matches!(hrx.try_recv().unwrap(), HostCommand::Binary(data) if data == clip));
    }

    #[test]
    fn host_disconnect_notifies_client() {
        let reg = Registry::new();
        let (htx, _hrx) = mpsc::channel(8);
        let conn = reg.register("a", info("d"), htx);
        let (ctx, mut crx) = mpsc::channel(4);
        reg.open_session("a", "d", ctx).unwrap();
        reg.unregister("a", "d", conn);
        assert!(matches!(
            crx.try_recv().unwrap(),
            ClientOut::Json(HubToClient::SessionClosed { .. })
        ));
    }

    #[tokio::test]
    async fn client_input_reaches_host_only_for_its_session() {
        let reg = Registry::new();
        let (htx, mut hrx) = mpsc::channel(8);
        reg.register("a", info("d"), htx);
        let (ctx, _crx) = mpsc::channel(4);
        let (sid, _) = reg.open_session("a", "d", ctx).unwrap();
        let _open = hrx.try_recv().unwrap();
        reg.relay_from_client("a", "d", "other", Bytes::from_static(b"x"))
            .await;
        assert!(hrx.try_recv().is_err());
        reg.relay_from_client("a", "d", &sid, Bytes::from_static(b"x"))
            .await;
        assert!(matches!(hrx.try_recv().unwrap(), HostCommand::Binary(_)));
    }
}

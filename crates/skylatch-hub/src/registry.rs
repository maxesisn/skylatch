//! In-memory device registry and session table.

use crate::gate::{FrameGate, Verdict};
use bytes::Bytes;
use skylatch_proto::control::{DeviceInfo, DirectEndpoint, HubToClient, HubToHost};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;
use tokio::sync::mpsc;

/// Queue depth towards a client. When it is full the frame gate starts dropping.
pub const CLIENT_QUEUE: usize = 32;
/// Queue depth towards a host.
pub const HOST_QUEUE: usize = 256;

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
        let conn_id = self.next_id();
        let key = (account.to_string(), info.device_id.clone());
        let old = self.lock().insert(
            key,
            Device {
                info,
                conn_id,
                tx,
                last_seen: Instant::now(),
                session: None,
            },
        );
        if let Some(session) = old.and_then(|d| d.session) {
            notify_closed(&session, "host_replaced");
        }
        conn_id
    }

    /// Removes the device if `conn_id` is still its current connection.
    pub fn unregister(&self, account: &str, device_id: &str, conn_id: u64) {
        let key = (account.to_string(), device_id.to_string());
        let mut map = self.lock();
        if map.get(&key).is_some_and(|d| d.conn_id == conn_id) {
            if let Some(session) = map.remove(&key).and_then(|d| d.session) {
                notify_closed(&session, "host_disconnected");
            }
        }
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
            }
        }
    }

    /// Devices of `account`, sorted by id.
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

    /// Relays a binary message from the host to the session's client, dropping under pressure.
    pub fn relay_from_host(&self, account: &str, device_id: &str, conn_id: u64, data: Bytes) {
        let mut map = self.lock();
        let Some(device) = map.get_mut(&(account.to_string(), device_id.to_string())) else {
            return;
        };
        if device.conn_id != conn_id {
            return;
        }
        let Some(session) = device.session.as_mut() else {
            return;
        };
        let congested = session.to_client.capacity() == 0;
        match session.gate.decide(&data, congested) {
            Verdict::Forward => {
                let _ = session.to_client.try_send(ClientOut::Binary(data));
            }
            Verdict::Drop => {}
            Verdict::DropAndRequestKeyframe => {
                let _ = device
                    .tx
                    .try_send(HostCommand::Json(HubToHost::RequestKeyframe {
                        session_id: session.id.clone(),
                    }));
            }
        }
    }

    /// Relays a binary message (input, ACK) from the client to the host.
    pub fn relay_from_client(&self, account: &str, device_id: &str, session_id: &str, data: Bytes) {
        let map = self.lock();
        if let Some(device) = map.get(&(account.to_string(), device_id.to_string())) {
            if device.session.as_ref().map(|s| s.id.as_str()) == Some(session_id) {
                let _ = device.tx.try_send(HostCommand::Binary(data));
            }
        }
    }
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
        reg.unregister("a", "d", old);
        assert_eq!(reg.list("a").len(), 1);
        reg.unregister("a", "d", new);
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

    #[test]
    fn slow_client_loses_frames_and_host_is_asked_for_keyframe() {
        let reg = Registry::new();
        let (htx, mut hrx) = mpsc::channel(HOST_QUEUE);
        let conn = reg.register("a", info("d"), htx);
        let (ctx, mut crx) = mpsc::channel(2);
        reg.open_session("a", "d", ctx).unwrap();
        let _open = hrx.try_recv().unwrap();
        reg.relay_from_host("a", "d", conn, frame(1, true));
        reg.relay_from_host("a", "d", conn, frame(2, false));
        // Queue is full now.
        reg.relay_from_host("a", "d", conn, frame(3, false));
        assert!(matches!(
            hrx.try_recv().unwrap(),
            HostCommand::Json(HubToHost::RequestKeyframe { .. })
        ));
        // Further deltas are dropped silently.
        reg.relay_from_host("a", "d", conn, frame(4, false));
        assert!(hrx.try_recv().is_err());
        // Client drains; a keyframe flows again.
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(_)));
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(_)));
        reg.relay_from_host("a", "d", conn, frame(5, false));
        assert!(crx.try_recv().is_err());
        reg.relay_from_host("a", "d", conn, frame(6, true));
        assert!(matches!(crx.try_recv().unwrap(), ClientOut::Binary(_)));
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

    #[test]
    fn client_input_reaches_host_only_for_its_session() {
        let reg = Registry::new();
        let (htx, mut hrx) = mpsc::channel(8);
        reg.register("a", info("d"), htx);
        let (ctx, _crx) = mpsc::channel(4);
        let (sid, _) = reg.open_session("a", "d", ctx).unwrap();
        let _open = hrx.try_recv().unwrap();
        reg.relay_from_client("a", "d", "other", Bytes::from_static(b"x"));
        assert!(hrx.try_recv().is_err());
        reg.relay_from_client("a", "d", &sid, Bytes::from_static(b"x"));
        assert!(matches!(hrx.try_recv().unwrap(), HostCommand::Binary(_)));
    }
}

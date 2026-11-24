//! JSON control plane carried in WebSocket text messages.
//!
//! Every message is a JSON object with a `type` discriminator in `snake_case`.
//! Binary WebSocket messages carry `NSL1` data-plane messages (see [`crate::nsl1`]).

use crate::nsl1::MonitorInfo;
use serde::{Deserialize, Serialize};

/// Control-plane protocol version advertised by the hub.
pub const CONTROL_VERSION: u32 = 1;

/// How a direct endpoint is reachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    /// Same local network.
    Lan,
    /// Overlay or VPN address.
    Overlay,
}

/// A direct-connect address the host listens on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectEndpoint {
    pub kind: EndpointKind,
    pub host: String,
    pub port: u16,
}

/// Physical or virtual form factor advertised by a host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DeviceFormFactor {
    #[default]
    Desktop,
    Laptop,
    Mini,
    Vm,
    Server,
}

/// Public description of a registered host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub name: String,
    /// `windows`, `macos` or `linux`.
    pub os: String,
    pub host_version: String,
    /// Legacy display count; a new agent reports at least one.
    pub monitors: u32,
    #[serde(default)]
    pub direct_endpoints: Vec<DirectEndpoint>,
    #[serde(default)]
    pub form_factor: DeviceFormFactor,
    #[serde(default)]
    pub monitor_list: Vec<MonitorInfo>,
    #[serde(default)]
    pub room: Option<String>,
    #[serde(default)]
    pub slot: Option<u32>,
}

/// Messages a host sends to the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostToHub {
    /// Must be the first message on `/ws/host`.
    Register {
        token: String,
        device: DeviceInfo,
    },
    Heartbeat {
        seq: u64,
    },
    /// Replaces the advertised direct endpoints.
    DirectEndpoints {
        endpoints: Vec<DirectEndpoint>,
    },
    /// The host is ready to stream for the session.
    SessionReady {
        session_id: String,
    },
    /// The host ended the session.
    SessionClosed {
        session_id: String,
    },
}

/// Messages the hub sends to a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToHost {
    Registered {
        heartbeat_interval_secs: u64,
    },
    HeartbeatAck {
        seq: u64,
    },
    /// A client wants to view this host. Binary messages on the host socket now belong to the session.
    OpenSession {
        session_id: String,
    },
    CloseSession {
        session_id: String,
    },
    /// The hub dropped frames for a slow client; send a keyframe.
    RequestKeyframe {
        session_id: String,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Messages a client sends to the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientToHub {
    /// Must be the first message on `/ws/client`.
    Auth {
        token: String,
    },
    ListDevices,
    ConnectDevice {
        device_id: String,
    },
    /// Ends the current session without closing the socket.
    Disconnect,
}

/// Messages the hub sends to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HubToClient {
    AuthOk {
        account: String,
    },
    Devices {
        devices: Vec<DeviceInfo>,
    },
    /// Direct endpoints the client may try before using the relay.
    DirectHint {
        device_id: String,
        endpoints: Vec<DirectEndpoint>,
    },
    /// Binary messages on this socket are now relayed for the session.
    RelayStream {
        session_id: String,
        device_id: String,
    },
    SessionClosed {
        session_id: String,
        reason: String,
    },
    Error {
        code: String,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::DeserializeOwned;

    fn rt<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: T, json: &str) {
        let text = serde_json::to_string(&value).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap(),
            serde_json::from_str::<serde_json::Value>(json).unwrap()
        );
        assert_eq!(serde_json::from_str::<T>(json).unwrap(), value);
    }

    fn device() -> DeviceInfo {
        DeviceInfo {
            device_id: "d1".into(),
            name: "Studio".into(),
            os: "macos".into(),
            host_version: "0.1.0".into(),
            monitors: 1,
            form_factor: DeviceFormFactor::Mini,
            monitor_list: vec![MonitorInfo {
                id: "display-1".into(),
                name: "Example display".into(),
                x: -1440,
                y: 0,
                width: 1440,
                height: 2560,
                primary: false,
                active: true,
            }],
            room: Some("Office".into()),
            slot: Some(3),
            direct_endpoints: vec![DirectEndpoint {
                kind: EndpointKind::Lan,
                host: "192.0.2.20".into(),
                port: 7001,
            }],
        }
    }

    const DEVICE_JSON: &str = r#"{"device_id":"d1","name":"Studio","os":"macos","host_version":"0.1.0","monitors":1,"direct_endpoints":[{"kind":"lan","host":"192.0.2.20","port":7001}],"form_factor":"mini","monitor_list":[{"id":"display-1","name":"Example display","x":-1440,"y":0,"width":1440,"height":2560,"primary":false,"active":true}],"room":"Office","slot":3}"#;

    #[test]
    fn device_info_roundtrip() {
        rt(device(), DEVICE_JSON);
    }

    #[test]
    fn legacy_device_info_defaults_new_fields() {
        let d: DeviceInfo = serde_json::from_str(
            r#"{"device_id":"x","name":"n","os":"linux","host_version":"1","monitors":1}"#,
        )
        .unwrap();
        assert!(d.direct_endpoints.is_empty());
        assert_eq!(d.monitors, 1);
        assert_eq!(d.form_factor, DeviceFormFactor::Desktop);
        assert!(d.monitor_list.is_empty());
        assert_eq!(d.room, None);
        assert_eq!(d.slot, None);
    }

    #[test]
    fn endpoint_kinds() {
        rt(
            DirectEndpoint {
                kind: EndpointKind::Overlay,
                host: "node.example".into(),
                port: 1,
            },
            r#"{"kind":"overlay","host":"node.example","port":1}"#,
        );
    }

    #[test]
    fn host_to_hub_roundtrip() {
        rt(
            HostToHub::Register {
                token: "t".into(),
                device: device(),
            },
            &format!(r#"{{"type":"register","token":"t","device":{DEVICE_JSON}}}"#),
        );
        rt(
            HostToHub::Heartbeat { seq: 9 },
            r#"{"type":"heartbeat","seq":9}"#,
        );
        rt(
            HostToHub::DirectEndpoints {
                endpoints: device().direct_endpoints,
            },
            r#"{"type":"direct_endpoints","endpoints":[{"kind":"lan","host":"192.0.2.20","port":7001}]}"#,
        );
        rt(
            HostToHub::SessionReady {
                session_id: "s1".into(),
            },
            r#"{"type":"session_ready","session_id":"s1"}"#,
        );
        rt(
            HostToHub::SessionClosed {
                session_id: "s1".into(),
            },
            r#"{"type":"session_closed","session_id":"s1"}"#,
        );
    }

    #[test]
    fn hub_to_host_roundtrip() {
        rt(
            HubToHost::Registered {
                heartbeat_interval_secs: 15,
            },
            r#"{"type":"registered","heartbeat_interval_secs":15}"#,
        );
        rt(
            HubToHost::HeartbeatAck { seq: 3 },
            r#"{"type":"heartbeat_ack","seq":3}"#,
        );
        rt(
            HubToHost::OpenSession {
                session_id: "s".into(),
            },
            r#"{"type":"open_session","session_id":"s"}"#,
        );
        rt(
            HubToHost::CloseSession {
                session_id: "s".into(),
            },
            r#"{"type":"close_session","session_id":"s"}"#,
        );
        rt(
            HubToHost::RequestKeyframe {
                session_id: "s".into(),
            },
            r#"{"type":"request_keyframe","session_id":"s"}"#,
        );
        rt(
            HubToHost::Error {
                code: "unauthorized".into(),
                message: "bad token".into(),
            },
            r#"{"type":"error","code":"unauthorized","message":"bad token"}"#,
        );
    }

    #[test]
    fn client_to_hub_roundtrip() {
        rt(
            ClientToHub::Auth { token: "t".into() },
            r#"{"type":"auth","token":"t"}"#,
        );
        rt(ClientToHub::ListDevices, r#"{"type":"list_devices"}"#);
        rt(
            ClientToHub::ConnectDevice {
                device_id: "d1".into(),
            },
            r#"{"type":"connect_device","device_id":"d1"}"#,
        );
        rt(ClientToHub::Disconnect, r#"{"type":"disconnect"}"#);
    }

    #[test]
    fn hub_to_client_roundtrip() {
        rt(
            HubToClient::AuthOk {
                account: "alice".into(),
            },
            r#"{"type":"auth_ok","account":"alice"}"#,
        );
        rt(
            HubToClient::Devices {
                devices: vec![device()],
            },
            &format!(r#"{{"type":"devices","devices":[{DEVICE_JSON}]}}"#),
        );
        rt(
            HubToClient::DirectHint {
                device_id: "d1".into(),
                endpoints: vec![],
            },
            r#"{"type":"direct_hint","device_id":"d1","endpoints":[]}"#,
        );
        rt(
            HubToClient::RelayStream {
                session_id: "s".into(),
                device_id: "d1".into(),
            },
            r#"{"type":"relay_stream","session_id":"s","device_id":"d1"}"#,
        );
        rt(
            HubToClient::SessionClosed {
                session_id: "s".into(),
                reason: "host_disconnected".into(),
            },
            r#"{"type":"session_closed","session_id":"s","reason":"host_disconnected"}"#,
        );
        rt(
            HubToClient::Error {
                code: "not_found".into(),
                message: "no such device".into(),
            },
            r#"{"type":"error","code":"not_found","message":"no such device"}"#,
        );
    }

    #[test]
    fn rejects_unknown_type_and_missing_fields() {
        assert!(serde_json::from_str::<ClientToHub>(r#"{"type":"nope"}"#).is_err());
        assert!(serde_json::from_str::<ClientToHub>(r#"{"type":"auth"}"#).is_err());
        assert!(serde_json::from_str::<HostToHub>(r#"{"seq":1}"#).is_err());
        assert!(serde_json::from_str::<HostToHub>(r#"{"type":"heartbeat","seq":-1}"#).is_err());
    }
}

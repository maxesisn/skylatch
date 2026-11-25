//! Live topology from `easytier-cli -o json peer` and `easytier-cli -o json node`.
//!
//! The CLI's JSON is parsed leniently: fields may be strings or numbers, `-` and empty strings
//! mean "unknown", and unknown fields are ignored so minor EasyTier releases keep working.

use crate::process::{command, output_with_timeout};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

pub const CLI_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeshRuntimeStatus {
    pub local_hostname: String,
    pub local_ipv4: Option<String>,
    pub nat_type: Option<String>,
    pub version: Option<String>,
    /// Every node in the network, this one included (`route: Local`).
    pub peers: Vec<MeshPeerStatus>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeshPeerStatus {
    pub id: Option<String>,
    pub hostname: String,
    pub ipv4: Option<String>,
    pub route: MeshRouteKind,
    pub latency_ms: Option<f64>,
    pub loss_rate: Option<String>,
    pub tunnel_proto: Option<String>,
    pub nat_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
pub enum MeshRouteKind {
    Local,
    P2p,
    /// Reached through other nodes; the number is EasyTier's route cost.
    Relay(u8),
    Other(String),
}

impl MeshRouteKind {
    fn parse(cost: &str) -> Self {
        let lower = cost.trim().to_ascii_lowercase();
        if lower == "local" {
            return Self::Local;
        }
        if lower == "p2p" {
            return Self::P2p;
        }
        lower
            .strip_prefix("relay(")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|n| n.trim().parse().ok())
            .map_or_else(|| Self::Other(cost.trim().to_string()), Self::Relay)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("cannot run easytier-cli: {0}")]
    Spawn(std::io::Error),
    #[error("easytier-cli did not answer within {}s", CLI_TIMEOUT.as_secs())]
    Timeout,
    #[error("easytier-cli failed: {0}")]
    Failed(String),
    #[error("unexpected easytier-cli output: {0}")]
    Parse(String),
}

/// A field as text; `None` for missing, empty, `-` or non-scalar values.
fn text(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    (!text.is_empty() && text != "-").then_some(text)
}

/// Names of EasyTier's `common.NatType` protobuf enum.
fn nat_type_name(code: i64) -> String {
    match code {
        0 => "Unknown",
        1 => "OpenInternet",
        2 => "NoPAT",
        3 => "FullCone",
        4 => "Restricted",
        5 => "PortRestricted",
        6 => "Symmetric",
        7 => "SymUdpFirewall",
        8 => "SymmetricEasyInc",
        9 => "SymmetricEasyDec",
        other => return format!("NatType({other})"),
    }
    .to_string()
}

fn strip_prefix_len(cidr: String) -> String {
    match cidr.split_once('/') {
        Some((addr, _)) => addr.to_string(),
        None => cidr,
    }
}

fn parse_peer(peer: &Value) -> MeshPeerStatus {
    let field = |name| text(peer.get(name));
    MeshPeerStatus {
        id: field("id").or_else(|| field("peer_id")),
        hostname: field("hostname").unwrap_or_default(),
        ipv4: field("ipv4").or_else(|| field("cidr").map(strip_prefix_len)),
        route: field("cost")
            .map(|cost| MeshRouteKind::parse(&cost))
            .unwrap_or_else(|| MeshRouteKind::Other(String::new())),
        latency_ms: field("lat_ms").and_then(|ms| ms.parse().ok()),
        loss_rate: field("loss_rate"),
        tunnel_proto: field("tunnel_proto"),
        nat_type: field("nat_type"),
    }
}

/// Builds the status from `peer` JSON and, if available, `node` JSON.
pub fn parse_status(
    peer_json: &str,
    node_json: Option<&str>,
) -> Result<MeshRuntimeStatus, StatusError> {
    let raw: Value =
        serde_json::from_str(peer_json).map_err(|e| StatusError::Parse(e.to_string()))?;
    let raw = raw
        .as_array()
        .ok_or_else(|| StatusError::Parse("peer list is not a JSON array".into()))?;
    let peers: Vec<MeshPeerStatus> = raw.iter().map(parse_peer).collect();
    let local_version = peers
        .iter()
        .position(|p| p.route == MeshRouteKind::Local)
        .and_then(|i| text(raw[i].get("version")));
    let node: Value = match node_json {
        Some(json) => serde_json::from_str(json).map_err(|e| StatusError::Parse(e.to_string()))?,
        None => Value::Null,
    };
    let local = peers.iter().find(|p| p.route == MeshRouteKind::Local);
    let stun_nat = node
        .get("stun_info")
        .and_then(|stun| stun.get("udp_nat_type"))
        .and_then(Value::as_i64)
        .map(nat_type_name);
    Ok(MeshRuntimeStatus {
        local_hostname: text(node.get("hostname"))
            .or_else(|| local.map(|p| p.hostname.clone()))
            .unwrap_or_default(),
        local_ipv4: text(node.get("ipv4_addr"))
            .map(strip_prefix_len)
            .or_else(|| local.and_then(|p| p.ipv4.clone())),
        nat_type: local.and_then(|p| p.nat_type.clone()).or(stun_nat),
        version: text(node.get("version")).or(local_version),
        peers,
    })
}

fn run_cli(
    cli: &Path,
    rpc_portal: Option<&str>,
    instance_name: Option<&str>,
    subcommand: &str,
) -> Result<String, StatusError> {
    let mut cmd = command(cli);
    if let Some(portal) = rpc_portal {
        cmd.args(["-p", portal]);
    }
    if let Some(name) = instance_name {
        cmd.args(["-n", name]);
    }
    cmd.args(["-o", "json", subcommand]);
    let output = output_with_timeout(cmd, CLI_TIMEOUT).map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            StatusError::Timeout
        } else {
            StatusError::Spawn(e)
        }
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(StatusError::Failed(
            stderr
                .lines()
                .last()
                .unwrap_or("non-zero exit")
                .trim()
                .to_string(),
        ));
    }
    String::from_utf8(output.stdout).map_err(|_| StatusError::Parse("output is not UTF-8".into()))
}

/// Queries a running `easytier-core` through `easytier-cli`. Read-only.
pub fn query_status(
    cli: &Path,
    rpc_portal: Option<&str>,
    instance_name: Option<&str>,
) -> Result<MeshRuntimeStatus, StatusError> {
    let peers = run_cli(cli, rpc_portal, instance_name, "peer")?;
    let node = run_cli(cli, rpc_portal, instance_name, "node").ok();
    parse_status(&peers, node.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEERS: &str = r#"[{"cidr":"10.126.126.3/24","ipv4":"10.126.126.3","hostname":"maxclaw-mac","cost":"Local","lat_ms":"-","loss_rate":"-","rx_bytes":"-","tx_bytes":"-","tunnel_proto":"-","nat_type":"PortRestricted","id":"4024064805","version":"2.6.4-8428a89d"}, {"cidr":"10.126.126.10/24","ipv4":"10.126.126.10","hostname":"pve","cost":"relay(2)","lat_ms":"6.00","loss_rate":"0.0%","rx_bytes":"0 B","tx_bytes":"0 B","tunnel_proto":"","nat_type":"PortRestricted","id":"123","version":"2.6.4-8428a89d"}]"#;
    const NODE: &str = r#"{"peer_id":4024064805,"ipv4_addr":"10.126.126.3/24","hostname":"maxclaw-mac","version":"2.6.4-8428a89d","stun_info":{"udp_nat_type":5,"tcp_nat_type":6,"public_ip":["114.246.252.53"]},"config":"..."}"#;

    #[test]
    fn parses_easytier_2_6_4_samples() {
        let status = parse_status(PEERS, Some(NODE)).unwrap();
        assert_eq!(status.local_hostname, "maxclaw-mac");
        assert_eq!(status.local_ipv4.as_deref(), Some("10.126.126.3"));
        assert_eq!(status.nat_type.as_deref(), Some("PortRestricted"));
        assert_eq!(status.version.as_deref(), Some("2.6.4-8428a89d"));
        assert_eq!(status.peers.len(), 2);
        let local = &status.peers[0];
        assert_eq!(local.route, MeshRouteKind::Local);
        assert_eq!(local.latency_ms, None);
        assert_eq!(local.loss_rate, None);
        assert_eq!(local.tunnel_proto, None);
        assert_eq!(
            status.peers[1],
            MeshPeerStatus {
                id: Some("123".into()),
                hostname: "pve".into(),
                ipv4: Some("10.126.126.10".into()),
                route: MeshRouteKind::Relay(2),
                latency_ms: Some(6.0),
                loss_rate: Some("0.0%".into()),
                tunnel_proto: None,
                nat_type: Some("PortRestricted".into()),
            }
        );
    }

    #[test]
    fn tolerates_missing_and_extra_fields() {
        let peers = r#"[{"hostname":"relay","cost":"p2p","lat_ms":5.5,"ipv4":"","tunnel_proto":"udp,tcp","extra":{"x":1}},
            {"hostname":"far","cost":"relay(3)"},{"cost":"Tunnel"}]"#;
        let status = parse_status(peers, None).unwrap();
        assert_eq!(status.local_hostname, "");
        assert_eq!(status.local_ipv4, None);
        assert_eq!(status.peers[0].route, MeshRouteKind::P2p);
        assert_eq!(status.peers[0].latency_ms, Some(5.5));
        assert_eq!(status.peers[0].ipv4, None);
        assert_eq!(status.peers[0].tunnel_proto.as_deref(), Some("udp,tcp"));
        assert_eq!(status.peers[1].route, MeshRouteKind::Relay(3));
        assert_eq!(status.peers[2].route, MeshRouteKind::Other("Tunnel".into()));

        let node = r#"{"ipv4_addr":"","hostname":"lab","stun_info":{"udp_nat_type":6}}"#;
        let status = parse_status("[]", Some(node)).unwrap();
        assert_eq!(status.local_hostname, "lab");
        assert_eq!(status.local_ipv4, None);
        assert_eq!(status.nat_type.as_deref(), Some("Symmetric"));
        assert!(parse_status("{}", None).is_err());
    }
}

//! Hub discovery and end-to-end encrypted mesh configuration types.
//!
//! The hub stores only [`MeshVaultEnvelope`] ciphertext. Clients decrypt it into a [`MeshVault`]
//! holding any number of isolated [`MeshProfile`] networks.

use crate::credentials::{hex, random_bytes};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::Ipv4Addr;

/// Response body of `GET /api/v1/info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubInfo {
    pub version: String,
    /// `relay` or `signaling-only`.
    pub mode: String,
    /// EasyTier peers operated with this hub. Profiles with `use_hub_peer` follow this list.
    pub easytier_peers: Vec<String>,
    pub allow_signup: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_devices_per_account: Option<u32>,
}

pub const VAULT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_SUBNET: &str = "10.126.1.0/24";
pub const DEFAULT_LISTEN_PORT: u16 = 11010;
pub const DEFAULT_PROFILE_ID: &str = "default";
pub const MAX_PROFILES: usize = 32;
const MAX_CUSTOM_PEERS: usize = 32;

/// One isolated EasyTier network.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshProfile {
    pub id: String,
    pub name: String,
    /// Private IPv4 `/24`, e.g. `10.126.1.0/24`.
    pub subnet: String,
    pub listen_port: u16,
    pub network_name: String,
    pub network_secret: String,
    #[serde(default)]
    pub custom_peers: Vec<String>,
    #[serde(default = "default_true")]
    pub use_hub_peer: bool,
}

fn default_true() -> bool {
    true
}

impl fmt::Debug for MeshProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeshProfile")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("subnet", &self.subnet)
            .field("listen_port", &self.listen_port)
            .field("network_name", &self.network_name)
            .field("custom_peers", &self.custom_peers)
            .field("use_hub_peer", &self.use_hub_peer)
            .finish_non_exhaustive()
    }
}

/// Decrypted per-account mesh configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshVault {
    pub schema_version: u32,
    pub profiles: Vec<MeshProfile>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeshError {
    #[error("unsupported mesh vault schema version {0}")]
    UnsupportedSchema(u32),
    #[error("the mesh vault has no profiles")]
    Empty,
    #[error("the mesh vault has more than {MAX_PROFILES} profiles")]
    TooManyProfiles,
    #[error("profile `{id}`: {reason}")]
    InvalidProfile { id: String, reason: &'static str },
    #[error("duplicate profile id `{0}`")]
    DuplicateId(String),
    #[error("profiles `{0}` and `{1}` use the same subnet")]
    SubnetOverlap(String, String),
    #[error("profiles `{0}` and `{1}` use the same listen port")]
    PortConflict(String, String),
    #[error("no free subnet or listen port left")]
    Exhausted,
    #[error("system random source failed")]
    Random,
}

fn random_hex<const N: usize>() -> Result<String, MeshError> {
    random_bytes::<N>()
        .map(|bytes| hex(&bytes))
        .map_err(|_| MeshError::Random)
}

/// Parses a private IPv4 `/24` CIDR and returns its network address.
pub fn parse_private_subnet(subnet: &str) -> Option<Ipv4Addr> {
    let (addr, prefix) = subnet.split_once('/')?;
    if prefix != "24" {
        return None;
    }
    let addr: Ipv4Addr = addr.parse().ok()?;
    (addr.is_private() && addr.octets()[3] == 0).then_some(addr)
}

/// Checks an EasyTier peer URL such as `tcp://public.example.com:11010`.
pub fn is_valid_peer_url(peer: &str) -> bool {
    peer.len() <= 256
        && peer.split_once("://").is_some_and(|(scheme, rest)| {
            !scheme.is_empty()
                && scheme.bytes().all(|b| b.is_ascii_lowercase())
                && !rest.is_empty()
                && !rest
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
        })
}

impl MeshProfile {
    pub fn validate(&self) -> Result<(), MeshError> {
        let invalid = |reason| MeshError::InvalidProfile {
            id: self.id.clone(),
            reason,
        };
        if self.id.is_empty()
            || self.id.len() > 32
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
        {
            return Err(invalid("id must be 1-32 of a-z, 0-9, '-' or '_'"));
        }
        if self.name.trim().is_empty() || self.name.chars().count() > 64 {
            return Err(invalid("name must be 1-64 characters"));
        }
        if parse_private_subnet(&self.subnet).is_none() {
            return Err(invalid("subnet must be a private IPv4 /24 network"));
        }
        if self.listen_port == 0 {
            return Err(invalid("listen port must be non-zero"));
        }
        if self.network_name.trim().is_empty() || self.network_name.len() > 128 {
            return Err(invalid("network name must be 1-128 bytes"));
        }
        if self.network_secret.len() < 16 || self.network_secret.len() > 256 {
            return Err(invalid("network secret must be 16-256 bytes"));
        }
        if self.custom_peers.len() > MAX_CUSTOM_PEERS
            || !self.custom_peers.iter().all(|p| is_valid_peer_url(p))
        {
            return Err(invalid(
                "custom peers must be at most 32 scheme://host URLs",
            ));
        }
        Ok(())
    }
}

impl MeshVault {
    /// A vault with one default network and fresh random credentials.
    pub fn new_default() -> Result<Self, MeshError> {
        Ok(Self {
            schema_version: VAULT_SCHEMA_VERSION,
            profiles: vec![MeshProfile {
                id: DEFAULT_PROFILE_ID.into(),
                name: "默认组网".into(),
                subnet: DEFAULT_SUBNET.into(),
                listen_port: DEFAULT_LISTEN_PORT,
                network_name: format!("skylatch-{}", random_hex::<8>()?),
                network_secret: random_hex::<32>()?,
                custom_peers: vec![],
                use_hub_peer: true,
            }],
        })
    }

    /// A new isolated profile on the lowest free `10.126.N.0/24` and listen port.
    pub fn next_isolated_defaults(&self) -> Result<MeshProfile, MeshError> {
        if self.profiles.len() >= MAX_PROFILES {
            return Err(MeshError::TooManyProfiles);
        }
        let used: Vec<Ipv4Addr> = self
            .profiles
            .iter()
            .filter_map(|p| parse_private_subnet(&p.subnet))
            .collect();
        let third = (1..=254u8)
            .find(|n| !used.contains(&Ipv4Addr::new(10, 126, *n, 0)))
            .ok_or(MeshError::Exhausted)?;
        let listen_port = (DEFAULT_LISTEN_PORT..=u16::MAX)
            .find(|port| self.profiles.iter().all(|p| p.listen_port != *port))
            .ok_or(MeshError::Exhausted)?;
        let id = loop {
            let id = random_hex::<4>()?;
            if self.profiles.iter().all(|p| p.id != id) {
                break id;
            }
        };
        Ok(MeshProfile {
            id,
            name: format!("隔离网络 {third}"),
            subnet: format!("10.126.{third}.0/24"),
            listen_port,
            network_name: format!("skylatch-{}", random_hex::<8>()?),
            network_secret: random_hex::<32>()?,
            custom_peers: vec![],
            use_hub_peer: true,
        })
    }

    pub fn validate(&self) -> Result<(), MeshError> {
        if self.schema_version != VAULT_SCHEMA_VERSION {
            return Err(MeshError::UnsupportedSchema(self.schema_version));
        }
        if self.profiles.is_empty() {
            return Err(MeshError::Empty);
        }
        if self.profiles.len() > MAX_PROFILES {
            return Err(MeshError::TooManyProfiles);
        }
        for (i, profile) in self.profiles.iter().enumerate() {
            profile.validate()?;
            for earlier in &self.profiles[..i] {
                if earlier.id == profile.id {
                    return Err(MeshError::DuplicateId(profile.id.clone()));
                }
                if parse_private_subnet(&earlier.subnet) == parse_private_subnet(&profile.subnet) {
                    return Err(MeshError::SubnetOverlap(
                        earlier.id.clone(),
                        profile.id.clone(),
                    ));
                }
                if earlier.listen_port == profile.listen_port {
                    return Err(MeshError::PortConflict(
                        earlier.id.clone(),
                        profile.id.clone(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn profile(&self, id: &str) -> Option<&MeshProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }
}

pub const VAULT_KDF: &str = "argon2id-v1";
pub const VAULT_SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;
pub const AEAD_TAG_LEN: usize = 16;
pub const KEY_LEN: usize = 32;
/// Largest accepted `PUT /api/v1/mesh-vault` body.
pub const MESH_VAULT_BODY_LIMIT: usize = 64 * 1024;
/// Largest accepted `mesh_invite.ct_b64`.
pub const MESH_INVITE_CT_LIMIT: usize = 16 * 1024;

/// The encrypted vault as stored by the hub.
///
/// `payload_ct_b64` is the `MeshVault` JSON sealed with a random data key (DEK);
/// `wrapped_dek_ct_b64` is that DEK sealed with an Argon2id key derived from the login password.
/// `version` and `updated_at_unix` are hub-assigned metadata, outside the authenticated data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshVaultEnvelope {
    pub version: u64,
    pub kdf: String,
    pub kdf_salt_b64: String,
    pub wrapped_dek_nonce_b64: String,
    pub wrapped_dek_ct_b64: String,
    pub payload_nonce_b64: String,
    pub payload_ct_b64: String,
    pub updated_at_unix: u64,
}

/// Decoded byte length of canonical padded standard base64, or `None` if malformed.
fn base64_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let pad = bytes.iter().rev().take_while(|b| **b == b'=').count();
    if pad > 2
        || !bytes[..bytes.len() - pad]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'))
    {
        return None;
    }
    Some(bytes.len() / 4 * 3 - pad)
}

impl MeshVaultEnvelope {
    /// Structural checks the hub can make without any key.
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if self.kdf != VAULT_KDF {
            return Err("unsupported kdf");
        }
        let exact = [
            (&self.kdf_salt_b64, VAULT_SALT_LEN),
            (&self.wrapped_dek_nonce_b64, NONCE_LEN),
            (&self.wrapped_dek_ct_b64, KEY_LEN + AEAD_TAG_LEN),
            (&self.payload_nonce_b64, NONCE_LEN),
        ];
        if exact
            .iter()
            .any(|(field, len)| base64_len(field) != Some(*len))
        {
            return Err("malformed key material");
        }
        if base64_len(&self.payload_ct_b64).is_none_or(|len| len <= AEAD_TAG_LEN) {
            return Err("malformed payload");
        }
        Ok(())
    }
}

/// Body of `PUT /api/v1/mesh-vault`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutMeshVaultRequest {
    /// `0` creates the vault; otherwise the version the update is based on.
    pub expected_version: u64,
    pub envelope: MeshVaultEnvelope,
}

/// `409 Conflict` body of `PUT /api/v1/mesh-vault`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshVaultConflict {
    pub error: String,
    /// The hub's current envelope, or `None` if the vault no longer exists.
    pub current: Option<MeshVaultEnvelope>,
}

/// A [`MeshProfile`] sealed with a client-generated invite secret the hub never sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeshInviteCiphertext {
    pub nonce_b64: String,
    pub ct_b64: String,
}

impl MeshInviteCiphertext {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if base64_len(&self.nonce_b64) != Some(NONCE_LEN) {
            return Err("malformed mesh invite nonce");
        }
        if self.ct_b64.len() > MESH_INVITE_CT_LIMIT
            || base64_len(&self.ct_b64).is_none_or(|len| len <= AEAD_TAG_LEN)
        {
            return Err("malformed or oversized mesh invite");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> MeshVault {
        MeshVault::new_default().unwrap()
    }

    #[test]
    fn default_vault_is_valid_and_random() {
        let a = vault();
        let b = vault();
        a.validate().unwrap();
        assert_eq!(a.profiles[0].subnet, DEFAULT_SUBNET);
        assert_eq!(a.profiles[0].listen_port, DEFAULT_LISTEN_PORT);
        assert!(a.profiles[0].use_hub_peer);
        assert_ne!(a.profiles[0].network_secret, b.profiles[0].network_secret);
        assert_ne!(a.profiles[0].network_name, b.profiles[0].network_name);
    }

    #[test]
    fn isolated_defaults_fill_gaps_without_conflicts() {
        let mut v = vault();
        for _ in 0..3 {
            let next = v.next_isolated_defaults().unwrap();
            v.profiles.push(next);
        }
        v.validate().unwrap();
        let subnets: Vec<_> = v.profiles.iter().map(|p| p.subnet.as_str()).collect();
        assert_eq!(
            subnets,
            [
                "10.126.1.0/24",
                "10.126.2.0/24",
                "10.126.3.0/24",
                "10.126.4.0/24"
            ]
        );
        let ports: Vec<_> = v.profiles.iter().map(|p| p.listen_port).collect();
        assert_eq!(ports, [11010, 11011, 11012, 11013]);

        v.profiles.remove(1);
        v.profiles[0].listen_port = 11011;
        let next = v.next_isolated_defaults().unwrap();
        assert_eq!(next.subnet, "10.126.2.0/24");
        assert_eq!(next.listen_port, 11010);
        v.profiles.push(next);
        v.validate().unwrap();
    }

    #[test]
    fn validation_rejects_conflicts() {
        let mut v = vault();
        let mut second = v.next_isolated_defaults().unwrap();
        second.subnet = DEFAULT_SUBNET.into();
        v.profiles.push(second.clone());
        assert!(matches!(v.validate(), Err(MeshError::SubnetOverlap(..))));

        second.subnet = "10.126.9.0/24".into();
        second.listen_port = DEFAULT_LISTEN_PORT;
        v.profiles[1] = second.clone();
        assert!(matches!(v.validate(), Err(MeshError::PortConflict(..))));

        second.listen_port = 12000;
        second.id = DEFAULT_PROFILE_ID.into();
        v.profiles[1] = second;
        assert!(matches!(v.validate(), Err(MeshError::DuplicateId(_))));

        v.profiles.clear();
        assert_eq!(v.validate(), Err(MeshError::Empty));
        let mut v = vault();
        v.schema_version = 2;
        assert_eq!(v.validate(), Err(MeshError::UnsupportedSchema(2)));
    }

    #[test]
    fn subnets_must_be_private_slash_24() {
        for ok in ["10.126.1.0/24", "192.168.7.0/24", "172.20.3.0/24"] {
            assert!(parse_private_subnet(ok).is_some(), "{ok}");
        }
        for bad in [
            "10.126.1.0/16",
            "10.126.1.5/24",
            "8.8.8.0/24",
            "172.32.0.0/24",
            "10.126.1.0",
            "10.126.256.0/24",
            "",
        ] {
            assert!(parse_private_subnet(bad).is_none(), "{bad}");
        }
        let mut v = vault();
        v.profiles[0].custom_peers = vec!["tcp://peer.example.com:11010".into()];
        v.validate().unwrap();
        v.profiles[0].custom_peers = vec!["peer.example.com".into()];
        assert!(matches!(
            v.validate(),
            Err(MeshError::InvalidProfile { .. })
        ));
    }

    #[test]
    fn profile_json_defaults_and_debug_redaction() {
        let profile: MeshProfile = serde_json::from_value(serde_json::json!({
            "id": "lab", "name": "Lab VMs", "subnet": "10.126.2.0/24", "listen_port": 11011,
            "network_name": "lab", "network_secret": "0123456789abcdef"
        }))
        .unwrap();
        assert!(profile.use_hub_peer);
        assert!(profile.custom_peers.is_empty());
        assert!(!format!("{profile:?}").contains("0123456789abcdef"));
    }

    #[test]
    fn envelope_shape() {
        let mut envelope = MeshVaultEnvelope {
            version: 1,
            kdf: VAULT_KDF.into(),
            kdf_salt_b64: "AAAAAAAAAAAAAAAAAAAAAA==".into(),
            wrapped_dek_nonce_b64: "AAAAAAAAAAAAAAAA".into(),
            wrapped_dek_ct_b64: "A".repeat(64),
            payload_nonce_b64: "AAAAAAAAAAAAAAAA".into(),
            payload_ct_b64: "AAAAAAAAAAAAAAAAAAAAAAAA".into(),
            updated_at_unix: 0,
        };
        envelope.validate_shape().unwrap();
        envelope.payload_ct_b64 = "AAAAAAAAAAAAAAAAAAAAAA==".into();
        assert!(envelope.validate_shape().is_err());
        envelope.payload_ct_b64 = "AAAA*AAAAAAAAAAAAAAAAAAA".into();
        assert!(envelope.validate_shape().is_err());
        assert_eq!(base64_len("AAA="), Some(2));
        assert_eq!(base64_len("A==="), None);
    }
}

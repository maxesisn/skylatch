//! Connection configuration.

/// Errors from [`ConnectionConfig::validate`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("hub URL must start with ws:// or wss://")]
    BadScheme,
    #[error("hub URL has no host")]
    MissingHost,
    #[error("token must not be empty")]
    EmptyToken,
}

/// How a client reaches the hub and which device it wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// Base hub URL, for example `wss://hub.example.com`.
    pub hub_url: String,
    /// Account token.
    pub token: String,
    /// Device to connect to, if already chosen.
    pub device_id: Option<String>,
    /// Try advertised direct endpoints before the relay.
    pub prefer_direct: bool,
}

impl ConnectionConfig {
    pub fn new(hub_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            hub_url: hub_url.into(),
            token: token.into(),
            device_id: None,
            prefer_direct: true,
        }
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let rest = self
            .hub_url
            .strip_prefix("wss://")
            .or_else(|| self.hub_url.strip_prefix("ws://"))
            .ok_or(ConfigError::BadScheme)?;
        if rest.trim_matches('/').is_empty() {
            return Err(ConfigError::MissingHost);
        }
        if self.token.is_empty() {
            return Err(ConfigError::EmptyToken);
        }
        Ok(())
    }

    /// URL of the hub's client WebSocket endpoint.
    pub fn client_ws_url(&self) -> String {
        format!("{}/ws/client", self.hub_url.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_builds_url() {
        let cfg = ConnectionConfig::new("wss://hub.example.com/", "tok");
        assert_eq!(cfg.validate(), Ok(()));
        assert_eq!(cfg.client_ws_url(), "wss://hub.example.com/ws/client");
        assert!(cfg.prefer_direct);
        assert_eq!(cfg.device_id, None);
    }

    #[test]
    fn rejects_bad_config() {
        assert_eq!(
            ConnectionConfig::new("https://x", "t").validate(),
            Err(ConfigError::BadScheme)
        );
        assert_eq!(
            ConnectionConfig::new("wss://", "t").validate(),
            Err(ConfigError::MissingHost)
        );
        assert_eq!(
            ConnectionConfig::new("ws://h", "").validate(),
            Err(ConfigError::EmptyToken)
        );
    }
}

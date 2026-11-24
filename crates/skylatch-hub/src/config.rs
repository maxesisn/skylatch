//! Hub configuration.

use skylatch_proto::credentials::constant_time_eq;
use std::time::Duration;

/// Runtime configuration.
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// `(account name, token)` pairs. Hosts and clients of one account see each other.
    pub accounts: Vec<(String, String)>,
    /// Heartbeat interval announced to hosts. A host silent for three intervals is dropped.
    pub heartbeat_interval: Duration,
}

impl HubConfig {
    pub fn new(accounts: Vec<(String, String)>) -> Self {
        Self {
            accounts,
            heartbeat_interval: Duration::from_secs(15),
        }
    }

    /// Returns the account that owns `token`. Compares every token in constant time per token.
    pub fn account_for_token(&self, token: &str) -> Option<&str> {
        let mut found = None;
        for (name, candidate) in &self.accounts {
            if constant_time_eq(candidate.as_bytes(), token.as_bytes()) {
                found = Some(name.as_str());
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_lookup() {
        let cfg = HubConfig::new(vec![
            ("a".into(), "tok-a".into()),
            ("b".into(), "tok-b".into()),
        ]);
        assert_eq!(cfg.account_for_token("tok-b"), Some("b"));
        assert_eq!(cfg.account_for_token("tok-a"), Some("a"));
        assert_eq!(cfg.account_for_token("tok-"), None);
        assert_eq!(cfg.account_for_token(""), None);
    }
}

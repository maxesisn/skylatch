//! Shared credential primitives for enrollment and authenticated direct connections.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use subtle::ConstantTimeEq;

pub fn random_bytes<const N: usize>() -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes)
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").expect("writing to a string cannot fail");
    }
    result
}

pub fn random_token() -> io::Result<String> {
    Ok(hex(&random_bytes::<32>()?))
}

pub fn is_token(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn token_digest(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

const TICKET_LIFETIME: u64 = 30;

fn ticket_mac(key: &str, prefix: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("HMAC accepts any key size");
    mac.update(b"skylatch:direct-ticket:v1:");
    mac.update(prefix.as_bytes());
    hex(&mac.finalize().into_bytes()[..16])
}

/// A 64-hex bearer containing a short deadline, a nonce, and a 128-bit authentication tag.
pub fn direct_ticket(key: &str) -> io::Result<String> {
    let deadline = unix_now() + TICKET_LIFETIME;
    let prefix = format!("{deadline:016x}{}", hex(&random_bytes::<8>()?));
    Ok(format!("{prefix}{}", ticket_mac(key, &prefix)))
}

pub fn valid_direct_ticket(key: &str, token: &str) -> bool {
    if !is_token(token) {
        return false;
    }
    let now = unix_now();
    let Ok(deadline) = u64::from_str_radix(&token[..16], 16) else {
        return false;
    };
    (now..=now + TICKET_LIFETIME + 10).contains(&deadline)
        && constant_time_eq(
            ticket_mac(key, &token[..32]).as_bytes(),
            &token.as_bytes()[32..],
        )
}

/// Replace a credential file atomically, without exposing the temporary file to other users.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(".skylatch-{}.tmp", hex(&random_bytes::<12>()?)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    let result = written.and_then(|()| fs::rename(&tmp, path));
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticket_binds_to_key_and_expiry() {
        let key = random_token().unwrap();
        let ticket = direct_ticket(&key).unwrap();
        assert!(valid_direct_ticket(&key, &ticket));
        assert!(!valid_direct_ticket(&random_token().unwrap(), &ticket));
        let prefix = format!("{:016x}{}", unix_now() - 1, "0".repeat(16));
        let expired = format!("{prefix}{}", ticket_mac(&key, &prefix));
        assert!(!valid_direct_ticket(&key, &expired));
    }
}

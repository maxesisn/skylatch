//! Account and device credential storage. All code consumption and credential mutations are serialized.

use crate::{HubConfig, Registry};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use serde::{Deserialize, Serialize};
use skylatch_proto::control::DeviceInfo;
use skylatch_proto::credentials::{
    constant_time_eq, is_token, random_bytes, random_token, token_digest, write_private_file,
};
use skylatch_proto::layout::WorkspaceLayout;
use skylatch_proto::onboarding::{
    EnrollResponse, LoginRequest, LoginResponse, PairingCodeResponse,
};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, Semaphore};

use crate::registry::HostCommand;

const PAIRING_LIFETIME: Duration = Duration::from_secs(600);
const ATTEMPT_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub(crate) enum AuthError {
    Unauthorized,
    Invalid,
    Conflict,
    NotFound,
    Limited,
    Storage(io::Error),
}

impl From<io::Error> for AuthError {
    fn from(error: io::Error) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Credentials {
    users: BTreeMap<String, User>,
    devices: BTreeMap<String, BTreeMap<String, String>>,
    /// Per-account workspace layouts. Absent in state files written by older hubs.
    #[serde(default)]
    layouts: HashMap<String, WorkspaceLayout>,
}

#[derive(Clone, Deserialize, Serialize)]
struct User {
    password_hash: String,
    session_token: String,
}

struct Code {
    account: String,
    expires: Instant,
    used: bool,
}

#[derive(Default)]
struct Data {
    credentials: Credentials,
    codes: HashMap<String, Code>,
    pairing_attempts: VecDeque<Instant>,
    password_attempts: VecDeque<Instant>,
}

pub(crate) struct Auth {
    path: Option<PathBuf>,
    data: Mutex<Data>,
    dummy_hash: Option<String>,
    password_workers: Semaphore,
}

fn hash_password(password: &str) -> io::Result<String> {
    let salt = SaltString::encode_b64(&random_bytes::<16>()?)
        .map_err(|error| io::Error::other(error.to_string()))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| io::Error::other(error.to_string()))
}

fn verify(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

fn write(path: Option<&Path>, credentials: &Credentials) -> io::Result<()> {
    if let Some(path) = path {
        write_private_file(path, &serde_json::to_vec_pretty(credentials)?)?;
    }
    Ok(())
}

fn try_attempt(
    attempts: &mut VecDeque<Instant>,
    now: Instant,
    limit: usize,
) -> Result<(), AuthError> {
    while attempts
        .front()
        .is_some_and(|t| now.duration_since(*t) >= ATTEMPT_WINDOW)
    {
        attempts.pop_front();
    }
    if attempts.len() >= limit {
        return Err(AuthError::Limited);
    }
    attempts.push_back(now);
    Ok(())
}

fn pairing_code() -> io::Result<String> {
    const SPACE: u32 = 1_000_000;
    const LIMIT: u32 = u32::MAX / SPACE * SPACE;
    loop {
        let sample = u32::from_le_bytes(random_bytes()?);
        if sample < LIMIT {
            let code = sample % SPACE;
            return Ok(format!("{:03}-{:03}", code / 1000, code % 1000));
        }
    }
}

fn validate_device(device_id: &str, name: &str) -> Result<(), AuthError> {
    if device_id.is_empty()
        || device_id.len() > 128
        || !device_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || matches!(device_id, "." | "..")
        || name.trim().is_empty()
        || name.len() > 256
    {
        return Err(AuthError::Invalid);
    }
    Ok(())
}

impl Auth {
    pub(crate) fn new(path: Option<PathBuf>, users: Vec<(String, String)>) -> io::Result<Self> {
        let mut credentials = Credentials::default();
        if let Some(path) = &path {
            match std::fs::read(path) {
                Ok(bytes) => credentials = serde_json::from_slice(&bytes)?,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        for (username, password) in users {
            if username.trim().is_empty()
                || username.len() > 128
                || password.is_empty()
                || password.len() > 1024
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid user credentials",
                ));
            }
            if credentials
                .users
                .get(&username)
                .is_some_and(|user| verify(&password, &user.password_hash))
            {
                continue;
            }
            credentials.users.insert(
                username,
                User {
                    password_hash: hash_password(&password)?,
                    session_token: random_token()?,
                },
            );
        }
        let dummy_hash = (!credentials.users.is_empty())
            .then(|| hash_password(&random_token()?))
            .transpose()?;
        write(path.as_deref(), &credentials)?;
        Ok(Self {
            path,
            data: Mutex::new(Data {
                credentials,
                ..Data::default()
            }),
            dummy_hash,
            password_workers: Semaphore::new(2),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Data> {
        self.data.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn account(&self, config: &HubConfig, token: &str) -> Option<String> {
        if let Some(account) = config.account_for_token(token) {
            return Some(account.into());
        }
        if !is_token(token) {
            return None;
        }
        let data = self.lock();
        let mut found = None;
        for (account, user) in &data.credentials.users {
            if constant_time_eq(user.session_token.as_bytes(), token.as_bytes()) {
                found = Some(account);
            }
        }
        found.cloned()
    }

    pub(crate) fn login(&self, request: LoginRequest) -> Result<LoginResponse, AuthError> {
        if request.username.len() > 128 || request.password.len() > 1024 {
            return Err(AuthError::Unauthorized);
        }
        let Some(dummy_hash) = &self.dummy_hash else {
            return Err(AuthError::Unauthorized);
        };
        let _worker = self
            .password_workers
            .try_acquire()
            .map_err(|_| AuthError::Limited)?;
        let user = {
            let mut data = self.lock();
            try_attempt(&mut data.password_attempts, Instant::now(), 60)?;
            data.credentials.users.get(&request.username).cloned()
        };
        let hash = user
            .as_ref()
            .map_or(dummy_hash.as_str(), |u| u.password_hash.as_str());
        let valid = verify(&request.password, hash);
        match user.filter(|_| valid) {
            Some(user) => Ok(LoginResponse {
                account: request.username,
                token: user.session_token,
            }),
            None => Err(AuthError::Unauthorized),
        }
    }

    pub(crate) fn create_code(&self, account: String) -> Result<PairingCodeResponse, AuthError> {
        let mut data = self.lock();
        let now = Instant::now();
        data.codes.retain(|_, code| code.expires > now);
        if data.codes.len() >= 10_000
            || data
                .codes
                .values()
                .filter(|code| code.account == account && !code.used)
                .count()
                >= 10
        {
            return Err(AuthError::Limited);
        }
        let code = loop {
            let code = pairing_code()?;
            if !data.codes.contains_key(&code) {
                break code;
            }
        };
        data.codes.insert(
            code.clone(),
            Code {
                account,
                expires: now + PAIRING_LIFETIME,
                used: false,
            },
        );
        Ok(PairingCodeResponse {
            code,
            expires_in_secs: PAIRING_LIFETIME.as_secs(),
        })
    }

    pub(crate) fn enroll_with_code(
        &self,
        code: &str,
        device_id: String,
        name: &str,
    ) -> Result<EnrollResponse, AuthError> {
        validate_device(&device_id, name)?;
        let code = code.trim();
        if code.len() != 7
            || code.as_bytes()[3] != b'-'
            || !code
                .bytes()
                .enumerate()
                .all(|(i, b)| i == 3 || b.is_ascii_digit())
        {
            return Err(AuthError::Invalid);
        }
        let mut data = self.lock();
        let now = Instant::now();
        try_attempt(&mut data.pairing_attempts, now, 20)?;
        let account = data
            .codes
            .get(code)
            .filter(|c| !c.used && c.expires > now)
            .ok_or(AuthError::Unauthorized)?
            .account
            .clone();
        let enrolled = self.enroll_locked(&mut data, account, device_id)?;
        data.codes
            .get_mut(code)
            .expect("code was checked above")
            .used = true;
        Ok(enrolled)
    }

    pub(crate) fn enroll_with_password(
        &self,
        request: LoginRequest,
        device_id: String,
        name: &str,
    ) -> Result<EnrollResponse, AuthError> {
        validate_device(&device_id, name)?;
        let account = self.login(request)?.account;
        self.enroll_locked(&mut self.lock(), account, device_id)
    }

    fn enroll_locked(
        &self,
        data: &mut Data,
        account: String,
        device_id: String,
    ) -> Result<EnrollResponse, AuthError> {
        if data
            .credentials
            .devices
            .get(&account)
            .is_some_and(|d| d.contains_key(&device_id))
        {
            return Err(AuthError::Conflict);
        }
        let device_token = random_token()?;
        let mut updated = data.credentials.clone();
        updated
            .devices
            .entry(account.clone())
            .or_default()
            .insert(device_id.clone(), token_digest(&device_token));
        write(self.path.as_deref(), &updated)?;
        data.credentials = updated;
        Ok(EnrollResponse {
            account,
            device_id,
            device_token,
        })
    }

    pub(crate) fn register_host(
        &self,
        config: &HubConfig,
        registry: &Registry,
        token: &str,
        device: DeviceInfo,
        tx: mpsc::Sender<HostCommand>,
    ) -> Result<(String, u64, oneshot::Receiver<()>), AuthError> {
        if let Some(account) = config.account_for_token(token) {
            if device.device_id.is_empty() {
                return Err(AuthError::Invalid);
            }
            let (id, cancel) = registry.register_host(account, device, tx, false);
            return Ok((account.into(), id, cancel));
        }
        if !is_token(token) {
            return Err(AuthError::Unauthorized);
        }
        let data = self.lock();
        let digest = token_digest(token);
        let mut account = None;
        for (name, devices) in &data.credentials.devices {
            if devices
                .get(&device.device_id)
                .is_some_and(|v| constant_time_eq(v.as_bytes(), digest.as_bytes()))
            {
                account = Some(name);
            }
        }
        let account = account.ok_or(AuthError::Unauthorized)?.clone();
        // Keep the credential lock until registration so revocation cannot miss this socket.
        let (id, cancel) = registry.register_host(&account, device, tx, true);
        Ok((account, id, cancel))
    }

    pub(crate) fn layout(&self, account: &str) -> WorkspaceLayout {
        self.lock()
            .credentials
            .layouts
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn set_layout(
        &self,
        account: &str,
        layout: WorkspaceLayout,
    ) -> Result<WorkspaceLayout, AuthError> {
        let mut data = self.lock();
        let mut updated = data.credentials.clone();
        updated.layouts.insert(account.to_string(), layout.clone());
        write(self.path.as_deref(), &updated)?;
        data.credentials = updated;
        Ok(layout)
    }

    pub(crate) fn revoke(
        &self,
        registry: &Registry,
        account: &str,
        device_id: &str,
    ) -> Result<(), AuthError> {
        let mut data = self.lock();
        let mut updated = data.credentials.clone();
        if updated
            .devices
            .get_mut(account)
            .and_then(|d| d.remove(device_id))
            .is_none()
        {
            return Err(AuthError::NotFound);
        }
        write(self.path.as_deref(), &updated)?;
        data.credentials = updated;
        registry.revoke_host(account, device_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_code_cannot_enroll() {
        let auth = Auth::new(None, vec![]).unwrap();
        let code = auth.create_code("demo".into()).unwrap().code;
        auth.lock().codes.get_mut(&code).unwrap().expires = Instant::now();
        assert!(matches!(
            auth.enroll_with_code(&code, "demo".into(), "Demo"),
            Err(AuthError::Unauthorized)
        ));
    }
}

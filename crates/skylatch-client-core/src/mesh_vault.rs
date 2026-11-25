//! End-to-end encryption of the account mesh vault and of pairing-code mesh invites.
//!
//! A random 256-bit data key (DEK) seals the `MeshVault` JSON with ChaCha20-Poly1305. An
//! Argon2id key derived from the login password and a per-wrap salt seals the DEK. Changing the
//! password re-wraps only the DEK; the recovery key is the DEK itself in a checksummed encoding.
//!
//! Mesh invites are sealed with a client-generated invite secret that travels next to the
//! pairing code but is never sent to the hub, which mints and therefore knows the code itself.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{digest, hkdf};
use skylatch_proto::mesh::{
    MeshError, MeshInviteCiphertext, MeshProfile, MeshVault, MeshVaultEnvelope, KEY_LEN,
    MESH_INVITE_CT_LIMIT, NONCE_LEN, VAULT_KDF, VAULT_SALT_LEN, VAULT_SCHEMA_VERSION,
};
use std::fmt;
use zeroize::{Zeroize, Zeroizing};

const ARGON2_MEMORY_KIB: u32 = 19 * 1024;
const ARGON2_ITERATIONS: u32 = 2;
const ARGON2_LANES: u32 = 1;

const AAD_DEK: &[u8] = b"skylatch:mesh-vault:dek:argon2id-v1";
const AAD_PAYLOAD: &[u8] = b"skylatch:mesh-vault:payload:v1";
const INVITE_CONTEXT: &[u8] = b"skylatch:mesh-invite:v1";

const RECOVERY_PREFIX: &str = "SLK1";
const RECOVERY_CHECK_LEN: usize = 3;
const INVITE_SECRET_SYMBOLS: usize = 20;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeshVaultError {
    #[error("wrong password, or the mesh vault is corrupted")]
    WrongPasswordOrCorrupted,
    #[error("wrong recovery key, or the mesh vault is corrupted")]
    WrongKeyOrCorrupted,
    #[error("wrong invite secret, or the mesh invite is corrupted")]
    WrongInviteSecretOrCorrupted,
    #[error("unsupported key derivation `{0}`")]
    UnsupportedKdf(String),
    #[error("malformed mesh vault envelope: {0}")]
    MalformedEnvelope(&'static str),
    #[error("not a Skylatch recovery key")]
    InvalidRecoveryKey,
    #[error("recovery key checksum mismatch; check for typos")]
    RecoveryKeyChecksum,
    #[error("not a mesh invite secret")]
    InvalidInviteSecret,
    #[error("invalid mesh configuration: {0}")]
    InvalidVault(#[from] MeshError),
    #[error("decrypted mesh configuration is not valid JSON")]
    InvalidPayload,
    #[error("mesh invite exceeds the hub size limit")]
    InviteTooLarge,
    #[error("cryptographic primitive failed")]
    Crypto,
}

/// A 256-bit symmetric key, wiped from memory on drop.
#[derive(Clone)]
pub struct VaultKey([u8; KEY_LEN]);

impl VaultKey {
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("VaultKey(<redacted>)")
    }
}

fn rng() -> &'static SystemRandom {
    static RNG: std::sync::OnceLock<SystemRandom> = std::sync::OnceLock::new();
    RNG.get_or_init(SystemRandom::new)
}

fn random<const N: usize>() -> Result<[u8; N], MeshVaultError> {
    let mut bytes = [0; N];
    rng().fill(&mut bytes).map_err(|_| MeshVaultError::Crypto)?;
    Ok(bytes)
}

pub fn generate_dek() -> Result<VaultKey, MeshVaultError> {
    random().map(VaultKey)
}

fn seal(
    key: &VaultKey,
    nonce: [u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, MeshVaultError> {
    let key =
        UnboundKey::new(&CHACHA20_POLY1305, key.as_bytes()).map_err(|_| MeshVaultError::Crypto)?;
    let mut buffer = plaintext.to_vec();
    LessSafeKey::new(key)
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut buffer,
        )
        .map_err(|_| MeshVaultError::Crypto)?;
    Ok(buffer)
}

/// Decrypts and authenticates; `None` means the key is wrong or the data was altered.
fn open(
    key: &VaultKey,
    nonce: [u8; NONCE_LEN],
    aad: &[u8],
    ciphertext: &[u8],
) -> Option<Zeroizing<Vec<u8>>> {
    let key = UnboundKey::new(&CHACHA20_POLY1305, key.as_bytes()).ok()?;
    let mut buffer = Zeroizing::new(ciphertext.to_vec());
    let len = LessSafeKey::new(key)
        .open_in_place(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(aad),
            &mut buffer,
        )
        .ok()?
        .len();
    buffer.truncate(len);
    Some(buffer)
}

fn derive_kek(password: &str, salt: &[u8]) -> Result<VaultKey, MeshVaultError> {
    let params = Params::new(
        ARGON2_MEMORY_KIB,
        ARGON2_ITERATIONS,
        ARGON2_LANES,
        Some(KEY_LEN),
    )
    .map_err(|_| MeshVaultError::Crypto)?;
    let mut key = VaultKey([0; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut key.0)
        .map_err(|_| MeshVaultError::Crypto)?;
    Ok(key)
}

fn decode<const N: usize>(field: &str, what: &'static str) -> Result<[u8; N], MeshVaultError> {
    B64.decode(field)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(MeshVaultError::MalformedEnvelope(what))
}

fn decode_vec(field: &str, what: &'static str) -> Result<Vec<u8>, MeshVaultError> {
    B64.decode(field)
        .map_err(|_| MeshVaultError::MalformedEnvelope(what))
}

fn parse_vault(json: &[u8]) -> Result<MeshVault, MeshVaultError> {
    let vault: MeshVault =
        serde_json::from_slice(json).map_err(|_| MeshVaultError::InvalidPayload)?;
    if vault.schema_version != VAULT_SCHEMA_VERSION {
        return Err(MeshError::UnsupportedSchema(vault.schema_version).into());
    }
    Ok(vault)
}

fn seal_payload(vault: &MeshVault, dek: &VaultKey) -> Result<(String, String), MeshVaultError> {
    vault.validate()?;
    let json =
        Zeroizing::new(serde_json::to_vec(vault).map_err(|_| MeshVaultError::InvalidPayload)?);
    let nonce = random()?;
    let ct = seal(dek, nonce, AAD_PAYLOAD, &json)?;
    Ok((B64.encode(nonce), B64.encode(ct)))
}

fn open_payload(
    envelope: &MeshVaultEnvelope,
    dek: &VaultKey,
) -> Result<Option<MeshVault>, MeshVaultError> {
    let nonce = decode(&envelope.payload_nonce_b64, "payload nonce")?;
    let ct = decode_vec(&envelope.payload_ct_b64, "payload")?;
    open(dek, nonce, AAD_PAYLOAD, &ct)
        .map(|json| parse_vault(&json))
        .transpose()
}

struct WrappedDek {
    salt_b64: String,
    nonce_b64: String,
    ct_b64: String,
}

fn wrap_dek(dek: &VaultKey, password: &str) -> Result<WrappedDek, MeshVaultError> {
    let salt: [u8; VAULT_SALT_LEN] = random()?;
    let kek = derive_kek(password, &salt)?;
    let nonce = random()?;
    let ct = seal(&kek, nonce, AAD_DEK, dek.as_bytes())?;
    Ok(WrappedDek {
        salt_b64: B64.encode(salt),
        nonce_b64: B64.encode(nonce),
        ct_b64: B64.encode(ct),
    })
}

/// Encrypts `vault` under `dek` and wraps `dek` under `password`.
pub fn seal_vault(
    vault: &MeshVault,
    dek: &VaultKey,
    password: &str,
    version: u64,
    now_unix: u64,
) -> Result<MeshVaultEnvelope, MeshVaultError> {
    let (payload_nonce_b64, payload_ct_b64) = seal_payload(vault, dek)?;
    let wrapped = wrap_dek(dek, password)?;
    Ok(MeshVaultEnvelope {
        version,
        kdf: VAULT_KDF.into(),
        kdf_salt_b64: wrapped.salt_b64,
        wrapped_dek_nonce_b64: wrapped.nonce_b64,
        wrapped_dek_ct_b64: wrapped.ct_b64,
        payload_nonce_b64,
        payload_ct_b64,
        updated_at_unix: now_unix,
    })
}

/// Replaces the payload of an existing envelope without the password, keeping its wrapped DEK.
/// Fails unless `dek` is the key that envelope's payload was sealed with.
pub fn reseal_vault(
    envelope: &MeshVaultEnvelope,
    vault: &MeshVault,
    dek: &VaultKey,
    now_unix: u64,
) -> Result<MeshVaultEnvelope, MeshVaultError> {
    open_payload(envelope, dek)?.ok_or(MeshVaultError::WrongKeyOrCorrupted)?;
    let (payload_nonce_b64, payload_ct_b64) = seal_payload(vault, dek)?;
    Ok(MeshVaultEnvelope {
        payload_nonce_b64,
        payload_ct_b64,
        updated_at_unix: now_unix,
        ..envelope.clone()
    })
}

/// Decrypts the vault with the login password and returns it with its DEK.
pub fn open_vault(
    envelope: &MeshVaultEnvelope,
    password: &str,
) -> Result<(MeshVault, VaultKey), MeshVaultError> {
    if envelope.kdf != VAULT_KDF {
        return Err(MeshVaultError::UnsupportedKdf(envelope.kdf.clone()));
    }
    let salt: [u8; VAULT_SALT_LEN] = decode(&envelope.kdf_salt_b64, "kdf salt")?;
    let nonce = decode(&envelope.wrapped_dek_nonce_b64, "wrapped key nonce")?;
    let ct = decode_vec(&envelope.wrapped_dek_ct_b64, "wrapped key")?;
    let kek = derive_kek(password, &salt)?;
    let dek = open(&kek, nonce, AAD_DEK, &ct)
        .and_then(|bytes| <[u8; KEY_LEN]>::try_from(bytes.as_slice()).ok())
        .map(VaultKey)
        .ok_or(MeshVaultError::WrongPasswordOrCorrupted)?;
    let vault = open_payload(envelope, &dek)?.ok_or(MeshVaultError::WrongPasswordOrCorrupted)?;
    Ok((vault, dek))
}

pub fn open_vault_with_recovery_key(
    envelope: &MeshVaultEnvelope,
    dek: &VaultKey,
) -> Result<MeshVault, MeshVaultError> {
    open_payload(envelope, dek)?.ok_or(MeshVaultError::WrongKeyOrCorrupted)
}

/// Re-wraps the DEK under `new_password`; the payload ciphertext is left untouched.
pub fn rewrap_dek(
    envelope: &MeshVaultEnvelope,
    dek: &VaultKey,
    new_password: &str,
    now_unix: u64,
) -> Result<MeshVaultEnvelope, MeshVaultError> {
    if envelope.kdf != VAULT_KDF {
        return Err(MeshVaultError::UnsupportedKdf(envelope.kdf.clone()));
    }
    open_payload(envelope, dek)?.ok_or(MeshVaultError::WrongKeyOrCorrupted)?;
    let wrapped = wrap_dek(dek, new_password)?;
    Ok(MeshVaultEnvelope {
        kdf_salt_b64: wrapped.salt_b64,
        wrapped_dek_nonce_b64: wrapped.nonce_b64,
        wrapped_dek_ct_b64: wrapped.ct_b64,
        updated_at_unix: now_unix,
        ..envelope.clone()
    })
}

fn base32_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 8 / 5 + 1);
    let (mut acc, mut bits) = (0u32, 0u32);
    for byte in bytes {
        acc = (acc << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(CROCKFORD[(acc >> bits) as usize & 31] as char);
        }
    }
    if bits > 0 {
        out.push(CROCKFORD[(acc << (5 - bits)) as usize & 31] as char);
    }
    out
}

fn base32_symbol(c: char) -> Option<u8> {
    let c = match c.to_ascii_uppercase() {
        'O' => '0',
        'I' | 'L' => '1',
        c => c,
    };
    CROCKFORD
        .iter()
        .position(|s| *s as char == c)
        .map(|i| i as u8)
}

/// Normalizes user input: ignores case, dashes and whitespace, and accepts O/I/L for 0/1/1.
fn base32_symbols(text: &str) -> Option<Vec<u8>> {
    text.chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .map(base32_symbol)
        .collect()
}

fn base32_decode(symbols: &[u8], len: usize) -> Option<Vec<u8>> {
    if symbols.len() != (len * 8).div_ceil(5) {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    let (mut acc, mut bits) = (0u32, 0u32);
    for symbol in symbols {
        acc = (acc << 5) | u32::from(*symbol);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    (acc & ((1 << bits) - 1) == 0).then_some(out)
}

fn group(text: &str) -> String {
    text.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("-")
}

fn recovery_check(dek: &[u8]) -> [u8; RECOVERY_CHECK_LEN] {
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(b"skylatch:recovery-key:v1:");
    ctx.update(dek);
    let digest = ctx.finish();
    let mut check = [0; RECOVERY_CHECK_LEN];
    check.copy_from_slice(&digest.as_ref()[..RECOVERY_CHECK_LEN]);
    check
}

/// `SLK1-` followed by 14 groups of four Crockford base32 symbols with a 24-bit checksum.
pub fn format_recovery_key(dek: &VaultKey) -> String {
    let mut bytes = Zeroizing::new(dek.as_bytes().to_vec());
    bytes.extend_from_slice(&recovery_check(dek.as_bytes()));
    let encoded = Zeroizing::new(base32_encode(&bytes));
    format!("{RECOVERY_PREFIX}-{}", group(&encoded))
}

pub fn parse_recovery_key(text: &str) -> Result<VaultKey, MeshVaultError> {
    let text = text.trim();
    let body = text
        .get(..RECOVERY_PREFIX.len())
        .filter(|p| p.eq_ignore_ascii_case(RECOVERY_PREFIX))
        .map(|_| &text[RECOVERY_PREFIX.len()..])
        .ok_or(MeshVaultError::InvalidRecoveryKey)?;
    let symbols = Zeroizing::new(base32_symbols(body).ok_or(MeshVaultError::InvalidRecoveryKey)?);
    let bytes = Zeroizing::new(
        base32_decode(&symbols, KEY_LEN + RECOVERY_CHECK_LEN)
            .ok_or(MeshVaultError::InvalidRecoveryKey)?,
    );
    let (key, check) = bytes.split_at(KEY_LEN);
    if recovery_check(key) != check {
        return Err(MeshVaultError::RecoveryKeyChecksum);
    }
    let mut dek = VaultKey([0; KEY_LEN]);
    dek.0.copy_from_slice(key);
    Ok(dek)
}

/// A 100-bit secret that accompanies a pairing code to decrypt its mesh invite.
#[derive(Clone)]
pub struct InviteSecret(Zeroizing<String>);

impl InviteSecret {
    pub fn generate() -> Result<Self, MeshVaultError> {
        let bytes = Zeroizing::new(random::<INVITE_SECRET_SYMBOLS>()?);
        Ok(Self(Zeroizing::new(
            bytes
                .iter()
                .map(|b| CROCKFORD[usize::from(b & 31)] as char)
                .collect(),
        )))
    }

    pub fn parse(text: &str) -> Result<Self, MeshVaultError> {
        let symbols = base32_symbols(text)
            .filter(|s| s.len() == INVITE_SECRET_SYMBOLS)
            .ok_or(MeshVaultError::InvalidInviteSecret)?;
        Ok(Self(Zeroizing::new(
            symbols
                .iter()
                .map(|s| CROCKFORD[usize::from(*s)] as char)
                .collect(),
        )))
    }

    /// Grouped form for display, e.g. `7K2M-9QXD-...`.
    pub fn display(&self) -> String {
        group(&self.0)
    }

    fn key(&self) -> Result<VaultKey, MeshVaultError> {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, INVITE_CONTEXT).extract(self.0.as_bytes());
        let okm = prk
            .expand(&[b"chacha20-poly1305"], hkdf::HKDF_SHA256)
            .map_err(|_| MeshVaultError::Crypto)?;
        let mut key = VaultKey([0; KEY_LEN]);
        okm.fill(&mut key.0).map_err(|_| MeshVaultError::Crypto)?;
        Ok(key)
    }
}

impl fmt::Debug for InviteSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("InviteSecret(<redacted>)")
    }
}

/// Joins a hub pairing code and an invite secret into one token, e.g. `482-917-7K2M-...`.
pub fn format_join_code(pairing_code: &str, secret: &InviteSecret) -> String {
    format!("{}-{}", pairing_code.trim(), secret.display())
}

/// Splits a join token into the pairing code sent to the hub and the optional invite secret.
pub fn split_join_code(join_code: &str) -> Result<(String, Option<InviteSecret>), MeshVaultError> {
    let join_code = join_code.trim();
    let is_code = join_code.len() >= 7
        && join_code.as_bytes()[..7].iter().enumerate().all(|(i, b)| {
            if i == 3 {
                *b == b'-'
            } else {
                b.is_ascii_digit()
            }
        });
    if !is_code {
        return Err(MeshVaultError::InvalidInviteSecret);
    }
    let (code, rest) = join_code.split_at(7);
    let secret = match rest.strip_prefix('-') {
        Some(secret) => Some(InviteSecret::parse(secret)?),
        None if rest.is_empty() => None,
        None => return Err(MeshVaultError::InvalidInviteSecret),
    };
    Ok((code.to_string(), secret))
}

pub fn seal_mesh_invite(
    profile: &MeshProfile,
    secret: &InviteSecret,
) -> Result<MeshInviteCiphertext, MeshVaultError> {
    profile.validate()?;
    let json =
        Zeroizing::new(serde_json::to_vec(profile).map_err(|_| MeshVaultError::InvalidPayload)?);
    let nonce = random()?;
    let invite = MeshInviteCiphertext {
        nonce_b64: B64.encode(nonce),
        ct_b64: B64.encode(seal(&secret.key()?, nonce, INVITE_CONTEXT, &json)?),
    };
    if invite.ct_b64.len() > MESH_INVITE_CT_LIMIT {
        return Err(MeshVaultError::InviteTooLarge);
    }
    Ok(invite)
}

pub fn open_mesh_invite(
    invite: &MeshInviteCiphertext,
    secret: &InviteSecret,
) -> Result<MeshProfile, MeshVaultError> {
    let nonce = decode(&invite.nonce_b64, "invite nonce")?;
    let ct = decode_vec(&invite.ct_b64, "invite")?;
    let json = open(&secret.key()?, nonce, INVITE_CONTEXT, &ct)
        .ok_or(MeshVaultError::WrongInviteSecretOrCorrupted)?;
    let profile: MeshProfile =
        serde_json::from_slice(&json).map_err(|_| MeshVaultError::InvalidPayload)?;
    profile.validate()?;
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_trip() {
        for len in [0, 1, 5, 13, 35] {
            let bytes: Vec<u8> = (0..len as u8).map(|b| b.wrapping_mul(37)).collect();
            let text = base32_encode(&bytes);
            let symbols = base32_symbols(&text).unwrap();
            assert_eq!(base32_decode(&symbols, len).unwrap(), bytes);
        }
    }

    #[test]
    fn kek_depends_on_password_and_salt() {
        let a = derive_kek("password one", &[1; 16]).unwrap();
        let b = derive_kek("password two", &[1; 16]).unwrap();
        let c = derive_kek("password one", &[2; 16]).unwrap();
        assert_eq!(
            a.as_bytes(),
            derive_kek("password one", &[1; 16]).unwrap().as_bytes()
        );
        assert_ne!(a.as_bytes(), b.as_bytes());
        assert_ne!(a.as_bytes(), c.as_bytes());
    }
}

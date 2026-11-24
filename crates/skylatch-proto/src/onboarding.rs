//! HTTP onboarding request and response bodies.

use crate::control::DirectEndpoint;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, Deserialize)]
pub struct LoginResponse {
    pub account: String,
    pub token: String,
}

#[derive(Serialize, Deserialize)]
pub struct PairingCodeResponse {
    pub code: String,
    pub expires_in_secs: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnrollRequest {
    Code(CodeEnrollment),
    Password(PasswordEnrollment),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeEnrollment {
    pub code: String,
    pub device_id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PasswordEnrollment {
    pub username: String,
    pub password: String,
    pub device_id: String,
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct EnrollResponse {
    pub account: String,
    pub device_id: String,
    pub device_token: String,
}

#[derive(Serialize, Deserialize)]
pub struct DirectTicketRequest {
    pub device_id: String,
}

#[derive(Serialize, Deserialize)]
pub struct DirectTicketResponse {
    pub ticket: String,
    pub endpoints: Vec<DirectEndpoint>,
}

impl fmt::Debug for LoginResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginResponse")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PairingCodeResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingCodeResponse")
            .field("expires_in_secs", &self.expires_in_secs)
            .finish_non_exhaustive()
    }
}

//! The hub's HTTP onboarding and direct-ticket endpoints.

use crate::auth::AuthError;
use crate::ratelimit::ClientIp;
use crate::server::{bearer, AppState};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use skylatch_proto::credentials::direct_ticket;
use skylatch_proto::layout::WorkspaceLayout;
use skylatch_proto::mesh::{HubInfo, MeshVaultConflict, PutMeshVaultRequest};
use skylatch_proto::onboarding::{
    CreatePairingCodeRequest, DirectTicketRequest, DirectTicketResponse, EnrollRequest,
    LoginRequest, SignupRequest,
};

pub(crate) async fn console() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, "default-src 'none'; connect-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Html(include_str!("console.html")),
    )
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::VaultConflict(current) => {
                let body = MeshVaultConflict {
                    error: "mesh vault version conflict".into(),
                    current: current.map(|envelope| *envelope),
                };
                return (
                    StatusCode::CONFLICT,
                    [(header::CACHE_CONTROL, "no-store")],
                    Json(body),
                )
                    .into_response();
            }
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::SignupDisabled => (StatusCode::FORBIDDEN, "signup is disabled on this hub"),
            Self::AccountExists => (StatusCode::CONFLICT, "account already exists"),
            Self::DeviceLimit => (
                StatusCode::FORBIDDEN,
                "this account has reached its device limit",
            ),
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "invalid or expired credentials"),
            Self::Invalid => (
                StatusCode::BAD_REQUEST,
                "invalid pairing code, device ID or name",
            ),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "device already enrolled; revoke its token first",
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, "device or credential not found"),
            Self::Offline => (StatusCode::CONFLICT, "device_offline"),
            Self::Online => (
                StatusCode::CONFLICT,
                "device is online; revoke its token to disconnect it",
            ),
            Self::Limited => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many attempts; try again shortly",
            ),
            Self::Storage(error) => {
                tracing::error!(%error, "credential operation failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "credential operation failed",
                )
            }
        };
        (
            status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"error": message})),
        )
            .into_response()
    }
}

fn secret(response: impl Serialize) -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Json(response))
}

fn account(state: &AppState, headers: &HeaderMap) -> Result<String, AuthError> {
    bearer(headers)
        .and_then(|token| state.auth.account(&state.config, token))
        .ok_or(AuthError::Unauthorized)
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, AuthError> + Send + 'static,
) -> Result<T, AuthError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(std::io::Error::other)?
}

pub(crate) async fn info(State(state): State<AppState>) -> Json<HubInfo> {
    Json(HubInfo {
        version: env!("CARGO_PKG_VERSION").into(),
        mode: state.config.mode.as_str().into(),
        easytier_peers: state.config.easytier_peers.clone(),
        allow_signup: state.config.allow_signup,
        max_devices_per_account: state.config.max_devices_per_account,
    })
}

pub(crate) async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse, AuthError> {
    if !state.limits.login.allow(ip) {
        return Err(AuthError::Limited);
    }
    blocking(move || state.auth.login(request))
        .await
        .map(secret)
}

pub(crate) async fn signup(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(request): Json<SignupRequest>,
) -> Result<impl IntoResponse, AuthError> {
    if !state.config.allow_signup {
        return Err(AuthError::SignupDisabled);
    }
    if !state.limits.signup.allow(ip) {
        return Err(AuthError::Limited);
    }
    let created = blocking(move || state.auth.signup(&state.config, request)).await?;
    Ok((StatusCode::CREATED, secret(created)))
}

pub(crate) async fn create_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AuthError> {
    let account = account(&state, &headers)?;
    let request = if body.iter().all(u8::is_ascii_whitespace) {
        CreatePairingCodeRequest::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|_| AuthError::BadRequest("invalid pairing code request"))?
    };
    state
        .auth
        .create_code(account, request.mesh_invite)
        .map(secret)
}

pub(crate) async fn enroll(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Json(request): Json<EnrollRequest>,
) -> Result<impl IntoResponse, AuthError> {
    if !state.limits.enroll.allow(ip) {
        return Err(AuthError::Limited);
    }
    blocking(move || match request {
        EnrollRequest::Code(request) => {
            state
                .auth
                .enroll_with_code(&request.code, request.device_id, &request.name)
        }
        EnrollRequest::Password(request) => state.auth.enroll_with_password(
            LoginRequest {
                username: request.username,
                password: request.password,
            },
            request.device_id,
            &request.name,
        ),
    })
    .await
    .map(secret)
}

pub(crate) async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<StatusCode, AuthError> {
    let account = account(&state, &headers)?;
    blocking(move || state.auth.revoke(&state.registry, &account, &device_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_device(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<StatusCode, AuthError> {
    let account = account(&state, &headers)?;
    blocking(move || {
        state
            .auth
            .delete_offline(&state.registry, &account, &device_id)
    })
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn direct_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<DirectTicketRequest>,
) -> Result<impl IntoResponse, AuthError> {
    let account = account(&state, &headers)?;
    let (key, endpoints) = state
        .registry
        .direct_access(&account, &request.device_id)
        .ok_or_else(|| {
            if state.registry.info(&account, &request.device_id).is_none()
                && state.auth.is_known(&account, &request.device_id)
            {
                AuthError::Offline
            } else {
                AuthError::NotFound
            }
        })?;
    Ok(secret(DirectTicketResponse {
        ticket: direct_ticket(&key)?,
        endpoints,
    }))
}

pub(crate) async fn get_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AuthError> {
    let account = account(&state, &headers)?;
    Ok(secret(state.auth.layout(&account)))
}

pub(crate) async fn put_layout(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AuthError> {
    let account = account(&state, &headers)?;
    let bad = |message: String| {
        (
            StatusCode::BAD_REQUEST,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"error": message})),
        )
            .into_response()
    };
    let layout = match serde_json::from_slice::<WorkspaceLayout>(&body) {
        Ok(layout) => layout,
        Err(error) => return Ok(bad(format!("invalid layout JSON: {error}"))),
    };
    if let Err(message) = layout.validate() {
        return Ok(bad(message));
    }
    let saved = blocking(move || state.auth.set_layout(&account, layout)).await?;
    Ok(secret(saved).into_response())
}

pub(crate) async fn get_mesh_vault(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AuthError> {
    let account = account(&state, &headers)?;
    Ok(match state.auth.mesh_vault(&account) {
        Some(envelope) => secret(envelope).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [(header::CACHE_CONTROL, "no-store")],
            Json(serde_json::json!({"error": "no mesh vault"})),
        )
            .into_response(),
    })
}

pub(crate) async fn put_mesh_vault(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, AuthError> {
    let account = account(&state, &headers)?;
    let request: PutMeshVaultRequest = serde_json::from_slice(&body)
        .map_err(|_| AuthError::BadRequest("invalid mesh vault request"))?;
    blocking(move || state.auth.put_mesh_vault(&account, request))
        .await
        .map(secret)
}

pub(crate) async fn delete_mesh_vault(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<StatusCode, AuthError> {
    let account = account(&state, &headers)?;
    blocking(move || state.auth.delete_mesh_vault(&account)).await?;
    Ok(StatusCode::NO_CONTENT)
}

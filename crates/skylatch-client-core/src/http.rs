//! Hub HTTP onboarding client. Credentials are never forwarded across redirects.

use crate::{transport, ClientError};
use reqwest::{Client, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use skylatch_proto::layout::WorkspaceLayout;
use skylatch_proto::onboarding::{
    DirectTicketRequest, DirectTicketResponse, EnrollRequest, EnrollResponse, LoginRequest,
    LoginResponse, PairingCodeResponse,
};
use std::sync::OnceLock;
use std::time::Duration;

pub(crate) fn http_runtime() -> Result<&'static tokio::runtime::Runtime, ClientError> {
    // Keep reqwest's connection pool on one dedicated, always-active reactor so a pooled
    // keep-alive socket is never parked on a caller's blocked or dropped current_thread runtime.
    static HTTP_RT: OnceLock<Result<tokio::runtime::Runtime, ()>> = OnceLock::new();
    HTTP_RT
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .thread_name("skylatch-http")
                .enable_all()
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| ClientError::Unexpected("cannot initialize hub HTTP runtime"))
}

fn client() -> Result<&'static Client, ClientError> {
    static CLIENT: OnceLock<Result<Client, ()>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(transport::HANDSHAKE_TIMEOUT)
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| ClientError::Unexpected("cannot initialize hub HTTP client"))
}

fn endpoint(hub_url: &str, route: &str) -> Result<String, ClientError> {
    let (scheme, uri) = transport::parse_base(hub_url, true)?;
    if uri.query().is_some() {
        return Err(ClientError::InvalidUrl);
    }
    let scheme = if scheme == "wss" { "https" } else { "http" };
    let root = uri.path().trim_end_matches('/');
    let root = root
        .strip_suffix("/ws/host")
        .or_else(|| root.strip_suffix("/ws/client"))
        .unwrap_or(root);
    let authority = uri.authority().ok_or(ClientError::InvalidUrl)?;
    Ok(format!("{scheme}://{authority}{root}/api/v1/{route}"))
}

fn post(hub_url: &str, route: &str) -> Result<RequestBuilder, ClientError> {
    Ok(client()?.post(endpoint(hub_url, route)?))
}

async fn dispatch(request: RequestBuilder) -> Result<Response, ClientError> {
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            ClientError::Timeout("hub HTTP request")
        } else {
            ClientError::Unexpected("could not reach hub HTTP endpoint")
        }
    })?;
    let status = response.status();
    if !status.is_success() {
        let message = match status.as_u16() {
            400 | 422 => "invalid request",
            401 => "invalid or expired credentials",
            404 => "endpoint or device not found",
            409 => "device already enrolled; revoke its token first",
            429 => "too many attempts; try again shortly",
            _ => "request failed",
        };
        return Err(ClientError::Hub {
            code: status.as_u16().to_string(),
            message: message.into(),
        });
    }
    Ok(response)
}

async fn send<T: DeserializeOwned + Send + 'static>(
    request: RequestBuilder,
) -> Result<T, ClientError> {
    http_runtime()?
        .spawn(async move {
            dispatch(request)
                .await?
                .json()
                .await
                .map_err(|_| ClientError::Unexpected("invalid hub HTTP response"))
        })
        .await
        .map_err(|_| ClientError::Unexpected("hub HTTP request task aborted"))?
}

/// Sign in with a password and receive the account's client-session token.
pub async fn login(
    hub_url: &str,
    username: &str,
    password: &str,
) -> Result<LoginResponse, ClientError> {
    send(post(hub_url, "login")?.json(&LoginRequest {
        username: username.into(),
        password: password.into(),
    }))
    .await
}

/// Mint a one-use code using a client-session or static account token.
pub async fn create_pairing_code(
    hub_url: &str,
    token: &str,
) -> Result<PairingCodeResponse, ClientError> {
    send(post(hub_url, "pairing-codes")?.bearer_auth(token)).await
}

/// Enroll one host by pairing code or password. Never reuse the returned token for another host.
pub async fn enroll(hub_url: &str, request: &EnrollRequest) -> Result<EnrollResponse, ClientError> {
    send(post(hub_url, "enroll")?.json(request)).await
}

pub(crate) async fn direct_session(
    hub_url: &str,
    token: &str,
    device_id: &str,
) -> Result<DirectTicketResponse, ClientError> {
    let request = post(hub_url, "direct-sessions")?
        .timeout(Duration::from_millis(750))
        .bearer_auth(token)
        .json(&DirectTicketRequest {
            device_id: device_id.into(),
        });
    send(request).await
}

/// Fetch the account's saved workspace layout. Hubs that predate layouts answer `404`, which
/// is reported as an empty default layout.
pub async fn get_layout(hub_url: &str, token: &str) -> Result<WorkspaceLayout, ClientError> {
    let request = client()?
        .get(endpoint(hub_url, "layout")?)
        .bearer_auth(token);
    match send(request).await {
        Err(ClientError::Hub { code, .. }) if code == "404" => Ok(WorkspaceLayout::default()),
        other => other,
    }
}

/// Replace the account's workspace layout and return the layout the hub saved.
pub async fn put_layout(
    hub_url: &str,
    token: &str,
    layout: &WorkspaceLayout,
) -> Result<WorkspaceLayout, ClientError> {
    let request = client()?
        .put(endpoint(hub_url, "layout")?)
        .bearer_auth(token)
        .json(layout);
    send(request).await
}

/// Forget an offline device and its saved record. The hub answers `409` while it is online.
pub async fn delete_device(hub_url: &str, token: &str, device_id: &str) -> Result<(), ClientError> {
    let mut url =
        reqwest::Url::parse(&endpoint(hub_url, "devices")?).map_err(|_| ClientError::InvalidUrl)?;
    url.path_segments_mut()
        .map_err(|_| ClientError::InvalidUrl)?
        .push(device_id);
    let request = client()?.delete(url).bearer_auth(token);
    http_runtime()?
        .spawn(async move { dispatch(request).await.map(drop) })
        .await
        .map_err(|_| ClientError::Unexpected("hub HTTP request task aborted"))?
}

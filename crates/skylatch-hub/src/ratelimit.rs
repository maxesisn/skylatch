//! Per-client-IP fixed-window limits for credential endpoints.

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::HeaderMap;
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);
const MAX_TRACKED: usize = 65_536;

pub(crate) struct IpLimiter {
    limit: u32,
    windows: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

impl IpLimiter {
    pub(crate) fn new(limit: u32) -> Self {
        Self {
            limit,
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn allow(&self, ip: IpAddr) -> bool {
        self.allow_at(ip, Instant::now())
    }

    fn allow_at(&self, ip: IpAddr, now: Instant) -> bool {
        let mut windows = self.windows.lock().unwrap_or_else(|p| p.into_inner());
        if windows.len() >= MAX_TRACKED {
            windows.retain(|_, (start, _)| now.duration_since(*start) < WINDOW);
        }
        if windows.len() >= MAX_TRACKED && !windows.contains_key(&ip) {
            return false;
        }
        let (start, count) = windows.entry(ip).or_insert((now, 0));
        if now.duration_since(*start) >= WINDOW {
            *start = now;
            *count = 0;
        }
        *count += 1;
        *count <= self.limit
    }
}

/// Limits for `login`, `signup` and `enroll`.
pub(crate) struct Limits {
    pub(crate) login: IpLimiter,
    pub(crate) signup: IpLimiter,
    pub(crate) enroll: IpLimiter,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            login: IpLimiter::new(30),
            signup: IpLimiter::new(10),
            enroll: IpLimiter::new(30),
        }
    }
}

fn forwarded(headers: &HeaderMap) -> Option<IpAddr> {
    let value = headers
        .get("x-forwarded-for")
        .or(headers.get("x-real-ip"))?;
    value.to_str().ok()?.rsplit(',').next()?.trim().parse().ok()
}

/// The TCP peer, or the proxy-reported client when the peer is a loopback reverse proxy.
pub(crate) fn client_ip(parts: &Parts) -> IpAddr {
    let peer = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    match peer {
        Some(ip) if ip.is_loopback() => forwarded(&parts.headers).unwrap_or(ip),
        Some(ip) => ip,
        None => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
    }
}

pub(crate) struct ClientIp(pub(crate) IpAddr);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        Ok(Self(client_ip(parts)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    #[test]
    fn fixed_window_resets() {
        let limiter = IpLimiter::new(2);
        let a = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1));
        let b = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2));
        let t = Instant::now();
        assert!(limiter.allow_at(a, t));
        assert!(limiter.allow_at(a, t));
        assert!(!limiter.allow_at(a, t + Duration::from_secs(59)));
        assert!(limiter.allow_at(b, t));
        assert!(limiter.allow_at(a, t + WINDOW));
    }

    #[test]
    fn forwarded_header_only_trusted_from_loopback() {
        let parts = |peer: &str| {
            let mut request = Request::builder()
                .header("x-forwarded-for", "198.51.100.7, 203.0.113.9")
                .body(())
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
            request.into_parts().0
        };
        assert_eq!(
            client_ip(&parts("127.0.0.1:5000")),
            "203.0.113.9".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(&parts("192.0.2.5:5000")),
            "192.0.2.5".parse::<IpAddr>().unwrap()
        );
    }
}

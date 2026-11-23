use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::Mutex;
use std::time::Instant;

use axum::extract::connect_info::{ConnectInfo, MockConnectInfo};
use axum::extract::{Request, State};
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::config::Cidr;
use crate::util::AppError;
use crate::St;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    Web,
    Auth,
    DeviceStart,
    DevicePoll,
    Events,
    Heartbeats,
    Api,
    Program,
    Export,
    Rewind,
}

impl Bucket {
    // Web, Auth and DeviceStart are per IP, and a club meeting or a hackathon puts dozens of people behind one address.
    fn quota(self) -> (f64, f64) {
        match self {
            Bucket::Web => (0.1, 300.0),
            Bucket::Auth => (0.5, 160.0),
            Bucket::DeviceStart => (1.0, 100.0),
            Bucket::DevicePoll => (1.0, 30.0),
            Bucket::Events => (0.5, 60.0),
            Bucket::Heartbeats => (0.1, 200.0),
            Bucket::Api => (0.5, 60.0),
            Bucket::Program => (0.05, 400.0),
            Bucket::Export => (600.0, 3.0),
            Bucket::Rewind => (6.0 * 3600.0, 3.0),
        }
    }
}

/// Lost on restart, which only ever makes it more lenient.
pub struct Limiter {
    epoch: Instant,
    tat: Mutex<HashMap<(Bucket, String), f64>>,
}

impl Default for Limiter {
    fn default() -> Self {
        Limiter { epoch: Instant::now(), tat: Mutex::new(HashMap::new()) }
    }
}

impl Limiter {
    pub fn check(&self, bucket: Bucket, key: &str) -> Result<(), u64> {
        let (interval, burst) = bucket.quota();
        let tolerance = interval * (burst - 1.0);
        let now = self.epoch.elapsed().as_secs_f64();
        let mut map = self.tat.lock().unwrap_or_else(|p| p.into_inner());
        let tat = map.get(&(bucket, key.to_string())).copied().unwrap_or(now).max(now);
        if tat - tolerance > now {
            return Err((tat - tolerance - now).ceil().max(1.0) as u64);
        }
        map.insert((bucket, key.to_string()), tat + interval);
        Ok(())
    }

    pub fn cleanup(&self) {
        let now = self.epoch.elapsed().as_secs_f64();
        self.tat.lock().unwrap_or_else(|p| p.into_inner()).retain(|_, tat| *tat > now);
    }

    pub fn len(&self) -> usize {
        self.tat.lock().unwrap_or_else(|p| p.into_inner()).len()
    }
}

pub fn too_many(secs: u64) -> AppError {
    let mut e = crate::util::err(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    e.retry_after = Some(secs);
    e
}

pub fn check(st: &St, bucket: Bucket, key: &str) -> Result<(), AppError> {
    st.limits.check(bucket, key).map_err(too_many)
}

/// The peer address, or the last X-Forwarded-For hop when the peer is a trusted proxy.
pub fn client_ip(ext: &Extensions, headers: &HeaderMap, trusted: &[Cidr]) -> IpAddr {
    let peer = ext
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .or_else(|| ext.get::<MockConnectInfo<SocketAddr>>().map(|c| c.0.ip()))
        .unwrap_or(IpAddr::V6(Ipv6Addr::UNSPECIFIED));
    if !trusted.iter().any(|c| c.contains(peer)) {
        return peer;
    }
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .last()
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .unwrap_or(peer)
}

/// IPv6 clients usually own a whole /64, so they share one key.
pub fn ip_key(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => {
                let s = v6.segments();
                format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
            }
        },
    }
}

pub async fn by_ip(State((st, bucket)): State<(St, Bucket)>, req: Request, next: Next) -> Response {
    let ip = client_ip(req.extensions(), req.headers(), &st.config.trusted_proxies);
    if let Err(secs) = st.limits.check(bucket, &ip_key(ip)) {
        return too_many(secs).into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn burst_then_wait() {
        let l = Limiter::default();
        for _ in 0..3 {
            assert!(l.check(Bucket::Export, "u1").is_ok());
        }
        let wait = l.check(Bucket::Export, "u1").unwrap_err();
        assert!(wait > 500 && wait <= 600, "{wait}");
        assert!(l.check(Bucket::Export, "u2").is_ok());
    }

    #[test]
    fn forwarded_only_from_trusted_peers() {
        let mut ext = Extensions::new();
        ext.insert(ConnectInfo("172.18.0.1:5000".parse::<SocketAddr>().unwrap()));
        let mut h = HeaderMap::new();
        h.insert("x-forwarded-for", "6.6.6.6, 1.2.3.4".parse().unwrap());
        assert_eq!(client_ip(&ext, &h, &[]).to_string(), "172.18.0.1");
        let trusted = [Cidr::parse("172.16.0.0/12").unwrap()];
        assert_eq!(client_ip(&ext, &h, &trusted).to_string(), "1.2.3.4");
        assert_eq!(ip_key("2001:db8:1:2:3:4:5:6".parse().unwrap()), "2001:db8:1:2::/64");
    }
}

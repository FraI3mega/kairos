use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use linux::listener_pid as scan;
#[cfg(target_os = "macos")]
use macos::listener_pid as scan;
#[cfg(windows)]
use windows::listener_pid as scan;

const TTL: Duration = Duration::from_secs(5);
const MAX_CACHED: usize = 32;

static CACHE: Mutex<Vec<(u16, Option<u32>, Instant)>> = Mutex::new(Vec::new());

pub fn listener_pid(port: u16) -> Option<u32> {
    if let Ok(cache) = CACHE.lock() {
        if let Some((_, pid, _)) = cache.iter().find(|(p, _, at)| *p == port && at.elapsed() < TTL) {
            return *pid;
        }
    }
    let pid = scan(port);
    if let Ok(mut cache) = CACHE.lock() {
        cache.retain(|(p, _, at)| *p != port && at.elapsed() < TTL);
        if cache.len() >= MAX_CACHED {
            cache.clear();
        }
        cache.push((port, pid, Instant::now()));
    }
    pid
}

fn reachable_locally(addr: IpAddr) -> bool {
    let addr = match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        v4 => v4,
    };
    addr.is_loopback() || addr.is_unspecified()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_and_wildcard_binds_are_reachable() {
        let ok = |s: &str| reachable_locally(s.parse().unwrap());
        assert!(ok("127.0.0.1"));
        assert!(ok("127.1.2.3"));
        assert!(ok("0.0.0.0"));
        assert!(ok("::1"));
        assert!(ok("::"));
        assert!(ok("::ffff:127.0.0.1"));
        assert!(!ok("192.168.1.20"));
        assert!(!ok("fe80::1"));
        assert!(!ok("::ffff:10.0.0.1"));
    }
}

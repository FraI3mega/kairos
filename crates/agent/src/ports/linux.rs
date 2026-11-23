use std::collections::HashSet;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::MetadataExt;

use super::reachable_locally;

const LISTEN: &str = "0A";

pub fn listener_pid(port: u16) -> Option<u32> {
    let mut inodes = HashSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        if let Ok(text) = fs::read_to_string(table) {
            inodes.extend(listening_inodes(&text, port));
        }
    }
    if inodes.is_empty() {
        return None;
    }
    let me = fs::metadata("/proc/self").ok()?.uid();
    let mut pids: Vec<u32> = fs::read_dir("/proc")
        .ok()?
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect();
    pids.sort_unstable();
    pids.into_iter().find(|pid| owned_by(*pid, me) && holds_socket(*pid, &inodes))
}

fn owned_by(pid: u32, uid: u32) -> bool {
    fs::metadata(format!("/proc/{pid}")).is_ok_and(|m| m.uid() == uid)
}

fn holds_socket(pid: u32, inodes: &HashSet<u64>) -> bool {
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else { return false };
    fds.flatten().any(|fd| {
        let Ok(target) = fs::read_link(fd.path()) else { return false };
        target
            .to_str()
            .and_then(|t| t.strip_prefix("socket:[")?.strip_suffix(']')?.parse::<u64>().ok())
            .is_some_and(|inode| inodes.contains(&inode))
    })
}

fn listening_inodes(table: &str, port: u16) -> Vec<u64> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (addr, hex_port) = f.get(1)?.split_once(':')?;
            if *f.get(3)? != LISTEN || u16::from_str_radix(hex_port, 16).ok()? != port {
                return None;
            }
            reachable_locally(parse_addr(addr)?).then_some(())?;
            f.get(9)?.parse().ok()
        })
        .collect()
}

// The kernel prints each 32-bit word of the address in native byte order.
fn parse_addr(hex: &str) -> Option<IpAddr> {
    let word = |s: &str| u32::from_str_radix(s, 16).ok().map(u32::to_ne_bytes);
    match hex.len() {
        8 => Some(IpAddr::V4(Ipv4Addr::from(word(hex)?))),
        32 => {
            let mut bytes = [0u8; 16];
            for (i, chunk) in bytes.chunks_exact_mut(4).enumerate() {
                chunk.copy_from_slice(&word(hex.get(i * 8..i * 8 + 8)?)?);
            }
            Some(IpAddr::V6(Ipv6Addr::from(bytes)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_endian = "little")]
    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 11111 1 0000000000000000 100 0 0 10 0
   1: 00000000:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 22222 1 0000000000000000 100 0 0 10 0
   2: 0100007F:0BB8 0100007F:D2F0 01 00000000:00000000 00:00000000 00000000  1000        0 33333 1 0000000000000000 100 0 0 10 0
   3: 1400A8C0:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 44444 1 0000000000000000 100 0 0 10 0
";

    #[cfg(target_endian = "little")]
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:0BB8 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 55555 1 0000000000000000 100 0 0 10 0
   1: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 66666 1 0000000000000000 100 0 0 10 0
   2: 0000000000000000FFFF00000100007F:1F91 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 77777 1 0000000000000000 100 0 0 10 0
";

    #[cfg(target_endian = "little")]
    #[test]
    fn picks_listening_loopback_and_wildcard_sockets() {
        assert_eq!(listening_inodes(TCP, 3000), vec![11111]);
        assert_eq!(listening_inodes(TCP, 8080), vec![22222]);
        assert_eq!(listening_inodes(TCP6, 3000), vec![55555]);
        assert_eq!(listening_inodes(TCP6, 8080), vec![66666]);
        assert_eq!(listening_inodes(TCP6, 8081), vec![77777]);
        assert!(listening_inodes(TCP, 9999).is_empty());
    }

    #[test]
    fn garbage_lines_are_ignored() {
        assert!(listening_inodes("header\nnot a table row\n   0: zz:zz\n", 80).is_empty());
    }
}

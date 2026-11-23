use std::mem::{size_of, zeroed};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ptr::null_mut;

use libc::{c_int, c_void, proc_bsdinfo, proc_fdinfo, proc_listallpids, proc_pidfdinfo, proc_pidinfo};
use libc::{PROC_PIDLISTFDS, PROC_PIDTBSDINFO, PROX_FDTYPE_SOCKET};

use super::reachable_locally;

// libc has no binding for `struct socket_fdinfo` (sys/proc_info.h), so the few fields needed are
// read from the raw 792-byte reply at their documented offsets: proc_fileinfo (24 bytes), then
// socket_info whose soi_kind sits at 232 and soi_proto at 240, then in_sockinfo (80 bytes) at the
// start of tcp_sockinfo, followed by tcpsi_state.
const PROC_PIDFDSOCKETINFO: c_int = 3;
const SOCKET_FDINFO_SIZE: usize = 792;
const SOCKINFO_TCP: i32 = 2;
const TSI_S_LISTEN: i32 = 1;
const INI_IPV4: u8 = 0x1;
const INI_IPV6: u8 = 0x2;
const KIND: usize = 24 + 232;
const IN_SOCKINFO: usize = 24 + 240;
const LPORT: usize = IN_SOCKINFO + 4;
const VFLAG: usize = IN_SOCKINFO + 24;
const LADDR: usize = IN_SOCKINFO + 48;
const TCP_STATE: usize = IN_SOCKINFO + 80;

pub fn listener_pid(port: u16) -> Option<u32> {
    // SAFETY: getuid has no preconditions.
    let me = unsafe { libc::getuid() };
    all_pids()
        .into_iter()
        .find(|pid| owned_by(*pid, me) && listens_on(*pid, port))
        .and_then(|pid| u32::try_from(pid).ok())
}

fn all_pids() -> Vec<c_int> {
    // SAFETY: a null buffer asks libproc for the process count.
    let count = unsafe { proc_listallpids(null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0 as c_int; count as usize + 64];
    let bytes = (pids.len() * size_of::<c_int>()) as c_int;
    // SAFETY: the buffer is `bytes` long and exclusively ours.
    let got = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(got.max(0) as usize);
    pids.retain(|p| *p > 0);
    pids
}

fn owned_by(pid: c_int, uid: libc::uid_t) -> bool {
    // SAFETY: proc_bsdinfo is plain old data, so all-zero is a valid value.
    let mut info: proc_bsdinfo = unsafe { zeroed() };
    let size = size_of::<proc_bsdinfo>() as c_int;
    // SAFETY: the buffer is exactly `size` bytes.
    let got = unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, (&mut info as *mut proc_bsdinfo).cast::<c_void>(), size) };
    got == size && info.pbi_uid == uid
}

fn listens_on(pid: c_int, port: u16) -> bool {
    let entry = size_of::<proc_fdinfo>();
    // SAFETY: a null buffer asks libproc for the size of the descriptor table.
    let needed = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, null_mut(), 0) };
    if needed <= 0 {
        return false;
    }
    let mut fds = vec![proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; needed as usize / entry + 16];
    let bytes = (fds.len() * entry) as c_int;
    // SAFETY: the buffer is `bytes` long and exclusively ours.
    let got = unsafe { proc_pidinfo(pid, PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), bytes) };
    fds.truncate(got.max(0) as usize / entry);
    fds.iter()
        .filter(|fd| fd.proc_fdtype == PROX_FDTYPE_SOCKET as u32)
        .any(|fd| tcp_listener_port(pid, fd.proc_fd) == Some(port))
}

fn tcp_listener_port(pid: c_int, fd: c_int) -> Option<u16> {
    // u64 words keep the buffer aligned for the kernel's 8-byte fields.
    let mut buf = [0u64; SOCKET_FDINFO_SIZE / 8];
    let bytes = (buf.len() * 8) as c_int;
    // SAFETY: the buffer is `bytes` long and exclusively ours.
    let got = unsafe { proc_pidfdinfo(pid, fd, PROC_PIDFDSOCKETINFO, buf.as_mut_ptr().cast(), bytes) };
    if got < bytes {
        return None;
    }
    // SAFETY: reinterpreting initialized u64s as bytes of the same length.
    let raw = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len() * 8) };
    listening_port(raw)
}

fn listening_port(raw: &[u8]) -> Option<u16> {
    let int = |at: usize| raw.get(at..at + 4).and_then(|b| b.try_into().ok()).map(i32::from_ne_bytes);
    if int(KIND)? != SOCKINFO_TCP || int(TCP_STATE)? != TSI_S_LISTEN {
        return None;
    }
    let vflag = *raw.get(VFLAG)?;
    let addr = if vflag & INI_IPV6 != 0 {
        IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(raw.get(LADDR..LADDR + 16)?).ok()?))
    } else if vflag & INI_IPV4 != 0 {
        IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(raw.get(LADDR + 12..LADDR + 16)?).ok()?))
    } else {
        return None;
    };
    // The port is stored in network byte order inside an int.
    let port = u16::from_be_bytes([*raw.get(LPORT)?, *raw.get(LPORT + 1)?]);
    reachable_locally(addr).then_some(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(vflag: u8, addr: &[u8], port: u16, state: i32) -> Vec<u8> {
        let mut raw = vec![0u8; SOCKET_FDINFO_SIZE];
        raw[KIND..KIND + 4].copy_from_slice(&SOCKINFO_TCP.to_ne_bytes());
        raw[TCP_STATE..TCP_STATE + 4].copy_from_slice(&state.to_ne_bytes());
        raw[VFLAG] = vflag;
        raw[LPORT..LPORT + 2].copy_from_slice(&port.to_be_bytes());
        if vflag == INI_IPV4 {
            raw[LADDR + 12..LADDR + 16].copy_from_slice(addr);
        } else {
            raw[LADDR..LADDR + 16].copy_from_slice(addr);
        }
        raw
    }

    #[test]
    fn reads_listening_tcp_sockets() {
        assert_eq!(listening_port(&reply(INI_IPV4, &[127, 0, 0, 1], 3000, TSI_S_LISTEN)), Some(3000));
        assert_eq!(listening_port(&reply(INI_IPV4, &[0, 0, 0, 0], 8080, TSI_S_LISTEN)), Some(8080));
        assert_eq!(listening_port(&reply(INI_IPV6, &[0; 16], 5173, TSI_S_LISTEN)), Some(5173));
        assert_eq!(listening_port(&reply(INI_IPV4, &[192, 168, 1, 5], 3000, TSI_S_LISTEN)), None);
        assert_eq!(listening_port(&reply(INI_IPV4, &[127, 0, 0, 1], 3000, 4)), None);
        assert_eq!(listening_port(&[0u8; 10]), None);
    }

    #[test]
    fn finds_own_listeners_through_the_kernel() {
        let me = std::process::id();
        let v4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        assert_eq!(listener_pid(v4.local_addr().unwrap().port()), Some(me));
        let v6 = std::net::TcpListener::bind("[::1]:0").unwrap();
        assert_eq!(listener_pid(v6.local_addr().unwrap().port()), Some(me));
        let outside = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = outside.local_addr().unwrap().port();
        drop(outside);
        assert_eq!(listener_pid(port), None);
    }
}

use std::mem::size_of;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, NO_ERROR};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use windows_sys::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use windows_sys::Win32::Security::{EqualSid, GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::reachable_locally;

pub fn listener_pid(port: u16) -> Option<u32> {
    let mut owners = listeners_v4(port);
    owners.extend(listeners_v6(port));
    owners.sort_unstable();
    owners.dedup();
    // SAFETY: GetCurrentProcess returns a pseudo handle that needs no cleanup.
    let mine = user_sid(unsafe { GetCurrentProcess() })?;
    owners.into_iter().find(|pid| same_user(*pid, &mine))
}

fn listeners_v4(port: u16) -> Vec<u32> {
    let Some(buf) = tcp_table(AF_INET as u32) else { return Vec::new() };
    // SAFETY: the buffer holds a MIB_TCPTABLE_OWNER_PID filled in by GetExtendedTcpTable.
    let rows: &[MIB_TCPROW_OWNER_PID] = unsafe {
        let table = &*buf.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
        rows(&buf, table.table.as_ptr(), table.dwNumEntries)
    };
    rows.iter()
        .filter(|r| u16::from_be(r.dwLocalPort as u16) == port)
        .filter(|r| reachable_locally(IpAddr::V4(Ipv4Addr::from(r.dwLocalAddr.to_ne_bytes()))))
        .map(|r| r.dwOwningPid)
        .collect()
}

fn listeners_v6(port: u16) -> Vec<u32> {
    let Some(buf) = tcp_table(AF_INET6 as u32) else { return Vec::new() };
    // SAFETY: the buffer holds a MIB_TCP6TABLE_OWNER_PID filled in by GetExtendedTcpTable.
    let rows: &[MIB_TCP6ROW_OWNER_PID] = unsafe {
        let table = &*buf.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
        rows(&buf, table.table.as_ptr(), table.dwNumEntries)
    };
    rows.iter()
        .filter(|r| u16::from_be(r.dwLocalPort as u16) == port)
        .filter(|r| reachable_locally(IpAddr::V6(Ipv6Addr::from(r.ucLocalAddr))))
        .map(|r| r.dwOwningPid)
        .collect()
}

/// SAFETY: `first` must point into `buf` at the first row of a table with `count` claimed rows.
unsafe fn rows<'a, T>(buf: &'a [u64], first: *const T, count: u32) -> &'a [T] {
    let start = first as usize - buf.as_ptr() as usize;
    let room = (buf.len() * 8).saturating_sub(start) / size_of::<T>();
    std::slice::from_raw_parts(first, (count as usize).min(room))
}

// u64 words keep the buffer aligned for the table structs.
fn tcp_table(family: u32) -> Option<Vec<u64>> {
    let mut size = 0u32;
    for _ in 0..4 {
        let mut buf = vec![0u64; (size as usize).div_ceil(8).max(8)];
        // SAFETY: `buf` is at least `size` bytes; a too-small buffer is reported through `size`.
        let status = unsafe {
            GetExtendedTcpTable(buf.as_mut_ptr().cast(), &mut size, 0, family, TCP_TABLE_OWNER_PID_LISTENER, 0)
        };
        match status {
            NO_ERROR => return Some(buf),
            ERROR_INSUFFICIENT_BUFFER => continue,
            _ => return None,
        }
    }
    None
}

struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by the OS and is closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn user_sid(process: HANDLE) -> Option<Vec<u64>> {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` receives a handle we close in `Handle::drop`.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return None;
    }
    let token = Handle(token);
    let mut len = 0u32;
    // SAFETY: a null buffer asks for the required length.
    unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer is at least `len` bytes.
    let ok = unsafe { GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) };
    (ok != 0).then_some(buf)
}

fn same_user(pid: u32, mine: &[u64]) -> bool {
    // SAFETY: plain call, the handle is closed by `Handle::drop`.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let process = Handle(process);
    let Some(theirs) = user_sid(process.0) else { return false };
    let sid = |buf: &[u64]| {
        // SAFETY: the buffer was filled with a TOKEN_USER by GetTokenInformation.
        unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    };
    // SAFETY: both SIDs point into live buffers.
    unsafe { EqualSid(sid(mine), sid(&theirs)) != 0 }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    #[test]
    fn finds_own_listeners_through_the_kernel() {
        let me = std::process::id();
        for addr in ["127.0.0.1:0", "0.0.0.0:0", "[::1]:0", "[::]:0"] {
            let l = TcpListener::bind(addr).unwrap();
            assert_eq!(listener_pid(l.local_addr().unwrap().port()), Some(me), "{addr}");
        }
        let gone = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = gone.local_addr().unwrap().port();
        drop(gone);
        assert_eq!(listener_pid(port), None);
    }
}

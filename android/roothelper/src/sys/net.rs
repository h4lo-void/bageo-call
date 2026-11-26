// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The connection to the app: an abstract-namespace Unix socket that the app listens on
//! and the helper connects to. The helper runs in the root manager's SELinux domain,
//! which may connect anywhere, so the phone needs no new SELinux rule; the app only
//! listens on a socket of its own. Before it sends or reads anything the helper checks
//! the listener's user ID (`SO_PEERCRED`) and SELinux context (`SO_PEERSEC`).
//!
//! The connect does not wait: a listener whose queue is full (or that never accepts)
//! is treated as not there. Reads and writes on the connection have time limits, set
//! before each read or write from a deadline on the monotonic clock (see
//! [`crate::proto::read_frame_by`]).

use std::io;
use std::mem::{offset_of, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::proto::Conn;

/// Most time a write to the app may block.
pub const WRITE_TIME: Duration = Duration::from_secs(5);

pub fn connect_abstract(name: &str) -> io::Result<UnixStream> {
    // SAFETY: a plain socket call; the descriptor is owned right after.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just created and belongs to nobody else.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: sockaddr_un is plain data.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let name = name.as_bytes();
    if name.is_empty() || name.len() + 1 > addr.sun_path.len() {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    // sun_path[0] stays 0: the abstract namespace.
    for (i, &b) in name.iter().enumerate() {
        addr.sun_path[i + 1] = b as libc::c_char;
    }
    let len = offset_of!(libc::sockaddr_un, sun_path) + 1 + name.len();
    // SAFETY: `addr` is a valid sockaddr_un and `len` lies within it. Without blocking:
    // a full queue gives EAGAIN at once instead of a wait.
    let r = unsafe {
        libc::connect(fd.as_raw_fd(), &addr as *const libc::sockaddr_un as *const libc::sockaddr, len as libc::socklen_t)
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    let s = UnixStream::from(fd);
    s.set_nonblocking(false)?;
    s.set_write_timeout(Some(WRITE_TIME))?;
    Ok(s)
}

/// The user ID of the process at the other end (for a connection the helper made: the
/// process that listens).
pub fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    // SAFETY: ucred is plain data.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` describe a writable ucred.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    if r < 0 || len as usize != size_of::<libc::ucred>() {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

/// The SELinux context of the process at the other end.
pub fn peer_context(s: &UnixStream) -> io::Result<String> {
    let mut buf = [0u8; 256];
    let mut len = buf.len() as libc::socklen_t;
    // SAFETY: `buf` and `len` describe a writable buffer.
    let r = unsafe {
        libc::getsockopt(s.as_raw_fd(), libc::SOL_SOCKET, libc::SO_PEERSEC, buf.as_mut_ptr().cast(), &mut len)
    };
    if r < 0 || len as usize > buf.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf8_lossy(&buf[..len as usize]).trim_end_matches('\0').to_string())
}

/// Whether `s` has data (or an end) to read within `timeout`.
pub fn wait_readable(s: &UnixStream, timeout: Duration) -> io::Result<bool> {
    let mut p = libc::pollfd { fd: s.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    loop {
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut p, 1, ms) };
        if r >= 0 {
            return Ok(r > 0);
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// A connection to the app with time-limited reads.
pub struct AppConn(pub UnixStream);

impl io::Read for AppConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl io::Write for AppConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Conn for AppConn {
    fn set_read_limit(&mut self, limit: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(limit.map(|d| d.max(Duration::from_millis(1))))
    }
    fn set_write_limit(&mut self, limit: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(limit.map(|d| d.max(Duration::from_millis(1))))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixListener};

    #[test]
    fn connects_to_an_abstract_listener_and_learns_its_user() {
        let name = format!("bageo-helper-test-{}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&addr).unwrap();
        let client = connect_abstract(&name).unwrap();
        let (mut server, _) = listener.accept().unwrap();
        // SAFETY: getuid has no preconditions.
        assert_eq!(peer_uid(&client).unwrap(), unsafe { libc::getuid() });
        assert!(!wait_readable(&client, Duration::from_millis(10)).unwrap());
        server.write_all(b"x").unwrap();
        assert!(wait_readable(&client, Duration::from_millis(1000)).unwrap());
        assert!(connect_abstract(&format!("{name}-nobody")).is_err());
        assert!(connect_abstract("").is_err());
    }

    #[test]
    fn a_listener_that_never_accepts_does_not_hold_the_helper() {
        let name = format!("bageo-helper-full-{}", std::process::id());
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let listener = UnixListener::bind_addr(&addr).unwrap();
        // A queue of one that nobody accepts from (std listens with a long queue).
        // SAFETY: listen on a socket we own.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 1) }, 0);
        // Fill it; each connect returns at once, and soon fails.
        let start = std::time::Instant::now();
        let mut held = Vec::new();
        let mut refused = false;
        for _ in 0..64 {
            match connect_abstract(&name) {
                Ok(s) => held.push(s),
                Err(_) => {
                    refused = true;
                    break;
                }
            }
        }
        assert!(refused, "a full queue is refused instead of waited on");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn reads_wait_no_longer_than_allowed() {
        let (a, _b) = UnixStream::pair().unwrap();
        let mut c = AppConn(a);
        c.set_read_limit(Some(Duration::from_millis(50))).unwrap();
        let start = std::time::Instant::now();
        let mut buf = [0u8; 1];
        let e = c.read(&mut buf).unwrap_err();
        assert!(matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}

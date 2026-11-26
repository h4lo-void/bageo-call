// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The connection to the app: an abstract-namespace Unix socket that the app listens on
//! and the helper connects to. The helper runs in the root manager's SELinux domain,
//! which may connect anywhere, so the phone needs no new SELinux rule; the app only
//! listens on a socket of its own. The helper checks the listener's user ID
//! (`SO_PEERCRED`) before it sends or accepts anything.

use std::io;
use std::mem::{offset_of, size_of};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

pub fn connect_abstract(name: &str) -> io::Result<UnixStream> {
    // SAFETY: a plain socket call; the descriptor is owned right after.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
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
    // SAFETY: `addr` is a valid sockaddr_un and `len` lies within it.
    let r = unsafe {
        libc::connect(fd.as_raw_fd(), &addr as *const libc::sockaddr_un as *const libc::sockaddr, len as libc::socklen_t)
    };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UnixStream::from(fd))
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::Write;
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
}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Writing into the app's own folder as root, without being sent elsewhere. The app
//! owns `files/` and `files/mode/` and can replace them with links at any moment, so the
//! folder is opened one step at a time from a fixed start, never following a link
//! (`O_NOFOLLOW`), each step checked to belong to the app; files are then made and
//! renamed relative to that open folder, which later renames of the path cannot move.

use std::ffi::CString;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;

pub struct AppDir {
    fd: OwnedFd,
    uid: u32,
    gid: u32,
    label: Option<Vec<u8>>,
}

fn cstr(s: &str) -> io::Result<CString> {
    if s.is_empty() || s.contains('/') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    CString::new(s).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn fstat(fd: RawFd) -> io::Result<libc::stat> {
    // SAFETY: stat is plain data; fd is open.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn open_step(parent: Option<RawFd>, name: &str, uid: u32) -> io::Result<OwnedFd> {
    let c = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: NUL-terminated name; the descriptor is owned right after.
    let fd = unsafe {
        match parent {
            Some(p) => libc::openat(p, c.as_ptr(), DIR_FLAGS),
            None => libc::open(c.as_ptr(), DIR_FLAGS),
        }
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: just opened.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    if fstat(fd.as_raw_fd())?.st_uid != uid {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(fd)
}

impl AppDir {
    /// Opens `<base>/<steps...>`: `base` is a path whose parents only root controls (it
    /// is opened as a whole, but not through a final link); it and every step must
    /// belong to `uid`.
    pub fn open(base: &Path, steps: &[&str], uid: u32) -> io::Result<AppDir> {
        let base = base.to_str().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut fd = open_step(None, base, uid)?;
        for s in steps {
            cstr(s)?;
            fd = open_step(Some(fd.as_raw_fd()), s, uid)?;
        }
        let st = fstat(fd.as_raw_fd())?;
        let label = get_label(fd.as_raw_fd());
        Ok(AppDir { fd, uid, gid: st.st_gid, label })
    }

    /// Writes `name` for the app: made new next to it (never through a link), owned by
    /// the app, mode 0600, with the folder's SELinux label, flushed, then renamed into
    /// place (a link already there is replaced, not followed).
    pub fn write_file(&self, name: &str, data: &[u8]) -> io::Result<()> {
        let dir = self.fd.as_raw_fd();
        let target = cstr(name)?;
        let tmp = cstr(&format!(".{name}.tmp"))?;
        // SAFETY: plain *at calls on our open folder with NUL-terminated names.
        unsafe {
            if libc::unlinkat(dir, tmp.as_ptr(), 0) != 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
            let fd = libc::openat(
                dir,
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut f = std::fs::File::from(fd);
            let result = (|| -> io::Result<()> {
                if libc::fchown(f.as_raw_fd(), self.uid, self.gid) != 0 || libc::fchmod(f.as_raw_fd(), 0o600) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if let Some(label) = &self.label {
                    set_label(f.as_raw_fd(), label)?;
                }
                f.write_all(data)?;
                f.sync_all()
            })();
            if result.is_ok() && libc::renameat(dir, tmp.as_ptr(), dir, target.as_ptr()) == 0 {
                return Ok(());
            }
            let e = result.err().unwrap_or_else(io::Error::last_os_error);
            libc::unlinkat(dir, tmp.as_ptr(), 0);
            Err(e)
        }
    }

    /// Removes the file `name` (only that name; a folder there stays).
    pub fn remove_file(&self, name: &str) {
        if let Ok(c) = cstr(name) {
            // SAFETY: unlinkat without AT_REMOVEDIR on our open folder.
            unsafe {
                libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0);
            }
        }
    }
}

const LABEL_ATTR: &std::ffi::CStr = c"security.selinux";

fn get_label(fd: RawFd) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    // SAFETY: the buffer has the length given.
    let n = unsafe { libc::fgetxattr(fd, LABEL_ATTR.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

fn set_label(fd: RawFd, label: &[u8]) -> io::Result<()> {
    // SAFETY: the label bytes live through the call.
    if unsafe { libc::fsetxattr(fd, LABEL_ATTR.as_ptr(), label.as_ptr().cast(), label.len(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn me() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::getuid() }
    }

    #[test]
    fn writes_only_inside_the_real_folder() {
        let base = std::env::temp_dir().join(format!("bageo-appdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("files/mode")).unwrap();
        std::fs::create_dir_all(base.join("elsewhere")).unwrap();

        let d = AppDir::open(&base, &["files", "mode"], me()).unwrap();
        // A link left where the file goes is replaced, and the link's target untouched.
        std::fs::write(base.join("elsewhere/target"), b"keep").unwrap();
        std::os::unix::fs::symlink(base.join("elsewhere/target"), base.join("files/mode/key")).unwrap();
        d.write_file("key", b"secret").unwrap();
        assert_eq!(std::fs::read(base.join("elsewhere/target")).unwrap(), b"keep");
        let meta = std::fs::symlink_metadata(base.join("files/mode/key")).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.uid(), me());
        assert_eq!(std::fs::read(base.join("files/mode/key")).unwrap(), b"secret");

        // The folder swapped for a link after opening: the write still lands in the
        // folder that was opened, not in the link's target.
        std::fs::rename(base.join("files/mode"), base.join("files/old")).unwrap();
        std::os::unix::fs::symlink(base.join("elsewhere"), base.join("files/mode")).unwrap();
        d.write_file("key2", b"x").unwrap();
        assert!(!base.join("elsewhere/key2").exists());
        assert!(base.join("files/old/key2").exists());

        // Opening through a link is refused.
        assert!(AppDir::open(&base, &["files", "mode"], me()).is_err());
        // A folder of another owner is refused.
        assert!(AppDir::open(&base, &["files", "old"], me() + 1).is_err());
        // A name with a slash is refused.
        assert!(d.write_file("../x", b"x").is_err());
        d.remove_file("key");
        assert!(!base.join("files/old/key").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}

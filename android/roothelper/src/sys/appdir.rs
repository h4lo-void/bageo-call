// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Writing into the app's own folder as root, without being sent elsewhere. The app
//! owns `files/` and `files/mode/` and can replace them with links at any moment, so the
//! folder is opened one step at a time from a fixed start, never following a link
//! (`O_NOFOLLOW`), each step checked to belong to the app; later renames of the path
//! cannot move that open folder.
//!
//! A file is published in three steps, none of which the app can redirect: it is made
//! in a folder only root can touch (on the same file system), given to the app (owner,
//! mode 0600, the app folder's SELinux label), written and flushed; renamed from there
//! into the app's folder; then the name in the app's folder is checked to be that very
//! file (device and inode). Without a label it can trust on the app's folder the helper
//! publishes nothing. Removing it later removes only that file, not something the app
//! put under the same name.

use std::ffi::CString;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct AppDir {
    fd: OwnedFd,
    uid: u32,
    gid: u32,
    label: Option<Vec<u8>>,
}

/// Whether the app's folder must carry an SELinux label the helper trusts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Labels {
    /// On a phone: no trusted label, no publishing.
    Required,
    /// Only for tests on a computer without SELinux.
    NotUsed,
}

/// Which file was published: compared before the file is removed. The file is kept
/// open, so its inode cannot be freed and its number given to a file the app makes
/// under the same name later.
pub struct Published {
    id: FileId,
    _held: std::fs::File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
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

/// The entry `name` in the folder `dir`, without following a link.
fn lstat_at(dir: RawFd, name: &CString) -> io::Result<libc::stat> {
    // SAFETY: stat is plain data; a NUL-terminated name relative to an open folder.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

// The field types differ between targets (32-bit Android among them).
#[allow(clippy::unnecessary_cast)]
fn is_file(st: &libc::stat) -> bool {
    (st.st_mode as u32) & (libc::S_IFMT as u32) == libc::S_IFREG as u32
}

#[allow(clippy::unnecessary_cast)]
fn file_id(st: &libc::stat) -> FileId {
    FileId { dev: st.st_dev as u64, ino: st.st_ino as u64 }
}

const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn open_dir(parent: Option<RawFd>, name: &str) -> io::Result<OwnedFd> {
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
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_step(parent: Option<RawFd>, name: &str, uid: u32) -> io::Result<OwnedFd> {
    let fd = open_dir(parent, name)?;
    if fstat(fd.as_raw_fd())?.st_uid != uid {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(fd)
}

/// Whether an SELinux label is one the helper gives to the app's key file: an app data
/// file type (`app_data_file`, `privapp_data_file` ...) with a level.
pub fn trusted_label(label: &[u8]) -> bool {
    let label = label.strip_suffix(b"\0").unwrap_or(label);
    let Ok(text) = std::str::from_utf8(label) else { return false };
    if text.is_empty() || text.len() > 255 || !text.bytes().all(|b| b.is_ascii_graphic()) {
        return false;
    }
    let mut parts = text.splitn(4, ':');
    let (Some("u"), Some("object_r"), Some(ty), Some(level)) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    ty.ends_with("app_data_file")
        && ty.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && level.starts_with("s0")
}

static NEXT_TMP: AtomicU32 = AtomicU32::new(0);

impl AppDir {
    /// Opens `<base>/<steps...>`: `base` is a path whose parents only root controls (it
    /// is opened as a whole, but not through a final link); it and every step must
    /// belong to `uid`. With [`Labels::Required`] the folder must carry a label
    /// [`trusted_label`] accepts.
    pub fn open(base: &Path, steps: &[&str], uid: u32, labels: Labels) -> io::Result<AppDir> {
        let base = base.to_str().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut fd = open_step(None, base, uid)?;
        for s in steps {
            cstr(s)?;
            fd = open_step(Some(fd.as_raw_fd()), s, uid)?;
        }
        let st = fstat(fd.as_raw_fd())?;
        let label = match labels {
            Labels::Required => {
                let l = get_label(fd.as_raw_fd()).filter(|l| trusted_label(l));
                Some(l.ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "no trusted SELinux label"))?)
            }
            Labels::NotUsed => None,
        };
        Ok(AppDir { fd, uid, gid: st.st_gid, label })
    }

    /// Publishes `data` as `name` for the app (see the top of this file). `stage`: a
    /// folder only root can touch, on the same file system as the app's folder.
    pub fn publish(&self, stage: &Path, name: &str, data: &[u8]) -> io::Result<Published> {
        self.publish_with(stage, name, data, || {})
    }

    /// [`AppDir::publish`], with `between` run after the rename and before the check
    /// (where the tests swap the file).
    fn publish_with(&self, stage: &Path, name: &str, data: &[u8], between: impl FnOnce()) -> io::Result<Published> {
        let target = cstr(name)?;
        let stage = stage.to_str().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let stage_fd = open_dir(None, stage)?;
        let sst = fstat(stage_fd.as_raw_fd())?;
        // SAFETY: geteuid has no preconditions.
        if sst.st_uid != unsafe { libc::geteuid() } || sst.st_mode & 0o077 != 0 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "the staging folder is not root's alone"));
        }
        let tmp = cstr(&format!(".{name}.{}.{}", std::process::id(), NEXT_TMP.fetch_add(1, Ordering::Relaxed)))?;
        let sdir = stage_fd.as_raw_fd();
        // SAFETY: plain *at calls on folders we hold open, with NUL-terminated names.
        unsafe {
            libc::unlinkat(sdir, tmp.as_ptr(), 0);
            let fd = libc::openat(
                sdir,
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut f = std::fs::File::from(OwnedFd::from_raw_fd(fd));
            let written = (|| -> io::Result<FileId> {
                if libc::fchown(f.as_raw_fd(), self.uid, self.gid) != 0 || libc::fchmod(f.as_raw_fd(), 0o600) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if let Some(label) = &self.label {
                    set_label(f.as_raw_fd(), label)?;
                }
                f.write_all(data)?;
                f.sync_all()?;
                Ok(file_id(&fstat(f.as_raw_fd())?))
            })();
            let id = match written {
                Ok(id) => id,
                Err(e) => {
                    libc::unlinkat(sdir, tmp.as_ptr(), 0);
                    return Err(e);
                }
            };
            if libc::renameat(sdir, tmp.as_ptr(), self.fd.as_raw_fd(), target.as_ptr()) != 0 {
                // Another file system (EXDEV) included: nothing is copied.
                let e = io::Error::last_os_error();
                libc::unlinkat(sdir, tmp.as_ptr(), 0);
                return Err(e);
            }
            between();
            match lstat_at(self.fd.as_raw_fd(), &target) {
                Ok(st) if is_file(&st) && file_id(&st) == id => Ok(Published { id, _held: f }),
                _ => Err(io::Error::other("the published file was replaced")),
            }
        }
    }

    /// Removes `name` only if it is still the file that was published.
    pub fn remove_published(&self, name: &str, p: &Published) -> bool {
        let Ok(c) = cstr(name) else { return false };
        match lstat_at(self.fd.as_raw_fd(), &c) {
            Ok(st) if is_file(&st) && file_id(&st) == p.id => {
                // SAFETY: unlinkat without AT_REMOVEDIR on our open folder.
                unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) == 0 }
            }
            _ => false,
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

#[cfg(test)]
mod label_tests {
    use super::trusted_label;

    #[test]
    fn only_app_data_labels_are_trusted() {
        assert!(trusted_label(b"u:object_r:privapp_data_file:s0:c512,c768\0"));
        assert!(trusted_label(b"u:object_r:app_data_file:s0:c10,c257,c512,c768"));
        for bad in [
            &b""[..],
            b"\0",
            b"u:object_r:system_file:s0",
            b"u:object_r:shell_data_file:s0",
            b"u:r:privapp_data_file:s0",
            b"u:object_r:privapp_data_file",
            b"u:object_r:privapp_data_file:",
            b"u:object_r:Privapp_data_file:s0",
            b"u:object_r:privapp_data_file:s0 x",
            b"u:object_r:privapp_data_file:s0\n",
        ] {
            assert!(!trusted_label(bad), "{:?}", String::from_utf8_lossy(bad));
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    fn me() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::getuid() }
    }

    struct Place {
        base: std::path::PathBuf,
        stage: std::path::PathBuf,
    }

    impl Place {
        fn new(tag: &str) -> Place {
            let base = std::env::temp_dir().join(format!("bageo-appdir-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(base.join("app/files/mode")).unwrap();
            std::fs::create_dir_all(base.join("elsewhere")).unwrap();
            let stage = base.join("root");
            std::fs::DirBuilder::new().mode(0o700).create(&stage).unwrap();
            Place { base, stage }
        }
        fn app(&self) -> AppDir {
            AppDir::open(&self.base.join("app"), &["files", "mode"], me(), Labels::NotUsed).unwrap()
        }
        fn mode(&self, name: &str) -> std::path::PathBuf {
            self.base.join("app/files/mode").join(name)
        }
    }

    impl Drop for Place {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn publishes_only_inside_the_real_folder() {
        let p = Place::new("inside");
        let d = p.app();
        // A link left where the file goes is replaced, and the link's target untouched.
        std::fs::write(p.base.join("elsewhere/target"), b"keep").unwrap();
        std::os::unix::fs::symlink(p.base.join("elsewhere/target"), p.mode("key")).unwrap();
        let k = d.publish(&p.stage, "key", b"secret").unwrap();
        assert_eq!(std::fs::read(p.base.join("elsewhere/target")).unwrap(), b"keep");
        let meta = std::fs::symlink_metadata(p.mode("key")).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.uid(), me());
        assert_eq!(std::fs::read(p.mode("key")).unwrap(), b"secret");
        assert_eq!(std::fs::read_dir(&p.stage).unwrap().count(), 0, "nothing left in the staging folder");

        // The folder swapped for a link after opening: the file still lands in the folder
        // that was opened, not in the link's target.
        std::fs::rename(p.base.join("app/files/mode"), p.base.join("app/files/old")).unwrap();
        std::os::unix::fs::symlink(p.base.join("elsewhere"), p.base.join("app/files/mode")).unwrap();
        d.publish(&p.stage, "key2", b"x").unwrap();
        assert!(!p.base.join("elsewhere/key2").exists());
        assert!(p.base.join("app/files/old/key2").exists());

        // Opening through a link is refused; so is a folder of another owner.
        assert!(AppDir::open(&p.base.join("app"), &["files", "mode"], me(), Labels::NotUsed).is_err());
        assert!(AppDir::open(&p.base.join("app"), &["files", "old"], me() + 1, Labels::NotUsed).is_err());
        assert!(d.publish(&p.stage, "../x", b"x").is_err());
        assert!(d.remove_published("key", &k));
        assert!(!p.base.join("app/files/old/key").exists());
    }

    #[test]
    fn a_file_swapped_in_after_the_rename_is_noticed_and_left_alone() {
        let p = Place::new("swap");
        let d = p.app();
        let mode = p.mode("");
        let r = d.publish_with(&p.stage, "key", b"secret", || {
            std::fs::remove_file(mode.join("key")).unwrap();
            std::fs::write(mode.join("key"), b"the app's own").unwrap();
        });
        assert!(r.is_err(), "the name is not the file that was published");
        assert_eq!(std::fs::read(p.mode("key")).unwrap(), b"the app's own");
    }

    #[test]
    fn removing_leaves_what_the_app_put_there_since() {
        let p = Place::new("remove");
        let d = p.app();
        let k = d.publish(&p.stage, "key", b"secret").unwrap();
        std::fs::remove_file(p.mode("key")).unwrap();
        std::fs::write(p.mode("key"), b"the app's own").unwrap();
        assert!(!d.remove_published("key", &k));
        assert_eq!(std::fs::read(p.mode("key")).unwrap(), b"the app's own");
        std::fs::remove_file(p.mode("key")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", p.mode("key")).unwrap();
        assert!(!d.remove_published("key", &k), "a link is not removed either");
    }

    #[test]
    fn a_staging_folder_others_can_use_is_refused() {
        let p = Place::new("stage");
        let d = p.app();
        std::fs::set_permissions(&p.stage, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(d.publish(&p.stage, "key", b"secret").is_err());
        assert!(!p.mode("key").exists());
    }

    #[test]
    fn without_a_trusted_label_nothing_is_published() {
        let p = Place::new("label");
        match AppDir::open(&p.base.join("app"), &["files", "mode"], me(), Labels::Required) {
            // A computer without SELinux: no label at all.
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::PermissionDenied),
            // With SELinux a temporary folder carries no app data label either.
            Ok(_) => panic!("a folder without an app data label was accepted"),
        }
    }
}

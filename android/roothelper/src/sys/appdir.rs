// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Writing into the app's own folder as root, without being sent elsewhere. The app
//! owns `files/` and `files/mode/` and can replace them with links at any moment, so the
//! folder is opened one step at a time from a fixed start, never following a link
//! (`O_NOFOLLOW`), each step checked to belong to the app; later renames of the path
//! cannot move that open folder.
//!
//! A file is published inside that open folder only, never moved in from elsewhere (the
//! app's folder has an encryption policy of its own: a rename from root's folders fails as
//! if from another file system): it is made there new under a name of its own, never
//! through a link (openat with O_CREAT | O_EXCL | O_NOFOLLOW), written and flushed while
//! it is still root's, given to the app (owner, mode 0600, the app folder's SELinux label),
//! renamed into place in the same folder, and the name is then checked to be that very
//! file (device and inode). Without a label it can trust on the app's folder the helper
//! publishes nothing. Removing it later removes only that file, never something the app
//! put under the same name (see [AppDir::remove_published]).

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
fn lstat_at(dir: RawFd, name: &std::ffi::CStr) -> io::Result<libc::stat> {
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

    /// Publishes `data` as `name` for the app (see the top of this file).
    pub fn publish(&self, name: &str, data: &[u8]) -> io::Result<Published> {
        self.publish_with(name, data, || {})
    }

    fn tmp_name(what: &str) -> io::Result<CString> {
        cstr(&format!(".bageo-{what}.{}.{}", std::process::id(), NEXT_TMP.fetch_add(1, Ordering::Relaxed)))
    }

    /// [`AppDir::publish`], with `between` run after the rename and before the check
    /// (where the tests swap the file).
    fn publish_with(&self, name: &str, data: &[u8], between: impl FnOnce()) -> io::Result<Published> {
        let target = cstr(name)?;
        let tmp = Self::tmp_name("new")?;
        let dir = self.fd.as_raw_fd();
        // SAFETY: an *at call on the folder we hold open, with a NUL-terminated name.
        let fd = unsafe {
            libc::openat(
                dir,
                tmp.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just opened.
        let mut f = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let id = file_id(&fstat(f.as_raw_fd())?);
        let written = (|| -> io::Result<()> {
            // Written and flushed while it is still root's (mode 0600), then given to the app.
            f.write_all(data)?;
            f.sync_all()?;
            // SAFETY: plain calls on the descriptor we own.
            if unsafe { libc::fchown(f.as_raw_fd(), self.uid, self.gid) } != 0
                || unsafe { libc::fchmod(f.as_raw_fd(), 0o600) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if let Some(label) = &self.label {
                set_label(f.as_raw_fd(), label)?;
            }
            Ok(())
        })();
        if let Err(e) = written {
            self.remove_if(&tmp, id);
            return Err(e);
        }
        // SAFETY: a rename inside the folder we hold open, NUL-terminated names.
        if unsafe { libc::renameat(dir, tmp.as_ptr(), dir, target.as_ptr()) } != 0 {
            let e = io::Error::last_os_error();
            self.remove_if(&tmp, id);
            return Err(e);
        }
        between();
        match lstat_at(dir, &target) {
            Ok(st) if is_file(&st) && file_id(&st) == id => Ok(Published { id, _held: f }),
            _ => Err(io::Error::other("the published file was replaced")),
        }
    }

    /// Removes `name` only if it is still the file that was published.
    pub fn remove_published(&self, name: &str, p: &Published) -> bool {
        match cstr(name) {
            Ok(c) => self.remove_if(&c, p.id),
            Err(_) => false,
        }
    }

    /// Removes the entry `name` only if it is the file `id`, with no moment in which the
    /// app could put something else under the name and have that removed instead:
    /// whatever is there is first renamed into a folder of root's own inside the app's
    /// folder (made for this, mode 0700: the app can no longer touch it there), checked
    /// there, and removed only if it is that file; anything else goes back under its
    /// name, or next to it if the app has used the name meanwhile.
    fn remove_if(&self, name: &CString, id: FileId) -> bool {
        let dir = self.fd.as_raw_fd();
        let Ok(sub) = Self::tmp_name("rm") else { return false };
        // SAFETY: mkdirat on the folder we hold open (it never follows a link and fails
        // when the name is taken).
        if unsafe { libc::mkdirat(dir, sub.as_ptr(), 0o700) } != 0 {
            return false;
        }
        let Ok(sub_fd) = open_dir(Some(dir), sub.to_str().unwrap_or("")) else {
            return false;
        };
        let sfd = sub_fd.as_raw_fd();
        // SAFETY: geteuid has no preconditions.
        let mine = fstat(sfd).is_ok_and(|st| st.st_uid == unsafe { libc::geteuid() } && st.st_mode & 0o077 == 0);
        let k = c"k";
        // SAFETY: a rename between two folders we hold open, NUL-terminated names.
        let moved = mine && unsafe { libc::renameat(dir, name.as_ptr(), sfd, k.as_ptr()) } == 0;
        let ours = moved && lstat_at(sfd, k).is_ok_and(|st| is_file(&st) && file_id(&st) == id);
        let removed = if ours {
            // SAFETY: unlinkat in our own folder, where the app cannot change anything.
            (unsafe { libc::unlinkat(sfd, k.as_ptr(), 0) }) == 0
        } else {
            if moved && !rename_noreplace(sfd, k, dir, name) {
                // Not that file, and the app has used the name meanwhile: next to it.
                // Failing that too it stays in our folder, which is then left.
                if let Ok(kept) = Self::tmp_name("kept") {
                    rename_noreplace(sfd, k, dir, &kept);
                }
            }
            false
        };
        drop(sub_fd);
        // SAFETY: removes the (now empty) folder we made; fails harmlessly otherwise.
        unsafe {
            libc::unlinkat(dir, sub.as_ptr(), libc::AT_REMOVEDIR);
        }
        removed
    }
}

/// renameat2 with RENAME_NOREPLACE: never replaces what is at the target.
fn rename_noreplace(from_dir: RawFd, from: &std::ffi::CStr, to_dir: RawFd, to: &std::ffi::CStr) -> bool {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    // SAFETY: a plain system call on folders we hold open with NUL-terminated names.
    unsafe { libc::syscall(libc::SYS_renameat2, from_dir, from.as_ptr(), to_dir, to.as_ptr(), RENAME_NOREPLACE) == 0 }
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
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn me() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::getuid() }
    }

    struct Place {
        base: std::path::PathBuf,
    }

    impl Place {
        fn new(tag: &str) -> Place {
            let base = std::env::temp_dir().join(format!("bageo-appdir-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(base.join("app/files/mode")).unwrap();
            std::fs::create_dir_all(base.join("elsewhere")).unwrap();
            Place { base }
        }
        fn app(&self) -> AppDir {
            AppDir::open(&self.base.join("app"), &["files", "mode"], me(), Labels::NotUsed).unwrap()
        }
        fn mode(&self, name: &str) -> std::path::PathBuf {
            self.base.join("app/files/mode").join(name)
        }
        /// The names in the app's folder, sorted.
        fn names(&self, sub: &str) -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(self.base.join(sub))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
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
        let k = d.publish("key", b"secret").unwrap();
        assert_eq!(std::fs::read(p.base.join("elsewhere/target")).unwrap(), b"keep");
        let meta = std::fs::symlink_metadata(p.mode("key")).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(meta.uid(), me());
        assert_eq!(std::fs::read(p.mode("key")).unwrap(), b"secret");
        assert_eq!(p.names("app/files/mode"), vec!["key"], "nothing else left in the folder");

        // The folder swapped for a link after opening: the file still lands in the folder
        // that was opened, not in the link's target.
        std::fs::rename(p.base.join("app/files/mode"), p.base.join("app/files/old")).unwrap();
        std::os::unix::fs::symlink(p.base.join("elsewhere"), p.base.join("app/files/mode")).unwrap();
        d.publish("key2", b"x").unwrap();
        assert!(!p.base.join("elsewhere/key2").exists());
        assert!(p.base.join("app/files/old/key2").exists());

        // Opening through a link is refused; so is a folder of another owner.
        assert!(AppDir::open(&p.base.join("app"), &["files", "mode"], me(), Labels::NotUsed).is_err());
        assert!(AppDir::open(&p.base.join("app"), &["files", "old"], me() + 1, Labels::NotUsed).is_err());
        assert!(d.publish("../x", b"x").is_err());
        assert!(d.remove_published("key", &k));
        assert!(!p.base.join("app/files/old/key").exists());
        assert_eq!(p.names("app/files/old"), vec!["key2"], "no folder of root's left behind");
    }

    #[test]
    fn a_file_swapped_in_after_the_rename_is_noticed_and_left_alone() {
        let p = Place::new("swap");
        let d = p.app();
        let mode = p.mode("");
        let r = d.publish_with("key", b"secret", || {
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
        let k = d.publish("key", b"secret").unwrap();
        std::fs::remove_file(p.mode("key")).unwrap();
        std::fs::write(p.mode("key"), b"the app's own").unwrap();
        assert!(!d.remove_published("key", &k));
        assert_eq!(std::fs::read(p.mode("key")).unwrap(), b"the app's own", "put back under its name");
        std::fs::remove_file(p.mode("key")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", p.mode("key")).unwrap();
        assert!(!d.remove_published("key", &k), "a link is not removed either");
        assert!(std::fs::symlink_metadata(p.mode("key")).unwrap().file_type().is_symlink());
        assert_eq!(p.names("app/files/mode"), vec!["key"], "no folder of root's left behind");
        assert!(!d.remove_published("gone", &k), "nothing there: nothing removed");
    }

    #[test]
    fn a_name_the_app_takes_meanwhile_is_not_overwritten() {
        // What is moved aside and turns out not to be ours goes back next to the name
        // when the app has put something new under it in the meantime.
        let p = Place::new("noreplace");
        let d = p.app();
        let _k = d.publish("key", b"secret").unwrap();
        std::fs::write(p.mode("other"), b"a").unwrap();
        let sfd = open_dir(Some(d.fd.as_raw_fd()), ".").unwrap();
        std::fs::write(p.mode("taken"), b"b").unwrap();
        assert!(!rename_noreplace(sfd.as_raw_fd(), c"other", d.fd.as_raw_fd(), c"taken"));
        assert_eq!(std::fs::read(p.mode("taken")).unwrap(), b"b");
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

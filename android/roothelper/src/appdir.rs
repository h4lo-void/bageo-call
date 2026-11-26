// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Safe file exchange inside the app's device-protected folder as root, without ever
//! crossing encryption policies or following symlinks.
//!
//! The app owns `files/` and `files/mode/` and can rename or replace them at any moment.
//! `AppDir::open` walks from `<apphome>` one step at a time with `O_DIRECTORY | O_NOFOLLOW`,
//! checking the owner on every step and (on Android) verifying a trusted app-data SELinux
//! label on the opened descriptor.
//!
//! Every publish, take, and removal works through a pinned root-owned `0700` working
//! directory (`WorkDir`) created inside that open folder and held open as a descriptor
//! with a `.pin` sentinel inside it:
//! - Being inside `files/mode`, `WorkDir` shares the app folder's fscrypt policy, so pure
//!   `renameat` between `WorkDir` and `files/mode` never crosses encryption boundaries and
//!   never falls back to copying.
//! - Being `0700` root-owned and non-empty while open, the app cannot enter it, cannot
//!   see or rename staged files inside it before publication, cannot `rmdir` it, and
//!   cannot move it to another parent directory.
//! - Even if the app renames `WorkDir` within `files/mode`, `WorkDir::drop` locates it by
//!   `(dev, ino)` in `files/mode` and removes it cleanly.

use std::ffi::{CStr, CString};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

pub struct AppDir {
    fd: OwnedFd,
    uid: u32,
    gid: u32,
    root_uid: u32,
    label: Option<Vec<u8>>,
}

/// Whether the app's folder must carry an SELinux label the helper trusts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Labels {
    /// On a phone: no trusted label, no publishing or taking.
    Required,
    /// Only for host tests without SELinux.
    NotUsed,
}

/// Which file was published: compared before the file is removed. The file is kept
/// open, so its inode cannot be freed and reused while this value lives.
pub struct Published {
    id: FileId,
    _held: std::fs::File,
}

#[derive(Debug)]
pub enum RemoveOutcome {
    Removed,
    AlreadyGone,
    Replaced,
    Failed(io::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub enum TakeOutcome {
    Taken(String),
    Nothing,
    Dropped,
    TakeFailed,
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

fn lstat_path(path: &Path) -> io::Result<libc::stat> {
    let s = path.to_str().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let c = CString::new(s).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: stat is plain data; c is NUL-terminated.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::lstat(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

/// The entry `name` in the folder `dir`, without following a link.
fn lstat_at(dir: RawFd, name: &CStr) -> io::Result<libc::stat> {
    // SAFETY: stat is plain data; a NUL-terminated name relative to an open folder.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir, name.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st)
}

#[allow(clippy::unnecessary_cast)]
fn is_file(st: &libc::stat) -> bool {
    (st.st_mode as u32) & (libc::S_IFMT as u32) == libc::S_IFREG as u32
}

#[allow(clippy::unnecessary_cast)]
fn is_dir(st: &libc::stat) -> bool {
    (st.st_mode as u32) & (libc::S_IFMT as u32) == libc::S_IFDIR as u32
}

#[allow(clippy::unnecessary_cast)]
fn mode_bits(st: &libc::stat) -> u32 {
    st.st_mode as u32
}

#[allow(clippy::unnecessary_cast)]
fn file_id(st: &libc::stat) -> FileId {
    FileId { dev: st.st_dev as u64, ino: st.st_ino as u64 }
}

const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn open_dir_c(parent: Option<RawFd>, name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: NUL-terminated name; descriptor owned immediately.
    let fd = unsafe {
        match parent {
            Some(p) => libc::openat(p, name.as_ptr(), DIR_FLAGS),
            None => libc::open(name.as_ptr(), DIR_FLAGS),
        }
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: just opened.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_step(parent: Option<RawFd>, name: &str, uid: u32) -> io::Result<OwnedFd> {
    let c = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let fd = open_dir_c(parent, &c)?;
    let st = fstat(fd.as_raw_fd())?;
    if !is_dir(&st) || st.st_uid != uid {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(fd)
}

fn raw_renameat(from_dir: RawFd, from: &CStr, to_dir: RawFd, to: &CStr) -> io::Result<()> {
    // SAFETY: pure renameat syscall on open directory descriptors and NUL-terminated names.
    if unsafe { libc::renameat(from_dir, from.as_ptr(), to_dir, to.as_ptr()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Lists all entry names (except `.` and `..`) currently in `dir`.
fn list_dir_names(dir: RawFd) -> Vec<CString> {
    // SAFETY: F_DUPFD_CLOEXEC duplicates an open directory fd.
    let dup_fd = unsafe { libc::fcntl(dir, libc::F_DUPFD_CLOEXEC, 0) };
    if dup_fd < 0 {
        return Vec::new();
    }
    // SAFETY: fdopendir takes ownership of dup_fd on success.
    let dirp = unsafe { libc::fdopendir(dup_fd) };
    if dirp.is_null() {
        unsafe {
            libc::close(dup_fd);
        }
        return Vec::new();
    }
    // `dup_fd` shares the directory offset of `dir`: rewind so every scan starts at the beginning.
    unsafe {
        libc::rewinddir(dirp);
    }
    let mut out = Vec::new();
    loop {
        // SAFETY: dirp is valid until closedir.
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            break;
        }
        // SAFETY: d_name is NUL-terminated by the kernel/libc.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        if name != c"." && name != c".." {
            out.push(name.to_owned());
        }
    }
    // SAFETY: closes dirp and dup_fd.
    unsafe {
        libc::closedir(dirp);
    }
    out
}

/// Whether an SELinux label is one the helper gives to files in the app's folder:
/// an app data file type (`app_data_file`, `privapp_data_file` ...) with a level.
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

fn next_tmp_name(prefix: &str) -> io::Result<CString> {
    cstr(&format!("{prefix}.{}.{}", std::process::id(), NEXT_TMP.fetch_add(1, Ordering::Relaxed)))
}

struct WorkDir<'a> {
    app: &'a AppDir,
    fd: OwnedFd,
    id: FileId,
    name: CString,
    handed_to_app: bool,
}

impl<'a> WorkDir<'a> {
    fn open(app: &'a AppDir) -> io::Result<WorkDir<'a>> {
        let dir = app.fd.as_raw_fd();
        let mut last_err = io::Error::other("cannot create working folder");
        for _ in 0..8 {
            let name = next_tmp_name(".bageo-work")?;
            // SAFETY: mkdirat on open folder; never follows a link.
            if unsafe { libc::mkdirat(dir, name.as_ptr(), 0o700 as libc::mode_t) } != 0 {
                last_err = io::Error::last_os_error();
                app.sweep();
                continue;
            }
            let wfd = match open_dir_c(Some(dir), &name) {
                Ok(fd) => fd,
                Err(e) => {
                    last_err = e;
                    app.sweep();
                    continue;
                }
            };
            let st = match fstat(wfd.as_raw_fd()) {
                Ok(st) if is_dir(&st) && st.st_uid == app.root_uid && (mode_bits(&st) & 0o777) == 0o700 => st,
                _ => {
                    last_err = io::Error::from(io::ErrorKind::PermissionDenied);
                    drop(wfd);
                    app.sweep();
                    continue;
                }
            };
            // Sentinel `.pin` keeps the folder non-empty while open so the app cannot rmdir it.
            // SAFETY: openat inside our verified 0700 folder.
            let pfd = unsafe {
                libc::openat(
                    wfd.as_raw_fd(),
                    c".pin".as_ptr(),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if pfd < 0 {
                last_err = io::Error::last_os_error();
                drop(wfd);
                app.sweep();
                continue;
            }
            // SAFETY: just opened.
            unsafe {
                libc::close(pfd);
            }
            return Ok(WorkDir { app, fd: wfd, id: file_id(&st), name, handed_to_app: false });
        }
        Err(last_err)
    }

    fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Moves `item` out of `self` back into `app.fd` under `preferred` (if free) or a
    /// fresh `<fallback_prefix>.<pid>.<n>` name without overwriting anything. If every
    /// rename attempt fails, hands this `WorkDir` (after removing `.pin` and root files)
    /// to the app so the item is never stranded in an inaccessible root-only folder.
    fn evacuate(&mut self, item: &CStr, preferred: Option<&CStr>, fallback_prefix: &str) -> bool {
        let dir = self.app.fd.as_raw_fd();
        if let Some(pref) = preferred {
            if rename_noreplace(self.raw(), item, dir, pref) {
                return true;
            }
        }
        for _ in 0..16 {
            let Ok(alt) = next_tmp_name(fallback_prefix) else { break };
            if rename_noreplace(self.raw(), item, dir, &alt) {
                return true;
            }
        }
        // Last-resort recovery: ensure only `item` remains, then give the folder to the app.
        for own in [c".pin", c"s"] {
            if own != item {
                unsafe {
                    libc::unlinkat(self.raw(), own.as_ptr(), 0);
                }
            }
        }
        // SAFETY: fchown/fchmod on our open folder descriptor.
        let _ = unsafe { libc::fchown(self.raw(), self.app.uid, self.app.gid) };
        let _ = unsafe { libc::fchmod(self.raw(), 0o700 as libc::mode_t) };
        if let Some(label) = &self.app.label {
            let _ = set_label(self.raw(), label);
        }
        self.handed_to_app = true;
        false
    }
}

impl Drop for WorkDir<'_> {
    fn drop(&mut self) {
        if self.handed_to_app {
            return;
        }
        let wfd = self.raw();
        for own in [c".pin", c"s", c"r", c"k"] {
            if let Ok(st) = lstat_at(wfd, own) {
                if !is_dir(&st) && st.st_uid == self.app.root_uid {
                    // SAFETY: unlinkat inside our verified 0700 folder.
                    unsafe {
                        libc::unlinkat(wfd, own.as_ptr(), 0);
                    }
                }
            }
        }
        let dir = self.app.fd.as_raw_fd();
        if lstat_at(dir, &self.name).is_ok_and(|st| file_id(&st) == self.id)
            && unsafe { libc::unlinkat(dir, self.name.as_ptr(), libc::AT_REMOVEDIR) } == 0
        {
            return;
        }
        // The app renamed our working folder within `files/mode`: find it by (dev, ino).
        for entry in list_dir_names(dir) {
            if lstat_at(dir, &entry).is_ok_and(|st| file_id(&st) == self.id) {
                unsafe {
                    libc::unlinkat(dir, entry.as_ptr(), libc::AT_REMOVEDIR);
                }
                break;
            }
        }
    }
}

impl AppDir {
    /// Opens `<base>/<steps...>`: `base` is a path whose parent only the system controls
    /// (opened without following a link); it and every step must belong to `uid`.
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
        // SAFETY: geteuid has no preconditions.
        let root_uid = unsafe { libc::geteuid() };
        let app = AppDir { fd, uid, gid: st.st_gid, root_uid, label };
        app.sweep();
        Ok(app)
    }

    /// Cleans any leftover root-owned `0700` working directories in `files/mode` from an
    /// earlier interrupted process, evacuating any non-root items back to the app first.
    pub fn sweep(&self) {
        let dir = self.fd.as_raw_fd();
        for entry in list_dir_names(dir) {
            let Ok(st) = lstat_at(dir, &entry) else { continue };
            let name_bytes = entry.to_bytes();
            let looks_ours = if self.root_uid == self.uid {
                name_bytes.starts_with(b".bageo-")
            } else {
                st.st_uid == self.root_uid
            };
            if !is_dir(&st) || !looks_ours || (mode_bits(&st) & 0o022) != 0 {
                continue;
            }
            let expected = file_id(&st);
            let Ok(dfd) = open_dir_c(Some(dir), &entry) else { continue };
            let Ok(dst) = fstat(dfd.as_raw_fd()) else { continue };
            if !is_dir(&dst) || file_id(&dst) != expected || dst.st_uid != self.root_uid || (mode_bits(&dst) & 0o022) != 0 {
                continue;
            }
            let raw_d = dfd.as_raw_fd();
            let mut stuck_app_item = false;
            for item in list_dir_names(raw_d) {
                let Ok(ist) = lstat_at(raw_d, &item) else { continue };
                let root_plain = !is_dir(&ist)
                    && ist.st_uid == self.root_uid
                    && (self.root_uid != self.uid || item == c".pin" || item == c"s");
                if root_plain {
                    unsafe {
                        libc::unlinkat(raw_d, item.as_ptr(), 0);
                    }
                } else {
                    let mut moved = false;
                    for _ in 0..16 {
                        let Ok(alt) = next_tmp_name(".bageo-kept") else { break };
                        if rename_noreplace(raw_d, &item, dir, &alt) {
                            moved = true;
                            break;
                        }
                    }
                    if !moved {
                        stuck_app_item = true;
                    }
                }
            }
            if stuck_app_item {
                let _ = unsafe { libc::fchown(raw_d, self.uid, self.gid) };
                let _ = unsafe { libc::fchmod(raw_d, 0o700 as libc::mode_t) };
                if let Some(label) = &self.label {
                    let _ = set_label(raw_d, label);
                }
                continue;
            }
            drop(dfd);
            unsafe {
                libc::unlinkat(dir, entry.as_ptr(), libc::AT_REMOVEDIR);
            }
        }
    }

    /// Publishes `data` as `name` for the app and keeps a handle open for later verified removal.
    pub fn publish(&self, name: &str, data: &[u8]) -> io::Result<Published> {
        self.publish_hooks(name, data, || {}, || {})
    }

    /// Publishes `data` as `name` for the app without holding the file open afterwards.
    pub fn publish_once(&self, name: &str, data: &[u8]) -> io::Result<()> {
        self.publish(name, data).map(|_| ())
    }

    fn publish_hooks(
        &self,
        name: &str,
        data: &[u8],
        after_work: impl FnOnce(),
        after_rename: impl FnOnce(),
    ) -> io::Result<Published> {
        let target = cstr(name)?;
        let work = WorkDir::open(self)?;
        after_work();
        let wfd = work.raw();
        let dir = self.fd.as_raw_fd();
        // SAFETY: openat inside our verified, open 0700 root working folder.
        let fd = unsafe {
            libc::openat(
                wfd,
                c"s".as_ptr(),
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
        (|| -> io::Result<()> {
            f.write_all(data)?;
            f.sync_all()?;
            // SAFETY: fchown/fchmod on our open file descriptor.
            if unsafe { libc::fchown(f.as_raw_fd(), self.uid, self.gid) } != 0
                || unsafe { libc::fchmod(f.as_raw_fd(), 0o600 as libc::mode_t) } != 0
            {
                return Err(io::Error::last_os_error());
            }
            if let Some(label) = &self.label {
                set_label(f.as_raw_fd(), label)?;
            }
            Ok(())
        })()?;

        if let Err(e) = raw_renameat(wfd, c"s", dir, &target) {
            // If the app placed a real directory at `target`, move it aside without
            // deleting it and retry the atomic rename once.
            if lstat_at(dir, &target).is_ok_and(|st| is_dir(&st)) {
                let mut aside = false;
                for _ in 0..16 {
                    let Ok(rej) = next_tmp_name(&format!("{name}.rejected")) else { break };
                    if rename_noreplace(dir, &target, dir, &rej) {
                        aside = true;
                        break;
                    }
                }
                if !aside {
                    return Err(e);
                }
                raw_renameat(wfd, c"s", dir, &target)?;
            } else {
                return Err(e);
            }
        }
        after_rename();
        match lstat_at(dir, &target) {
            Ok(st) if is_file(&st) && file_id(&st) == id => Ok(Published { id, _held: f }),
            _ => Err(io::Error::other("the published file was replaced")),
        }
    }

    /// Removes `name` only if it is still the file that was published.
    pub fn remove_published(&self, name: &str, p: &Published) -> bool {
        matches!(self.remove_published_status(name, p), RemoveOutcome::Removed)
    }

    /// Removes `name` only if it is still `p`, distinguishing benign disappearance or
    /// replacement from an actual cleanup failure so callers can log failures.
    pub fn remove_published_status(&self, name: &str, p: &Published) -> RemoveOutcome {
        let Ok(target) = cstr(name) else {
            return RemoveOutcome::Failed(io::Error::from(io::ErrorKind::InvalidInput));
        };
        let dir = self.fd.as_raw_fd();
        match lstat_at(dir, &target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return RemoveOutcome::AlreadyGone,
            Err(e) => return RemoveOutcome::Failed(e),
            // `p._held` keeps our inode alive, so any other file_id or type is the app's own
            // replacement; leave it right where it is without moving it.
            Ok(st) if !is_file(&st) || file_id(&st) != p.id => return RemoveOutcome::Replaced,
            Ok(_) => {}
        }
        let mut work = match WorkDir::open(self) {
            Ok(w) => w,
            Err(e) => return RemoveOutcome::Failed(e),
        };
        let wfd = work.raw();
        if let Err(e) = raw_renameat(dir, &target, wfd, c"k") {
            return match lstat_at(dir, &target) {
                Err(e2) if e2.kind() == io::ErrorKind::NotFound => RemoveOutcome::AlreadyGone,
                Ok(st) if !is_file(&st) || file_id(&st) != p.id => RemoveOutcome::Replaced,
                _ => RemoveOutcome::Failed(e),
            };
        }
        let ours = lstat_at(wfd, c"k").is_ok_and(|st| is_file(&st) && file_id(&st) == p.id);
        if ours {
            // SAFETY: unlinkat inside our 0700 root working folder.
            if unsafe { libc::unlinkat(wfd, c"k".as_ptr(), 0) } == 0 {
                RemoveOutcome::Removed
            } else {
                RemoveOutcome::Failed(io::Error::last_os_error())
            }
        } else if work.evacuate(c"k", Some(&target), ".bageo-kept") {
            RemoveOutcome::Replaced
        } else {
            RemoveOutcome::Failed(io::Error::other("swapped entry left in recovered app folder"))
        }
    }

    /// Atomically takes a single-line request file `name` (at most `max_bytes` including
    /// its trailing newline) from the app's folder using pure `renameat` into `WorkDir`.
    pub fn take_line(&self, name: &str, max_bytes: usize) -> TakeOutcome {
        let Ok(target) = cstr(name) else { return TakeOutcome::Dropped };
        let dir = self.fd.as_raw_fd();
        #[cfg(not(target_os = "android"))]
        if let Ok(fail_path) = std::env::var("BAGEO_TEST_FAIL_FILE") {
            if std::fs::read_to_string(fail_path).is_ok_and(|s| s.trim() == name) {
                return if lstat_at(dir, &target).is_ok() {
                    TakeOutcome::TakeFailed
                } else {
                    TakeOutcome::Nothing
                };
            }
        }
        match lstat_at(dir, &target) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return TakeOutcome::Nothing,
            Err(_) => return TakeOutcome::TakeFailed,
            Ok(_) => {}
        }
        let mut work = match WorkDir::open(self) {
            Ok(w) => w,
            Err(_) => return TakeOutcome::TakeFailed,
        };
        let wfd = work.raw();
        if raw_renameat(dir, &target, wfd, c"r").is_err() {
            return match lstat_at(dir, &target) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => TakeOutcome::Nothing,
                _ => TakeOutcome::TakeFailed,
            };
        }
        let Ok(st) = lstat_at(wfd, c"r") else { return TakeOutcome::Dropped };
        if is_dir(&st) {
            let _ = work.evacuate(c"r", None, &format!("{name}.rejected"));
            return TakeOutcome::Dropped;
        }
        if !is_file(&st) {
            unsafe {
                libc::unlinkat(wfd, c"r".as_ptr(), 0);
            }
            return TakeOutcome::Dropped;
        }
        let size = st.st_size;
        if st.st_uid != self.uid || size < 2 || (size as usize) > max_bytes {
            unsafe {
                libc::unlinkat(wfd, c"r".as_ptr(), 0);
            }
            return TakeOutcome::Dropped;
        }
        let expected = file_id(&st);
        // SAFETY: openat on a verified plain file inside our 0700 root folder.
        let rfd = unsafe {
            libc::openat(wfd, c"r".as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        };
        if rfd < 0 {
            unsafe {
                libc::unlinkat(wfd, c"r".as_ptr(), 0);
            }
            return TakeOutcome::Dropped;
        }
        // SAFETY: just opened.
        let f = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(rfd) });
        let mut buf = Vec::new();
        let read_ok = fstat(f.as_raw_fd())
            .is_ok_and(|fst| is_file(&fst) && fst.st_uid == self.uid && file_id(&fst) == expected)
            && f.take(max_bytes as u64 + 1).read_to_end(&mut buf).is_ok();
        unsafe {
            libc::unlinkat(wfd, c"r".as_ptr(), 0);
        }
        if !read_ok || buf.len() > max_bytes || !buf.ends_with(b"\n") {
            return TakeOutcome::Dropped;
        }
        buf.pop();
        if buf.is_empty() || buf.contains(&b'\n') || buf.contains(&b'\0') {
            return TakeOutcome::Dropped;
        }
        match String::from_utf8(buf) {
            Ok(line) => TakeOutcome::Taken(line),
            Err(_) => TakeOutcome::Dropped,
        }
    }
}

fn ipc_labels() -> Labels {
    #[cfg(target_os = "android")]
    {
        Labels::Required
    }
    #[cfg(not(target_os = "android"))]
    {
        if std::env::var_os("BAGEO_TEST_NO_SELINUX").is_some() {
            Labels::NotUsed
        } else {
            Labels::Required
        }
    }
}

fn open_ipc_appdir(apphome: &str) -> io::Result<AppDir> {
    let p = Path::new(apphome);
    let st = lstat_path(p)?;
    if !is_dir(&st) {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    #[cfg(target_os = "android")]
    if st.st_uid == 0 {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    #[cfg(not(target_os = "android"))]
    if st.st_uid == 0 && std::env::var_os("BAGEO_TEST_NO_SELINUX").is_none() {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    AppDir::open(p, &["files", "mode"], st.st_uid, ipc_labels())
}

/// Internal subcommands used by `mode_daemon.sh` so mode-switch requests and state
/// publication use FD-pinned pure `renameat` instead of shell `mv`.
pub fn run_ipc(args: &[&str]) -> Option<i32> {
    match *args {
        ["ipc-sweep", apphome] => {
            let _ = open_ipc_appdir(apphome);
            Some(0)
        }
        ["ipc-take", apphome, name, max] => {
            if !matches!(name, "request" | "audio_helper") {
                return Some(2);
            }
            let Ok(max_bytes) = max.parse::<usize>() else { return Some(2) };
            if !(2..=4096).contains(&max_bytes) {
                return Some(2);
            }
            let Ok(dir) = open_ipc_appdir(apphome) else {
                return Some(1);
            };
            let code = match dir.take_line(name, max_bytes) {
                TakeOutcome::Taken(line) => {
                    print!("{line}");
                    0
                }
                TakeOutcome::Nothing => 1,
                TakeOutcome::Dropped => 2,
                TakeOutcome::TakeFailed => 3,
            };
            Some(code)
        }
        ["ipc-publish", apphome] => {
            let mut data = Vec::new();
            if io::stdin().take(65537).read_to_end(&mut data).is_err() || data.is_empty() || data.len() > 65536 {
                return Some(1);
            }
            let Ok(dir) = open_ipc_appdir(apphome) else {
                return Some(1);
            };
            Some(if dir.publish_once("state", &data).is_ok() { 0 } else { 1 })
        }
        _ => None,
    }
}

/// Atomic rename that never overwrites an existing target.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn rename_noreplace(from_dir: RawFd, from: &CStr, to_dir: RawFd, to: &CStr) -> bool {
    const RENAME_NOREPLACE: libc::c_uint = 1;
    // SAFETY: syscall on open folder descriptors with NUL-terminated names.
    if unsafe { libc::syscall(libc::SYS_renameat2, from_dir, from.as_ptr(), to_dir, to.as_ptr(), RENAME_NOREPLACE) } == 0 {
        return true;
    }
    let err = io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS))
        && lstat_at(to_dir, to).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
    {
        return raw_renameat(from_dir, from, to_dir, to).is_ok();
    }
    false
}

#[cfg(target_os = "macos")]
fn rename_noreplace(from_dir: RawFd, from: &CStr, to_dir: RawFd, to: &CStr) -> bool {
    // SAFETY: renameatx_np on open folder descriptors with NUL-terminated names.
    unsafe { libc::renameatx_np(from_dir, from.as_ptr(), to_dir, to.as_ptr(), libc::RENAME_EXCL) == 0 }
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos")))]
fn rename_noreplace(from_dir: RawFd, from: &CStr, to_dir: RawFd, to: &CStr) -> bool {
    lstat_at(to_dir, to).is_err_and(|e| e.kind() == io::ErrorKind::NotFound)
        && raw_renameat(from_dir, from, to_dir, to).is_ok()
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const LABEL_ATTR: &CStr = c"security.selinux";

#[cfg(any(target_os = "linux", target_os = "android"))]
fn get_label(fd: RawFd) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; 256];
    // SAFETY: buffer has the given length.
    let n = unsafe { libc::fgetxattr(fd, LABEL_ATTR.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn get_label(_fd: RawFd) -> Option<Vec<u8>> {
    None
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn set_label(fd: RawFd, label: &[u8]) -> io::Result<()> {
    // SAFETY: label bytes live through the call.
    if unsafe { libc::fsetxattr(fd, LABEL_ATTR.as_ptr(), label.as_ptr().cast(), label.len(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn set_label(_fd: RawFd, _label: &[u8]) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn me() -> u32 {
        // SAFETY: no preconditions.
        unsafe { libc::geteuid() }
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
    fn publishes_only_inside_the_real_folder_including_over_dir_symlinks() {
        let p = Place::new("inside");
        let d = p.app();
        // A link to a file is replaced, target untouched.
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

        // A link pointing to a directory is also replaced atomically (never followed, never fails).
        std::os::unix::fs::symlink(p.base.join("elsewhere"), p.mode("state")).unwrap();
        d.publish_once("state", b"mode=ai\n").unwrap();
        assert!(std::fs::symlink_metadata(p.mode("state")).unwrap().file_type().is_file());
        assert!(!p.base.join("elsewhere/state").exists());

        // A real directory placed at `state` is moved aside untouched and `state` is published.
        std::fs::remove_file(p.mode("state")).unwrap();
        std::fs::create_dir_all(p.mode("state/keepdir")).unwrap();
        std::fs::write(p.mode("state/keepdir/f"), b"mine").unwrap();
        d.publish_once("state", b"mode=normal\n").unwrap();
        assert_eq!(std::fs::read(p.mode("state")).unwrap(), b"mode=normal\n");
        let rej: Vec<String> = p.names("app/files/mode").into_iter().filter(|n| n.starts_with("state.rejected.")).collect();
        assert_eq!(rej.len(), 1);
        assert_eq!(std::fs::read(p.mode(&format!("{}/keepdir/f", rej[0]))).unwrap(), b"mine");
        std::fs::remove_dir_all(p.mode(&rej[0])).unwrap();
        std::fs::remove_file(p.mode("state")).unwrap();

        // Swapping the folder for a symlink after opening does not redirect open AppDir.
        std::fs::rename(p.base.join("app/files/mode"), p.base.join("app/files/old")).unwrap();
        std::os::unix::fs::symlink(p.base.join("elsewhere"), p.base.join("app/files/mode")).unwrap();
        d.publish("key2", b"x").unwrap();
        assert!(!p.base.join("elsewhere/key2").exists());
        assert!(p.base.join("app/files/old/key2").exists());

        assert!(AppDir::open(&p.base.join("app"), &["files", "mode"], me(), Labels::NotUsed).is_err());
        assert!(AppDir::open(&p.base.join("app"), &["files", "old"], me() + 1, Labels::NotUsed).is_err());
        assert!(d.publish("../x", b"x").is_err());
        assert!(matches!(d.remove_published_status("key", &k), RemoveOutcome::Removed));
        assert!(!p.base.join("app/files/old/key").exists());
        assert_eq!(p.names("app/files/old"), vec!["key2"], "no folder of root's left behind");
    }

    #[test]
    fn renaming_the_working_folder_mid_publish_neither_breaks_publish_nor_leaks_it() {
        let p = Place::new("rename-work");
        let d = p.app();
        let mode_dir = p.mode("");
        let k = d
            .publish_hooks(
                "key",
                b"secret",
                || {
                    for entry in std::fs::read_dir(&mode_dir).unwrap().flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if name.starts_with(".bageo-work.") {
                            std::fs::rename(entry.path(), mode_dir.join(".app-renamed-work")).unwrap();
                        }
                    }
                },
                || {},
            )
            .unwrap();
        assert_eq!(std::fs::read(p.mode("key")).unwrap(), b"secret");
        assert_eq!(p.names("app/files/mode"), vec!["key"], "renamed working folder still cleaned by inode");
        assert!(d.remove_published("key", &k));
        assert!(p.names("app/files/mode").is_empty());
    }

    #[test]
    fn a_file_swapped_in_after_the_rename_is_noticed_and_left_alone() {
        let p = Place::new("swap");
        let d = p.app();
        let mode = p.mode("");
        let r = d.publish_hooks(
            "key",
            b"secret",
            || {},
            || {
                std::fs::remove_file(mode.join("key")).unwrap();
                std::fs::write(mode.join("key"), b"the app's own").unwrap();
            },
        );
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
        assert!(matches!(d.remove_published_status("key", &k), RemoveOutcome::Replaced));
        assert_eq!(std::fs::read(p.mode("key")).unwrap(), b"the app's own", "untouched under its name");
        std::fs::remove_file(p.mode("key")).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", p.mode("key")).unwrap();
        assert!(matches!(d.remove_published_status("key", &k), RemoveOutcome::Replaced));
        assert!(std::fs::symlink_metadata(p.mode("key")).unwrap().file_type().is_symlink());
        assert_eq!(p.names("app/files/mode"), vec!["key"], "no folder of root's left behind");
        assert!(matches!(d.remove_published_status("gone", &k), RemoveOutcome::AlreadyGone));
    }

    #[test]
    fn a_name_the_app_takes_meanwhile_is_not_overwritten() {
        let p = Place::new("noreplace");
        let d = p.app();
        let _k = d.publish("key", b"secret").unwrap();
        std::fs::write(p.mode("other"), b"a").unwrap();
        let sfd = open_dir_c(Some(d.fd.as_raw_fd()), c".").unwrap();
        std::fs::write(p.mode("taken"), b"b").unwrap();
        assert!(!rename_noreplace(sfd.as_raw_fd(), c"other", d.fd.as_raw_fd(), c"taken"));
        assert_eq!(std::fs::read(p.mode("taken")).unwrap(), b"b");
    }

    #[test]
    fn take_line_accepts_only_plain_single_line_files_and_keeps_folders() {
        let p = Place::new("take");
        let d = p.app();
        assert_eq!(d.take_line("request", 129), TakeOutcome::Nothing);

        std::fs::write(p.mode("request"), b"ai req-1 4 3\n").unwrap();
        assert_eq!(d.take_line("request", 129), TakeOutcome::Taken("ai req-1 4 3".into()));
        assert!(!p.mode("request").exists());

        // Missing newline or two lines -> dropped.
        std::fs::write(p.mode("request"), b"ai req-2").unwrap();
        assert_eq!(d.take_line("request", 129), TakeOutcome::Dropped);
        std::fs::write(p.mode("request"), b"ai req-2\nnormal req-3\n").unwrap();
        assert_eq!(d.take_line("request", 129), TakeOutcome::Dropped);

        // Symlink -> removed without reading target.
        std::fs::write(p.base.join("elsewhere/req"), b"normal evil-1\n").unwrap();
        std::os::unix::fs::symlink(p.base.join("elsewhere/req"), p.mode("request")).unwrap();
        assert_eq!(d.take_line("request", 129), TakeOutcome::Dropped);
        assert_eq!(std::fs::read(p.base.join("elsewhere/req")).unwrap(), b"normal evil-1\n");

        // Directory -> handed back whole under request.rejected.*.
        std::fs::create_dir_all(p.mode("request/sub")).unwrap();
        std::fs::write(p.mode("request/sub/data"), b"keep").unwrap();
        assert_eq!(d.take_line("request", 129), TakeOutcome::Dropped);
        let rej: Vec<String> = p.names("app/files/mode").into_iter().filter(|n| n.starts_with("request.rejected.")).collect();
        assert_eq!(rej.len(), 1);
        assert_eq!(std::fs::read(p.mode(&format!("{}/sub/data", rej[0]))).unwrap(), b"keep");
    }

    #[test]
    fn without_a_trusted_label_nothing_is_published() {
        let p = Place::new("label");
        match AppDir::open(&p.base.join("app"), &["files", "mode"], me(), Labels::Required) {
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::PermissionDenied),
            Ok(_) => panic!("a folder without an app data label was accepted"),
        }
    }
}

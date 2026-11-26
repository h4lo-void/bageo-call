// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Audio HAL parameters through Android's own Java runtime: the helper starts
//! `/system/bin/app_process` with a small class of this project
//! (`android/roothelper/java/.../AudioParams.java`, shipped as
//! `bageo-audio-params.jar`) that calls `AudioSystem.getParameters` or
//! `setParameters`, the same calls an app's `AudioManager.setParameters` ends up in.
//! The interface to the audio server changes between Android versions; the Java
//! method does not, so this works without knowing binder transaction numbers.
//!
//! The child gets an empty environment plus fixed paths and the boot class path, which
//! is taken only from a zygote process that is verified to be root, to run
//! `/system/bin/app_process64` or `app_process32`, and to be in the `zygote` SELinux
//! domain. Everything about that process is read through one open `/proc/<pid>` folder,
//! so it is all about the same process even if the pid is reused meanwhile. Every entry
//! of the class paths taken from it must be a root-owned file nobody else can write
//! under the system's own folders (`/system`, `/system_ext`, `/product`, `/vendor`,
//! `/apex`). The jar must be a root-owned file nobody else can write, in a root-owned
//! folder nobody else can write. A call that takes longer than 3 seconds is killed; its
//! output is read while it runs.

use std::ffi::{CStr, CString};
use std::fs;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicI32;
use std::time::Duration;

use super::child;
use crate::control::ParamBackend;
use crate::params;

const CLASS: &str = "com.pixelcallai.roothelper.AudioParams";
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OUTPUT: usize = 64 * 1024;

/// The running `app_process` child, so a stop signal can end it at once.
pub static CHILD: AtomicI32 = AtomicI32::new(0);

/// Fixed runtime paths of Android 10 and later.
const FIXED_ENV: &[(&str, &str)] = &[
    ("PATH", "/system/bin:/system/xbin"),
    ("ANDROID_ROOT", "/system"),
    ("ANDROID_DATA", "/data"),
    ("ANDROID_ART_ROOT", "/apex/com.android.art"),
    ("ANDROID_I18N_ROOT", "/apex/com.android.i18n"),
    ("ANDROID_TZDATA_ROOT", "/apex/com.android.tzdata"),
    ("ANDROID_RUNTIME_ROOT", "/apex/com.android.runtime"),
];

/// Taken from the verified zygote.
const ZYGOTE_ENV: &[&str] = &["BOOTCLASSPATH", "DEX2OATBOOTCLASSPATH", "SYSTEMSERVERCLASSPATH"];

/// A class path value: colon-separated absolute paths of plain characters.
pub fn trusted_class_path(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 16 * 1024
        && v.split(':').all(|p| p.starts_with('/') && !p.contains(".."))
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b':' | b'.' | b'_' | b'-' | b'@'))
}

/// Whether a process is the real zygote: its `/proc/<pid>/status`, the target of its
/// `exe` link and its SELinux context (`attr/current`).
pub fn is_zygote(status: &str, exe: &str, context: &str) -> bool {
    let root = status
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .is_some_and(|ids| ids.split_whitespace().all(|id| id == "0"));
    root && (exe == "/system/bin/app_process64" || exe == "/system/bin/app_process32")
        && context.trim_end_matches(['\0', '\n']) == "u:r:zygote:s0"
}

/// The folders a class path entry may lie under: the system's own, read-only images.
const SYSTEM_DIRS: &[&str] = &["/system/", "/system_ext/", "/product/", "/vendor/", "/apex/"];

/// Whether a class path entry's place is acceptable (the file itself is checked
/// separately): under a system folder, every step a plain name.
pub fn system_path(p: &str) -> bool {
    SYSTEM_DIRS.iter().any(|d| p.starts_with(d))
        && p[1..].split('/').all(|step| !step.is_empty() && step != "." && step != "..")
}

/// Whether every entry of a class path is a root-owned file nobody else can write,
/// under a system folder.
fn class_path_files_ok(v: &str) -> bool {
    v.split(':').all(|p| {
        system_path(p) && fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file() && root_only(&m))
    })
}

/// A process's `/proc/<pid>` folder, held open: whatever is read through it belongs to
/// that one process (once it has gone, reads fail instead of reaching another one that
/// got the same pid).
struct ProcDir(OwnedFd);

impl ProcDir {
    fn open(pid: &str) -> io::Result<ProcDir> {
        let path = CString::new(format!("/proc/{pid}")).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: a NUL-terminated path; the descriptor is owned right after.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just opened.
        Ok(ProcDir(unsafe { OwnedFd::from_raw_fd(fd) }))
    }

    /// At most `max` bytes of the file `name` inside the folder.
    fn read(&self, name: &CStr, max: usize) -> io::Result<Vec<u8>> {
        // SAFETY: a NUL-terminated name relative to our open folder.
        let fd = unsafe { libc::openat(self.0.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: just opened.
        let f = fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let mut buf = Vec::new();
        f.take(max as u64 + 1).read_to_end(&mut buf)?;
        if buf.len() > max {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(buf)
    }

    fn link(&self, name: &CStr) -> io::Result<String> {
        let mut buf = [0u8; 512];
        // SAFETY: the buffer has the length given.
        let n = unsafe { libc::readlinkat(self.0.as_raw_fd(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let n = n as usize;
        if n >= buf.len() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        String::from_utf8(buf[..n].to_vec()).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    }
}

const MAX_PROC_FILE: usize = 256 * 1024;

fn zygote_env() -> Result<Vec<(String, String)>, String> {
    let dir = fs::read_dir("/proc").map_err(|_| "cannot list processes")?;
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().filter(|s| s.bytes().all(|b| b.is_ascii_digit())) else { continue };
        let Ok(proc_dir) = ProcDir::open(pid) else { continue };
        let Ok(cmdline) = proc_dir.read(c"cmdline", 4096) else { continue };
        let argv0 = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
        if argv0 != b"zygote64" && argv0 != b"zygote" {
            continue;
        }
        let text = |name: &CStr| proc_dir.read(name, MAX_PROC_FILE).ok().and_then(|b| String::from_utf8(b).ok());
        let status = text(c"status").unwrap_or_default();
        let exe = proc_dir.link(c"exe").unwrap_or_default();
        let ctx = text(c"attr/current").unwrap_or_default();
        if !is_zygote(&status, &exe, &ctx) {
            continue;
        }
        let environ = proc_dir.read(c"environ", MAX_PROC_FILE).map_err(|_| "cannot read zygote")?;
        // Still the same process: its status reads again through the same folder.
        if text(c"status").is_none_or(|s| !is_zygote(&s, &exe, &ctx)) {
            continue;
        }
        let found: Vec<(String, String)> = environ
            .split(|&b| b == 0)
            .filter_map(|kv| std::str::from_utf8(kv).ok()?.split_once('='))
            .filter(|(k, v)| ZYGOTE_ENV.contains(k) && trusted_class_path(v) && class_path_files_ok(v))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        if found.iter().any(|(k, _)| k == "BOOTCLASSPATH") {
            return Ok(found);
        }
    }
    Err("no trustworthy zygote found".into())
}

/// Whether a file (or folder) belongs to root and nobody else may write it.
fn root_only(meta: &fs::Metadata) -> bool {
    meta.uid() == 0 && meta.mode() & 0o022 == 0
}

pub struct AppProcessParams {
    jar: Result<PathBuf, String>,
}

impl AppProcessParams {
    /// The jar next to the running helper, checked once.
    pub fn next_to_helper() -> AppProcessParams {
        let jar = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(super::PARAMS_JAR)))
            .ok_or_else(|| "cannot find the helper's folder".to_string())
            .and_then(|jar| check_jar(&jar).map(|_| jar));
        AppProcessParams { jar }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let jar = self.jar.as_ref().map_err(|e| e.clone())?;
        check_jar(jar)?;
        let mut cmd = Command::new("/system/bin/app_process");
        cmd.env_clear();
        for (k, v) in FIXED_ENV {
            cmd.env(k, v);
        }
        for (k, v) in zygote_env()? {
            cmd.env(k, v);
        }
        cmd.env("CLASSPATH", jar).arg("/system/bin").arg(CLASS).args(args);
        let out = child::run(&mut cmd, TIMEOUT, MAX_OUTPUT, Some(&CHILD)).map_err(|_| "cannot start app_process".to_string())?;
        if out.status.is_none() {
            return Err("the audio parameter tool did not answer".into());
        }
        if !out.ok() {
            return Err("the audio parameter tool failed".into());
        }
        String::from_utf8(out.stdout).map_err(|_| "the audio parameter tool failed".into())
    }
}

fn check_jar(jar: &Path) -> Result<(), String> {
    let bad = || "the audio parameter tool is missing or not installed safely".to_string();
    let meta = fs::symlink_metadata(jar).map_err(|_| bad())?;
    let dir = jar.parent().ok_or_else(bad)?;
    let dir_meta = fs::symlink_metadata(dir).map_err(|_| bad())?;
    if meta.file_type().is_file() && root_only(&meta) && dir_meta.file_type().is_dir() && root_only(&dir_meta) {
        Ok(())
    } else {
        Err(bad())
    }
}

impl ParamBackend for AppProcessParams {
    fn get(&mut self, key: &str) -> Result<Option<String>, String> {
        if !params::valid_key(key) {
            return Err("bad parameter name".into());
        }
        let reply = self.run(&["get", key])?;
        Ok(params::value_in_reply(&reply, key))
    }

    fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        if !params::valid_key(key) || !params::valid_value(value) {
            return Err("bad parameter".into());
        }
        self.run(&["set", &format!("{key}={value}")]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_paths_must_be_plain_absolute_paths() {
        assert!(trusted_class_path("/apex/com.android.art/javalib/core-oj.jar:/system/framework/framework.jar"));
        assert!(!trusted_class_path(""));
        assert!(!trusted_class_path("relative.jar"));
        assert!(!trusted_class_path("/system/../data/x.jar"));
        assert!(!trusted_class_path("/system/framework/x.jar;rm"));
        assert!(!trusted_class_path("/data/local/tmp/$x.jar"));
    }

    #[test]
    fn class_path_entries_only_from_system_folders() {
        assert!(system_path("/apex/com.android.art/javalib/core-oj.jar"));
        assert!(system_path("/system/framework/framework.jar"));
        assert!(system_path("/system_ext/framework/x.jar"));
        for bad in [
            "/data/local/tmp/x.jar",
            "/data/adb/modules/x/system/framework/x.jar",
            "/system/framework/../../data/x.jar",
            "/system//x.jar",
            "/system/./x.jar",
            "/systemx/x.jar",
            "/system/",
            "relative/system/x.jar",
        ] {
            assert!(!system_path(bad), "{bad}");
        }
        // A file that is not root's (as in a test run) is refused too.
        let mine = std::env::temp_dir().join(format!("bageo-cp-{}.jar", std::process::id()));
        std::fs::write(&mine, b"x").unwrap();
        assert!(!class_path_files_ok(mine.to_str().unwrap()));
        let _ = std::fs::remove_file(mine);
    }

    #[test]
    fn only_the_real_zygote() {
        let status = "Name:\tmain\nUid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\n";
        assert!(is_zygote(status, "/system/bin/app_process64", "u:r:zygote:s0\0"));
        assert!(is_zygote(status, "/system/bin/app_process32", "u:r:zygote:s0\n"));
        assert!(!is_zygote(status, "/data/local/tmp/app_process64", "u:r:zygote:s0"));
        assert!(!is_zygote(status, "/system/bin/app_process64", "u:r:untrusted_app:s0"));
        assert!(!is_zygote("Uid:\t10123\t10123\t10123\t10123\n", "/system/bin/app_process64", "u:r:zygote:s0"));
        assert!(!is_zygote("", "/system/bin/app_process64", "u:r:zygote:s0"));
    }
}

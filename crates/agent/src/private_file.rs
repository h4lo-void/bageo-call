// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Where recordings, transcripts, messages, contacts and other private data are written.
//!
//! On Unix the folders are made owner-only (0700) and the files owner-only
//! (0600), so other users on the machine cannot read them. On Windows nothing changes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

/// Create `dir` (and its parents) and make `dir` itself owner-only on Unix.
pub fn create_dir_private(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        // A folder that already existed keeps its old mode above; tighten it. Not being
        // the owner (a shared mount) is not worth failing a save over.
        if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
            let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Create (or empty) a file that only the owner can read on Unix.
pub fn create_file(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(f)
}

/// Open a file for appending (created if missing), owner-only on Unix.
pub fn append_file(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
    }
    Ok(f)
}

/// Write `body` to the temporary file `tmp`, then rename it over `path`: a crash leaves
/// the old file or the new one, never half of one. The folder is created if missing.
pub fn write_atomic(path: &Path, tmp: &Path, body: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        create_dir_private(dir)?;
    }
    let written = (|| -> io::Result<()> {
        let mut f = create_file(tmp)?;
        f.write_all(body)?;
        f.sync_all()?;
        drop(f);
        fs::rename(tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("bageo-private-{tag}-{}", uuid::Uuid::new_v4().simple()))
    }

    #[cfg(unix)]
    fn mode(p: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp() {
        let dir = temp_dir("atomic");
        let path = dir.join("sub").join("data.json");
        let tmp = dir.join("sub").join(".data.json.tmp");
        write_atomic(&path, &tmp, b"one").unwrap();
        write_atomic(&path, &tmp, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(!tmp.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_owner_only_and_folders_too() {
        let dir = temp_dir("modes");
        let path = dir.join("data.json");
        write_atomic(&path, &dir.join(".data.json.tmp"), b"x").unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        let wav = dir.join("a.wav");
        create_file(&wav).unwrap();
        assert_eq!(mode(&wav), 0o600);
        let log = dir.join("a.log");
        drop(append_file(&log).unwrap());
        assert_eq!(mode(&log), 0o600);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn an_old_file_and_folder_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp_dir("old");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        let path = dir.join("data.json");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&path, &dir.join(".data.json.tmp"), b"new").unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        assert_eq!(fs::read(&path).unwrap(), b"new");
        let _ = fs::remove_dir_all(&dir);
    }
}

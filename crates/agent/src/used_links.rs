//! One-tap notification links that were already used.
//!
//! A link that does something (call back, send a text, agree to an
//! appointment) works once: the first press records the link here, and any
//! later press only gets the "already done" page. The record is kept in the
//! data directory (`CALLS_DATA_DIR`, next to `settings.json`), so it
//! survives a restart; an entry is dropped once its link would have expired
//! anyway.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// File in the data directory holding the used links.
const USED_LINKS_FILE: &str = "used-links.json";

/// The id a link is recorded under: a hash of its signature, which covers the
/// action, its target, the name and its time, so the record itself holds
/// nothing a link could be rebuilt from.
pub fn link_id(sig: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(sig.trim().to_ascii_lowercase().as_bytes());
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

pub struct UsedLinks {
    /// None: kept in memory only (tests).
    path: Option<PathBuf>,
    /// Link id -> unix seconds after which the link no longer works.
    used: Mutex<HashMap<String, i64>>,
}

impl UsedLinks {
    pub fn in_memory() -> Self {
        Self { path: None, used: Mutex::new(HashMap::new()) }
    }

    /// The record kept in `path` (missing or unreadable: empty), without the
    /// links that have expired by `now`.
    pub fn open(path: &Path, now: i64) -> Self {
        let mut used: HashMap<String, i64> = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        used.retain(|_, exp| *exp >= now);
        Self { path: Some(path.to_path_buf()), used: Mutex::new(used) }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, i64>> {
        self.used.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Whether the link was used (and has not expired since).
    pub fn is_used(&self, id: &str, now: i64) -> bool {
        self.lock().get(id).is_some_and(|exp| *exp >= now)
    }

    /// Record a link as used. True for the first press; false when it was
    /// already used. `exp` is the link's own expiry (unix seconds).
    pub fn claim(&self, id: &str, exp: i64, now: i64) -> bool {
        let mut used = self.lock();
        used.retain(|_, e| *e >= now);
        if used.contains_key(id) {
            return false;
        }
        used.insert(id.to_string(), exp);
        if let Some(path) = &self.path {
            if let Err(e) = write_record(path, &used) {
                // The press still counts for this run.
                tracing::warn!("used one-tap links {}: {e}", path.display());
            }
        }
        true
    }
}

fn write_record(path: &Path, used: &HashMap<String, i64>) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string(used).map_err(std::io::Error::other)?;
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// The agent's record (in the data directory; in memory under test).
pub fn global() -> &'static UsedLinks {
    static USED: std::sync::OnceLock<UsedLinks> = std::sync::OnceLock::new();
    USED.get_or_init(|| {
        if cfg!(test) {
            return UsedLinks::in_memory();
        }
        let default_dir = if Path::new("/data/calls").is_dir() { "/data/calls" } else { "data/calls" };
        let path = PathBuf::from(crate::config::env_or("CALLS_DATA_DIR", default_dir)).join(USED_LINKS_FILE);
        UsedLinks::open(&path, chrono::Utc::now().timestamp())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("bageo-used-{}", uuid::Uuid::new_v4().simple()));
        (dir.clone(), dir.join(USED_LINKS_FILE))
    }

    #[test]
    fn a_link_is_used_once_and_the_record_survives_a_restart() {
        let (dir, file) = temp_file();
        let now = 1_700_000_000;
        let id = link_id(&"ab".repeat(32));
        let first = UsedLinks::open(&file, now);
        assert!(!first.is_used(&id, now));
        assert!(first.claim(&id, now + 600, now), "the first press does it");
        assert!(!first.claim(&id, now + 600, now), "the second press does not");
        assert!(first.is_used(&id, now));
        // Bageo starts again: the file is read back.
        let again = UsedLinks::open(&file, now + 60);
        assert!(again.is_used(&id, now + 60));
        assert!(!again.claim(&id, now + 600, now + 60));
        // Another link is not affected.
        assert!(again.claim(&link_id(&"cd".repeat(32)), now + 600, now + 60));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expired_links_are_dropped_from_the_record() {
        let (dir, file) = temp_file();
        let now = 1_700_000_000;
        let (old, fresh) = (link_id("old"), link_id("fresh"));
        let store = UsedLinks::open(&file, now);
        assert!(store.claim(&old, now + 10, now));
        assert!(store.claim(&fresh, now + 1000, now));
        // Loading after the first expired: it is gone.
        let later = UsedLinks::open(&file, now + 500);
        assert!(!later.is_used(&old, now + 500));
        assert!(later.is_used(&fresh, now + 500));
        // A claim after the second one's expiry cleans the file too.
        assert!(later.claim(&link_id("third"), now + 2000, now + 1500));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains(&fresh), "{text}");
        assert!(text.contains(&link_id("third")), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_record_holds_a_hash_and_a_broken_file_starts_empty() {
        let (dir, file) = temp_file();
        let sig = "0123456789abcdef".repeat(4);
        let id = link_id(&sig);
        assert_eq!(id.len(), 32);
        assert!(!id.contains(&sig[..16]));
        assert_eq!(link_id(&format!(" {} ", sig.to_ascii_uppercase())), id, "case and spaces do not make another link");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "not json").unwrap();
        let store = UsedLinks::open(&file, 1);
        assert!(store.claim(&id, 10, 1));
        let _ = std::fs::remove_dir_all(dir);
    }
}

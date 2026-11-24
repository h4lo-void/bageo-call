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

/// The id a link is recorded under: a hash of its signature's bytes (decoded,
/// so the same signature written another way is the same link), which cover
/// the action, its target, the name and its time. The record holds nothing a
/// link could be rebuilt from.
pub fn link_id(sig: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"bageo-used1");
    h.update(sig);
    h.finalize().iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// What recording a press gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Claim {
    /// The first press, now recorded: do the action.
    First,
    /// Already used: do nothing.
    Used,
    /// The record could not be saved: do nothing (a press that would not be
    /// remembered after a restart could be repeated).
    NotSaved,
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

    /// Record a link as used. `exp` is the link's own expiry (unix seconds).
    pub fn claim(&self, id: &str, exp: i64, now: i64) -> Claim {
        let mut used = self.lock();
        used.retain(|_, e| *e >= now);
        if used.contains_key(id) {
            return Claim::Used;
        }
        used.insert(id.to_string(), exp);
        if let Some(path) = &self.path {
            if let Err(e) = write_record(path, &used) {
                tracing::warn!("used one-tap links {}: {e}", path.display());
                used.remove(id);
                return Claim::NotSaved;
            }
        }
        Claim::First
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
        let path = crate::config::data_dir().join(USED_LINKS_FILE);
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
        let id = link_id(&[0xab; 32]);
        let first = UsedLinks::open(&file, now);
        assert!(!first.is_used(&id, now));
        assert_eq!(first.claim(&id, now + 600, now), Claim::First, "the first press does it");
        assert_eq!(first.claim(&id, now + 600, now), Claim::Used, "the second press does not");
        assert!(first.is_used(&id, now));
        // Bageo starts again: the file is read back.
        let again = UsedLinks::open(&file, now + 60);
        assert!(again.is_used(&id, now + 60));
        assert_eq!(again.claim(&id, now + 600, now + 60), Claim::Used);
        // Another link is not affected.
        assert_eq!(again.claim(&link_id(&[0xcd; 32]), now + 600, now + 60), Claim::First);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn expired_links_are_dropped_from_the_record() {
        let (dir, file) = temp_file();
        let now = 1_700_000_000;
        let (old, fresh, third) = (link_id(&[1; 32]), link_id(&[2; 32]), link_id(&[3; 32]));
        let store = UsedLinks::open(&file, now);
        assert_eq!(store.claim(&old, now + 10, now), Claim::First);
        assert_eq!(store.claim(&fresh, now + 1000, now), Claim::First);
        // Loading after the first expired: it is gone.
        let later = UsedLinks::open(&file, now + 500);
        assert!(!later.is_used(&old, now + 500));
        assert!(later.is_used(&fresh, now + 500));
        // A claim after the second one's expiry cleans the file too.
        assert_eq!(later.claim(&third, now + 2000, now + 1500), Claim::First);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains(&fresh), "{text}");
        assert!(text.contains(&third), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_record_holds_a_hash_and_a_broken_file_starts_empty() {
        let (dir, file) = temp_file();
        let sig = [0x5a; 32];
        let id = link_id(&sig);
        assert_eq!(id.len(), 32);
        assert!(!id.contains("5a5a5a5a"), "{id}");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "not json").unwrap();
        let store = UsedLinks::open(&file, 1);
        assert_eq!(store.claim(&id, 10, 1), Claim::First);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A press the record cannot keep does not count: nothing is done, and the
    /// link is not taken as used either (it may be pressed again once the
    /// record can be saved).
    #[test]
    fn a_press_that_cannot_be_saved_is_refused_and_not_kept() {
        let (dir, _) = temp_file();
        std::fs::create_dir_all(&dir).unwrap();
        // The record's folder is an ordinary file: nothing can be written under it.
        let blocker = dir.join("not-a-folder");
        std::fs::write(&blocker, "x").unwrap();
        let store = UsedLinks::open(&blocker.join(USED_LINKS_FILE), 1);
        let id = link_id(&[7; 32]);
        assert_eq!(store.claim(&id, 100, 1), Claim::NotSaved);
        assert!(!store.is_used(&id, 1));
        assert_eq!(store.claim(&id, 100, 1), Claim::NotSaved, "still not done the second time");
        let _ = std::fs::remove_dir_all(dir);
    }
}

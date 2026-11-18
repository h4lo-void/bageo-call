// Copyright (C) 2026 h4lo-void (https://github.com/h4lo-void)
// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>
// See LICENSE, LICENSING.md (AGPL-3.0 Section 7 additional terms), and COMMERCIAL_LICENSE.md.

//! Phones over Wi-Fi: the pairing key and the addresses a phone is given.
//!
//! A phone plugged in by USB reaches the agent through `adb reverse` (it
//! connects to loopback). On that connection the agent hands it, in
//! `agent_info`, the Wi-Fi addresses of the agent with a random pairing key
//! (`ws://192.168.1.20:8788/ws?token=<key>`). Unplugged, the phone connects
//! there. The Wi-Fi entry (a listener that only serves `/ws`, started in
//! `main`) lets in nobody without that key or `DEVICE_TOKEN`.
//!
//! The key is made the first time it is needed, kept in [`KEY_FILE`] in the
//! call data folder (mode 600 on Unix) so it survives a restart, and replaced by
//! "Pair phones again" in the console ([`Pairing::renew`]), which also drops
//! every phone connected over Wi-Fi. It is never shown in the console,
//! `/health`, logs or chat replies.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::Engine as _;
use tokio::sync::watch;

/// The file in the call data folder that holds the pairing key.
pub const KEY_FILE: &str = "phone-pairing.key";

/// Characters in a key: 32 random bytes in unpadded base64url.
const KEY_CHARS: usize = 43;

/// The pairing key and the signal that drops the phones connected over Wi-Fi.
pub struct Pairing {
    path: PathBuf,
    key: Mutex<Option<String>>,
    kick: watch::Sender<u64>,
}

/// 32 random bytes (the system's random source, through uuid v4), base64url.
fn fresh_key() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for _ in 0..4 {
        h.update(uuid::Uuid::new_v4().as_bytes());
    }
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(h.finalize())
}

fn valid_key(s: &str) -> bool {
    s.len() >= KEY_CHARS && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Same bytes, compared in time that does not depend on where they differ.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Write `key` to `path` (mode 600 on Unix), through a temporary file.
fn write_key(path: &Path, key: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("key.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(key.as_bytes())?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

impl Pairing {
    /// The pairing of the call data folder `dir`: the key saved there, if any.
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(KEY_FILE);
        let key = std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| valid_key(s));
        #[cfg(unix)]
        if key.is_some() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Self {
            path,
            key: Mutex::new(key),
            kick: watch::channel(0).0,
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.key.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The key, made and saved now when there is none yet. When it cannot be
    /// saved it still works until the agent restarts.
    pub fn key(&self) -> String {
        let mut slot = self.slot();
        if let Some(k) = slot.as_ref() {
            return k.clone();
        }
        let k = fresh_key();
        if let Err(e) = write_key(&self.path, &k) {
            tracing::warn!(
                "phone pairing: could not save the key in {} ({e}); phones must be plugged in again after a restart",
                self.path.display()
            );
        }
        *slot = Some(k.clone());
        k
    }

    /// The key when one was made; never makes one.
    pub fn peek(&self) -> Option<String> {
        self.slot().clone()
    }

    /// `token` is the pairing key. False while there is none.
    pub fn accepts(&self, token: Option<&str>) -> bool {
        match (token.map(str::trim).filter(|t| !t.is_empty()), self.slot().as_deref()) {
            (Some(t), Some(k)) => same(t, k),
            _ => false,
        }
    }

    /// "Pair phones again": a new key replaces the old one (which stops
    /// working at once) and the phones connected over Wi-Fi are dropped.
    pub fn renew(&self) -> std::io::Result<()> {
        let k = fresh_key();
        write_key(&self.path, &k)?;
        *self.slot() = Some(k);
        self.kick();
        Ok(())
    }

    /// Drop every phone connected over Wi-Fi (the Wi-Fi entry, or the pairing key).
    pub fn kick(&self) {
        self.kick.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Changes when the phones connected over Wi-Fi must be dropped.
    pub fn kicked(&self) -> watch::Receiver<u64> {
        self.kick.subscribe()
    }
}

static PAIRING: OnceLock<Pairing> = OnceLock::new();

/// Use the call data folder `dir` (at start, before the first phone connects).
pub fn init(dir: &Path) -> &'static Pairing {
    PAIRING.get_or_init(|| Pairing::open(dir))
}

/// The agent's pairing ([`init`]; else the folder `CALLS_DATA_DIR` names).
pub fn pairing() -> &'static Pairing {
    PAIRING.get_or_init(|| {
        if cfg!(test) {
            return Pairing::open(&std::env::temp_dir().join(format!("bageo-pair-{}", uuid::Uuid::new_v4().simple())));
        }
        let default_dir = if Path::new("/data/calls").is_dir() { "/data/calls" } else { "data/calls" };
        Pairing::open(Path::new(&crate::config::env_or("CALLS_DATA_DIR", default_dir)))
    })
}

/// The port of the running Wi-Fi entry; 0 while there is none.
static ENTRY_PORT: AtomicU16 = AtomicU16::new(0);

pub fn set_entry_port(port: u16) {
    ENTRY_PORT.store(port, Ordering::Relaxed);
}

/// The port of the running Wi-Fi entry.
pub fn entry_port() -> Option<u16> {
    Some(ENTRY_PORT.load(Ordering::Relaxed)).filter(|p| *p != 0)
}

/// May a phone use the Wi-Fi entry: its `token` is the pairing key or
/// `DEVICE_TOKEN` (`device_token`). No exception for loopback or an unset token.
pub fn entry_allows(token: Option<&str>, device_token: &str, pairing: &Pairing) -> bool {
    let device_token = device_token.trim();
    let device_ok = !device_token.is_empty() && token.is_some_and(|t| same(t.trim(), device_token));
    device_ok || pairing.accepts(token)
}

/// The addresses a phone tries over Wi-Fi, in order: the LAN IPv4 address,
/// then `<mdns_host>.local` (left out when the network name is off). Empty
/// without a LAN address.
pub fn wifi_urls(lan_ip: Option<Ipv4Addr>, port: u16, mdns_host: &str, key: &str) -> Vec<String> {
    let Some(ip) = lan_ip else {
        return Vec::new();
    };
    let mut urls = vec![format!("ws://{ip}:{port}/ws?token={key}")];
    if !mdns_host.is_empty() {
        urls.push(format!("ws://{mdns_host}.local:{port}/ws?token={key}"));
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("bageo-pair-{tag}-{}", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn the_wifi_entry_wants_the_pairing_key_or_the_device_token() {
        let dir = temp_dir("entry");
        let p = Pairing::open(&dir);
        assert!(!entry_allows(None, "", &p), "nothing set, nothing sent");
        assert!(!entry_allows(Some(""), "", &p), "an empty token is no token");
        assert!(!entry_allows(Some("anything"), "", &p), "no key made yet");
        let key = p.key();
        assert!(!entry_allows(None, "", &p), "no token");
        assert!(!entry_allows(Some("wrong"), "", &p), "wrong token");
        assert!(!entry_allows(Some(&key[..KEY_CHARS - 1]), "", &p), "part of the key");
        assert!(entry_allows(Some(&key), "", &p), "the pairing key");
        assert!(entry_allows(Some("dev-test-token"), "dev-test-token", &p), "DEVICE_TOKEN");
        assert!(!entry_allows(Some("dev-test-token"), "", &p));
        assert!(!entry_allows(None, "dev-test-token", &p));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_survives_a_restart_and_pairing_again_replaces_it() {
        let dir = temp_dir("keep");
        let first = Pairing::open(&dir);
        assert_eq!(first.peek(), None, "made only when needed");
        assert!(!dir.join(KEY_FILE).exists());
        let key = first.key();
        assert!(valid_key(&key) && key.len() == KEY_CHARS, "{} characters", key.len());
        assert_eq!(first.key(), key, "same key on the next call");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let restarted = Pairing::open(&dir);
        assert_eq!(restarted.peek().as_deref(), Some(key.as_str()), "same key after a restart");
        assert!(restarted.accepts(Some(&key)));

        let kicked = restarted.kicked();
        restarted.renew().unwrap();
        assert!(kicked.has_changed().unwrap(), "the phones on Wi-Fi are dropped");
        let renewed = restarted.key();
        assert_ne!(renewed, key);
        assert!(!restarted.accepts(Some(&key)), "the old key stops working");
        assert!(restarted.accepts(Some(&renewed)));
        assert_eq!(Pairing::open(&dir).peek().as_deref(), Some(renewed.as_str()), "the new key is saved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_damaged_key_file_is_replaced() {
        let dir = temp_dir("bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(KEY_FILE), "short").unwrap();
        let p = Pairing::open(&dir);
        assert_eq!(p.peek(), None);
        assert!(!p.accepts(Some("short")));
        let key = p.key();
        assert_eq!(std::fs::read_to_string(dir.join(KEY_FILE)).unwrap(), key);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn wifi_addresses_ip_first_then_the_network_name() {
        let ip = Some(Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(
            wifi_urls(ip, 8788, "bageo", "k3y"),
            vec!["ws://192.168.1.20:8788/ws?token=k3y".to_string(), "ws://bageo.local:8788/ws?token=k3y".to_string()]
        );
        assert_eq!(wifi_urls(ip, 8788, "", "k3y"), vec!["ws://192.168.1.20:8788/ws?token=k3y".to_string()], "network name off");
        assert!(wifi_urls(None, 8788, "bageo", "k3y").is_empty(), "no LAN address");
    }
}

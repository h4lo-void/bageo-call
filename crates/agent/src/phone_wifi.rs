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
//!
//! A phone that cannot be plugged in (the agent runs in Docker or on another
//! computer) gets the key with a pairing code instead ([`PairCodes`]): the
//! console makes a 6-digit code, the owner types it with the agent's address
//! in the app, and the app trades it once for the key
//! (`POST /api/phone-pairing/claim`). It then connects to the address typed,
//! on `/ws` with the key.

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

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
    refresh: watch::Sender<u64>,
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

/// Constant-time comparison for pairing keys and device tokens.
pub fn same_token(a: &str, b: &str) -> bool {
    same(a, b)
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
            refresh: watch::channel(0).0,
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

    /// Read the key file again (a backup put it back). Another key: the phones
    /// on Wi-Fi are dropped and plugged-in phones get the new one. No key in
    /// the file: this one stays.
    pub fn reload(&self) {
        let key = std::fs::read_to_string(&self.path).ok().map(|s| s.trim().to_string()).filter(|s| valid_key(s));
        let changed = {
            let mut slot = self.slot();
            match key {
                Some(k) if slot.as_deref() != Some(k.as_str()) => {
                    *slot = Some(k);
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.kick();
            self.notify_refresh();
        }
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
        self.notify_refresh();
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

    /// Notify plugged-in USB phones to push updated `agent_info`.
    pub fn notify_refresh(&self) {
        self.refresh.send_modify(|n| *n = n.wrapping_add(1));
    }

    /// Changes when plugged-in USB phones should receive updated `agent_info`.
    pub fn refreshed(&self) -> watch::Receiver<u64> {
        self.refresh.subscribe()
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

/// How long a pairing code works.
pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);
/// Wrong tries that void the code in use.
pub const CODE_MAX_WRONG: u32 = 5;
/// Wrong tries per minute, from anywhere, after which every try is refused
/// until the minute is over.
pub const CLAIM_WRONG_PER_MINUTE: u32 = 20;
/// Digits in a pairing code.
pub const CODE_DIGITS: usize = 6;

/// Why a pairing code was not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// No code in use: none was made, it expired, it was used, or it was voided.
    NoCode,
    /// Not the code in use.
    Wrong,
    /// Too many wrong tries in the last minute.
    TooMany,
}

impl ClaimError {
    /// The `code` of the API's error answer.
    pub fn code(self) -> &'static str {
        match self {
            Self::NoCode => "no_code",
            Self::Wrong => "wrong_code",
            Self::TooMany => "too_many_tries",
        }
    }
}

struct PendingCode {
    code: String,
    made: Instant,
    wrong: u32,
}

#[derive(Default)]
struct CodesInner {
    pending: Option<PendingCode>,
    window_start: Option<Instant>,
    window_wrong: u32,
}

/// The pairing code the console made: one at a time, [`CODE_TTL`] long, good
/// once, voided after [`CODE_MAX_WRONG`] wrong tries. Never logged.
#[derive(Default)]
pub struct PairCodes {
    inner: Mutex<CodesInner>,
}

/// A random number below 10^[`CODE_DIGITS`], from the system's random source
/// (through uuid v4), without bias.
fn fresh_code() -> String {
    const SPAN: u32 = 1_000_000;
    const LIMIT: u32 = u32::MAX - (u32::MAX % SPAN);
    loop {
        let b = uuid::Uuid::new_v4().into_bytes();
        let n = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        if n < LIMIT {
            return format!("{:0width$}", n % SPAN, width = CODE_DIGITS);
        }
    }
}

impl PairCodes {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CodesInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A new code; the one before stops working.
    pub fn issue(&self, now: Instant) -> String {
        let code = fresh_code();
        self.lock().pending = Some(PendingCode { code: code.clone(), made: now, wrong: 0 });
        code
    }

    /// Use `code`: Ok once for the code in use, which then stops working.
    pub fn claim(&self, code: &str, now: Instant) -> Result<(), ClaimError> {
        let code = code.trim();
        let mut inner = self.lock();
        if inner.window_start.is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(60)) {
            inner.window_start = Some(now);
            inner.window_wrong = 0;
        }
        if inner.window_wrong >= CLAIM_WRONG_PER_MINUTE {
            return Err(ClaimError::TooMany);
        }
        if inner.pending.as_ref().is_some_and(|p| now.saturating_duration_since(p.made) >= CODE_TTL) {
            inner.pending = None;
        }
        let Some(pending) = inner.pending.as_mut() else {
            inner.window_wrong += 1;
            return Err(ClaimError::NoCode);
        };
        if same(code, &pending.code) {
            inner.pending = None;
            return Ok(());
        }
        pending.wrong += 1;
        if pending.wrong >= CODE_MAX_WRONG {
            inner.pending = None;
        }
        inner.window_wrong += 1;
        Err(ClaimError::Wrong)
    }
}

static CODES: OnceLock<PairCodes> = OnceLock::new();

/// The agent's pairing code.
pub fn codes() -> &'static PairCodes {
    CODES.get_or_init(PairCodes::new)
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
        let refreshed = restarted.refreshed();
        restarted.renew().unwrap();
        assert!(kicked.has_changed().unwrap(), "the phones on Wi-Fi are dropped");
        assert!(refreshed.has_changed().unwrap(), "USB phones receive the new pairing");
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

    #[test]
    fn a_pairing_code_is_six_digits_and_works_once() {
        let codes = PairCodes::new();
        let t0 = Instant::now();
        let code = codes.issue(t0);
        assert_eq!(code.len(), CODE_DIGITS);
        assert!(code.bytes().all(|b| b.is_ascii_digit()), "{code:?}");
        assert_eq!(codes.claim(&format!(" {code} "), t0), Ok(()), "blanks around it are fine");
        assert_eq!(codes.claim(&code, t0), Err(ClaimError::NoCode), "used: it stops working");
    }

    #[test]
    fn a_pairing_code_expires_after_ten_minutes() {
        let codes = PairCodes::new();
        let t0 = Instant::now();
        let code = codes.issue(t0);
        assert_eq!(codes.claim(&code, t0 + CODE_TTL), Err(ClaimError::NoCode));
        let code = codes.issue(t0);
        assert_eq!(codes.claim(&code, t0 + CODE_TTL - Duration::from_secs(1)), Ok(()), "still good just before");
    }

    #[test]
    fn five_wrong_tries_void_the_code() {
        let codes = PairCodes::new();
        let t0 = Instant::now();
        let code = codes.issue(t0);
        let wrong = if code == "000000" { "000001" } else { "000000" };
        for _ in 0..CODE_MAX_WRONG {
            assert_eq!(codes.claim(wrong, t0), Err(ClaimError::Wrong));
        }
        assert_eq!(codes.claim(&code, t0), Err(ClaimError::NoCode), "voided: even the right code is refused");
        let code = codes.issue(t0);
        assert_eq!(codes.claim(&code, t0), Ok(()), "a new code works");
    }

    #[test]
    fn a_new_code_replaces_the_old_one() {
        let codes = PairCodes::new();
        let t0 = Instant::now();
        let old = codes.issue(t0);
        let mut new = codes.issue(t0);
        while new == old {
            new = codes.issue(t0);
        }
        assert_eq!(codes.claim(&old, t0), Err(ClaimError::Wrong), "the old one no longer works");
        assert_eq!(codes.claim(&new, t0), Ok(()));
    }

    #[test]
    fn too_many_wrong_tries_a_minute_stop_every_try() {
        let codes = PairCodes::new();
        let t0 = Instant::now();
        for _ in 0..CLAIM_WRONG_PER_MINUTE {
            assert_eq!(codes.claim("123456", t0), Err(ClaimError::NoCode));
        }
        let code = codes.issue(t0);
        assert_eq!(codes.claim(&code, t0), Err(ClaimError::TooMany), "held for the rest of the minute");
        assert_eq!(codes.claim(&code, t0 + Duration::from_secs(60)), Ok(()), "the next minute it works");
    }
}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The phone link check: the phone app and this agent show each other that they
//! hold the same link key before the phone says anything or the agent trusts it.
//!
//! The key is 32 random bytes written as 64 lowercase hex characters; the
//! HMAC key is that text. A desktop agent keeps it in [`KEY_FILE`] in the call
//! data folder (mode 600 on Unix) and hands it to a plugged-in phone together
//! with the start of its call service (`usb_bridge`); "Pair phones again" in
//! the console replaces it. The standalone edition uses the bridge key made at
//! each start instead (`embedded`), which never leaves memory.
//!
//! On each phone WebSocket:
//! 1. the phone sends [`HEADER_LINK`] `1` and [`HEADER_CNONCE`] (32 random bytes, hex);
//! 2. the agent's 101 answer carries [`HEADER_SNONCE`] and [`HEADER_PROOF`],
//!    `HMAC-SHA256(K, "bageo-link-v1|host|" + cnonce + "|" + snonce)`;
//! 3. the phone checks it and otherwise closes without a word;
//! 4. the phone's `hello` carries `link_proof`,
//!    `HMAC-SHA256(K, "bageo-link-v1|phone|" + cnonce + "|" + snonce)`, which
//!    the agent checks before it registers the phone.
//!
//! `PHONE_LINK_AUTH` picks what happens to a phone on this computer that does
//! not take part (an app from before the check): `compat` (the default) lets it
//! connect without trust, `required` refuses it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// The file in the call data folder that holds the link key.
pub const KEY_FILE: &str = "phone-link.key";

pub const HEADER_LINK: &str = "x-bageo-link";
pub const HEADER_CNONCE: &str = "x-bageo-link-cnonce";
pub const HEADER_SNONCE: &str = "x-bageo-link-snonce";
pub const HEADER_PROOF: &str = "x-bageo-link-proof";

const LABEL_HOST: &str = "bageo-link-v1|host|";
const LABEL_PHONE: &str = "bageo-link-v1|phone|";

/// What a phone on this computer that skips the check gets (`PHONE_LINK_AUTH`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// It connects, but is not trusted: no Wi-Fi addresses, no pairing key.
    Compat,
    /// It is refused.
    Required,
}

impl Mode {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "required" | "require" | "strict" | "on" => Mode::Required,
            _ => Mode::Compat,
        }
    }
}

/// `PHONE_LINK_AUTH` now (`compat` unless set to `required`).
pub fn mode() -> Mode {
    Mode::parse(&std::env::var("PHONE_LINK_AUTH").unwrap_or_default())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim().as_bytes();
    if s.len() != 64 {
        return None;
    }
    let digit = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 32];
    for (i, pair) in s.chunks(2).enumerate() {
        out[i] = (digit(pair[0])? << 4) | digit(pair[1])?;
    }
    Some(out)
}

fn random32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    if getrandom::getrandom(&mut bytes).is_err() {
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    bytes
}

/// A new link key: 32 random bytes as hex.
pub fn fresh_key() -> String {
    hex(&random32())
}

/// 64 hex characters.
pub fn valid_key(s: &str) -> bool {
    unhex32(s).is_some()
}

fn mac(key: &str, label: &str, cnonce: &[u8; 32], snonce: &[u8; 32]) -> [u8; 32] {
    let mut m = Hmac::<Sha256>::new_from_slice(key.trim().as_bytes()).expect("HMAC accepts any key length");
    m.update(label.as_bytes());
    m.update(hex(cnonce).as_bytes());
    m.update(b"|");
    m.update(hex(snonce).as_bytes());
    let mut out = [0u8; 32];
    out.copy_from_slice(&m.finalize().into_bytes());
    out
}

fn same32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The phone's nonce from its request, when it asked for the check.
pub fn requested_cnonce(headers: &axum::http::HeaderMap) -> Option<[u8; 32]> {
    let asked = headers
        .get(HEADER_LINK)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "1");
    if !asked {
        return None;
    }
    headers.get(HEADER_CNONCE).and_then(|v| v.to_str().ok()).and_then(unhex32)
}

/// One connection's check, started when the agent answers the upgrade. Only
/// what the phone must answer is kept, never the key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handshake {
    expected_phone: [u8; 32],
}

/// What goes into the 101 answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostAnswer {
    pub snonce: String,
    pub proof: String,
}

impl Handshake {
    /// Start the check for the phone's `cnonce` with `key` (a fresh server nonce).
    pub fn start(key: &str, cnonce: &[u8; 32]) -> (Self, HostAnswer) {
        Self::start_with(key, cnonce, &random32())
    }

    pub fn start_with(key: &str, cnonce: &[u8; 32], snonce: &[u8; 32]) -> (Self, HostAnswer) {
        let host = mac(key, LABEL_HOST, cnonce, snonce);
        let phone = mac(key, LABEL_PHONE, cnonce, snonce);
        (
            Self { expected_phone: phone },
            HostAnswer { snonce: hex(snonce), proof: hex(&host) },
        )
    }

    /// The hello's `link_proof` is the right one.
    pub fn accepts(&self, proof: Option<&str>) -> bool {
        proof.and_then(unhex32).is_some_and(|p| same32(&p, &self.expected_phone))
    }
}

/// The two proofs (host, phone) as the phone computes them.
#[cfg(test)]
pub fn proofs(key: &str, cnonce: &str, snonce: &str) -> Option<(String, String)> {
    let (c, s) = (unhex32(cnonce)?, unhex32(snonce)?);
    Some((hex(&mac(key, LABEL_HOST, &c, &s)), hex(&mac(key, LABEL_PHONE, &c, &s))))
}

/// Put the agent's half of the check into the 101 answer.
pub fn add_answer(res: &mut axum::response::Response, answer: &HostAnswer) {
    use axum::http::HeaderValue;
    let h = res.headers_mut();
    if let (Ok(s), Ok(p)) = (HeaderValue::from_str(&answer.snonce), HeaderValue::from_str(&answer.proof)) {
        h.insert(HEADER_SNONCE, s);
        h.insert(HEADER_PROOF, p);
    }
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

/// The desktop agent's link key.
pub struct LinkKey {
    path: PathBuf,
    key: Mutex<Option<String>>,
    generation: AtomicU64,
    renewed: tokio::sync::Notify,
}

impl LinkKey {
    pub fn open(dir: &Path) -> Self {
        let path = dir.join(KEY_FILE);
        let key = std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| valid_key(s));
        #[cfg(unix)]
        if key.is_some() {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Self { path, key: Mutex::new(key), generation: AtomicU64::new(0), renewed: tokio::sync::Notify::new() }
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
                "phone link: could not save the key in {} ({e}); phones get a new one after a restart",
                self.path.display()
            );
        }
        *slot = Some(k.clone());
        k
    }

    /// A new key replaces the old one; plugged-in phones are given it again.
    pub fn renew(&self) -> std::io::Result<()> {
        let k = fresh_key();
        write_key(&self.path, &k)?;
        *self.slot() = Some(k);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.renewed.notify_waiters();
        Ok(())
    }

    /// Changes each time the key is replaced.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Woken when the key is replaced (the USB side hands it over at once).
    pub fn renewed(&self) -> &tokio::sync::Notify {
        &self.renewed
    }
}

static LINK: OnceLock<LinkKey> = OnceLock::new();

/// Use the call data folder `dir` (at start, before the first phone connects).
pub fn init(dir: &Path) -> &'static LinkKey {
    LINK.get_or_init(|| LinkKey::open(dir))
}

/// The agent's link key ([`init`]; else the folder `CALLS_DATA_DIR` names).
pub fn link() -> &'static LinkKey {
    LINK.get_or_init(|| {
        if cfg!(test) {
            return LinkKey::open(&std::env::temp_dir().join(format!("bageo-link-{}", uuid::Uuid::new_v4().simple())));
        }
        let default_dir = if Path::new("/data/calls").is_dir() { "/data/calls" } else { "data/calls" };
        LinkKey::open(Path::new(&crate::config::env_or("CALLS_DATA_DIR", default_dir)))
    })
}

/// A connection-specific link key for the phone WebSocket (the standalone
/// bridge key); without it the agent's own [`link`] key is used.
#[derive(Clone)]
pub struct KeyOverride(pub std::sync::Arc<str>);

/// Phones whose control connection passed the check, by serial: a connection
/// that did not pass may not take their place.
#[derive(Default)]
pub struct Holders {
    inner: Mutex<HashMap<String, u64>>,
    next: AtomicU64,
}

impl Holders {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, u64>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A checked connection holds `serial` now. Drop the guard when it closes.
    pub fn hold(&'static self, serial: &str) -> HoldGuard {
        let id = self.next.fetch_add(1, Ordering::AcqRel) + 1;
        self.lock().insert(serial.to_string(), id);
        HoldGuard { holders: self, serial: serial.to_string(), id }
    }

    pub fn is_held(&self, serial: &str) -> bool {
        self.lock().contains_key(serial)
    }
}

/// Releases a serial when its checked connection ends (unless a newer one took it).
pub struct HoldGuard {
    holders: &'static Holders,
    serial: String,
    id: u64,
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        let mut g = self.holders.lock();
        if g.get(&self.serial) == Some(&self.id) {
            g.remove(&self.serial);
        }
    }
}

pub fn holders() -> &'static Holders {
    static H: OnceLock<Holders> = OnceLock::new();
    H.get_or_init(Holders::default)
}

/// May a phone `hello` for `serial` go ahead: a connection that passed the
/// check always; one that did not only while no checked connection holds it.
pub fn may_take(serial: &str, verified: bool, holders: &Holders) -> bool {
    verified || !holders.is_held(serial)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("bageo-link-{tag}-{}", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn link_key_is_saved_private_and_renew_replaces_it() {
        let dir = temp_dir("keep");
        let first = LinkKey::open(&dir);
        let key = first.key();
        assert!(valid_key(&key) && key.len() == 64);
        assert_eq!(first.key(), key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(KEY_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let again = LinkKey::open(&dir);
        assert_eq!(again.key(), key, "same key after a restart");
        let g = again.generation();
        again.renew().unwrap();
        assert_ne!(again.key(), key);
        assert_eq!(again.generation(), g + 1);
        assert_eq!(LinkKey::open(&dir).key(), again.key(), "the new key is saved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn link_renew_wakes_the_usb_side_at_once() {
        let dir = temp_dir("wake");
        let link = LinkKey::open(&dir);
        let notified = link.renewed().notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        link.renew().unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_secs(2), notified).await.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn link_handshake_proofs_match_both_ways() {
        let key = fresh_key();
        let cnonce = random32();
        let snonce = random32();
        let (hs, answer) = Handshake::start_with(&key, &cnonce, &snonce);
        let (host, phone) = proofs(&key, &hex(&cnonce), &answer.snonce).unwrap();
        assert_eq!(answer.proof, host);
        assert!(hs.accepts(Some(&phone)));
        assert!(hs.accepts(Some(&phone.to_ascii_uppercase())), "hex in any case");
        assert!(!hs.accepts(Some(&host)), "the host proof is not the phone proof");
        assert!(!hs.accepts(None));
        assert!(!hs.accepts(Some("")));
        assert!(!hs.accepts(Some(&phone[..63])));
        let (_, other) = proofs(&fresh_key(), &hex(&cnonce), &answer.snonce).unwrap();
        assert!(!hs.accepts(Some(&other)), "another key");
        // Known answer, the same as the app's PhoneLinkAuthTest.
        let k = "00".repeat(32);
        let (h, p) = proofs(&k, &"11".repeat(32), &"22".repeat(32)).unwrap();
        assert_eq!(h, "edae13eb159e4215e1f2e7735b5445e0b748df7a1abc3f26f2f713eee7d9bb58");
        assert_eq!(p, "0cd686cff360ad2b0db8c08f5ebb6d0116f1dfb408c127af87f1d7725ac73b8a");
    }

    #[test]
    fn link_cnonce_is_read_only_when_asked_for() {
        let mut h = axum::http::HeaderMap::new();
        assert_eq!(requested_cnonce(&h), None);
        h.insert(HEADER_CNONCE, "ab".repeat(32).parse().unwrap());
        assert_eq!(requested_cnonce(&h), None, "without x-bageo-link: 1");
        h.insert(HEADER_LINK, "1".parse().unwrap());
        assert_eq!(requested_cnonce(&h), Some([0xab; 32]));
        h.insert(HEADER_CNONCE, "ab".repeat(31).parse().unwrap());
        assert_eq!(requested_cnonce(&h), None, "too short");
        h.insert(HEADER_CNONCE, "zz".repeat(32).parse().unwrap());
        assert_eq!(requested_cnonce(&h), None, "not hex");
    }

    #[test]
    fn link_mode_is_compat_unless_required() {
        assert_eq!(Mode::parse(""), Mode::Compat);
        assert_eq!(Mode::parse("compat"), Mode::Compat);
        assert_eq!(Mode::parse(" Required "), Mode::Required);
        assert_eq!(Mode::parse("bogus"), Mode::Compat);
    }

    #[test]
    fn link_unchecked_hello_cannot_take_a_checked_serial() {
        static H: OnceLock<Holders> = OnceLock::new();
        let h = H.get_or_init(Holders::default);
        assert!(may_take("pixel-a", false, h), "nobody holds it");
        let guard = h.hold("pixel-a");
        assert!(!may_take("pixel-a", false, h), "an unchecked connection is refused");
        assert!(may_take("pixel-a", true, h), "a checked one takes over");
        assert!(may_take("pixel-b", false, h));
        let newer = h.hold("pixel-a");
        drop(guard);
        assert!(h.is_held("pixel-a"), "the older guard does not release the newer hold");
        drop(newer);
        assert!(may_take("pixel-a", false, h));
    }
}

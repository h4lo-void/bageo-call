// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The one-time keys of one helper run.
//!
//! Each time the helper starts it draws two random 256-bit keys and writes them to
//! `files/mode/audio_helper_key` in the app's device-protected folder: owned by the
//! app, mode 0600, with that folder's SELinux label (which carries the app's own
//! categories), so only root and a process running as the app, in the app's SELinux
//! context, can read it. The file is removed when the helper quits.
//!
//! - The app proves it is the app: the first request on every connection, control or
//!   PCM, is `auth` with the app key. Anything else, or a wrong key, ends the
//!   connection. A process that merely holds the app's user ID but runs in another
//!   SELinux context (for example `run-as`, or another app sharing the ID) cannot read
//!   the file, so a user-ID check alone would have let it in and the key does not.
//! - The helper proves it is the helper: its hello carries the helper key. Only root or
//!   the app could know it, and the helper only sends it to a listener it has already
//!   checked (the app's user ID and SELinux context), so the app can tell a fake helper
//!   (some other process connecting to its socket) apart.
//!
//! Neither key leaves the phone or is logged. A new run means new keys; an old key is
//! worthless after the helper that made it quits.

pub const KEY_LEN: usize = 32;
pub const KEY_FILE: &str = "audio_helper_key";
const FILE_HEADER: &str = "bageo-audio-helper-keys 1";

#[derive(Clone)]
pub struct Keys {
    pub app: [u8; KEY_LEN],
    pub helper: [u8; KEY_LEN],
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn unhex_key(s: &str) -> Option<[u8; KEY_LEN]> {
    if s.len() != 2 * KEY_LEN || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut out = [0u8; KEY_LEN];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Compares in time that does not depend on where the first difference is.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Keys {
    pub fn from_random(r: [u8; 2 * KEY_LEN]) -> Keys {
        let mut app = [0u8; KEY_LEN];
        let mut helper = [0u8; KEY_LEN];
        app.copy_from_slice(&r[..KEY_LEN]);
        helper.copy_from_slice(&r[KEY_LEN..]);
        Keys { app, helper }
    }

    /// The key file's contents.
    pub fn file_text(&self) -> String {
        format!("{FILE_HEADER}\napp={}\nhelper={}\n", hex(&self.app), hex(&self.helper))
    }

    /// Reads a key file (what the app does).
    pub fn parse_file(text: &str) -> Option<Keys> {
        let mut lines = text.lines();
        if lines.next()? != FILE_HEADER {
            return None;
        }
        let app = unhex_key(lines.next()?.strip_prefix("app=")?)?;
        let helper = unhex_key(lines.next()?.strip_prefix("helper=")?)?;
        Some(Keys { app, helper })
    }

    /// Whether `presented` (hex) is the app key.
    pub fn app_key_ok(&self, presented: &str) -> bool {
        match unhex_key(presented) {
            Some(k) => ct_eq(&k, &self.app),
            None => false,
        }
    }

    pub fn helper_hex(&self) -> String {
        hex(&self.helper)
    }
}

#[cfg(test)]
pub(crate) fn test_keys() -> Keys {
    let mut r = [0u8; 2 * KEY_LEN];
    for (i, b) in r.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(37).wrapping_add(11);
    }
    Keys::from_random(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_file_round_trip() {
        let k = test_keys();
        let back = Keys::parse_file(&k.file_text()).unwrap();
        assert_eq!(back.app, k.app);
        assert_eq!(back.helper, k.helper);
        assert_ne!(k.app, k.helper);
        assert!(Keys::parse_file("something\napp=00\n").is_none());
    }

    #[test]
    fn only_the_exact_app_key_passes() {
        let k = test_keys();
        assert!(k.app_key_ok(&hex(&k.app)));
        assert!(!k.app_key_ok(&hex(&k.helper)), "the helper key is not the app key");
        let mut wrong = k.app;
        wrong[31] ^= 1;
        assert!(!k.app_key_ok(&hex(&wrong)));
        assert!(!k.app_key_ok(""));
        assert!(!k.app_key_ok(&hex(&k.app)[..62]));
        assert!(!k.app_key_ok(&format!("{}zz", &hex(&k.app)[..62])));
        assert!(!ct_eq(b"ab", b"abc"));
        assert!(ct_eq(b"abc", b"abc"));
    }
}

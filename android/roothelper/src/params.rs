// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Audio HAL parameters (`AudioSystem.setParameters` / `getParameters`): which keys
//! and values are accepted, and how a reply is read.
//!
//! The HAL takes `key=value;key=value`. One key per request, and keys and values
//! limited to letters, digits and `_ . : , + -`, so a value can never smuggle in a
//! second pair.

pub const MAX_KEY_LEN: usize = 64;
pub const MAX_VALUE_LEN: usize = 128;

fn allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | ',' | '+' | '-')
}

pub fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= MAX_KEY_LEN && k.chars().all(allowed)
}

/// Values may be empty (some keys are switched off that way).
pub fn valid_value(v: &str) -> bool {
    v.len() <= MAX_VALUE_LEN && v.chars().all(allowed)
}

/// The value of `key` in a `getParameters` reply (`k=v;k2=v2`); `None` when the HAL
/// did not report the key.
pub fn value_in_reply(reply: &str, key: &str) -> Option<String> {
    reply
        .trim()
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_values() {
        assert!(valid_key("incall_music_enabled"));
        assert!(valid_key("Set_BGS_UL_Mute"));
        assert!(!valid_key(""));
        assert!(!valid_key("a=b"));
        assert!(!valid_key("a;b"));
        assert!(!valid_key("a b"));
        assert!(!valid_key(&"k".repeat(MAX_KEY_LEN + 1)));
        assert!(valid_value("11c05000"));
        assert!(valid_value(""));
        assert!(valid_value("true"));
        assert!(!valid_value("1;routing=2"));
        assert!(!valid_value("x\n"));
    }

    #[test]
    fn reads_replies() {
        assert_eq!(value_in_reply("incall_music_enabled=true", "incall_music_enabled"), Some("true".into()));
        assert_eq!(value_in_reply("a=1;b=2\n", "b"), Some("2".into()));
        assert_eq!(value_in_reply("a=", "a"), Some(String::new()));
        assert_eq!(value_in_reply("", "a"), None);
        assert_eq!(value_in_reply("ab=1", "a"), None);
    }
}

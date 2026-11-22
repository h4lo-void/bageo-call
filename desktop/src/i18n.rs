// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Texts the Rust side needs (tray tooltip and menu). Shared with the window UI.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::status::{Reason, Snapshot};

const RAW: &str = include_str!("../ui/i18n.json");

type Table = HashMap<String, HashMap<String, String>>;

fn base() -> &'static Table {
    static BASE: OnceLock<Table> = OnceLock::new();
    BASE.get_or_init(|| serde_json::from_str(RAW).unwrap_or_default())
}

fn extra_slot() -> &'static Mutex<Option<(String, HashMap<String, String>)>> {
    static EXTRA: OnceLock<Mutex<Option<(String, HashMap<String, String>)>>> = OnceLock::new();
    EXTRA.get_or_init(|| Mutex::new(None))
}

/// True when `code` looks like a standalone non-core language code safe to fetch on demand.
pub fn is_valid_ext_code(code: &str) -> bool {
    (2..=12).contains(&code.len())
        && code.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !code.ends_with('-')
        && !code.contains("--")
        && !base().contains_key(code)
}

/// Extracts the string map from `/calls/i18n/desktop/<code>.json` (`{"schema":1,"lang":"...","strings":{...}}`).
pub fn parse_desktop_payload(body: &serde_json::Value) -> HashMap<String, String> {
    let Some(obj) = body.get("strings").and_then(serde_json::Value::as_object) else {
        return HashMap::new();
    };
    obj.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

/// Replaces the currently loaded extra language strings (if `code` is valid and non-core).
pub fn set_extra(code: &str, strings: HashMap<String, String>) {
    if !is_valid_ext_code(code) {
        return;
    }
    *extra_slot().lock().unwrap_or_else(|e| e.into_inner()) = Some((code.to_string(), strings));
}

/// Returns the currently loaded extra `(code, strings)` pair, if any.
pub fn get_extra() -> Option<(String, HashMap<String, String>)> {
    extra_slot().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// True when the given non-core language is currently loaded in memory.
pub fn has_extra(code: &str) -> bool {
    matches!(&*extra_slot().lock().unwrap_or_else(|e| e.into_inner()), Some((c, _)) if c == code)
}

/// Returns `Some(code)` when `code` is currently loaded as the active extra language.
pub fn ext_code(code: &str) -> Option<String> {
    if has_extra(code) {
        Some(code.to_string())
    } else {
        None
    }
}

#[cfg(test)]
pub(crate) fn clear_extra() {
    *extra_slot().lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|e| e.into_inner())
}

/// Looks a key up in the given language, then in English, then returns the key itself.
pub fn t(lang: &str, key: &str) -> String {
    let b = base();
    if let Some(val) = b.get(lang).and_then(|m| m.get(key)) {
        return val.clone();
    }
    if !b.contains_key(lang) {
        if let Some((c, m)) = &*extra_slot().lock().unwrap_or_else(|e| e.into_inner()) {
            if c == lang {
                if let Some(val) = m.get(key) {
                    return val.clone();
                }
            }
        }
    }
    b.get("en").and_then(|m| m.get(key)).cloned().unwrap_or_else(|| key.to_string())
}

/// The sentence that explains the current state.
pub fn reason_text(s: &Snapshot) -> String {
    let lang = s.lang.as_str();
    match s.reason {
        Reason::Ready => t(lang, "r_ready").replace("{n}", &s.ready.to_string()),
        Reason::Busy => t(lang, "r_busy"),
        Reason::PhoneOffline => {
            if s.offline_count <= 1 {
                t(lang, "r_phone_offline").replace("{name}", &s.offline_name)
            } else {
                t(lang, "r_phones_offline").replace("{n}", &s.offline_count.to_string())
            }
        }
        Reason::AllNormal => t(lang, "r_all_normal"),
        Reason::NoPhone => t(lang, "r_no_phone"),
        Reason::Offline => t(lang, "r_offline"),
        Reason::Starting => t(lang, "r_starting"),
        Reason::Launching => t(lang, "r_launching"),
        Reason::LaunchFailed => t(lang, "r_launch_failed"),
        Reason::NeedAdb => t(lang, "r_need_adb"),
        Reason::Setup => t(lang, "r_setup").replace("{n}", &s.setup_todo.to_string()),
        Reason::Found => t(lang, "r_found").replace("{address}", &crate::status::short_address(&s.candidate)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Level, LANGS};

    const KEYS: [&str; 19] = [
        "r_ready",
        "r_busy",
        "r_phone_offline",
        "r_phones_offline",
        "r_all_normal",
        "r_no_phone",
        "r_offline",
        "r_starting",
        "r_launching",
        "r_launch_failed",
        "r_need_adb",
        "r_setup",
        "r_found",
        "found_hint",
        "connect",
        "menu_show",
        "open",
        "quit",
        "quit_all",
    ];

    #[test]
    fn every_language_has_every_key() {
        for lang in LANGS {
            for key in KEYS {
                let v = base().get(lang).and_then(|m| m.get(key));
                assert!(v.map(|s| !s.is_empty()).unwrap_or(false), "{lang} is missing {key}");
            }
        }
    }

    #[test]
    fn reason_text_fills_placeholders() {
        let mut s = Snapshot::blank(Level::Ok, Reason::Ready, "en", false, "");
        s.ready = 2;
        assert_eq!(reason_text(&s), "Phones ready: 2");

        s.level = Level::Warn;
        s.reason = Reason::PhoneOffline;
        s.offline_count = 1;
        s.offline_name = "Pixel A".into();
        assert_eq!(reason_text(&s), "Pixel A is not connected");
        s.offline_count = 3;
        assert_eq!(reason_text(&s), "3 phones are not connected");

        s.lang = "zh-TW".into();
        assert!(reason_text(&s).contains('3'));
        assert!(!reason_text(&s).contains("{n}"));
    }

    #[test]
    fn found_text_names_the_address_in_every_language() {
        let mut s = Snapshot::found("en", false, "http://bageo.local/");
        for lang in LANGS {
            s.lang = lang.to_string();
            let text = reason_text(&s);
            assert!(text.contains("bageo.local"), "{lang}: {text}");
            assert!(!text.contains('{') && !text.contains("http://"), "{lang}: {text}");
        }
    }

    #[test]
    fn unknown_language_and_key_fall_back() {
        assert_eq!(t("xx", "quit"), t("en", "quit"));
        assert_eq!(t("en", "no_such_key"), "no_such_key");
    }

    #[test]
    fn valid_ext_code_rejects_core_and_unsafe_names() {
        assert!(is_valid_ext_code("it"));
        assert!(is_valid_ext_code("fil"));
        assert!(is_valid_ext_code("zh-hans"));
        assert!(!is_valid_ext_code("en"));
        assert!(!is_valid_ext_code("ja"));
        assert!(!is_valid_ext_code("zh-TW"));
        assert!(!is_valid_ext_code("../it"));
        assert!(!is_valid_ext_code("a"));
        assert!(!is_valid_ext_code("-it"));
        assert!(!is_valid_ext_code("it-"));
    }

    #[test]
    fn extra_language_text_loads_on_demand_and_never_overrides_nine() {
        let _g = test_lock();
        clear_extra();
        assert_eq!(ext_code("it"), None);
        let mut it = HashMap::new();
        it.insert("quit".to_string(), "Esci".to_string());
        set_extra("it", it);
        // Core languages cannot be registered as extra.
        let mut ja = HashMap::new();
        ja.insert("quit".to_string(), "X".to_string());
        set_extra("ja", ja);
        assert_eq!(ext_code("it").as_deref(), Some("it"));
        assert_eq!(ext_code("ja"), None);
        assert_eq!(t("it", "quit"), "Esci");
        assert_eq!(t("ja", "quit"), base()["ja"]["quit"]);
        // A key the extra language lacks falls back to English.
        assert_eq!(t("it", "lang_unreviewed"), t("en", "lang_unreviewed"));
        let payload = serde_json::json!({
            "schema": 1,
            "lang": "it",
            "strings": { "h_ok": "Tutto pronto", "quit": "Esci" }
        });
        let parsed = parse_desktop_payload(&payload);
        assert_eq!(parsed.get("h_ok").map(String::as_str), Some("Tutto pronto"));
        assert_eq!(parsed.get("quit").map(String::as_str), Some("Esci"));
        assert!(!parsed.contains_key("lang"));
        clear_extra();
    }

    #[test]
    fn every_built_in_language_says_its_text_waits_for_a_native_speaker() {
        for lang in LANGS {
            assert!(base()[lang].get("lang_unreviewed").is_some_and(|s| !s.is_empty()), "{lang}");
        }
    }
}

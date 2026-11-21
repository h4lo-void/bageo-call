// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Texts the Rust side needs (tray tooltip and menu). Shared with the window UI.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::status::{Reason, Snapshot};

const RAW: &str = include_str!("../ui/i18n.json");
/// The languages `packs export-app` writes (see tools/bageo-packs.ref). Their text
/// still waits for a native speaker to confirm, so the nine built-in languages win on any clash.
const EXT_RAW: &str = include_str!("../ui/i18n.ext.json");

type Table = HashMap<String, HashMap<String, String>>;

fn base() -> &'static Table {
    static BASE: OnceLock<Table> = OnceLock::new();
    BASE.get_or_init(|| serde_json::from_str(RAW).unwrap_or_default())
}

fn ext() -> &'static Table {
    static EXT: OnceLock<Table> = OnceLock::new();
    EXT.get_or_init(|| serde_json::from_str(EXT_RAW).unwrap_or_default())
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut all = base().clone();
        for (code, strings) in ext() {
            all.entry(code.clone()).or_insert_with(|| strings.clone());
        }
        all
    })
}

/// The code of an extra (not yet confirmed) language the program has text for.
pub fn ext_code(code: &str) -> Option<&'static str> {
    let (k, _) = ext().get_key_value(code)?;
    (!base().contains_key(code)).then_some(k.as_str())
}

/// Looks a key up in the given language, then in English, then returns the key itself.
pub fn t(lang: &str, key: &str) -> String {
    let t = table();
    t.get(lang)
        .and_then(|m| m.get(key))
        .or_else(|| t.get("en").and_then(|m| m.get(key)))
        .cloned()
        .unwrap_or_else(|| key.to_string())
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Level, LANGS};

    const KEYS: [&str; 16] = [
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
        "menu_show",
        "open",
        "quit",
        "quit_all",
    ];

    #[test]
    fn every_language_has_every_key() {
        for lang in LANGS {
            for key in KEYS {
                let v = table().get(lang).and_then(|m| m.get(key));
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
    fn unknown_language_and_key_fall_back() {
        assert_eq!(t("xx", "quit"), t("en", "quit"));
        assert_eq!(t("en", "no_such_key"), "no_such_key");
    }

    #[test]
    fn extra_languages_come_from_the_export_and_keep_to_the_english_keys() {
        let en = &base()["en"];
        assert_eq!(ext().len(), 20, "the export carries 20 languages");
        for (lang, strings) in ext() {
            assert!(!base().contains_key(lang), "{lang} is one of the nine");
            for key in KEYS {
                assert!(strings.get(key).is_some_and(|s| !s.is_empty()), "{lang} is missing {key}");
            }
            for (key, value) in strings {
                let english = en.get(key).unwrap_or_else(|| panic!("{lang} has {key}, English does not"));
                for ph in ["{n}", "{name}"] {
                    assert_eq!(value.contains(ph), english.contains(ph), "{lang} {key} placeholder {ph}");
                }
            }
        }
    }

    #[test]
    fn extra_language_text_is_found_and_the_nine_are_not_taken_over() {
        assert_eq!(ext_code("it"), Some("it"));
        assert_eq!(ext_code("ja"), None);
        assert_eq!(ext_code("xx"), None);
        assert_ne!(t("it", "quit"), t("en", "quit"));
        assert_eq!(t("ja", "quit"), base()["ja"]["quit"]);
        // A key the extra language lacks falls back to English.
        assert_eq!(t("it", "lang_unreviewed"), t("en", "lang_unreviewed"));
    }

    #[test]
    fn every_built_in_language_says_its_text_waits_for_a_native_speaker() {
        for lang in LANGS {
            assert!(base()[lang].get("lang_unreviewed").is_some_and(|s| !s.is_empty()), "{lang}");
        }
    }
}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Texts the Rust side needs (tray tooltip and menu). Shared with the window UI.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::status::{Reason, Snapshot};

const RAW: &str = include_str!("../ui/i18n.json");

type Table = HashMap<String, HashMap<String, String>>;

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| serde_json::from_str(RAW).unwrap_or_default())
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
}

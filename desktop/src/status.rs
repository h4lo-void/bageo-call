// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Pure logic: Bageo JSON answers in, one Snapshot out. No I/O here.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;

pub const LANGS: [&str; 9] = ["en", "zh-TW", "zh-CN", "ja", "ko", "es", "fr", "de", "pt"];

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    Warn,
    Bad,
    Unknown,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Ready,
    Busy,
    PhoneOffline,
    AllNormal,
    NoPhone,
    Offline,
    Starting,
    /// This program started Bageo and is waiting for it to answer.
    Launching,
    /// Bageo did not come up, or kept stopping.
    LaunchFailed,
    /// No phone, and the Android tools (adb) are missing on this computer.
    NeedAdb,
    /// Bageo answers, but its console still lists settings to fill in.
    Setup,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum PhoneState {
    Ready,
    Call,
    Normal,
    Offline,
}

impl PhoneState {
    fn rank(self) -> u8 {
        match self {
            PhoneState::Call => 0,
            PhoneState::Ready => 1,
            PhoneState::Normal => 2,
            PhoneState::Offline => 3,
        }
    }
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Phone {
    pub serial: String,
    pub name: String,
    pub state: PhoneState,
}

#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct Snapshot {
    pub level: Level,
    pub reason: Reason,
    pub ready: u32,
    pub calls: u32,
    pub offline_count: u32,
    pub offline_name: String,
    pub locked: bool,
    pub has_password: bool,
    pub address: String,
    pub version: String,
    pub lang: String,
    pub phones: Vec<Phone>,
    /// Whether this Bageo can report text messages: null = not known, false = too old.
    pub sms_supported: Option<bool>,
    /// True when this program started Bageo and will stop it on quit.
    pub local: bool,
    /// Android tools on this computer: "" (fine or not our business), "missing", "downloading" or "failed".
    pub adb: String,
    /// How many items the console's "Ready?" list still asks for (0 for an older Bageo).
    pub setup_todo: u32,
}

impl Snapshot {
    pub fn blank(level: Level, reason: Reason, lang: &str, has_password: bool, address: &str) -> Self {
        Snapshot {
            level,
            reason,
            ready: 0,
            calls: 0,
            offline_count: 0,
            offline_name: String::new(),
            locked: false,
            has_password,
            address: address.to_string(),
            version: String::new(),
            lang: lang.to_string(),
            phones: Vec::new(),
            sms_supported: None,
            local: false,
            adb: String::new(),
            setup_todo: 0,
        }
    }
}

/// What the devices request returned.
pub enum DevicesResult {
    /// HTTP 200 with a JSON body.
    Detail(Value),
    /// HTTP 401: the console wants a password.
    Locked,
    /// Anything else: no detail, but not a password problem.
    Unavailable,
}

/// Maps a system locale such as "zh_Hant_TW" or "pt-BR" to one of the nine UI languages.
pub fn locale_to_lang(locale: &str) -> &'static str {
    let norm = locale.trim().replace('_', "-").to_lowercase();
    let mut parts = norm.split(|c| c == '-' || c == '.' || c == '@');
    match parts.next().unwrap_or("") {
        "zh" => {
            let traditional = parts.any(|p| matches!(p, "hant" | "tw" | "hk" | "mo"));
            if traditional {
                "zh-TW"
            } else {
                "zh-CN"
            }
        }
        "ja" => "ja",
        "ko" => "ko",
        "es" => "es",
        "fr" => "fr",
        "de" => "de",
        "pt" => "pt",
        _ => "en",
    }
}

/// Picks the UI language: Bageo's own notify language first, then the system language.
pub fn resolve_lang(notify_language: Option<&str>, system_locale: &str) -> &'static str {
    if let Some(n) = notify_language {
        if let Some(found) = LANGS.iter().find(|l| **l == n) {
            return found;
        }
    }
    locale_to_lang(system_locale)
}

/// True for addresses that start with http:// or https://.
pub fn is_web_url(s: &str) -> bool {
    let l = s.trim().to_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// The console address Bageo reports about itself in its full health
/// (`console_url`; older versions only `calls_ui_url`). Empty when there is none.
pub fn reported_console_url(health: &Value) -> String {
    ["console_url", "calls_ui_url"]
        .iter()
        .map(|k| text(health.get(*k)).trim())
        .find(|s| is_web_url(s))
        .unwrap_or("")
        .to_string()
}

/// The address to show, copy and open: the one Bageo reports once it opened
/// on this computer (`reachable`), else the address used for polling.
pub fn shown_address(base: &str, reported: &str, reachable: bool) -> String {
    if reachable && is_web_url(reported) {
        reported.trim().to_string()
    } else {
        base.to_string()
    }
}

/// Failed checks in a row before a reported address that opened here before
/// stops being shown (a `.local` name is sometimes slow to look up).
pub const UI_URL_KEEP_MISSES: u32 = 3;

/// After one check of the console address Bageo reports (`opens`: it answered
/// from this computer just now): the address to show (empty = the polling
/// address) and the failed checks so far. An address that is already shown
/// stays through up to `UI_URL_KEEP_MISSES - 1` failed checks of that same
/// address, so the window does not flip between it and 127.0.0.1.
pub fn next_ui_url(current: &str, misses: u32, reported: &str, opens: bool) -> (String, u32) {
    if opens {
        return (reported.trim().to_string(), 0);
    }
    let same = !current.is_empty() && current.trim() == reported.trim();
    if same && misses + 1 < UI_URL_KEEP_MISSES {
        (current.to_string(), misses + 1)
    } else {
        (String::new(), 0)
    }
}

/// Turns what the user typed into a base address with a trailing slash.
/// A pasted console link ("…/calls/", with or without "?…" or "#…") is cut back to the
/// address in front of "/calls", because every request adds "calls/…" itself.
/// Returns None when it is empty or does not start with http:// or https://.
pub fn normalize_address(raw: &str) -> Option<String> {
    let t = raw.trim();
    let t = t.split(['?', '#']).next().unwrap_or("");
    let lower = t.to_lowercase();
    let scheme_len = if lower.starts_with("http://") {
        7
    } else if lower.starts_with("https://") {
        8
    } else {
        return None;
    };
    let (scheme, rest) = t.split_at(scheme_len);
    let mut rest = rest.trim_end_matches('/');
    if rest.len() > 6 && rest[rest.len() - 6..].eq_ignore_ascii_case("/calls") {
        rest = &rest[..rest.len() - 6];
    }
    if rest.is_empty() || rest.starts_with('/') {
        return None;
    }
    Some(format!("{scheme}{rest}/"))
}

pub fn count(v: Option<&Value>) -> u32 {
    match v {
        Some(x) => x
            .as_u64()
            .map(|n| n.min(u32::MAX as u64) as u32)
            .or_else(|| x.as_f64().filter(|f| *f >= 0.0).map(|f| f as u32))
            .unwrap_or(0),
        None => 0,
    }
}

fn text(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).map(str::trim).unwrap_or("")
}

/// Builds the Snapshot from a successful /health answer and the devices answer.
pub fn build_snapshot(
    health: &Value,
    devices: &DevicesResult,
    has_password: bool,
    address: &str,
    system_locale: &str,
) -> Snapshot {
    let lang = resolve_lang(health.get("notify_language").and_then(Value::as_str), system_locale);
    let mut snap = Snapshot::blank(Level::Ok, Reason::Ready, lang, has_password, address);
    snap.version = text(health.get("version")).to_string();
    snap.locked = matches!(devices, DevicesResult::Locked);

    match devices {
        DevicesResult::Detail(d) => apply_detail(&mut snap, d),
        _ => {
            snap.ready = count(health.get("control_idle_clients"));
            snap.calls = count(health.get("active_call_count"));
            let (level, reason) = if snap.calls > 0 {
                (Level::Ok, Reason::Busy)
            } else if snap.ready > 0 {
                (Level::Ok, Reason::Ready)
            } else {
                (Level::Bad, Reason::NoPhone)
            };
            snap.level = level;
            snap.reason = reason;
        }
    }
    snap.setup_todo = health
        .get("setup_missing")
        .and_then(Value::as_array)
        .map(|a| a.len().min(u32::MAX as usize) as u32)
        .unwrap_or(0);
    apply_setup(&mut snap);
    snap
}

/// Unfinished setup turns anything short of red into yellow: it is never "all good".
fn apply_setup(snap: &mut Snapshot) {
    if snap.setup_todo > 0 && snap.level != Level::Bad {
        snap.level = Level::Warn;
        snap.reason = Reason::Setup;
    }
}

fn apply_detail(snap: &mut Snapshot, d: &Value) {
    let connected: BTreeSet<String> = d
        .get("connected")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    let labels: BTreeMap<String, String> = d
        .get("labels")
        .and_then(Value::as_object)
        .map(|o| o.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").trim().to_string())).collect())
        .unwrap_or_default();
    let mut info: BTreeMap<String, &Value> = BTreeMap::new();
    if let Some(list) = d.get("phones").and_then(Value::as_array) {
        for p in list {
            let serial = text(p.get("serial"));
            if !serial.is_empty() {
                info.insert(serial.to_string(), p);
            }
        }
    }

    let name_of = |serial: &str| -> String {
        if let Some(l) = labels.get(serial).filter(|l| !l.is_empty()) {
            return l.clone();
        }
        if let Some(model) = info.get(serial).map(|p| text(p.get("model"))).filter(|m| !m.is_empty()) {
            return model.to_string();
        }
        serial.to_string()
    };

    let mut phones: Vec<Phone> = Vec::new();
    for serial in &connected {
        let p = info.get(serial.as_str());
        let in_call = p.and_then(|p| p.get("in_call")).and_then(Value::as_bool).unwrap_or(false);
        let mode = p.map(|p| text(p.get("mode"))).unwrap_or("");
        let state = if in_call {
            PhoneState::Call
        } else if mode == "normal" {
            PhoneState::Normal
        } else {
            PhoneState::Ready
        };
        phones.push(Phone { serial: serial.clone(), name: name_of(serial), state });
    }

    let mut offline: BTreeSet<String> = labels.keys().cloned().collect();
    for (serial, p) in &info {
        if p.get("connected").and_then(Value::as_bool) == Some(false) {
            offline.insert(serial.clone());
        }
    }
    for serial in offline.iter().filter(|s| !connected.contains(*s)) {
        phones.push(Phone { serial: serial.clone(), name: name_of(serial), state: PhoneState::Offline });
    }

    phones.sort_by(|a, b| {
        (a.state.rank(), a.name.to_lowercase(), &a.name, &a.serial).cmp(&(
            b.state.rank(),
            b.name.to_lowercase(),
            &b.name,
            &b.serial,
        ))
    });

    let ready = phones.iter().filter(|p| p.state == PhoneState::Ready).count() as u32;
    let in_calls = phones.iter().filter(|p| p.state == PhoneState::Call).count() as u32;
    let calls = count(d.get("calls_in_progress")).max(in_calls);
    let offline_phones: Vec<&Phone> = phones.iter().filter(|p| p.state == PhoneState::Offline).collect();

    snap.ready = ready;
    snap.calls = calls;
    snap.offline_count = offline_phones.len() as u32;
    snap.offline_name = offline_phones.first().map(|p| p.name.clone()).unwrap_or_default();

    let (level, reason) = if connected.is_empty() && calls == 0 {
        (Level::Bad, Reason::NoPhone)
    } else if snap.offline_count > 0 {
        (Level::Warn, Reason::PhoneOffline)
    } else if ready == 0 && calls == 0 {
        (Level::Warn, Reason::AllNormal)
    } else if calls > 0 {
        (Level::Ok, Reason::Busy)
    } else {
        (Level::Ok, Reason::Ready)
    };
    snap.level = level;
    snap.reason = reason;
    snap.phones = phones;
}

/// `usb.enabled` and `usb.adb_found` from the devices answer.
/// Without detail: (false, true), which never asks for adb.
pub fn usb_flags(devices: &DevicesResult) -> (bool, bool) {
    let DevicesResult::Detail(d) = devices else { return (false, true) };
    let Some(usb) = d.get("usb") else { return (false, true) };
    let flag = |key: &str, default: bool| usb.get(key).and_then(Value::as_bool).unwrap_or(default);
    (flag("enabled", false), flag("adb_found", true))
}

/// Stores the adb state. Missing Android tools explain "no phone", so they are named first.
pub fn apply_adb(snap: &mut Snapshot, adb: &str) {
    snap.adb = adb.to_string();
    if !adb.is_empty() && snap.reason == Reason::NoPhone {
        snap.level = Level::Warn;
        snap.reason = Reason::NeedAdb;
    }
}

/// What to show after /health failed `failures` times in a row.
/// `last_good` is the last Snapshot built from a successful answer, if any.
pub fn after_failure(last_good: Option<&Snapshot>, failures: u32, lang: &str, has_password: bool, address: &str) -> Snapshot {
    match last_good {
        Some(prev) if failures < 2 => prev.clone(),
        Some(prev) => {
            let mut s = Snapshot::blank(Level::Bad, Reason::Offline, &prev.lang, has_password, address);
            s.version = prev.version.clone();
            s
        }
        None if failures <= 2 => Snapshot::blank(Level::Unknown, Reason::Starting, lang, has_password, address),
        None => Snapshot::blank(Level::Bad, Reason::Offline, lang, has_password, address),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn health() -> Value {
        json!({"ok": true, "version": "1.4.0", "notify_language": "en", "active_call_count": 0, "control_idle_clients": 0})
    }

    fn snap(devices: Value) -> Snapshot {
        build_snapshot(&health(), &DevicesResult::Detail(devices), false, "http://192.168.1.20:8787/", "en-US")
    }

    #[test]
    fn ready() {
        let s = snap(json!({
            "connected": ["A1", "B2"],
            "labels": {"A1": "Pixel A", "B2": "Pixel B"},
            "phones": [
                {"serial": "A1", "connected": true, "mode": "ai", "model": "Pixel 9", "in_call": false},
                {"serial": "B2", "connected": true, "mode": "ai", "model": "Pixel 7", "in_call": false}
            ],
            "calls_in_progress": 0
        }));
        assert_eq!((s.level, s.reason, s.ready, s.calls), (Level::Ok, Reason::Ready, 2, 0));
        assert_eq!(s.version, "1.4.0");
        assert_eq!(s.address, "http://192.168.1.20:8787/");
        assert!(!s.locked);
    }

    #[test]
    fn busy() {
        let s = snap(json!({
            "connected": ["A1", "B2"],
            "phones": [
                {"serial": "A1", "connected": true, "mode": "ai", "in_call": true},
                {"serial": "B2", "connected": true, "mode": "ai", "in_call": false}
            ],
            "calls_in_progress": 1
        }));
        assert_eq!((s.level, s.reason, s.ready, s.calls), (Level::Ok, Reason::Busy, 1, 1));
    }

    #[test]
    fn calls_uses_larger_of_counter_and_phones() {
        let s = snap(json!({
            "connected": ["A1"],
            "phones": [{"serial": "A1", "connected": true, "in_call": true}],
            "calls_in_progress": 0
        }));
        assert_eq!(s.calls, 1);
        let s = snap(json!({"connected": ["A1"], "phones": [], "calls_in_progress": 3}));
        assert_eq!(s.calls, 3);
        assert_eq!(s.reason, Reason::Busy);
    }

    #[test]
    fn one_phone_offline() {
        let s = snap(json!({
            "connected": ["A1"],
            "labels": {"A1": "Pixel A", "B2": "Pixel B"},
            "phones": [{"serial": "A1", "connected": true, "mode": "ai"}],
            "calls_in_progress": 0
        }));
        assert_eq!((s.level, s.reason), (Level::Warn, Reason::PhoneOffline));
        assert_eq!((s.offline_count, s.offline_name.as_str()), (1, "Pixel B"));
        assert_eq!(s.phones.last().unwrap().state, PhoneState::Offline);
    }

    #[test]
    fn two_phones_offline_use_sorted_first_name() {
        let s = snap(json!({
            "connected": ["A1"],
            "labels": {"A1": "Pixel A", "Z9": "Zeta", "C3": "Charlie"},
            "phones": [
                {"serial": "A1", "connected": true},
                {"serial": "Z9", "connected": false},
                {"serial": "C3", "connected": false}
            ]
        }));
        assert_eq!((s.level, s.reason), (Level::Warn, Reason::PhoneOffline));
        assert_eq!((s.offline_count, s.offline_name.as_str()), (2, "Charlie"));
    }

    #[test]
    fn all_normal() {
        let s = snap(json!({
            "connected": ["A1"],
            "phones": [{"serial": "A1", "connected": true, "mode": "normal"}],
            "calls_in_progress": 0
        }));
        assert_eq!((s.level, s.reason, s.ready), (Level::Warn, Reason::AllNormal, 0));
        assert_eq!(s.phones[0].state, PhoneState::Normal);
    }

    #[test]
    fn no_phone() {
        let s = snap(json!({"connected": [], "labels": {}, "phones": [], "calls_in_progress": 0}));
        assert_eq!((s.level, s.reason), (Level::Bad, Reason::NoPhone));
        assert!(s.phones.is_empty());
    }

    #[test]
    fn nothing_connected_wins_over_offline() {
        // Nothing connected and no calls: no_phone wins over phone_offline.
        let s = snap(json!({"connected": [], "labels": {"A1": "Pixel A"}, "phones": []}));
        assert_eq!((s.level, s.reason), (Level::Bad, Reason::NoPhone));
        assert_eq!(s.offline_count, 1);
    }

    #[test]
    fn locked_uses_health_counters() {
        let mut h = health();
        h["control_idle_clients"] = json!(2);
        let s = build_snapshot(&h, &DevicesResult::Locked, true, "", "en");
        assert!(s.locked && s.has_password);
        assert_eq!((s.level, s.reason, s.ready, s.calls), (Level::Ok, Reason::Ready, 2, 0));
        assert!(s.phones.is_empty());

        h["active_call_count"] = json!(1);
        let s = build_snapshot(&h, &DevicesResult::Locked, false, "", "en");
        assert_eq!((s.level, s.reason, s.calls), (Level::Ok, Reason::Busy, 1));

        let s = build_snapshot(&health(), &DevicesResult::Locked, false, "", "en");
        assert_eq!((s.level, s.reason), (Level::Bad, Reason::NoPhone));
    }

    #[test]
    fn unavailable_devices_are_not_locked() {
        let s = build_snapshot(&health(), &DevicesResult::Unavailable, false, "", "en");
        assert!(!s.locked);
        assert_eq!((s.level, s.reason), (Level::Bad, Reason::NoPhone));
    }

    #[test]
    fn unfinished_setup_is_never_green() {
        let mut h = health();
        h["setup_missing"] = json!(["gemini_key", "owner_name", "notify", "summary"]);
        let one = json!({
            "connected": ["A1"],
            "phones": [{"serial": "A1", "connected": true, "mode": "ai", "model": "Pixel 9", "in_call": false}]
        });
        let s = build_snapshot(&h, &DevicesResult::Detail(one.clone()), false, "", "en");
        assert_eq!((s.level, s.reason, s.setup_todo, s.ready), (Level::Warn, Reason::Setup, 4, 1));
        assert_eq!(serde_json::to_value(&s).unwrap()["reason"], "setup");

        // Locked console: the short /health still carries the list.
        h["control_idle_clients"] = json!(1);
        let s = build_snapshot(&h, &DevicesResult::Locked, true, "", "en");
        assert_eq!((s.level, s.reason), (Level::Warn, Reason::Setup));

        // Red stays red.
        let s = build_snapshot(&h, &DevicesResult::Detail(json!({"connected": [], "phones": []})), false, "", "en");
        assert_eq!((s.level, s.reason), (Level::Bad, Reason::NoPhone));

        // Everything set, or an older Bageo without the field: green again.
        h["setup_missing"] = json!([]);
        let s = build_snapshot(&h, &DevicesResult::Detail(one.clone()), false, "", "en");
        assert_eq!((s.level, s.reason, s.setup_todo), (Level::Ok, Reason::Ready, 0));
        let s = build_snapshot(&health(), &DevicesResult::Detail(one), false, "", "en");
        assert_eq!((s.level, s.reason), (Level::Ok, Reason::Ready));
    }

    #[test]
    fn name_falls_back_label_then_model_then_serial() {
        let s = snap(json!({
            "connected": ["A1", "B2", "C3"],
            "labels": {"A1": "Pixel A", "B2": "  "},
            "phones": [
                {"serial": "A1", "connected": true, "model": "Pixel 9"},
                {"serial": "B2", "connected": true, "model": "Pixel 7"},
                {"serial": "C3", "connected": true, "model": ""}
            ]
        }));
        let name = |serial: &str| s.phones.iter().find(|p| p.serial == serial).unwrap().name.clone();
        assert_eq!(name("A1"), "Pixel A");
        assert_eq!(name("B2"), "Pixel 7");
        assert_eq!(name("C3"), "C3");
    }

    #[test]
    fn phones_sorted_call_ready_normal_offline_then_name() {
        let s = snap(json!({
            "connected": ["A", "B", "C", "D", "E"],
            "labels": {"A": "Zed", "B": "Amy", "C": "Bob", "D": "Cat", "E": "Dan", "F": "Eve"},
            "phones": [
                {"serial": "A", "connected": true, "mode": "ai", "in_call": true},
                {"serial": "B", "connected": true, "mode": "normal"},
                {"serial": "C", "connected": true, "mode": "ai"},
                {"serial": "D", "connected": true, "mode": "ai"},
                {"serial": "E", "connected": true, "mode": "normal"},
                {"serial": "F", "connected": false}
            ]
        }));
        let order: Vec<(&str, PhoneState)> = s.phones.iter().map(|p| (p.name.as_str(), p.state)).collect();
        assert_eq!(
            order,
            vec![
                ("Zed", PhoneState::Call),
                ("Bob", PhoneState::Ready),
                ("Cat", PhoneState::Ready),
                ("Amy", PhoneState::Normal),
                ("Dan", PhoneState::Normal),
                ("Eve", PhoneState::Offline),
            ]
        );
    }

    #[test]
    fn language_mapping_covers_all_nine() {
        assert_eq!(locale_to_lang("en-US"), "en");
        assert_eq!(locale_to_lang("zh-TW"), "zh-TW");
        assert_eq!(locale_to_lang("zh-HK"), "zh-TW");
        assert_eq!(locale_to_lang("zh-MO"), "zh-TW");
        assert_eq!(locale_to_lang("zh-Hant-CN"), "zh-TW");
        assert_eq!(locale_to_lang("zh_Hant_TW"), "zh-TW");
        assert_eq!(locale_to_lang("zh-CN"), "zh-CN");
        assert_eq!(locale_to_lang("zh-SG"), "zh-CN");
        assert_eq!(locale_to_lang("zh-Hans"), "zh-CN");
        assert_eq!(locale_to_lang("zh"), "zh-CN");
        assert_eq!(locale_to_lang("ja-JP"), "ja");
        assert_eq!(locale_to_lang("ko-KR"), "ko");
        assert_eq!(locale_to_lang("es-MX"), "es");
        assert_eq!(locale_to_lang("fr-CA"), "fr");
        assert_eq!(locale_to_lang("de-AT"), "de");
        assert_eq!(locale_to_lang("pt-BR"), "pt");
        assert_eq!(locale_to_lang("it-IT"), "en");
        assert_eq!(locale_to_lang(""), "en");
    }

    #[test]
    fn notify_language_wins_when_supported() {
        assert_eq!(resolve_lang(Some("ja"), "en-US"), "ja");
        assert_eq!(resolve_lang(Some("zh-CN"), "fr-FR"), "zh-CN");
        assert_eq!(resolve_lang(Some("xx"), "de-DE"), "de");
        assert_eq!(resolve_lang(None, "pt-PT"), "pt");
        let mut h = health();
        h["notify_language"] = json!("ko");
        let s = build_snapshot(&h, &DevicesResult::Unavailable, false, "", "en-US");
        assert_eq!(s.lang, "ko");
        h["notify_language"] = json!("");
        let s = build_snapshot(&h, &DevicesResult::Unavailable, false, "", "zh-HK");
        assert_eq!(s.lang, "zh-TW");
    }

    #[test]
    fn address_normalisation() {
        assert_eq!(normalize_address("http://192.168.1.20:8787").as_deref(), Some("http://192.168.1.20:8787/"));
        assert_eq!(normalize_address("  https://bageo.example/calls/  ").as_deref(), Some("https://bageo.example/"));
        assert_eq!(normalize_address("http://192.168.1.20:8787/calls").as_deref(), Some("http://192.168.1.20:8787/"));
        assert_eq!(normalize_address("http://host:8787/calls/?token=x#call=1").as_deref(), Some("http://host:8787/"));
        assert_eq!(normalize_address("http://host:8787?a=b").as_deref(), Some("http://host:8787/"));
        assert_eq!(normalize_address("https://example.test/bageo/CALLS/").as_deref(), Some("https://example.test/bageo/"));
        assert_eq!(normalize_address("https://example.test/bageo/").as_deref(), Some("https://example.test/bageo/"));
        assert_eq!(normalize_address("http://calls").as_deref(), Some("http://calls/"));
        assert_eq!(normalize_address("http:///calls"), None);
        assert_eq!(normalize_address("http://?x=1"), None);
        assert_eq!(normalize_address("HTTP://host").as_deref(), Some("HTTP://host/"));
        assert_eq!(normalize_address("192.168.1.20:8787"), None);
        assert_eq!(normalize_address("ftp://host"), None);
        assert_eq!(normalize_address("http://"), None);
        assert_eq!(normalize_address("   "), None);
    }

    #[test]
    fn console_address_is_the_reported_one_once_it_opens_here() {
        let h = json!({"console_url": "http://bageo.local/", "calls_ui_url": "http://bageo.local/"});
        assert_eq!(reported_console_url(&h), "http://bageo.local/");
        assert_eq!(reported_console_url(&json!({"calls_ui_url": " http://bageo.local:8787/ "})), "http://bageo.local:8787/");
        assert_eq!(reported_console_url(&json!({"console_url": "", "calls_ui_url": ""})), "");
        assert_eq!(reported_console_url(&json!({"console_url": "javascript:x"})), "");
        assert_eq!(reported_console_url(&json!({"ok": true})), "");
        let base = "http://127.0.0.1:8787/";
        assert_eq!(shown_address(base, "http://bageo.local/", true), "http://bageo.local/");
        assert_eq!(shown_address(base, "http://bageo.local/", false), base);
        assert_eq!(shown_address(base, "", true), base);
        assert_eq!(shown_address(base, "ftp://x/", true), base);
    }

    #[test]
    fn a_console_address_that_opened_stays_through_a_slow_lookup() {
        let local = "http://bageo.local/";
        // Opens: shown.
        assert_eq!(next_ui_url("", 0, local, true), (local.to_string(), 0));
        // A failed check or two of the same address: still shown.
        let (shown, m) = next_ui_url(local, 0, local, false);
        assert_eq!((shown.as_str(), m), (local, 1));
        let (shown, m) = next_ui_url(&shown, m, local, false);
        assert_eq!((shown.as_str(), m), (local, 2));
        // Opens again: the count starts over.
        assert_eq!(next_ui_url(&shown, m, local, true), (local.to_string(), 0));
        // Third failure in a row: the polling address is shown.
        assert_eq!(next_ui_url(local, 2, local, false), (String::new(), 0));
        // Never opened here, or Bageo now reports another address that does not open: not shown.
        assert_eq!(next_ui_url("", 0, local, false), (String::new(), 0));
        assert_eq!(next_ui_url(local, 0, "http://other.local/", false), (String::new(), 0));
        assert_eq!(next_ui_url(local, 0, "", false), (String::new(), 0));
    }

    #[test]
    fn failure_handling_after_success_and_before() {
        let good = snap(json!({"connected": ["A1"], "phones": [{"serial": "A1", "connected": true}]}));
        assert_eq!(after_failure(Some(&good), 1, "en", false, "x"), good);
        let off = after_failure(Some(&good), 2, "en", false, "x");
        assert_eq!((off.level, off.reason), (Level::Bad, Reason::Offline));
        assert_eq!(off.version, "1.4.0");

        for n in 1..=2 {
            let s = after_failure(None, n, "en", false, "");
            assert_eq!((s.level, s.reason), (Level::Unknown, Reason::Starting));
        }
        let s = after_failure(None, 3, "en", false, "");
        assert_eq!((s.level, s.reason), (Level::Bad, Reason::Offline));
    }

    #[test]
    fn usb_flags_come_from_the_devices_answer() {
        let d = |v: Value| DevicesResult::Detail(v);
        assert_eq!(usb_flags(&d(json!({"usb": {"enabled": true, "adb_found": false}}))), (true, false));
        assert_eq!(usb_flags(&d(json!({"usb": {"enabled": true, "adb_found": true}}))), (true, true));
        assert_eq!(usb_flags(&d(json!({"usb": {"enabled": false, "adb_found": false}}))), (false, false));
        // Older Bageo or no detail: never ask for adb.
        assert_eq!(usb_flags(&d(json!({"connected": []}))), (false, true));
        assert_eq!(usb_flags(&d(json!({"usb": {"enabled": true}}))), (true, true));
        assert_eq!(usb_flags(&DevicesResult::Locked), (false, true));
        assert_eq!(usb_flags(&DevicesResult::Unavailable), (false, true));
    }

    #[test]
    fn need_adb_replaces_no_phone_only() {
        // No phone connected: missing Android tools are the thing to fix.
        let mut s = snap(json!({"connected": [], "labels": {"A1": "Pixel A"}, "phones": []}));
        apply_adb(&mut s, "missing");
        assert_eq!((s.level, s.reason, s.adb.as_str()), (Level::Warn, Reason::NeedAdb, "missing"));
        for state in ["downloading", "failed"] {
            let mut s = snap(json!({"connected": [], "phones": []}));
            apply_adb(&mut s, state);
            assert_eq!((s.level, s.reason, s.adb.as_str()), (Level::Warn, Reason::NeedAdb, state));
        }
        // Nothing missing: no_phone stays.
        let mut s = snap(json!({"connected": [], "phones": []}));
        apply_adb(&mut s, "");
        assert_eq!((s.level, s.reason, s.adb.as_str()), (Level::Bad, Reason::NoPhone, ""));
        // A phone is connected (over Wi-Fi, say): every other state keeps its own reason.
        let mut s = snap(json!({"connected": ["A1"], "phones": [{"serial": "A1", "connected": true}]}));
        apply_adb(&mut s, "missing");
        assert_eq!((s.level, s.reason, s.adb.as_str()), (Level::Ok, Reason::Ready, "missing"));
        let mut s = snap(json!({"connected": ["A1"], "labels": {"B2": "Pixel B"}, "phones": [{"serial": "A1", "connected": true}]}));
        apply_adb(&mut s, "missing");
        assert_eq!((s.level, s.reason), (Level::Warn, Reason::PhoneOffline));
        let mut s = snap(json!({"connected": [], "phones": [], "calls_in_progress": 1}));
        apply_adb(&mut s, "missing");
        assert_eq!((s.level, s.reason), (Level::Ok, Reason::Busy));
    }

    #[test]
    fn new_reasons_and_fields_serialise_for_the_page() {
        let mut s = Snapshot::blank(Level::Unknown, Reason::Launching, "en", false, "");
        s.local = true;
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!((v["reason"].as_str(), v["local"].as_bool(), v["adb"].as_str()), (Some("launching"), Some(true), Some("")));
        s.reason = Reason::LaunchFailed;
        assert_eq!(serde_json::to_value(&s).unwrap()["reason"], "launch_failed");
        s.reason = Reason::NeedAdb;
        assert_eq!(serde_json::to_value(&s).unwrap()["reason"], "need_adb");
    }

    #[test]
    fn snapshot_serialises_with_snake_case_names() {
        let s = snap(json!({
            "connected": ["A1"], "labels": {"B2": "Pixel B"},
            "phones": [{"serial": "A1", "connected": true}]
        }));
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["level"], "warn");
        assert_eq!(v["reason"], "phone_offline");
        assert_eq!(v["offline_name"], "Pixel B");
        assert_eq!(v["phones"][1]["state"], "offline");
        assert_eq!(v["lang"], "en");
    }
}

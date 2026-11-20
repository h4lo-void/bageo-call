// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Words the owner reads (notifications, summaries, Telegram cards, the web
//! record), kept as one JSON file per core language in `data/i18n/notify/`.

use super::Lang;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::sync::OnceLock;

/// The built-in files, English first: every other file falls back to it.
const FILES: [(Lang, &str); 9] = [
    (Lang::En, include_str!("../../data/i18n/notify/en.json")),
    (Lang::ZhTw, include_str!("../../data/i18n/notify/zh-TW.json")),
    (Lang::ZhCn, include_str!("../../data/i18n/notify/zh-CN.json")),
    (Lang::Ja, include_str!("../../data/i18n/notify/ja.json")),
    (Lang::Ko, include_str!("../../data/i18n/notify/ko.json")),
    (Lang::Es, include_str!("../../data/i18n/notify/es.json")),
    (Lang::Fr, include_str!("../../data/i18n/notify/fr.json")),
    (Lang::De, include_str!("../../data/i18n/notify/de.json")),
    (Lang::Pt, include_str!("../../data/i18n/notify/pt.json")),
];

/// Everything the owner reads that is a short phrase or a format: one value
/// per key of the files (`strings`, `lists` and `formats` side by side).
#[derive(Deserialize)]
#[cfg_attr(test, derive(serde::Serialize))]
#[serde(deny_unknown_fields)]
pub struct Words {
    /// Speaker labels of the dialogue in the web record: the caller, the AI.
    pub d_caller: String,
    pub d_ai: String,
    // Notifications
    pub n_incoming: String,
    pub n_outgoing: String,
    pub n_summary: String,
    pub n_blocked: String,
    pub n_voicemail: String,
    pub n_number: String,
    pub n_phone: String,
    pub n_time: String,
    pub n_reason: String,
    pub n_length: String,
    pub n_seconds: String,
    pub n_hidden: String,
    pub n_unknown: String,
    pub n_details: String,
    pub n_recording: String,
    /// `{}` = the announcement text.
    pub n_blocked_done: String,
    /// Label of the calendar suggestion line.
    pub n_calendar: String,
    pub n_test_title: String,
    pub n_test_body: String,
    /// Held-back calls of one number, in one message. `{number}` = the number,
    /// `{count}` = how many calls.
    pub n_repeat_title: String,
    pub n_repeat_body: String,
    // Post-call summary lines
    pub s_caller: String,
    pub s_matter: String,
    pub s_reply: String,
    pub s_todo: String,
    pub s_no_summary: String,
    pub s_ai_failed: String,
    // Formats
    /// Between a label and its value ("Number: …", "號碼：…").
    pub sep: String,
    /// Between listed words.
    pub list_sep: String,
    /// Quotation marks around a quoted phrase.
    pub quote: [String; 2],
    /// chrono format of a date and time in notifications ([`super::date_time`]).
    pub dt_fmt: String,
    /// chrono format of a date alone ([`super::date_only`]).
    pub date_fmt: String,
    /// `{}` seconds / `{}` minutes ([`super::duration`]).
    pub u_sec: String,
    pub u_min: String,
    // Summaries written without the summary model. `{}` = a length
    // ([`super::duration`]) or the words that were named.
    pub s_no_service: String,
    pub s_see_transcript: String,
    pub s_no_transcript: String,
    pub s_listen_recording: String,
    pub s_unclear: String,
    pub s_ai_fail_matter: String,
    pub s_call_back_if_known: String,
    /// First line put above a summary when the AI never connected.
    pub s_ai_fail_prefix: String,
    pub s_stall_topic: String,
    pub s_stall_topic_kw: String,
    pub s_stall_reply: String,
    /// Nothing to do.
    pub s_nothing: String,
    // Voicemail (older menu flow): how the caller wants an answer. `{}` = the
    // number or address, or a length.
    pub vm_call_back_to: String,
    pub vm_call_back_unknown: String,
    pub vm_email_to: String,
    pub vm_email_unknown: String,
    pub vm_call_back_unchecked: String,
    pub vm_no_reply: String,
    pub vm_no_choice: String,
    pub vm_decide_callback: String,
    pub vm_length: String,
    pub vm_no_transcript: String,
    pub vm_task_failed: String,
    // Telegram task cards, their buttons and the follow-up message
    pub tc_status: String,
    pub tc_pending: String,
    pub tc_transcript: String,
    pub tc_confirm: String,
    pub tc_ignore: String,
    pub tc_confirm_ack: String,
    pub tc_ignore_ack: String,
    pub tc_task: String,
    pub tc_ignored_task: String,
    pub tc_done: String,
    pub tc_in_progress: String,
    pub tc_failed: String,
    pub tc_confirm_failed: String,
    pub tc_ignore_failed: String,
    // Why a task could not be carried out (`fulfill`, Gmail).
    // `{}` = the time that was not understood / the error.
    pub f_callback_waiting: String,
    pub f_callback_whitelist: String,
    pub f_callback_no_number: String,
    pub f_callback_send_failed: String,
    pub f_email_no_address: String,
    pub f_gmail_missing: String,
    pub f_bad_time: String,
    pub f_openclaw_failed: String,
    pub g_bad_address: String,
    pub g_no_recipient: String,
    pub g_bad_recipient: String,
    pub g_short_password: String,
    // The e-mail and the calendar event a task sends. `{}` = the caller.
    pub e_subject: String,
    pub e_summary: String,
    pub e_empty: String,
    pub e_appointment: String,
    // Appointments the AI takes on a call (`appointments`): the Telegram card,
    // its buttons and results, the summary line and the text to the caller.
    /// Weekday names, Monday first.
    pub ap_weekdays: [String; 7],
    /// chrono format of an appointment's time; `{}` = the weekday.
    pub ap_dt_fmt: String,
    /// chrono format of an end time on the same day ("15:00–16:00").
    pub ap_end_fmt: String,
    pub ap_confirm: String,
    pub ap_decline: String,
    pub ap_card_title: String,
    pub ap_what: String,
    pub ap_name: String,
    pub ap_sms: String,
    pub ap_sms_will: String,
    pub ap_sms_off: String,
    pub ap_sms_cannot: String,
    pub ap_pending: String,
    pub ap_replaced: String,
    pub ap_time_unconfirmed: String,
    pub ap_confirmed: String,
    pub ap_declined: String,
    pub ap_already: String,
    /// The link that adds an accepted appointment to the owner's calendar; `{}` = the link.
    pub ap_cal_link: String,
    pub ap_sms_sent: String,
    pub ap_sms_not_sent: String,
    pub ap_sms_old_app: String,
    pub ap_sms_offline: String,
    pub ap_sms_timeout: String,
    pub ap_sms_code: String,
    pub ap_sms_hi_owner: String,
    pub ap_sms_hi: String,
    pub ap_sms_ok: String,
    pub ap_sms_no: String,
    /// The same text is already on its way.
    pub ap_sms_busy: String,
    /// An event that no longer stands could not be deleted; `{}` = when and what.
    pub ap_void_failed: String,
    /// The summary judged the call a sales call afterwards: the event the call
    /// put in the calendar was deleted; `{}` = when and what.
    pub ap_sales_voided: String,
    /// After the pending line of the summary: where to confirm; `{}` = the
    /// call's page in the web console.
    pub ap_web_confirm: String,
    /// The same without a known console address.
    pub ap_web_console: String,
    /// Label of the web link on the Telegram card.
    pub ap_web_label: String,
}

/// The `strings`, `lists` and `formats` of a file in one object; nothing
/// when the text is no JSON object.
fn flatten(text: &str) -> Map<String, Value> {
    let mut out = Map::new();
    if let Ok(Value::Object(file)) = serde_json::from_str::<Value>(text) {
        for section in ["strings", "lists", "formats"] {
            if let Some(Value::Object(m)) = file.get(section) {
                out.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
            }
        }
    }
    out
}

/// A text where English has a text, a list of texts as long as English's
/// where English has a list.
fn same_shape(en: &Value, v: &Value) -> bool {
    match (en, v) {
        (Value::String(_), Value::String(_)) => true,
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && b.iter().all(Value::is_string),
        _ => false,
    }
}

/// English with the keys of `text` laid over it: keys English does not have
/// are left out, missing ones stay English.
fn load(text: &str) -> Words {
    let mut merged = flatten(FILES[0].1);
    for (k, v) in flatten(text) {
        if merged.get(&k).is_some_and(|en| same_shape(en, &v)) {
            merged.insert(k, v);
        }
    }
    serde_json::from_value(Value::Object(merged)).expect("the English notify file names every field of Words")
}

static CACHE: [OnceLock<Words>; 9] = [const { OnceLock::new() }; 9];

/// The words of a language, read from its file the first time they are
/// needed. Languages beyond the nine core ones read English.
pub fn words(lang: Lang) -> &'static Words {
    let i = FILES.iter().position(|(l, _)| *l == lang).unwrap_or(0);
    CACHE[i].get_or_init(|| load(FILES[i].1))
}

/// The words of what the owner reads ([`super::notify_lang`]).
pub fn notify_words() -> &'static Words {
    words(super::notify_lang())
}

#[cfg(test)]
mod tests {
    use super::{flatten, load, words, FILES};
    use crate::locale::{Ext, Lang, CORE};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    /// SHA-256 (of the text with LF line ends) and key count of every file.
    /// The files were written from the old `Phrases` constants and must stay
    /// word for word what they were; change a value on purpose and update the
    /// line here in the same commit.
    const GOLDEN: [(&str, &str, usize); 9] = [
        ("en", "8e601d52786149cf87b75d417d4181150ffbffd5600650ea5392a38c17d14493", 124),
        ("zh-TW", "b61dce3c2445d9dfd597379b10233f88b96df76fafb2ce74aba67e4c22d4dd15", 124),
        ("zh-CN", "a28e02ea8da5e86342a0c9cd8d7b62a4794230a30a8b0a36e8bd5f6a9700f4fc", 124),
        ("ja", "6886128c56b6ab2da230c93f12eda8259579933b121def770d73f193055d7c4c", 124),
        ("ko", "84b7df2618b458955e74da2cb1f5c1ca9101ff6a5abac6df7d4a91e4d3e51e2d", 124),
        ("es", "27625d41b254fd9d2a38906b9185110b190dd42b006e3e13ebe33c6482757bdc", 124),
        ("fr", "e8d060a8b32f1f7f3aa3eca68b272738aabceaee37d19e96c52ed99de0d353c4", 124),
        ("de", "b09d09a83600c9610147fb2ca337f517e47e745688a9c5c8d49ee74919006e6e", 124),
        ("pt", "0f9f0b7343194b94c401a3eaa909d9c992cca7a8fed077aae1a30f59e3c953fa", 124),
    ];

    const META: &str = include_str!("../../data/i18n/notify/_meta.json");

    /// Keys a language leaves empty on purpose, as (language code, key).
    const MAY_BE_EMPTY: &[(&str, &str)] = &[];

    fn lf(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    fn key_count(text: &str) -> usize {
        let v: Value = serde_json::from_str(text).expect("valid JSON");
        assert_eq!(v["schema"], 1);
        ["strings", "lists", "formats"]
            .iter()
            .map(|s| v[*s].as_object().expect("section").len())
            .sum()
    }

    /// The texts of a value: a text alone or the items of a list.
    fn texts(v: &Value) -> Vec<String> {
        match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a.iter().map(|x| x.as_str().expect("a list of texts").to_string()).collect(),
            other => panic!("neither a text nor a list: {other}"),
        }
    }

    /// The `{}` / `{name}` slots of a text, sorted.
    fn slots(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = text;
        while let Some(i) = rest.find('{') {
            let after = &rest[i + 1..];
            match after.find('}') {
                Some(j) if after[..j].chars().all(|c| c.is_ascii_lowercase() || c == '_') => {
                    out.push(format!("{{{}}}", &after[..j]));
                    rest = &after[j + 1..];
                }
                _ => rest = after,
            }
        }
        out.sort();
        out
    }

    #[test]
    fn notify_files_match_their_golden_hashes() {
        for ((lang, text), (code, sha, keys)) in FILES.iter().zip(GOLDEN) {
            assert_eq!(lang.code(), code);
            let text = lf(text);
            let got = format!("{:x}", Sha256::digest(text.as_bytes()));
            assert_eq!(got, sha, "{code}.json changed; if on purpose, update GOLDEN");
            assert_eq!(key_count(&text), keys, "{code}.json");
        }
    }

    #[test]
    fn every_core_file_has_exactly_the_english_keys() {
        let en: Vec<String> = flatten(FILES[0].1).keys().cloned().collect();
        for (lang, text) in FILES {
            let keys: Vec<String> = flatten(text).keys().cloned().collect();
            assert_eq!(keys, en, "{lang:?}");
        }
    }

    #[test]
    fn every_value_is_filled_and_has_the_slots_of_english() {
        let en = serde_json::to_value(words(Lang::En)).expect("serialize");
        let en = en.as_object().expect("object");
        for l in CORE {
            let w = serde_json::to_value(words(l)).expect("serialize");
            let w = w.as_object().expect("object");
            assert_eq!(w.len(), en.len(), "{l:?}");
            for (k, v) in w {
                let (got, want) = (texts(v), texts(&en[k]));
                assert_eq!(got.len(), want.len(), "{l:?} {k}");
                for (x, e) in got.iter().zip(&want) {
                    if !MAY_BE_EMPTY.contains(&(l.code(), k.as_str())) {
                        assert!(!x.is_empty(), "{l:?} {k}");
                    }
                    assert_eq!(slots(x), slots(e), "{l:?} {k}: {x}");
                }
            }
        }
    }

    #[test]
    fn formats_read_as_chrono_formats() {
        use chrono::format::{Item, StrftimeItems};
        for l in CORE {
            let w = words(l);
            for f in [&w.dt_fmt, &w.date_fmt, &w.ap_dt_fmt, &w.ap_end_fmt] {
                assert!(StrftimeItems::new(f).all(|i| !matches!(i, Item::Error)), "{l:?}: {f}");
            }
        }
    }

    #[test]
    fn unknown_keys_are_left_out_and_missing_ones_read_english() {
        let en = words(Lang::En);
        let w = load(r#"{"schema":1,"strings":{"n_incoming":"X","no_such_key":"Y","n_outgoing":["Z"]},"lists":{"quote":["<",">","!"]}}"#);
        assert_eq!(w.n_incoming, "X");
        assert_eq!(w.n_outgoing, en.n_outgoing);
        assert_eq!(w.quote, en.quote);
        assert_eq!(w.n_summary, en.n_summary);
        assert_eq!(load("not JSON").n_summary, en.n_summary);
        for e in Ext::all() {
            assert!(std::ptr::eq(words(Lang::Ext(e)), en), "{e:?}");
        }
    }

    #[test]
    fn every_key_has_its_meta() {
        let meta: serde_json::Map<String, Value> = serde_json::from_str(META).expect("valid JSON");
        let en = flatten(FILES[0].1);
        assert_eq!(meta.keys().collect::<Vec<_>>(), en.keys().collect::<Vec<_>>());
        for (k, m) in &meta {
            for field in ["where", "who"] {
                assert!(m[field].as_str().is_some_and(|s| !s.trim().is_empty()), "{k} {field}");
            }
            let mut got: Vec<String> = texts(&m["placeholders"]);
            got.sort();
            let want: Vec<String> = {
                let mut s: Vec<String> = texts(&en[k]).iter().flat_map(|t| slots(t)).collect();
                s.sort();
                s
            };
            assert_eq!(got, want, "{k}");
        }
    }
}

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
    // Lines of the call record, notifications and the note for the summary
    // model that were written in Chinese and English only.
    /// A number with its contact's name in a title; `{number}`, `{name}`.
    pub n_named_number: String,
    /// Headline of a call the phone turned away during another call;
    /// `{caller}`, `{device}`.
    pub n_cw_headline: String,
    /// What became of a blocked number's call that was declined while it rang.
    pub n_blocked_declined: String,
    /// Matter line of a call turned away during another call; `{sim}` = the
    /// SIM in brackets, or nothing.
    pub cw_matter: String,
    pub cw_end_hint: String,
    pub s_call_back_if_needed: String,
    pub bl_declined_reply: String,
    pub bl_declined_hint: String,
    pub bl_announced_hint: String,
    /// Where the middle of a long dialogue is left out for the summary model.
    pub h_clip_mark: String,
    // How the call ended, one line for the summary model.
    pub h_ai_fail_hangup: String,
    pub h_ai_fail_voicemail: String,
    pub h_reject_hidden: String,
    pub h_reject_list: String,
    pub h_urgent_merged: String,
    pub h_urgent_failed: String,
    /// The urgent caller also said words only scammers say: the owner was not called.
    pub h_urgent_scam_words: String,
    pub h_urgent_transfer: String,
    pub h_stall_released: String,
    /// `{t}` = the kind of call.
    pub h_stall_spam: String,
    pub h_stall_spam_nokind: String,
    pub h_scam_stalling_off: String,
    pub h_ai_ended: String,
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

/// The words of a translated file (strings, lists and formats of the
/// translations' notify part) laid over English: only keys English has are
/// read, missing ones stay English.
pub(crate) fn words_over_english(text: &str) -> Words {
    load(text)
}

/// The words of a language, read from its file the first time they are
/// needed. Languages beyond the nine core ones read their translation when it
/// reaches the notify stage ([crate::locales_store]), else English.
pub fn words(lang: Lang) -> &'static Words {
    if let Lang::Ext(e) = lang {
        if let Some(w) = crate::locales_store::current().words(e) {
            return w;
        }
    }
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
    /// The first 124 keys were written from the old `Phrases` constants, the
    /// next 22 from the lines of the code that were Chinese and English only,
    /// and `h_urgent_scam_words` came after them; change a value on purpose
    /// and update the line here in the same commit.
    const GOLDEN: [(&str, &str, usize); 9] = [
        ("en", "ae71ba1767c4fc89508e192d8c516a499505521310e97ffb8f7ef2cff5cdb260", 147),
        ("zh-TW", "fa31430a772a9d4947365a043381dadc88cdde854f26e2f15646a1e8ee403ef6", 147),
        ("zh-CN", "cbb40fb8f5bae1a422ae06f17b4768e61b192c8c7b67c84ec3ad45196c4b3168", 147),
        ("ja", "2d5369ef38d7f194df6c483b942d9ca28584a3c7f352ce74ce7252d2c086ccc4", 147),
        ("ko", "5f385556a0c10e74bd2a642824c65a6d50c19b2ac76a969d916c77b59d1d76a3", 147),
        ("es", "96619969e6f7516d583c51e5ec8769e5c9006cd5bc32e2c2b09a00de0ede8d2d", 147),
        ("fr", "80d2c66037c34757284e4b779a3f7e16faf360ddb1a44a94d5ec3c0488342c7a", 147),
        ("de", "26594eb6fd3963c4e03b05a4971a2997a15a64ba3898bc671a63a44008804afa", 147),
        ("pt", "be1d99d4fcaa3bf7a0634b42dbab47df79e3befa6384126c055d1731dd77cf43", 147),
    ];

    const META: &str = include_str!("../../data/i18n/notify/_meta.json");

    /// The lines that were Chinese-and-English-only, as the code wrote them
    /// before they moved into the files: (key, zh-TW, en). Simplified Chinese
    /// was the converter's output for the zh-TW line.
    const OLD_LINES: &[(&str, &str, &str)] = &[
        ("n_named_number", "{number}（{name}）", "{number} ({name})"),
        ("n_cw_headline", "通話中插播未接：{caller}（{device}）", "Missed while on another call: {caller} ({device})"),
        ("n_blocked_declined", "響鈴時直接拒接，沒有接起來，沒有用 AI", "Declined while it rang: not picked up, no AI"),
        ("cw_matter", "通話中插播，手機忙線拒接{sim}", "came in during another call; the phone turned it away as busy{sim}"),
        ("cw_end_hint", "通話中插播：手機忙線拒接（沒有用 AI）", "call waiting: the phone was on another call and turned it away (no AI)"),
        ("s_call_back_if_needed", "要聯絡的話自己回電", "call back if needed"),
        ("bl_declined_reply", "響鈴時直接拒接，沒有接起來（沒用 AI）", "declined while it rang: not picked up (no AI)"),
        ("bl_declined_hint", "黑名單：響鈴時直接拒接（沒有用 AI）", "blocked number: declined while it rang (no AI)"),
        ("bl_announced_hint", "黑名單：播無法接通語音後掛斷（沒有用 AI）", "blocked number: the announcement was played, then hung up (no AI)"),
        ("h_clip_mark", "…（中間省略）…", "… (middle left out) …"),
        ("h_ai_fail_hangup", "AI 連不上，系統直接掛斷，對方沒聽到任何回應", "the AI could not connect; the system hung up and the caller heard nothing"),
        ("h_ai_fail_voicemail", "AI 失敗，改走語音留言", "the AI failed; the call went to voicemail"),
        ("h_reject_hidden", "未顯示號碼，播拒接語音後掛斷（AI 沒有接）", "hidden number: the refusal announcement was played, then hung up (the AI did not answer)"),
        ("h_reject_list", "黑名單：播無法接通語音後掛斷（AI 沒有接）", "blocked number: the unavailable announcement was played, then hung up (the AI did not answer)"),
        ("h_urgent_merged", "來電者說有急事：手機打給主人，主人接起來後直接跟來電者通話，AI 退出；這通沒有被當成推銷或詐騙陪聊拖時間", "the caller said it was urgent: the phone called the owner, who picked up and was put through to the caller, and the AI left; this call was not treated as a sales or scam call to stall"),
        ("h_urgent_failed", "來電者說有急事：手機打給主人但沒接通，AI 跟對方說主人會盡快回電；這通沒有被當成推銷或詐騙陪聊拖時間，「要處理」寫盡快回電給對方", "the caller said it was urgent: the phone called the owner but did not get through, and the AI told the caller the owner would call back soon; this call was not treated as a sales or scam call to stall, so the to-do is to call them back as soon as possible"),
        ("h_urgent_transfer", "來電者說有急事，系統已馬上通知主人；這通沒有被當成推銷或詐騙陪聊拖時間，「要處理」寫盡快回電給對方", "the caller said it was urgent and the owner was alerted at once; this call was not treated as a sales or scam call to stall, so the to-do is to call them back as soon as possible"),
        ("h_stall_released", "系統一度以為是推銷／詐騙，再確認一次是正常電話，AI 回到正常應對", "the system first took it for a sales / scam call; a second check found an ordinary call and the AI went back to talking normally"),
        ("h_stall_spam", "系統判斷是推銷／詐騙（{t}），AI 改成故意陪他聊、拖時間", "the system judged it a sales / scam call ({t}), so the AI deliberately kept the caller talking to waste their time"),
        ("h_stall_spam_nokind", "系統判斷像推銷／詐騙，AI 改成故意陪他聊、拖時間", "the system judged it a sales / scam call, so the AI deliberately kept the caller talking to waste their time"),
        ("h_scam_stalling_off", "系統判斷是推銷／詐騙（{t}）；陪聊功能關著，AI 照一般方式應對", "the system judged it a sales / scam call ({t}); stalling is off, so the AI answered as usual"),
        ("h_ai_ended", "AI 接聽並對話後結束（AI 說掰掰掛斷，或對方先掛）", "the AI answered and talked; the call ended normally (the AI said goodbye and hung up, or the caller hung up first)"),
    ];

    /// The keys of [`OLD_LINES`], the ones the six other languages were added to.
    const FILLED_KEYS: &[&str] = &["n_named_number", "n_cw_headline", "n_blocked_declined", "cw_matter", "cw_end_hint", "s_call_back_if_needed", "bl_declined_reply", "bl_declined_hint", "bl_announced_hint", "h_clip_mark", "h_ai_fail_hangup", "h_ai_fail_voicemail", "h_reject_hidden", "h_reject_list", "h_urgent_merged", "h_urgent_failed", "h_urgent_transfer", "h_stall_released", "h_stall_spam", "h_stall_spam_nokind", "h_scam_stalling_off", "h_ai_ended"];

    /// Texts a language writes the way English does, as (language code, key).
    const SAME_AS_ENGLISH: &[(&str, &str)] = &[
        ("ko", "n_named_number"),
        ("es", "n_named_number"),
        ("fr", "n_named_number"),
        ("de", "n_named_number"),
        ("pt", "n_named_number"),
    ];

    const SLOTS: [(&str, &str); 6] = [
        ("number", "0912345678"),
        ("name", "陳國華"),
        ("caller", "0912345678"),
        ("device", "Pixel 9"),
        ("sim", " (SIM TW)"),
        ("t", "疑似詐騙：假冒客服"),
    ];

    /// A text with the named slots replaced, one after the other.
    fn plain(template: &str) -> String {
        SLOTS.iter().fold(template.to_string(), |s, (k, v)| s.replace(&format!("{{{k}}}"), v))
    }

    fn word(l: Lang, key: &str) -> String {
        let w = serde_json::to_value(words(l)).expect("serialize");
        w[key].as_str().unwrap_or_else(|| panic!("{key} is no text")).to_string()
    }

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
    fn lines_that_were_chinese_and_english_only_read_as_before() {
        use crate::locale::fill_in;
        assert_eq!(OLD_LINES.len(), FILLED_KEYS.len());
        for (key, tw, en) in OLD_LINES {
            let (tw, en) = (plain(tw), plain(en));
            assert_eq!(fill_in(Lang::ZhTw, &word(Lang::ZhTw, key), &SLOTS), tw, "{key}");
            assert_eq!(fill_in(Lang::En, &word(Lang::En, key), &SLOTS), en, "{key}");
            assert_eq!(fill_in(Lang::ZhCn, &word(Lang::ZhCn, key), &SLOTS), crate::zh_convert::to_cn(&tw), "{key}");
            // Without slots the Simplified file holds the converter's output.
            assert_eq!(word(Lang::ZhCn, key), crate::zh_convert::to_cn(&word(Lang::ZhTw, key)), "{key}");
        }
    }

    #[test]
    fn the_filled_in_languages_are_written_not_left_in_english() {
        for l in [Lang::Ja, Lang::Ko, Lang::Es, Lang::Fr, Lang::De, Lang::Pt] {
            for key in FILLED_KEYS {
                if !SAME_AS_ENGLISH.contains(&(l.code(), *key)) {
                    assert_ne!(word(l, key), word(Lang::En, key), "{l:?} {key}");
                }
            }
        }
    }

    #[test]
    fn urgent_lines_name_the_to_do_label_of_their_own_language() {
        for l in CORE.into_iter().filter(|l| *l != Lang::En) {
            for key in ["h_urgent_failed", "h_urgent_scam_words", "h_urgent_transfer"] {
                assert!(word(l, key).contains(words(l).s_todo.as_str()), "{l:?} {key}");
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
        // Without translations every other language reads English.
        crate::locales_store::testing::use_empty();
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

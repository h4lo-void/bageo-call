// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Desktop notifications: decides what is new in Bageo's answers. No network or window code here.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

use crate::i18n;
use crate::status::Phone;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Call,
    Sms,
    Summary,
}

/// One notification. `url` stays on the Rust side; the page gets `ToastData`.
#[derive(Clone, PartialEq, Debug)]
pub struct Notice {
    pub kind: Kind,
    pub who: String,
    pub text: String,
    pub phone: String,
    pub url: String,
}

/// Caller ids that mean "no number".
const HIDDEN_IDS: [&str; 7] = ["unknown", "private", "anonymous", "restricted", "unavailable", "withheld", "blocked"];
/// Retry the SMS endpoint this often (in rounds) after it answered 404.
const SMS_RETRY_ROUNDS: u32 = 12;
/// Remember at most this many call ids.
const MAX_SEEN: usize = 500;
/// The call list is read every this many polling rounds. A summary shows up 5 to 60 seconds after
/// the call ends, so a few seconds of delay on top is not noticeable.
const SUMMARY_EVERY: u32 = 2;

fn text_of(v: Option<&Value>) -> &str {
    v.and_then(Value::as_str).map(str::trim).unwrap_or("")
}

/// The name to show for a caller: contact name, else the number, else "" for hidden numbers.
pub fn caller_name(caller_id: &str, contacts: Option<&Value>) -> String {
    let id = caller_id.trim();
    if let Some(c) = contacts {
        for key in [caller_id, id] {
            let name = text_of(c.get(key).and_then(|v| v.get("name")));
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    if id.is_empty() || HIDDEN_IDS.contains(&id.to_lowercase().as_str()) {
        String::new()
    } else {
        id.to_string()
    }
}

/// Link that opens the console on this call. Falls back to the plain address for odd ids.
pub fn call_url(address: &str, id: &str) -> String {
    let valid = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid || address.is_empty() {
        return address.to_string();
    }
    let base = address.trim_end_matches('/');
    // Bageo may report its console address as ".../calls/" already.
    let base = base.strip_suffix("/calls").unwrap_or(base);
    format!("{base}/calls/#call={id}")
}

/// Link that opens the console on the conversation with sender (the list of
/// conversations when the sender is not known).
pub fn sms_url(address: &str, sender: &str) -> String {
    if address.is_empty() {
        return String::new();
    }
    let base = address.trim_end_matches('/');
    let base = base.strip_suffix("/calls").unwrap_or(base);
    let sender = sender.trim();
    if sender.is_empty() {
        return format!("{base}/calls/#sms");
    }
    format!("{base}/calls/#sms/{}", url_component(sender))
}

/// Percent-encoding of everything but letters, digits and "-._~", like the
/// browser's encodeURIComponent.
fn url_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Phone name for a call, only worth showing when more than one phone is known.
fn phone_label(serial: &str, phones: &[Phone]) -> String {
    if phones.len() < 2 || serial.is_empty() {
        return String::new();
    }
    phones
        .iter()
        .find(|p| p.serial == serial)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| serial.to_string())
}

/// True for calls Bageo placed itself (AI call-backs and errands) or turned away because the
/// number is on the reject list: nobody is calling the owner, so there is nothing to announce.
fn is_ours(call: &Value) -> bool {
    call.get("events")
        .and_then(Value::as_array)
        .is_some_and(|ev| ev.iter().filter_map(Value::as_str).any(|e| matches!(e, "ai_callback" | "ai_errand" | "reject_list")))
}

/// Watches the live call list for calls it has not seen before.
#[derive(Default)]
pub struct CallWatch {
    seen: HashSet<String>,
    live_primed: bool,
    last_active: Option<u32>,
}

impl CallWatch {
    pub fn reset(&mut self) {
        *self = CallWatch::default();
    }

    /// Handles a successful answer from the live calls endpoint.
    pub fn on_live(&mut self, body: &Value, phones: &[Phone], address: &str) -> Vec<Notice> {
        let contacts = body.get("contacts_by_number");
        let mut out = Vec::new();
        let mut current: Vec<String> = Vec::new();
        for c in body.get("calls").and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[]) {
            let id = text_of(c.get("id"));
            if id.is_empty() {
                continue;
            }
            current.push(id.to_string());
            if self.seen.insert(id.to_string()) && self.live_primed && !is_ours(c) {
                out.push(Notice {
                    kind: Kind::Call,
                    who: caller_name(text_of(c.get("caller_id")), contacts),
                    text: String::new(),
                    phone: phone_label(text_of(c.get("serial")), phones),
                    url: call_url(address, id),
                });
            }
        }
        self.live_primed = true;
        if self.seen.len() > MAX_SEEN {
            self.seen = current.into_iter().collect();
        }
        out
    }

    /// Used when the live list needs a password we do not have: a rising call count means a new call.
    pub fn on_active_count(&mut self, count: u32, address: &str) -> Vec<Notice> {
        let rose = matches!(self.last_active, Some(prev) if count > prev);
        self.last_active = Some(count);
        if rose {
            vec![Notice { kind: Kind::Call, who: String::new(), text: String::new(), phone: String::new(), url: address.to_string() }]
        } else {
            Vec::new()
        }
    }
}

/// Watches the recent-events endpoint for new text messages.
#[derive(Default)]
pub struct SmsWatch {
    last_id: Option<u64>,
    /// None: not known; Some(false): this Bageo has no such endpoint.
    pub supported: Option<bool>,
    skipped: u32,
}

impl SmsWatch {
    pub fn reset(&mut self) {
        *self = SmsWatch::default();
    }

    /// Whether to ask this round. After a 404 only every 12th round asks again.
    pub fn due(&mut self) -> bool {
        if self.supported == Some(false) {
            self.skipped += 1;
            if self.skipped < SMS_RETRY_ROUNDS {
                return false;
            }
            self.skipped = 0;
        }
        true
    }

    /// Value for the `after` query parameter, if any.
    pub fn after(&self) -> Option<u64> {
        self.last_id
    }

    pub fn on_not_found(&mut self) {
        self.supported = Some(false);
        self.skipped = 0;
    }

    pub fn on_error(&mut self) {
        self.supported = None;
    }

    /// Handles a successful answer. The first answer only records where the list ends.
    pub fn on_response(&mut self, body: &Value, address: &str) -> Vec<Notice> {
        self.supported = Some(true);
        let events = body.get("events").and_then(Value::as_array).map(|a| a.as_slice()).unwrap_or(&[]);
        let newest = events.iter().filter_map(|e| e.get("id").and_then(Value::as_u64)).max();
        let last = body.get("last_id").and_then(Value::as_u64).or(newest).or(self.last_id).unwrap_or(0);
        let first = self.last_id.is_none();
        self.last_id = Some(last);
        if first {
            return Vec::new();
        }
        events
            .iter()
            .filter(|e| e.get("kind").and_then(Value::as_str) == Some("sms"))
            .map(|e| Notice {
                kind: Kind::Sms,
                who: text_of(e.get("from")).to_string(),
                text: e.get("text").and_then(Value::as_str).unwrap_or("").to_string(),
                phone: String::new(),
                url: sms_url(address, text_of(e.get("from"))),
            })
            .collect()
    }
}

/// The summary without its first line (who the caller is, already in the title) when there are
/// three lines or more; shorter summaries stay whole. Blank lines are dropped.
pub fn summary_text(summary: &str) -> String {
    let lines: Vec<&str> = summary.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let skip = if lines.len() >= 3 { 1 } else { 0 };
    lines[skip..].join("\n")
}

/// Watches the list of calls for summaries that have just been written.
#[derive(Default)]
pub struct SummaryWatch {
    seen: HashSet<String>,
    primed: bool,
    /// Rounds still to skip before the next read.
    wait: u32,
}

impl SummaryWatch {
    pub fn reset(&mut self) {
        *self = SummaryWatch::default();
    }

    /// Whether to read the list this round (the first round after a reset always does).
    pub fn due(&mut self) -> bool {
        if self.wait == 0 {
            self.wait = SUMMARY_EVERY - 1;
            true
        } else {
            self.wait -= 1;
            false
        }
    }

    /// Handles a successful answer from the call list. The first answer only records the
    /// summaries that already exist. Oldest notice first, so the newest call ends up on screen.
    pub fn on_list(&mut self, body: &Value, address: &str, lang: &str) -> Vec<Notice> {
        let Some(calls) = body.get("calls").and_then(Value::as_array) else {
            return Vec::new();
        };
        let contacts = body.get("contacts_by_number");
        let mut out = Vec::new();
        let mut current: Vec<String> = Vec::new();
        for c in calls {
            let id = text_of(c.get("id"));
            if id.is_empty() {
                continue;
            }
            current.push(id.to_string());
            let finished = !text_of(c.get("status")).eq_ignore_ascii_case("active");
            let summary = text_of(c.get("summary"));
            if !finished || summary.is_empty() || !self.seen.insert(id.to_string()) || !self.primed {
                continue;
            }
            let mut who = caller_name(text_of(c.get("caller_id")), contacts);
            if who.is_empty() {
                who = i18n::t(lang, "n_hidden");
            }
            out.push(Notice {
                kind: Kind::Summary,
                who,
                text: summary_text(summary),
                phone: String::new(),
                url: call_url(address, id),
            });
        }
        self.primed = true;
        if self.seen.len() > MAX_SEEN {
            self.seen.retain(|id| current.contains(id));
        }
        out.reverse();
        out
    }
}

/// What the toast page receives.
#[derive(Serialize, Clone, PartialEq, Debug)]
pub struct ToastData {
    pub kind: Kind,
    pub who: String,
    pub text: String,
    pub phone: String,
    pub more: u32,
    pub lang: String,
}

/// The notification currently on screen, and how many it replaced.
#[derive(Default)]
pub struct ToastState {
    pub current: Option<Notice>,
    pub more: u32,
    pub lang: String,
    /// Changes whenever the notification changes; used by the 45 second failsafe.
    pub generation: u64,
}

impl ToastState {
    pub fn push(&mut self, notice: Notice, lang: &str) {
        if self.current.is_some() {
            self.more = self.more.saturating_add(1);
        }
        self.current = Some(notice);
        self.lang = lang.to_string();
        self.generation += 1;
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.more = 0;
        self.generation += 1;
    }

    pub fn data(&self) -> Option<ToastData> {
        self.current.as_ref().map(|n| ToastData {
            kind: n.kind,
            who: n.who.clone(),
            text: n.text.clone(),
            phone: n.phone.clone(),
            more: self.more,
            lang: self.lang.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Level, PhoneState, Reason, Snapshot};
    use serde_json::json;

    const ADDR: &str = "http://192.168.1.20:8787/";

    fn phone(serial: &str, name: &str) -> Phone {
        Phone { serial: serial.into(), name: name.into(), state: PhoneState::Ready }
    }

    fn two_phones() -> Vec<Phone> {
        vec![phone("A1", "Pixel A"), phone("B2", "Pixel B")]
    }

    fn live(calls: Value) -> Value {
        json!({"calls": calls, "contacts_by_number": {"+15550100": {"name": "Alex"}, "+15550101": {"name": ""}}})
    }

    #[test]
    fn first_live_answer_only_records() {
        let mut w = CallWatch::default();
        let body = live(json!([{"id": "c1", "caller_id": "+15550100", "serial": "A1", "status": "ringing"}]));
        assert!(w.on_live(&body, &two_phones(), ADDR).is_empty());
        assert!(w.on_live(&body, &two_phones(), ADDR).is_empty());
    }

    #[test]
    fn new_id_notifies_once() {
        let mut w = CallWatch::default();
        assert!(w.on_live(&live(json!([])), &two_phones(), ADDR).is_empty());
        let body = live(json!([{"id": "c1", "caller_id": "+15550199", "serial": "B2", "status": "ringing"}]));
        let n = w.on_live(&body, &two_phones(), ADDR);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].kind, Kind::Call);
        assert_eq!(n[0].who, "+15550199");
        assert_eq!(n[0].phone, "Pixel B");
        assert_eq!(n[0].url, "http://192.168.1.20:8787/calls/#call=c1");
        // Same id again, even after the status changes: nothing.
        let body = live(json!([{"id": "c1", "caller_id": "+15550199", "serial": "B2", "status": "active"}]));
        assert!(w.on_live(&body, &two_phones(), ADDR).is_empty());
        // A second call arrives next to the first: only the new one.
        let body = live(json!([
            {"id": "c1", "caller_id": "+15550199", "serial": "B2", "status": "active"},
            {"id": "c2", "caller_id": "+15550198", "serial": "A1", "status": "ringing"}
        ]));
        let n = w.on_live(&body, &two_phones(), ADDR);
        assert_eq!(n.len(), 1);
        assert!(n[0].url.ends_with("#call=c2"));
    }

    #[test]
    fn calls_placed_or_turned_away_by_bageo_do_not_notify() {
        let mut w = CallWatch::default();
        w.on_live(&live(json!([])), &[], ADDR);
        let body = live(json!([
            {"id": "cb", "caller_id": "+15550100", "serial": "A1", "events": ["ringing", "ai_callback"]},
            {"id": "er", "caller_id": "+15550101", "serial": "A1", "events": ["ai_callback", "ai_errand"]},
            {"id": "rj", "caller_id": "+15550102", "serial": "A1", "status": "ringing", "events": ["reject_list"]},
            {"id": "in", "caller_id": "+15550103", "serial": "A1", "events": ["ringing", "reject_list_declined"]}
        ]));
        let n = w.on_live(&body, &[], ADDR);
        assert_eq!(n.len(), 1);
        assert!(n[0].url.ends_with("#call=in"));
        // A call that was skipped once never comes back as a notice.
        assert!(w.on_live(&body, &[], ADDR).is_empty());
    }

    #[test]
    fn contact_name_wins_and_empty_name_falls_back_to_number() {
        let mut w = CallWatch::default();
        w.on_live(&live(json!([])), &[], ADDR);
        let body = live(json!([
            {"id": "c1", "caller_id": "+15550100", "serial": "A1"},
            {"id": "c2", "caller_id": " +15550101 ", "serial": "A1"}
        ]));
        let n = w.on_live(&body, &[], ADDR);
        assert_eq!(n[0].who, "Alex");
        assert_eq!(n[1].who, "+15550101");
    }

    #[test]
    fn hidden_numbers_give_empty_who() {
        for id in ["", "  ", "unknown", "Private", "ANONYMOUS", "restricted"] {
            assert_eq!(caller_name(id, None), "", "{id:?}");
        }
        assert_eq!(caller_name("+15550123", None), "+15550123");
    }

    #[test]
    fn url_uses_call_id_only_when_it_is_safe() {
        assert_eq!(call_url(ADDR, "abc-12_X"), "http://192.168.1.20:8787/calls/#call=abc-12_X");
        assert_eq!(call_url("http://192.168.1.20:8787", "c1"), "http://192.168.1.20:8787/calls/#call=c1");
        assert_eq!(call_url("http://192.168.1.20:8787/calls/", "c1"), "http://192.168.1.20:8787/calls/#call=c1");
        for bad in ["", "a b", "a/b", "a#b", "a\"b", "é"] {
            assert_eq!(call_url(ADDR, bad), ADDR, "{bad:?}");
        }
        assert_eq!(call_url("", "c1"), "");
    }

    #[test]
    fn phone_name_only_with_more_than_one_phone() {
        let mut w = CallWatch::default();
        w.on_live(&live(json!([])), &[], ADDR);
        let body = live(json!([{"id": "c1", "caller_id": "+15550123", "serial": "A1"}]));
        let n = w.on_live(&body, &[phone("A1", "Pixel A")], ADDR);
        assert_eq!(n[0].phone, "");

        let mut w = CallWatch::default();
        w.on_live(&live(json!([])), &[], ADDR);
        let n = w.on_live(&body, &two_phones(), ADDR);
        assert_eq!(n[0].phone, "Pixel A");
    }

    #[test]
    fn locked_uses_rising_call_count() {
        let mut w = CallWatch::default();
        assert!(w.on_active_count(1, ADDR).is_empty()); // first answer only records
        assert!(w.on_active_count(1, ADDR).is_empty());
        let n = w.on_active_count(2, ADDR);
        assert_eq!(n.len(), 1);
        assert_eq!((n[0].kind, n[0].who.as_str(), n[0].phone.as_str(), n[0].url.as_str()), (Kind::Call, "", "", ADDR));
        assert!(w.on_active_count(0, ADDR).is_empty());
        assert_eq!(w.on_active_count(1, ADDR).len(), 1);
    }

    #[test]
    fn reset_makes_the_next_answer_a_recording_one() {
        let mut w = CallWatch::default();
        w.on_live(&live(json!([])), &[], ADDR);
        w.reset();
        let body = live(json!([{"id": "c1", "caller_id": "+15550123", "serial": "A1"}]));
        assert!(w.on_live(&body, &[], ADDR).is_empty());
    }

    #[test]
    fn sms_first_answer_records_last_id_only() {
        let mut s = SmsWatch::default();
        assert_eq!(s.after(), None);
        let body = json!({"ok": true, "last_id": 123, "events": [{"id": 123, "kind": "sms", "from": "+15550100", "text": "old"}]});
        assert!(s.on_response(&body, ADDR).is_empty());
        assert_eq!(s.after(), Some(123));
        assert_eq!(s.supported, Some(true));
    }

    #[test]
    fn sms_takes_only_sms_events_and_advances_last_id() {
        let mut s = SmsWatch::default();
        s.on_response(&json!({"ok": true, "last_id": 10, "events": []}), ADDR);
        let body = json!({"ok": true, "last_id": 13, "events": [
            {"id": 11, "at_ms": 0, "kind": "sms", "title": "t", "from": " +15550100 ", "text": "hello"},
            {"id": 12, "at_ms": 0, "kind": "message", "title": "t", "from": "x", "text": "chat"},
            {"id": 13, "at_ms": 0, "kind": "sms", "title": "t", "from": "+15550101", "text": "again"}
        ]});
        let n = s.on_response(&body, ADDR);
        assert_eq!(n.len(), 2);
        assert_eq!((n[0].kind, n[0].who.as_str(), n[0].text.as_str()), (Kind::Sms, "+15550100", "hello"));
        assert_eq!(n[0].url, "http://192.168.1.20:8787/calls/#sms/%2B15550100");
        assert_eq!(n[1].url, "http://192.168.1.20:8787/calls/#sms/%2B15550101");
        assert_eq!(n[1].text, "again");
        assert_eq!(s.after(), Some(13));
        assert!(s.on_response(&json!({"ok": true, "last_id": 13, "events": []}), ADDR).is_empty());
    }

    #[test]
    fn sms_link_opens_the_text_messages() {
        assert_eq!(sms_url(ADDR, ""), "http://192.168.1.20:8787/calls/#sms");
        assert_eq!(sms_url("http://192.168.1.20:8787", " "), "http://192.168.1.20:8787/calls/#sms");
        assert_eq!(sms_url("http://192.168.1.20:8787/calls/", "0912345678"), "http://192.168.1.20:8787/calls/#sms/0912345678");
        assert_eq!(sms_url(ADDR, "+1 (650) 555-0142"), "http://192.168.1.20:8787/calls/#sms/%2B1%20%28650%29%20555-0142");
        assert_eq!(sms_url(ADDR, "王小明"), "http://192.168.1.20:8787/calls/#sms/%E7%8E%8B%E5%B0%8F%E6%98%8E");
        assert_eq!(sms_url(ADDR, "Bank/Info#1"), "http://192.168.1.20:8787/calls/#sms/Bank%2FInfo%231");
        assert_eq!(sms_url("", "0912345678"), "");
    }

    #[test]
    fn sms_404_marks_unsupported_and_retries_every_12_rounds() {
        let mut s = SmsWatch::default();
        assert!(s.due());
        s.on_not_found();
        assert_eq!(s.supported, Some(false));
        for round in 1..12 {
            assert!(!s.due(), "round {round}");
        }
        assert!(s.due());
        assert_eq!(s.supported, Some(false));
        s.on_response(&json!({"ok": true, "last_id": 1, "events": []}), ADDR);
        assert_eq!(s.supported, Some(true));
        assert!(s.due());
        s.on_error();
        assert_eq!(s.supported, None);
    }

    #[test]
    fn more_counts_replaced_notices_and_resets_on_close() {
        let n = |who: &str| Notice { kind: Kind::Sms, who: who.into(), text: "hi".into(), phone: String::new(), url: ADDR.into() };
        let mut t = ToastState::default();
        assert!(t.data().is_none());
        t.push(n("a"), "en");
        assert_eq!(t.data().unwrap().more, 0);
        t.push(n("b"), "ja");
        t.push(n("c"), "ja");
        let d = t.data().unwrap();
        assert_eq!((d.who.as_str(), d.more, d.lang.as_str()), ("c", 2, "ja"));
        let before = t.generation;
        t.clear();
        assert!(t.data().is_none());
        assert_eq!(t.more, 0);
        assert!(t.generation > before);
        t.push(n("d"), "en");
        assert_eq!(t.data().unwrap().more, 0);
    }

    #[test]
    fn toast_data_serialises_for_the_page() {
        let mut t = ToastState::default();
        t.push(Notice { kind: Kind::Call, who: "Alex".into(), text: String::new(), phone: "Pixel A".into(), url: ADDR.into() }, "zh-TW");
        let v = serde_json::to_value(t.data().unwrap()).unwrap();
        assert_eq!(v, json!({"kind": "call", "who": "Alex", "text": "", "phone": "Pixel A", "more": 0, "lang": "zh-TW"}));
    }

    #[test]
    fn snapshot_reports_sms_support_as_null_true_false() {
        let mut s = Snapshot::blank(Level::Ok, Reason::Ready, "en", false, "");
        assert!(serde_json::to_value(&s).unwrap()["sms_supported"].is_null());
        s.sms_supported = Some(false);
        assert_eq!(serde_json::to_value(&s).unwrap()["sms_supported"], false);
        s.sms_supported = Some(true);
        assert_eq!(serde_json::to_value(&s).unwrap()["sms_supported"], true);
    }

    const FOUR: &str = "Contact: Alex (friend)\nWhat: Asks you to call back\nAI reply: Said you would call\nTo do: Call Alex";

    fn list(calls: Value) -> Value {
        json!({"calls": calls, "contacts_by_number": {"+15550100": {"name": "Alex"}, "+15550101": {"name": ""}}})
    }

    fn done(id: &str, caller: &str, summary: Value) -> Value {
        json!({"id": id, "caller_id": caller, "status": "ended", "summary": summary})
    }

    #[test]
    fn summary_first_answer_records_existing_summaries_without_notifying() {
        let mut w = SummaryWatch::default();
        let body = list(json!([done("c2", "+15550100", json!(FOUR)), done("c1", "+15550199", json!("Contact: Bo\nWhat: Sales"))]));
        assert!(w.on_list(&body, ADDR, "en").is_empty());
        assert!(w.on_list(&body, ADDR, "en").is_empty());
    }

    #[test]
    fn new_summary_notifies_once() {
        let mut w = SummaryWatch::default();
        assert!(w.on_list(&list(json!([done("c1", "+15550199", json!("Contact: Bo\nWhat: Sales"))])), ADDR, "en").is_empty());
        let body = list(json!([done("c2", "+15550100", json!(FOUR)), done("c1", "+15550199", json!("Contact: Bo\nWhat: Sales"))]));
        let n = w.on_list(&body, ADDR, "en");
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].kind, Kind::Summary);
        assert_eq!(n[0].url, "http://192.168.1.20:8787/calls/#call=c2");
        assert_eq!(n[0].phone, "");
        assert!(w.on_list(&body, ADDR, "en").is_empty());
    }

    #[test]
    fn summary_waits_until_it_is_written_and_the_call_is_over() {
        let mut w = SummaryWatch::default();
        w.on_list(&list(json!([])), ADDR, "en");
        // Ended, summary not written yet (null, missing or blank): nothing, and nothing is used up.
        for pending in [done("c1", "+15550100", Value::Null), json!({"id": "c1", "caller_id": "+15550100", "status": "ended"}), done("c1", "+15550100", json!("  "))] {
            assert!(w.on_list(&list(json!([pending])), ADDR, "en").is_empty());
        }
        // Still on the phone: nothing, even if a summary is already there.
        let live = json!({"id": "c1", "caller_id": "+15550100", "status": "active", "summary": "Contact: Alex\nWhat: Hi"});
        assert!(w.on_list(&list(json!([live])), ADDR, "en").is_empty());
        // The summary arrives after the call: one notice.
        let n = w.on_list(&list(json!([done("c1", "+15550100", json!("Contact: Alex\nWhat: Hi"))])), ADDR, "en");
        assert_eq!(n.len(), 1);
        assert!(w.on_list(&list(json!([done("c1", "+15550100", json!("Contact: Alex\nWhat: Hi"))])), ADDR, "en").is_empty());
    }

    #[test]
    fn summary_title_is_the_contact_name_before_the_number() {
        let mut w = SummaryWatch::default();
        w.on_list(&list(json!([])), ADDR, "en");
        let body = list(json!([
            done("c1", "+15550100", json!("Contact: a\nWhat: b")),
            done("c2", "+15550101", json!("Contact: a\nWhat: b")),
            done("c3", "+15550123", json!("Contact: a\nWhat: b"))
        ]));
        let n = w.on_list(&body, ADDR, "en");
        // Oldest first, so the newest call is the one left on screen.
        let who: Vec<&str> = n.iter().map(|x| x.who.as_str()).collect();
        assert_eq!(who, ["+15550123", "+15550101", "Alex"]);
    }

    #[test]
    fn summary_of_a_hidden_number_says_so_in_the_toast_language() {
        let mut w = SummaryWatch::default();
        w.on_list(&list(json!([])), ADDR, "en");
        let body = list(json!([done("c1", "unknown", json!("Contact: a\nWhat: b")), done("c2", "", json!("Contact: a\nWhat: b"))]));
        let n = w.on_list(&body, ADDR, "zh-TW");
        assert_eq!(n.len(), 2);
        assert!(n.iter().all(|x| x.who == "未顯示號碼"));
        let mut w = SummaryWatch::default();
        w.on_list(&list(json!([])), ADDR, "en");
        let n = w.on_list(&list(json!([done("c1", "private", json!("Contact: a\nWhat: b"))])), ADDR, "en");
        assert_eq!(n[0].who, "Hidden number");
    }

    #[test]
    fn summary_text_drops_the_first_line_from_three_lines_up() {
        assert_eq!(summary_text(FOUR), "What: Asks you to call back\nAI reply: Said you would call\nTo do: Call Alex");
        assert_eq!(summary_text("Contact: Alex\nWhat: Hi\nTo do: Nothing"), "What: Hi\nTo do: Nothing");
        assert_eq!(summary_text("Contact: Alex\r\n\r\nWhat: Hi\r\n  To do: Nothing  \r\n"), "What: Hi\nTo do: Nothing");
        // One or two lines stay whole.
        assert_eq!(summary_text("Contact: Alex\nWhat: Hi"), "Contact: Alex\nWhat: Hi");
        assert_eq!(summary_text("Just one line"), "Just one line");
        // Any language works: only the position of the lines counts.
        assert_eq!(summary_text("\u{5c0d}\u{65b9}\u{ff1a}\u{5c0f}\u{9673}\n\u{4e8b}\u{60c5}\u{ff1a}A\n\u{8981}\u{8655}\u{7406}\u{ff1a}B"), "\u{4e8b}\u{60c5}\u{ff1a}A\n\u{8981}\u{8655}\u{7406}\u{ff1a}B");
    }

    #[test]
    fn summary_reset_and_odd_answers() {
        let mut w = SummaryWatch::default();
        // An answer without a call list records nothing and does not count as the first answer.
        assert!(w.on_list(&json!({"ok": true}), ADDR, "en").is_empty());
        w.on_list(&list(json!([])), ADDR, "en");
        let body = list(json!([done("c1", "+15550100", json!("Contact: a\nWhat: b"))]));
        w.reset();
        // After a reset (address changed, setting turned off and on) the next answer only records.
        assert!(w.on_list(&body, ADDR, "en").is_empty());
        assert!(w.on_list(&body, ADDR, "en").is_empty());
        // Calls without an id are skipped.
        assert!(w.on_list(&list(json!([{"status": "ended", "summary": "x"}])), ADDR, "en").is_empty());
    }

    #[test]
    fn summary_list_is_read_every_second_round() {
        let mut w = SummaryWatch::default();
        let rounds: Vec<bool> = (0..5).map(|_| w.due()).collect();
        assert_eq!(rounds, [true, false, true, false, true]);
        w.reset();
        assert!(w.due());
    }

    #[test]
    fn summary_kind_serialises_for_the_page() {
        let mut t = ToastState::default();
        t.push(Notice { kind: Kind::Summary, who: "Alex".into(), text: "What: Hi".into(), phone: String::new(), url: ADDR.into() }, "en");
        let v = serde_json::to_value(t.data().unwrap()).unwrap();
        assert_eq!(v["kind"], "summary");
        assert_eq!(v["text"], "What: Hi");
    }
}

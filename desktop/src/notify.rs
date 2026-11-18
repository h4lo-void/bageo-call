//! Desktop notifications: decides what is new in Bageo's answers. No network or window code here.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

use crate::status::Phone;

#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Call,
    Sms,
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
            if self.seen.insert(id.to_string()) && self.live_primed {
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
                url: address.to_string(),
            })
            .collect()
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
        assert_eq!((n[0].kind, n[0].who.as_str(), n[0].text.as_str(), n[0].url.as_str()), (Kind::Sms, "+15550100", "hello", ADDR));
        assert_eq!(n[1].text, "again");
        assert_eq!(s.after(), Some(13));
        assert!(s.on_response(&json!({"ok": true, "last_id": 13, "events": []}), ADDR).is_empty());
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
}

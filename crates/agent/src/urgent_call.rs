//! Whitelist urgent call (setting `whitelist_urgent_call`, trial).
//!
//! A Whitelist caller (whitelist_mode emergency) says it is urgent: the AI asks
//! them to hold on, the agent sends the phone add_call on the call's media
//! connection, and the phone dials the owner's number (whitelist_forward_number)
//! from the SIM of this call and merges the two calls. Merged: the AI leaves the
//! call (it stays up and is recorded). Anything else: the phone ends the second
//! call, takes this one off hold and the AI tells the caller the owner did not
//! answer.
//!
//! Agent -> phone: {"type":"add_call","id":…,"number":…,"merge":true,"answer_timeout_ms":25000}
//! Phone -> agent: {"type":"add_call_result","id":…,"status":"dialing"|"active"|"merged"|"failed","reason":…}

use crate::locale::Lang;
use crate::settings_store::RuntimeSettings;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// How long the phone lets the owner's phone ring.
pub const OWNER_ANSWER_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the agent waits for the phone to settle it (dial, ring, merge)
/// before it treats the attempt as failed.
pub const RESULT_WAIT: Duration = Duration::from_secs(50);
/// Longest wait for the AI's "hold on" line to play before the phone dials.
pub const HOLD_LINE_MAX_WAIT: Duration = Duration::from_secs(10);

/// The owner's number to dial, only when the trial is on: whitelist_mode
/// emergency, whitelist_urgent_call on and whitelist_forward_number filled.
pub fn number_to_call(rt: &RuntimeSettings) -> Option<String> {
    let number = rt.whitelist_forward_number.trim();
    (rt.whitelist_mode == "emergency" && rt.whitelist_urgent_call && !number.is_empty()).then(|| number.to_string())
}

/// The phone's add_call command.
pub fn add_call_payload(id: &str, number: &str) -> Value {
    json!({
        "type": "add_call",
        "id": id,
        "number": number,
        "merge": true,
        "answer_timeout_ms": OWNER_ANSWER_TIMEOUT.as_millis() as u64,
    })
}

/// One add_call_result from the phone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Dialing,
    Active,
    Merged,
    Failed(String),
}

/// A phone's add_call_result: its id and what happened. The reason is kept to a
/// short code (letters, digits, _ and -).
pub fn parse_result(msg: &Value) -> Option<(String, Report)> {
    if msg.get("type")?.as_str()? != "add_call_result" {
        return None;
    }
    let id = msg.get("id")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let report = match msg.get("status")?.as_str()? {
        "dialing" => Report::Dialing,
        "active" => Report::Active,
        "merged" => Report::Merged,
        "failed" => Report::Failed(reason_code(msg.get("reason").and_then(|v| v.as_str()).unwrap_or(""))),
        _ => return None,
    };
    Some((id.to_string(), report))
}

fn reason_code(s: &str) -> String {
    let code: String = s
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(48)
        .collect();
    if code.is_empty() {
        "unknown".to_string()
    } else {
        code
    }
}

/// What the agent does after a report or the wait running out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Keep waiting.
    Wait,
    /// The owner is on the call: the AI leaves it.
    Merged,
    /// Not merged: the AI comes back and says so.
    Failed(String),
}

/// Where one attempt stands. Settled once (merged or failed); later reports
/// are ignored.
#[derive(Debug, Default)]
pub struct Tracker {
    dialing: bool,
    active: bool,
    settled: bool,
}

impl Tracker {
    pub fn on_report(&mut self, report: Report) -> Step {
        if self.settled {
            return Step::Wait;
        }
        match report {
            Report::Dialing => {
                self.dialing = true;
                Step::Wait
            }
            Report::Active => {
                self.active = true;
                Step::Wait
            }
            Report::Merged => {
                self.settled = true;
                Step::Merged
            }
            Report::Failed(reason) => {
                self.settled = true;
                Step::Failed(reason)
            }
        }
    }

    /// No final result in RESULT_WAIT, or the phone could not be reached.
    pub fn on_timeout(&mut self) -> Step {
        if self.settled {
            return Step::Wait;
        }
        self.settled = true;
        let reason = if self.active {
            "merge_timeout"
        } else if self.dialing {
            "no_result"
        } else {
            "phone_no_reply"
        };
        Step::Failed(reason.to_string())
    }

    pub fn settled(&self) -> bool {
        self.settled
    }
}

/// Shared by the agent's watcher and the Live session of one call.
#[derive(Debug, Default)]
pub struct CallCtl {
    /// The phone is dialing the owner: no AI audio goes out, the caller's audio
    /// does not go to the model, and the session neither nudges nor hangs up.
    paused: AtomicBool,
    /// Merged: the session ends without a hangup.
    handed: AtomicBool,
    /// A turn for the model (the "did not answer" line), sent once.
    say: Mutex<Option<String>>,
}

impl CallCtl {
    pub fn pause(&self) {
        self.paused.store(true, Ordering::Relaxed);
    }

    /// Back to the caller with one turn for the model.
    pub fn resume_with(&self, turn: String) {
        if let Ok(mut g) = self.say.lock() {
            *g = Some(turn);
        }
        self.paused.store(false, Ordering::Relaxed);
    }

    pub fn hand_over(&self) {
        self.handed.store(true, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn is_handed(&self) -> bool {
        self.handed.load(Ordering::Relaxed)
    }

    /// AI audio and the caller's audio to the model are held back.
    pub fn ai_quiet(&self) -> bool {
        self.is_paused() || self.is_handed()
    }

    /// The agent must not hang up now (dialing the owner, or the owner is on).
    pub fn blocks_hangup(&self) -> bool {
        self.ai_quiet()
    }

    pub fn take_say(&self) -> Option<String> {
        self.say.lock().ok().and_then(|mut g| g.take())
    }
}

type Waiting = Mutex<HashMap<String, mpsc::UnboundedSender<Report>>>;

fn waiting() -> &'static Waiting {
    static W: OnceLock<Waiting> = OnceLock::new();
    W.get_or_init(Default::default)
}

/// Results for attempt id arrive on the returned receiver until unregister.
pub fn register(id: &str) -> mpsc::UnboundedReceiver<Report> {
    let (tx, rx) = mpsc::unbounded_channel();
    if let Ok(mut g) = waiting().lock() {
        g.insert(id.to_string(), tx);
    }
    rx
}

pub fn unregister(id: &str) {
    if let Ok(mut g) = waiting().lock() {
        g.remove(id);
    }
}

/// A phone's add_call_result (media connection): true when an attempt waits for it.
pub fn deliver(msg: &Value) -> bool {
    let Some((id, report)) = parse_result(msg) else {
        return false;
    };
    let tx = waiting().lock().ok().and_then(|g| g.get(&id).cloned());
    match tx {
        Some(tx) => tx.send(report).is_ok(),
        None => false,
    }
}

/// The turn that tells the model the owner did not come on, and what to say.
pub fn not_answered_turn(lang: Lang, assistant: bool, owner: &str) -> String {
    let owner = crate::prompts::owner_label(lang, owner);
    let text = match (lang, assistant) {
        (Lang::ZhTw, false) => "（系統：剛才沒有接通。現在用一句話跟對方說「抱歉，剛剛沒接通，我等一下馬上打給你。」不要說已經接通，也不要說要轉給誰。）".to_string(),
        (Lang::ZhTw, true) => format!("（系統：{owner}沒有接電話。現在用一句話跟對方說「{owner}現在沒接，我已經通知他了，請他馬上打給你。」不要說已經接通。）"),
        (Lang::ZhCn, false) => "（系统：刚才没有接通。现在用一句话跟对方说「抱歉，刚刚没接通，我等一下马上打给你。」不要说已经接通，也不要说要转给谁。）".to_string(),
        (Lang::ZhCn, true) => format!("（系统：{owner}没有接电话。现在用一句话跟对方说「{owner}现在没接，我已经通知他了，请他马上打给你。」不要说已经接通。）"),
        (_, false) => "(System: the call did not go through. Say in one sentence, in the caller's language: \"Sorry, that didn't go through. I'll call you right back.\" Do not say anyone is on the line.)".to_string(),
        (_, true) => format!("(System: {owner} did not answer. Say in one sentence, in the caller's language: \"{owner} isn't picking up. I've let them know and asked them to call you straight back.\" Do not say anyone is on the line.)"),
    };
    crate::prompts::client_turn(&text)
}

/// Last four digits only, for logs.
pub fn masked(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(|c| c.is_ascii_digit()).collect();
    let tail: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    format!("***{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(mode: &str, on: bool, number: &str) -> RuntimeSettings {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.whitelist_mode = mode.to_string();
        rt.whitelist_urgent_call = on;
        rt.whitelist_forward_number = number.to_string();
        rt
    }

    #[test]
    fn only_emergency_mode_with_the_setting_on_and_a_number_dials() {
        assert_eq!(number_to_call(&rt("emergency", true, " +15550001111 ")).as_deref(), Some("+15550001111"));
        assert_eq!(number_to_call(&rt("emergency", false, "+15550001111")), None);
        assert_eq!(number_to_call(&rt("emergency", true, "   ")), None);
        assert_eq!(number_to_call(&rt("emergency", true, "")), None);
        assert_eq!(number_to_call(&rt("direct", true, "+15550001111")), None);
        assert_eq!(number_to_call(&rt("off", true, "+15550001111")), None);
    }

    #[test]
    fn the_command_carries_number_id_and_merge() {
        let v = add_call_payload("u-1", "+15550001111");
        assert_eq!(v["type"], "add_call");
        assert_eq!(v["id"], "u-1");
        assert_eq!(v["number"], "+15550001111");
        assert_eq!(v["merge"], true);
        assert_eq!(v["answer_timeout_ms"], 25_000);
    }

    #[test]
    fn results_parse_and_reasons_stay_short_codes() {
        let r = |s: Value| parse_result(&s);
        assert_eq!(r(json!({"type":"add_call_result","id":"u-1","status":"dialing"})), Some(("u-1".into(), Report::Dialing)));
        assert_eq!(r(json!({"type":"add_call_result","id":"u-1","status":"merged"})), Some(("u-1".into(), Report::Merged)));
        assert_eq!(
            r(json!({"type":"add_call_result","id":"u-1","status":"failed","reason":"no answer; +1555"})),
            Some(("u-1".into(), Report::Failed("noanswer1555".into())))
        );
        assert_eq!(
            r(json!({"type":"add_call_result","id":"u-1","status":"failed"})),
            Some(("u-1".into(), Report::Failed("unknown".into())))
        );
        assert_eq!(r(json!({"type":"add_call_result","id":"","status":"merged"})), None);
        assert_eq!(r(json!({"type":"add_call_result","id":"u-1","status":"maybe"})), None);
        assert_eq!(r(json!({"type":"send_sms_result","id":"u-1","status":"merged"})), None);
    }

    #[test]
    fn merged_settles_once() {
        let mut t = Tracker::default();
        assert_eq!(t.on_report(Report::Dialing), Step::Wait);
        assert_eq!(t.on_report(Report::Active), Step::Wait);
        assert_eq!(t.on_report(Report::Merged), Step::Merged);
        assert!(t.settled());
        assert_eq!(t.on_report(Report::Failed("leg_ended".into())), Step::Wait);
        assert_eq!(t.on_timeout(), Step::Wait);
    }

    #[test]
    fn each_failure_comes_back_with_its_reason() {
        for reason in ["no_answer", "dial_failed", "merge_failed", "leg_ended", "cannot_hold"] {
            let mut t = Tracker::default();
            t.on_report(Report::Dialing);
            assert_eq!(t.on_report(Report::Failed(reason.into())), Step::Failed(reason.into()));
            assert_eq!(t.on_report(Report::Merged), Step::Wait, "a late merge after a failure is ignored");
        }
    }

    #[test]
    fn no_result_in_time_fails_by_how_far_it_got() {
        assert_eq!(Tracker::default().on_timeout(), Step::Failed("phone_no_reply".into()));
        let mut t = Tracker::default();
        t.on_report(Report::Dialing);
        assert_eq!(t.on_timeout(), Step::Failed("no_result".into()));
        let mut t = Tracker::default();
        t.on_report(Report::Active);
        assert_eq!(t.on_timeout(), Step::Failed("merge_timeout".into()));
    }

    #[test]
    fn results_reach_only_a_waiting_attempt() {
        let mut rx = register("u-test-1");
        assert!(deliver(&json!({"type":"add_call_result","id":"u-test-1","status":"active"})));
        assert_eq!(rx.try_recv().ok(), Some(Report::Active));
        assert!(!deliver(&json!({"type":"add_call_result","id":"u-other","status":"active"})));
        unregister("u-test-1");
        assert!(!deliver(&json!({"type":"add_call_result","id":"u-test-1","status":"merged"})));
    }

    #[test]
    fn the_ctl_pauses_hands_over_and_says_one_line() {
        let c = CallCtl::default();
        assert!(!c.blocks_hangup());
        c.pause();
        assert!(c.ai_quiet() && c.blocks_hangup());
        c.resume_with("x".into());
        assert!(!c.ai_quiet());
        assert_eq!(c.take_say().as_deref(), Some("x"));
        assert_eq!(c.take_say(), None);
        c.hand_over();
        assert!(c.is_handed() && c.blocks_hangup());
    }

    #[test]
    fn the_not_answered_line_never_says_the_call_is_through() {
        for lang in [Lang::ZhTw, Lang::ZhCn, Lang::En, Lang::Ja, Lang::Es] {
            for assistant in [false, true] {
                let t = not_answered_turn(lang, assistant, "Sam");
                let v: Value = serde_json::from_str(&t).unwrap();
                let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
                assert!(text.contains("打給你") || text.contains("打给你") || text.contains("call you"), "{text}");
            }
        }
    }

    #[test]
    fn logs_show_only_the_last_four_digits() {
        assert_eq!(masked("+1 (555) 000-1111"), "***1111");
        assert_eq!(masked("12"), "***12");
    }
}

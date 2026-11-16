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
//!                 {"type":"add_call_abort","id":…} (no final result in time)
//! Phone -> agent: {"type":"add_call_result","id":…,"status":"dialing"|"active"|"merged"|"failed","reason":…}

use crate::locale::Lang;
use crate::settings_store::RuntimeSettings;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// How long the phone lets the owner's phone ring.
pub const OWNER_ANSWER_TIMEOUT: Duration = Duration::from_secs(25);
/// How long the agent waits for the phone to settle it (dial, ring, merge)
/// before it treats the attempt as failed.
pub const RESULT_WAIT: Duration = Duration::from_secs(50);
/// Longest wait for the AI's "hold on" line to play before the phone dials.
pub const HOLD_LINE_MAX_WAIT: Duration = Duration::from_secs(10);
/// No `dialing` from the phone this soon after add_call: its app does not know the
/// command (an older version); the AI comes back at once.
pub const DIAL_ACK_WAIT: Duration = Duration::from_secs(5);
/// The AI said the hold-on line but the caller's words never named an urgent
/// matter within this long: no call, the AI says the owner will call back.
pub const HOLD_LINE_WITHOUT_TRIGGER: Duration = Duration::from_secs(6);
/// After add_call_abort: how long a final result may still come.
pub const ABORT_WAIT: Duration = Duration::from_secs(8);
/// After the attempt was given up: a late "merged" still hands the call over.
pub const LATE_MERGE_WAIT: Duration = Duration::from_secs(90);

/// The owner's number to dial, only when the trial is on: whitelist_mode
/// emergency, whitelist_urgent_call on and whitelist_forward_number filled.
pub fn number_to_call(rt: &RuntimeSettings) -> Option<String> {
    let number = rt.whitelist_forward_number.trim();
    (rt.whitelist_mode == "emergency" && rt.whitelist_urgent_call && !number.is_empty()).then(|| number.to_string())
}

/// The owner's number as the phone may dial it from the call's SIM (`sim`, ISO
/// country; None when unknown): the errands check (no emergency, short,
/// premium-rate or MMI codes, nothing that goes abroad from home). Unknown SIM:
/// only a full +number is taken, checked against its own country.
pub fn check_owner_number(number: &str, sim: Option<&str>) -> Result<String, &'static str> {
    if number.contains('*') || number.contains('#') {
        return Err("bad_number");
    }
    let region = match sim.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_ascii_lowercase(),
        None => {
            let clean = crate::errands::clean_number(number).ok_or("bad_number")?;
            if !clean.starts_with('+') {
                return Err("bad_number");
            }
            crate::phone_region::normalize(&clean, None).ok_or("bad_number")?.country.to_string()
        }
    };
    crate::errands::check_number(number, &region)
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

/// The phone's add_call_abort: end the second call and take this one off hold
/// (merged already: it answers "merged").
pub fn abort_payload(id: &str) -> Value {
    json!({ "type": "add_call_abort", "id": id })
}

/// The AI's hold-on line (urgent_block with the call on) in what it said, in any
/// of the nine languages. Dialing waits for it.
pub fn is_hold_line(ai_said: &str) -> bool {
    let lower = ai_said.to_lowercase();
    const CUES: &[&str] = &[
        "不要掛", "不要挂", "別掛", "别挂", "先別掛", "先别挂",
        "don't hang up", "do not hang up", "dont hang up", "stay on the line",
        "切らないで", "끊지 마", "끊지 말",
        "no cuelgue", "no cuelgues", "ne raccroche", "nicht auflegen", "leg nicht auf", "legen sie nicht auf",
        "não desligue", "nao desligue",
    ];
    CUES.iter().any(|k| lower.contains(k))
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
    /// When the AI's output first held the hold-on line.
    hold_said_at: Mutex<Option<Instant>>,
    /// No phone call on this call any more (fallback taken, or tried once).
    dial_blocked: AtomicBool,
    /// The Live session ended while the AI was paused: nobody can talk to the caller.
    session_gone: AtomicBool,
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

    /// The AI's output held the hold-on line (first time counts).
    pub fn note_hold_line(&self) {
        if let Ok(mut g) = self.hold_said_at.lock() {
            g.get_or_insert_with(Instant::now);
        }
    }

    pub fn hold_said_at(&self) -> Option<Instant> {
        self.hold_said_at.lock().ok().and_then(|g| *g)
    }

    /// No (further) phone call on this call. True the first time.
    pub fn block_dial(&self) -> bool {
        !self.dial_blocked.swap(true, Ordering::Relaxed)
    }

    pub fn dial_blocked(&self) -> bool {
        self.dial_blocked.load(Ordering::Relaxed)
    }

    pub fn mark_session_gone(&self) {
        self.session_gone.store(true, Ordering::Relaxed);
    }

    pub fn session_gone(&self) -> bool {
        self.session_gone.load(Ordering::Relaxed)
    }
}

/// The hold-on line was said this long ago without the caller's words naming an
/// urgent matter (and no call started): time for the "will call you back" line.
pub fn hold_line_unbacked(ctl: &CallCtl, now: Instant) -> bool {
    !ctl.dial_blocked() && ctl.hold_said_at().is_some_and(|t| now.duration_since(t) >= HOLD_LINE_WITHOUT_TRIGGER)
}

/// What the agent does when the Live session ends (any reason, any error).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEnd {
    /// No urgent call in progress: the usual end (goodbye, retry, voicemail, hangup).
    Usual,
    /// The owner is merged in: leave the call up, nothing else.
    LeaveUp,
    /// The phone is dialing the owner (or the caller is stuck on hold): leave the
    /// call up and alert the owner — nobody is left to talk to the caller.
    LeaveUpAndAlert,
}

pub fn on_session_end(ctl: Option<&CallCtl>) -> SessionEnd {
    match ctl {
        Some(c) if c.is_handed() => SessionEnd::LeaveUp,
        Some(c) if c.is_paused() => SessionEnd::LeaveUpAndAlert,
        _ => SessionEnd::Usual,
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

/// The turn for when no call to the owner happens after the hold-on line (the
/// caller's words did not say it is urgent, or the phone cannot dial): the AI
/// says the owner will call back soon.
pub fn callback_turn(lang: Lang, assistant: bool, owner: &str) -> String {
    let owner = crate::prompts::owner_label(lang, owner);
    let text = match (lang, assistant) {
        (Lang::ZhTw, false) => "（系統：現在不轉接。用一句話跟對方說「好，我知道了，我等一下盡快打給你。」不要說已經接通，也不要說要轉給誰。）".to_string(),
        (Lang::ZhTw, true) => format!("（系統：現在不轉接。用一句話跟對方說「好，我會請{owner}盡快打給你。」不要說已經接通。）"),
        (Lang::ZhCn, false) => "（系统：现在不转接。用一句话跟对方说「好，我知道了，我等一下尽快打给你。」不要说已经接通，也不要说要转给谁。）".to_string(),
        (Lang::ZhCn, true) => format!("（系统：现在不转接。用一句话跟对方说「好，我会请{owner}尽快打给你。」不要说已经接通。）"),
        (_, false) => "(System: no call is put through now. Say in one sentence, in the caller's language: \"Okay, got it. I'll call you back as soon as I can.\" Do not say anyone is on the line.)".to_string(),
        (_, true) => format!("(System: no call is put through now. Say in one sentence, in the caller's language: \"Okay, I'll ask {owner} to call you back as soon as possible.\" Do not say anyone is on the line.)"),
    };
    crate::prompts::client_turn(&text)
}

/// What went wrong in a way the AI cannot fix: the owner is told at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    /// The Live session ended while the phone dialed the owner.
    AiEnded,
    /// The phone could not take the caller's call off hold.
    StillOnHold,
}

/// One line for the owner's urgent alert.
pub fn alert_line(alert: Alert, lang: Lang) -> &'static str {
    match (alert, lang) {
        (Alert::AiEnded, Lang::ZhTw) => "AI 中途斷線，對方可能還在線上，請立刻回電。",
        (Alert::AiEnded, Lang::ZhCn) => "AI 中途断线，对方可能还在线上，请立刻回电。",
        (Alert::AiEnded, Lang::Ja) => "AI の通話が途中で切れました。相手はまだ待っているかもしれません。すぐに折り返してください。",
        (Alert::AiEnded, Lang::Ko) => "AI 연결이 중간에 끊겼습니다. 상대가 아직 기다릴 수 있으니 바로 전화해 주세요.",
        (Alert::AiEnded, Lang::Es) => "La IA se desconectó. Quien llama puede seguir en la línea: devuelve la llamada ya.",
        (Alert::AiEnded, Lang::Fr) => "L'IA s'est coupée. L'appelant est peut-être encore en ligne : rappelez tout de suite.",
        (Alert::AiEnded, Lang::De) => "Die KI ist ausgefallen. Der Anrufer ist vielleicht noch dran: Ruf sofort zurück.",
        (Alert::AiEnded, Lang::Pt) => "A IA caiu. Quem ligou pode ainda estar na linha: retorne a ligação já.",
        (Alert::AiEnded, Lang::En) => "The AI dropped off. The caller may still be on the line: call back now.",
        (Alert::StillOnHold, Lang::ZhTw) => "對方的電話沒能從保留恢復，他可能還在等，請立刻回電。",
        (Alert::StillOnHold, Lang::ZhCn) => "对方的电话没能从保留恢复，他可能还在等，请立刻回电。",
        (Alert::StillOnHold, Lang::Ja) => "相手の通話を保留から戻せませんでした。まだ待っているかもしれません。すぐに折り返してください。",
        (Alert::StillOnHold, Lang::Ko) => "상대 통화를 보류에서 되돌리지 못했습니다. 아직 기다릴 수 있으니 바로 전화해 주세요.",
        (Alert::StillOnHold, Lang::Es) => "No se pudo quitar la espera de la llamada. Puede que siga esperando: devuelve la llamada ya.",
        (Alert::StillOnHold, Lang::Fr) => "L'appel n'a pas pu être repris. L'appelant attend peut-être encore : rappelez tout de suite.",
        (Alert::StillOnHold, Lang::De) => "Der Anruf konnte nicht aus dem Halten geholt werden. Ruf sofort zurück.",
        (Alert::StillOnHold, Lang::Pt) => "Não deu para tirar a ligação da espera. A pessoa pode ainda estar esperando: retorne já.",
        (Alert::StillOnHold, Lang::En) => "The call could not be taken off hold. The caller may still be waiting: call back now.",
    }
}

/// Title of the owner's urgent alert (same as the urgent-matter alert).
pub fn alert_title(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "白名單緊急事件通知",
        Lang::ZhCn => "白名单紧急事件通知",
        Lang::Ja => "ホワイトリスト緊急通知",
        Lang::Ko => "화이트리스트 긴급 알림",
        Lang::Es => "Alerta de emergencia (Lista blanca)",
        Lang::Fr => "Alerte d'urgence (Liste blanche)",
        Lang::De => "Whitelist-Notfallalarm",
        Lang::Pt => "Alerta de emergência (Lista branca)",
        Lang::En => "Whitelist Emergency Alert",
    }
}

/// Last four digits only, for logs.
pub fn masked(number: &str) -> String {
    let digits: Vec<char> = number.chars().filter(|c| c.is_ascii_digit()).collect();
    let tail: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    format!("***{tail}")
}

/// Longest a phone stays marked "owner on the call" without its idle status.
pub const OWNER_ON_CALL_MAX: Duration = Duration::from_secs(3 * 60 * 60);

fn owner_calls() -> &'static Mutex<HashMap<String, Instant>> {
    static M: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

/// This phone (canonical serial) is dialing the owner or has the owner merged in: any
/// new media hello or ring on it belongs to that call (the merge adds calls on the
/// phone) and gets no AI, no speech and no hangup until the phone is idle again.
pub fn mark_owner_on_call(phone: &str) {
    if let Ok(mut g) = owner_calls().lock() {
        g.insert(phone.to_string(), Instant::now());
    }
}

/// The phone reported no call (idle), or the AI came back to the caller.
pub fn clear_owner_on_call(phone: &str) -> bool {
    owner_calls().lock().ok().is_some_and(|mut g| g.remove(phone).is_some())
}

pub fn owner_on_call(phone: &str) -> bool {
    owner_on_call_at(phone, Instant::now())
}

fn owner_on_call_at(phone: &str, now: Instant) -> bool {
    owner_calls()
        .lock()
        .ok()
        .and_then(|g| g.get(phone).copied())
        .is_some_and(|t| now.duration_since(t) < OWNER_ON_CALL_MAX)
}

/// A phone's control `status`: `in_call` false clears the mark.
pub fn status_ends_owner_call(status: &Value) -> bool {
    status.get("in_call").and_then(Value::as_bool) == Some(false)
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

    // ---- reviewer additions ----

    #[test]
    fn reviewer_default_settings_never_dial() {
        let mut rt = RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.whitelist_mode = "emergency".to_string();
        rt.whitelist_forward_number = "+15550001111".to_string();
        assert!(!rt.whitelist_urgent_call, "the trial is off unless the owner turns it on");
        assert_eq!(number_to_call(&rt), None);
    }

    #[test]
    fn reviewer_only_one_settlement_whatever_the_order() {
        let mut t = Tracker::default();
        let steps: Vec<Step> = [Report::Merged, Report::Dialing, Report::Active, Report::Merged, Report::Failed("x".into())]
            .into_iter()
            .map(|r| t.on_report(r))
            .collect();
        assert_eq!(steps, vec![Step::Merged, Step::Wait, Step::Wait, Step::Wait, Step::Wait]);
        assert_eq!(t.on_timeout(), Step::Wait);

        let mut t = Tracker::default();
        let steps: Vec<Step> = [
            Report::Failed("no_answer".into()),
            Report::Failed("no_answer".into()),
            Report::Merged,
            Report::Active,
        ]
        .into_iter()
        .map(|r| t.on_report(r))
        .collect();
        assert_eq!(steps, vec![Step::Failed("no_answer".into()), Step::Wait, Step::Wait, Step::Wait]);
        assert_eq!(t.on_timeout(), Step::Wait);

        // Repeated progress reports never settle; the timeout names how far it got.
        let mut t = Tracker::default();
        for _ in 0..3 {
            assert_eq!(t.on_report(Report::Active), Step::Wait);
            assert_eq!(t.on_report(Report::Dialing), Step::Wait);
        }
        assert!(!t.settled());
        assert_eq!(t.on_timeout(), Step::Failed("merge_timeout".into()));
        assert_eq!(t.on_timeout(), Step::Wait);
    }

    #[test]
    fn reviewer_repeated_and_late_results_over_the_channel_settle_once() {
        let id = "u-rev-chan-1";
        let mut rx = register(id);
        for status in ["dialing", "active", "merged", "merged", "failed", "active"] {
            // The phone pads the id with spaces: still this attempt.
            assert!(deliver(&json!({"type":"add_call_result","id":format!(" {id} "),"status":status,"reason":"merge_failed"})));
        }
        let mut t = Tracker::default();
        let mut settled = Vec::new();
        while let Ok(report) = rx.try_recv() {
            let step = t.on_report(report);
            if step != Step::Wait {
                settled.push(step);
            }
        }
        assert_eq!(settled, vec![Step::Merged], "exactly one settlement, the first final result");
        drop(rx);
        assert!(!deliver(&json!({"type":"add_call_result","id":id,"status":"merged"})), "a dropped waiter takes nothing");
        unregister(id);
    }

    #[test]
    fn reviewer_a_late_merge_after_the_ai_came_back_stays_quiet_for_the_ai() {
        // Handed after a resume still keeps the AI out and the hangup blocked.
        let c = CallCtl::default();
        c.pause();
        c.hand_over();
        c.resume_with("late".into());
        assert!(c.ai_quiet() && c.blocks_hangup());
        // A second resume overwrites, and the line is taken once.
        let c = CallCtl::default();
        c.pause();
        c.resume_with("one".into());
        c.resume_with("two".into());
        assert_eq!(c.take_say().as_deref(), Some("two"));
        assert_eq!(c.take_say(), None);
        assert!(!c.blocks_hangup());
    }

    #[test]
    fn reviewer_handing_over_never_ends_in_a_goodbye_hangup() {
        use crate::gemini_live::SessionEndReason as R;
        assert!(!R::Handed.should_play_goodbye());
        assert_eq!(R::Handed.as_str(), "handed_to_owner");
        // The reasons that do hang up are the ones the session-end code must hold back
        // while CallCtl::blocks_hangup is true.
        for r in [R::Goodbye, R::MaxDuration, R::CantCommunicate, R::WsClosed, R::IvrHangup] {
            assert!(r.should_play_goodbye(), "{}", r.as_str());
        }
    }

    #[test]
    fn reviewer_the_agent_waits_longer_than_the_phone_needs() {
        // Phone: owner answer timeout, 0.6 s before the merge, up to 10 s to merge, then
        // up to about 5 s to take the original call off hold before it reports failed.
        assert!(RESULT_WAIT >= OWNER_ANSWER_TIMEOUT + Duration::from_secs(20));
        // The phone coerces answer_timeout_ms into 5 s..60 s.
        assert!(OWNER_ANSWER_TIMEOUT >= Duration::from_secs(5) && OWNER_ANSWER_TIMEOUT <= Duration::from_secs(60));
        assert!(HOLD_LINE_MAX_WAIT < RESULT_WAIT);
    }

    #[test]
    fn reviewer_logs_never_carry_more_than_four_digits() {
        for n in ["+1 (555) 000-1111", "0912-345-678", "+886 912 345 678", "tel:+15550001111;ext=22", "", "abc"] {
            let m = masked(n);
            assert!(m.starts_with("***"), "{m}");
            assert!(m.chars().filter(|c| c.is_ascii_digit()).count() <= 4, "{m}");
        }
        assert_eq!(masked(""), "***");
    }

    #[test]
    fn reviewer_the_not_answered_line_works_for_every_language_and_a_blank_owner() {
        for lang in crate::locale::ALL {
            for assistant in [false, true] {
                for owner in ["Sam", "", "  ", "{x}\nSam"] {
                    let t = not_answered_turn(lang, assistant, owner);
                    let v: Value = serde_json::from_str(&t).unwrap();
                    let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
                    assert!(!text.is_empty());
                    assert!(!text.contains("{x}") && !text.contains('{') && !text.contains('}'), "{lang:?} {assistant} {owner:?}: {text}");
                    assert!(!text.contains("「」") && !text.contains("\"\""), "{lang:?} {assistant} {owner:?}: {text}");
                    assert!(text.contains("打給你") || text.contains("打给你") || text.contains("call you") || text.contains("call you right back"), "{text}");
                }
            }
        }
    }

    #[test]
    fn emergency_short_premium_and_mmi_numbers_are_never_dialed() {
        for n in ["110", "911", "112", "119", "*67#", "**21*+15550001111#", "#31#+15550001111", "123", "+19005550123", "011886912345678", "abc"] {
            assert!(check_owner_number(n, Some("us")).is_err(), "{n}");
            assert!(check_owner_number(n, None).is_err(), "{n}");
        }
        assert_eq!(check_owner_number("+1 555 000 1111", Some("us")).as_deref(), Ok("+15550001111"));
        assert_eq!(check_owner_number("+15550001111", None).as_deref(), Ok("+15550001111"));
        // Unknown SIM: a number without its country code is not guessed at.
        assert!(check_owner_number("5550001111", None).is_err());
    }

    #[test]
    fn the_hold_on_line_is_found_in_the_ai_output() {
        for said in ["好，你等一下，先不要掛。", "好，我现在帮你打给他，请先不要挂。", "Okay, hold on a moment, don't hang up.", "Please stay on the line.", "No cuelgues, por favor."] {
            assert!(is_hold_line(said), "{said}");
        }
        for said in ["好，我知道了，我馬上處理，等一下打給你。", "I'll call you right back.", "掛號了嗎？"] {
            assert!(!is_hold_line(said), "{said}");
        }
    }

    #[test]
    fn a_hold_on_line_without_urgent_words_falls_back_after_six_seconds() {
        let c = CallCtl::default();
        let t0 = Instant::now();
        assert!(!hold_line_unbacked(&c, t0));
        c.note_hold_line();
        let said = c.hold_said_at().unwrap();
        c.note_hold_line();
        assert_eq!(c.hold_said_at(), Some(said), "the first time counts");
        assert!(!hold_line_unbacked(&c, said + Duration::from_secs(5)));
        assert!(hold_line_unbacked(&c, said + HOLD_LINE_WITHOUT_TRIGGER));
        assert!(c.block_dial());
        assert!(!c.block_dial(), "blocked once");
        assert!(!hold_line_unbacked(&c, said + Duration::from_secs(30)), "no fallback once a call was tried or given up");
    }

    #[test]
    fn the_callback_line_promises_a_call_back_and_never_a_connection() {
        for lang in crate::locale::ALL {
            for assistant in [false, true] {
                let t = callback_turn(lang, assistant, "Sam");
                let v: Value = serde_json::from_str(&t).unwrap();
                let text = v.pointer("/clientContent/turns/0/parts/0/text").and_then(|x| x.as_str()).unwrap();
                assert!(text.contains("打給你") || text.contains("打给你") || text.contains("call you back"), "{text}");
            }
        }
    }

    #[test]
    fn the_abort_names_the_attempt_and_alerts_exist_in_every_language() {
        let v = abort_payload("u-9");
        assert_eq!(v["type"], "add_call_abort");
        assert_eq!(v["id"], "u-9");
        for lang in crate::locale::ALL {
            assert!(!alert_line(Alert::AiEnded, lang).is_empty());
            assert!(!alert_line(Alert::StillOnHold, lang).is_empty());
        }
    }

    #[test]
    fn the_phone_gets_a_dialing_ack_window_shorter_than_the_hold_line_wait() {
        assert!(DIAL_ACK_WAIT < OWNER_ANSWER_TIMEOUT);
        assert!(ABORT_WAIT < RESULT_WAIT);
    }

    #[test]
    fn a_session_that_ends_while_dialing_or_merged_never_reaches_the_hangup_path() {
        assert_eq!(on_session_end(None), SessionEnd::Usual);
        let c = CallCtl::default();
        assert_eq!(on_session_end(Some(&c)), SessionEnd::Usual);
        c.pause();
        assert_eq!(on_session_end(Some(&c)), SessionEnd::LeaveUpAndAlert, "dialing: no voicemail, retry or hangup");
        c.resume_with("x".into());
        assert_eq!(on_session_end(Some(&c)), SessionEnd::Usual, "the AI is back: the usual end");
        c.pause();
        c.hand_over();
        assert_eq!(on_session_end(Some(&c)), SessionEnd::LeaveUp, "merged: leave it up");
    }

    #[test]
    fn a_phone_with_the_owner_on_the_call_takes_no_new_ai_session_until_idle() {
        let phone = "test-phone-owner-on";
        assert!(!owner_on_call(phone));
        mark_owner_on_call(phone);
        assert!(owner_on_call(phone), "new hellos / rings on this phone are ignored");
        assert!(!owner_on_call("test-phone-other"), "other phones are not affected");
        assert!(!owner_on_call_at(phone, Instant::now() + OWNER_ON_CALL_MAX), "a lost idle status does not block forever");
        assert!(!status_ends_owner_call(&json!({"type":"status","in_call":true})));
        assert!(!status_ends_owner_call(&json!({"type":"status"})));
        assert!(status_ends_owner_call(&json!({"type":"status","in_call":false})));
        assert!(clear_owner_on_call(phone));
        assert!(!owner_on_call(phone));
        assert!(!clear_owner_on_call(phone));
    }
}

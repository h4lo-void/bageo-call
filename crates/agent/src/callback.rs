//! Outbound callback routing: idle control WS → `place_call` → session tracking.
//!
//! Phone idle control client (`AgentControlClient`) sends hello with `mode=CONTROL_IDLE`.
//! Fulfillment (`ReplyMethod::Phone`) pushes `callback_requested`, then dispatches
//! `place_call` only when: user confirmed + number on `callback_whitelist` + idle
//! control client connected (never auto-dials; empty whitelist = fail-closed).
//!
//! The same connection carries text messages to callers (appointments):
//! `send_sms` out, `send_sms_result` back, only to apps whose hello says
//! `supports_sms` ([`ControlRegistry::send_sms`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use futures_util::SinkExt;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex};
use tracing::{info, warn};
use uuid::Uuid;

use crate::tasks::TaskStore;

pub type WsSink = Arc<Mutex<SplitSink<WebSocket, Message>>>;

/// Outbound callback session phases (agent-side state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackPhase {
    /// Task queued; `place_call` not yet sent.
    Queued,
    /// `place_call` sent; waiting for phone dial / OFFHOOK.
    Dialing,
    /// Remote party ringing (optional telemetry from phone).
    Ringing,
    /// OFFHOOK + uplink path ready (`outbound_ready` from phone).
    Connected,
    /// Dial or bridge failed.
    Failed,
    /// Call ended normally.
    Done,
}

impl CallbackPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Dialing => "dialing",
            Self::Ringing => "ringing",
            Self::Connected => "connected",
            Self::Failed => "failed",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CallbackSession {
    pub task_id: String,
    pub number: String,
    pub call_id: String,
    pub serial: String,
    pub phase: CallbackPhase,
}

impl CallbackSession {
    pub fn new(task_id: impl Into<String>, number: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            number: number.into(),
            call_id: String::new(),
            serial: String::new(),
            phase: CallbackPhase::Queued,
        }
    }

    pub fn advance(&mut self, phase: CallbackPhase) {
        self.phase = phase;
    }
}

/// Build the WS JSON command payload for the phone to place an outbound callback.
///
/// Returns `{"type":"place_call","number":"…","call_id":"…","task_id":"…"}`.
pub fn start_callback_request(session: &mut CallbackSession, call_id: Option<&str>) -> Value {
    let call_id = call_id
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    session.call_id = call_id.clone();
    session.advance(CallbackPhase::Dialing);
    json!({
        "type": "place_call",
        "number": session.number,
        "call_id": call_id,
        "task_id": session.task_id,
    })
}

/// Phone acknowledged dial intent (`place_call_ack` or `place_call_failed`).
pub fn on_place_call_result(session: &mut CallbackSession, ok: bool) {
    session.advance(if ok {
        CallbackPhase::Dialing
    } else {
        CallbackPhase::Failed
    });
}

/// Phone reached OFFHOOK and reused the uplink inject path (`outbound_ready`).
pub fn on_outbound_ready(session: &mut CallbackSession) {
    session.advance(CallbackPhase::Connected);
}

/// Cellular session ended (`call_end` or WS drop while Connected).
pub fn on_call_end(session: &mut CallbackSession, failed: bool) {
    session.advance(if failed {
        CallbackPhase::Failed
    } else {
        CallbackPhase::Done
    });
}

/// What became of a text message the agent asked a phone to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmsOutcome {
    Sent,
    /// No control connection from that phone (or it dropped before answering).
    Offline,
    /// The app's hello did not say `supports_sms` (an older app).
    Unsupported,
    /// No `send_sms_result` in time.
    Timeout,
    /// The phone's error code (one of [`SMS_ERROR_CODES`]).
    Failed(String),
}

/// Error codes a phone may answer `send_sms` with; anything else reads as `generic_failure`.
pub const SMS_ERROR_CODES: [&str; 7] = [
    "no_permission",
    "bad_request",
    "no_service",
    "radio_off",
    "generic_failure",
    "timeout",
    "exception",
];
/// How long the agent waits for `send_sms_result` (the phone gives up after 60 s).
pub const SMS_RESULT_WAIT: Duration = Duration::from_secs(90);
const SMS_TEXT_MAX: usize = 500;
const SMS_ID_MAX: usize = 64;

/// `{"type":"send_sms","id":…,"to":…,"text":…}` for the phone.
pub fn send_sms_payload(id: &str, to: &str, text: &str) -> Value {
    json!({ "type": "send_sms", "id": id, "to": to, "text": text })
}

/// Digits only, with an optional leading `+`.
fn sms_to_ok(to: &str) -> bool {
    let digits = to.strip_prefix('+').unwrap_or(to);
    (3..=20).contains(&digits.len()) && digits.chars().all(|c| c.is_ascii_digit())
}

/// A phone's `send_sms_result`: the message id and what happened.
pub fn parse_sms_result(msg: &Value) -> Option<(String, SmsOutcome)> {
    if msg.get("type")?.as_str()? != "send_sms_result" {
        return None;
    }
    let id = msg.get("id")?.as_str()?.to_string();
    if msg.get("ok").and_then(Value::as_bool) == Some(true) {
        return Some((id, SmsOutcome::Sent));
    }
    let code = msg.get("error").and_then(Value::as_str).map(str::trim).unwrap_or("");
    let code = if SMS_ERROR_CODES.contains(&code) {
        code
    } else {
        "generic_failure"
    };
    Some((id, SmsOutcome::Failed(code.to_string())))
}

/// The control serials a call's phone may be registered under: its own, and the
/// main app's for a work-profile twin copy (`pixel-101NNNN` → `pixel-1NNNN`).
fn control_serials(serial: &str) -> Vec<String> {
    let s = serial.trim();
    let mut out = vec![s.to_string()];
    if crate::calls::is_twin_serial(s) {
        if let Some(app) = s.strip_prefix("pixel-").and_then(|r| r.get(2..)) {
            out.push(format!("pixel-{app}"));
        }
    }
    out
}

type SmsWaiters = HashMap<String, (String, oneshot::Sender<SmsOutcome>)>;

/// Connected idle-control WebSocket clients and in-flight callback sessions.
#[derive(Clone, Default)]
pub struct ControlRegistry {
    clients: Arc<Mutex<HashMap<String, WsSink>>>,
    sessions: Arc<Mutex<HashMap<String, CallbackSession>>>,
    call_index: Arc<Mutex<HashMap<String, String>>>,
    /// Phones switched to "normal phone" (their owner answers): call-backs skip them.
    normal_mode: Arc<Mutex<HashSet<String>>>,
    /// Phones whose app sends text messages (`supports_sms` in the control hello).
    sms_capable: Arc<Mutex<HashSet<String>>>,
    /// `send_sms` sent, waiting for its result: message id → (serial, answer).
    sms_waiters: Arc<Mutex<SmsWaiters>>,
}

impl ControlRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record whether `serial` is a normal phone (from its control hello and status).
    pub async fn set_normal_mode(&self, serial: &str, normal: bool) {
        let mut set = self.normal_mode.lock().await;
        if normal {
            set.insert(serial.to_string());
        } else {
            set.remove(serial);
        }
    }

    pub async fn register_client(&self, serial: String, sink: WsSink) {
        let mut clients = self.clients.lock().await;
        if let Some(prev) = clients.insert(serial.clone(), sink) {
            // Drop stale sink for same serial (reconnect).
            let _ = prev;
        }
        info!(
            serial,
            count = clients.len(),
            "control_idle client registered"
        );
    }

    pub async fn unregister_client(&self, serial: &str) {
        let mut clients = self.clients.lock().await;
        clients.remove(serial);
        info!(
            serial,
            count = clients.len(),
            "control_idle client unregistered"
        );
        drop(clients);
        self.forget_sms_phone(serial).await;
    }

    /// Remove `serial` only if it still maps to `sink` (reconnect-safe).
    pub async fn unregister_client_sink(&self, serial: &str, sink: &WsSink) {
        let mut clients = self.clients.lock().await;
        let ours = clients
            .get(serial)
            .map(|cur| Arc::ptr_eq(cur, sink))
            .unwrap_or(false);
        if ours {
            clients.remove(serial);
            info!(serial, count = clients.len(), "control_idle client unregistered");
            drop(clients);
            self.forget_sms_phone(serial).await;
        } else {
            info!(
                serial,
                count = clients.len(),
                "control_idle stale socket closed (newer client kept)"
            );
        }
    }

    /// Record whether the phone's app sends text messages (from its control hello).
    pub async fn set_supports_sms(&self, serial: &str, on: bool) {
        let mut set = self.sms_capable.lock().await;
        if on {
            set.insert(serial.to_string());
        } else {
            set.remove(serial);
        }
    }

    /// A phone went away: it sends no more texts, and messages still waiting
    /// for its answer end as [`SmsOutcome::Offline`].
    async fn forget_sms_phone(&self, serial: &str) {
        self.sms_capable.lock().await.remove(serial);
        // Dropping the senders wakes the waiters.
        self.sms_waiters.lock().await.retain(|_, (s, _)| s != serial);
    }

    /// Have the phone that took a call (`serial`) text `to` and wait up to
    /// `wait` for its `send_sms_result`. `id`: up to 64 characters (a task id);
    /// `text` is cut at 500 characters.
    pub async fn send_sms(&self, serial: &str, id: &str, to: &str, text: &str, wait: Duration) -> SmsOutcome {
        let picked = {
            let clients = self.clients.lock().await;
            control_serials(serial)
                .into_iter()
                .find_map(|s| clients.get(&s).map(|sink| (s, sink.clone())))
        };
        let Some((serial, sink)) = picked else {
            info!(serial, "send_sms: phone not connected");
            return SmsOutcome::Offline;
        };
        if !self.sms_capable.lock().await.contains(&serial) {
            info!(serial, "send_sms: the phone's app does not send texts");
            return SmsOutcome::Unsupported;
        }
        let id: String = id.chars().take(SMS_ID_MAX).collect();
        let text: String = text.chars().take(SMS_TEXT_MAX).collect();
        if id.is_empty() || text.trim().is_empty() || !sms_to_ok(to) {
            return SmsOutcome::Failed("bad_request".to_string());
        }
        let (tx, rx) = oneshot::channel();
        self.sms_waiters
            .lock()
            .await
            .insert(id.clone(), (serial.clone(), tx));
        if let Err(e) = send_ws_json(&sink, send_sms_payload(&id, to, &text)).await {
            self.sms_waiters.lock().await.remove(&id);
            warn!(serial, "send_sms write failed: {e:#}");
            return SmsOutcome::Offline;
        }
        info!(
            serial,
            id,
            to_len = to.len(),
            text_chars = text.chars().count(),
            "send_sms dispatched"
        );
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(out)) => out,
            Ok(Err(_)) => SmsOutcome::Offline,
            Err(_) => {
                self.sms_waiters.lock().await.remove(&id);
                warn!(serial, id, "send_sms: no result in {}s", wait.as_secs());
                SmsOutcome::Timeout
            }
        }
    }

    /// A phone's `send_sms_result`; false when nothing was waiting for it.
    pub async fn handle_sms_result(&self, msg: &Value) -> bool {
        let Some((id, out)) = parse_sms_result(msg) else {
            return false;
        };
        let Some((serial, tx)) = self.sms_waiters.lock().await.remove(&id) else {
            info!(id, "send_sms_result for a message nobody waits for");
            return false;
        };
        info!(serial, id, outcome = ?out, "send_sms_result");
        let _ = tx.send(out);
        true
    }

    /// Idle phones the owner actually has (work-profile twin copies excluded), sorted.
    pub async fn connected_serials(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .clients
            .lock()
            .await
            .keys()
            .filter(|s| !crate::calls::is_twin_serial(s))
            .cloned()
            .collect();
        v.sort();
        v
    }

    /// Push current blocklist to one idle control client (after hello or reconnect).
    pub async fn send_blocklist_sync(&self, serial: &str, numbers: &[String]) -> Result<()> {
        let clients = self.clients.lock().await;
        let Some(sink) = clients.get(serial) else {
            return Ok(());
        };
        let sink = sink.clone();
        drop(clients);
        send_blocklist_payload(&sink, numbers).await
    }

    /// Push blocklist to all connected idle control clients. Returns send count.
    pub async fn broadcast_blocklist_sync(&self, numbers: &[String]) -> usize {
        let clients = self.clients.lock().await;
        if clients.is_empty() {
            return 0;
        }
        let sinks: Vec<(String, WsSink)> = clients
            .iter()
            .map(|(s, sink)| (s.clone(), sink.clone()))
            .collect();
        drop(clients);

        let payload = blocklist_sync_payload(numbers);
        let text = match serde_json::to_string(&payload) {
            Ok(t) => t,
            Err(e) => {
                warn!("blocklist_sync serialize failed: {e}");
                return 0;
            }
        };
        let mut sent = 0usize;
        for (serial, sink) in sinks {
            if sink
                .lock()
                .await
                .send(Message::Text(text.clone().into()))
                .await
                .is_ok()
            {
                sent += 1;
            } else {
                warn!(serial, "blocklist_sync send failed");
            }
        }
        if sent > 0 {
            info!(
                sent,
                numbers_len = numbers.len(),
                "blocklist_sync broadcast"
            );
        }
        sent
    }

    pub async fn session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    /// Dispatch `place_call` to a connected control client. Returns `true` when sent.
    ///
    /// Gates (fail-closed):
    /// - `number` non-empty
    /// - `callback_whitelist_allows(number, whitelist)` — empty list denies
    /// - at least one `CONTROL_IDLE` client (`Ok(false)` when none)
    ///
    /// Prefers `prefer_serial` when that phone is connected; otherwise first available.
    pub async fn try_dispatch_phone_callback(
        &self,
        task_id: &str,
        number: &str,
        prefer_serial: &str,
        whitelist: &[String],
    ) -> Result<bool> {
        let number = number.trim();
        if number.is_empty() {
            anyhow::bail!("callback number empty");
        }
        if !callback_whitelist_allows(number, whitelist) {
            anyhow::bail!("callback number not on whitelist");
        }

        let clients = self.clients.lock().await;
        if clients.is_empty() {
            return Ok(false);
        }
        let normal = self.normal_mode.lock().await.clone();
        let picked = pick_callback_phone(clients.keys(), prefer_serial, &normal)
            .and_then(|serial| clients.get(&serial).map(|sink| (serial, sink.clone())));
        let Some((serial, sink)) = picked else {
            info!(task_id, "callback: every connected phone is a normal phone");
            return Ok(false);
        };
        drop(clients);

        let mut session = CallbackSession::new(task_id, number);
        session.serial = serial.clone();
        let payload = start_callback_request(&mut session, None);

        send_ws_json(&sink, payload)
            .await
            .with_context(|| format!("send place_call serial={serial} task_id={task_id}"))?;

        let call_id = session.call_id.clone();
        self.sessions
            .lock()
            .await
            .insert(task_id.to_string(), session);
        self.call_index
            .lock()
            .await
            .insert(call_id, task_id.to_string());

        info!(
            task_id,
            serial,
            number_len = number.len(),
            "callback place_call dispatched"
        );
        Ok(true)
    }

    /// Advance session from phone → agent control WS telemetry.
    pub async fn handle_phone_event(&self, msg: &Value, store: &TaskStore) {
        let mtype = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let call_id = msg.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
        let task_id = msg
            .get("task_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let task_id = if let Some(tid) = task_id {
            tid
        } else if !call_id.is_empty() {
            match self.call_index.lock().await.get(call_id) {
                Some(tid) => tid.clone(),
                None => {
                    info!(mtype, call_id, "callback event: unknown call_id");
                    return;
                }
            }
        } else {
            info!(mtype, "callback event: missing task_id/call_id");
            return;
        };

        let mut sessions = self.sessions.lock().await;
        let Some(session) = sessions.get_mut(&task_id) else {
            info!(mtype, task_id, "callback event: no session");
            return;
        };

        match mtype {
            "place_call_ack" => {
                on_place_call_result(session, true);
                drop(sessions);
                let _ = store.push_event(&task_id, "callback_place_call_ack").await;
                let _ = store
                    .patch(
                        &task_id,
                        crate::tasks::TaskPatch {
                            fulfill_error: Some(None),
                            ..Default::default()
                        },
                    )
                    .await;
                info!(task_id, "callback place_call_ack");
            }
            "place_call_failed" => {
                on_place_call_result(session, false);
                let reason = msg
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("dial failed");
                drop(sessions);
                let _ = store
                    .push_event(&task_id, "callback_place_call_failed")
                    .await;
                let _ = store
                    .patch(
                        &task_id,
                        crate::tasks::TaskPatch {
                            fulfill_error: Some(Some(format!("callback: {reason}"))),
                            ..Default::default()
                        },
                    )
                    .await;
                warn!(task_id, reason, "callback place_call_failed");
            }
            "outbound_ready" => {
                on_outbound_ready(session);
                drop(sessions);
                let _ = store.push_event(&task_id, "callback_outbound_ready").await;
                info!(task_id, "callback outbound_ready");
            }
            "call_end" => {
                let failed = msg
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .is_some_and(|r| !r.is_empty() && r != "normal");
                on_call_end(session, failed);
                drop(sessions);
                let evt = if failed {
                    "callback_failed"
                } else {
                    "callback_done"
                };
                let _ = store.push_event(&task_id, evt).await;
                info!(task_id, failed, "callback call_end");
            }
            _ => {}
        }
    }
}

/// The phone for a call-back: `prefer` when it is connected and not a normal phone,
/// else the first such phone by serial (a stable choice). None: no phone can call.
fn pick_callback_phone<'a>(
    connected: impl Iterator<Item = &'a String>,
    prefer: &str,
    normal: &HashSet<String>,
) -> Option<String> {
    let mut usable: Vec<&String> = connected.filter(|s| !normal.contains(s.as_str())).collect();
    if usable.iter().any(|s| s.as_str() == prefer) {
        return Some(prefer.to_string());
    }
    usable.sort();
    usable.first().map(|s| (*s).clone())
}

pub async fn send_ws_json(sink: &WsSink, payload: Value) -> Result<()> {
    let text = serde_json::to_string(&payload)?;
    sink.lock()
        .await
        .send(Message::Text(text.into()))
        .await
        .context("ws send")?;
    Ok(())
}

fn blocklist_sync_payload(numbers: &[String]) -> Value {
    json!({
        "type": "blocklist_sync",
        "numbers": numbers,
    })
}

async fn send_blocklist_payload(sink: &WsSink, numbers: &[String]) -> Result<()> {
    send_ws_json(sink, blocklist_sync_payload(numbers)).await
}

/// Resolve E.164 / caller-id number for outbound callback.
pub fn callback_number(reply_contact: Option<&str>, caller_id: &str) -> String {
    reply_contact
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let c = caller_id.trim();
            if c.is_empty() {
                None
            } else {
                Some(c)
            }
        })
        .unwrap_or("")
        .to_string()
}

/// Digits-only normalize for whitelist compare.
pub fn normalize_callback_digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn strip_tw_mobile_core(digits: &str) -> String {
    let d = digits.trim_start_matches('0');
    if let Some(rest) = d.strip_prefix("886") {
        rest.trim_start_matches('0').to_string()
    } else {
        d.to_string()
    }
}

fn numbers_equivalent(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let ca = strip_tw_mobile_core(a);
    let cb = strip_tw_mobile_core(b);
    !ca.is_empty() && !cb.is_empty() && (ca == cb || ca.ends_with(&cb) || cb.ends_with(&ca))
}

/// Outbound dial allow-list. **Fail-closed:** empty `whitelist` denies every number.
///
/// Entries ending in `*` are digit-prefix matches (after normalize). Other entries
/// use digit-exact / TW-mobile-equivalent match (`09…` ↔ `8869…`).
pub fn callback_whitelist_allows(number: &str, whitelist: &[String]) -> bool {
    if whitelist.is_empty() {
        return false;
    }
    let num = normalize_callback_digits(number);
    if num.is_empty() {
        return false;
    }
    for entry in whitelist {
        let e = entry.trim();
        if e.is_empty() {
            continue;
        }
        let prefix = e.ends_with('*');
        let pat_raw = if prefix {
            e.trim_end_matches('*').trim()
        } else {
            e
        };
        let pat = normalize_callback_digits(pat_raw);
        if pat.is_empty() {
            continue;
        }
        if prefix {
            if num.starts_with(&pat)
                || strip_tw_mobile_core(&num).starts_with(&strip_tw_mobile_core(&pat))
            {
                return true;
            }
        } else if numbers_equivalent(&num, &pat) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_skips_normal_phones() {
        let phones = vec!["pixel-1".to_string(), "pixel-2".to_string(), "pixel-3".to_string()];
        let normal: HashSet<String> = ["pixel-2".to_string()].into_iter().collect();
        assert_eq!(pick_callback_phone(phones.iter(), "pixel-3", &normal).as_deref(), Some("pixel-3"));
        // The preferred phone is a normal phone: another one calls.
        assert_eq!(pick_callback_phone(phones.iter(), "pixel-2", &normal).as_deref(), Some("pixel-1"));
        let all: HashSet<String> = phones.iter().cloned().collect();
        assert_eq!(pick_callback_phone(phones.iter(), "pixel-1", &all), None);
        assert_eq!(pick_callback_phone(std::iter::empty::<&String>(), "", &normal), None);
    }

    #[test]
    fn start_callback_request_payload() {
        let mut s = CallbackSession::new("task-1", "+15551234567");
        let v = start_callback_request(&mut s, Some("call-abc"));
        assert_eq!(v["type"], "place_call");
        assert_eq!(v["number"], "+15551234567");
        assert_eq!(v["call_id"], "call-abc");
        assert_eq!(v["task_id"], "task-1");
        assert_eq!(s.phase, CallbackPhase::Dialing);
    }

    #[test]
    fn callback_number_prefers_reply_contact() {
        assert_eq!(callback_number(Some("+886911"), "+886900"), "+886911");
        assert_eq!(callback_number(None, "+886900"), "+886900");
        assert_eq!(callback_number(Some(""), ""), "");
    }

    #[test]
    fn whitelist_fail_closed_empty() {
        assert!(!callback_whitelist_allows("+886911222333", &[]));
        assert!(!callback_whitelist_allows("0911222333", &[String::new()]));
    }

    #[test]
    fn whitelist_exact_and_tw_equiv() {
        let list = vec!["+886 911-222-333".to_string()];
        assert!(callback_whitelist_allows("0911222333", &list));
        assert!(callback_whitelist_allows("+886911222333", &list));
        assert!(!callback_whitelist_allows("0911222334", &list));
    }

    #[test]
    fn whitelist_prefix_star() {
        let list = vec!["+8869*".to_string()];
        assert!(callback_whitelist_allows("0911222333", &list));
        assert!(!callback_whitelist_allows("0212345678", &list));
    }

    #[test]
    fn send_sms_message_and_its_result() {
        assert_eq!(
            send_sms_payload("task-1", "+886912345678", "你好"),
            json!({"type": "send_sms", "id": "task-1", "to": "+886912345678", "text": "你好"})
        );
        let ok = json!({"type": "send_sms_result", "id": "task-1", "ok": true});
        assert_eq!(parse_sms_result(&ok), Some(("task-1".to_string(), SmsOutcome::Sent)));
        let failed = json!({"type": "send_sms_result", "id": "task-1", "ok": false, "error": "no_service"});
        assert_eq!(
            parse_sms_result(&failed),
            Some(("task-1".to_string(), SmsOutcome::Failed("no_service".into())))
        );
        let odd = json!({"type": "send_sms_result", "id": "x", "ok": false, "error": "<script>"});
        assert_eq!(parse_sms_result(&odd).unwrap().1, SmsOutcome::Failed("generic_failure".into()));
        assert_eq!(parse_sms_result(&json!({"type": "place_call_ack", "id": "x"})), None);
        assert_eq!(parse_sms_result(&json!({"type": "send_sms_result", "ok": true})), None);
        assert!(sms_to_ok("+886912345678") && sms_to_ok("0912345678"));
        assert!(!sms_to_ok("09-12") && !sms_to_ok("++886") && !sms_to_ok("") && !sms_to_ok("abc"));
        assert_eq!(control_serials("pixel-1010341"), vec!["pixel-1010341", "pixel-10341"]);
        assert_eq!(control_serials("pixel-10341"), vec!["pixel-10341"]);
    }

    #[tokio::test]
    async fn send_sms_needs_a_connected_phone_and_results_reach_their_waiter() {
        let reg = ControlRegistry::default();
        let out = reg
            .send_sms("pixel-1", "t1", "+886912345678", "hi", Duration::from_millis(50))
            .await;
        assert_eq!(out, SmsOutcome::Offline);
        // A result arrives for a message that is waiting.
        let (tx, rx) = oneshot::channel();
        reg.sms_waiters.lock().await.insert("t2".into(), ("pixel-1".into(), tx));
        let msg = json!({"type": "send_sms_result", "id": "t2", "ok": false, "error": "radio_off"});
        assert!(reg.handle_sms_result(&msg).await);
        assert_eq!(rx.await.unwrap(), SmsOutcome::Failed("radio_off".into()));
        assert!(!reg.handle_sms_result(&msg).await);
        // The phone drops: its waiting messages end, its texting flag goes.
        let (tx, rx) = oneshot::channel();
        reg.sms_waiters.lock().await.insert("t3".into(), ("pixel-1".into(), tx));
        reg.set_supports_sms("pixel-1", true).await;
        reg.unregister_client("pixel-1").await;
        assert!(rx.await.is_err());
        assert!(!reg.sms_capable.lock().await.contains("pixel-1"));
    }
}

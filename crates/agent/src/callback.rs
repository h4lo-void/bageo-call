//! Outbound callback routing: idle control WS → `place_call` → session tracking.
//!
//! Phone idle control client (`AgentControlClient`) sends hello with `mode=CONTROL_IDLE`.
//! Fulfillment (`ReplyMethod::Phone`) pushes `callback_requested`, then dispatches
//! `place_call` only when: user confirmed + number on `callback_whitelist` + idle
//! control client connected (never auto-dials; empty whitelist = fail-closed).
//!
//! The same connection carries text messages to callers (appointments):
//! `send_sms` out, `send_sms_result` and `send_sms_delivered` back, only to
//! apps whose hello says `supports_sms` ([`ControlRegistry::send_sms_on`]); a
//! SIM picked by name only to apps that also say `supports_sim_select`.

use std::collections::{HashMap, HashSet, VecDeque};
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
    /// The SIM (Android subscription id) of the call the task came from, when the
    /// call-back goes out on that same phone.
    pub sub_id: Option<i64>,
}

impl CallbackSession {
    pub fn new(task_id: impl Into<String>, number: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            number: number.into(),
            call_id: String::new(),
            serial: String::new(),
            phase: CallbackPhase::Queued,
            sub_id: None,
        }
    }

    pub fn advance(&mut self, phase: CallbackPhase) {
        self.phase = phase;
    }
}

/// Build the WS JSON command payload for the phone to place an outbound callback.
///
/// Returns `{"type":"place_call","number":"…","call_id":"…","task_id":"…"}`, plus
/// `"sub_id"` when the SIM the call came in on is known (older apps ignore it and
/// dial on their default SIM).
pub fn start_callback_request(session: &mut CallbackSession, call_id: Option<&str>) -> Value {
    let call_id = call_id
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    session.call_id = call_id.clone();
    session.advance(CallbackPhase::Dialing);
    let mut payload = json!({
        "type": "place_call",
        "number": session.number,
        "call_id": call_id,
        "task_id": session.task_id,
    });
    if let Some(sub) = session.sub_id {
        payload["sub_id"] = json!(sub);
    }
    payload
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
    /// A message with this id is already waiting for its result (not sent twice).
    Busy,
    /// The phone's error code (one of [`SMS_ERROR_CODES`]).
    Failed(String),
}

/// Error codes a phone may answer `send_sms` with; anything else reads as `generic_failure`.
/// `no_sim`: no SIM, or more than one, matched the `sim` of the message.
pub const SMS_ERROR_CODES: [&str; 8] = [
    "no_permission",
    "bad_request",
    "no_service",
    "radio_off",
    "generic_failure",
    "timeout",
    "exception",
    "no_sim",
];
/// How long the agent waits for `send_sms_result` (the phone gives up after 60 s).
pub const SMS_RESULT_WAIT: Duration = Duration::from_secs(90);
const SMS_TEXT_MAX: usize = 500;
const SMS_ID_MAX: usize = 64;
/// Texts whose result is remembered ([`ControlRegistry::sms_outcome`]), and
/// tasks whose call's SIM is remembered for a call-back: the newest this many.
const RECENT_MAX: usize = 256;

/// The SIM a text goes out from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmsSim {
    /// The phone's default SMS SIM.
    Default,
    /// Android's subscription id (the SIM that took the call).
    SubId(i64),
    /// A SIM the phone finds by its carrier name or its own number (setting
    /// `appointment_test_sms_sim`). Only for apps whose hello says
    /// `supports_sim_select`: an older app would send from its default SIM.
    Named(String),
}

impl SmsSim {
    /// `sub_id` when known, else the default SIM.
    pub fn from_sub_id(sub_id: Option<i64>) -> Self {
        sub_id.map_or(Self::Default, Self::SubId)
    }

    /// For logs: never the name itself (it may be the SIM's own number).
    pub fn describe(&self) -> String {
        match self {
            Self::Default => "default".to_string(),
            Self::SubId(id) => format!("sub_id={id}"),
            Self::Named(name) => format!("named(len={})", name.chars().count()),
        }
    }
}

/// `{"type":"send_sms","id":…,"to":…,"text":…}` for the phone, with `"sub_id"`
/// (the SIM to send from) or `"sim"` (a SIM to look up by carrier name or
/// number, never together with `sub_id`); without either the app uses its default SIM.
pub fn send_sms_payload(id: &str, to: &str, text: &str, sim: &SmsSim) -> Value {
    let mut payload = json!({ "type": "send_sms", "id": id, "to": to, "text": text });
    match sim {
        SmsSim::Default => {}
        SmsSim::SubId(sub) => payload["sub_id"] = json!(sub),
        SmsSim::Named(name) => {
            if !name.trim().is_empty() {
                payload["sim"] = json!(name.trim());
            }
        }
    }
    payload
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

/// A phone's `send_sms_delivered` (the carrier's delivery report): message id and
/// whether it reached the recipient.
pub fn parse_sms_delivered(msg: &Value) -> Option<(String, bool)> {
    if msg.get("type")?.as_str()? != "send_sms_delivered" {
        return None;
    }
    let id = msg.get("id")?.as_str()?.to_string();
    Some((id, msg.get("ok").and_then(Value::as_bool) == Some(true)))
}

/// A map that keeps only its newest [`RECENT_MAX`] keys.
#[derive(Debug)]
struct Recent<V> {
    entries: HashMap<String, V>,
    order: VecDeque<String>,
}

impl<V> Default for Recent<V> {
    fn default() -> Self {
        Self { entries: HashMap::new(), order: VecDeque::new() }
    }
}

impl<V> Recent<V> {
    fn get(&self, key: &str) -> Option<&V> {
        self.entries.get(key)
    }

    fn insert(&mut self, key: &str, value: V) {
        if self.entries.insert(key.to_string(), value).is_none() {
            self.order.push_back(key.to_string());
        }
        while self.order.len() > RECENT_MAX {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
    }

    fn remove(&mut self, key: &str) {
        if self.entries.remove(key).is_some() {
            self.order.retain(|k| k != key);
        }
    }

    fn get_or_insert(&mut self, key: &str) -> &mut V
    where
        V: Default,
    {
        if !self.entries.contains_key(key) {
            self.insert(key, V::default());
        }
        self.entries.get_mut(key).expect("just inserted")
    }
}

/// What one text came to, as the phone reported it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SmsReport {
    /// `send_sms_result`; None = no result yet.
    result: Option<SmsOutcome>,
    /// `send_sms_delivered`: false once any report says not delivered.
    delivered: Option<bool>,
}

/// The phone gave up waiting for the radio after 60 s (`timeout`): not final —
/// the real result can still come, under the same id.
fn is_phone_timeout(out: &SmsOutcome) -> bool {
    matches!(out, SmsOutcome::Failed(code) if code == "timeout")
}

/// What a `send_sms_result` adds to what is known about its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultNews {
    /// The first result, or the real one after the phone's own `timeout`.
    New,
    /// The same result again (the phone resends its recent ones after reconnecting).
    Repeat,
    /// A different result for a text already settled: it stays as it was.
    Settled,
}

fn weigh_result(known: Option<&SmsOutcome>, new: &SmsOutcome) -> ResultNews {
    match known {
        None => ResultNews::New,
        Some(k) if k == new => ResultNews::Repeat,
        Some(k) if is_phone_timeout(k) => ResultNews::New,
        Some(_) => ResultNews::Settled,
    }
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
    /// Phones whose app can refuse a ringing call (`supports_reject` in the
    /// control hello); older apps pick up whatever `answer_plan` says.
    reject_capable: Arc<Mutex<HashSet<String>>>,
    /// Phones whose app picks a SIM by name for a text (`supports_sim_select`
    /// in the control hello).
    sim_select_capable: Arc<Mutex<HashSet<String>>>,
    /// `send_sms` sent, waiting for its result: message id → (serial, answer).
    sms_waiters: Arc<Mutex<SmsWaiters>>,
    /// What each recent text came to, also when its answer came after the wait
    /// ended (the phone resends its last result after reconnecting).
    sms_reports: Arc<Mutex<Recent<SmsReport>>>,
    /// The phone and SIM (subscription id) of the call each recent task came
    /// from, so its call-back dials out on the SIM the caller called.
    task_sims: Arc<Mutex<Recent<(String, i64)>>>,
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
        self.forget_phone(serial).await;
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
            self.forget_phone(serial).await;
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

    /// Record whether the phone's app can refuse a ringing call (from its control hello).
    pub async fn set_supports_reject(&self, serial: &str, on: bool) {
        let mut set = self.reject_capable.lock().await;
        if on {
            set.insert(serial.to_string());
        } else {
            set.remove(serial);
        }
    }

    /// The phone's app said it can refuse a ringing call (`answer_plan` `reject`).
    pub async fn supports_reject(&self, serial: &str) -> bool {
        let set = self.reject_capable.lock().await;
        control_serials(serial).iter().any(|s| set.contains(s))
    }

    /// Record whether the phone's app sends a text from a SIM named by carrier
    /// or number (`"sim"` in `send_sms`; from its control hello).
    pub async fn set_supports_sim_select(&self, serial: &str, on: bool) {
        let mut set = self.sim_select_capable.lock().await;
        if on {
            set.insert(serial.to_string());
        } else {
            set.remove(serial);
        }
    }

    /// A phone went away: it sends no more texts (messages still waiting for its
    /// answer end as [`SmsOutcome::Offline`]) and its abilities are forgotten
    /// until its next hello.
    async fn forget_phone(&self, serial: &str) {
        self.sms_capable.lock().await.remove(serial);
        self.reject_capable.lock().await.remove(serial);
        self.sim_select_capable.lock().await.remove(serial);
        // Dropping the senders wakes the waiters.
        self.sms_waiters.lock().await.retain(|_, (s, _)| s != serial);
    }

    /// Have the phone that took a call (`serial`) text `to` and wait up to
    /// `wait` for its `send_sms_result`. `id`: up to 64 characters (a task id);
    /// `text` is cut at 500 characters. A message whose id is already waiting
    /// is not sent again ([`SmsOutcome::Busy`]); the waiting one is left alone.
    pub async fn send_sms(&self, serial: &str, id: &str, to: &str, text: &str, wait: Duration) -> SmsOutcome {
        self.send_sms_on(serial, None, id, to, text, wait).await
    }

    /// [`Self::send_sms`] from the SIM `sub_id` (Android subscription id; None =
    /// the phone's default SIM).
    pub async fn send_sms_on(
        &self,
        serial: &str,
        sub_id: Option<i64>,
        id: &str,
        to: &str,
        text: &str,
        wait: Duration,
    ) -> SmsOutcome {
        self.send_sms_from(serial, &SmsSim::from_sub_id(sub_id), id, to, text, wait).await
    }

    /// [`Self::send_sms`] from the SIM `sim`. A SIM by name goes only to an app
    /// that said `supports_sim_select`; any other app gets nothing
    /// ([`SmsOutcome::Unsupported`]) rather than a text from the wrong SIM.
    /// What the phone reports later, also after `wait`, is kept for
    /// [`Self::sms_outcome`].
    pub async fn send_sms_from(
        &self,
        serial: &str,
        sim: &SmsSim,
        id: &str,
        to: &str,
        text: &str,
        wait: Duration,
    ) -> SmsOutcome {
        let id: String = id.chars().take(SMS_ID_MAX).collect();
        if self.sms_waiters.lock().await.contains_key(&id) {
            warn!(serial, id, "send_sms: a message with this id is already waiting");
            return SmsOutcome::Busy;
        }
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
        if matches!(sim, SmsSim::Named(_)) && !self.sim_select_capable.lock().await.contains(&serial) {
            info!(serial, "send_sms: the phone's app cannot pick a SIM by name");
            return SmsOutcome::Unsupported;
        }
        let text: String = text.chars().take(SMS_TEXT_MAX).collect();
        if id.is_empty() || text.trim().is_empty() || !sms_to_ok(to) {
            return SmsOutcome::Failed("bad_request".to_string());
        }
        let (tx, rx) = oneshot::channel();
        {
            let mut waiters = self.sms_waiters.lock().await;
            if waiters.contains_key(&id) {
                return SmsOutcome::Busy;
            }
            waiters.insert(id.clone(), (serial.clone(), tx));
        }
        // A new send under this id: what an earlier one reported no longer applies.
        self.sms_reports.lock().await.remove(&id);
        if let Err(e) = send_ws_json(&sink, send_sms_payload(&id, to, &text, sim)).await {
            self.sms_waiters.lock().await.remove(&id);
            warn!(serial, "send_sms write failed: {e:#}");
            return SmsOutcome::Offline;
        }
        info!(
            serial,
            id,
            sim = %sim.describe(),
            to_len = to.len(),
            text_chars = text.chars().count(),
            "send_sms dispatched"
        );
        match tokio::time::timeout(wait, rx).await {
            Ok(Ok(out)) => out,
            Ok(Err(_)) => SmsOutcome::Offline,
            Err(_) => {
                self.sms_waiters.lock().await.remove(&id);
                warn!(
                    serial,
                    id,
                    "send_sms: no result in {}s (a later one is still recorded)",
                    wait.as_secs()
                );
                SmsOutcome::Timeout
            }
        }
    }

    /// A phone's `send_sms_result`; true when a waiting send took it. Recorded
    /// also after the wait ended. The phone's own `timeout` is not final: the
    /// real result may follow under the same id and replaces it, and a waiting
    /// send keeps waiting for it (up to its own limit). The same result again
    /// (the phone resends its recent ones after reconnecting) changes nothing,
    /// nor does a different one for a text already settled.
    pub async fn handle_sms_result(&self, msg: &Value) -> bool {
        let Some((id, out)) = parse_sms_result(msg) else {
            return false;
        };
        {
            let mut reports = self.sms_reports.lock().await;
            let report = reports.get_or_insert(&id);
            match weigh_result(report.result.as_ref(), &out) {
                ResultNews::New => report.result = Some(out.clone()),
                ResultNews::Repeat => {
                    info!(id, "send_sms_result repeated: already counted");
                    return false;
                }
                ResultNews::Settled => {
                    info!(id, outcome = ?out, known = ?report.result, "send_sms_result ignored: already settled");
                    return false;
                }
            }
        }
        if is_phone_timeout(&out) && self.sms_waiters.lock().await.contains_key(&id) {
            info!(id, "send_sms_result timeout from the phone: still waiting for its real result");
            return false;
        }
        let Some((serial, tx)) = self.sms_waiters.lock().await.remove(&id) else {
            info!(id, outcome = ?out, "send_sms_result after the wait ended: recorded");
            return false;
        };
        info!(serial, id, outcome = ?out, "send_sms_result");
        let _ = tx.send(out);
        true
    }

    /// A phone's `send_sms_delivered`: recorded for [`Self::sms_outcome`]; false
    /// when the message is not one.
    pub async fn handle_sms_delivered(&self, msg: &Value) -> bool {
        let Some((id, ok)) = parse_sms_delivered(msg) else {
            return false;
        };
        let mut reports = self.sms_reports.lock().await;
        let report = reports.get_or_insert(&id);
        // A long text is several parts, each reported: one lost part is not delivered.
        report.delivered = Some(report.delivered.unwrap_or(true) && ok);
        info!(id, ok, "send_sms_delivered");
        true
    }

    /// What became of the text `id` (one of the newest few hundred): `(sent,
    /// delivered)`; `delivered` is None until the phone reports it. None = no
    /// report from the phone yet.
    pub async fn sms_outcome(&self, id: &str) -> Option<(bool, Option<bool>)> {
        let id: String = id.chars().take(SMS_ID_MAX).collect();
        let reports = self.sms_reports.lock().await;
        let r = reports.get(&id)?;
        if r.result.is_none() && r.delivered.is_none() {
            return None;
        }
        // A delivery report only exists for a text that went out.
        let sent = r.result == Some(SmsOutcome::Sent) || r.delivered.is_some();
        Some((sent, r.delivered))
    }

    /// The call task `task_id` came from was taken by `serial` on the SIM
    /// `sub_id`: a call-back from that phone dials out on that SIM.
    pub async fn remember_task_sim(&self, task_id: &str, serial: &str, sub_id: i64) {
        if task_id.trim().is_empty() {
            return;
        }
        self.task_sims
            .lock()
            .await
            .insert(task_id, (serial.trim().to_string(), sub_id));
    }

    /// The SIM for `task_id`'s call-back on `dialing_serial`: the one its call came
    /// in on, when the call-back goes out on that same phone.
    async fn task_sim_on(&self, task_id: &str, dialing_serial: &str) -> Option<i64> {
        let sims = self.task_sims.lock().await;
        let (serial, sub) = sims.get(task_id)?;
        control_serials(serial)
            .iter()
            .any(|s| s == dialing_serial)
            .then_some(*sub)
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
        session.sub_id = self.task_sim_on(task_id, &serial).await;
        let sub_id = session.sub_id;
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
            sub_id = ?sub_id,
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

/// A real control connection for tests: the agent's end ([`WsSink`], as a
/// phone's hello registers it) and the phone's end, which reads what the agent sends.
#[cfg(test)]
pub(crate) mod test_socket {
    use super::*;
    use futures_util::StreamExt;

    pub(crate) type PhoneEnd =
        tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

    pub(crate) async fn phone_socket() -> (WsSink, PhoneEnd) {
        use axum::extract::ws::WebSocketUpgrade;
        let (tx, rx) = oneshot::channel::<WsSink>();
        let tx = Arc::new(std::sync::Mutex::new(Some(tx)));
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(move |ws: WebSocketUpgrade| {
                let tx = tx.clone();
                async move {
                    ws.on_upgrade(move |socket| async move {
                        let (sink, mut stream) = socket.split();
                        if let Some(tx) = tx.lock().unwrap().take() {
                            let _ = tx.send(Arc::new(Mutex::new(sink)));
                        }
                        while let Some(Ok(_)) = stream.next().await {}
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let (phone, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/")).await.unwrap();
        (sink_of(rx).await, phone)
    }

    async fn sink_of(rx: oneshot::Receiver<WsSink>) -> WsSink {
        tokio::time::timeout(Duration::from_secs(5), rx).await.unwrap().unwrap()
    }

    /// The next JSON message the phone gets, or None when nothing comes within `wait`.
    pub(crate) async fn next_json(phone: &mut PhoneEnd, wait: Duration) -> Option<Value> {
        use tokio_tungstenite::tungstenite::Message as WsMessage;
        loop {
            match tokio::time::timeout(wait, phone.next()).await {
                Ok(Some(Ok(WsMessage::Text(t)))) => return serde_json::from_str(t.as_str()).ok(),
                Ok(Some(Ok(_))) => continue,
                _ => return None,
            }
        }
    }
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
        assert!(v.get("sub_id").is_none());
        // The SIM of the call the task came from goes with the dial command.
        let mut s = CallbackSession::new("task-2", "0912345678");
        s.sub_id = Some(3);
        assert_eq!(start_callback_request(&mut s, None)["sub_id"], 3);
    }

    #[tokio::test]
    async fn call_back_uses_the_calls_sim_only_on_the_same_phone() {
        let reg = ControlRegistry::default();
        // The call came in on the work-profile twin; its control connection is the main app's.
        reg.remember_task_sim("task-1", "pixel-1010341", 4).await;
        assert_eq!(reg.task_sim_on("task-1", "pixel-10341").await, Some(4));
        assert_eq!(reg.task_sim_on("task-1", "pixel-10342").await, None);
        assert_eq!(reg.task_sim_on("task-2", "pixel-10341").await, None);
        // Only the newest tasks are kept.
        for i in 0..RECENT_MAX {
            reg.remember_task_sim(&format!("t{i}"), "pixel-10341", 1).await;
        }
        assert_eq!(reg.task_sim_on("task-1", "pixel-10341").await, None);
        assert_eq!(reg.task_sim_on("t0", "pixel-10341").await, Some(1));
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
            send_sms_payload("task-1", "+886912345678", "你好", &SmsSim::Default),
            json!({"type": "send_sms", "id": "task-1", "to": "+886912345678", "text": "你好"})
        );
        assert_eq!(
            send_sms_payload("task-1", "0912345678", "hi", &SmsSim::from_sub_id(Some(2))),
            json!({"type": "send_sms", "id": "task-1", "to": "0912345678", "text": "hi", "sub_id": 2})
        );
        // A SIM by name: "sim" and never "sub_id"; a blank name is the default SIM.
        assert_eq!(
            send_sms_payload("task-1", "+16505550123", "hi", &SmsSim::Named(" Tello ".into())),
            json!({"type": "send_sms", "id": "task-1", "to": "+16505550123", "text": "hi", "sim": "Tello"})
        );
        assert_eq!(
            send_sms_payload("task-1", "+16505550123", "hi", &SmsSim::Named("  ".into())),
            json!({"type": "send_sms", "id": "task-1", "to": "+16505550123", "text": "hi"})
        );
        assert_eq!(SmsSim::Named("+16505550199".into()).describe(), "named(len=12)");
        let ok = json!({"type": "send_sms_result", "id": "task-1", "ok": true});
        assert_eq!(parse_sms_result(&ok), Some(("task-1".to_string(), SmsOutcome::Sent)));
        let failed = json!({"type": "send_sms_result", "id": "task-1", "ok": false, "error": "no_service"});
        assert_eq!(
            parse_sms_result(&failed),
            Some(("task-1".to_string(), SmsOutcome::Failed("no_service".into())))
        );
        let odd = json!({"type": "send_sms_result", "id": "x", "ok": false, "error": "<script>"});
        assert_eq!(parse_sms_result(&odd).unwrap().1, SmsOutcome::Failed("generic_failure".into()));
        let no_sim = json!({"type": "send_sms_result", "id": "x", "ok": false, "error": "no_sim"});
        assert_eq!(parse_sms_result(&no_sim).unwrap().1, SmsOutcome::Failed("no_sim".into()));
        assert_eq!(parse_sms_result(&json!({"type": "place_call_ack", "id": "x"})), None);
        assert_eq!(parse_sms_result(&json!({"type": "send_sms_result", "ok": true})), None);
        assert!(sms_to_ok("+886912345678") && sms_to_ok("0912345678"));
        assert!(!sms_to_ok("09-12") && !sms_to_ok("++886") && !sms_to_ok("") && !sms_to_ok("abc"));
        assert_eq!(control_serials("pixel-1010341"), vec!["pixel-1010341", "pixel-10341"]);
        assert_eq!(control_serials("pixel-10341"), vec!["pixel-10341"]);
    }

    #[tokio::test]
    async fn a_sim_by_name_goes_only_to_apps_that_pick_one() {
        use test_socket::{next_json, phone_socket};
        let reg = ControlRegistry::default();
        let (sink, mut phone) = phone_socket().await;
        reg.register_client("pixel-12345".into(), sink).await;
        reg.set_supports_sms("pixel-12345", true).await;
        assert_eq!(reg.connected_serials().await, vec!["pixel-12345".to_string()]);
        let tello = SmsSim::Named("Tello".into());
        let wait = Duration::from_millis(200);
        // An app without `supports_sim_select`: nothing is sent (it would use its default SIM).
        let out = reg.send_sms_from("pixel-12345", &tello, "n1", "+16505550123", "hi", wait).await;
        assert_eq!(out, SmsOutcome::Unsupported);
        assert_eq!(next_json(&mut phone, Duration::from_millis(100)).await, None);
        // A SIM by id still goes to it.
        let out = reg.send_sms_from("pixel-12345", &SmsSim::SubId(3), "n2", "+16505550123", "hi", wait).await;
        assert_eq!(out, SmsOutcome::Timeout);
        assert_eq!(next_json(&mut phone, wait).await.unwrap()["sub_id"], 3);
        // An app that picks a SIM by name gets it, without `sub_id`.
        reg.set_supports_sim_select("pixel-12345", true).await;
        let (out, msg) = tokio::join!(
            reg.send_sms_from("pixel-12345", &tello, "n3", "+16505550123", "hi", Duration::from_secs(5)),
            async {
                let msg = next_json(&mut phone, Duration::from_secs(5)).await.unwrap();
                reg.handle_sms_result(&json!({"type": "send_sms_result", "id": "n3", "ok": true})).await;
                msg
            }
        );
        assert_eq!(out, SmsOutcome::Sent);
        assert_eq!((msg["sim"].clone(), msg.get("sub_id")), (json!("Tello"), None));
        // Forgotten with the phone, until its next hello.
        reg.unregister_client("pixel-12345").await;
        assert!(!reg.sim_select_capable.lock().await.contains("pixel-12345"));
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
        // The same id while one waits: refused, and the waiting one still gets its answer.
        let (tx, rx) = oneshot::channel();
        reg.sms_waiters.lock().await.insert("t4".into(), ("pixel-1".into(), tx));
        let again = reg
            .send_sms("pixel-1", "t4", "+886912345678", "hi", Duration::from_millis(50))
            .await;
        assert_eq!(again, SmsOutcome::Busy);
        assert!(reg.handle_sms_result(&json!({"type": "send_sms_result", "id": "t4", "ok": true})).await);
        assert_eq!(rx.await.unwrap(), SmsOutcome::Sent);
        // The phone drops: its waiting messages end, its texting flag goes.
        let (tx, rx) = oneshot::channel();
        reg.sms_waiters.lock().await.insert("t3".into(), ("pixel-1".into(), tx));
        reg.set_supports_sms("pixel-1", true).await;
        reg.unregister_client("pixel-1").await;
        assert!(rx.await.is_err());
        assert!(!reg.sms_capable.lock().await.contains("pixel-1"));
    }

    #[tokio::test]
    async fn sms_results_count_once_and_late_ones_are_kept() {
        let reg = ControlRegistry::default();
        assert_eq!(reg.sms_outcome("t5").await, None);
        // The agent stopped waiting (90 s) before the phone answered: still recorded.
        let late = json!({"type": "send_sms_result", "id": "t5", "ok": true});
        assert!(!reg.handle_sms_result(&late).await);
        assert_eq!(reg.sms_outcome("t5").await, Some((true, None)));
        // The phone resends its recent results after reconnecting: counted once,
        // and a different one does not undo a settled text.
        assert!(!reg.handle_sms_result(&late).await);
        let replay = json!({"type": "send_sms_result", "id": "t5", "ok": false, "error": "no_service"});
        assert!(!reg.handle_sms_result(&replay).await);
        assert_eq!(reg.sms_outcome("t5").await, Some((true, None)));
        // The phone timed out (60 s), then its radio answered: the real result
        // replaces the timeout, a resent timeout does not come back.
        let timeout = json!({"type": "send_sms_result", "id": "t8", "ok": false, "error": "timeout"});
        let sent = json!({"type": "send_sms_result", "id": "t8", "ok": true});
        assert!(!reg.handle_sms_result(&timeout).await);
        assert_eq!(reg.sms_outcome("t8").await, Some((false, None)));
        assert!(!reg.handle_sms_result(&sent).await);
        assert_eq!(reg.sms_outcome("t8").await, Some((true, None)));
        assert!(!reg.handle_sms_result(&timeout).await);
        assert!(!reg.handle_sms_result(&sent).await);
        assert_eq!(reg.sms_outcome("t8").await, Some((true, None)));
        // A send still waiting is not ended by the phone's timeout; it gets the
        // real result that follows.
        let (tx, mut rx) = oneshot::channel();
        reg.sms_waiters.lock().await.insert("t9".into(), ("pixel-1".into(), tx));
        let t9_timeout = json!({"type": "send_sms_result", "id": "t9", "ok": false, "error": "timeout"});
        assert!(!reg.handle_sms_result(&t9_timeout).await);
        assert!(rx.try_recv().is_err());
        assert!(reg.handle_sms_result(&json!({"type": "send_sms_result", "id": "t9", "ok": true})).await);
        assert_eq!(rx.await.unwrap(), SmsOutcome::Sent);
        // A waiting send gets the first result; a repeat does not reach it twice.
        let (tx, rx) = oneshot::channel();
        reg.sms_waiters.lock().await.insert("t6".into(), ("pixel-1".into(), tx));
        let failed = json!({"type": "send_sms_result", "id": "t6", "ok": false, "error": "radio_off"});
        assert!(reg.handle_sms_result(&failed).await);
        assert_eq!(rx.await.unwrap(), SmsOutcome::Failed("radio_off".into()));
        assert!(!reg.handle_sms_result(&failed).await);
        assert_eq!(reg.sms_outcome("t6").await, Some((false, None)));
        // Delivery reports: every part has to arrive.
        let delivered = |ok: bool| json!({"type": "send_sms_delivered", "id": "t5", "ok": ok});
        assert!(reg.handle_sms_delivered(&delivered(true)).await);
        assert_eq!(reg.sms_outcome("t5").await, Some((true, Some(true))));
        assert!(reg.handle_sms_delivered(&delivered(false)).await);
        assert!(reg.handle_sms_delivered(&delivered(true)).await);
        assert_eq!(reg.sms_outcome("t5").await, Some((true, Some(false))));
        assert!(!reg.handle_sms_delivered(&json!({"type": "send_sms_result", "id": "t5"})).await);
        // A delivery report before the result: it went out.
        let early = json!({"type": "send_sms_delivered", "id": "t7", "ok": true});
        assert!(reg.handle_sms_delivered(&early).await);
        assert_eq!(reg.sms_outcome("t7").await, Some((true, Some(true))));
    }

    #[tokio::test]
    async fn only_apps_that_say_so_can_decline() {
        let reg = ControlRegistry::default();
        assert!(!reg.supports_reject("pixel-10341").await);
        reg.set_supports_reject("pixel-10341", true).await;
        assert!(reg.supports_reject("pixel-10341").await);
        // The work-profile twin's call rings on the main app's connection.
        assert!(reg.supports_reject("pixel-1010341").await);
        // A reconnect with an older app (no field) takes it back; so does a drop.
        reg.set_supports_reject("pixel-10341", false).await;
        assert!(!reg.supports_reject("pixel-10341").await);
        reg.set_supports_reject("pixel-10341", true).await;
        reg.unregister_client("pixel-10341").await;
        assert!(!reg.supports_reject("pixel-10341").await);
    }
}

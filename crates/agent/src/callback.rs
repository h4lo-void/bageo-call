//! Outbound callback routing: idle control WS → `place_call` → session tracking.
//!
//! Phone idle control client (`AgentControlClient`) sends hello with `mode=CONTROL_IDLE`.
//! Fulfillment (`ReplyMethod::Phone`) pushes `callback_requested`, then dispatches
//! `place_call` only when: user confirmed + number on `callback_whitelist` + idle
//! control client connected (never auto-dials; empty whitelist = fail-closed).

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use futures_util::SinkExt;
use serde_json::{json, Value};
use tokio::sync::Mutex;
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

/// Connected idle-control WebSocket clients and in-flight callback sessions.
#[derive(Clone, Default)]
pub struct ControlRegistry {
    clients: Arc<Mutex<HashMap<String, WsSink>>>,
    sessions: Arc<Mutex<HashMap<String, CallbackSession>>>,
    call_index: Arc<Mutex<HashMap<String, String>>>,
}

impl ControlRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
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
        } else {
            info!(
                serial,
                count = clients.len(),
                "control_idle stale socket closed (newer client kept)"
            );
        }
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

        let (serial, sink) = if let Some(s) = clients.get(prefer_serial) {
            (prefer_serial.to_string(), s.clone())
        } else {
            let (serial, sink) = clients.iter().next().expect("non-empty");
            (serial.clone(), sink.clone())
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
}

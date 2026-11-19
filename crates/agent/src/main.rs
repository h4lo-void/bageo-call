// Copyright (C) 2026 h4lo-void (https://github.com/h4lo-void)
// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>
// See LICENSE, LICENSING.md (AGPL-3.0 Section 7 additional terms), and COMMERCIAL_LICENSE.md.

//! Bageo agent.
//!
//! Phones connect by WebSocket (`/` or `/ws`): JSON text frames and raw s16le
//! mono PCM in both directions (the messages are listed in `android/README.md`).
//! Each phone keeps one idle control connection and opens one media connection
//! per answered call.
//!
//! For a call the agent runs a Gemini Live session ([`gemini_live`]), records
//! it ([`calls`]), writes the summary and the calendar suggestion
//! ([`summary`], [`calendar`]) and sends the notifications ([`notify`]). The
//! web console and its API are in [`calls_http`], the settings in
//! [`settings_store`], languages in [`locale`] and [`prompts`], and the USB
//! link to the phones in [`usb_bridge`].
//!
//! Default: the AI answers directly (`AGENT_MODE=ivr` with persona mode on).
//! This file also still carries the older menu-based flows — recorded
//! greeting, DTMF, hold music, voicemail, a TRTC bridge and an
//! STT → LLM → TTS pipeline (`AGENT_MODE=llm`); see `docs/en/ARCHITECTURE.md`.

// The older menu-based call flows are still compiled in (docs/en/ARCHITECTURE.md,
// "Older code paths"); much of what only they use is unreachable by default.
#![allow(dead_code)]

mod adpcm;
mod ambience;
mod appointments;
mod backup;
mod backup_crypt;
mod calendar;
mod callback;
mod calls;
mod calls_http;
mod closing;
mod config;
mod contacts;
mod control;
mod country_profile;
mod debug_probe;
mod dialect;
mod summary;
mod dtmf;
mod errands;
mod extract;
mod fulfill;
mod gemini_live;
mod gmail;
mod hold_music;
mod lan;
mod lang_lock;
mod locale;
mod log_text;
mod mcp;
mod mdns;
mod notify;
mod openclaw;
mod phone_region;
mod phone_wifi;
mod preflight;
mod prompts;
mod recent_notices;
mod services;
mod settings_store;
mod sms_store;
mod scam_db;
mod scenario;
mod stall_review;
mod stall_script;
mod tasks;
mod tg_inbound;
mod trtc;
mod trtc_api;
mod trtc_bridge;
mod urgent_call;
mod urgency;
mod usb_bridge;
mod vad;
mod voice_preview;
mod wyoming;
mod zh_convert;
mod zh_tw;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Timelike;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{Mutex, Notify, RwLock};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};

use callback::{ControlRegistry, WsSink};
use calls::{ActiveCall, CallRecord, CallStore};
use calls_http::{router as calls_router, CallsState};
use config::{AgentMode, LlmProvider, Settings};
use debug_probe::agent_dbg;
use dtmf::DtmfDecoder;
use extract::{process_voicemail_file, SpokenContact};
use hold_music::{
    load_fail_wav_pcm, load_hold_music_pcm, load_ivr_segments, parse_wav_list,
    trim_leading_silence_s16le, IvrSegment,
};
use notify::{
    call_detail_line, listen_recording_line, notify_ai_request, notify_configured,
    notify_incoming_call, notify_outgoing_call, notify_primary_backend, notify_rejected_call, notify_voicemail,
};
use services::{chat, ensure_ollama_model, synthesize_piper, transcribe_pcm16, ChatMessage};
use settings_store::{RuntimeSettings, SettingsStore};
use tasks::{draft_from_voicemail, ReplyMethod, TaskIntent, TaskPatch, TaskStore};
use tg_inbound::send_task_card;
use trtc_bridge::TrtcBridge;
use vad::{EnergyVad, RemoteVoiceGate};

type ActiveCallSlot = Arc<Mutex<Option<ActiveCall>>>;
type GeminiAudioOpenedAt = Arc<std::sync::Mutex<Option<tokio::time::Instant>>>;

/// Identifies the logical media call currently owning this WebSocket.
/// A new hello, reset, call_end, or disconnect invalidates every older clone.
#[derive(Clone)]
struct MediaCallGuard {
    generation: Arc<AtomicU64>,
    lease: Arc<RwLock<()>>,
    epoch: u64,
}

impl MediaCallGuard {
    fn inactive() -> Self {
        Self {
            generation: Arc::new(AtomicU64::new(0)),
            lease: Arc::new(RwLock::new(())),
            epoch: 0,
        }
    }

    async fn next(&self) -> Self {
        let _write = self.lease.write().await;
        let epoch = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        Self {
            generation: self.generation.clone(),
            lease: self.lease.clone(),
            epoch,
        }
    }

    async fn invalidate(&self) {
        let _write = self.lease.write().await;
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn is_current(&self) -> bool {
        self.epoch != 0 && self.generation.load(Ordering::SeqCst) == self.epoch
    }

    async fn lease_current(&self) -> Option<tokio::sync::OwnedRwLockReadGuard<()>> {
        let lease = self.lease.clone().read_owned().await;
        if self.is_current() {
            Some(lease)
        } else {
            None
        }
    }
}

/// IVR session phases (`AGENT_MODE=ivr|transfer|ivr_transfer`).
/// Product: period greeting → auto Gemini AI; fail → voicemail. Optional DTMF 2 = VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IvrPhase {
    /// Period auto-transfer greeting (local inject or agent-side WAV).
    PlayPrompt,
    /// Brief post-greeting gate before auto-AI arm (not a keypad menu).
    WaitDtmf,
    /// Legacy hold-music phase (unused by product DTMF1; kept for enum/compat).
    PlayHoldMusic,
    /// Voicemail intro + beep playing after DTMF 2 (recording starts after beep).
    VoicemailIntro,
    /// Recording the caller's message (ends on trailing silence / `#` / max length / hangup).
    VoicemailRecord,
    /// Whisper transcribing the just-recorded message for phone/email read-back.
    VoicemailContactProbe,
    /// Spoken contact read-back prompt; waiting for `1` confirm / `2` reject.
    VoicemailContactReadback,
    /// Callback-confirm prompt after recording; waiting for `1` / end key / timeout.
    VoicemailConfirm,
    /// Legacy local AI (OpenRouter); product AI talk uses TrtcAi instead.
    LiveAi,
    /// Menu DTMF 1 → Tencent TRTC AI (preferred when configured). Soft scaffold until bridge.
    TrtcAi,
    /// Goodbye / fail WAV then WS `hangup`.
    Goodbye,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineKind {
    IvrReprompt,
    /// No DTMF after the spoken-contact read-back prompt.
    VmContactReadback,
    /// No DTMF after the voicemail callback-confirm prompt.
    VmConfirm,
}

#[derive(Clone)]
struct AppState {
    settings: Settings,
    /// Runtime-editable settings (web console); env stays the default source.
    settings_store: Arc<SettingsStore>,
    http: reqwest::Client,
    /// Pre-decoded hold music (16 kHz s16le mono), if available.
    hold_pcm: Option<Arc<Vec<u8>>>,
    /// IVR menu WAV segments in play order (01→02→03→04), each 16 kHz s16le mono.
    ivr_prompt_segments: Arc<Vec<IvrSegment>>,
    /// Fail / leave-message WAV PCM after hold or DTMF 2.
    ivr_fail_pcm: Option<Arc<Vec<u8>>>,
    /// Goodbye WAV after max menu plays / DTMF 0|#.
    ivr_goodbye_pcm: Option<Arc<Vec<u8>>>,
    /// Reject unknown/private/hidden caller (no greet / no Gemini).
    ivr_reject_unknown_pcm: Option<Arc<Vec<u8>>>,
    /// Optional DTMF1 wait / AI hello / nudge clone WAVs.
    ivr_dtmf1_wait_pcm: Option<Arc<Vec<u8>>>,
    ivr_ai_hello_pcm: Option<Arc<Vec<u8>>>,
    ivr_ai_nudge_pcm: Option<Arc<Vec<u8>>>,
    /// Voicemail intro WAV (after DTMF 2, before the beep), if present.
    ivr_vm_intro_pcm: Option<Arc<Vec<u8>>>,
    /// Voicemail callback-confirm WAV (after the message is recorded), if present.
    ivr_vm_confirm_pcm: Option<Arc<Vec<u8>>>,
    /// Pre-decoded period full greets (`ivr_menu_full_*.wav`) for zero disk I/O at answer.
    ivr_menu_full_pcm: Arc<Vec<(String, Arc<Vec<u8>>)>>,
    /// Synthesized beep tone that cues the caller to start speaking.
    vm_beep_pcm: Arc<Vec<u8>>,
    /// Binary marker so health/logs prove post-latency-fix agent is live.
    latency_build: &'static str,
    calls: Arc<CallStore>,
    /// Assistant tasks (voicemail → extract → fulfill).
    task_store: Arc<TaskStore>,
    /// Idle control WS clients (`mode=CONTROL_IDLE`) for outbound callbacks.
    control: Arc<ControlRegistry>,
    /// What each phone reported on its control connection (AI / normal phone mode,
    /// app version) and the web console's mode switch.
    phones: Arc<calls_http::PhoneDevices>,
    /// Rolling TRTC AI talk metrics (caller finished → AI audio back).
    trtc_talk: Arc<TrtcTalkMetrics>,
    /// Process-wide short-window dedup for incoming-call notifications. A bridge
    /// reconnect can create a new WS and replay `hello` for the same call.
    incoming_notify_dedup: Arc<IncomingNotifyDedup>,
    /// Process-wide live/parked media calls so a reconnect hello on a new WS
    /// reuses the same `call_id` instead of `CallStore::start()` (ghost rows).
    call_reattach: Arc<CallReattachRegistry>,
    /// Exclusive greet/reject per physical device (twins `pixel-1NNNN` + `pixel-101NNNN`).
    media_play: Arc<MediaPlayGate>,
    /// Gemini sessions started while a phone rings, waiting for its media hello.
    ring_prewarm: Arc<RingPrewarmRegistry>,
    /// Ringing calls from blocked numbers, waiting for their media hello to hang up.
    reject_pending: Arc<RejectPending>,
    /// Voice preview lines and the blocked-number announcement, cached on disk.
    previews: Arc<voice_preview::VoicePreviews>,
    /// Background sound beds (`ambience_<kind>.wav` in the assets dir).
    ambience: Arc<ambience::AmbienceLibrary>,
    /// Calls the owner has the AI make ([`errands`]).
    errands: Arc<errands::ErrandStore>,
}

const INCOMING_NOTIFY_DEDUP_WINDOW: Duration = Duration::from_secs(30);
/// Park a dropped media call this long so a reconnect hello can attach.
const CALL_REATTACH_WINDOW: Duration = Duration::from_secs(20);
/// Empty media-hello CID is not proof of hidden (Android often late). Bound wait
/// for ringing/hello caller_id; then reject-only if still empty.
const EMPTY_CID_WAIT: Duration = Duration::from_millis(1500);

#[derive(Default)]
struct IncomingNotifyDedup {
    entries: Mutex<HashMap<String, tokio::time::Instant>>,
}

impl IncomingNotifyDedup {
    async fn claim(&self, serial: &str, caller: &str) -> bool {
        let key = format!("{}\u{1}{}", serial.trim(), caller.trim());
        let now = tokio::time::Instant::now();
        let mut entries = self.entries.lock().await;
        entries.retain(|_, seen| now.duration_since(*seen) < INCOMING_NOTIFY_DEDUP_WINDOW);
        if entries.contains_key(&key) {
            return false;
        }
        entries.insert(key, now);
        true
    }

    async fn release(&self, serial: &str, caller: &str) {
        let key = format!("{}\u{1}{}", serial.trim(), caller.trim());
        self.entries.lock().await.remove(&key);
    }
}

/// An unanswered ring prewarm is dropped this long after the planned pickup.
const RING_PREWARM_GRACE: Duration = Duration::from_secs(12);

/// Ring once before answering, with the AI ready: the phone reports `incoming` on CONTROL_IDLE while it
/// rings; the call's Gemini session starts then (connect, setup, 「喂？」 generated
/// and held) and the same phone's media hello takes it over after pickup.
#[derive(Default)]
struct RingPrewarmRegistry {
    next_id: AtomicU64,
    entries: std::sync::Mutex<HashMap<String, RingPrewarm>>,
}

/// One ringing call's Gemini session (warmhold until UnmuteAfterGreet).
struct RingPrewarm {
    id: u64,
    caller_id: String,
    sample_rate: u32,
    started: tokio::time::Instant,
    core: GeminiSessionCore,
    up_tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    cancel: Arc<AtomicBool>,
    transcript: Arc<std::sync::Mutex<String>>,
    audio_open: Arc<AtomicBool>,
    audio_opened_at: GeminiAudioOpenedAt,
    setup_ready: Arc<AtomicBool>,
    connect_failed: Arc<AtomicBool>,
    cfg: GeminiRetryCfg,
    /// What the session was set up for from the number and SIM seen while it rang.
    plan: CallerPlan,
    persona: bool,
    goodbye_hangup_delay_ms: u64,
}

impl RingPrewarmRegistry {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, RingPrewarm>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Store `entry` as this phone's ringing call (a previous one is stopped).
    fn insert(&self, serial: &str, mut entry: RingPrewarm) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        entry.id = id;
        if let Some(old) = self.entries().insert(serial.to_string(), entry) {
            old.cancel.store(true, Ordering::Relaxed);
            info!("Ring prewarm replaced serial={serial} old_id={}", old.id);
        }
        id
    }

    /// Stop and forget the phone's entry (only if it is still `id`, when given).
    fn cancel(&self, serial: &str, id: Option<u64>, why: &str) {
        let mut entries = self.entries();
        if !entries
            .get(serial)
            .is_some_and(|e| id.map_or(true, |id| e.id == id))
        {
            return;
        }
        if let Some(e) = entries.remove(serial) {
            e.cancel.store(true, Ordering::Relaxed);
            info!(
                "Ring prewarm dropped serial={serial} id={} why={why} age_ms={}",
                e.id,
                e.started.elapsed().as_millis()
            );
        }
    }

    /// Media hello after pickup: the ringing call's session, if still usable.
    /// `plan`: what the call needs, decided now with the number and SIM the hello
    /// reported — a session set up for something else (the number sent while ringing
    /// can be empty or the previous call's, the SIM guessed) is not handed over, so
    /// the pickup starts a fresh one.
    fn take(
        &self,
        serial: &str,
        caller_id: &str,
        sample_rate: u32,
        plan: &CallerPlan,
    ) -> Option<RingPrewarm> {
        let e = self.entries().remove(serial)?;
        let why = if e.cancel.load(Ordering::Relaxed) {
            "cancelled"
        } else if e.core.session.is_finished() {
            "session already ended"
        } else if e.sample_rate != sample_rate {
            "sample rate differs"
        } else if e.plan.lang != plan.lang {
            info!(
                "Ring prewarm languages serial={serial} id={} rang={:?} pickup={:?}",
                e.id, e.plan.lang, plan.lang
            );
            "languages differ at pickup"
        } else if e.plan.intl_hold != plan.intl_hold && has_digits(&e.caller_id) && has_digits(caller_id) {
            // (With no number on one side, the session reads the other one at
            // pickup: a hidden number waits for the carrier's warning, a local
            // one does not.)
            "international-call prompt differs at pickup"
        } else if e.plan.appointments != plan.appointments {
            "appointments differ at pickup"
        } else if plan.appointments && e.caller_id.trim() != caller_id.trim() {
            // The session's appointments are bound to the number it rang with.
            "appointments bound to the ringing number"
        } else if e.plan.contact != plan.contact {
            // The prompt names the contact of the number it rang with (or none).
            "caller's contact differs at pickup"
        } else {
            // The number sent while ringing can be stale (call-log fallback before
            // Telecom publishes the CID): everything it decided is the same for the
            // hello's number, which the session takes over at adoption.
            if !same_ringing_caller(&e.caller_id, caller_id) {
                info!(
                    "Ring prewarm caller differs serial={serial} id={} rang_caller={} hello_caller={} — still the ringing call, adopt",
                    e.id,
                    urgent_call::masked(&e.caller_id),
                    urgent_call::masked(caller_id)
                );
            }
            return Some(e);
        };
        e.cancel.store(true, Ordering::Relaxed);
        warn!(
            "Ring prewarm not used serial={serial} id={} why={why} rang_caller={} hello_caller={} age_ms={}",
            e.id,
            urgent_call::masked(&e.caller_id),
            urgent_call::masked(caller_id),
            e.started.elapsed().as_millis()
        );
        None
    }

    fn pending(&self) -> usize {
        self.entries().len()
    }
}

/// Pickup: the ringing call's prewarmed session (persona mode only — the prewarm
/// starts the persona opening).
fn take_ring_prewarm(
    state: &AppState,
    rt: &RuntimeSettings,
    serial: &str,
    caller_id: &str,
    sample_rate: u32,
) -> Option<RingPrewarm> {
    if !rt.persona_flow_active() {
        state.ring_prewarm.cancel(serial, None, "persona mode off at pickup");
        return None;
    }
    // The number and SIM are known now (media hello); the ring may have had to guess.
    let plan = caller_plan(rt, caller_id, call_sim_country(serial).as_deref());
    state.ring_prewarm.take(serial, caller_id, sample_rate, &plan)
}

/// What a call's Live session is set up for from the caller's number and the SIM
/// it came in on. A ring prewarm decides it with what the phone said while it
/// rang; the pickup decides again with the media hello's.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CallerPlan {
    /// Opening language and the caller's speech-to-text hints.
    lang: gemini_live::CallLangPlan,
    /// The opening waits for the carrier's international-call warning.
    intl_hold: bool,
    /// Appointments are on for this caller (the session is bound to the number).
    appointments: bool,
    /// The contact the caller's number belongs to (its id): the prompt names them.
    contact: Option<String>,
}

/// [`CallerPlan`] for `caller_id` on the SIM of country `sim` (None = unknown),
/// as a persona call's session decides each part.
fn caller_plan(rt: &RuntimeSettings, caller_id: &str, sim: Option<&str>) -> CallerPlan {
    CallerPlan {
        lang: call_lang_plan(rt, caller_id, sim),
        intl_hold: gemini_live::expects_carrier_intl_prompt(
            rt.lang() == locale::Lang::ZhTw,
            caller_id,
            sim,
        ),
        appointments: appointments::Mode::parse(&rt.appointment_mode) != appointments::Mode::Off,
        contact: contacts::lookup(caller_id, sim).map(|c| c.id),
    }
}

/// The number has at least one digit (a hidden number is empty or a word).
fn has_digits(number: &str) -> bool {
    number.chars().any(|c| c.is_ascii_digit())
}

/// Ring and hello report the same call (CID may still be empty on one side, or
/// carry +886 on one and 0 on the other).
fn same_ringing_caller(rang: &str, hello: &str) -> bool {
    let digits = |s: &str| -> String { s.chars().filter(char::is_ascii_digit).collect() };
    let (a, b) = (digits(rang), digits(hello));
    if a.is_empty() || b.is_empty() {
        return true;
    }
    if a.len() >= 8 && b.len() >= 8 {
        return a[a.len() - 8..] == b[b.len() - 8..];
    }
    a == b
}

#[derive(Default)]
struct CallReattachRegistry {
    entries: Mutex<HashMap<String, ReattachEntry>>,
}

struct ReattachEntry {
    parked: Option<ActiveCall>,
    live_slot: Option<ActiveCallSlot>,
    call_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnsureCall {
    Existing,
    Reattached,
    Started,
    Skipped,
}

impl CallReattachRegistry {
    fn key(serial: &str, caller: &str) -> String {
        format!("{}\u{1}{}", serial.trim(), caller.trim())
    }

    async fn steal(&self, serial: &str, caller: &str) -> Option<ActiveCall> {
        let key = Self::key(serial, caller);
        let live_slot = {
            let mut entries = self.entries.lock().await;
            let entry = entries.get_mut(&key)?;
            if let Some(call) = entry.parked.take() {
                return Some(call);
            }
            entry.live_slot.clone()
        };
        if let Some(slot) = live_slot {
            if let Some(call) = slot.lock().await.take() {
                return Some(call);
            }
        }
        let mut entries = self.entries.lock().await;
        entries.get_mut(&key).and_then(|e| e.parked.take())
    }

    async fn mark_live(&self, serial: &str, caller: &str, call_id: &str, slot: ActiveCallSlot) {
        let key = Self::key(serial, caller);
        let mut entries = self.entries.lock().await;
        entries.insert(
            key,
            ReattachEntry {
                parked: None,
                live_slot: Some(slot),
                call_id: call_id.to_string(),
            },
        );
    }

    async fn rekey(&self, serial: &str, old_caller: &str, new_caller: &str) {
        let new_caller = new_caller.trim();
        let old_caller = old_caller.trim();
        if new_caller.is_empty() || old_caller == new_caller {
            return;
        }
        let old_key = Self::key(serial, old_caller);
        let new_key = Self::key(serial, new_caller);
        let mut entries = self.entries.lock().await;
        if let Some(entry) = entries.remove(&old_key) {
            entries.insert(new_key, entry);
        }
    }

    async fn park(&self, serial: &str, caller: &str, call: ActiveCall) {
        let key = Self::key(serial, caller);
        let call_id = call.id.clone();
        let mut entries = self.entries.lock().await;
        entries.insert(
            key,
            ReattachEntry {
                parked: Some(call),
                live_slot: None,
                call_id,
            },
        );
    }

    async fn take_parked_by_id(&self, call_id: &str) -> Option<ActiveCall> {
        let mut entries = self.entries.lock().await;
        let key = entries
            .iter()
            .find(|(_, e)| e.call_id == call_id && e.parked.is_some())
            .map(|(k, _)| k.clone())?;
        let mut entry = entries.remove(&key)?;
        entry.parked.take()
    }

    async fn forget(&self, serial: &str, caller: &str, call_id: &str) {
        let key = Self::key(serial, caller);
        let mut entries = self.entries.lock().await;
        if entries.get(&key).is_some_and(|e| e.call_id == call_id) {
            entries.remove(&key);
            return;
        }
        entries.retain(|_, e| e.call_id != call_id);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaPlayKind {
    Greet,
    Reject,
}

struct DevicePlay {
    kind: MediaPlayKind,
    serial: String,
    cancel: Arc<AtomicBool>,
}

/// One greet or reject clip per canonical device. Twin serials from one phone
/// (`pixel-1NNNN` + isolated `pixel-101NNNN`) must not play reject on one WS
/// and period greet on the other.
#[derive(Default)]
struct MediaPlayGate {
    inner: Mutex<HashMap<String, DevicePlay>>,
}

impl MediaPlayGate {
    fn key(serial: &str) -> String {
        canonical_device_serial(serial)
    }

    async fn claim(
        &self,
        serial: &str,
        kind: MediaPlayKind,
        cancel: &Arc<AtomicBool>,
    ) -> bool {
        let key = Self::key(serial);
        let mut g = self.inner.lock().await;
        let snapshot = g.get(&key).map(|e| {
            (
                e.kind,
                e.serial.clone(),
                e.cancel.clone(),
            )
        });
        match snapshot {
            None => {
                g.insert(
                    key,
                    DevicePlay {
                        kind,
                        serial: serial.to_string(),
                        cancel: cancel.clone(),
                    },
                );
                true
            }
            Some((ekind, eserial, ecancel)) if eserial == serial => {
                if kind == MediaPlayKind::Reject {
                    ecancel.store(true, Ordering::Relaxed);
                    g.insert(
                        key,
                        DevicePlay {
                            kind: MediaPlayKind::Reject,
                            serial: serial.to_string(),
                            cancel: cancel.clone(),
                        },
                    );
                    true
                } else {
                    ekind != MediaPlayKind::Reject
                }
            }
            Some((ekind, _, _)) if ekind == MediaPlayKind::Reject => {
                kind == MediaPlayKind::Reject
            }
            Some((_, _, ecancel)) if kind == MediaPlayKind::Reject => {
                ecancel.store(true, Ordering::Relaxed);
                g.insert(
                    key,
                    DevicePlay {
                        kind: MediaPlayKind::Reject,
                        serial: serial.to_string(),
                        cancel: cancel.clone(),
                    },
                );
                true
            }
            Some(_) => false,
        }
    }

    async fn release(&self, serial: &str) {
        let key = Self::key(serial);
        let raw = serial.to_string();
        let mut g = self.inner.lock().await;
        if g.get(&key).is_some_and(|e| e.serial == raw) {
            g.remove(&key);
        }
    }
}

/// Hello CID decision. Empty waits; explicit hidden tokens reject immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CidRoute {
    Greet,
    Reject,
    /// On the blocked-numbers list: hang up at once, nothing played.
    RejectList,
    Wait,
    IgnoreIsolated,
}

/// `answer_unknown` = runtime setting `answer_unknown_callers`: hidden / private /
/// empty CID is answered like any caller. No CID wait then — both outcomes
/// greet, and a late CID still updates the call record on `ringing`.
/// `listed` = the caller is on the blocked-numbers list (or was when the phone rang).
fn cid_route_on_hello(serial: &str, caller: &str, answer_unknown: bool, listed: bool) -> CidRoute {
    if is_isolated_screening_serial(serial) {
        return CidRoute::IgnoreIsolated;
    }
    if listed {
        return CidRoute::RejectList;
    }
    if answer_unknown {
        return CidRoute::Greet;
    }
    if is_immediate_hidden_caller(caller) {
        return CidRoute::Reject;
    }
    if caller.trim().is_empty() {
        return CidRoute::Wait;
    }
    CidRoute::Greet
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HelloRoutePlan {
    outbound: bool,
    reject_listed: bool,
    route: CidRoute,
    allow_ring_prewarm: bool,
}

/// Decide media-hello routing while giving active outbound calls (appointments
/// callbacks, errands, to-do callbacks) priority over incoming-call blocking
/// and ring prewarm adoption.
fn plan_hello_routing(
    serial: &str,
    caller: &str,
    answer_unknown: bool,
    outbound: bool,
    is_reject_listed: impl FnOnce() -> bool,
) -> HelloRoutePlan {
    if is_isolated_screening_serial(serial) {
        return HelloRoutePlan {
            outbound,
            reject_listed: false,
            route: CidRoute::IgnoreIsolated,
            allow_ring_prewarm: false,
        };
    }
    if outbound {
        return HelloRoutePlan {
            outbound: true,
            reject_listed: false,
            route: CidRoute::Greet,
            allow_ring_prewarm: false,
        };
    }
    let reject_listed = is_reject_listed();
    let route = cid_route_on_hello(serial, caller, answer_unknown, reject_listed);
    HelloRoutePlan {
        outbound: false,
        reject_listed,
        route,
        allow_ring_prewarm: route == CidRoute::Greet,
    }
}

/// Whether this media `hello` / `ringing` matches an active outbound call
/// placed by the agent (appointment callback, errand, or to-do voicemail callback).
async fn hello_is_outbound(
    state: &AppState,
    serial: &str,
    caller_id: &str,
    parsed: &Value,
) -> bool {
    hello_is_outbound_with(state.control.as_ref(), state.errands.as_ref(), serial, caller_id, parsed).await
}

async fn hello_is_outbound_with(
    control: &callback::ControlRegistry,
    errands: &errands::ErrandStore,
    serial: &str,
    caller_id: &str,
    parsed: &Value,
) -> bool {
    let task_id = parsed
        .get("task_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let call_id = parsed
        .get("call_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !task_id.is_empty() || !call_id.is_empty() {
        if let Some(session) = control.callback_session_for(task_id, call_id).await {
            let same_phone = session.serial.eq_ignore_ascii_case(serial.trim())
                || canonical_device_serial(&session.serial)
                    .eq_ignore_ascii_case(&canonical_device_serial(serial));
            if same_phone
                && !matches!(
                    session.phase,
                    callback::CallbackPhase::Failed | callback::CallbackPhase::Done
                )
            {
                return true;
            }
        }
    }
    if appointments::outgoing_call_on(serial, caller_id).is_some() {
        return true;
    }
    if fulfill::voicemail_callback_dialing(control, serial, caller_id).await {
        return true;
    }
    if caller_id.trim().is_empty()
        && (appointments::callback_pending_on(serial) || fulfill::voicemail_callback_pending(serial))
    {
        return true;
    }
    errands.list().into_iter().any(|e| {
        e.status == "dialing"
            && e.serial.eq_ignore_ascii_case(serial.trim())
            && (caller_id.trim().is_empty()
                || contacts::same_number(
                    &e.number,
                    caller_id,
                    (!e.sim_country.is_empty()).then_some(e.sim_country.as_str()),
                ))
    })
}

fn cid_route_after_wait(caller: &str, answer_unknown: bool, listed: bool) -> CidRoute {
    if listed {
        CidRoute::RejectList
    } else if !answer_unknown && is_unknown_or_hidden_caller(caller) {
        CidRoute::Reject
    } else {
        CidRoute::Greet
    }
}

/// How long a blocked number seen while ringing still applies to the phone's next
/// media hello (pickup follows at once; the hello can come without a CID).
const REJECT_PENDING_TTL: Duration = Duration::from_secs(30);

/// A ringing call from a blocked number, waiting for the phone's media hello.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RejectRing {
    caller: String,
    /// Declined while ringing (`reject_mode` `decline`): its record and
    /// notification are done. A hello still coming means the phone app could not
    /// decline and picked up: the caller gets the announcement.
    declined: bool,
    /// The record written when it was declined.
    record_id: Option<String>,
}

/// Phones whose ringing call is from a blocked number (serial → that call, when).
#[derive(Default)]
struct RejectPending {
    inner: std::sync::Mutex<HashMap<String, (RejectRing, tokio::time::Instant)>>,
}

impl RejectPending {
    /// Picked up for the announcement (`reject_mode` `announce`).
    fn mark(&self, serial: &str, caller: &str) {
        self.mark_ring(
            serial,
            RejectRing {
                caller: caller.to_string(),
                declined: false,
                record_id: None,
            },
        );
    }

    fn mark_ring(&self, serial: &str, ring: RejectRing) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(serial.to_string(), (ring, tokio::time::Instant::now()));
    }

    /// The phone's ringing call from a blocked number, if still fresh.
    fn take(&self, serial: &str) -> Option<RejectRing> {
        let (ring, at) = self.inner.lock().unwrap_or_else(|e| e.into_inner()).remove(serial)?;
        (at.elapsed() <= REJECT_PENDING_TTL).then_some(ring)
    }

    /// The media hello's share: the phone's ringing blocked call, only when the
    /// hello is that call — its number is the same, or not known yet. A left-over
    /// entry (its `incoming_cancel` lost) never turns the next caller away.
    fn take_for_hello(&self, serial: &str, hello_caller: &str) -> Option<RejectRing> {
        let ring = self.take(serial)?;
        if hello_caller.trim().is_empty() || same_ringing_caller(&ring.caller, hello_caller) {
            Some(ring)
        } else {
            info!(
                "拒接號碼 ring left over serial={serial} rang={} hello={} — not this call",
                urgent_call::masked(&ring.caller),
                urgent_call::masked(hello_caller)
            );
            None
        }
    }
}

/// `reject_mode` `decline` applies to a phone whose app said it can refuse a
/// ringing call (`supports_reject` in its control hello). Any other phone picks
/// up whatever the plan says, so its blocked callers get the announcement — and
/// the notification says so.
async fn blocked_number_declined(rt: &RuntimeSettings, control: &ControlRegistry, serial: &str) -> bool {
    rt.reject_declines() && control.supports_reject(serial).await
}

/// Isolated / secondary-user extra uid serial (`pixel-101NNNN` vs `pixel-1NNNN`).
/// 7+ digit `pixel-101…` — not 5-digit app uids like `pixel-10103`.
fn is_isolated_screening_serial(serial: &str) -> bool {
    let Some(rest) = serial.trim().strip_prefix("pixel-") else {
        return false;
    };
    rest.len() >= 7 && rest.starts_with("101") && rest.chars().all(|c| c.is_ascii_digit())
}

/// Map isolated extra uid back to the main app uid serial for exclusive play.
fn canonical_device_serial(serial: &str) -> String {
    let s = serial.trim();
    if is_isolated_screening_serial(s) {
        if let Some(rest) = s.strip_prefix("pixel-") {
            if let Some(app) = rest.get(2..) {
                return format!("pixel-{app}");
            }
        }
    }
    s.to_string()
}

/// Process-wide stats for "how long until AI replies after caller stops speaking".
#[derive(Default)]
struct TrtcTalkMetrics {
    /// Last measured reply latency (ms). 0 = none yet.
    last_reply_latency_ms: AtomicU64,
    /// Sum of latencies for average.
    sum_reply_latency_ms: AtomicU64,
    /// Number of measured turns.
    reply_count: AtomicU32,
    /// Last serial that reported a latency (best-effort, truncated).
    last_serial: std::sync::Mutex<String>,
}

impl TrtcTalkMetrics {
    fn record_reply(&self, serial: &str, latency_ms: u64) {
        self.last_reply_latency_ms
            .store(latency_ms, Ordering::Relaxed);
        self.sum_reply_latency_ms
            .fetch_add(latency_ms, Ordering::Relaxed);
        self.reply_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut g) = self.last_serial.lock() {
            *g = serial.to_string();
        }
        info!(
            "TrtcAi reply_latency_ms={latency_ms} serial={serial} (caller stop → AI audio start)"
        );
    }

    fn avg_ms(&self) -> u64 {
        let n = self.reply_count.load(Ordering::Relaxed);
        if n == 0 {
            return 0;
        }
        self.sum_reply_latency_ms.load(Ordering::Relaxed) / u64::from(n)
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Per-call turn tracker (shared between uplink WS loop and downlink pull).
#[derive(Default)]
struct TrtcTurnTracker {
    last_user_voice_ms: AtomicU64,
    user_turn_end_ms: AtomicU64,
    silence_after_voice_ms: AtomicU64,
    /// Avoid double-counting the same turn end.
    latched_end_ms: AtomicU64,
}

/// PCM s16le mono RMS (for barge-in / turn metrics).
fn pcm_rms_s16le(pcm: &[u8]) -> f32 {
    let n = (pcm.len() / 2) as f32;
    if n < 1.0 {
        return 0.0;
    }
    let mut sum = 0.0f64;
    for c in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([c[0], c[1]]) as f64;
        sum += s * s;
    }
    (sum / f64::from(n)).sqrt() as f32
}

impl AppState {
    fn ivr_prompt_pcm_bytes(&self) -> usize {
        self.ivr_prompt_segments.iter().map(|s| s.pcm.len()).sum()
    }

    fn ivr_prompt_loaded(&self) -> bool {
        !self.ivr_prompt_segments.is_empty()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // rustls 0.23: required for tokio-tungstenite WSS (Gemini Live).
    let _ = rustls::crypto::ring::default_provider().install_default();
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "agent=info".into()),
        )
        .init();
    log_text::announce();

    // `agent make-voice-samples [dir]`: record the bundled voice samples and exit.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("make-voice-samples") {
        let out = args.get(2).map(PathBuf::from).unwrap_or_else(|| config::assets_dir().join("voices"));
        return voice_preview::make_bundle(&out).await;
    }

    let settings = Settings::from_env();
    // Runtime overrides (web console) persisted inside the calls_data volume.
    let settings_store = SettingsStore::load(settings.clone());
    settings_store.snapshot().apply_globals();
    let hold_pcm = load_hold_music_pcm(
        &settings.transfer_hold_music,
        settings.sample_rate,
        settings.transfer_use_hold_music,
    )
    .map(Arc::new);
    let ivr_prompt_segments = Arc::new(
        load_ivr_segments(
            &parse_wav_list(&settings.ivr_prompt_wavs),
            settings.sample_rate,
        )
        .unwrap_or_default(),
    );
    let ivr_fail_pcm =
        load_fail_wav_pcm(&settings.ivr_fail_wav, settings.sample_rate).map(Arc::new);
    let ivr_goodbye_pcm =
        load_fail_wav_pcm(&settings.ivr_goodbye_wav, settings.sample_rate).map(Arc::new);
    let ivr_reject_unknown_pcm =
        load_fail_wav_pcm(&settings.ivr_reject_unknown_wav, settings.sample_rate).map(|pcm| {
            let (trim, ms) = trim_leading_silence_s16le(&pcm, settings.sample_rate, 80.0, 20);
            if ms > 0 {
                info!(
                    "IVR reject-unknown lead silence trimmed_ms={ms} pcm_bytes={}→{}",
                    pcm.len(),
                    trim.len()
                );
            }
            Arc::new(trim)
        });
    if ivr_reject_unknown_pcm.is_some() {
        info!(
            "IVR reject-unknown WAV loaded path={} pcm_bytes={}",
            settings.ivr_reject_unknown_wav,
            ivr_reject_unknown_pcm
                .as_ref()
                .map(|p| p.len())
                .unwrap_or(0)
        );
    } else {
        tracing::debug!(
            "no refusal announcement at {} (hidden numbers are answered by the AI, or hung up on without one)",
            settings.ivr_reject_unknown_wav
        );
    }
    let ivr_dtmf1_wait_pcm =
        load_fail_wav_pcm(&settings.ivr_dtmf1_wait_wav, settings.sample_rate).map(Arc::new);
    let ivr_ai_hello_pcm =
        load_fail_wav_pcm(&settings.ivr_ai_hello_wav, settings.sample_rate).map(Arc::new);
    let ivr_ai_nudge_pcm =
        load_fail_wav_pcm(&settings.ivr_ai_nudge_wav, settings.sample_rate).map(Arc::new);
    // Optional voicemail prompt WAVs; Piper text fallback when missing.
    let ivr_vm_intro_pcm =
        load_fail_wav_pcm(&settings.ivr_vm_intro_wav, settings.sample_rate).map(Arc::new);
    let ivr_vm_confirm_pcm =
        load_fail_wav_pcm(&settings.ivr_vm_confirm_wav, settings.sample_rate).map(Arc::new);
    // Preload period greets so answer → first PCM never blocks on disk decode.
    let mut menu_full: Vec<(String, Arc<Vec<u8>>)> = Vec::new();
    for name in [
        "ivr_menu_full_morning.wav",
        "ivr_menu_full_noon.wav",
        "ivr_menu_full_afternoon.wav",
        "ivr_menu_full_evening.wav",
        "ivr_menu_full_hello.wav",
    ] {
        let path = config::asset_path(name);
        if let Some(pcm) = load_fail_wav_pcm(&path, settings.sample_rate) {
            let (trim, ms) = trim_leading_silence_s16le(&pcm, settings.sample_rate, 80.0, 20);
            if ms > 0 {
                info!(
                    "IVR period greet preloaded name={name} trimmed_lead_ms={ms} pcm_bytes={}→{}",
                    pcm.len(),
                    trim.len()
                );
            } else {
                info!(
                    "IVR period greet preloaded name={name} pcm_bytes={}",
                    trim.len()
                );
            }
            menu_full.push((name.to_string(), Arc::new(trim)));
        }
    }
    let ivr_menu_full_pcm = Arc::new(menu_full);
    let vm_beep_pcm = Arc::new(make_beep_pcm(settings.sample_rate, 1000.0, 700, 0.32));
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        // Keys go in request headers (Gemini's x-goog-api-key, Pushbullet's
        // Access-Token…): a redirect to another host must not take them along.
        .redirect(calls_http::same_host_redirects())
        .build()?;
    let calls = CallStore::new(
        PathBuf::from(&settings.calls_data_dir),
        settings.calls_record,
        settings.calls_record_tts,
    );
    let task_store = TaskStore::new(PathBuf::from(&settings.calls_data_dir).join("tasks"));
    // The key phones use over Wi-Fi, kept next to the call records (phone_wifi).
    phone_wifi::init(std::path::Path::new(&settings.calls_data_dir));
    // The owner's contacts (names for caller IDs), next to the call records.
    let contacts = contacts::ContactStore::open(PathBuf::from(&settings.calls_data_dir).join("contacts.json"));
    contacts::install(contacts.clone());
    // Blocked numbers older versions kept in the settings join the Blacklist.
    contacts::absorb_reject_numbers(&settings_store, &contacts).await;
    // The scam database's owner file and number book live next to the settings.
    scam_db::init(&PathBuf::from(&settings.calls_data_dir));
    // Calls the owner has the AI make (errands), and the SIMs each phone has
    // reported (an errand dials from a SIM of the number's country).
    let phone_sims = errands::SimBook::open(PathBuf::from(&settings.calls_data_dir).join("phone_sims.json"));
    errands::install_sims(phone_sims.clone());
    let errand_store = errands::ErrandStore::open(PathBuf::from(&settings.calls_data_dir).join("errands.json"), phone_sims.clone());
    // Text messages the SMS forwarder hands over, for the console's SMS page.
    let sms_store = sms_store::SmsStore::open(
        PathBuf::from(&settings.calls_data_dir).join("sms"),
        sms_store::now_ms(),
        settings_store.snapshot().retention_days,
    );
    {
        let calls = calls.clone();
        let task_store = task_store.clone();
        let errands = errand_store.clone();
        let sms = sms_store.clone();
        let settings_store = settings_store.clone();
        tokio::spawn(async move {
            // Every hour (the first right away), with the days the settings say now.
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                ticker.tick().await;
                let days = settings_store.snapshot().retention_days;
                calls_http::purge_expired_records(&calls, &task_store, &errands, &sms, days).await;
            }
        });
    }
    let control = ControlRegistry::new();
    control.set_sim_book(phone_sims);
    let phones = calls_http::PhoneDevices::new();
    let state = AppState {
        settings: settings.clone(),
        settings_store: settings_store.clone(),
        http: http.clone(),
        hold_pcm,
        ivr_prompt_segments,
        ivr_fail_pcm,
        ivr_goodbye_pcm,
        ivr_reject_unknown_pcm,
        ivr_dtmf1_wait_pcm,
        ivr_ai_hello_pcm,
        ivr_ai_nudge_pcm,
        ivr_vm_intro_pcm,
        ivr_vm_confirm_pcm,
        ivr_menu_full_pcm,
        vm_beep_pcm,
        latency_build: env!("CARGO_PKG_VERSION"),
        calls: calls.clone(),
        task_store: task_store.clone(),
        control: control.clone(),
        phones: phones.clone(),
        trtc_talk: Arc::new(TrtcTalkMetrics::default()),
        incoming_notify_dedup: Arc::new(IncomingNotifyDedup::default()),
        call_reattach: Arc::new(CallReattachRegistry::default()),
        media_play: Arc::new(MediaPlayGate::default()),
        ring_prewarm: Arc::new(RingPrewarmRegistry::default()),
        reject_pending: Arc::new(RejectPending::default()),
        previews: Arc::new(voice_preview::VoicePreviews::new(
            calls.root().join("voice_preview"),
            config::assets_dir().join("voices"),
        )),
        ambience: Arc::new(ambience::AmbienceLibrary::load(
            &std::env::var("AMBIENCE_DIR")
                .ok()
                .filter(|d| !d.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(config::assets_dir),
            settings.sample_rate,
        )),
        errands: errand_store.clone(),
    };
    // Blocked numbers: have the announcement in the chosen voice ready before the first
    // such call (made once, then cached on disk). The network can be down for a
    // moment right after a restart, so wait and retry.
    {
        let state = state.clone();
        tokio::spawn(async move {
            for wait_s in [10u64, 60, 600] {
                tokio::time::sleep(Duration::from_secs(wait_s)).await;
                if prepare_reject_clip(&state).await {
                    break;
                }
            }
        });
    }
    // Appointment call-backs a phone never reported on (the app drops a dial
    // that does not start without saying so): checked every 15 s.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tick.tick().await;
                appointment_callbacks(&state, "", None).await;
                errand_tick(&state).await;
            }
        });
    }

    if settings.agent_mode == AgentMode::Llm
        && settings.llm_provider == LlmProvider::OpenRouter
        && settings.openrouter_api_key.trim().is_empty()
    {
        error!(
            "LLM_PROVIDER=openrouter but OPENROUTER_API_KEY is empty; /v1/turn and chat will fail until the key is set"
        );
    }

    {
        let http = http.clone();
        let settings = settings.clone();
        tokio::spawn(async move {
            ensure_ollama_model(&http, &settings).await;
        });
    }

    let hold_pcm_bytes = state.hold_pcm.as_ref().map(|p| p.len()).unwrap_or(0);
    let ivr_prompt_pcm_bytes = state.ivr_prompt_pcm_bytes();
    let ivr_seg_labels: Vec<String> = state
        .ivr_prompt_segments
        .iter()
        .map(|s| s.label.clone())
        .collect();
    let ivr_fail_pcm_bytes = state.ivr_fail_pcm.as_ref().map(|p| p.len()).unwrap_or(0);
    let ivr_goodbye_pcm_bytes = state.ivr_goodbye_pcm.as_ref().map(|p| p.len()).unwrap_or(0);
    let ivr_reject_unknown_pcm_bytes = state
        .ivr_reject_unknown_pcm
        .as_ref()
        .map(|p| p.len())
        .unwrap_or(0);
    let calls_state = CallsState {
        settings: settings.clone(),
        previews: state.previews.clone(),
        store: calls,
        settings_store: settings_store.clone(),
        task_store: task_store.clone(),
        http: http.clone(),
        control: control.clone(),
        ambience: state.ambience.clone(),
        phones: phones.clone(),
        contacts: contacts.clone(),
        errands: errand_store.clone(),
        sms: sms_store,
    };
    // Automatic backups (backup_schedule), checked every 10 minutes.
    backup::spawn(calls_state.clone());
    let addr: SocketAddr = format!("{}:{}", settings.host, settings.port).parse()?;
    let mode = settings.agent_mode.as_str();
    let llm_desc = match settings.llm_provider {
        LlmProvider::Ollama => format!(
            "ollama={} model={}",
            settings.ollama_url, settings.ollama_model
        ),
        LlmProvider::OpenRouter => format!(
            "openrouter={} model={}",
            settings.openrouter_base_url, settings.openrouter_model
        ),
    };
    // AGENT_HOST not set: every address, and the console's network name.
    let listening = if settings.host_auto {
        let name = mdns::parse_name_setting(&settings_store.snapshot().mdns_name);
        let lan = if name.is_empty() { String::new() } else { format!(", on this network http://{name}.local:{}/", settings.port) };
        format!("0.0.0.0:{} (this computer http://localhost:{}/{lan})", settings.port, settings.port)
    } else {
        addr.to_string()
    };
    info!(
        "pixel-agent listening on http://{} (ws /ws, ui /calls/) mode={} ivr_prompt={:?} ivr_start_delay_ms={} ivr_segments={:?} ivr_prompt_pcm_bytes={} ivr_fail_pcm_bytes={} ivr_goodbye_pcm_bytes={} ivr_reject_unknown_pcm_bytes={} dtmf_hold={} dtmf_vm={} dtmf_end={:?} ivr_loop={} max_menu_plays={} reprompt_ms={} wait_remote_voice={} hold_music={} hold_pcm_bytes={} calls_dir={} calls_record={} llm={} whisper={} piper={}",
        listening,
        mode,
        settings.ivr_prompt_text,
        settings.ivr_start_delay_ms,
        ivr_seg_labels,
        ivr_prompt_pcm_bytes,
        ivr_fail_pcm_bytes,
        ivr_goodbye_pcm_bytes,
        ivr_reject_unknown_pcm_bytes,
        settings.transfer_dtmf_trigger,
        settings.ivr_dtmf_voicemail,
        settings.ivr_dtmf_end,
        settings.ivr_loop,
        settings.ivr_max_menu_plays,
        settings.ivr_reprompt_ms,
        settings.transfer_wait_for_remote_voice,
        settings.transfer_hold_music,
        hold_pcm_bytes,
        settings.calls_data_dir,
        settings.calls_record,
        llm_desc,
        settings.whisper_url,
        settings.piper_url
    );
    info!(
        "voicemail: intro_wav_loaded={} confirm_wav_loaded={} beep_bytes={} max_ms={} silence_ms={} silence_rms={} confirm_wait_ms={}",
        state.ivr_vm_intro_pcm.is_some(),
        state.ivr_vm_confirm_pcm.is_some(),
        state.vm_beep_pcm.len(),
        settings.vm_max_ms,
        settings.vm_silence_ms,
        settings.vm_silence_rms,
        settings.vm_confirm_wait_ms
    );
    {
        let rt = settings_store.snapshot();
        info!(
            "runtime settings: path={} ai_live={} shaming={} call_notify={} (spam deny/blocklist gating removed)",
            settings_store.storage_path().display(),
            rt.ai_live_enabled,
            rt.shaming_enabled,
            rt.call_notify
        );
    }

    // Chat commands (crate::control): the chats that may operate the agent;
    // the AI's questions during an errand call go there too.
    let remote = control::Controller::new(calls_state.clone());
    errand_store.set_question_hook(remote.question_hook());
    // Telegram: every bot that serves a chat is long-polled
    // (control::telegram). The to-do card buttons of the environment's bot
    // come here and are handled as before. No spam-blocklist dependency.
    let (card_tx, mut card_rx) = tokio::sync::mpsc::unbounded_channel::<tg_inbound::TgAction>();
    control::telegram::spawn(remote.clone(), settings_store.clone(), http.clone(), card_tx);
    // Discord, Slack and Matrix: each channel switched on with "take commands"
    // connects (control::discord / slack / matrix); settings changes apply
    // within seconds, as for Telegram.
    control::discord::spawn(remote.clone(), settings_store.clone(), http.clone());
    control::slack::spawn(remote.clone(), settings_store.clone(), http.clone());
    control::matrix::spawn(remote.clone(), settings_store.clone(), http.clone());
    {
        let settings_store = settings_store.clone();
        let task_store = task_store.clone();
        let http = http.clone();
        let control = control.clone();
        let calls = state.calls.clone();
        let base = state.settings.clone();
        tokio::spawn(async move {
            while let Some(first) = card_rx.recv().await {
                let token = settings_store.snapshot().telegram_bot_token.trim().to_string();
                // Presses that came in one poll are handled in turn, as before.
                let mut actions = vec![first];
                while let Ok(more) = card_rx.try_recv() {
                    actions.push(more);
                }
                let mut pending_replies: Vec<(String, String, String)> = Vec::new();
                for action in actions {
                    let rt = settings_store.snapshot();
                    // Appointments: ✅ / ❌ on their card. On its own task — a text
                    // message to the caller may wait up to a minute and a half for the phone.
                    let is_appointment = task_store
                        .get(&action.task_id)
                        .await
                        .is_some_and(|t| t.appointment.is_some());
                    if is_appointment {
                        let (http, task_store, calls, control, token) = (
                            http.clone(),
                            task_store.clone(),
                            calls.clone(),
                            control.clone(),
                            token.clone(),
                        );
                        // The call's page for "Add to calendar" while no calendar is chosen.
                        let console = settings_store::effective_console_url(&rt.effective(&base));
                        tokio::spawn(async move {
                            let text = match appointments::decide(
                                &rt,
                                &console,
                                &task_store,
                                &calls,
                                Some(control.as_ref()),
                                &action.task_id,
                                action.confirm,
                            )
                            .await
                            {
                                Ok((_, text)) => text,
                                Err(e) => {
                                    warn!(
                                        "tg appointment decision failed task_id={}: {e:#}",
                                        action.task_id
                                    );
                                    let p = locale::notify_phrases();
                                    let head = if action.confirm {
                                        p.tc_confirm_failed
                                    } else {
                                        p.tc_ignore_failed
                                    };
                                    format!("{head}{}{e:#}", locale::notify_kv_sep())
                                }
                            };
                            if let Err(e) =
                                tg_inbound::send_plain_message(&http, &token, &action.chat_id, &text).await
                            {
                                warn!(
                                    "tg appointment result notify failed task_id={}: {e:#}",
                                    action.task_id
                                );
                            }
                        });
                        continue;
                    }
                    let result_text = if action.confirm {
                        match fulfill::on_confirm(
                            &http,
                            &rt,
                            &task_store,
                            &action.task_id,
                            Some(control.as_ref()),
                        )
                        .await
                        {
                            Ok(task) => fulfill::tg_result_text(&task, true),
                            Err(e) => {
                                warn!(
                                    "tg confirm fulfill failed task_id={}: {e:#}",
                                    action.task_id
                                );
                                format!(
                                    "{}{}{e:#}",
                                    locale::notify_phrases().tc_confirm_failed,
                                    locale::notify_kv_sep()
                                )
                            }
                        }
                    } else {
                        match fulfill::on_ignore(&task_store, &action.task_id).await {
                            Ok(task) => fulfill::tg_result_text(&task, false),
                            Err(e) => {
                                warn!("tg ignore fulfill failed task_id={}: {e:#}", action.task_id);
                                format!(
                                    "{}{}{e:#}",
                                    locale::notify_phrases().tc_ignore_failed,
                                    locale::notify_kv_sep()
                                )
                            }
                        }
                    };
                    pending_replies.push((action.chat_id, result_text, action.task_id));
                }
                if !pending_replies.is_empty() {
                    let http = http.clone();
                    tokio::spawn(async move {
                        let futs = pending_replies.into_iter().map(|(chat_id, text, task_id)| {
                            let http = http.clone();
                            let token = token.clone();
                            async move {
                                if let Err(e) =
                                    tg_inbound::send_plain_message(&http, &token, &chat_id, &text).await
                                {
                                    warn!("tg fulfill result notify failed task_id={task_id}: {e:#}");
                                }
                            }
                        });
                        futures_util::future::join_all(futs).await;
                    });
                }
            }
        });
    }

    // calls_router is Router<()> (own CallsState); nest_service avoids AppState mismatch.
    // Root /index land on the calls UI so LAN operators do not need to remember /calls/.
    // The phones' side: keep `adb reverse` up and the app's call service running.
    {
        let control = control.clone();
        let calls = state.calls.clone();
        usb_bridge::spawn(usb_bridge::Config::from_env(settings.port), move || {
            let control = control.clone();
            let calls = calls.clone();
            async move {
                (
                    control.connected_serials().await.len(),
                    calls.live_calls().await.len(),
                )
            }
        });
    }
    if !addr.ip().is_loopback() {
        if settings_store::active_ui_token(&settings.calls_ui_token).is_empty() {
            warn!("listening on {addr} without CALLS_UI_TOKEN: anyone on this network can open the web console");
        }
    }
    // Links to an `.ics` that leave the console (notifications, Telegram) carry
    // a key made from the console token, never the token itself.
    calendar::set_link_secret(&settings_store::active_ui_token(&settings.calls_ui_token));
    if settings.host_auto {
        // AGENT_HOST not set: the console on this network (crate::lan). Other
        // devices get in once a console password is set.
        if settings_store::active_ui_token(&settings.calls_ui_token).is_empty() {
            info!("web console open on this network: other devices can use it once a console password is set");
        }
        // `http://bageo.local/`: the network name answers for the console.
        let store = settings_store.clone();
        mdns::spawn(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), settings.port, move || {
            store.snapshot().mdns_name.clone()
        });
    } else if !addr.ip().is_loopback() {
        if settings.device_token.trim().is_empty() {
            warn!("listening on {addr} without DEVICE_TOKEN: anyone on this network can connect as a phone");
        }
        // `http://bageo.local:8787/`: answer the console's network name (setting
        // `mdns_name`, re-read while running; empty = off). Not in a container:
        // Docker's bridge network keeps the answers inside Docker.
        if settings_store::in_container() {
            info!("mDNS: not started inside a container (see docker-compose.yml)");
        } else {
            let store = settings_store.clone();
            mdns::spawn(addr.ip(), addr.port(), move || store.snapshot().mdns_name.clone());
        }
    } else if !settings_store::in_container() {
        // Listening on this computer only: the network name answers while
        // phones may connect over Wi-Fi, so `bageo.local` reaches the Wi-Fi entry.
        let store = settings_store.clone();
        mdns::spawn(addr.ip(), settings.phone_wifi_port, move || {
            let rt = store.snapshot();
            if rt.phone_wifi {
                rt.mdns_name.clone()
            } else {
                String::new()
            }
        });
    }
    spawn_phone_wifi(state.clone(), addr);

    let auto = settings.host_auto;
    let guard_settings = settings.clone();
    let guard_store = state.settings_store.clone();
    let app = build_app(state, calls_state, remote);
    // Automatic: every address, behind the password check for other devices.
    let (app, listen) = if auto {
        (
            lan::guard(app, guard_settings, guard_store),
            SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), addr.port()),
        )
    } else {
        (app, addr)
    };
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| anyhow::anyhow!("cannot listen on {listen}: {e} (is another copy running? AGENT_PORT picks another port)"))?;
    if auto {
        lan::set_auto(true);
        open_port_80(&app, addr.port()).await;
        let eff = settings_store.snapshot().effective(&settings);
        let on_lan = settings_store::console_url_for(&eff, false, true, lan::port_80(), true, mdns::answer_ip);
        info!("web console: http://localhost:{}/ on this computer, {on_lan} on this network (listening on {listen})", listen.port());
    } else {
        info!("web console: http://{}:{}/", if listen.ip().is_unspecified() { "localhost".to_string() } else { listen.ip().to_string() }, listen.port());
    }
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<lan::ConnAddrs>(),
    )
    .await?;
    Ok(())
}

/// Automatic mode: the console at port 80 too, so `http://bageo.local/` opens
/// it; `CONSOLE_PORT_80=off` keeps the port closed. Port 80 in use or not
/// allowed: one line in the log, and the console stays at its own port.
async fn open_port_80(app: &Router, port: u16) {
    if port == 80 || !lan::port_80_wanted(std::env::var("CONSOLE_PORT_80").ok().as_deref()) {
        return;
    }
    match tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 80)).await {
        Ok(listener) => {
            lan::set_port_80(true);
            info!("web console also at port 80");
            let app = app.clone();
            tokio::spawn(async move {
                if let Err(e) = axum::serve(listener, app.into_make_service_with_connect_info::<lan::ConnAddrs>()).await {
                    warn!("web console at port 80 stopped ({e})");
                }
                lan::set_port_80(false);
            });
        }
        Err(e) => info!("web console not at port 80 ({e}); it stays at port {port}"),
    }
}

/// The agent's HTTP routes: the phone and test routes with the agent's state,
/// and the web console (`/calls`) with its own.
fn build_app(state: AppState, calls_state: CallsState, remote: Arc<control::Controller>) -> Router {
    let settings = state.settings.clone();
    let app = Router::new()
        .route("/", get(root))
        .route("/index", get(redirect_to_calls))
        .route("/favicon.ico", get(calls_http::favicon))
        .route("/health", get(health))
        // The console's copy of the full health JSON (it cannot read `/health`
        // from another computer, or through a proxy, when no password is set).
        .route("/calls/api/health", get(console_health))
        .route("/v1/turn", post(turn))
        .route("/v1/transfer", get(transfer_smoke).post(transfer_smoke))
        .route("/ws", get(ws_upgrade))
        // A phone trades the console's pairing code for the pairing key (no sign-in).
        .route(PAIRING_CLAIM_PATH, post(phone_pairing_claim))
        // MCP tool interface (crate::mcp): off and silent until enabled with a token.
        .merge(mcp::router(remote))
        .nest_service("/calls", calls_router(calls_state))
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::map_response(add_source_header))
        // Served with `lan::ConnAddrs`: the handlers still read the peer as a `SocketAddr`.
        .layer(axum::middleware::from_fn(lan::copy_peer))
        .with_state(state);
    // Wrong passwords from other devices: slower, then a minute's wait.
    lan::throttle(app, settings)
}

/// Authorship marker that stays in the compiled program (LICENSING.md, Section 7(b)).
#[used]
pub static BAGEO_SOURCE: &str = "bageo-call by h4lo-void https://github.com/h4lo-void/bageo-call";

/// Adds `X-Bageo-Source` to every HTTP response the agent sends.
async fn add_source_header(mut res: axum::response::Response) -> axum::response::Response {
    res.headers_mut().insert(
        "x-bageo-source",
        axum::http::HeaderValue::from_static("h4lo-void/bageo-call"),
    );
    res
}

/// `/`: a phone over the USB tunnel opens its WebSocket here (the app's default
/// address has no path); a browser is sent to the web console.
async fn root(
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> axum::response::Response {
    match ws {
        Ok(ws) => phone_ws(ws, peer, &headers, &q, state),
        Err(_) => Redirect::permanent("/calls/").into_response(),
    }
}

/// A phone may connect when it comes through this machine (the USB tunnel ends
/// on loopback), or carries `?token=` matching `DEVICE_TOKEN`. Without a token
/// set, anyone who can reach the port directly may connect (the phone bridges
/// on the LAN connect so). A connection through a reverse proxy on this
/// machine (`proxied`: it carries a forwarding header) is not this machine's
/// own even from loopback: it needs `DEVICE_TOKEN`, and without one set it is
/// refused. The pairing key ([`phone_wifi`]) is checked apart, in [`phone_ws`].
fn phone_allowed(peer: &SocketAddr, proxied: bool, token: Option<&str>, required: &str) -> bool {
    let required = required.trim();
    let token_ok = !required.is_empty() && token.is_some_and(|t| crate::mcp::same(t, required));
    if proxied {
        return token_ok;
    }
    peer.ip().is_loopback() || required.is_empty() || token_ok
}

fn phone_ws(
    ws: WebSocketUpgrade,
    peer: SocketAddr,
    headers: &axum::http::HeaderMap,
    q: &HashMap<String, String>,
    state: AppState,
) -> axum::response::Response {
    let proxied = calls_http::came_through_proxy(headers);
    let token = q.get("token").map(String::as_str);
    // A phone paired over USB that reaches this port over Wi-Fi (the agent is
    // open to the LAN): its pairing key counts while phone Wi-Fi is on.
    let paired = state.settings_store.snapshot().phone_wifi && phone_wifi::pairing().accepts(token);
    if !paired && !phone_allowed(&peer, proxied, token, &state.settings.device_token) {
        warn!("phone websocket refused from {peer} (through a proxy: {proxied}): missing or wrong token");
        return (StatusCode::UNAUTHORIZED, "device token required").into_response();
    }
    let device_token = state.settings.device_token.trim();
    let link = PhoneLink::main_port(
        peer.ip().is_loopback(),
        proxied,
        paired,
        !device_token.is_empty() && token.is_some_and(|t| phone_wifi::same_token(t.trim(), device_token)),
    );
    ws.on_upgrade(move |socket| handle_ws(socket, state, link))
}

/// How a phone's WebSocket came in.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct PhoneLink {
    /// Over the network (shown as Wi-Fi), not the USB tunnel that ends on loopback.
    wifi: bool,
    /// Through a reverse proxy on this machine.
    proxied: bool,
    /// May be given the Wi-Fi addresses with the pairing key: the USB tunnel,
    /// or a connection that showed the pairing key or `DEVICE_TOKEN`.
    trusted: bool,
    /// Dropped when the phones are paired again or phone Wi-Fi is turned off.
    on_pairing: bool,
}

impl PhoneLink {
    /// A connection on the main port. `paired`: it showed the pairing key;
    /// `device_token`: it showed `DEVICE_TOKEN`.
    fn main_port(loopback: bool, proxied: bool, paired: bool, device_token: bool) -> Self {
        let usb = loopback && !proxied;
        Self {
            wifi: !usb,
            proxied,
            trusted: usb || paired || device_token,
            on_pairing: paired,
        }
    }

    /// A connection on the Wi-Fi entry (it showed the pairing key or `DEVICE_TOKEN`).
    fn wifi_entry(proxied: bool, on_pairing: bool) -> Self {
        Self {
            wifi: true,
            proxied,
            trusted: true,
            on_pairing,
        }
    }
}

/// What a phone connection reads: the WebSocket, ending early when a phone
/// on Wi-Fi must be dropped ([`until_dropped`]).
type PhoneStream = futures_util::stream::TakeUntil<
    futures_util::stream::SplitStream<WebSocket>,
    std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
>;

/// Finishes when the phones are paired again or phone Wi-Fi is turned off,
/// for a connection made over Wi-Fi with the pairing; never for the others.
fn until_dropped(link: PhoneLink) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    if !link.on_pairing {
        return Box::pin(std::future::pending());
    }
    let mut kicked = phone_wifi::pairing().kicked();
    Box::pin(async move {
        if kicked.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
        info!("phone on Wi-Fi disconnected: paired again or phone Wi-Fi turned off");
    })
}

/// Where phones reach this agent over Wi-Fi: the address to announce from and
/// the port. None while phone Wi-Fi is off, in a container (its addresses are
/// Docker's), or while the Wi-Fi entry is not running.
fn wifi_entry(base: &Settings, rt: &RuntimeSettings) -> Option<(std::net::IpAddr, u16)> {
    wifi_entry_for(
        rt.phone_wifi,
        settings_store::in_container(),
        base.host.trim().parse().ok(),
        base.port,
        phone_wifi::entry_port(),
    )
}

/// [`wifi_entry`]: an agent open to the LAN (`AGENT_HOST` not loopback) takes
/// phones on its own port; one on this computer only, on the Wi-Fi entry.
fn wifi_entry_for(
    on: bool,
    in_container: bool,
    host: Option<std::net::IpAddr>,
    port: u16,
    entry_port: Option<u16>,
) -> Option<(std::net::IpAddr, u16)> {
    if !on || in_container {
        return None;
    }
    let host = host?;
    if !host.is_loopback() {
        return Some((host, port));
    }
    entry_port.map(|p| (host, p))
}

/// The `wifi.urls` of `agent_info` for the Wi-Fi `entry`; `lan_ip` finds the
/// address to give for the address listened on ([`mdns::answer_ip`]).
fn agent_info_wifi_urls(
    entry: Option<(std::net::IpAddr, u16)>,
    lan_ip: impl FnOnce(std::net::IpAddr) -> Option<std::net::Ipv4Addr>,
    mdns_host: &str,
    key: impl FnOnce() -> String,
) -> Vec<String> {
    match entry {
        Some((listen, port)) => match lan_ip(listen) {
            Some(ip) => phone_wifi::wifi_urls(Some(ip), port, mdns_host, &key()),
            None => Vec::new(),
        },
        None => Vec::new(),
    }
}

/// The value of `?token=` in a query string.
fn token_param(query: Option<&str>) -> Option<String> {
    url::form_urlencoded::parse(query?.as_bytes())
        .find(|(k, _)| k == "token")
        .map(|(_, v)| v.into_owned())
}

/// The Wi-Fi entry lets in only a phone with the pairing key or
/// `DEVICE_TOKEN` (`device_token`): no exception for loopback or an unset token.
fn wifi_entry_gate(router: Router, device_token: String) -> Router {
    router.layer(axum::middleware::from_fn(
        move |req: axum::extract::Request, next: axum::middleware::Next| {
            let device_token = device_token.clone();
            async move {
                let token = token_param(req.uri().query());
                if !phone_wifi::entry_allows(token.as_deref(), &device_token, phone_wifi::pairing()) {
                    let peer = req
                        .extensions()
                        .get::<ConnectInfo<SocketAddr>>()
                        .map(|c| c.0.to_string())
                        .unwrap_or_default();
                    warn!("phone refused on the Wi-Fi entry from {peer}: missing or wrong pairing key");
                    return (StatusCode::UNAUTHORIZED, "pairing key required").into_response();
                }
                next.run(req).await
            }
        },
    ))
}

/// The Wi-Fi entry's only page: `/ws` for phones.
async fn wifi_entry_ws(
    ws: WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    RawQuery(query): RawQuery,
    State(state): State<AppState>,
) -> axum::response::Response {
    let token = token_param(query.as_deref());
    let device_token = state.settings.device_token.trim();
    let device_ok = !device_token.is_empty()
        && token.as_deref().is_some_and(|t| phone_wifi::same_token(t.trim(), device_token));
    let link = PhoneLink::wifi_entry(calls_http::came_through_proxy(&headers), !device_ok);
    ws.on_upgrade(move |socket| handle_ws(socket, state, link))
}

/// Where a phone trades a pairing code for the pairing key.
const PAIRING_CLAIM_PATH: &str = "/api/phone-pairing/claim";

/// `POST /api/phone-pairing/claim` `{"code": "123456", "name": "..."}`: a phone
/// paired by IP address. The code the console made ([`phone_wifi::PairCodes`])
/// gives the pairing key once; the phone then opens `/ws?token=<key>` on the
/// address it typed. No sign-in: the code is the proof. On the main port and
/// on the Wi-Fi entry.
async fn phone_pairing_claim(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let on = state.settings_store.snapshot().phone_wifi;
    let res = claim_answer(on, &headers, &body, phone_wifi::codes(), phone_wifi::pairing(), phone_wifi::entry_port());
    if res.status().is_success() {
        info!("phone pairing: a phone at {} paired with a pairing code", peer.ip());
    } else {
        warn!("phone pairing: refused a pairing code from {} ({})", peer.ip(), res.status().as_u16());
    }
    res
}

/// [`phone_pairing_claim`]'s answer. Errors carry a `code`: `bad_request` (no
/// JSON or no code), `phone_wifi_off`, `no_code` (none in use: expired, used
/// or voided), `wrong_code`, `too_many_tries`.
fn claim_answer(
    phone_wifi_on: bool,
    headers: &axum::http::HeaderMap,
    body: &[u8],
    codes: &phone_wifi::PairCodes,
    pairing: &phone_wifi::Pairing,
    entry_port: Option<u16>,
) -> axum::response::Response {
    let json_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(';').next().unwrap_or("").trim().eq_ignore_ascii_case("application/json"));
    let code = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("code").and_then(Value::as_str).map(|c| c.trim().to_string()))
        .filter(|c| !c.is_empty());
    let Some(code) = code.filter(|_| json_type) else {
        return calls_http::api_error(StatusCode::BAD_REQUEST, "bad_request", "send JSON with the pairing code: {\"code\": \"123456\"}");
    };
    if !phone_wifi_on {
        return calls_http::api_error(StatusCode::CONFLICT, "phone_wifi_off", "phones over Wi-Fi are turned off");
    }
    match codes.claim(&code, std::time::Instant::now()) {
        Ok(()) => {
            let mut body = json!({"ok": true, "token": pairing.key(), "path": "/ws"});
            if let Some(port) = entry_port {
                body["ws_port"] = json!(port);
            }
            Json(body).into_response()
        }
        Err(e) => {
            let (status, words) = match e {
                phone_wifi::ClaimError::TooMany => (StatusCode::TOO_MANY_REQUESTS, "too many wrong pairing codes; try again in a minute"),
                phone_wifi::ClaimError::NoCode => (StatusCode::FORBIDDEN, "no pairing code in use; make a new one in the console"),
                phone_wifi::ClaimError::Wrong => (StatusCode::FORBIDDEN, "wrong pairing code"),
            };
            calls_http::api_error(status, e.code(), words)
        }
    }
}

/// Phone Wi-Fi (setting `phone_wifi`, re-read every 2 s): while the agent
/// listens on this computer only, a second listener on all addresses at
/// `PHONE_WIFI_PORT` that serves only `/ws` ([`wifi_entry_gate`]). Turning the
/// setting off closes it and drops the phones on Wi-Fi. Not in a container.
fn spawn_phone_wifi(state: AppState, main_addr: SocketAddr) {
    if settings_store::in_container() {
        info!("phone Wi-Fi: no Wi-Fi entry inside a container (see docker-compose.yml)");
        return;
    }
    let port = state.settings.phone_wifi_port;
    let own_entry = main_addr.ip().is_loopback() && port != 0 && port != main_addr.port();
    tokio::spawn(async move {
        let mut running: Option<(tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>)> = None;
        let mut was_on = state.settings_store.snapshot().phone_wifi;
        let mut retry_at: Option<tokio::time::Instant> = None;
        loop {
            let prev_port = phone_wifi::entry_port();
            let on = state.settings_store.snapshot().phone_wifi;
            let switched = was_on != on;
            if was_on && !on {
                info!("phone Wi-Fi: turned off");
                phone_wifi::pairing().kick();
            }
            was_on = on;
            if running.as_ref().is_some_and(|(_, task)| task.is_finished()) {
                running = None;
                phone_wifi::set_entry_port(0);
            }
            if own_entry && on && running.is_none() && retry_at.is_none_or(|t| tokio::time::Instant::now() >= t) {
                match tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await {
                    Ok(listener) => {
                        let app = wifi_entry_gate(
                            Router::new().route("/ws", get(wifi_entry_ws)).with_state(state.clone()),
                            state.settings.device_token.clone(),
                        )
                        // Pairing codes need no key (they are how a phone gets one).
                        .merge(Router::new().route(PAIRING_CLAIM_PATH, post(phone_pairing_claim)).with_state(state.clone()));
                        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
                        let task = tokio::spawn(async move {
                            let served = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
                                .with_graceful_shutdown(async {
                                    let _ = stopped.await;
                                })
                                .await;
                            if let Err(e) = served {
                                warn!("phone Wi-Fi: the Wi-Fi entry stopped ({e})");
                            }
                        });
                        phone_wifi::set_entry_port(port);
                        retry_at = None;
                        running = Some((stop, task));
                        info!("phone Wi-Fi: phones paired over USB can connect on port {port}");
                    }
                    Err(e) => {
                        warn!("phone Wi-Fi: cannot listen on port {port} ({e}); PHONE_WIFI_PORT picks another port. Trying again in 60 s");
                        retry_at = Some(tokio::time::Instant::now() + Duration::from_secs(60));
                    }
                }
            } else if !on {
                retry_at = None;
                if let Some((stop, _)) = running.take() {
                    let _ = stop.send(());
                    phone_wifi::set_entry_port(0);
                    info!("phone Wi-Fi: Wi-Fi entry closed");
                }
            }
            if switched || phone_wifi::entry_port() != prev_port {
                phone_wifi::pairing().notify_refresh();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// Permanent redirect to the web console.
async fn redirect_to_calls() -> Redirect {
    Redirect::permanent("/calls/")
}

/// How long records are kept, for /health: retention_days, and the older
/// name calls_retention_days (same value) that other tools still read.
fn health_retention(rt: &settings_store::RuntimeSettings) -> [(&'static str, Value); 2] {
    [("retention_days", json!(rt.retention_days)), ("calls_retention_days", json!(rt.retention_days))]
}

/// What the console's "Ready?" list still asks for, for /health `setup_missing`.
/// Bageo Status shows yellow while this is not empty. Phones are not here:
/// /health already counts the connected ones.
fn setup_missing(rt: &RuntimeSettings, eff: &Settings) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !rt.gemini_live_enabled {
        out.push("ai_off");
    }
    if rt.gemini_api_key.trim().is_empty() {
        out.push("gemini_key");
    }
    if rt.owner_name.trim().is_empty() {
        out.push("owner_name");
    }
    if !notify_configured(eff) {
        out.push("notify");
    }
    if summary::summary_endpoint(eff).is_none() {
        out.push("summary");
    }
    out
}

/// Who gets the full `/health` JSON (phone serials, caller numbers of calls in
/// progress, folder paths, prompts): with a console password set, a request that
/// carries it; with none set, only a request from this machine itself, which
/// did not come through a reverse proxy. Everyone else gets the short version.
fn health_full_allowed(password_set: bool, token_ok: bool, peer_loopback: bool, proxied: bool) -> bool {
    if password_set {
        token_ok
    } else {
        peer_loopback && !proxied
    }
}

async fn health(
    State(state): State<AppState>,
    ext: axum::http::Extensions,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    // Anyone else only learns that the agent is up (enough for monitors and the
    // SMS forwarder). The web console reads the full version from
    // `GET /calls/api/health`, which has the console's own password check.
    let password_set = !settings_store::active_ui_token(&state.settings.calls_ui_token).is_empty();
    let token_ok = calls_http::token_ok_parts(&state.settings, &headers, q.get("token").map(String::as_str));
    // Loopback, or this computer's own LAN address (`http://bageo.local/health` here).
    let peer_loopback = lan::peer_is_this_computer(&ext);
    let proxied = calls_http::came_through_proxy(&headers);
    if !health_full_allowed(password_set, token_ok, peer_loopback, proxied) {
        let rt = state.settings_store.snapshot();
        return Json(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "local_time": chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
            "language": rt.language,
            // What the owner reads (the SMS forwarder labels its messages with it).
            "notify_language": rt.notify_lang().code(),
            "active_call_count": state.calls.live_calls().await.len(),
            "control_idle_clients": state.control.connected_serials().await.len(),
            // Names only (no values), so Bageo Status can tell setup is not finished.
            "setup_missing": setup_missing(&rt, &rt.effective(&state.settings)),
        }));
    }
    Json(health_full(&state).await)
}

/// `GET /calls/api/health`: the full health JSON for the web console, behind
/// the console's password like its other API routes.
async fn console_health(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> axum::response::Response {
    if !calls_http::token_ok_parts(&state.settings, &headers, q.get("token").map(String::as_str)) {
        return calls_http::unauthorized();
    }
    Json(health_full(&state).await).into_response()
}

/// The full health JSON, shared by `/health` (for who may see it) and the
/// console's `/calls/api/health`.
async fn health_full(state: &AppState) -> Value {
    // Effective view: env base + runtime overrides, so the health JSON shows
    // what a call starting right now would actually use.
    let rt = state.settings_store.snapshot();
    let eff = rt.effective(&state.settings);
    let s = &eff;
    let mut body = serde_json::Map::new();
    body.insert("ok".into(), json!(true));
    body.insert("latency_build".into(), json!(state.latency_build));
    body.insert(
        "ivr_menu_full_preloaded".into(),
        json!(state.ivr_menu_full_pcm.len()),
    );
    body.insert("ai_live_enabled".into(), json!(rt.ai_live_enabled));
    body.insert(
        "ai_live_ready".into(),
        json!(rt.ai_live_enabled && !rt.openrouter_api_key.trim().is_empty()),
    );
    {
        // Effective TRTC (env + UI overrides), same as a call starting now.
        let t = s.trtc();
        body.insert("trtc_ai_enabled".into(), json!(t.ai_enabled));
        body.insert("trtc_configured".into(), json!(t.configured()));
        body.insert("trtc_transfer_ready".into(), json!(t.transfer_ready()));
        body.insert("trtc_cloud_api_ready".into(), json!(t.cloud_api_ready()));
        body.insert("trtc_bridge_enabled".into(), json!(t.bridge_enabled));
        body.insert(
            "trtc_sdk_app_id".into(),
            json!(if t.sdk_app_id > 0 { t.sdk_app_id } else { 0 }),
        );
        // Never expose secret_key / UserSig / LLM·TTS JSON.
        body.insert(
            "trtc_usersig_ok".into(),
            json!(t
                .configured()
                .then(|| t.gen_user_sig("healthcheck").is_ok())
                .unwrap_or(false)),
        );
        let bridge_probe = trtc_bridge::probe_bridge_health(&t, &state.http).await;
        body.insert("trtc_bridge_ready".into(), json!(bridge_probe.ready));
        // Sidecar chromium_join / joined_sessions — false until real enterRoom.
        body.insert("media_bridge_live".into(), json!(bridge_probe.media_live));
    }
    body.insert("shaming_enabled".into(), json!(rt.shaming_enabled));
    // Spam deny/blocklist gating removed from call path; fields kept for UI compat (always report inactive gate).
    body.insert("spam_deny_gating".into(), json!(false));
    body.insert("spam_blocklist_len".into(), json!(0));
    body.insert("spam_ai_filter_enabled".into(), json!(false));
    body.insert("spam_keywords_len".into(), json!(0));
    body.insert("spam_keywords_custom".into(), json!(false));
    // Outbound confirm→dial allow-list (not inbound spam).
    body.insert(
        "callback_whitelist_len".into(),
        json!(rt.callback_whitelist.len()),
    );
    body.insert("agent_mode".into(), json!(s.agent_mode.as_str()));
    body.insert("ivr_mode".into(), json!(s.agent_mode.is_transfer()));
    body.insert("ivr_loop".into(), json!(s.ivr_loop));
    body.insert("transfer_prompt".into(), json!(s.transfer_prompt));
    body.insert("ivr_prompt_text".into(), json!(s.ivr_prompt_text));
    body.insert("transfer_prompt_text".into(), json!(s.ivr_prompt_text));
    body.insert(
        "transfer_dtmf_trigger".into(),
        json!(s.transfer_dtmf_trigger.to_string()),
    );
    body.insert(
        "ivr_dtmf_voicemail".into(),
        json!(s.ivr_dtmf_voicemail.to_string()),
    );
    body.insert("ivr_dtmf_end".into(), json!(s.ivr_dtmf_end));
    body.insert("ivr_goodbye_text".into(), json!(s.ivr_goodbye_text));
    body.insert("ivr_dtmf1_wait_text".into(), json!(s.ivr_dtmf1_wait_text));
    body.insert(
        "ivr_voicemail_stub_text".into(),
        json!(s.ivr_voicemail_stub_text),
    );
    body.insert("ivr_reprompt_ms".into(), json!(s.ivr_reprompt_ms));
    body.insert("ivr_dtmf_wait_ms".into(), json!(s.ivr_reprompt_ms));
    body.insert("voicemail_enabled".into(), json!(true));
    body.insert("ivr_vm_intro_wav".into(), json!(s.ivr_vm_intro_wav));
    body.insert("ivr_vm_confirm_wav".into(), json!(s.ivr_vm_confirm_wav));
    body.insert(
        "ivr_vm_intro_wav_loaded".into(),
        json!(state.ivr_vm_intro_pcm.is_some()),
    );
    body.insert(
        "ivr_vm_confirm_wav_loaded".into(),
        json!(state.ivr_vm_confirm_pcm.is_some()),
    );
    body.insert(
        "ivr_vm_wavs_optional_piper".into(),
        json!(state.ivr_vm_intro_pcm.is_none() || state.ivr_vm_confirm_pcm.is_none()),
    );
    body.insert("vm_max_ms".into(), json!(s.vm_max_ms));
    body.insert("vm_silence_ms".into(), json!(s.vm_silence_ms));
    body.insert("vm_silence_rms".into(), json!(s.vm_silence_rms));
    body.insert("vm_confirm_wait_ms".into(), json!(s.vm_confirm_wait_ms));
    body.insert("ivr_max_menu_plays".into(), json!(s.ivr_max_menu_plays));
    body.insert("ivr_start_delay_ms".into(), json!(s.ivr_start_delay_ms));
    body.insert(
        "post_greet_ai_delay_ms".into(),
        json!(s.post_greet_ai_delay_ms),
    );
    body.insert(
        "goodbye_hangup_delay_ms".into(),
        json!(s.goodbye_hangup_delay_ms),
    );
    body.insert("ivr_prompt_wavs".into(), json!(s.ivr_prompt_wavs));
    body.insert("ivr_fail_wav".into(), json!(s.ivr_fail_wav));
    body.insert("ivr_goodbye_wav".into(), json!(s.ivr_goodbye_wav));
    body.insert(
        "ivr_reject_unknown_wav".into(),
        json!(s.ivr_reject_unknown_wav),
    );
    body.insert(
        "ivr_reject_unknown_text".into(),
        json!(s.ivr_reject_unknown_text),
    );
    body.insert(
        "ivr_prompt_wav_loaded".into(),
        json!(state.ivr_prompt_loaded()),
    );
    body.insert(
        "ivr_prompt_pcm_bytes".into(),
        json!(state.ivr_prompt_pcm_bytes()),
    );
    body.insert(
        "ivr_prompt_segments".into(),
        json!(state
            .ivr_prompt_segments
            .iter()
            .map(|s| json!({
                "label": s.label,
                "path": s.path.display().to_string(),
                "pcm_bytes": s.pcm.len(),
            }))
            .collect::<Vec<_>>()),
    );
    body.insert(
        "ivr_prompt_segment_count".into(),
        json!(state.ivr_prompt_segments.len()),
    );
    body.insert(
        "ivr_fail_wav_loaded".into(),
        json!(state.ivr_fail_pcm.is_some()),
    );
    body.insert(
        "ivr_fail_pcm_bytes".into(),
        json!(state.ivr_fail_pcm.as_ref().map(|p| p.len()).unwrap_or(0)),
    );
    body.insert(
        "ivr_goodbye_wav_loaded".into(),
        json!(state.ivr_goodbye_pcm.is_some()),
    );
    body.insert(
        "ivr_goodbye_pcm_bytes".into(),
        json!(state.ivr_goodbye_pcm.as_ref().map(|p| p.len()).unwrap_or(0)),
    );
    body.insert(
        "ivr_reject_unknown_wav_loaded".into(),
        json!(state.ivr_reject_unknown_pcm.is_some()),
    );
    body.insert(
        "ivr_reject_unknown_pcm_bytes".into(),
        json!(state
            .ivr_reject_unknown_pcm
            .as_ref()
            .map(|p| p.len())
            .unwrap_or(0)),
    );
    body.insert("ivr_dtmf1_wait_wav".into(), json!(s.ivr_dtmf1_wait_wav));
    body.insert(
        "ivr_dtmf1_wait_wav_loaded".into(),
        json!(state.ivr_dtmf1_wait_pcm.is_some()),
    );
    body.insert(
        "ivr_dtmf1_wait_pcm_bytes".into(),
        json!(state
            .ivr_dtmf1_wait_pcm
            .as_ref()
            .map(|p| p.len())
            .unwrap_or(0)),
    );
    body.insert("ivr_ai_hello_wav".into(), json!(s.ivr_ai_hello_wav));
    body.insert(
        "ivr_ai_hello_wav_loaded".into(),
        json!(state.ivr_ai_hello_pcm.is_some()),
    );
    body.insert("ivr_ai_nudge_wav".into(), json!(s.ivr_ai_nudge_wav));
    body.insert(
        "ivr_ai_nudge_wav_loaded".into(),
        json!(state.ivr_ai_nudge_pcm.is_some()),
    );
    body.insert("transfer_wait_for_remote_voice".into(), json!(false));
    body.insert("wait_for_remote".into(), json!(false));
    body.insert("transfer_voice_ms".into(), json!(s.transfer_voice_ms));
    body.insert(
        "transfer_voice_timeout_ms".into(),
        json!(s.transfer_voice_timeout_ms),
    );
    body.insert("vad_rms_threshold".into(), json!(s.vad_rms_threshold));
    body.insert(
        "transfer_use_hold_music".into(),
        json!(s.transfer_use_hold_music),
    );
    body.insert("transfer_hold_music".into(), json!(s.transfer_hold_music));
    body.insert("hold_music_loaded".into(), json!(state.hold_pcm.is_some()));
    body.insert(
        "hold_pcm_bytes".into(),
        json!(state.hold_pcm.as_ref().map(|p| p.len()).unwrap_or(0)),
    );
    body.insert("llm_provider".into(), json!(s.llm_provider.as_str()));
    body.insert("ollama_url".into(), json!(s.ollama_url));
    body.insert("ollama_model".into(), json!(s.ollama_model));
    body.insert("openrouter_model".into(), json!(s.openrouter_model));
    body.insert("openrouter_base_url".into(), json!(s.openrouter_base_url));
    body.insert(
        "openrouter_api_key_set".into(),
        json!(!s.openrouter_api_key.trim().is_empty()),
    );
    body.insert("piper_url".into(), json!(s.piper_url));
    body.insert("whisper_url".into(), json!(s.whisper_url));
    body.insert("whisper_model".into(), json!(s.whisper_model));
    body.insert("sample_rate".into(), json!(s.sample_rate));
    body.insert("calls_ui".into(), json!("/calls/"));
    body.insert(
        "calls_ui_url".into(),
        json!(settings_store::effective_console_url(s)),
    );
    // The same, under the name the console's own API uses.
    body.insert("console_url".into(), json!(settings_store::effective_console_url(s)));
    body.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    // The clock notifications and "today" use (set TZ when it is not yours).
    body.insert(
        "local_time".into(),
        json!(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)),
    );
    body.insert("language".into(), json!(rt.language));
    body.insert("notify_language".into(), json!(rt.notify_lang().code()));
    body.insert("usb_bridge".into(), usb_bridge::status_json());
    // Phones over Wi-Fi: the setting, the port they use (null: none now) and
    // how many are connected so. Never the pairing key.
    body.insert(
        "phone_wifi".into(),
        json!({
            "on": rt.phone_wifi,
            "port": wifi_entry(s, &rt).map(|(_, port)| port),
            "wifi_phones": state.phones.wifi_count(),
        }),
    );
    body.insert("calls_data_dir".into(), json!(s.calls_data_dir));
    body.insert("calls_record".into(), json!(s.calls_record));
    body.insert("calls_record_tts".into(), json!(s.calls_record_tts));
    for (k, v) in health_retention(&rt) {
        body.insert(k.into(), v);
    }
    body.insert(
        "calls_ui_token_set".into(),
        json!(!settings_store::active_ui_token(&s.calls_ui_token).is_empty()),
    );
    body.insert("call_notify".into(), json!(s.call_notify));
    body.insert("call_notify_configured".into(), json!(notify_configured(s)));
    body.insert("setup_missing".into(), json!(setup_missing(&rt, s)));
    // The press-1 summary notification uses the same Apprise/webhook/bot chain as call/VM
    // (`notify_ai_request`). Bot token is NOT required when Apprise gateway is set.
    body.insert(
        "ai_request_notify_ready".into(),
        json!(notify_configured(s)),
    );
    body.insert(
        "notify_primary_backend".into(),
        json!(notify_primary_backend(s)),
    );
    body.insert(
        "notify_gateway_configured".into(),
        json!(!s.notify_gateway_url.trim().is_empty()),
    );
    body.insert(
        "notify_webhook_configured".into(),
        json!(!s.notify_webhook_url.trim().is_empty()),
    );
    // Task cards (confirm / ignore) + inbound poller only — not a gate for press-1 AI text.
    body.insert(
        "telegram_bot_configured".into(),
        json!(!s.telegram_bot_token.trim().is_empty() && !s.telegram_chat_id.trim().is_empty()),
    );
    let control_serials = state.control.connected_serials().await;
    body.insert("control_idle_clients".into(), json!(control_serials.len()));
    body.insert("control_idle_serials".into(), json!(control_serials));
    // Connected phones switched to "normal phone": still in control_idle_*, but the AI
    // does not answer their calls.
    body.insert(
        "normal_mode_serials".into(),
        json!(state.phones.normal_mode_serials()),
    );
    // TRTC AI: caller finished speaking → first AI audio (ms). 0 = no sample yet.
    let n = state.trtc_talk.reply_count.load(Ordering::Relaxed);
    body.insert(
        "ai_reply_latency_last_ms".into(),
        json!(state
            .trtc_talk
            .last_reply_latency_ms
            .load(Ordering::Relaxed)),
    );
    body.insert(
        "ai_reply_latency_avg_ms".into(),
        json!(state.trtc_talk.avg_ms()),
    );
    body.insert("ai_reply_latency_count".into(), json!(n));
    body.insert(
        "ai_reply_latency_last_serial".into(),
        json!(state
            .trtc_talk
            .last_serial
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()),
    );
    // Gemini Live / summary readiness (keys filled in /calls/ settings; never expose secrets).
    let rt = state.settings_store.snapshot();
    body.insert("gemini_live_enabled".into(), json!(rt.gemini_live_enabled));
    body.insert(
        "gemini_live_ready".into(),
        json!(rt.gemini_live_enabled && !rt.gemini_api_key.trim().is_empty()),
    );
    body.insert(
        "gemini_live_model".into(),
        json!(if rt.gemini_live_model.trim().is_empty() {
            gemini_live::default_model().to_string()
        } else {
            rt.gemini_live_model.clone()
        }),
    );
    // Sent only to models that need it (gemini_live::live_thinking_level).
    body.insert(
        "gemini_thinking_level".into(),
        json!(gemini_live::live_thinking_level(&rt.gemini_live_model, &rt.gemini_thinking_level)),
    );
    body.insert("gemini_persona_mode".into(), json!(rt.gemini_persona_mode));
    body.insert(
        "answer_unknown_callers".into(),
        json!(rt.answer_unknown_callers),
    );
    body.insert("persona_stall_spam".into(), json!(rt.persona_stall_spam));
    body.insert("scam_review_without_keywords".into(), json!(rt.scam_review_without_keywords));
    body.insert(
        "persona_stall_max_min".into(),
        json!(rt.persona_stall_max_secs() / 60),
    );
    body.insert(
        "persona_normal_max_min".into(),
        json!(rt.persona_max_secs() / 60),
    );
    body.insert("gemini_stall_model".into(), json!(rt.gemini_stall_model));
    body.insert("gemini_server_vad".into(), json!(rt.gemini_server_vad));
    body.insert("gemini_barge_in".into(), json!(rt.gemini_barge_in));
    body.insert(
        "gemini_voice_name".into(),
        json!(if rt.gemini_voice_name.trim().is_empty() {
            gemini_live::default_voice_name()
        } else {
            rt.gemini_voice_name.trim()
        }),
    );
    body.insert("ambience_kind".into(), json!(rt.ambience_kind));
    body.insert("ambience_level".into(), json!(rt.ambience_level));
    body.insert("blacklist_len".into(), json!(contacts::blacklist_len()));
    body.insert("reject_mode".into(), json!(rt.reject_mode));
    body.insert("reject_voice".into(), json!(voice_preview::reject_voice(&rt)));
    body.insert(
        "reject_clip_ready".into(),
        json!(state
            .previews
            .cached_reject_clip(
                &rt.gemini_live_model,
                voice_preview::reject_voice(&rt),
                rt.lang(),
                state.settings.sample_rate,
            )
            .is_some()),
    );
    body.insert("ambience_beds".into(), json!(state.ambience.loaded()));
    body.insert("answer_ring_ms".into(), json!(rt.answer_ring_ms));
    body.insert("gemini_ring_prewarm".into(), json!(rt.gemini_ring_prewarm));
    body.insert(
        "ring_prewarm_pending".into(),
        json!(state.ring_prewarm.pending()),
    );
    body.insert(
        "device_labels".into(),
        json!(notify::known_devices()
            .into_iter()
            .map(|(id, l)| (id, json!(l)))
            .collect::<serde_json::Map<String, Value>>()),
    );
    let owner_note = rt.owner_status_note();
    body.insert(
        "owner_status_enabled".into(),
        json!(rt.owner_status_enabled),
    );
    body.insert(
        "owner_status_active".into(),
        json!(!owner_note.is_empty()),
    );
    body.insert("owner_status_text".into(), json!(owner_note));
    // Effective view (env SUMMARY_API_KEY + UI override) — same as post-call summarize.
    let summary_ready = summary::summary_endpoint(&eff).is_some();
    body.insert("summary_api_key_set".into(), json!(summary_ready));
    body.insert("summary_ready".into(), json!(summary_ready));
    body.insert(
        "callback_sessions".into(),
        json!(state.control.session_count().await),
    );
    let live = state.calls.live_calls().await;
    body.insert("active_call_count".into(), json!(live.len()));
    body.insert(
        "active_calls".into(),
        json!(live
            .iter()
            .map(|c| json!({
                "id": c.id,
                "status": c.status,
                "caller_id": urgent_call::masked(&c.caller_id),
                "serial": c.serial,
                "started_at": c.started_at,
            }))
            .collect::<Vec<_>>()),
    );
    body.insert("runtime".into(), json!("rust"));
    Value::Object(body)
}

#[derive(Deserialize)]
struct TurnBody {
    text: Option<String>,
}

async fn turn(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    Json(body): Json<TurnBody>,
) -> impl IntoResponse {
    // An old test route: the console password, like the console's own API.
    if !calls_http::token_ok_parts(&state.settings, &headers, q.get("token").map(String::as_str)) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized", "code": "unauthorized"})),
        );
    }
    if state.settings.agent_mode.is_transfer() {
        let reply = state.settings.ivr_prompt_text.clone();
        return (
            axum::http::StatusCode::OK,
            Json(json!({
                "mode": state.settings.agent_mode.as_str(),
                "transcript": body.text.unwrap_or_default(),
                "reply": reply,
            })),
        );
    }

    let text = body.text.unwrap_or_default().trim().to_string();
    if text.is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({"error": "text required for /v1/turn in llm mode"})),
        );
    }
    match chat(&state.http, &state.settings, &text, &[]).await {
        Ok(reply) => (
            axum::http::StatusCode::OK,
            Json(json!({"mode": "llm", "transcript": text, "reply": reply})),
        ),
        Err(e) => (
            axum::http::StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("{e:#}")})),
        ),
    }
}

/// Smoke: IVR WAV (or Piper) prompt + hold-music PCM; confirms transfer/IVR audio path.
async fn transfer_smoke(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> axum::response::Response {
    if !calls_http::token_ok_parts(&state.settings, &headers, q.get("token").map(String::as_str)) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized", "code": "unauthorized"})),
        )
            .into_response();
    }
    let ivr = state.settings.ivr_prompt_text.clone();
    let (ivr_source, ivr_pcm_bytes) = if state.ivr_prompt_loaded() {
        ("ivr_wav_segments", state.ivr_prompt_pcm_bytes())
    } else {
        let pcm = synthesize_piper(&state.http, &state.settings, &ivr).await;
        ("piper_ivr_fallback", pcm.len())
    };
    let (hold_source, hold_bytes) = if let Some(hold) = state.hold_pcm.as_ref() {
        ("hold_music", hold.len())
    } else {
        let reply = state.settings.transfer_prompt.clone();
        let pcm = synthesize_piper(&state.http, &state.settings, &reply).await;
        ("piper_hold_fallback", pcm.len())
    };
    let fail_bytes = state.ivr_fail_pcm.as_ref().map(|p| p.len()).unwrap_or(0);
    let ok = ivr_pcm_bytes > 0 || hold_bytes > 0;
    let status = if ok {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::BAD_GATEWAY
    };
    (
        status,
        Json(json!({
            "ok": ok,
            "mode": state.settings.agent_mode.as_str(),
            "ivr_mode": state.settings.agent_mode.is_transfer(),
            "ivr_prompt_text": ivr,
            "ivr_prompt_source": ivr_source,
            "ivr_pcm_bytes": ivr_pcm_bytes,
            "ivr_prompt_pcm_bytes": ivr_pcm_bytes,
            "ivr_prompt_segment_count": state.ivr_prompt_segments.len(),
            "ivr_prompt_segments": state
                .ivr_prompt_segments
                .iter()
                .map(|s| s.label.clone())
                .collect::<Vec<_>>(),
            "ivr_fail_pcm_bytes": fail_bytes,
            "ivr_fail_wav_loaded": state.ivr_fail_pcm.is_some(),
            "ivr_goodbye_pcm_bytes": state.ivr_goodbye_pcm.as_ref().map(|p| p.len()).unwrap_or(0),
            "ivr_goodbye_wav_loaded": state.ivr_goodbye_pcm.is_some(),
            "ivr_reject_unknown_pcm_bytes": state.ivr_reject_unknown_pcm.as_ref().map(|p| p.len()).unwrap_or(0),
            "ivr_reject_unknown_wav_loaded": state.ivr_reject_unknown_pcm.is_some(),
            "ivr_max_menu_plays": state.settings.ivr_max_menu_plays,
            "ivr_start_delay_ms": state.settings.ivr_start_delay_ms,
            "post_greet_ai_delay_ms": state.settings.post_greet_ai_delay_ms,
            "goodbye_hangup_delay_ms": state.settings.goodbye_hangup_delay_ms,
            "source": hold_source,
            "pcm_bytes": hold_bytes,
            "sample_rate": state.settings.sample_rate,
            "transfer_hold_music": state.settings.transfer_hold_music,
            "hold_music_loaded": state.hold_pcm.is_some(),
            "transfer_dtmf_trigger": state.settings.transfer_dtmf_trigger.to_string(),
            "ivr_dtmf_end": state.settings.ivr_dtmf_end,
            "piper_url": state.settings.piper_url,
        })),
    )
        .into_response()
}

async fn handle_control_idle_ws(
    state: AppState,
    serial: String,
    sink: WsSink,
    mut stream: PhoneStream,
    link: PhoneLink,
) {
    let mut refreshed = phone_wifi::pairing().refreshed();
    loop {
        let next_msg = if link.trusted && !link.proxied {
            tokio::select! {
                msg = stream.next() => msg,
                changed = refreshed.changed() => {
                    if changed.is_ok() {
                        send_agent_info(&state, &sink, link).await;
                    }
                    continue;
                }
            }
        } else {
            stream.next().await
        };
        let Some(msg) = next_msg else {
            break;
        };
        let msg = match msg {
            Ok(m) => m,
            Err(e) => {
                error!("control ws read error serial={serial}: {e}");
                break;
            }
        };
        match msg {
            Message::Text(t) => {
                let parsed: Value = match serde_json::from_str(&t) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let mtype = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
                state.phones.touch(&serial);
                match mtype {
                    "place_call_ack" | "place_call_failed" | "outbound_ready" | "call_end" => {
                        state
                            .control
                            .handle_phone_event(&parsed, &state.task_store)
                            .await;
                        // An appointment's call-back: its call on this phone, or its end.
                        appointment_callbacks(&state, &serial, Some(&parsed)).await;
                    }
                    // Answer to `send_sms` (appointments: a text to the caller).
                    "send_sms_result" => {
                        state.control.handle_sms_result(&parsed).await;
                    }
                    // The carrier's delivery report for such a text.
                    "send_sms_delivered" => {
                        state.control.handle_sms_delivered(&parsed).await;
                    }
                    // A second call came in during a call and the phone turned it away.
                    "call_waiting_rejected" => {
                        on_call_waiting_rejected(&state, &serial, &parsed);
                    }
                    "ping" => {
                        let _ = send_json(&sink, json!({"type":"pong"})).await;
                    }
                    // The phone's mode (AI / normal phone) or call state changed.
                    "status" => {
                        // The phone has no call any more: a merged whitelist urgent call is over.
                        if urgent_call::status_ends_owner_call(&parsed)
                            && urgent_call::clear_owner_on_call(&canonical_device_serial(&serial))
                        {
                            info!("Whitelist urgent call: phone idle serial={serial} — owner-on-call mark cleared");
                        }
                        state.phones.on_status(&serial, &parsed);
                        sync_normal_mode(&state, &serial).await;
                        errands::note_hello_sims(&canonical_device_serial(&serial), &parsed);
                    }
                    // Answer to a web console mode switch (`set_mode`).
                    "set_mode_result" => {
                        state.phones.on_mode_reply(&serial, &parsed);
                        sync_normal_mode(&state, &serial).await;
                    }
                    // Phone rings (not answered yet): how long to ring, and the AI gets
                    // ready meanwhile (ring once before answering).
                    "incoming" => {
                        // Whitelist urgent call dialing / merged on this phone: the merge adds
                        // calls; none of them is a new call for the AI.
                        if urgent_call::owner_on_call(&canonical_device_serial(&serial)) {
                            info!("incoming serial={serial} ignored, owner is on the call (whitelist urgent call) — no AI, no answer plan");
                            continue;
                        }
                        let caller = extract_caller_id_from_msg(&parsed).unwrap_or_default();
                        note_call_sim(&serial, &parsed);
                        // A new call on this phone: a blocked call still waiting for its
                        // hello (declined, or its incoming_cancel lost) is over.
                        if let Some(old) = state.reject_pending.take(&serial) {
                            info!(
                                "incoming serial={serial} drops the earlier 拒接號碼 ring caller={}",
                                urgent_call::masked(&old.caller)
                            );
                        }
                        let sample_rate = parsed
                            .get("sample_rate")
                            .and_then(|v| v.as_u64())
                            .filter(|v| (8_000..=48_000).contains(v))
                            .map_or(state.settings.sample_rate, |v| v as u32);
                        let rt = state.settings_store.snapshot();
                        if rt.whitelist_mode == "direct" {
                            let sim = call_sim_country(&serial);
                            // Who may be forwarded: the switches under the transfer menu. Our
                            // own calls going out are never declined.
                            let target = match urgent_call::transfer_target(&rt, &caller, sim.as_deref()) {
                                Some(t) if !errands::is_active_outbound_call(state.control.as_ref(), &serial, &caller).await => Some(t),
                                _ => None,
                            };
                            if let Some(t) = target {
                                let decline = state.control.supports_reject(&serial).await;
                                if whitelist_direct_ready(&rt.whitelist_forward_number, decline) {
                                    record_whitelist_direct_transfer(
                                        &state,
                                        &serial,
                                        &caller,
                                        &t.name,
                                        sample_rate,
                                        &rt,
                                    )
                                    .await;
                                    let _ = send_json(&sink, reject_answer_plan(true)).await;
                                    info!(
                                        "incoming serial={serial} caller={} may be forwarded ({:?}) whitelist_mode=direct → decline, transfer to '{}'",
                                        urgent_call::masked(&caller),
                                        t.who,
                                        urgent_call::masked(&rt.whitelist_forward_number)
                                    );
                                    continue;
                                }
                                // Never hang up on a caller to forward: without a number to
                                // forward to, or a phone that can decline, the AI answers.
                                warn!(
                                    "incoming serial={serial} caller={} may be forwarded ({:?}) whitelist_mode=direct but the transfer is not ready (number set={}, phone can decline={decline}) → the AI answers",
                                    urgent_call::masked(&caller),
                                    t.who,
                                    !rt.whitelist_forward_number.trim().is_empty()
                                );
                            }
                        }
                        // A number without a country code reads as dialled on the SIM that rings.
                        if reject_listed_on_line(&rt, &caller, &serial) {
                            // Blocked number: no Gemini. Decline (`reject_mode` decline and
                            // an app that can): the phone refuses the ringing call, so the
                            // record and the notification are made now. Otherwise pick up at
                            // once: the media hello plays the announcement and notifies.
                            let decline =
                                blocked_number_declined(&rt, &state.control, &serial).await;
                            let record_id = if decline {
                                record_declined_call(&state, &serial, &caller, sample_rate, &rt).await
                            } else {
                                None
                            };
                            state.reject_pending.mark_ring(
                                &serial,
                                RejectRing {
                                    caller: caller.clone(),
                                    declined: decline,
                                    record_id,
                                },
                            );
                            let _ = send_json(&sink, reject_answer_plan(decline)).await;
                            info!(
                                "incoming serial={serial} caller={} on 拒接號碼 → {} (no AI)",
                                urgent_call::masked(&caller),
                                if decline { "decline while ringing" } else { "pick up, announce, hang up" }
                            );
                            continue;
                        }
                        let ring_ms = rt.answer_ring_ms;
                        let prewarm =
                            start_ring_prewarm(&state, &serial, &caller, sample_rate, &rt, ring_ms);
                        let _ = send_json(
                            &sink,
                            json!({"type": "answer_plan", "delay_ms": ring_ms, "prewarm": prewarm}),
                        )
                        .await;
                        info!(
                            "incoming serial={serial} caller={} answer_ring_ms={ring_ms} prewarm={prewarm}",
                            urgent_call::masked(&caller)
                        );
                    }
                    "incoming_cancel" => {
                        state
                            .ring_prewarm
                            .cancel(&serial, None, "call ended before pickup");
                        state.reject_pending.take(&serial);
                    }
                    other => {
                        info!("control_idle text serial={serial} type={other}");
                    }
                }
            }
            Message::Binary(_) => {}
            Message::Close(_) => break,
            _ => {}
        }
    }
    // Only drop our own sink: if the phone already reconnected, the registry holds
    // the new socket and removing by serial alone would hide a live idle phone.
    state.control.unregister_client_sink(&serial, &sink).await;
    state.phones.disconnected(&serial, &sink);
}

/// Call-backs skip phones in normal-phone mode: keep the dispatcher's list in line.
async fn sync_normal_mode(state: &AppState, serial: &str) {
    let normal = state.phones.mode_of(serial) == Some(calls_http::PhoneMode::Normal);
    state.control.set_normal_mode(serial, normal).await;
}

/// Tell a phone's app where the web console is, for its "Open the web console"
/// button: the address typed in the settings (`public_url`) or, when that is empty,
/// the LAN IP address (some Android phones cannot open `.local` names, so the
/// automatic `bageo.local` address is not used here); left out while it is unknown.
/// `wifi.urls`: where the phone connects once unplugged, with the pairing key
/// ([`phone_wifi`]); empty when phone Wi-Fi is off or there is no LAN address
/// (the phone then forgets the ones it kept). Only on a connection that may
/// have the key ([`PhoneLink::trusted`]) and not through a reverse proxy; the
/// phone keeps what it has when the field is missing. Older apps ignore the message.
async fn send_agent_info(state: &AppState, sink: &WsSink, link: PhoneLink) {
    let rt = state.settings_store.snapshot();
    let eff = rt.effective(&state.settings);
    let base = if eff.public_url.trim().is_empty() {
        settings_store::lan_console_url(&eff)
    } else {
        settings_store::effective_console_url(&eff)
    };
    let mut msg = serde_json::Map::new();
    msg.insert("type".into(), json!("agent_info"));
    if let Some(url) = calls_http::phone_console_url(&base) {
        msg.insert("console_url".into(), json!(url));
    }
    if link.trusted && !link.proxied {
        let urls = agent_info_wifi_urls(
            wifi_entry(&state.settings, &rt),
            mdns::answer_ip,
            &mdns::parse_name_setting(&rt.mdns_name),
            || phone_wifi::pairing().key(),
        );
        msg.insert("wifi".into(), json!({ "urls": urls }));
    }
    if msg.len() > 1 {
        let _ = send_json(sink, Value::Object(msg)).await;
    }
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> axum::response::Response {
    phone_ws(ws, peer, &headers, &q, state)
}

async fn handle_ws(socket: WebSocket, base_state: AppState, link: PhoneLink) {
    // Session view of runtime settings: snapshot here, refreshed at call start
    // (hello/ringing). `state.settings` carries the env+runtime merge so all
    // existing helpers keep reading `state.settings.*` unchanged.
    let mut rt: Arc<RuntimeSettings> = base_state.settings_store.snapshot();
    let mut state = base_state.clone();
    state.settings = rt.effective(&base_state.settings);

    let (sink, stream) = socket.split();
    let mut stream: PhoneStream = stream.take_until(until_dropped(link));
    let sink = Arc::new(Mutex::new(sink));

    let mut serial = "unknown".to_string();
    let mut caller_id = String::new();
    let mut sample_rate = state.settings.sample_rate;
    let mut history: Vec<ChatMessage> = Vec::new();
    let mut vad = EnergyVad::new(
        sample_rate,
        state.settings.vad_rms_threshold,
        state.settings.silence_ms,
        state.settings.min_utterance_ms,
        state.settings.max_utterance_ms,
    );
    let busy = Arc::new(Mutex::new(()));
    let mut uplink_bytes: u64 = 0;
    let mut hold_cancel: Option<Arc<AtomicBool>> = None;
    // Cancels in-flight menu/prompt PCM on DTMF barge-in (avoids overlap crackle).
    let prompt_cancel = Arc::new(AtomicBool::new(false));
    // Empty hello CID: wait for ringing/hello caller_id before reject-only.
    let cid_wait_pending = Arc::new(AtomicBool::new(false));
    let mut remote_gate = RemoteVoiceGate::new(
        sample_rate,
        state.settings.vad_rms_threshold,
        state.settings.transfer_voice_ms,
    );
    let mut dtmf = DtmfDecoder::new(sample_rate);
    let ivr_phase = Arc::new(std::sync::Mutex::new(IvrPhase::WaitDtmf));
    // After greeting ends → set true to start auto Gemini AI (or VM if Gemini unavailable).
    let arm_auto_ai = Arc::new(AtomicBool::new(false));
    // Wakes the session select so arm_auto_ai is handled without waiting for next PCM frame.
    let auto_ai_wake = Arc::new(Notify::new());
    // Completed greeting plays this session.
    let menu_plays = Arc::new(AtomicU32::new(0));
    let mut hold_started = false;
    let mut deadline: Option<(tokio::time::Instant, DeadlineKind)> = None;
    // After greeting ends, brief DTMF ignore (false keys from residual speech).
    let dtmf_ignore_until: Arc<std::sync::Mutex<Option<tokio::time::Instant>>> =
        Arc::new(std::sync::Mutex::new(None));
    let active_call: ActiveCallSlot = Arc::new(Mutex::new(None));
    let mut call_notified = false;
    // This call's number is on the blocked list (the notification card says so instead of "incoming call").
    let mut call_reject_listed = false;
    // Set when DTMF1 enters TrtcAi; uplink voice clears silence nudge/hangup.
    let mut trtc_ai_heard_voice: Option<Arc<AtomicBool>> = None;
    // Per-call turn timing for reply_latency_ms (caller stop → AI audio).
    let mut trtc_turn: Option<Arc<TrtcTurnTracker>> = None;
    // Gemini Live uplink PCM channel (set while DTMF1 Gemini session runs).
    let mut gemini_uplink_tx: Arc<
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>,
    > = Arc::new(std::sync::Mutex::new(None));
    // Live conversation text from Gemini input/output transcription (for the notification).
    let mut gemini_transcript: Arc<std::sync::Mutex<String>> =
        Arc::new(std::sync::Mutex::new(String::new()));
    // ConnectAtAnswer: false until UnmuteAfterGreet (full IVR greet done). Cold start leaves true.
    let mut gemini_audio_open = Arc::new(AtomicBool::new(true));
    // Exact dialogue-open instant used by the absolute first-delivery watchdog.
    let mut gemini_audio_opened_at: GeminiAudioOpenedAt = Arc::new(std::sync::Mutex::new(None));
    // setupComplete (or cold-open) for brief wait at greet end.
    let mut gemini_setup_ready = Arc::new(AtomicBool::new(false));
    // Warm connect failed before unmute → cold-start Gemini (VM only if that also fails).
    let mut gemini_connect_failed = Arc::new(AtomicBool::new(false));
    // True after start_gemini_connect_at_answer spawned a muted session for this call.
    let mut gemini_prewarmed = false;
    // Per-media-call ownership token for spawned Gemini tasks on this WS.
    let mut media_call_guard = MediaCallGuard::inactive();
    // Voicemail (DTMF 2) capture state — recording happens in this loop.
    let mut vm_buf: Vec<u8> = Vec::new();
    let mut vm_rec_started: Option<tokio::time::Instant> = None;
    let mut vm_last_voice: Option<tokio::time::Instant> = None;
    let mut vm_voiced = false;
    let mut vm_saved = false;
    let vm_notified = Arc::new(AtomicBool::new(false));
    // Parsed phone/email from live VM transcription awaiting DTMF 1/2.
    let vm_contact_candidate: Arc<std::sync::Mutex<Option<SpokenContact>>> =
        Arc::new(std::sync::Mutex::new(None));
    // Set by the confirm-prompt task once playback ends → arms the DTMF wait.
    let arm_vm_confirm = Arc::new(AtomicBool::new(false));
    let arm_vm_contact_readback = Arc::new(AtomicBool::new(false));

    if send_json(
        &sink,
        json!({
            "type": "ready",
            "sample_rate": sample_rate,
            "agent_mode": state.settings.agent_mode.as_str(),
            "ivr_mode": state.settings.agent_mode.is_transfer(),
        }),
    )
    .await
    .is_err()
    {
        return;
    }

    loop {
        // Product: after period greeting finishes → auto Gemini (no DTMF 1 / no re-menu).
        if arm_auto_ai.swap(false, Ordering::Relaxed) {
            let phase = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
            if phase == IvrPhase::WaitDtmf
                && state.settings.agent_mode.is_transfer()
                && !hold_started
            {
                deadline = None;
                remote_gate.reset();
                info!("IVR auto-AI arm serial={serial} (greeting done → Gemini or VM fallback)");
                enter_gemini_or_voicemail(
                    &sink,
                    &state,
                    &serial,
                    &ivr_phase,
                    &busy,
                    &mut hold_started,
                    &mut hold_cancel,
                    &active_call,
                    &rt,
                    &gemini_transcript,
                    &gemini_uplink_tx,
                    sample_rate,
                    false, // greeting already said "please hold" — skip connect-wait clip
                    &gemini_audio_open,
                    &gemini_audio_opened_at,
                    &gemini_setup_ready,
                    &gemini_connect_failed,
                    &mut gemini_prewarmed,
                    &media_call_guard,
                );
            }
        }
        if arm_vm_contact_readback.swap(false, Ordering::Relaxed) {
            let phase = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
            if phase == IvrPhase::VoicemailContactReadback && state.settings.vm_confirm_wait_ms > 0
            {
                deadline = Some((
                    tokio::time::Instant::now()
                        + Duration::from_millis(state.settings.vm_confirm_wait_ms as u64),
                    DeadlineKind::VmContactReadback,
                ));
                info!(
                    "VM contact readback armed wait_ms={} serial={serial}",
                    state.settings.vm_confirm_wait_ms
                );
            }
        }
        if arm_vm_confirm.swap(false, Ordering::Relaxed) {
            let phase = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
            if phase == IvrPhase::VoicemailConfirm && state.settings.vm_confirm_wait_ms > 0 {
                deadline = Some((
                    tokio::time::Instant::now()
                        + Duration::from_millis(state.settings.vm_confirm_wait_ms as u64),
                    DeadlineKind::VmConfirm,
                ));
                info!(
                    "VM confirm armed wait_ms={} serial={serial}",
                    state.settings.vm_confirm_wait_ms
                );
            }
        }

        let deadline_copy = deadline;
        let timeout_sleep = async move {
            if let Some((d, _)) = deadline_copy {
                tokio::time::sleep_until(d).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(timeout_sleep);
        let ai_woken = auto_ai_wake.notified();
        tokio::pin!(ai_woken);

        tokio::select! {
            _ = &mut ai_woken => {
                // arm_auto_ai is re-checked at top of next loop iteration.
                continue;
            }
            maybe_msg = stream.next() => {
                let Some(msg) = maybe_msg else { break };
                let msg = match msg {
                    Ok(m) => m,
                    Err(e) => {
                        error!("ws read error serial={serial}: {e}");
                        break;
                    }
                };
                match msg {
                    Message::Text(t) => {
                        let parsed: Value = match serde_json::from_str(&t) {
                            Ok(v) => v,
                            Err(_) => {
                                let _ = send_json(&sink, json!({"type":"error","message":"invalid json"})).await;
                                continue;
                            }
                        };
                        let mtype = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match mtype {
                            "hello" => {
                                let ws_mode = parsed
                                    .get("mode")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if let Some(s) = parsed.get("serial").and_then(|v| v.as_str()) {
                                    if !s.is_empty() {
                                        serial = s.to_string();
                                    }
                                }
                                if let Some(model) = parsed.get("model").and_then(|v| v.as_str()) {
                                    notify::remember_device(&canonical_device_serial(&serial), model);
                                }
                                if ws_mode == "CONTROL_IDLE" {
                                    state
                                        .control
                                        .register_client(serial.clone(), sink.clone())
                                        .await;
                                    state.phones.connected(&serial, sink.clone(), &parsed);
                                    state.phones.set_wifi(&serial, link.wifi);
                                    sync_normal_mode(&state, &serial).await;
                                    // An app that lists its SIMs: errands dial from the right one.
                                    errands::note_hello_sims(&canonical_device_serial(&serial), &parsed);
                                    // Newer apps send texts for the agent (appointments).
                                    state
                                        .control
                                        .set_supports_sms(
                                            &serial,
                                            parsed.get("supports_sms").and_then(Value::as_bool)
                                                == Some(true),
                                        )
                                        .await;
                                    // Newer apps can refuse a ringing call (blocked numbers).
                                    state
                                        .control
                                        .set_supports_reject(
                                            &serial,
                                            parsed.get("supports_reject").and_then(Value::as_bool)
                                                == Some(true),
                                        )
                                        .await;
                                    let _ = send_json(
                                        &sink,
                                        json!({
                                            "type": "hello_ack",
                                            "serial": serial,
                                            "mode": "CONTROL_IDLE",
                                        }),
                                    )
                                    .await;
                                    // Blacklist gating removed: push empty list so phones clear any cached deny list.
                                    if let Err(e) = state
                                        .control
                                        .send_blocklist_sync(&serial, &[])
                                        .await
                                    {
                                        warn!(
                                            "control_idle blocklist clear failed serial={serial}: {e:#}"
                                        );
                                    }
                                    send_agent_info(&state, &sink, link).await;
                                    handle_control_idle_ws(state, serial, sink, stream, link).await;
                                    return;
                                }
                                // Whitelist urgent call dialing / merged on this phone: a new media
                                // connection is one of the calls the merge made. No Gemini, no
                                // speech, no hangup: read it until it closes.
                                if urgent_call::owner_on_call(&canonical_device_serial(&serial)) {
                                    info!(
                                        "session hello serial={serial} ignored, owner is on the call (whitelist urgent call) — no AI, no speech, no hangup"
                                    );
                                    while let Some(m) = stream.next().await {
                                        if m.is_err() {
                                            break;
                                        }
                                    }
                                    info!("ignored media connection closed serial={serial}");
                                    return;
                                }
                                media_call_guard = media_call_guard.next().await;
                                // New media call: reset DTMF1 latch. Only leave TRTC if a stale
                                // TaskId remains (do not thrash open_session on every hello).
                                hold_started = false;
                                trtc_ai_heard_voice = None;
                                trtc_turn = None;
                                stop_hold_loop(&mut hold_cancel);
                                // A media WS may carry another logical call. Replace every Gemini
                                // gate/channel so an old prewarm task cannot mark the new call ready,
                                // failed, open, or clear its uplink sender during rapid recall.
                                gemini_prewarmed = false;
                                gemini_audio_open = Arc::new(AtomicBool::new(true));
                                gemini_audio_opened_at = Arc::new(std::sync::Mutex::new(None));
                                gemini_setup_ready = Arc::new(AtomicBool::new(false));
                                gemini_connect_failed = Arc::new(AtomicBool::new(false));
                                gemini_uplink_tx = Arc::new(std::sync::Mutex::new(None));
                                gemini_transcript =
                                    Arc::new(std::sync::Mutex::new(String::new()));
                                if trtc_bridge::peek_ai_task_id(&serial).is_some() {
                                    warn!(
                                        "session hello cleanup serial={serial} (stale TRTC TaskId)"
                                    );
                                    trtc_bridge::make_bridge(
                                        &state.settings.trtc(),
                                        state.http.clone(),
                                    )
                                    .leave(&serial, "session_hello_cleanup");
                                }
                                *ivr_phase.lock().unwrap_or_else(|e| e.into_inner()) =
                                    IvrPhase::WaitDtmf;
                                if active_call.lock().await.is_none() {
                                    call_notified = false;
                                    call_reject_listed = false;
                                    vm_saved = false;
                                    vm_notified.store(false, Ordering::Relaxed);
                                    caller_id = extract_caller_id_from_msg(&parsed).unwrap_or_default();
                                } else if let Some(c) = extract_caller_id_from_msg(&parsed) {
                                    caller_id = c;
                                }
                                note_call_sim(&serial, &parsed);
                                if let Some(sr) = parsed.get("sample_rate").and_then(|v| v.as_u64()) {
                                    sample_rate = sr as u32;
                                    vad = EnergyVad::new(
                                        sample_rate,
                                        state.settings.vad_rms_threshold,
                                        state.settings.silence_ms,
                                        state.settings.min_utterance_ms,
                                        state.settings.max_utterance_ms,
                                    );
                                    remote_gate = RemoteVoiceGate::new(
                                        sample_rate,
                                        state.settings.vad_rms_threshold,
                                        state.settings.transfer_voice_ms,
                                    );
                                    dtmf.set_sample_rate(sample_rate);
                                }
                                // Hot-swap boundary: refresh runtime settings at call start.
                                rt = base_state.settings_store.snapshot();
                                state.settings = rt.effective(&base_state.settings);
                                // t0 for answer/OFFHOOK-equivalent (media hello). All
                                // hello_to_*_ms IVR logs share this wall reference.
                                let session_hello_t0_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis())
                                    .unwrap_or(0);
                                info!(
                                    "session hello serial={serial} caller={} mode={} ivr_prompt={:?} ivr_loop={} t0_unix_ms={session_hello_t0_ms}",
                                    urgent_call::masked(&caller_id),
                                    state.settings.agent_mode.as_str(),
                                    state.settings.ivr_prompt_text,
                                    state.settings.ivr_loop
                                );
                                agent_dbg(
                                    "main.rs:hello",
                                    "session_hello",
                                    "A",
                                    json!({
                                        "serial": serial,
                                        "sample_rate": state.settings.sample_rate,
                                        "mode": state.settings.agent_mode.as_str(),
                                        "pcm_codec": "s16le_raw_no_opus",
                                    }),
                                );
                                // Active outbound calls (appointment callbacks, errands, to-do
                                // callbacks) must be recognized BEFORE incoming reject-list /
                                // ring-prewarm logic runs.
                                let is_outbound =
                                    hello_is_outbound(&state, &serial, &caller_id, &parsed).await;
                                // Blocked numbers: this number, or the phone's ringing call was on
                                // it (the hello may come before Telecom publishes the CID).
                                let rang_listed = if is_outbound {
                                    let _ = state.reject_pending.take(&serial);
                                    None
                                } else {
                                    state.reject_pending.take_for_hello(&serial, &caller_id)
                                };
                                if !is_outbound && caller_id.trim().is_empty() {
                                    if let Some(c) = rang_listed
                                        .as_ref()
                                        .map(|r| r.caller.as_str())
                                        .filter(|c| !c.trim().is_empty())
                                    {
                                        caller_id = c.to_string();
                                    }
                                }
                                let hello_plan = plan_hello_routing(
                                    &serial,
                                    &caller_id,
                                    rt.answer_unknown_callers,
                                    is_outbound,
                                    || {
                                        rang_listed.is_some()
                                            || reject_listed_on_line(&rt, &caller_id, &serial)
                                    },
                                );
                                let reject_listed = hello_plan.reject_listed;
                                call_reject_listed = reject_listed;
                                // Declined while it rang, picked up anyway (the app said it
                                // can decline, then did not): the caller gets the
                                // announcement and the record written at the decline goes
                                // on. Its "declined" notification was wrong: the usual one
                                // below says what happened.
                                if let Some(ring) = rang_listed.as_ref().filter(|r| r.declined) {
                                    base_state
                                        .incoming_notify_dedup
                                        .release(&serial, &ring.caller)
                                        .await;
                                    info!(
                                        "declined 拒接號碼 picked up anyway serial={serial} caller={} record={:?} → announcement",
                                        urgent_call::masked(&caller_id),
                                        ring.record_id
                                    );
                                    if let Some(id) = ring.record_id.as_deref() {
                                        let free = active_call.lock().await.is_none();
                                        if free {
                                            if let Some(call) = state
                                                .calls
                                                .resume(id, &["reject_list_declined", "ended"])
                                                .await
                                            {
                                                *active_call.lock().await = Some(call);
                                                state
                                                    .call_reattach
                                                    .mark_live(&serial, &caller_id, id, active_call.clone())
                                                    .await;
                                            }
                                        }
                                    }
                                }
                                let ensured = ensure_active_call(
                                    &state,
                                    &active_call,
                                    &serial,
                                    &caller_id,
                                    sample_rate,
                                    "active",
                                )
                                .await;
                                if let Some(c) = active_call.lock().await.as_mut() {
                                    c.set_sim(call_sim_country(&serial), call_sim_sub_id(&serial));
                                }
                                if ensured == EnsureCall::Reattached {
                                    info!(
                                        "session hello reattached existing call_id serial={serial} caller={}",
                                        urgent_call::masked(&caller_id)
                                    );
                                }
                                if !call_notified {
                                    call_notified = true;
                                    let dedup = base_state.incoming_notify_dedup.clone();
                                    let caller = caller_id.clone();
                                    let ser = serial.clone();
                                    let notice = ring_notice(&state, call_reject_listed, &ser, &caller).await;
                                    if notice == errands::CallNotice::Silent {
                                        info!("call notify skipped (redial try after the first) serial={ser} caller={}", urgent_call::masked(&caller));
                                    } else if dedup.claim(&ser, &caller).await {
                                        let http = state.http.clone();
                                        // Fresh snapshot: honor hot-swapped call_notify + gateway URL.
                                        let settings = {
                                            let snap = base_state.settings_store.snapshot();
                                            snap.effective(&base_state.settings)
                                        };
                                        let dedup_for_task = dedup.clone();
                                        let rejected = call_reject_listed;
                                        let reject_note =
                                            reject_reason_on_line(&rt, &caller_id, &serial).unwrap_or_default();
                                        tokio::spawn(async move {
                                            let ok = if rejected {
                                                notify_rejected_call(
                                                    &http, &settings, &caller, &ser, &reject_note, false,
                                                )
                                                .await
                                            } else if notice == errands::CallNotice::Outgoing {
                                                notify_outgoing_call(&http, &settings, &caller, &ser)
                                                    .await
                                            } else {
                                                notify_incoming_call(&http, &settings, &caller, &ser)
                                                    .await
                                            };
                                            let what = if notice == errands::CallNotice::Outgoing { "outgoing" } else { "incoming" };
                                            if ok {
                                                info!(
                                                    "{what} call notify ok serial={ser} caller={}",
                                                    urgent_call::masked(&caller)
                                                );
                                            } else {
                                                dedup_for_task.release(&ser, &caller).await;
                                                warn!(
                                                    "{what} call notify FAILED serial={ser} caller={} call_notify={} gateway_set={}",
                                                    urgent_call::masked(&caller),
                                                    settings.call_notify,
                                                    !settings.notify_gateway_url.trim().is_empty()
                                                );
                                            }
                                        });
                                    } else {
                                        info!(
                                            "incoming call notify dedup serial={ser} caller={} window_s={}",
                                            urgent_call::masked(&caller),
                                            INCOMING_NOTIFY_DEDUP_WINDOW.as_secs()
                                        );
                                    }
                                }
                                let _ = send_json(
                                    &sink,
                                    json!({
                                        "type": "hello_ack",
                                        "serial": serial,
                                        "caller_id": caller_id,
                                        "sample_rate": sample_rate,
                                        "agent_mode": state.settings.agent_mode.as_str(),
                                        "ivr_mode": true,
                                        "ivr_loop": state.settings.ivr_loop,
                                        "ivr_prompt_text": state.settings.ivr_prompt_text,
                                        "transfer_dtmf_trigger": state.settings.transfer_dtmf_trigger.to_string(),
                                        "ivr_dtmf_voicemail": state.settings.ivr_dtmf_voicemail.to_string(),
                                        "ivr_dtmf_end": state.settings.ivr_dtmf_end,
                                        "ivr_reprompt_ms": state.settings.ivr_reprompt_ms,
                                        "ivr_max_menu_plays": state.settings.ivr_max_menu_plays,
                                        "ivr_start_delay_ms": state.settings.ivr_start_delay_ms,
                                        "post_greet_ai_delay_ms": state.settings.post_greet_ai_delay_ms,
                                        "goodbye_hangup_delay_ms": state.settings.goodbye_hangup_delay_ms,
                                        "ivr_prompt_wav_loaded": state.ivr_prompt_loaded(),
                                        "ivr_prompt_segment_count": state.ivr_prompt_segments.len(),
                                        "ivr_goodbye_wav_loaded": state.ivr_goodbye_pcm.is_some(),
                                        "ivr_reject_unknown_wav_loaded": state.ivr_reject_unknown_pcm.is_some(),
                                        "transfer_wait_for_remote_voice": false,
                                        "wait_for_remote": false,
                                    }),
                                )
                                .await;

                                // Product: always the agent's period greeting over WS binary
                                // (USB bridge → TYPE_TELEPHONY). Ignore phone local_menu —
                                // old APKs may still send it but MUST NOT skip the agent's stream.
                                if state.settings.agent_mode.is_transfer() {
                                    deadline = None;
                                    menu_plays.store(0, Ordering::Relaxed);
                                    prompt_cancel.store(false, Ordering::Relaxed);
                                    let claimed_local = parsed
                                        .get("local_menu")
                                        .and_then(|v| v.as_bool())
                                        .unwrap_or(false);
                                    if claimed_local {
                                        info!(
                                            "IVR ignore phone local_menu=true serial={serial} — force agent greeting stream"
                                        );
                                    }
                                    // Hidden tokens: reject WAV only. Empty CID waits (Android late).
                                    // `answer_unknown_callers` on: both answered like a normal call.
                                    // Isolated extra uid (pixel-101xxxx) must not inject greet/reject.
                                    let answer_unknown = rt.answer_unknown_callers;
                                    if answer_unknown && is_unknown_or_hidden_caller(&caller_id) {
                                        info!(
                                            "unknown/hidden caller serial={serial} caller={:?} → answer like a normal call (answer_unknown_callers=on)",
                                            urgent_call::masked(&caller_id)
                                        );
                                    }
                                    match hello_plan.route {
                                        CidRoute::IgnoreIsolated => {
                                            // Park the twin as finished so no later path (DTMF decode of
                                            // overheard audio, legacy menu timeout, auto-AI) can start
                                            // voicemail/AI audio on top of the real session.
                                            cid_wait_pending.store(false, Ordering::SeqCst);
                                            prompt_cancel.store(true, Ordering::Relaxed);
                                            deadline = None;
                                            *ivr_phase.lock().unwrap_or_else(|e| e.into_inner()) =
                                                IvrPhase::Done;
                                            info!(
                                                "IVR skip isolated screening serial={serial} caller={:?} → phase Done (no greet/reject/VM/AI inject)",
                                                urgent_call::masked(&caller_id)
                                            );
                                        }
                                        CidRoute::Wait => {
                                            if cid_wait_pending
                                                .compare_exchange(
                                                    false,
                                                    true,
                                                    Ordering::SeqCst,
                                                    Ordering::SeqCst,
                                                )
                                                .is_ok()
                                            {
                                                info!(
                                                    "empty caller_id on hello serial={serial} — wait {}ms for ringing/hello CID before reject",
                                                    EMPTY_CID_WAIT.as_millis()
                                                );
                                                spawn_empty_cid_wait(
                                                    sink.clone(),
                                                    state.clone(),
                                                    serial.clone(),
                                                    ivr_phase.clone(),
                                                    busy.clone(),
                                                    hold_cancel.clone(),
                                                    active_call.clone(),
                                                    prompt_cancel.clone(),
                                                    cid_wait_pending.clone(),
                                                );
                                            }
                                        }
                                        CidRoute::Reject => {
                                            cid_wait_pending.store(false, Ordering::SeqCst);
                                            info!(
                                                "unknown/private caller serial={serial} caller={:?} → reject WAV + hangup (skip greet/AI)",
                                                urgent_call::masked(&caller_id)
                                            );
                                            if let Some(c) = active_call.lock().await.as_mut() {
                                                c.push_event("reject_unknown_caller");
                                            }
                                            start_unknown_caller_reject(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &mut hold_cancel,
                                                &active_call,
                                                &prompt_cancel,
                                            )
                                            .await;
                                        }
                                        CidRoute::RejectList => {
                                            cid_wait_pending.store(false, Ordering::SeqCst);
                                            start_reject_list_announce(
                                                &sink,
                                                &state,
                                                &serial,
                                                &caller_id,
                                                &ivr_phase,
                                                &busy,
                                                &mut hold_cancel,
                                                &active_call,
                                                &prompt_cancel,
                                            )
                                            .await;
                                        }
                                        CidRoute::Greet => {
                                            cid_wait_pending.store(false, Ordering::SeqCst);
                                            start_ivr_prompt(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &active_call,
                                                &arm_auto_ai,
                                                &auto_ai_wake,
                                                &menu_plays,
                                                &prompt_cancel,
                                                true, // IVR_START_DELAY_MS (hard-capped ≤400ms; not stacked on Android)
                                                Some(dtmf_ignore_until.clone()),
                                            )
                                            .await;
                                            // The session started while the phone rang (「喂？」
                                            // ready), else ConnectAtAnswer: warm Gemini now (silent).
                                            // Active outbound calls never adopt an incoming ring prewarm.
                                            let adopted = if !hello_plan.allow_ring_prewarm {
                                                state.ring_prewarm.cancel(
                                                    &serial,
                                                    None,
                                                    "outbound call",
                                                );
                                                false
                                            } else {
                                                match take_ring_prewarm(
                                                    &state,
                                                    &rt,
                                                    &serial,
                                                    &caller_id,
                                                    sample_rate,
                                                ) {
                                                    Some(pw) => {
                                                        adopt_ring_prewarm(
                                                            pw,
                                                            &sink,
                                                            &state,
                                                            &serial,
                                                            &caller_id,
                                                            &ivr_phase,
                                                            &busy,
                                                            &mut hold_cancel,
                                                            &active_call,
                                                            &mut gemini_transcript,
                                                            &gemini_uplink_tx,
                                                            &mut gemini_audio_open,
                                                            &mut gemini_audio_opened_at,
                                                            &mut gemini_setup_ready,
                                                            &mut gemini_connect_failed,
                                                            &mut gemini_prewarmed,
                                                            &media_call_guard,
                                                        )
                                                        .await
                                                    }
                                                    None => false,
                                                }
                                            };
                                            if !adopted {
                                                start_gemini_connect_at_answer(
                                                    &sink,
                                                    &state,
                                                    &serial,
                                                    &ivr_phase,
                                                    &busy,
                                                    &mut hold_cancel,
                                                    &active_call,
                                                    &rt,
                                                    &gemini_transcript,
                                                    &gemini_uplink_tx,
                                                    sample_rate,
                                                    &gemini_audio_open,
                                                    &gemini_audio_opened_at,
                                                    &gemini_setup_ready,
                                                    &gemini_connect_failed,
                                                    &mut gemini_prewarmed,
                                                    &media_call_guard,
                                                    &caller_id,
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            "local_menu_started" => {
                                // Legacy: the agent owns the greeting; ignore phone-local lifecycle.
                                info!(
                                    "IVR local_menu_started ignored serial={serial} (agent greeting path)"
                                );
                            }
                            "local_menu_done" => {
                                // Do not auto-AI here — the end of the agent's start_ivr_prompt arms that.
                                // Prevents double-AI if stale APK still plays local menu.
                                info!(
                                    "IVR local_menu_done ignored serial={serial} (agent greeting owns auto-AI)"
                                );
                            }
                            "local_menu_failed" => {
                                info!(
                                    "IVR local_menu_failed ignored serial={serial} (agent already streaming)"
                                );
                            }
                            "ringing" => {
                                if let Some(s) = parsed.get("serial").and_then(|v| v.as_str()) {
                                    if !s.is_empty() {
                                        serial = s.to_string();
                                    }
                                }
                                // A blank CID here never wipes the number the hello (or the
                                // blocked number seen while ringing) already gave this call.
                                if let Some(c) = extract_caller_id_from_msg(&parsed)
                                    .filter(|c| !c.trim().is_empty() || caller_id.trim().is_empty())
                                {
                                    caller_id = c;
                                }
                                // Hot-swap boundary: refresh runtime settings at call start.
                                rt = base_state.settings_store.snapshot();
                                state.settings = rt.effective(&base_state.settings);
                                info!("session ringing serial={serial} caller={}", urgent_call::masked(&caller_id));
                                let is_outbound =
                                    hello_is_outbound(&state, &serial, &caller_id, &parsed).await;
                                if !is_outbound && reject_listed_on_line(&rt, &caller_id, &serial) {
                                    call_reject_listed = true;
                                }
                                // Late CID after empty media hello: resolve wait → greet or reject-only.
                                if !caller_id.trim().is_empty()
                                    && cid_wait_pending
                                        .compare_exchange(
                                            true,
                                            false,
                                            Ordering::SeqCst,
                                            Ordering::SeqCst,
                                        )
                                        .is_ok()
                                {
                                    let wait_route = if is_outbound {
                                        CidRoute::Greet
                                    } else {
                                        cid_route_after_wait(
                                            &caller_id,
                                            rt.answer_unknown_callers,
                                            call_reject_listed,
                                        )
                                    };
                                    match wait_route {
                                        CidRoute::Reject => {
                                            info!(
                                                "late hidden CID on ringing serial={serial} caller={:?} → reject-only",
                                                urgent_call::masked(&caller_id)
                                            );
                                            if let Some(c) = active_call.lock().await.as_mut() {
                                                c.push_event("reject_unknown_caller");
                                            }
                                            start_unknown_caller_reject(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &mut hold_cancel,
                                                &active_call,
                                                &prompt_cancel,
                                            )
                                            .await;
                                        }
                                        CidRoute::RejectList => {
                                            info!(
                                                "late CID on ringing serial={serial} caller={:?} → 拒接號碼",
                                                urgent_call::masked(&caller_id)
                                            );
                                            start_reject_list_announce(
                                                &sink,
                                                &state,
                                                &serial,
                                                &caller_id,
                                                &ivr_phase,
                                                &busy,
                                                &mut hold_cancel,
                                                &active_call,
                                                &prompt_cancel,
                                            )
                                            .await;
                                        }
                                        CidRoute::Greet => {
                                            info!(
                                                "late visible CID on ringing serial={serial} caller={:?} → period greet",
                                                urgent_call::masked(&caller_id)
                                            );
                                            start_ivr_prompt(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &active_call,
                                                &arm_auto_ai,
                                                &auto_ai_wake,
                                                &menu_plays,
                                                &prompt_cancel,
                                                true,
                                                Some(dtmf_ignore_until.clone()),
                                            )
                                            .await;
                                            // Active outbound calls never adopt an incoming ring prewarm.
                                            let adopted = if is_outbound {
                                                state.ring_prewarm.cancel(
                                                    &serial,
                                                    None,
                                                    "outbound call",
                                                );
                                                false
                                            } else {
                                                match take_ring_prewarm(
                                                    &state,
                                                    &rt,
                                                    &serial,
                                                    &caller_id,
                                                    sample_rate,
                                                ) {
                                                    Some(pw) => {
                                                        adopt_ring_prewarm(
                                                            pw,
                                                            &sink,
                                                            &state,
                                                            &serial,
                                                            &caller_id,
                                                            &ivr_phase,
                                                            &busy,
                                                            &mut hold_cancel,
                                                            &active_call,
                                                            &mut gemini_transcript,
                                                            &gemini_uplink_tx,
                                                            &mut gemini_audio_open,
                                                            &mut gemini_audio_opened_at,
                                                            &mut gemini_setup_ready,
                                                            &mut gemini_connect_failed,
                                                            &mut gemini_prewarmed,
                                                            &media_call_guard,
                                                        )
                                                        .await
                                                    }
                                                    None => false,
                                                }
                                            };
                                            if !adopted {
                                                start_gemini_connect_at_answer(
                                                    &sink,
                                                    &state,
                                                    &serial,
                                                    &ivr_phase,
                                                    &busy,
                                                    &mut hold_cancel,
                                                    &active_call,
                                                    &rt,
                                                    &gemini_transcript,
                                                    &gemini_uplink_tx,
                                                    sample_rate,
                                                    &gemini_audio_open,
                                                    &gemini_audio_opened_at,
                                                    &gemini_setup_ready,
                                                    &gemini_connect_failed,
                                                    &mut gemini_prewarmed,
                                                    &media_call_guard,
                                                    &caller_id,
                                                );
                                            }
                                        }
                                        other => {
                                            info!(
                                                "late CID ringing ignored serial={serial} route={other:?}"
                                            );
                                        }
                                    }
                                }
                                ensure_active_call(
                                    &state,
                                    &active_call,
                                    &serial,
                                    &caller_id,
                                    sample_rate,
                                    "ringing",
                                )
                                .await;
                                if let Some(c) = active_call.lock().await.as_mut() {
                                    c.set_caller_id(&caller_id);
                                    c.set_status("ringing");
                                    c.push_event("ringing");
                                }
                                if !call_notified {
                                    call_notified = true;
                                    let caller = caller_id.clone();
                                    let ser = serial.clone();
                                    let dedup = base_state.incoming_notify_dedup.clone();
                                    let notice = ring_notice(&state, call_reject_listed, &ser, &caller).await;
                                    if notice == errands::CallNotice::Silent {
                                        info!("call notify skipped (redial try after the first) serial={ser} caller={}", urgent_call::masked(&caller));
                                    } else if dedup.claim(&ser, &caller).await {
                                        let http = state.http.clone();
                                        let settings = {
                                            let snap = base_state.settings_store.snapshot();
                                            snap.effective(&base_state.settings)
                                        };
                                        let dedup_for_task = dedup.clone();
                                        let rejected = call_reject_listed;
                                        let reject_note =
                                            reject_reason_on_line(&rt, &caller_id, &serial).unwrap_or_default();
                                        tokio::spawn(async move {
                                            let ok = if rejected {
                                                notify_rejected_call(
                                                    &http, &settings, &caller, &ser, &reject_note, false,
                                                )
                                                .await
                                            } else if notice == errands::CallNotice::Outgoing {
                                                notify_outgoing_call(&http, &settings, &caller, &ser)
                                                    .await
                                            } else {
                                                notify_incoming_call(&http, &settings, &caller, &ser)
                                                    .await
                                            };
                                            let what = if notice == errands::CallNotice::Outgoing { "outgoing" } else { "incoming" };
                                            if ok {
                                                info!(
                                                    "{what} call notify ok serial={ser} caller={}",
                                                    urgent_call::masked(&caller)
                                                );
                                            } else {
                                                dedup_for_task.release(&ser, &caller).await;
                                                warn!(
                                                    "{what} call notify FAILED serial={ser} caller={} call_notify={} gateway_set={}",
                                                    urgent_call::masked(&caller),
                                                    settings.call_notify,
                                                    !settings.notify_gateway_url.trim().is_empty()
                                                );
                                            }
                                        });
                                    } else {
                                        info!(
                                            "incoming call notify dedup serial={ser} caller={} window_s={}",
                                            urgent_call::masked(&caller),
                                            INCOMING_NOTIFY_DEDUP_WINDOW.as_secs()
                                        );
                                    }
                                }
                                let _ = send_json(
                                    &sink,
                                    json!({"type":"ringing_ack","serial": serial, "caller_id": caller_id}),
                                )
                                .await;
                            }
                            "call_end" => {
                                media_call_guard.invalidate().await;
                                if let Some(c) = parsed.get("caller_id").and_then(|v| v.as_str()) {
                                    if !c.trim().is_empty() {
                                        caller_id = c.to_string();
                                    }
                                }
                                info!("session call_end serial={serial} caller={}", urgent_call::masked(&caller_id));
                                // Capture phase before Done — press-1 AI needs the summary notification.
                                let phase_at_end = *ivr_phase
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                let was_ai_talk = matches!(
                                    phase_at_end,
                                    IvrPhase::TrtcAi | IvrPhase::LiveAi
                                );
                                // Detect hangup → cut Gemini Live API **immediately** (no extra token burn).
                                if was_ai_talk {
                                    info!(
                                        "hangup detected → tear down Gemini Live API serial={serial} phase={phase_at_end:?}"
                                    );
                                }
                                stop_hold_loop(&mut hold_cancel);
                                prompt_cancel.store(true, Ordering::Relaxed);
                                if let Ok(mut g) = gemini_uplink_tx.lock() {
                                    *g = None; // drop uplink sender → session sees None + cancel
                                }
                                if let Some(c) = active_call.lock().await.as_mut() {
                                    c.set_ai_segment_active(false);
                                }
                                deadline = None;
                                hold_started = false;
                                gemini_prewarmed = false;
                                trtc_ai_heard_voice = None;
                                trtc_turn = None;
                                // Stop TRTC AI / Chromium immediately — do not wait for WS drop
                                // (leftover StartAIConversation burns Tencent voice minutes).
                                let trtc_cfg = state.settings.trtc();
                                if trtc_cfg.configured() || trtc_bridge::peek_ai_task_id(&serial).is_some() {
                                    trtc_bridge::make_bridge(&trtc_cfg, state.http.clone())
                                        .leave(&serial, "remote_call_end");
                                }
                                *ivr_phase.lock().unwrap_or_else(|e| e.into_inner()) =
                                    IvrPhase::Done;
                                if let Some(mut call) = active_call.lock().await.take() {
                                    call.set_caller_id(&caller_id);
                                    state
                                        .call_reattach
                                        .forget(&serial, &caller_id, &call.id)
                                        .await;
                                    if !vm_saved && !vm_buf.is_empty() {
                                        vm_saved = true;
                                        let pcm = std::mem::take(&mut vm_buf);
                                        if state.calls.save_voicemail_wav(&mut call, &pcm).is_some()
                                        {
                                            call.push_event("vm_end_hangup");
                                        }
                                    }
                                    maybe_notify_voicemail(&state, &call, &vm_notified);
                                    let appointment_book = call.appointment_book();
                                    let record = state.calls.finalize(call, "ended").await;
                                    save_reject_list_notes(&state, &record);
                                    // Press-1 Gemini/TRTC: notification with full transcript (brief delay for last STT).
                                    // A persona-mode AI-fail hangup already set phase=Done, but the owner
                                    // still needs a card (else a silent few-second call leaves no notification).
                                    // AI callbacks also finalize state here even if dropped before unmute (400ms settle).
                                    if should_run_ai_call_summary(was_ai_talk, &record.events) {
                                        spawn_ai_talk_tg_summary(
                                            &state,
                                            &record,
                                            gemini_transcript.clone(),
                                            appointment_book,
                                        );
                                    }
                                }
                                clear_call_sim(&serial);
                                caller_id.clear();
                                call_notified = false;
                                call_reject_listed = false;
                                vm_saved = false;
                                vm_notified.store(false, Ordering::Relaxed);
                                vm_buf.clear();
                                let _ = send_json(
                                    &sink,
                                    json!({"type":"call_end_ack","serial": serial}),
                                )
                                .await;
                            }
                            "ping" => {
                                let _ = send_json(&sink, json!({"type":"pong"})).await;
                            }
                            "end_utterance" => {
                                if state.settings.agent_mode.is_transfer() {
                                    info!(
                                        "transfer mode end_utterance serial={serial} uplink_bytes={uplink_bytes} (no STT)"
                                    );
                                    continue;
                                }
                                let pcm = if vad.has_buffered_audio() {
                                    vad.flush()
                                } else {
                                    Vec::new()
                                };
                                if !pcm.is_empty() {
                                    if let Ok(_guard) = busy.try_lock() {
                                        if let Err(e) = handle_utterance(
                                            &sink,
                                            &state,
                                            &pcm,
                                            &mut history,
                                            sample_rate,
                                            &active_call,
                                        )
                                        .await
                                        {
                                            error!("utterance error: {e:#}");
                                            let _ = send_json(
                                                &sink,
                                                json!({"type":"error","message":"utterance error"}),
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                            "reset" => {
                                media_call_guard.invalidate().await;
                                history.clear();
                                vad.reset();
                                remote_gate.reset();
                                dtmf.reset();
                                stop_hold_loop(&mut hold_cancel);
                                prompt_cancel.store(true, Ordering::Relaxed);
                                hold_started = false;
                                gemini_prewarmed = false;
                                if let Ok(mut opened_at) = gemini_audio_opened_at.lock() {
                                    *opened_at = Some(tokio::time::Instant::now());
                                }
                                gemini_audio_open.store(true, Ordering::Relaxed);
                                gemini_setup_ready.store(false, Ordering::Relaxed);
                                gemini_connect_failed.store(false, Ordering::Relaxed);
                                arm_auto_ai.store(false, Ordering::Relaxed);
                                menu_plays.store(0, Ordering::Relaxed);
                                *ivr_phase.lock().unwrap_or_else(|e| e.into_inner()) =
                                    IvrPhase::WaitDtmf;
                                uplink_bytes = 0;
                                deadline = None;
                                vm_buf.clear();
                                vm_rec_started = None;
                                vm_last_voice = None;
                                vm_voiced = false;
                                vm_saved = false;
                                vm_notified.store(false, Ordering::Relaxed);
                                arm_vm_confirm.store(false, Ordering::Relaxed);
                                arm_vm_contact_readback.store(false, Ordering::Relaxed);
                                *vm_contact_candidate.lock().unwrap_or_else(|e| e.into_inner()) =
                                    None;
                                let _ = send_json(&sink, json!({"type":"reset_ack"})).await;
                            }
                            // The phone turned a second call away during this one.
                            "call_waiting_rejected" => {
                                on_call_waiting_rejected(&state, &serial, &parsed);
                            }
                            // Whitelist urgent call: how the phone's second call goes.
                            "add_call_result" => {
                                let waited = urgent_call::deliver(&parsed);
                                info!(
                                    "add_call_result serial={serial} id={} status={} reason={} waited={waited}",
                                    parsed.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                                    parsed.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                                    parsed.get("reason").and_then(|v| v.as_str()).unwrap_or("")
                                );
                            }
                            other => {
                                let _ = send_json(
                                    &sink,
                                    json!({"type":"error","message": format!("unknown type {other}")}),
                                )
                                .await;
                            }
                        }
                    }
                    Message::Binary(bin) => {
                        uplink_bytes += bin.len() as u64;
                        if let Some(c) = active_call.lock().await.as_mut() {
                            c.write_uplink(&bin);
                        }
                        if state.settings.agent_mode.is_transfer() {
                            let phase = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
                            match phase {
                                // Accept DTMF during menu (barge-in) and WaitDtmf.
                                // High-conf single-block emit covers short cellular tones.
                                IvrPhase::WaitDtmf | IvrPhase::PlayPrompt => {
                                    // When media bridge is live, push cellular uplink into TRTC room.
                                    if trtc_bridge::room_media_live(
                                        &trtc_bridge::SoftTrtcBridge::room_id_for(&serial),
                                    ) {
                                        let trtc = state.settings.trtc();
                                        let room =
                                            trtc_bridge::SoftTrtcBridge::room_id_for(&serial);
                                        let http = state.http.clone();
                                        let pcm = bin.clone();
                                        let sr = sample_rate;
                                        tokio::spawn(async move {
                                            let _ = trtc_bridge::push_uplink_http(
                                                &trtc, &http, &room, &pcm, sr,
                                            )
                                            .await;
                                        });
                                    }
                                    // NEVER accept DTMF during PlayPrompt — menu speech
                                    // ("press 1") falsely triggers wait/AI.
                                    if matches!(phase, IvrPhase::PlayPrompt) {
                                        dtmf.reset();
                                        continue;
                                    }
                                    // Post-menu grace: ignore residual echo of menu speech.
                                    {
                                        let mut guard = dtmf_ignore_until
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner());
                                        if let Some(until) = *guard {
                                            if tokio::time::Instant::now() < until {
                                                dtmf.reset();
                                                continue;
                                            }
                                            *guard = None;
                                        }
                                    }
                                    // WaitDtmf: remote keypad only (drop handset echo of leftover audio).
                                    let sidetone = {
                                        let g = active_call.lock().await;
                                        g.as_ref()
                                            .map(|c| c.uplink_is_sidetone(&bin))
                                            .unwrap_or(false)
                                    };
                                    if sidetone {
                                        continue;
                                    }
                                    if let Some(hit) = dtmf.feed(&bin) {
                                        if !hit.is_product_key() {
                                            info!(
                                                "IVR reject non-product DTMF digit={} conf={:.3} purity={:.3} serial={serial}",
                                                hit.digit, hit.confidence, hit.purity
                                            );
                                            continue;
                                        }
                                        let accept = hit.menu_barge_in_ok();
                                        if !accept {
                                            info!(
                                                "IVR reject structure DTMF digit={} conf={:.3} purity={:.3} high_margin={:.2} phase={phase:?} serial={serial}",
                                                hit.digit, hit.confidence, hit.purity, hit.high_margin
                                            );
                                            continue;
                                        }
                                        let hold_key = state.settings.transfer_dtmf_trigger;
                                        let vm_key = state.settings.ivr_dtmf_voicemail;
                                        let end_keys = state.settings.ivr_dtmf_end.as_str();
                                        let d = hit.digit.to_ascii_uppercase();
                                        // Product: 1=AI, 2=voicemail only. End keys off by default (empty).
                                        let is_action = d == hold_key.to_ascii_uppercase()
                                            || d == vm_key.to_ascii_uppercase()
                                            || d == '2'
                                            || (!end_keys.is_empty()
                                                && end_keys.to_ascii_uppercase().contains(d));
                                        if !is_action {
                                            info!(
                                                "IVR ignore non-action DTMF digit={} conf={:.3} serial={serial}",
                                                hit.digit, hit.confidence
                                            );
                                            continue;
                                        }
                                        // Real remote press → stop prompt, then route.
                                        prompt_cancel.store(true, Ordering::Relaxed);
                                        deadline = None;
                                        info!(
                                            "IVR DTMF digit={} conf={:.3} purity={:.3} high_margin={:.2} phase={phase:?} remote_only serial={serial}",
                                            hit.digit, hit.confidence, hit.purity, hit.high_margin
                                        );
                                        agent_dbg(
                                            "main.rs:dtmf", "dtmf_received", "H4",
                                            json!({
                                                "serial": serial,
                                                "digit": hit.digit.to_string(),
                                                "confidence": hit.confidence,
                                                "purity": hit.purity,
                                                "high_margin": hit.high_margin,
                                                "phase": format!("{phase:?}"),
                                            }),
                                        );
                                        let _ = send_json(
                                            &sink,
                                            json!({
                                                "type": "dtmf",
                                                "digit": hit.digit.to_string(),
                                                "confidence": hit.confidence,
                                                "purity": hit.purity,
                                                "high_margin": hit.high_margin,
                                            }),
                                        )
                                        .await;
                                        if d == hold_key.to_ascii_uppercase() {
                                            remote_gate.reset();
                                            // Manual DTMF 1 still starts AI (same as auto path).
                                            enter_gemini_or_voicemail(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &mut hold_started,
                                                &mut hold_cancel,
                                                &active_call,
                                                &rt,
                                                &gemini_transcript,
                                                &gemini_uplink_tx,
                                                sample_rate,
                                                true, // optional connect-wait if user pressed 1
                                                &gemini_audio_open,
                                                &gemini_audio_opened_at,
                                                &gemini_setup_ready,
                                                &gemini_connect_failed,
                                                &mut gemini_prewarmed,
                                                &media_call_guard,
                                            );
                                        } else if d == vm_key.to_ascii_uppercase() || d == '2' {
                                            // Voicemail = 2 only (never 9 — false 9 stole press-1).
                                            deadline = None;
                                            prompt_cancel.store(true, Ordering::Relaxed);
                                            vm_buf.clear();
                                            vm_rec_started = None;
                                            vm_last_voice = None;
                                            vm_voiced = false;
                                            // Exit TRTC AI/menu path before local voicemail.
                                            stop_hold_loop(&mut hold_cancel);
                                            let trtc_cfg = state.settings.trtc();
                                            if trtc_cfg.configured() || trtc_bridge::peek_ai_task_id(&serial).is_some() {
                                                trtc_bridge::make_bridge(&trtc_cfg, state.http.clone())
                                                    .leave(&serial, "dtmf_voicemail");
                                            }
                                            start_voicemail_intro(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &active_call,
                                            );
                                        } else if !end_keys.is_empty()
                                            && end_keys.to_ascii_uppercase().contains(d)
                                        {
                                            // Optional only if IVR_DTMF_END set (default empty).
                                            deadline = None;
                                            prompt_cancel.store(true, Ordering::Relaxed);
                                            start_ivr_stub_hangup(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &mut hold_cancel,
                                                &active_call,
                                                state.settings.ivr_goodbye_text.clone(),
                                                "ivr_dtmf_end",
                                                false,
                                                true,
                                                &rt,
                                            );
                                        } else {
                                            info!(
                                                "IVR ignore digit={} (hold={} vm={} end={:?}) serial={serial}",
                                                hit.digit, hold_key, vm_key, end_keys
                                            );
                                        }
                                    }
                                }
                                IvrPhase::PlayHoldMusic | IvrPhase::TrtcAi | IvrPhase::LiveAi => {
                                    // Live: remote uplink only. Wait/AI inject must not look like DTMF 0/#
                                    // (seen live: wait clip mid-play → false digit=0 → goodbye clip).
                                    let room =
                                        trtc_bridge::SoftTrtcBridge::room_id_for(&serial);
                                    let (tts_out, sidetone, echo, overlap) = {
                                        let mut g = active_call.lock().await;
                                        match g.as_mut() {
                                            Some(c) => (
                                                c.tts_outbound_active(),
                                                c.uplink_is_sidetone(&bin),
                                                c.uplink_is_echo(&bin),
                                                c.barge_in_on(),
                                            ),
                                            None => (false, false, false, false),
                                        }
                                    };
                                    // LiveAi: no DTMF end keys (0/#). Unwanted callers hang up the phone;
                                    // false 0 from wait/AI audio used to force a goodbye.
                                    let rms = pcm_rms_s16le(&bin);
                                    let thr = state.settings.vad_rms_threshold;
                                    let caller_voice = !sidetone && rms >= thr;
                                    let skip_uplink_to_trtc = sidetone;
                                    // Gemini: only remote speech. Never feed wait/AI inject echo
                                    // (was activityStart on wait playout → empty AI turns).
                                    // Echo of our own voice (incl. the tail after playout stops) is
                                    // replaced by silence, not dropped, so Gemini's local VAD keeps
                                    // counting end-of-turn silence and never hears itself. A line
                                    // proven echo-free (barge-in on) passes the caller while the AI
                                    // talks, so they can cut in.
                                    if matches!(phase, IvrPhase::LiveAi) {
                                        let frame = if sidetone || (!overlap && (tts_out || echo)) {
                                            vec![0u8; bin.len()]
                                        } else {
                                            bin.to_vec()
                                        };
                                        if let Ok(g) = gemini_uplink_tx.lock() {
                                            if let Some(tx) = g.as_ref() {
                                                let _ = tx.send(frame);
                                            }
                                        }
                                    }
                                    if matches!(phase, IvrPhase::TrtcAi) && caller_voice {
                                        if let Some(ref m) = trtc_turn {
                                            let now = now_unix_ms();
                                            m.last_user_voice_ms.store(now, Ordering::Relaxed);
                                            // New speech cancels pending "turn end".
                                            m.user_turn_end_ms.store(0, Ordering::Relaxed);
                                            m.silence_after_voice_ms.store(0, Ordering::Relaxed);
                                        }
                                        if trtc_ai_heard_voice
                                            .as_ref()
                                            .map(|f| !f.load(Ordering::Relaxed))
                                            .unwrap_or(false)
                                        {
                                            if remote_gate.feed(&bin) {
                                                if let Some(flag) = trtc_ai_heard_voice.as_ref() {
                                                    flag.store(true, Ordering::Relaxed);
                                                }
                                                info!(
                                                    "TrtcAi remote voice detected serial={serial} rms={rms:.0} — continue AI talk"
                                                );
                                            }
                                        }
                                    } else if matches!(phase, IvrPhase::TrtcAi)
                                        && !sidetone
                                        && trtc_turn.is_some()
                                    {
                                        // Silence after caller speech → mark turn end for latency.
                                        if let Some(ref m) = trtc_turn {
                                            let last = m.last_user_voice_ms.load(Ordering::Relaxed);
                                            if last > 0
                                                && m.user_turn_end_ms.load(Ordering::Relaxed) == 0
                                            {
                                                // ~20 ms phone frames; accumulate quiet time.
                                                let acc = m
                                                    .silence_after_voice_ms
                                                    .fetch_add(20, Ordering::Relaxed)
                                                    + 20;
                                                // End-of-utterance after ~220 ms quiet (low-latency turn).
                                                if acc >= 220 {
                                                    let end = now_unix_ms();
                                                    m.user_turn_end_ms
                                                        .store(end, Ordering::Relaxed);
                                                    info!(
                                                        "TrtcAi caller_turn_end serial={serial} silence_ms={acc} — waiting AI reply"
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    if trtc_bridge::room_media_live(&room) && !skip_uplink_to_trtc {
                                        let trtc = state.settings.trtc();
                                        let http = state.http.clone();
                                        let pcm = bin.clone();
                                        let sr = sample_rate;
                                        tokio::spawn(async move {
                                            let _ = trtc_bridge::push_uplink_http(
                                                &trtc, &http, &room, &pcm, sr,
                                            )
                                            .await;
                                        });
                                    } else if !skip_uplink_to_trtc
                                        && state.settings.trtc().configured()
                                        && uplink_bytes % 320_000 < bin.len() as u64
                                    {
                                        tracing::debug!(
                                            "IVR trtc uplink pending serial={serial} phase={phase:?} live=false bytes={uplink_bytes}"
                                        );
                                    }
                                }
                                // Intro/beep playing; recording starts when phase flips.
                                IvrPhase::VoicemailIntro => {}
                                IvrPhase::VoicemailRecord => {
                                    let now = tokio::time::Instant::now();
                                    if vm_rec_started.is_none() {
                                        vm_rec_started = Some(now);
                                        vm_last_voice = Some(now);
                                        vm_voiced = false;
                                        vm_buf.clear();
                                        dtmf.reset();
                                        info!(
                                            "VM record begin serial={serial} max_ms={} silence_ms={} rms_thr={}",
                                            state.settings.vm_max_ms,
                                            state.settings.vm_silence_ms,
                                            state.settings.vm_silence_rms
                                        );
                                    }
                                    // Never put system TTS/beep sidetone into _vm.wav
                                    // (that sounded like "my voice was replaced by another voice").
                                    let sidetone = {
                                        let g = active_call.lock().await;
                                        g.as_ref()
                                            .map(|c| c.uplink_is_sidetone(&bin))
                                            .unwrap_or(false)
                                    };
                                    let cap = state.settings.vm_max_ms as usize / 1000
                                        * sample_rate as usize
                                        * 2
                                        + sample_rate as usize * 2;
                                    if !sidetone && vm_buf.len() < cap {
                                        let room = cap - vm_buf.len();
                                        let take = room.min(bin.len()) & !1usize;
                                        vm_buf.extend_from_slice(&bin[..take]);
                                    }
                                    if frame_rms(&bin) >= state.settings.vm_silence_rms {
                                        vm_last_voice = Some(now);
                                        vm_voiced = true;
                                    }
                                    let mut end_reason: Option<&'static str> = None;
                                    if let Some(hit) = dtmf.feed(&bin) {
                                        if hit.digit == '#' {
                                            end_reason = Some("vm_end_hash");
                                        }
                                    }
                                    let started = vm_rec_started.unwrap_or(now);
                                    if end_reason.is_none()
                                        && now.duration_since(started).as_millis() as u64
                                            >= u64::from(state.settings.vm_max_ms)
                                    {
                                        end_reason = Some("vm_end_max");
                                    }
                                    if end_reason.is_none() {
                                        let last = vm_last_voice.unwrap_or(started);
                                        if now.duration_since(last).as_millis() as u64
                                            >= u64::from(state.settings.vm_silence_ms)
                                        {
                                            end_reason = Some("vm_end_silence");
                                        }
                                    }
                                    if let Some(reason) = end_reason {
                                        if reason == "vm_end_hash" {
                                            // Trim the trailing `#` tone from the message.
                                            let trim = (sample_rate as usize * 2 * 400 / 1000)
                                                & !1usize;
                                            let keep = vm_buf.len().saturating_sub(trim);
                                            vm_buf.truncate(keep);
                                        }
                                        let pcm = std::mem::take(&mut vm_buf);
                                        vm_saved = true;
                                        vm_rec_started = None;
                                        info!(
                                            "VM record end serial={serial} reason={reason} bytes={} voiced={vm_voiced}",
                                            pcm.len()
                                        );
                                        {
                                            let mut g = active_call.lock().await;
                                            if let Some(c) = g.as_mut() {
                                                if state
                                                    .calls
                                                    .save_voicemail_wav(c, &pcm)
                                                    .is_some()
                                                {
                                                    c.push_event("voicemail_saved");
                                                }
                                                c.push_event(reason);
                                            }
                                        }
                                        prompt_cancel.store(false, Ordering::Relaxed);
                                        // After # / silence / max: goodbye + hangup immediately.
                                        // (Old path waited on callback-confirm DTMF, so the line stayed open after the goodbye.)
                                        finish_voicemail(
                                            &sink,
                                            &state,
                                            &serial,
                                            &ivr_phase,
                                            &busy,
                                            &active_call,
                                            &vm_notified,
                                            &rt,
                                        );
                                    }
                                }
                                IvrPhase::VoicemailContactProbe => {}
                                IvrPhase::VoicemailContactReadback => {
                                    if let Some(hit) = dtmf.feed(&bin) {
                                        let d = hit.digit.to_ascii_uppercase();
                                        if d == '1' {
                                            deadline = None;
                                            prompt_cancel.store(true, Ordering::Relaxed);
                                            let candidate = vm_contact_candidate
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner())
                                                .clone();
                                            if let Some(c) = candidate {
                                                info!(
                                                    "VM contact confirmed serial={serial} method={} contact={}",
                                                    c.method,
                                                    log_text::Said(&c.contact)
                                                );
                                                {
                                                    let mut g = active_call.lock().await;
                                                    if let Some(call) = g.as_mut() {
                                                        call.set_reply(&c.method, &c.contact, true);
                                                        call.push_event("vm_reply_contact_confirmed");
                                                    }
                                                }
                                                start_vm_goodbye(
                                                    &sink,
                                                    &state,
                                                    &serial,
                                                    &ivr_phase,
                                                    &busy,
                                                    &active_call,
                                                    &vm_notified,
                                                    true,
                                                    &rt,
                                                );
                                            } else {
                                                start_vm_caller_id_confirm(
                                                    &sink,
                                                    &state,
                                                    &serial,
                                                    &ivr_phase,
                                                    &busy,
                                                    &active_call,
                                                    &arm_vm_confirm,
                                                    &prompt_cancel,
                                                );
                                            }
                                        } else if d == '2' {
                                            deadline = None;
                                            prompt_cancel.store(true, Ordering::Relaxed);
                                            *vm_contact_candidate
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner()) = None;
                                            info!("VM contact rejected serial={serial} → caller-id confirm");
                                            {
                                                let mut g = active_call.lock().await;
                                                if let Some(call) = g.as_mut() {
                                                    call.push_event("vm_contact_rejected");
                                                }
                                            }
                                            start_vm_caller_id_confirm(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &active_call,
                                                &arm_vm_confirm,
                                                &prompt_cancel,
                                            );
                                        }
                                    }
                                }
                                IvrPhase::VoicemailConfirm => {
                                    if let Some(hit) = dtmf.feed(&bin) {
                                        let d = hit.digit.to_ascii_uppercase();
                                        let end_keys =
                                            state.settings.ivr_dtmf_end.to_ascii_uppercase();
                                        if d == '1' {
                                            deadline = None;
                                            prompt_cancel.store(true, Ordering::Relaxed);
                                            info!(
                                                "VM callback confirmed serial={serial} caller={:?}",
                                                urgent_call::masked(&caller_id)
                                            );
                                            {
                                                let mut g = active_call.lock().await;
                                                if let Some(c) = g.as_mut() {
                                                    c.set_reply("phone", &caller_id, true);
                                                    c.push_event("vm_reply_phone_confirmed");
                                                }
                                            }
                                            start_vm_goodbye(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &active_call,
                                                &vm_notified,
                                                true,
                                                &rt,
                                            );
                                        } else if end_keys.contains(d) {
                                            deadline = None;
                                            prompt_cancel.store(true, Ordering::Relaxed);
                                            {
                                                let mut g = active_call.lock().await;
                                                if let Some(c) = g.as_mut() {
                                                    c.set_reply("none", "", false);
                                                    c.push_event("vm_no_reply");
                                                }
                                            }
                                            start_vm_goodbye(
                                                &sink,
                                                &state,
                                                &serial,
                                                &ivr_phase,
                                                &busy,
                                                &active_call,
                                                &vm_notified,
                                                false,
                                                &rt,
                                            );
                                        }
                                    }
                                }
                                IvrPhase::Goodbye | IvrPhase::Done => {}
                            }
                            continue;
                        }
                        if let Some(utterance) = vad.feed(&bin) {
                            if let Ok(_guard) = busy.try_lock() {
                                if let Err(e) = handle_utterance(
                                    &sink,
                                    &state,
                                    &utterance,
                                    &mut history,
                                    sample_rate,
                                    &active_call,
                                )
                                .await
                                {
                                    error!("utterance error: {e:#}");
                                    let _ = send_json(
                                        &sink,
                                        json!({"type":"error","message":"utterance error"}),
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Ping(p) => {
                        let mut s = sink.lock().await;
                        let _ = s.send(Message::Pong(p)).await;
                    }
                    _ => {}
                }
            }
            _ = &mut timeout_sleep => {
                let kind = deadline.map(|(_, k)| k);
                deadline = None;
                match kind {
                    Some(DeadlineKind::IvrReprompt) => {
                        // Legacy deadline kind: product never re-menus — re-arm auto AI.
                        let phase = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
                        if phase != IvrPhase::WaitDtmf
                            || !state.settings.agent_mode.is_transfer()
                            || hold_started
                        {
                            continue;
                        }
                        info!(
                            "IVR WaitDtmf legacy timeout → auto-AI serial={serial}"
                        );
                        arm_auto_ai.store(true, Ordering::Relaxed);
                    }
                    Some(DeadlineKind::VmContactReadback) => {
                        let phase = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
                        if phase == IvrPhase::VoicemailContactReadback {
                            info!("VM contact readback timeout → caller-id confirm serial={serial}");
                            prompt_cancel.store(true, Ordering::Relaxed);
                            *vm_contact_candidate.lock().unwrap_or_else(|e| e.into_inner()) = None;
                            {
                                let mut g = active_call.lock().await;
                                if let Some(c) = g.as_mut() {
                                    c.push_event("vm_contact_readback_timeout");
                                }
                            }
                            start_vm_caller_id_confirm(
                                &sink,
                                &state,
                                &serial,
                                &ivr_phase,
                                &busy,
                                &active_call,
                                &arm_vm_confirm,
                                &prompt_cancel,
                            );
                        }
                    }
                    Some(DeadlineKind::VmConfirm) => {
                        let phase = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
                        if phase == IvrPhase::VoicemailConfirm {
                            info!("VM confirm timeout → goodbye serial={serial}");
                            prompt_cancel.store(true, Ordering::Relaxed);
                            {
                                let mut g = active_call.lock().await;
                                if let Some(c) = g.as_mut() {
                                    c.set_reply("none", "", false);
                                    c.push_event("vm_confirm_timeout");
                                }
                            }
                            start_vm_goodbye(
                                &sink,
                                &state,
                                &serial,
                                &ivr_phase,
                                &busy,
                                &active_call,
                                &vm_notified,
                                false,
                                &rt,
                            );
                        }
                    }
                    None => {}
                }
            }
        }
    }
    // WS drop / client gone — same as call_end: cancel Gemini Live ASAP.
    media_call_guard.invalidate().await;
    let phase_before_teardown = *ivr_phase.lock().unwrap_or_else(|e| e.into_inner());
    if matches!(phase_before_teardown, IvrPhase::LiveAi | IvrPhase::TrtcAi) {
        info!(
            "client disconnect → tear down Gemini/Live API serial={serial} phase={phase_before_teardown:?}"
        );
    }
    stop_hold_loop(&mut hold_cancel);
    prompt_cancel.store(true, Ordering::Relaxed);
    if let Ok(mut g) = gemini_uplink_tx.lock() {
        *g = None;
    }
    deadline = None;
    let phase = phase_before_teardown;
    let trtc_ai_ended = matches!(phase, IvrPhase::TrtcAi | IvrPhase::LiveAi);
    // Always tear down TRTC room/AI task on disconnect — menu may have opened a
    // sidecar session / StartAIConversation; skipping leave left a "task already exists" error
    // and stale room_media_live → silent ServerPush next call.
    let trtc_cfg = state.settings.trtc();
    if state.settings.agent_mode.is_transfer()
        && (trtc_cfg.configured() || trtc_bridge::peek_ai_task_id(&serial).is_some())
        && matches!(
            phase,
            IvrPhase::PlayPrompt
                | IvrPhase::WaitDtmf
                | IvrPhase::PlayHoldMusic
                | IvrPhase::TrtcAi
                | IvrPhase::LiveAi
        )
    {
        trtc_bridge::make_bridge(&trtc_cfg, state.http.clone())
            .leave(&serial, "ivr_disconnect");
    } else if trtc_ai_ended && (trtc_cfg.configured() || trtc_bridge::peek_ai_task_id(&serial).is_some()) {
        trtc_bridge::make_bridge(&trtc_cfg, state.http.clone())
            .leave(&serial, "trtc_ai_disconnect");
    }
    if let Some(mut call) = active_call.lock().await.take() {
        if !vm_saved && !vm_buf.is_empty() {
            let pcm = std::mem::take(&mut vm_buf);
            if state.calls.save_voicemail_wav(&mut call, &pcm).is_some() {
                call.push_event("vm_end_hangup");
            }
        }
        // WS drop without call_end (Android reconnect / USB blip): park so the
        // next hello for this device+number reuses the same call_id.
        maybe_notify_voicemail(&state, &call, &vm_notified);
        let call_id = call.id.clone();
        let ser = serial.clone();
        let caller = caller_id.clone();
        state.call_reattach.park(&ser, &caller, call).await;
        let registry = state.call_reattach.clone();
        let store = state.calls.clone();
        let transcript = gemini_transcript.clone();
        let notify_state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CALL_REATTACH_WINDOW).await;
            if let Some(call) = registry.take_parked_by_id(&call_id).await {
                info!(
                    "call reattach window expired id={call_id} serial={ser} — finalize"
                );
                let appointment_book = call.appointment_book();
                let record = store.finalize(call, "ended").await;
                save_reject_list_notes(&notify_state, &record);
                if should_run_ai_call_summary(trtc_ai_ended, &record.events) {
                    spawn_ai_talk_tg_summary(&notify_state, &record, transcript, appointment_book);
                }
            }
        });
    }
    if state.settings.agent_mode.is_transfer()
        && !matches!(
            phase,
            IvrPhase::PlayHoldMusic
                | IvrPhase::Goodbye
                | IvrPhase::Done
                // Hanging up mid-message / mid-confirm is a normal voicemail end.
                | IvrPhase::VoicemailRecord
                | IvrPhase::VoicemailContactProbe
                | IvrPhase::VoicemailContactReadback
                | IvrPhase::VoicemailConfirm
                // Caller hanging up mid-conversation is a normal live-AI end.
                | IvrPhase::LiveAi
                | IvrPhase::TrtcAi
        )
    {
        warn!(
            "client disconnected before IVR complete serial={serial} phase={phase:?} uplink_bytes={}",
            uplink_bytes
        );
    }
    cid_wait_pending.store(false, Ordering::SeqCst);
    state.media_play.release(&serial).await;
    info!(
        "client disconnected serial={serial} caller={} uplink_bytes={uplink_bytes} phase={phase:?} hold_started={hold_started}",
        urgent_call::masked(&caller_id)
    );
    let pacing_started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    agent_dbg(
        "main.rs:disconnect",
        "client_disconnected",
        "A",
        json!({
            "serial": serial,
            "uplink_bytes": uplink_bytes,
            "phase": format!("{phase:?}"),
            "hold_started": hold_started,
        }),
    );
}

/// Country of the SIM each phone's current call came in on (`sim_country` on
/// `incoming` / `hello`, phone app 0.2.5+; ISO 3166 alpha-2, lowercase). Decides
/// whether the AI waits for the carrier's international-call warning.
fn call_sim_countries() -> &'static std::sync::Mutex<HashMap<String, String>> {
    static M: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
        std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

/// The `sim_country` field as a lowercase ISO code; `None` when it is missing (older
/// app) or not two letters (the phone could not tell).
fn sim_country_from_msg(parsed: &Value) -> Option<String> {
    parsed
        .get("sim_country")?
        .as_str()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_lowercase()))
}

/// Every `incoming` / `hello` replaces what is known about the phone's SIM, so a
/// value never outlives its call (nor a downgrade to an app that does not send it).
/// The last SIM a phone reported is also kept apart ([`phone_region::last_sim`]).
fn is_incoming_sim_msg(parsed: &Value) -> bool {
    parsed.get("type").and_then(Value::as_str) == Some("incoming")
}

fn note_call_sim_country(serial: &str, parsed: &Value) {
    let sim = sim_country_from_msg(parsed);
    let key = canonical_device_serial(serial);
    if let Some(iso) = sim.as_deref() {
        phone_region::note_sim(&key, iso);
    }
    let mut m = call_sim_countries().lock().unwrap_or_else(|e| e.into_inner());
    match sim {
        Some(iso) => {
            m.insert(key, iso);
        }
        None if is_incoming_sim_msg(parsed) => {
            m.remove(&key);
        }
        None => {}
    }
}

fn call_sim_country(serial: &str) -> Option<String> {
    call_sim_countries()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&canonical_device_serial(serial))
        .cloned()
}

fn whitelist_exempts_reject(
    rt: &RuntimeSettings,
    caller: &str,
    sim: Option<&str>,
    contacts_store: Option<&contacts::ContactStore>,
) -> bool {
    if caller.trim().is_empty()
        || !matches!(rt.whitelist_mode.as_str(), "direct" | "emergency" | "urgent")
    {
        return false;
    }
    match contacts_store {
        Some(store) => store.is_whitelist(caller, sim).is_some(),
        None => contacts::is_whitelist(caller, sim).is_some(),
    }
}

fn reject_listed_on_sim(
    rt: &RuntimeSettings,
    caller: &str,
    sim: Option<&str>,
    contacts_store: Option<&contacts::ContactStore>,
) -> bool {
    if whitelist_exempts_reject(rt, caller, sim, contacts_store) {
        return false;
    }
    settings_store::reject_number_hit_on(rt.pending_rejects(), caller, sim)
}

fn reject_reason_on_sim(
    rt: &RuntimeSettings,
    caller: &str,
    sim: Option<&str>,
    contacts_store: Option<&contacts::ContactStore>,
) -> Option<String> {
    if whitelist_exempts_reject(rt, caller, sim, contacts_store) {
        return None;
    }
    settings_store::reject_reason_on(rt.pending_rejects(), caller, sim)
}

/// The caller is on the blocked-numbers list (or the Contacts Blacklist) as the
/// SIM of the phone's current call reads the number: one without a country code
/// is dialled in that SIM's country ([`settings_store::reject_number_hit_on`]).
fn reject_listed_on_line(rt: &RuntimeSettings, caller: &str, serial: &str) -> bool {
    reject_listed_on_sim(rt, caller, call_sim_country(serial).as_deref(), None)
}

/// Why the caller is blocked, read as in [`reject_listed_on_line`].
fn reject_reason_on_line(rt: &RuntimeSettings, caller: &str, serial: &str) -> Option<String> {
    reject_reason_on_sim(rt, caller, call_sim_country(serial).as_deref(), None)
}

/// Android subscription id of the SIM each phone's current call came in on
/// (`sim_sub_id` on `incoming` / `hello`, newer phone apps): texts and call-backs
/// for that call go out on the same SIM.
fn call_sim_sub_ids() -> &'static std::sync::Mutex<HashMap<String, i64>> {
    static M: std::sync::OnceLock<std::sync::Mutex<HashMap<String, i64>>> =
        std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

/// The `sim_sub_id` field; None when it is missing (older app) or not a valid id
/// (Android's "no subscription" is -1).
fn sim_sub_id_from_msg(parsed: &Value) -> Option<i64> {
    parsed.get("sim_sub_id")?.as_i64().filter(|id| *id >= 0)
}

/// Like [`note_call_sim_country`]: every `incoming` / `hello` replaces it, so it
/// never outlives its call.
fn note_call_sim_sub_id(serial: &str, parsed: &Value) {
    let key = canonical_device_serial(serial);
    let mut m = call_sim_sub_ids().lock().unwrap_or_else(|e| e.into_inner());
    match sim_sub_id_from_msg(parsed) {
        Some(id) => {
            m.insert(key, id);
        }
        None if is_incoming_sim_msg(parsed) => {
            m.remove(&key);
        }
        None => {}
    }
}

fn clear_call_sim(serial: &str) {
    let key = canonical_device_serial(serial);
    call_sim_countries()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
    call_sim_sub_ids()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&key);
}

/// The SIM (subscription id) the phone's current call came in on.
pub(crate) fn call_sim_sub_id(serial: &str) -> Option<i64> {
    call_sim_sub_ids()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&canonical_device_serial(serial))
        .copied()
}

/// The SIM fields of an `incoming` / `hello` (country and subscription id).
fn note_call_sim(serial: &str, parsed: &Value) {
    note_call_sim_country(serial, parsed);
    note_call_sim_sub_id(serial, parsed);
    // Kept for errands: which SIM of this phone is of which country.
    errands::note_call_sim(
        &canonical_device_serial(serial),
        sim_sub_id_from_msg(parsed),
        sim_country_from_msg(parsed).as_deref(),
    );
}

/// How long a second report of the same turned-away call counts as that call.
const CALL_WAITING_DEDUP: Duration = Duration::from_secs(5);

/// First report of this phone turning this number away (the control and the
/// media connection may both carry it).
fn first_call_waiting_report(serial: &str, caller: &str) -> bool {
    static SEEN: std::sync::OnceLock<std::sync::Mutex<HashMap<String, std::time::Instant>>> =
        std::sync::OnceLock::new();
    let key = format!("{}\u{1}{}", canonical_device_serial(serial), caller.trim());
    let now = std::time::Instant::now();
    let mut seen = SEEN
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    seen.retain(|_, at| now.duration_since(*at) < CALL_WAITING_DEDUP);
    if seen.contains_key(&key) {
        return false;
    }
    seen.insert(key, now);
    true
}

/// The web record of a call turned away during another call: fixed text, no LLM.
fn call_waiting_summary(caller: &str, sim_country: Option<&str>) -> String {
    let p = locale::notify_phrases();
    let sep = p.sep;
    let number = display_caller_id(caller);
    let sim = sim_country
        .map(|c| format!(" (SIM {})", c.to_ascii_uppercase()))
        .unwrap_or_default();
    format!(
        "{}{sep}{number}\n{}{sep}{}\n{}{sep}{}",
        p.s_caller,
        p.s_matter,
        locale::notify_zh_or(
            &format!("通話中插播，手機忙線拒接{sim}"),
            &format!("came in during another call; the phone turned it away as busy{sim}"),
        ),
        p.s_todo,
        locale::notify_zh_or("要聯絡的話自己回電", "call back if needed"),
    )
}

/// `call_waiting_rejected`: a second call came in during a call and the phone
/// turned it away (busy). A call record and a notification — no AI, no LLM. The
/// call in progress keeps its own SIM ([`call_sim_country`]).
fn on_call_waiting_rejected(state: &AppState, conn_serial: &str, parsed: &Value) {
    let serial = parsed
        .get("serial")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(conn_serial)
        .to_string();
    let caller = extract_caller_id_from_msg(parsed).unwrap_or_default();
    let sim_country = sim_country_from_msg(parsed);
    let sim_sub = sim_sub_id_from_msg(parsed);
    if is_isolated_screening_serial(&serial) || !first_call_waiting_report(&serial, &caller) {
        return;
    }
    info!(
        "call waiting rejected serial={serial} caller={} sim_country={}",
        urgent_call::masked(&caller),
        sim_country.as_deref().unwrap_or("?")
    );
    let state = state.clone();
    tokio::spawn(async move {
        let sample_rate = state.settings.sample_rate;
        if let Some(mut call) = state
            .calls
            .start(&serial, &caller, sample_rate, "call_waiting_rejected")
            .await
        {
            call.set_sim(sim_country.clone(), sim_sub);
            let record = state.calls.finalize(call, "ended").await;
            let notes = calls::CallNotes {
                summary: call_waiting_summary(&caller, sim_country.as_deref()),
                end_hint: locale::notify_zh_or(
                    "通話中插播：手機忙線拒接（沒有用 AI）",
                    "call waiting: the phone was on another call and turned it away (no AI)",
                ),
                source: "call_waiting".to_string(),
                updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                ..Default::default()
            };
            if let Err(e) = state.calls.save_notes(&record.id, &notes).await {
                warn!("call notes save failed id={}: {e:#}", record.id);
            }
        }
        let settings = state.settings_store.snapshot().effective(&state.settings);
        let ok = notify::notify_call_waiting_rejected(
            &state.http,
            &settings,
            &caller,
            &serial,
            sim_country.as_deref(),
        )
        .await;
        info!("call waiting notify serial={serial} caller={} ok={ok}", urgent_call::masked(&caller));
    });
}

/// The call's languages ([`gemini_live::call_lang_plan`]): `caller_id` on the SIM
/// of country `sim` (`None` = unknown), with the settings `language`,
/// `country_code` and `sim_languages`.
fn call_lang_plan(
    rt: &RuntimeSettings,
    caller_id: &str,
    sim: Option<&str>,
) -> gemini_live::CallLangPlan {
    gemini_live::call_lang_plan(
        caller_id,
        sim,
        rt.lang(),
        &rt.effective_country_code(),
        &rt.sim_languages,
    )
}

/// Prefer `caller_id`, then `from` (Android may send either). Empty omitted.
fn extract_caller_id_from_msg(parsed: &Value) -> Option<String> {
    for key in ["caller_id", "from"] {
        if let Some(c) = parsed.get(key).and_then(|v| v.as_str()) {
            let t = c.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// True when CID is empty / unknown / private / hidden / anonymous (case-insensitive).
/// Real dialable MSISDNs pass through (normal greet → AI path unchanged).
/// Empty is *not* immediate-reject (Android CID often late); see
/// [`is_immediate_hidden_caller`] + [`EMPTY_CID_WAIT`].
fn is_unknown_or_hidden_caller(caller: &str) -> bool {
    let s = caller.trim();
    if s.is_empty() {
        return true;
    }
    // Strip common wrappers / punctuation for token match.
    let stripped: String = s
        .chars()
        .filter(|c| {
            !matches!(
                c,
                '<' | '>' | '"' | '\'' | '[' | ']' | '(' | ')' | '{' | '}'
            )
        })
        .collect();
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    let compact: String = lower
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && *c != '-' && *c != '_' && *c != '.')
        .collect();
    // ASCII placeholders (exact after compact).
    const EXACT: &[&str] = &[
        "unknown",
        "private",
        "hidden",
        "anonymous",
        "withheld",
        "restricted",
        "unavailable",
        "notavailable",
        "null",
        "none",
        "na",
        "n/a",
        "?",
        "??",
        "???",
        "cidunknown",
        "nocallerid",
        "nonumber",
    ];
    if EXACT.iter().any(|t| compact == *t || lower == *t) {
        return true;
    }
    // Multi-word phrases (lower contains).
    const PHRASES: &[&str] = &[
        "private number",
        "number withheld",
        "caller withheld",
        "caller unknown",
        "unknown caller",
        "no caller id",
        "cid withheld",
        "anonymous call",
        "number unavailable",
        "number restricted",
        "number blocked",
        "caller id withheld",
        "callerid withheld",
    ];
    if PHRASES.iter().any(|p| lower.contains(p)) {
        return true;
    }
    // CJK placeholders (exact full string).
    const CJK: &[&str] = &[
        "未知",
        "保密",
        "匿名",
        "隱藏",
        "隐藏",
        "私人",
        "私人號碼",
        "私人号码",
        "無號碼",
        "无号码",
        "無法顯示",
        "无法显示",
        "不顯示",
        "不显示",
        "未知號碼",
        "未知号码",
        "禁顯示",
        "禁显示",
    ];
    if CJK.iter().any(|t| trimmed == *t) {
        return true;
    }
    // Only filler characters (* # x …).
    if trimmed
        .chars()
        .all(|c| matches!(c, '*' | '#' | 'x' | 'X' | '…' | '·' | '•'))
    {
        return true;
    }
    false
}

/// Explicit hidden/private tokens reject immediately. Blank CID waits.
fn is_immediate_hidden_caller(caller: &str) -> bool {
    !caller.trim().is_empty() && is_unknown_or_hidden_caller(caller)
}

fn spawn_empty_cid_wait(
    sink: WsSink,
    state: AppState,
    serial: String,
    phase: Arc<std::sync::Mutex<IvrPhase>>,
    busy: Arc<Mutex<()>>,
    hold_cancel: Option<Arc<AtomicBool>>,
    active_call: ActiveCallSlot,
    prompt_cancel: Arc<AtomicBool>,
    cid_wait_pending: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(EMPTY_CID_WAIT).await;
        if cid_wait_pending
            .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        if *phase.lock().unwrap_or_else(|e| e.into_inner()) != IvrPhase::WaitDtmf {
            info!(
                "empty caller_id wait elapsed but call already left WaitDtmf serial={serial} — skip reject"
            );
            return;
        }
        info!(
            "empty caller_id still empty after {}ms serial={serial} → reject-only (no greet)",
            EMPTY_CID_WAIT.as_millis()
        );
        if let Some(c) = active_call.lock().await.as_mut() {
            c.push_event("reject_unknown_caller");
        }
        let mut hold_cancel = hold_cancel;
        start_unknown_caller_reject(
            &sink,
            &state,
            &serial,
            &phase,
            &busy,
            &mut hold_cancel,
            &active_call,
            &prompt_cancel,
        )
        .await;
    });
}

/// Hidden number while "answer hidden numbers" is off: one short line, then
/// hang up — no Gemini Live. The line is the owner's own recording when
/// `IVR_REJECT_UNKNOWN_WAV` is set, else the same "cannot be reached"
/// announcement blocked numbers get, in the AI's voice and the agent's language
/// (made beforehand, never during the call). Neither ready: hang up at once.
async fn start_unknown_caller_reject(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    prompt_cancel: &Arc<AtomicBool>,
) {
    if is_isolated_screening_serial(serial) {
        info!("unknown-caller reject skipped isolated screening serial={serial}");
        return;
    }
    prompt_cancel.store(true, Ordering::Relaxed);
    if matches!(*phase.lock().unwrap_or_else(|e| e.into_inner()), IvrPhase::Goodbye | IvrPhase::Done) {
        return;
    }
    if !state
        .media_play
        .claim(serial, MediaPlayKind::Reject, prompt_cancel)
        .await
    {
        warn!("unknown-caller reject skipped exclusive serial={serial}");
        return;
    }
    let already_ended = {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*p, IvrPhase::Goodbye | IvrPhase::Done) {
            true
        } else {
            *p = IvrPhase::Goodbye;
            false
        }
    };
    if already_ended {
        state.media_play.release(serial).await;
        return;
    }
    stop_hold_loop(hold_cancel);
    if busy.try_lock().is_err() {
        state.media_play.release(serial).await;
        warn!("unknown-caller reject skipped busy serial={serial}");
        return;
    }
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let active_call = active_call.clone();
    let busy = busy.clone();
    tokio::spawn(async move {
        let _guard = busy.lock().await;
        // Same post-answer settle hard-cap as period greeting (WAV already trimmed).
        let delay_ms = state
            .settings
            .ivr_start_delay_ms
            .min(config::IVR_FIRST_AUDIO_DELAY_CAP_MS);
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(u64::from(delay_ms))).await;
        }
        {
            let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if p != IvrPhase::Goodbye {
                return;
            }
        }
        let announce = voice_preview::reject_announce_text().to_string();
        let (source, tts_pcm, spoken) = if let Some(pcm) = state.ivr_reject_unknown_pcm.clone() {
            let own = state.settings.ivr_reject_unknown_text.trim();
            let spoken = if own.is_empty() { announce } else { own.to_string() };
            ("ivr_reject_unknown_wav", pcm.as_ref().clone(), spoken)
        } else {
            let rt = state.settings_store.snapshot();
            let voice = voice_preview::reject_voice(&rt).to_string();
            // Made earlier, else the recording that comes with the program (no key needed).
            match state
                .previews
                .cached_reject_clip(&rt.gemini_live_model, &voice, rt.lang(), state.settings.sample_rate)
            {
                Some(pcm) => ("reject_unknown_announce", pcm, announce),
                None => {
                    // No recording for this voice and not made yet: have it for the next one.
                    let st = state.clone();
                    tokio::spawn(async move {
                        prepare_reject_clip(&st).await;
                    });
                    ("reject_unknown_none", Vec::new(), String::new())
                }
            }
        };
        info!(
            "unknown-caller reject serial={serial} source={source} pcm_bytes={} text={spoken:?}",
            tts_pcm.len()
        );
        if !tts_pcm.is_empty() {
            if let Some(c) = active_call.lock().await.as_mut() {
                c.push_event("reject_unknown_play");
            }
            let _ = send_json(
                &sink,
                json!({"type":"reply_text","text": spoken, "source": source}),
            )
            .await;
            let _ = send_tts_pcm(&sink, &state, &tts_pcm, &active_call, source, None).await;
        }
        send_hangup_with_retries(&sink, &serial, "unknown_caller_reject", 3, None).await;
        *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::Done;
        info!("unknown-caller reject hangup sent serial={serial}");
    });
}

/// [`voice_preview::prepare_reject_clip`] for the current settings.
async fn prepare_reject_clip(state: &AppState) -> bool {
    voice_preview::prepare_reject_clip(
        &state.previews,
        &state.settings_store.snapshot(),
        state.settings.sample_rate,
    )
    .await
}

/// Blocked number: play the cached announcement in the AI's voice, then hang up. No
/// Gemini session and no summary; the record keeps the number, the clip and a
/// fixed note ([`save_reject_list_notes`]).
#[allow(clippy::too_many_arguments)]
async fn start_reject_list_announce(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    caller_id: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    prompt_cancel: &Arc<AtomicBool>,
) {
    if is_isolated_screening_serial(serial) {
        return;
    }
    prompt_cancel.store(true, Ordering::Relaxed);
    state.ring_prewarm.cancel(serial, None, "拒接號碼");
    if let Some(c) = active_call.lock().await.as_mut() {
        // A record taken up again after a decline already says so.
        if !c.has_event("reject_list") {
            c.push_event("reject_list");
        }
    }
    if !state
        .media_play
        .claim(serial, MediaPlayKind::Reject, prompt_cancel)
        .await
    {
        warn!("拒接號碼 skipped exclusive serial={serial}");
        return;
    }
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*p, IvrPhase::Goodbye | IvrPhase::Done) {
            return;
        }
        *p = IvrPhase::Goodbye;
    }
    stop_hold_loop(hold_cancel);
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let caller = caller_id.to_string();
    let phase = phase.clone();
    let active_call = active_call.clone();
    let busy = busy.clone();
    tokio::spawn(async move {
        let _guard = busy.lock().await;
        let delay_ms = state
            .settings
            .ivr_start_delay_ms
            .min(config::IVR_FIRST_AUDIO_DELAY_CAP_MS);
        if delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(u64::from(delay_ms))).await;
        }
        let rt = state.settings_store.snapshot();
        let voice = voice_preview::reject_voice(&rt).to_string();
        match state
            .previews
            .cached_reject_clip(&rt.gemini_live_model, &voice, rt.lang(), state.settings.sample_rate)
        {
            Some(pcm) => {
                info!(
                    "拒接號碼 serial={serial} caller={} → 「{}」 voice={voice} audio_ms={} (no AI)",
                    urgent_call::masked(&caller),
                    voice_preview::reject_announce_text(),
                    pcm.len() * 1000 / (state.settings.sample_rate as usize * 2).max(1)
                );
                let _ = send_json(
                    &sink,
                    json!({"type":"reply_text","text": voice_preview::reject_announce_text(), "source": "reject_list"}),
                )
                .await;
                let _ = send_tts_pcm(&sink, &state, &pcm, &active_call, "reject_list", None).await;
            }
            None => {
                warn!(
                    "拒接號碼 serial={serial} caller={} announcement not ready voice={voice} — hang up without it",
                    urgent_call::masked(&caller)
                );
                let st = state.clone();
                tokio::spawn(async move {
                    prepare_reject_clip(&st).await;
                });
            }
        }
        send_hangup_with_retries(&sink, &serial, "reject_list", 3, None).await;
        *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::Done;
    });
}

/// `answer_plan` for a ringing blocked number: `decline` asks the phone to refuse
/// the call; without it (`announce`, and what an app that does not know `reject`
/// does anyway) the phone picks up at once for the announcement.
fn reject_answer_plan(decline: bool) -> Value {
    let mut plan = json!({"type": "answer_plan", "delay_ms": 0, "prewarm": false});
    if decline {
        plan["reject"] = json!(true);
    }
    plan
}

/// `whitelist_mode` `direct` hands a whitelisted caller to the owner's own
/// number only when that can work: the number to forward to is filled in and
/// the phone's app can decline a ringing call (the carrier then forwards it).
/// Otherwise the AI answers as for any whitelisted caller: declining anyway
/// would hang up on them.
fn whitelist_direct_ready(forward_number: &str, phone_can_decline: bool) -> bool {
    phone_can_decline && forward_number.chars().filter(char::is_ascii_digit).count() >= 3
}

/// `reject_mode` `decline`: a declined call never gets a media hello, so its
/// record (events `reject_list`, `reject_list_declined`) and the "blocked
/// number" notification are made while it rings. Returns the record's id.
async fn record_declined_call(
    state: &AppState,
    serial: &str,
    caller: &str,
    sample_rate: u32,
    rt: &RuntimeSettings,
) -> Option<String> {
    // The work-profile twin's copy of the call is not the owner's.
    if is_isolated_screening_serial(serial) {
        return None;
    }
    let mut record_id = None;
    if let Some(mut call) = state
        .calls
        .start(serial, caller, sample_rate, "reject_list")
        .await
    {
        call.push_event("reject_list_declined");
        call.set_sim(call_sim_country(serial), call_sim_sub_id(serial));
        let record = state.calls.finalize(call, "ended").await;
        save_reject_list_notes(state, &record);
        record_id = Some(record.id);
    }
    let dedup = state.incoming_notify_dedup.clone();
    if dedup.claim(serial, caller).await {
        let http = state.http.clone();
        let settings = state.settings_store.snapshot().effective(&state.settings);
        let reason = reject_reason_on_line(rt, caller, serial).unwrap_or_default();
        let (caller, serial) = (caller.to_string(), serial.to_string());
        tokio::spawn(async move {
            if notify_rejected_call(&http, &settings, &caller, &serial, &reason, true).await {
                info!("declined call notify ok serial={serial} caller={}", urgent_call::masked(&caller));
            } else {
                dedup.release(&serial, &caller).await;
                warn!(
                    "declined call notify FAILED serial={serial} caller={} call_notify={}",
                    urgent_call::masked(&caller),
                    settings.call_notify
                );
            }
        });
    }
    record_id
}

/// What a call that starts ringing tells the owner: a blocked number is always
/// told as such (the caller handles it); any other is an incoming call, or one
/// of our own going out ([`errands::call_notice`]), which is judged here, at the
/// first ring, from what was sent to the phone.
async fn ring_notice(state: &AppState, reject_listed: bool, serial: &str, caller: &str) -> errands::CallNotice {
    if reject_listed {
        return errands::CallNotice::Incoming;
    }
    errands::call_notice(state.control.as_ref(), state.errands.as_ref(), serial, caller).await
}

/// The words of a direct transfer's record and alert (`whitelist_mode` direct):
/// real number, matter, reply, to-do, end hint, alert title, status, forwarding number.
fn direct_transfer_words(lang: locale::Lang) -> [&'static str; 8] {
    match lang {
        locale::Lang::ZhTw => [
            "真實號碼",
            "來電直接轉到你的手機",
            "未經 AI 篩選，直接轉接至",
            "請留意真實電話來電或回電",
            "直接轉到你的手機（未經 AI 篩選）",
            "來電已轉到你的手機",
            "已觸發直接轉接（未由 AI 攔截）",
            "轉接號碼",
        ],
        locale::Lang::ZhCn => [
            "真实号码",
            "来电直接转到你的手机",
            "未经 AI 筛选，直接转接至",
            "请留意真实电话来电或回电",
            "直接转到你的手机（未经 AI 筛选）",
            "来电已转到你的手机",
            "已触发直接转接（未由 AI 拦截）",
            "转接号码",
        ],
        locale::Lang::Ja => [
            "本人の番号",
            "着信をあなたのスマホに直接転送",
            "AI を通さず直接転送：",
            "本人の電話への着信をご確認いただくか折り返してください",
            "あなたのスマホへ直接転送（AI 応答なし）",
            "着信をあなたのスマホに転送しました",
            "直接転送を実行しました（AI 応答なし）",
            "転送先",
        ],
        locale::Lang::Ko => [
            "실제 번호",
            "착신을 내 휴대폰으로 바로 전환",
            "AI 거치지 않고 직접 전환:",
            "실제 전화 착신을 확인하거나 회신하세요",
            "내 휴대폰으로 바로 전환 (AI 응답 없음)",
            "전화를 내 휴대폰으로 전환함",
            "직접 전환 실행됨 (AI 응답 없음)",
            "전환 번호",
        ],
        locale::Lang::Es => [
            "número real",
            "Llamada pasada directamente a tu teléfono",
            "Transferido directamente sin filtro de IA a",
            "Atiende tu teléfono real o devuelve la llamada",
            "Pasada directamente a tu teléfono (sin la IA)",
            "Llamada pasada a tu teléfono",
            "Transferencia directa activada (sin pasar por la IA)",
            "Número de desvío",
        ],
        locale::Lang::Fr => [
            "vrai numéro",
            "Appel transféré directement vers votre téléphone",
            "Transféré directement sans filtrage IA vers",
            "Surveillez votre vrai téléphone ou rappelez",
            "Transféré directement vers votre téléphone (sans l'IA)",
            "Appel transféré vers votre téléphone",
            "Transfert direct déclenché (sans interception IA)",
            "Numéro de transfert",
        ],
        locale::Lang::De => [
            "echte Nummer",
            "Anruf direkt an dein Telefon weitergeleitet",
            "Ohne KI-Filter direkt weitergeleitet an",
            "Bitte auf dem echten Telefon entgegennehmen oder zurückrufen",
            "Direkt an dein Telefon weitergeleitet (ohne KI)",
            "Anruf an dein Telefon weitergeleitet",
            "Direktweiterleitung ausgelöst (ohne KI)",
            "Weiterleitungsnummer",
        ],
        locale::Lang::Pt => [
            "número real",
            "Ligação passada direto para o seu celular",
            "Transferido diretamente sem triagem da IA para",
            "Acompanhe no seu telefone real ou retorne a ligação",
            "Passada direto para o seu celular (sem a IA)",
            "Ligação passada para o seu celular",
            "Transferência direta acionada (sem passar pela IA)",
            "Número de transferência",
        ],
        locale::Lang::En => [
            "real number",
            "Call forwarded straight to your phone",
            "Forwarded directly without AI screening to",
            "Check your real phone or call back",
            "Forwarded straight to your phone (no AI screening)",
            "Call Forwarded to Your Phone",
            "Direct transfer triggered (bypassed AI)",
            "Forwarding number",
        ],
    }
}

/// `whitelist_mode == "direct"`: when a caller who may be forwarded rings
/// ([`urgent_call::transfer_target`]), the phone
/// declines the call so carrier conditional forwarding (`**67*<number>#`)
/// transfers the caller directly to the owner's real phone number, while Bageo
/// records the transfer and alerts all notification channels immediately.
/// Only when it can: [`whitelist_direct_ready`].
async fn record_whitelist_direct_transfer(
    state: &AppState,
    serial: &str,
    caller: &str,
    contact_name: &str,
    sample_rate: u32,
    rt: &RuntimeSettings,
) {
    if is_isolated_screening_serial(serial) {
        return;
    }
    let forward_to = rt.whitelist_forward_number.trim().to_string();
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let [real_lbl, about_lbl, reply_prefix, todo_lbl, end_hint, alert_title, status_lbl, fwd_lbl] = direct_transfer_words(lang);
    if let Some(mut call) = state
        .calls
        .start(serial, caller, sample_rate, "whitelist_direct_transfer")
        .await
    {
        call.set_sim(call_sim_country(serial), call_sim_sub_id(serial));
        let record = state.calls.finalize(call, "ended").await;
        let target_label = if forward_to.is_empty() {
            real_lbl.to_string()
        } else {
            forward_to.clone()
        };
        let summary = format!(
            "{}: {}\n{}: {}\n{}: {} {}\n{}: {}",
            p.s_caller,
            contacts::named_number(display_caller_id(caller), Some(contact_name)),
            p.s_matter,
            about_lbl,
            p.s_reply,
            reply_prefix,
            target_label,
            p.s_todo,
            todo_lbl
        );
        let notes = calls::CallNotes {
            summary,
            dialogue: String::new(),
            dialogue_translations: Vec::new(),
            recording_text: String::new(),
            end_hint: end_hint.to_string(),
            source: "whitelist_transfer".to_string(),
            updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            calendar: None,
            appointment: None,
        };
        let calls = state.calls.clone();
        let id = record.id.clone();
        tokio::spawn(async move {
            let _ = calls.save_notes(&id, &notes).await;
        });
    }
    let http = state.http.clone();
    let settings = rt.effective(&state.settings);
    let caller_disp = contacts::named_number(&display_caller_id(caller), Some(contact_name));
    let dev = notify::device_label(serial);
    let fwd_line = if forward_to.is_empty() {
        String::new()
    } else {
        format!("\n{fwd_lbl}: {forward_to}")
    };
    let text = format!(
        "{alert_title}\n{}: {caller_disp}\n{}: {dev}{fwd_line}\n{status_lbl}",
        p.s_caller, p.n_phone
    );
    tokio::spawn(async move {
        let _ = notify::send_text_alert(&http, &settings, "whitelist_direct_transfer", alert_title, &text).await;
    });
}

/// The web record of a blocked-number call: fixed text, no LLM.
fn save_reject_list_notes(state: &AppState, record: &CallRecord) {
    if !record.events.iter().any(|e| e == "reject_list") {
        return;
    }
    // Refused while it rang (`reject_mode` `decline`): nothing was played.
    let declined = record.events.iter().any(|e| e == "reject_list_declined");
    let rt = state.settings_store.snapshot();
    let number = display_caller_id(&record.caller_id).to_string();
    // Number and reason on lines of their own.
    let reason = settings_store::reject_reason_on(rt.pending_rejects(), &record.caller_id, record.sim_country.as_deref());
    // The record is the owner's; the announcement quoted is what the caller heard.
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let announce = voice_preview::reject_announce_text();
    let summary = if declined {
        let reason = reason
            .map(|r| format!("\n{}{}{r}", p.n_reason, p.sep))
            .unwrap_or_default();
        let sep = p.sep;
        format!(
            "{}{sep}{number}{reason}\n{}{sep}{}\n{}{sep}{}",
            p.s_caller,
            p.s_matter,
            p.n_blocked,
            p.s_reply,
            locale::notify_zh_or(
                "響鈴時直接拒接，沒有接起來（沒用 AI）",
                "declined while it rang: not picked up (no AI)",
            )
        )
    } else if lang.is_zh() {
        let reason = reason.map(|r| format!("\n原因：{r}")).unwrap_or_default();
        locale::zh_or_in(
            lang,
            &format!(
                "對方：{number}{reason}\n事情：黑名單來電，播「{announce}」後掛斷\nAI 回覆：沒有接（沒用 AI）\n要處理：要聯絡的話自己回電"
            ),
            "",
        )
    } else {
        let reason = reason
            .map(|r| format!("\n{}{}{r}", p.n_reason, p.sep))
            .unwrap_or_default();
        let sep = p.sep;
        format!(
            "{}{sep}{number}{reason}\n{}{sep}{}\n{}{sep}{}",
            p.s_caller,
            p.s_matter,
            p.n_blocked,
            p.s_reply,
            p.n_blocked_done.replace("{}", announce)
        )
    };
    let notes = calls::CallNotes {
        summary,
        dialogue: String::new(),
        dialogue_translations: Vec::new(),
        recording_text: String::new(),
        end_hint: if declined {
            locale::notify_zh_or(
                "黑名單：響鈴時直接拒接（沒有用 AI）",
                "blocked number: declined while it rang (no AI)",
            )
        } else {
            locale::notify_zh_or(
                "黑名單：播無法接通語音後掛斷（沒有用 AI）",
                "blocked number: the announcement was played, then hung up (no AI)",
            )
        },
        source: "reject_list".to_string(),
        updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        calendar: None,
        appointment: None,
    };
    let calls = state.calls.clone();
    let id = record.id.clone();
    tokio::spawn(async move {
        match calls.save_notes(&id, &notes).await {
            Ok(()) => info!("call notes saved id={id} source=reject_list"),
            Err(e) => warn!("call notes save failed id={id}: {e:#}"),
        }
    });
}

async fn start_ivr_prompt(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
    arm_auto_ai: &Arc<AtomicBool>,
    auto_ai_wake: &Arc<Notify>,
    menu_plays: &Arc<AtomicU32>,
    prompt_cancel: &Arc<AtomicBool>,
    apply_start_delay: bool,
    dtmf_ignore_until: Option<Arc<std::sync::Mutex<Option<tokio::time::Instant>>>>,
) {
    if is_isolated_screening_serial(serial) {
        info!("IVR prompt skipped isolated screening serial={serial}");
        return;
    }
    if *phase.lock().unwrap_or_else(|e| e.into_inner()) != IvrPhase::WaitDtmf {
        return;
    }
    if !state
        .media_play
        .claim(serial, MediaPlayKind::Greet, prompt_cancel)
        .await
    {
        warn!("IVR prompt skipped exclusive serial={serial} (reject/twin already playing)");
        return;
    }
    let not_wait = {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        // Greeting may only start from idle wait (not during voicemail/hold/goodbye).
        if *p != IvrPhase::WaitDtmf {
            true
        } else {
            *p = IvrPhase::PlayPrompt;
            false
        }
    };
    if not_wait {
        state.media_play.release(serial).await;
        return;
    }
    if busy.try_lock().is_err() {
        *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::WaitDtmf;
        state.media_play.release(serial).await;
        warn!("IVR prompt skipped busy serial={serial}");
        return;
    }

    let sink = sink.clone();
    let mut state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let active_call = active_call.clone();
    let arm_auto_ai = arm_auto_ai.clone();
    let auto_ai_wake = auto_ai_wake.clone();
    let menu_plays = menu_plays.clone();
    let prompt_cancel = prompt_cancel.clone();
    let busy = busy.clone();
    let dtmf_ignore_until = dtmf_ignore_until;
    let hello_at = tokio::time::Instant::now();
    tokio::spawn(async move {
        let _guard = busy.lock().await;
        // Bail if already moved out of PlayPrompt (barge-in).
        {
            let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if p != IvrPhase::PlayPrompt || prompt_cancel.load(Ordering::Relaxed) {
                return;
            }
        }
        // Honor latest web-console overrides (hot-swap) for this play cycle.
        // A to-do's call-back, or an appointment's (or an errand's) call, going out
        // on this phone is our call: no greeting for a call coming in, the AI
        // follows its call-back script.
        let persona = {
            let rt = state.settings_store.snapshot();
            state.settings = rt.effective(&state.settings);
            rt.persona_flow_active()
                || fulfill::voicemail_callback_pending(&serial)
                || appointments::callback_pending_on(&serial)
        };
        // Persona mode: no period greeting WAV, no post-greet wait — after the short
        // settle, hand the line straight to Gemini (it answers 「喂？」 itself).
        if persona {
            let settle_ms = if apply_start_delay {
                u64::from(
                    state
                        .settings
                        .ivr_start_delay_ms
                        .min(config::IVR_FIRST_AUDIO_DELAY_CAP_MS),
                )
            } else {
                0
            };
            let remain = settle_ms.saturating_sub(hello_at.elapsed().as_millis() as u64);
            if remain > 0 {
                tokio::time::sleep(Duration::from_millis(remain)).await;
            }
            drop(_guard);
            if prompt_cancel.load(Ordering::Relaxed) {
                info!("Persona answer cancelled (hangup) serial={serial} — skip AI");
                return;
            }
            let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
            if *p == IvrPhase::PlayPrompt {
                *p = IvrPhase::WaitDtmf;
                menu_plays.fetch_add(1, Ordering::Relaxed);
                arm_auto_ai.store(true, Ordering::Relaxed);
                auto_ai_wake.notify_one();
                if let Some(ref until) = dtmf_ignore_until {
                    *until.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(tokio::time::Instant::now() + Duration::from_millis(120));
                }
                info!(
                    "Persona answer → arm_auto_ai serial={serial} (no greeting WAV) hello_to_arm_ms={}",
                    hello_at.elapsed().as_millis()
                );
            } else {
                info!("Persona answer ignored (phase moved) serial={serial} phase={p:?}");
            }
            return;
        }
        // Hard ceiling: answer→first PCM ≤ IVR_FIRST_AUDIO_DELAY_CAP_MS (env/UI may still claim 800).
        // post_greet_ai_delay_ms is untouched (after full greet, before auto-AI).
        let delay_ms = if apply_start_delay {
            state
                .settings
                .ivr_start_delay_ms
                .min(config::IVR_FIRST_AUDIO_DELAY_CAP_MS)
        } else {
            0
        };
        let text = config::format_ivr_menu_spoken(&state.settings.ivr_prompt_text);
        // Prefer full period clone WAV (greeting + auto-transfer body). No menu AI.
        // Resolve from preloaded RAM first so settle sleep is the only wait before first PCM.
        let mut segs: Vec<IvrSegment> = Vec::new();
        let hour = config::ivr_menu_now_plus8().hour();
        let candidates: Vec<&str> = {
            let primary = config::ivr_menu_full_wav_basename(hour);
            let mut v = vec![primary];
            for alt in [
                "ivr_menu_full_evening.wav",
                "ivr_menu_full_afternoon.wav",
                "ivr_menu_full_morning.wav",
                "ivr_menu_full_noon.wav",
            ] {
                if alt != primary {
                    v.push(alt);
                }
            }
            v
        };
        for name in &candidates {
            if let Some((_, pcm)) = state
                .ivr_menu_full_pcm
                .iter()
                .find(|(n, _)| n.as_str() == *name)
            {
                info!(
                    "IVR greeting full WAV preloaded serial={serial} hour={hour} file={name} pcm_bytes={}",
                    pcm.len()
                );
                segs.push(IvrSegment {
                    label: name.trim_end_matches(".wav").to_string(),
                    path: PathBuf::from(config::asset_path(name)),
                    pcm: pcm.as_ref().clone(),
                });
                break;
            }
        }
        if segs.is_empty() {
            for name in &candidates {
                let full_path = PathBuf::from(config::asset_path(name));
                if let Some(pcm) =
                    load_fail_wav_pcm(full_path.to_str().unwrap_or(""), state.settings.sample_rate)
                {
                    let (pcm, ms) =
                        trim_leading_silence_s16le(&pcm, state.settings.sample_rate, 80.0, 20);
                    info!(
                        "IVR greeting full WAV loaded serial={serial} hour={hour} file={name} pcm_bytes={} trim_lead_ms={ms}",
                        pcm.len()
                    );
                    segs.push(IvrSegment {
                        label: name.trim_end_matches(".wav").to_string(),
                        path: full_path,
                        pcm,
                    });
                    break;
                }
            }
        }
        if segs.is_empty() {
            let greet_path = PathBuf::from(config::asset_path(config::ivr_greet_wav_basename(hour)));
            if let Some(pcm) = load_fail_wav_pcm(
                greet_path.to_str().unwrap_or(""),
                state.settings.sample_rate,
            ) {
                segs.push(IvrSegment {
                    label: format!("ivr_greet_{hour}"),
                    path: greet_path,
                    pcm,
                });
                let body_path = PathBuf::from(&state.settings.ivr_menu_body_wav);
                if let Some(pcm) = load_fail_wav_pcm(
                    &state.settings.ivr_menu_body_wav,
                    state.settings.sample_rate,
                ) {
                    segs.push(IvrSegment {
                        label: "ivr_menu_body".into(),
                        path: body_path,
                        pcm,
                    });
                }
            }
        }
        if segs.is_empty() && !state.ivr_prompt_segments.is_empty() {
            segs = state.ivr_prompt_segments.iter().cloned().collect();
        }
        if segs.len() == 1 && segs[0].label.starts_with("ivr_greet_") {
            if !state.ivr_prompt_segments.is_empty() {
                segs = state.ivr_prompt_segments.iter().cloned().collect();
            } else {
                segs.clear();
            }
        }
        // Settle after load so disk fallback overlaps the same wait window.
        if delay_ms > 0 {
            let spent_ms = hello_at.elapsed().as_millis() as u64;
            let remain = (delay_ms as u64).saturating_sub(spent_ms);
            if remain > 0 {
                info!(
                    "IVR start delay serial={serial} ms={remain} (cap={delay_ms} after_load={spent_ms}ms)"
                );
                tokio::time::sleep(Duration::from_millis(remain)).await;
            } else {
                info!(
                    "IVR start delay serial={serial} skipped (load already took {spent_ms}ms >= {delay_ms})"
                );
            }
            let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if p != IvrPhase::PlayPrompt || prompt_cancel.load(Ordering::Relaxed) {
                return;
            }
        }
        {
            let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if p != IvrPhase::PlayPrompt || prompt_cancel.load(Ordering::Relaxed) {
                return;
            }
        }
        let segments: Arc<Vec<IvrSegment>> = Arc::new(segs);

        // Greeting never starts AI itself — auto-AI only after full play below.
        let play_source_tag = if segments.is_empty() {
            "ivr_menu_missing"
        } else {
            "ivr_menu_wav"
        };
        info!(
            "IVR greeting local path serial={serial} source={play_source_tag} segments={} hello_to_ready_ms={} (auto-AI only after full PCM)",
            segments.len(),
            hello_at.elapsed().as_millis()
        );
        // Product hard rule: play greet to last frame of phone playout (not just last WS send),
        // then post_greet_ai_delay; only then arm_auto_ai / UnmuteAfterGreet.
        // `greet_fully_done` is set only after every segment's send_tts_pcm returns Ok
        // (which now wall-clock pads for head-burst queue lag).
        let mut greet_fully_done = false;
        let mut greet_total_pcm: usize = 0;
        let greet_wall_t0 = tokio::time::Instant::now();
        if segments.is_empty() {
            error!(
                "IVR greeting clone WAV missing serial={serial} — skip Piper; text only for UI: {text:?}"
            );
            let _ = send_json(
                &sink,
                json!({"type":"reply_text","text": text, "source": play_source_tag}),
            )
            .await;
            // No audio — still hand off to AI (or VM if Gemini off).
            greet_fully_done = true;
        } else {
            info!(
                "IVR PlayPrompt serial={serial} source={play_source_tag} segments={} total_pcm={} order={:?} text={text:?}",
                segments.len(),
                segments.iter().map(|s| s.pcm.len()).sum::<usize>(),
                segments.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
            );
            let _ = send_json(
                &sink,
                json!({
                    "type": "reply_text",
                    "text": text,
                    "source": play_source_tag,
                    "segments": segments.iter().map(|s| s.label.clone()).collect::<Vec<_>>(),
                }),
            )
            .await;
            let mut all_ok = true;
            for (idx, seg) in segments.iter().enumerate() {
                {
                    let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
                    if p != IvrPhase::PlayPrompt || prompt_cancel.load(Ordering::Relaxed) {
                        info!(
                            "IVR greeting aborted mid-cycle serial={serial} after_seg={} phase={p:?} cancelled={} — NO auto-AI",
                            seg.label,
                            prompt_cancel.load(Ordering::Relaxed)
                        );
                        all_ok = false;
                        break;
                    }
                }
                let secs = seg.pcm.len() as f64 / (state.settings.sample_rate as f64 * 2.0);
                info!(
                    "IVR segment play serial={serial} idx={}/{} label={} pcm_bytes={} (~{secs:.1}s) via={play_source_tag} hello_to_first_pcm_ms={} (schedule before first WS binary byte)",
                    idx + 1,
                    segments.len(),
                    seg.label,
                    seg.pcm.len(),
                    hello_at.elapsed().as_millis()
                );
                // cancel is hangup-only for greet (DTMF blocked in PlayPrompt).
                if let Err(e) = send_tts_pcm(
                    &sink,
                    &state,
                    &seg.pcm,
                    &active_call,
                    &seg.label,
                    Some(&prompt_cancel),
                )
                .await
                {
                    error!(
                        "IVR segment TTS failed serial={serial} label={}: {e:#} — NO auto-AI",
                        seg.label
                    );
                    all_ok = false;
                    break;
                }
                greet_total_pcm = greet_total_pcm.saturating_add(seg.pcm.len());
                if prompt_cancel.load(Ordering::Relaxed) {
                    info!(
                        "IVR greeting hangup cancel serial={serial} after label={} — NO auto-AI",
                        seg.label
                    );
                    all_ok = false;
                    break;
                }
            }
            greet_fully_done = all_ok;
        }
        // Release busy before arming AI so Live never overlaps greet path lock.
        drop(_guard);
        if greet_fully_done && !prompt_cancel.load(Ordering::Relaxed) {
            // Align wall clock to full greet playback duration (belt+suspenders vs send_tts pad).
            let sr = state.settings.sample_rate.max(8000) as u64;
            let full_ms = if greet_total_pcm > 0 {
                (greet_total_pcm as u64).saturating_mul(1000) / (sr.saturating_mul(2)).max(1)
            } else {
                0
            };
            let elapsed_ms = greet_wall_t0.elapsed().as_millis() as u64;
            let remain_play_ms = full_ms.saturating_sub(elapsed_ms);
            if remain_play_ms > 0 {
                info!(
                    "IVR greet wall-align serial={serial} remain_play_ms={remain_play_ms} full_ms={full_ms} elapsed_ms={elapsed_ms}"
                );
                tokio::time::sleep(Duration::from_millis(remain_play_ms)).await;
            }
            let post_ms = u64::from(state.settings.post_greet_ai_delay_ms).min(15_000);
            info!(
                "IVR greet_fully_done serial={serial} hello_to_done_ms={} full_pcm_ms={full_ms} post_greet_delay_ms={post_ms} (zero Gemini inject until unmute)",
                hello_at.elapsed().as_millis()
            );
            if post_ms > 0 && !prompt_cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(post_ms)).await;
                info!(
                    "IVR post_greet_delay done serial={serial} waited_ms={post_ms} hello_to_post_done_ms={}",
                    hello_at.elapsed().as_millis()
                );
            }
            if prompt_cancel.load(Ordering::Relaxed) {
                info!("IVR greeting post-delay cancelled (hangup) serial={serial} — skip auto-AI");
            } else {
                let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
                if *p == IvrPhase::PlayPrompt {
                    *p = IvrPhase::WaitDtmf;
                    let n = menu_plays.fetch_add(1, Ordering::Relaxed) + 1;
                    // ★ GATE: greet fully played + post delay → main loop starts Gemini (or VM).
                    arm_auto_ai.store(true, Ordering::Relaxed);
                    auto_ai_wake.notify_one();
                    if let Some(ref until) = dtmf_ignore_until {
                        *until.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(tokio::time::Instant::now() + Duration::from_millis(120));
                    }
                    info!(
                        "IVR greeting FULLY done → arm_auto_ai serial={serial} menu_plays={n} hello_to_arm_ms={} (no race)",
                        hello_at.elapsed().as_millis()
                    );
                } else {
                    info!("IVR greeting done ignored (phase moved) serial={serial} phase={p:?}");
                }
            }
        } else {
            info!(
                "IVR greeting incomplete serial={serial} full={greet_fully_done} cancel={} — skip auto-AI",
                prompt_cancel.load(Ordering::Relaxed)
            );
            let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
            if *p == IvrPhase::PlayPrompt {
                *p = IvrPhase::WaitDtmf;
            }
        }
    });
}

/// Early TRTC session for the choice menu via [`trtc_bridge`].
///
/// Returns `(true, Some(join))` when menu is spoken via `ServerPushText`
/// (caller skips local WAV and pulls downlink). `(false, _)` → local
/// `trtc_menu_local_fallback` until **this room** is media-live + cloud ready.
fn arm_trtc_menu_session(
    state: &AppState,
    serial: &str,
    menu_text: &str,
) -> (bool, Option<trtc_bridge::TrtcJoinParams>) {
    let trtc = state.settings.trtc();
    let bridge = trtc_bridge::make_bridge(&trtc, state.http.clone());
    match trtc_bridge::arm_and_queue_menu(bridge.as_ref(), &trtc, serial, menu_text) {
        Ok((join, trtc_bridge::MenuPlayPath::ServerPushText)) => {
            info!(
                "TRTC menu session UserSig ok serial={serial} sdk_app_id={} room_id={} user_id={} — menu via ServerPushText (room_media_live=true cloud_api_ready=true)",
                trtc.sdk_app_id,
                join.room_id,
                join.user_id
            );
            // Downlink uses bridge_* participant (sidecar media user).
            let dl_join = trtc_bridge::TrtcJoinParams {
                sdk_app_id: join.sdk_app_id,
                room_id: join.room_id.clone(),
                user_id: format!("bridge_{serial}"),
                user_sig: join.user_sig.clone(),
            };
            (true, Some(dl_join))
        }
        Ok((join, trtc_bridge::MenuPlayPath::LocalFallback)) => {
            info!(
                "TRTC menu session UserSig ok serial={serial} sdk_app_id={} room_id={} user_id={} — menu audio queued; keep trtc_menu_local_fallback (room_media_live={} media_bridge_live={})",
                trtc.sdk_app_id,
                join.room_id,
                join.user_id,
                trtc_bridge::room_media_live(&join.room_id),
                bridge.media_bridge_live()
            );
            (false, None)
        }
        Err(e) => {
            warn!(
                "TRTC menu session arm failed serial={serial} err={e:#}; soft local menu fallback"
            );
            (false, None)
        }
    }
}

/// Rough TTS duration for ServerPushText before starting silence timers.
fn estimate_spoken_ms(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    (chars.saturating_mul(180).saturating_add(500)).clamp(800, 10_000)
}

/// Speak one AI line after DTMF1 (hello / silence nudge).
///
/// Preferred product path: TRTC AI realtime — `ControlAIConversation`
/// `ServerPushText` (cloud LLM room + clone TTS) when `cloud_api_ready` and
/// StartAI TaskId is live. Local `ivr_ai_hello` / `ivr_ai_nudge` WAV or Piper
/// only as fallback (never double-play with ServerPush). Menu stays local WAV.
async fn speak_trtc_ai_line(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    active_call: &ActiveCallSlot,
    text: &str,
    source: &str,
    pre_recorded: Option<&[u8]>,
) {
    let text = text.trim();
    if text.is_empty() && pre_recorded.map(|p| p.is_empty()).unwrap_or(true) {
        return;
    }
    let mut play_source = source.to_string();
    // Primary: TRTC clone TTS in the live AI conversation (same voice as talk).
    if !text.is_empty() {
        let trtc = state.settings.trtc();
        if trtc.cloud_api_ready() {
            if trtc_bridge::push_ai_line_await(&trtc, state.http.clone(), serial, text).await {
                play_source = format!("{source}_server_push");
                let settle = estimate_spoken_ms(text);
                info!(
                    "TrtcAi line ServerPushText serial={serial} source={play_source} settle_ms={settle} text={text:?}"
                );
                let _ = send_json(
                    sink,
                    json!({
                        "type": "reply_text",
                        "text": text,
                        "source": play_source,
                    }),
                )
                .await;
                // Downlink pull loop already carries TTS→phone; settle timer for silence windows.
                tokio::time::sleep(Duration::from_millis(settle)).await;
                return;
            }
            info!(
                "TrtcAi line ServerPushText miss serial={serial} source={source} — local WAV/Piper fallback"
            );
        }
    }
    let _ = send_json(
        sink,
        json!({
            "type": "reply_text",
            "text": text,
            "source": play_source,
        }),
    )
    .await;
    if let Some(pcm) = pre_recorded {
        if !pcm.is_empty() {
            info!(
                "TrtcAi line WAV serial={serial} source={source} pcm_bytes={}",
                pcm.len()
            );
            let _ = send_tts_pcm(sink, state, pcm, active_call, source, None).await;
            return;
        }
    }
    // No Piper fallback — wait for TRTC/Gemini clone voice only.
    if !text.is_empty() {
        warn!(
            "TrtcAi line no WAV/ServerPush serial={serial} source={source} — silence (no Piper) text={text:?}"
        );
    }
}

/// After DTMF1 arm: **do not** play a canned "hello / are you there" — realtime TRTC AI owns talk.
///
/// Soft fallback only when StartAI TaskId never appears (no cloud AI): optional
/// local hello once, then hangup. When AI is live, only a long silence hangup
/// safety net (TRTC MaxIdleTime also stops the cloud robot).
fn spawn_trtc_ai_silence_supervisor(
    sink: WsSink,
    state: AppState,
    serial: String,
    phase: Arc<std::sync::Mutex<IvrPhase>>,
    busy: Arc<Mutex<()>>,
    cancel: Arc<AtomicBool>,
    active_call: ActiveCallSlot,
    rt: Arc<RuntimeSettings>,
    heard: Arc<AtomicBool>,
) {
    let hello = state.settings.ivr_ai_hello_text.clone();
    let before_nudge_ms = state.settings.ivr_ai_silence_before_nudge_ms as u64;
    let after_nudge_ms = state.settings.ivr_ai_silence_after_nudge_ms as u64;
    let goodbye = state.settings.ivr_goodbye_text.clone();
    // Cloud MaxIdle floor 30s; keep phone hangup a bit longer so AI can greet + listen.
    let ai_max_idle_ms = (state.settings.trtc_ai_max_idle_secs as u64)
        .saturating_mul(1000)
        .max(30_000);
    tokio::spawn(async move {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        // Wait briefly for StartAI TaskId (armed in parallel during wait clip).
        if state.settings.trtc().cloud_api_ready()
            && trtc_bridge::peek_ai_task_id(&serial).is_none()
        {
            let wait_task = tokio::time::Instant::now() + Duration::from_secs(6);
            while tokio::time::Instant::now() < wait_task {
                if cancel.load(Ordering::Relaxed) || heard.load(Ordering::Relaxed) {
                    return;
                }
                if trtc_bridge::peek_ai_task_id(&serial).is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        let ai_live = trtc_bridge::peek_ai_task_id(&serial).is_some();
        if ai_live {
            // Product path: no ServerPush/WAV hello or "are you there" lines — the LLM + TTS in TRTC
            // greets and talks. SystemPrompt should instruct the first greeting.
            info!(
                "TrtcAi skip canned hello/nudge serial={serial} — realtime AI owns dialogue (task=1)"
            );
            let silence_ms = ai_max_idle_ms
                .saturating_add(before_nudge_ms)
                .saturating_add(after_nudge_ms)
                .clamp(45_000, 180_000);
            let deadline = tokio::time::Instant::now() + Duration::from_millis(silence_ms);
            while tokio::time::Instant::now() < deadline {
                if cancel.load(Ordering::Relaxed) || heard.load(Ordering::Relaxed) {
                    info!(
                        "TrtcAi silence supervisor exit serial={serial} heard={} cancel={}",
                        heard.load(Ordering::Relaxed),
                        cancel.load(Ordering::Relaxed)
                    );
                    return;
                }
                let phase_now = *phase.lock().unwrap_or_else(|e| e.into_inner());
                if !matches!(phase_now, IvrPhase::TrtcAi) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            if cancel.load(Ordering::Relaxed) || heard.load(Ordering::Relaxed) {
                return;
            }
            let phase_now = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(phase_now, IvrPhase::TrtcAi) {
                return;
            }
            info!(
                "TrtcAi silence hangup serial={serial} (no remote voice for ~{silence_ms}ms; AI was live)"
            );
            let mut hold_cancel = Some(cancel);
            start_ivr_stub_hangup(
                &sink,
                &state,
                &serial,
                &phase,
                &busy,
                &mut hold_cancel,
                &active_call,
                goodbye,
                "ai_silence_hangup",
                false,
                true,
                &rt,
            );
            return;
        }

        // Soft path: StartAI never armed — one local fallback line then short hangup.
        // Do not loop "are you there" nudges when cloud AI is the product path.
        info!("TrtcAi soft fallback serial={serial} (no TaskId) — one local line then hangup gate");
        if !hello.trim().is_empty() || state.ivr_ai_hello_pcm.is_some() {
            speak_trtc_ai_line(
                &sink,
                &state,
                &serial,
                &active_call,
                &hello,
                "trtc_ai_hello_soft",
                state.ivr_ai_hello_pcm.as_ref().map(|p| p.as_slice()),
            )
            .await;
        }
        if cancel.load(Ordering::Relaxed) || heard.load(Ordering::Relaxed) {
            return;
        }
        let soft_ms = before_nudge_ms.saturating_add(after_nudge_ms).max(8_000);
        let deadline = tokio::time::Instant::now() + Duration::from_millis(soft_ms);
        while tokio::time::Instant::now() < deadline {
            if cancel.load(Ordering::Relaxed) || heard.load(Ordering::Relaxed) {
                return;
            }
            let phase_now = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(phase_now, IvrPhase::TrtcAi) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if cancel.load(Ordering::Relaxed) || heard.load(Ordering::Relaxed) {
            return;
        }
        let phase_now = *phase.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(phase_now, IvrPhase::TrtcAi) {
            return;
        }
        info!("TrtcAi silence hangup serial={serial} (soft path, no TaskId / no reply)");
        let mut hold_cancel = Some(cancel);
        start_ivr_stub_hangup(
            &sink,
            &state,
            &serial,
            &phase,
            &busy,
            &mut hold_cancel,
            &active_call,
            goodbye,
            "ai_silence_hangup",
            false,
            true,
            &rt,
        );
    });
}

/// Product AI entry: Gemini Live when ready; otherwise automatic voicemail.
/// Must only be called after greeting fully finished (arm_auto_ai gate),
/// except DTMF-1 fallback. Prefer UnmuteAfterGreet when ConnectAtAnswer already warmed.
fn enter_gemini_or_voicemail(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_started: &mut bool,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    rt: &Arc<RuntimeSettings>,
    gemini_transcript: &Arc<std::sync::Mutex<String>>,
    gemini_uplink_tx: &Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    sample_rate: u32,
    play_connect_wait: bool,
    gemini_audio_open: &Arc<AtomicBool>,
    gemini_audio_opened_at: &GeminiAudioOpenedAt,
    gemini_setup_ready: &Arc<AtomicBool>,
    gemini_connect_failed: &Arc<AtomicBool>,
    gemini_prewarmed: &mut bool,
    media_call_guard: &MediaCallGuard,
) {
    if !media_call_guard.is_current() {
        info!("IVR AI entry skipped for stale media call serial={serial}");
        return;
    }
    {
        let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
        // Hard rule: never ungate AI speech while greet still playing.
        if matches!(p, IvrPhase::PlayPrompt) {
            warn!("IVR AI blocked serial={serial} phase=PlayPrompt — greet must finish first");
            return;
        }
        if matches!(
            p,
            IvrPhase::LiveAi
                | IvrPhase::TrtcAi
                | IvrPhase::VoicemailIntro
                | IvrPhase::VoicemailRecord
                | IvrPhase::Goodbye
                | IvrPhase::Done
        ) {
            warn!("IVR AI skipped serial={serial} phase={p:?}");
            return;
        }
    }

    // ★ UnmuteAfterGreet: ConnectAtAnswer already running (muted dialogue).
    // If warmhold died (phone WS reset / Gemini Close during greet), do not dump
    // to VM — cold-start a fresh Live session. VM remains the fallback if that fails.
    if *gemini_prewarmed {
        if gemini_connect_failed.load(Ordering::Relaxed) {
            warn!(
                "IVR UnmuteAfterGreet skipped serial={serial} (prewarm failed) → cold-start Gemini"
            );
            *gemini_prewarmed = false;
            stop_hold_loop(hold_cancel);
        } else {
            info!("IVR UnmuteAfterGreet serial={serial} (prewarmed Gemini)");
            unmute_gemini_after_greet(
                sink,
                state,
                serial,
                phase,
                busy,
                hold_started,
                hold_cancel,
                active_call,
                gemini_audio_open,
                gemini_audio_opened_at,
                gemini_setup_ready,
                gemini_connect_failed,
                gemini_prewarmed,
                media_call_guard,
            );
            return;
        }
    }

    let use_gemini = rt.gemini_live_enabled && !rt.gemini_api_key.trim().is_empty();
    if !use_gemini {
        warn!(
            "IVR AI unavailable → auto voicemail serial={serial} enabled={} key_set={}",
            rt.gemini_live_enabled,
            !rt.gemini_api_key.trim().is_empty()
        );
        stop_hold_loop(hold_cancel);
        {
            let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(*p, IvrPhase::PlayPrompt) {
                *p = IvrPhase::WaitDtmf;
            }
        }
        start_voicemail_intro_for_gemini(
            sink,
            state,
            serial,
            phase,
            busy,
            active_call,
            media_call_guard,
        );
        return;
    }
    if let Ok(mut g) = gemini_transcript.lock() {
        g.clear();
    }
    // Cold path (no ConnectAtAnswer): open audio immediately.
    if let Ok(mut opened_at) = gemini_audio_opened_at.lock() {
        *opened_at = Some(tokio::time::Instant::now());
    }
    gemini_audio_open.store(true, Ordering::Relaxed);
    gemini_setup_ready.store(false, Ordering::Relaxed);
    gemini_connect_failed.store(false, Ordering::Relaxed);
    start_gemini_live_after_choice(
        sink,
        state,
        serial,
        phase,
        busy,
        hold_started,
        hold_cancel,
        active_call,
        rt,
        gemini_transcript,
        gemini_uplink_tx,
        sample_rate,
        play_connect_wait,
        gemini_audio_open,
        gemini_audio_opened_at,
        gemini_setup_ready,
        gemini_connect_failed,
        false, // not a prewarm-only spawn
        media_call_guard,
    );
}

/// Wait briefly for warm setup, then open Gemini speech to telephony (never mid-greet).
fn unmute_gemini_after_greet(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_started: &mut bool,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    gemini_audio_open: &Arc<AtomicBool>,
    gemini_audio_opened_at: &GeminiAudioOpenedAt,
    gemini_setup_ready: &Arc<AtomicBool>,
    gemini_connect_failed: &Arc<AtomicBool>,
    gemini_prewarmed: &mut bool,
    media_call_guard: &MediaCallGuard,
) {
    if !media_call_guard.is_current() {
        info!("IVR UnmuteAfterGreet skipped for stale media call serial={serial}");
        return;
    }
    if *hold_started {
        warn!("IVR UnmuteAfterGreet ignored serial={serial} (already hold_started)");
        return;
    }
    // Claim the AI arm so auto-AI cannot double-fire while we brief-wait.
    *hold_started = true;
    *gemini_prewarmed = false; // consumed

    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let busy = busy.clone();
    let active_call = active_call.clone();
    let audio_open = gemini_audio_open.clone();
    let audio_opened_at = gemini_audio_opened_at.clone();
    let setup_ready = gemini_setup_ready.clone();
    let connect_failed = gemini_connect_failed.clone();
    let cancel = hold_cancel.clone();
    let media_call_guard = media_call_guard.clone();

    tokio::spawn(async move {
        // Brief wait only if warm setup lagged entire greet duration (normally 0).
        // Persona mode unmutes ~400ms after answer (no greeting to hide setup behind),
        // so allow the WS connect + setup more time before falling back to voicemail.
        let wait_ms: u64 = if state.settings_store.snapshot().persona_flow_active() {
            5_000
        } else {
            2_000
        };
        let wait_t0 = tokio::time::Instant::now();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        while !setup_ready.load(Ordering::Relaxed)
            && !connect_failed.load(Ordering::Relaxed)
            && tokio::time::Instant::now() < deadline
        {
            if !media_call_guard.is_current() {
                info!("IVR UnmuteAfterGreet invalidated serial={serial}");
                return;
            }
            if cancel
                .as_ref()
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(false)
            {
                info!("IVR UnmuteAfterGreet aborted (cancel) serial={serial}");
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        if !media_call_guard.is_current() {
            info!("IVR UnmuteAfterGreet invalidated after wait serial={serial}");
            return;
        }

        if connect_failed.load(Ordering::Relaxed) || !setup_ready.load(Ordering::Relaxed) {
            warn!(
                "IVR UnmuteAfterGreet serial={serial} Gemini not ready (failed={} ready={} waited_ms={}) → VM",
                connect_failed.load(Ordering::Relaxed),
                setup_ready.load(Ordering::Relaxed),
                wait_t0.elapsed().as_millis()
            );
            if cancel
                .as_ref()
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(false)
            {
                return;
            }
            if let Some(c) = cancel.as_ref() {
                c.store(true, Ordering::Relaxed);
            }
            if !media_call_guard.is_current() {
                return;
            }
            start_voicemail_intro_for_gemini(
                &sink,
                &state,
                &serial,
                &phase,
                &busy,
                &active_call,
                &media_call_guard,
            );
            return;
        }

        let Some(phase_lease) = media_call_guard.lease_current().await else {
            return;
        };
        {
            let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(
                *p,
                IvrPhase::Done
                    | IvrPhase::Goodbye
                    | IvrPhase::VoicemailIntro
                    | IvrPhase::VoicemailRecord
            ) {
                info!("IVR UnmuteAfterGreet skipped serial={serial} phase={p:?}");
                return;
            }
            *p = IvrPhase::LiveAi;
        }
        drop(phase_lease);
        {
            let Some(_call_lease) = media_call_guard.lease_current().await else {
                return;
            };
            let mut slot = active_call.lock().await;
            if let Some(c) = slot.as_mut() {
                c.set_ai_segment_active(true);
                c.push_event("gemini_unmute_after_greet");
            }
        }
        // ★ Open dialogue: warm prebuf + greets already kicked; downlink may reach telephony.
        let Some(_open_lease) = media_call_guard.lease_current().await else {
            return;
        };
        if let Ok(mut opened_at) = audio_opened_at.lock() {
            *opened_at = Some(tokio::time::Instant::now());
        }
        audio_open.store(true, Ordering::Relaxed);
        info!(
            "IVR unmute serial={serial} audio_open=true waited_ms={} first_inject_gate_open (prebuf flush if any)",
            wait_t0.elapsed().as_millis()
        );
    });
}

/// `incoming` (phone still ringing): start the call's Gemini session now so its
/// 「喂？」 is generated and held when the media hello arrives after pickup. Same
/// gates as the hello path: persona mode, Gemini ready, caller ID route = answer.
fn start_ring_prewarm(
    state: &AppState,
    serial: &str,
    caller_id: &str,
    sample_rate: u32,
    rt: &RuntimeSettings,
    ring_ms: u32,
) -> bool {
    // `incoming` often comes before the phone has looked up the SIM: the SIM of the
    // phone's last call stands in, and the pickup checks the guess
    // (`RingPrewarmRegistry::take`).
    let sim = call_sim_country(serial)
        .or_else(|| phone_region::last_sim(&canonical_device_serial(serial)));
    if !rt.gemini_ring_prewarm
        || !rt.persona_flow_active()
        || !state.settings.agent_mode.is_transfer()
        || !rt.gemini_live_enabled
        || rt.gemini_api_key.trim().is_empty()
        || cid_route_on_hello(
            serial,
            caller_id,
            rt.answer_unknown_callers,
            reject_listed_on_sim(rt, caller_id, sim.as_deref(), None),
        ) != CidRoute::Greet
    {
        return false;
    }
    let plan = caller_plan(rt, caller_id, sim.as_deref());
    let cfg = GeminiRetryCfg {
        api_key: rt.gemini_api_key.clone(),
        model: rt.gemini_live_model.clone(),
        // A SIM's own number in that SIM's language (setting `sim_languages`).
        system: rt.gemini_system_prompt_for_call_in(plan.lang.prompt_lang),
        voice_name: if rt.gemini_voice_name.trim().is_empty() {
            gemini_live::default_voice_name().to_string()
        } else {
            rt.gemini_voice_name.clone()
        },
        lang: plan.lang.clone(),
        caller_id: caller_id.to_string(),
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let transcript = Arc::new(std::sync::Mutex::new(String::new()));
    let audio_open = Arc::new(AtomicBool::new(false));
    let audio_opened_at: GeminiAudioOpenedAt = Arc::new(std::sync::Mutex::new(None));
    let setup_ready = Arc::new(AtomicBool::new(false));
    let (up_tx, up_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let goodbye_hangup_delay_ms = u64::from(rt.goodbye_hangup_delay_ms);
    info!(
        "Ring prewarm Gemini start serial={serial} caller={} model={} voice={} phone_sr={sample_rate} answer_ring_ms={ring_ms} (「喂？」 held until pickup)",
        urgent_call::masked(caller_id),
        if cfg.model.trim().is_empty() {
            gemini_live::default_model()
        } else {
            cfg.model.as_str()
        },
        cfg.voice_name,
    );
    let core = start_gemini_session_core(
        serial,
        rt,
        &cfg,
        sample_rate,
        true,
        cancel.clone(),
        up_rx,
        transcript.clone(),
        audio_open.clone(),
        audio_opened_at.clone(),
        setup_ready.clone(),
        goodbye_hangup_delay_ms,
        Arc::new(AtomicBool::new(false)),
        None,
        None,
        None,
    );
    let id = state.ring_prewarm.insert(
        serial,
        RingPrewarm {
            id: 0,
            caller_id: caller_id.to_string(),
            sample_rate,
            started: tokio::time::Instant::now(),
            core,
            up_tx,
            cancel,
            transcript,
            audio_open,
            audio_opened_at,
            setup_ready,
            connect_failed: Arc::new(AtomicBool::new(false)),
            cfg,
            plan,
            persona: true,
            goodbye_hangup_delay_ms,
        },
    );
    let registry = state.ring_prewarm.clone();
    let serial = serial.to_string();
    let ttl = Duration::from_millis(u64::from(ring_ms)) + RING_PREWARM_GRACE;
    tokio::spawn(async move {
        tokio::time::sleep(ttl).await;
        registry.cancel(&serial, Some(id), "not answered");
    });
    true
}

/// Pickup of a call whose Gemini session started while it rang: this media call
/// takes the session over. Its 「喂？」 is already held, so UnmuteAfterGreet plays
/// it right after the answer settle. `caller_id`: the hello's number, which the
/// session reads from now on. False = not adopted (session stopped).
#[allow(clippy::too_many_arguments)]
async fn adopt_ring_prewarm(
    pw: RingPrewarm,
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    caller_id: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    gemini_transcript: &mut Arc<std::sync::Mutex<String>>,
    gemini_uplink_tx: &Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    gemini_audio_open: &mut Arc<AtomicBool>,
    gemini_audio_opened_at: &mut GeminiAudioOpenedAt,
    gemini_setup_ready: &mut Arc<AtomicBool>,
    gemini_connect_failed: &mut Arc<AtomicBool>,
    gemini_prewarmed: &mut bool,
    media_call_guard: &MediaCallGuard,
) -> bool {
    if !media_call_guard.is_current() || *gemini_prewarmed || hold_cancel.is_some() {
        pw.cancel.store(true, Ordering::Relaxed);
        warn!(
            "Ring prewarm not adopted serial={serial} id={} current={} prewarmed={} hold_cancel={}",
            pw.id,
            media_call_guard.is_current(),
            *gemini_prewarmed,
            hold_cancel.is_some()
        );
        return false;
    }
    let RingPrewarm {
        id,
        started,
        core,
        up_tx,
        cancel,
        transcript,
        audio_open,
        audio_opened_at,
        setup_ready,
        connect_failed,
        mut cfg,
        persona,
        goodbye_hangup_delay_ms,
        sample_rate,
        ..
    } = pw;
    let _ = core.media_guard.set(media_call_guard.clone());
    let rang_caller = cfg.caller_id.clone();
    if take_hello_caller(&core, &mut cfg, caller_id) {
        info!(
            "Ring prewarm caller updated serial={serial} id={id} rang_caller={} hello_caller={}",
            urgent_call::masked(&rang_caller),
            urgent_call::masked(&cfg.caller_id)
        );
    }
    // The media hello says which SIM took the call: appointments text back from it.
    if let Some(book) = core.appointments.as_ref() {
        book.set_line(call_sim_country(serial), call_sim_sub_id(serial));
    }
    if let Ok(mut g) = gemini_uplink_tx.lock() {
        *g = Some(up_tx);
    }
    *gemini_transcript = transcript.clone();
    *gemini_audio_open = audio_open.clone();
    *gemini_audio_opened_at = audio_opened_at.clone();
    *gemini_setup_ready = setup_ready.clone();
    *gemini_connect_failed = connect_failed.clone();
    *hold_cancel = Some(cancel.clone());
    *gemini_prewarmed = true;
    {
        let mut slot = active_call.lock().await;
        if let Some(c) = slot.as_mut() {
            c.share_barge_in_flag(core.barge_in.clone());
            c.push_event("gemini_ring_prewarm");
        }
    }
    let opening_text = transcript
        .lock()
        .map(|t| t.trim().to_string())
        .unwrap_or_default();
    info!(
        "Ring prewarm adopted serial={serial} id={id} rang_ms={} setup_ready={} opening_stt={opening_text:?}",
        started.elapsed().as_millis(),
        setup_ready.load(Ordering::Relaxed),
        opening_text = log_text::Said(&opening_text)
    );
    tokio::spawn(run_gemini_session_to_end(
        core,
        sink.clone(),
        state.clone(),
        serial.to_string(),
        active_call.clone(),
        cancel,
        gemini_uplink_tx.clone(),
        transcript,
        Some(cfg),
        sample_rate,
        audio_open,
        audio_opened_at,
        setup_ready,
        connect_failed,
        true,
        Some((phase.clone(), busy.clone())),
        goodbye_hangup_delay_ms,
        media_call_guard.clone(),
        persona,
    ));
    true
}

/// The media hello's number replaces the one a ring prewarm started with (empty
/// or the previous call's; [`RingPrewarmRegistry::take`] checked it leads to the
/// same set-up): the carrier-prompt check and a fresh retry read it from now on.
/// An empty hello number keeps the ringing one. True when it changed.
fn take_hello_caller(core: &GeminiSessionCore, cfg: &mut GeminiRetryCfg, hello: &str) -> bool {
    let hello = hello.trim();
    if hello.is_empty() || hello == cfg.caller_id.trim() {
        return false;
    }
    *core.caller.lock().unwrap_or_else(|e| e.into_inner()) = hello.to_string();
    cfg.caller_id = hello.to_string();
    true
}

/// ConnectAtAnswer: start Gemini WS + setup while period greeting plays.
/// Speech stays muted until UnmuteAfterGreet (full greet PCM). Silent to caller.
fn start_gemini_connect_at_answer(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    rt: &Arc<RuntimeSettings>,
    gemini_transcript: &Arc<std::sync::Mutex<String>>,
    gemini_uplink_tx: &Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    sample_rate: u32,
    gemini_audio_open: &Arc<AtomicBool>,
    gemini_audio_opened_at: &GeminiAudioOpenedAt,
    gemini_setup_ready: &Arc<AtomicBool>,
    gemini_connect_failed: &Arc<AtomicBool>,
    gemini_prewarmed: &mut bool,
    media_call_guard: &MediaCallGuard,
    caller_id: &str,
) {
    if !media_call_guard.is_current() {
        info!("ConnectAtAnswer skipped for stale media call serial={serial}");
        return;
    }
    let use_gemini = rt.gemini_live_enabled && !rt.gemini_api_key.trim().is_empty();
    if !use_gemini {
        info!("ConnectAtAnswer skip serial={serial} (Gemini off or no key)");
        return;
    }
    if *gemini_prewarmed || hold_cancel.is_some() {
        warn!(
            "ConnectAtAnswer skip serial={serial} prewarmed={} hold_cancel={}",
            *gemini_prewarmed,
            hold_cancel.is_some()
        );
        return;
    }
    if let Ok(mut g) = gemini_transcript.lock() {
        g.clear();
    }
    if let Ok(mut opened_at) = gemini_audio_opened_at.lock() {
        *opened_at = None;
    }
    gemini_audio_open.store(false, Ordering::Relaxed);
    gemini_setup_ready.store(false, Ordering::Relaxed);
    gemini_connect_failed.store(false, Ordering::Relaxed);
    *gemini_prewarmed = true;

    // Do NOT set hold_started / LiveAi here — greets gate + DTMF rules stay on PlayPrompt.
    let cancel = Arc::new(AtomicBool::new(false));
    *hold_cancel = Some(cancel.clone());
    let (up_tx, up_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    if let Ok(mut g) = gemini_uplink_tx.lock() {
        *g = Some(up_tx);
    }

    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let active_call = active_call.clone();
    let phase = phase.clone();
    let busy = busy.clone();
    let api_key = rt.gemini_api_key.clone();
    let model = rt.gemini_live_model.clone();
    let system = rt.gemini_system_prompt_for_call();
    let voice_name = if rt.gemini_voice_name.trim().is_empty() {
        gemini_live::default_voice_name().to_string()
    } else {
        rt.gemini_voice_name.clone()
    };
    let goodbye_hangup_delay_ms = u64::from(state.settings.goodbye_hangup_delay_ms);
    let gemini_uplink_tx = gemini_uplink_tx.clone();
    let gemini_transcript = gemini_transcript.clone();
    let audio_open = gemini_audio_open.clone();
    let audio_opened_at = gemini_audio_opened_at.clone();
    let setup_ready = gemini_setup_ready.clone();
    let connect_failed = gemini_connect_failed.clone();
    let media_call_guard = media_call_guard.clone();
    let persona = rt.persona_flow_active();
    let caller_id = caller_id.to_string();

    info!(
        "ConnectAtAnswer Gemini warm start serial={serial} persona={persona} model={} voice={} phone_sr={sample_rate} (mute until UnmuteAfterGreet)",
        if model.trim().is_empty() {
            gemini_live::default_model()
        } else {
            model.as_str()
        },
        voice_name,
    );

    tokio::spawn(async move {
        if !media_call_guard.is_current() {
            info!("ConnectAtAnswer spawn invalidated serial={serial}");
            return;
        }
        spawn_gemini_session_tasks(
            sink,
            state,
            serial,
            active_call,
            cancel,
            up_rx,
            gemini_uplink_tx,
            gemini_transcript,
            api_key,
            model,
            system,
            sample_rate,
            audio_open,
            audio_opened_at,
            setup_ready,
            connect_failed,
            true,                // prewarm: fail before unmute only marks failed flag
            Some((phase, busy)), // post-unmute fail → VM
            voice_name,
            goodbye_hangup_delay_ms,
            media_call_guard,
            persona,
            true, // 0 audio after unmute → one fresh Live session first
            caller_id,
        )
        .await;
    });
}

/// Shared Gemini Live runtime (warm or cold): session + paced downlink player.
async fn spawn_gemini_session_tasks(
    sink: WsSink,
    state: AppState,
    serial: String,
    active_call: ActiveCallSlot,
    cancel: Arc<AtomicBool>,
    up_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    gemini_uplink_tx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    gemini_transcript: Arc<std::sync::Mutex<String>>,
    api_key: String,
    model: String,
    system: String,
    sample_rate: u32,
    audio_open: Arc<AtomicBool>,
    audio_opened_at: GeminiAudioOpenedAt,
    setup_ready: Arc<AtomicBool>,
    connect_failed: Arc<AtomicBool>,
    prewarm: bool,
    fail_vm: Option<(Arc<std::sync::Mutex<IvrPhase>>, Arc<Mutex<()>>)>,
    voice_name: String,
    goodbye_hangup_delay_ms: u64,
    media_call_guard: MediaCallGuard,
    persona: bool,
    fresh_retry_left: bool,
    caller_id: String,
) {
    if !media_call_guard.is_current() {
        info!("Gemini session spawn skipped for stale media call serial={serial}");
        return;
    }
    let rt = state.settings_store.snapshot();
    // The AI's call-back with an appointment's answer: only the call this phone
    // reported dialing for it (`outbound_ready` with its task and number) —
    // never an incoming call or a ring prewarm (which starts elsewhere).
    // A to-do's phone call-back (a voicemail answered by phone) the same way.
    let callback = match appointments::callback_for_call(&serial, &caller_id).await {
        Some(cb) => Some(cb),
        None => fulfill::voicemail_callback_for_call(state.control.as_ref(), &serial, &caller_id).await,
    };
    let mut errand = None;
    if let Some(cb) = &callback {
        info!("appointment call-back session serial={serial} task_id={} errand={}", cb.task_id, cb.errand);
        let mut record_id = String::new();
        if let Some(c) = active_call.lock().await.as_mut() {
            c.push_event(appointments::AI_CALLBACK_EVENT);
            if cb.errand {
                c.push_event(errands::ERRAND_EVENT);
            }
            record_id = c.id.clone();
        }
        if cb.errand {
            // Only a call the errand still wants (not cancelled, deleted or over),
            // on the SIM it was given (the media hello says which one the phone
            // used): anything else is hung up at once, before the AI starts.
            let (sim_sub, sim_country) = (call_sim_sub_id(&serial), call_sim_country(&serial));
            let gate = state.errands.call_gate(&cb.task_id, sim_sub, sim_country.as_deref());
            if gate != errands::CallGate::Go {
                info!("errand call not wanted serial={serial} id={} gate={gate:?} — hang up", cb.task_id);
                cancel.store(true, Ordering::Relaxed);
                let why = if gate == errands::CallGate::WrongSim { "errand_wrong_sim" } else { "errand_cancelled" };
                send_hangup_with_retries(&sink, &serial, why, 3, Some(&media_call_guard)).await;
                // Its outbound call ends here: nothing of it may stay on the phone
                // for the phone's next call.
                let eff = rt.effective(&state.settings);
                let ctx = appointments::CallbackCtx {
                    http: &state.http,
                    settings: &eff,
                    tasks: state.task_store.as_ref(),
                    control: state.control.as_ref(),
                    errands: Some(state.errands.as_ref()),
                };
                errands::gated_call(&ctx, &state.errands, &cb.task_id, gate, sim_sub, sim_country.as_deref()).await;
                return;
            }
            errand = Some(state.errands.attach_call(&cb.task_id, &record_id));
        }
        // This call's summary ends this call-back (by the call's own record).
        appointments::note_call_task(&record_id, &cb.task_id);
    }
    // Started at pickup: the media hello has reported the SIM (when the app does).
    // An appointment's call-back speaks the language its script is in (the call
    // it answers, else the setting's: appointments::callback_lang), whatever the
    // number and the SIM suggest. An errand's call plans its languages as any call.
    let lang = match callback.as_ref().filter(|c| !c.errand).and_then(|c| c.lang) {
        Some(spoken) => gemini_live::callback_lang_plan(spoken, rt.lang()),
        None => call_lang_plan(&rt, &caller_id, call_sim_country(&serial).as_deref()),
    };
    // A SIM's own number in that SIM's language (setting `sim_languages`): the
    // call's prompt in it. A call-back follows its own script.
    let system = match lang.prompt_lang {
        Some(l) if callback.is_none() => rt.gemini_system_prompt_for_call_in(Some(l)),
        _ => system,
    };
    let cfg = GeminiRetryCfg {
        api_key,
        model,
        system,
        voice_name,
        lang,
        caller_id,
    };
    // Set by the call's echo check once the caller may talk over the AI.
    let barge_in = match active_call.lock().await.as_ref() {
        Some(c) => c.barge_in_flag(),
        None => Arc::new(AtomicBool::new(false)),
    };
    let core = start_gemini_session_core(
        &serial,
        &rt,
        &cfg,
        sample_rate,
        persona,
        cancel.clone(),
        up_rx,
        gemini_transcript.clone(),
        audio_open.clone(),
        audio_opened_at.clone(),
        setup_ready.clone(),
        goodbye_hangup_delay_ms,
        barge_in,
        Some(media_call_guard.clone()),
        callback.as_ref().map(|c| c.script.clone()),
        errand,
    );
    // Kept for one fresh-socket retry if this session never delivers audio
    // (not for a call-back: nobody may have answered, and a retry would greet).
    let retry_cfg = (fresh_retry_left && callback.is_none()).then_some(cfg);
    run_gemini_session_to_end(
        core,
        sink,
        state,
        serial,
        active_call,
        cancel,
        gemini_uplink_tx,
        gemini_transcript,
        retry_cfg,
        sample_rate,
        audio_open,
        audio_opened_at,
        setup_ready,
        connect_failed,
        prewarm,
        fail_vm,
        goodbye_hangup_delay_ms,
        media_call_guard,
        persona,
    )
    .await;
}

/// A Live session task plus what the player / completion side needs from it.
/// Started at pickup (media hello) or, ring prewarm, while the phone still rings.
struct GeminiSessionCore {
    session: tokio::task::JoinHandle<anyhow::Result<gemini_live::SessionEndReason>>,
    dl_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    /// Media call allowed to receive the audio; a ring prewarm gets it at pickup.
    media_guard: Arc<std::sync::OnceLock<MediaCallGuard>>,
    force_hangup: Arc<AtomicBool>,
    delivered_bytes: Arc<AtomicU64>,
    play_head: Arc<gemini_live::DownlinkPlayHead>,
    stall_engaged: Arc<AtomicBool>,
    /// How the call was judged as sales / scam (for the call record).
    stall_judgement: Arc<std::sync::Mutex<Option<calls::ScamJudgement>>>,
    barge_in: Arc<AtomicBool>,
    /// What the caller agrees to on this call (appointments on), for the call record.
    appointments: Option<Arc<appointments::CallAppointments>>,
    /// The caller's number as the session reads it (the carrier-prompt check): a
    /// ring prewarm's is replaced by the media hello's at adoption.
    caller: Arc<std::sync::Mutex<String>>,
    /// Set once when a Whitelist caller (`whitelist_mode == "emergency"`) states an emergency.
    urgent_reason: Arc<std::sync::Mutex<Option<String>>>,
    /// Whitelist urgent call (`whitelist_mode` emergency with the owner's number set) on this call.
    urgent_call: Option<Arc<urgent_call::CallCtl>>,
}

#[allow(clippy::too_many_arguments)]
fn start_gemini_session_core(
    serial: &str,
    rt: &RuntimeSettings,
    cfg: &GeminiRetryCfg,
    sample_rate: u32,
    persona: bool,
    cancel: Arc<AtomicBool>,
    up_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    transcript: Arc<std::sync::Mutex<String>>,
    audio_open: Arc<AtomicBool>,
    audio_opened_at: GeminiAudioOpenedAt,
    setup_ready: Arc<AtomicBool>,
    goodbye_hangup_delay_ms: u64,
    barge_in: Arc<AtomicBool>,
    media_guard: Option<MediaCallGuard>,
    callback_script: Option<String>,
    errand: Option<Arc<errands::CallHooks>>,
) -> GeminiSessionCore {
    // The AI's own call-back with an appointment's answer
    // (`appointments::callback_for_call`): its script instead of the answering
    // prompt, no opening while it rings, and no appointments, sales stalling
    // or international carrier-prompt wait on that call.
    let callback_cfg = callback_script.map(|system| GeminiRetryCfg { system, ..cfg.clone() });
    let is_callback = callback_cfg.is_some();
    let cfg = callback_cfg.as_ref().unwrap_or(cfg);
    // Persona mode has no fixed clock, only the call-length safety cap. Sales / scam
    // calls are judged on every persona call (not AI call-backs); stalling (setting
    // persona_stall_spam) swaps in its prompt once one is.
    let stall_engaged = Arc::new(AtomicBool::new(false));
    let stall_judgement: Arc<std::sync::Mutex<Option<calls::ScamJudgement>>> = Default::default();
    let urgent_reason: Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let persona_max_secs = rt.persona_max_secs();
    let server_vad = rt.gemini_server_vad;
    let thinking_level = rt.gemini_thinking_level.clone();
    let caller_slot = Arc::new(std::sync::Mutex::new(cfg.caller_id.clone()));
    let sim_init = call_sim_country(serial)
        .or_else(|| phone_region::last_sim(&canonical_device_serial(serial)));
    let force_stall = contacts::is_force_stall(&cfg.caller_id, sim_init.as_deref()).is_some();
    // `whitelist_mode` emergency for a caller who may be forwarded
    // (urgent_call::transfer_target): the urgent part of the prompt, the alert and
    // the call to the owner. Calls answered for the owner only: not the AI's own
    // call-backs and errands.
    let urgent_target = (rt.whitelist_mode == "emergency" && persona && !is_callback && errand.is_none())
        .then(|| urgent_call::transfer_target(rt, &cfg.caller_id, sim_init.as_deref()))
        .flatten();
    // A number not in the contacts: one call to the owner per STRANGER_DIAL_EVERY.
    let urgent_key = urgent_target
        .as_ref()
        .filter(|t| t.who == urgent_call::TransferWho::Stranger)
        .map(|_| urgent_call::stranger_key(&cfg.caller_id, sim_init.as_deref()));
    // Urgent call: decided when the session starts (with the prompt that tells the
    // AI to ask the caller to hold on, or that the owner will call back).
    let urgent_will_call = urgent_target.as_ref().is_some_and(|t| {
        urgent_call::will_dial(
            rt,
            t,
            urgent_key.as_deref().unwrap_or_default(),
            urgent_call::stranger_dials(),
            std::time::Instant::now(),
        )
    });
    if let Some(t) = &urgent_target {
        info!(
            "urgent forwarding on this call serial={serial} who={:?} will_call={urgent_will_call}",
            t.who
        );
    }
    let urgent_prompt = urgent_target.as_ref().map(|t| (t.who, urgent_will_call));
    let urgent_ctl = urgent_will_call.then(|| {
        Arc::new(match urgent_key.clone() {
            Some(key) => urgent_call::CallCtl::limited(key),
            None => urgent_call::CallCtl::default(),
        })
    });
    let stall_cfg = (persona && !is_callback).then(|| {
        let serial_for_ctx = serial.to_string();
        let caller_slot_for_ctx = caller_slot.clone();
        let urgent_slot = urgent_reason.clone();
        gemini_live::StallCfg {
            engaged: stall_engaged.clone(),
            force_start: force_stall,
            system_prompt: rt.persona_stall_system_prompt_in(cfg.lang.prompt_lang),
            model: rt.gemini_stall_model.clone(),
            max_secs: rt.persona_stall_max_secs(),
            play_along: rt.persona_stall_spam || force_stall,
            review_without_keywords: rt.scam_review_without_keywords,
            judgement: stall_judgement.clone(),
            caller_context: Some(gemini_live::CallerContextFn(Arc::new(move || {
                let caller = caller_slot_for_ctx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone();
                let sim = call_sim_country(&serial_for_ctx)
                    .or_else(|| phone_region::last_sim(&canonical_device_serial(&serial_for_ctx)));
                let saved_contact = contacts::review_label(&caller, sim.as_deref());
                // The owner's marks, earlier verdicts and number hints (scam database).
                let facts = scam_db::caller_facts(&caller, sim.as_deref());
                stall_review::CallerContext {
                    number: caller,
                    saved_contact,
                    number_notes: facts.notes,
                    owner_safe: facts.owner_safe,
                    flagged_before: facts.flagged_before,
                }
            }))),
            urgent_transfer: urgent_target.is_some().then(|| {
                gemini_live::UrgentTransferFn(Arc::new(move |said: &str| {
                    if let Ok(mut g) = urgent_slot.lock() {
                        if g.is_none() {
                            *g = Some(said.trim().to_string());
                        }
                    }
                }))
            }),
            urgent_call: urgent_ctl.clone(),
        }
    });
    // Asked at pickup: by then the media hello has said which SIM took the call
    // (the ring prewarm starts before the phone knows) and which number calls.
    let intl_caller: gemini_live::IntlPromptCheck = {
        let serial = serial.to_string();
        let caller_slot = caller_slot.clone();
        let zh_tw = rt.lang() == locale::Lang::ZhTw;
        Box::new(move || {
            if !persona || is_callback {
                return false;
            }
            let caller = caller_slot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let sim = call_sim_country(&serial);
            let hold = gemini_live::expects_carrier_intl_prompt(zh_tw, &caller, sim.as_deref());
            if gemini_live::is_intl_caller(&caller) || hold {
                // How the check read the number on this SIM.
                let number = sim
                    .as_deref()
                    .and_then(|s| phone_region::normalize(&caller, Some(s)))
                    .map_or_else(|| "?".to_string(), |n| format!("{} ({})", urgent_call::masked(&n.e164), n.country));
                info!(
                    "intl caller serial={serial} caller={} number={number} sim_country={} → hold opening for carrier prompt={hold}",
                    urgent_call::masked(&caller),
                    sim.as_deref().unwrap_or("?")
                );
            }
            hold
        })
    };
    // Appointments (setting `appointment_mode`): normal persona calls only.
    let mut appointments = if persona && !is_callback {
        appointments::ApptSession::for_call(rt, &cfg.caller_id)
    } else {
        None
    };
    // A SIM's own number in that SIM's language: the appointment part in it too.
    if let Some(a) = appointments.as_mut() {
        a.set_prompt_lang(cfg.lang.prompt_lang);
    }
    // The SIM that took the call: a text back to the caller goes from it, and
    // its country reads a number without a country code. A ring prewarm sets it
    // again at pickup from the media hello.
    if let Some(a) = appointments.as_mut() {
        a.set_line(call_sim_country(serial), call_sim_sub_id(serial));
    }
    // The opening language and the caller's speech-to-text hints, decided with the
    // call's config (a ring prewarm's is checked again at pickup).
    let lang_plan = cfg.lang.clone();
    if (persona && (lang_plan.opening.is_some() || lang_plan.local_hello.is_some())) || lang_plan.caller_stt.is_none() {
        info!(
            "caller languages serial={serial} caller={} opening={} caller_stt={}",
            urgent_call::masked(&cfg.caller_id),
            lang_plan
                .opening
                .map(|l| l.code())
                .or(lang_plan.local_hello.map(|h| h.code))
                .filter(|_| persona)
                .unwrap_or("-"),
            lang_plan
                .caller_stt
                .as_ref()
                .map_or_else(|| "auto".to_string(), |c| c.join(","))
        );
    }
    let appointment_book = appointments.as_ref().map(|a| a.book.clone());
    // For the `persona_prompt` log: how much of the prompt is the persona text
    // (the owner's or the built-in one; an errand's or a call-back's script has none).
    let user_prompt_chars = if persona && !is_callback && errand.is_none() {
        rt.persona_user_chars(cfg.lang.prompt_lang)
    } else {
        0
    };
    let force_hangup = Arc::new(AtomicBool::new(false));
    let delivered_bytes = Arc::new(AtomicU64::new(0));
    let play_head = gemini_live::DownlinkPlayHead::new();
    let guard_slot = Arc::new(std::sync::OnceLock::new());
    if let Some(g) = media_guard {
        let _ = guard_slot.set(g);
    }
    let (dl_tx, dl_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let session = {
        let cancel_for_run = cancel.clone();
        let cancel_for_cb = cancel;
        let audio_open_cb = audio_open.clone();
        let serial = serial.to_string();
        let GeminiRetryCfg {
            api_key,
            model,
            system,
            voice_name,
            ..
        } = cfg.clone();
        // A caller in the owner's contacts: the prompt says who it is. Normal calls
        // only: the sales / scam prompt the session may switch to has no such line.
        // The SIM as the ring prewarm reads it (the last one the phone reported while
        // the call rings), so the contact is the one its plan checks at pickup.
        // An errand's script names whom it calls itself.
        let system = if force_stall && persona && errand.is_none() {
            rt.persona_stall_system_prompt_in(cfg.lang.prompt_lang)
        } else if persona && errand.is_none() {
            let sim = call_sim_country(&serial).or_else(|| phone_region::last_sim(&canonical_device_serial(&serial)));
            let lang = cfg.lang.prompt_lang.unwrap_or_else(|| rt.lang());
            contacts::with_caller(&system, rt, &cfg.caller_id, sim.as_deref(), lang, urgent_prompt)
        } else {
            system
        };
        let force_hangup = force_hangup.clone();
        let delivered_bytes = delivered_bytes.clone();
        let play_head = play_head.clone();
        let play_head_cb = play_head.clone();
        let guard_cb = guard_slot.clone();
        let barge_in = barge_in.clone();
        let urgent_cb = urgent_ctl.clone();
        tokio::spawn(async move {
            gemini_live::run_session(
                &api_key,
                &model,
                &system,
                sample_rate,
                up_rx,
                move |pcm| {
                    // Double-gate: never inject before UnmuteAfterGreet.
                    // Whitelist urgent call: nothing from the AI while the phone dials
                    // the owner, or once the owner is on.
                    if cancel_for_cb.load(Ordering::Relaxed)
                        || pcm.is_empty()
                        || !audio_open_cb.load(Ordering::Relaxed)
                        || !guard_cb.get().is_some_and(MediaCallGuard::is_current)
                        || urgent_cb.as_ref().is_some_and(|c| c.ai_quiet())
                    {
                        return;
                    }
                    let n = pcm.len() as u64;
                    if dl_tx.send(pcm).is_err() {
                        cancel_for_cb.store(true, Ordering::Relaxed);
                    } else {
                        play_head_cb.enqueue(n);
                    }
                },
                cancel_for_run,
                &serial,
                Some(transcript),
                audio_open,
                audio_opened_at,
                setup_ready,
                &voice_name,
                goodbye_hangup_delay_ms,
                Some(force_hangup),
                delivered_bytes,
                play_head,
                persona,
                persona_max_secs,
                stall_cfg,
                server_vad,
                barge_in,
                &thinking_level,
                intl_caller,
                appointments,
                lang_plan,
                is_callback,
                errand,
                user_prompt_chars,
            )
            .await
        })
    };
    GeminiSessionCore {
        session,
        dl_rx,
        media_guard: guard_slot,
        force_hangup,
        delivered_bytes,
        play_head,
        stall_engaged,
        stall_judgement,
        barge_in,
        appointments: appointment_book,
        caller: caller_slot,
        urgent_reason,
        urgent_call: urgent_ctl,
    }
}

/// Push events onto this call's record (while it is current).
async fn push_call_events(active_call: &ActiveCallSlot, media_call_guard: &MediaCallGuard, events: &[String]) {
    if let Some(_lease) = media_call_guard.lease_current().await {
        if let Some(c) = active_call.lock().await.as_mut() {
            for e in events {
                c.push_event(e);
            }
        }
    }
}

fn urgent_call_failed_events(reason: &str) -> Vec<String> {
    vec![
        "whitelist_urgent_call_failed".to_string(),
        format!("whitelist_urgent_call_failed:{reason}"),
    ]
}

/// Whitelist urgent call: something the AI cannot fix (it dropped off while the
/// owner was dialed, or the caller is stuck on hold): tell the owner at once.
async fn send_urgent_call_alert(state: &AppState, serial: &str, caller_num: &str, alert: urgent_call::Alert) {
    let http = state.http.clone();
    let rt = state.settings_store.snapshot();
    let settings = rt.effective(&state.settings);
    let sim = call_sim_country(serial);
    let ct_name = contacts::lookup(caller_num, sim.as_deref()).map(|c| c.name);
    let caller_disp = contacts::named_number(display_caller_id(caller_num), ct_name.as_deref());
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let title = urgent_call::alert_title(lang);
    let text = format!(
        "{title}\n{}: {caller_disp}\n{}: {}\n{}",
        p.s_caller,
        p.n_phone,
        notify::device_label(serial),
        urgent_call::alert_line(alert, lang)
    );
    info!("Whitelist urgent call: alert {alert:?} serial={serial}");
    tokio::spawn(async move {
        let _ = notify::send_text_alert(&http, &settings, "whitelist_urgent_transfer", title, &text).await;
    });
}

/// Whitelist urgent call (`whitelist_mode` emergency with the owner's number set). Dials only once the
/// AI has said its hold-on line (from its own words) and that line has played
/// (hold cuts this call's audio for the caller); the owner's number must pass the
/// errands check. Then the phone dials from this call's SIM and merges; the call
/// is handed over, or the AI comes back (the owner did not answer / will call
/// back), or the owner is alerted when the AI cannot talk. Nothing here hangs the
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UrgentCallOutcome {
    Pending,
    Merged,
    StillOnHold,
    Failed,
}

#[derive(Clone)]
struct UrgentCallCoord {
    tx: tokio::sync::watch::Sender<UrgentCallOutcome>,
}

impl UrgentCallCoord {
    fn new() -> Self {
        let (tx, _) = tokio::sync::watch::channel(UrgentCallOutcome::Pending);
        Self { tx }
    }

    fn set(&self, outcome: UrgentCallOutcome) {
        self.tx.send_if_modified(|cur| {
            if *cur == outcome {
                false
            } else {
                *cur = outcome;
                true
            }
        });
    }

    #[cfg(test)]
    fn outcome(&self) -> UrgentCallOutcome {
        *self.tx.borrow()
    }

    fn subscribe(&self) -> tokio::sync::watch::Receiver<UrgentCallOutcome> {
        self.tx.subscribe()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UrgentEndAction {
    Usual,
    Handed,
    StillOnHold,
    AlertAiEnded,
}

const URGENT_END_MERGE_GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UrgentFailureDisposition {
    StillOnHold,
    SessionGoneHangup,
    ResumeAi,
}

async fn handle_urgent_call_failure_disposition(
    sink: &WsSink,
    serial: &str,
    reason: &str,
    ctl: &urgent_call::CallCtl,
    cancel: &AtomicBool,
    urgent_coord: &UrgentCallCoord,
    media_call_guard: &MediaCallGuard,
) -> UrgentFailureDisposition {
    if reason == "unhold_failed" {
        urgent_coord.set(UrgentCallOutcome::StillOnHold);
        cancel.store(true, Ordering::Relaxed);
        return UrgentFailureDisposition::StillOnHold;
    }
    if ctl.session_gone() && reason != "call_ended" {
        urgent_coord.set(UrgentCallOutcome::Failed);
        cancel.store(true, Ordering::Relaxed);
        send_hangup_with_retries(
            sink,
            serial,
            "urgent_call_ai_gone",
            2,
            Some(media_call_guard),
        )
        .await;
        return UrgentFailureDisposition::SessionGoneHangup;
    }
    UrgentFailureDisposition::ResumeAi
}

async fn finish_urgent_session_end(
    ctl: Option<&urgent_call::CallCtl>,
    urgent_coord: &UrgentCallCoord,
    grace: Duration,
) -> UrgentEndAction {
    let Some(ctl) = ctl else {
        return UrgentEndAction::Usual;
    };
    let end = urgent_call::on_session_end(Some(ctl));
    if end == urgent_call::SessionEnd::Usual {
        return UrgentEndAction::Usual;
    }
    if end == urgent_call::SessionEnd::LeaveUp {
        return UrgentEndAction::Handed;
    }
    let mut rx = urgent_coord.subscribe();
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        if ctl.is_handed() || *rx.borrow() == UrgentCallOutcome::Merged {
            ctl.hand_over();
            return UrgentEndAction::Handed;
        }
        if *rx.borrow() == UrgentCallOutcome::StillOnHold {
            return UrgentEndAction::StillOnHold;
        }
        match urgent_call::on_session_end(Some(ctl)) {
            urgent_call::SessionEnd::Usual => return UrgentEndAction::Usual,
            urgent_call::SessionEnd::LeaveUp => return UrgentEndAction::Handed,
            urgent_call::SessionEnd::LeaveUpAndAlert => {}
        }
        if *rx.borrow() == UrgentCallOutcome::Failed || tokio::time::Instant::now() >= deadline {
            break;
        }
        if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
            break;
        }
    }
    if ctl.is_handed() || *rx.borrow() == UrgentCallOutcome::Merged {
        ctl.hand_over();
        return UrgentEndAction::Handed;
    }
    if *rx.borrow() == UrgentCallOutcome::StillOnHold {
        return UrgentEndAction::StillOnHold;
    }
    if urgent_call::on_session_end(Some(ctl)) == urgent_call::SessionEnd::Usual {
        return UrgentEndAction::Usual;
    }
    ctl.mark_session_gone();
    UrgentEndAction::AlertAiEnded
}

/// call up.
#[allow(clippy::too_many_arguments)]
async fn run_urgent_call(
    sink: WsSink,
    state: AppState,
    serial: String,
    caller_num: String,
    number: String,
    ctl: Arc<urgent_call::CallCtl>,
    active_call: ActiveCallSlot,
    media_call_guard: MediaCallGuard,
    play_head: Arc<gemini_live::DownlinkPlayHead>,
    delivered_bytes: Arc<AtomicU64>,
    on_fail: String,
    on_callback: String,
    cancel: Arc<AtomicBool>,
    urgent_coord: UrgentCallCoord,
) {
    // Wait until the AI has said its hold-on line (whenever in the call that happens),
    // and that line has finished playing out on the phone.
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !media_call_guard.is_current() {
            urgent_coord.set(UrgentCallOutcome::Failed);
            info!("Whitelist urgent call: call over before dialing serial={serial}");
            return;
        }
        if ctl.dial_blocked() {
            urgent_coord.set(UrgentCallOutcome::Failed);
            info!("Whitelist urgent call: no call any more on this call serial={serial}");
            return;
        }
        let Some((said_at, base_bytes)) = ctl.hold_said() else {
            continue;
        };
        let since_said = said_at.elapsed();
        if since_said >= urgent_call::HOLD_LINE_MAX_WAIT {
            info!(
                "Whitelist urgent call: hold-on line said {}s ago, dialing now serial={serial}",
                since_said.as_secs()
            );
            break;
        }
        if since_said < Duration::from_millis(600) {
            continue;
        }
        let spoke = delivered_bytes.load(Ordering::Relaxed) > base_bytes;
        if spoke && !play_head.generating() && !play_head.ai_audible(Duration::from_millis(400)) {
            info!(
                "Whitelist urgent call: hold-on line said and played ({}ms after hold-on) serial={serial}",
                since_said.as_millis()
            );
            break;
        }
    }
    let sim = call_sim_country(&serial);
    // SIM unknown: a number without a country code reads as the owner's country's.
    let home_code = state.settings_store.snapshot().effective_country_code();
    let number = match urgent_call::check_owner_number(&number, sim.as_deref(), &home_code) {
        Ok(n) => n,
        Err(why) => {
            warn!(
                "Whitelist urgent call: owner number {} not dialable ({why}) serial={serial} — alert only",
                urgent_call::masked(&number)
            );
            ctl.block_dial();
            urgent_coord.set(UrgentCallOutcome::Failed);
            push_call_events(&active_call, &media_call_guard, &urgent_call_failed_events("bad_number")).await;
            // The AI said its hold-on line and is waiting for the next instruction.
            ctl.resume_with(on_callback);
            return;
        }
    };
    if !ctl.block_dial() {
        urgent_coord.set(UrgentCallOutcome::Failed);
        return;
    }
    ctl.pause();
    // Any new media hello / ring on this phone is the merge at work, not a new call.
    urgent_call::mark_owner_on_call(&canonical_device_serial(&serial));
    let id = format!("urgent-{}", chrono::Utc::now().timestamp_millis());
    let mut rx = urgent_call::register(&id);
    let mut tracker = urgent_call::Tracker::default();
    info!(
        "Whitelist urgent call: add_call id={id} serial={serial} number={} — AI paused",
        urgent_call::masked(&number)
    );
    let sent = send_gemini_media_json(&sink, urgent_call::add_call_payload(&id, &number), "add_call", &media_call_guard).await;
    let mut step = match sent {
        Err(e) => {
            warn!("Whitelist urgent call: add_call not sent id={id} serial={serial}: {e:#}");
            urgent_call::Step::Failed("send_failed".to_string())
        }
        Ok(()) => {
            let start = tokio::time::Instant::now();
            let deadline = start + urgent_call::RESULT_WAIT;
            let mut heard = false;
            loop {
                if !media_call_guard.is_current() {
                    break urgent_call::Step::Failed("call_ended".to_string());
                }
                let now = tokio::time::Instant::now();
                if !heard && now >= start + urgent_call::DIAL_ACK_WAIT {
                    warn!(
                        "Whitelist urgent call: nothing from the phone in {}s id={id} serial={serial} — its app does not dial (older version?)",
                        urgent_call::DIAL_ACK_WAIT.as_secs()
                    );
                    tracker.on_timeout();
                    break urgent_call::Step::Failed("no_dial_ack".to_string());
                }
                if now >= deadline {
                    break tracker.on_timeout();
                }
                match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
                    Ok(Some(report)) => {
                        heard = true;
                        info!("Whitelist urgent call: id={id} serial={serial} phone says {report:?}");
                        match tracker.on_report(report) {
                            urgent_call::Step::Wait => {}
                            settled => break settled,
                        }
                    }
                    Ok(None) => break tracker.on_timeout(),
                    Err(_) => {}
                }
            }
        }
    };
    // No final result: the phone ends its second call and takes this one off hold
    // (merged already: it says so).
    let unsettled = matches!(&step, urgent_call::Step::Failed(r)
        if matches!(r.as_str(), "no_dial_ack" | "no_result" | "merge_timeout" | "phone_no_reply"));
    if unsettled && media_call_guard.is_current() {
        if let Err(e) = send_gemini_media_json(&sink, urgent_call::abort_payload(&id), "add_call_abort", &media_call_guard).await {
            warn!("Whitelist urgent call: add_call_abort not sent id={id}: {e:#}");
        }
        let until = tokio::time::Instant::now() + urgent_call::ABORT_WAIT;
        while let Ok(Some(report)) = tokio::time::timeout_at(until, rx.recv()).await {
            info!("Whitelist urgent call: id={id} serial={serial} after abort phone says {report:?}");
            match report {
                urgent_call::Report::Merged => {
                    step = urgent_call::Step::Merged;
                    break;
                }
                urgent_call::Report::Failed(r) if r == "unhold_failed" => {
                    step = urgent_call::Step::Failed(r);
                    break;
                }
                urgent_call::Report::Failed(_) => break,
                _ => {}
            }
        }
    }
    let reason = match &step {
        urgent_call::Step::Merged => None,
        urgent_call::Step::Failed(r) => Some(r.clone()),
        urgent_call::Step::Wait => Some("unknown".to_string()),
    };
    // Not merged (the caller hung up, the caller is still on hold, the AI's
    // session is gone, or the AI is back): this phone's next calls are the AI's
    // again, not the owner's call for up to three hours.
    settle_owner_mark(&canonical_device_serial(&serial), reason.as_deref());
    match reason {
        None => {
            // Mark handed/merged before any .await so a concurrent Gemini WS close
            // cannot emit a false Alert::AiEnded.
            ctl.hand_over();
            urgent_coord.set(UrgentCallOutcome::Merged);
            urgent_call::unregister(&id);
            push_call_events(&active_call, &media_call_guard, &["whitelist_urgent_call_merged".to_string()]).await;
            info!("Whitelist urgent call merged id={id} serial={serial} — the AI leaves, the call stays up");
        }
        Some(reason) if reason == "call_ended" => {
            urgent_coord.set(UrgentCallOutcome::Failed);
            urgent_call::unregister(&id);
            info!("Whitelist urgent call: call over id={id} serial={serial}");
        }
        Some(reason) => {
            if reason == "unhold_failed" {
                urgent_coord.set(UrgentCallOutcome::StillOnHold);
                cancel.store(true, Ordering::Relaxed);
            }
            push_call_events(&active_call, &media_call_guard, &urgent_call_failed_events(&reason)).await;
            match handle_urgent_call_failure_disposition(
                &sink,
                &serial,
                &reason,
                &ctl,
                &cancel,
                &urgent_coord,
                &media_call_guard,
            )
            .await
            {
                UrgentFailureDisposition::StillOnHold => {
                    // The AI would talk into a held call: it stays quiet, the owner is told.
                    urgent_call::unregister(&id);
                    warn!("Whitelist urgent call: caller still on hold id={id} serial={serial} — owner alerted, AI stays quiet");
                    send_urgent_call_alert(&state, &serial, &caller_num, urgent_call::Alert::StillOnHold).await;
                    return;
                }
                UrgentFailureDisposition::SessionGoneHangup => {
                    urgent_call::unregister(&id);
                    info!("Whitelist urgent call failed id={id} serial={serial} reason={reason} — no AI left (owner was alerted), sent hangup");
                    return;
                }
                UrgentFailureDisposition::ResumeAi => {}
            }
            info!("Whitelist urgent call failed id={id} serial={serial} reason={reason} — the AI is back");
            ctl.resume_with(if reason == "no_dial_ack" { on_callback } else { on_fail });
            urgent_coord.set(UrgentCallOutcome::Failed);
            // A merge the agent did not hear in time still hands the call over.
            if unsettled {
                let until = tokio::time::Instant::now() + urgent_call::LATE_MERGE_WAIT;
                while let Ok(Some(report)) = tokio::time::timeout_at(until, rx.recv()).await {
                    if report == urgent_call::Report::Merged && media_call_guard.is_current() {
                        warn!("Whitelist urgent call: late merge id={id} serial={serial} — the AI leaves, the call stays up");
                        ctl.hand_over();
                        urgent_coord.set(UrgentCallOutcome::Merged);
                        urgent_call::mark_owner_on_call(&canonical_device_serial(&serial));
                        push_call_events(&active_call, &media_call_guard, &["whitelist_urgent_call_merged".to_string()]).await;
                        break;
                    }
                }
            }
            urgent_call::unregister(&id);
        }
    }
}

/// After a whitelist urgent call settles on `phone`: merged (`reason` None)
/// keeps the owner-on-call mark until the phone is idle; any other end clears
/// it, so the phone's next ring goes to the AI again.
fn settle_owner_mark(phone: &str, reason: Option<&str>) {
    if let Some(reason) = reason {
        if urgent_call::clear_owner_on_call(phone) {
            info!("Whitelist urgent call over ({reason}) serial={phone} — owner-on-call mark cleared");
        }
    }
}

/// Paced player, hangup watcher and end-of-session handling (voicemail, fresh
/// retry, goodbye) for a started session.
#[allow(clippy::too_many_arguments)]
async fn run_gemini_session_to_end(
    core: GeminiSessionCore,
    sink: WsSink,
    state: AppState,
    serial: String,
    active_call: ActiveCallSlot,
    cancel: Arc<AtomicBool>,
    gemini_uplink_tx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    gemini_transcript: Arc<std::sync::Mutex<String>>,
    retry_cfg: Option<GeminiRetryCfg>,
    sample_rate: u32,
    audio_open: Arc<AtomicBool>,
    audio_opened_at: GeminiAudioOpenedAt,
    setup_ready: Arc<AtomicBool>,
    connect_failed: Arc<AtomicBool>,
    prewarm: bool,
    fail_vm: Option<(Arc<std::sync::Mutex<IvrPhase>>, Arc<Mutex<()>>)>,
    goodbye_hangup_delay_ms: u64,
    media_call_guard: MediaCallGuard,
    persona: bool,
) {
    let GeminiSessionCore {
        session,
        dl_rx,
        force_hangup,
        delivered_bytes,
        play_head,
        stall_engaged,
        stall_judgement,
        appointments,
        urgent_reason,
        urgent_call: urgent_ctl,
        caller,
        ..
    } = core;
    let caller_at_end = caller.clone();
    // What the caller agrees to goes with this call's record.
    if let Some(book) = appointments {
        if let Some(_lease) = media_call_guard.lease_current().await {
            if let Some(c) = active_call.lock().await.as_mut() {
                c.set_appointments(book);
            }
        }
    }
    // Persona mode: the model's own closing is the goodbye — no clone goodbye WAV.
    let play_goodbye_wav = !persona;
    let hangup_sent = Arc::new(AtomicBool::new(false));
    let session_done = Arc::new(AtomicBool::new(false));

    let play = {
        let sink = sink.clone();
        let state = state.clone();
        let active_call = active_call.clone();
        let cancel = cancel.clone();
        let delivered_bytes = delivered_bytes.clone();
        let play_head = play_head.clone();
        let serial = serial.clone();
        let media_call_guard = media_call_guard.clone();
        let urgent_ctl = urgent_ctl.clone();
        tokio::spawn(async move {
            stream_gemini_downlink_pcm(
                &sink,
                &state,
                dl_rx,
                &active_call,
                &cancel,
                &delivered_bytes,
                &play_head,
                &serial,
                &media_call_guard,
                urgent_ctl.as_deref(),
            )
            .await
        })
    };

    let urgent_coord = UrgentCallCoord::new();

    // Concurrent hangup watcher: fire WS hangup after gemini arms force_hangup
    // (player already drained). Does not wait for Live WS fully closed / cancel race.
    let hangup_watch = {
        let sink = sink.clone();
        let state = state.clone();
        let active_call = active_call.clone();
        let serial = serial.clone();
        let force_hangup = force_hangup.clone();
        let hangup_sent = hangup_sent.clone();
        let cancel = cancel.clone();
        let session_done = session_done.clone();
        let play_head = play_head.clone();
        let media_call_guard = media_call_guard.clone();
        let stall_engaged = stall_engaged.clone();
        let stall_judgement = stall_judgement.clone();
        let urgent_reason = urgent_reason.clone();
        let urgent_ctl = urgent_ctl.clone();
        let urgent_coord_w = urgent_coord.clone();
        let delivered_bytes = delivered_bytes.clone();
        let transcript_w = gemini_transcript.clone();
        tokio::spawn(async move {
            let mut stall_recorded = false;
            let mut urgent_recorded = false;
            let mut judged_seen: Option<calls::ScamJudgement> = None;
            loop {
                if !media_call_guard.is_current() {
                    info!("Gemini hangup watcher invalidated serial={serial}");
                    break;
                }
                // Whitelist urgent call: when the AI says its strong hold-on line, it
                // triggers the urgent alert and dial even if the caller's phrasing was
                // not in the keyword table.
                if !urgent_recorded {
                    if let Some(ctl) = urgent_ctl.as_ref() {
                        if ctl.hold_said_at().is_some() {
                            let said = transcript_w.lock().map(|g| g.clone()).unwrap_or_default();
                            let tail: String = {
                                let chars: Vec<char> = said.trim().chars().collect();
                                chars[chars.len().saturating_sub(200)..].iter().collect()
                            };
                            if let Ok(mut g) = urgent_reason.lock() {
                                if g.as_deref().unwrap_or("").trim().is_empty() && !tail.is_empty() {
                                    *g = Some(tail);
                                } else if g.is_none() {
                                    *g = Some(String::new());
                                }
                            }
                        }
                    }
                }
                // A call judged sales / scam (stalling started, or a judgement that
                // stands): no urgent alert and no call to the owner, whoever calls.
                // A call to the owner already dialing goes on.
                if !urgent_recorded
                    && urgent_reason.lock().is_ok_and(|g| g.is_some())
                    && urgent_call::urgent_blocked_by_scam(
                        stall_engaged.load(Ordering::Relaxed),
                        stall_judgement.lock().ok().and_then(|g| g.clone()).as_ref(),
                    )
                {
                    urgent_recorded = true;
                    if let Some(ctl) = urgent_ctl.as_ref() {
                        ctl.block_dial();
                    }
                    info!("urgent matter on a call judged sales / scam serial={serial} — no alert, no call to the owner");
                }
                if !urgent_recorded {
                    if let Some(reason) = urgent_reason.lock().ok().and_then(|g| g.clone()) {
                        urgent_recorded = true;
                        let caller_num = caller.lock().ok().map(|g| g.clone()).unwrap_or_default();
                        if let Some(_lease) = media_call_guard.lease_current().await {
                            let mut slot = active_call.lock().await;
                            if let Some(c) = slot.as_mut() {
                                c.push_event("whitelist_urgent_transfer");
                            }
                        }
                        let http = state.http.clone();
                        let rt = state.settings_store.snapshot();
                        let settings = rt.effective(&state.settings);
                        let sim = call_sim_country(&serial);
                        let ct_name = contacts::lookup(&caller_num, sim.as_deref()).map(|c| c.name);
                        let caller_disp = contacts::named_number(
                            &display_caller_id(&caller_num),
                            ct_name.as_deref(),
                        );
                        let dev = notify::device_label(&serial);
                        let fwd = rt.whitelist_forward_number.trim().to_string();
                        let lang = locale::notify_lang();
                        let p = locale::phrases(lang);
                        let urg_title = urgent_call::alert_title(lang);
                        let (urg_label, fwd_label, urg_footer) = match lang {
                            locale::Lang::ZhTw => ("緊急內容", "轉接號碼", "請立即回電或接手處理！"),
                            locale::Lang::ZhCn => ("紧急内容", "转接号码", "请立即回电或接手处理！"),
                            locale::Lang::Ja => ("緊急の用件", "転送先番号", "直ちに折り返すか対応してください！"),
                            locale::Lang::Ko => ("긴급 내용", "전환 번호", "즉시 회신하거나 확인해 주세요!"),
                            locale::Lang::Es => ("Detalle urgente", "Número de desvío", "¡Por favor devuelve la llamada o atiende de inmediato!"),
                            locale::Lang::Fr => ("Urgence", "Numéro de transfert", "Veuillez rappeler ou prendre le relais immédiatement !"),
                            locale::Lang::De => ("Dringendes Anliegen", "Weiterleitungsnummer", "Bitte sofort zurückrufen oder übernehmen!"),
                            locale::Lang::Pt => ("Assunto urgente", "Número de transferência", "Por favor, retorne a ligação ou assuma imediatamente!"),
                            locale::Lang::En => ("Urgent matter", "Forwarding number", "Please call back or take over immediately!"),
                        };
                        let fwd_line = if fwd.is_empty() {
                            String::new()
                        } else {
                            format!("\n{fwd_label}: {fwd}")
                        };
                        let alert = format!(
                            "{urg_title}\n{}: {caller_disp}\n{}: {dev}\n{urg_label}: {reason}{fwd_line}\n{urg_footer}",
                            p.s_caller, p.n_phone
                        );
                        tokio::spawn(async move {
                            let _ = notify::send_text_alert(&http, &settings, "whitelist_urgent_transfer", urg_title, &alert).await;
                        });
                        // `whitelist_mode` emergency with the owner's number set: the phone dials the
                        // owner and puts them through; the alert above goes out either way.
                        if let Some(ctl) = urgent_ctl.clone().filter(|c| !c.dial_blocked()) {
                            let on_fail = urgent_call::not_answered_turn(rt.lang(), rt.is_assistant_style(), &rt.owner_name);
                            let on_callback = urgent_call::callback_turn(rt.lang(), rt.is_assistant_style(), &rt.owner_name);
                            match urgent_call::number_to_call(&rt) {
                                // A number not in the contacts: its allowance is used now.
                                Some(number)
                                    if ctl
                                        .limit_key()
                                        .is_none_or(|k| urgent_call::stranger_dials().take(k, std::time::Instant::now())) =>
                                {
                                    tokio::spawn(run_urgent_call(
                                        sink.clone(),
                                        state.clone(),
                                        serial.clone(),
                                        caller_num.clone(),
                                        number,
                                        ctl,
                                        active_call.clone(),
                                        media_call_guard.clone(),
                                        play_head.clone(),
                                        delivered_bytes.clone(),
                                        on_fail,
                                        on_callback,
                                        cancel.clone(),
                                        urgent_coord_w.clone(),
                                    ));
                                }
                                Some(_) => {
                                    info!("Urgent call: this number was put through less than 30 minutes ago serial={serial} — alert only");
                                    ctl.block_dial();
                                    if ctl.hold_said_at().is_some() {
                                        ctl.resume_with(on_callback);
                                    }
                                }
                                None => {
                                    info!("Whitelist urgent call turned off during the call serial={serial} — no call");
                                    ctl.block_dial();
                                    if ctl.hold_said_at().is_some() {
                                        ctl.resume_with(on_callback);
                                    }
                                }
                            }
                        }
                    }
                }
                // Record while the call is live: the summary notification is built at call_end.
                if !stall_recorded && stall_engaged.load(Ordering::Relaxed) {
                    stall_recorded = true;
                    if let Some(_lease) = media_call_guard.lease_current().await {
                        let mut slot = active_call.lock().await;
                        if let Some(c) = slot.as_mut() {
                            c.push_event("persona_stall_spam");
                        }
                    }
                }
                // How it was judged (kind of call, let go again), and judged with
                // stalling off: the record says so too.
                let judged_now = stall_judgement.lock().ok().and_then(|g| g.clone());
                if judged_now != judged_seen {
                    if let Some(j) = judged_now.clone() {
                        if let Some(_lease) = media_call_guard.lease_current().await {
                            let mut slot = active_call.lock().await;
                            if let Some(c) = slot.as_mut() {
                                if !stall_engaged.load(Ordering::Relaxed) && !c.has_event("persona_scam_detected") {
                                    c.push_event("persona_scam_detected");
                                }
                                if j.released && !c.has_event("persona_stall_released") {
                                    c.push_event("persona_stall_released");
                                }
                                c.set_scam(j);
                            }
                        }
                    }
                    judged_seen = judged_now;
                }
                if force_hangup.load(Ordering::Relaxed) && !urgent_ctl.as_ref().is_some_and(|c| c.blocks_hangup()) {
                    do_gemini_goodbye_hangup(
                        &sink,
                        &state,
                        &serial,
                        &active_call,
                        &cancel,
                        &hangup_sent,
                        "goodbye",
                        play_goodbye_wav, // farewell WAV once (off in persona mode)
                        &media_call_guard,
                        &play_head,
                    )
                    .await;
                    break;
                }
                if session_done.load(Ordering::Relaxed) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };

    let result = match session.await {
        Ok(r) => r,
        Err(e) => Err(anyhow::anyhow!("gemini session join: {e}")),
    };
    session_done.store(true, Ordering::Relaxed);
    let delivery = match play.await {
        Ok(r) => r,
        Err(e) => Err(anyhow::anyhow!("gemini player join: {e}")),
    };
    let _ = hangup_watch.await;

    if let Ok(mut g) = gemini_uplink_tx.lock() {
        *g = None;
    }
    if !media_call_guard.is_current() {
        info!("Gemini completion ignored for stale media call serial={serial}");
        return;
    }
    // Whitelist urgent call: the phone dials the owner (or the owner is on). No
    // voicemail, fresh retry or hangup may touch this call now; a session that
    // ended while dialing leaves nobody to talk to the caller: the owner is told,
    // except when a concurrent merge or StillOnHold already resolved it.
    match finish_urgent_session_end(
        urgent_ctl.as_deref(),
        &urgent_coord,
        URGENT_END_MERGE_GRACE,
    )
    .await
    {
        UrgentEndAction::Usual => {}
        UrgentEndAction::Handed => {
            let reason = match &result {
                Ok(r) => r.as_str().to_string(),
                Err(e) => format!("{e:#}"),
            };
            info!(
                "Gemini Live ended during a whitelist urgent call serial={serial} handed=true reason={reason} — no hangup, voicemail or retry"
            );
            return;
        }
        UrgentEndAction::StillOnHold => {
            info!(
                "Gemini Live ended after unhold_failed during a whitelist urgent call serial={serial} — owner already alerted (StillOnHold), no duplicate AiEnded"
            );
            return;
        }
        UrgentEndAction::AlertAiEnded => {
            let reason = match &result {
                Ok(r) => r.as_str().to_string(),
                Err(e) => format!("{e:#}"),
            };
            info!(
                "Gemini Live ended during a whitelist urgent call serial={serial} handed=false reason={reason} — no hangup, voicemail or retry"
            );
            push_call_events(&active_call, &media_call_guard, &["whitelist_urgent_call_ai_ended".to_string()]).await;
            let caller_num = caller_at_end.lock().map(|g| g.clone()).unwrap_or_default();
            send_urgent_call_alert(&state, &serial, &caller_num, urgent_call::Alert::AiEnded).await;
            return;
        }
    }
    match (result, delivery) {
        (_, Err(e)) => {
            warn!(
                "Gemini Live delivery failed serial={serial} err={e:#} -> auto voicemail (prewarm={prewarm})"
            );
            clear_gemini_delivery_state(
                &active_call,
                &media_call_guard,
                "gemini_delivery_fail_to_voicemail",
            )
            .await;
            if let Some((phase, busy)) = fail_vm.as_ref() {
                if !media_call_guard.is_current() {
                    return;
                }
                let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
                if !matches!(p, IvrPhase::Done | IvrPhase::Goodbye) {
                    start_voicemail_intro_for_gemini(
                        &sink,
                        &state,
                        &serial,
                        phase,
                        busy,
                        &active_call,
                        &media_call_guard,
                    );
                }
            }
        }
        (Ok(reason), Ok(stats)) if stats.bytes == 0 => {
            let was_open = audio_open.load(Ordering::Relaxed);
            if !was_open {
                connect_failed.store(true, Ordering::Relaxed);
                warn!(
                    "Gemini Live ConnectAtAnswer ended without delivered audio serial={serial} reason={} -- soft until UnmuteAfterGreet",
                    reason.as_str()
                );
            } else {
                warn!(
                    "Gemini Live ended without delivered audio serial={serial} reason={} frames={} turns={} -> auto voicemail",
                    reason.as_str(), stats.frames, stats.turns
                );
                if let Some(cfg) = retry_cfg.clone() {
                    if spawn_gemini_fresh_retry(
                        &sink,
                        &state,
                        &serial,
                        &active_call,
                        &cancel,
                        &gemini_uplink_tx,
                        &gemini_transcript,
                        cfg,
                        sample_rate,
                        &audio_open,
                        &audio_opened_at,
                        &setup_ready,
                        &connect_failed,
                        fail_vm.clone(),
                        goodbye_hangup_delay_ms,
                        &media_call_guard,
                        persona,
                        reason.as_str(),
                    ) {
                        return;
                    }
                }
                clear_gemini_delivery_state(
                    &active_call,
                    &media_call_guard,
                    "gemini_zero_delivery_to_voicemail",
                )
                .await;
                if let Some((phase, busy)) = fail_vm.as_ref() {
                    if !media_call_guard.is_current() {
                        return;
                    }
                    let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
                    if !matches!(p, IvrPhase::Done | IvrPhase::Goodbye) {
                        start_voicemail_intro_for_gemini(
                            &sink,
                            &state,
                            &serial,
                            phase,
                            busy,
                            &active_call,
                            &media_call_guard,
                        );
                    }
                }
            }
        }
        (Ok(reason), Ok(stats)) => {
            info!(
                "Gemini Live session ok serial={serial} reason={} delivered_bytes={} frames={} turns={}",
                reason.as_str(),
                stats.bytes,
                stats.frames,
                stats.turns
            );
            if reason == gemini_live::SessionEndReason::IvrHangup {
                let mut slot = active_call.lock().await;
                if let Some(c) = slot.as_mut() {
                    c.push_event("ivr_hangup");
                }
            }
            // Always hangup on agent-initiated end — even if cancel raced mid-drain.
            // (Old gate `!cancel` skipped hangup after WS glitch during the goodbye, leaving line open.)
            // Whitelist urgent call: the owner is on (or being dialed): never hang up.
            let urgent_hold = urgent_ctl.as_ref().is_some_and(|c| c.blocks_hangup());
            if urgent_hold {
                info!(
                    "Gemini Live ended during a whitelist urgent call serial={serial} reason={} — call left up",
                    reason.as_str()
                );
            }
            if reason.should_play_goodbye() && !urgent_hold {
                do_gemini_goodbye_hangup(
                    &sink,
                    &state,
                    &serial,
                    &active_call,
                    &cancel,
                    &hangup_sent,
                    reason.as_str(),
                    play_goodbye_wav,
                    &media_call_guard,
                    &play_head,
                )
                .await;
            }
        }
        (Err(e), Ok(stats)) => {
            warn!(
                "Gemini Live error delivery stats serial={serial} delivered_bytes={} frames={} turns={}",
                stats.bytes, stats.frames, stats.turns
            );
            let was_open = audio_open.load(Ordering::Relaxed);
            if !was_open {
                connect_failed.store(true, Ordering::Relaxed);
                warn!(
                    "Gemini Live ConnectAtAnswer fail serial={serial} err={e:#} — soft until UnmuteAfterGreet"
                );
            } else if !cancel.load(Ordering::Relaxed) {
                warn!(
                    "Gemini Live session failed serial={serial} err={e:#} → auto voicemail (prewarm={prewarm})"
                );
                if stats.bytes == 0 {
                    if let Some(cfg) = retry_cfg.clone() {
                        let why = format!("{e:#}");
                        if spawn_gemini_fresh_retry(
                            &sink,
                            &state,
                            &serial,
                            &active_call,
                            &cancel,
                            &gemini_uplink_tx,
                            &gemini_transcript,
                            cfg,
                            sample_rate,
                            &audio_open,
                            &audio_opened_at,
                            &setup_ready,
                            &connect_failed,
                            fail_vm.clone(),
                            goodbye_hangup_delay_ms,
                            &media_call_guard,
                            persona,
                            &why,
                        ) {
                            return;
                        }
                    }
                }
                clear_gemini_delivery_state(
                    &active_call,
                    &media_call_guard,
                    "gemini_fail_to_voicemail",
                )
                .await;
                if let Some((phase, busy)) = fail_vm.as_ref() {
                    if !media_call_guard.is_current() {
                        return;
                    }
                    start_voicemail_intro_for_gemini(
                        &sink,
                        &state,
                        &serial,
                        phase,
                        busy,
                        &active_call,
                        &media_call_guard,
                    );
                }
            }
        }
    }
}

/// What a fresh Live session needs that the first one consumed.
#[derive(Clone)]
struct GeminiRetryCfg {
    api_key: String,
    model: String,
    system: String,
    voice_name: String,
    /// Caller ID of the call (empty when unknown): calls from abroad hold the
    /// opening for the carrier's international-call prompt.
    caller_id: String,
    /// The call's languages: opening line and the caller's speech-to-text hints
    /// ([`gemini_live::call_lang_plan`]).
    lang: gemini_live::CallLangPlan,
}

/// One fresh Live socket after a session that never delivered audio.
///
/// Seen on a real call: Gemini answered the kick with a 「喂？你好。」 transcript and
/// 0 audio, the same-socket re-kick did the same, and persona mode hung up after 5 s
/// of silence. A new session a few minutes later worked —
/// so retry once on a new WS (same call, same cancel flag + uplink slot, so a
/// hangup still tears it down). Returns false when the call is no longer in
/// LiveAi (caller hung up / goodbye) — the caller then runs the old fallback.
#[allow(clippy::too_many_arguments)]
fn spawn_gemini_fresh_retry(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    active_call: &ActiveCallSlot,
    cancel: &Arc<AtomicBool>,
    gemini_uplink_tx: &Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    gemini_transcript: &Arc<std::sync::Mutex<String>>,
    cfg: GeminiRetryCfg,
    sample_rate: u32,
    audio_open: &Arc<AtomicBool>,
    audio_opened_at: &GeminiAudioOpenedAt,
    setup_ready: &Arc<AtomicBool>,
    connect_failed: &Arc<AtomicBool>,
    fail_vm: Option<(Arc<std::sync::Mutex<IvrPhase>>, Arc<Mutex<()>>)>,
    goodbye_hangup_delay_ms: u64,
    media_call_guard: &MediaCallGuard,
    persona: bool,
    why: &str,
) -> bool {
    if cancel.load(Ordering::Relaxed) || !media_call_guard.is_current() {
        return false;
    }
    let Some((phase, busy)) = fail_vm else {
        return false;
    };
    {
        let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(p, IvrPhase::LiveAi) {
            info!("Gemini fresh retry skipped serial={serial} phase={p:?}");
            return false;
        }
    }
    let (up_tx, up_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    if let Ok(mut g) = gemini_uplink_tx.lock() {
        *g = Some(up_tx);
    }
    // The dead session's transcript is text the caller never heard.
    if let Ok(mut g) = gemini_transcript.lock() {
        g.clear();
    }
    // Cold path on the new socket: dialogue open immediately, kick after setup.
    setup_ready.store(false, Ordering::Relaxed);
    connect_failed.store(false, Ordering::Relaxed);
    if let Ok(mut t) = audio_opened_at.lock() {
        *t = Some(tokio::time::Instant::now());
    }
    audio_open.store(true, Ordering::Relaxed);
    // The extended-thinking model answers the opening with text only more often
    // (in test calls about 3 of 10 re-kicked, 1 needed this retry): the
    // last try goes on the stall model (`gemini-3.8-live`) instead.
    let mut cfg = cfg;
    let fallback = state.settings_store.snapshot().gemini_stall_model.trim().to_string();
    if gemini_live::live_thinking_level(&cfg.model, "").is_some() && !fallback.is_empty() {
        cfg.model = fallback;
    }
    warn!(
        "Gemini Live no audio delivered serial={serial} why={why} → fresh Live session (1 retry, new WS) model={}",
        cfg.model
    );

    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let active_call = active_call.clone();
    let cancel = cancel.clone();
    let gemini_uplink_tx = gemini_uplink_tx.clone();
    let gemini_transcript = gemini_transcript.clone();
    let audio_open = audio_open.clone();
    let audio_opened_at = audio_opened_at.clone();
    let setup_ready = setup_ready.clone();
    let connect_failed = connect_failed.clone();
    let media_call_guard = media_call_guard.clone();
    let fut = async move {
        {
            let Some(_lease) = media_call_guard.lease_current().await else {
                return;
            };
            let mut slot = active_call.lock().await;
            if let Some(c) = slot.as_mut() {
                c.set_tts_outbound_active(false);
                c.set_ai_segment_active(true);
                c.push_event("gemini_zero_audio_reconnect");
            }
        }
        spawn_gemini_session_tasks(
            sink,
            state,
            serial,
            active_call,
            cancel,
            up_rx,
            gemini_uplink_tx,
            gemini_transcript,
            cfg.api_key,
            cfg.model,
            cfg.system,
            sample_rate,
            audio_open,
            audio_opened_at,
            setup_ready,
            connect_failed,
            false, // not a prewarm: audio already open
            Some((phase, busy)),
            cfg.voice_name,
            goodbye_hangup_delay_ms,
            media_call_guard,
            persona,
            false, // no second retry → old fallback (persona-mode hangup + notification)
            cfg.caller_id,
        )
        .await;
    };
    // Boxed + Send: this fn is reached from inside `spawn_gemini_session_tasks`.
    let fut: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> = Box::pin(fut);
    tokio::spawn(fut);
    true
}

async fn clear_gemini_delivery_state(
    active_call: &ActiveCallSlot,
    media_call_guard: &MediaCallGuard,
    event: &str,
) {
    let Some(_lease) = media_call_guard.lease_current().await else {
        return;
    };
    let mut slot = active_call.lock().await;
    if let Some(call) = slot.as_mut() {
        call.set_tts_outbound_active(false);
        call.set_ai_segment_active(false);
        call.push_event(event);
    }
}

async fn wait_play_head_drain(play_head: &gemini_live::DownlinkPlayHead, serial: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        if play_head.is_drained_slack(gemini_live::PLAYER_DRAIN_SLACK_BYTES) {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            warn!(
                "Goodbye WAV drain wait timeout serial={serial} queued={} outbound={} — playing WAV anyway"
            , play_head.queued(), play_head.is_outbound());
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Clone goodbye WAV (optional) + WS hangup ×3. Idempotent via `hangup_sent`.
/// Waits for the Gemini player to drain the current turn before WAV / hangup.
async fn do_gemini_goodbye_hangup(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    active_call: &ActiveCallSlot,
    cancel: &Arc<AtomicBool>,
    hangup_sent: &Arc<AtomicBool>,
    end_reason: &str,
    play_wav: bool,
    media_call_guard: &MediaCallGuard,
    play_head: &Arc<gemini_live::DownlinkPlayHead>,
) {
    if !media_call_guard.is_current() {
        info!("Gemini goodbye skipped for stale media call serial={serial}");
        return;
    }
    if hangup_sent.swap(true, Ordering::Relaxed) {
        return;
    }
    wait_play_head_drain(play_head.as_ref(), serial).await;
    if !media_call_guard.is_current() {
        return;
    }
    cancel.store(true, Ordering::Relaxed);
    let bye = state
        .ivr_goodbye_pcm
        .as_ref()
        .map(|p| p.as_ref().clone())
        .unwrap_or_default();
    let spoken = state.settings.ivr_goodbye_text.clone();
    info!(
        "Goodbye detected → hangup serial={serial} reason={end_reason} play_wav={play_wav} pcm_bytes={}",
        bye.len()
    );
    if play_wav {
        let _ = send_gemini_media_json(
            sink,
            json!({
                "type": "reply_text",
                "text": spoken,
                "source": "gemini_end_goodbye",
                "end_reason": end_reason,
            }),
            "goodbye reply_text",
            media_call_guard,
        )
        .await;
    }
    if !media_call_guard.is_current() {
        return;
    }
    if play_wav && !bye.is_empty() {
        let _ = send_tts_pcm_guarded(
            sink,
            state,
            &bye,
            active_call,
            "ivr_goodbye_wav",
            None,
            Some(media_call_guard),
        )
        .await;
    }
    if !media_call_guard.is_current() {
        return;
    }
    // Hangup always (3 retries) — Android LiveAi must receive disconnect.
    let hangup_reason = format!("gemini_live_{end_reason}");
    send_hangup_with_retries(sink, serial, &hangup_reason, 3, Some(media_call_guard)).await;
}

/// Send WS hangup several times (bridge / phone may drop one packet).
async fn send_hangup_with_retries(
    sink: &WsSink,
    serial: &str,
    reason: &str,
    attempts: u32,
    media_call_guard: Option<&MediaCallGuard>,
) {
    let attempts = attempts.max(1);
    for i in 1..=attempts {
        if media_call_guard
            .map(|guard| !guard.is_current())
            .unwrap_or(false)
        {
            info!("hangup retries stopped for stale media call serial={serial} reason={reason}");
            return;
        }
        let payload = json!({
            "type": "hangup",
            "reason": reason,
            "serial": serial,
            "attempt": i,
        });
        let sent = if media_call_guard.is_some() {
            send_gemini_media_json(
                sink,
                payload,
                "hangup",
                media_call_guard.expect("guarded hangup"),
            )
            .await
        } else {
            send_json(sink, payload).await
        };
        match sent {
            Ok(()) => {
                info!("hangup sent serial={serial} reason={reason} attempt={i}/{attempts}");
            }
            Err(e) => {
                warn!("hangup send fail serial={serial} attempt={i}/{attempts}: {e:#}");
            }
        }
        if i < attempts {
            tokio::time::sleep(Duration::from_millis(280)).await;
        }
    }
}

/// Gemini Live AI (official voice). Key from /calls/ settings.
/// On start/session failure → auto voicemail (not fail-hangup alone).
fn start_gemini_live_after_choice(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_started: &mut bool,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    rt: &Arc<RuntimeSettings>,
    gemini_transcript: &Arc<std::sync::Mutex<String>>,
    gemini_uplink_tx: &Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    sample_rate: u32,
    play_connect_wait: bool,
    gemini_audio_open: &Arc<AtomicBool>,
    gemini_audio_opened_at: &GeminiAudioOpenedAt,
    gemini_setup_ready: &Arc<AtomicBool>,
    gemini_connect_failed: &Arc<AtomicBool>,
    _reserved_prewarm: bool,
    media_call_guard: &MediaCallGuard,
) {
    if !media_call_guard.is_current() {
        info!("Gemini cold start skipped for stale media call serial={serial}");
        return;
    }
    if *hold_started {
        warn!("IVR Gemini ignored serial={serial} (hold_started already true)");
        return;
    }
    stop_hold_loop(hold_cancel);
    *hold_started = true;
    *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::LiveAi;
    let cancel = Arc::new(AtomicBool::new(false));
    *hold_cancel = Some(cancel.clone());
    let (up_tx, up_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    if let Ok(mut g) = gemini_uplink_tx.lock() {
        *g = Some(up_tx);
    }
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let active_call = active_call.clone();
    let phase = phase.clone();
    let busy = busy.clone();
    let api_key = rt.gemini_api_key.clone();
    let model = rt.gemini_live_model.clone();
    let system = rt.gemini_system_prompt_for_call();
    let voice_name = if rt.gemini_voice_name.trim().is_empty() {
        gemini_live::default_voice_name().to_string()
    } else {
        rt.gemini_voice_name.clone()
    };
    let goodbye_hangup_delay_ms = u64::from(state.settings.goodbye_hangup_delay_ms);
    let gemini_uplink_tx = gemini_uplink_tx.clone();
    let gemini_transcript = gemini_transcript.clone();
    let wait_text = state.settings.ivr_dtmf1_wait_text.clone();
    let persona = rt.persona_flow_active();
    // Persona mode never plays the "please hold" connect-wait clip.
    let play_connect_wait = play_connect_wait && !persona;
    let audio_open = gemini_audio_open.clone();
    let audio_opened_at = gemini_audio_opened_at.clone();
    let setup_ready = gemini_setup_ready.clone();
    let connect_failed = gemini_connect_failed.clone();
    let media_call_guard = media_call_guard.clone();
    // Cold start: speech allowed immediately (no greet race when called after greets).
    if let Ok(mut opened_at) = audio_opened_at.lock() {
        *opened_at = Some(tokio::time::Instant::now());
    }
    audio_open.store(true, Ordering::Relaxed);
    setup_ready.store(false, Ordering::Relaxed);
    connect_failed.store(false, Ordering::Relaxed);

    tokio::spawn(async move {
        if !media_call_guard.is_current() {
            info!("Gemini cold spawn invalidated serial={serial}");
            return;
        }
        info!(
            "Gemini Live cold start serial={serial} model={} voice={} phone_sr={sample_rate} play_connect_wait={play_connect_wait}",
            if model.trim().is_empty() {
                gemini_live::default_model()
            } else {
                model.as_str()
            },
            voice_name,
        );

        // Optional connect-wait (auto-AI skips: period greeting already said "please hold").
        if play_connect_wait {
            let (source, pcm) = if let Some(p) = state.ivr_dtmf1_wait_pcm.clone() {
                ("ivr_dtmf1_wait_wav", p.as_ref().clone())
            } else {
                warn!(
                    "Gemini wait WAV missing serial={serial}; silence (no Piper) text={wait_text:?}"
                );
                ("ivr_dtmf1_wait_missing", Vec::new())
            };
            info!(
                "Gemini wait prompt serial={serial} source={source} pcm_bytes={} (~{:.1}s full)",
                pcm.len(),
                pcm.len() as f64 / (state.settings.sample_rate as f64 * 2.0)
            );
            if !pcm.is_empty() && !cancel.load(Ordering::Relaxed) {
                if !media_call_guard.is_current() {
                    return;
                }
                let wait_ms = ((pcm.len() as u64) * 1000)
                    / ((state.settings.sample_rate as u64).saturating_mul(2).max(1));
                let wait_ms = wait_ms.clamp(800, 12_000);
                info!(
                    "Gemini wait local-only serial={serial} source={source} wait_ms={wait_ms} (no PCM stream)"
                );
                let _ = send_gemini_media_json(
                    &sink,
                    json!({
                        "type": "tts_start",
                        "sample_rate": state.settings.sample_rate,
                        "source": "gemini_wait",
                        "pcm_bytes": pcm.len(),
                        "local_wait": true,
                    }),
                    "gemini wait tts_start",
                    &media_call_guard,
                )
                .await;
                let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
                while tokio::time::Instant::now() < deadline {
                    if cancel.load(Ordering::Relaxed) || !media_call_guard.is_current() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(40)).await;
                }
                if media_call_guard.is_current() {
                    let _ = send_gemini_media_json(
                        &sink,
                        json!({
                            "type": "tts_end",
                            "source": "gemini_wait",
                            "local_wait": true,
                        }),
                        "gemini wait tts_end",
                        &media_call_guard,
                    )
                    .await;
                }
                info!("Gemini wait finished serial={serial} — now starting Live AI");
            }
        }

        if cancel.load(Ordering::Relaxed) || !media_call_guard.is_current() {
            info!("Gemini aborted before Live (hangup) serial={serial}");
            if let Ok(mut g) = gemini_uplink_tx.lock() {
                *g = None;
            }
            return;
        }

        {
            let Some(_call_lease) = media_call_guard.lease_current().await else {
                return;
            };
            let mut slot = active_call.lock().await;
            if let Some(c) = slot.as_mut() {
                c.set_ai_segment_active(true);
            }
        }
        if !media_call_guard.is_current() {
            return;
        }
        info!("Gemini AI segment capture ON serial={serial}");

        spawn_gemini_session_tasks(
            sink,
            state,
            serial,
            active_call,
            cancel,
            up_rx,
            gemini_uplink_tx,
            gemini_transcript,
            api_key,
            model,
            system,
            sample_rate,
            audio_open,
            audio_opened_at,
            setup_ready,
            connect_failed,
            false,
            Some((phase, busy)),
            voice_name,
            goodbye_hangup_delay_ms,
            media_call_guard,
            persona,
            true, // 0 audio → one fresh Live session first
            String::new(), // cold start after the menu: nothing held to drop
        )
        .await;
    });
}

/// DTMF 1 (AI call): TRTC when armed; else brief local goodbye + hangup (not hold music).
/// Decision helper: [`trtc::TrtcSettings::arm_ai_after_dtmf1`].
fn start_trtc_ai_after_choice(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    hold_started: &mut bool,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    busy: &Arc<Mutex<()>>,
    rt: &Arc<RuntimeSettings>,
    trtc_ai_heard_voice: &mut Option<Arc<AtomicBool>>,
    trtc_turn: &mut Option<Arc<TrtcTurnTracker>>,
) {
    if *hold_started {
        // Silent return made "press 1 again" look dead — always log.
        warn!(
            "IVR DTMF 1 ignored serial={serial} (hold_started already true — wait/AI arm already running)"
        );
        return;
    }
    stop_hold_loop(hold_cancel);
    *hold_started = true;
    // Only clear leftovers when a prior AI task is still latched (second call bug).
    // Always-leave here was racing menu open_session and delaying wait inject.
    if trtc_bridge::peek_ai_task_id(serial).is_some() {
        warn!("IVR DTMF 1 prearm cleanup serial={serial} (stale TaskId present)");
        let br = trtc_bridge::make_bridge(&state.settings.trtc(), state.http.clone());
        br.leave(serial, "dtmf1_prearm_cleanup");
    }
    match state.settings.trtc().arm_ai_after_dtmf1(serial) {
        trtc::TrtcAiArm::Scaffold { sdk_app_id } => {
            *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::TrtcAi;
            let heard = Arc::new(AtomicBool::new(false));
            *trtc_ai_heard_voice = Some(heard.clone());
            let turn = Arc::new(TrtcTurnTracker::default());
            *trtc_turn = Some(turn.clone());
            let wait_text = state.settings.ivr_dtmf1_wait_text.clone();
            let sink = sink.clone();
            let state = state.clone();
            let serial = serial.to_string();
            let active_call = active_call.clone();
            let phase = phase.clone();
            let busy = busy.clone();
            let rt = rt.clone();
            let talk_metrics = state.trtc_talk.clone();
            // Product path: arm media+StartAI first, play short wait, then **must**
            // ServerPush AI greeting so realtime talk is audible (STT alone stays silent
            // if uplink is late / empty — live logs: joined but uplink/downlink bytes=0).
            let cancel_for_parent = Arc::new(AtomicBool::new(false));
            *hold_cancel = Some(cancel_for_parent.clone());
            tokio::spawn(async move {
                let bridge: Arc<dyn TrtcBridge> = Arc::from(trtc_bridge::make_bridge(
                    &state.settings.trtc(),
                    state.http.clone(),
                ));
                let trtc_cfg = state.settings.trtc();
                info!(
                    "TrtcAi runtime config serial={serial} llm_ready={} tts_ready={} max_idle_secs={} (secrets masked)",
                    !trtc_cfg.llm_config.as_str().trim().is_empty(),
                    !trtc_cfg.tts_config.as_str().trim().is_empty(),
                    trtc_cfg.ai_max_idle_secs,
                );

                // Short local wait (max ~3.5s) so inject is heard; arm in parallel.
                let arm_bridge = bridge.clone();
                let arm_cfg = trtc_cfg.clone();
                let arm_serial = serial.clone();
                let arm_task = tokio::spawn(async move {
                    tokio::task::block_in_place(|| arm_bridge.enter_ai_talk(&arm_cfg, &arm_serial))
                });

                if !wait_text.trim().is_empty() || state.ivr_dtmf1_wait_pcm.is_some() {
                    let (source, pcm) = if let Some(p) = state.ivr_dtmf1_wait_pcm.clone() {
                        ("ivr_dtmf1_wait_wav", p.as_ref().clone())
                    } else {
                        warn!("IVR DTMF 1 wait WAV missing serial={serial}; silence (no Piper)");
                        ("ivr_dtmf1_wait_missing", Vec::new())
                    };
                    let wait_ms = if pcm.is_empty() {
                        2000
                    } else {
                        ((pcm.len() as u64) * 1000)
                            / ((state.settings.sample_rate as u64).saturating_mul(2).max(1))
                    }
                    .clamp(800, 12_000);
                    info!(
                        "IVR DTMF 1 wait local-only serial={serial} source={source} wait_ms={wait_ms} text={wait_text:?}"
                    );
                    let _ = send_json(
                        &sink,
                        json!({
                            "type": "reply_text",
                            "text": wait_text,
                            "source": source,
                        }),
                    )
                    .await;
                    let _ = send_json(
                        &sink,
                        json!({
                            "type": "tts_start",
                            "sample_rate": state.settings.sample_rate,
                            "source": "gemini_wait",
                            "pcm_bytes": pcm.len(),
                            "local_wait": true,
                        }),
                    )
                    .await;
                    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
                    while tokio::time::Instant::now() < deadline {
                        if cancel_for_parent.load(Ordering::Relaxed) {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(40)).await;
                    }
                    let _ = send_json(
                        &sink,
                        json!({
                            "type": "tts_end",
                            "source": "gemini_wait",
                            "local_wait": true,
                        }),
                    )
                    .await;
                }
                if cancel_for_parent.load(Ordering::Relaxed) {
                    return;
                }
                // After wait → capture AI dialog uplink until hangup (input for the summary).
                if let Some(c) = active_call.lock().await.as_mut() {
                    c.set_ai_segment_active(true);
                }
                info!("TrtcAi AI segment capture ON serial={serial} (after wait → hangup)");

                let enter = match arm_task.await {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            "IVR DTMF 1 arm task join failed serial={serial} err={e:#}; retry enter_ai_talk"
                        );
                        bridge.enter_ai_talk(&trtc_cfg, &serial)
                    }
                };
                let enter_ok = enter.is_ok();
                let join_opt = match enter {
                    Ok(join) => {
                        info!(
                            "IVR DTMF 1 → TrtcAi (UserSig ok) serial={serial} sdk_app_id={sdk_app_id} room_id={} user_id={}",
                            join.room_id, join.user_id
                        );
                        Some(join)
                    }
                    Err(e) => {
                        warn!(
                            "IVR DTMF 1 → TrtcAi enter_ai_talk failed serial={serial} sdk_app_id={sdk_app_id} err={e:#}"
                        );
                        None
                    }
                };

                let room_id = join_opt
                    .as_ref()
                    .map(|j| j.room_id.clone())
                    .unwrap_or_else(|| trtc_bridge::SoftTrtcBridge::room_id_for(&serial));

                // Start downlink ASAP so TRTC TTS can reach the phone.
                if let Some(ref join) = join_opt {
                    spawn_trtc_downlink_pull_loop(
                        sink.clone(),
                        state.clone(),
                        bridge.clone(),
                        join.clone(),
                        active_call.clone(),
                        cancel_for_parent.clone(),
                        Some(turn.clone()),
                        talk_metrics.clone(),
                        serial.clone(),
                    );
                }

                // Wait for TaskId (StartAI is async).
                let task_deadline = tokio::time::Instant::now() + Duration::from_secs(12);
                while tokio::time::Instant::now() < task_deadline {
                    if cancel_for_parent.load(Ordering::Relaxed) {
                        return;
                    }
                    if trtc_bridge::peek_ai_task_id(&serial).is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let live_final = trtc_bridge::room_media_live(&room_id);
                let task_final = trtc_bridge::peek_ai_task_id(&serial).is_some();
                info!(
                    "IVR DTMF 1 talk gate serial={serial} media_live={live_final} task={task_final} enter_ok={enter_ok}"
                );

                // CRITICAL: force TRTC clone-TTS greeting. Without this, AI sits silent
                // waiting for STT while uplink may still be empty → "no realtime AI".
                if task_final {
                    let greet = state.settings.ivr_ai_hello_text.clone();
                    if !greet.trim().is_empty() {
                        let pushed = trtc_bridge::push_ai_line_await(
                            &trtc_cfg,
                            state.http.clone(),
                            &serial,
                            &greet,
                        )
                        .await;
                        info!(
                            "TrtcAi ServerPush greeting serial={serial} ok={pushed} text={greet:?}"
                        );
                        if !pushed {
                            // Local fallback so caller still hears a prompt.
                            if let Some(p) = state.ivr_ai_hello_pcm.as_ref() {
                                let _ = send_tts_pcm(
                                    &sink,
                                    &state,
                                    p.as_ref(),
                                    &active_call,
                                    "trtc_ai_hello_local",
                                    Some(&cancel_for_parent),
                                )
                                .await;
                            }
                        } else {
                            // Let ServerPush TTS settle into downlink → phone.
                            let settle = estimate_spoken_ms(&greet);
                            tokio::time::sleep(Duration::from_millis(settle.min(4000))).await;
                        }
                    }
                } else {
                    warn!("TrtcAi no TaskId serial={serial} — realtime AI unavailable this call");
                    if let Some(p) = state.ivr_ai_hello_pcm.as_ref() {
                        let _ = send_tts_pcm(
                            &sink,
                            &state,
                            p.as_ref(),
                            &active_call,
                            "trtc_ai_hello_no_task",
                            Some(&cancel_for_parent),
                        )
                        .await;
                    }
                }

                if cancel_for_parent.load(Ordering::Relaxed) {
                    return;
                }
                // Silence hangup safety only (greeting already done via ServerPush/local).
                spawn_trtc_ai_silence_supervisor(
                    sink,
                    state,
                    serial,
                    phase,
                    busy,
                    cancel_for_parent,
                    active_call,
                    rt,
                    heard,
                );
            });
        }
        trtc::TrtcAiArm::Degrade {
            reason,
            detail,
            sdk_app_id,
            ai_enabled,
        } => {
            if reason == "trtc_usersig_fail" {
                warn!(
                    "IVR DTMF 1 → TRTC UserSig failed serial={serial} sdk_app_id={sdk_app_id} err={} — brief goodbye + hangup (not hold music)",
                    detail.as_deref().unwrap_or("?")
                );
            } else {
                warn!(
                    "IVR DTMF 1 → TRTC not ready serial={serial} sdk_app_id={sdk_app_id} ai_enabled={ai_enabled} — brief goodbye + hangup (not hold music)"
                );
            }
            start_trtc_degrade_hangup(
                sink,
                state,
                serial,
                phase,
                busy,
                hold_cancel,
                active_call,
                reason,
                rt,
            );
        }
    }
}

/// TRTC DTMF-1 failure: fail/unavailable WAV + hangup — not the "have a nice day" goodbye
/// (that misled callers into thinking the menu ended normally after press-1).
fn start_trtc_degrade_hangup(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    reason: &'static str,
    rt: &Arc<RuntimeSettings>,
) {
    start_ivr_stub_hangup(
        sink,
        state,
        serial,
        phase,
        busy,
        hold_cancel,
        active_call,
        "AI通話暫時無法接通，請稍後再撥或按2留言。".into(),
        reason,
        true,  // prefer fail/unavailable WAV
        false, // not the "have a nice day" goodbye
        rt,
    );
}

fn start_ivr_stub_hangup(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    hold_cancel: &mut Option<Arc<AtomicBool>>,
    active_call: &ActiveCallSlot,
    spoken: String,
    reason: &'static str,
    prefer_fail_wav: bool,
    prefer_goodbye_wav: bool,
    rt: &Arc<RuntimeSettings>,
) {
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        // Allow hangup from TrtcAi (silence supervisor / DTMF end). Reject only
        // terminal or hold-music phases to avoid double hangup.
        if matches!(
            *p,
            IvrPhase::Goodbye | IvrPhase::Done | IvrPhase::PlayHoldMusic
        ) {
            return;
        }
        *p = IvrPhase::Goodbye;
    }
    stop_hold_loop(hold_cancel);
    // Stop TRTC AI / Chromium before goodbye audio — remote hangup and local
    // goodbye must not leave StartAIConversation billing until max idle.
    let trtc_cfg = state.settings.trtc();
    if trtc_cfg.configured() || trtc_bridge::peek_ai_task_id(serial).is_some() {
        trtc_bridge::make_bridge(&trtc_cfg, state.http.clone()).leave(serial, reason);
    }
    if busy.try_lock().is_err() {
        warn!("IVR hangup stub skipped busy serial={serial} reason={reason}");
        return;
    }
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let active_call = active_call.clone();
    let busy = busy.clone();
    let rt = rt.clone();
    tokio::spawn(async move {
        let _guard = busy.lock().await;
        // Resource-consumption line right before the goodbye clip (default OFF).
        play_shaming_if_enabled(&sink, &state, &rt, &active_call).await;
        // Clone WAV only — never Piper female synth on hangup path.
        let (source, tts_pcm) = if prefer_fail_wav {
            if let Some(pcm) = state.ivr_fail_pcm.clone() {
                ("ivr_fail_wav", pcm.as_ref().clone())
            } else {
                ("ivr_fail_missing", Vec::new())
            }
        } else if prefer_goodbye_wav {
            if let Some(pcm) = state.ivr_goodbye_pcm.clone() {
                ("ivr_goodbye_wav", pcm.as_ref().clone())
            } else {
                ("ivr_goodbye_empty", Vec::new())
            }
        } else {
            (reason, Vec::new())
        };
        info!(
            "IVR hangup path serial={serial} reason={reason} source={source} pcm_bytes={} text={spoken:?}",
            tts_pcm.len()
        );
        let _ = send_json(
            &sink,
            json!({"type":"reply_text","text": spoken, "source": source}),
        )
        .await;
        if !tts_pcm.is_empty() {
            let _ = send_tts_pcm(&sink, &state, &tts_pcm, &active_call, source, None).await;
        }
        let _ = send_json(
            &sink,
            json!({"type":"hangup","reason": reason, "serial": serial}),
        )
        .await;
        *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::Done;
        info!("IVR hangup sent serial={serial} reason={reason}");
    });
}

/// After AI fail or optional DTMF 2: play voicemail intro (WAV) + beep, then
/// `VoicemailRecord` — the session loop captures the caller's message.
fn start_voicemail_intro(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
) {
    start_voicemail_intro_guarded(sink, state, serial, phase, busy, active_call, None);
}

/// Gemini unavailable / failed after answer. Assistant mode → voicemail intro WAV.
/// Persona mode → **no pre-recorded audio at all**: just hang up.
fn start_voicemail_intro_for_gemini(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
    media_call_guard: &MediaCallGuard,
) {
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let busy = busy.clone();
    let active_call = active_call.clone();
    let media_call_guard = media_call_guard.clone();
    let persona = state.settings_store.snapshot().persona_flow_active();
    tokio::spawn(async move {
        // No lease is held from here on: the hangup's guarded sends take it per
        // message, and one held across them deadlocks with the media loop's
        // `invalidate()` on the phone's call_end (tokio's RwLock queues a later
        // reader behind a waiting writer, the writer waits for the held lease).
        if !media_call_guard.is_current() {
            return;
        }
        if persona {
            persona_ai_unavailable_hangup(&sink, &serial, &phase, &active_call, &media_call_guard)
                .await;
            return;
        }
        start_voicemail_intro_guarded(
            &sink,
            &state,
            &serial,
            &phase,
            &busy,
            &active_call,
            Some(media_call_guard.clone()),
        );
    });
}

/// Persona-mode AI failure: nothing pre-recorded goes to the caller — mark the call
/// done and hang up (the owner sees `persona_ai_fail_hangup` in the record).
async fn persona_ai_unavailable_hangup(
    sink: &WsSink,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    active_call: &ActiveCallSlot,
    media_call_guard: &MediaCallGuard,
) {
    ai_fail_then_hang_up(serial, phase, active_call, media_call_guard, move || {
        send_hangup_with_retries(
            sink,
            serial,
            "persona_ai_unavailable",
            3,
            Some(media_call_guard),
        )
    })
    .await;
}

/// [`persona_ai_unavailable_hangup`] with the hangup passed in. The phase and the
/// record change under the media call's lease; the lease ends before `hang_up`,
/// whose guarded sends each take it again — the phone answers the first hangup
/// with call_end, and the media loop's `invalidate()` must get through between
/// them.
async fn ai_fail_then_hang_up<F, Fut>(
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    active_call: &ActiveCallSlot,
    media_call_guard: &MediaCallGuard,
    hang_up: F,
) where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    {
        let Some(_lease) = media_call_guard.lease_current().await else {
            return;
        };
        {
            let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(*p, IvrPhase::Done | IvrPhase::Goodbye) {
                info!("persona AI-fail hangup skipped serial={serial} phase={p:?}");
                return;
            }
            *p = IvrPhase::Done;
        }
        warn!(
            "真人模式 AI unavailable serial={serial} → hangup, no voicemail / no pre-recorded audio"
        );
        if let Some(c) = active_call.lock().await.as_mut() {
            c.push_event("persona_ai_fail_hangup");
        }
    }
    hang_up().await;
}

fn start_voicemail_intro_guarded(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
    media_call_guard: Option<MediaCallGuard>,
) {
    if media_call_guard
        .as_ref()
        .map(|guard| !guard.is_current())
        .unwrap_or(false)
    {
        return;
    }
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        // WaitDtmf/PlayPrompt: normal path. LiveAi: AI connect failed → VM fallback.
        if !matches!(
            *p,
            IvrPhase::WaitDtmf | IvrPhase::PlayPrompt | IvrPhase::LiveAi
        ) {
            warn!("VM intro skipped serial={serial} phase={p:?} (not wait/play/liveAi)");
            return;
        }
        *p = IvrPhase::VoicemailIntro;
    }
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let busy = busy.clone();
    let active_call = active_call.clone();
    tokio::spawn(async move {
        // Wait for the cancelled menu prompt task to release `busy` (fast after barge-in).
        let _guard = busy.lock().await;
        let playback_lease = if let Some(guard) = media_call_guard.as_ref() {
            let Some(lease) = guard.lease_current().await else {
                info!("VM intro invalidated before playback serial={serial}");
                return;
            };
            Some(lease)
        } else {
            None
        };
        {
            let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if p != IvrPhase::VoicemailIntro {
                return;
            }
        }
        {
            let mut slot = active_call.lock().await;
            if let Some(c) = slot.as_mut() {
                c.push_event("voicemail_intro");
            }
        }
        drop(playback_lease);
        // Clean splice after menu barge-in.
        if media_call_guard.is_none() {
            let _ = stream_silence_gap(&sink, &state, 240, &active_call).await;
        }
        let (source, intro) = if let Some(pcm) = state.ivr_vm_intro_pcm.clone() {
            ("ivr_vm_intro_wav", pcm.as_ref().clone())
        } else {
            let text = state.settings.ivr_vm_intro_text.clone();
            let pcm = synthesize_piper(&state.http, &state.settings, &text).await;
            ("piper_vm_intro", pcm)
        };
        if media_call_guard
            .as_ref()
            .map(|guard| !guard.is_current())
            .unwrap_or(false)
        {
            return;
        }
        info!(
            "VM intro serial={serial} source={source} pcm_bytes={}",
            intro.len()
        );
        if media_call_guard.is_some() {
            let _ = send_gemini_media_json(
                &sink,
                json!({"type":"reply_text","text":"vm_intro","source": source}),
                "voicemail intro reply_text",
                media_call_guard.as_ref().expect("guarded voicemail intro"),
            )
            .await;
        } else {
            let _ = send_json(
                &sink,
                json!({"type":"reply_text","text":"vm_intro","source": source}),
            )
            .await;
        }
        if !intro.is_empty() {
            let _ = send_tts_pcm_guarded(
                &sink,
                &state,
                &intro,
                &active_call,
                source,
                None,
                media_call_guard.as_ref(),
            )
            .await;
        }
        if media_call_guard
            .as_ref()
            .map(|guard| !guard.is_current())
            .unwrap_or(false)
        {
            return;
        }
        {
            let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if p != IvrPhase::VoicemailIntro {
                return;
            }
        }
        let beep = state.vm_beep_pcm.as_ref().clone();
        let _ = send_tts_pcm_guarded(
            &sink,
            &state,
            &beep,
            &active_call,
            "vm_beep",
            None,
            media_call_guard.as_ref(),
        )
        .await;
        // Let beep leave the downlink capture path before arming record — otherwise
        // the first ~200ms of `_vm.wav` is loud tone and buries a quiet first word.
        if media_call_guard.is_none() {
            let _ = stream_silence_gap(&sink, &state, 320, &active_call).await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        let final_phase_lease = if let Some(guard) = media_call_guard.as_ref() {
            let Some(lease) = guard.lease_current().await else {
                return;
            };
            Some(lease)
        } else {
            None
        };
        {
            let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
            if *p == IvrPhase::VoicemailIntro {
                *p = IvrPhase::VoicemailRecord;
                info!("VM record armed serial={serial} (waiting for caller message)");
            }
        }
        drop(final_phase_lease);
    });
}

/// After the message is captured + saved: play goodbye once and hang up.
///
/// Product: press 2 → leave message → `#` (or silence) → goodbye → auto hangup.
/// Does **not** wait on callback-confirm DTMF (that left the line open after the goodbye).
fn finish_voicemail(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
    vm_notified: &Arc<AtomicBool>,
    rt: &Arc<RuntimeSettings>,
) {
    {
        let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
        if p != IvrPhase::VoicemailRecord {
            return;
        }
    }
    info!("VM finish → goodbye+hangup serial={serial} (no confirm wait)");
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let busy = busy.clone();
    let active_call = active_call.clone();
    let vm_notified = vm_notified.clone();
    let rt = rt.clone();
    tokio::spawn(async move {
        {
            let mut g = active_call.lock().await;
            if let Some(c) = g.as_mut() {
                c.set_reply("none", "", false);
                c.push_event("vm_finish_auto_hangup");
            }
        }
        start_vm_goodbye(
            &sink,
            &state,
            &serial,
            &phase,
            &busy,
            &active_call,
            &vm_notified,
            false,
            &rt,
        );
    });
}

/// Piper read-back of a phone/email heard in the voicemail message.
async fn play_vm_contact_readback(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    active_call: &ActiveCallSlot,
    arm_vm_contact_readback: &Arc<AtomicBool>,
    prompt_cancel: &Arc<AtomicBool>,
    contact: &SpokenContact,
) {
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        if *p != IvrPhase::VoicemailContactProbe {
            return;
        }
        *p = IvrPhase::VoicemailContactReadback;
    }
    let text = if contact.method == "email" {
        format!("您留的電子信箱是 {}，正確請按1，錯誤請按2", contact.display)
    } else {
        format!("您留的號碼是 {}，正確請按1，錯誤請按2", contact.display)
    };
    info!(
        "VM contact readback serial={serial} method={} contact={}",
        contact.method, contact.contact
    );
    let pcm = synthesize_piper(&state.http, &state.settings, &text).await;
    let _ = send_json(
        sink,
        json!({
            "type": "reply_text",
            "text": "vm_contact_readback",
            "method": contact.method,
            "contact": contact.contact,
        }),
    )
    .await;
    if !pcm.is_empty() {
        let _ = send_tts_pcm(
            sink,
            state,
            &pcm,
            active_call,
            "piper_vm_contact_readback",
            Some(prompt_cancel),
        )
        .await;
    }
    {
        let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
        if p == IvrPhase::VoicemailContactReadback {
            arm_vm_contact_readback.store(true, Ordering::Relaxed);
        }
    }
}

async fn play_vm_caller_id_confirm(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    active_call: &ActiveCallSlot,
    arm_vm_confirm: &Arc<AtomicBool>,
    prompt_cancel: &Arc<AtomicBool>,
) {
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            *p,
            IvrPhase::VoicemailContactProbe | IvrPhase::VoicemailContactReadback
        ) {
            return;
        }
        *p = IvrPhase::VoicemailConfirm;
    }
    let (source, pcm) = if let Some(p) = state.ivr_vm_confirm_pcm.clone() {
        ("ivr_vm_confirm_wav", p.as_ref().clone())
    } else {
        let text = state.settings.ivr_vm_confirm_text.clone();
        let pcm = synthesize_piper(&state.http, &state.settings, &text).await;
        ("piper_vm_confirm", pcm)
    };
    info!(
        "VM confirm prompt serial={serial} source={source} pcm_bytes={}",
        pcm.len()
    );
    let _ = send_json(
        sink,
        json!({"type":"reply_text","text":"vm_confirm","source": source}),
    )
    .await;
    if !pcm.is_empty() {
        let _ = send_tts_pcm(sink, state, &pcm, active_call, source, Some(prompt_cancel)).await;
    }
    {
        let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
        if p == IvrPhase::VoicemailConfirm {
            arm_vm_confirm.store(true, Ordering::Relaxed);
        }
    }
}

/// Play the caller-id callback-confirm prompt ("press 1 to call back this number").
fn start_vm_caller_id_confirm(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
    arm_vm_confirm: &Arc<AtomicBool>,
    prompt_cancel: &Arc<AtomicBool>,
) {
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let busy = busy.clone();
    let active_call = active_call.clone();
    let arm_vm_confirm = arm_vm_confirm.clone();
    let prompt_cancel = prompt_cancel.clone();
    tokio::spawn(async move {
        let _guard = busy.lock().await;
        play_vm_caller_id_confirm(
            &sink,
            &state,
            &serial,
            &phase,
            &active_call,
            &arm_vm_confirm,
            &prompt_cancel,
        )
        .await;
    });
}

/// Voicemail wrap-up: optional Piper callback-ack, goodbye WAV, WS `hangup`, notification.
fn start_vm_goodbye(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
    vm_notified: &Arc<AtomicBool>,
    reply_confirmed: bool,
    rt: &Arc<RuntimeSettings>,
) {
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*p, IvrPhase::Goodbye | IvrPhase::Done) {
            return;
        }
        *p = IvrPhase::Goodbye;
    }
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let busy = busy.clone();
    let active_call = active_call.clone();
    let vm_notified = vm_notified.clone();
    let rt = rt.clone();
    tokio::spawn(async move {
        let _guard = busy.lock().await;
        if reply_confirmed && !state.settings.vm_reply_ack_text.trim().is_empty() {
            let ack = synthesize_piper(
                &state.http,
                &state.settings,
                &state.settings.vm_reply_ack_text,
            )
            .await;
            if !ack.is_empty() {
                let _ = send_tts_pcm(&sink, &state, &ack, &active_call, "piper_vm_ack", None).await;
            }
        }
        // Resource-consumption line right before the goodbye clip (default OFF).
        play_shaming_if_enabled(&sink, &state, &rt, &active_call).await;
        let (source, bye) = if let Some(p) = state.ivr_goodbye_pcm.clone() {
            ("ivr_goodbye_wav", p.as_ref().clone())
        } else {
            let text = state.settings.ivr_goodbye_text.clone();
            let pcm = synthesize_piper(&state.http, &state.settings, &text).await;
            ("piper_goodbye_fallback", pcm)
        };
        if !bye.is_empty() {
            let _ = send_tts_pcm(&sink, &state, &bye, &active_call, source, None).await;
        }
        let reply_method = {
            let g = active_call.lock().await;
            g.as_ref()
                .and_then(|c| c.reply_method())
                .unwrap_or("")
                .to_string()
        };
        let reason = if reply_confirmed {
            match reply_method.as_str() {
                "email" => "vm_done_reply_email",
                _ => "vm_done_reply_phone",
            }
        } else {
            "vm_done"
        };
        let _ = send_json(
            &sink,
            json!({"type":"hangup","reason": reason, "serial": serial}),
        )
        .await;
        *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::Done;
        info!("VM goodbye+hangup serial={serial} reason={reason}");
        let g = active_call.lock().await;
        if let Some(c) = g.as_ref() {
            maybe_notify_voicemail(&state, c, &vm_notified);
        }
    });
}

/// Resource-shaming line via Piper before agent-initiated goodbyes (OFF by
/// default). Deliberately exaggerated: watts=47/s, water=3.8 L/s, CO2=12 g/s,
/// all scaled by `shaming_multiplier`. Skips silently when disabled, when the
/// template renders empty, or when Piper yields nothing.
async fn play_shaming_if_enabled(
    sink: &WsSink,
    state: &AppState,
    rt: &RuntimeSettings,
    active_call: &ActiveCallSlot,
) {
    if !rt.shaming_enabled {
        return;
    }
    let secs = {
        let g = active_call.lock().await;
        match g.as_ref() {
            Some(c) => c.elapsed_secs(),
            None => return,
        }
    };
    let m = if rt.shaming_multiplier.is_finite() && rt.shaming_multiplier > 0.0 {
        rt.shaming_multiplier
    } else {
        1.0
    };
    let watts = (secs * 47.0 * m).round().max(1.0);
    let water = (secs * 3.8 * m).round().max(1.0);
    let co2 = (secs * 12.0 * m).round().max(1.0);
    let text = rt
        .shaming_template
        .replace("{watts}", &format!("{watts:.0}"))
        .replace("{water}", &format!("{water:.0}"))
        .replace("{co2}", &format!("{co2:.0}"));
    if text.trim().is_empty() {
        return;
    }
    let pcm = synthesize_piper(&state.http, &state.settings, &text).await;
    if pcm.is_empty() {
        return;
    }
    info!(
        "shaming announcement secs={secs:.0} multiplier={m} pcm_bytes={}",
        pcm.len()
    );
    let _ = send_json(
        sink,
        json!({"type":"reply_text","text": text, "source": "piper_shaming"}),
    )
    .await;
    let _ = send_tts_pcm(sink, state, &pcm, active_call, "piper_shaming", None).await;
    if let Some(c) = active_call.lock().await.as_mut() {
        c.push_event("shaming_played");
    }
}

/// Legacy LiveAi intro (OpenRouter + Piper). **Not armed from DTMF 1** — product
/// path is TRTC via `start_trtc_ai_after_choice` (Scaffold / Degrade). Kept for
/// residual `IvrPhase::LiveAi` handling if an old session is mid-call.
#[allow(dead_code)]
fn start_live_ai_intro(
    sink: &WsSink,
    state: &AppState,
    serial: &str,
    phase: &Arc<std::sync::Mutex<IvrPhase>>,
    busy: &Arc<Mutex<()>>,
    active_call: &ActiveCallSlot,
) {
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(*p, IvrPhase::WaitDtmf | IvrPhase::PlayPrompt) {
            return;
        }
        *p = IvrPhase::LiveAi;
    }
    let sink = sink.clone();
    let state = state.clone();
    let serial = serial.to_string();
    let phase = phase.clone();
    let busy = busy.clone();
    let active_call = active_call.clone();
    tokio::spawn(async move {
        // Wait for the cancelled menu prompt task to release `busy`.
        let _guard = busy.lock().await;
        {
            let p = *phase.lock().unwrap_or_else(|e| e.into_inner());
            if p != IvrPhase::LiveAi {
                return;
            }
        }
        if let Some(c) = active_call.lock().await.as_mut() {
            c.push_event("live_ai_start");
        }
        // Clean splice after menu barge-in.
        let _ = stream_silence_gap(&sink, &state, 240, &active_call).await;
        let text = "已為您接通 AI 助理，請說話。";
        let pcm = synthesize_piper(&state.http, &state.settings, text).await;
        info!("LiveAi intro serial={serial} pcm_bytes={}", pcm.len());
        let _ = send_json(
            &sink,
            json!({"type":"reply_text","text": text, "source": "piper_live_ai_intro"}),
        )
        .await;
        if !pcm.is_empty() {
            let _ = send_tts_pcm(
                &sink,
                &state,
                &pcm,
                &active_call,
                "piper_live_ai_intro",
                None,
            )
            .await;
        }
        info!("LiveAi listening serial={serial} (VAD → Whisper → OpenRouter)");
    });
}

/// One-shot voicemail notification (guarded by `notified`).
/// Whisper `_vm.wav` → summary → notification; also creates the task / task card.
fn maybe_notify_voicemail(state: &AppState, call: &ActiveCall, notified: &Arc<AtomicBool>) {
    if !call.has_voicemail() {
        return;
    }
    if notified.swap(true, Ordering::Relaxed) {
        return;
    }
    let contact = call.reply_contact().unwrap_or("").to_string();
    let p = locale::notify_phrases();
    let reply_line = match (call.reply_method(), call.contact_verified()) {
        (Some("phone"), true) => {
            if contact.is_empty() {
                p.vm_call_back_unknown.to_string()
            } else {
                locale::fill(p.vm_call_back_to, &contact)
            }
        }
        (Some("email"), true) => {
            if contact.is_empty() {
                p.vm_email_unknown.to_string()
            } else {
                locale::fill(p.vm_email_to, &contact)
            }
        }
        (Some("phone"), false) => p.vm_call_back_unchecked.to_string(),
        (Some(_), _) => p.vm_no_reply.to_string(),
        (None, _) => p.vm_no_choice.to_string(),
    };
    let http = state.http.clone();
    let settings = state.settings.clone();
    let settings_store = state.settings_store.clone();
    let calls_root = state.calls.root().to_path_buf();
    let caller = call.caller().to_string();
    let serial = call.serial_str().to_string();
    let secs = call.voicemail_secs();
    let vm_rel = call.voicemail_path().map(str::to_string);
    let call_id = call.id.clone();
    tokio::spawn(async move {
        let rt = settings_store.snapshot();
        let eff = rt.effective(&settings);
        let backend = notify_primary_backend(&eff).unwrap_or("none");
        if !notify_configured(&eff) {
            warn!("voicemail TG skipped (notify not configured) serial={serial} call_id={call_id}");
            return;
        }
        let lang = locale::notify_lang();
        let p = locale::phrases(lang);
        let sep = p.sep;
        let mut summary_block = String::new();
        let mut whisper_chars = 0usize;
        if let Some(rel) = vm_rel.as_deref() {
            let abs = calls_root.join(rel);
            info!(
                "voicemail TG analyze start serial={serial} call_id={call_id} path={} backend={backend}",
                abs.display()
            );
            match extract::transcribe_voicemail_wav(&http, &eff, &abs).await {
                Ok(tx) if !tx.trim().is_empty() => {
                    whisper_chars = tx.chars().count();
                    info!(
                        "voicemail whisper ok serial={serial} call_id={call_id} chars={whisper_chars}"
                    );
                    if summary::summary_endpoint(&eff).is_some() {
                        summary_block = summary::summarize(&http, &eff, &caller, &tx).await;
                        info!(
                            "voicemail summary ok serial={serial} call_id={call_id} chars={}",
                            summary_block.chars().count()
                        );
                    } else {
                        warn!(
                            "voicemail summary skipped (no summary service) serial={serial} call_id={call_id}"
                        );
                        summary_block = format!(
                            "{}{sep}{}\n{}{sep}{}",
                            p.s_matter,
                            tx.chars().take(400).collect::<String>(),
                            p.s_todo,
                            p.vm_decide_callback
                        );
                    }
                }
                Ok(_) => warn!(
                    "voicemail whisper empty serial={serial} call_id={call_id} path={}",
                    abs.display()
                ),
                Err(e) => warn!(
                    "voicemail whisper for TG failed serial={serial} call_id={call_id}: {e:#}"
                ),
            }
        } else {
            warn!("voicemail TG no vm path serial={serial} call_id={call_id}");
        }
        // Prefer the summary card; fall back to classic voicemail notify.
        let ok = if !summary_block.trim().is_empty() {
            let mut body = summary_block;
            body.push_str(&format!("\n{reply_line}"));
            body.push_str(&format!(
                "\n{}",
                listen_recording_line(
                    &eff,
                    &call_id,
                    &locale::fill(p.vm_length, &locale::duration(lang, secs))
                )
            ));
            notify_ai_request(&http, &eff, &caller, &serial, &body).await
        } else {
            let mut reply = reply_line;
            reply.push_str(&format!(
                "\n{}{sep}{}",
                p.s_matter,
                locale::fill(p.vm_no_transcript, &locale::duration(lang, secs))
            ));
            reply.push_str(&format!("\n{}", listen_recording_line(&eff, &call_id, "")));
            notify_voicemail(&http, &eff, &caller, &serial, secs, &reply).await
        };
        info!(
            "voicemail TG notify serial={serial} call_id={call_id} backend={backend} ok={ok} whisper_chars={whisper_chars}"
        );
    });
    spawn_voicemail_task(state, call);
}

/// Create a pending task from a saved voicemail; Whisper transcribe always;
/// the intent from OpenRouter (its own slot) or the summary service when one is
/// set, else a heuristic;
/// Telegram task card when bot+chat set. On-page STT snippets are not shown in calls UI.
fn spawn_voicemail_task(state: &AppState, call: &ActiveCall) {
    let Some(rel) = call.voicemail_path().map(str::to_string) else {
        return;
    };
    let abs = state.calls.root().join(&rel);
    let http = state.http.clone();
    let settings = state.settings.clone();
    let settings_store = state.settings_store.clone();
    let task_store = state.task_store.clone();
    let call_id = call.id.clone();
    let serial = call.serial_str().to_string();
    let caller_id = call.caller().to_string();
    let reply_method = match call.reply_method() {
        Some("phone") => ReplyMethod::Phone,
        Some("email") => ReplyMethod::Email,
        Some("none") | None => ReplyMethod::None,
        Some(_) => ReplyMethod::Other,
    };
    let reply_contact = call.reply_contact().map(str::to_string);
    let contact_verified = call.contact_verified();
    let control = state.control.clone();
    let sim_sub_id = call.sim_sub_id();

    tokio::spawn(async move {
        let draft = draft_from_voicemail(
            &call_id,
            &serial,
            &caller_id,
            &rel,
            reply_method,
            reply_contact,
            contact_verified,
        );
        let task = match task_store.create(draft).await {
            Ok(t) => t,
            Err(e) => {
                warn!("task create from voicemail failed call_id={call_id}: {e:#}");
                return;
            }
        };
        // A call-back for this task dials out on the SIM the caller called.
        if let Some(sub) = sim_sub_id {
            control.remember_task_sim(&task.id, &serial, sub).await;
        }
        let task_id = task.id.clone();
        let rt = settings_store.snapshot();
        let eff = rt.effective(&settings);

        // The OpenRouter slot when it has a key, else the summary service
        // ([extract::extract_intent]).
        let (llm_key, llm_model) = (rt.openrouter_api_key.clone(), rt.openrouter_model.clone());

        let task = match process_voicemail_file(
            &http,
            &eff,
            &llm_key,
            &llm_model,
            &abs,
            &caller_id,
            &rt.spam_keywords,
        )
        .await
        {
            Ok((transcript, intent)) => {
                // Spam deny/blocklist gating removed — label intent only, never hangup/deny.
                let patch = TaskPatch {
                    summary: Some(intent.summary.clone()),
                    transcript: Some(transcript),
                    intent: Some(map_extracted_intent(&intent.intent)),
                    datetime_hint: Some(intent.datetime_hint.clone()),
                    reply_method: map_extracted_reply_method(intent.reply_method.as_deref()),
                    reply_contact: Some(intent.reply_contact.clone()),
                    ..Default::default()
                };
                match task_store.patch(&task_id, patch).await {
                    Ok(t) => {
                        let _ = task_store.push_event(&task_id, "extracted").await;
                        t
                    }
                    Err(e) => {
                        warn!("task extract patch failed id={task_id}: {e:#}");
                        task
                    }
                }
            }
            Err(e) => {
                warn!("voicemail extract failed id={task_id}: {e:#}");
                match task_store
                    .patch(
                        &task_id,
                        TaskPatch {
                            summary: Some(locale::notify_phrases().vm_task_failed.to_string()),
                            fulfill_error: Some(Some(format!("extract: {e:#}"))),
                            ..Default::default()
                        },
                    )
                    .await
                {
                    Ok(t) => t,
                    Err(_) => task,
                }
            }
        };

        // Fresh snapshot: honor hot-swapped call_notify / bot creds.
        // Task cards: Bot API only (Apprise cannot carry inline buttons).
        // Same CALL_NOTIFY master switch as call/VM alerts; no spam-blocklist gate.
        let rt = settings_store.snapshot();
        let token = rt.telegram_bot_token.trim();
        let chat = rt.telegram_chat_id.trim();
        if tg_inbound::task_card_ready(rt.call_notify, token, chat) {
            let p = locale::notify_phrases();
            let sep = p.sep;
            let title = if task.summary.trim().is_empty() {
                format!("{} — {}", p.n_voicemail, display_caller_id(&caller_id))
            } else {
                task.summary.clone()
            };
            let mut body = format!(
                "{}{sep}{}\n{}{sep}{}\n{}{sep}{}",
                p.n_number,
                display_caller_id(&caller_id),
                p.n_phone,
                notify::device_label(&serial),
                p.tc_status,
                p.tc_pending,
            );
            if !task.transcript.trim().is_empty() {
                body.push_str(&format!(
                    "\n{}{sep}{}",
                    p.tc_transcript,
                    task.transcript.chars().take(400).collect::<String>()
                ));
            }
            if let Err(e) = send_task_card(&http, token, chat, &title, &task_id, &body).await {
                warn!("telegram task card failed id={task_id}: {e:#}");
            }
        }
    });
}

async fn whisper_call_wav(
    http: &reqwest::Client,
    eff: &Settings,
    root: &std::path::Path,
    rel: Option<&str>,
    serial: &str,
    label: &str,
) -> String {
    let Some(rel) = rel.filter(|s| !s.is_empty()) else {
        return String::new();
    };
    // No Whisper sidecar set up: the Live session's own transcript is the record.
    if eff.whisper_url.trim().is_empty() {
        return String::new();
    }
    let abs = root.join(rel);
    match extract::transcribe_voicemail_wav(http, eff, &abs).await {
        Ok(t) if !t.trim().is_empty() => {
            let t = locale::fix_names(&t);
            info!(
                "ai talk whisper {label} ok serial={serial} chars={}",
                t.chars().count()
            );
            t
        }
        Ok(_) => {
            info!("ai talk whisper {label} empty serial={serial}");
            String::new()
        }
        Err(e) => {
            warn!("ai talk whisper {label} failed serial={serial}: {e:#}");
            String::new()
        }
    }
}

/// Calls longer than this skip the post-call Whisper pass (Gemini dialogue only).
const WHISPER_MAX_CALL_MS: u64 = 5 * 60 * 1000;

/// How long the translation of the lines in another language may still take
/// once the notes are saved, the notification sent and the appointments
/// handled (it starts with the summary; nothing waits on it). It is added to
/// the saved notes when it arrives.
const TRANSLATE_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// Press-1 hangup: Gemini live STT (+ optional Whisper AI segment) → **summary** → notification.
/// The notification body is the summary only (not raw fragment dump). Hangup already stopped Live.
fn spawn_ai_talk_tg_summary(
    state: &AppState,
    record: &CallRecord,
    live_transcript: Arc<std::sync::Mutex<String>>,
    appointment_book: Option<Arc<appointments::CallAppointments>>,
) {
    let http = state.http.clone();
    let settings = state.settings.clone();
    let settings_store = state.settings_store.clone();
    let calls = state.calls.clone();
    let calls_root = state.calls.root().to_path_buf();
    let caller = record.caller_id.clone();
    let serial = record.serial.clone();
    let call_id = record.id.clone();
    let ai_rel = record.ai_audio_path.clone();
    let remote_rel = record.remote_audio_path.clone();
    let duration_ms = record.duration_ms;
    let end_hint = summary_end_hint(&record.events, record.scam.as_ref());
    // The AI's call-back with an appointment's answer: no appointment and no
    // calendar suggestion from it, its own title, its result for the task.
    let ai_callback = appointments::is_ai_callback(&record.events);
    let callback_task = if ai_callback { appointments::take_call_task(&call_id) } else { None };
    // An errand's call: its own result and notification ([`errands::after_call`]).
    let errand_call = errands::is_errand_record(&record.events);
    let errand_store = state.errands.clone();
    // A call-back nobody answered ends the AI's way too: no "AI failed".
    let ai_failed = record_had_ai_fail_hangup(&record.events) && !ai_callback;
    // Stalled and not let go again by the later check.
    let stalled = record.events.iter().any(|e| e == "persona_stall_spam")
        && !record.events.iter().any(|e| e == "persona_stall_released");
    // Judged sales / scam during the call (stalling on or off), not let go again.
    let scam = record.scam.clone().filter(|j| j.stands());
    let judged = scam.is_some();
    // Remembered for the number's next calls (hidden numbers are not): the owner's
    // own marks stand over this.
    if let Some(j) = &scam {
        let sim = record.sim_country.clone().or_else(|| phone_region::last_sim(&canonical_device_serial(&serial)));
        if let Some(key) = scam_db::number_key(&caller, sim.as_deref()) {
            match scam_db::numbers().record_auto(&key, &j.kind, &j.typology) {
                Some(e) => info!("scam number remembered call_id={call_id} kind={} typology={} times={}", j.kind, j.typology, e.count),
                None => info!("scam number already marked by the owner call_id={call_id}"),
            }
        }
    }
    // Nothing from the AI reached the caller: Gemini's own transcript is text it
    // generated but never played (e.g. 「喂？你好。」 with 0 audio) — drop it.
    let ai_silent = record.tts_bytes == 0;
    // What the call's record says it agreed to (read again from the call's book below).
    let recorded_appointment = record.appointment.clone();
    let task_store = state.task_store.clone();
    let control = state.control.clone();
    let call_record = record.clone();

    tokio::spawn(async move {
        let rt = settings_store.snapshot();
        let eff = rt.effective(&settings);
        // The appointment the caller agreed to. A proposal may still be writing
        // to the calendar: wait for it. Never one from a sales / scam call or a
        // call the AI did not answer — what such a call put in the calendar
        // (and what was voided during the call) is deleted.
        let (agreed, mut voided) = match &appointment_book {
            Some(book) => {
                book.settled(appointments::TOOL_WAIT).await;
                // A time sent back for the caller's yes that they hung up on is kept, marked unconfirmed.
                book.seal_unconfirmed();
                (book.outcome(), book.voided())
            }
            None => (recorded_appointment, Vec::new()),
        };
        let mut appointment = match agreed {
            Some(a) if stalled || judged || ai_failed => {
                voided.extend(appointments::all_events(&a));
                None
            }
            other => other,
        };
        let mut void_failed = appointments::delete_voided(&voided);
        if !voided.is_empty() {
            info!(
                "voided appointment events serial={serial} call_id={call_id} count={} not_deleted={}",
                voided.len(),
                void_failed.len()
            );
        }
        // The web record (summary + full dialogue) is kept even with notifications off.
        let notify_on = notify_configured(&eff);
        let backend = notify_primary_backend(&eff).unwrap_or("none");

        // Caller-side Whisper first (Live STT can be empty / assistant-only, and the summary then misses the matter).
        let live = if ai_silent {
            String::new()
        } else {
            // One line per speaker run, STT placeholders (<no speech detected>) dropped.
            // Names the transcription keeps getting wrong are put right once
            // the fragments are joined (a name can span two fragments).
            locale::fix_names(&gemini_live::merge_dialogue_fragments(
                live_transcript
                    .lock()
                    .map(|g| g.clone())
                    .unwrap_or_default()
                    .trim(),
            ))
        };
        // Lines said in another language than the owner's, translated while the
        // summary is written (same service); the web record shows them under the line.
        let mut translation = {
            let http = http.clone();
            let eff = eff.clone();
            let dialogue = live.clone();
            let owner = locale::notify_lang();
            tokio::spawn(async move { summary::translate_dialogue(&http, &eff, &dialogue, owner).await })
        };
        // A call-back: did the callee say anything (else nobody answered)?
        let callee_answered = ai_callback
            && appointments::callee_spoke(&gemini_live::merge_dialogue_fragments(
                live_transcript.lock().map(|g| g.clone()).unwrap_or_default().trim(),
            ));
        // A stalled sales / scam call — or any long call — may run up
        // to 30 min: Whisper on that would pin the agent host's CPUs during other calls and
        // time out, and Gemini's own dialogue is the primary record anyway.
        let long_call = duration_ms > WHISPER_MAX_CALL_MS;
        let (whisper, remote_tx) = if stalled || long_call {
            info!(
                "ai talk whisper skipped (stall={stalled} long={long_call}) serial={serial} secs={}",
                duration_ms / 1000
            );
            (String::new(), String::new())
        } else {
            let whisper = whisper_call_wav(
                &http,
                &eff,
                &calls_root,
                ai_rel.as_deref(),
                &serial,
                "_ai.wav",
            )
            .await;
            let remote_tx = whisper_call_wav(
                &http,
                &eff,
                &calls_root,
                remote_rel.as_deref(),
                &serial,
                "_remote.wav",
            )
            .await;
            (whisper, remote_tx)
        };
        let live_full = live.clone();
        // The language the call was held in: the AI's call-back with the owner's
        // answer speaks it. One the AI has no setting for (Thai, Russian, Arabic,
        // Hindi…): marked so, and the call-back speaks the callee's language.
        if let Some(a) = appointment.as_mut() {
            if let Some(spoken) = gemini_live::dialogue_lang(&live_full, locale::current()) {
                a.set_call_lang(spoken);
            } else if gemini_live::dialogue_in_other_language(&live_full, locale::current()) {
                a.set_other_call_lang();
            }
        }
        let live = clip_dialogue_for_summary(&live);
        // A number (or e-mail) the caller said to reach them on, from their own
        // words only (the AI may repeat or make one up): kept on the record and
        // on the call's task. Not from a sales / scam call or one the AI missed.
        let spoken_contact = if stalled || ai_failed || ai_callback {
            None
        } else {
            let region = call_record
                .sim_country
                .clone()
                .or_else(|| phone_region::country_of_code(&rt.effective_country_code()).map(str::to_string))
                .unwrap_or_else(|| rt.default_region().to_string());
            extract::extract_spoken_contact_in(
                &caller_side_text(&live_full, &remote_tx),
                &caller,
                Some(&region),
            )
        };
        if let Some(c) = &spoken_contact {
            let kept = calls.set_reply_contact(&call_id, &c.method, &c.contact).await;
            info!(
                "spoken reply contact serial={serial} call_id={call_id} method={} kept_on_record={kept}",
                c.method
            );
        }

        // Gemini's own labelled dialogue is the primary source; Whisper backs it up
        // (noisy lines → garbage). Sections are explained to the summary model
        // in `TG_SUMMARY_SYSTEM_PROMPT`.
        let have_any = !live.is_empty() || !whisper.is_empty() || !remote_tx.is_empty();
        let transcript = if have_any {
            summary::build_summary_input(
                &caller,
                duration_ms,
                &end_hint,
                &live,
                &whisper,
                &remote_tx,
            )
        } else {
            String::new()
        };
        let mut source = match (!live.is_empty(), !whisper.is_empty() || !remote_tx.is_empty()) {
            (true, true) => "summary_input=gemini+whisper",
            (true, false) => "gemini_live_stt",
            (false, true) => "whisper_only",
            (false, false) => "none",
        };

        let caller_disp = if caller.trim().is_empty() {
            "unknown"
        } else {
            caller.trim()
        };
        let secs = duration_ms as f64 / 1000.0;
        let mut note_source = "model";

        let mut body = if !transcript.trim().is_empty() {
            if summary::summary_endpoint(&eff).is_some() {
                info!(
                    "ai talk summarize serial={serial} tx_chars={}",
                    transcript.chars().count()
                );
                source = "llm_summary";
                summary::summarize(&http, &eff, &caller, &transcript).await
            } else {
                warn!("ai talk summary skipped serial={serial}: no summary service (set a Gemini key or another OpenAI-compatible service)");
                let _ = caller_disp;
                note_source = "no_key";
                let p = locale::notify_phrases();
                let sep = p.sep;
                format!(
                    "{}{sep}{}\n{}{sep}{}",
                    p.s_matter, p.s_no_service, p.s_todo, p.s_see_transcript
                )
            }
        } else if ai_failed {
            note_source = "ai_fail";
            ai_fail_summary_body(secs)
        } else {
            note_source = "none";
            let lang = locale::notify_lang();
            let p = locale::phrases(lang);
            let sep = p.sep;
            format!(
                "{}{sep}{}\n{}{sep}{}",
                p.s_matter,
                locale::fill(p.s_no_transcript, &locale::duration(lang, secs)),
                p.s_todo,
                p.s_listen_recording
            )
        };
        if note_source == "model" && summary::is_fallback_summary(&body) {
            note_source = "fallback";
            if stalled {
                // We know what it was without the summary model: say so instead of a quote.
                body = stall_fallback_summary(&live_full, secs, scam.as_ref());
            }
        }
        let ai_failed_line = locale::notify_phrases().s_ai_failed;
        if ai_failed && !body.contains("AI 沒接上") && !body.contains(ai_failed_line) {
            // The summary model usually writes it (prompt rule); make sure the owner sees it.
            body.insert_str(0, &format!("{}\n", locale::notify_phrases().s_ai_fail_prefix));
        }
        if ai_callback && !errand_call {
            let code = if callee_answered { "answered" } else { "no_answer" };
            body.insert_str(0, &format!("{}\n", appointments::callback_result_line(locale::notify_lang(), code, &caller)));
        }

        // An appointment from a sales / scam call the check during the call
        // missed: the summary marks the call so, in the words its prompt
        // prescribes (or, when no summary model answered, the caller's words
        // score as one — the model's judgement beats keywords: a bank booking
        // a visit about the owner's own loan is no pitch). A caller following
        // up on the owner's own matter never is. Like a call judged during the
        // call: no card, what it put in the calendar is deleted, and the
        // summary says so. Calls without an appointment are left as they were.
        let late_sales = !stalled
            && !judged
            && appointment.is_some()
            && !gemini_live::dialogue_own_matter(&live_full)
            && (summary::summary_says_sales(locale::notify_lang(), &body)
                || (note_source != "model"
                    && !appointment_book.as_ref().is_some_and(|b| b.was_reviewed_normal())
                    && gemini_live::dialogue_is_sales(&live_full)));
        let mut sales_voided: Vec<appointments::Earlier> = Vec::new();
        if late_sales {
            if let Some(a) = appointment.take() {
                let events = appointments::all_events(&a);
                let failed = appointments::delete_voided(&events);
                info!(
                    "appointment voided after the call (sales / scam) serial={serial} call_id={call_id} events={} not_deleted={}",
                    events.len(),
                    failed.len()
                );
                sales_voided = events
                    .into_iter()
                    .filter(|e| !e.event_id.is_empty() && !failed.contains(e))
                    .collect();
                void_failed.extend(failed);
            }
        }

        // A date the call fixed (delivery, appointment, callback time) becomes a
        // calendar suggestion; sales calls and failed calls are not asked about.
        // An appointment the AI took is that date already (no second suggestion).
        let mut calendar_event: Option<calendar::Event> = None;
        if rt.calendar_suggest
            && appointment.is_none()
            && !ai_callback
            && note_source == "model"
            && !stalled
            && !judged
            && !late_sales
            && !ai_failed
        {
            if let Some(ev) = calendar::suggest(&http, &eff, &transcript).await {
                info!("calendar suggestion serial={serial} start={} all_day={}", ev.start, ev.all_day);
                calendar_event = Some(ev);
            }
        }

        // The web console keeps the details; the notification stays short.
        let notes = calls::CallNotes {
            summary: body.clone(),
            // Speaker labels in the owner's language ("Caller: …").
            dialogue: gemini_live::localize_dialogue(&live_full, locale::notify_lang()),
            // Added below when the translation arrives (the notes do not wait for it).
            dialogue_translations: Vec::new(),
            recording_text: remote_tx.clone(),
            end_hint: end_hint.clone(),
            source: note_source.to_string(),
            updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            calendar: calendar_event
                .clone()
                .or_else(|| appointment.as_ref().map(|a| a.calendar_event())),
            appointment: appointment.clone(),
        };
        match calls.save_notes(&call_id, &notes).await {
            Ok(()) => info!(
                "call notes saved id={call_id} source={note_source} summary_chars={} dialogue_chars={}",
                notes.summary.chars().count(),
                notes.dialogue.chars().count()
            ),
            Err(e) => warn!("call notes save failed id={call_id}: {e:#}"),
        }
        // Appointment: its task now (the owner's buttons act on it), the card or
        // the text message after the summary.
        // A call can agree on several appointments: one task (and one card or
        // text) each.
        let mut appointment_tasks = match &appointment {
            Some(a) => appointments::create_tasks(&task_store, &call_record, a).await,
            None => Vec::new(),
        };
        for task in appointment_tasks.iter_mut() {
            if let Some(sub) = call_record.sim_sub_id {
                control.remember_task_sim(&task.id, &serial, sub).await;
            }
            if let Some(c) = &spoken_contact {
                let patch = TaskPatch {
                    reply_contact: Some(Some(c.contact.clone())),
                    ..Default::default()
                };
                match task_store.patch(&task.id, patch).await {
                    Ok(t) => *task = t,
                    Err(e) => warn!("task reply contact patch failed id={}: {e:#}", task.id),
                }
            }
        }
        // The call's page in the web console, where a pending appointment is decided.
        let detail_url = notify::call_detail_url(&eff, &call_id);
        if notify_on && !errand_call {
            if let Some(ev) = &calendar_event {
                let label = locale::notify_phrases().n_calendar;
                body.push_str(&format!("\n{label}{}{} ({})", locale::notify_kv_sep(), ev.title, ev.when()));
                // One tap adds it to the calendar the owner chose (setting
                // `calendar_app`): its form, or the call's `.ics` at the console's
                // address (no link for Apple / .ics without one). Not chosen
                // yet: no calendar is picked for them — the call's page (the
                // last line) asks which one on "Add to calendar".
                let ics = calendar::ics_url(&settings_store::effective_console_url(&eff), &format!("call/{call_id}"));
                let link = if calendar::sanitize_app(&rt.calendar_app).is_empty() {
                    String::new()
                } else {
                    ev.link_for(&rt.calendar_app, "", &ics)
                };
                if !link.is_empty() {
                    body.push_str(&format!("\n{link}"));
                }
            }
            if let Some(a) = &appointment {
                body.push('\n');
                body.push_str(&appointments::summary_line(a, &detail_url));
            }
            for e in &void_failed {
                body.push('\n');
                body.push_str(&appointments::void_failed_line(e));
            }
            for e in &sales_voided {
                body.push('\n');
                body.push_str(&appointments::sales_void_line(e));
            }
            let link = call_detail_line(&eff, &call_id);
            if !link.is_empty() {
                body.push('\n');
                body.push_str(&link);
            }
            let ok = if ai_callback {
                let title = appointments::callback_headline(locale::notify_lang(), &caller);
                notify::notify_ai_callback(&http, &eff, &caller, &serial, &title, &body).await
            } else {
                notify_ai_request(&http, &eff, &caller, &serial, &body).await
            };
            info!(
                "ai talk tg summary notify serial={serial} backend={backend} ok={ok} source={source} body_chars={} tx_chars={}",
                body.chars().count(),
                transcript.chars().count()
            );
        } else if errand_call {
            info!("errand call serial={serial}: no summary notice, the errand sends its own");
        } else {
            info!("ai talk tg summary skipped (notify not configured) serial={serial} — notes on web only");
        }
        let rt = settings_store.snapshot();
        for task in appointment_tasks {
            appointments::after_summary(&http, &rt, &task_store, &task, &detail_url).await;
        }
        if let Some(task_id) = &callback_task {
            let ctx = appointments::CallbackCtx {
                http: &http,
                settings: &eff,
                tasks: task_store.as_ref(),
                control: control.as_ref(),
                errands: Some(errand_store.as_ref()),
            };
            if errand_call {
                errands::after_call(&ctx, &errand_store, task_id, &call_id, &live_full, &notes.summary, callee_answered).await;
            }
            appointments::callback_call_over(&ctx, task_id, callee_answered).await;
        }
        // Last, so a slow translation holds up nothing and its write comes after
        // every other write to these notes (the appointments' included).
        let dialogue_translations = match tokio::time::timeout(TRANSLATE_WAIT, &mut translation).await {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                warn!("dialogue translation task failed serial={serial}: {e}");
                Vec::new()
            }
            Err(_) => {
                translation.abort();
                warn!("dialogue translation timed out serial={serial}");
                Vec::new()
            }
        };
        if dialogue_translations.iter().any(|t| t.is_some()) {
            if let Some(mut saved) = calls.load_notes(&call_id).await {
                saved.dialogue_translations = dialogue_translations;
                match calls.save_notes(&call_id, &saved).await {
                    Ok(()) => info!("call notes translations added id={call_id}"),
                    Err(e) => warn!("call notes translations save failed id={call_id}: {e:#}"),
                }
            }
        }
    });
}

/// Appointment call-backs: a phone's report on a call it was asked to place
/// (`ev`, from `serial`), or None for the periodic check of ones never
/// reported on ([`appointments::on_callback_event`], [`appointments::sweep_callbacks`]).
async fn appointment_callbacks(state: &AppState, serial: &str, ev: Option<&Value>) {
    let eff = state.settings_store.snapshot().effective(&state.settings);
    let ctx = appointments::CallbackCtx {
        http: &state.http,
        settings: &eff,
        tasks: state.task_store.as_ref(),
        control: state.control.as_ref(),
        errands: Some(state.errands.as_ref()),
    };
    match ev {
        Some(msg) => appointments::on_callback_event(&ctx, serial, msg).await,
        None => appointments::sweep_callbacks(&ctx).await,
    }
}

/// Errands waiting for a phone: dialed once it is free, failed after their
/// limit ([`errands::sweep`]). Every 15 s.
async fn errand_tick(state: &AppState) {
    let rt = state.settings_store.snapshot();
    let eff = rt.effective(&state.settings);
    let phones = errands::phone_view(&state.control, &state.phones, &state.calls, &state.errands.sims).await;
    let env = errands::Env {
        store: &state.errands,
        rt: &rt,
        settings: &eff,
        http: &state.http,
        tasks: &state.task_store,
        control: &state.control,
        calls: &state.calls,
        phones,
    };
    errands::sweep(&env).await;
}

/// Persona mode gave up on the AI and hung up (`persona_ai_unavailable_hangup`).
fn record_had_ai_fail_hangup(events: &[String]) -> bool {
    events.iter().any(|e| e == "persona_ai_fail_hangup")
}

/// Whether call teardown must invoke [`spawn_ai_talk_tg_summary`] (which also completes
/// active AI callback/errand state even if the call dropped during the 400ms settle wait).
fn should_run_ai_call_summary(was_ai_talk: bool, events: &[String]) -> bool {
    was_ai_talk || record_had_ai_fail_hangup(events) || appointments::is_ai_callback(events)
}

/// What the caller said, without the AI's lines: the caller's lines of the
/// merged Live dialogue, else the speech-to-text of the caller's own track.
fn caller_side_text(dialogue: &str, caller_track: &str) -> String {
    let lines: Vec<&str> = dialogue
        .lines()
        .filter_map(|l| l.trim().strip_prefix("對方："))
        .collect();
    if lines.is_empty() {
        caller_track.trim().to_string()
    } else {
        lines.join("\n")
    }
}

/// Notification body when the AI never spoke and nothing usable was transcribed.
fn ai_fail_summary_body(secs: f64) -> String {
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let sep = p.sep;
    format!(
        "{}{sep}{}\n{}{sep}{}\n{}{sep}{}\n{}{sep}{}",
        p.s_caller,
        p.s_unclear,
        p.s_matter,
        locale::fill(p.s_ai_fail_matter, &locale::duration(lang, secs)),
        p.s_reply,
        p.s_ai_failed,
        p.s_todo,
        p.s_call_back_if_known
    )
}

/// Notification lines for a stalled sales / scam call when the summary model is unavailable
/// (e.g. repeated 503s): the kind of call it was judged ("疑似詐騙：假冒檢警要求監管帳戶"), or
/// the topic from the caller's own keywords; no transcript quote.
fn stall_fallback_summary(dialogue: &str, secs: f64, scam: Option<&calls::ScamJudgement>) -> String {
    let caller: String = dialogue
        .lines()
        .filter_map(|l| l.trim().strip_prefix("對方："))
        .collect::<Vec<_>>()
        .join("");
    let hits = gemini_live::stall_keyword_hits(&caller);
    let lang = locale::notify_lang();
    let p = locale::phrases(lang);
    let sep = p.sep;
    let named: String = hits.iter().take(4).copied().collect::<Vec<_>>().join(p.list_sep);
    let topic = if let Some(t) = scam.and_then(scam_topic) {
        t
    } else if named.is_empty() {
        p.s_stall_topic.to_string()
    } else {
        locale::fill(p.s_stall_topic_kw, &named)
    };
    format!(
        "{}{sep}{topic}\n{}{sep}{}\n{}{sep}{}",
        p.s_matter,
        p.s_reply,
        locale::fill(p.s_stall_reply, &locale::duration(lang, secs)),
        p.s_todo,
        p.s_nothing
    )
}

/// Long stall dialogues: keep the opening pitch and the ending for the summary model.
fn clip_dialogue_for_summary(dialogue: &str) -> String {
    const HEAD: usize = 4_000;
    const TAIL: usize = 1_500;
    let n = dialogue.chars().count();
    if n <= HEAD + TAIL {
        return dialogue.to_string();
    }
    let head: String = dialogue.chars().take(HEAD).collect();
    let tail: String = dialogue.chars().skip(n - TAIL).collect();
    format!(
        "{head}\n{}\n{tail}",
        locale::notify_zh_or("…（中間省略）…", "… (middle left out) …")
    )
}

/// The kind of call a sales / scam judgement names, in the notification
/// language and the summary's own words: 「疑似詐騙：假冒檢警要求監管帳戶」,
/// "Sales call: Insurance sales". None when it named no kind.
fn scam_topic(j: &calls::ScamJudgement) -> Option<String> {
    let lang = locale::notify_lang();
    let t = scam_db::typology(&j.typology)?;
    let (sales, scam) = summary::sales_labels(lang);
    let label = if t.category == scam_db::Category::Scam { scam } else { sales };
    Some(format!("{label}{}{}", locale::phrases(lang).sep, t.name(lang)))
}

/// One short line for the summary model about how the call ended, from the
/// record events and how the call was judged (in the owner's language, like the summary).
fn summary_end_hint(events: &[String], scam: Option<&calls::ScamJudgement>) -> String {
    let has = |e: &str| events.iter().any(|x| x == e);
    let topic = scam.and_then(scam_topic);
    if has("persona_ai_fail_hangup") {
        locale::notify_zh_or("AI 連不上，系統直接掛斷，對方沒聽到任何回應", "the AI could not connect; the system hung up and the caller heard nothing")
    } else if has("gemini_fail_to_voicemail") || has("gemini_zero_delivery_to_voicemail") {
        locale::notify_zh_or("AI 失敗，改走語音留言", "the AI failed; the call went to voicemail")
    } else if has("reject_unknown_caller") {
        locale::notify_zh_or("未顯示號碼，播拒接語音後掛斷（AI 沒有接）", "hidden number: the refusal announcement was played, then hung up (the AI did not answer)")
    } else if has("reject_list") {
        locale::notify_zh_or("黑名單：播無法接通語音後掛斷（AI 沒有接）", "blocked number: the unavailable announcement was played, then hung up (the AI did not answer)")
    } else if has("whitelist_urgent_call_merged") {
        locale::notify_zh_or(
            "來電者說有急事：手機打給主人，主人接起來後直接跟來電者通話，AI 退出；這通沒有被當成推銷或詐騙陪聊拖時間",
            "the caller said it was urgent: the phone called the owner, who picked up and was put through to the caller, and the AI left; this call was not treated as a sales or scam call to stall",
        )
    } else if events.iter().any(|e| e.starts_with("whitelist_urgent_call_failed") || e == "whitelist_urgent_call_ai_ended") {
        locale::notify_zh_or(
            "來電者說有急事：手機打給主人但沒接通，AI 跟對方說主人會盡快回電；這通沒有被當成推銷或詐騙陪聊拖時間，「要處理」寫盡快回電給對方",
            "the caller said it was urgent: the phone called the owner but did not get through, and the AI told the caller the owner would call back soon; this call was not treated as a sales or scam call to stall, so the to-do is to call them back as soon as possible",
        )
    } else if has("whitelist_urgent_transfer") {
        locale::notify_zh_or(
            "來電者說有急事，系統已馬上通知主人；這通沒有被當成推銷或詐騙陪聊拖時間，「要處理」寫盡快回電給對方",
            "the caller said it was urgent and the owner was alerted at once; this call was not treated as a sales or scam call to stall, so the to-do is to call them back as soon as possible",
        )
    } else if has("persona_stall_released") {
        locale::notify_zh_or("系統一度以為是推銷／詐騙，再確認一次是正常電話，AI 回到正常應對", "the system first took it for a sales / scam call; a second check found an ordinary call and the AI went back to talking normally")
    } else if has("persona_stall_spam") {
        match topic {
            Some(t) => locale::notify_zh_or(
                &format!("系統判斷是推銷／詐騙（{t}），AI 改成故意陪他聊、拖時間"),
                &format!("the system judged it a sales / scam call ({t}), so the AI deliberately kept the caller talking to waste their time"),
            ),
            None => locale::notify_zh_or("系統判斷像推銷／詐騙，AI 改成故意陪他聊、拖時間", "the system judged it a sales / scam call, so the AI deliberately kept the caller talking to waste their time"),
        }
    } else if has("persona_scam_detected") {
        let t = topic.unwrap_or_else(|| locale::notify_phrases().s_stall_topic.to_string());
        locale::notify_zh_or(
            &format!("系統判斷是推銷／詐騙（{t}）；陪聊功能關著，AI 照一般方式應對"),
            &format!("the system judged it a sales / scam call ({t}); stalling is off, so the AI answered as usual"),
        )
    } else if has("ai_segment_end") || has("gemini_unmute_after_greet") {
        locale::notify_zh_or("AI 接聽並對話後結束（AI 說掰掰掛斷，或對方先掛）", "the AI answered and talked; the call ended normally (the AI said goodbye and hung up, or the caller hung up first)")
    } else {
        String::new()
    }
}

fn map_extracted_intent(raw: &str) -> TaskIntent {
    match raw.trim().to_ascii_lowercase().as_str() {
        "message" | "question" | "spam" => TaskIntent::Message,
        "appointment" | "schedule" => TaskIntent::Appointment,
        "callback" => TaskIntent::Callback,
        "email_reply" | "email" => TaskIntent::EmailReply,
        other => TaskIntent::Other(other.to_string()),
    }
}

fn map_extracted_reply_method(raw: Option<&str>) -> Option<ReplyMethod> {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("phone") => Some(ReplyMethod::Phone),
        Some("email") => Some(ReplyMethod::Email),
        Some("none") => Some(ReplyMethod::None),
        Some(_) => Some(ReplyMethod::Other),
        None => None,
    }
}

fn display_caller_id(caller_id: &str) -> &str {
    let t = caller_id.trim();
    if t.is_empty() {
        locale::notify_phrases().n_hidden
    } else {
        t
    }
}

async fn ensure_active_call(
    state: &AppState,
    active_call: &ActiveCallSlot,
    serial: &str,
    caller_id: &str,
    sample_rate: u32,
    status: &str,
) -> EnsureCall {
    // Secondary-user twin (pixel-101xxxx) mirrors the real call: never record it
    // (it produced empty-CID duplicate rows next to every real call).
    if is_isolated_screening_serial(serial) {
        return EnsureCall::Skipped;
    }
    let old_caller = {
        let mut slot = active_call.lock().await;
        if let Some(c) = slot.as_mut() {
            let old = c.caller().to_string();
            c.set_caller_id(caller_id);
            if status == "active" {
                c.set_status("active");
                c.push_event("hello");
            }
            Some(old)
        } else {
            None
        }
    };
    if let Some(old) = old_caller {
        state.call_reattach.rekey(serial, &old, caller_id).await;
        return EnsureCall::Existing;
    }

    if let Some(mut stolen) = state.call_reattach.steal(serial, caller_id).await {
        stolen.set_caller_id(caller_id);
        if status == "active" {
            stolen.set_status("active");
            stolen.push_event("hello_reattach");
        }
        let id = stolen.id.clone();
        *active_call.lock().await = Some(stolen);
        state
            .call_reattach
            .mark_live(serial, caller_id, &id, active_call.clone())
            .await;
        info!(
            "call reattach id={id} serial={serial} caller={} status={status}",
            urgent_call::masked(caller_id)
        );
        return EnsureCall::Reattached;
    }

    if let Some(mut call) = state
        .calls
        .start(serial, caller_id, sample_rate, status)
        .await
    {
        call.set_barge_in_allowed(state.settings_store.snapshot().gemini_barge_in);
        let id = call.id.clone();
        *active_call.lock().await = Some(call);
        state
            .call_reattach
            .mark_live(serial, caller_id, &id, active_call.clone())
            .await;
        return EnsureCall::Started;
    }
    EnsureCall::Skipped
}

fn stop_hold_loop(hold_cancel: &mut Option<Arc<AtomicBool>>) {
    if let Some(flag) = hold_cancel.take() {
        flag.store(true, Ordering::Relaxed);
    }
}

/// Sidecar TRTC downlink → phone playout, recorded on the system lane.
///
/// For every non-empty `pull_downlink_pcm` chunk:
/// 1. [`ActiveCall::write_tts`] → `_tts.wav` (+ full mix)
/// 2. binary WS frame → cellular uplink inject
///
/// Soft bridge returns empty pulls (no-op). Cancel via `hold_cancel` (DTMF2 / hangup / reset).
fn spawn_trtc_downlink_pull_loop(
    sink: WsSink,
    state: AppState,
    bridge: Arc<dyn TrtcBridge>,
    join: trtc_bridge::TrtcJoinParams,
    active_call: ActiveCallSlot,
    cancel: Arc<AtomicBool>,
    turn: Option<Arc<TrtcTurnTracker>>,
    talk_metrics: Arc<TrtcTalkMetrics>,
    serial: String,
) {
    tokio::spawn(async move {
        // 10 ms poll — WS buffer is non-blocking; HTTP fallback is short-timeout.
        let max_bytes = trtc_bridge::PCM_FRAME_BYTES.saturating_mul(4); // ~80 ms
        let mut interval = tokio::time::interval(Duration::from_millis(10));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut started = false;
        let mut outbound_armed = false;
        let mut total: u64 = 0;
        // Clear tts_outbound quickly after AI audio stops so sidetone filter
        // does not linger. Caller speech is still forwarded (non-sidetone only).
        let mut idle_empty_ticks: u32 = 0;
        const CLEAR_OUTBOUND_AFTER_EMPTY_TICKS: u32 = 8; // ~80 ms @ 10 ms tick
        let trtc_settings = state.settings.trtc();
        let http = state.http.clone();
        let room_id = join.room_id.clone();
        loop {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            interval.tick().await;
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            // Prefer async HTTP pull (no block_in_place on the WS task).
            let chunk =
                match trtc_bridge::pull_downlink_http(&trtc_settings, &http, &room_id, max_bytes)
                    .await
                {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("TRTC pull_downlink_http failed room_id={room_id} err={e:#}");
                        // Fallback to trait path if HTTP helper fails open.
                        match bridge.pull_downlink_pcm(&join, max_bytes) {
                            Ok(c) => c,
                            Err(_) => continue,
                        }
                    }
                };
            if chunk.is_empty() {
                idle_empty_ticks = idle_empty_ticks.saturating_add(1);
                if outbound_armed && idle_empty_ticks >= CLEAR_OUTBOUND_AFTER_EMPTY_TICKS {
                    if let Some(c) = active_call.lock().await.as_mut() {
                        c.set_tts_outbound_active(false);
                    }
                    outbound_armed = false;
                    if started {
                        let _ =
                            send_json(&sink, json!({"type": "tts_end", "source": "trtc_downlink"}))
                                .await;
                        started = false;
                    }
                }
                continue;
            }
            idle_empty_ticks = 0;
            if !started {
                started = true;
                // Latency: caller_turn_end → first AI audio frame.
                if let Some(ref t) = turn {
                    let end = t.user_turn_end_ms.load(Ordering::Relaxed);
                    let latched = t.latched_end_ms.load(Ordering::Relaxed);
                    if end > 0 && end != latched {
                        let now = now_unix_ms();
                        let latency = now.saturating_sub(end);
                        // Ignore absurd / clock-skew values.
                        if latency > 0 && latency < 120_000 {
                            talk_metrics.record_reply(&serial, latency);
                            t.latched_end_ms.store(end, Ordering::Relaxed);
                        }
                    }
                }
                let _ = send_json(
                    &sink,
                    json!({
                        "type": "tts_start",
                        "sample_rate": state.settings.sample_rate,
                        "source": "trtc_downlink",
                    }),
                )
                .await;
                info!(
                    "TRTC downlink playout start room_id={room_id} serial={serial} source=trtc_downlink (write_tts → _tts.wav / 僅系統／TRTC)"
                );
            }
            if !outbound_armed {
                if let Some(c) = active_call.lock().await.as_mut() {
                    c.set_tts_outbound_active(true);
                }
                outbound_armed = true;
            }
            // Recording contract: same write_tts lane as local Piper/menu/hold.
            if let Some(c) = active_call.lock().await.as_mut() {
                c.write_tts(&chunk);
            }
            total = total.saturating_add(chunk.len() as u64);
            {
                let mut s = sink.lock().await;
                if s.send(Message::Binary(chunk.into())).await.is_err() {
                    break;
                }
            }
        }
        if outbound_armed {
            if let Some(c) = active_call.lock().await.as_mut() {
                c.set_tts_outbound_active(false);
            }
        }
        if started {
            let _ = send_json(&sink, json!({"type": "tts_end", "source": "trtc_downlink"})).await;
        }
        if total > 0 {
            info!(
                "TRTC downlink playout end room_id={room_id} bytes={total} (recorded via write_tts)"
            );
        }
    });
}

/// ~`gap_ms` of zero PCM at realtime pace (barge-in splice clean-up).
async fn stream_silence_gap(
    sink: &WsSink,
    state: &AppState,
    gap_ms: u32,
    active_call: &ActiveCallSlot,
) -> Result<()> {
    let gap_ms = gap_ms.clamp(40, 800);
    let frame = (state.settings.sample_rate as usize * 40 / 1000) * 2;
    let frame = frame.max(2);
    let frames = (gap_ms as usize + 39) / 40;
    let zeros = vec![0u8; frame];
    send_json(
        sink,
        json!({
            "type": "tts_start",
            "sample_rate": state.settings.sample_rate,
            "source": "barge_silence",
            "pcm_bytes": frame * frames,
        }),
    )
    .await?;
    if let Some(c) = active_call.lock().await.as_mut() {
        c.set_tts_outbound_active(true);
    }
    let mut interval = tokio::time::interval(Duration::from_millis(40));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;
    let mut send_err: Option<anyhow::Error> = None;
    for _ in 0..frames {
        interval.tick().await;
        if let Some(c) = active_call.lock().await.as_mut() {
            c.write_tts(&zeros);
        }
        let mut s = sink.lock().await;
        if let Err(e) = s.send(Message::Binary(zeros.clone().into())).await {
            send_err = Some(e.into());
            break;
        }
    }
    if let Some(c) = active_call.lock().await.as_mut() {
        c.set_tts_outbound_active(false);
    }
    if let Some(e) = send_err {
        return Err(e);
    }
    let _ = send_json(sink, json!({"type":"tts_end","source":"barge_silence"})).await;
    info!("IVR barge silence gap_ms={gap_ms} frames={frames}");
    Ok(())
}

/// One `tts_start`, play the track once at ~realtime, then `tts_end`.
/// CONSTANT-GAIN playback: the asset is pre-normalized offline and the load
/// path applies one whole-buffer RMS normalize + static soft ceiling. No
/// per-frame gain riding here — a runtime AGC stacked on the asset compressor
/// and carrier VoLTE AGC caused audible pumping (measured: the
/// leveler rode 0.71–1.40 ≈ 6 dB adding ±2.9 dB envelope swing at 35–130 ms).
/// Short join blend only smooths frame-boundary clicks near peaks.
///
/// **Not on the product AI path.** DTMF1 never calls this (no
/// `play_transfer_hold` / `start_transfer_hold`); TrtcAi uses silence scaffold
/// via `start_trtc_ai_after_choice`. Kept as the reference for
/// the constant-gain / no-runtime-AGC policy on the playback loop body.
#[allow(dead_code)]
async fn stream_hold_music_once(
    sink: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    state: &AppState,
    pcm: &[u8],
    cancel: Arc<AtomicBool>,
    active_call: &ActiveCallSlot,
) -> Result<()> {
    if pcm.is_empty() {
        anyhow::bail!("empty hold PCM");
    }
    send_json(
        sink,
        json!({"type":"tts_start","sample_rate": state.settings.sample_rate, "source": "hold_music"}),
    )
    .await?;

    if let Some(c) = active_call.lock().await.as_mut() {
        c.set_tts_outbound_active(true);
    }

    // 20 ms frames: smoother VoLTE inject pacing than 40 ms (same realtime budget).
    let frame = (state.settings.sample_rate as usize * 20 / 1000) * 2;
    let frame = frame.max(2);
    let mut interval = tokio::time::interval(Duration::from_millis(20));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;

    let mut prev_last = 0i16;
    let mut first = true;
    let mut cancelled = false;
    for chunk in pcm.chunks(frame) {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            let mut faded = chunk.to_vec();
            blend_hold_join(prev_last, &mut faded);
            fade_out_s16le(&mut faded);
            interval.tick().await;
            if let Some(c) = active_call.lock().await.as_mut() {
                c.write_tts(&faded);
            }
            let mut s = sink.lock().await;
            let _ = s.send(Message::Binary(faded.into())).await;
            break;
        }
        let mut out = chunk.to_vec();
        if first {
            fade_in_s16le(&mut out);
            first = false;
        } else {
            blend_hold_join(prev_last, &mut out);
        }
        if let Some(last) = last_s16le_sample(&out) {
            prev_last = last;
        }
        interval.tick().await;
        if let Some(c) = active_call.lock().await.as_mut() {
            c.write_tts(&out);
        }
        let mut s = sink.lock().await;
        if s.send(Message::Binary(out.into())).await.is_err() {
            cancel.store(true, Ordering::Relaxed);
            cancelled = true;
            break;
        }
    }

    if let Some(c) = active_call.lock().await.as_mut() {
        c.set_tts_outbound_active(false);
    }
    let _ = send_json(sink, json!({"type":"tts_end","source":"hold_music"})).await;
    info!(
        "hold music finished once pcm_bytes={} cancelled={cancelled}",
        pcm.len(),
    );
    Ok(())
}

/// Linear fade-out of one s16le mono frame (reduces barge-in click).
fn fade_out_s16le(pcm: &mut [u8]) {
    let n = pcm.len() / 2;
    if n == 0 {
        return;
    }
    for i in 0..n {
        let o = i * 2;
        let s = i16::from_le_bytes([pcm[o], pcm[o + 1]]) as f64;
        let g = 1.0 - (i as f64 + 1.0) / n as f64;
        let out = (s * g).round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        let b = out.to_le_bytes();
        pcm[o] = b[0];
        pcm[o + 1] = b[1];
    }
}

/// Short fade-in at the start of a PCM frame (hold-music loop / barge cleanup).
fn fade_in_s16le(pcm: &mut [u8]) {
    let n = pcm.len() / 2;
    if n == 0 {
        return;
    }
    for i in 0..n {
        let o = i * 2;
        let s = i16::from_le_bytes([pcm[o], pcm[o + 1]]) as f64;
        let g = (i as f64 + 1.0) / n as f64;
        let out = (s * g).round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        let b = out.to_le_bytes();
        pcm[o] = b[0];
        pcm[o + 1] = b[1];
    }
}

fn last_s16le_sample(pcm: &[u8]) -> Option<i16> {
    if pcm.len() < 2 {
        return None;
    }
    let o = pcm.len() - 2;
    Some(i16::from_le_bytes([pcm[o], pcm[o + 1]]))
}

/// RMS of one s16le mono frame (voicemail silence detection).
fn frame_rms(pcm: &[u8]) -> f32 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.0;
    }
    let mut acc = 0f64;
    for i in 0..n {
        let s = i16::from_le_bytes([pcm[2 * i], pcm[2 * i + 1]]) as f64;
        acc += s * s;
    }
    ((acc / n as f64).sqrt()) as f32
}

/// Sine beep with 12 ms edge fades, s16le mono at `sample_rate` (voicemail cue).
fn make_beep_pcm(sample_rate: u32, hz: f64, ms: u32, amp: f64) -> Vec<u8> {
    let n = (sample_rate as usize).saturating_mul(ms as usize) / 1000;
    let fade = (sample_rate as usize * 12 / 1000).max(1);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / sample_rate as f64;
        let mut g = 1.0;
        if i < fade {
            g = i as f64 / fade as f64;
        }
        if n - i <= fade {
            g = g.min((n - i) as f64 / fade as f64);
        }
        let s = (2.0 * std::f64::consts::PI * hz * t).sin() * amp * g * f64::from(i16::MAX);
        let v = s.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Blend ~2 ms at frame head when the boundary jump would click on loud/high notes.
fn blend_hold_join(prev_last: i16, pcm: &mut [u8]) {
    let n = pcm.len() / 2;
    if n < 4 {
        return;
    }
    let first = i16::from_le_bytes([pcm[0], pcm[1]]);
    let jump = (first as i32 - prev_last as i32).abs();
    if jump < 2500 {
        return;
    }
    let blend = n.min(32); // ≤2 ms @ 16 kHz
    for i in 0..blend {
        let o = i * 2;
        let s = i16::from_le_bytes([pcm[o], pcm[o + 1]]) as f64;
        let g = (i as f64 + 1.0) / blend as f64;
        let mixed = prev_last as f64 * (1.0 - g) + s * g;
        let out = mixed.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        let b = out.to_le_bytes();
        pcm[o] = b[0];
        pcm[o + 1] = b[1];
    }
}

/// Stream PCM at ~realtime (40 ms frames). Blasting faster overflows the phone
/// uplink queue (128 frames) → garbled audio + agent finishes early → hangup.
/// When `cancel` flips, fade the current frame and stop (DTMF barge-in).
/// Continuous Gemini Live playout: coalesce many small PCM chunks into one paced stream.
/// Avoids per-chunk `tts_start`/`tts_end` which added multi-hundred-ms gaps between phrases.
#[derive(Debug, Default, Clone, Copy)]
struct GeminiDeliveryStats {
    bytes: u64,
    frames: u64,
    turns: u32,
}

const GEMINI_MEDIA_SEND_TIMEOUT: Duration = Duration::from_secs(2);

async fn send_gemini_media_json(
    sink: &WsSink,
    payload: Value,
    label: &str,
    media_call_guard: &MediaCallGuard,
) -> Result<()> {
    let Some(_lease) = media_call_guard.lease_current().await else {
        anyhow::bail!("gemini media ws {label} skipped for stale media call");
    };
    tokio::time::timeout(GEMINI_MEDIA_SEND_TIMEOUT, send_json(sink, payload))
        .await
        .map_err(|_| anyhow::anyhow!("gemini media ws {label} send timed out"))??;
    Ok(())
}

async fn deliver_gemini_media_binary(
    sink: &WsSink,
    active_call: &ActiveCallSlot,
    pcm: Vec<u8>,
    label: &str,
    media_call_guard: &MediaCallGuard,
) -> Result<()> {
    let Some(_lease) = media_call_guard.lease_current().await else {
        anyhow::bail!("gemini media ws {label} skipped for stale media call");
    };
    tokio::time::timeout(GEMINI_MEDIA_SEND_TIMEOUT, async {
        let mut s = sink.lock().await;
        s.send(Message::Binary(pcm.clone().into())).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("gemini media ws {label} send timed out"))?
    .map_err(|e| anyhow::anyhow!("gemini media ws {label} send failed: {e}"))?;
    if let Some(call) = active_call.lock().await.as_mut() {
        call.write_tts(&pcm);
    }
    Ok(())
}

/// Background sound on an echo-free phone: in pauses the bed is kept this far ahead of the
/// phone's playout. 80 ms left no room for USB / bridge jitter (the phone ran
/// dry → room noise with holes); a reply still starts at once — the queued bed
/// is cut for it (`ambience_cut`).
const AMBIENCE_LEAD: Duration = Duration::from_millis(200);
/// A reply drops room noise still queued on the phone once the previous AI
/// audio has surely played out (the phone runs ~this far behind our estimate).
const AMBIENCE_CUT_AFTER: Duration = Duration::from_millis(150);
/// After a queue clear the phone's AudioTrack still plays about this much.
const AMBIENCE_PHONE_BUFFER: Duration = Duration::from_millis(200);
/// Head start for replies of a model that ran the phone dry mid-reply
/// (gemini-3.8-live-extended-thinking: lumps 40–120 ms ahead, some late).
const PLAYOUT_CUSHION: Duration = Duration::from_millis(120);

/// This call's background sound (setting `ambience_kind`, persona mode only), starting at a
/// random point of the loop.
fn ambience_mixer_for_call(
    state: &AppState,
    serial: &str,
    sample_rate: u32,
) -> Option<ambience::AmbienceMixer> {
    let rt = state.settings_store.snapshot();
    if !rt.persona_flow_active() || !ambience::valid_kind(&rt.ambience_kind) {
        return None;
    }
    let Some(bed) = state.ambience.get(&rt.ambience_kind) else {
        warn!(
            "Gemini ambience serial={serial} kind={} not loaded — off",
            rt.ambience_kind
        );
        return None;
    };
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as usize)
        .unwrap_or(0);
    let start = seed % bed.len().max(1);
    info!(
        "Gemini ambience serial={serial} kind={} level={} start_s={}",
        rt.ambience_kind,
        rt.ambience_level,
        start / sample_rate.max(1) as usize
    );
    Some(ambience::AmbienceMixer::new(
        bed,
        ambience::level_gain(rt.ambience_level),
        start,
        sample_rate,
    ))
}

/// Background-only frame (ambience in a pause): to the phone, and to the call's own
/// room-noise track for the recording — not into the system track / echo
/// reference, not counted as delivered AI audio.
async fn deliver_gemini_media_bed(
    sink: &WsSink,
    active_call: &ActiveCallSlot,
    pcm: Vec<u8>,
    media_call_guard: &MediaCallGuard,
) -> Result<()> {
    let Some(_lease) = media_call_guard.lease_current().await else {
        anyhow::bail!("gemini media ws ambience skipped for stale media call");
    };
    tokio::time::timeout(GEMINI_MEDIA_SEND_TIMEOUT, async {
        let mut s = sink.lock().await;
        s.send(Message::Binary(pcm.clone().into())).await
    })
    .await
    .map_err(|_| anyhow::anyhow!("gemini media ws ambience send timed out"))?
    .map_err(|e| anyhow::anyhow!("gemini media ws ambience send failed: {e}"))?;
    if let Some(call) = active_call.lock().await.as_mut() {
        call.write_bed(&pcm);
    }
    Ok(())
}

/// The phone dropped its queue: unplayed audio leaves the call's recording.
async fn drop_unplayed_recording(
    active_call: &ActiveCallSlot,
    media_call_guard: &MediaCallGuard,
    ahead: Duration,
    ai: bool,
) {
    let Some(_lease) = media_call_guard.lease_current().await else {
        return;
    };
    if let Some(call) = active_call.lock().await.as_mut() {
        call.drop_unplayed(ahead, ai);
    }
}

async fn set_gemini_tts_outbound(
    active_call: &ActiveCallSlot,
    media_call_guard: &MediaCallGuard,
    active: bool,
) {
    let Some(_lease) = media_call_guard.lease_current().await else {
        return;
    };
    let mut slot = active_call.lock().await;
    if let Some(call) = slot.as_mut() {
        call.set_tts_outbound_active(active);
    }
}

async fn stream_gemini_downlink_pcm(
    sink: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    state: &AppState,
    mut dl_rx: tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    active_call: &ActiveCallSlot,
    cancel: &AtomicBool,
    delivered_bytes: &AtomicU64,
    play_head: &gemini_live::DownlinkPlayHead,
    serial: &str,
    media_call_guard: &MediaCallGuard,
    urgent: Option<&urgent_call::CallCtl>,
) -> Result<GeminiDeliveryStats> {
    let sr = state.settings.sample_rate.max(8000);
    // Match send_tts_pcm: 40 ms frames + 1s head-burst per spoken segment for USB prebuffer.
    const FRAME_MS: u64 = 40;
    const BURST_MS: u64 = 1000;
    let frame = (sr as usize * FRAME_MS as usize / 1000) * 2;
    let frame = frame.max(2);
    let burst_cap = (sr as usize).saturating_mul(BURST_MS as usize) / 1000 * 2;
    let mut buf: Vec<u8> = Vec::with_capacity(frame * 50);
    let mut interval = tokio::time::interval(Duration::from_millis(FRAME_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;

    let mut outbound_on = false;
    let mut total_played: u64 = 0;
    let mut delivered_frames: u64 = 0;
    let mut turns: u32 = 0;
    let mut burst_left = 0usize;
    // Idle frames after buffer empty before ending a playout segment.
    let mut empty_ticks: u32 = 0;
    const END_SEG_EMPTY_TICKS: u32 = 4; // ~160 ms quiet @ 40 ms → segment end
    /// …unless the model is still streaming the reply; then only after ~1.6 s.
    const MID_REPLY_MAX_EMPTY_TICKS: u32 = 40;

    info!(
        "Gemini continuous downlink player start serial={serial} frame_ms={FRAME_MS} burst_ms={BURST_MS}"
    );
    let mut flush_seen = play_head.flush_seq();
    // Background sound under the AI's voice (None = off / not persona mode / bed missing).
    let mut amb = ambience_mixer_for_call(state, serial, sr);
    // Echo-free phone (the call's echo check): the bed also runs through pauses.
    let mut amb_continuous = false;
    let mut amb_checked_at = tokio::time::Instant::now();
    // Whitelist urgent call: while the phone calls the owner and once the owner is
    // on, no room noise goes out (the phone's outgoing audio is the owner's call then).
    let mut amb_off_for_owner = false;
    let frame_dur = Duration::from_millis(FRAME_MS);
    // Estimated moment the phone has played everything sent / the last AI frame.
    let mut phone_tail = tokio::time::Instant::now();
    let mut speech_tail = tokio::time::Instant::now();
    let mut bed_since_speech = false;
    // This reply: AI audio sent, and how long the phone sat without any because
    // the model's next lump came late.
    let mut seg_bytes: u64 = 0;
    let mut seg_late_ms: u64 = 0;
    let mut ran_empty = false;
    // A model that ran the phone dry mid-reply streams barely ahead of real time:
    // from then on each reply starts PLAYOUT_CUSHION late so its lumps make it.
    let mut cushion = false;
    let mut reply_buffered_at: Option<tokio::time::Instant> = None;
    let cushion_enough = (sr as usize * 600 / 1000) * 2;

    loop {
        if cancel.load(Ordering::Relaxed) || !media_call_guard.is_current() {
            break;
        }
        if amb.is_some()
            && !amb_continuous
            && amb_checked_at.elapsed() >= Duration::from_millis(200)
        {
            amb_checked_at = tokio::time::Instant::now();
            if active_call.lock().await.as_ref().and_then(|c| c.echo_free()) == Some(true) {
                amb_continuous = true;
                info!("Gemini ambience serial={serial} continuous (echo-free line)");
            }
        }
        let owner_call = urgent.is_some_and(|c| c.ai_quiet());
        if owner_call != amb_off_for_owner {
            amb_off_for_owner = owner_call;
            if amb.is_some() {
                if owner_call {
                    info!("Gemini ambience serial={serial} off: the phone is calling the owner");
                } else {
                    info!("Gemini ambience serial={serial} back on: the AI talks to the caller again");
                }
            }
        }

        // Caller cut in (barge-in): drop what is left of this turn here and on the
        // phone (a non-Gemini tts_start clears its inject queue).
        let flush_now = play_head.flush_seq();
        if flush_now != flush_seen {
            flush_seen = flush_now;
            let mut dropped = buf.len() as u64;
            buf.clear();
            while let Ok(chunk) = dl_rx.try_recv() {
                dropped += chunk.len() as u64;
            }
            play_head.drop_unplayed(dropped);
            play_head.set_outbound(false);
            if outbound_on {
                set_gemini_tts_outbound(active_call, media_call_guard, false).await;
                let _ = send_gemini_media_json(
                    sink,
                    json!({"type":"tts_end","source":"gemini_live_tts"}),
                    "barge-in tts_end",
                    media_call_guard,
                )
                .await;
                outbound_on = false;
            }
            for t in ["tts_start", "tts_end"] {
                let _ = send_gemini_media_json(
                    sink,
                    json!({"type": t, "source": "barge_in"}),
                    "barge-in phone queue clear",
                    media_call_guard,
                )
                .await;
            }
            burst_left = 0;
            empty_ticks = 0;
            reply_buffered_at = None;
            // The phone dropped its queue; what its AudioTrack holds still plays.
            drop_unplayed_recording(active_call, media_call_guard, AMBIENCE_PHONE_BUFFER, true)
                .await;
            phone_tail = tokio::time::Instant::now() + AMBIENCE_PHONE_BUFFER;
            speech_tail = phone_tail;
            play_head.set_speech_tail(speech_tail);
            bed_since_speech = false;
            info!(
                "Gemini barge-in flush serial={serial} dropped_bytes={dropped} (~{} ms) played={total_played}",
                dropped * 1000 / (u64::from(sr) * 2).max(1)
            );
            continue;
        }

        // Pull all currently queued chunks without blocking.
        loop {
            match dl_rx.try_recv() {
                Ok(chunk) if !chunk.is_empty() => {
                    buf.extend_from_slice(&chunk);
                    empty_ticks = 0;
                }
                Ok(_) => {}
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    // Drain remaining then exit after buffer plays out.
                    if buf.is_empty() {
                        play_head.set_outbound(false);
                        if outbound_on {
                            set_gemini_tts_outbound(active_call, media_call_guard, false).await;
                            let _ = send_gemini_media_json(
                                sink,
                                json!({"type":"tts_end","source":"gemini_live_tts"}),
                                "tts_end",
                                media_call_guard,
                            )
                            .await;
                            outbound_on = false;
                        }
                        info!(
                            "Gemini continuous downlink end serial={serial} played={total_played} turns={turns}"
                        );
                        return Ok(GeminiDeliveryStats {
                            bytes: total_played,
                            frames: delivered_frames,
                            turns,
                        });
                    }
                    break;
                }
            }
        }

        if buf.len() >= frame {
            if !outbound_on {
                if cushion && play_head.generating() && buf.len() < cushion_enough {
                    let since = *reply_buffered_at.get_or_insert_with(tokio::time::Instant::now);
                    if since.elapsed() < PLAYOUT_CUSHION {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                }
                reply_buffered_at = None;
                outbound_on = true;
                turns += 1;
                burst_left = burst_cap;
                seg_bytes = 0;
                seg_late_ms = 0;
                info!(
                    "Gemini first_inject serial={serial} turn={turns} buf_bytes={}",
                    buf.len()
                );
                // Room noise still queued on the phone ahead of this reply: drop it
                // (a non-Gemini tts_start clears the phone queue) once the previous
                // AI audio has surely played out.
                if bed_since_speech
                    && tokio::time::Instant::now() > speech_tail + AMBIENCE_CUT_AFTER
                {
                    let queued_ms = phone_tail
                        .saturating_duration_since(tokio::time::Instant::now())
                        .as_millis();
                    let _ = send_gemini_media_json(
                        sink,
                        json!({"type": "tts_start", "source": "ambience_cut"}),
                        "ambience cut",
                        media_call_guard,
                    )
                    .await;
                    drop_unplayed_recording(active_call, media_call_guard, Duration::ZERO, false)
                        .await;
                    phone_tail = tokio::time::Instant::now();
                    info!(
                        "Gemini ambience serial={serial} cut queued room noise (~{queued_ms} ms) before turn={turns}"
                    );
                }
                bed_since_speech = false;
                if let Err(e) = send_gemini_media_json(
                    sink,
                    json!({
                        "type": "tts_start",
                        "sample_rate": sr,
                        "source": "gemini_live_tts",
                        "frame_ms": FRAME_MS,
                        "burst_ms": BURST_MS,
                    }),
                    "tts_start",
                    media_call_guard,
                )
                .await
                {
                    cancel.store(true, Ordering::Relaxed);
                    return Err(e.context("gemini media ws tts_start send"));
                }
                set_gemini_tts_outbound(active_call, media_call_guard, true).await;
            } else if ran_empty {
                seg_late_ms += tokio::time::Instant::now()
                    .saturating_duration_since(speech_tail)
                    .as_millis() as u64;
            }
            ran_empty = false;
            if burst_left == 0 {
                interval.tick().await;
            } else {
                burst_left = burst_left.saturating_sub(frame);
            }
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let mut chunk: Vec<u8> = buf.drain(..frame).collect();
            if !media_call_guard.is_current() {
                break;
            }
            if let Some(a) = amb.as_mut() {
                a.mix_under(&mut chunk, amb_continuous);
            }
            if let Err(e) =
                deliver_gemini_media_binary(sink, active_call, chunk, "binary", media_call_guard)
                    .await
            {
                cancel.store(true, Ordering::Relaxed);
                return Err(e);
            }
            phone_tail = phone_tail.max(tokio::time::Instant::now()) + frame_dur;
            speech_tail = phone_tail;
            play_head.set_speech_tail(speech_tail);
            play_head.dequeue(frame as u64);
            total_played += frame as u64;
            seg_bytes += frame as u64;
            delivered_frames += 1;
            let before = delivered_bytes.fetch_add(frame as u64, Ordering::Relaxed);
            if before == 0 {
                info!(
                    "Gemini first_delivery serial={serial} bytes={} frame={} turn={turns}",
                    frame, delivered_frames
                );
            }
            empty_ticks = 0;
        } else {
            ran_empty = true;
            // Not enough for a frame: wait briefly for more audio or end segment.
            tokio::select! {
                _ = interval.tick() => {
                    empty_ticks = empty_ticks.saturating_add(1);
                }
                chunk = dl_rx.recv() => {
                    match chunk {
                        Some(c) if !c.is_empty() => {
                            buf.extend_from_slice(&c);
                            empty_ticks = 0;
                        }
                        Some(_) => {}
                        None => {
                            // Channel closed — play remainder then exit.
                            if !buf.is_empty() {
                                // pad to even
                                if buf.len() % 2 == 1 {
                                    buf.push(0);
                                }
                                if !outbound_on {
                                    outbound_on = true;
                                    if let Err(e) = send_gemini_media_json(
                                        sink,
                                        json!({
                                            "type": "tts_start",
                                            "sample_rate": sr,
                                            "source": "gemini_live_tts",
                                        }),
                                        "tail tts_start",
                                        media_call_guard,
                                    )
                                    .await
                                    {
                                        cancel.store(true, Ordering::Relaxed);
                                        return Err(
                                            e.context("gemini media ws tail tts_start send")
                                        );
                                    }
                                    set_gemini_tts_outbound(
                                        active_call,
                                        media_call_guard,
                                        true,
                                    )
                                    .await;
                                }
                                while buf.len() >= frame {
                                    interval.tick().await;
                                    let mut chunk: Vec<u8> = buf.drain(..frame).collect();
                                    if !media_call_guard.is_current() {
                                        break;
                                    }
                                    if let Some(a) = amb.as_mut() {
                                        a.mix_under(&mut chunk, amb_continuous);
                                    }
                                    if let Err(e) = deliver_gemini_media_binary(
                                        sink,
                                        active_call,
                                        chunk.clone(),
                                        "tail binary",
                                        media_call_guard,
                                    )
                                    .await
                                    {
                                        cancel.store(true, Ordering::Relaxed);
                                        return Err(e);
                                    }
                                    play_head.dequeue(frame as u64);
                                    total_played += frame as u64;
                                    delivered_frames += 1;
                                    let before = delivered_bytes
                                        .fetch_add(frame as u64, Ordering::Relaxed);
                                    if before == 0 {
                                        info!(
                                            "Gemini first_delivery serial={serial} bytes={} frame={} turn={turns}",
                                            frame, delivered_frames
                                        );
                                    }
                                }
                                if !buf.is_empty() {
                                    interval.tick().await;
                                    let mut last = std::mem::take(&mut buf);
                                    fade_out_s16le(&mut last);
                                    if !media_call_guard.is_current() {
                                        break;
                                    }
                                    if let Err(e) = deliver_gemini_media_binary(
                                        sink,
                                        active_call,
                                        last.clone(),
                                        "final partial binary",
                                        media_call_guard,
                                    )
                                    .await
                                    {
                                        cancel.store(true, Ordering::Relaxed);
                                        return Err(e);
                                    }
                                    play_head.dequeue(last.len() as u64);
                                    total_played += last.len() as u64;
                                    delivered_frames += 1;
                                    let before = delivered_bytes
                                        .fetch_add(last.len() as u64, Ordering::Relaxed);
                                    if before == 0 {
                                        info!(
                                            "Gemini first_delivery serial={serial} bytes={} frame={} turn={turns}",
                                            last.len(), delivered_frames
                                        );
                                    }
                                }
                            }
                            if outbound_on {
                                play_head.set_outbound(false);
                                set_gemini_tts_outbound(
                                    active_call,
                                    media_call_guard,
                                    false,
                                )
                                .await;
                                let _ = send_gemini_media_json(
                                    sink,
                                    json!({"type":"tts_end","source":"gemini_live_tts"}),
                                    "tail tts_end",
                                    media_call_guard,
                                )
                                .await;
                            }
                            info!(
                                "Gemini continuous downlink end serial={serial} played={total_played} turns={turns}"
                            );
                            return Ok(GeminiDeliveryStats {
                                bytes: total_played,
                                frames: delivered_frames,
                                turns,
                            });
                        }
                    }
                }
            }
            // The model is still streaming this reply: its audio comes in lumps
            // barely ahead of real time (gemini-3.8-live-extended-thinking), so a
            // quiet buffer here is a gap between lumps, not the end of the reply.
            let mid_reply = play_head.generating() && empty_ticks < MID_REPLY_MAX_EMPTY_TICKS;
            if outbound_on && buf.len() < frame && empty_ticks >= END_SEG_EMPTY_TICKS && mid_reply {
                // Same head burst for the next lump as a new segment would get.
                burst_left = burst_cap;
            } else if outbound_on && buf.len() < frame && empty_ticks >= END_SEG_EMPTY_TICKS {
                // Voice-following bed: let it fade out after the voice instead of
                // stopping dead (still inside the echo gate's window of our speech).
                if !amb_continuous {
                    if let Some(a) = amb.as_mut() {
                        while a.env() > 0.0 {
                            let pcm = a.bed_frame(frame, 0.0);
                            if deliver_gemini_media_binary(
                                sink,
                                active_call,
                                pcm,
                                "ambience fade",
                                media_call_guard,
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                            phone_tail = phone_tail.max(tokio::time::Instant::now()) + frame_dur;
                        }
                    }
                }
                // End this spoken segment so phone uplink isn't treated as sidetone forever.
                play_head.set_outbound(false);
                set_gemini_tts_outbound(active_call, media_call_guard, false).await;
                let _ = send_gemini_media_json(
                    sink,
                    json!({"type":"tts_end","source":"gemini_live_tts"}),
                    "idle tts_end",
                    media_call_guard,
                )
                .await;
                outbound_on = false;
                empty_ticks = 0;
                // Keep tiny remainder for next segment (slack-drained).
                info!(
                    "Gemini reply played serial={serial} turn={turns} audio_ms={} late_ms={seg_late_ms} cushion={cushion}",
                    seg_bytes * 1000 / (u64::from(sr) * 2).max(1)
                );
                if seg_late_ms >= FRAME_MS && !cushion {
                    cushion = true;
                    info!(
                        "Gemini playout cushion on serial={serial} ({} ms): the model streams barely ahead of real time",
                        PLAYOUT_CUSHION.as_millis()
                    );
                }
            }
            // Echo-free phone: keep the room going through the pause, just ahead of
            // the phone's playout (a deeper cushion would delay the next reply).
            // Mid-reply only once the phone has run dry: room noise queued ahead
            // of the next lump would sit inside a word (every reply
            // of the thinking model had 40–80 ms holes on an echo-free phone).
            if amb_continuous && !amb_off_for_owner && buf.len() < frame {
                if let Some(a) = amb.as_mut() {
                    let lead = if mid_reply { Duration::ZERO } else { AMBIENCE_LEAD };
                    let mut sent = 0;
                    while sent < 4 && {
                        let ahead =
                            phone_tail.saturating_duration_since(tokio::time::Instant::now());
                        ahead.is_zero() || ahead < lead
                    } {
                        let pcm = a.bed_frame(frame, 1.0);
                        if deliver_gemini_media_bed(sink, active_call, pcm, media_call_guard)
                            .await
                            .is_err()
                        {
                            break;
                        }
                        phone_tail = phone_tail.max(tokio::time::Instant::now()) + frame_dur;
                        bed_since_speech = true;
                        sent += 1;
                    }
                }
            }
        }
    }

    if !buf.is_empty() {
        play_head.drop_unplayed(buf.len() as u64);
        info!(
            "Gemini continuous downlink cancel dropped serial={serial} unplayed={} played={total_played}"
        , buf.len());
        buf.clear();
    }
    play_head.set_outbound(false);
    if outbound_on {
        set_gemini_tts_outbound(active_call, media_call_guard, false).await;
        let _ = send_gemini_media_json(
            sink,
            json!({"type":"tts_end","source":"gemini_live_tts"}),
            "cancel tts_end",
            media_call_guard,
        )
        .await;
    }
    info!("Gemini continuous downlink cancel serial={serial} played={total_played} turns={turns}");
    Ok(GeminiDeliveryStats {
        bytes: total_played,
        frames: delivered_frames,
        turns,
    })
}

async fn send_tts_pcm(
    sink: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    state: &AppState,
    tts_pcm: &[u8],
    active_call: &ActiveCallSlot,
    source: &str,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    send_tts_pcm_guarded(sink, state, tts_pcm, active_call, source, cancel, None).await
}

async fn send_tts_pcm_guarded(
    sink: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    state: &AppState,
    tts_pcm: &[u8],
    active_call: &ActiveCallSlot,
    source: &str,
    cancel: Option<&AtomicBool>,
    media_call_guard: Option<&MediaCallGuard>,
) -> Result<()> {
    if media_call_guard
        .map(|guard| !guard.is_current())
        .unwrap_or(false)
    {
        return Ok(());
    }
    if tts_pcm.is_empty() {
        warn!("send_tts_pcm empty source={source}");
        return Ok(());
    }
    let pacing_started_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let t0 = tokio::time::Instant::now();
    // Stable reverse-USB: larger frames (fewer WS messages). 40 ms @ 16 kHz mono s16le = 1280 B.
    // Aggressive head-burst ~1s without per-frame wait so phone inject deeply prebuffers USB path.
    // Wall-clock pad after loop ensures return only after full playout duration (not early).
    const FRAME_MS: u64 = 40;
    const BURST_MS: u64 = 1000;
    let start = json!({
        "type": "tts_start",
        "sample_rate": state.settings.sample_rate,
        "source": source,
        "pcm_bytes": tts_pcm.len(),
        "frame_ms": FRAME_MS,
        "burst_ms": BURST_MS,
    });
    if let Some(guard) = media_call_guard {
        send_gemini_media_json(sink, start, "guarded tts_start", guard).await?;
    } else {
        send_json(sink, start).await?;
    }
    agent_dbg(
        "main.rs:send_tts_pcm",
        "tts_start",
        "F",
        json!({
            "source": source,
            "pcm_bytes": tts_pcm.len(),
            "sample_rate": state.settings.sample_rate,
            "frame_ms": FRAME_MS,
            "burst_ms": BURST_MS,
            "phase": "PlayPrompt",
            "pacing_started_ms": pacing_started_ms,
        }),
    );
    if let Some(guard) = media_call_guard {
        set_gemini_tts_outbound(active_call, guard, true).await;
    } else if let Some(c) = active_call.lock().await.as_mut() {
        c.set_tts_outbound_active(true);
    }
    let frame = (state.settings.sample_rate as usize * FRAME_MS as usize / 1000) * 2;
    let frame = frame.max(2);
    let burst_bytes =
        (state.settings.sample_rate as usize).saturating_mul(BURST_MS as usize) / 1000 * 2;
    let mut interval = tokio::time::interval(Duration::from_millis(FRAME_MS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First tick completes immediately for post-burst pacing.
    interval.tick().await;
    let mut sent = 0usize;
    let mut cancelled = false;
    let mut send_err: Option<anyhow::Error> = None;
    let mut burst_left = burst_bytes;
    let mut frame_i = 0usize;
    let mut logged_first_binary = false;
    for chunk in tts_pcm.chunks(frame) {
        if media_call_guard
            .map(|guard| !guard.is_current())
            .unwrap_or(false)
        {
            cancelled = true;
            break;
        }
        if cancel.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false) {
            cancelled = true;
            let mut faded = chunk.to_vec();
            fade_out_s16le(&mut faded);
            if burst_left == 0 {
                interval.tick().await;
            }
            if let Some(guard) = media_call_guard {
                if let Err(e) = deliver_gemini_media_binary(
                    sink,
                    active_call,
                    faded.clone(),
                    "guarded cancel fade",
                    guard,
                )
                .await
                {
                    send_err = Some(e);
                    break;
                }
            } else {
                let send_result = {
                    let mut s = sink.lock().await;
                    s.send(Message::Binary(faded.clone().into())).await
                };
                if let Err(e) = send_result {
                    send_err = Some(e.into());
                    break;
                }
            }
            if media_call_guard.is_none() {
                if let Some(c) = active_call.lock().await.as_mut() {
                    c.write_tts(&faded);
                }
            }
            sent += faded.len();
            break;
        }
        // Head burst: ship ASAP so inject queue prebuffers. Then realtime wall-clock.
        if burst_left == 0 {
            interval.tick().await;
        } else {
            burst_left = burst_left.saturating_sub(chunk.len());
            if frame_i > 0 && frame_i % 4 == 0 {
                tokio::task::yield_now().await;
            }
        }
        if media_call_guard
            .map(|guard| !guard.is_current())
            .unwrap_or(false)
        {
            cancelled = true;
            break;
        }
        if !logged_first_binary {
            info!(
                "first_ws_binary_to_device source={source} bytes={} send_tts_elapsed_ms={} (agent→client WS; bridge/phone after)",
                chunk.len(),
                t0.elapsed().as_millis()
            );
            logged_first_binary = true;
        }
        if let Some(guard) = media_call_guard {
            if let Err(e) = deliver_gemini_media_binary(
                sink,
                active_call,
                chunk.to_vec(),
                "guarded binary",
                guard,
            )
            .await
            {
                send_err = Some(e);
                break;
            }
        } else {
            let mut s = sink.lock().await;
            if let Err(e) = s.send(Message::Binary(chunk.to_vec().into())).await {
                send_err = Some(e.into());
                break;
            }
            drop(s);
        }
        if media_call_guard.is_none() {
            if let Some(c) = active_call.lock().await.as_mut() {
                c.write_tts(chunk);
            }
        }
        sent += chunk.len();
        frame_i += 1;
    }
    let still_current = if let Some(guard) = media_call_guard {
        set_gemini_tts_outbound(active_call, guard, false).await;
        guard.is_current()
    } else {
        if let Some(c) = active_call.lock().await.as_mut() {
            c.set_tts_outbound_active(false);
        }
        true
    };
    if let Some(e) = send_err {
        return Err(e);
    }
    if !still_current {
        return Ok(());
    }
    if let Some(guard) = media_call_guard {
        send_gemini_media_json(
            sink,
            json!({"type":"tts_end","source": source}),
            "guarded tts_end",
            guard,
        )
        .await?;
    } else {
        send_json(sink, json!({"type":"tts_end","source": source})).await?;
    }
    let secs = tts_pcm.len() as f64 / (state.settings.sample_rate as f64 * 2.0);
    if cancelled {
        info!(
            "TTS paced barge-in stop source={source} sent_bytes={sent}/{} (~{secs:.1}s full) frame_ms={FRAME_MS} burst_ms={BURST_MS}",
            tts_pcm.len()
        );
    } else {
        // Head burst finishes network-send ~BURST_MS early vs phone injector drain.
        // Sleep remaining wall-clock so callers (greet gate / hangup) never race mid-playout.
        let full_ms = (tts_pcm.len() as u64).saturating_mul(1000)
            / (state.settings.sample_rate.max(8000) as u64 * 2);
        let elapsed_ms = t0.elapsed().as_millis() as u64;
        let pad_ms = full_ms.saturating_sub(elapsed_ms);
        if pad_ms > 0 {
            info!(
                "TTS wall-clock pad source={source} pad_ms={pad_ms} full_ms={full_ms} elapsed_ms={elapsed_ms} (phone still playing prebuf)"
            );
            tokio::time::sleep(Duration::from_millis(pad_ms)).await;
        }
        info!(
            "TTS paced done source={source} pcm_bytes={} (~{secs:.1}s) frame_ms={FRAME_MS} burst_ms={BURST_MS} wall_pad_ms={pad_ms}",
            tts_pcm.len()
        );
    }
    agent_dbg(
        "main.rs:send_tts_pcm",
        "tts_done",
        "D",
        json!({
            "source": source,
            "sent_bytes": sent,
            "total_bytes": tts_pcm.len(),
            "cancelled": cancelled,
            "frame_ms": FRAME_MS,
            "burst_ms": BURST_MS,
            "pacing_started_ms": pacing_started_ms,
            "pacing_elapsed_ms": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0).saturating_sub(pacing_started_ms),
        }),
    );
    Ok(())
}

async fn handle_utterance(
    sink: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    state: &AppState,
    pcm: &[u8],
    history: &mut Vec<ChatMessage>,
    sample_rate: u32,
    active_call: &ActiveCallSlot,
) -> Result<()> {
    let transcript = transcribe_pcm16(&state.http, &state.settings, pcm, sample_rate).await?;
    send_json(sink, json!({"type":"transcript","text": transcript})).await?;
    if transcript.is_empty() {
        return Ok(());
    }
    let reply = chat(&state.http, &state.settings, &transcript, history).await?;
    history.push(ChatMessage {
        role: "user".into(),
        content: transcript,
    });
    history.push(ChatMessage {
        role: "assistant".into(),
        content: reply.clone(),
    });
    send_json(sink, json!({"type":"reply_text","text": reply})).await?;
    if reply.is_empty() {
        return Ok(());
    }
    let tts_pcm = synthesize_piper(&state.http, &state.settings, &reply).await;
    send_tts_pcm(sink, state, &tts_pcm, active_call, "piper_llm", None).await
}

async fn send_json(
    sink: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    payload: Value,
) -> Result<()> {
    let text = serde_json::to_string(&payload)?;
    let mut s = sink.lock().await;
    s.send(Message::Text(text.into())).await?;
    Ok(())
}

#[cfg(test)]
mod ring_prewarm_tests {
    use super::*;

    fn entry(
        caller: &str,
        session: tokio::task::JoinHandle<anyhow::Result<gemini_live::SessionEndReason>>,
    ) -> (RingPrewarm, Arc<AtomicBool>) {
        let cancel = Arc::new(AtomicBool::new(false));
        let (up_tx, _up_rx) = tokio::sync::mpsc::unbounded_channel();
        let (_dl_tx, dl_rx) = tokio::sync::mpsc::unbounded_channel();
        let pw = RingPrewarm {
            id: 0,
            caller_id: caller.to_string(),
            sample_rate: 16_000,
            started: tokio::time::Instant::now(),
            core: GeminiSessionCore {
                session,
                dl_rx,
                media_guard: Arc::new(std::sync::OnceLock::new()),
                force_hangup: Arc::new(AtomicBool::new(false)),
                delivered_bytes: Arc::new(AtomicU64::new(0)),
                play_head: gemini_live::DownlinkPlayHead::new(),
                stall_engaged: Arc::new(AtomicBool::new(false)),
                stall_judgement: Default::default(),
                urgent_reason: Default::default(),
                urgent_call: None,
                barge_in: Arc::new(AtomicBool::new(false)),
                appointments: None,
                caller: Arc::new(std::sync::Mutex::new(caller.to_string())),
            },
            up_tx,
            cancel: cancel.clone(),
            transcript: Arc::new(std::sync::Mutex::new(String::new())),
            audio_open: Arc::new(AtomicBool::new(false)),
            audio_opened_at: Arc::new(std::sync::Mutex::new(None)),
            setup_ready: Arc::new(AtomicBool::new(false)),
            connect_failed: Arc::new(AtomicBool::new(false)),
            cfg: GeminiRetryCfg {
                api_key: String::new(),
                model: String::new(),
                system: String::new(),
                voice_name: String::new(),
                caller_id: caller.to_string(),
                lang: usual(),
            },
            plan: plan_of(usual()),
            persona: true,
            goodbye_hangup_delay_ms: 1_000,
        };
        (pw, cancel)
    }

    /// A Taiwanese call's languages: the setting's opening, the usual hints.
    fn usual() -> gemini_live::CallLangPlan {
        gemini_live::CallLangPlan {
            opening: None,
            caller_stt: Some(vec!["zh-TW", "en-US"]),
            ..Default::default()
        }
    }

    /// A set-up with these languages, no carrier-prompt hold, appointments off.
    fn plan_of(lang: gemini_live::CallLangPlan) -> CallerPlan {
        CallerPlan {
            lang,
            intl_hold: false,
            appointments: false,
            contact: None,
        }
    }

    /// Traditional Chinese, Taiwan's calling code.
    fn tw_settings() -> RuntimeSettings {
        let mut rt = RuntimeSettings::from_base(&Settings::from_env());
        rt.language = "zh-TW".into();
        rt.country_code = "886".into();
        rt.sim_languages = Default::default();
        rt.appointment_mode = "off".into();
        rt
    }

    fn live() -> tokio::task::JoinHandle<anyhow::Result<gemini_live::SessionEndReason>> {
        tokio::spawn(std::future::pending())
    }

    #[test]
    fn ringing_caller_matches_across_formats() {
        assert!(same_ringing_caller("0900111222", "0900111222"));
        assert!(same_ringing_caller("+886900111222", "0900-111-222"));
        assert!(same_ringing_caller("", "0900111222"));
        assert!(same_ringing_caller("0900111222", ""));
        assert!(!same_ringing_caller("0912345678", "0900111222"));
        assert!(!same_ringing_caller("110", "119"));
    }

    #[tokio::test]
    async fn pickup_takes_the_ringing_call_only() {
        let reg = RingPrewarmRegistry::default();
        let (pw, cancel) = entry("+886900111222", live());
        reg.insert("pixel-1", pw);
        assert_eq!(reg.pending(), 1);
        let got = reg
            .take("pixel-1", "0900111222", 16_000, &plan_of(usual()))
            .expect("same call");
        assert!(!cancel.load(Ordering::Relaxed));
        assert_eq!(reg.pending(), 0);
        drop(got);

        // A stale number at ring time (call-log fallback) is still this phone's
        // ringing call; another sample rate is not usable.
        let (pw, cancel) = entry("0912345678", live());
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "0900111222", 16_000, &plan_of(usual())).is_some());
        assert!(!cancel.load(Ordering::Relaxed));
        let (pw, cancel) = entry("0912345678", live());
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "0912345678", 8_000, &plan_of(usual())).is_none());
        assert!(cancel.load(Ordering::Relaxed));

        // A session that already ended is never handed over.
        let done = tokio::spawn(async { Ok(gemini_live::SessionEndReason::Cancelled) });
        while !done.is_finished() {
            tokio::task::yield_now().await;
        }
        let (pw, _) = entry("0912345678", done);
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "0912345678", 16_000, &plan_of(usual())).is_none());
    }

    #[tokio::test]
    async fn pickup_drops_a_session_set_up_for_other_languages() {
        // Rang without the SIM (guessed Taiwanese); the hello says a US SIM set to
        // English: the held 「喂？」 and its speech-to-text hints are the wrong ones.
        let reg = RingPrewarmRegistry::default();
        let (pw, cancel) = entry("2025550143", live());
        reg.insert("pixel-1", pw);
        let us = gemini_live::CallLangPlan {
            opening: Some(locale::Lang::En),
            caller_stt: Some(vec!["en-US", "zh-TW"]),
            ..Default::default()
        };
        assert!(reg.take("pixel-1", "2025550143", 16_000, &plan_of(us)).is_none());
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(reg.pending(), 0);
        // Same opening, other hints: not handed over either.
        let (pw, cancel) = entry("+12025550143", live());
        reg.insert("pixel-1", pw);
        let auto = gemini_live::CallLangPlan { opening: None, caller_stt: None, ..Default::default() };
        assert!(reg.take("pixel-1", "+12025550143", 16_000, &plan_of(auto)).is_none());
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn a_sims_own_number_gets_its_whole_prompt_in_the_sims_language() {
        // Taiwanese setup with a US SIM set to English (setting sim_languages).
        let mut rt = tw_settings();
        rt.gemini_persona_mode = true;
        rt.owner_name = "林先生".into();
        rt.sim_languages = [("us".to_string(), "en".to_string())].into_iter().collect();
        let en_guard = crate::prompts::pack(locale::Lang::En).injection_guard.clone();
        let tw_guard = crate::prompts::pack(locale::Lang::ZhTw).injection_guard.clone();
        // A US number on the US SIM: English prompt, stalling prompt and guard.
        let us = caller_plan(&rt, "(202) 555-0143", Some("us"));
        assert_eq!(us.lang.prompt_lang, Some(locale::Lang::En));
        let p = rt.gemini_system_prompt_for_call_in(us.lang.prompt_lang);
        assert!(p.contains(en_guard.trim()) && !p.contains(tw_guard.trim()), "{p}");
        let stall = rt.persona_stall_system_prompt_in(us.lang.prompt_lang);
        assert!(stall.contains(en_guard.trim()) && !stall.contains(tw_guard.trim()), "{stall}");
        // A Taiwanese number on the Taiwan SIM: unchanged, Chinese throughout.
        let tw = caller_plan(&rt, "0912345678", Some("tw"));
        assert_eq!(tw.lang.prompt_lang, None);
        assert_eq!(rt.gemini_system_prompt_for_call_in(tw.lang.prompt_lang), rt.gemini_system_prompt_for_call());
        assert!(rt.gemini_system_prompt_for_call().contains(tw_guard.trim()));
        assert_eq!(rt.persona_stall_system_prompt_in(None), rt.persona_stall_system_prompt());
        // A Taiwanese number on the US SIM is not the SIM's own: the setting's prompt.
        assert_eq!(caller_plan(&rt, "+886912345678", Some("us")).lang.prompt_lang, None);
    }

    #[test]
    fn a_ring_without_the_sim_serves_the_local_call_it_picks_up() {
        // The SIM was not known while it rang (no earlier call): a local number plans
        // the same session either way, so the held 「喂？」 is used.
        let rt = tw_settings();
        let rang = caller_plan(&rt, "0912345678", None);
        let picked = caller_plan(&rt, "0912345678", Some("tw"));
        assert_eq!((rang.lang.sim_local, picked.lang.sim_local), (None, Some(true)));
        assert_eq!(rang.lang, picked.lang);
    }

    #[tokio::test]
    async fn a_hidden_number_keeps_the_ring_session() {
        let rt = tw_settings();
        let reg = RingPrewarmRegistry::default();
        // Rang and picked up with a hidden number: both wait for the carrier's
        // warning, so the ring's session is adopted.
        let hidden = caller_plan(&rt, "unknown", Some("tw"));
        assert!(hidden.intl_hold);
        let (mut pw, cancel) = entry("", live());
        pw.plan = caller_plan(&rt, "", Some("tw"));
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "unknown", 16_000, &hidden).is_some());
        assert!(!cancel.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn pickup_decides_again_with_the_hellos_number() {
        let rt = tw_settings();
        let reg = RingPrewarmRegistry::default();
        // It rang with the previous call's number (call-log fallback); the hello has
        // the real one, from abroad: the carrier's international-call warning and
        // the opening language were decided for the wrong number.
        let rang = caller_plan(&rt, "0912345678", Some("tw"));
        let picked = caller_plan(&rt, "+12025550143", Some("tw"));
        assert!(!rang.intl_hold && picked.intl_hold);
        assert_ne!(rang.lang, picked.lang);
        let (mut pw, cancel) = entry("0912345678", live());
        pw.plan = rang;
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "+12025550143", 16_000, &picked).is_none());
        assert!(cancel.load(Ordering::Relaxed));
        // Only the carrier prompt differs (both numbers foreign to the SIM, same
        // language): not handed over either.
        let mut held = caller_plan(&rt, "+12025550143", Some("tw"));
        held.intl_hold = false;
        let (mut pw, cancel) = entry("+12025550143", live());
        pw.plan = held;
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "+12025550143", 16_000, &picked).is_none());
        assert!(cancel.load(Ordering::Relaxed));

        // Rang without a number, picked up with a local one: the same set-up, so
        // the session is adopted and reads the hello's number from then on.
        let local = caller_plan(&rt, "0922222222", Some("tw"));
        let hidden = caller_plan(&rt, "", Some("tw"));
        // A hidden number waits for the carrier's warning, a local one does not;
        // the session decides again with the number it has at pickup.
        assert!(hidden.intl_hold && !local.intl_hold);
        assert_eq!(hidden.lang, local.lang);
        let (mut pw, cancel) = entry("", live());
        pw.plan = hidden.clone();
        reg.insert("pixel-1", pw);
        let mut got = reg
            .take("pixel-1", "0922222222", 16_000, &local)
            .expect("same set-up");
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(take_hello_caller(&got.core, &mut got.cfg, " 0922222222 "));
        assert_eq!(got.cfg.caller_id, "0922222222");
        assert_eq!(*got.core.caller.lock().unwrap(), "0922222222");
        // An empty hello number keeps the one it has.
        assert!(!take_hello_caller(&got.core, &mut got.cfg, ""));
        assert_eq!(got.cfg.caller_id, "0922222222");

        // Appointments on: the session is bound to the number it rang with, so
        // another number at pickup starts a fresh one; the same number does not.
        let mut rt = rt;
        rt.appointment_mode = "confirm".into();
        let booked = caller_plan(&rt, "0922222222", Some("tw"));
        assert!(booked.appointments);
        let (mut pw, cancel) = entry("0912345678", live());
        pw.plan = caller_plan(&rt, "0912345678", Some("tw"));
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "0922222222", 16_000, &booked).is_none());
        assert!(cancel.load(Ordering::Relaxed));
        let (mut pw, _) = entry("0922222222", live());
        pw.plan = booked.clone();
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "0922222222", 16_000, &booked).is_some());
    }

    #[tokio::test]
    async fn pickup_drops_a_session_set_up_for_another_contact() {
        let reg = RingPrewarmRegistry::default();
        let named = |id: Option<&str>| CallerPlan { contact: id.map(str::to_string), ..plan_of(usual()) };
        // Rang with one contact's number, picked up with another's, or one that is
        // no contact (or the other way round): the held prompt names the wrong person.
        for (rang, picked) in [(Some("a"), Some("b")), (Some("a"), None), (None, Some("b"))] {
            let (mut pw, cancel) = entry("0912345678", live());
            pw.plan = named(rang);
            reg.insert("pixel-1", pw);
            assert!(reg.take("pixel-1", "0922222222", 16_000, &named(picked)).is_none(), "{rang:?} {picked:?}");
            assert!(cancel.load(Ordering::Relaxed));
        }
        // The same contact (its number written another way): adopted.
        let (mut pw, cancel) = entry("+886912345678", live());
        pw.plan = named(Some("a"));
        reg.insert("pixel-1", pw);
        assert!(reg.take("pixel-1", "0912345678", 16_000, &named(Some("a"))).is_some());
        assert!(!cancel.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn new_ring_replaces_and_expiry_only_drops_its_own_entry() {
        let reg = RingPrewarmRegistry::default();
        let (first, first_cancel) = entry("0912345678", live());
        let first_id = reg.insert("pixel-1", first);
        let (second, second_cancel) = entry("0900111222", live());
        let second_id = reg.insert("pixel-1", second);
        assert!(first_cancel.load(Ordering::Relaxed));
        assert_ne!(first_id, second_id);
        // The first ring's expiry timer must not drop the second ring.
        reg.cancel("pixel-1", Some(first_id), "not answered");
        assert_eq!(reg.pending(), 1);
        assert!(!second_cancel.load(Ordering::Relaxed));
        // incoming_cancel (caller hung up while it rang).
        reg.cancel("pixel-1", None, "call ended before pickup");
        assert_eq!(reg.pending(), 0);
        assert!(second_cancel.load(Ordering::Relaxed));
    }
}

#[cfg(test)]
mod unknown_caller_tests {
    use super::{
        canonical_device_serial, cid_route_after_wait, cid_route_on_hello,
        extract_caller_id_from_msg, is_immediate_hidden_caller, is_isolated_screening_serial,
        call_sim_country, is_unknown_or_hidden_caller, note_call_sim_country,
        sim_country_from_msg, summary_end_hint, CidRoute, MediaCallGuard, MediaPlayGate,
        MediaPlayKind, RejectPending, EMPTY_CID_WAIT, GEMINI_MEDIA_SEND_TIMEOUT,
        REJECT_PENDING_TTL,
    };
    use serde_json::json;
    use std::time::Duration;

    #[tokio::test]
    async fn media_call_guard_invalidates_older_generations() {
        let inactive = MediaCallGuard::inactive();
        assert!(!inactive.is_current());

        let first = inactive.next().await;
        assert!(first.is_current());

        let second = first.next().await;
        assert!(!first.is_current());
        assert!(second.is_current());

        second.invalidate().await;
        assert!(!second.is_current());
    }

    #[tokio::test]
    async fn media_call_generation_waits_for_inflight_side_effect_lease() {
        let first = MediaCallGuard::inactive().next().await;
        let read_lease = first.lease_current().await.expect("current lease");
        let old = first.clone();
        let next = tokio::spawn(async move { old.next().await });

        tokio::task::yield_now().await;
        assert!(!next.is_finished());
        drop(read_lease);

        let second = tokio::time::timeout(Duration::from_secs(1), next)
            .await
            .expect("generation writer unblocked")
            .expect("generation task");
        assert!(!first.is_current());
        assert!(second.is_current());
    }

    #[test]
    fn gemini_media_send_timeout_is_bounded() {
        assert_eq!(GEMINI_MEDIA_SEND_TIMEOUT, Duration::from_secs(2));
    }

    #[test]
    fn unknown_cid_patterns() {
        assert!(is_unknown_or_hidden_caller(""));
        assert!(is_unknown_or_hidden_caller("   "));
        assert!(is_unknown_or_hidden_caller("unknown"));
        assert!(is_unknown_or_hidden_caller("Unknown"));
        assert!(is_unknown_or_hidden_caller("PRIVATE"));
        assert!(is_unknown_or_hidden_caller("hidden"));
        assert!(is_unknown_or_hidden_caller("anonymous"));
        assert!(is_unknown_or_hidden_caller("withheld"));
        assert!(is_unknown_or_hidden_caller("Private Number"));
        assert!(is_unknown_or_hidden_caller("number withheld"));
        assert!(is_unknown_or_hidden_caller("未知"));
        assert!(is_unknown_or_hidden_caller("保密"));
        assert!(is_unknown_or_hidden_caller("***"));
        assert!(!is_unknown_or_hidden_caller("+886912345678"));
        assert!(!is_unknown_or_hidden_caller("0912345678"));
        assert!(!is_unknown_or_hidden_caller("021234567"));
    }

    #[test]
    fn empty_cid_is_not_immediate_hidden() {
        assert!(!is_immediate_hidden_caller(""));
        assert!(!is_immediate_hidden_caller("   "));
        assert_eq!(cid_route_on_hello("pixel-10111", "", false, false), CidRoute::Wait);
        assert_eq!(cid_route_on_hello("pixel-10111", "   ", false, false), CidRoute::Wait);
        assert!(EMPTY_CID_WAIT >= Duration::from_millis(500));
        assert!(EMPTY_CID_WAIT <= Duration::from_secs(3));
    }

    #[test]
    fn visible_msisdn_never_hidden() {
        assert!(!is_unknown_or_hidden_caller("0912345678"));
        assert!(!is_immediate_hidden_caller("0912345678"));
        assert!(!is_unknown_or_hidden_caller("+886912345678"));
        for answer_unknown in [false, true] {
            assert_eq!(
                cid_route_on_hello("pixel-10111", "0912345678", answer_unknown, false),
                CidRoute::Greet
            );
            assert_eq!(
                cid_route_on_hello("pixel-10111", "+886912345678", answer_unknown, false),
                CidRoute::Greet
            );
            assert_eq!(
                cid_route_after_wait("0912345678", answer_unknown, false),
                CidRoute::Greet
            );
        }
    }

    #[test]
    fn reject_list_hangs_up_before_anything_else() {
        // A blocked number is hung up on at once, whatever the hidden-number setting.
        for answer_unknown in [false, true] {
            assert_eq!(
                cid_route_on_hello("pixel-10133", "0255501234", answer_unknown, true),
                CidRoute::RejectList
            );
            assert_eq!(
                cid_route_after_wait("0255501234", answer_unknown, true),
                CidRoute::RejectList
            );
        }
        // The work-profile twin never plays anything, listed or not.
        assert_eq!(
            cid_route_on_hello("pixel-1010111", "0255501234", false, true),
            CidRoute::IgnoreIsolated
        );
        // What rang on the list still counts at pickup, once and only for a while.
        let p = RejectPending::default();
        p.mark("pixel-10133", "0255501234");
        let ring = p.take("pixel-10133").expect("marked");
        assert_eq!(ring.caller, "0255501234");
        assert!(!ring.declined && ring.record_id.is_none());
        assert_eq!(p.take("pixel-10133"), None);
        assert_eq!(p.take("pixel-10111"), None);
        assert!(REJECT_PENDING_TTL >= Duration::from_secs(10));
        // The record says why there was no AI.
        assert!(summary_end_hint(&["reject_list".to_string()], None).contains("黑名單"));
        // An urgent call that went to the owner is never written up as a stalled sales call.
        let urgent = |evs: &[&str]| summary_end_hint(&evs.iter().map(|e| e.to_string()).collect::<Vec<_>>(), None);
        let missed = urgent(&["whitelist_urgent_transfer", "whitelist_urgent_call_failed", "whitelist_urgent_call_failed:no_answer"]);
        assert!(missed.contains("沒接通") && missed.contains("盡快回電") && !missed.contains("陪他聊"), "{missed}");
        assert!(urgent(&["whitelist_urgent_transfer", "whitelist_urgent_call_merged"]).contains("直接跟來電者通話"));
        assert!(urgent(&["whitelist_urgent_transfer"]).contains("盡快回電"));
    }

    #[test]
    fn answer_unknown_setting_greets_hidden_and_empty_cid() {
        // Hidden numbers are answered like any other call.
        for caller in ["", "   ", "unknown", "PRIVATE", "hidden", "保密", "withheld", "***"] {
            assert_eq!(
                cid_route_on_hello("pixel-10111", caller, true, false),
                CidRoute::Greet,
                "caller={caller:?}"
            );
            assert_eq!(
                cid_route_after_wait(caller, true, false),
                CidRoute::Greet,
                "caller={caller:?}"
            );
        }
        // Work-profile twin stays parked even with the setting on.
        assert_eq!(
            cid_route_on_hello("pixel-1010111", "", true, false),
            CidRoute::IgnoreIsolated
        );
    }

    #[test]
    fn explicit_hidden_tokens_still_immediate_reject() {
        for token in ["unknown", "private", "hidden", "保密", "PRIVATE", "withheld"] {
            assert!(
                is_immediate_hidden_caller(token),
                "token={token} should reject immediately"
            );
            assert_eq!(
                cid_route_on_hello("pixel-10111", token, false, false),
                CidRoute::Reject,
                "token={token}"
            );
        }
    }

    #[test]
    fn after_wait_empty_rejects_without_greet() {
        assert_eq!(cid_route_after_wait("", false, false), CidRoute::Reject);
        assert_eq!(cid_route_after_wait("   ", false, false), CidRoute::Reject);
        assert_eq!(cid_route_after_wait("unknown", false, false), CidRoute::Reject);
        assert_ne!(cid_route_after_wait("", false, false), CidRoute::Greet);
    }

    #[test]
    fn isolated_screening_serial_is_ignored_for_ivr() {
        assert!(is_isolated_screening_serial("pixel-1010111"));
        assert!(is_isolated_screening_serial("pixel-1010122"));
        assert!(!is_isolated_screening_serial("pixel-10111"));
        assert!(!is_isolated_screening_serial("pixel-10122"));
        assert!(!is_isolated_screening_serial("pixel-10103"));
        assert_eq!(
            cid_route_on_hello("pixel-1010111", "0912345678", false, false),
            CidRoute::IgnoreIsolated
        );
        assert_eq!(
            cid_route_on_hello("pixel-1010111", "", false, false),
            CidRoute::IgnoreIsolated
        );
        assert_eq!(
            cid_route_on_hello("pixel-1010111", "hidden", false, false),
            CidRoute::IgnoreIsolated
        );
        assert_eq!(canonical_device_serial("pixel-1010111"), "pixel-10111");
        assert_eq!(canonical_device_serial("pixel-10111"), "pixel-10111");
    }

    #[tokio::test]
    async fn media_play_gate_reject_preempts_twin_greet() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let gate = MediaPlayGate::default();
        let greet_cancel = Arc::new(AtomicBool::new(false));
        let reject_cancel = Arc::new(AtomicBool::new(false));
        assert!(
            gate.claim("pixel-10111", MediaPlayKind::Greet, &greet_cancel)
                .await
        );
        assert!(
            !gate
                .claim("pixel-1010111", MediaPlayKind::Greet, &greet_cancel)
                .await,
            "twin serial must not start a second greet"
        );
        assert!(
            gate.claim("pixel-1010111", MediaPlayKind::Reject, &reject_cancel)
                .await
        );
        assert!(greet_cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn extract_from_json() {
        let v = json!({"type":"hello","caller_id":"unknown"});
        assert_eq!(extract_caller_id_from_msg(&v).as_deref(), Some("unknown"));
        let v2 = json!({"type":"hello","from":"+886900000000"});
        assert_eq!(
            extract_caller_id_from_msg(&v2).as_deref(),
            Some("+886900000000")
        );
        let v3 = json!({"type":"hello"});
        assert!(extract_caller_id_from_msg(&v3).is_none());
    }

    #[test]
    fn sim_country_field() {
        // Older phone app: no field, nothing known.
        assert_eq!(sim_country_from_msg(&json!({"type":"incoming"})), None);
        assert_eq!(
            sim_country_from_msg(&json!({"type":"incoming","sim_country":" US "})),
            Some("us".to_string())
        );
        // Sent but unreadable: the SIM is unknown for this call.
        assert_eq!(sim_country_from_msg(&json!({"type":"hello","sim_country":""})), None);
        assert_eq!(sim_country_from_msg(&json!({"type":"hello","sim_country":"usa"})), None);
        assert_eq!(sim_country_from_msg(&json!({"type":"hello","sim_country":7})), None);
    }

    #[test]
    fn sim_country_does_not_outlive_its_call() {
        let serial = "TESTSIM0001";
        note_call_sim_country(serial, &json!({"type":"incoming","sim_country":"us"}));
        assert_eq!(call_sim_country(serial).as_deref(), Some("us"));
        note_call_sim_country(serial, &json!({"type":"hello","sim_country":"tw"}));
        assert_eq!(call_sim_country(serial).as_deref(), Some("tw"));
        // The next call from an app that does not send the field: unknown again.
        note_call_sim_country(serial, &json!({"type":"incoming"}));
        assert_eq!(call_sim_country(serial), None);
        // The phone's last reported SIM stays known apart (the ring prewarm's guess).
        assert_eq!(crate::phone_region::last_sim(serial).as_deref(), Some("tw"));
    }

    #[test]
    fn a_blocked_number_reads_as_dialled_on_the_sim_that_rings() {
        use super::{reject_listed_on_line, reject_reason_on_line};
        let serial = "TESTSIMREJ01";
        let mut rt = crate::settings_store::RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.country_code = "886".into();
        rt.reject_numbers = vec![crate::settings_store::RejectEntry {
            number: "+81 3 1234 5678".into(),
            reason: "営業".into(),
        }];
        // A Japanese SIM rings: the national form is that Japanese number.
        note_call_sim_country(serial, &json!({"type":"incoming","sim_country":"jp"}));
        assert!(reject_listed_on_line(&rt, "03-1234-5678", serial));
        assert_eq!(reject_reason_on_line(&rt, "0312345678", serial).as_deref(), Some("営業"));
        assert!(!reject_listed_on_line(&rt, "03-1234-5679", serial));
        // The same digits on the Taiwan SIM are a Taiwanese number: not blocked.
        note_call_sim_country(serial, &json!({"type":"incoming","sim_country":"tw"}));
        assert!(!reject_listed_on_line(&rt, "03-1234-5678", serial));
        assert_eq!(reject_reason_on_line(&rt, "0312345678", serial), None);
        // The full number is blocked on any SIM.
        assert!(reject_listed_on_line(&rt, "+81312345678", serial));
        note_call_sim_country(serial, &json!({"type":"incoming"}));
    }
}

#[cfg(test)]
mod retention_health_tests {
    use super::*;

    #[test]
    fn retention_shows_in_health_under_both_names() {
        let mut rt = RuntimeSettings::from_base(&Settings::from_env());
        rt.retention_days = 21;
        let got: HashMap<&str, Value> = health_retention(&rt).into_iter().collect();
        assert_eq!((got["retention_days"].as_u64(), got["calls_retention_days"].as_u64()), (Some(21), Some(21)));
    }

    #[test]
    fn setup_missing_follows_the_ready_list() {
        let base = Settings::from_env();
        let mut rt = RuntimeSettings::from_base(&base);
        rt.gemini_live_enabled = true;
        rt.gemini_api_key = String::new();
        rt.owner_name = "  ".into();
        let got = setup_missing(&rt, &rt.effective(&base));
        assert!(got.contains(&"gemini_key") && got.contains(&"owner_name"), "{got:?}");
        assert!(!got.contains(&"ai_off"), "{got:?}");

        rt.gemini_api_key = "test-key".into();
        rt.owner_name = "Alex".into();
        rt.gemini_live_enabled = false;
        let got = setup_missing(&rt, &rt.effective(&base));
        assert!(!got.contains(&"gemini_key") && !got.contains(&"owner_name"), "{got:?}");
        assert!(got.contains(&"ai_off"), "{got:?}");
        // A Gemini key also covers summaries.
        assert!(!got.contains(&"summary"), "{got:?}");
    }
}

#[cfg(test)]
mod call_reattach_tests {
    use super::{
        ActiveCallSlot, CallReattachRegistry, CALL_REATTACH_WINDOW, INCOMING_NOTIFY_DEDUP_WINDOW,
    };
    use crate::calls::CallStore;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::sync::Mutex;

    async fn temp_store() -> Arc<CallStore> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("pixel-call-reattach-{nanos}"));
        CallStore::new(dir, true, false)
    }

    #[tokio::test]
    async fn parked_hello_reuses_call_id() {
        let store = temp_store().await;
        let first = store
            .start("pixel-1", "0911111111", 16000, "active")
            .await
            .unwrap();
        let id = first.id.clone();
        let registry = CallReattachRegistry::default();
        registry.park("pixel-1", "0911111111", first).await;
        let stolen = registry.steal("pixel-1", "0911111111").await.unwrap();
        assert_eq!(stolen.id, id);
        assert!(registry.steal("pixel-1", "0911111111").await.is_none());
    }

    #[tokio::test]
    async fn overlapping_socket_steals_live_slot() {
        let store = temp_store().await;
        let first = store
            .start("pixel-1", "0911111111", 16000, "active")
            .await
            .unwrap();
        let id = first.id.clone();
        let slot: ActiveCallSlot = Arc::new(Mutex::new(Some(first)));
        let registry = CallReattachRegistry::default();
        registry
            .mark_live("pixel-1", "0911111111", &id, slot.clone())
            .await;
        let stolen = registry.steal("pixel-1", "0911111111").await.unwrap();
        assert_eq!(stolen.id, id);
        assert!(slot.lock().await.is_none());
    }

    #[tokio::test]
    async fn forget_after_call_end_does_not_reattach() {
        let store = temp_store().await;
        let first = store
            .start("pixel-1", "0911111111", 16000, "active")
            .await
            .unwrap();
        let id = first.id.clone();
        let registry = CallReattachRegistry::default();
        registry.park("pixel-1", "0911111111", first).await;
        registry.forget("pixel-1", "0911111111", &id).await;
        assert!(registry.steal("pixel-1", "0911111111").await.is_none());
    }

    #[tokio::test]
    async fn different_caller_does_not_steal() {
        let store = temp_store().await;
        let first = store
            .start("pixel-1", "0911111111", 16000, "active")
            .await
            .unwrap();
        let registry = CallReattachRegistry::default();
        registry.park("pixel-1", "0911111111", first).await;
        assert!(registry.steal("pixel-1", "0922222222").await.is_none());
        assert!(registry.steal("pixel-2", "0911111111").await.is_none());
    }

    #[test]
    fn reattach_window_is_short() {
        assert_eq!(CALL_REATTACH_WINDOW, Duration::from_secs(20));
        assert!(CALL_REATTACH_WINDOW <= INCOMING_NOTIFY_DEDUP_WINDOW);
    }
}

#[cfg(test)]
mod summary_fallback_tests {
    use super::*;

    #[test]
    fn stall_fallback_names_the_pitch_not_the_carrier_prompt() {
        let dialogue = "接聽：喂？\n對方：是國際電話, 請注意小心詐騙\n對方：你有聽到嗎?\n接聽：有啊\n對方：我有一個保險想要推薦給你。";
        let s = stall_fallback_summary(dialogue, 71.9, None);
        assert!(s.starts_with("事情：推銷／詐騙電話（提到：保險、推薦）"), "{s}");
        assert!(s.contains("約 72 秒") && s.ends_with("要處理：不用"), "{s}");
        assert!(stall_fallback_summary("對方：你好", 600.0, None).contains("約 10 分鐘"));
    }

    /// The kind of call the call was judged names the matter (2026-11: a fake
    /// prosecutor's call read 「推銷／詐騙電話」 only).
    #[test]
    fn the_judged_kind_of_call_names_the_matter_and_the_end_hint() {
        let judged = calls::ScamJudgement {
            kind: "scam".into(),
            typology: "fake_official".into(),
            confidence: "high".into(),
            source: "review".into(),
            released: false,
        };
        let s = stall_fallback_summary("對方：這裡是地檢署，你的帳戶涉及洗錢", 90.0, Some(&judged));
        assert!(s.starts_with("事情：疑似詐騙：假冒檢警要求監管帳戶"), "{s}");
        let sales = calls::ScamJudgement { kind: "sales".into(), typology: "insurance_sales".into(), ..judged.clone() };
        assert_eq!(scam_topic(&sales).as_deref(), Some("推銷電話：保險推銷"));
        let hint = summary_end_hint(&["persona_stall_spam".to_string()], Some(&judged));
        assert!(hint.contains("疑似詐騙：假冒檢警要求監管帳戶") && hint.contains("陪他聊"), "{hint}");
        let off = summary_end_hint(&["persona_scam_detected".to_string()], Some(&judged));
        assert!(off.contains("陪聊功能關著"), "{off}");
        let let_go = summary_end_hint(&["persona_stall_spam".to_string(), "persona_stall_released".to_string()], Some(&judged));
        assert!(let_go.contains("正常電話"), "{let_go}");
        // A kind the database does not know: the old wording.
        let unknown = calls::ScamJudgement { typology: String::new(), ..judged };
        assert!(summary_end_hint(&["persona_stall_spam".to_string()], Some(&unknown)).contains("像推銷／詐騙"));
    }

    #[test]
    fn long_dialogue_is_clipped_head_and_tail() {
        let d = "對方：".to_string() + &"保".repeat(9_000);
        let c = clip_dialogue_for_summary(&d);
        assert!(c.contains("中間省略") && c.chars().count() < 5_600);
        assert_eq!(clip_dialogue_for_summary("接聽：喂？"), "接聽：喂？");
    }
}

#[cfg(test)]
mod call_control_tests {
    use super::*;

    #[tokio::test]
    async fn ai_fail_hangup_lets_the_call_end_through_between_attempts() {
        let guard = MediaCallGuard::inactive().next().await;
        let phase = Arc::new(std::sync::Mutex::new(IvrPhase::LiveAi));
        let slot: ActiveCallSlot = Arc::new(Mutex::new(None));
        let g = &guard;
        let run = ai_fail_then_hang_up("pixel-1", &phase, &slot, &guard, move || async move {
            // Hangup attempt 1: a guarded send holds the lease while it writes…
            let first = g.lease_current().await;
            assert!(first.is_some());
            // …the phone answers it with call_end and the media loop invalidates
            // (a writer: it waits for every lease still held)…
            let writer = tokio::spawn({
                let g = g.clone();
                async move { g.invalidate().await }
            });
            tokio::task::yield_now().await;
            drop(first);
            // …and attempt 2 asks for the lease again: it finds the call over
            // instead of queueing behind the writer for ever.
            assert!(g.lease_current().await.is_none());
            writer.await.expect("invalidate");
        });
        tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .expect("the AI-fail hangup and the call_end waited on each other");
        assert_eq!(*phase.lock().unwrap(), IvrPhase::Done);
        assert!(!guard.is_current());
        // The call is over: no second decision, no hangup.
        let hung_up = AtomicBool::new(false);
        let flag = &hung_up;
        ai_fail_then_hang_up("pixel-1", &phase, &slot, &guard, move || async move {
            flag.store(true, Ordering::Relaxed);
        })
        .await;
        assert!(!hung_up.load(Ordering::Relaxed));
    }

    #[test]
    fn blocked_number_answer_plan() {
        assert_eq!(
            reject_answer_plan(true),
            json!({"type": "answer_plan", "delay_ms": 0, "prewarm": false, "reject": true})
        );
        // `announce`, and what an app that does not know `reject` does: pick up at once.
        assert_eq!(
            reject_answer_plan(false),
            json!({"type": "answer_plan", "delay_ms": 0, "prewarm": false})
        );
        // A declined ring tells a later media hello (an app that picked up anyway)
        // which record goes on.
        let p = RejectPending::default();
        p.mark_ring(
            "pixel-10133",
            RejectRing {
                caller: "0912345678".into(),
                declined: true,
                record_id: Some("rec-1".into()),
            },
        );
        let ring = p.take("pixel-10133").expect("marked");
        assert!(ring.declined);
        assert_eq!(ring.record_id.as_deref(), Some("rec-1"));
    }

    #[test]
    fn sim_sub_id_does_not_outlive_its_call() {
        let serial = "pixel-sub-id-test";
        note_call_sim(serial, &json!({"type": "incoming", "sim_country": "tw", "sim_sub_id": 3}));
        assert_eq!(call_sim_sub_id(serial), Some(3));
        assert_eq!(call_sim_country(serial).as_deref(), Some("tw"));
        note_call_sim(serial, &json!({"type": "hello", "sim_sub_id": 4}));
        assert_eq!(call_sim_sub_id(serial), Some(4));
        // An app that does not send it (or a new call without it): unknown.
        note_call_sim(serial, &json!({"type": "incoming"}));
        assert_eq!(call_sim_sub_id(serial), None);
        assert_eq!(sim_sub_id_from_msg(&json!({"sim_sub_id": -1})), None);
        assert_eq!(sim_sub_id_from_msg(&json!({"sim_sub_id": "3"})), None);
        assert_eq!(sim_sub_id_from_msg(&json!({"sim_sub_id": 0})), Some(0));
    }

    #[test]
    fn a_turned_away_call_reported_twice_is_one() {
        assert!(first_call_waiting_report("pixel-cw-1", "+12025550143"));
        assert!(!first_call_waiting_report("pixel-cw-1", " +12025550143 "));
        assert!(first_call_waiting_report("pixel-cw-1", "+81312345678"));
        assert!(first_call_waiting_report("pixel-cw-2", "+12025550143"));
    }

    #[tokio::test]
    async fn blocked_numbers_are_declined_only_by_apps_that_can() {
        let mut rt = RuntimeSettings::from_base(&Settings::from_env());
        rt.reject_mode = "decline".into();
        let control = ControlRegistry::default();
        // An older app (no `supports_reject` in its hello) would pick up anyway: it
        // is told to, and the hello plays and notifies the announcement.
        let old_app = blocked_number_declined(&rt, &control, "pixel-10341").await;
        assert!(!old_app);
        assert!(reject_answer_plan(old_app).get("reject").is_none());
        // An app that says it can: refused while ringing.
        control.set_supports_reject("pixel-10342", true).await;
        let new_app = blocked_number_declined(&rt, &control, "pixel-10342").await;
        assert!(new_app);
        assert_eq!(reject_answer_plan(new_app)["reject"], true);
        // `announce`: every phone picks up.
        rt.reject_mode = "announce".into();
        assert!(!blocked_number_declined(&rt, &control, "pixel-10342").await);
    }

    #[test]
    fn a_left_over_blocked_ring_never_turns_the_next_caller_away() {
        let p = RejectPending::default();
        let declined = |caller: &str| RejectRing {
            caller: caller.into(),
            declined: true,
            record_id: Some("rec-1".into()),
        };
        // A blocked number was declined and its incoming_cancel never came. The
        // next, ordinary call's incoming drops the entry (what the control
        // connection does first), so its hello — even before the CID is out —
        // does not take the blocked call's record and announcement.
        p.mark_ring("pixel-10341", declined("0912345678"));
        assert!(p.take("pixel-10341").is_some());
        assert_eq!(p.take_for_hello("pixel-10341", ""), None);
        // Its incoming lost too: a hello with another number still is not that call.
        p.mark_ring("pixel-10341", declined("0912345678"));
        assert_eq!(p.take_for_hello("pixel-10341", "0922222222"), None);
        assert_eq!(p.take("pixel-10341"), None);
        // The blocked call's own hello (an app that picked up): the same number in
        // another form, or none yet.
        p.mark_ring("pixel-10341", declined("0912345678"));
        assert!(p.take_for_hello("pixel-10341", "+886912345678").is_some());
        p.mark("pixel-10341", "0912345678");
        assert!(p.take_for_hello("pixel-10341", " ").is_some());
    }

    #[test]
    fn caller_side_text_leaves_the_ai_out() {
        let dialogue = "接聽：喂？\n對方：請回電給我\n接聽：好的，您的電話是？\n對方：零九三三";
        assert_eq!(caller_side_text(dialogue, "whisper"), "請回電給我\n零九三三");
        // Without a Live dialogue: the caller's own track.
        assert_eq!(caller_side_text("", " 請回電 "), "請回電");
        assert_eq!(caller_side_text("接聽：喂？", ""), "");
    }
}

#[cfg(test)]
mod review_t4_tests {
    use super::*;

    /// The AI-fail hangup lets go of the lease before it hangs up. If the phone's
    /// call_end and the next call's hello land in that gap, the first call's
    /// guarded sends must find their call over and never reach the next call.
    #[tokio::test]
    async fn ai_fail_hangup_after_call_end_never_reaches_the_next_call() {
        let first = MediaCallGuard::inactive().next().await;
        let phase = Arc::new(std::sync::Mutex::new(IvrPhase::LiveAi));
        let slot: ActiveCallSlot = Arc::new(Mutex::new(None));
        let next_call: Arc<std::sync::Mutex<Option<MediaCallGuard>>> =
            Arc::new(std::sync::Mutex::new(None));
        let f = &first;
        let nc = next_call.clone();
        let run = ai_fail_then_hang_up("pixel-1", &phase, &slot, &first, move || async move {
            // No lease is held here: call_end (a writer) and the next hello get through.
            f.invalidate().await;
            let second = f.next().await;
            // What every guarded send of the first call (send_gemini_media_json) asks.
            assert!(f.lease_current().await.is_none());
            assert!(!f.is_current());
            assert!(second.is_current());
            *nc.lock().unwrap() = Some(second);
        });
        tokio::time::timeout(Duration::from_secs(1), run)
            .await
            .expect("hangup closure ran with the lease still held");
        let second = next_call.lock().unwrap().clone().expect("next hello");
        assert_eq!(*phase.lock().unwrap(), IvrPhase::Done);
        // A late AI failure of the first call does nothing to the second one.
        let phase2 = Arc::new(std::sync::Mutex::new(IvrPhase::LiveAi));
        let hung = AtomicBool::new(false);
        let h = &hung;
        ai_fail_then_hang_up("pixel-1", &phase2, &slot, &first, move || async move {
            h.store(true, Ordering::Relaxed);
        })
        .await;
        assert!(!hung.load(Ordering::Relaxed));
        assert_eq!(*phase2.lock().unwrap(), IvrPhase::LiveAi);
        assert!(second.is_current());
    }

    /// The control and the media connection both report the turned-away call;
    /// the work-profile twin's copy is never the owner's.
    #[test]
    fn call_waiting_report_dedup_across_connections() {
        assert!(first_call_waiting_report("pixel-20341", "0912000111"));
        assert!(!first_call_waiting_report(" pixel-20341 ", "0912000111"));
        // Another phone, same number: its own call.
        assert!(first_call_waiting_report("pixel-20342", "0912000111"));
        // Hidden number reported twice within the window: one call.
        assert!(first_call_waiting_report("pixel-20343", ""));
        assert!(!first_call_waiting_report("pixel-20343", "  "));
        assert!(is_isolated_screening_serial("pixel-1010341"));
    }

    /// `reject_mode` decline sends `delay_ms` 0: an app that ignores `reject`
    /// picks up at once and the hello plays the announcement.
    #[test]
    fn decline_plan_still_picks_up_at_once_on_old_apps() {
        let plan = reject_answer_plan(true);
        assert_eq!(plan["delay_ms"], 0);
        assert_eq!(plan["prewarm"], false);
        assert_eq!(plan["reject"], true);
    }
}

#[cfg(test)]
mod access_and_whitelist_tests {
    use super::*;

    /// The phone bridges on the LAN and the USB tunnel connect as before; a
    /// connection through a reverse proxy on this machine is not let in for
    /// coming from loopback.
    #[test]
    fn a_phone_through_a_reverse_proxy_needs_the_device_token() {
        let lo: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let lan: SocketAddr = "192.168.1.20:50000".parse().unwrap();
        assert!(phone_allowed(&lan, false, None, ""), "a LAN bridge without DEVICE_TOKEN set");
        assert!(phone_allowed(&lo, false, None, ""));
        assert!(phone_allowed(&lo, false, None, "dev-test"), "the USB tunnel");
        assert!(!phone_allowed(&lan, false, None, "dev-test"));
        assert!(phone_allowed(&lan, false, Some("dev-test"), "dev-test"));
        assert!(!phone_allowed(&lo, true, None, ""), "proxied, no token set");
        assert!(!phone_allowed(&lo, true, None, "dev-test"));
        assert!(!phone_allowed(&lo, true, Some("wrong"), "dev-test"));
        assert!(phone_allowed(&lo, true, Some("dev-test"), "dev-test"));
    }

    /// The Wi-Fi entry: no pairing key, no entry, even from this computer
    /// and with no `DEVICE_TOKEN` exception.
    #[tokio::test]
    async fn the_wifi_entry_refuses_phones_without_the_pairing_key() {
        let key = phone_wifi::pairing().key();
        let app = wifi_entry_gate(Router::new().route("/ws", get(|| async { "in" })), "dev-test-token".into());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        let http = reqwest::Client::new();
        let status = |query: String| {
            let http = http.clone();
            async move { http.get(format!("http://{addr}/ws{query}")).send().await.unwrap().status().as_u16() }
        };
        assert_eq!(status(String::new()).await, 401, "no token");
        assert_eq!(status("?token=".into()).await, 401, "empty token");
        assert_eq!(status("?token=wrong".into()).await, 401, "wrong token");
        assert_eq!(status(format!("?token={}", &key[..20])).await, 401, "part of the key");
        assert_eq!(status(format!("?token={key}")).await, 200, "the pairing key");
        assert_eq!(status("?token=dev-test-token".into()).await, 200, "DEVICE_TOKEN");

        let open = wifi_entry_gate(Router::new().route("/ws", get(|| async { "in" })), String::new());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, open.into_make_service_with_connect_info::<SocketAddr>()).await;
        });
        let no_device_token = http.get(format!("http://{addr}/ws")).send().await.unwrap().status().as_u16();
        assert_eq!(no_device_token, 401, "no DEVICE_TOKEN set is no way in");
    }

    /// Pairing by IP address: the code gives the pairing key once, and that
    /// key opens the phone entry (the Wi-Fi entry's gate, as on the main port).
    #[tokio::test]
    async fn a_pairing_code_gives_the_key_that_opens_the_phone_entry() {
        let dir = std::env::temp_dir().join(format!("bageo-claim-{}", uuid::Uuid::new_v4().simple()));
        let pairing = phone_wifi::Pairing::open(&dir);
        let codes = phone_wifi::PairCodes::new();
        let mut json_headers = axum::http::HeaderMap::new();
        json_headers.insert(axum::http::header::CONTENT_TYPE, "application/json".parse().unwrap());
        let answer = |res: axum::response::Response| async move {
            let status = res.status().as_u16();
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(res.into_body(), 4096).await.unwrap()).unwrap();
            (status, body)
        };

        let (s, b) = answer(claim_answer(true, &json_headers, br#"{"code":"123456"}"#, &codes, &pairing, None)).await;
        assert_eq!((s, b["code"].as_str()), (403, Some("no_code")), "no code made yet");
        let code = codes.issue(std::time::Instant::now());
        let body = format!(r#"{{"code":"{code}","name":"Pixel"}}"#);
        let (s, b) = answer(claim_answer(true, &axum::http::HeaderMap::new(), body.as_bytes(), &codes, &pairing, None)).await;
        assert_eq!((s, b["code"].as_str()), (400, Some("bad_request")), "no JSON header");
        let (s, b) = answer(claim_answer(true, &json_headers, b"{}", &codes, &pairing, None)).await;
        assert_eq!((s, b["code"].as_str()), (400, Some("bad_request")), "no code sent");
        let (s, b) = answer(claim_answer(false, &json_headers, body.as_bytes(), &codes, &pairing, None)).await;
        assert_eq!((s, b["code"].as_str()), (409, Some("phone_wifi_off")));

        let (s, b) = answer(claim_answer(true, &json_headers, body.as_bytes(), &codes, &pairing, Some(8788))).await;
        assert_eq!(s, 200);
        let token = b["token"].as_str().unwrap().to_string();
        assert_eq!(token, pairing.key(), "the pairing key");
        assert_eq!(b["path"], "/ws");
        assert_eq!(b["ws_port"], 8788);
        assert!(pairing.accepts(Some(&token)));
        assert!(phone_wifi::entry_allows(Some(&token), "", &pairing), "it opens the phone entry");
        let (s, b) = answer(claim_answer(true, &json_headers, body.as_bytes(), &codes, &pairing, None)).await;
        assert_eq!((s, b["code"].as_str()), (403, Some("no_code")), "used once, then gone");

        let code = codes.issue(std::time::Instant::now());
        let wrong = if code == "000000" { "000001" } else { "000000" };
        let wrong_body = format!(r#"{{"code":"{wrong}"}}"#);
        for _ in 0..phone_wifi::CODE_MAX_WRONG {
            let (s, b) = answer(claim_answer(true, &json_headers, wrong_body.as_bytes(), &codes, &pairing, None)).await;
            assert_eq!((s, b["code"].as_str()), (403, Some("wrong_code")));
        }
        let right_body = format!(r#"{{"code":"{code}"}}"#);
        let (s, b) = answer(claim_answer(true, &json_headers, right_body.as_bytes(), &codes, &pairing, None)).await;
        assert_eq!((s, b["code"].as_str()), (403, Some("no_code")), "five wrong tries void it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn how_a_phone_came_in() {
        let usb = PhoneLink::main_port(true, false, false, false);
        assert!(!usb.wifi && usb.trusted && !usb.on_pairing, "the USB tunnel");
        let lan_open = PhoneLink::main_port(false, false, false, false);
        assert!(lan_open.wifi && !lan_open.trusted, "a LAN phone without a token gets no pairing key");
        let paired = PhoneLink::main_port(false, false, true, false);
        assert!(paired.wifi && paired.trusted && paired.on_pairing);
        let device = PhoneLink::main_port(false, false, false, true);
        assert!(device.trusted && !device.on_pairing, "DEVICE_TOKEN phones stay when paired again");
        let proxied = PhoneLink::main_port(true, true, false, true);
        assert!(proxied.wifi && proxied.proxied, "a reverse proxy is not the USB tunnel");
        let entry = PhoneLink::wifi_entry(false, true);
        assert!(entry.wifi && entry.trusted && entry.on_pairing);
        let entry_dev = PhoneLink::wifi_entry(false, false);
        assert!(entry_dev.wifi && entry_dev.trusted && !entry_dev.on_pairing, "DEVICE_TOKEN on Wi-Fi entry survives renew");
    }

    #[test]
    fn agent_info_gives_the_wifi_addresses_of_the_entry_in_use() {
        use std::net::{IpAddr, Ipv4Addr};
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        let any: IpAddr = "0.0.0.0".parse().unwrap();
        let lan = |_: IpAddr| Some(Ipv4Addr::new(192, 168, 1, 20));
        let key = || "k3y".to_string();
        // This computer only (the default): the Wi-Fi entry's port, IP first.
        assert_eq!(
            agent_info_wifi_urls(wifi_entry_for(true, false, Some(lo), 8787, Some(8788)), lan, "bageo", key),
            vec!["ws://192.168.1.20:8788/ws?token=k3y", "ws://bageo.local:8788/ws?token=k3y"]
        );
        // Network name off: the IP address only.
        assert_eq!(
            agent_info_wifi_urls(wifi_entry_for(true, false, Some(lo), 8787, Some(8788)), lan, "", key),
            vec!["ws://192.168.1.20:8788/ws?token=k3y"]
        );
        // Open to the LAN: the agent's own port.
        assert_eq!(
            agent_info_wifi_urls(wifi_entry_for(true, false, Some(any), 8787, None), lan, "front-desk", key),
            vec!["ws://192.168.1.20:8787/ws?token=k3y", "ws://front-desk.local:8787/ws?token=k3y"]
        );
        // AGENT_HOST set to one LAN address (a VPN on this computer): that address.
        let one: IpAddr = "192.0.2.7".parse().unwrap();
        assert_eq!(
            agent_info_wifi_urls(wifi_entry_for(true, false, Some(one), 8787, None), mdns::answer_ip, "", key),
            vec!["ws://192.0.2.7:8787/ws?token=k3y"]
        );
        // Empty (the phone forgets its addresses): phone Wi-Fi off, in a
        // container, the Wi-Fi entry not running, no LAN address.
        let none = || -> String { panic!("no key is made when there is nothing to send") };
        assert!(agent_info_wifi_urls(wifi_entry_for(false, false, Some(lo), 8787, Some(8788)), lan, "bageo", none).is_empty());
        assert!(agent_info_wifi_urls(wifi_entry_for(true, true, Some(any), 8787, None), lan, "bageo", none).is_empty());
        assert!(agent_info_wifi_urls(wifi_entry_for(true, false, Some(lo), 8787, None), lan, "bageo", none).is_empty());
        assert!(agent_info_wifi_urls(wifi_entry_for(true, false, Some(lo), 8787, Some(8788)), |_| None, "bageo", none).is_empty());
    }

    #[test]
    fn the_token_of_a_phone_address() {
        assert_eq!(token_param(Some("token=abc-_1")).as_deref(), Some("abc-_1"));
        assert_eq!(token_param(Some("a=1&token=x%2By")).as_deref(), Some("x+y"));
        assert_eq!(token_param(Some("a=1")), None);
        assert_eq!(token_param(None), None);
    }

    #[test]
    fn a_whitelisted_caller_is_declined_only_when_the_transfer_can_work() {
        assert!(!whitelist_direct_ready("", true), "no number to forward to");
        assert!(!whitelist_direct_ready("  ", true));
        assert!(!whitelist_direct_ready("+886900000000", false), "the phone cannot decline");
        assert!(whitelist_direct_ready("+886900000000", true));
    }

    /// The caller hung up, the caller is still on hold, or the AI's session is
    /// gone: the phone's next call goes to the AI, not ignored for three hours.
    #[test]
    fn an_urgent_call_that_ends_early_frees_the_phone() {
        for reason in ["call_ended", "unhold_failed", "no_result", "send_failed"] {
            let phone = format!("test-phone-urgent-{reason}");
            urgent_call::mark_owner_on_call(&phone);
            settle_owner_mark(&phone, Some(reason));
            assert!(!urgent_call::owner_on_call(&phone), "{reason}");
        }
        let phone = "test-phone-urgent-merged";
        urgent_call::mark_owner_on_call(phone);
        settle_owner_mark(phone, None);
        assert!(urgent_call::owner_on_call(phone), "merged: the owner is on the call");
        urgent_call::clear_owner_on_call(phone);
    }
}

#[cfg(test)]
mod audit_a_tests {
    use super::*;

    fn unique_tmp(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("bageo-audit-a-{name}-{}-{nanos}.json", std::process::id()))
    }

    #[tokio::test]
    async fn p0_4_outbound_hello_bypasses_reject_list_and_ring_prewarm() {
        let numbers = [
            "+886900111222",
            "+81300001111",
            "+493000001111",
            "+33100001111",
            "+82200001111",
            "+12025550101",
            "+5511900001111",
            "+34910001111",
            "",
        ];
        for caller in numbers {
            let mut reject_checked = false;
            let plan = plan_hello_routing("pixel-out-1", caller, false, true, || {
                reject_checked = true;
                true
            });
            assert!(!reject_checked, "outbound hello must not consult reject list for {caller:?}");
            assert_eq!(plan.route, CidRoute::Greet, "{caller:?}");
            assert!(!plan.reject_listed, "{caller:?}");
            assert!(!plan.allow_ring_prewarm, "{caller:?}");

            let isolated = plan_hello_routing("pixel-1010123", caller, false, true, || true);
            assert_eq!(isolated.route, CidRoute::IgnoreIsolated);
            assert!(!isolated.allow_ring_prewarm);
        }

        let incoming_rejected = plan_hello_routing("pixel-out-1", "+81300001111", true, false, || true);
        assert_eq!(incoming_rejected.route, CidRoute::RejectList);
        assert!(!incoming_rejected.allow_ring_prewarm);

        let incoming_ok = plan_hello_routing("pixel-out-1", "+493000001111", false, false, || false);
        assert_eq!(incoming_ok.route, CidRoute::Greet);
        assert!(incoming_ok.allow_ring_prewarm);

        // Verify ControlRegistry active callback session matches outbound hello.
        let control = callback::ControlRegistry::new();
        let (sink, _sock) = callback::test_socket::phone_socket().await;
        control.register_client("pixel-cb-match".into(), sink).await;
        let errands_path = unique_tmp("errands");
        let sims_path = unique_tmp("sims");
        let sims = errands::SimBook::open(sims_path.clone());
        let errands = errands::ErrandStore::open(errands_path.clone(), sims);
        let dispatched = control
            .try_dispatch_phone_callback(
                "task-out-match",
                "+81300001111",
                "pixel-cb-match",
                &["+81300001111".to_string()],
            )
            .await
            .unwrap();
        assert!(dispatched);
        assert!(
            hello_is_outbound_with(
                &control,
                &errands,
                "pixel-cb-match",
                "+81300001111",
                &json!({"type": "hello", "task_id": "task-out-match"}),
            )
            .await
        );
        assert!(
            !hello_is_outbound_with(
                &control,
                &errands,
                "pixel-other-phone",
                "+81300001111",
                &json!({"type": "hello", "caller_id": "+81300001111"}),
            )
            .await
        );
        let _ = std::fs::remove_file(&errands_path);
        let _ = std::fs::remove_file(&sims_path);
    }

    #[tokio::test]
    async fn p0_5_whitelist_urgent_and_emergency_not_dropped_by_reject_list() {
        let path = unique_tmp("contacts-p05");
        let store = contacts::ContactStore::open(path.clone());
        store
            .add(&contacts::Draft {
                name: "田中花子 / Clínica Sol / Alice".into(),
                phones: vec!["0800123456".into(), "+81312340009".into(), "+493012340009".into()],
                note: String::new(),
                source: "test".into(),
                kind: "whitelist".into(),
            })
            .await
            .unwrap();

        let mut rt = crate::settings_store::RuntimeSettings::from_base(
            &crate::config::Settings::from_env(),
        );
        rt.country_code = "886".into();
        rt.reject_numbers = vec![
            crate::settings_store::RejectEntry {
                number: "0800*".into(),
                reason: "廣告推銷".into(),
            },
            crate::settings_store::RejectEntry {
                number: "+81312340009".into(),
                reason: "営業".into(),
            },
            crate::settings_store::RejectEntry {
                number: "+493012340009".into(),
                reason: "Werbung".into(),
            },
        ];

        for mode in ["urgent", "emergency", "direct"] {
            rt.whitelist_mode = mode.into();
            assert!(!reject_listed_on_sim(&rt, "0800123456", Some("tw"), Some(&store)), "mode={mode}");
            assert_eq!(reject_reason_on_sim(&rt, "0800123456", Some("tw"), Some(&store)), None, "mode={mode}");
            assert!(!reject_listed_on_sim(&rt, "03-1234-0009", Some("jp"), Some(&store)), "mode={mode}");
            assert!(!reject_listed_on_sim(&rt, "03012340009", Some("de"), Some(&store)), "mode={mode}");
        }

        // Non-whitelisted prefix match is still blocked.
        rt.whitelist_mode = "urgent".into();
        assert!(reject_listed_on_sim(&rt, "0800999888", Some("tw"), Some(&store)));
        assert_eq!(
            reject_reason_on_sim(&rt, "0800999888", Some("tw"), Some(&store)).as_deref(),
            Some("廣告推銷")
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn p0_7_unhold_failed_session_gone_and_late_merge_coordination() {
        // 1. unhold_failed stops audio tasks (`cancel`) and suppresses Alert::AiEnded.
        let (sink, _sock) = callback::test_socket::phone_socket().await;
        let guard = MediaCallGuard::inactive().next().await;
        let ctl_hold = urgent_call::CallCtl::default();
        ctl_hold.pause();
        let cancel_hold = AtomicBool::new(false);
        let coord_hold = UrgentCallCoord::new();
        let disp = handle_urgent_call_failure_disposition(
            &sink,
            "pixel-p07-1",
            "unhold_failed",
            &ctl_hold,
            &cancel_hold,
            &coord_hold,
            &guard,
        )
        .await;
        assert_eq!(disp, UrgentFailureDisposition::StillOnHold);
        assert!(cancel_hold.load(Ordering::Relaxed));
        assert_eq!(coord_hold.outcome(), UrgentCallOutcome::StillOnHold);
        assert_eq!(
            finish_urgent_session_end(Some(&ctl_hold), &coord_hold, Duration::from_millis(50)).await,
            UrgentEndAction::StillOnHold
        );
        assert!(!ctl_hold.session_gone());

        // 2. ctl.session_gone() on failure (`reason != "call_ended"`) sends hangup JSON.
        let (sink2, mut sock2) = callback::test_socket::phone_socket().await;
        let ctl_gone = urgent_call::CallCtl::default();
        ctl_gone.pause();
        ctl_gone.mark_session_gone();
        let cancel_gone = AtomicBool::new(false);
        let coord_gone = UrgentCallCoord::new();
        let disp_gone = handle_urgent_call_failure_disposition(
            &sink2,
            "pixel-p07-2",
            "dial_failed",
            &ctl_gone,
            &cancel_gone,
            &coord_gone,
            &guard,
        )
        .await;
        assert_eq!(disp_gone, UrgentFailureDisposition::SessionGoneHangup);
        assert!(cancel_gone.load(Ordering::Relaxed));
        let frame = callback::test_socket::next_json(&mut sock2, Duration::from_millis(500))
            .await
            .expect("hangup frame sent when session_gone on failure");
        assert_eq!(frame["type"], "hangup");
        assert_eq!(frame["reason"], "urgent_call_ai_gone");

        // 3. call_ended must NOT send extra hangup.
        let (sink3, mut sock3) = callback::test_socket::phone_socket().await;
        let cancel_ended = AtomicBool::new(false);
        let coord_ended = UrgentCallCoord::new();
        let disp_ended = handle_urgent_call_failure_disposition(
            &sink3,
            "pixel-p07-3",
            "call_ended",
            &ctl_gone,
            &cancel_ended,
            &coord_ended,
            &guard,
        )
        .await;
        assert_eq!(disp_ended, UrgentFailureDisposition::ResumeAi);
        assert!(callback::test_socket::next_json(&mut sock3, Duration::from_millis(60)).await.is_none());

        // 4. Late Report::Merged completing concurrently with Gemini WS close suppresses false Alert::AiEnded.
        let ctl_merge = Arc::new(urgent_call::CallCtl::default());
        ctl_merge.pause();
        let coord_merge = UrgentCallCoord::new();
        let ctl_spawn = ctl_merge.clone();
        let coord_spawn = coord_merge.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(35)).await;
            ctl_spawn.hand_over();
            coord_spawn.set(UrgentCallOutcome::Merged);
        });
        let action = finish_urgent_session_end(
            Some(ctl_merge.as_ref()),
            &coord_merge,
            URGENT_END_MERGE_GRACE,
        )
        .await;
        assert_eq!(action, UrgentEndAction::Handed);
        assert!(!ctl_merge.session_gone());

        // 5. Real unexpected Gemini end without merge still marks session_gone and triggers Alert::AiEnded.
        let ctl_alert = urgent_call::CallCtl::default();
        ctl_alert.pause();
        let coord_alert = UrgentCallCoord::new();
        let action_alert = finish_urgent_session_end(
            Some(&ctl_alert),
            &coord_alert,
            Duration::from_millis(30),
        )
        .await;
        assert_eq!(action_alert, UrgentEndAction::AlertAiEnded);
        assert!(ctl_alert.session_gone());
    }

    #[test]
    fn p1_9_early_callback_teardown_and_reconnect_sim_preservation() {
        // 1. Callback dropped during 400ms settle (`was_ai_talk == false`) still runs summary/completion.
        assert!(!should_run_ai_call_summary(false, &[]));
        assert!(should_run_ai_call_summary(true, &[]));
        assert!(should_run_ai_call_summary(false, &["persona_ai_fail_hangup".to_string()]));
        assert!(should_run_ai_call_summary(
            false,
            &[appointments::AI_CALLBACK_EVENT.to_string()],
        ));

        // 2. Reconnect / status / hello omitting SIM fields must preserve cached SIM country & sub_id.
        let serial = "TESTP19SIM01";
        clear_call_sim(serial);
        note_call_sim(
            serial,
            &json!({"type": "incoming", "sim_country": "jp", "sim_sub_id": 2}),
        );
        assert_eq!(call_sim_country(serial).as_deref(), Some("jp"));
        assert_eq!(call_sim_sub_id(serial), Some(2));

        // Reconnect status/hello missing SIM fields -> preserve existing values (no WrongSim wipe).
        note_call_sim(serial, &json!({"type": "status"}));
        assert_eq!(call_sim_country(serial).as_deref(), Some("jp"));
        assert_eq!(call_sim_sub_id(serial), Some(2));

        note_call_sim(serial, &json!({"type": "hello", "caller_id": "+81300001111"}));
        assert_eq!(call_sim_country(serial).as_deref(), Some("jp"));
        assert_eq!(call_sim_sub_id(serial), Some(2));

        // call_end clears cached SIM.
        clear_call_sim(serial);
        assert_eq!(call_sim_country(serial), None);
        assert_eq!(call_sim_sub_id(serial), None);
    }

    #[tokio::test]
    async fn phone_allowed_and_media_play_gate_behave_safely() {
        let lan: SocketAddr = "203.0.113.10:1234".parse().unwrap();
        let lo: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert!(phone_allowed(&lan, false, Some("dev-tok-1"), "dev-tok-1"));
        assert!(!phone_allowed(&lan, false, Some("dev-tok-2"), "dev-tok-1"));
        assert!(phone_allowed(&lo, true, Some("dev-tok-1"), "dev-tok-1"));
        assert!(!phone_allowed(&lo, true, None, ""));
        assert_eq!(urgent_call::masked("0912345678"), "***5678");

        let gate = MediaPlayGate::default();
        let c1 = Arc::new(AtomicBool::new(false));
        let c2 = Arc::new(AtomicBool::new(false));
        assert!(gate.claim("pixel-10111", MediaPlayKind::Greet, &c1).await);
        assert!(!gate.claim("pixel-1010111", MediaPlayKind::Greet, &c2).await);
        gate.release("pixel-10111").await;
        assert!(gate.claim("pixel-1010111", MediaPlayKind::Greet, &c2).await);
    }
}

#[cfg(test)]
mod health_access_tests {
    use super::*;
    use axum::extract::connect_info::MockConnectInfo;

    /// The agent's real routes (`build_app`) over a temp data folder, answering
    /// as if every request came from `peer`. Returns the base URL and the folder.
    async fn serve(password: &str, peer: &str) -> (String, PathBuf) {
        serve_as(password, peer, None).await
    }

    /// [`serve`] in the automatic mode (`lan::guard`), the connection reaching
    /// this computer at `local`.
    async fn serve_auto(password: &str, peer: &str, local: &str) -> (String, PathBuf) {
        serve_as(password, peer, Some(local)).await
    }

    async fn serve_as(password: &str, peer: &str, auto_local: Option<&str>) -> (String, PathBuf) {
        let root = std::env::temp_dir().join(format!("bageo-health-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = root.to_string_lossy().into_owned();
        base.calls_ui_token = password.to_string();
        base.telegram_bot_token = String::new();
        base.telegram_chat_id = String::new();
        let settings_store = SettingsStore::load(base.clone());
        let calls = CallStore::new(root.join("calls"), true, false);
        let task_store = TaskStore::new(root.join("tasks"));
        let control = ControlRegistry::new();
        let phones = calls_http::PhoneDevices::new();
        let http = reqwest::Client::new();
        let ambience = Arc::new(ambience::AmbienceLibrary::load(&root, 16000));
        let previews = Arc::new(voice_preview::VoicePreviews::new(root.join("voice_preview"), root.join("voices")));
        let errands = errands::ErrandStore::open(root.join("errands.json"), errands::SimBook::in_memory());
        let calls_state = CallsState {
            settings: base.clone(),
            store: calls.clone(),
            settings_store: settings_store.clone(),
            task_store: task_store.clone(),
            http: http.clone(),
            control: control.clone(),
            ambience: ambience.clone(),
            previews: previews.clone(),
            phones: phones.clone(),
            contacts: contacts::ContactStore::open(root.join("contacts.json")),
            errands: errands.clone(),
            sms: sms_store::SmsStore::open(root.join("sms"), sms_store::now_ms(), 15),
        };
        let state = AppState {
            settings: base,
            settings_store,
            http,
            hold_pcm: None,
            ivr_prompt_segments: Arc::new(Vec::new()),
            ivr_fail_pcm: None,
            ivr_goodbye_pcm: None,
            ivr_reject_unknown_pcm: None,
            ivr_dtmf1_wait_pcm: None,
            ivr_ai_hello_pcm: None,
            ivr_ai_nudge_pcm: None,
            ivr_vm_intro_pcm: None,
            ivr_vm_confirm_pcm: None,
            ivr_menu_full_pcm: Arc::new(Vec::new()),
            vm_beep_pcm: Arc::new(Vec::new()),
            latency_build: "test",
            calls,
            task_store,
            control,
            phones,
            trtc_talk: Arc::new(TrtcTalkMetrics::default()),
            incoming_notify_dedup: Arc::new(IncomingNotifyDedup::default()),
            call_reattach: Arc::new(CallReattachRegistry::default()),
            media_play: Arc::new(MediaPlayGate::default()),
            ring_prewarm: Arc::new(RingPrewarmRegistry::default()),
            reject_pending: Arc::new(RejectPending::default()),
            previews,
            ambience,
            errands,
        };
        let remote = control::Controller::new(calls_state.clone());
        let peer: SocketAddr = peer.parse().unwrap();
        let guard_settings = state.settings.clone();
        let guard_store = state.settings_store.clone();
        let app = build_app(state, calls_state, remote);
        let app = match auto_local {
            Some(local) => lan::guard(app, guard_settings, guard_store)
                .layer(MockConnectInfo(lan::ConnAddrs { peer, local: Some(local.parse().unwrap()) })),
            None => app.layer(MockConnectInfo(peer)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        (format!("http://127.0.0.1:{port}"), root)
    }

    /// GET `path` with optional extra headers; the status and the JSON body
    /// (`null` when the answer is not JSON, like the console page).
    async fn get_json(base: &str, path: &str, headers: &[(&str, &str)]) -> (u16, Value) {
        let mut req = reqwest::Client::new().get(format!("{base}{path}"));
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let body = res.json::<Value>().await.unwrap_or(Value::Null);
        (status, body)
    }

    const FULL_ONLY: [&str; 4] = ["active_calls", "device_labels", "calls_data_dir", "control_idle_serials"];

    fn assert_brief(body: &Value) {
        for key in ["ok", "version", "language", "notify_language", "local_time", "active_call_count", "control_idle_clients", "setup_missing"] {
            assert!(body.get(key).is_some(), "short health lacks {key}");
        }
        assert_eq!(body.as_object().unwrap().len(), 8, "short health has other keys: {body}");
        for key in FULL_ONLY {
            assert!(body.get(key).is_none(), "short health leaks {key}");
        }
    }

    fn assert_full(body: &Value) {
        for key in FULL_ONLY {
            assert!(body.get(key).is_some(), "full health lacks {key}");
        }
        assert_eq!(body["ok"], json!(true));
        assert!(body.get("version").is_some() && body.get("setup_missing").is_some());
    }

    #[test]
    fn health_full_allowed_covers_every_combination() {
        for password_set in [false, true] {
            for token_ok in [false, true] {
                for peer_loopback in [false, true] {
                    for proxied in [false, true] {
                        let want = if password_set { token_ok } else { peer_loopback && !proxied };
                        assert_eq!(
                            health_full_allowed(password_set, token_ok, peer_loopback, proxied),
                            want,
                            "password_set={password_set} token_ok={token_ok} loopback={peer_loopback} proxied={proxied}"
                        );
                    }
                }
            }
        }
        // The cases that matter, spelled out.
        assert!(!health_full_allowed(false, true, false, false), "no password, another computer");
        assert!(health_full_allowed(false, true, true, false), "no password, this computer");
        assert!(!health_full_allowed(false, true, true, true), "no password, through a proxy");
        assert!(health_full_allowed(true, true, false, true), "right password from anywhere");
        assert!(!health_full_allowed(true, false, true, false), "password set, none sent, this computer");
    }

    #[tokio::test]
    async fn no_password_other_computer_gets_short_health_and_console_gets_full() {
        let (base, root) = serve("", "203.0.113.9:5000").await;
        let (status, body) = get_json(&base, "/health", &[]).await;
        assert_eq!(status, 200);
        assert_brief(&body);
        // A forwarding header or a query token changes nothing for this caller.
        let (_, body) = get_json(&base, "/health?token=whatever", &[("x-forwarded-for", "198.51.100.7")]).await;
        assert_brief(&body);

        let (status, body) = get_json(&base, "/calls/api/health", &[]).await;
        assert_eq!(status, 200);
        assert_full(&body);
        // The console's own routes still answer (the new route did not shadow the nest).
        assert_eq!(get_json(&base, "/calls/api/devices", &[]).await.0, 200);
        assert_eq!(get_json(&base, "/calls/api/auth-status", &[]).await.0, 200);
        let page = reqwest::get(format!("{base}/calls/")).await.unwrap();
        assert_eq!(page.status().as_u16(), 200);
        assert!(page.text().await.unwrap().contains("<html"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn no_password_this_computer_gets_full_health_unless_proxied() {
        for peer in ["127.0.0.1:5000", "[::ffff:127.0.0.1]:5000", "[::1]:5000"] {
            let (base, root) = serve("", peer).await;
            let (status, body) = get_json(&base, "/health", &[]).await;
            assert_eq!(status, 200, "{peer}");
            assert_full(&body);
            for header in ["x-forwarded-for", "forwarded", "x-real-ip", "cf-connecting-ip"] {
                let (status, body) = get_json(&base, "/health", &[(header, "198.51.100.7")]).await;
                assert_eq!(status, 200, "{peer} {header}");
                assert_brief(&body);
            }
            // The console has no password to check, so it stays open behind a proxy too.
            let (status, body) = get_json(&base, "/calls/api/health", &[("x-forwarded-for", "198.51.100.7")]).await;
            assert_eq!(status, 200, "{peer}");
            assert_full(&body);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[tokio::test]
    async fn with_password_full_health_needs_the_password() {
        for peer in ["203.0.113.9:5000", "127.0.0.1:5000"] {
            let (base, root) = serve("health-test-pw", peer).await;
            let (status, body) = get_json(&base, "/health", &[]).await;
            assert_eq!(status, 200, "{peer}");
            assert_brief(&body);
            let (_, body) = get_json(&base, "/health?token=wrong-pw", &[]).await;
            assert_brief(&body);
            let (_, body) = get_json(&base, "/health?token=health-test-pw", &[]).await;
            assert_full(&body);
            let (_, body) = get_json(&base, "/health", &[("authorization", "Bearer health-test-pw")]).await;
            assert_full(&body);
            let (_, body) = get_json(&base, "/health", &[("x-calls-token", "health-test-pw")]).await;
            assert_full(&body);

            for (path, headers) in [
                ("/calls/api/health", vec![]),
                ("/calls/api/health?token=wrong-pw", vec![]),
                ("/calls/api/health", vec![("authorization", "Bearer wrong-pw")]),
            ] {
                let (status, body) = get_json(&base, path, &headers).await;
                assert_eq!(status, 401, "{peer} {path}");
                assert_eq!(body["code"], json!("unauthorized"));
                assert!(body.get("calls_data_dir").is_none());
            }
            let (status, body) = get_json(&base, "/calls/api/health", &[("authorization", "Bearer health-test-pw")]).await;
            assert_eq!(status, 200);
            assert_full(&body);
            let (status, body) = get_json(&base, "/calls/api/health?token=health-test-pw", &[("x-forwarded-for", "198.51.100.7")]).await;
            assert_eq!(status, 200);
            assert_full(&body);

            // The other console routes keep their own check.
            assert_eq!(get_json(&base, "/calls/api/devices", &[]).await.0, 401);
            assert_eq!(get_json(&base, "/calls/api/devices?token=health-test-pw", &[]).await.0, 200);
            assert_eq!(reqwest::get(format!("{base}/calls/")).await.unwrap().status().as_u16(), 200);
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    /// GET `path` with `Accept-Language`: the status and the text.
    async fn get_text(base: &str, path: &str, accept_language: &str) -> (u16, String) {
        let res = reqwest::Client::new()
            .get(format!("{base}{path}"))
            .header("accept-language", accept_language)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.text().await.unwrap_or_default())
    }

    #[tokio::test]
    async fn automatic_mode_without_password_keeps_other_devices_out() {
        let (base, root) = serve_auto("", "203.0.113.9:5000", "192.168.1.20:8787").await;
        // A browser gets the page in its language; the API gets need_password.
        let (status, page) = get_text(&base, "/calls/", "zh-TW,zh;q=0.9").await;
        assert_eq!(status, 403);
        assert!(page.contains("請先設密碼") && page.contains("lang=\"zh-Hant\""), "{page}");
        let (status, page) = get_text(&base, "/", "de-DE,de;q=0.9").await;
        assert_eq!(status, 403);
        assert!(page.contains("Passwort"), "{page}");
        for path in ["/calls/api/list", "/calls/api/settings", "/calls/api/health", "/calls/api/devices"] {
            let (status, body) = get_json(&base, path, &[]).await;
            assert_eq!(status, 403, "{path}");
            assert_eq!(body["code"], json!("need_password"), "{path}");
        }
        // Nobody else may set the first password.
        let res = reqwest::Client::new()
            .post(format!("{base}/calls/api/initial-password"))
            .json(&json!({"password": "taken-over"}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status().as_u16(), 403);
        // Short health, the notification buttons (signed) and phones with their key only.
        let (status, body) = get_json(&base, "/health", &[]).await;
        assert_eq!(status, 200);
        assert_brief(&body);
        let (status, _) = get_json(&base, "/calls/api/quick-action?op=block&target=0912345678", &[("accept", "application/json")]).await;
        assert_eq!(status, 410, "an unsigned link reaches its own check");
        assert_eq!(get_json(&base, "/ws", &[]).await.0, 401);
        // An .ics link: its own key opens it, another key does not get past the gate.
        let k = calendar::link_key("call/abc");
        let (status, body) = get_json(&base, &format!("/calls/api/call/abc/event.ics?k={k}"), &[]).await;
        assert_ne!(body["code"], json!("need_password"));
        assert_ne!(status, 403);
        let (status, body) = get_json(&base, &format!("/calls/api/call/other/event.ics?k={k}"), &[]).await;
        assert_eq!((status, body["code"].clone()), (403, json!("need_password")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn automatic_mode_lets_this_computer_in_by_its_own_address() {
        for (peer, local) in [("127.0.0.1:5000", "127.0.0.1:8787"), ("192.168.1.20:5000", "192.168.1.20:80")] {
            let (base, root) = serve_auto("", peer, local).await;
            assert_eq!(get_json(&base, "/calls/api/devices", &[]).await.0, 200, "{peer}");
            let (status, body) = get_json(&base, "/health", &[]).await;
            assert_eq!(status, 200);
            assert_full(&body);
            assert_eq!(reqwest::get(format!("{base}/calls/")).await.unwrap().status().as_u16(), 200);
            // Through a proxy on this computer it is another device.
            let (status, body) = get_json(&base, "/calls/api/devices", &[("x-forwarded-for", "198.51.100.7")]).await;
            assert_eq!((status, body["code"].clone()), (403, json!("need_password")), "{peer}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[tokio::test]
    async fn automatic_mode_with_password_other_devices_use_it() {
        let (base, root) = serve_auto("auto-test-pw", "203.0.113.9:5000", "192.168.1.20:8787").await;
        assert_eq!(reqwest::get(format!("{base}/calls/")).await.unwrap().status().as_u16(), 200);
        let (status, body) = get_json(&base, "/calls/api/devices", &[]).await;
        assert_eq!((status, body["code"].clone()), (401, json!("unauthorized")));
        assert_eq!(get_json(&base, "/calls/api/devices?token=auto-test-pw", &[]).await.0, 200);
        let (_, body) = get_json(&base, "/health?token=auto-test-pw", &[]).await;
        assert_full(&body);
        // Phones still need their own key, whatever the console password.
        assert_eq!(get_json(&base, "/ws?token=auto-test-pw", &[]).await.0, 401);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn automatic_mode_refuses_names_that_are_not_this_computers() {
        // From this computer itself, no password: a page pointing its own name
        // here (DNS rebinding) gets nothing; this computer's names do.
        let (base, root) = serve_auto("", "192.168.1.20:5000", "192.168.1.20:8787").await;
        for host in ["evil.example", "evil.example:8787", "bageo.local.evil.example", "0.0.0.0"] {
            let (status, body) = get_json(&base, "/calls/api/settings", &[("host", host)]).await;
            assert_eq!((status, body["code"].clone()), (403, json!("bad_host")), "{host}");
            let (status, _) = get_json(&base, "/calls/api/initial-password", &[("host", host)]).await;
            assert_eq!(status, 403, "{host}");
        }
        let (status, page) = get_text_host(&base, "/calls/", "evil.example", "ja").await;
        assert_eq!(status, 403);
        assert!(page.contains("このアドレスではコンソールを開けません"), "{page}");
        for host in ["127.0.0.1:8787", "localhost", "LOCALHOST.", "[::1]:8787", "192.168.1.20", "bageo.local", "bageo.local:8787"] {
            assert_eq!(get_json(&base, "/calls/api/settings", &[("host", host)]).await.0, 200, "{host}");
        }
        let _ = std::fs::remove_dir_all(&root);
        // Another device with the password: the same names only.
        let (base, root) = serve_auto("auto-host-pw", "203.0.113.30:5000", "192.168.1.20:8787").await;
        let (status, body) = get_json(&base, "/calls/api/settings?token=auto-host-pw", &[("host", "evil.example")]).await;
        assert_eq!((status, body["code"].clone()), (403, json!("bad_host")));
        assert_eq!(get_json(&base, "/calls/api/settings?token=auto-host-pw", &[("host", "bageo.local")]).await.0, 200);
        assert_eq!(get_json(&base, "/calls/api/settings?token=auto-host-pw", &[("host", "192.168.1.20:8787")]).await.0, 200);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// GET `path` naming `host`, with `Accept-Language`: the status and the text.
    async fn get_text_host(base: &str, path: &str, host: &str, accept_language: &str) -> (u16, String) {
        let res = reqwest::Client::new()
            .get(format!("{base}{path}"))
            .header("host", host)
            .header("accept-language", accept_language)
            .send()
            .await
            .unwrap();
        let status = res.status().as_u16();
        (status, res.text().await.unwrap_or_default())
    }

    #[tokio::test]
    async fn wrong_passwords_from_another_device_slow_down_then_wait() {
        let (base, root) = serve("throttle-test-pw", "198.51.100.77:5000").await;
        let started = std::time::Instant::now();
        for n in 1..=lan::FREE_TRIES {
            let (status, _) = get_json(&base, &format!("/calls/api/devices?token=wrong-{n}"), &[]).await;
            assert_eq!(status, 401, "try {n}");
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(900), "the first tries answer at once");
        let slow = std::time::Instant::now();
        for n in lan::FREE_TRIES + 1..=lan::MAX_TRIES {
            let (status, _) = get_json(&base, &format!("/calls/api/devices?token=wrong-{n}"), &[]).await;
            assert_eq!(status, 401, "try {n}");
        }
        assert!(slow.elapsed() >= std::time::Duration::from_secs(u64::from(lan::MAX_TRIES - lan::FREE_TRIES)), "later tries answer late");
        // Now even the right password waits.
        for path in ["/calls/api/devices?token=wrong-again", "/calls/api/devices?token=throttle-test-pw"] {
            let (status, body) = get_json(&base, path, &[]).await;
            assert_eq!((status, body["code"].clone()), (429, json!("too_many_tries")), "{path}");
        }
        // A page without a password still opens.
        assert_eq!(reqwest::get(format!("{base}/calls/")).await.unwrap().status().as_u16(), 200);
        let _ = std::fs::remove_dir_all(&root);

        // The same wrong password over and over (the console's parallel requests) counts once,
        // and a right one starts over.
        let (base, root) = serve("throttle-test-pw", "198.51.100.78:5000").await;
        for _ in 0..15 {
            assert_eq!(get_json(&base, "/calls/api/devices?token=stale-pw", &[]).await.0, 401);
        }
        assert_eq!(get_json(&base, "/calls/api/devices?token=throttle-test-pw", &[]).await.0, 200);
        let _ = std::fs::remove_dir_all(&root);

        // This computer is never held back.
        let (base, root) = serve("throttle-test-pw", "127.0.0.1:5000").await;
        for n in 0..12 {
            assert_eq!(get_json(&base, &format!("/calls/api/devices?token=local-wrong-{n}"), &[]).await.0, 401);
        }
        assert_eq!(get_json(&base, "/calls/api/devices?token=throttle-test-pw", &[]).await.0, 200);
        let _ = std::fs::remove_dir_all(&root);
    }
}

//! Bageo agent.
//!
//! Phones connect by WebSocket (`/` or `/ws`): JSON text frames and raw s16le
//! mono PCM in both directions (the messages are listed in `android/README.md`).
//! Each phone keeps one idle control connection and opens one media connection
//! per answered call.
//!
//! For a call the agent runs a Gemini Live session ([`gemini_live`]), records
//! it ([`calls`]), writes the summary and the calendar suggestion
//! ([`deepseek`], [`calendar`]) and sends the notifications ([`notify`]). The
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
mod calendar;
mod callback;
mod calls;
mod calls_http;
mod config;
mod contacts;
mod debug_probe;
mod deepseek;
mod dtmf;
mod extract;
mod fulfill;
mod gcal;
mod gemini_live;
mod gmail;
mod hold_music;
mod locale;
mod mdns;
mod notify;
mod openclaw;
mod phone_region;
mod prompts;
mod services;
mod settings_store;
mod stall_review;
mod tasks;
mod tg_inbound;
mod trtc;
mod trtc_api;
mod trtc_bridge;
mod usb_bridge;
mod vad;
mod voice_preview;
mod wyoming;
mod zh_tw;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, State};
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
    notify_incoming_call, notify_primary_backend, notify_rejected_call, notify_voicemail,
};
use services::{chat, ensure_ollama_model, synthesize_piper, transcribe_pcm16, ChatMessage};
use settings_store::{RuntimeSettings, SettingsStore};
use tasks::{draft_from_voicemail, ReplyMethod, TaskIntent, TaskPatch, TaskStore};
use tg_inbound::{send_task_card, TgPoller};
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
        } else if e.plan.intl_hold != plan.intl_hold {
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
                    "Ring prewarm caller differs serial={serial} id={} rang_caller={} hello_caller={caller_id} — still the ringing call, adopt",
                    e.id, e.caller_id
                );
            }
            return Some(e);
        };
        e.cancel.store(true, Ordering::Relaxed);
        warn!(
            "Ring prewarm not used serial={serial} id={} why={why} rang_caller={} hello_caller={caller_id} age_ms={}",
            e.id,
            e.caller_id,
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
    if !rt.gemini_persona_mode {
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
            &rt.intl_carrier_prompt,
            rt.lang() == locale::Lang::ZhTw,
            caller_id,
            sim,
        ),
        appointments: rt.is_appointment_test_number(caller_id)
            || appointments::Mode::parse(&rt.appointment_mode) != appointments::Mode::Off,
        contact: contacts::lookup(caller_id, sim).map(|c| c.id),
    }
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
                "拒接號碼 ring left over serial={serial} rang={} hello={hello_caller} — not this call",
                ring.caller
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
        .build()?;
    let calls = CallStore::new(
        PathBuf::from(&settings.calls_data_dir),
        settings.calls_record,
        settings.calls_record_tts,
        settings.calls_retention_days,
    );
    let task_store = TaskStore::new(PathBuf::from(&settings.calls_data_dir).join("tasks"));
    // The owner's contacts (names for caller IDs), next to the call records.
    let contacts = contacts::ContactStore::open(PathBuf::from(&settings.calls_data_dir).join("contacts.json"));
    contacts::install(contacts.clone());
    {
        let calls = calls.clone();
        let task_store = task_store.clone();
        let retention_days = settings.calls_retention_days;
        tokio::spawn(async move {
            calls.purge_expired().await;
            task_store.purge_expired(retention_days).await;
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3600));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                calls.purge_expired().await;
                task_store.purge_expired(retention_days).await;
            }
        });
    }
    let control = ControlRegistry::new();
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
    };
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
    info!(
        "pixel-agent listening on http://{} (ws /ws, ui /calls/) mode={} ivr_prompt={:?} ivr_start_delay_ms={} ivr_segments={:?} ivr_prompt_pcm_bytes={} ivr_fail_pcm_bytes={} ivr_goodbye_pcm_bytes={} ivr_reject_unknown_pcm_bytes={} dtmf_hold={} dtmf_vm={} dtmf_end={:?} ivr_loop={} max_menu_plays={} reprompt_ms={} wait_remote_voice={} hold_music={} hold_pcm_bytes={} calls_dir={} calls_record={} llm={} whisper={} piper={}",
        addr,
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

    // Telegram inbound poller: confirm/ignore task cards (re-reads settings each loop).
    // No spam-blocklist dependency. Backs off on poll errors (avoids tight CPU/API spin).
    {
        let settings_store = settings_store.clone();
        let task_store = task_store.clone();
        let http = http.clone();
        let control = control.clone();
        let calls = state.calls.clone();
        tokio::spawn(async move {
            let mut poller = TgPoller::new();
            loop {
                let rt = settings_store.snapshot();
                let token = rt.telegram_bot_token.trim().to_string();
                let chat = rt.telegram_chat_id.trim().to_string();
                if token.is_empty() || chat.is_empty() {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    continue;
                }
                poller.set_allowed_chat_id(&chat);
                let actions = match poller.poll_once(&http, &token).await {
                    Ok(a) => a,
                    Err(e) => {
                        warn!("telegram poll failed: {e:#}");
                        tokio::time::sleep(Duration::from_secs(5)).await;
                        continue;
                    }
                };
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
                        tokio::spawn(async move {
                            let text = match appointments::decide(
                                &http,
                                &rt,
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
                    if let Err(e) =
                        tg_inbound::send_plain_message(&http, &token, &action.chat_id, &result_text)
                            .await
                    {
                        warn!(
                            "tg fulfill result notify failed task_id={}: {e:#}",
                            action.task_id
                        );
                    }
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
        if settings.calls_ui_token.trim().is_empty() {
            warn!("listening on {addr} without CALLS_UI_TOKEN: anyone on this network can open the web console");
        }
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
            mdns::spawn(addr.ip(), move || store.snapshot().mdns_name.clone());
        }
    }

    let app = Router::new()
        .route("/", get(root))
        .route("/index", get(redirect_to_calls))
        .route("/favicon.ico", get(calls_http::favicon))
        .route("/health", get(health))
        .route("/v1/turn", post(turn))
        .route("/v1/transfer", get(transfer_smoke).post(transfer_smoke))
        .route("/ws", get(ws_upgrade))
        .nest_service("/calls", calls_router(calls_state))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("cannot listen on {addr}: {e} (is another copy running? AGENT_PORT picks another port)"))?;
    info!("web console: http://{}:{}/", if addr.ip().is_unspecified() { "localhost".to_string() } else { addr.ip().to_string() }, addr.port());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

/// `/`: a phone over the USB tunnel opens its WebSocket here (the app's default
/// address has no path); a browser is sent to the web console.
async fn root(
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(q): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> axum::response::Response {
    match ws {
        Ok(ws) => phone_ws(ws, peer, &q, state),
        Err(_) => Redirect::permanent("/calls/").into_response(),
    }
}

/// A phone may connect when it comes through this machine (the USB tunnel ends
/// on loopback), or carries `?token=` matching `DEVICE_TOKEN`. Without a token
/// set, anyone who can reach the port may connect.
fn phone_allowed(peer: &SocketAddr, token: Option<&str>, required: &str) -> bool {
    let required = required.trim();
    peer.ip().is_loopback() || required.is_empty() || token.is_some_and(|t| t == required)
}

fn phone_ws(
    ws: WebSocketUpgrade,
    peer: SocketAddr,
    q: &HashMap<String, String>,
    state: AppState,
) -> axum::response::Response {
    if !phone_allowed(&peer, q.get("token").map(String::as_str), &state.settings.device_token) {
        warn!("phone websocket refused from {peer}: missing or wrong token");
        return (StatusCode::UNAUTHORIZED, "device token required").into_response();
    }
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

/// Permanent redirect to the web console.
async fn redirect_to_calls() -> Redirect {
    Redirect::permanent("/calls/")
}

async fn health(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Json<Value> {
    // With a console token set, anyone else only learns that the agent is up
    // (enough for monitors and the SMS forwarder).
    if !calls_http::token_ok_parts(&state.settings, &headers, q.get("token").map(String::as_str)) {
        let rt = state.settings_store.snapshot();
        return Json(json!({
            "ok": true,
            "version": env!("CARGO_PKG_VERSION"),
            "language": rt.language,
            // What the owner reads (the SMS forwarder labels its messages with it).
            "notify_language": rt.notify_lang().code(),
            "active_call_count": state.calls.live_calls().await.len(),
            "control_idle_clients": state.control.connected_serials().await.len(),
        }));
    }
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
    body.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    // The clock notifications and "today" use (set TZ when it is not yours).
    body.insert(
        "local_time".into(),
        json!(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)),
    );
    body.insert("language".into(), json!(rt.language));
    body.insert("notify_language".into(), json!(rt.notify_lang().code()));
    body.insert("usb_bridge".into(), usb_bridge::status_json());
    body.insert("calls_data_dir".into(), json!(s.calls_data_dir));
    body.insert("calls_record".into(), json!(s.calls_record));
    body.insert("calls_record_tts".into(), json!(s.calls_record_tts));
    body.insert("calls_retention_days".into(), json!(s.calls_retention_days));
    body.insert(
        "calls_ui_token_set".into(),
        json!(!s.calls_ui_token.trim().is_empty()),
    );
    body.insert("call_notify".into(), json!(s.call_notify));
    body.insert("call_notify_configured".into(), json!(notify_configured(s)));
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
    // Gemini Live / DeepSeek readiness (keys filled in /calls/ settings; never expose secrets).
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
    body.insert("reject_numbers_len".into(), json!(rt.reject_numbers.len()));
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
    // Effective view (env DEEPSEEK_API_KEY + UI override) — same as post-call summarize.
    let deepseek_ready = deepseek::summary_endpoint(&eff).is_some();
    body.insert("deepseek_api_key_set".into(), json!(deepseek_ready));
    body.insert("deepseek_summary_ready".into(), json!(deepseek_ready));
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
                "caller_id": c.caller_id,
                "serial": c.serial,
                "started_at": c.started_at,
            }))
            .collect::<Vec<_>>()),
    );
    body.insert("runtime".into(), json!("rust"));
    Json(Value::Object(body))
}

#[derive(Deserialize)]
struct TurnBody {
    text: Option<String>,
}

async fn turn(State(state): State<AppState>, Json(body): Json<TurnBody>) -> impl IntoResponse {
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
async fn transfer_smoke(State(state): State<AppState>) -> impl IntoResponse {
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
}

async fn handle_control_idle_ws(
    state: AppState,
    serial: String,
    sink: WsSink,
    mut stream: futures_util::stream::SplitStream<WebSocket>,
) {
    loop {
        let Some(msg) = stream.next().await else {
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
                        state.phones.on_status(&serial, &parsed);
                        sync_normal_mode(&state, &serial).await;
                    }
                    // Answer to a web console mode switch (`set_mode`).
                    "set_mode_result" => {
                        state.phones.on_mode_reply(&serial, &parsed);
                        sync_normal_mode(&state, &serial).await;
                    }
                    // Phone rings (not answered yet): how long to ring, and the AI gets
                    // ready meanwhile (ring once before answering).
                    "incoming" => {
                        let caller = extract_caller_id_from_msg(&parsed).unwrap_or_default();
                        note_call_sim(&serial, &parsed);
                        // A new call on this phone: a blocked call still waiting for its
                        // hello (declined, or its incoming_cancel lost) is over.
                        if let Some(old) = state.reject_pending.take(&serial) {
                            info!(
                                "incoming serial={serial} drops the earlier 拒接號碼 ring caller={}",
                                old.caller
                            );
                        }
                        let sample_rate = parsed
                            .get("sample_rate")
                            .and_then(|v| v.as_u64())
                            .filter(|v| (8_000..=48_000).contains(v))
                            .map_or(state.settings.sample_rate, |v| v as u32);
                        let rt = state.settings_store.snapshot();
                        if settings_store::reject_number_hit(&rt.reject_numbers, &caller) {
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
                                "incoming serial={serial} caller={caller} on 拒接號碼 → {} (no AI)",
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
                            "incoming serial={serial} caller={caller} answer_ring_ms={ring_ms} prewarm={prewarm}"
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
/// automatic `bageo.local` address is not used here). Nothing is sent while it is
/// unknown; older apps ignore the message.
async fn send_agent_info(state: &AppState, sink: &WsSink) {
    let eff = state.settings_store.snapshot().effective(&state.settings);
    let base = if eff.public_url.trim().is_empty() {
        settings_store::lan_console_url(&eff)
    } else {
        settings_store::effective_console_url(&eff)
    };
    if let Some(url) = calls_http::phone_console_url(&base) {
        let _ = send_json(sink, json!({"type": "agent_info", "console_url": url})).await;
    }
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(q): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> axum::response::Response {
    phone_ws(ws, peer, &q, state)
}

async fn handle_ws(socket: WebSocket, base_state: AppState) {
    // Session view of runtime settings: snapshot here, refreshed at call start
    // (hello/ringing). `state.settings` carries the env+runtime merge so all
    // existing helpers keep reading `state.settings.*` unchanged.
    let mut rt: Arc<RuntimeSettings> = base_state.settings_store.snapshot();
    let mut state = base_state.clone();
    state.settings = rt.effective(&base_state.settings);

    let (sink, mut stream) = socket.split();
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
                                    sync_normal_mode(&state, &serial).await;
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
                                    send_agent_info(&state, &sink).await;
                                    handle_control_idle_ws(state, serial, sink, stream).await;
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
                                if let Some(c) = extract_caller_id_from_msg(&parsed) {
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
                                    "session hello serial={serial} caller={caller_id} mode={} ivr_prompt={:?} ivr_loop={} t0_unix_ms={session_hello_t0_ms}",
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
                                // Blocked numbers: this number, or the phone's ringing call was on
                                // it (the hello may come before Telecom publishes the CID).
                                let rang_listed = state.reject_pending.take_for_hello(&serial, &caller_id);
                                if caller_id.trim().is_empty() {
                                    if let Some(c) = rang_listed
                                        .as_ref()
                                        .map(|r| r.caller.as_str())
                                        .filter(|c| !c.trim().is_empty())
                                    {
                                        caller_id = c.to_string();
                                    }
                                }
                                let reject_listed = rang_listed.is_some()
                                    || settings_store::reject_number_hit(
                                        &rt.reject_numbers,
                                        &caller_id,
                                    );
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
                                        "declined 拒接號碼 picked up anyway serial={serial} caller={caller_id} record={:?} → announcement",
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
                                        "session hello reattached existing call_id serial={serial} caller={caller_id}"
                                    );
                                }
                                if !call_notified {
                                    call_notified = true;
                                    let dedup = base_state.incoming_notify_dedup.clone();
                                    let caller = caller_id.clone();
                                    let ser = serial.clone();
                                    if dedup.claim(&ser, &caller).await {
                                        let http = state.http.clone();
                                        // Fresh snapshot: honor hot-swapped call_notify + gateway URL.
                                        let settings = {
                                            let snap = base_state.settings_store.snapshot();
                                            snap.effective(&base_state.settings)
                                        };
                                        let dedup_for_task = dedup.clone();
                                        let rejected = call_reject_listed;
                                        let reject_note = settings_store::reject_reason(
                                            &rt.reject_numbers,
                                            &caller_id,
                                        )
                                        .unwrap_or_default();
                                        tokio::spawn(async move {
                                            let ok = if rejected {
                                                notify_rejected_call(
                                                    &http, &settings, &caller, &ser, &reject_note, false,
                                                )
                                                .await
                                            } else {
                                                notify_incoming_call(&http, &settings, &caller, &ser)
                                                    .await
                                            };
                                            if ok {
                                                info!(
                                                    "incoming call notify ok serial={ser} caller={caller}"
                                                );
                                            } else {
                                                dedup_for_task.release(&ser, &caller).await;
                                                warn!(
                                                    "incoming call notify FAILED serial={ser} caller={caller} call_notify={} gateway_set={}",
                                                    settings.call_notify,
                                                    !settings.notify_gateway_url.trim().is_empty()
                                                );
                                            }
                                        });
                                    } else {
                                        info!(
                                            "incoming call notify dedup serial={ser} caller={caller} window_s={}",
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
                                            "unknown/hidden caller serial={serial} caller={caller_id:?} → answer like a normal call (answer_unknown_callers=on)"
                                        );
                                    }
                                    match cid_route_on_hello(&serial, &caller_id, answer_unknown, reject_listed) {
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
                                                "IVR skip isolated screening serial={serial} caller={caller_id:?} → phase Done (no greet/reject/VM/AI inject)"
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
                                                "unknown/private caller serial={serial} caller={caller_id:?} → reject WAV + hangup (skip greet/AI)"
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
                                            let adopted = match take_ring_prewarm(
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
                                info!("session ringing serial={serial} caller={caller_id}");
                                if settings_store::reject_number_hit(&rt.reject_numbers, &caller_id) {
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
                                    match cid_route_after_wait(
                                        &caller_id,
                                        rt.answer_unknown_callers,
                                        call_reject_listed,
                                    ) {
                                        CidRoute::Reject => {
                                            info!(
                                                "late hidden CID on ringing serial={serial} caller={caller_id:?} → reject-only"
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
                                                "late CID on ringing serial={serial} caller={caller_id:?} → 拒接號碼"
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
                                                "late visible CID on ringing serial={serial} caller={caller_id:?} → period greet"
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
                                            let adopted = match take_ring_prewarm(
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
                                    if dedup.claim(&ser, &caller).await {
                                        let http = state.http.clone();
                                        let settings = {
                                            let snap = base_state.settings_store.snapshot();
                                            snap.effective(&base_state.settings)
                                        };
                                        let dedup_for_task = dedup.clone();
                                        let rejected = call_reject_listed;
                                        let reject_note = settings_store::reject_reason(
                                            &rt.reject_numbers,
                                            &caller_id,
                                        )
                                        .unwrap_or_default();
                                        tokio::spawn(async move {
                                            let ok = if rejected {
                                                notify_rejected_call(
                                                    &http, &settings, &caller, &ser, &reject_note, false,
                                                )
                                                .await
                                            } else {
                                                notify_incoming_call(&http, &settings, &caller, &ser)
                                                    .await
                                            };
                                            if ok {
                                                info!(
                                                    "incoming call notify ok serial={ser} caller={caller}"
                                                );
                                            } else {
                                                dedup_for_task.release(&ser, &caller).await;
                                                warn!(
                                                    "incoming call notify FAILED serial={ser} caller={caller} call_notify={} gateway_set={}",
                                                    settings.call_notify,
                                                    !settings.notify_gateway_url.trim().is_empty()
                                                );
                                            }
                                        });
                                    } else {
                                        info!(
                                            "incoming call notify dedup serial={ser} caller={caller} window_s={}",
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
                                info!("session call_end serial={serial} caller={caller_id}");
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
                                trtc_bridge::make_bridge(
                                    &state.settings.trtc(),
                                    state.http.clone(),
                                )
                                .leave(&serial, "remote_call_end");
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
                                    if was_ai_talk || record_had_ai_fail_hangup(&record.events) {
                                        spawn_ai_talk_tg_summary(
                                            &state,
                                            &record,
                                            gemini_transcript.clone(),
                                            appointment_book,
                                        );
                                    }
                                }
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
                                            trtc_bridge::make_bridge(
                                                &state.settings.trtc(),
                                                state.http.clone(),
                                            )
                                            .leave(&serial, "dtmf_voicemail");
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
                                        && uplink_bytes % 320_000 < bin.len() as u64
                                    {
                                        info!(
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
                                                    c.method, c.contact
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
                                                "VM callback confirmed serial={serial} caller={caller_id:?}"
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
    if state.settings.agent_mode.is_transfer()
        && matches!(
            phase,
            IvrPhase::PlayPrompt
                | IvrPhase::WaitDtmf
                | IvrPhase::PlayHoldMusic
                | IvrPhase::TrtcAi
                | IvrPhase::LiveAi
        )
    {
        trtc_bridge::make_bridge(&state.settings.trtc(), state.http.clone())
            .leave(&serial, "ivr_disconnect");
    } else if trtc_ai_ended {
        trtc_bridge::make_bridge(&state.settings.trtc(), state.http.clone())
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
                if trtc_ai_ended || record_had_ai_fail_hangup(&record.events) {
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
        "client disconnected serial={serial} caller={caller_id} uplink_bytes={uplink_bytes} phase={phase:?} hold_started={hold_started}"
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
        None => {
            m.remove(&key);
        }
    }
}

fn call_sim_country(serial: &str) -> Option<String> {
    call_sim_countries()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&canonical_device_serial(serial))
        .cloned()
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
        None => {
            m.remove(&key);
        }
    }
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
        "call waiting rejected serial={serial} caller={caller} sim_country={}",
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
        info!("call waiting notify serial={serial} caller={caller} ok={ok}");
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
    if !state
        .media_play
        .claim(serial, MediaPlayKind::Reject, prompt_cancel)
        .await
    {
        warn!("unknown-caller reject skipped exclusive serial={serial}");
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
    if busy.try_lock().is_err() {
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
                    "拒接號碼 serial={serial} caller={caller} → 「{}」 voice={voice} audio_ms={} (no AI)",
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
                    "拒接號碼 serial={serial} caller={caller} announcement not ready voice={voice} — hang up without it"
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

/// `reject_mode` `decline`: a declined call never gets a media hello, so its
/// record (events `reject_list`, `reject_list_declined`) and the "⛔ blocked
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
        let reason = settings_store::reject_reason(&rt.reject_numbers, caller).unwrap_or_default();
        let (caller, serial) = (caller.to_string(), serial.to_string());
        tokio::spawn(async move {
            if notify_rejected_call(&http, &settings, &caller, &serial, &reason, true).await {
                info!("declined call notify ok serial={serial} caller={caller}");
            } else {
                dedup.release(&serial, &caller).await;
                warn!(
                    "declined call notify FAILED serial={serial} caller={caller} call_notify={}",
                    settings.call_notify
                );
            }
        });
    }
    record_id
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
    let reason = settings_store::reject_reason(&rt.reject_numbers, &record.caller_id);
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
                "對方：{number}{reason}\n事情：拒接號碼來電，播「{announce}」後掛斷\nAI 回覆：沒有接（沒用 AI）\n要處理：要聯絡的話自己回電"
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
        recording_text: String::new(),
        end_hint: if declined {
            locale::notify_zh_or(
                "拒接號碼：響鈴時直接拒接（沒有用 AI）",
                "blocked number: declined while it rang (no AI)",
            )
        } else {
            locale::notify_zh_or(
                "拒接號碼：播無法接通語音後掛斷（沒有用 AI）",
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
    if !state
        .media_play
        .claim(serial, MediaPlayKind::Greet, prompt_cancel)
        .await
    {
        warn!("IVR prompt skipped exclusive serial={serial} (reject/twin already playing)");
        return;
    }
    {
        let mut p = phase.lock().unwrap_or_else(|e| e.into_inner());
        // Greeting may only start from idle wait (not during voicemail/hold/goodbye).
        if *p != IvrPhase::WaitDtmf {
            return;
        }
        *p = IvrPhase::PlayPrompt;
    }
    if busy.try_lock().is_err() {
        *phase.lock().unwrap_or_else(|e| e.into_inner()) = IvrPhase::WaitDtmf;
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
        let persona = {
            let rt = state.settings_store.snapshot();
            state.settings = rt.effective(&state.settings);
            rt.gemini_persona_mode
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
        let wait_ms: u64 = if state.settings_store.snapshot().gemini_persona_mode {
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
    if !rt.gemini_ring_prewarm
        || !rt.gemini_persona_mode
        || !state.settings.agent_mode.is_transfer()
        || !rt.gemini_live_enabled
        || rt.gemini_api_key.trim().is_empty()
        || cid_route_on_hello(
            serial,
            caller_id,
            rt.answer_unknown_callers,
            settings_store::reject_number_hit(&rt.reject_numbers, caller_id),
        ) != CidRoute::Greet
    {
        return false;
    }
    // `incoming` often comes before the phone has looked up the SIM: the SIM of the
    // phone's last call stands in, and the pickup checks the guess
    // (`RingPrewarmRegistry::take`).
    let sim = call_sim_country(serial)
        .or_else(|| phone_region::last_sim(&canonical_device_serial(serial)));
    let plan = caller_plan(rt, caller_id, sim.as_deref());
    let cfg = GeminiRetryCfg {
        api_key: rt.gemini_api_key.clone(),
        model: rt.gemini_live_model.clone(),
        system: rt.gemini_system_prompt_for_call(),
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
        "Ring prewarm Gemini start serial={serial} caller={caller_id} model={} voice={} phone_sr={sample_rate} answer_ring_ms={ring_ms} (「喂？」 held until pickup)",
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
        &state.http,
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
            "Ring prewarm caller updated serial={serial} id={id} rang_caller={rang_caller} hello_caller={}",
            cfg.caller_id
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
        setup_ready.load(Ordering::Relaxed)
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
    let persona = rt.gemini_persona_mode;
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
    // Started at pickup: the media hello has reported the SIM (when the app does).
    let lang = call_lang_plan(&rt, &caller_id, call_sim_country(&serial).as_deref());
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
        &state.http,
    );
    // Kept for one fresh-socket retry if this session never delivers audio.
    let retry_cfg = fresh_retry_left.then_some(cfg);
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
    barge_in: Arc<AtomicBool>,
    /// What the caller agrees to on this call (appointments on), for the call record.
    appointments: Option<Arc<appointments::CallAppointments>>,
    /// The caller's number as the session reads it (the carrier-prompt check): a
    /// ring prewarm's is replaced by the media hello's at adoption.
    caller: Arc<std::sync::Mutex<String>>,
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
    http: &reqwest::Client,
) -> GeminiSessionCore {
    // Persona mode has no fixed clock, only the call-length safety cap; sales / scam stalling
    // swaps in its prompt once the caller scores as sales/scam.
    let stall_engaged = Arc::new(AtomicBool::new(false));
    let stall_cfg = (persona && rt.persona_stall_spam).then(|| gemini_live::StallCfg {
        engaged: stall_engaged.clone(),
        system_prompt: rt.persona_stall_system_prompt(),
        model: rt.gemini_stall_model.clone(),
        max_secs: rt.persona_stall_max_secs(),
    });
    let persona_max_secs = rt.persona_max_secs();
    let server_vad = rt.gemini_server_vad;
    let thinking_level = rt.gemini_thinking_level.clone();
    let caller_slot = Arc::new(std::sync::Mutex::new(cfg.caller_id.clone()));
    // Asked at pickup: by then the media hello has said which SIM took the call
    // (the ring prewarm starts before the phone knows) and which number calls.
    let intl_caller: gemini_live::IntlPromptCheck = {
        let serial = serial.to_string();
        let caller_slot = caller_slot.clone();
        let setting = rt.intl_carrier_prompt.clone();
        let auto_on = rt.lang() == locale::Lang::ZhTw;
        Box::new(move || {
            if !persona {
                return false;
            }
            let caller = caller_slot
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let sim = call_sim_country(&serial);
            let hold = gemini_live::expects_carrier_intl_prompt(
                &setting,
                auto_on,
                &caller,
                sim.as_deref(),
            );
            if gemini_live::is_intl_caller(&caller) || hold {
                // How the check read the number on this SIM.
                let number = sim
                    .as_deref()
                    .and_then(|s| phone_region::normalize(&caller, Some(s)))
                    .map_or_else(|| "?".to_string(), |n| format!("{} ({})", n.e164, n.country));
                info!(
                    "intl caller serial={serial} caller={caller} number={number} sim_country={} setting={setting} → hold opening for carrier prompt={hold}",
                    sim.as_deref().unwrap_or("?")
                );
            }
            hold
        })
    };
    // Appointments (setting `appointment_mode`): normal persona calls only.
    let mut appointments = if persona {
        appointments::ApptSession::for_call(rt, http, &cfg.caller_id)
    } else {
        None
    };
    // The SIM that took the call: a text back to the caller goes from it, and
    // its country reads a number without a country code. A ring prewarm sets it
    // again at pickup from the media hello.
    if let Some(a) = appointments.as_mut() {
        a.set_line(call_sim_country(serial), call_sim_sub_id(serial));
    }
    // The opening language and the caller's speech-to-text hints, decided with the
    // call's config (a ring prewarm's is checked again at pickup).
    let lang_plan = cfg.lang.clone();
    if (persona && lang_plan.opening.is_some()) || lang_plan.caller_stt.is_none() {
        info!(
            "caller languages serial={serial} caller={} opening={} caller_stt={}",
            cfg.caller_id,
            lang_plan.opening.filter(|_| persona).map_or("-", |l| l.code()),
            lang_plan
                .caller_stt
                .as_ref()
                .map_or_else(|| "auto".to_string(), |c| c.join(","))
        );
    }
    let appointment_book = appointments.as_ref().map(|a| a.book.clone());
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
        let system = if persona {
            let sim = call_sim_country(&serial).or_else(|| phone_region::last_sim(&canonical_device_serial(&serial)));
            contacts::with_caller(&system, rt, &cfg.caller_id, sim.as_deref())
        } else {
            system
        };
        let force_hangup = force_hangup.clone();
        let delivered_bytes = delivered_bytes.clone();
        let play_head = play_head.clone();
        let play_head_cb = play_head.clone();
        let guard_cb = guard_slot.clone();
        let barge_in = barge_in.clone();
        tokio::spawn(async move {
            gemini_live::run_session(
                &api_key,
                &model,
                &system,
                sample_rate,
                up_rx,
                move |pcm| {
                    // Double-gate: never inject before UnmuteAfterGreet.
                    if cancel_for_cb.load(Ordering::Relaxed)
                        || pcm.is_empty()
                        || !audio_open_cb.load(Ordering::Relaxed)
                        || !guard_cb.get().is_some_and(MediaCallGuard::is_current)
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
        barge_in,
        appointments: appointment_book,
        caller: caller_slot,
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
        appointments,
        ..
    } = core;
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
            )
            .await
        })
    };

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
        tokio::spawn(async move {
            let mut stall_recorded = false;
            loop {
                if !media_call_guard.is_current() {
                    info!("Gemini hangup watcher invalidated serial={serial}");
                    break;
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
                if force_hangup.load(Ordering::Relaxed) {
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
            if reason.should_play_goodbye() {
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
    let persona = rt.gemini_persona_mode;
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
    trtc_bridge::make_bridge(&state.settings.trtc(), state.http.clone()).leave(serial, reason);
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
    let persona = state.settings_store.snapshot().gemini_persona_mode;
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
                format!("☎️ {}", p.vm_call_back_unknown)
            } else {
                format!("☎️ {}", locale::fill(p.vm_call_back_to, &contact))
            }
        }
        (Some("email"), true) => {
            if contact.is_empty() {
                format!("📧 {}", p.vm_email_unknown)
            } else {
                format!("📧 {}", locale::fill(p.vm_email_to, &contact))
            }
        }
        (Some("phone"), false) => format!("☎️ {}", p.vm_call_back_unchecked),
        (Some(_), _) => format!("ℹ️ {}", p.vm_no_reply),
        (None, _) => format!("ℹ️ {}", p.vm_no_choice),
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
        let mut deepseek_block = String::new();
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
                    if deepseek::summary_endpoint(&eff).is_some() {
                        deepseek_block = deepseek::summarize(&http, &eff, &caller, &tx).await;
                        info!(
                            "voicemail deepseek summary ok serial={serial} call_id={call_id} chars={}",
                            deepseek_block.chars().count()
                        );
                    } else {
                        warn!(
                            "voicemail deepseek skipped (no API key in trtc_llm_config) serial={serial} call_id={call_id}"
                        );
                        deepseek_block = format!(
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
        let ok = if !deepseek_block.trim().is_empty() {
            let mut body = deepseek_block;
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
/// DeepSeek (TRTC_LLM_CONFIG) or OpenRouter intent when key set, else heuristic;
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

        // Prefer DeepSeek key from TRTC_LLM_CONFIG; fall back to legacy OpenRouter slot.
        let (llm_key, llm_model) =
            if let Some(k) = deepseek::api_key_from_llm_config(rt.trtc_llm_config.as_ref()) {
                (k, deepseek::DEEPSEEK_MODEL.to_string())
            } else {
                (rt.openrouter_api_key.clone(), rt.openrouter_model.clone())
            };

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
    let end_hint = summary_end_hint(&record.events);
    let ai_failed = record_had_ai_fail_hangup(&record.events);
    let stalled = record.events.iter().any(|e| e == "persona_stall_spam");
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
                (book.outcome(), book.voided())
            }
            None => (recorded_appointment, Vec::new()),
        };
        let mut appointment = match agreed {
            Some(a) if stalled || ai_failed => {
                voided.extend(appointments::all_events(&a));
                None
            }
            other => other,
        };
        let mut void_failed = appointments::delete_voided(&http, &rt, &voided).await;
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
        let live = clip_dialogue_for_summary(&live);
        // A number (or e-mail) the caller said to reach them on, from their own
        // words only (the AI may repeat or make one up): kept on the record and
        // on the call's task. Not from a sales / scam call or one the AI missed.
        let spoken_contact = if stalled || ai_failed {
            None
        } else {
            extract::extract_spoken_contact(&caller_side_text(&live_full, &remote_tx), &caller)
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
            deepseek::build_summary_input(
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
            (true, true) => "deepseek_input=gemini+whisper",
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
        let mut note_source = "deepseek";

        let mut body = if !transcript.trim().is_empty() {
            if deepseek::summary_endpoint(&eff).is_some() {
                let preview: String = transcript.chars().take(180).collect();
                info!(
                    "ai talk summarize serial={serial} tx_chars={} preview={}",
                    transcript.chars().count(),
                    preview.replace('\n', " | ")
                );
                source = "llm_summary";
                deepseek::summarize(&http, &eff, &caller, &transcript).await
            } else {
                warn!("ai talk summary skipped serial={serial}: no summary service (set a Gemini, DeepSeek or OpenAI-compatible key)");
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
        if note_source == "deepseek" && deepseek::is_fallback_summary(&body) {
            note_source = "fallback";
            if stalled {
                // We know what it was without the summary model: say so instead of a quote.
                body = stall_fallback_summary(&live_full, secs);
            }
        }
        let ai_failed_line = locale::notify_phrases().s_ai_failed;
        if ai_failed && !body.contains("AI 沒接上") && !body.contains(ai_failed_line) {
            // The summary model usually writes it (prompt rule); make sure the owner sees it.
            body.insert_str(0, &format!("{}\n", locale::notify_phrases().s_ai_fail_prefix));
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
            && appointment.is_some()
            && !gemini_live::dialogue_own_matter(&live_full)
            && (deepseek::summary_says_sales(locale::notify_lang(), &body)
                || (note_source != "deepseek" && gemini_live::dialogue_is_sales(&live_full)));
        let mut sales_voided: Vec<appointments::Earlier> = Vec::new();
        if late_sales {
            if let Some(a) = appointment.take() {
                let events = appointments::all_events(&a);
                let failed = appointments::delete_voided(&http, &rt, &events).await;
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
            && note_source == "deepseek"
            && !stalled
            && !late_sales
            && !ai_failed
        {
            if let Some(mut ev) = calendar::suggest(&http, &eff, &transcript).await {
                info!("calendar suggestion serial={serial} start={} all_day={}", ev.start, ev.all_day);
                if rt.calendar_auto_add && rt.gcal_configured() {
                    if let (Some(start), Some(end)) = (
                        calendar::local_rfc3339(&ev.start),
                        calendar::local_rfc3339(&ev.end),
                    ) {
                        match gcal::insert_event(
                            &http,
                            &rt.gcal_client_id,
                            &rt.gcal_client_secret,
                            &rt.gcal_refresh_token,
                            &rt.gcal_calendar_id,
                            &ev.title,
                            &body,
                            &start,
                            &end,
                        )
                        .await
                        {
                            Ok(id) => ev.google_event_id = id,
                            Err(e) => warn!("calendar auto-add failed serial={serial}: {e:#}"),
                        }
                    }
                }
                calendar_event = Some(ev);
            }
        }

        // The web console keeps the details; the notification stays short.
        let notes = calls::CallNotes {
            summary: body.clone(),
            // Speaker labels in the owner's language ("Caller: …").
            dialogue: gemini_live::localize_dialogue(&live_full, locale::notify_lang()),
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
        if notify_on {
            if let Some(ev) = &calendar_event {
                let label = locale::notify_phrases().n_calendar;
                body.push_str(&format!("\n📅 {label}{}{} ({})", locale::notify_kv_sep(), ev.title, ev.when()));
                if ev.google_event_id.is_empty() {
                    // Not inserted for the owner: one tap opens the filled-in form.
                    body.push_str(&format!("\n{}", ev.google_link("")));
                } else {
                    body.push_str(" ✓");
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
            let ok = notify_ai_request(&http, &eff, &caller, &serial, &body).await;
            info!(
                "ai talk tg summary notify serial={serial} backend={backend} ok={ok} source={source} body_chars={} tx_chars={}",
                body.chars().count(),
                transcript.chars().count()
            );
        } else {
            info!("ai talk tg summary skipped (notify not configured) serial={serial} — notes on web only");
        }
        let rt = settings_store.snapshot();
        for task in appointment_tasks {
            appointments::after_summary(&http, &rt, &task_store, &calls, &control, &task, &detail_url).await;
        }
    });
}

/// Persona mode gave up on the AI and hung up (`persona_ai_unavailable_hangup`).
fn record_had_ai_fail_hangup(events: &[String]) -> bool {
    events.iter().any(|e| e == "persona_ai_fail_hangup")
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
/// (e.g. repeated 503s) — topic from the caller's own keywords, no transcript quote.
fn stall_fallback_summary(dialogue: &str, secs: f64) -> String {
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
    let topic = if named.is_empty() {
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

/// One short line for the summary model about how the call ended, from the
/// record events (in the owner's language, like the summary).
fn summary_end_hint(events: &[String]) -> String {
    let has = |e: &str| events.iter().any(|x| x == e);
    if has("persona_ai_fail_hangup") {
        locale::notify_zh_or("AI 連不上，系統直接掛斷，對方沒聽到任何回應", "the AI could not connect; the system hung up and the caller heard nothing")
    } else if has("gemini_fail_to_voicemail") || has("gemini_zero_delivery_to_voicemail") {
        locale::notify_zh_or("AI 失敗，改走語音留言", "the AI failed; the call went to voicemail")
    } else if has("reject_unknown_caller") {
        locale::notify_zh_or("未顯示號碼，播拒接語音後掛斷（AI 沒有接）", "hidden number: the refusal announcement was played, then hung up (the AI did not answer)")
    } else if has("reject_list") {
        locale::notify_zh_or("拒接號碼：播無法接通語音後掛斷（AI 沒有接）", "blocked number: the unavailable announcement was played, then hung up (the AI did not answer)")
    } else if has("persona_stall_spam") {
        locale::notify_zh_or("系統判斷像推銷／詐騙，AI 改成故意陪他聊、拖時間", "the system judged it a sales / scam call, so the AI deliberately kept the caller talking to waste their time")
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
            "call reattach id={id} serial={serial} caller={caller_id} status={status}"
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
    if !rt.gemini_persona_mode || !ambience::valid_kind(&rt.ambience_kind) {
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
            if amb_continuous && buf.len() < frame {
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

    /// Traditional Chinese, Taiwan's calling code, the carrier prompt on auto.
    fn tw_settings() -> RuntimeSettings {
        let mut rt = RuntimeSettings::from_base(&Settings::from_env());
        rt.language = "zh-TW".into();
        rt.country_code = "886".into();
        rt.intl_carrier_prompt = "auto".into();
        rt.sim_languages = Default::default();
        rt.appointment_mode = "off".into();
        rt.appointment_test_numbers = Vec::new();
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
        };
        assert!(reg.take("pixel-1", "2025550143", 16_000, &plan_of(us)).is_none());
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(reg.pending(), 0);
        // Same opening, other hints: not handed over either.
        let (pw, cancel) = entry("+12025550143", live());
        reg.insert("pixel-1", pw);
        let auto = gemini_live::CallLangPlan { opening: None, caller_stt: None };
        assert!(reg.take("pixel-1", "+12025550143", 16_000, &plan_of(auto)).is_none());
        assert!(cancel.load(Ordering::Relaxed));
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
        assert_eq!(caller_plan(&rt, "", Some("tw")), local);
        let (mut pw, cancel) = entry("", live());
        pw.plan = local.clone();
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
        assert!(summary_end_hint(&["reject_list".to_string()]).contains("拒接號碼"));
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
        CallStore::new(dir, true, false, 7)
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
        let s = stall_fallback_summary(dialogue, 71.9);
        assert!(s.starts_with("事情：推銷／詐騙電話（提到：保險、推薦）"), "{s}");
        assert!(s.contains("約 72 秒") && s.ends_with("要處理：不用"), "{s}");
        assert!(stall_fallback_summary("對方：你好", 600.0).contains("約 10 分鐘"));
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

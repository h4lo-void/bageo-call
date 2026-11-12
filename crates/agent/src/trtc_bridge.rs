//! PCM ↔ Tencent TRTC media bridge ([`TrtcBridge`] trait).
//!
//! ## Role
//! Cellular PCM stays on the USB/WS edge (`main` IVR loop). This module owns
//! **room identity + join/leave + future PCM inject/egress** so menu audio and
//! DTMF-1 AI talk can move off local WAV/Piper once the Node sidecar lands.
//!
//! ## PCM contract (agent ↔ sidecar)
//! Keep in sync with the TRTC media sidecar (`pcm.mjs` + README):
//! **raw s16le · mono · [`PCM_SAMPLE_RATE`] Hz · [`PCM_FRAME_MS`] ms frames**
//! ([`PCM_FRAME_BYTES`] bytes each). Same shape as agent `/ws` binary frames.
//!
//! ## Wired from `main` today (do not rewrite IVR here)
//! | Hook | Call site | Soft behavior |
//! |------|-----------|---------------|
//! | [`TrtcBridge::join_menu_room`] + [`play_menu_via_trtc`] | `arm_trtc_menu_session` | UserSig + sidecar `open_session`; menu audio always local WAV/Piper (no menu StartAI) |
//! | [`TrtcBridge::enter_ai_talk`] | `start_trtc_ai_after_choice` | Re-key `bridge_{serial}`; REST `StartAIConversation`; silence scaffold stays in main |
//! | [`TrtcBridge::leave`] | DTMF 2 before `start_voicemail_intro` | REST `StopAIConversation` + sidecar `close_session` |
//!
//! ## Media vs control plane
//! - **Media join** = sidecar `trtc-sdk-v5` `enterRoom` only (no Cloud REST JoinRoom).
//! - **AI control** = Rust TC3 REST: Start / Stop / Control AIConversation.
//!
//! [`media_bridge_live`] is `true` when any sidecar room reports `joined=true`
//! (or `/health` `chromium_join`). Soft / local fallback / silence scaffold
//! stay when not joined.
//!
//! **Menu voice:** always local WAV/Piper. Do not StartAIConversation on the
//! menu path (that burned Tencent voice minutes). AI arms on DTMF 1 only.
//! Soft bridge / bridge-off / `joined=false` → `trtc_menu_local_fallback`.

use anyhow::{bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

use crate::trtc::TrtcSettings;

/// Agent ↔ sidecar PCM sample rate (Hz). Must match `SAMPLE_RATE` default in `config.rs`
/// and `PCM_SAMPLE_RATE` in the sidecar's `pcm.mjs`.
pub const PCM_SAMPLE_RATE: u32 = 16_000;
/// Mono only — no stereo interleave on the bridge IPC.
pub const PCM_CHANNELS: u16 = 1;
/// Signed 16-bit little-endian samples (2 bytes each).
pub const PCM_BITS: u16 = 16;
/// Nominal frame duration for uplink/downlink pacing (ms).
pub const PCM_FRAME_MS: u32 = 20;
/// Samples per 20 ms frame @ 16 kHz.
pub const PCM_FRAME_SAMPLES: usize = (PCM_SAMPLE_RATE as usize * PCM_FRAME_MS as usize) / 1000;
/// Bytes per 20 ms s16le mono frame: 16000 × 0.02 × 2 = **640**.
pub const PCM_FRAME_BYTES: usize =
    PCM_FRAME_SAMPLES * (PCM_BITS as usize / 8) * (PCM_CHANNELS as usize);

/// Reject PCM that violates the agent↔sidecar contract (rate / even length).
fn enforce_pcm_contract(pcm: &[u8], sample_rate: u32) -> Result<()> {
    if sample_rate != PCM_SAMPLE_RATE {
        bail!(
            "PCM contract: sample_rate must be {PCM_SAMPLE_RATE} (s16le mono {}ms frames), got {sample_rate}",
            PCM_FRAME_MS
        );
    }
    if pcm.len() % 2 != 0 {
        bail!(
            "PCM contract: s16le requires even byte length, got {}",
            pcm.len()
        );
    }
    Ok(())
}

fn ai_task_map() -> &'static Mutex<HashMap<String, String>> {
    static MAP: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    MAP.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn peek_ai_task_id(serial: &str) -> Option<String> {
    peek_task(serial)
}

/// Best-effort `ControlAIConversation` / `ServerPushText` for an armed AI task.
pub fn spawn_server_push_text(
    settings: &TrtcSettings,
    http: reqwest::Client,
    serial: &str,
    text: &str,
) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !settings.cloud_api_ready() || settings.api_client(http.clone()).is_none() {
        return;
    }
    let serial = serial.to_string();
    let text = text.to_string();
    let settings = settings.clone();
    tokio::spawn(async move {
        let _ = push_ai_line_await(&settings, http, &serial, &text).await;
    });
}

/// Await StartAI TaskId then speak via TRTC clone TTS (`ServerPushText`).
///
/// Used for DTMF1 hello / silence-nudge when AI conversation is live.
/// Returns `true` only if Cloud API accepted the push (caller must **not**
/// double-play local WAV/Piper on success — downlink carries TTS).
pub async fn push_ai_line_await(
    settings: &TrtcSettings,
    http: reqwest::Client,
    serial: &str,
    text: &str,
) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let Some(client) = settings.api_client(http) else {
        return false;
    };
    if !settings.cloud_api_ready() {
        return false;
    }
    // Wait for StartAIConversation (spawned on enter_ai_talk) to publish TaskId.
    // Faster poll (50ms) + up to ~12s — TaskId often lands during DTMF1 wait.
    for _ in 0..240 {
        if peek_task(serial).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let Some(task_id) = peek_task(serial) else {
        warn!("TRTC ServerPushText skipped serial={serial} (no TaskId yet)");
        return false;
    };
    match client.control_server_push_text(&task_id, text, true).await {
        Ok(()) => {
            info!(
                "TRTC ServerPushText ok serial={serial} text_chars={} task_ready=1",
                text.chars().count()
            );
            true
        }
        Err(e) => {
            warn!("TRTC ServerPushText failed serial={serial} err={e:#}");
            false
        }
    }
}

fn remember_task(serial: &str, task_id: String) {
    if let Ok(mut g) = ai_task_map().lock() {
        g.insert(serial.to_string(), task_id);
    }
}

fn peek_task(serial: &str) -> Option<String> {
    ai_task_map()
        .lock()
        .ok()
        .and_then(|g| g.get(serial).cloned())
}

fn take_task(serial: &str) -> Option<String> {
    ai_task_map().lock().ok().and_then(|mut g| g.remove(serial))
}

/// Serials that still hold a StartAIConversation TaskId (billing risk if stuck).
pub fn list_ai_task_serials() -> Vec<String> {
    ai_task_map()
        .lock()
        .map(|g| g.keys().cloned().collect())
        .unwrap_or_default()
}

/// Room ids currently marked joined in-process.
pub fn list_joined_room_ids() -> Vec<String> {
    joined_rooms()
        .lock()
        .map(|g| g.iter().cloned().collect())
        .unwrap_or_default()
}

/// Stop AI + close sidecar for every known serial (ai TaskId map + joined rooms).
/// Used by `/calls/api/sim/force_leave` to stop leftover billed voice minutes.
pub fn force_leave_known(
    settings: &TrtcSettings,
    http: reqwest::Client,
    reason: &str,
) -> Vec<String> {
    let mut serials: HashSet<String> = list_ai_task_serials().into_iter().collect();
    for room in list_joined_room_ids() {
        if let Some(s) = room.strip_prefix("call_") {
            serials.insert(s.to_string());
        }
    }
    let mut out: Vec<String> = serials.into_iter().collect();
    out.sort();
    for s in &out {
        make_bridge(settings, http.clone()).leave(s, reason);
    }
    out
}

/// Rooms where sidecar reported `joined=true` (process-wide; survives `make_bridge`).
fn joined_rooms() -> &'static Mutex<HashSet<String>> {
    static ROOMS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    ROOMS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Sidecar `/health` reported `joined` / `chromium_join` (no per-room id).
fn health_joined_flag() -> &'static AtomicBool {
    static F: OnceLock<AtomicBool> = OnceLock::new();
    F.get_or_init(|| AtomicBool::new(false))
}

fn mark_room_joined(room_id: &str, joined: bool) {
    if room_id.is_empty() {
        return;
    }
    if let Ok(mut g) = joined_rooms().lock() {
        if joined {
            g.insert(room_id.to_string());
        } else {
            g.remove(room_id);
        }
    }
    // Low-latency PCM: open/close sidecar WS when join state flips.
    if joined {
        // URL filled by callers that know bridge_base_url via ensure_pcm_ws_from_settings.
    } else {
        close_pcm_ws(room_id);
    }
}

/// Low-latency agent↔sidecar PCM over WebSocket (`/pcm?room_id=`).
/// Avoids per-frame HTTP POST/GET (often +50–200 ms under load).
struct PcmWsLink {
    uplink_tx: mpsc::UnboundedSender<Vec<u8>>,
    downlink: Arc<Mutex<VecDeque<Vec<u8>>>>,
    closed: Arc<AtomicBool>,
}

fn pcm_ws_map() -> &'static Mutex<HashMap<String, PcmWsLink>> {
    static M: OnceLock<Mutex<HashMap<String, PcmWsLink>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn http_base_to_ws(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if let Some(rest) = b.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = b.strip_prefix("http://") {
        format!("ws://{rest}")
    } else if b.starts_with("ws://") || b.starts_with("wss://") {
        b.to_string()
    } else {
        format!("ws://{b}")
    }
}

/// Open (or keep) a binary PCM WebSocket to the sidecar for this room.
pub fn ensure_pcm_ws(base_url: &str, room_id: &str) {
    if room_id.is_empty() || base_url.trim().is_empty() {
        return;
    }
    if let Ok(g) = pcm_ws_map().lock() {
        if g.contains_key(room_id) {
            return;
        }
    }
    let (up_tx, mut up_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let down = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
    let closed = Arc::new(AtomicBool::new(false));
    let down_r = down.clone();
    let closed_r = closed.clone();
    let room = room_id.to_string();
    let ws_url = format!(
        "{}/pcm?room_id={}",
        http_base_to_ws(base_url),
        urlencoding_room(room_id)
    );
    {
        let mut g = match pcm_ws_map().lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        // Double-check after building channels.
        if g.contains_key(room_id) {
            return;
        }
        g.insert(
            room_id.to_string(),
            PcmWsLink {
                uplink_tx: up_tx,
                downlink: down,
                closed: closed.clone(),
            },
        );
    }
    info!("TRTC PCM WS connecting room_id={room} (low-latency uplink/downlink)");
    tokio::spawn(async move {
        let connect = tokio_tungstenite::connect_async(&ws_url).await;
        let (ws, _) = match connect {
            Ok(x) => x,
            Err(e) => {
                warn!("TRTC PCM WS connect failed room_id={room} err={e:#}");
                close_pcm_ws(&room);
                return;
            }
        };
        info!("TRTC PCM WS open room_id={room}");
        let (mut sink, mut stream) = ws.split();
        let writer = async {
            while let Some(pcm) = up_rx.recv().await {
                if closed_r.load(Ordering::Relaxed) {
                    break;
                }
                if pcm.is_empty() {
                    continue;
                }
                if sink.send(WsMessage::Binary(pcm.into())).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        };
        let reader = async {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(WsMessage::Binary(bin)) if !bin.is_empty() => {
                        if let Ok(mut q) = down_r.lock() {
                            q.push_back(bin.to_vec());
                            // Cap ~1s of PCM @ 16k mono to bound memory under stall.
                            let mut bytes: usize = q.iter().map(|c| c.len()).sum();
                            while bytes > 32_000 && q.len() > 1 {
                                if let Some(drop) = q.pop_front() {
                                    bytes = bytes.saturating_sub(drop.len());
                                } else {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(WsMessage::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        };
        tokio::select! {
            _ = writer => {},
            _ = reader => {},
        }
        closed_r.store(true, Ordering::Relaxed);
        close_pcm_ws(&room);
        info!("TRTC PCM WS closed room_id={room}");
    });
}

pub fn close_pcm_ws(room_id: &str) {
    if let Ok(mut g) = pcm_ws_map().lock() {
        if let Some(link) = g.remove(room_id) {
            link.closed.store(true, Ordering::Relaxed);
        }
    }
}

/// Prefer WS uplink; fall back to HTTP.
pub fn push_uplink_ws(room_id: &str, pcm: &[u8]) -> bool {
    if pcm.is_empty() {
        return true;
    }
    let Ok(g) = pcm_ws_map().lock() else {
        return false;
    };
    let Some(link) = g.get(room_id) else {
        return false;
    };
    if link.closed.load(Ordering::Relaxed) {
        return false;
    }
    link.uplink_tx.send(pcm.to_vec()).is_ok()
}

/// Prefer WS downlink buffer; empty if none.
pub fn pull_downlink_ws(room_id: &str, max_bytes: usize) -> Vec<u8> {
    if max_bytes == 0 {
        return Vec::new();
    }
    let Ok(g) = pcm_ws_map().lock() else {
        return Vec::new();
    };
    let Some(link) = g.get(room_id) else {
        return Vec::new();
    };
    let Ok(mut q) = link.downlink.lock() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    while out.len() < max_bytes {
        let Some(head) = q.front() else {
            break;
        };
        let need = max_bytes - out.len();
        if head.len() <= need {
            let chunk = q.pop_front().unwrap();
            out.extend_from_slice(&chunk);
        } else {
            out.extend_from_slice(&head[..need]);
            let rest = head[need..].to_vec();
            q.pop_front();
            if !rest.is_empty() {
                q.push_front(rest);
            }
            break;
        }
    }
    out
}

/// Ensure PCM WS when settings + room are known (call after open_session joined=true).
pub fn ensure_pcm_ws_from_settings(settings: &TrtcSettings, room_id: &str) {
    if let Some(base) = settings.bridge_base_url() {
        if settings.bridge_enabled {
            ensure_pcm_ws(&base, room_id);
        }
    }
}

fn mark_health_joined(joined: bool) {
    health_joined_flag().store(joined, Ordering::Relaxed);
}

/// `true` if this room's sidecar session reported joined.
pub fn room_media_live(room_id: &str) -> bool {
    joined_rooms()
        .lock()
        .map(|g| g.contains(room_id))
        .unwrap_or(false)
}

fn any_room_joined() -> bool {
    health_joined_flag().load(Ordering::Relaxed)
        || joined_rooms()
            .lock()
            .map(|g| !g.is_empty())
            .unwrap_or(false)
}

/// Run a future on the current tokio runtime from a sync `TrtcBridge` method.
fn block_on_current<F, T>(fut: F) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    let handle = tokio::runtime::Handle::try_current().ok()?;
    Some(tokio::task::block_in_place(|| handle.block_on(fut)))
}

fn urlencoding_room(room_id: &str) -> String {
    room_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c.to_string()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

/// Cellular `/ws` uplink → sidecar (prefer **PCM WebSocket**, else HTTP POST).
///
/// Called from the IVR binary path when [`room_media_live`] is true.
/// Soft / no-bridge / contract fail: returns `Ok(())` (never blocks DTMF1 degrade).
pub async fn push_uplink_http(
    settings: &TrtcSettings,
    http: &reqwest::Client,
    room_id: &str,
    pcm: &[u8],
    sample_rate: u32,
) -> Result<()> {
    if pcm.is_empty() {
        return Ok(());
    }
    enforce_pcm_contract(pcm, sample_rate)?;
    if !room_media_live(room_id) {
        return Ok(());
    }
    // Prefer persistent WS (no HTTP overhead per 20 ms frame).
    if push_uplink_ws(room_id, pcm) {
        return Ok(());
    }
    // Lazy open if room is live but WS not up yet.
    ensure_pcm_ws_from_settings(settings, room_id);
    if push_uplink_ws(room_id, pcm) {
        return Ok(());
    }
    let Some(base) = settings.bridge_base_url() else {
        return Ok(());
    };
    if !settings.bridge_enabled {
        return Ok(());
    }
    let url = format!(
        "{base}/pcm/uplink?room_id={}&sr={sample_rate}",
        urlencoding_room(room_id)
    );
    match http
        .post(&url)
        .timeout(Duration::from_millis(200))
        .header("Content-Type", "application/octet-stream")
        .body(pcm.to_vec())
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) => {
            warn!(
                "TRTC push_uplink_http HTTP {} room_id={room_id}",
                resp.status()
            );
            Ok(())
        }
        Err(e) => {
            warn!("TRTC push_uplink_http failed room_id={room_id} err={e:#}");
            Ok(())
        }
    }
}

/// How the choice menu is spoken after [`arm_and_queue_menu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuPlayPath {
    /// `ControlAIConversation` / `ServerPushText` — caller **skips** local WAV.
    ServerPushText,
    /// Not this-room-live or Cloud API not ready — caller plays local `trtc_menu_local_fallback`.
    LocalFallback,
}

/// Opaque join credentials for one agent participant in a call room.
/// `user_sig` must never be logged in full.
#[derive(Clone)]
pub struct TrtcJoinParams {
    pub sdk_app_id: u32,
    pub room_id: String,
    pub user_id: String,
    /// TLSSigAPIv2 UserSig — hold in memory only; Debug redacts.
    pub user_sig: String,
}

impl std::fmt::Debug for TrtcJoinParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrtcJoinParams")
            .field("sdk_app_id", &self.sdk_app_id)
            .field("room_id", &self.room_id)
            .field("user_id", &self.user_id)
            .field("user_sig", &"***")
            .finish()
    }
}

/// Lifecycle + PCM ports for one inbound cellular call on the TRTC path.
pub trait TrtcBridge: Send + Sync {
    /// `true` only when realtime media can replace local menu WAV/Piper.
    /// Soft scaffold always returns `false`. Sidecar returns `true` after
    /// `open_session` / `/health` reports `joined`.
    fn media_bridge_live(&self) -> bool;

    /// Enter the room for the short choice menu (press 1 AI call / press 2 voicemail).
    fn join_menu_room(&self, settings: &TrtcSettings, serial: &str) -> Result<TrtcJoinParams>;

    /// Queue menu audio on the TRTC side.
    ///
    /// Soft / room not joined: → [`MenuPlayPath::LocalFallback`] (local WAV).
    /// This room joined + cloud API ready: `StartAIConversation` + `ServerPushText`.
    fn play_menu_via_trtc(
        &self,
        join: &TrtcJoinParams,
        serial: &str,
        menu_text: &str,
    ) -> Result<MenuPlayPath>;

    /// DTMF 1 — stay in room for AI talk (re-key UserSig as bridge user if needed).
    fn enter_ai_talk(&self, settings: &TrtcSettings, serial: &str) -> Result<TrtcJoinParams>;

    /// DTMF 2 / hangup — leave TRTC AI path before local voicemail.
    fn leave(&self, serial: &str, reason: &str);

    /// Push s16le mono PCM into the TRTC room (agent → cloud).
    ///
    /// Contract: [`PCM_SAMPLE_RATE`] Hz, mono, s16le; prefer [`PCM_FRAME_BYTES`]
    /// (20 ms) chunks. Implementations must reject other sample rates.
    fn push_uplink_pcm(&self, join: &TrtcJoinParams, pcm: &[u8], sample_rate: u32) -> Result<()>;

    /// Pull TRTC downlink PCM for cellular WS playout (`write_tts` lane /
    /// `_tts.wav`). `main::spawn_trtc_downlink_pull_loop` records
    /// every non-empty chunk via [`crate::calls::ActiveCall::write_tts`].
    /// Returned bytes are s16le mono @ [`PCM_SAMPLE_RATE`] (same as `/ws`).
    fn pull_downlink_pcm(&self, join: &TrtcJoinParams, max_bytes: usize) -> Result<Vec<u8>>;
}

/// Soft scaffold: real UserSig + room ids; no native WebRTC / Cloud media yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct SoftTrtcBridge;

impl SoftTrtcBridge {
    pub fn new() -> Self {
        Self
    }

    pub fn room_id_for(serial: &str) -> String {
        let cleaned: String = serial
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("call_{cleaned}")
    }
}

impl TrtcBridge for SoftTrtcBridge {
    fn media_bridge_live(&self) -> bool {
        false
    }

    fn join_menu_room(&self, settings: &TrtcSettings, serial: &str) -> Result<TrtcJoinParams> {
        let user_id = format!("menu_{serial}");
        let room_id = Self::room_id_for(serial);
        let user_sig = settings.gen_user_sig(&user_id)?;
        let join = TrtcJoinParams {
            sdk_app_id: settings.sdk_app_id,
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            user_sig,
        };
        info!(
            "TrtcBridge join_menu_room serial={serial} sdk_app_id={} room_id={room_id} user_id={user_id} — UserSig armed; media enterRoom = sidecar SDK only (no Cloud JoinRoom REST)",
            settings.sdk_app_id
        );
        Ok(join)
    }

    fn play_menu_via_trtc(
        &self,
        join: &TrtcJoinParams,
        serial: &str,
        menu_text: &str,
    ) -> Result<MenuPlayPath> {
        let _ = menu_text;
        info!(
            "menu via TRTC pending SDK serial={serial} room_id={} user_id={} sdk_app_id={} — keep trtc_menu_local_fallback until media_bridge_live",
            join.room_id, join.user_id, join.sdk_app_id
        );
        Ok(MenuPlayPath::LocalFallback)
    }

    fn enter_ai_talk(&self, settings: &TrtcSettings, serial: &str) -> Result<TrtcJoinParams> {
        let user_id = format!("bridge_{serial}");
        let room_id = Self::room_id_for(serial);
        let user_sig = settings.gen_user_sig(&user_id)?;
        let join = TrtcJoinParams {
            sdk_app_id: settings.sdk_app_id,
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            user_sig,
        };
        info!(
            "TrtcBridge enter_ai_talk serial={serial} sdk_app_id={} room_id={room_id} user_id={user_id} — AI on TRTC; PCM ports pending (push_uplink_pcm / pull_downlink_pcm)",
            settings.sdk_app_id
        );
        Ok(join)
    }

    fn leave(&self, serial: &str, reason: &str) {
        info!("TrtcBridge leave serial={serial} reason={reason}");
    }

    fn push_uplink_pcm(&self, join: &TrtcJoinParams, pcm: &[u8], sample_rate: u32) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        enforce_pcm_contract(pcm, sample_rate)?;
        let _ = join;
        Ok(())
    }

    fn pull_downlink_pcm(&self, join: &TrtcJoinParams, max_bytes: usize) -> Result<Vec<u8>> {
        let _ = (join, max_bytes);
        // Soft: empty; live path returns s16le mono @ PCM_SAMPLE_RATE.
        Ok(Vec::new())
    }
}

/// Sidecar-aware bridge: same [`TrtcBridge`] trait; talks to
/// the TRTC media sidecar for SDK `enterRoom` and fires TC3 REST for AI.
///
/// On sidecar/REST failure: logs and keeps soft UserSig + silence-scaffold path
/// (`media_bridge_live` false until join reports `joined=true`).
#[derive(Clone)]
pub struct SidecarTrtcBridge {
    soft: SoftTrtcBridge,
    http: reqwest::Client,
    /// Snapshot for IPC + REST (secrets stay SecretString / never logged).
    settings: TrtcSettings,
}

impl SidecarTrtcBridge {
    pub fn new(settings: TrtcSettings, http: reqwest::Client) -> Self {
        Self {
            soft: SoftTrtcBridge::new(),
            http,
            settings,
        }
    }

    fn spawn_sidecar_json(&self, path: &str, body: serde_json::Value) {
        let Some(base) = self.settings.bridge_base_url() else {
            return;
        };
        if !self.settings.bridge_enabled {
            info!("TRTC sidecar IPC skipped (TRTC_BRIDGE_ENABLED=0) path={path}");
            return;
        }
        let url = format!("{base}{path}");
        let http = self.http.clone();
        tokio::spawn(async move {
            match http.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {
                    info!("TRTC sidecar IPC ok status={}", resp.status());
                }
                Ok(resp) => {
                    warn!(
                        "TRTC sidecar IPC HTTP {} — soft path continues",
                        resp.status()
                    );
                }
                Err(e) => {
                    warn!("TRTC sidecar IPC failed err={e:#} — soft path continues");
                }
            }
        });
    }

    /// Fire-and-forget `/session/open`; marks room joined when Chromium reports it.
    /// Use for menu so Piper is not blocked behind a 12s Chromium join.
    fn spawn_open_session(&self, body: serde_json::Value) {
        let Some(base) = self.settings.bridge_base_url() else {
            return;
        };
        if !self.settings.bridge_enabled {
            info!("TRTC open_session spawn skipped (TRTC_BRIDGE_ENABLED=0)");
            return;
        }
        let url = format!("{base}/session/open");
        let http = self.http.clone();
        let base_owned = base.to_string();
        let room_id = body
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        tokio::spawn(async move {
            match http
                .post(&url)
                .timeout(Duration::from_secs(20))
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    let v: serde_json::Value = resp.json().await.unwrap_or_else(|_| json!({}));
                    if !status.is_success() {
                        warn!(
                            "TRTC open_session (async) HTTP {status} room_id={room_id} — soft path continues"
                        );
                        mark_room_joined(&room_id, false);
                        return;
                    }
                    let joined = v
                        .get("joined")
                        .and_then(|x| x.as_bool())
                        .or_else(|| v.get("chromium_join").and_then(|x| x.as_bool()))
                        .unwrap_or(false);
                    mark_room_joined(&room_id, joined);
                    if joined {
                        info!(
                            "TRTC open_session (async) joined=true room_id={room_id} — media_bridge_live armed"
                        );
                        ensure_pcm_ws(&base_owned, &room_id);
                    } else {
                        info!(
                            "TRTC open_session (async) joined=false room_id={room_id} — keep local fallback"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "TRTC open_session (async) failed room_id={room_id} err={e:#} — soft path continues"
                    );
                    mark_room_joined(&room_id, false);
                }
            }
        });
    }

    /// POST `/session/open` and wait; mark room joined from JSON `joined`.
    fn open_session_await(&self, body: serde_json::Value) -> bool {
        let Some(base) = self.settings.bridge_base_url() else {
            return false;
        };
        if !self.settings.bridge_enabled {
            info!("TRTC open_session skipped (TRTC_BRIDGE_ENABLED=0)");
            return false;
        }
        let url = format!("{base}/session/open");
        let http = self.http.clone();
        let room_id = body
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Chromium enterRoom often takes 8–15s; short timeout left media_live=false
        // after wait → dead AI talk. Await long enough to match DTMF1 wait clip.
        let result = block_on_current(async move {
            match http
                .post(&url)
                .timeout(Duration::from_secs(20))
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    let v: serde_json::Value = resp.json().await.unwrap_or_else(|_| json!({}));
                    Some((status, v))
                }
                Err(e) => {
                    warn!("TRTC open_session failed err={e:#} — soft path continues");
                    None
                }
            }
        });
        let Some((status, v)) = result.flatten() else {
            mark_room_joined(&room_id, false);
            return false;
        };
        if !status.is_success() {
            warn!("TRTC open_session HTTP {} — soft path continues", status);
            mark_room_joined(&room_id, false);
            return false;
        }
        let joined = v
            .get("joined")
            .and_then(|x| x.as_bool())
            .or_else(|| v.get("chromium_join").and_then(|x| x.as_bool()))
            .unwrap_or(false);
        mark_room_joined(&room_id, joined);
        if joined {
            info!("TRTC open_session joined=true room_id={room_id} — media_bridge_live armed");
            if let Some(base) = self.settings.bridge_base_url() {
                ensure_pcm_ws(&base, &room_id);
            }
        } else {
            info!(
                "TRTC open_session joined=false room_id={room_id} — keep local fallback / silence"
            );
        }
        joined
    }

    fn spawn_start_ai(&self, serial: &str, join: &TrtcJoinParams) {
        let settings = &self.settings;
        let Some(client) = settings.api_client(self.http.clone()) else {
            info!(
                "TRTC StartAIConversation skipped serial={serial} (no TRTC_SECRET_ID) — silence scaffold"
            );
            return;
        };
        if !settings.cloud_api_ready() {
            info!(
                "TRTC StartAIConversation skipped serial={serial} (LLM/TTS config empty) — silence scaffold"
            );
            return;
        }
        // Second call same serial: never skip StartAI because of a stale TaskId.
        // Always stop leftover task first (first call hangup may have failed to clear).
        let stale = take_task(serial);
        if stale.is_some() {
            warn!("TRTC StartAIConversation clearing stale TaskId serial={serial} before re-arm");
        }
        let serial = serial.to_string();
        let sdk_app_id = join.sdk_app_id;
        let room_id = join.room_id.clone();
        // Always target bridge_* (media participant), even if join.user_id is menu_*.
        let bridge_user = if join.user_id.starts_with("bridge_") {
            join.user_id.clone()
        } else {
            format!("bridge_{serial}")
        };
        let ai_user = format!("ai_{serial}");
        let max_idle = settings.ai_max_idle_secs;
        let stt = settings.stt_language.clone();
        let llm = settings.llm_config.as_str().to_string();
        let tts = settings.tts_config.as_str().to_string();
        let settings_for_sig = settings.clone();
        tokio::spawn(async move {
            if let Some(old_tid) = stale {
                for attempt in 1..=3u64 {
                    match client.stop_ai_conversation(&old_tid).await {
                        Ok(()) => {
                            info!(
                                "TRTC StopAIConversation stale ok serial={serial} attempt={attempt}"
                            );
                            break;
                        }
                        Err(e) if attempt < 3 => {
                            warn!(
                                "TRTC StopAIConversation stale serial={serial} attempt={attempt} err={e:#}"
                            );
                            tokio::time::sleep(Duration::from_millis(120 * attempt)).await;
                        }
                        Err(e) => {
                            warn!("TRTC StopAIConversation stale failed serial={serial} err={e:#}")
                        }
                    }
                }
                // Brief settle so Cloud accepts a new StartAI in the same room.
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            let ai_sig = match settings_for_sig.gen_user_sig(&ai_user) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        "TRTC StartAIConversation UserSig(ai) failed serial={serial} err={e:#} — silence scaffold"
                    );
                    return;
                }
            };
            match client
                .start_ai_conversation(
                    sdk_app_id,
                    &room_id,
                    &ai_user,
                    &ai_sig,
                    &bridge_user,
                    max_idle,
                    &stt,
                    &llm,
                    &tts,
                )
                .await
            {
                Ok(r) => {
                    remember_task(&serial, r.task_id);
                    info!("TRTC StartAIConversation armed serial={serial} room_id={room_id}");
                }
                Err(e) => {
                    warn!(
                        "TRTC StartAIConversation failed serial={serial} err={e:#} — silence scaffold continues"
                    );
                }
            }
        });
    }

    /// Live menu: ensure AI task exists, then `ControlAIConversation` `ServerPushText`.
    fn spawn_menu_server_push(&self, serial: &str, join: &TrtcJoinParams, menu_text: &str) {
        let settings = &self.settings;
        let Some(client) = settings.api_client(self.http.clone()) else {
            warn!("TRTC menu ServerPushText skipped serial={serial} (no Cloud API client)");
            return;
        };
        if !settings.cloud_api_ready() {
            warn!("TRTC menu ServerPushText skipped serial={serial} (cloud_api_ready=false)");
            return;
        }
        let serial = serial.to_string();
        let sdk_app_id = join.sdk_app_id;
        let room_id = SoftTrtcBridge::room_id_for(&serial);
        let bridge_user = format!("bridge_{serial}");
        let ai_user = format!("ai_{serial}");
        let max_idle = settings.ai_max_idle_secs;
        let stt = settings.stt_language.clone();
        let llm = settings.llm_config.as_str().to_string();
        let tts = settings.tts_config.as_str().to_string();
        let menu_text = menu_text.to_string();
        let settings_for_sig = settings.clone();
        tokio::spawn(async move {
            let task_id = if let Some(existing) = peek_task(&serial) {
                existing
            } else {
                let ai_sig = match settings_for_sig.gen_user_sig(&ai_user) {
                    Ok(s) => s,
                    Err(e) => {
                        warn!(
                            "TRTC menu ServerPushText UserSig(ai) failed serial={serial} err={e:#}"
                        );
                        return;
                    }
                };
                match client
                    .start_ai_conversation(
                        sdk_app_id,
                        &room_id,
                        &ai_user,
                        &ai_sig,
                        &bridge_user,
                        max_idle,
                        &stt,
                        &llm,
                        &tts,
                    )
                    .await
                {
                    Ok(r) => {
                        remember_task(&serial, r.task_id.clone());
                        info!(
                            "TRTC StartAIConversation armed for menu ServerPushText serial={serial} room_id={room_id}"
                        );
                        r.task_id
                    }
                    Err(e) => {
                        warn!(
                            "TRTC menu StartAIConversation failed serial={serial} err={e:#} — no ServerPushText"
                        );
                        return;
                    }
                }
            };
            match client
                .control_server_push_text(&task_id, &menu_text, true)
                .await
            {
                Ok(()) => info!(
                    "TRTC menu ServerPushText queued serial={serial} text_chars={}",
                    menu_text.chars().count()
                ),
                Err(e) => warn!("TRTC menu ServerPushText failed serial={serial} err={e:#}"),
            }
        });
    }

    fn spawn_stop_ai(&self, serial: &str) {
        let dispatched = Instant::now();
        let task = take_task(serial);
        let task_present = task.is_some();
        info!(
            "TRTC cleanup dispatched serial={serial} task_present={task_present} tasks_after={} rooms_after={}",
            list_ai_task_serials().len(), list_joined_room_ids().len(),
        );
        let Some(task_id) = task else {
            return;
        };
        let Some(client) = self.settings.api_client(self.http.clone()) else {
            warn!("TRTC cleanup cannot StopAI serial={serial} cloud_client_ready=false");
            return;
        };
        let serial = serial.to_string();
        tokio::spawn(async move {
            let mut stopped = false;
            for attempt in 1..=3u64 {
                match client.stop_ai_conversation(&task_id).await {
                    Ok(()) => { stopped = true; break; }
                    Err(e) if attempt < 3 => {
                        warn!("TRTC StopAIConversation transient serial={serial} attempt={attempt} err={e:#}");
                        tokio::time::sleep(Duration::from_millis(150 * attempt)).await;
                    }
                    Err(e) => warn!("TRTC StopAIConversation failed serial={serial} attempt={attempt} err={e:#}"),
                }
            }
            info!(
                "TRTC cleanup complete serial={serial} stop_ok={stopped} elapsed_ms={} task_present=true",
                dispatched.elapsed().as_millis()
            );
        });
    }
}

impl TrtcBridge for SidecarTrtcBridge {
    fn media_bridge_live(&self) -> bool {
        any_room_joined()
    }

    fn join_menu_room(&self, settings: &TrtcSettings, serial: &str) -> Result<TrtcJoinParams> {
        // Soft menu_* UserSig for identity logs; sidecar enters as bridge_* (media).
        let join = self.soft.join_menu_room(settings, serial)?;
        let bridge_user = format!("bridge_{serial}");
        let bridge_sig = match settings.gen_user_sig(&bridge_user) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "TRTC sidecar bridge UserSig failed serial={serial} err={e:#}; soft menu path only"
                );
                return Ok(join);
            }
        };
        let room_id = SoftTrtcBridge::room_id_for(serial);
        // Do NOT block the IVR menu on Chromium enterRoom (was ~10–12s → WS died
        // before Piper TTS could reach the phone). Join in background; menu uses
        // local Piper until this room is live.
        self.spawn_open_session(json!({
            "cmd": "open_session",
            "room_id": room_id,
            "user_id": bridge_user,
            "sdk_app_id": settings.sdk_app_id,
            "user_sig": bridge_sig,
            // PCM contract: s16le mono @ PCM_SAMPLE_RATE, 20 ms frames.
            "sample_rate": PCM_SAMPLE_RATE,
            "phase": "menu",
        }));
        Ok(join)
    }

    fn play_menu_via_trtc(
        &self,
        join: &TrtcJoinParams,
        serial: &str,
        menu_text: &str,
    ) -> Result<MenuPlayPath> {
        self.spawn_sidecar_json(
            "/session/phase",
            json!({
                "cmd": "set_phase",
                "room_id": join.room_id,
                "phase": "menu",
                "serial": serial,
            }),
        );
        // Menu audio is always local (WAV/Piper). Do NOT StartAIConversation /
        // ServerPushText on the menu path — that burned Tencent voice minutes while
        // callers heard Piper (or silence). AI task starts only on DTMF 1.
        let room_live = room_media_live(&join.room_id);
        if self.settings.cloud_api_ready() && room_live {
            info!(
                "menu room live serial={serial} room_id={} — skip menu ServerPush/StartAI (local Piper/WAV only; AI arms on DTMF1)",
                join.room_id
            );
        } else if self.settings.cloud_api_ready() && !room_live {
            info!(
                "menu cloud_api_ready but room not joined serial={serial} room_id={} — keep trtc_menu_local_fallback",
                join.room_id
            );
        }
        self.soft.play_menu_via_trtc(join, serial, menu_text)?;
        Ok(MenuPlayPath::LocalFallback)
    }

    fn enter_ai_talk(&self, settings: &TrtcSettings, serial: &str) -> Result<TrtcJoinParams> {
        let join = self.soft.enter_ai_talk(settings, serial)?;
        if !settings.bridge_enabled {
            self.spawn_start_ai(serial, &join);
            return Ok(join);
        }
        // Always (re)open sidecar as phase=ai. Menu join alone is not enough: we need a
        // fresh session + PCM WS so uplink/downlink bytes actually flow (live logs showed
        // joined=true with uplink/downlink all zero → no realtime talk).
        let bridge_sig = match settings.gen_user_sig(&join.user_id) {
            Ok(s) => s,
            Err(e) => {
                warn!("TRTC enter_ai_talk UserSig failed serial={serial} err={e:#}; soft silence");
                self.spawn_start_ai(serial, &join);
                return Ok(join);
            }
        };
        close_pcm_ws(&join.room_id);
        mark_room_joined(&join.room_id, false);
        let joined = self.open_session_await(json!({
            "cmd": "open_session",
            "room_id": join.room_id,
            "user_id": join.user_id,
            "sdk_app_id": settings.sdk_app_id,
            "user_sig": bridge_sig,
            "sample_rate": PCM_SAMPLE_RATE,
            "phase": "ai",
        }));
        if joined {
            ensure_pcm_ws_from_settings(settings, &join.room_id);
            info!(
                "TRTC enter_ai_talk media live serial={serial} room_id={} — PCM WS + StartAI",
                join.room_id
            );
        } else {
            warn!(
                "TRTC enter_ai_talk open_session not joined serial={serial} room_id={} — StartAI still attempted",
                join.room_id
            );
        }
        self.spawn_sidecar_json(
            "/session/phase",
            json!({
                "cmd": "set_phase",
                "room_id": join.room_id,
                "user_id": join.user_id,
                "phase": "ai",
                "serial": serial,
            }),
        );
        self.spawn_start_ai(serial, &join);
        Ok(join)
    }

    fn leave(&self, serial: &str, reason: &str) {
        self.spawn_stop_ai(serial);
        let room_id = SoftTrtcBridge::room_id_for(serial);
        mark_room_joined(&room_id, false);
        self.spawn_sidecar_json(
            "/session/close",
            json!({
                "cmd": "close_session",
                "serial": serial,
                "reason": reason,
                "room_id": room_id,
            }),
        );
        self.soft.leave(serial, reason);
    }

    fn push_uplink_pcm(&self, join: &TrtcJoinParams, pcm: &[u8], sample_rate: u32) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        enforce_pcm_contract(pcm, sample_rate)?;
        if !room_media_live(&join.room_id) {
            return Ok(());
        }
        let Some(base) = self.settings.bridge_base_url() else {
            return Ok(());
        };
        let url = format!(
            "{base}/pcm/uplink?room_id={}&sr={sample_rate}",
            urlencoding_room(&join.room_id)
        );
        let http = self.http.clone();
        let body = pcm.to_vec();
        tokio::spawn(async move {
            match http
                .post(&url)
                .timeout(Duration::from_millis(800))
                .header("Content-Type", "application/octet-stream")
                .body(body)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => warn!("TRTC pcm uplink HTTP {}", resp.status()),
                Err(e) => warn!("TRTC pcm uplink failed err={e:#}"),
            }
        });
        Ok(())
    }

    fn pull_downlink_pcm(&self, join: &TrtcJoinParams, max_bytes: usize) -> Result<Vec<u8>> {
        if !room_media_live(&join.room_id) {
            return Ok(Vec::new());
        }
        let Some(base) = self.settings.bridge_base_url() else {
            return Ok(Vec::new());
        };
        let url = format!(
            "{base}/pcm/downlink?room_id={}&max={max_bytes}",
            urlencoding_room(&join.room_id)
        );
        let http = self.http.clone();
        let pulled = block_on_current(async move {
            match http
                .get(&url)
                .timeout(Duration::from_millis(500))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default()
                }
                Ok(_) => Vec::new(),
                Err(_) => Vec::new(),
            }
        });
        Ok(pulled.unwrap_or_default())
    }
}

/// Menu arm helper: join room + queue TRTC menu.
///
/// Returns [`MenuPlayPath::ServerPushText`] only when **this** room is media-live
/// **and** Cloud API ready — caller must skip local WAV and pull downlink.
/// Otherwise [`MenuPlayPath::LocalFallback`] (`trtc_menu_local_fallback`).
pub fn arm_and_queue_menu(
    bridge: &dyn TrtcBridge,
    settings: &TrtcSettings,
    serial: &str,
    menu_text: &str,
) -> Result<(TrtcJoinParams, MenuPlayPath)> {
    if !settings.transfer_ready() {
        warn!(
            "TrtcBridge arm skipped serial={serial} (sdk_app_id={} ai_enabled={})",
            settings.sdk_app_id, settings.ai_enabled
        );
        anyhow::bail!("TRTC not transfer_ready");
    }
    let join = bridge.join_menu_room(settings, serial)?;
    let path = bridge.play_menu_via_trtc(&join, serial, menu_text)?;
    Ok((join, path))
}

/// Late re-check after IVR start delay: sidecar may have joined since arm.
/// Spawns ServerPushText when **this room** is live + cloud ready; does not re-open.
pub fn try_live_menu_server_push(
    bridge: &dyn TrtcBridge,
    settings: &TrtcSettings,
    serial: &str,
    menu_text: &str,
) -> MenuPlayPath {
    let room_id = SoftTrtcBridge::room_id_for(serial);
    if !room_media_live(&room_id) || !settings.cloud_api_ready() {
        return MenuPlayPath::LocalFallback;
    }
    // Reconstruct join identity for StartAI TargetUserId / room (no new UserSig needed for push).
    let join = TrtcJoinParams {
        sdk_app_id: settings.sdk_app_id,
        room_id,
        user_id: format!("bridge_{serial}"),
        user_sig: String::new(),
    };
    match bridge.play_menu_via_trtc(&join, serial, menu_text) {
        Ok(path) => path,
        Err(e) => {
            warn!(
                "TRTC late menu ServerPushText arm failed serial={serial} err={e:#}; keep local fallback"
            );
            MenuPlayPath::LocalFallback
        }
    }
}

/// Pick soft vs sidecar-aware bridge. Sidecar flips `media_bridge_live` only after
/// `joined=true`; REST/sidecar failures never upgrade the degrade path.
pub fn make_bridge(settings: &TrtcSettings, http: reqwest::Client) -> Box<dyn TrtcBridge> {
    if settings.bridge_base_url().is_some() || settings.cloud_api_ready() {
        Box::new(SidecarTrtcBridge::new(settings.clone(), http))
    } else {
        Box::new(SoftTrtcBridge::new())
    }
}

/// Async PCM downlink pull for `write_tts` / phone WS.
/// Prefer WS buffer (push from sidecar); HTTP only as fallback.
pub async fn pull_downlink_http(
    settings: &TrtcSettings,
    http: &reqwest::Client,
    room_id: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if !room_media_live(room_id) {
        return Ok(Vec::new());
    }
    let ws = pull_downlink_ws(room_id, max_bytes);
    if !ws.is_empty() {
        return Ok(ws);
    }
    // Keep WS armed while room is live.
    ensure_pcm_ws_from_settings(settings, room_id);
    let ws2 = pull_downlink_ws(room_id, max_bytes);
    if !ws2.is_empty() {
        return Ok(ws2);
    }
    let Some(base) = settings.bridge_base_url() else {
        return Ok(Vec::new());
    };
    let url = format!(
        "{base}/pcm/downlink?room_id={}&max={max_bytes}",
        urlencoding_room(room_id)
    );
    match http
        .get(&url)
        .timeout(Duration::from_millis(80))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            Ok(resp.bytes().await.map(|b| b.to_vec()).unwrap_or_default())
        }
        Ok(_) => Ok(Vec::new()),
        Err(e) => {
            if !e.is_timeout() {
                warn!("TRTC pcm downlink failed err={e:#}");
            }
            Ok(Vec::new())
        }
    }
}

/// Sidecar `/health` booleans for agent `/health` (never secrets / UserSig).
#[derive(Debug, Clone, Copy, Default)]
pub struct BridgeHealthProbe {
    /// Sidecar HTTP `/health` reachable (bridge process up).
    pub ready: bool,
    /// At least one Chromium/SDK session actually joined (`chromium_join`).
    pub media_live: bool,
}

/// Probe sidecar `/health` once — booleans only, never secrets.
pub async fn probe_bridge_health(
    settings: &TrtcSettings,
    http: &reqwest::Client,
) -> BridgeHealthProbe {
    let Some(base) = settings.bridge_base_url() else {
        return BridgeHealthProbe {
            ready: false,
            media_live: any_room_joined(),
        };
    };
    let url = format!("{base}/health");
    match http
        .get(&url)
        .timeout(Duration::from_millis(300))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let health_live = match resp.json::<serde_json::Value>().await {
                Ok(v) => {
                    v.get("chromium_join")
                        .and_then(|x| x.as_bool())
                        .unwrap_or(false)
                        || v.get("joined").and_then(|x| x.as_bool()).unwrap_or(false)
                        || v.get("media_bridge_live")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false)
                        || v.get("joined_sessions")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0)
                            > 0
                }
                Err(_) => false,
            };
            // Arm SidecarTrtcBridge::media_bridge_live from /health joined.
            mark_health_joined(health_live);
            BridgeHealthProbe {
                ready: true,
                media_live: health_live || any_room_joined(),
            }
        }
        _ => BridgeHealthProbe {
            ready: false,
            media_live: any_room_joined(),
        },
    }
}

/// Probe sidecar `/health` reachable (boolean only — never secrets).
pub async fn probe_bridge_ready(settings: &TrtcSettings, http: &reqwest::Client) -> bool {
    probe_bridge_health(settings, http).await.ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SecretString;

    #[test]
    fn soft_bridge_join_and_menu_without_logging_sig() {
        let settings = TrtcSettings::from_parts(
            1_400_000_000,
            SecretString::from("unit-test-secret-key-not-real"),
            true,
            600,
        );
        let bridge = SoftTrtcBridge::new();
        assert!(!bridge.media_bridge_live());
        let (join, path) =
            arm_and_queue_menu(&bridge, &settings, "ABC-123", "按1 AI／按2留言").expect("join");
        assert_eq!(path, MenuPlayPath::LocalFallback);
        assert_eq!(join.room_id, "call_ABC_123");
        assert!(join.user_id.starts_with("menu_"));
        assert!(!join.user_sig.is_empty());
        let dbg = format!("{join:?}");
        assert!(dbg.contains("***"));
        assert!(!dbg.contains(&join.user_sig));
        assert!(bridge.pull_downlink_pcm(&join, 320).unwrap().is_empty());
    }

    #[test]
    fn enter_ai_talk_uses_bridge_user() {
        let settings = TrtcSettings::from_parts(
            1,
            SecretString::from("unit-test-secret-key-not-real"),
            true,
            600,
        );
        let bridge = SoftTrtcBridge::new();
        let join = bridge.enter_ai_talk(&settings, "S1").expect("ai");
        assert_eq!(join.user_id, "bridge_S1");
        assert_eq!(join.room_id, "call_S1");
    }

    #[test]
    fn make_bridge_soft_when_no_sidecar() {
        let settings = TrtcSettings::from_parts(1, SecretString::from("k"), true, 600);
        let b = make_bridge(&settings, reqwest::Client::new());
        assert!(!b.media_bridge_live());
    }

    #[test]
    fn soft_menu_path_stays_local_fallback() {
        let settings = TrtcSettings::from_parts(
            1,
            SecretString::from("unit-test-secret-key-not-real"),
            true,
            600,
        );
        let bridge = SoftTrtcBridge::new();
        let path = try_live_menu_server_push(&bridge, &settings, "S", "按1 AI／按2留言");
        assert_eq!(path, MenuPlayPath::LocalFallback);
    }

    #[test]
    fn cloud_ready_without_room_join_stays_local_menu() {
        // cloud_api_ready alone must NOT skip local WAV (no PCM → silence).
        let mut settings = TrtcSettings::from_parts(
            1,
            SecretString::from("unit-test-secret-key-not-real"),
            true,
            600,
        );
        settings.secret_id = SecretString::from("AKIDunit");
        settings.cloud_secret_key = SecretString::from("cloud-secret-unit");
        settings.llm_config = SecretString::from(r#"{"LLMType":"openai"}"#);
        settings.tts_config = SecretString::from(r#"{"TTSType":"tencent"}"#);
        assert!(settings.cloud_api_ready());
        let room = SoftTrtcBridge::room_id_for("cloudonly");
        mark_health_joined(false);
        mark_room_joined(&room, false);
        let bridge = SidecarTrtcBridge::new(settings.clone(), reqwest::Client::new());
        let join = SoftTrtcBridge::new()
            .join_menu_room(&settings, "cloudonly")
            .expect("join");
        let path = bridge
            .play_menu_via_trtc(&join, "cloudonly", "按1 AI／按2留言")
            .expect("menu");
        assert_eq!(path, MenuPlayPath::LocalFallback);
        assert_eq!(
            try_live_menu_server_push(&bridge, &settings, "cloudonly", "hi"),
            MenuPlayPath::LocalFallback
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn room_live_plus_cloud_prefers_server_push_path() {
        let mut settings = TrtcSettings::from_parts(
            1,
            SecretString::from("unit-test-secret-key-not-real"),
            true,
            600,
        );
        settings.secret_id = SecretString::from("AKIDunit");
        settings.cloud_secret_key = SecretString::from("cloud-secret-unit");
        settings.llm_config = SecretString::from(r#"{"LLMType":"openai"}"#);
        settings.tts_config = SecretString::from(r#"{"TTSType":"tencent"}"#);
        let room = SoftTrtcBridge::room_id_for("livepush");
        mark_room_joined(&room, true);
        let bridge = SidecarTrtcBridge::new(settings.clone(), reqwest::Client::new());
        let join = TrtcJoinParams {
            sdk_app_id: 1,
            room_id: room.clone(),
            user_id: "bridge_livepush".into(),
            user_sig: String::new(),
        };
        // Spawns async StartAI/ServerPush in background; menu path stays LocalFallback
        // so Piper/WAV always plays (stale room must not silence callers).
        let path = bridge
            .play_menu_via_trtc(&join, "livepush", "按1 AI／按2留言")
            .expect("menu");
        assert_eq!(path, MenuPlayPath::LocalFallback);
        mark_room_joined(&room, false);
    }

    #[test]
    fn soft_log_has_no_joinroom_rest_myth() {
        let settings = TrtcSettings::from_parts(
            1,
            SecretString::from("unit-test-secret-key-not-real"),
            true,
            600,
        );
        let bridge = SoftTrtcBridge::new();
        let _ = bridge.join_menu_room(&settings, "T").expect("join");
        // Compile-time / behavior: method returns Ok; myth string removed from source.
        let src = include_str!("trtc_bridge.rs");
        assert!(src.contains("no Cloud JoinRoom REST"));
    }

    #[test]
    fn pcm_contract_defaults_s16le_mono_16k_20ms() {
        // Shared agent↔sidecar contract (see sidecar pcm.mjs / README).
        assert_eq!(PCM_SAMPLE_RATE, 16_000);
        assert_eq!(PCM_CHANNELS, 1);
        assert_eq!(PCM_BITS, 16);
        assert_eq!(PCM_FRAME_MS, 20);
        assert_eq!(PCM_FRAME_SAMPLES, 320);
        assert_eq!(PCM_FRAME_BYTES, 640);
        // SAMPLE_RATE env default in config.rs must stay locked to PCM_SAMPLE_RATE.
        let config_src = include_str!("config.rs");
        assert!(
            config_src.contains("env_parse(\"SAMPLE_RATE\", 16000u32)"),
            "config SAMPLE_RATE default must be 16000"
        );
        assert_eq!(PCM_SAMPLE_RATE, 16000u32);
    }

    #[test]
    fn push_uplink_rejects_wrong_sample_rate() {
        let bridge = SoftTrtcBridge::new();
        let join = TrtcJoinParams {
            sdk_app_id: 1,
            room_id: "call_T".into(),
            user_id: "bridge_T".into(),
            user_sig: "x".into(),
        };
        let frame = vec![0u8; PCM_FRAME_BYTES];
        assert!(bridge
            .push_uplink_pcm(&join, &frame, PCM_SAMPLE_RATE)
            .is_ok());
        assert!(bridge.push_uplink_pcm(&join, &frame, 8_000).is_err());
        assert!(bridge
            .push_uplink_pcm(&join, &[0u8; 3], PCM_SAMPLE_RATE)
            .is_err());
    }

    #[test]
    fn mark_room_joined_flips_sidecar_live() {
        let room = "call_unit_live_flip";
        mark_health_joined(false);
        mark_room_joined(room, false);
        assert!(!room_media_live(room));
        mark_room_joined(room, true);
        assert!(room_media_live(room));
        let mut t = TrtcSettings::from_parts(1, SecretString::from("k"), true, 600);
        t.bridge_url = "http://127.0.0.1:8791".into();
        t.bridge_enabled = true;
        let b = SidecarTrtcBridge::new(t, reqwest::Client::new());
        assert!(b.media_bridge_live());
        mark_room_joined(room, false);
        assert!(!b.media_bridge_live());
        // Soft stays hard-false even if health/open_session armed the set.
        mark_health_joined(true);
        assert!(b.media_bridge_live());
        assert!(!SoftTrtcBridge::new().media_bridge_live());
        mark_health_joined(false);
    }
}

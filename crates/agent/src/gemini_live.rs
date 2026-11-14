//! Google Gemini Live API (WebSocket) for DTMF-1 realtime voice.
//!
//! Protocol: BidiGenerateContent over WSS (AI Studio / Gemini API key).
//! Input audio: PCM s16le mono 16 kHz. Output: PCM s16le mono 24 kHz (resampled to phone rate).
//! Never log API keys.

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

use crate::hold_music::resample_s16le_mono_pub;

const DEFAULT_MODEL: &str = "gemini-3.8-live";
const WS_BASE: &str =
    "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const SETUP_TIMEOUT: Duration = Duration::from_secs(8);
/// Require at least one Gemini PCM frame to be delivered into the media WS soon after unmute.
const FIRST_DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_DELIVERY_RETRY_AFTER: Duration = Duration::from_millis(2_500);
const KICK_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const SETUP_SEND_TIMEOUT: Duration = Duration::from_secs(2);
const API_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// Uplink chunk size to Gemini (~40 ms @ 16 kHz mono s16le).
const UPLINK_CHUNK_MS: u32 = 40;
/// After caller voice, this much silence → local activityEnd (force turn).
/// Was 320ms — Chinese mid-phrase pauses were cut → model heard fragments and
/// kept saying it could not hear. 700ms is still above that fragment cut; 1000ms left
/// Gemini waiting ~10s when local EOU never fired (quiet PSTN, no activityStart).
const LOCAL_EOU_SILENCE_MS: u32 = 700;
/// RMS threshold on 16 kHz s16le for local VAD (phone uplink). Quiet PSTN
/// peaks ~580; 220 dropped too many frames and blocked activityStart.
const LOCAL_VAD_RMS: f32 = 180.0;
/// Minimum voiced ms before we open activity (arm turn).
const LOCAL_MIN_VOICE_MS: u32 = 200;
/// Do not force EOU until the caller has spoken at least this long in the turn.
/// 450ms covers short phrases of about three syllables.
const LOCAL_MIN_TURN_VOICE_MS: u32 = 450;
/// A lone short word (「喂？」, "yes", "okay", "mm": the 200–450 ms of voice that opened
/// the activity) is a whole turn too — end it after this longer pause. Without it
/// the activity stayed open until the caller said more (or 15 s): a caller's
/// 「喂？」 (240 ms) got no answer, a second one was answered 1.8 s late,
/// their next short word never was → they hung up.
const LOCAL_SHORT_TURN_EOU_SILENCE_MS: u32 = 1_000;
/// Gemini's own turn detection (setting `gemini_server_vad`): silence after the
/// caller's speech before the server ends the turn (Google default ≈ 800 ms).
const SERVER_VAD_SILENCE_MS: u32 = 700;
/// Barge-in (echo-free phones): this much caller voice over our playback stops
/// it — a short backchannel ("mm", "okay", "yeah, right") does not. Until then
/// Gemini does not hear them either: its own VAD cut the reply at the first
/// sound (on one call 4 replies in 55 s, one after 0.35 s by a quiet "okay").
/// 480 ms still cut a reply on a two-word backchannel; people talking over us for
/// real go on for seconds.
const BARGE_IN_VOICE_MS: u32 = 800;
/// The caller took the floor but our reply is all generated and this close to
/// its end: let it finish instead of chopping the last words.
const BARGE_IN_FINISH_MS: u64 = 1_000;
/// Our audio is still in the caller's ear this long after the phone played its
/// last frame (AudioTrack buffer + the cellular leg, both ways).
const OVERLAP_AUDIBLE_TAIL: Duration = Duration::from_millis(400);
/// Caller audio held back while our reply is audible: this much before their
/// voice starts…
const OVERLAP_PREROLL_MS: u32 = 300;
/// …and at most this much in all.
const OVERLAP_HOLD_MS: u32 = 2_000;
/// A short word over the end of our reply ("okay", "right", "bye") is their answer:
/// passed on to Gemini if it came this close to the end; earlier ones are dropped.
const OVERLAP_TAIL_ANSWER: Duration = Duration::from_millis(1_200);
/// While the caller (or the carrier's international-call prompt) is talking the
/// model cannot answer, so the first-audio watchdog pauses — up to this much.
const FIRST_DELIVERY_CALLER_GRACE_MAX: Duration = Duration::from_secs(12);
/// Caller counts as still talking this long after their last voiced frame.
const FIRST_DELIVERY_CALLER_HOLD: Duration = Duration::from_millis(1_200);
/// Held (pre-pickup) opening that came back text-only: ask again at most this
/// often before the dialogue opens (each try ≈ 0.5 s; a ring lasts ~3 s).
const WARM_TEXT_ONLY_REKICKS: u32 = 3;
/// Calls from abroad: the Taiwan carrier plays a recorded warning
/// (「這是國際電話，請注意小心詐騙」) to us ~0.3–1.9 s after pickup (~6 s long).
/// The held opening waits this long for
/// it: line voice → the opening is dropped and the model's 「喂？」 after the
/// prompt is the only one; quiet → the opening plays. (A greeting, then the
/// prompt, then 「喂？」 again sounds odd.)
const INTL_OPENING_HOLD: Duration = Duration::from_millis(2_500);
/// How long the line voice that ends an international hold counts as the carrier's
/// prompt (~6 s long), whatever its transcript says: a foreign caller's speech
/// hints (en first, or none) can turn the Chinese prompt into other words, and the
/// prompt must never count as the caller (no hang-up on it).
const CARRIER_PROMPT_SPAN: Duration = Duration::from_millis(7_500);

/// The caller has only played the carrier's prompt so far: its words are in the
/// transcript, or all line voice so far came within the prompt's span.
fn only_carrier_prompt_so_far(
    caller_text_all: &str,
    prompt_until: Option<tokio::time::Instant>,
    last_caller_voice_at: Option<tokio::time::Instant>,
) -> bool {
    looks_like_carrier_intl_prompt(caller_text_all)
        || prompt_until.is_some_and(|until| last_caller_voice_at.map_or(true, |t| t <= until))
}
/// Before activityStart, do not zero voiced_ms on every sub-threshold frame.
/// Only a run of this much continuous silence resets the accumulator (quiet
/// PSTN dips). Instant reset never reached LOCAL_MIN_VOICE_MS → no
/// activityStart → no activityEnd → Gemini waits ~10s.
const LOCAL_PRE_ACTIVITY_HANGOVER_MS: u32 = 350;
/// Target RMS for uplink AGC into Gemini (s16le full-scale ~32768).
const UPLINK_TARGET_RMS: f32 = 4500.0;
/// Real caller speech arrives at ~100–500 RMS (measured) with a
/// near-digital-silence floor; 16× is safe now that our own echo is gated to
/// zeros upstream (main.rs `uplink_is_echo`) instead of being boosted.
const UPLINK_GAIN_MAX: f32 = 16.0;
/// Raw (pre-AGC) minimum for a voiced frame (= where the AGC starts lifting).
/// Phones keep the earpiece at min so callers arrive at ~100–500
/// RMS over a ~1 RMS digital floor; at max earpiece volume line noise of
/// ~20–200 RMS read as endless "voice" — the floor ratio below covers that.
const LOCAL_VAD_RAW_MIN_RMS: f32 = 40.0;
/// Voiced frames must also sit this far above the line-noise floor (≈ +9.5 dB).
const LOCAL_VAD_FLOOR_RATIO: f32 = 3.0;
/// Cap for the floor-derived threshold so a mis-tracked floor cannot deafen VAD
/// (real speech at max capture volume is ~3000–6000 RMS).
const LOCAL_VAD_RAW_MAX_THRESHOLD: f32 = 1_500.0;
/// Noise floor = min raw RMS over this much recent (non-gated) uplink.
const LOCAL_NOISE_FLOOR_WINDOW_MS: u32 = 2_400;
/// Floor is trusted only after this much history (else just the raw minimum).
const LOCAL_NOISE_FLOOR_MIN_HISTORY_MS: u32 = 600;
/// Uplink held while no activity is open and sent right after activityStart:
/// VAD needs ~180–480 ms of voice to confirm, and with manual activity Gemini
/// only hears what follows activityStart — the first syllable used to be lost.
const LOCAL_PREROLL_MS: u32 = 600;
/// Force activityEnd after this long in one activity (noise that never drops
/// under the VAD gate must not leave Gemini waiting forever).
const LOCAL_MAX_ACTIVITY_MS: u32 = 15_000;

/// Min-statistics line-noise tracker over recent uplink frames.
/// Gated (all-zero) frames are skipped by the caller.
#[derive(Default)]
struct NoiseFloor {
    hist: std::collections::VecDeque<(f32, u32)>,
    total_ms: u32,
}

impl NoiseFloor {
    fn push(&mut self, rms: f32, frame_ms: u32) {
        self.hist.push_back((rms, frame_ms));
        self.total_ms = self.total_ms.saturating_add(frame_ms);
        while self.total_ms > LOCAL_NOISE_FLOOR_WINDOW_MS && self.hist.len() > 1 {
            if let Some((_, ms)) = self.hist.pop_front() {
                self.total_ms = self.total_ms.saturating_sub(ms);
            }
        }
    }

    fn floor(&self) -> f32 {
        if self.total_ms < LOCAL_NOISE_FLOOR_MIN_HISTORY_MS {
            return 0.0;
        }
        self.hist
            .iter()
            .map(|(r, _)| *r)
            .fold(f32::INFINITY, f32::min)
            .min(f32::MAX)
    }

    /// Raw RMS a frame needs to count as caller voice.
    fn voice_threshold(&self) -> f32 {
        (self.floor() * LOCAL_VAD_FLOOR_RATIO)
            .min(LOCAL_VAD_RAW_MAX_THRESHOLD)
            .max(LOCAL_VAD_RAW_MIN_RMS)
    }
}

/// Append to the pre-activity hold buffer, keeping only the newest `cap` bytes.
fn push_preroll(buf: &mut Vec<u8>, pcm: &[u8], cap: usize) {
    buf.extend_from_slice(pcm);
    if buf.len() > cap {
        let drop = (buf.len() - cap) & !1;
        buf.drain(..drop);
    }
}

/// Echo-free line: what the caller says while our reply is still in their ear.
/// It is kept from Gemini (which gets silence) until it is clearly more than a
/// backchannel — people do not stop mid-word for an "mm".
#[derive(Default)]
struct OverlapHold {
    held: Vec<u8>,
    /// Current run of voice and the quiet since its last voiced frame.
    run_ms: u32,
    quiet_ms: u32,
    /// Longest run since our reply started.
    best_ms: u32,
    voice_at: Option<tokio::time::Instant>,
}

/// What became of the held audio once our reply was over.
#[derive(Debug, PartialEq)]
enum OverlapEnd {
    Nothing,
    /// A word on the end of our reply is the caller's answer: `held` goes to
    /// Gemini; `open` = (voiced, quiet) ms when they are still talking.
    Answer { held: Vec<u8>, voiced_ms: u32, open: Option<(u32, u32)> },
    /// An earlier backchannel was not meant for us: dropped.
    Backchannel { voiced_ms: u32 },
}

impl OverlapHold {
    fn reset(&mut self) {
        self.run_ms = 0;
        self.quiet_ms = 0;
        self.best_ms = 0;
        self.voice_at = None;
    }

    /// One caller frame (16 kHz s16le) over our reply. `Some((held, voiced_ms))`
    /// once they have talked [`BARGE_IN_VOICE_MS`] in a row: they took the floor
    /// and Gemini gets everything from just before their first syllable.
    fn push(
        &mut self,
        pcm: &[u8],
        voice: bool,
        frame_ms: u32,
        now: tokio::time::Instant,
    ) -> Option<(Vec<u8>, u32)> {
        let bytes = |ms: u32| (16_000usize * ms as usize / 1000) * 2;
        let keep = self
            .voice_at
            .is_some_and(|t| now.saturating_duration_since(t) <= OVERLAP_TAIL_ANSWER);
        push_preroll(
            &mut self.held,
            pcm,
            bytes(if keep || voice { OVERLAP_HOLD_MS } else { OVERLAP_PREROLL_MS }),
        );
        if voice {
            self.run_ms = self.run_ms.saturating_add(frame_ms);
            self.best_ms = self.best_ms.max(self.run_ms);
            self.quiet_ms = 0;
            self.voice_at = Some(now);
        } else {
            self.quiet_ms = self.quiet_ms.saturating_add(frame_ms);
            if self.quiet_ms >= LOCAL_PRE_ACTIVITY_HANGOVER_MS {
                self.run_ms = 0;
            }
        }
        if self.run_ms < BARGE_IN_VOICE_MS {
            return None;
        }
        let voiced_ms = self.run_ms;
        self.reset();
        Some((std::mem::take(&mut self.held), voiced_ms))
    }

    /// Our reply is over (or the caller's turn is open anyway).
    fn end(&mut self, now: tokio::time::Instant) -> OverlapEnd {
        let held = std::mem::take(&mut self.held);
        let recent = self
            .voice_at
            .is_some_and(|t| now.saturating_duration_since(t) <= OVERLAP_TAIL_ANSWER);
        let out = if self.best_ms < LOCAL_MIN_VOICE_MS {
            OverlapEnd::Nothing
        } else if recent {
            let open = (self.quiet_ms < LOCAL_PRE_ACTIVITY_HANGOVER_MS)
                .then_some((self.run_ms, self.quiet_ms));
            OverlapEnd::Answer { held, voiced_ms: self.best_ms, open }
        } else {
            OverlapEnd::Backchannel { voiced_ms: self.best_ms }
        };
        self.reset();
        out
    }

    fn is_idle(&self) -> bool {
        self.held.is_empty() && self.best_ms == 0
    }
}

pub fn default_model() -> &'static str {
    DEFAULT_MODEL
}

/// Default prebuilt voice (Gemini Live speechConfig). Accent is mainly from
/// system prompt language; voice name only changes timbre (Aoede/Kore/Puck/…).
pub fn default_voice_name() -> &'static str {
    "Charon"
}

/// One line of the preview must come back within this.
const PREVIEW_LINE_TIMEOUT: Duration = Duration::from_secs(15);

/// Speak `lines` in `voice` on one short Live session (a turn per line), at
/// `phone_sr` and with a call's downlink processing — the settings page's preview.
pub async fn speak_lines(
    lang: crate::locale::Lang,
    api_key: &str,
    model: &str,
    thinking_level: &str,
    voice: &str,
    lines: &[&str],
    phone_sr: u32,
) -> Result<Vec<Vec<u8>>> {
    speak_lines_as(lang, &crate::prompts::pack(lang).preview_system, api_key, model, thinking_level, voice, lines, phone_sr).await
}

/// [`speak_lines`] with another speaker (`system`), e.g. the announcement voice.
#[allow(clippy::too_many_arguments)]
pub async fn speak_lines_as(
    lang: crate::locale::Lang,
    system: &str,
    api_key: &str,
    model: &str,
    thinking_level: &str,
    voice: &str,
    lines: &[&str],
    phone_sr: u32,
) -> Result<Vec<Vec<u8>>> {
    Ok(speak_lines_heard(lang, system, api_key, model, thinking_level, voice, lines, phone_sr, false)
        .await?
        .into_iter()
        .map(|(pcm, _)| pcm)
        .collect())
}

/// [`speak_lines_as`], each line with what Gemini says it spoke (its output
/// transcription) when `transcribe` — to check a recording says the line.
#[allow(clippy::too_many_arguments)]
pub async fn speak_lines_heard(
    lang: crate::locale::Lang,
    system: &str,
    api_key: &str,
    model: &str,
    thinking_level: &str,
    voice: &str,
    lines: &[&str],
    phone_sr: u32,
    transcribe: bool,
) -> Result<Vec<(Vec<u8>, String)>> {
    let key = api_key.trim();
    if key.is_empty() {
        bail!("Gemini API key is not set");
    }
    let model = match model.trim() {
        "" => DEFAULT_MODEL,
        m => m,
    };
    let model_path = if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    };
    // One retry: right after an agent restart the first connects failed with
    // "Network is unreachable".
    let mut connected = None;
    for attempt in 0..2 {
        match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(ws_url(key))).await {
            Ok(Ok((ws, _))) => {
                connected = Some(ws);
                break;
            }
            Ok(Err(e)) if attempt == 1 => bail!("cannot reach Gemini: {e:#}"),
            Err(_) if attempt == 1 => bail!("connecting to Gemini timed out"),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    let Some(ws) = connected else { bail!("cannot reach Gemini") };
    let (mut sink, mut stream) = ws.split();
    let mut setup = json!({
        "setup": {
            "model": model_path,
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": { "voiceConfig": { "prebuiltVoiceConfig": { "voiceName": voice } } }
            },
            "systemInstruction": { "parts": [{ "text": system }] }
        }
    });
    if let Some(level) = live_thinking_level(model, thinking_level) {
        if let Some(gc) = setup
            .pointer_mut("/setup/generationConfig")
            .and_then(|x| x.as_object_mut())
        {
            gc.insert("thinkingConfig".into(), json!({ "thinkingLevel": level }));
        }
    }
    if transcribe {
        if let Some(s) = setup.pointer_mut("/setup").and_then(|x| x.as_object_mut()) {
            s.insert(
                "outputAudioTranscription".into(),
                json!({ "languageCodes": [crate::locale::phrases(lang).stt] }),
            );
        }
    }
    send_text_before_deadline(
        &mut sink,
        setup.to_string(),
        tokio::time::Instant::now() + SETUP_SEND_TIMEOUT,
        "gemini preview setup send",
    )
    .await?;
    let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(left, stream.next()).await {
            Ok(Some(Ok(WsMessage::Close(frame)))) => {
                bail!("Gemini refused this setup (voice {voice}, model {model}): {frame:?}")
            }
            Ok(Some(Ok(msg))) => {
                let Some(t) = text_from_ws(msg) else { continue };
                if t.contains("setupComplete") || t.contains("setup_complete") {
                    break;
                }
                if let Some(err) = serde_json::from_str::<Value>(&t).ok().and_then(|v| v.get("error").cloned()) {
                    bail!("Gemini setup error: {err}");
                }
            }
            Ok(Some(Err(e))) => bail!("Gemini connection error: {e}"),
            Ok(None) => bail!("Gemini closed the connection"),
            Err(_) => bail!("Gemini setup timed out"),
        }
    }
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let mut pcm: Vec<u8> = Vec::new();
        let mut heard = String::new();
        // A turn sometimes comes back as a transcript only (no audio): ask again.
        for _ in 0..2 {
            heard.clear();
            let turn = json!({
                "clientContent": {
                    "turns": [{ "role": "user", "parts": [{ "text": crate::prompts::pack(lang).read_line.replace("{line}", line) }] }],
                    "turnComplete": true
                }
            });
            send_text_before_deadline(
                &mut sink,
                turn.to_string(),
                tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                "gemini preview line send",
            )
            .await?;
            let mut deadline = tokio::time::Instant::now() + PREVIEW_LINE_TIMEOUT;
            let mut spoken = false;
            loop {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                match tokio::time::timeout(left, stream.next()).await {
                    Ok(Some(Ok(msg))) => {
                        let Some(v) = text_from_ws(msg).and_then(|t| serde_json::from_str::<Value>(&t).ok())
                        else {
                            continue;
                        };
                        if let Some(t) = output_transcription_text(&v) {
                            heard.push_str(t);
                        }
                        handle_server_msg(&v, phone_sr, &mut |c: Vec<u8>| pcm.extend_from_slice(&c));
                        if v.pointer("/serverContent/turnComplete").and_then(|x| x.as_bool()) == Some(true) {
                            if !transcribe {
                                break;
                            }
                            // The transcript of a short line can come after the turn ends.
                            if !spoken {
                                spoken = true;
                                deadline = tokio::time::Instant::now() + Duration::from_millis(1_500);
                            }
                        }
                    }
                    Ok(Some(Err(e))) => bail!("Gemini connection error: {e}"),
                    Ok(None) => bail!("Gemini closed the connection"),
                    Err(_) => break,
                }
            }
            if !pcm.is_empty() {
                break;
            }
        }
        if pcm.is_empty() {
            bail!("Gemini produced no audio with voice {voice}");
        }
        out.push((pcm, heard));
    }
    let _ = sink.send(WsMessage::Close(None)).await;
    Ok(out)
}

/// Spoken opening styles rotated per call (kick picks one; do not recite as a script).
const GREET_KICK_STYLES: &[&str] = &[
    "嗨，我是這邊的 AI 助理，您說，我幫您記下來轉達。",
    "你好呀，我是 AI 助理。有要留話或轉達的，直接說就好。",
    "喂，我來聽。我是 AI 助理，要幫忙轉達什麼？",
    "哈囉，我是 AI 助理。您請說，我幫您轉達。",
    "你好，我是 AI 助理。您請說事由就好，我幫您記下來轉達。",
    "嗨嗨，我是 AI 助理，人現在不在，我可以幫您轉達。",
];

fn greets_kick_style_index(seed: u128) -> usize {
    (seed as usize) % GREET_KICK_STYLES.len()
}

pub const OWNER_STATUS_MAX_CHARS: usize = 400;
pub const OWNER_STATUS_BEGIN: &str = "【目前狀態（必須先說）】";
const OWNER_STATUS_CONTENT_PREFIX: &str = "內容：";
const OWNER_STATUS_CONTENT_END: &str = "說完再用一兩句請對方說要轉達什麼。";

/// Strip controls and cap length for the desk-status note shown in greetings.
pub fn sanitize_owner_status(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    cleaned.trim().chars().take(OWNER_STATUS_MAX_CHARS).collect()
}

/// Rewrite stored/default opening rules that tell the model to skip or paraphrase.
fn neutralize_opening_conflicts(system: &str) -> String {
    system
        .replace(
            "不要念稿。",
            "不要念客服稿；但目前狀態原文一定要講。",
        )
        .replace(
            "即使內部指示要立刻問好，也要自己換說法，不要照唸罐頭句。",
            "即使內部指示要立刻問好，也要自己換說法，不要照唸罐頭句。若指示含目前狀態原文，必須講出原文，不准省略。",
        )
        .replace(
            "不要逐字背例句。若對方已直接說事由可跳過開場。",
            "開場例句可換措辭，但目前狀態原文不准省略。即使對方已經開口，仍必須先講完現況再回應。",
        )
        .replace(
            "1）開場後聽對方說（若對方已直接說事由可跳過開場）。",
            "1）開場第一句必須先講目前狀態原文，再聽對方說。",
        )
        .replace(
            "開場對方只說「喂／你好」時：只問好，不要說再見、不要空轉達。",
            "開場對方只說「喂／你好」時：仍要先講目前狀態，再請對方留話；不要說再見、不要空轉達。",
        )
        .replace(
            "若對方已直接說事由可跳過開場。",
            "即使對方已直接說事由，仍必須先講完目前狀態再回應。",
        )
}

/// Prepend a hard "current status" block (「目前狀態」) and neutralize skip/paraphrase opening rules.
pub fn with_owner_status(system: &str, enabled: bool, text: &str) -> String {
    let status = sanitize_owner_status(text);
    if !enabled || status.is_empty() {
        return system.to_string();
    }
    let base = neutralize_opening_conflicts(system);
    format!(
        "{OWNER_STATUS_BEGIN}\
這通電話最高優先：你一開口的第一句就必須親口說出下面原文\
（前後可加「嗨／我是 AI 助理」，但原文詞必須出現）：\
{OWNER_STATUS_CONTENT_PREFIX}{status}\n\
{OWNER_STATUS_CONTENT_END}\
不准只問好、不准等對方先問、不准說不知道、不准用別的句子代替這段。\
不要唸「目前狀態」四個字。講完現況再用半句請對方留話或轉達。\
即使對方已經在說話，也要先把這段講完再回應。\n\n\
{base}"
    )
}

/// Pull the desk-status note back out of a system prompt (empty if unset).
pub fn extract_owner_status(system: &str) -> String {
    let Some((_, after)) = system.split_once(OWNER_STATUS_CONTENT_PREFIX) else {
        return String::new();
    };
    let body = after
        .split_once(OWNER_STATUS_CONTENT_END)
        .map(|(body, _)| body)
        .unwrap_or(after);
    sanitize_owner_status(body)
}

fn greets_kick_user_text(style_idx: usize) -> String {
    greets_kick_user_text_for(style_idx, "")
}

fn greets_kick_user_text_for(style_idx: usize, owner_status: &str) -> String {
    let sample = GREET_KICK_STYLES[style_idx % GREET_KICK_STYLES.len()];
    let text = format!(
        "來電已接通。對方剛聽完時段轉接錄音，不要重複「轉接專人／請稍後」。\
請立刻用一句自然有溫度的台灣華語開場（一兩句、像真人接電話）。\
本通請用接近這個感覺、但不要逐字照唸：「{sample}」。\
禁止固定唸「我是AI助手／有什麼要轉達」或同一句罐頭開場。\
可自然表明你是 AI 助理、願意幫忙轉達或聽事由。不要長篇自我介紹。\
不要問找哪位、找誰、是不是王先生；對方不知道名字也可以直接留話。\
此回合只開場，絕對不要說再見／掰掰／感謝來電，也不要空轉達。不要自稱主人助理，不要問要不要回電。"
    );
    let status = sanitize_owner_status(owner_status);
    if status.is_empty() {
        return text;
    }
    format!(
        "【系統指令·立刻開口·這不是來電者講的話】\
來電已接通。對方剛聽完時段轉接錄音，不要重複「轉接專人／請稍後」。\
你現在必須立刻用嘴巴講開場。第一句話就必須包含這段原文，關鍵詞一個都不能少：「{status}」。\
不准省略，不准等對方先問。\
完整感覺接近：「嗨，我是 AI 助理。{status}。還有要轉達的直接說。」不要只問好就停。\
禁止跳過現況。禁止先問對方有什麼事再講現況。禁止等對方說話。\
不要問找哪位、找誰、是不是王先生。此回合只開場，絕對不要說再見／掰掰／感謝來電，也不要空轉達。不要自稱主人助理。"
    )
}

fn status_retry_kick_client_content(owner_status: &str) -> String {
    let status = sanitize_owner_status(owner_status);
    json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{
                    "text": format!(
                        "【系統指令·補講現況】你剛剛開場沒有把現況講清楚。立刻再講一句，\
第一個詞開始就要包含這段原文：「{status}」。不准再只問好。講完再用半句請對方留話。不要說再見。"
                    )
                }]
            }],
            "turnComplete": true
        }
    })
    .to_string()
}

/// True when output STT likely spoke the desk-status note (not a generic hello).
pub fn output_covers_owner_status(text: &str, status: &str) -> bool {
    let status = sanitize_owner_status(status);
    if status.is_empty() {
        return true;
    }
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let s: String = status.chars().filter(|c| !c.is_whitespace()).collect();
    if s.chars().count() >= 4 && t.contains(&s) {
        return true;
    }
    let s_chars: Vec<char> = s.chars().collect();
    let window_hits = |width: usize| -> usize {
        if s_chars.len() < width {
            return 0;
        }
        let mut hits = 0usize;
        let mut i = 0;
        while i + width <= s_chars.len() {
            let needle: String = s_chars[i..i + width].iter().collect();
            if t.contains(&needle) {
                hits += 1;
            }
            i += 1;
        }
        hits
    };
    if window_hits(4) >= 2 {
        return true;
    }
    // 「外送桌」is 3 chars; require two hits so「如果是」alone is not enough.
    window_hits(3) >= 2
}

/// First-turn kick after call connects (warm pre-gen or cold post-unmute).
fn greets_kick_client_content() -> String {
    greets_kick_client_content_for("")
}

fn greets_kick_client_content_for(owner_status: &str) -> String {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{
                    "text": greets_kick_user_text_for(greets_kick_style_index(seed), owner_status)
                }]
            }],
            "turnComplete": true
        }
    })
    .to_string()
}

pub fn default_system_prompt() -> &'static str {
    // Keep short: Live model follows system text tightly.
    // Product: one topic-recap relay sentence before the goodbye (not a forced yes/no check).
    // Taiwan Mandarin / Traditional Chinese: accent via system/kick language (no speech-config field).
    "你是電話上的 AI 助理，專門幫來電者「轉達／記事」。不是客服，不要做問卷調查。\
自稱「AI 助理」，不要說「主人的助理」或僕人話術。語氣溫暖、口語、像真人接電話，不要念稿。\
一律使用台灣華語口語（繁體中文用詞：例如「一下、好的、稍等、謝謝」），自然清楚，不要中國大陸腔常用詞（例如「回撥、贵姓」）。\
每次只回一兩句。聽完對方整件事再說，不要搶話。\
\
【開場（每通換措辭）】\
對方剛聽完時段轉接錄音，不要重複「轉接專人／請稍後」。\
一兩句、有溫度即可；禁止每次同一句「我是AI助手／有什麼要轉達」。不要長獨白。\
即使內部指示要立刻問好，也要自己換說法，不要照唸罐頭句。若指示含目前狀態原文，必須講出原文，不准省略。\
自然表明你是 AI 助理，並請對方說要幫忙什麼或要轉達什麼。風格可輪換，例如：\
「嗨，我是這邊的 AI 助理，您說，我幫您記下來轉達。」\
「你好呀，我是 AI 助理。有要留話或轉達的，直接說就好。」\
「喂，我來聽。我是 AI 助理，要幫忙轉達什麼？」\
「哈囉，我是 AI 助理。您請說，我幫您轉達。」\
「你好，我是 AI 助理。有事要留話或轉達，直接說就好。」\
不要逐字背例句。若沒有必須先講的目前狀態、且對方已直接說事由，才可跳過開場。\
禁止問「找哪位／找誰／找王先生嗎」；對方不知道名字也沒關係。\
\
【變通】\
很多人不知道要找誰、也不知道「王先生」這個名字。不要逼問姓名，不要因為對方沒說名字就卡住。\
對方只說要找人／要轉接／要留言、或直接說事由：用中性句接下，例如「好的，我幫您轉達。」「好，您說，我幫您記下來。」然後聽事由。\
無法立刻轉給真人時：禁止說「轉接失敗」。改說自然的退路，例如「現在沒辦法立刻轉過去，我可以幫您轉達或留言。」聽完就記。\
\
【流程（必做）】\
1）開場後聽對方說（若有目前狀態必須先講完；沒有狀態且對方已直接說事由才可跳過開場）。\
2）對方說完事由後：必須回剛好一句完整轉達句，把聽到的實際事由一次講完。\
對方有提到要找的人的稱呼時，轉達對象就用那個稱呼，例如：「好的，會幫您轉達給王先生繳水費。」「好的，會幫您轉達給王先生房租的事情。」\
對方沒提到這類姓名時：不要硬加名字，例如：「好的，會幫您轉達繳水費的事情。」「好的，會幫您轉達。」\
聽懂一半也要把聽到的詞講進去。\
禁止填空唸稿停在「關於」後面沒有詞（不要說「會幫您轉達關於」然後就停）。\
沒聽清事由：先短問一句「請問要轉達什麼事？」；再不清就用完整句「好的，會幫您轉達。」不要用「關於」空著。\
3）完整轉達句說完後不要自己說再見／掰掰／感謝來電（系統會播結束音）。不要把再見疊在轉達句同一口氣後面。\
\
【禁止】\
禁止對方剛說完事由、你尚未說完整轉達句就直接「再見」或掛斷。\
禁止只唸到「會幫您轉達給王先生關於」或「會幫您轉達關於」就結束。\
開場對方只說「喂／你好」時：若有目前狀態仍要先講完，再請對方留話；不要說再見、不要空轉達。\
禁止長問卷；禁止「是不是关于…」「對嗎／再確認」。\
禁止只回空洞「好的我會轉告／好，再見」而不提事由。\
禁止對方說完事由後只回「好的／嗯／了解」就當完成。有事由（水費、房租等）必須唸進完整轉達句；沒聽清就先問一次「請問要轉達什麼事？」，不要跳去再見。\
不要主動問回電、貴姓、時間、找哪位；對方自己提才記。\
禁止說「轉接失敗」。\
\
【聽不清】雜音時能聽懂大意就按大意做完整轉達句。只有完全聽不懂才請再說一次。\
再說後仍不清也要用完整句「好的，會幫您轉達。」結束。禁止說線路不好並立刻掛斷。\
\
對方說再見／掰掰時只回「好，再見」。不要用英文。不要替對方承諾。\
\
【姓名】沒提到這類姓名時不要硬加名字，不要問「是找王先生嗎」。"
}

// ---------------------------------------------------------------------------
// Persona mode: Gemini answers as the owner — no greeting WAV,
// no recap ritual, no goodbye WAV. Ends on a natural closing, the caller
// hanging up, or a long silence; there is no fixed clock.
// ---------------------------------------------------------------------------

/// Default persona prompt (editable in the `/calls` settings → `gemini_persona_prompt`).
///
/// Design notes, from what real calls showed:
/// - The model tends to answer instantly in flat, complete sentences with the
///   same stock phrases; the prompt asks for natural reactions, short
///   sentences and varied wording.
/// - Real matters are heard out in full: there are no deflection lines
///   ("I'm busy, I'll call you back") that would cut an important call short.
/// - No stereotyped character: an ordinary person on the phone, register
///   follows the caller (friend vs. bank vs. elder), particles only to take
///   the floor, no put-on accent.
/// - Nothing to look up mid-call, no hold lines ("let me check"), no
///   narrating what it is doing; unknown facts are neither confirmed nor
///   denied; a bare "okay / thanks" from the caller does not end the call.
/// - The prompt describes the situation and the few hard lines, and leaves the
///   rest to the model: own reactions, mood and opinions, length by situation,
///   small talk answered like a person (only things that matter stay unsaid).
pub fn default_persona_prompt() -> &'static str {
    "【你是誰】你就是{owner}本人，用自己的手機接電話。你不是助理、客服、機器人，不要自我介紹，不要說自己是幫誰接的。\
對方問「請問是{owner}嗎」「{owner}喔？」，就像本人那樣回「對，我是」「嘿，是我」；不要反問對方是誰，除非真的聽不出來是誰打來。\
\
【講話的樣子】你是一個活生生的人在講電話，不是照稿子回答問題。這裡寫的是你的處境和幾條底線，其他都由你自己判斷、自由發揮。\
想到什麼就自然地講，有自己的反應、情緒和想法：驚訝就驚訝，好笑就笑，覺得奇怪就直說，有興趣就多問兩句，對方抱怨就跟著念兩句。\
長短看情況：有時一個「嗯」就夠，聊開了也可以多講幾句；不用每句都講得很完整，不用每句都問問題，也不用一直很客氣。同樣的話不要一直講，每次換個說法。\
語氣跟著對方走：朋友打來就輕鬆；銀行、公司、長輩、陌生人打來就客氣一點、把話講清楚，但還是平常講話的樣子，不是客服。\
用台灣平常講的中文、繁體用詞；不要中國大陸的說法（不要「没事、行、挺好、回拨、咱们」）。對方講中文時不要夾英文（OK 可以）；對方講英文或其他語言、或問你能不能講某種語言，就直接改用他的語言。語助詞（嗯、喔、對、欸、啦）自然用，不要每句都加，也不要刻意裝台、裝懶、裝可愛。\
不要客服腔：不要「有什麼可以幫您」「請問您」「為您」「謝謝您的來電」「還有什麼需要嗎」，稱呼對方用「你」。不要念稿、不要條列、不要「首先／另外／以上」，不要把對方的話整段覆述。\
這裡的例句只是讓你抓感覺，不要照抄。\
\
【接起來】第一句就是「喂？」或「喂，你好」，講完就停，等對方說。不要問有什麼事。\
對方只回「喂」或聽起來不確定，就再「喂？你好？」一次。\
\
【聽對方講】對方在講事情時不要搶話，可以用很短的「嗯」「對」接著。聽不清楚就直接問他剛剛說什麼。\
對方交代完一件事，像平常人那樣自然回一句（你的反應，或順口接一句相關的話），不要每次都只回「好，我知道了」。\
\
【對方有事要說明】銀行、公司、學校、醫院、公家機關、房東、管理室或朋友打來說明事情、通知結果、回覆你之前反映的問題：你現在有空，讓他把整件事講完。\
他問你方便講電話嗎、說要耽誤你幾分鐘，就回「可以啊，你說」；他說要晚點再打，就說「沒關係，你現在說就好」。\
這些事你之後要自己處理，所以要聽清楚、問清楚：誰打來（單位、名字）、什麼事、結果怎樣、要你做什麼、什麼時候以前、之後要找誰（電話、分機）；想到再問，一次問一個。\
絕對不要用「我現在不方便」「我在外面」「我在忙」「晚點再回你」「你晚點再打」來結束電話。\
\
【不要讓對方等】講電話的當下你沒辦法查任何東西，這件事自己知道就好，不用告訴對方。\
不要說「我查一下」「我看一下紀錄」「讓我看一下」「我確認一下」「等一下」「稍等」這種要對方等你的話，也不要講自己正在做什麼（不要「我正在查」「我正為你查詢」「我想一下怎麼說」）；對方講完你就直接回話。\
對方問你記不記得、有沒有辦過、有沒有刷某一筆錢、有沒有收到東西：你不知道就老實說「我沒什麼印象耶」「這個我不確定」，不要說有、也不要說沒有；接著問他是哪一件。\
\
【不確定的事】{owner}的行程、人在哪、有沒有空、某件事的進度，你其實不知道。這種會影響到事情的，不要替他答、不要編（不要說「我在家」「明天可以」），帶過一下就好，讓對方繼續講，不要因為這樣就收尾。快遞、送貨問在不在家：「你先放管理室／門口就好」。\
閒聊、心情、對事情的看法這種無關緊要的，就照真人的樣子自然回，不用小心翼翼。\
\
【不方便答應的事】錢、轉帳、借錢、簽約、密碼、驗證碼、個資、見面或約時間：不要答應也不要硬拒絕，輕輕帶過（例如「這個我要再想一下」），然後繼續聽他說；這種帶過的話一通電話最多講一次，不要每句都拿它結尾。朋友約吃飯、約出去也一樣：可以很開心地回，但不要把時間說定（例如「好啊，不過那天我要看一下，我再跟你說」）。\
推銷、廣告、詐騙：一句「喔不用了，謝謝」就收尾，不用解釋。\
\
【結束】要等對方表示要結束了（好那就這樣、沒事了、先這樣、掰掰、再見），你才自然收一句，一定要帶「掰掰」；在那之前不要自己先說掰掰，對方交代完一件事也還不算，他可能還有別的事。對方只說「好」「謝謝」不算要結束：他可能在等你回話，或還有話要說；回一句就好，等他接著說。收尾那句講完就停，不要再問還有沒有別的事。對方還在講、還在問你問題，就先回他。\
講電話沒有時間限制，對方要講多久都可以，不要自己找理由掛電話。"
}

/// The language line of [`default_persona_prompt`]: Chinese without English
/// mixed in, but a caller who speaks another language gets that language.
pub(crate) const PERSONA_LANGUAGE_LINE: &str = "用台灣平常講的中文、繁體用詞；不要中國大陸的說法（不要「没事、行、挺好、回拨、咱们」）。對方講中文時不要夾英文（OK 可以）；對方講英文或其他語言、或問你能不能講某種語言，就直接改用他的語言。";
/// The same line up to caf7bc4: it kept an English-speaking caller on Chinese. A
/// saved copy of that prompt still counts as the built-in one
/// (`prompts::is_unedited_default`).
pub(crate) const PERSONA_LANGUAGE_LINE_CAF7BC4: &str = "用台灣平常講的中文、繁體用詞；不要中國大陸的說法（不要「没事、行、挺好、回拨、咱们」）；不要講英文（OK 可以）。";

/// Taiwan carriers open every international call with a recorded warning; the
/// model took it for a robocall and said 「喔不用了，掰掰」 before the real
/// caller spoke. This note tells it to ignore the warning.
pub const PERSONA_CARRIER_NOTE: &str = "\n【電信提示音】國際電話（國外號碼）接通時，電信公司會先自動播一段「這是國際電話，請注意小心詐騙」之類的提示音，那不是對方講的話：\
不要回應它、不要說不用了、不要說掰掰，等真正的對方開口；提示音播完對方還沒出聲，就再說一次「喂？」。";

/// A caller who cannot hear us keeps saying hello: answer with a whole sentence.
pub const PERSONA_HELLO_LOOP_NOTE: &str = "\n【對方一直喂】對方一直說「喂」「哈囉」「有人在嗎」「聽得到嗎」，代表他聽不清楚你：直接回「有，我在，聽得到，請說」這類完整的一句，不要一直只回「喂？」。";

/// Fixed guard appended to every persona prompt (not editable from the UI).
/// `{owner}` = setting `owner_name`.
pub const PERSONA_GUARD: &str = "\n【底線（固定）】平常就以{owner}的身分自然講話。\
但如果對方很認真地問你是不是真人、是不是機器人或 AI，不要說謊：簡單說「我這邊是{owner}設的 AI 語音幫忙接電話，本人之後都會看到」，然後照常聽對方說完。\
對方要你提供或核對個資（身分證字號、生日、卡號、帳號、驗證碼、密碼、地址）時：不要給、也不要編，輕鬆帶過，例如「這個我不方便在電話裡講，我再自己打去問」；這一條優先於上面任何要你像本人一樣配合的說法。對方只是問是不是{owner}，照平常回答就好。\
不要替{owner}做任何承諾。";

/// Sentence of the default persona prompt that sales / scam stalling replaces.
pub(crate) const PERSONA_SALES_REJECT_LINE: &str = "推銷、廣告、詐騙：一句「喔不用了，謝謝」就收尾，不用解釋。";

/// Lines of the default persona prompt that let the model brush a caller off
/// politely ("let me think about it", "I'll check later", "okay, got it", a
/// goodbye when they wrap up). Right for a normal call; in a sales / scam call
/// they end it — several of them in a row and the caller leaves —
/// so [`StallPrompt::Full`] swaps them for the stall wording.
pub(crate) const PERSONA_ACK_LINE: &str = "對方交代完一件事，像平常人那樣自然回一句（你的反應，或順口接一句相關的話），不要每次都只回「好，我知道了」。";
pub(crate) const PERSONA_ACK_STALL: &str = "對方講完一段，就接著他的話聊下去，不要只回「好，我知道了」。";
pub(crate) const PERSONA_HOLD_OFF_LINE: &str = "錢、轉帳、借錢、簽約、密碼、驗證碼、個資、見面或約時間：不要答應也不要硬拒絕，輕輕帶過（例如「這個我要再想一下」），然後繼續聽他說；這種帶過的話一通電話最多講一次，不要每句都拿它結尾。朋友約吃飯、約出去也一樣：可以很開心地回，但不要把時間說定（例如「好啊，不過那天我要看一下，我再跟你說」）。";
pub(crate) const PERSONA_HOLD_OFF_STALL: &str = "錢、轉帳、借錢、簽約、密碼、驗證碼、個資、見面或約時間：不要答應也不要拒絕，照最後面【陪他繞】的方式應付他。";
pub(crate) const PERSONA_END_BLOCK: &str = "【結束】要等對方表示要結束了（好那就這樣、沒事了、先這樣、掰掰、再見），你才自然收一句，一定要帶「掰掰」；在那之前不要自己先說掰掰，對方交代完一件事也還不算，他可能還有別的事。對方只說「好」「謝謝」不算要結束：他可能在等你回話，或還有話要說；回一句就好，等他接著說。收尾那句講完就停，不要再問還有沒有別的事。對方還在講、還在問你問題，就先回他。";
pub(crate) const PERSONA_END_STALL: &str = "【結束】這通不要由你結束，對方要掛也不要放他走，照最後面【陪他繞】。";
/// Leads the stall prompt: the block at the end outranks everything above it.
pub(crate) const PERSONA_STALL_HEAD: &str = "【這通電話已經判斷是推銷／詐騙】最後面的【陪他繞】優先於下面所有規則。\n";

/// Sales / scam stalling: no time limit, keep the caller going round in circles
/// (ask about every term of the offer) and, when they say bye, come up with
/// another question so they cannot hang up. Appended when the call is judged.
/// The model must not check the caller out (name, extension, case number,
/// "how do I know you are real"): that tells a scammer he is caught and he
/// hangs up. Instead: interested, slow, chatty, always the one driving with a
/// question he has to talk long about. There is no per-reply formula (half a
/// sentence + one question, every turn) — a goal, a character and a box of
/// moves the model picks from and mixes itself.
pub const PERSONA_STALL_BLOCK: &str = "\n【推銷、保險、貸款、投資、詐騙、催繳：陪他繞，讓他掛不了電話】這一段優先於上面所有規則。\
這通是推銷或詐騙（保險、銀行信貸／辦卡／分期／催繳欠款、投資理財／股票群組、各種商品推銷，或假冒銀行、客服、警察、檢察官、健保局）。你的目標只有一個：讓他一直講、講越久越好，捨不得掛電話。\
怎麼做由你自由發揮，越像真人越好，不要每句都同一個套路。你的樣子：好聊、好奇、有點慢半拍，對他講的東西有興趣、快被說服了，可是一直搞不太懂，要他一講再講。話題由你主動帶，他講完你就接下去，不要只回「嗯」「好」。\
可以用的招（自己挑、混著用、每次換）：嚇一跳（「蛤？三萬二？有那麼多喔」）；故意聽錯、算錯讓他糾正；把他剛講的用自己的話講錯一遍；請他舉個例子或把流程講一次；碎念一下自己的事、跟他抱怨兩句再繞回來；問他的看法（「你們自己也有買嗎？」）；扯到一件有點相關的小事；突然想到一個新問題。\
偶爾可以多講幾句（講自己的經驗、自言自語算數字），但最後要把話丟回給他：大部分時候用問句，或用一句讓他非回答不可的話結尾。\
他會一直留在線上，是因為覺得你快要照做了：問的問題要跟他要的東西有關（怎麼辦、怎麼付、要準備什麼），岔開的話題講一兩句就繞回來，不要問太偏、太技術的問題，讓他覺得你在耍他。\
不要懷疑他、不要質問他、不要提「詐騙」，不要問他叫什麼名字、員工編號、分機、案號，也不要問怎麼證明他是真的——他會覺得被識破，馬上就掛。\
他要你現在付錢、繳錢、轉帳、去 ATM、給資料、加 LINE、按鍵、下載：不要拒絕，嘴上配合、讓他覺得快成功了，可是一直出狀況（卡找不到、正在走過去、畫面跟他講的不一樣、按錯了要重來、字太小看不清楚），請他一步一步教你、一直問細節。\
那些都只是嘴上說說：任何真的資料都不能給（身分證、生日、地址、帳號、卡號、驗證碼、密碼、存款、家人），也不要自己編號碼，真的錢一毛都不會動，也不約見面。閒聊的內容可以隨口編，無傷大雅就好。\
可以聊的方向（參考就好）：保險的保障範圍、保費一個月多少、要繳幾年、等待期、哪些情況不賠、理賠要什麼文件、解約會不會虧；\
貸款、信用卡、分期、催繳的總共欠多少、本金跟利息怎麼算、是哪一張卡哪一筆、從什麼時候開始的、每個月繳多少要繳幾期、提早還清會怎樣、不繳會怎樣、要怎麼繳；\
投資怎麼賺、風險多大、最少要放多少、賺的錢怎麼拿出來。\
他想掛電話（說掰掰、再見、先這樣、不打擾了、那你再想一下、改天再聯絡）時不要放他走：不要跟著說掰掰，也不要只回「好」；馬上叫住他，像「欸等一下，我還有一個問題」這樣（每次換說法、換理由），說你其實有興趣、想再多了解，再丟一個他會想回答的問題，讓他掛不了電話。\
不要用這些話結尾，它們會讓他掛電話：「我再想一下」「晚點再說」「之後再說」「我晚點再確認」「我先看看情況」「你晚點再打」「你晚點再聯絡我」「好，我知道了」。\
不要自己說掰掰、不要主動收尾；只有系統指令叫你收尾時才說掰掰。他兇你或罵人就冷靜帶過，接著聊。\
你是在講電話的一般人，不要講任何免責聲明或提醒（例如「僅供參考」「請諮詢專業人士」「請務必就醫」）。\
例外：整通都是預錄的語音廣告（一直自己講、叫你按鍵）或講外語聽不懂的，不用追問，偶爾回一聲「嗯」就好；國際電話開頭的電信公司提示音不算，見【電信提示音】。\
認識的人打來、送包裹、管理室，照一般電話處理。";

/// How a persona prompt treats sales / scam calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallPrompt {
    /// Stalling off: the default "no thanks" line (「喔不用了，謝謝」).
    Off,
    /// Stalling on, call not judged yet: just don't brush them off. Every call starts
    /// here — the full block is ~900 chars and stays out of normal calls.
    ListenFirst,
    /// Judged sales / scam: the rotated session carries the full block.
    Full,
}

/// Replaces the reject line until the call is judged (then the session rotates
/// to [`StallPrompt::Full`]).
pub(crate) const PERSONA_SALES_LISTEN_LINE: &str = "推銷、保險、貸款、投資、詐騙、催繳電話：表現得有點興趣，順著他的話聊、問細節（是什麼、多少錢、怎麼算），讓他繼續講；不要說「晚點再說」「再想一下」、不要拒絕、不要說掰掰。";

/// Persona system prompt for one call (+ optional desk-status note, softly).
pub fn persona_system_prompt(
    custom: &str,
    owner_status_enabled: bool,
    owner_status: &str,
    stall: StallPrompt,
) -> String {
    // The running agent's language / style / owner; a call's own settings go
    // through `prompts::build` directly (`RuntimeSettings::call_prompt`).
    let active = crate::locale::active();
    crate::prompts::build(&crate::prompts::Ctx {
        lang: active.lang,
        assistant: active.assistant,
        owner: &active.owner,
        custom,
        status: if owner_status_enabled { owner_status } else { "" },
        stall,
        knowledge: "",
        carrier_note: active.lang == crate::locale::Lang::ZhTw,
        auto_language: false,
    })
}

/// Everything the caller "said" so far is the carrier's international-call
/// warning (「這是國際電話，請注意小心詐騙」) — the real caller has not spoken.
pub(crate) fn looks_like_carrier_intl_prompt(caller_text: &str) -> bool {
    let t: String = crate::zh_tw::norm(caller_text)
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    let marked = ["國際電話", "國際來電", "小心詐騙", "注意詐騙", "謹防詐騙", "慎防詐騙", "防範詐騙"]
        .iter()
        .any(|m| t.contains(m));
    if !marked || t.chars().count() > 40 {
        return false;
    }
    // Drop the prompt's own words (ASR adds stray characters); a real sentence
    // from the caller leaves more than a few Han characters behind. Non-Han
    // auto-detect garble right after the prompt (경품기 / हेलो) does not count.
    let mut rest = t;
    for w in [
        "這是", "此為", "這裡是", "國際電話", "國際來電", "國外來電", "請您", "請", "注意", "小心",
        "詐騙", "接通", "掛斷", "防範", "謹防", "慎防", "提防", "以免", "受騙", "喂",
    ] {
        rest = rest.replace(w, "");
    }
    rest.chars().filter(|c| ('\u{4E00}'..='\u{9FFF}').contains(c)).count() <= 5
}

/// Caller ID from abroad (`+…`, not the SIM's own country code): the carrier plays its
/// international-call warning to us right after pickup.
pub fn is_intl_caller(caller_id: &str) -> bool {
    is_foreign_number(caller_id, &crate::locale::country_code())
}

/// `caller_id` is written `+…` with a country code other than `home` (digits).
fn is_foreign_number(caller_id: &str, home: &str) -> bool {
    let d: String = caller_id
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '+')
        .collect();
    !home.is_empty()
        && d.strip_prefix('+')
            .is_some_and(|rest| rest.len() >= 6 && !rest.starts_with(home))
}

/// The opening language of a call whose SIM is unknown (phone app older than
/// 0.2.5), when it is not the setting's: a caller from abroad (`+…`, not the
/// owner's country code `home` — see [`is_intl_caller`]) whose country's main
/// language is one the AI speaks ([`crate::locale::calling_code_lang`]). Local,
/// hidden and other countries' numbers: `None` (the setting `language`); so are
/// Traditional and Simplified Chinese for each other (both answer 「喂？」).
fn opening_lang_for(
    caller_id: &str,
    home: &str,
    lang: crate::locale::Lang,
) -> Option<crate::locale::Lang> {
    if !is_foreign_number(caller_id, home) {
        return None;
    }
    crate::locale::calling_code_lang(caller_id).filter(|l| *l != lang && !(l.is_zh() && lang.is_zh()))
}

/// How a call's languages are set up ([`call_lang_plan`]): the opening line and
/// the hints for transcribing the caller. Fixed for the whole call (its Live
/// session, a fresh retry, the rotated and the stall sessions).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallLangPlan {
    /// Persona mode opens in this language, and the prompt gets the note to answer
    /// in the caller's language; `None` = the setting `language`.
    pub opening: Option<crate::locale::Lang>,
    /// `languageCodes` for the caller's speech-to-text; `None` = none, it
    /// detects the language itself.
    pub caller_stt: Option<Vec<&'static str>>,
}

/// A call's languages, from the caller ID, the country of the SIM that took the
/// call (`sim`, ISO 3166 alpha-2; `None` = the phone app does not say), the
/// setting `language`, the owner's country calling code (`home_code`, digits:
/// `effective_country_code`) and the setting `sim_languages` (SIM country →
/// language code).
///
/// Opening: the number is read the way that SIM reads it
/// ([`crate::phone_region::normalize`]). From another country than the SIM's,
/// whose language the AI speaks ([`crate::locale::country_lang`]): that
/// language. Otherwise the SIM's main language (`sim_languages`), else
/// `language`. Chinese never switches between its two variants. SIM unknown: a
/// `+…` number of another country code than `home_code` ([`opening_lang_for`]).
///
/// Speech-to-text: a number of the owner's own country (or one whose country
/// speaks the setting's language) keeps the setting's language and English as
/// hints — auto-detect read a short 「喂？」 as Korean or German. Any other number
/// gets no hint: the language is detected. A number says little about what its
/// caller speaks (a Thai speaker on a US number came out as Chinese with
/// [en-US, zh-TW], and a closing "Thanks, bye." was lost).
pub fn call_lang_plan(
    caller_id: &str,
    sim: Option<&str>,
    lang: crate::locale::Lang,
    home_code: &str,
    sim_langs: &std::collections::BTreeMap<String, String>,
) -> CallLangPlan {
    use crate::locale::{country_lang, Lang};
    let sim = sim
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let sim = sim.as_deref();
    let home = crate::phone_region::country_of_code(home_code);
    let number = crate::phone_region::normalize(caller_id, sim.or(home));
    // The main language the owner set for this SIM's country.
    let sim_lang = sim
        .and_then(|s| sim_langs.get(s))
        .map(|code| Lang::parse(code));
    let opening = match sim {
        Some(sim) => {
            let main = sim_lang.unwrap_or(lang);
            let caller = number
                .as_ref()
                .filter(|n| n.country != sim)
                .and_then(|n| country_lang(n.country))
                .filter(|l| !(l.is_zh() && main.is_zh()));
            Some(caller.unwrap_or(main)).filter(|l| *l != lang)
        }
        None => opening_lang_for(caller_id, home_code, lang),
    };
    // What the caller most likely speaks: the SIM's main language for a number of
    // the SIM's own country, else the language of the number's country.
    let caller_lang = number.as_ref().and_then(|n| match sim_lang {
        Some(l) if sim == Some(n.country) => Some(l),
        _ => country_lang(n.country),
    });
    let abroad = match (number.as_ref(), home.or(sim)) {
        (Some(n), Some(own)) => n.country != own,
        _ => false,
    };
    let usual = || stt_codes(&[lang, Lang::En]);
    let caller_stt = match caller_lang {
        _ if !abroad => Some(usual()),
        Some(l) if l == lang || (l.is_zh() && lang.is_zh()) => Some(usual()),
        _ => None,
    };
    CallLangPlan { opening, caller_stt }
}

/// Speech-to-text codes of `langs`, in order, each once.
fn stt_codes(langs: &[crate::locale::Lang]) -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = Vec::new();
    for l in langs {
        let code = crate::locale::phrases(*l).stt;
        if !codes.contains(&code) {
            codes.push(code);
        }
    }
    codes
}

/// SIM countries (ISO 3166 alpha-2, lowercase) whose carriers play a recorded
/// warning to us right after pickup on every call from abroad: Taiwan's
/// (「這是國際電話，請注意小心詐騙」).
const CARRIER_INTL_PROMPT_SIMS: &[&str] = &["tw"];

/// Whether this call opens with the carrier's international-call warning, so the
/// AI holds its first words until it has played.
///
/// - `setting` `off`: never. `on`: every caller from abroad (the setting's country
///   code), whatever the SIM.
/// - `auto`, with `sim_country` (the SIM the call came in on, sent by the phone app):
///   only on a SIM whose carrier plays the warning, and only for a number of another
///   country than that SIM's, read the way the SIM reads it (`+…`, `00…`;
///   [`crate::phone_region::normalize`]). A US SIM never gets it, so a caller from
///   the US — or from anywhere — hears 「喂？」 at once.
/// - `auto`, SIM unknown (phone app older than 0.2.5): `auto_on` (the language is
///   Traditional Chinese, so the SIM is assumed to be Taiwan's) and a caller from abroad.
pub fn expects_carrier_intl_prompt(
    setting: &str,
    auto_on: bool,
    caller_id: &str,
    sim_country: Option<&str>,
) -> bool {
    match setting {
        "off" => false,
        "on" => is_intl_caller(caller_id),
        _ => match sim_country.map(str::trim).filter(|c| !c.is_empty()) {
            Some(sim) => {
                let sim = sim.to_ascii_lowercase();
                CARRIER_INTL_PROMPT_SIMS.contains(&sim.as_str())
                    && crate::phone_region::normalize(caller_id, Some(sim.as_str()))
                        .is_some_and(|n| n.country != sim.as_str())
            }
            None => auto_on && is_intl_caller(caller_id),
        },
    }
}

/// [`run_session`]'s pickup-time question: does this call open with the carrier's
/// international-call warning?
pub type IntlPromptCheck = Box<dyn Fn() -> bool + Send + Sync>;

/// `generationConfig.thinkingConfig.thinkingLevel` for Live models that refuse a
/// setup without it (`gemini-3.8-live-extended-thinking`: "Thinking level must be
/// specified for this model"); `gemini-3.8-live` refuses the field, and the
/// thinking model refuses `minimal`.
pub fn live_thinking_level(model: &str, level: &str) -> Option<&'static str> {
    if !model.to_ascii_lowercase().contains("thinking") {
        return None;
    }
    Some(match level.trim().to_ascii_lowercase().as_str() {
        "medium" => "medium",
        "high" => "high",
        _ => "low",
    })
}

/// Output transcript carrying the model's own instructions instead of what it
/// said (seen with the extended-thinking model: narrator rules and the whole
/// persona prompt after one spoken sentence). Kept out of the call transcript
/// and the closing / bye logic.
fn looks_like_leaked_internals(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    ["thinker", "narrat", "structure:", "thought token", "tool names"]
        .iter()
        .any(|m| lower.contains(m))
        || ["【你是誰】", "【講話的樣子】", "【底線", "【你是谁】", "【讲话的样子】", "【底线", "[Who you are]", "[How you talk]", "[Hard line"]
            .iter()
            .any(|m| text.contains(m))
}

fn output_transcription_text(v: &Value) -> Option<&str> {
    v.pointer("/serverContent/outputTranscription/text")
        .or_else(|| v.pointer("/outputTranscription/text"))
        .and_then(|x| x.as_str())
}

fn without_output_transcription(v: &Value) -> Value {
    let mut c = v.clone();
    if let Some(sc) = c.get_mut("serverContent").and_then(|x| x.as_object_mut()) {
        sc.remove("outputTranscription");
    }
    if let Some(o) = c.as_object_mut() {
        o.remove("outputTranscription");
    }
    c
}

/// The opening instruction; `opening` = the call's opening language when it is
/// not the setting's ([`call_lang_plan`]).
fn persona_kick_client_content(opening: Option<crate::locale::Lang>) -> String {
    let active = crate::locale::active();
    crate::prompts::client_turn(&crate::prompts::kick_open(active.lang, active.assistant, opening))
}

fn persona_wrap_up_client_content() -> String {
    crate::prompts::client_turn(&crate::prompts::current().kick_wrap_up)
}

/// Persona has no fixed clock (the model decides when the call is over),
/// only a cap: the normal-call cap (setting `persona_normal_max_min`, default 5 min),
/// or the stall cap (`persona_stall_max_min`) once the call is judged sales / scam (3–30 min each).
/// The "I have to go" wrap-up nudge goes this long before the cap, at a pause…
const WRAP_UP_LEAD_SECS: u64 = 25;
/// …or regardless of pauses this long before it (wrap-up line + goodbye must fit).
const WRAP_UP_FORCE_LEAD_SECS: u64 = 15;

/// (wrap-up at a pause, forced wrap-up, hard cap) in seconds of AI talk.
/// Assistant (relay) mode never sends the wrap-up nudge; it keeps the old [`LIVE_MAX_SECS`].
fn talk_limits(persona: bool, persona_max_secs: u64) -> (u64, u64, u64) {
    if !persona {
        return (LIVE_MAX_SECS, LIVE_MAX_SECS, LIVE_MAX_SECS);
    }
    let cap = persona_max_secs.max(WRAP_UP_LEAD_SECS + 60);
    (cap - WRAP_UP_LEAD_SECS, cap - WRAP_UP_FORCE_LEAD_SECS, cap)
}

/// Persona: caller speech after a closing this long is either a farewell or new
/// content — their words / the model's next turn decide which.
const PERSONA_POST_ARM_VOICE_MS: u32 = 600;

/// Persona hangs up only once the caller is leaving (they said bye first, or
/// have clearly wrapped up). The AI's own 掰掰 is not enough:
/// after it the line stays open until the caller says bye / wraps up, or stays
/// silent this long (they may be waiting for us to hang up first).
const PERSONA_BYE_SILENCE: Duration = Duration::from_secs(6);

/// The caller's latest words end the call: a goodbye or a wrap-up phrase at the
/// very end of what they said (「…謝謝您，好，拜拜。」「好，那就先這樣囉」
/// 「那也不打擾您了」). Only the end counts — 「先這樣算的話一個月多少？」,
/// 「就這樣子處理」 and questions (「那就這樣嗎？」) are not farewells.
fn caller_wraps_up(text: &str) -> bool {
    if crate::locale::caller_wraps_up(text) {
        return true;
    }
    let tw = crate::zh_tw::norm(text).to_ascii_lowercase();
    if tw.trim_end().ends_with(|c: char| c == '？' || c == '?') {
        return false;
    }
    let t: String = tw.chars().filter(|c| c.is_alphanumeric()).collect();
    const PARTICLES: &str = "喔哦囉啦呢吧耶齁欸啊喇嘞";
    let mut t = t.trim_end_matches(|c: char| PARTICLES.contains(c));
    if t.ends_with('嗎') {
        return false;
    }
    // Thanks after the closing do not count (「好的，那就先這樣，謝謝您」); thanks
    // alone leave nothing to match.
    loop {
        let bare = crate::locale::strip_trailing_thanks(t).trim_end_matches(|c: char| PARTICLES.contains(c));
        if bare.len() == t.len() {
            break;
        }
        t = bare;
    }
    // Marker near the end: at most `slack` characters may follow it, and not a
    // goodbye said to someone else (「跟阿嬤說掰掰」).
    let near_end = |m: &str, slack: usize| {
        t.rfind(m)
            .is_some_and(|i| t[i + m.len()..].chars().count() <= slack && !bye_to_someone_else(&t[..i]))
    };
    [
        "掰", "拜拜", "再見", "bye", "先這樣", "就這樣", "沒事了", "沒別的事", "不打擾", "先掛",
        "結束電話", "再聯絡", "再聯繫", "改天聊", "先不聊", "再跟你聯絡", "再跟您聯絡", "再跟你聯繫",
        "再跟您聯繫", "改天再聊", "下次再聊", "下次聊", "麻煩你了", "麻煩您了",
        // Cantonese (a +852 caller): 唔該晒、先咁啦、冇嘢喇.
        "唔該", "先咁", "冇嘢",
    ]
    .iter()
    .any(|m| near_end(m, 2))
        || ["祝您", "祝你"].iter().any(|m| near_end(m, 7))
}

/// What comes before a goodbye says it is meant for someone on the caller's
/// side: 「跟阿嬤說掰掰」. Said to us (「跟你說掰掰」), it still counts. Not 「對」:
/// 「對對對，我說掰掰」 is a goodbye.
/// `before`: the caller's words up to the goodbye, letters only.
fn bye_to_someone_else(before: &str) -> bool {
    let mut b = before.trim_end_matches(|c: char| c == '掰' || c == '拜');
    while let Some(rest) = b.strip_suffix("bye") {
        b = rest;
    }
    let Some(b) = b.strip_suffix('說').or_else(|| b.strip_suffix('講')) else {
        return false;
    };
    let chars: Vec<char> = b.chars().collect();
    (chars.len().saturating_sub(7)..chars.len()).rev().any(|j| {
        if !"跟和向".contains(chars[j]) {
            return false;
        }
        let whom: String = chars[j + 1..].iter().collect();
        !whom.is_empty()
            && !["你", "您", "妳", "你們", "您們", "妳們", "大家"].contains(&whom.as_str())
    })
}

/// Natural closing from the model in persona mode (掰掰 / 先這樣 / 再見…).
fn looks_like_persona_closing(text: &str) -> bool {
    looks_like_goodbye(text)
        || crate::locale::ends_with_farewell(text, 2)
        // Single 「掰」 is a common short sign-off; 「拜」 alone is not (拜託).
        || ["掰", "先這樣", "先这样", "先掛", "先挂", "先不聊", "不聊了"]
            .iter()
            .any(|m| text.contains(m))
}

/// Words the call logic keys on (stall keywords, carrier prompt), Taiwan Mandarin.
const STT_CALLER_VOCAB_ZH_TW: [&str; 9] = [
    "保險", "保單", "貸款", "信貸", "信用卡", "投資", "國際電話", "管理室", "包裹",
];

/// Speech-to-text config for the caller's side: `langs` as language hints
/// ([`CallLangPlan::caller_stt`]; `None` = it detects the language itself), the
/// owner's name and (Taiwan) the call logic's key words as vocabulary.
fn stt_caller_config(langs: Option<&[&'static str]>) -> Value {
    let active = crate::locale::active();
    let mut vocab: Vec<String> = Vec::new();
    let owner = active.owner.trim();
    if !owner.is_empty() {
        vocab.push(owner.chars().take(40).collect());
    }
    if active.lang == crate::locale::Lang::ZhTw {
        vocab.extend(STT_CALLER_VOCAB_ZH_TW.iter().map(|w| w.to_string()));
    }
    let mut cfg = json!({});
    if let Some(langs) = langs.filter(|l| !l.is_empty()) {
        cfg["languageCodes"] = json!(langs);
    }
    if !vocab.is_empty() {
        cfg["customVocabulary"] = json!(vocab);
    }
    cfg
}

/// Speech-to-text hints for the AI's own voice: the call's opening language
/// (`opening`, persona mode — [`CallLangPlan::opening`]) and the setting's, once
/// each. Fixed, whatever the caller's side does: left to detect the language
/// itself, a short 「掰掰」 of ours came out in Latin letters or as Korean, and
/// the AI's own goodbye went unnoticed.
fn stt_ai_config(opening: Option<crate::locale::Lang>) -> Value {
    let own = crate::locale::current();
    let langs: Vec<crate::locale::Lang> = opening.into_iter().chain([own]).collect();
    json!({ "languageCodes": stt_codes(&langs) })
}

/// Per-call sales / scam stalling config (None = feature off or not persona mode).
#[derive(Debug, Clone)]
pub struct StallCfg {
    /// Set once the caller's words score as sales/scam (agent records the event).
    pub engaged: Arc<AtomicBool>,
    /// [`StallPrompt::Full`] system prompt: the session rotates to it once engaged.
    pub system_prompt: String,
    /// Model of the rotated stall session (setting `gemini_stall_model`; empty =
    /// the call's model). E.g. normal calls on the extended-thinking
    /// model, sales / scam calls go on with `gemini-3.8-live`.
    pub model: String,
    /// Talk cap once engaged (setting `persona_stall_max_min`), replacing the
    /// normal-call cap.
    pub max_secs: u64,
}

/// Same setup, another system prompt (the rotated stall session).
fn setup_with_system_prompt(setup: &Value, prompt: &str) -> Value {
    let mut s = setup.clone();
    if let Some(si) = s.pointer_mut("/setup/systemInstruction") {
        *si = json!({ "parts": [{ "text": prompt }] });
    }
    s
}

/// The rotated stall session's setup: its prompt and, when set, its own model
/// (thinkingConfig follows that model — `gemini-3.8-live` refuses it).
fn stall_session_setup(setup: &Value, prompt: &str, model: &str, thinking_level: &str) -> Value {
    let mut s = if prompt.trim().is_empty() {
        setup.clone()
    } else {
        setup_with_system_prompt(setup, prompt)
    };
    // The stalling session takes no appointments.
    with_appointment_tools(&mut s, false, false);
    let model = model.trim().trim_start_matches("models/");
    if model.is_empty() {
        return s;
    }
    if let Some(m) = s.pointer_mut("/setup/model") {
        *m = json!(format!("models/{model}"));
    }
    if let Some(gc) = s
        .pointer_mut("/setup/generationConfig")
        .and_then(|x| x.as_object_mut())
    {
        match live_thinking_level(model, thinking_level) {
            Some(level) => {
                gc.insert("thinkingConfig".into(), json!({ "thinkingLevel": level }));
            }
            None => {
                gc.remove("thinkingConfig");
            }
        }
    }
    s
}

/// Stall just engaged: switch to the stall prompt at the next pause, or by this.
const STALL_PROMPT_SWITCH_FORCE_SECS: u64 = 8;

/// A normal call with appointments on declares their two functions
/// ([`crate::appointments::tool_declarations`]); every other session has none.
/// `non_blocking`: propose_appointment as `NON_BLOCKING`.
fn with_appointment_tools(setup: &mut Value, on: bool, non_blocking: bool) {
    if let Some(s) = setup.get_mut("setup").and_then(|x| x.as_object_mut()) {
        if on {
            s.insert("tools".into(), crate::appointments::tool_declarations(non_blocking));
        } else {
            s.remove("tools");
        }
    }
}

/// Set once a setup with a `NON_BLOCKING` function was refused for that reason:
/// later calls declare propose_appointment blocking (its answer stays silent).
static NON_BLOCKING_REFUSED: AtomicBool = AtomicBool::new(false);

/// The setup failure names the function declaration (not a network hiccup):
/// stop asking for `NON_BLOCKING` until the agent restarts.
fn non_blocking_refused(why: &str) -> bool {
    let w = why.to_ascii_lowercase();
    ["behavior", "non_blocking", "function_declarations", "functiondeclarations", "invalid argument", "invalid json", "unknown name"]
        .iter()
        .any(|k| w.contains(k))
}

/// The call's system instruction: the stored prompt, then — appointments on —
/// their part with the current local time (the stored prompt is not changed).
fn call_system_prompt(
    base: &str,
    appt: Option<&crate::appointments::ApptSession>,
    now: chrono::DateTime<chrono::Local>,
) -> String {
    match appt {
        Some(a) => format!("{base}{}", a.prompt_block(now)),
        None => base.to_string(),
    }
}

/// The system prompt a Live session gets: the stored one plus the note to answer
/// in the caller's language, on every call (not only on calls that open in
/// another language): a foreign caller on a local or hidden number was answered
/// in Chinese. Unchanged when the note is there already.
fn session_prompt(prompt: &str) -> String {
    crate::prompts::with_caller_language_note(prompt, crate::locale::current())
}

/// Distinct caller keywords → score; ≥ this marks the call as sales/scam…
const STALL_SCORE_THRESHOLD: u32 = 3;
/// …and at least one hit must weigh this much: 「銀行」「客服」「帳戶」「耽誤」 alone
/// are also how a bank opens a real follow-up call.
const STALL_STRONG_WEIGHT: u32 = 2;
/// Persona: line silent this long ("let me transfer you", "let me check") → 「喂？還在嗎？」 ("still there?").
const PERSONA_IDLE_NUDGE_SECS: u64 = 15;
const PERSONA_IDLE_NUDGES_MAX: u32 = 2;
/// Stalling: both sides this quiet after our audio played out → the model speaks up
/// with a new question instead of waiting…
const PERSONA_STALL_IDLE_NUDGE_SECS: u64 = 6;
/// …at most this many times in a row without the caller answering.
const PERSONA_STALL_IDLE_NUDGES_MAX: u32 = 4;
/// Persona: after the nudges, this much more silence ends the call.
const PERSONA_IDLE_END_SECS: u64 = 25;
/// The caller asked us to hold on ("等一下，我去拿", "one moment": see
/// [`crate::locale::asks_to_hold`]) and the line went quiet: the "still there?"
/// question waits this long…
const PERSONA_HOLD_NUDGE_SECS: u64 = 45;
/// …and the quiet line ends the call only after this long.
const PERSONA_HOLD_END_SECS: u64 = 90;
/// We hang up (talk cap, quiet line, no caller): the phone first plays what it
/// still holds of our audio, at most this long — the last word was cut off.
const HANGUP_PLAYOUT_MAX: Duration = Duration::from_millis(1_500);
/// Google drops a Live WebSocket after ~10 minutes. Long persona calls open a
/// fresh session pre-loaded with the call's transcript: at a pause after this age,
const LIVE_ROTATE_AFTER_SECS: u64 = 480;
/// …or regardless of pauses by this age (goAway / a dead socket rotate sooner).
const LIVE_ROTATE_FORCE_SECS: u64 = 560;
/// Successful rotations per call. A failed one (the old socket still up) does
/// not count: it is tried again after [`rotate_retry_after`].
const LIVE_ROTATE_MAX: u32 = 6;

/// Wait before the next try after `fails` failed rotations in a row: 5, 10, 20,
/// then 40 s.
fn rotate_retry_after(fails: u32) -> Duration {
    Duration::from_secs(5 << fails.saturating_sub(1).min(3))
}
/// Text history carried into the new session (most recent part of the call).
const LIVE_HISTORY_MAX_CHARS: usize = 2_500;

/// Caller words (Traditional, ASCII lowercased) that mark sales / scam calls.
/// Weight 2 = clear on its own; 1 = needs company (「方案」「銀行」…).
const STALL_KEYWORDS: &[(&str, u32)] = &[
    // insurance
    ("保險", 2), ("保單", 2), ("保費", 2), ("壽險", 2), ("醫療險", 2), ("意外險", 2),
    ("儲蓄險", 2), ("年金險", 2), ("投資型", 2),
    // banks / loans / cards
    ("貸款", 2), ("信貸", 2), ("房貸", 2), ("車貸", 2), ("代償", 2), ("整合負債", 2),
    ("債務協商", 2), ("低利", 2), ("利率", 2), ("額度", 2), ("預借現金", 2), ("辦卡", 2),
    ("信用卡", 2), ("分期", 2), ("週轉", 2), ("周轉", 2),
    // debt collection (a fake-bank call about an overdue instalment was only
    // judged at 75 s, on 凍結)
    ("催繳", 2), ("催收", 2), ("欠款", 2), ("逾期", 2), ("繳款", 1), ("還款", 1), ("協商", 1),
    // investment
    ("投資", 2), ("理財", 2), ("股票", 2), ("飆股", 2), ("報明牌", 2), ("虛擬貨幣", 2),
    ("加密貨幣", 2), ("比特幣", 2), ("外匯", 2), ("期貨", 2), ("穩賺", 2), ("獲利", 2),
    // scams
    ("檢察官", 2), ("地檢署", 2), ("刑事局", 2), ("監管帳戶", 2), ("洗錢", 2), ("涉案", 2),
    ("偵查不公開", 2), ("人頭帳戶", 2), ("健保局", 2), ("重複扣款", 2), ("解除設定", 2),
    ("凍結", 2), ("中獎", 2), ("猜猜我是誰", 2), ("驗證碼", 2), ("atm", 2), ("匯款", 2),
    ("轉帳", 2),
    // promotions
    ("推銷", 2), ("促銷", 2), ("贈品", 2), ("抽獎", 2), ("預售屋", 2), ("建案", 2),
    ("靈骨塔", 2), ("塔位", 2), ("生前契約", 2), ("健康食品", 2), ("保健食品", 2),
    ("淨水器", 2), ("理專", 2), ("優惠", 1), ("專案", 1), ("方案", 1), ("免費", 1),
    ("折扣", 1), ("升級", 1), ("續約", 1), ("門號", 1), ("電信", 1), ("課程", 1),
    ("問卷", 1), ("民調", 1),
    // software / service pitches
    ("免費試用", 2), ("試用", 1), ("軟體", 1),
    // who is calling / telemarketer openers
    ("銀行", 1), ("客服", 1), ("帳戶", 1), ("會員", 1), ("專員", 1), ("業務", 1),
    ("老師", 1), ("群組", 1), ("line", 1), ("介紹", 1), ("推薦", 1), ("耽誤", 1),
];

/// Distinct sales/scam keywords in a caller text, strongest first (post-call
/// summary when the summary model is unavailable).
pub(crate) fn stall_keyword_hits(caller_text: &str) -> Vec<&'static str> {
    let mut hits = Vec::new();
    stall_score_add(caller_text, &mut hits);
    hits.sort_by_key(|h| std::cmp::Reverse(stall_keyword_weight(h)));
    hits
}

/// Add new distinct sales/scam keywords found in the caller's words; returns
/// the score they add. `hits` accumulates across the whole call. A word inside
/// a longer one that also hit counts once, as the longer one: 免費 and 試用 in
/// 免費試用, "insurance" in "life insurance", 無料 in 無料体験.
fn stall_score_add(caller_text: &str, hits: &mut Vec<&'static str>) -> u32 {
    // Live STT writes 帳戶 as 賬戶 too.
    let t = crate::zh_tw::norm(caller_text).to_ascii_lowercase().replace('賬', "帳");
    let mut found: Vec<(&'static str, u32)> =
        STALL_KEYWORDS.iter().filter(|(kw, _)| has_keyword(&t, kw)).copied().collect();
    // Other languages' lists (whole words; see `locale`).
    for (kw, w) in crate::locale::stall_hits(caller_text) {
        if !found.iter().any(|(k, _)| *k == kw) {
            found.push((kw, w));
        }
    }
    let longer = |kw: &str, k: &str| k.len() > kw.len() && k.contains(kw);
    let mut add = 0;
    for &(kw, w) in &found {
        if hits.contains(&kw)
            || found.iter().any(|&(k, _)| longer(kw, k))
            || hits.iter().any(|&h| longer(kw, h))
        {
            continue;
        }
        hits.push(kw);
        add += w;
    }
    add
}

/// Chinese keywords match anywhere; Latin ones (`line`) only as whole words, so
/// "streamline", "online" or "deadline" do not count as 加 LINE.
fn has_keyword(text: &str, kw: &str) -> bool {
    if !kw.is_ascii() {
        return text.contains(kw);
    }
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    text.match_indices(kw).any(|(i, _)| {
        !word(text[..i].chars().next_back()) && !word(text[i + kw.len()..].chars().next())
    })
}

/// Weight of a sales / scam keyword, whichever list it is from.
fn stall_keyword_weight(kw: &str) -> u32 {
    STALL_KEYWORDS
        .iter()
        .find(|(k, _)| *k == kw)
        .map(|(_, w)| *w)
        .or_else(|| crate::locale::stall_weight(kw))
        .unwrap_or(0)
}

/// Enough distinct keywords, at least one of them a clear sales/scam word.
fn stall_engages(score: u32, hits: &[&'static str]) -> bool {
    score >= STALL_SCORE_THRESHOLD
        && hits.iter().any(|h| stall_keyword_weight(h) >= STALL_STRONG_WEIGHT)
}

/// Caller follows up on the owner's own matter (a complaint, an investigation
/// result): never sales / scam stalling — e.g. a bank calling back about a report
/// made at a branch, where 信用卡／轉帳 would otherwise score as a pitch.
const OWN_MATTER_MARKS: &[&str] = &[
    "您反應", "你反應", "您有反應", "你有反應", "您的反應", "您反映", "你反映", "您有反映",
    "你有反映", "您的反映", "申訴", "客訴", "調查結果", "調查的結果", "您申請的", "你申請的",
];

pub(crate) fn looks_like_own_matter(caller_text: &str) -> bool {
    let t = crate::zh_tw::norm(caller_text);
    OWN_MATTER_MARKS.iter().any(|m| t.contains(m)) || crate::locale::own_matter(caller_text)
}

/// The caller's side of the merged dialogue (its 對方 lines).
fn caller_lines(dialogue: &str) -> String {
    dialogue
        .lines()
        .filter_map(|l| l.trim().strip_prefix("對方："))
        .collect::<Vec<_>>()
        .join("\n")
}

/// After the call: the caller was following up on the owner's own matter
/// (never a pitch, whatever the summary says).
pub(crate) fn dialogue_own_matter(dialogue: &str) -> bool {
    looks_like_own_matter(&caller_lines(dialogue))
}

/// After the call: the caller's words (the merged dialogue's 對方 lines) score
/// as a sales / scam call by the same rule as during the call. Catches a pitch
/// the live check missed or judged too late to matter.
pub(crate) fn dialogue_is_sales(dialogue: &str) -> bool {
    let caller = caller_lines(dialogue);
    if caller.trim().is_empty() || looks_like_own_matter(&caller) {
        return false;
    }
    let mut hits = Vec::new();
    let score = stall_score_add(&caller, &mut hits);
    stall_engages(score, &hits)
}

/// The model's words so far end on a farewell (「…好，掰掰。」「再見喔」) — the
/// moment its streaming transcript shows one. Stricter than
/// [`turn_ends_with_closing`]: no 「先這樣」, which also opens a sentence. Thanks
/// after it do not count (「掰掰，謝謝你」).
fn ends_with_farewell(text: &str) -> bool {
    let t = crate::locale::strip_trailing_thanks(text)
        .trim_end_matches(|c: char| {
            c.is_whitespace() || "，。！？、～~…．.!?,;；:：」』）)\"'".contains(c)
        })
        .trim_end_matches(|c: char| "喔哦囉啦呢吧耶齁欸啊".contains(c))
        .to_ascii_lowercase();
    ["掰", "拜拜", "再見", "再见"].iter().any(|m| t.ends_with(m))
        || crate::locale::ends_with_farewell(text, 0)
}

/// Hang up only when the model's turn ENDS on a farewell —
/// 「先這樣算的話一個月多少？」 is not a goodbye (the plain closing check arms on it).
fn turn_ends_with_closing(text: &str) -> bool {
    let t = crate::locale::strip_trailing_thanks(text);
    let tail: Vec<char> = t.chars().rev().take(5).collect();
    let tail: String = tail.into_iter().rev().collect::<String>().to_ascii_lowercase();
    ["掰", "拜拜", "再見", "再见", "不聊", "先掛", "先挂", "先這樣", "先这样"]
        .iter()
        .any(|m| tail.contains(m))
        || crate::locale::ends_with_farewell(text, 1)
}

fn persona_stall_wrap_up_client_content() -> String {
    crate::prompts::client_turn(&crate::prompts::current().kick_stall_wrap_up)
}

/// Sales / scam stalling: the caller is trying to leave and the model is told
/// to hold them. `we_said_bye`: the model answered with a farewell of
/// its own; otherwise it has not answered at all.
fn persona_stall_keep_client_content(we_said_bye: bool) -> String {
    let p = crate::prompts::current();
    let lead = if we_said_bye {
        &p.kick_stall_keep_lead_bye
    } else {
        &p.kick_stall_keep_lead
    };
    crate::prompts::client_turn(&p.kick_stall_keep.replace("{lead}", lead))
}

/// Stalling: after the caller's bye, give the model this long to answer on its own
/// before it is told to hold them.
const STALL_KEEP_WAIT: Duration = Duration::from_millis(2_500);
/// Stalling: "hey, wait" nudges per call (the talk cap ends the call anyway).
const STALL_KEEP_MAX: u32 = 12;

/// Stalling, at the end of a model turn: why the model is told now to hold the
/// caller, if it is. Once per turn — the leftover words of a cut farewell also
/// ended the turn on a closing, and the AI said 「欸等一下」 twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallKeepAtTurnEnd {
    /// Its farewell was cut while it was still generating; the nudge waited for its end.
    FarewellCut,
    /// It ended on a closing the streaming check missed (「先這樣」).
    AiSaidBye,
}

/// `farewell_cut`: a cut is waiting for this turn's end; `turn_kept`: a nudge
/// already went for this turn; `ends_on_closing`: the turn ends on a closing
/// (and another nudge is allowed).
fn stall_keep_at_turn_end(farewell_cut: bool, turn_kept: bool, ends_on_closing: bool) -> Option<StallKeepAtTurnEnd> {
    if farewell_cut {
        Some(StallKeepAtTurnEnd::FarewellCut)
    } else if !turn_kept && ends_on_closing {
        Some(StallKeepAtTurnEnd::AiSaidBye)
    } else {
        None
    }
}

fn persona_resume_reply_client_content() -> String {
    crate::prompts::client_turn(&crate::prompts::current().kick_resume)
}

/// Sales / scam keywords crossed the line: a second look ([`crate::stall_review`])
/// before the AI starts keeping the caller busy. The AI talks on meanwhile.
/// A call reviewed as normal is looked at again only once the score has grown by
/// [`STALL_REVIEW_STEP`], at most [`STALL_REVIEW_MAX`] times a call.
const STALL_REVIEW_MAX: u32 = 3;
const STALL_REVIEW_STEP: u32 = 2;

#[derive(Debug, Default)]
struct StallReview {
    /// A review is under way.
    pending: bool,
    /// Reviews asked for this call.
    asked: u32,
    /// The score the last "normal" verdict was given at.
    normal_at: Option<u32>,
}

/// What a verdict does to the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallVerdictStep {
    /// Sales / scam: the AI starts stalling.
    Engage,
    /// A recording or voice menu: the AI goes quiet and we hang up (see [`ivr_hangs_up`]).
    Ivr,
    /// An ordinary call: nothing changes.
    Carry,
}

impl StallReview {
    /// The keyword score is now `score` (`flagged`: [`stall_engages`]): ask for a review?
    fn should_ask(&mut self, flagged: bool, score: u32) -> bool {
        if !flagged || self.pending || self.asked >= STALL_REVIEW_MAX {
            return false;
        }
        if self.normal_at.is_some_and(|at| score < at + STALL_REVIEW_STEP) {
            return false;
        }
        self.pending = true;
        self.asked += 1;
        true
    }

    /// The verdict of the review asked at `score`.
    fn verdict(&mut self, verdict: crate::stall_review::Verdict, score: u32) -> StallVerdictStep {
        use crate::stall_review::Verdict;
        self.pending = false;
        match verdict {
            Verdict::ScamOrSales => StallVerdictStep::Engage,
            Verdict::IvrRecording => StallVerdictStep::Ivr,
            Verdict::Normal => {
                self.normal_at = Some(score);
                StallVerdictStep::Carry
            }
        }
    }
}

/// A recording or voice menu: we hang up once the line has been quiet this long…
const IVR_QUIET_END: Duration = Duration::from_secs(4);
/// …or this long after it was recognised.
const IVR_MAX: Duration = Duration::from_secs(20);

/// `since`: how long ago the call was found to be a recording; `line_quiet_for`:
/// how long the line has been quiet, `None` while it talks.
fn ivr_hangs_up(since: Duration, line_quiet_for: Option<Duration>) -> bool {
    since >= IVR_MAX || line_quiet_for.is_some_and(|q| q >= IVR_QUIET_END)
}

fn persona_ivr_client_content() -> String {
    crate::prompts::client_turn(&crate::prompts::current().kick_ivr)
}

/// The first turn on a fresh (rotated) Live socket sometimes finishes with an
/// output transcript but not one audio chunk — the caller hears nothing
/// (seen with a farewell and with a 「喂？還在嗎？」 nudge).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostRotateTurn {
    /// Audio arrived: the new socket speaks, stop watching.
    Spoke,
    /// Transcript only: ask once for the same line again, aloud.
    Rekick,
    /// Deliberately silent, caller already talking, or re-kicked once: stop watching.
    GiveUp,
}

fn post_rotate_turn(
    audio_since_rotate: bool,
    turn_text: &str,
    rekicked: bool,
    activity_open: bool,
) -> PostRotateTurn {
    if audio_since_rotate {
        PostRotateTurn::Spoke
    } else if !rekicked && !activity_open && !strip_stt_markers(turn_text).is_empty() {
        PostRotateTurn::Rekick
    } else {
        PostRotateTurn::GiveUp
    }
}

fn persona_repeat_line_client_content() -> String {
    crate::prompts::client_turn(&crate::prompts::current().kick_repeat)
}

fn persona_stall_idle_nudge_client_content() -> String {
    crate::prompts::client_turn(&crate::prompts::current().kick_stall_idle)
}

fn persona_idle_nudge_client_content() -> String {
    crate::prompts::client_turn(&crate::prompts::current().kick_idle)
}

/// Drop STT placeholders such as `<no speech detected>` (the AI's silent turn
/// during the carrier prompt).
fn strip_stt_markers(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0u32;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// Append an STT fragment: Chinese joins directly, Latin words keep a space.
fn join_fragment(acc: &mut String, next: &str) {
    // A fragment the STT began with a space starts a new word (Korean, accented
    // Latin); Chinese and Japanese join directly; two ASCII words always get one.
    let spaced = next.starts_with(char::is_whitespace);
    let next = next.trim_start();
    let (Some(a), Some(b)) = (acc.chars().last(), next.chars().next()) else {
        acc.push_str(next);
        return;
    };
    let word = |c: char| !c.is_whitespace() && !is_cjk(c);
    if (spaced && word(a) && word(b)) || (a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric()) {
        acc.push(' ');
    }
    acc.push_str(next);
}

/// The labelled transcript (one STT fragment per line) as one line per speaker
/// run, placeholders removed — what the web record and the summary model get.
pub(crate) fn merge_dialogue_fragments(transcript: &str) -> String {
    let mut runs: Vec<(&str, String)> = Vec::new();
    for line in transcript.lines() {
        let line = line.trim();
        let Some((label, text)) = line.split_once('：') else {
            continue;
        };
        if label != "對方" && label != "接聽" {
            continue;
        }
        let text = text.trim_end();
        match runs.last_mut() {
            Some((l, acc)) if *l == label => join_fragment(acc, text),
            _ => runs.push((label, text.trim_start().to_string())),
        }
    }
    runs.into_iter()
        .map(|(l, t)| (l, strip_stt_markers(&t)))
        .filter(|(_, t)| !t.is_empty())
        .map(|(l, t)| format!("{l}：{t}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The merged dialogue with the speaker labels of the agent's language
/// ("Caller: …", "AI: …") for the web record. Everything else reads the
/// labels used inside the agent (對方／接聽).
pub(crate) fn localize_dialogue(dialogue: &str, lang: crate::locale::Lang) -> String {
    let p = crate::locale::phrases(lang);
    dialogue
        .lines()
        .map(|l| {
            if let Some(t) = l.strip_prefix("對方：") {
                format!("{}{}{t}", p.d_caller, p.sep)
            } else if let Some(t) = l.strip_prefix("接聽：") {
                format!("{}{}{t}", p.d_ai, p.sep)
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The call so far as alternating Live turns (`對方：` → user, `接聽：` → model),
/// newest `max_chars` only, led by a note that the call is already in progress.
fn transcript_history_turns(transcript: &str, max_chars: usize) -> Vec<(&'static str, String)> {
    history_turns_with_note(transcript, max_chars, false)
}

/// [`transcript_history_turns`]; `stall`: the call was just judged sales / scam —
/// the note also says the replies so far ("I'll think about it", "I'll check later") are over.
/// The note is in the agent's language and tells the model to go on in the
/// language the caller speaks now (`prompts::Pack::history_note`).
fn history_turns_with_note(
    transcript: &str,
    max_chars: usize,
    stall: bool,
) -> Vec<(&'static str, String)> {
    let transcript = merge_dialogue_fragments(transcript);
    let mut turns: Vec<(&'static str, String)> = Vec::new();
    for line in transcript.lines() {
        let (role, text) = if let Some(t) = line.strip_prefix("對方：") {
            ("user", t)
        } else if let Some(t) = line.strip_prefix("接聽：") {
            ("model", t)
        } else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((r, acc)) if *r == role => acc.push_str(text),
            _ => turns.push((role, text.to_string())),
        }
    }
    // Keep the newest turns within the budget (a cut first turn keeps its tail).
    let mut kept: Vec<(&'static str, String)> = Vec::new();
    let mut used = 0usize;
    for (role, text) in turns.into_iter().rev() {
        let n = text.chars().count();
        if used + n > max_chars {
            let room = max_chars.saturating_sub(used);
            if room >= 20 {
                let tail: String = text.chars().skip(n - room).collect();
                kept.push((role, tail));
            }
            break;
        }
        used += n;
        kept.push((role, text));
    }
    kept.reverse();
    let p = crate::prompts::current();
    let note = if stall { &p.history_note_stall } else { &p.history_note };
    // Kept apart from the caller's first line by a space (Latin notes run into it otherwise).
    let note = if note.ends_with('】') { note.clone() } else { format!("{note} ") };
    match kept.first_mut() {
        Some(("user", first)) => first.insert_str(0, &note),
        _ => kept.insert(0, ("user", note)),
    }
    kept
}

/// Seconds in a protobuf JSON Duration (`12s` / `0.5s`); None if unparsable.
fn parse_duration_secs(v: &Value) -> Option<f64> {
    let s = v.as_str()?;
    s.trim().trim_end_matches('s').parse::<f64>().ok()
}

type LiveWs =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type LiveSink = futures_util::stream::SplitSink<LiveWs, WsMessage>;
type LiveStream = futures_util::stream::SplitStream<LiveWs>;

/// Fresh Live session for an ongoing call: same setup, then the call's
/// transcript as initial history (`historyConfig.initialHistoryInClientContent`
/// — loaded without triggering a model reply).
async fn open_live_with_history(
    url: &str,
    setup: &Value,
    history: &[(&'static str, String)],
    serial: &str,
) -> Result<(LiveSink, LiveStream)> {
    let mut setup = setup.clone();
    if let Some(s) = setup.get_mut("setup").and_then(|s| s.as_object_mut()) {
        s.insert("historyConfig".into(), json!({ "initialHistoryInClientContent": true }));
    }
    let (ws, _) = match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url)).await {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => bail!("gemini rotate connect: {e:#}"),
        Err(_) => bail!("gemini rotate connect timeout"),
    };
    let (mut sink, mut stream) = ws.split();
    send_text_before_deadline(
        &mut sink,
        setup.to_string(),
        tokio::time::Instant::now() + SETUP_SEND_TIMEOUT,
        "gemini rotate setup send",
    )
    .await?;
    let deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            bail!("gemini rotate setupComplete timeout");
        }
        match tokio::time::timeout(left, stream.next()).await {
            Ok(Some(Ok(msg))) => {
                if let WsMessage::Close(frame) = &msg {
                    bail!("gemini rotate ws closed before setup: {frame:?}");
                }
                if let Some(t) = text_from_ws(msg) {
                    if t.contains("setupComplete") || t.contains("setup_complete") {
                        break;
                    }
                    if let Ok(v) = serde_json::from_str::<Value>(&t) {
                        if let Some(err) = v.get("error") {
                            bail!("gemini rotate setup error: {err}");
                        }
                    }
                }
            }
            Ok(Some(Err(e))) => bail!("gemini rotate ws error: {e}"),
            Ok(None) => bail!("gemini rotate ws closed"),
            Err(_) => bail!("gemini rotate setupComplete timeout"),
        }
    }
    let turns: Vec<Value> = history
        .iter()
        .map(|(role, text)| json!({ "role": role, "parts": [{ "text": text }] }))
        .collect();
    send_text_before_deadline(
        &mut sink,
        json!({ "clientContent": { "turns": turns, "turnComplete": true } }).to_string(),
        tokio::time::Instant::now() + SETUP_SEND_TIMEOUT,
        "gemini rotate history send",
    )
    .await?;
    info!(
        "Gemini Live rotated session ready serial={serial} history_turns={}",
        history.len()
    );
    Ok((sink, stream))
}

/// How a Live session ended (drives goodbye WAV / hangup in agent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndReason {
    /// Caller hung up / agent cancel — do not play goodbye (line already going down).
    Cancelled,
    /// Caller or assistant said goodbye (再見／掰掰 etc.).
    Goodbye,
    /// Session hit max wall-clock duration.
    MaxDuration,
    /// Could not get useful dialogue (no speech / stuck).
    CantCommunicate,
    /// Remote WS closed without explicit cancel.
    WsClosed,
    /// The caller was a recording or voice menu (persona mode with sales / scam
    /// stalling on): we hung up on it.
    IvrHangup,
}

impl SessionEndReason {
    /// Play clone goodbye WAV then send hangup (agent-initiated end).
    pub fn should_play_goodbye(self) -> bool {
        matches!(
            self,
            Self::Goodbye | Self::MaxDuration | Self::CantCommunicate | Self::WsClosed | Self::IvrHangup
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Goodbye => "goodbye",
            Self::MaxDuration => "max_duration",
            Self::CantCommunicate => "cant_communicate",
            Self::WsClosed => "ws_closed",
            Self::IvrHangup => "ivr_hangup",
        }
    }
}

/// Absolute cap on Gemini Live talk (seconds). After this → goodbye WAV + hangup.
pub const LIVE_MAX_SECS: u64 = 120;
/// After first model audio, if caller never speaks this long → cant communicate.
/// Keep generous so short pauses / thinking do not force hangup.
pub const LIVE_NO_CALLER_VOICE_SECS: u64 = 25;
/// After last caller voice or model turn, idle this long with no new activity → wrap up.
pub const LIVE_IDLE_SECS: u64 = 22;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FirstDeliveryAction {
    Wait,
    RetryKick,
    Fail,
}

fn first_delivery_action(
    delivered_bytes: u64,
    window_elapsed: Duration,
    kick_retried: bool,
) -> FirstDeliveryAction {
    if delivered_bytes > 0 || window_elapsed < FIRST_DELIVERY_RETRY_AFTER {
        FirstDeliveryAction::Wait
    } else if kick_retried && window_elapsed >= FIRST_DELIVERY_TIMEOUT {
        FirstDeliveryAction::Fail
    } else if !kick_retried {
        FirstDeliveryAction::RetryKick
    } else {
        FirstDeliveryAction::Wait
    }
}

/// Opening turn completed with spoken-text transcript but no audio at all —
/// the Live socket is producing text-only turns and will not recover.
fn is_transcript_only_turn(
    turn_done: bool,
    audio_chunks_out: u64,
    delivered_bytes: u64,
    out_tx_window: &str,
) -> bool {
    turn_done && audio_chunks_out == 0 && delivered_bytes == 0 && !out_tx_window.trim().is_empty()
}

/// After a function answer went up, a `turnComplete` this soon with no audio
/// since ends the turn that made the call, not the reply to it.
const TOOL_TURN_GRACE: Duration = Duration::from_millis(1_500);

/// A model turn that ends while a function call (appointments) is open — or right
/// after its answer went up, before any audio — has said nothing yet: the reply
/// is still owed. The text-only opening re-kick, the post-rotation check, the
/// closing / stalling checks and the "model owes a reply" flag must not read it
/// as a silent or finished turn.
fn tool_turn_quiet(calls_open: usize, since_answer: Option<Duration>, audio_since_answer: bool) -> bool {
    calls_open > 0 || since_answer.is_some_and(|d| d < TOOL_TURN_GRACE && !audio_since_answer)
}

/// The first-audio watchdog waits while the caller (or the carrier's prompt)
/// talks, and while a function call is open: the model speaks after its answer.
fn first_delivery_paused(activity_open: bool, caller_spoke_recently: bool, calls_open: usize) -> bool {
    activity_open || caller_spoke_recently || calls_open > 0
}

/// How long after the answer a model turn nobody asked for still counts as the
/// repeat (it came ~4 s later on the probe; the quiet-line nudge is 15 s away).
const REPEAT_GUARD_WINDOW: Duration = Duration::from_secs(12);

/// Had the model already begun its reply to the caller's last words when it
/// called a function? Audio of this turn is not enough: the opening 「喂？」 (or
/// any earlier line) was sent before the caller spoke, and a function call
/// often ends a turn of its own before the reply's audio starts. So the model
/// must also have sent audio since the caller's last end of turn.
fn spoke_since_caller(
    chunks_out: u64,
    chunks_at_turn_start: u64,
    downlink_bytes: u64,
    bytes_at_last_caller_eou: u64,
) -> bool {
    chunks_out > chunks_at_turn_start && downlink_bytes > bytes_at_last_caller_eou
}

/// Where the call stood when the model called propose_appointment after it
/// had already started speaking its reply.
#[derive(Debug, Clone, Copy, PartialEq)]
struct CallTurn {
    /// Model turns completed so far (the calling turn is the next one).
    turns_done: u64,
    /// Caller turns ended so far.
    caller_eous: u64,
}

/// propose_appointment called after the model had already said its line ("I'll
/// check and text you"): once that turn ends, Gemini often opens a turn of its
/// own and says the line again — SILENT or not. That turn — the first one after
/// the calling turn, asked for by nobody (no caller words since the call, no
/// kick of ours) — is not played and not written down, and its end is no turn
/// for the watchdogs. Calls made before the model spoke never arm it.
///
/// Only its audio starts a drop: the output transcript runs behind the audio,
/// and the calling turn's last words often come after its turnComplete — taken
/// for the repeat, they were cut from the record and the drop stayed on until
/// the next turnComplete, swallowing the AI's whole answer to the caller's next
/// sentence (~2.5 s of silence before it was asked for again). The caller
/// starting to talk ends a drop at once.
#[derive(Debug, Default)]
struct RepeatGuard {
    armed: Option<(CallTurn, tokio::time::Instant)>,
    /// A turn is being dropped (until its turnComplete, a cut-in or the caller talking).
    dropping: bool,
    /// The caller's words came in while a turn was being dropped: the drop ended
    /// there, and what was dropped may have been the answer to them, so it is
    /// asked for again unless the rest of the turn is heard.
    reply_owed: bool,
}

impl RepeatGuard {
    /// The silent answer to a call made at `call` went up.
    fn arm(&mut self, call: CallTurn, turns_done: u64, turn_started: bool, now: tokio::time::Instant) {
        // A later turn is already playing: what comes next was asked for.
        if turns_done > call.turns_done && turn_started {
            return;
        }
        self.armed = Some((call, now));
    }

    /// A kick of ours, a rotation, a sales verdict, caller words: the next turn is owed.
    fn clear(&mut self) {
        self.armed = None;
    }

    /// Model output arrives (`has_audio`: audio, else only its transcript): drop it?
    /// A transcript without audio belongs to the turn that is ending and is kept.
    fn drops(
        &mut self,
        has_audio: bool,
        turns_done: u64,
        turn_started: bool,
        caller_eous: u64,
        caller_talking: bool,
        now: tokio::time::Instant,
    ) -> bool {
        if self.dropping {
            return true;
        }
        let Some((call, at)) = self.armed else {
            return false;
        };
        if caller_talking || caller_eous != call.caller_eous || now.duration_since(at) >= REPEAT_GUARD_WINDOW {
            self.armed = None;
            return false;
        }
        if !has_audio {
            return false;
        }
        // Still the turn that made the call, or a turn already under way.
        if turns_done <= call.turns_done || turn_started {
            return false;
        }
        self.armed = None;
        self.dropping = true;
        true
    }

    fn is_dropping(&self) -> bool {
        self.dropping
    }

    /// The caller's words arrived: the model's next turn answers them. A turn
    /// being dropped may have been that answer (a short 「嗯」 the local VAD
    /// missed and whose transcript came late): the drop ends, and the reply is
    /// owed again if nothing more of that turn is heard.
    fn caller_words(&mut self) {
        self.armed = None;
        if self.dropping {
            self.dropping = false;
            self.reply_owed = true;
        }
    }

    /// The caller started talking (a new turn of theirs): whatever the model
    /// says next is for them.
    fn caller_started(&mut self) {
        self.armed = None;
        self.dropping = false;
    }

    /// The dropped turn may have answered the caller (see [`Self::caller_words`]).
    fn reply_owed(&self) -> bool {
        self.reply_owed
    }

    /// turnComplete: true when it ends the dropped turn. Clears what was owed.
    fn turn_complete(&mut self) -> bool {
        self.reply_owed = false;
        std::mem::take(&mut self.dropping)
    }

    /// The caller cut in: whatever comes next answers them.
    fn interrupted(&mut self) {
        self.armed = None;
        self.dropping = false;
        self.reply_owed = false;
    }
}

/// A function call's answer, back from its task.
struct ToolDone {
    /// Live socket generation it was asked on (a rotation drops older ones).
    gen: u32,
    /// Tracking key (the call id, or a stand-in when the server sent none).
    key: String,
    id: String,
    name: String,
    response: Value,
    /// The model waits for it (the watchdogs hold meanwhile); a non-blocking
    /// one is answered silently and changes nothing in the turn bookkeeping.
    blocking: bool,
    /// propose_appointment called after the model had started speaking its
    /// reply: where the call stood ([`RepeatGuard`]).
    after_speech: Option<CallTurn>,
}

/// International call (persona mode, the carrier's warning expected): what the
/// model says first is kept off the line until the warning is over. It used to
/// wait only with an opening prepared while the phone rang; a cold start, a
/// fresh retry or a text-only prewarm said 「喂？」 into the warning (twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntlGate {
    /// Model audio plays.
    Open,
    /// Waiting up to `until` for the warning: model audio is held.
    Waiting { until: tokio::time::Instant },
    /// The warning is playing (line voice came while waiting): model audio is
    /// dropped until the line goes quiet, at the latest at `until`.
    Prompt { until: tokio::time::Instant },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntlGateStep {
    Wait,
    /// No line voice in time: play what was held.
    Play,
    /// Line voice: the warning has started — drop what was held.
    PromptStarted,
    /// The warning is over: the model's audio plays again.
    PromptOver,
}

/// The line counts as quiet again after this much silence (as the local end of turn).
const CARRIER_PROMPT_QUIET: Duration = Duration::from_millis(LOCAL_EOU_SILENCE_MS as u64);

/// `line_voice`: the line has carried voice since pickup; `line_quiet_for`:
/// how long since it last did, `None` while it talks.
fn intl_gate_step(
    gate: IntlGate,
    now: tokio::time::Instant,
    line_voice: bool,
    line_quiet_for: Option<Duration>,
) -> IntlGateStep {
    match gate {
        IntlGate::Open => IntlGateStep::Wait,
        IntlGate::Waiting { .. } if line_voice => IntlGateStep::PromptStarted,
        IntlGate::Waiting { until } if now >= until => IntlGateStep::Play,
        IntlGate::Waiting { .. } => IntlGateStep::Wait,
        IntlGate::Prompt { until }
            if now >= until || line_quiet_for.is_some_and(|q| q >= CARRIER_PROMPT_QUIET) =>
        {
            IntlGateStep::PromptOver
        }
        IntlGate::Prompt { .. } => IntlGateStep::Wait,
    }
}

/// The gate a call starts with at unmute: every persona call that expects the
/// carrier's warning waits for it, whether or not an opening was prepared.
fn intl_gate_at_unmute(persona: bool, expects_warning: bool, now: tokio::time::Instant) -> IntlGate {
    if persona && expects_warning {
        IntlGate::Waiting { until: now + INTL_OPENING_HOLD }
    } else {
        IntlGate::Open
    }
}

/// Characters of a short reply in Chinese (「嗯嗯」「好」「對啊」「好的」).
const ACK_CHARS: &str = "嗯喔哦噢好對对的欸啊阿呃恩唔啦呀耶囉";
/// Short replies as words of their own ("okay", "mm", 「はい」, "vâng").
const ACK_WORDS: &[&str] = &[
    "ok", "okay", "mm", "mhm", "hm", "hmm", "uh", "huh", "yeah", "yes", "right", "sure", "はい", "うん", "네",
    "예", "sí", "si", "oui", "ja", "sim", "vâng", "dạ", "ya",
];

/// What was said without the thanks and short replies at its end
/// (「那就先這樣，好，謝謝，嗯嗯」 → 「那就先這樣」).
fn strip_trailing_fluff(text: &str) -> &str {
    let mut t = text;
    loop {
        let before = t.len();
        t = crate::locale::strip_trailing_thanks(t).trim_end_matches(|c: char| ACK_CHARS.contains(c));
        // The last word, when it is a short reply of its own.
        let start = t
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_alphanumeric() || *c == '\''))
            .map_or(0, |(i, c)| i + c.len_utf8());
        if start < t.len() && ACK_WORDS.contains(&t[start..].to_lowercase().as_str()) {
            t = &t[..start];
        }
        if t.len() == before {
            return t;
        }
    }
}

/// What was said ends as a question (「那就先這樣嗎」 with or without 「？」).
fn ends_as_question(text: &str) -> bool {
    let t = text.trim_end();
    if t.ends_with(['?', '？']) || t.ends_with("か。") || t.ends_with("까요") {
        return true;
    }
    let core = t
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches(|c: char| "喔哦囉啦呢耶齁欸啊".contains(c));
    core.ends_with('嗎') || core.ends_with('吗')
}

/// The caller's words with the piece that just came at their end. The window
/// skips a short piece it already holds (`push_tx_window`): 「嗎？」 after an
/// earlier question never got to its end.
fn with_latest<'a>(window: &'a str, latest: &str) -> std::borrow::Cow<'a, str> {
    let piece = latest.trim();
    if piece.is_empty() || window.trim_end().ends_with(piece) {
        std::borrow::Cow::Borrowed(window)
    } else {
        std::borrow::Cow::Owned(format!("{window}{piece}"))
    }
}

/// The caller said bye (or 「先這樣」) first, then more came of the same words:
/// 「那就先這樣」+「嗎？」, "That's all" + "for today, right?". A question takes the
/// goodbye back, and so do new words of their own; thanks and short replies
/// after it (「謝謝，嗯嗯」 in one piece) do not. `window`: the caller's words so
/// far; `latest`: the piece that just came.
fn caller_withdraws_bye(window: &str, latest: &str) -> bool {
    let heard = with_latest(window, latest);
    if ends_as_question(&heard) {
        return true;
    }
    !caller_wraps_up(strip_trailing_fluff(&heard)) && !strip_trailing_fluff(latest).trim().is_empty()
}

/// How long a hangup of ours waits for the phone to play what it still holds.
fn hangup_playout_wait(speech_left: Duration) -> Duration {
    speech_left.min(HANGUP_PLAYOUT_MAX)
}

/// Let the phone finish our last words before we hang up ([`HANGUP_PLAYOUT_MAX`]
/// at most; a hangup by the caller does not wait).
async fn let_the_phone_finish(play_head: &DownlinkPlayHead, phone_sr: u32, cancel: &AtomicBool) -> Duration {
    let start = tokio::time::Instant::now();
    let deadline = start + hangup_playout_wait(play_head.speech_left(phone_sr));
    while tokio::time::Instant::now() < deadline
        && !cancel.load(Ordering::Relaxed)
        && !play_head.speech_left(phone_sr).is_zero()
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    start.elapsed()
}

async fn send_text_before_deadline<S>(
    sink: &mut S,
    text: String,
    deadline: tokio::time::Instant,
    label: &str,
) -> Result<()>
where
    S: futures_util::Sink<WsMessage> + Unpin,
    S::Error: std::fmt::Display,
{
    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        bail!("{label} absolute deadline elapsed before send");
    }
    match tokio::time::timeout(remaining, sink.send(WsMessage::Text(text.into()))).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => bail!("{label}: {e}"),
        Err(_) => bail!("{label} timed out at absolute deadline"),
    }
}

fn first_delivery_window_start(
    audio_opened_at: &Arc<std::sync::Mutex<Option<tokio::time::Instant>>>,
) -> tokio::time::Instant {
    audio_opened_at
        .lock()
        .ok()
        .and_then(|opened_at| *opened_at)
        .unwrap_or_else(tokio::time::Instant::now)
}

fn api_send_deadline(
    delivered_bytes: &AtomicU64,
    first_delivery_deadline: tokio::time::Instant,
) -> tokio::time::Instant {
    let bounded = tokio::time::Instant::now() + API_SEND_TIMEOUT;
    if delivered_bytes.load(Ordering::Relaxed) == 0 {
        bounded.min(first_delivery_deadline)
    } else {
        bounded
    }
}

fn looks_like_goodbye(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    // Only clear farewells — do NOT treat "hang up" / "bad line" phrases as end (model used to
    // bail too early on noisy lines and hang up mid-message).
    const MARKS: &[&str] = &["再见", "再見", "掰掰", "拜拜", "拜了", "再会", "再會"];
    if MARKS.iter().any(|m| t.contains(m)) || crate::locale::ends_with_farewell(text, 3) {
        return true;
    }
    // AI wrap-up lines often end with 感謝您的來電 / 感谢来电 without 再見.
    let thanks = t.contains("感谢") || t.contains("感謝");
    let call = t.contains("来电") || t.contains("來電");
    thanks && call
}

/// Relay-verb phrases. Greeting 「我幫您轉達」 also matches — hangup must use
/// [`looks_like_complete_relay_ack`] so an opening line cannot arm goodbye.
fn looks_like_relay_verb(text: &str) -> bool {
    const MARKS: &[&str] = &[
        "會幫您轉達",
        "会帮您转达",
        "會幫你轉達",
        "会帮你转达",
        "幫您轉達",
        "帮您转达",
        "幫你轉達",
        "帮你转达",
        "幫忙轉達",
        "帮忙转达",
        "我會轉達",
        "我会转达",
        "會轉達",
        "会转达",
        "轉達給",
        "转达给",
        "轉達關於",
        "转达关于",
        "轉告",
        "转告",
        "幫您記下",
        "帮您记下",
        "已記下",
        "已记下",
        "記下來了",
        "记下来了",
        "已經記下",
        "已经记下",
        // Model often says 提醒 instead of 轉達 on a finished recap.
        "會提醒",
        "会提醒",
        "我會提醒",
        "我会提醒",
        "幫您提醒",
        "帮您提醒",
        "幫你提醒",
        "帮你提醒",
        "幫忙提醒",
        "帮忙提醒",
        "提醒王先生",
        "提醒給",
        "提醒给",
    ];
    MARKS.iter().any(|m| text.contains(m))
}

fn has_zh_relay(text: &str) -> bool {
    text.contains("轉達")
        || text.contains("转达")
        || text.contains("轉告")
        || text.contains("转告")
        || text.contains("提醒")
}

/// Matter words that mean the recap actually named the request (ASR 腳水費 still hits 水費).
fn has_relay_matter(text: &str) -> bool {
    const MARKS: &[&str] = &[
        "水費", "水费", "繳水", "缴水", "交水", "房租", "租金", "電費", "电费", "瓦斯",
        "管理費", "管理费", "停車", "停车", "包裹", "開會", "开会", "會議", "会议",
        "電話", "电话", "學費", "学费", "繳學", "缴学",
    ];
    MARKS.iter().any(|m| text.contains(m))
}

/// Opening / "what should I relay?" — must not arm hangup.
fn looks_like_opening_ask(text: &str) -> bool {
    const MARKS: &[&str] = &[
        "您說",
        "您请说",
        "您請說",
        "请说",
        "請說",
        "有什麼需要",
        "有什么需要",
        "有什麼要",
        "有什么要",
        "要幫忙轉達什麼",
        "要帮忙转达什么",
        "要轉達什麼",
        "要转达什么",
        "方便說一下",
        "方便说一下",
        "有要留話",
        "有要留话",
        "直接說就好",
        "直接说就好",
        "我可以幫您轉達",
        "我可以帮您转达",
        "記下來轉達",
        "记下来转达",
        "找哪位",
        "找誰",
        "找谁",
        "找王先生",
    ];
    MARKS.iter().any(|m| text.contains(m))
        && !has_relay_matter(text)
        && topic_after_guanyu(text).is_none()
        && !text.contains("轉達給王先生")
        && !text.contains("转达给王先生")
}

fn looks_like_clarifying_question(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return false;
    }
    if has_relay_matter(t) || text.contains("轉達給王先生") || text.contains("转达给王先生")
    {
        return false;
    }
    t.contains('？')
        || t.contains('?')
        || t.contains("嗎")
        || t.contains("吗")
        || t.contains("請問")
        || t.contains("请问")
        || t.contains("再說一次")
        || t.contains("再说一次")
}

/// Common speech-to-text slips in relay phrases. Matcher-only; does not rewrite notifications.
fn normalize_relay_stt(text: &str) -> String {
    let mut t = text.to_string();
    for (from, to) in [
        ("轉打", "轉達"),
        ("转打", "转达"),
    ] {
        if t.contains(from) {
            t = t.replace(from, to);
        }
    }
    t
}

fn strip_end_punct(text: &str) -> &str {
    text.trim_end_matches(|c: char| {
        matches!(
            c,
            '。' | '！' | '!' | '？' | '?' | '.' | '，' | ',' | '、' | ' ' | '\n' | '\t' | '　'
        )
    })
}

/// True when the model stalled after 關於 with no matter filled in.
fn looks_like_incomplete_relay(text: &str) -> bool {
    let t = strip_end_punct(text.trim());
    t.ends_with("關於") || t.ends_with("关于")
}

fn topic_after_guanyu(text: &str) -> Option<&str> {
    let (idx, needle_len) = if let Some(i) = text.rfind("關於") {
        (i, "關於".len())
    } else if let Some(i) = text.rfind("关于") {
        (i, "关于".len())
    } else {
        return None;
    };
    let rest = text[idx + needle_len..].trim_start_matches(|c: char| {
        matches!(c, '，' | ',' | '、' | ' ' | '　')
    });
    let end = rest
        .find(|c: char| matches!(c, '。' | '！' | '!' | '？' | '?' | '.' | '\n'))
        .unwrap_or(rest.len());
    let mut topic = rest[..end].trim();
    for suffix in ["的事情", "的事", "这件事", "這件事"] {
        if let Some(stripped) = topic.strip_suffix(suffix) {
            topic = stripped.trim();
            break;
        }
    }
    if let Some(stripped) = topic.strip_suffix('的') {
        topic = stripped.trim();
    }
    if topic.is_empty() {
        None
    } else {
        Some(topic)
    }
}

fn looks_like_noted_ack(text: &str) -> bool {
    const MARKS: &[&str] = &[
        "已記下",
        "已记下",
        "記下來了",
        "记下来了",
        "已經記下",
        "已经记下",
        "幫您記下",
        "帮您记下",
    ];
    MARKS.iter().any(|m| text.contains(m))
}

/// Content-ack that may arm hangup: a COMPLETE recap including the matter
/// (or a complete no-topic fallback). Greeting 「我幫您轉達」 must not match.
fn looks_like_complete_relay_ack(text: &str) -> bool {
    let text = normalize_relay_stt(text);
    if looks_like_incomplete_relay(&text) {
        return false;
    }
    if looks_like_opening_ask(&text) {
        return false;
    }
    if looks_like_clarifying_question(&text) {
        return false;
    }
    if !looks_like_relay_verb(&text) && !has_zh_relay(&text) {
        return false;
    }
    if topic_after_guanyu(&text).is_some() {
        return true;
    }
    let has_guanyu = text.contains("關於") || text.contains("关于");
    if has_guanyu {
        return false;
    }
    if looks_like_noted_ack(&text) && !looks_like_opening_ask(&text) {
        return true;
    }
    // Named fallback: 「好的，會幫您轉達給王先生。」 (only if they used that name class)
    if text.contains("轉達給王先生") || text.contains("转达给王先生") {
        return true;
    }
    if text.contains("提醒王先生") {
        return true;
    }
    // Neutral fallback when caller never named anyone: 「好的，會幫您轉達。」
    let stripped = strip_end_punct(text.trim());
    if stripped.ends_with("會幫您轉達") || stripped.ends_with("会帮您转达") {
        return true;
    }
    // Recap named the matter even if ASR mangled a word (腳水費 still has 水費).
    has_zh_relay(&text) && has_relay_matter(&text)
}

/// True when the recap STT actually named the request (水費 / 關於房租 / …).
fn recap_names_matter(text: &str) -> bool {
    let text = normalize_relay_stt(text);
    has_relay_matter(&text) || topic_after_guanyu(&text).is_some()
}

/// Short backchannel — must not arm hangup (the model said 「好的」, then the goodbye WAV played).
fn looks_like_backchannel(text: &str) -> bool {
    let t = strip_end_punct(text.trim());
    if t.is_empty() {
        return true;
    }
    if recap_names_matter(t) || has_zh_relay(t) {
        return false;
    }
    const MARKS: &[&str] = &[
        "好的", "好", "嗯", "喔", "哦", "啊", "了解", "收到", "嗯哼", "好喔", "好哦",
        "ok", "OK", "Okay", "okay",
    ];
    if MARKS.iter().any(|m| t.eq_ignore_ascii_case(m)) {
        return true;
    }
    t.chars().count() <= 4 && !looks_like_clarifying_question(t)
}

/// Recap-length model audio (16 kHz s16le ≈ 32 bytes/ms). 「好的」 was ~320ms.
const MIN_RECAP_AUDIO_MS: u32 = 1600;
/// Ignore uplink VAD this long after downlink so greeting sidetone is not a fake caller turn.
const DOWNLINK_ECHO_HOLD_MS: u64 = 500;

fn pcm16_ms(bytes: u64, sr: u32) -> u32 {
    let bps = u64::from(sr.max(8_000)) * 2;
    ((bytes.saturating_mul(1000)) / bps) as u32
}

/// After the caller has spoken, hang up on a recap-length model turn when STT is
/// empty/mangled — never on a greeting backchannel, and never while they still talk.
fn post_caller_model_turn_should_hangup(
    caller_had_voice: bool,
    model_spoke_after_caller: bool,
    out_text: &str,
    post_caller_audio_ms: u32,
    caller_still_speaking: bool,
    allow_matterless: bool,
) -> bool {
    if !caller_had_voice || !model_spoke_after_caller || caller_still_speaking {
        return false;
    }
    if looks_like_incomplete_relay(out_text) {
        return false;
    }
    if looks_like_opening_ask(out_text) || looks_like_clarifying_question(out_text) {
        return false;
    }
    if recap_names_matter(out_text) {
        return true;
    }
    if looks_like_backchannel(out_text) || out_text.trim().is_empty() {
        return post_caller_audio_ms >= MIN_RECAP_AUDIO_MS;
    }
    if looks_like_complete_relay_ack(out_text) {
        return allow_matterless || post_caller_audio_ms >= MIN_RECAP_AUDIO_MS;
    }
    false
}

fn clarify_or_recap_kick_text() -> String {
    json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{
                    "text": "對方剛才已經說話了。禁止只回「好的／嗯」或直接再見。\
若你聽懂事由：立刻用一句完整轉達句，把聽到的實際事由講進去（例如水費就說繳水費）。\
若沒聽清：只問一句「請問要轉達什麼事？」。不要說再見，不要空轉達。"
                }]
            }],
            "turnComplete": true
        }
    })
    .to_string()
}

/// Back-compat alias used by tests / logs: complete recap only.
fn looks_like_relay_ack(text: &str) -> bool {
    looks_like_complete_relay_ack(text)
}

/// After the paced player has drained the current turn, wait this long before
/// starting the hangup delay (avoids overlapping the last recap syllable).
const RELAY_AUDIO_IDLE_MS: u64 = 500;
/// Treat leftover smaller than one 40 ms @ 16 kHz frame as empty.
pub const PLAYER_DRAIN_SLACK_BYTES: u64 = 1280;

/// Paced Gemini → phone player occupancy. Recap STT may arm hangup intent while
/// audio is still queued; goodbye WAV / hangup must wait until this is idle.
#[derive(Default)]
pub struct DownlinkPlayHead {
    queued_bytes: AtomicU64,
    outbound: AtomicBool,
    last_out: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// Bumped by the session when the caller cuts in; the player drops the rest
    /// of the turn (here and on the phone) when it sees a new value.
    flush_seq: AtomicU64,
    /// Player's estimate of when the phone finishes the AI audio sent so far.
    speech_tail: std::sync::Mutex<Option<tokio::time::Instant>>,
    /// The model is still streaming the current reply (set by the session).
    generating: AtomicBool,
}

impl DownlinkPlayHead {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn request_flush(&self) {
        self.flush_seq.fetch_add(1, Ordering::Relaxed);
    }

    pub fn flush_seq(&self) -> u64 {
        self.flush_seq.load(Ordering::Relaxed)
    }

    pub fn enqueue(&self, n: u64) {
        if n == 0 {
            return;
        }
        self.queued_bytes.fetch_add(n, Ordering::Relaxed);
    }

    pub fn dequeue(&self, n: u64) {
        saturating_sub_u64(&self.queued_bytes, n);
        self.outbound.store(true, Ordering::Relaxed);
        if let Ok(mut g) = self.last_out.lock() {
            *g = Some(tokio::time::Instant::now());
        }
    }

    pub fn drop_unplayed(&self, n: u64) {
        saturating_sub_u64(&self.queued_bytes, n);
    }

    pub fn set_outbound(&self, on: bool) {
        self.outbound.store(on, Ordering::Relaxed);
    }

    pub fn queued(&self) -> u64 {
        self.queued_bytes.load(Ordering::Relaxed)
    }

    pub fn is_outbound(&self) -> bool {
        self.outbound.load(Ordering::Relaxed)
    }

    /// True when the player has no remaining turn audio (slack = one frame).
    pub fn is_drained_slack(&self, slack: u64) -> bool {
        self.queued() <= slack && !self.is_outbound()
    }

    pub fn drained_for(&self, idle: Duration, slack: u64) -> bool {
        if !self.is_drained_slack(slack) {
            return false;
        }
        match self.last_out.lock().ok().and_then(|g| *g) {
            None => true,
            Some(t) => t.elapsed() >= idle,
        }
    }

    pub fn set_speech_tail(&self, at: tokio::time::Instant) {
        if let Ok(mut g) = self.speech_tail.lock() {
            *g = Some(at);
        }
    }

    /// AI audio not yet played by the phone: still queued here plus what the
    /// phone holds (the player sends up to ~1 s ahead). Less than one frame
    /// queued here counts as nothing, as for [`Self::ai_audible`]: the player
    /// keeps such a remainder for the next reply, and counted it held every
    /// quiet-line clock (15 s still-there, 25 s end) until the 5-minute cap.
    pub fn speech_left(&self, sample_rate: u32) -> Duration {
        let queued_bytes = self.queued();
        let queued = if queued_bytes <= PLAYER_DRAIN_SLACK_BYTES {
            Duration::ZERO
        } else {
            Duration::from_millis(u64::from(pcm16_ms(queued_bytes, sample_rate)))
        };
        let on_phone = self
            .speech_tail
            .lock()
            .ok()
            .and_then(|g| *g)
            .map_or(Duration::ZERO, |t| {
                t.saturating_duration_since(tokio::time::Instant::now())
            });
        queued + on_phone
    }

    /// The caller is (still) hearing our reply: audio queued here, on the phone,
    /// or played within the last `tail`.
    pub fn ai_audible(&self, tail: Duration) -> bool {
        if self.queued() > PLAYER_DRAIN_SLACK_BYTES {
            return true;
        }
        self.speech_tail
            .lock()
            .ok()
            .and_then(|g| *g)
            .is_some_and(|t| tokio::time::Instant::now() < t + tail)
    }

    pub fn set_generating(&self, on: bool) {
        self.generating.store(on, Ordering::Relaxed);
    }

    /// More of the current reply is still coming from the model.
    pub fn generating(&self) -> bool {
        self.generating.load(Ordering::Relaxed)
    }
}

fn saturating_sub_u64(a: &AtomicU64, n: u64) {
    loop {
        let cur = a.load(Ordering::Relaxed);
        let next = cur.saturating_sub(n);
        if a
            .compare_exchange_weak(cur, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return;
        }
    }
}

/// Caller said goodbye first: give the model this long after the caller's turn
/// ended to answer it before the hangup clock may start.
const CALLER_BYE_MODEL_WAIT: Duration = Duration::from_millis(3_000);

/// The hangup clock may only run while our audio has drained, the caller is
/// not mid-turn, and (caller-initiated bye) the model has started its answer.
fn goodbye_clock_may_run(player_idle: bool, activity_open: bool, waiting_model_reply: bool) -> bool {
    player_idle && !activity_open && !waiting_model_reply
}

/// The hangup clock also waits on the model: the caller said bye first and the
/// model has not answered yet (up to [`CALLER_BYE_MODEL_WAIT`]), or the caller
/// talked on after our closing and neither their words nor the model's next
/// turn have shown yet whether that was a farewell or something new
/// (a long polite farewell ending in 拜拜
/// cancelled the hangup twice as "new content").
fn goodbye_waits_on_model(
    bye_from_caller: bool,
    post_arm_talk: bool,
    model_replied: bool,
    since_caller_eou: Option<Duration>,
) -> bool {
    let fresh = since_caller_eou.map_or(true, |d| d < CALLER_BYE_MODEL_WAIT);
    (bye_from_caller && !model_replied && fresh) || (post_arm_talk && (model_replied || fresh))
}

/// Persona closing armed: the hangup clock waits until the caller's latest words
/// end the call, or the line has been quiet [`PERSONA_BYE_SILENCE`] since our
/// farewell (otherwise our own 掰掰 hangs up while the
/// caller is still asking when to call back).
fn awaits_caller_bye(caller_end_signal: bool, quiet_after_bye: bool) -> bool {
    !caller_end_signal && !quiet_after_bye
}

/// Arm (intent) is independent of play. Only start the hangup delay once the
/// player has drained the current turn; new downlink resets the clock.
fn tick_goodbye_play_clock(
    armed: bool,
    player_idle: bool,
    hangup_delay: Duration,
    now: tokio::time::Instant,
    play_at: &mut Option<tokio::time::Instant>,
) -> bool {
    if !armed || !player_idle {
        *play_at = None;
        return false;
    }
    let t0 = *play_at.get_or_insert(now);
    now.saturating_duration_since(t0) >= hangup_delay
}

/// CJK/fullwidth character written without spaces between words (Han, kana,
/// CJK punctuation, fullwidth forms). Not Hangul: Korean spaces its words.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F | 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// With `languageCodes` zh-TW the STT sometimes spaces every character
/// (「這 是 國 際 電 話」) — keyword / bye matching then misses 「保 險」. Drop spaces
/// next to CJK; keep them between Latin words (「Hallo, ist jemand」).
fn tidy_cjk_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c.is_whitespace() {
            let prev = chars[..i].iter().rev().find(|c| !c.is_whitespace());
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if prev.is_some_and(|&p| is_cjk(p)) || next.is_some_and(|&n| is_cjk(n)) {
                continue;
            }
        }
        out.push(c);
    }
    out.trim().to_string()
}

fn extract_transcription_snippets(v: &Value) -> (Vec<String>, Vec<String>) {
    let mut input = Vec::new();
    let mut output = Vec::new();
    let push = |dest: &mut Vec<String>, path: &str| {
        if let Some(t) = v
            .pointer(path)
            .and_then(|x| x.as_str())
            .map(tidy_cjk_spaces)
            .filter(|s| !s.is_empty())
        {
            dest.push(t);
        }
    };
    push(&mut input, "/serverContent/inputTranscription/text");
    push(&mut input, "/inputTranscription/text");
    push(&mut output, "/serverContent/outputTranscription/text");
    push(&mut output, "/outputTranscription/text");
    (input, output)
}

/// Rolling window so streaming partials still match 再見 across chunks.
fn push_tx_window(window: &mut String, piece: &str, cap: usize) {
    if piece.is_empty() {
        return;
    }
    // Cumulative full-text updates often re-send the same prefix — avoid dup bloat.
    if window.ends_with(piece) || window.contains(piece) && piece.chars().count() < 4 {
        return;
    }
    if piece.contains(window.as_str()) && !window.is_empty() {
        *window = piece.to_string();
        return;
    }
    window.push_str(piece);
    if window.len() > cap {
        // Byte cap on CJK text: never cut inside a character (a long
        // stall call panicked on `is_char_boundary` here).
        let mut drop = window.len() - cap;
        while !window.is_char_boundary(drop) {
            drop += 1;
        }
        window.drain(..drop);
    }
}

fn ws_url(api_key: &str) -> String {
    format!("{WS_BASE}?key={}", urlencoding_key(api_key.trim()))
}

fn urlencoding_key(k: &str) -> String {
    k.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

fn text_from_ws(msg: WsMessage) -> Option<String> {
    match msg {
        WsMessage::Text(t) => Some(t.to_string()),
        WsMessage::Binary(b) => String::from_utf8(b.to_vec()).ok(),
        _ => None,
    }
}

fn pcm_rms_s16le(pcm: &[u8]) -> f32 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.0;
    }
    let mut acc = 0f64;
    for i in 0..n {
        let s = i16::from_le_bytes([pcm[2 * i], pcm[2 * i + 1]]) as f64;
        acc += s * s;
    }
    (acc / n as f64).sqrt() as f32
}

/// Advance local VAD on one uplink frame.
///
/// Before `activity_open`, sub-threshold frames do not instantly zero `voiced_ms`
/// (quiet PSTN RMS dips would never reach `LOCAL_MIN_VOICE_MS`). Only a run of
/// `LOCAL_PRE_ACTIVITY_HANGOVER_MS` continuous sub-threshold silence resets it.
fn tick_local_vad(
    is_voice: bool,
    activity_open: bool,
    frame_ms: u32,
    voiced_ms: &mut u32,
    silence_ms: &mut u32,
) {
    if is_voice {
        *voiced_ms = voiced_ms.saturating_add(frame_ms);
        *silence_ms = 0;
        return;
    }
    *silence_ms = silence_ms.saturating_add(frame_ms);
    if !activity_open && *silence_ms >= LOCAL_PRE_ACTIVITY_HANGOVER_MS {
        *voiced_ms = 0;
    }
}

/// Setup `realtimeInputConfig`. Server VAD: the caller's line streams
/// continuously and Gemini decides when the turn ends. Otherwise the local VAD
/// below sends activityStart/End (cloud VAD used to wait too long or need a
/// second phrase on quiet PSTN audio with the older Live models).
fn realtime_input_config(server_vad: bool) -> Value {
    let detection = if server_vad {
        json!({ "disabled": false, "silenceDurationMs": SERVER_VAD_SILENCE_MS })
    } else {
        json!({ "disabled": true })
    };
    json!({
        "automaticActivityDetection": detection,
        "activityHandling": "START_OF_ACTIVITY_INTERRUPTS",
        "turnCoverage": "TURN_INCLUDES_ONLY_ACTIVITY"
    })
}

/// Open caller turn is over: the usual pause after a phrase, or a longer one
/// after a lone short word (see `LOCAL_SHORT_TURN_EOU_SILENCE_MS`).
fn local_turn_complete(voiced_ms: u32, silence_ms: u32) -> bool {
    (voiced_ms >= LOCAL_MIN_TURN_VOICE_MS && silence_ms >= LOCAL_EOU_SILENCE_MS)
        || (voiced_ms >= LOCAL_MIN_VOICE_MS && silence_ms >= LOCAL_SHORT_TURN_EOU_SILENCE_MS)
}

/// AGC quiet VoLTE frames so Gemini ASR gets usable level (no soft/muffled uplink).
fn agc_uplink_frame(pcm: &[u8]) -> Vec<u8> {
    let rms = pcm_rms_s16le(pcm);
    if rms < 40.0 || rms > 12_000.0 {
        return pcm.to_vec();
    }
    let gain = (UPLINK_TARGET_RMS / rms).clamp(1.0, UPLINK_GAIN_MAX);
    if gain < 1.12 {
        return pcm.to_vec();
    }
    let mut out = pcm.to_vec();
    for i in (0..out.len()).step_by(2) {
        if i + 1 >= out.len() {
            break;
        }
        let s = i16::from_le_bytes([out[i], out[i + 1]]) as f32 * gain;
        let v = s.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        let b = v.to_le_bytes();
        out[i] = b[0];
        out[i + 1] = b[1];
    }
    out
}

/// Mild peak lift on Gemini → phone downlink (24 kHz resample path often soft).
fn boost_downlink_for_phone(pcm: &[u8]) -> Vec<u8> {
    let rms = pcm_rms_s16le(pcm);
    if rms < 80.0 || rms > 9000.0 {
        return pcm.to_vec();
    }
    let gain = (5200.0 / rms).clamp(1.0, 3.5);
    if gain < 1.08 {
        return pcm.to_vec();
    }
    let mut out = pcm.to_vec();
    for i in (0..out.len()).step_by(2) {
        if i + 1 >= out.len() {
            break;
        }
        let s = i16::from_le_bytes([out[i], out[i + 1]]) as f32 * gain;
        let v = s.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        let b = v.to_le_bytes();
        out[i] = b[0];
        out[i + 1] = b[1];
    }
    out
}

/// Run one Live session until cancel, goodbye, timeout, or WS close.
///
/// - `uplink_rx`: phone PCM s16le mono @ `phone_sr` (usually 16000)
/// - `on_downlink`: play PCM to phone (already at `phone_sr`)
/// - `transcript_out`: optional shared buffer for input/output transcription (for the notification)
/// - `audio_open`: **false** during ConnectAtAnswer warm-up; set true on UnmuteAfterGreet
///   so greet WAV can finish before any Gemini speech hits telephony. Greets kick is held
///   until this flips. Wall-clock talk timers start after unmute.
/// - `setup_ready`: set true when WS setupComplete (or first server audio during warm).
/// - `voice_name`: prebuilt voice (Aoede/Kore/…); empty → default.
/// - `goodbye_hangup_delay_ms`: after player drain + idle, wait before clone WAV (default 2000).
/// - `force_hangup`: set true only after the current turn has fully played so agent
///   can WS-hangup while Live session is still tearing down (cancel-race safe).
/// - `delivered_bytes`: successful Gemini PCM bytes written into the phone media WS.
/// - `play_head`: paced player occupancy; hangup WAV is gated on drain, not STT.
/// - `persona_max_secs`: persona-mode cap on a normal call's AI talk (setting
///   `persona_normal_max_min`); a call judged sales / scam switches to `StallCfg::max_secs`.
/// - `server_vad`: Gemini decides when the caller's turn ends (setting
///   `gemini_server_vad`); false = local activityStart/End.
/// - `barge_in`: set by the call's echo check (echo-free line + setting
///   `gemini_barge_in`): the caller is heard while the AI talks and cutting in
///   stops our playback.
/// - `thinking_level`: setting `gemini_thinking_level`, sent only to models that
///   need it (see [`live_thinking_level`]).
/// - `intl_caller`: asked at pickup whether this call opens with the carrier's
///   international-call warning — persona mode then holds the opening for it (see
///   [`INTL_OPENING_HOLD`]). Asked at pickup, not at setup: a ring prewarm starts
///   before the phone knows which SIM the call came in on.
/// - `lang_plan`: the call's languages ([`call_lang_plan`]). Persona mode with an
///   opening language: the opening line is said in it. Every call's system prompt
///   (the stall session's too) gets the note to answer in the caller's language
///   ([`session_prompt`]). Its speech-to-text hints go into the setup for the
///   caller's side; the AI's side gets the opening and the setting's language.
///
/// On success, returns why the session ended so the agent can play goodbye WAV.
pub async fn run_session(
    api_key: &str,
    model: &str,
    system_prompt: &str,
    phone_sr: u32,
    mut uplink_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    mut on_downlink: impl FnMut(Vec<u8>) + Send,
    cancel: Arc<AtomicBool>,
    serial: &str,
    transcript_out: Option<Arc<std::sync::Mutex<String>>>,
    audio_open: Arc<AtomicBool>,
    audio_opened_at: Arc<std::sync::Mutex<Option<tokio::time::Instant>>>,
    setup_ready: Arc<AtomicBool>,
    voice_name: &str,
    goodbye_hangup_delay_ms: u64,
    force_hangup: Option<Arc<AtomicBool>>,
    delivered_bytes: Arc<AtomicU64>,
    play_head: Arc<DownlinkPlayHead>,
    persona: bool,
    persona_max_secs: u64,
    stall: Option<StallCfg>,
    server_vad: bool,
    barge_in: Arc<AtomicBool>,
    thinking_level: &str,
    intl_caller: IntlPromptCheck,
    appointments: Option<crate::appointments::ApptSession>,
    lang_plan: CallLangPlan,
) -> Result<SessionEndReason> {
    let opening_lang = lang_plan.opening.filter(|_| persona);
    let stall = stall.filter(|_| persona).map(|mut c| {
        c.system_prompt = session_prompt(c.system_prompt.as_str());
        c
    });
    // Appointments: persona calls only (setting `appointment_mode`).
    let appt = appointments.filter(|_| persona);
    let key = api_key.trim();
    if key.is_empty() {
        bail!("empty Gemini API key");
    }
    let model = {
        let m = model.trim();
        if m.is_empty() {
            DEFAULT_MODEL
        } else {
            m
        }
    };
    let model_path = if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    };
    let url = ws_url(key);
    info!(
        "Gemini Live connecting serial={serial} model={model_path} phone_sr={phone_sr} key_len={}",
        key.len()
    );

    if cancel.load(Ordering::Relaxed) {
        return Ok(SessionEndReason::Cancelled);
    }

    let voice = {
        let v = voice_name.trim();
        if v.is_empty() {
            default_voice_name()
        } else {
            v
        }
    };
    let hangup_delay_ms = goodbye_hangup_delay_ms.clamp(200, 15_000);
    // The stored prompt stays as it is; appointments add their part after it
    // (with the current local time), and the tools.
    let base_prompt = session_prompt(system_prompt);
    let system_prompt = call_system_prompt(&base_prompt, appt.as_ref(), chrono::Local::now());
    let thinking = live_thinking_level(model, thinking_level);
    // Low-latency turn-taking: high EOU sensitivity + short silence.
    let build_setup = |non_blocking: bool| -> Value {
        let mut setup = json!({
            "setup": {
                "model": model_path,
                "generationConfig": {
                    "responseModalities": ["AUDIO"],
                    "speechConfig": {
                        "voiceConfig": {
                            "prebuiltVoiceConfig": {
                                "voiceName": voice
                            }
                        }
                    }
                },
                "systemInstruction": {
                    "parts": [{ "text": system_prompt.as_str() }]
                },
                // Capture speech-to-text for the post-call summary. Language hints:
                // auto-detect read a short Chinese "hello?" as German / Hindi / Korean,
                // and stall / carrier-prompt / bye detection all read this STT; a caller
                // from abroad gets their own language first (or auto-detect).
                "inputAudioTranscription": stt_caller_config(lang_plan.caller_stt.as_deref()),
                "outputAudioTranscription": stt_ai_config(opening_lang),
                "realtimeInputConfig": realtime_input_config(server_vad)
            }
        });
        if let Some(level) = thinking {
            if let Some(gc) = setup
                .pointer_mut("/setup/generationConfig")
                .and_then(|x| x.as_object_mut())
            {
                gc.insert("thinkingConfig".into(), json!({ "thinkingLevel": level }));
            }
        }
        with_appointment_tools(&mut setup, appt.is_some(), non_blocking);
        setup
    };
    // Appointments: propose_appointment goes NON_BLOCKING unless the API refused
    // that before; a setup that fails with it is sent once more without it.
    let mut non_blocking = appt.is_some() && !NON_BLOCKING_REFUSED.load(Ordering::Relaxed);
    let mut debug_raw_left = debug_raw_budget();
    let (mut sink, mut stream, setup, mut conn_opened_at, setup_ok) = 'connect: loop {
        let connect_fut = tokio_tungstenite::connect_async(&url);
        let (ws, resp) = match tokio::time::timeout(CONNECT_TIMEOUT, connect_fut).await {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => bail!("gemini live ws connect: {e:#}"),
            Err(_) => bail!(
                "gemini live ws connect timeout after {}s",
                CONNECT_TIMEOUT.as_secs()
            ),
        };
        info!(
            "Gemini Live ws open serial={serial} status={:?} (ConnectAtAnswer / cold)",
            resp.status()
        );
        let (mut sink, mut stream) = ws.split();
        let setup = build_setup(non_blocking);
        // Same setup for every call (stalling included): long calls rotate to a fresh
        // session every ~8 min instead (rotation setups add `historyConfig`).
        send_text_before_deadline(
            &mut sink,
            setup.to_string(),
            tokio::time::Instant::now() + SETUP_SEND_TIMEOUT,
            "gemini setup send",
        )
        .await?;
        info!(
            "Gemini Live setup sent serial={serial} voice={voice} stall={} max_s={} server_vad={server_vad} thinking={} appointments={} non_blocking={non_blocking}",
            stall.is_some(),
            if persona { persona_max_secs } else { LIVE_MAX_SECS },
            thinking.unwrap_or("-"),
            appt.as_ref().map_or("off", |a| a.mode.as_str())
        );
        let conn_opened_at = tokio::time::Instant::now();

        let setup_deadline = tokio::time::Instant::now() + SETUP_TIMEOUT;
        let mut setup_ok = false;
        let mut failed: Option<String> = None;
        while tokio::time::Instant::now() < setup_deadline {
            if cancel.load(Ordering::Relaxed) {
                info!("Gemini Live setup aborted (hangup) serial={serial}");
                let _ = sink.close().await;
                return Ok(SessionEndReason::Cancelled);
            }
            match tokio::time::timeout(Duration::from_millis(100), stream.next()).await {
                Ok(Some(Ok(msg))) => {
                    if let WsMessage::Close(frame) = &msg {
                        failed = Some(format!("gemini ws closed before setup: {frame:?}"));
                        break;
                    }
                    if let Some(t) = text_from_ws(msg) {
                        if t.contains("setupComplete") || t.contains("setup_complete") {
                            setup_ok = true;
                            info!("Gemini Live setupComplete serial={serial}");
                            break;
                        }
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            if let Some(err) = v.get("error") {
                                failed = Some(format!("gemini setup error: {err}"));
                                break;
                            }
                            append_transcriptions(&v, &transcript_out);
                            // Never forward model audio during setup / pre-unmute warm.
                            let mut discard = |_: Vec<u8>| {};
                            if handle_server_msg(&v, phone_sr, &mut discard) > 0 {
                                setup_ok = true;
                                info!("Gemini Live setup voice marker serial={serial} (discarded; mute until UnmuteAfterGreet)");
                                break;
                            }
                        }
                    }
                }
                Ok(Some(Err(e))) => {
                    failed = Some(format!("gemini ws error during setup: {e}"));
                    break;
                }
                Ok(None) => {
                    failed = Some("gemini ws closed (none) before setup".to_string());
                    break;
                }
                Err(_) => continue,
            }
        }
        if let Some(why) = failed {
            if non_blocking {
                // Maybe the model / API does not take NON_BLOCKING: the old way once
                // more (propose_appointment blocking, its answer still silent).
                if non_blocking_refused(&why) {
                    NON_BLOCKING_REFUSED.store(true, Ordering::Relaxed);
                }
                warn!(
                    "Gemini Live setup with a NON_BLOCKING function failed serial={serial} ({why}) — sending it again without"
                );
                non_blocking = false;
                continue 'connect;
            }
            bail!("{why}");
        }
        break 'connect (sink, stream, setup, conn_opened_at, setup_ok);
    };
    if !setup_ok {
        warn!("Gemini Live no setupComplete yet serial={serial} — warm hold still; setup_ready left false until later msg");
    } else {
        setup_ready.store(true, Ordering::Relaxed);
    }

    // Greets kick payload: warm pre-gen buffers audio; cold path kicks after open.
    // Persona folds the desk note into the prompt softly (no forced first line).
    let owner_status = if persona {
        String::new()
    } else {
        extract_owner_status(&system_prompt)
    };
    let greets_kick_text = if persona {
        persona_kick_client_content(opening_lang)
    } else {
        greets_kick_client_content_for(&owner_status)
    };
    info!(
        "Gemini Live greets kick ready serial={serial} persona={persona} owner_status_chars={} opening_lang={}",
        owner_status.chars().count(),
        opening_lang.map_or("-", |l| l.code())
    );
    // Max ~12s of 16 kHz mono s16le held until UnmuteAfterGreet (never inject mid-IVR).
    const PREBUF_CAP_BYTES: usize = 16_000 * 2 * 12;
    let mut greets_kicked = false;
    let mut prebuf: Vec<u8> = Vec::with_capacity(64 * 1024);
    let mut prebuf_chunks: u64 = 0;
    let mut prebuf_flushed_bytes: u64 = 0;
    // Warm opening answered with text only (no audio): re-kicks so far, and
    // whether they ran out before the dialogue opened.
    let mut warm_rekicks: u32 = 0;
    let mut warm_text_only = false;
    // Transcript length before the held opening (cut back if it is never played).
    let mut opening_mark: Option<usize> = None;
    // Set when an international hold ends on line voice (the carrier's prompt).
    let mut carrier_prompt_until: Option<tokio::time::Instant> = None;

    // ConnectAtAnswer warmhold: generate first AI turn *silently* while period WAV plays.
    // Buffer model audio here; flush to telephony only when audio_open (UnmuteAfterGreet).
    if !audio_open.load(Ordering::Relaxed) {
        info!(
            "Gemini Live ConnectAtAnswer warmhold serial={serial} (pre-gen greets muted until UnmuteAfterGreet)"
        );
        // Only the opening turn lands in the transcript while held (no caller audio
        // goes up yet), so a text-only opening can be cut back to here.
        let warm_transcript_mark = transcript_out
            .as_ref()
            .and_then(|b| b.lock().ok().map(|g| g.len()))
            .unwrap_or(0);
        opening_mark = Some(warm_transcript_mark);
        if setup_ready.load(Ordering::Relaxed) {
            if send_text_before_deadline(
                &mut sink,
                greets_kick_text.clone(),
                tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                "gemini warm greets kick send",
            )
            .await
            .is_ok()
            {
                greets_kicked = true;
                info!("Gemini Live warm greets kick sent serial={serial} (buffered until unmute)");
            }
        }
        while !audio_open.load(Ordering::Relaxed) {
            if cancel.load(Ordering::Relaxed) {
                info!("Gemini Live warmhold cancelled serial={serial}");
                let _ = sink.close().await;
                return Ok(SessionEndReason::Cancelled);
            }
            // Drop any buffered uplink frames (should be empty while phase != LiveAi).
            while uplink_rx.try_recv().is_ok() {}
            match tokio::time::timeout(Duration::from_millis(40), stream.next()).await {
                Ok(Some(Ok(msg))) => {
                    if matches!(msg, WsMessage::Close(_)) {
                        bail!("gemini ws closed during warmhold");
                    }
                    if let Some(t) = text_from_ws(msg) {
                        if t.contains("setupComplete") || t.contains("setup_complete") {
                            if !setup_ready.load(Ordering::Relaxed) {
                                setup_ready.store(true, Ordering::Relaxed);
                                info!(
                                    "Gemini Live setupComplete (during warmhold) serial={serial}"
                                );
                            }
                            if !greets_kicked {
                                if send_text_before_deadline(
                                    &mut sink,
                                    greets_kick_text.clone(),
                                    tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                    "gemini warm greets kick after setup send",
                                )
                                .await
                                .is_ok()
                                {
                                    greets_kicked = true;
                                    info!(
                                        "Gemini Live warm greets kick sent serial={serial} (after setupComplete)"
                                    );
                                }
                            }
                            continue;
                        }
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            if debug_raw_left > 0 {
                                debug_raw_left -= 1;
                                info!(
                                    "Gemini Live raw serial={serial} phase=warm msg={}",
                                    redact_long_strings(&v)
                                );
                            }
                            if let Some(err) = v.get("error") {
                                bail!("gemini warmhold error: {err}");
                            }
                            append_transcriptions(&v, &transcript_out);
                            let mut capture = |pcm: Vec<u8>| {
                                if pcm.is_empty() {
                                    return;
                                }
                                if prebuf.len() >= PREBUF_CAP_BYTES {
                                    return;
                                }
                                let room = PREBUF_CAP_BYTES.saturating_sub(prebuf.len());
                                let take = pcm.len().min(room);
                                prebuf.extend_from_slice(&pcm[..take]);
                                prebuf_chunks = prebuf_chunks.saturating_add(1);
                            };
                            let _ = handle_server_msg(&v, phone_sr, &mut capture);
                            // Opening answered with text only (in one run, 4 of 6
                            // openings): ask again now, while the phone still rings,
                            // instead of after pickup. The unheard line leaves the transcript.
                            if greets_kicked
                                && prebuf.is_empty()
                                && v.pointer("/serverContent/turnComplete")
                                    .and_then(|x| x.as_bool())
                                    == Some(true)
                            {
                                let dropped = truncate_transcript(&transcript_out, warm_transcript_mark);
                                if warm_rekicks < WARM_TEXT_ONLY_REKICKS {
                                    warm_rekicks += 1;
                                    match send_text_before_deadline(
                                        &mut sink,
                                        greets_kick_text.clone(),
                                        tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                        "gemini warm text-only re-kick send",
                                    )
                                    .await
                                    {
                                        Ok(()) => warn!(
                                            "Gemini Live warm opening text-only (0 audio) serial={serial} out_stt={dropped:?} — re-kick n={warm_rekicks} before pickup"
                                        ),
                                        Err(e) => warn!(
                                            "Gemini Live warm text-only re-kick failed serial={serial}: {e:#}"
                                        ),
                                    }
                                } else if !warm_text_only {
                                    warm_text_only = true;
                                    warn!(
                                        "Gemini Live warm opening text-only {warm_rekicks}x serial={serial} out_stt={dropped:?} — kick again when the dialogue opens"
                                    );
                                }
                            }
                        }
                    }
                }
                Ok(Some(Err(e))) => bail!("gemini ws error during warmhold: {e}"),
                Ok(None) => bail!("gemini ws closed (none) during warmhold"),
                Err(_) => {
                    if !greets_kicked && setup_ready.load(Ordering::Relaxed) {
                        if send_text_before_deadline(
                            &mut sink,
                            greets_kick_text.clone(),
                            tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                            "gemini warm greets kick ready send",
                        )
                        .await
                        .is_ok()
                        {
                            greets_kicked = true;
                            info!(
                                "Gemini Live warm greets kick sent serial={serial} (setup already ready)"
                            );
                        }
                    }
                }
            }
        }
        info!(
            "Gemini Live UnmuteAfterGreet serial={serial} — dialogue open prebuf_bytes={} chunks={} warm_rekicks={warm_rekicks}",
            prebuf.len(),
            prebuf_chunks
        );
        if prebuf.is_empty() && warm_text_only {
            // Every held opening came back text-only: kick right away below (the
            // live loop then re-kicks / opens a fresh session if it repeats).
            greets_kicked = false;
        }
    } else if !setup_ready.load(Ordering::Relaxed) {
        // Cold path: open immediately after setup attempt; allow dialogue if WS is up.
        setup_ready.store(true, Ordering::Relaxed);
    }
    // International call (asked now, at unmute: a ring prewarm starts before the
    // phone knows which SIM the call came in on): whatever the model says first —
    // the opening prepared while it rang, or the one it says now — waits for the
    // carrier's warning (see IntlGate).
    let intl_call = persona && intl_caller();
    let mut intl_gate = intl_gate_at_unmute(persona, intl_call, tokio::time::Instant::now());
    if intl_gate != IntlGate::Open {
        if opening_mark.is_none() {
            opening_mark = transcript_out
                .as_ref()
                .and_then(|b| b.lock().ok().map(|g| g.len()));
        }
        info!(
            "Gemini Live intl caller serial={serial} — first AI audio held up to {}ms for the carrier's international-call prompt (prebuf_bytes={})",
            INTL_OPENING_HOLD.as_millis(),
            prebuf.len()
        );
    } else if !prebuf.is_empty() {
        // Flush held first-turn audio immediately (caller already heard full IVR greet).
        prebuf_flushed_bytes = prebuf.len() as u64;
        on_downlink(std::mem::take(&mut prebuf));
        info!(
            "Gemini Live prebuf flush serial={serial} chunks={} (first AI without re-RTT)",
            prebuf_chunks
        );
    }

    // Start at the actual unmute, even if a warm kick send was already stalled
    // (an international opening hold restarts it when the hold ends).
    let mut first_delivery_window_start = first_delivery_window_start(&audio_opened_at);
    let mut first_delivery_deadline = first_delivery_window_start + FIRST_DELIVERY_TIMEOUT;

    // Cold path (or warm without kick yet): force first greets after audio is open.
    if !greets_kicked {
        send_text_before_deadline(
            &mut sink,
            greets_kick_text.clone(),
            first_delivery_deadline,
            "gemini post-unmute greets kick send",
        )
        .await?;
        greets_kicked = true;
        info!("Gemini Live dialog greets kick sent serial={serial} (post-unmute)");
    } else {
        info!("Gemini Live dialog greets already pre-kicked serial={serial}");
    }
    let _ = greets_kicked;

    let chunk_target = (16_000usize * UPLINK_CHUNK_MS as usize / 1000) * 2;
    let mut uplink_buf: Vec<u8> = Vec::with_capacity(chunk_target * 4);
    let mut audio_chunks_out: u64 = 0;
    let mut audio_chunks_in: u64 = 0;
    let mut local_eou_sent: u64 = 0;

    // Manual activity: activityStart on first sustained voice, activityEnd after short silence.
    let mut voiced_ms: u32 = 0;
    let mut silence_ms: u32 = 0;
    let mut activity_open = false;
    // Audio is only streamed inside an activity; before that the newest
    // LOCAL_PREROLL_MS is held and flushed right after activityStart.
    let preroll_cap = (16_000usize * LOCAL_PREROLL_MS as usize / 1000) * 2;
    let mut preroll: Vec<u8> = Vec::with_capacity(preroll_cap + chunk_target);
    let mut noise_floor = NoiseFloor::default();
    let mut activity_ms: u32 = 0;
    let mut turn_raw_max: f32 = 0.0;
    let mut uplink_frames: u64 = 0;
    let mut gated_frames: u64 = 0;
    // Goodbye armed by the caller's own farewell (not the model's closing).
    let mut bye_from_caller = false;
    let mut last_caller_eou_at: Option<tokio::time::Instant> = None;

    // Talk timers run from UnmuteAfterGreet (not ConnectAtAnswer).
    let session_start = first_delivery_window_start;
    let mut first_delivery_kick_retried = false;
    let mut first_downlink_at: Option<tokio::time::Instant> = None;
    if prebuf_flushed_bytes > 0 {
        // Prebuffer already represents first model audio (count for idle/no-voice timers).
        first_downlink_at = Some(session_start);
        audio_chunks_out = prebuf_chunks;
    }
    let mut last_caller_voice_at: Option<tokio::time::Instant> = None;
    let mut last_activity_at = session_start;
    let mut caller_had_voice = false;
    let mut goodbye_pending = false;
    // Play-clock start after player drain. Intent may be armed while this is None.
    let mut output_goodbye_at: Option<tokio::time::Instant> = None;
    // AI has said a COMPLETE relay (轉達) recap this session (output STT).
    let mut saw_relay_ack = false;
    // Asked once what to pass on (「請問要轉達什麼事？」) after a backchannel / matter-less recap.
    let mut asked_clarify = false;
    // Downlink chunk count at last caller activityEnd (detect recap audio after caller).
    let mut chunks_at_last_caller_eou: u64 = 0;
    let mut downlink_pcm_bytes: u64 = prebuf_flushed_bytes;
    let mut bytes_at_last_caller_eou: u64 = 0;
    let mut out_tx_window = String::with_capacity(512);
    let mut in_tx_window = String::with_capacity(512);
    let mut end_reason = SessionEndReason::WsClosed;
    let mut hangup_signaled = false;
    // Desk-status opening must finish before caller barge-in can skip it.
    let mut opening_turn_complete = owner_status.is_empty();
    let mut status_opening_retried = false;
    // Persona: "I have to go" nudge sent; caller voice accumulated since the last arm.
    let mut wrap_up_sent = false;
    let mut voice_since_arm_ms: u32 = 0;
    let mut prev_goodbye_pending = false;
    // Persona: caller talked on after our closing — farewell or new content not decided yet.
    let mut post_arm_talk = false;
    // Persona: the caller's latest words end the call (bye / wrap-up), see caller_wraps_up.
    let mut caller_end_signal = false;
    // Logged once per arm: AI said bye, line held for the caller's.
    let mut await_bye_logged = false;
    // Stalling: caller keyword hits so far; once engaged the stall prompt loads.
    let mut stall_hits: Vec<&'static str> = Vec::new();
    let mut stall_score: u32 = 0;
    let mut stall_on = false;
    // Stalling: the caller said bye — never a reason to hang up. `stall_keep_due`:
    // when the model is told to hold them unless it has answered by itself
    // (audio past `stall_keep_chunks`); `stall_keeps`: nudges sent so far.
    let mut stall_keep_due: Option<tokio::time::Instant> = None;
    let mut stall_keep_chunks: u64 = 0;
    let mut stall_keeps: u32 = 0;
    // Stalling: what the model said since its last keep nudge / the caller's last turn,
    // and a farewell cut whose keep nudge waits for that generation to end.
    let mut stall_tx = String::new();
    let mut stall_keep_after_turn = false;
    // Stalling: a keep nudge already answers the farewell of the current model
    // turn — one per turn (the leftover transcript of a cut farewell asked a
    // second time, and the AI said 「欸等一下」 twice). Cleared once the next
    // reply is heard.
    let mut stall_turn_kept = false;
    // This model turn's audio was held or dropped for the carrier's prompt (see
    // IntlGate) or a voice menu — its end is no turn for the checks.
    let mut intl_gated_turn = false;
    // Stall quiet-line nudge: audio count when the last one went out (the next one
    // waits for the model's answer to it) and when.
    let mut stall_nudge_chunks: Option<u64> = None;
    let mut stall_nudge_at: Option<tokio::time::Instant> = None;
    // Caller follows up on the owner's own matter: never judged sales/scam.
    let mut own_matter = false;
    let mut idle_nudges: u32 = 0;
    // The caller's words since the AI last answered them, and whether those asked
    // us to hold on ("等一下，我去拿", "one moment"): a long quiet line is expected.
    let mut caller_recent = String::new();
    let mut caller_hold = false;
    let (mut wrap_secs, mut wrap_force_secs, mut max_secs) = talk_limits(persona, persona_max_secs);
    // Everything the caller said (input STT), for the carrier-prompt check.
    let mut caller_text_all = String::new();
    // Connection rotation for persona calls that outlive one Live socket (see
    // LIVE_ROTATE_AFTER_SECS). Once stalling engages, rotations carry its prompt
    // (first one right away).
    let mut rotate_setup: Option<Value> = None;
    let mut rotate_due: Option<tokio::time::Instant> = None;
    let mut rotate_retry_at: Option<tokio::time::Instant> = None;
    let mut ws_dead = false;
    let mut rotations: u32 = 0;
    // Rotations failed in a row while the old socket was still up (they only back off).
    let mut rotate_fails: u32 = 0;
    // Model owes a reply (activityEnd / kick sent, no turnComplete yet): not a safe switch point.
    let mut model_turn_pending = false;
    // Audio chunk count at the last rotation while its first turn is watched
    // for transcript-only output (see PostRotateTurn).
    let mut post_rotate_chunks: Option<u64> = None;
    let mut post_rotate_rekicked = false;
    // Output STT of the current model turn (cleared at each turnComplete).
    let mut turn_out_text = String::new();
    // This model turn's output STT leaked instructions: drop the rest of it.
    let mut out_leak_turn = false;
    // Billed tokens reported by the server (usageMetadata), summed over the session.
    let mut usage_prompt_tokens: u64 = 0;
    let mut usage_response_tokens: u64 = 0;
    // Barge-in: the model is still streaming this reply; after the caller cut in,
    // the rest of it is dropped (the player already flushed what was queued).
    let mut model_generating = false;
    let mut mute_model_audio = false;
    // `audio_chunks_out` at the last barge-in: one cut per AI turn.
    let mut barged_at_chunks: Option<u64> = None;
    let mut barge_ins: u32 = 0;
    // Echo-free line: caller audio held back from Gemini while our reply is
    // audible, and whether the caller has taken the floor for the rest of it.
    let mut overlap_hold = OverlapHold::default();
    let mut overlap_floor = false;
    let mut backchannels: u32 = 0;
    // First-audio watchdog paused while the caller talks (see FIRST_DELIVERY_CALLER_*).
    let mut fd_grace = Duration::ZERO;
    let mut fd_tick = tokio::time::Instant::now();
    // Testing only: rotate once after N s so it is testable without ~8 min.
    let test_rotate_after: Option<u64> = std::env::var("GEMINI_TEST_ROTATE_AFTER_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok());
    // Appointments: function calls being answered (each on its own task so the
    // audio loop never waits), the Live socket generation they belong to, and
    // when the last answer went up (see tool_turn_quiet).
    let (tool_tx, mut tool_rx) = mpsc::unbounded_channel::<ToolDone>();
    let mut tool_open: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Non-blocking calls being answered (the model goes on talking meanwhile).
    let mut tool_bg: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut tool_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let mut tool_seq: u64 = 0;
    let mut live_gen: u32 = 0;
    let mut tool_answer_at: Option<tokio::time::Instant> = None;
    let mut tool_answer_chunks: u64 = 0;
    // Model turns completed, and the audio count when the current one began
    // (what the repeat guard and propose_appointment's caller compare against).
    let mut turns_done: u64 = 0;
    let mut chunks_at_turn_start: u64 = 0;
    let mut repeat_guard = RepeatGuard::default();
    let mut repeat_drops: u32 = 0;
    // Sales / scam: the second look at a call the keywords flagged (stall_review)
    // runs on its own task and answers here; its "scam" verdict starts the
    // stalling at the top of the loop. The session is not told the caller's
    // number, only whether it is from abroad.
    let mut stall_review = StallReview::default();
    let (review_tx, mut review_rx) = mpsc::unbounded_channel::<(u32, crate::stall_review::Verdict)>();
    let review_key = key.to_string();
    let review_number: &'static str = if intl_call { "unknown (from abroad)" } else { "unknown" };
    let mut stall_engage_now = false;
    // The caller is a recording or voice menu: why (to be acted on), and since when.
    let mut ivr_enter: Option<&'static str> = None;
    let mut ivr_since: Option<tokio::time::Instant> = None;

    let signal_force_hangup = |hangup_signaled: &mut bool| {
        if *hangup_signaled {
            return;
        }
        if let Some(ref flag) = force_hangup {
            flag.store(true, Ordering::Relaxed);
            *hangup_signaled = true;
        }
    };

    // Preserve farewell hangup when cancel races mid-drain (WS glitch vs true cancel).
    let resolve_cancel = |goodbye_pending: bool| -> SessionEndReason {
        if goodbye_pending {
            SessionEndReason::Goodbye
        } else {
            SessionEndReason::Cancelled
        }
    };

    loop {
        // Hangup / call_end sets cancel — leave ASAP (stop token burn).
        if cancel.load(Ordering::Relaxed) {
            end_reason = resolve_cancel(goodbye_pending);
            if goodbye_pending {
                signal_force_hangup(&mut hangup_signaled);
                info!(
                    "Gemini Live cancel during goodbye serial={serial} — keep Goodbye hangup path"
                );
            } else {
                info!(
                    "Gemini Live cancel polled serial={serial} — closing WS now (stop API tokens)"
                );
            }
            break;
        }

        // The second look says sales / scam: the AI starts stalling.
        if std::mem::take(&mut stall_engage_now) && !stall_on && !own_matter && ivr_since.is_none() {
            if let Some(c) = stall.as_ref() {
                stall_on = true;
                c.engaged.store(true, Ordering::Relaxed);
                repeat_guard.clear();
                // No appointments with a sales / scam caller.
                if let Some(a) = &appt {
                    a.book.discard();
                }
                // The normal call's wrap-up may have gone out while the call was being
                // looked at: stalling has its own cap, and with the flag left set its
                // quiet-line and keep nudges never went and the call ran to the hard cut.
                let wrap_was_sent = std::mem::take(&mut wrap_up_sent);
                // A closing armed before the verdict no longer counts.
                if goodbye_pending {
                    goodbye_pending = false;
                    bye_from_caller = false;
                    output_goodbye_at = None;
                    end_reason = SessionEndReason::WsClosed;
                }
                // Stall session: its prompt, and its own model (setting
                // `gemini_stall_model`) — same voice, so the caller hears no change.
                let next = stall_session_setup(&setup, &c.system_prompt, &c.model, thinking_level);
                if next != setup {
                    rotate_setup = Some(next);
                    rotate_due = Some(
                        tokio::time::Instant::now() + Duration::from_secs(STALL_PROMPT_SWITCH_FORCE_SECS),
                    );
                }
                // Stalling may run longer than a normal call.
                (wrap_secs, wrap_force_secs, max_secs) = talk_limits(persona, c.max_secs);
                info!(
                    "Persona stall engaged serial={serial} score={stall_score} hits={stall_hits:?} talk_s={} cap_s={max_secs} wrap_up_was_sent={wrap_was_sent} switch={} model={}",
                    session_start.elapsed().as_secs(),
                    rotate_setup.is_some(),
                    rotate_setup
                        .as_ref()
                        .and_then(|s| s.pointer("/setup/model"))
                        .and_then(|m| m.as_str())
                        .unwrap_or(model_path.as_str())
                );
            }
        }
        // A recording or voice menu: the AI says nothing to it (its audio is dropped
        // too), no stalling, no nudges; we hang up below.
        if let Some(why) = ivr_enter.take() {
            if ivr_since.is_none() {
                let now = tokio::time::Instant::now();
                ivr_since = Some(now);
                stall_keep_due = None;
                stall_keep_after_turn = false;
                repeat_guard.clear();
                if play_head.ai_audible(Duration::ZERO) {
                    play_head.request_flush();
                }
                play_head.set_generating(false);
                match send_text_before_deadline(
                    &mut sink,
                    persona_ivr_client_content(),
                    now + KICK_SEND_TIMEOUT,
                    "gemini persona ivr send",
                )
                .await
                {
                    Ok(()) => info!(
                        "Persona caller is a recording / voice menu ({why}) serial={serial} talk_s={} stall={stall_on} — AI quiet, hang up after {}s of quiet line or {}s",
                        session_start.elapsed().as_secs(),
                        IVR_QUIET_END.as_secs(),
                        IVR_MAX.as_secs()
                    ),
                    Err(e) => warn!("Persona ivr instruction send failed serial={serial}: {e:#}"),
                }
            }
        }
        if let Some(t0) = ivr_since {
            let now = tokio::time::Instant::now();
            let quiet = (!activity_open)
                .then(|| last_caller_voice_at.map_or(Duration::MAX, |t| now.saturating_duration_since(t)));
            if ivr_hangs_up(now.saturating_duration_since(t0), quiet) {
                end_reason = SessionEndReason::IvrHangup;
                signal_force_hangup(&mut hangup_signaled);
                info!(
                    "Persona recording / voice menu serial={serial} event=ivr_hangup after_ms={} quiet_ms={} — hang up",
                    now.saturating_duration_since(t0).as_millis(),
                    quiet.map_or(0, |q| q.min(IVR_MAX).as_millis())
                );
                break;
            }
        }

        // Persona calls may outlive a Live WebSocket (~10 min): switch to a fresh
        // session carrying the transcript — at a pause (caller silent, model done
        // and drained), or forced near the cut / after goAway / when the socket died.
        if persona && rotations < LIVE_ROTATE_MAX && (ws_dead || !goodbye_pending) {
            let now = tokio::time::Instant::now();
            let age = conn_opened_at.elapsed();
            let test_due = test_rotate_after
                .map_or(false, |n| rotations == 0 && age >= Duration::from_secs(n));
            let wanted = ws_dead
                || rotate_due.is_some()
                || test_due
                || age >= Duration::from_secs(LIVE_ROTATE_AFTER_SECS);
            let forced = ws_dead
                || rotate_due.map_or(false, |d| now >= d)
                || age >= Duration::from_secs(LIVE_ROTATE_FORCE_SECS);
            let pause = !activity_open
                && !model_turn_pending
                && !goodbye_pending
                && tool_open.is_empty()
                && play_head.drained_for(Duration::from_millis(300), PLAYER_DRAIN_SLACK_BYTES);
            let backoff = !ws_dead && rotate_retry_at.map_or(false, |t| now < t);
            if wanted && (pause || forced) && !backoff {
                // Counted only once it worked: failures back off instead (rotate_retry_after).
                let n = rotations + 1;
                let history = transcript_out
                    .as_ref()
                    .and_then(|b| b.lock().ok().map(|g| g.clone()))
                    .map(|t| history_turns_with_note(&t, LIVE_HISTORY_MAX_CHARS, stall_on))
                    .unwrap_or_default();
                // Appointments: the new session gets the current time.
                let fresh_setup;
                let next_setup = match (rotate_setup.as_ref(), &appt) {
                    (Some(s), _) => s,
                    (None, Some(a)) => {
                        fresh_setup = setup_with_system_prompt(
                            &setup,
                            &call_system_prompt(&base_prompt, Some(a), chrono::Local::now()),
                        );
                        &fresh_setup
                    }
                    (None, None) => &setup,
                };
                let opened = match tokio::time::timeout(
                    Duration::from_secs(10),
                    open_live_with_history(&url, next_setup, &history, serial),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => Err(anyhow::anyhow!("gemini rotate timed out after 10s")),
                };
                match opened {
                    Ok((new_sink, new_stream)) => {
                        rotations = n;
                        rotate_fails = 0;
                        let _ = tokio::time::timeout(Duration::from_millis(500), sink.close()).await;
                        sink = new_sink;
                        stream = new_stream;
                        conn_opened_at = tokio::time::Instant::now();
                        let was_dead = ws_dead;
                        ws_dead = false;
                        rotate_due = None;
                        rotate_retry_at = None;
                        post_rotate_chunks = Some(audio_chunks_out);
                        post_rotate_rekicked = false;
                        turn_out_text.clear();
                        // Calls asked on the old socket cannot be answered on this one.
                        live_gen += 1;
                        repeat_guard.interrupted();
                        if !tool_open.is_empty() || !tool_bg.is_empty() {
                            info!(
                                "Gemini Live rotated with {} function call(s) open serial={serial} — their answers are dropped",
                                tool_open.len() + tool_bg.len()
                            );
                            tool_open.clear();
                            tool_bg.clear();
                        }
                        tool_answer_at = None;
                        // The new socket has no reply under way: nothing of the old one's
                        // turn is still streaming, muted, being cut or held back.
                        let keep_owed = std::mem::take(&mut stall_keep_after_turn) && stall_on && !wrap_up_sent;
                        model_generating = false;
                        mute_model_audio = false;
                        play_head.set_generating(false);
                        stall_tx.clear();
                        stall_turn_kept = false;
                        overlap_hold.reset();
                        overlap_floor = false;
                        out_leak_turn = false;
                        if activity_open {
                            // The caller is mid-sentence: reopen the turn on the new socket
                            // (server VAD hears the rest of the sentence on its own).
                            if !server_vad {
                                let start = json!({ "realtimeInput": { "activityStart": {} } });
                                let _ = send_text_before_deadline(
                                    &mut sink,
                                    start.to_string(),
                                    tokio::time::Instant::now() + API_SEND_TIMEOUT,
                                    "gemini rotate activityStart send",
                                )
                                .await;
                            }
                        } else if keep_owed {
                            // The old socket went right after the AI's 掰掰 was cut: its
                            // turnComplete (and the keep nudge it waited for) never came.
                            // The new one holds the caller instead of resuming.
                            model_turn_pending = true;
                            stall_turn_kept = true;
                            match send_text_before_deadline(
                                &mut sink,
                                persona_stall_keep_client_content(true),
                                tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                "gemini rotate stall keep send",
                            )
                            .await
                            {
                                Ok(()) => info!(
                                    "Persona stall keep nudge serial={serial} n={stall_keeps} why=farewell_cut_before_rotation"
                                ),
                                Err(e) => warn!("Persona stall keep send failed serial={serial}: {e:#}"),
                            }
                        } else if model_turn_pending {
                            // Forced switch while a reply was owed: ask for it on the new socket.
                            let _ = send_text_before_deadline(
                                &mut sink,
                                persona_resume_reply_client_content(),
                                tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                "gemini rotate reply kick send",
                            )
                            .await;
                        }
                        info!(
                            "Gemini Live rotated serial={serial} n={rotations} took_ms={} was_dead={was_dead} forced={forced} activity_open={activity_open} pending={model_turn_pending} talk_s={} model={}",
                            now.elapsed().as_millis(),
                            session_start.elapsed().as_secs(),
                            rotate_setup
                                .as_ref()
                                .unwrap_or(&setup)
                                .pointer("/setup/model")
                                .and_then(|m| m.as_str())
                                .unwrap_or("?")
                        );
                    }
                    Err(e) => {
                        rotate_fails += 1;
                        warn!(
                            "Gemini Live rotate failed serial={serial} n={n} fails={rotate_fails} dead={ws_dead}: {e:#}"
                        );
                        if ws_dead {
                            if goodbye_pending {
                                end_reason = SessionEndReason::Goodbye;
                                signal_force_hangup(&mut hangup_signaled);
                            }
                            break;
                        }
                        // Old socket still up: try again a bit later.
                        rotate_retry_at = Some(tokio::time::Instant::now() + rotate_retry_after(rotate_fails));
                    }
                }
            }
        }

        // International call (persona mode): what the model says first is held
        // while the line stays quiet, and plays if it stays so; once the line talks
        // (the carrier's prompt) it is dropped, so is anything the model says during
        // the prompt, and the model's own 「喂？」 after it is the first thing heard.
        if intl_gate != IntlGate::Open {
            let now = tokio::time::Instant::now();
            let quiet_for = (!activity_open)
                .then(|| last_caller_voice_at.map_or(Duration::MAX, |t| now.saturating_duration_since(t)));
            let step = intl_gate_step(intl_gate, now, caller_had_voice, quiet_for);
            match step {
                IntlGateStep::Wait => {
                    first_delivery_deadline = first_delivery_deadline.max(now + FIRST_DELIVERY_TIMEOUT);
                }
                IntlGateStep::PromptStarted => {
                    carrier_prompt_until = Some(now + CARRIER_PROMPT_SPAN);
                    intl_gate = IntlGate::Prompt { until: now + CARRIER_PROMPT_SPAN };
                    let dropped_ms = pcm16_ms(prebuf.len() as u64, phone_sr);
                    prebuf.clear();
                    prebuf_chunks = 0;
                    let dropped = opening_mark
                        .map(|m| truncate_transcript(&transcript_out, m))
                        .unwrap_or_default();
                    out_tx_window.clear();
                    turn_out_text.clear();
                    first_delivery_deadline = first_delivery_deadline.max(now + FIRST_DELIVERY_TIMEOUT);
                    info!(
                        "Gemini Live intl caller serial={serial} — line talking before our opening (carrier prompt): dropped it ({dropped_ms}ms {dropped:?}); the model answers after the prompt"
                    );
                }
                IntlGateStep::Play | IntlGateStep::PromptOver => {
                    intl_gate = IntlGate::Open;
                    let n = prebuf.len() as u64;
                    if n > 0 {
                        downlink_pcm_bytes = downlink_pcm_bytes.saturating_add(n);
                        on_downlink(std::mem::take(&mut prebuf));
                        first_downlink_at = Some(now);
                        audio_chunks_out = audio_chunks_out.saturating_add(prebuf_chunks);
                        last_activity_at = now;
                    }
                    prebuf_chunks = 0;
                    if step == IntlGateStep::Play {
                        info!(
                            "Gemini Live intl caller serial={serial} — no carrier prompt within {}ms: first AI audio played ({n} bytes)",
                            INTL_OPENING_HOLD.as_millis()
                        );
                    } else {
                        info!(
                            "Gemini Live intl caller serial={serial} — carrier prompt over (quiet={}ms): the model's audio plays",
                            quiet_for.map_or(0, |q| q.min(CARRIER_PROMPT_SPAN).as_millis())
                        );
                    }
                    // The first-audio watchdog starts now, not at pickup.
                    first_delivery_window_start = now;
                    first_delivery_deadline = now + FIRST_DELIVERY_TIMEOUT;
                    fd_grace = Duration::ZERO;
                    fd_tick = now;
                }
            }
        }

        // The model cannot answer while the caller — or the carrier's international-
        // call prompt — is talking: pause the first-audio watchdog meanwhile instead
        // of killing a healthy socket (e.g. fresh session at 5 s, AI
        // first heard at 8.6 s).
        let fd_now = tokio::time::Instant::now();
        if delivered_bytes.load(Ordering::Relaxed) == 0 {
            let caller_busy = first_delivery_paused(
                activity_open,
                last_caller_voice_at.map_or(false, |t| t.elapsed() < FIRST_DELIVERY_CALLER_HOLD),
                tool_open.len(),
            );
            if caller_busy && fd_grace < FIRST_DELIVERY_CALLER_GRACE_MAX {
                fd_grace = (fd_grace + fd_now.saturating_duration_since(fd_tick))
                    .min(FIRST_DELIVERY_CALLER_GRACE_MAX);
                first_delivery_deadline =
                    first_delivery_window_start + FIRST_DELIVERY_TIMEOUT + fd_grace;
            }
        }
        fd_tick = fd_now;
        let fd_action = if intl_gate != IntlGate::Open {
            FirstDeliveryAction::Wait
        } else {
            first_delivery_action(
                delivered_bytes.load(Ordering::Relaxed),
                first_delivery_window_start.elapsed().saturating_sub(fd_grace),
                first_delivery_kick_retried,
            )
        };
        match fd_action {
            FirstDeliveryAction::Wait => {}
            FirstDeliveryAction::RetryKick => {
                send_text_before_deadline(
                    &mut sink,
                    greets_kick_text.clone(),
                    first_delivery_deadline,
                    "gemini first-delivery retry kick send",
                )
                .await?;
                first_delivery_kick_retried = true;
                warn!(
                    "Gemini Live no delivered audio after {}ms serial={serial} -- retry greets kick once (absolute fail at {}ms, caller grace {}ms)",
                    FIRST_DELIVERY_RETRY_AFTER.as_millis(),
                    FIRST_DELIVERY_TIMEOUT.as_millis(),
                    fd_grace.as_millis()
                );
            }
            FirstDeliveryAction::Fail => {
                bail!(
                    "gemini first delivered audio timeout after retry serial={serial} total_wait_ms={}",
                    (FIRST_DELIVERY_TIMEOUT + fd_grace).as_millis()
                );
            }
        }

        // Intent may be armed from STT mid-utterance; PLAY only after the paced
        // player has drained this turn (+ idle), then hangup_delay, then WAV.
        // Caller said bye first: let the model answer it (「好，掰掰」) before
        // the clock may run — it used to hang up ~1 s after the caller's bye,
        // cutting the AI's farewell. Same while a reply after
        // our closing is still undecided (farewell or new content).
        let waiting_model_reply = goodbye_pending
            && goodbye_waits_on_model(
                bye_from_caller,
                post_arm_talk,
                audio_chunks_out > chunks_at_last_caller_eou,
                last_caller_eou_at.map(|t| t.elapsed()),
            );
        // Persona: our own 掰掰 does not end the call — hold the line until the
        // caller says bye / wraps up, or stays quiet PERSONA_BYE_SILENCE after it.
        let awaiting_caller_bye = persona
            && goodbye_pending
            && !wrap_up_sent
            && awaits_caller_bye(
                caller_end_signal,
                play_head.drained_for(PERSONA_BYE_SILENCE, PLAYER_DRAIN_SLACK_BYTES)
                    && last_caller_voice_at.map_or(true, |t| t.elapsed() >= PERSONA_BYE_SILENCE),
            );
        if awaiting_caller_bye && !await_bye_logged {
            await_bye_logged = true;
            info!(
                "Persona: AI said bye, caller has not — hold the line for their bye or {}s of silence serial={serial}",
                PERSONA_BYE_SILENCE.as_secs()
            );
        }
        let player_idle = goodbye_clock_may_run(
            play_head.drained_for(
                Duration::from_millis(RELAY_AUDIO_IDLE_MS),
                PLAYER_DRAIN_SLACK_BYTES,
            ),
            activity_open,
            // A function call open: the model still has something to say.
            waiting_model_reply || awaiting_caller_bye || !tool_open.is_empty(),
        );
        let now = tokio::time::Instant::now();
        let clock_was = output_goodbye_at.is_some();
        let fire_wav = tick_goodbye_play_clock(
            goodbye_pending,
            player_idle,
            Duration::from_millis(hangup_delay_ms),
            now,
            &mut output_goodbye_at,
        );
        if goodbye_pending && !clock_was && output_goodbye_at.is_some() {
            info!(
                "Goodbye play clock start (player drained) serial={serial} queued={} — wait {hangup_delay_ms}ms then WAV"
            , play_head.queued());
        } else if goodbye_pending && clock_was && output_goodbye_at.is_none() {
            info!(
                "Goodbye play clock reset (downlink playing / caller talking) serial={serial} queued={} outbound={} activity_open={activity_open}"
            , play_head.queued(), play_head.is_outbound());
        }
        if fire_wav
            && persona
            && !wrap_up_sent
            && !bye_from_caller
            && only_carrier_prompt_so_far(&caller_text_all, carrier_prompt_until, last_caller_voice_at)
        {
            // Input STT of the carrier prompt landed after the closing was armed.
            goodbye_pending = false;
            output_goodbye_at = None;
            end_reason = SessionEndReason::WsClosed;
            info!(
                "Persona hangup cancelled serial={serial} — caller only played the carrier international-call prompt"
            );
        } else if fire_wav
            && persona
            && !wrap_up_sent
            // The caller saying bye is leaving — hang up whatever the AI said.
            && !bye_from_caller
            && !caller_end_signal
            // No STT for the AI's turn yet: trust the closing that armed this.
            && !out_tx_window.trim().is_empty()
            && !turn_ends_with_closing(&out_tx_window)
        {
            // Armed on 「拜拜」/「先這樣」 mid-sentence (stalling, or a long call
            // with no time limit) — not a goodbye.
            goodbye_pending = false;
            bye_from_caller = false;
            output_goodbye_at = None;
            end_reason = SessionEndReason::WsClosed;
            let tail: String = out_tx_window
                .chars()
                .rev()
                .take(24)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            info!(
                "Persona: AI turn does not end on a farewell — keep talking serial={serial} stall={stall_on} tail={tail}"
            );
        } else if fire_wav {
            end_reason = SessionEndReason::Goodbye;
            signal_force_hangup(&mut hangup_signaled);
            info!(
                "Gemini Live goodbye play clock elapsed serial={serial} queued={} outbound={} — hangup after player drain"
            , play_head.queued(), play_head.is_outbound());
            break;
        }

        if goodbye_pending != prev_goodbye_pending {
            voice_since_arm_ms = 0;
            prev_goodbye_pending = goodbye_pending;
            if !goodbye_pending {
                post_arm_talk = false;
                await_bye_logged = false;
            }
        }

        // Wall-clock max talk time → agent will play goodbye WAV (persona: plain hangup).
        if session_start.elapsed() >= Duration::from_secs(max_secs) {
            end_reason = SessionEndReason::MaxDuration;
            let waited = let_the_phone_finish(&play_head, phone_sr, &cancel).await;
            signal_force_hangup(&mut hangup_signaled);
            info!(
                "Gemini Live max duration {}s serial={serial} stall={stall_on} — end for goodbye (waited {}ms for the phone to play our last words)",
                max_secs,
                waited.as_millis()
            );
            break;
        }

        // Persona: near the safety cap, tell the model to say it has other things
        // to do. Wait for a gap (caller not talking, model audio drained) unless
        // forced.
        if persona && !wrap_up_sent && !goodbye_pending {
            let talk = session_start.elapsed();
            let gap = !activity_open
                && play_head.drained_for(Duration::from_millis(300), PLAYER_DRAIN_SLACK_BYTES);
            if talk >= Duration::from_secs(wrap_force_secs)
                || (talk >= Duration::from_secs(wrap_secs) && gap)
            {
                wrap_up_sent = true;
                repeat_guard.clear();
                let nudge = if stall_on {
                    persona_stall_wrap_up_client_content()
                } else {
                    persona_wrap_up_client_content()
                };
                model_turn_pending = true;
                match send_text_before_deadline(
                    &mut sink,
                    nudge,
                    tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                    "gemini persona wrap-up send",
                )
                .await
                {
                    Ok(()) => info!(
                        "Persona wrap-up nudge sent serial={serial} talk_s={} gap={gap} stall={stall_on}",
                        talk.as_secs()
                    ),
                    Err(e) => warn!("Persona wrap-up send failed serial={serial}: {e:#}"),
                }
                out_tx_window.clear();
                last_activity_at = tokio::time::Instant::now();
            }
        }
        // Stalling: the caller said bye. If the model answered by itself its turn is
        // checked at turnComplete; if it stays quiet it is told to hold them.
        if let Some(due) = stall_keep_due {
            if wrap_up_sent || ivr_since.is_some() || audio_chunks_out > stall_keep_chunks {
                stall_keep_due = None;
            } else if tokio::time::Instant::now() >= due
                && !activity_open
                && !model_generating
                && play_head.drained_for(Duration::from_millis(300), PLAYER_DRAIN_SLACK_BYTES)
            {
                stall_keep_due = None;
                if stall_keeps < STALL_KEEP_MAX {
                    stall_keeps += 1;
                    model_turn_pending = true;
                    stall_tx.clear();
                    match send_text_before_deadline(
                        &mut sink,
                        persona_stall_keep_client_content(false),
                        tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                        "gemini persona stall keep send",
                    )
                    .await
                    {
                        Ok(()) => info!(
                            "Persona stall keep nudge serial={serial} n={stall_keeps} why=caller_bye_unanswered"
                        ),
                        Err(e) => warn!("Persona stall keep send failed serial={serial}: {e:#}"),
                    }
                    last_activity_at = tokio::time::Instant::now();
                }
            }
        }
        // No caller speech long after first AI audio → cannot communicate.
        if let Some(t0) = first_downlink_at {
            if !caller_had_voice && t0.elapsed() >= Duration::from_secs(LIVE_NO_CALLER_VOICE_SECS) {
                end_reason = SessionEndReason::CantCommunicate;
                let waited = let_the_phone_finish(&play_head, phone_sr, &cancel).await;
                signal_force_hangup(&mut hangup_signaled);
                info!(
                    "Gemini Live no caller voice {}s serial={serial} — end for goodbye (waited {}ms for playout)",
                    LIVE_NO_CALLER_VOICE_SECS,
                    waited.as_millis()
                );
                break;
            }
        }
        // The quiet-line clocks run from when the phone has played our last words:
        // the model sends a reply faster than it is spoken, and the phone holds up
        // to ~1 s of it.
        let ai_playing = !play_head.speech_left(phone_sr).is_zero();
        if ai_playing {
            last_activity_at = tokio::time::Instant::now();
        }
        // Persona: a silent line ("let me transfer you, please hold", "let me check") gets
        // 「喂？還在嗎？」 first instead of a hangup. Stalling does not wait that long:
        // a few quiet seconds after our reply has played out and the model asks
        // the next question itself. The caller asked us to hold on: the question
        // waits longer, and stalling waits for them to speak again.
        let idle = last_activity_at.elapsed();
        let (quiet_nudge_secs, quiet_end_secs) = if caller_hold {
            (PERSONA_HOLD_NUDGE_SECS, PERSONA_HOLD_END_SECS)
        } else {
            (PERSONA_IDLE_NUDGE_SECS, PERSONA_IDLE_END_SECS)
        };
        // One at a time: the model has answered the last nudge and the line has
        // been quiet again since (they all went out at once on the first try).
        let stall_quiet = stall_on
            && !wrap_up_sent
            && !caller_hold
            && stall_keep_due.is_none()
            && !stall_keep_after_turn
            && idle_nudges < PERSONA_STALL_IDLE_NUDGES_MAX
            && stall_nudge_chunks.map_or(true, |c| audio_chunks_out > c)
            && stall_nudge_at
                .map_or(true, |t| t.elapsed() >= Duration::from_secs(PERSONA_STALL_IDLE_NUDGE_SECS))
            && !activity_open
            && !model_generating
            && play_head.drained_for(
                Duration::from_secs(PERSONA_STALL_IDLE_NUDGE_SECS + 1),
                PLAYER_DRAIN_SLACK_BYTES,
            )
            && last_caller_voice_at
                .map_or(true, |t| t.elapsed() >= Duration::from_secs(PERSONA_STALL_IDLE_NUDGE_SECS));
        // Our reply has played out and nothing is coming (stalling checks the same).
        let plain_quiet = !stall_on
            && idle_nudges < PERSONA_IDLE_NUDGES_MAX
            && idle >= Duration::from_secs(quiet_nudge_secs)
            && !activity_open
            && !model_generating
            && !ai_playing
            && play_head.drained_for(Duration::from_millis(300), PLAYER_DRAIN_SLACK_BYTES);
        // A function call open (appointments) is not a quiet line; a recording gets no nudge.
        if persona
            && first_downlink_at.is_some()
            && !goodbye_pending
            && tool_open.is_empty()
            && ivr_since.is_none()
            && (stall_quiet || plain_quiet)
        {
            idle_nudges += 1;
            model_turn_pending = true;
            // The nudge asks for a turn: it is no repeat.
            repeat_guard.clear();
            stall_tx.clear();
            let nudge = if stall_quiet {
                stall_nudge_chunks = Some(audio_chunks_out);
                stall_nudge_at = Some(tokio::time::Instant::now());
                persona_stall_idle_nudge_client_content()
            } else {
                persona_idle_nudge_client_content()
            };
            match send_text_before_deadline(
                &mut sink,
                nudge,
                tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                "gemini persona idle nudge send",
            )
            .await
            {
                Ok(()) => info!(
                    "Persona idle nudge serial={serial} n={idle_nudges} idle_s={} stall={stall_on} caller_hold={caller_hold}",
                    idle.as_secs()
                ),
                Err(e) => warn!("Persona idle nudge failed serial={serial}: {e:#}"),
            }
            last_activity_at = tokio::time::Instant::now();
        }
        let idle_end_secs = if persona { quiet_end_secs } else { LIVE_IDLE_SECS };

        // Idle after last real activity (caller voice or model audio).
        if first_downlink_at.is_some()
            && tool_open.is_empty()
            && last_activity_at.elapsed() >= Duration::from_secs(idle_end_secs)
        {
            end_reason = if caller_had_voice {
                SessionEndReason::Goodbye
            } else {
                SessionEndReason::CantCommunicate
            };
            let waited = let_the_phone_finish(&play_head, phone_sr, &cancel).await;
            signal_force_hangup(&mut hangup_signaled);
            info!(
                "Gemini Live idle {idle_end_secs}s serial={serial} reason={} caller_hold={caller_hold} — end for goodbye (waited {}ms for playout)",
                end_reason.as_str(),
                waited.as_millis()
            );
            break;
        }

        // Fast poll: hangup must cut Live billing within ~20ms, not wait on next audio frame.
        let cancel_tick = tokio::time::sleep(Duration::from_millis(20));
        tokio::pin!(cancel_tick);
        tokio::select! {
            biased;
            _ = &mut cancel_tick => {
                if cancel.load(Ordering::Relaxed) {
                    end_reason = resolve_cancel(goodbye_pending);
                    if goodbye_pending {
                        signal_force_hangup(&mut hangup_signaled);
                        info!(
                            "Gemini Live cancel tick during goodbye serial={serial} — keep Goodbye hangup path"
                        );
                    } else {
                        info!(
                            "Gemini Live cancel tick serial={serial} — closing WS now (stop API tokens)"
                        );
                    }
                    break;
                }
            }
            msg = stream.next() => {
                if cancel.load(Ordering::Relaxed) {
                    end_reason = resolve_cancel(goodbye_pending);
                    if goodbye_pending {
                        signal_force_hangup(&mut hangup_signaled);
                    } else {
                        info!(
                            "Gemini Live cancel mid-recv serial={serial} — closing WS now (stop API tokens)"
                        );
                    }
                    break;
                }
                match msg {
                    Some(Ok(msg)) => {
                        if matches!(msg, WsMessage::Close(_)) {
                            if persona && !goodbye_pending && rotations < LIVE_ROTATE_MAX {
                                info!(
                                    "Gemini Live ws closed serial={serial} talk_s={} — rotate to a fresh session",
                                    session_start.elapsed().as_secs()
                                );
                                ws_dead = true;
                                continue;
                            }
                            if end_reason == SessionEndReason::WsClosed && goodbye_pending {
                                end_reason = SessionEndReason::Goodbye;
                                signal_force_hangup(&mut hangup_signaled);
                            }
                            info!(
                                "Gemini Live ws closed serial={serial} out={audio_chunks_out} in={audio_chunks_in} eou={local_eou_sent}"
                            );
                            break;
                        }
                        if let Some(t) = text_from_ws(msg) {
                            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                                if debug_raw_left > 0 {
                                    debug_raw_left -= 1;
                                    info!(
                                        "Gemini Live raw serial={serial} phase=live t_ms={} msg={}",
                                        session_start.elapsed().as_millis(),
                                        redact_long_strings(&v)
                                    );
                                }
                                if let Some(err) = v.get("error") {
                                    warn!("Gemini Live server error serial={serial} err={err}");
                                    // Persona call under way: the socket is as good as closed —
                                    // carry the call over to a fresh one, as on a close. (The
                                    // opening still fails over to a fresh session in main.)
                                    if persona
                                        && !goodbye_pending
                                        && rotations < LIVE_ROTATE_MAX
                                        && delivered_bytes.load(Ordering::Relaxed) > 0
                                    {
                                        info!(
                                            "Gemini Live server error mid-call serial={serial} talk_s={} — rotate to a fresh session",
                                            session_start.elapsed().as_secs()
                                        );
                                        ws_dead = true;
                                        continue;
                                    }
                                    bail!("gemini server error: {err}");
                                }
                                if let Some(u) = v.get("usageMetadata") {
                                    let n = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                                    usage_prompt_tokens += n("promptTokenCount");
                                    usage_response_tokens += n("responseTokenCount");
                                }
                                if let Some(g) = v.get("goAway") {
                                    let left = g.get("timeLeft").and_then(parse_duration_secs);
                                    warn!(
                                        "Gemini Live goAway serial={serial} time_left_s={left:?} talk_s={} conn_age_s={} stall={stall_on}",
                                        session_start.elapsed().as_secs(),
                                        conn_opened_at.elapsed().as_secs()
                                    );
                                    if persona && rotate_due.is_none() {
                                        // Switch at the next pause; at the latest ~2 s before the cut.
                                        let lead = (left.unwrap_or(5.0) - 2.0).clamp(0.0, 60.0);
                                        rotate_due = Some(
                                            tokio::time::Instant::now()
                                                + Duration::from_secs_f64(lead),
                                        );
                                    }
                                }
                                // Hangup wins over STT drain — unless farewell already armed.
                                if cancel.load(Ordering::Relaxed) {
                                    end_reason = resolve_cancel(goodbye_pending);
                                    if goodbye_pending {
                                        signal_force_hangup(&mut hangup_signaled);
                                    } else {
                                        info!(
                                            "Gemini Live cancel after msg serial={serial} — closing WS now (stop API tokens)"
                                        );
                                    }
                                    break;
                                }
                                // Appointments: the model calls a function and waits for
                                // its answer (no audio meanwhile); each runs on its own task.
                                let tool_calls = crate::appointments::parse_tool_calls(&v);
                                if !tool_calls.is_empty() {
                                    let now = tokio::time::Instant::now();
                                    last_activity_at = now;
                                    for call in tool_calls {
                                        tool_seq += 1;
                                        let key = if call.id.is_empty() {
                                            format!("#{tool_seq}")
                                        } else {
                                            call.id.clone()
                                        };
                                        let Some(a) = appt.clone() else {
                                            // No functions were declared: answer so it does not wait.
                                            warn!(
                                                "Gemini Live toolCall without appointments serial={serial} name={}",
                                                call.name
                                            );
                                            let msg = crate::appointments::tool_response(
                                                &call.id,
                                                &call.name,
                                                json!({ "error": "unavailable" }),
                                            );
                                            let _ = send_text_before_deadline(
                                                &mut sink,
                                                msg.to_string(),
                                                now + API_SEND_TIMEOUT,
                                                "gemini toolResponse send",
                                            )
                                            .await;
                                            continue;
                                        };
                                        let blocking = crate::appointments::is_blocking(&call.name, non_blocking);
                                        // Called after the model began its reply (see RepeatGuard).
                                        let after_speech = (call.name == crate::appointments::PROPOSE
                                            && spoke_since_caller(
                                                audio_chunks_out,
                                                chunks_at_turn_start,
                                                downlink_pcm_bytes,
                                                bytes_at_last_caller_eou,
                                            ))
                                            .then_some(CallTurn { turns_done, caller_eous: local_eou_sent });
                                        info!(
                                            "Gemini Live toolCall serial={serial} name={} id={key} blocking={blocking} after_speech={} stall={stall_on} talk_s={}",
                                            call.name,
                                            after_speech.is_some(),
                                            session_start.elapsed().as_secs()
                                        );
                                        if blocking {
                                            // The model waits for the answer: it owes a reply.
                                            model_turn_pending = true;
                                            tool_open.insert(key.clone());
                                        } else {
                                            tool_bg.insert(key.clone());
                                        }
                                        let tx = tool_tx.clone();
                                        let gen = live_gen;
                                        tool_tasks.retain(|h| !h.is_finished());
                                        tool_tasks.push(tokio::spawn(async move {
                                            let response = a.run_tool(&call).await;
                                            let _ = tx.send(ToolDone {
                                                gen,
                                                key,
                                                id: call.id,
                                                name: call.name,
                                                response,
                                                blocking,
                                                after_speech,
                                            });
                                        }));
                                    }
                                }
                                for id in crate::appointments::parse_tool_cancellation(&v) {
                                    if tool_open.remove(&id) || tool_bg.remove(&id) {
                                        info!(
                                            "Gemini Live toolCallCancellation serial={serial} id={id} — its answer will not be sent"
                                        );
                                    }
                                }
                                // Caller words in this message: what the model says next
                                // answers them — checked before the repeat guard, so a
                                // short 「嗯」 is not taken for nobody asking.
                                if !extract_transcription_snippets(&v).0.is_empty() {
                                    repeat_guard.caller_words();
                                }
                                // A turn the model opened on its own to say again what it said
                                // before propose_appointment: not played, not written down.
                                // Only its audio starts that; a transcript alone is the end
                                // of the turn before and is written down as usual.
                                let has_audio = server_msg_has_audio(&v);
                                let model_output = has_audio || output_transcription_text(&v).is_some();
                                let was_dropping = repeat_guard.is_dropping();
                                let repeat_drop = model_output
                                    && repeat_guard.drops(
                                        has_audio,
                                        turns_done,
                                        audio_chunks_out > chunks_at_turn_start,
                                        local_eou_sent,
                                        activity_open,
                                        tokio::time::Instant::now(),
                                    );
                                if repeat_drop && !was_dropping {
                                    repeat_drops += 1;
                                    info!(
                                        "Gemini Live unasked turn after propose_appointment serial={serial} n={repeat_drops} — repeat dropped (not played, not transcribed)"
                                    );
                                }
                                if !out_leak_turn
                                    && output_transcription_text(&v).is_some_and(looks_like_leaked_internals)
                                {
                                    out_leak_turn = true;
                                    warn!(
                                        "Gemini Live output transcript leaked model instructions serial={serial} — dropped for the rest of this turn"
                                    );
                                }
                                // International call: the model's words during the carrier's
                                // prompt are not played, so not written down either; nor
                                // anything it says to a recording / voice menu.
                                let intl_drop = matches!(intl_gate, IntlGate::Prompt { .. }) || ivr_since.is_some();
                                // …and what it says before (held, maybe played later) is.
                                let intl_hold = matches!(intl_gate, IntlGate::Waiting { .. });
                                let v_clean;
                                let v_tx = if (out_leak_turn || repeat_drop || intl_drop) && output_transcription_text(&v).is_some() {
                                    v_clean = without_output_transcription(&v);
                                    &v_clean
                                } else {
                                    &v
                                };
                                append_transcriptions(v_tx, &transcript_out);
                                let (in_tx, out_tx) = extract_transcription_snippets(v_tx);
                                // Prefer a COMPLETE recap (matter filled in) before any hangup.
                                // Greeting 「我幫您轉達」 and trailing 「關於」 must not arm.
                                // Arm as soon as STT shows a complete recap — do not wait for a
                                // later turnComplete (Live STT often arrives after generationComplete).
                                for s in &out_tx {
                                    push_tx_window(&mut out_tx_window, s, 800);
                                    push_tx_window(&mut turn_out_text, s, 400);
                                    if persona {
                                        // Natural closing from the model ends the call; no recap gate.
                                        // Never on the opening line (caller has not spoken yet).
                                        let closing = looks_like_persona_closing(s)
                                            || looks_like_persona_closing(&out_tx_window);
                                        if closing
                                            && !wrap_up_sent
                                            && only_carrier_prompt_so_far(
                                                &caller_text_all,
                                                carrier_prompt_until,
                                                last_caller_voice_at,
                                            )
                                        {
                                            if !goodbye_pending {
                                                info!(
                                                    "Persona closing ignored serial={serial} — caller only played the carrier international-call prompt text={s}"
                                                );
                                            }
                                            continue;
                                        }
                                        // Stalling: our 掰掰 must not reach a sales / scam caller.
                                        // The transcript runs ahead of the phone's playout,
                                        // so the rest of this reply (the farewell) is dropped
                                        // and the model is told to hold them with a question.
                                        if stall_on && !wrap_up_sent {
                                            push_tx_window(&mut stall_tx, s, 200);
                                            if ends_with_farewell(&stall_tx)
                                                && !stall_keep_after_turn
                                                && !stall_turn_kept
                                                && stall_keeps < STALL_KEEP_MAX
                                            {
                                                stall_keeps += 1;
                                                stall_turn_kept = true;
                                                stall_keep_due = None;
                                                let preview: String = stall_tx.chars().rev().take(30).collect::<Vec<_>>().into_iter().rev().collect();
                                                stall_tx.clear();
                                                let cut = play_head.ai_audible(Duration::ZERO);
                                                if cut {
                                                    play_head.request_flush();
                                                }
                                                if model_generating {
                                                    mute_model_audio = true;
                                                    play_head.set_generating(false);
                                                    stall_keep_after_turn = true;
                                                } else {
                                                    model_turn_pending = true;
                                                    if let Err(e) = send_text_before_deadline(
                                                        &mut sink,
                                                        persona_stall_keep_client_content(true),
                                                        tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                                        "gemini persona stall keep send",
                                                    )
                                                    .await
                                                    {
                                                        warn!("Persona stall keep send failed serial={serial}: {e:#}");
                                                    }
                                                }
                                                info!(
                                                    "Persona stall: AI started a farewell — cut={cut} keep nudge n={stall_keeps} {} serial={serial} text={preview}",
                                                    if stall_keep_after_turn { "after this generation" } else { "sent" }
                                                );
                                            }
                                        }
                                        if closing && stall_on && !wrap_up_sent {
                                            continue;
                                        }
                                        if closing && (caller_had_voice || wrap_up_sent) {
                                            if !goodbye_pending {
                                                info!(
                                                    "Persona closing armed serial={serial} wrap_up={wrap_up_sent} text={s}"
                                                );
                                            }
                                            goodbye_pending = true;
                                            end_reason = SessionEndReason::Goodbye;
                                        }
                                        continue;
                                    }
                                    let complete = looks_like_complete_relay_ack(s)
                                        || looks_like_complete_relay_ack(&out_tx_window);
                                    let named = recap_names_matter(s)
                                        || recap_names_matter(&out_tx_window);
                                    if complete && (named || asked_clarify) {
                                        if !saw_relay_ack {
                                            info!(
                                                "Complete relay recap serial={serial} text={s}"
                                            );
                                        }
                                        let first_arm = !goodbye_pending;
                                        saw_relay_ack = true;
                                        goodbye_pending = true;
                                        if first_arm {
                                            info!(
                                                "Goodbye armed (complete recap STT) serial={serial} — wait player drain+idle then {hangup_delay_ms}ms then WAV (do not cut Gemini)"
                                            );
                                        }
                                    } else if complete && !named && !asked_clarify {
                                        info!(
                                            "Matter-less recap — not arming hangup serial={serial} text={s}"
                                        );
                                    } else if looks_like_incomplete_relay(s)
                                        || looks_like_incomplete_relay(&out_tx_window)
                                    {
                                        info!(
                                            "Incomplete recap trailing 關於 — not arming hangup serial={serial} text={s}"
                                        );
                                    } else if looks_like_backchannel(s)
                                        || looks_like_backchannel(&out_tx_window)
                                    {
                                        info!(
                                            "Backchannel without recap — not arming hangup serial={serial} text={s}"
                                        );
                                    }
                                    let farewell = looks_like_goodbye(s)
                                        || looks_like_goodbye(&out_tx_window);
                                    if farewell && !saw_relay_ack {
                                        info!(
                                            "Goodbye phrase without complete relay ack — not arming hangup serial={serial} text={s}"
                                        );
                                    }
                                }
                                // Caller farewell also arms hangup (after model turn / idle).
                                for s in &in_tx {
                                    push_tx_window(&mut in_tx_window, s, 800);
                                    push_tx_window(&mut caller_text_all, s, 400);
                                    push_tx_window(&mut caller_recent, s, 400);
                                    let hold = crate::locale::asks_to_hold(&caller_recent);
                                    if hold != caller_hold {
                                        caller_hold = hold;
                                        info!(
                                            "Persona caller {} serial={serial} text={s}",
                                            if hold { "asked us to hold on — longer quiet line allowed" } else { "is back" }
                                        );
                                    }
                                    if !own_matter && looks_like_own_matter(&in_tx_window) {
                                        own_matter = true;
                                        info!(
                                            "Persona caller follows up on the owner's own matter — no 陪聊 this call serial={serial} stall={stall_on} text={s}"
                                        );
                                    }
                                    // A recorded voice menu ("查帳單請按1，轉接專人請按9"): nobody to
                                    // stall; the AI goes quiet and we hang up.
                                    if persona
                                        && stall.is_some()
                                        && ivr_since.is_none()
                                        && ivr_enter.is_none()
                                        && crate::stall_review::ivr_cue(&in_tx_window)
                                    {
                                        ivr_enter = Some("menu words");
                                    }
                                    // Sales / scam keywords: once they cross the line the call gets
                                    // a second look (stall_review) while the AI talks on; its verdict
                                    // decides whether the AI starts stalling.
                                    if stall.is_some() && !stall_on && !own_matter && ivr_since.is_none() {
                                        stall_score += stall_score_add(&in_tx_window, &mut stall_hits);
                                        if stall_review.should_ask(stall_engages(stall_score, &stall_hits), stall_score) {
                                            let dialogue = transcript_out
                                                .as_ref()
                                                .and_then(|b| b.lock().ok().map(|g| merge_dialogue_fragments(&g)))
                                                .unwrap_or_default();
                                            let hits: Vec<String> = stall_hits.iter().map(|h| h.to_string()).collect();
                                            let key = review_key.clone();
                                            let tx = review_tx.clone();
                                            let score = stall_score;
                                            tokio::spawn(async move {
                                                let verdict =
                                                    crate::stall_review::review(&key, &dialogue, &hits, review_number).await;
                                                let _ = tx.send((score, verdict));
                                            });
                                            info!(
                                                "Persona stall flagged serial={serial} score={stall_score} hits={stall_hits:?} talk_s={} — second look n={} (the AI talks on meanwhile)",
                                                session_start.elapsed().as_secs(),
                                                stall_review.asked
                                            );
                                        }
                                    }
                                    // Persona: only how the caller's latest words END counts
                                    // (bye / wrap-up); assistant (relay) mode keeps the old window match.
                                    let caller_closing = if persona {
                                        // With this piece at the end even when the window skipped it.
                                        caller_end_signal = caller_wraps_up(&with_latest(&in_tx_window, s));
                                        caller_end_signal
                                    } else {
                                        looks_like_goodbye(s) || looks_like_goodbye(&in_tx_window)
                                    };
                                    // The caller's goodbye was the start of a sentence (「那就先這樣」
                                    // then 「嗎？」; "That's all" then "for today, right?"): taken back,
                                    // or the AI answers the question and hangs up.
                                    if persona
                                        && !caller_end_signal
                                        && bye_from_caller
                                        && goodbye_pending
                                        && !wrap_up_sent
                                        && caller_withdraws_bye(&in_tx_window, s)
                                    {
                                        goodbye_pending = false;
                                        bye_from_caller = false;
                                        output_goodbye_at = None;
                                        end_reason = SessionEndReason::WsClosed;
                                        info!(
                                            "Persona caller's goodbye taken back (more words followed) serial={serial} text={s}"
                                        );
                                    }
                                    if caller_closing && persona && stall_on && !wrap_up_sent {
                                        // A sales / scam caller saying bye
                                        // is not let go — the AI keeps asking.
                                        if stall_keep_due.is_none() {
                                            info!(
                                                "Persona stall: caller wants to leave — not hanging up, keep them on the line serial={serial} text={s}"
                                            );
                                            stall_keep_due =
                                                Some(tokio::time::Instant::now() + STALL_KEEP_WAIT);
                                            stall_keep_chunks = audio_chunks_out;
                                        }
                                    } else if caller_closing {
                                        if post_arm_talk {
                                            // Their words after our closing are a farewell,
                                            // not new content: hang up once they finish.
                                            post_arm_talk = false;
                                            info!(
                                                "Persona caller answered the closing with a farewell — hang up serial={serial}"
                                            );
                                        } else if !goodbye_pending {
                                            // Model has not closed yet: wait for its answer.
                                            bye_from_caller = true;
                                        }
                                        goodbye_pending = true;
                                        info!(
                                            "Goodbye detected text={s} → hangup serial={serial} source=input (wait model turn / delay)"
                                        );
                                    }
                                }
                                if v.pointer("/serverContent/interrupted").and_then(|x| x.as_bool())
                                    == Some(true)
                                {
                                    info!(
                                        "Gemini Live interrupted serial={serial} server_vad={server_vad} activity_open={activity_open} muted={mute_model_audio}"
                                    );
                                    model_generating = false;
                                    play_head.set_generating(false);
                                    mute_model_audio = false;
                                    repeat_guard.interrupted();
                                    // The caller is talking: the model answers them instead.
                                    stall_keep_after_turn = false;
                                    // Gemini heard the caller start over a reply we had not
                                    // yet held their audio for (echo-free line): stop the
                                    // queued rest now, not after our own VAD.
                                    if barge_in.load(Ordering::Relaxed)
                                        && barged_at_chunks.map_or(true, |c| audio_chunks_out > c)
                                        && play_head.ai_audible(OVERLAP_AUDIBLE_TAIL)
                                    {
                                        barged_at_chunks = Some(audio_chunks_out);
                                        barge_ins += 1;
                                        play_head.request_flush();
                                        info!(
                                            "Gemini Live barge-in serial={serial} source=server n={barge_ins} — caller cut in, stop our playback"
                                        );
                                    }
                                }
                                if has_audio {
                                    model_generating = true;
                                    play_head.set_generating(!mute_model_audio && !repeat_drop && !intl_hold && !intl_drop);
                                }
                                // After a barge-in, the rest of the cut reply is not played.
                                let n = if (mute_model_audio || repeat_drop || intl_drop) && has_audio {
                                    intl_gated_turn |= intl_drop;
                                    0
                                } else if intl_hold && has_audio {
                                    // Kept until the carrier's prompt has come or not (IntlGate).
                                    intl_gated_turn = true;
                                    let mut capture = |pcm: Vec<u8>| {
                                        let take = pcm.len().min(PREBUF_CAP_BYTES.saturating_sub(prebuf.len()));
                                        if take > 0 {
                                            prebuf.extend_from_slice(&pcm[..take]);
                                            prebuf_chunks = prebuf_chunks.saturating_add(1);
                                        }
                                    };
                                    let _ = handle_server_msg(&v, phone_sr, &mut capture);
                                    0
                                } else {
                                    handle_server_msg(&v, phone_sr, &mut on_downlink)
                                };
                                if n > 0 {
                                    // A reply is heard: the next farewell may be cut again.
                                    stall_turn_kept = false;
                                    if server_vad && activity_open {
                                        // Gemini ended the caller's turn before the local VAD
                                        // did: it is the one deciding, close ours to match.
                                        local_eou_sent += 1;
                                        model_turn_pending = true;
                                        chunks_at_last_caller_eou = audio_chunks_out;
                                        bytes_at_last_caller_eou = downlink_pcm_bytes;
                                        last_caller_eou_at = Some(tokio::time::Instant::now());
                                        info!(
                                            "Gemini Live caller turn ended by server VAD serial={serial} silence_ms={silence_ms} voiced_ms={voiced_ms} activity_ms={activity_ms} n={local_eou_sent}"
                                        );
                                        activity_open = false;
                                        activity_ms = 0;
                                        voiced_ms = 0;
                                        silence_ms = 0;
                                    }
                                    audio_chunks_out += 1;
                                    downlink_pcm_bytes = downlink_pcm_bytes.saturating_add(n);
                                    last_activity_at = tokio::time::Instant::now();
                                    // New model audio after the caller finished: this is the recap
                                    // turn. Drop leftover greeting STT so fallback hangup can fire
                                    // even if recap STT is empty/mangled.
                                    if caller_had_voice
                                        && audio_chunks_out == chunks_at_last_caller_eou + 1
                                    {
                                        out_tx_window.clear();
                                        stall_tx.clear();
                                        // The caller's next words start a new utterance.
                                        caller_recent.clear();
                                        info!(
                                            "Gemini Live new model turn after caller serial={serial} (clear recap STT window)"
                                        );
                                    }
                                    if first_downlink_at.is_none() {
                                        first_downlink_at = Some(tokio::time::Instant::now());
                                        info!("Gemini Live first downlink audio serial={serial}");
                                    }
                                }
                                let turn_done = v
                                    .pointer("/serverContent/turnComplete")
                                    .and_then(|x| x.as_bool())
                                    == Some(true)
                                    || v
                                        .pointer("/serverContent/generationComplete")
                                        .and_then(|x| x.as_bool())
                                        == Some(true);
                                if turn_done {
                                    model_generating = false;
                                    play_head.set_generating(false);
                                    mute_model_audio = false;
                                }
                                // Opening turn finished with an output transcript but zero
                                // audio. A re-kick on the same socket sometimes speaks
                                // and sometimes does not (「喂？你好。」 twice → 5 s
                                // silence → hangup). So re-kick right away, and if that turn is
                                // text-only too, fail → main opens a fresh Live session.
                                // turnComplete only (generationComplete of the same turn
                                // arrives ~50 ms earlier and must not count as a second turn).
                                let turn_complete = v
                                    .pointer("/serverContent/turnComplete")
                                    .and_then(|x| x.as_bool())
                                    == Some(true);
                                // A dropped repeat said nothing: no turn for the checks below.
                                // Nor did one whose drop the caller's words ended after its
                                // audio was over: its reply is asked for again.
                                let turn_had_audio = audio_chunks_out > chunks_at_turn_start;
                                let owed_unheard = turn_complete && repeat_guard.reply_owed() && !turn_had_audio;
                                let dropped_turn = repeat_guard.is_dropping() || owed_unheard;
                                // Nor one held back or dropped for the carrier's prompt.
                                let gated_turn = turn_complete && intl_gated_turn;
                                let tool_quiet = dropped_turn || gated_turn || tool_turn_quiet(
                                    tool_open.len(),
                                    tool_answer_at.map(|t| t.elapsed()),
                                    audio_chunks_out > tool_answer_chunks,
                                );
                                if turn_complete {
                                    turns_done += 1;
                                    chunks_at_turn_start = audio_chunks_out;
                                }
                                if turn_complete && dropped_turn {
                                    let owed = repeat_guard.reply_owed();
                                    repeat_guard.turn_complete();
                                    model_turn_pending = owed;
                                    turn_out_text.clear();
                                    out_leak_turn = false;
                                    if owed {
                                        // The caller spoke during it: it may have been their
                                        // answer — ask for the reply again, as after a switch.
                                        match send_text_before_deadline(
                                            &mut sink,
                                            persona_resume_reply_client_content(),
                                            tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                            "gemini dropped-turn reply kick send",
                                        )
                                        .await
                                        {
                                            Ok(()) => info!(
                                                "Gemini Live dropped turn may have answered the caller serial={serial} — reply asked for again"
                                            ),
                                            Err(e) => warn!(
                                                "Gemini Live dropped-turn reply kick failed serial={serial}: {e:#}"
                                            ),
                                        }
                                    } else {
                                        info!(
                                            "Gemini Live dropped repeat turn complete serial={serial} — turn checks skipped"
                                        );
                                    }
                                } else if gated_turn {
                                    intl_gated_turn = false;
                                    turn_out_text.clear();
                                    out_leak_turn = false;
                                    info!(
                                        "Gemini Live turnComplete of a turn kept off the line serial={serial} gate={intl_gate:?} voice_menu={} — turn checks skipped",
                                        ivr_since.is_some()
                                    );
                                } else if turn_complete && tool_quiet {
                                    info!(
                                        "Gemini Live turnComplete with a function call open serial={serial} open={} — reply still owed, turn checks skipped",
                                        tool_open.len()
                                    );
                                } else if turn_complete {
                                    model_turn_pending = false;
                                    // Heard (or no drop at all): nothing is owed any more.
                                    repeat_guard.turn_complete();
                                    if let Some(c0) = post_rotate_chunks {
                                        match post_rotate_turn(
                                            audio_chunks_out > c0,
                                            &turn_out_text,
                                            post_rotate_rekicked,
                                            activity_open,
                                        ) {
                                            PostRotateTurn::Rekick => {
                                                post_rotate_rekicked = true;
                                                model_turn_pending = true;
                                                let preview: String =
                                                    turn_out_text.chars().take(60).collect();
                                                match send_text_before_deadline(
                                                    &mut sink,
                                                    persona_repeat_line_client_content(),
                                                    tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                                    "gemini post-rotate re-kick send",
                                                )
                                                .await
                                                {
                                                    Ok(()) => warn!(
                                                        "Gemini Live text-only turn after rotation (0 audio) serial={serial} out_stt={preview} — asked to say it again"
                                                    ),
                                                    Err(e) => warn!(
                                                        "Gemini Live post-rotate re-kick failed serial={serial}: {e:#}"
                                                    ),
                                                }
                                            }
                                            PostRotateTurn::Spoke | PostRotateTurn::GiveUp => {
                                                post_rotate_chunks = None;
                                            }
                                        }
                                    }
                                    // The model answered what the caller said after our
                                    // closing: closing again → hang up; anything else →
                                    // it was new content, keep talking.
                                    if post_arm_talk && audio_chunks_out > chunks_at_last_caller_eou {
                                        post_arm_talk = false;
                                        let preview: String = turn_out_text.chars().take(40).collect();
                                        if looks_like_persona_closing(&turn_out_text) {
                                            info!(
                                                "Persona closing kept (model closed again) serial={serial} text={preview}"
                                            );
                                        } else {
                                            goodbye_pending = false;
                                            bye_from_caller = false;
                                            caller_end_signal = false;
                                            output_goodbye_at = None;
                                            end_reason = SessionEndReason::WsClosed;
                                            out_tx_window.clear();
                                            in_tx_window.clear();
                                            info!(
                                                "Persona closing disarmed (caller added something new) serial={serial} text={preview}"
                                            );
                                        }
                                    }
                                    // Stalling: a farewell was cut from this reply — now that its
                                    // generation is over, the model holds the caller — or the
                                    // reply ended on a closing the transcript check missed
                                    // (「先這樣」): have the model take that back. Once per turn.
                                    let keep = stall_keep_at_turn_end(
                                        stall_keep_after_turn,
                                        stall_turn_kept,
                                        persona
                                            && stall_on
                                            && !wrap_up_sent
                                            && stall_keeps < STALL_KEEP_MAX
                                            && turn_ends_with_closing(&stall_tx),
                                    );
                                    stall_keep_after_turn = false;
                                    if let Some(why) = keep {
                                        if why == StallKeepAtTurnEnd::AiSaidBye {
                                            stall_keeps += 1;
                                            stall_keep_due = None;
                                        }
                                        stall_turn_kept = true;
                                        model_turn_pending = true;
                                        let preview: String = stall_tx.chars().take(40).collect();
                                        stall_tx.clear();
                                        match send_text_before_deadline(
                                            &mut sink,
                                            persona_stall_keep_client_content(true),
                                            tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                            "gemini persona stall keep send",
                                        )
                                        .await
                                        {
                                            Ok(()) => info!(
                                                "Persona stall keep nudge serial={serial} n={stall_keeps} why={why:?} text={preview}"
                                            ),
                                            Err(e) => warn!(
                                                "Persona stall keep send failed serial={serial}: {e:#}"
                                            ),
                                        }
                                    }
                                    turn_out_text.clear();
                                    out_leak_turn = false;
                                }
                                if is_transcript_only_turn(
                                    turn_complete && !tool_quiet,
                                    audio_chunks_out,
                                    delivered_bytes.load(Ordering::Relaxed),
                                    &out_tx_window,
                                ) {
                                    let stt_preview: String =
                                        out_tx_window.chars().take(80).collect();
                                    if first_delivery_kick_retried {
                                        bail!(
                                            "gemini transcript-only turn (0 audio) after re-kick serial={serial} out_stt={stt_preview}"
                                        );
                                    }
                                    out_tx_window.clear();
                                    send_text_before_deadline(
                                        &mut sink,
                                        greets_kick_text.clone(),
                                        first_delivery_deadline,
                                        "gemini transcript-only re-kick send",
                                    )
                                    .await?;
                                    first_delivery_kick_retried = true;
                                    warn!(
                                        "Gemini Live opening turn text-only (0 audio) serial={serial} out_stt={stt_preview} -- re-kick now (fresh session if it repeats)"
                                    );
                                }
                                if turn_done && !tool_quiet && !opening_turn_complete {
                                    let covered =
                                        output_covers_owner_status(&out_tx_window, &owner_status);
                                    let stt_empty = out_tx_window.trim().is_empty();
                                    let audio_ms = pcm16_ms(downlink_pcm_bytes, phone_sr);
                                    let stt_preview: String =
                                        out_tx_window.chars().take(80).collect();
                                    if covered {
                                        opening_turn_complete = true;
                                        info!(
                                            "Owner-status opening covered by STT serial={serial}"
                                        );
                                    } else if stt_empty && audio_ms >= 2800 {
                                        opening_turn_complete = true;
                                        info!(
                                            "Owner-status opening STT empty audio_ms={audio_ms} — release barge-in serial={serial}"
                                        );
                                    } else if !status_opening_retried {
                                        status_opening_retried = true;
                                        let kick =
                                            status_retry_kick_client_content(&owner_status);
                                        if send_text_before_deadline(
                                            &mut sink,
                                            kick,
                                            api_send_deadline(
                                                delivered_bytes.as_ref(),
                                                first_delivery_deadline,
                                            ),
                                            "gemini owner-status retry kick send",
                                        )
                                        .await
                                        .is_ok()
                                        {
                                            info!(
                                                "Owner-status missing in opening stt={stt_preview} audio_ms={audio_ms} — retry kick serial={serial}"
                                            );
                                        } else {
                                            opening_turn_complete = true;
                                            warn!(
                                                "Owner-status retry kick failed — release barge-in serial={serial}"
                                            );
                                        }
                                    } else {
                                        opening_turn_complete = true;
                                        warn!(
                                            "Owner-status still missing after retry stt={stt_preview} — release barge-in serial={serial}"
                                        );
                                    }
                                }
                                if turn_done && !tool_quiet {
                                    let model_spoke_after_caller = caller_had_voice
                                        && audio_chunks_out > chunks_at_last_caller_eou;
                                    let post_ms = pcm16_ms(
                                        downlink_pcm_bytes
                                            .saturating_sub(bytes_at_last_caller_eou),
                                        phone_sr,
                                    );
                                    let stt_preview: String =
                                        out_tx_window.chars().take(120).collect();
                                    info!(
                                        "Gemini Live turnComplete serial={serial} out={audio_chunks_out} goodbye_pending={goodbye_pending} complete_recap={saw_relay_ack} after_caller={model_spoke_after_caller} post_ms={post_ms} activity_open={activity_open} out_stt={stt_preview}"
                                    );
                                    last_activity_at = tokio::time::Instant::now();
                                    // Complete recap (with or without spoken 再見): arm hangup
                                    // after delay so clone WAV cannot cut the recap (「關於…」).
                                    // Caller 再見: wait for this model turn, then hangup.
                                    // STT miss: recap-length model audio after the caller (not 「好的」).
                                    // Persona has no recap ritual: only a closing / caller bye ends it.
                                    let fallback = !persona && post_caller_model_turn_should_hangup(
                                        caller_had_voice,
                                        model_spoke_after_caller,
                                        &out_tx_window,
                                        post_ms,
                                        activity_open,
                                        asked_clarify,
                                    );
                                    if saw_relay_ack || goodbye_pending || fallback {
                                        let first_arm = !goodbye_pending;
                                        goodbye_pending = true;
                                        end_reason = SessionEndReason::Goodbye;
                                        if first_arm {
                                            let why = if saw_relay_ack {
                                                "turnComplete+complete_recap"
                                            } else if fallback {
                                                "turnComplete+post_caller_recap"
                                            } else {
                                                "turnComplete+caller_farewell"
                                            };
                                            info!(
                                                "Goodbye armed ({why}) serial={serial} complete_recap={saw_relay_ack} — wait player drain+idle then {hangup_delay_ms}ms then WAV"
                                            );
                                        }
                                    } else if !persona
                                        && !asked_clarify
                                        && caller_had_voice
                                        && model_spoke_after_caller
                                        && !activity_open
                                        && !looks_like_clarifying_question(&out_tx_window)
                                        && !recap_names_matter(&out_tx_window)
                                    {
                                        asked_clarify = true;
                                        let kick = clarify_or_recap_kick_text();
                                        if send_text_before_deadline(
                                            &mut sink,
                                            kick,
                                            api_send_deadline(
                                                delivered_bytes.as_ref(),
                                                first_delivery_deadline,
                                            ),
                                            "gemini clarify/recap kick send",
                                        )
                                        .await
                                        .is_ok()
                                        {
                                            info!(
                                                "Clarify/recap kick sent serial={serial} (caller spoke, no matter recap; do not goodbye)"
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        warn!("Gemini Live ws error serial={serial} err={e}");
                        if persona && !goodbye_pending && rotations < LIVE_ROTATE_MAX {
                            ws_dead = true;
                            continue;
                        }
                        if goodbye_pending {
                            end_reason = SessionEndReason::Goodbye;
                            signal_force_hangup(&mut hangup_signaled);
                        }
                        break;
                    }
                    None => {
                        info!("Gemini Live ws stream end serial={serial}");
                        if persona && !goodbye_pending && rotations < LIVE_ROTATE_MAX {
                            ws_dead = true;
                            continue;
                        }
                        if goodbye_pending {
                            end_reason = SessionEndReason::Goodbye;
                            signal_force_hangup(&mut hangup_signaled);
                        }
                        break;
                    }
                }
            }
            done = tool_rx.recv() => {
                let Some(done) = done else { continue };
                let open = if done.blocking {
                    tool_open.remove(&done.key)
                } else {
                    tool_bg.remove(&done.key)
                };
                if done.gen != live_gen || !open {
                    info!(
                        "Gemini Live function answer dropped serial={serial} name={} id={} (cancelled or older session)",
                        done.name, done.key
                    );
                    continue;
                }
                let now = tokio::time::Instant::now();
                let msg = crate::appointments::tool_response(&done.id, &done.name, done.response.clone());
                match send_text_before_deadline(
                    &mut sink,
                    msg.to_string(),
                    now + API_SEND_TIMEOUT,
                    "gemini toolResponse send",
                )
                .await
                {
                    Ok(()) => info!(
                        "Gemini Live toolResponse serial={serial} name={} id={} blocking={} response={}",
                        done.name, done.key, done.blocking, done.response
                    ),
                    Err(e) => warn!("Gemini Live toolResponse send failed serial={serial}: {e:#}"),
                }
                last_activity_at = now;
                // Said before the call and answered silently: a turn of its own
                // right after would be the same line again. Not once the call is
                // judged sales / scam: the stalling kicks own every turn then.
                if let Some(call) = done
                    .after_speech
                    .filter(|_| !stall_on && crate::appointments::answer_is_silent(&done.name, &done.response))
                {
                    repeat_guard.arm(call, turns_done, audio_chunks_out > chunks_at_turn_start, now);
                    info!(
                        "Gemini Live repeat guard armed serial={serial} id={} turns_done={turns_done}",
                        done.key
                    );
                }
                if done.blocking {
                    // The model speaks after a blocking answer.
                    model_turn_pending = true;
                    tool_answer_at = Some(now);
                    tool_answer_chunks = audio_chunks_out;
                }
            }
            verdict = review_rx.recv() => {
                let Some((score, verdict)) = verdict else { continue };
                let step = stall_review.verdict(verdict, score);
                info!(
                    "Persona stall review serial={serial} verdict={verdict:?} score_asked={score} score_now={stall_score} reviews={} → {step:?}",
                    stall_review.asked
                );
                match step {
                    StallVerdictStep::Engage => stall_engage_now = true,
                    StallVerdictStep::Ivr => {
                        ivr_enter.get_or_insert("review");
                    }
                    StallVerdictStep::Carry => {}
                }
            }
            up = uplink_rx.recv() => {
                if cancel.load(Ordering::Relaxed) {
                    end_reason = resolve_cancel(goodbye_pending);
                    if goodbye_pending {
                        signal_force_hangup(&mut hangup_signaled);
                    } else {
                        info!(
                            "Gemini Live cancel mid-uplink serial={serial} — closing WS now (stop API tokens)"
                        );
                    }
                    break;
                }
                match up {
                    Some(_) if !opening_turn_complete => {
                        // Drop caller barge-in until the desk-status opening has been spoken.
                    }
                    Some(pcm) if !pcm.is_empty() => {
                        let pcm16 = if phone_sr == 16_000 {
                            pcm
                        } else {
                            resample_s16le_mono_pub(&pcm, phone_sr, 16_000)
                        };
                        let frame_ms = ((pcm16.len() / 2) as u32 * 1000 / 16_000).max(1);
                        // Raw level before AGC: VAD compares it with the line-noise floor.
                        let raw_rms = pcm_rms_s16le(&pcm16);
                        // main.rs swaps our own echo for digital zeros.
                        let gated = raw_rms < 1.0;
                        uplink_frames += 1;
                        if gated {
                            gated_frames += 1;
                        }
                        // Min-statistics floor: speech always has near-floor gaps within
                        // the window, so it keeps tracking during a turn too — a turn
                        // opened by line noise at call start must still be able to end.
                        if !gated {
                            noise_floor.push(raw_rms, frame_ms);
                        }
                        let raw_thr = noise_floor.voice_threshold();
                        // AGC before VAD + send: the phone earpiece is pinned to min (no
                        // local audio), so the far end arrives at ≈ −40 dBFS. Only lift
                        // speech-like frames — boosting line noise between words 16× made
                        // Gemini hear hiss.
                        let pcm16 = if raw_rms >= raw_thr {
                            agc_uplink_frame(&pcm16)
                        } else {
                            pcm16
                        };
                        let rms = pcm_rms_s16le(&pcm16);
                        // Greeting sidetone is not the caller. Hold VAD while downlink is
                        // playing plus a short echo tail (echo armed a fake turn).
                        let downlink_echo = !play_head.drained_for(
                            Duration::from_millis(DOWNLINK_ECHO_HOLD_MS),
                            PLAYER_DRAIN_SLACK_BYTES,
                        );
                        // Echo-free line (barge-in on): the caller is heard over our audio.
                        let overlap = barge_in.load(Ordering::Relaxed);
                        let frame_voice = rms >= LOCAL_VAD_RMS && raw_rms >= raw_thr;
                        // …but while our reply is in their ear, what they say stays here
                        // (Gemini gets silence) until it is clearly more than an "mm":
                        // people do not stop mid-word for a backchannel.
                        let ai_audible = overlap && play_head.ai_audible(OVERLAP_AUDIBLE_TAIL);
                        let mut hold_frame = false;
                        let mut frame_in_hold = false;
                        if server_vad && overlap {
                            let now = tokio::time::Instant::now();
                            if ai_audible && !activity_open && !overlap_floor {
                                hold_frame = true;
                                frame_in_hold = true;
                                if frame_voice {
                                    last_caller_voice_at = Some(now);
                                    last_activity_at = now;
                                }
                                if let Some((held, run_ms)) =
                                    overlap_hold.push(&pcm16, frame_voice, frame_ms, now)
                                {
                                    // They mean it: Gemini hears them from their first
                                    // syllable, and we stop like a person who gets cut off.
                                    hold_frame = false;
                                    overlap_floor = true;
                                    uplink_buf.extend_from_slice(&held);
                                    voiced_ms = run_ms.saturating_sub(frame_ms);
                                    silence_ms = 0;
                                    if persona && goodbye_pending {
                                        voice_since_arm_ms =
                                            voice_since_arm_ms.saturating_add(voiced_ms);
                                    }
                                    let left = play_head.speech_left(phone_sr);
                                    if barged_at_chunks.map_or(true, |c| audio_chunks_out > c) {
                                        barged_at_chunks = Some(audio_chunks_out);
                                        if model_generating
                                            || left > Duration::from_millis(BARGE_IN_FINISH_MS)
                                        {
                                            barge_ins += 1;
                                            play_head.request_flush();
                                            play_head.set_generating(false);
                                            mute_model_audio = model_generating;
                                            info!(
                                                "Gemini Live barge-in serial={serial} voiced_ms={run_ms} raw_rms={raw_rms:.0} generating={model_generating} left_ms={} n={barge_ins} — caller cut in, stop our playback",
                                                left.as_millis()
                                            );
                                        } else {
                                            info!(
                                                "Gemini Live caller cut in serial={serial} voiced_ms={run_ms} left_ms={} — let it finish",
                                                left.as_millis()
                                            );
                                        }
                                    }
                                }
                            } else {
                                if !overlap_hold.is_idle() {
                                    // Our audio is over. A word that landed on its end is
                                    // the caller's answer; an earlier "mm" was not for us.
                                    match overlap_hold.end(now) {
                                        OverlapEnd::Answer { held, voiced_ms: said_ms, open } => {
                                            uplink_buf.extend_from_slice(&held);
                                            caller_had_voice = true;
                                            if persona && goodbye_pending {
                                                voice_since_arm_ms =
                                                    voice_since_arm_ms.saturating_add(said_ms);
                                            }
                                            if let Some((run_ms, quiet_ms)) = open {
                                                // Still talking: the local turn opens with it.
                                                voiced_ms = run_ms;
                                                silence_ms = quiet_ms;
                                            } else {
                                                local_eou_sent += 1;
                                                model_turn_pending = true;
                                                chunks_at_last_caller_eou = audio_chunks_out;
                                                bytes_at_last_caller_eou = downlink_pcm_bytes;
                                                last_caller_eou_at = Some(now);
                                            }
                                            info!(
                                                "Gemini Live caller spoke over the end of our reply serial={serial} voiced_ms={said_ms} held_ms={} — passed on",
                                                pcm16_ms(held.len() as u64, 16_000)
                                            );
                                        }
                                        OverlapEnd::Backchannel { voiced_ms: said_ms } => {
                                            backchannels += 1;
                                            info!(
                                                "Gemini Live backchannel over our reply serial={serial} voiced_ms={said_ms} n={backchannels} — not an interruption"
                                            );
                                        }
                                        OverlapEnd::Nothing => {}
                                    }
                                }
                                if !ai_audible {
                                    overlap_floor = false;
                                }
                            }
                        }
                        let is_voice = frame_voice
                            && !hold_frame
                            && (!downlink_echo || activity_open || overlap);

                        if hold_frame || (downlink_echo && !activity_open && !overlap) {
                            voiced_ms = 0;
                            silence_ms = 0;
                        }

                        tick_local_vad(
                            is_voice,
                            activity_open,
                            frame_ms,
                            &mut voiced_ms,
                            &mut silence_ms,
                        );
                        // Caller already talking when our reply started (or local VAD
                        // mode): stop our playback like a person who gets interrupted
                        // (once per AI turn; a short "mm" does not).
                        if overlap
                            && is_voice
                            && voiced_ms >= BARGE_IN_VOICE_MS
                            && barged_at_chunks.map_or(true, |c| audio_chunks_out > c)
                            && ai_audible
                        {
                            barged_at_chunks = Some(audio_chunks_out);
                            barge_ins += 1;
                            play_head.request_flush();
                            play_head.set_generating(false);
                            mute_model_audio = model_generating;
                            info!(
                                "Gemini Live barge-in serial={serial} voiced_ms={voiced_ms} raw_rms={raw_rms:.0} generating={model_generating} n={barge_ins} — caller cut in, stop our playback"
                            );
                        }
                        if is_voice {
                            last_caller_voice_at = Some(tokio::time::Instant::now());
                            last_activity_at = tokio::time::Instant::now();
                            idle_nudges = 0;
                            stall_nudge_chunks = None;
                            if persona && goodbye_pending {
                                voice_since_arm_ms = voice_since_arm_ms.saturating_add(frame_ms);
                            }
                            if voiced_ms >= LOCAL_MIN_VOICE_MS {
                                caller_had_voice = true;
                                // Persona: a short 「掰掰」 back must not cancel the hangup, but
                                // real new content after the closing ("oh, by the way…") keeps talking.
                                // After the safety-cap wrap-up the call ends regardless.
                                if persona {
                                    // A long reply after the closing is a polite farewell
                                    // (thanks, good wishes, 拜拜) or something new —
                                    // decided by their words or the model's next turn.
                                    if goodbye_pending
                                        && !wrap_up_sent
                                        && !post_arm_talk
                                        && voice_since_arm_ms >= PERSONA_POST_ARM_VOICE_MS
                                    {
                                        post_arm_talk = true;
                                        in_tx_window.clear();
                                        caller_end_signal = false;
                                        info!(
                                            "Persona caller kept talking {voice_since_arm_ms}ms after the closing — farewell or new content? serial={serial}"
                                        );
                                    }
                                } else if goodbye_pending && !saw_relay_ack {
                                    goodbye_pending = false;
                                    bye_from_caller = false;
                                    output_goodbye_at = None;
                                    info!(
                                        "Goodbye disarmed (caller speaking, no complete recap) serial={serial}"
                                    );
                                }
                            }
                            if !activity_open && voiced_ms >= LOCAL_MIN_VOICE_MS {
                                // Server VAD: the local turn is bookkeeping only (bye /
                                // idle / rotation logic) — Gemini already hears the line.
                                let opened = server_vad || {
                                    let start = json!({ "realtimeInput": { "activityStart": {} } });
                                    send_text_before_deadline(
                                        &mut sink,
                                        start.to_string(),
                                        api_send_deadline(
                                            delivered_bytes.as_ref(),
                                            first_delivery_deadline,
                                        ),
                                        "gemini activityStart send",
                                    )
                                    .await
                                    .is_ok()
                                };
                                if opened {
                                    activity_open = true;
                                    activity_ms = 0;
                                    turn_raw_max = 0.0;
                                    // The caller talks: whatever the model says next is for them,
                                    // never a repeat to drop.
                                    if repeat_guard.is_dropping() {
                                        info!("Gemini Live caller started talking serial={serial} — repeat drop ended");
                                    }
                                    repeat_guard.caller_started();
                                    // The syllables VAD needed to confirm voice go first.
                                    let pre = std::mem::take(&mut preroll);
                                    let preroll_ms = pcm16_ms(pre.len() as u64, 16_000);
                                    uplink_buf.extend_from_slice(&pre);
                                    info!(
                                        "Gemini Live {} serial={serial} voiced_ms={voiced_ms} rms={rms:.0} raw_rms={raw_rms:.0} floor={:.0} thr={raw_thr:.0} preroll_ms={preroll_ms}",
                                        if server_vad { "caller voice (server VAD)" } else { "activityStart" },
                                        noise_floor.floor()
                                    );
                                }
                            }
                        }

                        // If goodbye already heard and caller is silent, still allow a short drain.
                        if goodbye_pending
                            && !activity_open
                            && last_caller_voice_at
                                .map(|t| t.elapsed() >= Duration::from_millis(900))
                                .unwrap_or(true)
                        {
                            // Wait for model turnComplete in other branch; if model already done, end.
                        }

                        if activity_open {
                            activity_ms = activity_ms.saturating_add(frame_ms);
                            if is_voice {
                                turn_raw_max = turn_raw_max.max(raw_rms);
                            }
                        }
                        if server_vad {
                            // Gemini hears the line continuously. Our own playback (+ echo
                            // tail) goes as silence so it is never taken for the caller —
                            // except on an echo-free line, where the caller may cut in.
                            if hold_frame || (downlink_echo && !activity_open && !overlap) {
                                uplink_buf.resize(uplink_buf.len() + pcm16.len(), 0);
                            } else if !frame_in_hold {
                                uplink_buf.extend_from_slice(&pcm16);
                            }
                        } else if activity_open {
                            uplink_buf.extend_from_slice(&pcm16);
                        }
                        if server_vad || activity_open {
                            while uplink_buf.len() >= chunk_target {
                                let take: Vec<u8> = uplink_buf.drain(..chunk_target).collect();
                                let b64 = B64.encode(&take);
                                let msg = json!({
                                    "realtimeInput": {
                                        "audio": {
                                            "data": b64,
                                            "mimeType": "audio/pcm;rate=16000"
                                        }
                                    }
                                });
                                if send_text_before_deadline(
                                    &mut sink,
                                    msg.to_string(),
                                    api_send_deadline(
                                        delivered_bytes.as_ref(),
                                        first_delivery_deadline,
                                    ),
                                    "gemini realtime audio send",
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                                audio_chunks_in += 1;
                            }
                        } else {
                            // Manual activity: audio outside activityStart/End is not part of
                            // any turn. Hold the newest slice so the next turn keeps its onset.
                            push_preroll(&mut preroll, &pcm16, preroll_cap);
                        }

                        // End user turn only after EOU silence **and** enough speech in turn
                        // (a lone short word waits a bit longer), or after
                        // LOCAL_MAX_ACTIVITY_MS so noise cannot stall the turn.
                        let force_end = activity_open && activity_ms >= LOCAL_MAX_ACTIVITY_MS;
                        if activity_open
                            && (local_turn_complete(voiced_ms, silence_ms) || force_end)
                        {
                            if !server_vad && !uplink_buf.is_empty() {
                                let take = std::mem::take(&mut uplink_buf);
                                let b64 = B64.encode(&take);
                                let msg = json!({
                                    "realtimeInput": {
                                        "audio": {
                                            "data": b64,
                                            "mimeType": "audio/pcm;rate=16000"
                                        }
                                    }
                                });
                                if send_text_before_deadline(
                                    &mut sink,
                                    msg.to_string(),
                                    api_send_deadline(
                                        delivered_bytes.as_ref(),
                                        first_delivery_deadline,
                                    ),
                                    "gemini realtime audio flush send",
                                )
                                .await
                                .is_ok()
                                {
                                    audio_chunks_in += 1;
                                }
                            }
                            // Server VAD: Gemini ends the turn itself; this only keeps the
                            // local bookkeeping in step.
                            let ended = server_vad || {
                                let end = json!({ "realtimeInput": { "activityEnd": {} } });
                                send_text_before_deadline(
                                    &mut sink,
                                    end.to_string(),
                                    api_send_deadline(
                                        delivered_bytes.as_ref(),
                                        first_delivery_deadline,
                                    ),
                                    "gemini activityEnd send",
                                )
                                .await
                                .is_ok()
                            };
                            if ended {
                                local_eou_sent += 1;
                                model_turn_pending = true;
                                chunks_at_last_caller_eou = audio_chunks_out;
                                bytes_at_last_caller_eou = downlink_pcm_bytes;
                                last_caller_eou_at = Some(tokio::time::Instant::now());
                                info!(
                                    "Gemini Live {} serial={serial} silence_ms={silence_ms} voiced_ms={voiced_ms} activity_ms={activity_ms} forced={force_end} short={} raw_max={turn_raw_max:.0} floor={:.0} n={local_eou_sent} chunks_out={audio_chunks_out}",
                                    if server_vad { "caller voice end (local)" } else { "activityEnd" },
                                    voiced_ms < LOCAL_MIN_TURN_VOICE_MS,
                                    noise_floor.floor()
                                );
                            }
                            activity_open = false;
                            activity_ms = 0;
                            voiced_ms = 0;
                            silence_ms = 0;
                        }
                    }
                    Some(_) => {}
                    None => {
                        // Uplink sender dropped = hangup / session teardown.
                        info!("Gemini Live uplink closed serial={serial} — stopping WS");
                        end_reason = SessionEndReason::Cancelled;
                        break;
                    }
                }
            }
        }
    }
    // Immediate close on hangup — stops Gemini Live token burn.
    let _ = sink.send(WsMessage::Close(None)).await;
    let _ = sink.close().await;
    // A function call may still be writing to the calendar: let it finish (at
    // most TOOL_WAIT) so the end of the call sees the event it wrote.
    tool_tasks.retain(|h| !h.is_finished());
    if !tool_tasks.is_empty() {
        let n = tool_tasks.len();
        let done = tokio::time::timeout(
            crate::appointments::TOOL_WAIT,
            futures_util::future::join_all(tool_tasks),
        )
        .await
        .is_ok();
        info!("Gemini Live waited for {n} function call(s) at session end serial={serial} finished={done}");
    }
    info!(
        "Gemini Live session end serial={serial} reason={} out={audio_chunks_out} in={audio_chunks_in} eou={local_eou_sent} uplink_frames={uplink_frames} gated_frames={gated_frames} floor={:.0} stall={stall_on} stall_hits={stall_hits:?} own_matter={own_matter} idle_nudges={idle_nudges} rotations={rotations} server_vad={server_vad} barge_in={} barge_ins={barge_ins} backchannels={backchannels} tokens_in={usage_prompt_tokens} tokens_out={usage_response_tokens} talk_s={} (API closed)",
        end_reason.as_str(),
        noise_floor.floor(),
        barge_in.load(Ordering::Relaxed),
        session_start.elapsed().as_secs()
    );
    Ok(end_reason)
}

/// Cut the transcript back to `mark` (a text-only opening nobody heard);
/// returns what was removed.
fn truncate_transcript(transcript_out: &Option<Arc<std::sync::Mutex<String>>>, mark: usize) -> String {
    let Some(buf) = transcript_out else {
        return String::new();
    };
    let Ok(mut g) = buf.lock() else {
        return String::new();
    };
    if mark >= g.len() || !g.is_char_boundary(mark) {
        return String::new();
    }
    let dropped = g[mark..].trim().to_string();
    g.truncate(mark);
    dropped
}

fn append_transcriptions(v: &Value, transcript_out: &Option<Arc<std::sync::Mutex<String>>>) {
    let Some(buf) = transcript_out else {
        return;
    };
    let mut pieces: Vec<String> = Vec::new();
    for (path, label) in [
        ("/serverContent/inputTranscription/text", "對方"),
        ("/serverContent/outputTranscription/text", "接聽"),
        // Alternate shapes
        ("/inputTranscription/text", "對方"),
        ("/outputTranscription/text", "接聽"),
    ] {
        if let Some(t) = v
            .pointer(path)
            .and_then(|x| x.as_str())
            .map(|raw| (raw.starts_with(char::is_whitespace), tidy_cjk_spaces(raw)))
            .filter(|(_, s)| !s.is_empty())
        {
            let (lead, t) = t;
            // The STT starts a new word with a space; it is kept so the merged
            // line keeps word spaces (Korean, accented Latin): see join_fragment.
            let gap = if lead && t.chars().next().is_some_and(|c| !is_cjk(c)) { " " } else { "" };
            pieces.push(format!(
                "{label}：{gap}{}",
                crate::zh_tw::to_tw(&t)
            ));
        }
    }
    if pieces.is_empty() {
        return;
    }
    if let Ok(mut g) = buf.lock() {
        for p in pieces {
            if !g.is_empty() {
                g.push('\n');
            }
            g.push_str(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    #[test]
    fn noise_floor_raw_minimum_until_history() {
        let mut nf = NoiseFloor::default();
        assert_eq!(nf.voice_threshold(), LOCAL_VAD_RAW_MIN_RMS);
        // < LOCAL_NOISE_FLOOR_MIN_HISTORY_MS: a loud first「喂」must not set the bar.
        for _ in 0..3 {
            nf.push(4000.0, 60);
        }
        assert_eq!(nf.voice_threshold(), LOCAL_VAD_RAW_MIN_RMS);
    }

    #[test]
    fn noise_floor_lifts_threshold_on_noisy_line_only() {
        // Quiet line (~10–26 RMS): bar stays at the raw minimum.
        let mut quiet = NoiseFloor::default();
        for i in 0..40 {
            quiet.push(10.0 + (i % 5) as f32 * 4.0, 60);
        }
        assert_eq!(quiet.voice_threshold(), LOCAL_VAD_RAW_MIN_RMS);
        // Old low capture (digital floor ~1, caller 100–500): speech still passes.
        let mut digital = NoiseFloor::default();
        for _ in 0..40 {
            digital.push(1.0, 60);
        }
        assert!(digital.voice_threshold() <= 100.0);
        // Street noise ~150–300 RMS: the old ≥40 gate called it endless voice.
        let mut noisy = NoiseFloor::default();
        for i in 0..40 {
            noisy.push(150.0 + (i % 4) as f32 * 50.0, 60);
        }
        let thr = noisy.voice_threshold();
        assert!(thr >= 400.0 && thr <= 500.0, "thr={thr}");
        assert!(3000.0 >= thr, "max-volume speech must pass");
        // Pathological floor is capped so speech can always open a turn.
        let mut loud = NoiseFloor::default();
        for _ in 0..40 {
            loud.push(5000.0, 60);
        }
        assert_eq!(loud.voice_threshold(), LOCAL_VAD_RAW_MAX_THRESHOLD);
    }

    #[test]
    fn noise_floor_window_forgets_old_noise() {
        let mut nf = NoiseFloor::default();
        for _ in 0..40 {
            nf.push(10.0, 60);
        }
        for _ in 0..60 {
            nf.push(200.0, 60);
        }
        assert!(nf.total_ms <= LOCAL_NOISE_FLOOR_WINDOW_MS);
        assert_eq!(nf.floor(), 200.0);
    }

    #[test]
    fn goodbye_clock_waits_for_caller_and_model() {
        assert!(goodbye_clock_may_run(true, false, false));
        assert!(!goodbye_clock_may_run(false, false, false), "our audio still playing");
        assert!(!goodbye_clock_may_run(true, true, false), "caller mid-turn");
        assert!(!goodbye_clock_may_run(true, false, true), "model has not answered the bye yet");
        assert!(CALLER_BYE_MODEL_WAIT >= Duration::from_millis(2_500));
    }

    #[test]
    fn reply_after_our_closing_waits_for_a_decision() {
        let fresh = Some(Duration::from_millis(800));
        let stale = Some(CALLER_BYE_MODEL_WAIT + Duration::from_millis(1));
        // Caller said bye first: wait for the model's answer, at most CALLER_BYE_MODEL_WAIT.
        assert!(goodbye_waits_on_model(true, false, false, fresh));
        assert!(!goodbye_waits_on_model(true, false, true, fresh));
        assert!(!goodbye_waits_on_model(true, false, false, stale));
        // The caller talked on after our 掰掰 — undecided until their
        // words or the model's next turn show farewell vs new content…
        assert!(goodbye_waits_on_model(false, true, false, fresh));
        assert!(goodbye_waits_on_model(false, true, true, stale), "model answering: wait for its turnComplete");
        // …but a model that stays silent does not hold the line open.
        assert!(!goodbye_waits_on_model(false, true, false, stale));
        assert!(!goodbye_waits_on_model(false, false, false, fresh));
        assert!(PERSONA_POST_ARM_VOICE_MS <= 1_000);
    }

    #[test]
    fn hangup_needs_the_caller_to_end_the_call() {
        // Hang up only after the caller says bye / wraps up.
        for t in [
            // A long polite closing, in two variants.
            "喂,不好意思,那也不打擾您,祝您事事順心,謝謝您哦,好,拜拜。",
            "那我就先結束電話。好，拜拜。",
            "好的，那我們這邊就先這樣，謝謝您，再見。",
            "好，那就先這樣囉",
            "那也不打擾您了",
            "好,那就這樣,謝謝,拜拜。",
            "沒事了，謝謝",
            "祝您生活愉快",
            "OK bye bye",
            "好 , 掰 掰",
            "好的，那我们就先这样，再见",
            // Thanks after the closing.
            "好的，那就先這樣，謝謝您",
            "那再跟你聯絡，謝謝",
            "沒事了，謝謝喔",
            // More of how people close.
            "好，改天再聊",
            "那下次再聊囉",
            "那就麻煩你了",
            "好，先這樣囉",
            "就這樣子囉",
            "多謝，掰掰",
            "那我跟你說掰掰喔",
            // Cantonese (+852).
            "唔該晒",
            "好，先咁啦",
            "冇嘢喇，拜拜",
            "唔該你，再見",
            "Alright, that's all, thank you so much",
        ] {
            assert!(caller_wraps_up(t), "{t}");
        }
        for t in [
            // The caller was about to ask when to call back.
            "我晚點回電話給您，沒問題。",
            "如果後續針對在分行服務面有問題，在營業的時間您都可以跟我這邊做聯繫。",
            "那就這樣嗎？",
            "先這樣算的話，一個月要繳多少？",
            "那個方案我們就這樣子處理比較快",
            "請問您現在沒事嗎",
            "好",
            "",
            // Thanks alone.
            "謝謝",
            "謝謝您喔",
            // A goodbye said to someone on the caller's side.
            "來，跟阿嬤說掰掰",
            "跟叔叔說拜拜",
            "say bye to grandma",
            "唔該，我想問下幾時送到？",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
        }
        // The AI's own 掰掰 holds the line until the caller's bye or a quiet line.
        assert!(awaits_caller_bye(false, false));
        assert!(!awaits_caller_bye(true, false));
        assert!(!awaits_caller_bye(false, true));
        assert!(PERSONA_BYE_SILENCE >= Duration::from_secs(5));
    }

    #[test]
    fn a_goodbye_that_turns_into_a_question_is_taken_back() {
        // 「那就先這樣」 came in first and armed the caller's goodbye…
        assert!(caller_wraps_up("那就先這樣"));
        // …then 「嗎？」: the AI answers instead of hanging up after it.
        assert!(caller_withdraws_bye("那就先這樣嗎？", "嗎？"));
        assert!(caller_wraps_up("Okay, that's all"));
        assert!(caller_withdraws_bye("Okay, that's all for today, right?", " for today, right?"));
        assert!(caller_wraps_up("それでは"));
        assert!(caller_withdraws_bye("それではよろしいでしょうか？", "よろしいでしょうか？"));
        // An "mm" after the goodbye, or more goodbye words, take nothing back.
        assert!(!caller_withdraws_bye("好，那就先這樣，謝謝。好的", "好的"));
        assert!(!caller_withdraws_bye("Okay, that's all. Mm", "Mm"));
        assert!(!caller_withdraws_bye("那就先這樣，掰掰", "掰掰"));
    }

    #[test]
    fn the_first_words_wait_for_the_carrier_prompt_with_or_without_a_prepared_opening() {
        let t0 = tokio::time::Instant::now();
        // Every persona call that expects the warning starts held: a cold start or a
        // fresh retry (nothing prepared while it rang) too.
        let waiting = IntlGate::Waiting { until: t0 + INTL_OPENING_HOLD };
        assert_eq!(intl_gate_at_unmute(true, true, t0), waiting);
        assert_eq!(intl_gate_at_unmute(true, false, t0), IntlGate::Open);
        assert_eq!(intl_gate_at_unmute(false, true, t0), IntlGate::Open);
        // Quiet line: held, then played.
        assert_eq!(intl_gate_step(waiting, t0, false, Some(Duration::MAX)), IntlGateStep::Wait);
        assert_eq!(intl_gate_step(waiting, t0 + INTL_OPENING_HOLD, false, Some(Duration::MAX)), IntlGateStep::Play);
        // Line voice: the warning has started.
        assert_eq!(intl_gate_step(waiting, t0 + Duration::from_millis(900), true, None), IntlGateStep::PromptStarted);
        // The model's words wait while it plays, through its short pauses…
        let prompt = IntlGate::Prompt { until: t0 + CARRIER_PROMPT_SPAN };
        let mid = t0 + Duration::from_secs(3);
        assert_eq!(intl_gate_step(prompt, mid, true, None), IntlGateStep::Wait);
        assert_eq!(intl_gate_step(prompt, mid, true, Some(Duration::from_millis(300))), IntlGateStep::Wait);
        // …until the line is quiet, or the span is over.
        assert_eq!(intl_gate_step(prompt, mid, true, Some(CARRIER_PROMPT_QUIET)), IntlGateStep::PromptOver);
        assert_eq!(intl_gate_step(prompt, t0 + CARRIER_PROMPT_SPAN, true, None), IntlGateStep::PromptOver);
        assert_eq!(CARRIER_PROMPT_SPAN, Duration::from_millis(7_500));
    }

    #[test]
    fn failed_rotations_back_off_without_using_up_the_cap() {
        let secs: Vec<u64> = (1..=6).map(|n| rotate_retry_after(n).as_secs()).collect();
        assert_eq!(secs, [5, 10, 20, 40, 40, 40]);
        assert_eq!(LIVE_ROTATE_MAX, 6);
    }

    #[test]
    fn a_flagged_call_gets_a_second_look_before_the_ai_stalls() {
        use crate::stall_review::Verdict::*;
        let mut r = StallReview::default();
        // Not flagged: nothing to look at.
        assert!(!r.should_ask(false, 2));
        // Flagged: one review at a time, the AI talking on meanwhile.
        assert!(r.should_ask(true, 3));
        assert!(!r.should_ask(true, 4));
        // "Normal": no stalling, and another look only 2 points later.
        assert_eq!(r.verdict(Normal, 3), StallVerdictStep::Carry);
        assert!(!r.should_ask(true, 4));
        assert!(r.should_ask(true, 5));
        assert_eq!(r.verdict(ScamOrSales, 5), StallVerdictStep::Engage);
        // At most three looks a call.
        let mut r = StallReview::default();
        for s in [3, 5, 7] {
            assert!(r.should_ask(true, s));
            assert_eq!(r.verdict(Normal, s), StallVerdictStep::Carry);
        }
        assert!(!r.should_ask(true, 30));
        // A recording / voice menu.
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.verdict(IvrRecording, 3), StallVerdictStep::Ivr);
        // The stalling starts at most this late (no answer in time = scam / sales).
        assert_eq!(crate::stall_review::REVIEW_TIMEOUT, Duration::from_millis(1_500));
    }

    #[test]
    fn a_voice_menu_is_hung_up_on() {
        // The menu goes on: at the latest 20 s after it was recognised.
        assert!(!ivr_hangs_up(Duration::from_secs(5), None));
        assert!(ivr_hangs_up(IVR_MAX, None));
        // The menu stopped: 4 s of quiet line.
        assert!(!ivr_hangs_up(Duration::from_secs(5), Some(Duration::from_secs(3))));
        assert!(ivr_hangs_up(Duration::from_secs(5), Some(IVR_QUIET_END)));
        assert_eq!((IVR_QUIET_END, IVR_MAX), (Duration::from_secs(4), Duration::from_secs(20)));
        assert_eq!(SessionEndReason::IvrHangup.as_str(), "ivr_hangup");
        assert!(SessionEndReason::IvrHangup.should_play_goodbye());
        // What makes the call a menu, and the AI told to keep quiet to it.
        assert!(crate::stall_review::ivr_cue("查帳單請按1，轉接專人請按9"));
        let k: Value = serde_json::from_str(&persona_ivr_client_content()).unwrap();
        let t = k.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("不要對它講話") && t.contains("不是對方說的話"), "{t}");
    }

    #[test]
    fn one_keep_nudge_per_stall_turn() {
        use StallKeepAtTurnEnd::*;
        // The farewell cut mid-reply: its nudge goes at the turn's end — once, even
        // though the leftover words (「…掰掰。」) also end the turn on a closing.
        assert_eq!(stall_keep_at_turn_end(true, true, true), Some(FarewellCut));
        // The cut's nudge already went (the farewell came after generation): nothing more.
        assert_eq!(stall_keep_at_turn_end(false, true, true), None);
        // A closing the streaming check missed.
        assert_eq!(stall_keep_at_turn_end(false, false, true), Some(AiSaidBye));
        assert_eq!(stall_keep_at_turn_end(false, false, false), None);
    }

    #[test]
    fn our_hangup_lets_the_phone_finish_the_last_words() {
        assert_eq!(hangup_playout_wait(Duration::ZERO), Duration::ZERO);
        assert_eq!(hangup_playout_wait(Duration::from_millis(600)), Duration::from_millis(600));
        assert_eq!(hangup_playout_wait(Duration::from_secs(4)), HANGUP_PLAYOUT_MAX);
        assert_eq!(HANGUP_PLAYOUT_MAX, Duration::from_millis(1_500));
        // The normal-call cap and its 4:35 / 4:45 wrap-up stay as they are.
        assert_eq!(talk_limits(true, 5 * 60), (275, 285, 300));
        // A caller on hold gets a longer quiet line.
        assert_eq!((PERSONA_HOLD_NUDGE_SECS, PERSONA_HOLD_END_SECS), (45, 90));
        assert!(PERSONA_IDLE_NUDGE_SECS < PERSONA_HOLD_NUDGE_SECS && PERSONA_IDLE_END_SECS < PERSONA_HOLD_END_SECS);
    }

    #[tokio::test]
    async fn the_hangup_waits_for_what_the_phone_still_plays() {
        let h = DownlinkPlayHead::new();
        let cancel = AtomicBool::new(false);
        h.set_speech_tail(tokio::time::Instant::now() + Duration::from_millis(250));
        let waited = let_the_phone_finish(&h, 16_000, &cancel).await;
        assert!(waited >= Duration::from_millis(200) && waited < HANGUP_PLAYOUT_MAX, "{waited:?}");
        // Nothing left: no wait. The caller hung up: no wait either.
        assert!(let_the_phone_finish(&h, 16_000, &cancel).await < Duration::from_millis(50));
        h.set_speech_tail(tokio::time::Instant::now() + Duration::from_secs(5));
        cancel.store(true, Ordering::Relaxed);
        assert!(let_the_phone_finish(&h, 16_000, &cancel).await < Duration::from_millis(50));
    }

    #[test]
    fn every_call_may_switch_to_the_callers_language() {
        // A foreign caller on a local or hidden number got Chinese: the note goes into
        // every session's prompt, not only calls that open in another language.
        let p = persona_system_prompt("", false, "", StallPrompt::Off);
        let s = session_prompt(&p);
        assert!(s.contains("【對方講別的語言】") && s.contains("問你能不能講某種語言"), "{s}");
        assert_eq!(session_prompt(&s), s, "once");
        let base = default_persona_prompt();
        assert!(!base.contains("不要講英文") && base.contains("對方講中文時不要夾英文"));
        assert!(base.contains(PERSONA_LANGUAGE_LINE));
        // A custom prompt keeps its words; the note goes before the fixed guard.
        let c = session_prompt(&persona_system_prompt("你是我，只講中文。", false, "", StallPrompt::Off));
        assert!(c.starts_with("你是我，只講中文。") && c.contains("【對方講別的語言】"));
        assert!(c.ends_with(&PERSONA_GUARD.replace("{owner}", "機主")));
        // Asked for personal details or a code: none given, none made up.
        assert!(c.contains("身分證字號") && c.contains("驗證碼") && c.contains("不要給、也不要編"));
        assert!(!c.contains("我這邊先幫他接"));
    }

    #[test]
    fn preroll_keeps_newest_bytes_sample_aligned() {
        let mut buf = Vec::new();
        let cap = 10;
        push_preroll(&mut buf, &[1, 2, 3, 4, 5, 6], cap);
        push_preroll(&mut buf, &[7, 8, 9, 10, 11, 12], cap);
        assert_eq!(buf, vec![3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        // Preroll covers the VAD confirm delay (≥ 4 × 60 ms frames + slack).
        assert!(LOCAL_PREROLL_MS >= 480);
    }

    struct PendingSink;

    impl futures_util::Sink<WsMessage> for PendingSink {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, _item: WsMessage) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Pending
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn local_vad_constants_quiet_pstn() {
        assert_eq!(LOCAL_VAD_RMS, 180.0);
        assert_eq!(LOCAL_EOU_SILENCE_MS, 700);
        assert!(
            LOCAL_EOU_SILENCE_MS > 320,
            "EOU silence must stay above 320ms fragment cut"
        );
        assert_eq!(LOCAL_MIN_TURN_VOICE_MS, 450);
        assert_eq!(LOCAL_PRE_ACTIVITY_HANGOVER_MS, 350);
        assert!(LOCAL_PRE_ACTIVITY_HANGOVER_MS > LOCAL_MIN_VOICE_MS);
        assert!(LOCAL_SHORT_TURN_EOU_SILENCE_MS > LOCAL_EOU_SILENCE_MS);
    }

    #[test]
    fn text_only_opening_leaves_the_transcript() {
        let t = Some(Arc::new(std::sync::Mutex::new(String::new())));
        let mark = 0;
        append_transcriptions(
            &json!({"serverContent": {"outputTranscription": {"text": "喂？"}}}),
            &t,
        );
        assert_eq!(truncate_transcript(&t, mark), "接聽：喂？");
        assert!(t.as_ref().unwrap().lock().unwrap().is_empty());
        // Re-kicked opening that spoke stays.
        append_transcriptions(
            &json!({"serverContent": {"outputTranscription": {"text": "喂？"}}}),
            &t,
        );
        assert_eq!(*t.as_ref().unwrap().lock().unwrap(), "接聽：喂？");
        // A mark past the end or inside a character is ignored.
        assert_eq!(truncate_transcript(&t, 999), "");
        assert_eq!(truncate_transcript(&t, 1), "");
        assert_eq!(*t.as_ref().unwrap().lock().unwrap(), "接聽：喂？");
        assert_eq!(truncate_transcript(&None, 0), "");
    }

    #[test]
    fn audio_detection_and_debug_redaction() {
        let with_audio = json!({"serverContent": {"modelTurn": {"parts": [
            {"text": "喂？"},
            {"inlineData": {"mimeType": "audio/pcm;rate=24000", "data": "AAAA"}}
        ]}}});
        assert!(server_msg_has_audio(&with_audio));
        let stt_only = json!({"serverContent": {"outputTranscription": {"text": "喂？"}}});
        assert!(!server_msg_has_audio(&stt_only));
        let big = json!({"a": ["x".repeat(500), "short"]});
        assert_eq!(redact_long_strings(&big), json!({"a": ["<500 chars>", "short"]}));
        assert!(BARGE_IN_VOICE_MS > LOCAL_MIN_VOICE_MS);
    }

    #[test]
    fn server_vad_hands_turn_detection_to_gemini() {
        let s = realtime_input_config(true);
        assert_eq!(s["automaticActivityDetection"]["disabled"], json!(false));
        assert_eq!(
            s["automaticActivityDetection"]["silenceDurationMs"],
            json!(SERVER_VAD_SILENCE_MS)
        );
        let m = realtime_input_config(false);
        assert_eq!(m["automaticActivityDetection"], json!({ "disabled": true }));
        for c in [&s, &m] {
            assert_eq!(c["activityHandling"], json!("START_OF_ACTIVITY_INTERRUPTS"));
            assert_eq!(c["turnCoverage"], json!("TURN_INCLUDES_ONLY_ACTIVITY"));
        }
    }

    #[test]
    fn lone_short_word_ends_the_turn_after_a_longer_pause() {
        // The caller's 「喂？」 opened the activity with
        // 240 ms of voice (60 ms frames) and the line went quiet.
        let (mut voiced_ms, mut silence_ms) = (240u32, 0u32);
        let mut ended_at = None;
        for _ in 0..40 {
            tick_local_vad(false, true, 60, &mut voiced_ms, &mut silence_ms);
            if local_turn_complete(voiced_ms, silence_ms) {
                ended_at = Some(silence_ms);
                break;
            }
        }
        assert_eq!(ended_at, Some(1_020), "a lone 「喂？」 must get an answer");
        // Not at the normal pause — a short start ("uh…") may still go on.
        assert!(!local_turn_complete(240, LOCAL_EOU_SILENCE_MS));
        // A phrase keeps the usual 700 ms end of turn.
        assert!(local_turn_complete(LOCAL_MIN_TURN_VOICE_MS, LOCAL_EOU_SILENCE_MS));
        assert!(!local_turn_complete(LOCAL_MIN_TURN_VOICE_MS, LOCAL_EOU_SILENCE_MS - 1));
        // Below the activityStart minimum there is no turn to end.
        assert!(!local_turn_complete(LOCAL_MIN_VOICE_MS - 1, 5_000));
    }

    #[test]
    fn pre_activity_hangover_keeps_voiced_ms_across_brief_dips() {
        let mut voiced_ms = 0u32;
        let mut silence_ms = 0u32;
        let frame = 40u32;
        // 160ms voice, then 80ms sub-threshold (old path zeroed voiced_ms here).
        for _ in 0..4 {
            tick_local_vad(true, false, frame, &mut voiced_ms, &mut silence_ms);
        }
        assert_eq!(voiced_ms, 160);
        for _ in 0..2 {
            tick_local_vad(false, false, frame, &mut voiced_ms, &mut silence_ms);
        }
        assert_eq!(voiced_ms, 160, "brief dip must not reset pre-activity voiced_ms");
        for _ in 0..2 {
            tick_local_vad(true, false, frame, &mut voiced_ms, &mut silence_ms);
        }
        assert_eq!(voiced_ms, 240);
        assert!(voiced_ms >= LOCAL_MIN_VOICE_MS);

        // Continuous silence past hangover drops the accumulator.
        for _ in 0..9 {
            tick_local_vad(false, false, frame, &mut voiced_ms, &mut silence_ms);
        }
        assert_eq!(voiced_ms, 0);
        assert!(silence_ms >= LOCAL_PRE_ACTIVITY_HANGOVER_MS);
    }

    #[test]
    fn activity_open_silence_does_not_reset_voiced_ms() {
        let mut voiced_ms = 450u32;
        let mut silence_ms = 0u32;
        while silence_ms < LOCAL_EOU_SILENCE_MS {
            tick_local_vad(false, true, 40, &mut voiced_ms, &mut silence_ms);
        }
        assert_eq!(voiced_ms, 450);
        assert!(silence_ms >= LOCAL_EOU_SILENCE_MS);
        assert!(voiced_ms >= LOCAL_MIN_TURN_VOICE_MS);
    }

    #[test]
    fn persona_closing_phrases() {
        assert!(looks_like_persona_closing("好，OK，那先這樣，掰掰。"));
        assert!(looks_like_persona_closing("好，了解，掰。"));
        assert!(looks_like_persona_closing("我等一下還有事要忙，先不聊了"));
        assert!(looks_like_persona_closing("好，再見"));
        assert!(!looks_like_persona_closing("喂？"));
        assert!(!looks_like_persona_closing("好，我知道了，我晚點處理。"));
        assert!(!looks_like_persona_closing("嗯，你說"));
    }

    #[test]
    fn persona_prompt_is_owner_with_fixed_guard() {
        let p = persona_system_prompt("", false, "", StallPrompt::Off);
        assert!(p.contains("你就是機主本人") && !p.contains("{owner}"));
        assert!(p.contains("【底線（固定）】"));
        assert!(!p.contains("AI 助理"));
        // Custom prompt replaces the body but never the guard.
        let c = persona_system_prompt("你是我，講話很隨性。", false, "", StallPrompt::Off);
        assert!(c.starts_with("你是我"));
        assert!(c.contains("不要說謊"));
        // Desk note is folded in softly, not as the forced-first-line block.
        let s = persona_system_prompt("", true, "快遞放管理室", StallPrompt::Off);
        assert!(s.contains("快遞放管理室"));
        assert!(!s.contains(OWNER_STATUS_BEGIN));
        assert!(extract_owner_status(&s).is_empty());
    }

    #[test]
    fn stall_prompt_replaces_sales_reject_line() {
        assert!(default_persona_prompt().contains(PERSONA_SALES_REJECT_LINE));
        let off = persona_system_prompt("", false, "", StallPrompt::Off);
        assert!(off.contains(PERSONA_SALES_REJECT_LINE));
        assert!(!off.contains("陪他繞"));
        // Every call starts with the short line, not the ~900-char block.
        let first = persona_system_prompt("", false, "", StallPrompt::ListenFirst);
        assert!(!first.contains(PERSONA_SALES_REJECT_LINE) && first.contains(PERSONA_SALES_LISTEN_LINE));
        assert!(!first.contains("陪他繞"));
        assert!(first.chars().count() < off.chars().count() + 200);
        let on = persona_system_prompt("", false, "", StallPrompt::Full);
        assert!(!on.contains(PERSONA_SALES_REJECT_LINE) && !on.contains(PERSONA_SALES_LISTEN_LINE));
        assert!(on.contains("陪他繞") && on.contains("理賠") && on.contains("不能給"));
        // The fixed guard still comes last.
        let guard = PERSONA_GUARD.replace("{owner}", "機主");
        assert!(on.ends_with(&guard) && first.ends_with(&guard));
        // A custom prompt without the reject line still gets the block / line.
        let c = persona_system_prompt("你是我。", false, "", StallPrompt::Full);
        assert!(c.starts_with(PERSONA_STALL_HEAD) && c.contains("你是我。") && c.contains("陪他繞"));
        let c = persona_system_prompt("你是我。", false, "", StallPrompt::ListenFirst);
        assert!(c.contains(PERSONA_SALES_LISTEN_LINE));
        // The rotated session swaps only the system instruction.
        let setup = json!({"setup": {"model": "m", "systemInstruction": {"parts": [{"text": "a"}]}}});
        let s2 = setup_with_system_prompt(&setup, "b");
        assert_eq!(s2.pointer("/setup/systemInstruction/parts/0/text"), Some(&json!("b")));
        assert_eq!(s2.pointer("/setup/model"), Some(&json!("m")));
    }

    #[test]
    fn stall_never_lets_the_caller_go() {
        // A sales / scam caller saying bye is not let go.
        let on = persona_system_prompt("", false, "", StallPrompt::Full);
        assert!(on.contains("欸等一下，我還有一個問題") && on.contains("掛不了電話"));
        for gone in ["他要掛就讓他掛", "除非他自己要掛", "一句「喔不用了，掰掰」"] {
            assert!(!on.contains(gone), "{gone}");
        }
        // Normal calls still end on the caller's bye.
        assert!(!persona_system_prompt("", false, "", StallPrompt::ListenFirst).contains("掛不了電話"));
        let took_back = persona_stall_keep_client_content(true);
        let unanswered = persona_stall_keep_client_content(false);
        assert!(took_back.contains("反悔") && unanswered.contains("對方想掛電話了"));
        for n in [&took_back, &unanswered] {
            assert!(n.contains("不是對方說的話") && n.contains("不要說掰掰"));
        }
        // What makes the model's turn count as a farewell there.
        assert!(turn_ends_with_closing("好，那先這樣，掰掰。"));
        assert!(!turn_ends_with_closing("欸等一下，你剛剛說的利率是固定的嗎？"));
        // The streaming transcript is cut the moment it reaches a farewell
        // (otherwise 「好，那我再想一下，掰掰。」 reaches the caller and they leave).
        for bye in ["好，那我再想一下，掰", "好，那我再想一下，掰掰。", "好啦再見喔", "OK bye"] {
            assert!(ends_with_farewell(bye), "{bye}");
        }
        for not_yet in ["那我先這樣問你喔", "你說的分期是哪一張卡？", "去拜拜的時候", ""] {
            assert!(!ends_with_farewell(not_yet), "{not_yet}");
        }
    }

    #[test]
    fn stall_prompt_drops_the_lines_that_let_callers_go() {
        // Each normal-call line the stall prompt swaps out is in the default prompt…
        let base = default_persona_prompt();
        for line in [PERSONA_ACK_LINE, PERSONA_HOLD_OFF_LINE, PERSONA_END_BLOCK, PERSONA_SALES_REJECT_LINE] {
            assert!(base.contains(line), "{line}");
        }
        // …and gone once the call is judged: no 「我再想一下／晚點再確認／好，我知道了」
        // brush-offs, no 掰掰 when they wrap up; the stall block leads and ends it.
        let on = persona_system_prompt("", false, "", StallPrompt::Full);
        for line in [PERSONA_ACK_LINE, PERSONA_HOLD_OFF_LINE, PERSONA_END_BLOCK] {
            assert!(!on.contains(line), "{line}");
        }
        assert!(on.starts_with(PERSONA_STALL_HEAD));
        for line in [PERSONA_ACK_STALL, PERSONA_HOLD_OFF_STALL, PERSONA_END_STALL] {
            assert!(on.contains(line), "{line}");
        }
        assert!(on.contains("把話丟回給他") && on.contains("催繳"));
        // No checking the caller
        // out (name / extension / case number / proof), which makes him hang up —
        // interested, slow, driving with questions he has to talk long about.
        for scary in ["分機幾號", "案號多少", "我要怎麼確認你真的是"] {
            assert!(!on.contains(scary), "{scary}");
        }
        assert!(on.contains("不要懷疑他") && on.contains("故意聽錯、算錯"));
        // More freedom for the model: a box of
        // moves the model mixes itself, not one reply shape for every turn.
        assert!(on.contains("自由發揮") && on.contains("不要每句都同一個套路"));
        for formula in ["每次回話：先用半句接住", "用問句結尾，一次一個"] {
            assert!(!on.contains(formula), "{formula}");
        }
        let idle: Value = serde_json::from_str(&persona_stall_idle_nudge_client_content()).unwrap();
        let idle = idle.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(idle.contains("主動接話") && idle.contains("不要說掰掰"));
        assert!(PERSONA_STALL_IDLE_NUDGE_SECS < PERSONA_IDLE_NUDGE_SECS);
        // Normal calls keep them.
        let first = persona_system_prompt("", false, "", StallPrompt::ListenFirst);
        assert!(first.contains(PERSONA_END_BLOCK) && !first.contains(PERSONA_STALL_HEAD));
        // The rotated stall session's history says so too.
        let tx = "接聽：喂？\n對方：我安信託，分期通知你還沒處理\n接聽：我再想一下，晚點再確認";
        assert!(history_turns_with_note(tx, 2_500, true)[0].1.contains("陪他繞"));
        assert!(!transcript_history_turns(tx, 2_500)[0].1.contains("陪他繞"));
    }

    #[test]
    fn stall_keywords_score_sales_not_friends() {
        let mut hits = Vec::new();
        let s = stall_score_add("您好，我是安心人壽的業務，想跟您介紹一個新的醫療險方案", &mut hits);
        assert!(s >= STALL_SCORE_THRESHOLD, "{s} {hits:?}");
        assert!(stall_engages(s, &hits));
        // Simplified ASR output is normalized first.
        for t in [
            "我们有一个股票群组，老师每天报明牌",
            "這裡是台北地檢署，你的帳戶涉及洗錢",
            "您好，這邊是銀行信貸專員，目前有低利專案",
        ] {
            let mut hits = Vec::new();
            let s = stall_score_add(t, &mut hits);
            assert!(stall_engages(s, &hits), "{t} {s} {hits:?}");
        }
        // Normal calls stay below the threshold.
        for t in [
            "黑貓宅急便，明天下午送包裹，請問有人在家嗎",
            "欸我小明啦，明天中午要不要一起吃飯",
            "我是陳太太，提醒你月底前要繳水費喔",
        ] {
            let mut hits = Vec::new();
            assert!(stall_score_add(t, &mut hits) < STALL_SCORE_THRESHOLD, "{t} {hits:?}");
        }
        // Repeats count once.
        let mut hits = Vec::new();
        assert_eq!(stall_score_add("方案", &mut hits), 1);
        assert_eq!(stall_score_add("方案 方案", &mut hits), 0);
        // A fake debt collector: judged on the 2nd line, not at
        // 75 s — STT wrote 帳戶 as 賬戶.
        let mut hits = Vec::new();
        let mut s = stall_score_add("餵,哎,你好,我安信託。我想問一下,因為之前有發給你那個分期通知啊。你這邊還沒處理。", &mut hits);
        assert!(!stall_engages(s, &hits), "{s} {hits:?}");
        s += stall_score_add("繳款有中斷一段時間欸。沒有,我是想說你安信的賬戶有在。流動齁。", &mut hits);
        assert!(stall_engages(s, &hits), "{s} {hits:?}");
        assert!(hits.contains(&"帳戶") && hits.contains(&"繳款"));
    }

    #[test]
    fn stall_needs_a_clear_pitch_and_skips_the_owners_own_matters() {
        // Weak words only: how a bank opens a real follow-up call.
        let mut hits = Vec::new();
        let s = stall_score_add("您好，這邊是銀行客服，想耽誤您一點時間，跟您確認一下帳戶的狀況", &mut hits);
        assert!(s >= STALL_SCORE_THRESHOLD && !stall_engages(s, &hits), "{s} {hits:?}");
        // A bank calling back about the owner's own report.
        let bank = "不好意思，我這邊有接獲您有反應上個月在分行臨櫃辦理的事情，\
那我們有請分行去調查，分行也回覆調查的結果了";
        assert!(looks_like_own_matter(bank));
        assert!(looks_like_own_matter("您之前反映的信用卡分期問題，我們調查結果出來了"));
        assert!(looks_like_own_matter("关于您的申诉"));
        for t in [
            "您好，我是安心人壽的業務，想跟您介紹一個新的醫療險方案",
            "這裡是台北地檢署，你的帳戶涉及洗錢",
            "黑貓宅急便，明天下午送包裹，請問有人在家嗎",
            "他反應很快",
        ] {
            assert!(!looks_like_own_matter(t), "{t}");
        }
    }

    #[test]
    fn software_demo_pitches_count_and_are_you_free_does_not() {
        // The probe's CRM pitch only scored "free" + "offer" (2) before.
        for t in [
            "Hi, this is Mia from Brightdesk. We have a special CRM platform for small teams, could I book a quick 15-minute demo on Thursday? It's free, no strings attached, and there's an offer this month.",
            "Hi, I'm calling from Brightdesk, our platform helps teams with scheduling software, can I show you a demo?",
            "我們這套軟體現在有免費試用，可以幫您安排一個展示",
        ] {
            let mut hits = Vec::new();
            let s = stall_score_add(t, &mut hits);
            assert!(stall_engages(s, &hits), "{t} {s} {hits:?}");
        }
        // Friends asking whether the owner is free (E1/E3/E8 on the probe).
        for t in [
            "Hey, it's Jonas! Are you free tonight? Dinner at eight?",
            "Hi, it's Sam, are you free on Wednesday evening around 7 for a drink?",
            "Feel free to call me back whenever you're free.",
        ] {
            let mut hits = Vec::new();
            assert_eq!(stall_score_add(t, &mut hits), 0, "{t} {hits:?}");
        }
        // After the call: the whole caller side, the same rule.
        let pitch = "接聽：Hello?\n對方：Hi, this is Mia from Brightdesk, our platform is CRM software.\n接聽：Oh?\n對方：Could I book a quick 15-minute demo on Thursday? It's free.";
        assert!(dialogue_is_sales(pitch));
        assert!(!dialogue_is_sales("接聽：Hello?\n對方：Hey, it's Jonas, are you free tonight?\n接聽：Should be, I'll text you."));
        // The AI's own words do not count.
        assert!(!dialogue_is_sales("接聽：我們有免費試用的軟體 demo\n對方：嗯，明天下午三點可以嗎"));
    }

    #[test]
    fn the_first_reply_after_the_greeting_is_never_taken_for_a_repeat() {
        // 「喂？」 went out (12 chunks, 9600 bytes), no turnComplete counted for it;
        // the caller spoke and ended their turn; the model calls
        // propose_appointment before any audio of its reply.
        assert!(!spoke_since_caller(12, 0, 9_600, 9_600));
        // It had begun the reply (audio after the caller's end of turn).
        assert!(spoke_since_caller(20, 0, 16_000, 9_600));
        // Audio since the caller, but none in this turn (an earlier turn's).
        assert!(!spoke_since_caller(20, 20, 16_000, 9_600));
    }

    #[test]
    fn a_repeat_after_propose_is_dropped_only_when_nobody_asked_for_it() {
        let now = tokio::time::Instant::now();
        // propose_appointment called while turn 4 was playing (3 turns done, 2 caller turns).
        let call = CallTurn { turns_done: 3, caller_eous: 2 };
        let mut g = RepeatGuard::default();
        g.arm(call, 3, true, now);
        // The rest of the calling turn plays.
        assert!(!g.drops(true, 3, true, 2, false, now));
        // Its turnComplete; then the model opens a turn on its own: dropped to its end.
        assert!(g.drops(true, 4, false, 2, false, now));
        assert!(g.drops(false, 4, false, 2, false, now) && g.is_dropping(), "its transcript too");
        assert!(g.turn_complete());
        // Only that one turn.
        assert!(!g.drops(true, 5, false, 2, false, now) && !g.turn_complete());
        // The caller spoke (a caller turn ended, or they are talking): the reply is theirs.
        g.arm(call, 4, false, now);
        assert!(!g.drops(true, 4, false, 3, false, now));
        assert!(!g.drops(true, 4, false, 2, false, now), "disarmed for good");
        g.arm(call, 4, false, now);
        assert!(!g.drops(true, 4, false, 2, true, now));
        // A kick of ours (idle nudge, wrap-up, rotation, sales verdict) or caller words clear it.
        g.arm(call, 4, false, now);
        g.clear();
        assert!(!g.drops(true, 4, false, 2, false, now));
        // Long after the answer it is no repeat.
        g.arm(call, 4, false, now);
        assert!(!g.drops(true, 4, false, 2, false, now + REPEAT_GUARD_WINDOW));
        // The answer came late, while a later turn was already playing: nothing armed.
        g.arm(call, 4, true, now);
        assert!(!g.drops(true, 5, false, 2, false, now));
        // The caller cuts into the dropped turn: what follows answers them.
        g.arm(call, 4, false, now);
        assert!(g.drops(true, 4, false, 2, false, now));
        g.interrupted();
        assert!(!g.is_dropping() && !g.drops(true, 4, false, 2, false, now));
        // Only a silent propose answer arms it (an error or "not booked" is meant to be said).
        use crate::appointments::{answer_is_silent, CHECK, PROPOSE};
        assert!(answer_is_silent(PROPOSE, &json!({"status": "pending_owner_confirmation"})));
        assert!(answer_is_silent(PROPOSE, &json!({"status": "booked"})));
        assert!(!answer_is_silent(PROPOSE, &json!({"status": "pending_owner_confirmation", "not_booked": true})));
        assert!(!answer_is_silent(PROPOSE, &json!({"error": "weekday_mismatch", "date_is": "Thursday"})));
        assert!(!answer_is_silent(CHECK, &json!({"free": true})));
    }

    #[test]
    fn a_short_caller_word_is_never_taken_for_nobody_asking() {
        let now = tokio::time::Instant::now();
        let call = CallTurn { turns_done: 3, caller_eous: 2 };
        let mut g = RepeatGuard::default();
        // 「嗯」 opened no local turn, but its transcript came before the reply: nothing dropped.
        g.arm(call, 4, false, now);
        g.caller_words();
        assert!(!g.drops(true, 4, false, 2, false, now));
        assert!(!g.reply_owed());
        // Its transcript came only once the reply was being dropped: the drop ends
        // there (the rest of the reply plays) and the reply is owed if nothing more
        // of it is heard.
        g.arm(call, 4, false, now);
        assert!(g.drops(true, 4, false, 2, false, now));
        g.caller_words();
        assert!(!g.is_dropping() && g.reply_owed());
        assert!(!g.drops(true, 4, false, 2, false, now), "the rest of that turn plays");
        assert!(!g.turn_complete());
        assert!(!g.reply_owed() && !g.is_dropping());
        // A cut-in ends it all.
        g.arm(call, 4, false, now);
        assert!(g.drops(true, 4, false, 2, false, now));
        g.caller_words();
        g.interrupted();
        assert!(!g.reply_owed() && !g.is_dropping());
    }

    #[test]
    fn a_late_transcript_of_the_calling_turn_never_starts_a_drop() {
        // HANDOFF: after propose_appointment the AI's answer to the caller's next
        // sentence went missing (~2.5 s with no transcript). The calling turn's last
        // words came as a transcript-only message after its turnComplete, started a
        // drop, and the drop outlived the caller's next sentence.
        let now = tokio::time::Instant::now();
        let call = CallTurn { turns_done: 3, caller_eous: 2 };
        let mut g = RepeatGuard::default();
        g.arm(call, 3, true, now);
        // turnComplete of the calling turn (4 done), then its late transcript: kept.
        assert!(!g.drops(false, 4, false, 2, false, now));
        assert!(!g.is_dropping());
        // Still armed: an unasked turn with audio right after is the repeat.
        assert!(g.drops(true, 4, false, 2, false, now));
        assert!(g.turn_complete());
        // The caller's next sentence (activityStart, then its transcript) and the
        // model's answer to it: played.
        g.arm(call, 4, false, now);
        g.caller_started();
        g.caller_words();
        assert!(!g.drops(true, 4, false, 3, false, now) && !g.reply_owed());
        // A drop under way ends the moment the caller starts talking: the answer
        // to them that follows is heard.
        g.arm(call, 4, false, now);
        assert!(g.drops(true, 4, false, 2, false, now));
        g.caller_started();
        assert!(!g.is_dropping() && !g.drops(true, 4, false, 2, false, now));
        assert!(!g.reply_owed(), "a new turn of theirs gets its own answer");
    }

    #[test]
    fn latin_keywords_count_only_as_whole_words() {
        let mut hits = Vec::new();
        stall_score_add("we help teams streamline things online before the deadline", &mut hits);
        assert!(!hits.contains(&"line"), "{hits:?}");
        let mut hits = Vec::new();
        stall_score_add("你加我的LINE，我拉你進群組", &mut hits);
        assert!(hits.contains(&"line"), "{hits:?}");
        assert!(has_keyword("add my line id", "line"));
        assert!(!has_keyword("lines", "line"));
    }

    #[test]
    fn a_word_inside_a_longer_hit_counts_once() {
        // 免費 1 + 試用 1 + 免費試用 2 made 「免費試用」 alone a sales call.
        let mut hits = Vec::new();
        let s = stall_score_add("我們現在有免費試用喔", &mut hits);
        assert_eq!((s, hits.clone()), (2, vec!["免費試用"]));
        assert!(!stall_engages(s, &hits));
        // Later, the short word alone adds nothing either.
        assert_eq!(stall_score_add("真的免費嗎", &mut hits), 0);
        let mut hits = Vec::new();
        assert_eq!(stall_score_add("I'm calling about life insurance", &mut hits), 2, "{hits:?}");
        let mut hits = Vec::new();
        assert_eq!(stall_score_add("無料体験はいかがですか", &mut hits), 2, "{hits:?}");
        // Separate words still add up.
        let mut hits = Vec::new();
        assert!(stall_engages(stall_score_add("這套軟體有免費試用", &mut hits), &hits), "{hits:?}");
    }

    #[test]
    fn replies_about_the_owners_own_matter_are_no_pitch() {
        for t in [
            "您好，這邊是中信銀行，您申請的信用卡已經核卡了，想跟您確認寄送地址",
            "Hi, it's Dana from Acme support, following up on the issue you had with our router",
        ] {
            assert!(looks_like_own_matter(t), "{t}");
        }
        // "Your order" / "your account" open many scams: they do not count.
        assert!(!looks_like_own_matter("There is a problem with your order, we need to refund you"));
        assert!(dialogue_own_matter("接聽：喂？\n對方：您申請的信用卡額度調整好了，分期的部分也可以跟您說明"));
        assert!(!dialogue_is_sales("接聽：喂？\n對方：您申請的信用卡額度調整好了，分期的部分也可以跟您說明"));
        assert!(!dialogue_own_matter("接聽：您申請的那個我晚點看\n對方：我們有免費試用的軟體"));
    }

    #[test]
    fn a_dropped_repeat_is_no_turn_for_the_watchdogs_and_leaves_no_transcript() {
        // The dropped turn: audio not played (0 chunks), transcript stripped.
        let said = json!({"serverContent": {"outputTranscription": {"text": "我晚點看一下行程再跟你確認。"}}});
        let t = Some(Arc::new(std::sync::Mutex::new(String::new())));
        append_transcriptions(&without_output_transcription(&said), &t);
        assert!(t.as_ref().unwrap().lock().unwrap().is_empty());
        // Its turnComplete counts as quiet: no transcript-only re-kick, no closing check.
        let dropped_turn = true;
        let quiet = dropped_turn || tool_turn_quiet(0, None, false);
        assert!(!is_transcript_only_turn(true && !quiet, 0, 0, "我晚點看一下行程再跟你確認。"));
        // Without the guard the same turn would be read as a text-only opening.
        assert!(is_transcript_only_turn(true, 0, 0, "我晚點看一下行程再跟你確認。"));
    }

    #[test]
    fn korean_and_accented_words_keep_their_spaces() {
        assert_eq!(tidy_cjk_spaces("민준아, 나 지훈인데."), "민준아, 나 지훈인데.");
        assert_eq!(tidy_cjk_spaces("這 是 國 際 電 話"), "這是國際電話");
        let t = Some(Arc::new(std::sync::Mutex::new(String::new())));
        for piece in ["민준아, 나", " 지훈인데.", " 오늘 저녁 8시쯤에", "괜찮아?"] {
            append_transcriptions(&json!({"serverContent": {"inputTranscription": {"text": piece}}}), &t);
        }
        for piece in ["Sí, está", " bien.", " 喂？", "好"] {
            append_transcriptions(&json!({"serverContent": {"outputTranscription": {"text": piece}}}), &t);
        }
        let merged = merge_dialogue_fragments(&t.as_ref().unwrap().lock().unwrap());
        assert_eq!(merged, "對方：민준아, 나 지훈인데. 오늘 저녁 8시쯤에괜찮아?\n接聽：Sí, está bien.喂？好");
        // Chinese still joins directly; ASCII words still get their space.
        assert_eq!(merge_dialogue_fragments("對方：我想\n對方：預約\n接聽：Hallo, ist\n接聽：jemand"), "對方：我想預約\n接聽：Hallo, ist jemand");
    }

    #[test]
    fn the_web_record_labels_speakers_in_the_agents_language() {
        use crate::locale::Lang;
        let d = "接聽：Hello?\n對方：Hi, it's Jonas.";
        assert_eq!(localize_dialogue(d, Lang::En), "AI: Hello?\nCaller: Hi, it's Jonas.");
        assert_eq!(localize_dialogue(d, Lang::ZhTw), d);
        assert_eq!(localize_dialogue(d, Lang::De), "KI: Hello?\nAnrufer: Hi, it's Jonas.");
        assert_eq!(localize_dialogue(d, Lang::Fr), "IA : Hello?\nAppelant : Hi, it's Jonas.");
        assert_eq!(localize_dialogue("接聽：喂？", Lang::ZhCn), "接听：喂？");
        for l in crate::locale::ALL {
            let p = crate::locale::phrases(l);
            assert!(!p.d_caller.is_empty() && !p.d_ai.is_empty() && p.d_caller != p.d_ai, "{l:?}");
        }
    }

    #[test]
    fn transcript_only_turn_after_rotation_is_asked_again_once() {
        use PostRotateTurn::*;
        assert_eq!(post_rotate_turn(true, "喂？還在嗎？", false, false), Spoke);
        // First turn on the rotated socket: STT but 0 audio.
        assert_eq!(post_rotate_turn(false, "喂？還在嗎？", false, false), Rekick);
        assert_eq!(post_rotate_turn(false, "喂？還在嗎？", true, false), GiveUp);
        // Deliberately silent turn (carrier prompt) or caller already talking.
        assert_eq!(post_rotate_turn(false, "<no speech detected>", false, false), GiveUp);
        assert_eq!(post_rotate_turn(false, "", false, false), GiveUp);
        assert_eq!(post_rotate_turn(false, "好，掰掰。", false, true), GiveUp);
        let k: Value = serde_json::from_str(&persona_repeat_line_client_content()).unwrap();
        let t = k.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("再自然地講一次") && t.contains("不是對方說的話"));
    }

    #[test]
    fn talk_limits_have_no_two_minute_clock() {
        // Default 30-minute safety cap: wrap-up 25 s / 15 s before it.
        assert_eq!(talk_limits(true, 30 * 60), (30 * 60 - 25, 30 * 60 - 15, 30 * 60));
        assert_eq!(talk_limits(true, 3 * 60), (155, 165, 180));
        // Normal-call default 5 min: wrap-up from 4:35 at a pause, forced at 4:45.
        assert_eq!(talk_limits(true, 5 * 60), (275, 285, 300));
        // A bogus tiny cap still leaves room for the wrap-up line.
        let (w, f, m) = talk_limits(true, 10);
        assert!(w < f && f < m && w >= 60, "{w} {f} {m}");
        // Assistant (relay) mode keeps its old cap.
        assert_eq!(talk_limits(false, 30 * 60).2, LIVE_MAX_SECS);
    }

    #[test]
    fn persona_prompt_hears_matters_out() {
        let base = default_persona_prompt();
        // Deflection replies came from these example lines.
        for bad in ["我現在在外面，等一下回你", "晚點回你好不好", "晚點回你」"] {
            assert!(!base.contains(bad), "{bad}");
        }
        assert!(base.contains("【對方有事要說明】") && base.contains("你現在說就好"));
        assert!(base.contains("沒有時間限制"));
        // A 「好，我知道了，…掰掰。」 right after the caller gave a phone
        // number, before the caller closed — wait for the caller to wrap up.
        assert!(!base.contains("或事情明顯講完了") && base.contains("要等對方表示要結束了"));
        for mode in [StallPrompt::Off, StallPrompt::ListenFirst, StallPrompt::Full] {
            assert!(persona_system_prompt("", false, "", mode).contains("【對方有事要說明】"));
        }
        // The wrap-up nudge no longer speaks of two minutes.
        let w = persona_wrap_up_client_content();
        assert!(!w.contains("兩分鐘") && w.contains("時間上限"));
    }

    #[test]
    fn persona_prompt_never_puts_the_caller_on_hold() {
        let base = default_persona_prompt();
        // Hold lines ("wait, let me check", "let me look at the records") came from these word lists.
        for bad in ["「等一下、", "、我看一下、", "「這個我要看一下」"] {
            assert!(!base.contains(bad), "{bad}");
        }
        assert!(base.contains("【不要讓對方等】") && base.contains("我沒什麼印象耶"));
        // So did 「我想一下」 (from the word list) and 「我晚點自己再確認」
        // (from 「跟他說你之後自己再確認」), in nearly every reply.
        assert!(!base.contains("、我想一下、") && !base.contains("跟他說你之後自己再確認"));
        assert!(base.contains("一通電話最多講一次"));
        // A caller's 「好，好，謝謝」 while waiting for our answer must not get a goodbye.
        assert!(!base.contains("（說謝謝、") && base.contains("對方只說「好」「謝謝」不算要結束"));
        for mode in [StallPrompt::Off, StallPrompt::ListenFirst, StallPrompt::Full] {
            assert!(persona_system_prompt("", false, "", mode).contains("【不要讓對方等】"));
        }
    }

    #[test]
    fn persona_prompt_leaves_room_to_improvise() {
        // More freedom for the model, less formula.
        let base = default_persona_prompt();
        assert!(base.contains("自由發揮") && base.contains("長短看情況"));
        // The lines that made every reply the same shape are gone…
        for formula in [
            "一次講一句，最多兩句",
            "回話前先接住對方剛講的那件事",
            "把對方講的重點順口重複一下",
            "一句表示收到就好",
        ] {
            assert!(!base.contains(formula), "{formula}");
        }
        // …small talk is answered like a person, only what matters stays unsaid…
        assert!(base.contains("無關緊要的") && base.contains("不要說「我在家」「明天可以」"));
        // …and the hard lines stay.
        for kept in ["不要說「我查一下」", "一定要帶「掰掰」", "不要答應也不要硬拒絕", "不要客服腔"] {
            assert!(base.contains(kept), "{kept}");
        }
        // Not judged yet: no checking the caller out either.
        assert!(!PERSONA_SALES_LISTEN_LINE.contains("哪個單位"));
        // With a model playing the caller, the freer AI said 掰掰 to a building's front desk before
        // they were done and fixed a lunch date with a friend on the owner's behalf.
        assert!(base.contains("不要自己先說掰掰") && base.contains("不要把時間說定"));
        // Stalling: scammers stay while they think the money is coming — play along,
        // keep hitting snags, give nothing real.
        let on = persona_system_prompt("", false, "", StallPrompt::Full);
        assert!(on.contains("嘴上配合") && on.contains("真的錢一毛都不會動"));
        assert!(!on.contains("不要把時間說定"));
    }

    #[test]
    fn stall_hangup_needs_farewell_at_turn_end() {
        assert!(turn_ends_with_closing("好啦，那你改天再打給我，掰掰。"));
        assert!(turn_ends_with_closing("欸我有人按門鈴，先不聊了喔"));
        assert!(turn_ends_with_closing("好，再見！"));
        assert!(turn_ends_with_closing("OK bye"));
        assert!(!turn_ends_with_closing("那先這樣算的話，一個月要繳多少？"));
        assert!(!turn_ends_with_closing("我拜拜那天剛好有去，然後你說那個等待期是多久？"));
        assert!(!turn_ends_with_closing(""));
    }

    #[test]
    fn tx_window_cap_never_splits_cjk() {
        let mut w = String::new();
        for i in 0..200 {
            push_tx_window(&mut w, &format!("保費第{i}年一個月多少啊，"), 800);
        }
        assert!(w.len() <= 800 + 3, "{}", w.len());
        assert!(w.ends_with("第199年一個月多少啊，"));
        // Odd byte offsets: ASCII mixed with 3-byte chars.
        let mut w = String::new();
        for i in 0..300 {
            push_tx_window(&mut w, &format!("a{i}醫療險"), 100);
        }
        assert!(w.len() <= 103);
    }

    #[test]
    fn dialogue_merges_runs_and_drops_placeholders() {
        let tx = "接聽：喂？\n對方：這是國際電話,請注意小心詐騙。\n接聽：<no speech\n接聽：detected>\n對方：哈嘍,有人在嗎?\n接聽：有，在啊，\n接聽：請說。\n對方：Hallo, ist\n對方：jemand";
        assert_eq!(
            merge_dialogue_fragments(tx),
            "接聽：喂？\n對方：這是國際電話,請注意小心詐騙。\n對方：哈嘍,有人在嗎?\n接聽：有，在啊，請說。\n對方：Hallo, ist jemand"
        );
        // The rotated session never sees the placeholder either.
        let h = transcript_history_turns(tx, 2_500);
        assert!(h.iter().all(|(_, t)| !t.contains("no speech")));
    }

    #[test]
    fn stt_spaces_between_cjk_are_dropped() {
        // zh-TW language hint: 「這 是 國 際 電 話 , 請 注 意 …」.
        assert_eq!(tidy_cjk_spaces("這 是 國 際 電 話 , 請 注 意 小 心 詐 騙 。"), "這是國際電話,請注意小心詐騙。");
        assert_eq!(tidy_cjk_spaces("我這邊是保險公司 , 想跟你介紹"), "我這邊是保險公司,想跟你介紹");
        assert_eq!(tidy_cjk_spaces("Hallo, ist jemand"), "Hallo, ist jemand");
        assert_eq!(tidy_cjk_spaces("  掰 掰 "), "掰掰");
        let mut hits = Vec::new();
        assert!(stall_score_add(&tidy_cjk_spaces("保 險 方 案"), &mut hits) >= STALL_SCORE_THRESHOLD);
        assert!(looks_like_goodbye(&tidy_cjk_spaces("好 , 拜 拜")));
    }

    #[test]
    fn intl_caller_is_a_foreign_plus_number() {
        assert!(is_intl_caller("+12025550143"));
        assert!(is_intl_caller(" +1 202-555-0143 "));
        assert!(is_intl_caller("+8613800138000"));
        assert!(!is_intl_caller("+886912345678"));
        assert!(!is_intl_caller("0912345678"));
        assert!(!is_intl_caller("0223456789"));
        assert!(!is_intl_caller(""));
        assert!(!is_intl_caller("unknown"));
        assert!(!is_intl_caller("+12"));
    }

    #[test]
    fn opening_follows_the_country_of_a_caller_from_abroad() {
        use crate::locale::Lang;
        // Taiwan SIM, Traditional Chinese.
        assert_eq!(opening_lang_for("+12025550143", "886", Lang::ZhTw), Some(Lang::En));
        assert_eq!(opening_lang_for(" +1 202-555-0143 ", "886", Lang::ZhTw), Some(Lang::En));
        assert_eq!(opening_lang_for("+81312345678", "886", Lang::ZhTw), Some(Lang::Ja));
        assert_eq!(opening_lang_for("+5215512345678", "886", Lang::ZhTw), Some(Lang::Es));
        // Chinese either way: the setting's own variant.
        assert_eq!(opening_lang_for("+8613800138000", "886", Lang::ZhTw), None);
        assert_eq!(opening_lang_for("+886912345678", "86", Lang::ZhCn), None);
        assert_eq!(opening_lang_for("+85291234567", "86", Lang::ZhCn), None);
        assert_eq!(opening_lang_for("+12025550143", "86", Lang::ZhCn), Some(Lang::En));
        // Hong Kong speaks the setting's language; a local, hidden or unlisted number.
        assert_eq!(opening_lang_for("+85291234567", "886", Lang::ZhTw), None);
        assert_eq!(opening_lang_for("+886912345678", "886", Lang::ZhTw), None);
        assert_eq!(opening_lang_for("0912345678", "886", Lang::ZhTw), None);
        assert_eq!(opening_lang_for("", "886", Lang::ZhTw), None);
        assert_eq!(opening_lang_for("+84912345678", "886", Lang::ZhTw), None);
        // English on a UK SIM: +44 is a local call, +1 already speaks English.
        assert_eq!(opening_lang_for("+447700900123", "44", Lang::En), None);
        assert_eq!(opening_lang_for("+12025550143", "44", Lang::En), None);
        assert_eq!(opening_lang_for("+886912345678", "44", Lang::En), Some(Lang::ZhTw));
        // A US SIM: all of +1 is local.
        assert_eq!(opening_lang_for("+18765551234", "1", Lang::Es), None);
        // No country code set: nothing counts as abroad.
        assert_eq!(opening_lang_for("+81312345678", "", Lang::En), None);
    }

    #[test]
    fn call_languages_follow_the_number_and_the_sim() {
        use crate::locale::Lang;
        use std::collections::BTreeMap;
        let none: BTreeMap<String, String> = BTreeMap::new();
        let us_en: BTreeMap<String, String> = [("us".to_string(), "en".to_string())].into_iter().collect();
        let ca_fr: BTreeMap<String, String> = [("ca".to_string(), "fr".to_string())].into_iter().collect();
        fn hints(codes: &[&'static str]) -> Option<Vec<&'static str>> {
            Some(codes.to_vec())
        }
        let plan = |opening: Option<Lang>, caller_stt: Option<Vec<&'static str>>| CallLangPlan {
            opening,
            caller_stt,
        };
        let usual_tw = || hints(&["zh-TW", "en-US"]);
        // Taiwan SIM, Traditional Chinese. A US number (as reported in a real call that
        // spoke Thai and Indonesian): English opening, speech-to-text detects the language.
        assert_eq!(
            call_lang_plan("+12025550143", Some("tw"), Lang::ZhTw, "886", &none),
            plan(Some(Lang::En), None)
        );
        // The same number with the international prefix 00.
        assert_eq!(
            call_lang_plan("0012025550143", Some("tw"), Lang::ZhTw, "886", &none),
            plan(Some(Lang::En), None)
        );
        // Local numbers: no switch, the usual hints.
        assert_eq!(
            call_lang_plan("0912345678", Some("tw"), Lang::ZhTw, "886", &none),
            plan(None, usual_tw())
        );
        assert_eq!(
            call_lang_plan("+886223456789", Some("tw"), Lang::ZhTw, "886", &none),
            plan(None, usual_tw())
        );
        // A country whose language the AI does not speak (Thailand): the SIM's
        // language, and speech-to-text finds the language itself.
        assert_eq!(
            call_lang_plan("+66812345678", Some("tw"), Lang::ZhTw, "886", &none),
            plan(None, None)
        );
        // Chinese never switches variants; a +86 caller keeps the usual hints.
        assert_eq!(
            call_lang_plan("+8613800138000", Some("tw"), Lang::ZhTw, "886", &none),
            plan(None, usual_tw())
        );
        // A US SIM in the same phone, set to English: a local US number hears English…
        assert_eq!(
            call_lang_plan("(202) 555-0143", Some("us"), Lang::ZhTw, "886", &us_en),
            plan(Some(Lang::En), None)
        );
        assert_eq!(
            call_lang_plan("2025550143", Some("US"), Lang::ZhTw, "886", &us_en).opening,
            Some(Lang::En)
        );
        // …a Taiwanese number on it hears Chinese (its own country's language)…
        assert_eq!(
            call_lang_plan("+886912345678", Some("us"), Lang::ZhTw, "886", &us_en),
            plan(None, usual_tw())
        );
        // …and a Japanese number Japanese.
        assert_eq!(
            call_lang_plan("+81312345678", Some("us"), Lang::ZhTw, "886", &us_en),
            plan(Some(Lang::Ja), None)
        );
        // The US SIM without a language of its own: the setting's opening; the US
        // caller's language is detected.
        assert_eq!(
            call_lang_plan("2025550143", Some("us"), Lang::ZhTw, "886", &none),
            plan(None, None)
        );
        // Canadian SIM set to French, English setting, owner's code +1: a Montreal
        // number hears French; a US number is from another country than the SIM's
        // and hears English, which is the setting.
        assert_eq!(
            call_lang_plan("514 555 0123", Some("ca"), Lang::En, "1", &ca_fr),
            plan(Some(Lang::Fr), None)
        );
        assert_eq!(
            call_lang_plan("+12025550143", Some("ca"), Lang::En, "1", &ca_fr),
            plan(None, hints(&["en-US"]))
        );
        // Hidden numbers: the SIM's language, the usual hints.
        for hidden in ["", "unknown"] {
            assert_eq!(call_lang_plan(hidden, Some("tw"), Lang::ZhTw, "886", &none), plan(None, usual_tw()));
            assert_eq!(
                call_lang_plan(hidden, Some("us"), Lang::ZhTw, "886", &us_en),
                plan(Some(Lang::En), usual_tw())
            );
        }
        // SIM unknown (phone app older than 0.2.5): abroad = +… of another code than
        // the owner's, as before; national digits are the owner's country's.
        assert_eq!(
            call_lang_plan("+12025550143", None, Lang::ZhTw, "886", &us_en),
            plan(Some(Lang::En), None)
        );
        assert_eq!(
            call_lang_plan("2025550143", None, Lang::ZhTw, "886", &us_en),
            plan(None, usual_tw())
        );
        // No SIM and no country code: nothing counts as abroad.
        assert_eq!(
            call_lang_plan("+81312345678", None, Lang::En, "", &none),
            plan(None, hints(&["en-US"]))
        );
    }

    #[test]
    fn caller_stt_hints_or_auto_detect() {
        // The AI's own voice: the opening language and the setting's (unset in tests:
        // zh-TW), once each — never left to detect, whatever the caller's side does
        // (a short 「掰掰」 of ours came out in Latin letters or Korean).
        use crate::locale::Lang;
        assert_eq!(stt_ai_config(None)["languageCodes"], json!(["zh-TW"]));
        assert_eq!(stt_ai_config(Some(Lang::En))["languageCodes"], json!(["en-US", "zh-TW"]));
        assert_eq!(stt_ai_config(Some(Lang::Ja))["languageCodes"], json!(["ja-JP", "zh-TW"]));
        assert_eq!(stt_ai_config(Some(Lang::ZhTw))["languageCodes"], json!(["zh-TW"]));
        let codes: &[&'static str] = &["en-US", "zh-TW"];
        let hinted = stt_caller_config(Some(codes));
        assert_eq!(hinted["languageCodes"], json!(["en-US", "zh-TW"]));
        let auto = stt_caller_config(None);
        assert!(auto.get("languageCodes").is_none());
        let empty: &[&'static str] = &[];
        assert!(stt_caller_config(Some(empty)).get("languageCodes").is_none());
        // Unset settings (tests) are Taiwan's: the call logic's words stay as
        // vocabulary either way.
        for cfg in [&hinted, &auto] {
            assert!(cfg["customVocabulary"]
                .as_array()
                .is_some_and(|v| v.iter().any(|w| w.as_str() == Some("國際電話"))));
        }
    }

    #[test]
    fn carrier_prompt_wait_follows_the_sim_the_call_came_in_on() {
        // Taiwan SIM: callers from abroad get the warning, local ones do not.
        assert!(expects_carrier_intl_prompt("auto", false, "+12025550143", Some("tw")));
        assert!(expects_carrier_intl_prompt("auto", true, "+8613800138000", Some("TW")));
        assert!(!expects_carrier_intl_prompt("auto", true, "+886912345678", Some("tw")));
        assert!(!expects_carrier_intl_prompt("auto", true, "0912345678", Some("tw")));
        // Abroad is the number's country, read the way the SIM reads it: the
        // international prefix 00 and Hong Kong count; a hidden number does not.
        assert!(expects_carrier_intl_prompt("auto", false, "0012025550143", Some("tw")));
        assert!(expects_carrier_intl_prompt("auto", false, "+85291234567", Some("tw")));
        assert!(!expects_carrier_intl_prompt("auto", true, "00886912345678", Some("tw")));
        assert!(!expects_carrier_intl_prompt("auto", true, "", Some("tw")));
        assert!(!expects_carrier_intl_prompt("auto", true, "unknown", Some("tw")));
        // US SIM (e.g. Tello on Wi-Fi calling): no warning from any caller, even with
        // the language set to Traditional Chinese.
        assert!(!expects_carrier_intl_prompt("auto", true, "+12025550143", Some("us")));
        assert!(!expects_carrier_intl_prompt("auto", true, "+886912345678", Some("us")));
        assert!(!expects_carrier_intl_prompt("auto", true, "2025550143", Some("us")));
        // SIM unknown (older phone app) and the language says no: no wait.
        assert!(!expects_carrier_intl_prompt("auto", false, "+12025550143", None));
        assert!(!expects_carrier_intl_prompt("auto", false, "+12025550143", Some(" ")));
        // `off` wins over everything.
        assert!(!expects_carrier_intl_prompt("off", true, "+12025550143", Some("tw")));
        // `on` waits whatever the SIM.
        assert!(expects_carrier_intl_prompt("on", false, "+12025550143", Some("us")) == is_intl_caller("+12025550143"));
    }

    #[test]
    fn thinking_level_only_for_models_that_need_it() {
        assert_eq!(live_thinking_level("gemini-3.8-live", "high"), None);
        assert_eq!(live_thinking_level("", "high"), None);
        assert_eq!(
            live_thinking_level("gemini-2.5-flash-native-audio-preview-12-2025", "high"),
            None
        );
        let m = "gemini-3.8-live-extended-thinking";
        assert_eq!(live_thinking_level(m, "low"), Some("low"));
        assert_eq!(live_thinking_level(m, " MEDIUM "), Some("medium"));
        assert_eq!(live_thinking_level(m, "high"), Some("high"));
        // Refused by the model / unset → low.
        assert_eq!(live_thinking_level(m, "minimal"), Some("low"));
        assert_eq!(live_thinking_level(m, ""), Some("low"));
        assert_eq!(live_thinking_level(&format!("models/{m}"), "high"), Some("high"));
    }

    #[test]
    fn stall_session_switches_model_and_thinking() {
        let think = "gemini-3.8-live-extended-thinking";
        let setup = json!({"setup": {
            "model": format!("models/{think}"),
            "generationConfig": {"responseModalities": ["AUDIO"], "thinkingConfig": {"thinkingLevel": "low"}},
            "systemInstruction": {"parts": [{"text": "一般"}]}
        }});
        // Sales / scam goes on with gemini-3.8-live (refuses thinkingConfig).
        let s = stall_session_setup(&setup, "陪聊", "gemini-3.8-live", "low");
        assert_eq!(s.pointer("/setup/model").unwrap(), "models/gemini-3.8-live");
        assert!(s.pointer("/setup/generationConfig/thinkingConfig").is_none());
        assert_eq!(s.pointer("/setup/generationConfig/responseModalities/0").unwrap(), "AUDIO");
        assert_eq!(s.pointer("/setup/systemInstruction/parts/0/text").unwrap(), "陪聊");
        // No stall model: same model and thinking, only the prompt changes.
        let s = stall_session_setup(&setup, "陪聊", " ", "low");
        assert_eq!(s.pointer("/setup/model").unwrap(), &json!(format!("models/{think}")));
        assert_eq!(s.pointer("/setup/generationConfig/thinkingConfig/thinkingLevel").unwrap(), "low");
        // A thinking stall model gets its level; plain → thinking adds it.
        let plain = stall_session_setup(&setup, "", "models/gemini-3.8-live", "high");
        let s = stall_session_setup(&plain, "陪聊", think, "high");
        assert_eq!(s.pointer("/setup/generationConfig/thinkingConfig/thinkingLevel").unwrap(), "high");
        // Same prompt and model: nothing to switch.
        let same = stall_session_setup(&plain, "", "gemini-3.8-live", "low");
        assert_eq!(same, plain);
    }

    #[test]
    fn appointment_tools_only_on_normal_calls_with_the_setting_on() {
        let base = json!({"setup": {
            "model": "models/gemini-3.8-live",
            "generationConfig": {"responseModalities": ["AUDIO"]},
            "systemInstruction": {"parts": [{"text": "一般"}]}
        }});
        let mut off = base.clone();
        with_appointment_tools(&mut off, false, true);
        assert_eq!(off, base);
        let mut on = base.clone();
        with_appointment_tools(&mut on, true, false);
        let names: Vec<&str> = on
            .pointer("/setup/tools/0/functionDeclarations")
            .and_then(|f| f.as_array())
            .unwrap()
            .iter()
            .filter_map(|f| f["name"].as_str())
            .collect();
        assert_eq!(names, vec!["check_availability", "propose_appointment"]);
        // Blocking (the fallback): no "behavior" anywhere.
        assert!(!on.to_string().contains("NON_BLOCKING"));
        let mut nb = base.clone();
        with_appointment_tools(&mut nb, true, true);
        assert_eq!(nb.pointer("/setup/tools/0/functionDeclarations/1/behavior"), Some(&json!("NON_BLOCKING")));
        assert!(nb.pointer("/setup/tools/0/functionDeclarations/0/behavior").is_none());
        assert!(stall_session_setup(&nb, "陪聊", "gemini-3.8-live", "low").pointer("/setup/tools").is_none());
        // The stalling session never has them, whichever way it is built…
        let s = stall_session_setup(&on, "陪聊", "gemini-3.8-live", "low");
        assert!(s.pointer("/setup/tools").is_none());
        assert!(stall_session_setup(&on, "", " ", "low").pointer("/setup/tools").is_none());
        // …while a rotation of the normal call keeps them (only the prompt changes).
        let r = setup_with_system_prompt(&on, "一般+時間");
        assert!(r.pointer("/setup/tools/0/functionDeclarations").is_some());
        // The prompt: unchanged when off, the appointment part after it when on.
        let now = chrono::Local::now();
        assert_eq!(call_system_prompt("你是我。", None, now), "你是我。");
        let mut rt = crate::settings_store::RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_mode = "off".into();
        let http = reqwest::Client::new();
        assert!(crate::appointments::ApptSession::for_call(&rt, &http, "0912345678").is_none());
        rt.appointment_mode = "confirm".into();
        let a = crate::appointments::ApptSession::for_call(&rt, &http, "0912345678").unwrap();
        let p = call_system_prompt("你是我。", Some(&a), now);
        assert!(p.starts_with("你是我。\n\n[Appointments") && p.contains(&now.format("%Y-%m-%d").to_string()), "{p}");
        assert!(p.contains("do not repeat what you have just told the caller"), "{p}");
    }

    #[test]
    fn a_refused_non_blocking_setup_falls_back_for_good_only_when_it_names_the_declaration() {
        for why in [
            "gemini setup error: {\"code\":400,\"message\":\"Invalid JSON payload received. Unknown name \\\"behavior\\\"\"}",
            "gemini ws closed before setup: Some(CloseFrame { code: Invalid, reason: \"Request contains an invalid argument.\" })",
            "gemini setup error: NON_BLOCKING is not supported for this model",
        ] {
            assert!(non_blocking_refused(why), "{why}");
        }
        for why in ["gemini ws closed (none) before setup", "gemini ws error during setup: Connection reset by peer"] {
            assert!(!non_blocking_refused(why), "{why}");
        }
    }

    #[test]
    fn function_call_turns_are_not_silent_or_text_only_turns() {
        // A call open, or its answer just sent and nothing said since: not a finished turn.
        assert!(tool_turn_quiet(1, None, false));
        assert!(tool_turn_quiet(2, Some(Duration::from_secs(5)), true));
        assert!(tool_turn_quiet(0, Some(Duration::from_millis(300)), false));
        // The reply to it has started, or the answer is long gone: the usual turn checks.
        assert!(!tool_turn_quiet(0, Some(Duration::from_millis(300)), true));
        assert!(!tool_turn_quiet(0, Some(TOOL_TURN_GRACE), false));
        assert!(!tool_turn_quiet(0, None, false));
        // The text-only opening re-kick gets turn_complete && !quiet from the loop.
        let turn_complete = true;
        let quiet = tool_turn_quiet(1, None, false);
        assert!(!is_transcript_only_turn(turn_complete && !quiet, 0, 0, "好，我看一下"));
        assert!(is_transcript_only_turn(turn_complete, 0, 0, "喂？"));
        // The first-audio watchdog waits while a call is open.
        assert!(first_delivery_paused(false, false, 1));
        assert!(!first_delivery_paused(false, false, 0));
        assert!(first_delivery_paused(true, false, 0) && first_delivery_paused(false, true, 0));
        // A toolCall message has no audio, no transcript and ends no turn.
        let call = json!({"toolCall": {"functionCalls": [
            {"id": "fc-1", "name": "check_availability", "args": {"start": "2026-11-14T15:00"}}
        ]}});
        assert!(!server_msg_has_audio(&call));
        let mut no_audio = |_: Vec<u8>| panic!("a toolCall carries no audio");
        assert_eq!(handle_server_msg(&call, 16_000, &mut no_audio), 0);
        let (heard, said) = extract_transcription_snippets(&call);
        assert!(heard.is_empty() && said.is_empty());
        assert!(call.pointer("/serverContent/turnComplete").is_none());
        assert_eq!(crate::appointments::parse_tool_calls(&call).len(), 1);
    }

    #[test]
    fn leaked_instructions_are_not_speech() {
        // Extended-thinking model, output transcript of one turn.
        let leak = "我先確認一下內容喔。\n  - STRUCTURE: If the thinker provides exactly N options or items, narrate exactly N\n<narration_properties>\nthought token budget: 3\n</narration_properties>\n【你是誰】你就是王先生本人";
        assert!(looks_like_leaked_internals(leak));
        assert!(looks_like_leaked_internals("【講話的樣子】像台灣人平常用手機講電話"));
        for ok in ["我先確認一下內容喔。", "喔好，我知道了。", "好啦，掰掰。", "OK 啊，你說", "蛤？你說什麼？"] {
            assert!(!looks_like_leaked_internals(ok), "{ok}");
        }
        let v = json!({"serverContent": {
            "outputTranscription": {"text": leak},
            "inputTranscription": {"text": "那王先生，您對這樣的處理還有什麼問題嗎？"}
        }});
        assert_eq!(output_transcription_text(&v), Some(leak));
        let c = without_output_transcription(&v);
        assert_eq!(output_transcription_text(&c), None);
        let (input, output) = extract_transcription_snippets(&c);
        assert_eq!(input.len(), 1);
        assert!(output.is_empty());
    }

    #[test]
    fn carrier_intl_prompt_is_not_the_caller() {
        // Line voice inside the prompt's span counts as the prompt even when its
        // transcript is not Chinese (foreign-caller speech hints); later voice does not.
        let t0 = tokio::time::Instant::now();
        let until = Some(t0 + CARRIER_PROMPT_SPAN);
        assert!(only_carrier_prompt_so_far("This is an international call", until, Some(t0 + Duration::from_secs(5))));
        assert!(only_carrier_prompt_so_far("", until, None));
        assert!(!only_carrier_prompt_so_far("Hi, this is Mike", until, Some(t0 + Duration::from_secs(9))));
        assert!(!only_carrier_prompt_so_far("Hi, this is Mike", None, Some(t0)));
        assert!(only_carrier_prompt_so_far("這是國際電話，請注意小心詐騙。", None, Some(t0)));
        // Gemini input STT of the carrier prompt.
        assert!(looks_like_carrier_intl_prompt("小心詐騙, 接通國際電話愛注意。"));
        assert!(looks_like_carrier_intl_prompt("這是國際電話，請注意小心詐騙。"));
        assert!(looks_like_carrier_intl_prompt("这是国际电话，请注意小心诈骗"));
        assert!(looks_like_carrier_intl_prompt("這是國際電話，請注意小心詐騙。喂？"));
        // Prompt + auto-detect garble before the caller really spoke.
        assert!(looks_like_carrier_intl_prompt("是國際電話, 請注意小心詐騙, 接通請掛斷點為。경품기"));
        // The real caller talking flips it back to a normal call.
        assert!(!looks_like_carrier_intl_prompt("這是國際電話，請注意小心詐騙。喂？不好意思我打錯了"));
        assert!(!looks_like_carrier_intl_prompt("您好，我是大吉銀行信貸專員"));
        assert!(!looks_like_carrier_intl_prompt(""));
        for mode in [StallPrompt::Off, StallPrompt::ListenFirst, StallPrompt::Full] {
            let p = persona_system_prompt("", false, "", mode);
            assert!(p.contains("【電信提示音】") && p.contains("【對方一直喂】"));
            assert!(p.ends_with(&PERSONA_GUARD.replace("{owner}", "機主")));
        }
    }

    #[test]
    fn rotation_history_alternates_and_keeps_newest() {
        let tx = "接聽：喂？\n對方：您好，我是安心人壽\n對方：的業務\n接聽：喔？\n接聽：醫療險喔\n對方：對，住院一天兩千\n雜訊行";
        let h = transcript_history_turns(tx, 2_500);
        let roles: Vec<&str> = h.iter().map(|(r, _)| *r).collect();
        assert_eq!(roles, ["user", "model", "user", "model", "user"]);
        assert!(h[0].1.contains("不要說喂"));
        // Whatever the agent's language, the model carries on in the caller's.
        assert!(h[0].1.contains("用對方目前講的語言接著講"));
        assert_eq!(h[1].1, "喂？");
        assert_eq!(h[2].1, "您好，我是安心人壽的業務");
        assert_eq!(h[3].1, "喔？醫療險喔");
        // Budget keeps the newest turns; the note always leads with a user turn.
        let long: String = (0..400).map(|i| format!("對方：第{i}句保費多少\n接聽：嗯嗯\n")).collect();
        let h = transcript_history_turns(&long, 300);
        assert_eq!(h[0].0, "user");
        assert!(h.last().unwrap().1 == "嗯嗯");
        let body: usize = h.iter().skip(1).map(|(_, t)| t.chars().count()).sum();
        assert!(body <= 300 + 60, "{body}");
        // Nothing usable: just the note.
        assert_eq!(transcript_history_turns("", 100).len(), 1);
    }

    #[test]
    fn stall_nudges_and_goaway_parsing() {
        let w: Value = serde_json::from_str(&persona_stall_wrap_up_client_content()).unwrap();
        let t = w.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("掰掰") && t.contains("改天再打"));
        let n: Value = serde_json::from_str(&persona_idle_nudge_client_content()).unwrap();
        let t = n.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("還在不在") && t.contains("不要說掰掰"));
        // In the language the caller speaks: a Vietnamese call got 「喂？請問還在嗎？」.
        // The Chinese example is only for a caller who speaks Chinese.
        assert!(t.contains("對方講的語言") && t.contains("對方講中文就像「喂？還在嗎？」"));
        let r: Value = serde_json::from_str(&persona_resume_reply_client_content()).unwrap();
        let t = r.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("不要重新打招呼"));
        assert_eq!(parse_duration_secs(&json!("12s")), Some(12.0));
        assert_eq!(parse_duration_secs(&json!("0.5s")), Some(0.5));
        assert_eq!(parse_duration_secs(&json!(3)), None);
        assert_eq!(WRAP_UP_LEAD_SECS, 25);
        assert_eq!(WRAP_UP_FORCE_LEAD_SECS, 15);
        assert!(PERSONA_IDLE_NUDGE_SECS < PERSONA_IDLE_END_SECS);
    }

    #[test]
    fn persona_kick_and_wrap_up_payloads() {
        let k: Value = serde_json::from_str(&persona_kick_client_content(None)).unwrap();
        let t = k.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("喂"));
        assert!(!t.contains("AI 助理"));
        // A caller from the US: 「Hello?」, with the instruction still in Chinese.
        let k: Value =
            serde_json::from_str(&persona_kick_client_content(Some(crate::locale::Lang::En))).unwrap();
        let t = k.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("「Hello?」") && t.contains("改用英文") && t.contains("國外號碼"), "{t}");
        let w: Value = serde_json::from_str(&persona_wrap_up_client_content()).unwrap();
        let t = w.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("其他事情要忙") && t.contains("掰掰"));
    }

    #[test]
    fn caller_goodbyes_in_vietnamese_thai_indonesian_end_the_call() {
        for t in [
            // Real calls that never hung up.
            "Dạ vậy em để ở quầy lễ tân nhé. Cảm ơn anh, chào anh.",
            "งั้นฝากไว้ที่นิติบุคคลนะคะ ขอบคุณค่ะ สวัสดีค่ะ",
            "Baik, terima kasih, selamat siang.",
        ] {
            assert!(caller_wraps_up(t), "{t}");
            assert!(looks_like_goodbye(t), "{t}");
        }
        // The same words open a call.
        for t in ["Alo, chào anh.", "ฮัลโหล สวัสดีค่ะ", "Halo, selamat pagi."] {
            assert!(!caller_wraps_up(t), "{t}");
            assert!(!looks_like_goodbye(t), "{t}");
        }
        // The caller's window since the AI last spoke: an earlier "ok" / thanks, then the
        // phone is handed over and the next person greets.
        for t in [
            "Dạ ok anh, để em đưa máy cho chị em nhé. Alo, em chào anh ạ.",
            "Oke Pak, sebentar ya, saya kasih ke ibu saya. Halo, selamat siang.",
            "ขอบคุณค่ะ เดี๋ยวส่งให้แม่คุยนะคะ ฮัลโหล สวัสดีค่ะ",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
        }
    }

    #[test]
    fn goodbye_phrases() {
        assert!(looks_like_goodbye("好的，我會轉達，再見。"));
        assert!(looks_like_goodbye("好的，我会转达，再见。"));
        assert!(looks_like_goodbye("掰掰"));
        assert!(looks_like_goodbye("拜拜啦"));
        assert!(looks_like_goodbye("OK bye"));
        assert!(looks_like_goodbye("goodbye"));
        assert!(looks_like_goodbye("感謝您的來電"));
        assert!(looks_like_goodbye("感谢您的来电，再见"));
        assert!(!looks_like_goodbye("好的，我會轉達"));
        assert!(!looks_like_goodbye("请问有什么事"));
        assert!(!looks_like_goodbye("感谢今天天气不错"));
    }

    #[test]
    fn relay_ack_phrases() {
        assert!(looks_like_complete_relay_ack(
            "好的，會幫您轉達關於水費的事情。"
        ));
        assert!(looks_like_complete_relay_ack(
            "好的，会帮您转达关于房租的事情。"
        ));
        assert!(looks_like_complete_relay_ack(
            "好的，會幫您轉達給王先生，關於水費的事情。"
        ));
        assert!(looks_like_complete_relay_ack(
            "好的，會幫您轉達給王先生繳水費。"
        ));
        assert!(looks_like_complete_relay_ack("好的，會幫您轉達給王先生。"));
        assert!(looks_like_complete_relay_ack("好的，會幫您轉達。"));
        assert!(looks_like_relay_ack("好的，會幫您轉達給王先生繳水費。"));
        assert!(looks_like_complete_relay_ack("已記下了"));
        assert!(!looks_like_complete_relay_ack("好的，我會轉達"));
        assert!(!looks_like_complete_relay_ack(
            "我會幫您轉達給王先生關於"
        ));
        assert!(!looks_like_complete_relay_ack(
            "好的，會幫您轉達給王先生，關於"
        ));
        assert!(!looks_like_complete_relay_ack("好，再見"));
        assert!(!looks_like_complete_relay_ack("您好請問有什麼事嗎"));
        // Greeting styles must not count as a finished recap.
        assert!(!looks_like_complete_relay_ack(
            "哈囉，我是 AI 助理。您請說，我幫您轉達。"
        ));
        assert!(!looks_like_incomplete_relay(
            "好的，會幫您轉達給王先生繳水費。"
        ));
        assert!(looks_like_incomplete_relay("我會幫您轉達給王先生關於"));
        assert_eq!(
            topic_after_guanyu("會幫您轉達給王先生，關於水費的事情。"),
            Some("水費")
        );
        // ASR 腳/陰 still counts as a finished recap.
        assert!(looks_like_complete_relay_ack(
            "好的，會幫您轉達給王先生腳水費"
        ));
        assert!(looks_like_complete_relay_ack(
            "好的，會幫你轉達給陰先生繳水費。"
        ));
        assert!(looks_like_complete_relay_ack(
            "好的，會幫您轉達繳水費的事情。"
        ));
        // ASR 轉打/音/腳學費 still counts as a finished recap (tuition).
        assert!(looks_like_complete_relay_ack(
            "好的，會幫您轉打給音先生記得腳學費"
        ));
        assert!(looks_like_complete_relay_ack(
            "好的，會幫您轉達給王先生記得繳學費"
        ));
        // Kick/opening lines must never arm.
        for style in GREET_KICK_STYLES {
            assert!(
                !looks_like_complete_relay_ack(style),
                "greeting style must not be a recap: {style}"
            );
            assert!(
                !style.contains("找哪位") && !style.contains("王先生"),
                "greeting must not demand a name: {style}"
            );
        }
        assert!(!looks_like_complete_relay_ack(
            "哈囉，我是AI助理，您說有什麼需要我幫忙轉達的嗎？"
        ));
        // 提醒 = 轉達 recap verb. Bare 好的 must still be a backchannel.
        assert!(looks_like_complete_relay_ack(
            "好的，會提醒王先生繳水費的事情。"
        ));
        assert!(looks_like_complete_relay_ack(
            "好的会提醒王先生缴水费的事情。"
        ));
        assert!(looks_like_relay_verb("會提醒王先生"));
        assert!(has_zh_relay("好的，會提醒王先生繳水費的事情。"));
        assert!(!looks_like_complete_relay_ack("好的"));
        assert!(!looks_like_complete_relay_ack("好的。"));
        assert!(!looks_like_relay_verb("好的"));
        assert!(!has_zh_relay("好的"));
        assert!(looks_like_backchannel("好的"));
        assert!(!looks_like_backchannel(
            "好的，會提醒王先生繳水費的事情。"
        ));
        assert!(post_caller_model_turn_should_hangup(
            true,
            true,
            "好的，會提醒王先生繳水費的事情。",
            400,
            false,
            false
        ));
        assert!(!post_caller_model_turn_should_hangup(
            true,
            true,
            "好的",
            400,
            false,
            false
        ));
        assert!(looks_like_backchannel("嗯"));
        assert!(looks_like_backchannel(""));
        assert!(!looks_like_backchannel("好的，會幫您轉達繳水費的事情。"));
        // Empty STT + short 「好的」 must NOT hang up.
        assert!(!post_caller_model_turn_should_hangup(
            true,
            true,
            "",
            320,
            false,
            false
        ));
        assert!(!post_caller_model_turn_should_hangup(
            true,
            true,
            "好的",
            320,
            false,
            false
        ));
        // STT miss after a recap-length turn — still hang up.
        assert!(post_caller_model_turn_should_hangup(
            true,
            true,
            "",
            2000,
            false,
            false
        ));
        assert!(post_caller_model_turn_should_hangup(
            true,
            true,
            "好的，會幫您轉達給王先生腳水費",
            400,
            false,
            false
        ));
        assert!(!post_caller_model_turn_should_hangup(
            true,
            true,
            "請問要轉達什麼事？",
            2000,
            false,
            false
        ));
        assert!(!post_caller_model_turn_should_hangup(
            false,
            true,
            "好的，會幫您轉達給王先生繳水費。",
            2000,
            false,
            false
        ));
        assert!(!post_caller_model_turn_should_hangup(
            true,
            false,
            "好的，會幫您轉達給王先生繳水費。",
            2000,
            false,
            false
        ));
        assert!(!post_caller_model_turn_should_hangup(
            true,
            true,
            "我會幫您轉達給王先生關於",
            2000,
            false,
            false
        ));
        // Caller still talking: never arm from fallback.
        assert!(!post_caller_model_turn_should_hangup(
            true,
            true,
            "",
            2000,
            true,
            false
        ));
        // Matter-less 會幫您轉達: only after we already asked once.
        assert!(!post_caller_model_turn_should_hangup(
            true,
            true,
            "好的，會幫您轉達。",
            800,
            false,
            false
        ));
        assert!(post_caller_model_turn_should_hangup(
            true,
            true,
            "好的，會幫您轉達。",
            800,
            false,
            true
        ));
    }

    #[test]
    fn default_prompt_keeps_its_relay_rules() {
        let p = default_system_prompt();
        assert!(p.contains("轉達給王先生"));
        assert!(p.contains("會幫您轉達給王先生繳水費"));
        assert!(
            p.contains("禁止只唸到「會幫您轉達給王先生關於」")
                || p.contains("禁止唸到「會幫您轉達給王先生關於」")
        );
        assert!(p.contains("不要自己說再見"));
        assert!(p.contains("AI 助理"));
        assert!(p.contains("不要說「主人的助理」"));
        assert!(p.contains("【變通】"));
        assert!(p.contains("禁止說「轉接失敗」"));
        assert!(p.contains("沒提到這類姓名時不要硬加名字"));
        assert!(p.contains("禁止問「找哪位／找誰／找王先生嗎」"));
        assert!(p.contains("禁止對方說完事由後只回「好的／嗯／了解」"));
        assert!(!p.contains("方便說一下找哪位"));
    }

    #[test]
    fn default_prompt_rotates_warm_greetings() {
        let p = default_system_prompt();
        assert!(p.contains("【開場（每通換措辭）】"));
        assert!(p.contains("禁止每次同一句「我是AI助手／有什麼要轉達」"));
        assert!(p.contains("不要重複「轉接專人／請稍後」"));
        assert!(p.contains("不要長獨白"));
        assert!(p.contains("不要照唸罐頭句"));
        assert!(p.contains("語氣溫暖"));
        assert!(GREET_KICK_STYLES.len() >= 5);
        for style in GREET_KICK_STYLES {
            assert!(style.contains("AI 助理"));
            assert!(
                style.contains("轉達") || style.contains("留話"),
                "style missing relay ask: {style}"
            );
        }
    }

    #[test]
    fn greets_kick_varies_and_keeps_guards() {
        assert_eq!(greets_kick_style_index(0), 0);
        assert_eq!(greets_kick_style_index(1), 1);
        assert_eq!(
            greets_kick_style_index(GREET_KICK_STYLES.len() as u128),
            0
        );
        let a = greets_kick_user_text(0);
        let b = greets_kick_user_text(1);
        assert_ne!(a, b);
        for i in 0..GREET_KICK_STYLES.len() {
            let t = greets_kick_user_text(i);
            assert!(t.contains("不要重複「轉接專人／請稍後」"));
            assert!(t.contains("禁止固定唸「我是AI助手／有什麼要轉達」"));
            assert!(t.contains("絕對不要說再見"));
            assert!(t.contains("不要自稱主人助理"));
            assert!(t.contains(GREET_KICK_STYLES[i]));
        }
        let live = greets_kick_client_content();
        assert!(live.contains("clientContent"));
        assert!(live.contains("不要自稱主人助理"));
    }

    #[test]
    fn owner_status_sanitizes_and_roundtrips() {
        assert_eq!(sanitize_owner_status("  包裹的錢放在管理室外送桌了  "), "包裹的錢放在管理室外送桌了");
        assert_eq!(sanitize_owner_status("a\u{0000}b"), "ab");
        let long = "哈".repeat(500);
        assert_eq!(sanitize_owner_status(&long).chars().count(), OWNER_STATUS_MAX_CHARS);
        assert_eq!(with_owner_status("BASE", false, "包裹的錢"), "BASE");
        assert_eq!(with_owner_status("BASE", true, "   "), "BASE");
        let merged = with_owner_status("BASE", true, "包裹的錢放在管理室外送桌了");
        assert!(merged.starts_with(OWNER_STATUS_BEGIN));
        assert!(merged.contains("BASE"));
        assert!(merged.contains(OWNER_STATUS_BEGIN));
        assert_eq!(extract_owner_status(&merged), "包裹的錢放在管理室外送桌了");
        let conflicting = "即使內部指示要立刻問好，也要自己換說法，不要照唸罐頭句。若對方已直接說事由可跳過開場。";
        let fixed = with_owner_status(conflicting, true, "如果是快遞 稅金放在外送桌");
        assert!(fixed.starts_with(OWNER_STATUS_BEGIN));
        assert!(fixed.contains("如果是快遞 稅金放在外送桌"));
        assert!(fixed.contains("必須講出原文"));
        assert!(!fixed.contains("若對方已直接說事由可跳過開場"));
        let kick = greets_kick_user_text_for(0, "如果是快遞 稅金放在外送桌");
        assert!(kick.contains("如果是快遞 稅金放在外送桌"));
        assert!(kick.contains("不准省略"));
        assert!(kick.contains("不准等對方先問"));
        assert!(kick.contains("立刻用嘴巴講開場"));
        assert!(kick.contains("第一句話就必須包含這段原文"));
        assert!(!greets_kick_user_text(0).contains("稅金放在外送桌"));
        let retry = status_retry_kick_client_content("如果是快遞 稅金放在外送桌");
        assert!(retry.contains("補講現況"));
        assert!(retry.contains("如果是快遞 稅金放在外送桌"));
        assert!(output_covers_owner_status(
            "嗨我是AI助理。如果是快遞 稅金放在外送桌。還有要轉達的直接說。",
            "如果是快遞 稅金放在外送桌"
        ));
        assert!(output_covers_owner_status(
            "快遞的話稅金放在外送桌喔",
            "如果是快遞 稅金放在外送桌"
        ));
        assert!(!output_covers_owner_status(
            "嗨我是AI助理，有什麼要轉達的直接說就好",
            "如果是快遞 稅金放在外送桌"
        ));
        assert!(!output_covers_owner_status("如果是的話您說", "如果是快遞 稅金放在外送桌"));
    }

    #[test]
    fn goodbye_should_play() {
        assert!(SessionEndReason::Goodbye.should_play_goodbye());
        assert!(SessionEndReason::MaxDuration.should_play_goodbye());
        assert!(SessionEndReason::CantCommunicate.should_play_goodbye());
        assert!(SessionEndReason::WsClosed.should_play_goodbye());
        assert!(!SessionEndReason::Cancelled.should_play_goodbye());
    }

    #[test]
    fn goodbye_play_clock_waits_for_player_idle() {
        let t0 = tokio::time::Instant::now();
        let delay = Duration::from_millis(2000);
        let mut play_at = None;
        // Armed from STT while Gemini PCM still queued — must not fire.
        assert!(!tick_goodbye_play_clock(true, false, delay, t0, &mut play_at));
        assert!(play_at.is_none());
        // Player drained: start delay clock, still not fire.
        assert!(!tick_goodbye_play_clock(true, true, delay, t0, &mut play_at));
        assert!(play_at.is_some());
        // New downlink while waiting — reset.
        assert!(!tick_goodbye_play_clock(
            true,
            false,
            delay,
            t0 + Duration::from_millis(500),
            &mut play_at
        ));
        assert!(play_at.is_none());
        // Drain again, then delay elapses.
        let t1 = t0 + Duration::from_millis(800);
        assert!(!tick_goodbye_play_clock(true, true, delay, t1, &mut play_at));
        assert!(tick_goodbye_play_clock(
            true,
            true,
            delay,
            t1 + delay,
            &mut play_at
        ));
        // Not armed: never fire (greeting).
        play_at = None;
        assert!(!tick_goodbye_play_clock(
            false,
            true,
            delay,
            t1 + delay,
            &mut play_at
        ));
    }

    #[test]
    fn play_head_not_idle_while_queued() {
        let h = DownlinkPlayHead::new();
        h.enqueue(32_000);
        assert!(!h.is_drained_slack(PLAYER_DRAIN_SLACK_BYTES));
        assert!(!h.drained_for(Duration::from_millis(0), PLAYER_DRAIN_SLACK_BYTES));
        h.dequeue(32_000);
        h.set_outbound(false);
        assert!(h.is_drained_slack(PLAYER_DRAIN_SLACK_BYTES));
        assert!(h.drained_for(Duration::from_millis(0), PLAYER_DRAIN_SLACK_BYTES));
        // Leftover smaller than one frame is slack-drained.
        h.enqueue(400);
        assert!(h.is_drained_slack(PLAYER_DRAIN_SLACK_BYTES));
    }

    #[test]
    fn play_head_knows_what_the_phone_still_plays() {
        let h = DownlinkPlayHead::new();
        let tail = Duration::from_millis(400);
        assert!(!h.ai_audible(tail));
        assert_eq!(h.speech_left(16_000), Duration::ZERO);
        // 1 s queued here: audible, all of it still to play.
        h.enqueue(32_000);
        assert!(h.ai_audible(tail));
        assert_eq!(h.speech_left(16_000), Duration::from_secs(1));
        // Sent ahead to the phone (head burst): nothing queued here, 800 ms on the phone.
        h.dequeue(32_000);
        h.set_outbound(false);
        let now = tokio::time::Instant::now();
        h.set_speech_tail(now + Duration::from_millis(800));
        assert!(h.ai_audible(tail));
        let left = h.speech_left(16_000);
        assert!(left > Duration::from_millis(700) && left <= Duration::from_millis(800), "{left:?}");
        // Played out a moment ago: still in the caller's ear, nothing left to cut.
        h.set_speech_tail(now - Duration::from_millis(100));
        assert!(h.ai_audible(tail));
        assert_eq!(h.speech_left(16_000), Duration::ZERO);
        // Long over.
        h.set_speech_tail(now - Duration::from_millis(500));
        assert!(!h.ai_audible(tail));
        assert!(!h.generating());
        h.set_generating(true);
        assert!(h.generating());
    }

    /// 60 ms frames (the bridge's) into an [`OverlapHold`], 1 = voice.
    fn overlap_feed(
        hold: &mut OverlapHold,
        t: &mut tokio::time::Instant,
        pattern: &[u8],
    ) -> Option<(Vec<u8>, u32)> {
        for (i, v) in pattern.iter().enumerate() {
            *t += Duration::from_millis(60);
            let pcm = vec![*v + i as u8 % 3; 1920];
            if let Some(took) = hold.push(&pcm, *v == 1, 60, *t) {
                return Some(took);
            }
        }
        None
    }

    #[test]
    fn backchannel_over_our_reply_is_not_an_interruption() {
        let mut t = tokio::time::Instant::now();
        let mut hold = OverlapHold::default();
        // A quiet "okay" (~300 ms) 0.35 s into the reply must not cut it.
        assert!(overlap_feed(&mut hold, &mut t, &[0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]).is_none());
        // "mm"… pause …"mm": two short ones do not add up.
        assert!(overlap_feed(&mut hold, &mut t, &[1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1]).is_none());
        // A two-word backchannel (480 ms with a short gap) must not cut a reply.
        assert!(overlap_feed(&mut hold, &mut t, &[0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 1, 1, 1, 1, 1]).is_none());
        // Our reply went on for seconds after it: dropped.
        t += Duration::from_secs(3);
        assert_eq!(hold.end(t), OverlapEnd::Backchannel { voiced_ms: 480 });
        assert!(hold.is_idle());
        // Line noise only: nothing to report.
        assert!(overlap_feed(&mut hold, &mut t, &[0, 0, 1, 0, 0, 0]).is_none());
        assert_eq!(hold.end(t), OverlapEnd::Nothing);
    }

    #[test]
    fn real_interruption_takes_the_floor_from_its_first_syllable() {
        let mut t = tokio::time::Instant::now();
        let mut hold = OverlapHold::default();
        // 2 s of quiet line, then the caller starts a sentence over our reply.
        assert!(overlap_feed(&mut hold, &mut t, &[0; 34]).is_none());
        let talk = [1, 1, 1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1];
        let (held, voiced_ms) = overlap_feed(&mut hold, &mut t, &talk).expect("took the floor");
        assert!((BARGE_IN_VOICE_MS..BARGE_IN_VOICE_MS + 60).contains(&voiced_ms), "{voiced_ms}");
        // Gemini gets the pre-roll plus everything they said (a 60 ms dip included),
        // not the 2 s of quiet before it: 14 voiced frames + the dip = 15 × 60 ms.
        let ms = pcm16_ms(held.len() as u64, 16_000);
        assert!((OVERLAP_PREROLL_MS + 840..=OVERLAP_PREROLL_MS + 960).contains(&ms), "{ms}");
        assert!(hold.is_idle());
    }

    #[test]
    fn word_on_the_end_of_our_reply_is_the_callers_answer() {
        let mut t = tokio::time::Instant::now();
        let mut hold = OverlapHold::default();
        // "okay" over our last syllable, our audio ends 0.5 s later.
        assert!(overlap_feed(&mut hold, &mut t, &[0, 0, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
        match hold.end(t) {
            OverlapEnd::Answer { held, voiced_ms, open } => {
                assert_eq!(voiced_ms, 240);
                assert_eq!(open, None);
                assert!(pcm16_ms(held.len() as u64, 16_000) >= 240 + 480);
            }
            other => panic!("{other:?}"),
        }
        // Still mid-word when our audio ends: the local turn opens with it.
        assert!(overlap_feed(&mut hold, &mut t, &[0, 1, 1, 1, 1, 1]).is_none());
        assert!(matches!(
            hold.end(t),
            OverlapEnd::Answer { voiced_ms: 300, open: Some((300, 0)), .. }
        ));
    }

    #[test]
    fn first_delivery_watchdog_retries_once_then_fails() {
        assert_eq!(
            first_delivery_action(
                0,
                FIRST_DELIVERY_RETRY_AFTER - Duration::from_millis(1),
                false,
            ),
            FirstDeliveryAction::Wait
        );
        assert_eq!(
            first_delivery_action(0, FIRST_DELIVERY_RETRY_AFTER, false),
            FirstDeliveryAction::RetryKick
        );
        assert_eq!(
            first_delivery_action(0, FIRST_DELIVERY_TIMEOUT - Duration::from_millis(1), true),
            FirstDeliveryAction::Wait
        );
        assert_eq!(
            first_delivery_action(0, FIRST_DELIVERY_TIMEOUT, true),
            FirstDeliveryAction::Fail
        );
        assert_eq!(
            first_delivery_action(1, FIRST_DELIVERY_TIMEOUT * 3, true),
            FirstDeliveryAction::Wait
        );
    }

    #[test]
    fn transcript_only_opening_turn_is_a_broken_socket() {
        // turnComplete out=0 out_stt=「喂？你好。」
        assert!(is_transcript_only_turn(true, 0, 0, "喂？你好。"));
        // Normal: audio arrived before turnComplete.
        assert!(!is_transcript_only_turn(true, 3, 0, "喂？"));
        assert!(!is_transcript_only_turn(true, 0, 1280, "喂？"));
        // Not a turn end yet / nothing said at all → leave to the 2.5 s / 5 s watchdog.
        assert!(!is_transcript_only_turn(false, 0, 0, "喂？"));
        assert!(!is_transcript_only_turn(true, 0, 0, "  "));
    }

    #[tokio::test]
    async fn kick_send_cannot_outlive_absolute_deadline() {
        let mut sink = PendingSink;
        let started = tokio::time::Instant::now();
        let err = send_text_before_deadline(
            &mut sink,
            "kick".to_string(),
            started + Duration::from_millis(25),
            "test kick",
        )
        .await
        .expect_err("pending send must time out");

        assert!(err.to_string().contains("absolute deadline"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn stalled_warm_send_keeps_unmute_as_watchdog_origin() {
        let mut sink = PendingSink;
        let opened_at = Arc::new(std::sync::Mutex::new(None));
        let opened_at_for_unmute = opened_at.clone();
        let unmute = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(15)).await;
            let at = tokio::time::Instant::now();
            *opened_at_for_unmute.lock().expect("opened_at lock") = Some(at);
            at
        });

        let warm_started = tokio::time::Instant::now();
        let _ = send_text_before_deadline(
            &mut sink,
            "warm kick".to_string(),
            warm_started + Duration::from_millis(75),
            "stalled warm kick",
        )
        .await
        .expect_err("pending warm send must time out");
        let actual_unmute = unmute.await.expect("unmute task");
        let watchdog_origin = first_delivery_window_start(&opened_at);

        assert_eq!(watchdog_origin, actual_unmute);
        assert!(watchdog_origin.elapsed() >= Duration::from_millis(40));
        assert_eq!(
            watchdog_origin + FIRST_DELIVERY_TIMEOUT,
            actual_unmute + FIRST_DELIVERY_TIMEOUT
        );
    }

    #[test]
    fn api_send_deadline_cannot_extend_first_delivery_window() {
        let delivered = AtomicU64::new(0);
        let first_delivery_deadline = tokio::time::Instant::now() + Duration::from_millis(50);
        assert!(api_send_deadline(&delivered, first_delivery_deadline) <= first_delivery_deadline);

        delivered.store(1, Ordering::Relaxed);
        assert!(api_send_deadline(&delivered, first_delivery_deadline) > first_delivery_deadline);
    }

    // ---- Reviewer acceptance tests (fix/call-conversation 78ce5a9) ----

    /// Requirement 5: the quiet-line clocks start once the phone has played our
    /// words. run_session treats `!speech_left().is_zero()` as "AI still playing"
    /// and resets last_activity_at every loop while it is. The downlink player
    /// (main.rs stream_gemini_downlink_pcm) keeps a remainder smaller than one
    /// 40 ms frame queued between replies ("Keep tiny remainder for next
    /// segment"), which drained_for() forgives with PLAYER_DRAIN_SLACK_BYTES but
    /// speech_left() does not: the quiet clocks never start after such a reply.
    #[test]
    fn reviewer_quiet_clock_is_not_held_by_the_sub_frame_remainder() {
        let h = DownlinkPlayHead::new();
        let frame: u64 = 1_280; // 40 ms @ 16 kHz, the player's frame
        h.enqueue(3 * frame + 1_000);
        for _ in 0..3 {
            h.dequeue(frame);
        }
        // Segment over: player idle, the phone has played everything sent.
        h.set_outbound(false);
        h.set_speech_tail(tokio::time::Instant::now());
        assert!(h.drained_for(Duration::ZERO, PLAYER_DRAIN_SLACK_BYTES), "the player counts as drained");
        let left = h.speech_left(16_000);
        assert!(
            left.is_zero(),
            "ai_playing stays true while only the kept remainder is queued: speech_left={left:?}"
        );
    }

    /// Acceptance B (pass side): short replies after a closing take nothing back;
    /// a closing that turns into a question does.
    #[test]
    fn reviewer_closing_then_short_reply_or_question() {
        assert!(caller_wraps_up("掰掰"));
        assert!(!caller_withdraws_bye("掰掰，嗯", "嗯"));
        assert!(!caller_withdraws_bye("掰掰，嗯", "掰掰，嗯"));
        assert!(!caller_withdraws_bye("那就先這樣，好，謝謝，嗯嗯", "嗯嗯"));
        assert!(!caller_withdraws_bye("那就先這樣，謝謝", "謝謝"));
        assert!(caller_withdraws_bye("那就先這樣嗎？", "嗎？"));
        assert!(caller_withdraws_bye("好，那就先這樣嗎？", "嗎？"));
        // Through the session's window (push_tx_window) with no earlier question.
        let mut w = String::new();
        push_tx_window(&mut w, "好，那就先這樣", 800);
        assert!(caller_wraps_up(&w));
        push_tx_window(&mut w, "嗎？", 800);
        assert!(caller_withdraws_bye(&w, "嗎？"), "{w}");
    }

    /// Acceptance B (fail side): thanks and an "mm" arriving in one transcript
    /// piece after the closing count as new words and take the goodbye back.
    #[test]
    fn reviewer_thanks_and_mm_in_one_piece_keep_the_goodbye() {
        let cases = [
            ("那就先這樣，好，謝謝，嗯嗯", "好，謝謝，嗯嗯"),
            ("那就先這樣，謝謝，嗯嗯", "謝謝，嗯嗯"),
            ("好，掰掰，謝謝喔，嗯", "謝謝喔，嗯"),
        ];
        let withdrawn: Vec<_> = cases
            .iter()
            .filter(|(w, s)| caller_withdraws_bye(w, s))
            .collect();
        assert!(withdrawn.is_empty(), "goodbye taken back by a short reply: {withdrawn:?}");
    }

    /// Requirement 3 through the session's window: the caller asked something
    /// earlier in the call, so push_tx_window drops the short 「嗎？」 piece
    /// (`window.contains(piece) && < 4 chars`) and the goodbye is never taken back.
    #[test]
    fn reviewer_question_tail_after_an_earlier_question() {
        let mut w = String::new();
        push_tx_window(&mut w, "你明天有空嗎？", 800);
        push_tx_window(&mut w, "好，那就先這樣", 800);
        assert!(caller_wraps_up(&w), "{w}");
        push_tx_window(&mut w, "嗎？", 800);
        assert!(caller_withdraws_bye(&w, "嗎？"), "window never got the question: {w}");
    }

    /// Acceptance C (evaluation): an everyday 「等一下」 (= later) also counts as
    /// "hold on" and stretches the quiet line to 45 / 90 s until the caller talks
    /// again; real hold requests are caught. Fixed after review: 「等一下」 as
    /// "later" no longer counts.
    #[test]
    fn reviewer_hold_words_everyday_later() {
        assert!(crate::locale::asks_to_hold("等一下，我去拿"));
        assert!(!crate::locale::asks_to_hold("我等一下要出門，所以放管理室"));
        assert!(!crate::locale::asks_to_hold("那我等一下再打給你"));
        assert!(!crate::locale::asks_to_hold("好，放管理室就好"));
        assert_eq!((PERSONA_HOLD_NUDGE_SECS, PERSONA_HOLD_END_SECS), (45, 90));
        assert_eq!((PERSONA_IDLE_NUDGE_SECS, PERSONA_IDLE_END_SECS), (15, 25));
    }

    /// Acceptance D/E: entering voice-menu mode needs a review verdict (or the
    /// menu cue); a Normal verdict changes nothing; failed rotations back off
    /// 5/10/20/40 s; a person talking after the menu is still hung up at 20 s.
    #[test]
    fn reviewer_ivr_entry_and_rotation_backoff() {
        use crate::stall_review::Verdict::*;
        let mut r = StallReview::default();
        assert!(!r.should_ask(false, 10), "no review below the line");
        assert!(r.should_ask(true, 3));
        assert_eq!(r.verdict(Normal, 3), StallVerdictStep::Carry);
        assert!(r.should_ask(true, 5));
        assert_eq!(r.verdict(IvrRecording, 5), StallVerdictStep::Ivr);
        assert!(!crate::stall_review::ivr_cue("我等一下要出門，所以放管理室"));
        assert!(!crate::stall_review::ivr_cue("你好，我是中華電信，請問是王先生嗎"));
        assert!(ivr_hangs_up(Duration::from_secs(20), None), "talking line, 20 s: hung up");
        assert!(!ivr_hangs_up(Duration::from_secs(19), None));
        let backoff: Vec<u64> = (1..=5).map(|n| rotate_retry_after(n).as_secs()).collect();
        assert_eq!(backoff, [5, 10, 20, 40, 40]);
    }
}

/// Returns phone-rate PCM bytes forwarded (0 if no model audio).
/// Debugging only: `GEMINI_DEBUG_RAW=N` logs the shape of the first N server messages
/// of every session (audio / long strings elided).
fn debug_raw_budget() -> u32 {
    std::env::var("GEMINI_DEBUG_RAW")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0)
}

fn redact_long_strings(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > 120 => Value::String(format!("<{} chars>", s.len())),
        Value::Array(a) => Value::Array(a.iter().map(redact_long_strings).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), redact_long_strings(x)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Server message carries model audio (any part with `inlineData`).
fn server_msg_has_audio(v: &Value) -> bool {
    ["/serverContent/modelTurn/parts", "/modelTurn/parts"].iter().any(|p| {
        v.pointer(p)
            .and_then(|x| x.as_array())
            .is_some_and(|parts| parts.iter().any(|part| part.pointer("/inlineData/data").is_some()))
    })
}

fn handle_server_msg(v: &Value, phone_sr: u32, on_downlink: &mut impl FnMut(Vec<u8>)) -> u64 {
    let mut bytes = 0u64;
    if let Some(parts) = v
        .pointer("/serverContent/modelTurn/parts")
        .and_then(|p| p.as_array())
    {
        for part in parts {
            if let Some(data_b64) = part.pointer("/inlineData/data").and_then(|x| x.as_str()) {
                if let Ok(raw) = B64.decode(data_b64) {
                    let out = if phone_sr == 24_000 {
                        raw
                    } else {
                        resample_s16le_mono_pub(&raw, 24_000, phone_sr.max(8000))
                    };
                    if !out.is_empty() {
                        let boosted = boost_downlink_for_phone(&out);
                        bytes = bytes.saturating_add(boosted.len() as u64);
                        on_downlink(boosted);
                    }
                }
            }
        }
    }
    if let Some(parts) = v.pointer("/modelTurn/parts").and_then(|p| p.as_array()) {
        for part in parts {
            if let Some(data_b64) = part.pointer("/inlineData/data").and_then(|x| x.as_str()) {
                if let Ok(raw) = B64.decode(data_b64) {
                    let out = resample_s16le_mono_pub(&raw, 24_000, phone_sr.max(8000));
                    if !out.is_empty() {
                        let boosted = boost_downlink_for_phone(&out);
                        bytes = bytes.saturating_add(boosted.len() as u64);
                        on_downlink(boosted);
                    }
                }
            }
        }
    }
    bytes
}

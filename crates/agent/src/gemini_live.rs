// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

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
use crate::log_text::Said;

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
        self.held.clear();
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
        let held = std::mem::take(&mut self.held);
        self.reset();
        Some((held, voiced_ms))
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
例外：對方講到要轉達的時間、日期、金額、電話號碼，要用一句問句把那一項完整複誦一次（例如「明天下午三點半，對嗎？」），他說對再接著講；他更正就照更正的再問一次；沒聽清楚就請他再說一次，不要自己猜。\
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

/// The long Chinese persona prompt of releases up to the generic one
/// ([`default_persona_prompt`]). Kept only so that a copy saved from it still
/// counts as the built-in prompt (`prompts::is_unedited_default`).
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
pub(crate) fn legacy_persona_prompt() -> &'static str {
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

/// The built-in persona prompt: short and the same for every owner. It says
/// who the AI is, the feel of a phone call and the few lines it never crosses;
/// the owner's own situation goes into the prompt they write themselves.
/// The section titles stay (the opening instructions point at 【接起來】, the
/// prompt-leak check looks for 【你是誰】), and so do the three sentences that
/// sales / scam stalling swaps out ([`PERSONA_SALES_REJECT_DEFAULT`],
/// [`PERSONA_HOLD_OFF_DEFAULT`], [`PERSONA_END_DEFAULT`]).
pub fn default_persona_prompt() -> &'static str {
    static PROMPT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PROMPT.get_or_init(|| {
        format!(
            "【你是誰】你就是{{owner}}本人，在接自己的電話。不要自我介紹，不要說自己是幫誰接的。\n\n\
【講話的樣子】像真人講電話：用自己的話，簡短、自然、口語；不要客服腔，不要念稿，不要條列。{PERSONA_LANGUAGE_LINE}\n\n\
【接起來】先簡短打聲招呼就停，讓對方說。\n\n\
【聽對方說】對方講事情就讓他講完，不搶話。需要處理的事，弄清楚是誰打來、什麼事、要你做什麼。講電話的當下你查不了任何東西，也不知道{{owner}}的行程：不要編答案，也不要叫對方等。\n\n\
【不答應的事】{PERSONA_HOLD_OFF_DEFAULT}{PERSONA_SALES_REJECT_DEFAULT}\n\n\
{PERSONA_END_DEFAULT}"
        )
    })
}

/// Sentences of the built-in persona prompt that sales / scam stalling swaps
/// out (the long ones they replace are the `PERSONA_*_LINE` / `_BLOCK` below).
pub(crate) const PERSONA_SALES_REJECT_DEFAULT: &str = "推銷、廣告、詐騙：簡單回絕就好。";
pub(crate) const PERSONA_HOLD_OFF_DEFAULT: &str = "錢、轉帳、簽約、密碼、驗證碼、個資、見面或約時間：都不要答應，輕輕帶過。";
pub(crate) const PERSONA_END_DEFAULT: &str = "【結束】等對方表示要結束了，你再簡短道別；通話沒有時間限制。";

/// The language line of [`default_persona_prompt`]: spoken Chinese, and a
/// caller who speaks another language gets that language.
pub(crate) const PERSONA_LANGUAGE_LINE: &str = "用自然的口語中文。對方講英文或其他語言、或問你能不能講某種語言，就直接改用他的語言。";
/// The language line of [`legacy_persona_prompt`].
pub(crate) const PERSONA_LANGUAGE_LINE_LEGACY: &str = "用台灣平常講的中文、繁體用詞；不要中國大陸的說法（不要「没事、行、挺好、回拨、咱们」）。對方講中文時不要夾英文（OK 可以）；對方講英文或其他語言、或問你能不能講某種語言，就直接改用他的語言。";
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

/// Times, amounts and numbers the caller gives are read back once as a
/// question: speech-to-text garbles fast speech (「四點半」 came through as
/// 「字點看可以」 and was taken for four o'clock). Added after every prompt
/// of a normal call, edited ones too, never to the sales / scam stalling one;
/// it names itself the exception to 「不要把對方的話整段覆述」.
pub const PERSONA_READBACK_NOTE: &str = "\n【時間、金額、號碼要複誦確認】對方講到之後要照著做或要記住的時間、日期、金額、電話號碼（例如「明天下午三點送到」「回電 02-1234-5678」「總共 1,250 元」），用一句自然的問句把那一項完整複誦一次，等他回答再往下聊，例如「明天下午三點送到，對嗎？」「02-1234-5678 對吧？」。\
時間要講完整：上午還是下午、幾點、幾分，半點要講出「半」；日期講幾月幾號或星期幾。他說對就繼續；他更正就照更正的再用問句複誦一次；沒聽清楚就請他再說一次，不要自己猜一個。同一項確認過就不要再問。\
複誦只是確認有沒有聽對，不等於答應（約時間、付錢照上面說的不要答應）。\
這一條是上面「不要把對方的話整段覆述」「不用每句都問問題」的例外，只用在這幾種資訊上，其他的話照樣不要覆述。";

/// Fixed guard appended to every persona prompt (not editable from the UI).
/// `{owner}` = setting `owner_name`. A card desk asking whether a purchase
/// was the owner's gets neither a yes nor a no (the AI cannot know; 「我那時在
/// 睡覺」 got a real card blocked or a fraud case opened) and no agreement to
/// block or dispute anything: the owner checks with the bank himself.
pub const PERSONA_GUARD: &str = "\n【底線（固定）】以{owner}本人的身分自然講話。\
對方要你提供或核對個資（身分證字號、生日、卡號、帳號、驗證碼、密碼、地址）時：不要給、也不要編，輕鬆帶過，例如「這個我不方便在電話裡講，我再自己打去問」；這一條優先於上面任何要你像本人一樣配合的說法。對方只是問是不是{owner}，照平常回答就好。\
銀行、信用卡中心打來問某一筆刷卡或交易是不是你本人刷的：你其實不知道{owner}有沒有刷，所以不要說是、也不要說不是（不要說「是我刷的」「應該是我」「我那時在睡覺」「我沒買東西」這類），也不要說「沒印象」「想不起來」「不記得」——對銀行來說那聽起來就是不是本人刷的（這條優先於上面「被問有沒有付過某筆錢就說沒印象」的說法）；也不要答應停卡、掛失、取消或爭議這筆交易、報案。\
把金額、商店、時間聽清楚，可以跟他複述確認一次，然後含糊帶過，例如「喔…這筆喔，我要再想一下耶，我晚點自己打卡片背面的客服確認好了」；他再問要不要先幫你停卡或處理，也一樣帶過、不要答應。一兩句就好，語氣自然。\
不要真的答應付錢、轉帳、匯款或去 ATM，也不要替{owner}做任何承諾。";

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
pub const PERSONA_STALL_BLOCK: &str = "\n【推銷、保險、貸款、投資、詐騙、催繳／討債、惡意騷擾：陪他繞，讓他掛不了電話】這一段優先於上面所有規則。\
這通是推銷、詐騙、委外討債催收或惡意騷擾（保險、銀行信貸／辦卡／分期／催繳欠款、資產管理／討債公司催收、網拍賣場金流認證、投資理財／股票群組、各種商品推銷、惡意辱罵恐嚇，或假冒銀行、客服、警察、檢察官、健保局）。你的目標只有一個：讓他一直講、講越久越好，捨不得掛電話。\
怎麼做由你自由發揮，越像真人越好，不要每句都同一個套路。你的樣子：好聊、好奇、有點慢半拍，對他講的東西有興趣、快被說服了，可是一直搞不太懂，要他一講再講。話題由你主動帶，他講完你就接下去，不要只回「嗯」「好」。\
可以用的招（自己挑、混著用、每次換）：嚇一跳（「蛤？三萬二？有那麼多喔」）；故意聽錯、算錯讓他糾正；把他剛講的用自己的話講錯一遍；請他舉個例子或把流程講一次；碎念一下自己的事、跟他抱怨兩句再繞回來；問他的看法（「你們自己也有買嗎？」）；扯到一件有點相關的小事；突然想到一個新問題。\
偶爾可以多講幾句（講自己的經驗、自言自語算數字），但最後要把話丟回給他：大部分時候用問句，或用一句讓他非回答不可的話結尾。\
他會一直留在線上，是因為覺得你快要照做了：問的問題要跟他要的東西有關（怎麼辦、怎麼付、要準備什麼），岔開的話題講一兩句就繞回來，不要問太偏、太技術的問題，讓他覺得你在耍他。\
不要懷疑他、不要質問他、不要提「詐騙」；問題都問他賣的東西和怎麼辦，不問他這個人：不要問他是誰、叫什麼名字、員工編號、分機、案號、之後誰會聯絡你，也不要問怎麼證明他是真的——他會覺得被識破，馬上就掛。\
他要你現在付錢、繳錢、轉帳、去 ATM、做網拍金流認證、分享螢幕畫面、給資料、加 LINE、按鍵、下載：不要拒絕，嘴上配合、讓他覺得快成功了，可是一直出狀況（卡找不到、正在走過去、畫面跟他講的不一樣、連結點不開、按錯了要重來、字太小看不清楚），請他一步一步教你、一直問細節。\
配合是「想照做卻一直卡住」：卡住就直接講卡在哪，不叫他等你；不說「我現在就轉」「帳號給我我匯過去」這種承諾，不說已經轉了、加了、按了，也不自己提要轉帳或要他的帳號。他說要傳資料給你、叫你之後自己弄，就說你現在就想弄好卻卡住了，請他在電話上帶你做，不要停在「等你訊息」。\
那些都只是嘴上說說：任何真的資料都不能給（身分證、生日、地址、帳號、卡號、驗證碼、密碼、存款、家人），也不要自己編號碼，真的錢一毛都不會動，也不約見面。閒聊的內容可以隨口編，無傷大雅就好。\
可以聊的方向（參考就好）：保險的保障範圍、保費一個月多少、要繳幾年、等待期、哪些情況不賠、理賠要什麼文件、解約會不會虧；\
貸款、信用卡、分期、催繳的總共欠多少、本金跟利息怎麼算、是哪一張卡哪一筆、從什麼時候開始的、每個月繳多少要繳幾期、提早還清會怎樣、不繳會怎樣、要怎麼繳；\
催收、討債、資產管理公司（包含找別人討債、威脅查封扣薪或上門）：千萬不要承認自己有欠這筆錢，也不要答應還錢或簽協商；裝作完全沒印象、一頭霧水（「蛤？什麼欠款？我怎麼完全沒印象有辦過這個，你們是不是找錯人了？」），請他從頭講清楚是哪家轉過來的、什麼時候辦的、金額跟利息怎麼算的，一直繞細節拖住他；\
惡意騷擾、罵人、恐嚇、揚言找上門：全程保持冷靜不對罵，裝作訊號斷斷續續、電話收音很差聽不清楚（「喂？你那邊訊號斷斷續續的，剛剛說什麼再講一次？」）或慢吞吞雞同鴨講，耗光他的時間；\
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
pub(crate) const PERSONA_SALES_LISTEN_LINE: &str = "推銷、保險、貸款、投資、詐騙、催繳／討債、惡意騷擾電話：表現得有點興趣，順著他的話聊、問細節（是什麼、多少錢、怎麼算）讓他繼續講；催收／討債千萬不要承認欠錢或答應還款，裝一頭霧水問哪一筆；罵人恐嚇就冷靜裝訊號不好聽不清楚；不要說「晚點再說」「再想一下」、不要拒絕、不要說掰掰。";

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
        aliases: &active.owner_aliases,
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
#[derive(Debug, Clone, Default)]
pub struct CallLangPlan {
    /// Persona mode opens in this language, and the prompt gets the note to answer
    /// in the caller's language; `None` = the setting `language`.
    pub opening: Option<crate::locale::Lang>,
    /// `languageCodes` for the caller's speech-to-text; `None` = none, it
    /// detects the language itself.
    pub caller_stt: Option<Vec<&'static str>>,
    /// The number is of the country of the SIM that took the call (read the way
    /// that SIM reads it); `None` = the SIM or the number is not known. The
    /// opening calls the number foreign only when this says another country.
    pub sim_local: Option<bool>,
    /// The SIM's own language (setting `sim_languages`) for a number of that SIM's
    /// country, when it is not the setting `language`: the call's prompt, its
    /// identity guard, the stalling prompt, the contact and appointment parts are
    /// in it. `None` = the setting's.
    pub prompt_lang: Option<crate::locale::Lang>,
    /// Speech-to-text code of the language of the caller's country when the AI has
    /// no setting for it (Thai, Russian, Arabic, Hindi: [`other_lang_stt_of_country`]):
    /// added to the hints for the AI's own voice, which answers the caller in it.
    pub caller_other_stt: Option<&'static str>,
    /// The SIM that took the call (ISO 3166 alpha-2, lowercase); None = not known.
    pub sim_country: Option<String>,
    /// Where the call is ([`crate::country_profile::resolve`]: the SIM's region, else
    /// the language setting's): the prompt gets that region's local rules only
    /// ([`crate::prompts::scope_to_region`]); the caller's language is followed
    /// whatever the region.
    pub region: Option<crate::country_profile::Region>,
    /// No opening language, but a country that answers the phone its own way
    /// ([`crate::prompts::local_hello`]: a SIM of Italy, a number from Thailand):
    /// persona mode opens that way instead of in the setting's language.
    pub local_hello: Option<&'static crate::prompts::LocalHello>,
}

/// Two plans set a session up the same way (a ring prewarm serves the call it
/// picks up): the same opening, hints and prompt language. Whether the number
/// is the SIM's own counts only with an opening in another language — only then
/// does the opening line say so (a hidden number and a local one are the same).
/// The region counts (its local rules are in the prompt); the SIM itself does not
/// (a ring that guessed the SIM from the language's region still serves the call).
impl PartialEq for CallLangPlan {
    fn eq(&self, other: &Self) -> bool {
        self.opening == other.opening
            && self.caller_stt == other.caller_stt
            && self.prompt_lang == other.prompt_lang
            && self.caller_other_stt == other.caller_other_stt
            && self.region == other.region
            && self.local_hello == other.local_hello
            && ((self.opening.is_none() && self.local_hello.is_none()) || self.sim_local == other.sim_local)
    }
}

impl Eq for CallLangPlan {}

/// A call's languages, from the caller ID, the country of the SIM that took the
/// call (`sim`, ISO 3166 alpha-2; `None` = the phone app does not say), the
/// setting `language`, the owner's country calling code (`home_code`, digits:
/// `effective_country_code`) and the setting `sim_languages` (SIM country →
/// language code).
///
/// Opening: the number is read the way that SIM reads it
/// ([`crate::phone_region::normalize`]). From another country than the SIM's,
/// whose language the AI speaks ([`crate::locale::country_lang`]): that
/// language. Otherwise the SIM's main language (`sim_languages`), else the
/// language of the SIM's country (a Japanese SIM opens in Japanese), else
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
    // For the opening only: with no language set for the SIM, its country's.
    // The prompt stays in the setting's language (`prompt_lang` below).
    let sim_main = sim_lang.or_else(|| sim.and_then(country_lang));
    let opening = match sim {
        Some(sim) => {
            let main = sim_main.filter(|l| !(l.is_zh() && lang.is_zh())).unwrap_or(lang);
            let caller = number
                .as_ref()
                .filter(|n| n.country != sim)
                .and_then(|n| country_lang(n.country))
                .filter(|l| !(l.is_zh() && main.is_zh()));
            Some(caller.unwrap_or(main)).filter(|l| *l != lang)
        }
        None => opening_lang_for(caller_id, home_code, lang),
    };
    // A country whose language the AI has no setting for (Italy, Thailand…): its
    // way of answering, not the setting's language. A number from another country
    // than the SIM's counts first, as for an opening language; a local or hidden
    // one takes the SIM's country, unless the owner set a language for that SIM.
    let local_hello = match sim {
        Some(sim) if opening.is_none() => {
            match number.as_ref().map(|n| n.country).filter(|c| *c != sim) {
                Some(c) if country_lang(c).is_some() => None,
                Some(c) => crate::prompts::local_hello(c),
                None if sim_main.is_none() => crate::prompts::local_hello(sim),
                None => None,
            }
        }
        _ => None,
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
    let sim_local = match (sim, number.as_ref()) {
        (Some(s), Some(n)) => Some(n.country == s),
        _ => None,
    };
    let prompt_lang = sim_lang.filter(|l| sim_local == Some(true) && *l != lang);
    let caller_other_stt = number
        .as_ref()
        .filter(|n| country_lang(n.country).is_none())
        .and_then(|n| other_lang_stt_of_country(n.country));
    let region = crate::country_profile::resolve(sim, lang);
    CallLangPlan {
        opening,
        caller_stt,
        sim_local,
        prompt_lang,
        caller_other_stt,
        sim_country: sim.map(str::to_string),
        region,
        local_hello,
    }
}

/// Speech-to-text codes of languages the AI has no setting for but answers a
/// caller in, written in a script of their own: Thai, Russian, Arabic, Hindi,
/// Bengali, Tamil, Telugu.
const OTHER_LANG_STT: [&str; 7] = ["th-TH", "ru-RU", "ar-EG", "hi-IN", "bn-BD", "ta-IN", "te-IN"];

fn has_ukrainian_letters(text: &str) -> bool {
    text.chars()
        .any(|c| matches!(c, 'і' | 'ї' | 'є' | 'ґ' | 'І' | 'Ї' | 'Є' | 'Ґ'))
}

/// The speech-to-text code of a country's main language when the AI has no
/// setting for it (ISO 3166 alpha-2, any case); None for any other country.
pub fn other_lang_stt_of_country(iso: &str) -> Option<&'static str> {
    const ARABIC: &[&str] = &[
        "sa", "ae", "eg", "jo", "kw", "qa", "bh", "om", "lb", "iq", "sy", "ye", "ly", "dz", "ma", "tn", "sd", "ps",
    ];
    let iso = iso.trim().to_ascii_lowercase();
    match iso.as_str() {
        "th" => Some(OTHER_LANG_STT[0]),
        "ru" | "by" | "kz" | "kg" => Some(OTHER_LANG_STT[1]),
        "ua" => Some("uk-UA"),
        c if ARABIC.contains(&c) => Some(OTHER_LANG_STT[2]),
        "in" => Some(OTHER_LANG_STT[3]),
        "bd" => Some(OTHER_LANG_STT[4]),
        "it" | "sm" | "va" => Some("it-IT"),
        "id" => Some("id-ID"),
        "vn" => Some("vi-VN"),
        "tr" => Some("tr-TR"),
        "nl" => Some("nl-NL"),
        "pl" => Some("pl-PL"),
        "ro" | "md" => Some("ro-RO"),
        _ => None,
    }
}

/// Which non-Latin script language `text` is written in, by its letters:
/// Thai, Cyrillic (Ukrainian or Russian), Arabic, Devanagari (Hindi), Bengali,
/// Tamil, or Telugu — the one with the most letters and at least
/// [`OTHER_SCRIPT_MIN_LETTERS`] of them (one stray word of a speech-to-text slip,
/// 「सात」 for 「七」, is not enough). None for anything else.
pub fn other_script_lang(text: &str) -> Option<&'static str> {
    let mut counts = [0usize; 7];
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        let i = match c {
            '\u{0E00}'..='\u{0E7F}' => 0,
            '\u{0400}'..='\u{04FF}' => 1,
            '\u{0600}'..='\u{06FF}' | '\u{0750}'..='\u{077F}' => 2,
            '\u{0900}'..='\u{097F}' => 3,
            '\u{0980}'..='\u{09FF}' => 4,
            '\u{0B80}'..='\u{0BFF}' => 5,
            '\u{0C00}'..='\u{0C7F}' => 6,
            _ => continue,
        };
        counts[i] += 1;
    }
    let (i, n) = counts.iter().copied().enumerate().max_by_key(|&(_, n)| n)?;
    if n < OTHER_SCRIPT_MIN_LETTERS {
        return None;
    }
    if i == 1 && has_ukrainian_letters(text) {
        return Some("uk-UA");
    }
    Some(OTHER_LANG_STT[i])
}

/// Fewest letters of one script for [`other_script_lang`].
const OTHER_SCRIPT_MIN_LETTERS: usize = 6;

/// The call was held in a language the AI has no setting for: its dialogue
/// tells no language of the nine ([`dialogue_lang`] is None) and is either written
/// in Latin letters of a language without a setting (Italian, Turkish…,
/// [`crate::urgency::unlisted_language`]) or in Thai, Cyrillic, Arabic or
/// Devanagari ([`other_script_lang`]).
pub(crate) fn dialogue_in_other_language(dialogue: &str, owner: crate::locale::Lang) -> bool {
    match dialogue_verdict(dialogue, owner) {
        DialogueVerdict::Lang(_) => false,
        DialogueVerdict::Unlisted => true,
        DialogueVerdict::Unknown => other_script_lang(dialogue).is_some(),
    }
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
/// AI holds its first words until it has played. There is no setting for it:
///
/// - With `sim_country` (the SIM the call came in on, sent by the phone app):
///   only on a SIM whose carrier plays the warning, and only for a number of another
///   country than that SIM's, read the way the SIM reads it (`+…`, `00…`;
///   [`crate::phone_region::normalize`]). A US SIM never gets it, so a caller from
///   the US — or from anywhere — hears 「喂？」 at once.
/// - SIM unknown (phone app older than 0.2.5): `zh_tw` (the language is
///   Traditional Chinese, so the SIM is assumed to be Taiwan's) and a caller from abroad.
/// - A number with no digit at all (hidden, private, unknown) does not say where the
///   call comes from, and the carrier plays its warning to such calls too: they wait
///   on a SIM that plays it (or, with the SIM unknown, for `zh_tw`). A number with
///   digits that reads as no country (a short code) is judged as above.
pub fn expects_carrier_intl_prompt(
    zh_tw: bool,
    caller_id: &str,
    sim_country: Option<&str>,
) -> bool {
    let hidden = !caller_id.chars().any(|c| c.is_ascii_digit());
    match sim_country.map(str::trim).filter(|c| !c.is_empty()) {
        Some(sim) => {
            let sim = sim.to_ascii_lowercase();
            CARRIER_INTL_PROMPT_SIMS.contains(&sim.as_str())
                && (hidden
                    || crate::phone_region::normalize(caller_id, Some(sim.as_str()))
                        .is_some_and(|n| n.country != sim.as_str()))
        }
        None => zh_tw && (hidden || is_intl_caller(caller_id)),
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

/// `v` with its output transcription's text replaced by `text`.
fn with_output_transcription(v: &Value, text: &str) -> Value {
    let mut c = v.clone();
    for path in ["/serverContent/outputTranscription/text", "/outputTranscription/text"] {
        if let Some(t) = c.pointer_mut(path) {
            *t = json!(text);
        }
    }
    c
}

/// Written in a script one of the AI's languages uses (Latin, Han, kana,
/// Hangul), or punctuation and spaces that go with them.
fn ai_script(c: char) -> bool {
    c.is_ascii()
        || c.is_whitespace()
        || matches!(c,
            '\u{00A0}'..='\u{024F}'
            | '\u{1100}'..='\u{11FF}'
            | '\u{1E00}'..='\u{1EFF}'
            | '\u{2000}'..='\u{206F}'
            | '\u{3000}'..='\u{30FF}'
            | '\u{3130}'..='\u{318F}'
            | '\u{31F0}'..='\u{31FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{AC00}'..='\u{D7AF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FFEF}')
}

/// A letter of a script none of the AI's languages is written in
/// (Devanagari, Thai, Cyrillic, Arabic…).
fn other_script(c: char) -> bool {
    c.is_alphabetic() && !ai_script(c)
}

/// The AI's own words without what is in a script none of its languages use
/// (a speech-to-text slip: 「七」 came back as 「सात」); None when nothing is left.
fn without_other_script(text: &str) -> Option<String> {
    let lead = text.starts_with(char::is_whitespace);
    let kept: String = text.chars().filter(|c| ai_script(*c)).collect();
    let kept = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    if kept.is_empty() {
        return None;
    }
    Some(if lead { format!(" {kept}") } else { kept })
}

/// The opening instruction; `opening` = the call's opening language when it is
/// not the setting's ([`call_lang_plan`]). `from_abroad`: the number is from
/// another country than the SIM that took the call — only then does the
/// instruction say so (a US number on a US SIM set to English is a local call).
fn persona_kick_client_content(opening: Option<crate::locale::Lang>, from_abroad: bool) -> String {
    persona_kick_with_local(opening, None, from_abroad)
}

/// [persona_kick_client_content], or the local way of answering when the call has
/// no opening language ([`CallLangPlan::local_hello`]).
fn persona_kick_with_local(
    opening: Option<crate::locale::Lang>,
    local: Option<&crate::prompts::LocalHello>,
    from_abroad: bool,
) -> String {
    let active = crate::locale::active();
    let text = match (opening, local) {
        (None, Some(h)) => crate::prompts::kick_open_local(active.lang, active.assistant, h, from_abroad),
        _ => crate::prompts::kick_open_call(active.lang, active.assistant, opening, from_abroad),
    };
    crate::prompts::client_turn(&text)
}

/// Whether the opening line calls the number foreign: only with an opening in
/// another language than the setting's, and only for a number of another
/// country than the SIM's ([`CallLangPlan::sim_local`]). SIM or number unknown:
/// `carrier_warning` (the carrier's international-call warning is due).
fn opening_from_abroad(
    opening: Option<crate::locale::Lang>,
    sim_local: Option<bool>,
    carrier_warning: impl FnOnce() -> bool,
) -> bool {
    opening.is_some() && sim_local.map_or_else(carrier_warning, |local| !local)
}

/// The AI's own call-back (an appointment's answer): nothing is asked for while
/// it rings (Gemini hears silence). Sent once the callee has picked up and
/// spoken ([`CallbackFloor::pickup`]), and again if the model stays quiet.
fn callback_kick_client_content() -> String {
    crate::prompts::client_turn(
        "[System note, not said by the other person] They have picked up and spoken (their words did not reach you). Start the call now as your instructions say: who is calling and why, the answer, then one goodbye — all of it, once.",
    )
}

/// Told to the model when a reply of its was dropped unheard
/// ([`FloorStep::Drop`]), so it does not count it as said: before the answer
/// reached them it says it all once they speak; after, it says nothing more
/// unless they speak. `turnComplete` false: it asks for no reply now.
fn callback_unheard_client_content(answered: bool) -> String {
    let text = if answered {
        "[System note, not said by the other person] They did not hear what you just said: nobody was speaking to you. Say nothing more unless they speak to you."
    } else {
        "[System note, not said by the other person] They did not hear what you just said: the line was still ringing or quiet. Say nothing now; once they have picked up and spoken, say all of it, as your instructions say."
    };
    json!({
        "clientContent": {
            "turns": [{ "role": "user", "parts": [{ "text": text }] }],
            "turnComplete": false
        }
    })
    .to_string()
}

/// A call-back nobody speaks on this long (it rings, or the line stays
/// silent) is ended: no answer.
const CALLBACK_RING_MAX: Duration = Duration::from_secs(70);

/// AI call-back: the callee counts as having spoken (the AI may answer) once
/// this much of their voice is on the line, in runs of at least
/// `LOCAL_MIN_VOICE_MS` (a click or a breath is none)…
const CALLBACK_CALLEE_VOICE_MS: u32 = 400;
/// Before pickup there is no transcript yet, and a crisp 「喂？」 or "Hello?" is
/// often ~250–300 ms followed by silence; ringback tones are filtered separately.
const CALLBACK_PICKUP_VOICE_MS: u32 = LOCAL_MIN_VOICE_MS;
/// …or this much when their words ask to hear it again (「什麼？」, "Sorry?").
const CALLBACK_CALLEE_ASK_VOICE_MS: u32 = LOCAL_MIN_VOICE_MS;
/// Their voice and their words count together only this close: a quiet line
/// this long starts over (a ringing tone, the line goes quiet, then a stray
/// transcript is no one speaking).
const CALLBACK_CALLEE_QUIET_RESET_MS: u32 = 2_500;
/// A reply the model generated before the callee had spoken waits this long
/// after its end for their words (the transcript may come after the reply),
/// then it is dropped unheard.
const CALLBACK_HELD_GRACE: Duration = Duration::from_millis(1_500);
/// Once the AI has given the answer (it ends on a goodbye), the line quiet this
/// long on both sides ends the call.
const CALLBACK_DONE_QUIET: Duration = Duration::from_secs(8);
/// Replies of the AI that reach the line on one call-back, the answer included;
/// the call ends a moment after the last one.
const CALLBACK_REPLIES_MAX: u32 = 4;
/// Quiet line after that last reply before the call ends.
const CALLBACK_LAST_REPLY_QUIET: Duration = Duration::from_secs(1);
/// Model audio held for a call-back reply, in seconds of speech (the answer
/// runs ~10–15 s and is generated faster than it is spoken).
const CALLBACK_HOLD_SECS: usize = 30;
/// Before the callee picks up, voice counts as theirs only once they pause
/// this long (the AI does not start over their 「喂？你好」)…
const CALLBACK_PICKUP_PAUSE_MS: u32 = 600;
/// …or after this much of it.
const CALLBACK_PICKUP_TALK_MS: u32 = 3_000;
/// Share of a frame's energy between 350 and 520 Hz that makes it a ringing
/// tone: ring-back tones are 400–480 Hz everywhere (440+480 Hz in Taiwan and
/// the US, 425 in Europe, 450 in China, 400 in Japan) and louder than a caller
/// on the line; speech spreads over far more of the spectrum.
const RINGBACK_BAND_SHARE: f64 = 0.6;

/// A 16 kHz frame that is a ringing tone rather than a voice
/// ([`RINGBACK_BAND_SHARE`], from the frame's own DFT bins).
fn ringback_like(pcm16: &[u8]) -> bool {
    let x: Vec<f64> = pcm16
        .chunks_exact(2)
        .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])))
        .collect();
    let n = x.len();
    let total: f64 = x.iter().map(|s| s * s).sum();
    if n < 160 || total <= 0.0 {
        return false;
    }
    let bins = |hz: f64| hz * n as f64 / 16_000.0;
    let mut band = 0.0;
    for k in bins(350.0).ceil() as usize..=bins(520.0).floor() as usize {
        let coeff = 2.0 * (2.0 * std::f64::consts::PI * k as f64 / n as f64).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in &x {
            let s0 = v + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        band += s1 * s1 + s2 * s2 - coeff * s1 * s2;
    }
    2.0 * band / (n as f64 * total) >= RINGBACK_BAND_SHARE
}

/// What a ringing or quiet line comes out as in the callee's transcript with
/// nobody speaking: the words of the Taiwan carrier's international-call warning
/// (they were vocabulary on the caller's side, and a silent call-back line came
/// out as 「國際電話」 twice — the AI gave its answer twice) and subtitle lines
/// speech-to-text makes up on silence. Longest first.
const CALLBACK_NOT_SPEECH: &[&str] = &[
    "謝謝觀看", "感謝觀看", "國際電話", "國際來電", "國外來電", "這是", "此為", "請您", "請", "注意", "小心", "詐騙",
    "接通", "掛斷", "字幕",
];

/// The callee's transcript has words of theirs in it: something is left once
/// STT markers (`<noise>`) and the mishearings of a quiet line
/// ([`CALLBACK_NOT_SPEECH`]) are taken out. A lone 「喂」 counts.
pub(crate) fn callee_said_words(text: &str) -> bool {
    let mut t: String = crate::zh_tw::norm(&strip_stt_markers(text))
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    for w in CALLBACK_NOT_SPEECH {
        t = t.replace(w, "");
    }
    !t.is_empty()
}

/// The callee asks to hear it again (「什麼？」「再說一次」, "Sorry?", "What?").
fn callee_asks_again(text: &str) -> bool {
    let t = crate::zh_tw::norm(text).to_lowercase();
    [
        "什麼", "蛤", "啥", "再說", "再講", "沒聽", "聽不", "what", "sorry", "pardon", "again", "repeat",
        "もう一度", "なんて", "뭐라고", "다시", "qué", "perdón", "repite", "quoi", "comment", "répét",
        "vous pouvez répéter", "wie bitte", "nochmal", "noch mal", "o quê", "desculpa", "repete",
        "come scusi", "come prego", "come ha detto", "come?", "può ripetere", "non ho capito",
        "sao ạ", "nói lại", "không nghe rõ", "gimana", "bisa ulangi", "kurang jelas",
        "อะไรนะ", "พูดอีกที", "ไม่ได้ยิน",
    ]
    .iter()
    .any(|w| t.contains(w))
}

/// AI call-back: what the callee has said since the AI's last reply began —
/// their voice on the line (local VAD frames) and their transcript — and
/// whether that is someone really speaking ([`Self::spoke`]).
#[derive(Debug, Default, Clone)]
struct CalleeHeard {
    /// Current run of voice, and the quiet line since its last voiced frame.
    run_ms: u32,
    quiet_ms: u32,
    /// Voice in runs of at least `LOCAL_MIN_VOICE_MS`.
    voice_ms: u32,
    words: String,
    /// They spoke: kept until the AI's next reply begins (the reply may come
    /// after a pause).
    spoke: bool,
    /// Before pickup Gemini hears nothing, so there is no transcript: their
    /// voice alone counts (ringing tones are not voice: [`ringback_like`]).
    voice_only: bool,
}

impl CalleeHeard {
    /// One uplink frame: `voiced` = their voice (our own echo is not).
    fn frame(&mut self, voiced: bool, frame_ms: u32) {
        if voiced {
            self.quiet_ms = 0;
            let before = self.run_ms;
            self.run_ms = self.run_ms.saturating_add(frame_ms);
            if self.run_ms >= LOCAL_MIN_VOICE_MS {
                let add = if before < LOCAL_MIN_VOICE_MS { self.run_ms } else { frame_ms };
                self.voice_ms = self.voice_ms.saturating_add(add);
            }
        } else {
            self.quiet_ms = self.quiet_ms.saturating_add(frame_ms);
            if self.quiet_ms >= LOCAL_PRE_ACTIVITY_HANGOVER_MS {
                self.run_ms = 0;
            }
            if self.quiet_ms >= CALLBACK_CALLEE_QUIET_RESET_MS && !self.spoke {
                self.voice_ms = 0;
                self.words.clear();
            }
        }
        self.check();
    }

    /// A piece of their transcript.
    fn words(&mut self, piece: &str) {
        push_tx_window(&mut self.words, piece, 400);
        self.check();
    }

    /// Their voice and their words, both: [`CALLBACK_CALLEE_VOICE_MS`] of voice
    /// (less when they ask to hear it again) and words that are not a quiet
    /// line's mishearing ([`callee_said_words`]).
    fn check(&mut self) {
        if self.spoke {
            return;
        }
        if self.voice_only {
            self.spoke = self.voice_ms >= CALLBACK_PICKUP_VOICE_MS;
            return;
        }
        if !callee_said_words(&self.words) {
            return;
        }
        let need = if callee_asks_again(&self.words) {
            CALLBACK_CALLEE_ASK_VOICE_MS
        } else {
            CALLBACK_CALLEE_VOICE_MS
        };
        self.spoke = self.voice_ms >= need;
    }

    fn spoke(&self) -> bool {
        self.spoke
    }

    /// They have picked up: spoken, then paused (or talked on long enough).
    fn picked_up(&self) -> bool {
        self.spoke && (self.quiet_ms >= CALLBACK_PICKUP_PAUSE_MS || self.voice_ms >= CALLBACK_PICKUP_TALK_MS)
    }
}

/// AI call-back. Until the callee picks up and speaks, Gemini hears silence and
/// no caller turn opens ([`Self::line_audio`], [`Self::turn_may_open`]): a
/// ringing tone or a quiet line cannot make the model talk. Their voice — not a
/// ringing tone — then sends the model its cue ([`Self::pickup`]). After that a
/// reply of the model's reaches the line only when the callee has really spoken
/// (voice and words, [`CalleeHeard`]) since the AI's last reply began; until
/// then its audio is held, and dropped — the model told so — if their words do
/// not come. At most [`CALLBACK_REPLIES_MAX`] replies reach the line.
#[derive(Debug, Default)]
struct CallbackFloor {
    callee: CalleeHeard,
    /// The callee has picked up and spoken: the line reaches Gemini.
    picked: bool,
    /// The model's current reply is on the line.
    on_line: bool,
    /// A reply held off the line: `Some(None)` while it is generated,
    /// `Some(Some(t))` once it ended at `t`.
    held: Option<Option<tokio::time::Instant>>,
    /// Replies that reached the line (the first one is the answer).
    replies: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FloorStep {
    Wait,
    /// The callee spoke: the held reply plays (`over`: its generation has ended).
    Play { over: bool },
    /// Their words did not come: the held reply is dropped unheard, and the
    /// model is told so (`note`, [`callback_unheard_client_content`]).
    Drop { note: String },
}

impl CallbackFloor {
    /// A call-back's floor at dialing: waiting for the callee to pick up.
    fn for_pickup() -> Self {
        Self { callee: CalleeHeard { voice_only: true, ..Default::default() }, ..Default::default() }
    }

    /// One uplink frame (16 kHz): `voiced` = the local VAD heard voice there.
    /// Before pickup a ringing tone is no voice.
    fn line_frame(&mut self, pcm16: &[u8], voiced: bool, frame_ms: u32) {
        let voiced = voiced && (self.picked || !ringback_like(pcm16));
        self.callee.frame(voiced, frame_ms);
    }

    /// Gemini gets this frame's real audio (else silence): only once the callee
    /// has picked up, and inside their turn.
    fn line_audio(&self, activity_open: bool) -> bool {
        self.picked && activity_open
    }

    /// The local VAD may open a caller turn: not before pickup.
    fn turn_may_open(&self) -> bool {
        self.picked
    }

    /// The callee has just picked up and spoken: the cue that has the model say
    /// the answer (once).
    fn pickup(&mut self) -> Option<String> {
        if self.picked || !self.callee.picked_up() {
            return None;
        }
        self.picked = true;
        Some(callback_kick_client_content())
    }

    fn drop_step(&self) -> FloorStep {
        FloorStep::Drop { note: callback_unheard_client_content(self.replies > 0) }
    }

    fn may_reply(&self) -> bool {
        self.picked && self.replies < CALLBACK_REPLIES_MAX && self.callee.spoke()
    }

    fn start_reply(&mut self) {
        self.on_line = true;
        self.replies += 1;
        self.callee = CalleeHeard::default();
    }

    /// Model audio came: true = it plays now, false = it is held.
    fn model_audio(&mut self) -> bool {
        if self.on_line {
            return true;
        }
        if self.held.is_none() && self.may_reply() {
            self.start_reply();
            return true;
        }
        self.held = Some(None);
        false
    }

    /// The model's turn is complete.
    fn turn_over(&mut self, now: tokio::time::Instant) {
        if self.on_line {
            self.on_line = false;
        } else if self.held == Some(None) {
            self.held = Some(Some(now));
        }
    }

    /// The line talked over the model (`interrupted`): its reply is over; a held
    /// one answered something earlier and is dropped (true).
    fn interrupted(&mut self) -> bool {
        self.on_line = false;
        self.held.take().is_some()
    }

    /// Each pass of the call loop: play or drop what is held.
    fn step(&mut self, now: tokio::time::Instant) -> FloorStep {
        let Some(ended) = self.held else {
            return FloorStep::Wait;
        };
        if self.may_reply() {
            self.held = None;
            self.start_reply();
            self.on_line = ended.is_none();
            return FloorStep::Play { over: ended.is_some() };
        }
        let stale = ended.is_some_and(|t| {
            self.replies >= CALLBACK_REPLIES_MAX || now.saturating_duration_since(t) >= CALLBACK_HELD_GRACE
        });
        if stale {
            self.held = None;
            return self.drop_step();
        }
        FloorStep::Wait
    }

    /// How long the line must stay quiet (both sides) before the call-back ends:
    /// none before the answer has been given.
    fn done_after(&self) -> Option<Duration> {
        match self.replies {
            0 => None,
            n if n >= CALLBACK_REPLIES_MAX => Some(CALLBACK_LAST_REPLY_QUIET),
            _ => Some(CALLBACK_DONE_QUIET),
        }
    }
}

/// Take the AI's lines written after `mark` back out of the transcript (a
/// reply held off the line and dropped); the caller's lines stay. Returns what
/// was removed.
fn drop_ai_lines_after(text: &mut String, mark: usize) -> String {
    if mark >= text.len() || !text.is_char_boundary(mark) {
        return String::new();
    }
    let tail = text.split_off(mark);
    let mut dropped: Vec<&str> = Vec::new();
    for line in tail.split('\n').filter(|l| !l.trim().is_empty()) {
        if let Some(said) = line.strip_prefix("接聽：") {
            dropped.push(said);
            continue;
        }
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(line);
    }
    dropped.concat()
}

/// Common short words of the Latin-script languages the AI speaks: which one a
/// line is in.
const LATIN_LANG_WORDS: [(crate::locale::Lang, &[&str]); 5] = [
    (crate::locale::Lang::En, &[
        "the", "and", "is", "are", "you", "your", "i", "i'm", "it", "to", "of", "for", "this", "that", "what",
        "yes", "hello", "thanks", "thank", "okay", "bye", "goodbye", "will", "can", "at", "we", "be", "not",
        "with", "on", "see", "about", "calling",
    ]),
    (crate::locale::Lang::Es, &[
        "el", "la", "los", "las", "que", "de", "y", "es", "usted", "para", "por", "con", "hola", "gracias",
        "sí", "está", "muy", "bien", "pero", "un", "una", "adiós", "llamo",
    ]),
    (crate::locale::Lang::Fr, &[
        "le", "la", "les", "de", "des", "et", "est", "vous", "je", "pas", "que", "un", "une", "bonjour",
        "merci", "oui", "c'est", "pour", "avec", "au", "revoir",
    ]),
    (crate::locale::Lang::De, &[
        "der", "die", "das", "und", "ist", "sie", "ich", "nicht", "ein", "eine", "zu", "mit", "danke", "ja",
        "bitte", "guten", "tag", "wir", "auf", "wiedersehen", "ihnen",
    ]),
    (crate::locale::Lang::Pt, &[
        "o", "os", "as", "que", "de", "e", "é", "você", "para", "com", "não", "um", "uma", "olá", "obrigado",
        "obrigada", "sim", "está", "bom", "tchau",
    ]),
];

/// The language a line of the dialogue is in, by its letters, and how much of
/// it there is: kana (Japanese, with its kanji), hangul (Korean), Chinese
/// characters (`zh`), or Latin words — which language by its common short
/// words, English when none stands out; a word weighs two characters. None for
/// a line too short to tell (「喂？」, 「好，掰掰」, "Hello?", "Okay, goodbye.").
fn line_lang(text: &str, zh: crate::locale::Lang) -> Option<(crate::locale::Lang, usize)> {
    let (mut han, mut kana, mut hangul) = (0usize, 0usize, 0usize);
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        let latin = c.is_ascii_alphabetic() || ('\u{00c0}'..='\u{024f}').contains(&c);
        if latin || (c == '\'' && !word.is_empty()) {
            word.extend(c.to_lowercase());
            continue;
        }
        if !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        if ('\u{3040}'..='\u{30ff}').contains(&c) {
            kana += 1;
        } else if ('\u{ac00}'..='\u{d7af}').contains(&c) || ('\u{3130}'..='\u{318f}').contains(&c) {
            hangul += 1;
        } else if ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c) {
            han += 1;
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    let latin = if words.len() >= 3 { words.len() * 2 } else { 0 };
    let cjk = |n: usize| if n >= 4 { n } else { 0 };
    // Kana make a line Japanese only as a fair share of its characters (a stray
    // kana or two that speech-to-text wrote into Chinese leaves it Chinese).
    let ja = kana >= 2 && kana * 4 >= kana + han;
    let japanese = if ja { cjk(kana + han) } else { 0 };
    let chinese = if ja { 0 } else { cjk(han) };
    let korean = cjk(hangul);
    let latin_lang = || {
        LATIN_LANG_WORDS
            .iter()
            .map(|(l, list)| (*l, words.iter().filter(|w| list.contains(&w.trim_matches('\''))).count()))
            .fold((crate::locale::Lang::En, 0), |best, (l, n)| if n > best.1 { (l, n) } else { best })
            .0
    };
    [(crate::locale::Lang::Ja, japanese), (crate::locale::Lang::Ko, korean), (zh, chinese), (crate::locale::Lang::En, latin)]
        .into_iter()
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
        .map(|(l, n)| if l == crate::locale::Lang::En { (latin_lang(), n) } else { (l, n) })
}

/// The language a call was held in, from its dialogue (`對方：` / `接聽：` lines,
/// [`merge_dialogue_fragments`]): what the AI said — it answers in the caller's
/// language, whatever its opening line was — or, when no line of the AI's tells,
/// what the caller said. The language with the most said in it ([`line_lang`]);
/// None when no line tells. Chinese is the owner's variant when `owner` is
/// Chinese, else Traditional. None too when the side that decides spoke a
/// language without a setting ([`dialogue_in_other_language`]): Latin letters
/// alone would otherwise pass as English.
pub(crate) fn dialogue_lang(dialogue: &str, owner: crate::locale::Lang) -> Option<crate::locale::Lang> {
    match dialogue_verdict(dialogue, owner) {
        DialogueVerdict::Lang(l) => Some(l),
        _ => None,
    }
}

enum DialogueVerdict {
    Lang(crate::locale::Lang),
    /// The deciding side spoke Latin letters of a language without a setting.
    Unlisted,
    Unknown,
}

fn dialogue_verdict(dialogue: &str, owner: crate::locale::Lang) -> DialogueVerdict {
    let zh = if owner.is_zh() { owner } else { crate::locale::Lang::ZhTw };
    let side = |label: &str| {
        let mut totals: Vec<(crate::locale::Lang, usize)> = Vec::new();
        let texts: Vec<&str> = dialogue.lines().filter_map(|line| line.trim().strip_prefix(label)).collect();
        for (l, n) in texts.iter().filter_map(|text| line_lang(text, zh)) {
            match totals.iter_mut().find(|(x, _)| *x == l) {
                Some(t) => t.1 += n,
                None => totals.push((l, n)),
            }
        }
        let unlisted = crate::urgency::unlisted_language(&texts.join(" "));
        (unlisted, totals.into_iter().max_by_key(|(_, n)| *n).map(|(l, _)| l))
    };
    for label in ["接聽：", "對方："] {
        match side(label) {
            (true, _) => return DialogueVerdict::Unlisted,
            (false, Some(l)) => return DialogueVerdict::Lang(l),
            (false, None) => {}
        }
    }
    DialogueVerdict::Unknown
}

/// The languages of an AI call-back: the one spoken on the call it answers
/// (`spoken`, [`dialogue_lang`]) for the AI, and with English for the callee's
/// speech-to-text — whatever the number or the SIM would suggest (a caller who
/// spoke Chinese from a US number was called back in English).
pub fn callback_lang_plan(spoken: crate::locale::Lang, setting: crate::locale::Lang) -> CallLangPlan {
    CallLangPlan {
        opening: Some(spoken).filter(|l| *l != setting),
        caller_stt: Some(stt_codes(&[spoken, crate::locale::Lang::En])),
        ..Default::default()
    }
}

/// What a Live session is for, as far as the calls the phone dials go
/// ([`run_session`]'s `callback` and `errand`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outbound {
    /// A call that came in.
    Incoming,
    /// An appointment's call-back: the owner's answer, said once.
    CallBack,
    /// An errand ([`crate::errands`]): a conversation the AI holds for the owner.
    Errand,
}

impl Outbound {
    fn of(callback: bool, errand: bool) -> Self {
        match (callback, errand) {
            (false, _) => Self::Incoming,
            (true, false) => Self::CallBack,
            (true, true) => Self::Errand,
        }
    }

    /// The answer said once ([`CallbackFloor`]): Gemini hears nothing until the
    /// callee's voice (not a ringing tone) has picked up, each reply waits for
    /// the callee to have spoken, at most [`CALLBACK_REPLIES_MAX`] replies, and a
    /// quiet line after the answer ends the call ([`CALLBACK_DONE_QUIET`]). An
    /// appointment's call-back only: an errand talks for as many turns as it
    /// takes, waits for the owner's answers (`ask_owner`), hears a voice menu
    /// or the carrier's recording from its first word, and leaves a message on
    /// voicemail.
    fn answers_once(self) -> bool {
        self == Self::CallBack
    }

    /// Speech-to-text for the other side: a call-back's callee without any
    /// vocabulary ([`stt_callee_config`]); an errand's callee with the call's
    /// own languages and the owner's name ([`stt_errand_config`], `opening` =
    /// [`CallLangPlan::opening`]); a caller with the caller's language hints
    /// and no vocabulary ([`stt_caller_config_for`]).
    fn other_side_stt(
        self,
        langs: Option<&[&'static str]>,
        opening: Option<crate::locale::Lang>,
        active: &crate::locale::Active,
    ) -> Value {
        self.other_side_stt_for(langs, opening, None, active)
    }

    fn other_side_stt_for(
        self,
        langs: Option<&[&'static str]>,
        opening: Option<crate::locale::Lang>,
        extra_other: Option<&'static str>,
        active: &crate::locale::Active,
    ) -> Value {
        match self {
            Self::CallBack => stt_callee_config(langs),
            Self::Errand => stt_errand_config_with(opening, extra_other, active),
            Self::Incoming => stt_caller_config_for(langs, active),
        }
    }
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

/// The limits of a call that stalling was released from (the check found an ordinary
/// call): the normal call's cap, but at least a minute past `elapsed_secs`, so a call
/// that already talked longer than that cap gets its wrap-up and a normal goodbye
/// instead of being cut at once.
fn released_talk_limits(persona: bool, persona_max_secs: u64, elapsed_secs: u64) -> (u64, u64, u64) {
    talk_limits(persona, persona_max_secs.max(elapsed_secs + 60))
}

/// Persona: caller speech after a closing this long is either a farewell or new
/// content — their words / the model's next turn decide which.
const PERSONA_POST_ARM_VOICE_MS: u32 = 600;

/// Persona hangs up only once the caller is leaving (they said bye first, or
/// have clearly wrapped up). The AI's own 掰掰 is not enough:
/// after it the line stays open until the caller says bye / wraps up, or stays
/// silent this long (they may be waiting for us to hang up first).
const PERSONA_BYE_SILENCE: Duration = Duration::from_secs(6);
/// Farewells exchanged on the caller's announced end (「係咁先啦」, then our
/// 「好，拜拜」): how long the line stays open for their own bye, which usually
/// follows at once. Quiet that long, the call ends.
const PERSONA_EXCHANGE_BYE_WAIT: Duration = Duration::from_millis(2_500);

/// The caller announced the end and the AI answered with its farewell
/// ([`crate::closing::ClosingEvent::FarewellsExchanged`]): the caller's latest
/// words close the call. Armed here too when the AI's farewell is one the
/// transcript check does not arm on (「我確認一下，晚點再打給你」) — not on an
/// appointment call-back (`may_arm` false), whose replies are held before they
/// play. True when something changed.
fn arm_on_farewells_exchanged(
    may_arm: bool,
    goodbye_pending: &mut bool,
    caller_end_signal: &mut bool,
    end_reason: &mut SessionEndReason,
) -> bool {
    if *goodbye_pending && *caller_end_signal {
        return false;
    }
    if !*goodbye_pending {
        if !may_arm {
            return false;
        }
        *goodbye_pending = true;
        *end_reason = SessionEndReason::Goodbye;
    }
    *caller_end_signal = true;
    true
}

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
    // 「掰／拜拜」 count only said as a goodbye (「我們在拜拜」「你不要瞎掰」 are not),
    // and not to someone else (「跟阿嬤說掰掰」).
    if zh_bye_said_to_us(text) {
        return true;
    }
    [
        "再見", "bye", "先這樣", "就這樣", "沒事了", "沒別的事", "不打擾", "先掛",
        "結束電話", "再聯絡", "再聯繫", "改天聊", "先不聊", "再跟你聯絡", "再跟您聯絡", "再跟你聯繫",
        "再跟您聯繫", "改天再聊", "下次再聊", "下次聊", "麻煩你了", "麻煩您了",
        // Cantonese (a +852 caller): 唔該晒、先咁啦、冇嘢喇.
        "唔該", "先咁", "冇嘢",
    ]
    .iter()
    .any(|m| near_end(m, 2))
        || ["祝您", "祝你"].iter().any(|m| near_end(m, 7))
}

/// A Chinese 「拜拜／掰掰／掰」 ends `text` as a goodbye ([`crate::closing::zh_bye_ends`]),
/// not one said to someone else ([`bye_to_someone_else`]).
fn zh_bye_said_to_us(text: &str) -> bool {
    crate::closing::zh_bye_ends(text).is_some_and(|before| {
        let letters: String = before.chars().filter(|c| c.is_alphanumeric()).collect();
        !bye_to_someone_else(&letters)
    })
}

/// A Chinese goodbye ends `text` or one of its clauses (「好，掰掰，下週見」):
/// the words up to each punctuation mark are tried as the end.
fn zh_bye_in(text: &str) -> bool {
    if !text.contains('掰') && !text.contains("拜拜") {
        return false;
    }
    text.char_indices()
        .filter(|(_, c)| "，。！？、；：,.!?;:~～…\n".contains(*c))
        .map(|(i, _)| &text[..i])
        .chain(std::iter::once(text))
        .any(|p| crate::closing::zh_bye_ends(p).is_some())
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
        // 「掰」 alone is a common short sign-off (looks_like_goodbye reads it
        // as one when said as a goodbye); 「拜」 alone is not (拜託).
        || ["先這樣", "先这样", "先掛", "先挂", "先不聊", "不聊了"]
            .iter()
            .any(|m| text.contains(m))
}

/// Speech-to-text config for the caller's side: `langs` as language hints
/// ([`CallLangPlan::caller_stt`]; `None` = it detects the language itself) and
/// no vocabulary.
fn stt_caller_config(langs: Option<&[&'static str]>) -> Value {
    stt_caller_config_for(langs, &crate::locale::active())
}

/// [`stt_caller_config`] for given settings: the language hints only. The
/// owner's name and the call logic's key words (保險, 管理室, 包裹, 國際電話…) used
/// to go in as vocabulary. A caller on a local number may speak any language,
/// and the vocabulary pulled what they said into the owner's: measured on
/// phone-quality audio, a Turkish courier's sentence came out as 「你好。我 Cargo
/// 管理室。包裹。」 and a short Vietnamese one as Chinese, with the same hints
/// and no vocabulary both came out right. The Chinese key words and the
/// carrier's 「這是國際電話，請注意小心詐騙」 were written right without it too,
/// also on a noisy line. A single word did it as well (a caller's "Alo" came
/// out as the owner's name), so nothing is sent.
fn stt_caller_config_for(langs: Option<&[&'static str]>, _active: &crate::locale::Active) -> Value {
    match langs.filter(|l| !l.is_empty()) {
        Some(l) => json!({ "languageCodes": l }),
        None => json!({}),
    }
}

/// Speech-to-text config for the callee of an AI call-back: language hints
/// only. No vocabulary: 「國際電話」 (there for the carrier's warning on calls
/// from abroad, which a call-back never gets) is what a silent call-back line
/// came out as, and the AI answered it.
fn stt_callee_config(langs: Option<&[&'static str]>) -> Value {
    match langs.filter(|l| !l.is_empty()) {
        Some(l) => json!({ "languageCodes": l }),
        None => json!({}),
    }
}

/// Speech-to-text config for the other side of an errand: the language hints
/// are the one the AI opens in (`opening`, else the setting's) and the one the
/// owner reads, once each; the owner's name is vocabulary only when the hints
/// include the setting's language. No carrier word and none of the call logic's
/// key words: an errand is dialled out, so no carrier warning plays on it, and
/// 「國際電話」 is what a callee's English sentence came out as on a real
/// errand, which the AI then answered in Chinese.
fn stt_errand_config(opening: Option<crate::locale::Lang>, active: &crate::locale::Active) -> Value {
    stt_errand_config_with(opening, None, active)
}

fn stt_errand_config_with(
    opening: Option<crate::locale::Lang>,
    extra_other: Option<&'static str>,
    active: &crate::locale::Active,
) -> Value {
    let mut langs = stt_codes(&[opening.unwrap_or(active.lang), active.notify_lang]);
    if let Some(code) = extra_other {
        if !langs.contains(&code) {
            langs.push(code);
        }
    }
    let mut cfg = json!({ "languageCodes": langs });
    let owner = active.owner.trim();
    if !owner.is_empty() && langs.contains(&crate::locale::phrases(active.lang).stt) {
        cfg["customVocabulary"] = json!([owner.chars().take(40).collect::<String>()]);
    }
    cfg
}

/// Speech-to-text hints for the AI's own voice: the call's opening language
/// (`opening`, persona mode — [`CallLangPlan::opening`]) and the setting's, once
/// each. Fixed, whatever the caller's side does: left to detect the language
/// itself, a short 「掰掰」 of ours came out in Latin letters or as Korean, and
/// the AI's own goodbye went unnoticed.
/// `extra`: a language the AI has no setting for that the caller speaks
/// ([`CallLangPlan::caller_other_stt`]): the AI answers in it, and its goodbye in
/// Thai, Russian, Arabic or Hindi must come back in that script to be noticed.
fn stt_ai_config(opening: Option<crate::locale::Lang>, extra: Option<&'static str>) -> Value {
    json!({ "languageCodes": stt_ai_codes(opening, extra) })
}

/// The codes of [`stt_ai_config`].
fn stt_ai_codes(opening: Option<crate::locale::Lang>, extra: Option<&'static str>) -> Vec<&'static str> {
    let own = crate::locale::current();
    let langs: Vec<crate::locale::Lang> = opening.into_iter().chain([own]).collect();
    let mut codes = stt_codes(&langs);
    if let Some(e) = extra.filter(|e| !codes.contains(e)) {
        codes.push(e);
    }
    codes
}

/// `setup` with `codes` as the hints for the AI's own voice (a rotated session,
/// once the caller turned out to speak another language).
fn with_ai_stt_codes(setup: &Value, codes: &[&'static str]) -> Value {
    let mut s = setup.clone();
    if let Some(o) = s.pointer_mut("/setup").and_then(|x| x.as_object_mut()) {
        o.insert("outputAudioTranscription".into(), json!({ "languageCodes": codes }));
    }
    s
}

/// The caller turned out to speak Thai, Russian, Arabic or Hindi
/// ([`other_script_lang`]) and the AI's hints lack it: the session rotates at
/// the next pause to add it, at the latest this long after.
const AI_STT_ROTATE_FORCE: Duration = Duration::from_secs(30);

/// Live lookup of the caller's number and saved-contact label for `stall_review`.
#[derive(Clone)]
pub struct CallerContextFn(pub Arc<dyn Fn() -> crate::stall_review::CallerContext + Send + Sync>);

impl std::fmt::Debug for CallerContextFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CallerContextFn(..)")
    }
}

/// Callback invoked once when a Whitelist caller (`whitelist_mode == "emergency"`)
/// expresses an urgent / emergency matter during the call.
#[derive(Clone)]
pub struct UrgentTransferFn(pub Arc<dyn Fn(&str) + Send + Sync>);

impl std::fmt::Debug for UrgentTransferFn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UrgentTransferFn(..)")
    }
}

/// Whether a Whitelist caller's utterance expresses an urgent or emergency matter
/// (across all 9 supported languages): an emergency in context, not a bare word
/// (see [`crate::urgency`]).
pub fn is_urgent_whitelist_turn(text: &str) -> bool {
    crate::urgency::is_urgent(text)
}

/// Per-call sales / scam config (None = not persona mode, or an AI call-back).
#[derive(Debug, Clone)]
pub struct StallCfg {
    /// Set once the caller's words score as sales/scam (agent records the event).
    pub engaged: Arc<AtomicBool>,
    /// Start in Stall mode immediately from second 0 (caller is on the Force-Stall list).
    pub force_start: bool,
    /// [`StallPrompt::Full`] system prompt: the session rotates to it once engaged.
    pub system_prompt: String,
    /// Model of the rotated stall session (setting `gemini_stall_model`; empty =
    /// the call's model). E.g. normal calls on the extended-thinking
    /// model, sales / scam calls go on with `gemini-3.8-live`.
    pub model: String,
    /// Talk cap once engaged (setting `persona_stall_max_min`), replacing the
    /// normal-call cap.
    pub max_secs: u64,
    /// Caller's number and saved-contact label (read when `stall_review` runs).
    pub caller_context: Option<CallerContextFn>,
    /// Keep a sales / scam caller talking (setting `persona_stall_spam`). Off,
    /// the call is still judged and marked, and the AI goes on as normal.
    pub play_along: bool,
    /// Look at calls no keyword flagged once the caller has said what they
    /// want (setting `scam_review_without_keywords`).
    pub review_without_keywords: bool,
    /// How the call was judged (kind of call, confidence, let go again), for
    /// the call record.
    pub judgement: Arc<std::sync::Mutex<Option<crate::calls::ScamJudgement>>>,
    /// Fired once if a Whitelist caller (`whitelist_mode == "emergency"`) states an emergency.
    pub urgent_transfer: Option<UrgentTransferFn>,
    /// Whitelist urgent call (`whitelist_mode` emergency with the owner's number set): the agent pauses the
    /// AI while the phone dials the owner, ends the session once merged, or has the
    /// AI say the owner did not answer.
    pub urgent_call: Option<Arc<crate::urgent_call::CallCtl>>,
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

/// An errand call declares `ask_owner` ([`crate::errands::tool_declarations`];
/// blocking: the AI settles nothing while the owner is asked).
fn with_errand_tools(setup: &mut Value) {
    if let Some(s) = setup.get_mut("setup").and_then(|x| x.as_object_mut()) {
        s.insert("tools".into(), crate::errands::tool_declarations());
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

/// What the session rotation state becomes when stalling is released (`Release`):
/// - already rotated to the stall session (`live_is_stall`): build a normal
///   setup (`Some(...)`) even when `appt` is `None`, re-enabling appointment
///   tools when `appt` is `Some`, and ask for a rotation back;
/// - still on the normal session (`!live_is_stall`): clear any pending stall
///   `rotate_setup` and `rotate_due` so no stale rotation fires.
fn release_rotation(
    live_is_stall: bool,
    setup: &Value,
    base_prompt: &str,
    appt: Option<&crate::appointments::ApptSession>,
    now: chrono::DateTime<chrono::Local>,
    appt_block_sent: bool,
    non_blocking: bool,
) -> (Option<Value>, bool) {
    if !live_is_stall {
        return (None, false);
    }
    let mut s = setup_with_system_prompt(setup, &call_system_prompt(base_prompt, appt, now, appt_block_sent));
    with_appointment_tools(&mut s, appt.is_some(), non_blocking);
    (Some(s), true)
}

/// The call's system instruction: the stored prompt, then — appointments on —
/// their short part, or (`full`: times came up on the call) the note with the
/// dates and the function's details that went in then, at the current local
/// time (the stored prompt is not changed).
fn call_system_prompt(
    base: &str,
    appt: Option<&crate::appointments::ApptSession>,
    now: chrono::DateTime<chrono::Local>,
    full: bool,
) -> String {
    match appt {
        Some(a) if full => format!("{base}\n\n{}", a.prompt_note(now)),
        Some(a) => format!("{base}{}", a.prompt_stub()),
        None => base.to_string(),
    }
}

/// The system prompt a Live session gets. A call that came in keeps its own
/// prompt: it has the note to answer in the caller's language exactly when the
/// setting `reply_in_caller_language` is on (`prompts::build`), so switching it
/// off takes the note away. The calls the phone dials (an errand, a call-back)
/// get the note on every call, as before. Unchanged when the note is there already.
fn session_prompt(prompt: &str, outbound: Outbound) -> String {
    match outbound {
        Outbound::Incoming => prompt.to_string(),
        Outbound::CallBack | Outbound::Errand => {
            crate::prompts::with_caller_language_note(prompt, crate::locale::current())
        }
    }
}

/// The call can hear the Taiwan carriers' international-call warning, so its
/// prompt keeps the note on it: a number from abroad on a SIM whose carrier plays
/// it (as [`expects_carrier_intl_prompt`] reads the number), or the SIM not known.
fn carrier_note_due(plan: &CallLangPlan) -> bool {
    match plan.sim_country.as_deref() {
        Some(sim) => CARRIER_INTL_PROMPT_SIMS.contains(&sim) && plan.sim_local != Some(true),
        None => true,
    }
}

/// The `persona_prompt` log line of each Live session a call opens (the first,
/// a ring prewarm's, a rotation): the prompt's size and what it was scoped to.
struct PromptLog {
    sim: String,
    lang: &'static str,
    region: String,
    /// Characters of its persona text (the owner's own or the built-in one).
    user_chars: usize,
}

impl PromptLog {
    fn new(
        plan: &CallLangPlan,
        lang: crate::locale::Lang,
        region: Option<crate::country_profile::Region>,
        user_chars: usize,
    ) -> Self {
        Self {
            sim: plan.sim_country.as_deref().map_or_else(|| "-".to_string(), str::to_ascii_uppercase),
            lang: lang.code(),
            region: region.map_or_else(|| "-".to_string(), |r| format!("{r:?}")),
            user_chars,
        }
    }

    fn log(&self, chars: usize, rotate: bool) {
        info!(
            "persona_prompt chars={chars} user_chars={} sim_country={} lang={} region={} rotate={}",
            self.user_chars,
            self.sim,
            self.lang,
            self.region,
            u8::from(rotate)
        );
    }
}

/// The system prompt a setup carries.
fn setup_system_prompt(setup: &Value) -> &str {
    setup
        .pointer("/setup/systemInstruction/parts/0/text")
        .and_then(Value::as_str)
        .unwrap_or("")
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

/// Distinct sales/scam keywords in a caller text, strongest first (post-call
/// summary when the summary model is unavailable).
pub(crate) fn stall_keyword_hits(caller_text: &str) -> Vec<&'static str> {
    let mut hits = Vec::new();
    stall_score_add(caller_text, &mut hits);
    hits.sort_by_key(|h| std::cmp::Reverse(stall_keyword_weight(h)));
    hits
}

/// Add new distinct sales/scam keywords found in the caller's words (the scam
/// database's lists of every language, [`crate::scam_db::keyword_hits`]);
/// returns the score they add. `hits` accumulates across the whole call. A word
/// inside a longer one that also hit counts once, as the longer one: 免費 and
/// 試用 in 免費試用, "insurance" in "life insurance", 無料 in 無料体験.
fn stall_score_add(caller_text: &str, hits: &mut Vec<&'static str>) -> u32 {
    let found: Vec<(&'static str, u32)> = crate::scam_db::keyword_hits(caller_text)
        .into_iter()
        .map(|h| (h.text, h.weight))
        .collect();
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
#[cfg(test)]
fn has_keyword(text: &str, kw: &str) -> bool {
    crate::scam_db::han_contains(text, kw)
}

/// Weight of a sales / scam keyword (0: not one).
fn stall_keyword_weight(kw: &str) -> u32 {
    crate::scam_db::keyword(kw).map_or(0, |h| h.weight)
}

/// Enough distinct keywords (at least one a clear sales/scam word), or any
/// scam-only demand (`hard: true`: OTP, ATM steps, safe account, screen sharing,
/// marketplace escrow verification).
fn stall_engages(score: u32, hits: &[&'static str]) -> bool {
    hits.iter().any(|h| crate::scam_db::is_hard(h))
        || (score >= STALL_SCORE_THRESHOLD
            && hits.iter().any(|h| stall_keyword_weight(h) >= STALL_STRONG_WEIGHT))
}

/// Caller follows up on the owner's own matter (a complaint, an investigation
/// result, a repair; the scam database's phrases in every language): never
/// sales / scam stalling — e.g. a bank calling back about a report made at a
/// branch, where 信用卡／轉帳 would otherwise score as a pitch.
pub(crate) fn looks_like_own_matter(caller_text: &str) -> bool {
    crate::scam_db::own_matter(caller_text)
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
    if crate::closing::zh_bye_ends(text).is_some() {
        return true;
    }
    [
        "再見",
        "再见",
        "arrivederci",
        "buona giornata",
        "buongiornata",
        "buonanotte",
        "a presto",
        "a domani",
        "ci vediamo",
        "stammi bene",
        "grazie a lei",
    ]
    .iter()
    .any(|m| t.ends_with(m))
        || crate::locale::ends_with_farewell(text, 0)
}

/// Hang up only when the model's turn ENDS on a farewell —
/// 「先這樣算的話一個月多少？」 is not a goodbye (the plain closing check arms on it).
fn turn_ends_with_closing(text: &str) -> bool {
    let t = crate::locale::strip_trailing_thanks(text);
    let tail: Vec<char> = t.chars().rev().take(5).collect();
    let tail: String = tail.into_iter().rev().collect::<String>().to_ascii_lowercase();
    crate::closing::zh_bye_ends(&t).is_some()
        || ["再見", "再见", "不聊", "先掛", "先挂", "先這樣", "先这样"]
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

/// [persona_stall_keep_client_content] with the stalling engine's last bait
/// line (crate::stall_script) when the call has one: the caller is leaving or
/// the AI started a farewell, so the retain stage's cue goes with the nudge.
fn persona_stall_keep_with_retain(
    we_said_bye: bool,
    engine: &mut Option<crate::stall_script::StallEngine>,
    serial: &str,
) -> String {
    let Some(e) = engine.as_mut() else {
        return persona_stall_keep_client_content(we_said_bye);
    };
    let cue = e.take_retain();
    info!(
        "stall_stage stage={} tactic={} typology={} turn={} via=keep_nudge serial={serial}",
        cue.stage.name(),
        cue.tactic,
        e.typology(),
        cue.turn
    );
    let p = crate::prompts::current();
    let lead = if we_said_bye {
        &p.kick_stall_keep_lead_bye
    } else {
        &p.kick_stall_keep_lead
    };
    crate::prompts::client_turn(&format!("{} {}", p.kick_stall_keep.replace("{lead}", lead), cue.text))
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

/// Sales / scam: a second look ([`crate::stall_review`]) before the AI starts
/// keeping the caller busy, and one more once it has started. The AI talks on
/// meanwhile.
///
/// - Keywords crossed the line ([`StallReview::should_ask`]). A call reviewed as
///   normal is looked at again only once the score has grown by
///   [`STALL_REVIEW_STEP`], at most [`STALL_REVIEW_MAX`] such looks a call.
/// - No keywords ([`StallReview::should_ask_blind`]): a caller who is not a contact
///   is looked at once they have said what they want (their second turn with
///   something in it, or [`BLIND_FIRST_UNITS`] words) and again after
///   [`BLIND_NEXT_UNITS`] more, at most [`BLIND_MAX`] times. Scams in words no list has
///   (a new script, a language without keywords, a speech-to-text slip) were
///   never looked at before; a number marked or judged sales / scam before is
///   looked at after its first such turn.
/// - Too early (the caller has only said who they are) or a low-confidence
///   scam: looked at again once their turn has ended. The look takes about a
///   second, and by then the caller has often finished that turn ("…信用卡風險
///   管控中心。您的帳戶被誤設成分期付款，要請您現在去ATM解除分期。") and the AI is
///   answering: a look at a turn that ended, or got more keywords, while it was
///   being looked at is asked again at once, not a whole turn later. Such looks
///   do not count against the caps. The [`STALL_REVIEW_EARLY_MAX`]th "too early" of
///   a keyword-flagged call counts as a scam; a second low-confidence scam in a
///   row does when keywords flagged the call, and is let be when none did.
/// - No answer ([`Verdict::Unavailable`]): words only scammers say still stall the
///   caller; a contact or a number the owner marked safe is left alone; anything
///   else is looked at again after the caller's next turn. After
///   [`STALL_REVIEW_FAIL_MAX`] failures in a row (at once without a key) the keywords
///   decide alone, as before the review existed: Gemini being down must not let
///   an obvious scam through. Taking every failure for a scam kept ordinary
///   callers talking for a whole call.
/// - Stalling ([`StallReview::should_ask_confirm`]): one more look after the
///   caller's next [`CONFIRM_AFTER_TURNS`] turns (one, if long); a sure "normal" lets
///   the caller go back to an ordinary call ([`StallVerdictStep::Release`]).
const STALL_REVIEW_MAX: u32 = 3;
const STALL_REVIEW_STEP: u32 = 2;
/// The keywords trip mid-sentence: the first look may see half the
/// introduction, the next one the whole of it, a third the caller's matter.
const STALL_REVIEW_EARLY_MAX: u32 = 4;
/// Failed looks in a row before the keywords decide alone.
const STALL_REVIEW_FAIL_MAX: u32 = 2;
/// Looks at a call no keyword flagged.
const BLIND_MAX: u32 = 3;
/// The first of them: once the caller has had this many turns with something in them…
const BLIND_FIRST_TURNS: u32 = 2;
/// …or has said this much (see [`content_units`]).
const BLIND_FIRST_UNITS: usize = 24;
/// The next ones: once the caller has said this much more.
const BLIND_NEXT_UNITS: usize = 16;
/// A caller turn with less is a 「喂？」 or a 「嗯」, not something said.
const CONTENT_TURN_UNITS: usize = 4;
/// Stalling: the check whether to let the caller go comes after this many of their turns…
const CONFIRM_AFTER_TURNS: u32 = 2;
/// …or after one, when they have said this much since stalling started.
const CONFIRM_EARLY_UNITS: usize = 12;

/// Why a look was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum LookOrigin {
    #[default]
    Keywords,
    Blind,
    Confirm,
}

/// Why the call was judged sales / scam (recorded with it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum EngageWhy {
    /// The second look said so.
    #[default]
    Review,
    /// The look did not answer, and the caller said words only scammers say.
    HardWords,
    /// The look kept failing: the keywords decided alone.
    KeywordRule,
}

impl EngageWhy {
    fn as_str(self) -> &'static str {
        match self {
            EngageWhy::Review => "review",
            EngageWhy::HardWords => "hard_words",
            EngageWhy::KeywordRule => "keywords",
        }
    }
}

/// What the call looks like when a verdict comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct StallFacts {
    /// The keyword score crossed the line ([`stall_engages`]).
    flagged: bool,
    /// A keyword only scammers say was heard ([`crate::scam_db::is_hard`]).
    hard_hit: bool,
    /// A saved contact or a number the owner marked safe.
    protected: bool,
}

#[derive(Debug, Default)]
struct StallReview {
    /// A review is under way.
    pending: bool,
    /// Why the pending (or put-off) look was asked.
    origin: LookOrigin,
    /// Reviews asked for this call.
    asked: u32,
    /// Reviews that came back too early to tell.
    early: u32,
    /// Keyword looks that count against [`STALL_REVIEW_MAX`] (the pending one included).
    kw_spent: u32,
    /// Looks without keywords asked.
    blind: u32,
    /// How much the caller had said at the last look (see [`content_units`]).
    units_seen: usize,
    /// Looks that did not answer, in a row.
    fails_in_row: u32,
    /// Low-confidence scam answers in a row.
    lows: u32,
    /// Unsure "normal" answers while words only scammers say were heard.
    hard_unsure: u32,
    /// The look kept failing (or there is no key): the keywords decide alone.
    keyword_rule: bool,
    /// Why the last verdict engaged.
    why: EngageWhy,
    /// The score the last "normal" verdict was given at.
    normal_at: Option<u32>,
    /// The last look was too early, unsure or unanswered: the next one goes
    /// when the caller's next turn has ended ([`StallReview::should_ask_after_turn`]).
    after_turn: Option<LookOrigin>,
    /// The caller's turn ended while the pending review was under way
    /// ([`StallReview::note_caller_turn_end`]): it saw only part of that turn.
    turn_ended: bool,
    /// Stalling: caller turns ended since it started, what they had said when it
    /// started, and whether the check to let them go was asked.
    stall_turns: u32,
    units_at_engage: usize,
    confirm_asked: bool,
}

/// What a verdict does to the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StallVerdictStep {
    /// Sales / scam: the AI starts stalling (stalling off: the call is marked so).
    Engage,
    /// A recording or voice menu: the AI goes quiet and we hang up (see [`ivr_hangs_up`]).
    Ivr,
    /// An ordinary call (or nothing new): nothing changes.
    Carry,
    /// Too early, unsure or unanswered: nothing changes until the caller's next turn has ended.
    AfterTurn,
    /// Too early or unsure, but the caller's turn ended or more keywords came
    /// while it was being looked at: look again now (already counted as asked).
    AskAgain,
    /// Stalling, and the check says a sure "normal": back to an ordinary call.
    Release,
}

impl StallReview {
    /// The keyword score is now `score` (`flagged`: [`stall_engages`]): ask for a review?
    fn should_ask(&mut self, flagged: bool, score: u32) -> bool {
        if !flagged || self.pending || self.after_turn.is_some() || self.kw_spent >= STALL_REVIEW_MAX {
            return false;
        }
        if self.normal_at.is_some_and(|at| score < at + STALL_REVIEW_STEP) {
            return false;
        }
        self.start(LookOrigin::Keywords);
        true
    }

    fn start(&mut self, origin: LookOrigin) {
        self.pending = true;
        self.origin = origin;
        self.asked += 1;
        match origin {
            LookOrigin::Keywords => self.kw_spent += 1,
            LookOrigin::Blind => self.blind += 1,
            LookOrigin::Confirm => self.confirm_asked = true,
        }
    }

    /// Why the look under way was asked, for the request.
    fn purpose(&self) -> crate::stall_review::Purpose {
        match self.origin {
            LookOrigin::Keywords => crate::stall_review::Purpose::Keywords,
            LookOrigin::Blind => crate::stall_review::Purpose::Content,
            LookOrigin::Confirm => crate::stall_review::Purpose::Confirm,
        }
    }

    /// The caller's turn has just ended (the AI's reply started): ask for the
    /// review a too-early, unsure or unanswered look put off?
    fn should_ask_after_turn(&mut self) -> bool {
        let Some(origin) = self.after_turn else { return false };
        if self.pending {
            return false;
        }
        self.after_turn = None;
        if origin == LookOrigin::Blind && self.blind >= BLIND_MAX {
            return false;
        }
        self.start(origin);
        true
    }

    /// The caller's turn has just ended: a review under way saw only part of it.
    fn note_caller_turn_end(&mut self) {
        if self.pending {
            self.turn_ended = true;
        }
    }

    /// A caller turn ended without keywords crossing the line: the caller has
    /// had `turns` turns with something in them and said `units` so far
    /// (`eager`: the number was marked or judged sales / scam before). Look anyway?
    fn should_ask_blind(&mut self, units: usize, turns: u32, eager: bool) -> bool {
        if self.pending || self.after_turn.is_some() || self.keyword_rule || self.blind >= BLIND_MAX {
            return false;
        }
        let ready = if self.blind == 0 {
            turns >= BLIND_FIRST_TURNS || units >= BLIND_FIRST_UNITS || (eager && turns >= 1)
        } else {
            true
        };
        // Nothing new since the last look (of any kind): not again.
        let fresh = self.units_seen == 0 || units >= self.units_seen + BLIND_NEXT_UNITS;
        if !ready || !fresh {
            return false;
        }
        self.units_seen = units;
        self.start(LookOrigin::Blind);
        true
    }

    /// A look of any kind was asked when the caller had said `units`.
    fn seen(&mut self, units: usize) {
        self.units_seen = self.units_seen.max(units);
    }

    /// Stalling started when the caller had said `units`.
    fn note_engaged(&mut self, units: usize) {
        self.stall_turns = 0;
        self.units_at_engage = units;
    }

    /// Unmistakable scam-only words (`hard: true`) from a caller who is not a
    /// saved contact or safe-marked number engage stalling at once without
    /// waiting for an HTTP review round-trip.
    fn engage_hard_at_once(&mut self, flagged: bool, hard: bool, protected: bool) -> bool {
        if !flagged || !hard || protected {
            return false;
        }
        self.why = EngageWhy::HardWords;
        true
    }

    /// Stalling, a caller turn just ended (`units` said so far): check now whether
    /// they should be let go? Once a call, and only when the review judged it.
    fn should_ask_confirm(&mut self, units: usize) -> bool {
        self.stall_turns += 1;
        if self.confirm_asked || self.pending || self.why != EngageWhy::Review {
            return false;
        }
        let said = units.saturating_sub(self.units_at_engage);
        if self.stall_turns < CONFIRM_AFTER_TURNS && said < CONFIRM_EARLY_UNITS {
            return false;
        }
        self.start(LookOrigin::Confirm);
        true
    }

    /// The verdict of the review asked at `score`, the score unchanged since.
    #[cfg(test)]
    fn verdict(&mut self, verdict: crate::stall_review::Verdict, score: u32) -> StallVerdictStep {
        self.verdict_now(verdict, score, score)
    }

    /// A sure verdict of a keyword-flagged call asked at score `asked_at`; the score is `now`.
    #[cfg(test)]
    fn verdict_now(&mut self, verdict: crate::stall_review::Verdict, asked_at: u32, now: u32) -> StallVerdictStep {
        let facts = StallFacts {
            flagged: true,
            ..StallFacts::default()
        };
        self.decide(&crate::stall_review::Review::of(verdict), asked_at, now, facts)
    }

    /// Look again now (`more`: the caller said more while it was looked at) or
    /// after their next turn; a look without keywords past its cap is not asked.
    fn again(&mut self, origin: LookOrigin, more: bool) -> StallVerdictStep {
        if origin == LookOrigin::Blind && self.blind >= BLIND_MAX {
            return StallVerdictStep::Carry;
        }
        if more {
            self.start(origin);
            StallVerdictStep::AskAgain
        } else {
            self.after_turn = Some(origin);
            StallVerdictStep::AfterTurn
        }
    }

    /// The answer to the look asked at score `asked_at` (the score is `now`).
    fn decide(&mut self, review: &crate::stall_review::Review, asked_at: u32, now: u32, facts: StallFacts) -> StallVerdictStep {
        use crate::stall_review::{Confidence, Verdict};
        self.pending = false;
        let origin = self.origin;
        let more = std::mem::take(&mut self.turn_ended) || now > asked_at;
        let not_spent = |s: &mut Self| {
            if origin == LookOrigin::Keywords {
                s.kw_spent = s.kw_spent.saturating_sub(1);
            }
        };
        if origin == LookOrigin::Confirm {
            // Stalling already: only a sure "normal" changes anything.
            return if review.verdict == Verdict::Normal && review.confidence == Confidence::High {
                StallVerdictStep::Release
            } else {
                StallVerdictStep::Carry
            };
        }
        if review.verdict == Verdict::Unavailable {
            self.fails_in_row += 1;
            not_spent(self);
            if facts.hard_hit {
                self.why = EngageWhy::HardWords;
                return StallVerdictStep::Engage;
            }
            if facts.protected {
                self.normal_at = Some(asked_at);
                return StallVerdictStep::Carry;
            }
            if review.no_key || self.fails_in_row >= STALL_REVIEW_FAIL_MAX {
                self.keyword_rule = true;
                if facts.flagged {
                    self.why = EngageWhy::KeywordRule;
                    return StallVerdictStep::Engage;
                }
                return StallVerdictStep::Carry;
            }
            self.after_turn = Some(origin);
            return StallVerdictStep::AfterTurn;
        }
        self.fails_in_row = 0;
        self.why = EngageWhy::Review;
        match review.verdict {
            Verdict::ScamOrSales if review.confidence == Confidence::Low => {
                self.lows += 1;
                not_spent(self);
                if self.lows >= 2 {
                    self.lows = 0;
                    if facts.flagged || facts.hard_hit {
                        return StallVerdictStep::Engage;
                    }
                    // Unsure twice, and no keyword says so either: let it be.
                    self.normal_at = Some(asked_at);
                    return StallVerdictStep::Carry;
                }
                self.again(origin, more)
            }
            Verdict::ScamOrSales => StallVerdictStep::Engage,
            Verdict::IvrRecording => StallVerdictStep::Ivr,
            Verdict::Normal => {
                self.lows = 0;
                if facts.hard_hit && review.confidence != Confidence::High {
                    // Words only scammers say ("the code", the ATM, a safe account,
                    // remote control) and only an unsure "normal": a spoofed number
                    // asking for a code looks like this. Look once more; unsure
                    // twice, it is kept talking as a scam.
                    self.hard_unsure += 1;
                    not_spent(self);
                    if self.hard_unsure >= 2 {
                        self.why = EngageWhy::HardWords;
                        return StallVerdictStep::Engage;
                    }
                    return self.again(origin, more);
                }
                self.normal_at = Some(asked_at);
                StallVerdictStep::Carry
            }
            Verdict::TooEarly => {
                self.early += 1;
                not_spent(self);
                if self.early >= STALL_REVIEW_EARLY_MAX {
                    // Still only who they are: a keyword-flagged call is a scam, as
                    // without the review; an unflagged one is let be.
                    return if origin == LookOrigin::Keywords || facts.flagged {
                        StallVerdictStep::Engage
                    } else {
                        StallVerdictStep::Carry
                    };
                }
                self.again(origin, more)
            }
            Verdict::Unavailable => StallVerdictStep::Carry,
        }
    }
}

/// How much a caller said: each Han, kana or hangul character counts one,
/// each word of another script one.
fn content_units(text: &str) -> usize {
    let dense = |c: char| {
        ('\u{3040}'..='\u{30ff}').contains(&c) || ('\u{3400}'..='\u{9fff}').contains(&c) || ('\u{ac00}'..='\u{d7af}').contains(&c)
    };
    let mut n = 0;
    let mut in_word = false;
    for c in text.chars() {
        if dense(c) {
            n += 1;
            in_word = false;
        } else if c.is_alphanumeric() {
            if !in_word {
                n += 1;
                in_word = true;
            }
        } else {
            in_word = false;
        }
    }
    n
}

/// The caller's side of the merged dialogue: turns with something in them, and
/// how much they said; the carrier's recorded warning is not the caller.
fn caller_content(dialogue: &str) -> (u32, usize) {
    let mut turns = 0;
    let mut units = 0;
    for line in dialogue.lines() {
        let Some(t) = line.trim().strip_prefix("對方：") else { continue };
        if looks_like_carrier_intl_prompt(t) {
            continue;
        }
        let u = content_units(t);
        units += u;
        if u >= CONTENT_TURN_UNITS {
            turns += 1;
        }
    }
    (turns, units)
}

/// What the call is recorded as once judged: the kind of call the look named,
/// or the one the keywords point to; a scam when only scammers' words were
/// heard, otherwise sales, when neither names one.
fn judgement_of(review: &crate::stall_review::Review, hits: &[&'static str], why: EngageWhy) -> crate::calls::ScamJudgement {
    let typology = review.typology.or_else(|| crate::scam_db::typology_for_hits(hits));
    let kind = match typology.and_then(crate::scam_db::typology) {
        Some(t) => t.category.as_str(),
        None if hits.iter().any(|h| crate::scam_db::is_hard(h)) => "scam",
        None => "sales",
    };
    let confidence = match (why, review.verdict) {
        (EngageWhy::Review, crate::stall_review::Verdict::ScamOrSales) => review.confidence.as_str(),
        (EngageWhy::Review, _) => "low",
        _ => "",
    };
    crate::calls::ScamJudgement {
        kind: kind.to_string(),
        typology: typology.unwrap_or_default().to_string(),
        confidence: confidence.to_string(),
        source: why.as_str().to_string(),
        released: false,
    }
}

/// Start a second look ([`crate::stall_review::review_with_context`]) at the call so
/// far (`transcript`, merged into lines); its answer comes back on `tx` with the
/// score it was asked at.
fn spawn_stall_review(
    tx: &mpsc::UnboundedSender<(u32, crate::stall_review::Review)>,
    key: &str,
    transcript: &Option<Arc<std::sync::Mutex<String>>>,
    hits: &[&'static str],
    ctx: crate::stall_review::CallerContext,
    score: u32,
    purpose: crate::stall_review::Purpose,
) {
    let dialogue = transcript
        .as_ref()
        .and_then(|b| b.lock().ok().map(|g| merge_dialogue_fragments(&g)))
        .unwrap_or_default();
    let key = key.to_string();
    let hits: Vec<String> = hits.iter().map(|h| h.to_string()).collect();
    let tx = tx.clone();
    tokio::spawn(async move {
        let review = crate::stall_review::review_with_context(&key, &dialogue, &hits, &ctx, purpose).await;
        let _ = tx.send((score, review));
    });
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

fn in_spoken_language(payload: String, spoken: Option<crate::lang_lock::Spoken>) -> String {
    match spoken {
        None => payload,
        Some(crate::lang_lock::Spoken::Lang(call)) => in_call_language(payload, Some(call)),
        Some(crate::lang_lock::Spoken::Other) => append_language_note(
            payload,
            &crate::prompts::other_call_language_note(crate::locale::current()),
        ),
    }
}

fn append_language_note(payload: String, note: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<Value>(&payload) else {
        return payload;
    };
    let text = v
        .pointer_mut("/clientContent/turns")
        .and_then(Value::as_array_mut)
        .and_then(|t| t.last_mut())
        .and_then(|t| t.get_mut("parts"))
        .and_then(Value::as_array_mut)
        .and_then(|p| p.last_mut())
        .and_then(|p| p.get_mut("text"));
    match text {
        Some(Value::String(text)) => {
            if text.contains(note.trim()) {
                return payload;
            }
            text.push_str(note);
            v.to_string()
        }
        _ => payload,
    }
}

/// A mid-call instruction turn or note (`clientContent`) with the call's
/// language named after its last text ([`crate::prompts::call_language_note`],
/// in the instructions' language): the model holds the call's language
/// ([`crate::lang_lock`]) instead of taking the instruction's or a stray word's.
/// None (a language without a setting) or anything else: unchanged.
fn in_call_language(payload: String, call: Option<crate::locale::Lang>) -> String {
    let Some(call) = call else {
        return payload;
    };
    append_language_note(
        payload,
        &crate::prompts::call_language_note(crate::locale::current(), call),
    )
}

/// Drop STT placeholders such as `<no speech detected>` (the AI's silent turn
/// during the carrier prompt).
pub(crate) fn strip_stt_markers(s: &str) -> String {
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
/// The STT puts the break between two words either at the start of the next
/// fragment (" bye") or at the end of this one ("Okay "): both make a space. A
/// fragment's trailing space is kept at the end of `acc` (a later fragment
/// adds no second one); whoever reads the line trims it.
pub(crate) fn join_fragment(acc: &mut String, next: &str) {
    // A fragment the STT began with a space starts a new word (Korean, accented
    // Latin); Chinese and Japanese join directly; two ASCII words always get one.
    let spaced = next.starts_with(char::is_whitespace);
    let trailing = next.ends_with(char::is_whitespace);
    let next = next.trim();
    let Some(b) = next.chars().next() else {
        return;
    };
    if let Some(a) = acc.chars().last() {
        let word = |c: char| !c.is_whitespace() && !is_cjk(c);
        if (spaced && word(a) && word(b)) || (a.is_ascii_alphanumeric() && b.is_ascii_alphanumeric()) {
            acc.push(' ');
        }
    }
    acc.push_str(next);
    if trailing && next.chars().next_back().is_some_and(|c| !is_cjk(c)) {
        acc.push(' ');
    }
}

/// The labelled transcript (one STT fragment per line) as one line per speaker
/// run, placeholders removed — what the web record and the summary model get.
pub(crate) fn merge_dialogue_fragments(transcript: &str) -> String {
    let mut runs: Vec<(&str, String)> = Vec::new();
    for line in transcript.lines() {
        // A fragment's trailing space is a word break (join_fragment).
        let line = line.trim_start();
        let Some((label, text)) = line.split_once('：') else {
            continue;
        };
        if label != "對方" && label != "接聽" {
            continue;
        }
        match runs.last_mut() {
            Some((l, acc)) if *l == label => join_fragment(acc, text),
            _ => runs.push((label, text.trim_start().to_string())),
        }
    }
    // Put into the owner's reading form once the line is whole: a Japanese
    // 「失礼」 fragment is only known to be Japanese once its 「します」 has joined it.
    runs.into_iter()
        .map(|(l, t)| (l, crate::locale::for_owner(&strip_stt_markers(&t))))
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
    history_turns_with_note(transcript, max_chars, false, None)
}

/// [`transcript_history_turns`]; `stall`: the call was just judged sales / scam —
/// the note also says the replies so far ("I'll think about it", "I'll check later") are over.
/// The note is in the agent's language and tells the model to go on in the
/// language the caller speaks now (`prompts::Pack::history_note`), named when
/// the call's language is known (`call`, [`crate::lang_lock`]).
fn history_turns_with_note(
    transcript: &str,
    max_chars: usize,
    stall: bool,
    call: Option<crate::locale::Lang>,
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
    let note = match call {
        Some(l) => format!("{note}{}", crate::prompts::call_language_note(crate::locale::current(), l).trim_start()),
        None => note.clone(),
    };
    // Kept apart from the caller's first line by a space (Latin notes run into it otherwise).
    let note = if note.ends_with('】') || note.ends_with('）') { note } else { format!("{note} ") };
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
    /// Whitelist urgent call merged with the owner's phone: the AI leaves, the
    /// call stays up (no hangup).
    Handed,
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
            Self::Handed => "handed_to_owner",
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
/// skips a longer piece it already ends with (`push_tx_window`).
fn with_latest<'a>(window: &'a str, latest: &str) -> std::borrow::Cow<'a, str> {
    let piece = latest.trim();
    if piece.is_empty() || window.trim_end().ends_with(piece) {
        std::borrow::Cow::Borrowed(window)
    } else {
        std::borrow::Cow::Owned(format!("{window}{}{piece}", tx_gap(window, latest)))
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
    // A backchannel or thanks in any language ("Genau", 「네네」) takes nothing back.
    if crate::closing::is_fluff(latest) {
        return false;
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
    const MARKS: &[&str] = &["再见", "再見", "拜了", "再会", "再會"];
    if MARKS.iter().any(|m| t.contains(m)) || zh_bye_in(text) || crate::locale::ends_with_farewell(text, 3) {
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
    // Other languages' backchannels too ("Vale, vale", 「なるほど」, "Genau").
    if MARKS.iter().any(|m| t.eq_ignore_ascii_case(m)) || crate::closing::is_backchannel(t) {
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

/// The hangup delay left once the clock may run. It counts from our last words:
/// after our own farewell and [`PERSONA_BYE_SILENCE`] of quiet both ways, that
/// long has passed already (the clock used to start only then: 6 s + the
/// regional 1.5 s after our bye, hidden_us_en 8.2 s after the caller's thanks).
fn hangup_delay_left(hangup_delay: Duration, quiet_after_our_bye: bool) -> Duration {
    if quiet_after_our_bye {
        hangup_delay.saturating_sub(PERSONA_BYE_SILENCE)
    } else {
        hangup_delay
    }
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
    // A snippet that starts a new word keeps one leading space (the STT sends
    // " bye" after "Okay"): push_tx_window and join_fragment read it. Callers
    // that log or match a snippet on its own trim it.
    let push = |dest: &mut Vec<String>, path: &str| {
        if let Some(t) = v.pointer(path).and_then(|x| x.as_str()).and_then(spaced_snippet) {
            dest.push(t);
        }
    };
    push(&mut input, "/serverContent/inputTranscription/text");
    push(&mut input, "/inputTranscription/text");
    push(&mut output, "/serverContent/outputTranscription/text");
    push(&mut output, "/outputTranscription/text");
    (input, output)
}

/// A transcription fragment tidied ([`tidy_cjk_spaces`]) that keeps the space
/// the STT put at its start (" bye") and at its end ("Okay ") as one space, as
/// long as the letter next to it is not Chinese or Japanese: the break between
/// two words is at the end of one fragment or the start of the next. None for
/// a blank fragment.
fn spaced_snippet(raw: &str) -> Option<String> {
    let t = tidy_cjk_spaces(raw);
    let (first, last) = (t.chars().next()?, t.chars().next_back()?);
    let lead = if raw.starts_with(char::is_whitespace) && !is_cjk(first) { " " } else { "" };
    let trail = if raw.ends_with(char::is_whitespace) && !is_cjk(last) { " " } else { "" };
    Some(format!("{lead}{t}{trail}"))
}

/// A snippet for the session's windows: the space its stream's previous
/// snippet ended with (`held`) becomes this one's leading space, and its own
/// trailing space is held back, so no window ends with a space for the checks
/// that read its end.
fn tx_carry(held: &mut bool, raw: &str) -> String {
    let piece = raw.trim_end();
    if piece.is_empty() {
        return String::new();
    }
    let lead = if *held && !piece.starts_with(char::is_whitespace) { " " } else { "" };
    *held = raw.ends_with(char::is_whitespace);
    format!("{lead}{piece}")
}

/// The space to put between a window and the piece that follows it. The STT
/// sends words in pieces and puts the break between two words at the start of
/// the next piece (" bye") or at the end of the window (a trailing space, see
/// [`push_tx_window`]): one space unless a side is Chinese or Japanese. A piece
/// that starts a word right after punctuation gets one too: a letter after a
/// comma, a capital (or a letter of a script without capitals) after the end of
/// a sentence ("for you." + "Cheers."). Never a digit ("3." + "5", "1," + "000")
/// nor a small letter after a full stop ("example." + "com"). A piece without a
/// leading space after a letter continues its word ("Hal" + "lo").
fn tx_gap(window: &str, piece: &str) -> &'static str {
    let Some(first) = piece.trim_start().chars().next() else {
        return "";
    };
    let Some(last) = window.chars().next_back() else {
        return "";
    };
    if last.is_whitespace() || is_cjk(first) || is_cjk(last) {
        return "";
    }
    let lead = piece.starts_with(char::is_whitespace);
    let after_mark = match last {
        ',' | ';' | ':' => first.is_alphabetic(),
        '.' | '!' | '?' | '…' => first.is_alphabetic() && !first.is_lowercase(),
        _ => false,
    };
    if lead || after_mark {
        " "
    } else {
        ""
    }
}

/// Rolling window so streaming partials still match 再見 across chunks.
/// `piece` may start with a space (a new word) or end with one (the next piece
/// is a new word): it is compared and added trimmed, with [`tx_gap`] between,
/// and a trailing space (not after Chinese or Japanese) is left at the end of
/// the window for that next piece. Session windows are fed through
/// [`tx_carry`] instead, so none of them ends with a space.
fn push_tx_window(window: &mut String, piece: &str, cap: usize) {
    let gap = tx_gap(window, piece);
    let trailing = piece.ends_with(char::is_whitespace);
    let piece = piece.trim();
    if piece.is_empty() {
        if trailing && window.chars().next_back().is_some_and(|c| !c.is_whitespace() && !is_cjk(c)) {
            window.push(' ');
        }
        return;
    }
    // A longer piece sent again whole is a re-send. A short one is new words,
    // even when the same: 「掰」+「掰」, 「爸」+「爸」, 「…失礼し」+「ます」.
    if piece.chars().count() >= 4 && window.trim_end().ends_with(piece) {
        return;
    }
    // A cumulative update (longer, holding all so far) replaces the window.
    let held = window.trim_end();
    if piece.len() > held.len() && piece.contains(held) && !held.is_empty() {
        *window = piece.to_string();
        if trailing && !piece.chars().next_back().is_some_and(is_cjk) {
            window.push(' ');
        }
        return;
    }
    window.push_str(gap);
    window.push_str(piece);
    if trailing && !piece.chars().next_back().is_some_and(is_cjk) {
        window.push(' ');
    }
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
    callback: bool,
    errand: Option<Arc<crate::errands::CallHooks>>,
    user_prompt_chars: usize,
) -> Result<SessionEndReason> {
    let outbound = Outbound::of(callback, errand.is_some());
    let opening_lang = lang_plan.opening.filter(|_| persona);
    // The language the call's prompts are written in, and — a persona call that
    // came in — what they are scoped to: the SIM's region, decided before pickup
    // (a table lookup; the ring prewarm has it too).
    let prompt_lang = lang_plan.prompt_lang.unwrap_or_else(crate::locale::current);
    let scope = (persona && outbound == Outbound::Incoming).then(|| crate::prompts::Scope {
        region: lang_plan.region,
        carrier_note: carrier_note_due(&lang_plan),
    });
    let scoped = |p: &str| -> String {
        match scope {
            Some(s) => crate::prompts::scope_to_region(p, prompt_lang, s),
            None => p.to_string(),
        }
    };
    let scope_region = scope.and_then(|s| s.region);
    let prompt_log = PromptLog::new(&lang_plan, prompt_lang, scope_region, user_prompt_chars);
    let stall = stall.filter(|_| persona).map(|mut c| {
        c.system_prompt = scoped(&session_prompt(c.system_prompt.as_str(), outbound));
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
    // Persona: the hangup timer of the call's region — the SIM's country, then the
    // call's language ([`crate::closing::hangup_timer`]); the owner's setting wins.
    let call_lang = lang_plan
        .opening
        .or(lang_plan.prompt_lang)
        .unwrap_or_else(crate::locale::current)
        .code();
    // AI errands keep the old timer (the regional one is for answered calls).
    let hangup_timer = (persona && errand.is_none()).then(|| {
        crate::closing::hangup_timer(goodbye_hangup_delay_ms, crate::call_sim_country(serial).as_deref(), call_lang)
    });
    let hangup_delay_ms = hangup_timer
        .as_ref()
        .map_or_else(|| goodbye_hangup_delay_ms.clamp(200, 15_000), |t| t.ms);
    // The stored prompt stays as it is; appointments add their part after it
    // (with the current local time), and the tools.
    let base_prompt = scoped(&session_prompt(system_prompt, outbound));
    let system_prompt = call_system_prompt(&base_prompt, appt.as_ref(), chrono::Local::now(), false);
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
                // An appointment's call-back: no vocabulary for its callee (Outbound::other_side_stt).
                "inputAudioTranscription": outbound.other_side_stt_for(
                    lang_plan.caller_stt.as_deref(),
                    lang_plan.opening,
                    lang_plan.caller_other_stt,
                    &crate::locale::active(),
                ),
                "outputAudioTranscription": stt_ai_config(opening_lang, lang_plan.caller_other_stt),
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
        if errand.is_some() {
            with_errand_tools(&mut setup);
        }
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
        prompt_log.log(system_prompt.chars().count(), false);
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
    // An AI call-back (`callback`): the phone dialed, so the line rings first
    // and nobody is there to greet; the model speaks once the callee has
    // spoken, and no opening is asked for.
    let greets_kick_text = if errand.is_some() {
        crate::errands::kick_client_content()
    } else if callback {
        callback_kick_client_content()
    } else if persona {
        // Said to be from abroad only when the number is of another country than
        // the SIM's (CallLangPlan::sim_local); a SIM's own language is no foreign
        // call. SIM unknown: when the carrier's international-call warning is due
        // (expects_carrier_intl_prompt).
        let from_abroad = opening_from_abroad(opening_lang, lang_plan.sim_local, || intl_caller());
        let local = lang_plan.local_hello.filter(|_| opening_lang.is_none());
        let from_abroad = from_abroad || (local.is_some() && lang_plan.sim_local == Some(false));
        persona_kick_with_local(opening_lang, local, from_abroad)
    } else {
        greets_kick_client_content_for(&owner_status)
    };
    info!(
        "Gemini Live greets kick ready serial={serial} persona={persona} owner_status_chars={} opening_lang={}",
        owner_status.chars().count(),
        opening_lang
            .map(|l| l.code())
            .or(lang_plan.local_hello.filter(|_| persona).map(|h| h.code))
            .unwrap_or("-")
    );
    // Max ~12s of 16 kHz mono s16le held until UnmuteAfterGreet (never inject mid-IVR).
    const PREBUF_CAP_BYTES: usize = 16_000 * 2 * 12;
    let mut greets_kicked = callback;
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
        if !greets_kicked && setup_ready.load(Ordering::Relaxed) {
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
                                && !callback
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
                                            "Gemini Live warm opening text-only (0 audio) serial={serial} out_stt={dropped:?} — re-kick n={warm_rekicks} before pickup",
                                            dropped = Said(&dropped)
                                        ),
                                        Err(e) => warn!(
                                            "Gemini Live warm text-only re-kick failed serial={serial}: {e:#}"
                                        ),
                                    }
                                } else if !warm_text_only {
                                    warm_text_only = true;
                                    warn!(
                                        "Gemini Live warm opening text-only {warm_rekicks}x serial={serial} out_stt={dropped:?} — kick again when the dialogue opens",
                                        dropped = Said(&dropped)
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
    // A call-back dials out: no carrier prompt comes, and the callee's first
    // words must not be taken for one.
    let intl_call = persona && !callback && intl_caller();
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
    } else if callback {
        // A call-back: nothing the model said before pickup is for anyone.
        prebuf.clear();
        prebuf_chunks = 0;
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
    } else if callback {
        info!("Gemini Live call-back serial={serial} — no opening; the AI waits for the callee to speak");
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
    let mut turn_base_bytes: u64 = prebuf_flushed_bytes;
    let mut out_tx_window = String::with_capacity(512);
    let mut in_tx_window = String::with_capacity(512);
    // Whether the last output / input snippet ended with a space ([`tx_carry`]).
    let (mut out_space_held, mut in_space_held) = (false, false);
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
    // Persona: Talking → PreClosing → Closing → Hangup, judged from each transcript
    // piece in memory (crate::closing); logs closing_state.
    let mut closing_sm = crate::closing::ClosingMachine::new(call_lang);
    // Logged once per arm: AI said bye, line held for the caller's.
    let mut await_bye_logged = false;
    let mut await_exchange_bye_logged = false;
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
    // An errand call: wrapped up at 9:30, ended at 10 minutes.
    let (mut wrap_secs, mut wrap_force_secs, mut max_secs) = if errand.is_some() {
        (crate::errands::WRAP_UP_AT_SECS, crate::errands::WRAP_UP_AT_SECS + 10, crate::errands::CALL_MAX_SECS)
    } else {
        talk_limits(persona, persona_max_secs)
    };
    // Everything the caller said (input STT), for the carrier-prompt check.
    let mut caller_text_all = String::new();
    // The language the call is held in (lang_lock): the opening's, then the
    // caller's first clear words; it follows them only into whole turns of
    // another language, two in a row. Every mid-call instruction names it.
    let mut call_language = crate::lang_lock::CallLanguage::new(opening_lang.unwrap_or(prompt_lang), prompt_lang);
    // Hints for the AI's own voice. A caller found to speak Thai, Russian, Arabic or
    // Hindi adds theirs; the next rotated session carries it (`ai_stt_grown`).
    let mut ai_stt_codes = stt_ai_codes(opening_lang, lang_plan.caller_other_stt);
    let mut ai_stt_grown = false;
    // An errand call: the other side has spoken a script none of the AI's
    // languages use (then the AI may speak it too).
    let mut caller_other_script = false;
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
    // An AI call-back: still waiting for the callee to pick up and speak.
    let mut callback_waiting = callback;
    // An appointment's call-back says its answer once (Outbound::answers_once,
    // CallbackFloor); an errand's call is a conversation and goes without it.
    let cb_once = outbound.answers_once();
    // An AI call-back: a reply plays only once the callee has really spoken
    // (CallbackFloor); a held one's transcript mark (its lines come out if it is
    // dropped) and its words (a closing in them counts once it plays).
    let mut cb_floor = CallbackFloor::for_pickup();
    let mut cb_held_mark: Option<usize> = None;
    let mut cb_held_text = String::new();
    let mut cb_drop_now = false;
    let cb_hold_cap = (phone_sr as usize).max(8_000) * 2 * CALLBACK_HOLD_SECS;
    // An errand: what the owner types on the web console, a voice menu heard
    // (since when, and the AI's audio count then), the carrier's recording.
    let mut errand_says = errand.as_ref().and_then(|e| e.take_says());
    let mut errand_menu_since: Option<tokio::time::Instant> = None;
    let mut errand_menu_chunks: u64 = 0;
    let mut errand_carrier: Option<String> = None;
    // "Redial until answered": voicemail picked up (hung up, no message).
    let mut errand_voicemail = false;
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
    // An errand's ask_owner calls whose answer has not come yet (still waited for
    // when the model dropped the call: the answer then goes as a note).
    let mut ask_waiting: std::collections::HashSet<String> = std::collections::HashSet::new();
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
    // Sales / scam stalling under way: the call's kind and pace (stall_script) —
    // bait, one move per AI turn, the last bait when the caller leaves.
    let mut stall_engine: Option<crate::stall_script::StallEngine> = None;
    // Ordinary calls: what the second look says the call is about (scenario),
    // its one short cue owed for the pause after the AI's turn.
    let mut scenario_router =
        crate::scenario::Router::new(crate::locale::Lang::parse(call_lang), scope_region, appt.is_some());
    // Appointments: the call's prompt has their short part; the whole one goes in
    // as a note in the pause after the AI's turn once the caller talks times.
    let mut appt_block_due = false;
    let mut appt_block_sent = false;
    let (review_tx, mut review_rx) = mpsc::unbounded_channel::<(u32, crate::stall_review::Review)>();
    let review_key = key.to_string();
    let fallback_review_number: &'static str = if intl_call { "unknown (from abroad)" } else { "unknown" };
    let review_ctx = || -> crate::stall_review::CallerContext {
        if let Some(f) = stall.as_ref().and_then(|c| c.caller_context.as_ref()) {
            let mut ctx = (f.0)();
            if ctx.number.trim().is_empty() {
                ctx.number = fallback_review_number.to_string();
            }
            ctx
        } else {
            crate::stall_review::CallerContext {
                number: fallback_review_number.to_string(),
                ..crate::stall_review::CallerContext::default()
            }
        }
    };
    // What the caller has said so far: turns with something in them, and how much.
    let caller_said = || -> (u32, usize) {
        transcript_out
            .as_ref()
            .and_then(|b| b.lock().ok().map(|g| caller_content(&merge_dialogue_fragments(&g))))
            .unwrap_or((0, 0))
    };
    // Stalling is on (setting persona_stall_spam); off, a judged call is only marked.
    let stall_play = stall.as_ref().is_some_and(|c| c.play_along || c.force_start);
    // Calls no keyword flagged are looked at too (setting scam_review_without_keywords).
    let blind_looks = stall.as_ref().is_some_and(|c| c.review_without_keywords);
    let mut stall_engage_now: Option<crate::calls::ScamJudgement> = stall
        .as_ref()
        .filter(|c| c.force_start || c.engaged.load(Ordering::Relaxed))
        .map(|_| crate::calls::ScamJudgement {
            kind: "sales".into(),
            typology: "contact_stall_list".into(),
            confidence: "high".into(),
            source: "stall_list".into(),
            released: false,
        });
    let mut stall_release_now = false;
    let mut urgent_transfer_fired = false;
    // Whitelist urgent call: what the AI said since the caller's urgent words (a line
    // said twice there is its hold-on line even when the wording is not known).
    let mut ai_after_urgent = String::new();
    // Judged sales / scam with stalling off: marked, and not looked at again.
    let mut stall_detected = false;
    // The live socket runs the stall prompt (a rotation went through while stalling).
    let mut live_is_stall = false;
    // The caller is a recording or voice menu: why (to be acted on), and since when.
    let mut ivr_enter: Option<&'static str> = None;
    let mut ivr_since: Option<tokio::time::Instant> = None;

    // Whitelist urgent call: while the phone dials the owner, or once the owner is
    // on, the session must not hang up the call.
    let urgent_ctl = stall.as_ref().and_then(|c| c.urgent_call.clone());
    let signal_force_hangup = |hangup_signaled: &mut bool| {
        if *hangup_signaled {
            return;
        }
        if urgent_ctl.as_ref().is_some_and(|c| c.blocks_hangup()) {
            info!("Whitelist urgent call in progress serial={serial} — hangup not signaled");
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

        // Whitelist urgent call: the owner is on the call now (the AI leaves), the
        // phone dials them (nothing from the AI meanwhile), or they did not answer
        // and the AI tells the caller.
        if let Some(ctl) = urgent_ctl.as_ref() {
            if ctl.is_handed() {
                end_reason = SessionEndReason::Handed;
                info!("Whitelist urgent call merged serial={serial} — the AI leaves the call, no hangup");
                break;
            }
            if let Some(turn) = ctl.take_say() {
                model_turn_pending = true;
                // In the language the call is held in (lang_lock), not the setting's.
                let spoken = call_language.lang();
                info!(
                    "Whitelist urgent call: line for the AI serial={serial} caller_language={}",
                    spoken.map_or("unknown", |l| l.code())
                );
                let turn = crate::urgent_call::with_caller_language(&turn, spoken);
                match send_text_before_deadline(
                    &mut sink,
                    turn,
                    tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                    "gemini urgent call line send",
                )
                .await
                {
                    Ok(()) => info!("Whitelist urgent call not merged serial={serial} — the AI tells the caller"),
                    Err(e) => warn!("Whitelist urgent call line failed serial={serial}: {e:#}"),
                }
                last_activity_at = tokio::time::Instant::now();
            }
            if ctl.is_paused() {
                last_activity_at = tokio::time::Instant::now();
            }
        }

        // An errand: the owner cancelled it, the carrier's recording answered,
        // or a voice menu never brought a person on.
        if let Some(e) = &errand {
            if e.cancel_requested() {
                end_reason = SessionEndReason::Goodbye;
                signal_force_hangup(&mut hangup_signaled);
                info!("Errand call cancelled by the owner serial={serial} — hang up");
                break;
            }
            if let Some(said) = errand_carrier.take() {
                e.note_carrier(&said);
                end_reason = SessionEndReason::IvrHangup;
                signal_force_hangup(&mut hangup_signaled);
                info!("Errand call reached the carrier's recording serial={serial} — hang up");
                break;
            }
            if errand_voicemail {
                e.note_voicemail();
                end_reason = SessionEndReason::IvrHangup;
                signal_force_hangup(&mut hangup_signaled);
                info!("Errand call reached voicemail serial={serial} — hang up, dialed again later");
                break;
            }
            if let Some(t0) = errand_menu_since {
                if audio_chunks_out > errand_menu_chunks {
                    errand_menu_since = None;
                    e.note_menu(false);
                    info!("Errand call: the AI talks after the voice menu (a person came on) serial={serial}");
                } else if t0.elapsed() >= crate::errands::MENU_WAIT {
                    end_reason = SessionEndReason::IvrHangup;
                    signal_force_hangup(&mut hangup_signaled);
                    info!(
                        "Errand call: voice menu, no person within {}s serial={serial} — hang up",
                        crate::errands::MENU_WAIT.as_secs()
                    );
                    break;
                }
            }
        }
        // An errand's question waits for the owner: the line is not idle.
        let errand_asking = errand.as_ref().is_some_and(|e| e.question_open()) || !ask_waiting.is_empty();

        // The second look says sales / scam: the AI starts stalling (stalling off:
        // the call is only marked so, and the AI goes on as it was).
        if let Some(judged) = stall_engage_now
            .take()
            .filter(|_| !stall_on && !stall_detected && (!own_matter || stall.as_ref().is_some_and(|c| c.force_start)) && ivr_since.is_none())
        {
            if let Some(c) = stall.as_ref() {
                if let Some(a) = &appt {
                    a.book.set_reviewed_normal(false);
                }
                if !c.play_along && !c.force_start {
                    stall_detected = true;
                    if let Ok(mut j) = c.judgement.lock() {
                        *j = Some(judged.clone());
                    }
                    info!(
                        "Persona call judged {} ({}) serial={serial} source={} — stalling is off, marked only",
                        judged.kind, judged.typology, judged.source
                    );
                } else {
                    stall_on = true;
                    stall_review.note_engaged(caller_said().1);
                    c.engaged.store(true, Ordering::Relaxed);
                    // After the flag: the call record reads both, the flag first.
                    if let Ok(mut j) = c.judgement.lock() {
                        *j = Some(judged.clone());
                    }
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
                    // Stall session: its prompt (+ country-specific bait hooks when matched),
                    // and its own model (setting `gemini_stall_model`) — same voice, so the caller hears no change.
                    let ctx_now = review_ctx();
                    let full_prompt = match crate::scam_db::country_bait_addon(
                        &ctx_now.number,
                        &ctx_now.number_notes,
                        &judged.typology,
                        &stall_hits,
                        &caller_text_all,
                    ) {
                        Some(addon) if !c.system_prompt.trim().is_empty() => format!("{}\n\n{addon}", c.system_prompt),
                        _ => c.system_prompt.clone(),
                    };
                    // Only this kind of call's script (stall_script): the stall block's
                    // lists of every move and topic give way to it.
                    let (stall_prompt, script_lang) =
                        crate::stall_script::slim_stall_prompt(&full_prompt, &judged.typology);
                    let engine = crate::stall_script::StallEngine::new(
                        &judged.typology,
                        script_lang.unwrap_or_else(|| crate::locale::Lang::parse(call_lang)),
                    );
                    info!(
                        "stall_stage stage=free tactic=script:{} typology={} turn=0 prompt_chars={}->{} serial={serial}",
                        engine.family.id(),
                        engine.typology(),
                        full_prompt.chars().count(),
                        stall_prompt.chars().count()
                    );
                    stall_engine = Some(engine);
                    scenario_router.drop_pending();
                    // Judged by the words alone (no look answered): the call's scenario is a
                    // scam all the same, logged like a look's (a look logs its own line).
                    if judged.source != EngageWhy::Review.as_str() {
                        info!(
                            "scenario_route scenario=scam confidence=- ms=0 verdict=ScamOrSales action=stalling source={} serial={serial}",
                            judged.source
                        );
                    }
                    let next = stall_session_setup(&setup, &stall_prompt, &c.model, thinking_level);
                    if next != setup {
                        rotate_setup = Some(next);
                        rotate_due = Some(
                            tokio::time::Instant::now() + Duration::from_secs(STALL_PROMPT_SWITCH_FORCE_SECS),
                        );
                    }
                    // Stalling may run longer than a normal call.
                    (wrap_secs, wrap_force_secs, max_secs) = talk_limits(persona, c.max_secs);
                    info!(
                        "Persona stall engaged serial={serial} kind={} typology={} confidence={} source={} score={stall_score} hits={stall_hits:?} talk_s={} cap_s={max_secs} wrap_up_was_sent={wrap_was_sent} switch={} model={}",
                        judged.kind,
                        judged.typology,
                        judged.confidence,
                        judged.source,
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
        }
        // The check after stalling started says an ordinary call, and is sure: if
        // the session already rotated to stalling, switch back to the normal
        // prompt, model and (when enabled) appointment tools at the next pause;
        // if not, cancel any pending stall rotation. Either way, restore the
        // normal call's cap without cutting an already longer call at once.
        if std::mem::take(&mut stall_release_now)
            && stall_on
            && !stall.as_ref().is_some_and(|c| c.force_start)
        {
            if let Some(c) = stall.as_ref() {
                stall_on = false;
                stall_engine = None;
                if let Ok(mut j) = c.judgement.lock() {
                    if let Some(j) = j.as_mut() {
                        j.released = true;
                    }
                }
                stall_keep_due = None;
                stall_keep_after_turn = false;
                stall_turn_kept = false;
                stall_nudge_chunks = None;
                stall_nudge_at = None;
                repeat_guard.clear();
                let (next_setup, need_rotate) = release_rotation(
                    live_is_stall,
                    &setup,
                    &base_prompt,
                    appt.as_ref(),
                    chrono::Local::now(),
                    appt_block_sent,
                    non_blocking,
                );
                rotate_setup = next_setup;
                rotate_due = need_rotate
                    .then(|| tokio::time::Instant::now() + Duration::from_secs(STALL_PROMPT_SWITCH_FORCE_SECS));
                // The call may have talked past the normal cap while it was thought a stall:
                // it gets a normal wrap-up and goodbye, not an instant cut.
                (wrap_secs, wrap_force_secs, max_secs) =
                    released_talk_limits(persona, persona_max_secs, session_start.elapsed().as_secs());
                info!(
                    "Persona stall released serial={serial} talk_s={} switch_back={live_is_stall} cap_s={max_secs} — the check found an ordinary call",
                    session_start.elapsed().as_secs()
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
                && !errand_asking
                && play_head.drained_for(Duration::from_millis(300), PLAYER_DRAIN_SLACK_BYTES);
            let backoff = !ws_dead && rotate_retry_at.map_or(false, |t| now < t);
            if wanted && (pause || forced) && !backoff {
                // Counted only once it worked: failures back off instead (rotate_retry_after).
                let n = rotations + 1;
                let history = transcript_out
                    .as_ref()
                    .and_then(|b| b.lock().ok().map(|g| g.clone()))
                    .map(|t| history_turns_with_note(&t, LIVE_HISTORY_MAX_CHARS, stall_on, call_language.lang()))
                    .unwrap_or_default();
                // Appointments: the new session gets the current time.
                let fresh_setup;
                let next_setup = match (rotate_setup.as_ref(), &appt) {
                    (Some(s), _) => s,
                    (None, Some(a)) => {
                        fresh_setup = setup_with_system_prompt(
                            &setup,
                            &call_system_prompt(&base_prompt, Some(a), chrono::Local::now(), appt_block_sent),
                        );
                        &fresh_setup
                    }
                    (None, None) => &setup,
                };
                // The caller's language the AI's hints lacked (ai_stt_codes).
                let stt_setup;
                let next_setup = if ai_stt_grown {
                    stt_setup = with_ai_stt_codes(next_setup, &ai_stt_codes);
                    &stt_setup
                } else {
                    next_setup
                };
                let next_prompt_chars = setup_system_prompt(next_setup).chars().count();
                let opened = {
                    let rotate_fut = open_live_with_history(&url, next_setup, &history, serial);
                    tokio::pin!(rotate_fut);
                    let deadline = tokio::time::sleep(Duration::from_secs(10));
                    tokio::pin!(deadline);
                    loop {
                        if cancel.load(Ordering::Relaxed) {
                            info!("Gemini Live rotate aborted (hangup) serial={serial}");
                            let _ = sink.send(WsMessage::Close(None)).await;
                            let _ = sink.close().await;
                            return Ok(resolve_cancel(goodbye_pending));
                        }
                        tokio::select! {
                            r = &mut rotate_fut => break r,
                            _ = &mut deadline => break Err(anyhow::anyhow!("gemini rotate timed out after 10s")),
                            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                        }
                    }
                };
                match opened {
                    Ok((new_sink, new_stream)) => {
                        rotations = n;
                        rotate_fails = 0;
                        prompt_log.log(next_prompt_chars, true);
                        // What the new socket runs (the release check switches back from it).
                        live_is_stall = stall_on;
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
                        turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
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
                                in_spoken_language(persona_stall_keep_with_retain(true, &mut stall_engine, serial), Some(call_language.spoken())),
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
                                in_spoken_language(persona_resume_reply_client_content(), errand.is_none().then(|| call_language.spoken())),
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
                        if !stall_on {
                            rotate_setup = None;
                        }
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
                    turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
                    first_delivery_deadline = first_delivery_deadline.max(now + FIRST_DELIVERY_TIMEOUT);
                    info!(
                        "Gemini Live intl caller serial={serial} — line talking before our opening (carrier prompt): dropped it ({dropped_ms}ms {dropped:?}); the model answers after the prompt",
                        dropped = Said(&dropped)
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
        // An appointment's call-back: a reply the model generated before the
        // callee spoke plays once they do, or is dropped unheard (CallbackFloor).
        if cb_once {
            let now = tokio::time::Instant::now();
            let step = if std::mem::take(&mut cb_drop_now) {
                cb_floor.drop_step()
            } else {
                cb_floor.step(now)
            };
            match step {
                FloorStep::Wait => {}
                FloorStep::Play { over } => {
                    let n = prebuf.len() as u64;
                    if n > 0 {
                        downlink_pcm_bytes = downlink_pcm_bytes.saturating_add(n);
                        on_downlink(std::mem::take(&mut prebuf));
                        audio_chunks_out = audio_chunks_out.saturating_add(prebuf_chunks);
                        last_activity_at = now;
                        if first_downlink_at.is_none() {
                            first_downlink_at = Some(now);
                        }
                    }
                    prebuf_chunks = 0;
                    cb_held_mark = None;
                    if over {
                        // Its turnComplete went by while it was held.
                        model_turn_pending = false;
                    } else {
                        // The rest of this reply plays, and its end is a turn like any other.
                        intl_gated_turn = false;
                        play_head.set_generating(model_generating);
                    }
                    if persona && !goodbye_pending && caller_had_voice && looks_like_persona_closing(&cb_held_text) {
                        goodbye_pending = true;
                        end_reason = SessionEndReason::Goodbye;
                    }
                    info!(
                        "Gemini Live call-back serial={serial} — the callee spoke: the held reply plays ({}ms, ended={over}) n={} closing={goodbye_pending}",
                        pcm16_ms(n, phone_sr),
                        cb_floor.replies
                    );
                    cb_held_text.clear();
                }
                FloorStep::Drop { note } => {
                    let dropped_ms = pcm16_ms(prebuf.len() as u64, phone_sr);
                    prebuf.clear();
                    prebuf_chunks = 0;
                    let dropped = match (cb_held_mark.take(), transcript_out.as_ref()) {
                        (Some(mark), Some(buf)) => buf
                            .lock()
                            .map(|mut g| drop_ai_lines_after(&mut g, mark))
                            .unwrap_or_default(),
                        _ => String::new(),
                    };
                    out_tx_window.clear();
                    turn_out_text.clear();
                    turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
                    cb_held_text.clear();
                    // The model is told nobody heard it, so it does not count it as said.
                    let told = send_text_before_deadline(
                        &mut sink,
                        note,
                        now + KICK_SEND_TIMEOUT,
                        "gemini call-back unheard note send",
                    )
                    .await
                    .is_ok();
                    info!(
                        "Gemini Live call-back serial={serial} — the AI spoke with nobody speaking to it: dropped unheard ({dropped_ms}ms {dropped:?}) replies={} model_told={told}",
                        cb_floor.replies,
                        dropped = Said(&dropped)
                    );
                }
            }
        }
        // A call the phone dialed rings first; nobody for CALLBACK_RING_MAX ends
        // it. An appointment's call-back: Gemini hears silence until the callee
        // has picked up and spoken (their voice, not a ringing tone); then the
        // model gets its cue and the first-audio watchdog starts. An errand: the
        // other side's first words (a person, a voice menu or the carrier's
        // recording, which its script and checks tell apart) or the model
        // already answering them.
        if callback_waiting {
            let now = tokio::time::Instant::now();
            let kick = if cb_once { cb_floor.pickup() } else { None };
            let errand_heard = !cb_once
                && (!caller_text_all.trim().is_empty() || delivered_bytes.load(Ordering::Relaxed) > 0);
            if kick.is_some() || errand_heard {
                callback_waiting = false;
                first_delivery_window_start = now;
                first_delivery_deadline = now + FIRST_DELIVERY_TIMEOUT;
                fd_grace = Duration::ZERO;
                if let Some(kick) = kick {
                    model_turn_pending = true;
                    let sent = send_text_before_deadline(
                        &mut sink,
                        kick,
                        now + KICK_SEND_TIMEOUT,
                        "gemini call-back pickup kick send",
                    )
                    .await
                    .is_ok();
                    info!(
                        "Gemini Live call-back serial={serial} — the callee picked up and spoke: the AI says the answer now (cue sent={sent})"
                    );
                } else {
                    info!("Gemini Live call-back serial={serial} — the callee spoke; the AI may answer");
                }
                if let Some(e) = &errand {
                    e.picked_up();
                }
            } else if session_start.elapsed() >= CALLBACK_RING_MAX {
                end_reason = SessionEndReason::CantCommunicate;
                signal_force_hangup(&mut hangup_signaled);
                info!(
                    "Gemini Live call-back serial={serial} — nobody spoke within {}s: no answer, hang up",
                    CALLBACK_RING_MAX.as_secs()
                );
                break;
            } else {
                first_delivery_deadline = now + FIRST_DELIVERY_TIMEOUT;
            }
        }
        let fd_now = tokio::time::Instant::now();
        if delivered_bytes.load(Ordering::Relaxed) == 0 && !callback_waiting {
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
        // An errand on a voice menu, or a caller talking continuously: pause first-delivery kicks/timeouts.
        let caller_talking = activity_open
            || last_caller_voice_at.is_some_and(|t| t.elapsed() < FIRST_DELIVERY_CALLER_HOLD);
        let fd_action = if intl_gate != IntlGate::Open
            || callback_waiting
            || errand_menu_since.is_some()
            || (delivered_bytes.load(Ordering::Relaxed) == 0 && caller_talking)
        {
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
        // An errand call is ours: the AI's own goodbye ends it.
        let quiet_after_bye = play_head.drained_for(PERSONA_BYE_SILENCE, PLAYER_DRAIN_SLACK_BYTES)
            && last_caller_voice_at.map_or(true, |t| t.elapsed() >= PERSONA_BYE_SILENCE);
        let awaiting_caller_bye = persona
            && errand.is_none()
            && goodbye_pending
            && !wrap_up_sent
            && awaits_caller_bye(caller_end_signal, quiet_after_bye);
        if awaiting_caller_bye && !await_bye_logged {
            await_bye_logged = true;
            info!(
                "Persona: AI said bye, caller has not — hold the line for their bye or {}s of silence serial={serial}",
                PERSONA_BYE_SILENCE.as_secs()
            );
        }
        // Farewells exchanged on the caller's announced end: their own bye
        // usually follows — the line stays open a moment for it, then the call
        // ends on the quiet (it used to end while their 「拜拜」 was on its way).
        let awaiting_exchange_bye = persona
            && errand.is_none()
            && goodbye_pending
            && !wrap_up_sent
            && closing_sm.awaits_caller_bye()
            && !(play_head.drained_for(PERSONA_EXCHANGE_BYE_WAIT, PLAYER_DRAIN_SLACK_BYTES)
                && last_caller_voice_at.map_or(true, |t| t.elapsed() >= PERSONA_EXCHANGE_BYE_WAIT));
        if awaiting_exchange_bye && !await_exchange_bye_logged {
            await_exchange_bye_logged = true;
            info!(
                "Persona: farewells exchanged, the caller's own bye not yet — hold the line up to {}ms of quiet for it serial={serial}",
                PERSONA_EXCHANGE_BYE_WAIT.as_millis()
            );
        }
        let player_idle = goodbye_clock_may_run(
            play_head.drained_for(
                Duration::from_millis(RELAY_AUDIO_IDLE_MS),
                PLAYER_DRAIN_SLACK_BYTES,
            ),
            activity_open,
            // A function call open: the model still has something to say.
            waiting_model_reply || awaiting_caller_bye || awaiting_exchange_bye || !tool_open.is_empty() || errand_asking,
        );
        let now = tokio::time::Instant::now();
        let clock_was = output_goodbye_at.is_some();
        // Our own bye ending the call on the quiet after it: that quiet counts.
        let hangup_delay = hangup_delay_left(
            Duration::from_millis(hangup_delay_ms),
            persona && errand.is_none() && !wrap_up_sent && !caller_end_signal && !bye_from_caller && quiet_after_bye,
        );
        let fire_wav = tick_goodbye_play_clock(
            goodbye_pending,
            player_idle,
            hangup_delay,
            now,
            &mut output_goodbye_at,
        );
        if goodbye_pending && !clock_was && output_goodbye_at.is_some() {
            info!(
                "Goodbye play clock start (player drained) serial={serial} queued={} — wait {}ms then WAV"
            , play_head.queued(), hangup_delay.as_millis());
            if let Some(t) = &hangup_timer {
                info!("closing_hangup_timer ms={} region={}", t.ms, t.region);
                if t.from_setting {
                    info!("Hangup timer is the owner's setting goodbye_hangup_delay_ms serial={serial} — the regional default is not used");
                }
            }
        } else if goodbye_pending && clock_was && output_goodbye_at.is_none() {
            info!(
                "Goodbye play clock reset (downlink playing / caller talking) serial={serial} queued={} outbound={} activity_open={activity_open}"
            , play_head.queued(), play_head.is_outbound());
        }
        if fire_wav
            && persona
            && !callback
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
                "Persona: AI turn does not end on a farewell — keep talking serial={serial} stall={stall_on} tail={tail}",
                tail = Said(&tail)
            );
        } else if fire_wav {
            end_reason = SessionEndReason::Goodbye;
            if persona {
                closing_sm.hangup(if caller_end_signal || bye_from_caller { "caller_closed_timer" } else { "ai_bye_quiet_timer" });
            }
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
                await_exchange_bye_logged = false;
            }
        }

        // Wall-clock max talk time → agent will play goodbye WAV (persona: plain hangup).
        // Not while the phone dials the owner for a whitelist urgent call.
        if session_start.elapsed() >= Duration::from_secs(max_secs)
            && !urgent_ctl.as_ref().is_some_and(|c| c.ai_quiet())
        {
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
        if (persona || errand.is_some()) && !wrap_up_sent && !goodbye_pending {
            let talk = session_start.elapsed();
            let gap = !activity_open
                && play_head.drained_for(Duration::from_millis(300), PLAYER_DRAIN_SLACK_BYTES);
            if talk >= Duration::from_secs(wrap_force_secs)
                || (talk >= Duration::from_secs(wrap_secs) && gap)
            {
                wrap_up_sent = true;
                repeat_guard.clear();
                let nudge = if stall_on {
                    in_spoken_language(persona_stall_wrap_up_client_content(), Some(call_language.spoken()))
                } else if errand.is_some() {
                    crate::errands::wrap_up_client_content()
                } else {
                    in_spoken_language(persona_wrap_up_client_content(), Some(call_language.spoken()))
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
                        in_spoken_language(persona_stall_keep_with_retain(false, &mut stall_engine, serial), Some(call_language.spoken())),
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
        // An appointment's call-back: the answer has been given and nobody has
        // said anything since (both sides quiet CALLBACK_DONE_QUIET; the answer
        // ends on a goodbye), or the last reply a call-back may have is over:
        // hang up. Never an errand's call (Outbound::answers_once).
        if let Some(quiet) = cb_floor.done_after().filter(|_| cb_once) {
            if !cb_floor.on_line
                && !activity_open
                && tool_open.is_empty()
                && play_head.drained_for(quiet, PLAYER_DRAIN_SLACK_BYTES)
                && last_caller_voice_at.map_or(true, |t| t.elapsed() >= quiet)
            {
                end_reason = SessionEndReason::Goodbye;
                signal_force_hangup(&mut hangup_signaled);
                info!(
                    "Gemini Live call-back serial={serial} — answer given, line quiet {}ms replies={}: hang up",
                    quiet.as_millis(),
                    cb_floor.replies
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
        // A function call open (appointments) is not a quiet line; a recording gets no
        // nudge; nor does an appointment's call-back (the AI talks only when the
        // callee has). An errand's call does get one.
        if persona
            && !cb_once
            && first_downlink_at.is_some()
            && !goodbye_pending
            && tool_open.is_empty()
            && !errand_asking
            && ivr_since.is_none()
            && !urgent_ctl.as_ref().is_some_and(|c| c.ai_quiet())
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
                in_spoken_language(persona_stall_idle_nudge_client_content(), errand.is_none().then(|| call_language.spoken()))
            } else {
                in_spoken_language(persona_idle_nudge_client_content(), errand.is_none().then(|| call_language.spoken()))
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
            && !errand_asking
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
                                        // An errand: ask_owner, answered on its own task (the
                                        // owner may take up to ASK_WAIT).
                                        if let Some(e) = errand.clone() {
                                            if call.name != crate::errands::ASK_OWNER {
                                                warn!(
                                                    "Gemini Live toolCall the errand call did not declare serial={serial} name={}",
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
                                            }
                                            // Always blocking: the AI waits for the
                                            // owner and settles nothing meanwhile.
                                            let blocking = true;
                                            info!(
                                                "Gemini Live toolCall serial={serial} name={} id={key} blocking={blocking} (errand: ask the owner) talk_s={}",
                                                call.name,
                                                session_start.elapsed().as_secs()
                                            );
                                            model_turn_pending = true;
                                            tool_open.insert(key.clone());
                                            ask_waiting.insert(key.clone());
                                            let tx = tool_tx.clone();
                                            let gen = live_gen;
                                            tool_tasks.retain(|h| !h.is_finished());
                                            tool_tasks.push(tokio::spawn(async move {
                                                let response = e.ask(&call.args).await;
                                                let _ = tx.send(ToolDone {
                                                    gen,
                                                    key,
                                                    id: call.id,
                                                    name: call.name,
                                                    response,
                                                    blocking,
                                                    after_speech: None,
                                                });
                                            }));
                                            continue;
                                        }
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
                                        // An errand's question still waits for the owner:
                                        // the AI is told so, and gets the answer as a note.
                                        if ask_waiting.contains(&id) {
                                            model_turn_pending = true;
                                            if let Err(e) = send_text_before_deadline(
                                                &mut sink,
                                                crate::errands::still_asking_client_content(),
                                                tokio::time::Instant::now() + API_SEND_TIMEOUT,
                                                "gemini errand still-asking note send",
                                            )
                                            .await
                                            {
                                                warn!("Gemini Live errand still-asking note failed serial={serial}: {e:#}");
                                            }
                                        }
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
                                // An appointment's call-back: this reply of the model's is held
                                // off the line until the callee has really spoken (CallbackFloor).
                                let cb_hold = cb_once
                                    && if has_audio { !cb_floor.model_audio() } else { !cb_floor.on_line };
                                if cb_hold && has_audio && cb_held_mark.is_none() {
                                    cb_held_mark = transcript_out
                                        .as_ref()
                                        .and_then(|b| b.lock().ok().map(|g| g.len()));
                                }
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
                                // An errand call: our own words read back in a script none of
                                // the AI's languages use are a speech-to-text slip (「七」 came
                                // back as 「सात」), left out of the captions and the record —
                                // unless the other side speaks that script.
                                if errand.is_some() && extract_transcription_snippets(&v).0.iter().any(|s| s.chars().any(other_script)) {
                                    caller_other_script = true;
                                }
                                let ai_stt_slip = if errand.is_some() && !caller_other_script {
                                    output_transcription_text(&v)
                                        .filter(|t| t.chars().any(other_script))
                                        .map(|t| (t.to_string(), without_other_script(t)))
                                } else {
                                    None
                                };
                                let v_clean;
                                let v_tx = if (out_leak_turn || repeat_drop || intl_drop) && output_transcription_text(&v).is_some() {
                                    v_clean = without_output_transcription(&v);
                                    &v_clean
                                } else if let Some((raw, fixed)) = &ai_stt_slip {
                                    info!("Errand: AI transcript in a script the call does not use serial={serial} raw={raw} kept={fixed:?}", raw = Said(raw), fixed = fixed.as_deref().map(Said));
                                    v_clean = match fixed {
                                        Some(t) => with_output_transcription(&v, t),
                                        None => without_output_transcription(&v),
                                    };
                                    &v_clean
                                } else {
                                    &v
                                };
                                append_transcriptions(v_tx, &transcript_out);
                                let (in_tx, out_tx) = extract_transcription_snippets(v_tx);
                                // An errand: live captions for the owner's web console.
                                if let Some(e) = &errand {
                                    for s in &in_tx {
                                        e.caption(true, s);
                                    }
                                    for s in &out_tx {
                                        e.caption(false, s);
                                    }
                                }
                                // Prefer a COMPLETE recap (matter filled in) before any hangup.
                                // Greeting 「我幫您轉達」 and trailing 「關於」 must not arm.
                                // Arm as soon as STT shows a complete recap — do not wait for a
                                // later turnComplete (Live STT often arrives after generationComplete).
                                for s_raw in &out_tx {
                                    // The snippet as it came (a new word starts with a space) goes into
                                    // the windows; everything else reads it trimmed.
                                    let s = s_raw.trim();
                                    let s_raw = tx_carry(&mut out_space_held, s_raw);
                                    let s_raw = s_raw.as_str();
                                    push_tx_window(&mut out_tx_window, s_raw, 800);
                                    push_tx_window(&mut turn_out_text, s_raw, 400);
                                    // Whitelist urgent call: dialing is decided by the AI saying its hold-on
                                    // line; the keyword table only lets weak hold-on phrases ("hold on",
                                    // "ちょっと待って") and a repeated line count too.
                                    if let Some(ctl) = urgent_ctl.as_ref() {
                                        let caller_urgent = is_urgent_whitelist_turn(&in_tx_window);
                                        let hold = if caller_urgent {
                                            crate::urgent_call::is_hold_line(&turn_out_text)
                                        } else {
                                            crate::urgent_call::is_strong_hold_line(&turn_out_text)
                                        };
                                        if ctl.hold_said_at().is_none() && hold {
                                            info!("Whitelist urgent call: the AI said its hold-on line serial={serial}");
                                            ctl.note_hold_line_from(turn_base_bytes);
                                        }
                                        if caller_urgent {
                                            push_tx_window(&mut ai_after_urgent, s_raw, 800);
                                            if ctl.hold_said_at().is_none() && crate::urgent_call::repeats_a_line(&ai_after_urgent) {
                                                info!(
                                                    "Whitelist urgent call: the AI said the same line twice after the urgent words serial={serial} — taken as its hold-on line"
                                                );
                                                ctl.note_hold_line_from(turn_base_bytes);
                                            }
                                        }
                                        if ctl.hold_said_at().is_some() && !urgent_transfer_fired {
                                            urgent_transfer_fired = true;
                                            if let Some(ut) = stall.as_ref().and_then(|c| c.urgent_transfer.as_ref()) {
                                                let said = if !in_tx_window.trim().is_empty() {
                                                    in_tx_window.as_str()
                                                } else {
                                                    caller_text_all.as_str()
                                                };
                                                info!(
                                                    "Whitelist urgent call: AI hold-on line triggered urgent transfer/alert serial={serial} chars={}",
                                                    said.chars().count()
                                                );
                                                (ut.0)(said);
                                            }
                                        }
                                    } else if !urgent_transfer_fired {
                                        // Alert-only (no urgent call to the owner): checked once the AI
                                        // starts replying so the caller's full sentence is in `in_tx_window`.
                                        if let Some(ut) = stall.as_ref().and_then(|c| c.urgent_transfer.as_ref()) {
                                            if is_urgent_whitelist_turn(&in_tx_window) {
                                                urgent_transfer_fired = true;
                                                info!(
                                                    "Whitelist caller reported an urgent matter serial={serial} chars={} — triggering urgent transfer/alert",
                                                    in_tx_window.chars().count()
                                                );
                                                (ut.0)(&in_tx_window);
                                            }
                                        }
                                    }
                                    if cb_hold {
                                        // Its closing counts once it plays.
                                        if cb_floor.held.is_some() {
                                            push_tx_window(&mut cb_held_text, s_raw, 400);
                                        }
                                        continue;
                                    }
                                    if persona {
                                        // Words of a turn already over (the transcript lags the
                                        // audio): judged as that turn — the AI's 「晚點再打給你」
                                        // after the caller's wrap-up closes the call.
                                        if errand.is_none() && !model_generating {
                                            let ev = closing_sm.on_ai_late(s, turn_ends_with_closing, stall_on && !wrap_up_sent);
                                            if ev == crate::closing::ClosingEvent::Withdrawn && goodbye_pending {
                                                goodbye_pending = false;
                                                caller_end_signal = false;
                                                output_goodbye_at = None;
                                                end_reason = SessionEndReason::WsClosed;
                                                info!(
                                                    "Persona closing withdrawn (the AI asked something after its closing words) serial={serial}"
                                                );
                                            } else if ev == crate::closing::ClosingEvent::FarewellsExchanged
                                                && arm_on_farewells_exchanged(
                                                    !callback,
                                                    &mut goodbye_pending,
                                                    &mut caller_end_signal,
                                                    &mut end_reason,
                                                )
                                            {
                                                info!(
                                                    "Persona farewells exchanged after the caller's pre-closing (AI words came after its turn) serial={serial} text={s} — hang up once our words have played",
                                                    s = Said(s)
                                                );
                                            }
                                        }
                                        // Natural closing from the model ends the call; no recap gate.
                                        // Never on the opening line (caller has not spoken yet).
                                        let closing = looks_like_persona_closing(s)
                                            || looks_like_persona_closing(&out_tx_window);
                                        if closing
                                            && !callback
                                            && !wrap_up_sent
                                            && only_carrier_prompt_so_far(
                                                &caller_text_all,
                                                carrier_prompt_until,
                                                last_caller_voice_at,
                                            )
                                        {
                                            if !goodbye_pending {
                                                info!(
                                                    "Persona closing ignored serial={serial} — caller only played the carrier international-call prompt text={s}",
                                                    s = Said(s)
                                                );
                                            }
                                            continue;
                                        }
                                        // Stalling: our 掰掰 must not reach a sales / scam caller.
                                        // The transcript runs ahead of the phone's playout,
                                        // so the rest of this reply (the farewell) is dropped
                                        // and the model is told to hold them with a question.
                                        if stall_on && !wrap_up_sent {
                                            push_tx_window(&mut stall_tx, s_raw, 200);
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
                                                        in_spoken_language(persona_stall_keep_with_retain(true, &mut stall_engine, serial), Some(call_language.spoken())),
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
                                                    if stall_keep_after_turn { "after this generation" } else { "sent" },
                                                    preview = Said(&preview)
                                                );
                                            }
                                        }
                                        if closing && stall_on && !wrap_up_sent {
                                            continue;
                                        }
                                        if closing && (caller_had_voice || wrap_up_sent) {
                                            if !goodbye_pending {
                                                info!(
                                                    "Persona closing armed serial={serial} wrap_up={wrap_up_sent} text={s}",
                                                    s = Said(s)
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
                                                "Complete relay recap serial={serial} text={s}",
                                                s = Said(s)
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
                                            "Matter-less recap — not arming hangup serial={serial} text={s}",
                                            s = Said(s)
                                        );
                                    } else if looks_like_incomplete_relay(s)
                                        || looks_like_incomplete_relay(&out_tx_window)
                                    {
                                        info!(
                                            "Incomplete recap trailing 關於 — not arming hangup serial={serial} text={s}",
                                            s = Said(s)
                                        );
                                    } else if looks_like_backchannel(s)
                                        || looks_like_backchannel(&out_tx_window)
                                    {
                                        info!(
                                            "Backchannel without recap — not arming hangup serial={serial} text={s}",
                                            s = Said(s)
                                        );
                                    }
                                    let farewell = looks_like_goodbye(s)
                                        || looks_like_goodbye(&out_tx_window);
                                    if farewell && !saw_relay_ack {
                                        info!(
                                            "Goodbye phrase without complete relay ack — not arming hangup serial={serial} text={s}",
                                            s = Said(s)
                                        );
                                    }
                                }
                                // Caller farewell also arms hangup (after model turn / idle).
                                for s_raw in &in_tx {
                                    let s = s_raw.trim();
                                    let s_raw = tx_carry(&mut in_space_held, s_raw);
                                    let s_raw = s_raw.as_str();
                                    if cb_once {
                                        cb_floor.callee.words(s_raw);
                                    }
                                    push_tx_window(&mut in_tx_window, s_raw, 800);
                                    push_tx_window(&mut caller_text_all, s_raw, 400);
                                    push_tx_window(&mut caller_recent, s_raw, 400);
                                    // The caller speaks a language the AI's own
                                    // speech-to-text has no hint for: its goodbye in
                                    // it would go unnoticed. Added at the next pause.
                                    if persona && !callback {
                                        if let Some(code) = other_script_lang(&caller_text_all).filter(|c| !ai_stt_codes.contains(c)) {
                                            ai_stt_codes.push(code);
                                            ai_stt_grown = true;
                                            if rotate_due.is_none() {
                                                rotate_due = Some(tokio::time::Instant::now() + AI_STT_ROTATE_FORCE);
                                            }
                                            info!(
                                                "Caller speaks {code} serial={serial} — the AI's speech-to-text gets it at the next pause (now {})",
                                                ai_stt_codes.join(",")
                                            );
                                        }
                                    }
                                    if let Some(ctl) = urgent_ctl.as_ref() {
                                        if ctl.hold_said_at().is_none()
                                            && is_urgent_whitelist_turn(&in_tx_window)
                                            && crate::urgent_call::is_hold_line(&turn_out_text)
                                        {
                                            info!("Whitelist urgent call: the AI said its hold-on line serial={serial}");
                                            ctl.note_hold_line_from(turn_base_bytes);
                                            if !urgent_transfer_fired {
                                                urgent_transfer_fired = true;
                                                if let Some(ut) = stall.as_ref().and_then(|c| c.urgent_transfer.as_ref()) {
                                                    (ut.0)(&in_tx_window);
                                                }
                                            }
                                        }
                                    }
                                    // An errand: a voice menu waits for a person (MENU_WAIT);
                                    // the carrier's recording before the AI said anything ends it.
                                    if let Some(e) = &errand {
                                        if errand_menu_since.is_none() && crate::stall_review::ivr_cue(&in_tx_window) {
                                            errand_menu_since = Some(tokio::time::Instant::now());
                                            errand_menu_chunks = audio_chunks_out;
                                            e.note_menu(true);
                                            info!(
                                                "Errand call: voice menu serial={serial} — the AI waits up to {}s for a person",
                                                crate::errands::MENU_WAIT.as_secs()
                                            );
                                        }
                                        if errand_carrier.is_none()
                                            && first_downlink_at.is_none()
                                            && crate::errands::carrier_message(&in_tx_window)
                                        {
                                            errand_carrier = Some(in_tx_window.clone());
                                        }
                                        // "Redial until answered": a busy line's recording, or
                                        // voicemail, ends the call too (it is dialed again).
                                        if errand_carrier.is_none() && !errand_voicemail && first_downlink_at.is_none() && e.redial() {
                                            if crate::errands::voicemail_message(&in_tx_window) {
                                                errand_voicemail = true;
                                            } else if crate::errands::busy_message(&in_tx_window) {
                                                errand_carrier = Some(in_tx_window.clone());
                                            }
                                        }
                                    }
                                    let hold = crate::locale::asks_to_hold(&caller_recent);
                                    if hold != caller_hold {
                                        caller_hold = hold;
                                        info!(
                                            "Persona caller {} serial={serial} text={s}",
                                            if hold { "asked us to hold on — longer quiet line allowed" } else { "is back" },
                                            s = Said(s)
                                        );
                                    }
                                    if crate::scam_db::has_hard_hit(&in_tx_window) {
                                        if own_matter {
                                            own_matter = false;
                                            stall_score += stall_score_add(&caller_text_all, &mut stall_hits);
                                            info!(
                                                "Persona caller hit scam-only keywords after own-matter phrasing — re-enabling stall check serial={serial} text={s}",
                                                s = Said(s)
                                            );
                                        }
                                    } else if !own_matter && looks_like_own_matter(&in_tx_window) {
                                        own_matter = true;
                                        info!(
                                            "Persona caller follows up on the owner's own matter — no 陪聊 this call serial={serial} stall={stall_on} text={s}",
                                            s = Said(s)
                                        );
                                    }
                                    // A recorded voice menu ("查帳單請按1，轉接專人請按9"): nobody to
                                    // stall; the AI goes quiet and we hang up.
                                    if persona
                                        && stall_play
                                        && ivr_since.is_none()
                                        && ivr_enter.is_none()
                                        && crate::stall_review::ivr_cue(&in_tx_window)
                                    {
                                        ivr_enter = Some("menu words");
                                    }
                                    // Sales / scam keywords: once they cross the line the call gets
                                    // a second look (stall_review) while the AI talks on; its verdict
                                    // decides whether the AI starts stalling.
                                    if stall.is_some() && !stall_on && !stall_detected && !own_matter && ivr_since.is_none() {
                                        stall_score += stall_score_add(&in_tx_window, &mut stall_hits);
                                        let flagged = stall_engages(stall_score, &stall_hits);
                                        let hard = stall_hits.iter().any(|h| crate::scam_db::is_hard(h));
                                        let protected = review_ctx().protected();
                                        if stall_engage_now.is_none()
                                            && stall_review.engage_hard_at_once(flagged, hard, protected)
                                        {
                                            stall_engage_now = Some(judgement_of(
                                                &crate::stall_review::Review::of(crate::stall_review::Verdict::Unavailable),
                                                &stall_hits,
                                                EngageWhy::HardWords,
                                            ));
                                            info!(
                                                "Persona stall by scam-only keywords at once serial={serial} score={stall_score} hits={stall_hits:?}"
                                            );
                                        } else if flagged && stall_review.keyword_rule && stall_engage_now.is_none() {
                                            // The look kept failing: the keywords decide alone, as
                                            // before it existed (a contact only on words only
                                            // scammers say).
                                            if hard || !protected {
                                                stall_engage_now = Some(judgement_of(
                                                    &crate::stall_review::Review::of(crate::stall_review::Verdict::Unavailable),
                                                    &stall_hits,
                                                    EngageWhy::KeywordRule,
                                                ));
                                                info!(
                                                    "Persona stall by keywords alone (the second look kept failing) serial={serial} score={stall_score} hits={stall_hits:?}"
                                                );
                                            }
                                        } else if stall_review.should_ask(flagged, stall_score) {
                                            stall_review.seen(caller_said().1);
                                            spawn_stall_review(
                                                &review_tx,
                                                &review_key,
                                                &transcript_out,
                                                &stall_hits,
                                                review_ctx(),
                                                stall_score,
                                                crate::stall_review::Purpose::Keywords,
                                            );
                                            info!(
                                                "Persona stall flagged serial={serial} score={stall_score} hits={stall_hits:?} talk_s={} — second look n={} (the AI talks on meanwhile)",
                                                session_start.elapsed().as_secs(),
                                                stall_review.asked
                                            );
                                        }
                                    }
                                    // Persona: only how the caller's latest words END counts
                                    // (bye / wrap-up); assistant (relay) mode keeps the old window match.
                                    let mut stall_retain_event = false;
                                    if appt.is_some() && !appt_block_sent && errand.is_none() && crate::appointments::time_talk(s) {
                                        appt_block_due = true;
                                    }
                                    let caller_closing = if persona {
                                        // With this piece at the end even when the window skipped it.
                                        // The closing state machine adds the other languages'
                                        // goodbyes and keeps opening greetings from closing.
                                        let heard = with_latest(&in_tx_window, s);
                                        // AI errands keep the old rule (only caller_wraps_up).
                                        let step = errand
                                            .is_none()
                                            .then(|| closing_sm.on_caller(&heard, s, caller_wraps_up(&heard), stall_on));
                                        caller_end_signal = match &step {
                                            Some(step) => step.ends_call,
                                            None => caller_wraps_up(&heard),
                                        };
                                        // Stalling: their pre-closing or bye is the moment for the
                                        // last bait (stall_script's retain stage); nothing hangs up.
                                        let retain_event = step.as_ref().map(|st| st.event).filter(|ev| {
                                            matches!(
                                                ev,
                                                crate::closing::ClosingEvent::CallerPreClosing
                                                    | crate::closing::ClosingEvent::CallerFarewell
                                            )
                                        });
                                        if let Some(event) = retain_event.filter(|_| stall_on) {
                                            stall_retain_event = true;
                                            if let Some(e) = stall_engine.as_mut() {
                                                if e.on_caller_closing() {
                                                    info!(
                                                        "stall_stage stage=retain tactic=pending typology={} turn={} trigger={:?} serial={serial}",
                                                        e.typology(),
                                                        e.turns(),
                                                        event
                                                    );
                                                }
                                            }
                                        }
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
                                            "Persona caller's goodbye taken back (more words followed) serial={serial} text={s}",
                                            s = Said(s)
                                        );
                                    }
                                    if (caller_closing || stall_retain_event) && persona && stall_on && !wrap_up_sent {
                                        // A sales / scam caller saying bye
                                        // is not let go — the AI keeps asking.
                                        if stall_keep_due.is_none() {
                                            info!(
                                                "Persona stall: caller wants to leave — not hanging up, keep them on the line serial={serial} text={s}",
                                                s = Said(s)
                                            );
                                            stall_keep_due =
                                                Some(tokio::time::Instant::now() + STALL_KEEP_WAIT);
                                            stall_keep_chunks = audio_chunks_out;
                                        }
                                    } else if caller_closing {
                                        // A question still waits for the owner: no time is
                                        // left for it — the AI gets its answer now (absent)
                                        // and says goodbye too.
                                        if let Some(e) = &errand {
                                            if e.close_question() {
                                                info!(
                                                    "Errand: the other side says goodbye while a question waits for the owner — closed serial={serial}"
                                                );
                                            }
                                        }
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
                                            "Goodbye detected text={s} → hangup serial={serial} source=input (wait model turn / delay)",
                                            s = Said(s)
                                        );
                                    }
                                }
                                if v.pointer("/serverContent/interrupted").and_then(|x| x.as_bool())
                                    == Some(true)
                                {
                                    info!(
                                        "Gemini Live interrupted serial={serial} server_vad={server_vad} activity_open={activity_open} muted={mute_model_audio}"
                                    );
                                    if let Some(e) = &errand {
                                        e.turn_done();
                                    }
                                    model_generating = false;
                                    play_head.set_generating(false);
                                    mute_model_audio = false;
                                    repeat_guard.interrupted();
                                    // A call-back reply held off the line answered something
                                    // earlier: dropped (the model answers what comes now).
                                    if cb_once && cb_floor.interrupted() {
                                        cb_drop_now = true;
                                    }
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
                                    if !model_generating {
                                        // A new turn: words coming in now are its own.
                                        closing_sm.ai_started();
                                    }
                                    model_generating = true;
                                    play_head.set_generating(
                                        !mute_model_audio && !repeat_drop && !intl_hold && !intl_drop && !cb_hold,
                                    );
                                }
                                // After a barge-in, the rest of the cut reply is not played.
                                let n = if (mute_model_audio || repeat_drop || intl_drop) && has_audio {
                                    intl_gated_turn |= intl_drop;
                                    0
                                } else if cb_hold && has_audio {
                                    // A call-back: kept until the callee has spoken (CallbackFloor).
                                    intl_gated_turn = true;
                                    let mut capture = |pcm: Vec<u8>| {
                                        let take = pcm.len().min(cb_hold_cap.saturating_sub(prebuf.len()));
                                        if take > 0 {
                                            prebuf.extend_from_slice(&pcm[..take]);
                                            prebuf_chunks = prebuf_chunks.saturating_add(1);
                                        }
                                    };
                                    let _ = handle_server_msg(&v, phone_sr, &mut capture);
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
                                        // Their finished words tell the call's language (lang_lock).
                                        let caller_turn = std::mem::take(&mut caller_recent);
                                        if let Some(now_in) = call_language.caller_turn(&caller_turn) {
                                            info!(
                                                "call_language now={} serial={serial}",
                                                match now_in {
                                                    crate::lang_lock::Spoken::Lang(l) => l.code(),
                                                    crate::lang_lock::Spoken::Other => "other",
                                                }
                                            );
                                        }
                                        info!(
                                            "Gemini Live new model turn after caller serial={serial} (clear recap STT window)"
                                        );
                                        // The caller's turn is over. A look put off until now (they
                                        // had only said who they were, the answer was unsure or did
                                        // not come) goes again; a look still under way saw only
                                        // part of this turn: noted, so its "too early" is asked
                                        // again as soon as it comes back. With no keyword so far, a
                                        // caller who is not a contact is looked at once they have
                                        // said what they want. Stalling: the check whether to let
                                        // them go.
                                        if stall.is_some() && !stall_on && !stall_detected && !own_matter && ivr_since.is_none() {
                                            stall_review.note_caller_turn_end();
                                            if stall_review.should_ask_after_turn() {
                                                spawn_stall_review(
                                                    &review_tx,
                                                    &review_key,
                                                    &transcript_out,
                                                    &stall_hits,
                                                    review_ctx(),
                                                    stall_score,
                                                    stall_review.purpose(),
                                                );
                                                info!(
                                                    "Persona stall second look after the caller's turn serial={serial} score={stall_score} hits={stall_hits:?} talk_s={} n={} purpose={:?}",
                                                    session_start.elapsed().as_secs(),
                                                    stall_review.asked,
                                                    stall_review.purpose()
                                                );
                                            } else if blind_looks {
                                                let (turns, units) = caller_said();
                                                let ctx = review_ctx();
                                                if !ctx.protected() && stall_review.should_ask_blind(units, turns, ctx.flagged_before) {
                                                    spawn_stall_review(
                                                        &review_tx,
                                                        &review_key,
                                                        &transcript_out,
                                                        &stall_hits,
                                                        ctx,
                                                        stall_score,
                                                        crate::stall_review::Purpose::Content,
                                                    );
                                                    info!(
                                                        "Persona call looked at without keywords serial={serial} turns={turns} units={units} score={stall_score} talk_s={} n={}",
                                                        session_start.elapsed().as_secs(),
                                                        stall_review.asked
                                                    );
                                                }
                                            }
                                        } else if stall_on && !own_matter && ivr_since.is_none() {
                                            stall_review.note_caller_turn_end();
                                            if stall_review.should_ask_confirm(caller_said().1) {
                                                spawn_stall_review(
                                                    &review_tx,
                                                    &review_key,
                                                    &transcript_out,
                                                    &stall_hits,
                                                    review_ctx(),
                                                    stall_score,
                                                    crate::stall_review::Purpose::Confirm,
                                                );
                                                info!(
                                                    "Persona stall check (let the caller go?) serial={serial} talk_s={}",
                                                    session_start.elapsed().as_secs()
                                                );
                                            }
                                        }
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
                                    if let Some(e) = &errand {
                                        e.turn_done();
                                    }
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
                                    if cb_once {
                                        cb_floor.turn_over(tokio::time::Instant::now());
                                    }
                                }
                                if turn_complete && dropped_turn {
                                    let owed = repeat_guard.reply_owed();
                                    repeat_guard.turn_complete();
                                    model_turn_pending = owed;
                                    turn_out_text.clear();
                                    turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
                                    out_leak_turn = false;
                                    if owed {
                                        // The caller spoke during it: it may have been their
                                        // answer — ask for the reply again, as after a switch.
                                        match send_text_before_deadline(
                                            &mut sink,
                                            in_spoken_language(persona_resume_reply_client_content(), errand.is_none().then(|| call_language.spoken())),
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
                                    turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
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
                                                    in_spoken_language(persona_repeat_line_client_content(), errand.is_none().then(|| call_language.spoken())),
                                                    tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                                    "gemini post-rotate re-kick send",
                                                )
                                                .await
                                                {
                                                    Ok(()) => warn!(
                                                        "Gemini Live text-only turn after rotation (0 audio) serial={serial} out_stt={preview} — asked to say it again",
                                                        preview = Said(&preview)
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
                                        if turn_out_text.trim().is_empty() {
                                            // No transcript of the reply (it lags or never came):
                                            // nothing says the caller added anything — keep the close.
                                            info!(
                                                "Persona closing kept (reply had no transcript) serial={serial}"
                                            );
                                        } else if looks_like_persona_closing(&turn_out_text) {
                                            info!(
                                                "Persona closing kept (model closed again) serial={serial} text={preview}",
                                                preview = Said(&preview)
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
                                                "Persona closing disarmed (caller added something new) serial={serial} text={preview}",
                                                preview = Said(&preview)
                                            );
                                        }
                                    }
                                    // Stalling: a farewell was cut from this reply — now that its
                                    // generation is over, the model holds the caller — or the
                                    // reply ended on a closing the transcript check missed
                                    // (「先這樣」): have the model take that back. Once per turn.
                                    if persona && errand.is_none() {
                                        let ev = closing_sm.on_ai_turn(
                                            &turn_out_text,
                                            turn_ends_with_closing(&turn_out_text),
                                            stall_on && !wrap_up_sent,
                                        );
                                        // The caller announced the end, the AI answered with its
                                        // farewell: their latest words close the call now.
                                        if ev == crate::closing::ClosingEvent::FarewellsExchanged
                                            && arm_on_farewells_exchanged(
                                                !callback,
                                                &mut goodbye_pending,
                                                &mut caller_end_signal,
                                                &mut end_reason,
                                            )
                                        {
                                            info!(
                                                "Persona farewells exchanged after the caller's pre-closing serial={serial} — hang up once our words have played"
                                            );
                                        }
                                    }
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
                                            in_spoken_language(persona_stall_keep_with_retain(true, &mut stall_engine, serial), Some(call_language.spoken())),
                                            tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                            "gemini persona stall keep send",
                                        )
                                        .await
                                        {
                                            Ok(()) => info!(
                                                "Persona stall keep nudge serial={serial} n={stall_keeps} why={why:?} text={preview}",
                                                preview = Said(&preview)
                                            ),
                                            Err(e) => warn!(
                                                "Persona stall keep send failed serial={serial}: {e:#}"
                                            ),
                                        }
                                    }
                                    // Stalling: the call's situation is in the stall prompt and the AI
                                    // finds its own way, so a hint (stall_script, asks for no reply)
                                    // goes in only to change course: the AI going round in circles, or
                                    // a leaving caller to keep. A note every turn slowed every stalled
                                    // reply. The caller already talking: a course hint waits (a note
                                    // mid-utterance holds up the reply to it); the keep hint goes out
                                    // regardless.
                                    if keep.is_none()
                                        && persona
                                        && stall_on
                                        && !wrap_up_sent
                                        && ivr_since.is_none()
                                        && !model_turn_pending
                                        && errand.is_none()
                                    {
                                        if let Some(e) = stall_engine.as_mut() {
                                            if let Some(cue) = e.on_ai_turn(&turn_out_text) {
                                                let typology = e.typology().to_string();
                                                let retain = cue.stage == crate::stall_script::Stage::Retain;
                                                if !(cue.looped || retain) || (activity_open && !retain) {
                                                    info!(
                                                        "stall_stage stage={} tactic={} typology={typology} turn={} held={} serial={serial}",
                                                        cue.stage.name(),
                                                        cue.tactic,
                                                        cue.turn,
                                                        if cue.looped || retain { "caller_talking" } else { "script_in_prompt" }
                                                    );
                                                } else {
                                                    match send_text_before_deadline(
                                                        &mut sink,
                                                        in_spoken_language(crate::stall_script::client_note(&cue.text), Some(call_language.spoken())),
                                                        tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                                        "gemini persona stall cue send",
                                                    )
                                                    .await
                                                    {
                                                        Ok(()) => info!(
                                                            "stall_stage stage={} tactic={} typology={typology} turn={} loop={} serial={serial}",
                                                            cue.stage.name(),
                                                            cue.tactic,
                                                            cue.turn,
                                                            cue.looped
                                                        ),
                                                        Err(e) => warn!("Persona stall cue send failed serial={serial}: {e:#}"),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    // An ordinary call's scenario cue (crate::scenario), once, and —
                                    // times came up — the appointment rules, once: one note that asks
                                    // for no reply, in this pause while the caller is quiet (a note
                                    // mid-utterance holds up the reply to it); never while stalling.
                                    if keep.is_none()
                                        && persona
                                        && errand.is_none()
                                        && !stall_on
                                        && !stall_detected
                                        && !wrap_up_sent
                                        && ivr_since.is_none()
                                        && !model_turn_pending
                                        && !activity_open
                                    {
                                        // In the language the call is held in (lang_lock, memory only);
                                        // a language without a setting (Thai, Italian…) gets the note in
                                        // English, marked to be said in theirs.
                                        let spoken = call_language.lang();
                                        let other = call_language.is_other();
                                        let scenario_cue =
                                            scenario_router.take_pending(stall_engage_now.is_some(), spoken, other);
                                        if let Some((scenario, _)) = &scenario_cue {
                                            if *scenario == crate::scenario::Scenario::ScheduleInvite && !appt_block_sent {
                                                appt_block_due = appt.is_some();
                                            }
                                        }
                                        let appt_note = match appt.as_ref() {
                                            Some(a) if appt_block_due && !appt_block_sent => {
                                                appt_block_sent = true;
                                                Some(a.prompt_note(chrono::Local::now()))
                                            }
                                            _ => None,
                                        };
                                        let note = scenario_cue
                                            .iter()
                                            .map(|(_, cue)| cue.as_str())
                                            .chain(appt_note.as_deref())
                                            .collect::<Vec<_>>()
                                            .join("\n");
                                        if !note.is_empty() {
                                            match send_text_before_deadline(
                                                &mut sink,
                                                in_spoken_language(crate::stall_script::client_note(&note), Some(call_language.spoken())),
                                                tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                                "gemini persona scenario / appointment note send",
                                            )
                                            .await
                                            {
                                                Ok(()) => {
                                                    if let Some((scenario, cue)) = &scenario_cue {
                                                        info!(
                                                            "scenario_route scenario={} action=injected turn={turns_done} chars={} serial={serial}",
                                                            scenario.as_str(),
                                                            cue.chars().count()
                                                        );
                                                    }
                                                    if let Some(n) = &appt_note {
                                                        info!(
                                                            "appointment_note sent chars={} turn={turns_done} serial={serial}",
                                                            n.chars().count()
                                                        );
                                                    }
                                                }
                                                Err(e) => warn!("Persona scenario / appointment note send failed serial={serial}: {e:#}"),
                                            }
                                        }
                                    }
                                    // The AI's turn came out in another language than the call's
                                    // though the caller did not switch: a short reminder in this
                                    // pause, asking for no reply (lang_lock, at most twice a call).
                                    if persona && errand.is_none() && !turn_out_text.trim().is_empty() {
                                        let can_tell = keep.is_none()
                                            && !model_turn_pending
                                            && !activity_open
                                            && !goodbye_pending
                                            && ivr_since.is_none();
                                        if let Some(d) = call_language.ai_turn(&turn_out_text, can_tell) {
                                            let note = crate::prompts::language_drift_note(crate::locale::current(), d.call);
                                            match send_text_before_deadline(
                                                &mut sink,
                                                crate::stall_script::client_note(&note),
                                                tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                                                "gemini persona language drift note send",
                                            )
                                            .await
                                            {
                                                Ok(()) => info!(
                                                    "language_drift ai={} call={} turn={} serial={serial}",
                                                    d.ai.code(),
                                                    d.call.code(),
                                                    d.turn
                                                ),
                                                Err(e) => warn!("Persona language drift note send failed serial={serial}: {e:#}"),
                                            }
                                        }
                                    }
                                    turn_out_text.clear();
                                    turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
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
                                            "gemini transcript-only turn (0 audio) after re-kick serial={serial} out_stt={stt_preview}",
                                            stt_preview = Said(&stt_preview)
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
                                        "Gemini Live opening turn text-only (0 audio) serial={serial} out_stt={stt_preview} -- re-kick now (fresh session if it repeats)",
                                        stt_preview = Said(&stt_preview)
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
                                                "Owner-status missing in opening stt={stt_preview} audio_ms={audio_ms} — retry kick serial={serial}",
                                                stt_preview = Said(&stt_preview)
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
                                            "Owner-status still missing after retry stt={stt_preview} — release barge-in serial={serial}",
                                            stt_preview = Said(&stt_preview)
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
                                        "Gemini Live turnComplete serial={serial} out={audio_chunks_out} goodbye_pending={goodbye_pending} complete_recap={saw_relay_ack} after_caller={model_spoke_after_caller} post_ms={post_ms} activity_open={activity_open} out_stt={stt_preview}",
                                        stt_preview = Said(&stt_preview)
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
                let asked = done.name == crate::errands::ASK_OWNER && ask_waiting.remove(&done.key);
                if asked && (done.gen != live_gen || !open) {
                    // The model dropped the call (the other side spoke over the
                    // wait) or the socket changed: the owner's answer still
                    // reaches the AI, as a note.
                    let now = tokio::time::Instant::now();
                    model_turn_pending = true;
                    last_activity_at = now;
                    match send_text_before_deadline(
                        &mut sink,
                        crate::errands::late_answer_client_content(&done.response),
                        now + API_SEND_TIMEOUT,
                        "gemini errand late answer send",
                    )
                    .await
                    {
                        Ok(()) => info!(
                            "Gemini Live errand answer sent as a note serial={serial} id={} response={}",
                            done.key, Said(&done.response.to_string())
                        ),
                        Err(e) => warn!("Gemini Live errand late answer send failed serial={serial}: {e:#}"),
                    }
                    continue;
                }
                if done.gen != live_gen || !open {
                    info!(
                        "Gemini Live function answer dropped serial={serial} name={} id={} (cancelled or older session)",
                        done.name, done.key
                    );
                    continue;
                }
                let now = tokio::time::Instant::now();
                let msg = if done.name == crate::errands::ASK_OWNER {
                    crate::errands::ask_response(&done.id, done.response.clone(), done.blocking)
                } else {
                    crate::appointments::tool_response(&done.id, &done.name, done.response.clone())
                };
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
                        done.name, done.key, done.blocking, Said(&done.response.to_string())
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
                if done.name == crate::appointments::PROPOSE && !appt_block_sent {
                    if let Some(a) = appt.as_ref() {
                        appt_block_sent = true;
                        let note = a.prompt_note(chrono::Local::now());
                        let payload = in_spoken_language(
                            crate::stall_script::client_note(&note),
                            Some(call_language.spoken()),
                        );
                        if let Err(e) = send_text_before_deadline(
                            &mut sink,
                            payload,
                            tokio::time::Instant::now() + KICK_SEND_TIMEOUT,
                            "gemini propose_appointment fallback note send",
                        )
                        .await
                        {
                            warn!("Persona propose_appointment fallback note failed serial={serial}: {e:#}");
                        }
                    }
                }
            }
            // An errand: what the owner typed on the web console during the call.
            said = async {
                match errand_says.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending::<Option<String>>().await,
                }
            } => {
                match said {
                    Some(text) => {
                        let now = tokio::time::Instant::now();
                        model_turn_pending = true;
                        repeat_guard.clear();
                        match send_text_before_deadline(
                            &mut sink,
                            crate::errands::owner_says_client_content(&text),
                            now + KICK_SEND_TIMEOUT,
                            "gemini errand owner note send",
                        )
                        .await
                        {
                            Ok(()) => info!(
                                "Errand call: the owner's words went to the AI serial={serial} chars={}",
                                text.chars().count()
                            ),
                            Err(e) => warn!("Errand owner note send failed serial={serial}: {e:#}"),
                        }
                        last_activity_at = now;
                    }
                    None => errand_says = None,
                }
            }
            verdict = review_rx.recv() => {
                let Some((score, review)) = verdict else { continue };
                if persona && errand.is_none() {
                    let action = scenario_router.on_review(
                        review.scenario,
                        review.verdict,
                        review.confidence,
                        stall_on || stall_detected || stall_engage_now.is_some(),
                    );
                    info!(
                        "scenario_route scenario={} confidence={} ms={} verdict={:?} action={} serial={serial}",
                        // A sales / scam verdict reads as its own scenario in the log.
                        if review.verdict == crate::stall_review::Verdict::ScamOrSales {
                            "scam"
                        } else {
                            review.scenario.as_str()
                        },
                        review.confidence.as_str(),
                        review.ms,
                        review.verdict,
                        action.as_str()
                    );
                }
                if (stall_on || stall_detected) && stall_review.origin != LookOrigin::Confirm {
                    stall_review.pending = false;
                    stall_review.turn_ended = false;
                    if review.verdict == crate::stall_review::Verdict::IvrRecording && stall_play {
                        ivr_enter.get_or_insert("review");
                    }
                    continue;
                }
                let ctx = review_ctx();
                let facts = StallFacts {
                    flagged: stall_engages(stall_score, &stall_hits),
                    hard_hit: stall_hits.iter().any(|h| crate::scam_db::is_hard(h)),
                    protected: ctx.protected(),
                };
                let step = stall_review.decide(&review, score, stall_score, facts);
                info!(
                    "Persona stall review serial={serial} verdict={:?} typology={:?} confidence={} score_asked={score} score_now={stall_score} reviews={} facts={facts:?} → {step:?}",
                    review.verdict,
                    review.typology,
                    review.confidence.as_str(),
                    stall_review.asked
                );
                match step {
                    StallVerdictStep::Engage => {
                        if let Some(a) = &appt {
                            a.book.set_reviewed_normal(false);
                        }
                        scenario_router.drop_pending();
                        stall_engage_now = Some(judgement_of(&review, &stall_hits, stall_review.why));
                    }
                    StallVerdictStep::Release => stall_release_now = true,
                    StallVerdictStep::Ivr if stall_play => {
                        ivr_enter.get_or_insert("review");
                    }
                    StallVerdictStep::Ivr => {}
                    // The caller finished that turn (or said more) meanwhile: what the
                    // look saw was only its start. Look again now, at all of it.
                    StallVerdictStep::AskAgain if stall.is_some() && !stall_on && !stall_detected && !own_matter && ivr_since.is_none() => {
                        spawn_stall_review(
                            &review_tx,
                            &review_key,
                            &transcript_out,
                            &stall_hits,
                            ctx,
                            stall_score,
                            stall_review.purpose(),
                        );
                        info!(
                            "Persona stall second look again at once (the caller's turn ended or more keywords came meanwhile) serial={serial} score={stall_score} hits={stall_hits:?} talk_s={} n={}",
                            session_start.elapsed().as_secs(),
                            stall_review.asked
                        );
                    }
                    // Stalling started, the caller's own matter or a voice menu: nothing to look at.
                    StallVerdictStep::AskAgain => stall_review.pending = false,
                    StallVerdictStep::Carry => {
                        if let Some(a) = &appt {
                            a.book.set_reviewed_normal(true);
                        }
                    }
                    StallVerdictStep::AfterTurn => {}
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
                    Some(_) if urgent_ctl.as_ref().is_some_and(|c| c.ai_quiet()) => {
                        // Whitelist urgent call: the phone is dialing the owner (or the
                        // owner is on): what the line carries is not for the model.
                    }
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
                                                turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
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
                        if cb_once {
                            let cb_voiced = is_voice
                                || (!cb_floor.picked
                                    && frame_voice
                                    && delivered_bytes.load(Ordering::Relaxed) == 0);
                            cb_floor.line_frame(&pcm16, cb_voiced, frame_ms);
                        }
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
                            // An appointment's call-back not picked up yet: a ringing tone is nobody.
                            if voiced_ms >= LOCAL_MIN_VOICE_MS && (!cb_once || cb_floor.turn_may_open()) {
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
                            // Before a call-back's callee has picked up no turn opens
                            // (no activityStart either): Gemini hears silence.
                            if !activity_open
                                && voiced_ms >= LOCAL_MIN_VOICE_MS
                                && (!cb_once || cb_floor.turn_may_open())
                            {
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
                            } else if cb_once && !cb_floor.line_audio(activity_open) {
                                // An appointment's call-back: Gemini hears silence until the callee has
                                // picked up, then their voice only (from its onset, the
                                // preroll sent when their turn opens) — never the ringing
                                // or a quiet line it could take for words.
                                uplink_buf.resize(uplink_buf.len() + pcm16.len(), 0);
                                push_preroll(&mut preroll, &pcm16, preroll_cap);
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
                                turn_base_bytes = delivered_bytes.load(Ordering::Relaxed);
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
    if urgent_ctl.is_none() && !urgent_transfer_fired {
        if let Some(ut) = stall.as_ref().and_then(|c| c.urgent_transfer.as_ref()) {
            if is_urgent_whitelist_turn(&in_tx_window) {
                info!(
                    "Whitelist caller reported an urgent matter before hangup serial={serial} text={in_tx_window} — triggering urgent transfer/alert",
                    in_tx_window = Said(&in_tx_window)
                );
                (ut.0)(&in_tx_window);
            }
        }
    }
    // An errand's question still waiting for the owner is not answered now.
    if let Some(e) = &errand {
        e.end_questions();
    }
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
            .and_then(spaced_snippet)
        {
            // The STT puts the break between two words at the end of one
            // fragment or the start of the next; kept as one space so the merged
            // line keeps word spaces (Korean, accented Latin): see join_fragment.
            // As heard: the owner's reading form is applied once the fragments
            // are joined into lines (merge_dialogue_fragments).
            pieces.push(format!("{label}：{t}"));
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
    fn the_plan_knows_the_calls_region_before_pickup() {
        use crate::country_profile::Region;
        use crate::locale::Lang;
        let none = std::collections::BTreeMap::new();
        let local = call_lang_plan("0912345678", Some("tw"), Lang::ZhTw, "886", &none);
        assert_eq!(local.region, Some(Region::Tw));
        assert_eq!(local.sim_country.as_deref(), Some("tw"));
        // A local number never hears the carrier's warning: its note goes.
        assert!(!carrier_note_due(&local));
        let abroad = call_lang_plan("+12025550143", Some("tw"), Lang::ZhTw, "886", &none);
        assert!(carrier_note_due(&abroad));
        let hk = call_lang_plan("+85291234567", Some("hk"), Lang::ZhTw, "886", &none);
        assert_eq!(hk.region, Some(Region::HkMo));
        assert!(!carrier_note_due(&hk));
        // SIM not known (older app): the language's region, the note kept as before.
        let unknown = call_lang_plan("0912345678", None, Lang::ZhTw, "886", &none);
        assert_eq!(unknown.region, Some(Region::Tw));
        assert!(carrier_note_due(&unknown));
        // Hidden caller on a Taiwan SIM still keeps the carrier prompt note.
        let hidden_tw = call_lang_plan("", Some("tw"), Lang::ZhTw, "886", &none);
        assert!(carrier_note_due(&hidden_tw));
        // A ring that guessed no SIM still serves the local call it picks up.
        assert_eq!(unknown, local);
        // Another region is another prompt.
        let mut other = local.clone();
        other.region = Some(Region::HkMo);
        assert_ne!(other, local);
    }

    #[test]
    fn an_errand_ai_transcript_keeps_only_scripts_its_languages_use() {
        assert_eq!(without_other_script("我們訂सात點").as_deref(), Some("我們訂點"));
        assert_eq!(without_other_script(" सात"), None);
        assert_eq!(without_other_script(" ¿Mesa para cuatro? Ça va, 네, はい、好"), Some(" ¿Mesa para cuatro? Ça va, 네, はい、好".to_string()));
        assert!("सात".chars().any(other_script) && "Привет".chars().any(other_script));
        assert!(!"七點半, Café, 네, はい, 12:30。".chars().any(other_script));
        let v = json!({"serverContent": {"outputTranscription": {"text": "七सात"}}});
        assert_eq!(output_transcription_text(&with_output_transcription(&v, "七")), Some("七"));
    }

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

    fn voice(c: &mut CalleeHeard, frames: u32) {
        for _ in 0..frames {
            c.frame(true, 40);
        }
    }

    fn hush(c: &mut CalleeHeard, frames: u32) {
        for _ in 0..frames {
            c.frame(false, 40);
        }
    }

    #[test]
    fn a_call_back_callee_speaks_only_with_voice_and_words() {
        for t in ["國際電話", "國際電話。", "這是國際電話，請注意小心詐騙", "<noise>", "謝謝觀看", "  "] {
            assert!(!callee_said_words(t), "{t}");
        }
        for t in ["喂？", "Hello?", "國際電話。喂？", "好，謝謝", "もしもし"] {
            assert!(callee_said_words(t), "{t}");
        }
        // A ringing tone and a misheard 「國際電話」: nobody.
        let mut c = CalleeHeard::default();
        voice(&mut c, 50);
        c.words("國際電話");
        assert!(!c.spoke());
        // Words without voice, or clicks under LOCAL_MIN_VOICE_MS: nobody.
        let mut c = CalleeHeard::default();
        c.words("喂？你好");
        assert!(!c.spoke());
        for _ in 0..20 {
            voice(&mut c, 2);
            hush(&mut c, 10);
        }
        assert!(!c.spoke());
        // One short 「喂」 (240 ms) is not enough; a second one is.
        let mut c = CalleeHeard::default();
        voice(&mut c, 6);
        c.words("喂？");
        assert!(!c.spoke());
        hush(&mut c, 12);
        voice(&mut c, 6);
        assert!(c.spoke());
        // Words first, then 400 ms of voice.
        let mut c = CalleeHeard::default();
        c.words("Hello, who is this?");
        voice(&mut c, 10);
        assert!(c.spoke());
        // A tone, a long quiet line, then a stray word: nobody.
        let mut c = CalleeHeard::default();
        voice(&mut c, 25);
        hush(&mut c, 70);
        c.words("嗯");
        assert!(!c.spoke());
        // Asking to hear it again needs less voice.
        let mut c = CalleeHeard::default();
        voice(&mut c, 6);
        c.words("什麼？");
        assert!(c.spoke());
    }

    #[test]
    fn a_call_back_reply_plays_only_after_the_callee_spoke() {
        let t0 = tokio::time::Instant::now();
        let mut f = CallbackFloor::for_pickup();
        // Anything the model said before pickup is held, and never plays then.
        assert!(!f.model_audio());
        f.turn_over(t0);
        assert_eq!(f.step(t0 + Duration::from_millis(500)), FloorStep::Wait);
        match f.step(t0 + CALLBACK_HELD_GRACE) {
            // Dropped, and the model is told nobody heard it (no reply asked for).
            FloorStep::Drop { note } => {
                assert!(note.contains("did not hear") && note.contains("say all of it"), "{note}");
                assert_eq!(serde_json::from_str::<Value>(&note).unwrap()["clientContent"]["turnComplete"], json!(false));
            }
            other => panic!("{other:?}"),
        }
        assert_eq!((f.replies, f.done_after()), (0, None));
        // They pick up and say 「喂？」: the cue goes out, and the answer plays.
        voice(&mut f.callee, 12);
        hush(&mut f.callee, 16);
        assert!(f.pickup().is_some());
        assert!(f.model_audio());
        assert!(f.model_audio(), "the rest of the answer plays");
        f.turn_over(t0);
        assert_eq!((f.replies, f.done_after()), (1, Some(CALLBACK_DONE_QUIET)));
        // Answer over, nobody says anything: a second answer never reaches the line.
        f.callee.words("國際電話");
        hush(&mut f.callee, 75);
        assert!(!f.model_audio());
        f.turn_over(t0);
        match f.step(t0 + CALLBACK_HELD_GRACE) {
            FloorStep::Drop { note } => assert!(note.contains("Say nothing more unless they speak"), "{note}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(f.replies, 1);
        // Words that come while a reply is held: it plays once they do.
        assert!(!f.model_audio());
        voice(&mut f.callee, 12);
        f.callee.words("喂？還在嗎？");
        assert_eq!(f.step(t0), FloorStep::Play { over: false });
        assert!(f.model_audio());
        f.turn_over(t0);
        assert_eq!(f.replies, 2);
        // 「什麼？」: it may say it again.
        voice(&mut f.callee, 6);
        f.callee.words("什麼？");
        assert!(f.model_audio());
        f.turn_over(t0);
        assert_eq!(f.replies, 3);
        // Talked over: a held reply is dropped at once.
        assert!(!f.model_audio());
        assert!(f.interrupted());
        assert_eq!(f.step(t0), FloorStep::Wait);
        // At most CALLBACK_REPLIES_MAX replies; then the call ends a moment after.
        let mut f = CallbackFloor { picked: true, replies: CALLBACK_REPLIES_MAX - 1, ..Default::default() };
        voice(&mut f.callee, 12);
        f.callee.words("好，那就這樣");
        assert!(f.model_audio());
        f.turn_over(t0);
        voice(&mut f.callee, 12);
        f.callee.words("等一下還有一件事");
        assert!(!f.model_audio());
        assert_eq!(f.done_after(), Some(CALLBACK_LAST_REPLY_QUIET));
    }

    #[test]
    fn a_dropped_call_back_reply_leaves_only_the_callee_s_lines() {
        let mut t = String::from("對方：喂？");
        let mark = t.len();
        t.push_str("\n接聽：您好，我是林先生\n對方：國際電話\n接聽：，我回覆您");
        assert_eq!(drop_ai_lines_after(&mut t, mark), "您好，我是林先生，我回覆您");
        assert_eq!(t, "對方：喂？\n對方：國際電話");
        let mut t = String::from("接聽：Hello");
        assert_eq!(drop_ai_lines_after(&mut t, 0), "Hello");
        assert_eq!(t, "");
        let mut t = String::new();
        assert_eq!(drop_ai_lines_after(&mut t, 0), "");
    }

    /// `n` samples at 16 kHz: a sum of sines (Hz, amplitude), phase `from`.
    fn sines(parts: &[(f64, f64)], n: usize, from: usize) -> Vec<u8> {
        (from..from + n)
            .flat_map(|i| {
                let t = i as f64 / 16_000.0;
                let s: f64 = parts.iter().map(|(hz, a)| a * (2.0 * std::f64::consts::PI * hz * t).sin()).sum();
                (s.clamp(-32_000.0, 32_000.0) as i16).to_le_bytes()
            })
            .collect()
    }

    /// A voice-like frame: twenty harmonics of 140 Hz.
    fn voicelike(n: usize, from: usize) -> Vec<u8> {
        let parts: Vec<(f64, f64)> = (1..=20).map(|h| (140.0 * h as f64, 700.0)).collect();
        sines(&parts, n, from)
    }

    #[test]
    fn ringing_tones_are_told_from_a_voice() {
        for n in [320, 640] {
            for tone in [vec![(440.0, 9_000.0), (480.0, 9_000.0)], vec![(425.0, 12_000.0)], vec![(450.0, 12_000.0)], vec![(400.0, 12_000.0)]] {
                assert!(ringback_like(&sines(&tone, n, 0)), "{tone:?} n={n}");
            }
            assert!(!ringback_like(&voicelike(n, 0)), "n={n}");
            assert!(!ringback_like(&sines(&[(1_000.0, 9_000.0)], n, 0)), "n={n}");
        }
        assert!(!ringback_like(&[0u8; 640]));
        assert!(!ringback_like(&sines(&[(440.0, 9_000.0)], 80, 0)), "too short to tell");
    }

    #[test]
    fn a_call_back_line_stays_silent_until_the_callee_picks_up() {
        let mut f = CallbackFloor::for_pickup();
        let ring = [(440.0, 9_000.0), (480.0, 9_000.0)];
        let mut at = 0;
        // Ten seconds of ringing (1 s on, 2 s off), all of it loud enough for the VAD:
        // Gemini gets silence, no turn opens, and the model gets no cue.
        for _ in 0..3 {
            for _ in 0..25 {
                f.line_frame(&sines(&ring, 640, at), true, 40);
                at += 640;
                assert!(!f.line_audio(true) && !f.line_audio(false) && !f.turn_may_open());
                assert!(f.pickup().is_none());
            }
            for _ in 0..50 {
                f.line_frame(&[0u8; 640], false, 40);
                assert!(f.pickup().is_none());
            }
        }
        // 「喂？」 while still talking: not yet (the AI does not start over them).
        for _ in 0..12 {
            f.line_frame(&voicelike(640, at), true, 40);
            at += 640;
            assert!(f.pickup().is_none() && !f.line_audio(true) && !f.turn_may_open());
        }
        // They pause: the cue goes out, once, and from now on the line reaches Gemini.
        let mut cue = None;
        for _ in 0..16 {
            f.line_frame(&[0u8; 640], false, 40);
            cue = cue.or_else(|| f.pickup());
        }
        let cue = cue.expect("picked up");
        assert!(cue.contains("picked up and spoken") && cue.contains("all of it, once"), "{cue}");
        assert_eq!(serde_json::from_str::<Value>(&cue).unwrap()["clientContent"]["turnComplete"], json!(true));
        assert!(f.pickup().is_none());
        assert!(f.turn_may_open() && f.line_audio(true) && !f.line_audio(false));

        // A crisp ~280 ms "Hello?" / 「喂？」 (7 x 40 ms < 400 ms) followed by silence
        // also picks up, while a 120 ms click/breath does not.
        let mut short = CallbackFloor::for_pickup();
        for _ in 0..3 {
            short.line_frame(&voicelike(640, at), true, 40);
            at += 640;
        }
        for _ in 0..20 {
            short.line_frame(&[0u8; 640], false, 40);
            assert!(short.pickup().is_none(), "120 ms click must not pick up");
        }
        for _ in 0..7 {
            short.line_frame(&voicelike(640, at), true, 40);
            at += 640;
            assert!(short.pickup().is_none());
        }
        let mut short_cue = None;
        for _ in 0..16 {
            short.line_frame(&[0u8; 640], false, 40);
            short_cue = short_cue.or_else(|| short.pickup());
        }
        assert!(short_cue.is_some(), "280 ms Hello? + pause must pick up");
    }

    #[test]
    fn the_call_s_language_is_what_was_said_on_it() {
        use crate::locale::Lang;
        // A US number, opened in English, held in Chinese.
        let d = "接聽：Hello?\n對方：喂，你好，我想約林先生明天中午吃飯\n接聽：好啊，明天中午可以，我確認一下時間再回你。\n對方：好，謝謝，掰掰\n接聽：掰掰";
        assert_eq!(dialogue_lang(d, Lang::ZhTw), Some(Lang::ZhTw));
        assert_eq!(dialogue_lang(d, Lang::ZhCn), Some(Lang::ZhCn));
        assert_eq!(dialogue_lang(d, Lang::En), Some(Lang::ZhTw));
        let en = "接聽：喂？\n對方：Hi, is this Mr. Lin? I would like to book a table for Sunday.\n接聽：Sure, Sunday at noon works for me, I will check and get back to you.";
        assert_eq!(dialogue_lang(en, Lang::ZhTw), Some(Lang::En));
        assert_eq!(dialogue_lang("接聽：はい、日曜日の十二時で大丈夫です。", Lang::ZhTw), Some(Lang::Ja));
        assert_eq!(dialogue_lang("接聽：네, 일요일 열두 시 괜찮습니다.", Lang::ZhTw), Some(Lang::Ko));
        assert_eq!(dialogue_lang("接聽：Sí, el domingo a las doce está muy bien, gracias.", Lang::ZhTw), Some(Lang::Es));
        assert_eq!(dialogue_lang("接聽：Oui, dimanche à midi c'est parfait, merci.", Lang::ZhTw), Some(Lang::Fr));
        // Nothing the AI said tells: the caller's lines do.
        assert_eq!(dialogue_lang("接聽：Hello?\n對方：Hola, quiero hacer una reserva para el domingo.", Lang::ZhTw), Some(Lang::Es));
        assert_eq!(dialogue_lang("接聽：喂？\n對方：喂？", Lang::ZhTw), None);
        assert_eq!(dialogue_lang("", Lang::ZhTw), None);
    }

    #[test]
    fn only_an_appointment_call_back_says_its_answer_once() {
        use crate::locale::{Active, Lang};
        let call_back = Outbound::of(true, false);
        let errand = Outbound::of(true, true);
        assert_eq!(
            (call_back, errand, Outbound::of(false, false)),
            (Outbound::CallBack, Outbound::Errand, Outbound::Incoming)
        );
        // The one-answer floor (held replies, the pickup gate, CALLBACK_REPLIES_MAX,
        // the quiet hang-up) is the appointment call-back's alone.
        assert!(call_back.answers_once());
        assert!(!errand.answers_once() && !Outbound::Incoming.answers_once());
        // Speech-to-text: a call-back's callee and a caller get no vocabulary; an
        // errand's other side gets the call's languages and the owner's name.
        let me = Active { owner: "王先生".into(), lang: Lang::ZhTw, ..Default::default() };
        let langs = stt_codes(&[Lang::ZhTw, Lang::En]);
        assert_eq!(call_back.other_side_stt(Some(&langs), None, &me), stt_callee_config(Some(&langs)));
        let errand_stt = errand.other_side_stt(Some(&langs), None, &me);
        assert_eq!(errand_stt, stt_errand_config(None, &me));
        assert!(errand_stt.to_string().contains("王先生"), "{errand_stt}");
        let incoming = Outbound::Incoming.other_side_stt(Some(&langs), None, &me);
        assert_eq!(incoming, stt_caller_config_for(Some(&langs), &me));
        assert_ne!(incoming, errand_stt);
    }

    #[test]
    fn an_errand_s_other_side_is_heard_without_carrier_words() {
        use crate::locale::{Active, Lang};
        // An errand opened in English by a Chinese-reading owner: both languages
        // as hints, the owner's name as vocabulary, never the carrier's warning.
        let me = Active { owner: "王先生".into(), lang: Lang::ZhTw, notify_lang: Lang::ZhTw, ..Default::default() };
        let cfg = Outbound::Errand.other_side_stt(None, Some(Lang::En), &me);
        assert_eq!(cfg["languageCodes"], json!(stt_codes(&[Lang::En, Lang::ZhTw])));
        assert_eq!(cfg["customVocabulary"], json!(["王先生"]));
        assert!(!cfg.to_string().contains("國際電話"), "{cfg}");
        // The same hints on every errand, whatever the call-back plan says.
        let langs = stt_codes(&[Lang::ZhTw]);
        assert_eq!(Outbound::Errand.other_side_stt(Some(&langs), Some(Lang::En), &me), cfg);
        // Opening in the setting's language, owner reading English: the same two
        // languages once each.
        let en_owner = Active { owner: "Mr. Wang".into(), lang: Lang::ZhTw, notify_lang: Lang::En, ..Default::default() };
        let cfg = Outbound::Errand.other_side_stt(None, None, &en_owner);
        assert_eq!(cfg["languageCodes"], json!(stt_codes(&[Lang::ZhTw, Lang::En])));
        assert!(!cfg.to_string().contains("國際電話"), "{cfg}");
        // The owner's name only goes in when the hints include the owner's language.
        let en_only = Active { owner: "王先生".into(), lang: Lang::ZhTw, notify_lang: Lang::En, ..Default::default() };
        let cfg = Outbound::Errand.other_side_stt(None, Some(Lang::En), &en_only);
        assert_eq!(cfg["languageCodes"], json!(stt_codes(&[Lang::En])));
        assert!(cfg.get("customVocabulary").is_none(), "{cfg}");
        // Non-setting target country adds its BCP-47 hint for the callee side.
        let in_cfg = Outbound::Errand.other_side_stt_for(None, None, Some("hi-IN"), &me);
        assert_eq!(in_cfg["languageCodes"], json!(["zh-TW", "hi-IN"]));
    }

    #[test]
    fn a_call_back_s_languages_follow_the_call_it_answers() {
        use crate::locale::Lang;
        // Spoke Chinese from a US number on a US SIM (that alone opens in English).
        let plan = callback_lang_plan(Lang::ZhTw, Lang::ZhTw);
        assert_eq!(plan.opening, None);
        assert_eq!(plan.caller_stt, Some(stt_codes(&[Lang::ZhTw, Lang::En])));
        let en = callback_lang_plan(Lang::En, Lang::ZhTw);
        assert_eq!((en.opening, en.caller_stt), (Some(Lang::En), Some(stt_codes(&[Lang::En]))));
        // The callee's speech-to-text has no vocabulary.
        let cfg = stt_callee_config(plan.caller_stt.as_deref());
        assert!(cfg.get("customVocabulary").is_none());
        assert_eq!(cfg["languageCodes"], json!(stt_codes(&[Lang::ZhTw, Lang::En])));
        assert_eq!(stt_callee_config(None), json!({}));
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
        // That quiet is past our last words already: the regional delay has run.
        assert_eq!(hangup_delay_left(Duration::from_millis(1_500), true), Duration::ZERO);
        assert_eq!(hangup_delay_left(Duration::from_millis(1_500), false), Duration::from_millis(1_500));
        // An owner's longer delay keeps what the quiet did not cover.
        assert_eq!(hangup_delay_left(Duration::from_millis(8_000), true), Duration::from_millis(2_000));
    }

    /// The closing state machine only adds hangups: whatever the older check
    /// closes on, it ends the call too (opening greetings aside), on the first
    /// turn and after some talk.
    #[test]
    fn the_closing_machine_ends_the_call_whenever_the_older_check_does() {
        let wraps = [
            "掰掰", "多謝，掰掰", "那就先這樣", "那我跟你說掰掰喔", "好，先咁啦", "唔該你，再見", "Okay, that's all",
            "Okay, bye to you", "Alright, that's all, thank you so much", "以上です", "それでは失礼します。",
            "では、よろしくお願いいたします。", "Vale, perfecto. Nos vemos.", "Bueno, adiós.",
            "かしこまりました。それでは、管理室の方にお預けしておきますね。ご利用ありがとうございました。",
            "お忙しいところありがとうございました",
        ];
        for t in wraps {
            assert!(caller_wraps_up(t), "{t}");
            let mut first = crate::closing::ClosingMachine::new("zh-TW");
            assert!(first.on_caller(t, t, caller_wraps_up(t), false).ends_call, "first turn: {t}");
            let mut later = crate::closing::ClosingMachine::new("zh-TW");
            let talk = "我是宅配司機，明天下午有個包裹要送過去給王先生";
            later.on_caller(talk, talk, caller_wraps_up(talk), false);
            assert!(later.on_caller(t, t, caller_wraps_up(t), false).ends_call, "after talk: {t}");
        }
    }

    /// Keyword stage of the sales / scam check ([stall_score_add], [stall_engages]
    /// or a hard hit): a bank asking to approve a transfer in its app is no scam;
    /// an "expiring Medicare card" asking for the number is.
    #[test]
    fn bank_app_approval_is_ordinary_and_an_expiring_medicare_card_is_a_scam() {
        let engages = |said: &str| {
            let mut hits = Vec::new();
            let score = stall_score_add(said, &mut hits);
            stall_engages(score, &hits) || crate::scam_db::has_hard_hit(said)
        };
        assert!(!engages(
            "Guten Tag, hier ist Ihre Bank. Bitte bestätigen Sie die Freigabe in der App, wenn die Überweisung von Ihnen stammt"
        ));
        assert!(!engages("ATMで手続きできます。銀行の窓口でも大丈夫です。"));
        assert!(engages("Your Medicare card is expiring, please give your Medicare number"));
        for kw in ["到atm前面操作解除", "검찰청 안전계좌", "pay the irs in gift cards", "irs agent will arrest you", "medicare card is expiring"] {
            assert!(crate::scam_db::keyword(kw).is_some(), "{kw} not in the scam database");
        }
    }

    #[test]
    fn a_goodbye_that_turns_into_a_question_is_taken_back() {
        // 「那就先這樣」 came in first and armed the caller's goodbye…
        assert!(caller_wraps_up("那就先這樣"));
        // …then 「嗎？」: the AI answers instead of hanging up after it.
        assert!(caller_withdraws_bye("那就先這樣嗎？", "嗎？"));
        assert!(caller_wraps_up("Okay, that's all"));
        assert!(caller_withdraws_bye("Okay, that's all for today, right?", " for today, right?"));
        assert!(caller_wraps_up("以上です"));
        assert!(caller_withdraws_bye("以上ですよね？", "よね？"));
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
        assert_eq!(crate::stall_review::REVIEW_TIMEOUT, Duration::from_millis(3_000));
    }

    fn review_of(verdict: crate::stall_review::Verdict, confidence: crate::stall_review::Confidence) -> crate::stall_review::Review {
        crate::stall_review::Review { confidence, ..crate::stall_review::Review::of(verdict) }
    }

    /// A look that does not answer (timeout, error) convicts nobody on its own:
    /// only words only scammers say engage at once; otherwise the next turn is
    /// looked at, and after two failures in a row the keywords decide as before.
    #[test]
    fn a_failed_look_convicts_only_on_hard_words_or_after_failing_twice() {
        use crate::stall_review::{Confidence::*, Verdict::*};
        let flagged = StallFacts { flagged: true, ..StallFacts::default() };
        let failed = review_of(Unavailable, Medium);
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.decide(&failed, 3, 3, flagged), StallVerdictStep::AfterTurn);
        assert!(r.should_ask_after_turn());
        assert_eq!(r.decide(&failed, 3, 3, flagged), StallVerdictStep::Engage);
        assert_eq!(r.why, EngageWhy::KeywordRule);
        // Hard words: at once.
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        let hard = StallFacts { flagged: true, hard_hit: true, ..StallFacts::default() };
        assert_eq!(r.decide(&failed, 3, 3, hard), StallVerdictStep::Engage);
        assert_eq!(r.why, EngageWhy::HardWords);
        // A saved contact or a number marked safe is left alone…
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        let contact = StallFacts { flagged: true, protected: true, ..StallFacts::default() };
        assert_eq!(r.decide(&failed, 3, 3, contact), StallVerdictStep::Carry);
        // …unless it said words only scammers say (caller ids can be spoofed).
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        let spoofed = StallFacts { flagged: true, hard_hit: true, protected: true };
        assert_eq!(r.decide(&failed, 3, 3, spoofed), StallVerdictStep::Engage);
        // No key: asking again will not help, so the keywords decide at once.
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        let no_key = crate::stall_review::Review { no_key: true, ..failed };
        assert_eq!(r.decide(&no_key, 3, 3, flagged), StallVerdictStep::Engage);
        // A call the keywords did not flag is not engaged by the keyword rule.
        let mut r = StallReview::default();
        assert!(r.should_ask_blind(30, 2, false));
        assert_eq!(r.decide(&no_key, 0, 0, StallFacts::default()), StallVerdictStep::Carry);
    }

    /// An unsure "scam" is looked at again after the caller's next turn; unsure
    /// twice, it engages only when the keywords also say so.
    #[test]
    fn an_unsure_scam_answer_waits_for_the_next_turn() {
        use crate::stall_review::{Confidence::*, Verdict::*};
        let unsure = review_of(ScamOrSales, Low);
        let flagged = StallFacts { flagged: true, ..StallFacts::default() };
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.decide(&unsure, 3, 3, flagged), StallVerdictStep::AfterTurn);
        assert!(r.should_ask_after_turn());
        assert_eq!(r.decide(&unsure, 3, 3, flagged), StallVerdictStep::Engage);
        // Without keywords behind it: let be.
        let mut r = StallReview::default();
        assert!(r.should_ask_blind(30, 2, false));
        assert_eq!(r.decide(&unsure, 0, 0, StallFacts::default()), StallVerdictStep::AfterTurn);
        assert!(r.should_ask_after_turn());
        assert_eq!(r.decide(&unsure, 0, 0, StallFacts::default()), StallVerdictStep::Carry);
        // A medium "scam" is enough.
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.decide(&review_of(ScamOrSales, Medium), 3, 3, flagged), StallVerdictStep::Engage);
    }

    /// Words only scammers say and an unsure "normal" (a spoofed contact asking
    /// for "the code"): looked at once more, a scam when unsure twice; a sure
    /// "normal" holds.
    #[test]
    fn hard_words_need_a_sure_normal_to_be_let_be() {
        use crate::stall_review::{Confidence::*, Verdict::*};
        let hard = StallFacts { flagged: true, hard_hit: true, protected: true };
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.decide(&review_of(Normal, Medium), 3, 3, hard), StallVerdictStep::AfterTurn);
        assert!(r.should_ask_after_turn());
        assert_eq!(r.decide(&review_of(Normal, Low), 3, 3, hard), StallVerdictStep::Engage);
        assert_eq!(r.why, EngageWhy::HardWords);
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.decide(&review_of(Normal, High), 3, 3, hard), StallVerdictStep::Carry);
        // Without hard words an unsure "normal" is still normal.
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        let flagged = StallFacts { flagged: true, ..StallFacts::default() };
        assert_eq!(r.decide(&review_of(Normal, Low), 3, 3, flagged), StallVerdictStep::Carry);
    }

    /// Calls no keyword flagged are looked at too: after the caller's second turn
    /// with something in it (or enough said), then again only when they have said
    /// more, at most three times a call; a number marked before is looked at sooner.
    #[test]
    fn calls_without_keywords_are_looked_at_a_few_times() {
        use crate::stall_review::{Confidence::*, Verdict::*};
        let normal = review_of(Normal, High);
        let mut r = StallReview::default();
        assert!(!r.should_ask_blind(8, 1, false));
        assert!(r.should_ask_blind(10, 2, false));
        assert!(!r.should_ask_blind(12, 3, false), "one at a time");
        assert_eq!(r.decide(&normal, 0, 0, StallFacts::default()), StallVerdictStep::Carry);
        assert!(!r.should_ask_blind(10 + BLIND_NEXT_UNITS - 1, 3, false), "nothing new said");
        assert!(r.should_ask_blind(10 + BLIND_NEXT_UNITS, 3, false));
        assert_eq!(r.decide(&normal, 0, 0, StallFacts::default()), StallVerdictStep::Carry);
        assert!(r.should_ask_blind(10 + 2 * BLIND_NEXT_UNITS, 4, false));
        assert_eq!(r.decide(&normal, 0, 0, StallFacts::default()), StallVerdictStep::Carry);
        assert!(!r.should_ask_blind(500, 9, false), "at most {BLIND_MAX} a call");
        // A number marked or judged before: after the first turn.
        let mut r = StallReview::default();
        assert!(r.should_ask_blind(6, 1, true));
        assert_eq!(r.decide(&review_of(ScamOrSales, High), 0, 0, StallFacts::default()), StallVerdictStep::Engage);
    }

    /// Once stalling, one check after the caller's next two turns: only a sure
    /// "normal" lets them go back to an ordinary call.
    #[test]
    fn a_wrongly_judged_caller_is_let_go_on_a_sure_normal() {
        use crate::stall_review::{Confidence::*, Verdict::*};
        let mut r = StallReview::default();
        r.note_engaged(20);
        assert!(!r.should_ask_confirm(22));
        assert!(r.should_ask_confirm(24));
        assert_eq!(r.decide(&review_of(Normal, High), 0, 0, StallFacts::default()), StallVerdictStep::Release);
        assert!(!r.should_ask_confirm(80), "once a call");
        for (verdict, confidence) in [(Normal, Medium), (ScamOrSales, High), (Unavailable, Medium)] {
            let mut r = StallReview::default();
            r.note_engaged(20);
            assert!(r.should_ask_confirm(20 + CONFIRM_EARLY_UNITS));
            assert_eq!(r.decide(&review_of(verdict, confidence), 0, 0, StallFacts::default()), StallVerdictStep::Carry, "{verdict:?} {confidence:?}");
        }
        // Unmistakable scam-only words from a non-protected caller engage at once
        // without waiting for an HTTP review or spending a follow-up confirm call;
        // a saved contact still gets a review first.
        let mut r = StallReview::default();
        assert!(!r.engage_hard_at_once(true, true, true));
        assert!(r.engage_hard_at_once(true, true, false));
        assert_eq!(r.why, EngageWhy::HardWords);
        r.note_engaged(20);
        assert!(!r.should_ask_confirm(80));
    }

    /// The card desk of 2026-11-14: 「這邊是XX銀行信用卡風險管控中心」 alone
    /// crossed the keywords and the second look, seeing nothing else, said
    /// scam. Too early to tell now: no new look on more of the same turn, one
    /// once it has ended; the desk's question about a purchase brings no new
    /// keyword, so a "normal" then holds for the rest of the call.
    #[test]
    fn an_introduction_is_looked_at_again_after_the_callers_turn() {
        use crate::stall_review::Verdict::*;
        let mut hits = Vec::new();
        let s = stall_score_add("喂您好，這邊是大愛銀行信用卡風險管控中心。", &mut hits);
        assert!(stall_engages(s, &hits), "{s} {hits:?}");
        let mut r = StallReview::default();
        assert!(r.should_ask(true, s));
        assert!(!r.should_ask_after_turn(), "one look at a time");
        assert_eq!(r.verdict(TooEarly, s), StallVerdictStep::AfterTurn);
        // More of the same turn, even more keywords: not yet.
        assert!(!r.should_ask(true, s + 4));
        // Their turn is over: look again, whatever the score.
        assert!(r.should_ask_after_turn());
        assert!(!r.should_ask_after_turn());
        assert_eq!(r.verdict(Normal, s), StallVerdictStep::Carry);
        assert!(!r.should_ask_after_turn());
        let more = stall_score_add(
            "我們看到您的卡片昨天晚上11點左右在網路電器商店有一筆3280元的消費。想跟您確認是您本人刷的嗎?",
            &mut hits,
        );
        assert_eq!(more, 0, "{hits:?}");
        assert!(!r.should_ask(true, s + more));
        // Too-early looks do not use up the three that decide.
        for score in [s + 2, s + 4] {
            assert!(r.should_ask(true, score));
            assert_eq!(r.verdict(Normal, score), StallVerdictStep::Carry);
        }
        assert!(!r.should_ask(true, 30));
        assert_eq!((r.asked, r.early), (4, 1));
    }

    /// A fake bank says what it wants after its introduction and is stalled; a
    /// caller who keeps saying only who they are is too, after the fourth look.
    #[test]
    fn after_the_introduction_a_scam_is_still_a_scam() {
        use crate::stall_review::Verdict::*;
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.verdict(TooEarly, 3), StallVerdictStep::AfterTurn);
        assert!(r.should_ask_after_turn());
        assert_eq!(r.verdict(ScamOrSales, 5), StallVerdictStep::Engage);
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        for _ in 1..STALL_REVIEW_EARLY_MAX {
            assert_eq!(r.verdict(TooEarly, 3), StallVerdictStep::AfterTurn);
            assert!(r.should_ask_after_turn());
        }
        assert_eq!(r.verdict(TooEarly, 3), StallVerdictStep::Engage);
        assert_eq!(STALL_REVIEW_EARLY_MAX, 4);
    }

    /// One turn: 「…信用卡風險管控中心。」 then 「您的帳戶被誤設成分期付款，要請您現在去
    /// ATM解除分期。」. The look asked on the introduction comes back "too early"
    /// only after that turn has ended and the AI is answering (seen on every
    /// probe call: asked 32.374, AI speaks 32.673, verdict 33.624). It is asked
    /// again at once, not a whole turn later, and the scam is stalled.
    #[test]
    fn a_too_early_look_at_a_finished_turn_is_asked_again_at_once() {
        use crate::stall_review::Verdict::*;
        let mut hits = Vec::new();
        let asked_at = stall_score_add("喂您好，這邊是大愛銀行信用卡風險管控中心。", &mut hits);
        let mut r = StallReview::default();
        assert!(r.should_ask(stall_engages(asked_at, &hits), asked_at));
        // The rest of the same turn arrives while it is looked at…
        let now = asked_at + stall_score_add("您的帳戶被誤設成分期付款，要請您現在去ATM解除分期。", &mut hits);
        assert_eq!(now, 8, "{hits:?}");
        for kw in ["分期", "atm", "帳戶"] {
            assert!(hits.contains(&kw), "{hits:?}");
        }
        assert!(!r.should_ask(true, now), "one look at a time");
        // …the turn ends and the AI answers…
        r.note_caller_turn_end();
        assert!(!r.should_ask_after_turn());
        // …then the look comes back "too early": asked again at once.
        assert_eq!(r.verdict_now(TooEarly, asked_at, now), StallVerdictStep::AskAgain);
        assert!(!r.should_ask(true, now) && !r.should_ask_after_turn(), "the new look is under way");
        assert_eq!(r.verdict_now(ScamOrSales, now, now), StallVerdictStep::Engage);
        assert_eq!((r.asked, r.early), (2, 1));
        // Either sign is enough: the turn ended with no new keyword…
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        r.note_caller_turn_end();
        assert_eq!(r.verdict_now(TooEarly, 3, 3), StallVerdictStep::AskAgain);
        // …or more keywords came while the turn goes on.
        let mut r = StallReview::default();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.verdict_now(TooEarly, 3, 5), StallVerdictStep::AskAgain);
        // A turn that ended before the look was asked is not a reason to ask again.
        let mut r = StallReview::default();
        r.note_caller_turn_end();
        assert!(r.should_ask(true, 3));
        assert_eq!(r.verdict_now(TooEarly, 3, 3), StallVerdictStep::AfterTurn);
    }

    /// A real card desk that so far only said who it is: its late "too early"
    /// is asked again at once and sees the whole turn — still only the
    /// introduction — so it is too early again and waits for the next turn,
    /// whose question about a purchase is normal. No stalling on the way.
    #[test]
    fn a_real_desk_looked_at_again_at_once_still_waits_for_its_matter() {
        use crate::stall_review::Verdict::*;
        let mut hits = Vec::new();
        let s = stall_score_add("喂您好，這邊是大愛銀行信用卡風險管控中心。", &mut hits);
        let mut r = StallReview::default();
        assert!(r.should_ask(true, s));
        r.note_caller_turn_end();
        assert_eq!(r.verdict_now(TooEarly, s, s), StallVerdictStep::AskAgain);
        // The new look was asked after the turn ended: nothing more came, so its
        // "too early" waits for the caller's next turn.
        assert_eq!(r.verdict_now(TooEarly, s, s), StallVerdictStep::AfterTurn);
        assert!(!r.should_ask(true, s));
        let more = stall_score_add(
            "我們看到您的卡片昨天晚上11點左右在網路電器商店有一筆3280元的消費。想跟您確認是您本人刷的嗎?",
            &mut hits,
        );
        assert_eq!(more, 0, "{hits:?}");
        r.note_caller_turn_end();
        assert!(r.should_ask_after_turn());
        assert_eq!(r.verdict_now(Normal, s, s + more), StallVerdictStep::Carry);
        assert!(!r.should_ask(true, s + more) && !r.should_ask_after_turn());
        assert_eq!((r.asked, r.early), (3, 2));
        assert!(r.early < STALL_REVIEW_EARLY_MAX);
    }

    /// The fixed guard of the answering-as-me style: a card desk's "was this
    /// purchase yours?" gets neither yes nor no and no blocking.
    #[test]
    fn the_persona_guard_answers_a_card_check_with_neither_yes_nor_no() {
        let g = PERSONA_GUARD;
        assert!(g.contains("某一筆刷卡或交易是不是你本人刷的") && g.contains("不要說是、也不要說不是"), "{g}");
        assert!(g.contains("「我那時在睡覺」「我沒買東西」"), "{g}");
        assert!(g.contains("不要答應停卡、掛失、取消或爭議這筆交易、報案"), "{g}");
        assert!(g.contains("把金額、商店、時間聽清楚") && g.contains("卡片背面的客服"), "{g}");
        assert!(g.ends_with("不要替{owner}做任何承諾。"), "{g}");
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
        // The setting `reply_in_caller_language` on: the call's prompt carries the note
        // (prompts::build), once.
        let p = persona_system_prompt("", false, "", StallPrompt::Off);
        let s = crate::prompts::with_caller_language_note(&p, crate::locale::Lang::ZhTw);
        assert!(s.contains("【對方講別的語言】") && s.contains("問你能不能講某種語言"), "{s}");
        assert_eq!(session_prompt(&s, Outbound::Incoming), s, "once");
        // The calls the phone dials still get it on every call.
        assert_eq!(session_prompt(&p, Outbound::Errand), s);
        assert_eq!(session_prompt(&s, Outbound::CallBack), s, "once");
        let base = default_persona_prompt();
        assert!(!base.contains("不要講英文") && base.contains("就直接改用他的語言"));
        assert!(base.contains(PERSONA_LANGUAGE_LINE));
        assert!(legacy_persona_prompt().contains(PERSONA_LANGUAGE_LINE_LEGACY));
        // A custom prompt keeps its words; the note goes before the fixed guard.
        let c = session_prompt(&persona_system_prompt("你是我，只講中文。", false, "", StallPrompt::Off), Outbound::Errand);
        assert!(c.starts_with("你是我，只講中文。") && c.contains("【對方講別的語言】"));
        assert!(c.ends_with(&PERSONA_GUARD.replace("{owner}", "機主")));
        // Asked for personal details or a code: none given, none made up.
        assert!(c.contains("身分證字號") && c.contains("驗證碼") && c.contains("不要給、也不要編"));
        assert!(!c.contains("我這邊先幫他接"));
    }

    #[test]
    fn reply_in_callers_language_off_adds_no_switch_note_to_a_call_that_came_in() {
        // Setting off: the prompt built for the call has no note, and the session
        // does not add one (it did on every call, so the switch could not be turned off).
        let off = persona_system_prompt("", false, "", StallPrompt::Off);
        assert!(!off.contains("【對方講別的語言】"));
        assert_eq!(session_prompt(&off, Outbound::Incoming), off);
        let custom = persona_system_prompt("你是我，只講中文。", false, "", StallPrompt::Off);
        assert!(!session_prompt(&custom, Outbound::Incoming).contains("【對方講別的語言】"));
        let stall = persona_system_prompt("", false, "", StallPrompt::Full);
        assert!(!session_prompt(&stall, Outbound::Incoming).contains("【對方講別的語言】"));
    }

    #[test]
    fn short_repeated_pieces_are_kept_in_the_window() {
        let mut w = String::new();
        push_tx_window(&mut w, "掰", 400);
        push_tx_window(&mut w, "掰", 400);
        assert_eq!(w, "掰掰");
        let mut w = String::new();
        push_tx_window(&mut w, "爸", 400);
        push_tx_window(&mut w, "爸", 400);
        assert_eq!(w, "爸爸");
        let mut w = String::new();
        push_tx_window(&mut w, "お願いします。", 400);
        push_tx_window(&mut w, "それでは失礼し", 400);
        push_tx_window(&mut w, "ます", 400);
        assert!(w.ends_with("それでは失礼します"), "{w}");
        assert!(crate::locale::ends_with_farewell(&w, 2), "{w}");
        // A longer piece sent again whole is not doubled; a cumulative update replaces.
        let mut w = String::new();
        push_tx_window(&mut w, "好，那就先這樣", 400);
        push_tx_window(&mut w, "好，那就先這樣", 400);
        assert_eq!(w, "好，那就先這樣");
        push_tx_window(&mut w, "好，那就先這樣，掰掰", 400);
        assert_eq!(w, "好，那就先這樣，掰掰");
    }

    #[test]
    fn transcript_lines_are_put_into_traditional_only_once_whole() {
        // Unset settings (tests): the owner reads Traditional Chinese.
        let t = Some(Arc::new(std::sync::Mutex::new(String::new())));
        for piece in ["失礼", "します"] {
            append_transcriptions(&json!({"serverContent": {"inputTranscription": {"text": piece}}}), &t);
        }
        for piece in ["电话", "番号"] {
            append_transcriptions(&json!({"serverContent": {"outputTranscription": {"text": piece}}}), &t);
        }
        let merged = merge_dialogue_fragments(&t.as_ref().unwrap().lock().unwrap());
        assert_eq!(merged, "對方：失礼します\n接聽：電話番號");
        assert_eq!(merge_dialogue_fragments("對方：我这边\n對方：是管理室"), "對方：我這邊是管理室");
    }

    #[test]
    fn the_opening_says_from_abroad_only_for_a_number_from_another_country() {
        // The plan says whether the number is of the SIM's country; with an opening
        // in another language, only a number of another country is called foreign.
        use crate::locale::Lang;
        let never = || -> bool { panic!("the SIM is known: no carrier check") };
        assert!(!opening_from_abroad(Some(Lang::En), Some(true), never));
        assert!(opening_from_abroad(Some(Lang::En), Some(false), never));
        assert!(!opening_from_abroad(None, Some(false), never));
        // SIM unknown: the carrier's warning decides, as before.
        assert!(opening_from_abroad(Some(Lang::En), None, || true));
        assert!(!opening_from_abroad(Some(Lang::En), None, || false));
        let text = |opening, abroad| -> String {
            let k: Value = serde_json::from_str(&persona_kick_client_content(opening, abroad)).unwrap();
            k.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap().to_string()
        };
        // A US number on a US SIM set to English: English, not a foreign call.
        let home = text(Some(crate::locale::Lang::En), false);
        assert!(home.contains("改用英文") && home.contains("「Hello?」") && !home.contains("國外號碼"), "{home}");
        let abroad = text(Some(crate::locale::Lang::En), true);
        assert!(abroad.contains("改用英文") && abroad.contains("國外號碼"), "{abroad}");
        assert_eq!(text(None, true), text(None, false));
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
        // Custom prompt replaces the body but never the guard; persona mode does not disclose AI.
        let c = persona_system_prompt("你是我，講話很隨性。", false, "", StallPrompt::Off);
        assert!(c.starts_with("你是我"));
        assert!(c.contains("【底線（固定）】") && c.contains("不要給、也不要編"));
        assert!(!c.contains("不要說謊") && !c.contains("AI 語音"));
        // Desk note is folded in softly, not as the forced-first-line block.
        let s = persona_system_prompt("", true, "快遞放管理室", StallPrompt::Off);
        assert!(s.contains("快遞放管理室"));
        assert!(!s.contains(OWNER_STATUS_BEGIN));
        assert!(extract_owner_status(&s).is_empty());
    }

    #[test]
    fn stall_prompt_replaces_sales_reject_line() {
        assert!(default_persona_prompt().contains(PERSONA_SALES_REJECT_DEFAULT));
        assert!(legacy_persona_prompt().contains(PERSONA_SALES_REJECT_LINE));
        let off = persona_system_prompt("", false, "", StallPrompt::Off);
        assert!(off.contains(PERSONA_SALES_REJECT_DEFAULT));
        assert!(!off.contains("陪他繞"));
        // Every call starts with the short line, not the ~900-char block.
        let first = persona_system_prompt("", false, "", StallPrompt::ListenFirst);
        assert!(!first.contains(PERSONA_SALES_REJECT_DEFAULT) && first.contains(PERSONA_SALES_LISTEN_LINE));
        assert!(!first.contains("陪他繞"));
        assert!(first.chars().count() < off.chars().count() + 200);
        let on = persona_system_prompt("", false, "", StallPrompt::Full);
        assert!(!on.contains(PERSONA_SALES_REJECT_DEFAULT) && !on.contains(PERSONA_SALES_LISTEN_LINE));
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
        for line in [PERSONA_HOLD_OFF_DEFAULT, PERSONA_END_DEFAULT, PERSONA_SALES_REJECT_DEFAULT] {
            assert!(base.contains(line), "{line}");
        }
        let legacy = legacy_persona_prompt();
        for line in [PERSONA_ACK_LINE, PERSONA_HOLD_OFF_LINE, PERSONA_END_BLOCK, PERSONA_SALES_REJECT_LINE] {
            assert!(legacy.contains(line), "{line}");
        }
        // …and gone once the call is judged: no 「我再想一下／晚點再確認／好，我知道了」
        // brush-offs, no 掰掰 when they wrap up; the stall block leads and ends it.
        let on = persona_system_prompt("", false, "", StallPrompt::Full);
        for line in [PERSONA_HOLD_OFF_DEFAULT, PERSONA_END_DEFAULT, PERSONA_SALES_REJECT_DEFAULT] {
            assert!(!on.contains(line), "{line}");
        }
        assert!(on.starts_with(PERSONA_STALL_HEAD));
        for line in [PERSONA_HOLD_OFF_STALL, PERSONA_END_STALL] {
            assert!(on.contains(line), "{line}");
        }
        // An edited copy starting from the legacy long prompt still gets all four swaps.
        let edited_legacy = format!("{legacy}\n備註：有養狗。");
        let on_legacy = persona_system_prompt(&edited_legacy, false, "", StallPrompt::Full);
        for line in [PERSONA_ACK_LINE, PERSONA_HOLD_OFF_LINE, PERSONA_END_BLOCK, PERSONA_SALES_REJECT_LINE] {
            assert!(!on_legacy.contains(line), "{line}");
        }
        for line in [PERSONA_ACK_STALL, PERSONA_HOLD_OFF_STALL, PERSONA_END_STALL] {
            assert!(on_legacy.contains(line), "{line}");
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
        assert!(first.contains(PERSONA_END_DEFAULT) && !first.contains(PERSONA_STALL_HEAD));
        // The rotated stall session's history says so too.
        let tx = "接聽：喂？\n對方：我安信託，分期通知你還沒處理\n接聽：我再想一下，晚點再確認";
        assert!(history_turns_with_note(tx, 2_500, true, None)[0].1.contains("陪他繞"));
        assert!(!transcript_history_turns(tx, 2_500)[0].1.contains("陪他繞"));
    }

    /// Every mid-call instruction and note says the language the call is held in
    /// (lang_lock), not the one the caller's latest words seem to be in.
    #[test]
    fn mid_call_instructions_name_the_calls_language() {
        use crate::locale::Lang;
        let text_of = |payload: &str| -> String {
            let v: Value = serde_json::from_str(payload).unwrap();
            v.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap().to_string()
        };
        // A Taiwan call whose caller's last words had a few kana in them: still Chinese.
        let mut call = crate::lang_lock::CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        call.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        call.caller_turn("那個包裹は放在管理室の可以嗎");
        let kicks = [
            persona_wrap_up_client_content(),
            persona_stall_wrap_up_client_content(),
            persona_stall_keep_client_content(true),
            persona_resume_reply_client_content(),
            persona_repeat_line_client_content(),
            persona_stall_idle_nudge_client_content(),
            persona_idle_nudge_client_content(),
            crate::stall_script::client_note("一個方向"),
        ];
        for k in &kicks {
            let t = text_of(&in_call_language(k.clone(), call.lang()));
            assert!(t.ends_with("（這通電話對方講中文，這句也用中文說，不要換成別的語言。）"), "{t}");
            assert!(!t.contains("日文"), "{t}");
            // Once only.
            let twice = in_call_language(in_call_language(k.clone(), call.lang()), call.lang());
            assert_eq!(text_of(&twice).matches("這通電話對方講").count(), 1);
            // A language without a setting: in_call_language keeps it unchanged (for
            // callers expecting None), while in_spoken_language adds the generic note.
            assert_eq!(in_call_language(k.clone(), None), *k);
            let other_t = text_of(&in_spoken_language(k.clone(), Some(crate::lang_lock::Spoken::Other)));
            assert!(other_t.contains("對方正在講的語言"), "{other_t}");
            assert_eq!(in_spoken_language(k.clone(), None), *k);
        }
        // The caller really switched to English (two whole turns): English from then on.
        call.caller_turn("Sorry, can you speak English? I don't understand.");
        call.caller_turn("I need to know if you are at home right now.");
        let t = text_of(&in_call_language(persona_idle_nudge_client_content(), call.lang()));
        assert!(t.ends_with("（這通電話對方講英文，這句也用英文說，不要換成別的語言。）"), "{t}");
        // A note that asks for no reply stays one.
        let note: Value = serde_json::from_str(&in_call_language(crate::stall_script::client_note("x"), call.lang())).unwrap();
        assert_eq!(note["clientContent"]["turnComplete"], json!(false));
        // The rotated session's history names it too.
        let tx = "接聽：喂？\n對方：Hello, I need to know if you are at home right now.";
        assert!(history_turns_with_note(tx, 2_500, false, call.lang())[0].1.contains("這通電話對方講英文"));
        // The drift reminder names the call's language and asks for no reply.
        let drift = crate::prompts::language_drift_note(Lang::ZhTw, Lang::ZhTw);
        assert!(drift.contains("對方講的是中文") && drift.contains("一直用中文回"), "{drift}");
        let en = crate::prompts::language_drift_note(Lang::En, Lang::Ja);
        assert!(en.contains("speaking Japanese") && en.contains("Answer in Japanese"), "{en}");
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

    /// A Japanese investment pitch scored only ご案内 and 口座 (投資 is in the
    /// Chinese list, which kana text skips; a keyword is listed once) and was never stalled; a promised
    /// profit is what only a scammer says, as 保證獲利 is in Chinese.
    #[test]
    fn a_japanese_investment_pitch_is_stalled() {
        let mut hits = Vec::new();
        let s = stall_score_add("投資コンサルのものですが、王さんはいらっしゃいますでしょうか", &mut hits);
        assert!(hits.contains(&"投資コンサル"), "{hits:?}");
        let s = s + stall_score_add("実は毎月8%の利益を保証する投資グループのご案内なんです", &mut hits);
        assert!(stall_engages(s, &hits), "{s} {hits:?}");
        assert!(hits.iter().any(|h| crate::scam_db::is_hard(h)), "{hits:?}");
        // A clinic or a courier stays below the line.
        for t in ["成田クリニックの受付ですが、健診の日程が変更になりました", "お届け物の件で、明日の午前中はご在宅でしょうか"] {
            let mut hits = Vec::new();
            let s = stall_score_add(t, &mut hits);
            assert!(!stall_engages(s, &hits), "{t} {s} {hits:?}");
        }
    }

    #[test]
    fn a_spanish_fake_bank_sending_the_caller_to_an_atm_engages_stalling() {
        // Two lines of a fake bank: go to the ATM to stop the instalments.
        let mut hits = Vec::new();
        let s = stall_score_add("Vaya al cajero automático para cancelar las cuotas.", &mut hits);
        assert!(s >= STALL_SCORE_THRESHOLD && stall_engages(s, &hits), "{s} {hits:?}");
        assert!(hits.contains(&"cajero automático") && hits.contains(&"cuotas"), "{hits:?}");
        assert!(!hits.contains(&"cajero"), "the longer phrase counts once: {hits:?}");
        // The same as speech-to-text writes it without accents, and a frozen account.
        for t in [
            "le llamamos del banco, tiene un cargo recurrente, vaya al cajero automatico",
            "su cuenta bloqueada tiene un plan de pagos, para desbloquear vaya al cajero",
            "su cuenta congelada tiene mensualidades y cargos automáticos pendientes",
        ] {
            let mut hits = Vec::new();
            let s = stall_score_add(t, &mut hits);
            assert!(s >= STALL_SCORE_THRESHOLD && stall_engages(s, &hits), "{t} {s} {hits:?}");
            assert!(hits.iter().any(|h| stall_keyword_weight(h) >= STALL_STRONG_WEIGHT), "{t} {hits:?}");
        }
        // An ordinary Spanish call stays below.
        let mut hits = Vec::new();
        let s = stall_score_add("Hola, soy Marta, ¿comemos mañana a las dos?", &mut hits);
        assert!(s < STALL_SCORE_THRESHOLD, "{s} {hits:?}");
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
        // Only a silent propose answer arms it (an error is meant to be said).
        use crate::appointments::{answer_is_silent, CHECK, PROPOSE};
        assert!(answer_is_silent(PROPOSE, &json!({"status": "pending_owner_confirmation"})));
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
            "您好，這邊是中信銀行，您反映的信用卡寄送問題，想跟您確認寄送地址",
            "Hi, it's Dana from Acme support, following up on the issue you had with our router",
        ] {
            assert!(looks_like_own_matter(t), "{t}");
        }
        // 「您申請的」 alone opens loan and card scams too: it counts only with another phrase.
        assert!(!looks_like_own_matter("您好，這邊是銀行，您申請的信用卡已經核卡了，想跟您確認寄送地址"));
        assert!(!looks_like_own_matter("這裡是地檢署，調查結果顯示你的帳戶涉及洗錢"));
        // "Your order" / "your account" open many scams: they do not count.
        assert!(!looks_like_own_matter("There is a problem with your order, we need to refund you"));
        assert!(dialogue_own_matter("接聽：喂？\n對方：您反映的信用卡額度調整好了，分期的部分也可以跟您說明"));
        assert!(!dialogue_is_sales("接聽：喂？\n對方：您反映的信用卡額度調整好了，分期的部分也可以跟您說明"));
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
    fn a_call_released_from_stalling_after_the_normal_cap_still_gets_a_wrap_up() {
        // Early release: the normal cap as before.
        assert_eq!(released_talk_limits(true, 5 * 60, 90), talk_limits(true, 5 * 60));
        // Released at 8 minutes with a 5-minute normal cap: a minute left, wrap-up inside it.
        let (w, f, m) = released_talk_limits(true, 5 * 60, 8 * 60);
        assert_eq!(m, 8 * 60 + 60);
        assert!(w > 8 * 60 && w < f && f < m, "{w} {f} {m}");
        // A cap never shrinks below what was already talked, at any length.
        for elapsed in [0, 299, 300, 301, 1500, 1800] {
            let (w, f, m) = released_talk_limits(true, 5 * 60, elapsed);
            assert!(m >= elapsed + 60 && w < f && f < m, "{elapsed}: {w} {f} {m}");
        }
    }

    #[test]
    fn release_rotation_switches_back_only_when_already_on_stall_session() {
        let now = chrono::Local::now();
        let think = "gemini-3.8-live-extended-thinking";
        let setup = json!({"setup": {
            "model": format!("models/{think}"),
            "generationConfig": {"responseModalities": ["AUDIO"], "thinkingConfig": {"thinkingLevel": "low"}},
            "systemInstruction": {"parts": [{"text": "一般"}]}
        }});
        let mut rt = crate::settings_store::RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_mode = "confirm".into();
        let appt = crate::appointments::ApptSession::for_call(&rt, "0912345678").unwrap();

        // Still on the normal session (!live_is_stall): cancel pending stall rotation, whether appt is off or on.
        assert_eq!(release_rotation(false, &setup, "一般", None, now, false, false), (None, false));
        assert_eq!(release_rotation(false, &setup, "一般", Some(&appt), now, false, true), (None, false));

        // Already on the stall session (live_is_stall) without appointments: ALWAYS Some(...) to revert model/prompt, no tools.
        let (back_plain, need_rotate) = release_rotation(true, &setup, "一般", None, now, false, false);
        assert!(need_rotate);
        let back_plain = back_plain.expect("must rotate back even with appt=None");
        assert_eq!(back_plain.pointer("/setup/model"), Some(&json!(format!("models/{think}"))));
        assert_eq!(back_plain.pointer("/setup/generationConfig/thinkingConfig/thinkingLevel"), Some(&json!("low")));
        assert_eq!(back_plain.pointer("/setup/systemInstruction/parts/0/text"), Some(&json!("一般")));
        assert!(back_plain.pointer("/setup/tools").is_none());

        // Already on the stall session (live_is_stall) with appointments: revert prompt/model AND re-enable appointment tools.
        let (back_appt, need_rotate) = release_rotation(true, &setup, "一般", Some(&appt), now, false, true);
        assert!(need_rotate);
        let back_appt = back_appt.expect("must rotate back with appt=Some");
        assert_eq!(back_appt.pointer("/setup/model"), Some(&json!(format!("models/{think}"))));
        assert!(setup_system_prompt(&back_appt).starts_with("一般\n[Appointments"));
        assert_eq!(back_appt.pointer("/setup/tools/0/functionDeclarations/1/behavior"), Some(&json!("NON_BLOCKING")));

        // Blocking fallback and full appointment note when appt_block_sent was true.
        let (back_blocking, need_rotate) = release_rotation(true, &setup, "一般", Some(&appt), now, true, false);
        assert!(need_rotate);
        let back_blocking = back_blocking.unwrap();
        assert!(setup_system_prompt(&back_blocking).starts_with("一般\n\n[Appointments"));
        assert!(!back_blocking.to_string().contains("NON_BLOCKING"));
        assert!(back_blocking.pointer("/setup/tools/0/functionDeclarations/0/name").is_some());
    }

    #[test]
    fn persona_prompt_hears_matters_out() {
        let base = default_persona_prompt();
        // Deflection replies came from these example lines.
        for bad in ["我現在在外面，等一下回你", "晚點回你好不好", "晚點回你」", "我現在不方便"] {
            assert!(!base.contains(bad), "{bad}");
        }
        assert!(base.contains("對方講事情就讓他講完") && base.contains("弄清楚是誰打來、什麼事、要你做什麼"));
        assert!(base.contains("沒有時間限制") && base.contains("等對方表示要結束了"));
        for mode in [StallPrompt::Off, StallPrompt::ListenFirst, StallPrompt::Full] {
            assert!(persona_system_prompt("", false, "", mode).contains("對方講事情就讓他講完"));
        }
        let legacy = legacy_persona_prompt();
        assert!(legacy.contains("【對方有事要說明】") && legacy.contains("你現在說就好"));
        assert!(!legacy.contains("或事情明顯講完了") && legacy.contains("要等對方表示要結束了"));
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
        assert!(base.contains("講電話的當下你查不了任何東西") && base.contains("不要編答案，也不要叫對方等"));
        // So did 「我想一下」 (from the word list) and 「我晚點自己再確認」
        // (from 「跟他說你之後自己再確認」), in nearly every reply.
        assert!(!base.contains("、我想一下、") && !base.contains("跟他說你之後自己再確認"));
        assert!(!base.contains("（說謝謝、"));
        for mode in [StallPrompt::Off, StallPrompt::ListenFirst, StallPrompt::Full] {
            assert!(persona_system_prompt("", false, "", mode).contains("也不要叫對方等"));
        }
        let legacy = legacy_persona_prompt();
        assert!(legacy.contains("【不要讓對方等】") && legacy.contains("我沒什麼印象耶"));
        assert!(legacy.contains("一通電話最多講一次") && legacy.contains("對方只說「好」「謝謝」不算要結束"));
    }

    #[test]
    fn persona_prompt_leaves_room_to_improvise() {
        // More freedom for the model, less formula.
        let base = default_persona_prompt();
        assert!(base.contains("像真人講電話：用自己的話") && base.contains("不要客服腔，不要念稿，不要條列"));
        // The lines that made every reply the same shape are gone…
        for formula in [
            "一次講一句，最多兩句",
            "回話前先接住對方剛講的那件事",
            "把對方講的重點順口重複一下",
            "一句表示收到就好",
        ] {
            assert!(!base.contains(formula), "{formula}");
        }
        // …and the hard lines stay.
        for kept in ["不要編答案，也不要叫對方等", "都不要答應，輕輕帶過", "等對方表示要結束了"] {
            assert!(base.contains(kept), "{kept}");
        }
        let legacy = legacy_persona_prompt();
        assert!(legacy.contains("自由發揮") && legacy.contains("長短看情況"));
        assert!(legacy.contains("無關緊要的") && legacy.contains("不要說「我在家」「明天可以」"));
        assert!(legacy.contains("不要自己先說掰掰") && legacy.contains("不要把時間說定"));
        // Not judged yet: no checking the caller out either.
        assert!(!PERSONA_SALES_LISTEN_LINE.contains("哪個單位"));
        // Stalling: scammers stay while they think the money is coming — play along,
        // keep hitting snags, give nothing real.
        let on = persona_system_prompt("", false, "", StallPrompt::Full);
        assert!(on.contains("嘴上配合") && on.contains("真的錢一毛都不會動"));
        assert!(!on.contains(PERSONA_HOLD_OFF_DEFAULT));
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
            ..Default::default()
        };
        // The opening and the caller's hints only (the other parts: own test).
        let call_lang_plan = |caller: &str, sim: Option<&str>, lang: Lang, home: &str, langs: &BTreeMap<String, String>| {
            let p = super::call_lang_plan(caller, sim, lang, home, langs);
            CallLangPlan { opening: p.opening, caller_stt: p.caller_stt, ..Default::default() }
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
            plan(Some(Lang::En), None)
        );
        // A Japanese SIM with no language set opens a Japanese number in Japanese;
        // a Hong Kong SIM stays on the setting's Chinese.
        assert_eq!(call_lang_plan("0312345678", Some("jp"), Lang::ZhTw, "886", &none).opening, Some(Lang::Ja));
        assert_eq!(call_lang_plan("+81312345006", Some("jp"), Lang::ZhTw, "886", &none).opening, Some(Lang::Ja));
        assert_eq!(call_lang_plan("+85291230002", Some("hk"), Lang::ZhTw, "886", &none).opening, None);
        // The prompt language still comes from the setting only.
        assert_eq!(super::call_lang_plan("0312345678", Some("jp"), Lang::ZhTw, "886", &none).prompt_lang, None);
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
    fn the_ais_own_voice_gets_the_callers_other_language() {
        // A caller of a country whose language the AI has no setting for: the AI's
        // own voice gets that language too, once.
        assert_eq!(stt_ai_config(None, Some("th-TH"))["languageCodes"], json!(["zh-TW", "th-TH"]));
        assert_eq!(stt_ai_config(None, Some("zh-TW"))["languageCodes"], json!(["zh-TW"]));
        let s = with_ai_stt_codes(&json!({"setup": {"model": "m"}}), &["zh-TW", "ru-RU"]);
        assert_eq!(s.pointer("/setup/outputAudioTranscription/languageCodes"), Some(&json!(["zh-TW", "ru-RU"])));
        assert_eq!(s.pointer("/setup/model"), Some(&json!("m")));
    }

    #[test]
    fn call_plan_knows_a_sims_own_number_its_language_and_other_languages() {
        use crate::locale::Lang;
        use std::collections::BTreeMap;
        let none: BTreeMap<String, String> = BTreeMap::new();
        let us_en: BTreeMap<String, String> = [("us".to_string(), "en".to_string())].into_iter().collect();
        // A US SIM set to English in a Chinese setup: a US number is the SIM's own
        // and the whole call's prompt is in English…
        let us = call_lang_plan("(202) 555-0143", Some("us"), Lang::ZhTw, "886", &us_en);
        assert_eq!((us.sim_local, us.prompt_lang), (Some(true), Some(Lang::En)));
        // …a Japanese number on it is not (its own opening, the setting's prompt).
        let jp = call_lang_plan("+81312345678", Some("us"), Lang::ZhTw, "886", &us_en);
        assert_eq!((jp.sim_local, jp.prompt_lang), (Some(false), None));
        // No language for the SIM, or the setting's own: the setting's prompt.
        let tw = call_lang_plan("0912345678", Some("tw"), Lang::ZhTw, "886", &none);
        assert_eq!((tw.sim_local, tw.prompt_lang, tw.caller_other_stt), (Some(true), None, None));
        assert_eq!(call_lang_plan("2025550143", Some("us"), Lang::En, "1", &us_en).prompt_lang, None);
        // SIM or number unknown: not known to be local.
        assert_eq!(call_lang_plan("0912345678", None, Lang::ZhTw, "886", &none).sim_local, None);
        assert_eq!(call_lang_plan("", Some("us"), Lang::ZhTw, "886", &us_en).prompt_lang, None);
        // Thai, Russian, Arabic, Hindi, Italian, Indonesian, Vietnamese, etc.: their language for the AI's own voice.
        for (n, code) in [
            ("+66812345678", Some("th-TH")),
            ("+79161234567", Some("ru-RU")),
            ("+966501234567", Some("ar-EG")),
            ("+201001234567", Some("ar-EG")),
            ("+919812345678", Some("hi-IN")),
            ("+390612345678", Some("it-IT")),
            ("+62812345678", Some("id-ID")),
            ("+84912345678", Some("vi-VN")),
            ("+8801712345678", Some("bn-BD")),
            ("+380501234567", Some("uk-UA")),
            ("+31201234567", Some("nl-NL")),
            ("+48221234567", Some("pl-PL")),
            ("+40211234567", Some("ro-RO")),
            ("+12025550143", None),
            ("+81312345678", None),
        ] {
            assert_eq!(call_lang_plan(n, Some("tw"), Lang::ZhTw, "886", &none).caller_other_stt, code, "{n}");
        }
        // An AI call-back keeps none of it.
        let cb = callback_lang_plan(Lang::Ja, Lang::ZhTw);
        assert_eq!((cb.sim_local, cb.prompt_lang, cb.caller_other_stt), (None, None, None));
    }

    #[test]
    fn thai_russian_arabic_and_hindi_are_told_by_their_letters() {
        assert_eq!(other_script_lang("สวัสดีค่ะ อยากจองโต๊ะค่ะ"), Some("th-TH"));
        assert_eq!(other_script_lang("Здравствуйте, я хотел бы записаться"), Some("ru-RU"));
        assert_eq!(other_script_lang("Добрий день, підкажіть будь ласка"), Some("uk-UA"));
        assert_eq!(other_script_lang("مرحبا، أريد حجز موعد"), Some("ar-EG"));
        assert_eq!(other_script_lang("नमस्ते, मुझे अपॉइंटमेंट चाहिए"), Some("hi-IN"));
        assert_eq!(other_script_lang("হ্যালো, আমি কথা বলতে চাই"), Some("bn-BD"));
        assert_eq!(other_script_lang("வணக்கம், நான் பேச வேண்டும்"), Some("ta-IN"));
        assert_eq!(other_script_lang("నమస్కారం, నేను మాట్లాడాలి"), Some("te-IN"));
        // A stray word of a speech-to-text slip is not enough; the nine are none of them.
        assert_eq!(other_script_lang("我們訂सात點"), None);
        assert_eq!(other_script_lang("喂？你好，請問哪位？ Hello, bonjour, 네, はい"), None);
        // A dialogue in one of them with no line of the nine.
        let thai = "對方：สวัสดีค่ะ อยากจองโต๊ะพรุ่งนี้ค่ะ\n接聽：ได้ค่ะ กี่โมงคะ";
        assert!(dialogue_in_other_language(thai, crate::locale::Lang::ZhTw));
        assert!(!dialogue_in_other_language("對方：你好，我想預約明天下午\n接聽：好，幾點？", crate::locale::Lang::ZhTw));
        assert!(!dialogue_in_other_language("對方：喂？\n接聽：喂？", crate::locale::Lang::ZhTw));
    }

    fn windowed(pieces: &[&str]) -> String {
        let mut w = String::new();
        for p in pieces {
            push_tx_window(&mut w, p, 800);
        }
        w
    }

    #[test]
    fn tx_pieces_keep_the_words_apart() {
        // The STT starts a new word with a space; the window keeps one space.
        assert_eq!(windowed(&["Okay", " bye"]), "Okay bye");
        assert!(caller_wraps_up(&windowed(&["Okay", " bye"])));
        assert_eq!(windowed(&["Hasta", " luego."]), "Hasta luego.");
        assert!(caller_wraps_up(&windowed(&["Hasta", " luego."])));
        assert_eq!(windowed(&["Sí, está", " bien."]), "Sí, está bien.");
        let thanks = windowed(&["Thanks for", " the reminder.", " Talk", " soon!"]);
        assert_eq!(thanks, "Thanks for the reminder. Talk soon!");
        assert!(turn_ends_with_closing(&thanks));
        // A word starting right after sentence punctuation is a new word.
        assert_eq!(windowed(&["for you.", "Cheers."]), "for you. Cheers.");
        let ru = windowed(&[" до", " свидания"]);
        assert_eq!(ru, "до свидания");
        assert!(crate::locale::ends_with_farewell(&ru, 1));
        // No space next to Chinese or Japanese, nor inside a word the STT cut in two.
        assert_eq!(windowed(&["好，那就先這樣", "嗎？"]), "好，那就先這樣嗎？");
        assert_eq!(windowed(&["好，那就先這樣", " 嗎？"]), "好，那就先這樣嗎？");
        assert_eq!(windowed(&["それでは失礼し", "ます"]), "それでは失礼します");
        assert_eq!(windowed(&["민준아, 나", " 지훈인데."]), "민준아, 나 지훈인데.");
        assert_eq!(windowed(&["Hal", "lo"]), "Hallo");
        // Numbers and addresses the STT cut at a mark stay whole.
        assert_eq!(windowed(&["3.", "5"]), "3.5");
        assert_eq!(windowed(&["1,", "000"]), "1,000");
        assert_eq!(windowed(&["example", ".", "com"]), "example.com");
        assert_eq!(windowed(&["okay,", "bye"]), "okay, bye");
        assert_eq!(windowed(&["Спасибо.", "До свидания."]), "Спасибо. До свидания.");
        // with_latest joins the same way.
        assert_eq!(with_latest("Okay", " bye"), "Okay bye");
        assert_eq!(with_latest("Okay bye", " bye"), "Okay bye");
        assert_eq!(with_latest("Hal", "lo"), "Hallo");
        assert_eq!(with_latest("for you.", "Cheers."), "for you. Cheers.");
    }

    #[test]
    fn tx_window_rules_compare_without_the_leading_space() {
        // A longer piece sent again is skipped, spaced or not.
        assert_eq!(windowed(&["See you tomorrow", " See you tomorrow"]), "See you tomorrow");
        // A short one is new words again.
        assert_eq!(windowed(&["Bye", " bye"]), "Bye bye");
        // A cumulative update replaces the window.
        assert_eq!(windowed(&["Okay", " bye", "Okay bye now"]), "Okay bye now");
    }

    #[test]
    fn snippets_keep_a_leading_space_for_a_new_word() {
        let v = json!({"serverContent": {
            "inputTranscription": {"text": " bye"},
            "outputTranscription": {"text": " 好的"}
        }});
        let (heard, said) = extract_transcription_snippets(&v);
        assert_eq!(heard, vec![" bye".to_string()]);
        assert_eq!(said, vec!["好的".to_string()]);
        let v = json!({"serverContent": {"inputTranscription": {"text": "Okay"}}});
        assert_eq!(extract_transcription_snippets(&v).0, vec!["Okay".to_string()]);
    }

    #[test]
    fn a_call_in_a_latin_language_without_a_setting_is_another_language() {
        use crate::locale::Lang;
        let calls = [
            (
                "Italian",
                "對方：Buongiorno, ho un pacco per il signor Wang, può scendere a ritirarlo alla portineria?\n接聽：Certo, grazie mille, scendo subito a prenderlo.",
            ),
            (
                "Vietnamese",
                "對方：Xin chào, tôi là nhân viên giao hàng, tôi có một gói hàng cho anh Wang ở dưới sảnh.\n接聽：Vâng, cảm ơn anh, tôi xuống lấy ngay bây giờ.",
            ),
            (
                "Indonesian",
                "對方：Selamat siang, saya kurir, ada paket untuk Bapak Wang di pos satpam.\n接聽：Baik, terima kasih banyak, saya segera turun untuk mengambilnya.",
            ),
            (
                "Dutch",
                "對方：Goedemiddag, ik ben de bezorger, ik heb een pakket voor meneer Wang bij de receptie.\n接聽：Dank u wel, ik kom het zo meteen ophalen.",
            ),
            (
                "Polish",
                "對方：Dzień dobry, jestem kurierem, mam paczkę dla pana Wanga w recepcji budynku.\n接聽：Dziękuję bardzo, zaraz zejdę po odbiór.",
            ),
            (
                "Romanian",
                "對方：Bună ziua, sunt curierul, am un colet pentru domnul Wang la recepția clădirii.\n接聽：Mulțumesc frumos, cobor imediat să îl ridic.",
            ),
            (
                "Turkish",
                "對方：Merhaba, ben kuryeyim, Wang Bey için bir paketim var, binanın girişinde bekliyorum.\n接聽：Teşekkür ederim, hemen inip alıyorum.",
            ),
        ];
        for (name, d) in calls {
            assert_eq!(dialogue_lang(d, Lang::ZhTw), None, "{name}");
            assert!(dialogue_in_other_language(d, Lang::ZhTw), "{name}");
            // Only the caller's side (the AI's line told nothing).
            let caller_only = d.lines().next().unwrap();
            assert_eq!(dialogue_lang(caller_only, Lang::ZhTw), None, "{name} caller");
            assert!(dialogue_in_other_language(caller_only, Lang::ZhTw), "{name} caller");
        }
        // The nine keep their language and are not "another language".
        let nine = [
            (Lang::En, "對方：Hello, I have a package for Mr. Wang at the front desk.\n接聽：Okay, thank you very much, I will come down and get it now."),
            (Lang::Es, "對方：Hola, tengo un paquete para el señor Wang en la portería.\n接聽：Muy bien, muchas gracias, ahora mismo bajo a recogerlo."),
            (Lang::Fr, "對方：Bonjour, j'ai un colis pour monsieur Wang à la loge.\n接聽：Très bien, merci beaucoup, je descends le chercher tout de suite."),
            (Lang::De, "對方：Guten Tag, ich habe ein Paket für Herrn Wang an der Pforte.\n接聽：Sehr gut, vielen Dank, ich komme sofort runter und hole es ab."),
            (Lang::Pt, "對方：Olá, sou o entregador, tenho uma encomenda para o senhor Wang na portaria.\n接聽：Muito bem, obrigado, eu já vou descer para buscar a encomenda, você pode deixar com o porteiro."),
            (Lang::ZhTw, "對方：你好，王先生的包裹放在管理室，麻煩您下來拿。\n接聽：好的，謝謝你，我馬上下去拿。"),
            (Lang::Ja, "對方：こんにちは、王さんの荷物が管理室にあります。\n接聽：はい、ありがとうございます、すぐに取りに行きます。"),
            (Lang::Ko, "對方：안녕하세요, 왕 선생님 택배가 관리실에 있습니다.\n接聽：네, 감사합니다, 지금 바로 가지러 내려가겠습니다."),
        ];
        for (l, d) in nine {
            assert_eq!(dialogue_lang(d, Lang::ZhTw), Some(l), "{l:?}");
            assert!(!dialogue_in_other_language(d, Lang::ZhTw), "{l:?}");
        }
        // Other scripts stay as they were.
        for d in [
            "對方：สวัสดีค่ะ มีพัสดุของคุณหวางอยู่ที่ห้องนิติบุคคลค่ะ\n接聽：ได้ค่ะ ขอบคุณค่ะ เดี๋ยวลงไปรับค่ะ",
            "對方：Здравствуйте, для господина Вана есть посылка в консьерже.\n接聽：Хорошо, спасибо большое, сейчас спущусь и заберу.",
            "對方：مرحبا، لدي طرد للسيد وانغ في مكتب الاستقبال.\n接聽：حسنا، شكرا جزيلا، سأنزل لأخذه الآن.",
            "對方：नमस्ते, श्री वांग के लिए रिसेप्शन पर एक पार्सल रखा है।\n接聽：ठीक है, बहुत धन्यवाद, मैं अभी लेने आता हूँ।",
        ] {
            assert_eq!(dialogue_lang(d, Lang::ZhTw), None, "{d}");
            assert!(dialogue_in_other_language(d, Lang::ZhTw), "{d}");
        }
    }

    #[test]
    fn turkish_calls_get_their_speech_to_text_code() {
        assert_eq!(other_lang_stt_of_country("tr"), Some("tr-TR"));
        assert_eq!(other_lang_stt_of_country("TR"), Some("tr-TR"));
        assert_eq!(other_lang_stt_of_country("vn"), Some("vi-VN"));
        assert_eq!(other_lang_stt_of_country("us"), None);
    }

    #[test]
    fn italian_calls_get_their_speech_to_text_code_and_the_ai_goodbyes_are_found() {
        for iso in ["it", "IT", "sm", "va"] {
            assert_eq!(other_lang_stt_of_country(iso), Some("it-IT"), "{iso}");
        }
        for said in [
            "Grazie, alla prossima.",
            "Va bene, grazie, buon lavoro!",
            "Allora la saluto.",
            "Va bene, a presto.",
            "Perfetto, buon proseguimento.",
            "Arrivederci, grazie mille.",
            "Perfetto, mille grazie, buonasera.",
        ] {
            assert!(ends_with_farewell(said), "{said}");
        }
        for said in [
            "Grazie mille.",
            "Pronto, buongiorno.",
            "Buongiorno, sono Marco, ho un pacco.",
            "A dopo le cinque non ci sono.",
            "Torno a casa dopo.",
            "Il tecnico passa un'ora più tardi.",
            "Cerchiamo persone per un buon lavoro.",
        ] {
            assert!(!ends_with_farewell(said), "{said}");
        }
        assert!(ends_with_farewell("Okay, bye!"));
        assert!(!ends_with_farewell("Gracias"));
    }

    /// Italian lines through the closing machine the way the session feeds it
    /// ([`caller_wraps_up`] as `old_rules`), on the first turn and after some talk.
    #[test]
    fn italian_everyday_lines_do_not_hang_up_and_real_goodbyes_do() {
        let talk = "Buongiorno, sono il corriere, ho un pacco per il signor Rossi.";
        let first = |t: &str| crate::closing::ClosingMachine::new("it").on_caller(t, t, caller_wraps_up(t), false).ends_call;
        let later = |t: &str| {
            let mut m = crate::closing::ClosingMachine::new("it");
            m.on_caller(talk, talk, caller_wraps_up(talk), false);
            m.on_caller(t, t, caller_wraps_up(t), false).ends_call
        };
        for t in [
            "Siamo un'azienda che cerca persone per un buon lavoro",
            "Alla prossima fermata scendo",
            "Buongiorno, buon lavoro, sono l'idraulico",
            "La saluto mia madre",
            "Ciao, sono Marco",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
            assert!(!first(t), "first turn: {t}");
            assert!(!later(t), "after talk: {t}");
        }
        for t in ["Grazie, buon lavoro", "Va bene, alla prossima", "Perfetto, la saluto", "Perfetto, ci sentiamo domani. Ciao"] {
            assert!(caller_wraps_up(t), "{t}");
            assert!(later(t), "after talk: {t}");
        }
        // Other languages as before.
        for t in ["Okay, bye!", "Vale, hasta luego.", "Görüşürüz."] {
            assert!(later(t), "after talk: {t}");
        }
        assert!(!later("I'm looking for a good job nearby"));
    }

    /// The raw pieces Gemini's output transcription was seen to send, with the
    /// word break at the end of one piece or at the start of the next, and the
    /// text each makes.
    const RAW_PIECES: [(&[&str], &str); 6] = [
        (&["Cảm ơn bạn, tôi ", "sẽ nhắn lại với", " anh Vương."], "Cảm ơn bạn, tôi sẽ nhắn lại với anh Vương."),
        (&["Cảm ơn bạn, ", "tôi sẽ xuống", " lấy ngay."], "Cảm ơn bạn, tôi sẽ xuống lấy ngay."),
        (&["Teşekkürler, Sayın Wang'a ", "bilgi vereceğim."], "Teşekkürler, Sayın Wang'a bilgi vereceğim."),
        (&["Bilgi verdiğiniz ", "için teşekkürler."], "Bilgi verdiğiniz için teşekkürler."),
        (&["ขอบคุณครับ ", "เดี๋ยวผมจะแจ้งให้คุณหวัง", "ทราบนะครับ"], "ขอบคุณครับ เดี๋ยวผมจะแจ้งให้คุณหวังทราบนะครับ"),
        (&["承知いたしました、ありがとう", "ございます。"], "承知いたしました、ありがとうございます。"),
    ];

    fn through_the_log(pieces: &[&str]) -> String {
        let buf = Arc::new(std::sync::Mutex::new(String::new()));
        for p in pieces {
            let v = json!({"serverContent": {"outputTranscription": {"text": p}}});
            append_transcriptions(&v, &Some(buf.clone()));
        }
        let log = buf.lock().unwrap().clone();
        merge_dialogue_fragments(&log)
    }

    #[test]
    fn a_space_at_the_end_of_a_piece_is_a_word_break_in_the_window() {
        for (pieces, text) in RAW_PIECES {
            assert_eq!(windowed(pieces), text, "{pieces:?}");
            // As the session feeds its windows: never a space at their end.
            let (mut w, mut held) = (String::new(), false);
            for p in pieces {
                push_tx_window(&mut w, &tx_carry(&mut held, p), 800);
                assert!(!w.ends_with(' '), "{w:?}");
            }
            assert_eq!(w, text, "{pieces:?}");
        }
        assert_eq!(windowed(&["спасибо ", "вам"]), "спасибо вам");
        let okay = windowed(&["Okay ", "bye"]);
        assert_eq!(okay, "Okay bye");
        assert!(caller_wraps_up(&okay));
        assert_eq!(windowed(&["好，那就先這樣 ", "嗎？"]), "好，那就先這樣嗎？");
        // The snippets a message yields keep the space on either side.
        let v = json!({"serverContent": {"outputTranscription": {"text": "Okay "}}});
        assert_eq!(extract_transcription_snippets(&v).1, vec!["Okay ".to_string()]);
        let v = json!({"serverContent": {"outputTranscription": {"text": "好，那就先這樣 "}}});
        assert_eq!(extract_transcription_snippets(&v).1, vec!["好，那就先這樣".to_string()]);
        // A cumulative update still replaces a window that ends with a space.
        assert_eq!(windowed(&["Okay ", "Okay bye now"]), "Okay bye now");
    }

    #[test]
    fn a_space_at_the_end_of_a_fragment_is_a_word_break_in_the_record() {
        for (pieces, text) in RAW_PIECES {
            assert_eq!(through_the_log(pieces), format!("接聽：{text}"), "{pieces:?}");
        }
        assert_eq!(through_the_log(&["спасибо ", "вам"]), "接聽：спасибо вам");
        // The merged line never ends with a space, and an errand caption's joins match.
        assert_eq!(through_the_log(&["Okay "]), "接聽：Okay");
        let mut acc = String::new();
        for p in ["спасибо ", "вам"] {
            join_fragment(&mut acc, p);
        }
        assert_eq!(acc, "спасибо вам");
        let mut acc = "好，那就先這樣".to_string();
        join_fragment(&mut acc, " 嗎？");
        assert_eq!(acc, "好，那就先這樣嗎？");
    }

    /// The closing machine on a caller line after some business, fed the way the
    /// session does (the older check, [`caller_wraps_up`], as `old_rules`).
    fn closing_step_for(line: &str) -> crate::closing::ClosingStep {
        let mut m = crate::closing::ClosingMachine::new("zh-TW");
        let first = "我要寄一個包裹給王先生 0912345678";
        m.on_caller(first, first, false, false);
        m.on_caller(line, line, caller_wraps_up(line), false)
    }

    #[test]
    fn worshipping_and_nonsense_are_not_the_callers_goodbye() {
        for t in [
            "我明天要去拜拜",
            "我們在拜拜",
            "等一下要拜拜",
            "我媽叫我回去拜拜",
            "今天公司拜拜",
            "初一十五都要拜拜",
            "我明天要去拜拜喔",
            "你不要瞎掰",
            "他在亂掰啦",
            "把它掰開",
            "我们在拜拜呢",
            "拜拜的時候再說",
        ] {
            assert!(!caller_wraps_up(t), "{t}");
            let step = closing_step_for(t);
            assert!(!step.ends_call, "{t}");
            assert_ne!(step.event, crate::closing::ClosingEvent::CallerFarewell, "{t}");
        }
    }

    #[test]
    fn a_bye_said_is_the_callers_goodbye() {
        for t in [
            "好，拜拜",
            "好拜拜",
            "那拜拜囉",
            "謝謝，拜拜",
            "嗯嗯拜拜",
            "OK拜拜",
            "拜拜",
            "掰掰",
            "好喔掰",
            "那先這樣，拜拜",
            "我要去拜拜了，拜拜",
            "好的謝謝你掰掰",
            "先這樣囉掰掰",
            "沒問題拜拜",
            "辛苦了拜拜",
            "跟你說拜拜",
            "喂,不好意思,那也不打擾您,祝您事事順心,謝謝您哦,好,拜拜。",
        ] {
            assert!(caller_wraps_up(t), "{t}");
            let step = closing_step_for(t);
            assert_eq!(step.event, crate::closing::ClosingEvent::CallerFarewell, "{t}");
            assert!(step.ends_call, "{t}");
        }
        // A goodbye said to someone else is still not the caller's own.
        assert!(!caller_wraps_up("來，跟阿嬤說掰掰"));
    }

    #[test]
    fn worshipping_and_nonsense_are_not_the_ais_goodbye() {
        for t in ["喔你們今天要拜拜喔", "你在瞎掰吧", "那你們去拜拜", "我們在拜拜"] {
            assert!(!looks_like_persona_closing(t), "{t}");
            assert!(!turn_ends_with_closing(t), "{t}");
            assert!(!looks_like_goodbye(t), "{t}");
            assert!(!ends_with_farewell(t), "{t}");
        }
        for t in ["好，掰掰", "那先這樣，拜拜喔", "好，那我再想一下，掰", "好喔，拜拜", "好，掰掰，謝謝你"] {
            assert!(looks_like_persona_closing(t), "{t}");
            assert!(looks_like_goodbye(t), "{t}");
        }
        for t in ["好，掰掰", "那先這樣，拜拜喔"] {
            assert!(turn_ends_with_closing(t), "{t}");
        }
        assert!(ends_with_farewell("好喔，拜拜"));
    }

    #[test]
    fn caller_stt_hints_or_auto_detect() {
        // The AI's own voice: the opening language and the setting's (unset in tests:
        // zh-TW), once each — never left to detect, whatever the caller's side does
        // (a short 「掰掰」 of ours came out in Latin letters or Korean).
        use crate::locale::Lang;
        assert_eq!(stt_ai_config(None, None)["languageCodes"], json!(["zh-TW"]));
        assert_eq!(stt_ai_config(Some(Lang::En), None)["languageCodes"], json!(["en-US", "zh-TW"]));
        assert_eq!(stt_ai_config(Some(Lang::Ja), None)["languageCodes"], json!(["ja-JP", "zh-TW"]));
        assert_eq!(stt_ai_config(Some(Lang::ZhTw), None)["languageCodes"], json!(["zh-TW"]));
        let codes: &[&'static str] = &["en-US", "zh-TW"];
        let hinted = stt_caller_config(Some(codes));
        assert_eq!(hinted["languageCodes"], json!(["en-US", "zh-TW"]));
        let auto = stt_caller_config(None);
        assert!(auto.get("languageCodes").is_none());
        let empty: &[&'static str] = &[];
        assert!(stt_caller_config(Some(empty)).get("languageCodes").is_none());
        // No vocabulary either way: it pulled other languages into the owner's.
        for cfg in [&hinted, &auto] {
            assert!(cfg.get("customVocabulary").is_none(), "{cfg}");
        }
    }

    #[test]
    fn a_caller_s_speech_to_text_gets_no_vocabulary() {
        // The owner's name and the Chinese key words as vocabulary pulled other
        // languages into Chinese: an English line about a parcel
        // (「…一個給你的包裹被送到我家門口了」), a Japanese もしもし as 摩西摩西, and on
        // phone-quality audio a Turkish courier's sentence as 「你好。我 Cargo 管理室。
        // 包裹。」 — also on a local number, where the hints include the owner's
        // language. The key words came out right without it.
        use crate::locale::{Active, Lang};
        let tw = Active { owner: "林先生".into(), ..Default::default() };
        let us: std::collections::BTreeMap<String, String> = [("us".to_string(), "en".to_string())].into();
        // A US number on a US SIM, owner in Taiwan: no hints, the language is detected.
        let plan = call_lang_plan("+12025550143", Some("us"), Lang::ZhTw, "886", &us);
        assert_eq!(plan.caller_stt, None);
        assert_eq!(stt_caller_config_for(plan.caller_stt.as_deref(), &tw), json!({}));
        // A Taiwanese number: the hints, and nothing else — its caller may speak
        // Vietnamese, Thai, Japanese or Turkish.
        let local = call_lang_plan("0912000000", Some("tw"), Lang::ZhTw, "886", &Default::default());
        let hinted = stt_caller_config_for(local.caller_stt.as_deref(), &tw);
        assert_eq!(hinted, json!({ "languageCodes": ["zh-TW", "en-US"] }));
        // Every owner language and every hint: never a vocabulary.
        for lang in crate::locale::ALL {
            let me = Active { lang, notify_lang: lang, owner: "Alex".into(), ..Default::default() };
            let own: &[&'static str] = &[crate::locale::phrases(lang).stt, "en-US"];
            let ja_only: &[&'static str] = &["ja-JP"];
            for hints in [Some(own), Some(ja_only), None] {
                let cfg = stt_caller_config_for(hints, &me);
                assert!(cfg.get("customVocabulary").is_none(), "{lang:?} {cfg}");
                assert!(!cfg.to_string().contains("Alex"), "{lang:?} {cfg}");
            }
        }
    }

    #[test]
    fn carrier_prompt_wait_follows_the_sim_the_call_came_in_on() {
        // Taiwan SIM: callers from abroad get the warning, local ones do not.
        assert!(expects_carrier_intl_prompt(false, "+12025550143", Some("tw")));
        assert!(expects_carrier_intl_prompt(true, "+8613800138000", Some("TW")));
        assert!(!expects_carrier_intl_prompt(true, "+886912345678", Some("tw")));
        assert!(!expects_carrier_intl_prompt(true, "0912345678", Some("tw")));
        // Abroad is the number's country, read the way the SIM reads it: the
        // international prefix 00 and Hong Kong count.
        assert!(expects_carrier_intl_prompt(false, "0012025550143", Some("tw")));
        assert!(expects_carrier_intl_prompt(false, "+85291234567", Some("tw")));
        assert!(!expects_carrier_intl_prompt(true, "00886912345678", Some("tw")));
        // A hidden number (no digit at all) gets the carrier's warning like any
        // call from abroad, whatever the language setting says.
        for hidden in ["", "  ", "unknown", "private", "Anonymous"] {
            assert!(expects_carrier_intl_prompt(true, hidden, Some("tw")), "{hidden:?}");
            assert!(expects_carrier_intl_prompt(false, hidden, Some("TW")), "{hidden:?}");
            assert!(!expects_carrier_intl_prompt(true, hidden, Some("us")), "{hidden:?}");
            assert!(!expects_carrier_intl_prompt(true, hidden, Some("jp")), "{hidden:?}");
        }
        // SIM unknown: Traditional Chinese reads as a Taiwan SIM, any other language does not.
        assert!(expects_carrier_intl_prompt(true, "", None));
        assert!(expects_carrier_intl_prompt(true, "unknown", None));
        assert!(!expects_carrier_intl_prompt(false, "", None));
        assert!(!expects_carrier_intl_prompt(false, "private", Some(" ")));
        // US SIM (e.g. Tello on Wi-Fi calling): no warning from any caller, even with
        // the language set to Traditional Chinese.
        assert!(!expects_carrier_intl_prompt(true, "+12025550143", Some("us")));
        assert!(!expects_carrier_intl_prompt(true, "+886912345678", Some("us")));
        assert!(!expects_carrier_intl_prompt(true, "2025550143", Some("us")));
        // SIM unknown (older phone app) and the language says no: no wait.
        assert!(!expects_carrier_intl_prompt(false, "+12025550143", None));
        assert!(!expects_carrier_intl_prompt(false, "+12025550143", Some(" ")));
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
        assert_eq!(call_system_prompt("你是我。", None, now, false), "你是我。");
        let mut rt = crate::settings_store::RuntimeSettings::from_base(&crate::config::Settings::from_env());
        rt.appointment_mode = "off".into();
        assert!(crate::appointments::ApptSession::for_call(&rt, "0912345678").is_none());
        rt.appointment_mode = "confirm".into();
        let a = crate::appointments::ApptSession::for_call(&rt, "0912345678").unwrap();
        let p = call_system_prompt("你是我。", Some(&a), now, true);
        assert!(p.starts_with("你是我。\n\n[Appointments") && p.contains(&now.format("%Y-%m-%d").to_string()), "{p}");
        assert!(p.contains("Do not repeat what you just said") && p.contains("confirmed_by_caller = true"), "{p}");
        // A later session keeps the note that went in, never the whole part.
        assert!(p.chars().count() < "你是我。".chars().count() + crate::appointments::PROMPT_NOTE_MAX_CHARS + 3, "{p}");
        // The call starts with the short part only.
        let stub = call_system_prompt("你是我。", Some(&a), now, false);
        assert!(stub.starts_with("你是我。\n[Appointments") && stub.contains("never free or booked"), "{stub}");
        assert!(stub.chars().count() < 450, "{}", stub.chars().count());
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
        let k: Value = serde_json::from_str(&persona_kick_client_content(None, false)).unwrap();
        let t = k.pointer("/clientContent/turns/0/parts/0/text").unwrap().as_str().unwrap();
        assert!(t.contains("喂"));
        assert!(!t.contains("AI 助理"));
        // A caller from the US: 「Hello?」, with the instruction still in Chinese.
        let k: Value =
            serde_json::from_str(&persona_kick_client_content(Some(crate::locale::Lang::En), true)).unwrap();
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
        // The one exception to 「對嗎／再確認」: a time, date, amount or number is asked back once.
        assert!(p.contains("例外：對方講到要轉達的時間、日期、金額、電話號碼，要用一句問句把那一項完整複誦一次") && p.contains("不要自己猜"));
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
        // Forced reset clears held audio so stale caller syllables cannot bleed across sessions.
        assert!(overlap_feed(&mut hold, &mut t, &[1, 1, 1]).is_none());
        assert!(!hold.held.is_empty());
        hold.reset();
        assert!(hold.held.is_empty());
        assert!(hold.is_idle());
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

    #[test]
    fn soft_insurance_loan_and_deduction_scams_engage_while_clinics_and_repairs_do_not() {
        // Soft pitches and newer scam scripts that previously missed the keyword gate.
        for pitch in [
            "您好，我是富邦人壽專員，今天打來幫老客戶做免費保單健檢",
            "王先生您好，我們這邊是中租融資，請問最近有資金需求或增貸規劃嗎",
            "您的訂單系統設錯成批發商，每個月會自動扣款，我們幫您轉接銀行客服解除扣款",
            "這裡是健保署，您的健保卡在南部被盜用，即將鎖卡",
            "老師每天帶單穩賺不賠，你先加頼領飆股",
            "Hi, calling about your extended warranty and a special offer on your account",
        ] {
            let mut hits = Vec::new();
            let s = stall_score_add(pitch, &mut hits);
            assert!(stall_engages(s, &hits), "{pitch} -> score={s} hits={hits:?}");
        }
        // Ordinary clinic, courier, repair and friend calls stay untouched.
        for ordinary in [
            "您好，這裡是康健牙醫診所，提醒您明天下午三點看診，記得帶健保卡喔",
            "您好，您的包裹貨到付款八百元，放管理室可以嗎",
            "我們看到您的卡片昨天晚上11點左右在網路電器商店有一筆3280元的消費。想跟您確認是您本人刷的嗎?",
        ] {
            let mut hits = Vec::new();
            let s = stall_score_add(ordinary, &mut hits);
            assert!(!stall_engages(s, &hits), "{ordinary} -> score={s} hits={hits:?}");
        }
        // Owner's repair follow-ups are never stalled even if payment words appear.
        assert!(looks_like_own_matter("您好，您報修的冷氣原廠報價出來了，可以轉帳或付現"));
        assert!(looks_like_own_matter("回覆您反映的帳單分期問題"));
    }

    #[test]
    fn all_nine_languages_engage_stalling_on_scam_debt_and_harassment_without_false_positives() {
        use crate::locale::Lang;
        let cases = [
            (
                Lang::ZhTw,
                "我是賣貨便客服，請加賴並分享畫面做金流認證",
                "我們是資產管理公司法務催收，今天不處理就強制執行扣薪查封",
                "你欠錢不還真不要臉，明天去你家堵你，出門小心",
                "接聽：喂？\n對方：回覆您反映的訂單問題，請把簡訊驗證碼唸給我",
                "接聽：喂？\n對方：回覆您反映的帳單問題，我們已經幫您修正了，分期金額不變",
                "您好，台水公司提醒您這期水費月底到期，記得去超商繳費喔",
            ),
            (
                Lang::ZhCn,
                "我是闲鱼客服，请开启屏幕共享做金流认证",
                "我们是资产管理催收公司，今天不还就上门催收申请强制执行限高令",
                "你这个傻逼王八蛋，明天去你公司堵你",
                "接聽：喂？\n對方：关于您的申诉，请提供验证码完成核实",
                "接聽：喂？\n對方：关于您的申诉，银行已经处理完毕，特此告知",
                "您好，顺丰快递，有个包裹放物业了麻烦取一下",
            ),
            (
                Lang::En,
                "Please share your screen now for seller escrow verification",
                "This is a debt collector from an asset management collection agency; pay up or face wage garnishment",
                "Listen piece of shit asshole, we will come to your house tonight",
                "接聽：Hello?\n對方：Following up on your request, please give me the one-time password",
                "接聽：Hello?\n對方：Following up on your request, your credit card replacement is on the way",
                "Hi, it's Dr. Miller's dental clinic confirming your check-up tomorrow at two",
            ),
            (
                Lang::Ja,
                "売上金の受け取り認証のため今すぐ画面共有をしてください",
                "債権回収の担当です。差し押さえと法的措置の手続きに入ります",
                "ふざけるなバカ野郎、今から家に行くぞ",
                "接聽：もしもし？\n對方：お問い合わせの件ですが、認証コードを教えてください",
                "接聽：もしもし？\n對方：お問い合わせの件ですが、口座の手続きが完了いたしました",
                "ヤマト運輸です。お荷物をお届けに伺いましたがご不在でした",
            ),
            (
                Lang::Ko,
                "안전결제 인증을 위해 지금 화면 공유 부탁드립니다",
                "추심업체 채권추심 팀입니다. 급여압류 및 법적 조치 진행합니다",
                "야 이 씨발 개새끼야 집으로 찾아간다",
                "接聽：여보세요?\n對方：문의하신 건으로 연락드렸습니다. 인증번호를 불러주세요",
                "接聽：여보세요?\n對方：문의하신 신용카드 배송 일정을 안내해 드립니다",
                "안녕하세요, 치과입니다. 내일 오후 세 시 예약 확인차 연락드렸습니다",
            ),
            (
                Lang::Es,
                "Necesitamos compartir pantalla para verificar su cuenta de vendedor",
                "Llamamos de la agencia de cobro y empresa de recobro por embargo y demanda judicial",
                "Oye hijo de puta imbécil, vamos a ir a su domicilio ahora",
                "接聽：¿Diga?\n對方：Sobre su reclamación, dígame el código de verificación",
                "接聽：¿Diga?\n對方：Sobre su reclamación de la tarjeta, ya quedó solucionado",
                "Hola, soy el repartidor de paquetes, estoy abajo en la recepción",
            ),
            (
                Lang::Fr,
                "Activez le partage d'écran pour le paiement sécurisé leboncoin",
                "Ici le cabinet de recouvrement, l'huissier lance une saisie sur salaire et des poursuites judiciaires",
                "Espèce de connard, on sait où tu habites et on va venir chez vous",
                "接聽：Allô ?\n對方：Suite à votre demande, donnez-moi le code de vérification",
                "接聽：Allô ?\n對方：Suite à votre réclamation concernant votre compte, tout est réglé",
                "Bonjour, c'est le livreur, j'ai un colis pour vous devant l'immeuble",
            ),
            (
                Lang::De,
                "Bitte jetzt Bildschirm teilen um den Käuferschutz verifizieren zu können",
                "Hier ist das Inkassobüro wegen Lohnpfändung und Zwangsvollstreckung durch den Gerichtsvollzieher",
                "Du Arschloch und Hurensohn, wir kommen bei Ihnen vorbei",
                "接聽：Hallo?\n對方：Bezüglich Ihrer Anfrage brauche ich sofort Ihren Bestätigungscode",
                "接聽：Hallo?\n對方：Bezüglich Ihrer Beschwerde zum Konto ist nun alles erledigt",
                "Guten Tag, Paketdienst hier, darf ich Ihr Paket beim Nachbarn abgeben?",
            ),
            (
                Lang::Pt,
                "Precisamos compartilhar a tela agora para a liberação do pagamento",
                "Aqui é da assessoria de cobrança sobre penhora, bloqueio judicial e protesto em cartório",
                "Seu filho da puta vagabundo, sei onde você mora e vamos na sua casa",
                "接聽：Alô?\n對方：Sobre sua reclamação, me passe o código de verificação agora",
                "接聽：Alô?\n對方：Sobre sua reclamação do cartão, o ajuste já foi concluído",
                "Olá, aqui é da portaria avisando que chegou uma encomenda para você",
            ),
        ];
        assert_eq!(cases.len(), crate::locale::ALL.len());
        for (lang, scam, debt, harass, spoofed_dlg, genuine_dlg, normal) in cases {
            for bad in [scam, debt, harass] {
                let mut hits = Vec::new();
                let s = stall_score_add(bad, &mut hits);
                assert!(stall_engages(s, &hits), "{lang:?}: expected stall_engages on {bad:?}, score={s}, hits={hits:?}");
            }
            assert!(!dialogue_own_matter(spoofed_dlg), "{lang:?}: spoofed own_matter must be rejected");
            assert!(dialogue_is_sales(spoofed_dlg), "{lang:?}: spoofed own_matter + hard word must flag as sales/scam");
            assert!(dialogue_own_matter(genuine_dlg), "{lang:?}: genuine own_matter must stay true");
            assert!(!dialogue_is_sales(genuine_dlg), "{lang:?}: genuine own_matter must not be flagged");
            let mut normal_hits = Vec::new();
            let ns = stall_score_add(normal, &mut normal_hits);
            assert!(!stall_engages(ns, &normal_hits), "{lang:?}: normal call must not engage: score={ns} hits={normal_hits:?}");

            for assistant in [false, true] {
                for stall in [StallPrompt::ListenFirst, StallPrompt::Full] {
                    let built = crate::prompts::build(&crate::prompts::Ctx {
                        lang,
                        assistant,
                        owner: "Alex",
                        aliases: &[],
                        custom: "",
                        status: "",
                        stall,
                        knowledge: "",
                        carrier_note: lang == Lang::ZhTw,
                        auto_language: true,
                    });
                    assert!(!built.contains('{') && !built.contains('}'), "{lang:?} {stall:?}: leftover placeholder in {built}");
                    if lang == Lang::ZhTw {
                        assert!(built.contains("不要承認") && built.contains("訊號"), "{lang:?} {stall:?}");
                    } else if lang == Lang::ZhCn {
                        assert!(built.contains("不要承认") && built.contains("讯号"), "{lang:?} {stall:?}");
                    } else {
                        assert!(
                            built.to_ascii_lowercase().contains("never admit owing") && built.contains("signal"),
                            "{lang:?} {stall:?}"
                        );
                    }
                }
            }
        }
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

/// 2026-11-17 acceptance run (9832627): Italian, Russian, Indonesian, Filipino and
/// Indian couriers on their own SIMs heard 「喂，你好」 first.
#[cfg(test)]
mod local_hello_opening {
    use super::call_lang_plan;
    use crate::locale::Lang;
    use std::collections::BTreeMap;

    fn hello_of(caller: &str, sim: &str, langs: &BTreeMap<String, String>) -> Option<&'static str> {
        let p = call_lang_plan(caller, Some(sim), Lang::ZhTw, "886", langs);
        p.local_hello.map(|h| h.hello)
    }

    #[test]
    fn a_country_without_a_language_setting_answers_its_own_way() {
        let none = BTreeMap::new();
        assert_eq!(hello_of("+390612345678", "it", &none), Some("Pronto?"));
        assert_eq!(hello_of("+74951234567", "ru", &none), Some("Алло?"));
        assert_eq!(hello_of("+62812345678", "id", &none), Some("Halo?"));
        assert_eq!(hello_of("+66812345678", "th", &none), Some("ฮัลโหล"));
        assert_eq!(hello_of("+84912345678", "vn", &none), Some("A lô?"));
        assert_eq!(hello_of("+639171234567", "ph", &none), Some("Hello?"));
        assert_eq!(hello_of("+919812345678", "in", &none), Some("Hello?"));
        // A hidden number on that SIM too.
        assert_eq!(hello_of("", "it", &none), Some("Pronto?"));
        // A Thai number on a Taiwan SIM, as a Japanese one opens in Japanese.
        assert_eq!(hello_of("+66812345678", "tw", &none), Some("ฮัลโหล"));
        // A Taiwanese number on the Italian SIM keeps Chinese.
        assert_eq!(hello_of("+886912345678", "it", &none), None);
        // The owner set a language for that SIM: theirs.
        let it_en: BTreeMap<String, String> = [("it".to_string(), "en".to_string())].into_iter().collect();
        let p = call_lang_plan("+390612345678", Some("it"), Lang::ZhTw, "886", &it_en);
        assert_eq!((p.opening, p.local_hello), (Some(Lang::En), None));
        // Countries with a language of the AI's, and Malaysia (Chinese callers), are as before.
        assert_eq!(hello_of("+81312345678", "jp", &none), None);
        assert_eq!(hello_of("0912345678", "tw", &none), None);
        assert_eq!(hello_of("+60123456789", "my", &none), None);
        // Unknown SIM: no guess.
        assert_eq!(call_lang_plan("+390612345678", None, Lang::ZhTw, "886", &none).local_hello, None);
    }

    #[test]
    fn the_opening_instruction_names_the_local_way() {
        let it = crate::prompts::local_hello("it").unwrap();
        let zh = crate::prompts::kick_open_local(Lang::ZhTw, false, it, false);
        assert!(zh.contains("義大利文") && zh.contains("Pronto?") && zh.contains("Pronto, mi sente?"), "{zh}");
        assert!(!zh.contains('{'), "{zh}");
        let en = crate::prompts::kick_open_local(Lang::En, false, it, true);
        assert!(en.contains("Italian") && en.contains("Pronto?") && !en.contains('{'), "{en}");
        let cn = crate::prompts::kick_open_local(Lang::ZhCn, true, it, false);
        assert!(cn.contains("意大利语") && !cn.contains('{'), "{cn}");
    }
}

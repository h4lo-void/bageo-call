//! Call history + agent-side audio capture for the LAN web UI.
//!
//! Honesty: BCR files stay on the phone. This store records metadata and:
//! - **`_remote.wav`**: continuous wall-clock remote/caller uplink for the
//!   **entire** call (menu, hold, VM, goodbye) — primary whole-call caller track.
//!   Always via `write_uplink` (cellular WS), never mixed with system playout.
//!   Unity-gain (no per-frame AGC) so the web player is not a pumped noise floor.
//! - **`<id>.wav` listen mix**: TTS plus remote **ducked** while system audio
//!   is energetic (inject echo on VOICE_DOWNLINK is distorted, so correlation
//!   sidetone misses it; energy ducking is what `/calls/` actually hears).
//! - **`_tts.wav`**: system-only (`write_tts`) — local Piper/menu/hold + TRTC
//!   downlink→phone playout (`spawn_trtc_downlink_pull_loop` in `main`). Same lane;
//!   not a separate file. Gated by `CALLS_RECORD_TTS`.
//! - **`_vm.wav`**: DTMF-2 message slice only.
//!
//! Live telephony inject is a different path (`write_tts` PCM → phone). Do not
//! put recording AGC / WebAudio makeup gain on that lane.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    pub id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub serial: String,
    pub caller_id: String,
    pub duration_ms: u64,
    pub status: String,
    /// Relative path under calls root, e.g. `2026-08-01/<id>.wav`
    /// Continuous mixed (remote + TTS) covering the full call timeline.
    pub audio_path: Option<String>,
    /// System-only stream (`<id>_tts.wav`): local TTS/hold + TRTC playout.
    pub tts_audio_path: Option<String>,
    /// Continuous remote/caller uplink only (`<id>_remote.wav`) — whole call.
    #[serde(default)]
    pub remote_audio_path: Option<String>,
    /// DTMF-1 AI segment only (caller uplink while LiveAi/TrtcAi) — input for the summary.
    #[serde(default)]
    pub ai_audio_path: Option<String>,
    pub uplink_bytes: u64,
    pub tts_bytes: u64,
    pub sample_rate: u32,
    pub events: Vec<String>,
    /// Message-only voicemail WAV (DTMF 2 flow), relative path under calls root.
    #[serde(default)]
    pub voicemail_path: Option<String>,
    #[serde(default)]
    pub vm_duration_ms: u64,
    /// Caller-requested reply channel: `phone` | `none`. None = never asked (legacy / early hangup).
    #[serde(default)]
    pub reply_method: Option<String>,
    #[serde(default)]
    pub reply_contact: Option<String>,
    /// True when the contact was confirmed with the caller (e.g. pressed 1 for caller-id callback).
    #[serde(default)]
    pub contact_verified: bool,
    /// Short post-call summary (the notification lines: who called, what about,
    /// what the AI said, what to do). The
    /// full dialogue lives in [`CallNotes`] (`{date}/{id}_notes.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The call's echo check: `Some(true)` = the phone's capture never carried our
    /// own audio (Pixel 9), so the caller track is kept whole (no echo removal)
    /// and the listen mix is not ducked under the AI. None = older recordings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub echo_free: Option<bool>,
    /// The appointment the caller agreed to on this call, as the call left it
    /// (later decisions are in [`CallNotes::appointment`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appointment: Option<crate::appointments::Appointment>,
}

/// Detailed post-call record for the web UI (the notification only gets `summary`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CallNotes {
    pub summary: String,
    /// Gemini's labelled dialogue (`對方：` / `接聽：` lines), whole call.
    #[serde(default)]
    pub dialogue: String,
    /// Whisper of the caller track (backup; empty for sales / scam stalling calls).
    #[serde(default)]
    pub recording_text: String,
    /// How the call ended (same hint the summary model gets).
    #[serde(default)]
    pub end_hint: String,
    /// `deepseek` | `fallback` | `no_key` | `ai_fail` | `none`.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub updated_at: String,
    /// A date the call fixed, as a calendar suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<crate::calendar::Event>,
    /// The appointment the AI took on this call and where it stands
    /// (`pending` / `booked` / `confirmed` / `declined`, text message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appointment: Option<crate::appointments::Appointment>,
}

/// `YYYY-MM-DD` directory holding a record's files.
fn record_date_dir(rec: &CallRecord) -> Option<String> {
    [
        rec.audio_path.as_deref(),
        rec.remote_audio_path.as_deref(),
        rec.tts_audio_path.as_deref(),
        rec.ai_audio_path.as_deref(),
    ]
    .into_iter()
    .flatten()
    .find_map(|p| p.split_once('/').map(|(d, _)| d.to_string()))
    .or_else(|| {
        DateTime::parse_from_rfc3339(&rec.started_at)
            .ok()
            .map(|d| d.with_timezone(&Utc).format("%Y-%m-%d").to_string())
    })
    .filter(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
}

/// Query params for filtered / paginated call history (used by `/calls/api/list`).
///
/// Results are always sorted **newest-first** by `started_at` (RFC3339 string
/// compare), then `id` descending for a stable tie-break.
#[derive(Debug, Clone, Default)]
pub struct CallListFilter {
    /// Exact status match (e.g. `blocked_spam`, `ended`, `ringing`).
    pub status: Option<String>,
    /// Case-insensitive substring of device serial.
    pub serial: Option<String>,
    /// Case-insensitive substring of `caller_id`.
    pub caller: Option<String>,
    /// Free-text search across caller_id, serial, id, events, and optional transcript.
    pub q: Option<String>,
    /// When set, require / exclude voicemail presence.
    pub has_voicemail: Option<bool>,
    /// When set, require / exclude a non-empty task transcript (via transcript index).
    pub has_transcript: Option<bool>,
    /// Max rows to return. `None` = no limit (backward compatible full list).
    pub limit: Option<usize>,
    /// Skip this many matching rows (default 0).
    pub offset: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallListPage {
    pub calls: Vec<CallRecord>,
    /// Total matches before limit/offset.
    pub total: usize,
    pub limit: Option<usize>,
    pub offset: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallStats {
    pub total: usize,
    /// Calls whose `started_at` falls on today's UTC date (matches `date_dir`).
    pub today: usize,
    pub voicemails: usize,
    pub blocked: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CallIndex {
    calls: Vec<CallRecord>,
}

pub struct ActiveCall {
    pub id: String,
    pub started_at: String,
    started_instant: Instant,
    serial: String,
    caller_id: String,
    status: String,
    sample_rate: u32,
    date_dir: String,
    /// Continuous mixed PCM (remote + TTS), wall-clock aligned.
    mix: Vec<u8>,
    /// System-only track (wall-clock) when `record_tts` — TTS/hold/TRTC playout.
    tts_mix: Vec<u8>,
    /// Remote/caller uplink only (wall-clock), always written for the whole call.
    remote_mix: Vec<u8>,
    /// Caller uplink only while press-1 AI is active (no menu / hold) — for Whisper → summary.
    ai_remote_pcm: Vec<u8>,
    /// When true, `write_uplink` also appends into `ai_remote_pcm`.
    ai_segment_active: bool,
    /// AI segment byte span in `remote_mix` (wall-clock), for the cleaned `_ai.wav`.
    ai_seg_start: Option<usize>,
    ai_seg_end: Option<usize>,
    uplink_bytes: u64,
    tts_bytes: u64,
    /// Cap mixed capture (bytes) so a stuck session cannot fill the disk.
    mix_bytes_cap: u64,
    events: Vec<String>,
    record_tts: bool,
    /// True while agent streams system audio (TTS/hold/TRTC) into the phone.
    /// While set, sidetone-correlated uplink is excluded from `mix` (not from
    /// `remote_mix`) so hold/menu is not doubled in the full recording.
    tts_outbound_active: bool,
    /// Recent outbound system PCM (~200 ms) for sidetone correlation.
    last_tts_tail: Vec<u8>,
    /// Live echo-gate probe: uplink/reference energy ratio for every frame judged
    /// while system audio was in the window (logged at finalize to tune the gate).
    echo_probe: Vec<f32>,
    /// Echo check over our first AI audio: `Some(false)` = the capture carries our
    /// own voice back (Pixel 7/7a), `Some(true)` = it does not (Pixel 9).
    echo_free: Option<bool>,
    echo_check_frames: u32,
    echo_check_max_ratio: f32,
    /// Caller may talk over the AI: echo-free line and the setting on. Shared with
    /// the Gemini session (stops our playback when the caller cuts in).
    barge_in: Arc<AtomicBool>,
    barge_in_allowed: bool,
    /// Next write offset into `remote_mix` (sequential; bursts append, not add).
    remote_write: usize,
    /// Next write offset into `tts_mix`.
    tts_write: usize,
    /// Background sound (ambience) sent on its own in the AI's pauses (echo-free
    /// phones), wall-clock.
    /// Kept apart from `tts_mix` (the live echo gate's reference) and added to
    /// the AI track at seal.
    bed_mix: Vec<u8>,
    bed_write: usize,
    /// Legacy raw paths unused; kept for finalize cleanup compatibility.
    pcm_path: PathBuf,
    tts_pcm_path: PathBuf,
    voicemail_rel: Option<String>,
    vm_duration_ms: u64,
    reply_method: Option<String>,
    reply_contact: Option<String>,
    contact_verified: bool,
    /// What the caller agreed to on this call (appointments), shared with the Live session.
    appointments: Option<Arc<crate::appointments::CallAppointments>>,
}

impl ActiveCall {
    pub fn push_event(&mut self, ev: &str) {
        self.events.push(ev.to_string());
    }

    /// The call's appointment book (its Live session writes, the record reads at the end).
    pub fn set_appointments(&mut self, book: Arc<crate::appointments::CallAppointments>) {
        self.appointments = Some(book);
    }

    pub fn set_caller_id(&mut self, caller_id: &str) {
        if !caller_id.is_empty() {
            self.caller_id = caller_id.to_string();
        }
    }

    pub fn set_status(&mut self, status: &str) {
        self.status = status.to_string();
    }

    /// Record the caller's requested reply channel (`phone` | `none`).
    pub fn set_reply(&mut self, method: &str, contact: &str, verified: bool) {
        self.reply_method = Some(method.to_string());
        let c = contact.trim();
        self.reply_contact = if c.is_empty() {
            None
        } else {
            Some(c.to_string())
        };
        self.contact_verified = verified;
    }

    pub fn has_voicemail(&self) -> bool {
        self.voicemail_rel.is_some()
    }

    /// Relative voicemail WAV path under the call store root, if saved.
    pub fn voicemail_path(&self) -> Option<&str> {
        self.voicemail_rel.as_deref()
    }

    pub fn voicemail_secs(&self) -> f64 {
        self.vm_duration_ms as f64 / 1000.0
    }

    /// Seconds since call start (resource-shaming math).
    pub fn elapsed_secs(&self) -> f64 {
        self.started_instant.elapsed().as_secs_f64()
    }

    pub fn caller(&self) -> &str {
        &self.caller_id
    }

    pub fn serial_str(&self) -> &str {
        &self.serial
    }

    pub fn reply_method(&self) -> Option<&str> {
        self.reply_method.as_deref()
    }

    pub fn reply_contact(&self) -> Option<&str> {
        self.reply_contact.as_deref()
    }

    /// Start/stop capturing DTMF-1 AI-only uplink (after wait → hangup; no menu/hold).
    pub fn set_ai_segment_active(&mut self, on: bool) {
        if on {
            // Fresh segment: only speech after wait prompt.
            self.ai_remote_pcm.clear();
            self.ai_segment_active = true;
            // Wall-clock span in `remote_mix`; at seal `_ai.wav` is cut from the
            // echo-cleaned caller track instead of the raw frame concat.
            self.ai_seg_start = Some(self.elapsed_bytes());
            self.ai_seg_end = None;
            self.push_event("ai_segment_start");
        } else {
            self.ai_segment_active = false;
            if self.ai_seg_start.is_some() && self.ai_seg_end.is_none() {
                self.ai_seg_end = Some(self.elapsed_bytes());
            }
            if !self.ai_remote_pcm.is_empty() {
                self.push_event("ai_segment_end");
            }
        }
    }

    pub fn contact_verified(&self) -> bool {
        self.contact_verified
    }

    /// Wall-clock bytes of s16le mono that should exist since call start.
    fn elapsed_bytes(&self) -> usize {
        let ms = self.started_instant.elapsed().as_millis() as u64;
        let b = ms
            .saturating_mul(u64::from(self.sample_rate))
            .saturating_mul(2)
            / 1000;
        // Keep even (complete samples).
        (b & !1u64) as usize
    }

    fn ensure_len(buf: &mut Vec<u8>, len: usize, cap: usize) {
        let target = len.min(cap);
        if buf.len() < target {
            buf.resize(target, 0);
        }
    }

    /// Mix s16le samples into `buf` at wall-clock offset for this chunk.
    ///
    /// Kept as the wall-clock index reference (`elapsed` end-aligned). Single-stream
    /// capture uses [`place_stream`] (copy/append) so WS bursts are not mix-added
    /// onto themselves.
    #[allow(dead_code)]
    fn mix_at_now(buf: &mut Vec<u8>, pcm: &[u8], sample_rate: u32, started: Instant, cap: usize) {
        if pcm.is_empty() {
            return;
        }
        let ms = started.elapsed().as_millis() as u64;
        let end =
            ((ms.saturating_mul(u64::from(sample_rate)).saturating_mul(2) / 1000) & !1) as usize;
        let start = end.saturating_sub(pcm.len() & !1);
        let end = start.saturating_add(pcm.len() & !1).min(cap);
        if start >= cap || start >= end {
            return;
        }
        Self::ensure_len(buf, end, cap);
        let dst = &mut buf[start..end];
        let src = &pcm[..dst.len()];
        mix_s16le_add(dst, src);
    }

    /// Place one capture stream at wall-clock without saturating-add.
    ///
    /// Continuous frames append; a real gap jumps to now (silence in between);
    /// a burst that would overlap **appends** so samples are not doubled/clipped.
    fn place_stream(
        buf: &mut Vec<u8>,
        pcm: &[u8],
        sample_rate: u32,
        started: Instant,
        cap: usize,
        cursor: &mut usize,
    ) {
        if pcm.is_empty() {
            return;
        }
        let n = pcm.len() & !1;
        if n == 0 {
            return;
        }
        let ms = started.elapsed().as_millis() as u64;
        let wall_end =
            ((ms.saturating_mul(u64::from(sample_rate)).saturating_mul(2) / 1000) & !1) as usize;
        let wall_start = wall_end.saturating_sub(n);
        // 40 ms: jitter, not a dropped-packet gap.
        let jitter = ((sample_rate as usize / 25) * 2).max(4);
        let start = if *cursor == 0 {
            wall_start
        } else if wall_start + jitter < *cursor {
            *cursor
        } else if wall_start > *cursor + jitter {
            wall_start
        } else {
            *cursor
        };
        let start = start.min(cap);
        let end = start.saturating_add(n).min(cap);
        if start >= cap || start >= end {
            return;
        }
        Self::ensure_len(buf, end, cap);
        buf[start..end].copy_from_slice(&pcm[..end - start]);
        *cursor = end;
    }

    /// Mark agent→phone system audio (TTS/hold/TRTC) so sidetone can be filtered.
    pub fn set_tts_outbound_active(&mut self, active: bool) {
        self.tts_outbound_active = active;
        if !active {
            self.last_tts_tail.clear();
        }
    }

    pub fn tts_outbound_active(&self) -> bool {
        self.tts_outbound_active
    }

    /// True when uplink looks like inject echo of our recent TTS/hold (sidetone).
    /// Public so IVR can skip false DTMF / TRTC STT while system playout is active.
    pub fn uplink_is_sidetone(&self, uplink: &[u8]) -> bool {
        if !self.tts_outbound_active || uplink.len() < 4 {
            return false;
        }
        let n = uplink.len() & !1;
        // Prefer correlating against the recent outbound tail (capture lag ~tens of ms).
        if self.last_tts_tail.len() >= 4 {
            let tn = n.min(self.last_tts_tail.len()) & !1;
            if tn >= 4 {
                let t0 = self.last_tts_tail.len() - tn;
                let corr = pcm_norm_corr_abs(&uplink[..tn], &self.last_tts_tail[t0..]);
                // Lower bar: catch more handset echo of menu so DTMF only sees remote keypad.
                if corr >= 0.30 {
                    return true;
                }
            }
        }
        // Wall-clock slice of the TTS-only track at "now".
        let ms = self.started_instant.elapsed().as_millis() as u64;
        let end = ((ms
            .saturating_mul(u64::from(self.sample_rate))
            .saturating_mul(2)
            / 1000)
            & !1) as usize;
        let start = end.saturating_sub(n);
        if start < self.tts_mix.len() {
            let t_end = end.min(self.tts_mix.len());
            if t_end > start {
                let tts = &self.tts_mix[start..t_end];
                if pcm_rms_s16le(tts) >= 160.0 {
                    let corr = pcm_norm_corr_abs(&uplink[..tts.len().min(n)], tts);
                    if corr >= 0.30 {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Energy gate for the live AI uplink: true when this caller frame is most
    /// likely our own inject coming back (Pixel capture returns it at ~full level
    /// 175–245 ms later — 10–20× louder than the real caller). Compares the frame
    /// against the loudest 10 ms of system audio sent in the last 600 ms; unlike
    /// [`uplink_is_sidetone`] it keeps working after `tts_outbound_active` clears,
    /// which is exactly when the echo tail arrives.
    ///
    /// The window is the last 600 ms of *wall clock* (`tts_mix` is wall-clock
    /// placed; Gemini bursts can sit ahead of "now" and are not played yet).
    /// It must never fall back to "the last 600 ms of TTS ever written": once the
    /// AI stopped talking that froze the reference at its own loud words and
    /// zeroed every later caller frame (Gemini heard nothing after 「喂？」,
    /// `eou=0` on every call).
    pub fn uplink_is_echo(&mut self, uplink: &[u8]) -> bool {
        let sr = self.sample_rate.max(8000) as usize;
        let (start, end) = echo_ref_window(self.elapsed_bytes(), self.tts_mix.len(), sr);
        if end <= start + 4 {
            return false;
        }
        let f = (sr / 100) * 2;
        let mut ref_e = 0f32;
        let mut i = start;
        while i + f <= end {
            let r = pcm_rms_s16le(&self.tts_mix[i..i + f]);
            ref_e = ref_e.max(r * r);
            i += f;
        }
        if ref_e < 300.0 * 300.0 {
            return false;
        }
        let up = pcm_rms_s16le(uplink);
        let ratio = (up * up) / ref_e.max(1.0);
        if self.echo_probe.len() < ECHO_PROBE_CAP {
            self.echo_probe.push(ratio);
        }
        self.check_echo_free(ratio);
        // Echo path gain measured ~1.1–1.25; caller must be clearly louder to pass.
        ratio < ECHO_GATE_PASS_RATIO
    }

    /// Decide once, over the first [`ECHO_FREE_MIN_FRAMES`] frames with our audio in
    /// the gate window, whether this phone returns our own voice. The loudest frame
    /// counts: echo shows up at ≈0 dB, an echo-free line stays far below even when
    /// the caller talks over our 「喂？」.
    fn check_echo_free(&mut self, ratio: f32) {
        if self.echo_free.is_some() {
            return;
        }
        self.echo_check_frames += 1;
        self.echo_check_max_ratio = self.echo_check_max_ratio.max(ratio);
        if self.echo_check_frames < ECHO_FREE_MIN_FRAMES {
            return;
        }
        let free = echo_free_verdict(self.echo_check_max_ratio);
        self.echo_free = Some(free);
        if free && self.barge_in_allowed {
            self.barge_in.store(true, Ordering::Relaxed);
        }
        info!(
            "echo check id={} serial={} frames={} max_db={:.1} echo_free={free} barge_in={}",
            self.id,
            self.serial,
            self.echo_check_frames,
            10.0 * self.echo_check_max_ratio.max(1e-6).log10(),
            free && self.barge_in_allowed
        );
    }

    /// Echo check verdict: `Some(true)` = this phone does not return our own audio.
    pub fn echo_free(&self) -> Option<bool> {
        self.echo_free
    }

    /// Shared with the Gemini session: true once the caller may talk over the AI.
    pub fn barge_in_flag(&self) -> Arc<AtomicBool> {
        self.barge_in.clone()
    }

    pub fn barge_in_on(&self) -> bool {
        self.barge_in.load(Ordering::Relaxed)
    }

    /// Ring prewarm: the Gemini session started before this call record existed and
    /// already holds its own flag — the echo check sets that one from now on.
    pub fn share_barge_in_flag(&mut self, flag: Arc<AtomicBool>) {
        flag.store(self.barge_in.load(Ordering::Relaxed), Ordering::Relaxed);
        self.barge_in = flag;
    }

    pub fn set_barge_in_allowed(&mut self, allowed: bool) {
        self.barge_in_allowed = allowed;
        if !allowed {
            self.barge_in.store(false, Ordering::Relaxed);
        }
    }

    /// (frames, frames gated, p50 dB, p90 dB) of uplink/reference energy while
    /// the live echo gate had system audio in its window.
    pub fn echo_probe_summary(&self) -> Option<(usize, usize, f32, f32)> {
        if self.echo_probe.is_empty() {
            return None;
        }
        let gated = self
            .echo_probe
            .iter()
            .filter(|r| **r < ECHO_GATE_PASS_RATIO)
            .count();
        let mut db: Vec<f32> = self
            .echo_probe
            .iter()
            .map(|r| 10.0 * r.max(1e-6).log10())
            .collect();
        db.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p = |q: f32| db[((db.len() - 1) as f32 * q) as usize];
        Some((db.len(), gated, p(0.5), p(0.9)))
    }

    pub fn write_uplink(&mut self, pcm: &[u8]) {
        if pcm.is_empty() {
            return;
        }
        self.uplink_bytes = self.uplink_bytes.saturating_add(pcm.len() as u64);
        let cap = self.mix_bytes_cap as usize;
        // Unity-gain archive. Per-frame AGC used to pump the cellular noise
        // floor into `_remote.wav` (live inject does not use this path).
        Self::place_stream(
            &mut self.remote_mix,
            pcm,
            self.sample_rate,
            self.started_instant,
            cap,
            &mut self.remote_write,
        );
        // Press-1 AI segment: plain concat of caller frames (for Whisper → summary only).
        if self.ai_segment_active {
            let room = (self.mix_bytes_cap as usize).saturating_sub(self.ai_remote_pcm.len());
            if room > 0 {
                let take = room.min(pcm.len()) & !1;
                if take > 0 {
                    self.ai_remote_pcm.extend_from_slice(&pcm[..take]);
                }
            }
        }
        // Full listen mix is rebuilt at seal (TTS + ducked remote). Live mix-add
        // of inject echo + clean TTS is what made `/calls/` harsh.
    }

    /// Record system outbound PCM into `_tts.wav` (the system-only track).
    /// Local Piper/menu/hold and TRTC downlink→phone playout (`spawn_trtc_downlink_pull_loop`)
    /// share this lane — caller uplink must never call this.
    pub fn write_tts(&mut self, pcm: &[u8]) {
        if !self.record_tts || pcm.is_empty() {
            return;
        }
        let cap = self.mix_bytes_cap as usize;
        Self::place_stream(
            &mut self.tts_mix,
            pcm,
            self.sample_rate,
            self.started_instant,
            cap,
            &mut self.tts_write,
        );
        self.tts_bytes = self.tts_bytes.saturating_add(pcm.len() as u64);
        // Rolling tail for sidetone correlation (~200 ms @ sample_rate).
        self.last_tts_tail.extend_from_slice(pcm);
        let max_tail = (self.sample_rate as usize / 5).saturating_mul(2).max(4);
        if self.last_tts_tail.len() > max_tail {
            let drop = self.last_tts_tail.len() - max_tail;
            self.last_tts_tail.drain(..drop);
        }
    }

    /// Background sound frame sent on its own (an AI pause on an echo-free phone): recorded
    /// apart from the AI's voice so the live echo gate never treats it as ours.
    pub fn write_bed(&mut self, pcm: &[u8]) {
        if !self.record_tts || pcm.is_empty() {
            return;
        }
        let cap = self.mix_bytes_cap as usize;
        Self::place_stream(
            &mut self.bed_mix,
            pcm,
            self.sample_rate,
            self.started_instant,
            cap,
            &mut self.bed_write,
        );
    }

    /// The phone dropped its queue (the caller cut in, or room noise made way for
    /// a reply): what we had sent for later than `ahead` from now was never
    /// played, so it leaves the recording — the AI track too when `ai` — and the
    /// next audio is placed where it was heard. Otherwise every cut reply runs
    /// on ~1 s in the recording over the caller who had interrupted it.
    pub fn drop_unplayed(&mut self, ahead: std::time::Duration, ai: bool) {
        let ms = (self.started_instant.elapsed() + ahead).as_millis() as u64;
        let pos = ((ms * u64::from(self.sample_rate) * 2 / 1000) & !1) as usize;
        if ai {
            truncate_stream(&mut self.tts_mix, &mut self.tts_write, pos);
        }
        truncate_stream(&mut self.bed_mix, &mut self.bed_write, pos);
    }

    /// Pad tracks to wall-clock and rebuild the listen mix (TTS + ducked remote).
    fn seal_timeline(&mut self) {
        let cap = self.mix_bytes_cap as usize;
        let end = self.elapsed_bytes().min(cap);
        Self::ensure_len(&mut self.remote_mix, end, cap);
        if self.record_tts {
            Self::ensure_len(&mut self.tts_mix, end, cap);
        }
        // Strip the inject echo before anything else uses the caller track
        // (listen mix, `_remote.wav`, `_ai.wav` → Whisper → summary). An
        // echo-free phone has none, and stripping muted whatever the caller said
        // over the AI (their interruptions vanished from the recording).
        let echo_free = self.echo_free == Some(true);
        if !self.tts_mix.is_empty() && !echo_free {
            self.remote_mix = suppress_echo_s16le(&self.remote_mix, &self.tts_mix, self.sample_rate);
        }
        self.remote_mix = normalize_caller_s16le(&self.remote_mix, self.sample_rate);
        if let Some(start) = self.ai_seg_start {
            let end = self.ai_seg_end.unwrap_or(self.remote_mix.len()).min(self.remote_mix.len());
            if start < end {
                self.ai_remote_pcm = self.remote_mix[start & !1..end & !1].to_vec();
            }
        }
        // Room noise played in the AI's pauses joins the AI track: `_tts.wav` and
        // the listen mix hold everything the caller heard from us.
        if self.record_tts && !self.bed_mix.is_empty() {
            let n = self.bed_mix.len().min(cap) & !1;
            Self::ensure_len(&mut self.tts_mix, n, cap);
            let n = n.min(self.tts_mix.len());
            mix_s16le_add(&mut self.tts_mix[..n], &self.bed_mix[..n]);
        }
        self.mix = build_listen_mix_opts(&self.remote_mix, &self.tts_mix, self.sample_rate, !echo_free);
        if self.mix.len() > cap {
            self.mix.truncate(cap & !1);
        }
        peak_limit_down_s16le(&mut self.remote_mix, 0.89);
        peak_limit_down_s16le(&mut self.tts_mix, 0.89);
        peak_limit_down_s16le(&mut self.mix, 0.89);
    }
}

/// Live echo gate: a caller frame passes only above this uplink/reference energy
/// ratio (echo path gain ~1.1–1.25 × 4 margin ≈ +7 dB over our loudest 10 ms).
const ECHO_GATE_PASS_RATIO: f32 = 1.25 * 4.0;
/// Max probe samples per call (~4 min of 60 ms frames with AI audio in window).
const ECHO_PROBE_CAP: usize = 4096;
/// Frames with our audio in the gate window before the echo verdict (~0.7 s:
/// the echo of even a short 「喂？」 lands 175–245 ms in).
const ECHO_FREE_MIN_FRAMES: u32 = 12;
/// Echo-free when no frame got within −20 dB of our loudest 10 ms (Pixel 7/7a
/// echo ≈ 0 dB; Pixel 9 measured −37…−60 dB, caller talk included).
const ECHO_FREE_MAX_RATIO: f32 = 0.01;

fn echo_free_verdict(max_ratio: f32) -> bool {
    max_ratio < ECHO_FREE_MAX_RATIO
}
/// How far back the live echo gate looks for our own audio (echo returns
/// 175–245 ms later ±40 ms, plus decay).
const ECHO_REF_WINDOW_MS: usize = 600;

/// Byte range `[start, end)` of `tts_mix` the live echo gate compares against:
/// the last [`ECHO_REF_WINDOW_MS`] of wall clock, clipped to what was written.
/// Empty (`end <= start`) once our audio is older than the window.
fn echo_ref_window(now_bytes: usize, tts_len: usize, sample_rate: usize) -> (usize, usize) {
    let start = now_bytes.saturating_sub(sample_rate * 2 * ECHO_REF_WINDOW_MS / 1000) & !1;
    let end = now_bytes.min(tts_len) & !1;
    (start, end)
}

/// Remove our own injected audio from the caller track.
///
/// Pixel `VOICE_DOWNLINK` capture contains the TYPE_TELEPHONY inject at roughly
/// full level ~200 ms later (measured: echo RMS ≈ system RMS, delay
/// 175–245 ms). That made `_remote.wav` / the listen mix sound doubled and fed
/// the AI's own words to Whisper as "caller speech".
///
/// Residual echo suppressor driven by the known reference (`tts`, same wall-clock):
/// 1. per-call delay = best log-energy envelope correlation over 0–400 ms,
/// 2. echo path gain = median remote/ref energy where the reference is active,
/// 3. per 10 ms frame, mute (≈−46 dB) where remote ≤ ~2× the expected echo;
///    pass through when clearly louder (caller talking over the AI).
/// Gains drop instantly and recover over ~40 ms. Idempotent enough to re-run
/// on already-cleaned tracks.
pub fn suppress_echo_s16le(remote: &[u8], tts: &[u8], sample_rate: u32) -> Vec<u8> {
    let f = (sample_rate as usize / 100).max(1); // samples per 10 ms frame
    let rs: Vec<f32> = remote
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32)
        .collect();
    let ts: Vec<f32> = tts
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32)
        .collect();
    let n = rs.len().min(ts.len()) / f;
    if n < 20 {
        return remote.to_vec();
    }
    let energy = |x: &[f32], i: usize| -> f32 {
        let s = &x[i * f..(i + 1) * f];
        s.iter().map(|v| v * v).sum::<f32>() / f as f32
    };
    let er: Vec<f32> = (0..n).map(|i| energy(&rs, i)).collect();
    let et: Vec<f32> = (0..n).map(|i| energy(&ts, i)).collect();
    if et.iter().all(|e| *e < 300.0 * 300.0) {
        return remote.to_vec(); // no system audio played → nothing to remove
    }

    // 1) delay (frames) by envelope correlation.
    let lr: Vec<f32> = er.iter().map(|e| (e + 1e3).ln()).collect();
    let lt: Vec<f32> = et.iter().map(|e| (e + 1e3).ln()).collect();
    let corr = |a: &[f32], b: &[f32]| -> f32 {
        let m = a.len() as f32;
        let (ma, mb) = (a.iter().sum::<f32>() / m, b.iter().sum::<f32>() / m);
        let (mut num, mut va, mut vb) = (0f32, 0f32, 0f32);
        for (x, y) in a.iter().zip(b) {
            num += (x - ma) * (y - mb);
            va += (x - ma) * (x - ma);
            vb += (y - mb) * (y - mb);
        }
        if va <= 1e-9 || vb <= 1e-9 {
            -2.0
        } else {
            num / (va.sqrt() * vb.sqrt())
        }
    };
    let mut delay = 20usize;
    let mut best = -2f32;
    for k in 0..=40usize.min(n - 1) {
        let c = corr(&lt[..n - k], &lr[k..]);
        if c > best {
            best = c;
            delay = k;
        }
    }

    // 2) echo path gain (median ratio over frames where the delayed reference is loud).
    let mut ratios: Vec<f32> = (delay..n)
        .filter(|&i| et[i - delay] > 300.0 * 300.0)
        .map(|i| er[i] / et[i - delay].max(1.0))
        .collect();
    let path_gain = if ratios.len() > 20 {
        ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ratios[ratios.len() / 2].clamp(0.05, 4.0)
    } else {
        1.0
    };

    // 3) per-frame gain from expected echo (delayed reference smeared −2..+6 frames).
    let mut target = vec![1f32; n];
    for (i, t) in target.iter_mut().enumerate() {
        let mut ref_e = 0f32;
        // Delay wanders ±40 ms around the estimate within one call; codec smear
        // and decay add ~80 ms of tail.
        for k in -6i64..=10 {
            let j = i as i64 - delay as i64 - k;
            if j >= 0 && (j as usize) < n {
                ref_e = ref_e.max(et[j as usize]);
            }
        }
        let expected = ref_e * path_gain;
        if expected > 120.0 * 120.0 {
            let ratio = er[i] / expected.max(1.0);
            // Real callers are ~15× quieter than the echo, so only treat a frame
            // as double-talk when it is far above the expected echo.
            let open = ((ratio - 4.0) / 8.0).clamp(0.0, 1.0);
            *t = ECHO_FLOOR_GAIN + (1.0 - ECHO_FLOOR_GAIN) * open;
        }
    }
    let mut out = Vec::with_capacity(remote.len());
    let mut cur = 1f32;
    let mut prev = 1f32;
    for i in 0..n {
        cur = if target[i] < cur {
            target[i]
        } else {
            cur + (target[i] - cur) * 0.25
        };
        for s in 0..f {
            let g = prev + (cur - prev) * (s as f32 / f as f32);
            let v = (rs[i * f + s] * g).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
        prev = cur;
    }
    // Tail beyond the reference: unchanged.
    out.extend_from_slice(&remote[(n * f * 2).min(remote.len())..]);
    tracing::debug!("echo suppress delay_ms={} path_gain={path_gain:.2} env_corr={best:.2}", delay * 10);
    out
}

/// Echo-only frames are muted outright: the capture floor is digital silence
/// (~1 RMS), so any residual stays intelligible to Whisper and audible after
/// the caller-level boost in [`normalize_caller_s16le`].
const ECHO_FLOOR_GAIN: f32 = 0.0;

/// Bring quiet caller speech up to a comfortable level for the web player and
/// Whisper. Pixel capture delivers the far end at ~100–500 RMS (≈ −40 dBFS) with
/// a ~1 RMS floor, so a fixed per-call gain (no pumping AGC) is clean.
/// Gain = target / 80th-percentile speech-frame RMS, clamped to 1–16×.
pub fn normalize_caller_s16le(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let f = ((sample_rate as usize / 50).max(1)) * 2; // 20 ms frames (bytes)
    let mut speech: Vec<f32> = pcm
        .chunks(f)
        .filter(|c| c.len() == f)
        .map(pcm_rms_s16le)
        .filter(|r| *r >= 40.0)
        .collect();
    if speech.len() < 10 {
        return pcm.to_vec();
    }
    speech.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p80 = speech[speech.len() * 8 / 10].max(1.0);
    let gain = (3200.0 / p80).clamp(1.0, 16.0);
    if gain < 1.2 {
        return pcm.to_vec();
    }
    let mut out = Vec::with_capacity(pcm.len());
    for c in pcm.chunks_exact(2) {
        let v = (i16::from_le_bytes([c[0], c[1]]) as f32 * gain)
            .round()
            .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    peak_limit_down_s16le(&mut out, 0.89);
    out
}

/// Saturating add of two s16le streams (in-place into `dst`).
#[allow(dead_code)]
fn mix_s16le_add(dst: &mut [u8], src: &[u8]) {
    let n = dst.len().min(src.len()) & !1;
    for i in (0..n).step_by(2) {
        let a = i16::from_le_bytes([dst[i], dst[i + 1]]) as i32;
        let b = i16::from_le_bytes([src[i], src[i + 1]]) as i32;
        let sum = (a + b).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        let bytes = sum.to_le_bytes();
        dst[i] = bytes[0];
        dst[i + 1] = bytes[1];
    }
}

/// Scale s16le down so peak ≤ `target_peak` (0..1 of i16::MAX). Never boosts.
fn peak_limit_down_s16le(pcm: &mut [u8], target_peak: f32) -> f32 {
    let n = pcm.len() & !1;
    if n < 2 {
        return 1.0;
    }
    let mut peak = 0i32;
    for i in (0..n).step_by(2) {
        let a = (i16::from_le_bytes([pcm[i], pcm[i + 1]]) as i32).abs();
        if a > peak {
            peak = a;
        }
    }
    if peak < 64 {
        return 1.0;
    }
    let target = target_peak.clamp(0.05, 0.99) * i16::MAX as f32;
    if peak as f32 <= target {
        return 1.0;
    }
    let gain = target / peak as f32;
    for i in (0..n).step_by(2) {
        let s = i16::from_le_bytes([pcm[i], pcm[i + 1]]) as f32 * gain;
        let out = s.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        let b = out.to_le_bytes();
        pcm[i] = b[0];
        pcm[i + 1] = b[1];
    }
    gain
}

/// Peak-normalize s16le mono toward `target_peak` (0..1 of i16::MAX). Returns gain applied.
fn peak_normalize_s16le(pcm: &mut [u8], target_peak: f32) -> f32 {
    let n = pcm.len() & !1;
    if n < 2 {
        return 1.0;
    }
    let mut peak = 0i32;
    for i in (0..n).step_by(2) {
        let s = i16::from_le_bytes([pcm[i], pcm[i + 1]]) as i32;
        let a = s.abs();
        if a > peak {
            peak = a;
        }
    }
    if peak < 64 {
        // Near-silence — do not amplify noise floor into "speech".
        return 1.0;
    }
    let target = (target_peak.clamp(0.05, 0.95) * i16::MAX as f32) as f32;
    let gain = (target / peak as f32).clamp(1.0, 12.0);
    if (gain - 1.0).abs() < 0.05 {
        return 1.0;
    }
    for i in (0..n).step_by(2) {
        let s = i16::from_le_bytes([pcm[i], pcm[i + 1]]) as f32 * gain;
        let out = s.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        let b = out.to_le_bytes();
        pcm[i] = b[0];
        pcm[i + 1] = b[1];
    }
    gain
}

fn pcm_rms_s16le(pcm: &[u8]) -> f32 {
    let n = (pcm.len() / 2).max(1);
    let mut acc = 0.0f64;
    let mut i = 0;
    while i + 1 < pcm.len() {
        let s = i16::from_le_bytes([pcm[i], pcm[i + 1]]) as f64;
        acc += s * s;
        i += 2;
    }
    (acc / n as f64).sqrt() as f32
}

/// Trim leading inject-beep bleed (loud near-constant tone) then RMS-boost speech.
/// Returns (pcm, trim_ms, gain).
fn prepare_voicemail_pcm(pcm: &[u8], sample_rate: u32) -> (Vec<u8>, u32, f32) {
    let sr = sample_rate.max(1) as usize;
    let win = ((sr / 50) * 2).max(2); // 20 ms
    let max_trim = (sr * 8 / 10) * 2; // up to 800 ms lead trim (bytes)
    let mut off = 0usize;
    while off + win <= pcm.len().min(max_trim + win) {
        let chunk = &pcm[off..off + win];
        let rms = pcm_rms_s16le(chunk);
        let mut peak = 0i32;
        for i in (0..chunk.len()).step_by(2) {
            let s = i16::from_le_bytes([chunk[i], chunk[i + 1]]) as i32;
            peak = peak.max(s.abs());
        }
        // Steady loud tone (beep sidetone) — not speech.
        if rms >= 2500.0 && peak >= 6000 {
            off += win;
            continue;
        }
        break;
    }
    let mut out = pcm[off.min(pcm.len())..].to_vec();
    if out.len() < 2 {
        return (pcm.to_vec(), 0, 1.0);
    }
    // RMS over active windows only — whole-buffer RMS is silence-diluted and
    // over-amplifies noise when the caller spoke briefly (a quick "testing, testing").
    let win = ((sr / 50) * 2).max(2);
    let mut acc = 0.0f64;
    let mut n_win = 0usize;
    let mut i = 0usize;
    while i + win <= out.len() {
        let wrms = pcm_rms_s16le(&out[i..i + win]) as f64;
        if wrms >= 80.0 {
            acc += wrms * wrms;
            n_win += 1;
        }
        i += win;
    }
    let rms = if n_win > 0 {
        (acc / n_win as f64).sqrt() as f32
    } else {
        pcm_rms_s16le(&out)
    };
    let mut gain = 1.0f32;
    if rms >= 40.0 && rms < 2500.0 {
        gain = (1800.0 / rms).clamp(1.0, 14.0);
        if gain > 1.05 {
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
        } else {
            gain = 1.0;
        }
    }
    let trim_ms = ((off / 2) * 1000 / sr) as u32;
    (out, trim_ms, gain)
}

/// Absolute normalized correlation of two s16le mono buffers (0..1).
fn pcm_norm_corr_abs(a: &[u8], b: &[u8]) -> f32 {
    let n = a.len().min(b.len()) & !1;
    if n < 4 {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    let mut i = 0;
    while i + 1 < n {
        let sa = i16::from_le_bytes([a[i], a[i + 1]]) as f64;
        let sb = i16::from_le_bytes([b[i], b[i + 1]]) as f64;
        dot += sa * sb;
        na += sa * sa;
        nb += sb * sb;
        i += 2;
    }
    if na < 1.0 || nb < 1.0 {
        return 0.0;
    }
    (dot.abs() / (na.sqrt() * nb.sqrt())) as f32
}

/// Cut a wall-clock stream back to `pos`: written audio past it is silenced and
/// the next write lands there.
fn truncate_stream(buf: &mut [u8], cursor: &mut usize, pos: usize) {
    if *cursor <= pos {
        return;
    }
    let end = (*cursor).min(buf.len());
    if pos < end {
        buf[pos..end].fill(0);
    }
    *cursor = pos;
}

/// Listen mix for `/calls/`: system PCM plus remote ducked while TTS is loud.
///
/// Inject echo on VOICE_DOWNLINK is not a linear copy of TTS (corr often &lt;0.2),
/// so correlation sidetone cannot stop the old saturating-add mix from clipping.
pub fn build_listen_mix(remote: &[u8], tts: &[u8], sample_rate: u32) -> Vec<u8> {
    build_listen_mix_opts(remote, tts, sample_rate, true)
}

/// [`build_listen_mix`]; `duck = false` for echo-free calls: the caller track
/// holds only the caller, so both sides play at full level like on the line
/// (ducking would also bury them under the background sound in the AI track).
pub fn build_listen_mix_opts(remote: &[u8], tts: &[u8], sample_rate: u32, duck: bool) -> Vec<u8> {
    let n = remote.len().max(tts.len()) & !1;
    if n < 2 {
        return Vec::new();
    }
    let win = ((sample_rate.max(1) as usize / 50) * 2).max(2); // 20 ms
    let mut out = vec![0u8; n];
    let mut i = 0usize;
    while i < n {
        let e = (i + win).min(n);
        let t_slice = if i < tts.len() {
            &tts[i..e.min(tts.len())]
        } else {
            &[]
        };
        let r_slice = if i < remote.len() {
            &remote[i..e.min(remote.len())]
        } else {
            &[]
        };
        let t_rms = if t_slice.len() >= 4 {
            pcm_rms_s16le(t_slice)
        } else {
            0.0
        };
        let duck = if duck && t_rms >= 400.0 { 0.12 } else { 1.0 };
        let span = e - i;
        let mut off = 0usize;
        while off + 1 < span {
            let t = if off + 1 < t_slice.len() {
                i16::from_le_bytes([t_slice[off], t_slice[off + 1]]) as i32
            } else {
                0
            };
            let r = if off + 1 < r_slice.len() {
                i16::from_le_bytes([r_slice[off], r_slice[off + 1]]) as i32
            } else {
                0
            };
            let mixed = t + ((r as f32) * duck).round() as i32;
            let v = mixed.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let b = v.to_le_bytes();
            out[i + off] = b[0];
            out[i + off + 1] = b[1];
            off += 2;
        }
        i = e;
    }
    peak_limit_down_s16le(&mut out, 0.89);
    out
}

pub struct CallStore {
    root: PathBuf,
    record_enabled: bool,
    record_tts: bool,
    retention_days: u32,
    index: Mutex<CallIndex>,
    /// Monotonic snapshot counter: bumped under the index lock for every save.
    index_seq: std::sync::atomic::AtomicU64,
    /// Serializes disk writes; holds the seq of the newest snapshot on disk so an
    /// older snapshot that finishes later never overwrites a newer one.
    index_written: Arc<std::sync::Mutex<u64>>,
    /// Small LRU-ish cache of rebuilt `/listen` WAVs (call id → bytes).
    listen_cache: std::sync::Mutex<Vec<(String, bytes::Bytes)>>,
}

const LISTEN_CACHE_MAX: usize = 6;

impl CallStore {
    pub fn new(
        root: PathBuf,
        record_enabled: bool,
        record_tts: bool,
        retention_days: u32,
    ) -> Arc<Self> {
        if record_enabled {
            if let Err(e) = fs::create_dir_all(&root) {
                warn!("calls dir create {}: {e}", root.display());
            }
        }
        let mut index = load_index(&root).unwrap_or_default();
        // Drop rows created by a work-profile twin of the phone app (empty CID copies of
        // real calls); its audio files age out with retention.
        let before = index.calls.len();
        index.calls.retain(|c| !is_twin_serial(&c.serial));
        if index.calls.len() != before {
            info!("dropped {} twin-session call rows", before - index.calls.len());
            if let Ok(data) = serde_json::to_vec(&index) {
                let _ = write_index_bytes(&root, &data);
            }
        }
        Arc::new(Self {
            root,
            record_enabled,
            record_tts,
            retention_days: retention_days.clamp(1, 3650),
            index: Mutex::new(index),
            index_seq: std::sync::atomic::AtomicU64::new(0),
            index_written: Arc::new(std::sync::Mutex::new(0)),
            listen_cache: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Serialize the index while the caller still holds the lock (cheap, in-memory),
    /// then write it on a blocking thread so the call/UI path never waits on disk.
    fn snapshot_index(&self, idx: &CallIndex) -> Option<(u64, Vec<u8>)> {
        let seq = self
            .index_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        match serde_json::to_vec(idx) {
            Ok(data) => Some((seq, data)),
            Err(e) => {
                warn!("index serialize failed: {e:#}");
                None
            }
        }
    }

    async fn write_index_snapshot(&self, snap: Option<(u64, Vec<u8>)>) {
        let Some((seq, data)) = snap else { return };
        let root = self.root.clone();
        let written = self.index_written.clone();
        let res = tokio::task::spawn_blocking(move || {
            let mut last = written.lock().unwrap_or_else(|e| e.into_inner());
            if *last >= seq {
                return Ok(()); // a newer snapshot already landed
            }
            write_index_bytes(&root, &data)?;
            *last = seq;
            Ok::<(), anyhow::Error>(())
        })
        .await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!("save index: {e:#}"),
            Err(e) => warn!("save index task: {e}"),
        }
    }

    pub fn enabled(&self) -> bool {
        self.record_enabled
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn retention_days(&self) -> u32 {
        self.retention_days
    }

    /// Remove metadata and every recording older than the configured retention window.
    ///
    /// Date directories are also pruned so orphaned WAV/PCM files cannot bypass the
    /// index-based cleanup. Only strict `YYYY-MM-DD` children of the calls root are
    /// eligible for directory removal.
    pub async fn purge_expired(&self) {
        if !self.record_enabled {
            return;
        }
        let cutoff = Utc::now() - Duration::days(i64::from(self.retention_days));
        let cutoff_date = cutoff.date_naive();
        let mut expired = Vec::new();
        let mut changed = false;
        let snap = {
            let mut idx = self.index.lock().await;
            idx.calls.retain(|record| {
                let is_expired = DateTime::parse_from_rfc3339(&record.started_at)
                    .map(|dt| dt.with_timezone(&Utc) < cutoff)
                    .unwrap_or(false);
                if is_expired {
                    expired.push(record.clone());
                    changed = true;
                    false
                } else {
                    true
                }
            });
            if changed {
                self.snapshot_index(&idx)
            } else {
                None
            }
        };
        self.write_index_snapshot(snap).await;

        let expired_count = expired.len();
        let root = self.root.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for record in &expired {
                for rel in [
                    record.audio_path.as_deref(),
                    record.tts_audio_path.as_deref(),
                    record.remote_audio_path.as_deref(),
                    record.ai_audio_path.as_deref(),
                    record.voicemail_path.as_deref(),
                ]
                .into_iter()
                .flatten()
                {
                    remove_record_file(&root, rel);
                }
            }
            prune_expired_date_dirs(&root, cutoff_date);
        })
        .await;
        if changed {
            info!(
                "expired call data purged records={} retention_days={}",
                expired_count, self.retention_days
            );
        }
    }

    pub async fn start(
        &self,
        serial: &str,
        caller_id: &str,
        sample_rate: u32,
        initial_status: &str,
    ) -> Option<ActiveCall> {
        if !self.record_enabled {
            return None;
        }
        let id = Uuid::new_v4().to_string();
        let started_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let date_dir = Utc::now().format("%Y-%m-%d").to_string();
        let dir = self.root.join(&date_dir);
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            warn!("calls date dir {}: {e}", dir.display());
            return None;
        }
        let pcm_path = dir.join(format!("{id}.pcm"));
        let tts_pcm_path = dir.join(format!("{id}_tts.pcm"));
        // ~30 minutes of 16-bit mono — covers long hold + IVR without unbounded growth.
        let mix_bytes_cap = u64::from(sample_rate) * 2 * 60 * 30;
        let call = ActiveCall {
            id: id.clone(),
            started_at: started_at.clone(),
            started_instant: Instant::now(),
            serial: serial.to_string(),
            caller_id: caller_id.to_string(),
            status: initial_status.to_string(),
            sample_rate,
            date_dir,
            mix: Vec::with_capacity(sample_rate as usize * 2 * 60),
            tts_mix: Vec::new(),
            remote_mix: Vec::with_capacity(sample_rate as usize * 2 * 60),
            ai_remote_pcm: Vec::new(),
            ai_segment_active: false,
            ai_seg_start: None,
            ai_seg_end: None,
            uplink_bytes: 0,
            tts_bytes: 0,
            mix_bytes_cap,
            events: vec![initial_status.to_string()],
            record_tts: self.record_tts,
            tts_outbound_active: false,
            last_tts_tail: Vec::new(),
            echo_probe: Vec::new(),
            echo_free: None,
            echo_check_frames: 0,
            echo_check_max_ratio: 0.0,
            barge_in: Arc::new(AtomicBool::new(false)),
            barge_in_allowed: false,
            remote_write: 0,
            tts_write: 0,
            bed_mix: Vec::new(),
            bed_write: 0,
            pcm_path,
            tts_pcm_path,
            voicemail_rel: None,
            vm_duration_ms: 0,
            reply_method: None,
            reply_contact: None,
            contact_verified: false,
            appointments: None,
        };
        let draft = call.to_record(None, None, 0);
        let snap = {
            let mut idx = self.index.lock().await;
            idx.calls.insert(0, draft);
            self.snapshot_index(&idx)
        };
        // Index write is off the hot path: the record is already visible in memory
        // for /live and /list; disk persistence follows on a blocking thread.
        let store_root = self.root.clone();
        let written = self.index_written.clone();
        if let Some((seq, data)) = snap {
            tokio::task::spawn_blocking(move || {
                let mut last = written.lock().unwrap_or_else(|e| e.into_inner());
                if *last < seq {
                    if let Err(e) = write_index_bytes(&store_root, &data) {
                        warn!("save index (start): {e:#}");
                    } else {
                        *last = seq;
                    }
                }
            });
        }
        info!(
            "call record start id={id} serial={serial} caller={caller_id} status={initial_status} continuous_mix=true remote_track=true"
        );
        Some(call)
    }

    pub async fn finalize(&self, mut call: ActiveCall, final_status: &str) -> CallRecord {
        call.push_event(final_status);
        call.status = final_status.to_string();
        if let Some((n, gated, p50, p90)) = call.echo_probe_summary() {
            // Uplink vs our loudest recent 10 ms while AI audio was in the gate
            // window. ~0 dB = echo at full level; caller passes above ~+7 dB.
            info!(
                "echo probe id={} serial={} frames={n} gated={gated} up_vs_ref_db p50={p50:.1} p90={p90:.1}",
                call.id, call.serial
            );
        }
        call.seal_timeline();

        let duration_ms = call.started_instant.elapsed().as_millis() as u64;
        let ended_at = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);

        // DTMF-1 AI segment only (caller speech while AI was live).
        call.ai_segment_active = false;
        let sr = call.sample_rate;
        let raw_mix_secs = (call.mix.len() as f64 / 2.0) / f64::from(sr.max(1));

        // Move the PCM buffers out and write every WAV on a blocking thread so the
        // device WS loop (which awaits this before `call_end_ack`) never stalls on
        // tens of MB of synchronous disk IO.
        let mut jobs: Vec<(&'static str, String, Vec<u8>)> = Vec::with_capacity(4);
        if !call.mix.is_empty() {
            jobs.push((
                "mix",
                format!("{}/{}.wav", call.date_dir, call.id),
                std::mem::take(&mut call.mix),
            ));
        }
        if call.record_tts && !call.tts_mix.is_empty() {
            jobs.push((
                "tts",
                format!("{}/{}_tts.wav", call.date_dir, call.id),
                std::mem::take(&mut call.tts_mix),
            ));
        }
        if !call.remote_mix.is_empty() {
            jobs.push((
                "remote",
                format!("{}/{}_remote.wav", call.date_dir, call.id),
                std::mem::take(&mut call.remote_mix),
            ));
        }
        if call.ai_remote_pcm.len() >= sr as usize {
            // ≥0.5s of audio at 16k mono
            jobs.push((
                "ai",
                format!("{}/{}_ai.wav", call.date_dir, call.id),
                std::mem::take(&mut call.ai_remote_pcm),
            ));
        }
        let root = self.root.clone();
        let call_id = call.id.clone();
        let legacy = [call.pcm_path.clone(), call.tts_pcm_path.clone()];
        let written: Vec<(&'static str, String)> = tokio::task::spawn_blocking(move || {
            let mut ok = Vec::with_capacity(jobs.len());
            for (kind, rel, pcm) in jobs {
                match write_wav(&root.join(&rel), &pcm, sr, 1) {
                    Ok(()) => {
                        if kind == "ai" {
                            info!("ai segment wav id={call_id} bytes={} path={rel}", pcm.len());
                        }
                        ok.push((kind, rel));
                    }
                    Err(e) => warn!("{kind} wav write failed id={call_id}: {e:#}"),
                }
            }
            // Clean any leftover legacy pcm paths.
            for p in legacy {
                let _ = fs::remove_file(p);
            }
            ok
        })
        .await
        .unwrap_or_else(|e| {
            warn!("wav write task failed: {e}");
            Vec::new()
        });
        let pick = |k: &str| written.iter().find(|(kind, _)| *kind == k).map(|(_, r)| r.clone());
        let audio_path = pick("mix");

        let mut final_record = call.to_record(Some(ended_at), audio_path, duration_ms);
        final_record.tts_audio_path = pick("tts");
        final_record.remote_audio_path = pick("remote");
        final_record.ai_audio_path = pick("ai");
        final_record.status = final_status.to_string();

        let snap = {
            let mut idx = self.index.lock().await;
            if let Some(slot) = idx.calls.iter_mut().find(|c| c.id == final_record.id) {
                // A late re-finalize must not wipe a summary already written.
                if final_record.summary.is_none() {
                    final_record.summary = slot.summary.take();
                }
                *slot = final_record.clone();
            } else {
                idx.calls.insert(0, final_record.clone());
            }
            idx.calls.sort_by(|a, b| b.started_at.cmp(&a.started_at));
            self.snapshot_index(&idx)
        };
        self.write_index_snapshot(snap).await;
        let mix_secs = if final_record.audio_path.is_some() {
            raw_mix_secs
        } else {
            0.0
        };
        info!(
            "call record end id={} serial={} caller={} dur_ms={} mix_s={:.1} uplink={} tts={} audio={:?} remote={:?}",
            final_record.id,
            final_record.serial,
            final_record.caller_id,
            final_record.duration_ms,
            mix_secs,
            final_record.uplink_bytes,
            final_record.tts_bytes,
            final_record.audio_path,
            final_record.remote_audio_path
        );
        final_record
    }

    pub async fn list(&self) -> Vec<CallRecord> {
        let idx = self.index.lock().await;
        idx.calls.clone()
    }

    /// Filter + paginate call history. `transcript_by_call` is optional (call_id → text)
    /// so `/api/list?q=` can search voicemail transcripts from TaskStore.
    pub async fn list_filtered(
        &self,
        filter: &CallListFilter,
        transcript_by_call: Option<&std::collections::HashMap<String, String>>,
    ) -> CallListPage {
        let idx = self.index.lock().await;
        let mut matched: Vec<CallRecord> = idx
            .calls
            .iter()
            .filter(|c| call_matches(c, filter, transcript_by_call))
            .cloned()
            .collect();
        // Stable newest-first: started_at DESC, then id DESC (documented as sort=started_at_desc).
        matched.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        let total = matched.len();
        let offset = filter.offset.unwrap_or(0).min(total);
        let end = match filter.limit {
            Some(lim) => (offset + lim).min(total),
            None => total,
        };
        let calls = if offset >= total {
            Vec::new()
        } else {
            matched[offset..end].to_vec()
        };
        CallListPage {
            calls,
            total,
            limit: filter.limit,
            offset: filter.offset.unwrap_or(0),
        }
    }

    /// Summary counts for the calls toolbar (today = the machine's calendar day).
    pub async fn stats(&self) -> CallStats {
        let idx = self.index.lock().await;
        let tw = |dt: DateTime<Utc>| dt.with_timezone(&chrono::Local).date_naive();
        let today_tw = tw(Utc::now());
        let mut today = 0usize;
        let mut voicemails = 0usize;
        let mut blocked = 0usize;
        let mut total = 0usize;
        for c in idx.calls.iter().filter(|c| !is_twin_serial(&c.serial)) {
            total += 1;
            let is_today = DateTime::parse_from_rfc3339(&c.started_at)
                .map(|d| tw(d.with_timezone(&Utc)) == today_tw)
                .unwrap_or(false);
            if is_today {
                today += 1;
            }
            if c.voicemail_path.is_some() {
                voicemails += 1;
            }
            // Calls from blocked numbers (and the older spam status).
            if c.status == "blocked_spam" || c.events.iter().any(|e| e == "reject_list") {
                blocked += 1;
            }
        }
        CallStats {
            total,
            today,
            voicemails,
            blocked,
        }
    }

    pub async fn get(&self, id: &str) -> Option<CallRecord> {
        let idx = self.index.lock().await;
        idx.calls.iter().find(|c| c.id == id).cloned()
    }

    /// Store the post-call notes: short summary on the index row (list / notification),
    /// everything else in `{date}/{id}_notes.json` (purged with the date dir).
    pub async fn save_notes(&self, id: &str, notes: &CallNotes) -> Result<()> {
        let (snap, dir) = {
            let mut idx = self.index.lock().await;
            let Some(rec) = idx.calls.iter_mut().find(|c| c.id == id) else {
                anyhow::bail!("call {id} not in index");
            };
            rec.summary = Some(notes.summary.clone());
            let dir = record_date_dir(rec);
            (self.snapshot_index(&idx), dir)
        };
        self.write_index_snapshot(snap).await;
        let dir = dir.with_context(|| format!("no date dir for call {id}"))?;
        let path = self.root.join(dir).join(format!("{id}_notes.json"));
        let data = serde_json::to_vec(notes)?;
        tokio::task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, data).with_context(|| format!("write {}", path.display()))
        })
        .await??;
        Ok(())
    }

    pub async fn load_notes(&self, id: &str) -> Option<CallNotes> {
        let rec = self.get(id).await?;
        let path = self
            .root
            .join(record_date_dir(&rec)?)
            .join(format!("{id}_notes.json"));
        let data = tokio::task::spawn_blocking(move || fs::read(path)).await.ok()?.ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// In-progress sessions still open in the index (`ringing` / `active`).
    pub async fn live_calls(&self) -> Vec<CallRecord> {
        let idx = self.index.lock().await;
        idx.calls
            .iter()
            .filter(|c| {
                let st = c.status.as_str();
                (st == "ringing" || st == "active") && !is_twin_serial(&c.serial)
            })
            .cloned()
            .collect()
    }

    pub fn audio_abs(&self, rel: &str) -> Option<PathBuf> {
        let p = self.root.join(rel);
        if p.is_file() {
            Some(p)
        } else {
            None
        }
    }

    fn read_wav_pcm(&self, rel: &str) -> Option<(Vec<u8>, u32, u16)> {
        let path = self.audio_abs(rel)?;
        let bytes = fs::read(&path).ok()?;
        parse_wav_pcm(&bytes)
    }

    /// Cached `listen_wav` for finished calls (blocking; call from `spawn_blocking`).
    /// Re-mixing a 10-minute call costs ~40 MB of reads + CPU; browsers re-request
    /// on every seek/replay, so keep the last few results in memory.
    pub fn listen_wav_cached(&self, rec: &CallRecord) -> Option<bytes::Bytes> {
        let cacheable = rec.ended_at.is_some();
        if cacheable {
            let cache = self.listen_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, v)) = cache.iter().find(|(id, _)| *id == rec.id) {
                return Some(v.clone());
            }
        }
        let wav = bytes::Bytes::from(self.listen_wav(rec)?);
        if cacheable {
            let mut cache = self.listen_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.retain(|(id, _)| *id != rec.id);
            cache.insert(0, (rec.id.clone(), wav.clone()));
            cache.truncate(LISTEN_CACHE_MAX);
        }
        Some(wav)
    }

    /// Echo-cleaned caller track for `/api/audio/{id}/remote` (blocking; cached).
    /// Calls recorded before the suppressor still have raw echo in `_remote.wav`.
    pub fn remote_clean_cached(&self, rec: &CallRecord) -> Option<bytes::Bytes> {
        let key = format!("{}:remote", rec.id);
        let cacheable = rec.ended_at.is_some();
        if cacheable {
            let cache = self.listen_cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((_, v)) = cache.iter().find(|(id, _)| *id == key) {
                return Some(v.clone());
            }
        }
        let (r, sr, _) = rec
            .remote_audio_path
            .as_deref()
            .and_then(|rel| self.read_wav_pcm(rel))?;
        let tts = if rec.echo_free == Some(true) {
            None
        } else {
            rec.tts_audio_path
                .as_deref()
                .and_then(|rel| self.read_wav_pcm(rel))
        };
        let cleaned = match tts {
            Some((t, _, _)) => suppress_echo_s16le(&r, &t, sr.max(1)),
            None => r,
        };
        let cleaned = normalize_caller_s16le(&cleaned, sr.max(1));
        let wav = bytes::Bytes::from(wav_from_pcm(&cleaned, sr.max(1), 1));
        if cacheable {
            let mut cache = self.listen_cache.lock().unwrap_or_else(|e| e.into_inner());
            cache.retain(|(id, _)| *id != key);
            cache.insert(0, (key, wav.clone()));
            cache.truncate(LISTEN_CACHE_MAX);
        }
        Some(wav)
    }

    /// On-the-fly listen WAV for `/calls/` (ducked mix, peak-limited down only).
    ///
    /// Rebuilds from `_remote` + `_tts` so **old** additive-clip mix files still
    /// play cleanly on the page. Live telephony path is not involved.
    pub fn listen_wav(&self, rec: &CallRecord) -> Option<Vec<u8>> {
        let remote = rec
            .remote_audio_path
            .as_deref()
            .and_then(|rel| self.read_wav_pcm(rel));
        let tts = rec
            .tts_audio_path
            .as_deref()
            .and_then(|rel| self.read_wav_pcm(rel));
        let (mut pcm, sr) = match (remote, tts) {
            (Some((r, rsr, _)), Some((t, tsr, _))) => {
                let sr = if rsr > 0 { rsr } else { tsr }.max(1);
                if rec.echo_free == Some(true) {
                    let r = normalize_caller_s16le(&r, sr);
                    (build_listen_mix_opts(&r, &t, sr, false), sr)
                } else {
                    let r = normalize_caller_s16le(&suppress_echo_s16le(&r, &t, sr), sr);
                    (build_listen_mix(&r, &t, sr), sr)
                }
            }
            (Some((r, rsr, _)), None) => (r, rsr.max(1)),
            (None, Some((t, tsr, _))) => (t, tsr.max(1)),
            (None, None) => {
                let (p, sr, _) = rec
                    .audio_path
                    .as_deref()
                    .and_then(|rel| self.read_wav_pcm(rel))?;
                (p, sr.max(1))
            }
        };
        if pcm.len() < 2 {
            return None;
        }
        peak_limit_down_s16le(&mut pcm, 0.89);
        Some(wav_from_pcm(&pcm, sr, 1))
    }

    /// Write the message-only voicemail WAV next to the call mix and tag the call.
    /// Returns the relative path on success.
    pub fn save_voicemail_wav(&self, call: &mut ActiveCall, pcm: &[u8]) -> Option<String> {
        if pcm.len() < 2 {
            return None;
        }
        // Strip leading beep-sidetone bleed, then RMS-boost quiet caller speech
        // (peak-norm alone fails when the file starts with a loud tone).
        let (boosted, trim_ms, gain) = prepare_voicemail_pcm(pcm, call.sample_rate);
        let rel = format!("{}/{}_vm.wav", call.date_dir, call.id);
        let abs = self.root.join(&rel);
        match write_wav(&abs, &boosted, call.sample_rate, 1) {
            Ok(()) => {
                let dur_ms = (boosted.len() as u64 / 2) * 1000 / u64::from(call.sample_rate.max(1));
                call.voicemail_rel = Some(rel.clone());
                call.vm_duration_ms = dur_ms;
                info!(
                    "voicemail saved id={} bytes={} dur_ms={dur_ms} trim_ms={trim_ms} rms_gain={gain:.2} rel={rel}",
                    call.id,
                    boosted.len()
                );
                Some(rel)
            }
            Err(e) => {
                warn!("voicemail wav write failed id={}: {e:#}", call.id);
                None
            }
        }
    }
}

impl ActiveCall {
    fn to_record(
        &self,
        ended_at: Option<String>,
        audio_path: Option<String>,
        duration_ms: u64,
    ) -> CallRecord {
        CallRecord {
            id: self.id.clone(),
            started_at: self.started_at.clone(),
            ended_at,
            serial: self.serial.clone(),
            caller_id: self.caller_id.clone(),
            duration_ms,
            status: self.status.clone(),
            audio_path,
            tts_audio_path: None,
            remote_audio_path: None,
            ai_audio_path: None,
            uplink_bytes: self.uplink_bytes,
            tts_bytes: self.tts_bytes,
            sample_rate: self.sample_rate,
            events: self.events.clone(),
            voicemail_path: self.voicemail_rel.clone(),
            vm_duration_ms: self.vm_duration_ms,
            reply_method: self.reply_method.clone(),
            reply_contact: self.reply_contact.clone(),
            contact_verified: self.contact_verified,
            summary: None,
            echo_free: self.echo_free,
            appointment: self.appointments.as_ref().and_then(|b| b.outcome()),
        }
    }
}

fn index_path(root: &Path) -> PathBuf {
    root.join("index.json")
}

fn remove_record_file(root: &Path, rel: &str) {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return;
    }
    let path = root.join(rel_path);
    if !path.starts_with(root) {
        return;
    }
    if let Err(e) = fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            warn!(
                "expired recording remove failed path={}: {e}",
                path.display()
            );
        }
    }
}

fn prune_expired_date_dirs(root: &Path, cutoff_date: NaiveDate) {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(name, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff_date {
            if let Err(e) = fs::remove_dir_all(&path) {
                warn!(
                    "expired recordings directory remove failed path={}: {e}",
                    path.display()
                );
            }
        }
    }
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    // Unicode-aware so Mandarin / mixed case Latin both match under `q=`.
    hay.to_lowercase().contains(&needle.to_lowercase())
}

/// Secondary-user / work-profile app copy (`pixel-101NNNN` next to `pixel-1NNNN`).
/// Mirrors main.rs `is_isolated_screening_serial`.
pub fn is_twin_serial(serial: &str) -> bool {
    serial
        .trim()
        .strip_prefix("pixel-")
        .map(|rest| rest.len() >= 7 && rest.starts_with("101") && rest.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn call_matches(
    c: &CallRecord,
    filter: &CallListFilter,
    transcript_by_call: Option<&std::collections::HashMap<String, String>>,
) -> bool {
    if is_twin_serial(&c.serial) {
        return false;
    }
    if let Some(ref st) = filter.status {
        let st = st.trim();
        if !st.is_empty() && !c.status.eq_ignore_ascii_case(st) {
            return false;
        }
    }
    if let Some(ref serial) = filter.serial {
        let serial = serial.trim();
        if !serial.is_empty() && !contains_ci(&c.serial, serial) {
            return false;
        }
    }
    if let Some(ref caller) = filter.caller {
        let caller = caller.trim();
        if !caller.is_empty() && !contains_ci(&c.caller_id, caller) {
            return false;
        }
    }
    if let Some(want_vm) = filter.has_voicemail {
        let has = c.voicemail_path.is_some();
        if has != want_vm {
            return false;
        }
    }
    if let Some(want_tx) = filter.has_transcript {
        let has = transcript_by_call
            .and_then(|m| m.get(&c.id))
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        if has != want_tx {
            return false;
        }
    }
    if let Some(ref q) = filter.q {
        let q = q.trim();
        if !q.is_empty() {
            let in_meta = contains_ci(&c.caller_id, q)
                || contains_ci(&c.serial, q)
                || contains_ci(&c.id, q)
                || contains_ci(&c.status, q)
                || c.summary.as_deref().is_some_and(|s| contains_ci(s, q))
                || c.events.iter().any(|e| contains_ci(e, q))
                || c.reply_contact
                    .as_deref()
                    .map(|r| contains_ci(r, q))
                    .unwrap_or(false);
            let in_transcript = transcript_by_call
                .and_then(|m| m.get(&c.id))
                .map(|t| contains_ci(t, q))
                .unwrap_or(false);
            if !in_meta && !in_transcript {
                return false;
            }
        }
    }
    true
}

fn load_index(root: &Path) -> Result<CallIndex> {
    let path = index_path(root);
    if !path.exists() {
        return Ok(CallIndex::default());
    }
    let mut f = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut buf = String::new();
    f.read_to_string(&mut buf)?;
    Ok(serde_json::from_str(&buf).unwrap_or_default())
}

fn write_index_bytes(root: &Path, data: &[u8]) -> Result<()> {
    fs::create_dir_all(root)?;
    let path = index_path(root);
    let tmp = root.join("index.json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&data)?;
        f.flush()?;
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn wav_from_pcm(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align = channels * bits_per_sample / 8;
    let data_len = pcm.len() as u32;
    let riff_chunk_size = 36 + data_len;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

pub fn parse_wav_pcm(bytes: &[u8]) -> Option<(Vec<u8>, u32, u16)> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12usize;
    let mut channels = 1u16;
    let mut rate = 16_000u32;
    let mut bits = 16u16;
    let mut pcm: Option<Vec<u8>> = None;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let size = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().ok()?) as usize;
        let start = i + 8;
        let end = start.saturating_add(size);
        if end > bytes.len() {
            break;
        }
        if id == b"fmt " && size >= 16 {
            channels = u16::from_le_bytes(bytes[start + 2..start + 4].try_into().ok()?);
            rate = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().ok()?);
            bits = u16::from_le_bytes(bytes[start + 14..start + 16].try_into().ok()?);
        } else if id == b"data" {
            pcm = Some(bytes[start..end].to_vec());
        }
        i = end + (size & 1);
    }
    let pcm = pcm?;
    if bits != 16 || channels == 0 {
        return None;
    }
    Some((pcm, rate.max(1), channels))
}

fn write_wav(path: &Path, pcm: &[u8], sample_rate: u32, channels: u16) -> Result<()> {
    let bytes = wav_from_pcm(pcm, sample_rate, channels);
    let mut f = File::create(path).with_context(|| format!("create {}", path.display()))?;
    f.write_all(&bytes)?;
    f.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_call() -> ActiveCall {
        ActiveCall {
            id: "t".into(),
            started_at: "x".into(),
            started_instant: Instant::now(),
            serial: "s".into(),
            caller_id: String::new(),
            status: "active".into(),
            sample_rate: 16000,
            date_dir: "d".into(),
            mix: Vec::new(),
            tts_mix: Vec::new(),
            remote_mix: Vec::new(),
            ai_remote_pcm: Vec::new(),
            ai_segment_active: false,
            ai_seg_start: None,
            ai_seg_end: None,
            uplink_bytes: 0,
            tts_bytes: 0,
            mix_bytes_cap: 16000 * 2 * 60,
            events: vec![],
            record_tts: true,
            tts_outbound_active: false,
            last_tts_tail: Vec::new(),
            echo_probe: Vec::new(),
            echo_free: None,
            echo_check_frames: 0,
            echo_check_max_ratio: 0.0,
            barge_in: Arc::new(AtomicBool::new(false)),
            barge_in_allowed: true,
            remote_write: 0,
            tts_write: 0,
            bed_mix: Vec::new(),
            bed_write: 0,
            pcm_path: PathBuf::from("/tmp/x.pcm"),
            tts_pcm_path: PathBuf::from("/tmp/x_tts.pcm"),
            voicemail_rel: None,
            vm_duration_ms: 0,
            reply_method: None,
            reply_contact: None,
            contact_verified: false,
            appointments: None,
        }
    }

    #[test]
    fn mix_pads_silence_to_wall_clock() {
        let mut call = test_call();
        std::thread::sleep(Duration::from_millis(50));
        // One short remote burst near "now".
        let tone = vec![100u8, 0u8, 100u8, 0u8]; // 2 samples
        call.write_uplink(&tone);
        call.seal_timeline();
        // ~50ms => ~1600 bytes at 16 kHz s16le; allow slack.
        assert!(
            call.mix.len() >= 1200,
            "expected silence pad, got {} bytes",
            call.mix.len()
        );
        assert!(call.mix.len() % 2 == 0);
        assert!(
            call.remote_mix.len() >= 1200,
            "remote track must pad too, got {}",
            call.remote_mix.len()
        );
    }

    #[test]
    fn write_tts_lands_on_system_lane_not_remote() {
        // TRTC downlink→phone and local Piper/hold share write_tts → `_tts.wav` (the system-only track).
        let mut call = test_call();
        std::thread::sleep(Duration::from_millis(20));
        let mut tone = Vec::new();
        for i in 0..80 {
            let s = if i % 2 == 0 { 8000i16 } else { -8000i16 };
            tone.extend_from_slice(&s.to_le_bytes());
        }
        call.write_tts(&tone);
        call.seal_timeline();
        assert_eq!(call.tts_bytes, tone.len() as u64);
        assert!(
            call.tts_mix.iter().any(|&b| b != 0),
            "system lane (_tts) must receive write_tts PCM"
        );
        assert!(
            call.remote_mix.iter().all(|&b| b == 0),
            "caller remote lane must stay empty for write_tts (僅系統／TRTC)"
        );
        assert!(
            call.mix.iter().any(|&b| b != 0),
            "full mix must also include system outbound"
        );
    }

    #[test]
    fn sidetone_skipped_in_mix_but_remote_always() {
        let mut call = test_call();
        std::thread::sleep(Duration::from_millis(30));
        call.set_tts_outbound_active(true);
        // Build a longer loud burst so correlation is meaningful.
        let mut loud = Vec::new();
        for i in 0..160 {
            let s = if i % 2 == 0 { 12000i16 } else { -12000i16 };
            loud.extend_from_slice(&s.to_le_bytes());
        }
        call.write_tts(&loud);
        assert!(call.tts_bytes > 0);
        // Identical uplink = inject sidetone. Remote always keeps it; listen mix
        // ducks remote while TTS energy is high so the two are not saturating-added.
        call.write_uplink(&loud);
        assert_eq!(call.uplink_bytes, loud.len() as u64);
        call.seal_timeline();
        assert!(
            call.remote_mix.iter().any(|&b| b != 0),
            "remote track must always capture uplink during TTS/hold"
        );
        let tts_peak = peak_abs_s16le(&call.tts_mix);
        let mix_peak = peak_abs_s16le(&call.mix);
        assert!(
            mix_peak <= tts_peak.saturating_add(tts_peak / 5).max(1000),
            "listen mix must not double TTS+echo (mix_peak={mix_peak} tts_peak={tts_peak})"
        );
        call.set_tts_outbound_active(false);
        std::thread::sleep(Duration::from_millis(30));
        let remote_before = call.remote_mix.clone();
        call.write_uplink(&loud);
        assert_ne!(
            call.remote_mix, remote_before,
            "uplink must still land on remote after TTS ends"
        );
    }

    fn peak_abs_s16le(pcm: &[u8]) -> u32 {
        pcm.chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs() as u32)
            .max()
            .unwrap_or(0)
    }

    /// Square-ish wave with RMS == `amp`.
    fn tone_s16le(amp: i16, samples: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(samples * 2);
        for i in 0..samples {
            let s = if i % 2 == 0 { amp } else { -amp };
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    /// Call whose clock started `secs_ago`, with AI audio at [`a`, `b`) s of the
    /// wall-clock `tts_mix` (nothing written after `b`, like a finished turn).
    fn call_with_tts(secs_ago: f32, a: f32, b: f32, amp: i16) -> ActiveCall {
        let mut call = test_call();
        call.started_instant = Instant::now()
            .checked_sub(Duration::from_secs_f32(secs_ago))
            .expect("monotonic clock older than test offset");
        let sr = call.sample_rate as usize;
        let s = (a * sr as f32) as usize * 2;
        let e = (b * sr as f32) as usize * 2;
        call.tts_mix = vec![0u8; s];
        call.tts_mix.extend_from_slice(&tone_s16le(amp, (e - s) / 2));
        call
    }

    /// `len_s` of silence with a tone at [`a`, `b`) s (16 kHz).
    fn tone_at(len_s: f32, a: f32, b: f32, amp: i16) -> Vec<u8> {
        let mut v = vec![0u8; (len_s * 16_000.0) as usize * 2];
        let (s, e) = ((a * 16_000.0) as usize, (b * 16_000.0) as usize);
        v[s * 2..e * 2].copy_from_slice(&tone_s16le(amp, e - s));
        v
    }

    fn rms_at(pcm: &[u8], a: f32, b: f32) -> f32 {
        pcm_rms_s16le(&pcm[(a * 16_000.0) as usize * 2..(b * 16_000.0) as usize * 2])
    }

    #[test]
    fn echo_free_recording_keeps_the_caller_over_the_ai_and_the_room_noise() {
        // Echo-free phone: whatever the caller said over the AI must not be muted
        // as "echo", and the background sound played in pauses must be recorded.
        let build = |echo_free: bool| {
            let mut call = test_call();
            call.started_instant = Instant::now()
                .checked_sub(Duration::from_secs(3))
                .expect("monotonic clock older than test offset");
            call.echo_free = Some(echo_free);
            call.tts_mix = tone_at(3.0, 1.0, 2.0, 8000); // AI 1.0–2.0 s
            call.remote_mix = tone_at(3.0, 1.5, 2.5, 300); // caller cuts in at 1.5 s
            call.bed_mix = tone_at(3.0, 2.0, 3.0, 200); // room noise in the pause
            call.seal_timeline();
            call
        };
        let free = build(true);
        assert!(rms_at(&free.remote_mix, 1.6, 1.9) > 1000.0);
        assert!(rms_at(&free.tts_mix, 2.6, 2.9) > 150.0);
        // Not ducked: both voices at once are louder than the AI alone.
        assert!(rms_at(&free.mix, 1.6, 1.9) > rms_at(&free.tts_mix, 1.6, 1.9) * 1.2);
        assert_eq!(free.to_record(None, None, 0).echo_free, Some(true));
        // A phone that returns our audio still gets its echo removed.
        let echo = build(false);
        assert!(rms_at(&echo.remote_mix, 1.6, 1.9) < 50.0);
    }

    #[test]
    fn unplayed_audio_leaves_the_recording() {
        let mut call = test_call();
        call.started_instant = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("monotonic clock older than test offset");
        // Two 1 s writes now: the second (sent ahead, head burst) lands at 1–2 s.
        call.write_tts(&tone_s16le(8000, 16_000));
        call.write_tts(&tone_s16le(8000, 16_000));
        call.write_bed(&tone_s16le(200, 16_000));
        call.write_bed(&tone_s16le(200, 16_000));
        // Caller cut in: the phone plays ~200 ms more, the rest never reached them.
        call.drop_unplayed(Duration::from_millis(200), true);
        assert!(rms_at(&call.tts_mix, 0.5, 1.15) > 7000.0);
        assert_eq!(rms_at(&call.tts_mix, 1.4, 1.95), 0.0);
        assert_eq!(rms_at(&call.bed_mix, 1.4, 1.95), 0.0);
        // The next audio is placed where the phone left off.
        let at = call.tts_write as f32 / 32_000.0;
        assert!((1.15..1.35).contains(&at), "{at}");
        // A room-noise cut leaves the AI track alone.
        call.write_bed(&tone_s16le(200, 16_000));
        call.drop_unplayed(Duration::ZERO, false);
        assert!(call.bed_write <= call.tts_write);
        assert!(rms_at(&call.tts_mix, 0.5, 1.15) > 7000.0);
    }

    #[test]
    fn echo_gate_blocks_echo_right_after_ai_audio() {
        // AI spoke 1.0–1.5 s, now 1.7 s: the echo tail (≈ same level) is gated.
        let mut call = call_with_tts(1.7, 1.0, 1.5, 8000);
        assert!(call.uplink_is_echo(&tone_s16le(8000, 960)));
        let (n, gated, _, _) = call.echo_probe_summary().expect("probe");
        assert_eq!((n, gated), (1, 1));
    }

    #[test]
    fn echo_gate_releases_once_ai_audio_leaves_window() {
        // Regression: the window was anchored at the end of the last
        // TTS write, so every caller frame after 「喂？」 stayed gated forever.
        let mut call = call_with_tts(3.0, 1.0, 1.5, 8000);
        assert!(
            !call.uplink_is_echo(&tone_s16le(3000, 960)),
            "caller 1.5 s after the AI stopped must reach Gemini"
        );
        let mut later = call_with_tts(20.0, 1.0, 1.5, 12000);
        assert!(
            !later.uplink_is_echo(&tone_s16le(200, 960)),
            "quiet caller long after the AI stopped must reach Gemini"
        );
        assert!(later.echo_probe_summary().is_none(), "no AI audio in window → no probe");
    }

    #[test]
    fn echo_gate_ignores_burst_not_played_yet() {
        // Gemini burst placed at 0.5–1.5 s while now = 0.4 s: nothing played yet.
        let mut call = call_with_tts(0.4, 0.5, 1.5, 8000);
        assert!(!call.uplink_is_echo(&tone_s16le(3000, 960)));
    }

    #[test]
    fn echo_check_enables_barge_in_only_on_echo_free_lines() {
        // Echo-free phone: nothing of our 「喂？」 comes back (caller line noise only).
        let mut p9 = call_with_tts(1.7, 1.0, 1.5, 8000);
        for _ in 0..ECHO_FREE_MIN_FRAMES {
            assert!(p9.uplink_is_echo(&tone_s16le(2, 960)));
        }
        assert_eq!(p9.echo_free, Some(true));
        assert!(p9.barge_in_on());
        // Echoing phone: one frame of our voice back at full level is enough.
        let mut p7 = call_with_tts(1.7, 1.0, 1.5, 8000);
        p7.uplink_is_echo(&tone_s16le(8000, 960));
        for _ in 1..ECHO_FREE_MIN_FRAMES {
            p7.uplink_is_echo(&tone_s16le(2, 960));
        }
        assert_eq!(p7.echo_free, Some(false));
        assert!(!p7.barge_in_on());
        // Echo-free but the setting is off: no barge-in.
        let mut off = call_with_tts(1.7, 1.0, 1.5, 8000);
        off.set_barge_in_allowed(false);
        for _ in 0..ECHO_FREE_MIN_FRAMES {
            off.uplink_is_echo(&tone_s16le(2, 960));
        }
        assert_eq!(off.echo_free, Some(true));
        assert!(!off.barge_in_on());
        // A caller talking over our first words still reads echo-free (−24 dB).
        assert!(echo_free_verdict((500.0f32 / 8000.0).powi(2)));
        assert!(!echo_free_verdict(1.0));
    }

    #[test]
    fn echo_gate_passes_caller_clearly_louder_than_ai() {
        let mut call = call_with_tts(1.6, 1.0, 1.5, 2000);
        assert!(!call.uplink_is_echo(&tone_s16le(9000, 960)));
        let (n, gated, p50, _) = call.echo_probe_summary().expect("probe");
        assert_eq!((n, gated), (1, 0));
        assert!(p50 > 10.0, "9000 vs 2000 RMS ≈ +13 dB, got {p50}");
    }

    #[test]
    fn echo_ref_window_is_wall_clock() {
        let sr = 16_000usize;
        let s = |secs: f32| ((secs * sr as f32) as usize) * 2;
        // TTS ended long ago → empty window (was: last 600 ms of that TTS).
        let (a, b) = echo_ref_window(s(5.0), s(1.5), sr);
        assert!(b <= a);
        // Still speaking with burst ahead of now → window ends at now.
        let (a, b) = echo_ref_window(s(2.0), s(3.0), sr);
        assert_eq!((a, b), (s(1.4), s(2.0)));
    }

    #[test]
    fn burst_uplink_appends_instead_of_mix_add() {
        let mut call = test_call();
        std::thread::sleep(Duration::from_millis(20));
        let mut tone = Vec::new();
        for _ in 0..160 {
            tone.extend_from_slice(&8000i16.to_le_bytes());
        }
        call.write_uplink(&tone);
        call.write_uplink(&tone);
        call.seal_timeline();
        let peak = peak_abs_s16le(&call.remote_mix);
        assert!(
            peak <= 9000,
            "burst frames must not saturating-add (peak={peak})"
        );
        let nonzero = call
            .remote_mix
            .chunks_exact(2)
            .filter(|c| i16::from_le_bytes([c[0], c[1]]) != 0)
            .count();
        assert!(
            nonzero >= 300,
            "both burst frames must be kept, nonzero_samples={nonzero}"
        );
    }

    #[test]
    fn write_uplink_does_not_agc_quiet_frames() {
        let mut call = test_call();
        std::thread::sleep(Duration::from_millis(10));
        let mut quiet = Vec::new();
        for _ in 0..160 {
            quiet.extend_from_slice(&200i16.to_le_bytes());
        }
        call.write_uplink(&quiet);
        let peak = peak_abs_s16le(&call.remote_mix);
        assert!(
            (180..=220).contains(&peak),
            "quiet uplink must stay unity-gain, peak={peak}"
        );
    }

    #[test]
    fn listen_mix_ducks_remote_under_tts() {
        let sr = 16000u32;
        let mut tts = vec![0u8; 640]; // 20 ms
        let mut remote = vec![0u8; 640];
        for i in (0..640).step_by(2) {
            tts[i..i + 2].copy_from_slice(&10_000i16.to_le_bytes());
            remote[i..i + 2].copy_from_slice(&10_000i16.to_le_bytes());
        }
        let mixed = build_listen_mix(&remote, &tts, sr);
        let peak = peak_abs_s16le(&mixed);
        assert!(
            peak < 16_000,
            "ducked mix must not clip-add two 10000 peaks, peak={peak}"
        );
        // Silent TTS window: remote should pass through.
        let silent_tts = vec![0u8; 640];
        let passed = build_listen_mix(&remote, &silent_tts, sr);
        let pass_peak = peak_abs_s16le(&passed);
        assert!(
            (8_000..12_000).contains(&pass_peak),
            "remote must pass when TTS is silent, peak={pass_peak}"
        );
    }

    #[test]
    fn echo_suppressor_removes_delayed_inject_keeps_caller() {
        let sr = 16_000u32;
        let n = sr as usize * 6;
        let mut tts = vec![0i16; n];
        let mut remote = vec![0i16; n];
        // System speech 0.5–2.5 s (noisy tone so envelopes vary).
        let mut seed = 1u32;
        for i in sr as usize / 2..sr as usize * 5 / 2 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let env = ((i as f32 / 900.0).sin().abs() * 0.8 + 0.2) * 6000.0;
            tts[i] = ((i as f32 * 0.07).sin() * env + ((seed >> 16) as f32 / 65536.0 - 0.5) * 800.0) as i16;
        }
        // Echo: same signal, 210 ms later, slightly distorted.
        let d = sr as usize * 21 / 100;
        for i in 0..n - d {
            remote[i + d] = (tts[i] as f32 * -0.9) as i16;
        }
        // Real caller speech 3.5–5 s while the system is silent.
        for i in sr as usize * 7 / 2..sr as usize * 5 {
            remote[i] = ((i as f32 * 0.031).sin() * 4000.0) as i16;
        }
        let le = |v: &[i16]| v.iter().flat_map(|s| s.to_le_bytes()).collect::<Vec<u8>>();
        let out = suppress_echo_s16le(&le(&remote), &le(&tts), sr);
        let rms = |b: &[u8], a: usize, z: usize| -> f32 {
            let s: Vec<f32> = b[a * 2..z * 2]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32)
                .collect();
            (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
        };
        let echo_in = rms(&le(&remote), sr as usize, sr as usize * 2);
        let echo_out = rms(&out, sr as usize, sr as usize * 2);
        assert!(echo_out < echo_in / 30.0, "echo {echo_in} -> {echo_out}");
        let caller_in = rms(&le(&remote), sr as usize * 4, sr as usize * 9 / 2);
        let caller_out = rms(&out, sr as usize * 4, sr as usize * 9 / 2);
        assert!((caller_out - caller_in).abs() < caller_in * 0.02, "caller {caller_in} -> {caller_out}");
        assert_eq!(out.len(), remote.len() * 2);
    }

    #[test]
    fn twin_serial_detection() {
        assert!(is_twin_serial("pixel-1010111"));
        assert!(!is_twin_serial("pixel-10111"));
        assert!(!is_twin_serial("pixel-10103"));
    }

    #[test]
    fn peak_normalize_boosts_quiet_speech() {
        // Quiet samples around ±500.
        let mut pcm = Vec::new();
        for _ in 0..100 {
            pcm.extend_from_slice(&500i16.to_le_bytes());
            pcm.extend_from_slice(&(-400i16).to_le_bytes());
        }
        let g = peak_normalize_s16le(&mut pcm, 0.72);
        assert!(g > 2.0, "expected boost, gain={g}");
        let peak = pcm
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
            .max()
            .unwrap_or(0);
        // The gain is capped at 12×: 500 → 6000.
        assert!(peak >= 5_500, "peak after norm={peak}");
    }

    #[test]
    fn prepare_voicemail_trims_beep_and_boosts_speech() {
        let sr = 16000u32;
        let mut pcm = Vec::new();
        let tone_n = sr as usize / 5; // 200ms
        for _ in 0..tone_n {
            pcm.extend_from_slice(&10000i16.to_le_bytes());
        }
        let speech_n = sr as usize * 3 / 10; // 300ms
        for _ in 0..speech_n {
            pcm.extend_from_slice(&400i16.to_le_bytes());
        }
        let (out, trim_ms, gain) = prepare_voicemail_pcm(&pcm, sr);
        assert!(trim_ms >= 150, "trim_ms={trim_ms}");
        assert!(gain > 2.0, "gain={gain}");
        let peak = out
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(peak > 1_500, "boosted peak={peak}");
    }

    #[tokio::test]
    async fn notes_round_trip_and_summary_on_row() {
        let dir = std::env::temp_dir().join(format!("pixel-call-notes-{}", Uuid::new_v4()));
        let store = CallStore::new(dir.clone(), true, false, 7);
        let call = store
            .start("pixel-1", "0911111111", 16000, "active")
            .await
            .unwrap();
        let id = call.id.clone();
        let rec = store.finalize(call, "ended").await;
        assert!(rec.summary.is_none());
        assert!(store.load_notes(&id).await.is_none());
        let notes = CallNotes {
            summary: "對方：林專員（銀行）\n事情：推銷信貸".into(),
            dialogue: "接聽：喂？\n對方：您好，我是林專員".into(),
            source: "deepseek".into(),
            ..Default::default()
        };
        store.save_notes(&id, &notes).await.unwrap();
        let row = store.get(&id).await.unwrap();
        assert_eq!(row.summary.as_deref(), Some(notes.summary.as_str()));
        let back = store.load_notes(&id).await.unwrap();
        assert_eq!(back.dialogue, notes.dialogue);
        assert_eq!(back.source, "deepseek");
        // Search hits the summary.
        assert!(call_matches(
            &row,
            &CallListFilter { q: Some("信貸".into()), ..Default::default() },
            None
        ));
        assert!(store.save_notes("no-such-call", &notes).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_record_carries_what_the_caller_agreed_to() {
        let dir = std::env::temp_dir().join(format!("myna-calls-appt-{}", Uuid::new_v4()));
        let store = CallStore::new(dir.clone(), true, false, 7);
        let mut call = store.start("pixel-1", "0911111111", 16000, "active").await.unwrap();
        let book = crate::appointments::CallAppointments::new();
        call.set_appointments(book.clone());
        book.record(crate::appointments::Appointment {
            start: "2026-11-14T15:00".into(),
            end: "2026-11-14T16:00".into(),
            title: "看房".into(),
            mode: "confirm".into(),
            status: crate::appointments::PENDING.into(),
            ..Default::default()
        });
        let rec = store.finalize(call, "ended").await;
        assert_eq!(rec.appointment.as_ref().map(|a| a.title.as_str()), Some("看房"));
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(json["appointment"]["status"], "pending");
        // A call without one has no key at all.
        let plain = store.start("pixel-1", "0922222222", 16000, "active").await.unwrap();
        let rec = store.finalize(plain, "ended").await;
        assert!(serde_json::to_value(&rec).unwrap().get("appointment").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn call_matches_status_caller_and_transcript() {
        let rec = CallRecord {
            id: "abc".into(),
            started_at: "2026-08-03T01:00:00Z".into(),
            ended_at: None,
            serial: "pixel-10144".into(),
            caller_id: "+886912345678".into(),
            duration_ms: 1000,
            status: "blocked_spam".into(),
            audio_path: None,
            tts_audio_path: None,
            ai_audio_path: None,
            remote_audio_path: None,
            uplink_bytes: 0,
            tts_bytes: 0,
            sample_rate: 16000,
            events: vec!["ringing".into()],
            voicemail_path: Some("2026-08-03/abc_vm.wav".into()),
            vm_duration_ms: 5000,
            reply_method: None,
            reply_contact: None,
            contact_verified: false,
            summary: None,
            echo_free: None,
            appointment: None,
        };
        let mut transcripts = std::collections::HashMap::new();
        transcripts.insert("abc".into(), "請回撥明天開會".into());

        assert!(call_matches(
            &rec,
            &CallListFilter {
                status: Some("blocked_spam".into()),
                ..Default::default()
            },
            None
        ));
        assert!(!call_matches(
            &rec,
            &CallListFilter {
                status: Some("ended".into()),
                ..Default::default()
            },
            None
        ));
        assert!(call_matches(
            &rec,
            &CallListFilter {
                caller: Some("912345".into()),
                ..Default::default()
            },
            None
        ));
        assert!(call_matches(
            &rec,
            &CallListFilter {
                q: Some("開會".into()),
                ..Default::default()
            },
            Some(&transcripts)
        ));
        assert!(call_matches(
            &rec,
            &CallListFilter {
                has_voicemail: Some(true),
                serial: Some("10144".into()),
                ..Default::default()
            },
            None
        ));
        assert!(call_matches(
            &rec,
            &CallListFilter {
                has_transcript: Some(true),
                ..Default::default()
            },
            Some(&transcripts)
        ));
        assert!(!call_matches(
            &rec,
            &CallListFilter {
                has_transcript: Some(true),
                ..Default::default()
            },
            None
        ));
        assert!(call_matches(
            &rec,
            &CallListFilter {
                has_transcript: Some(false),
                ..Default::default()
            },
            None
        ));
    }
}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Runtime-editable settings: env defaults + JSON overrides, hot-swappable.
//!
//! Merge order: `config::Settings::from_env()` (compose env) is always the
//! base. The overrides JSON stores ONLY keys the user changed in the web UI,
//! so untouched keys keep following env defaults across redeploys.
//!
//! Persistence: overrides live at `{CALLS_DATA_DIR}/config/settings.json`.
//! The compose file mounts only the `calls_data` named volume at `/data/calls`,
//! so this is the one /data path that survives container recreation
//! (a bare `/data/config` would be wiped by `compose up --force-recreate`).
//!
//! Secrets API contract (`masked_json` / `apply_patch`):
//! - GET returns a masked value ("abc…wxyz" or "••••") plus `<field>_set`.
//! - GET also includes `gmail_configured` (fulfill readiness; no secrets).
//! - PUT with the secret field absent or "" keeps the stored value.
//! - PUT with `<field>_clear: true` clears it (stored as an empty override).
//!
//! Keys a settings.json saved by an older agent may still hold and this one no
//! longer knows (the Google Calendar credentials and `calendar_auto_add`, gone
//! with the Google Calendar connection) are skipped when it is read and left
//! out the next time it is written.
//!
//! Hot-swap: the merged snapshot is an `Arc<RuntimeSettings>` behind a
//! `std::sync::RwLock`. Call sessions grab one snapshot at call start; HTTP
//! handlers read the current one per request. No extra crates needed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tracing::{info, warn};

use crate::config::{env_bool, env_or, env_parse, Settings};

pub const DEFAULT_SHAMING_TEMPLATE: &str = "提醒您，本次通話共消耗了大約 {watts} 瓦電力、{water} 公升冷卻水與 {co2} 公克碳排放，感謝您對地球資源的貢獻，再見。";

/// Snapshot of everything editable from the web console (`/calls` settings tab).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeSettings {
    // AI / OpenRouter
    pub openrouter_api_key: String,
    pub openrouter_model: String,
    /// DTMF 1 → live LLM conversation instead of hold music (needs key too).
    pub ai_live_enabled: bool,
    // Notify (Telegram / Apprise gateway)
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
    pub notify_gateway_url: String,
    pub notify_gateway_tag: String,
    pub call_notify: bool,
    // Email (Gmail) — for task fulfillment
    pub gmail_user: String,
    pub gmail_app_password: String,
    pub gmail_from_name: String,
    // OpenClaw — for task fulfillment
    pub openclaw_url: String,
    pub openclaw_token: String,
    // Resource-shaming announcement before agent-initiated goodbyes
    pub shaming_enabled: bool,
    pub shaming_multiplier: f64,
    pub shaming_template: String,
    // Spam handling (inbound deny removed — fields kept for UI compat only)
    /// Numbers or prefixes (legacy storage; `blocklist_hit` always false).
    pub spam_blocklist: Vec<String>,
    /// Keyword spam filter on menu/VM speech; LLM classify when OpenRouter key set.
    pub spam_ai_filter_enabled: bool,
    /// Custom spam keywords; empty = use built-in ZH defaults in `extract::SPAM_KEYWORDS_ZH`.
    pub spam_keywords: Vec<String>,
    /// Outbound callback allowlist (confirm→dial). Exact or `*` prefix.
    /// Empty = fail-closed (no arbitrary dial). Not the retired inbound spam list.
    pub callback_whitelist: Vec<String>,
    // Voicemail tuning
    pub vm_max_ms: u32,
    pub vm_silence_ms: u32,
    pub vm_silence_rms: f32,
    pub vm_confirm_wait_ms: u32,
    // IVR timing
    pub ivr_start_delay_ms: u32,
    /// After period greets fully done, silence before AI unmute (default 2000).
    pub post_greet_ai_delay_ms: u32,
    /// After the AI's goodbye, wait then hangup (default 2000).
    pub goodbye_hangup_delay_ms: u32,
    pub ivr_reprompt_ms: u32,
    pub ivr_max_menu_plays: u32,
    /// Piper fallbacks when optional WAVs (`ivr_07` / `ivr_08`) are missing.
    pub ivr_goodbye_text: String,
    pub ivr_vm_intro_text: String,
    pub ivr_vm_confirm_text: String,
    pub vm_reply_ack_text: String,
    // Tencent TRTC (menu DTMF-1 AI) — hot-swap via UI; env remains base default
    pub trtc_sdk_app_id: u32,
    pub trtc_secret_key: String,
    pub trtc_secret_id: String,
    /// CAM Cloud API SecretKey (TC3) — distinct from SDK UserSig `trtc_secret_key`.
    pub trtc_cloud_secret_key: String,
    pub trtc_ai_enabled: bool,
    pub trtc_usersig_expire_secs: u32,
    pub trtc_ai_region: String,
    pub trtc_api_host: String,
    pub trtc_api_intl: bool,
    pub trtc_llm_config: String,
    pub trtc_tts_config: String,
    pub trtc_stt_language: String,
    pub trtc_ai_max_idle_secs: u32,
    pub trtc_bridge_enabled: bool,
    pub trtc_bridge_url: String,
    // Google Gemini Live (DTMF-1 realtime voice) — fill key in /calls/ settings UI
    pub gemini_live_enabled: bool,
    pub gemini_api_key: String,
    pub gemini_live_model: String,
    pub gemini_system_prompt: String,
    /// Gemini Live prebuilt voice name (Aoede, Kore, Puck, Charon, Fenrir, Zephyr, …).
    pub gemini_voice_name: String,
    /// Key of the service that writes the post-call summary when
    /// summary_provider is custom (not live talk).
    pub summary_api_key: String,
    /// The summary key (or a TRTC LLM config with a key) comes from
    /// settings.json, not from the environment.
    #[serde(skip)]
    pub summary_key_saved: bool,
    /// Desk note spoken in the AI opening (next call).
    pub owner_status_enabled: bool,
    pub owner_status_text: String,
    /// Persona mode: Gemini answers as the owner — no greeting WAV / post-greet
    /// wait / goodbye WAV; a natural closing ends the call (no fixed clock, only
    /// the caps `persona_normal_max_min` / `persona_stall_max_min`).
    #[serde(default)]
    pub gemini_persona_mode: bool,
    /// Custom prompt for "As me" (`answer_style = "persona"`); empty = built-in.
    #[serde(default)]
    pub gemini_persona_prompt: String,
    /// Custom prompt for "AI assistant" (`answer_style = "assistant"`); empty = built-in.
    #[serde(default)]
    pub gemini_assistant_prompt: String,
    /// Answer hidden numbers too: hidden / private / empty caller ID takes the
    /// normal AI path instead of the reject WAV + hangup.
    #[serde(default)]
    pub answer_unknown_callers: bool,
    /// Blocked numbers saved by older versions. They are copied onto the
    /// Contacts Blacklist at start ([`crate::contacts::absorb_reject_numbers`])
    /// and kept here as they were, for an older version to read; until then
    /// they are turned away like the Blacklist ([`RuntimeSettings::pending_rejects`]).
    /// The settings API no longer writes it: numbers sent in it go onto the
    /// Blacklist.
    #[serde(default)]
    pub reject_numbers: Vec<RejectEntry>,
    /// `reject_numbers` is on the Blacklist: from then on only the Blacklist counts.
    #[serde(default)]
    pub reject_numbers_moved: bool,
    /// Voice of the blocked-number announcement; empty = the AI's voice.
    #[serde(default)]
    pub reject_voice_name: String,
    /// What a blocked number gets: `decline` (default) = the phone refuses the
    /// ringing call, nothing is played; `announce` = picked up, the "cannot be
    /// reached" announcement, hung up. A phone app that cannot decline announces.
    #[serde(default = "default_reject_mode")]
    pub reject_mode: String,
    /// Sales / scam stalling: in persona mode, once the caller is
    /// clearly selling (insurance / loans / investment / promos) or scamming, the
    /// AI keeps them talking about every term of the offer — never giving data
    /// or agreeing to anything.
    #[serde(default = "default_true")]
    pub persona_stall_spam: bool,
    /// Look at calls no keyword flagged once the caller has said what they want
    /// (a cheap Gemini model, at most three times a call): catches scams in
    /// words no keyword list has. Off: only keywords start the second look.
    #[serde(default = "default_true")]
    pub scam_review_without_keywords: bool,
    /// Cap of a persona-mode call judged sales / scam (the stall cap), minutes
    /// (clamped 3–30; recording caps at 30). Normal calls have their own cap,
    /// `persona_normal_max_min`.
    #[serde(default = "default_stall_max_min")]
    pub persona_stall_max_min: u32,
    /// Cap of a normal persona-mode call, minutes (clamped 3–30). Normal calls
    /// should wrap up around 4½ minutes → default 5.
    #[serde(default = "default_normal_max_min")]
    pub persona_normal_max_min: u32,
    /// Days the records are kept (clamped 1–365): calls and their recordings,
    /// text messages, AI errands and to-do items older than this are deleted.
    /// Default: CALLS_RETENTION_DAYS, else 15.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    /// Model a call switches to once judged sales / scam (its stall session);
    /// empty = keep the call's model. E.g. normal calls on
    /// `gemini-3.8-live-extended-thinking`, stalling on `gemini-3.8-live`.
    #[serde(default = "default_stall_model")]
    pub gemini_stall_model: String,
    /// Let Gemini decide when the caller has finished: Gemini Live's own voice
    /// activity detection decides when the caller's turn ends (the line streams
    /// continuously). Off = the local RMS VAD sends activityStart/End (old path).
    #[serde(default = "default_true")]
    pub gemini_server_vad: bool,
    /// Barge-in: on phones whose capture does not carry our own voice back
    /// (e.g. Pixel 9) the caller can cut in and our playback stops.
    #[serde(default = "default_true")]
    pub gemini_barge_in: bool,
    /// Ring once before answering: the phone lets an incoming call ring
    /// this long before it answers (agent `answer_plan` reply to `incoming`), ms.
    #[serde(default = "default_answer_ring_ms")]
    pub answer_ring_ms: u32,
    /// Persona mode: while the phone rings, the Gemini session is already connected and
    /// its 「喂？」 generated, so the AI speaks right after pickup.
    #[serde(default = "default_true")]
    pub gemini_ring_prewarm: bool,
    /// Background sound (ambience) under the AI's voice: `off` or one of
    /// [`crate::ambience::KINDS`] (`office` / `home` / `street` / `hospital` / …).
    #[serde(default = "default_ambience_kind")]
    pub ambience_kind: String,
    /// Background sound level 1 low / 2 medium / 3 high.
    #[serde(default = "default_ambience_level")]
    pub ambience_level: u32,
    /// Thinking level `low` / `medium` / `high` for Live models that require one
    /// (`gemini-3.8-live-extended-thinking`); ignored by the others.
    #[serde(default = "default_thinking_level")]
    pub gemini_thinking_level: String,
    /// The language the AI speaks with callers: its prompt, its first words, the
    /// blocked-number announcement, the texts it sends callers
    /// (`en`, `zh-TW`, `zh-CN`, `ja`, `ko`, `es`, `fr`, `de`, `pt`).
    #[serde(default = "default_language")]
    pub language: String,
    /// The language of what the owner reads: notifications, the post-call
    /// summary (also in the web console's call list), appointment cards and
    /// their results, the SMS forwarder's messages. Empty = `language`;
    /// otherwise one of its codes.
    #[serde(default)]
    pub notify_language: String,
    /// The main language of each SIM's country: SIM country (ISO 3166 alpha-2,
    /// lowercase, as the phone reports it) → language code (`{"us": "en"}`). A call
    /// that comes in on that SIM opens in it, unless the caller's number is from
    /// another country whose language the AI speaks
    /// ([`crate::gemini_live::call_lang_plan`]). Empty = `language` for every SIM.
    #[serde(default)]
    pub sim_languages: std::collections::BTreeMap<String, String>,
    /// The owner's name as callers say it (`{owner}` in prompts).
    #[serde(default)]
    pub owner_name: String,
    /// Other names callers use for the owner (an English name, a nickname): the
    /// AI knows they mean the owner. Stored by [`clean_owner_aliases`].
    #[serde(default)]
    pub owner_aliases: Vec<String>,
    /// `assistant` = the AI says it is the owner's AI assistant and takes a
    /// message; `persona` = it answers as the owner.
    #[serde(default = "default_answer_style")]
    pub answer_style: String,
    /// Facts the AI may answer from (opening hours, address, what to tell
    /// couriers…). Anything not written here it must not make up.
    #[serde(default)]
    pub knowledge_notes: String,
    /// Switch to the caller's language when it is not the agent's.
    /// On unless the owner turned it off.
    #[serde(default = "default_reply_in_caller_language")]
    pub reply_in_caller_language: bool,
    /// Country calling code of the phone's SIM, digits only (`886`, `1`, `81`):
    /// how blocked numbers written with `+…` match the national form.
    #[serde(default = "default_country_code")]
    pub country_code: String,
    /// OpenAI-compatible chat endpoint for the post-call summary when
    /// `summary_provider` is `custom`, e.g. `https://api.openai.com/v1`.
    /// Empty = Gemini with the Live API key, else OpenRouter.
    #[serde(default)]
    pub summary_api_base: String,
    /// Model for the post-call summary; empty = the endpoint's default.
    #[serde(default)]
    pub summary_model: String,
    /// Which service writes the post-call summary: `gemini` (Gemini's
    /// lowest-cost model with the Live API key) or `custom` (the endpoint
    /// above, else Gemini, else OpenRouter). Gemini without its
    /// key works like `custom` ([`crate::summary::summary_endpoint_for`]).
    /// Older values are turned into one of the two when the settings load.
    #[serde(default = "default_auto")]
    pub summary_provider: String,
    /// Where notifications go (Telegram, Discord, ntfy, …), all of them at once.
    #[serde(default)]
    pub notify_channels: Vec<crate::notify::Channel>,
    /// Address of this web console as the owner reaches it, for the links in
    /// notifications (`http://192.168.1.20:8787`). Empty = automatic, see
    /// [`effective_console_url`].
    #[serde(default)]
    pub public_url: String,
    /// Name this computer answers to on the LAN: `<name>.local` (mDNS).
    /// Empty = off.
    #[serde(default = "default_mdns_name")]
    pub mdns_name: String,
    /// Phones may connect over Wi-Fi: a phone plugged in by USB once gets
    /// the Wi-Fi addresses with the pairing key ([`crate::phone_wifi`]) and
    /// connects there when unplugged. Off: the Wi-Fi entry closes (no restart)
    /// and the phones on Wi-Fi are dropped.
    #[serde(default = "default_true")]
    pub phone_wifi: bool,
    /// When a call fixes a date (delivery, appointment, callback time), add an
    /// "add to calendar" link to the summary. One more request to the summary
    /// model per call.
    #[serde(default = "default_true")]
    pub calendar_suggest: bool,
    /// The calendar "Add to calendar" opens ([`crate::calendar::APPS`]): in
    /// the web console and in notifications. Empty = not chosen yet (the web
    /// console asks on the first press).
    #[serde(default)]
    pub calendar_app: String,
    /// Appointments on a call ([`crate::appointments`]): `off` (default) or
    /// `confirm` (the AI says it will check and get back to the caller; the
    /// owner accepts or declines on the web console or a Telegram card). An
    /// `auto` saved by an older agent reads as `confirm`.
    #[serde(default = "default_appointment_mode")]
    pub appointment_mode: String,
    /// After the owner accepts or declines, text the caller through the phone
    /// that took the call (numbers that take texts only).
    #[serde(default)]
    pub appointment_sms: bool,
    /// How the owner's answer reaches a caller when the owner does not pick a
    /// way ([`crate::appointments::default_reply`]): `auto` (default: a text to
    /// a mobile, the AI calls back a landline, nobody for a hidden number),
    /// `sms` or `call`. Also what the AI promises on the call.
    #[serde(default = "default_auto")]
    pub appointment_reply: String,
    /// Who the AI is on the calls it makes for the owner ([`crate::errands`]):
    /// `assistant` (the owner's assistant; default) or `owner` (the owner
    /// in person). Each errand may pick its own.
    #[serde(default = "default_errand_identity")]
    pub errand_identity: String,
    /// Chat commands from the environment's Telegram bot, in its chat
    /// (TELEGRAM_CHAT_ID; [crate::control]). Off by default.
    #[serde(default)]
    pub telegram_control: bool,
    /// Telegram user ids that may give commands there; empty = anyone in that chat.
    #[serde(default)]
    pub telegram_control_users: Vec<String>,
    /// AI calls one chat may start per hour (0 = none; at most 30).
    #[serde(default = "default_control_errands_per_hour")]
    pub control_errands_per_hour: u32,
    /// Messages one chat may send per minute; past it the agent stops
    /// answering that chat for the rest of the minute (1–120).
    #[serde(default = "default_control_commands_per_minute")]
    pub control_commands_per_minute: u32,
    /// The MCP tool interface at `POST /mcp` ([crate::mcp]). Off by default;
    /// it answers nothing until `mcp_token` is set too.
    #[serde(default)]
    pub mcp_enabled: bool,
    /// The bearer token an MCP client sends (`Authorization: Bearer …`).
    #[serde(default)]
    pub mcp_token: String,
    /// What MCP clients may do about calls: `off` (look only), `confirm`
    /// (the AI calls once the owner says yes in a command chat) or `direct`.
    #[serde(default = "default_mcp_calls")]
    pub mcp_calls: String,
    /// AI calls MCP clients may ask for per hour, all together (0 = none; at most 30).
    #[serde(default = "default_mcp_errands_per_hour")]
    pub mcp_errands_per_hour: u32,
    /// Browser origins allowed to reach `/mcp` (exact, `https://app.example`).
    /// A request without an Origin header (a local client) is not a browser's.
    #[serde(default)]
    pub mcp_allowed_origins: Vec<String>,
    /// Names speech-to-text keeps getting wrong, one `wrong=right` per entry
    /// (`黃先生=王先生`): applied to transcripts and summaries.
    #[serde(default)]
    pub name_corrections: Vec<String>,
    /// Your own names for phones: the id a phone reports (`pixel-10123`) → the
    /// label shown in notifications and the web console. Wins over the model
    /// the phone reports itself.
    #[serde(default)]
    pub device_labels: std::collections::BTreeMap<String, String>,
    /// What to do when a Whitelist contact (`kind == "whitelist"`) calls:
    /// `off` (normal AI answering with known-contact prompt), `direct` (decline
    /// on the AI phone so carrier conditional call forwarding rings the owner's
    /// real number immediately, or bridge/notify), or `emergency` (AI answers first
    /// and transfers / alerts the owner's real number only when the caller has an
    /// urgent matter).
    #[serde(default = "default_whitelist_mode")]
    pub whitelist_mode: String,
    /// The owner's real phone number for Whitelist forwarding / emergency call.
    #[serde(default)]
    pub whitelist_forward_number: String,
    /// Old switch for the urgent call. Kept so older settings files and backups still
    /// load; it no longer changes anything. With `whitelist_mode` emergency and a
    /// forwarding number, an urgent Whitelist caller is put on hold, the phone dials
    /// that number from the same SIM and merges the two calls (`crate::urgent_call`);
    /// without the number only an alert goes out.
    #[serde(default)]
    pub whitelist_urgent_call: bool,
    /// Who may be forwarded to the owner (`whitelist_mode` direct or emergency):
    /// Whitelist contacts (on by default, as before these switches), other
    /// contacts, and numbers not in the contacts (hidden ones too). Blacklist
    /// and stall-list contacts never are (`crate::urgent_call::transfer_target`).
    #[serde(default = "default_true")]
    pub transfer_whitelist: bool,
    #[serde(default)]
    pub transfer_contacts: bool,
    #[serde(default)]
    pub transfer_others: bool,
    /// Password for accessing the web console (`?token=` or `Authorization: Bearer`);
    /// overrides `CALLS_UI_TOKEN` when non-empty.
    #[serde(default)]
    pub ui_password: String,
    /// Whether the first-time password setup prompt has already been shown/dismissed.
    #[serde(default)]
    pub ui_password_prompted: bool,
    /// Automatic backups: `off`, `daily`, `weekly` or `monthly` ([`crate::backup`]).
    #[serde(default = "default_backup_schedule")]
    pub backup_schedule: String,
    /// Folder backups are saved in (absolute path); empty = `backups` in the
    /// data folder.
    #[serde(default)]
    pub backup_dir: String,
    /// Automatic backups kept (1–100); the oldest go first.
    #[serde(default = "default_backup_keep")]
    pub backup_keep: u32,
    /// Backups ("Back up now" and automatic ones) also hold the call records,
    /// recordings, text messages and to-do items.
    #[serde(default)]
    pub backup_include_calls: bool,
    /// Backups ("Back up now", automatic ones, the copy made before an import)
    /// are encrypted with `backup_password` ([`crate::backup_crypt`]). Never
    /// on without a password.
    #[serde(default)]
    pub backup_encrypt: bool,
    /// The password backups are encrypted with (at least 8 characters).
    #[serde(default)]
    pub backup_password: String,
    /// Where calls, recordings and settings are kept (`CALLS_DATA_DIR`, from the
    /// env base). Not a setting: never shown or saved.
    #[serde(skip)]
    pub calls_data_dir: String,
}

pub const STALL_MAX_MIN_DEFAULT: u32 = 30;
pub const NORMAL_MAX_MIN_DEFAULT: u32 = 5;
const STALL_MAX_MIN_FLOOR: u32 = 3;
const STALL_MAX_MIN_CEIL: u32 = 30;
pub const RETENTION_DAYS_DEFAULT: u32 = 15;
pub const RETENTION_DAYS_MIN: u32 = 1;
pub const RETENTION_DAYS_MAX: u32 = 365;
pub const STALL_MODEL_DEFAULT: &str = "gemini-3.8-live";
/// About one ring (Taiwan ringback: 1 s tone, 2 s pause).
pub const ANSWER_RING_MS_DEFAULT: u32 = 3_000;
const ANSWER_RING_MS_CEIL: u32 = 8_000;

fn default_true() -> bool {
    true
}

fn default_stall_max_min() -> u32 {
    STALL_MAX_MIN_DEFAULT
}

fn default_normal_max_min() -> u32 {
    NORMAL_MAX_MIN_DEFAULT
}

fn default_retention_days() -> u32 {
    RETENTION_DAYS_DEFAULT
}

fn default_stall_model() -> String {
    STALL_MODEL_DEFAULT.to_string()
}

fn default_answer_ring_ms() -> u32 {
    ANSWER_RING_MS_DEFAULT
}

fn default_ambience_kind() -> String {
    "off".to_string()
}

fn default_ambience_level() -> u32 {
    2
}

fn default_thinking_level() -> String {
    "low".to_string()
}

fn default_language() -> String {
    "en".to_string()
}

fn default_answer_style() -> String {
    "assistant".to_string()
}

fn default_country_code() -> String {
    String::new()
}

/// A caller who speaks another language is answered in it: on unless the owner
/// turned it off (the AI did so on every call before the setting existed).
fn default_reply_in_caller_language() -> bool {
    true
}

fn default_auto() -> String {
    "auto".to_string()
}

fn default_backup_schedule() -> String {
    "weekly".to_string()
}

fn default_backup_keep() -> u32 {
    10
}

/// `off` / `daily` / `weekly` / `monthly`; anything else is the default, weekly.
pub fn sanitize_backup_schedule(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        v @ ("off" | "daily" | "weekly" | "monthly") => v.to_string(),
        _ => default_backup_schedule(),
    }
}

/// A backup folder: trimmed, and empty (the default place) unless it is an
/// absolute path.
fn sanitize_backup_dir(s: &str) -> String {
    let t = s.trim();
    if Path::new(t).is_absolute() {
        t.to_string()
    } else {
        String::new()
    }
}

fn default_mdns_name() -> String {
    "bageo".to_string()
}

fn default_appointment_mode() -> String {
    "off".to_string()
}

fn default_errand_identity() -> String {
    "assistant".to_string()
}

fn default_control_errands_per_hour() -> u32 {
    5
}

fn default_control_commands_per_minute() -> u32 {
    20
}

fn default_mcp_calls() -> String {
    "off".to_string()
}

fn default_mcp_errands_per_hour() -> u32 {
    3
}

/// Setting `mcp_calls`: `confirm` or `direct`, else `off`.
pub fn sanitize_mcp_calls(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "confirm" => "confirm".to_string(),
        "direct" => "direct".to_string(),
        _ => default_mcp_calls(),
    }
}

/// Setting `mcp_allowed_origins`: `scheme://host[:port]` each, without a
/// path or a trailing slash; at most 20, each once.
pub fn sanitize_origins(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for o in list.iter().flat_map(|s| s.split([',', ' ', '\n', ';'])) {
        let o = o.trim().trim_end_matches('/').to_ascii_lowercase();
        let Some((scheme, host)) = o.split_once("://") else { continue };
        if !matches!(scheme, "http" | "https") || host.is_empty() || host.contains(['/', '?', '#', '@']) {
            continue;
        }
        if !out.contains(&o) && out.len() < 20 {
            out.push(o);
        }
    }
    out
}

/// Who may give chat commands: platform user ids (letters, digits and
/// _ @ . : + -), separated by commas, spaces or lines; at most 20, each once.
pub fn sanitize_sender_ids(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in list.iter().flat_map(|s| s.split([',', ' ', '\n', ';'])) {
        let id = id.trim();
        // Kept as written even when it is no id (a name): it then matches no
        // sender, so the list still narrows the chat. Dropping it would leave
        // the list empty, and an empty list lets everyone in the chat.
        let ok = !id.is_empty() && id.chars().count() <= 64 && !id.chars().any(char::is_control);
        if !ok || out.iter().any(|o| o == id) {
            continue;
        }
        out.push(id.to_string());
        if out.len() == 20 {
            break;
        }
    }
    out
}

/// Setting `errand_identity`: `owner`, else `assistant`.
pub fn sanitize_errand_identity(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "owner" => "owner".to_string(),
        _ => default_errand_identity(),
    }
}

fn default_reject_mode() -> String {
    "decline".to_string()
}

fn default_whitelist_mode() -> String {
    "off".to_string()
}

/// `off` / `direct` / `emergency`; anything else is `off`.
pub fn sanitize_whitelist_mode(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "direct" => "direct".to_string(),
        "emergency" | "urgent" => "emergency".to_string(),
        _ => default_whitelist_mode(),
    }
}

/// `decline` / `announce`; anything else is decline.
fn sanitize_reject_mode(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "announce" => "announce".to_string(),
        _ => default_reject_mode(),
    }
}

/// `off` / `confirm` (an old `auto` is `confirm`); anything else is off.
fn sanitize_appointment_mode(s: &str) -> String {
    crate::appointments::Mode::parse(s).as_str().to_string()
}

fn sanitize_language(s: &str) -> String {
    crate::locale::Lang::parse(s).code().to_string()
}

/// One of the language codes (any case, `_` for `-`); anything else is empty
/// (= the AI's language).
fn sanitize_notify_language(s: &str) -> String {
    let t = s.trim().replace('_', "-");
    crate::locale::ALL
        .iter()
        .find(|l| l.code().eq_ignore_ascii_case(&t))
        .map(|l| l.code().to_string())
        .unwrap_or_default()
}

/// Most SIM countries kept in `sim_languages`.
const SIM_LANGUAGES_MAX: usize = 50;

/// `sim_languages` as stored: keys two letters (lowercased), values one of the
/// language codes ([`sanitize_notify_language`]); anything else is dropped.
fn sanitize_sim_languages<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> std::collections::BTreeMap<String, String> {
    entries
        .into_iter()
        .filter_map(|(country, lang)| {
            let country = country.trim().to_ascii_lowercase();
            let lang = sanitize_notify_language(lang);
            (country.len() == 2 && country.bytes().all(|b| b.is_ascii_lowercase()) && !lang.is_empty())
                .then_some((country, lang))
        })
        .take(SIM_LANGUAGES_MAX)
        .collect()
}

/// `gemini` / `custom`, or a value from before there were only these two
/// (resolved when the settings load): the old name of `custom`, else `auto`.
fn sanitize_summary_provider(s: &str) -> String {
    let v = s.trim().to_ascii_lowercase();
    match v.as_str() {
        // Reading settings of older versions: "deepseek" was "custom".
        "gemini" | "custom" | "deepseek" => v,
        _ => default_auto(),
    }
}

// Reading settings of older versions: there, "another service" with a key and
// no address went to DeepSeek. That address and model are written down once
// ([fill_old_summary_service]), so nothing else needs to know.
const OLD_SUMMARY_API_BASE: &str = "https://api.deepseek.com/v1";
const OLD_SUMMARY_MODEL: &str = "deepseek-chat";

/// For a setup from an older version: "another service" (custom) with a key and
/// no address gets the address (and, when none is set, the model) its
/// summaries went to. True when it changed something.
fn fill_old_summary_service(provider: &str, has_key: bool, base: &mut String, model: &mut String) -> bool {
    if provider != "custom" || !has_key || !base.trim().is_empty() {
        return false;
    }
    *base = OLD_SUMMARY_API_BASE.to_string();
    if model.trim().is_empty() {
        *model = OLD_SUMMARY_MODEL.to_string();
    }
    true
}

/// settings.json as it is read: the summary key of older versions moves to its
/// new name. True when the file had it.
/// [`SettingsStore::language_chosen`] for a settings file and the environment (`env_set`).
fn language_chosen_in(o: &Overrides, env_set: impl Fn(&str) -> bool) -> bool {
    o.language.is_some() || o.notify_language.is_some() || env_set("APP_LANGUAGE") || env_set("NOTIFY_LANGUAGE")
}

fn read_overrides(text: &str) -> serde_json::Result<(Overrides, bool)> {
    let mut v: Value = serde_json::from_str(text)?;
    let mut moved = false;
    if let Some(m) = v.as_object_mut() {
        // Reading settings of older versions: the summary key was "deepseek_api_key".
        if let Some(old) = m.remove("deepseek_api_key") {
            moved = true;
            let unset = m.get("summary_api_key").and_then(Value::as_str).map_or(true, |s| s.trim().is_empty());
            if unset {
                m.insert("summary_api_key".into(), old);
            }
        }
    }
    Ok((serde_json::from_value(v)?, moved))
}

/// Write settings.json whole: to a temporary file, then renamed over it.
fn write_overrides(path: &Path, ov: &Overrides) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let data = serde_json::to_vec_pretty(ov)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &data).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

/// The credentials among the settings: one half of the backup item `keys`
/// ([`crate::backup`]). Every field the settings page masks ([`SECRET_FIELDS`])
/// and the old name of the summary key.
pub const CREDENTIAL_FIELDS: [&str; 15] = [
    "openrouter_api_key",
    "telegram_bot_token",
    "gmail_app_password",
    "openclaw_token",
    "mcp_token",
    "ui_password",
    "trtc_secret_key",
    "trtc_secret_id",
    "trtc_cloud_secret_key",
    "trtc_llm_config",
    "trtc_tts_config",
    "gemini_api_key",
    "summary_api_key",
    "deepseek_api_key",
    "backup_password",
];

/// Where data goes and who may operate Bageo from outside: the other half of
/// the backup item `keys` ("keys, passwords and connections"). The
/// notification channels whole (where they send, who may give commands there),
/// the Telegram chat and remote control, the notification gateway, the mail
/// account, the services summaries and calls are sent to, the owner's number
/// calls are forwarded to and whether an urgent caller is dialled through to
/// it, the numbers Bageo may call back, and what AI tools may do over MCP.
pub const CONNECTION_FIELDS: [&str; 25] = [
    "notify_channels",
    "notify_gateway_url",
    "notify_gateway_tag",
    "telegram_chat_id",
    "telegram_control",
    "telegram_control_users",
    "control_errands_per_hour",
    "control_commands_per_minute",
    "gmail_user",
    "openclaw_url",
    "summary_provider",
    "summary_api_base",
    "summary_model",
    "trtc_sdk_app_id",
    "trtc_api_host",
    "trtc_api_intl",
    "trtc_ai_region",
    "trtc_bridge_url",
    "whitelist_forward_number",
    "whitelist_urgent_call",
    "callback_whitelist",
    "mcp_enabled",
    "mcp_calls",
    "mcp_errands_per_hour",
    "mcp_allowed_origins",
];

/// Settings whose names look like a credential or an address but are neither
/// (a test checks every setting against the two lists above and this one):
/// `public_url` is this computer's own address and never leaves it nor comes
/// from a backup; `ui_password_prompted` only says the password hint was shown;
/// `spam_keywords` and `scam_review_without_keywords` are about words callers say.
pub const KEY_LOOKALIKES: [&str; 4] = ["public_url", "ui_password_prompted", "spam_keywords", "scam_review_without_keywords"];

/// Settings that make Bageo delete things on its own: how many days calls,
/// recordings, texts, to-do items and AI calls are kept (`retention_days`; the
/// older `sms_retention_days` is no setting any more) and how many automatic
/// backups (`backup_keep`). A restore only keeps or lengthens them
/// ([`SettingsStore::replace_from_backup`]): a shorter one would delete
/// records at once, and the copy made before the restore does not hold them.
/// Only the console shortens them.
pub const RETENTION_FIELDS: [&str; 2] = ["retention_days", "backup_keep"];

/// A setting of the backup item `keys` ([`CREDENTIAL_FIELDS`], [`CONNECTION_FIELDS`]).
pub fn is_key_field(name: &str) -> bool {
    CREDENTIAL_FIELDS.contains(&name) || CONNECTION_FIELDS.contains(&name)
}

fn key_fields() -> impl Iterator<Item = &'static str> {
    CREDENTIAL_FIELDS.into_iter().chain(CONNECTION_FIELDS)
}

/// A value that says something (not null, not empty).
fn filled(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// A settings file (JSON object) without the item `keys`.
pub fn settings_part(v: &Value) -> Value {
    let mut m = v.as_object().cloned().unwrap_or_default();
    m.retain(|k, _| !is_key_field(k));
    Value::Object(m)
}

/// Only the item `keys` of a settings file.
pub fn keys_part(v: &Value) -> Value {
    let mut m = v.as_object().cloned().unwrap_or_default();
    m.retain(|k, _| is_key_field(k));
    Value::Object(m)
}

/// The settings file holds something of the item `keys` that is set.
pub fn has_keys(v: &Value) -> bool {
    key_fields().any(|f| v.get(f).is_some_and(filled))
}

/// The settings file holds a setting outside the item `keys`.
pub fn has_settings(v: &Value) -> bool {
    settings_part(v).as_object().is_some_and(|m| !m.is_empty())
}

/// The settings file a restore makes ([`crate::backup`]) from this computer's
/// (`cur`) and the backup's (`inc`). `settings`: the backup's settings outside
/// `keys` take the place of these, whole; else these stay. `keys`: a field of
/// the group the backup has set takes the place of this computer's; one it
/// does not have, or leaves empty, stays as it is here, so an older or partial
/// backup never takes away a password or key set since. `whole_keys` (putting
/// back this computer's own `before_import` copy): the group is the backup's
/// exactly, a field it does not have going back to its default here, so that
/// copy takes away every key, channel and remote control the restore before
/// brought. Without `keys` the whole group stays as it is here.
pub fn combine_settings(cur: &Value, inc: &Value, settings: bool, keys: bool, whole_keys: bool) -> Value {
    let mut out = settings_part(if settings { inc } else { cur });
    if let Some(m) = out.as_object_mut() {
        for f in key_fields() {
            let pick = match (keys, whole_keys) {
                (false, _) => cur.get(f),
                (true, true) => inc.get(f),
                (true, false) => inc.get(f).filter(|v| filled(v)).or_else(|| cur.get(f)),
            };
            if let Some(x) = pick {
                m.insert(f.to_string(), x.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod combine_tests {
    use super::*;

    #[test]
    fn keys_of_an_older_backup_keep_what_it_does_not_have() {
        let cur = json!({"owner_name": "Here", "ui_password": "test-fake-pw-here", "gemini_api_key": "test-fake-key-here", "mcp_token": "test-fake-mcp-here"});
        let inc = json!({"owner_name": "There", "telegram_bot_token": "test-fake-bot-there", "gemini_api_key": ""});
        let mixed = combine_settings(&cur, &inc, false, true, false);
        assert_eq!(mixed["ui_password"], "test-fake-pw-here");
        assert_eq!(mixed["gemini_api_key"], "test-fake-key-here", "an empty key in the file does not clear this one");
        assert_eq!(mixed["mcp_token"], "test-fake-mcp-here");
        assert_eq!(mixed["telegram_bot_token"], "test-fake-bot-there");
        assert_eq!(mixed["owner_name"], "Here", "settings not chosen");
        let whole = combine_settings(&cur, &inc, false, true, true);
        assert!(whole.get("ui_password").is_none(), "the before_import copy takes the group exactly");
        assert!(whole.get("mcp_token").is_none());
        assert_eq!(whole["telegram_bot_token"], "test-fake-bot-there");
        let none = combine_settings(&cur, &inc, true, false, false);
        assert_eq!(none["owner_name"], "There");
        assert_eq!(none["ui_password"], "test-fake-pw-here");
        assert!(none.get("telegram_bot_token").is_none());
    }
}

/// [`RETENTION_FIELDS`] of a restore (`out`) never shorter than this
/// computer's (`now`: the settings in use): the backup's (`inc`) only when it
/// is longer, else this computer's own setting (`cur`) stays, missing or not.
fn never_shorter(out: &mut Map<String, Value>, inc: &Value, cur: &Value, now: &Value) {
    for f in RETENTION_FIELDS {
        let longer = match (inc.get(f).and_then(Value::as_u64), now.get(f).and_then(Value::as_u64)) {
            (Some(new), Some(here)) => new > here,
            _ => false,
        };
        if longer {
            continue;
        }
        match cur.get(f) {
            Some(v) => {
                out.insert(f.to_string(), v.clone());
            }
            None => {
                out.remove(f);
            }
        }
    }
}

/// A notification channel as a destination: where it sends and who may give
/// commands there (its id and tokens do not count).
fn destination(field: &str, v: Option<&Value>) -> Value {
    let v = v.cloned().unwrap_or(Value::Null);
    if field != "notify_channels" {
        return v;
    }
    let mut rows: Vec<String> = v
        .as_array()
        .map(|l| {
            l.iter()
                .map(|c| {
                    let pick = |k: &str| c.get(k).cloned().unwrap_or(Value::Null);
                    json!([pick("kind"), pick("url"), pick("target"), pick("enabled"), pick("control"), pick("control_users")]).to_string()
                })
                .collect()
        })
        .unwrap_or_default();
    rows.sort();
    json!(rows)
}

/// What the import preview shows of a settings file ([`crate::backup`]).
#[derive(Debug, Clone, Default)]
pub struct SettingsPeek {
    /// Settings outside the item `keys`.
    pub has_settings: bool,
    /// Something of the item `keys` that is set.
    pub has_keys: bool,
}

/// What restoring the item `keys` of a settings file would change here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeysChange {
    /// The console password (set, another one, or none any more).
    pub password: bool,
    /// Where data goes or who may operate Bageo ([`CONNECTION_FIELDS`]).
    pub connections: bool,
}

/// Read a settings file for the import preview. Err: not a settings file (not
/// a JSON object of settings). Keys this agent does not know count for nothing.
pub fn peek_settings_text(text: &str) -> Result<SettingsPeek> {
    let (ov, _) = read_overrides(text)?;
    let raw: Value = serde_json::from_str(text)?;
    let known: Vec<String> = serde_json::to_value(&ov)?.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let mut m = raw.as_object().cloned().unwrap_or_default();
    m.retain(|k, _| known.contains(k) || k == "deepseek_api_key");
    let v = Value::Object(m);
    Ok(SettingsPeek {
        has_settings: has_settings(&v),
        has_keys: has_keys(&v),
    })
}

/// `public_url` and `backup_dir` belong to this computer: a backup never brings them.
fn keep_this_computers(m: &mut Map<String, Value>, cur: &Value) {
    for k in ["public_url", "backup_dir"] {
        match cur.get(k) {
            Some(v) => {
                m.insert(k.to_string(), v.clone());
            }
            None => {
                m.remove(k);
            }
        }
    }
}

fn sanitize_answer_style(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "persona" => "persona".to_string(),
        _ => default_answer_style(),
    }
}

fn digits_only(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).take(4).collect()
}

/// `low` / `medium` / `high`; anything else (the model refuses `minimal`) → low.
fn sanitize_thinking_level(level: &str) -> String {
    match level.trim().to_ascii_lowercase().as_str() {
        l @ ("medium" | "high") => l.to_string(),
        _ => default_thinking_level(),
    }
}

/// Unknown kind → off; level clamped to 1–3.
fn sanitize_ambience(kind: &str, level: u32) -> (String, u32) {
    let k = kind.trim().to_ascii_lowercase();
    let k = if crate::ambience::valid_kind(&k) { k } else { "off".to_string() };
    (k, level.clamp(1, 3))
}

impl RuntimeSettings {
    /// Env-derived defaults. Existing keys come from the parsed `Settings`;
    /// newer keys read their own env vars (all default to off/empty).
    pub fn from_base(base: &Settings) -> Self {
        Self {
            openrouter_api_key: base.openrouter_api_key.clone(),
            openrouter_model: base.openrouter_model.clone(),
            ai_live_enabled: env_bool("AI_LIVE_ENABLED", false),
            telegram_bot_token: base.telegram_bot_token.clone(),
            telegram_chat_id: base.telegram_chat_id.clone(),
            notify_gateway_url: base.notify_gateway_url.clone(),
            notify_gateway_tag: base.notify_gateway_tag.clone(),
            call_notify: base.call_notify,
            gmail_user: env_or("GMAIL_USER", ""),
            gmail_app_password: env_or("GMAIL_APP_PASSWORD", ""),
            gmail_from_name: env_or("GMAIL_FROM_NAME", ""),
            openclaw_url: env_or("OPENCLAW_URL", ""),
            openclaw_token: env_or("OPENCLAW_TOKEN", ""),
            shaming_enabled: env_bool("SHAMING_ENABLED", false),
            shaming_multiplier: env_parse("SHAMING_MULTIPLIER", 1.0f64),
            shaming_template: env_or("SHAMING_TEMPLATE", DEFAULT_SHAMING_TEMPLATE),
            spam_blocklist: split_list(&env_or("SPAM_BLOCKLIST", "")),
            spam_ai_filter_enabled: env_bool("SPAM_AI_FILTER_ENABLED", false),
            spam_keywords: split_list(&env_or("SPAM_KEYWORDS", "")),
            callback_whitelist: split_list(&env_or("CALLBACK_WHITELIST", "")),
            vm_max_ms: base.vm_max_ms,
            vm_silence_ms: base.vm_silence_ms,
            vm_silence_rms: base.vm_silence_rms,
            vm_confirm_wait_ms: base.vm_confirm_wait_ms,
            ivr_start_delay_ms: base.ivr_start_delay_ms,
            post_greet_ai_delay_ms: base.post_greet_ai_delay_ms,
            goodbye_hangup_delay_ms: base.goodbye_hangup_delay_ms,
            ivr_reprompt_ms: base.ivr_reprompt_ms,
            ivr_max_menu_plays: base.ivr_max_menu_plays,
            ivr_goodbye_text: base.ivr_goodbye_text.clone(),
            ivr_vm_intro_text: base.ivr_vm_intro_text.clone(),
            ivr_vm_confirm_text: base.ivr_vm_confirm_text.clone(),
            vm_reply_ack_text: base.vm_reply_ack_text.clone(),
            trtc_sdk_app_id: base.trtc_sdk_app_id,
            trtc_secret_key: base.trtc_secret_key.as_str().to_string(),
            trtc_secret_id: base.trtc_secret_id.as_str().to_string(),
            trtc_cloud_secret_key: base.trtc_cloud_secret_key.as_str().to_string(),
            trtc_ai_enabled: base.trtc_ai_enabled,
            trtc_usersig_expire_secs: base.trtc_usersig_expire_secs.min(u64::from(u32::MAX)) as u32,
            trtc_ai_region: base.trtc_ai_region.clone(),
            trtc_api_host: base.trtc_api_host.clone(),
            trtc_api_intl: base.trtc_api_intl,
            trtc_llm_config: base.trtc_llm_config.as_str().to_string(),
            trtc_tts_config: base.trtc_tts_config.as_str().to_string(),
            trtc_stt_language: base.trtc_stt_language.clone(),
            trtc_ai_max_idle_secs: base.trtc_ai_max_idle_secs,
            trtc_bridge_enabled: base.trtc_bridge_enabled,
            trtc_bridge_url: base.trtc_bridge_url.clone(),
            gemini_live_enabled: env_bool("GEMINI_LIVE_ENABLED", true),
            gemini_api_key: env_or("GEMINI_API_KEY", ""),
            gemini_live_model: env_or("GEMINI_LIVE_MODEL", crate::gemini_live::default_model()),
            gemini_system_prompt: env_or(
                "GEMINI_SYSTEM_PROMPT",
                crate::gemini_live::default_system_prompt(),
            ),
            gemini_voice_name: env_or(
                "GEMINI_VOICE_NAME",
                crate::gemini_live::default_voice_name(),
            ),
            summary_api_key: base.summary_api_key.clone(),
            summary_key_saved: false,
            owner_status_enabled: env_bool("OWNER_STATUS_ENABLED", false),
            owner_status_text: env_or("OWNER_STATUS_TEXT", ""),
            gemini_persona_mode: env_bool("GEMINI_PERSONA_MODE", true),
            // Empty = the built-in prompt of the language and answer style.
            gemini_persona_prompt: env_or("GEMINI_PERSONA_PROMPT", ""),
            gemini_assistant_prompt: env_or("GEMINI_ASSISTANT_PROMPT", ""),
            answer_unknown_callers: env_bool("ANSWER_UNKNOWN_CALLERS", true),
            reject_numbers: clean_reject_list(
                split_list(&env_or("REJECT_NUMBERS", ""))
                    .into_iter()
                    .map(|line| RejectEntry::from(RejectEntryRepr::Line(line)))
                    .collect(),
            ),
            reject_numbers_moved: false,
            reject_voice_name: env_or("REJECT_VOICE_NAME", ""),
            reject_mode: sanitize_reject_mode(&env_or("REJECT_MODE", "decline")),
            persona_stall_spam: env_bool("PERSONA_STALL_SPAM", true),
            scam_review_without_keywords: env_bool("SCAM_REVIEW_WITHOUT_KEYWORDS", true),
            persona_stall_max_min: env_parse("PERSONA_STALL_MAX_MIN", STALL_MAX_MIN_DEFAULT),
            persona_normal_max_min: env_parse("PERSONA_NORMAL_MAX_MIN", NORMAL_MAX_MIN_DEFAULT),
            retention_days: base.calls_retention_days.clamp(RETENTION_DAYS_MIN, RETENTION_DAYS_MAX),
            gemini_stall_model: env_or("GEMINI_STALL_MODEL", STALL_MODEL_DEFAULT),
            gemini_server_vad: env_bool("GEMINI_SERVER_VAD", true),
            gemini_barge_in: env_bool("GEMINI_BARGE_IN", true),
            answer_ring_ms: env_parse("ANSWER_RING_MS", ANSWER_RING_MS_DEFAULT)
                .min(ANSWER_RING_MS_CEIL),
            gemini_ring_prewarm: env_bool("GEMINI_RING_PREWARM", true),
            ambience_kind: sanitize_ambience(&env_or("AMBIENCE_KIND", "off"), 2).0,
            ambience_level: env_parse("AMBIENCE_LEVEL", 2u32).clamp(1, 3),
            gemini_thinking_level: sanitize_thinking_level(&env_or("GEMINI_THINKING_LEVEL", "low")),
            language: sanitize_language(&env_or("APP_LANGUAGE", "en")),
            notify_language: sanitize_notify_language(&env_or("NOTIFY_LANGUAGE", "")),
            sim_languages: std::collections::BTreeMap::new(),
            owner_name: env_or("OWNER_NAME", ""),
            owner_aliases: clean_owner_aliases(&split_list(&env_or("OWNER_ALIASES", "")), &env_or("OWNER_NAME", "")),
            answer_style: sanitize_answer_style(&env_or("ANSWER_STYLE", "assistant")),
            knowledge_notes: env_or("KNOWLEDGE_NOTES", ""),
            reply_in_caller_language: env_bool("REPLY_IN_CALLER_LANGUAGE", default_reply_in_caller_language()),
            country_code: digits_only(&env_or("COUNTRY_CODE", "")),
            summary_api_base: env_or("SUMMARY_API_BASE", ""),
            summary_model: env_or("SUMMARY_MODEL", ""),
            summary_provider: sanitize_summary_provider(&env_or("SUMMARY_PROVIDER", "auto")),
            notify_channels: Vec::new(),
            public_url: env_or("PUBLIC_URL", ""),
            mdns_name: base.mdns_name.clone(),
            phone_wifi: env_bool("PHONE_WIFI", true),
            calendar_suggest: env_bool("CALENDAR_SUGGEST", true),
            calendar_app: String::new(),
            appointment_mode: sanitize_appointment_mode(&env_or("APPOINTMENT_MODE", "off")),
            appointment_sms: env_bool("APPOINTMENT_SMS", false),
            appointment_reply: crate::appointments::sanitize_reply_setting(&env_or("APPOINTMENT_REPLY", "auto")),
            errand_identity: sanitize_errand_identity(&env_or("ERRAND_IDENTITY", "assistant")),
            telegram_control: env_bool("TELEGRAM_CONTROL", false),
            telegram_control_users: sanitize_sender_ids(&[env_or("TELEGRAM_CONTROL_USERS", "")]),
            control_errands_per_hour: env_parse("CONTROL_ERRANDS_PER_HOUR", default_control_errands_per_hour()).min(30),
            control_commands_per_minute: env_parse("CONTROL_COMMANDS_PER_MINUTE", default_control_commands_per_minute()).clamp(1, 120),
            mcp_enabled: env_bool("MCP_ENABLED", false),
            mcp_token: env_or("MCP_TOKEN", ""),
            mcp_calls: sanitize_mcp_calls(&env_or("MCP_CALLS", "off")),
            mcp_errands_per_hour: env_parse("MCP_ERRANDS_PER_HOUR", default_mcp_errands_per_hour()).min(30),
            mcp_allowed_origins: sanitize_origins(&[env_or("MCP_ALLOWED_ORIGINS", "")]),
            name_corrections: split_list(&env_or("NAME_CORRECTIONS", "")),
            device_labels: std::collections::BTreeMap::new(),
            whitelist_mode: sanitize_whitelist_mode(&env_or("WHITELIST_MODE", "off")),
            whitelist_forward_number: env_or("WHITELIST_FORWARD_NUMBER", ""),
            whitelist_urgent_call: env_bool("WHITELIST_URGENT_CALL", false),
            transfer_whitelist: env_bool("TRANSFER_WHITELIST", true),
            transfer_contacts: env_bool("TRANSFER_CONTACTS", false),
            transfer_others: env_bool("TRANSFER_OTHERS", false),
            ui_password: String::new(),
            ui_password_prompted: false,
            backup_schedule: default_backup_schedule(),
            backup_dir: String::new(),
            backup_keep: default_backup_keep(),
            backup_include_calls: false,
            backup_encrypt: false,
            backup_password: String::new(),
            calls_data_dir: base.calls_data_dir.clone(),
        }
    }

    /// Hand the settings the call helpers read as process-wide state to them
    /// (start-up and every save).
    pub fn apply_globals(&self) {
        crate::locale::set_active(self.locale_active());
        crate::summary::set_provider(self.summary_provider());
        crate::notify::set_device_labels(&self.device_labels);
    }

    pub fn lang(&self) -> crate::locale::Lang {
        crate::locale::Lang::parse(&self.language)
    }

    /// The language of what the owner reads: `notify_language`, else `language`.
    pub fn notify_lang(&self) -> crate::locale::Lang {
        match sanitize_notify_language(&self.notify_language).as_str() {
            "" => self.lang(),
            code => crate::locale::Lang::parse(code),
        }
    }

    /// Which service writes the post-call summary.
    pub fn summary_provider(&self) -> crate::summary::Provider {
        crate::summary::Provider::parse(&self.summary_provider)
    }

    /// `summary_provider` as `gemini` or `custom`. A value from before keeps
    /// the summary going where it went: its old name for `custom` is `custom`
    /// (with a key, the address it passed over is set aside, and
    /// [fill_old_summary_service] writes down the one it used); `auto` (or none)
    /// is `custom` when an address or a summary key of its own is set, else
    /// `gemini`.
    fn settle_summary_provider(&mut self) {
        let v = sanitize_summary_provider(&self.summary_provider);
        if v == "gemini" || v == "custom" {
            self.summary_provider = v;
            return;
        }
        let summary_key = !self.summary_api_key.trim().is_empty()
            || crate::summary::api_key_from_llm_config(&self.trtc_llm_config).is_some();
        // Reading settings of older versions: "deepseek" was "custom".
        if v == "deepseek" && summary_key {
            self.summary_api_base.clear();
        }
        let custom = v == "deepseek" || summary_key || !self.summary_api_base.trim().is_empty();
        self.summary_provider = if custom { "custom" } else { "gemini" }.to_string();
    }

    /// Blocked numbers are refused while they ring (`reject_mode` `decline`).
    pub fn reject_declines(&self) -> bool {
        self.reject_mode != "announce"
    }

    /// Blocked numbers of an older version not on the Blacklist yet (still
    /// turned away); none once they have moved.
    pub fn pending_rejects(&self) -> &[RejectEntry] {
        if self.reject_numbers_moved {
            &[]
        } else {
            &self.reject_numbers
        }
    }

    pub fn is_assistant_style(&self) -> bool {
        self.answer_style_is_assistant() || self.legacy_flow_replaced()
    }

    /// The saved `answer_style` is "AI assistant" (what the console shows).
    pub fn answer_style_is_assistant(&self) -> bool {
        self.answer_style != "persona"
    }

    /// `gemini_persona_mode` is off, but the older relay flow (greeting lines and
    /// call rules written in Traditional Chinese only) is not used for an owner
    /// whose language is another one: the call runs in the current flow, as the
    /// owner's AI assistant, in the owner's language.
    pub fn legacy_flow_replaced(&self) -> bool {
        !self.gemini_persona_mode && self.lang() != crate::locale::Lang::ZhTw
    }

    /// Whether calls run in the current flow (opening, language rules, closing):
    /// the value every call path reads instead of `gemini_persona_mode`.
    pub fn persona_flow_active(&self) -> bool {
        self.gemini_persona_mode || self.legacy_flow_replaced()
    }

    /// Says in the log that a turned-off `gemini_persona_mode` is not honored.
    fn warn_if_legacy_flow_replaced(&self) {
        if self.legacy_flow_replaced() {
            warn!(
                "gemini_persona_mode is off, but the older answering flow only exists in Traditional Chinese: calls in language={} use the current flow as the AI assistant",
                self.language
            );
        }
    }

    /// Country calling code: the setting; else Taiwan's for zh-TW; else the
    /// country of the SIMs the connected phones reported (the language's own
    /// country when one of them is in it); else the language's own country.
    pub fn effective_country_code(&self) -> String {
        self.effective_country_code_with(&crate::phone_region::seen_sims())
    }

    /// [`Self::effective_country_code`] with the SIM countries (ISO 3166
    /// alpha-2, lowercase) the phones reported given.
    pub fn effective_country_code_with(&self, sims: &[String]) -> String {
        let c = digits_only(&self.country_code);
        if !c.is_empty() {
            return c;
        }
        let lang = self.lang();
        if lang == crate::locale::Lang::ZhTw {
            return "886".to_string();
        }
        let own = language_country(lang);
        let iso = if sims.iter().any(|s| s == own) {
            own
        } else {
            sims.iter().map(String::as_str).find(|s| crate::phone_region::calling_code(s).is_some()).unwrap_or(own)
        };
        crate::phone_region::calling_code(iso).unwrap_or("").to_string()
    }

    /// The country (ISO 3166 alpha-2, lowercase) a number without a country
    /// code reads in when nothing else says: the language's own.
    pub fn default_region(&self) -> &'static str {
        language_country(self.lang())
    }

    /// What the Live session's helpers read (`locale::set_active`).
    pub fn locale_active(&self) -> crate::locale::Active {
        crate::locale::Active {
            lang: self.lang(),
            notify_lang: self.notify_lang(),
            assistant: self.is_assistant_style(),
            owner: self.owner_name.trim().to_string(),
            owner_aliases: self.owner_aliases.clone(),
            country_code: self.effective_country_code(),
            name_fixes: crate::locale::parse_name_fixes(&self.name_corrections),
        }
    }

    /// Custom prompt for the active `answer_style` (`gemini_assistant_prompt`
    /// for `assistant`, `gemini_persona_prompt` for `persona`).
    pub fn active_custom_prompt(&self) -> &str {
        if self.is_assistant_style() {
            &self.gemini_assistant_prompt
        } else {
            &self.gemini_persona_prompt
        }
    }

    /// A call's prompt in `lang` (None: the setting `language`). A call in a SIM's
    /// own language (setting `sim_languages`, [`crate::gemini_live::CallLangPlan::prompt_lang`])
    /// gets its prompt, its rules and the identity guard in that language.
    fn call_prompt(&self, stall: crate::gemini_live::StallPrompt, lang: Option<crate::locale::Lang>) -> String {
        let lang = lang.unwrap_or_else(|| self.lang());
        crate::prompts::build(&crate::prompts::Ctx {
            lang,
            assistant: self.is_assistant_style(),
            owner: &self.owner_name,
            aliases: &self.owner_aliases,
            custom: self.active_custom_prompt(),
            status: if self.owner_status_enabled { &self.owner_status_text } else { "" },
            stall,
            knowledge: &self.knowledge_notes,
            carrier_note: lang == crate::locale::Lang::ZhTw,
            auto_language: self.reply_in_caller_language,
        })
    }

    /// The built-in prompt for the current language, style and owner name
    /// (what an empty custom prompt for that style means).
    pub fn default_prompt(&self) -> String {
        crate::prompts::default_prompt(self.lang(), self.is_assistant_style(), &self.owner_name)
    }

    /// Characters of the persona text in a call's prompt in `lang` (None = the
    /// setting's): the owner's own, else the built-in one.
    pub fn persona_user_chars(&self, lang: Option<crate::locale::Lang>) -> usize {
        crate::prompts::persona_chars(
            lang.unwrap_or_else(|| self.lang()),
            self.is_assistant_style(),
            &self.owner_name,
            self.active_custom_prompt(),
        )
    }

    /// System prompt the session switches to once a call is judged sales / scam.
    pub fn persona_stall_system_prompt(&self) -> String {
        self.persona_stall_system_prompt_in(None)
    }

    /// [`Self::persona_stall_system_prompt`] in `lang` (None: the setting's).
    pub fn persona_stall_system_prompt_in(&self, lang: Option<crate::locale::Lang>) -> String {
        self.call_prompt(crate::gemini_live::StallPrompt::Full, lang)
    }

    /// Persona-mode normal-call cap in seconds (setting clamped to 3–30 minutes).
    pub fn persona_max_secs(&self) -> u64 {
        u64::from(
            self.persona_normal_max_min
                .clamp(STALL_MAX_MIN_FLOOR, STALL_MAX_MIN_CEIL),
        ) * 60
    }

    /// Cap in seconds once a call is judged sales / scam (3–30 minutes).
    pub fn persona_stall_max_secs(&self) -> u64 {
        u64::from(
            self.persona_stall_max_min
                .clamp(STALL_MAX_MIN_FLOOR, STALL_MAX_MIN_CEIL),
        ) * 60
    }

    /// Non-empty desk note that should be spoken this call.
    pub fn owner_status_note(&self) -> String {
        if !self.owner_status_enabled {
            return String::new();
        }
        crate::gemini_live::sanitize_owner_status(&self.owner_status_text)
    }

    /// System prompt plus optional desk-status block for this call.
    pub fn gemini_system_prompt_for_call(&self) -> String {
        self.gemini_system_prompt_for_call_in(None)
    }

    /// [`Self::gemini_system_prompt_for_call`]; persona mode builds it in `lang`
    /// (None: the setting's).
    pub fn gemini_system_prompt_for_call_in(&self, lang: Option<crate::locale::Lang>) -> String {
        if self.persona_flow_active() {
            use crate::gemini_live::StallPrompt;
            return self.call_prompt(
                if self.persona_stall_spam {
                    StallPrompt::ListenFirst
                } else {
                    StallPrompt::Off
                },
                lang,
            );
        }
        let base = if self.gemini_system_prompt.trim().is_empty() {
            crate::gemini_live::default_system_prompt().to_string()
        } else {
            self.gemini_system_prompt.clone()
        };
        crate::gemini_live::with_owner_status(
            &base,
            self.owner_status_enabled,
            &self.owner_status_text,
        )
    }

    fn apply(&mut self, ov: &Overrides) {
        macro_rules! set {
            ($($f:ident),+ $(,)?) => {$(
                if let Some(v) = &ov.$f { self.$f = v.clone(); }
            )+};
        }
        set!(
            openrouter_api_key,
            openrouter_model,
            telegram_bot_token,
            telegram_chat_id,
            notify_gateway_url,
            notify_gateway_tag,
            gmail_user,
            gmail_app_password,
            gmail_from_name,
            openclaw_url,
            openclaw_token,
            shaming_template,
            spam_blocklist,
            spam_keywords,
            callback_whitelist,
            reject_numbers,
            reject_numbers_moved,
            reject_voice_name,
            reject_mode,
            ivr_goodbye_text,
            ivr_vm_intro_text,
            ivr_vm_confirm_text,
            vm_reply_ack_text,
            trtc_secret_key,
            trtc_secret_id,
            trtc_cloud_secret_key,
            trtc_ai_region,
            trtc_api_host,
            trtc_llm_config,
            trtc_tts_config,
            trtc_stt_language,
            trtc_bridge_url,
            gemini_api_key,
            gemini_live_model,
            gemini_system_prompt,
            gemini_voice_name,
            summary_api_key,
            owner_status_text,
            gemini_persona_prompt,
            gemini_assistant_prompt,
            ambience_kind,
            gemini_thinking_level,
            gemini_stall_model,
            language,
            notify_language,
            sim_languages,
            owner_name,
            owner_aliases,
            answer_style,
            knowledge_notes,
            country_code,
            summary_api_base,
            summary_model,
            summary_provider,
            notify_channels,
            public_url,
            mdns_name,
            name_corrections,
            device_labels,
            appointment_mode,
            calendar_app,
            appointment_reply,
            errand_identity,
            telegram_control_users,
            mcp_token,
            mcp_calls,
            mcp_allowed_origins,
            whitelist_mode,
            whitelist_forward_number,
            ui_password,
        );
        macro_rules! set_copy {
            ($($f:ident),+ $(,)?) => {$(
                if let Some(v) = ov.$f { self.$f = v; }
            )+};
        }
        set_copy!(
            ai_live_enabled,
            telegram_control,
            control_errands_per_hour,
            control_commands_per_minute,
            mcp_enabled,
            mcp_errands_per_hour,
            call_notify,
            shaming_enabled,
            spam_ai_filter_enabled,
            shaming_multiplier,
            vm_max_ms,
            vm_silence_ms,
            vm_silence_rms,
            vm_confirm_wait_ms,
            ivr_start_delay_ms,
            post_greet_ai_delay_ms,
            goodbye_hangup_delay_ms,
            ivr_reprompt_ms,
            ivr_max_menu_plays,
            trtc_sdk_app_id,
            trtc_ai_enabled,
            trtc_usersig_expire_secs,
            trtc_api_intl,
            trtc_ai_max_idle_secs,
            trtc_bridge_enabled,
            gemini_live_enabled,
            owner_status_enabled,
            gemini_persona_mode,
            answer_unknown_callers,
            phone_wifi,
            persona_stall_spam,
            scam_review_without_keywords,
            whitelist_urgent_call,
            transfer_whitelist,
            transfer_contacts,
            transfer_others,
            persona_stall_max_min,
            persona_normal_max_min,
            retention_days,
            gemini_server_vad,
            gemini_barge_in,
            answer_ring_ms,
            gemini_ring_prewarm,
            ambience_level,
            reply_in_caller_language,
            calendar_suggest,
            appointment_sms,
            ui_password_prompted,
        );
        self.summary_key_saved = ov.summary_api_key.as_deref().is_some_and(|k| !k.trim().is_empty())
            || ov.trtc_llm_config.as_deref().is_some_and(|c| crate::summary::api_key_from_llm_config(c).is_some());
        self.ivr_start_delay_ms = self
            .ivr_start_delay_ms
            .min(crate::config::IVR_FIRST_AUDIO_DELAY_CAP_MS);
        self.answer_ring_ms = self.answer_ring_ms.min(ANSWER_RING_MS_CEIL);
        self.retention_days = self
            .retention_days
            .clamp(RETENTION_DAYS_MIN, RETENTION_DAYS_MAX);
        (self.ambience_kind, self.ambience_level) =
            sanitize_ambience(&self.ambience_kind, self.ambience_level);
        self.gemini_thinking_level = sanitize_thinking_level(&self.gemini_thinking_level);
        self.language = sanitize_language(&self.language);
        self.notify_language = sanitize_notify_language(&self.notify_language);
        self.sim_languages = sanitize_sim_languages(
            self.sim_languages
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        );
        self.settle_summary_provider();
        self.answer_style = sanitize_answer_style(&self.answer_style);
        self.reject_mode = sanitize_reject_mode(&self.reject_mode);
        self.whitelist_mode = sanitize_whitelist_mode(&self.whitelist_mode);
        self.whitelist_forward_number = self.whitelist_forward_number.trim().chars().filter(|c| !c.is_control()).take(32).collect();
        self.country_code = digits_only(&self.country_code);
        self.public_url = sanitize_public_url(&self.public_url);
        self.mdns_name = crate::mdns::parse_name_setting(&self.mdns_name);
        self.appointment_mode = sanitize_appointment_mode(&self.appointment_mode);
        self.calendar_app = crate::calendar::sanitize_app(&self.calendar_app);
        self.appointment_reply = crate::appointments::sanitize_reply_setting(&self.appointment_reply);
        self.owner_aliases = clean_owner_aliases(&self.owner_aliases, &self.owner_name);
        self.errand_identity = sanitize_errand_identity(&self.errand_identity);
        if let Some(v) = &ov.backup_schedule {
            self.backup_schedule = v.clone();
        }
        if let Some(v) = &ov.backup_dir {
            self.backup_dir = v.clone();
        }
        if let Some(v) = ov.backup_keep {
            self.backup_keep = v;
        }
        if let Some(v) = ov.backup_include_calls {
            self.backup_include_calls = v;
        }
        if let Some(v) = ov.backup_encrypt {
            self.backup_encrypt = v;
        }
        if let Some(v) = &ov.backup_password {
            self.backup_password = v.trim().to_string();
        }
        // Encryption needs a password: a settings file with one and not the
        // other (a restore without the keys) backs up as before.
        if self.backup_password.is_empty() {
            self.backup_encrypt = false;
        }
        self.backup_schedule = sanitize_backup_schedule(&self.backup_schedule);
        self.backup_dir = sanitize_backup_dir(&self.backup_dir);
        self.backup_keep = self.backup_keep.clamp(1, 100);
    }

    /// Overlay runtime-editable values onto the env base so existing call
    /// sites (`notify`, `services`, IVR/VM timing reads) keep using `&Settings`.
    pub fn effective(&self, base: &Settings) -> Settings {
        let mut s = base.clone();
        s.openrouter_api_key = self.openrouter_api_key.clone();
        s.openrouter_model = self.openrouter_model.clone();
        s.telegram_bot_token = self.telegram_bot_token.clone();
        s.telegram_chat_id = self.telegram_chat_id.clone();
        // Do not wipe env NOTIFY_GATEWAY_URL with empty UI override.
        if !self.notify_gateway_url.trim().is_empty() {
            s.notify_gateway_url = self.notify_gateway_url.clone();
        }
        if !self.notify_gateway_tag.trim().is_empty() {
            s.notify_gateway_tag = self.notify_gateway_tag.clone();
        }
        s.call_notify = self.call_notify;
        s.vm_max_ms = self.vm_max_ms;
        s.vm_silence_ms = self.vm_silence_ms;
        s.vm_silence_rms = self.vm_silence_rms;
        s.vm_confirm_wait_ms = self.vm_confirm_wait_ms;
        // Honest health/UI: never advertise > first-audio hard ceiling.
        s.ivr_start_delay_ms = self
            .ivr_start_delay_ms
            .min(crate::config::IVR_FIRST_AUDIO_DELAY_CAP_MS);
        s.post_greet_ai_delay_ms = self.post_greet_ai_delay_ms;
        s.goodbye_hangup_delay_ms = self.goodbye_hangup_delay_ms;
        s.ivr_reprompt_ms = self.ivr_reprompt_ms;
        s.ivr_max_menu_plays = self.ivr_max_menu_plays.max(1);
        s.ivr_goodbye_text = self.ivr_goodbye_text.clone();
        s.ivr_vm_intro_text = self.ivr_vm_intro_text.clone();
        s.ivr_vm_confirm_text = self.ivr_vm_confirm_text.clone();
        s.vm_reply_ack_text = self.vm_reply_ack_text.clone();
        s.trtc_sdk_app_id = self.trtc_sdk_app_id;
        s.trtc_secret_key = self.trtc_secret_key.clone().into();
        s.trtc_secret_id = self.trtc_secret_id.clone().into();
        s.trtc_cloud_secret_key = self.trtc_cloud_secret_key.clone().into();
        s.trtc_ai_enabled = self.trtc_ai_enabled;
        s.trtc_usersig_expire_secs = u64::from(self.trtc_usersig_expire_secs);
        s.trtc_ai_region = self.trtc_ai_region.clone();
        s.trtc_api_host = self.trtc_api_host.clone();
        // Product lock: mainland domestic TRTC Cloud API only.
        s.trtc_api_intl = false;
        s.trtc_llm_config = self.trtc_llm_config.clone().into();
        s.trtc_tts_config = self.trtc_tts_config.clone().into();
        s.trtc_stt_language = self.trtc_stt_language.clone();
        // Tencent requires [60, 259200]; preserve 120 as the normal safety net.
        s.trtc_ai_max_idle_secs = self.trtc_ai_max_idle_secs.clamp(60, 259_200);
        s.trtc_bridge_enabled = self.trtc_bridge_enabled;
        s.trtc_bridge_url = self.trtc_bridge_url.clone();
        if !self.summary_api_key.trim().is_empty() {
            s.summary_api_key = self.summary_api_key.clone();
        }
        s.notify_channels = self.notify_channels.clone();
        s.summary_api_base = self.summary_api_base.clone();
        s.summary_model = self.summary_model.clone();
        // A key only in the environment, under the variable's former name (or in
        // a TRTC LLM config there): an older setup, its summaries go where they went.
        let old_env_key = base.summary_api_key_old_env
            || (base.summary_api_key.trim().is_empty()
                && crate::summary::api_key_from_llm_config(base.trtc_llm_config.as_ref()).is_some());
        if !self.summary_key_saved && old_env_key {
            let filled = fill_old_summary_service(&self.summary_provider, true, &mut s.summary_api_base, &mut s.summary_model);
            // Only a TRTC LLM config key: it becomes the summary key of that service.
            if filled && s.summary_api_key.trim().is_empty() {
                if let Some(k) = crate::summary::api_key_from_llm_config(base.trtc_llm_config.as_ref()) {
                    s.summary_api_key = k;
                }
            }
        }
        s.gemini_api_key = self.gemini_api_key.clone().into();
        if !self.public_url.trim().is_empty() {
            s.public_url = self.public_url.clone();
        }
        // Empty is a choice here (mDNS off), so it always applies.
        s.mdns_name = self.mdns_name.clone();
        if !self.ui_password.trim().is_empty() {
            s.calls_ui_token = self.ui_password.trim().to_string();
        }
        s
    }

    /// Fulfill email gate: Gmail user + app password both non-empty after trim.
    pub fn gmail_configured(&self) -> bool {
        !self.gmail_user.trim().is_empty() && !self.gmail_app_password.trim().is_empty()
    }

    /// UserSig path ready (SDKAppID + secret key). Mirrors `TrtcSettings::configured`.
    pub fn trtc_configured(&self) -> bool {
        self.trtc_sdk_app_id > 0 && !self.trtc_secret_key.trim().is_empty()
    }

    /// Cloud API StartAIConversation ready. Mirrors `TrtcSettings::cloud_api_ready`.
    pub fn trtc_cloud_api_ready(&self) -> bool {
        !self.trtc_secret_id.trim().is_empty()
            && !self.trtc_cloud_secret_key.trim().is_empty()
            && !self.trtc_llm_config.trim().is_empty()
            && !self.trtc_tts_config.trim().is_empty()
    }
}

/// The address of this web console as other devices reach it, for links in
/// notifications, `/health` and the console's own "Console address":
/// what the owner typed in "Address in notifications"; else, when the agent is
/// open to the LAN, `http://<mdns_name>.local:<port>/` while the mDNS
/// responder answers (`http://<mdns_name>.local/` when `AGENT_HOST` is not
/// set and port 80 is open too, `crate::lan`), or `http://<LAN IP>:<port>/`.
/// Empty when the agent listens on this computer only and nothing was typed (a
/// link to localhost would not open on a phone), and in a container: there the
/// name and the address are Docker's own network, which other devices cannot reach.
pub fn effective_console_url(s: &Settings) -> String {
    let host = crate::mdns::parse_name_setting(&s.mdns_name);
    let mdns_on = !host.is_empty() && crate::mdns::is_active();
    console_url_for(s, in_container(), crate::lan::auto(), crate::lan::port_80(), mdns_on, crate::mdns::answer_ip)
}

/// [`effective_console_url`] with what it reads from the running agent given:
/// inside a container, `AGENT_HOST` not set (`auto`), port 80 open, mDNS answering.
pub(crate) fn console_url_for(
    s: &Settings,
    in_container: bool,
    auto: bool,
    port80: bool,
    mdns_on: bool,
    lan_ip: impl FnOnce(std::net::IpAddr) -> Option<std::net::Ipv4Addr>,
) -> String {
    let listen = if in_container { None } else { console_listen(s, auto) };
    let host = crate::mdns::parse_name_setting(&s.mdns_name);
    console_url(&s.public_url, listen, s.port, &host, mdns_on && !host.is_empty(), auto && port80, lan_ip)
}

/// The address the console listens on: all of them when `AGENT_HOST` is not set.
fn console_listen(s: &Settings, auto: bool) -> Option<std::net::IpAddr> {
    if auto {
        return Some(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    }
    s.host.trim().parse::<std::net::IpAddr>().ok()
}

/// [`effective_console_url`] without the typed address: what an empty
/// "Address in notifications" stands for.
pub fn default_console_url(s: &Settings) -> String {
    let mut plain = s.clone();
    plain.public_url.clear();
    effective_console_url(&plain)
}

/// Who can open the console: `local` = this computer only (`AGENT_HOST` set
/// to a loopback address), `lan` = other devices too.
pub fn console_access(s: &Settings) -> &'static str {
    console_access_for(s, in_container(), crate::lan::auto())
}

fn console_access_for(s: &Settings, in_container: bool, auto: bool) -> &'static str {
    if in_container || auto {
        return "lan";
    }
    match s.host.trim().parse::<std::net::IpAddr>() {
        Ok(ip) if ip.is_loopback() => "local",
        _ => "lan",
    }
}

/// Running inside Docker or Podman.
pub(crate) fn in_container() -> bool {
    static IN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN.get_or_init(|| Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists())
}

/// The console at this computer's LAN IP (`http://192.168.1.20:8787/`), for
/// devices that cannot open `.local` names (some older Android phones). Empty
/// when only this computer can open the console, or inside a container.
pub fn lan_console_url(s: &Settings) -> String {
    if in_container() {
        return String::new();
    }
    console_url("", console_listen(s, crate::lan::auto()), s.port, "", false, false, crate::mdns::answer_ip)
}

fn console_url(
    public_url: &str,
    listen: Option<std::net::IpAddr>,
    port: u16,
    mdns_host: &str,
    mdns_on: bool,
    port80: bool,
    lan_ip: impl FnOnce(std::net::IpAddr) -> Option<std::net::Ipv4Addr>,
) -> String {
    let own = sanitize_public_url(public_url);
    if !own.is_empty() {
        return own;
    }
    let Some(listen) = listen.filter(|ip| !ip.is_loopback()) else {
        return String::new();
    };
    if mdns_on {
        if port80 {
            return format!("http://{mdns_host}.local/");
        }
        return format!("http://{mdns_host}.local:{port}/");
    }
    match lan_ip(listen) {
        Some(ip) => format!("http://{ip}:{port}/"),
        None => String::new(),
    }
}

/// Persisted override document — only user-changed keys (all optional).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Overrides {
    #[serde(skip_serializing_if = "Option::is_none")]
    openrouter_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    openrouter_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ai_live_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telegram_bot_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telegram_chat_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notify_gateway_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notify_gateway_tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    call_notify: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gmail_user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gmail_app_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gmail_from_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    openclaw_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    openclaw_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shaming_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shaming_multiplier: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shaming_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spam_blocklist: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spam_ai_filter_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spam_keywords: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    callback_whitelist: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reject_numbers: Option<Vec<RejectEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reject_numbers_moved: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reject_voice_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reject_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_max_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_silence_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_silence_rms: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_confirm_wait_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ivr_start_delay_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    post_greet_ai_delay_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    goodbye_hangup_delay_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ivr_reprompt_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ivr_max_menu_plays: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ivr_goodbye_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ivr_vm_intro_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ivr_vm_confirm_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    vm_reply_ack_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_sdk_app_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_secret_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_secret_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_cloud_secret_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_ai_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_usersig_expire_secs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_ai_region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_api_host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_api_intl: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_llm_config: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_tts_config: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_stt_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_ai_max_idle_secs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_bridge_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trtc_bridge_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_live_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_live_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_system_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_voice_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_status_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_persona_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_persona_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_assistant_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer_unknown_callers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_stall_spam: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scam_review_without_keywords: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_stall_max_min: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_server_vad: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_barge_in: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer_ring_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_ring_prewarm: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ambience_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ambience_level: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_thinking_level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_normal_max_min: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retention_days: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_stall_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notify_language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sim_languages: Option<std::collections::BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_aliases: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    knowledge_notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_in_caller_language: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_api_base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notify_channels: Option<Vec<crate::notify::Channel>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mdns_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phone_wifi: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_suggest: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar_app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    appointment_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    appointment_sms: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    appointment_reply: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errand_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telegram_control: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telegram_control_users: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_errands_per_hour: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    control_commands_per_minute: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_calls: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_errands_per_hour: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mcp_allowed_origins: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name_corrections: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_labels: Option<std::collections::BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    whitelist_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    whitelist_forward_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    whitelist_urgent_call: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer_whitelist: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer_contacts: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transfer_others: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ui_password_prompted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_schedule: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_keep: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_include_calls: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_encrypt: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    backup_password: Option<String>,
}

struct StoreState {
    overrides: Overrides,
    current: Arc<RuntimeSettings>,
}

pub struct SettingsStore {
    base: Settings,
    path: PathBuf,
    state: RwLock<StoreState>,
}

impl SettingsStore {
    /// Load env base + optional overrides file (missing/corrupt → defaults).
    pub fn load(base: Settings) -> Arc<Self> {
        let path = Path::new(&base.calls_data_dir)
            .join("config")
            .join("settings.json");
        let mut old_key_moved = false;
        let overrides = match std::fs::read_to_string(&path) {
            Ok(text) => match read_overrides(&text) {
                Ok((o, moved)) => {
                    info!("runtime settings overrides loaded path={}", path.display());
                    old_key_moved = moved;
                    o
                }
                Err(e) => {
                    warn!(
                        "settings.json parse failed path={} ({e}); using env defaults",
                        path.display()
                    );
                    Overrides::default()
                }
            },
            Err(_) => Overrides::default(),
        };
        let (overrides, rt) = Self::settle_overrides(&base, &path, overrides, old_key_moved);
        rt.warn_if_legacy_flow_replaced();
        set_active_ui_password(&rt.ui_password);
        crate::calendar::set_link_secret(&active_ui_token(&base.calls_ui_token));
        Arc::new(Self {
            base,
            path,
            state: RwLock::new(StoreState {
                overrides,
                current: Arc::new(rt),
            }),
        })
    }

    /// A settings file as read (`overrides`) after the changes settings of
    /// older versions need, and the runtime settings it makes. The file at
    /// `path` is written again when one of those changed it.
    fn settle_overrides(base: &Settings, path: &Path, mut overrides: Overrides, old_key_moved: bool) -> (Overrides, RuntimeSettings) {
        // An `auto` from before the Google Calendar connection was removed is
        // `confirm` now, and is saved so.
        if let Some(m) = overrides.appointment_mode.take() {
            overrides.appointment_mode = Some(sanitize_appointment_mode(&m));
        }
        let mut rt = RuntimeSettings::from_base(base);
        let summary_provider = overrides
            .summary_provider
            .clone()
            .unwrap_or_else(|| rt.summary_provider.clone());
        rt.apply(&overrides);
        // Reading settings of older versions (before `gemini_assistant_prompt`
        // existed, when both answer styles shared `gemini_persona_prompt`): keep
        // the saved prompt under the answer style that was active at the time.
        let mut prompt_split = false;
        if overrides.gemini_assistant_prompt.is_none()
            && overrides
                .gemini_persona_prompt
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        {
            prompt_split = true;
            if rt.answer_style_is_assistant() {
                let moved = std::mem::take(&mut rt.gemini_persona_prompt);
                rt.gemini_assistant_prompt = moved.clone();
                overrides.gemini_assistant_prompt = Some(moved);
                overrides.gemini_persona_prompt = Some(String::new());
            } else {
                overrides.gemini_assistant_prompt = Some(String::new());
            }
        }
        // Reading settings of older versions:
        // A value from before the summary service was Gemini or custom (`auto`,
        // none, `deepseek`) is resolved once, here, and saved so: a key saved
        // later does not move a summary the console shows on Gemini. `deepseek`
        // also drops the address it passed over.
        let older = sanitize_summary_provider(&summary_provider);
        if older != "gemini" && older != "custom" {
            overrides.summary_provider = Some(rt.summary_provider.clone());
        }
        if older == "deepseek" {
            overrides.summary_api_base = Some(rt.summary_api_base.clone());
        }
        // A settings file from an older version (its summary key under the old
        // name, a provider value from before, or only a TRTC LLM config key):
        // "another service" without an address gets the one it used, saved now.
        let llm_key_only = rt.summary_api_key.trim().is_empty()
            && crate::summary::api_key_from_llm_config(&rt.trtc_llm_config).is_some();
        let from_older = old_key_moved || (older != "gemini" && older != "custom") || llm_key_only;
        let filled = rt.summary_key_saved
            && from_older
            && fill_old_summary_service(&rt.summary_provider, true, &mut rt.summary_api_base, &mut rt.summary_model);
        if filled {
            overrides.summary_api_base = Some(rt.summary_api_base.clone());
            overrides.summary_model = Some(rt.summary_model.clone());
            // Only a TRTC LLM config key: it is written down as the summary key,
            // so it goes to this service and nowhere else.
            if llm_key_only {
                if let Some(k) = crate::summary::api_key_from_llm_config(&rt.trtc_llm_config) {
                    rt.summary_api_key = k.clone();
                    overrides.summary_api_key = Some(k);
                }
            }
            info!("summary service of an older version written down path={}", path.display());
        }
        if old_key_moved || filled || prompt_split {
            if let Err(e) = write_overrides(path, &overrides) {
                warn!("settings from an older version not saved yet: {e:#}");
            }
        }
        (overrides, rt)
    }

    pub fn snapshot(&self) -> Arc<RuntimeSettings> {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current
            .clone()
    }

    pub fn storage_path(&self) -> &Path {
        &self.path
    }

    /// The language was chosen: saved in the settings (`language` or
    /// `notify_language`), or set in the environment. Until then the console
    /// follows the browser's language and saves it once.
    pub fn language_chosen(&self) -> bool {
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        language_chosen_in(&st.overrides, |k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
    }

    /// The settings file as it is written now (what a backup keeps).
    pub fn overrides_json(&self) -> Vec<u8> {
        let st = self.state.read().unwrap_or_else(|e| e.into_inner());
        serde_json::to_vec_pretty(&st.overrides).unwrap_or_else(|_| b"{}".to_vec())
    }

    /// A restore of the backup items `settings` and / or `keys` ([`combine_settings`]):
    /// `text` is the backup's settings.json. What belongs to this computer
    /// stays: `public_url` and `backup_dir`, never taken from a file. The
    /// [`RETENTION_FIELDS`] only keep or lengthen. Read the way [`Self::load`]
    /// reads a file (the same changes for older versions), saved, then swapped in.
    pub fn replace_from_backup(&self, text: &str, settings: bool, keys: bool, whole_keys: bool) -> Result<Arc<RuntimeSettings>> {
        let inc: Value = serde_json::from_str(text).context("not a settings file")?;
        if !inc.is_object() {
            anyhow::bail!("not a settings file");
        }
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        let cur = serde_json::to_value(&st.overrides)?;
        let now = serde_json::to_value(&*st.current)?;
        let mut merged = combine_settings(&cur, &inc, settings, keys, whole_keys);
        if let Some(m) = merged.as_object_mut() {
            keep_this_computers(m, &cur);
            never_shorter(m, &inc, &cur, &now);
            if m.get("ui_password").is_some_and(filled) {
                m.insert("ui_password_prompted".into(), json!(true));
            }
        }
        let (ov, moved) = read_overrides(&merged.to_string()).context("not a settings file")?;
        let rt = self.swap_in(&mut st, ov, moved)?;
        info!("runtime settings restored from a backup path={} settings={settings} keys={keys}", self.path.display());
        Ok(rt)
    }

    /// The settings file of a backup taken whole (putting back the copy made
    /// before a restore), but for `public_url` and `backup_dir`, which stay.
    pub fn replace_from_text(&self, text: &str) -> Result<Arc<RuntimeSettings>> {
        let mut inc: Value = serde_json::from_str(text).context("not a settings file")?;
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        let cur = serde_json::to_value(&st.overrides)?;
        let Some(m) = inc.as_object_mut() else {
            anyhow::bail!("not a settings file");
        };
        keep_this_computers(m, &cur);
        let (ov, moved) = read_overrides(&inc.to_string()).context("not a settings file")?;
        let rt = self.swap_in(&mut st, ov, moved)?;
        info!("runtime settings replaced from a backup path={}", self.path.display());
        Ok(rt)
    }

    fn swap_in(&self, st: &mut StoreState, ov: Overrides, moved: bool) -> Result<Arc<RuntimeSettings>> {
        let (ov, rt) = Self::settle_overrides(&self.base, &self.path, ov, moved);
        write_overrides(&self.path, &ov)?;
        st.overrides = ov;
        set_active_ui_password(&rt.ui_password);
        crate::calendar::set_link_secret(&active_ui_token(&self.base.calls_ui_token));
        st.current = Arc::new(rt);
        Ok(st.current.clone())
    }

    /// What restoring the item `keys` of the settings file `text` would change:
    /// the console password, and where data goes or who may operate Bageo (a
    /// value of [`CONNECTION_FIELDS`]; a channel's id and tokens do not count).
    /// Worked out the way the restore does it ([`combine_settings`];
    /// `whole_keys` for this computer's own `before_import` copy).
    pub fn keys_change(&self, text: &str, whole_keys: bool) -> KeysChange {
        let Ok(inc) = serde_json::from_str::<Value>(text) else {
            return KeysChange::default();
        };
        let (cur, now) = {
            let st = self.state.read().unwrap_or_else(|e| e.into_inner());
            (serde_json::to_value(&st.overrides), st.current.clone())
        };
        let Ok(cur) = cur else {
            return KeysChange::default();
        };
        let merged = combine_settings(&cur, &inc, false, true, whole_keys);
        let Ok((ov, _)) = read_overrides(&merged.to_string()) else {
            return KeysChange::default();
        };
        let mut after = RuntimeSettings::from_base(&self.base);
        after.apply(&ov);
        let (Ok(a), Ok(b)) = (serde_json::to_value(&*now), serde_json::to_value(&after)) else {
            return KeysChange::default();
        };
        KeysChange {
            password: now.ui_password.trim() != after.ui_password.trim(),
            connections: CONNECTION_FIELDS.iter().any(|f| destination(f, a.get(*f)) != destination(f, b.get(*f))),
        }
    }

    fn apply_patch_locked(&self, st: &mut StoreState, patch: &Value) -> Result<Arc<RuntimeSettings>> {
        let mut ov = st.overrides.clone();
        patch_overrides(&mut ov, patch);
        let mut rt = RuntimeSettings::from_base(&self.base);
        rt.apply(&ov);
        write_overrides(&self.path, &ov)?;

        rt.warn_if_legacy_flow_replaced();
        st.overrides = ov;
        set_active_ui_password(&rt.ui_password);
        crate::calendar::set_link_secret(&active_ui_token(&self.base.calls_ui_token));
        st.current = Arc::new(rt);
        info!(
            "runtime settings updated path={} ai_live={} shaming={} blocklist_len={} callback_whitelist_len={} trtc_sdk_app_id={} trtc_configured={}",
            self.path.display(),
            st.current.ai_live_enabled,
            st.current.shaming_enabled,
            st.current.spam_blocklist.len(),
            st.current.callback_whitelist.len(),
            st.current.trtc_sdk_app_id,
            st.current.trtc_configured()
        );
        Ok(st.current.clone())
    }

    /// Apply a JSON patch from PUT /calls/api/settings, persist, hot-swap.
    /// Persist failure keeps the previous state (error returned to caller).
    pub fn apply_patch(&self, patch: &Value) -> Result<Arc<RuntimeSettings>> {
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        self.apply_patch_locked(&mut st, patch)
    }

    /// GET representation: secrets masked + `*_set` flags; rest verbatim.
    /// Also includes `gmail_configured` (fulfill gate; no secrets).
    pub fn masked_json(&self) -> Value {
        masked(&self.snapshot())
    }

    /// Append a caller number to `spam_blocklist` if not already present; persist + hot-swap.
    /// Storage dedupe only — `blocklist_hit` gate stays always-false (deny removed).
    pub fn append_blocklist_number(&self, number: &str) -> Result<Arc<RuntimeSettings>> {
        let trimmed = number.trim();
        if trimmed.is_empty() {
            return Ok(self.snapshot());
        }
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        let norm = normalize_number(trimmed);
        if st
            .current
            .spam_blocklist
            .iter()
            .any(|x| normalize_number(x) == norm)
        {
            return Ok(st.current.clone());
        }
        let mut list = st.current.spam_blocklist.clone();
        list.push(trimmed.to_string());
        self.apply_patch_locked(&mut st, &json!({ "spam_blocklist": list }))
    }

    /// Remove one phone's saved label from `device_labels` (`settings.json`) and
    /// refresh the in-memory device label table. Returns `Ok(true)` when a saved
    /// label was present and removed.
    pub fn remove_device_label(&self, serial: &str) -> Result<bool> {
        let s = serial.trim();
        if s.is_empty() {
            return Ok(false);
        }
        {
            let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
            if !st.current.device_labels.contains_key(s) {
                return Ok(false);
            }
            let mut labels = st.current.device_labels.clone();
            labels.remove(s);
            self.apply_patch_locked(&mut st, &json!({ "device_labels": labels }))?;
        }
        crate::notify::forget_device_override(s);
        Ok(true)
    }
}

static ACTIVE_UI_PASSWORD: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// The fewest characters a new console password may have.
pub const MIN_UI_PASSWORD_CHARS: usize = 8;

/// A new console password that is too short. Empty is not: it keeps the
/// saved password.
pub fn ui_password_too_short(pw: &str) -> bool {
    let pw = pw.trim();
    !pw.is_empty() && pw.chars().count() < MIN_UI_PASSWORD_CHARS
}

/// The active console password (`ui_password` from settings if set, else `env_token`).
pub fn active_ui_token(env_token: &str) -> String {
    let pw = ACTIVE_UI_PASSWORD
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if !pw.trim().is_empty() {
        return pw.trim().to_string();
    }
    env_token.trim().to_string()
}

fn set_active_ui_password(pw: &str) {
    *ACTIVE_UI_PASSWORD
        .write()
        .unwrap_or_else(|e| e.into_inner()) = pw.trim().to_string();
}

const SECRET_FIELDS: [&str; 14] = [
    "openrouter_api_key",
    "telegram_bot_token",
    "gmail_app_password",
    "openclaw_token",
    "mcp_token",
    "ui_password",
    "trtc_secret_key",
    "trtc_secret_id",
    "trtc_cloud_secret_key",
    "trtc_llm_config",
    "trtc_tts_config",
    "gemini_api_key",
    "summary_api_key",
    "backup_password",
];

fn masked(rt: &RuntimeSettings) -> Value {
    let full = serde_json::to_value(rt).unwrap_or_else(|_| json!({}));
    let mut m: Map<String, Value> = full.as_object().cloned().unwrap_or_default();
    for key in SECRET_FIELDS {
        let raw = m
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        m.insert(key.to_string(), json!(mask_secret(&raw)));
        m.insert(format!("{key}_set"), json!(!raw.trim().is_empty()));
    }
    // Fulfill readiness (no secrets) — UI Settings tab + offline units.
    m.insert("gmail_configured".into(), json!(rt.gmail_configured()));
    m.insert("trtc_configured".into(), json!(rt.trtc_configured()));
    m.insert(
        "trtc_cloud_api_ready".into(),
        json!(rt.trtc_cloud_api_ready()),
    );
    m.insert(
        "trtc_transfer_ready".into(),
        json!(rt.trtc_ai_enabled && rt.trtc_configured()),
    );
    m.insert(
        "gemini_live_ready".into(),
        json!(rt.gemini_live_enabled && !rt.gemini_api_key.trim().is_empty()),
    );
    // Channels: addresses and tokens never leave the server in full.
    m.insert(
        "notify_channels".into(),
        Value::Array(rt.notify_channels.iter().map(crate::notify::Channel::masked).collect()),
    );
    m.insert(
        "notify_kinds".into(),
        json!(crate::notify::KINDS
            .iter()
            .map(|k| json!({
                "kind": k.kind,
                "label": k.label,
                "url": k.url,
                "token": k.token,
                "target": k.target,
                "url_default": k.url_default,
                "app_token": crate::notify::app_token_label(k.kind),
            }))
            .collect::<Vec<_>>()),
    );
    m.insert("default_prompt".into(), json!(rt.default_prompt()));
    // The same with `{owner}` left in, for copying into the editor.
    m.insert(
        "default_prompt_template".into(),
        json!(crate::prompts::default_prompt_template(rt.lang(), rt.answer_style_is_assistant())),
    );
    m.insert(
        "default_prompts".into(),
        json!({
            "assistant": crate::prompts::default_prompt(rt.lang(), true, &rt.owner_name),
            "persona": crate::prompts::default_prompt(rt.lang(), false, &rt.owner_name),
        }),
    );
    m.insert(
        "default_prompt_templates".into(),
        json!({
            "assistant": crate::prompts::default_prompt_template(rt.lang(), true),
            "persona": crate::prompts::default_prompt_template(rt.lang(), false),
        }),
    );
    // Lists the page builds its menus from.
    m.insert(
        "voices".into(),
        json!(crate::voice_preview::VOICES
            .iter()
            .map(|(name, character, male)| json!({"name": name, "character": character, "male": male}))
            .collect::<Vec<_>>()),
    );
    m.insert("ambience_kinds".into(), json!(crate::ambience::KINDS));
    m.insert(
        "sample_lines".into(),
        json!(crate::locale::phrases(rt.lang()).preview),
    );
    m.insert(
        "languages".into(),
        json!(crate::locale::ALL.iter().map(|l| l.code()).collect::<Vec<_>>()),
    );
    // "Main language of each SIM": every country the menu offers, and the SIM
    // countries the phones have reported since the agent started.
    m.insert(
        "countries".into(),
        json!(crate::phone_region::countries().collect::<Vec<_>>()),
    );
    m.insert("seen_sim_countries".into(), json!(crate::phone_region::seen_sims()));
    m.insert(
        "summary_ready".into(),
        json!(
            !rt.summary_api_base.trim().is_empty()
                || !rt.summary_api_key.trim().is_empty()
                || !rt.gemini_api_key.trim().is_empty()
        ),
    );
    Value::Object(m)
}

fn mask_secret(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = t.chars().collect();
    if chars.len() >= 10 {
        let head: String = chars[..3].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    } else {
        "••••".to_string()
    }
}

fn patch_overrides(ov: &mut Overrides, patch: &Value) {
    fn p_string(patch: &Value, key: &str, slot: &mut Option<String>) {
        if let Some(v) = patch.get(key).and_then(|v| v.as_str()) {
            *slot = Some(v.trim().to_string());
        }
    }
    // Secret semantics: "" or absent keeps; `<key>_clear: true` clears.
    fn p_secret(patch: &Value, key: &str, slot: &mut Option<String>) {
        if patch.get(format!("{key}_clear")).and_then(|v| v.as_bool()) == Some(true) {
            *slot = Some(String::new());
            return;
        }
        if let Some(v) = patch.get(key).and_then(|v| v.as_str()) {
            let t = v.trim();
            if !t.is_empty() {
                *slot = Some(t.to_string());
            }
        }
    }
    fn p_bool(patch: &Value, key: &str, slot: &mut Option<bool>) {
        if let Some(v) = patch.get(key).and_then(|v| v.as_bool()) {
            *slot = Some(v);
        }
    }
    fn p_u32(patch: &Value, key: &str, slot: &mut Option<u32>) {
        if let Some(v) = patch.get(key).and_then(|v| v.as_u64()) {
            *slot = Some(v.min(u64::from(u32::MAX)) as u32);
        }
    }
    fn p_f64(patch: &Value, key: &str, slot: &mut Option<f64>) {
        if let Some(v) = patch.get(key).and_then(|v| v.as_f64()) {
            if v.is_finite() {
                *slot = Some(v);
            }
        }
    }
    fn p_f32(patch: &Value, key: &str, slot: &mut Option<f32>) {
        if let Some(v) = patch.get(key).and_then(|v| v.as_f64()) {
            if v.is_finite() {
                *slot = Some(v as f32);
            }
        }
    }
    fn p_list(patch: &Value, key: &str, slot: &mut Option<Vec<String>>) {
        if let Some(arr) = patch.get(key).and_then(|v| v.as_array()) {
            let list: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            *slot = Some(list);
        }
    }

    p_secret(patch, "openrouter_api_key", &mut ov.openrouter_api_key);
    p_string(patch, "openrouter_model", &mut ov.openrouter_model);
    p_bool(patch, "ai_live_enabled", &mut ov.ai_live_enabled);
    p_secret(patch, "telegram_bot_token", &mut ov.telegram_bot_token);
    p_string(patch, "telegram_chat_id", &mut ov.telegram_chat_id);
    p_string(patch, "notify_gateway_url", &mut ov.notify_gateway_url);
    p_string(patch, "notify_gateway_tag", &mut ov.notify_gateway_tag);
    p_bool(patch, "call_notify", &mut ov.call_notify);
    p_string(patch, "gmail_user", &mut ov.gmail_user);
    p_secret(patch, "gmail_app_password", &mut ov.gmail_app_password);
    p_string(patch, "gmail_from_name", &mut ov.gmail_from_name);
    p_string(patch, "openclaw_url", &mut ov.openclaw_url);
    p_secret(patch, "openclaw_token", &mut ov.openclaw_token);
    p_bool(patch, "shaming_enabled", &mut ov.shaming_enabled);
    p_f64(patch, "shaming_multiplier", &mut ov.shaming_multiplier);
    p_string(patch, "shaming_template", &mut ov.shaming_template);
    p_list(patch, "spam_blocklist", &mut ov.spam_blocklist);
    p_bool(
        patch,
        "spam_ai_filter_enabled",
        &mut ov.spam_ai_filter_enabled,
    );
    p_list(patch, "spam_keywords", &mut ov.spam_keywords);
    p_list(patch, "callback_whitelist", &mut ov.callback_whitelist);
    // `reject_numbers` is not written any more: the settings API puts those
    // numbers on the Blacklist ([`reject_entries`]).
    p_bool(patch, "reject_numbers_moved", &mut ov.reject_numbers_moved);
    p_u32(patch, "vm_max_ms", &mut ov.vm_max_ms);
    p_u32(patch, "vm_silence_ms", &mut ov.vm_silence_ms);
    p_f32(patch, "vm_silence_rms", &mut ov.vm_silence_rms);
    p_u32(patch, "vm_confirm_wait_ms", &mut ov.vm_confirm_wait_ms);
    p_u32(patch, "ivr_start_delay_ms", &mut ov.ivr_start_delay_ms);
    if let Some(v) = ov.ivr_start_delay_ms {
        ov.ivr_start_delay_ms = Some(v.min(crate::config::IVR_FIRST_AUDIO_DELAY_CAP_MS));
    }
    p_u32(
        patch,
        "post_greet_ai_delay_ms",
        &mut ov.post_greet_ai_delay_ms,
    );
    p_u32(
        patch,
        "goodbye_hangup_delay_ms",
        &mut ov.goodbye_hangup_delay_ms,
    );
    p_u32(patch, "ivr_reprompt_ms", &mut ov.ivr_reprompt_ms);
    p_u32(patch, "ivr_max_menu_plays", &mut ov.ivr_max_menu_plays);
    p_string(patch, "ivr_goodbye_text", &mut ov.ivr_goodbye_text);
    p_string(patch, "ivr_vm_intro_text", &mut ov.ivr_vm_intro_text);
    p_string(patch, "ivr_vm_confirm_text", &mut ov.ivr_vm_confirm_text);
    p_string(patch, "vm_reply_ack_text", &mut ov.vm_reply_ack_text);
    p_u32(patch, "trtc_sdk_app_id", &mut ov.trtc_sdk_app_id);
    p_secret(patch, "trtc_secret_key", &mut ov.trtc_secret_key);
    p_secret(patch, "trtc_secret_id", &mut ov.trtc_secret_id);
    p_secret(
        patch,
        "trtc_cloud_secret_key",
        &mut ov.trtc_cloud_secret_key,
    );
    p_bool(patch, "trtc_ai_enabled", &mut ov.trtc_ai_enabled);
    p_u32(
        patch,
        "trtc_usersig_expire_secs",
        &mut ov.trtc_usersig_expire_secs,
    );
    p_string(patch, "trtc_ai_region", &mut ov.trtc_ai_region);
    p_string(patch, "trtc_api_host", &mut ov.trtc_api_host);
    p_bool(patch, "trtc_api_intl", &mut ov.trtc_api_intl);
    p_secret(patch, "trtc_llm_config", &mut ov.trtc_llm_config);
    p_secret(patch, "trtc_tts_config", &mut ov.trtc_tts_config);
    p_string(patch, "trtc_stt_language", &mut ov.trtc_stt_language);
    p_u32(
        patch,
        "trtc_ai_max_idle_secs",
        &mut ov.trtc_ai_max_idle_secs,
    );
    p_bool(patch, "trtc_bridge_enabled", &mut ov.trtc_bridge_enabled);
    p_string(patch, "trtc_bridge_url", &mut ov.trtc_bridge_url);
    p_bool(patch, "gemini_live_enabled", &mut ov.gemini_live_enabled);
    p_secret(patch, "gemini_api_key", &mut ov.gemini_api_key);
    p_string(patch, "gemini_live_model", &mut ov.gemini_live_model);
    p_string(patch, "gemini_system_prompt", &mut ov.gemini_system_prompt);
    p_string(patch, "gemini_voice_name", &mut ov.gemini_voice_name);
    p_string(patch, "reject_voice_name", &mut ov.reject_voice_name);
    p_string(patch, "reject_mode", &mut ov.reject_mode);
    if let Some(v) = ov.reject_mode.take() {
        ov.reject_mode = Some(sanitize_reject_mode(&v));
    }
    p_secret(patch, "summary_api_key", &mut ov.summary_api_key);
    // Reading requests of older versions: the key's former name.
    if patch.get("summary_api_key").is_none() && patch.get("summary_api_key_clear").is_none() {
        p_secret(patch, "deepseek_api_key", &mut ov.summary_api_key);
    }
    p_bool(patch, "owner_status_enabled", &mut ov.owner_status_enabled);
    p_bool(patch, "gemini_persona_mode", &mut ov.gemini_persona_mode);
    p_string(patch, "gemini_persona_prompt", &mut ov.gemini_persona_prompt);
    p_string(patch, "gemini_assistant_prompt", &mut ov.gemini_assistant_prompt);
    p_bool(patch, "answer_unknown_callers", &mut ov.answer_unknown_callers);
    p_bool(patch, "persona_stall_spam", &mut ov.persona_stall_spam);
    p_bool(patch, "scam_review_without_keywords", &mut ov.scam_review_without_keywords);
    p_bool(patch, "gemini_server_vad", &mut ov.gemini_server_vad);
    p_bool(patch, "gemini_barge_in", &mut ov.gemini_barge_in);
    p_bool(patch, "gemini_ring_prewarm", &mut ov.gemini_ring_prewarm);
    p_string(patch, "ambience_kind", &mut ov.ambience_kind);
    p_u32(patch, "ambience_level", &mut ov.ambience_level);
    if ov.ambience_kind.is_some() || ov.ambience_level.is_some() {
        let (k, l) = sanitize_ambience(
            ov.ambience_kind.as_deref().unwrap_or("off"),
            ov.ambience_level.unwrap_or(2),
        );
        if ov.ambience_kind.is_some() {
            ov.ambience_kind = Some(k);
        }
        if ov.ambience_level.is_some() {
            ov.ambience_level = Some(l);
        }
    }
    p_string(patch, "gemini_thinking_level", &mut ov.gemini_thinking_level);
    if let Some(v) = ov.gemini_thinking_level.take() {
        ov.gemini_thinking_level = Some(sanitize_thinking_level(&v));
    }
    p_u32(patch, "answer_ring_ms", &mut ov.answer_ring_ms);
    if let Some(v) = ov.answer_ring_ms {
        ov.answer_ring_ms = Some(v.min(ANSWER_RING_MS_CEIL));
    }
    p_u32(patch, "persona_stall_max_min", &mut ov.persona_stall_max_min);
    if let Some(v) = ov.persona_stall_max_min {
        ov.persona_stall_max_min = Some(v.clamp(STALL_MAX_MIN_FLOOR, STALL_MAX_MIN_CEIL));
    }
    p_u32(patch, "persona_normal_max_min", &mut ov.persona_normal_max_min);
    if let Some(v) = ov.persona_normal_max_min {
        ov.persona_normal_max_min = Some(v.clamp(STALL_MAX_MIN_FLOOR, STALL_MAX_MIN_CEIL));
    }
    p_u32(patch, "retention_days", &mut ov.retention_days);
    if let Some(v) = ov.retention_days {
        ov.retention_days = Some(v.clamp(RETENTION_DAYS_MIN, RETENTION_DAYS_MAX));
    }
    p_string(patch, "gemini_stall_model", &mut ov.gemini_stall_model);
    p_string(patch, "language", &mut ov.language);
    if let Some(v) = ov.language.take() {
        ov.language = Some(sanitize_language(&v));
    }
    p_string(patch, "notify_language", &mut ov.notify_language);
    if let Some(v) = ov.notify_language.take() {
        ov.notify_language = Some(sanitize_notify_language(&v));
    }
    if let Some(map) = patch.get("sim_languages").and_then(|v| v.as_object()) {
        ov.sim_languages = Some(sanitize_sim_languages(
            map.iter()
                .filter_map(|(k, v)| Some((k.as_str(), v.as_str()?))),
        ));
    }
    p_string(patch, "owner_name", &mut ov.owner_name);
    if let Some(v) = ov.owner_name.take() {
        ov.owner_name = Some(v.chars().filter(|c| !c.is_control()).take(40).collect());
    }
    p_list(patch, "owner_aliases", &mut ov.owner_aliases);
    if let Some(v) = ov.owner_aliases.take() {
        ov.owner_aliases = Some(clean_owner_aliases(&v, ov.owner_name.as_deref().unwrap_or("")));
    }
    p_string(patch, "answer_style", &mut ov.answer_style);
    if let Some(v) = ov.answer_style.take() {
        ov.answer_style = Some(sanitize_answer_style(&v));
    }
    if ov.gemini_assistant_prompt.is_none()
        && (patch.get("answer_style").is_some()
            || patch.get("gemini_persona_prompt").is_some()
            || patch.get("gemini_assistant_prompt").is_some())
    {
        ov.gemini_assistant_prompt = Some(String::new());
    }
    if let Some(v) = patch.get("knowledge_notes").and_then(|v| v.as_str()) {
        ov.knowledge_notes = Some(crate::prompts::sanitize_knowledge(v));
    }
    p_bool(
        patch,
        "reply_in_caller_language",
        &mut ov.reply_in_caller_language,
    );
    p_string(patch, "country_code", &mut ov.country_code);
    if let Some(v) = ov.country_code.take() {
        ov.country_code = Some(digits_only(&v));
    }
    p_string(patch, "summary_api_base", &mut ov.summary_api_base);
    p_string(patch, "summary_model", &mut ov.summary_model);
    p_string(patch, "summary_provider", &mut ov.summary_provider);
    if let Some(v) = ov.summary_provider.take() {
        ov.summary_provider = Some(sanitize_summary_provider(&v));
    }
    p_string(patch, "public_url", &mut ov.public_url);
    if let Some(v) = ov.public_url.take() {
        ov.public_url = Some(sanitize_public_url(&v));
    }
    p_string(patch, "mdns_name", &mut ov.mdns_name);
    p_bool(patch, "phone_wifi", &mut ov.phone_wifi);
    if let Some(v) = ov.mdns_name.take() {
        ov.mdns_name = Some(crate::mdns::parse_name_setting(&v));
    }
    p_bool(patch, "calendar_suggest", &mut ov.calendar_suggest);
    p_string(patch, "calendar_app", &mut ov.calendar_app);
    if let Some(v) = ov.calendar_app.take() {
        ov.calendar_app = Some(crate::calendar::sanitize_app(&v));
    }
    p_string(patch, "appointment_mode", &mut ov.appointment_mode);
    if let Some(v) = ov.appointment_mode.take() {
        ov.appointment_mode = Some(sanitize_appointment_mode(&v));
    }
    p_bool(patch, "appointment_sms", &mut ov.appointment_sms);
    p_string(patch, "appointment_reply", &mut ov.appointment_reply);
    if let Some(v) = ov.appointment_reply.take() {
        ov.appointment_reply = Some(crate::appointments::sanitize_reply_setting(&v));
    }
    p_string(patch, "errand_identity", &mut ov.errand_identity);
    if let Some(v) = ov.errand_identity.take() {
        ov.errand_identity = Some(sanitize_errand_identity(&v));
    }
    p_bool(patch, "telegram_control", &mut ov.telegram_control);
    p_list(patch, "telegram_control_users", &mut ov.telegram_control_users);
    if let Some(v) = ov.telegram_control_users.take() {
        ov.telegram_control_users = Some(sanitize_sender_ids(&v));
    }
    p_u32(patch, "control_errands_per_hour", &mut ov.control_errands_per_hour);
    if let Some(v) = ov.control_errands_per_hour.take() {
        ov.control_errands_per_hour = Some(v.min(30));
    }
    p_u32(patch, "control_commands_per_minute", &mut ov.control_commands_per_minute);
    if let Some(v) = ov.control_commands_per_minute.take() {
        ov.control_commands_per_minute = Some(v.clamp(1, 120));
    }
    p_bool(patch, "mcp_enabled", &mut ov.mcp_enabled);
    p_secret(patch, "mcp_token", &mut ov.mcp_token);
    p_string(patch, "mcp_calls", &mut ov.mcp_calls);
    if let Some(v) = ov.mcp_calls.take() {
        ov.mcp_calls = Some(sanitize_mcp_calls(&v));
    }
    p_u32(patch, "mcp_errands_per_hour", &mut ov.mcp_errands_per_hour);
    if let Some(v) = ov.mcp_errands_per_hour.take() {
        ov.mcp_errands_per_hour = Some(v.min(30));
    }
    p_list(patch, "mcp_allowed_origins", &mut ov.mcp_allowed_origins);
    if let Some(v) = ov.mcp_allowed_origins.take() {
        ov.mcp_allowed_origins = Some(sanitize_origins(&v));
    }
    p_list(patch, "name_corrections", &mut ov.name_corrections);
    if let Some(map) = patch.get("device_labels").and_then(|v| v.as_object()) {
        ov.device_labels = Some(
            map.iter()
                .filter_map(|(k, v)| Some((k.trim().to_string(), v.as_str()?.trim().to_string())))
                .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                .map(|(k, v)| (k, v.chars().filter(|c| !c.is_control()).take(40).collect()))
                .take(50)
                .collect(),
        );
    }
    if let Some(v) = patch.get("notify_channels") {
        if let Ok(list) = serde_json::from_value::<Vec<crate::notify::Channel>>(v.clone()) {
            let stored = ov.notify_channels.clone().unwrap_or_default();
            ov.notify_channels = Some(crate::notify::merge_channels(&stored, list));
        }
    }
    if let Some(v) = patch.get("owner_status_text").and_then(|v| v.as_str()) {
        ov.owner_status_text = Some(crate::gemini_live::sanitize_owner_status(v));
    }
    p_string(patch, "whitelist_mode", &mut ov.whitelist_mode);
    if let Some(v) = ov.whitelist_mode.take() {
        ov.whitelist_mode = Some(sanitize_whitelist_mode(&v));
    }
    p_string(patch, "whitelist_forward_number", &mut ov.whitelist_forward_number);
    if let Some(v) = ov.whitelist_forward_number.take() {
        ov.whitelist_forward_number = Some(v.chars().filter(|c| !c.is_control()).take(32).collect());
    }
    p_bool(patch, "whitelist_urgent_call", &mut ov.whitelist_urgent_call);
    p_bool(patch, "transfer_whitelist", &mut ov.transfer_whitelist);
    p_bool(patch, "transfer_contacts", &mut ov.transfer_contacts);
    p_bool(patch, "transfer_others", &mut ov.transfer_others);
    p_secret(patch, "ui_password", &mut ov.ui_password);
    p_bool(patch, "ui_password_prompted", &mut ov.ui_password_prompted);
    if ov.ui_password.as_ref().is_some_and(|s| !s.is_empty()) {
        ov.ui_password_prompted = Some(true);
    }
    p_string(patch, "backup_schedule", &mut ov.backup_schedule);
    if let Some(v) = ov.backup_schedule.take() {
        ov.backup_schedule = Some(sanitize_backup_schedule(&v));
    }
    p_string(patch, "backup_dir", &mut ov.backup_dir);
    if let Some(v) = ov.backup_dir.take() {
        ov.backup_dir = Some(sanitize_backup_dir(&v));
    }
    p_u32(patch, "backup_keep", &mut ov.backup_keep);
    if let Some(v) = ov.backup_keep.take() {
        ov.backup_keep = Some(v.clamp(1, 100));
    }
    p_bool(patch, "backup_include_calls", &mut ov.backup_include_calls);
    p_bool(patch, "backup_encrypt", &mut ov.backup_encrypt);
    p_secret(patch, "backup_password", &mut ov.backup_password);
}

fn split_list(spec: &str) -> Vec<String> {
    spec.split(|c: char| c == ',' || c == ';' || c == '\n')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Strip spaces/dashes for legacy blocklist storage dedupe.
fn normalize_number(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect()
}

/// Blacklist removed — always false (callers proceed to AI/VM menu).
/// Outbound dial gating uses `callback::callback_whitelist_allows`, not this.
pub fn blocklist_hit(_list: &[String], _caller: &str) -> bool {
    false
}

/// The country each language is mainly spoken in (ISO 3166 alpha-2): where a
/// number without a country code is dialled when no SIM or setting says.
fn language_country(lang: crate::locale::Lang) -> &'static str {
    use crate::locale::Lang;
    match lang {
        Lang::ZhTw => "tw",
        Lang::ZhCn => "cn",
        Lang::En => "us",
        Lang::Ja => "jp",
        Lang::Ko => "kr",
        Lang::Es => "es",
        Lang::Fr => "fr",
        Lang::De => "de",
        Lang::Pt => "pt",
    }
}

/// `public_url` as stored: only a web address (`http://` or `https://`) is
/// kept. Anything else (`javascript:`, `data:`, a bare host) is cleared, so the
/// console never shows or opens a link that runs a script.
pub fn sanitize_public_url(s: &str) -> String {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let ok = (lower.starts_with("http://") && t.len() > "http://".len())
        || (lower.starts_with("https://") && t.len() > "https://".len());
    if ok && !t.chars().any(|c| c.is_control() || c.is_whitespace()) {
        t.to_string()
    } else {
        String::new()
    }
}

/// A phone number as the national digits the phones report. With Taiwan's
/// country code (`886`): `+886 2 5550-1234`, `886255501234`, `00886255501234`
/// and `(02)5550-1234` all → `0255501234`. With another country code the
/// international prefix is dropped (`+44 20 7946 0018` → `2079460018`); without
/// one the digits stay as written. Matching ignores leading zeros
/// ([`same_number`]), so the national trunk `0` does not matter.
pub fn national_digits(s: &str) -> String {
    national_digits_for(s, &crate::locale::country_code())
}

pub fn national_digits_for(s: &str, cc: &str) -> String {
    let mut d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if cc.is_empty() {
        return d;
    }
    if d.starts_with(&format!("00{cc}")) {
        d.drain(..2);
    }
    let international = s.trim_start().starts_with('+') || d.len() >= cc.len() + 8;
    if d.starts_with(cc) && international {
        // Taiwan numbers are written with the trunk 0; elsewhere it varies.
        d.replace_range(..cc.len(), if cc == "886" { "0" } else { "" });
    }
    d
}

fn without_leading_zeros(d: &str) -> &str {
    d.trim_start_matches('0')
}

/// Two national numbers are the same line (a leading trunk `0` or not).
fn same_number(a: &str, b: &str) -> bool {
    a == b || without_leading_zeros(a) == without_leading_zeros(b)
}

/// `owner_aliases` as stored: each name without control characters, trimmed,
/// at most 40 characters; blank ones, repeats and the owner's own name dropped
/// (any case); at most [`crate::prompts::OWNER_ALIASES_MAX`].
fn clean_owner_aliases(list: &[String], owner: &str) -> Vec<String> {
    let mut seen = vec![owner.trim().to_lowercase()];
    let mut out = Vec::new();
    for a in list {
        let a: String = a.chars().filter(|c| !c.is_control()).collect();
        let a: String = a.trim().chars().take(40).collect();
        let a = a.trim().to_string();
        let key = a.to_lowercase();
        if a.is_empty() || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(a);
        if out.len() == crate::prompts::OWNER_ALIASES_MAX {
            break;
        }
    }
    out
}

/// One blocked number: the number and why, as separate fields. Settings saved
/// before stored one line `number reason`; those still load
/// ([`RejectEntryRepr`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RejectEntryRepr")]
pub struct RejectEntry {
    /// National digits (`0255501234`), or a prefix ending in `*` (`0800*`).
    pub number: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RejectEntryRepr {
    Line(String),
    Entry {
        #[serde(default)]
        number: String,
        #[serde(default)]
        reason: String,
    },
}

impl From<RejectEntryRepr> for RejectEntry {
    fn from(r: RejectEntryRepr) -> Self {
        let (number, reason) = match r {
            RejectEntryRepr::Line(line) => {
                let (n, note) = split_reject_entry(&line);
                (n.to_string(), note.to_string())
            }
            RejectEntryRepr::Entry { number, reason } => (number, reason),
        };
        RejectEntry {
            number: clean_reject_number(&number),
            reason: reason.trim().to_string(),
        }
    }
}

/// Stored form of a blocked number: national digits (`+886 2-5550-1234` → `0255501234`),
/// a trailing `*` kept. Empty when there are no digits.
pub fn clean_reject_number(raw: &str) -> String {
    let raw = raw.trim();
    let d = national_digits(raw.trim_end_matches('*'));
    if d.is_empty() {
        String::new()
    } else if raw.ends_with('*') {
        format!("{d}*")
    } else {
        d
    }
}

/// Rows without a number dropped, the same number kept once (first reason wins).
fn clean_reject_list(list: Vec<RejectEntry>) -> Vec<RejectEntry> {
    let mut out: Vec<RejectEntry> = Vec::new();
    for e in list {
        let e = RejectEntry::from(RejectEntryRepr::Entry {
            number: e.number,
            reason: e.reason,
        });
        if !e.number.is_empty() && !out.iter().any(|o| o.number == e.number) {
            out.push(e);
        }
    }
    out
}

/// A one-line blocked-number entry (older settings): the number (digits, spaces,
/// `+-()`, trailing `*`) and the note after it (`0255501234 My bank` →
/// (`0255501234`, `My bank`)).
pub fn split_reject_entry(entry: &str) -> (&str, &str) {
    let e = entry.trim();
    let cut = e
        .find(|c: char| !(c.is_ascii_digit() || c.is_whitespace() || "+-()*".contains(c)))
        .unwrap_or(e.len());
    (e[..cut].trim(), e[cut..].trim())
}

/// Fewest digits a `…*` entry needs after its leading zeros: `02*` / `080*`
/// would turn whole areas away, and `0000*` everyone.
const REJECT_PREFIX_MIN_DIGITS: usize = 3;

/// Blocked numbers sent the old way (`reject_numbers`: rows `{number,
/// reason}` or one-line strings), cleaned; anything else: none.
pub fn reject_entries(v: &Value) -> Vec<RejectEntry> {
    serde_json::from_value::<Vec<RejectEntry>>(v.clone()).map(clean_reject_list).unwrap_or_default()
}

/// The caller is turned away: on the Contacts Blacklist (`kind == "blacklist"`),
/// or in `list` (blocked numbers not moved onto it yet): the same national
/// number, or it starts with an entry ending in `*`. Hidden / garbled numbers
/// never match.
pub fn reject_number_hit(list: &[RejectEntry], caller: &str) -> bool {
    reject_number_hit_on(list, caller, None)
}

/// [`reject_number_hit`] for a call that came in on the SIM of country `sim`
/// (ISO 3166 alpha-2): a number without a country code reads as dialled there
/// (`03-1234-5678` on a Japanese SIM is `+81 3 1234 5678`), as the Contacts
/// Blacklist reads it too. None: the owner's country.
pub fn reject_number_hit_on(list: &[RejectEntry], caller: &str, sim: Option<&str>) -> bool {
    reject_entry_for(list, caller, sim).is_some()
        || crate::contacts::is_blacklist(caller, sim).is_some()
}

/// Why the caller's number is on the blocked-numbers list, when a reason was given.
pub fn reject_reason(list: &[RejectEntry], caller: &str) -> Option<String> {
    reject_reason_on(list, caller, None)
}

/// [`reject_reason`] for a call that came in on the SIM of country `sim`
/// ([`reject_number_hit_on`]).
pub fn reject_reason_on(list: &[RejectEntry], caller: &str, sim: Option<&str>) -> Option<String> {
    if let Some(r) = reject_entry_for(list, caller, sim)
        .map(|e| e.reason.trim().to_string())
        .filter(|r| !r.is_empty())
    {
        return Some(r);
    }
    crate::contacts::is_blacklist(caller, sim).and_then(|c| {
        let note = c.note.trim();
        // A name that is only the number (or repeats the note, as moved-over
        // blocked numbers do) says nothing new.
        let name = c.name.trim();
        let name = if name.is_empty()
            || name.eq_ignore_ascii_case(note)
            || name == caller.trim()
            || crate::contacts::same_number(name, caller, sim)
        {
            ""
        } else {
            name
        };
        match (name.is_empty(), note.is_empty()) {
            (false, false) => Some(format!("{name} · {note}")),
            (true, false) => Some(note.to_string()),
            (false, true) => Some(name.to_string()),
            (true, true) => None,
        }
    })
}

/// The country calling code a call on the SIM of country `sim` reads numbers
/// in (the caller ID and the owner's entries); without one, the owner's.
fn reading_code(sim: Option<&str>) -> String {
    sim
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(crate::phone_region::calling_code)
        .map(str::to_string)
        .unwrap_or_else(crate::locale::country_code)
}

/// Whether a blocked-number entry can stand: a number, or a prefix ending in
/// `*` with at least [`REJECT_PREFIX_MIN_DIGITS`] digits after its leading
/// zeros (`0800*`, `0912*`; not `080*`, `0008*`, `0000*`).
pub fn reject_prefix_ok(entry: &str) -> bool {
    match entry.trim().strip_suffix('*') {
        Some(prefix) => without_leading_zeros(&national_digits(prefix)).len() >= REJECT_PREFIX_MIN_DIGITS,
        None => true,
    }
}

/// `caller` is the blocked `entry` read as dialled on the SIM of country
/// `sim` (None: the owner's country): the same national number, or, for an
/// entry ending in `*`, it starts with those digits. Hidden / garbled
/// numbers never match.
pub fn reject_entry_hit(entry: &str, caller: &str, sim: Option<&str>) -> bool {
    entry_hit(entry, caller, &reading_code(sim))
}

fn entry_hit(entry: &str, caller: &str, cc: &str) -> bool {
    let national_digits = |s: &str| national_digits_for(s, cc);
    let c = national_digits(caller);
    if c.len() < 5 {
        return false;
    }
    match entry.trim().strip_suffix('*') {
        Some(prefix) => {
            let p = national_digits(prefix);
            // Too few digits (an empty one would match everyone): nobody.
            let significant = without_leading_zeros(&p);
            significant.len() >= REJECT_PREFIX_MIN_DIGITS
                && (c.starts_with(&p) || without_leading_zeros(&c).starts_with(significant))
        }
        None => {
            let n = national_digits(entry);
            n.len() >= 5 && same_number(&n, &c)
        }
    }
}

fn reject_entry_for<'a>(list: &'a [RejectEntry], caller: &str, sim: Option<&str>) -> Option<&'a RejectEntry> {
    let cc = reading_code(sim);
    list.iter().find(|e| entry_hit(&e.number, caller, &cc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_masked_setting_is_a_key_of_backups() {
        for f in SECRET_FIELDS {
            assert!(is_key_field(f), "{f}");
        }
    }

    #[test]
    fn settings_named_like_keys_or_addresses_go_with_the_keys() {
        let rt = RuntimeSettings::from_base(&Settings::from_env());
        let v = serde_json::to_value(&rt).unwrap();
        let looks = ["url", "base", "endpoint", "webhook", "token", "key", "secret", "password", "chat_id", "forward"];
        let names: Vec<&String> = v.as_object().unwrap().keys().collect();
        for name in &names {
            if looks.iter().any(|w| name.contains(w)) {
                assert!(is_key_field(name) || KEY_LOOKALIKES.contains(&name.as_str()), "{name} is not in the backup's keys");
            }
        }
        for f in KEY_LOOKALIKES {
            assert!(!is_key_field(f) && names.iter().any(|n| n.as_str() == f), "{f}");
        }
        for f in CONNECTION_FIELDS {
            assert!(names.iter().any(|n| n.as_str() == f), "{f} is no setting");
        }
    }

    #[test]
    fn numbers_match_across_countries() {
        // United Kingdom: the list has the national form, the phone reports +44…
        assert_eq!(national_digits_for("+44 20 7946 0018", "44"), "2079460018");
        assert_eq!(national_digits_for("020 7946 0018", "44"), "02079460018");
        assert!(same_number("2079460018", "02079460018"));
        // United States: no trunk digit.
        assert_eq!(national_digits_for("+1 (202) 555-0143", "1"), "2025550143");
        assert_eq!(national_digits_for("202-555-0143", "1"), "2025550143");
        // No country code set: digits as written.
        assert_eq!(national_digits_for("+81 90-1234-5678", ""), "819012345678");
        assert!(!same_number("2079460018", "2079460019"));
        // Taiwan keeps its trunk 0.
        assert_eq!(national_digits_for("+886 912-345-678", "886"), "0912345678");
    }

    fn entry(number: &str, reason: &str) -> RejectEntry {
        RejectEntry::from(RejectEntryRepr::Entry {
            number: number.to_string(),
            reason: reason.to_string(),
        })
    }

    #[test]
    fn reject_numbers_match_however_the_number_is_written() {
        let list = vec![
            entry("02-5550-1234", "安信（銀行要確認事情）"),
            entry("+886 912-345-678", ""),
            entry("0800*", "免付費推銷"),
            entry("02*", ""),
            entry("*", ""),
            entry("0000*", ""),
            entry("0008*", ""),
        ];
        // Stored as plain national digits.
        assert_eq!(list[0].number, "0255501234");
        assert_eq!(list[1].number, "0912345678");
        assert_eq!(list[2].number, "0800*");
        for caller in ["0255501234", "+886255501234", "886255501234", "(02)5550-1234", "00886 2 5550 1234"] {
            assert!(reject_number_hit(&list, caller), "{caller}");
        }
        assert!(reject_number_hit(&list, "0912345678"));
        assert!(reject_number_hit(&list, "0800123456"));
        // Prefixes with fewer than 3 digits after their zeros (`0000*` would
        // match everyone) and a bare `*` turn nobody away.
        assert!(!reject_number_hit(&list, "0223456789"));
        assert!(!reject_number_hit(&list, "0255501235"));
        assert!(!reject_number_hit(&list, "0008123456"));
        assert!(!reject_number_hit(&list, "0000123456"));
        for (prefix, ok) in [("0800*", true), ("0912*", true), ("123*", true), ("080*", false), ("0008*", false), ("0000*", false), ("02*", false)] {
            assert_eq!(reject_prefix_ok(prefix), ok, "{prefix}");
        }
        // Hidden / unknown callers never match.
        for caller in ["", "unknown", "private", "anonymous"] {
            assert!(!reject_number_hit(&list, caller), "{caller}");
        }
        assert!(!reject_number_hit(&[], "0255501234"));
        assert_eq!(reject_reason(&list, "+886255501234").as_deref(), Some("安信（銀行要確認事情）"));
        assert_eq!(reject_reason(&list, "0912345678"), None);
        assert_eq!(national_digits("+886 912 345 678"), "0912345678");
        // A local number that merely contains 886 stays as it is.
        assert_eq!(national_digits("0288612345"), "0288612345");
        // A reason that starts with digits stays the reason (the old one-line
        // form would have glued 2024 onto the number).
        let e = entry("0255501234", "2024 欠款通知");
        assert_eq!((e.number.as_str(), e.reason.as_str()), ("0255501234", "2024 欠款通知"));
    }

    #[test]
    fn reject_numbers_load_old_lines_and_new_rows() {
        // Saved by older versions: one line per number. Saved from now on: {number, reason}.
        let old: Vec<RejectEntry> =
            serde_json::from_value(json!(["0255501234 推銷電話"])).unwrap();
        let new: Vec<RejectEntry> = serde_json::from_value(json!([
            { "number": "+886 2 5550 1234", "reason": "推銷電話" }
        ]))
        .unwrap();
        assert_eq!(old, new);
        assert_eq!(old[0].number, "0255501234");
        assert_eq!(
            serde_json::to_value(&new).unwrap(),
            json!([{ "number": "0255501234", "reason": "推銷電話" }])
        );
    }

    #[test]
    fn reject_numbers_are_read_but_no_longer_written() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-settings-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join("config")).expect("temp settings dir");
        // A settings file of an older version keeps its list.
        std::fs::write(
            dir.join("config").join("settings.json"),
            r#"{"reject_numbers": [{"number": "02-5550-1234", "reason": " 安信 "}, "0912345678"]}"#,
        )
        .unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        let rt = store.snapshot();
        assert_eq!(rt.reject_numbers, vec![entry("0255501234", "安信"), entry("0912345678", "")]);
        assert!(reject_number_hit(rt.pending_rejects(), "+886255501234"));
        assert_eq!(reject_reason(rt.pending_rejects(), "0255501234").as_deref(), Some("安信"));
        // The settings API does not write it (those numbers go onto the Blacklist).
        let rt = store.apply_patch(&json!({ "reject_numbers": [{ "number": "0800*" }] })).unwrap();
        assert_eq!(rt.reject_numbers.len(), 2);
        // Once moved, the list stays as it was but counts no more.
        let rt = store.apply_patch(&json!({ "reject_numbers_moved": true })).unwrap();
        assert_eq!(rt.reject_numbers.len(), 2);
        assert!(rt.pending_rejects().is_empty());
        let again = SettingsStore::load(base).snapshot();
        assert!(again.reject_numbers_moved);
        assert_eq!(again.reject_numbers.len(), 2);
        let saved = std::fs::read_to_string(dir.join("config").join("settings.json")).unwrap();
        assert!(saved.contains("0255501234"), "{saved}");
        assert_eq!(
            reject_entries(&json!(["0255501234 推銷", { "number": " " }, { "number": "0255501234" }])),
            vec![entry("0255501234", "推銷")]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reject_mode_defaults_to_decline_and_keeps_only_known_values() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-settings-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().reject_mode, "decline");
        assert!(store.snapshot().reject_declines());
        let rt = store.apply_patch(&json!({ "reject_mode": " Announce " })).unwrap();
        assert_eq!(rt.reject_mode, "announce");
        assert!(!rt.reject_declines());
        // Saved: a restart keeps it; an unknown value is decline.
        assert_eq!(SettingsStore::load(base.clone()).snapshot().reject_mode, "announce");
        let rt = store.apply_patch(&json!({ "reject_mode": "hang-up" })).unwrap();
        assert_eq!(rt.reject_mode, "decline");
        assert_eq!(sanitize_reject_mode(""), "decline");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocklist_disabled_always_miss() {
        let list = vec!["+886900111222".to_string(), "+8862*".to_string()];
        assert!(!blocklist_hit(&list, "+886 900-111-222"));
        assert!(!blocklist_hit(&list, "+886212345678"));
        assert!(!blocklist_hit(&list, "+886900111223"));
        assert!(!blocklist_hit(&list, ""));
        assert!(!blocklist_hit(&[], "+886900111222"));
    }

    #[test]
    fn append_blocklist_number_dedupes() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-settings-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base);
        let rt = store.append_blocklist_number("+886900111222").unwrap();
        assert_eq!(rt.spam_blocklist.len(), 1);
        let rt2 = store.append_blocklist_number("+886900111222").unwrap();
        assert_eq!(rt2.spam_blocklist.len(), 1);
        let rt3 = store.append_blocklist_number("+886900111223").unwrap();
        assert_eq!(rt3.spam_blocklist.len(), 2);
        let rt4 = store
            .apply_patch(&json!({ "spam_keywords": ["貸款", "測試詞"] }))
            .unwrap();
        assert_eq!(
            rt4.spam_keywords,
            vec!["貸款".to_string(), "測試詞".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn secret_mask_shapes() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("short"), "••••");
        assert_eq!(mask_secret("sk-or-v1-abcdefwxyz"), "sk-…wxyz");
    }

    #[test]
    fn gmail_configured_and_masked_flags() {
        let mut rt = RuntimeSettings::from_base(&Settings::from_env());
        rt.gmail_user.clear();
        rt.gmail_app_password.clear();
        assert!(!rt.gmail_configured());
        let empty = masked(&rt);
        assert_eq!(empty.get("gmail_configured"), Some(&json!(false)));
        // Secrets stay masked / never raw in GET payload.
        assert_eq!(empty.get("gmail_app_password"), Some(&json!("")));

        rt.gmail_user = "bot@gmail.com".into();
        rt.gmail_app_password = "abcdefghijklmnop".into();
        assert!(rt.gmail_configured());
        let ready = masked(&rt);
        assert_eq!(ready.get("gmail_configured"), Some(&json!(true)));
        let masked_pw = ready
            .get("gmail_app_password")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(!masked_pw.is_empty());
        assert!(!masked_pw.contains("abcdefghijklmnop"));
        assert_eq!(ready.get("gmail_app_password_set"), Some(&json!(true)));
    }

    #[test]
    fn owner_status_patch_hot_swaps() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-owner-status-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base);
        assert!(!store.snapshot().owner_status_enabled);
        assert!(store.snapshot().owner_status_note().is_empty());
        let rt = store
            .apply_patch(&json!({
                // The older relay-assistant mode (forced first line).
                // (Kept for Traditional Chinese; other languages use the current flow.)
                "gemini_persona_mode": false,
                "language": "zh-TW",
                "owner_status_enabled": true,
                "owner_status_text": "  包裹的錢放在管理室外送桌了  "
            }))
            .unwrap();
        assert!(rt.owner_status_enabled);
        assert_eq!(rt.owner_status_text, "包裹的錢放在管理室外送桌了");
        assert_eq!(rt.owner_status_note(), "包裹的錢放在管理室外送桌了");
        let prompt = rt.gemini_system_prompt_for_call();
        assert!(prompt.contains("包裹的錢放在管理室外送桌了"));
        assert!(prompt.starts_with(crate::gemini_live::OWNER_STATUS_BEGIN));
        assert!(prompt.contains("必須講出原文") || prompt.contains("第一句"));
        let cleared = store
            .apply_patch(&json!({
                "owner_status_enabled": false,
                "owner_status_text": ""
            }))
            .unwrap();
        assert!(!cleared.owner_status_enabled);
        assert!(cleared.owner_status_note().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transfer_switches_keep_an_older_file_on_the_whitelist_only_and_save() {
        let dir = std::env::temp_dir().join(format!("bageo-call-transfer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("config")).expect("temp settings dir");
        // An older version's file: the mode and number, none of the new switches.
        std::fs::write(
            dir.join("config").join("settings.json"),
            r#"{"whitelist_mode":"emergency","whitelist_forward_number":"+15555550100"}"#,
        )
        .unwrap();
        let base = || {
            let mut b = Settings::from_env();
            b.calls_data_dir = dir.to_string_lossy().to_string();
            b
        };
        let store = SettingsStore::load(base());
        let rt = store.snapshot();
        assert_eq!(rt.whitelist_mode, "emergency");
        assert!(rt.transfer_whitelist && !rt.transfer_contacts && !rt.transfer_others);
        use crate::urgent_call::{transfer_who, TransferWho};
        assert_eq!(transfer_who(&rt, Some("whitelist"), false), Some(TransferWho::Whitelist));
        assert_eq!(transfer_who(&rt, Some(""), false), None);
        assert_eq!(transfer_who(&rt, None, false), None);
        let shown = store.masked_json();
        assert_eq!(shown["transfer_whitelist"], json!(true));
        assert_eq!(shown["transfer_others"], json!(false));
        let rt = store
            .apply_patch(&json!({ "transfer_whitelist": false, "transfer_contacts": true, "transfer_others": true }))
            .unwrap();
        assert!(!rt.transfer_whitelist && rt.transfer_contacts && rt.transfer_others);
        let shown = store.masked_json();
        assert_eq!(
            (&shown["transfer_whitelist"], &shown["transfer_contacts"], &shown["transfer_others"]),
            (&json!(false), &json!(true), &json!(true))
        );
        // Saved: read back the same after a restart.
        let again = SettingsStore::load(base()).snapshot();
        assert!(!again.transfer_whitelist && again.transfer_contacts && again.transfer_others);
        assert_eq!(transfer_who(&again, Some("whitelist"), false), None);
        assert_eq!(transfer_who(&again, None, false), Some(TransferWho::Stranger));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stall_spam_defaults_on_and_clamps_cap() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-stall-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base);
        let rt = store
            .apply_patch(&json!({
                "gemini_persona_mode": true,
                "language": "zh-TW",
                "answer_style": "persona"
            }))
            .unwrap();
        assert!(rt.persona_stall_spam);
        // Defaults: normal calls 5 min, stalling 30 min on gemini-3.8-live.
        assert_eq!(rt.persona_max_secs(), 5 * 60);
        assert_eq!(rt.persona_stall_max_secs(), 30 * 60);
        assert_eq!(rt.gemini_stall_model, "gemini-3.8-live");
        let pk = crate::prompts::pack(crate::locale::Lang::ZhTw);
        // Every call starts short; the full stall block only after the switch.
        let first = rt.gemini_system_prompt_for_call();
        assert!(first.contains(&pk.sales_listen_line) && first.contains("讓他繼續講") && !first.contains("陪他繞"));
        assert!(!first.contains(&pk.sales_reject_default));
        let full = rt.persona_stall_system_prompt();
        assert!(full.contains("陪他繞") && !full.contains("讓他繼續講"));
        assert!(full.len() > first.len());
        let rt = store
            .apply_patch(&json!({ "persona_stall_max_min": 999 }))
            .unwrap();
        assert_eq!(rt.persona_stall_max_min, 30);
        let rt = store
            .apply_patch(&json!({ "persona_stall_max_min": 1 }))
            .unwrap();
        assert_eq!(rt.persona_stall_max_secs(), 3 * 60);
        assert_eq!(rt.persona_max_secs(), 5 * 60, "normal cap is its own setting");
        let rt = store
            .apply_patch(&json!({ "persona_normal_max_min": 99, "gemini_stall_model": " " }))
            .unwrap();
        assert_eq!(rt.persona_max_secs(), 30 * 60);
        assert_eq!(rt.gemini_stall_model, "", "empty = keep the call's model");
        let rt = store
            .apply_patch(&json!({ "persona_normal_max_min": 1 }))
            .unwrap();
        assert_eq!(rt.persona_max_secs(), 3 * 60);
        let off = store
            .apply_patch(&json!({ "persona_stall_spam": false }))
            .unwrap();
        let p = off.gemini_system_prompt_for_call();
        assert!(!p.contains("陪他繞") && !p.contains(&pk.sales_listen_line));
        assert!(p.contains(&pk.sales_reject_default));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn server_vad_defaults_on_and_persists_off() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-vad-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert!(store.snapshot().gemini_server_vad);
        assert!(store.snapshot().gemini_barge_in);
        assert_eq!(store.masked_json()["gemini_server_vad"], json!(true));
        let rt = store
            .apply_patch(&json!({ "gemini_server_vad": false }))
            .unwrap();
        assert!(!rt.gemini_server_vad);
        // Survives a restart (settings.json override).
        assert!(!SettingsStore::load(base).snapshot().gemini_server_vad);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_days_defaults_to_15_and_clamps() {
        // The environment only gives the default: unset or unreadable is 15.
        assert_eq!(crate::config::retention_days_default(None), RETENTION_DAYS_DEFAULT);
        assert_eq!(RETENTION_DAYS_DEFAULT, 15);
        for (raw, days) in [("30", 30), (" 7 ", 7), ("0", 1), ("400", 365), ("3650", 365), ("-3", 15), ("soon", 15), ("", 15)] {
            assert_eq!(crate::config::retention_days_default(Some(raw)), days, "CALLS_RETENTION_DAYS={raw:?}");
        }
        let dir = std::env::temp_dir().join(format!("bageo-retention-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        base.calls_retention_days = RETENTION_DAYS_DEFAULT;
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().retention_days, RETENTION_DAYS_DEFAULT);
        for (asked, kept) in [(30, 30), (1, 1), (365, 365), (0, 1), (366, 365), (100_000, 365)] {
            let rt = store.apply_patch(&json!({ "retention_days": asked })).unwrap();
            assert_eq!(rt.retention_days, kept, "asked {asked}");
        }
        // Not a number: the value stays.
        let rt = store.apply_patch(&json!({ "retention_days": "soon" })).unwrap();
        assert_eq!(rt.retention_days, 365);
        // It is saved: a restart reads it back.
        store.apply_patch(&json!({ "retention_days": 7 })).unwrap();
        assert_eq!(SettingsStore::load(base.clone()).snapshot().retention_days, 7);
        let _ = std::fs::remove_dir_all(&dir);
        // CALLS_RETENTION_DAYS gives the default when the owner never set it.
        let dir = std::env::temp_dir().join(format!("bageo-retention-env-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        base.calls_data_dir = dir.to_string_lossy().to_string();
        base.calls_retention_days = 40;
        assert_eq!(SettingsStore::load(base.clone()).snapshot().retention_days, 40);
        // A settings file from before (with the text-message-only key) still loads.
        let path = SettingsStore::load(base.clone()).storage_path().to_path_buf();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, br#"{"sms_retention_days": 3, "persona_normal_max_min": 6}"#).unwrap();
        let old = SettingsStore::load(base.clone());
        assert_eq!((old.snapshot().retention_days, old.snapshot().persona_max_secs()), (40, 6 * 60));
        assert_eq!(old.apply_patch(&json!({ "sms_retention_days": 2 })).unwrap().retention_days, 40, "the old key is ignored");
        assert!(old.apply_patch(&json!({ "retention_days": 9 })).is_ok());
        assert_eq!(SettingsStore::load(base).snapshot().retention_days, 9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn answer_ring_defaults_to_one_ring_and_clamps() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-ring-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base);
        assert_eq!(store.snapshot().answer_ring_ms, ANSWER_RING_MS_DEFAULT);
        assert!(store.snapshot().gemini_ring_prewarm);
        let rt = store.apply_patch(&json!({ "answer_ring_ms": 60_000 })).unwrap();
        assert_eq!(rt.answer_ring_ms, ANSWER_RING_MS_CEIL);
        let rt = store
            .apply_patch(&json!({ "answer_ring_ms": 0, "gemini_ring_prewarm": false }))
            .unwrap();
        assert_eq!(rt.answer_ring_ms, 0);
        assert!(!rt.gemini_ring_prewarm);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thinking_level_defaults_low_and_sanitizes() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-thinking-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().gemini_thinking_level, "low");
        let rt = store
            .apply_patch(&json!({ "gemini_thinking_level": " High " }))
            .unwrap();
        assert_eq!(rt.gemini_thinking_level, "high");
        let rt = store
            .apply_patch(&json!({ "gemini_thinking_level": "minimal" }))
            .unwrap();
        assert_eq!(rt.gemini_thinking_level, "low");
        store
            .apply_patch(&json!({ "gemini_thinking_level": "medium" }))
            .unwrap();
        // Survives a restart (settings.json override).
        let reloaded = SettingsStore::load(base);
        assert_eq!(reloaded.snapshot().gemini_thinking_level, "medium");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ambience_defaults_off_and_sanitizes() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-ambience-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().ambience_kind, "off");
        assert_eq!(store.snapshot().ambience_level, 2);
        let rt = store
            .apply_patch(&json!({ "ambience_kind": " Office ", "ambience_level": 3 }))
            .unwrap();
        assert_eq!(rt.ambience_kind, "office");
        assert_eq!(rt.ambience_level, 3);
        let rt = store
            .apply_patch(&json!({ "ambience_kind": "cafe", "ambience_level": 9 }))
            .unwrap();
        assert_eq!(rt.ambience_kind, "off");
        assert_eq!(rt.ambience_level, 3);
        let rt = store.apply_patch(&json!({ "ambience_kind": "street" })).unwrap();
        assert_eq!((rt.ambience_kind.as_str(), rt.ambience_level), ("street", 3));
        assert_eq!(
            SettingsStore::load(base).snapshot().ambience_kind,
            "street"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn console_url_prefers_the_typed_address_then_the_network_name() {
        use std::net::{IpAddr, Ipv4Addr};
        let any = Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let local = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let lan = |_: IpAddr| Some(Ipv4Addr::new(192, 168, 1, 20));
        let no_lan = |_: IpAddr| -> Option<Ipv4Addr> { None };
        // Typed by the owner: used as typed, whatever else is true.
        assert_eq!(
            console_url(" https://calls.example.com ", any, 8787, "bageo", true, true, lan),
            "https://calls.example.com"
        );
        assert_eq!(
            console_url("http://10.0.0.5:8787", local, 8787, "bageo", false, false, lan),
            "http://10.0.0.5:8787"
        );
        // Open to the LAN: the network name while it answers, else the IP.
        assert_eq!(console_url("", any, 8787, "bageo", true, false, lan), "http://bageo.local:8787/");
        assert_eq!(console_url("", any, 8787, "bageo", true, true, lan), "http://bageo.local/");
        assert_eq!(console_url("", any, 9000, "bageo", false, false, lan), "http://192.168.1.20:9000/");
        // Port 80 without the network name: the IP at the console's own port.
        assert_eq!(console_url("", any, 8787, "bageo", false, true, lan), "http://192.168.1.20:8787/");
        assert_eq!(console_url("", any, 8787, "bageo", false, false, no_lan), "");
        // This computer only: no link a phone could open.
        assert_eq!(console_url("", local, 8787, "bageo", true, false, lan), "");
        assert_eq!(console_url("", None, 8787, "bageo", true, false, lan), "");
    }

    #[test]
    fn automatic_mode_console_url_and_access() {
        use std::net::{IpAddr, Ipv4Addr};
        let lan = |_: IpAddr| Some(Ipv4Addr::new(192, 168, 1, 20));
        let mut s = Settings::from_env();
        s.host = "127.0.0.1".into();
        s.port = 8787;
        s.mdns_name = "bageo".into();
        s.public_url = String::new();
        // AGENT_HOST not set: port 80 and the name, the name at 8787, the IP.
        assert_eq!(console_url_for(&s, false, true, true, true, lan), "http://bageo.local/");
        assert_eq!(console_url_for(&s, false, true, false, true, lan), "http://bageo.local:8787/");
        assert_eq!(console_url_for(&s, false, true, true, false, lan), "http://192.168.1.20:8787/");
        // The network name turned off: the IP even with mDNS running.
        s.mdns_name = "off".into();
        assert_eq!(console_url_for(&s, false, true, true, true, lan), "http://192.168.1.20:8787/");
        s.mdns_name = "bageo".into();
        // AGENT_HOST=127.0.0.1 set: this computer only, as before.
        assert_eq!(console_url_for(&s, false, false, true, true, lan), "");
        assert_eq!(console_access_for(&s, false, false), "local");
        assert_eq!(console_access_for(&s, false, true), "lan");
        // AGENT_HOST=0.0.0.0 set: the name at the console's port, never port 80.
        s.host = "0.0.0.0".into();
        assert_eq!(console_url_for(&s, false, false, true, true, lan), "http://bageo.local:8787/");
        assert_eq!(console_access_for(&s, false, false), "lan");
        // The address typed in the settings wins; a container has no own address.
        s.public_url = "https://calls.example.org/".into();
        assert_eq!(console_url_for(&s, false, true, true, true, lan), "https://calls.example.org/");
        s.public_url = String::new();
        assert_eq!(console_url_for(&s, true, false, true, true, lan), "");
    }

    #[test]
    fn language_counts_as_chosen_once_saved_or_set_in_the_environment() {
        let none = |_: &str| false;
        let (fresh, _) = read_overrides("{}").unwrap();
        assert!(!language_chosen_in(&fresh, none));
        assert!(language_chosen_in(&fresh, |k| k == "APP_LANGUAGE"));
        assert!(language_chosen_in(&fresh, |k| k == "NOTIFY_LANGUAGE"));
        let (other, _) = read_overrides(r#"{"owner_name":"Lee"}"#).unwrap();
        assert!(!language_chosen_in(&other, none));
        for saved in [r#"{"language":"ja"}"#, r#"{"notify_language":"zh-TW"}"#] {
            let (o, _) = read_overrides(saved).unwrap();
            assert!(language_chosen_in(&o, none), "{saved}");
        }
    }

    #[test]
    fn network_name_is_cleaned_and_empty_stays_off() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-mdns-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        base.mdns_name = "bageo".to_string();
        let store = SettingsStore::load(base.clone());
        let rt = store.apply_patch(&json!({ "mdns_name": " Front-Desk.local " })).unwrap();
        assert_eq!(rt.mdns_name, "front-desk");
        assert_eq!(rt.effective(&base).mdns_name, "front-desk");
        // Cleared in the console: off, also after a restart (not back to "bageo").
        store.apply_patch(&json!({ "mdns_name": "" })).unwrap();
        let again = SettingsStore::load(base.clone());
        assert_eq!(again.snapshot().mdns_name, "");
        assert_eq!(again.snapshot().effective(&base).mdns_name, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn appointments_default_off_and_persist() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-appointments-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        let rt = store.snapshot();
        assert_eq!((rt.appointment_mode.as_str(), rt.appointment_sms), ("off", false));
        let shown = store.masked_json();
        assert_eq!(shown["appointment_mode"], "off");
        assert_eq!(shown["appointment_sms"], false);
        let rt = store
            .apply_patch(&json!({ "appointment_mode": " Confirm ", "appointment_sms": true }))
            .unwrap();
        assert_eq!((rt.appointment_mode.as_str(), rt.appointment_sms), ("confirm", true));
        // Survives a restart (settings.json override).
        let again = SettingsStore::load(base.clone());
        assert_eq!(again.snapshot().appointment_mode, "confirm");
        assert!(again.snapshot().appointment_sms);
        // Anything unknown is off; "auto" (gone with Google Calendar) is
        // confirm; another key leaves both alone.
        let rt = again.apply_patch(&json!({ "appointment_mode": "always" })).unwrap();
        assert_eq!(rt.appointment_mode, "off");
        let rt = again.apply_patch(&json!({ "appointment_mode": "auto" })).unwrap();
        let rt2 = again.apply_patch(&json!({ "calendar_suggest": false })).unwrap();
        assert_eq!((rt.appointment_mode.as_str(), rt2.appointment_mode.as_str()), ("confirm", "confirm"));
        assert!(rt2.appointment_sms);
        assert_eq!(SettingsStore::load(base).snapshot().appointment_mode, "confirm");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A settings.json written while Google Calendar could be connected loads
    /// without an error: the credentials and `calendar_auto_add` are skipped
    /// (and gone once it is saved again), `auto` appointments are `confirm`,
    /// everything else is kept.
    #[test]
    fn settings_from_the_google_calendar_days_still_load() {
        let dir = std::env::temp_dir().join(format!("bageo-call-old-gcal-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("config")).expect("temp settings dir");
        let old = json!({
            "gmail_user": "bot@gmail.com",
            "gcal_client_id": "id.apps.example",
            "gcal_client_secret": "secret-123456",
            "gcal_refresh_token": "refresh-123456",
            "gcal_calendar_id": "primary",
            "calendar_auto_add": true,
            "calendar_suggest": false,
            "appointment_mode": "auto",
            "appointment_sms": true
        });
        let path = dir.join("config").join("settings.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&old).unwrap()).unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        let rt = store.snapshot();
        assert_eq!(rt.gmail_user, "bot@gmail.com");
        assert!(!rt.calendar_suggest && rt.appointment_sms);
        assert_eq!((rt.appointment_mode.as_str(), rt.calendar_app.as_str()), ("confirm", ""));
        let shown = store.masked_json().to_string();
        assert!(!shown.contains("gcal") && !shown.contains("calendar_auto_add") && !shown.contains("refresh-123456"), "{shown}");
        // The next save writes only what this agent knows.
        let rt = store.apply_patch(&json!({ "calendar_app": " Outlook " })).unwrap();
        assert_eq!(rt.calendar_app, "outlook");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let saved = saved.as_object().unwrap();
        assert!(saved.keys().all(|k| !k.starts_with("gcal") && k != "calendar_auto_add"), "{saved:?}");
        assert_eq!((saved["appointment_mode"].as_str(), saved["calendar_app"].as_str()), (Some("confirm"), Some("outlook")));
        assert_eq!(saved["gmail_user"], "bot@gmail.com");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_chosen_calendar_is_kept_and_unknown_ones_are_not_chosen() {
        let dir = std::env::temp_dir().join(format!("bageo-call-calendar-app-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().calendar_app, "");
        assert_eq!(store.masked_json()["calendar_app"], "");
        assert_eq!(store.apply_patch(&json!({ "calendar_app": "APPLE" })).unwrap().calendar_app, "apple");
        assert_eq!(SettingsStore::load(base.clone()).snapshot().calendar_app, "apple");
        // Another key leaves it alone; a name it does not know is "not chosen".
        assert_eq!(store.apply_patch(&json!({ "calendar_suggest": true })).unwrap().calendar_app, "apple");
        assert_eq!(store.apply_patch(&json!({ "calendar_app": "outlook.com" })).unwrap().calendar_app, "");
        // The default way to tell a caller: automatic until another is picked.
        assert_eq!(store.snapshot().appointment_reply, "auto");
        assert_eq!(store.apply_patch(&json!({ "appointment_reply": " Call " })).unwrap().appointment_reply, "call");
        assert_eq!(SettingsStore::load(base.clone()).snapshot().appointment_reply, "call");
        assert_eq!(store.apply_patch(&json!({ "appointment_reply": "fax" })).unwrap().appointment_reply, "auto");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_errand_identity_is_the_assistant_unless_set_to_the_owner() {
        let dir = std::env::temp_dir().join(format!("bageo-call-errand-identity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().errand_identity, "assistant");
        assert_eq!(store.masked_json()["errand_identity"], "assistant");
        assert_eq!(store.apply_patch(&json!({ "errand_identity": " Owner " })).unwrap().errand_identity, "owner");
        assert_eq!(SettingsStore::load(base.clone()).snapshot().errand_identity, "owner");
        assert_eq!(store.apply_patch(&json!({ "calendar_suggest": true })).unwrap().errand_identity, "owner");
        assert_eq!(store.apply_patch(&json!({ "errand_identity": "robot" })).unwrap().errand_identity, "assistant");
        assert_eq!(sanitize_errand_identity(""), "assistant");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notify_language_follows_the_ai_unless_set() {
        use crate::locale::Lang;
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-notify-lang-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        // Default: empty = the AI's language (what it always was).
        let rt = store.apply_patch(&json!({ "language": "en" })).unwrap();
        assert_eq!(rt.notify_language, "");
        assert_eq!((rt.lang(), rt.notify_lang()), (Lang::En, Lang::En));
        assert_eq!(store.masked_json()["notify_language"], "");
        // The AI speaks English with callers, the owner reads Traditional Chinese.
        let rt = store.apply_patch(&json!({ "notify_language": " zh_tw " })).unwrap();
        assert_eq!(rt.notify_language, "zh-TW");
        let a = rt.locale_active();
        assert_eq!((a.lang, a.notify_lang), (Lang::En, Lang::ZhTw));
        assert_eq!(store.masked_json()["notify_language"], "zh-TW");
        // Survives a restart (settings.json override); another key leaves it alone.
        assert_eq!(SettingsStore::load(base.clone()).snapshot().notify_language, "zh-TW");
        let rt = store.apply_patch(&json!({ "language": "de" })).unwrap();
        assert_eq!((rt.lang(), rt.notify_lang()), (Lang::De, Lang::ZhTw));
        // Not one of the codes: empty again, so the AI's language.
        for bad in ["klingon", "pt-BR", "same"] {
            let rt = store.apply_patch(&json!({ "notify_language": bad })).unwrap();
            assert_eq!(rt.notify_language, "", "{bad}");
            assert_eq!(rt.notify_lang(), Lang::De, "{bad}");
        }
        let rt = store.apply_patch(&json!({ "notify_language": "JA" })).unwrap();
        assert_eq!((rt.notify_language.as_str(), rt.notify_lang()), ("ja", Lang::Ja));
        let rt = store.apply_patch(&json!({ "notify_language": "" })).unwrap();
        assert_eq!((rt.notify_language.as_str(), rt.locale_active().notify_lang), ("", Lang::De));
        assert_eq!(SettingsStore::load(base).snapshot().notify_language, "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sim_languages_keep_countries_and_languages_only() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-sim-languages-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        // Default: none, every SIM uses the AI's language.
        assert!(store.snapshot().sim_languages.is_empty());
        assert_eq!(store.masked_json()["sim_languages"], json!({}));
        let rt = store
            .apply_patch(&json!({ "sim_languages": {
                "US": "en", " jp ": "JA", "tw": "zh_tw",
                "usa": "en", "x1": "en", "": "en", "fr": "klingon", "de": "", "ca": 7
            } }))
            .unwrap();
        let want: std::collections::BTreeMap<String, String> = [("jp", "ja"), ("tw", "zh-TW"), ("us", "en")]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(rt.sim_languages, want);
        assert_eq!(store.masked_json()["sim_languages"], json!({"jp": "ja", "tw": "zh-TW", "us": "en"}));
        // Survives a restart; another key leaves it alone.
        assert_eq!(SettingsStore::load(base.clone()).snapshot().sim_languages, want);
        let rt = store.apply_patch(&json!({ "language": "de" })).unwrap();
        assert_eq!(rt.sim_languages, want);
        // Not an object: ignored. An empty object clears it.
        let rt = store.apply_patch(&json!({ "sim_languages": ["us"] })).unwrap();
        assert_eq!(rt.sim_languages, want);
        let rt = store.apply_patch(&json!({ "sim_languages": {} })).unwrap();
        assert!(rt.sim_languages.is_empty());
        assert!(SettingsStore::load(base).snapshot().sim_languages.is_empty());
        // The settings page gets the countries to choose from.
        let shown = store.masked_json();
        let countries = shown["countries"].as_array().expect("countries");
        assert!(countries.iter().any(|c| c == "tw") && countries.iter().any(|c| c == "us"));
        assert!(shown["seen_sim_countries"].is_array());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reply_in_the_callers_language_is_on_unless_turned_off() {
        let dir = std::env::temp_dir().join(format!("bageo-call-reply-lang-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("config")).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        // Never touched (no settings file, or one saved before the setting existed): on.
        if std::env::var("REPLY_IN_CALLER_LANGUAGE").is_err() {
            assert!(RuntimeSettings::from_base(&base).reply_in_caller_language);
            assert!(SettingsStore::load(base.clone()).snapshot().reply_in_caller_language);
            std::fs::write(dir.join("config").join("settings.json"), r#"{"language":"zh-TW"}"#).unwrap();
            assert!(SettingsStore::load(base.clone()).snapshot().reply_in_caller_language);
        }
        // Turned off and saved: stays off, also after a restart; on again when turned on.
        let store = SettingsStore::load(base.clone());
        assert!(!store.apply_patch(&json!({ "reply_in_caller_language": false })).unwrap().reply_in_caller_language);
        assert!(!SettingsStore::load(base.clone()).snapshot().reply_in_caller_language);
        assert!(store.apply_patch(&json!({ "reply_in_caller_language": true })).unwrap().reply_in_caller_language);
        assert!(SettingsStore::load(base).snapshot().reply_in_caller_language);
        // The prompt of a call carries the note to switch exactly when it is on.
        let mut rt = RuntimeSettings::from_base(&Settings::from_env());
        rt.gemini_persona_mode = true;
        rt.language = "zh-TW".into();
        rt.reply_in_caller_language = true;
        assert!(rt.gemini_system_prompt_for_call().contains("【對方講別的語言】"));
        rt.reply_in_caller_language = false;
        assert!(!rt.gemini_system_prompt_for_call().contains("【對方講別的語言】"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_older_answering_flow_is_only_kept_for_traditional_chinese() {
        use crate::locale::{Lang, ALL};
        let mk = |lang: Lang, persona_mode: bool, style: &str| {
            let mut rt = RuntimeSettings::from_base(&Settings::from_env());
            rt.language = lang.code().into();
            rt.owner_name = "Alex".into();
            rt.gemini_persona_mode = persona_mode;
            rt.answer_style = style.into();
            rt.gemini_system_prompt.clear();
            rt.gemini_persona_prompt.clear();
            rt.gemini_assistant_prompt.clear();
            rt
        };
        let mut prompts = Vec::new();
        for lang in ALL {
            // Turned on: every language runs the current flow with the saved identity.
            let on_persona = mk(lang, true, "persona");
            let on_assistant = mk(lang, true, "assistant");
            assert!(on_persona.persona_flow_active() && !on_persona.legacy_flow_replaced(), "{lang:?}");
            assert!(!on_persona.is_assistant_style() && on_assistant.is_assistant_style(), "{lang:?}");

            // Turned off, even with "as me" saved.
            let off = mk(lang, false, "persona");
            assert!(!off.gemini_persona_mode, "{lang:?}");
            if lang == Lang::ZhTw {
                // Traditional Chinese: the older relay flow stays as it was.
                assert!(!off.persona_flow_active() && !off.legacy_flow_replaced(), "{lang:?}");
                assert!(!off.is_assistant_style() && !off.locale_active().assistant, "{lang:?}");
                let legacy = crate::gemini_live::with_owner_status(
                    crate::gemini_live::default_system_prompt(),
                    off.owner_status_enabled,
                    &off.owner_status_text,
                );
                assert_eq!(off.gemini_system_prompt_for_call(), legacy, "{lang:?}");
                // Its opening is the persona one, as before.
                assert!(!mk(lang, false, "assistant").legacy_flow_replaced());
                continue;
            }
            // Any other language: the current flow, as the AI assistant, in the owner's language.
            assert!(off.persona_flow_active() && off.legacy_flow_replaced(), "{lang:?}");
            assert!(off.is_assistant_style() && off.locale_active().assistant, "{lang:?}");
            // The saved identity is left alone (the console still shows it).
            assert_eq!(off.answer_style, "persona", "{lang:?}");
            assert!(!off.answer_style_is_assistant(), "{lang:?}");
            // The call's prompt is the assistant prompt of the current flow, not the older text.
            assert_eq!(off.gemini_system_prompt_for_call(), on_assistant.gemini_system_prompt_for_call(), "{lang:?}");
            assert_ne!(off.gemini_system_prompt_for_call(), crate::gemini_live::default_system_prompt(), "{lang:?}");
            assert_ne!(off.gemini_system_prompt_for_call(), on_persona.gemini_system_prompt_for_call(), "{lang:?}");
            assert!(off.gemini_system_prompt_for_call().contains("Alex"), "{lang:?}");
            // The opening is the assistant's in the owner's language, never the owner's own.
            let opening = crate::prompts::kick_open_call(off.lang(), off.is_assistant_style(), None, false);
            assert_eq!(opening, crate::prompts::kick_open_call(lang, true, None, false), "{lang:?}");
            assert_ne!(opening, crate::prompts::kick_open_call(lang, false, None, false), "{lang:?}");
            prompts.push(off.gemini_system_prompt_for_call());
            // With the assistant already saved, turning the mode off changes nothing more.
            let off_assistant = mk(lang, false, "assistant");
            assert!(off_assistant.persona_flow_active() && off_assistant.is_assistant_style(), "{lang:?}");
        }
        // Eight languages, eight different prompts (each in the owner's language).
        let mut unique = prompts.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 8);
        assert_eq!(prompts.len(), 8);
    }


    #[test]
    fn summary_provider_defaults_gemini_and_resolves_older_values() {
        use crate::summary::{summary_endpoint_for, Provider};
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-summary-provider-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        base.summary_api_key = String::new();
        base.summary_api_key_old_env = false;
        base.trtc_llm_config = "".into();
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().summary_provider, "gemini");
        assert_eq!(store.snapshot().summary_provider(), Provider::Gemini);
        assert_eq!(store.masked_json()["summary_provider"], "gemini");
        let rt = store.apply_patch(&json!({ "summary_provider": " Custom " })).unwrap();
        assert_eq!((rt.summary_provider.as_str(), rt.summary_provider()), ("custom", Provider::Custom));
        // Survives a restart (settings.json override); another key leaves it alone.
        let again = SettingsStore::load(base.clone());
        assert_eq!(again.snapshot().summary_provider, "custom");
        let rt = again.apply_patch(&json!({ "summary_model": "gemini-2.5-flash" })).unwrap();
        assert_eq!(rt.summary_provider, "custom");
        let rt = again.apply_patch(&json!({ "summary_provider": "openai" })).unwrap();
        assert_eq!(rt.summary_provider, "gemini");
        assert_eq!(SettingsStore::load(base.clone()).snapshot().summary_provider, "gemini");
        // Settings files from before: the service and address they end up with,
        // after loading, after saving something else and after a restart.
        let own = "http://127.0.0.1:11434/v1";
        let older = [
            (r#"{"language":"fr","summary_model":"gpt-4o-mini"}"#.to_string(), "gemini", ""),
            (r#"{"summary_provider":"auto","gemini_api_key":"AIzaTEST"}"#.to_string(), "gemini", ""),
            (r#"{"summary_provider":"auto","deepseek_api_key":"sk-test"}"#.to_string(), "custom", OLD_SUMMARY_API_BASE),
            (format!(r#"{{"summary_api_base":"{own}"}}"#), "custom", own),
            (r#"{"summary_provider":"openai","trtc_llm_config":"{\"APIKey\":\"sk-cfg\"}"}"#.to_string(), "custom", OLD_SUMMARY_API_BASE),
            (r#"{"summary_provider":"gemini","deepseek_api_key":"sk-test"}"#.to_string(), "gemini", ""),
            (r#"{"summary_provider":"deepseek"}"#.to_string(), "custom", ""),
            (format!(r#"{{"summary_provider":"deepseek","summary_api_base":"{own}"}}"#), "custom", own),
            (
                format!(r#"{{"summary_provider":"deepseek","summary_api_base":"{own}","deepseek_api_key":"sk-test"}}"#),
                "custom",
                OLD_SUMMARY_API_BASE,
            ),
            (
                format!(r#"{{"summary_provider":"custom","summary_api_base":"{own}","summary_model":"llama3","deepseek_api_key":"sk-test"}}"#),
                "custom",
                own,
            ),
            // Saved by this version: a key without an address stays without one.
            (r#"{"summary_provider":"custom","summary_api_key":"sk-test"}"#.to_string(), "custom", ""),
        ];
        for (i, (file, provider, api_base)) in older.iter().enumerate() {
            let old_dir = dir.join(format!("old{i}"));
            std::fs::create_dir_all(old_dir.join("config")).expect("old settings dir");
            let path = old_dir.join("config").join("settings.json");
            std::fs::write(&path, file).expect("old settings.json");
            let mut old_base = base.clone();
            old_base.calls_data_dir = old_dir.to_string_lossy().to_string();
            let old = SettingsStore::load(old_base.clone());
            let rt = old.snapshot();
            assert_eq!((rt.summary_provider.as_str(), rt.summary_api_base.as_str()), (*provider, *api_base), "{file}");
            let rt = old.apply_patch(&json!({ "owner_name": "Alex" })).unwrap();
            assert_eq!((rt.summary_provider.as_str(), rt.summary_api_base.as_str()), (*provider, *api_base), "{file}");
            let rt = SettingsStore::load(old_base.clone()).snapshot();
            assert_eq!((rt.summary_provider.as_str(), rt.summary_api_base.as_str()), (*provider, *api_base), "{file}");
            // The key's old name is gone from the file once it is read.
            let saved = std::fs::read_to_string(&path).unwrap();
            assert!(!saved.contains("deepseek_api_key"), "{file} -> {saved}");
            if file.contains("deepseek_api_key") {
                assert_eq!(rt.summary_api_key, "sk-test", "{file}");
                assert!(saved.contains("summary_api_key"), "{saved}");
            }
            if i == 0 {
                // Written before either key existed: the notifications in the AI's language.
                assert_eq!((rt.summary_model.as_str(), rt.notify_language.as_str()), ("gpt-4o-mini", ""));
                assert_eq!(rt.notify_lang(), crate::locale::Lang::Fr);
            }
            if *api_base == OLD_SUMMARY_API_BASE {
                // The summaries still go where they went, now written down.
                let e = summary_endpoint_for(&rt.effective(&old_base), rt.summary_provider()).unwrap();
                let key = if i == 4 { "sk-cfg" } else { "sk-test" };
                assert_eq!((e.url.as_str(), e.key.as_str()), ("https://api.deepseek.com/v1/chat/completions", key), "{file}");
                assert_eq!(e.models, vec![OLD_SUMMARY_MODEL.to_string()], "{file}");
                assert!(saved.contains(OLD_SUMMARY_API_BASE), "{saved}");
            }
            if i == 9 {
                assert_eq!(rt.summary_model, "llama3", "a model already set is kept");
            }
            if i == older.len() - 1 {
                assert_eq!(summary_endpoint_for(&rt.effective(&old_base), rt.summary_provider()), None);
            }
        }
        // Left on Gemini, it stays Gemini when a summary key is saved later.
        let fresh_dir = dir.join("fresh");
        std::fs::create_dir_all(&fresh_dir).expect("fresh settings dir");
        let mut fresh_base = base.clone();
        fresh_base.calls_data_dir = fresh_dir.to_string_lossy().to_string();
        let fresh = SettingsStore::load(fresh_base.clone());
        let rt = fresh.apply_patch(&json!({ "summary_api_key": "sk-test" })).unwrap();
        assert_eq!(rt.summary_provider, "gemini");
        let rt = fresh.apply_patch(&json!({ "trtc_llm_config": "{\"APIKey\":\"sk-cfg\"}" })).unwrap();
        assert_eq!(rt.summary_provider, "gemini");
        let rt = SettingsStore::load(fresh_base).snapshot();
        assert_eq!((rt.summary_provider.as_str(), rt.summary_api_base.as_str()), ("gemini", ""), "Gemini: the address is left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_key_under_its_old_name_is_taken_and_saved_under_the_new_one() {
        let dir = std::env::temp_dir().join(format!("bageo-summary-key-{}", uuid::Uuid::new_v4()));
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        base.summary_api_key = String::new();
        base.summary_api_key_old_env = false;
        base.trtc_llm_config = "".into();
        let store = SettingsStore::load(base.clone());
        // A request of an older console sends the old name.
        let rt = store.apply_patch(&json!({ "deepseek_api_key": "sk-test" })).unwrap();
        assert_eq!(rt.summary_api_key, "sk-test");
        // The new name wins when a request has both.
        let rt = store.apply_patch(&json!({ "deepseek_api_key": "sk-a", "summary_api_key": "sk-b" })).unwrap();
        assert_eq!(rt.summary_api_key, "sk-b");
        let saved = std::fs::read_to_string(store.storage_path()).unwrap();
        assert!(saved.contains("summary_api_key") && !saved.contains("deepseek"), "{saved}");
        let masked = store.masked_json();
        assert_eq!((masked["summary_api_key_set"].as_bool(), masked.get("deepseek_api_key").is_none()), (Some(true), true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn summary_key_only_in_the_environment_keeps_its_old_service_without_saving() {
        use crate::summary::{summary_endpoint_for, Provider};
        let dir = std::env::temp_dir().join(format!("bageo-summary-env-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("config")).unwrap();
        let path = dir.join("config").join("settings.json");
        std::fs::write(&path, r#"{"summary_provider":"custom"}"#).unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        base.trtc_llm_config = "".into();
        base.gemini_api_key = "".into();
        base.openrouter_api_key = String::new();
        base.summary_api_key = "sk-test".into();
        // Under the variable's former name: an older setup.
        base.summary_api_key_old_env = true;
        let rt = SettingsStore::load(base.clone()).snapshot();
        assert_eq!(rt.summary_api_base, "", "nothing is written for a key in the environment");
        let e = summary_endpoint_for(&rt.effective(&base), Provider::Custom).unwrap();
        assert_eq!((e.url.as_str(), e.key.as_str()), ("https://api.deepseek.com/v1/chat/completions", "sk-test"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"summary_provider":"custom"}"#);
        // An address of its own wins.
        let mut own = (*rt).clone();
        own.summary_api_base = "https://api.openai.com/v1".into();
        own.summary_model = "gpt-4o-mini".into();
        let e = summary_endpoint_for(&own.effective(&base), Provider::Custom).unwrap();
        assert_eq!(e.url, "https://api.openai.com/v1/chat/completions");
        // Under the new name: no service of its own without an address.
        base.summary_api_key_old_env = false;
        let rt = SettingsStore::load(base.clone()).snapshot();
        assert_eq!(summary_endpoint_for(&rt.effective(&base), Provider::Custom), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A settings.json saved while test numbers had their own settings (demo
    /// calendar, phone and SIM for their texts) loads without an error: those
    /// keys are skipped (and gone once it is saved again), everything else is kept.
    #[test]
    fn settings_with_the_old_test_number_keys_still_load() {
        let dir = std::env::temp_dir().join(format!("bageo-call-old-test-numbers-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("config")).expect("temp settings dir");
        let old = json!({
            "appointment_mode": "confirm",
            "appointment_sms": true,
            "appointment_test_numbers": ["16505550100"],
            "appointment_test_sms_phone": "Pixel 9",
            "appointment_test_sms_sim": "Tello"
        });
        let path = dir.join("config").join("settings.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&old).unwrap()).unwrap();
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        let rt = store.snapshot();
        assert_eq!((rt.appointment_mode.as_str(), rt.appointment_sms), ("confirm", true));
        // The data folder comes from the env base and is never shown.
        assert_eq!(rt.calls_data_dir, base.calls_data_dir);
        let shown = store.masked_json();
        assert!(shown.get("calls_data_dir").is_none());
        let shown = shown.to_string();
        assert!(!shown.contains("appointment_test") && !shown.contains("Tello"), "{shown}");
        // The next save writes only what this agent knows.
        let rt = store.apply_patch(&json!({ "appointment_reply": "sms" })).unwrap();
        assert_eq!(rt.appointment_reply, "sms");
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let saved = saved.as_object().unwrap();
        assert!(saved.keys().all(|k| !k.starts_with("appointment_test")), "{saved:?}");
        assert_eq!((saved["appointment_mode"].as_str(), saved["appointment_sms"].as_bool()), (Some("confirm"), Some(true)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn owner_aliases_default_empty_clean_and_persist() {
        let dir = std::env::temp_dir().join(format!("bageo-call-owner-aliases-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert!(store.snapshot().owner_aliases.is_empty());
        assert_eq!(store.masked_json()["owner_aliases"], json!([]));
        let mut list = vec![
            json!(" Lex "),
            json!(""),
            json!("   "),
            json!("lex"),
            json!("Al\u{7}ex"),
            json!("ALEX"),
            json!("x".repeat(60)),
            json!("Le\nx"),
        ];
        list.extend((1..=12).map(|i| json!(format!("N{i}"))));
        let rt = store
            .apply_patch(&json!({
                "gemini_persona_mode": true,
                "language": "zh-TW",
                "answer_style": "persona",
                "owner_name": "Alex",
                "owner_aliases": list
            }))
            .unwrap();
        // Trimmed, control characters out, 40 characters; blanks, repeats and
        // the owner's own name (any case) dropped; at most 10.
        let mut want = vec!["Lex".to_string(), "x".repeat(40)];
        want.extend((1..=8).map(|i| format!("N{i}")));
        assert_eq!(rt.owner_aliases, want);
        assert_eq!(rt.locale_active().owner_aliases, want);
        // Every prompt of a call carries them: the call's, the stalling one,
        // the assistant style's and an edited one.
        assert!(rt.gemini_system_prompt_for_call().contains("來電的人也可能叫你「Lex」、「xxxx"));
        assert!(rt.persona_stall_system_prompt().contains("來電的人也可能叫你「Lex」"));
        let rt = store
            .apply_patch(&json!({ "answer_style": "assistant", "gemini_assistant_prompt": "你幫{owner}接電話。" }))
            .unwrap();
        let p = rt.gemini_system_prompt_for_call();
        assert!(p.starts_with("你幫Alex接電話。") && p.contains("來電的人說要找「Lex」"), "{p}");
        // Survives a restart; another key leaves it alone.
        let again = SettingsStore::load(base.clone());
        assert_eq!(again.snapshot().owner_aliases, want);
        // An alias that becomes the owner's name drops out.
        let rt = again.apply_patch(&json!({ "owner_name": "lex" })).unwrap();
        assert_eq!(rt.owner_aliases, want[1..].to_vec());
        // An empty list clears it.
        let rt = again.apply_patch(&json!({ "owner_aliases": [] })).unwrap();
        assert!(rt.owner_aliases.is_empty());
        assert!(SettingsStore::load(base).snapshot().owner_aliases.is_empty());
        // A settings file written before the key existed loads as before.
        let old_dir = dir.join("old");
        std::fs::create_dir_all(old_dir.join("config")).expect("old settings dir");
        std::fs::write(old_dir.join("config").join("settings.json"), r#"{"owner_name":"Alex","language":"en"}"#)
            .expect("old settings.json");
        let mut old_base = Settings::from_env();
        old_base.calls_data_dir = old_dir.to_string_lossy().to_string();
        let old = SettingsStore::load(old_base).snapshot();
        assert_eq!((old.owner_name.as_str(), old.owner_aliases.len()), ("Alex", 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod review_t4_tests {
    use super::*;

    /// A settings.json saved by an older agent (no `reject_mode` key) loads as
    /// decline.
    #[test]
    fn saved_settings_without_reject_mode_decline() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-review-t4-settings-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        store.apply_patch(&json!({ "gemini_persona_mode": true })).unwrap();
        let saved = std::fs::read_to_string(dir.join("config").join("settings.json"))
            .expect("settings.json written");
        assert!(!saved.contains("reject_mode"));
        let rt = SettingsStore::load(base).snapshot();
        assert!(rt.gemini_persona_mode);
        assert_eq!(rt.reject_mode, "decline");
        assert!(rt.reject_declines());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The AI waits for the carrier's international-call warning on its own: a
    /// settings.json from when `intl_carrier_prompt` was a setting still loads,
    /// its `on` / `off` changes nothing, and the next save drops it.
    #[test]
    fn old_intl_carrier_prompt_setting_is_ignored() {
        for old in ["on", "off", "auto"] {
            let dir = std::env::temp_dir().join(format!(
                "bageo-intl-carrier-prompt-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(dir.join("config")).expect("temp settings dir");
            let path = dir.join("config").join("settings.json");
            let saved = json!({
                "language": "zh-TW",
                "answer_style": "persona",
                "intl_carrier_prompt": old
            });
            std::fs::write(&path, serde_json::to_vec_pretty(&saved).unwrap()).unwrap();
            let mut base = Settings::from_env();
            base.calls_data_dir = dir.to_string_lossy().to_string();
            let store = SettingsStore::load(base.clone());
            let rt = store.snapshot();
            assert_eq!((rt.language.as_str(), rt.answer_style.as_str()), ("zh-TW", "persona"));
            let mut fresh = RuntimeSettings::from_base(&base);
            fresh.language = "zh-TW".into();
            fresh.answer_style = "persona".into();
            let prompt = rt.persona_stall_system_prompt();
            assert_eq!(prompt, fresh.persona_stall_system_prompt(), "{old}");
            assert!(prompt.contains(crate::gemini_live::PERSONA_CARRIER_NOTE), "{old}");
            assert!(!store.masked_json().to_string().contains("intl_carrier_prompt"));
            store.apply_patch(&json!({ "owner_name": "Alex" })).unwrap();
            let text = std::fs::read_to_string(&path).unwrap();
            assert!(!text.contains("intl_carrier_prompt"), "{text}");
            let file: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(
                (file["language"].as_str(), file["owner_name"].as_str()),
                (Some("zh-TW"), Some("Alex"))
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn the_default_country_is_the_sims_then_the_languages_own() {
        let mut rt = RuntimeSettings::from_base(&Settings::from_env());
        rt.country_code.clear();
        let none: Vec<String> = Vec::new();
        for (lang, want, region) in [
            ("zh-TW", "886", "tw"),
            ("zh-CN", "86", "cn"),
            ("en", "1", "us"),
            ("ja", "81", "jp"),
            ("ko", "82", "kr"),
            ("es", "34", "es"),
            ("fr", "33", "fr"),
            ("de", "49", "de"),
            ("pt", "351", "pt"),
        ] {
            rt.language = lang.into();
            assert_eq!(rt.effective_country_code_with(&none), want, "{lang}");
            assert_eq!(rt.default_region(), region, "{lang}");
        }
        // A connected phone's SIM first; the language's own country when one
        // of the SIMs is there; a SIM country nobody knows is left out.
        rt.language = "ja".into();
        assert_eq!(rt.effective_country_code_with(&["us".to_string()]), "1");
        assert_eq!(rt.effective_country_code_with(&["gb".to_string(), "jp".to_string()]), "81");
        assert_eq!(rt.effective_country_code_with(&["zz".to_string()]), "81");
        // Traditional Chinese keeps Taiwan; the setting beats everything.
        rt.language = "zh-TW".into();
        assert_eq!(rt.effective_country_code_with(&["us".to_string()]), "886");
        rt.language = "ja".into();
        rt.country_code = "44".into();
        assert_eq!(rt.effective_country_code_with(&["us".to_string()]), "44");
    }

    #[test]
    fn a_blocked_number_is_read_the_way_the_calls_sim_reads_it() {
        let blocked = |number: &str, reason: &str| RejectEntry { number: number.into(), reason: reason.into() };
        let list = vec![blocked("+81 3 1234 5678", "")];
        // A Japanese SIM reports the local form of a +81 number.
        assert!(reject_number_hit_on(&list, "03-1234-5678", Some("jp")));
        assert!(reject_number_hit_on(&list, "+81 3 1234 5678", Some("jp")));
        assert!(!reject_number_hit_on(&list, "03-1234-5679", Some("jp")));
        assert_eq!(reject_reason_on(&[blocked("+81 3 1234 5678", "営業")], "0312345678", Some("jp")).as_deref(), Some("営業"));
    }

    #[test]
    fn the_public_address_is_a_web_address_or_nothing() {
        for (given, kept) in [
            ("https://bageo.example.com/", "https://bageo.example.com/"),
            ("  HTTP://192.168.1.20:8787  ", "HTTP://192.168.1.20:8787"),
            ("javascript:alert(1)", ""),
            ("JaVaScRiPt:alert(1)", ""),
            ("data:text/html,x", ""),
            ("ftp://files.example.com", ""),
            ("bageo.example.com", ""),
            ("http://", ""),
            ("https://a b", ""),
            ("", ""),
        ] {
            assert_eq!(sanitize_public_url(given), kept, "{given}");
        }
        use std::net::{IpAddr, Ipv4Addr};
        let any = Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let lan = |_: IpAddr| Some(Ipv4Addr::new(192, 168, 1, 20));
        assert_eq!(console_url("javascript:alert(1)", any, 8787, "bageo", true, false, lan), "http://bageo.local:8787/");
        // Saved through the settings page: cleared.
        let dir = std::env::temp_dir().join(format!("bageo-call-public-url-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base);
        let rt = store.apply_patch(&json!({ "public_url": "javascript:alert(1)" })).unwrap();
        assert_eq!(rt.public_url, "");
        let rt = store.apply_patch(&json!({ "public_url": "https://bageo.example.com" })).unwrap();
        assert_eq!(rt.public_url, "https://bageo.example.com");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_answer_style_keeps_its_own_custom_prompt_and_migrates_older_files() {
        for (lang, persona_line, assistant_line) in [
            ("en", "Answer as {owner}, casual and short.", "You are {owner}'s AI assistant; take a short message."),
            ("ja", "{owner}本人として短く自然に出てください。", "{owner}のAIアシスタントとして用件を伺ってください。"),
            ("es", "Responde como {owner}, breve y natural.", "Eres el asistente de IA de {owner} y tomas el recado."),
            ("zh-TW", "你是{owner}本人，講話簡短自然。", "你是{owner}的AI助理，負責記下留言。"),
        ] {
            let dir = std::env::temp_dir().join(format!("bageo-prompt-split-{lang}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("temp settings dir");
            let mut base = Settings::from_env();
            base.calls_data_dir = dir.to_string_lossy().to_string();
            let store = SettingsStore::load(base.clone());

            // 1. Custom persona prompt saved under "persona", then user switches to
            // "assistant": the assistant call uses the built-in assistant prompt
            // (never the persona text), and switching back restores the persona text.
            let rt = store
                .apply_patch(&json!({
                    "gemini_persona_mode": true,
                    "language": lang,
                    "owner_name": "Alex",
                    "answer_style": "persona",
                    "gemini_persona_prompt": persona_line,
                }))
                .unwrap();
            let persona_expected = persona_line.replace("{owner}", "Alex");
            assert!(rt.gemini_system_prompt_for_call().starts_with(&persona_expected), "{lang}");

            let rt = store.apply_patch(&json!({ "answer_style": "assistant" })).unwrap();
            let call_p = rt.gemini_system_prompt_for_call();
            assert!(!call_p.contains(&persona_expected), "{lang}: {call_p}");
            let mut fresh = RuntimeSettings::from_base(&base);
            fresh.gemini_persona_mode = true;
            fresh.language = lang.into();
            fresh.owner_name = "Alex".into();
            fresh.answer_style = "assistant".into();
            assert_eq!(call_p, fresh.gemini_system_prompt_for_call(), "{lang}");

            // Reload after switching to assistant: persona prompt stays in its own slot.
            let reloaded = SettingsStore::load(base.clone());
            let rt = reloaded.snapshot();
            assert_eq!(rt.answer_style, "assistant");
            assert_eq!(rt.gemini_assistant_prompt, "");
            assert_eq!(rt.gemini_persona_prompt, persona_line);
            assert!(!rt.gemini_system_prompt_for_call().contains(&persona_expected), "{lang}");

            // 2. Saving a custom assistant prompt keeps both prompts independent.
            let rt = reloaded
                .apply_patch(&json!({ "gemini_assistant_prompt": assistant_line }))
                .unwrap();
            let assistant_expected = assistant_line.replace("{owner}", "Alex");
            assert!(rt.gemini_system_prompt_for_call().starts_with(&assistant_expected), "{lang}");
            let rt = reloaded.apply_patch(&json!({ "answer_style": "persona" })).unwrap();
            assert!(rt.gemini_system_prompt_for_call().starts_with(&persona_expected), "{lang}");

            let masked = reloaded.masked_json();
            assert!(masked["default_prompts"]["assistant"].as_str().is_some_and(|s| !s.is_empty()));
            assert!(masked["default_prompts"]["persona"].as_str().is_some_and(|s| !s.is_empty()));
            let _ = std::fs::remove_dir_all(&dir);
        }

        // 3. Older settings.json (no `gemini_assistant_prompt`) saved while
        // `answer_style` was `assistant`: migrates into `gemini_assistant_prompt`.
        let old_asst_dir = std::env::temp_dir().join(format!("bageo-old-asst-prompt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(old_asst_dir.join("config")).unwrap();
        std::fs::write(
            old_asst_dir.join("config").join("settings.json"),
            r#"{"language":"en","owner_name":"Alex","answer_style":"assistant","gemini_persona_prompt":"Legacy assistant prompt for {owner}."}"#,
        )
        .unwrap();
        let mut old_asst_base = Settings::from_env();
        old_asst_base.calls_data_dir = old_asst_dir.to_string_lossy().to_string();
        let old_asst = SettingsStore::load(old_asst_base).snapshot();
        assert_eq!(old_asst.gemini_assistant_prompt, "Legacy assistant prompt for {owner}.");
        assert_eq!(old_asst.gemini_persona_prompt, "");
        assert!(old_asst.gemini_system_prompt_for_call().starts_with("Legacy assistant prompt for Alex."));
        let _ = std::fs::remove_dir_all(&old_asst_dir);

        // 4. Older settings.json saved while `answer_style` was `persona`: stays in
        // `gemini_persona_prompt` and switching to `assistant` uses built-in assistant.
        let old_per_dir = std::env::temp_dir().join(format!("bageo-old-per-prompt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(old_per_dir.join("config")).unwrap();
        std::fs::write(
            old_per_dir.join("config").join("settings.json"),
            r#"{"language":"es","owner_name":"Alex","answer_style":"persona","gemini_persona_prompt":"Soy {owner}, dime."}"#,
        )
        .unwrap();
        let mut old_per_base = Settings::from_env();
        old_per_base.calls_data_dir = old_per_dir.to_string_lossy().to_string();
        let store = SettingsStore::load(old_per_base.clone());
        assert_eq!(store.snapshot().gemini_persona_prompt, "Soy {owner}, dime.");
        assert_eq!(store.snapshot().gemini_assistant_prompt, "");
        let rt = store.apply_patch(&json!({ "answer_style": "assistant" })).unwrap();
        assert!(!rt.gemini_system_prompt_for_call().contains("Soy Alex, dime."));
        assert_eq!(SettingsStore::load(old_per_base).snapshot().gemini_assistant_prompt, "");
        let _ = std::fs::remove_dir_all(&old_per_dir);
    }
}

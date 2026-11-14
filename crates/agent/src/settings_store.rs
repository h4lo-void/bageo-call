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
    /// DeepSeek key for the post-call notification summary only.
    pub deepseek_api_key: String,
    /// Desk note spoken in the AI opening (next call).
    pub owner_status_enabled: bool,
    pub owner_status_text: String,
    /// Persona mode: Gemini answers as the owner — no greeting WAV / post-greet
    /// wait / goodbye WAV; a natural closing ends the call (no fixed clock, only
    /// the caps `persona_normal_max_min` / `persona_stall_max_min`).
    #[serde(default)]
    pub gemini_persona_mode: bool,
    /// Persona system prompt; empty = built-in `gemini_live::default_persona_prompt`.
    #[serde(default)]
    pub gemini_persona_prompt: String,
    /// Answer hidden numbers too: hidden / private / empty caller ID takes the
    /// normal AI path instead of the reject WAV + hangup.
    #[serde(default)]
    pub answer_unknown_callers: bool,
    /// Blocked numbers: for callers the AI should not handle, the phone picks up,
    /// plays the "cannot be reached" line and hangs up — no AI. Number and reason
    /// apart ([`RejectEntry`]); a trailing `*` on a number matches a prefix
    /// ([`reject_number_hit`]).
    #[serde(default)]
    pub reject_numbers: Vec<RejectEntry>,
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
    /// Cap of a persona-mode call judged sales / scam (the stall cap), minutes
    /// (clamped 3–30; recording caps at 30). Normal calls have their own cap,
    /// `persona_normal_max_min`.
    #[serde(default = "default_stall_max_min")]
    pub persona_stall_max_min: u32,
    /// Cap of a normal persona-mode call, minutes (clamped 3–30). Normal calls
    /// should wrap up around 4½ minutes → default 5.
    #[serde(default = "default_normal_max_min")]
    pub persona_normal_max_min: u32,
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
    /// `assistant` = the AI says it is the owner's AI assistant and takes a
    /// message; `persona` = it answers as the owner (and still admits to being
    /// an AI when sincerely asked).
    #[serde(default = "default_answer_style")]
    pub answer_style: String,
    /// Facts the AI may answer from (opening hours, address, what to tell
    /// couriers…). Anything not written here it must not make up.
    #[serde(default)]
    pub knowledge_notes: String,
    /// Switch to the caller's language when it is not the agent's.
    #[serde(default)]
    pub reply_in_caller_language: bool,
    /// Country calling code of the phone's SIM, digits only (`886`, `1`, `81`):
    /// how blocked numbers written with `+…` match the national form.
    #[serde(default = "default_country_code")]
    pub country_code: String,
    /// Taiwan carriers play a recorded warning at the start of every call from
    /// abroad; the AI waits for it before its first words. `auto` = on when the
    /// language is zh-TW, or `on` / `off`.
    #[serde(default = "default_auto")]
    pub intl_carrier_prompt: String,
    /// OpenAI-compatible chat endpoint for the post-call summary, e.g.
    /// `https://api.openai.com/v1`. Empty = DeepSeek when `deepseek_api_key` is
    /// set, else Gemini with the Live API key.
    #[serde(default)]
    pub summary_api_base: String,
    /// Model for the post-call summary; empty = the endpoint's default.
    #[serde(default)]
    pub summary_model: String,
    /// Which service writes the post-call summary: `auto` (the endpoint above,
    /// else DeepSeek, else Gemini, else OpenRouter), `gemini` (Gemini's
    /// lowest-cost model with the Live API key) or `deepseek`. A choice without
    /// its key works like `auto` ([`crate::deepseek::summary_endpoint_for`]).
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
    /// Names speech-to-text keeps getting wrong, one `wrong=right` per entry
    /// (`黃先生=王先生`): applied to transcripts and summaries.
    #[serde(default)]
    pub name_corrections: Vec<String>,
    /// Your own names for phones: the id a phone reports (`pixel-10123`) → the
    /// label shown in notifications and the web console. Wins over the model
    /// the phone reports itself.
    #[serde(default)]
    pub device_labels: std::collections::BTreeMap<String, String>,
    /// Where calls, recordings and settings are kept (`CALLS_DATA_DIR`, from the
    /// env base). Not a setting: never shown or saved.
    #[serde(skip)]
    pub calls_data_dir: String,
}

pub const STALL_MAX_MIN_DEFAULT: u32 = 30;
pub const NORMAL_MAX_MIN_DEFAULT: u32 = 5;
const STALL_MAX_MIN_FLOOR: u32 = 3;
const STALL_MAX_MIN_CEIL: u32 = 30;
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

fn default_auto() -> String {
    "auto".to_string()
}

fn default_mdns_name() -> String {
    "bageo".to_string()
}

fn default_appointment_mode() -> String {
    "off".to_string()
}

fn default_reject_mode() -> String {
    "decline".to_string()
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

/// `auto` / `gemini` / `deepseek`; anything else is auto.
fn sanitize_summary_provider(s: &str) -> String {
    crate::deepseek::Provider::parse(s).as_str().to_string()
}

fn sanitize_answer_style(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "persona" => "persona".to_string(),
        _ => default_answer_style(),
    }
}

fn sanitize_tristate(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        l @ ("on" | "off") => l.to_string(),
        _ => default_auto(),
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
            deepseek_api_key: env_or("DEEPSEEK_API_KEY", &base.deepseek_api_key),
            owner_status_enabled: env_bool("OWNER_STATUS_ENABLED", false),
            owner_status_text: env_or("OWNER_STATUS_TEXT", ""),
            gemini_persona_mode: env_bool("GEMINI_PERSONA_MODE", true),
            // Empty = the built-in prompt of the language and answer style.
            gemini_persona_prompt: env_or("GEMINI_PERSONA_PROMPT", ""),
            answer_unknown_callers: env_bool("ANSWER_UNKNOWN_CALLERS", true),
            reject_numbers: clean_reject_list(
                split_list(&env_or("REJECT_NUMBERS", ""))
                    .into_iter()
                    .map(|line| RejectEntry::from(RejectEntryRepr::Line(line)))
                    .collect(),
            ),
            reject_voice_name: env_or("REJECT_VOICE_NAME", ""),
            reject_mode: sanitize_reject_mode(&env_or("REJECT_MODE", "decline")),
            persona_stall_spam: env_bool("PERSONA_STALL_SPAM", true),
            persona_stall_max_min: env_parse("PERSONA_STALL_MAX_MIN", STALL_MAX_MIN_DEFAULT),
            persona_normal_max_min: env_parse("PERSONA_NORMAL_MAX_MIN", NORMAL_MAX_MIN_DEFAULT),
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
            answer_style: sanitize_answer_style(&env_or("ANSWER_STYLE", "assistant")),
            knowledge_notes: env_or("KNOWLEDGE_NOTES", ""),
            reply_in_caller_language: env_bool("REPLY_IN_CALLER_LANGUAGE", false),
            country_code: digits_only(&env_or("COUNTRY_CODE", "")),
            intl_carrier_prompt: sanitize_tristate(&env_or("INTL_CARRIER_PROMPT", "auto")),
            summary_api_base: env_or("SUMMARY_API_BASE", ""),
            summary_model: env_or("SUMMARY_MODEL", ""),
            summary_provider: sanitize_summary_provider(&env_or("SUMMARY_PROVIDER", "auto")),
            notify_channels: Vec::new(),
            public_url: env_or("PUBLIC_URL", ""),
            mdns_name: base.mdns_name.clone(),
            calendar_suggest: env_bool("CALENDAR_SUGGEST", true),
            calendar_app: String::new(),
            appointment_mode: sanitize_appointment_mode(&env_or("APPOINTMENT_MODE", "off")),
            appointment_sms: env_bool("APPOINTMENT_SMS", false),
            appointment_reply: crate::appointments::sanitize_reply_setting(&env_or("APPOINTMENT_REPLY", "auto")),
            name_corrections: split_list(&env_or("NAME_CORRECTIONS", "")),
            device_labels: std::collections::BTreeMap::new(),
            calls_data_dir: base.calls_data_dir.clone(),
        }
    }

    /// Hand the settings the call helpers read as process-wide state to them
    /// (start-up and every save).
    pub fn apply_globals(&self) {
        crate::locale::set_active(self.locale_active());
        crate::deepseek::set_provider(self.summary_provider());
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
    pub fn summary_provider(&self) -> crate::deepseek::Provider {
        crate::deepseek::Provider::parse(&self.summary_provider)
    }

    /// Blocked numbers are refused while they ring (`reject_mode` `decline`).
    pub fn reject_declines(&self) -> bool {
        self.reject_mode != "announce"
    }

    pub fn is_assistant_style(&self) -> bool {
        self.answer_style != "persona"
    }

    /// Country calling code: the setting, else Taiwan's for zh-TW, else none.
    pub fn effective_country_code(&self) -> String {
        let c = digits_only(&self.country_code);
        if !c.is_empty() {
            c
        } else if self.lang() == crate::locale::Lang::ZhTw {
            "886".to_string()
        } else {
            String::new()
        }
    }

    /// The AI holds its first words for the carrier's international-call warning.
    pub fn intl_carrier_prompt_on(&self) -> bool {
        match self.intl_carrier_prompt.as_str() {
            "on" => true,
            "off" => false,
            _ => self.lang() == crate::locale::Lang::ZhTw,
        }
    }

    /// What the Live session's helpers read (`locale::set_active`).
    pub fn locale_active(&self) -> crate::locale::Active {
        crate::locale::Active {
            lang: self.lang(),
            notify_lang: self.notify_lang(),
            assistant: self.is_assistant_style(),
            owner: self.owner_name.trim().to_string(),
            country_code: self.effective_country_code(),
            name_fixes: crate::locale::parse_name_fixes(&self.name_corrections),
        }
    }

    fn call_prompt(&self, stall: crate::gemini_live::StallPrompt) -> String {
        crate::prompts::build(&crate::prompts::Ctx {
            lang: self.lang(),
            assistant: self.is_assistant_style(),
            owner: &self.owner_name,
            custom: &self.gemini_persona_prompt,
            status: if self.owner_status_enabled { &self.owner_status_text } else { "" },
            stall,
            knowledge: &self.knowledge_notes,
            carrier_note: self.intl_carrier_prompt_on(),
            auto_language: self.reply_in_caller_language,
        })
    }

    /// The built-in prompt for the current language, style and owner name
    /// (what an empty `gemini_persona_prompt` means).
    pub fn default_prompt(&self) -> String {
        crate::prompts::default_prompt(self.lang(), self.is_assistant_style(), &self.owner_name)
    }

    /// System prompt the session switches to once a call is judged sales / scam.
    pub fn persona_stall_system_prompt(&self) -> String {
        self.call_prompt(crate::gemini_live::StallPrompt::Full)
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
        if self.gemini_persona_mode {
            use crate::gemini_live::StallPrompt;
            return self.call_prompt(if self.persona_stall_spam {
                StallPrompt::ListenFirst
            } else {
                StallPrompt::Off
            });
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
            deepseek_api_key,
            owner_status_text,
            gemini_persona_prompt,
            ambience_kind,
            gemini_thinking_level,
            gemini_stall_model,
            language,
            notify_language,
            sim_languages,
            owner_name,
            answer_style,
            knowledge_notes,
            country_code,
            intl_carrier_prompt,
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
        );
        macro_rules! set_copy {
            ($($f:ident),+ $(,)?) => {$(
                if let Some(v) = ov.$f { self.$f = v; }
            )+};
        }
        set_copy!(
            ai_live_enabled,
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
            persona_stall_spam,
            persona_stall_max_min,
            persona_normal_max_min,
            gemini_server_vad,
            gemini_barge_in,
            answer_ring_ms,
            gemini_ring_prewarm,
            ambience_level,
            reply_in_caller_language,
            calendar_suggest,
            appointment_sms,
        );
        self.ivr_start_delay_ms = self
            .ivr_start_delay_ms
            .min(crate::config::IVR_FIRST_AUDIO_DELAY_CAP_MS);
        self.answer_ring_ms = self.answer_ring_ms.min(ANSWER_RING_MS_CEIL);
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
        self.summary_provider = sanitize_summary_provider(&self.summary_provider);
        self.answer_style = sanitize_answer_style(&self.answer_style);
        self.intl_carrier_prompt = sanitize_tristate(&self.intl_carrier_prompt);
        self.reject_mode = sanitize_reject_mode(&self.reject_mode);
        self.country_code = digits_only(&self.country_code);
        self.mdns_name = crate::mdns::parse_name_setting(&self.mdns_name);
        self.appointment_mode = sanitize_appointment_mode(&self.appointment_mode);
        self.calendar_app = crate::calendar::sanitize_app(&self.calendar_app);
        self.appointment_reply = crate::appointments::sanitize_reply_setting(&self.appointment_reply);
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
        if !self.deepseek_api_key.trim().is_empty() {
            s.deepseek_api_key = self.deepseek_api_key.clone();
        }
        s.notify_channels = self.notify_channels.clone();
        s.summary_api_base = self.summary_api_base.clone();
        s.summary_model = self.summary_model.clone();
        s.gemini_api_key = self.gemini_api_key.clone().into();
        if !self.public_url.trim().is_empty() {
            s.public_url = self.public_url.clone();
        }
        // Empty is a choice here (mDNS off), so it always applies.
        s.mdns_name = self.mdns_name.clone();
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
/// what the owner typed in "Address of this console"; else, when the agent is
/// open to the LAN, `http://<mdns_name>.local:<port>/` while the mDNS
/// responder answers, or `http://<LAN IP>:<port>/`. Empty when the agent
/// listens on this computer only and nothing was typed (a link to localhost
/// would not open on a phone), and in a container: there the name and the
/// address are Docker's own network, which other devices cannot reach.
pub fn effective_console_url(s: &Settings) -> String {
    let listen = if in_container() {
        None
    } else {
        s.host.trim().parse::<std::net::IpAddr>().ok()
    };
    let host = crate::mdns::sanitize_name(&s.mdns_name);
    let mdns_on = !host.is_empty() && crate::mdns::is_active();
    console_url(&s.public_url, listen, s.port, &host, mdns_on, crate::mdns::answer_ip)
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
    let listen = s.host.trim().parse::<std::net::IpAddr>().ok();
    console_url("", listen, s.port, "", false, crate::mdns::answer_ip)
}

fn console_url(
    public_url: &str,
    listen: Option<std::net::IpAddr>,
    port: u16,
    mdns_host: &str,
    mdns_on: bool,
    lan_ip: impl FnOnce(std::net::IpAddr) -> Option<std::net::Ipv4Addr>,
) -> String {
    let own = public_url.trim();
    if !own.is_empty() {
        return own.to_string();
    }
    let Some(listen) = listen.filter(|ip| !ip.is_loopback()) else {
        return String::new();
    };
    if mdns_on {
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
    deepseek_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_status_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_status_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_persona_mode: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini_persona_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    answer_unknown_callers: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_stall_spam: Option<bool>,
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
    answer_style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    knowledge_notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reply_in_caller_language: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    country_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    intl_carrier_prompt: Option<String>,
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
    name_corrections: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_labels: Option<std::collections::BTreeMap<String, String>>,
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
        let mut overrides = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Overrides>(&text) {
                Ok(o) => {
                    info!("runtime settings overrides loaded path={}", path.display());
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
        // An `auto` from before the Google Calendar connection was removed is
        // `confirm` now, and is saved so.
        if let Some(m) = overrides.appointment_mode.take() {
            overrides.appointment_mode = Some(sanitize_appointment_mode(&m));
        }
        let mut rt = RuntimeSettings::from_base(&base);
        rt.apply(&overrides);
        Arc::new(Self {
            base,
            path,
            state: RwLock::new(StoreState {
                overrides,
                current: Arc::new(rt),
            }),
        })
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

    /// Apply a JSON patch from PUT /calls/api/settings, persist, hot-swap.
    /// Persist failure keeps the previous state (error returned to caller).
    pub fn apply_patch(&self, patch: &Value) -> Result<Arc<RuntimeSettings>> {
        let mut st = self.state.write().unwrap_or_else(|e| e.into_inner());
        let mut ov = st.overrides.clone();
        patch_overrides(&mut ov, patch);
        let mut rt = RuntimeSettings::from_base(&self.base);
        rt.apply(&ov);

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        }
        let data = serde_json::to_vec_pretty(&ov)?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &data).with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rename to {}", self.path.display()))?;

        st.overrides = ov;
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
        let current = self.snapshot();
        let norm = normalize_number(trimmed);
        if current
            .spam_blocklist
            .iter()
            .any(|x| normalize_number(x) == norm)
        {
            return Ok(current);
        }
        let mut list = current.spam_blocklist.clone();
        list.push(trimmed.to_string());
        self.apply_patch(&json!({ "spam_blocklist": list }))
    }
}

const SECRET_FIELDS: [&str; 11] = [
    "openrouter_api_key",
    "telegram_bot_token",
    "gmail_app_password",
    "openclaw_token",
    "trtc_secret_key",
    "trtc_secret_id",
    "trtc_cloud_secret_key",
    "trtc_llm_config",
    "trtc_tts_config",
    "gemini_api_key",
    "deepseek_api_key",
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
    m.insert(
        "deepseek_summary_ready".into(),
        json!(!rt.deepseek_api_key.trim().is_empty()),
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
            }))
            .collect::<Vec<_>>()),
    );
    m.insert("default_prompt".into(), json!(rt.default_prompt()));
    // The same with `{owner}` left in, for copying into the editor.
    m.insert(
        "default_prompt_template".into(),
        json!(crate::prompts::default_prompt_template(rt.lang(), rt.is_assistant_style())),
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
                || !rt.deepseek_api_key.trim().is_empty()
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
    if let Some(v) = patch.get("reject_numbers") {
        // Rows {number, reason} from the settings page (older one-line strings too).
        if let Ok(list) = serde_json::from_value::<Vec<RejectEntry>>(v.clone()) {
            ov.reject_numbers = Some(clean_reject_list(list));
        }
    }
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
    p_secret(patch, "deepseek_api_key", &mut ov.deepseek_api_key);
    p_bool(patch, "owner_status_enabled", &mut ov.owner_status_enabled);
    p_bool(patch, "gemini_persona_mode", &mut ov.gemini_persona_mode);
    p_string(patch, "gemini_persona_prompt", &mut ov.gemini_persona_prompt);
    p_bool(patch, "answer_unknown_callers", &mut ov.answer_unknown_callers);
    p_bool(patch, "persona_stall_spam", &mut ov.persona_stall_spam);
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
    p_string(patch, "answer_style", &mut ov.answer_style);
    if let Some(v) = ov.answer_style.take() {
        ov.answer_style = Some(sanitize_answer_style(&v));
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
    p_string(patch, "intl_carrier_prompt", &mut ov.intl_carrier_prompt);
    if let Some(v) = ov.intl_carrier_prompt.take() {
        ov.intl_carrier_prompt = Some(sanitize_tristate(&v));
    }
    p_string(patch, "summary_api_base", &mut ov.summary_api_base);
    p_string(patch, "summary_model", &mut ov.summary_model);
    p_string(patch, "summary_provider", &mut ov.summary_provider);
    if let Some(v) = ov.summary_provider.take() {
        ov.summary_provider = Some(sanitize_summary_provider(&v));
    }
    p_string(patch, "public_url", &mut ov.public_url);
    p_string(patch, "mdns_name", &mut ov.mdns_name);
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

/// Shortest prefix a `…*` entry may have: `0*` / `02*` would turn whole areas away.
const REJECT_PREFIX_MIN_DIGITS: usize = 4;

/// The caller is on the blocked-numbers list: same national number, or it starts with
/// an entry ending in `*`. Hidden / garbled numbers never match.
pub fn reject_number_hit(list: &[RejectEntry], caller: &str) -> bool {
    reject_entry_for(list, caller).is_some()
}

/// Why the caller's number is on the blocked-numbers list, when a reason was given.
pub fn reject_reason(list: &[RejectEntry], caller: &str) -> Option<String> {
    reject_entry_for(list, caller)
        .map(|e| e.reason.trim().to_string())
        .filter(|r| !r.is_empty())
}

fn reject_entry_for<'a>(list: &'a [RejectEntry], caller: &str) -> Option<&'a RejectEntry> {
    let c = national_digits(caller);
    if c.len() < 5 {
        return None;
    }
    list.iter().find(|e| match e.number.trim().strip_suffix('*') {
        Some(prefix) => {
            let p = national_digits(prefix);
            p.len() >= REJECT_PREFIX_MIN_DIGITS
                && (c.starts_with(&p)
                    || without_leading_zeros(&c).starts_with(without_leading_zeros(&p)))
        }
        None => {
            let n = national_digits(&e.number);
            n.len() >= 5 && same_number(&n, &c)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Prefixes shorter than 4 digits and a bare `*` turn nobody away.
        assert!(!reject_number_hit(&list, "0223456789"));
        assert!(!reject_number_hit(&list, "0255501235"));
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
    fn reject_numbers_persist_through_a_patch() {
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-settings-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        let rt = store
            .apply_patch(&json!({ "reject_numbers": [
                { "number": "02-5550-1234", "reason": " 安信 " },
                { "number": "  ", "reason": "沒有號碼的列不存" },
                { "number": "0912345678" },
                { "number": "0255501234", "reason": "重複的不存" }
            ] }))
            .unwrap();
        assert_eq!(rt.reject_numbers, vec![entry("0255501234", "安信"), entry("0912345678", "")]);
        // Saved: a restart keeps the list.
        let again = SettingsStore::load(base);
        assert!(reject_number_hit(&again.snapshot().reject_numbers, "+886255501234"));
        assert_eq!(reject_reason(&again.snapshot().reject_numbers, "0255501234").as_deref(), Some("安信"));
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
                "gemini_persona_mode": false,
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
        // Every call starts short; the full stall block only after the switch.
        let first = rt.gemini_system_prompt_for_call();
        assert!(first.contains("讓他繼續講") && !first.contains("陪他繞"));
        assert!(!first.contains("喔不用了，謝謝"));
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
        assert!(!p.contains("陪他繞"));
        assert!(p.contains("喔不用了，謝謝"));
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
            console_url(" https://calls.example.com ", any, 8787, "bageo", true, lan),
            "https://calls.example.com"
        );
        assert_eq!(
            console_url("http://10.0.0.5:8787", local, 8787, "bageo", false, lan),
            "http://10.0.0.5:8787"
        );
        // Open to the LAN: the network name while it answers, else the IP.
        assert_eq!(console_url("", any, 8787, "bageo", true, lan), "http://bageo.local:8787/");
        assert_eq!(console_url("", any, 9000, "bageo", false, lan), "http://192.168.1.20:9000/");
        assert_eq!(console_url("", any, 8787, "bageo", false, no_lan), "");
        // This computer only: no link a phone could open.
        assert_eq!(console_url("", local, 8787, "bageo", true, lan), "");
        assert_eq!(console_url("", None, 8787, "bageo", true, lan), "");
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
    fn summary_provider_defaults_auto_and_sanitizes() {
        use crate::deepseek::Provider;
        let dir = std::env::temp_dir().join(format!(
            "bageo-call-summary-provider-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp settings dir");
        let mut base = Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = SettingsStore::load(base.clone());
        assert_eq!(store.snapshot().summary_provider, "auto");
        assert_eq!(store.snapshot().summary_provider(), Provider::Auto);
        assert_eq!(store.masked_json()["summary_provider"], "auto");
        let rt = store.apply_patch(&json!({ "summary_provider": " Gemini " })).unwrap();
        assert_eq!((rt.summary_provider.as_str(), rt.summary_provider()), ("gemini", Provider::Gemini));
        // Survives a restart (settings.json override); another key leaves it alone.
        let again = SettingsStore::load(base.clone());
        assert_eq!(again.snapshot().summary_provider, "gemini");
        let rt = again.apply_patch(&json!({ "summary_model": "gemini-2.5-flash" })).unwrap();
        assert_eq!(rt.summary_provider, "gemini");
        let rt = again.apply_patch(&json!({ "summary_provider": "deepseek" })).unwrap();
        assert_eq!(rt.summary_provider(), Provider::DeepSeek);
        let rt = again.apply_patch(&json!({ "summary_provider": "openai" })).unwrap();
        assert_eq!(rt.summary_provider, "auto");
        assert_eq!(SettingsStore::load(base).snapshot().summary_provider, "auto");
        // A settings file written before either key existed: auto, and the
        // notifications in the AI's language.
        let old_dir = dir.join("old");
        std::fs::create_dir_all(old_dir.join("config")).expect("old settings dir");
        std::fs::write(
            old_dir.join("config").join("settings.json"),
            r#"{"language":"fr","summary_model":"deepseek-chat"}"#,
        )
        .expect("old settings.json");
        let mut old_base = Settings::from_env();
        old_base.calls_data_dir = old_dir.to_string_lossy().to_string();
        let old = SettingsStore::load(old_base).snapshot();
        assert_eq!(old.summary_model, "deepseek-chat");
        assert_eq!((old.summary_provider.as_str(), old.notify_language.as_str()), ("auto", ""));
        assert_eq!(old.notify_lang(), crate::locale::Lang::Fr);
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
}

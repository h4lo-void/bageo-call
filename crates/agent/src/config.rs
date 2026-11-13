use std::env;

use chrono::Timelike;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMode {
    /// IVR: loop Piper menu → DTMF → hold / stub / hangup (no STT/LLM).
    Transfer,
    /// Alias for [`Self::Transfer`] (`AGENT_MODE=ivr` / `ivr_transfer`).
    IvrTransfer,
    /// Full STT → LLM → TTS path.
    Llm,
}

impl AgentMode {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "llm" => Self::Llm,
            "ivr_transfer" | "ivr" => Self::IvrTransfer,
            _ => Self::Transfer,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::IvrTransfer => "ivr_transfer",
            Self::Llm => "llm",
        }
    }

    /// Transfer / IVR path (hold music + DTMF menu); not LLM.
    pub fn is_transfer(&self) -> bool {
        matches!(self, Self::Transfer | Self::IvrTransfer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmProvider {
    Ollama,
    OpenRouter,
}

impl LlmProvider {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "openrouter" => Self::OpenRouter,
            _ => Self::Ollama,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::OpenRouter => "openrouter",
        }
    }
}

/// Spoken transfer greeting body (clone period WAVs preferred). Product: auto-AI, no menu keys.
pub const DEFAULT_IVR_PROMPT: &str = "這裡是自動轉接系統。正在為您轉接專人，請稍後。";

/// Hard ceiling for answer → first outgoing product PCM (greeting / reject).
/// OFFHOOK needs some settle; 0 is historically risky — product band is ~300–400ms.
/// Does **not** touch `post_greet_ai_delay_ms` (full greet play + 2s before Gemini).
pub const IVR_FIRST_AUDIO_DELAY_CAP_MS: u32 = 400;

/// Asia/Taipei (+08) wall clock for domestic TRTC / Piper menu greeting.
pub fn ivr_menu_now_plus8() -> chrono::DateTime<chrono::FixedOffset> {
    let east = chrono::FixedOffset::east_opt(8 * 3600).expect("fixed +08");
    chrono::Utc::now().with_timezone(&east)
}

/// Time-of-day greeting in zh (早上好／中午好／下午好／晚上好／您好).
pub fn ivr_menu_greeting(hour: u32) -> &'static str {
    match hour {
        5..=10 => "早上好",
        11..=13 => "中午好",
        14..=17 => "下午好",
        18..=21 => "晚上好",
        _ => "您好",
    }
}

/// Asset basename for the greeting clip (under `/app/assets/`).
pub fn ivr_greet_wav_basename(hour: u32) -> &'static str {
    match hour {
        5..=10 => "ivr_greet_morning.wav",
        11..=13 => "ivr_greet_noon.wav",
        14..=17 => "ivr_greet_afternoon.wav",
        18..=21 => "ivr_greet_evening.wav",
        _ => "ivr_greet_hello.wav",
    }
}

/// Single-clip full menu (greeting + body) by time of day — preferred when present
/// so clone MP3s that already include the greeting are not double-greeted.
/// Late night / early morning (no dedicated hello clip) falls back to evening.
pub fn ivr_menu_full_wav_basename(hour: u32) -> &'static str {
    match hour {
        5..=10 => "ivr_menu_full_morning.wav",
        11..=13 => "ivr_menu_full_noon.wav",
        14..=17 => "ivr_menu_full_afternoon.wav",
        18..=21 => "ivr_menu_full_evening.wav",
        // 22–4: use the evening clip, so a missing `ivr_menu_full_hello.wav` does not fall through silently to Piper
        _ => "ivr_menu_full_evening.wav",
    }
}

/// Build the full spoken menu: `{greeting}。{body}` (no "the time is now…" line).
pub fn format_ivr_menu_spoken(body: &str) -> String {
    format_ivr_menu_spoken_at(body, ivr_menu_now_plus8())
}

pub fn format_ivr_menu_spoken_at(body: &str, now: chrono::DateTime<chrono::FixedOffset>) -> String {
    let body = body.trim();
    let body = if body.is_empty() {
        DEFAULT_IVR_PROMPT
    } else {
        body
    };
    let greeting = ivr_menu_greeting(now.hour());
    format!("{greeting}。{body}")
}

/// Secret string whose [`Debug`] never prints plaintext (`""` or `"***"`).
/// Matches [`crate::trtc::TrtcSettings`] redaction.
#[derive(Clone, Default)]
pub struct SecretString(String);

impl SecretString {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same shape as TrtcSettings: empty stays empty, else "***".
        std::fmt::Debug::fmt(if self.0.is_empty() { "" } else { "***" }, f)
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for SecretString {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::ops::Deref for SecretString {
    type Target = str;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub agent_mode: AgentMode,
    /// Piper fallback when hold MP3 missing (`TRANSFER_USE_HOLD_MUSIC=false` / decode fail).
    pub transfer_prompt: String,
    /// Spoken IVR menu (Piper zh). Prefer `IVR_PROMPT_TEXT`, then `TRANSFER_PROMPT_TEXT`.
    pub ivr_prompt_text: String,
    /// Digit that starts hold music once → fail → goodbye (default `1`).
    pub transfer_dtmf_trigger: char,
    /// Digit for voicemail stub then hangup (default `2`).
    pub ivr_dtmf_voicemail: char,
    /// Digits that play goodbye + send WS `hangup` (default `0#`).
    pub ivr_dtmf_end: String,
    /// Goodbye phrase before hangup.
    pub ivr_goodbye_text: String,
    /// Spoken immediately after DTMF 1 while TRTC AI connects.
    pub ivr_dtmf1_wait_text: String,
    /// First AI line after connect (Piper / ServerPushText).
    pub ivr_ai_hello_text: String,
    /// Nudge if caller silent after hello.
    pub ivr_ai_nudge_text: String,
    /// Silence after hello before nudge (ms).
    pub ivr_ai_silence_before_nudge_ms: u32,
    /// Silence after nudge before goodbye hangup (ms).
    pub ivr_ai_silence_after_nudge_ms: u32,
    /// Stub phrase for DTMF `2` until real voicemail exists.
    pub ivr_voicemail_stub_text: String,
    /// Legacy: post-menu DTMF wait. Product is auto-AI after greeting — unused for re-menu.
    pub ivr_reprompt_ms: u32,
    /// Legacy name: when false, behave like `IVR_MAX_MENU_PLAYS=1` unless max is set higher.
    /// Prefer `IVR_MAX_MENU_PLAYS` for the 2× menu then goodbye flow.
    pub ivr_loop: bool,
    /// Max menu WAV plays before no-DTMF goodbye + hangup (default 2).
    pub ivr_max_menu_plays: u32,
    /// After hello/media ready, wait this many ms before first period greeting
    /// (OFFHOOK settle; hard-capped by [`IVR_FIRST_AUDIO_DELAY_CAP_MS`]).
    pub ivr_start_delay_ms: u32,
    /// After period greets fully finish on the phone path, wait this many ms before unmuting AI.
    /// Default 2000 — the product's 2 s gap setting; not answer→greet delay.
    pub post_greet_ai_delay_ms: u32,
    /// After the goodbye turn, wait this many ms then hang up (default 2000).
    pub goodbye_hangup_delay_ms: u32,
    /// Comma-separated pre-recorded menu WAVs under `/app/assets/` (played in order, segment-by-segment).
    pub ivr_prompt_wavs: String,
    /// Failure / leave-message clip after hold music ends (or DTMF 2).
    pub ivr_fail_wav: String,
    /// Goodbye WAV after max menu plays with no DTMF (or DTMF 0/#). Empty → Piper text.
    pub ivr_goodbye_wav: String,
    /// Pre-recorded reject for unknown/private/hidden caller (no Gemini Live).
    pub ivr_reject_unknown_wav: String,
    /// Spoken text paired with `ivr_reject_unknown_wav` (log / reply_text only).
    pub ivr_reject_unknown_text: String,
    /// Static menu body WAV (no greeting). Paired with time-of-day `ivr_greet_*.wav`.
    pub ivr_menu_body_wav: String,
    /// DTMF1 connect-wait clip (optional; missing → Piper text).
    pub ivr_dtmf1_wait_wav: String,
    /// AI first-hello clip (optional; missing → Piper text).
    pub ivr_ai_hello_wav: String,
    /// AI silence-nudge clip (optional; missing → Piper text).
    pub ivr_ai_nudge_wav: String,
    /// Voicemail intro WAV after DTMF 2 (before beep). Piper `ivr_vm_intro_text` fallback.
    pub ivr_vm_intro_wav: String,
    pub ivr_vm_intro_text: String,
    /// Post-recording callback-confirm WAV ("need a call back on this number? press 1").
    pub ivr_vm_confirm_wav: String,
    pub ivr_vm_confirm_text: String,
    /// Piper ack after the caller presses 1 to confirm a callback.
    pub vm_reply_ack_text: String,
    /// Max voicemail message length (ms).
    pub vm_max_ms: u32,
    /// Trailing silence that ends the voicemail recording (ms).
    pub vm_silence_ms: u32,
    /// Frame RMS below this counts as silence while recording a voicemail.
    pub vm_silence_rms: f32,
    /// How long to wait for DTMF after the callback-confirm prompt (ms).
    pub vm_confirm_wait_ms: u32,
    /// Deprecated: wait-for-remote-voice removed from IVR path. Kept for health/compat; default false.
    pub transfer_wait_for_remote_voice: bool,
    /// Unused for IVR loop (compat).
    pub transfer_voice_ms: u32,
    /// Unused for IVR loop (compat).
    pub transfer_voice_timeout_ms: u32,
    /// Prefer MP3 hold music after DTMF `1` (default true). Falls back to Piper if missing.
    pub transfer_use_hold_music: bool,
    /// Path to hold-music MP3 (decoded to 16 kHz s16le mono at startup).
    pub transfer_hold_music: String,
    pub llm_provider: LlmProvider,
    pub ollama_url: String,
    pub ollama_model: String,
    pub openrouter_api_key: String,
    pub openrouter_model: String,
    pub openrouter_base_url: String,
    pub openrouter_http_referer: String,
    pub openrouter_app_title: String,
    /// DeepSeek API key for the post-call notification summary only (not live talk).
    pub deepseek_api_key: String,
    pub piper_url: String,
    pub whisper_url: String,
    pub whisper_model: String,
    pub sample_rate: u32,
    pub vad_rms_threshold: f32,
    pub silence_ms: u32,
    pub min_utterance_ms: u32,
    pub max_utterance_ms: u32,
    pub system_prompt: String,
    /// Directory for call index + WAV (`/data/calls` in Docker).
    pub calls_data_dir: String,
    /// Persist call metadata + continuous mixed WAV (remote+TTS, silence-padded).
    pub calls_record: bool,
    /// Also save TTS/hold PCM we stream to the phone.
    pub calls_record_tts: bool,
    /// Retain call metadata and audio for this many days (default 7).
    pub calls_retention_days: u32,
    /// Optional token for `/calls` UI/API. Empty = LAN-only open (compose binds LAN IP).
    pub calls_ui_token: String,
    /// Send Telegram/webhook when a call session starts (hello / ringing).
    pub call_notify: bool,
    /// Apprise gateway base, e.g. `http://notify-apprise:8000`.
    pub notify_gateway_url: String,
    /// Apprise config key (tag) the notification goes to, e.g. `normal`.
    pub notify_gateway_tag: String,
    /// Generic JSON webhook fallback (`title`/`body`/`caller_id`/`serial`/`text`).
    pub notify_webhook_url: String,
    /// Direct Telegram Bot API fallback (only if gateway/webhook unset or fail).
    pub telegram_bot_token: String,
    pub telegram_chat_id: String,
    /// Tencent TRTC SDKAppID (0 = unset). Secret never logged.
    pub trtc_sdk_app_id: u32,
    /// SDK secret key for UserSig (TLSSigAPIv2) — do not commit / Debug-redacted.
    pub trtc_secret_key: SecretString,
    /// Cloud API / CAM SecretId (AKID…) for TC3 Start/Stop/ControlAIConversation.
    pub trtc_secret_id: SecretString,
    /// Cloud API / CAM SecretKey for TC3 signing — distinct from SDK UserSig key.
    pub trtc_cloud_secret_key: SecretString,
    /// Prefer TRTC for menu DTMF-1 AI talk when credentials present.
    pub trtc_ai_enabled: bool,
    /// UserSig TTL seconds (default 1 day).
    pub trtc_usersig_expire_secs: u64,
    /// `X-TC-Region` for TRTC REST (e.g. ap-guangzhou).
    pub trtc_ai_region: String,
    /// Optional host override (`trtc.tencentcloudapi.com` / intl).
    pub trtc_api_host: String,
    /// When true and host empty → `trtc.intl.tencentcloudapi.com`.
    pub trtc_api_intl: bool,
    /// Opaque JSON string for StartAIConversation `LLMConfig` (never commit / log).
    pub trtc_llm_config: SecretString,
    /// Opaque JSON string for StartAIConversation `TTSConfig` (never commit / log).
    pub trtc_tts_config: SecretString,
    pub trtc_stt_language: String,
    pub trtc_ai_max_idle_secs: u32,
    /// Master switch for sidecar media IPC (`TRTC_BRIDGE_ENABLED`).
    pub trtc_bridge_enabled: bool,
    /// Sidecar base URL (`TRTC_BRIDGE_URL`), e.g. `http://127.0.0.1:8791`.
    pub trtc_bridge_url: String,
    /// Notification channels from the settings page (runtime settings only).
    pub notify_channels: Vec<crate::notify::Channel>,
    /// Address of the web console for links in notifications (`PUBLIC_URL`).
    pub public_url: String,
    /// Name this computer answers to on the LAN, `<name>.local` (`MDNS_NAME`,
    /// default `myna`; empty = off). Only while the agent is open to the LAN.
    pub mdns_name: String,
    /// OpenAI-compatible endpoint for the post-call summary (`SUMMARY_API_BASE`).
    pub summary_api_base: String,
    /// Model for the post-call summary (`SUMMARY_MODEL`).
    pub summary_model: String,
    /// Gemini API key: the summary falls back to it when nothing else is set.
    pub gemini_api_key: SecretString,
    /// When set, phones must connect with `/ws?token=<this>` (`DEVICE_TOKEN`).
    /// Needed once the agent listens beyond this machine (Wi-Fi phones).
    pub device_token: String,
}

impl Settings {
    pub fn from_env() -> Self {
        Self {
            // This machine only by default; `AGENT_HOST=0.0.0.0` opens it to the LAN
            // (set `CALLS_UI_TOKEN` and `DEVICE_TOKEN` then).
            host: env_or("AGENT_HOST", "127.0.0.1"),
            port: env_parse("AGENT_PORT", 8787u16),
            agent_mode: AgentMode::parse(&env_or("AGENT_MODE", "ivr")),
            transfer_prompt: env_or("TRANSFER_PROMPT", "轉接中，請稍候。"),
            ivr_prompt_text: env_first(
                &["IVR_PROMPT_TEXT", "TRANSFER_PROMPT_TEXT"],
                DEFAULT_IVR_PROMPT,
            ),
            transfer_dtmf_trigger: env_digit("TRANSFER_DTMF_TRIGGER", '1'),
            // Matches clone full-menu WAVs (press 2 to leave a message). Detector uses high_margin so 1≠2.
            ivr_dtmf_voicemail: env_digit("IVR_DTMF_VOICEMAIL", '2'),
            // Empty = no DTMF end keys. Callers hang up the phone if they want to leave
            // (0/# false triggers used to force a goodbye mid-call).
            ivr_dtmf_end: env_or("IVR_DTMF_END", ""),
            ivr_goodbye_text: env_or("IVR_GOODBYE_TEXT", "感謝您的來電 謝謝 再見"),
            ivr_dtmf1_wait_text: env_or("IVR_DTMF1_WAIT_TEXT", "請稍等一下 Hold on just a moment"),
            ivr_ai_hello_text: env_or("IVR_AI_HELLO_TEXT", "您好請問有什麼事情嗎"),
            ivr_ai_nudge_text: env_or("IVR_AI_NUDGE_TEXT", "在嗎，您好請問有什麼事情嗎"),
            ivr_ai_silence_before_nudge_ms: env_parse("IVR_AI_SILENCE_BEFORE_NUDGE_MS", 3000u32),
            ivr_ai_silence_after_nudge_ms: env_parse("IVR_AI_SILENCE_AFTER_NUDGE_MS", 5000u32),
            ivr_voicemail_stub_text: env_or("IVR_VOICEMAIL_STUB_TEXT", "留言功能稍後開放"),
            // Gap after each menu ends before re-play / goodbye (default 3s).
            // IVR_DTMF_WAIT_MS is an alias for IVR_REPROMPT_MS.
            ivr_reprompt_ms: env_parse_first(&["IVR_REPROMPT_MS", "IVR_DTMF_WAIT_MS"], 3000u32),
            ivr_loop: env_bool("IVR_LOOP", true),
            ivr_max_menu_plays: {
                let n = env_parse("IVR_MAX_MENU_PLAYS", 2u32);
                if !env_bool("IVR_LOOP", true) && env::var("IVR_MAX_MENU_PLAYS").is_err() {
                    1
                } else {
                    n.max(1)
                }
            },
            // Settle telephony uplink after answer before greeting PCM.
            // Hard ceiling: answer→first product PCM ≤ IVR_FIRST_AUDIO_DELAY_CAP_MS.
            // post_greet_ai_delay (default 2s) is separate — after full greeting, before Gemini.
            ivr_start_delay_ms: env_parse("IVR_START_DELAY_MS", IVR_FIRST_AUDIO_DELAY_CAP_MS)
                .min(IVR_FIRST_AUDIO_DELAY_CAP_MS),
            post_greet_ai_delay_ms: env_parse("POST_GREET_AI_DELAY_MS", 2000u32),
            goodbye_hangup_delay_ms: env_parse("GOODBYE_HANGUP_DELAY_MS", 2000u32),
            ivr_prompt_wavs: env_or("IVR_PROMPT_WAVS", ""),
            ivr_fail_wav: env_or("IVR_FAIL_WAV", &asset_path("ivr_05_fail.wav")),
            // New short clone goodbye; set empty in env to disable WAV and use Piper text.
            ivr_goodbye_wav: env_or("IVR_GOODBYE_WAV", &asset_path("ivr_06_goodbye.wav")),
            ivr_reject_unknown_wav: env_or("IVR_REJECT_UNKNOWN_WAV", &asset_path("ivr_reject_unknown.wav")),
            ivr_reject_unknown_text: env_or(
                "IVR_REJECT_UNKNOWN_TEXT",
                "無法為此號碼提供服務。感謝您的來電。再見。",
            ),
            ivr_menu_body_wav: env_or("IVR_MENU_BODY_WAV", &asset_path("ivr_menu_body.wav")),
            ivr_dtmf1_wait_wav: env_or("IVR_DTMF1_WAIT_WAV", &asset_path("ivr_dtmf1_wait.wav")),
            ivr_ai_hello_wav: env_or("IVR_AI_HELLO_WAV", &asset_path("ivr_ai_hello.wav")),
            ivr_ai_nudge_wav: env_or("IVR_AI_NUDGE_WAV", &asset_path("ivr_ai_nudge.wav")),
            ivr_vm_intro_wav: env_or("IVR_VM_INTRO_WAV", &asset_path("ivr_07_vm_intro.wav")),
            ivr_vm_intro_text: env_or(
                "IVR_VM_INTRO_TEXT",
                "請在嗶聲後留言，我會幫您轉達。說完請按井字鍵。",
            ),
            ivr_vm_confirm_wav: env_or("IVR_VM_CONFIRM_WAV", &asset_path("ivr_08_vm_confirm.wav")),
            ivr_vm_confirm_text: env_or(
                "IVR_VM_CONFIRM_TEXT",
                "如果需要我們回撥您來電的這個號碼，請按一。不需要回覆的話，可以直接掛斷，謝謝。",
            ),
            vm_reply_ack_text: env_or("VM_REPLY_ACK_TEXT", "好的，已記下您的回電需求。"),
            vm_max_ms: env_parse("VM_MAX_MS", 120_000u32),
            vm_silence_ms: env_parse("VM_SILENCE_MS", 6000u32),
            vm_silence_rms: env_parse("VM_SILENCE_RMS", 260.0f32),
            vm_confirm_wait_ms: env_parse("VM_CONFIRM_WAIT_MS", 7000u32),
            // Removed from product path; default false so health shows wait=false.
            transfer_wait_for_remote_voice: env_bool("TRANSFER_WAIT_FOR_REMOTE_VOICE", false),
            transfer_voice_ms: env_parse("TRANSFER_VOICE_MS", 300u32),
            transfer_voice_timeout_ms: env_parse("TRANSFER_VOICE_TIMEOUT_MS", 0u32),
            transfer_use_hold_music: env_bool("TRANSFER_USE_HOLD_MUSIC", true),
            transfer_hold_music: env_or("TRANSFER_HOLD_MUSIC", &asset_path("hold_transfer.wav")),
            llm_provider: LlmProvider::parse(&env_or("LLM_PROVIDER", "ollama")),
            ollama_url: env_or("OLLAMA_URL", "http://127.0.0.1:11434"),
            ollama_model: env_or("OLLAMA_MODEL", "qwen2.5:7b"),
            openrouter_api_key: env_or("OPENROUTER_API_KEY", ""),
            openrouter_model: env_or("OPENROUTER_MODEL", "openai/gpt-4o-mini"),
            openrouter_base_url: env_or("OPENROUTER_BASE_URL", "https://openrouter.ai/api/v1"),
            openrouter_http_referer: env_or(
                "OPENROUTER_HTTP_REFERER",
                "https://github.com/h4lo-void/myna-call",
            ),
            openrouter_app_title: env_or("OPENROUTER_APP_TITLE", "myna-call"),
            deepseek_api_key: env_or("DEEPSEEK_API_KEY", ""),
            piper_url: env_or("PIPER_URL", "http://127.0.0.1:10200"),
            whisper_url: env_or("WHISPER_URL", ""),
            whisper_model: env_or("WHISPER_MODEL", "base"),
            // Default 16000 — must match trtc_bridge::PCM_SAMPLE_RATE (agent↔sidecar contract).
            sample_rate: env_parse("SAMPLE_RATE", 16000u32),
            vad_rms_threshold: env_parse("VAD_RMS_THRESHOLD", 350.0f32),
            silence_ms: env_parse("SILENCE_MS", 800u32),
            min_utterance_ms: env_parse("MIN_UTTERANCE_MS", 400u32),
            max_utterance_ms: env_parse("MAX_UTTERANCE_MS", 15000u32),
            system_prompt: env_or(
                "SYSTEM_PROMPT",
                "你是 AI 助理，用簡短中文口語回覆，一兩句話即可。",
            ),
            calls_data_dir: env_or("CALLS_DATA_DIR", &default_data_dir()),
            calls_record: env_bool("CALLS_RECORD", true),
            calls_record_tts: env_bool("CALLS_RECORD_TTS", true),
            calls_retention_days: env_parse("CALLS_RETENTION_DAYS", 7u32).clamp(1, 3650),
            calls_ui_token: env_or("CALLS_UI_TOKEN", ""),
            call_notify: env_bool("CALL_NOTIFY", true),
            notify_gateway_url: env_or("NOTIFY_GATEWAY_URL", ""),
            notify_gateway_tag: env_or("NOTIFY_GATEWAY_TAG", "normal"),
            notify_webhook_url: env_or("NOTIFY_WEBHOOK_URL", ""),
            telegram_bot_token: env_or("TELEGRAM_BOT_TOKEN", ""),
            telegram_chat_id: env_or("TELEGRAM_CHAT_ID", ""),
            trtc_sdk_app_id: env_parse("TRTC_SDK_APP_ID", 0u32),
            trtc_secret_key: env_or("TRTC_SECRET_KEY", "").into(),
            trtc_secret_id: env_or("TRTC_SECRET_ID", "").into(),
            trtc_cloud_secret_key: env_or("TRTC_CLOUD_SECRET_KEY", "").into(),
            // Default on: menu DTMF 1 → TRTC AI when credentials present.
            // false / missing keys → soft silence or brief Piper, then hangup (not hold theater).
            trtc_ai_enabled: env_bool("TRTC_AI_ENABLED", true),
            trtc_usersig_expire_secs: env_parse("TRTC_USERSIG_EXPIRE_SECS", 86_400u64),
            trtc_ai_region: env_or("TRTC_AI_REGION", "ap-guangzhou"),
            trtc_api_host: env_or("TRTC_API_HOST", ""),
            trtc_api_intl: env_bool("TRTC_API_INTL", false),
            // Opaque JSON strings — set them in the deployment's environment only; never commit real values.
            trtc_llm_config: env_or("TRTC_LLM_CONFIG", "").into(),
            trtc_tts_config: env_or("TRTC_TTS_CONFIG", "").into(),
            trtc_stt_language: env_or("TRTC_STT_LANGUAGE", "zh"),
            trtc_ai_max_idle_secs: env_parse("TRTC_AI_MAX_IDLE_SECS", 120u32).clamp(60, 259_200),
            trtc_bridge_enabled: env_bool("TRTC_BRIDGE_ENABLED", false),
            // Prefer TRTC_BRIDGE_URL; accept TRTC_BRIDGE_SIDECAR_URL alias from docs.
            trtc_bridge_url: {
                let primary = env::var("TRTC_BRIDGE_URL").unwrap_or_default();
                if !primary.trim().is_empty() {
                    primary
                } else {
                    env_or("TRTC_BRIDGE_SIDECAR_URL", "")
                }
            },
            notify_channels: Vec::new(),
            public_url: env_or("PUBLIC_URL", ""),
            mdns_name: crate::mdns::sanitize_name(&env_or("MDNS_NAME", "myna")),
            summary_api_base: env_or("SUMMARY_API_BASE", ""),
            summary_model: env_or("SUMMARY_MODEL", ""),
            gemini_api_key: env_or("GEMINI_API_KEY", "").into(),
            device_token: env_or("DEVICE_TOKEN", ""),
        }
    }

    pub fn trtc(&self) -> crate::trtc::TrtcSettings {
        let mut t = crate::trtc::TrtcSettings::from_parts(
            self.trtc_sdk_app_id,
            self.trtc_secret_key.clone(),
            self.trtc_ai_enabled,
            self.trtc_usersig_expire_secs,
        );
        t.secret_id = self.trtc_secret_id.clone();
        t.cloud_secret_key = self.trtc_cloud_secret_key.clone();
        // Product: mainland domestic TRTC only (ap-guangzhou / trtc.tencentcloudapi.com).
        t.api_region = if self.trtc_ai_region.trim().is_empty() {
            "ap-guangzhou".into()
        } else {
            self.trtc_ai_region.clone()
        };
        t.api_host = self.trtc_api_host.clone();
        t.api_use_intl = false;
        t.llm_config = self.trtc_llm_config.clone();
        t.tts_config = self.trtc_tts_config.clone();
        t.stt_language = self.trtc_stt_language.clone();
        t.ai_max_idle_secs = self.trtc_ai_max_idle_secs;
        t.bridge_enabled = self.trtc_bridge_enabled;
        t.bridge_url = self.trtc_bridge_url.clone();
        t
    }
}

/// Where the bundled sounds live: `ASSETS_DIR`, else `assets/` in the working
/// directory, next to the program, or one / two levels above it (a source
/// checkout), else the container path.
pub fn assets_dir() -> std::path::PathBuf {
    if let Ok(dir) = env::var("ASSETS_DIR") {
        if !dir.trim().is_empty() {
            return std::path::PathBuf::from(dir.trim());
        }
    }
    let mut candidates = vec![std::path::PathBuf::from("assets")];
    if let Ok(exe) = env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..4 {
            let Some(d) = dir else { break };
            candidates.push(d.join("assets"));
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    candidates
        .into_iter()
        .find(|c| c.is_dir())
        .unwrap_or_else(|| std::path::PathBuf::from("/app/assets"))
}

/// A bundled sound by file name.
pub fn asset_path(name: &str) -> String {
    assets_dir().join(name).to_string_lossy().into_owned()
}

/// Where calls, recordings and settings are kept unless `CALLS_DATA_DIR` says
/// otherwise: the container's volume when it is there, else `data/calls` in
/// the working directory.
fn default_data_dir() -> String {
    if std::path::Path::new("/data/calls").is_dir() {
        "/data/calls".to_string()
    } else {
        "data/calls".to_string()
    }
}

pub(crate) fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_first(keys: &[&str], default: &str) -> String {
    for k in keys {
        if let Ok(v) = env::var(k) {
            let t = v.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    default.to_string()
}

fn env_digit(key: &str, default: char) -> char {
    env::var(key)
        .ok()
        .and_then(|v| v.trim().chars().next())
        .filter(|c| c.is_ascii_digit() || matches!(c, '*' | '#' | 'A'..='D' | 'a'..='d'))
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or(default)
}

pub(crate) fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_parse_first<T: std::str::FromStr>(keys: &[&str], default: T) -> T {
    for k in keys {
        if let Ok(v) = env::var(k) {
            if let Ok(parsed) = v.parse() {
                return parsed;
            }
        }
    }
    default
}

pub(crate) fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone};

    #[test]
    fn ivr_menu_greeting_by_hour() {
        assert_eq!(ivr_menu_greeting(8), "早上好");
        assert_eq!(ivr_menu_greeting(12), "中午好");
        assert_eq!(ivr_menu_greeting(15), "下午好");
        assert_eq!(ivr_menu_greeting(20), "晚上好");
        assert_eq!(ivr_menu_greeting(2), "您好");
        assert_eq!(ivr_menu_full_wav_basename(8), "ivr_menu_full_morning.wav");
        assert_eq!(ivr_menu_full_wav_basename(12), "ivr_menu_full_noon.wav");
        assert_eq!(
            ivr_menu_full_wav_basename(15),
            "ivr_menu_full_afternoon.wav"
        );
        assert_eq!(ivr_menu_full_wav_basename(20), "ivr_menu_full_evening.wav");
        // 22–4: evening, not morning
        assert_eq!(ivr_menu_full_wav_basename(2), "ivr_menu_full_evening.wav");
        assert_eq!(ivr_menu_full_wav_basename(23), "ivr_menu_full_evening.wav");
    }

    #[test]
    fn format_ivr_menu_spoken_greeting_only_no_clock() {
        let east = FixedOffset::east_opt(8 * 3600).unwrap();
        let now = east.with_ymd_and_hms(2026, 8, 3, 9, 5, 0).unwrap();
        let s = format_ivr_menu_spoken_at(DEFAULT_IVR_PROMPT, now);
        assert!(s.starts_with("早上好。"));
        assert!(!s.contains("現在是"));
    }

    #[test]
    fn settings_debug_redacts_trtc_secret_key() {
        let mut settings = Settings::from_env();
        let distinctive = "unit-test-trtc-secret-KEY-do-not-leak-9f3a";
        settings.trtc_secret_key = distinctive.into();
        let rendered = format!("{settings:?}");
        assert!(
            !rendered.contains(distinctive),
            "Settings Debug must not contain trtc_secret_key plaintext"
        );
        assert!(
            rendered.contains("***"),
            "expected redacted secret marker in Debug output"
        );
    }

    #[test]
    fn sample_rate_default_is_16000() {
        // Agent↔sidecar PCM contract (see trtc_bridge::PCM_SAMPLE_RATE).
        assert_eq!(crate::trtc_bridge::PCM_SAMPLE_RATE, 16_000);
        let src = include_str!("config.rs");
        assert!(
            src.contains("env_parse(\"SAMPLE_RATE\", 16000u32)"),
            "SAMPLE_RATE default must stay 16000"
        );
    }
}

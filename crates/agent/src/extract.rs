// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Voicemail transcription (Whisper) and structured intent extraction (OpenRouter).
//!
//! # Public API
//! - [`ExtractedIntent`] — parsed task fields from a voicemail transcript
//! - [`transcribe_voicemail_wav`] — WAV → PCM → Whisper text
//! - [`extract_intent`] — transcript → JSON intent (OpenRouter or heuristic fallback)
//! - [`process_voicemail_file`] — both steps in one call
//! - [`extract_spoken_contact`] — quick phone/email pick for live VM read-back
//!
//! Heuristic path (no OpenRouter / no Ollama): keyword labels
//! `Message|Callback|Schedule|Question` plus optional `datetime_hint`.
//! Spam hangup gate (`is_spam_text*`) stays off.
//!
//! # System prompt (OpenRouter)
//! See [`INTENT_SYSTEM_PROMPT`]: the model returns one JSON object with
//! `intent`, `summary`, `datetime_hint`, `reply_method`, `reply_contact` and
//! `confidence`; `caller_id` is given for reference only.

use std::path::Path;

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::Settings;
use crate::locale::{self, Lang};
use crate::services::{resample_pcm16, transcribe_pcm16};

/// Traditional-Chinese spam / telemarketing phrases (keyword path; no OpenRouter required).
pub const SPAM_KEYWORDS_ZH: &[&str] = &[
    "貸款",
    "投資",
    "保險",
    "優惠專線",
    "恭喜中獎",
    "房貸",
    "降額",
    "廣告",
    "信用卡",
    "債務協商",
    "免費領取",
    "抽獎",
    "辦卡",
    "利率優惠",
    "推銷",
    "健康食品",
    "綠能光電",
    "補助專案",
];

/// Resolve effective keywords: empty custom list → built-in defaults.
pub fn effective_spam_keywords(custom: &[String]) -> Vec<&str> {
    if custom.iter().any(|s| !s.trim().is_empty()) {
        custom
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        SPAM_KEYWORDS_ZH.to_vec()
    }
}

/// Count how many spam keywords appear in `text` (built-in defaults).
pub fn spam_keyword_hits(text: &str) -> u32 {
    spam_keyword_hits_with(text, &[])
}

/// Count hits using custom keywords when non-empty; otherwise built-in defaults.
pub fn spam_keyword_hits_with(text: &str, custom_keywords: &[String]) -> u32 {
    effective_spam_keywords(custom_keywords)
        .iter()
        .filter(|kw| text.contains(**kw))
        .count() as u32
}

/// Keyword spam filter removed — always false.
pub fn is_spam_text(_text: &str) -> bool {
    false
}

/// Keyword spam filter removed — always false.
pub fn is_spam_text_with(_text: &str, _custom_keywords: &[String]) -> bool {
    false
}

/// Contact parsed from a live voicemail transcript for DTMF read-back.
#[derive(Debug, Clone)]
pub struct SpokenContact {
    pub method: String,
    pub contact: String,
    /// Spoken form for Piper (may differ from normalized `contact`).
    pub display: String,
}

/// Structured intent parsed from a voicemail transcript.
#[derive(Debug, Clone)]
pub struct ExtractedIntent {
    pub intent: String,
    pub summary: String,
    pub datetime_hint: Option<String>,
    pub reply_method: Option<String>,
    pub reply_contact: Option<String>,
    pub confidence: f32,
    pub raw_json: Value,
}

/// Strict JSON-only system prompt for OpenRouter intent extraction, Traditional
/// Chinese; [`intent_system_prompt`] picks the one for the agent's language.
pub const INTENT_SYSTEM_PROMPT: &str = r#"你是電話留言意圖分析器。分析來電者留言，只輸出單一 JSON 物件（不要 markdown、不要解釋）。

欄位：
- intent: Message | Callback | Schedule | Question | Spam | Other
- summary: 一句話繁體中文摘要
- datetime_hint: 提到的日期時間（ISO 或自然語言），沒有則 null
- reply_method: phone | email | none
- reply_contact: 對方留下的電話或 email；沒有則 null
- confidence: 0.0 到 1.0 的浮點數

reply_method=phone 且未留聯絡方式時，可將 reply_contact 設為提供的 caller_id。"#;

/// The same instructions for every language other than Chinese; `{language}`
/// is the language the summary is written in.
const INTENT_SYSTEM_PROMPT_EN: &str = r#"You analyse a voicemail a caller left. Output exactly one JSON object (no markdown, no explanation).

Fields:
- intent: Message | Callback | Schedule | Question | Spam | Other
- summary: a one-sentence summary written in {language}
- datetime_hint: the date or time mentioned (ISO or plain words), or null
- reply_method: phone | email | none
- reply_contact: the phone number or email the caller left, or null
- confidence: a number from 0.0 to 1.0

If reply_method is phone and the caller left no contact, reply_contact may be the given caller_id."#;

/// The intent prompt with the summary asked for in `lang`: Traditional Chinese
/// for zh-TW, Simplified for zh-CN, otherwise the language named in English.
pub fn intent_system_prompt(lang: Lang) -> String {
    match lang {
        Lang::ZhTw => INTENT_SYSTEM_PROMPT.to_string(),
        Lang::ZhCn => crate::zh_convert::to_cn(
            &INTENT_SYSTEM_PROMPT.replace("一句話繁體中文摘要", "一句話簡體中文摘要"),
        ),
        other => INTENT_SYSTEM_PROMPT_EN.replace("{language}", locale::phrases(other).name_en),
    }
}

/// What the intent model reads: the caller ID and the message, labelled in the
/// prompt's language.
fn intent_user_body(lang: Lang, caller_id: &str, transcript: &str) -> String {
    let label = match lang {
        Lang::ZhTw => "留言內容：",
        Lang::ZhCn => "留言内容：",
        _ => "Message:",
    };
    format!("caller_id: {caller_id}\n\n{label}\n{transcript}")
}

/// Read a voicemail WAV, decode to PCM, and transcribe via Whisper.
pub async fn transcribe_voicemail_wav(
    http: &Client,
    settings: &Settings,
    wav_path: &Path,
) -> Result<String> {
    let bytes = tokio::fs::read(wav_path)
        .await
        .with_context(|| format!("read voicemail wav {}", wav_path.display()))?;
    let (pcm, sample_rate) = decode_wav_pcm(&bytes, settings.sample_rate)
        .with_context(|| format!("decode voicemail wav {}", wav_path.display()))?;
    transcribe_pcm16(http, settings, &pcm, sample_rate).await
}

/// Extract structured intent from a transcript: OpenRouter when its key is
/// given, else the summary service when one is set, else a keyword heuristic.
///
/// `spam_keywords` empty → built-in ZH defaults (no OpenRouter required for spam label).
pub async fn extract_intent(
    http: &Client,
    settings: &Settings,
    openrouter_key: &str,
    openrouter_model: &str,
    transcript: &str,
    caller_id: &str,
    spam_keywords: &[String],
) -> Result<ExtractedIntent> {
    let trimmed = transcript.trim();
    let user_body = intent_user_body(locale::notify_lang(), caller_id, trimmed);
    let raw_text = if !openrouter_key.trim().is_empty() {
        chat_openrouter_intent(http, settings, openrouter_key.trim(), openrouter_model.trim(), user_body).await?
    } else if let Some(endpoint) = crate::summary::summary_endpoint(settings) {
        let system = intent_system_prompt(locale::notify_lang());
        let model = endpoint.models.first().cloned().unwrap_or_default();
        match crate::summary::chat_at(http, &endpoint, &model, &system, &user_body, 256).await {
            Ok(text) => text,
            // The service is busy or unreachable: the keywords still give an answer.
            Err(e) => {
                warn!("voicemail intent: summary service failed ({e:#}); using heuristic");
                return Ok(heuristic_intent_with(trimmed, caller_id, spam_keywords));
            }
        }
    } else {
        return Ok(heuristic_intent_with(trimmed, caller_id, spam_keywords));
    };

    match parse_intent_json(&raw_text, caller_id) {
        Ok(intent) => Ok(intent),
        Err(e) => {
            warn!("intent JSON parse failed ({e:#}); using heuristic");
            Ok(heuristic_intent_with(trimmed, caller_id, spam_keywords))
        }
    }
}

/// Transcribe a voicemail WAV (Whisper) then extract intent.
///
/// The intent as [extract_intent] reads it (a keyword heuristic with the
/// transcript excerpt as summary when no model is set). Whisper must be
/// reachable for transcription to succeed.
pub async fn process_voicemail_file(
    http: &Client,
    settings: &Settings,
    openrouter_key: &str,
    openrouter_model: &str,
    wav_path: &Path,
    caller_id: &str,
    spam_keywords: &[String],
) -> Result<(String, ExtractedIntent)> {
    let transcript = transcribe_voicemail_wav(http, settings, wav_path).await?;
    info!(
        "voicemail transcribed path={} chars={} caller={}",
        wav_path.display(),
        transcript.len(),
        crate::urgent_call::masked(caller_id)
    );
    let intent = extract_intent(
        http,
        settings,
        openrouter_key,
        openrouter_model,
        &transcript,
        caller_id,
        spam_keywords,
    )
    .await?;
    info!(
        "voicemail intent={} reply_method={:?} confidence={}",
        intent.intent, intent.reply_method, intent.confidence
    );
    Ok((transcript, intent))
}

/// Decode RIFF/WAVE PCM16 to s16le mono at `target_rate`.
fn decode_wav_pcm(bytes: &[u8], target_rate: u32) -> Result<(Vec<u8>, u32)> {
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        let (pcm, src_rate, channels) = parse_wav_pcm16(bytes)?;
        let pcm = resample_pcm16(&pcm, src_rate, target_rate, channels);
        if pcm.is_empty() {
            bail!("decoded empty PCM from WAV");
        }
        return Ok((pcm, target_rate));
    }
    // Raw s16le mono fallback (unlikely for saved voicemails).
    if bytes.len() >= 2 && bytes.len() % 2 == 0 {
        return Ok((bytes.to_vec(), target_rate));
    }
    bail!("unsupported audio: not PCM WAV or aligned s16le")
}

fn parse_wav_pcm16(bytes: &[u8]) -> Result<(Vec<u8>, u32, u16)> {
    if bytes.len() < 44 || &bytes[8..12] != b"WAVE" {
        bail!("not a WAVE file");
    }
    let mut pos = 12usize;
    let mut rate = 0u32;
    let mut channels = 1u16;
    let mut bits = 16u16;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let start = pos + 8;
        let end = start.saturating_add(size);
        if end > bytes.len() {
            break;
        }
        if id == b"fmt " && size >= 16 {
            channels = u16::from_le_bytes(bytes[start + 2..start + 4].try_into().unwrap());
            rate = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap());
            bits = u16::from_le_bytes(bytes[start + 14..start + 16].try_into().unwrap());
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        pos = end + (size % 2);
    }
    let pcm = data.context("WAV missing data chunk")?;
    if bits != 16 {
        bail!("unsupported WAV bits={bits} (need 16)");
    }
    if rate == 0 {
        bail!("WAV missing sample rate");
    }
    Ok((pcm.to_vec(), rate, channels.max(1)))
}

async fn chat_openrouter_intent(
    http: &Client,
    settings: &Settings,
    key: &str,
    model: &str,
    user_text: String,
) -> Result<String> {
    let model = if model.is_empty() {
        settings.openrouter_model.as_str()
    } else {
        model
    };
    let system = intent_system_prompt(locale::notify_lang());
    let payload = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user_text},
        ],
        "stream": false,
        "temperature": 0.2,
        "max_tokens": 256,
    });
    let url = format!(
        "{}/chat/completions",
        settings.openrouter_base_url.trim_end_matches('/')
    );
    let resp = http
        .post(&url)
        .bearer_auth(key)
        .header("HTTP-Referer", &settings.openrouter_http_referer)
        .header("X-Title", &settings.openrouter_app_title)
        .json(&payload)
        .send()
        .await
        .context("openrouter intent chat")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("openrouter intent status {status}: {body}");
    }
    let data: Value = resp.json().await.context("openrouter intent json")?;
    Ok(data
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string())
}

fn heuristic_intent(transcript: &str, caller_id: &str) -> ExtractedIntent {
    heuristic_intent_with(transcript, caller_id, &[])
}

fn heuristic_intent_with(
    transcript: &str,
    caller_id: &str,
    spam_keywords: &[String],
) -> ExtractedIntent {
    let summary: String = transcript.to_string()
        .chars()
        .take(120)
        .collect();
    // Spam hangup gate stays off (`is_spam_text*` always false); keyword hits still logged.
    let spam = is_spam_text_with(transcript, spam_keywords);
    let (intent_label, confidence) = if spam {
        ("Spam", 0.75_f32)
    } else {
        infer_intent_keywords(transcript)
    };
    let reply_method = if spam {
        None
    } else {
        infer_reply_method_keywords(transcript)
    };
    let reply_contact = match reply_method.as_deref() {
        Some("phone") if !caller_id.trim().is_empty() => Some(caller_id.trim().to_string()),
        Some("email") => extract_email_hint(transcript),
        _ => None,
    };
    let datetime_hint = if spam {
        None
    } else {
        extract_datetime_hint_keywords(transcript)
    };
    let raw_json = json!({
        "intent": intent_label,
        "summary": summary,
        "datetime_hint": datetime_hint,
        "reply_method": reply_method,
        "reply_contact": reply_contact,
        "confidence": confidence,
        "source": "heuristic",
        "spam_keyword_hits": spam_keyword_hits_with(transcript, spam_keywords),
    });
    ExtractedIntent {
        intent: intent_label.to_string(),
        summary,
        datetime_hint,
        reply_method,
        reply_contact,
        confidence,
        raw_json,
    }
}

/// Keyword intent labels without OpenRouter/Ollama.
/// Priority: Schedule → Callback → Question → Message.
fn infer_intent_keywords(text: &str) -> (&'static str, f32) {
    if looks_schedule(text) {
        return ("Schedule", 0.55);
    }
    if looks_callback(text) {
        return ("Callback", 0.55);
    }
    if looks_question(text) {
        return ("Question", 0.50);
    }
    ("Message", 0.35)
}

fn looks_callback(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "回電", "回电", "回撥", "回拨", "回個電話", "回个电话", "打給我", "打给我", "請打給", "请打给",
        "有空打", "方便時來電", "方便时来电",
        "call back", "callback", "call me back", "ring me",
        // ja, ko
        "折り返し", "お電話", "電話ください", "전화 주세요", "전화주세요", "다시 전화", "연락 주세요", "연락주세요",
        // es, fr, de, pt, it
        "devuelva la llamada", "llámeme", "llameme", "llámame", "llamame",
        "rappelez", "rappelle-moi", "rappeler", "rappel",
        "rufen sie zurück", "ruf mich zurück", "rückruf", "ruckruf",
        "me liga", "retorne a ligação", "retornar a ligação", "ligue de volta",
        "mi richiami", "richiamami", "richiami",
        // ru, vi, id, th, hi, ar
        "перезвоните", "перезвони", "обратный звонок",
        "gọi lại", "goi lai", "liên hệ lại",
        "telepon balik", "hubungi kembali", "telpon lagi",
        "โทรกลับ", "ติดต่อกลับ",
        "कॉल बैक", "वापस कॉल", "फ़ोन करें", "फोन करें",
        "عاود الاتصال", "معاودة الاتصال", "اتصل بي", "أعد الاتصال",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

fn looks_schedule(text: &str) -> bool {
    let lower = text.to_lowercase();
    let schedule_verb = [
        "預約", "预约", "約見", "约见", "約個時間", "约个时间", "約時間", "约时间", "安排時間", "安排时间",
        "排個時間", "排个时间", "改期", "改約", "改约", "開會", "开会", "會議", "会议", "面談", "面谈",
        "會面", "会面", "拜訪", "拜访", "看診", "看诊",
        "appointment", "reschedule", "schedule a", "schedule an", "book a meeting", "set up a meeting",
        // ja, ko, es, fr, de, pt, it, ru, vi, id, th, hi, ar
        "予約", "打ち合わせ", "예약", "미팅", "일정",
        "cita", "reunión", "reunion", "rendez-vous", "réunion", "termin", "besprechung",
        "marcar", "reunião", "reuniao", "consulta", "appuntamento", "riunione",
        "встреча", "запись", "совещание", "cuộc hẹn", "đặt lịch", "janji", "jadwal",
        "นัดหมาย", "นัดเจอ", "अपॉइंटमेंट", "मीटिंग", "موعد", "اجتماع",
    ]
    .iter()
    .any(|k| lower.contains(k))
        || (lower.contains("meeting")
            && (lower.contains("tomorrow")
                || lower.contains("next week")
                || lower.contains("monday")
                || lower.contains("tuesday")
                || lower.contains("wednesday")
                || lower.contains("thursday")
                || lower.contains("friday")));
    if schedule_verb {
        return true;
    }
    // Datetime + soft schedule cue (方便/約/來) without explicit callback-only phrasing.
    let has_when = extract_datetime_hint_keywords(text).is_some();
    let soft_cue = text.contains("方便")
        || text.contains("約在")
        || text.contains("约在")
        || text.contains("約了")
        || text.contains("约了")
        || text.contains("約個")
        || text.contains("约个")
        || text.contains("想約")
        || text.contains("想约")
        || text.contains("見面")
        || text.contains("见面")
        || text.contains("過來")
        || text.contains("过来")
        || lower.contains("available")
        || lower.contains("free at");
    has_when && soft_cue && !looks_callback(text)
}

fn looks_question(text: &str) -> bool {
    let lower = text.to_lowercase();
    text.contains('？')
        || text.contains('?')
        || text.contains('؟')
        || [
            "請問", "请问", "想問", "想问", "請教", "请教", "有沒有", "有没有", "能不能", "可不可以",
            "是否", "如何", "怎麼", "怎么",
            "do you", "can you", "could you", "what time", "how much", "is it possible",
            "教えてください", "でしょうか", "여쭤", "문의", "있나요", "되나요",
            "puedo", "puede", "cuánto", "cuanto", "pouvez-vous", "est-ce que",
            "können sie", "wie viel", "wann", "você pode", "quanto",
            "vorrei sapere", "posso", "можно", "подскажите", "сколько",
            "cho hỏi", "có thể", "bao nhiêu", "apakah", "berapa", "bisa",
            "สอบถาม", "เท่าไหร่", "ได้ไหม", "क्या आप", "कितना", "कब", "هل يمكن", "كم", "متى",
        ]
        .iter()
        .any(|k| lower.contains(k))
}

/// Pull a short natural-language datetime snippet for GCal hint parsing.
fn extract_datetime_hint_keywords(text: &str) -> Option<String> {
    const MARKERS: &[&str] = &[
        "大後天",
        "大后天",
        "後天",
        "后天",
        "明天",
        "明日",
        "今天",
        "今日",
        "下星期一",
        "下星期二",
        "下星期三",
        "下星期四",
        "下星期五",
        "下星期六",
        "下星期日",
        "下星期天",
        "下週一",
        "下週二",
        "下週三",
        "下週四",
        "下週五",
        "下週六",
        "下週日",
        "下周一",
        "下周二",
        "下周三",
        "下周四",
        "下周五",
        "下周六",
        "下周日",
        "下星期",
        "下週",
        "下周",
        "星期一",
        "星期二",
        "星期三",
        "星期四",
        "星期五",
        "星期六",
        "星期日",
        "星期天",
        "週一",
        "週二",
        "週三",
        "週四",
        "週五",
        "週六",
        "週日",
        "周一",
        "周二",
        "周三",
        "周四",
        "周五",
        "周六",
        "周日",
        "上午",
        "下午",
        "中午",
        "晚上",
        "凌晨",
        "tomorrow",
        "next week",
        "monday",
        "tuesday",
        "wednesday",
        "thursday",
        "friday",
        "saturday",
        "sunday",
    ];
    let lower = text.to_ascii_lowercase();
    let mut best: Option<(usize, usize)> = None; // start, end in char indices via byte? use char positions
    let chars: Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = lower.chars().collect();
    for marker in MARKERS {
        let mchars: Vec<char> = marker.chars().collect();
        if mchars.is_empty() {
            continue;
        }
        let hay: &[char] = if marker.is_ascii() {
            &lower_chars
        } else {
            &chars
        };
        if let Some(pos) = find_subslice(hay, &mchars) {
            let start = pos.saturating_sub(0);
            let end = (pos + mchars.len() + 12).min(chars.len());
            let span = (start, end);
            best = Some(match best {
                None => span,
                Some(cur) => {
                    if span.1 - span.0 > cur.1 - cur.0 {
                        span
                    } else {
                        cur
                    }
                }
            });
        }
    }
    // HH:MM / H點 style (ASCII digits + 點/点/:).
    if let Some((s, e)) = find_clock_span(&chars) {
        best = Some(match best {
            None => (s, e),
            Some((bs, be)) => (bs.min(s), be.max(e)),
        });
    }
    best.map(|(s, e)| chars[s..e].iter().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
}

fn find_subslice(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (0..=(hay.len() - needle.len())).find(|&i| hay[i..i + needle.len()] == needle[..])
}

fn find_clock_span(chars: &[char]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i < chars.len() && matches!(chars[i], ':' | '：' | '點' | '点') {
                let sep = i;
                i += 1;
                let mut digits_after = 0usize;
                while i < chars.len() && chars[i].is_ascii_digit() && digits_after < 2 {
                    i += 1;
                    digits_after += 1;
                }
                // Accept "3點" / "15:30" / "3：00".
                if chars[sep] == '點' || chars[sep] == '点' || digits_after > 0 {
                    return Some((start, i));
                }
            }
        } else {
            i += 1;
        }
    }
    None
}

fn infer_reply_method_keywords(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if looks_callback(text)
        || text.contains("打給")
        || text.contains("打给")
        || lower.contains("call me")
    {
        return Some("phone".to_string());
    }
    if text.contains('@')
        || [
            "email", "e-mail", "信箱", "郵件", "邮件", "メール", "이메일", "메일", "correo", "courriel",
            "e-mail-adresse", "почт", "thư điện tử", "surat elektronik", "อีเมล", "ईमेल", "بريد إلكتروني",
        ]
        .iter()
        .any(|k| lower.contains(k))
    {
        return Some("email".to_string());
    }
    None
}

/// Pull an alternate phone or email from a short Whisper transcript.
/// Returns `None` when nothing usable is found or the phone matches `caller_id`.
///
/// Accepts ASCII digits, full-width digits, and common Mandarin phone digits
/// (零/〇/洞、一/幺、二…); Piper `display` spaces digits / spells `@`·`.` for TTS.
/// With `region == None`, only Taiwan mobile/landline or `+` international numbers match.
/// With `region == Some(iso)`, valid domestic numbers for that region (validated via
/// [`crate::phone_region`]) also match.
pub fn extract_spoken_contact(transcript: &str, caller_id: &str) -> Option<SpokenContact> {
    extract_spoken_contact_in(transcript, caller_id, None)
}

pub fn extract_spoken_contact_in(
    transcript: &str,
    caller_id: &str,
    region: Option<&str>,
) -> Option<SpokenContact> {
    let trimmed = transcript.trim();
    if trimmed.is_empty() {
        return None;
    }
    let folded = fold_spoken_contact_text(trimmed);
    if let Some(email) = extract_email_hint(trimmed).or_else(|| extract_email_hint(&folded)) {
        return Some(SpokenContact {
            method: "email".into(),
            display: email_readback_display(&email),
            contact: email,
        });
    }
    // The raw text: folding turns 。 into a dot, which would join two numbers.
    if let Some(phone) = extract_phone_hint_in(trimmed, region) {
        let spoken = normalize_phone_digits(&phone);
        let caller = normalize_phone_digits(caller_id);
        if spoken.len() >= 8 && !phones_equivalent(&spoken, &caller) {
            return Some(SpokenContact {
                method: "phone".into(),
                display: phone_readback_display(&spoken),
                contact: phone,
            });
        }
    }
    None
}

/// Map Mandarin / full-width phone digits (and a few spoken email words) to ASCII.
fn fold_spoken_contact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if let Some(d) = spoken_digit_char(ch) {
            out.push(d);
        } else {
            out.push(ch);
        }
    }
    // Longer / more specific tokens first (avoid partial clobber).
    out.replace("小老鼠", "@")
        .replace("圈A", "@")
        .replace("圈ａ", "@")
        .replace("艾特", "@")
        .replace("＠", "@")
        .replace(" at ", "@")
        .replace(" AT ", "@")
        .replace(" At ", "@")
        .replace("點", ".")
        .replace("点", ".")
        .replace("．", ".")
        .replace("。", ".") // Whisper sometimes ends domain with 。
}

fn spoken_digit_char(ch: char) -> Option<char> {
    match ch {
        '0' | '０' | '零' | '〇' | '洞' => Some('0'),
        '1' | '１' | '一' | '幺' | '么' => Some('1'),
        '2' | '２' | '二' | '兩' | '两' => Some('2'),
        '3' | '３' | '三' => Some('3'),
        '4' | '４' | '四' => Some('4'),
        '5' | '５' | '五' => Some('5'),
        '6' | '６' | '六' => Some('6'),
        '7' | '７' | '七' => Some('7'),
        '8' | '８' | '八' => Some('8'),
        '9' | '９' | '九' | '勾' => Some('9'),
        _ => None,
    }
}

/// Spaced digits so Piper reads each digit clearly ("0 9 1 2 …").
pub fn phone_readback_display(digits: &str) -> String {
    let chars: Vec<char> = digits.chars().filter(|c| c.is_ascii_digit()).collect();
    if chars.is_empty() {
        return digits.to_string();
    }
    chars
        .into_iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Spell `@` / `.` for clearer Piper email read-back.
pub fn email_readback_display(email: &str) -> String {
    email.replace('@', " at ").replace('.', " 點 ")
}

fn normalize_phone_digits(s: &str) -> String {
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

fn phones_equivalent(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let ca = strip_tw_mobile_core(a);
    let cb = strip_tw_mobile_core(b);
    ca == cb
        || (ca.len().min(cb.len()) >= 8
            && ca.len().abs_diff(cb.len()) <= 3
            && (ca.ends_with(&cb) || cb.ends_with(&ca)))
}

/// A digit as said or typed, and whether it was typed (Arabic or full-width)
/// rather than a spoken numeral (零九幺…).
fn phone_digit(ch: char) -> Option<(char, bool)> {
    let d = spoken_digit_char(ch)?;
    Some((d, ch.is_ascii_digit() || ('０'..='９').contains(&ch)))
}

/// Separators inside a typed number: "0912-345-678", "(415) 555-0199",
/// "0912、345、678". Never a comma or a full stop: those end a sentence.
fn is_phone_separator(c: char) -> bool {
    matches!(c, ' ' | '-' | '－' | '.' | '、' | '/' | '／' | '(' | ')')
}

/// How much a digit run looks like a number to call back, or `None` when it
/// is no phone number at all: a Taiwan mobile (10 digits, 09…) first, then an
/// international number written with + (8–15 digits), then a Taiwan landline
/// (9–10 digits, 02…08…), or a domestic number matching `region`.
fn phone_candidate_score_in(number: &str, region: Option<&str>) -> Option<i32> {
    if let Some(digits) = number.strip_prefix('+') {
        return (8..=15).contains(&digits.len()).then_some(2);
    }
    let b = number.as_bytes();
    let iso = region.map(|r| r.trim().to_ascii_lowercase()).filter(|r| !r.is_empty());
    if iso.is_none() || iso.as_deref() == Some("tw") {
        if b.len() == 10 && number.starts_with("09") {
            return Some(3);
        }
        if (9..=10).contains(&b.len()) && b[0] == b'0' && (b'2'..=b'8').contains(&b[1]) {
            return Some(1);
        }
        return None;
    }
    if let Some(iso) = iso.as_deref() {
        if let Some(code) = crate::phone_region::calling_code(iso) {
            if (8..=15).contains(&b.len())
                && crate::phone_region::dialled_at_home(code, number)
                && crate::phone_region::normalize(number, Some(iso)).is_some()
            {
                return Some(2);
            }
        }
    }
    None
}

/// The phone number in `text`: an unbroken run of digits (typed, spoken or
/// mixed), where typed digits may be grouped by [`is_phone_separator`]. Runs
/// that are no phone number ([`phone_candidate_score_in`]) are skipped.
fn extract_phone_hint_in(text: &str, region: Option<&str>) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let typed = |i: usize| chars.get(i).and_then(|&c| phone_digit(c)).is_some_and(|(_, t)| t);
    let mut best: Option<(String, i32)> = None;
    let mut i = 0;
    while i < chars.len() {
        let plus = matches!(chars[i], '+' | '＋') && typed(i + 1);
        if !plus && phone_digit(chars[i]).is_none() {
            i += 1;
            continue;
        }
        let mut number = String::new();
        if plus {
            number.push('+');
            i += 1;
        }
        while i < chars.len() {
            if let Some((d, _)) = phone_digit(chars[i]) {
                number.push(d);
                i += 1;
                continue;
            }
            // Up to two separators, between typed digits only.
            let mut j = i;
            while j < chars.len() && j - i < 2 && is_phone_separator(chars[j]) {
                j += 1;
            }
            if j > i && i > 0 && typed(i - 1) && typed(j) {
                i = j;
                continue;
            }
            break;
        }
        if let Some(score) = phone_candidate_score_in(&number, region) {
            if best.as_ref().map_or(true, |(_, s)| score > *s) {
                best = Some((number, score));
            }
        }
    }
    best.map(|(n, _)| n)
}

/// Scan for `local@domain.tld` even when glued to CJK punctuation / words.
fn extract_email_hint(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut best: Option<String> = None;
    for (i, &c) in chars.iter().enumerate() {
        if c != '@' {
            continue;
        }
        // Walk left for local-part.
        let mut left = i;
        while left > 0 {
            let p = chars[left - 1];
            if is_email_local_char(p) {
                left -= 1;
            } else {
                break;
            }
        }
        // Walk right for domain.
        let mut right = i + 1;
        while right < chars.len() {
            let p = chars[right];
            if is_email_domain_char(p) {
                right += 1;
            } else {
                break;
            }
        }
        if left >= i || right <= i + 1 {
            continue;
        }
        let local: String = chars[left..i].iter().collect();
        let mut domain: String = chars[i + 1..right].iter().collect();
        while domain.ends_with('.') {
            domain.pop();
        }
        if !email_parts_ok(&local, &domain) {
            continue;
        }
        let email = format!("{local}@{domain}").to_ascii_lowercase();
        // Prefer longer / more complete addresses when several appear.
        if best.as_ref().map(|b| email.len() > b.len()).unwrap_or(true) {
            best = Some(email);
        }
    }
    best
}

fn is_email_local_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

fn is_email_domain_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
}

fn email_parts_ok(local: &str, domain: &str) -> bool {
    if local.is_empty() || domain.len() < 3 {
        return false;
    }
    if local.starts_with('.') || local.ends_with('.') {
        return false;
    }
    // Need at least one dot in domain and a plausible TLD (≥2 letters).
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !host.is_empty()
        && tld.len() >= 2
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.starts_with('-')
        && !host.ends_with('-')
}

fn parse_intent_json(raw_text: &str, caller_id: &str) -> Result<ExtractedIntent> {
    let json_str = strip_json_fences(raw_text);
    let v: Value = serde_json::from_str(&json_str).context("intent JSON parse")?;
    let intent = v
        .get("intent")
        .and_then(|x| x.as_str())
        .unwrap_or("Message")
        .trim()
        .to_string();
    let summary = v
        .get("summary")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let datetime_hint = opt_string(v.get("datetime_hint"));
    let reply_method = normalize_reply_method(v.get("reply_method"));
    let mut reply_contact = opt_string(v.get("reply_contact"));
    if reply_method.as_deref() == Some("phone") {
        if let Some(ref rc) = reply_contact {
            let digits = rc.chars().filter(|c| c.is_ascii_digit()).count();
            if rc.contains('@') || digits < 7 {
                reply_contact = None;
            }
        }
    } else if reply_method.as_deref() == Some("email") {
        if let Some(ref rc) = reply_contact {
            if !rc.contains('@') {
                reply_contact = None;
            }
        }
    }
    if reply_contact.is_none()
        && reply_method.as_deref() == Some("phone")
        && !caller_id.trim().is_empty()
    {
        reply_contact = Some(caller_id.trim().to_string());
    }
    let confidence = v
        .get("confidence")
        .and_then(|x| x.as_f64())
        .map(|c| c.clamp(0.0, 1.0) as f32)
        .unwrap_or(0.5);
    Ok(ExtractedIntent {
        intent,
        summary,
        datetime_hint,
        reply_method,
        reply_contact,
        confidence,
        raw_json: v,
    })
}

fn strip_json_fences(text: &str) -> String {
    let t = text.trim();
    if t.starts_with("```") {
        let inner = t
            .trim_start_matches("```")
            .trim_start_matches("json")
            .trim_start_matches("JSON");
        if let Some(end) = inner.rfind("```") {
            return inner[..end].trim().to_string();
        }
        return inner.trim().to_string();
    }
    t.to_string()
}

fn opt_string(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Null) | None => None,
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() || t.eq_ignore_ascii_case("null") {
                None
            } else {
                Some(t.to_string())
            }
        }
        Some(other) => Some(other.to_string()),
    }
}

fn normalize_reply_method(v: Option<&Value>) -> Option<String> {
    let s = opt_string(v)?;
    let norm = s.to_ascii_lowercase();
    match norm.as_str() {
        "phone" | "tel" | "call" | "mobile" => Some("phone".to_string()),
        "email" | "mail" => Some("email".to_string()),
        "none" | "no" | "null" => None,
        _ => Some(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_summary_is_asked_for_in_the_agents_language() {
        assert!(intent_system_prompt(Lang::ZhTw).contains("一句話繁體中文摘要"));
        let cn = intent_system_prompt(Lang::ZhCn);
        assert!(cn.contains("简体中文摘要") && !cn.contains("繁體"), "{cn}");
        let de = intent_system_prompt(Lang::De);
        assert!(de.contains("written in German") && !de.contains('{'), "{de}");
        assert!(intent_user_body(Lang::Ja, "0912", "x").contains("Message:\nx"));
        assert!(intent_user_body(Lang::ZhTw, "0912", "x").contains("留言內容：\nx"));
    }

    #[test]
    fn strip_fences_and_parse() {
        let raw = r#"```json
{"intent":"Callback","summary":"請回電","datetime_hint":null,"reply_method":"phone","reply_contact":null,"confidence":0.9}
```"#;
        let intent = parse_intent_json(raw, "+886912345678").unwrap();
        assert_eq!(intent.intent, "Callback");
        assert_eq!(intent.reply_method.as_deref(), Some("phone"));
        assert!((intent.confidence - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn heuristic_callback_keyword() {
        let i = heuristic_intent("麻煩回電給我，謝謝", "+886900");
        assert_eq!(i.intent, "Callback");
        assert_eq!(i.reply_method.as_deref(), Some("phone"));
        assert!((i.confidence - 0.55).abs() < f32::EPSILON);
        assert_eq!(i.reply_contact.as_deref(), Some("+886900"));
    }

    #[test]
    fn heuristic_schedule_meeting() {
        let i = heuristic_intent("想預約明天下午三點開會", "");
        assert_eq!(i.intent, "Schedule");
        assert!(i.datetime_hint.as_ref().is_some_and(|h| h.contains("明天")));
        assert!((i.confidence - 0.55).abs() < f32::EPSILON);
    }

    #[test]
    fn heuristic_schedule_soft_datetime() {
        let i = heuristic_intent("明天下午兩點方便見面嗎", "");
        assert_eq!(i.intent, "Schedule");
        assert!(i.datetime_hint.is_some());
    }

    #[test]
    fn heuristic_question_keyword() {
        let i = heuristic_intent("請問你們營業時間到幾點？", "");
        assert_eq!(i.intent, "Question");
        assert!((i.confidence - 0.50).abs() < f32::EPSILON);
    }

    #[test]
    fn heuristic_message_default() {
        let i = heuristic_intent("請轉告他我已經到辦公室了", "");
        assert_eq!(i.intent, "Message");
        assert!((i.confidence - 0.35).abs() < f32::EPSILON);
        assert!(i.datetime_hint.is_none());
    }

    #[test]
    fn heuristic_callback_beats_soft_schedule() {
        // Explicit callback + datetime → Callback (not soft Schedule).
        let i = heuristic_intent("請回電，明天方便再聊", "+886900");
        assert_eq!(i.intent, "Callback");
        assert!(i.datetime_hint.as_ref().is_some_and(|h| h.contains("明天")));
    }

    #[test]
    fn heuristic_email_at() {
        let i = heuristic_intent("請寄到 foo@bar.com", "");
        assert_eq!(i.intent, "Message");
        assert_eq!(i.reply_method.as_deref(), Some("email"));
    }

    #[test]
    fn spoken_contact_email() {
        let c = extract_spoken_contact("請寄信到 alice@test.com 謝謝", "+886900111222").unwrap();
        assert_eq!(c.method, "email");
        assert_eq!(c.contact, "alice@test.com");
        assert!(c.display.contains("at"));
        assert!(c.display.contains("點"));
    }

    #[test]
    fn spoken_contact_alt_phone() {
        let c = extract_spoken_contact("請回電 0912-345-678", "+886900111222").unwrap();
        assert_eq!(c.method, "phone");
        assert_eq!(c.contact, "0912345678");
        assert_eq!(c.display, "0 9 1 2 3 4 5 6 7 8");
    }

    #[test]
    fn spoken_contact_mandarin_digits() {
        let c = extract_spoken_contact("請回電 零九一三三三四五六七", "+886900111222").unwrap();
        assert_eq!(c.method, "phone");
        assert_eq!(c.contact, "0913334567");
        assert_eq!(c.display, "0 9 1 3 3 3 4 5 6 7");
    }

    #[test]
    fn spoken_contact_fullwidth_and_yao() {
        let c = extract_spoken_contact("號碼是０９幺２３４５６７８", "+886900111222").unwrap();
        assert_eq!(c.contact, "0912345678");
    }

    #[test]
    fn spoken_contact_skips_caller_id_match() {
        assert!(extract_spoken_contact("請回電給我", "+886912345678").is_none());
        assert!(extract_spoken_contact("我的號碼 0912345678", "886912345678").is_none());
        assert!(extract_spoken_contact("零九一二三四五六七八", "886912345678").is_none());
    }

    #[test]
    fn spoken_contact_email_spoken_words_glued() {
        let c = extract_spoken_contact("請寄到alice小老鼠gmail點com謝謝", "+886900111222").unwrap();
        assert_eq!(c.method, "email");
        assert_eq!(c.contact, "alice@gmail.com");
    }

    #[test]
    fn spoken_contact_email_at_word() {
        let c = extract_spoken_contact("email is bob at example.org thanks", "+886900").unwrap();
        assert_eq!(c.contact, "bob@example.org");
    }

    #[test]
    fn spoken_contact_email_plus_local() {
        let c = extract_spoken_contact("寄到 alice+vm@test.co.uk", "").unwrap();
        assert_eq!(c.contact, "alice+vm@test.co.uk");
    }

    #[test]
    fn spoken_contact_prefers_tw_mobile_over_long_noise() {
        let c = extract_spoken_contact("訂單202401011234567 請回電 0912-345-678", "+886900111222")
            .unwrap();
        assert_eq!(c.method, "phone");
        assert_eq!(c.contact, "0912345678");
    }

    #[test]
    fn spoken_contact_phone_chinese_separators() {
        let c = extract_spoken_contact("號碼 0912、345、678", "+886900111222").unwrap();
        assert_eq!(c.contact, "0912345678");
    }

    #[test]
    fn spoken_contact_rejects_too_long_digit_run() {
        assert!(
            extract_spoken_contact("序號 1234567890123456 沒有電話", "+886900111222").is_none()
        );
    }

    #[test]
    fn spoken_digits_stop_at_punctuation() {
        let c = extract_spoken_contact("零九一二三四五六七八，一下", "+886900111222").unwrap();
        assert_eq!(c.contact, "0912345678");
        let c = extract_spoken_contact("我是八，零九一二三四五六七八。九點後打", "").unwrap();
        assert_eq!(c.contact, "0912345678");
        // Commas and full stops split typed digits too.
        assert!(extract_spoken_contact("0912，345，678", "").is_none());
        assert!(extract_spoken_contact("0912。345678", "").is_none());
        // Spoken numerals do not take separators.
        assert!(extract_spoken_contact("零九一二 三四五 六七八", "").is_none());
        // Glued on, the run is no number any more.
        assert!(extract_spoken_contact("零九一二三四五六七八一", "").is_none());
    }

    #[test]
    fn spoken_contact_takes_only_phone_shaped_numbers() {
        let c = extract_spoken_contact("公司電話 02 2345 6789", "").unwrap();
        assert_eq!(c.contact, "0223456789");
        let c = extract_spoken_contact("打 03-1234567 找我", "").unwrap();
        assert_eq!(c.contact, "031234567");
        let c = extract_spoken_contact("call me on +1 202 555 0143", "").unwrap();
        assert_eq!(c.contact, "+12025550143");
        assert_eq!(c.display, "1 2 0 2 5 5 5 0 1 4 3");
        let c = extract_spoken_contact("東京オフィス +81312345678", "").unwrap();
        assert_eq!(c.contact, "+81312345678");
        // A mobile wins over a landline or an international number in the same words.
        let c = extract_spoken_contact("+81312345678 或 0912345678", "").unwrap();
        assert_eq!(c.contact, "0912345678");
        for text in ["12345678", "091234567", "09123456789", "886912345678", "0912345", "+1234567"] {
            assert!(extract_spoken_contact(text, "").is_none(), "{text}");
        }
        // The caller's own number in international form is still the caller's.
        assert!(extract_spoken_contact("+886 912 345 678", "0912345678").is_none());
    }

    #[test]
    fn spam_filter_disabled_no_hangup_match() {
        // Keyword counters still work; hangup gate (`is_spam_text*`) is always off.
        assert!(!is_spam_text("您好，這裡是銀行貸款優惠專線"));
        assert_eq!(spam_keyword_hits("貸款與房貸廣告"), 3);
        assert!(!is_spam_text("麻煩回電，謝謝"));
        assert!(!is_spam_text("提醒您信用卡辦卡利率優惠"));
        let custom = vec!["測試關鍵字".to_string()];
        assert!(!is_spam_text_with("這是測試關鍵字來電", &custom));
        assert!(SPAM_KEYWORDS_ZH.len() >= 16);
    }

    #[test]
    fn heuristic_no_longer_auto_spam_hangup() {
        let i = heuristic_intent("恭喜中獎，請洽優惠專線", "+886900");
        // Spam hangup gate disabled; intent may still be non-Spam from heuristics.
        assert_ne!(i.intent, "Spam");
    }

    #[test]
    fn multilingual_heuristics_spoken_region_and_reply_contact_validation() {
        for sample in [
            "折り返しお電話ください",
            "다시 전화 주세요",
            "Por favor devuelva la llamada",
            "Merci de me rappeler",
            "Bitte rufen Sie zurück",
            "Me liga de volta por favor",
            "Mi richiami appena può",
            "Пожалуйста, перезвоните",
            "Làm ơn gọi lại cho tôi",
            "Tolong telepon balik",
            "กรุณาโทรกลับด้วย",
            "कृपया वापस कॉल करें",
            "يرجى معاودة الاتصال بي",
        ] {
            let h = heuristic_intent(sample, "+886900");
            assert_eq!(h.intent, "Callback", "{sample}");
            assert_eq!(h.reply_method.as_deref(), Some("phone"), "{sample}");
        }
        assert_eq!(heuristic_intent("متى الموعد؟", "").intent, "Schedule");
        assert_eq!(heuristic_intent("هل يمكن الحضور اليوم؟", "").intent, "Question");

        // Domestic numbers in US/JP/KR are recognized when region is passed.
        assert_eq!(
            extract_spoken_contact_in("Call me at (415) 555-0199", "", Some("us")).map(|c| c.contact),
            Some("4155550199".into())
        );
        assert_eq!(
            extract_spoken_contact_in("番号は 090-1234-5678 です", "", Some("jp")).map(|c| c.contact),
            Some("09012345678".into())
        );
        // Short tail suffix no longer passes as equivalent.
        assert!(!phones_equivalent("678", "+886912345678"));
        // Email placed into reply_contact under phone method falls back to caller_id.
        let parsed = parse_intent_json(
            r#"{"intent":"Callback","summary":"Call back","reply_method":"phone","reply_contact":"x@example.com"}"#,
            "+886912345678",
        )
        .unwrap();
        assert_eq!(parsed.reply_contact.as_deref(), Some("+886912345678"));
    }
}

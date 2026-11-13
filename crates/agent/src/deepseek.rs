//! The post-call summary: a few short lines for the notification, written by a
//! chat model over an OpenAI-compatible API ([`summary_endpoint`]).
//!
//! Which service: the settings' own endpoint (`summary_api_base`, e.g. OpenAI,
//! OpenRouter or a local Ollama), else DeepSeek when its key is set, else
//! Gemini with the Live API key — so one Gemini key is enough to get going.
//!
//! The module keeps its first name: DeepSeek was the only service it knew, and
//! the older voicemail / TRTC paths still call it directly.

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::Settings;
use crate::locale::{self, Lang};

/// Stable default for summarize (TRTC config may use a realtime-only model id).
pub const DEEPSEEK_MODEL: &str = "deepseek-chat";
pub const DEEPSEEK_API_URL: &str = "https://api.deepseek.com/v1/chat/completions";

/// System prompt for short notification-ready Chinese summaries.
///
/// The input is a labelled dialogue (`對方：` / `接聽：` from Gemini's own
/// transcription) plus Whisper as a backup, so the
/// model is told exactly which source to trust and what each line means.
/// Output is four plain lines the owner can read in two seconds.
/// `{owner}` = the owner's name ([`summary_system_prompt`]).
pub const TG_SUMMARY_SYSTEM_PROMPT: &str = "\
你是{owner}的電話秘書。{owner}的手機由 AI 語音接電話；通話結束後，你把這通電話整理成幾行短訊息給{owner}看。\
一律繁體中文、台灣用語，不要 markdown、不要表情符號、不要客套。\n\
\n\
你會拿到：\n\
1. 【對話】逐行對話。「對方：」= 打電話來的人講的話；「接聽：」= AI（代表{owner}）講的話。這是最可靠的來源，以它為主。\n\
2. 【錄音轉寫】Whisper 對整段錄音的轉寫，常常錯字、混進雜音或別的語言，只拿來補【對話】裡缺的事由；兩邊衝突時以【對話】為準。\n\
3. 【通話資訊】號碼、長度、怎麼結束。\n\
\n\
固定輸出四行：\n\
對方：是誰。用對方自己說的身分（例如「小明（朋友）」「黑貓宅急便」「中華電信客服」）；沒說就寫「沒說是誰」。語音辨識錯字要自己修正成合理的名字或單位。\n\
事情：對方打來要幹嘛，一兩句，寫具體（例如「明天下午送包裹，問有沒有人在家」「提醒繳水費」「約明天中午吃飯」）。推銷／詐騙／語音廣告就直接寫「推銷電話：賣什麼」「疑似詐騙」。對方什麼都沒講就寫「沒講事情就掛了」。\n\
AI 回覆：AI 怎麼回的，一句（例如「說放管理室就好」「說晚點再確認」「直接拒絕」）；AI 沒講到重點就寫「只有打招呼」。\n\
要處理：{owner}需不需要做什麼。要的話寫清楚動作，例如「回電給小明確認明天中午」「明天下午有人在家收包裹」「去繳水費」；不需要就寫「不用」。推銷和詐騙一律「不用」。\n\
\n\
規則：\n\
- 事由出現繳水費／房租／電費／包裹／開會這類字眼，一定要寫進「事情」。\n\
- 每行最多 25 個字左右，像傳訊息，不要寫成句子解釋。\n\
- 「國際電話請注意小心詐騙」是電信公司的提示音，不是對方講的；之後對方講外語或聽不懂的內容，就當「疑似詐騙」。\n\
- 【通話資訊】寫 AI 連不上、系統掛斷時，「AI 回覆」寫「AI 沒接上，沒講話」。\n\
- 【通話資訊】寫 AI 故意陪聊拖時間、而【對話】看起來確實是推銷或詐騙時：「事情」寫賣什麼或什麼詐騙手法，「AI 回覆」寫「陪他繞了約 N 分鐘」（用長度換算），「要處理」寫「不用」；其實是正常電話就照一般寫。\n\
- 【對話】裡「接聽：」是 AI 講的，不是對方的要求；不要把 AI 的話當成對方的事情。\n\
- 不確定的事不要編，寫「聽不清楚」。\n\
- 不要重複號碼、時間、手機型號，這些訊息外面已經有了。";

/// The same job for every language without its own prompt: English
/// instructions, the four labels in the language the owner reads.
const SUMMARY_SYSTEM_PROMPT_EN: &str = "\
You write the short after-call note for {owner}. {owner}'s phone is answered by an AI voice; when a call ends you turn it into a few short lines that {owner} can read at a glance.\n\
Write in {language}. No markdown, no emoji, no pleasantries.\n\
\n\
You are given:\n\
1. [Dialogue] the call line by line. \"Caller:\" is what the person who called said; \"AI:\" is what the AI said on {owner}'s behalf. This is the most reliable source.\n\
2. [Recording transcript] a speech-to-text pass over the recording; it is often wrong or noisy. Use it only to fill in what [Dialogue] lacks; where the two disagree, [Dialogue] wins.\n\
3. [Call facts] the number, the length and how the call ended.\n\
\n\
Output exactly four lines with these labels:\n\
{s_caller}: who it was, in the caller's own words (a name, a company, a courier); if they did not say, write that they did not say. Turn obvious speech-recognition errors into a sensible name.\n\
{s_matter}: what they wanted, in one or two short concrete phrases. A sales call or recorded advertisement: start this line with \"{s_sales}:\" and what was being sold. A call that looks like a scam: start it with \"{s_scam}\". Use these two only for calls that are themselves selling or scamming, not for a bank, the police or anyone else calling about fraud. If they said nothing: that they hung up without saying anything.\n\
{s_reply}: how the AI answered, in one phrase; if it only said hello, say so.\n\
{s_todo}: whether {owner} has to do anything, as a clear action (call someone back about something, be home for a delivery tomorrow afternoon); otherwise that there is nothing to do. Sales and scam calls: nothing to do.\n\
\n\
Rules:\n\
- Each line is short, like a text message: about fifteen words at most.\n\
- \"AI:\" lines are the AI talking; they are not the caller's request.\n\
- If [Call facts] says the AI could not connect and the system hung up, the {s_reply} line is: {s_ai_failed}.\n\
- If [Call facts] says the AI deliberately kept the caller talking and the dialogue does look like a sales or scam call: {s_matter} is what was being sold or which scam it was, {s_reply} is that the AI kept them busy for about N minutes (work N out from the length), {s_todo} is nothing to do. If it was in fact a normal call, write it up normally.\n\
- Do not invent anything you are not sure of; write that it was unclear.\n\
- Do not repeat the number, the time or the phone model; the message already shows them.";

/// The summary model's instructions in the agent's language.
pub fn summary_system_prompt(lang: Lang, owner: &str) -> String {
    let owner = crate::prompts::owner_label(lang, owner);
    match lang {
        Lang::ZhTw => TG_SUMMARY_SYSTEM_PROMPT.replace("{owner}", &owner),
        Lang::ZhCn => zhconv::zhconv(
            &TG_SUMMARY_SYSTEM_PROMPT
                .replace("一律繁體中文、台灣用語", "一律簡體中文")
                .replace("{owner}", &owner),
            zhconv::Variant::ZhCN,
        ),
        other => {
            let p = locale::phrases(other);
            let (sales, scam) = sales_labels(other);
            SUMMARY_SYSTEM_PROMPT_EN
                .replace("{owner}", &owner)
                .replace("{language}", p.name_en)
                .replace("{s_caller}", p.s_caller)
                .replace("{s_matter}", p.s_matter)
                .replace("{s_sales}", sales)
                .replace("{s_scam}", scam)
                .replace("{s_reply}", p.s_reply)
                .replace("{s_todo}", p.s_todo)
                .replace("{s_ai_failed}", p.s_ai_failed)
        }
    }
}

/// Where the summary request goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// Full `…/chat/completions` URL.
    pub url: String,
    /// Empty = no `Authorization` header (a local server).
    pub key: String,
    /// Tried in this order.
    pub models: Vec<String>,
}

pub const GEMINI_OPENAI_URL: &str =
    "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions";
pub const GEMINI_SUMMARY_MODEL: &str = "gemini-flash-latest";

fn chat_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else {
        format!("{b}/chat/completions")
    }
}

/// The service that writes the summary, or `None` when no key is set at all.
pub fn summary_endpoint(settings: &Settings) -> Option<Endpoint> {
    let model = settings.summary_model.trim();
    let own_models = |fallback: &[&str]| -> Vec<String> {
        let mut m: Vec<String> = Vec::new();
        if !model.is_empty() {
            m.push(model.to_string());
        }
        for f in fallback {
            if !m.iter().any(|x| x == f) {
                m.push(f.to_string());
            }
        }
        m
    };
    let base = settings.summary_api_base.trim();
    if !base.is_empty() {
        return Some(Endpoint {
            url: chat_url(base),
            key: settings.deepseek_api_key.trim().to_string(),
            models: own_models(&[]),
        })
        .filter(|e| !e.models.is_empty());
    }
    let deepseek = settings.deepseek_api_key.trim();
    let from_llm_config = api_key_from_llm_config(settings.trtc_llm_config.as_ref());
    if !deepseek.is_empty() || from_llm_config.is_some() {
        let key = if deepseek.is_empty() {
            from_llm_config.unwrap_or_default()
        } else {
            deepseek.to_string()
        };
        return Some(Endpoint {
            url: DEEPSEEK_API_URL.to_string(),
            key,
            models: own_models(&[DEEPSEEK_MODEL, "deepseek-reasoner"]),
        });
    }
    let gemini = settings.gemini_api_key.trim();
    if !gemini.is_empty() {
        return Some(Endpoint {
            url: GEMINI_OPENAI_URL.to_string(),
            key: gemini.to_string(),
            models: own_models(&[GEMINI_SUMMARY_MODEL]),
        });
    }
    let openrouter = settings.openrouter_api_key.trim();
    if !openrouter.is_empty() {
        return Some(Endpoint {
            url: chat_url(&settings.openrouter_base_url),
            key: openrouter.to_string(),
            models: own_models(&[settings.openrouter_model.trim()]),
        });
    }
    None
}

/// One chat completion against any OpenAI-compatible endpoint.
pub async fn chat_at(
    http: &Client,
    endpoint: &Endpoint,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String> {
    let payload = chat_payload(endpoint, model, system, user, max_tokens);
    let mut req = http.post(&endpoint.url).json(&payload);
    if !endpoint.key.is_empty() {
        req = req.bearer_auth(&endpoint.key);
    }
    let resp = req.send().await.context("summary chat request")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("summary model={model} status {status}: {body}");
    }
    let data: Value = resp.json().await.context("summary chat json")?;
    Ok(data
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string())
}

/// Gemini's OpenAI-compatible endpoint (only a Gemini key is set).
fn is_gemini(endpoint: &Endpoint) -> bool {
    endpoint.url == GEMINI_OPENAI_URL
}

/// The request body. Gemini thinks before it answers and the thinking counts
/// against `max_tokens`: with 400 a summary came back cut off mid-line
/// ("Anliegen: Einladung zum Abendessen heute um"). It is asked to think
/// little (`reasoning_effort` low, which every Gemini model takes).
fn chat_payload(endpoint: &Endpoint, model: &str, system: &str, user: &str, max_tokens: u32) -> Value {
    let mut payload = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "stream": false,
        "temperature": 0.2,
        "max_tokens": max_tokens,
    });
    if is_gemini(endpoint) {
        payload["reasoning_effort"] = json!("low");
    }
    payload
}

/// Room for the four summary lines: 400 tokens, and on Gemini (whose
/// thinking takes from the same budget) 1500.
fn summary_max_tokens(endpoint: &Endpoint) -> u32 {
    if is_gemini(endpoint) {
        1_500
    } else {
        400
    }
}

/// The call as short lines for the notification, in the agent's language.
/// `transcript` = [`build_summary_input`]. Falls back to the caller's first
/// words when no service is set or none answers.
pub async fn summarize(http: &Client, settings: &Settings, caller_id: &str, transcript: &str) -> String {
    let active = locale::active();
    let lang = active.lang;
    let trimmed = transcript.trim().to_string();
    let caller = caller_id.trim();
    if trimmed.is_empty() {
        return empty_transcript_summary(lang);
    }
    let Some(endpoint) = summary_endpoint(settings) else {
        return fallback_summary_in(lang, caller, &trimmed);
    };
    let system = summary_system_prompt(lang, &active.owner);
    // A busy service (503 on every model) gets one more round a few seconds later.
    for round in 0..2 {
        if round > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            info!("call summary retry caller={caller}");
        }
        for model in &endpoint.models {
            match chat_at(http, &endpoint, model, &system, &trimmed, summary_max_tokens(&endpoint)).await {
                Ok(text) if !text.trim().is_empty() => {
                    let text = if lang.is_zh() {
                        crate::zh_tw::to_tw(&overlay_summary_matter(text.trim(), &trimmed))
                    } else {
                        text.trim().to_string()
                    };
                    let text = locale::fix_names(&text);
                    info!(
                        "call summary ok caller={caller} model={model} chars={}",
                        text.chars().count()
                    );
                    return text;
                }
                Ok(_) => warn!("call summary empty model={model}"),
                Err(e) => warn!("call summary failed model={model}: {e:#}"),
            }
        }
    }
    warn!("call summary: no model answered; using the caller's first words");
    fallback_summary_in(lang, caller, &trimmed)
}

fn empty_transcript_summary(lang: Lang) -> String {
    const TW: &str = "對方：沒說是誰\n事情：（沒有逐字稿，請聽錄音）\nAI 回覆：不明\n要處理：請聽錄音確認";
    match lang {
        Lang::ZhTw => TW.to_string(),
        Lang::ZhCn => zhconv::zhconv(TW, zhconv::Variant::ZhCN),
        _ => {
            let p = locale::phrases(lang);
            format!("{}{}{}", p.s_matter, p.sep, p.s_no_summary)
        }
    }
}

/// [`fallback_summary`] for any language: the caller's first real line and a
/// pointer to the web console.
fn fallback_summary_in(lang: Lang, caller: &str, transcript: &str) -> String {
    match lang {
        Lang::ZhTw => return fallback_summary(caller, transcript),
        // The same Chinese lines (the input is labelled 對方／接聽 too), in Simplified.
        Lang::ZhCn => return zhconv::zhconv(&fallback_summary(caller, transcript), zhconv::Variant::ZhCN),
        _ => {}
    }
    let p = locale::phrases(lang);
    let sep = p.sep;
    let first = transcript
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("Caller:").or_else(|| l.strip_prefix("對方：")))
        .map(str::trim)
        .find(|t| t.chars().filter(|c| c.is_alphanumeric()).count() >= 3);
    match first {
        Some(t) => format!(
            "{}{sep}{}\n{}{sep}{}",
            p.s_matter,
            locale::quoted(lang, &clip_chars(t, 80)),
            p.s_todo,
            p.s_no_summary
        ),
        None => format!("{}{sep}{}", p.s_matter, p.s_no_summary),
    }
}

/// The summary is one of the stand-ins [`summarize`] writes when no model
/// answered (no Chinese gist line, or the "no summary" note of the agent's language).
pub fn is_fallback_summary(text: &str) -> bool {
    text.contains("（摘要服務暫時沒回應）")
        || text.contains("（摘要服务暂时没回应）")
        || text.contains(locale::phrases(locale::current()).s_no_summary)
}

/// How the summary prompt tells the model to mark a sales call and a likely
/// scam on its "what they wanted" line (the Chinese prompt has its own:
/// 「推銷電話：…」「疑似詐騙」).
fn sales_labels(lang: Lang) -> (&'static str, &'static str) {
    match lang {
        Lang::ZhTw => ("推銷電話", "疑似詐騙"),
        Lang::ZhCn => ("推销电话", "疑似诈骗"),
        Lang::En => ("Sales call", "Suspected scam"),
        Lang::Ja => ("営業電話", "詐欺の疑い"),
        Lang::Ko => ("영업 전화", "사기 의심"),
        Lang::Es => ("Llamada comercial", "Posible estafa"),
        Lang::Fr => ("Démarchage", "Arnaque présumée"),
        Lang::De => ("Werbeanruf", "Betrugsverdacht"),
        Lang::Pt => ("Ligação de vendas", "Suspeita de golpe"),
    }
}

/// Words of a matter line about fraud that is not itself a pitch or a scam: a
/// bank's fraud desk, the police, a report the owner made.
const NOT_A_PITCH: &[&str] = &[
    "防詐", "防诈", "反詐", "反诈", "報案", "报案", "165", "筆錄", "笔录", "警察", "派出所",
    "被詐騙", "被诈骗", "不是推銷", "不是推销", "不是詐騙", "不是诈骗",
    "fraud department", "fraud alert", "fraud team", "fraud prevention", "anti-fraud", "antifraud",
    "police", "not a sales", "not sales", "not a scam",
    "詐欺対策", "被害届", "不正利用",
    "사기 예방", "경찰", "피해 신고", "부정 사용",
    "departamento de fraude", "policía", "denuncia", "no es una estafa",
    "anti-fraude", "service fraude", "gendarmerie", "plainte", "pas une arnaque",
    "betrugsabteilung", "betrugsprävention", "polizei", "anzeige", "kein betrug",
    "departamento de fraudes", "polícia", "boletim de ocorrência", "não é golpe",
];

/// The summary's "what they wanted" line marks the call as a sales call or a
/// likely scam in the words the summary prompt prescribes — a call the keyword
/// check during the call missed. Only those words count: "fraud" or 詐騙 alone
/// also stand in a line about the bank's fraud desk or a police report; such
/// lines, and lines about the owner's own matter, are never a pitch.
pub fn summary_says_sales(lang: Lang, summary: &str) -> bool {
    let (sales, scam) = sales_labels(lang);
    let extra: &[&str] = match lang {
        Lang::ZhTw | Lang::ZhCn => &["語音廣告", "语音广告"],
        Lang::En => &["sales pitch", "likely scam", "probable scam", "sales call"],
        Lang::Ja => &["セールス電話", "詐欺電話"],
        Lang::Ko => &["판매 전화", "광고 전화"],
        Lang::Es => &["llamada de ventas", "presunta estafa", "intento de estafa"],
        Lang::Fr => &["appel commercial", "arnaque probable", "tentative d'arnaque"],
        Lang::De => &["verkaufsanruf", "betrugsversuch"],
        Lang::Pt => &["ligação comercial", "tentativa de golpe"],
    };
    let label = locale::phrases(lang).s_matter;
    summary
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix(label).or_else(|| l.strip_prefix("事情")))
        .map(|l| l.trim_start_matches(|c: char| c.is_whitespace() || c == ':' || c == '：').to_lowercase())
        .any(|line| {
            let marked = if lang.is_zh() {
                // 「推銷電話：…」 opens the line; 「疑似詐騙」 may stand anywhere
                // (either script: the summary is put into Traditional first).
                ["推銷電話", "推销电话"].iter().chain(extra).any(|m| line.starts_with(m))
                    || ["疑似詐騙", "疑似诈骗"].iter().any(|m| line.contains(m))
            } else {
                line.starts_with(&sales.to_lowercase())
                    || line.contains(&scam.to_lowercase())
                    || extra.iter().any(|m| line.contains(m))
            };
            marked
                && !NOT_A_PITCH.iter().any(|n| line.contains(n))
                && !crate::gemini_live::looks_like_own_matter(&line)
        })
}

/// Pull `APIKey` from opaque `TRTC_LLM_CONFIG` JSON (never log the value).
pub fn api_key_from_llm_config(llm_config: &str) -> Option<String> {
    let raw = llm_config.trim();
    if raw.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(raw).ok()?;
    let key = v
        .get("APIKey")
        .or_else(|| v.get("api_key"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    Some(key.to_string())
}

pub fn model_from_llm_config(llm_config: &str) -> Option<String> {
    let raw = llm_config.trim();
    if raw.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_str(raw).ok()?;
    v.get("Model")
        .or_else(|| v.get("model"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Prefer dedicated DeepSeek key; then TRTC_LLM_CONFIG.APIKey; then OpenRouter key.
pub fn resolve_summarize_key(settings: &Settings) -> Option<String> {
    let direct = settings.deepseek_api_key.trim();
    if !direct.is_empty() {
        return Some(direct.to_string());
    }
    if let Some(k) = api_key_from_llm_config(settings.trtc_llm_config.as_ref()) {
        return Some(k);
    }
    let legacy = settings.openrouter_api_key.trim();
    if legacy.is_empty() {
        None
    } else {
        Some(legacy.to_string())
    }
}

/// True when key should hit DeepSeek API (LLMConfig present, or model name looks like deepseek).
pub fn should_use_deepseek_endpoint(settings: &Settings) -> bool {
    if api_key_from_llm_config(settings.trtc_llm_config.as_ref()).is_some() {
        return true;
    }
    let model = settings.openrouter_model.to_ascii_lowercase();
    model.contains("deepseek")
}

pub async fn chat_completions(
    http: &Client,
    api_key: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String> {
    chat_completions_model(http, api_key, DEEPSEEK_MODEL, system, user, max_tokens).await
}

pub async fn chat_completions_model(
    http: &Client,
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<String> {
    let model = {
        let m = model.trim();
        if m.is_empty() {
            DEEPSEEK_MODEL
        } else {
            m
        }
    };
    let payload = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "stream": false,
        "temperature": 0.2,
        "max_tokens": max_tokens,
    });
    let resp = http
        .post(DEEPSEEK_API_URL)
        .bearer_auth(api_key.trim())
        .json(&payload)
        .send()
        .await
        .context("deepseek chat")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("deepseek model={model} status {status}: {body}");
    }
    let data: Value = resp.json().await.context("deepseek json")?;
    let text = data
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        warn!("deepseek empty content model={model}");
    }
    Ok(text)
}

/// Organize caller needs into short Chinese text for the notification.
pub async fn summarize_for_telegram(
    http: &Client,
    api_key: &str,
    caller_id: &str,
    transcript: &str,
) -> String {
    summarize_for_telegram_with_llm_config(http, api_key, "", caller_id, transcript).await
}

/// Same as [`summarize_for_telegram`], optionally trying `Model` from TRTC_LLM_CONFIG first.
pub async fn summarize_for_telegram_with_llm_config(
    http: &Client,
    api_key: &str,
    llm_config: &str,
    caller_id: &str,
    transcript: &str,
) -> String {
    let caller = {
        let t = caller_id.trim();
        if t.is_empty() {
            "未知"
        } else {
            t
        }
    };
    let trimmed = transcript.trim().to_string();
    if trimmed.is_empty() {
        return "對方：沒說是誰\n事情：（沒有逐字稿，請聽錄音）\nAI 回覆：不明\n要處理：請聽錄音確認".to_string();
    }
    // Caller already carries the 【…】 sections (main.rs `build_summary_input`).
    let user = if trimmed.contains("【對話】") || trimmed.contains("【通話資訊】") {
        trimmed.clone()
    } else {
        format!("【通話資訊】\n號碼：{caller}\n\n【錄音轉寫】\n{trimmed}")
    };

    let mut models: Vec<String> = Vec::new();
    if let Some(m) = model_from_llm_config(llm_config) {
        // Skip realtime-only looking ids that often return empty on chat API.
        let ml = m.to_ascii_lowercase();
        if !ml.contains("realtime") && !ml.contains("native-audio") {
            models.push(m);
        }
    }
    for m in [DEEPSEEK_MODEL, "deepseek-chat", "deepseek-reasoner"] {
        if !models.iter().any(|x| x == m) {
            models.push(m.to_string());
        }
    }

    // Both models may answer 503 "Service is too busy" → one
    // more round a few seconds later before falling back to the raw dialogue.
    for round in 0..2 {
        if round > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            info!("deepseek tg summary retry caller={caller}");
        }
        for model in &models {
            let system = summary_system_prompt(Lang::ZhTw, &locale::active().owner);
            match chat_completions_model(http, api_key, model, &system, &user, 320).await {
                Ok(text) if !text.trim().is_empty() => {
                    let text = crate::zh_tw::to_tw(&overlay_summary_matter(text.trim(), &trimmed));
                    info!(
                        "deepseek tg summary ok caller={caller} model={model} chars={}",
                        text.chars().count()
                    );
                    return text;
                }
                Ok(_) => warn!("deepseek tg summary empty model={model}"),
                Err(e) => warn!("deepseek tg summary failed model={model}: {e:#}"),
            }
        }
    }
    warn!("deepseek tg summary all models failed; using transcript excerpt");
    fallback_summary(caller, &trimmed)
}

/// The notification stays short: without the summary model, one gist line —
/// the caller's first words — and the web has the rest.
fn fallback_summary(caller: &str, transcript: &str) -> String {
    // Never the 【對話】/【錄音轉寫】/【通話資訊】 scaffolding meant for the summary model.
    // First real caller sentence: not the carrier's international-call warning,
    // not auto-detect garble (경품기 / हेलो) — at least a few Han characters.
    let first_caller = transcript
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("對方："))
        .map(str::trim)
        .find(|t| {
            t.chars().filter(|c| ('\u{4E00}'..='\u{9FFF}').contains(c)).count() >= 3
                && !crate::gemini_live::looks_like_carrier_intl_prompt(t)
        });
    let has_dialogue = transcript
        .lines()
        .any(|l| l.trim_start().starts_with("對方：") || l.trim_start().starts_with("接聽："));
    let gist = match first_caller {
        Some(t) => format!("（摘要服務暫時沒回應）對方開頭說：「{}」", clip_chars(t, 40)),
        None if has_dialogue => "（摘要服務暫時沒回應）對方沒講到話".to_string(),
        None => clip_chars(transcript.trim(), 60),
    };
    let _ = caller;
    crate::zh_tw::to_tw(&overlay_summary_matter(
        &format!("事情：{gist}\n要處理：請看網頁逐字稿"),
        transcript,
    ))
}

fn clip_chars(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(n).collect();
        out.push('…');
        out
    }
}

/// Matter words the caller may have said (ASR 腳水費 still hits 水費).
const MATTER_MARKS: &[&str] = &[
    "水費", "水费", "繳水", "缴水", "交水", "交稅", "交税", "繳稅", "缴税", "房租", "租金",
    "電費", "电费", "瓦斯", "管理費", "管理费", "停車", "停车", "包裹", "開會", "开会",
    "會議", "会议",
];

fn transcript_matter_hits(transcript: &str) -> Vec<&'static str> {
    MATTER_MARKS
        .iter()
        .copied()
        .filter(|m| transcript.contains(m))
        .collect()
}

fn matter_demand_phrase(hits: &[&str]) -> String {
    if hits
        .iter()
        .any(|h| matches!(*h, "水費" | "水费" | "繳水" | "缴水" | "交水"))
    {
        "缴水费".to_string()
    } else if hits
        .iter()
        .any(|h| matches!(*h, "交稅" | "交税" | "繳稅" | "缴税"))
    {
        "缴税".to_string()
    } else if let Some(h) = hits.first() {
        (*h).to_string()
    } else {
        String::new()
    }
}

/// If the transcript named a matter but the LLM summary dropped it, patch 需求.
pub fn overlay_summary_matter(summary: &str, transcript: &str) -> String {
    let hits = transcript_matter_hits(transcript);
    if hits.is_empty() {
        return summary.to_string();
    }
    let phrase = matter_demand_phrase(&hits);
    if phrase.is_empty() {
        return summary.to_string();
    }
    if summary.contains(&phrase)
        || hits.iter().any(|h| summary.contains(h))
        || (phrase == "缴水费"
            && (summary.contains("水費")
                || summary.contains("水费")
                || summary.contains("繳水")
                || summary.contains("缴水")))
    {
        return summary.to_string();
    }
    let mut lines: Vec<String> = summary.lines().map(|s| s.to_string()).collect();
    let mut found = false;
    for line in &mut lines {
        let t = line.trim_start();
        if t.starts_with("事情：") || t.starts_with("事情:") {
            *line = format!("事情：{phrase}");
            found = true;
            break;
        }
        if t.starts_with("需求：") || t.starts_with("需求:") {
            *line = format!("需求：{phrase}");
            found = true;
            break;
        }
    }
    if !found {
        if let Some(i) = lines.iter().position(|l| {
            let t = l.trim_start();
            t.starts_with("来电：")
                || t.starts_with("来电:")
                || t.starts_with("來電：")
                || t.starts_with("對方：")
        }) {
            lines.insert(i + 1, format!("事情：{phrase}"));
        } else {
            lines.insert(0, format!("事情：{phrase}"));
        }
    }
    lines.join("\n")
}

/// Build the summary model's user message: labelled Gemini dialogue first (most
/// reliable), Whisper transcripts as backup, then call facts the model may
/// need (hidden number, length, who hung up). Sections the model can cite.
pub fn build_summary_input(
    caller_id: &str,
    duration_ms: u64,
    end_hint: &str,
    gemini_dialogue: &str,
    whisper_ai_segment: &str,
    whisper_remote: &str,
) -> String {
    let lang = locale::current();
    if !lang.is_zh() {
        return build_summary_input_en(
            caller_id,
            duration_ms,
            end_hint,
            gemini_dialogue,
            whisper_ai_segment,
            whisper_remote,
        );
    }
    let mut s = String::new();
    let dialogue = gemini_dialogue.trim();
    if !dialogue.is_empty() {
        s.push_str("【對話】（對方：= 來電者，接聽：= AI）\n");
        s.push_str(dialogue);
        s.push_str("\n\n");
    }
    let w1 = whisper_ai_segment.trim();
    let w2 = whisper_remote.trim();
    if !w1.is_empty() || !w2.is_empty() {
        s.push_str("【錄音轉寫】（Whisper，可能有錯字雜音，只用來補事由）\n");
        if !w1.is_empty() {
            s.push_str("AI 對話段：");
            s.push_str(w1);
            s.push('\n');
        }
        if !w2.is_empty() && w2 != w1 {
            s.push_str("整通對方側：");
            s.push_str(w2);
            s.push('\n');
        }
        s.push('\n');
    }
    s.push_str("【通話資訊】\n");
    let cid = caller_id.trim();
    if cid.is_empty() {
        s.push_str("號碼：未顯示號碼（對方隱藏來電顯示）\n");
    } else {
        s.push_str(&format!("號碼：{cid}\n"));
    }
    s.push_str(&format!("長度：約 {} 秒\n", (duration_ms + 500) / 1000));
    if !end_hint.trim().is_empty() {
        s.push_str(&format!("結束：{}\n", end_hint.trim()));
    }
    s
}

/// The transcript's stored speaker labels (`對方：` / `接聽：`) as `Caller:` / `AI:`.
pub fn relabel_dialogue_en(dialogue: &str) -> String {
    dialogue
        .lines()
        .map(|l| {
            let t = l.trim_start();
            if let Some(rest) = t.strip_prefix("對方：") {
                format!("Caller: {}", rest.trim_start())
            } else if let Some(rest) = t.strip_prefix("接聽：") {
                format!("AI: {}", rest.trim_start())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_summary_input_en(
    caller_id: &str,
    duration_ms: u64,
    end_hint: &str,
    dialogue: &str,
    whisper_ai_segment: &str,
    whisper_remote: &str,
) -> String {
    let mut s = String::new();
    let dialogue = dialogue.trim();
    if !dialogue.is_empty() {
        s.push_str("[Dialogue] (Caller: = the person who called, AI: = the AI answering)\n");
        s.push_str(&relabel_dialogue_en(dialogue));
        s.push_str("\n\n");
    }
    let w1 = whisper_ai_segment.trim();
    let w2 = whisper_remote.trim();
    if !w1.is_empty() || !w2.is_empty() {
        s.push_str("[Recording transcript] (speech-to-text, may be wrong or noisy)\n");
        if !w1.is_empty() {
            s.push_str(&format!("AI part of the call: {w1}\n"));
        }
        if !w2.is_empty() && w2 != w1 {
            s.push_str(&format!("Caller's side, whole call: {w2}\n"));
        }
        s.push('\n');
    }
    s.push_str("[Call facts]\n");
    let cid = caller_id.trim();
    if cid.is_empty() {
        s.push_str("Number: hidden (the caller withheld their number)\n");
    } else {
        s.push_str(&format!("Number: {cid}\n"));
    }
    s.push_str(&format!("Length: about {} seconds\n", (duration_ms + 500) / 1000));
    if !end_hint.trim().is_empty() {
        s.push_str(&format!("How it ended: {}\n", end_hint.trim()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        let mut s = Settings::from_env();
        s.summary_api_base = String::new();
        s.summary_model = String::new();
        s.deepseek_api_key = String::new();
        s.openrouter_api_key = String::new();
        s.trtc_llm_config = "".into();
        s.gemini_api_key = "".into();
        s
    }

    #[test]
    fn summary_goes_to_the_first_service_that_is_set() {
        let mut s = settings();
        assert_eq!(summary_endpoint(&s), None);
        s.gemini_api_key = "AIzaTEST".into();
        let e = summary_endpoint(&s).unwrap();
        assert_eq!(e.url, GEMINI_OPENAI_URL);
        assert_eq!(e.key, "AIzaTEST");
        assert_eq!(e.models, vec![GEMINI_SUMMARY_MODEL.to_string()]);
        // A DeepSeek key wins over the Gemini fallback.
        s.deepseek_api_key = "sk-ds".into();
        let e = summary_endpoint(&s).unwrap();
        assert_eq!(e.url, DEEPSEEK_API_URL);
        assert_eq!(e.models[0], DEEPSEEK_MODEL);
        // The settings' own endpoint wins over both and uses the same key field.
        s.summary_api_base = "http://127.0.0.1:11434/v1/".into();
        assert_eq!(summary_endpoint(&s), None, "an own endpoint needs a model");
        s.summary_model = "qwen2.5:7b".into();
        let e = summary_endpoint(&s).unwrap();
        assert_eq!(e.url, "http://127.0.0.1:11434/v1/chat/completions");
        assert_eq!(e.models, vec!["qwen2.5:7b".to_string()]);
        assert_eq!(e.key, "sk-ds");
        assert_eq!(chat_url("https://api.openai.com/v1/chat/completions"), "https://api.openai.com/v1/chat/completions");
    }

    #[test]
    fn summary_prompt_follows_the_language_and_owner() {
        let tw = summary_system_prompt(Lang::ZhTw, "王小明");
        assert!(tw.contains("你是王小明的電話秘書") && !tw.contains("{owner}"));
        let cn = summary_system_prompt(Lang::ZhCn, "王小明");
        assert!(cn.contains("一律简体中文") && cn.contains("电话秘书") && !cn.contains("繁體"));
        let es = summary_system_prompt(Lang::Es, "Ana");
        assert!(es.contains("Write in Spanish") && es.contains("Asunto:") && es.contains("Pendiente:"));
        assert!(!es.contains('{'), "{es}");
        let en = summary_system_prompt(Lang::En, "");
        assert!(en.contains("for the owner.") && en.contains("To do:"));
    }

    #[test]
    fn other_languages_get_english_labels_in_the_summary_input() {
        let s = build_summary_input_en(
            "",
            27_074,
            "the caller hung up",
            "接聽：Hello?\n對方：Hi, it's Jay. Lunch tomorrow?\n接聽：Sounds good.",
            "",
            "hi its jay lunch tomorrow",
        );
        assert!(s.starts_with("[Dialogue]"), "{s}");
        assert!(s.contains("AI: Hello?\nCaller: Hi, it's Jay. Lunch tomorrow?\nAI: Sounds good."), "{s}");
        assert!(s.contains("Number: hidden") && s.contains("Length: about 27 seconds"));
        assert!(s.contains("How it ended: the caller hung up"));
        assert!(!s.contains("對方") && !s.contains("接聽"));
        let f = fallback_summary_in(Lang::En, "", &s);
        assert!(f.starts_with("About: \"Hi, it's Jay. Lunch tomorrow?\""), "{f}");
    }

    #[test]
    fn parses_api_key_from_llm_config() {
        let cfg = r#"{"LLMType":"openai","Model":"deepseek-chat","APIKey":"sk-test","APIUrl":"https://api.deepseek.com/v1/chat/completions"}"#;
        assert_eq!(api_key_from_llm_config(cfg).as_deref(), Some("sk-test"));
        assert_eq!(model_from_llm_config(cfg).as_deref(), Some("deepseek-chat"));
        assert!(api_key_from_llm_config("").is_none());
        assert!(api_key_from_llm_config("{bad").is_none());
    }

    #[test]
    fn fallback_shows_dialogue_not_deepseek_scaffolding() {
        let input = build_summary_input(
            "+12025550143",
            11_965,
            "AI 接聽並對話後結束",
            "接聽：喂？\n對方：小心詐騙, 接通國際電話愛注意。\n接聽：喔不用了，掰掰。",
            "",
            "字國記電話 請注意小心炸片接通息",
        );
        let s = fallback_summary("+12025550143", &input);
        // The only 「對方」 line was the carrier's warning: the caller never spoke.
        assert!(s.starts_with("事情：（摘要服務暫時沒回應）對方沒講到話"), "{s}");
        assert!(!s.contains("【對話】") && !s.contains("【通話資訊】") && !s.contains("接聽："), "{s}");
        assert!(!s.contains("Whisper"), "{s}");
        assert_eq!(s.lines().count(), 2, "{s}");
        assert!(s.ends_with("要處理：請看網頁逐字稿"));
    }

    #[test]
    fn fallback_skips_carrier_prompt_and_garble() {
        // A sales call from abroad where the summary service answered 503 on every round.
        let input = build_summary_input(
            "+12025550143",
            71_863,
            "",
            "接聽：喂？\n對方：是國際電話, 請注意小心詐騙, 接通請掛斷點為。\n對方：경품기\n對方：हेलो\n對方：Hallo, ist jemand\n對方：你有聽到嗎?\n對方：我有一個保險想要推薦給你。",
            "",
            "",
        );
        let s = fallback_summary("+12025550143", &input);
        assert!(s.contains("對方開頭說：「你有聽到嗎?」"), "{s}");
        assert!(!s.contains("國際電話") && !s.contains("경품기"), "{s}");
    }

    #[test]
    fn fallback_is_traditional_and_keeps_matter() {
        // Caller number lives in the notify header now, not the body.
        let s = fallback_summary("+886900", "明天开会请回电");
        assert!(s.contains("明天開會"), "{s}");
        assert!(s.starts_with("事情："), "{s}");
    }

    #[test]
    fn fallbacks_use_each_languages_separator() {
        let input = "接聽：喂？\n對方：我是小明，明天中午吃飯？\n";
        let cn = fallback_summary_in(Lang::ZhCn, "", input);
        assert!(cn.starts_with("事情：（摘要服务暂时没回应）"), "{cn}");
        assert!(is_fallback_summary(&cn));
        let fr = fallback_summary_in(Lang::Fr, "", "Caller: Bonjour, c'est Paul.");
        assert!(fr.starts_with("Objet : « Bonjour, c'est Paul. »"), "{fr}");
        assert!(empty_transcript_summary(Lang::ZhCn).starts_with("对方：没说是谁"));
        assert_eq!(empty_transcript_summary(Lang::De), "Anliegen: (Keine Zusammenfassung – siehe Transkript in der Web-Konsole.)");
        assert!(!is_fallback_summary("事情：明天中午吃飯"));
    }

    #[test]
    fn summary_input_labels_sources_for_deepseek() {
        let s = build_summary_input(
            "",
            27_074,
            "AI 接聽並對話後結束",
            "接聽：喂？\n對方：我是小明，明天吃飯？\n接聽：好啊",
            "我是小明明天吃飯",
            "炸天 接通",
        );
        assert!(s.starts_with("【對話】"), "{s}");
        assert!(s.contains("接聽：= AI"));
        assert!(s.contains("【錄音轉寫】"));
        assert!(s.contains("AI 對話段：我是小明明天吃飯"));
        assert!(s.contains("整通對方側：炸天 接通"));
        assert!(s.contains("未顯示號碼"));
        assert!(s.contains("長度：約 27 秒"));
        assert!(s.contains("結束：AI 接聽並對話後結束"));
        // Whisper-only (no Gemini dialogue) still has the facts block.
        let w = build_summary_input("0912345678", 5_000, "", "", "", "水費");
        assert!(!w.contains("【對話】"));
        assert!(w.contains("號碼：0912345678"));
        assert!(w.contains("整通對方側：水費"));
    }

    #[test]
    fn tg_prompt_explains_dialogue_labels_and_four_lines() {
        for needle in ["【對話】", "接聽：", "對方：", "事情：", "AI 回覆：", "要處理：", "【錄音轉寫】"] {
            assert!(TG_SUMMARY_SYSTEM_PROMPT.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn tg_prompt_is_traditional_chinese() {
        assert!(TG_SUMMARY_SYSTEM_PROMPT.contains("繁體中文、台灣用語"));
        assert!(TG_SUMMARY_SYSTEM_PROMPT.contains("【錄音轉寫】"));
        assert!(TG_SUMMARY_SYSTEM_PROMPT.contains("{owner}"));
    }

    #[test]
    fn overlay_patches_empty_intent_when_water_in_transcript() {
        let summary = "来电：0900000000\n需求：对方未说明具体事由，仅回应助理引导语。\n回电：无\n状态：需要联系";
        let tx = "助理：喂我是AI助理\n来电：嗯要繳水費\n[对方侧录音转写]\n嗯要繳水費";
        let out = overlay_summary_matter(summary, tx);
        assert!(out.contains("缴水费"), "{out}");
        assert!(!out.contains("未说明具体事由"), "{out}");
        // New four-line shape: 事情 is the line that gets patched.
        let s4 = "對方：沒說是誰\n事情：聽不清楚\nAI 回覆：只有打招呼\n要處理：不用";
        let out4 = overlay_summary_matter(s4, "對方：記得繳水費喔");
        assert!(out4.contains("事情：缴水费"), "{out4}");
        assert!(out4.contains("AI 回覆：只有打招呼"), "{out4}");
    }

    #[test]
    fn overlay_keeps_summary_if_already_has_matter() {
        let summary = "来电：0900\n需求：缴水费\n回电：无\n状态：需要联系";
        let out = overlay_summary_matter(summary, "請轉達繳水費");
        assert_eq!(out, summary);
    }

    #[test]
    fn gemini_summaries_think_little_and_get_room_deepseek_is_unchanged() {
        let gemini = Endpoint { url: GEMINI_OPENAI_URL.into(), key: "k".into(), models: vec![GEMINI_SUMMARY_MODEL.into()] };
        let deepseek = Endpoint { url: DEEPSEEK_API_URL.into(), key: "k".into(), models: vec![DEEPSEEK_MODEL.into()] };
        let g = chat_payload(&gemini, GEMINI_SUMMARY_MODEL, "sys", "call", summary_max_tokens(&gemini));
        assert_eq!(g["reasoning_effort"], "low");
        assert_eq!(g["max_tokens"], 1500);
        let d = chat_payload(&deepseek, DEEPSEEK_MODEL, "sys", "call", summary_max_tokens(&deepseek));
        assert!(d.get("reasoning_effort").is_none());
        assert_eq!(d["max_tokens"], 400);
        assert_eq!(d["messages"][1]["content"], "call");
    }

    #[test]
    fn a_summary_that_calls_it_sales_or_a_scam_is_recognised() {
        assert!(summary_says_sales(Lang::En, "Caller: Mia from Brightdesk\nAbout: Sales call pitching CRM software, wants a 15-minute demo\nAI said: listened\nTo do: nothing"));
        assert!(summary_says_sales(Lang::ZhTw, "對方：保險業務\n事情：推銷電話：醫療險\nAI 回覆：陪他聊\n要處理：不用"));
        assert!(summary_says_sales(Lang::ZhTw, "事情：疑似詐騙，自稱檢察官要求轉帳"));
        assert!(summary_says_sales(Lang::En, "About: Suspected scam: a parcel held at customs, asked for a card number"));
        assert!(summary_says_sales(Lang::De, "Anrufer: unbekannt\nAnliegen: Betrugsverdacht (Gewinnspiel)"));
        assert!(summary_says_sales(Lang::Ja, "相手：不明\n用件：営業電話：光回線の案内"));
        // A friend inviting the owner to dinner; "not a sales call"; the word elsewhere.
        assert!(!summary_says_sales(Lang::En, "Caller: Jonas\nAbout: dinner tonight at 8\nAI said: will check\nTo do: confirm with Jonas"));
        assert!(!summary_says_sales(Lang::En, "About: the bank calling back, not a sales call"));
        assert!(!summary_says_sales(Lang::En, "Caller: a scam-prevention officer\nAbout: your report from Monday"));
        assert!(!summary_says_sales(Lang::ZhTw, "對方：小明\n事情：約明天中午吃飯"));
    }

    #[test]
    fn calls_about_fraud_are_not_called_a_pitch() {
        // Real calls about fraud: the bank's fraud desk, the police, the owner's own report.
        for (lang, s) in [
            (Lang::ZhTw, "對方：中國信託\n事情：銀行防詐騙中心確認一筆可疑刷卡，請明天上午回電"),
            (Lang::ZhTw, "對方：派出所\n事情：派出所通知你被詐騙的案子，約明天去做筆錄"),
            (Lang::ZhTw, "事情：165 反詐騙專線回覆你的報案"),
            (Lang::ZhTw, "事情：說他們在推銷的方案你上次有問過"),
            (Lang::En, "Caller: First Bank\nAbout: bank fraud department about a suspicious charge, call back at 3 pm"),
            (Lang::En, "About: fraud alert on your card, they want you to confirm a purchase"),
            (Lang::En, "About: scam warning from the police about fake parcel texts"),
            (Lang::En, "About: following up on your complaint about a sales call they made"),
            (Lang::De, "Anliegen: Polizei wegen Ihrer Anzeige zum Betrugsverdacht"),
            (Lang::Es, "Asunto: departamento de fraude del banco por un cargo sospechoso"),
        ] {
            assert!(!summary_says_sales(lang, s), "{lang:?}: {s}");
        }
        // Only the prescribed words, not "fraud" / "scam" / 詐騙 / 推銷 on their own.
        assert!(!summary_says_sales(Lang::En, "About: fraud on the account, wants a call back"));
        assert!(!summary_says_sales(Lang::En, "About: talked about a scam on the news"));
        assert!(!summary_says_sales(Lang::ZhTw, "事情：問你上次說的詐騙新聞"));
        // Every language's prompt names its two marks.
        for l in locale::ALL {
            let (sales, scam) = sales_labels(l);
            let p = summary_system_prompt(l, "Alex");
            assert!(l.is_zh() || (p.contains(sales) && p.contains(scam) && !p.contains("{s_sales}")), "{l:?}");
        }
    }
}

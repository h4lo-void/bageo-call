//! The post-call summary: a few short lines for the notification, written by a
//! chat model over an OpenAI-compatible API ([`summary_endpoint`]).
//!
//! Which service: the settings' own endpoint (`summary_api_base`, e.g. OpenAI,
//! OpenRouter or a local Ollama), else DeepSeek when its key is set, else
//! Gemini with the Live API key — so one Gemini key is enough to get going.
//! The setting `summary_provider` can pin Gemini (on its lowest-cost model) or
//! DeepSeek instead ([`Provider`]).
//!
//! The summary is written in the language the owner reads
//! ([`locale::notify_lang`]), whatever language the call was in.
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
一律繁體中文、台灣用語（通話是別的語言也一樣，翻成中文寫），不要 markdown、不要表情符號、不要客套。\n\
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
- 「國際電話請注意小心詐騙」是電信公司的提示音，不是對方講的，也不是詐騙的證據。對方講外語本身不算推銷或詐騙；只有對方講的內容本身像推銷或詐騙，才寫「推銷電話」「疑似詐騙」。\n\
- 銀行、信用卡中心打來確認某一筆刷卡或交易是不是{owner}本人（沒有要轉帳、去 ATM、給驗證碼或密碼、裝 App、解除分期）：不是推銷也不是詐騙，就算【通話資訊】寫 AI 陪他聊也一樣。「事情」寫哪家銀行、金額、商店、時間（例如「大愛銀行：昨晚11點網路電器行3280元，問是否本人刷」）；「要處理」提醒{owner}自己確認（例如「自己確認是否本人刷；不是就打卡片背面客服，別回撥來電」），不能寫「不用」。\n\
- 保險公司或銀行單純通知{owner}既有申請進度、既有保單理賠結果／入帳、卡片寄送或帳單提醒，以及親友、房東、廠商談日常匯款或帳務（沒有推銷新保單／貸款／方案，也沒有要操作 ATM、給驗證碼或密碼、轉帳到安全帳戶、加 LINE）：不是推銷也不是詐騙，照一般電話寫事由與待辦，不要寫「推銷電話」或「疑似詐騙」。\n\
- 【通話資訊】寫 AI 連不上、系統掛斷時，「AI 回覆」寫「AI 沒接上，沒講話」。\n\
- 【通話資訊】寫 AI 故意陪聊拖時間、而【對話】看起來確實是推銷或詐騙時：「事情」寫賣什麼或什麼詐騙手法，「AI 回覆」寫「陪他繞了約 N 分鐘」（用長度換算），「要處理」寫「不用」；其實是正常電話就照一般寫。\n\
- 【對話】裡「接聽：」是 AI 講的，不是對方的要求；不要把 AI 的話當成對方的事情。\n\
- 不確定的事不要編，寫「聽不清楚」。\n\
- 不要重複號碼、時間、手機型號，這些訊息外面已經有了。";

/// The same job for every language without its own prompt: English
/// instructions, the four labels in the language the owner reads.
const SUMMARY_SYSTEM_PROMPT_EN: &str = "\
You write the short after-call note for {owner}. {owner}'s phone is answered by an AI voice; when a call ends you turn it into a few short lines that {owner} can read at a glance.\n\
Write in {language}, whatever language the call was in (translate what was said). No markdown, no emoji, no pleasantries.\n\
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
- The carrier's warning about international calls is not the caller and no sign of a scam. A caller speaking another language is not a sales or scam call either; only what they actually say can make it one.\n\
- A bank or card centre calling only to check whether a card charge or transaction was {owner}'s (no request to transfer money, go to an ATM, give a code or password, install an app or cancel an instalment plan) is neither a sales call nor a scam, even if [Call facts] says the AI kept them talking: {s_matter} names the bank, the amount, the shop and the time; {s_todo} tells {owner} to check whether the charge was theirs and, if it was not, to call the number on the back of the card (not the number that called) — never that there is nothing to do.\n\
- An insurer or bank updating {owner} on an existing application, claim payout, card delivery or bill reminder, or a known person / vendor discussing an ordinary transfer or invoice (with no pitch for new insurance, loans or paid plans, and no request for ATM steps, codes, passwords, safe accounts or chat apps), is neither a sales call nor a scam: write it up as a normal call.\n\
- If [Call facts] says the AI could not connect and the system hung up, the {s_reply} line is: {s_ai_failed}.\n\
- If [Call facts] says the AI deliberately kept the caller talking and the dialogue does look like a sales or scam call: {s_matter} is what was being sold or which scam it was, {s_reply} is that the AI kept them busy for about N minutes (work N out from the length), {s_todo} is nothing to do. If it was in fact a normal call, write it up normally.\n\
- Do not invent anything you are not sure of; write that it was unclear.\n\
- Do not repeat the number, the time or the phone model; the message already shows them.";

/// The summary model's instructions in the language the owner reads.
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
/// Gemini's lowest-cost model: the first choice of [`Provider::Gemini`].
pub const GEMINI_SUMMARY_LITE_MODEL: &str = "gemini-flash-lite-latest";

/// Which service writes the summary (settings `summary_provider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// The settings' own endpoint, else DeepSeek, else Gemini, else OpenRouter.
    Auto,
    /// Gemini with the Live API key, on its lowest-cost model.
    Gemini,
    /// DeepSeek.
    DeepSeek,
}

impl Provider {
    /// The settings value: `auto` / `gemini` / `deepseek`; anything else is auto.
    pub fn parse(s: &str) -> Provider {
        match s.trim().to_ascii_lowercase().as_str() {
            "gemini" => Provider::Gemini,
            "deepseek" => Provider::DeepSeek,
            _ => Provider::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Provider::Auto => "auto",
            Provider::Gemini => "gemini",
            Provider::DeepSeek => "deepseek",
        }
    }
}

/// The running agent's choice. A process-wide slot like the language
/// ([`locale::set_active`]): the summary's callers only hold the env-shaped
/// [`Settings`].
static PROVIDER: std::sync::RwLock<Provider> = std::sync::RwLock::new(Provider::Auto);

/// Set at startup and on every settings save.
pub fn set_provider(p: Provider) {
    *PROVIDER.write().unwrap_or_else(|e| e.into_inner()) = p;
}

/// The running agent's [`Provider`]; unset (unit tests) = auto.
pub fn provider() -> Provider {
    *PROVIDER.read().unwrap_or_else(|e| e.into_inner())
}

fn chat_url(base: &str) -> String {
    let b = base.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else {
        format!("{b}/chat/completions")
    }
}

/// The service that writes the summary, or `None` when no key is set at all
/// ([`summary_endpoint_for`] with the running agent's [`provider`]).
pub fn summary_endpoint(settings: &Settings) -> Option<Endpoint> {
    summary_endpoint_for(settings, provider())
}

/// The service for a given [`Provider`]. `Gemini` and `DeepSeek` without
/// their key work like `Auto`. `Gemini` tries the settings' `summary_model`
/// first when one is set, then [`GEMINI_SUMMARY_LITE_MODEL`], then
/// [`GEMINI_SUMMARY_MODEL`].
pub fn summary_endpoint_for(settings: &Settings, provider: Provider) -> Option<Endpoint> {
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
    let deepseek_endpoint = || -> Option<Endpoint> {
        let deepseek = settings.deepseek_api_key.trim();
        let key = if deepseek.is_empty() {
            api_key_from_llm_config(settings.trtc_llm_config.as_ref())?
        } else {
            deepseek.to_string()
        };
        Some(Endpoint {
            url: DEEPSEEK_API_URL.to_string(),
            key,
            models: own_models(&[DEEPSEEK_MODEL, "deepseek-reasoner"]),
        })
    };
    let gemini = settings.gemini_api_key.trim();
    match provider {
        Provider::Gemini if !gemini.is_empty() => {
            // A model name left over from another service (e.g. deepseek-chat) would
            // fail on every summary before the lite model is tried: skip it.
            let mut models: Vec<String> = Vec::new();
            let own = settings.summary_model.trim();
            if own.starts_with("gemini") {
                models.push(own.to_string());
            }
            for m in [GEMINI_SUMMARY_LITE_MODEL, GEMINI_SUMMARY_MODEL] {
                if !models.iter().any(|x| x == m) {
                    models.push(m.to_string());
                }
            }
            return Some(Endpoint {
                url: GEMINI_OPENAI_URL.to_string(),
                key: gemini.to_string(),
                models,
            });
        }
        Provider::DeepSeek => {
            if let Some(e) = deepseek_endpoint() {
                return Some(e);
            }
        }
        _ => {}
    }
    let base = settings.summary_api_base.trim();
    if !base.is_empty() {
        return Some(Endpoint {
            url: chat_url(base),
            key: settings.deepseek_api_key.trim().to_string(),
            models: own_models(&[]),
        })
        .filter(|e| !e.models.is_empty());
    }
    if let Some(e) = deepseek_endpoint() {
        return Some(e);
    }
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

/// The call as short lines for the notification, in the language the owner
/// reads ([`locale::notify_lang`]). `transcript` = [`build_summary_input`].
/// Falls back to the caller's first words when no service is set or none answers.
pub async fn summarize(http: &Client, settings: &Settings, caller_id: &str, transcript: &str) -> String {
    let active = locale::active();
    let lang = active.notify_lang;
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
                    let text = locale::fix_names(&polish_summary(lang, &text, &trimmed));
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

/// A model's summary as the owner reads it: in Chinese a matter the model
/// dropped is put back ([`overlay_summary_matter`]) and zh-TW gets
/// Traditional characters ([`locale::for_owner_in`]); other languages as written.
fn polish_summary(lang: Lang, text: &str, transcript: &str) -> String {
    if lang.is_zh() {
        locale::for_owner_in(lang, &overlay_summary_matter(text.trim(), transcript))
    } else {
        text.trim().to_string()
    }
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
/// answered (no Chinese gist line, or the "no summary" note of the owner's language).
pub fn is_fallback_summary(text: &str) -> bool {
    text.contains("（摘要服務暫時沒回應）")
        || text.contains("（摘要服务暂时没回应）")
        || text.contains(locale::notify_phrases().s_no_summary)
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

/// Organize caller needs into short lines for the notification, in the
/// language the owner reads (DeepSeek only).
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
    let lang = locale::notify_lang();
    let caller = caller_id.trim();
    let trimmed = transcript.trim().to_string();
    if trimmed.is_empty() {
        return empty_transcript_summary(lang);
    }
    // Caller already carries the 【…】 / […] sections ([`build_summary_input`]).
    let sectioned = ["【對話】", "【通話資訊】", "[Dialogue]", "[Call facts]"]
        .iter()
        .any(|m| trimmed.contains(m));
    let user = if sectioned {
        trimmed.clone()
    } else if lang.is_zh() {
        let number = if caller.is_empty() { "未知" } else { caller };
        format!("【通話資訊】\n號碼：{number}\n\n【錄音轉寫】\n{trimmed}")
    } else {
        let number = if caller.is_empty() { "unknown" } else { caller };
        format!("[Call facts]\nNumber: {number}\n\n[Recording transcript]\n{trimmed}")
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
            let system = summary_system_prompt(lang, &locale::active().owner);
            match chat_completions_model(http, api_key, model, &system, &user, 320).await {
                Ok(text) if !text.trim().is_empty() => {
                    let text = polish_summary(lang, &text, &trimmed);
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
    fallback_summary_in(lang, caller, &trimmed)
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
    locale::for_owner(&overlay_summary_matter(
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

/// Labels of the caller's lines in what the summary model reads
/// ([`build_summary_input`]; 來電 in older voicemail transcripts), and of the
/// AI's. Both Whisper passes are the caller's uplink: the whole call, and
/// "AI 對話段" — the caller while the AI was on (`calls.rs` `ai_remote_pcm`).
const CALLER_LABELS: &[&str] = &[
    "對方：", "对方：", "來電：", "来电：", "Caller:", "整通對方側：", "Caller's side, whole call:",
    "AI 對話段：", "AI part of the call:",
];
const AI_LABELS: &[&str] = &["接聽：", "接听：", "助理：", "AI:"];

/// What the caller said. A transcript without speaker labels (a voicemail
/// recording) is all the caller's.
fn caller_side(transcript: &str) -> String {
    let labelled = transcript.lines().map(str::trim).any(|l| {
        CALLER_LABELS.iter().chain(AI_LABELS).any(|p| l.starts_with(p))
    });
    if !labelled {
        return transcript.to_string();
    }
    transcript
        .lines()
        .map(str::trim)
        .filter_map(|l| CALLER_LABELS.iter().find_map(|p| l.strip_prefix(p)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Matter words in the caller's lines only: the AI saying 我在開會 is no
/// matter of the caller's, and neither are the call facts.
fn transcript_matter_hits(transcript: &str) -> Vec<&'static str> {
    let said = caller_side(transcript);
    MATTER_MARKS
        .iter()
        .copied()
        .filter(|m| said.contains(m))
        .collect()
}

/// A matter line that says nothing: the model did not catch what it was.
fn matter_is_placeholder(text: &str) -> bool {
    text.is_empty()
        || [
            "聽不清楚", "听不清楚", "未說明", "未说明", "沒講事情", "没讲事情", "沒有逐字稿", "没有逐字稿",
            "事由不明",
        ]
        .iter()
        .any(|p| text.contains(p))
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

/// If the caller named a matter but the LLM summary dropped it, add it to the
/// 事情／需求 line. What the model wrote stays: it may have the call right and
/// the word be a side remark; only a line that says nothing is replaced.
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
        let Some((label, rest)) = ["事情", "需求"].iter().find_map(|label| {
            let rest = t.strip_prefix(label)?;
            let rest = rest.strip_prefix('：').or_else(|| rest.strip_prefix(':'))?;
            Some((*label, rest.trim()))
        }) else {
            continue;
        };
        *line = if matter_is_placeholder(rest) {
            format!("{label}：{phrase}")
        } else {
            format!("{label}：{rest}；也提到{phrase}")
        };
        found = true;
        break;
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
/// need (hidden number, length, who hung up). Sections the model can cite, in
/// the language of the summary prompt ([`locale::notify_lang`]).
pub fn build_summary_input(
    caller_id: &str,
    duration_ms: u64,
    end_hint: &str,
    gemini_dialogue: &str,
    whisper_ai_segment: &str,
    whisper_remote: &str,
) -> String {
    build_summary_input_in(
        locale::notify_lang(),
        caller_id,
        duration_ms,
        end_hint,
        gemini_dialogue,
        whisper_ai_segment,
        whisper_remote,
    )
}

/// [`build_summary_input`] for a summary in `lang`: the Chinese prompt reads
/// 【…】 sections, every other language's prompt reads […] ones.
fn build_summary_input_in(
    lang: Lang,
    caller_id: &str,
    duration_ms: u64,
    end_hint: &str,
    gemini_dialogue: &str,
    whisper_ai_segment: &str,
    whisper_remote: &str,
) -> String {
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

// ---------------------------------------------------------------------------
// Dialogue translation: after the call, the lines said in another language
// than the owner's get a translation into it (one request, the summary's
// service), shown under each line in the web record.

/// Lines and characters sent for translation, from the start of the call (a
/// stalled sales call can run for half an hour).
const TRANSLATE_MAX_LINES: usize = 120;
const TRANSLATE_MAX_CHARS: usize = 8_000;

/// Which writing a line is mostly in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineScript {
    /// Chinese characters, no kana.
    Han,
    /// Kana, with or without kanji.
    Japanese,
    Hangul,
    Latin,
    /// Cyrillic, Thai, Arabic and the rest: none of the owner's languages.
    Other,
}

/// How much of a line is in each writing: Chinese characters, kana and hangul
/// one each, words of the alphabets one each (Thai and the other scripts
/// written without spaces: three letters a word).
#[derive(Debug, Default, Clone, Copy)]
struct ScriptCount {
    han: usize,
    kana: usize,
    hangul: usize,
    latin_words: usize,
    other_words: usize,
}

fn is_han(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
        || ('\u{3400}'..='\u{4dbf}').contains(&c)
        || ('\u{f900}'..='\u{faff}').contains(&c)
}

fn is_kana(c: char) -> bool {
    ('\u{3040}'..='\u{30ff}').contains(&c)
        || ('\u{31f0}'..='\u{31ff}').contains(&c)
        || ('\u{ff66}'..='\u{ff9f}').contains(&c)
}

fn is_hangul(c: char) -> bool {
    ('\u{ac00}'..='\u{d7af}').contains(&c)
        || ('\u{1100}'..='\u{11ff}').contains(&c)
        || ('\u{3130}'..='\u{318f}').contains(&c)
}

fn is_latin_letter(c: char) -> bool {
    c.is_ascii_alphabetic()
        || (c.is_alphabetic() && (('\u{00c0}'..='\u{024f}').contains(&c) || ('\u{1e00}'..='\u{1eff}').contains(&c)))
}

/// Thai, Lao, Myanmar, Khmer: written without spaces between words.
fn is_unspaced_script(c: char) -> bool {
    ('\u{0e00}'..='\u{0eff}').contains(&c)
        || ('\u{1000}'..='\u{109f}').contains(&c)
        || ('\u{1780}'..='\u{17ff}').contains(&c)
}

fn count_scripts(text: &str) -> ScriptCount {
    let mut n = ScriptCount::default();
    let mut unspaced = 0usize;
    // The word being read: Some(true) Latin, Some(false) another alphabet.
    let mut word: Option<bool> = None;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if is_han(c) {
            n.han += 1;
        } else if is_kana(c) {
            n.kana += 1;
        } else if is_hangul(c) {
            n.hangul += 1;
        } else if is_unspaced_script(c) {
            unspaced += 1;
        } else if c.is_alphabetic() {
            let latin = is_latin_letter(c);
            if word != Some(latin) {
                if latin {
                    n.latin_words += 1;
                } else {
                    n.other_words += 1;
                }
                word = Some(latin);
            }
            continue;
        } else if matches!(c, '\'' | '’') && word.is_some() && chars.get(i + 1).is_some_and(|x| x.is_alphabetic()) {
            // it's, l'agent, d'accord: one word.
            continue;
        }
        word = None;
    }
    n.other_words += unspaced.div_ceil(3);
    n
}

/// Common short words of the Latin-script languages, to tell them apart.
fn latin_function_words(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::En => &[
            "the", "and", "is", "are", "was", "you", "your", "i", "i'm", "it", "it's", "to", "of", "for", "with",
            "this", "that", "what", "do", "does", "have", "has", "can", "will", "my", "me", "we", "be", "not",
            "just", "hello", "hi", "yes", "thanks", "thank", "please", "okay", "sure", "there", "here", "at",
            "from", "would", "could", "should", "about", "if", "or", "but", "how", "when", "where", "who", "why",
            "get", "got", "good", "bye", "tonight", "today", "tomorrow", "speak", "call", "don't",
        ],
        Lang::Es => &[
            "el", "la", "los", "las", "que", "de", "y", "es", "usted", "para", "por", "con", "no", "un", "una",
            "hola", "gracias", "sí", "qué", "está", "estoy", "muy", "bien", "pero", "como", "cómo", "mi", "su",
            "lo", "le", "se", "del", "al", "buenos", "días", "mañana", "hoy", "señor", "señora", "vale", "claro",
            "puedo", "puede", "tengo", "tiene", "hay", "habla", "llamo", "quiero",
        ],
        Lang::Fr => &[
            "le", "la", "les", "de", "des", "et", "est", "vous", "je", "pas", "que", "un", "une", "pour", "avec",
            "bonjour", "merci", "oui", "c'est", "ce", "il", "elle", "nous", "mais", "du", "au", "aux", "sur",
            "dans", "ne", "mon", "votre", "très", "bien", "bonsoir", "monsieur", "madame", "d'accord", "demain",
            "aujourd'hui", "ça", "j'ai", "suis", "je suis", "allô", "qui",
        ],
        Lang::De => &[
            "der", "die", "das", "und", "ist", "sie", "ich", "nicht", "ein", "eine", "zu", "mit", "für", "guten",
            "tag", "danke", "ja", "bitte", "haben", "wir", "auf", "den", "dem", "es", "sind", "bin", "wie", "was",
            "aber", "oder", "morgen", "heute", "herr", "frau", "schön", "gut", "hallo", "tschüss", "noch", "auch",
            "kann", "können", "ihre", "ihnen",
        ],
        Lang::Pt => &[
            "o", "os", "as", "que", "de", "e", "é", "você", "para", "com", "não", "um", "uma", "olá", "obrigado",
            "obrigada", "sim", "está", "estou", "por", "do", "da", "dos", "das", "no", "na", "meu", "minha", "seu",
            "sua", "mas", "como", "bom", "dia", "tarde", "noite", "amanhã", "hoje", "senhor", "senhora", "tudo",
            "bem", "tchau", "então", "aqui",
        ],
        _ => &[],
    }
}

/// A Latin-script line reads as another of the Latin-script languages than
/// `owner`: one of them has at least two of its common words in it, more than
/// the owner's language has.
fn latin_line_in_other_language(text: &str, owner: Lang) -> bool {
    let words: Vec<String> = text
        .split(|c: char| !(c.is_alphabetic() || c == '\'' || c == '’'))
        .map(|w| w.trim_matches(|c| c == '\'' || c == '’').replace('’', "'").to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let hits = |l: Lang| {
        let list = latin_function_words(l);
        words.iter().filter(|w| list.contains(&w.as_str())).count()
    };
    let own = hits(owner);
    [Lang::En, Lang::Es, Lang::Fr, Lang::De, Lang::Pt]
        .into_iter()
        .filter(|l| *l != owner)
        .any(|l| {
            let n = hits(l);
            n >= 2 && n > own
        })
}

/// A line of the dialogue is in another language than `owner` and long enough
/// to translate. Told by its letters (Chinese characters, kana, hangul, Latin
/// words — which Latin-script language by its common words — or anything
/// else). Short lines are left as they are: "OK", "Hello?", 「もしもし」, a name
/// read the same to anyone. Mixed lines go by what most of the line is in
/// (「我是 Tom」 is Chinese).
pub fn needs_translation(text: &str, owner: Lang) -> bool {
    let n = count_scripts(text);
    let japanese = n.kana > 0;
    let sizes = [
        (LineScript::Han, if japanese { 0 } else { n.han }),
        (LineScript::Japanese, if japanese { n.han + n.kana } else { 0 }),
        (LineScript::Hangul, n.hangul),
        (LineScript::Latin, n.latin_words),
        (LineScript::Other, n.other_words),
    ];
    let Some(&(script, size)) = sizes.iter().filter(|(_, s)| *s > 0).max_by_key(|(_, s)| *s) else {
        return false;
    };
    let short = match script {
        LineScript::Latin | LineScript::Other => size < 3,
        _ => size < 5,
    };
    if short {
        return false;
    }
    let own = match owner {
        Lang::ZhTw | Lang::ZhCn => LineScript::Han,
        Lang::Ja => LineScript::Japanese,
        Lang::Ko => LineScript::Hangul,
        _ => LineScript::Latin,
    };
    let own_size = sizes.iter().find(|(s, _)| *s == own).map_or(0, |(_, s)| *s);
    if own_size < size {
        return true;
    }
    own == LineScript::Latin && latin_line_in_other_language(text, owner)
}

/// The lines of a dialogue (`對方：` / `接聽：`, one per speaker run:
/// [`crate::gemini_live::merge_dialogue_fragments`]) to translate for `owner`:
/// the line's index and its words. From the start, at most
/// [`TRANSLATE_MAX_LINES`] lines and [`TRANSLATE_MAX_CHARS`] characters.
pub fn lines_to_translate(dialogue: &str, owner: Lang) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut chars = 0usize;
    for (i, line) in dialogue.lines().enumerate() {
        let Some((label, text)) = line.split_once('：') else {
            continue;
        };
        if !matches!(label.trim(), "對方" | "接聽") {
            continue;
        }
        let text = text.trim();
        if !needs_translation(text, owner) {
            continue;
        }
        let len = text.chars().count();
        if out.len() >= TRANSLATE_MAX_LINES || chars + len > TRANSLATE_MAX_CHARS {
            break;
        }
        chars += len;
        out.push((i, text.to_string()));
    }
    out
}

fn translate_system_prompt(owner: Lang) -> String {
    let name = locale::phrases(owner).name_en;
    format!(
        "You translate lines of a phone call transcript into {name}.\n\
         The user message is a JSON array of objects {{\"i\": number, \"t\": text}}, one per line.\n\
         Reply with only a JSON array of objects {{\"i\": the same number, \"t\": the translation}}, \
         one for every line, in the same order. No other text.\n\
         Translate each line on its own, faithfully and naturally, as words spoken on the phone. \
         Keep names, numbers, amounts and times as they are. Do not add notes; do not merge, split \
         or leave out lines."
    )
}

fn translate_request(asked: &[(usize, String)]) -> String {
    Value::Array(asked.iter().map(|(i, t)| json!({"i": i, "t": t})).collect()).to_string()
}

/// Room for the translations: about two tokens a character, and on Gemini
/// (whose thinking takes from the same budget) 1500 more.
fn translate_max_tokens(endpoint: &Endpoint, chars: usize) -> u32 {
    let base = (300 + chars * 2).min(8_000) as u32;
    if is_gemini(endpoint) {
        base + 1_500
    } else {
        base
    }
}

/// Keep one translation of a line asked for: not empty, not the line itself
/// (the model copied it), the first one for its index.
fn keep_translation(out: &mut Vec<(usize, String)>, asked: &[(usize, String)], i: usize, t: &str) {
    let Some((_, src)) = asked.iter().find(|(a, _)| *a == i) else {
        return;
    };
    let t = t.trim();
    if t.is_empty() || t == src.trim() || out.iter().any(|(j, _)| *j == i) {
        return;
    }
    out.push((i, t.to_string()));
}

/// The model's answer as (line index, translation) for the lines asked
/// (`asked`: [`lines_to_translate`]). Objects `{"i", "t"}` go by their index; a
/// bare array of strings only when it has one per line asked. A code fence or
/// words around the array are skipped. `None` when nothing usable came back.
pub fn parse_translations(reply: &str, asked: &[(usize, String)]) -> Option<Vec<(usize, String)>> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')?;
    if end <= start {
        return None;
    }
    let items: Vec<Value> = serde_json::from_str(&reply[start..=end]).ok()?;
    let mut out: Vec<(usize, String)> = Vec::new();
    if !items.is_empty() && items.iter().all(Value::is_string) {
        if items.len() != asked.len() {
            return None;
        }
        for ((i, _), v) in asked.iter().zip(&items) {
            keep_translation(&mut out, asked, *i, v.as_str().unwrap_or(""));
        }
    } else {
        for v in &items {
            let i = v.get("i").and_then(|x| {
                x.as_u64()
                    .or_else(|| x.as_str().and_then(|s| s.trim().parse().ok()))
            });
            let t = ["t", "text", "translation"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str));
            if let (Some(i), Some(t)) = (i, t) {
                keep_translation(&mut out, asked, i as usize, t);
            }
        }
    }
    out.sort_by_key(|(i, _)| *i);
    (!out.is_empty()).then_some(out)
}

/// Translations as one entry per line of a dialogue of `lines` lines, what the
/// call record keeps ([`crate::calls::CallNotes::dialogue_translations`]):
/// empty when there is none.
pub fn translations_by_line(lines: usize, found: &[(usize, String)]) -> Vec<Option<String>> {
    let mut v: Vec<Option<String>> = vec![None; lines];
    for (i, t) in found {
        if let Some(slot) = v.get_mut(*i) {
            *slot = Some(t.clone());
        }
    }
    if v.iter().all(Option::is_none) {
        Vec::new()
    } else {
        v
    }
}

/// The lines of `dialogue` said in another language than `owner`'s
/// ([`needs_translation`]), translated into it in one request to the summary's
/// service ([`summary_endpoint`], its models in order). One entry per line of
/// `dialogue` (`None` = left as said). Empty when nothing needs translating,
/// no service is set or none answered usably: the record then shows the lines
/// as they were said, and the summary and notification do not wait on this.
pub async fn translate_dialogue(http: &Client, settings: &Settings, dialogue: &str, owner: Lang) -> Vec<Option<String>> {
    let asked = lines_to_translate(dialogue, owner);
    if asked.is_empty() {
        return Vec::new();
    }
    let Some(endpoint) = summary_endpoint(settings) else {
        return Vec::new();
    };
    let system = translate_system_prompt(owner);
    let user = translate_request(&asked);
    let chars: usize = asked.iter().map(|(_, t)| t.chars().count()).sum();
    let max_tokens = translate_max_tokens(&endpoint, chars);
    for model in &endpoint.models {
        match chat_at(http, &endpoint, model, &system, &user, max_tokens).await {
            Ok(reply) => match parse_translations(&reply, &asked) {
                Some(found) => {
                    let found: Vec<(usize, String)> = found
                        .into_iter()
                        .map(|(i, t)| (i, locale::for_owner_in(owner, &t)))
                        .collect();
                    info!(
                        "dialogue translated model={model} lines={} asked={}",
                        found.len(),
                        asked.len()
                    );
                    return translations_by_line(dialogue.lines().count(), &found);
                }
                None => warn!(
                    "dialogue translation unreadable model={model} reply_chars={}",
                    reply.chars().count()
                ),
            },
            Err(e) => warn!("dialogue translation failed model={model}: {e:#}"),
        }
    }
    Vec::new()
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
    fn summary_provider_pins_gemini_lite_or_deepseek() {
        fn models(e: &Endpoint) -> Vec<&str> {
            e.models.iter().map(String::as_str).collect()
        }
        let mut s = settings();
        s.gemini_api_key = "AIzaTEST".into();
        s.deepseek_api_key = "sk-ds".into();
        s.summary_api_base = "http://127.0.0.1:11434/v1".into();
        // Gemini: its lowest-cost model, then the regular one; the own endpoint
        // and the DeepSeek key are passed over.
        let e = summary_endpoint_for(&s, Provider::Gemini).unwrap();
        assert_eq!((e.url.as_str(), e.key.as_str()), (GEMINI_OPENAI_URL, "AIzaTEST"));
        assert_eq!(models(&e), vec![GEMINI_SUMMARY_LITE_MODEL, GEMINI_SUMMARY_MODEL]);
        assert_eq!(summary_max_tokens(&e), 1_500);
        // A model typed in the settings goes first.
        s.summary_model = "gemini-2.5-flash".into();
        let e = summary_endpoint_for(&s, Provider::Gemini).unwrap();
        assert_eq!(models(&e), vec!["gemini-2.5-flash", GEMINI_SUMMARY_LITE_MODEL, GEMINI_SUMMARY_MODEL]);
        // A model from another service is not sent to Gemini.
        s.summary_model = "deepseek-chat".into();
        let e = summary_endpoint_for(&s, Provider::Gemini).unwrap();
        assert_eq!(models(&e), vec![GEMINI_SUMMARY_LITE_MODEL, GEMINI_SUMMARY_MODEL]);
        s.summary_model = "gemini-2.5-flash".into();
        // DeepSeek: passes over the own endpoint too.
        let e = summary_endpoint_for(&s, Provider::DeepSeek).unwrap();
        assert_eq!((e.url.as_str(), e.key.as_str()), (DEEPSEEK_API_URL, "sk-ds"));
        // Auto is the order it always was: the own endpoint first.
        let e = summary_endpoint_for(&s, Provider::Auto).unwrap();
        assert_eq!(e.url, "http://127.0.0.1:11434/v1/chat/completions");
        assert_eq!(models(&e), vec!["gemini-2.5-flash"]);
        // A choice without its key works like auto.
        s.gemini_api_key = "".into();
        assert_eq!(summary_endpoint_for(&s, Provider::Gemini), summary_endpoint_for(&s, Provider::Auto));
        s.summary_api_base = String::new();
        s.deepseek_api_key = String::new();
        s.summary_model = String::new();
        s.gemini_api_key = "AIzaTEST".into();
        let e = summary_endpoint_for(&s, Provider::DeepSeek).unwrap();
        assert_eq!((e.url.as_str(), models(&e)), (GEMINI_OPENAI_URL, vec![GEMINI_SUMMARY_MODEL]));
        // The DeepSeek key may come from the older TRTC LLM config.
        s.trtc_llm_config = r#"{"APIKey":"sk-cfg"}"#.into();
        let e = summary_endpoint_for(&s, Provider::DeepSeek).unwrap();
        assert_eq!((e.url.as_str(), e.key.as_str()), (DEEPSEEK_API_URL, "sk-cfg"));
        // Nothing set: nothing to call, whatever the choice.
        let empty = settings();
        for p in [Provider::Auto, Provider::Gemini, Provider::DeepSeek] {
            assert_eq!(summary_endpoint_for(&empty, p), None, "{p:?}");
        }
    }

    #[test]
    fn summary_provider_setting_values() {
        assert_eq!(Provider::parse(" Gemini "), Provider::Gemini);
        assert_eq!(Provider::parse("deepseek"), Provider::DeepSeek);
        for other in ["", "auto", "openai", "gemini-flash-lite-latest"] {
            assert_eq!(Provider::parse(other), Provider::Auto, "{other}");
        }
        for p in [Provider::Auto, Provider::Gemini, Provider::DeepSeek] {
            assert_eq!(Provider::parse(p.as_str()), p);
        }
    }

    #[test]
    fn summary_is_written_for_the_owner_whatever_the_call_language() {
        // The prompt asks for the owner's language even when the call was in another.
        assert!(summary_system_prompt(Lang::ZhTw, "").contains("通話是別的語言也一樣"));
        assert!(summary_system_prompt(Lang::ZhCn, "").contains("通话是别的语言也一样"));
        assert!(summary_system_prompt(Lang::Ja, "").contains("Write in Japanese, whatever language the call was in"));
        // The input's sections follow the summary's language, not the call's.
        let dialogue = "接聽：Hello?\n對方：Hi, it's the dentist. Your appointment is tomorrow at 3.";
        let zh = build_summary_input_in(Lang::ZhTw, "", 30_000, "", dialogue, "", "");
        assert!(zh.starts_with("【對話】") && zh.contains("對方：Hi, it's the dentist."), "{zh}");
        let en = build_summary_input_in(Lang::En, "", 30_000, "", dialogue, "", "");
        assert!(en.starts_with("[Dialogue]") && en.contains("Caller: Hi, it's the dentist."), "{en}");
        // Traditional characters only when the owner reads zh-TW.
        assert_eq!(polish_summary(Lang::ZhTw, " 事情：听录音 ", ""), "事情：聽錄音");
        assert_eq!(polish_summary(Lang::ZhCn, "事情：听录音", ""), "事情：听录音");
        assert_eq!(polish_summary(Lang::Ja, " 用件：会議の件 ", "記得繳水費"), "用件：会議の件");
        // A matter the model dropped is put back in Chinese only.
        assert!(polish_summary(Lang::ZhTw, "事情：聽不清楚", "對方：記得繳水費").contains("事情：繳水費"));
        assert_eq!(polish_summary(Lang::En, "About: unclear", "對方：記得繳水費"), "About: unclear");
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
    fn the_ais_words_are_not_the_callers_matter() {
        let input = build_summary_input(
            "0912345678",
            30_000,
            "",
            "接聽：喂？我在開會。\n對方：明天送貨，有人在家嗎？\n接聽：好，放管理室就好。",
            "",
            "",
        );
        let summary = "對方：沒說是誰\n事情：明天送貨，問有沒有人在家\nAI 回覆：說放管理室\n要處理：不用";
        assert_eq!(overlay_summary_matter(summary, &input), summary);
        let en = build_summary_input_en("0912345678", 30_000, "", "接聽：我在開會\n對方：明天送貨", "", "");
        assert_eq!(overlay_summary_matter(summary, &en), summary);
    }

    #[test]
    fn a_matter_the_summary_dropped_is_added_to_what_it_wrote() {
        let summary = "對方：小明（朋友）\n事情：明天送貨\nAI 回覆：說好\n要處理：不用";
        let out = overlay_summary_matter(summary, "接聽：喂？\n對方：明天送貨，順便提醒你繳水費");
        assert!(out.contains("事情：明天送貨；也提到缴水费"), "{out}");
        assert!(out.contains("AI 回覆：說好"), "{out}");
    }

    #[test]
    fn speaking_another_language_is_not_called_a_scam() {
        for lang in [Lang::ZhTw, Lang::ZhCn] {
            let p = summary_system_prompt(lang, "");
            assert!(!p.contains("講外語或聽不懂的內容，就當") && !p.contains("讲外语或听不懂的内容，就当"), "{p}");
            assert!(p.contains("對方講外語本身不算") || p.contains("对方讲外语本身不算"), "{p}");
        }
        assert!(summary_system_prompt(Lang::En, "").contains("speaking another language is not a sales or scam call"));
    }

    /// A card desk checking a purchase (2026-11-14: summed up as 「疑似詐騙」,
    /// 「要處理：不用」): not a scam; the matter names bank, amount, shop and
    /// time; the owner is told to check it on the number on the back of the
    /// card — in every language the owner reads.
    #[test]
    fn a_card_check_summary_tells_the_owner_to_check_it() {
        for l in locale::ALL {
            let p = summary_system_prompt(l, "Alex");
            let marks: &[&str] = match l {
                Lang::ZhTw => &[
                    "確認某一筆刷卡或交易是不是Alex本人",
                    "不是推銷也不是詐騙，就算【通話資訊】寫 AI 陪他聊也一樣",
                    "「事情」寫哪家銀行、金額、商店、時間",
                    "不是就打卡片背面客服，別回撥來電",
                    "不能寫「不用」",
                ],
                Lang::ZhCn => &["确认某一笔刷卡或交易是不是Alex本人", "不是推销也不是诈骗", "哪家银行、金额、商店、时间", "卡片背面客服", "不能写"],
                _ => &[
                    "check whether a card charge or transaction was Alex's",
                    "is neither a sales call nor a scam, even if [Call facts] says the AI kept them talking",
                    "the bank, the amount, the shop and the time",
                    "call the number on the back of the card (not the number that called)",
                    "never that there is nothing to do",
                ],
            };
            for m in marks {
                assert!(p.contains(m), "{l:?}: {m}\n{p}");
            }
            assert!(!p.contains("{s_") && !p.contains("{owner}"), "{l:?}");
        }
        // Its matter line is no pitch.
        let s = "對方：大愛銀行信用卡中心\n事情：大愛銀行：昨晚11點網路電器行3280元，問是否本人刷\nAI 回覆：說要再想一下\n要處理：自己確認是否本人刷；不是就打卡片背面客服，別回撥來電";
        assert!(!summary_says_sales(Lang::ZhTw, s));
    }

    /// Reviewer: Whisper's "AI 對話段" is the caller's uplink while the AI was
    /// on (calls.rs `ai_remote_pcm` → `_ai.wav`), so the caller's matter in it counts.
    #[test]
    fn reviewer_whisper_ai_segment_is_the_callers_voice() {
        let summary = "對方：沒說是誰\n事情：聽不清楚\nAI 回覆：只有打招呼\n要處理：不用";
        for dialogue in ["", "接聽：喂？"] {
            let input = build_summary_input("0912345678", 20_000, "", dialogue, "嗯要繳水費", "嗯要繳水費");
            let out = overlay_summary_matter(summary, &input);
            assert!(out.contains("事情：缴水费"), "{input}\n---\n{out}");
        }
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

#[cfg(test)]
mod translate_tests {
    use super::*;

    fn no_service() -> Settings {
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
    fn foreign_lines_by_their_letters() {
        let tw = Lang::ZhTw;
        // The call this came from: English on a Taiwanese owner's record.
        assert!(needs_translation("Hello. Do you speak English?", tw));
        assert!(needs_translation(
            "Should I leave it with the front desk, or do you want to come and get it tonight?",
            tw
        ));
        // Short lines read the same to anyone.
        for short in ["Hello?", "OK", "Hey Mike!", "Bye-bye.", "Goodbye.", "もしもし。", "여보세요?", "1234 5678", ""] {
            assert!(!needs_translation(short, tw), "{short}");
        }
        // Chinese with an English name or brand in it is Chinese; Simplified too.
        assert!(!needs_translation("嗨。我是Tom。我剛搬進你的大樓。", tw));
        assert!(!needs_translation("我要訂 iPhone 15 Pro 兩支", tw));
        assert!(!needs_translation("我这边是管理室，你有一个包裹", tw));
        // Japanese (kanji or not), Korean, Russian are other languages.
        assert!(needs_translation("土曜日のハイキング、日曜日に変更になったよ。", tw));
        assert!(needs_translation("그럼 경비실에 맡겨 둘게요", tw));
        assert!(needs_translation("Здравствуйте, это служба доставки", tw));
        // A Japanese owner: Chinese is foreign, Japanese is not.
        assert!(needs_translation("你好，我這邊是黑貓宅急便。", Lang::Ja));
        assert!(!needs_translation("土曜日のハイキング、日曜日に変更になったよ。", Lang::Ja));
        // An English owner: Chinese and Spanish are foreign, English is not.
        assert!(!needs_translation("Should I leave it with the front desk?", Lang::En));
        assert!(needs_translation("你好，我這邊是黑貓宅急便。", Lang::En));
        assert!(needs_translation("Hola, buenos días, ¿está el señor Wang?", Lang::En));
        assert!(!needs_translation("Gracias", Lang::En));
        // A Spanish owner reads Spanish, not English; German for a French owner.
        assert!(needs_translation("Should I leave it with the front desk?", Lang::Es));
        assert!(!needs_translation("Hola, buenos días, ¿está el señor Wang?", Lang::Es));
        assert!(!needs_translation("Guten Tag, ich rufe wegen Ihrer Bestellung an.", Lang::De));
        assert!(needs_translation("Guten Tag, ich rufe wegen Ihrer Bestellung an.", Lang::Fr));
        // Latin words that tell no language apart: left as they are.
        assert!(!needs_translation("Maple Family Clinic", Lang::En));
    }

    #[test]
    fn translations_go_back_to_their_lines() {
        let dialogue = "接聽：Hello?\n對方：Hello. Do you speak English?\n接聽：Yes, I do. What can I do for you?\n對方：好，那我放管理室了。\n對方：Okay, sounds good. Thanks. Bye.";
        let asked = lines_to_translate(dialogue, Lang::ZhTw);
        assert_eq!(asked.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![1, 2, 4]);
        assert_eq!(asked[0].1, "Hello. Do you speak English?");
        let req: Value = serde_json::from_str(&translate_request(&asked)).unwrap();
        assert_eq!(req[0], json!({"i": 1, "t": "Hello. Do you speak English?"}));
        assert!(translate_system_prompt(Lang::ZhTw).contains("Traditional Chinese (Taiwan Mandarin)"));
        // By index, in any order, in a code fence; an index not asked is dropped.
        let reply = "```json\n[{\"i\": 4, \"t\": \"好，聽起來不錯。謝謝，掰掰。\"}, {\"i\": 1, \"t\": \"你好，你會說英文嗎？\"}, {\"i\": \"2\", \"text\": \"會，有什麼事嗎？\"}, {\"i\": 9, \"t\": \"沒問的\"}]\n```";
        let found = parse_translations(reply, &asked).unwrap();
        assert_eq!(
            found,
            vec![
                (1, "你好，你會說英文嗎？".to_string()),
                (2, "會，有什麼事嗎？".to_string()),
                (4, "好，聽起來不錯。謝謝，掰掰。".to_string()),
            ]
        );
        let by_line = translations_by_line(dialogue.lines().count(), &found);
        assert_eq!(by_line.len(), 5);
        assert_eq!(by_line[0], None);
        assert_eq!(by_line[1].as_deref(), Some("你好，你會說英文嗎？"));
        assert_eq!(by_line[3], None);
        // A bare array of strings counts only with one per line asked.
        let plain = "[\"你好，你會說英文嗎？\", \"會，有什麼事嗎？\", \"好，掰掰。\"]";
        assert_eq!(parse_translations(plain, &asked).unwrap().len(), 3);
        assert!(parse_translations("[\"你好\", \"會\"]", &asked).is_none());
        // A line copied as it was, or left empty, is no translation.
        let copied = "[{\"i\":1,\"t\":\"Hello. Do you speak English?\"},{\"i\":2,\"t\":\" \"}]";
        assert!(parse_translations(copied, &asked).is_none());
        for bad in ["Sorry, I cannot help with that.", "[{\"i\": 1}", "{\"i\": 1, \"t\": \"x\"}", "[]"] {
            assert!(parse_translations(bad, &asked).is_none(), "{bad}");
        }
        assert!(translations_by_line(5, &[]).is_empty());
        assert!(translations_by_line(2, &[(7, "x".into())]).is_empty());
        // Nothing in another language: nothing to ask.
        assert!(lines_to_translate("對方：好，那我放管理室了。\n接聽：好，謝謝，掰掰。", Lang::ZhTw).is_empty());
    }

    #[tokio::test]
    async fn one_request_and_the_owners_script() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dialogue = "接聽：Hello?\n對方：Hello. Do you speak English?\n接聽：Yes, I do. What can I do for you?";
        // No service set: nothing, and the record shows the lines as said.
        assert!(translate_dialogue(&Client::new(), &no_service(), dialogue, Lang::ZhTw).await.is_empty());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf: Vec<u8> = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = sock.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(head) = text.find("\r\n\r\n") {
                    let len = text[..head]
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if buf.len() >= head + 4 + len {
                        break;
                    }
                }
            }
            // The model answers in Simplified Chinese.
            let answer = json!([{"i": 1, "t": "你好，你会说英文吗？"}, {"i": 2, "t": "会，有什么事吗？"}]).to_string();
            let body = json!({"choices": [{"message": {"content": answer}}]}).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buf).to_string()
        });
        let mut s = no_service();
        s.summary_api_base = format!("http://127.0.0.1:{port}/v1");
        s.summary_model = "local-model".into();
        let out = translate_dialogue(&Client::new(), &s, dialogue, Lang::ZhTw).await;
        // One entry per line, the short "Hello?" left alone, Taiwan's script.
        assert_eq!(
            out,
            vec![None, Some("你好，你會說英文嗎？".to_string()), Some("會，有什麼事嗎？".to_string())]
        );
        let request = server.await.unwrap();
        assert!(request.starts_with("POST /v1/chat/completions "), "{request}");
        assert!(request.contains("local-model") && request.contains("Hello. Do you speak English?"), "{request}");
    }
}

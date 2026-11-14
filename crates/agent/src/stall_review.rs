//! A second look before the AI starts keeping a caller busy.
//!
//! The keyword score in `gemini_live` trips on words that honest calls use as
//! well: a relative asking for a transfer, a courier collecting cash on
//! delivery, the card desk confirming a purchase the owner made, a recorded
//! menu ("press 1"). The keywords stay as they are; when they cross the line, a
//! cheap model reads the dialogue so far and says which kind of call it is
//! ([`review`]).
//!
//! The answer has to come within [`REVIEW_TIMEOUT`]: the caller is on the line.
//! No key, an error, a slow or unreadable answer all count as a scam or sales
//! call — what the keywords alone decided before — because the owner would
//! rather keep a doubtful caller talking than let a scammer go.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};

/// What the reviewing model makes of a call the keywords flagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// A sales pitch or a scam: keep them talking.
    ScamOrSales,
    /// A recorded message or voice menu: nobody to keep talking.
    IvrRecording,
    /// An ordinary call that happened to use the words.
    Normal,
}

impl Verdict {
    fn from_label(label: &str) -> Option<Verdict> {
        match label.trim().to_ascii_lowercase().as_str() {
            "scam_or_sales" => Some(Verdict::ScamOrSales),
            "ivr_recording" => Some(Verdict::IvrRecording),
            "normal" => Some(Verdict::Normal),
            _ => None,
        }
    }
}

/// How long the caller may wait for the verdict.
pub const REVIEW_TIMEOUT: Duration = Duration::from_millis(1500);

/// Gemini's lowest-cost model: the question is short and the answer one word.
pub const REVIEW_MODEL: &str = "gemini-flash-lite-latest";

const GEMINI_BASE: &str = "https://generativelanguage.googleapis.com";

/// The end of a long call is what tripped the keywords; a shorter input also
/// answers faster.
const MAX_DIALOGUE_CHARS: usize = 3000;

const SYSTEM_PROMPT: &str = "\
You check a phone call that is still going on. An AI answers the phone for its owner. A keyword check thinks the caller may be a scammer or a salesperson; if so, the AI will keep them talking on purpose. Decide from the dialogue so far whether that is right.\n\
The dialogue may be in any language. \"Caller:\" lines are the person (or recording) that called; \"AI:\" lines are the AI answering for the owner.\n\
Answer with JSON only, one of: {\"verdict\":\"scam_or_sales\"} {\"verdict\":\"ivr_recording\"} {\"verdict\":\"normal\"}.\n\
\n\
scam_or_sales: someone posing as a bank, the police, a prosecutor, a government office or customer service who wants the owner to work an ATM or online banking, cancel an instalment plan, move money into a supervised or safe account, add them on LINE or another chat app, join an investment group, or who promises guaranteed profits; anyone selling insurance, loans, investments or other products or services. Asking for a password, a one-time code, a PIN or ATM steps is scam_or_sales, whoever the caller claims to be.\n\
ivr_recording: a pre-recorded message or an automatic voice menu: a machine reading out options, \"press 1\", \"press the pound key\", leave a message after the tone.\n\
normal: family or friends talking about sending or receiving money; a courier delivering a parcel, cash on delivery included; a bank, card issuer or insurer following up on something the owner applied for or did (a call to verify an application, a fraud-risk check on a card purchase, a claim's progress) without asking for passwords, verification codes or ATM steps; anyone else calling about their own ordinary business.\n\
\n\
If you are unsure, answer scam_or_sales.";

/// One client for the life of the agent: a pooled connection saves the TLS
/// handshake on the next review.
fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(Client::new)
}

/// Ask the model whether a call the keywords flagged is a scam or sales call,
/// a recording or an ordinary call. `dialogue` = the call so far (`對方：` /
/// `接聽：` or `Caller:` / `AI:` lines), `hits` = the keywords that matched.
/// Anything but a clear answer within [`REVIEW_TIMEOUT`] is
/// [`Verdict::ScamOrSales`].
pub async fn review(api_key: &str, dialogue: &str, hits: &[String], caller_id: &str) -> Verdict {
    review_at(client(), GEMINI_BASE, api_key, dialogue, hits, caller_id).await
}

/// [`review`] against any server (tests run a local one).
async fn review_at(
    http: &Client,
    base: &str,
    api_key: &str,
    dialogue: &str,
    hits: &[String],
    caller_id: &str,
) -> Verdict {
    let head = head_chars(dialogue.trim(), 80);
    let key = api_key.trim();
    if key.is_empty() {
        warn!("stall review: no Gemini key, kept as scam_or_sales caller={caller_id} dialogue={head:?}");
        return Verdict::ScamOrSales;
    }
    let started = Instant::now();
    match tokio::time::timeout(REVIEW_TIMEOUT, ask(http, base, key, dialogue, hits, caller_id)).await {
        Ok(Ok(verdict)) => {
            info!(
                "stall review verdict={verdict:?} caller={caller_id} hits={hits:?} ms={} dialogue={head:?}",
                started.elapsed().as_millis()
            );
            verdict
        }
        Ok(Err(e)) => {
            warn!("stall review failed, kept as scam_or_sales caller={caller_id}: {e:#} dialogue={head:?}");
            Verdict::ScamOrSales
        }
        Err(_) => {
            warn!(
                "stall review timed out after {} ms, kept as scam_or_sales caller={caller_id} dialogue={head:?}",
                REVIEW_TIMEOUT.as_millis()
            );
            Verdict::ScamOrSales
        }
    }
}

async fn ask(
    http: &Client,
    base: &str,
    key: &str,
    dialogue: &str,
    hits: &[String],
    caller_id: &str,
) -> Result<Verdict> {
    let url = format!("{}/v1beta/models/{REVIEW_MODEL}:generateContent", base.trim_end_matches('/'));
    let mut thinking_off = true;
    loop {
        let body = request_body(dialogue, hits, caller_id, thinking_off);
        let resp = http
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await
            .context("request")?;
        let status = resp.status();
        let text = resp.text().await.context("response body")?;
        if status.as_u16() == 400 && thinking_off && text.contains("thinking") {
            // A model that cannot switch thinking off refuses the whole request;
            // asking again without the setting still beats the timeout.
            thinking_off = false;
            continue;
        }
        if !status.is_success() {
            bail!("status {status}: {}", head_chars(&text, 80));
        }
        return parse_verdict(&text).with_context(|| format!("unreadable answer: {}", head_chars(&text, 80)));
    }
}

/// The generateContent request. Thinking is switched off and the answer is
/// held to the three labels: every token is time the caller waits.
fn request_body(dialogue: &str, hits: &[String], caller_id: &str, thinking_off: bool) -> Value {
    let dialogue = crate::deepseek::relabel_dialogue_en(dialogue.trim());
    let number = match caller_id.trim() {
        "" => "hidden",
        n => n,
    };
    let user = format!(
        "Number: {number}\nKeywords that matched: {}\n\nDialogue so far:\n{}",
        hits.join(", "),
        tail_chars(&dialogue, MAX_DIALOGUE_CHARS)
    );
    let mut generation = json!({
        "temperature": 0,
        "maxOutputTokens": 32,
        "responseMimeType": "application/json",
        "responseSchema": {
            "type": "OBJECT",
            "properties": {
                "verdict": {"type": "STRING", "enum": ["scam_or_sales", "ivr_recording", "normal"]}
            },
            "required": ["verdict"]
        }
    });
    if thinking_off {
        generation["thinkingConfig"] = json!({"thinkingBudget": 0});
    }
    json!({
        "systemInstruction": {"parts": [{"text": SYSTEM_PROMPT}]},
        "contents": [{"role": "user", "parts": [{"text": user}]}],
        "generationConfig": generation,
    })
}

/// The verdict in a generateContent response, or in the model's own
/// `{"verdict": …}` text (also when wrapped in a code fence).
fn parse_verdict(body: &str) -> Option<Verdict> {
    let body = body.trim();
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        if let Some(label) = v.get("verdict").and_then(Value::as_str) {
            return Verdict::from_label(label);
        }
        let parts = v.pointer("/candidates/0/content/parts")?.as_array()?;
        let text: String = parts
            .iter()
            .filter(|p| !p.get("thought").and_then(Value::as_bool).unwrap_or(false))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect();
        return parse_verdict(&text);
    }
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    if end <= start || end + 1 - start == body.len() {
        return None;
    }
    parse_verdict(&body[start..=end])
}

fn head_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn tail_chars(s: &str, n: usize) -> &str {
    let count = s.chars().count();
    if count <= n {
        return s;
    }
    let skip = s.char_indices().nth(count - n).map_or(0, |(i, _)| i);
    &s[skip..]
}

/// Words a recorded voice menu says: "請按 1", "press one", "番を押して", … in
/// the agent's languages. A hint for the caller of [`review`]; a menu cannot
/// be kept talking.
pub fn ivr_cue(text: &str) -> bool {
    let lower = text.to_lowercase();
    let squeezed: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    run_cue(&squeezed) || word_cue(&lower)
}

/// Phrases that only a menu says, matched with spaces removed.
const RUN_PHRASES: &[&str] = &[
    "重聽請按", "重听请按", "轉接專人請按", "转接专人请按", "請按井字鍵", "请按井字键",
    "語音信箱", "语音信箱", "番を押して", "번을눌러", "번을누르",
];

const HAN_NUMERALS: &str = "零〇一二三四五六七八九";
const THAI_NUMERALS: &[&str] = &[
    "ศูนย์", "หนึ่ง", "สอง", "สาม", "สี่", "ห้า", "หก", "เจ็ด", "แปด", "เก้า",
];

fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || ('０'..='９').contains(&c)
}

/// Languages written without spaces: a press verb followed by a key.
fn run_cue(s: &str) -> bool {
    if RUN_PHRASES.iter().any(|p| s.contains(p)) {
        return true;
    }
    // 請按 / 撳 (Cantonese) take a spoken numeral too; a bare 按 only a digit,
    // since 按一下 is "press it once" and 按時 "on time".
    let han = |verb: &str, numerals: bool| {
        s.match_indices(verb).any(|(i, _)| {
            let mut rest = s[i + verb.len()..].chars();
            let Some(key) = rest.next() else { return false };
            let next = rest.next();
            if is_digit(key) {
                // 按1成 / 按10% are rates, not keys.
                !next.is_some_and(|n| is_digit(n) || matches!(n, '%' | '％' | '成' | '折'))
            } else if matches!(key, '#' | '＃' | '*' | '＊') {
                true
            } else if matches!(key, '井' | '星' | '米') {
                // 井字鍵 / 星號 / 米字鍵, not 按星期.
                next.is_some_and(|n| matches!(n, '字' | '號' | '号' | '鍵' | '键'))
            } else if numerals && HAN_NUMERALS.contains(key) {
                !next.is_some_and(|n| matches!(n, '下' | '次' | '按' | '點' | '点'))
            } else {
                false
            }
        })
    };
    if han("請按", true) || han("请按", true) || han("撳", true) || han("按", false) {
        return true;
    }
    // Japanese: 1を押して / ＃を押して (番を押して is a phrase above).
    let ja = s.match_indices("を押して").any(|(i, _)| {
        s[..i]
            .chars()
            .last()
            .is_some_and(|c| is_digit(c) || HAN_NUMERALS.contains(c) || matches!(c, '#' | '＃' | '*' | '＊'))
    });
    // Thai: กด 1 / กดหนึ่ง, not กดดัน ("pressure").
    let th = s.match_indices("กด").any(|(i, _)| {
        let rest = &s[i + "กด".len()..];
        rest.chars().next().is_some_and(|c| is_digit(c) || ('๐'..='๙').contains(&c))
            || THAI_NUMERALS.iter().any(|n| rest.starts_with(n))
    });
    ja || th
}

/// Phrases of word languages that only a menu says.
const WORD_PHRASES: &[&[&str]] = &[
    &["to", "speak", "to", "an", "agent"],
    &["para", "español"],
    &["para", "espanol"],
    &["appuyez", "sur"],
    &["drücken", "sie"],
    &["pressione"],
    &["nhấn", "phím"],
    &["bấm", "phím"],
];

/// Press verbs that take a key: "press 1", "pulse 1", "tapez 1", "tekan 1", …
const PRESS_VERBS: &[&str] = &["press", "pulse", "marque", "oprima", "presione", "tapez", "digite", "tecle", "tekan"];

/// Words that may stand between the verb and the key ("press the pound key").
const KEY_ARTICLES: &[&str] = &["the", "la", "el", "le", "a", "o", "on"];

/// Keys by name: en, es, fr, de, pt, id.
const KEY_WORDS: &[&str] = &[
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "pound", "star", "hash",
    "cero", "uno", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve", "asterisco",
    "zéro", "un", "deux", "trois", "quatre", "cinq", "sept", "huit", "neuf", "dièse", "étoile",
    "null", "eins", "zwei", "drei", "vier", "fünf", "sechs", "sieben", "acht", "neun",
    "um", "dois", "três", "quatro", "sete", "oito", "nove",
    "nol", "satu", "dua", "tiga", "empat", "lima", "enam", "tujuh", "delapan", "sembilan",
];

/// Whole words, so "pressed" is not "press" and "pressione" not "press".
/// `#` and `*` are words of their own.
fn tokens(lower: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in lower.chars() {
        if c.is_alphanumeric() {
            cur.push(c);
            continue;
        }
        if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if matches!(c, '#' | '*') {
            out.push(c.to_string());
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn word_cue(lower: &str) -> bool {
    let ws = tokens(lower);
    let is_key = |w: &str| {
        (w.len() <= 2 && w.chars().all(|c| c.is_ascii_digit())) || w == "#" || w == "*" || KEY_WORDS.contains(&w)
    };
    if WORD_PHRASES
        .iter()
        .any(|p| ws.windows(p.len()).any(|win| win.iter().zip(p.iter()).all(|(a, b)| a == b)))
    {
        return true;
    }
    ws.iter().enumerate().any(|(i, w)| {
        if !PRESS_VERBS.contains(&w.as_str()) {
            return false;
        }
        let mut j = i + 1;
        if ws.get(j).is_some_and(|a| KEY_ARTICLES.contains(&a.as_str())) {
            j += 1;
        }
        ws.get(j).is_some_and(|k| is_key(k))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn menu_words_in_every_language() {
        for text in [
            "查詢帳單請按1，重聽請按九",
            "您好，轉接專人請按 9",
            "請按井字鍵結束",
            "請按#號",
            "您撥的電話將轉接到語音信箱",
            "按 1 查詢餘額",
            "普通話服務請按二",
            "广东话服务请按三，重听请按零",
            "粵語服務請撳一字",
            "For billing, press 1.",
            "Press one for sales.",
            "Press pound to repeat this menu.",
            "To speak to an agent, stay on the line.",
            "Para español, oprima dos.",
            "1番を押してください",
            "日本語は１を押してください",
            "상담원 연결은 0번을 눌러 주세요",
            "Pulse 1 para ventas",
            "Para soporte, marque uno",
            "Appuyez sur la touche étoile",
            "Pour le service client, tapez 1",
            "Für Deutsch drücken Sie die Eins",
            "Pressione a tecla asterisco",
            "Para suporte, digite 1",
            "Para vendas, tecle um",
            "Vui lòng nhấn phím 1 để gặp nhân viên",
            "Bấm phím 9 để nghe lại",
            "กด 1 เพื่อติดต่อเจ้าหน้าที่",
            "กดหนึ่งเพื่อฟังอีกครั้ง",
            "Untuk layanan pelanggan, tekan 1",
        ] {
            assert!(ivr_cue(text), "{text}");
        }
    }

    #[test]
    fn everyday_words_are_not_a_menu() {
        for text in [
            "我請你吃飯按讚",
            "請按時繳費喔",
            "請按照合約辦理",
            "你到了按一下門鈴",
            "利息按1成計算",
            "請按星期一的時間來",
            "I pressed one key and it broke",
            "She was pressing 1 again",
            "The press release is out",
            "Je suis sur la route",
            "Sie drücken nicht",
            "กดดันมากเลย",
            "明天送貨到你家",
            "",
        ] {
            assert!(!ivr_cue(text), "{text}");
        }
    }

    #[test]
    fn verdicts_from_the_response_and_the_model_text() {
        let body = r#"{"candidates":[{"content":{"parts":[{"text":"{\"verdict\":\"normal\"}"}],"role":"model"}}]}"#;
        assert_eq!(parse_verdict(body), Some(Verdict::Normal));
        assert_eq!(parse_verdict(r#"{"verdict":"ivr_recording"}"#), Some(Verdict::IvrRecording));
        assert_eq!(parse_verdict(r#"{"verdict":"Scam_Or_Sales"}"#), Some(Verdict::ScamOrSales));
        assert_eq!(
            parse_verdict("```json\n{\"verdict\": \"normal\"}\n```"),
            Some(Verdict::Normal)
        );
        // Thought parts are not the answer.
        let thought = r#"{"candidates":[{"content":{"parts":[{"text":"maybe normal","thought":true},{"text":"{\"verdict\":\"scam_or_sales\"}"}]}}]}"#;
        assert_eq!(parse_verdict(thought), Some(Verdict::ScamOrSales));
        assert_eq!(parse_verdict(r#"{"verdict":"maybe"}"#), None);
        assert_eq!(parse_verdict(r#"{"candidates":[]}"#), None);
        assert_eq!(parse_verdict("not json {at all"), None);
        assert_eq!(parse_verdict(""), None);
    }

    #[test]
    fn the_request_switches_thinking_off_and_keeps_the_end_of_a_long_call() {
        let long = format!("對方：{}\n對方：請你去 ATM 解除分期", "喂".repeat(4000));
        let hits = vec!["ATM".to_string(), "解除分期".to_string()];
        let body = request_body(&long, &hits, "+12025550143", true);
        let g = &body["generationConfig"];
        assert_eq!(g["temperature"], 0);
        assert_eq!(g["responseMimeType"], "application/json");
        assert_eq!(g["thinkingConfig"]["thinkingBudget"], 0);
        let user = body["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(user.contains("Caller: 請你去 ATM 解除分期"), "{user}");
        assert!(user.contains("Keywords that matched: ATM, 解除分期"), "{user}");
        assert!(user.contains("Number: +12025550143"), "{user}");
        assert!(user.chars().count() < MAX_DIALOGUE_CHARS + 200);
        let plain = request_body("對方：你好", &[], "", false);
        assert!(plain["generationConfig"].get("thinkingConfig").is_none());
        assert!(plain["contents"][0]["parts"][0]["text"].as_str().unwrap().contains("Number: hidden"));
    }

    /// A one-request-per-connection HTTP server: answers each request in turn
    /// with (status, body, delay) and hands back what it was sent.
    async fn serve(replies: Vec<(u16, &'static str, Duration)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut seen = Vec::new();
            for (status, body, delay) in replies {
                let (mut sock, _) = listener.accept().await.unwrap();
                seen.push(read_request(&mut sock).await);
                tokio::time::sleep(delay).await;
                let reply = format!(
                    "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(reply.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
            seen
        });
        (base, handle)
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(h) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..h]).to_lowercase();
                let len = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= h + 4 + len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    const NORMAL: &str = r#"{"candidates":[{"content":{"parts":[{"text":"{\"verdict\":\"normal\"}"}]}}]}"#;

    fn hits() -> Vec<String> {
        vec!["匯款".to_string()]
    }

    #[tokio::test]
    async fn a_quick_answer_is_the_verdict() {
        let (base, server) = serve(vec![(200, NORMAL, Duration::ZERO)]).await;
        let v = review_at(&Client::new(), &base, "test-key", "對方：媽，我下午匯款給你", &hits(), "0912345678").await;
        assert_eq!(v, Verdict::Normal);
        let seen = server.await.unwrap();
        let req = seen[0].to_lowercase();
        assert!(req.starts_with("post /v1beta/models/gemini-flash-lite-latest:generatecontent "), "{req}");
        assert!(req.contains("x-goog-api-key: test-key"), "{req}");
        assert!(req.contains("\"thinkingbudget\":0"), "{req}");
    }

    #[tokio::test]
    async fn a_slow_answer_keeps_the_caller_talking() {
        let (base, server) = serve(vec![(200, NORMAL, Duration::from_millis(2500))]).await;
        let started = Instant::now();
        let v = review_at(&Client::new(), &base, "test-key", "對方：你的帳戶被盜用", &hits(), "").await;
        let took = started.elapsed();
        assert_eq!(v, Verdict::ScamOrSales);
        assert!(took >= REVIEW_TIMEOUT && took < Duration::from_millis(2300), "{took:?}");
        server.abort();
    }

    #[tokio::test]
    async fn a_server_error_keeps_the_caller_talking() {
        let (base, server) = serve(vec![(500, r#"{"error":{"code":500}}"#, Duration::ZERO)]).await;
        let v = review_at(&Client::new(), &base, "test-key", "對方：保證獲利", &hits(), "").await;
        assert_eq!(v, Verdict::ScamOrSales);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_model_that_must_think_is_asked_again_without_the_setting() {
        let refused = r#"{"error":{"code":400,"message":"thinking budget is not supported by this model"}}"#;
        let menu = r#"{"candidates":[{"content":{"parts":[{"text":"{\"verdict\":\"ivr_recording\"}"}]}}]}"#;
        let (base, server) = serve(vec![(400, refused, Duration::ZERO), (200, menu, Duration::ZERO)]).await;
        let v = review_at(&Client::new(), &base, "test-key", "對方：查詢帳單請按1", &hits(), "").await;
        assert_eq!(v, Verdict::IvrRecording);
        let seen = server.await.unwrap();
        assert!(seen[0].contains("thinkingBudget"));
        assert!(!seen[1].contains("thinkingBudget"));
    }

    #[tokio::test]
    async fn no_key_keeps_the_caller_talking() {
        let v = review_at(&Client::new(), "http://127.0.0.1:9", " ", "對方：信用卡", &hits(), "").await;
        assert_eq!(v, Verdict::ScamOrSales);
    }

    /// Reviewer: things people say on an ordinary call. A menu verdict hangs up.
    #[test]
    fn reviewer_everyday_sentences_are_not_a_menu() {
        let cases = [
            "按照你說的",
            "請按時吃藥",
            "I'll press the issue",
            "The press release is out",
            "プレスの仕事をしています",
            "按讚",
            "我按了兩次門鈴",
            "我打給你都轉進語音信箱",
            "你手機一直進語音信箱，我留言了",
            "請按一般程序辦理就好",
            "請按三餐飯後吃藥",
            "這個方案請按八折計算給你",
            "你搭電梯按 5 樓就到了",
            "你按 1 號電梯上來",
            "門鈴按2下就好",
            "撳一撳個掣就得",
            "I want to speak to an agent about my policy",
            "Just press one button and it starts",
            "I'll press, um, the doorbell",
            "We need to press on one more time",
            "His pulse 72, blood pressure fine",
            "Marque um horário com o médico",
            "C'est une marque un peu chère",
            "Appuyez sur la sonnette en arrivant",
            "Drücken Sie mir die Daumen",
            "몇 번을 눌러도 안 돼요",
            "กดสองครั้งนะ",
            "Tekan dua kali tombolnya",
        ];
        let hit: Vec<&str> = cases.iter().copied().filter(|t| ivr_cue(t)).collect();
        assert!(hit.is_empty(), "everyday sentences read as a menu ({} of {}): {hit:#?}", hit.len(), cases.len());
    }

    /// Reviewer: the 400-then-retry path is inside the same 1.5 s budget.
    #[tokio::test]
    async fn reviewer_retry_after_400_stays_within_the_timeout() {
        let refused = r#"{"error":{"code":400,"message":"thinking budget is not supported by this model"}}"#;
        let (base, server) = serve(vec![
            (400, refused, Duration::from_millis(1000)),
            (200, NORMAL, Duration::from_millis(1000)),
        ])
        .await;
        let started = Instant::now();
        let v = review_at(&Client::new(), &base, "test-key", "對方：媽，我下午匯款給你", &hits(), "").await;
        let took = started.elapsed();
        assert_eq!(v, Verdict::ScamOrSales);
        assert!(took >= REVIEW_TIMEOUT && took < Duration::from_millis(1900), "{took:?}");
        server.abort();
    }

    /// Reviewer: an unreadable 200 and a server that is not there both keep the stall.
    #[tokio::test]
    async fn reviewer_garbage_and_unreachable_keep_the_caller_talking() {
        let (base, server) = serve(vec![(200, "<html>oops</html>", Duration::ZERO)]).await;
        let v = review_at(&Client::new(), &base, "test-key", "對方：保證獲利", &hits(), "").await;
        assert_eq!(v, Verdict::ScamOrSales);
        server.await.unwrap();
        let v = review_at(&Client::new(), "http://127.0.0.1:9", "test-key", "對方：保證獲利", &hits(), "").await;
        assert_eq!(v, Verdict::ScamOrSales);
    }
}

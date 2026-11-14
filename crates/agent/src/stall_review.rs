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

/// One client for the life of the agent, its connections kept open between
/// reviews: a fresh TLS handshake to Gemini takes about a quarter of a second
/// of the caller's wait.
fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .pool_idle_timeout(Duration::from_secs(600))
            .pool_max_idle_per_host(2)
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| Client::new())
    })
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
    let mut strict = true;
    loop {
        let body = request_body(dialogue, hits, caller_id, strict);
        let resp = http
            .post(&url)
            .header("x-goog-api-key", key)
            .json(&body)
            .send()
            .await
            .context("request")?;
        let status = resp.status();
        let text = resp.text().await.context("response body")?;
        if status.as_u16() == 400 && strict {
            // A setting the model does not take comes back as a bare
            // INVALID_ARGUMENT that does not name it. One more try without the
            // answer schema, inside the same deadline.
            strict = false;
            continue;
        }
        if !status.is_success() {
            bail!("status {status}: {}", head_chars(&text, 80));
        }
        return parse_verdict(&text).with_context(|| {
            let finish = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| v.pointer("/candidates/0/finishReason").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            format!("unreadable answer (finish {finish:?}): {}", head_chars(&text.replace('\n', " "), 120))
        });
    }
}

/// The generateContent request. gemini-flash-lite-latest is a thinking model:
/// on a long, unclear call it thinks before it answers, and the thoughts count
/// against `maxOutputTokens` — a small cap cut the answer off (every review of
/// a real call came back unreadable). Room for both, and the least thinking it
/// takes (`thinkingLevel` minimal; a `thinkingBudget` is refused with a 400).
/// `strict` holds the answer to the three labels; the retry after a 400 leaves
/// that and the thinking setting out and reads the model's text.
fn request_body(dialogue: &str, hits: &[String], caller_id: &str, strict: bool) -> Value {
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
        "maxOutputTokens": 512,
        "responseMimeType": "application/json",
    });
    if strict {
        generation["thinkingConfig"] = json!({"thinkingLevel": "minimal"});
        generation["responseSchema"] = json!({
            "type": "OBJECT",
            "properties": {
                "verdict": {"type": "STRING", "enum": ["scam_or_sales", "ivr_recording", "normal"]}
            },
            "required": ["verdict"]
        });
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

/// A recorded voice menu: at least two key options ("查帳單請按1，轉接專人請按9",
/// "for billing press 1, for support press 2"), or one option that says what
/// it is for ("若要查詢餘額，請按1", "for billing, press 1", "日本語は1を押して").
/// The key has to follow the press word straight away and end there.
///
/// Strict on purpose: a menu verdict hangs up, and people on ordinary calls
/// say 按 5 樓, 語音信箱 or "press one button".
pub fn ivr_cue(text: &str) -> bool {
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let squeezed: Vec<char> = chars.iter().copied().filter(|c| !c.is_whitespace()).collect();
    let mut p = Presses::default();
    for q in [
        han_presses(&chars),
        ja_presses(&chars),
        ko_presses(&squeezed),
        th_presses(&squeezed),
        word_presses(&lower),
    ] {
        p.keys += q.keys;
        p.purposed |= q.purposed;
    }
    p.keys >= 2 || p.purposed
}

/// Key presses found in a text: how many, and whether any says what it is for.
#[derive(Debug, Default, Clone, Copy)]
struct Presses {
    keys: usize,
    purposed: bool,
}

impl Presses {
    fn add(&mut self, purposed: bool) {
        self.keys += 1;
        self.purposed |= purposed;
    }
}

const HAN_NUMERALS: &str = "零〇一二三四五六七八九";

fn is_digit(c: char) -> bool {
    c.is_ascii_digit() || ('０'..='９').contains(&c)
}

fn starts_at(chars: &[char], i: usize, s: &str) -> bool {
    s.chars().enumerate().all(|(k, c)| chars.get(i + k) == Some(&c))
}

/// Length in chars of the first of `list` that starts at `i`.
fn match_any(chars: &[char], i: usize, list: &[&str]) -> Option<usize> {
    list.iter().find(|s| starts_at(chars, i, s)).map(|s| s.chars().count())
}

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

const CLAUSE_SEPS: &str = "，,、；;：:。.！!？?\n";
const SENTENCE_ENDS: &str = "。.！!？?\n";

/// The words before a key press that can say what it is for: its own clause,
/// or the clause before when the press opens one ("若要查詢餘額，請按1"). Never
/// back past `floor` (the previous key) or a sentence end.
fn lead_in(chars: &[char], at: usize, floor: usize) -> String {
    let clause_start = |end: usize| {
        let mut s = end;
        while s > floor && !CLAUSE_SEPS.contains(chars[s - 1]) {
            s -= 1;
        }
        s
    };
    let s = clause_start(at);
    let own: String = chars[s..at].iter().collect();
    if !own.trim().is_empty() || s <= floor || SENTENCE_ENDS.contains(chars[s - 1]) {
        return own;
    }
    let p = clause_start(s - 1);
    chars[p..s - 1].iter().collect()
}

/// Chinese and Cantonese.
const HAN_VERBS: &[&str] = &["請按", "请按", "請撳", "请撳", "按", "撳"];
const HAN_NAMED_KEYS: &[&str] = &[
    "井字鍵", "井字键", "井號鍵", "井号键", "井號", "井号", "米字鍵", "米字键", "星號鍵", "星号键",
    "星號", "星号", "星字鍵", "星字键",
];
const HAN_KEY_SUFFIXES: &[&str] = &["字鍵", "字键", "號鍵", "号键", "鍵", "键", "字", "號", "号"];
/// What a menu option is for: 若要…、查詢…、轉接…、國語…
const HAN_PURPOSES: &[&str] = &[
    "若要", "如要", "如需", "如果要", "欲", "查詢", "查询", "轉接", "转接", "重聽", "重听", "專人", "专人",
    "人工", "客服", "服務", "服务", "國語", "国语", "台語", "台语", "華語", "华语", "英語", "英语", "客語",
    "客语", "粵語", "粤语", "普通話", "普通话", "廣東話", "广东话", "english", "返回", "主選單", "主选单",
    "上一層", "上一层", "結束", "结束", "繳費", "缴费", "帳單", "帐单", "账单", "餘額", "余额", "掛失",
    "挂失", "預約", "预约", "留言",
];

/// The end of a key right after a Chinese press verb: one digit or numeral
/// (with 字／號／鍵 after it), # or *, or a named key such as 井字鍵.
fn han_key(chars: &[char], j: usize) -> Option<usize> {
    if let Some(n) = match_any(chars, j, HAN_NAMED_KEYS) {
        return Some(j + n);
    }
    let c = *chars.get(j)?;
    let numeral = |c: char| is_digit(c) || HAN_NUMERALS.contains(c);
    if !(numeral(c) || matches!(c, '#' | '＃' | '*' | '＊')) {
        return None;
    }
    // One key: 按10% / 按一二 are not.
    if chars.get(j + 1).is_some_and(|&n| numeral(n)) {
        return None;
    }
    Some(j + 1 + match_any(chars, j + 1, HAN_KEY_SUFFIXES).unwrap_or(0))
}

fn han_presses(chars: &[char]) -> Presses {
    let mut p = Presses::default();
    let (mut i, mut floor) = (0, 0);
    while i < chars.len() {
        let Some(v) = match_any(chars, i, HAN_VERBS) else {
            i += 1;
            continue;
        };
        let Some(key_end) = han_key(chars, skip_ws(chars, i + v)) else {
            i += v;
            continue;
        };
        // The key ends at punctuation, the end, or the next option's purpose
        // (按1查詢、國語請按1台語請按2); 按5樓、按2下、請按八折 go on with a word.
        let after = skip_ws(chars, key_end);
        let purpose_after = match_any(chars, after, HAN_PURPOSES).is_some();
        if after == chars.len() || !chars[after].is_alphanumeric() || purpose_after {
            let before = lead_in(chars, i, floor);
            p.add(purpose_after || HAN_PURPOSES.iter().any(|w| before.contains(w)));
            floor = key_end;
        }
        i = key_end;
    }
    p
}

/// Japanese: 1を押して / 1番を押して / シャープを押して, "for" = …は／…場合.
fn ja_presses(chars: &[char]) -> Presses {
    const NAMED: &[&str] = &["シャープ", "米印", "アスタリスク"];
    const LEADS: &[&str] = &["は", "場合", "なら", "方"];
    let mut p = Presses::default();
    let mut floor = 0;
    for b in 0..chars.len() {
        if !starts_at(chars, b, "を押") {
            continue;
        }
        let k = if b > 0 && chars[b - 1] == '番' { b - 1 } else { b };
        let named = NAMED.iter().find_map(|s| {
            let n = s.chars().count();
            (k >= n && starts_at(chars, k - n, s)).then_some(n)
        });
        let key_start = match named {
            Some(n) => k - n,
            None => {
                let numeral = |c: char| is_digit(c) || HAN_NUMERALS.contains(c);
                let Some(&c) = k.checked_sub(1).and_then(|i| chars.get(i)) else { continue };
                if !(numeral(c) || matches!(c, '#' | '＃' | '*' | '＊'))
                    || (k >= 2 && numeral(chars[k - 2]))
                {
                    continue;
                }
                k - 1
            }
        };
        if key_start < floor {
            continue;
        }
        let before = lead_in(chars, key_start, floor);
        p.add(LEADS.iter().any(|l| before.contains(l)));
        floor = b;
    }
    p
}

/// Korean (spaces removed): 0번을 눌러, "for" = …은／는／…면.
fn ko_presses(s: &[char]) -> Presses {
    const PRESS: &[&str] = &["번을눌러", "번을누르", "번눌러", "번누르"];
    const LEADS: &[&str] = &["려면", "원하시면", "경우"];
    let mut p = Presses::default();
    let mut floor = 0;
    for b in 1..s.len() {
        if match_any(s, b, PRESS).is_none() || !is_digit(s[b - 1]) || (b >= 2 && is_digit(s[b - 2])) {
            continue;
        }
        let before = lead_in(s, b - 1, floor);
        let purposed = before.chars().last().is_some_and(|c| matches!(c, '은' | '는' | '면'))
            || LEADS.iter().any(|l| before.contains(l));
        p.add(purposed);
        floor = b;
    }
    p
}

/// Thai (spaces removed): กด 1 / กดหนึ่ง, "for" = หาก／ถ้า／สำหรับ… before or เพื่อ after.
fn th_presses(s: &[char]) -> Presses {
    const NUMERALS: &[&str] = &["ศูนย์", "หนึ่ง", "สอง", "สาม", "สี่", "ห้า", "หก", "เจ็ด", "แปด", "เก้า"];
    // What may follow a key: what it is for, the next option, a polite ending.
    const AFTER: &[&str] = &["เพื่อ", "หาก", "ถ้า", "สำหรับ", "กด", "ค่ะ", "คะ", "ครับ", "หรือ"];
    const LEADS: &[&str] = &["หาก", "ถ้า", "ต้องการ", "สำหรับ", "เพื่อ"];
    let thai_digit = |c: char| is_digit(c) || ('๐'..='๙').contains(&c);
    let mut p = Presses::default();
    let (mut i, mut floor) = (0, 0);
    while i < s.len() {
        if !starts_at(s, i, "กด") {
            i += 1;
            continue;
        }
        let j = i + 2;
        let key_end = if s.get(j).is_some_and(|&c| thai_digit(c)) && !s.get(j + 1).is_some_and(|&c| thai_digit(c)) {
            j + 1
        } else if let Some(n) = match_any(s, j, NUMERALS) {
            j + n
        } else {
            i = j;
            continue;
        };
        // กดสองครั้ง is "press twice".
        if key_end == s.len() || !s[key_end].is_alphanumeric() || match_any(s, key_end, AFTER).is_some() {
            let before: String = s[floor.max(i.saturating_sub(40))..i].iter().collect();
            p.add(starts_at(s, key_end, "เพื่อ") || LEADS.iter().any(|l| before.contains(l)));
            floor = key_end;
        }
        i = key_end;
    }
    p
}

/// Press verbs of the word languages (en, es, fr, de, pt, vi, id).
const PRESS_VERBS: &[&[&str]] = &[
    &["appuyez", "sur"],
    &["drücken", "sie"],
    &["press"],
    &["pulse"],
    &["marque"],
    &["oprima"],
    &["presione"],
    &["pressione"],
    &["tapez"],
    &["digite"],
    &["tecle"],
    &["tekan"],
    &["nhấn"],
    &["bấm"],
];

/// Words that may stand between the verb and the key ("press the pound key").
const KEY_ARTICLES: &[&str] = &["the", "la", "el", "le", "a", "o", "die"];
/// The key's own name before it: "la touche 2", "die Taste 1", "phím 1".
const KEY_NOUNS: &[&str] = &["touche", "taste", "tecla", "phím", "số"];

/// Keys by name: en, es, fr, de, pt, id.
const KEY_WORDS: &[&str] = &[
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "pound", "star", "hash",
    "cero", "uno", "dos", "tres", "cuatro", "cinco", "seis", "siete", "ocho", "nueve", "asterisco",
    "zéro", "deux", "trois", "quatre", "cinq", "sept", "huit", "neuf", "dièse", "étoile",
    "null", "eins", "zwei", "drei", "vier", "fünf", "sechs", "sieben", "acht", "neun",
    "dois", "três", "quatro", "sete", "oito", "nove",
    "nol", "satu", "dua", "tiga", "empat", "lima", "enam", "tujuh", "delapan", "sembilan",
];

/// What an option is for: "for billing, press 1", "press 1 to …", "para …".
const PURPOSE_WORDS: &[&str] = &[
    "for", "to", "if", "para", "pour", "si", "se", "für", "um", "wenn", "falls", "untuk", "jika", "kalau",
    "nếu", "để",
];

/// Words a clause may open with before its purpose or verb.
const FILLERS: &[&str] = &[
    "and", "or", "y", "o", "et", "ou", "und", "oder", "e", "atau", "dan", "và", "hoặc", "please", "bitte",
    "silakan", "veuillez", "vui", "lòng", "por", "favor",
];

/// Whole words, so "pressed" is not "press". `#` and `*` are words of their
/// own; commas and colons become "," and sentence ends ".".
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
        match c {
            '#' | '*' => out.push(c.to_string()),
            ',' | ':' => out.push(",".to_string()),
            '.' | ';' | '!' | '?' | '¿' | '¡' => out.push(".".to_string()),
            _ => {}
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn clause_start(ws: &[String], at: usize) -> usize {
    let mut s = at;
    while s > 0 && ws[s - 1] != "," && ws[s - 1] != "." {
        s -= 1;
    }
    s
}

fn skip_fillers(ws: &[String], mut k: usize, end: usize) -> usize {
    while k < end && FILLERS.contains(&ws[k].as_str()) {
        k += 1;
    }
    k
}

/// The clause with the verb opens with a purpose ("for billing press 1"), or
/// the verb opens its clause and the one before it, after a comma, does
/// ("for billing, press 1").
fn purpose_before(ws: &[String], verb: usize) -> bool {
    let is_purpose = |w: &String| PURPOSE_WORDS.contains(&w.as_str());
    let start = clause_start(ws, verb);
    let first = skip_fillers(ws, start, verb);
    if first < verb {
        return is_purpose(&ws[first]);
    }
    if start == 0 || ws[start - 1] != "," {
        return false;
    }
    let prev = clause_start(ws, start - 1);
    let first = skip_fillers(ws, prev, start - 1);
    first < start - 1 && is_purpose(&ws[first])
}

fn word_presses(lower: &str) -> Presses {
    let ws = tokens(lower);
    let mut p = Presses::default();
    let mut i = 0;
    while i < ws.len() {
        let Some(verb) = PRESS_VERBS
            .iter()
            .find(|v| ws.len() - i >= v.len() && v.iter().zip(&ws[i..]).all(|(a, b)| a == b))
        else {
            i += 1;
            continue;
        };
        let mut j = i + verb.len();
        if ws.get(j).is_some_and(|a| KEY_ARTICLES.contains(&a.as_str())) {
            j += 1;
        }
        if ws.get(j).is_some_and(|a| KEY_NOUNS.contains(&a.as_str())) {
            j += 1;
        }
        let is_key = ws.get(j).is_some_and(|k| {
            (k.len() == 1 && k.chars().all(|c| c.is_ascii_digit()))
                || k == "#"
                || k == "*"
                || KEY_WORDS.contains(&k.as_str())
        });
        if !is_key {
            i += verb.len();
            continue;
        }
        let purpose_after = ws.get(j + 1).is_some_and(|a| PURPOSE_WORDS.contains(&a.as_str()));
        p.add(purpose_after || purpose_before(&ws, i));
        i = j + 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn menus_in_every_language() {
        for text in [
            "查詢帳單請按1，重聽請按九",
            "您好，轉接專人請按 9",
            "若要查詢餘額，請按1",
            "國語請按1台語請按2",
            "請按井字鍵結束",
            "按 1 查詢餘額",
            "普通話服務請按二",
            "广东话服务请按三，重听请按零",
            "粵語服務請撳一字",
            "For billing, press 1.",
            "Press one for sales.",
            "Press pound to repeat this menu.",
            "For billing press 1, for support press 2",
            "Para español, oprima dos.",
            "Pulse 1 para ventas",
            "Para soporte, marque uno",
            "お問い合わせは1番を押してください",
            "日本語は１を押してください",
            "상담원 연결은 0번을 눌러 주세요",
            "Pour le service client, tapez 1",
            "Pour la facturation, appuyez sur la touche 2",
            "Für Deutsch drücken Sie die Eins",
            "Para vendas, pressione 1",
            "Para suporte, digite 1",
            "Para vendas, tecle 2",
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
            "我要轉接，門鈴按2",
            "I pressed one key and it broke",
            "She was pressing 1 again",
            "The press release is out",
            "Please press 1",
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
    fn the_request_holds_the_answer_to_three_labels_and_keeps_the_end_of_a_long_call() {
        let long = format!("對方：{}\n對方：請你去 ATM 解除分期", "喂".repeat(4000));
        let hits = vec!["ATM".to_string(), "解除分期".to_string()];
        let body = request_body(&long, &hits, "+12025550143", true);
        let g = &body["generationConfig"];
        assert_eq!(g["temperature"], 0);
        assert_eq!(g["responseMimeType"], "application/json");
        assert_eq!(g["responseSchema"]["properties"]["verdict"]["enum"][2], "normal");
        assert_eq!(g["thinkingConfig"]["thinkingLevel"], "minimal", "{g}");
        assert!(g.get("thinkingConfig").and_then(|t| t.get("thinkingBudget")).is_none(), "{g}");
        assert!(g["maxOutputTokens"].as_u64().unwrap() >= 256, "{g}");
        let user = body["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert!(user.contains("Caller: 請你去 ATM 解除分期"), "{user}");
        assert!(user.contains("Keywords that matched: ATM, 解除分期"), "{user}");
        assert!(user.contains("Number: +12025550143"), "{user}");
        assert!(user.chars().count() < MAX_DIALOGUE_CHARS + 200);
        let plain = request_body("對方：你好", &[], "", false);
        assert!(plain["generationConfig"].get("responseSchema").is_none());
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
    /// What the API answers a setting the model does not take.
    const INVALID_ARGUMENT: &str =
        r#"{"error":{"code":400,"message":"Request contains an invalid argument.","status":"INVALID_ARGUMENT"}}"#;

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
        assert!(req.contains("\"responseschema\""), "{req}");
        assert!(req.contains("\"thinkinglevel\":\"minimal\""), "{req}");
        assert!(!req.contains("thinkingbudget"), "{req}");
    }

    #[test]
    fn the_shared_client_is_built_once() {
        assert!(std::ptr::eq(client(), client()));
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
    async fn a_refused_request_is_asked_again_without_the_schema() {
        let menu = r#"{"candidates":[{"content":{"parts":[{"text":"{\"verdict\":\"ivr_recording\"}"}]}}]}"#;
        let (base, server) = serve(vec![(400, INVALID_ARGUMENT, Duration::ZERO), (200, menu, Duration::ZERO)]).await;
        let v = review_at(&Client::new(), &base, "test-key", "對方：查詢帳單請按1", &hits(), "").await;
        assert_eq!(v, Verdict::IvrRecording);
        let seen = server.await.unwrap();
        assert!(seen[0].contains("responseSchema"));
        assert!(!seen[1].contains("responseSchema"));
        assert!(!seen[1].contains("thinkingConfig"));
    }

    #[tokio::test]
    async fn a_second_refusal_is_not_asked_again() {
        let (base, server) = serve(vec![
            (400, INVALID_ARGUMENT, Duration::ZERO),
            (400, INVALID_ARGUMENT, Duration::ZERO),
        ])
        .await;
        let v = review_at(&Client::new(), &base, "test-key", "對方：保證獲利", &hits(), "").await;
        assert_eq!(v, Verdict::ScamOrSales);
        assert_eq!(server.await.unwrap().len(), 2);
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

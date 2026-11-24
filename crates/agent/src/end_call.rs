// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The AI ends a call itself, and a short nudge when it leaves a caller's turn
//! unanswered.
//!
//! - **end_call(reason)**: a Live function, declared on answered persona calls
//!   (`NON_BLOCKING` when the API takes it). The model calls it once the
//!   business is done and the caller is ready to finish, after its own closing
//!   line. The code, not the model, decides whether it counts ([`judge`]): never
//!   while stalling a sales / scam caller, never in the first seconds of the call,
//!   never right after a question of the caller's the AI has not answered. A
//!   refusal is answered so the model knows the call goes on.
//! - Once accepted the line is not cut at once ([`step`]): our last words play
//!   out, then [`GRACE`] of quiet when the caller has already shown they are done
//!   (their goodbye, their own wrap-up or thanks for a settled matter), or
//!   [`QUIET_CLOSE`] when they have not: a caller usually answers our goodbye
//!   with theirs, and that is the moment to close ([`BYE_SETTLE`] after it, so
//!   words they add are heard first). The caller saying anything else in that
//!   time cancels it and the call goes on. Our last words asking the caller
//!   something ("anything else?") withdraw it too: their answer comes first.
//! - **Cold gap** ([`ColdGap`]): the caller finished a turn and the AI has said
//!   nothing for [`COLD_GAP`]: one light nudge per caller turn to pick it up in
//!   the caller's language and the call's talk style. The 15 s quiet-line nudge
//!   stays for a line where nobody speaks; stalling has its own.
//!
//! The closing state machine ([`crate::closing`]) stays as the fallback: a caller
//! who says goodbye still ends the call when the AI forgets end_call.

use serde_json::{json, Value};
use std::time::Duration;

// The words are in the prompt book (data/prompts/<language>.json); these are their handles.
crate::prompt_book::prompt_texts! {
    EN_END_CALL_DECLARATION_1 = "en/end_call_declaration_1";
    EN_END_CALL_DECLARATION_2 = "en/end_call_declaration_2";
    EN_END_CALL_NEXT_1 = "en/end_call_next_1";
    EN_END_CALL_NEXT_2 = "en/end_call_next_2";
    EN_END_CALL_NEXT_3 = "en/end_call_next_3";
}

/// The function's name in the Live setup.
pub const NAME: &str = "end_call";
/// No end_call this soon after the call was answered (the AI's first words).
pub const MIN_TALK: Duration = Duration::from_secs(8);
/// Quiet after our last words before the line is closed.
pub const GRACE: Duration = Duration::from_millis(1500);
/// Quiet after our last words before the line is closed when the caller has not
/// shown they are done (no goodbye, wrap-up or thanks yet): the same wait as for
/// a goodbye of the AI's left unanswered.
pub const QUIET_CLOSE: Duration = Duration::from_secs(6);
/// Quiet after the caller's goodbye before the line is closed: their words can
/// reach us a moment after they stop ("bye… oh, one more thing").
pub const BYE_SETTLE: Duration = Duration::from_millis(2500);
/// The model called end_call before its closing line: wait this long for it.
pub const LINE_WAIT: Duration = Duration::from_secs(6);
/// The caller's turn ended and the AI has said nothing for this long: nudge.
pub const COLD_GAP: Duration = Duration::from_millis(2500);
/// …even when the model has not closed its turn (it may still be thinking
/// before that; after this it is a silence).
pub const COLD_GAP_FORCE: Duration = Duration::from_secs(4);

/// The reasons the model may give (anything else is logged as "other").
pub const REASONS: &[&str] = &["matter_done", "caller_finishing", "other"];

/// The function declaration. `non_blocking`: the model goes on speaking while
/// it is answered (its closing line is not held up).
pub fn declaration(non_blocking: bool) -> Value {
    let mut d = json!({
        "name": NAME,
        "description": EN_END_CALL_DECLARATION_1.get(),
        "parameters": {
            "type": "OBJECT",
            "properties": {
                "reason": {
                    "type": "STRING",
                    "description": EN_END_CALL_DECLARATION_2.get()
                }
            },
            "required": ["reason"]
        }
    });
    if non_blocking {
        d["behavior"] = json!("NON_BLOCKING");
    }
    d
}

/// Adds end_call to the setup's functions: after the ones already there (the
/// appointments'), or as the only one.
pub fn add_to_setup(setup: &mut Value, non_blocking: bool) {
    let Some(s) = setup.get_mut("setup").and_then(|x| x.as_object_mut()) else {
        return;
    };
    let tools = s.entry("tools").or_insert_with(|| json!([]));
    if !tools.is_array() {
        *tools = json!([]);
    }
    let list = tools.as_array_mut().expect("tools is an array");
    let decl = declaration(non_blocking);
    if let Some(fns) = list
        .iter_mut()
        .find_map(|t| t.get_mut("functionDeclarations").and_then(Value::as_array_mut))
    {
        fns.retain(|f| f.get("name").and_then(Value::as_str) != Some(NAME));
        fns.push(decl);
    } else {
        list.push(json!({ "functionDeclarations": [decl] }));
    }
}

/// The reason as it goes into the log: one of [`REASONS`], never the model's own words.
pub fn reason_label(args: &Value) -> &'static str {
    let r = args.get("reason").and_then(Value::as_str).unwrap_or("").trim().to_ascii_lowercase();
    REASONS.iter().copied().find(|k| *k == r).unwrap_or("other")
}

/// Why an end_call does not count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Stalling a sales / scam caller: the AI never lets them go.
    Stalling,
    /// The call has only just been answered.
    TooEarly,
    /// The caller's last words ask something the AI has not answered.
    UnansweredQuestion,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::Stalling => "stalling",
            Refusal::TooEarly => "too_early",
            Refusal::UnansweredQuestion => "unanswered_question",
        }
    }

    /// What the model is told to do instead (it reads the answer, the caller does not).
    fn next(self) -> &'static str {
        match self {
            Refusal::Stalling => EN_END_CALL_NEXT_1.get(),
            Refusal::TooEarly => EN_END_CALL_NEXT_2.get(),
            Refusal::UnansweredQuestion => EN_END_CALL_NEXT_3.get(),
        }
    }
}

/// Where the call stands when the model calls end_call.
#[derive(Debug, Clone, Copy)]
pub struct Check<'a> {
    /// Stalling a sales / scam caller (or on the stalling session).
    pub stalling: bool,
    /// Since the call was answered (the AI's first audio); None = not yet.
    pub since_answer: Option<Duration>,
    /// What the caller has said since the AI last started speaking.
    pub caller_turn: &'a str,
    /// The AI has spoken since the caller's last turn ended.
    pub ai_spoke_since_caller: bool,
}

/// Whether an end_call counts (the guard rails the model does not decide).
pub fn judge(c: &Check) -> Result<(), Refusal> {
    if c.stalling {
        return Err(Refusal::Stalling);
    }
    if c.since_answer.map_or(true, |d| d < MIN_TALK) {
        return Err(Refusal::TooEarly);
    }
    if !c.ai_spoke_since_caller && ends_on_question(c.caller_turn) {
        return Err(Refusal::UnansweredQuestion);
    }
    Ok(())
}

/// The answer to one end_call. Accepted: silent (the model has said its line).
/// Refused: spoken when idle, so the model carries on.
pub fn response(id: &str, verdict: Result<(), Refusal>) -> Value {
    let response = match verdict {
        Ok(()) => json!({ "status": "ending", "scheduling": "SILENT" }),
        Err(r) => json!({
            "status": "not_ended",
            "reason": r.as_str(),
            "next": r.next(),
            "scheduling": "WHEN_IDLE"
        }),
    };
    json!({
        "toolResponse": {
            "functionResponses": [{ "id": id, "name": NAME, "response": response }]
        }
    })
}

/// An accepted end_call waiting for our last words and the quiet after them.
#[derive(Debug, Clone, Copy)]
pub struct Pending {
    pub accepted_at: tokio::time::Instant,
    /// Model audio chunks sent when it was accepted.
    pub audio_at_accept: u64,
    /// Caller turns ended when it was accepted.
    pub eou_at_accept: u64,
    /// The AI had already spoken since the caller's last turn (its closing line).
    pub spoke_before: bool,
    pub reason: &'static str,
}

/// One look at an accepted end_call.
#[derive(Debug, Clone, Copy)]
pub struct PendingView {
    pub since_accept: Duration,
    /// The caller started speaking again after it was accepted.
    pub caller_spoke: bool,
    /// The caller is speaking now.
    pub caller_talking: bool,
    /// The caller has been quiet [`BYE_SETTLE`] (or never spoke).
    pub caller_settled: bool,
    /// The model is still producing or our audio is still playing.
    pub ai_busy: bool,
    /// The AI has said its closing line (before or after the call).
    pub ai_spoke: bool,
    /// The AI's words since the caller's last turn end on a question.
    pub ai_asked: bool,
    /// Our audio has been played out for [`GRACE`].
    pub quiet_for_grace: bool,
    /// Our audio has been played out for [`QUIET_CLOSE`].
    pub quiet_for_close: bool,
    /// The caller's latest words show they are done: their goodbye, their own
    /// wrap-up, or thanks for a settled matter ([`crate::closing::ClosingMachine::caller_done`]),
    /// or words since it was accepted that bring nothing new ([`caller_done`]).
    pub caller_done: bool,
    /// The call is being stalled (a sales or scam call). The verdict can come
    /// after the end_call was accepted: such a call is never closed by it.
    pub stalling: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wait,
    /// The caller spoke again: the call goes on.
    Cancel,
    /// Our last words ask the caller something: wait for the answer.
    Withdraw,
    /// The call turned out to be one to stall: the line stays open.
    Stalled,
    HangUp,
}

pub fn step(v: &PendingView) -> Step {
    if v.stalling {
        Step::Stalled
    } else if v.caller_spoke && !v.caller_talking && !v.caller_done && v.caller_settled {
        // They said something other than goodbye (their words have had time to
        // come in): the call goes on.
        Step::Cancel
    } else if v.caller_talking || v.ai_busy || (!v.ai_spoke && v.since_accept < LINE_WAIT) {
        Step::Wait
    } else if v.ai_asked {
        Step::Withdraw
    } else if (v.caller_done && v.quiet_for_grace && v.caller_settled) || v.quiet_for_close {
        Step::HangUp
    } else {
        Step::Wait
    }
}

/// [`PendingView::caller_done`]: the closing says the caller is done; or their
/// latest words bring nothing new — thanks, an okay, a goodbye ("Muchas gracias",
/// 「감사합니다」) — and were said after the end_call was accepted
/// (`spoke_after_accept`) or closed the call themselves (their thanks for a
/// settled matter, then the AI's closing line with its end_call): no reason to
/// go on, the line closes after their words.
pub fn caller_done(closing: &crate::closing::ClosingMachine, spoke_after_accept: bool) -> bool {
    closing.caller_done() || (closing.caller_nothing_new() && (spoke_after_accept || closing.caller_closing()))
}

/// One look at the line for the cold-gap nudge.
#[derive(Debug, Clone, Copy)]
pub struct ColdView {
    /// An answered persona call (not an errand, not a call-back).
    pub on: bool,
    pub stalling: bool,
    /// Anything else owns the next turn or the line: the caller talking, the AI
    /// speaking or generating, a closing, a function call, a hold, a recording…
    pub busy: bool,
    /// Caller turns ended so far.
    pub eou_n: u64,
    /// Since the caller's last turn ended.
    pub since_eou: Option<Duration>,
    pub ai_spoke_since_eou: bool,
    /// The model closed a turn since then (it chose to say nothing).
    pub model_done_since_eou: bool,
}

/// One cold-gap nudge per caller turn.
#[derive(Debug, Default, Clone)]
pub struct ColdGap {
    nudged_for: Option<u64>,
}

impl ColdGap {
    pub fn due(&self, v: &ColdView) -> bool {
        v.on
            && !v.stalling
            && !v.busy
            && v.eou_n > 0
            && self.nudged_for != Some(v.eou_n)
            && !v.ai_spoke_since_eou
            && v
                .since_eou
                .is_some_and(|d| d >= COLD_GAP && (v.model_done_since_eou || d >= COLD_GAP_FORCE))
    }

    pub fn sent(&mut self, eou_n: u64) {
        self.nudged_for = Some(eou_n);
    }
}

/// Question words that may stand anywhere in a sentence (scripts written
/// without spaces, and languages that put them in place): Chinese, Japanese,
/// Korean, Thai, Vietnamese.
const ASK_ANYWHERE: &[&str] = &[
    "幾點", "几点", "什麼", "什么", "哪裡", "哪里", "哪天", "哪個", "哪个", "多少", "怎麼", "怎么", "為什麼", "为什么",
    "是不是", "有沒有", "有没有", "可不可以", "能不能", "要不要", "對不對", "对不对", "好不好", "誰", "谁",
    "どこ", "なぜ", "何時", "どうして", "いくら",
    "언제", "어디", "몇", "뭐", "왜", "무엇", "어떻게",
    "ไหม", "มั้ย", "หรือเปล่า", "หรือยัง", "อะไร", "ยังไง", "อย่างไร", "ใคร", "ทำไม", "ที่ไหน", "เมื่อไหร่", "กี่", "เท่าไหร่",
    "bao giờ", "mấy giờ", "ở đâu", "tại sao", "thế nào", "bao nhiêu",
];

/// Phrases that hold a question word but say "nothing" / "never mind": taken
/// out before looking (「沒什麼事了」, "ไม่มีอะไรแล้ว").
const NOT_ASKING: &[&str] = &[
    "沒什麼", "没什么", "沒有什麼", "没有什么", "沒有沒有", "没有没有", "不怎麼", "不怎么", "ไม่มีอะไร", "ไม่เป็นไร",
];

/// Endings that make a question in scripts without spaces: Chinese, Japanese, Korean.
const ASK_ENDINGS: &[&str] = &[
    "嗎", "吗", "ですか", "ますか", "でしょうか", "か", "까요", "나요", "습니까", "니까", "인가요", "어때요",
    // -세요 is also the polite command ("하세요", "계세요", "되세요"): only these stems ask.
    "괜찮으세요", "괜찮으신가요", "있으세요", "없으세요", "맞으세요", "편하세요", "가능하세요",
];

/// Words that end a question in Vietnamese ("…có ở nhà không").
const ASK_LAST_WORDS: &[&str] = &["không", "chưa", "nhỉ"];

/// Words a question opens with in languages written with spaces.
const ASK_OPENERS: &[&str] = &[
    // English
    "what", "when", "where", "which", "who", "whose", "why", "how", "can you", "could you", "can i", "could i",
    "may i", "is it", "is there", "is that", "are you", "are there", "do you", "does", "did you", "will you",
    "would you", "should i", "have you",
    // Spanish
    "qué", "cuándo", "dónde", "cómo", "cuál", "cuáles", "cuánto", "cuánta", "quién", "por qué", "puede", "puedes",
    "podría",
    // German
    "wann", "wo", "wie", "was", "welche", "welcher", "welches", "wer", "warum", "wieso", "können sie", "kannst du",
    "haben sie", "hast du", "ist das", "gibt es", "sind sie",
    // French
    "quand", "où", "comment", "quel", "quelle", "pourquoi", "qui", "est-ce que", "pouvez-vous",
    // Portuguese, Italian
    "quando", "onde", "qual", "quem", "por que", "dove", "quale", "chi", "perché", "può", "puoi",
    // Arabic
    "هل", "وهل", "ماذا", "لماذا", "متى", "ومتى", "أين", "كيف", "وكيف", "كم", "أي",
    // Russian
    "когда", "где", "сколько", "почему", "как", "что", "кто", "какой", "можно",
    // Hindi
    "क्या", "कब", "कहाँ", "कितना", "कैसे", "क्यों", "कौन",
    // Indonesian
    "apa", "apakah", "kapan", "di mana", "berapa", "bagaimana", "kenapa", "mengapa", "siapa",
    // Turkish
    "ne zaman", "nerede", "nasıl", "neden", "kaç",
];

/// Hindi and Indonesian put the question word anywhere too ("आप कब घर पर होंगे").
const ASK_WORDS_ANYWHERE: &[&str] = &["कब", "कहाँ", "कितना", "कैसे", "क्यों", "कौन", "kapan", "berapa", "di mana", "siapa"];

/// Question particles that are words of their own, at the end of a sentence (Turkish).
const ASK_PARTICLES: &[&str] = &["mi", "mı", "mu", "mü", "misiniz", "mısınız", "musunuz", "müsünüz"];

/// "Anything else?" in Spanish and Portuguese, asked without a mark.
const ASK_ANYTHING_ELSE: &[&str] = &[
    "algo más", "algo mas", "alguna otra cosa", "alguna cosa más", "mais alguma", "alguma coisa mais",
    "alguma coisa a mais", "algo mais", "alguma outra",
];

/// Words that make "algo más" a statement or a condition ("si necesita algo más, llámenos").
const NOT_ASKING_ELSE: &[&str] = &["si", "se", "no", "não", "nada", "nunca", "nem", "cuando", "quando"];

/// A first word that does not change what the sentence is ("and when…", "y dónde…").
const LEAD_FILLERS: &[&str] = &[
    "and", "so", "but", "oh", "um", "also", "y", "pero", "und", "aber", "et", "mais", "e", "ma", "و", "а", "и", "और",
    "ve", "dan",
];

/// Words end on a question (the caller's turn, or the AI's last words): a
/// question mark, or a last sentence that asks (a question word or ending in
/// its language). Earlier sentences do not count: "what time? ok never mind,
/// thanks" asks nothing any more.
pub fn ends_on_question(turn: &str) -> bool {
    let t = turn
        .trim()
        .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '…' | '"' | '」' | '』' | ')' | '）'));
    if t.is_empty() {
        return false;
    }
    if t.ends_with(['?', '？', '؟']) {
        return true;
    }
    let last = t
        .split(|c: char| matches!(c, '.' | '。' | '!' | '！' | '\n' | '।' | '?' | '？' | '؟'))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .last()
        .unwrap_or(t);
    if last.contains('¿') {
        return true;
    }
    // Thai writes no full stops: a space ends a sentence.
    let last = if last.chars().any(|c| ('\u{0E00}'..='\u{0E7F}').contains(&c)) {
        last.split_whitespace().last().unwrap_or(last)
    } else {
        last
    };
    let mut lower = last.to_lowercase();
    for n in NOT_ASKING {
        lower = lower.replace(n, " ");
    }
    if ASK_ANYWHERE.iter().any(|w| lower.contains(w)) {
        return true;
    }
    let words: Vec<&str> = lower
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | '、' | ';' | ':' | '"' | '¡' | '«' | '»' | '،'))
        .filter(|w| !w.is_empty())
        .collect();
    let Some(last_word) = words.last() else {
        return false;
    };
    // Chinese / Japanese / Korean endings; a lone か only after kana or kanji.
    let core = lower
        .trim_end_matches(|c: char| !c.is_alphanumeric())
        .trim_end_matches(|c: char| "喔哦囉啰啦呢吧耶齁欸啊呀".contains(c));
    if ASK_ENDINGS
        .iter()
        .any(|e| core.ends_with(e) && (*e != "か" || core.chars().rev().nth(1).is_some_and(|c| !c.is_ascii())))
    {
        return true;
    }
    if ASK_LAST_WORDS.contains(last_word) || ASK_PARTICLES.contains(last_word) {
        return true;
    }
    // Only the last clause asks: "Wann immer Sie wollen, rufen Sie an" and
    // "Kapan saja boleh, terima kasih" open with a question word and ask nothing.
    let last_clause = lower
        .split([',', '，', '、', ';', '،'])
        .map(|c| c.trim_matches(|ch: char| !ch.is_alphanumeric() && !ch.is_whitespace()))
        .filter(|c| !c.is_empty())
        .last()
        .unwrap_or(&lower);
    let clause_words: Vec<&str> = last_clause.split_whitespace().collect();
    if ASK_ANYTHING_ELSE.iter().any(|p| last_clause.contains(p)) && !clause_words.iter().any(|w| NOT_ASKING_ELSE.contains(w)) {
        return true;
    }
    let spaced = format!(" {} ", clause_words.join(" "));
    if ASK_WORDS_ANYWHERE.iter().any(|w| spaced.contains(&format!(" {w} "))) {
        return true;
    }
    // The last clause ("Perfecto, cuándo le viene bien") opens with a question word,
    // unless it is an exclamation ("What a nice chat").
    let start = if clause_words.len() > 1 && LEAD_FILLERS.contains(&clause_words[0]) { 1 } else { 0 };
    let head = clause_words.get(start..).map(|h| h.join(" ")).unwrap_or_default();
    if head.starts_with("what a ") || head.starts_with("what an ") {
        return false;
    }
    !head.is_empty() && ASK_OPENERS.iter().any(|o| head == *o || head.starts_with(&format!("{o} ")))
}

/// Words of the AI's that ask the caller to wait now ("one moment", 「稍等」,
/// "Chị đợi em một tí nhé"): it cannot look anything up on a call, and the caller
/// is left waiting for nothing. Checking later and getting back to them is fine
/// and not in here. Whole words, in the languages written with spaces.
const WAIT_WORDS: &[&str] = &[
    // English
    "hold on", "hang on", "one moment", "just a moment", "one sec", "just a sec", "just a second", "give me a moment",
    "give me a second", "give me a sec", "bear with me", "wait a moment", "wait a second", "please hold",
    // Spanish
    "un momento", "un momentito", "un segundito", "espera un", "espere un", "espérame", "espéreme",
    // French
    "un instant", "un petit instant", "une petite seconde", "attendez un", "attends un", "patientez", "ne quittez pas",
    // German
    "moment mal", "einen moment", "einen augenblick", "warte kurz", "warten sie kurz", "bleiben sie dran", "bleib dran",
    // Portuguese
    "um momento", "só um momento", "um minutinho", "espera aí", "espere um", "aguarde",
    // Italian
    "attenda", "aspetti un", "aspetta un", "solo un attimo", "un attimo solo", "un attimo per favore", "un attimino",
    // Vietnamese
    "đợi một chút", "đợi một tí", "đợi chút", "đợi tí", "đợi em một", "đợi tôi một", "chờ một chút", "chờ một tí", "chờ chút",
    "chờ tí", "chờ em một", "chờ tôi một", "giữ máy",
    // Indonesian
    "tunggu sebentar", "sebentar ya", "mohon tunggu",
    // Russian
    "минутку", "одну минуту", "секунду", "одну секунду", "подождите", "подожди",
    // Turkish
    "bir dakika", "bir saniye", "bekleyin",
    // Dutch
    "momentje", "een moment", "wacht even",
    // Polish
    "chwileczkę", "proszę poczekać", "poczekaj",
    // Hindi
    "एक मिनट", "रुकिए", "ज़रा रुकिए",
    // Arabic
    "لحظة", "انتظر", "انتظري",
];

/// The same in Chinese, Japanese, Korean and Thai, anywhere in the words.
const WAIT_ANYWHERE: &[&str] = &[
    // Chinese (folded to simplified)
    "稍等", "等我一下", "你等一下", "您等一下", "先等一下", "我查一下", "我帮你查", "让我查",
    // Japanese
    "少々お待ち", "お待ちください", "ちょっと待って", "お調べします",
    // Korean
    "잠깐만", "잠시만", "기다려",
    // Thai
    "รอสักครู่", "รอแป๊บ", "รอเดี๋ยว",
];

/// The AI's words ask the caller to wait or say it is looking something up
/// ([`WAIT_WORDS`], [`WAIT_ANYWHERE`]); both sides folded alike (case, accents,
/// punctuation, one Chinese script).
pub fn asks_to_wait(said: &str) -> bool {
    static FOLDED: std::sync::OnceLock<(Vec<String>, Vec<String>)> = std::sync::OnceLock::new();
    let (words, anywhere) = FOLDED.get_or_init(|| {
        let words = WAIT_WORDS
            .iter()
            .map(|w| crate::urgency::fold_spaced(w))
            .filter(|w| !w.trim().is_empty())
            .collect();
        let anywhere = WAIT_ANYWHERE
            .iter()
            .map(|w| crate::urgency::fold_compact(w))
            .filter(|w| !w.is_empty())
            .collect();
        (words, anywhere)
    });
    if said.trim().is_empty() {
        return false;
    }
    let spaced = crate::urgency::fold_spaced(said);
    if words.iter().any(|w| spaced.contains(w.as_str())) {
        return true;
    }
    let compact = crate::urgency::fold_compact(said);
    anywhere.iter().any(|w| compact.contains(w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn check(turn: &str) -> Check<'_> {
        Check { stalling: false, since_answer: Some(30 * S), caller_turn: turn, ai_spoke_since_caller: false }
    }

    #[test]
    fn the_declaration_is_one_function_with_a_reason() {
        let d = declaration(false);
        assert_eq!(d["name"], NAME);
        assert_eq!(d["parameters"]["type"], "OBJECT");
        assert_eq!(d["parameters"]["properties"]["reason"]["type"], "STRING");
        assert_eq!(d["parameters"]["required"], json!(["reason"]));
        assert!(d.get("behavior").is_none());
        assert_eq!(declaration(true)["behavior"], "NON_BLOCKING");
        // Only the keys a Live function declaration takes.
        let nb = declaration(true);
        let keys: Vec<&str> = nb.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 4, "{keys:?}");
        for k in keys {
            assert!(["name", "description", "parameters", "behavior"].contains(&k), "{k}");
        }
    }

    #[test]
    fn added_after_the_appointments_or_alone() {
        let mut plain = json!({"setup": {"model": "m"}});
        add_to_setup(&mut plain, true);
        assert_eq!(plain.pointer("/setup/tools/0/functionDeclarations/0/name"), Some(&json!(NAME)));
        assert_eq!(plain.pointer("/setup/tools/0/functionDeclarations/0/behavior"), Some(&json!("NON_BLOCKING")));
        let mut appt = json!({"setup": {"tools": crate::appointments::tool_declarations(false)}});
        add_to_setup(&mut appt, false);
        add_to_setup(&mut appt, false);
        let names: Vec<&str> = appt
            .pointer("/setup/tools/0/functionDeclarations")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .filter_map(|f| f["name"].as_str())
            .collect();
        assert_eq!(names, vec!["check_availability", "propose_appointment", NAME]);
        assert!(!appt.to_string().contains("NON_BLOCKING"));
    }

    #[test]
    fn the_reason_is_logged_only_from_the_known_set() {
        assert_eq!(reason_label(&json!({"reason": "matter_done"})), "matter_done");
        assert_eq!(reason_label(&json!({"reason": " Caller_Finishing "})), "caller_finishing");
        assert_eq!(reason_label(&json!({"reason": "王先生說他要去繳費"})), "other");
        assert_eq!(reason_label(&json!({})), "other");
    }

    #[test]
    fn stalling_refuses_end_call() {
        let mut c = check("OK, thanks.");
        c.stalling = true;
        assert_eq!(judge(&c), Err(Refusal::Stalling));
        // Even a caller who clearly wants to go.
        for bye in ["好，那就這樣，掰掰", "Okay, bye bye", "じゃあ失礼します", "Vale, adiós", "Tschüss", "โอเค บาย", "مع السلامة"] {
            let mut c = check(bye);
            c.stalling = true;
            c.ai_spoke_since_caller = true;
            assert_eq!(judge(&c), Err(Refusal::Stalling), "{bye}");
        }
    }

    #[test]
    fn too_early_after_pickup_refuses_end_call() {
        let mut c = check("好");
        c.since_answer = None;
        assert_eq!(judge(&c), Err(Refusal::TooEarly));
        c.since_answer = Some(5 * S);
        assert_eq!(judge(&c), Err(Refusal::TooEarly));
        c.since_answer = Some(MIN_TALK);
        assert_eq!(judge(&c), Ok(()));
    }

    /// The caller's last words ask something, in many languages.
    const QUESTIONS: &[(&str, &str)] = &[
        ("zh", "好，那你明天幾點在家"),
        ("zh", "包裹可以放管理室嗎"),
        ("zh-cn", "那我什么时候再打给你"),
        ("en", "Okay. And what time will you be home"),
        ("en", "Could you call me back tomorrow"),
        ("ja", "では、明日の午後はご在宅ですか"),
        ("ja", "何時ごろがよろしいでしょうか"),
        ("es", "Vale. ¿Y a qué hora estará en casa"),
        ("es", "Perfecto, cuándo le viene bien"),
        ("de", "Alles klar. Wann sind Sie morgen zu Hause"),
        ("de", "Können Sie mich zurückrufen"),
        ("th", "โอเคครับ พรุ่งนี้บ่ายอยู่บ้านไหมครับ"),
        ("th", "จะให้วางของไว้ที่ไหนครับ"),
        ("ar", "حسناً. متى تكون في البيت غداً"),
        ("ar", "هل يمكنني ترك الطرد عند الحارس"),
        ("ko", "내일 몇 시에 집에 계세요"),
        ("fr", "D'accord. Quand est-ce que vous serez chez vous"),
        ("pt", "Certo, quando posso passar aí"),
        ("it", "Va bene. Dove lascio il pacco"),
        ("ru", "Хорошо. Когда вам удобно"),
        ("hi", "ठीक है। आप कब घर पर होंगे"),
        ("tr", "Tamam. Yarın evde misiniz"),
        ("vi", "Dạ, mai anh có ở nhà không"),
        ("id", "Baik. Kapan Bapak ada di rumah"),
        ("en", "Is that okay?"),
        ("ar", "تمام، كده تمام؟"),
    ];

    /// The caller is done: thanks, okay, that's all — no question left.
    const DONE: &[(&str, &str)] = &[
        ("zh", "好，那就這樣，謝謝"),
        ("zh", "記得明天要繳費喔"),
        ("zh-cn", "好的，没别的事了，谢谢"),
        ("en", "Okay, that's all, thanks."),
        ("en", "What time was it? Never mind, I'll check. Thanks."),
        ("en", "It was nice talking to you."),
        ("ja", "わかりました、ありがとうございます"),
        ("ja", "明日の午後に荷物が届きます"),
        ("es", "Vale, gracias, eso es todo"),
        ("es", "Como le dije, el paquete llega mañana"),
        ("de", "Alles klar, danke schön"),
        ("de", "Das war's dann, vielen Dank"),
        ("th", "โอเคครับ ขอบคุณครับ"),
        ("th", "อย่าลืมจ่ายค่าไฟนะครับ"),
        ("ar", "حسناً، شكراً جزيلاً"),
        ("ar", "لا تنسَ دفع الفاتورة"),
        ("ko", "네, 알겠습니다. 감사합니다"),
        ("fr", "D'accord, merci beaucoup"),
        ("ru", "Хорошо, спасибо"),
        ("tr", "Tamam, teşekkürler"),
        ("vi", "Dạ vâng, cảm ơn anh"),
        ("id", "Baik, terima kasih"),
        ("hi", "ठीक है, धन्यवाद"),
        ("pt", "Tá bom, obrigado"),
        ("it", "Va bene, grazie mille"),
        ("zh", "沒什麼事了，謝謝"),
        ("zh", "沒有沒有，謝謝你"),
        ("ja", "いつもありがとうございます"),
        ("ko", "얼마 전에 보냈어요"),
        ("th", "ไม่มีอะไรแล้วครับ ขอบคุณครับ"),
        ("th", "ไม่เป็นไรครับ"),
    ];

    #[test]
    fn an_unanswered_question_refuses_end_call_in_every_language() {
        for (lang, q) in QUESTIONS {
            assert!(ends_on_question(q), "{lang}: {q}");
            assert_eq!(judge(&check(q)), Err(Refusal::UnansweredQuestion), "{lang}: {q}");
            // Answered since: the AI may close.
            let mut c = check(q);
            c.ai_spoke_since_caller = true;
            assert_eq!(judge(&c), Ok(()), "{lang}: {q}");
        }
    }

    #[test]
    fn a_caller_who_is_done_lets_end_call_through_in_every_language() {
        for (lang, t) in DONE {
            assert!(!ends_on_question(t), "{lang}: {t}");
            assert_eq!(judge(&check(t)), Ok(()), "{lang}: {t}");
        }
        assert!(!ends_on_question(""));
        assert!(!ends_on_question("   "));
    }

    #[test]
    fn the_answer_says_whether_the_call_goes_on() {
        let ok = response("fc-1", Ok(()));
        let r = &ok["toolResponse"]["functionResponses"][0];
        assert_eq!(r["id"], "fc-1");
        assert_eq!(r["name"], NAME);
        assert_eq!(r["response"]["status"], "ending");
        assert_eq!(r["response"]["scheduling"], "SILENT");
        for refusal in [Refusal::Stalling, Refusal::TooEarly, Refusal::UnansweredQuestion] {
            let no = response("fc-2", Err(refusal));
            let r = &no["toolResponse"]["functionResponses"][0]["response"];
            assert_eq!(r["status"], "not_ended");
            assert_eq!(r["reason"], refusal.as_str());
            assert_eq!(r["scheduling"], "WHEN_IDLE");
            assert!(r["next"].as_str().is_some_and(|n| !n.is_empty()));
        }
    }

    fn view() -> PendingView {
        PendingView {
            since_accept: S,
            caller_spoke: false,
            ai_busy: false,
            ai_spoke: true,
            ai_asked: false,
            quiet_for_grace: false,
            quiet_for_close: false,
            caller_done: true,
            caller_talking: false,
            caller_settled: true,
            stalling: false,
        }
    }

    /// The AI said its closing line and called end_call, but the caller has not
    /// said goodbye (seen in Italian, Korean and French calls cut at "matter_done"):
    /// the line waits for their goodbye, or for a long quiet.
    #[test]
    fn without_the_callers_goodbye_the_line_waits_for_a_long_quiet() {
        let v = PendingView { caller_done: false, quiet_for_grace: true, ..view() };
        assert_eq!(step(&v), Step::Wait);
        assert_eq!(step(&PendingView { quiet_for_close: true, ..v }), Step::HangUp);
        // Their goodbye in that time: the short grace is enough.
        assert_eq!(step(&PendingView { caller_done: true, ..v }), Step::HangUp);
        // They speak instead: the call goes on.
        assert_eq!(step(&PendingView { caller_spoke: true, quiet_for_close: true, ..v }), Step::Cancel);
        // Our last words still playing, or a question of ours: never closed.
        assert_eq!(step(&PendingView { ai_busy: true, quiet_for_close: true, ..v }), Step::Wait);
        assert_eq!(step(&PendingView { ai_asked: true, quiet_for_close: true, ..v }), Step::Withdraw);
        assert!(QUIET_CLOSE > GRACE);
    }

    /// The AI asking the caller to wait (seen in a Vietnamese courier call: "Chị
    /// đợi em một tí nhé") is told in every language; ordinary lines with "moment",
    /// "wait" or 「等一下」 meaning later are not.
    #[test]
    fn asking_the_caller_to_wait_is_told_in_every_language() {
        for (lang, said) in [
            ("en", "Okay, hold on a second, let me check."),
            ("en", "Just a moment please."),
            ("es", "Un momento, déjame ver."),
            ("es", "Espere un segundo por favor."),
            ("fr", "Un instant, je vérifie."),
            ("fr", "Ne quittez pas, s'il vous plaît."),
            ("de", "Moment mal, ich schau nach."),
            ("pt", "Só um momento, deixa eu ver."),
            ("it", "Solo un attimo, per favore."),
            ("it", "Attenda in linea, grazie."),
            ("ja", "少々お待ちください。"),
            ("ja", "ちょっと待ってね、確認してみます。"),
            ("ko", "잠깐만요, 확인해 볼게요."),
            ("vi", "Chị đợi em một tí nhé."),
            ("vi", "Dạ, anh chờ chút nha, để em xem."),
            ("zh-TW", "好，請稍等一下喔。"),
            ("zh-TW", "我查一下喔。"),
            ("zh-CN", "您稍等，我帮你查一下。"),
            ("ru", "Минутку, сейчас посмотрю."),
            ("id", "Tunggu sebentar ya."),
            ("tr", "Bir dakika, bakayım."),
        ] {
            assert!(asks_to_wait(said), "{lang}: {said}");
        }
        for (lang, said) in [
            ("en", "Okay, I'll let Alex know and he'll get back to you."),
            ("en", "I'm waiting for the parcel, you can leave it at the door."),
            ("es", "En este momento no estoy en casa, déjelo en portería."),
            ("es", "Espero el paquete mañana, gracias."),
            ("fr", "J'attends le colis demain, merci."),
            ("de", "Im Moment bin ich nicht da, danke, tschüss."),
            ("pt", "No momento não estou em casa."),
            ("it", "Al momento non sono a casa, lo lasci al portiere."),
            ("it", "Domani pomeriggio devo controllare un attimo i miei impegni e ti faccio sapere più tardi."),
            ("ja", "はい、承知しました。明日の午後なら大丈夫です。"),
            ("ko", "네, 내일 오후에 집에 있어요."),
            ("vi", "Dạ, mai chiều em ở nhà ạ."),
            ("vi", "Em đang đợi gói hàng này."),
            ("vi", "Chỗ mai mình đi làm rồi, để mình xem lại rồi báo lại em sau nhé."),
            ("en", "Let me check my schedule and I'll get back to you."),
            ("ja", "予定を確認して、あとで折り返しますね。"),
            ("ko", "내일 오후에는 제가 일정을 좀 확인해 봐야 할 것 같아요. 나중에 다시 연락드릴게요."),
            ("zh-TW", "我晚點看一下行程再跟你說。"),
            ("zh-TW", "我等一下再打給你。"),
            ("zh-TW", "好，我會轉告他，謝謝。"),
            ("ru", "Спасибо, до свидания."),
            ("id", "Terima kasih, sampai jumpa."),
            ("any", ""),
        ] {
            assert!(!asks_to_wait(said), "{lang}: {said}");
        }
    }

    #[test]
    fn the_caller_speaking_in_the_grace_cancels_the_hangup() {
        let mut v = view();
        v.caller_spoke = true;
        // Words of theirs that are not a goodbye.
        v.caller_done = false;
        assert_eq!(step(&v), Step::Cancel);
        // Even once the grace has run.
        v.quiet_for_grace = true;
        assert_eq!(step(&v), Step::Cancel);
        // Still speaking, or their words not settled yet: wait for them first.
        assert_eq!(step(&PendingView { caller_talking: true, ..v }), Step::Wait);
        assert_eq!(step(&PendingView { caller_settled: false, ..v }), Step::Wait);
    }

    /// The caller answering our goodbye with theirs (Italian, German, Korean,
    /// Vietnamese courier calls): the line closes once they have finished and
    /// settled, without the long quiet.
    #[test]
    fn the_callers_goodbye_in_the_grace_closes_the_line() {
        let v = PendingView { caller_spoke: true, caller_done: true, quiet_for_grace: true, ..view() };
        assert_eq!(step(&v), Step::HangUp);
        assert_eq!(step(&PendingView { caller_talking: true, ..v }), Step::Wait);
        assert_eq!(step(&PendingView { caller_settled: false, ..v }), Step::Wait);
        // Our own goodbye back still plays first.
        assert_eq!(step(&PendingView { ai_busy: true, ..v }), Step::Wait);
    }

    /// The sales / scam verdict can come after an end_call was accepted (the
    /// review runs beside the call): the line then stays open, whatever state
    /// the closing was in.
    #[test]
    fn a_call_being_stalled_is_not_closed() {
        for (busy, quiet, spoke, asked, caller) in [
            (false, true, true, false, false),
            (true, false, true, false, false),
            (false, false, false, false, false),
            (false, true, true, true, false),
            (false, true, true, false, true),
        ] {
            let v = PendingView {
                stalling: true,
                ai_busy: busy,
                quiet_for_grace: quiet,
                ai_spoke: spoke,
                ai_asked: asked,
                caller_spoke: caller,
                ..view()
            };
            assert_eq!(step(&v), Step::Stalled, "{v:?}");
        }
        // The same quiet line without the verdict closes.
        assert_eq!(step(&PendingView { quiet_for_grace: true, ..view() }), Step::HangUp);
    }

    #[test]
    fn the_line_closes_after_our_words_and_the_grace() {
        let mut v = view();
        // Our closing line still playing.
        v.ai_busy = true;
        assert_eq!(step(&v), Step::Wait);
        // Played out, not yet quiet long enough.
        v.ai_busy = false;
        assert_eq!(step(&v), Step::Wait);
        v.quiet_for_grace = true;
        assert_eq!(step(&v), Step::HangUp);
    }

    #[test]
    fn end_call_before_the_closing_line_waits_for_it() {
        let mut v = view();
        v.ai_spoke = false;
        v.quiet_for_grace = true;
        assert_eq!(step(&v), Step::Wait);
        // The line never comes: the call ends anyway.
        v.since_accept = LINE_WAIT;
        assert_eq!(step(&v), Step::HangUp);
    }

    #[test]
    fn our_last_words_asking_something_withdraw_the_hangup_in_every_language() {
        let asks = [
            ("zh", "好喔，知道了，那還有別的事嗎"),
            ("en", "Got it, thanks. Anything else?"),
            ("ja", "承知いたしました。他に何かございますか"),
            ("es", "Vale, entendido. ¿Algo más"),
            ("de", "Alles klar. Gibt es sonst noch etwas"),
            ("th", "รับทราบครับ มีเรื่องอื่นอีกไหมครับ"),
            ("ar", "حسناً، فهمت. هل هناك شيء آخر"),
        ];
        let closes = [
            ("zh", "好喔，那先這樣囉"),
            ("en", "Alright, take care."),
            ("ja", "かしこまりました。ご連絡ありがとうございました"),
            ("es", "Vale, gracias por avisar"),
            ("de", "Alles klar, vielen Dank"),
            ("th", "รับทราบครับ ขอบคุณครับ"),
            ("ar", "حسناً، شكراً لك"),
        ];
        for (lang, said) in asks {
            let v = PendingView { ai_asked: ends_on_question(said), quiet_for_grace: true, ..view() };
            assert_eq!(step(&v), Step::Withdraw, "{lang}: {said}");
            // Still playing: wait first.
            assert_eq!(step(&PendingView { ai_busy: true, ..v }), Step::Wait, "{lang}");
        }
        for (lang, said) in closes {
            let v = PendingView { ai_asked: ends_on_question(said), quiet_for_grace: true, ..view() };
            assert_eq!(step(&v), Step::HangUp, "{lang}: {said}");
        }
    }

    fn cold() -> ColdView {
        ColdView {
            on: true,
            stalling: false,
            busy: false,
            eou_n: 3,
            since_eou: Some(COLD_GAP),
            ai_spoke_since_eou: false,
            model_done_since_eou: true,
        }
    }

    #[test]
    fn the_cold_gap_nudge_goes_once_per_caller_turn() {
        let mut g = ColdGap::default();
        let v = cold();
        assert!(g.due(&v));
        g.sent(v.eou_n);
        assert!(!g.due(&v));
        assert!(!g.due(&ColdView { since_eou: Some(10 * S), ..v }));
        // The caller's next turn may get one again.
        assert!(g.due(&ColdView { eou_n: 4, ..v }));
    }

    #[test]
    fn the_cold_gap_waits_for_a_real_silence() {
        let g = ColdGap::default();
        let v = cold();
        assert!(!g.due(&ColdView { since_eou: Some(COLD_GAP - Duration::from_millis(1)), ..v }));
        assert!(!g.due(&ColdView { ai_spoke_since_eou: true, ..v }));
        assert!(!g.due(&ColdView { busy: true, ..v }));
        assert!(!g.due(&ColdView { since_eou: None, eou_n: 0, ..v }));
        assert!(!g.due(&ColdView { on: false, ..v }));
        // The model has not closed its turn: it may still be thinking, up to COLD_GAP_FORCE.
        assert!(!g.due(&ColdView { model_done_since_eou: false, ..v }));
        assert!(g.due(&ColdView { model_done_since_eou: false, since_eou: Some(COLD_GAP_FORCE), ..v }));
    }

    #[test]
    fn no_cold_gap_nudge_while_stalling() {
        let g = ColdGap::default();
        let v = ColdView { stalling: true, since_eou: Some(30 * S), ..cold() };
        assert!(!g.due(&v));
    }
}
/// Acceptance tests added by the A6 reviewer (a6rev).
#[cfg(test)]
mod a6rev_tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn check(turn: &str) -> Check<'_> {
        Check { stalling: false, since_answer: Some(30 * S), caller_turn: turn, ai_spoke_since_caller: false }
    }

    fn view() -> PendingView {
        PendingView {
            since_accept: S,
            caller_spoke: false,
            ai_busy: false,
            ai_spoke: true,
            ai_asked: false,
            quiet_for_grace: false,
            quiet_for_close: false,
            caller_done: true,
            caller_talking: false,
            caller_settled: true,
            stalling: false,
        }
    }

    /// Statements that say "that is all" in many languages: never a question.
    #[test]
    fn that_is_all_is_not_a_question_in_any_language() {
        let said = [
            "that was all", "That's all I wanted, thanks", "That is all, thank you",
            "以上です", "以上でございます", "それだけです、ありがとうございました",
            "Das ist alles", "Das war's, danke", "C'est tout, merci", "C'est tout ce que je voulais",
            "Questo è tutto, grazie", "Es todo, gracias", "Eso es todo",
            "Это всё, спасибо", "Itu saja, terima kasih", "그게 다예요, 감사합니다",
            "Isso é tudo, obrigado", "बस इतना ही, धन्यवाद", "แค่นั้นแหละครับ ขอบคุณครับ",
            "Hết rồi ạ, cảm ơn", "هذا كل شيء، شكراً", "Hepsi bu kadar, teşekkürler", "就這些，謝謝", "就这些，谢谢",
        ];
        for t in said {
            assert!(!ends_on_question(t), "{t}");
            assert_eq!(judge(&check(t)), Ok(()), "{t}");
        }
    }

    /// Questions with no mark (speech-to-text often leaves none) that the check must still catch.
    #[test]
    fn real_questions_without_a_mark_still_refuse() {
        let missed: Vec<&str> = [
            "Is that all", "Is there anything else", "그게 전부인가요", "以上でよろしいですか",
            "Ist das alles", "Hay algo más", "Tem mais alguma coisa", "Alguma coisa mais",
            "还有别的事吗", "還有別的事嗎", "Apakah itu saja", "ยังมีอะไรอีกไหมครับ", "시간 괜찮으세요", "Tienes algo más que decirme",
        ]
        .into_iter()
        .filter(|q| !ends_on_question(q))
        .collect();
        assert!(missed.is_empty(), "not seen as questions: {missed:?}");
    }

    /// Statements and commands next to the marker-less question endings: never questions.
    #[test]
    fn marker_less_endings_do_not_turn_statements_into_questions() {
        for t in [
            "좋은 하루 하세요", "안녕히 계세요", "좋은 하루 되세요", "건강하세요", "네 곧 갈게요",
            "Si necesita algo más, llámenos", "No hay nada más", "Eso es todo, nada más",
            "Se precisar de mais alguma coisa, ligue", "Não tenho mais nada", "Tengo algo más que decirle, no se preocupe",
        ] {
            assert!(!ends_on_question(t), "{t}");
        }
    }

    /// The caller speaks inside the buffer (words that are not a goodbye), whether
    /// or not our words are still playing.
    #[test]
    fn the_caller_in_the_buffer_cancels_in_every_state() {
        for (busy, quiet, spoke, asked) in [
            (true, false, true, false), (true, true, false, false), (false, false, false, true),
            (false, true, true, true), (false, true, false, false),
        ] {
            let v = PendingView {
                caller_spoke: true,
                caller_done: false,
                ai_busy: busy,
                quiet_for_grace: quiet,
                ai_spoke: spoke,
                ai_asked: asked,
                ..view()
            };
            assert_eq!(step(&v), Step::Cancel, "{v:?}");
        }
        // And the hangup needs: not busy, quiet for the grace, nobody spoke.
        let ok = PendingView { quiet_for_grace: true, ..view() };
        assert_eq!(step(&ok), Step::HangUp);
        assert_eq!(step(&PendingView { ai_busy: true, ..ok }), Step::Wait);
        assert_eq!(step(&PendingView { caller_spoke: true, caller_done: false, ..ok }), Step::Cancel);
    }

    /// A stalled call is refused whatever the caller said or how long it has run.
    #[test]
    fn stalling_refuses_at_any_time_and_any_turn() {
        for since in [None, Some(Duration::ZERO), Some(8 * S), Some(3600 * S)] {
            for turn in ["", "bye", "Okay, that's all.", "그럼 이만 끊을게요", "До свидания", "Tchau, obrigado", "Terima kasih, sampai jumpa", "Ciao, grazie"] {
                for spoke in [false, true] {
                    let c = Check { stalling: true, since_answer: since, caller_turn: turn, ai_spoke_since_caller: spoke };
                    assert_eq!(judge(&c), Err(Refusal::Stalling), "{since:?} {turn} {spoke}");
                }
            }
        }
    }

    /// No cold-gap nudge once end_call is accepted (busy), while stalling, or for an errand/call-back (on=false).
    #[test]
    fn the_cold_nudge_stays_out_of_a_closing_or_stalled_call() {
        let v = ColdView { on: true, stalling: false, busy: false, eou_n: 2, since_eou: Some(COLD_GAP), ai_spoke_since_eou: false, model_done_since_eou: true };
        assert!(ColdGap::default().due(&v));
        for bad in [ColdView { busy: true, ..v }, ColdView { stalling: true, ..v }, ColdView { on: false, ..v }] {
            assert!(!ColdGap::default().due(&bad));
        }
    }

    /// A closing line whose first words are a question word is still a statement
    /// (it is the AI's own last words that decide whether the hangup is withdrawn).
    #[test]
    fn closing_lines_that_open_with_a_question_word_are_statements() {
        let lines = [
            ("en", "When you get a chance, just give us a call"),
            ("en", "What a nice chat, take care"),
            ("pt", "Quando precisar, é só ligar"),
            ("fr", "Quand vous voulez, appelez-nous"),
            ("ru", "Как договорились, до свидания"),
            ("de", "Wie besprochen, bis morgen"),
            ("de", "Wann immer Sie wollen, rufen Sie an"),
            ("it", "Mi raccomando, buona giornata"),
            ("it", "Mi dispiace, buona giornata"),
            ("id", "Kapan saja boleh, terima kasih"),
        ];
        let bad: Vec<_> = lines.iter().filter(|(_, l)| ends_on_question(l)).collect();
        assert!(bad.is_empty(), "judged as questions: {bad:?}");
        for (lang, l) in lines {
            let v = PendingView { ai_asked: ends_on_question(l), quiet_for_grace: true, ..view() };
            assert_eq!(step(&v), Step::HangUp, "{lang}: {l}");
        }
    }
}

/// The caller only thanking or saying goodbye after the end_call was accepted
/// ("Muchas gracias", 「네, 감사합니다」) never calls it off; anything new does.
#[cfg(test)]
mod thanks_after_end_call_tests {
    use super::*;
    use crate::closing::ClosingMachine;

    fn view(caller_done: bool) -> PendingView {
        PendingView {
            since_accept: Duration::from_secs(4),
            caller_spoke: true,
            caller_talking: false,
            caller_settled: true,
            ai_busy: false,
            ai_spoke: true,
            ai_asked: false,
            quiet_for_grace: true,
            quiet_for_close: false,
            caller_done,
            stalling: false,
        }
    }

    /// (language, the caller's business, the AI's closing line with its end_call)
    const CALLS: &[(&str, &str, &str)] = &[
        ("es", "Hola, le llamo de la paquetería, mañana le dejo el paquete en portería.", "Con gusto, ahí lo recojo."),
        ("ko", "택배 기사인데요, 내일 경비실에 맡겨 둘게요.", "네, 좋아요. 그렇게 해 주세요."),
        ("en", "Hi, this is the courier, I'll leave the parcel at the front desk tomorrow.", "Sounds good, I'll pick it up there."),
        ("th", "สวัสดีครับ พรุ่งนี้ผมฝากพัสดุไว้ที่นิติบุคคลนะครับ", "ได้ค่ะ เดี๋ยวไปรับเองค่ะ"),
        ("yue", "你好，我係速遞，聽日放喺管理處俾你。", "好呀，我自己去攞。"),
        ("tl", "Magandang hapon po, courier po ito, iiwan ko po bukas sa guard.", "Sige po, kukunin ko na lang doon."),
        ("it", "Buongiorno, sono il corriere, domani lascio il pacco in portineria.", "Perfetto, lo ritiro lì."),
    ];

    fn after_end_call(lang: &str, said: &str) -> Step {
        let &(code, business, ai) = CALLS.iter().find(|c| c.0 == lang).expect("a call in this language");
        let mut m = ClosingMachine::new(code);
        m.on_caller(business, business, false, false);
        m.on_ai_turn(ai, false, false);
        m.on_caller(said, said, crate::locale::caller_wraps_up(said), false);
        step(&view(caller_done(&m, true)))
    }

    #[test]
    fn thanks_or_a_goodbye_after_end_call_closes_the_line() {
        for (lang, said) in [
            ("es", "Muchas gracias."), ("es", "Gracias, adiós."), ("ko", "네, 감사합니다."), ("en", "Okay, thanks!"),
            ("en", "Have a great day!"), ("th", "ขอบคุณครับ"), ("tl", "Salamat po."), ("it", "Grazie, arrivederci!"),
            ("yue", "好，拜拜。"),
        ] {
            assert_eq!(after_end_call(lang, said), Step::HangUp, "{lang}: {said}");
        }
    }

    #[test]
    fn something_new_after_end_call_calls_it_off() {
        for (lang, said) in [
            ("es", "Gracias, pero una cosa más: el paquete es frágil."),
            ("ko", "감사합니다. 근데 경비실은 몇 시까지 해요?"),
            ("en", "Thanks, one more thing, the gate code changed."),
            ("th", "ขอบคุณครับ แต่นิติบุคคลเปิดกี่โมงครับ"),
            ("it", "Grazie, ma a che ora chiude la portineria?"),
        ] {
            assert_eq!(after_end_call(lang, said), Step::Cancel, "{lang}: {said}");
        }
    }

    #[test]
    fn before_the_caller_speaks_again_it_is_the_closings_own_verdict() {
        let mut m = ClosingMachine::new("es");
        m.on_caller("Le dejo el paquete en portería.", "Le dejo el paquete en portería.", false, false);
        m.on_ai_turn("Con gusto.", false, false);
        m.on_caller("Vale.", "Vale.", false, false);
        // Their "vale" came before the end_call: no goodbye of theirs yet.
        assert_eq!(caller_done(&m, false), m.caller_done());
        assert!(!caller_done(&m, false));
    }

    /// The caller's thanks for a settled matter, then the AI's closing line that
    /// is no goodbye word (「好，謝謝你」"Con gusto") with its end_call: the line
    /// closes on the short quiet, not the long one for an unanswered goodbye.
    #[test]
    fn the_callers_own_closing_then_end_call_closes_on_the_short_quiet() {
        for (lang, business, closing, ai) in [
            ("zh-TW", "我是快遞，明天下午家裡有人嗎？", "沒問題，那我明天幫你放在管理室，感謝你喔！", "好，謝謝你。"),
            ("es", "Le llamo de la paquetería, ¿mañana hay alguien en casa?", "Vale, se lo dejo en portería, muchas gracias.", "Con gusto."),
            ("ko", "택배인데요, 내일 집에 계세요?", "네, 알겠습니다. 문 앞에 둘게요. 이용해 주셔서 감사합니다.", "네, 감사합니다."),
            ("th", "สวัสดีครับ ผมเป็นคนส่งพัสดุ พรุ่งนี้มีคนอยู่บ้านไหมครับ", "ได้ครับ งั้นผมฝากไว้ที่นิติบุคคลนะครับ ขอบคุณที่ใช้บริการครับ", "ค่ะ ขอบคุณค่ะ"),
            ("en", "Hi, courier here, anyone home tomorrow?", "Okay, I'll leave it at the front desk. Thanks for your help.", "Sounds good."),
        ] {
            let mut m = ClosingMachine::new(lang);
            m.on_caller(business, business, false, false);
            m.on_ai_turn("Front desk, please.", false, false);
            m.on_caller(closing, closing, crate::locale::caller_wraps_up(closing), false);
            m.on_ai_turn(ai, false, false);
            assert!(caller_done(&m, false), "{lang}: {closing} / {ai}");
            let v = PendingView { caller_spoke: false, ..view(caller_done(&m, false)) };
            assert_eq!(step(&v), Step::HangUp, "{lang}");
        }
    }
}

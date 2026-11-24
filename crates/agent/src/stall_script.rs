// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Sales / scam stalling, one kind of call at a time.
//!
//! Once a call is judged sales / scam, the stall session loads only the text
//! for its kind ([Family], from the scam database's typology): six short
//! descriptions of where the AI stands and what it could do there, or the full
//! stall block when none fits. The AI picks and mixes its own moves and words.
//! [StallEngine] only watches: when the talk goes round in circles (the AI says
//! nearly the same thing twice or gives an earlier reply again, the caller says
//! again what they said before, reply after reply ends in a question) or the
//! caller is leaving, it hands out a short hint on where to take the talk (a
//! direction, never a line), sent in the pause after the AI's turn. The checks
//! compare characters, so they work the same in every language. Pure in-memory
//! work: no I/O, no model call, no waiting.

use crate::locale::Lang;
use crate::prompt_book::Text;
use std::collections::VecDeque;

// The words are in the prompt book (data/prompts/<language>.json); these are their handles.
crate::prompt_book::prompt_texts! {
    EN_IN_CALLER_LANGUAGE = "en/stall_script_en_in_caller_language";
    /// A call no script fits (a guess-who call, a lottery, a job offer…), in the
    /// short English block: moves that need no product, plans or figures.
    EN_GENERAL_SCRIPT = "en/stall_script_en_general_script";
    /// The stall session's rules for every language but Chinese, in place of the
    /// whole English stall block (~4,400 characters): a longer prompt slows every
    /// reply's first sound. The script of the call's kind follows it. `{wait}`: the
    /// language's line for stopping a leaving caller.
    EN_COMPACT_BLOCK = "en/stall_script_en_compact_block";
    ZH_SCRIPT_INVESTMENT = "zh-TW/stall_script_investment";
    ZH_SCRIPT_INSURANCE = "zh-TW/stall_script_insurance";
    ZH_SCRIPT_CREDIT = "zh-TW/stall_script_credit";
    ZH_SCRIPT_IMPERSONATION = "zh-TW/stall_script_impersonation";
    ZH_SCRIPT_COLLECTION = "zh-TW/stall_script_collection";
    ZH_SCRIPT_HARASSMENT = "zh-TW/stall_script_harassment";
    ZH_LOOP_HINTS_CHANGE_TOPIC = "zh-TW/stall_script_loop_hints_change_topic";
    ZH_LOOP_HINTS_NEW_ANGLE = "zh-TW/stall_script_loop_hints_new_angle";
    ZH_COLLECTION_LOOP_OTHER_DETAIL = "zh-TW/stall_script_collection_loop_other_detail";
    ZH_COLLECTION_LOOP_CHANGE_TOPIC = "zh-TW/stall_script_collection_loop_change_topic";
    ZH_IMPERSONATION_LOOP_STUCK_ELSEWHERE = "zh-TW/stall_script_impersonation_loop_stuck_elsewhere";
    ZH_IMPERSONATION_LOOP_CHANGE_TOPIC = "zh-TW/stall_script_impersonation_loop_change_topic";
    ZH_HARASSMENT_LOOP_OTHER_WAY = "zh-TW/stall_script_harassment_loop_other_way";
    ZH_REPEAT_SAID_BEFORE = "zh-TW/stall_script_repeat_said_before";
    ZH_REPEAT_CALLER_AGAIN = "zh-TW/stall_script_repeat_caller_again";
    ZH_REPEAT_QUESTION_RUN = "zh-TW/stall_script_repeat_question_run";
    ZH_RETAIN_KEEP = "zh-TW/stall_script_retain_keep";
    ZH_RETAIN_KEEP_REASON = "zh-TW/stall_script_retain_keep_reason";
    ZH_IMPERSONATION_RETAIN_KEEP = "zh-TW/stall_script_impersonation_retain_keep";
    ZH_COLLECTION_RETAIN_KEEP = "zh-TW/stall_script_collection_retain_keep";
    ZH_HARASSMENT_RETAIN_KEEP = "zh-TW/stall_script_harassment_retain_keep";
    ZH_MARK = "zh-TW/stall_script_note_mark";
    EN_SCRIPT_INVESTMENT = "en/stall_script_investment";
    EN_SCRIPT_INSURANCE = "en/stall_script_insurance";
    EN_SCRIPT_CREDIT = "en/stall_script_credit";
    EN_SCRIPT_IMPERSONATION = "en/stall_script_impersonation";
    EN_SCRIPT_COLLECTION = "en/stall_script_collection";
    EN_SCRIPT_HARASSMENT = "en/stall_script_harassment";
    EN_LOOP_HINTS_CHANGE_TOPIC = "en/stall_script_loop_hints_change_topic";
    EN_LOOP_HINTS_NEW_ANGLE = "en/stall_script_loop_hints_new_angle";
    EN_COLLECTION_LOOP_OTHER_DETAIL = "en/stall_script_collection_loop_other_detail";
    EN_COLLECTION_LOOP_CHANGE_TOPIC = "en/stall_script_collection_loop_change_topic";
    EN_IMPERSONATION_LOOP_STUCK_ELSEWHERE = "en/stall_script_impersonation_loop_stuck_elsewhere";
    EN_IMPERSONATION_LOOP_CHANGE_TOPIC = "en/stall_script_impersonation_loop_change_topic";
    EN_HARASSMENT_LOOP_OTHER_WAY = "en/stall_script_harassment_loop_other_way";
    EN_REPEAT_SAID_BEFORE = "en/stall_script_repeat_said_before";
    EN_REPEAT_CALLER_AGAIN = "en/stall_script_repeat_caller_again";
    EN_REPEAT_QUESTION_RUN = "en/stall_script_repeat_question_run";
    EN_RETAIN_KEEP = "en/stall_script_retain_keep";
    EN_RETAIN_KEEP_REASON = "en/stall_script_retain_keep_reason";
    EN_IMPERSONATION_RETAIN_KEEP = "en/stall_script_impersonation_retain_keep";
    EN_COLLECTION_RETAIN_KEEP = "en/stall_script_collection_retain_keep";
    EN_HARASSMENT_RETAIN_KEEP = "en/stall_script_harassment_retain_keep";
    EN_MARK = "en/stall_script_note_mark";
    EN_OTHER_MARK = "en/stall_script_note_mark_other_language";
}

/// The kind of sales / scam call the script is written for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Investment,
    Insurance,
    Credit,
    /// Fake police / prosecutor / bank / shop or platform verification.
    Impersonation,
    /// Outsourced debt collection: never admit the debt.
    Collection,
    /// Abuse and threats: the line keeps breaking up.
    Harassment,
    /// No better fit: the full stall block with every move.
    General,
}

impl Family {
    /// The family of a scam database typology id (unknown or empty: general).
    pub fn of(typology: &str) -> Family {
        match typology.trim() {
            "investment_group_scam" | "crypto_scam" | "investment_sales" => Family::Investment,
            "insurance_sales" | "fake_health_insurance" | "medicare_benefits" | "health_beauty_sales" => {
                Family::Insurance
            }
            "loan_sales" | "loan_advance_fee" | "bank_card_sales" | "credit_record_scam" => Family::Credit,
            "fake_official"
            | "fake_bank_installment"
            | "fake_shop_order_error"
            | "fake_bank_account_alert"
            | "otp_theft"
            | "marketplace_escrow_scam"
            | "gift_card_payment"
            | "tech_support_remote"
            | "fake_customs_parcel"
            | "fake_telecom"
            | "fake_utility"
            | "tax_refund_scam"
            | "irs_ssa_impersonation"
            | "fake_government_subsidy" => Family::Impersonation,
            "fake_debt_collection" | "debt_collection_harassment" => Family::Collection,
            "harassment_intimidation" => Family::Harassment,
            _ => Family::General,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Family::Investment => "investment",
            Family::Insurance => "insurance",
            Family::Credit => "credit",
            Family::Impersonation => "impersonation",
            Family::Collection => "collection",
            Family::Harassment => "harassment",
            Family::General => "general",
        }
    }
}

static ZH_SCRIPTS: [(Family, &Text); 6] = [
    (Family::Investment, &ZH_SCRIPT_INVESTMENT),
    (Family::Insurance, &ZH_SCRIPT_INSURANCE),
    (Family::Credit, &ZH_SCRIPT_CREDIT),
    (Family::Impersonation, &ZH_SCRIPT_IMPERSONATION),
    (Family::Collection, &ZH_SCRIPT_COLLECTION),
    (Family::Harassment, &ZH_SCRIPT_HARASSMENT),
];

static EN_SCRIPTS: [(Family, &Text); 6] = [
    (Family::Investment, &EN_SCRIPT_INVESTMENT),
    (Family::Insurance, &EN_SCRIPT_INSURANCE),
    (Family::Credit, &EN_SCRIPT_CREDIT),
    (Family::Impersonation, &EN_SCRIPT_IMPERSONATION),
    (Family::Collection, &EN_SCRIPT_COLLECTION),
    (Family::Harassment, &EN_SCRIPT_HARASSMENT),
];

/// [EN_COMPACT_BLOCK] in `lang` with the script of `family` (general moves when
/// none fits).
fn compact_block(lang: Lang, family: Family) -> String {
    let script = script(family, lang).unwrap_or_else(|| match lang.base() {
        Lang::En => EN_GENERAL_SCRIPT.get().to_string(),
        _ => format!("{EN_GENERAL_SCRIPT}{EN_IN_CALLER_LANGUAGE}"),
    });
    format!("{}{script}", EN_COMPACT_BLOCK.get().replace("{wait}", crate::prompts::wait_one_more(lang)))
}

/// The script of `family` in the call's language (Traditional Chinese, Simplified
/// Chinese, English; the other languages get the English text and are told to
/// speak the caller's language). General: none (the full stall block stays).
pub fn script(family: Family, lang: Lang) -> Option<String> {
    let pick = |table: &[(Family, &'static Text)]| table.iter().find(|(f, _)| *f == family).map(|(_, s)| s.get());
    match lang.base() {
        Lang::ZhTw => pick(&ZH_SCRIPTS).map(str::to_string),
        Lang::ZhCn => pick(&ZH_SCRIPTS).map(crate::zh_convert::to_cn),
        Lang::En => pick(&EN_SCRIPTS).map(str::to_string),
        _ => pick(&EN_SCRIPTS).map(|s| format!("{s}{EN_IN_CALLER_LANGUAGE}")),
    }
}

/// Where the stall block lists every move or every topic: from `start` to the
/// first `end` after it (`end` included unless `keep_end`).
struct Span {
    start: &'static str,
    end: &'static str,
    keep_end: bool,
}

/// Chinese block (Simplified is the converted text: the markers read the same).
const ZH_MOVES: Span = Span { start: "可以用的招", end: "。", keep_end: false };
const ZH_TOPICS: Span = Span { start: "可以聊的方向", end: "他想", keep_end: true };
/// English block (shared by the other languages).
const EN_MOVES: Span = Span { start: "Moves you can use", end: "\n", keep_end: false };
const EN_TOPICS: Span = Span { start: "Things to talk about", end: "\n", keep_end: false };

/// Byte range of `span` in `s`.
fn find_span(s: &str, span: &Span) -> Option<(usize, usize)> {
    let a = s.find(span.start)?;
    let rel = s[a..].find(span.end)?;
    let b = if span.keep_end { a + rel } else { a + rel + span.end.len() };
    Some((a, b))
}

/// The stall block with its list of moves replaced by `script` and its list of
/// topics left out (None: the block does not have them).
fn slim_block(block: &str, script: &str) -> Option<String> {
    let (moves, topics) = match (find_span(block, &ZH_MOVES), find_span(block, &ZH_TOPICS)) {
        (Some(m), Some(t)) => (m, t),
        _ => (find_span(block, &EN_MOVES)?, find_span(block, &EN_TOPICS)?),
    };
    if topics.0 < moves.1 {
        return None;
    }
    let sep = if block[moves.0..moves.1].ends_with('\n') { "\n" } else { "" };
    Some(format!(
        "{}{script}{sep}{}{}",
        &block[..moves.0],
        &block[moves.1..topics.0],
        &block[topics.1..]
    ))
}

/// The stall session's prompt for `typology`: the stall block keeps its rules,
/// and its every-move / every-topic lists give way to the script of the call's
/// kind; in every language but Chinese the whole block gives way to the short
/// one ([EN_COMPACT_BLOCK]) and the script. Returns the prompt and the language
/// its block is in (None: no known block in it, the prompt is unchanged).
/// General: the Chinese block unchanged, the short one with general moves.
pub fn slim_stall_prompt(prompt: &str, typology: &str) -> (String, Option<Lang>) {
    let family = Family::of(typology);
    for lang in crate::locale::ALL {
        let block = crate::prompts::pack(lang).stall_block.as_str();
        if block.trim().is_empty() || !prompt.contains(block) {
            continue;
        }
        if !lang.is_zh() {
            return (prompt.replacen(block, &compact_block(lang, family), 1), Some(lang));
        }
        let Some(script) = script(family, lang) else {
            return (prompt.to_string(), Some(lang));
        };
        return match slim_block(block, &script) {
            Some(slim) => (prompt.replacen(block, &slim, 1), Some(lang)),
            None => (format!("{prompt}\n{script}"), Some(lang)),
        };
    }
    (prompt.to_string(), None)
}

// ---------------------------------------------------------------- course hints

/// What the engine's last word was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The AI finds its own way: no hint.
    Free,
    /// The talk goes round in circles (the AI or the caller says again what
    /// was said, or every reply ends the same way): a hint to change course.
    Loop,
    /// The caller is leaving: a hint to keep them.
    Retain,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Free => "free",
            Stage::Loop => "loop",
            Stage::Retain => "retain",
        }
    }
}

/// Longest cue (characters, whole text with its note mark).
pub const MAX_CUE_CHARS: usize = 80;
/// Two AI turns this alike (shared character pairs) count as going round in circles.
pub const LOOP_SIMILARITY: f32 = 0.6;
/// An AI turn this like an earlier one of the call (not only the last) is that
/// reply given again: the caller came back to a point and got the old answer
/// word for word. Unrelated replies of one call stay under 0.5 in every
/// language tried, the same reply said again is above 0.7.
pub const REUSE_SIMILARITY: f32 = 0.7;
/// A caller turn this like an earlier one of theirs is the same thing said again.
pub const CALLER_REPEAT_SIMILARITY: f32 = 0.7;
/// Turns kept on each side for those two checks.
const MEMORY_TURNS: usize = 60;
/// A line lighter than this ([content_weight]) is too short to tell from another
/// (「好好好」, "yes, I'm here"): it is neither kept nor looked up.
const REPEAT_MIN_WEIGHT: usize = 24;
/// This many AI turns in a row ending in a question bring the hint to end the
/// next one another way.
const QUESTION_RUN: u32 = 5;
/// AI turns between two hints of the kinds above (the hint for two turns alike
/// in a row and the keep hint are never held back).
const HINT_GAP_TURNS: u32 = 3;

/// Where to take the talk next, never a line to say: id, Chinese, English.
type Hint = (&'static str, &'static Text, &'static Text);

/// The AI is going round in circles.
static LOOP_HINTS: [Hint; 2] = [
    ("change_topic", &ZH_LOOP_HINTS_CHANGE_TOPIC, &EN_LOOP_HINTS_CHANGE_TOPIC),
    ("new_angle", &ZH_LOOP_HINTS_NEW_ANGLE, &EN_LOOP_HINTS_NEW_ANGLE),
];
static COLLECTION_LOOP: [Hint; 2] = [
    ("other_detail", &ZH_COLLECTION_LOOP_OTHER_DETAIL, &EN_COLLECTION_LOOP_OTHER_DETAIL),
    ("change_topic", &ZH_COLLECTION_LOOP_CHANGE_TOPIC, &EN_COLLECTION_LOOP_CHANGE_TOPIC),
];
static IMPERSONATION_LOOP: [Hint; 2] = [
    ("stuck_elsewhere", &ZH_IMPERSONATION_LOOP_STUCK_ELSEWHERE, &EN_IMPERSONATION_LOOP_STUCK_ELSEWHERE),
    ("change_topic", &ZH_IMPERSONATION_LOOP_CHANGE_TOPIC, &EN_IMPERSONATION_LOOP_CHANGE_TOPIC),
];
static HARASSMENT_LOOP: [Hint; 1] = [
    ("other_way", &ZH_HARASSMENT_LOOP_OTHER_WAY, &EN_HARASSMENT_LOOP_OTHER_WAY),
];
/// The AI gave a reply of some turns ago again.
static SAID_BEFORE: Hint = ("said_before", &ZH_REPEAT_SAID_BEFORE, &EN_REPEAT_SAID_BEFORE);
/// The caller is saying again what they said earlier in the call.
static CALLER_AGAIN: Hint = ("caller_again", &ZH_REPEAT_CALLER_AGAIN, &EN_REPEAT_CALLER_AGAIN);
/// Reply after reply ends in a question.
static QUESTION_RUN_HINT: Hint = ("question_run", &ZH_REPEAT_QUESTION_RUN, &EN_REPEAT_QUESTION_RUN);

/// The caller is leaving. 「欸等一下，我還有一個問題」 is the owner's own example
/// of the feel, not a line to recite.
static RETAIN: [Hint; 2] = [
    ("keep", &ZH_RETAIN_KEEP, &EN_RETAIN_KEEP),
    ("keep_reason", &ZH_RETAIN_KEEP_REASON, &EN_RETAIN_KEEP_REASON),
];
static IMPERSONATION_RETAIN: [Hint; 1] = [
    ("keep", &ZH_IMPERSONATION_RETAIN_KEEP, &EN_IMPERSONATION_RETAIN_KEEP),
];
static COLLECTION_RETAIN: [Hint; 1] = [
    ("keep", &ZH_COLLECTION_RETAIN_KEEP, &EN_COLLECTION_RETAIN_KEEP),
];
static HARASSMENT_RETAIN: [Hint; 1] = [
    ("keep", &ZH_HARASSMENT_RETAIN_KEEP, &EN_HARASSMENT_RETAIN_KEEP),
];

/// A hint's text in `lang`, with its note mark.
fn cue_text(m: &Hint, lang: Lang) -> String {
    match lang.base() {
        Lang::ZhTw => format!("{ZH_MARK}{}", m.1.get()),
        Lang::ZhCn => crate::zh_convert::to_cn(&format!("{ZH_MARK}{}", m.1.get())),
        Lang::En => format!("{EN_MARK}{}", m.2.get()),
        _ => format!("{EN_OTHER_MARK}{}", m.2.get()),
    }
}

/// Characters that carry content, lower-cased (punctuation and spaces out).
fn content_chars(s: &str) -> Vec<char> {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// How alike two lines are: shared character pairs (Dice), 0..=1. Short lines: 0.
pub fn similarity(a: &str, b: &str) -> f32 {
    let (a, b) = (content_chars(a), content_chars(b));
    if a.len() < 6 || b.len() < 6 {
        return 0.0;
    }
    let pairs = |v: &[char]| -> Vec<(char, char)> {
        let mut p: Vec<(char, char)> = v.windows(2).map(|w| (w[0], w[1])).collect();
        p.sort_unstable();
        p
    };
    let (pa, pb) = (pairs(&a), pairs(&b));
    let (mut i, mut j, mut shared) = (0, 0, 0usize);
    while i < pa.len() && j < pb.len() {
        match pa[i].cmp(&pb[j]) {
            std::cmp::Ordering::Equal => {
                shared += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    (2 * shared) as f32 / (pa.len() + pb.len()) as f32
}

/// A character of a script written without spaces between words, where one
/// character says about as much as a short word (Chinese, kana, hangul).
fn is_wide(c: char) -> bool {
    matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF)
}

/// How much a line says: its letters and digits, a wide character counting as three.
fn content_weight(s: &str) -> usize {
    content_chars(s).iter().map(|c| if is_wide(*c) { 3 } else { 1 }).sum()
}

/// `said` is nearly one of the `earlier` lines (at least `threshold` alike).
fn said_before(earlier: &VecDeque<String>, said: &str, threshold: f32) -> bool {
    content_weight(said) >= REPEAT_MIN_WEIGHT && earlier.iter().any(|e| similarity(e, said) >= threshold)
}

/// Keeps `said` for later turns to be compared with (the newest [MEMORY_TURNS]).
fn remember(lines: &mut VecDeque<String>, said: &str) {
    if content_weight(said) < REPEAT_MIN_WEIGHT {
        return;
    }
    if lines.len() >= MEMORY_TURNS {
        lines.pop_front();
    }
    lines.push_back(said.to_string());
}

/// The line ends in a question mark of any script (closing quotes, brackets and
/// further marks after it do not matter). Languages that mark no question in
/// writing are not seen.
fn ends_with_question(s: &str) -> bool {
    s.trim_end()
        .chars()
        .rev()
        .take_while(|c| !c.is_alphanumeric() && !c.is_whitespace())
        .any(|c| matches!(c, '?' | '？' | '؟' | '⁇' | '⁈' | '⁉'))
}

/// A hint for the AI's next reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub stage: Stage,
    pub tactic: &'static str,
    pub turn: u32,
    /// The talk went round in circles ([Stage::Loop]).
    pub looped: bool,
    pub text: String,
}

/// One per stalled call. The AI picks its own moves from the prompt; this only
/// counts its turns, keeps what was said and, when the talk goes round in
/// circles or the caller is leaving, hands out a short hint on where to go next.
#[derive(Debug, Clone)]
pub struct StallEngine {
    pub family: Family,
    typology: String,
    lang: Lang,
    ai_turns: u32,
    stage: Stage,
    last_ai: String,
    /// The AI's turns so far while stalling (the newest [MEMORY_TURNS]).
    ai_said: VecDeque<String>,
    /// The caller's turns so far while stalling (the newest [MEMORY_TURNS]).
    caller_said: VecDeque<String>,
    /// The caller's last turn said again what an earlier one of theirs said.
    caller_again: bool,
    /// AI turns in a row that ended in a question.
    question_run: u32,
    /// AI turns since the last hint went out.
    turns_since_hint: u32,
    retain_pending: bool,
    next_loop: usize,
    next_retain: usize,
}

impl StallEngine {
    pub fn new(typology: &str, lang: Lang) -> Self {
        StallEngine {
            family: Family::of(typology),
            typology: typology.trim().to_string(),
            lang,
            ai_turns: 0,
            stage: Stage::Free,
            last_ai: String::new(),
            ai_said: VecDeque::new(),
            caller_said: VecDeque::new(),
            caller_again: false,
            question_run: 0,
            turns_since_hint: HINT_GAP_TURNS,
            retain_pending: false,
            next_loop: 0,
            next_retain: 0,
        }
    }

    pub fn typology(&self) -> &str {
        if self.typology.is_empty() {
            "-"
        } else {
            &self.typology
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn turns(&self) -> u32 {
        self.ai_turns
    }

    fn loop_hints(&self) -> &'static [Hint] {
        match self.family {
            Family::Collection => &COLLECTION_LOOP,
            Family::Harassment => &HARASSMENT_LOOP,
            Family::Impersonation => &IMPERSONATION_LOOP,
            _ => &LOOP_HINTS,
        }
    }

    fn retain_hints(&self) -> &'static [Hint] {
        match self.family {
            Family::Impersonation => &IMPERSONATION_RETAIN,
            Family::Collection => &COLLECTION_RETAIN,
            Family::Harassment => &HARASSMENT_RETAIN,
            _ => &RETAIN,
        }
    }

    /// The caller announced the end or said bye (closing machine events while
    /// stalling): a keep hint is owed. True when this starts it (not already owed).
    pub fn on_caller_closing(&mut self) -> bool {
        if self.retain_pending {
            return false;
        }
        self.retain_pending = true;
        self.stage = Stage::Retain;
        true
    }

    /// The keep hint, for a keep nudge that goes out now (the caller is leaving
    /// or the AI started a farewell): it is no longer owed.
    pub fn take_retain(&mut self) -> Cue {
        self.retain_pending = false;
        self.stage = Stage::Retain;
        self.turns_since_hint = 0;
        let list = self.retain_hints();
        let h = &list[self.next_retain % list.len()];
        self.next_retain += 1;
        Cue { stage: Stage::Retain, tactic: h.0, turn: self.ai_turns, looped: false, text: cue_text(h, self.lang) }
    }

    /// A caller turn ended with `said`: it is kept, and noted when it says again
    /// what an earlier turn of theirs said. The hint for that goes out after the
    /// AI's turn ([Self::on_ai_turn]), like every other.
    pub fn on_caller_turn(&mut self, said: &str) {
        let said = said.trim();
        if said.is_empty() {
            return;
        }
        self.caller_again = said_before(&self.caller_said, said, CALLER_REPEAT_SIMILARITY);
        remember(&mut self.caller_said, said);
    }

    /// An AI turn ended with `said`: the hint to send in the pause now, if any —
    /// only when a keep hint is owed or the talk is going round in circles: the
    /// AI said nearly its last turn again, gave an earlier reply again, the
    /// caller is repeating themselves, or reply after reply ends in a question.
    pub fn on_ai_turn(&mut self, said: &str) -> Option<Cue> {
        self.ai_turns += 1;
        self.turns_since_hint = self.turns_since_hint.saturating_add(1);
        let said = said.trim();
        let looped = !said.is_empty() && similarity(&self.last_ai, said) >= LOOP_SIMILARITY;
        let reused = !looped && said_before(&self.ai_said, said, REUSE_SIMILARITY);
        // Noted for this reply only: one that is held back is not owed later.
        let caller_again = std::mem::take(&mut self.caller_again);
        if !said.is_empty() {
            self.question_run = if ends_with_question(said) { self.question_run + 1 } else { 0 };
            self.last_ai = said.to_string();
            remember(&mut self.ai_said, said);
        }
        if self.retain_pending {
            let mut cue = self.take_retain();
            cue.looped = looped;
            return Some(cue);
        }
        let h: &Hint = if looped {
            let list = self.loop_hints();
            self.next_loop += 1;
            &list[(self.next_loop - 1) % list.len()]
        } else if self.turns_since_hint < HINT_GAP_TURNS {
            self.stage = Stage::Free;
            return None;
        } else if caller_again {
            // Named first: it is also why an old reply came back.
            &CALLER_AGAIN
        } else if reused {
            &SAID_BEFORE
        } else if self.question_run >= QUESTION_RUN {
            self.question_run = 0;
            &QUESTION_RUN_HINT
        } else {
            self.stage = Stage::Free;
            return None;
        };
        self.stage = Stage::Loop;
        self.turns_since_hint = 0;
        Some(Cue { stage: Stage::Loop, tactic: h.0, turn: self.ai_turns, looped: true, text: cue_text(h, self.lang) })
    }
}

/// A note for the Live model that asks for no reply now (`turnComplete` false):
/// it shapes the AI's next reply, sent in the pause after its turn.
pub fn client_note(text: &str) -> String {
    serde_json::json!({
        "clientContent": {
            "turns": [{ "role": "user", "parts": [{ "text": text }] }],
            "turnComplete": false
        }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_FAMILIES: [Family; 6] = [
        Family::Investment,
        Family::Insurance,
        Family::Credit,
        Family::Impersonation,
        Family::Collection,
        Family::Harassment,
    ];

    #[test]
    fn typologies_map_to_six_families() {
        assert_eq!(Family::of("investment_group_scam"), Family::Investment);
        assert_eq!(Family::of("crypto_scam"), Family::Investment);
        assert_eq!(Family::of("insurance_sales"), Family::Insurance);
        assert_eq!(Family::of("fake_health_insurance"), Family::Insurance);
        assert_eq!(Family::of("loan_sales"), Family::Credit);
        assert_eq!(Family::of("loan_advance_fee"), Family::Credit);
        assert_eq!(Family::of("fake_official"), Family::Impersonation);
        assert_eq!(Family::of("fake_bank_installment"), Family::Impersonation);
        assert_eq!(Family::of("marketplace_escrow_scam"), Family::Impersonation);
        assert_eq!(Family::of("irs_ssa_impersonation"), Family::Impersonation);
        assert_eq!(Family::of("fake_debt_collection"), Family::Collection);
        assert_eq!(Family::of("debt_collection_harassment"), Family::Collection);
        assert_eq!(Family::of("harassment_intimidation"), Family::Harassment);
        assert_eq!(Family::of("general_promotion"), Family::General);
        assert_eq!(Family::of("contact_stall_list"), Family::General);
        assert_eq!(Family::of(""), Family::General);
    }

    #[test]
    fn every_database_typology_has_a_family_or_is_general_on_purpose() {
        // Kinds the six scripts do not fit stay general (the full stall block).
        let general = [
            "romance_scam", "job_account_mule", "guess_who", "fake_relative_money", "prize_lottery",
            "columbarium_scam", "fake_charity", "training_fund_scam", "telecom_energy_sales",
            "course_trial_sales", "software_demo_sales", "real_estate_sales", "warranty_robocall",
            "home_service_sales", "business_listing_sales", "travel_timeshare_sales", "survey_lead",
            "general_promotion", "hidden_number", "foreign_number", "tw_shown_as_international",
            "tw_premium_rate", "cn_virtual_operator", "kr_internet_phone", "jp_ip_phone", "gb_personal_number",
        ];
        for t in &crate::scam_db::db().typologies {
            let f = Family::of(&t.id);
            assert!(f != Family::General || general.contains(&t.id.as_str()), "{} has no family", t.id);
        }
    }

    #[test]
    fn scripts_are_short_in_every_language() {
        for f in ALL_FAMILIES {
            for lang in crate::locale::ALL {
                let s = script(f, lang).unwrap();
                assert!(s.chars().count() <= 600, "{f:?} {lang:?} {}", s.chars().count());
            }
        }
        assert!(script(Family::General, Lang::ZhTw).is_none());
        assert!(script(Family::Investment, Lang::Ja).unwrap().ends_with(EN_IN_CALLER_LANGUAGE.get()));
    }

    #[test]
    fn scripts_never_check_the_caller_out_or_admit_debt() {
        for f in ALL_FAMILIES {
            let zh = script(f, Lang::ZhTw).unwrap();
            for bad in ["名字", "工號", "員工編號", "分機", "案號", "證明", "我查一下", "請稍等"] {
                assert!(!zh.contains(bad), "{f:?} {bad}");
            }
            let en = script(f, Lang::En).unwrap().to_lowercase();
            for bad in ["your name", "badge", "extension", "case number", "prove"] {
                assert!(!en.contains(bad), "{f:?} {bad}");
            }
        }
        // The lines it must not cross stay, said as part of the situation.
        for f in [Family::Collection, Family::Credit] {
            let zh = script(f, Lang::ZhTw).unwrap();
            assert!(zh.contains("不承認欠錢") && zh.contains("不答應還款"), "{f:?}");
            assert!(script(f, Lang::En).unwrap().contains("never admit owing money"), "{f:?}");
        }
        let fake = script(Family::Impersonation, Lang::ZhTw).unwrap();
        assert!(fake.contains("驗證碼、帳號、密碼一個字都不講") && fake.contains("錢一毛不動"), "{fake}");
        // A situation and a few ideas, never a fixed line for the AI to say.
        for f in ALL_FAMILIES {
            assert!(!script(f, Lang::ZhTw).unwrap().contains('「'), "{f:?}");
        }
    }

    #[test]
    fn stall_prompt_loads_only_the_calls_script() {
        // Every stalled call, whatever its kind and language, asks about the offer
        // and never about the caller, keeps the next step on this call, and
        // plays along without a promise to pay.
        for lang in crate::locale::ALL {
            let block = crate::prompts::pack(lang).stall_block.clone();
            for typology in ["investment_group_scam", "fake_official", "fake_debt_collection", "general_promotion"] {
                let (slim, _) = slim_stall_prompt(&block, typology);
                let marks: &[&str] = if lang.is_zh() {
                    &["不問他這個人", "之後誰會聯絡你", "請他在電話上帶你做", "不叫他等你", "不說已經轉了"]
                } else {
                    &["never about them", "who will contact you", "step by step on this call, not by text later", "never ask them to wait", "already did"]
                };
                for m in marks {
                    let m = if lang == Lang::ZhCn { crate::zh_convert::to_cn(m) } else { m.to_string() };
                    assert!(slim.contains(&m), "{lang:?} {typology}: {m}");
                }
            }
        }
        for lang in crate::locale::ALL {
            let block = crate::prompts::pack(lang).stall_block.clone();
            let prompt = format!("BASE PROMPT{block}\nGUARD");
            let (slim, found) = slim_stall_prompt(&prompt, "investment_group_scam");
            assert!(found.is_some(), "{lang:?}");
            let found = found.unwrap();
            let own = script(Family::Investment, found).unwrap();
            assert!(slim.contains(&own), "{lang:?}");
            assert!(slim.starts_with("BASE PROMPT") && slim.ends_with("\nGUARD"), "{lang:?}");
            assert!(slim.chars().count() < prompt.chars().count(), "{lang:?}");
            // The other scripts and the full lists are out.
            let other = script(Family::Collection, found).unwrap();
            assert!(!slim.contains(&other), "{lang:?}");
            assert!(!slim.contains("可以聊的方向") && !slim.contains("Things to talk about"), "{lang:?}");
            assert!(!slim.contains("可以用的招") && !slim.contains("Moves you can use"), "{lang:?}");
            // The block's rules stay.
            if lang.is_zh() {
                assert!(slim.contains("叫什麼名字、員工編號") || slim.contains("叫什么名字、员工编号"), "{lang:?}");
            } else {
                // The short block in its place: the rules, this language's line to stop a leaving caller.
                assert!(!slim.contains(block.trim()), "{lang:?}");
                assert!(slim.contains("never about them: no name, staff number, extension, case number"), "{lang:?}");
                assert!(slim.contains("never admit owing money") && slim.contains("Never give or invent real data"), "{lang:?}");
                assert!(slim.contains(crate::prompts::wait_one_more(lang)), "{lang:?}");
                assert!(!slim.to_lowercase().contains("hang on") && !slim.to_lowercase().contains("hold on"), "{lang:?}");
            }
            // General: the full Chinese block; elsewhere the short one with general moves.
            let (same, _) = slim_stall_prompt(&prompt, "general_promotion");
            if lang.is_zh() {
                assert_eq!(same, prompt);
            } else {
                assert!(same.contains(EN_GENERAL_SCRIPT.get()) && same.chars().count() * 2 < prompt.chars().count(), "{lang:?}");
            }
        }
        // No stall block: unchanged.
        assert_eq!(slim_stall_prompt("你是我。", "fake_official"), ("你是我。".to_string(), None));
    }

    /// The stall session's prompt besides its persona part stays within 3,000
    /// characters for an English call on a US SIM, whatever the kind of call
    /// (the short generic persona no longer loses ~840 chars of normal-call
    /// boilerplate during stalling, while the full prompt stays ~1,400 chars
    /// shorter overall than before).
    #[test]
    fn the_english_stall_prompt_stays_within_the_budget() {
        // As a call has it: the region's rules and its stalling snags.
        let scope = crate::prompts::Scope { region: crate::country_profile::Region::of_code("us"), stall: true, ..Default::default() };
        let full = crate::prompts::scope_to_region(&crate::prompts::build(&english_stall_ctx()), Lang::En, scope);
        let persona = crate::prompts::persona_chars(Lang::En, false, "Sam", "");
        for typology in ["investment_group_scam", "fake_official", "fake_debt_collection", "general_promotion"] {
            let (slim, found) = slim_stall_prompt(&full, typology);
            assert_eq!(found, Some(Lang::En));
            let rest = slim.chars().count() - persona;
            assert!(rest <= STALL_ADDITION_BUDGET, "{typology}: {rest} besides the persona (was {})", full.chars().count() - persona);
        }
    }

    const STALL_ADDITION_BUDGET: usize = 3_100;

    fn english_stall_ctx() -> crate::prompts::Ctx<'static> {
        crate::prompts::Ctx {
            lang: Lang::En,
            assistant: false,
            owner: "Sam",
            aliases: &[],
            custom: "",
            status: "",
            stall: crate::gemini_live::StallPrompt::Full,
            knowledge: "",
            about: None,
            carrier_note: false,
            auto_language: true,
        }
    }

    /// The pack's limit on the stalling prompt's region texts
    /// ([bageo_packs::check::STALL_SUM_MAX]: delivery + readback + fraud +
    /// stall_local) keeps the English stalling prompt within its budget with
    /// any region.
    #[test]
    fn the_packs_stall_limit_keeps_the_english_stall_prompt_within_the_budget() {
        let (m_en, _) = bageo_packs::check::STALL_SUM_MAX;
        let bare = crate::prompts::scope_to_region(&crate::prompts::build(&english_stall_ctx()), Lang::En, Default::default());
        let persona = crate::prompts::persona_chars(Lang::En, false, "Sam", "");
        for typology in ["investment_group_scam", "fake_official", "fake_debt_collection", "general_promotion"] {
            let (slim, _) = slim_stall_prompt(&bare, typology);
            let rest = slim.chars().count() - persona;
            let room = STALL_ADDITION_BUDGET - rest - crate::country_profile::stall_overhead(Lang::En);
            println!("STALL_ROOM {typology} rest={rest} room={room} limit={m_en}");
            assert!(m_en <= room, "{typology}: the pack lets in {m_en}, the budget has room for {room}");
        }
    }

    fn all_hints() -> Vec<&'static Hint> {
        LOOP_HINTS
            .iter()
            .chain(&COLLECTION_LOOP)
            .chain(&IMPERSONATION_LOOP)
            .chain(&HARASSMENT_LOOP)
            .chain([&SAID_BEFORE, &CALLER_AGAIN, &QUESTION_RUN_HINT])
            .chain(&RETAIN)
            .chain(&IMPERSONATION_RETAIN)
            .chain(&COLLECTION_RETAIN)
            .chain(&HARASSMENT_RETAIN)
            .collect()
    }

    #[test]
    fn cues_are_short() {
        for lang in crate::locale::ALL {
            for h in all_hints() {
                let t = cue_text(h, lang);
                assert!(t.chars().count() <= MAX_CUE_CHARS, "{lang:?} {} {}", h.0, t.chars().count());
            }
        }
    }

    #[test]
    fn no_hint_while_the_ai_finds_its_own_way() {
        let mut e = StallEngine::new("insurance_sales", Lang::ZhTw);
        let turns = [
            "喔真的嗎？好險你有打來，我最近剛好在想這個",
            "那保費一個月大概多少錢啊？",
            "要繳幾年呢？我想一下我的預算",
            "我上個月才去看過醫生，膝蓋一直不太好",
            "住院的話是一天給多少？手術也有嗎？",
            "你們自己也有買這個嗎？",
        ];
        for (n, said) in turns.iter().enumerate() {
            assert!(e.on_ai_turn(said).is_none(), "turn {n}");
            assert_eq!(e.stage(), Stage::Free);
        }
        assert_eq!(e.turns(), 6);
    }

    #[test]
    fn going_round_in_circles_brings_a_hint_to_change_course() {
        assert!(similarity("那保費一個月多少錢？要繳幾年？", "那保費一個月多少錢？要繳幾年啊？") >= LOOP_SIMILARITY);
        assert!(similarity("那保費一個月多少錢？", "我家的貓今天早上跑出去了") < LOOP_SIMILARITY);
        assert_eq!(similarity("嗯", "嗯"), 0.0);
        let mut e = StallEngine::new("insurance_sales", Lang::ZhTw);
        assert!(e.on_ai_turn("好險你有打來，我正想問保險").is_none());
        let a = e.on_ai_turn("好險你有打來，我正想問保險耶").unwrap();
        assert!(a.looped && a.stage == Stage::Loop && a.text.starts_with(ZH_MARK.get()) && a.text.contains("換"), "{}", a.text);
        assert!(e.on_ai_turn("那等待期要多久？哪些情況不賠？").is_none());
        let b = e.on_ai_turn("那等待期要多久？哪些情況不賠啊？").unwrap();
        assert_ne!(a.tactic, b.tactic, "the hints take turns");
        // Debt collection: the hint never lets go of the debt line.
        let mut c = StallEngine::new("fake_debt_collection", Lang::ZhTw);
        c.on_ai_turn("什麼欠款？我完全沒印象耶");
        let h = c.on_ai_turn("什麼欠款？我完全沒有印象耶").unwrap();
        assert!(h.text.contains("對不上") || h.text.contains("不認債"), "{}", h.text);
    }

    #[test]
    fn a_leaving_caller_brings_one_keep_hint() {
        let mut e = StallEngine::new("investment_group_scam", Lang::ZhTw);
        for n in 0..5 {
            e.on_ai_turn(&format!("不同的句子第{n}號，內容{}", n * 13));
        }
        assert!(e.on_caller_closing());
        assert!(!e.on_caller_closing(), "already owed: not again");
        assert_eq!(e.stage(), Stage::Retain);
        let c = e.on_ai_turn("那最少要放多少？我想先試試看").unwrap();
        assert_eq!((c.stage, c.tactic), (Stage::Retain, "keep"));
        assert!(c.text.contains("留他") && c.text.contains("欸等一下，我還有一個問題"), "{}", c.text);
        // Then the AI is on its own again.
        assert!(e.on_ai_turn("那一個月大概可以賺多少？").is_none());
        // A keep nudge takes it instead: the turn end owes nothing more.
        assert!(e.on_caller_closing());
        let k = e.take_retain();
        assert_eq!(k.tactic, "keep_reason");
        assert!(e.on_ai_turn("好啊那我再想想看").is_none());
    }

    #[test]
    fn keep_hints_fit_the_kind_of_call() {
        for t in ["fake_official", "fake_shop_order_error", "fake_debt_collection", "harassment_intimidation"] {
            let mut e = StallEngine::new(t, Lang::ZhTw);
            for _ in 0..3 {
                let c = e.take_retain();
                assert!(!c.text.contains("要辦") && !c.text.contains("划算") && !c.text.contains("ATM"), "{t} {}", c.text);
            }
        }
        let mut en = StallEngine::new("fake_debt_collection", Lang::En);
        en.on_caller_closing();
        assert!(en.on_ai_turn("what debt?").unwrap().text.contains("admit no debt"));
    }

    #[test]
    fn languages_of_the_cues() {
        let looped = |lang: Lang| {
            let mut e = StallEngine::new("loan_sales", lang);
            e.on_ai_turn("so how much could I borrow at that rate then");
            e.on_ai_turn("so how much could I borrow at that rate then?").unwrap().text
        };
        assert!(looped(Lang::ZhCn).contains("系统小抄"));
        assert!(looped(Lang::Ja).starts_with(EN_OTHER_MARK.get()));
        assert!(looped(Lang::En).starts_with(EN_MARK.get()));
    }

    /// In each language: three replies that have nothing to do with each other,
    /// the first one given again in nearly the same words, then two things a
    /// caller says and the first of those said again as speech-to-text hears it.
    const REPEATS: [(&str, [&str; 7]); 16] = [
        (
            "zh-TW",
            [
                "天哪，怎麼會這樣？我人在外地耶，怎麼會有人報案？",
                "我老花眼鏡找不到，你念慢一點，我拿筆記下來。",
                "附近是有一家超商，可是我膝蓋不好，走路很慢。",
                "天哪，怎麼會這樣？我人在外地耶，怎麼會有人報案啊？",
                "這支門號被拿去發詐騙簡訊，已經有人報案了。",
                "你家附近有沒有超商或是提款機？",
                "這支門號被拿去發詐騙簡訊已經有人報案了",
            ],
        ),
        (
            "zh-CN",
            [
                "天哪，怎么会这样？我人在外地啊，怎么会有人报案？",
                "我老花镜找不到了，你念慢一点，我拿笔记下来。",
                "附近是有一家便利店，可是我膝盖不好，走路很慢。",
                "天哪，怎么会这样？我人在外地啊，怎么会有人报案呢？",
                "这个号码被拿去发诈骗短信，已经有人报案了。",
                "你家附近有没有便利店或者取款机？",
                "这个号码被拿去发诈骗短信已经有人报案了",
            ],
        ),
        (
            "en",
            [
                "Oh no, really? Which number is it? I only use this one phone.",
                "Hold on, let me find a pen, my glasses are somewhere around here.",
                "There's a corner store nearby, but my knee is bad, I walk slowly.",
                "Oh no, really? Which number is it? I only use this one phone, you know.",
                "That number was used to send scam texts, and there is a police report on it.",
                "Is there a store or an ATM near where you are?",
                "That number was used to send scam texts and there's a police report on it",
            ],
        ),
        (
            "ja",
            [
                "えっ、本当ですか？どの番号ですか？この携帯しか使っていないんですが。",
                "ちょっと待ってください、ペンを探します。眼鏡が見つからなくて。",
                "近くにコンビニはありますが、膝が悪くて歩くのが遅いんです。",
                "えっ、本当ですか？どの番号ですか？この携帯しか使ってないんですけど。",
                "その番号が詐欺メッセージの送信に使われて、被害届が出ています。",
                "お近くにコンビニかATMはありますか？",
                "その番号が詐欺メッセージの送信に使われて被害届が出ています",
            ],
        ),
        (
            "ko",
            [
                "어머, 정말요? 어느 번호예요? 저는 이 전화만 쓰는데요.",
                "잠깐만요, 펜 좀 찾을게요. 안경이 어디 있는지 모르겠네요.",
                "근처에 편의점은 있는데 무릎이 안 좋아서 천천히 걸어요.",
                "어머 정말요? 어느 번호예요? 저는 이 전화만 쓰는데요",
                "그 번호가 사기 문자를 보내는 데 쓰여서 신고가 접수됐습니다.",
                "근처에 편의점이나 현금인출기가 있습니까?",
                "그 번호가 사기 문자를 보내는 데 쓰여서 신고가 접수됐습니다",
            ],
        ),
        (
            "es",
            [
                "¿De verdad? ¿Qué número es? Yo solo uso este teléfono.",
                "Espere, voy a buscar un bolígrafo, no encuentro mis gafas.",
                "Hay una tienda cerca, pero tengo mal la rodilla y camino despacio.",
                "¿De verdad? ¿Y qué número es? Yo solo uso este teléfono.",
                "Ese número se usó para enviar mensajes de estafa y ya hay una denuncia.",
                "¿Hay una tienda o un cajero cerca de donde está?",
                "Ese número se usó para enviar mensajes de estafa y ya hay una denuncia",
            ],
        ),
        (
            "fr",
            [
                "Ah bon, vraiment ? C'est quel numéro ? Je n'utilise que ce téléphone.",
                "Attendez, je cherche un stylo, mes lunettes sont quelque part par ici.",
                "Il y a une épicerie pas loin, mais j'ai mal au genou, je marche lentement.",
                "Ah bon, vraiment ? C'est quel numéro ? Moi je n'utilise que ce téléphone.",
                "Ce numéro a servi à envoyer des messages frauduleux et une plainte a été déposée.",
                "Y a-t-il un magasin ou un distributeur près de chez vous ?",
                "Ce numéro a servi à envoyer des messages frauduleux, une plainte a été déposée",
            ],
        ),
        (
            "de",
            [
                "Ach wirklich? Welche Nummer denn? Ich benutze nur dieses eine Telefon.",
                "Moment, ich suche einen Stift, meine Brille ist irgendwo hier.",
                "Es gibt einen Laden in der Nähe, aber mein Knie ist schlecht, ich gehe langsam.",
                "Ach wirklich? Welche Nummer denn? Ich benutze doch nur dieses eine Telefon.",
                "Mit dieser Nummer wurden Betrugsnachrichten verschickt, es liegt eine Anzeige vor.",
                "Gibt es einen Laden oder einen Geldautomaten in Ihrer Nähe?",
                "Mit dieser Nummer wurden Betrugsnachrichten verschickt und es liegt eine Anzeige vor",
            ],
        ),
        (
            "pt",
            [
                "É mesmo? Qual é o número? Eu só uso este telefone.",
                "Espere, vou procurar uma caneta, não acho meus óculos.",
                "Tem uma loja aqui perto, mas meu joelho está ruim, ando devagar.",
                "É mesmo? E qual é o número? Eu só uso este telefone.",
                "Esse número foi usado para enviar mensagens de golpe e já existe uma queixa.",
                "Tem alguma loja ou caixa eletrônico perto de você?",
                "Esse número foi usado para enviar mensagens de golpe, já existe uma queixa",
            ],
        ),
        (
            "it",
            [
                "Davvero? Che numero è? Io uso soltanto questo telefono.",
                "Aspetti, cerco una penna, non trovo i miei occhiali.",
                "C'è un negozio qui vicino, ma ho male al ginocchio e cammino piano.",
                "Davvero? E che numero è? Io uso soltanto questo telefono.",
                "Quel numero è stato usato per inviare messaggi truffa e c'è già una denuncia.",
                "C'è un negozio o un bancomat vicino a lei?",
                "Quel numero è stato usato per inviare messaggi truffa, c'è già una denuncia",
            ],
        ),
        (
            "ru",
            [
                "Правда? А какой это номер? Я пользуюсь только этим телефоном.",
                "Подождите, я найду ручку, очки куда-то пропали.",
                "Рядом есть магазин, но у меня болит колено, я хожу медленно.",
                "Правда? А какой это номер? Я ведь пользуюсь только этим телефоном.",
                "С этого номера рассылали мошеннические сообщения, уже есть заявление в полицию.",
                "Рядом с вами есть магазин или банкомат?",
                "С этого номера рассылали мошеннические сообщения и уже есть заявление в полицию",
            ],
        ),
        (
            "id",
            [
                "Masa sih? Nomor yang mana? Saya cuma pakai telepon yang ini.",
                "Tunggu sebentar, saya cari pulpen dulu, kacamata saya entah di mana.",
                "Ada warung dekat sini, tapi lutut saya sakit, jalannya pelan.",
                "Masa sih? Nomor yang mana ya? Saya cuma pakai telepon yang ini.",
                "Nomor itu dipakai untuk mengirim pesan penipuan dan sudah ada laporan polisi.",
                "Apakah ada toko atau ATM di dekat Anda?",
                "Nomor itu dipakai untuk mengirim pesan penipuan, sudah ada laporan polisi",
            ],
        ),
        (
            "vi",
            [
                "Thật vậy sao? Số nào vậy? Tôi chỉ dùng mỗi cái điện thoại này thôi.",
                "Chờ chút, để tôi tìm cây bút, kính của tôi không biết để đâu rồi.",
                "Gần đây có một cửa hàng, nhưng đầu gối tôi đau nên đi chậm lắm.",
                "Thật vậy sao? Số nào vậy ạ? Tôi chỉ dùng mỗi cái điện thoại này thôi.",
                "Số đó đã bị dùng để gửi tin nhắn lừa đảo và đã có người trình báo công an.",
                "Gần chỗ anh có cửa hàng hay cây ATM nào không?",
                "Số đó đã bị dùng để gửi tin nhắn lừa đảo, đã có người trình báo công an",
            ],
        ),
        (
            "th",
            [
                "จริงเหรอครับ เบอร์ไหนครับ ผมใช้แค่เครื่องนี้เครื่องเดียว",
                "รอสักครู่นะครับ ผมหาปากกาก่อน แว่นไม่รู้อยู่ไหน",
                "แถวนี้มีร้านสะดวกซื้อครับ แต่เข่าผมไม่ดี เดินช้า",
                "จริงเหรอครับ เบอร์ไหนครับ ผมใช้แค่เครื่องนี้เครื่องเดียวเอง",
                "เบอร์นั้นถูกใช้ส่งข้อความหลอกลวง และมีคนแจ้งความแล้ว",
                "ใกล้ ๆ คุณมีร้านค้าหรือตู้เอทีเอ็มไหม",
                "เบอร์นั้นถูกใช้ส่งข้อความหลอกลวงและมีคนแจ้งความแล้วครับ",
            ],
        ),
        (
            "ar",
            [
                "حقاً؟ أي رقم هذا؟ أنا أستخدم هذا الهاتف فقط.",
                "انتظر لحظة، سأبحث عن قلم، لا أجد نظارتي.",
                "يوجد متجر قريب، لكن ركبتي تؤلمني وأمشي ببطء.",
                "حقاً؟ وأي رقم هذا؟ أنا أستخدم هذا الهاتف فقط.",
                "هذا الرقم استُخدم لإرسال رسائل احتيال وهناك بلاغ لدى الشرطة.",
                "هل يوجد متجر أو صراف آلي بالقرب منك؟",
                "هذا الرقم استخدم لإرسال رسائل احتيال، وهناك بلاغ لدى الشرطة",
            ],
        ),
        (
            "hi",
            [
                "सच में? कौन सा नंबर है? मैं तो बस यही फ़ोन इस्तेमाल करता हूँ।",
                "रुकिए, मैं पेन ढूँढता हूँ, मेरा चश्मा नहीं मिल रहा।",
                "पास में एक दुकान है, पर मेरे घुटने में दर्द है, मैं धीरे चलता हूँ।",
                "सच में? और कौन सा नंबर है? मैं तो बस यही फ़ोन इस्तेमाल करता हूँ।",
                "उस नंबर से धोखाधड़ी के संदेश भेजे गए हैं और पुलिस में शिकायत दर्ज है।",
                "क्या आपके पास कोई दुकान या एटीएम है?",
                "उस नंबर से धोखाधड़ी के संदेश भेजे गए हैं, पुलिस में शिकायत दर्ज है",
            ],
        ),
    ];

    #[test]
    fn an_earlier_reply_given_again_brings_a_hint_in_every_language() {
        for (lang, [a, b, c, a_again, ..]) in REPEATS {
            let mut e = StallEngine::new("fake_telecom", Lang::En);
            for said in [a, b, c] {
                assert!(e.on_ai_turn(said).is_none(), "{lang}: {said}");
            }
            let cue = e.on_ai_turn(a_again).unwrap_or_else(|| panic!("{lang}: {a_again}"));
            assert_eq!((cue.stage, cue.tactic, cue.looped), (Stage::Loop, "said_before", true), "{lang}");
            assert_eq!(e.stage(), Stage::Loop);
        }
    }

    #[test]
    fn a_caller_saying_it_again_brings_a_hint_in_every_language() {
        for (lang, [a, b, c, _, x, y, x_again]) in REPEATS {
            let mut e = StallEngine::new("fake_telecom", Lang::En);
            e.on_caller_turn(x);
            assert!(e.on_ai_turn(a).is_none(), "{lang}");
            e.on_caller_turn(y);
            assert!(e.on_ai_turn(b).is_none(), "{lang}: {y}");
            e.on_caller_turn(x_again);
            let cue = e.on_ai_turn(c).unwrap_or_else(|| panic!("{lang}: {x_again}"));
            assert_eq!((cue.stage, cue.tactic), (Stage::Loop, "caller_again"), "{lang}");
        }
        // The hint is in the language of the call's script, marked like the others.
        let hint = |lang: Lang| {
            let mut e = StallEngine::new("fake_telecom", lang);
            e.on_caller_turn(REPEATS[0].1[4]);
            e.on_caller_turn(REPEATS[0].1[6]);
            e.on_ai_turn(REPEATS[0].1[0]).unwrap().text
        };
        assert!(hint(Lang::ZhTw).starts_with(ZH_MARK.get()) && hint(Lang::ZhTw).contains("講過"), "{}", hint(Lang::ZhTw));
        assert!(hint(Lang::ZhCn).contains("讲过"), "{}", hint(Lang::ZhCn));
        assert!(hint(Lang::En).starts_with(EN_MARK.get()));
        assert!(hint(Lang::Ja).starts_with(EN_OTHER_MARK.get()));
    }

    /// The call this was written for: the caller's whole pitch comes round a
    /// second time and the AI answers every point as it did the first time.
    #[test]
    fn a_pitch_that_comes_round_again_is_noticed_but_not_at_every_turn() {
        let pitch = [
            ("這支門號被拿去發詐騙簡訊，已經有人報案了。", "天哪，怎麼會這樣？我人在外地耶，怎麼會有人報案？"),
            ("我們要把你的錢先轉到監管帳戶保護起來。", "監管帳戶喔？聽起來好複雜，我不太會操作耶。"),
            ("你如果不配合，明天就會收到法院的傳票。", "傳票？拜託不要，我一定配合，你一步一步教我。"),
            ("你家附近有沒有超商或是提款機？", "附近是有一家超商，可是我膝蓋不好，走路很慢。"),
            ("這是偵查不公開，你不可以跟家人講。", "好，我誰都不講，連我太太都不會說。"),
            ("你先拿紙筆，我念一組案號給你。", "我老花眼鏡找不到，你念慢一點，我拿筆記下來。"),
        ];
        let mut e = StallEngine::new("fake_telecom", Lang::ZhTw);
        for (caller, ai) in pitch {
            e.on_caller_turn(caller);
            assert!(e.on_ai_turn(ai).is_none(), "first time round: {ai}");
        }
        let second: Vec<Option<&str>> = pitch
            .iter()
            .map(|(caller, ai)| {
                e.on_caller_turn(caller);
                e.on_ai_turn(ai).map(|c| c.tactic)
            })
            .collect();
        assert_eq!(second, [Some("caller_again"), None, None, Some("caller_again"), None, None]);
        // The caller moves on but the AI falls back on an old reply: that alone is named.
        e.on_caller_turn("那你現在把身分證字號念給我聽。");
        e.on_ai_turn("等一下喔，我這邊收訊不太好，你剛剛說什麼？");
        e.on_caller_turn("我說身分證字號，你聽得到嗎？聽到就回答。");
        assert_eq!(e.on_ai_turn(pitch[1].1).map(|c| c.tactic), Some("said_before"));
    }

    #[test]
    fn short_lines_said_again_are_nothing_to_notice() {
        let replies = [
            "天哪，怎麼會這樣？我人在外地耶，怎麼會有人報案？",
            "監管帳戶喔？聽起來好複雜，我不太會操作耶。",
            "傳票？拜託不要，我一定配合，你一步一步教我。",
            "附近是有一家超商，可是我膝蓋不好，走路很慢。",
            "好，我誰都不講，連我太太都不會說。",
            "我老花眼鏡找不到，你念慢一點，我拿筆記下來。",
        ];
        let callers = ["喂？你還在嗎？", "Hello? Are you there?", "喂？你還在嗎？", "Hello? Are you there?", "여보세요?", "喂？你還在嗎？"];
        let mut e = StallEngine::new("fake_telecom", Lang::ZhTw);
        for (caller, reply) in callers.iter().zip(replies) {
            e.on_caller_turn(caller);
            assert!(e.on_ai_turn(reply).is_none(), "{caller}");
        }
        let mut lines = VecDeque::new();
        for n in 0..MEMORY_TURNS + 5 {
            remember(&mut lines, &format!("這是第{n}句夠長的話，留著給後面的話比對"));
        }
        assert_eq!(lines.len(), MEMORY_TURNS);
        assert!(lines.front().unwrap().contains("第5句"), "the oldest go first");
        remember(&mut lines, "好好好");
        assert_eq!(lines.len(), MEMORY_TURNS, "too short to keep");
    }

    #[test]
    fn reply_after_reply_ending_in_a_question_brings_a_hint_to_end_another_way() {
        for q in ["那要怎麼辦？", "So what do I do?", "¿Y ahora qué hago?", "ماذا أفعل الآن؟", "それで、どうすれば？」", "Really?!", "Et maintenant ? "] {
            assert!(ends_with_question(q), "{q}");
        }
        for s in ["好，我知道了。", "Okay, I see.", "What? No, wait", "…嗯", ""] {
            assert!(!ends_with_question(s), "{s}");
        }
        let asks = [
            "那保費一個月大概多少錢啊？",
            "要繳幾年呢？繳完之後錢拿得回來嗎？",
            "住院的話是一天給多少？手術也有嗎？",
            "你們自己也有買這個嗎？",
            "那等待期要多久？哪些情況不賠？",
        ];
        let mut e = StallEngine::new("insurance_sales", Lang::ZhTw);
        for said in &asks[..4] {
            assert!(e.on_ai_turn(said).is_none(), "{said}");
        }
        let cue = e.on_ai_turn(asks[4]).unwrap();
        assert_eq!((cue.stage, cue.tactic), (Stage::Loop, "question_run"));
        assert!(cue.text.contains("收尾"), "{}", cue.text);
        // A reply that does not ask breaks the run; the count starts over after a hint.
        let en = [
            "So how much would that be a month?",
            "And how many years do I pay for?",
            "Does it cover a hospital stay too?",
            "Do you have this plan yourself?",
            "Hold on, my tea is boiling over.",
            "What happens if I miss a payment?",
            "Is there a waiting period at all?",
            "Can my wife be on it as well?",
            "Could you send me that in writing?",
        ];
        let mut e = StallEngine::new("insurance_sales", Lang::En);
        for said in en {
            assert!(e.on_ai_turn(said).is_none(), "{said}");
        }
        let cue = e.on_ai_turn("And who do I call if something goes wrong?").unwrap();
        assert_eq!(cue.tactic, "question_run");
        assert!(e.on_ai_turn("Would my son be covered as well, then?").is_none());
    }

    /// Words that hand the AI a fixed line to recite.
    const SCRIPTED: [&str; 19] = [
        "必須說", "必须说", "一定要說", "一定要说", "照這句", "照这句", "照著說", "照着说", "照念", "逐字", "一字不改",
        "台詞", "台词", "下一句：", "must say", "say exactly", "word for word", "verbatim", "use this line",
    ];

    /// The call texts this plan added give a situation, a direction and the
    /// lines not to cross; the AI finds its own words. None of them hands it a
    /// line to recite.
    #[test]
    fn no_call_text_hands_the_ai_a_line_to_recite() {
        use crate::scenario::Scenario;
        let mut texts: Vec<String> = Vec::new();
        for lang in crate::locale::ALL {
            texts.extend(ALL_FAMILIES.iter().filter_map(|f| script(*f, lang)));
            texts.push(compact_block(lang, Family::General));
            texts.extend(all_hints().into_iter().map(|h| cue_text(h, lang)));
            for s in Scenario::ALL {
                for booking in [false, true] {
                    texts.extend(crate::scenario::cue(s, lang, None, booking));
                }
            }
            for r in crate::country_profile::all() {
                texts.push(crate::country_profile::block(r, lang, false));
                texts.push(crate::country_profile::habits_block(r, lang));
                texts.push(crate::country_profile::stall_line(r, lang));
                for level in 1..=5 {
                    for assistant in [false, true] {
                        texts.push(crate::country_profile::style_line(r, lang, assistant, level, "Alex"));
                    }
                }
                texts.push(crate::prompts::dialect_note(r, lang));
            }
            let answers = [("phone".to_string(), "0912345678".to_string())];
            texts.push(crate::preflight::prompt_block("Book at 7 pm, otherwise 8:30 pm", &answers, lang));
        }
        for e in crate::locale::Ext::all() {
            let l = crate::locale::Lang::Ext(e);
            for r in crate::country_profile::all() {
                if crate::country_profile::has_lang_override(r, l) {
                    texts.push(crate::country_profile::habits_block(r, l));
                    for level in 1..=5 {
                        for assistant in [false, true] {
                            texts.push(crate::country_profile::style_line(r, l, assistant, level, "Alex"));
                        }
                    }
                }
            }
        }
        assert!(texts.len() > 500, "{}", texts.len());
        for t in &texts {
            let low = t.to_lowercase();
            for bad in SCRIPTED {
                assert!(!low.contains(bad), "{bad}: {t}");
            }
        }
        // Chinese hints quote no line but the owner's own example of the feel.
        for h in all_hints() {
            assert!(!h.1.get().contains('「') || h.1.get().contains("「欸等一下，我還有一個問題」"), "{}", h.1);
        }
    }

    #[test]
    fn the_note_asks_for_no_reply() {
        let v: serde_json::Value = serde_json::from_str(&client_note("x")).unwrap();
        assert_eq!(v["clientContent"]["turnComplete"], serde_json::json!(false));
        assert_eq!(v["clientContent"]["turns"][0]["parts"][0]["text"], "x");
    }
}


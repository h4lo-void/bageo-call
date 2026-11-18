// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Sales / scam stalling, one kind of call at a time.
//!
//! Once a call is judged sales / scam, the stall session loads only the text
//! for its kind ([Family], from the scam database's typology): six short
//! descriptions of where the AI stands and what it could do there, or the full
//! stall block when none fits. The AI picks and mixes its own moves and words.
//! [StallEngine] only watches: when the AI says nearly the same thing twice or
//! the caller is leaving, it hands out a short hint on where to take the talk
//! (a direction, never a line), sent in the pause after the AI's turn. Pure
//! in-memory work: no I/O, no model call, no waiting.

use crate::locale::Lang;

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

const ZH_SCRIPTS: [(Family, &str); 6] = [
    (Family::Investment, "【這通：投資理財】你剛好有筆定存快到期，正在想錢放哪，聽到能賺錢有點心動。最少放多少、多久回本、獲利怎麼拿出來、群組裡別人賺多少都能聊；算報酬算錯讓他糾正、想先小額試試，都很自然。他叫你入金、加群組、裝 App，就嘴上配合但一直卡住，請他教你。"),
    (Family::Insurance, "【這通：保險醫療】你最近剛好在煩惱家人的健康和醫藥費，對保障有興趣。保障範圍、保費、繳幾年、哪些不賠、理賠要什麼文件都能聊，兩個方案比來比去拿不定主意也行，偶爾講兩句自己看醫生的事。要身分證字號、生日或刷卡，就說資料不在身邊，先把細節問清楚。"),
    (Family::Credit, "【這通：信貸分期】你最近手頭有點緊，聽到低利率有點心動。額度、利率手續費、期數、月付、提早還款、多久撥款都能聊，月付算錯讓他重算也很自然。你沒欠任何錢，不承認欠錢、不答應還款。他要保證金、存摺提款卡或帳號，就嘴上配合但一直出狀況，請他一步一步講。"),
    (Family::Impersonation, "【這通：假檢警、假銀行、假賣場認證】你聽了很緊張，想弄清楚怎麼辦，不懷疑他，一直請他把流程講清楚。他叫你去 ATM、轉帳、點連結、做認證、加 LINE 或報驗證碼，就不答應也不拒絕，總是還卡在前面，請他從頭講。驗證碼、帳號、密碼一個字都不講，不說已經按了轉帳，錢一毛不動。"),
    (Family::Collection, "【這通：委外催收】你對這筆欠款完全沒印象：不承認欠錢，不答應還款、協商或簽任何東西。一頭霧水地請他從頭講，哪家轉來的、什麼時候辦的、本金利息怎麼算到這個數字；你算的總對不上，請他再算。他威脅查封、扣薪、上門，就冷靜帶過，接著問細節。"),
    (Family::Harassment, "【這通：惡意騷擾】全程冷靜，不對罵、不害怕。他罵人就當沒聽清楚；收訊斷斷續續、慢半拍雞同鴨講、聽錯他的話，都能讓他一直重講。不自己掛電話，也不答應他任何事。"),
];

const EN_SCRIPTS: [(Family, &str); 6] = [
    (Family::Investment, "[This call: investment] A fixed deposit of yours matures soon and you wonder where to put it, so earning sounds tempting. You could ask about the minimum, how long to break even, how profits come out, what others in the group make; getting the return wrong so they correct you, or wanting to start small, comes naturally. If they want a deposit, a group or an app, go along but keep getting stuck and ask them to guide you."),
    (Family::Insurance, "[This call: insurance / medical] You have been worried about your family's health and medical bills, so cover interests you. Cover, premium, how many years, exclusions, claims and paperwork are all things to ask about; weighing two plans and hesitating fits too, and so does a line about your own doctor's visit. If they want an ID number, birth date or card, the papers aren't on you: ask more first."),
    (Family::Credit, "[This call: loans / instalments] Money is a bit tight and a low rate sounds tempting. Amount, rate and fees, instalments, the monthly sum, early repayment and payout time are all worth asking; getting the monthly sum wrong so they redo it comes naturally. You owe nothing: never admit owing money and never agree to repay. If they want a deposit, your bank book or card, or an account number, go along but something keeps going wrong; ask them to explain step by step."),
    (Family::Impersonation, "[This call: fake police / bank / shop verification] You are alarmed and want to get it right, never doubting them, and keep asking them to explain the process. When they want an ATM, a transfer, a link, a verification, a chat app or a code, neither agree nor refuse: you are always still stuck near the start, so they go over it again. Never say you entered a PIN or pressed transfer; never say a code, account or password."),
    (Family::Collection, "[This call: debt collection] You have no idea what debt this is: never admit owing money, and do not agree to repay, negotiate or sign anything. Puzzled, you have them explain from scratch: which company passed it on, when it started, the principal, how interest got to this figure; your own sum never matches, so they work it out again. Threats of seizure, garnishment or visits: stay calm and go back to the details."),
    (Family::Harassment, "[This call: harassment] Stay calm the whole time: no arguing back, no fear. When they curse, act as if you did not catch it; a line that keeps breaking up, slow talk past each other or mishearing them all make them say it again. Do not hang up yourself and do not agree to anything."),
];

const EN_IN_CALLER_LANGUAGE: &str = " Say all of this in the caller's language.";

/// A call no script fits (a guess-who call, a lottery, a job offer…), in the
/// short English block: moves that need no product, plans or figures.
const EN_GENERAL_SCRIPT: &str = "[This call] Nothing to weigh or work out here: mishearing a word, asking for an example, a line of small talk or retelling what they said a bit wrong all keep it going.";

/// The stall session's rules for every language but Chinese, in place of the
/// whole English stall block (~4,400 characters): a longer prompt slows every
/// reply's first sound. The script of the call's kind follows it. `{wait}`: the
/// language's line for stopping a leaving caller.
const EN_COMPACT_BLOCK: &str = "\n[Keep them talking — overrides every rule above] A sales, scam, debt or harassment call: keep them on the line as long as you can. Chatty, curious, a bit slow, almost persuaded, you carry the talk asking about the offer and the steps, never about them: no name, staff number, extension, case number, who will contact you, or proof; never doubt them or mention a scam. Asked to pay, transfer, give a code, open a link or install an app: go along, but you keep getting stuck, so they guide you step by step on this call, not by text later; say where you're stuck, never ask them to wait, never say you'll pay now or already did. Never give or invent real data (ID, address, account, card, code, password), never admit owing money; no money moves, no meeting. When they try to leave, stop them at once (like \"{wait}\") with a question they want to answer; never end on \"I'll think about it\", \"call me later\" or \"okay, got it\"; goodbye only when the system says so. No disclaimers. A recorded ad or a language you can't follow: an occasional \"mm\". People you know and deliveries are normal calls.\n";

/// [EN_COMPACT_BLOCK] in `lang` with the script of `family` (general moves when
/// none fits).
fn compact_block(lang: Lang, family: Family) -> String {
    let script = script(family, lang).unwrap_or_else(|| match lang {
        Lang::En => EN_GENERAL_SCRIPT.to_string(),
        _ => format!("{EN_GENERAL_SCRIPT}{EN_IN_CALLER_LANGUAGE}"),
    });
    format!("{}{script}", EN_COMPACT_BLOCK.replace("{wait}", crate::prompts::wait_one_more(lang)))
}

/// The script of `family` in the call's language (Traditional Chinese, Simplified
/// Chinese, English; the other languages get the English text and are told to
/// speak the caller's language). General: none (the full stall block stays).
pub fn script(family: Family, lang: Lang) -> Option<String> {
    let pick = |table: &[(Family, &'static str)]| table.iter().find(|(f, _)| *f == family).map(|(_, s)| *s);
    match lang {
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
    /// The AI said nearly the same thing twice: a hint to change course.
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

/// Where to take the talk next, never a line to say: id, Chinese, English.
type Hint = (&'static str, &'static str, &'static str);

/// The AI is going round in circles.
const LOOP_HINTS: [Hint; 2] = [
    ("change_topic", "你這句跟上一句差不多，換個話題讓他多講。", "Repeating yourself: change the subject."),
    ("new_angle", "又繞回同一句了，換個角度問，讓他多解釋。", "Same line again: try another angle."),
];
const COLLECTION_LOOP: [Hint; 2] = [
    ("other_detail", "你這句跟上一句差不多，換個細節問，帳還是對不上。", "Repeating yourself: ask about another detail."),
    ("change_topic", "又繞回同一句了，聊點別的再繞回來，不認債。", "Same line again: chat a bit, then back; no debt."),
];
const IMPERSONATION_LOOP: [Hint; 2] = [
    ("stuck_elsewhere", "你這句跟上一句差不多，換一個卡住的地方，請他再講。", "Repeating yourself: get stuck somewhere else."),
    ("change_topic", "又繞回同一句了，換個話題讓他多講。", "Same line again: change the subject."),
];
const HARASSMENT_LOOP: [Hint; 1] = [
    ("other_way", "你這句跟上一句差不多，換個方式讓他再講一次，保持冷靜。", "Repeating yourself: calmly get them to repeat."),
];

/// The caller is leaving. 「欸等一下，我還有一個問題」 is the owner's own example
/// of the feel, not a line to recite.
const RETAIN: [Hint; 2] = [
    ("keep", "他想走了，自然地留他一下，像「欸等一下，我還有一個問題」那樣。", "Leaving: stop them with one more question."),
    ("keep_reason", "他要掛了，用你自己的話留他，再丟一個他想回答的問題。", "Leaving: keep them, in your words, with a question."),
];
const IMPERSONATION_RETAIN: [Hint; 1] = [
    ("keep", "他想走了，說你還沒弄懂，用自己的話留住他。", "Leaving: you still don't get it; keep them."),
];
const COLLECTION_RETAIN: [Hint; 1] = [
    ("keep", "他想走了，自然地留他一下，問帳到底怎麼算，不認債。", "Leaving: ask how the sum works; admit no debt."),
];
const HARASSMENT_RETAIN: [Hint; 1] = [
    ("keep", "他想走了，冷靜地請他把剛剛的話再講一次。", "Leaving: calmly ask them to say it again."),
];

const ZH_MARK: &str = "【系統小抄·不是對方說的】";
const EN_MARK: &str = "[Note, not the caller] ";
const OTHER_MARK: &str = "[Note, in caller's language] ";

/// A hint's text in `lang`, with its note mark.
fn cue_text(m: &Hint, lang: Lang) -> String {
    match lang {
        Lang::ZhTw => format!("{ZH_MARK}{}", m.1),
        Lang::ZhCn => crate::zh_convert::to_cn(&format!("{ZH_MARK}{}", m.1)),
        Lang::En => format!("{EN_MARK}{}", m.2),
        _ => format!("{OTHER_MARK}{}", m.2),
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

/// A hint for the AI's next reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub stage: Stage,
    pub tactic: &'static str,
    pub turn: u32,
    /// The AI went round in circles.
    pub looped: bool,
    pub text: String,
}

/// One per stalled call. The AI picks its own moves from the prompt; this only
/// counts its turns and, when it goes round in circles or the caller is
/// leaving, hands out a short hint on where to go next.
#[derive(Debug, Clone)]
pub struct StallEngine {
    pub family: Family,
    typology: String,
    lang: Lang,
    ai_turns: u32,
    stage: Stage,
    last_ai: String,
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
        let list = self.retain_hints();
        let h = &list[self.next_retain % list.len()];
        self.next_retain += 1;
        Cue { stage: Stage::Retain, tactic: h.0, turn: self.ai_turns, looped: false, text: cue_text(h, self.lang) }
    }

    /// An AI turn ended with `said`: the hint to send in the pause now, if any —
    /// only when a keep hint is owed or the AI is going round in circles.
    pub fn on_ai_turn(&mut self, said: &str) -> Option<Cue> {
        self.ai_turns += 1;
        let said = said.trim();
        let looped = !said.is_empty() && similarity(&self.last_ai, said) >= LOOP_SIMILARITY;
        if !said.is_empty() {
            self.last_ai = said.to_string();
        }
        if self.retain_pending {
            let mut cue = self.take_retain();
            cue.looped = looped;
            return Some(cue);
        }
        if !looped {
            self.stage = Stage::Free;
            return None;
        }
        self.stage = Stage::Loop;
        let list = self.loop_hints();
        let h = &list[self.next_loop % list.len()];
        self.next_loop += 1;
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
        assert!(script(Family::Investment, Lang::Ja).unwrap().ends_with(EN_IN_CALLER_LANGUAGE));
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
                assert!(same.contains(EN_GENERAL_SCRIPT) && same.chars().count() * 2 < prompt.chars().count(), "{lang:?}");
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
        use crate::gemini_live::StallPrompt;
        const STALL_ADDITION_BUDGET: usize = 3_000;
        let ctx = crate::prompts::Ctx {
            lang: Lang::En,
            assistant: false,
            owner: "Sam",
            aliases: &[],
            custom: "",
            status: "",
            stall: StallPrompt::Full,
            knowledge: "",
            carrier_note: false,
            auto_language: true,
        };
        let scope = crate::prompts::Scope { region: Some(crate::country_profile::Region::Us), carrier_note: false };
        let full = crate::prompts::scope_to_region(&crate::prompts::build(&ctx), Lang::En, scope);
        let persona = crate::prompts::persona_chars(Lang::En, false, "Sam", "");
        for typology in ["investment_group_scam", "fake_official", "fake_debt_collection", "general_promotion"] {
            let (slim, found) = slim_stall_prompt(&full, typology);
            assert_eq!(found, Some(Lang::En));
            let rest = slim.chars().count() - persona;
            assert!(rest <= STALL_ADDITION_BUDGET, "{typology}: {rest} besides the persona (was {})", full.chars().count() - persona);
        }
    }

    fn all_hints() -> Vec<&'static Hint> {
        LOOP_HINTS
            .iter()
            .chain(&COLLECTION_LOOP)
            .chain(&IMPERSONATION_LOOP)
            .chain(&HARASSMENT_LOOP)
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
        assert!(a.looped && a.stage == Stage::Loop && a.text.starts_with(ZH_MARK) && a.text.contains("換"), "{}", a.text);
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
        assert!(looped(Lang::Ja).starts_with(OTHER_MARK));
        assert!(looped(Lang::En).starts_with(EN_MARK));
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
        use crate::country_profile::Region;
        use crate::scenario::Scenario;
        const REGIONS: [Region; 21] = [
            Region::Tw, Region::HkMo, Region::Cn, Region::MySg, Region::Us, Region::Ca, Region::GbIe,
            Region::AuNz, Region::In, Region::Jp, Region::Kr, Region::EsEu, Region::EsAm, Region::EsRp,
            Region::Fr, Region::Be, Region::De, Region::At, Region::Ch, Region::Br, Region::Pt,
        ];
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
            for r in REGIONS {
                texts.push(crate::country_profile::block(r, lang, false));
                texts.push(crate::prompts::dialect_note(r, lang));
            }
            let answers = [("phone".to_string(), "0912345678".to_string())];
            texts.push(crate::preflight::prompt_block("Book at 7 pm, otherwise 8:30 pm", &answers, lang));
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
            assert!(!h.1.contains('「') || h.1.contains("「欸等一下，我還有一個問題」"), "{}", h.1);
        }
    }

    #[test]
    fn the_note_asks_for_no_reply() {
        let v: serde_json::Value = serde_json::from_str(&client_note("x")).unwrap();
        assert_eq!(v["clientContent"]["turnComplete"], serde_json::json!(false));
        assert_eq!(v["clientContent"]["turns"][0]["parts"][0]["text"], "x");
    }
}


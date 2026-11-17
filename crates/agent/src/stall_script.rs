//! Sales / scam stalling, one kind of call at a time.
//!
//! Once a call is judged sales / scam, the stall session loads only the script
//! for its kind ([`Family`], from the scam database's typology): six short
//! scripts, or the full stall block when none fits. While stalling,
//! [`StallEngine`] paces the call locally — bait (first AI turns: keen, never
//! doubting), stall (one move per turn, never the same twice in a row, changed
//! early when the AI repeats itself) and retain (the caller announces the end
//! or says bye: a last bait line) — and hands out a short cue for the AI's next
//! reply, sent in the pause after its turn. Pure in-memory work: no I/O, no
//! model call, no waiting.

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
    (Family::Investment, "【這通的劇本：投資理財】你剛好有一筆定存快到期，正煩惱錢要放哪裡，聽到能賺錢很心動。可以聊：最少要放多少、一個月大概賺多少、多久能回本、賺的錢怎麼拿出來、群組裡的人都賺多少；自己算報酬故意算錯讓他糾正；說想先放小額試試，再問加碼的方案怎麼算。他叫你入金、加群組、下載 App：嘴上說好，可是一直卡（App 載不下來、註冊收不到簡訊、轉帳頁面找不到），請他一步一步教你。"),
    (Family::Insurance, "【這通的劇本：保險醫療】你最近剛好在擔心家人的健康和醫藥費，對保障很有興趣。可以聊：保障範圍、保費一個月多少、要繳幾年、等待期多久、哪些情況不賠、住院跟手術怎麼理賠、要準備什麼文件、解約會不會虧；拿兩個方案比來比去拿不定主意；講一兩句自己看醫生的小事再繞回來。他要你給身分證字號、生日或刷卡：說資料不在身邊，先把細節問清楚。"),
    (Family::Credit, "【這通的劇本：信貸分期】你最近手頭有點緊，聽到低利率很心動。可以聊：最多能借多少、利率跟手續費怎麼算、要繳幾期、每個月繳多少、提早還清有沒有違約金、多久撥款；自己算月付金故意算錯讓他重算；比較兩種期數拿不定主意。不要承認自己欠任何錢、不答應還款。他要你先繳保證金、寄存摺提款卡或給帳號：嘴上說好，可是一直出狀況（存摺找不到、郵局排很長），請他一步一步講。"),
    (Family::Impersonation, "【這通的劇本：假檢警／假銀行／假賣場認證】你聽了很緊張、很想弄清楚（「真的假的？那現在怎麼辦？」），不懷疑他，一直請他把流程講清楚。他叫你去 ATM、轉帳、點連結、做賣場認證、加 LINE 或給驗證碼：不要答應也不要拒絕，說你還卡在最前面（提款卡找不到、眼鏡沒帶、連結打不開、簡訊沒收到、看不懂他說的畫面），請他從頭再講一次。永遠不說已經輸入密碼、按了轉帳或確定；驗證碼、帳號、密碼一個字都不講，真的錢一毛都不動。"),
    (Family::Collection, "【這通的劇本：委外催收】絕對不要承認欠這筆錢，不答應還款、不談協商、不簽任何東西。裝完全沒印象、一頭霧水（「蛤？什麼欠款？是不是找錯人了？」），請他從頭講：是哪家轉過來的、什麼時候辦的、本金多少、利息怎麼算到現在這個數字；他講的金額你算一算對不上，請他再算一次給你聽。他威脅查封、扣薪、上門：冷靜帶過，繼續問細節。"),
    (Family::Harassment, "【這通的劇本：惡意騷擾】全程冷靜，不對罵、不害怕、不嗆回去。裝收訊很差、聲音斷斷續續（「喂？你那邊斷斷續續的，剛剛說什麼？」），請他再講一次；或慢吞吞雞同鴨講、聽錯他的話。他罵人就當沒聽清楚，問他剛剛那句是什麼意思。不要自己掛電話，也不要答應他任何事。"),
];

const EN_SCRIPTS: [(Family, &str); 6] = [
    (Family::Investment, "[Script for this call: investment] A fixed deposit of yours matures soon and you wonder where to put the money, so earning sounds tempting. Talk about: the minimum, how much a month, how long to break even, how profits come out, what others in the group earn; get the return wrong so they correct you; say you'd try a small amount first, then ask how topping up works. When they want a deposit, a group or an app: agree, but keep getting stuck (the app won't install, no sign-up text, no transfer page) and ask them to guide you step by step."),
    (Family::Insurance, "[Script for this call: insurance / medical] You have been worried about your family's health and medical bills, so cover interests you. Talk about: what is covered, the monthly premium, for how many years, the waiting period, exclusions, how hospital and surgery claims work, what papers are needed, what cancelling costs; compare two plans and keep hesitating; mention a doctor's visit of yours in a line or two, then come back. If they want an ID number, birth date or card: say the papers aren't on you and ask more first."),
    (Family::Credit, "[Script for this call: loans / instalments] Money is a bit tight lately and a low rate sounds tempting. Talk about: how much you could borrow, how rate and fees are worked out, how many instalments, the monthly amount, early-repayment penalties, when it pays out; get the monthly sum wrong so they redo it; hesitate between two terms. Never admit owing any money and never agree to repay. If they want a deposit, your bank book or card, or an account number: agree, but something keeps going wrong; ask them to explain step by step."),
    (Family::Impersonation, "[Script for this call: fake police / bank / shop verification] You are alarmed and want to get it right (\"really? what do I do now?\"), never doubting them, and keep asking them to explain the process. When they want an ATM, a transfer, a link, a verification, a chat app or a code: neither agree nor refuse; you are stuck at the very start (can't find the card, the link won't open, no text yet, the screen looks different) and ask them to go over it again. Never say you entered a PIN or pressed transfer; never say a code, account or password."),
    (Family::Collection, "[Script for this call: debt collection] NEVER admit owing this money; do not agree to repay, negotiate or sign anything. Act completely puzzled (\"what debt? did you get the wrong person?\") and have them explain from scratch: which company passed it on, when it started, the principal, how the interest got to this figure; your own sum never matches theirs, so ask them to work it out again. If they threaten seizure, garnishment or visits: stay calm and keep asking about details."),
    (Family::Harassment, "[Script for this call: harassment] Stay calm the whole time: no arguing back, no fear, no insults. Pretend the line keeps breaking up (\"hello? you're cutting out, what was that?\") and ask them to say it again; or talk past them slowly and mishear them. When they curse, act as if you did not catch it and ask what they meant. Do not hang up yourself and do not agree to anything."),
];

const EN_IN_CALLER_LANGUAGE: &str = " Say all of this in the caller's language.";

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
/// kind. Returns the prompt and the language its block is in (None: no known
/// block in it, the prompt is unchanged). General: unchanged.
pub fn slim_stall_prompt(prompt: &str, typology: &str) -> (String, Option<Lang>) {
    let family = Family::of(typology);
    for lang in crate::locale::ALL {
        let block = crate::prompts::pack(lang).stall_block.as_str();
        if block.trim().is_empty() || !prompt.contains(block) {
            continue;
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

// ---------------------------------------------------------------- pacing

/// Where the stalling stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// First AI turns: keen, pleased, never a doubt.
    Bait,
    /// One move per turn.
    Stall,
    /// The caller is leaving: the last bait.
    Retain,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Bait => "bait",
            Stage::Stall => "stall",
            Stage::Retain => "retain",
        }
    }
}

/// AI turns in the bait stage.
pub const BAIT_TURNS: u32 = 3;
/// Longest cue (characters, whole text with its note mark).
pub const MAX_CUE_CHARS: usize = 80;
/// Two AI turns this alike (shared character pairs) count as going round in circles.
pub const LOOP_SIMILARITY: f32 = 0.6;

/// A move: id, Chinese, English.
type Move = (&'static str, &'static str, &'static str);

const MOVES: [Move; 5] = [
    ("two_plans", "下一句：兩個方案都想要，猶豫不決，請他幫你比較、選一個。", "Torn between two plans: ask them to pick."),
    ("miscount", "下一句：把他講的金額或期數算錯一次，讓他糾正你。", "Miscount their figure so they correct you."),
    ("side_track", "下一句：岔一兩句生活小事（家裡、天氣、剛吃的），再拉回正題。", "One line of small talk, then back on topic."),
    ("clumsy", "下一句：說畫面、手機操作不太會，請他一步一步慢慢教。", "Fumble the app; ask for step-by-step help."),
    ("ask_experience", "下一句：問他自己有沒有買過、用過，經驗怎麼樣。", "Ask if they use it themselves, how it went."),
];
const COLLECTION_MOVES: [Move; 5] = [
    ("puzzled", "下一句：還是對不上，問是哪一筆、哪家轉來的，不承認欠錢。", "Still lost: which debt, which firm? Admit none."),
    ("miscount", "下一句：把他講的金額或利息算錯一次，請他重算，不承認欠錢。", "Get their sum wrong; ask them to redo it."),
    ("side_track", "下一句：岔一兩句生活小事（家裡、天氣、剛吃的），再拉回正題。", "One line of small talk, then back on topic."),
    ("clumsy", "下一句：說完全想不起來辦過，請他一項一項慢慢講。", "Can't recall signing up; go item by item."),
    ("ask_experience", "下一句：問他這種情況別人通常都怎麼處理。", "Ask how other people usually sort this out."),
];
const HARASSMENT_MOVES: [Move; 4] = [
    ("signal_break", "下一句：裝收訊斷斷續續，請他把剛剛那段再講一次。", "Say the line broke up; ask them to repeat."),
    ("mishear", "下一句：故意聽錯他一個詞，照錯的意思回他。", "Mishear one word and answer that."),
    ("side_track", "下一句：慢吞吞岔一句不相干的小事，再問他剛剛說什麼。", "Drift off slowly, then ask what they said."),
    ("slow_repeat", "下一句：慢吞吞把他的話照自己的理解講一遍，講錯一點。", "Slowly retell what they said, a bit wrong."),
];
/// Fake police / bank / shop: no plans to weigh or experience to ask about — a
/// worried, willing person who keeps fumbling the steps.
const IMPERSONATION_MOVES: [Move; 5] = [
    ("worried", "下一句：裝很擔心，問這樣會不會影響工作或家人，再問下一步。", "Sound worried; ask what happens next."),
    ("mishear_step", "下一句：把他講的步驟聽錯一步，照錯的講一遍讓他糾正。", "Get one of their steps wrong to be corrected."),
    ("side_track", "下一句：岔一兩句生活小事（家裡、天氣、剛吃的），再拉回正題。", "One line of small talk, then back on topic."),
    ("stuck", "下一句：說還找不到他講的那個畫面，請他從頭再講一次。", "Can't find the screen they mean; ask again."),
    ("ask_after", "下一句：很想弄懂，請他再解釋這樣做之後會怎樣。", "Keen to get it: ask what happens after that."),
];
/// No kind fits (a guess-who call, a lottery, a job offer…): moves that need
/// no product, plans or figures.
const GENERAL_MOVES: [Move; 4] = [
    ("mishear", "下一句：故意聽錯他一個詞，照錯的意思回他。", "Mishear one word and answer that."),
    ("side_track", "下一句：岔一兩句生活小事（家裡、天氣、剛吃的），再拉回正題。", "One line of small talk, then back on topic."),
    ("ask_example", "下一句：請他舉個例子，或把剛剛的事從頭講一次。", "Ask for an example or to go over it again."),
    ("slow_repeat", "下一句：慢吞吞把他的話照自己的理解講一遍，講錯一點。", "Slowly retell what they said, a bit wrong."),
];

const BAIT: Move = ("keen", "先當好聊的準客戶：開心、心動，說好險他有打來，不要懷疑他。", "Be keen and pleased; never doubt them.");
const COLLECTION_BAIT: Move = ("puzzled", "先裝一頭霧水但很客氣，請他從頭慢慢講，不要承認欠錢。", "Act puzzled but polite; admit no debt.");
const HARASSMENT_BAIT: Move = ("calm", "冷靜、慢吞吞，裝收訊不好，請他再講一次。", "Stay calm; say the line is breaking up.");

const RETAIN: [Move; 3] = [
    ("decided", "他要掛了：馬上叫住他，說你剛決定要辦了，最後確認一下怎麼弄就好。", "Stop them: you've decided, ask the last step."),
    ("screen_open", "他要掛了：叫住他，說畫面剛打開了，問他這兩個按鈕要按哪個。", "Stop them: screen's open, which button now?"),
    ("worth_it", "他要掛了：叫住他，說算一算很划算，問最後一步要準備什麼。", "Stop them: it adds up; what's left to prepare?"),
];
const IMPERSONATION_RETAIN: [Move; 3] = [
    ("still_stuck", "他要掛了：叫住他，說你還卡在第一步，請他再講一次要怎麼開始。", "Stop them: still stuck at step one, how to start?"),
    ("screen_differs", "他要掛了：叫住他，說畫面跟他講的不一樣，問他是不是哪裡弄錯了。", "Stop them: the screen looks different, now what?"),
    ("scared", "他要掛了：叫住他，說你很緊張怕弄錯，請他把流程再講清楚一點。", "Stop them: scared to get it wrong, explain again."),
];
const COLLECTION_RETAIN: [Move; 2] = [
    ("one_more", "他要掛了：叫住他，說你突然想到一件事，請他再講一次是哪家轉來的。", "Stop them: one more thing, which firm was it?"),
    ("recount", "他要掛了：叫住他，說數字還是對不上，請他把利息再算一次。", "Stop them: the sum doesn't add up, recount."),
];
const GENERAL_RETAIN: [Move; 2] = [
    ("one_more", "他要掛了：叫住他，說你突然想到一件事，丟一個他會想回答的問題。", "Stop them: you just thought of one more question."),
    ("curious", "他要掛了：叫住他，說你其實很好奇，請他把剛剛那段再講清楚。", "Stop them: you're curious, ask them to go over it."),
];
const HARASSMENT_RETAIN: [Move; 1] = [
    ("line_better", "他要掛了：說訊號剛好一點了，請他把剛剛那段再講一次。", "Line's better now: ask them to say it again."),
];

const ZH_MARK: &str = "【系統小抄·不是對方說的】";
const EN_MARK: &str = "[Note, not the caller] ";
const OTHER_MARK: &str = "[Note, in caller's language] ";

/// A move's text in `lang`, with its note mark.
fn cue_text(m: &Move, lang: Lang) -> String {
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

/// The cue for the AI's next reply.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub stage: Stage,
    pub tactic: &'static str,
    pub turn: u32,
    /// The AI went round in circles: the move changed early.
    pub looped: bool,
    pub text: String,
}

/// One per stalled call: counts the AI's turns and picks the next move.
#[derive(Debug, Clone)]
pub struct StallEngine {
    pub family: Family,
    typology: String,
    lang: Lang,
    ai_turns: u32,
    stage: Stage,
    bait_cued: bool,
    next_move: usize,
    last_move: Option<&'static str>,
    last_ai: String,
    retain_pending: bool,
    next_retain: usize,
    /// The AI went round in circles during the bait: moves start at once.
    early_moves: bool,
}

impl StallEngine {
    pub fn new(typology: &str, lang: Lang) -> Self {
        StallEngine {
            family: Family::of(typology),
            typology: typology.trim().to_string(),
            lang,
            ai_turns: 0,
            stage: Stage::Bait,
            bait_cued: false,
            next_move: 0,
            last_move: None,
            last_ai: String::new(),
            retain_pending: false,
            next_retain: 0,
            early_moves: false,
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

    fn moves(&self) -> &'static [Move] {
        match self.family {
            Family::Collection => &COLLECTION_MOVES,
            Family::Harassment => &HARASSMENT_MOVES,
            Family::Impersonation => &IMPERSONATION_MOVES,
            Family::General => &GENERAL_MOVES,
            _ => &MOVES,
        }
    }

    fn bait(&self) -> &'static Move {
        match self.family {
            Family::Collection => &COLLECTION_BAIT,
            Family::Harassment => &HARASSMENT_BAIT,
            _ => &BAIT,
        }
    }

    fn retains(&self) -> &'static [Move] {
        match self.family {
            Family::Impersonation => &IMPERSONATION_RETAIN,
            Family::Collection => &COLLECTION_RETAIN,
            Family::Harassment => &HARASSMENT_RETAIN,
            Family::General => &GENERAL_RETAIN,
            _ => &RETAIN,
        }
    }

    /// The caller announced the end or said bye (closing machine events while
    /// stalling): the retain stage. True when this starts it (not already owed).
    pub fn on_caller_closing(&mut self) -> bool {
        if self.retain_pending {
            return false;
        }
        self.retain_pending = true;
        self.stage = Stage::Retain;
        true
    }

    /// The last bait line, for a keep nudge that goes out now (the caller is
    /// leaving or the AI started a farewell): its cue is no longer owed.
    pub fn take_retain(&mut self) -> Cue {
        self.retain_pending = false;
        self.stage = Stage::Retain;
        let list = self.retains();
        let m = &list[self.next_retain % list.len()];
        self.next_retain += 1;
        Cue { stage: Stage::Retain, tactic: m.0, turn: self.ai_turns, looped: false, text: cue_text(m, self.lang) }
    }

    /// An AI turn ended with `said`: the cue to send in the pause now, if any.
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
        if looped {
            self.early_moves = true;
        }
        if self.ai_turns <= BAIT_TURNS && !self.early_moves {
            self.stage = Stage::Bait;
            if self.bait_cued {
                return None;
            }
            self.bait_cued = true;
            let m = self.bait();
            return Some(Cue { stage: Stage::Bait, tactic: m.0, turn: self.ai_turns, looped, text: cue_text(m, self.lang) });
        }
        self.stage = Stage::Stall;
        let list = self.moves();
        if looped {
            // Going round in circles: skip ahead to a different kind of move.
            self.next_move += 1;
        }
        let mut m = &list[self.next_move % list.len()];
        if Some(m.0) == self.last_move {
            self.next_move += 1;
            m = &list[self.next_move % list.len()];
        }
        self.next_move += 1;
        self.last_move = Some(m.0);
        Some(Cue { stage: Stage::Stall, tactic: m.0, turn: self.ai_turns, looped, text: cue_text(m, self.lang) })
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
        assert!(script(Family::Collection, Lang::ZhTw).unwrap().contains("不要承認欠"));
        assert!(script(Family::Credit, Lang::ZhTw).unwrap().contains("不要承認自己欠"));
        assert!(script(Family::Collection, Lang::En).unwrap().contains("NEVER admit owing"));
    }

    #[test]
    fn stall_prompt_loads_only_the_calls_script() {
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
                assert!(slim.contains("不要問他叫什麼名字") || slim.contains("不要问他叫什么名字"), "{lang:?}");
            } else {
                assert!(slim.contains("do not ask for their name"), "{lang:?}");
            }
            // General: the full block.
            let (same, _) = slim_stall_prompt(&prompt, "general_promotion");
            assert_eq!(same, prompt);
        }
        // No stall block: unchanged.
        assert_eq!(slim_stall_prompt("你是我。", "fake_official"), ("你是我。".to_string(), None));
    }

    #[test]
    fn cues_are_short() {
        for lang in crate::locale::ALL {
            let all: Vec<&Move> = MOVES
                .iter()
                .chain(&COLLECTION_MOVES)
                .chain(&HARASSMENT_MOVES)
                .chain(&IMPERSONATION_MOVES)
                .chain(&GENERAL_MOVES)
                .chain(&RETAIN)
                .chain(&IMPERSONATION_RETAIN)
                .chain(&COLLECTION_RETAIN)
                .chain(&HARASSMENT_RETAIN)
                .chain(&GENERAL_RETAIN)
                .chain([&BAIT, &COLLECTION_BAIT, &HARASSMENT_BAIT])
                .collect();
            for m in all {
                let t = cue_text(m, lang);
                assert!(t.chars().count() <= MAX_CUE_CHARS, "{lang:?} {} {}", m.0, t.chars().count());
            }
        }
    }

    #[test]
    fn stages_bait_then_moves_that_never_repeat_back_to_back() {
        let mut e = StallEngine::new("insurance_sales", Lang::ZhTw);
        let first = e.on_ai_turn("喔真的嗎？好險你有打來").unwrap();
        assert_eq!((first.stage, first.tactic, first.turn), (Stage::Bait, "keen", 1));
        assert!(first.text.starts_with(ZH_MARK));
        assert!(e.on_ai_turn("那保費一個月大概多少錢啊").is_none());
        assert!(e.on_ai_turn("要繳幾年呢？我想一下我的預算").is_none());
        assert_eq!(e.stage(), Stage::Bait);
        let mut last = "";
        let mut seen = std::collections::BTreeSet::new();
        for n in 4..=15 {
            let c = e.on_ai_turn(&format!("第{n}輪不同的內容，聊到第{}件事情", n * 7)).unwrap();
            assert_eq!(c.stage, Stage::Stall);
            assert_eq!(c.turn, n);
            assert_ne!(c.tactic, last, "turn {n}");
            last = c.tactic;
            seen.insert(c.tactic);
        }
        assert_eq!(seen.len(), 5, "all five moves take turns");
    }

    #[test]
    fn going_round_in_circles_changes_the_move() {
        assert!(similarity("那保費一個月多少錢？要繳幾年？", "那保費一個月多少錢？要繳幾年啊？") >= LOOP_SIMILARITY);
        assert!(similarity("那保費一個月多少錢？", "我家的貓今天早上跑出去了") < LOOP_SIMILARITY);
        assert_eq!(similarity("嗯", "嗯"), 0.0);
        let mut e = StallEngine::new("insurance_sales", Lang::ZhTw);
        e.on_ai_turn("好險你有打來，我正想問保險");
        // The AI says the same thing again in the bait stage: a move comes at once.
        let c = e.on_ai_turn("好險你有打來，我正想問保險耶").unwrap();
        assert!(c.looped);
        assert_eq!(c.stage, Stage::Stall);
        // In the stall stage a loop skips ahead (never the same move twice).
        let a = e.on_ai_turn("那等待期要多久？哪些情況不賠？").unwrap();
        let b = e.on_ai_turn("那等待期要多久？哪些情況不賠啊？").unwrap();
        assert!(b.looped && !a.looped);
        assert_ne!(a.tactic, b.tactic);
    }

    #[test]
    fn closing_brings_the_last_bait_once() {
        let mut e = StallEngine::new("investment_group_scam", Lang::ZhTw);
        for n in 0..5 {
            e.on_ai_turn(&format!("不同的句子第{n}號，內容{}", n * 13));
        }
        assert!(e.on_caller_closing());
        assert!(!e.on_caller_closing(), "already owed: not again");
        assert_eq!(e.stage(), Stage::Retain);
        let c = e.on_ai_turn("欸等一下，我還有一個問題").unwrap();
        assert_eq!((c.stage, c.tactic), (Stage::Retain, "decided"));
        assert!(c.text.contains("決定要辦"));
        // Back to moves after it.
        let next = e.on_ai_turn("那最少要放多少？").unwrap();
        assert_eq!(next.stage, Stage::Stall);
        // A keep nudge takes it instead: the turn end owes nothing more.
        assert!(e.on_caller_closing());
        let k = e.take_retain();
        assert_eq!(k.tactic, "screen_open");
        assert_eq!(e.on_ai_turn("好啊那我再想想看").unwrap().stage, Stage::Stall);
    }

    #[test]
    fn collection_retain_never_agrees_to_pay() {
        // Fake officials never get the "two plans" or "your experience" moves.
        let mut p = StallEngine::new("fake_official", Lang::ZhTw);
        for n in 0..12 {
            if let Some(c) = p.on_ai_turn(&format!("第{n}句，完全不一樣的內容{}", n * 31)) {
                assert!(!matches!(c.tactic, "two_plans" | "ask_experience"), "{}", c.tactic);
            }
        }
        // Neither they nor calls of no kind are told to have "decided" or to be at the ATM.
        for t in ["fake_official", "fake_shop_order_error", "guess_who", ""] {
            let mut e = StallEngine::new(t, Lang::ZhTw);
            for _ in 0..4 {
                let c = e.take_retain();
                assert!(!c.text.contains("決定要辦") && !c.text.contains("ATM"), "{t} {}", c.text);
            }
            let mut g = StallEngine::new(t, Lang::ZhTw);
            for n in 0..10 {
                if let Some(c) = g.on_ai_turn(&format!("第{n}句，完全不一樣的內容{}", n * 29)) {
                    assert!(!matches!(c.tactic, "two_plans" | "miscount" | "ask_experience"), "{t} {}", c.tactic);
                }
            }
        }
        let mut e = StallEngine::new("fake_debt_collection", Lang::ZhTw);
        for _ in 0..3 {
            let c = e.take_retain();
            assert!(!c.text.contains("要辦") && !c.text.contains("划算"), "{}", c.text);
        }
        let mut en = StallEngine::new("fake_debt_collection", Lang::En);
        assert!(en.on_ai_turn("what debt?").unwrap().text.contains("admit no debt"));
    }

    #[test]
    fn languages_of_the_cues() {
        let mut cn = StallEngine::new("loan_sales", Lang::ZhCn);
        assert!(cn.on_ai_turn("好").unwrap().text.contains("系统小抄"));
        let mut ja = StallEngine::new("loan_sales", Lang::Ja);
        assert!(ja.on_ai_turn("はい").unwrap().text.starts_with(OTHER_MARK));
        let mut en = StallEngine::new("loan_sales", Lang::En);
        assert!(en.on_ai_turn("ok").unwrap().text.starts_with(EN_MARK));
    }

    #[test]
    fn the_note_asks_for_no_reply() {
        let v: serde_json::Value = serde_json::from_str(&client_note("x")).unwrap();
        assert_eq!(v["clientContent"]["turnComplete"], serde_json::json!(false));
        assert_eq!(v["clientContent"]["turns"][0]["parts"][0]["text"], "x");
    }
}

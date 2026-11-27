// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The language a call is held in. Bageo decides it, not the model: the model
//! keeps to it and is told when it changes ([crate::prompts::language_switch_note]).
//! A call has base languages: the one it opened in, the language of its prompt,
//! the main language of the SIM that took it and a local way of answering's
//! ([CallLanguage::with_base]). The call moves between them on one clear turn of
//! the caller's ([crate::urgency::read_language]); into any other language only
//! when it is certain the caller speaks it: two turns in a row of whole, clear
//! sentences in it, or a plain request ("Can you speak English?",
//! 「可以講中文嗎」, [asks_for_language]). One or two words (a "Sí", an "Oi",
//! 「喂」), a greeting, a few foreign words, a name, a line not read clearly or
//! the carrier's warning move nothing. With the setting `reply_in_caller_language`
//! off a call keeps to its base languages ([CallLanguage::following_others]).
//! Every mid-call instruction names the call's language
//! ([crate::prompts::call_language_note]), and when the AI's turn comes out in
//! another core language it is reminded, at most [DRIFT_NOTES_MAX] times a call,
//! whatever the caller's last words seemed to be in. Memory only; a few string
//! scans per turn.
//! The caller's speech-to-text often comes in after the AI has begun its reply,
//! so a turn can hold the end of the last one: a turn in two scripts
//! (「もしもし、大和運輸です。」 then a Spanish misreading) is read part by part
//! ([script_parts]), and one turn never counts twice toward a switch.

use crate::locale::Lang;

/// What a caller speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoken {
    /// One of the languages with a setting.
    Lang(Lang),
    /// A language without a setting (Thai, Russian, Vietnamese…).
    Other,
}

/// Drift reminders per call.
pub const DRIFT_NOTES_MAX: u32 = 3;

/// Letters (Chinese, Japanese, Korean, other scripts) or Latin words a part of
/// a turn needs to be read on its own ([script_parts]).
const PART_MIN: usize = 4;

/// A turn of fewer words or letters than this is one or two stray words: it
/// moves no call's language ([CallLanguage::caller_turn]).
const SHORT_TURN_WORDS: usize = 3;

/// What a whole sentence holds at least, to count toward a language outside
/// the call's base: Latin words, Chinese, Japanese or Korean letters, letters of
/// another script ([whole_sentence]).
const SENTENCE_MIN_WORDS: usize = 4;
const SENTENCE_MIN_CJK: usize = 6;
const SENTENCE_MIN_OTHER: usize = 8;

/// Chinese characters a line without kana needs to count as Chinese on a
/// call held in Japanese: Japanese names and nouns are written in them alone
/// (「大和運輸の田中」 has a single kana).
const JA_CALL_HAN_MIN: usize = 8;

/// The AI's turn came out in another language than the call's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Drift {
    pub ai: Lang,
    pub call: Lang,
    /// The AI's turns so far, this one included.
    pub turn: u32,
}

#[derive(Debug, Clone)]
pub struct CallLanguage {
    now: Spoken,
    /// Languages the call moves into on one clear turn (the opening's first).
    base: Vec<Spoken>,
    /// Languages outside the base may be taken up (setting `reply_in_caller_language`).
    follow_others: bool,
    /// The other language of the caller's last whole clear turn, waiting for a second one.
    pending: Option<Spoken>,
    /// The caller turn the pending language was heard in.
    pending_turn: u32,
    /// Caller turns so far.
    turns: u32,
    /// The language the caller asked for in their latest turn.
    asked: Option<Spoken>,
    /// Chinese is held in this variant (the prompt's when it is Chinese).
    zh: Lang,
    ai_turns: u32,
    drift_notes: u32,
}

impl CallLanguage {
    /// `opening`: the language the call opens in; `prompt`: the language of the
    /// call's prompt (its Chinese variant is kept whatever script the
    /// speech-to-text writes). Both are base languages.
    pub fn new(opening: Lang, prompt: Lang) -> Self {
        let zh = if prompt.is_zh() {
            prompt
        } else if opening.is_zh() {
            opening
        } else {
            Lang::ZhTw
        };
        let opening = if opening.is_zh() { zh } else { opening };
        let mut c = Self {
            now: Spoken::Lang(opening),
            base: vec![Spoken::Lang(opening)],
            follow_others: true,
            pending: None,
            pending_turn: 0,
            turns: 0,
            asked: None,
            zh,
            ai_turns: 0,
            drift_notes: 0,
        };
        c.add_base(Spoken::Lang(c.same_variant(prompt)));
        c
    }

    /// More base languages: the SIM's main language, a local way of answering's.
    /// `other`: a language without a setting is one too (the call opened the
    /// way a country answers whose language has none: Thai, Italian…).
    pub fn with_base(mut self, langs: impl IntoIterator<Item = Lang>, other: bool) -> Self {
        for l in langs {
            let s = Spoken::Lang(self.same_variant(l));
            self.add_base(s);
        }
        if other {
            self.add_base(Spoken::Other);
        }
        self
    }

    /// `on` = false (setting `reply_in_caller_language` off): the call never
    /// leaves its base languages, whatever the caller speaks or asks for.
    pub fn following_others(mut self, on: bool) -> Self {
        self.follow_others = on;
        self
    }

    fn add_base(&mut self, s: Spoken) {
        if !self.base.contains(&s) {
            self.base.push(s);
        }
    }

    /// The call's language; None for a language without a setting.
    pub fn lang(&self) -> Option<Lang> {
        match self.now {
            Spoken::Lang(l) => Some(l),
            Spoken::Other => None,
        }
    }

    pub fn is_other(&self) -> bool {
        self.now == Spoken::Other
    }

    pub fn spoken(&self) -> Spoken {
        self.now
    }

    /// The base languages, the opening's first (for the logs).
    pub fn base(&self) -> &[Spoken] {
        &self.base
    }

    /// Languages outside the base may be taken up ([Self::following_others]).
    pub fn follows_others(&self) -> bool {
        self.follow_others
    }

    fn same_variant(&self, lang: Lang) -> Lang {
        if lang.is_zh() {
            self.zh
        } else {
            lang
        }
    }

    /// The call may be held in `s`.
    fn may_take(&self, s: Spoken) -> bool {
        self.follow_others || self.base.contains(&s)
    }

    /// A language the caller asked for, as the call holds it: one without a
    /// setting that does not reach the call stage is [Spoken::Other].
    fn requested(&self, l: Lang) -> Spoken {
        match l {
            Lang::Ext(_) if crate::locales_store::stage(l) < crate::locales_store::Stage::Call => Spoken::Other,
            _ => Spoken::Lang(self.same_variant(l)),
        }
    }

    /// What `said` is clearly in, if anything. Words in a language without a
    /// setting read as that language when the translations carry it to the
    /// call stage ([crate::urgency::read_other]); in a call held in such a
    /// language, words in the same script count as it (Arabic letters do not
    /// tell Arabic, Persian and Urdu apart) unless their own words tell another
    /// language ([crate::urgency::other_told_apart]: Italian to Indonesian,
    /// Ukrainian to Russian).
    fn clear(&self, said: &str) -> Option<Spoken> {
        if crate::urgency::unlisted_language(said) {
            if let Some(r) = crate::urgency::read_other(said) {
                if let Spoken::Lang(Lang::Ext(now)) = self.now {
                    if now.script() == r.script {
                        return Some(crate::urgency::other_told_apart(said, now).map_or(self.now, Spoken::Lang));
                    }
                }
                if let Some(l) = r.lang {
                    return Some(Spoken::Lang(l));
                }
            }
            return Some(Spoken::Other);
        }
        let r = crate::urgency::read_language(said).filter(|r| r.clear)?;
        if r.lang.is_zh() && self.now == Spoken::Lang(Lang::Ja) && han_letters(said) < JA_CALL_HAN_MIN {
            return None;
        }
        Some(Spoken::Lang(self.same_variant(r.lang)))
    }

    /// A finished turn of the caller's. Returns the call's new language when it
    /// changed. A plain request for a language takes the call there at once; a
    /// turn in two scripts is read part by part, in order.
    pub fn caller_turn(&mut self, said: &str) -> Option<Spoken> {
        self.turns += 1;
        let before = self.now;
        let asked = asks_for_language(said).map(|l| self.requested(l)).filter(|s| self.may_take(*s));
        self.asked = asked;
        if let Some(asked) = asked {
            self.now = asked;
            self.pending = None;
            return (self.now != before).then_some(self.now);
        }
        // One or two stray words (a "Sí", an "Oi", 「喂」) say nothing about the language.
        if words_or_letters(said) < SHORT_TURN_WORDS && said.chars().filter(|c| c.is_alphabetic()).count() < PART_MIN {
            return None;
        }
        // Nor do hellos (「여보세요」, 「もしもし」, "Hola, buenas"), even read clearly;
        // one of another language than the pending one breaks the row.
        if greeting_only(said) {
            self.forget_pending_unless(said);
            return None;
        }
        // Whole clear sentences outside the base, in this turn: two different
        // languages in one turn are no evidence for either.
        let mut outside: Option<Spoken> = None;
        let mut mixed = false;
        for part in script_parts(said) {
            if let Some(heard) = self.caller_part(part) {
                match outside {
                    Some(o) if o != heard => mixed = true,
                    _ => outside = Some(heard),
                }
            }
        }
        match outside {
            _ if mixed => self.pending = None,
            Some(heard) if self.pending == Some(heard) && self.pending_turn < self.turns => {
                self.now = heard;
                self.pending = None;
            }
            Some(heard) => {
                self.pending = Some(heard);
                self.pending_turn = self.turns;
            }
            None => {}
        }
        (self.now != before).then_some(self.now)
    }

    /// One part of a caller turn. The call's language keeps it (and forgets a
    /// pending one); a base language takes the call at once; a whole clear
    /// sentence in another language is returned, to wait for a second turn.
    fn caller_part(&mut self, said: &str) -> Option<Spoken> {
        let Some(heard) = self.clear(said) else {
            self.forget_pending_unless(said);
            return None;
        };
        if heard == self.now {
            self.pending = None;
            return None;
        }
        if self.base.contains(&heard) {
            self.now = heard;
            self.pending = None;
            return None;
        }
        (self.follow_others && whole_sentence(said)).then_some(heard)
    }

    /// Words that count for no language but read as another one than the
    /// pending one: it is no longer in a row.
    fn forget_pending_unless(&mut self, said: &str) {
        if let Some(pending) = self.pending {
            if let Some(r) = crate::urgency::read_language(said) {
                if Spoken::Lang(self.same_variant(r.lang)) != pending {
                    self.pending = None;
                }
            }
        }
    }

    /// A finished turn of the AI's (its speech-to-text): the drift to remind it
    /// of, when its turn is clearly in another core language than the call's,
    /// and reminders are left. `can_tell`: a reminder can go now; otherwise
    /// nothing is counted.
    pub fn ai_turn(&mut self, said: &str, can_tell: bool) -> Option<Drift> {
        self.ai_turn_after(said, "", can_tell)
    }

    /// [Self::ai_turn] with `late`: the caller's words that came in after the
    /// AI began this turn (speech-to-text of the caller often lags behind the
    /// reply). Only a language the caller asked for, there or in their last
    /// turn, or a base language they spoke clearly there, is no drift: a
    /// language the caller's words only seemed to be in is one.
    pub fn ai_turn_after(&mut self, said: &str, late: &str, can_tell: bool) -> Option<Drift> {
        self.ai_turns += 1;
        let call = self.lang()?;
        // One or two words: too short to be read as a language ([Self::clear] never
        // does), but the model can still follow a caller's stray "Sí" or "Oi" into
        // it. A greeting or "yes" of another core language than the call's is a drift.
        let short = words_or_letters(said) < SHORT_TURN_WORDS;
        let ai = if short {
            // Words like "Ja" or "Hallo" belong to several languages (Dutch, Swedish,
            // Romanian…): on a call in one without a setting, a short word is no drift.
            if matches!(call, Lang::Ext(_)) {
                return None;
            }
            short_word_lang(said).filter(|l| !(l.is_zh() && call.is_zh()))?
        } else {
            match self.clear(said)? {
                Spoken::Lang(l) => l,
                Spoken::Other => return None,
            }
        };
        // Only a turn in a core language is a drift: another language read from
        // its letters may be the call's own told apart wrong.
        if !ai.is_core() {
            return None;
        }
        let ai_s = Spoken::Lang(ai);
        if ai == call || self.asked == Some(ai_s) || self.late_moves_to(late, ai_s) {
            return None;
        }
        // Close Latin languages share short words (a Spanish line can score a
        // point of Portuguese): a drift between them needs a clear margin.
        if !short
            && call.is_core()
            && crate::urgency::is_latin_lang(ai)
            && crate::urgency::is_latin_lang(call)
            && crate::urgency::latin_margin(said, ai, call) < 2
        {
            return None;
        }
        if !can_tell || self.drift_notes >= DRIFT_NOTES_MAX {
            return None;
        }
        self.drift_notes += 1;
        Some(Drift { ai, call, turn: self.ai_turns })
    }

    /// The caller's late words (`late`) ask for `to`, or are a clear line in
    /// it that takes the call there at once (a base language): the move comes
    /// with their next turn.
    fn late_moves_to(&self, late: &str, to: Spoken) -> bool {
        if late.trim().is_empty() || !self.may_take(to) {
            return false;
        }
        if asks_for_language(late).map(|l| self.requested(l)) == Some(to) {
            return true;
        }
        self.base.contains(&to) && script_parts(late).into_iter().any(|p| self.clear(p) == Some(to))
    }
}

/// Hellos and one-word answers, in Chinese, Japanese and Korean letters…
const CJK_GREETINGS: &[&str] = &[
    "안녕하세요", "여보세요", "こんにちは", "こんばんは", "もしもし", "喂你好", "喂您好", "你好", "您好", "哈囉", "哈啰", "是的",
    "好的", "はい", "ええ", "うん", "喂", "嗨", "嗯", "對", "对", "네", "예", "응",
];
/// …and in Latin letters.
const LATIN_GREETINGS: &[&str] = &[
    "hello", "hi", "hey", "yes", "yeah", "yep", "ok", "okay", "hallo", "ja", "nein", "genau", "guten", "tag", "morgen",
    "abend", "hola", "sí", "si", "aló", "alo", "diga", "dígame", "digame", "bueno", "buenas", "oi", "olá", "ola", "alô",
    "sim", "oui", "allô", "allo", "salut", "bonjour", "bonsoir", "pronto", "ciao", "halo",
];

/// `said` holds hellos and one-word answers only ([CJK_GREETINGS],
/// [LATIN_GREETINGS]): 「여보세요」, 「もしもし」, "Hola, buenas", "Guten Tag".
fn greeting_only(said: &str) -> bool {
    let lower = said.to_lowercase();
    let mut rest = lower.clone();
    for g in CJK_GREETINGS {
        rest = rest.replace(g, " ");
    }
    let words: Vec<&str> = rest
        .split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|w| !w.is_empty())
        .collect();
    (rest != lower || !words.is_empty()) && words.iter().all(|w| LATIN_GREETINGS.contains(w))
}

/// A whole sentence, not a word or two: enough Latin words or letters.
fn whole_sentence(said: &str) -> bool {
    let (mut cjk, mut other) = (0, 0);
    for c in said.chars() {
        match script_of(c) {
            Some(Script::Cjk) => cjk += 1,
            Some(Script::Other) => other += 1,
            _ => {}
        }
    }
    latin_words(said) >= SENTENCE_MIN_WORDS || cjk >= SENTENCE_MIN_CJK || other >= SENTENCE_MIN_OTHER
}

/// Names of languages a caller may ask for, by language code: in English and
/// the language itself, in the core languages' words for it.
const LANGUAGE_NAMES: &[(&str, &[&str])] = &[
    (
        "en",
        &["english", "inglés", "ingles", "anglais", "englisch", "inglês", "inglese", "английски", "英文", "英語", "英语", "영어"],
    ),
    (
        "zh-TW",
        &[
            "chinese", "mandarin", "chino", "chinois", "chinesisch", "chinês", "cinese", "китайски", "中文", "國語", "国语",
            "普通話", "普通话", "華語", "华语", "中国語", "중국어",
        ],
    ),
    ("ja", &["japanese", "japonés", "japones", "japonais", "japanisch", "japonês", "giapponese", "японски", "日文", "日語", "日语", "日本語", "일본어"]),
    ("ko", &["korean", "coreano", "coréen", "koreanisch", "корейски", "韓文", "韩文", "韓語", "韩语", "韓国語", "한국어"]),
    (
        "es",
        &[
            "spanish", "español", "espanol", "castellano", "espagnol", "spanisch", "espanhol", "spagnolo", "испански", "西班牙文",
            "西班牙語", "西班牙语", "スペイン語", "스페인어",
        ],
    ),
    (
        "fr",
        &["french", "francés", "frances", "français", "francais", "französisch", "francês", "francese", "французски", "法文", "法語", "法语", "フランス語", "프랑스어"],
    ),
    ("de", &["german", "alemán", "aleman", "allemand", "deutsch", "alemão", "tedesco", "немецки", "德文", "德語", "德语", "ドイツ語", "독일어"]),
    (
        "pt",
        &["portuguese", "portugués", "portugues", "portugais", "portugiesisch", "português", "portoghese", "葡萄牙文", "葡萄牙語", "葡萄牙语", "ポルトガル語", "포르투갈어"],
    ),
    ("it", &["italian", "italiano", "italien", "italienisch", "義大利文", "意大利文", "意大利语", "イタリア語", "이탈리아어"]),
    ("th", &["thai", "tailandés", "thaï", "thailändisch", "ภาษาไทย", "泰文", "泰語", "泰语", "タイ語", "태국어"]),
    ("vi", &["vietnamese", "vietnamita", "vietnamien", "vietnamesisch", "tiếng việt", "越南文", "越南語", "越南语", "ベトナム語", "베트남어"]),
    ("ru", &["russian", "ruso", "russe", "russisch", "russo", "русски", "русский", "俄文", "俄語", "俄语", "ロシア語", "러시아어"]),
    ("id", &["indonesian", "indonesio", "indonésien", "indonesisch", "bahasa indonesia", "印尼文", "印尼語", "印尼语", "インドネシア語", "인도네시아어"]),
];

/// Words that make a clause with a language's name in it a request: speak,
/// talk, please, switch, understand…, in the core languages and a few others.
const REQUEST_WORDS: &[&str] = &[
    "speak", "speaks", "talk", "please", "switch", "understand", "habla", "hablar", "hable", "hablas", "hablamos", "hablo",
    "falo", "parlo",
    "favor", "entiende", "entiendes", "cambiar", "parle", "parler", "parlez", "parles", "parlons", "comprenez", "plaît",
    "plait", "sprechen", "sprichst", "spreche", "sprich", "bitte", "verstehen", "verstehst", "wechseln", "falar", "fala",
    "fale", "falas", "falamos", "entende", "mudar", "parla", "parlare", "parli", "говорите", "говорить", "говоришь",
    "nói", "bisa", "berbicara",
];

/// The word before a language's name that asks for it: "in English", "en
/// español", "auf Deutsch", "em português".
const REQUEST_BEFORE: &[&str] = &["in", "en", "auf", "em", "по"];

/// A "not" before a name in Latin or Cyrillic letters: that name is not asked
/// for ("I don't speak Chinese").
const NEGATION_WORDS: &[&str] = &[
    "not", "don't", "dont", "doesn't", "didn't", "can't", "cant", "cannot", "won't", "wouldn't", "shouldn't", "couldn't",
    "never", "stop", "no", "ne", "pas", "nicht", "kein", "keine", "não", "nao", "nunca", "не",
];
/// "No" and "ne" also open a turn on their own ("No, can you speak English?"
/// with the comma lost): they only count this close to the verb or the name.
const NEGATION_NEAR_ONLY: &[&str] = &["no", "ne"];
const NEGATION_NEAR_WORDS: usize = 2;
/// A "not" right before a name in Chinese letters (「我不會講英文」, 「我不要講英文」),
/// or asking how to say something (「怎麼用英文說」): that name is not asked for.
const NEGATION_BEFORE_NAME: &[&str] = &[
    "不會", "不会", "不懂", "不能", "不行", "不要", "不想", "不講", "不讲", "不說", "不说", "不用", "不必", "不需要",
    "不太", "沒辦法", "没办法", "別", "别", "怎麼", "怎么",
];
/// Chinese "A or not A" questions (「你能不能講英文」): a request, not a "not".
const A_NOT_A: &[&str] = &[
    "能不能", "會不會", "会不会", "可不可以", "要不要", "行不行", "好不好", "可以不可以", "有沒有辦法", "有没有办法",
];
/// The same right after a name: Japanese and Korean put their "not" there
/// (「英語は話せません」, 「영어 못해요」), and 「這個用英文怎麼說」.
const NEGATION_AFTER_NAME: &[&str] = &[
    "怎麼", "怎么", "什麼意思", "什么意思", "不太", "できない", "できません", "話せない", "話せません", "分からない",
    "わからない", "分かりません", "わかりません", "どう言", "못", "모르", "몰라", "안 돼", "안돼", "뭐라고",
];

/// Around a name in Chinese, Japanese, Korean or Thai letters, a request on
/// its own: right before it (「請用中文」「พูดภาษาไทย」), right after it
/// (「英語でお願い」「영어 할 줄 아세요」).
const STRONG_BEFORE: &[&str] = &["請用", "请用", "改用", "改講", "改讲", "改說", "改说", "請講", "请讲", "請說", "请说", "พูด"];
const STRONG_AFTER: &[&str] = &[
    "で話", "でお願い", "できます", "できる", "話せ", "でしゃべ", "할 줄", " 할 줄", "가능", " 가능", "ได้",
];
/// The same, only with a request or a question in the clause (「你會講中文嗎」,
/// 「中文可以嗎」, 「영어로 말해 주세요」): 「他說英文名字叫 Tom」 asks nothing.
const WEAK_BEFORE: &[&str] = &["講", "讲", "說", "说", "會", "会", "用"];
const WEAK_AFTER: &[&str] = &[
    "로", "으로", "可以", "好嗎", "好吗", "嗎", "吗", "行嗎", "行吗", "でいい", "で大丈夫", "돼", " 돼", "되", " 되",
];
const REQUEST_CUES: &[&str] = &[
    "嗎", "吗", "可以", "能不能", "能否", "請", "请", "好不好", "拜託", "拜托", "吧", "か", "ください", "주세요", "주실",
    "줄래", "해줘", "해 줘", "할 수", "돼", "되나", "되세요", "ไหม", "หน่อย",
];

/// The language a caller plainly asks the AI to speak ("Can you speak English?",
/// "English, please", 「可以講中文嗎」, 「英語でお願いします」, 「영어로 해 주세요」,
/// "¿Habla español?"); a name with a "not" of its own is not asked for ("I
/// don't speak Chinese" asks for nothing, "I don't speak Chinese can you speak
/// English" for English). The last such clause counts. A turn that
/// is only a language's name ("English?", 「中文。」) asks for it too.
pub fn asks_for_language(said: &str) -> Option<Lang> {
    let said = said.replace('’', "'").to_lowercase();
    let short_turn = words_or_letters(&said) <= 3;
    clauses(&said)
        .into_iter()
        .rev()
        .find_map(|c| clause_asks(c, short_turn))
        .and_then(Lang::parse_any)
}

/// `said` (lower case) cut after each mark that ends a clause; the mark stays.
fn clauses(said: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in said.char_indices() {
        if matches!(c, '.' | ',' | ';' | '!' | '?' | '。' | '，' | '；' | '！' | '？' | '、' | '¿' | '¡') {
            let end = i + c.len_utf8();
            out.push(&said[start..end]);
            start = end;
        }
    }
    out.push(&said[start..]);
    out.into_iter().filter(|c| c.chars().any(char::is_alphabetic)).collect()
}

/// Characters before and after a name in Chinese, Japanese, Korean or Thai
/// letters that a "not" of its own is looked for in (「我不會講中文」,
/// 「英語は話せません」), up to the name before or after it.
const NEGATION_BEFORE_CHARS: usize = 6;
const NEGATION_AFTER_CHARS: usize = 8;

/// Words of Latin or Cyrillic letters before a name that a "not" of its own is
/// looked for in: from this many before the verb that goes with it ("I don't
/// want to speak English"), else before the name.
const NEGATION_BEFORE_WORDS: usize = 4;

/// A language's name in Latin or Cyrillic letters.
fn is_latin_name(w: &str) -> bool {
    LANGUAGE_NAMES.iter().any(|(_, names)| names.contains(&w))
}

/// Where each language name in Chinese, Japanese, Korean or Thai letters
/// stands in `clause` (byte ranges).
fn cjk_name_spans(clause: &str) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = LANGUAGE_NAMES
        .iter()
        .flat_map(|(_, names)| names.iter())
        .filter(|n| !n.chars().all(|c| c.is_alphabetic() && !is_cjk_or_thai(c)))
        .flat_map(|n| clause.match_indices(*n).map(|(i, m)| (i, i + m.len())))
        .collect();
    spans.sort_unstable();
    spans
}

/// The language code a clause (lower case) asks for. A "not" counts for the
/// name it goes with only: in 「我不會講中文可以講英文嗎」 or "I don't speak
/// Chinese can you speak English", English is asked for.
fn clause_asks(clause: &str, short_turn: bool) -> Option<&'static str> {
    let words: Vec<&str> = clause
        .split(|c: char| !(c.is_alphabetic() || c == '\''))
        .filter(|w| !w.is_empty())
        .collect();
    let question = clause.trim_end().ends_with(['?', '？']);
    let request_word = words.iter().any(|w| REQUEST_WORDS.contains(w));
    let cue = question || REQUEST_CUES.iter().any(|m| clause.contains(*m));
    let spans = cjk_name_spans(clause);
    for (code, names) in LANGUAGE_NAMES {
        for &name in names.iter() {
            let asked = if name.chars().all(|c| c.is_alphabetic() && !is_cjk_or_thai(c)) {
                // A word of Latin or Cyrillic letters.
                let Some(at) = words.iter().position(|w| *w == name) else { continue };
                // The "not" before this name: from a little before its verb, after
                // the name before it ("Ich spreche kein Englisch, können wir Deutsch…").
                let after_name = words[..at].iter().rposition(|w| is_latin_name(w)).map_or(0, |i| i + 1);
                let anchor = words[..at].iter().rposition(|w| REQUEST_WORDS.contains(w)).unwrap_or(at);
                let from = anchor.saturating_sub(NEGATION_BEFORE_WORDS).max(after_name);
                let near = anchor.saturating_sub(NEGATION_NEAR_WORDS);
                if words[from..at]
                    .iter()
                    .enumerate()
                    .any(|(i, w)| NEGATION_WORDS.contains(w) && (from + i >= near || !NEGATION_NEAR_ONLY.contains(w)))
                {
                    continue;
                }
                request_word
                    || (at > 0 && REQUEST_BEFORE.contains(&words[at - 1]))
                    || (question && words.len() <= 3)
                    || (short_turn && words.len() <= 2)
            } else {
                let Some(at) = clause.find(name) else { continue };
                let end = at + name.len();
                let before = clause[..at].trim_end();
                let after = &clause[end..];
                // The "not" around this name, up to the names next to it.
                let prev_end = spans.iter().filter(|s| s.1 <= at).map(|s| s.1).max().unwrap_or(0);
                let next_start = spans.iter().filter(|s| s.0 >= end).map(|s| s.0).min().unwrap_or(clause.len());
                let near_before: String = {
                    let b: Vec<char> = clause[prev_end..at].chars().collect();
                    b[b.len().saturating_sub(NEGATION_BEFORE_CHARS)..].iter().collect()
                };
                let near_after: String = clause[end..next_start].chars().take(NEGATION_AFTER_CHARS).collect();
                // 「能不能」「會不會」 ask; their 「不能」「不會」 are no "not".
                let near_before = A_NOT_A.iter().fold(near_before, |s, q| s.replace(q, "？"));
                if NEGATION_BEFORE_NAME.iter().any(|m| near_before.contains(m))
                    || NEGATION_AFTER_NAME.iter().any(|m| near_after.contains(m))
                {
                    continue;
                }
                request_word
                    || STRONG_BEFORE.iter().any(|m| before.ends_with(*m))
                    || STRONG_AFTER.iter().any(|m| after.starts_with(*m))
                    || (cue
                        && (WEAK_BEFORE.iter().any(|m| before.ends_with(*m))
                            || WEAK_AFTER.iter().any(|m| after.starts_with(*m))))
                    || (question && words_or_letters(clause) <= words_or_letters(name) + 2)
                    || (short_turn && clause.chars().filter(|c| c.is_alphabetic()).count() <= name.chars().count())
            };
            if asked {
                return Some(code);
            }
        }
    }
    None
}

fn is_cjk_or_thai(c: char) -> bool {
    matches!(script_of(c), Some(Script::Cjk)) || ('\u{0E00}'..='\u{0E7F}').contains(&c)
}

/// The core language of a one-word greeting or "yes" ("Sí", "Oi", "Ja", 「はい」,
/// 「喂」), when it is one only that language uses.
fn short_word_lang(said: &str) -> Option<Lang> {
    const WORDS: &[(Lang, &[&str])] = &[
        (Lang::En, &["yes", "yeah", "yep", "hi", "hello", "hey"]),
        (Lang::Es, &["sí", "hola", "aló"]),
        (Lang::Pt, &["sim", "oi", "olá", "alô"]),
        (Lang::Fr, &["oui", "allô", "salut", "bonjour", "bonsoir"]),
        (Lang::De, &["ja", "hallo", "nein", "genau"]),
        (Lang::Ja, &["はい", "もしもし", "ええ", "うん"]),
        (Lang::Ko, &["네", "예", "여보세요", "응"]),
        (Lang::ZhTw, &["喂", "你好", "嗯", "是的", "對", "对", "好的"]),
    ];
    let w: String = said
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    WORDS.iter().find(|(_, ws)| ws.contains(&w.as_str())).map(|(l, _)| *l)
}

/// Chinese, Japanese and Korean letters in `said`.
fn cjk_letters(said: &str) -> usize {
    said.chars()
        .filter(|c| matches!(*c as u32, 0x3040..=0x30FF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF))
        .count()
}

/// Words of Latin letters (or other alphabetic letters below U+2000) in `said`.
fn latin_words(said: &str) -> usize {
    said.split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|w| !w.is_empty() && w.chars().all(|c| c == '\'' || (c.is_alphabetic() && (c as u32) < 0x2000)))
        .count()
}

/// Latin words plus Chinese, Japanese and Korean letters in `said`.
fn words_or_letters(said: &str) -> usize {
    cjk_letters(said) + latin_words(said)
}

/// Chinese characters (Japanese kanji too) in said.
fn han_letters(said: &str) -> usize {
    said.chars().filter(|c| matches!(*c as u32, 0x4E00..=0x9FFF)).count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    /// Chinese, Japanese and Korean letters.
    Cjk,
    Latin,
    /// Any other letters (Thai, Cyrillic, Arabic…).
    Other,
}

fn script_of(c: char) -> Option<Script> {
    match c as u32 {
        0x3040..=0x30FA | 0x30FC..=0x30FF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => {
            Some(Script::Cjk)
        }
        _ if c.is_ascii_alphabetic() => Some(Script::Latin),
        0xC0..=0x24F | 0x1E00..=0x1EFF if c != '\u{D7}' && c != '\u{F7}' => Some(Script::Latin),
        _ if c.is_alphabetic() => Some(Script::Other),
        _ => None,
    }
}

/// A caller turn cut where it goes over from one script to another: each cut
/// starts a run of [PART_MIN] letters (Latin: words) of a script other than
/// the part before it. Stray words of another script ("OK", 「LINE」, a name)
/// stay in the part around them, and a turn in one script is one part.
fn script_parts(said: &str) -> Vec<&str> {
    // Runs of one script: (script, where it starts, its letters or words).
    let mut runs: Vec<(Script, usize, usize)> = Vec::new();
    let mut in_word = false;
    for (i, c) in said.char_indices() {
        let Some(s) = script_of(c) else {
            in_word = false;
            continue;
        };
        let counts = s != Script::Latin || !in_word;
        in_word = s == Script::Latin;
        match runs.last_mut() {
            Some(r) if r.0 == s => r.2 += usize::from(counts),
            _ => runs.push((s, i, 1)),
        }
    }
    let mut cuts = vec![0];
    let mut last: Option<Script> = None;
    for &(s, start, _) in runs.iter().filter(|r| r.2 >= PART_MIN) {
        if last.is_some_and(|l| l != s) {
            cuts.push(start);
        }
        last = Some(s);
    }
    cuts.push(said.len());
    cuts.windows(2).map(|w| &said[w[0]..w[1]]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::urgency::{caller_language, read_language};

    fn zh_call() -> CallLanguage {
        CallLanguage::new(Lang::ZhTw, Lang::ZhTw)
    }

    #[test]
    fn stray_foreign_letters_and_words_leave_chinese() {
        // Two kana that speech-to-text wrote into Chinese.
        assert_eq!(caller_language("我是你樓下的鄰居の，那個包裹は放在管理室了"), Some(Lang::ZhTw));
        // Taiwan Mandarin with English words in it.
        assert_eq!(caller_language("好啦OK，那我等一下用LINE傳給你"), Some(Lang::ZhTw));
        assert_eq!(caller_language("這個案子你先hold住，OK嗎"), Some(Lang::ZhTw));
        // A foreign name written with 「・」.
        assert_eq!(caller_language("我是幫約翰・史密斯送貨的"), Some(Lang::ZhTw));
        let mut c = zh_call();
        assert_eq!(c.caller_turn("好啦OK，那我等一下用LINE傳給你"), None);
        assert_eq!(c.caller_turn("我是你樓下的鄰居の，那個包裹は放在管理室了"), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
    }

    #[test]
    fn whole_sentences_tell_their_language() {
        let r = read_language("すみません、宅配便です。今ご自宅にいらっしゃいますか？").unwrap();
        assert_eq!(r, crate::urgency::Reading { lang: Lang::Ja, clear: true });
        let r = read_language("여보세요, 택배 왔는데 집에 계세요?").unwrap();
        assert_eq!(r, crate::urgency::Reading { lang: Lang::Ko, clear: true });
        // An English caller with a Chinese name in it.
        let r = read_language("Hi, this is 王小明 calling about the package for your office").unwrap();
        assert_eq!(r, crate::urgency::Reading { lang: Lang::En, clear: true });
        assert_eq!(caller_language("Hi, this is 王小明 calling about the package"), Some(Lang::En));
        // Too little to tell.
        assert_eq!(read_language("OK"), None);
        assert!(!read_language("喂？").is_some_and(|r| r.clear));
        assert!(!read_language("Hello?").is_some_and(|r| r.clear));
    }

    #[test]
    fn a_language_outside_the_base_needs_two_clear_turns() {
        // Without translations: the other languages read as languages without a setting.
        crate::locales_store::testing::use_empty();
        // A US SIM opens in English; the caller speaks Chinese from the start.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(c.caller_turn("喂？"), None);
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), None);
        assert_eq!(c.lang(), Some(Lang::En));
        assert_eq!(c.caller_turn("那我就幫你放在一樓管理室，等一下記得去拿喔。"), Some(Spoken::Lang(Lang::ZhTw)));
        // One English sentence: not yet.
        let mut c = zh_call();
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), None);
        assert_eq!(c.caller_turn("Can you hear me? Is this the right number?"), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
        // Back to Chinese: the English one is forgotten.
        assert_eq!(c.caller_turn("好，那我放在管理室喔"), None);
        assert_eq!(c.caller_turn("Can you hear me? Is this the right number?"), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
        // Two in a row: the call is in English now.
        assert_eq!(c.caller_turn("I need to know if you are at home right now."), Some(Spoken::Lang(Lang::En)));
        assert_eq!(c.lang(), Some(Lang::En));
        // Simplified script on a Taiwan call is the same Chinese.
        let mut c = zh_call();
        assert_eq!(c.caller_turn("你好，我是快递，你的包裹放哪里？"), None);
        assert_eq!(c.lang(), Some(Lang::ZhTw));
        // A language without a setting: two turns too.
        let mut c = zh_call();
        assert_eq!(c.caller_turn("สวัสดีครับ ผมเป็นคนส่งของ"), None);
        assert_eq!(c.caller_turn("สวัสดีครับ ผมเป็นคนส่งของ"), Some(Spoken::Other));
        assert!(c.is_other() && c.lang().is_none());
    }

    #[test]
    fn the_ai_drifting_alone_is_told_three_times_at_most() {
        let mut c = zh_call();
        c.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        assert_eq!(c.ai_turn("好，那你放管理室就好，謝謝。", true), None);
        // Japanese out of nowhere.
        let d = c.ai_turn("はい、分かりました。管理室に置いてください。", true).unwrap();
        assert_eq!((d.ai, d.call, d.turn), (Lang::Ja, Lang::ZhTw, 2));
        // English: the second one.
        let d = c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true).unwrap();
        assert_eq!((d.ai, d.call, d.turn), (Lang::En, Lang::ZhTw, 3));
        // Spanish: the third.
        let d = c.ai_turn("Hola, sí, puede dejar el paquete en la puerta, muchas gracias por todo.", true).unwrap();
        assert_eq!((d.ai, d.call, d.turn), (Lang::Es, Lang::ZhTw, 4));
        // No more after three.
        assert_eq!(c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true), None);
        // A few words in another language are no drift.
        let mut c = zh_call();
        c.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        assert_eq!(c.ai_turn("好啊OK，我等一下LINE你。", true), None);
        // Held (the caller is talking): not counted.
        assert_eq!(c.ai_turn("はい、分かりました。管理室に置いてください。", false), None);
        assert!(c.ai_turn("はい、分かりました。管理室に置いてください。", true).is_some());
    }

    #[test]
    fn the_ai_following_one_line_before_the_call_switched_is_told() {
        let mut c = zh_call();
        c.caller_turn("你好，我是快遞，你的包裹要放哪裡？");
        // The caller says one sentence in English; the AI answers in English: the
        // call is still in Chinese.
        assert_eq!(c.caller_turn("Can you hear me? Is this the right number?"), None);
        let d = c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true).unwrap();
        assert_eq!((d.ai, d.call), (Lang::En, Lang::ZhTw));
        // A second English turn: the call is in English, and English is no drift.
        assert_eq!(c.caller_turn("I need to know if you are at home right now."), Some(Spoken::Lang(Lang::En)));
        assert_eq!(c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true), None);
        // Now Chinese from the AI is the drift.
        let d = c.ai_turn("好，謝謝你，那就麻煩你放管理室了。", true).unwrap();
        assert_eq!((d.ai, d.call), (Lang::ZhTw, Lang::En));
    }

    #[test]
    fn a_base_language_takes_the_call_on_one_clear_line() {
        // A number from the US on a Taiwan SIM: opens in English, Chinese is the owner's.
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), Some(Spoken::Lang(Lang::ZhTw)));
        // The AI still in English is told at once.
        let d = c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true).unwrap();
        assert_eq!((d.ai, d.call), (Lang::En, Lang::ZhTw));
        // A clear English line takes it back.
        assert_eq!(c.caller_turn("I need to know if you are at home right now."), Some(Spoken::Lang(Lang::En)));
    }

    #[test]
    fn latin_words_of_a_language_without_a_word_list_lock_the_call_as_other() {
        for s in [
            "Jeg er på jobb nå og kommer hjem i kveld, det er ikke så langt.",
            "Habari za asubuhi, mimi niko nyumbani leo na nitakuja jioni.",
            "Aš esu namuose šiandien ir grįšiu vakare po darbo.",
        ] {
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(s), None, "{s}");
            assert_eq!(c.caller_turn(s), Some(Spoken::Other), "{s}");
            assert_eq!(c.ai_turn("Sure, just leave it with the front desk, thank you.", true), None, "{s}");
        }
        // Languages that now carry word lists (sv, fi, cs) lock to their exact language.
        crate::locales_store::testing::use_builtin();
        for (code, s) in [
            ("sv", "Hej, jag har ett paket till dig idag, är du hemma nu?"),
            ("fi", "Hei, olen kuriiri ja minulla on paketti sinulle tänään."),
            ("cs", "Dobrý den, jsem kurýr a mám pro vás dnes balíček."),
        ] {
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            let want = Lang::Ext(crate::locale::Ext::from_code(code).unwrap());
            assert_eq!(c.caller_turn(s), None, "{code}: {s}");
            assert_eq!(c.caller_turn(s), Some(Spoken::Lang(want)), "{code}: {s}");
        }
    }
}


#[cfg(test)]
mod close_latin_languages {
    use super::*;

    #[test]
    fn a_short_line_of_the_call_language_is_no_drift_but_a_clear_other_one_is() {
        let mut es = CallLanguage::new(Lang::Es, Lang::Es);
        assert_eq!(es.ai_turn("Sí, soy yo. Puedes dejarlo en la portería.", true), None);
        assert!(es.ai_turn("Olá, não estou em casa hoje, você pode deixar com o porteiro, tá bom?", true).is_some());
        let mut en = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(en.ai_turn("Sure, just leave it with the front desk, merci.", true), None);
        let mut de = CallLanguage::new(Lang::De, Lang::De);
        assert_eq!(de.ai_turn("Ja, das ist mein Paket, bitte beim Nachbarn abgeben.", true), None);
        let mut fr = CallLanguage::new(Lang::Fr, Lang::Fr);
        assert_eq!(fr.ai_turn("Oui, c'est moi, vous pouvez le laisser chez le gardien.", true), None);
    }

    #[test]
    fn one_line_outside_the_base_moves_nothing() {
        // A US SIM opens in English; the caller's first words are in another
        // language, short or not quite clear.
        for said in [
            "Hola. Le habla Carlos de DHL.",
            "Bonjour, ici la Poste, j'ai un colis pour monsieur.",
            "Guten Tag, hier ist der Paketdienst.",
            "もしもし、宅配便です。",
            "여보세요, 택배입니다.",
            "你好，我這邊是宅配。",
        ] {
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.lang(), Some(Lang::En), "{said}");
        }
    }

    #[test]
    fn a_few_stray_words_leave_the_opening_and_a_settled_call_alone() {
        // Too short to move anything.
        for said in ["Hello?", "Hello hello", "喂？", "Allô ?"] {
            let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.lang(), Some(Lang::ZhTw), "{said}");
        }
        // A call the caller already spoke clearly: one unclear turn changes nothing.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(c.caller_turn("Hi, I'm calling about the package for your office today."), None);
        assert_eq!(c.caller_turn("Hola. Le habla Carlos."), None);
        assert_eq!(c.lang(), Some(Lang::En));
        // A line not quite clear does not move it into a base language either; a clear one does.
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        assert_eq!(c.caller_turn("Hola. Le habla Carlos de DHL."), None);
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), Some(Spoken::Lang(Lang::ZhTw)));
    }

    #[test]
    fn the_ai_following_the_callers_unclear_words_is_told_but_not_a_late_request() {
        // Settled in English; the caller says a few unclear words in Spanish and
        // the AI answers in Spanish: a drift.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        c.caller_turn("Hi, I'm calling about the package for your office today.");
        c.caller_turn("Sí, sí, gracias.");
        assert!(c.ai_turn("Hola, sí, puede dejar el paquete en la puerta, muchas gracias por todo.", true).is_some());
        // The caller asked for French in words that came in late (after the AI began).
        let fr = "D'accord, je note, je lui transmets votre message, merci beaucoup.";
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        c.caller_turn("Hi, I'm calling about the package for your office today.");
        assert_eq!(c.ai_turn_after(fr, "Bonjour, je préfère parler français, s'il vous plaît.", true), None);
        // Without them, the same French turn is a drift from English…
        assert!(c.ai_turn(fr, true).is_some());
        // …and with late words that only sound French, too.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert!(c.ai_turn_after(fr, "Oui, oui, merci.", true).is_some());
    }

    #[test]
    fn multi_turn_unlisted_latin_locks_as_other_without_mislocking() {
        // Without translations: the other languages read as languages without a setting.
        crate::locales_store::testing::use_empty();
        let cases = [
            (
                "it",
                "Ciao, sono Marco, ho un pacco per lei da consegnare oggi.",
                "Grazie mille, però posso lasciarlo in portineria adesso?",
                "Prego, lasci pure il pacco però grazie mille per la consegna.",
            ),
            (
                "vi",
                "Xin chào, tôi là nhân viên giao hàng, anh có ở nhà không ạ?",
                "Cảm ơn anh nhiều nhé, tôi gửi bưu kiện ở quầy lễ tân ạ.",
                "Vâng cảm ơn anh, cứ để bưu kiện ở đây giúp tôi nhé.",
            ),
            (
                "id",
                "Halo, saya kurir paket, bapak atau ibu ada di rumah sekarang?",
                "Terima kasih, paketnya sudah saya titipkan kepada satpam ya.",
                "Baik terima kasih banyak, silakan titip paketnya di depan.",
            ),
            (
                "tr",
                "Merhaba efendim, kargonuz kapıda bekliyor, evde misiniz?",
                "Teşekkürler efendim, paketinizi kapıya bırakabilir miyim şimdi?",
                "Tamam teşekkür ederim, kapıya bırakabilirsiniz efendim.",
            ),
            (
                "pl",
                "Dzień dobry, dzwonię z firmy kurierskiej, mam paczkę dla pani.",
                "Dziękuję bardzo, czy można zostawić paczkę u sąsiada?",
                "Tak, dziękuję bardzo, proszę zostawić paczkę u ochrony.",
            ),
            (
                "nl",
                "Goedemorgen, ik bel voor een bezorging en heb een pakketje bij me.",
                "Dank u wel, zal ik het pakket bij de buren neerzetten?",
                "Ja bedankt, zet het pakketje maar bij de buren neer.",
            ),
        ];
        for (code, turn1, turn2, ai_reply) in cases {
            for default in [Lang::En, Lang::Es, Lang::Fr, Lang::Pt, Lang::ZhTw] {
                let mut call = CallLanguage::new(default, default);
                assert_eq!(call.caller_turn(turn1), None, "{code} from {default:?} turn1 waits");
                assert!(!call.is_other(), "{code} from {default:?}: not after one turn");
                assert_eq!(call.caller_turn(turn1), Some(Spoken::Other), "{code} from {default:?} turn2");
                assert!(call.is_other(), "{code} from {default:?} should lock Other");
                assert_eq!(call.lang(), None, "{code} must not lock as setting lang");
                assert_eq!(call.caller_turn(turn2), None, "{code} from {default:?} turn3 should stay Other");
                assert!(call.is_other(), "{code} must remain Other after turn3");
                assert_eq!(call.ai_turn(ai_reply, true), None, "{code} AI reply must not trigger drift");
            }
        }
    }

    #[test]
    fn multi_turn_setting_latin_languages_lock_and_switch_after_two_turns() {
        let scripts = [
            (
                Lang::Es,
                "Hola, tengo un paquete para usted, ¿lo dejo en la portería?",
                "Sí, muchas gracias, ya se lo dejo con el portero ahora.",
                "Perfecto, muchas gracias, puede dejarlo con el portero.",
            ),
            (
                Lang::Fr,
                "Bonjour, je suis le livreur, vous êtes chez vous pour le colis ?",
                "Très bien, je vais laisser le colis chez votre gardien.",
                "D'accord, merci beaucoup, laissez le colis chez le gardien.",
            ),
            (
                Lang::De,
                "Guten Tag, ich habe ein Paket für Sie, sind Sie gerade zu Hause?",
                "Kein Problem, dann gebe ich das Paket beim Nachbarn ab.",
                "Vielen Dank, geben Sie das Paket bitte beim Nachbarn ab.",
            ),
            (
                Lang::Pt,
                "Olá, sou o entregador, você está em casa para receber o pacote?",
                "Tudo bem, vou deixar o pacote com o porteiro do prédio.",
                "Obrigado, pode deixar o pacote com o porteiro agora.",
            ),
            (
                Lang::En,
                "Hello, I have a delivery for you, are you at home right now?",
                "No worries, I will leave the package with your front desk.",
                "Thanks, please leave the package with the front desk.",
            ),
        ];
        for (lang, t1, t2, ai) in scripts {
            let start = if lang == Lang::En { Lang::Es } else { Lang::En };
            let mut call = CallLanguage::new(start, start);
            assert_eq!(call.caller_turn(t1), None, "{lang:?} turn1 waits");
            assert_eq!(call.lang(), Some(start));
            assert_eq!(call.caller_turn(t1), Some(Spoken::Lang(lang)), "{lang:?} turn2 switches");
            assert_eq!(call.caller_turn(t2), None, "{lang:?} turn3 steady");
            assert_eq!(call.ai_turn(ai, true), None, "{lang:?} matching AI reply should not drift");
            // Two turns in another setting Latin language (not the opening's) switch cleanly.
            let next = if lang == Lang::Es || lang == Lang::En { Lang::Pt } else { Lang::Es };
            let (_, nt1, nt2, _) = scripts.iter().find(|(l, _, _, _)| *l == next).unwrap();
            assert_eq!(call.caller_turn(nt1), None, "{lang:?} -> {next:?} needs 2nd turn confirmation");
            assert_eq!(call.lang(), Some(lang));
            assert_eq!(call.caller_turn(nt2), Some(Spoken::Lang(next)), "{lang:?} -> {next:?} switches on turn2");
            assert_eq!(call.lang(), Some(next));
        }
    }

    #[test]
    fn spoken_reports_variant_and_pending_survives_neutral_turn_only() {
        // Without translations: the other languages read as languages without a setting.
        crate::locales_store::testing::use_empty();
        let mut call = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        assert_eq!(call.spoken(), Spoken::Lang(Lang::ZhTw));

        let mut other = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        other.caller_turn("สวัสดีครับ ผมเป็นคนส่งของครับ");
        other.caller_turn("สวัสดีครับ ผมเป็นคนส่งของครับ");
        assert!(other.is_other());
        assert_eq!(other.spoken(), Spoken::Other);

        // Settle call on the initial language before switching.
        assert_eq!(call.caller_turn("喂你好，請問包裹放哪裡？"), None);
        // First English turn sets pending.
        assert_eq!(call.caller_turn("Hello, I am calling about your parcel delivery"), None);
        // Short neutral acknowledgement does not wipe pending.
        assert_eq!(call.caller_turn("Okay"), None);
        // Second English turn still promotes the pending language switch.
        assert_eq!(call.caller_turn("Where should I leave the package?"), Some(Spoken::Lang(Lang::En)));

        // Different-language short turn between two Spanish turns clears pending.
        let mut call2 = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(call2.caller_turn("Hello, how are you doing today?"), None);
        assert_eq!(call2.caller_turn("Buenos días, llamo por su cita de mañana"), None);
        assert_eq!(call2.caller_turn("こんにちは"), None);
        assert_eq!(call2.caller_turn("Confirmamos el horario de mañana"), None);
        assert_eq!(call2.lang(), Some(Lang::En));
    }

    fn ext(code: &str) -> Lang {
        Lang::Ext(crate::locale::Ext::from_code(code).unwrap())
    }

    const IT_SAID: &str = "Buongiorno, sono il corriere, ho un pacco per lei, posso lasciarlo in portineria?";
    const TH_SAID: &str = "สวัสดีค่ะ มีพัสดุมาส่งค่ะ อยู่บ้านไหมคะ";
    const AR_SAID: &str = "مرحبا، أنا من شركة التوصيل، عندي طرد لك اليوم";
    const FA_SAID: &str = "سلام، از شرکت پست تماس می‌گیرم، یک بسته برای شما دارم";

    #[test]
    fn translated_languages_are_told_by_their_words() {
        crate::locales_store::testing::use_table();
        for (said, code) in [(IT_SAID, "it"), (TH_SAID, "th"), (AR_SAID, "ar")] {
            assert_eq!(crate::urgency::read_other(said).and_then(|r| r.lang), Some(ext(code)), "{said}");
            // An English call: two clear turns move it.
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.caller_turn(said), Some(Spoken::Lang(ext(code))), "{said}");
            assert_eq!(c.lang(), Some(ext(code)));
        }
        // Without the translations they are languages without a setting.
        crate::locales_store::testing::use_empty();
        for said in [IT_SAID, TH_SAID, AR_SAID] {
            assert_eq!(crate::urgency::read_other(said).and_then(|r| r.lang), None, "{said}");
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.caller_turn(said), Some(Spoken::Other), "{said}");
        }
    }

    #[test]
    fn a_call_in_a_translated_language_holds_and_changes_only_when_certain() {
        crate::locales_store::testing::use_table();
        // An Italian call: Italian keeps it; an "OK" changes nothing; asking for
        // English changes it at once.
        let mut c = CallLanguage::new(ext("it"), ext("it"));
        assert_eq!(c.caller_turn(IT_SAID), None);
        assert_eq!(c.caller_turn("OK"), None);
        assert_eq!(
            c.caller_turn("Sorry, I do not speak Italian, can we talk in English please?"),
            Some(Spoken::Lang(Lang::En))
        );
        // An Arabic call: Persian words share its letters and keep it.
        let mut c = CallLanguage::new(ext("ar"), ext("ar"));
        assert_eq!(c.caller_turn(AR_SAID), None);
        assert_eq!(c.caller_turn(FA_SAID), None);
        assert_eq!(c.lang(), Some(ext("ar")));
        // A Thai call: Thai holds; Japanese once does not change it.
        let mut c = CallLanguage::new(ext("th"), ext("th"));
        assert_eq!(c.caller_turn(TH_SAID), None);
        assert_eq!(c.caller_turn("すみません、宅配便です。今ご自宅にいらっしゃいますか？"), None);
        assert_eq!(c.caller_turn(TH_SAID), None);
        assert_eq!(c.lang(), Some(ext("th")));
    }

    /// A call in `from` that hears `turns` after its opening line `first`;
    /// returns what each turn changed and where the call ends up.
    fn run(from: &str, first: &str, turns: &[&str]) -> (Vec<Option<Spoken>>, Option<Lang>) {
        let mut c = CallLanguage::new(ext(from), ext(from));
        assert_eq!(c.caller_turn(first), None, "{first}");
        let changes = turns.iter().map(|t| c.caller_turn(t)).collect();
        (changes, c.lang())
    }

    #[test]
    fn a_translated_call_follows_another_language_of_its_script_told_by_its_words() {
        crate::locales_store::testing::use_builtin();
        let to = |code: &str| Some(Spoken::Lang(ext(code)));
        // Italian to Indonesian and to Vietnamese, Polish to Turkish (Latin
        // letters), Ukrainian to Russian and back (Cyrillic): two clear turns.
        for (from, first, turns, code) in [
            (
                "it",
                IT_SAID,
                [
                    "Maaf, saya tidak bisa datang hari ini, nanti saya telepon lagi ya.",
                    "Iya, paketnya titip di rumah tetangga saja, terima kasih.",
                ],
                "id",
            ),
            (
                "it",
                IT_SAID,
                ["Dạ, tôi không có ở nhà, anh gọi lại sau nhé.", "Vâng, anh cứ để hàng ở cửa giúp tôi, cảm ơn anh."],
                "vi",
            ),
            (
                "pl",
                "Dzień dobry, jestem kurierem, czy pani jest teraz w domu?",
                ["Merhaba, kargo için arıyorum, şimdi evde misiniz?", "Evet, tamam, teşekkür ederim, yarın tekrar ararım."],
                "tr",
            ),
            (
                "uk",
                "Добрий день, це кур'єр, ви сьогодні вдома?",
                ["Здравствуйте, это курьер, вы сейчас дома?", "Хорошо, тогда я оставлю посылку у соседей, если можно."],
                "ru",
            ),
            (
                "ru",
                "Здравствуйте, это курьер, вы сейчас дома?",
                ["Вибачте, я не розумію, можна українською, будь ласка?", "Добре, залиште посилку біля дверей, я її потім заберу."],
                "uk",
            ),
            (
                "ar",
                AR_SAID,
                [FA_SAID, FA_SAID],
                "fa",
            ),
            (
                "ar",
                AR_SAID,
                [
                    "السلام علیکم، میں کورئیر کمپنی سے بات کر رہا ہوں، آپ گھر پر ہیں؟",
                    "جی شکریہ، میں آپ کا پارسل آج گھر کے دروازے پر رکھ دیتا ہوں۔",
                ],
                "ur",
            ),
        ] {
            let (changes, lang) = run(from, first, &turns);
            assert_eq!(changes, vec![None, to(code)], "{from} -> {code}");
            assert_eq!(lang, Some(ext(code)), "{from} -> {code}");
        }
        // One such turn between turns of the call's language changes nothing.
        let (_, lang) = run("it", IT_SAID, &["Maaf, saya tidak bisa datang hari ini, nanti saya telepon lagi ya.", IT_SAID]);
        assert_eq!(lang, Some(ext("it")));
    }

    #[test]
    fn a_translated_call_holds_on_words_that_do_not_tell_another_language() {
        crate::locales_store::testing::use_builtin();
        for (from, first, turns) in [
            // One Persian turn then one Urdu turn (not two of the same language), or Pashto, keep an Arabic call.
            ("ar", AR_SAID, [FA_SAID, "السلام علیکم، میں کورئیر کمپنی سے بات کر رہا ہوں، آپ گھر پر ہیں؟"]),
            ("ar", AR_SAID, ["سلام، زه د پوست له شرکت څخه زنګ وهم، ستاسو لپاره بسته لرم", "سلام، زه د پوست له شرکت څخه زنګ وهم، ستاسو لپاره بسته لرم"]),
            // An English word, a name, a short reply in an Italian call.
            ("it", IT_SAID, ["Sì, perfetto, mi mandi tutto per email, grazie, okay?", "Pronto, sono Budi Santoso, il corriere, ho un pacco per lei."]),
            ("it", IT_SAID, ["Iya, terima kasih.", "Iya, terima kasih."]),
            ("it", IT_SAID, ["Sì, grazie.", "Va bene, a domani."]),
            // Belarusian and Bulgarian are not Russian in a Ukrainian call.
            ("uk", "Добрий день, це кур'єр, ви сьогодні вдома?", ["Добры дзень, я кур'ер, вы сёння дома?", "Здравейте, аз съм куриерът, вкъщи ли сте днес?"]),
            ("uk", "Добрий день, це кур'єр, ви сьогодні вдома?", ["Так, дякую.", "Добре, дякую, до побачення."]),
        ] {
            let (changes, lang) = run(from, first, &turns);
            assert_eq!(changes, vec![None, None], "{from}: {turns:?}");
            assert_eq!(lang, Some(ext(from)), "{from}: {turns:?}");
        }
    }

    #[test]
    fn the_ai_drifting_from_a_translated_language_into_a_core_one_is_told() {
        crate::locales_store::testing::use_table();
        let mut c = CallLanguage::new(ext("it"), ext("it"));
        c.caller_turn(IT_SAID);
        // Its own language, or another read from the letters: no drift.
        assert_eq!(c.ai_turn("Sì, va bene, lo lasci pure in portineria, grazie mille.", true), None);
        assert_eq!(c.ai_turn(TH_SAID, true), None);
        // Clearly English: a drift, named for the call's language.
        let d = c.ai_turn("Okay, you can leave the package at the front desk, thank you so much.", true).expect("drift");
        assert_eq!((d.ai, d.call), (Lang::En, ext("it")));
        let note = crate::prompts::language_drift_note(ext("it"), ext("it"));
        assert!(note.contains("Italian"), "{note}");
        let mut c = CallLanguage::new(ext("ar"), ext("ar"));
        c.caller_turn(AR_SAID);
        assert_eq!(c.ai_turn("حسنًا، اترك الطرد عند الباب، شكرًا لك.", true), None);
        assert!(c.ai_turn("Hola, sí, puede dejar el paquete en la puerta, muchas gracias por todo.", true).is_some());
    }
}

/// A call the caller has spoken clearly is not carried off by one misheard line.
#[cfg(test)]
mod one_misheard_line {
    use super::*;
    use crate::urgency::read_language;

    fn clear_in(said: &str, lang: Lang) {
        let r = read_language(said).unwrap_or_else(|| panic!("nothing read: {said}"));
        assert!(r.clear && (r.lang == lang || (r.lang.is_zh() && lang.is_zh())), "{said}: {r:?}");
    }

    /// Two clear lines in each language.
    pub(super) const LINES: &[(Lang, &str, &str)] = &[
        (Lang::ZhTw, "你好，我是快遞，你的包裹要放哪裡？", "那我就幫你放在一樓管理室，等一下記得去拿喔。"),
        (
            Lang::Ja,
            "もしもし、大和運輸の田中です。お荷物のお届けに参りました。",
            "では、管理人室に預けておきますので、後で受け取ってください。",
        ),
        (Lang::Ko, "여보세요, 택배 왔는데 집에 계세요?", "네, 그럼 경비실에 맡겨 둘게요. 나중에 찾아가세요."),
        (
            Lang::Es,
            "Hola, buenas tardes, le llamo de la empresa de mensajería por un paquete para el señor.",
            "Si no está en casa, lo dejo en la portería y él lo recoge después.",
        ),
        (
            Lang::Fr,
            "Bonjour, je vous appelle pour le colis de monsieur, il est chez le gardien.",
            "Si vous n'êtes pas là, je le laisse chez le voisin et vous le récupérez ce soir.",
        ),
        (
            Lang::De,
            "Guten Tag, ich habe ein Paket für Sie, sind Sie heute zu Hause?",
            "Wenn Sie nicht da sind, gebe ich es beim Nachbarn ab und Sie holen es heute Abend.",
        ),
    ];

    /// Real call: a US SIM opened in English, the caller spoke Japanese, and
    /// one of their Japanese lines was written down as Spanish.
    #[test]
    fn a_spanish_misreading_in_a_japanese_call_does_not_move_it() {
        let (_, ja1, ja2) = LINES[1];
        let (_, es1, _) = LINES[3];
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        assert_eq!(c.caller_turn(ja1), None);
        assert_eq!(c.caller_turn(ja2), Some(Spoken::Lang(Lang::Ja)));
        let misread = "¿Cómo es lo de ustedes? ¿Qué es lo que va a hacer?";
        assert_eq!(c.caller_turn(misread), None);
        assert_eq!(c.lang(), Some(Lang::Ja));
        // The AI answering in Spanish is told to keep to Japanese.
        let d = c.ai_turn(es1, true).expect("drift");
        assert_eq!((d.ai, d.call), (Lang::Es, Lang::Ja));
        // Back in Japanese: the Spanish line is forgotten, one more changes nothing.
        assert_eq!(c.caller_turn(ja2), None);
        assert_eq!(c.caller_turn(misread), None);
        assert_eq!(c.lang(), Some(Lang::Ja));
    }

    /// Their words came in only after the AI began to answer, so they are read
    /// with the next turn: two languages in one turn are no evidence for either.
    #[test]
    fn the_late_end_of_the_last_turn_is_read_apart_from_this_one() {
        let (_, ja1, ja2) = LINES[1];
        let (_, es1, es2) = LINES[3];
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        assert_eq!(c.caller_turn(""), None);
        assert_eq!(c.caller_turn(&format!("{ja1}{es1}")), None);
        // Spanish once more is only the first in a row.
        assert_eq!(c.caller_turn(es1), None);
        assert_eq!(c.caller_turn(es2), Some(Spoken::Lang(Lang::Es)));
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        assert_eq!(c.caller_turn("もしもし、大和運輸の田中です。¿Cómo es lo de ustedes? ¿Qué es lo que va a hacer?"), None);
        c.caller_turn(ja1);
        c.caller_turn(ja2);
        assert_eq!(c.lang(), Some(Lang::Ja));
        // One script, stray words of another in it: one part.
        assert_eq!(script_parts("好啦OK，那我等一下用LINE傳給你").len(), 1);
        assert_eq!(script_parts("Hi, this is 王小明 calling about the package for your office").len(), 1);
        assert_eq!(script_parts("").len(), 1);
        assert_eq!(script_parts(&format!("{ja1}{es1}")).len(), 2);
    }

    #[test]
    fn the_first_clear_line_moves_the_opening_only_into_a_base_language() {
        for &(lang, said, two) in LINES {
            let opening = if lang == Lang::En { Lang::Es } else { Lang::En };
            let mut c = CallLanguage::new(opening, opening).with_base([lang], false);
            assert_eq!(c.caller_turn(said), Some(Spoken::Lang(lang)), "{said}");
            let mut c = CallLanguage::new(opening, opening);
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.caller_turn(two), Some(Spoken::Lang(lang)), "{two}");
        }
    }

    #[test]
    fn a_settled_call_moves_only_on_two_clear_lines_in_a_row() {
        for (i, &(from, first, _)) in LINES.iter().enumerate() {
            let (to, one, two) = LINES[(i + 1) % LINES.len()];
            clear_in(one, to);
            clear_in(two, to);
            let mut c = CallLanguage::new(from, from);
            assert_eq!(c.caller_turn(first), None, "{first}");
            assert_eq!(c.caller_turn(one), None, "{from:?}: {one}");
            assert_eq!(c.lang(), Some(from), "{one}");
            assert_eq!(c.caller_turn(two), Some(Spoken::Lang(to)), "{from:?}: {two}");
            // Back to the language it opened in, a base language: one clear line.
            let (_, back, _) = LINES[i];
            assert_eq!(c.caller_turn(back), Some(Spoken::Lang(from)), "{to:?}: {back}");
        }
    }

    #[test]
    fn kanji_alone_on_a_japanese_call_is_no_chinese() {
        let mut c = CallLanguage::new(Lang::Ja, Lang::Ja);
        c.caller_turn("もしもし、宅配便です。");
        for said in ["大和運輸田中", "管理人室", "大和運輸の田中"] {
            assert_eq!(c.caller_turn(said), None, "{said}");
        }
        assert_eq!(c.lang(), Some(Lang::Ja));
        // Whole Chinese sentences still count: two move the call.
        assert_eq!(c.caller_turn("你好，我是快遞，你的包裹要放哪裡？"), None);
        assert_eq!(c.caller_turn("那我就幫你放在一樓管理室，等一下記得去拿喔。"), Some(Spoken::Lang(Lang::ZhTw)));
    }

    const SHORT_WORDS: &[&str] = &["Sí.", "Oi.", "Hi.", "Ja.", "Да.", "はい。", "네.", "喂", "Oui.", "Sim"];

    #[test]
    fn one_or_two_stray_words_do_not_change_the_language_of_the_call() {
        for opening in [Lang::En, Lang::ZhTw, Lang::Ja, Lang::De, Lang::Fr] {
            let mut c = CallLanguage::new(opening, Lang::ZhTw);
            for said in SHORT_WORDS {
                assert_eq!(c.caller_turn(said), None, "{opening:?}: {said}");
                assert_eq!(c.lang(), Some(c.same_variant(opening)), "{opening:?}: {said}");
            }
        }
        // Also after the call has settled.
        let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
        c.caller_turn("Hello, I am calling from the courier about a package for your office");
        for said in SHORT_WORDS {
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.caller_turn(said), None, "{said}");
        }
        assert_eq!(c.lang(), Some(Lang::En));
    }

    #[test]
    fn whole_sentences_in_another_language_move_the_call_on_the_second() {
        let lines = [
            ("Buenos días, le llamo de la empresa de mensajería por un paquete que tengo para usted", Lang::Es),
            ("Bonjour, je vous appelle de la société de livraison à propos d'un colis pour vous", Lang::Fr),
            ("すみません、宅配便です。今ご自宅にいらっしゃいますか？", Lang::Ja),
            ("여보세요, 택배 왔는데 집에 계세요?", Lang::Ko),
        ];
        for (said, to) in lines {
            let mut c = CallLanguage::new(Lang::En, Lang::ZhTw);
            c.caller_turn("Sí.");
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.caller_turn(said), Some(Spoken::Lang(to)), "{said}");
        }
    }

    #[test]
    fn the_ai_following_a_stray_word_into_another_language_is_told() {
        for (said, ai) in [("Sí.", Lang::Es), ("Oi.", Lang::Pt), ("Ja.", Lang::De), ("Oui.", Lang::Fr), ("はい。", Lang::Ja), ("네.", Lang::Ko)] {
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert_eq!(c.caller_turn(said), None);
            let d = c.ai_turn(said, true).unwrap_or_else(|| panic!("{said}"));
            assert_eq!((d.ai, d.call), (ai, Lang::En), "{said}");
        }
        // The call's own greeting or "yes" is no drift.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(c.ai_turn("Hi.", true), None);
        assert_eq!(c.ai_turn("Yes.", true), None);
        let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        assert_eq!(c.ai_turn("喂？", true), None);
        let mut c = CallLanguage::new(Lang::Pt, Lang::Pt);
        assert_eq!(c.ai_turn("Oi.", true), None);
        // Not while it cannot be told.
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(c.ai_turn("Sí.", false), None);
    }

    #[test]
    fn a_short_word_on_a_call_in_a_language_without_a_setting_is_no_drift() {
        for code in ["nl", "sv", "ro"] {
            for said in ["Ja.", "Hallo.", "Salut."] {
                let mut c = CallLanguage::new(Lang::Ext(crate::locale::Ext::from_code(code).unwrap()), Lang::En);
                assert_eq!(c.ai_turn(said, true), None, "{code}: {said}");
            }
        }
    }
}

/// Only a certain change moves the call (2026-11-27): a Taiwan SIM set to
/// Traditional Chinese took calls from a US number whose 「喂」 the
/// speech-to-text wrote as 「Sí.」, 「Oi.」, 「Hallo」 or 「das Frühstück」, and the
/// AI answered in Spanish, Portuguese or German.
#[cfg(test)]
mod certain_switch {
    use super::one_misheard_line::LINES;
    use super::*;

    fn ext(code: &str) -> Lang {
        Lang::Ext(crate::locale::Ext::from_code(code).unwrap())
    }

    /// A number from the US on a Taiwan SIM (its main language Traditional
    /// Chinese): opens in English, Chinese is the owner's.
    fn tw_sim_us_number() -> CallLanguage {
        CallLanguage::new(Lang::En, Lang::ZhTw).with_base([Lang::ZhTw], false)
    }

    const ZH_LINE: &str = "你好，我是快遞，你的包裹要放哪裡？";
    const EN_LINE: &str = "I need to know if you are at home right now.";
    const TH_SAID: &str = "สวัสดีค่ะ มีพัสดุมาส่งค่ะ อยู่บ้านไหมคะ";
    const IT_SAID: &str = "Buongiorno, sono il corriere, ho un pacco per lei, posso lasciarlo in portineria?";

    /// Hellos and short words that speech-to-text may write in any language.
    const MISHEARD: &[&str] = &[
        "Oi.", "Sí.", "Hallo", "das Frühstück", "Ja.", "Hallo?", "Oi, oi.", "喂？", "はい。", "네.", "Oui.", "Да.",
        "Pronto?", "Alô?", "Allô ?", "Ciao.", "Привет.", "สวัสดี", "Xin chào", "Halo.", "Hello?", "もしもし", "여보세요",
        "你好你好", "Guten Tag", "Hola, buenas.", "Hello hello",
    ];

    #[test]
    fn a_hello_misheard_as_another_language_moves_nothing() {
        let mut c = tw_sim_us_number();
        for said in MISHEARD {
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.lang(), Some(Lang::En), "{said}");
        }
        // A clear Chinese line: Chinese; a clear English one: English again.
        assert_eq!(c.caller_turn(ZH_LINE), Some(Spoken::Lang(Lang::ZhTw)));
        for said in MISHEARD {
            assert_eq!(c.caller_turn(said), None, "{said}");
            assert_eq!(c.lang(), Some(Lang::ZhTw), "{said}");
        }
        assert_eq!(c.caller_turn(EN_LINE), Some(Spoken::Lang(Lang::En)));
    }

    #[test]
    fn two_whole_clear_sentences_outside_the_base_move_the_call_and_one_does_not() {
        crate::locales_store::testing::use_table();
        let (_, es1, es2) = LINES[3];
        let (_, ja1, ja2) = LINES[1];
        let (_, ko1, ko2) = LINES[2];
        for (one, two, to) in [
            (es1, es2, Spoken::Lang(Lang::Es)),
            (ja1, ja2, Spoken::Lang(Lang::Ja)),
            (ko1, ko2, Spoken::Lang(Lang::Ko)),
            (TH_SAID, TH_SAID, Spoken::Lang(ext("th"))),
            (IT_SAID, IT_SAID, Spoken::Lang(ext("it"))),
        ] {
            let mut c = tw_sim_us_number();
            assert_eq!(c.caller_turn(one), None, "{one}");
            assert_eq!(c.lang(), Some(Lang::En), "{one}");
            assert_eq!(c.caller_turn(two), Some(to), "{two}");
            // Only one, then the call's own language: nothing changes.
            let mut c = tw_sim_us_number();
            assert_eq!(c.caller_turn(one), None, "{one}");
            assert_eq!(c.caller_turn(EN_LINE), None, "{one}");
            assert_eq!(c.caller_turn(two), None, "{two}");
            assert_eq!(c.lang(), Some(Lang::En), "{two}");
        }
        // A clear line too short to be a whole sentence (「はい、はい。」) counts for nothing.
        let mut c = tw_sim_us_number();
        for _ in 0..3 {
            assert_eq!(c.caller_turn("はい、はい。"), None);
        }
        assert_eq!(c.lang(), Some(Lang::En));
    }

    #[test]
    fn a_plain_request_for_a_language_moves_the_call_at_once() {
        crate::locales_store::testing::use_table();
        let core: &[(Lang, Lang, &str, Lang)] = &[
            (Lang::ZhTw, Lang::ZhTw, "Sorry, I don't speak Chinese, can you speak English?", Lang::En),
            (Lang::Ja, Lang::Ja, "すみません、可以講中文嗎？", Lang::ZhTw),
            (Lang::Ko, Lang::ZhCn, "你好，可以说中文吗？", Lang::ZhCn),
            (Lang::ZhTw, Lang::ZhTw, "日本語で話してもらえますか？", Lang::Ja),
            (Lang::ZhTw, Lang::ZhTw, "영어로 말해 주세요.", Lang::En),
            (Lang::En, Lang::En, "여보세요? 한국어 할 줄 아세요?", Lang::Ko),
            (Lang::En, Lang::En, "¿Puede hablar español, por favor?", Lang::Es),
            (Lang::En, Lang::En, "Vous parlez français ?", Lang::Fr),
            (Lang::En, Lang::En, "Können wir bitte Deutsch sprechen?", Lang::De),
            (Lang::En, Lang::En, "Você fala português?", Lang::Pt),
            (Lang::Es, Lang::Es, "English, please.", Lang::En),
            (Lang::En, Lang::En, "中文。", Lang::ZhTw),
        ];
        for &(opening, prompt, said, to) in core {
            assert!(asks_for_language(said).is_some_and(|l| l == to || (l.is_zh() && to.is_zh())), "{said}");
            let mut c = CallLanguage::new(opening, prompt);
            assert_eq!(c.caller_turn(said), Some(Spoken::Lang(to)), "{said}");
        }
        // Answering a request at once is no drift, even when the request came in late.
        let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        c.caller_turn("Sorry, I don't speak Chinese, can you speak English?");
        assert_eq!(c.ai_turn("Okay, just leave it at the front desk, thank you so much.", true), None);
        let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        let (_, ja1, _) = LINES[1];
        assert_eq!(c.ai_turn_after(ja1, "日本語で話してもらえますか？", true), None);
        for (said, code) in [
            ("Parla italiano?", "it"),
            ("Говорите по-русски?", "ru"),
            ("พูดภาษาไทยได้ไหมครับ", "th"),
            ("Anh nói tiếng Việt được không?", "vi"),
            ("Bisa bahasa Indonesia?", "id"),
        ] {
            assert_eq!(asks_for_language(said), Some(ext(code)), "{said}");
            let mut c = CallLanguage::new(Lang::En, Lang::En);
            assert!(c.caller_turn(said).is_some(), "{said}");
            assert_ne!(c.lang(), Some(Lang::En), "{said}");
        }
        // A "not" counts for its own language only, with no mark between the two halves.
        for (said, to) in [
            ("我不會講中文可以講英文嗎", Lang::En),
            ("我不会讲中文可以讲英文吗", Lang::En),
            ("I don't speak Chinese can you speak English", Lang::En),
            ("No hablo inglés puede hablar español", Lang::Es),
            ("Je ne parle pas anglais vous parlez français", Lang::Fr),
            ("Ich spreche kein Englisch können wir Deutsch sprechen", Lang::De),
            ("Não falo inglês você fala português", Lang::Pt),
            ("英語は話せません日本語でお願いします", Lang::Ja),
            ("영어 못해요 한국어로 말해 주세요", Lang::Ko),
            // A "no" that opens the turn, its comma lost, is no "not" of the name.
            ("no can you speak English", Lang::En),
            ("No sorry can you speak English", Lang::En),
            ("No ¿puede hablar español?", Lang::Es),
            // 「能不能」「會不會」 ask.
            ("你能不能講英文？", Lang::En),
            ("你会不会说日文？", Lang::Ja),
            ("可不可以用英文？", Lang::En),
            ("要不要改講英文？", Lang::En),
        ] {
            assert!(asks_for_language(said).is_some_and(|l| l == to || (l.is_zh() && to.is_zh())), "{said}: {:?}", asks_for_language(said));
        }
        let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        assert_eq!(c.caller_turn("我不會講中文可以講英文嗎"), Some(Spoken::Lang(Lang::En)));
        let mut c = CallLanguage::new(Lang::En, Lang::En);
        assert_eq!(c.caller_turn("No hablo inglés puede hablar español"), Some(Spoken::Lang(Lang::Es)));
        // A "not", a statement or a question about a word asks for nothing.
        for said in [
            "我不要講英文",
            "我不要讲英文",
            "我不會講英文也不會講日文",
            "I don't want to speak English",
            "No quiero hablar inglés",
            "I don't speak Chinese.",
            "我不會講英文",
            "Je ne parle pas anglais.",
            "Ich spreche kein Spanisch.",
            "Não falo inglês.",
            "No hablo francés.",
            "英語は話せません。",
            "영어 못해요.",
            "I'm calling from the English school about your son's class.",
            "這份文件是英文的",
            "這個字用英文怎麼說？",
            "他說英文名字叫 Tom",
            // Asking not to speak a language, politely or as a question.
            "可以不講英文嗎？",
            "可以不讲英文吗？",
            "我不想講英文可以嗎？",
            "不用講英文了嗎？",
            "能不能不說日文？",
            "I wouldn't speak English",
            "Don't ask me to speak English",
            "Please stop speaking English",
            "I didn't speak English",
            "Je ne veux pas parler anglais",
        ] {
            assert_eq!(asks_for_language(said), None, "{said}");
        }
    }

    #[test]
    fn other_sims_and_numbers_keep_their_language_on_short_words() {
        let (_, ja1, _) = LINES[1];
        let (_, de1, _) = LINES[5];
        // A Japanese SIM taking a Korean number: opens in Korean, Japanese is the SIM's.
        let jp_kr = || CallLanguage::new(Lang::Ko, Lang::Ja);
        // A German owner taking a French number.
        let de_fr = || CallLanguage::new(Lang::Fr, Lang::De);
        // An English owner, and a Simplified Chinese one, taking a local number.
        let en = || CallLanguage::new(Lang::En, Lang::En);
        let cn = || CallLanguage::new(Lang::ZhCn, Lang::ZhCn);
        for (name, mut c, lang) in [
            ("jp sim, kr number", jp_kr(), Lang::Ko),
            ("de owner, fr number", de_fr(), Lang::Fr),
            ("en owner, local", en(), Lang::En),
            ("zh-CN owner, local", cn(), Lang::ZhCn),
        ] {
            for said in MISHEARD {
                assert_eq!(c.caller_turn(said), None, "{name}: {said}");
                assert_eq!(c.lang(), Some(lang), "{name}: {said}");
            }
        }
        // Their base languages still move on one clear line.
        let mut c = jp_kr();
        assert_eq!(c.caller_turn(ja1), Some(Spoken::Lang(Lang::Ja)));
        let mut c = de_fr();
        assert_eq!(c.caller_turn(de1), Some(Spoken::Lang(Lang::De)));
    }

    #[test]
    fn the_ai_moving_to_a_third_language_is_told_even_after_a_misread_word() {
        let (_, es1, _) = LINES[3];
        let (_, ja1, _) = LINES[1];
        for (caller, ai, lang) in [
            ("Sí.", "Sí.", Lang::Es),
            ("Oi.", "Oi.", Lang::Pt),
            ("Hallo", "Hallo?", Lang::De),
            ("das Frühstück", "Hallo?", Lang::De),
            ("Sí, sí, gracias.", es1, Lang::Es),
            ("はい、はい。", ja1, Lang::Ja),
        ] {
            let mut c = tw_sim_us_number();
            c.caller_turn(caller);
            let d = c.ai_turn(ai, true).unwrap_or_else(|| panic!("{caller} -> {ai}"));
            assert_eq!((d.ai, d.call), (lang, Lang::En), "{caller} -> {ai}");
        }
        // After the call moved to Chinese, Spanish from the AI is a drift too.
        let mut c = tw_sim_us_number();
        c.caller_turn(ZH_LINE);
        c.caller_turn("Sí.");
        let d = c.ai_turn("Sí.", true).expect("drift");
        assert_eq!((d.ai, d.call), (Lang::Es, Lang::ZhTw));
    }

    #[test]
    fn with_reply_in_callers_language_off_the_call_keeps_to_its_base() {
        let (_, es1, es2) = LINES[3];
        let mut c = tw_sim_us_number().following_others(false);
        assert_eq!(c.caller_turn(es1), None);
        assert_eq!(c.caller_turn(es2), None);
        assert_eq!(c.caller_turn("¿Puede hablar español, por favor?"), None);
        assert_eq!(c.lang(), Some(Lang::En));
        assert!(c.ai_turn_after(es1, "¿Puede hablar español, por favor?", true).is_some());
        // Its base languages still.
        assert_eq!(c.caller_turn(ZH_LINE), Some(Spoken::Lang(Lang::ZhTw)));
        assert_eq!(c.caller_turn("Can you speak English, please?"), Some(Spoken::Lang(Lang::En)));
    }

    #[test]
    fn a_call_opened_the_local_way_takes_that_language_on_one_line() {
        // Italian without a setting here: an Italian SIM opened the Italian way.
        crate::locales_store::testing::use_empty();
        let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw).with_base(std::iter::empty(), true);
        assert_eq!(c.caller_turn(IT_SAID), Some(Spoken::Other));
        // Without it, two lines.
        let mut c = CallLanguage::new(Lang::ZhTw, Lang::ZhTw);
        assert_eq!(c.caller_turn(IT_SAID), None);
        assert_eq!(c.caller_turn(IT_SAID), Some(Spoken::Other));
    }
}

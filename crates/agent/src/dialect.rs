//! Dialects and regional ways of speaking a call may meet, by the region of the
//! SIM that took it ([`crate::country_profile`]), and the local check that tells
//! when the caller really speaks one (scenario plan, module 4).
//!
//! The Live model hears the audio and switches in the same session, following
//! the gate its prompt describes ([`crate::prompts::scope_to_region`]). This
//! module only keeps track of it for the call: pure string checks on a finished
//! turn (microseconds, never on the audio path, no reconnect), so a rotated
//! session is told which dialect the call is in.

use crate::country_profile::Region;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// Taiwanese / Minnan / Singapore-Malaysia Hokkien.
    Hokkien,
    Hakka,
    Cantonese,
    /// Sichuan / Chongqing (southwestern Mandarin).
    Sichuanese,
    /// Singlish / Manglish (English with lah, lor, meh).
    Singlish,
    /// Canadian French.
    Quebec,
    /// British and Irish regional English.
    BritishRegional,
    /// Australian / New Zealand English.
    Aussie,
    IndianEnglish,
    Kansai,
    Gyeongsang,
    /// Spanish of Spain (vale, vosotros).
    PeninsularSpanish,
    /// Mexican, Andean and Caribbean Spanish.
    LatinSpanish,
    /// Argentina / Uruguay (vos, dale).
    Rioplatense,
    /// Belgian / Swiss French numbers.
    BelgianSwissFrench,
    /// Bavarian / Austrian German.
    Bavarian,
    SwissGerman,
    EuropeanPortuguese,
    BrazilianPortuguese,
    /// Standard Mandarin (普通話), on a line whose own language is Cantonese
    /// (Hong Kong, Macau): switched to only when the caller asks for it —
    /// speech-to-text writes Cantonese as standard Chinese, so a sentence of it
    /// tells nothing.
    Mandarin,
}

impl Dialect {
    /// Short code for logs (`dialect_switch`, `persona_prompt dialects=`).
    pub fn code(self) -> &'static str {
        match self {
            Dialect::Hokkien => "nan",
            Dialect::Hakka => "hak",
            Dialect::Cantonese => "yue",
            Dialect::Sichuanese => "sichuan",
            Dialect::Singlish => "singlish",
            Dialect::Quebec => "fr-qc",
            Dialect::BritishRegional => "en-gb",
            Dialect::Aussie => "en-au",
            Dialect::IndianEnglish => "en-in",
            Dialect::Kansai => "kansai",
            Dialect::Gyeongsang => "gyeongsang",
            Dialect::PeninsularSpanish => "es-es",
            Dialect::LatinSpanish => "es-419",
            Dialect::Rioplatense => "es-rp",
            Dialect::BelgianSwissFrench => "fr-be",
            Dialect::Bavarian => "de-by",
            Dialect::SwissGerman => "gsw",
            Dialect::EuropeanPortuguese => "pt-pt",
            Dialect::BrazilianPortuguese => "pt-br",
            Dialect::Mandarin => "cmn",
        }
    }

    /// A spoken variety of its own: the AI switches to it (in whole sentences)
    /// once the gate holds. The others are ways of speaking the call's language:
    /// understood and followed in wording, never put on.
    pub fn switches(self) -> bool {
        matches!(self, Dialect::Hokkien | Dialect::Hakka | Dialect::Cantonese | Dialect::Sichuanese | Dialect::Mandarin)
    }

    /// Its name in Chinese instructions (Traditional; converted for Simplified)
    /// and in English ones, as people of `region` call it.
    pub fn name(self, region: Region, zh: bool) -> &'static str {
        match (self, zh) {
            (Dialect::Hokkien, true) => match region {
                Region::Tw => "台語",
                Region::MySg => "福建話",
                _ => "閩南話",
            },
            (Dialect::Hokkien, false) => match region {
                Region::Tw => "Taiwanese Hokkien",
                Region::MySg => "Hokkien",
                _ => "Minnan",
            },
            (Dialect::Hakka, true) => "客語",
            (Dialect::Hakka, false) => "Hakka",
            (Dialect::Cantonese, true) => "粵語",
            (Dialect::Cantonese, false) => "Cantonese",
            (Dialect::Sichuanese, true) => "四川話",
            (Dialect::Sichuanese, false) => "Sichuanese",
            (Dialect::Singlish, true) => "Singlish",
            (Dialect::Singlish, false) => "Singlish / Manglish",
            (Dialect::Quebec, true) => "魁北克法語",
            (Dialect::Quebec, false) => "Quebec French",
            (Dialect::BritishRegional, true) => "英國各地",
            (Dialect::BritishRegional, false) => "British and Irish regional English",
            (Dialect::Aussie, true) => "澳紐英語",
            (Dialect::Aussie, false) => "Australian / New Zealand English",
            (Dialect::IndianEnglish, true) => "印度英語",
            (Dialect::IndianEnglish, false) => "Indian English",
            (Dialect::Kansai, true) => "關西腔",
            (Dialect::Kansai, false) => "Kansai dialect",
            (Dialect::Gyeongsang, true) => "慶尚道方言",
            (Dialect::Gyeongsang, false) => "Gyeongsang dialect",
            (Dialect::PeninsularSpanish, true) => "西班牙本土",
            (Dialect::PeninsularSpanish, false) => "Spain Spanish",
            (Dialect::LatinSpanish, true) => "拉美西語",
            (Dialect::LatinSpanish, false) => "Latin American Spanish",
            (Dialect::Rioplatense, true) => "阿根廷西語",
            (Dialect::Rioplatense, false) => "Rioplatense Spanish",
            (Dialect::BelgianSwissFrench, true) => "比利時、瑞士法語",
            (Dialect::BelgianSwissFrench, false) => "Belgian / Swiss French",
            (Dialect::Bavarian, true) => "巴伐利亞、奧地利德語",
            (Dialect::Bavarian, false) => "Bavarian / Austrian German",
            (Dialect::SwissGerman, true) => "瑞士德語",
            (Dialect::SwissGerman, false) => "Swiss German",
            (Dialect::EuropeanPortuguese, true) => "葡萄牙葡語",
            (Dialect::EuropeanPortuguese, false) => "European Portuguese",
            (Dialect::BrazilianPortuguese, true) => "巴西葡語",
            (Dialect::BrazilianPortuguese, false) => "Brazilian Portuguese",
            (Dialect::Mandarin, true) => "普通話",
            (Dialect::Mandarin, false) => "Mandarin",
        }
    }

    /// A few of its own words, as examples in the prompt (in its own script).
    pub fn examples(self) -> &'static [&'static str] {
        match self {
            Dialect::Hokkien => &["佇咧", "按怎"],
            Dialect::Hakka => &["屋下", "仰般"],
            Dialect::Cantonese => &["唔該", "係咪", "點樣"],
            Dialect::Sichuanese => &["啥子", "莫得"],
            Dialect::Singlish => &["lah", "lor"],
            Dialect::Quebec => &["char", "magasiner"],
            Dialect::BritishRegional => &["cheers", "ta"],
            Dialect::Aussie => &["no worries", "arvo"],
            Dialect::IndianEnglish => &["do the needful", "prepone"],
            Dialect::Kansai => &["せやな", "ほんま", "おおきに"],
            Dialect::Gyeongsang => &["머하노", "단디"],
            Dialect::PeninsularSpanish => &["vale", "vosotros"],
            Dialect::LatinSpanish => &["¿bueno?", "ahorita"],
            Dialect::Rioplatense => &["vos", "dale"],
            Dialect::BelgianSwissFrench => &["septante", "nonante"],
            Dialect::Bavarian => &["Servus"],
            Dialect::SwissGerman => &["Grüezi"],
            Dialect::EuropeanPortuguese => &["pois", "tu"],
            Dialect::BrazilianPortuguese => &["você", "tá bom"],
            Dialect::Mandarin => &["是不是", "什麼"],
        }
    }

    /// Words of the dialect alone (its grammar and core words), as speech-to-text
    /// writes them (in Traditional script: see [marker_hits]). One of them is a
    /// hint; two in a sentence is the sentence. Hokkien also counts a question
    /// ending on 「無」 ([bo_question]).
    fn markers(self) -> &'static [&'static str] {
        match self {
            Dialect::Hokkien => &[
                "佇咧", "啥物", "按怎", "無要緊", "毋是", "毋免", "毋知", "袂", "欲", "恁", "阮", "汝", "遮", "遐", "攏",
                "閣", "厝", "明仔載", "下晡", "足感謝", "免客氣", "免煩惱", "按呢", "阿捏", "食飽", "好勢", "敢有", "有影",
                "真濟", "今嘛", "這馬", "哪會", "佮", "今暗", "暗時", "阮兜", "恁兜", "食飽未", "呷飽未", "食飯未", "好料",
                "共你講", "共我講", "共恁講",
                // As speech-to-text writes them in Mandarin letters (「汝吃飽未？」「有空才吼我知」).
                "吃飽未", "吃飯未", "予我知", "互我知", "吼我知",
            ],
            Dialect::Hakka => &[
                "屋下", "仰般", "承蒙", "麼个", "𠊎", "佢兜", "毋使", "恁仔細", "食飯吂", "吂", "這兜", "該兜", "樣般", "做麼",
                "恁靚", "毋會", "今晡日",
            ],
            Dialect::Cantonese => &[
                "邊位", "唔該", "係咪", "陣間", "點樣", "嘅", "咗", "冇", "喺", "佢", "嘢", "啲", "乜嘢", "咩", "唔係", "唔使",
                "唔好", "唔知", "畀", "嚟", "睇", "嗰", "呢度", "哋", "噉", "搵", "揾", "而家",
            ],
            Dialect::Sichuanese => &["啥子", "莫得", "巴适", "咋个", "安逸", "瓜娃子", "噻", "要得嘛", "晓得不"],
            _ => &[],
        }
    }

    /// Words of the dialect everyone mixes into the standard language: never
    /// a reason to switch (「歹勢」「阿嬤」 in Taiwanese Mandarin).
    pub fn loans(self) -> &'static [&'static str] {
        match self {
            Dialect::Hokkien => &["歹勢", "阿嬤"],
            _ => &[],
        }
    }

    /// Ways a caller names it when asking for it.
    fn request_names(self) -> &'static [&'static str] {
        match self {
            Dialect::Hokkien => &[
                "台語", "臺語", "台语", "閩南語", "闽南语", "閩南話", "闽南话", "福建話", "福建话", "河洛話", "hokkien", "taiwanese",
            ],
            Dialect::Hakka => &["客語", "客语", "客家話", "客家话", "hakka"],
            Dialect::Cantonese => &["廣東話", "广东话", "粵語", "粤语", "cantonese"],
            Dialect::Sichuanese => &["四川話", "四川话", "sichuanese"],
            Dialect::Mandarin => &["普通話", "普通话", "國語", "国语", "mandarin"],
            _ => &[],
        }
    }
}

/// A question ending on 「無」 (「你佇咧無？」「今暗愛來食飯無？」「好無？」): Hokkien
/// (and Hakka) asks so, Mandarin does not (「有無」「毫無」 are left out).
fn bo_question(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    chars.iter().enumerate().any(|(i, &c)| {
        c == '無'
            && i > 0
            && ('\u{4E00}'..='\u{9FFF}').contains(&chars[i - 1])
            && !"有毫虛虚並并絕绝別别從从全空若一".contains(chars[i - 1])
            && chars.get(i + 1).map_or(true, |n| "？?，,。！!～~ 啦喔咧".contains(*n))
    })
}

/// Why the call switched to a dialect (`dialect_switch reason=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// A whole sentence in it: two of its own words and five syllables or more.
    Sentence,
    /// Its own words in two turns of the caller in a row.
    TwoTurns,
    /// The caller asked for it.
    Explicit,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Sentence => "sentence",
            Reason::TwoTurns => "two_turns",
            Reason::Explicit => "explicit",
        }
    }
}

/// A switch the gate found: `from` None = the call's own language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switch {
    pub from: Option<Dialect>,
    pub to: Dialect,
    pub reason: Reason,
}

/// Fewest syllables (Han characters) of a sentence that counts as one in the dialect.
const SENTENCE_MIN_SYLLABLES: usize = 5;

/// The dialect a region's lines speak as their own language (Cantonese in Hong
/// Kong and Macau): taken up without a switch.
pub fn base(region: Region) -> Option<Dialect> {
    matches!(region, Region::HkMo).then_some(Dialect::Cantonese)
}

/// Distinct own words of `d` in `text`, loans left out (a word inside a longer
/// one found already is not counted again). Speech-to-text writes Chinese in
/// either script (a Singapore caller's 「来阮兜…好无？」): read in Traditional.
fn marker_hits(d: Dialect, text: &str) -> usize {
    let mut t = crate::zh_tw::norm(text);
    for l in d.loans() {
        t = t.replace(&crate::zh_tw::norm(l), "");
    }
    // The lists hold some simplified or variant forms (麼个, 巴适): compared
    // the way the caller's words are, after the same conversion.
    let mut seen: Vec<String> = Vec::new();
    for m in d.markers() {
        let m = crate::zh_tw::norm(m);
        if t.contains(&m) && !seen.iter().any(|s| s.contains(&m) || m.contains(s.as_str())) {
            seen.push(m);
        }
    }
    // Hakka asks with 「無」 too (「你屋下今晡日有人無？」).
    seen.len() + usize::from(matches!(d, Dialect::Hokkien | Dialect::Hakka) && bo_question(&t))
}

fn syllables(text: &str) -> usize {
    text.chars()
        .filter(|c| ('\u{4E00}'..='\u{9FFF}').contains(c) || ('\u{20000}'..='\u{2FFFF}').contains(c))
        .count()
}

/// The caller asks for `d` by name ("你會曉講台語無？", "講廣東話得唔得？",
/// "Can you speak Cantonese?"). Naming it in a statement ("我阿嬤講台語") is not asking.
fn asks_for(d: Dialect, text: &str) -> bool {
    const VERBS: &[&str] = &["講", "說", "讲", "说", "用", "會曉", "識", "识", "speak ", "in "];
    const ASKS: &[&str] = &[
        "嗎", "吗", "無", "冇", "得唔得", "好不好", "可以", "可唔可以", "能不能", "會不會", "会不会", "識唔識", "请", "請", "?", "？",
        "can you", "could you", "please",
    ];
    let t = text.to_lowercase();
    let named = d
        .request_names()
        .iter()
        .any(|n| VERBS.iter().any(|v| t.contains(&format!("{v}{n}"))));
    named && ASKS.iter().any(|a| t.contains(a))
}

/// One call's dialect state: the candidates of its region, the one it is in,
/// and the caller's last turn (for two turns in a row). Fed whole turns only.
/// Hong Kong and Macau lines speak Cantonese of their own: a Cantonese caller
/// there is noted (for a rotated session) but is no switch; asking for
/// Mandarin is.
#[derive(Debug, Clone, Default)]
pub struct Gate {
    pool: Vec<Dialect>,
    active: Option<Dialect>,
    /// Dialects with at least one own word in the caller's previous turn.
    last_hint: Vec<Dialect>,
    /// The line's own spoken language when it is one of the dialects (Cantonese
    /// in Hong Kong and Macau): taken up without a switch.
    base: Option<Dialect>,
}

impl Gate {
    pub fn new(region: Option<Region>) -> Self {
        let mut pool: Vec<Dialect> = region
            .map(|r| crate::country_profile::dialects(r).iter().copied().filter(|d| d.switches()).collect())
            .unwrap_or_default();
        let base = region.and_then(base);
        if base.is_some() {
            pool.push(Dialect::Mandarin);
        }
        Self { pool, active: None, last_hint: Vec::new(), base }
    }

    pub fn active(&self) -> Option<Dialect> {
        self.active
    }

    fn switch_to(&mut self, to: Dialect, reason: Reason) -> Option<Switch> {
        if self.active == Some(to) {
            return None;
        }
        if self.active.is_none() && self.base == Some(to) {
            // The line's own language: kept for a rotated session, no switch.
            self.active = Some(to);
            return None;
        }
        let from = self.active.replace(to);
        Some(Switch { from, to, reason })
    }

    /// The caller's finished turn (speech-to-text). Some = the call is now in that dialect.
    pub fn caller_turn(&mut self, text: &str) -> Option<Switch> {
        let text = text.trim();
        if self.pool.is_empty() || text.is_empty() {
            return None;
        }
        let mut found: Option<(Dialect, Reason, usize)> = None;
        let mut hints = Vec::new();
        for &d in &self.pool {
            let hits = marker_hits(d, text);
            if hits > 0 {
                hints.push(d);
            }
            let reason = if asks_for(d, text) {
                Some(Reason::Explicit)
            } else if hits >= 2 && syllables(text) >= SENTENCE_MIN_SYLLABLES {
                Some(Reason::Sentence)
            } else if hits >= 1 && self.last_hint.contains(&d) {
                Some(Reason::TwoTurns)
            } else {
                None
            };
            if let Some(r) = reason {
                // An explicit request wins over another dialect's words; otherwise
                // the dialect with more of its own words (a Hakka sentence with
                // 「屋下」 and a 「無」 question is no Hokkien one); a tie keeps the
                // region's order.
                let better = match found {
                    None => true,
                    Some((_, fr, fh)) => {
                        (r == Reason::Explicit && fr != Reason::Explicit)
                            || (fr != Reason::Explicit && r != Reason::Explicit && hits > fh)
                    }
                };
                if better {
                    found = Some((d, r, hits));
                }
            }
        }
        self.last_hint = hints;
        found.and_then(|(d, r, _)| self.switch_to(d, r))
    }

    /// The AI's finished turn (its own speech-to-text). The model hears the
    /// caller's voice, which speech-to-text often writes as Mandarin: a whole
    /// reply of the AI in a dialect of the pool means it heard the caller's
    /// sentence in it and applied the prompt's gate, so it counts as that sentence.
    pub fn ai_turn(&mut self, text: &str) -> Option<Switch> {
        let text = text.trim();
        if self.pool.is_empty() || text.is_empty() {
            return None;
        }
        let d = self
            .pool
            .iter()
            .copied()
            .find(|&d| marker_hits(d, text) >= 2 && syllables(text) >= SENTENCE_MIN_SYLLABLES)?;
        self.switch_to(d, Reason::Sentence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tw() -> Gate {
        Gate::new(Some(Region::Tw))
    }

    #[test]
    fn a_loan_word_in_mandarin_does_not_switch() {
        let mut g = tw();
        assert_eq!(g.caller_turn("不好意思歹勢啦，我阿嬤叫我打來問一下包裹的事"), None);
        assert_eq!(g.caller_turn("對啊，好啦，那就麻煩你了，歹勢歹勢"), None);
        assert_eq!(g.active(), None);
        // Taiwanese Mandarin with English words is no dialect either.
        let mut g = tw();
        assert_eq!(g.caller_turn("那個 meeting 改到下週二下午兩點，你 OK 嗎？"), None);
        assert_eq!(g.caller_turn("好，那我再 email 給你 agenda"), None);
        assert_eq!(g.active(), None);
    }

    #[test]
    fn a_whole_sentence_switches() {
        let mut g = tw();
        let s = g.caller_turn("你好，我是貨運的，明仔載下晡你敢有佇咧厝？").unwrap();
        assert_eq!((s.from, s.to, s.reason), (None, Dialect::Hokkien, Reason::Sentence));
        assert_eq!(g.active(), Some(Dialect::Hokkien));
        // Already in it: no second switch.
        assert_eq!(g.caller_turn("按呢好勢，無要緊啦"), None);
        // Cantonese where it is not the line's own language (mainland China SIM).
        let mut g = Gate::new(Some(Region::Cn));
        let s = g.caller_turn("喂，唔該你，係咪王生呀？我喺速遞公司打嚟嘅").unwrap();
        assert_eq!((s.to, s.reason), (Dialect::Cantonese, Reason::Sentence));
        let mut g = tw();
        assert_eq!(g.caller_turn("你仰般屋下有人在家嗎").map(|s| s.to), Some(Dialect::Hakka));
    }

    #[test]
    fn two_short_turns_in_a_row_switch() {
        let mut g = tw();
        assert_eq!(g.caller_turn("喂，按怎？"), None);
        let s = g.caller_turn("好，免客氣").unwrap();
        assert_eq!((s.to, s.reason), (Dialect::Hokkien, Reason::TwoTurns));
        // A turn in between without it breaks the run.
        let mut g = tw();
        assert_eq!(g.caller_turn("喂，按怎？"), None);
        assert_eq!(g.caller_turn("我是要問明天的包裹"), None);
        assert_eq!(g.caller_turn("好，免客氣"), None);
    }

    #[test]
    fn asking_for_it_switches() {
        let mut g = tw();
        let s = g.caller_turn("你會曉講台語無？").unwrap();
        assert_eq!((s.to, s.reason), (Dialect::Hokkien, Reason::Explicit));
        let mut g = Gate::new(Some(Region::Cn));
        assert_eq!(g.caller_turn("Can you speak Cantonese please?").map(|s| s.reason), Some(Reason::Explicit));
        let mut g = Gate::new(Some(Region::MySg));
        assert_eq!(g.caller_turn("講廣東話得唔得？").map(|s| s.to), Some(Dialect::Cantonese));
        // Saying someone speaks it is not asking.
        let mut g = tw();
        assert_eq!(g.caller_turn("我阿嬤講台語比較習慣"), None);
    }

    #[test]
    fn only_the_regions_dialects_count() {
        // Cantonese words on a Taiwan SIM: not a candidate here.
        let mut g = tw();
        assert_eq!(g.caller_turn("唔該你，係咪王生呀？我喺速遞公司打嚟嘅"), None);
        // A region without a dialect to switch to.
        let mut g = Gate::new(Some(Region::Us));
        assert_eq!(g.caller_turn("Hey, it's the courier, where should I leave it?"), None);
        let mut g = Gate::new(None);
        assert_eq!(g.caller_turn("你好，我是貨運的，明仔載下晡你敢有佇咧厝？"), None);
    }

    #[test]
    fn the_ais_reply_in_the_dialect_stands_for_the_callers_sentence() {
        let mut g = tw();
        // Speech-to-text wrote the caller's Taiwanese as Mandarin…
        assert_eq!(g.caller_turn("我是貨運公司的，想請問你明天下午在家嗎"), None);
        // …the AI answered in it.
        let s = g.ai_turn("好，免客氣，足感謝喔，你放管理室就好勢").unwrap();
        assert_eq!((s.to, s.reason), (Dialect::Hokkien, Reason::Sentence));
        // A Mandarin reply with one Taiwanese word does not.
        let mut g = tw();
        assert_eq!(g.ai_turn("好啊，歹勢喔，你先放管理室就好"), None);
    }

    /// 2026-11-17 bench: a Hong Kong courier speaking Cantonese on a Hong Kong
    /// SIM was logged as a switch — the line's own language, not a dialect.
    #[test]
    fn cantonese_on_a_hong_kong_line_is_no_switch_but_mandarin_asked_for_is() {
        let mut g = Gate::new(Some(Region::HkMo));
        assert_eq!(g.caller_turn("OK,沒問題。咁我放喺管理室啦。唔該曬王先生。"), None);
        assert_eq!(g.active(), Some(Dialect::Cantonese), "kept for a rotated session");
        assert_eq!(g.ai_turn("唔該晒，我而家喺屋企，你放喺管理處得喇"), None);
        assert_eq!(g.caller_turn("講廣東話得唔得？"), None);
        let s = g.caller_turn("唔好意思，可以講普通話嗎？").unwrap();
        assert_eq!((s.from, s.to, s.reason), (Some(Dialect::Cantonese), Dialect::Mandarin, Reason::Explicit));
        assert_eq!(s.to.code(), "cmn");
        // Mandarin from the start, asked in English.
        let mut g = Gate::new(Some(Region::HkMo));
        let s = g.caller_turn("Hello, can you speak Mandarin?").unwrap();
        assert_eq!((s.from, s.to), (None, Dialect::Mandarin));
        // A Mandarin sentence alone (speech-to-text writes Cantonese so too) switches nothing.
        let mut g = Gate::new(Some(Region::HkMo));
        assert_eq!(g.caller_turn("喂，你好，請問是不是王先生啊？我是順豐派件的。"), None);
        assert_eq!(g.active(), None);
        // Mandarin is no candidate elsewhere.
        let mut g = tw();
        assert_eq!(g.caller_turn("你可以講普通話嗎？"), None);
    }

    #[test]
    fn a_hokkien_question_ending_on_bo_counts() {
        // Singapore: 「今暗愛來捧場無？」 is two of its own words.
        let mut g = Gate::new(Some(Region::MySg));
        let s = g.caller_turn("我某煮幾道好料理。今暗你愛來捧場無？").unwrap();
        assert_eq!((s.to, s.reason), (Dialect::Hokkien, Reason::Sentence));
        assert!(bo_question("你佇咧無？") && bo_question("好無") && bo_question("有人無，我等一下再來"));
        // Mandarin 無 is no question.
        for t in ["有無問題都可以", "這樣毫無意義", "無論如何你先來", "這是有無的問題", "一無所有"] {
            assert!(!bo_question(t), "{t}");
        }
        // Loan words and Taiwanese Mandarin still do not switch.
        let mut g = tw();
        assert_eq!(g.caller_turn("歹勢啦，我是你阿嬤隔壁的，想問她今天下午有沒有空？"), None);
        assert_eq!(g.caller_turn("好啦好啦，那我們就訂那間義大利餐廳，你覺得可以嗎？"), None);
        assert_eq!(g.caller_turn("夭壽喔，今天塞車塞了一個小時，你有沒有遇到？"), None);
        assert_eq!(g.active(), None);
    }

    /// 2026-11-17 bench d_sg_nan_dinner: the Singapore caller's 「你甲飯未？今晚來阮兜
    /// 食飯好無？」 came in from speech-to-text in Simplified, its 「無」 as 「无」 —
    /// one own word left, no switch.
    #[test]
    fn simplified_speech_to_text_counts_too() {
        let mut g = Gate::new(Some(Region::MySg));
        let s = g.caller_turn("喂,王先生啊。你家饭围。今晚来阮都是饭好无?").unwrap();
        assert_eq!((s.to, s.reason), (Dialect::Hokkien, Reason::Sentence));
        let mut g = Gate::new(Some(Region::Cn));
        let s = g.caller_turn("喂，唔该你，系咪王生呀？我喺速递公司打嚟嘅").unwrap();
        assert_eq!(s.to, Dialect::Cantonese);
        // Simplified Mandarin is still no dialect.
        let mut g = Gate::new(Some(Region::MySg));
        assert_eq!(g.caller_turn("喂，王先生，今晚来我家吃饭好吗？"), None);
        assert_eq!(g.caller_turn("有无问题都可以，你check好schedule再跟我讲"), None);
        assert_eq!(g.active(), None);
        // The AI's reply in Hokkien stands for the caller's sentence.
        let s = Gate::new(Some(Region::MySg)).ai_turn("好啊，等咧再共你讲，阮兜今暗有闲").unwrap();
        assert_eq!(s.to, Dialect::Hokkien);
    }
}

#[cfg(test)]
mod norm_markers {
    use super::{Dialect, Gate};
    use crate::country_profile::Region;

    #[test]
    fn simplified_or_variant_markers_still_count() {
        let s = Gate::new(Some(Region::Cn)).caller_turn("这个巴适得很，咋个办嘛，安逸");
        assert_eq!(s.map(|s| s.to), Some(Dialect::Sichuanese));
    }
}

/// 2026-11-17 acceptance run (9832627): d_tw_hak_parcel and d_sg_nan_dinner.
#[cfg(test)]
mod acceptance_dialects {
    use super::{Dialect, Gate, Reason};
    use crate::country_profile::Region;

    /// A Hakka courier asks with 「無」 as Hokkien does: its own words decide.
    #[test]
    fn a_hakka_question_with_bo_is_hakka() {
        let mut g = Gate::new(Some(Region::Tw));
        let s = g.caller_turn("喂，王先生，我是送貨个，請問你屋下今晡日有人收包裹無？");
        assert_eq!(s.map(|s| (s.to, s.reason)), Some((Dialect::Hakka, Reason::Sentence)));
        // A Hokkien sentence with a 「無」 question stays Hokkien.
        let mut g = Gate::new(Some(Region::Tw));
        let s = g.caller_turn("你明仔載下晡佇咧無？");
        assert_eq!(s.map(|s| s.to), Some(Dialect::Hokkien));
        // A bare 「無」 question in two turns: the region's first dialect, as before.
        let mut g = Gate::new(Some(Region::Tw));
        assert_eq!(g.caller_turn("你好無？"), None);
        assert_eq!(g.caller_turn("今仔日有閒無？").map(|s| s.to), Some(Dialect::Hokkien));
    }

    /// Speech-to-text writes Singapore Hokkien in Mandarin letters: 吃飽未, 吼我知.
    #[test]
    fn hokkien_in_mandarin_letters() {
        let mut g = Gate::new(Some(Region::MySg));
        assert_eq!(g.caller_turn("Hello, 王先生 ah! 你吃飽未？Today come my house dine together, okay?"), None);
        let s = g.caller_turn("好啦，你先 check，有空才吼我知。");
        assert_eq!(s.map(|s| (s.to, s.reason)), Some((Dialect::Hokkien, Reason::TwoTurns)));
        // Mandarin asking whether you have eaten is no Hokkien.
        let mut g = Gate::new(Some(Region::MySg));
        assert_eq!(g.caller_turn("你吃飽了沒？要不要一起吃飯"), None);
        assert_eq!(g.caller_turn("那你吃過飯了嗎"), None);
    }
}

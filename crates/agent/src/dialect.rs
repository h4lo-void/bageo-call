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
        }
    }

    /// A spoken variety of its own: the AI switches to it (in whole sentences)
    /// once the gate holds. The others are ways of speaking the call's language:
    /// understood and followed in wording, never put on.
    pub fn switches(self) -> bool {
        matches!(self, Dialect::Hokkien | Dialect::Hakka | Dialect::Cantonese | Dialect::Sichuanese)
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
        }
    }

    /// Words of the dialect alone (its grammar and core words), as speech-to-text
    /// writes them. One of them is a hint; two in a sentence is the sentence.
    fn markers(self) -> &'static [&'static str] {
        match self {
            Dialect::Hokkien => &[
                "佇咧", "啥物", "按怎", "無要緊", "毋是", "毋免", "毋知", "袂", "欲", "恁", "阮", "汝", "遮", "遐", "攏",
                "閣", "厝", "明仔載", "下晡", "足感謝", "免客氣", "免煩惱", "按呢", "阿捏", "食飽", "好勢", "敢有", "有影",
                "真濟", "今嘛", "這馬", "哪會", "佮",
            ],
            Dialect::Hakka => &[
                "屋下", "仰般", "承蒙", "麼个", "𠊎", "佢兜", "毋使", "恁仔細", "食飯吂", "吂", "這兜", "該兜", "樣般", "做麼",
                "恁靚", "毋會",
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
            _ => &[],
        }
    }
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

/// Distinct own words of `d` in `text`, loans left out (a word inside a longer
/// one found already is not counted again).
fn marker_hits(d: Dialect, text: &str) -> usize {
    let mut t = text.to_string();
    for l in d.loans() {
        t = t.replace(l, "");
    }
    let mut seen: Vec<&str> = Vec::new();
    for m in d.markers() {
        if t.contains(m) && !seen.iter().any(|s| s.contains(m) || m.contains(s)) {
            seen.push(m);
        }
    }
    seen.len()
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
#[derive(Debug, Clone, Default)]
pub struct Gate {
    pool: Vec<Dialect>,
    active: Option<Dialect>,
    /// Dialects with at least one own word in the caller's previous turn.
    last_hint: Vec<Dialect>,
}

impl Gate {
    pub fn new(region: Option<Region>) -> Self {
        let pool = region
            .map(|r| crate::country_profile::dialects(r).iter().copied().filter(|d| d.switches()).collect())
            .unwrap_or_default();
        Self { pool, active: None, last_hint: Vec::new() }
    }

    pub fn active(&self) -> Option<Dialect> {
        self.active
    }

    fn switch_to(&mut self, to: Dialect, reason: Reason) -> Option<Switch> {
        if self.active == Some(to) {
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
        let mut found: Option<(Dialect, Reason)> = None;
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
                // An explicit request wins over another dialect's words.
                if found.map_or(true, |(_, fr)| r == Reason::Explicit && fr != Reason::Explicit) {
                    found = Some((d, r));
                }
            }
        }
        self.last_hint = hints;
        found.and_then(|(d, r)| self.switch_to(d, r))
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
        let mut g = Gate::new(Some(Region::HkMo));
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
        let mut g = Gate::new(Some(Region::HkMo));
        assert_eq!(g.caller_turn("Can you speak Cantonese please?").map(|s| s.reason), Some(Reason::Explicit));
        let mut g = Gate::new(Some(Region::HkMo));
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
}

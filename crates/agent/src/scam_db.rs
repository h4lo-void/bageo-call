// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The scam and sales database: what sales and scam calls sound like, in many
//! languages, kept as data (`data/scam_db.json`, built into the agent)
//! instead of word lists in the code.
//!
//! - **Typologies**: the kinds of call (a fake prosecutor wanting a "safe
//!   account", a fake bank cancelling instalments at an ATM, insurance sales …),
//!   each with its name in the notification languages, what the caller asks of
//!   the owner, whether that request is a scam whoever asks (`hard`) and the
//!   ordinary call it is easily mistaken for. The second look
//!   ([`crate::stall_review`]) reads them; the call record and the summary name the kind.
//! - **Keywords** per language, weighted and tied to a typology: the cheap
//!   first check while the caller talks ([`keyword_hits`]).
//! - **Own-matter phrases**: a caller following up on something the owner
//!   raised is never a pitch ([`own_matter`]).
//! - **Number rules**: how the caller's number looks, as hints for the second
//!   look, never proof ([`number_rules`]).
//!
//! The owner adds keywords and number marks in
//! `{CALLS_DATA_DIR}/config/scam_db.local.json` (read again when it changes; a missing
//! or broken file changes nothing). Marks given on the console and the calls
//! judged during a call are kept per number in `config/scam_numbers.json`
//! ([`NumberBook`]); every console mark also goes to `config/scam_feedback.jsonl`
//! for tuning the database later.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

// The words are in the prompt book (data/prompts/<language>.json); these are their handles.
crate::prompt_book::prompt_texts! {
    /// The stall hooks of a country's scam in Traditional Chinese, Simplified Chinese and English
    /// ([country_bait_addon]): `{country}`, `{scam}`, `{hooks}`.
    ZH_COUNTRY_BAIT_ADDON = "zh-TW/scam_db_country_bait_addon";
    ZH_COUNTRY_BAIT_ADDON_CN = "zh-TW/scam_db_country_bait_addon_cn";
    EN_COUNTRY_BAIT_ADDON = "en/scam_db_country_bait_addon";
}

const BUILTIN_GZ: &[u8] = include_bytes!("../data/packs/bageo-scams.json.gz");

pub const SCAMS_FILE_NAME: &str = bageo_packs::SCAMS_FILE;
pub const MAX_INSTALLS: usize = 8;
const SCAMS_INCOMING_PREFIX: &str = ".incoming-scams-";
const SCAMS_INCOMING_SUFFIX: &str = ".json.gz";

/// Every typology is named in these (the notification languages, [`crate::locale::ALL`]).
pub const NAME_LANGS: [&str; 9] = ["zh-TW", "zh-CN", "en", "ja", "ko", "es", "fr", "de", "pt"];

/// Times a number must have been judged sales / scam during calls before it is
/// looked at sooner: once may have been a mistake.
pub const AUTO_FLAG_MIN: u32 = 2;

/// The local file is looked at again at most this often (its keywords are read
/// on every piece of the caller's speech).
const LOCAL_RECHECK: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Scam,
    Sales,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Scam => "scam",
            Category::Sales => "sales",
        }
    }
}

/// One kind of scam or sales call.
#[derive(Debug, Clone)]
pub struct Typology {
    pub id: String,
    pub category: Category,
    /// What it asks of the owner is a scam whoever asks (codes, ATM steps,
    /// gift cards, remote access, a "safe account", secrecy).
    pub hard: bool,
    /// Language code → name, in every one of [`NAME_LANGS`].
    pub names: BTreeMap<String, String>,
    /// What the caller says and wants (English, for the reviewing model).
    pub signals: String,
    /// The ordinary call it is easily mistaken for (English, for the reviewing model).
    pub legit: String,
}

impl Typology {
    /// Its name in `lang` (English, then the id, if missing).
    pub fn name(&self, lang: crate::locale::Lang) -> &str {
        self.names
            .get(lang.code())
            .or_else(|| self.names.get("en"))
            .map(String::as_str)
            .unwrap_or(&self.id)
    }
}

/// How a language's keywords are found in what the caller said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Chinese: the text is put into Traditional first (speech-to-text writes
    /// Simplified too); ASCII words (`atm`, `line`) only as whole words, so
    /// "streamline" is not "add me on LINE".
    Han,
    /// Words separated by spaces: a keyword matches at the start of a word
    /// ("loan" in "loans", "refinanc" in "refinancing"), not inside one ("bank"
    /// is not in "embankment").
    Word,
    /// Scripts without spaces between words (Japanese, Korean) and scripts whose
    /// words take prefixes (Arabic): anywhere in the text.
    Run,
    /// Thai: anywhere, spaces ignored (speech-to-text puts spaces between Thai words).
    Compact,
}

impl Match {
    fn parse(s: &str) -> Option<Match> {
        match s {
            "han" => Some(Match::Han),
            "word" => Some(Match::Word),
            "run" => Some(Match::Run),
            "compact" => Some(Match::Compact),
            _ => None,
        }
    }
}

/// A weighted keyword: weight 2 is a clear pitch or scam on its own, 1 needs company.
#[derive(Debug, Clone)]
pub struct Keyword {
    /// The text as it is matched and reported (lowercase; a Simplified Chinese
    /// keyword in the form the caller's normalised words take).
    pub text: String,
    pub weight: u32,
    pub typology: String,
    pub hard: bool,
    pub lang: String,
    pub mode: Match,
    /// What is searched for: " text" for word languages.
    needle: String,
    /// Korean: matched without spaces on both sides ("안전계좌로 이체" is also
    /// heard as "안전계좌로이체").
    squeeze: bool,
    /// A short keyword in a language with word endings: the heard word must end
    /// there, or after an ordinary ending.
    word_end: bool,
    /// Spelled like an earlier keyword of the same language once accents are
    /// off (prestamo after préstamo): it is never matched itself, so one heard
    /// word is not scored twice.
    shadow: bool,
}

/// A keyword heard on the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub text: &'static str,
    pub weight: u32,
    pub typology: &'static str,
    pub hard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    Hidden,
    Foreign,
    Prefix,
}

/// A hint about how the caller's number looks.
#[derive(Debug, Clone)]
pub struct NumberRule {
    pub id: String,
    pub kind: RuleKind,
    /// Only on calls to a SIM of these countries (empty: any).
    pub sim_countries: Vec<String>,
    /// The caller ID as shown starts with one of these (`+886` on a Taiwanese SIM).
    pub raw_prefixes: Vec<String>,
    /// The number in international form starts with one of these.
    pub e164_prefixes: Vec<String>,
    /// What it means (English, for the reviewing model).
    pub note: String,
}

/// One country's phone-scam profile from `data/country_scams.json`.
#[derive(Debug, Clone)]
pub struct CountryProfile {
    pub code: String,
    pub name: String,
    pub name_local: String,
    pub iso: String,
    pub calling_code: String,
    pub languages: Vec<String>,
    pub prevalence_note: String,
    pub cross_border_note: String,
    pub scams: Vec<CountryScam>,
}

/// One region-specific scam pattern inside a [`CountryProfile`].
#[derive(Debug, Clone)]
pub struct CountryScam {
    pub id: String,
    pub typology: String,
    pub name: String,
    pub name_en: String,
    pub prevalence: String,
    pub lang: String,
    pub context_keywords: Vec<String>,
    pub red_flag_keywords: Vec<CountryRedFlag>,
    pub signals_en: String,
    pub legit_en: String,
    pub bait_tactics: Vec<String>,
    pub sources: Vec<String>,
}

/// A strong region-specific red-flag phrase (`weight` 1 or 2).
#[derive(Debug, Clone, Deserialize)]
pub struct CountryRedFlag {
    pub text: String,
    #[serde(default = "default_red_flag_weight")]
    pub weight: u32,
    #[serde(default)]
    pub hard: bool,
}

fn default_red_flag_weight() -> u32 {
    2
}

#[derive(Debug, Clone, Default)]
pub struct KeywordIndex {
    han_first: HashMap<char, Vec<usize>>,
    word_prefix: HashMap<(char, char), Vec<usize>>,
    run_first: HashMap<char, Vec<usize>>,
    compact_first: HashMap<char, Vec<usize>>,
}

#[derive(Debug, Clone, Default)]
pub struct ScamDb {
    pub version: u32,
    pub typologies: Vec<Typology>,
    pub keywords: Vec<Keyword>,
    /// Language → phrases of a caller following up on the owner's own matter.
    pub own_matter: Vec<(String, Vec<String>)>,
    pub number_rules: Vec<NumberRule>,
    /// Lowercase ISO country (`tw`, `us`, `gb`, `jp`, …) → regional scam profile.
    pub countries: BTreeMap<String, CountryProfile>,
    by_text: HashMap<(String, String), usize>,
    by_text_any: HashMap<String, usize>,
    by_typology: HashMap<String, usize>,
    index: KeywordIndex,
    /// Normalized keyword text → `(country_iso, scam_id)` pairs.
    country_kw_map: HashMap<String, Vec<(String, String)>>,
}

#[derive(Deserialize)]
struct RawDb {
    version: u32,
    typologies: Vec<RawTypology>,
    keywords: Vec<RawGroup>,
    own_matter: Vec<RawOwn>,
    number_rules: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawTypology {
    id: String,
    category: String,
    hard: bool,
    names: BTreeMap<String, String>,
    signals: String,
    legit: String,
}

#[derive(Deserialize)]
struct RawGroup {
    lang: String,
    #[serde(rename = "match")]
    mode: String,
    words: Vec<Value>,
}

#[derive(Deserialize)]
struct RawOwn {
    lang: String,
    phrases: Vec<String>,
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    kind: String,
    #[serde(default)]
    sim_countries: Vec<String>,
    #[serde(default)]
    raw_prefixes: Vec<String>,
    #[serde(default)]
    e164_prefixes: Vec<String>,
    note: String,
}

/// Chinese the way the keywords read it: Traditional, ASCII lowercased, and
/// the variants live speech-to-text writes (賬戶 for 帳戶, 加頼 for 加賴).
pub fn han_text(text: &str) -> String {
    crate::zh_tw::norm(text)
        .to_ascii_lowercase()
        .replace('賬', "帳")
        .replace("加頼", "加賴")
        .replace("賣貨變", "賣貨便")
        .replace("好賣加", "好賣+")
}

/// An ASCII keyword inside Chinese text only as a whole word: "streamline",
/// "online" or "deadline" are not 加 LINE.
pub fn han_contains(text: &str, kw: &str) -> bool {
    if !kw.is_ascii() {
        return text.contains(kw);
    }
    let word = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    text.match_indices(kw)
        .any(|(i, _)| !word(text[..i].chars().next_back()) && !word(text[i + kw.len()..].chars().next()))
}

/// Lowercase words of `text`, punctuation dropped (apostrophes kept inside words).
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('’', "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .map(|w| w.trim_matches('\'').to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

fn compact(text: &str) -> String {
    text.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect()
}

/// Lowercase with Latin accents taken off, whether the letter is one character
/// (é) or a letter plus a combining mark (e + U+0301), and Vietnamese tone marks
/// and đ folded to plain letters: "préstamo" and "prestamo", "prêt" and "pret",
/// "đầu tư" and "dau tu" read the same. Other scripts are left as they are.
fn fold_accents(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for raw in text.chars() {
        let raw = match raw {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(raw as u32 - 0xFEE0).unwrap_or(raw),
            '\u{3000}' => ' ',
            _ => raw,
        };
        for c in raw.to_lowercase() {
            let base = match c {
                // A combining mark only after a Latin letter (Cyrillic й is и + a mark).
                '\u{0300}'..='\u{036F}' if out.chars().next_back().is_some_and(|p| p.is_ascii_alphabetic()) => continue,
                'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ạ' | 'ả' | 'ấ' | 'ầ' | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => 'a',
                'ç' | 'ć' | 'č' => 'c',
                'đ' | 'ď' => 'd',
                'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' | 'ẹ' | 'ẻ' | 'ẽ' | 'ế' | 'ề' | 'ể' | 'ễ' | 'ệ' => 'e',
                'ğ' => 'g',
                'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĩ' | 'ỉ' | 'ị' => 'i',
                'ł' => 'l',
                'ñ' | 'ń' | 'ň' => 'n',
                'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ơ' | 'ọ' | 'ỏ' | 'ố' | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' => 'o',
                'ř' => 'r',
                'ş' | 'ś' | 'š' | 'ș' => 's',
                'ť' | 'ț' | 'ţ' => 't',
                'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ũ' | 'ů' | 'ư' | 'ụ' | 'ủ' | 'ứ' | 'ừ' | 'ử' | 'ữ' | 'ự' => 'u',
                'ý' | 'ÿ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => 'y',
                'ź' | 'ż' | 'ž' => 'z',
                'ß' => {
                    out.push_str("ss");
                    continue;
                }
                'œ' => {
                    out.push_str("oe");
                    continue;
                }
                'æ' => {
                    out.push_str("ae");
                    continue;
                }
                _ => c,
            };
            out.push(base);
        }
    }
    out
}

/// Hiragana, katakana and hangul letters (what a Japanese or Korean sentence has
/// and a Chinese one does not), and ideographs.
fn script_counts(text: &str) -> (usize, usize) {
    let (mut own, mut han) = (0, 0);
    for c in text.chars() {
        match c as u32 {
            0x3041..=0x309F | 0x30A1..=0x30FA | 0x30FD..=0x30FF | 0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => own += 1,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF => han += 1,
            _ => {}
        }
    }
    (own, han)
}

/// Endings an English, Spanish, French, Portuguese or Italian word may take
/// after a short keyword ("loan" → loans, loaned, loaning; "conta" → contas).
/// Anything longer is another word ("pix" is not "pixel", "demo" is not
/// "demonstrate", "conta" is not "contacts").
const SHORT_WORD_ENDINGS: &[&str] = &["", "s", "es", "e", "x", "d", "ed", "ing", "er", "ers", "'s", "ly", "n", "en", "i", "he"];

/// Languages whose short keywords must end where the heard word ends. German
/// (compounds), Turkish, Indonesian, Malay, Tagalog (affixes), Russian (cases)
/// and Vietnamese keep matching word starts.
fn needs_word_end(lang: &str) -> bool {
    matches!(lang, "en" | "es" | "fr" | "pt" | "it")
}

/// The caller's words in every form the match modes read.
struct Prepared {
    han: String,
    words_list: Vec<String>,
    padded: String,
    lower: String,
    compact: String,
    /// The text is Japanese or Korean (letters of those scripts outweigh the
    /// ideographs): the Chinese keywords are not read in it.
    skip_han: bool,
}

impl Prepared {
    fn of(text: &str) -> Prepared {
        let words_list = words(&fold_accents(text));
        let padded = format!(" {} ", words_list.join(" "));
        let (own, han) = script_counts(text);
        Prepared {
            han: han_text(text),
            words_list,
            padded,
            lower: text.to_lowercase(),
            compact: compact(text),
            skip_han: own >= 3 && own * 4 >= own + han,
        }
    }
}

impl KeywordIndex {
    fn insert(&mut self, idx: usize, kw: &Keyword) {
        match kw.mode {
            Match::Han => {
                if let Some(c0) = kw.needle.chars().next() {
                    self.han_first.entry(c0).or_default().push(idx);
                }
            }
            Match::Word => {
                let first_word = kw.needle.trim_start_matches(' ').split(' ').next().unwrap_or("");
                let mut chars = first_word.chars();
                if let Some(c0) = chars.next() {
                    let c1 = chars.next().unwrap_or('\0');
                    self.word_prefix.entry((c0, c1)).or_default().push(idx);
                }
            }
            Match::Run => {
                if let Some(c0) = kw.needle.chars().next() {
                    self.run_first.entry(c0).or_default().push(idx);
                }
            }
            Match::Compact => {
                if let Some(c0) = kw.needle.chars().next() {
                    self.compact_first.entry(c0).or_default().push(idx);
                }
            }
        }
    }

    fn candidates(&self, p: &Prepared) -> Vec<usize> {
        let mut out = Vec::new();
        if !self.han_first.is_empty() {
            for c in p.han.chars().filter(|_| !p.skip_han) {
                if let Some(list) = self.han_first.get(&c) {
                    out.extend_from_slice(list);
                }
            }
        }
        if !self.word_prefix.is_empty() {
            for w in &p.words_list {
                let mut chars = w.chars();
                if let Some(c0) = chars.next() {
                    if let Some(list) = self.word_prefix.get(&(c0, '\0')) {
                        out.extend_from_slice(list);
                    }
                    if let Some(c1) = chars.next() {
                        if let Some(list) = self.word_prefix.get(&(c0, c1)) {
                            out.extend_from_slice(list);
                        }
                    }
                }
            }
        }
        if !self.run_first.is_empty() {
            for c in p.lower.chars() {
                if let Some(list) = self.run_first.get(&c) {
                    out.extend_from_slice(list);
                }
            }
        }
        if !self.compact_first.is_empty() {
            for c in p.compact.chars() {
                if let Some(list) = self.compact_first.get(&c) {
                    out.extend_from_slice(list);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }
}

impl Keyword {
    fn new(text: &str, weight: u32, typology: &str, hard: bool, lang: &str, mode: Match) -> Keyword {
        let text = match mode {
            Match::Han => han_text(text.trim()),
            Match::Word | Match::Run => text.trim().to_lowercase(),
            Match::Compact => compact(text),
        };
        let squeeze = mode == Match::Run && lang.starts_with("ko");
        let needle = match mode {
            Match::Word => format!(" {}", fold_accents(&text)),
            _ if squeeze => compact(&text),
            _ => text.clone(),
        };
        let word_end =
            mode == Match::Word && needs_word_end(lang) && needle.trim_start().rsplit(' ').next().is_some_and(|w| w.chars().count() <= 5);
        Keyword {
            text,
            weight,
            typology: typology.to_string(),
            hard,
            lang: lang.to_string(),
            mode,
            needle,
            squeeze,
            word_end,
            shadow: false,
        }
    }

    fn found_in(&self, p: &Prepared) -> bool {
        if self.shadow {
            return false;
        }
        match self.mode {
            Match::Han => !p.skip_han && han_contains(&p.han, &self.needle),
            Match::Word => {
                if !self.word_end {
                    return p.padded.contains(&self.needle);
                }
                p.padded.match_indices(&self.needle).any(|(i, m)| {
                    let rest = &p.padded[i + m.len()..];
                    SHORT_WORD_ENDINGS.contains(&rest.split(' ').next().unwrap_or(""))
                })
            }
            Match::Run if self.squeeze => p.compact.contains(&self.needle),
            Match::Run => p.lower.contains(&self.needle),
            Match::Compact => p.compact.contains(&self.needle),
        }
    }
}

fn hit_of(kw: &'static Keyword) -> Hit {
    Hit {
        text: &kw.text,
        weight: kw.weight,
        typology: &kw.typology,
        hard: kw.hard,
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// One `[text, weight, typology]` or `[text, weight, typology, "hard"]` entry.
fn keyword_entry(v: &Value) -> Result<(String, u32, String, bool), String> {
    let a = v.as_array().ok_or_else(|| format!("keyword entry is not an array: {v}"))?;
    if a.len() != 3 && a.len() != 4 {
        return Err(format!("keyword entry needs 3 or 4 items: {v}"));
    }
    let text = a[0].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| format!("keyword text: {v}"))?;
    let weight = a[1].as_u64().filter(|w| (1..=2).contains(w)).ok_or_else(|| format!("keyword weight must be 1 or 2: {v}"))?;
    let typology = a[2].as_str().ok_or_else(|| format!("keyword typology: {v}"))?;
    let hard = match a.get(3) {
        None => false,
        Some(Value::String(s)) if s == "hard" => true,
        Some(_) => return Err(format!("the 4th item can only be \"hard\": {v}")),
    };
    Ok((text.to_string(), weight as u32, typology.to_string(), hard))
}

/// Read and check a database: any mistake is an error naming it (a unit test
/// reads the built-in one, so a bad edit fails the build's tests, not a call).
pub fn parse(json: &str) -> Result<ScamDb, String> {
    let raw: RawDb = serde_json::from_str(json).map_err(|e| format!("not a scam database: {e}"))?;
    if raw.version == 0 {
        return Err("version must be 1 or more".into());
    }
    let mut db = ScamDb {
        version: raw.version,
        ..ScamDb::default()
    };
    for t in raw.typologies {
        if !valid_id(&t.id) {
            return Err(format!("typology id {:?}: lowercase letters, digits and _ only", t.id));
        }
        if db.by_typology.contains_key(&t.id) {
            return Err(format!("typology {} listed twice", t.id));
        }
        let category = match t.category.as_str() {
            "scam" => Category::Scam,
            "sales" => Category::Sales,
            other => return Err(format!("typology {}: category {other:?} (scam or sales)", t.id)),
        };
        for lang in NAME_LANGS {
            if t.names.get(lang).map_or(true, |n| n.trim().is_empty()) {
                return Err(format!("typology {} has no {lang} name", t.id));
            }
        }
        if t.signals.trim().is_empty() || t.legit.trim().is_empty() {
            return Err(format!("typology {} needs signals and legit", t.id));
        }
        db.by_typology.insert(t.id.clone(), db.typologies.len());
        db.typologies.push(Typology {
            id: t.id,
            category,
            hard: t.hard,
            names: t.names,
            signals: t.signals,
            legit: t.legit,
        });
    }
    let mut langs = HashSet::new();
    let mut seen_needles: HashSet<(String, String, String, bool)> = HashSet::new();
    for g in raw.keywords {
        if g.lang.trim().is_empty() || !langs.insert(g.lang.clone()) {
            return Err(format!("keyword language {:?} missing or listed twice", g.lang));
        }
        let mode = Match::parse(&g.mode).ok_or_else(|| format!("{}: match {:?} (han, word, run or compact)", g.lang, g.mode))?;
        if g.words.is_empty() {
            return Err(format!("{} has no keywords", g.lang));
        }
        for v in &g.words {
            let (text, weight, typology, hard) = keyword_entry(v).map_err(|e| format!("{}: {e}", g.lang))?;
            let Some(&ti) = db.by_typology.get(&typology) else {
                return Err(format!("{}: {text} names unknown typology {typology}", g.lang));
            };
            if hard && !db.typologies[ti].hard {
                return Err(format!("{}: {text} is hard but {typology} is not", g.lang));
            }
            if (mode == Match::Word || mode == Match::Run) && text != text.to_lowercase() {
                return Err(format!("{}: write {text:?} in lowercase", g.lang));
            }
            let kw = Keyword::new(&text, weight, &typology, hard, &g.lang, mode);
            let mut kw = kw;
            if mode == Match::Word && !seen_needles.insert((kw.lang.clone(), kw.needle.clone(), kw.typology.clone(), kw.hard)) {
                kw.shadow = true;
            }
            if db.by_text.contains_key(&(kw.lang.clone(), kw.text.clone())) {
                // 周轉 next to 週轉: a spelling the Chinese normaliser folds into
                // one already listed matches the same words; it is not kept twice.
                if mode == Match::Han && text.trim() != kw.text {
                    continue;
                }
                return Err(format!("{}: keyword {:?} listed twice", g.lang, kw.text));
            }
            db.insert_keyword(kw);
        }
    }
    for o in raw.own_matter {
        let zh = o.lang.starts_with("zh");
        let phrases: Vec<String> = o
            .phrases
            .iter()
            .map(|p| if zh { crate::zh_tw::norm(p.trim()) } else { p.trim().to_lowercase() })
            .collect();
        if phrases.is_empty() || phrases.iter().any(|p| p.is_empty()) {
            return Err(format!("own_matter {}: empty phrase", o.lang));
        }
        db.own_matter.push((o.lang, phrases));
    }
    let mut rule_ids = HashSet::new();
    for r in raw.number_rules {
        if !valid_id(&r.id) || !rule_ids.insert(r.id.clone()) {
            return Err(format!("number rule id {:?} invalid or listed twice", r.id));
        }
        let kind = match r.kind.as_str() {
            "hidden" => RuleKind::Hidden,
            "foreign" => RuleKind::Foreign,
            "prefix" => RuleKind::Prefix,
            other => return Err(format!("number rule {}: kind {other:?}", r.id)),
        };
        let prefix_ok = |p: &String| p.starts_with('+') || p.starts_with("00");
        if kind == RuleKind::Prefix
            && (r.raw_prefixes.is_empty() && r.e164_prefixes.is_empty()
                || !r.raw_prefixes.iter().chain(&r.e164_prefixes).all(prefix_ok))
        {
            return Err(format!("number rule {}: prefixes must start with + or 00", r.id));
        }
        if r.note.trim().is_empty() {
            return Err(format!("number rule {} has no note", r.id));
        }
        db.number_rules.push(NumberRule {
            id: r.id,
            kind,
            sim_countries: r.sim_countries.iter().map(|c| c.to_ascii_lowercase()).collect(),
            raw_prefixes: r.raw_prefixes,
            e164_prefixes: r.e164_prefixes,
            note: r.note,
        });
    }
    Ok(db)
}

impl ScamDb {
    pub fn typology(&self, id: &str) -> Option<&Typology> {
        self.by_typology.get(id).map(|&i| &self.typologies[i])
    }

    pub fn keyword(&self, text: &str) -> Option<&Keyword> {
        self.by_text_any.get(text).map(|&i| &self.keywords[i])
    }

    pub fn keyword_in(&self, lang: &str, text: &str) -> Option<&Keyword> {
        self.by_text
            .get(&(lang.to_string(), text.to_string()))
            .map(|&i| &self.keywords[i])
    }

    fn sync_any(&mut self, idx: usize) {
        let kw = &self.keywords[idx];
        self.by_text_any
            .entry(kw.text.clone())
            .and_modify(|existing| {
                let prev = &self.keywords[*existing];
                if kw.weight > prev.weight || (kw.weight == prev.weight && kw.hard && !prev.hard) {
                    *existing = idx;
                }
            })
            .or_insert(idx);
    }

    fn insert_keyword(&mut self, kw: Keyword) -> usize {
        let idx = self.keywords.len();
        self.by_text.insert((kw.lang.clone(), kw.text.clone()), idx);
        self.index.insert(idx, &kw);
        self.keywords.push(kw);
        self.sync_any(idx);
        idx
    }

    /// The match mode of a language's keywords.
    pub fn mode_of(&self, lang: &str) -> Option<Match> {
        self.keywords.iter().find(|k| k.lang == lang).map(|k| k.mode)
    }

    /// Languages with keywords, in the order they are matched.
    pub fn languages(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for k in &self.keywords {
            if !out.contains(&k.lang.as_str()) {
                out.push(&k.lang);
            }
        }
        out
    }
}

#[derive(Deserialize)]
struct RawCountryFile {
    countries: BTreeMap<String, RawCountryProfile>,
}

#[derive(Deserialize)]
struct RawCountryProfile {
    name: String,
    #[serde(default)]
    name_local: String,
    iso: String,
    calling_code: String,
    #[serde(default)]
    languages: Vec<String>,
    #[serde(default)]
    prevalence_note: String,
    #[serde(default)]
    cross_border_note: String,
    scams: Vec<RawCountryScam>,
}

#[derive(Deserialize)]
struct RawCountryScam {
    id: String,
    typology: String,
    name: String,
    name_en: String,
    prevalence: String,
    lang: String,
    #[serde(default)]
    context_keywords: Vec<String>,
    #[serde(default)]
    red_flag_keywords: Vec<CountryRedFlag>,
    signals_en: String,
    legit_en: String,
    #[serde(default)]
    bait_tactics: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
}

/// Phrases that expose detection to a scammer ("I'll call 165/110", "hang up",
/// "police never ask for transfers") — forbidden in `bait_tactics`.
const ANTI_STALL_MARKERS: &[&str] = &[
    "165",
    "110",
    "112",
    "911",
    "報警",
    "报警",
    "掛斷",
    "挂断",
    "警察不會",
    "警察不会",
    "警察は電話で",
    "검찰은 전화로",
    "hang up",
    "auflegen",
    "call the official",
    "llamar al número oficial",
    "appeler le numéro officiel",
    "ligar para o número oficial",
];

fn normalize_country_iso(s: &str) -> String {
    match s.trim().to_ascii_lowercase().as_str() {
        "uk" => "gb".to_string(),
        other => other.to_string(),
    }
}

/// Merge `data/country_scams.json` into a parsed [`ScamDb`]: validates every
/// typology, keyword, look-alike and bait tactic, indexes regional scam cards,
/// and adds new context (`weight=1`) and red-flag (`weight=2`/`hard`) keywords.
pub fn merge_countries(db: &mut ScamDb, json: &str) -> Result<(), String> {
    let raw: RawCountryFile = serde_json::from_str(json).map_err(|e| format!("country_scams.json: {e}"))?;
    if raw.countries.is_empty() {
        return Err("country_scams.json has no countries".into());
    }
    let mut scam_ids = HashSet::new();
    for (code, cp) in raw.countries {
        let iso = normalize_country_iso(&cp.iso);
        if iso.len() != 2 || !iso.bytes().all(|b| b.is_ascii_lowercase()) {
            return Err(format!("country {code}: invalid iso {:?}", cp.iso));
        }
        if !cp.calling_code.starts_with('+') || cp.calling_code.len() < 2 {
            return Err(format!("country {code}: invalid calling_code {:?}", cp.calling_code));
        }
        if db.countries.contains_key(&iso) {
            return Err(format!("country iso {iso} listed twice"));
        }
        if cp.scams.is_empty() {
            return Err(format!("country {code} has no scams"));
        }
        let mut scams = Vec::with_capacity(cp.scams.len());
        for s in cp.scams {
            if !valid_id(&s.id) || !scam_ids.insert(s.id.clone()) {
                return Err(format!("country {code}: scam id {:?} invalid or listed twice", s.id));
            }
            let Some(&ti) = db.by_typology.get(&s.typology) else {
                return Err(format!("{}: unknown typology {}", s.id, s.typology));
            };
            let typ_hard = db.typologies[ti].hard;
            let Some(mode) = db.mode_of(&s.lang) else {
                return Err(format!("{}: unknown keyword language {}", s.id, s.lang));
            };
            if s.name.trim().is_empty() || s.name_en.trim().is_empty() {
                return Err(format!("{}: name and name_en required", s.id));
            }
            if s.signals_en.trim().is_empty() || s.legit_en.trim().is_empty() {
                return Err(format!("{}: signals_en and legit_en required", s.id));
            }
            if s.signals_en.chars().count() > 260 || s.legit_en.chars().count() > 260 {
                return Err(format!("{}: keep signals_en and legit_en <= 260 chars", s.id));
            }
            if s.bait_tactics.is_empty() {
                return Err(format!("{}: at least one bait_tactic required", s.id));
            }
            for tactic in &s.bait_tactics {
                let low = tactic.to_lowercase();
                if let Some(&bad) = ANTI_STALL_MARKERS.iter().find(|&&m| low.contains(m)) {
                    return Err(format!("{}: bait_tactic contains anti-stalling giveaway {bad:?}: {tactic}", s.id));
                }
            }
            let norm_word = |raw_kw: &str| -> String {
                if mode == Match::Word {
                    words(raw_kw).join(" ")
                } else {
                    raw_kw.trim().to_string()
                }
            };
            for ctx_kw in &s.context_keywords {
                let cleaned = norm_word(ctx_kw);
                if cleaned.is_empty() {
                    return Err(format!("{}: empty context_keyword", s.id));
                }
                let kw = Keyword::new(&cleaned, 1, &s.typology, false, &s.lang, mode);
                if !db.by_text.contains_key(&(kw.lang.clone(), kw.text.clone())) {
                    db.insert_keyword(kw.clone());
                }
                let entry = db.country_kw_map.entry(kw.text).or_default();
                let pair = (iso.clone(), s.id.clone());
                if !entry.contains(&pair) {
                    entry.push(pair);
                }
            }
            for rf in &s.red_flag_keywords {
                if !(1..=2).contains(&rf.weight) {
                    return Err(format!("{}: red_flag {:?} weight must be 1 or 2", s.id, rf.text));
                }
                if rf.hard && !typ_hard {
                    return Err(format!("{}: red_flag {:?} is hard but {} is not", s.id, rf.text, s.typology));
                }
                let cleaned = norm_word(&rf.text);
                if cleaned.is_empty() {
                    return Err(format!("{}: empty red_flag_keyword", s.id));
                }
                let kw = Keyword::new(&cleaned, rf.weight, &s.typology, rf.hard, &s.lang, mode);
                if let Some(&idx) = db.by_text.get(&(kw.lang.clone(), kw.text.clone())) {
                    if rf.hard && typ_hard && db.keywords[idx].typology != s.typology {
                        return Err(format!(
                            "{}: hard red_flag {:?} conflicts with typology {}",
                            s.id, rf.text, db.keywords[idx].typology
                        ));
                    }
                    if rf.weight > db.keywords[idx].weight {
                        db.keywords[idx].weight = rf.weight;
                    }
                    if rf.hard && typ_hard {
                        db.keywords[idx].hard = true;
                    }
                    db.sync_any(idx);
                } else {
                    db.insert_keyword(kw.clone());
                }
                let entry = db.country_kw_map.entry(kw.text).or_default();
                let pair = (iso.clone(), s.id.clone());
                if !entry.contains(&pair) {
                    entry.push(pair);
                }
            }
            scams.push(CountryScam {
                id: s.id,
                typology: s.typology,
                name: s.name,
                name_en: s.name_en,
                prevalence: s.prevalence,
                lang: s.lang,
                context_keywords: s.context_keywords,
                red_flag_keywords: s.red_flag_keywords,
                signals_en: s.signals_en,
                legit_en: s.legit_en,
                bait_tactics: s.bait_tactics,
                sources: s.sources,
            });
        }
        db.countries.insert(
            iso.clone(),
            CountryProfile {
                code,
                name: cp.name,
                name_local: if cp.name_local.trim().is_empty() { String::new() } else { cp.name_local },
                iso,
                calling_code: cp.calling_code,
                languages: cp.languages,
                prevalence_note: cp.prevalence_note,
                cross_border_note: cp.cross_border_note,
                scams,
            },
        );
    }
    Ok(())
}

pub struct InstalledScams {
    pub version: u64,
    pub db: ScamDb,
}

fn scam_issue(rule: &'static str, detail: impl Into<String>) -> Vec<bageo_packs::Issue> {
    vec![bageo_packs::Issue::new(SCAMS_FILE_NAME, "", rule, detail)]
}

fn parse_scam_bundle(gz: &[u8]) -> Result<InstalledScams, Vec<bageo_packs::Issue>> {
    let sb = bageo_packs::parse_scams(gz)?;
    let db_str = serde_json::to_string(&sb.db).map_err(|e| scam_issue(bageo_packs::rule::FORMAT, e.to_string()))?;
    let cc_str = serde_json::to_string(&sb.countries).map_err(|e| scam_issue(bageo_packs::rule::FORMAT, e.to_string()))?;
    let mut parsed = parse(&db_str).map_err(|e| scam_issue(bageo_packs::rule::FORMAT, e))?;
    merge_countries(&mut parsed, &cc_str).map_err(|e| scam_issue(bageo_packs::rule::FORMAT, e))?;
    supplement_otp_demands(&mut parsed);
    Ok(InstalledScams { version: sb.version, db: parsed })
}

/// Ensure Vietnamese verification-code demands (`mã xác minh`) and Indonesian OTP
/// extraction phrases (`sebutkan otp`, `nomor otp`, `kode rahasia`, …) are present
/// as hard scam triggers even before an external scam-pack update is downloaded.
fn supplement_otp_demands(db: &mut ScamDb) {
    let Some(typology) = db
        .keywords
        .iter()
        .find(|k| k.hard && (k.text.contains("otp") || k.text.contains("驗證碼")))
        .map(|k| k.typology.clone())
        .or_else(|| db.typologies.iter().find(|t| t.hard).map(|t| t.id.clone()))
    else {
        return;
    };
    const EXTRA_HARD: &[(&str, &str)] = &[
        ("vi", "đọc mã xác minh"),
        ("vi", "gửi mã xác minh"),
        ("vi", "cung cấp mã xác minh"),
        ("vi", "nhập mã xác minh"),
        ("vi", "cho xin mã xác minh"),
        ("id", "sebutkan otp"),
        ("id", "sebutkan kode otp"),
        ("id", "sebutkan nomor otp"),
        ("id", "kirimkan kode otp"),
        ("id", "beritahu kode otp"),
        ("id", "kasih kode otp"),
        ("id", "minta kode otp"),
        ("id", "berikan kode rahasia"),
    ];
    for &(lang, phrase) in EXTRA_HARD {
        let kw = Keyword::new(phrase, 2, &typology, true, lang, Match::Word);
        if let Some(&idx) = db.by_text.get(&(kw.lang.clone(), kw.text.clone())) {
            db.keywords[idx].hard = true;
            if db.keywords[idx].weight < 2 {
                db.keywords[idx].weight = 2;
            }
        } else {
            db.insert_keyword(kw);
        }
    }
    for &(lang, phrase) in &[("vi", "mã xác minh"), ("id", "nomor otp"), ("id", "kode rahasia")] {
        let kw = Keyword::new(phrase, 2, &typology, false, lang, Match::Word);
        if !db.by_text.contains_key(&(kw.lang.clone(), kw.text.clone())) {
            db.insert_keyword(kw);
        }
    }
}

pub fn builtin_scams() -> &'static InstalledScams {
    static B: OnceLock<&'static InstalledScams> = OnceLock::new();
    B.get_or_init(|| match parse_scam_bundle(BUILTIN_GZ) {
        Ok(s) => Box::leak(Box::new(s)),
        Err(issues) => {
            warn!(
                "built-in scam database unreadable: {}",
                issues.first().map(|i| i.to_string()).unwrap_or_default()
            );
            Box::leak(Box::new(InstalledScams { version: 0, db: ScamDb::default() }))
        }
    })
}

static CURRENT_SCAMS: RwLock<Option<&'static InstalledScams>> = RwLock::new(None);
static SCAM_INSTALLS: AtomicUsize = AtomicUsize::new(0);
static SCAM_INSTALL_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
thread_local! {
    static TEST_CURRENT_SCAMS: std::cell::Cell<Option<&'static InstalledScams>> = const { std::cell::Cell::new(None) };
}

fn set_current_scams(s: &'static InstalledScams) {
    #[cfg(test)]
    {
        TEST_CURRENT_SCAMS.with(|c| c.set(Some(s)));
    }
    #[cfg(not(test))]
    {
        *CURRENT_SCAMS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
    }
}

fn load_installed_scams(dir: &Path, builtin_ver: u64) -> Option<&'static InstalledScams> {
    let packs = dir.join("packs");
    if let Ok(entries) = std::fs::read_dir(&packs) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(SCAMS_INCOMING_PREFIX) && name.ends_with(SCAMS_INCOMING_SUFFIX) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let path = packs.join(SCAMS_FILE_NAME);
    let bytes = std::fs::read(&path).ok()?;
    match parse_scam_bundle(&bytes) {
        Ok(s) if s.version > builtin_ver => {
            info!("scam database {} in use ({} countries)", s.version, s.db.countries.len());
            Some(Box::leak(Box::new(s)))
        }
        Ok(_) => None,
        Err(issues) => {
            warn!(
                "installed scam database rejected ({}); using built-in",
                issues.first().map(|i| i.to_string()).unwrap_or_default()
            );
            None
        }
    }
}

pub fn current_scams() -> &'static InstalledScams {
    #[cfg(test)]
    {
        if let Some(s) = TEST_CURRENT_SCAMS.with(|c| c.get()) {
            return s;
        }
    }
    if let Some(s) = *CURRENT_SCAMS.read().unwrap_or_else(|e| e.into_inner()) {
        return s;
    }
    let mut slot = CURRENT_SCAMS.write().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = *slot {
        return s;
    }
    let s = if cfg!(test) {
        builtin_scams()
    } else {
        load_installed_scams(&crate::packs_store::data_dir(), builtin_scams().version).unwrap_or_else(builtin_scams)
    };
    *slot = Some(s);
    s
}

/// The active scam database (installed hot-update when newer, else built-in).
pub fn db() -> &'static ScamDb {
    &current_scams().db
}

pub fn current_version() -> u64 {
    current_scams().version
}

pub(crate) fn builtin_in_use() -> bool {
    std::ptr::eq(current_scams(), builtin_scams())
}

pub(crate) fn installs_left() -> usize {
    MAX_INSTALLS.saturating_sub(SCAM_INSTALLS.load(Ordering::SeqCst))
}

pub(crate) fn incoming_path(packs: &Path) -> PathBuf {
    packs.join(format!(
        "{SCAMS_INCOMING_PREFIX}{}-{}{SCAMS_INCOMING_SUFFIX}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ))
}

pub(crate) fn install_file(staged: &Path, want: u64) -> Result<u64, Vec<bageo_packs::Issue>> {
    let _guard = SCAM_INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let res = (|| {
        if SCAM_INSTALLS.load(Ordering::SeqCst) >= MAX_INSTALLS {
            return Err(scam_issue(
                bageo_packs::rule::STATUS,
                format!("{MAX_INSTALLS} updates installed since Bageo started; restart Bageo first"),
            ));
        }
        let bytes = std::fs::read(staged).map_err(|e| scam_issue(bageo_packs::rule::FORMAT, format!("cannot read scams pack: {e}")))?;
        let parsed = parse_scam_bundle(&bytes)?;
        if parsed.version != want {
            return Err(scam_issue(bageo_packs::rule::STATUS, format!("version {} is not {want}", parsed.version)));
        }
        let in_use = current_version();
        if parsed.version <= in_use {
            return Err(scam_issue(bageo_packs::rule::STATUS, format!("version {} is not newer than {in_use}", parsed.version)));
        }
        let dest = crate::packs_store::installed_dir().join(SCAMS_FILE_NAME);
        std::fs::rename(staged, dest).map_err(|e| scam_issue(bageo_packs::rule::FORMAT, format!("cannot install scams pack: {e}")))?;
        SCAM_INSTALLS.fetch_add(1, Ordering::SeqCst);
        let leaked: &'static InstalledScams = Box::leak(Box::new(parsed));
        set_current_scams(leaked);
        Ok(leaked.version)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    res
}

pub fn restore_builtin() -> std::io::Result<()> {
    let _guard = SCAM_INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = crate::packs_store::installed_dir().join(SCAMS_FILE_NAME);
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    set_current_scams(builtin_scams());
    Ok(())
}

#[cfg(test)]
fn builtin_db_json() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| {
        let sb = bageo_packs::parse_scams(BUILTIN_GZ).expect("built-in scams gz");
        serde_json::to_string(&sb.db).unwrap()
    })
}

#[cfg(test)]
fn builtin_countries_json() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| {
        let sb = bageo_packs::parse_scams(BUILTIN_GZ).expect("built-in scams gz");
        serde_json::to_string(&sb.countries).unwrap()
    })
}

pub fn typology(id: &str) -> Option<&'static Typology> {
    db().typology(id)
}

// ---------------------------------------------------------------- the owner's own file

/// A number the owner listed in `scam_db.local.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LocalNumber {
    pub number: String,
    /// `scam`, `sales` or `safe`.
    pub verdict: String,
    #[serde(default)]
    pub typology: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Deserialize)]
struct RawLocal {
    #[serde(default)]
    keywords: Vec<RawLocalKeyword>,
    #[serde(default)]
    numbers: Vec<LocalNumber>,
}

#[derive(Debug, Deserialize)]
struct RawLocalKeyword {
    text: String,
    #[serde(default = "default_weight")]
    weight: u32,
    typology: String,
    lang: String,
    #[serde(default)]
    hard: bool,
}

fn default_weight() -> u32 {
    1
}

/// The owner's additions, as read from their file.
#[derive(Debug, Default)]
pub struct LocalDb {
    pub keywords: Vec<&'static Keyword>,
    pub numbers: Vec<LocalNumber>,
    /// Entries left out, and why.
    pub skipped: Vec<String>,
    index: KeywordIndex,
}

/// Local keywords live as long as the agent (hits are `&'static str`); the
/// same entry read again after the file changed is not stored twice.
fn intern(kw: Keyword) -> &'static Keyword {
    static SEEN: OnceLock<Mutex<HashMap<(String, String, u32, String, bool), &'static Keyword>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let key = (kw.lang.clone(), kw.text.clone(), kw.weight, kw.typology.clone(), kw.hard);
    seen.entry(key).or_insert_with(|| Box::leak(Box::new(kw)))
}

/// Read the owner's file. Entries the built-in database cannot place (an
/// unknown typology or language, a weight other than 1 or 2, a keyword identical
/// to what is already listed, or a verdict other than scam / sales / safe) are
/// left out and named; an entry with the same `(lang, text)` but changed
/// weight, typology, or hard flag overrides the prior definition.
pub fn parse_local(json: &str) -> Result<LocalDb, String> {
    let raw: RawLocal = serde_json::from_str(json).map_err(|e| format!("scam_db.local.json: {e}"))?;
    let base = db();
    let mut out = LocalDb::default();
    let mut seen: HashMap<(String, String), usize> = HashMap::new();
    for k in raw.keywords {
        let Some(t) = base.typology(&k.typology) else {
            out.skipped.push(format!("{}: unknown typology {}", k.text, k.typology));
            continue;
        };
        let Some(mode) = base.mode_of(&k.lang) else {
            out.skipped.push(format!("{}: unknown language {}", k.text, k.lang));
            continue;
        };
        if k.text.trim().is_empty() || !(1..=2).contains(&k.weight) {
            out.skipped.push(format!("{:?}: empty, or weight not 1 or 2", k.text));
            continue;
        }
        let kw = Keyword::new(&k.text, k.weight, &t.id, k.hard && t.hard, &k.lang, mode);
        let key = (kw.lang.clone(), kw.text.clone());
        if let Some(&prev_idx) = seen.get(&key) {
            let prev = out.keywords[prev_idx];
            if prev.weight == kw.weight && prev.typology == kw.typology && prev.hard == kw.hard {
                out.skipped.push(format!("{}: already listed", kw.text));
            } else {
                out.keywords[prev_idx] = intern(kw);
            }
            continue;
        }
        if base.keyword_in(&kw.lang, &kw.text).is_some_and(|b| {
            b.weight == kw.weight && b.typology == kw.typology && b.hard == kw.hard
        }) {
            out.skipped.push(format!("{}: already listed", kw.text));
            continue;
        }
        let idx = out.keywords.len();
        seen.insert(key, idx);
        let interned = intern(kw);
        out.index.insert(idx, interned);
        out.keywords.push(interned);
    }
    for n in raw.numbers {
        if !["scam", "sales", "safe"].contains(&n.verdict.as_str()) || number_key(&n.number, None).is_none() {
            out.skipped.push(format!("{}: verdict must be scam, sales or safe and the number at least 5 digits", n.number));
            continue;
        }
        out.numbers.push(n);
    }
    Ok(out)
}

struct LocalState {
    path: Option<PathBuf>,
    checked: Option<Instant>,
    stamp: Option<(SystemTime, u64)>,
    db: LocalDb,
}

fn local_state() -> &'static Mutex<LocalState> {
    static LOCAL: OnceLock<Mutex<LocalState>> = OnceLock::new();
    LOCAL.get_or_init(|| {
        Mutex::new(LocalState {
            path: None,
            checked: None,
            stamp: None,
            db: LocalDb::default(),
        })
    })
}

/// The owner's additions, read again when the file changed (looked at every
/// [`LOCAL_RECHECK`] at most). A missing file is none; a broken one is
/// ignored and logged.
fn with_local<R>(f: impl FnOnce(&LocalDb) -> R) -> R {
    let mut st = local_state().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(path) = st.path.clone() {
        if st.checked.map_or(true, |t| t.elapsed() >= LOCAL_RECHECK) {
            st.checked = Some(Instant::now());
            let stamp = std::fs::metadata(&path).ok().map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
            if stamp != st.stamp {
                st.stamp = stamp;
                st.db = match stamp.and_then(|_| std::fs::read_to_string(&path).ok()) {
                    None => LocalDb::default(),
                    Some(text) => match parse_local(&text) {
                        Ok(local) => {
                            info!(
                                "scam database: {} own keyword(s), {} own number(s) from {} (left out: {:?})",
                                local.keywords.len(),
                                local.numbers.len(),
                                path.display(),
                                local.skipped
                            );
                            local
                        }
                        Err(e) => {
                            warn!("scam database: own file ignored: {e}");
                            LocalDb::default()
                        }
                    },
                };
            }
        }
    }
    f(&st.db)
}

// ---------------------------------------------------------------- keywords

fn keyword_hits_in(text: &str, local: &LocalDb) -> Vec<Hit> {
    let p = Prepared::of(text);
    let base = db();
    let mut tagged: Vec<(&str, Hit)> = base
        .index
        .candidates(&p)
        .into_iter()
        .map(|i| &base.keywords[i])
        .filter(|k| k.found_in(&p))
        .map(|k| (k.lang.as_str(), hit_of(k)))
        .collect();
    if !local.keywords.is_empty() {
        for i in local.index.candidates(&p) {
            let k = local.keywords[i];
            if !k.found_in(&p) {
                continue;
            }
            if let Some(slot) = tagged
                .iter_mut()
                .find(|(lang, h)| *lang == k.lang.as_str() && h.text == k.text)
            {
                slot.1 = hit_of(k);
            } else {
                tagged.push((k.lang.as_str(), hit_of(k)));
            }
        }
    }
    tagged.into_iter().map(|(_, h)| h).collect()
}

/// The hits without an accented keyword that only matched as part of a longer
/// hit of another language ("crédit" of the French list inside "credit card"
/// heard in English): one word heard, kept as the longer keyword. Same-language
/// pairs ("bearbeitungsgebühr" in "bearbeitungsgebühr vorab") and hard words stay.
pub fn without_folded_repeats(hits: &[String]) -> Vec<String> {
    let folded: Vec<String> = hits.iter().map(|h| fold_accents(h.trim())).collect();
    hits.iter()
        .enumerate()
        .filter(|(i, h)| {
            let h = h.trim();
            let accented = folded[*i] != h.to_lowercase();
            let inside_other = hits.iter().enumerate().any(|(j, o)| {
                j != *i && !o.contains(h) && folded[j].len() > folded[*i].len() && folded[j].contains(folded[*i].as_str())
            });
            !(accented && inside_other && !is_hard(h))
        })
        .map(|(_, h)| h.clone())
        .collect()
}

/// Distinct keywords (built-in, then the owner's) found in `text`, in database order.
pub fn keyword_hits(text: &str) -> Vec<Hit> {
    with_local(|local| keyword_hits_in(text, local))
}

/// A keyword by the text a hit reports (owner overrides take precedence).
pub fn keyword(text: &str) -> Option<Hit> {
    if let Some(hit) = with_local(|local| local.keywords.iter().find(|k| k.text == text).map(|&k| hit_of(k))) {
        return Some(hit);
    }
    db().keyword(text).map(hit_of)
}

/// A hit asks for something only scammers ask for (a code, ATM steps, gift
/// cards, remote access, a "safe account", secrecy).
pub fn is_hard(text: &str) -> bool {
    keyword(text.trim()).is_some_and(|h| h.hard)
}

/// `text` contains at least one keyword that only scammers say (`hard: true`).
pub fn has_hard_hit(text: &str) -> bool {
    keyword_hits(text).iter().any(|h| h.hard)
}

/// The kind of call the hits point to: the typology with the most weight among
/// them (the first heard on a tie).
pub fn typology_for_hits(hits: &[&str]) -> Option<&'static str> {
    let mut sums: Vec<(&'static str, u32)> = Vec::new();
    for h in hits.iter().filter_map(|t| keyword(t)) {
        match sums.iter_mut().find(|(t, _)| *t == h.typology) {
            Some((_, w)) => *w += h.weight,
            None => sums.push((h.typology, h.weight)),
        }
    }
    let best = sums.iter().map(|&(_, w)| w).max()?;
    sums.into_iter().find(|&(_, w)| w == best).map(|(t, _)| t)
}

/// Phrases that scam and sales openers use as well ("your claim", "votre
/// dossier", 折り返し, 민원, "seu pedido"): one of them alone is no proof of a
/// follow-up on the owner's own matter; two different ones, or any other
/// own-matter phrase, are.
///
/// "About your request / application / inquiry / call" is how sales and scam
/// calls open too ("по вашему запросу на кредит…", 「お申込みの件で、保証金を…」,
/// "신청하신 건 대출 승인…"): weak. Only a phrase about a complaint or repair the
/// owner reported being dealt with is strong on its own. The same goes for the
/// generic "your request / inquiry / call", "following up on your…", "returning
/// your call", 您申請的 and 調查結果 (fake prosecutors say it too) in the other
/// languages. Each string must equal an own-matter phrase of the database.
const WEAK_OWN_MATTER: &[&str] = &[
    "折り返し", "민원", "votre dossier", "seu pedido", "your claim",
    // en
    "your request", "your inquiry", "your enquiry", "following up on your", "regarding your case", "you contacted us",
    "returning your call",
    // zh-TW
    "您申請的", "你申請的", "調查結果", "調查的結果",
    // es, fr, de, pt, it
    "su consulta", "su solicitud", "devolver su llamada",
    "votre demande", "suite à votre appel",
    "ihre anfrage", "ihrer anfrage", "ihr anliegen", "ihrem anliegen", "ihres anliegens", "ihren rückruf", "ihrem rückruf",
    "sua solicitação", "retornando sua ligação",
    "la sua richiesta", "come da sua richiesta", "la sua pratica",
    // id, ms, tl, tr, vi
    "menindaklanjuti permintaan anda", "pertanyaan anda", "susulan permohonan anda", "tungkol sa request mo",
    "talebiniz üzerine", "phản hồi yêu cầu của",
    // ru
    "по вашему запросу", "по вашей заявке", "по вашему заявлению", "вашего обращения", "ваше обращение рассмотрено",
    // ja
    "お申し込みの件", "お申込みの件", "ご申請の件", "ご相談の件", "お問合せの件", "ご依頼いただいた件", "お電話いただいた件",
    // ko
    "신청하신 건", "문의해 주신", "접수해 주신", "요청해 주신", "남겨 주신 문의",
    // hi
    "आपके आवेदन के बारे में", "आपके अनुरोध के बारे में", "आपकी पूछताछ", "आपकी समस्या के बारे में", "आपने संपर्क किया था",
    // ar
    "بخصوص طلبك", "بشأن طلبك", "استفسارك",
];

/// The caller follows up on something the owner raised (a complaint, a repair,
/// an application): never a pitch, in any language of the database — unless the
/// caller also makes a scam-only demand (OTP, ATM steps, safe account, screen
/// sharing, marketplace escrow verification) or an advance-fee loan pitch,
/// which spoofs a follow-up.
pub fn own_matter(text: &str) -> bool {
    if keyword_hits(text).iter().any(|h| h.hard || h.typology == "loan_advance_fee") {
        return false;
    }
    let han = crate::zh_tw::norm(text);
    let lower = text.to_lowercase();
    let folded = fold_accents(text);
    let squeezed = compact(&folded);
    let mut weak: Vec<&str> = Vec::new();
    for (lang, phrases) in &db().own_matter {
        for p in phrases {
            let found = if lang.starts_with("zh") {
                han.contains(p.as_str())
            } else if lang.starts_with("ko") {
                squeezed.contains(compact(p).as_str())
            } else {
                lower.contains(p.as_str()) || folded.contains(fold_accents(p).as_str())
            };
            if !found {
                continue;
            }
            if !WEAK_OWN_MATTER.contains(&p.as_str()) {
                return true;
            }
            if !weak.contains(&p.as_str()) {
                weak.push(p.as_str());
            }
        }
    }
    weak.len() >= 2
}

// ---------------------------------------------------------------- numbers

fn digits(s: &str) -> String {
    s.chars().filter(char::is_ascii_digit).collect()
}

/// The caller ID is hidden (`unknown`, `private`, empty, no digits).
pub fn is_hidden(raw: &str) -> bool {
    let t = raw.trim();
    t.is_empty() || t.chars().any(char::is_alphabetic) || digits(t).is_empty()
}

/// The key a number is remembered under: its international form when the
/// SIM's country places it, its digits otherwise. Hidden numbers have none.
pub fn number_key(raw: &str, sim: Option<&str>) -> Option<String> {
    if is_hidden(raw) {
        return None;
    }
    if let Some(n) = crate::phone_region::normalize(raw, sim) {
        return Some(n.e164);
    }
    let d = digits(raw);
    (d.len() >= 5).then_some(d)
}

/// The number rules that apply to a caller ID on a SIM of `sim` (ISO country).
pub fn number_rules(raw: &str, sim: Option<&str>) -> Vec<&'static NumberRule> {
    let hidden = is_hidden(raw);
    let sim = sim.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty());
    let shown: String = raw.trim().chars().filter(|c| c.is_ascii_digit() || *c == '+').collect();
    let normal = if hidden { None } else { crate::phone_region::normalize(raw, sim.as_deref()) };
    db().number_rules
        .iter()
        .filter(|r| match r.kind {
            RuleKind::Hidden => hidden,
            RuleKind::Foreign => match (&normal, &sim) {
                (Some(n), Some(s)) => n.country != s.as_str(),
                _ => false,
            },
            RuleKind::Prefix => {
                !hidden
                    && (r.sim_countries.is_empty() || sim.as_ref().is_some_and(|s| r.sim_countries.contains(s)))
                    && (r.raw_prefixes.iter().any(|p| shown.starts_with(p.as_str()))
                        || normal.as_ref().is_some_and(|n| r.e164_prefixes.iter().any(|p| n.e164.starts_with(p.as_str()))))
            }
        })
        .collect()
}

/// What is remembered about one number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NumberEntry {
    /// `scam`, `sales` or `safe`.
    pub verdict: String,
    /// `owner` (marked on the console) or `auto` (judged during a call).
    pub source: String,
    /// Times marked or judged so.
    pub count: u32,
    /// RFC 3339, UTC.
    pub last_at: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub typology: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct NumberFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    numbers: BTreeMap<String, NumberEntry>,
}

/// Numbers marked by the owner or judged sales / scam during calls, kept in
/// `scam_numbers.json` (in memory only without a folder).
pub struct NumberBook {
    dir: Option<PathBuf>,
    numbers: Mutex<BTreeMap<String, NumberEntry>>,
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl NumberBook {
    /// The book kept in `dir` (`{CALLS_DATA_DIR}/config`). A broken file is
    /// logged and started over (it is a record of marks, not settings).
    pub fn open(dir: Option<PathBuf>) -> NumberBook {
        let numbers = dir
            .as_ref()
            .and_then(|d| std::fs::read_to_string(d.join("scam_numbers.json")).ok())
            .map(|text| match serde_json::from_str::<NumberFile>(&text) {
                Ok(f) => f.numbers,
                Err(e) => {
                    warn!("scam_numbers.json unreadable, starting empty: {e}");
                    BTreeMap::new()
                }
            })
            .unwrap_or_default();
        NumberBook {
            dir,
            numbers: Mutex::new(numbers),
        }
    }

    pub fn get(&self, key: &str) -> Option<NumberEntry> {
        self.numbers.lock().unwrap_or_else(|e| e.into_inner()).get(key).cloned()
    }

    pub fn len(&self) -> usize {
        self.numbers.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Read `scam_numbers.json` again (a backup put it back).
    pub fn reload(&self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let fresh = NumberBook::open(Some(dir.clone()));
        let map = fresh.numbers.into_inner().unwrap_or_else(|e| e.into_inner());
        *self.numbers.lock().unwrap_or_else(|e| e.into_inner()) = map;
    }

    fn save(&self, map: &BTreeMap<String, NumberEntry>) {
        let Some(dir) = &self.dir else { return };
        let file = NumberFile {
            version: 1,
            numbers: map.clone(),
        };
        let res = serde_json::to_vec_pretty(&file).map_err(anyhow::Error::from).and_then(|data| {
            std::fs::create_dir_all(dir)?;
            let tmp = dir.join("scam_numbers.json.tmp");
            std::fs::write(&tmp, data)?;
            std::fs::rename(&tmp, dir.join("scam_numbers.json"))?;
            Ok(())
        });
        if let Err(e) = res {
            warn!("scam_numbers.json not saved: {e:#}");
        }
    }

    /// The owner's mark (`scam` / `sales` / `safe`): it replaces what the calls judged.
    pub fn mark_owner(&self, key: &str, verdict: &str, typology: &str) -> NumberEntry {
        let mut map = self.numbers.lock().unwrap_or_else(|e| e.into_inner());
        let count = match map.get(key) {
            Some(e) if e.source == "owner" && e.verdict == verdict => e.count + 1,
            _ => 1,
        };
        let entry = NumberEntry {
            verdict: verdict.to_string(),
            source: "owner".into(),
            count,
            last_at: now_rfc3339(),
            typology: typology.to_string(),
        };
        map.insert(key.to_string(), entry.clone());
        self.save(&map);
        entry
    }

    /// A call from `key` was judged sales / scam. The owner's own mark stands
    /// (`None`): they know the number better than one call.
    pub fn record_auto(&self, key: &str, verdict: &str, typology: &str) -> Option<NumberEntry> {
        let mut map = self.numbers.lock().unwrap_or_else(|e| e.into_inner());
        let prev = map.get(key).cloned();
        if prev.as_ref().is_some_and(|e| e.source == "owner") {
            return None;
        }
        let entry = NumberEntry {
            verdict: verdict.to_string(),
            source: "auto".into(),
            count: prev.as_ref().map_or(1, |e| e.count + 1),
            last_at: now_rfc3339(),
            typology: if typology.is_empty() { prev.map(|e| e.typology).unwrap_or_default() } else { typology.to_string() },
        };
        map.insert(key.to_string(), entry.clone());
        self.save(&map);
        Some(entry)
    }

    /// One line of `scam_feedback.jsonl` (the owner's marks with what the call
    /// had judged), for tuning the database later.
    pub fn log_feedback(&self, line: &Value) {
        let Some(dir) = &self.dir else { return };
        let res = (|| -> std::io::Result<()> {
            use std::io::Write as _;
            std::fs::create_dir_all(dir)?;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("scam_feedback.jsonl"))?;
            writeln!(f, "{line}")
        })();
        if let Err(e) = res {
            warn!("scam_feedback.jsonl not written: {e}");
        }
    }
}

fn book_slot() -> &'static OnceLock<NumberBook> {
    static BOOK: OnceLock<NumberBook> = OnceLock::new();
    &BOOK
}

/// The agent's number book (in memory until [`init`] gives it a folder).
pub fn numbers() -> &'static NumberBook {
    book_slot().get_or_init(|| NumberBook::open(None))
}

/// At start: the owner's file and the number book live in `{data_dir}/config`.
pub fn init(data_dir: &Path) {
    let config = data_dir.join("config");
    let _ = book_slot().set(NumberBook::open(Some(config.clone())));
    local_state().lock().unwrap_or_else(|e| e.into_inner()).path = Some(config.join("scam_db.local.json"));
    let base = db();
    info!(
        "scam database v{}: {} typologies, {} keywords in {} languages ({} country profiles), {} number rules; {} remembered number(s)",
        base.version,
        base.typologies.len(),
        base.keywords.len(),
        base.languages().len(),
        base.countries.len(),
        base.number_rules.len(),
        numbers().len()
    );
}

/// What the second look is told about the caller's number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallerFacts {
    /// The owner marked the number safe: treated like a saved contact.
    pub owner_safe: bool,
    /// The owner marked it scam / sales, or [`AUTO_FLAG_MIN`] calls from it were
    /// judged so: the call is looked at sooner.
    pub flagged_before: bool,
    /// One English line each, for the reviewing model.
    pub notes: Vec<String>,
}

fn date_of(rfc3339: &str) -> &str {
    rfc3339.get(..10).unwrap_or(rfc3339)
}

fn typology_note(id: &str) -> String {
    typology(id).map(|t| format!(" ({})", t.id)).unwrap_or_default()
}

/// The number book's and the owner's file's marks and the number rules, for
/// a caller ID on a SIM of `sim`.
pub fn caller_facts_in(book: &NumberBook, local: &[LocalNumber], raw: &str, sim: Option<&str>) -> CallerFacts {
    let mut f = CallerFacts::default();
    let key = number_key(raw, sim);
    if let Some(e) = key.as_deref().and_then(|k| book.get(k)) {
        let times = if e.count == 1 { "once".to_string() } else { format!("{} times", e.count) };
        match (e.source.as_str(), e.verdict.as_str()) {
            ("owner", "safe") => {
                f.owner_safe = true;
                f.notes.push("The owner marked this number as safe (not a scam or sales call).".into());
            }
            ("owner", v) => {
                f.flagged_before = true;
                f.notes.push(format!("The owner marked this number as {v} {times}, last on {}{}.", date_of(&e.last_at), typology_note(&e.typology)));
            }
            (_, v) => {
                f.flagged_before = e.count >= AUTO_FLAG_MIN;
                f.notes.push(format!("Earlier calls from this number were judged {v} {times}, last on {}{}.", date_of(&e.last_at), typology_note(&e.typology)));
            }
        }
    }
    let caller_digits = digits(raw);
    for n in local {
        let same = match (number_key(&n.number, sim), &key) {
            (Some(a), Some(b)) => &a == b,
            _ => false,
        } || (!caller_digits.is_empty() && digits(&n.number) == caller_digits);
        if !same {
            continue;
        }
        if n.verdict == "safe" {
            f.owner_safe = true;
        } else {
            f.flagged_before = true;
        }
        let note = if n.note.trim().is_empty() { String::new() } else { format!(": {}", n.note.trim()) };
        f.notes.push(format!("The owner's own list marks this number as {}{}{note}.", n.verdict, typology_note(&n.typology)));
    }
    for r in number_rules(raw, sim) {
        f.notes.push(format!("Number hint ({}): {}", r.id, r.note));
    }
    f
}

/// [`caller_facts_in`] with the agent's number book and the owner's file, plus
/// the receiving SIM's ISO region for regional scam review.
pub fn caller_facts(raw: &str, sim: Option<&str>) -> CallerFacts {
    let local = with_local(|l| l.numbers.clone());
    let mut f = caller_facts_in(numbers(), &local, raw, sim);
    if let Some(iso) = sim.map(normalize_country_iso).filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_lowercase())) {
        f.notes.push(format!("Phone SIM region: {}", iso.to_ascii_uppercase()));
    }
    f
}

// ---------------------------------------------------------------- country intelligence

const SIM_NOTE_PREFIX: &str = "Phone SIM region: ";
const COUNTRY_BRIEF_MAX_CHARS: usize = 780;

fn sim_from_notes(notes: &[String]) -> Option<String> {
    notes.iter().find_map(|n| {
        n.trim()
            .strip_prefix(SIM_NOTE_PREFIX)
            .map(normalize_country_iso)
            .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_lowercase()))
    })
}

fn normalized_hit_key(h: &str) -> String {
    let t = h.trim();
    let base = db();
    let low = t.to_lowercase();
    if base.country_kw_map.contains_key(&low) {
        return low;
    }
    let w = words(t).join(" ");
    if base.country_kw_map.contains_key(&w) {
        return w;
    }
    let han = han_text(t);
    if base.country_kw_map.contains_key(&han) {
        return han;
    }
    low
}

fn prevalence_rank(p: &str) -> u8 {
    match p {
        "very_high" => 3,
        "high" => 2,
        "medium" => 1,
        _ => 0,
    }
}

fn dialogue_script_iso(dialogue: &str) -> Option<&'static str> {
    if dialogue.chars().any(|c| ('\u{3040}'..='\u{30FF}').contains(&c)) {
        return Some("jp");
    }
    if dialogue.chars().any(|c| ('\u{AC00}'..='\u{D7AF}').contains(&c)) {
        return Some("kr");
    }
    if dialogue.chars().any(|c| matches!(c, '账' | '户' | '转' | '证' | '骗' | '贷' | '鱼' | '们')) {
        return Some("cn");
    }
    if dialogue.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c)) {
        return Some("tw");
    }
    None
}

/// Active country ISO codes (`tw`, `us`, `gb`, `jp`, …) for one call: SIM
/// country first, then caller E.164 country, then countries matched by keywords
/// or script.
pub fn resolve_call_countries(
    raw_number: &str,
    number_notes: &[String],
    hits: &[String],
    dialogue: &str,
) -> (Option<String>, Option<String>, Vec<String>) {
    let base = db();
    let sim_iso = sim_from_notes(number_notes);
    let caller_iso = (!is_hidden(raw_number))
        .then(|| crate::phone_region::normalize(raw_number, sim_iso.as_deref()).map(|n| normalize_country_iso(n.country)))
        .flatten();
    let mut active: Vec<String> = Vec::new();
    fn push_iso(active: &mut Vec<String>, base: &ScamDb, iso: &str) {
        if base.countries.contains_key(iso) && !active.iter().any(|x| x == iso) {
            active.push(iso.to_string());
        }
    }
    if let Some(ref s) = sim_iso {
        push_iso(&mut active, base, s);
    }
    if let Some(ref c) = caller_iso {
        push_iso(&mut active, base, c);
    }
    for h in hits {
        let key = normalized_hit_key(h);
        if let Some(pairs) = base.country_kw_map.get(&key) {
            for (iso, _) in pairs {
                push_iso(&mut active, base, iso);
            }
        }
    }
    if active.is_empty() {
        if let Some(s) = dialogue_script_iso(dialogue) {
            push_iso(&mut active, base, s);
        }
    }
    (sim_iso, caller_iso, active)
}

/// Compact regional scam intelligence for [`crate::stall_review`], kept within
/// [`COUNTRY_BRIEF_MAX_CHARS`] so Gemini Flash-Lite stays well under its 1.5s budget.
pub fn country_review_brief(
    raw_number: &str,
    number_notes: &[String],
    hits: &[String],
    dialogue: &str,
) -> String {
    let base = db();
    let (sim_iso, caller_iso, active) = resolve_call_countries(raw_number, number_notes, hits, dialogue);
    if active.is_empty() {
        return String::new();
    }
    let mut matched_ids: HashSet<&str> = HashSet::new();
    for h in hits {
        let key = normalized_hit_key(h);
        if let Some(pairs) = base.country_kw_map.get(&key) {
            for (_, sid) in pairs {
                matched_ids.insert(sid.as_str());
            }
        }
    }
    let mut out = String::new();
    if let (Some(s), Some(c)) = (sim_iso.as_deref(), caller_iso.as_deref()) {
        if s != c {
            if let Some(cp) = base.countries.get(c) {
                if !cp.cross_border_note.trim().is_empty() {
                    out.push_str(&format!("Cross-border note ({}->{}): {}\n", cp.code, s.to_ascii_uppercase(), cp.cross_border_note.trim()));
                }
            }
        }
    }
    let mut candidates: Vec<(&CountryProfile, &CountryScam, bool)> = Vec::new();
    for iso in active.iter().take(2) {
        if let Some(cp) = base.countries.get(iso) {
            for s in &cp.scams {
                let hit = matched_ids.contains(s.id.as_str());
                candidates.push((cp, s, hit));
            }
        }
    }
    candidates.sort_by_key(|&(_, s, hit)| std::cmp::Reverse((hit, prevalence_rank(&s.prevalence))));
    let mut shown = 0usize;
    for (cp, s, _) in candidates {
        if shown >= 2 {
            break;
        }
        let line = format!(
            "- [{} -> {}] {} Ordinary: {}\n",
            cp.code,
            s.typology,
            s.signals_en.trim(),
            s.legit_en.trim()
        );
        if out.chars().count() + line.chars().count() + 28 > COUNTRY_BRIEF_MAX_CHARS {
            break;
        }
        if shown == 0 {
            out.push_str("Regional scam patterns:\n");
        }
        out.push_str(&line);
        shown += 1;
    }
    out
}

/// Country-specific Stall Mode bait hooks for the rotated Gemini Live prompt
/// once a call is judged sales/scam. Never contains police/hotline giveaways.
pub fn country_bait_addon(
    raw_number: &str,
    number_notes: &[String],
    typology_id: &str,
    hits: &[&str],
    dialogue: &str,
) -> Option<String> {
    let base = db();
    let hit_strings: Vec<String> = hits.iter().map(|s| (*s).to_string()).collect();
    let (_, _, active) = resolve_call_countries(raw_number, number_notes, &hit_strings, dialogue);
    let mut matched_ids: Vec<(String, String)> = Vec::new();
    for h in hits {
        let key = normalized_hit_key(h);
        if let Some(pairs) = base.country_kw_map.get(&key) {
            for pair in pairs {
                if !matched_ids.contains(pair) {
                    matched_ids.push(pair.clone());
                }
            }
        }
    }
    let chosen: Option<(&CountryProfile, &CountryScam)> = matched_ids
        .iter()
        .filter_map(|(iso, sid)| {
            let cp = base.countries.get(iso)?;
            let sc = cp.scams.iter().find(|s| &s.id == sid)?;
            Some((cp, sc))
        })
        .max_by_key(|(cp, sc)| {
            (
                active.first().is_some_and(|a| a == &cp.iso),
                sc.typology == typology_id,
                prevalence_rank(&sc.prevalence),
            )
        })
        .or_else(|| {
            active.iter().find_map(|iso| {
                let cp = base.countries.get(iso)?;
                let sc = cp.scams.iter().find(|s| s.typology == typology_id)?;
                Some((cp, sc))
            })
        });
    let (cp, sc) = chosen?;
    let hooks = sc.bait_tactics.join("；");
    Some(match sc.lang.as_str() {
        "zh-TW" => ZH_COUNTRY_BAIT_ADDON.fill(&[("country", &cp.name_local), ("scam", &sc.name), ("hooks", &hooks)]),
        "zh-CN" => ZH_COUNTRY_BAIT_ADDON_CN.fill(&[("country", &cp.name_local), ("scam", &sc.name), ("hooks", &hooks)]),
        _ => EN_COUNTRY_BAIT_ADDON.fill(&[("country", &cp.name), ("scam", &sc.name_en), ("hooks", &sc.bait_tactics.join("; "))]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Lang;

    /// The 80 languages the database covers (9 core languages, then 71 extra languages).
    const LANGS: [&str; 80] = [
        "zh-TW", "zh-CN", "en", "es", "fr", "de", "pt", "ja", "ko", "it", "vi", "id", "ms", "tl", "tr", "ru", "hi", "ar", "th",
        "nl", "pl", "ro", "uk", "bn", "mr", "ta", "te", "sv", "nb", "da", "fi", "is", "cs", "sk", "hu", "bg", "el", "hr", "sr",
        "sl", "mk", "sq", "he", "ur", "ne", "si", "my", "lo", "mn", "fa", "ps", "az", "hy", "ka", "sw", "am", "af", "et", "lv", "lt", "be",
        "ca", "ceb", "eu", "fil", "gl", "gu", "ht", "jv", "kn", "kok", "la", "lb", "mai", "mg", "ml", "nn", "or", "pa", "sd",
    ];

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bageo-scamdb-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_built_in_database_parses_and_hangs_together() {
        let db = parse(builtin_db_json()).expect("data/scam_db.json");
        assert!(db.version >= 1);
        assert!(db.typologies.len() >= 40, "{}", db.typologies.len());
        for t in &db.typologies {
            for lang in NAME_LANGS {
                assert!(!t.names[lang].trim().is_empty(), "{} {lang}", t.id);
            }
            assert!(t.signals.len() <= 200 && t.legit.len() <= 200, "{}: keep the review prompt short", t.id);
            assert!(db.keywords.iter().any(|k| k.typology == t.id), "{} has no keyword", t.id);
        }
        for l in crate::locale::ALL {
            assert!(db.typologies.iter().all(|t| !t.name(l).is_empty()), "{l:?}");
        }
        let mut seen = HashSet::new();
        for k in &db.keywords {
            assert!(seen.insert((k.lang.as_str(), k.text.as_str())), "{} / {} twice", k.lang, k.text);
            assert!(db.typology(&k.typology).is_some(), "{}", k.text);
            assert!((1..=2).contains(&k.weight), "{}", k.text);
            assert!(!k.hard || db.typology(&k.typology).unwrap().hard, "{}", k.text);
            assert!(k.hard as u32 <= k.weight - 1, "a hard keyword weighs 2: {}", k.text);
        }
        assert_eq!(db.languages(), LANGS.to_vec());
        for lang in LANGS {
            assert!(db.keywords.iter().filter(|k| k.lang == lang && k.weight == 2).count() >= 8, "{lang}");
            if lang != "zh-CN" {
                assert!(db.own_matter.iter().any(|(l, p)| l == lang && !p.is_empty()), "{lang}: own-matter phrases");
            }
        }
        assert!(db.number_rules.iter().any(|r| r.id == "tw_shown_as_international"));
        assert!(db.keywords.iter().filter(|k| k.hard).count() >= 40);
    }

    #[test]
    fn country_scam_database_merges_self_matches_and_protects_normal_calls() {
        let mut parsed = parse(builtin_db_json()).expect("data/scam_db.json");
        merge_countries(&mut parsed, builtin_countries_json()).expect("data/country_scams.json");
        let expected_isos = ["tw", "cn", "us", "gb", "jp", "kr", "de", "es", "fr", "br", "pt", "id", "it", "in"];
        for iso in expected_isos {
            assert!(parsed.countries.contains_key(iso), "missing country profile: {iso}");
        }
        // Every single country keyword must match itself in keyword_hits, and context keywords alone stay weight 1.
        for (iso, cp) in &db().countries {
            for sc in &cp.scams {
                for ctx_kw in &sc.context_keywords {
                    let hits = keyword_hits(ctx_kw);
                    assert!(!hits.is_empty(), "{iso}/{} context_keyword {:?} failed to self-match", sc.id, ctx_kw);
                }
                for rf in &sc.red_flag_keywords {
                    let hits = keyword_hits(&rf.text);
                    assert!(
                        hits.iter().any(|h| h.weight >= rf.weight && (!rf.hard || h.hard)),
                        "{iso}/{} red_flag {:?} failed to match with expected strength: {hits:?}",
                        sc.id,
                        rf.text
                    );
                }
            }
        }
        // Anti-stalling giveaways in bait_tactics must be rejected by merge_countries.
        let bad_json = builtin_countries_json().replace("裝得很緊張配合", "表示要掛斷後打165查證");
        assert!(merge_countries(&mut parse(builtin_db_json()).unwrap(), &bad_json).is_err());

        // Cross-border +62 call onto a TW SIM surfaces both ID cross-border warning and TW regional patterns.
        let notes = vec!["Phone SIM region: TW".to_string()];
        let brief = country_review_brief("+628123456789", &notes, &["視訊筆錄".to_string()], "對方：這裡是戶政事務所，請加LINE做視訊筆錄");
        assert!(brief.contains("Cross-border note (ID->TW)"), "{brief}");
        assert!(brief.contains("[TW -> fake_official]"), "{brief}");
        assert!(brief.chars().count() <= COUNTRY_BRIEF_MAX_CHARS, "{}", brief.chars().count());

        // Localized Stall Mode bait hooks resolve cleanly without police/hang-up giveaways.
        let tw_bait = country_bait_addon("+886912345678", &notes, "fake_official", &["視訊筆錄"], "").unwrap();
        assert!(tw_bait.contains("台灣") && !tw_bait.contains("165"), "{tw_bait}");
        let br_bait = country_bait_addon("+5511987654321", &[], "fake_bank_account_alert", &["chave pix"], "").unwrap();
        assert!(br_bait.contains("Brazil") && br_bait.contains("Wi-Fi"), "{br_bait}");
        let de_bait = country_bait_addon("+4930123456", &[], "prize_lottery", &["gewinnspiel abo"], "").unwrap();
        assert!(de_bait.contains("Germany") && de_bait.contains("Notar"), "{de_bait}");
    }

    #[test]
    fn keyword_index_matches_full_scan_and_runs_in_microseconds() {
        let base = db();
        let samples = [
            "您好，這裡是戶政事務所，您的證件遭冒用，請加LINE做視訊筆錄並匯入監管專戶",
            "Hi, calling from Medicare Advantage to verify your Medicare number for a new chip Medicare card",
            "HMRC arrest warrant: your National Insurance number has been compromised",
            "県警捜査二課です。LINEで逮捕状を送りますのでキャッシュカードを預かりに伺います",
            "서울중앙지검입니다. 본인 명의로 대포통장이 개설되어 안전계좌로 이체하셔야 합니다",
            "Herzlichen Glückwunsch zum Gewinnspiel, bitte zahlen Sie die Bearbeitungsgebühr vorab beim Notar",
            "Endesa informa de un corte de suministro hoy si no facilita su código CUPS",
            "Ici Enedis compteur Linky, risque de coupure immédiate sans virement vers un compte sécurisé",
            "Central de segurança: pix por engano devolva ou transferir via pix para conta segura",
            "Estou interessado no artigo à venda, vamos associar número no MB WAY com código de levantamento MB WAY",
            "Ini dari Otoritas Jasa Keuangan Coretax, segera pindahkan dana ke rekening aman",
            "您好，這裡是台水客服提醒您本期水費月底前到超商繳納即可，祝您順心掰掰",
        ];
        // 1. Exact equivalence with linear scan on every sample + every keyword in db().
        for s in samples {
            let p = Prepared::of(s);
            let brute: Vec<Hit> = base.keywords.iter().filter(|k| k.found_in(&p)).map(hit_of).collect();
            let fast = keyword_hits_in(s, &LocalDb::default());
            assert_eq!(fast, brute, "mismatch on sample: {s}");
        }
        for kw in &base.keywords {
            let p = Prepared::of(&kw.text);
            let brute: Vec<Hit> = base.keywords.iter().filter(|k| k.found_in(&p)).map(hit_of).collect();
            let fast = keyword_hits_in(&kw.text, &LocalDb::default());
            assert_eq!(fast, brute, "mismatch on keyword: {}", kw.text);
        }
        // 1b. The owner's keywords go through their own index: same hits, same
        // order as a full scan of the built-in list and then the owner's.
        let local = parse_local(
            r#"{"keywords":[
                {"text":"紫色長頸鹿退款","weight":2,"typology":"fake_shop_order_error","lang":"zh-TW"},
                {"text":"鵺","typology":"fake_shop_order_error","lang":"zh-TW"},
                {"text":"qx7","typology":"fake_shop_order_error","lang":"zh-TW"},
                {"text":"purple giraffe refund","weight":2,"typology":"fake_shop_order_error","lang":"en"},
                {"text":"q","typology":"fake_shop_order_error","lang":"en"},
                {"text":"jirafa morada","typology":"fake_shop_order_error","lang":"es"},
                {"text":"むらさききりん","typology":"fake_shop_order_error","lang":"ja"},
                {"text":"보라색기린","typology":"fake_shop_order_error","lang":"ko"},
                {"text":"ยีราฟ สีม่วง","typology":"fake_shop_order_error","lang":"th"}
            ]}"#,
        )
        .unwrap();
        assert!(local.skipped.is_empty(), "{:?}", local.skipped);
        assert_eq!(local.keywords.len(), 9);
        let full_scan = |s: &str| -> Vec<Hit> {
            let p = Prepared::of(s);
            let mut out: Vec<Hit> = base.keywords.iter().filter(|k| k.found_in(&p)).map(hit_of).collect();
            for &k in &local.keywords {
                if k.found_in(&p) && !out.iter().any(|h| h.text == k.text) {
                    out.push(hit_of(k));
                }
            }
            out
        };
        let mut owner_samples: Vec<String> = samples.iter().map(|s| s.to_string()).collect();
        owner_samples.push("Quick question: the purple giraffe refund, qx7 and 紫色長頸鹿退款 鵺 むらさききりん 보라색기린 ยีราฟสีม่วง jirafa morada".into());
        for k in &local.keywords {
            owner_samples.push(k.text.clone());
            owner_samples.push(format!("… {} …", k.text));
        }
        for s in &owner_samples {
            assert_eq!(keyword_hits_in(s, &local), full_scan(s), "owner keyword mismatch on sample: {s}");
        }
        for k in &local.keywords {
            let sample = format!("… {} …", k.text);
            assert!(
                keyword_hits_in(&sample, &local).iter().any(|h| h.text == k.text),
                "owner keyword {:?} not found in {sample:?}",
                k.text
            );
        }
        // 2. Latency check: 1,000 full utterances in an unoptimized debug build. A normal machine
        // takes well under 250ms; shared CI machines have taken up to ~700ms. The limit only has to
        // catch the index being dropped for a much slower scan, so it leaves room for slow machines.
        let _ = db();
        let t0 = Instant::now();
        let mut total_hits = 0usize;
        for i in 0..1000 {
            total_hits += keyword_hits_in(samples[i % samples.len()], &LocalDb::default()).len();
        }
        let elapsed = t0.elapsed();
        assert!(total_hits > 1000);
        assert!(
            elapsed < Duration::from_millis(2_000),
            "1000 keyword scans took {:?} (expected <2s in debug)",
            elapsed
        );
    }

    #[test]
    fn twelve_countries_2026_scams_vs_polite_lookalikes_and_latency_budget() {
        // (iso, caller_num, sim_iso, scam_utterance, polite_lookalike_utterance)
        let matrix = [
            (
                "TW",
                "+886223456789",
                "TW",
                "您好，我是7-11賣貨便客服，買家結帳顯示帳戶凍結，請加LINE專員協助綁定台灣Pay驗證並簽署安全協定",
                "您好，這裡是7-11門市，提醒您賣貨便包裹已經到店了，這週五前帶證件來領取就可以囉",
            ),
            (
                "CN",
                "+8613800138000",
                "CN",
                "您好，微信支付客服通知您名下百万医疗保险今晚自动扣费八百元，请下载云会议开启屏幕共享协助关闭百万医疗保险",
                "您好，顺丰速运派件员，您的快递放丰巢柜了，取件码发您短信里了，谢谢",
            ),
            (
                "US",
                "+12125550199",
                "US",
                "This is E-ZPass toll enforcement: you have an unpaid toll balance and face license suspension unless you pay immediately",
                "Hi, calling from Dr. Miller's dental office to confirm your cleaning appointment tomorrow at 2 PM",
            ),
            (
                "GB",
                "+442079460123",
                "GB",
                "This is Santander fraud department: your account is compromised, move your money to a safe account now",
                "Hello, calling from Santander branch to let you know the replacement debit card you ordered is ready to collect",
            ),
            (
                "JP",
                "+81312345678",
                "JP",
                "警視庁捜査二課です。あなたの口座が犯罪に悪用されており、警察庁のセキュリティアプリを入れて資産調査のため金地金を購入してください",
                "東京電力カスタマーセンターです。来週月曜日の午前中に計器交換作業でお伺いするご案内のお電話です",
            ),
            (
                "KR",
                "+82212345678",
                "KR",
                "서울중앙지검입니다. 본인 명의로 신용카드가 발급되어 대포통장 사건에 연루되었으니 보안 점검 앱 설치 후 안전계좌로 이체하세요",
                "안녕하세요, 서울치과의원입니다. 내일 오후 세 시 정기검진 예약 확인차 연락드렸습니다",
            ),
            (
                "DE",
                "+49301234567",
                "DE",
                "Kriminalpolizei hier: nach einem Einbruch in Ihrer Nähe steht Ihr Name auf einer Liste, bitte Bargeld oder Gold übergeben zur Sicherung",
                "Guten Tag, Stadtwerke München hier, wir möchten nur kurz Ihren Termin zum Zählerwechsel nächste Woche bestätigen",
            ),
            (
                "ES",
                "+34911234567",
                "ES",
                "Le llamamos de Endesa: hay un corte de suministro hoy mismo si no paga ahora y nos da su código CUPS para cambiar de comercializadora",
                "Hola, le llamamos del taller mecánico para avisarle que la revisión de su coche ya está terminada",
            ),
            (
                "FR",
                "+33142685500",
                "FR",
                "Service anti-fraude de votre banque : opération suspecte en cours, veuillez valider la notification pour annuler et transférer vers un compte sécurisé",
                "Bonjour, c'est le livreur Chronopost, j'arrive devant votre immeuble dans cinq minutes avec votre colis",
            ),
            (
                "BR",
                "+5511987654321",
                "BR",
                "Oi, fiz um Pix por engano devolva agora por favor na chave Pix que vou te mandar senão vou abrir boletim",
                "Olá, aqui é da Sabesp apenas avisando que amanhã haverá manutenção programada de água no seu bairro pela manhã",
            ),
            (
                "PT",
                "+351211234567",
                "PT",
                "Vi o seu anúncio no OLX, para pagar já vamos associar número no MB WAY e diga-me o código de levantamento MB WAY",
                "Bom dia, falo da clínica médica para confirmar a sua consulta de rotina amanhã às dez horas",
            ),
            (
                "ID",
                "+628123456789",
                "TW",
                "Kami dari Direktorat Jenderal Pajak Coretax, segera lakukan sinkronisasi NIK NPWP dan bagikan layar WhatsApp ke rekening aman",
                "Halo, saya kurir paket mau konfirmasi apakah ada orang di rumah untuk terima paket siang ini",
            ),
        ];

        for (iso, num, sim, scam, polite) in matrix {
            let notes = vec![format!("Phone SIM region: {sim}")];
            let scam_hits = keyword_hits(scam);
            let scam_score: u32 = scam_hits.iter().map(|h| h.weight).sum();
            assert!(
                has_hard_hit(scam) || scam_score >= 3,
                "{iso}: 2026 scam must trigger immediate review/stall: score={scam_score} hits={scam_hits:?}"
            );
            let hit_words: Vec<String> = scam_hits.iter().map(|h| h.text.to_string()).collect();
            let brief = country_review_brief(num, &notes, &hit_words, scam);
            assert!(
                brief.contains(&format!("[{iso} ->")),
                "{iso}: country_review_brief should surface {iso} pattern, got: {brief}"
            );
            assert!(brief.contains("Ordinary:"), "{iso}: brief must include Ordinary look-alike guard: {brief}");
            assert!(brief.chars().count() <= COUNTRY_BRIEF_MAX_CHARS, "{iso}: brief exceeded budget: {}", brief.chars().count());

            let hit_refs: Vec<&str> = hit_words.iter().map(|s| s.as_str()).collect();
            let top_typology = typology_for_hits(&hit_refs).expect("scam typology");
            let bait = country_bait_addon(num, &notes, top_typology, &hit_refs, scam)
                .unwrap_or_else(|| panic!("{iso}: expected country_bait_addon for {top_typology}"));
            assert!(!bait.trim().is_empty(), "{iso}: empty bait");

            // Polite look-alike must NOT hit hard scam words or debt/harassment typologies.
            assert!(!has_hard_hit(polite), "{iso}: polite call falsely hit hard word: {polite}");
            let polite_hits = keyword_hits(polite);
            let polite_score: u32 = polite_hits.iter().map(|h| h.weight).sum();
            assert!(
                polite_score <= 2,
                "{iso}: polite look-alike scored too high ({polite_score}): {polite_hits:?}"
            );
        }

        // Microsecond benchmark across 5,000 calls to prove zero impact on live call audio/turn latency.
        let _ = db();
        let iters = 5_000usize;
        let t_kw = Instant::now();
        let mut sink = 0usize;
        for i in 0..iters {
            let (_, _, _, scam, polite) = matrix[i % matrix.len()];
            sink += keyword_hits(if i % 2 == 0 { scam } else { polite }).len();
        }
        let kw_us_per_call = t_kw.elapsed().as_secs_f64() * 1_000_000.0 / (iters as f64);

        let notes = vec!["Phone SIM region: TW".to_string()];
        let sample_hits = vec!["賣貨便".to_string(), "綁定台灣pay".to_string()];
        let t_brief = Instant::now();
        for _ in 0..1_000 {
            sink += country_review_brief("+628123456789", &notes, &sample_hits, matrix[0].3).len();
        }
        let brief_us_per_call = t_brief.elapsed().as_secs_f64() * 1_000_000.0 / 1_000.0;
        assert!(sink > 0);
        println!(
            "[latency_bench] total_keywords={} | keyword_hits={:.2} us/utterance | country_review_brief={:.2} us/review",
            db().keywords.len(),
            kw_us_per_call,
            brief_us_per_call
        );
    }

    #[test]
    fn a_broken_database_is_an_error_not_a_panic() {
        assert!(parse("{").is_err());
        assert!(parse("[]").is_err());
        let base: Value = serde_json::from_str(builtin_db_json()).unwrap();
        let broken = |f: &dyn Fn(&mut Value)| {
            let mut v = base.clone();
            f(&mut v);
            parse(&v.to_string())
        };
        let e = broken(&|v| v["keywords"][0]["words"][0][2] = "no_such_type".into()).unwrap_err();
        assert!(e.contains("no_such_type"), "{e}");
        let e = broken(&|v| v["keywords"][2]["words"][0][1] = 3.into()).unwrap_err();
        assert!(e.contains("1 or 2"), "{e}");
        let e = broken(&|v| {
            let first = v["keywords"][2]["words"][0].clone();
            v["keywords"][2]["words"].as_array_mut().unwrap().push(first);
        })
        .unwrap_err();
        assert!(e.contains("twice"), "{e}");
        let e = broken(&|v| {
            v["typologies"][0]["names"].as_object_mut().unwrap().remove("ko");
        })
        .unwrap_err();
        assert!(e.contains("ko"), "{e}");
        let e = broken(&|v| v["keywords"][2]["words"][0] = serde_json::json!(["Loan", 2, "loan_sales"])).unwrap_err();
        assert!(e.contains("lowercase"), "{e}");
        let e = broken(&|v| v["typologies"][0]["category"] = "maybe".into()).unwrap_err();
        assert!(e.contains("category"), "{e}");
        // A wrong built-in database leaves the agent running without keywords.
        assert!(ScamDb::default().keywords.is_empty());
    }

    /// The lists moved here from locale.rs match as they did there.
    #[test]
    fn keywords_match_whole_word_starts_like_the_old_lists() {
        let hits = keyword_hits("Hi, I'm calling about your car's extended warranty and a low interest loan");
        let kws: Vec<&str> = hits.iter().map(|h| h.text).collect();
        assert!(kws.contains(&"warranty") && kws.contains(&"loan"), "{kws:?}");
        // Only the start of a word counts: "bank" does not hit "embankment".
        assert!(keyword_hits("the embankment by the river").is_empty());
        assert!(!keyword_hits("保険のご案内です").is_empty());
        assert_eq!(keyword("warranty").map(|h| h.weight), Some(2));
        assert_eq!(keyword("nonsense"), None);
        // Chinese: Simplified speech, 賬 for 帳, ASCII words whole.
        let zh: Vec<&str> = keyword_hits("您的账户已被冻结，请去ATM").iter().map(|h| h.text).collect();
        assert!(zh.contains(&"帳戶") && zh.contains(&"凍結") && zh.contains(&"atm"), "{zh:?}");
        assert!(keyword_hits("we help teams streamline things online").iter().all(|h| h.text != "line"));
    }

    #[test]
    fn simplified_chinese_keywords_match_in_either_script() {
        for text in ["他说要注销校园贷，不然影响征信", "他說要註銷校園貸，不然影響徵信"] {
            let hits = keyword_hits(text);
            assert!(hits.iter().any(|h| h.typology == "credit_record_scam" && h.weight == 2), "{text}: {hits:?}");
        }
    }

    #[test]
    fn thai_keywords_match_across_the_spaces_speech_to_text_puts_in() {
        let hits = keyword_hits("พัสดุ ของ คุณ มี สิ่ง ผิดกฎหมาย พัสดุผิด กฎหมาย");
        assert!(hits.iter().any(|h| h.text == "พัสดุผิดกฎหมาย"), "{hits:?}");
        // A courier's ordinary words are no keyword.
        assert!(keyword_hits("มี พัสดุ มา ส่ง ค่ะ บ่าย นี้ อยู่ บ้าน ไหม คะ").is_empty());
    }

    #[test]
    fn hard_requests_are_marked_in_every_script() {
        for t in ["驗證碼", "atm", "verification code", "認証コード", "인증번호", "contraseña", "senha", "mã otp", "kode otp", "код из смс", "ओटीपी", "رمز التحقق", "รหัสotp", "doğrulama kodu"] {
            assert!(is_hard(t), "{t}");
        }
        for t in ["銀行", "保險", "loan", "bank", "nonsense"] {
            assert!(!is_hard(t), "{t}");
        }
    }

    #[test]
    fn the_kind_of_call_follows_the_heaviest_keywords() {
        assert_eq!(typology_for_hits(&["銀行", "atm", "分期"]), Some("fake_bank_installment"));
        assert_eq!(typology_for_hits(&["保險", "保單健檢", "方案"]), Some("insurance_sales"));
        assert_eq!(typology_for_hits(&[]), None);
        assert_eq!(typology("fake_official").unwrap().name(Lang::ZhTw), "假冒檢警要求監管帳戶");
        assert_eq!(typology("fake_official").unwrap().category, Category::Scam);
        assert_eq!(typology("insurance_sales").unwrap().category, Category::Sales);
    }

    #[test]
    fn own_matter_phrases_cover_every_language() {
        for t in [
            "回覆您反映的信用卡分期問題",
            "关于您的申诉",
            "following up on your ticket",
            "お問い合わせの件でお電話しました",
            "Llamo por su reclamación",
            "Rappel suite à votre appel au sujet de votre signalement",
            "come da sua richiesta la richiamo per la sua pratica",
            "Saya menelepon soal keluhan anda",
            "Звоню по вашему обращению",
            "şikayetiniz hakkında arıyorum",
            "आपकी शिकायत के बारे में",
            "بخصوص شكواك",
            "โทรมาเรื่องที่แจ้งไว้ค่ะ",
        ] {
            assert!(own_matter(t), "{t}");
        }
        assert!(!own_matter("There is a problem with your order"));
        assert!(!own_matter("他反應很快"));
    }

    /// Russian, Hindi, Arabic, Japanese and Korean: a reply about a complaint or
    /// repair the owner raised is their own matter; "about your request /
    /// application / inquiry" alone is how sales and scam calls open too.
    #[test]
    fn follow_ups_in_russian_hindi_arabic_japanese_and_korean_are_the_owners_own_matter() {
        // One strong phrase, or two different weak ones.
        let cases: [(&str, [&str; 3], &str, &str); 5] = [
            (
                "ru",
                [
                    "Звоним сообщить, что мы рассмотрели вашу жалобу на шум, мастер придёт в пятницу",
                    "Перезваниваю по вашей жалобе на протечку в ванной",
                    "По вашей заявке на ремонт: ваше обращение рассмотрено, мастер приедет завтра",
                ],
                "По вашему запросу назовите код из смс и перейдите на безопасный счёт",
                "Предлагаем вам выгодный кредит на особых условиях",
            ),
            (
                "hi",
                [
                    "आपने जो समस्या बताई थी, उसका समाधान हो गया है",
                    "आपकी शिकायत पर इंजीनियर कल आपके घर आएगा",
                    "आपके आवेदन के बारे में और आपकी पूछताछ का जवाब देने के लिए फोन किया है",
                ],
                "आपके आवेदन के बारे में ओटीपी कोड बताइए",
                "हमारी कंपनी में निवेश करें और ज्यादा मुनाफा कमाएं",
            ),
            (
                "ar",
                [
                    "بخصوص المشكلة التي أبلغت عنها، تم إصلاح العطل",
                    "الشكوى التي قدمتها الأسبوع الماضي تم حلها",
                    "أتصل بخصوص طلبك واستفسارك الأسبوع الماضي، انتهى الإصلاح",
                ],
                "بخصوص طلبك أرسل لي رمز التحقق",
                "لدينا عرض خاص لقرض بدون فوائد",
            ),
            (
                "ja",
                [
                    "ご依頼の件でお見積もりができました",
                    "修理ご依頼の件でご連絡いたしました",
                    "お申し込みの件と、ご相談の件について合わせてご連絡いたしました",
                ],
                "お申し込みの件ですが、認証コードと暗証番号を教えてください",
                "新しい投資プランのご案内でお電話しました",
            ),
            (
                "ko",
                [
                    "접수하신 민원 처리 결과를 알려드립니다",
                    "요청하신 인터넷 설치 일정 안내드립니다",
                    "신청하신 건과 문의해 주신 내용 함께 안내드립니다",
                ],
                "신청하신 건 승인을 위해 문자로 간 인증번호와 비밀번호를 말씀해 주세요",
                "민원 처리 결과를 알려드립니다",
            ),
        ];
        for (lang, genuine, spoofed, other) in cases {
            for t in genuine {
                assert!(!has_hard_hit(t), "{lang}: a genuine follow-up has no scam-only demand: {t}");
                assert!(own_matter(t), "{lang}: {t}");
            }
            assert!(has_hard_hit(spoofed), "{lang}: {spoofed}");
            assert!(!own_matter(spoofed), "{lang}: a scam-only demand cancels it: {spoofed}");
            assert!(!own_matter(other), "{lang}: not a follow-up: {other}");
        }
        // Sales and scam openers that start with one weak phrase.
        for t in [
            "ご相談の件で、新しい投資プランをご紹介します。元本保証で年利二十パーセントです",
            "お問合せの件で、保険の見直しのご案内です",
            "お申込みの件で、融資の保証金として先に五万円お振込みください",
            "お申し込みの件ですが、未納料金が発生しています",
            "문의해 주신 투자 상품 안내드립니다. 수익률 삼십 퍼센트 보장입니다",
            "접수해 주신 건과 관련해 검찰청입니다. 계좌가 범죄에 연루되었습니다",
            "남겨 주신 문의 확인해서 연락드렸습니다. 보험 가입하시면 혜택이 있습니다",
            "신청하신 건 대출 승인되었습니다. 수수료를 먼저 입금해 주세요",
            "आपकी समस्या के बारे में, हम आपको नया क्रेडिट कार्ड ऑफर देते हैं",
            "आपकी पूछताछ के लिए धन्यवाद, हमारी बीमा पॉलिसी में निवेश करें",
            "आपके अनुरोध के बारे में निवेश योजना बताता हूँ",
            "أنا من الشرطة، بخصوص طلبك، حسابك متورط في غسيل أموال",
            "بخصوص طلبك فزت بجائزة كبرى، ادفع رسوم التحويل لاستلامها",
            "بخصوص طلبك، تمت الموافقة على قرض بدون فوائد",
            "Это служба безопасности банка, по вашему запросу на кредит зафиксирована подозрительная операция по карте",
            "Звоню по вашему запросу на кредит, мы одобрили вам сумму",
            "Добрый день, по вашему запросу в интернете мы звоним предложить натяжные потолки",
            "Ваше обращение рассмотрено, вам положена выплата, оплатите комиссию",
        ] {
            assert!(!own_matter(t), "one weak phrase is no follow-up: {t}");
        }
        // English, Spanish, French and Portuguese read as before.
        for t in [
            "Hello, about your claim, returning your call",
            "Following up on your ticket",
            "Llamo por su reclamación de la semana pasada",
            "Rappel suite à votre appel au sujet de votre signalement",
            "Retornando sua ligação sobre sua solicitação de reparo do aparelho",
        ] {
            assert!(own_matter(t), "{t}");
        }
        for t in [
            "We are calling about your claim",
            "Nous appelons au sujet de votre dossier CPF",
            "Estou ligando sobre seu pedido",
            "Le llamamos para ofrecerle un seguro de vida",
        ] {
            assert!(!own_matter(t), "{t}");
        }
    }

    #[test]
    fn the_owners_file_adds_keywords_and_numbers_and_skips_what_it_cannot_place() {
        let local = parse_local(
            r#"{"keywords": [
                {"text": "幫你處理退費", "weight": 2, "typology": "fake_shop_order_error", "lang": "zh-TW"},
                {"text": "Wellness Club", "typology": "health_beauty_sales", "lang": "en"},
                {"text": "保險", "weight": 2, "typology": "insurance_sales", "lang": "zh-TW"},
                {"text": "x", "weight": 5, "typology": "insurance_sales", "lang": "en"},
                {"text": "y", "typology": "no_such", "lang": "en"},
                {"text": "z", "typology": "insurance_sales", "lang": "xx"}
              ],
              "numbers": [
                {"number": "0912345678", "verdict": "scam", "note": "假檢察官"},
                {"number": "0223456789", "verdict": "safe"},
                {"number": "12", "verdict": "scam"},
                {"number": "0912000000", "verdict": "maybe"}
              ]}"#,
        )
        .unwrap();
        assert_eq!(local.keywords.len(), 2, "{:?}", local.skipped);
        assert_eq!(local.numbers.len(), 2, "{:?}", local.skipped);
        assert_eq!(local.skipped.len(), 6, "{:?}", local.skipped);
        let hits = keyword_hits_in("我是客服，要幫你處理退費", &local);
        assert!(hits.iter().any(|h| h.text == "幫你處理退費" && h.weight == 2), "{hits:?}");
        let hits = keyword_hits_in("Hi, this is the wellness club", &local);
        assert!(hits.iter().any(|h| h.text == "wellness club" && h.weight == 1), "{hits:?}");
        // Read again: the same entries are not stored twice.
        let again = parse_local(r#"{"keywords":[{"text":"幫你處理退費","weight":2,"typology":"fake_shop_order_error","lang":"zh-TW"}]}"#).unwrap();
        assert!(std::ptr::eq(again.keywords[0], local.keywords[0]));
        assert!(parse_local("{oops").is_err());
        assert!(parse_local("{}").unwrap().keywords.is_empty());
        // The owner's list marks numbers.
        let book = NumberBook::open(None);
        let f = caller_facts_in(&book, &local.numbers, "+886912345678", Some("tw"));
        assert!(f.flagged_before && !f.owner_safe, "{f:?}");
        assert!(f.notes.iter().any(|n| n.contains("own list") && n.contains("假檢察官")), "{f:?}");
        assert!(caller_facts_in(&book, &local.numbers, "02-2345-6789", Some("tw")).owner_safe);
    }

    #[test]
    fn the_number_book_keeps_owner_marks_over_automatic_ones_and_survives_a_restart() {
        let dir = temp_dir("book");
        let book = NumberBook::open(Some(dir.clone()));
        let key = number_key("0912345678", Some("tw")).unwrap();
        assert_eq!(key, "+886912345678");
        assert_eq!(number_key("+886 912-345-678", None).as_deref(), Some("+886912345678"));
        let a = book.record_auto(&key, "scam", "fake_official").unwrap();
        assert_eq!((a.source.as_str(), a.count), ("auto", 1));
        let a = book.record_auto(&key, "sales", "").unwrap();
        assert_eq!((a.verdict.as_str(), a.count, a.typology.as_str()), ("sales", 2, "fake_official"));
        let o = book.mark_owner(&key, "safe", "");
        assert_eq!((o.source.as_str(), o.verdict.as_str(), o.count), ("owner", "safe", 1));
        assert_eq!(book.record_auto(&key, "scam", "fake_official"), None, "the owner's mark stands");
        assert_eq!(book.mark_owner(&key, "safe", "").count, 2);
        book.log_feedback(&serde_json::json!({"call_id": "c1", "verdict": "safe"}));
        book.log_feedback(&serde_json::json!({"call_id": "c2", "verdict": "scam"}));
        let reopened = NumberBook::open(Some(dir.clone()));
        assert_eq!(reopened.get(&key).unwrap().verdict, "safe");
        let log = std::fs::read_to_string(dir.join("scam_feedback.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 2);
        std::fs::write(dir.join("scam_numbers.json"), "{broken").unwrap();
        assert_eq!(NumberBook::open(Some(dir.clone())).len(), 0, "a broken file starts over");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hidden_numbers_are_never_remembered() {
        for raw in ["", "  ", "unknown", "private", "Anonymous"] {
            assert_eq!(number_key(raw, Some("tw")), None, "{raw}");
            assert!(is_hidden(raw));
        }
        assert!(number_rules("", Some("tw")).iter().any(|r| r.kind == RuleKind::Hidden));
    }

    #[test]
    fn a_taiwanese_number_shown_as_international_is_a_hint() {
        let ids = |raw: &str, sim: Option<&str>| number_rules(raw, sim).iter().map(|r| r.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids("+886912345678", Some("tw")), vec!["tw_shown_as_international"]);
        assert!(ids("0912345678", Some("tw")).is_empty());
        assert_eq!(ids("+886912345678", Some("us")), vec!["foreign_number"]);
        assert!(ids("+15550100123", Some("us")).is_empty());
        assert_eq!(ids("+8613812345678", Some("tw")), vec!["foreign_number"]);
        assert_eq!(ids("+8617012345678", Some("cn")), vec!["cn_virtual_operator"]);
        assert_eq!(ids("070-1234-5678", Some("kr")), vec!["kr_internet_phone"]);
    }

    #[test]
    fn caller_facts_turn_marks_and_rules_into_notes_for_the_review() {
        let book = NumberBook::open(None);
        let key = number_key("0912345678", Some("tw")).unwrap();
        // Judged once: a note, not yet looked at sooner.
        book.record_auto(&key, "scam", "fake_bank_installment");
        let f = caller_facts_in(&book, &[], "0912345678", Some("tw"));
        assert!(!f.flagged_before && !f.owner_safe);
        assert!(f.notes[0].starts_with("Earlier calls from this number were judged scam once"), "{f:?}");
        book.record_auto(&key, "scam", "fake_bank_installment");
        let f = caller_facts_in(&book, &[], "0912345678", Some("tw"));
        assert!(f.flagged_before);
        assert!(f.notes[0].contains("2 times") && f.notes[0].contains("(fake_bank_installment)"), "{f:?}");
        book.mark_owner(&key, "safe", "");
        let f = caller_facts_in(&book, &[], "+886912345678", Some("tw"));
        assert!(f.owner_safe && !f.flagged_before, "{f:?}");
        assert!(f.notes.iter().any(|n| n.contains("marked this number as safe")));
        assert!(f.notes.iter().any(|n| n.starts_with("Number hint (tw_shown_as_international)")), "{f:?}");
        let hidden = caller_facts_in(&book, &[], "", Some("tw"));
        assert_eq!(hidden.notes, vec!["Number hint (hidden_number): The number is hidden.".to_string()]);
    }

    #[test]
    fn nine_supported_languages_catch_scams_debt_harassment_and_protect_normal_calls() {
        // (lang, scam_hard, debt_dunning, harassment, spoofed_own_matter, genuine_own_matter, polite_normal)
        let cases = [
            (
                Lang::ZhTw,
                "您好，我是賣貨便客服，您的賣場需要做金流認證並分享畫面",
                "這裡是資產管理公司委外催收，再不處理就聲請強制執行查封扣薪",
                "你欠錢不還真不要臉，明天去你公司堵你，出門小心一點",
                "回覆您反映的訂單問題，請立刻去ATM操作並把驗證碼唸給我",
                "回覆您反映的網路斷線問題，工程師已經修復好了",
                "您好，這裡是自來水公司，溫馨提醒您這期水費記得在月底前到超商繳納喔",
            ),
            (
                Lang::ZhCn,
                "您好，闲鱼买家拍下商品了，请开启屏幕共享完成实名认证",
                "我们是催收公司，再不处理就上门催收并把你列入失信被执行人限高令",
                "你这个傻逼，明天去你家堵你，让你好看",
                "关于您的申诉已经受理，请提供短信验证码并转账到安全账户",
                "关于您的申诉我们已经核实完毕，特此回电说明结果",
                "您好，物业中心提醒您本月的物业费和水费账单已经发到信箱了",
            ),
            (
                Lang::En,
                "Hi, buyer support here: please share your screen to finish escrow verification",
                "This is Apex collection agency and debt collector; pay up today or we start wage garnishment and legal action",
                "Listen asshole, we will come to your house and ruin your life, watch your back",
                "Following up on your request, please read me the verification code on your phone",
                "Following up on your request from Tuesday, your router replacement has shipped",
                "Hi, calling from City Water with a friendly reminder that your monthly water bill is due next Friday",
            ),
            (
                Lang::Ja,
                "フリマサポートです。売上金の受け取り認証のため画面共有をお願いします",
                "債権回収の担当です。本日中にお支払いがない場合、法的措置および給与の差し押さえを行います",
                "ふざけるなバカ野郎、今から家に行くぞ、会社に行くから覚悟しろ",
                "お問い合わせの件につきまして、確認のため認証コードと暗証番号を教えてください",
                "お問い合わせの件につきまして、調査結果をご報告いたします",
                "水道局からのお知らせです。今月分の水道料金のお支払い期限は来週金曜日となっております",
            ),
            (
                Lang::Ko,
                "중고거래 고객센터입니다. 안전결제 인증을 위해 지금 화면 공유를 켜주세요",
                "신용정보회사 추심업체입니다. 오늘까지 변제하지 않으면 법적 조치 및 급여압류 강제집행 들어갑니다",
                "야 이 개새끼야, 내일 당장 집으로 찾아가고 회사로 찾아가서 가만 안 둬",
                "문의하신 건 처리해 드리겠습니다. 문자로 간 인증번호와 비밀번호를 말씀해 주세요",
                "문의하신 인터넷 수리 건이 완료되어 결과 안내차 연락드렸습니다",
                "관리사무소입니다. 이번 달 수도요금 납부 기한이 다음 주 금요일까지라 안내드립니다",
            ),
            (
                Lang::Es,
                "Soporte de compraventa: necesitamos compartir pantalla para verificar su cuenta de vendedor",
                "Hablamos de la agencia de cobro y empresa de recobro; si no paga hoy iniciaremos demanda judicial y embargo",
                "Escúchame hijo de puta sinvergüenza, vamos a ir a su domicilio hoy mismo",
                "Sobre su reclamación, por favor dígame el código de verificación para desbloquear la cuenta segura",
                "Llamo sobre su reclamación de la semana pasada para informarle que ya quedó resuelta",
                "Hola, le llamamos de la clínica dental para recordar su cita de limpieza mañana a las diez",
            ),
            (
                Lang::Fr,
                "Support client : activez le partage d'écran pour valider le paiement sécurisé leboncoin",
                "Ici le cabinet de recouvrement : sans règlement immédiat, un huissier lancera une saisie sur salaire et des poursuites judiciaires",
                "Écoute connard, on sait où tu habites et on va venir chez vous ce soir",
                "Suite à votre demande, veuillez me donner le code de vérification reçu par SMS",
                "Suite à votre demande d'hier, votre dossier de réparation est terminé",
                "Bonjour, c'est le cabinet dentaire pour vous rappeler votre rendez-vous de demain à quatorze heures",
            ),
            (
                Lang::De,
                "Kundendienst hier: bitte Bildschirm teilen, um den Käuferschutz verifizieren zu können",
                "Hier ist das Inkassobüro: zahlen Sie sofort, sonst folgen Mahnbescheid, Zwangsvollstreckung und Lohnpfändung durch den Gerichtsvollzieher",
                "Hör zu du Arschloch, wir wissen wo du wohnst und kommen bei Ihnen vorbei, ich mach dich fertig",
                "Bezüglich Ihrer Anfrage nennen Sie mir bitte sofort den Bestätigungscode und Ihr Passwort",
                "Bezüglich Ihrer Anfrage von gestern und Ihres Anliegens wollte ich Ihnen kurz mitteilen, dass das Teil da ist",
                "Guten Tag, hier ist die Zahnarztpraxis zur Erinnerung an Ihren Termin morgen um zehn Uhr",
            ),
            (
                Lang::Pt,
                "Suporte da plataforma: precisamos compartilhar a tela para fazer a liberação do pagamento",
                "Aqui é da assessoria de cobrança e recuperação de crédito; sem pagamento hoje haverá bloqueio judicial, penhora e protesto em cartório",
                "Olha aqui seu vagabundo caloteiro, sei onde você mora e vamos na sua casa hoje",
                "Sobre sua reclamação, informe agora o código de verificação e sua senha para liberar",
                "Retornando sua ligação sobre sua solicitação de reparo do aparelho, ele já está pronto para retirada",
                "Olá, aqui é da clínica odontológica apenas lembrando da sua consulta amanhã às três da tarde",
            ),
        ];
        assert_eq!(cases.len(), crate::locale::ALL.len());
        for (lang, scam, debt, harass, spoofed_own, genuine_own, normal) in cases {
            assert!(has_hard_hit(scam), "{lang:?}: scam should hit hard: {scam}");
            let debt_hits = keyword_hits(debt);
            let debt_score: u32 = debt_hits.iter().map(|h| h.weight).sum();
            assert!(
                debt_score >= 4 && debt_hits.iter().any(|h| h.typology == "debt_collection_harassment"),
                "{lang:?}: debt should hit debt_collection_harassment: score={debt_score} hits={debt_hits:?}"
            );
            let harass_hits = keyword_hits(harass);
            let harass_score: u32 = harass_hits.iter().map(|h| h.weight).sum();
            assert!(
                harass_score >= 4 && harass_hits.iter().any(|h| h.typology == "harassment_intimidation"),
                "{lang:?}: harassment should hit harassment_intimidation: score={harass_score} hits={harass_hits:?}"
            );
            assert!(!own_matter(spoofed_own), "{lang:?}: spoofed own_matter must be overridden by hard hit");
            assert!(has_hard_hit(spoofed_own), "{lang:?}: spoofed own_matter has hard hit");
            assert!(own_matter(genuine_own), "{lang:?}: genuine own_matter must remain protected");
            assert!(!has_hard_hit(normal), "{lang:?}: normal call must have no hard hit");
            let normal_hits = keyword_hits(normal);
            assert!(
                !normal_hits.iter().any(|h| h.typology == "debt_collection_harassment" || h.typology == "harassment_intimidation"),
                "{lang:?}: polite normal call must not hit debt/harassment: {normal_hits:?}"
            );
        }
        // Taiwan STT homophones for marketplace scams normalize cleanly.
        assert!(keyword_hits("我是賣貨變客服，還有好賣加的訂單").iter().filter(|h| h.typology == "marketplace_escrow_scam").count() >= 2);
    }

    #[test]
    fn indexed_keyword_hits_match_linear_scan_in_exact_order() {
        let base = db();
        for kw in &base.keywords {
            let sample = format!("… {} …", kw.text);
            let p = Prepared::of(&sample);
            let linear: Vec<Hit> = base.keywords.iter().filter(|k| k.found_in(&p)).map(hit_of).collect();
            let indexed = keyword_hits_in(&sample, &LocalDb::default());
            assert_eq!(indexed, linear, "keyword sample mismatch for {:?}", kw.text);
        }
    }

    fn hit_texts(text: &str) -> Vec<&'static str> {
        keyword_hits(text).into_iter().map(|h| h.text).collect()
    }

    #[test]
    fn a_short_keyword_does_not_match_the_start_of_a_longer_word() {
        // Portuguese "pix", "conta" and English "demo" are inside Pixel, contacts, demonstrate.
        let said = "calling from your contacts to demonstrate the Pixel";
        let hits = hit_texts(said);
        for kw in ["pix", "conta", "demo"] {
            assert!(!hits.contains(&kw), "{kw} in {said:?}: {hits:?}");
        }
        let weight: u32 = keyword_hits(said).iter().map(|h| h.weight).sum();
        assert!(weight < 5, "{said:?} must not reach the scam threshold: {hits:?}");
        // The word itself, with an ordinary ending, still counts.
        assert!(hit_texts("we can book a demo for you").contains(&"demo"));
        assert!(hit_texts("two demos this week").contains(&"demo"));
        assert!(hit_texts("faz um pix agora").contains(&"pix"));
        assert!(hit_texts("minhas contas").contains(&"conta"));
        assert!(hit_texts("we have loans and refinancing").contains(&"loan"));
        assert!(hit_texts("we have loans and refinancing").contains(&"refinanc"));
    }

    #[test]
    fn a_japanese_sentence_is_not_read_with_the_chinese_keywords() {
        let said = "保険証を持って銀行の前で待っています";
        for h in keyword_hits(said) {
            let lang = &db().keyword(h.text).map(|k| k.lang.clone()).unwrap_or_default();
            assert!(!lang.starts_with("zh"), "{} ({lang}) read in a Japanese sentence", h.text);
        }
        // Chinese still reads them.
        let zh = hit_texts("請帶保險證到銀行");
        assert!(zh.contains(&"保險") && zh.contains(&"銀行"), "{zh:?}");
        // A Chinese call with a few Japanese words in it keeps its Chinese reading.
        let mixed = hit_texts("我們是保險公司，請問有興趣了解保險規劃嗎，ありがとう");
        assert!(mixed.contains(&"保險"), "{mixed:?}");
    }

    #[test]
    fn accents_and_korean_spaces_do_not_hide_a_keyword() {
        // "código de verificación", "prêt", "empréstimo", "mã otp", "đầu tư", spoken or written bare.
        assert!(hit_texts("dame el código de verificación").contains(&"código de verificación"));
        assert!(hit_texts("dame el codigo de verificacion").contains(&"código de verificación"));
        assert!(hit_texts("dame el co\u{301}digo de verificacio\u{301}n").contains(&"código de verificación"));
        assert!(hit_texts("je vous propose un pret rapide").contains(&"prêt"));
        assert!(hit_texts("je vous propose un pre\u{302}t rapide").contains(&"prêt"));
        assert!(hit_texts("um emprestimo sem consulta").contains(&"empréstimo"));
        assert!(hit_texts("cho tôi mã OTP").contains(&"mã otp"));
        assert!(hit_texts("cho toi ma otp").contains(&"mã otp"));
        assert!(hit_texts("cơ hội dau tu tốt").contains(&"đầu tư"));
        // The folded short word still ends where the word ends.
        assert!(!hit_texts("it is pretty good, we pretend nothing").contains(&"prêt"));
        // Korean: a keyword with a space matches the text with or without spaces.
        assert!(hit_texts("카드 배송 일정 안내").contains(&"카드 배송"));
        assert!(hit_texts("카드배송 일정 안내").contains(&"카드 배송"));
        assert!(hit_texts("카 드 배 송 일정").contains(&"카드 배송"));
        assert!(hit_texts("화면공유 앱을 설치하세요").contains(&"화면 공유"));
    }

    #[test]
    fn an_opener_that_scams_use_does_not_make_the_whole_call_the_owners_own_matter() {
        for t in [
            "折り返しお電話しました",
            "민원 처리 결과를 알려드립니다",
            "Nous appelons au sujet de votre dossier CPF",
            "Estou ligando sobre seu pedido",
            "We are calling about your claim",
        ] {
            assert!(!own_matter(t), "{t}");
        }
        // Two of them, or a clearer one, still make it a follow-up.
        assert!(own_matter("Hello, about your claim, returning your call"));
        assert!(own_matter("ご連絡いただいた件で折り返しました"));
        assert!(own_matter("문의하신 민원 건입니다"));
        assert!(own_matter("Llamo por su reclamacion"));
    }

    /// "credit card" heard in English also matched the French 「crédit」 (accents
    /// folded): a loan pitch read into a card desk's words.
    #[test]
    fn an_accented_word_inside_a_longer_hit_counts_once() {
        let texts = |t: &str| keyword_hits(t).iter().map(|h| h.text.to_string()).collect::<Vec<_>>();
        let en = texts("This is the credit card risk-control center at First National Bank");
        assert!(en.iter().any(|h| h == "crédit"), "the scan itself still reports it: {en:?}");
        let kept = without_folded_repeats(&en);
        assert!(kept.iter().any(|h| h == "credit card") && !kept.iter().any(|h| h == "crédit"), "{kept:?}");
        // Alone it still counts.
        let fr = without_folded_repeats(&texts("Je vous propose un crédit à taux réduit"));
        assert!(fr.iter().any(|h| h == "crédit"), "{fr:?}");
        // The same language's longer phrase keeps both.
        let de = without_folded_repeats(&texts("bitte zahlen Sie die Bearbeitungsgebühr vorab beim Notar"));
        assert!(de.iter().any(|h| h == "bearbeitungsgebühr") && de.iter().any(|h| h == "bearbeitungsgebühr vorab"), "{de:?}");
    }

    #[test]
    fn tightened_skip_han_keeps_scam_hits_with_few_kana_and_skips_real_japanese() {
        // Two stray kana STT artifacts, or 3 kana across a longer Chinese line, must NOT suppress Han scam keywords.
        assert!(!Prepared::of("の安全帳戶す").skip_han);
        assert!(!Prepared::of("の請問您的帳戶安全嗎，需要安全帳戶轉帳です").skip_han);
        let hits = keyword_hits("の請問您的帳戶安全嗎，需要安全帳戶轉帳です");
        assert!(hits.iter().any(|h| h.text == "安全帳戶"), "{hits:?}");
        // Genuine Japanese text (own >= 3 and own * 4 >= own + han) sets skip_han.
        let jp = Prepared::of("お振込みのご確認をお願いいたします");
        assert!(jp.skip_han);
    }

    #[test]
    fn db_keys_keywords_by_lang_and_text_and_local_allows_updates() {
        let json = r#"{
            "version": 1,
            "typologies": [
                {
                    "id": "bank_impersonation",
                    "category": "scam",
                    "hard": true,
                    "names": {"zh-TW":"假冒銀行","zh-CN":"假冒银行","en":"Bank","ja":"銀行","ko":"은행","es":"Banco","fr":"Banque","de":"Bank","pt":"Banco"},
                    "signals": "Asks for account verification",
                    "legit": "Official app notification"
                },
                {
                    "id": "insurance_sales",
                    "category": "sales",
                    "hard": false,
                    "names": {"zh-TW":"推銷保險","zh-CN":"推销保险","en":"Insurance","ja":"保険","ko":"보험","es":"Seguro","fr":"Assurance","de":"Versicherung","pt":"Seguro"},
                    "signals": "Cold insurance pitch",
                    "legit": "Existing policy renewal"
                }
            ],
            "keywords": [
                {"lang": "en", "match": "word", "words": [["bank", 1, "bank_impersonation"]]},
                {"lang": "id", "match": "word", "words": [["bank", 2, "bank_impersonation", "hard"]]}
            ],
            "own_matter": [],
            "number_rules": []
        }"#;
        let db = parse(json).expect("cross-language same keyword text must parse");
        assert_eq!(db.keyword_in("en", "bank").map(|k| (k.weight, k.hard)), Some((1, false)));
        assert_eq!(db.keyword_in("id", "bank").map(|k| (k.weight, k.hard)), Some((2, true)));
        assert_eq!(db.keyword("bank").map(|k| (k.weight, k.hard)), Some((2, true)));

        // parse_local allows updating an already-listed builtin keyword (weight/typology/hard)
        // as well as cross-language same text and updating duplicate entries in local JSON.
        let local = parse_local(
            r#"{"keywords": [
                {"text": "保險", "weight": 1, "typology": "insurance_sales", "lang": "zh-TW"},
                {"text": "custom keyword", "weight": 1, "typology": "insurance_sales", "lang": "en"},
                {"text": "custom keyword", "weight": 2, "typology": "fake_official", "hard": true, "lang": "en"},
                {"text": "custom keyword", "weight": 2, "typology": "insurance_sales", "lang": "es"}
            ]}"#,
        )
        .expect("local updates");
        assert!(local.skipped.is_empty(), "{:?}", local.skipped);
        let updated = keyword_hits_in("請問是保險公司嗎", &local);
        assert_eq!(updated.iter().find(|h| h.text == "保險").map(|h| h.weight), Some(1), "{updated:?}");
        let custom_en = local.keywords.iter().find(|k| k.lang == "en" && k.text == "custom keyword").unwrap();
        assert_eq!((custom_en.weight, custom_en.typology.as_str(), custom_en.hard), (2, "fake_official", true));
        assert!(local.keywords.iter().any(|k| k.lang == "es" && k.text == "custom keyword"));
    }

    #[test]
    fn fullwidth_folding_cross_typology_conflict_soft_advance_fee_and_extended_languages_work() {
        // N-L23: fullwidth ASCII folds into plain ASCII words.
        assert_eq!(fold_accents("ＡＴＭ　ＯＴＰ"), "atm otp");
        assert!(has_hard_hit("Please tell me your ＯＴＰ right now"));

        // NL15: hard red_flag in country_scams colliding with a different typology errors.
        let mut base = parse(builtin_db_json()).unwrap();
        let conflict_json = r#"{
            "countries": {
                "XX": {
                    "name": "Test",
                    "iso": "xx",
                    "calling_code": "+99",
                    "languages": ["en"],
                    "scams": [{
                        "id": "xx_conflict",
                        "typology": "otp_theft",
                        "name": "Test conflict",
                        "name_en": "Test conflict",
                        "prevalence": "high",
                        "lang": "en",
                        "red_flag_keywords": [{"text": "anydesk", "weight": 2, "hard": true}],
                        "bait_tactics": ["Look for reading glasses"],
                        "signals_en": "Asks to install remote desktop.",
                        "legit_en": "Ordinary call."
                    }]
                }
            }
        }"#;
        let err = merge_countries(&mut base, conflict_json).unwrap_err();
        assert!(err.contains("conflicts with typology"), "{err}");

        // N1: soft advance-fee loan openers do not bypass detection, while real complaints stay protected.
        for scam_opener in [
            "您申請的信貸已通過，先付保證金美化帳戶",
            "Llamo por su solicitud de préstamo preaprobado, pague por adelantado",
            "Nous appelons au sujet de votre demande de financement, payez les frais",
        ] {
            assert!(!own_matter(scam_opener), "soft scam opener must not bypass: {scam_opener}");
        }
        for genuine in [
            "Llamo por su reclamación de la semana pasada",
            "關於您反映的客訴問題已經處理完成",
        ] {
            assert!(own_matter(genuine), "real follow-up must stay protected: {genuine}");
        }

        // N2: extended official Live languages detect hard scams and protect own_matter.
        for (lang, hard_phrase, genuine) in [
            ("nl", "Maak uw geld over naar een veilige rekening", "Wij bellen terug over uw klacht van gisteren"),
            ("pl", "Proszę przelać środki na bezpieczne konto prokuratury", "Dzwonimy w sprawie pani reklamacja jest gotowa"),
            ("ro", "Transferați banii într-un cont sigur și spuneți codul", "Vă sunăm pentru reclamația dumneavoastră rezolvată"),
            ("uk", "Терміново перекажіть кошти на безпечний рахунок", "Телефонуємо щодо ваше звернення вже опрацьовано"),
            ("bn", "আপনি ডিজিটাল অ্যারেস্ট হয়েছেন এখনই সেফ অ্যাকাউন্ট পাঠান", "আপনার অভিযোগ যাচাই করে আমরা কল করছি"),
            ("mr", "तुम्ही डिजिटल अरेस्ट मध्ये आहात तुमचा ओटीपी कोड सांगा", "तुमची तक्रार सोडवण्यात आली आहे"),
            ("ta", "உங்கள் மீது டிஜிட்டல் கைது உள்ளது ஓடிபி எண் சொல்லுங்கள்", "உங்கள் புகார் குறித்து பேசுகிறோம்"),
            ("te", "మీపై డిజిటల్ అరెస్ట్ ఉంది వెంటనే ఓటీపీ కోడ్ చెప్పండి", "మీ ఫిర్యాదు పరిష్కరించబడింది"),
        ] {
            assert!(has_hard_hit(hard_phrase), "{lang}: expected hard hit on {hard_phrase}");
            assert!(own_matter(genuine), "{lang}: expected own_matter on {genuine}");
        }
    }

    /// A scam-free opener: no OTP / ATM word and no advance-fee loan pitch, so
    /// only the own-matter phrases decide.
    fn without_spoof_demand(t: &str) -> bool {
        keyword_hits(t).iter().all(|h| !h.hard && h.typology != "loan_advance_fee")
    }

    #[test]
    fn every_weak_own_matter_phrase_is_a_phrase_of_the_database() {
        for w in WEAK_OWN_MATTER {
            assert!(
                db().own_matter.iter().any(|(_, ps)| ps.iter().any(|p| p == w)),
                "not in own_matter, so it would never match: {w}"
            );
        }
    }

    /// "Regarding your request / your application / returning your call" opens
    /// loan, investment and police-impersonation calls too: one such phrase is no
    /// follow-up, and the stalling starts as usual.
    #[test]
    fn generic_openers_in_every_language_do_not_make_the_call_the_owners_own_matter() {
        let cases: [(&str, &str, bool); 28] = [
            ("en", "Regarding your request, you have been approved for a loan with no interest, pay the fee first", false),
            ("en", "Following up on your application, we can offer you a credit card with zero interest", true),
            ("en", "Returning your call about the investment plan, I can also get you a credit card with no annual fee", true),
            ("en", "Regarding your case, this is the police, your bank account is involved in money laundering", false),
            ("zh-TW", "您申請的貸款已經核准了，先繳手續費就能撥款", false),
            ("zh-TW", "您申請的信用卡已經核卡，另外我們有投資方案可以介紹", true),
            ("es", "Sobre su solicitud, le hemos aprobado un préstamo sin intereses", true),
            ("es", "Sobre su consulta, tenemos un seguro de vida con descuento y una inversión con rentabilidad garantizada", true),
            ("fr", "Suite à votre demande, nous vous proposons une offre de placement", true),
            ("fr", "Suite à votre appel, nous avons un crédit à taux zéro pour vous", true),
            ("de", "Zu Ihrer Anfrage: wir haben einen Kredit ohne Zinsen für Sie", true),
            ("de", "Wegen Ihres Anliegens: wir bieten Ihnen eine Geldanlage mit garantierter Rendite", true),
            ("pt", "Sobre sua solicitação, seu empréstimo foi aprovado com juros baixos, pague a taxa", true),
            ("pt", "Retornando sua ligação, temos um investimento com retorno garantido de vinte por cento", true),
            ("it", "Riguardo la sua richiesta, le offriamo un prestito e una carta di credito senza spese", true),
            ("it", "Come da sua richiesta, abbiamo un prestito senza interessi, paghi prima la commissione", true),
            ("id", "Menindaklanjuti permintaan anda, kami menawarkan pinjaman dengan bunga rendah, bayar biaya dulu", true),
            ("ms", "Susulan permohonan anda, kami tawarkan pelaburan dan kad kredit tanpa yuran", true),
            ("tl", "Tungkol sa request mo, may alok kaming loan na walang interes, bayaran muna ang fee", true),
            ("tr", "Talebiniz üzerine, size faizsiz bir kredi onayladık, önce ücreti ödeyin", true),
            ("vi", "Phản hồi yêu cầu của anh, chúng tôi có khoản vay không lãi suất, anh đóng phí trước", true),
            ("es", "Para devolver su llamada, le ofrecemos una inversión con rentabilidad garantizada del veinte por ciento", true),
            ("de", "Ich rufe wegen Ihrer Anfrage an: wir haben eine Geldanlage mit garantierter Rendite", true),
            ("pt", "Sobre sua solicitação, temos um seguro de vida com desconto e uma carta de crédito contemplada", true),
            ("it", "Per la sua pratica, le proponiamo di investire in un piano e una assicurazione vantaggiosa", true),
            ("fr", "Au sujet de votre demande, nous avons une offre de placement avec rendement garanti", true),
            ("zh-TW", "你申請的貸款已經通過了，我們還有投資課程可以介紹", true),
            ("en", "Returning your call, we have a special offer on life insurance with a free gift", true),
        ];
        let mut bad: Vec<String> = Vec::new();
        for (lang, t, engages) in cases {
            if !without_spoof_demand(t) {
                bad.push(format!("{lang}: a spoof demand is in it, so the phrases prove nothing: {t} {:?}", keyword_hits(t)));
            }
            if own_matter(t) {
                bad.push(format!("{lang}: one generic phrase is no follow-up: {t}"));
            }
            if engages {
                let dialogue = format!("接聽：喂？\n對方：{t}");
                if !crate::gemini_live::dialogue_is_sales(&dialogue) {
                    bad.push(format!("{lang}: stalling must still start: {t}"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// A complaint or repair the owner reported is strong alone; the generic
    /// phrases need a second, different one.
    #[test]
    fn real_follow_ups_in_every_language_stay_the_owners_own_matter() {
        for (lang, t) in [
            ("en", "I'm calling about the issue you reported with your router"),
            ("en", "Following up on your request \u{2014} returning your call from yesterday"),
            ("zh-TW", "您反映的漏水問題，我們明天派人過去"),
            ("zh-TW", "您報修的冷氣已經排定明天派人處理"),
            ("es", "Llamo por su reclamación de la semana pasada"),
            ("es", "Para devolver su llamada sobre su consulta de ayer"),
            ("fr", "Je vous appelle au sujet de votre réclamation"),
            ("fr", "Suite à votre appel, je reviens vers vous au sujet de votre demande"),
            ("de", "Ich rufe wegen Ihrer Beschwerde an"),
            ("de", "Ich rufe auf Ihre Anfrage zurück, wegen Ihres Anliegens"),
            ("pt", "Ligo sobre sua reclamação da semana passada"),
            ("pt", "Retornando sua ligação sobre sua solicitação de ontem"),
            ("it", "Chiamo per il suo reclamo di ieri"),
            ("it", "Come da sua richiesta, richiamo per la sua pratica"),
            ("id", "Saya menelepon soal keluhan anda"),
            ("id", "Terkait laporan anda minggu lalu, teknisi kami datang besok"),
            ("ms", "Saya telefon tentang aduan anda semalam"),
            ("ms", "Susulan permohonan anda dan pertanyaan anda, kami sudah semak"),
            ("tl", "Tumatawag ako tungkol sa reklamo mo kahapon"),
            ("tl", "Ang inireklamo mo ay naayos na namin"),
            ("tr", "Şikayetiniz hakkında arıyorum"),
            ("tr", "Bildiriminiz üzerine yarın teknisyen gelecek"),
            ("vi", "Tôi gọi về việc anh đã phản ánh tiếng ồn hôm qua"),
            ("vi", "Về khiếu nại của anh tuần trước, bên tôi đã xử lý xong"),
        ] {
            assert!(own_matter(t), "{lang}: {t}");
        }
    }

    /// Countries added from the regional research: two typical scam lines in the
    /// country's language trip the keywords of its own cards and engage the way a
    /// live call does (a scam-only demand, or a score of 3 with one keyword of
    /// weight 2, over the whole call); ordinary bank, courier and clinic calls
    /// there trip none of its keywords and do not engage, one by one or together.
    #[test]
    fn added_countries_catch_their_scams_and_leave_ordinary_calls_alone() {
        let engages = |hits: &[Hit]| {
            let mut seen: Vec<&str> = Vec::new();
            let (mut score, mut strong, mut hard) = (0, false, false);
            for h in hits {
                if seen.contains(&h.text) {
                    continue;
                }
                seen.push(h.text);
                score += h.weight;
                strong |= h.weight >= 2;
                hard |= h.hard;
            }
            hard || (score >= 3 && strong)
        };
        let from_country = |iso: &str, hits: &[Hit]| {
            hits.iter().any(|h| {
                db().country_kw_map
                    .get(&normalized_hit_key(h.text))
                    .is_some_and(|pairs| pairs.iter().any(|(i, _)| i == iso))
            })
        };
        let countries: &[(&str, [&str; 2], [&str; 3])] = &[
            (
                "hk",
                [
                    "你好，我係內地公安，你名下電話卡涉及跨境洗黑錢，依家要你簽保密令，配合資產清查",
                    "阿媽係我呀，聽唔出我係邊個？我撞車喺警署被扣留，急需保釋金，我叫朋友上門收現金",
                ],
                [
                    "你好，呢度係銀行，通知你上個月嘅月結單已經寄出咗，有問題可以去分行查詢",
                    "你好，順豐速運，你嘅包裹今日下晝派到你公司，麻煩有人簽收",
                    "你好，呢度係診所，提提你聽日朝早十點覆診，記得帶埋覆診卡",
                ],
            ),
            (
                "ca",
                [
                    "This is the Canada Revenue Agency. Your compromised SIN is linked to unpaid taxes and the RCMP will be sent unless you verify your SIN today",
                    "Grandpa, it's me, I was in a car accident and the public defender says I need bail money, a cash courier will come by tonight",
                ],
                [
                    "Hi, this is your bank calling to let you know your new debit card was mailed today",
                    "Hello, this is Canada Post, your parcel will arrive tomorrow between nine and noon",
                    "Hi, this is the family clinic reminding you of your appointment on Thursday at ten",
                ],
            ),
            (
                "au",
                [
                    "This is the Australian Taxation Office, there is a warrant for your arrest, you must move your funds to a federal holding account today",
                    "Hi, this is CommBank fraud protection, please read me the NetCode we just sent and we will move your savings to a safe account",
                ],
                [
                    "G'day, it's your bank, just letting you know your new card has been posted",
                    "Hi, Australia Post here, your parcel will be delivered this afternoon",
                    "Hi, it's the medical centre reminding you of your appointment tomorrow at nine",
                ],
            ),
            (
                "mx",
                [
                    "Le hablamos de prevención de fraudes de su banco, detectamos cargos no reconocidos, para protegerlo vamos a pasar su saldo a una bóveda segura",
                    "Tenemos a tu hijo, apaga tu celular y haz un depósito en Oxxo ahora mismo",
                ],
                [
                    "Buenas tardes, le hablamos de su banco para avisarle que su nueva tarjeta ya llegó a la sucursal",
                    "Hola, le hablo de la paquetería, su paquete llega mañana entre las nueve y las dos",
                    "Buenos días, le llamamos del consultorio para confirmar su cita del jueves a las diez",
                ],
            ),
            (
                "ar",
                [
                    "Te llamo del área de seguridad de Mercado Pago, hay un consumo desconocido y para frenarlo tenés que pasar el CBU y cambiar el alias",
                    "Hola tía, me dijeron en el banco que los dólares cara chica son billetes fuera de circulación, mando a alguien a buscarlos",
                ],
                [
                    "Hola, te llamo del banco para avisarte que tu tarjeta nueva ya está en la sucursal",
                    "Hola, soy del correo, mañana te llevo el paquete a la tarde",
                    "Buenos días, le llamamos del consultorio para recordarle el turno del jueves a las diez",
                ],
            ),
            (
                "at",
                [
                    "Hier ist die Sicherheitsabteilung Ihrer Bank, Ihr ID Austria Zertifikat läuft ab, nennen Sie mir Ihre Verfügernummer und bestätigen Sie die Freigabe in George",
                    "Landeskriminalamt hier, Einbrecher haben Ihre Adresse, Sie müssen Stillschweigen bewahren und die Goldmünzen übergeben",
                ],
                [
                    "Grüß Gott, hier ist Ihre Bank, Ihre neue Bankomatkarte liegt in der Filiale zur Abholung bereit",
                    "Hallo, hier ist die Österreichische Post, Ihr Paket kommt morgen Vormittag",
                    "Grüß Gott, Ordination Doktor Huber, wir erinnern Sie an Ihren Termin am Donnerstag um zehn",
                ],
            ),
            (
                "ch",
                [
                    "Hier ist die Kantonspolizei, Ihr Sohn hatte einen schweren Unfall, das bleibt streng geheim, Sie müssen die Wertsachen übergeben",
                    "Hier ist der TWINT Support, lesen Sie mir den TWINT-Code vor, dann müssen Sie die Zahlung freigeben",
                ],
                [
                    "Grüezi, hier ist Ihre Kantonalbank, Ihre neue Debitkarte ist unterwegs",
                    "Grüezi, die Post hier, Ihr Paket kommt morgen zwischen acht und zwölf",
                    "Grüezi, hier ist die Arztpraxis am See, wir erinnern Sie an Ihren Termin am Donnerstag um zehn",
                ],
            ),
        ];
        for (iso, scams, ordinary) in countries {
            assert!(db().countries.contains_key(*iso), "missing country profile: {iso}");
            for t in scams {
                let hits = keyword_hits(t);
                assert!(engages(&hits), "{iso}: scam not caught: {t} {hits:?}");
                assert!(from_country(iso, &hits), "{iso}: none of its own keywords: {t} {hits:?}");
            }
            for t in ordinary {
                let hits = keyword_hits(t);
                assert!(!engages(&hits), "{iso}: ordinary call engaged: {t} {hits:?}");
                assert!(!from_country(iso, &hits), "{iso}: ordinary call hit its keywords: {t} {hits:?}");
            }
            let whole = keyword_hits(&ordinary.join(" "));
            assert!(!engages(&whole), "{iso}: ordinary calls together engaged: {whole:?}");
        }
    }
}

#[cfg(test)]
mod review_probe_tests {
    use super::*;

    /// The engage rule the live call uses (gemini_live::stall_engages): any scam-only
    /// demand, or a score of 3 with one clear keyword (weight 2 or more).
    /// Hits are distinct keywords, as a call accumulates them.
    fn engaged(text: &str) -> (bool, u32, Vec<String>) {
        let hits = keyword_hits(text);
        let mut seen: Vec<&'static str> = Vec::new();
        let mut score = 0;
        let mut strong = false;
        let mut hard = false;
        let mut shown = Vec::new();
        for h in &hits {
            shown.push(format!("{}({}{})", h.text, h.weight, if h.hard { ",hard" } else { "" }));
            if seen.contains(&h.text) {
                continue;
            }
            seen.push(h.text);
            score += h.weight;
            strong |= h.weight >= 2;
            hard |= h.hard;
        }
        (hard || (score >= 3 && strong), score, shown)
    }

    /// Existing behavior, not asserted: these ordinary calls score on keywords the database had before the added countries (信用卡, 轉帳, credit card, insurance, transferencia, crédito, kredit).
    const ORDINARY_ON_EXISTING_WORDS: &[&str] = &[
        "你好，呢度係銀行客戶服務部，你張信用卡就嚟到期，我哋會寄新卡畀你，請留意郵件",
        "你好，我係業主，今個月租金可以轉賬到我之前個戶口，下個月我會請師傅嚟檢查冷氣機",
        "Hi, this is TD Bank calling about your credit card statement. Your payment is due on the fifteenth and you can pay online or at any branch.",
        "Hello, this is the dental office reminding you about your cleaning on Friday at two. Please bring your insurance card.",
        "Hola, le habla el arrendador, el depósito de la renta se hace el día primero, puede hacer la transferencia a la misma cuenta.",
        "Hola, te llamo del Banco Galicia, tu tarjeta de crédito vence el mes que viene y te enviamos una nueva al domicilio.",
        "Hola, soy el dueño del departamento, el alquiler se paga por transferencia como siempre, el CBU es el mismo.",
        "Grüß Gott, hier ist die Erste Bank, Ihre Kreditkartenabrechnung ist im Online-Banking bereit, die Zahlung wird am Monatsende abgebucht.",
        "Grüezi, hier ist die UBS, Ihre Kreditkartenabrechnung ist im E-Banking bereit, die Zahlung wird per Lastschrift eingezogen.",
    ];

    const ORDINARY: &[(&str, &[&str])] = &[
        ("hk", &[
            "你好，我係送貨員，你嘅快遞喺大廈樓下，可唔可以落嚟攞，或者留低喺管理處",
            "你好，呢度係牙醫診所，提醒你星期三下晝三點洗牙，如果唔方便可以改期",
            "你好，我係學校老師，你個仔今日喺學校發燒，請你盡快嚟接佢",
        ]),
        ("ca", &[
            "Hello, this is Canada Post, we tried to deliver your package today. We will try again tomorrow, or you can pick it up at the post office.",
            "Hi, this is your landlord, the rent for next month is due on the first, you can send an e-transfer as usual.",
            "Hello, this is Mrs. Lee from the school, your son has a bit of a fever and we need you to pick him up.",
        ]),
        ("au", &[
            "Hi, this is Commonwealth Bank, your new card has been posted and should arrive in a few days. You can activate it in the app.",
            "G'day, Australia Post here, we have a parcel for you, we'll deliver it this afternoon, someone needs to sign.",
            "Hi, it's the dental clinic, just confirming your check-up on Thursday at ten.",
            "Hi, it's the real estate agent, the rental inspection will be next Tuesday, and the rent is due on Friday via direct debit.",
            "Hi, it's your accountant, your tax return is ready to lodge with the ATO, do you want me to submit it?",
        ]),
        ("mx", &[
            "Buenas tardes, le llamamos de BBVA para avisarle que su estado de cuenta ya está disponible en la aplicación y que su pago vence el día quince.",
            "Hola, soy repartidor de Mercado Libre, tengo su paquete, estoy afuera de su casa, ¿me abre?",
            "Buenos días, le llamamos del consultorio dental para recordarle su limpieza del viernes a las cuatro de la tarde.",
            "Buenas tardes, soy la maestra de su hijo, la próxima semana hay junta de padres, ¿podría venir?",
        ]),
        ("ar", &[
            "Hola, soy de Andreani, tengo un paquete para vos, ¿vas a estar en casa esta tarde?",
            "Buen día, te llamamos de la clínica para recordarte el turno con el odontólogo el jueves a las diez.",
            "Hola, soy la directora de la escuela, mañana no hay clases por el paro, avisale a tu hija.",
        ]),
        ("at", &[
            "Hallo, hier ist der Paketdienst, wir stehen vor Ihrer Tür, aber niemand öffnet. Wir liefern morgen noch einmal.",
            "Grüß Gott, hier ist die Zahnarztpraxis, wir erinnern Sie an Ihren Kontrolltermin am Dienstag um halb neun.",
            "Grüß Gott, hier ist Ihr Vermieter, die Miete überweisen Sie bitte wieder auf das gleiche Konto, nächste Woche kommt der Installateur wegen der Heizung.",
            "Grüß Gott, hier ist die Volksschule, Ihr Sohn hat Fieber und muss abgeholt werden.",
        ]),
        ("ch", &[
            "Grüezi, hier ist die Post, wir haben ein Paket für Sie, wir bringen es morgen Nachmittag noch einmal.",
            "Grüezi, hier ist die Zahnarztpraxis, wir erinnern Sie an Ihre Dentalhygiene am Mittwoch um vierzehn Uhr.",
            "Grüezi, hier ist die Verwaltung, bitte zahlen Sie die Miete wie immer per Dauerauftrag, die Heizung wird morgen kontrolliert.",
            "Grüezi, hier ist die Schule, Ihre Tochter hat Bauchweh und muss abgeholt werden.",
        ]),
    ];

    const SCAMS: &[(&str, [&str; 2])] = &[
        ("hk", [
            "我係香港警務處，你涉嫌洗黑錢，要將戶口所有款項轉入政府保管賬戶，唔好同任何人講",
            "喂，你仲記唔記得我？我係你老友，我而家出咗事要借錢，你快啲轉數快俾我",
        ]),
        ("ca", [
            "This is Service Canada, your social insurance number has been suspended due to suspicious activity, press 1 to speak with an officer, you must pay in gift cards",
            "Hi grandma, it's me, I've been arrested, please don't tell mom and dad, I need bail money in bitcoin",
        ]),
        ("au", [
            "This is Telstra, your internet will be disconnected, we need remote access to your computer, please download AnyDesk",
            "This is the Australian Federal Police, your identity was used for money laundering, you must transfer your savings to a safe account",
        ]),
        ("mx", [
            "Soy de la policía federal, su número está ligado a un secuestro, no cuelgue, deposite en Oxxo el rescate y no hable con nadie",
            "Buenas tardes, soy del SAT, tiene un adeudo pendiente, debe pagar hoy con tarjetas de regalo o se le emitirá una orden de aprehensión",
        ]),
        ("ar", [
            "Hola mamá, soy yo, se me rompió el celular, es un número nuevo, necesito que me hagas una transferencia urgente",
            "Le llamamos de ARCA, su CUIT tiene una deuda, para evitar un embargo debe pasar el token de su home banking",
        ]),
        ("at", [
            "Hier ist Europol, Ihr Konto wurde gehackt, Sie müssen Ihr Geld auf ein sicheres Konto überweisen und mit niemandem darüber sprechen",
            "Hier ist Microsoft Support, Ihr Computer ist infiziert, bitte installieren Sie AnyDesk, damit wir Fernzugriff haben",
        ]),
        ("ch", [
            "Hier ist die Polizei, Sie müssen Bargeld und Schmuck an einen Abholer übergeben, sprechen Sie mit niemandem",
            "Hier ist die Bank, bitte geben Sie mir den Code aus der Banking-App, damit wir Ihr Konto schützen können",
        ]),
    ];

    /// Ordinary calls, one at a time: no hard hit, not engaged, no strong keyword at all.
    #[test]
    fn review_probe_ordinary_calls_one_by_one() {
        let mut bad = Vec::new();
        for t in ORDINARY_ON_EXISTING_WORDS {
            let (_, _, shown) = engaged(t);
            println!("existing words: {shown:?} :: {t}");
        }
        for (iso, calls) in ORDINARY {
            for t in calls.iter() {
                let (eng, score, shown) = engaged(t);
                if eng || score > 1 {
                    bad.push(format!("{iso}: engaged={eng} score={score} {shown:?} :: {t}"));
                }
            }
        }
        assert!(bad.is_empty(), "ordinary calls with hits:\n{}", bad.join("\n"));
    }

    /// The same five calls heard in one call (hits accumulate over a call).
    #[test]
    fn review_probe_ordinary_calls_accumulated() {
        let mut bad = Vec::new();
        for (iso, calls) in ORDINARY {
            let all = calls.join(" ");
            let (eng, score, shown) = engaged(&all);
            if eng {
                bad.push(format!("{iso}: engaged score={score} {shown:?}"));
            }
        }
        assert!(bad.is_empty(), "accumulated ordinary calls engaged:\n{}", bad.join("\n"));
    }

    #[test]
    fn review_probe_scams_engage() {
        let mut bad = Vec::new();
        for (iso, calls) in SCAMS {
            for t in calls {
                let (eng, score, shown) = engaged(t);
                if !eng {
                    bad.push(format!("{iso}: not engaged score={score} {shown:?} :: {t}"));
                }
            }
        }
        assert!(bad.is_empty(), "scams not caught:\n{}", bad.join("\n"));
    }

    /// Weights of the new countries' keywords: plain common words stay at 1.
    #[test]
    fn review_probe_weights_listed() {
        let mut lines = Vec::new();
        for iso in ["hk", "ca", "au", "mx", "ar", "at", "ch"] {
            let mut by_w = std::collections::BTreeMap::<u32, Vec<&str>>::new();
            for (k, pairs) in db().country_kw_map.iter() {
                if pairs.iter().any(|(i, _)| i == iso) {
                    if let Some(h) = keyword(k) {
                        by_w.entry(h.weight).or_default().push(h.text);
                    }
                }
            }
            for (w, v) in by_w {
                lines.push(format!("{iso} w={w} n={} {:?}", v.len(), v));
            }
        }
        println!("{}", lines.join("\n"));
    }
}

/// Countries added from the regional research, second batch (Russia, Turkey,
/// Vietnam, Thailand, Saudi Arabia, the Emirates, Egypt, the Netherlands, Poland):
/// two typical scam lines in the country's language trip its own keywords and
/// engage the way a live call does; ordinary bank, courier, clinic and landlord
/// or school calls there trip none of its keywords and do not engage, one by one
/// or together.
#[cfg(test)]
mod added_countries_2_tests {
    use super::*;

    /// The engage rule the live call uses (gemini_live::stall_engages): any scam-only
    /// demand, or a score of 3 with one keyword of weight 2. Hits count once each.
    fn engages(hits: &[Hit]) -> bool {
        let mut seen: Vec<&str> = Vec::new();
        let (mut score, mut strong, mut hard) = (0, false, false);
        for h in hits {
            if seen.contains(&h.text) {
                continue;
            }
            seen.push(h.text);
            score += h.weight;
            strong |= h.weight >= 2;
            hard |= h.hard;
        }
        hard || (score >= 3 && strong)
    }

    fn from_country(iso: &str, hits: &[Hit]) -> bool {
        hits.iter().any(|h| {
            db().country_kw_map
                .get(&normalized_hit_key(h.text))
                .is_some_and(|pairs| pairs.iter().any(|(i, _)| i == iso))
        })
    }

    #[test]
    fn added_countries_2_catch_their_scams_and_leave_ordinary_calls_alone() {
        let countries: &[(&str, [&str; 2], [&str; 4])] = &[
            (
                "ru",
                [
                    "Здравствуйте, я сотрудник ФСБ, на ваше имя пытаются оформить кредит, чтобы сохранить деньги, переведите их на безопасный счёт",
                    "Добрый день, это ваш оператор связи, срок действия сим-карты заканчивается, назовите код для продления договора из СМС",
                ],
                [
                    "Здравствуйте, это ваш банк, ваша новая карта готова, её можно забрать в отделении с паспортом",
                    "Добрый день, это курьерская служба, ваша посылка будет завтра с десяти до двух",
                    "Здравствуйте, это поликлиника, напоминаем о вашем приёме у терапевта в четверг в десять утра",
                    "Добрый вечер, это хозяйка квартиры, в субботу придёт сантехник посмотреть кран на кухне",
                ],
            ),
            (
                "tr",
                [
                    "Emniyet Terörle Mücadele şubesinden arıyorum, adınız bir terör örgütü soruşturmasında geçiyor, paranızı devlet güvence hesabına aktarmanız gerekiyor",
                    "Bankanızın güvenlik biriminden arıyorum, birikmiş kart aidatı iadeniz var, iade için telefonunuza gelen şifreyi söylemeniz lazım",
                ],
                [
                    "Merhaba, bankanızdan arıyoruz, yeni banka kartınız şubeye ulaştı, kimliğinizle gelip alabilirsiniz",
                    "Merhaba, kargo firmasından arıyorum, paketiniz yarın öğleden sonra adresinize teslim edilecek",
                    "İyi günler, diş kliniğinden arıyoruz, perşembe saat ondaki randevunuzu hatırlatmak istedik",
                    "Merhaba, okuldan arıyorum, oğlunuzun biraz ateşi var, gelip alabilir misiniz",
                ],
            ),
            (
                "vn",
                [
                    "Tôi là cán bộ công an phường, anh liên quan đường dây rửa tiền, anh phải giữ bí mật điều tra và chuyển tiền vào tài khoản tạm giữ",
                    "Em là nhân viên điện lực, nhà anh sẽ bị cắt điện sau hai tiếng, anh cài ứng dụng điện lực qua đường link em gửi rồi đọc mã Smart OTP cho em",
                ],
                [
                    "Chào anh, em gọi từ ngân hàng, thẻ mới của anh đã về chi nhánh, anh mang căn cước đến nhận nhé",
                    "Alo, em là shipper, em giao hàng cho anh, anh ra cổng nhận giúp em nhé",
                    "Chào chị, phòng khám nha khoa nhắc chị lịch hẹn chiều thứ năm lúc ba giờ",
                    "Chào em, chị là chủ nhà, cuối tuần thợ sẽ đến sửa vòi nước trong bếp",
                ],
            ),
            (
                "th",
                [
                    "ผมเป็นเจ้าหน้าที่ตำรวจ บัญชีของคุณพัวพันคดีฟอกเงิน ให้แอดไลน์สอบปากคำทางวิดีโอคอล แล้วโอนเงินมาตรวจสอบความบริสุทธิ์",
                    "ติดต่อจากการไฟฟ้าค่ะ จะคืนเงินประกันมิเตอร์ให้ กรุณากดลิงก์โหลดแอปการไฟฟ้าที่ส่งไปทางไลน์นะคะ",
                ],
                [
                    "สวัสดีค่ะ ติดต่อจากธนาคาร บัตรเดบิตใบใหม่ของคุณมาถึงสาขาแล้ว มารับได้พร้อมบัตรประชาชนค่ะ",
                    "สวัสดีครับ ไปรษณีย์ไทยครับ พรุ่งนี้จะไปส่งพัสดุช่วงบ่าย มีคนอยู่บ้านไหมครับ",
                    "สวัสดีค่ะ จากคลินิกทันตกรรม ขอเตือนนัดขูดหินปูนวันพฤหัสบดีสิบโมงค่ะ",
                    "สวัสดีค่ะ ครูประจำชั้นนะคะ วันนี้ลูกมีไข้ รบกวนผู้ปกครองมารับที่โรงเรียนค่ะ",
                ],
            ),
            (
                "sa",
                [
                    "معك موظف من منصة أبشر، حسابك بيتوقف اليوم، اختر الرقم الظاهر في نفاذ وعطني الرمز",
                    "نتصل من البنك المركزي، حسابك مجمد، لازم نحول رصيدك إلى حساب حماية مؤقت حتى نفك التجميد",
                ],
                [
                    "السلام عليكم، معك البنك الأهلي، بطاقتك الجديدة وصلت الفرع وتقدر تستلمها بالهوية",
                    "مرحبا، معك مندوب أرامكس، شحنتك بتوصل بكرة العصر",
                    "السلام عليكم، معك عيادة الأسنان، نذكرك بموعدك يوم الخميس الساعة عشرة",
                    "مرحبا، معك صاحب الشقة، بكرة يجي الفني يصلح المكيف",
                ],
            ),
            (
                "ae",
                [
                    "معك شرطة دبي، عليك بلاغ غسيل أموال، لازم تدفع الغرامة اليوم عن طريق الصرافة",
                    "معك بريد الإمارات، طردك محجوز، ادفع رسوم التوصيل عبر الرابط اللي أرسلناه وأدخل بيانات البطاقة في الرابط",
                ],
                [
                    "مرحبا، معك بنك الإمارات دبي الوطني، بطاقتك الجديدة جاهزة للاستلام من الفرع",
                    "مرحبا، معك مندوب التوصيل، طلبك بيوصل الساعة خمسة، وين أتركه",
                    "مرحبا، معك المركز الطبي، نذكرك بموعد التطعيم يوم الأحد الساعة تسعة",
                    "مرحبا، معك المدرسة، ولدك عنده حرارة، ممكن تيجي تاخذه",
                ],
            ),
            (
                "eg",
                [
                    "معاك خدمة عملاء البنك، حساب إنستاباي بتاعك هيتقفل، قولي الأرقام اللي على ضهر الكارت وهنعمل تحويل تجريبي",
                    "مبروك يا فندم، كسبت جايزة من فودافون كاش، بس لازم تحول رسوم تشغيل الجايزة الأول",
                ],
                [
                    "أهلا يا فندم، معاك البنك الأهلي، الكارت الجديد بتاعك وصل الفرع وممكن تستلمه بالبطاقة",
                    "ألو، معاك مندوب الشحن، الأوردر بتاعك هيوصل بكرة الصبح",
                    "مساء الخير، معاك عيادة الدكتور، بنفكر حضرتك بميعاد الكشف يوم الخميس الساعة سبعة",
                    "ألو، أنا صاحب الشقة، السباك جاي بكرة يصلح الحنفية",
                ],
            ),
            (
                "nl",
                [
                    "U spreekt met de afdeling fraude van uw bank, er is ingelogd vanaf een vreemd apparaat, we moeten uw geld veiligstellen op een kluisrekening",
                    "Goedemiddag, met de politie, inbrekers hebben uw adres, een collega komt uw waardevolle spullen veiligstellen en u moet uw pincode doorgeven",
                ],
                [
                    "Goedemiddag, u spreekt met uw bank, uw nieuwe betaalpas is verstuurd en komt binnen vijf werkdagen",
                    "Hallo, met PostNL, uw pakket wordt morgen tussen negen en twaalf bezorgd",
                    "Goedemorgen, met de huisartsenpraktijk, we bellen om uw afspraak van donderdag om tien uur te bevestigen",
                    "Hallo, met de verhuurder, volgende week komt de monteur de cv-ketel nakijken",
                ],
            ),
            (
                "pl",
                [
                    "Dzwonię z departamentu bezpieczeństwa banku, ktoś składa wniosek o kredyt na pana dane, proszę przelać środki na konto techniczne",
                    "Tu komisarz z CBŚP, prowadzimy tajną akcję, obowiązuje panią tajemnica śledztwa, potrzebne jest zabezpieczenie gotówki",
                ],
                [
                    "Dzień dobry, dzwonię z banku, pana nowa karta płatnicza została wysłana i dotrze w ciągu tygodnia",
                    "Dzień dobry, kurier, paczka będzie jutro po południu, czy ktoś będzie w domu",
                    "Dzień dobry, przychodnia, przypominamy o wizycie u lekarza w czwartek o dziesiątej",
                    "Dzień dobry, dzwonię ze szkoły, syn ma gorączkę, proszę go odebrać",
                ],
            ),
        ];
        let mut bad = Vec::new();
        for (iso, scams, ordinary) in countries {
            if !db().countries.contains_key(*iso) {
                bad.push(format!("missing country profile: {iso}"));
                continue;
            }
            for t in scams {
                let hits = keyword_hits(t);
                if !engages(&hits) || !from_country(iso, &hits) {
                    bad.push(format!("{iso}: scam not caught by its own keywords: {t} {hits:?}"));
                }
            }
            for t in ordinary {
                let hits = keyword_hits(t);
                if engages(&hits) || from_country(iso, &hits) {
                    bad.push(format!("{iso}: ordinary call hit: {t} {hits:?}"));
                }
            }
            let whole = keyword_hits(&ordinary.join(" "));
            if engages(&whole) {
                bad.push(format!("{iso}: ordinary calls together engaged: {whole:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Reviewer probes for the third batch of countries, under the live rule
/// (gemini_live::stall_score_add + stall_engages, copied step by step).
#[cfg(test)]
mod review_probe3_tests {
    use super::*;

    fn live(text: &str, skip_new: bool) -> (bool, u32, Vec<&'static str>) {
        let found: Vec<(&'static str, u32, bool)> = keyword_hits(text)
            .into_iter()
            .filter(|h| {
                !(skip_new
                    && db()
                        .country_kw_map
                        .get(&normalized_hit_key(h.text))
                        .is_some_and(|p| !p.is_empty() && p.iter().all(|(i, _)| ["ro", "md", "ua"].contains(&i.as_str()))))
            })
            .map(|h| (h.text, h.weight, h.hard))
            .collect();
        let longer = |kw: &str, k: &str| k.len() > kw.len() && k.contains(kw);
        let mut hits: Vec<&'static str> = Vec::new();
        let mut score = 0;
        for &(kw, w, _) in &found {
            if hits.contains(&kw) || found.iter().any(|&(k, _, _)| longer(kw, k)) || hits.iter().any(|&h| longer(kw, h)) {
                continue;
            }
            hits.push(kw);
            score += w;
        }
        let engaged = hits.iter().any(|h| is_hard(h))
            || (score >= 3 && hits.iter().any(|h| keyword(h).map_or(0, |k| k.weight) >= 2));
        (engaged, score, hits)
    }

    const NORMAL: &[(&str, &str)] = &[
        ("ro", "Bună ziua, sunt de la banca dumneavoastră, extrasul de cont pentru luna aceasta este disponibil în aplicație"),
        ("ro", "Bună ziua, sunt curier Fan Courier, am un colet pentru dumneavoastră, sunteți acasă între patru și șase"),
        ("ro", "Bună ziua, vă sunăm de la clinică, analizele dumneavoastră sunt gata, puteți trece să le ridicați"),
        ("ro", "Bună seara, sunt proprietara apartamentului, vreau să trec luni să verific contorul de gaze"),
        ("ro", "Bună ziua, de la școala copilului, ședința cu părinții este miercuri la ora șase"),
        ("ro", "Alo, Poșta Română, coletul dumneavoastră este la oficiul poștal, vă rugăm să veniți cu buletinul"),
        ("ro", "Bună ziua, ANAF, aveți de depus declarația unică până pe 25 mai, puteți face asta online în spațiul privat virtual"),
        ("ro", "Bună ziua, sunt de la bancă, cardul dumneavoastră expiră luna viitoare, vă trimitem unul nou prin curier la adresa de domiciliu"),
        ("md", "Bună ziua, sunt de la Moldova Agroindbank, extrasul de cont este gata, îl puteți ridica la filială"),
        ("md", "Bună ziua, sunt curier, am un colet pentru dumneavoastră, sunteți acasă astăzi după ora patru"),
        ("md", "Bună ziua, vă sun de la policlinică, rezultatele analizelor sunt gata, puteți veni după ele"),
        ("md", "Bună ziua, sunt proprietarul apartamentului, mâine vine meșterul să schimbe contorul de apă"),
        ("md", "Bună ziua, de la liceu, ședința cu părinții este joi la ora cinci"),
        ("ua", "Добрий день, це Приватбанк, виписка по вашому рахунку доступна в застосунку"),
        ("ua", "Добрий день, кур'єр Укрпошти, у мене для вас посилка, ви вдома"),
        ("ua", "Добрий день, клініка, ваші аналізи готові, можна забрати"),
        ("ua", "Добрий день, я орендодавець, у понеділок прийду перевірити лічильники"),
        ("ua", "Добрий день, школа, батьківські збори у середу о шостій"),
        ("ua", "Добрий день, це ваш банк, ви подавали заявку на кредит, запрошуємо підписати договір у відділенні"),
        ("ua", "Добрий день, Нова пошта, ваша посилка чекає у відділенні, при отриманні потрібен паспорт"),
        ("ua", "Добрий день, ваша картка закінчується у наступному місяці, ми відправимо нову кур'єром на вашу адресу"),
        ("ua", "Добрый день, это курьер, у меня для вас посылка, вы дома"),
        ("ua", "Добрый день, это ваш банк, выписка по счету доступна в приложении"),
        ("ua", "Добрий день, Ощадбанк, пенсійна виплата надійшла на вашу картку, баланс у застосунку"),
    ];

    /// Existing behavior, not asserted: Romanian "credit" matches the older French "crédit" (weight 2) and with "banca" engages.
    #[allow(dead_code)]
    const ENGAGE_ON_EXISTING_WORDS: &[(&str, &str)] = &[
        ("ro", "Bună ziua, sunt de la bancă, ați cerut un credit pentru renovare, vă propunem o programare la sucursală pentru semnarea contractului"),
        ("ro", "Bună ziua, banca vă anunță că rata de credit a fost debitată din contul dumneavoastră, soldul este în aplicație"),
        ("md", "Bună ziua, vă sunăm de la banca MAIB, ați solicitat un credit de consum, vă așteptăm la semnarea contractului"),
    ];

    const SCAMS: &[(&str, &str)] = &[
        ("ro", "Sunt de la Poliția Capitalei, nepotul dumneavoastră a lovit pe cineva cu mașina, trebuie să dați urgent bani ca să nu fie arestat"),
        ("ro", "Ați câștigat un premiu de la Kaufland, pentru a-l primi trimiteți banii pentru taxa de transport prin Western Union"),
        ("ro", "Aici serviciul de securitate al băncii, mutați urgent banii în contul sigur"),
        ("ro", "Sunt de la bancă, cineva a luat un credit în numele dumneavoastră, trebuie să luați un credit de anulare chiar acum"),
        ("md", "Sunt de la Moldova Agroindbank, contul dumneavoastră a fost compromis, spuneți-mi codul primit prin SMS"),
        ("md", "Mamă, am avut un accident, dă bani unui om care vine acum la tine"),
        ("ua", "Ваша картка заблокована, я співробітник служби безпеки банку, продиктуйте код з СМС щоб розблокувати"),
        ("ua", "Мамо, я в лікарні, терміново переведи гроші на цю картку"),
        ("ua", "Ви виграли автомобіль, для отримання сплатіть податок за посиланням"),
        ("ua", "Вас турбує служба безпеки банку, на вас оформлюють кредит, переведіть гроші на безпечний рахунок"),
    ];

    #[test]
    fn live_rule_leaves_ordinary_calls_alone() {
        let mut bad = Vec::new();
        for (iso, t) in NORMAL {
            let (e, s, h) = live(t, false);
            let (e0, _, _) = live(t, true);
            println!("NORMAL [{iso}] engaged={e} without_new={e0} score={s} hits={h:?}");
            if e {
                bad.push(format!("[{iso}] engaged (without the new words: {e0}) score={s} hits={h:?}: {t}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn live_rule_on_reworded_scams_report() {
        for (iso, t) in SCAMS {
            let (e, s, h) = live(t, false);
            let (e0, _, _) = live(t, true);
            println!("SCAM [{iso}] engaged={e} without_new={e0} score={s} hits={h:?}");
        }
    }

    /// The new words must never make a call engage that did not without them.
    #[test]
    fn new_words_never_tip_anything() {
        let mut bad = Vec::new();
        for (iso, t) in NORMAL.iter().chain(SCAMS.iter()) {
            let (e, _, h) = live(t, false);
            let (e0, _, _) = live(t, true);
            if e && !e0 && NORMAL.iter().any(|(_, n)| n == t) {
                bad.push(format!("[{iso}] tipped by new words: {h:?}: {t}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Reviewer probe (second batch of countries): own ordinary calls, legitimate
/// anti-fraud warnings and reworded scams, run through the live engage rule.
#[cfg(test)]
mod review_probe2_tests {
    use super::*;

    fn engages(hits: &[Hit]) -> bool {
        let mut seen: Vec<&str> = Vec::new();
        let (mut score, mut strong, mut hard) = (0, false, false);
        for h in hits {
            if seen.contains(&h.text) {
                continue;
            }
            seen.push(h.text);
            score += h.weight;
            strong |= h.weight >= 2;
            hard |= h.hard;
        }
        hard || (score >= 3 && strong)
    }

    fn from_country(iso: &str, hits: &[Hit]) -> bool {
        hits.iter().any(|h| {
            db().country_kw_map
                .get(&normalized_hit_key(h.text))
                .is_some_and(|pairs| pairs.iter().any(|(i, _)| i == iso))
        })
    }

    type Row = (&'static str, &'static [&'static str], &'static [&'static str], &'static [&'static str]);

    /// Existing behavior, not asserted: these engage on keywords the database had before the added countries (kredi kartı, khoản vay, สินเชื่อ, kredyt; the scam-only безопасный счёт, otp and رمز التحقق inside an anti-fraud warning).
    #[allow(dead_code)]
    const ENGAGE_ON_EXISTING_WORDS: &[&str] = &[
        "Здравствуйте, это банк, напоминаем: никогда не переводите деньги на безопасный счёт, это делают только мошенники",
        "Merhaba, müşteri hizmetlerinden arıyorum, kredi kartı ekstrenizin son ödeme tarihi cuma günüdür",
        "Chào anh, em gọi từ ngân hàng, khoản vay của anh đã tất toán xong, anh không cần làm gì thêm",
        "Ngân hàng nhắc anh: không bao giờ đọc mã Smart OTP cho bất kỳ ai, kể cả người xưng là nhân viên ngân hàng",
        "สวัสดีค่ะ ติดต่อจากธนาคาร สินเชื่อของคุณปิดบัญชีเรียบร้อยแล้วนะคะ ไม่ต้องดำเนินการเพิ่มค่ะ",
        "تنبيه من البنك: لا تشارك رمز التحقق من أبشر مع أي أحد",
        "Dzień dobry, bank, założyliśmy dla pana konto techniczne do obsługi kredytu hipotecznego, wystarczy podpisać aneks w placówce",
    ];

    fn rows() -> Vec<Row> {
        vec![
            ("ru",
             &["Здравствуйте, это сервисный центр, ваш телефон отремонтирован, можно забирать с девяти до шести",
               "Добрый день, вас беспокоит управляющая компания, завтра с десяти до двенадцати отключат горячую воду",
               "Здравствуйте, это банк, по вашему кредиту платёж в этом месяце списан автоматически, дополнительных действий не требуется",
               "Добрый день, это школа, завтра родительское собрание в шесть вечера, приходите, пожалуйста"],
             &["Добрый день, помните, что код от Госуслуг нельзя сообщать никому, даже сотруднику банка"],
             &["Это Следственный комитет, на вас заведено дело, для сохранности средств переведите всё на безопасный счет",
               "Здравствуйте, вам положена компенсация, продиктуйте код из смс, который придёт сейчас"]),
            ("tr",
             &["İyi günler, belediyeden arıyoruz, yarın mahallenizde su kesintisi olacak",
               "Merhaba, eczaneden arıyorum, reçetenizdeki ilaç geldi, uğrayabilirsiniz",
               "Merhaba, ev sahibiyim, kombi bakımı için cumartesi usta gelecek"],
             &["Merhaba, bankanızdan arıyoruz, dolandırıcılar sivil polise teslim etmeniz için kartınızı isteyebilir, asla vermeyin"],
             &["Ben polis memuruyum, hesabınızdaki paranın devlet güvence hesabına alınması lazım yoksa dosya açılacak",
               "Kartınızın aidat iadesi çıktı, güvenli hesaba havale yapmanız gerekiyor"]),
            ("vn",
             &["Alo, bên điện lực đây ạ, tuần sau khu vực nhà anh cắt điện bảo trì từ 8 giờ đến 11 giờ",
               "Chào chị, em gọi từ bệnh viện, nhắc chị mai đi tái khám lúc 9 giờ",
               "Chào anh, trường mầm non gọi, mai anh đón bé sớm giúp em nhé"],
             &[],
             &["Bên công an đây, tài khoản của anh bị đóng băng, anh chuyển tiền sang tài khoản tạm giữ để điều tra",
               "Điện lực thông báo anh nợ tiền điện, cài app điện lực theo đường link này và đọc mã Smart OTP"]),
            ("th",
             &["สวัสดีครับ การไฟฟ้าครับ พรุ่งนี้จะมีไฟดับช่วงเช้าเพื่อซ่อมบำรุงสายไฟแถวบ้านคุณครับ",
               "สวัสดีค่ะ โรงพยาบาลโทรมาเตือนนัดตรวจเลือดพรุ่งนี้เช้าค่ะ",
               "สวัสดีค่ะ เจ้าของห้องนะคะ วันเสาร์ช่างจะมาซ่อมแอร์ค่ะ"],
             &["สวัสดีค่ะ ธนาคารแจ้งเตือนว่าอย่าโหลดแอปการไฟฟ้าปลอมจากลิงก์ที่ส่งมาทางไลน์นะคะ"],
             &["ตำรวจไซเบอร์ค่ะ บัญชีคุณเกี่ยวข้องกับคดีฟอกเงิน ต้องโอนเงินมาตรวจสอบ ห้ามบอกใคร",
               "การไฟฟ้าจะคืนเงินประกันมิเตอร์ ให้โหลดแอปการไฟฟ้าจากลิงก์นี้"]),
            ("sa",
             &["السلام عليكم، معك البنك الأهلي، تم سداد قسط التمويل الشهري بنجاح",
               "هلا، معك شركة الكهرباء، بكرة بيكون فيه انقطاع للتيار من الثامنة للعاشرة",
               "مرحبا، معك المستشفى، نذكرك بموعد التحاليل بكرة الصبح",
               "هلا والله، أنا المالك، يوم السبت بيجي السباك"],
             &["تنبيه من البنك: لا تعطي الرقم الظاهر في نفاذ لأي شخص يتصل فيك"],
             &["أنا من الأمن السيبراني، حسابك فيه عملية مشبوهة، حوّل فلوسك لحساب حماية مؤقت",
               "معك أبشر، لازم تدخل نفاذ وتقول لي رمز التحقق من أبشر"]),
            ("ae",
             &["مرحبا، معك مصرف أبوظبي الإسلامي، تم تجديد بطاقتك الائتمانية وراح توصلك خلال أسبوع",
               "هلا، معك شركة ديوا، بتكون فيه صيانة للمياه يوم الخميس",
               "مرحبا، معك العيادة، نذكرك بموعد تنظيف الأسنان",
               "مرحبا، معك مكتب الإيجار، لازم نجدد العقد",
               "مرحبا، معك بنك الإمارات، حوالتك وصلت وتقدر تستلمها عبر الصرافة أو من التطبيق"],
             &[],
             &["شرطة أبوظبي، عليك مخالفة وبلاغ، ادفع الغرامة اليوم عبر الصرافة قبل الحجز",
               "طردك محجوز في الجمارك، ادفع رسوم التوصيل عبر الرابط وأدخل بيانات البطاقة في الرابط"]),
            ("eg",
             &["ألو، معاك فودافون، رصيدك قرب يخلص، تحب تشحن",
               "أهلا، معاك البنك الأهلي، تم تحويل مرتبك الساعة تسعة",
               "مساء الخير، معاك المستشفى، ميعاد التحاليل بكرة",
               "ألو، أنا المدرس، الواد محتاج كراسة",
               "ألو، معاك خدمة عملاء فودافون، كسبت جايزة من فودافون كاش ضمن مسابقة التطبيق",
               "معاك شركة مدفوعات، هنعمل تحويل تجريبي بجنيه واحد للتأكد من الحساب"],
             &[],
             &["معاك من البنك، كارتك هيتوقف، ادخل الأرقام اللي على ضهر الكارت",
               "كسبت جايزة من فودافون كاش، ادفع رسوم تشغيل الجايزة الأول"]),
            ("nl",
             &["Goedemiddag, u spreekt met ING, uw hypotheekrente wordt volgende maand aangepast, u ontvangt hierover een brief",
               "Hallo, met de gemeente, de afvalcontainer wordt donderdag geleegd",
               "Goedemorgen, met de tandarts, we bellen om uw controle van vrijdag te herinneren",
               "Hallo, met de school van Daan, hij is ziek geworden, kunt u hem ophalen",
               "Goedemiddag, met uw bank, wilt u uw geld veiligstellen op een spaardeposito met vaste rente"],
             &[],
             &["Met de bankhelpdesk, uw rekening is gehackt, u moet uw geld overmaken naar een kluisrekening",
               "Met de politie, we hebben inbrekers gepakt met uw gegevens, een agent komt langs om uw waardevolle spullen veiligstellen, en geeft u uw pincode door"]),
            ("pl",
             &["Dzień dobry, dzwonię z banku, rata kredytu została pobrana z konta, nie musi pan nic robić",
               "Dzień dobry, tu dentysta, przypominam o kontroli w piątek o ósmej",
               "Dzień dobry, jestem właścicielem mieszkania, w sobotę przyjdzie hydraulik",
               "Dzień dobry, dzwonię z urzędu gminy, odbiór dowodu osobistego jest możliwy od poniedziałku",
               "Dzień dobry, policja, jestem rzecznikiem, nie mogę podać szczegółów ze względu na tajemnicę śledztwa, ale pana samochód został odnaleziony"],
             &[],
             &["Mówi policja, pana konto jest zagrożone, proszę wpłacić gotówkę na konto techniczne do zabezpieczenia",
               "Jestem z prokuratury, sprawa objęta tajemnicą śledztwa, nie wolno informować nikogo w banku"]),
        ]
    }

    /// Ordinary calls trip none of the country's keywords. An anti-fraud warning
    /// names the scam, so it may trip one; it must not engage.
    #[test]
    fn ordinary_calls_and_warnings_do_not_engage() {
        let mut bad = Vec::new();
        for (iso, normal, warn, _) in rows() {
            for t in normal.iter() {
                let hits = keyword_hits(t);
                if engages(&hits) || from_country(iso, &hits) {
                    bad.push(format!("{iso}: engaged={} {t} {hits:?}", engages(&hits)));
                }
            }
            for t in warn.iter() {
                let hits = keyword_hits(t);
                if engages(&hits) {
                    bad.push(format!("{iso}: warning engaged: {t} {hits:?}"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Only the new country words count: an ordinary call that engages with them
    /// but not without them is the new batch's fault.
    #[test]
    fn new_country_words_do_not_tip_ordinary_calls() {
        let mut bad = Vec::new();
        for (iso, normal, warn, _) in rows() {
            for t in normal.iter().chain(warn.iter()) {
                let hits = keyword_hits(t);
                let without: Vec<Hit> = hits.iter().copied().filter(|h| !from_country(iso, &[*h])).collect();
                if engages(&hits) && !engages(&without) {
                    bad.push(format!("{iso}: new words tip it: {t} own={:?}", hits.iter().filter(|h| from_country(iso, &[**h])).map(|h| (h.text, h.weight, h.hard)).collect::<Vec<_>>()));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn reworded_scams_are_caught() {
        let mut bad = Vec::new();
        for (iso, _, _, scams) in rows() {
            for t in scams {
                let hits = keyword_hits(t);
                println!("SCAM {iso} engaged={} own={} hits={:?}", engages(&hits), from_country(iso, &hits), hits.iter().map(|h| (h.text, h.weight, h.hard)).collect::<Vec<_>>());
                if !engages(&hits) {
                    bad.push(format!("{iso}: reworded scam not engaged: {t}"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Third batch of countries (Romania, Moldova, Ukraine): own scams caught, ordinary
/// calls and real anti-fraud warnings left alone, under the live engage rule.
#[cfg(test)]
mod added_countries_3_tests {
    use super::*;

    /// The engage rule the live call uses (gemini_live::stall_engages): any scam-only
    /// demand, or a score of 3 with one keyword of weight 2. Hits count once each.
    fn engages(hits: &[Hit]) -> bool {
        let mut seen: Vec<&str> = Vec::new();
        let (mut score, mut strong, mut hard) = (0, false, false);
        for h in hits {
            if seen.contains(&h.text) {
                continue;
            }
            seen.push(h.text);
            score += h.weight;
            strong |= h.weight >= 2;
            hard |= h.hard;
        }
        hard || (score >= 3 && strong)
    }

    fn from_country(iso: &str, hits: &[Hit]) -> bool {
        hits.iter().any(|h| {
            db().country_kw_map
                .get(&normalized_hit_key(h.text))
                .is_some_and(|pairs| pairs.iter().any(|(i, _)| i == iso))
        })
    }

    type Row = (&'static str, &'static [&'static str], &'static [&'static str], &'static [&'static str]);

    /// Existing behavior, not asserted to stay quiet: these engage on keywords the
    /// database had before this batch (the Romanian "credit" folds into the French
    /// "crédit" of weight 2, with "banca" it reaches 3). The new words must not add to it.
    const ENGAGE_ON_EXISTING_WORDS: &[(&str, &str)] = &[
        ("ro", "Banca vă reamintește: nu faceți niciodată un credit la telefon și nu mutați banii în alt cont"),
        ("ro", "Am observat o cerere de credit pe numele dumneavoastră, ați depus-o chiar dumneavoastră"),
        ("md", "Banca vă anunță că un credit pe numele dumneavoastră a fost achitat integral"),
    ];

    /// Country, typical scams, ordinary calls (bank, courier, clinic, landlord or
    /// school), real anti-fraud warnings.
    const ROWS: &[Row] = &[
        (
            "ro",
            &[
                "Bună ziua, sunt de la Banca Națională a României, cineva a cerut un credit pe numele dumneavoastră, ca să-l oprim facem un credit de anulare",
                "Sunt avocatul fiului dumneavoastră, a provocat un accident grav, pentru o împăcare amiabilă trimitem un curier după bani",
                "Sunt inspector ANAF, aveți poprire pe cont, deblocăm contul după plată prin linkul trimis",
                "Sunt de la Poliția Capitalei, nepotul dumneavoastră a lovit pe cineva cu mașina, trebuie să dați urgent bani ca să nu fie arestat",
            ],
            &[
                "Bună ziua, vă sunăm de la bancă, cardul dumneavoastră nou a ajuns la sucursală și îl puteți ridica cu buletinul",
                "Bună ziua, sunt curierul, coletul dumneavoastră ajunge mâine între zece și douăsprezece",
                "Bună ziua, cabinetul stomatologic, vă reamintim de programarea de joi la ora zece",
                "Bună seara, sunt proprietarul, sâmbătă vine instalatorul să repare robinetul din bucătărie",
            ],
            &[
                "Banca vă reamintește: nu mutați niciodată banii în alt cont la cererea cuiva care vă sună",
                "Poliția Română avertizează: nu dați bani curierilor trimiși de cei care spun că o rudă a provocat un accident",
                "Bună ziua, banca vă informează că poprirea pe cont de la ANAF a fost ridicată",
                "Poliția vă roagă să nu dați bani cuiva care spune că o rudă va fi arestată",
            ],
        ),
        (
            "md",
            &[
                "Bună ziua, sunt de la poliție, fiul dumneavoastră a provocat un accident, vine un curier să ia banii pentru împăcare",
                "Vă sun de la Banca Națională a Moldovei, s-a deschis un credit pe numele dumneavoastră, trebuie să facem un credit de anulare",
                "Mamă, am avut un accident, dă bani unui om care vine acum la tine",
            ],
            &[
                "Bună ziua, vă sună banca, cardul nou e gata, îl puteți ridica de la filială",
                "Alo, Poșta Moldovei, coletul dumneavoastră a sosit la oficiul poștal",
                "Bună ziua, centrul medical, vă amintim de vizita la medicul de familie mâine la nouă",
                "Bună ziua, vă sun de la școală, copilul are febră, puteți veni să-l luați",
            ],
            &["Poliția vă roagă să nu dați bani persoanelor care spun că o rudă a fost implicată într-un accident"],
        ),
        (
            "ua",
            &[
                "Вас турбує служба безпеки банку, з вашої картки намагаються списати гроші, назвіть три цифри на звороті картки і перекажіть кошти на резервний рахунок",
                "Ваш син потрапив у ДТП, щоб закрити кримінальну справу, передайте гроші водієві",
                "Вам нарахована грошова допомога ООН, для зарахування виплати введіть номер картки і пін-код",
                "Мамо, я в лікарні, терміново переведи гроші на цю картку",
            ],
            &[
                "Добрий день, це ваш банк, нова картка вже у відділенні, можете забрати з паспортом",
                "Добрий день, Нова пошта, ваша посилка прибуде завтра до відділення",
                "Доброго дня, стоматологія, нагадуємо про ваш запис у четвер о десятій",
                "Добрий вечір, це власниця квартири, у суботу прийде сантехнік подивитися кран",
                "Це служба безпеки банку, на ваше ім'я подано заявку на кредит, ви її подавали",
            ],
            &[
                "Банк нагадує: ніколи не називайте три цифри на звороті картки і не переказуйте кошти на резервний рахунок",
                "Поліція попереджає: не передавайте гроші водіям незнайомих людей, навіть якщо кажуть, що родич потрапив у ДТП",
                "Служба безпеки банку нагадує: працівники банку ніколи не просять пін-код",
                "Грошова допомога ООН зараховується без жодних кодів і посилань",
            ],
        ),
    ];

    #[test]
    fn added_countries_3_catch_their_scams_and_leave_ordinary_calls_alone() {
        let mut bad = Vec::new();
        for (iso, scams, ordinary, warnings) in ROWS {
            if !db().countries.contains_key(*iso) {
                bad.push(format!("missing country profile: {iso}"));
                continue;
            }
            for t in scams.iter() {
                let hits = keyword_hits(t);
                if !engages(&hits) || !from_country(iso, &hits) {
                    bad.push(format!("{iso}: scam not caught by its own keywords: {t} {hits:?}"));
                }
            }
            for t in ordinary.iter() {
                let hits = keyword_hits(t);
                if engages(&hits) || from_country(iso, &hits) {
                    bad.push(format!("{iso}: ordinary call hit: {t} {hits:?}"));
                }
            }
            let whole = keyword_hits(&ordinary.join(" "));
            if engages(&whole) {
                bad.push(format!("{iso}: ordinary calls together engaged: {whole:?}"));
            }
            for t in warnings.iter() {
                let hits = keyword_hits(t);
                if engages(&hits) {
                    bad.push(format!("{iso}: warning engaged: {t} {hits:?}"));
                }
            }
            let all_warnings = keyword_hits(&warnings.join(" "));
            if engages(&all_warnings) {
                bad.push(format!("{iso}: warnings together engaged: {all_warnings:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Only the new country words count: a sentence that engages with them but not
    /// without them is this batch's fault. And no weight-2 country word turns up in
    /// an ordinary call or a warning.
    #[test]
    fn new_country_words_do_not_tip_ordinary_calls_or_warnings() {
        let mut bad = Vec::new();
        let rows = ROWS.iter().flat_map(|(iso, _, normal, warn)| normal.iter().chain(warn.iter()).map(move |t| (*iso, *t)));
        for (iso, t) in rows.chain(ENGAGE_ON_EXISTING_WORDS.iter().copied()) {
            let hits = keyword_hits(t);
            let without: Vec<Hit> = hits.iter().copied().filter(|h| !from_country(iso, &[*h])).collect();
            if engages(&hits) && !engages(&without) {
                bad.push(format!("{iso}: new words tip it: {t} {hits:?}"));
            }
            if hits.iter().any(|h| h.weight >= 2 && from_country(iso, &[*h])) {
                bad.push(format!("{iso}: a weight-2 country word in an ordinary call or warning: {t} {hits:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// No new keyword is scam-only, and none is a weight-1 word turned stronger.
    #[test]
    fn new_country_keywords_are_weight_two_at_most_and_never_hard() {
        let mut bad = Vec::new();
        for iso in ["ro", "md", "ua"] {
            for sc in &db().countries[iso].scams {
                for rf in &sc.red_flag_keywords {
                    if rf.hard || rf.weight != 2 {
                        bad.push(format!("{iso}/{}: {:?} weight {} hard {}", sc.id, rf.text, rf.weight, rf.hard));
                    }
                }
                for ctx in &sc.context_keywords {
                    let hits = keyword_hits(ctx);
                    if hits.iter().any(|h| h.hard) {
                        bad.push(format!("{iso}/{}: context {ctx:?} is hard: {hits:?}", sc.id));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Recheck: ordinary calls near the newly added RO / MD / UA phrases, under the live rule.
#[cfg(test)]
mod review_probe3_recheck_tests {
    use super::*;

    fn is_new(h: &Hit) -> bool {
        db().country_kw_map
            .get(&normalized_hit_key(h.text))
            .is_some_and(|p| !p.is_empty() && p.iter().all(|(i, _)| ["ro", "md", "ua"].contains(&i.as_str())))
    }

    /// gemini_live::stall_score_add + stall_engages, step by step.
    fn live(text: &str, skip_new: bool) -> (bool, u32, Vec<&'static str>) {
        let found: Vec<(&'static str, u32)> =
            keyword_hits(text).into_iter().filter(|h| !(skip_new && is_new(h))).map(|h| (h.text, h.weight)).collect();
        let longer = |kw: &str, k: &str| k.len() > kw.len() && k.contains(kw);
        let mut hits: Vec<&'static str> = Vec::new();
        let mut score = 0;
        for &(kw, w) in &found {
            if hits.contains(&kw) || found.iter().any(|&(k, _)| longer(kw, k)) || hits.iter().any(|&h| longer(kw, h)) {
                continue;
            }
            hits.push(kw);
            score += w;
        }
        let engaged = hits.iter().any(|h| is_hard(h))
            || (score >= 3 && hits.iter().any(|h| keyword(h).map_or(0, |k| k.weight) >= 2));
        (engaged, score, hits)
    }

    const NORMAL: &[(&str, &str)] = &[
        ("ro", "Bună ziua, banca vă informează că transferul de bani în contul dumneavoastră a fost efectuat urgent, cum ați cerut"),
        ("ro", "Bună ziua, curierul vine azi după-amiază, pregătiți banii, plata se face numerar sau cu cardul la livrare"),
        ("ro", "Bună ziua, clinica vă roagă să veniți urgent mâine dimineață, medicul vrea să discute rezultatele"),
        ("ro", "Bună seara, sunt proprietarul, chiria o puteți da omului meu care trece sâmbătă, ca să nu mai faceți drumul"),
        ("md", "Bună ziua, vă sunăm de la bancă, vă rugăm să ne spuneți dacă ați primit codul de activare a cardului nou prin poștă"),
        ("md", "Bună ziua, Nova Poshta, curierul vine mâine, dați banii pentru livrare direct curierului"),
        ("md", "Bună ziua, policlinica, fiul dumneavoastră a căzut la sport, nu e grav, veniți să-l luați ca să nu stea singur"),
        ("md", "Bună ziua, sunt proprietara, dă bani pentru gaz vecinului meu, el plătește factura mâine"),
        ("ua", "Добрий день, Приватбанк, кошти за вашим переказом успішно зараховано на картку"),
        ("ua", "Добрий день, кур'єр Нової пошти, оплата при отриманні, можна готівкою або на картку"),
        ("ua", "Добрий день, клініка, будь ласка, терміново зателефонуйте лікарю щодо аналізів"),
        ("ua", "Привіт, це хазяїн квартири, у мене нова картка, переведи гроші за оренду на цю картку до п'ятниці"),
        ("ua", "Добрий день, це власниця, терміново переведи гроші за комуналку, бо завтра відключать світло"),
        ("ua", "Добрий день, школа, батьківський комітет збирає гроші на екскурсію, можна переказати на картку класного керівника"),
    ];

    #[test]
    fn ordinary_calls_near_new_phrases_stay_quiet() {
        let mut bad = Vec::new();
        for (iso, t) in NORMAL {
            let (e, s, h) = live(t, false);
            let (e0, _, _) = live(t, true);
            let new_hits: Vec<&str> = keyword_hits(t).iter().filter(|h| is_new(h)).map(|h| h.text).collect();
            println!("RECHECK [{iso}] engaged={e} without_new={e0} score={s} hits={h:?} new={new_hits:?}");
            if e {
                bad.push(format!("[{iso}] engaged (without the new words: {e0}) score={s} hits={h:?}: {t}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Fourth batch: European countries (Belgium, Luxembourg, the Czech Republic,
/// Slovakia, Hungary, Bulgaria, Greece, Cyprus, Croatia, Serbia, Bosnia,
/// Montenegro, Slovenia, North Macedonia, Albania, Kosovo, Sweden, Norway,
/// Denmark, Finland, Iceland) and the scam types the older European countries
/// were missing. Run through the live engage rule.
#[cfg(test)]
mod added_countries_4_tests {
    use super::*;

    const NEW_COUNTRIES: [&str; 21] = ["be", "lu", "cz", "sk", "hu", "bg", "gr", "cy", "hr", "rs", "ba", "me", "si", "mk", "al", "xk", "se", "no", "dk", "fi", "is"];
    const NEW_LANGS: [&str; 15] = ["sv", "nb", "da", "fi", "is", "cs", "sk", "hu", "bg", "el", "hr", "sr", "sl", "mk", "sq"];

    /// gemini_live::stall_score_add + stall_engages, step by step: a keyword inside a
    /// longer one heard with it counts once, as the longer one.
    fn live(text: &str) -> (bool, u32, Vec<&'static str>) {
        let found: Vec<(&'static str, u32)> = keyword_hits(text).into_iter().map(|h| (h.text, h.weight)).collect();
        let longer = |kw: &str, k: &str| k.len() > kw.len() && k.contains(kw);
        let mut hits: Vec<&'static str> = Vec::new();
        let mut score = 0;
        for &(kw, w) in &found {
            if hits.contains(&kw) || found.iter().any(|&(k, _)| longer(kw, k)) || hits.iter().any(|&h| longer(kw, h)) {
                continue;
            }
            hits.push(kw);
            score += w;
        }
        let engaged = hits.iter().any(|h| is_hard(h)) || (score >= 3 && hits.iter().any(|h| keyword(h).map_or(0, |k| k.weight) >= 2));
        (engaged, score, hits)
    }

    fn from_country(iso: &str, text: &str) -> bool {
        keyword_hits(text).iter().any(|h| {
            db().country_kw_map
                .get(&normalized_hit_key(h.text))
                .is_some_and(|pairs| pairs.iter().any(|(i, _)| i == iso))
        })
    }

    type Row = (&'static str, &'static [&'static str], &'static [&'static str]);

    /// Country, typical scams in its language, ordinary calls (bank, courier, clinic).
    const ROWS: &[Row] = &[
        (
            "be",
            &[
                "Goedendag, hier Card Stop, we zagen een verdachte betaling, geef me de code van uw kaartlezer en zet het geld op een kluisrekening",
                "U moet nu bevestig in de itsme-app drukken en mij uw itsme-code geven, anders wordt uw toegang geblokkeerd",
                "Bonjour, ici Card Stop, donnez-moi le code de votre lecteur de carte, un coursier viendra chercher votre carte bancaire",
            ],
            &[
                "Goedendag, met de bank, uw nieuwe bankkaart ligt klaar in het kantoor, u kunt ze ophalen met uw identiteitskaart",
                "Hallo, met de koerier van bpost, ik sta morgen tussen tien en twaalf bij u met een pakje",
                "Goedemiddag, de praktijk van de huisarts, we willen uw afspraak van donderdag verzetten naar vrijdag",
            ],
        ),
        (
            "lu",
            &[
                "Bonjour, service de sécurité, vous devez confirmer dans l'application LuxTrust et remettre votre carte bancaire au coursier",
                "Bonjour, votre ordinateur infecté envoie des alertes, installez le logiciel de téléassistance pour qu'on puisse prendre la main sur votre ordinateur",
            ],
            &[
                "Bonjour, votre banque, votre nouvelle carte est arrivée à l'agence, vous pouvez passer la chercher",
                "Bonjour, le cabinet médical, nous vous rappelons votre rendez-vous de mardi à neuf heures",
            ],
        ),
        (
            "cz",
            &[
                "Dobrý den, tady bezpečnostní oddělení vaší banky, váš napadený účet musíme zachránit, převeďte peníze na bezpečný účet",
                "Jsem investiční poradce, pošlete peníze do kryptoměnové peněženky a pak vám pošleme výplatu zisku",
                "Dobrý den, Portál občana, máte nezaplacenou pokutu, musíte potvrdit bankovní identitu přes odkaz",
            ],
            &[
                "Dobrý den, tady vaše banka, nová platební karta je připravená na pobočce, můžete si ji vyzvednout s občankou",
                "Dobrý den, kurýr, mám pro vás balík, budete zítra dopoledne doma",
                "Dobrý den, ordinace praktického lékaře, chceme vám přesunout termín ze čtvrtka na pátek",
            ],
        ),
        (
            "sk",
            &[
                "Dobrý deň, tu kriminálna polícia, vediem tajné vyšetrovanie, peniaze musíte presunúť na vyšetrovací účet",
                "Volám z Národnej banky Slovenska, ide o záchranu peňazí, povedzte mi overovací kód a presuňte peniaze na účet NBS",
                "Babka, mal som dopravnú nehodu, musíte pripraviť hotovosť, príde kolega pre peniaze",
            ],
            &[
                "Dobrý deň, tu vaša banka, nová platobná karta je pripravená na pobočke",
                "Dobrý deň, kuriér, mám pre vás balík, budete zajtra doma",
                "Dobrý deň, ambulancia, výsledky vašich krvných testov sú hotové",
            ],
        ),
        (
            "hu",
            &[
                "Jó napot, a bankbiztonsági osztálytól hívom, zárolják a számláját, utalja át a pénzt a biztonsági számlára",
                "A bűnügyi nyomozó beszél, titkos nyomozás folyik, titoktartási kötelezettsége van, vegye fel a készpénzt",
                "Mama, autóbaleset volt, kell az óvadék, küldök egy ismerőst a pénzért, ne szólj a szülőknek",
            ],
            &[
                "Jó napot, a bankból hívom, az új bankkártyája megérkezett a fiókba, átveheti személyivel",
                "Jó napot, futár vagyok, holnap délelőtt hozom a csomagját, otthon lesz",
                "Jó napot, a háziorvosi rendelőből hívom, a vérvétel eredménye megjött",
            ],
        ),
        (
            "bg",
            &[
                "Обаждам се от полицията, тече специализирана акция за залавяне на измамници, хвърлете парите от терасата",
                "Синът ви е в тежка катастрофа, трябват пари за златни пирони, предайте парите на санитаря",
                "Обаждам се от отдел сигурност на банката, имаше опит за теглене на кредит, прехвърлете сумата по защитена сметка",
            ],
            &[
                "Здравейте, от банката сме, новата ви карта е готова в клона",
                "Здравейте, куриер съм, утре сутринта ще ви донеса пратката",
                "Здравейте, от личния лекар, резултатите от изследванията са готови",
            ],
        ),
        (
            "gr",
            &[
                "Καλημέρα, είμαι από το λογιστικό γραφείο, για το επίδομα δώστε μου τους κωδικούς e-banking και τον κωδικό OTP",
                "Σας παίρνω από την αστυνομία, πρέπει να παραδώσετε τα τιμαλφή για φύλαξη, αφήστε τα κοσμήματα στην είσοδο",
                "Από τη ΔΕΔΔΗΕ, έχετε διαρροή ρεύματος, τυλίξτε τα τιμαλφή σε αλουμινόχαρτο και αφήστε τα στην πόρτα",
            ],
            &[
                "Καλημέρα, από την τράπεζα, η νέα σας κάρτα έφτασε στο κατάστημα",
                "Γεια σας, κούριερ, θα φέρω το δέμα σας αύριο το πρωί",
                "Καλημέρα, από το ιατρείο, σας θυμίζουμε το ραντεβού της Πέμπτης στις δέκα",
            ],
        ),
        (
            "cy",
            &[
                "Ο γιος σας είχε τροχαίο ατύχημα, χρειάζονται μετρητά για το χειρουργείο, θα έρθει συνεργάτης να παραλάβει τα χρήματα",
            ],
            &[
                "Καλησπέρα, από την τράπεζα, η κάρτα σας ανανεώθηκε και θα σας σταλεί ταχυδρομικά",
            ],
        ),
        (
            "hr",
            &[
                "Dobar dan, policija, kruže krivotvorene novčanice, predajte novčanice službeniku u civilu radi provjere otisaka",
                "Ovdje liječnik, vaš sin je u teškoj prometnoj nesreći, pripremite gotovinu ili zlato, poslat ćemo posrednika",
                "Služba za sigurnost banke, istek aplikacije je danas, pročitajte kod koji generira vaš m-token",
            ],
            &[
                "Dobar dan, vaša banka, nova kartica je stigla u poslovnicu",
                "Dobar dan, dostavljač, sutra ujutro donosim vaš paket",
                "Dobar dan, ordinacija obiteljske medicine, nalazi su gotovi",
            ],
        ),
        (
            "rs",
            &[
                "Dobar dan, Pošta Srbije, paket čeka, unesite broj kartice i CVV kod na link za potvrdu adrese",
                "Ovde bezbednost banke, vaši nagradni poeni ističu, recite mi jednokratni kod i pošaljite fotografiju lične karte",
                "Ćao, novi broj, hitno prebaci novac, uplata na tuđi račun je samo do sutra",
            ],
            &[
                "Dobar dan, vaša banka, nova kartica je stigla u ekspozituru",
                "Dobar dan, kurir, sutra pre podne donosim paket",
                "Dobar dan, dom zdravlja, nalazi su gotovi",
            ],
        ),
        (
            "ba",
            &[
                "Dobar dan, BH pošta, za paket nam treba CVV kod preko linka za potvrdu adrese",
            ],
            &[
                "Dobar dan, banka, vaša nova kartica čeka u poslovnici",
            ],
        ),
        (
            "me",
            &[
                "Investicioni savetnik ovde, vaš kripto novčanik ima zaradu, potrebna je uplata za otključavanje profita",
            ],
            &[
                "Dobar dan, Pošta Crne Gore, paket stiže sjutra prije podne",
            ],
        ),
        (
            "si",
            &[
                "Dober dan, vaši pozabljeni bitcoini čakajo, potrebujemo dostop do spletne banke in sken osebnega dokumenta",
                "Varnostna služba banke, sumljivo nakazilo, povejte mi geslo za spletno banko za potrditev transakcije",
                "Pošta Slovenije, za paket je doplačilo poštnine, vpišite varnostno kodo CVV, varnostna koda CVV je na hrbtni strani",
            ],
            &[
                "Dober dan, vaša banka, nova kartica je prispela v poslovalnico",
                "Dober dan, dostava, jutri dopoldne vam prinesem paket",
                "Dober dan, ambulanta, izvidi so pripravljeni",
            ],
        ),
        (
            "mk",
            &[
                "Ве бара лекар, вашиот син имаше сообраќајна несреќа, треба пари за импланти, човек ќе дојде пред зграда",
                "Од Народна банка, ни треба број на платежна картичка и безбедносен код од смс",
                "Од инвестициска платформа, за исплата на добивка потребна е уплата на провизија",
            ],
            &[
                "Добар ден, од банката, новата картичка е подготвена во филијалата",
                "Добар ден, курир, утре наутро ви го носам пакетот",
                "Добар ден, од амбулантата, резултатите се готови",
            ],
        ),
        (
            "al",
            &[
                "Përshëndetje, nga banka, për përditësim i të dhënave na duhet kodi OTP dhe fjalëkalimi i e-banking",
                "Nga platformë tregtimi, depozitoni shumën fillestare dhe pastaj paguani taksën për të tërhequr fitimin",
            ],
            &[
                "Përshëndetje, nga banka, karta juaj e re ka mbërritur në degë",
                "Përshëndetje, korrieri, nesër në mëngjes ju sjell pakon",
                "Përshëndetje, nga klinika, rezultatet e analizave janë gati",
            ],
        ),
        (
            "xk",
            &[
                "Policia, keni një gjobë trafiku, duhet të paguar gjobën përmes linkut ose të dhënat e kartës në telefon",
            ],
            &[
                "Përshëndetje, nga banka, karta juaj e re është gati",
            ],
        ),
        (
            "se",
            &[
                "Hej, säkerhetsavdelningen på banken, du måste logga in med Mobilt BankID och läsa upp koden från bankdosan",
                "Polisen här, det är en inbrottsvåg, en kollega hämtar ditt bankkort och tar guld och smycken i förvar",
                "Du får sänkt månadskostnad, men ditt nummer stängs av idag om du inte vill signera avtalet direkt med BankID",
            ],
            &[
                "Hej, det är från banken, ditt nya bankkort har skickats och kommer med posten i veckan",
                "Hej, det är budet, jag kommer med paketet i morgon förmiddag",
                "Hej, det är vårdcentralen, vi behöver flytta din tid på torsdag till fredag",
            ],
        ),
        (
            "no",
            &[
                "Hei, dette er Økokrim, oppgi BankID-passord og engangskode, vi flytter pengene til en trygg konto",
                "Teknisk support her, gi oss fjerntilgang til PC-en din og logg inn i nettbanken mens vi ser på",
                "Fra Skatteetaten om skatteoppgjør, klikk på lenken og bekreft med BankID, eller oppgi kortnummer og sikkerhetskode",
            ],
            &[
                "Hei, det er banken, det nye bankkortet ditt er sendt i posten",
                "Hei, det er budet, jeg kommer med pakken i morgen formiddag",
                "Hei, det er legekontoret, vi må flytte timen din på torsdag",
            ],
        ),
        (
            "dk",
            &[
                "Hej, sikkerhedsafdelingen i banken, du skal godkend med MitID og flytte pengene til en sikkerhedskonto",
                "Politiet her om identitetsmisbrug, giv mig dine kortoplysninger og godkend MitID for politiet",
                "Teknisk afdeling, hent et program så vi kan overtage kontrollen med din computer",
            ],
            &[
                "Hej, det er banken, dit nye betalingskort er sendt med posten",
                "Hej, det er budet, jeg kommer med pakken i morgen formiddag",
                "Hej, det er lægen, vi skal flytte din tid på torsdag",
            ],
        ),
        (
            "fi",
            &[
                "Hei, pankin turvallisuusosasto, kerro pankkitunnuksesi ja vahvistuskoodi, siirrämme rahat turvatilille",
                "Poliisi, identiteettivarkaus, tarvitsen verkkopankkitunnuksesi ja kortin tiedot puhelimessa",
                "Tekninen tuki, asenna ohjelma, etähallintaohjelma korjaa viruksen",
            ],
            &[
                "Hei, täällä pankista, uusi pankkikorttisi on lähetetty postissa",
                "Hei, kuriiri tässä, tuon paketin huomenna aamupäivällä",
                "Hei, terveysasemalta, siirretään torstain aikanne perjantaille",
            ],
        ),
        (
            "is",
            &[
                "Halló, öryggisdeild bankans, þú þarft að samþykkja rafræn skilríki í Auðkennisappið og gefa öryggisnúmer",
                "Tæknileg aðstoð, þú þarft að setja upp forrit svo við fáum fjaraðgang að tölvunni",
                "Þú átt óinnleyst inneign á viðskiptareikningur, skráðu þig, skrá þig inn í heimabanka til að fá hana",
            ],
            &[
                "Halló, þetta er frá bankanum, nýja kortið þitt er komið í útibúið",
                "Halló, sendill hér, ég kem með pakkann á morgun fyrir hádegi",
                "Halló, heilsugæslan, við þurfum að færa tímann þinn á fimmtudag",
            ],
        ),
        (
            "de",
            &[
                "Hier ist die Kriminalpolizei, ein Zivilbeamter holt heute Ihr Bargeld, es gilt strengste Geheimhaltung",
            ],
            &[
                "Guten Tag, Ihre Bank, die neue Karte liegt in der Filiale zur Abholung bereit",
            ],
        ),
        (
            "at",
            &[
                "Kriminalpolizei, Ihr Sohn ist verhaftet, Sie müssen die Kaution sofort bezahlen, ein Zivilbeamter holt das Geld",
            ],
            &[
                "Grüß Gott, Ihre Bank, Ihre neue Karte ist in der Filiale angekommen",
            ],
        ),
        (
            "ch",
            &[
                "Microsoft-Support hier, Ihr Computer ist infiziert, bitte AnyDesk installieren und TeamViewer öffnen",
            ],
            &[
                "Grüezi, Ihre Bank, die neue Karte ist unterwegs",
            ],
        ),
        (
            "fr",
            &[
                "Bonjour, Assurance maladie, il faut renouveler votre carte Vitale, donnez-moi vos identifiants ameli",
            ],
            &[
                "Bonjour, votre pharmacie, votre commande est prête",
            ],
        ),
        (
            "nl",
            &[
                "Hallo, helpdesk van Microsoft, installeer AnyDesk of TeamViewer zodat we kunnen meekijken op uw computer",
            ],
            &[
                "Goedendag, uw bank, de nieuwe pas is onderweg",
            ],
        ),
        (
            "pl",
            &[
                "Dzień dobry, Zakład Ubezpieczeń Społecznych, należy się pani dodatek, potrzebna jest opłata weryfikacyjna i dane logowania do banku",
            ],
            &[
                "Dzień dobry, kurier, będę jutro rano z paczką",
            ],
        ),
        (
            "ro",
            &[
                "Bună ziua, de la platformă de tranzacționare, câștig rapid garantat, dar pentru bani plătiți o taxă de retragere",
            ],
            &[
                "Bună ziua, curierul, ajung mâine dimineață cu coletul",
            ],
        ),
        (
            "md",
            &[
                "Serviciul de securitate al băncii, mutați banii în cont de siguranță și instalați aplicație de acces la distanță",
            ],
            &[
                "Bună ziua, de la policlinică, analizele sunt gata",
            ],
        ),
        (
            "it",
            &[
                "Buongiorno, per la nuova offerta luce mi legge il codice POD e poi dobbiamo registrare il consenso",
            ],
            &[
                "Buongiorno, la farmacia, il suo ordine è pronto",
            ],
        ),
        (
            "pt",
            &[
                "Olá mãe, este é o meu número novo, o meu telemóvel avariou, preciso de um pagamento urgente",
            ],
            &[
                "Bom dia, a farmácia, a sua encomenda já chegou",
            ],
        ),
        (
            "es",
            &[
                "Le llamamos de soporte, su ordenador envía alertas de virus, instale este programa de control remoto",
            ],
            &[
                "Buenos días, la farmacia, su pedido ya está listo",
            ],
        ),
        (
            "gb",
            &[
                "This is HMRC, there is an immediate arrest warrant in your name, we will send bailiffs to your address",
            ],
            &[
                "Hello, it's the pharmacy, your prescription is ready to collect",
            ],
        ),
        (
            "ua",
            &[
                "Добрий день, ваш оператор, щоб перейти на нову eSIM, назвіть код від оператора",
            ],
            &[
                "Добрий день, аптека, ваше замовлення готове",
            ],
        ),
    ];

    #[test]
    fn every_new_country_has_a_profile_in_its_languages() {
        for iso in NEW_COUNTRIES {
            let cp = db().countries.get(iso).unwrap_or_else(|| panic!("missing {iso}"));
            assert!(!cp.scams.is_empty(), "{iso}");
            for sc in &cp.scams {
                assert!(cp.languages.contains(&sc.lang), "{iso}/{}: {} not among {:?}", sc.id, sc.lang, cp.languages);
            }
        }
        for lang in NEW_LANGS {
            assert!(db().languages().contains(&lang), "{lang} has no keywords");
        }
    }

    #[test]
    fn scams_are_caught_by_their_own_countrys_words() {
        let mut bad = Vec::new();
        for (iso, scams, _) in ROWS {
            for t in scams.iter() {
                let (e, s, h) = live(t);
                if !e || !from_country(iso, t) {
                    bad.push(format!("{iso}: scam not caught (engaged={e} score={s} hits={h:?}): {t}"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn ordinary_bank_courier_and_clinic_calls_stay_quiet() {
        let mut bad = Vec::new();
        for (iso, _, ordinary) in ROWS {
            for t in ordinary.iter() {
                let (e, s, h) = live(t);
                // A post office or a bank may name itself (a weight-1 context word); no
                // weight-2 country word may turn up.
                let strong = keyword_hits(t).iter().any(|k| k.weight >= 2 && from_country(iso, k.text));
                if e || strong {
                    bad.push(format!("{iso}: ordinary call hit (engaged={e} score={s} hits={h:?}): {t}"));
                }
            }
            let all = ordinary.join(" ");
            let (e, s, h) = live(&all);
            if e {
                bad.push(format!("{iso}: ordinary calls together engaged (score={s} hits={h:?})"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Country words are weight 2 at most and never scam-only on their own; the
    /// scam-only words of the new languages live in data/scam_db.json.
    #[test]
    fn new_country_keywords_are_weight_two_at_most_and_never_hard() {
        let mut bad = Vec::new();
        for iso in NEW_COUNTRIES {
            for sc in &db().countries[iso].scams {
                for rf in &sc.red_flag_keywords {
                    if rf.hard || rf.weight != 2 {
                        bad.push(format!("{iso}/{}: {:?} weight {} hard {}", sc.id, rf.text, rf.weight, rf.hard));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Each new language: a scam-only demand engages at once; a common word of
    /// the same field (bank, parcel, clinic) does not, alone or together.
    #[test]
    fn new_language_lists_catch_demands_but_not_everyday_words() {
        let demands: &[(&str, &str)] = &[
            ("sv", "Flytta pengarna till ett säkerhetskonto nu"), ("sv", "Läs upp din verifieringskod för mig"),
            ("nb", "Vi flytter pengene til en trygg konto"), ("nb", "Oppgi engangskode fra BankID"),
            ("da", "Flyt pengene til en sikkerhedskonto"), ("da", "Læs din engangskode op for mig"),
            ("fi", "Siirrä rahat turvatilille heti"), ("fi", "Kerro minulle vahvistuskoodi"),
            ("is", "Færðu peningana á öryggisreikning"), ("is", "Lestu upp staðfestingarkóðann"),
            ("cs", "Převeďte peníze na bezpečný účet"), ("cs", "Řekněte mi ověřovací kód"),
            ("sk", "Presuňte peniaze na bezpečný účet"), ("sk", "Povedzte mi overovací kód"),
            ("hu", "Utalja át a pénzt a biztonsági számlára"), ("hu", "Mondja meg a megerősítő kódot"),
            ("bg", "Прехвърлете парите по защитена сметка"), ("bg", "Продиктувайте кода от съобщението"),
            ("el", "Μεταφέρετε τα χρήματα σε ασφαλή λογαριασμό"), ("el", "Πείτε μου τον κωδικό OTP"),
            ("hr", "Prebacite novac na siguran račun"), ("hr", "Recite mi jednokratni kod"),
            ("sr", "Prebacite novac na bezbedan račun"), ("sr", "Реците ми једнократни код"),
            ("sl", "Denar nakažite na varen račun"), ("sl", "Povejte mi enkratno kodo"),
            ("mk", "Префрлете ги парите на безбедна сметка"), ("mk", "Кажете ми го кодот од смс"),
            ("sq", "Kaloni paratë në një llogari të sigurt"), ("sq", "Më thoni kodin OTP"),
        ];
        let everyday: &[(&str, &str)] = &[
            ("sv", "Banken ringer om ditt bankkonto, paketet kommer i morgon, din tid på vårdcentralen är flyttad"),
            ("nb", "Banken ringer om bankkontoen din, pakken kommer i morgen, timen hos legen er flyttet"),
            ("da", "Banken ringer om din bankkonto, pakken kommer i morgen, din tid hos lægen er flyttet"),
            ("fi", "Pankista soitetaan pankkitilisi takia, paketti tulee huomenna, lääkäriaika siirtyi"),
            ("is", "Bankinn hringir út af bankareikninginn þinn, pakkinn kemur á morgun, tíminn hjá lækninum er færður"),
            ("cs", "Volá banka kvůli váš bankovní účet, balík přijde zítra, termín u lékaře se posouvá"),
            ("sk", "Volá banka kvôli váš bankový účet, balík príde zajtra, termín u lekára sa posúva"),
            ("hu", "A bank hív az ön bankszámlája miatt, a csomag holnap jön, az orvosi időpont változott"),
            ("bg", "Банката се обажда за вашата банкова сметка, пратката идва утре, часът при лекаря е преместен"),
            ("el", "Η τράπεζα καλεί για τον τραπεζικό σας λογαριασμό, το δέμα έρχεται αύριο, το ραντεβού στον γιατρό άλλαξε"),
            ("hr", "Banka zove zbog vaš bankovni račun, paket stiže sutra, termin kod liječnika je pomaknut"),
            ("sr", "Banka zove zbog vaš bankovni račun, paket stiže sutra, termin kod lekara je pomeren"),
            ("sl", "Banka kliče glede vaš bančni račun, paket pride jutri, termin pri zdravniku je prestavljen"),
            ("mk", "Банката се јавува за вашата банкарска сметка, пакетот доаѓа утре, терминот кај докторот е поместен"),
            ("sq", "Banka telefonon për llogarinë tuaj bankare, pakoja vjen nesër, takimi te mjeku u shty"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in demands {
            let (e, s, h) = live(t);
            if !e || !h.iter().any(|x| is_hard(x)) {
                bad.push(format!("[{lang}] demand missed (score={s} hits={h:?}): {t}"));
            }
        }
        for (lang, t) in everyday {
            let (e, s, h) = live(t);
            if e {
                bad.push(format!("[{lang}] everyday words engaged (score={s} hits={h:?}): {t}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The new words must not change how the core languages' ordinary calls score.
    #[test]
    fn core_language_ordinary_calls_stay_quiet() {
        let calls = [
            "您好，這裡是銀行，您的新卡已經寄出，請注意查收",
            "你好，快递到了，放门口可以吗",
            "Hi, this is the clinic, your appointment moved to Friday, and your insurance policies are on file",
            "Hola, le llamo del banco, su tarjeta nueva ya está en la oficina, la policía local cortó la calle",
            "Bonjour, la pharmacie, votre commande est prête, la police municipale a fermé la rue",
            "Guten Tag, Ihre Bank, die neue Karte ist da, die Polizei hat die Straße gesperrt",
            "Olá, é da clínica, a sua consulta passou para sexta, a polícia fechou a rua",
            "もしもし、病院です、予約が金曜日に変わりました",
            "여보세요, 택배입니다, 내일 오전에 도착합니다",
        ];
        let mut bad = Vec::new();
        for t in calls {
            let (e, s, h) = live(t);
            if e {
                bad.push(format!("engaged (score={s} hits={h:?}): {t}"));
            }
            if keyword_hits(t).iter().any(|k| NEW_LANGS.contains(&db().keyword(k.text).map_or("", |kw| kw.lang.as_str()))) {
                bad.push(format!("a new-language word heard in: {t} {h:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

/// Fifth batch: Asia, the Middle East, Africa, Latin America and the Baltics (Singapore,
/// Malaysia, the Philippines, New Zealand, Israel, Mongolia, Myanmar, Laos, Bangladesh,
/// Pakistan, Nepal, Sri Lanka, Afghanistan, Ireland, the Spanish-speaking Americas, Uruguay,
/// Belarus, Estonia, Latvia, Lithuania, the Levant, the Maghreb, Iran, Azerbaijan, Armenia,
/// Georgia, South Africa, East Africa, Haiti, Madagascar, Macao and the Gulf) and the scam
/// types a few listed countries were missing. Run through the live engage rule.
#[cfg(test)]
mod added_countries_5_tests {
    use super::*;

    const NEW_COUNTRIES: [&str; 58] = [
        "sg", "my", "ph", "nz", "il", "mn", "mm", "la", "bd", "pk", "np", "lk", "af", "ie", "co", "pe", "cl", "ec", "ve", "bo",
        "py", "gt", "hn", "sv", "ni", "cr", "pa", "cu", "do", "pr", "uy", "by", "ee", "lv", "lt", "lb", "jo", "sy", "ps", "iq",
        "ma", "dz", "tn", "ir", "az", "am", "ge", "za", "ke", "tz", "et", "mg", "ht", "mo", "qa", "kw", "bh", "om",
    ];
    const NEW_LANGS: [&str; 19] =
        ["he", "ur", "ne", "si", "my", "lo", "mn", "fa", "ps", "az", "hy", "ka", "sw", "am", "af", "et", "lv", "lt", "be"];

    /// gemini_live::stall_score_add + stall_engages, step by step: a keyword inside a
    /// longer one heard with it counts once, as the longer one.
    fn live(text: &str) -> (bool, u32, Vec<&'static str>) {
        let found: Vec<(&'static str, u32)> = keyword_hits(text).into_iter().map(|h| (h.text, h.weight)).collect();
        let longer = |kw: &str, k: &str| k.len() > kw.len() && k.contains(kw);
        let mut hits: Vec<&'static str> = Vec::new();
        let mut score = 0;
        for &(kw, w) in &found {
            if hits.contains(&kw) || found.iter().any(|&(k, _)| longer(kw, k)) || hits.iter().any(|&h| longer(kw, h)) {
                continue;
            }
            hits.push(kw);
            score += w;
        }
        let engaged = hits.iter().any(|h| is_hard(h)) || (score >= 3 && hits.iter().any(|h| keyword(h).map_or(0, |k| k.weight) >= 2));
        (engaged, score, hits)
    }

    fn from_country(iso: &str, text: &str) -> bool {
        keyword_hits(text).iter().any(|h| {
            db().country_kw_map
                .get(&normalized_hit_key(h.text))
                .is_some_and(|pairs| pairs.iter().any(|(i, _)| i == iso))
        })
    }

    type Row = (&'static str, &'static [&'static str], &'static [&'static str]);

    /// Country, typical scams in its language, ordinary calls (bank, courier, clinic).
    const ROWS: &[Row] = &[
        (
            "il",
            &[
                "שלום, מדברים ממחלקת הביטחון של הבנק, זוהתה פעולה חשודה, תקריא לי את קוד האימות במסרון",
                "כאן משטרת ישראל, היה ניסיון פריצה לחשבון, צריך להוריד אפליקציה לתמיכה מרחוק ואסור לספר לאף אחד",
                "החבילה שלך מעוכבת בבדיקת מכס, תן לי מספר כרטיס אשראי ושלוש ספרות כדי לשחרר אותה",
            ],
            &[
                "שלום, מדברים מהבנק, הכרטיס החדש שלך מחכה בסניף, אפשר לאסוף אותו עם תעודת זהות",
                "היי, זה השליח, אני מגיע מחר בין עשר לשתים עשרה עם החבילה",
                "שלום, מהמרפאה, רצינו להזיז את התור שלך מיום חמישי ליום שישי",
            ],
        ),
        (
            "my",
            &[
                "Saya pegawai polis, ada waran tangkap atas nama encik, pindahkan wang untuk audit ke akaun selamat",
                "Ini bahagian keselamatan bank, ada transaksi mencurigakan, sila beri kod pengesahan transaksi dan nombor PIN",
                "Bungkusan encik ditahan kastam, ada dokumen haram dalam bungkusan, saya sambungkan talian ke balai polis",
            ],
            &[
                "Selamat pagi, ini dari bank, kad debit baru encik sudah sampai di cawangan, boleh ambil dengan kad pengenalan",
                "Hello encik, saya kurier, bungkusan sampai esok pagi, ada orang di rumah tak",
                "Selamat petang, ini klinik, temujanji encik hari Khamis kami tukar ke hari Jumaat",
            ],
        ),
        (
            "ph",
            &[
                "Hi sir, this is your bank, there is an unauthorized charge, please read out the OTP and the CVV number",
                "This is the telecommunications commission, we will deactivate your SIM today unless you settle a clearance fee",
                "Para ma-unlock higher tasks, kailangan mo munang mag top-up deposit at send funds first",
            ],
            &[
                "Hi po, this is the bank, your new debit card is ready at the branch, you can pick it up any time",
                "Hello sir, rider po ito, nandito na ako sa labas ng gate niyo with your parcel",
                "Good morning, this is the clinic, your check-up on Thursday is moved to Friday",
            ],
        ),
        (
            "bd",
            &[
                "আমি মোবাইল ব্যাংকিং কাস্টমার কেয়ার থেকে বলছি, আপনার পিন নম্বর বলুন, না হলে অ্যাকাউন্ট লক হয়ে যাবে",
                "আপনার নামে মামলা হয়েছে, মামলা নিষ্পত্তি করতে টাকা লাগবে, এখনই টাকা পাঠান",
                "আপনি লটারি জিতেছেন, পুরস্কারের টাকা পেতে ট্যাক্সের টাকা আগে পাঠান",
            ],
            &[
                "আসসালামু আলাইকুম, ব্যাংক থেকে বলছি, আপনার নতুন কার্ড শাখায় এসেছে, এসে নিয়ে যাবেন",
                "ভাই, কুরিয়ার থেকে বলছি, আপনার পার্সেল কাল সকালে পৌঁছাবে",
                "নমস্কার, ক্লিনিক থেকে বলছি, আপনার বৃহস্পতিবারের অ্যাপয়েন্টমেন্ট শুক্রবারে সরানো হয়েছে",
            ],
        ),
        (
            "pk",
            &[
                "سرکاری امداد کی نئی قسط آئی ہے، پہلے رجسٹریشن فیس جمع کروائیں اور جو کوڈ آیا ہے وہ بتائیں",
                "میں ٹیلی کام اتھارٹی سے بات کر رہا ہوں، پانچ منٹ میں سم بند ہو جائے گی، او ٹی پی کوڈ بتائیں",
                "بینک ہیلپ لائن سے بات ہو رہی ہے، مشکوک ٹرانزیکشن ہوئی ہے، کارڈ کے پیچھے تین ہندسے اور اے ٹی ایم پن بتائیں",
            ],
            &[
                "السلام علیکم، بینک سے بات ہو رہی ہے، آپ کا نیا کارڈ برانچ میں آ گیا ہے، آ کر لے لیں",
                "جی، کورئیر والا بول رہا ہوں، آپ کا پارسل کل صبح پہنچ جائے گا",
                "السلام علیکم، کلینک سے بات ہو رہی ہے، آپ کی جمعرات والی اپوائنٹمنٹ جمعہ کو کر دی ہے",
            ],
        ),
        (
            "np",
            &[
                "तपाईंको केवाईसी अपडेट गर्नुपर्छ, ओटीपी कोड भन्नुहोस्, नत्र खाता बन्द हुन्छ",
                "विदेशबाट पार्सल आएको छ, भन्सार कर पठाउनुहोस्, नत्र पार्सल जफत हुन्छ",
                "घरमै बसेर काम छ, तपाईंको खाता भाडामा दिनुस्, कमिशन आउँछ",
            ],
            &[
                "नमस्ते, बैंकबाट बोलेको, तपाईंको नयाँ कार्ड शाखामा आइसकेको छ",
                "हजुर, कुरियरबाट बोलेको, तपाईंको सामान भोलि बिहान आइपुग्छ",
                "नमस्ते, क्लिनिकबाट बोलेको, बिहीबारको भेट शुक्रबार सारिएको छ",
            ],
        ),
        (
            "lk",
            &[
                "ඔබට රථවාහන දඩ මුදලක් තියෙනවා, වහාම දඩය ගෙවන්න, OTP අංකය කියන්න",
                "බැංකු ගිණුමේ සැකකටයුතු ගනුදෙනුවක් තියෙනවා, OTP කේතය දෙන්න",
                "ඔබේ තැපැල් පාර්සලය රඳවාගෙන තියෙනවා, ලින්ක් එකෙන් ගෙවන්න",
            ],
            &[
                "ආයුබෝවන්, බැංකුවෙන් කතා කරන්නේ, ඔබේ අලුත් කාඩ්පත ශාඛාවට ඇවිත් තියෙනවා",
                "හලෝ, කූරියර් එකෙන්, ඔබේ පාර්සලය හෙට උදේ ගෙනෙනවා",
                "ආයුබෝවන්, සායනයෙන්, බ්‍රහස්පතින්දා වෙලාව සිකුරාදාට මාරු කළා",
            ],
        ),
        (
            "mm",
            &[
                "ဘဏ်ဝန်ဆောင်မှုကပါ၊ အကောင့်အဆင့်မြှင့်ဖို့ OTP နံပါတ်ပြောပြပါ",
                "ကံစမ်းမဲပေါက်ပါတယ်၊ ဆုကြေးငွေရဖို့ ကြိုတင်ငွေလွှဲရမည်",
                "နေ့စဉ်အတိုးအမြတ်ရမည်၊ ငွေထုတ်ရန်အခွန်အရင်သွင်းပါ",
            ],
            &[
                "မင်္ဂလာပါ၊ ဘဏ်ကပါ၊ ကတ်အသစ်ရောက်နေပြီ၊ ဘဏ်ခွဲမှာလာယူလို့ရပါတယ်",
                "မင်္ဂလာပါ၊ ပါဆယ်ပို့တဲ့သူပါ၊ မနက်ဖြန်မနက်ရောက်မယ်",
                "မင်္ဂလာပါ၊ ဆေးခန်းကပါ၊ ကြာသပတေးနေ့ချိန်းကို သောကြာနေ့ပြောင်းထားပါတယ်",
            ],
        ),
        (
            "la",
            &[
                "ສະບາຍດີ, ຈາກທະນາຄານ, ບັນຊີຖືກລະງັບ, ກະລຸນາບອກລະຫັດ OTP",
                "ລູກເກີດອຸບັດຕິເຫດ, ໂອນເງິນມາດ່ວນດຽວນີ້, ຢ່າຟ້າວບອກໃຜ",
                "ທ່ານຖືກລາງວັນ, ໂອນຄ່າທຳນຽມກ່ອນ",
            ],
            &[
                "ສະບາຍດີ, ຈາກທະນາຄານ, ບັດໃໝ່ຂອງທ່ານມາຮອດສາຂາແລ້ວ",
                "ສະບາຍດີ, ຂົນສົ່ງ, ພັດສະດຸຈະຮອດມື້ອື່ນເຊົ້າ",
                "ສະບາຍດີ, ຄລີນິກ, ນັດວັນພະຫັດປ່ຽນເປັນວັນສຸກ",
            ],
        ),
        (
            "mn",
            &[
                "Сайн байна уу, утсанд очсон кодыг хэлээд өгөөч, баталгаажуулах дөрвөн оронтой код",
                "Та сугалаанд ялж азтан боллоо, шагналаа авахын тулд урьдчилгаа шимтгэл шилжүүл",
                "Дансаа түр ашиглуулаад урамшуулал ав, интернэт банкны нэвтрэх нэр өг",
            ],
            &[
                "Сайн байна уу, банкнаас залгаж байна, таны шинэ карт салбарт ирсэн",
                "Сайн байна уу, хүргэлтийн ажилтан байна, илгээмж маргааш өглөө очно",
                "Сайн байна уу, эмнэлгээс залгаж байна, пүрэв гарагийн цагийг баасан руу шилжүүллээ",
            ],
        ),
        (
            "ir",
            &[
                "سلام، از طرف سامانه کالابرگ، برای ثبت اعتراض رمز پویا و اطلاعات کارت بانکی رو بگید",
                "ابلاغیه قضایی دارید، حکم جلب صادر شده، هزینه دادرسی از طریق لینک رو پرداخت کنید",
                "شما برنده قرعه کشی شدید، کد تایید پیامکی را بخوانید",
            ],
            &[
                "سلام، از بانک تماس می‌گیرم، کارت جدیدتون به شعبه رسیده",
                "سلام، پیک هستم، بسته‌تون فردا صبح می‌رسه",
                "سلام، از مطب تماس می‌گیرم، نوبت پنجشنبه‌تون رو به جمعه منتقل کردیم",
            ],
        ),
        (
            "af",
            &[
                "مبارک شه، لویه جایزه مو ګټلې، د جایزې فیس او د کارت پټ کوډ راکړئ",
                "زه د مخابراتو له دفتر څخه یم، د پیغام کوډ ووایاست او پټ نمبر راکړئ",
            ],
            &[
                "السلام علیکم، له بانک څخه زنګ وهم، ستاسو نوی کارت څانګې ته راغلی",
                "سلام، زه د ډاکټر له دفتر څخه یم، ستاسو وخت جمعې ته بدل شو",
            ],
        ),
        (
            "az",
            &[
                "Bank kartınızda şübhəli əməliyyat var, OTP kodu və CVV kodu deyin",
                "Poçt bağlamanız saxlanılıb, kart məlumatlarını daxil edin və SMS təsdiq kodu göndərin",
                "Kart hesabınızı icarəyə verin, köçürmələrdən faiz qazanacaqsınız",
            ],
            &[
                "Salam, bankdan zəng edirik, yeni kartınız filiala çatıb",
                "Salam, kuryerəm, bağlamanız sabah səhər gələcək",
                "Salam, klinikadan zəng edirik, cümə axşamı növbənizi cüməyə keçirdik",
            ],
        ),
        (
            "am",
            &[
                "Բարև, օպերատորից ենք, 5G ակտիվացման համար պետք է էկրանի համօգտագործում միացնել և ասել հաստատման կոդ",
                "Կենտրոնական բանկի անվտանգության բաժնից ենք, գումարը փոխանցեք անվտանգ հաշիվ և ասեք մեկանգամյա գաղտնաբառ",
                "Երաշխավորված եկամուտ ամեն ամիս, շտապ ներդրում արեք",
            ],
            &[
                "Բարև ձեզ, բանկից ենք զանգում, ձեր նոր քարտը մասնաճյուղում է",
                "Բարև, առաքիչն եմ, ծանրոցը վաղը առավոտյան կբերեմ",
                "Բարև ձեզ, կլինիկայից ենք, հինգշաբթի օրվա այցը տեղափոխեցինք ուրբաթ",
            ],
        ),
        (
            "ge",
            &[
                "გამარჯობა, ბანკის წარმომადგენელი ვარ, საეჭვო ტრანზაქციაა, მითხარით ერთჯერადი კოდი და CVV კოდი",
                "ეროვნული ბანკიდან გირეკავთ, გადადით ბმულზე და შეიყვანეთ ავტორიზაციის მონაცემები",
                "საინვესტიციო პლატფორმაზე გარანტირებული მოგება და სწრაფი გამდიდრება",
            ],
            &[
                "გამარჯობა, ბანკიდან გირეკავთ, თქვენი ახალი ბარათი ფილიალშია",
                "გამარჯობა, კურიერი ვარ, ამანათს ხვალ დილით მოგიტანთ",
                "გამარჯობა, კლინიკიდან გირეკავთ, ხუთშაბათის ვიზიტი პარასკევზე გადავიტანეთ",
            ],
        ),
        (
            "ke",
            &[
                "Samahani, nimekosea namba, nimekutumia pesa kimakosa, rudisha pesa sasa hivi kwenye namba hii",
                "Ile fedha tuma kwenye namba hii, usitumie huduma rasmi ya kurudisha, ingiza namba ya siri",
            ],
            &[
                "Habari, ni benki, kadi yako mpya imefika tawini, unaweza kuja kuichukua",
                "Habari, mimi ni mletaji wa mizigo, kifurushi chako kitafika kesho asubuhi",
                "Habari, ni kliniki, miadi yako ya Alhamisi imehamishwa hadi Ijumaa",
            ],
        ),
        (
            "et",
            &[
                "ሰላም፣ ከባንክ ነው የምደውለው፣ አሁን የደረሰዎትን ኮድ ይንገሩኝ፣ አካውንትዎ እንዳይዘጋ",
                "ሚስጥር ቁጥርዎን ያረጋግጡ፣ ይህን መተግበሪያ ይጫኑ",
            ],
            &[
                "ሰላም፣ ከባንክ ነው፣ አዲሱ ካርድዎ ቅርንጫፍ ደርሷል",
                "ሰላም፣ ከክሊኒክ ነው፣ የሐሙስ ቀጠሮዎ ወደ ዓርብ ተቀይሯል",
            ],
        ),
        (
            "ee",
            &[
                "Tere, kinnitage PIN1 ja seejärel sisestage PIN2, muidu ei saa hüvitist",
                "Teie pangakonto on ohus, raha tuleb kanda turvakonto peale, ärge kellelegi rääkige",
                "Investeerimisplatvorm annab garanteeritud tulu, aga väljamakse tasu tuleb enne maksta",
            ],
            &[
                "Tere, pangast helistan, teie uus kaart on kontoris kättesaadav",
                "Tere, kuller siin, pakk tuleb homme hommikul",
                "Tere, kliinikust helistan, neljapäevane aeg tõsteti reedele",
            ],
        ),
        (
            "lv",
            &[
                "Labdien, jums ir sūtījums, lūdzu ievadiet PIN1 un apstipriniet ar PIN2",
                "Tas ir kriminālprocess, nododiet skaidru naudu kurjeram, ielieciet karti pakomātā",
                "Garantēta peļņa, tikai komisija par naudas izmaksu",
            ],
            &[
                "Labdien, zvanu no bankas, jūsu jaunā karte ir filiālē",
                "Labdien, kurjers, sūtījums būs rīt no rīta",
                "Labdien, no klīnikas, jūsu ceturtdienas vizīte pārcelta uz piektdienu",
            ],
        ),
        (
            "lt",
            &[
                "Laba diena, banke įtartina operacija, patvirtinkite PIN1 ir suveskite PIN2, niekam nepasakokite",
                "Močiute, avarija, skubiai reikia grynųjų, atvyks kurjeris paimti",
                "Garantuotas pelnas, o paskui pinigų susigrąžinimo tarnyba paprašys išankstinis mokestis už grąžinimą",
            ],
            &[
                "Laba diena, skambinu iš banko, jūsų nauja kortelė jau skyriuje",
                "Laba diena, kurjeris, siunta atvyks rytoj ryte",
                "Laba diena, iš klinikos, ketvirtadienio vizitas perkeltas į penktadienį",
            ],
        ),
        (
            "by",
            &[
                "Следственный комитет, вам нужно задекларировать наличные сбережения и передать деньги курьеру для проверки",
                "Приложите карточку к смартфону и введите пин-код, установите обновление приложения из мессенджера",
            ],
            &[
                "Добрый день, банк, ваша новая карточка в отделении",
                "Добрый день, поликлиника, ваш приём перенесли с четверга на пятницу",
            ],
        ),
        (
            "sg",
            &[
                "This is the monetary authority, your bank account frozen due to money laundering, transfer funds for investigation to a safe account",
                "Download remote access app and approve digital token, your device compromised",
            ],
            &[
                "Hi, calling from the bank, your new debit card is ready for collection at the branch",
                "Hello, delivery here, your parcel will reach tomorrow morning, anyone at home or not",
                "Hi, this is the clinic, your appointment on Thursday moved to Friday",
            ],
        ),
        (
            "nz",
            &[
                "Your account compromised, move your funds to a safe account and read out the verification code",
                "Mum, I dropped my phone, new mobile number, transfer money urgently to this account",
            ],
            &[
                "Kia ora, it's the bank, your new card's ready at the branch",
                "Hi, courier here, I'll drop the parcel off tomorrow morning",
            ],
        ),
        (
            "ie",
            &[
                "This is the fraud team, move your money to a safe account, a courier to collect your bank card is on the way",
                "There is an immediate arrest warrant, pay immediately by gift vouchers",
            ],
            &[
                "Hiya, it's the bank, your new card is in the branch for you",
                "Hello, An Post here, the parcel will be with you tomorrow",
            ],
        ),
        (
            "co",
            &[
                "Le hablamos del área de seguridad, deme la clave dinámica y el código enviado por mensaje",
                "Hola tía, soy yo, cambié mi número telefónico, necesito transferir dinero de inmediato",
            ],
            &[
                "Buenos días, le llamamos del banco, su tarjeta nueva ya está en la oficina",
                "Hola, le habla el mensajero, su paquete llega mañana en la mañana",
            ],
        ),
        (
            "uy",
            &[
                "Abuela, habla tu nieto, hay que entregar dólares o efectivo al contador, son billetes que salen de circulación",
                "Pasame el código de seis números, el código que te llegó por mensaje, es para el turno de vacunación",
            ],
            &[
                "Buenas, le llamo del banco, su tarjeta nueva está en la sucursal",
                "Hola, del correo, el paquete llega mañana de mañana",
            ],
        ),
        (
            "lb",
            &[
                "مرحبا، من البنك، بدنا تحديث البيانات المصرفية، أعطيني رمز التحقق وأرقام البطاقة",
                "وصلك رمز بالخطأ، اقرأ لي الكود الرمز المؤلف من ستة أرقام",
            ],
            &[
                "مرحبا، من البنك، بطاقتك الجديدة صارت بالفرع",
                "مرحبا، من العيادة، موعدك يوم الخميس صار يوم الجمعة",
            ],
        ),
        (
            "ma",
            &[
                "السلام عليكم، من البنك، عطيني رمز التأكيد السري والرقم السري للبطاقة",
                "ربحت جائزة مالية، خاص دفع رسوم التسليم لاستلام الجائزة",
            ],
            &[
                "السلام عليكم، من البنك، البطاقة الجديدة ديالك وصلات للوكالة",
                "السلام عليكم، من العيادة، الموعد ديالك تبدل للجمعة",
            ],
        ),
        (
            "qa",
            &[
                "معك خدمة عملاء البنك، أعطني رمز التحقق واقبل طلب الدخول",
                "معك التحريات والمباحث، عندك بلاغ مالي، لازم تحويل المبلغ لحساب آمن",
            ],
            &[
                "مرحبا، من البنك، بطاقتك الجديدة جاهزة في الفرع",
                "مرحبا، المندوب، الشحنة توصلك بكرة الصبح",
            ],
        ),
        (
            "mo",
            &[
                "你好，我是司法警察，你涉嫌洗黑錢，要做資金審查，開視頻辦案",
                "會員到期自動續費，取消訂閱要開啟共享屏幕，再報一次性密碼",
            ],
            &[
                "你好，銀行這邊，你的新卡已經到分行了，可以來拿",
                "你好，快遞，包裹明天上午送到",
            ],
        ),
        (
            "za",
            &[
                "This is the fraud department, read out the one-time PIN and approve the reversal prompt",
                "Your identity number linked to illegal SIM registration, I am transferring you to a police officer, pay bail or clearance fee",
            ],
            &[
                "Howzit, it's the bank, your new card is at the branch",
                "Hi, courier here, I'll be there tomorrow morning with the parcel",
            ],
        ),
    ];

    #[test]
    fn every_new_country_has_a_profile_in_its_languages() {
        for iso in NEW_COUNTRIES {
            let cp = db().countries.get(iso).unwrap_or_else(|| panic!("missing {iso}"));
            assert!(!cp.scams.is_empty(), "{iso}");
            for sc in &cp.scams {
                assert!(cp.languages.contains(&sc.lang), "{iso}/{}: {} not among {:?}", sc.id, sc.lang, cp.languages);
            }
        }
        for lang in NEW_LANGS {
            assert!(db().languages().contains(&lang), "{lang} has no keywords");
        }
    }

    #[test]
    fn scams_are_caught_by_their_own_countrys_words() {
        let mut bad = Vec::new();
        for (iso, scams, _) in ROWS {
            for t in scams.iter() {
                let (e, s, h) = live(t);
                if !e || !from_country(iso, t) {
                    bad.push(format!("{iso}: scam not caught (engaged={e} score={s} hits={h:?}): {t}"));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn ordinary_bank_courier_and_clinic_calls_stay_quiet() {
        let mut bad = Vec::new();
        for (iso, _, ordinary) in ROWS {
            for t in ordinary.iter() {
                let (e, s, h) = live(t);
                let strong = keyword_hits(t).iter().any(|k| k.weight >= 2 && from_country(iso, k.text));
                if e || strong {
                    bad.push(format!("{iso}: ordinary call hit (engaged={e} score={s} hits={h:?}): {t}"));
                }
            }
            let all = ordinary.join(" ");
            let (e, s, h) = live(&all);
            if e {
                bad.push(format!("{iso}: ordinary calls together engaged (score={s} hits={h:?})"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Country words are weight 2 at most and never scam-only on their own; the
    /// scam-only words of the new languages live in data/scam_db.json.
    #[test]
    fn new_country_keywords_are_weight_two_at_most_and_never_hard() {
        let mut bad = Vec::new();
        for iso in NEW_COUNTRIES {
            for sc in &db().countries[iso].scams {
                if !sc.id.starts_with(&format!("{iso}_")) {
                    continue;
                }
                for rf in &sc.red_flag_keywords {
                    if rf.hard || rf.weight != 2 {
                        bad.push(format!("{iso}/{}: {:?} weight {} hard {}", sc.id, rf.text, rf.weight, rf.hard));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// Each new language: a scam-only demand engages at once; a common word of
    /// the same field (bank, parcel, clinic) does not, alone or together.
    #[test]
    fn new_language_lists_catch_demands_but_not_everyday_words() {
        let demands: &[(&str, &str)] = &[
            ("he", "תעביר את הכסף לחשבון מאובטח עכשיו"), ("he", "תקריא לי את קוד האימות שקיבלת"),
            ("ur", "پیسے سیف اکاؤنٹ میں منتقل کریں"), ("ur", "مجھے او ٹی پی بتائیں"),
            ("ne", "पैसा सुरक्षित खाता मा पठाउनुहोस्"), ("ne", "मलाई ओटीपी भन्नुहोस्"),
            ("si", "මුදල් ආරක්ෂිත ගිණුමට දාන්න"), ("si", "OTP අංකය මට කියන්න"),
            ("my", "ပိုက်ဆံကို လုံခြုံသောအကောင့်ထဲ လွှဲပါ"), ("my", "အတည်ပြုကုဒ် ပြောပြပါ"),
            ("lo", "ໂອນເງິນເຂົ້າບັນຊີປອດໄພ"), ("lo", "ບອກລະຫັດຢືນຢັນໃຫ້ແດ່"),
            ("mn", "Мөнгөө аюулгүй данс руу шилжүүл"), ("mn", "Баталгаажуулах код хэлээрэй"),
            ("fa", "پول رو به حساب امن منتقل کنید"), ("fa", "رمز دوم کارتتون رو بگید"),
            ("ps", "پیسې خوندي حساب ته واستوئ"), ("ps", "تایید کوډ راته ووایاست"),
            ("az", "Pulu təhlükəsiz hesaba köçürün"), ("az", "Mənə təsdiq kodu deyin"),
            ("hy", "Գումարը փոխանցեք անվտանգ հաշվին"), ("hy", "Ասեք հաստատման կոդը"),
            ("ka", "ფული უსაფრთხო ანგარიშზე გადარიცხეთ"), ("ka", "მითხარით ერთჯერადი კოდი"),
            ("sw", "Hamisha pesa kwenye akaunti salama"), ("sw", "Niambie namba ya siri"),
            ("am", "ገንዘቡን ደህንነቱ የተጠበቀ አካውንት ያስገቡ"), ("am", "የማረጋገጫ ኮድ ይንገሩኝ"),
            ("af", "Skuif die geld na 'n veilige rekening"), ("af", "Lees vir my die eenmalige PIN voor"),
            ("et", "Kandke raha turvalisele kontole"), ("et", "Öelge mulle kinnituskood"),
            ("lv", "Pārskaitiet naudu uz drošu kontu"), ("lv", "Pasakiet man apstiprinājuma kodu"),
            ("lt", "Pervesk pinigus į saugią sąskaitą"), ("lt", "Pasakykite patvirtinimo kodą"),
            ("be", "Перавядзіце грошы на бяспечны рахунак"), ("be", "Скажыце код пацвярджэння"),
        ];
        let everyday: &[(&str, &str)] = &[
            ("he", "מהבנק לגבי חשבון הבנק שלך, החבילה מגיעה מחר, התור במרפאה זז"),
            ("ur", "بینک سے آپ کے بینک اکاؤنٹ کے بارے میں، پارسل کل آئے گا، کلینک کی اپوائنٹمنٹ بدل گئی"),
            ("ne", "बैंकबाट तपाईंको बैंक खाता बारे, पार्सल भोलि आउँछ, क्लिनिकको समय सारियो"),
            ("si", "බැංකුවෙන් ඔබේ බැංකු ගිණුම ගැන, පාර්සලය හෙට එනවා, සායනයේ වෙලාව මාරු කළා"),
            ("my", "ဘဏ်ကပါ ဘဏ်အကောင့်အကြောင်း၊ ပါဆယ်မနက်ဖြန်ရောက်မယ်၊ ဆေးခန်းချိန်းပြောင်းပြီ"),
            ("lo", "ຈາກທະນາຄານເລື່ອງບັນຊີທະນາຄານ, ພັດສະດຸຮອດມື້ອື່ນ, ນັດຄລີນິກປ່ຽນແລ້ວ"),
            ("mn", "Банкнаас таны банкны данс талаар, илгээмж маргааш ирнэ, эмнэлгийн цаг өөрчлөгдсөн"),
            ("fa", "از بانک درباره حساب بانکی شما، بسته فردا می‌رسه، نوبت مطب عوض شد"),
            ("ps", "له بانک څخه ستاسو د بانکي حساب په اړه، بسته سبا راځي، د ډاکټر وخت بدل شو"),
            ("az", "Bankdan bank hesabı haqqında zəng edirik, bağlama sabah gəlir, həkim növbəsi dəyişdi"),
            ("hy", "Բանկից ձեր բանկային հաշիվ մասին, ծանրոցը վաղը կգա, բժշկի այցը փոխվեց"),
            ("ka", "ბანკიდან თქვენი საბანკო ანგარიში შესახებ, ამანათი ხვალ მოვა, ექიმთან ვიზიტი გადაიწია"),
            ("sw", "Ni benki kuhusu akaunti ya benki yako, kifurushi kitafika kesho, miadi ya kliniki imehamishwa"),
            ("am", "ከባንክ ስለ የባንክ አካውንት ነው፣ እቃው ነገ ይደርሳል፣ የክሊኒክ ቀጠሮ ተቀይሯል"),
            ("af", "Die bank bel oor jou bankrekening, die pakkie kom môre, jou afspraak by die dokter is geskuif"),
            ("et", "Pangast helistatakse teie pangakonto pärast, pakk tuleb homme, arsti aeg muutus"),
            ("lv", "Zvana no bankas par jūsu bankas konts, sūtījums būs rīt, ārsta vizīte pārcelta"),
            ("lt", "Skambina iš banko dėl jūsų banko sąskaita, siunta bus rytoj, gydytojo vizitas perkeltas"),
            ("be", "Тэлефануюць з банка наконт вашага банкаўскі рахунак, пасылка будзе заўтра, прыём у лекара перанеслі"),
        ];
        let mut bad = Vec::new();
        for (lang, t) in demands {
            let (e, s, h) = live(t);
            if !e || !h.iter().any(|x| is_hard(x)) {
                bad.push(format!("[{lang}] demand missed (score={s} hits={h:?}): {t}"));
            }
        }
        for (lang, t) in everyday {
            let (e, s, h) = live(t);
            if e {
                bad.push(format!("[{lang}] everyday words engaged (score={s} hits={h:?}): {t}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// The new words must not change how the core languages' ordinary calls score.
    #[test]
    fn core_language_ordinary_calls_stay_quiet() {
        let calls = [
            "您好，這裡是銀行，您的新卡已經寄出，請注意查收",
            "你好，快递到了，放门口可以吗",
            "Hi, this is the clinic, your appointment moved to Friday, and your insurance policies are on file",
            "Hi, this is the bank's card fraud desk, did you make a purchase of 129 dollars yesterday evening?",
            "Hola, le llamo del banco, su tarjeta nueva ya está en la oficina, la policía local cortó la calle",
            "Bonjour, la pharmacie, votre commande est prête, la police municipale a fermé la rue",
            "Guten Tag, Ihre Bank, die neue Karte ist da, die Polizei hat die Straße gesperrt",
            "Olá, é da clínica, a sua consulta passou para sexta, a polícia fechou a rua",
            "もしもし、病院です、予約が金曜日に変わりました",
            "여보세요, 택배입니다, 내일 오전에 도착합니다",
        ];
        let mut bad = Vec::new();
        for t in calls {
            let (e, s, h) = live(t);
            if e {
                bad.push(format!("engaged (score={s} hits={h:?}): {t}"));
            }
            if keyword_hits(t).iter().any(|k| NEW_LANGS.contains(&db().keyword(k.text).map_or("", |kw| kw.lang.as_str()))) {
                bad.push(format!("a new-language word heard in: {t} {h:?}"));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn vietnamese_and_indonesian_otp_demands_trigger_hard_engagement() {
        for demand in [
            "Anh đọc mã xác minh vừa gửi qua tin nhắn giúp em nhé",
            "Vui lòng cung cấp mã xác minh để hủy giao dịch",
            "Tolong sebutkan kode OTP yang masuk ke nomor Anda sekarang",
            "Mohon sebutkan OTP untuk pembatalan transaksi ini",
        ] {
            let (engaged, score, hits) = live(demand);
            assert!(engaged && hits.iter().any(|h| is_hard(h)), "missed OTP demand (score={score} hits={hits:?}): {demand}");
        }
    }
}

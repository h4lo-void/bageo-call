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
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};

const BUILTIN: &str = include_str!("../data/scam_db.json");

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
    by_text: HashMap<String, usize>,
    by_typology: HashMap<String, usize>,
    index: KeywordIndex,
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

/// The caller's words in every form the match modes read.
struct Prepared {
    han: String,
    words_list: Vec<String>,
    padded: String,
    lower: String,
    compact: String,
}

impl Prepared {
    fn of(text: &str) -> Prepared {
        let words_list = words(text);
        let padded = format!(" {} ", words_list.join(" "));
        Prepared {
            han: han_text(text),
            words_list,
            padded,
            lower: text.to_lowercase(),
            compact: compact(text),
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
            for c in p.han.chars() {
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
        let needle = if mode == Match::Word { format!(" {text}") } else { text.clone() };
        Keyword {
            text,
            weight,
            typology: typology.to_string(),
            hard,
            lang: lang.to_string(),
            mode,
            needle,
        }
    }

    fn found_in(&self, p: &Prepared) -> bool {
        match self.mode {
            Match::Han => han_contains(&p.han, &self.needle),
            Match::Word => p.padded.contains(&self.needle),
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
            if db.by_text.contains_key(&kw.text) {
                // 周轉 next to 週轉: a spelling the Chinese normaliser folds into
                // one already listed matches the same words; it is not kept twice.
                if mode == Match::Han && text.trim() != kw.text {
                    continue;
                }
                return Err(format!("{}: keyword {:?} listed twice", g.lang, kw.text));
            }
            let idx = db.keywords.len();
            db.by_text.insert(kw.text.clone(), idx);
            db.index.insert(idx, &kw);
            db.keywords.push(kw);
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
        self.by_text.get(text).map(|&i| &self.keywords[i])
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

/// The built-in database, read once. It cannot fail in a build whose tests
/// passed; if it ever did, the agent goes on without keywords (the second look
/// still runs on calls that say what they want) and says why.
pub fn db() -> &'static ScamDb {
    static DB: OnceLock<ScamDb> = OnceLock::new();
    DB.get_or_init(|| match parse(BUILTIN) {
        Ok(db) => db,
        Err(e) => {
            warn!("built-in scam database unreadable: {e}");
            ScamDb::default()
        }
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
    static SEEN: OnceLock<Mutex<HashMap<(String, u32, String, bool), &'static Keyword>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    let key = (kw.text.clone(), kw.weight, kw.typology.clone(), kw.hard);
    seen.entry(key).or_insert_with(|| Box::leak(Box::new(kw)))
}

/// Read the owner's file. Entries the built-in database cannot place (an
/// unknown typology or language, a weight other than 1 or 2, a keyword it has
/// already, a verdict other than scam / sales / safe) are left out and named.
pub fn parse_local(json: &str) -> Result<LocalDb, String> {
    let raw: RawLocal = serde_json::from_str(json).map_err(|e| format!("scam_db.local.json: {e}"))?;
    let base = db();
    let mut out = LocalDb::default();
    let mut texts = HashSet::new();
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
        if base.keyword(&kw.text).is_some() || !texts.insert(kw.text.clone()) {
            out.skipped.push(format!("{}: already listed", kw.text));
            continue;
        }
        let idx = out.keywords.len();
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
    let mut out: Vec<Hit> = base
        .index
        .candidates(&p)
        .into_iter()
        .map(|i| &base.keywords[i])
        .filter(|k| k.found_in(&p))
        .map(hit_of)
        .collect();
    if !local.keywords.is_empty() {
        for i in local.index.candidates(&p) {
            let k = local.keywords[i];
            if k.found_in(&p) && !out.iter().any(|h| h.text == k.text) {
                out.push(hit_of(k));
            }
        }
    }
    out
}

/// Distinct keywords (built-in, then the owner's) found in `text`, in database order.
pub fn keyword_hits(text: &str) -> Vec<Hit> {
    with_local(|local| keyword_hits_in(text, local))
}

/// A keyword by the text a hit reports.
pub fn keyword(text: &str) -> Option<Hit> {
    let base = db();
    if let Some(&i) = base.by_text.get(text) {
        return Some(hit_of(&base.keywords[i]));
    }
    with_local(|local| local.keywords.iter().find(|k| k.text == text).map(|&k| hit_of(k)))
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

/// The caller follows up on something the owner raised (a complaint, a repair,
/// an application): never a pitch, in any language of the database — unless the
/// caller also makes a scam-only demand (OTP, ATM steps, safe account, screen
/// sharing, marketplace escrow verification), which spoofs a follow-up.
pub fn own_matter(text: &str) -> bool {
    if has_hard_hit(text) {
        return false;
    }
    let han = crate::zh_tw::norm(text);
    let lower = text.to_lowercase();
    db().own_matter.iter().any(|(lang, phrases)| {
        let t = if lang.starts_with("zh") { &han } else { &lower };
        phrases.iter().any(|p| t.contains(p.as_str()))
    })
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
        "scam database v{}: {} typologies, {} keywords in {} languages, {} number rules; {} remembered number(s)",
        base.version,
        base.typologies.len(),
        base.keywords.len(),
        base.languages().len(),
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

/// [`caller_facts_in`] with the agent's number book and the owner's file.
pub fn caller_facts(raw: &str, sim: Option<&str>) -> CallerFacts {
    let local = with_local(|l| l.numbers.clone());
    caller_facts_in(numbers(), &local, raw, sim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Lang;

    /// The 19 languages the database covers.
    const LANGS: [&str; 19] = [
        "zh-TW", "zh-CN", "en", "es", "fr", "de", "pt", "it", "vi", "id", "ms", "tl", "tr", "ru", "ja", "ko", "hi", "ar", "th",
    ];

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bageo-scamdb-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_built_in_database_parses_and_hangs_together() {
        let db = parse(BUILTIN).expect("data/scam_db.json");
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
            assert!(seen.insert(k.text.as_str()), "{} twice", k.text);
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
    fn a_broken_database_is_an_error_not_a_panic() {
        assert!(parse("{").is_err());
        assert!(parse("[]").is_err());
        let base: Value = serde_json::from_str(BUILTIN).unwrap();
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
            "Rappel suite à votre appel",
            "come da sua richiesta la richiamo",
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
                "Bezüglich Ihrer Anfrage von gestern wollte ich Ihnen kurz mitteilen, dass das Teil da ist",
                "Guten Tag, hier ist die Zahnarztpraxis zur Erinnerung an Ihren Termin morgen um zehn Uhr",
            ),
            (
                Lang::Pt,
                "Suporte da plataforma: precisamos compartilhar a tela para fazer a liberação do pagamento",
                "Aqui é da assessoria de cobrança e recuperação de crédito; sem pagamento hoje haverá bloqueio judicial, penhora e protesto em cartório",
                "Olha aqui seu vagabundo caloteiro, sei onde você mora e vamos na sua casa hoje",
                "Sobre sua reclamação, informe agora o código de verificação e sua senha para liberar",
                "Retornando sua ligação sobre o reparo do aparelho, ele já está pronto para retirada",
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
}

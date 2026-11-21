// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Translations for the languages outside the nine core ones: the source
//! files in the packs repository, the rules L01 to L12, and the download
//! Bageo reads (bageo-locales.json.gz).
//!
//! Source layout, per language folder languages/<code>/: names.json,
//! call.json (schema 2, its optional speech block), notify.json,
//! console.json, app.json, desktop.json and _review.json (never packed).
//! The English originals and their notes sit in source/en/<file>.json and
//! source/meta/<file>.json, the fixed terms in source/glossary.json.
//!
//! CI runs the rules strictly: every problem is an issue and nothing is
//! built. Bageo reads leniently ([sanitize_locales]): a key it does not know
//! is ignored and a key that breaks a rule is dropped on its own (that line
//! shows in English), so one bad sentence never costs a whole language.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::de::{DeserializeOwned, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::check::{banned_phrase, plain_file_name, push, unsupported_phrase, valid_commit, valid_min_bageo, MAX_NAME};
use crate::data::{is_country, official_ext_index, OfficialLang, Script, OFFICIAL_LANG_INFO};
use crate::guard::{foreign_letter, has_web_address, hidden_char, hidden_reason, mixed_word, rule_change, Writing};
use crate::model::LocalesRef;
use crate::source::{sorted_paths, SourceFile, SrcHello, SrcLayer};
use crate::util::{days_from_civil, fold, is_sha256_hex, now_days, parse_date, sha256_hex, valid_date, valid_rfc3339_utc};
use crate::{checker_id, Issue};

/// Rule codes carried by [Issue::rule] for translations.
pub mod lrule {
    /// Unreadable file, unknown field, wrong schema, value type or list length, bad status or date.
    pub const FORMAT: &str = "L01-format";
    /// Language code not on the official list, a core language, or not the folder's name.
    pub const CODE: &str = "L02-code";
    /// A key the English source does not have, or a file without English source.
    pub const KEY: &str = "L03-key";
    /// Placeholders differ from the English (Android also %n$s), or a date format does not parse.
    pub const PLACEHOLDER: &str = "L04-placeholder";
    /// Longer than the notes allow, or than the English length times the script's factor plus 8.
    pub const LENGTH: &str = "L05-length";
    /// Line breaks, control or bidirectional characters; zero-width joiners outside Arabic-script and Indic languages.
    pub const CHARSET: &str = "L06-charset";
    /// Brackets, web or e-mail addresses, @, backticks, six digits or a Markdown line start the English does not have.
    pub const PATTERN: &str = "L07-pattern";
    /// Left in English, or mostly not written in the language's script.
    pub const UNTRANSLATED: &str = "L08-untranslated";
    /// Call speech with a banned phrase, or one that changes which language the call may use.
    pub const SPEECH: &str = "L09-speech";
    /// Closing words marked verified that the classifier has not confirmed.
    pub const CLOSING: &str = "L10-closing";
    /// names.json missing, or its code or official code wrong.
    pub const NAMES: &str = "L11-names";
    /// One language over its size limit, or the whole download over its limit.
    pub const SIZE: &str = "L12-size";
}

/// The translations format this build reads and writes.
pub const LOCALES_FORMAT: u32 = 1;
/// The name issues of a translations download are filed under.
pub const LOCALES_FILE: &str = "bageo-locales.json";
/// Most bytes a translations download may unpack to.
pub const MAX_LOCALES_BYTES: usize = 48 * 1024 * 1024;
/// Most bytes one language may take (its JSON in the download, or one source file).
pub const MAX_LANG_BYTES: usize = 600 * 1024;
/// Most characters of a call sentence, of no_wait, of one closing word.
pub const SPEECH_MAX: usize = 80;
pub const NO_WAIT_MAX: usize = 120;
pub const WORD_MAX: usize = 24;
pub const MAX_ALIASES: usize = 12;
/// Share of a file's English keys (in percent) a language needs before the
/// apps carry it.
pub const READY_PERCENT: usize = 95;
/// Review states, lowest first.
pub const REVIEW_STATES: [&str; 4] = ["needs_fix", "ai_draft", "ai_checked", "local_reviewed"];
const REVIEW_FILES: [&str; 6] = ["names", "call", "notify", "console", "app", "desktop"];
const NOTES_MAX: usize = 1000;
/// Room for the header around the languages when a download is unpacked.
const HEAD_BYTES: usize = 64 * 1024;

/// What a key may be: what the notify JSON, the Android strings.xml and
/// the desktop i18n.json use, and nothing that could leave an XML
/// attribute or a JSON path.
pub const KEY_RULE: &str = "a key is 1 to 64 ASCII letters, digits, '_' or '.'";

pub fn valid_key(k: &str) -> bool {
    (1..=64).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

/// A JSON object read into a map, refusing a key that appears twice (serde
/// would quietly keep the last one).
pub(crate) fn unique_map<'de, D, V>(d: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct Unique<V>(std::marker::PhantomData<V>);
    impl<'de, V: Deserialize<'de>> Visitor<'de> for Unique<V> {
        type Value = BTreeMap<String, V>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some(k) = m.next_key::<String>()? {
                if out.contains_key(&k) {
                    return Err(serde::de::Error::custom(format!("duplicate key \"{k}\"")));
                }
                let v = m.next_value::<V>()?;
                out.insert(k, v);
            }
            Ok(out)
        }
    }
    d.deserialize_map(Unique(std::marker::PhantomData))
}

fn count_keys<'de, D: Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    Ok(unique_map::<D, IgnoredAny>(d)?.len())
}

/// The four translated files besides names and call speech.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Notify,
    Console,
    App,
    Desktop,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Notify, Kind::Console, Kind::App, Kind::Desktop];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Notify => "notify",
            Kind::Console => "console",
            Kind::App => "app",
            Kind::Desktop => "desktop",
        }
    }

    pub fn from_name(s: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.name() == s)
    }

    /// Shipped in bageo-locales.json.gz; app and desktop are built into the apps.
    pub fn packed(self) -> bool {
        matches!(self, Kind::Notify | Kind::Console)
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn rank(s: &str) -> Option<usize> {
    REVIEW_STATES.iter().position(|x| *x == s)
}

fn status_msg(s: &str) -> String {
    format!("status \"{s}\" must be one of {}", REVIEW_STATES.join(", "))
}

// ---- source files ---------------------------------------------------------------------

/// languages/<code>/names.json
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcNames {
    pub schema: u32,
    pub code: String,
    pub official: String,
    pub native: String,
    pub en: String,
    pub zh: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub country: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcExamples {
    #[serde(default)]
    pub ends: Vec<String>,
    #[serde(default)]
    pub not_ends: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcClosing {
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub farewells: Vec<String>,
    #[serde(default)]
    pub wrapups: Vec<String>,
    #[serde(default)]
    pub examples: SrcExamples,
}

/// The speech block of a schema 2 call.json.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcSpeech {
    pub im_here: String,
    pub still_there: String,
    pub bye: String,
    pub go_ahead: String,
    pub no_thanks: String,
    pub think_about_it: String,
    pub wait_one_more: String,
    pub no_wait: String,
    pub unavailable: String,
    pub preview: Vec<String>,
    #[serde(default)]
    pub closing: Option<SrcClosing>,
}

/// languages/<code>/{notify,console,app,desktop}.json. Values stay JSON so a
/// wrong type is reported on its key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcStrings {
    pub schema: u32,
    pub lang: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(deserialize_with = "unique_map")]
    pub strings: BTreeMap<String, Value>,
    #[serde(default, deserialize_with = "unique_map")]
    pub lists: BTreeMap<String, Value>,
    #[serde(default, deserialize_with = "unique_map")]
    pub formats: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcReviewEntry {
    pub status: String,
    #[serde(default)]
    pub back: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub on: Option<String>,
}

/// languages/<code>/_review.json: per "<file>.<key>", where its review stands.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcReview {
    #[serde(default)]
    pub schema: Option<u32>,
    #[serde(deserialize_with = "unique_map")]
    pub keys: BTreeMap<String, SrcReviewEntry>,
}

/// source/en/<file>.json: the English text as Bageo has it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnText {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(deserialize_with = "unique_map")]
    pub strings: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "unique_map")]
    pub lists: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "unique_map")]
    pub formats: BTreeMap<String, String>,
}

impl EnText {
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.strings.keys().chain(self.lists.keys()).chain(self.formats.keys())
    }
}

/// One entry of source/meta/<file>.json: what the translator needs to know.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    #[serde(default, rename = "where")]
    pub place: String,
    #[serde(default)]
    pub who: String,
    #[serde(default)]
    pub placeholders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<usize>,
    /// The translation may be the same as the English (a name, a code).
    #[serde(default, skip_serializing_if = "is_false")]
    pub same_ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct EnSource {
    pub text: EnText,
    pub meta: BTreeMap<String, Meta>,
}

/// The English originals translations are checked against. Bageo builds one
/// from its own text; CI reads source/en and source/meta.
#[derive(Debug, Clone, Default)]
pub struct English {
    pub files: BTreeMap<Kind, EnSource>,
}

impl English {
    /// The bageo-call commit the English was taken from (empty when unknown).
    pub fn source(&self) -> String {
        Kind::ALL.iter().find_map(|k| self.files.get(k)?.text.source.clone()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlossaryTerm {
    pub en: String,
    #[serde(default)]
    pub note: Option<String>,
    /// Never translated (Bageo).
    #[serde(default)]
    pub keep: bool,
}

/// source/glossary.json: the fixed terms every language settles first.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Glossary {
    pub schema: u32,
    pub terms: Vec<GlossaryTerm>,
}

/// One language folder that carries translations.
#[derive(Debug, Clone, Default)]
pub struct Locale {
    pub code: String,
    pub names: Option<SourceFile<SrcNames>>,
    /// call.json, kept only when it has a speech block.
    pub layer: Option<SourceFile<SrcLayer>>,
    pub files: BTreeMap<Kind, SourceFile<SrcStrings>>,
    pub review: Option<SourceFile<SrcReview>>,
}

#[derive(Debug, Clone, Default)]
pub struct LocaleSources {
    pub english: English,
    pub glossary: Option<Glossary>,
    pub langs: Vec<Locale>,
}

impl LocaleSources {
    /// Nothing about translations in the folder.
    pub fn is_empty(&self) -> bool {
        self.langs.is_empty() && self.english.files.is_empty() && self.glossary.is_none()
    }
}

// ---- the download ---------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Names {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub official: String,
    #[serde(default)]
    pub native: String,
    #[serde(default)]
    pub en: String,
    #[serde(default)]
    pub zh: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Examples {
    #[serde(default)]
    pub ends: Vec<String>,
    #[serde(default)]
    pub not_ends: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closing {
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub farewells: Vec<String>,
    #[serde(default)]
    pub wrapups: Vec<String>,
    #[serde(default)]
    pub examples: Examples,
}

/// Call speech as packed: the layer's hello copied in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Speech {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hello_again: Option<String>,
    #[serde(default)]
    pub im_here: String,
    #[serde(default)]
    pub still_there: String,
    #[serde(default)]
    pub bye: String,
    #[serde(default)]
    pub go_ahead: String,
    #[serde(default)]
    pub no_thanks: String,
    #[serde(default)]
    pub think_about_it: String,
    #[serde(default)]
    pub wait_one_more: String,
    #[serde(default)]
    pub no_wait: String,
    #[serde(default)]
    pub unavailable: String,
    #[serde(default)]
    pub preview: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closing: Option<Closing>,
}

/// One translated file: values stay JSON so one wrong value costs one key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocFile {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "unique_map")]
    pub strings: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "unique_map")]
    pub lists: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", deserialize_with = "unique_map")]
    pub formats: BTreeMap<String, Value>,
}

impl LocFile {
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty() && self.lists.is_empty() && self.formats.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.strings.keys().chain(self.lists.keys()).chain(self.formats.keys())
    }

    fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.strings.retain(|k, _| keep(k));
        self.lists.retain(|k, _| keep(k));
        self.formats.retain(|k, _| keep(k));
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocaleLang {
    #[serde(default)]
    pub names: Names,
    /// The lowest review state of what is packed.
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech: Option<Speech>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<LocFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<LocFile>,
}

/// bageo-locales.json (gzipped for download).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Locales {
    pub format: u32,
    /// YYYYMMDDNN.
    pub version: u64,
    pub built_at: String,
    pub source_commit: String,
    pub checker: String,
    /// The bageo-call commit the English originals came from (empty when unknown).
    #[serde(default)]
    pub english_source: String,
    #[serde(default, deserialize_with = "unique_map")]
    pub langs: BTreeMap<String, LocaleLang>,
}

/// The first look at a download: its format and how many languages it
/// says it has, before anything else is read.
#[derive(Deserialize)]
struct Head {
    #[serde(default)]
    format: Option<u64>,
    #[serde(default, deserialize_with = "count_keys")]
    langs: usize,
}

/// Runs a closing example through Bageo's classifier: language code, the
/// closing words under test, what the caller said; true when it ends the call.
/// The classifier lives in the agent, so callers bring it.
pub type ClosingJudge<'a> = &'a dyn Fn(&str, &Closing, &str) -> bool;

// ---- reading the sources ----------------------------------------------------------------

fn read_loc<T: DeserializeOwned>(path: &Path, file: &str, out: &mut Vec<Issue>) -> Option<(String, T)> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            push(out, file, "", lrule::FORMAT, format!("cannot read: {e}"));
            return None;
        }
    };
    if bytes.len() > MAX_LANG_BYTES {
        push(out, file, "", lrule::SIZE, format!("{} bytes, the limit for one file is {MAX_LANG_BYTES}", bytes.len()));
        return None;
    }
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => Some((sha256_hex(&bytes), v)),
        Err(e) => {
            push(out, file, "", lrule::FORMAT, e.to_string());
            None
        }
    }
}

fn source_file<T: DeserializeOwned>(dir: &Path, id: &str, file: String, out: &mut Vec<Issue>) -> Option<SourceFile<T>> {
    let path = dir.join(&file);
    if !path.is_file() {
        return None;
    }
    let (sha256, data) = read_loc::<T>(&path, &file, out)?;
    Some(SourceFile { id: id.to_string(), file, sha256, data })
}

/// Reads the translation sources under dir: source/en, source/meta,
/// source/glossary.json and every language folder with translations (a
/// folder whose call.json has no speech and that has no other translation
/// file is a plain region layer and is left out).
pub fn load_locales(dir: &Path) -> Result<LocaleSources, Vec<Issue>> {
    let mut out = Vec::new();
    let mut ls = LocaleSources::default();

    for kind in Kind::ALL {
        let en = format!("source/en/{}.json", kind.name());
        let Some(text) = source_file::<EnText>(dir, kind.name(), en, &mut out) else { continue };
        for k in text.data.keys().filter(|k| !valid_key(k)) {
            push(&mut out, &text.file, k, lrule::KEY, KEY_RULE);
        }
        let meta_file = format!("source/meta/{}.json", kind.name());
        let meta = match source_file::<BTreeMap<String, Meta>>(dir, kind.name(), meta_file.clone(), &mut out) {
            Some(m) => m.data,
            None => {
                if !dir.join(&meta_file).is_file() {
                    push(&mut out, &meta_file, "", lrule::FORMAT, "missing: every English file comes with its notes");
                }
                BTreeMap::new()
            }
        };
        ls.english.files.insert(kind, EnSource { text: text.data, meta });
    }
    ls.glossary = source_file::<Glossary>(dir, "glossary", "source/glossary.json".into(), &mut out).map(|g| g.data);

    if let Ok(paths) = sorted_paths(&dir.join("languages")) {
        for p in paths.iter().filter(|p| p.is_dir()) {
            let code = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let base = format!("languages/{code}");
            let mut l = Locale { code: code.clone(), ..Locale::default() };
            l.names = source_file(dir, &code, format!("{base}/names.json"), &mut out);
            l.layer = source_file::<SrcLayer>(dir, &code, format!("{base}/call.json"), &mut out).filter(|c| c.data.speech.is_some());
            for kind in Kind::ALL {
                if let Some(f) = source_file(dir, &code, format!("{base}/{}.json", kind.name()), &mut out) {
                    l.files.insert(kind, f);
                }
            }
            l.review = source_file(dir, &code, format!("{base}/_review.json"), &mut out);
            if l.names.is_some() || l.layer.is_some() || !l.files.is_empty() || l.review.is_some() {
                ls.langs.push(l);
            }
        }
    }

    if out.is_empty() {
        Ok(ls)
    } else {
        Err(out)
    }
}

// ---- text rules -----------------------------------------------------------------------

/// The non-core entry of the official list a translation is for (L02).
pub fn ext_lang(code: &str) -> Result<&'static OfficialLang, String> {
    if let Some(i) = official_ext_index(code) {
        return Ok(&OFFICIAL_LANG_INFO[i]);
    }
    let low = code.to_ascii_lowercase();
    let core = low == "zh"
        || low.starts_with("zh-")
        || OFFICIAL_LANG_INFO.iter().any(|l| l.core && (l.code == low || l.official.eq_ignore_ascii_case(code)));
    if core {
        Err(format!("\"{code}\" is a core language; its text lives in bageo-call"))
    } else {
        Err(format!("\"{code}\" is not a non-core code on the official language list (use the primary code, for example it or fil)"))
    }
}

/// Whether c is a letter, mark or digit of script s.
pub fn in_script(c: char, s: Script) -> bool {
    let u = c as u32;
    let r = |a: u32, b: u32| (a..=b).contains(&u);
    match s {
        Script::Latin => {
            // U+0250–U+02AF holds ə, the Azerbaijani schwa (Ə is in the block before).
            c.is_ascii_alphabetic()
                || matches!(c, '\u{AA}' | '\u{BA}')
                || (r(0xC0, 0x24F) && c != '\u{D7}' && c != '\u{F7}')
                || r(0x250, 0x2AF)
                || r(0x1E00, 0x1EFF)
        }
        Script::Arabic => r(0x600, 0x6FF) || r(0x750, 0x77F) || r(0x8A0, 0x8FF) || r(0xFB50, 0xFDFF) || r(0xFE70, 0xFEFF),
        Script::Devanagari => r(0x900, 0x97F) || r(0xA8E0, 0xA8FF),
        Script::Bengali => r(0x980, 0x9FF),
        Script::Gurmukhi => r(0xA00, 0xA7F),
        Script::Gujarati => r(0xA80, 0xAFF),
        Script::Odia => r(0xB00, 0xB7F),
        Script::Tamil => r(0xB80, 0xBFF),
        Script::Telugu => r(0xC00, 0xC7F),
        Script::Kannada => r(0xC80, 0xCFF),
        Script::Malayalam => r(0xD00, 0xD7F),
        Script::Sinhala => r(0xD80, 0xDFF),
        Script::Thai => r(0xE00, 0xE7F),
        Script::Lao => r(0xE80, 0xEFF),
        Script::Myanmar => r(0x1000, 0x109F) || r(0xAA60, 0xAA7F),
        Script::Georgian => r(0x10A0, 0x10FF) || r(0x1C90, 0x1CBF),
        Script::Hangul => r(0x1100, 0x11FF) || r(0x3130, 0x318F) || r(0xAC00, 0xD7AF),
        Script::Ethiopic => r(0x1200, 0x139F) || r(0x2D80, 0x2DDF),
        Script::Cyrillic => r(0x400, 0x52F) || r(0x1C80, 0x1C8F),
        Script::Greek => r(0x370, 0x3FF) || r(0x1F00, 0x1FFF),
        Script::Armenian => r(0x530, 0x58F),
        Script::Hebrew => r(0x590, 0x5FF) || r(0xFB1D, 0xFB4F),
        Script::Han => r(0x3005, 0x3007) || r(0x3400, 0x4DBF) || r(0x4E00, 0x9FFF) || r(0xF900, 0xFAFF) || r(0x20000, 0x2FA1F),
        Script::Japanese => r(0x3040, 0x30FF) || r(0x31F0, 0x31FF) || r(0xFF66, 0xFF9F) || in_script(c, Script::Han),
    }
}

fn length_limit(en: &str, meta: Option<&Meta>, script: Script) -> usize {
    if let Some(m) = meta.and_then(|m| m.max) {
        return m;
    }
    let tenths = match script {
        Script::Latin => 16,
        Script::Japanese | Script::Han | Script::Hangul => 10,
        _ => 18,
    };
    en.chars().count() * tenths / 10 + 8
}

/// L06, the same rule the region checks use ([crate::guard::hidden_char]).
fn charset(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, w: Writing) {
    if let Some(c) = hidden_char(s, w) {
        push(out, file, field, lrule::CHARSET, format!("contains U+{:04X} ({})", c as u32, hidden_reason(c)));
    }
}

/// L06 for text with no English to compare: every letter in the allowed
/// scripts, no word mixing look-alike scripts.
fn script_letters(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, allowed: &[Script]) {
    if let Some(c) = foreign_letter(s, allowed) {
        let names = allowed.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>().join(" or ");
        push(
            out,
            file,
            field,
            lrule::CHARSET,
            format!("the letter '{c}' (U+{:04X}) is not in the {names} script, or mixes look-alike scripts in one word", c as u32),
        );
    }
}

/// What L07 looks for in a text.
fn marks(s: &str) -> BTreeSet<&'static str> {
    let folded = fold(s);
    let mut m = BTreeSet::new();
    for (c, name) in [
        ('<', "'<'"),
        ('>', "'>'"),
        ('&', "'&'"),
        ('[', "'['"),
        (']', "']'"),
        ('【', "'【'"),
        ('】', "'】'"),
        ('@', "'@' (an e-mail address or mention)"),
        ('`', "a backtick"),
    ] {
        if folded.contains(c) {
            m.insert(name);
        }
    }
    if has_web_address(s) {
        m.insert("a web address");
    }
    if rule_change(s).is_some() {
        m.insert("an instruction that changes the AI's rules");
    }
    let mut run = 0;
    for c in folded.chars() {
        run = if c.is_numeric() { run + 1 } else { 0 };
        if run >= 6 {
            m.insert("six or more digits in a row");
            break;
        }
    }
    if folded.trim_start().starts_with(['#', '>', '-', '*']) {
        m.insert("a line start that Markdown reads ('#', '>', '-' or '*')");
    }
    m
}

fn patterns(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, en: Option<&str>) {
    let allowed = en.map(marks).unwrap_or_default();
    for m in marks(s) {
        if !allowed.contains(m) {
            let tail = if en.is_some() { " (the English text does not)" } else { "" };
            push(out, file, field, lrule::PATTERN, format!("contains {m}{tail}"));
        }
    }
}

#[derive(Clone, Copy)]
enum Phrases {
    /// Names: no banned or language phrases (rule changes are always checked).
    Names,
    All,
    /// no_wait lists what the AI must not say, so banned phrases belong there.
    NoBanned,
}

fn phrases(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, p: Phrases) {
    if matches!(p, Phrases::All) {
        if let Some(w) = banned_phrase(s) {
            push(out, file, field, lrule::SPEECH, format!("contains the banned phrase \"{w}\""));
        }
    }
    if !matches!(p, Phrases::Names) {
        if let Some(w) = unsupported_phrase(s) {
            push(out, file, field, lrule::SPEECH, format!("names a language or accent Gemini Live does not speak, or pins the call to one language: \"{w}\""));
        }
    }
}

/// A text with no English original (names, speech), written in w with
/// letters from the allowed scripts.
#[allow(clippy::too_many_arguments)]
fn own_text(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, max: usize, w: Writing, allowed: &[Script], p: Phrases) {
    if s.trim().is_empty() {
        push(out, file, field, lrule::FORMAT, "empty");
        return;
    }
    let n = s.chars().count();
    if n > max {
        push(out, file, field, lrule::LENGTH, format!("{n} characters, the limit is {max}"));
    }
    charset(out, file, field, s, w);
    script_letters(out, file, field, s, allowed);
    patterns(out, file, field, s, None);
    phrases(out, file, field, s, p);
}

/// {name} placeholders, sorted; an error for unbalanced braces.
pub(crate) fn brace_tokens(s: &str) -> Result<Vec<String>, &'static str> {
    let mut v = Vec::new();
    let mut cur = String::new();
    let mut open = false;
    for c in s.chars() {
        match c {
            '{' if open => return Err("a { inside a placeholder"),
            '{' => {
                open = true;
                cur = String::from("{");
            }
            '}' if !open => return Err("a } without its {"),
            '}' => {
                cur.push('}');
                v.push(std::mem::take(&mut cur));
                open = false;
            }
            _ if open => cur.push(c),
            _ => {}
        }
    }
    if open {
        return Err("an unclosed {");
    }
    v.sort();
    Ok(v)
}

/// The length of the printf specifier at the start of c (c[0] is '%'):
/// "%%", or %[n$][flags][width][.precision]conversion.
pub(crate) fn printf_spec_len(c: &[char]) -> Option<usize> {
    if c.first() != Some(&'%') {
        return None;
    }
    if c.get(1) == Some(&'%') {
        return Some(2);
    }
    let digits = |mut j: usize| {
        while c.get(j).is_some_and(|d| d.is_ascii_digit()) {
            j += 1;
        }
        j
    };
    let mut j = digits(1);
    j = if j > 1 && c.get(j) == Some(&'$') { j + 1 } else { 1 };
    while c.get(j).is_some_and(|f| "-#+0,(".contains(*f)) {
        j += 1;
    }
    j = digits(j);
    if c.get(j) == Some(&'.') {
        j = digits(j + 1);
    }
    c.get(j).filter(|x| "sdfcxXoeEgGbBhHaA".contains(**x)).map(|_| j + 1)
}

/// printf specifiers (%1$s, %d), sorted; "%%" is not one.
pub(crate) fn printf_tokens(s: &str) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let mut v = Vec::new();
    let mut i = 0;
    while i < c.len() {
        match (c[i] == '%').then(|| printf_spec_len(&c[i..])).flatten() {
            Some(n) => {
                if n > 2 || c[i + 1] != '%' {
                    v.push(c[i..i + n].iter().collect());
                }
                i += n;
            }
            None => i += 1,
        }
    }
    v.sort();
    v
}

fn latin_words(s: &str) -> usize {
    let mut n = 0;
    let mut run = 0;
    for c in s.chars().chain(std::iter::once(' ')) {
        if in_script(c, Script::Latin) {
            run += 1;
        } else {
            if run >= 2 {
                n += 1;
            }
            run = 0;
        }
    }
    n
}

/// The product name, kept as it is in every language: its letters say nothing
/// about whether a line was translated.
const PRODUCT_NAME: &str = "Bageo";

/// More than half of the letters are in the script. Placeholders ({n},
/// %1$s) and the product name are left out of the count: "خدمة Bageo" is
/// Arabic, "Bageo service" is not.
fn mostly_in_script(s: &str, script: Script) -> bool {
    let c: Vec<char> = s.chars().collect();
    let mut kept = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        if c[i] == '%' {
            if let Some(n) = printf_spec_len(&c[i..]) {
                i += n;
                continue;
            }
        }
        kept.push(c[i]);
        i += 1;
    }
    let s = kept.replace(PRODUCT_NAME, " ");
    let (mut letters, mut own, mut inside) = (0usize, 0usize, false);
    for c in s.chars() {
        match c {
            '{' => inside = true,
            '}' => inside = false,
            _ if inside => {}
            _ if c.is_alphabetic() => {
                letters += 1;
                if in_script(c, script) {
                    own += 1;
                }
            }
            _ => {}
        }
    }
    letters == 0 || own * 2 > letters
}

/// Where a translated file's issues go.
pub(crate) struct Ctx<'a> {
    pub file: &'a str,
    pub prefix: &'a str,
    pub kind: Kind,
    pub info: &'static OfficialLang,
    /// CI: unknown keys are issues. Bageo: they are ignored.
    pub strict: bool,
}

fn check_value(out: &mut Vec<Issue>, cx: &Ctx, field: &str, tr: &str, en: &str, meta: Option<&Meta>, format: bool) {
    let file = cx.file;
    if tr.trim().is_empty() && !en.trim().is_empty() {
        push(out, file, field, lrule::FORMAT, "empty");
        return;
    }
    let script = cx.info.script;
    let max = length_limit(en, meta, script);
    let n = tr.chars().count();
    if n > max {
        push(out, file, field, lrule::LENGTH, format!("{n} characters, the limit is {max}"));
    }
    charset(out, file, field, tr, Writing::of_info(cx.info));
    if let Some(c) = mixed_word(tr) {
        push(out, file, field, lrule::CHARSET, format!("the letter '{c}' (U+{:04X}) mixes Latin, Cyrillic or Greek letters in one word", c as u32));
    }
    patterns(out, file, field, tr, Some(en));
    match (brace_tokens(tr), brace_tokens(en)) {
        (Err(e), _) => push(out, file, field, lrule::PLACEHOLDER, e),
        (Ok(a), Ok(b)) if a != b => push(out, file, field, lrule::PLACEHOLDER, format!("placeholders {a:?} differ from the English {b:?}")),
        _ => {}
    }
    if format {
        use chrono::format::{Item, StrftimeItems};
        if StrftimeItems::new(tr).any(|i| matches!(i, Item::Error)) {
            push(out, file, field, lrule::PLACEHOLDER, "not a valid date format");
        }
        if tr.contains("%n") || tr.contains("%t") {
            push(out, file, field, lrule::CHARSET, "%n and %t write line breaks and tabs");
        }
        return;
    }
    if cx.kind == Kind::App {
        let (a, b) = (printf_tokens(tr), printf_tokens(en));
        if a != b {
            push(out, file, field, lrule::PLACEHOLDER, format!("format arguments {a:?} differ from the English {b:?}"));
        }
    }
    if meta.is_some_and(|m| m.same_ok) {
        return;
    }
    if tr == en {
        if latin_words(en) >= 2 {
            push(out, file, field, lrule::UNTRANSLATED, "same as the English text (set same_ok in its notes if that is right)");
        }
    } else if script != Script::Latin && !mostly_in_script(tr, script) {
        push(out, file, field, lrule::UNTRANSLATED, "most letters are not in this language's script");
    }
}

/// Checks one translated file against its English and returns the keys that
/// passed. Strict mode reports unknown keys; lenient mode skips them quietly.
pub(crate) fn check_loc_file(out: &mut Vec<Issue>, cx: &Ctx, f: &LocFile, en: &EnSource) -> LocFile {
    let mut kept = LocFile::default();
    let unknown = |out: &mut Vec<Issue>, field: &str| {
        if cx.strict {
            push(out, cx.file, field, lrule::KEY, "not in the English source");
        }
    };
    for (key, v) in &f.strings {
        let field = format!("{}strings.{key}", cx.prefix);
        if !valid_key(key) {
            push(out, cx.file, &field, lrule::KEY, KEY_RULE);
            continue;
        }
        let Some(e) = en.text.strings.get(key) else {
            unknown(out, &field);
            continue;
        };
        let mut bad = Vec::new();
        match v.as_str() {
            Some(t) => check_value(&mut bad, cx, &field, t, e, en.meta.get(key), false),
            None => push(&mut bad, cx.file, &field, lrule::FORMAT, "must be a string"),
        }
        if bad.is_empty() {
            kept.strings.insert(key.clone(), v.clone());
        }
        out.extend(bad);
    }
    for (key, v) in &f.lists {
        let field = format!("{}lists.{key}", cx.prefix);
        if !valid_key(key) {
            push(out, cx.file, &field, lrule::KEY, KEY_RULE);
            continue;
        }
        let Some(e) = en.text.lists.get(key) else {
            unknown(out, &field);
            continue;
        };
        let mut bad = Vec::new();
        let items: Option<Vec<&str>> = v.as_array().and_then(|a| a.iter().map(Value::as_str).collect());
        match items {
            Some(items) if items.len() == e.len() => {
                for (i, (t, en_item)) in items.iter().zip(e).enumerate() {
                    check_value(&mut bad, cx, &format!("{field}[{i}]"), t, en_item, en.meta.get(key), false);
                }
            }
            Some(items) => push(&mut bad, cx.file, &field, lrule::FORMAT, format!("{} items, the English has {}", items.len(), e.len())),
            None => push(&mut bad, cx.file, &field, lrule::FORMAT, "must be a list of strings"),
        }
        if bad.is_empty() {
            kept.lists.insert(key.clone(), v.clone());
        }
        out.extend(bad);
    }
    for (key, v) in &f.formats {
        let field = format!("{}formats.{key}", cx.prefix);
        if !valid_key(key) {
            push(out, cx.file, &field, lrule::KEY, KEY_RULE);
            continue;
        }
        let Some(e) = en.text.formats.get(key) else {
            unknown(out, &field);
            continue;
        };
        let mut bad = Vec::new();
        match v.as_str() {
            Some(t) => check_value(&mut bad, cx, &field, t, e, en.meta.get(key), true),
            None => push(&mut bad, cx.file, &field, lrule::FORMAT, "must be a string"),
        }
        if bad.is_empty() {
            kept.formats.insert(key.clone(), v.clone());
        }
        out.extend(bad);
    }
    kept
}

pub(crate) fn check_names(out: &mut Vec<Issue>, file: &str, prefix: &str, code: &str, info: &OfficialLang, n: &Names) {
    let fld = |k: &str| format!("{prefix}{k}");
    if n.code != code {
        push(out, file, &fld("code"), lrule::NAMES, format!("\"{}\" must be \"{code}\"", n.code));
    }
    if n.official != info.official {
        push(out, file, &fld("official"), lrule::NAMES, format!("\"{}\" is not the official code of {code} (\"{}\")", n.official, info.official));
    }
    let own = Writing::of_info(info);
    own_text(out, file, &fld("native"), &n.native, MAX_NAME, own, &[info.script], Phrases::Names);
    own_text(out, file, &fld("en"), &n.en, MAX_NAME, Writing::EN, &[Script::Latin], Phrases::Names);
    own_text(out, file, &fld("zh"), &n.zh, MAX_NAME, Writing::ZH, &[Script::Han], Phrases::Names);
    if n.aliases.len() > MAX_ALIASES {
        push(out, file, &fld("aliases"), lrule::LENGTH, format!("{} aliases, the limit is {MAX_ALIASES}", n.aliases.len()));
    }
    // An alias may also be how the name is written in Latin letters.
    for (i, a) in n.aliases.iter().enumerate() {
        own_text(out, file, &fld(&format!("aliases[{i}]")), a, MAX_NAME, own, &[info.script, Script::Latin], Phrases::Names);
    }
    if let Some(c) = &n.country {
        if !is_country(c) {
            push(out, file, &fld("country"), lrule::FORMAT, format!("\"{c}\" is not a lower-case country code"));
        }
    }
}

fn check_speech(out: &mut Vec<Issue>, file: &str, prefix: &str, info: &OfficialLang, sp: &Speech) {
    let p = |k: &str| format!("{prefix}{k}");
    let (w, own) = (Writing::of_info(info), [info.script]);
    let lines: [(&str, &str, usize, Phrases); 9] = [
        ("im_here", &sp.im_here, SPEECH_MAX, Phrases::All),
        ("still_there", &sp.still_there, SPEECH_MAX, Phrases::All),
        ("bye", &sp.bye, SPEECH_MAX, Phrases::All),
        ("go_ahead", &sp.go_ahead, SPEECH_MAX, Phrases::All),
        ("no_thanks", &sp.no_thanks, SPEECH_MAX, Phrases::All),
        ("think_about_it", &sp.think_about_it, SPEECH_MAX, Phrases::All),
        ("wait_one_more", &sp.wait_one_more, SPEECH_MAX, Phrases::All),
        ("no_wait", &sp.no_wait, NO_WAIT_MAX, Phrases::NoBanned),
        ("unavailable", &sp.unavailable, SPEECH_MAX, Phrases::All),
    ];
    for (k, s, max, ph) in lines {
        own_text(out, file, &p(k), s, max, w, &own, ph);
    }
    for (k, h) in [("hello", &sp.hello), ("hello_again", &sp.hello_again)] {
        if let Some(h) = h {
            own_text(out, file, &p(k), h, SPEECH_MAX, w, &own, Phrases::All);
        }
    }
    if sp.preview.len() != 3 {
        push(out, file, &p("preview"), lrule::FORMAT, format!("{} lines, needs exactly 3", sp.preview.len()));
    }
    for (i, s) in sp.preview.iter().enumerate() {
        own_text(out, file, &p(&format!("preview[{i}]")), s, SPEECH_MAX, w, &own, Phrases::All);
    }
    if let Some(c) = &sp.closing {
        for (name, words) in [("farewells", &c.farewells), ("wrapups", &c.wrapups)] {
            for (i, w) in words.iter().enumerate() {
                own_text(out, file, &p(&format!("closing.{name}[{i}]")), w, WORD_MAX, Writing::of_info(info), &own, Phrases::All);
            }
        }
        for (name, ex) in [("ends", &c.examples.ends), ("not_ends", &c.examples.not_ends)] {
            for (i, s) in ex.iter().enumerate() {
                let f = p(&format!("closing.examples.{name}[{i}]"));
                if s.trim().is_empty() {
                    push(out, file, &f, lrule::FORMAT, "empty");
                } else if s.chars().count() > SPEECH_MAX {
                    push(out, file, &f, lrule::LENGTH, format!("more than {SPEECH_MAX} characters"));
                }
                charset(out, file, &f, s, w);
            }
        }
    }
}

/// L10: verified closing words need a local review and every example
/// classified right by the judge Bageo uses.
fn check_closing(out: &mut Vec<Issue>, file: &str, field: &str, code: &str, c: &Closing, reviewed: bool, judge: Option<ClosingJudge>) {
    if !c.verified {
        return;
    }
    if !reviewed {
        push(out, file, field, lrule::CLOSING, "verified closing words need the language's call.json to be local_reviewed");
    }
    if c.examples.ends.is_empty() || c.examples.not_ends.is_empty() {
        push(out, file, field, lrule::CLOSING, "verified closing words need examples that end the call and examples that do not");
    }
    let Some(judge) = judge else {
        push(out, file, field, lrule::CLOSING, "verified, but no classifier ran the examples; leave verified false until Bageo's classifier confirms them");
        return;
    };
    for s in &c.examples.ends {
        if !judge(code, c, s) {
            push(out, file, field, lrule::CLOSING, format!("should end the call but does not: \"{s}\""));
        }
    }
    for s in &c.examples.not_ends {
        if judge(code, c, s) {
            push(out, file, field, lrule::CLOSING, format!("should not end the call but does: \"{s}\""));
        }
    }
}

// ---- strict pipeline ---------------------------------------------------------------------

pub(crate) fn names_of(n: &SrcNames) -> Names {
    Names {
        code: n.code.clone(),
        official: n.official.clone(),
        native: n.native.clone(),
        en: n.en.clone(),
        zh: n.zh.clone(),
        aliases: n.aliases.clone(),
        country: n.country.clone(),
    }
}

fn speech_of(sp: &SrcSpeech, hello: Option<&SrcHello>) -> Speech {
    Speech {
        hello: hello.map(|h| h.hello.clone()),
        hello_again: hello.map(|h| h.hello_again.clone()),
        im_here: sp.im_here.clone(),
        still_there: sp.still_there.clone(),
        bye: sp.bye.clone(),
        go_ahead: sp.go_ahead.clone(),
        no_thanks: sp.no_thanks.clone(),
        think_about_it: sp.think_about_it.clone(),
        wait_one_more: sp.wait_one_more.clone(),
        no_wait: sp.no_wait.clone(),
        unavailable: sp.unavailable.clone(),
        preview: sp.preview.clone(),
        closing: sp.closing.as_ref().map(|c| Closing {
            verified: c.verified,
            farewells: c.farewells.clone(),
            wrapups: c.wrapups.clone(),
            examples: Examples { ends: c.examples.ends.clone(), not_ends: c.examples.not_ends.clone() },
        }),
    }
}

pub(crate) fn locfile_of(s: &SrcStrings) -> LocFile {
    LocFile { strings: s.strings.clone(), lists: s.lists.clone(), formats: s.formats.clone() }
}

fn review_status<'a>(l: &'a Locale, file: &str, key: &str) -> &'a str {
    l.review
        .as_ref()
        .and_then(|r| r.data.keys.get(&format!("{file}.{key}")))
        .map(|e| e.status.as_str())
        .unwrap_or("ai_draft")
}

/// Keys a reviewer marked needs_fix are left out (that line shows in English).
pub(crate) fn drop_needs_fix(l: &Locale, kind: Kind, f: &mut LocFile) {
    f.retain(|k| review_status(l, kind.name(), k) != "needs_fix");
}

fn call_states(l: &Locale) -> impl Iterator<Item = &str> + '_ {
    l.review.iter().flat_map(|r| r.data.keys.iter()).filter(|(k, _)| k.starts_with("call.")).map(|(_, e)| e.status.as_str())
}

fn check_review(out: &mut Vec<Issue>, rf: &SourceFile<SrcReview>, l: &Locale, now: i64) {
    let f = rf.file.as_str();
    if let Some(s) = rf.data.schema {
        if s != 1 {
            push(out, f, "schema", lrule::FORMAT, format!("schema {s} is not 1"));
        }
    }
    for (k, e) in &rf.data.keys {
        let field = format!("keys.{k}");
        match k.split_once('.') {
            Some((file, key)) if REVIEW_FILES.contains(&file) => {
                if let Some(kind) = Kind::from_name(file) {
                    let has = l.files.get(&kind).is_some_and(|sf| {
                        sf.data.strings.contains_key(key) || sf.data.lists.contains_key(key) || sf.data.formats.contains_key(key)
                    });
                    if !has {
                        push(out, f, &field, lrule::KEY, format!("{file}.json has no key \"{key}\""));
                    }
                }
            }
            _ => push(out, f, &field, lrule::FORMAT, format!("must be <file>.<key> with the file one of {}", REVIEW_FILES.join(", "))),
        }
        if rank(&e.status).is_none() {
            push(out, f, &format!("{field}.status"), lrule::FORMAT, status_msg(&e.status));
        }
        if e.status == "local_reviewed" && e.on.is_none() {
            push(out, f, &format!("{field}.on"), lrule::FORMAT, "the review date is required for local_reviewed");
        }
        if let Some(d) = &e.on {
            match parse_date(d) {
                None => push(out, f, &format!("{field}.on"), lrule::FORMAT, format!("\"{d}\" is not a date (YYYY-MM-DD)")),
                Some((y, m, dd)) if days_from_civil(y, m, dd) > now + 1 => push(out, f, &format!("{field}.on"), lrule::FORMAT, "is in the future"),
                Some(_) => {}
            }
        }
        for (name, t) in [("back", &e.back), ("note", &e.note)] {
            if t.as_ref().is_some_and(|t| t.chars().count() > NOTES_MAX) {
                push(out, f, &format!("{field}.{name}"), lrule::LENGTH, format!("more than {NOTES_MAX} characters"));
            }
        }
    }
}

fn lang_status(l: &Locale, notify: Option<&LocFile>, console: Option<&LocFile>, has_speech: bool) -> String {
    let mut low = l.names.as_ref().and_then(|n| rank(&n.data.status)).unwrap_or(1);
    for (kind, f) in [(Kind::Notify, notify), (Kind::Console, console)] {
        for k in f.into_iter().flat_map(|f| f.keys()) {
            low = low.min(rank(review_status(l, kind.name(), k)).unwrap_or(1));
        }
    }
    if has_speech {
        low = low.min(call_states(l).filter_map(rank).min().unwrap_or(1));
    }
    REVIEW_STATES[low].to_string()
}

fn check_lang(out: &mut Vec<Issue>, ls: &LocaleSources, l: &Locale, judge: Option<ClosingJudge>, now: i64) -> Option<LocaleLang> {
    let dir = format!("languages/{}", l.code);
    let info = match ext_lang(&l.code) {
        Ok(i) => i,
        Err(e) => {
            push(out, &dir, "", lrule::CODE, e);
            return None;
        }
    };
    let names = match &l.names {
        None => {
            push(out, &dir, "", lrule::NAMES, "names.json is missing (every translated language needs one)");
            None
        }
        Some(n) => {
            if n.data.schema != 1 {
                push(out, &n.file, "schema", lrule::FORMAT, format!("schema {} is not 1", n.data.schema));
            }
            if rank(&n.data.status).is_none() {
                push(out, &n.file, "status", lrule::FORMAT, status_msg(&n.data.status));
            }
            let names = names_of(&n.data);
            check_names(out, &n.file, "", &l.code, info, &names);
            Some(names)
        }
    };

    let mut speech = None;
    if let Some(layer) = &l.layer {
        if let Some(sp) = &layer.data.speech {
            if layer.data.schema != 2 {
                push(out, &layer.file, "schema", lrule::FORMAT, "a speech block needs schema 2");
            }
            let sp = speech_of(sp, layer.data.hello.as_ref());
            check_speech(out, &layer.file, "speech.", info, &sp);
            if let Some(c) = &sp.closing {
                check_closing(out, &layer.file, "speech.closing", &l.code, c, layer.data.status == "local_reviewed", judge);
            }
            speech = Some(sp);
        }
    }

    let mut files = BTreeMap::new();
    for (kind, sf) in &l.files {
        let d = &sf.data;
        if d.schema != 1 {
            push(out, &sf.file, "schema", lrule::FORMAT, format!("schema {} is not 1", d.schema));
        }
        if d.lang != l.code {
            push(out, &sf.file, "lang", lrule::CODE, format!("\"{}\" must be the folder name \"{}\"", d.lang, l.code));
        }
        if d.source.as_ref().is_some_and(|s| !valid_commit(s)) {
            push(out, &sf.file, "source", lrule::FORMAT, "must be a commit (1 to 64 letters, digits, '.', '_' or '-')");
        }
        let Some(en) = ls.english.files.get(kind) else {
            push(out, &sf.file, "", lrule::KEY, format!("there is no English source (source/en/{}.json)", kind.name()));
            continue;
        };
        let cx = Ctx { file: &sf.file, prefix: "", kind: *kind, info, strict: true };
        let mut kept = check_loc_file(out, &cx, &locfile_of(d), en);
        drop_needs_fix(l, *kind, &mut kept);
        files.insert(*kind, kept);
    }
    if let Some(r) = &l.review {
        check_review(out, r, l, now);
    }

    let names = names?;
    if call_states(l).any(|s| s == "needs_fix") {
        speech = None;
    }
    let mut take = |k: Kind| files.remove(&k).filter(|f| k.packed() && !f.is_empty());
    let (notify, console) = (take(Kind::Notify), take(Kind::Console));
    let status = lang_status(l, notify.as_ref(), console.as_ref(), speech.is_some());
    let lang = LocaleLang { names, status, speech, notify, console };
    let size = serde_json::to_vec(&lang).map(|v| v.len()).unwrap_or(usize::MAX);
    if size > MAX_LANG_BYTES {
        push(out, &dir, "", lrule::SIZE, format!("{size} bytes packed, the limit for one language is {MAX_LANG_BYTES}"));
    }
    Some(lang)
}

fn pipeline(ls: &LocaleSources, judge: Option<ClosingJudge>, now: i64) -> (BTreeMap<String, LocaleLang>, Vec<Issue>) {
    let mut out = Vec::new();
    for (kind, en) in &ls.english.files {
        if en.text.schema != 1 {
            push(&mut out, &format!("source/en/{}.json", kind.name()), "schema", lrule::FORMAT, format!("schema {} is not 1", en.text.schema));
        }
    }
    if ls.glossary.as_ref().is_some_and(|g| g.schema != 1) {
        push(&mut out, "source/glossary.json", "schema", lrule::FORMAT, "schema is not 1");
    }
    let mut langs = BTreeMap::new();
    for l in &ls.langs {
        if let Some(lang) = check_lang(&mut out, ls, l, judge, now) {
            langs.insert(l.code.clone(), lang);
        }
    }
    (langs, out)
}

/// Every translation rule on a set of sources (what CI runs). judge runs the
/// closing examples; without one, verified closing words cannot pass.
pub fn check_locales(ls: &LocaleSources, judge: Option<ClosingJudge>) -> Vec<Issue> {
    pipeline(ls, judge, now_days()).1
}

fn version_ok(v: u64, now: i64) -> Result<(), String> {
    if !(1_000_000_000..10_000_000_000).contains(&v) {
        return Err(format!("{v} is not YYYYMMDDNN"));
    }
    let (y, m, d) = ((v / 1_000_000) as i64, (v / 10_000 % 100) as u32, (v / 100 % 100) as u32);
    if !valid_date(y, m, d) {
        return Err(format!("{v} does not start with a real date"));
    }
    if days_from_civil(y, m, d) > now + 1 {
        return Err(format!("{v} is later than tomorrow"));
    }
    Ok(())
}

fn check_header(out: &mut Vec<Issue>, l: &Locales, now: i64) {
    let f = LOCALES_FILE;
    if l.format != LOCALES_FORMAT {
        push(out, f, "format", lrule::FORMAT, format!("format {} is not {LOCALES_FORMAT}", l.format));
    }
    if let Err(e) = version_ok(l.version, now) {
        push(out, f, "version", lrule::FORMAT, e);
    }
    if !valid_rfc3339_utc(&l.built_at) {
        push(out, f, "built_at", lrule::FORMAT, "must look like 2026-11-20T08:00:00Z");
    }
    if !valid_commit(&l.source_commit) {
        push(out, f, "source_commit", lrule::FORMAT, "must be 1 to 64 letters, digits, '.', '_' or '-'");
    }
    if !l.english_source.is_empty() && !valid_commit(&l.english_source) {
        push(out, f, "english_source", lrule::FORMAT, "must be empty or 1 to 64 letters, digits, '.', '_' or '-'");
    }
    if l.checker.is_empty() || l.checker.len() > 64 || !l.checker.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        push(out, f, "checker", lrule::FORMAT, "must be 1 to 64 printable ASCII characters");
    }
    if l.langs.len() > OFFICIAL_LANG_INFO.len() {
        push(out, f, "langs", lrule::SIZE, format!("{} languages, the official list has {}", l.langs.len(), OFFICIAL_LANG_INFO.len()));
    }
}

/// Checks and packs the sources; built_at is given so the same sources
/// always give the same file.
pub fn build_locales_at(ls: &LocaleSources, version: u64, commit: &str, built_at: &str, judge: Option<ClosingJudge>) -> Result<Locales, Vec<Issue>> {
    let now = now_days();
    let (langs, mut out) = pipeline(ls, judge, now);
    let loc = Locales {
        format: LOCALES_FORMAT,
        version,
        built_at: built_at.to_string(),
        source_commit: commit.to_string(),
        checker: checker_id(),
        english_source: ls.english.source(),
        langs,
    };
    check_header(&mut out, &loc, now);
    if out.is_empty() {
        let size = locales_json(&loc).map(|j| j.len()).unwrap_or(usize::MAX);
        if size > MAX_LOCALES_BYTES {
            push(&mut out, LOCALES_FILE, "", lrule::SIZE, format!("{size} bytes, the limit is {MAX_LOCALES_BYTES}"));
        }
    }
    if out.is_empty() {
        Ok(loc)
    } else {
        Err(out)
    }
}

// ---- packing and reading ---------------------------------------------------------------

/// The JSON inside the download.
pub fn locales_json(l: &Locales) -> Result<Vec<u8>, String> {
    serde_json::to_vec(l).map_err(|e| e.to_string())
}

/// bageo-locales.json.gz: the JSON, gzipped (no file name or time in the
/// header, so equal input gives equal bytes).
pub fn pack_locales(l: &Locales) -> Result<Vec<u8>, String> {
    let json = locales_json(l)?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&json).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

/// Unpacks a download, counting as it goes and stopping past max bytes.
/// The file is exactly one gzip member: a second member or any bytes after
/// the first are refused (they would be ignored, so a checked file could
/// carry data nobody looked at).
pub fn unpack_locales(gz: &[u8], max: usize) -> Result<Vec<u8>, Vec<Issue>> {
    let one = |rule: &'static str, d: String| vec![Issue::new(LOCALES_FILE, "", rule, d)];
    if gz.len() > max {
        return Err(one(lrule::SIZE, format!("{} bytes packed, the limit is {max}", gz.len())));
    }
    let mut dec = GzDecoder::new(gz);
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = dec.read(&mut buf).map_err(|e| one(lrule::FORMAT, format!("cannot unpack: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len() + n > max {
            return Err(one(lrule::SIZE, format!("more than {max} bytes unpacked; stopped reading")));
        }
        out.extend_from_slice(&buf[..n]);
    }
    let rest = dec.into_inner();
    if !rest.is_empty() {
        return Err(one(lrule::FORMAT, format!("{} bytes after the packed data (a second gzip member or trailing bytes)", rest.len())));
    }
    Ok(out)
}

/// Reads a download: size while unpacking (at most [MAX_LOCALES_BYTES]),
/// format number, shape and header. The languages themselves are checked by
/// [sanitize_locales], which needs Bageo's English.
pub fn parse_locales(gz: &[u8]) -> Result<Locales, Vec<Issue>> {
    parse_locales_limited(gz, MAX_LOCALES_BYTES)
}

/// Same, with the unpacked limit given. Before the languages are read the
/// unpacked size must also fit the number of languages the file lists
/// (600 KB each, plus room for the header), and no key may appear twice.
pub fn parse_locales_limited(gz: &[u8], max: usize) -> Result<Locales, Vec<Issue>> {
    parse_locales_at(gz, max, now_days())
}

/// Same, with today given (days since 1970-01-01) for the "not later than
/// tomorrow" check of the version: the file Bageo ships passes a day far
/// ahead, so a computer whose clock is behind still reads it.
pub fn parse_locales_at(gz: &[u8], max: usize, today: i64) -> Result<Locales, Vec<Issue>> {
    let json = unpack_locales(gz, max)?;
    let one = |rule: &'static str, field: &str, d: String| vec![Issue::new(LOCALES_FILE, field, rule, d)];
    let head: Head = serde_json::from_slice(&json).map_err(|e| one(lrule::FORMAT, "", e.to_string()))?;
    match head.format {
        Some(n) if n > LOCALES_FORMAT as u64 => {
            return Err(one(lrule::FORMAT, "format", format!("format {n} is newer than this Bageo reads ({LOCALES_FORMAT}); update Bageo first")))
        }
        Some(_) => {}
        None => return Err(one(lrule::FORMAT, "format", "missing".to_string())),
    }
    let cap = head.langs.min(OFFICIAL_LANG_INFO.len()) * MAX_LANG_BYTES + HEAD_BYTES;
    if json.len() > cap {
        return Err(one(
            lrule::SIZE,
            "",
            format!("{} bytes unpacked for {} language(s); the limit is {cap} ({MAX_LANG_BYTES} a language)", json.len(), head.langs),
        ));
    }
    let loc: Locales = serde_json::from_slice(&json).map_err(|e| one(lrule::FORMAT, "", e.to_string()))?;
    let mut out = Vec::new();
    check_header(&mut out, &loc, today);
    if out.is_empty() {
        Ok(loc)
    } else {
        Err(out)
    }
}

/// The lenient pass Bageo runs on a download it read: a language that is too
/// big, not on the list or has broken names is left out; broken speech is
/// left out (verified closing words whose examples fail only lose verified);
/// in the files, unknown keys are dropped quietly and each broken key is
/// dropped and reported. Returns what was dropped.
pub fn sanitize_locales(l: &mut Locales, english: &English, judge: Option<ClosingJudge>) -> Vec<Issue> {
    let mut out = Vec::new();
    let f = LOCALES_FILE;
    for (code, mut lang) in std::mem::take(&mut l.langs) {
        let at = format!("langs.{code}");
        let size = serde_json::to_vec(&lang).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > MAX_LANG_BYTES {
            push(&mut out, f, &at, lrule::SIZE, format!("{size} bytes, the limit for one language is {MAX_LANG_BYTES}"));
            continue;
        }
        let info = match ext_lang(&code) {
            Ok(i) => i,
            Err(e) => {
                push(&mut out, f, &at, lrule::CODE, e);
                continue;
            }
        };
        let mut bad = Vec::new();
        check_names(&mut bad, f, &format!("{at}.names."), &code, info, &lang.names);
        if !bad.is_empty() {
            out.extend(bad);
            continue;
        }
        if rank(&lang.status).is_none() {
            push(&mut out, f, &format!("{at}.status"), lrule::FORMAT, status_msg(&lang.status));
            lang.status = "ai_draft".to_string();
        }
        if let Some(mut sp) = lang.speech.take() {
            let mut bad = Vec::new();
            check_speech(&mut bad, f, &format!("{at}.speech."), info, &sp);
            if bad.is_empty() {
                if let Some(c) = sp.closing.as_mut() {
                    let mut wrong = Vec::new();
                    check_closing(&mut wrong, f, &format!("{at}.speech.closing"), &code, c, true, judge);
                    if !wrong.is_empty() {
                        c.verified = false;
                        out.extend(wrong);
                    }
                }
                lang.speech = Some(sp);
            } else {
                out.extend(bad);
            }
        }
        for kind in [Kind::Notify, Kind::Console] {
            let slot = if kind == Kind::Notify { &mut lang.notify } else { &mut lang.console };
            let Some(file) = slot.take() else { continue };
            let Some(en) = english.files.get(&kind) else {
                push(&mut out, f, &format!("{at}.{}", kind.name()), lrule::KEY, "this Bageo has no English text for it; left out");
                continue;
            };
            let prefix = format!("{at}.{}.", kind.name());
            let cx = Ctx { file: f, prefix: &prefix, kind, info, strict: false };
            let kept = check_loc_file(&mut out, &cx, &file, en);
            if !kept.is_empty() {
                *slot = Some(kept);
            }
        }
        l.langs.insert(code, lang);
    }
    out
}

/// The translations entry of the release index.
pub fn check_locales_ref(r: &LocalesRef) -> Vec<Issue> {
    let f = "bageo-pack-index.json";
    let mut out = Vec::new();
    if !plain_file_name(&r.file) {
        push(&mut out, f, "locales.file", lrule::FORMAT, "must be a plain file name (letters, digits, '.', '_', '-'), not a path");
    }
    if r.bytes == 0 || r.bytes > MAX_LOCALES_BYTES as u64 {
        push(&mut out, f, "locales.bytes", lrule::SIZE, format!("{} bytes, must be 1 to {MAX_LOCALES_BYTES}", r.bytes));
    }
    if !is_sha256_hex(&r.sha256) {
        push(&mut out, f, "locales.sha256", lrule::FORMAT, "must be 64 lower-case hex digits");
    }
    if r.format != LOCALES_FORMAT {
        push(&mut out, f, "locales.format", lrule::FORMAT, format!("format {} is not {LOCALES_FORMAT}", r.format));
    }
    if let Err(e) = version_ok(r.version, now_days()) {
        push(&mut out, f, "locales.version", lrule::FORMAT, e);
    }
    if !valid_min_bageo(&r.min_bageo) {
        push(&mut out, f, "locales.min_bageo", lrule::FORMAT, "must look like 1.6.9");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printf_specifiers_are_found_and_percent_signs_are_not() {
        assert_eq!(printf_tokens("Version %1$s (%2$d)"), ["%1$s", "%2$d"]);
        assert_eq!(printf_tokens("100%% sure, 50% off"), Vec::<String>::new());
        assert_eq!(printf_tokens("%s and %.1f"), ["%.1f", "%s"]);
    }

    #[test]
    fn brace_placeholders_must_balance() {
        assert_eq!(brace_tokens("{number} called {count}").unwrap(), ["{count}", "{number}"]);
        assert!(brace_tokens("{oops").is_err());
        assert!(brace_tokens("oops}").is_err());
    }

    #[test]
    fn scripts_are_told_apart() {
        assert!(mostly_in_script("สายเรียกเข้า Bageo", Script::Thai));
        assert!(!mostly_in_script("Incoming call สาย", Script::Thai));
        assert!(mostly_in_script("{number} : 12", Script::Arabic));
        assert_eq!(latin_words("Bageo Status"), 2);
        assert_eq!(latin_words("{} s"), 0);
    }

    #[test]
    fn the_product_name_and_placeholders_do_not_count_against_the_script() {
        // The product name stays in Latin letters in a translated line.
        assert!(mostly_in_script("خدمة Bageo", Script::Arabic));
        assert!(mostly_in_script("Bageo सेवा", Script::Devanagari));
        assert!(mostly_in_script("%1$s مكالمة {n}", Script::Arabic));
        assert!(mostly_in_script("Bageo %1$s सेवा %2$d", Script::Devanagari));
        // A line left in English is still caught, the product name or not.
        assert!(!mostly_in_script("Bageo service", Script::Arabic));
        assert!(!mostly_in_script("The Bageo service is off", Script::Devanagari));
        assert!(!mostly_in_script("Phone %1$s is not connected", Script::Arabic));
        // Through the check itself: the translated line passes, the English one does not.
        let info = ext_lang("ar").unwrap();
        let cx = Ctx { file: "languages/ar/app.json", prefix: "", kind: Kind::App, info, strict: true };
        let mut out = Vec::new();
        check_value(&mut out, &cx, "strings.channel_service", "خدمة Bageo", "Bageo service", None, false);
        assert!(out.is_empty(), "{out:?}");
        check_value(&mut out, &cx, "strings.channel_service", "Bageo service", "Bageo service", None, false);
        assert!(out.iter().any(|i| i.rule == lrule::UNTRANSLATED), "{out:?}");
        let hi = Ctx { info: ext_lang("hi").unwrap(), file: "languages/hi/app.json", ..cx };
        let mut out = Vec::new();
        check_value(&mut out, &hi, "strings.channel_service", "Bageo सेवा", "Bageo service", None, false);
        assert!(out.is_empty(), "{out:?}");
        check_value(&mut out, &hi, "strings.x", "The service is running", "The service is running", None, false);
        assert!(out.iter().any(|i| i.rule == lrule::UNTRANSLATED), "{out:?}");
    }
}

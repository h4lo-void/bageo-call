// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The prompt texts: the words Bageo tells the models, the rules a changed
//! text must pass, and the download that carries changed texts
//! (bageo-prompts.json.gz).
//!
//! - The built-in texts are the files in crates/agent/data/prompts, compiled
//!   in here ([builtin_book]); Bageo reads them through this module only.
//!   A file is {"format": 1, "lang": "<language>", "locked": [<key>, ...],
//!   "texts": {"<key>": <text>}}, a text being a string or a list of lines
//!   (joined with a line break). "locked" may be left out.
//! - A locked text is a rule of the product: only a new Bageo changes it,
//!   never a download.
//! - The download only carries texts that differ from the built-in ones. Each
//!   is checked on its own ([check_text]) against the built-in text of the same
//!   key: the same {placeholders}, the same spaces and line breaks at both
//!   ends, a bounded length, and no control, replacement or bidirectional
//!   characters. Bageo uses the texts that pass and keeps its own for the rest.
//! - In bageo-packs the changed texts are prompts/<language>.json, in the same
//!   file format; [load_prompt_changes] checks them all, [build_prompts_at]
//!   makes the download.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;

use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::check::{plain_file_name, push, valid_commit, valid_min_bageo};
use crate::scams::valid_version;
use crate::source::sorted_paths;
use crate::util::{is_sha256_hex, now_days, sha256_hex, valid_rfc3339_utc};
use crate::{rule, Index, Issue, LocalesRef};

/// The format of the download this build reads and writes.
pub const PROMPTS_FORMAT: u32 = 1;
/// The download's file name.
pub const PROMPTS_FILE: &str = "bageo-prompts.json.gz";
/// The download as it comes (packed).
pub const MAX_PROMPTS_PACKED: usize = 512 * 1024;
/// The download unpacked; counted while unpacking.
pub const MAX_PROMPTS_BYTES: usize = 4 * 1024 * 1024;
/// The format of a file of texts (built-in, or prompts/<language>.json).
pub const BOOK_FORMAT: u64 = 1;
const INDEX_FILE: &str = "bageo-pack-index.json";

/// The built-in files, by the language their words are written in.
pub const BUILTIN_FILES: &[(&str, &str)] = &[
    ("de", include_str!("../../agent/data/prompts/de.json")),
    ("en", include_str!("../../agent/data/prompts/en.json")),
    ("es", include_str!("../../agent/data/prompts/es.json")),
    ("fr", include_str!("../../agent/data/prompts/fr.json")),
    ("ja", include_str!("../../agent/data/prompts/ja.json")),
    ("ko", include_str!("../../agent/data/prompts/ko.json")),
    ("pt", include_str!("../../agent/data/prompts/pt.json")),
    ("zh-TW", include_str!("../../agent/data/prompts/zh-TW.json")),
];

// ---- one file of texts -------------------------------------------------------------------------

/// The texts of one file, by key, and the keys only a new Bageo may change.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookFile {
    pub texts: BTreeMap<String, String>,
    pub locked: BTreeSet<String>,
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Reads one file of texts written in lang.
pub fn parse_book_file(lang: &str, json: &str) -> Result<BookFile, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let obj = v.as_object().ok_or("not a JSON object")?;
    if let Some(field) = obj.keys().find(|k| !matches!(k.as_str(), "format" | "lang" | "locked" | "texts")) {
        return Err(format!("field {field:?}: not part of this format"));
    }
    match v.get("format").and_then(Value::as_u64) {
        Some(BOOK_FORMAT) => {}
        other => return Err(format!("format {other:?}, this program reads {BOOK_FORMAT}")),
    }
    if v.get("lang").and_then(Value::as_str) != Some(lang) {
        return Err(format!("\"lang\" is not {lang}"));
    }
    let texts = v.get("texts").and_then(Value::as_object).ok_or("no \"texts\"")?;
    let mut out = BookFile::default();
    for (key, t) in texts {
        if !valid_key(key) {
            return Err(format!("key {key:?}: lower-case letters, digits and _ only"));
        }
        let text = match t {
            Value::String(s) => s.clone(),
            Value::Array(lines) => {
                let mut parts = Vec::with_capacity(lines.len());
                for l in lines {
                    parts.push(l.as_str().ok_or_else(|| format!("{key}: a line that is not a string"))?);
                }
                parts.join("\n")
            }
            _ => return Err(format!("{key}: neither a string nor a list of lines")),
        };
        out.texts.insert(key.clone(), text);
    }
    match v.get("locked") {
        None => {}
        Some(Value::Array(keys)) => {
            for k in keys {
                let k = k.as_str().ok_or("\"locked\": a key that is not a string")?;
                if !out.texts.contains_key(k) {
                    return Err(format!("\"locked\": {k:?} is not a key of this file"));
                }
                out.locked.insert(k.to_string());
            }
        }
        Some(_) => return Err("\"locked\" is not a list of keys".into()),
    }
    Ok(out)
}

/// The built-in texts, by language; a file that does not read is left out
/// and named in errors (the tests keep that from shipping).
#[derive(Debug, Default)]
pub struct BuiltinBook {
    pub langs: BTreeMap<String, BookFile>,
    pub errors: Vec<String>,
}

impl BuiltinBook {
    pub fn text(&self, lang: &str, key: &str) -> Option<&str> {
        self.langs.get(lang)?.texts.get(key).map(String::as_str)
    }

    pub fn locked(&self, lang: &str, key: &str) -> bool {
        self.langs.get(lang).is_some_and(|f| f.locked.contains(key))
    }

    /// SHA-256 of what the files say (languages and keys in order, locked
    /// marks included), whatever their spacing or line endings.
    pub fn digest(&self) -> String {
        let mut buf = Vec::new();
        for (lang, file) in &self.langs {
            for (key, text) in &file.texts {
                let locked = if file.locked.contains(key) { "locked" } else { "" };
                for part in [lang.as_str(), key.as_str(), locked, text.as_str()] {
                    buf.extend_from_slice(&(part.len() as u64).to_le_bytes());
                    buf.extend_from_slice(part.as_bytes());
                }
            }
        }
        sha256_hex(&buf)
    }
}

/// The texts this build carries.
pub fn builtin_book() -> &'static BuiltinBook {
    static BOOK: OnceLock<BuiltinBook> = OnceLock::new();
    BOOK.get_or_init(|| {
        let mut b = BuiltinBook::default();
        for (lang, json) in BUILTIN_FILES {
            match parse_book_file(lang, json) {
                Ok(f) => {
                    b.langs.insert(lang.to_string(), f);
                }
                Err(e) => b.errors.push(format!("{lang}.json: {e}")),
            }
        }
        b
    })
}

// ---- one changed text --------------------------------------------------------------------------

/// Why a changed text is not used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextProblem {
    /// One of the codes in [rule].
    pub rule: &'static str,
    pub why: String,
}

impl fmt::Display for TextProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.rule, self.why)
    }
}

fn problem(rule: &'static str, why: impl Into<String>) -> TextProblem {
    TextProblem { rule, why: why.into() }
}

/// The {name} placeholders of a text (letters, digits and _ between braces).
pub fn placeholders(s: &str) -> BTreeSet<&str> {
    let mut out = BTreeSet::new();
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let name_len: usize = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
        if name_len > 0 && after[name_len..].starts_with('}') {
            out.insert(&after[..name_len]);
        }
        rest = after;
    }
    out
}

fn leading_space(s: &str) -> &str {
    &s[..s.len() - s.trim_start().len()]
}

fn trailing_space(s: &str) -> &str {
    &s[s.trim_end().len()..]
}

fn bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Characters nobody sees on a page but a model reads: zero-width and joining
/// marks, the soft hyphen, line and paragraph separators, tag characters.
fn hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{2028}' | '\u{2029}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}'
    )
}

/// Whether new may take the place of builtin (the built-in text of the same
/// key). The reason names no words of either text.
pub fn check_text(builtin: &str, new: &str) -> Result<(), TextProblem> {
    if new.trim().is_empty() {
        return Err(problem(rule::REQUIRED, "empty"));
    }
    // A hidden character is only kept where the built-in text has the same one
    // (a script that joins letters with it).
    let refused =
        |c: &char| (c.is_control() && *c != '\n') || *c == '\u{FFFD}' || bidi_control(*c) || (hidden(*c) && !builtin.contains(*c));
    if let Some(c) = new.chars().find(refused) {
        return Err(problem(rule::CHARSET, format!("character U+{:04X} is not allowed", c as u32)));
    }
    let (had, has) = (placeholders(builtin), placeholders(new));
    if had != has {
        let missing: Vec<_> = had.difference(&has).map(|p| format!("{{{p}}}")).collect();
        let extra: Vec<_> = has.difference(&had).map(|p| format!("{{{p}}}")).collect();
        // Names come from the download: what is logged of them is kept short.
        let short = |v: Vec<String>| v.join(" ").chars().take(120).collect::<String>();
        return Err(problem(
            rule::PLACEHOLDER,
            format!("placeholders differ (missing: {}; not in the built-in text: {})", short(missing), short(extra)),
        ));
    }
    if leading_space(new) != leading_space(builtin) || trailing_space(new) != trailing_space(builtin) {
        return Err(problem(rule::FORMAT, "spaces or line breaks at its start or end differ from the built-in text"));
    }
    let (b, n) = (builtin.chars().count(), new.chars().count());
    let max = (b * 2).max(b + 400);
    if n > max {
        return Err(problem(rule::LENGTH, format!("{n} characters, at most {max} for this text")));
    }
    Ok(())
}

// ---- the download ------------------------------------------------------------------------------

/// A text as written: a string, or a list of lines joined with a line break.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PromptText {
    One(String),
    Lines(Vec<String>),
}

impl PromptText {
    pub fn joined(&self) -> String {
        match self {
            PromptText::One(s) => s.clone(),
            PromptText::Lines(l) => l.join("\n"),
        }
    }
}

/// bageo-prompts.json.gz: the texts that differ from the built-in ones, by
/// language and key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptsPack {
    pub format: u32,
    /// YYYYMMDDNN.
    pub version: u64,
    pub built_at: String,
    pub source_commit: String,
    pub langs: BTreeMap<String, BTreeMap<String, PromptText>>,
}

impl PromptsPack {
    /// How many texts it carries.
    pub fn count(&self) -> usize {
        self.langs.values().map(BTreeMap::len).sum()
    }
}

pub fn pack_prompts(p: &PromptsPack) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(p).map_err(|e| e.to_string())?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&json).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

fn one(r: &'static str, field: &str, d: impl Into<String>) -> Vec<Issue> {
    vec![Issue::new(PROMPTS_FILE, field, r, d)]
}

/// Unpacks gz, stopping as soon as it passes [MAX_PROMPTS_BYTES]. The file
/// is exactly one gzip member: the decoder reads the slice itself, so any
/// byte after the member is still there to be refused.
fn unpack(gz: &[u8]) -> Result<Vec<u8>, Vec<Issue>> {
    if gz.len() > MAX_PROMPTS_PACKED {
        return Err(one(rule::LENGTH, "", format!("{} bytes packed, the limit is {MAX_PROMPTS_PACKED}", gz.len())));
    }
    let mut dec = GzDecoder::new(gz);
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = dec.read(&mut buf).map_err(|e| one(rule::FORMAT, "", format!("cannot unpack: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len() + n > MAX_PROMPTS_BYTES {
            return Err(one(rule::LENGTH, "", format!("more than {MAX_PROMPTS_BYTES} bytes unpacked; stopped reading")));
        }
        out.extend_from_slice(&buf[..n]);
    }
    let rest = dec.into_inner();
    if !rest.is_empty() {
        return Err(one(rule::FORMAT, "", format!("{} bytes after the packed data", rest.len())));
    }
    Ok(out)
}

/// Reads a download: its size, format, fields and version. The texts in it
/// are checked one by one where they are used ([check_text]).
pub fn parse_prompts(gz: &[u8]) -> Result<PromptsPack, Vec<Issue>> {
    let raw = unpack(gz)?;
    let val: Value = serde_json::from_slice(&raw).map_err(|e| one(rule::FORMAT, "", e.to_string()))?;
    match val.get("format").and_then(Value::as_u64) {
        Some(n) if n > u64::from(PROMPTS_FORMAT) => {
            return Err(one(rule::FORMAT, "format", format!("format {n} is newer than this Bageo reads ({PROMPTS_FORMAT}); update Bageo first")));
        }
        Some(n) if n == u64::from(PROMPTS_FORMAT) => {}
        Some(n) => return Err(one(rule::FORMAT, "format", format!("format {n} is not {PROMPTS_FORMAT}"))),
        None => return Err(one(rule::FORMAT, "format", "missing")),
    }
    let pack: PromptsPack = serde_json::from_value(val).map_err(|e| one(rule::FORMAT, "", e.to_string()))?;
    if !valid_version(pack.version, now_days()) {
        return Err(one(rule::STATUS, "version", format!("{} is not YYYYMMDDNN", pack.version)));
    }
    if !valid_rfc3339_utc(&pack.built_at) {
        return Err(one(rule::STATUS, "built_at", "must look like 2026-11-22T08:00:00Z"));
    }
    if !valid_commit(&pack.source_commit) {
        return Err(one(rule::FORMAT, "source_commit", "invalid commit string"));
    }
    Ok(pack)
}

// ---- the release index -------------------------------------------------------------------------

/// Every field of the index's prompts entry.
pub fn check_prompts_ref(r: &LocalesRef) -> Vec<Issue> {
    let mut out = Vec::new();
    let f = INDEX_FILE;
    if r.format != PROMPTS_FORMAT {
        push(&mut out, f, "prompts.format", rule::FORMAT, format!("format {} is not {PROMPTS_FORMAT}", r.format));
    }
    if !valid_version(r.version, now_days()) {
        push(&mut out, f, "prompts.version", rule::STATUS, format!("{} is not YYYYMMDDNN", r.version));
    }
    if !plain_file_name(&r.file) || !r.file.ends_with(".json.gz") {
        push(&mut out, f, "prompts.file", rule::RELEASE_INDEX, "must be a plain .json.gz file name");
    }
    if r.bytes == 0 || r.bytes > MAX_PROMPTS_PACKED as u64 {
        push(&mut out, f, "prompts.bytes", rule::LENGTH, format!("{} bytes, must be 1 to {MAX_PROMPTS_PACKED}", r.bytes));
    }
    if !is_sha256_hex(&r.sha256) {
        push(&mut out, f, "prompts.sha256", rule::RELEASE_INDEX, "must be 64 lower-case hex digits");
    }
    if !valid_min_bageo(&r.min_bageo) {
        push(&mut out, f, "prompts.min_bageo", rule::RELEASE_INDEX, "must look like 1.6.9");
    }
    out
}

/// The index's prompts entry when every field of it is sound; otherwise None
/// and one issue saying why it was left out.
pub fn sound_prompts(ix: &Index) -> (Option<LocalesRef>, Option<Issue>) {
    let Some(p) = &ix.prompts else { return (None, None) };
    let bad = check_prompts_ref(p);
    if bad.is_empty() {
        return (Some(p.clone()), None);
    }
    let why = bad.iter().map(|i| format!("{}: {}", i.field, i.detail)).collect::<Vec<_>>().join("; ");
    let note = Issue::new(INDEX_FILE, "prompts", rule::RELEASE_INDEX, format!("the prompts entry is left out ({why}); {PROMPTS_FILE} is not downloaded"));
    (None, Some(note))
}

// ---- building the download ---------------------------------------------------------------------

/// Changed texts by language and key.
pub type PromptChanges = BTreeMap<String, BTreeMap<String, String>>;

/// The texts prompts/<language>.json in dir change, each checked against
/// the built-in one: a key Bageo does not have, a change to a locked text or
/// a text that fails [check_text] is an issue. Texts equal to the built-in
/// ones are left out. Ok(None): dir has no prompts folder.
pub fn load_prompt_changes(dir: &Path) -> Result<Option<PromptChanges>, Vec<Issue>> {
    let folder = dir.join("prompts");
    if !folder.is_dir() {
        return Ok(None);
    }
    let book = builtin_book();
    let mut issues = Vec::new();
    let mut changes = PromptChanges::new();
    let paths = sorted_paths(&folder).map_err(|e| one(rule::FORMAT, "", format!("prompts: cannot read folder: {e}")))?;
    for p in paths.iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json")) {
        let lang = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let rel = format!("prompts/{lang}.json");
        let Some(builtin) = book.langs.get(&lang) else {
            push(&mut issues, &rel, "", rule::CODE, format!("Bageo has no texts in \"{lang}\""));
            continue;
        };
        let text = match fs::metadata(p).map(|m| m.len()) {
            Ok(n) if n > MAX_PROMPTS_BYTES as u64 => {
                push(&mut issues, &rel, "", rule::LENGTH, format!("{n} bytes, the limit is {MAX_PROMPTS_BYTES}"));
                continue;
            }
            _ => match fs::read_to_string(p) {
                Ok(t) => t,
                Err(e) => {
                    push(&mut issues, &rel, "", rule::FORMAT, e.to_string());
                    continue;
                }
            },
        };
        let file = match parse_book_file(&lang, &text) {
            Ok(f) => f,
            Err(e) => {
                push(&mut issues, &rel, "", rule::FORMAT, e);
                continue;
            }
        };
        for (key, new) in &file.texts {
            let field = format!("texts.{key}");
            let Some(old) = builtin.texts.get(key) else {
                push(&mut issues, &rel, &field, rule::CODE, "not a key Bageo knows");
                continue;
            };
            if new == old {
                continue;
            }
            if builtin.locked.contains(key) {
                push(&mut issues, &rel, &field, rule::CODE, "locked: only a new Bageo changes this text");
                continue;
            }
            if let Err(bad) = check_text(old, new) {
                push(&mut issues, &rel, &field, bad.rule, bad.why);
                continue;
            }
            changes.entry(lang.clone()).or_default().insert(key.clone(), new.clone());
        }
    }
    if issues.is_empty() {
        Ok(Some(changes))
    } else {
        Err(issues)
    }
}

/// The download for dir's prompts folder (Ok(None): there is none).
pub fn build_prompts_at(dir: &Path, version: u64, commit: &str, built_at: &str) -> Result<Option<PromptsPack>, Vec<Issue>> {
    let mut issues = Vec::new();
    if !valid_version(version, now_days()) {
        push(&mut issues, PROMPTS_FILE, "version", rule::STATUS, format!("{version} is not YYYYMMDDNN"));
    }
    if !valid_commit(commit) {
        push(&mut issues, PROMPTS_FILE, "source_commit", rule::FORMAT, "invalid commit string");
    }
    if !valid_rfc3339_utc(built_at) {
        push(&mut issues, PROMPTS_FILE, "built_at", rule::STATUS, "must look like 2026-11-22T08:00:00Z");
    }
    let changes = match load_prompt_changes(dir) {
        Ok(Some(c)) => c,
        Ok(None) => return if issues.is_empty() { Ok(None) } else { Err(issues) },
        Err(more) => {
            issues.extend(more);
            return Err(issues);
        }
    };
    if !issues.is_empty() {
        return Err(issues);
    }
    let langs = changes
        .into_iter()
        .map(|(lang, texts)| (lang, texts.into_iter().map(|(k, t)| (k, PromptText::One(t))).collect()))
        .collect();
    let pack = PromptsPack { format: PROMPTS_FORMAT, version, built_at: built_at.to_string(), source_commit: commit.to_string(), langs };
    let raw_len = serde_json::to_vec(&pack).map(|b| b.len()).unwrap_or(usize::MAX);
    if raw_len > MAX_PROMPTS_BYTES {
        return Err(one(rule::LENGTH, "", format!("{raw_len} bytes unpacked, the limit is {MAX_PROMPTS_BYTES}")));
    }
    Ok(Some(pack))
}

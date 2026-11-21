// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Translations for the languages outside the nine core ones: one file
//! (bageo-locales.json.gz) built by bageo-packs from the public language
//! folders. Bageo ships one (data/packs/bageo-locales.json.gz); a newer one
//! downloaded by "Check for updates" is kept next to the local data pack in
//! {CALLS_DATA_DIR}/packs/ and used once it reads back.
//!
//! The file is read leniently ([bageo_packs::locales::sanitize_locales]): a
//! key that breaks a rule is dropped on its own (that line reads English), a
//! language whose names are broken is left out. How far each language goes is
//! worked out here, never taken from the file ([Stage]):
//! - Call: its names and every required line of its call speech passed, and
//!   it has a hello. The AI answers and talks in it.
//! - Notify: Call, and at least [READY_PERCENT] % of Bageo's notification
//!   keys passed. Notifications, summaries and chat replies use it; a missing
//!   key reads English.
//!
//! Like the local data pack, a table in use is never freed (Box::leak): a call
//! keeps its &'static phrases and prompts for its whole length, the next call
//! reads the new table. Installs per run are capped ([MAX_INSTALLS]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

use bageo_packs::locales::{
    lrule, parse_locales_at, sanitize_locales, Closing, EnSource, EnText, English, Kind, LocaleLang, Locales, Meta, Names, Speech,
    MAX_LOCALES_BYTES, READY_PERCENT,
};
use bageo_packs::Issue;
use serde_json::{json, Value};

use crate::closing::ExtraWords;
use crate::locale::{Ext, Lang, Phrases, Words};
use crate::prompts::Pack;

/// The translations compiled into Bageo.
const BUILTIN: &[u8] = include_bytes!("../data/packs/bageo-locales.json.gz");

/// Bageo's English notification text and its notes: what a translation is
/// checked against.
const NOTIFY_EN: &str = include_str!("../data/i18n/notify/en.json");
const NOTIFY_META: &str = include_str!("../data/i18n/notify/_meta.json");

/// The installed file's name, in {CALLS_DATA_DIR}/packs/.
pub const FILE_NAME: &str = "bageo-locales.json.gz";

/// Most updates of the translations installed in one run; after that Bageo
/// has to be restarted. Counted apart from the local data pack's.
pub const MAX_INSTALLS: usize = 8;

const ISSUE_FILE: &str = "bageo-locales.json.gz";
const INCOMING_PREFIX: &str = ".incoming-locales-";
const INCOMING_SUFFIX: &str = ".json.gz";

/// The review state that drops "not yet checked by a native speaker".
pub const REVIEWED: &str = "local_reviewed";

/// How far a language goes, worked out when the file is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    None,
    /// The AI answers and talks in it.
    Call,
    /// Call, and what the owner reads is in it too.
    Notify,
}

impl Stage {
    pub fn code(self) -> &'static str {
        match self {
            Stage::None => "none",
            Stage::Call => "call",
            Stage::Notify => "notify",
        }
    }
}

/// One language of a table.
pub struct ExtLocale {
    pub names: Names,
    /// The lowest review state of what was packed.
    pub status: String,
    pub stage: Stage,
    /// Keys and lines the checks dropped (they read English).
    pub dropped: usize,
    zh_tw: &'static str,
    zh_cn: &'static str,
    phrases: Option<Phrases>,
    words: Option<Words>,
    /// The call prompts, built for the first call in this language and kept
    /// with this table: a newer table builds its own.
    prompt: OnceLock<Pack>,
}

impl ExtLocale {
    /// A native speaker checked it.
    pub fn reviewed(&self) -> bool {
        self.status == REVIEWED
    }
}

/// A translations file in use.
pub struct LocaleTable {
    /// YYYYMMDDNN; 0 for the empty table used when the built-in one is broken.
    pub version: u64,
    langs: BTreeMap<Ext, ExtLocale>,
    /// Closing words that passed every check, for [crate::closing].
    closing: ExtraWords,
    /// Each language's farewells that passed their examples, for telling the
    /// AI's own goodbye ([crate::closing::ai_says_translated_farewell]).
    ai_farewells: Vec<(&'static str, &'static [&'static str])>,
}

impl LocaleTable {
    pub fn stage(&self, e: Ext) -> Stage {
        self.langs.get(&e).map_or(Stage::None, |l| l.stage)
    }

    pub fn get(&self, e: Ext) -> Option<&ExtLocale> {
        self.langs.get(&e)
    }

    pub fn langs(&self) -> impl Iterator<Item = (Ext, &ExtLocale)> {
        self.langs.iter().map(|(e, l)| (*e, l))
    }

    /// The call phrases of a language at Call or further.
    pub fn phrases(&'static self, e: Ext) -> Option<&'static Phrases> {
        self.langs.get(&e)?.phrases.as_ref()
    }

    /// The words the owner reads, for a language at Notify.
    pub fn words(&'static self, e: Ext) -> Option<&'static Words> {
        self.langs.get(&e)?.words.as_ref()
    }

    /// The language's name in Chinese instructions.
    pub fn zh_name(&'static self, e: Ext, simplified: bool) -> Option<&'static str> {
        let l = self.langs.get(&e)?;
        Some(if simplified { l.zh_cn } else { l.zh_tw })
    }

    /// The call prompts of a language at Call or further, built once per table.
    pub(crate) fn prompt_pack(&'static self, e: Ext, build: impl FnOnce() -> Pack) -> Option<&'static Pack> {
        let l = self.langs.get(&e).filter(|l| l.stage >= Stage::Call)?;
        Some(l.prompt.get_or_init(build))
    }

    pub(crate) fn closing_words(&'static self) -> &'static ExtraWords {
        &self.closing
    }

    pub(crate) fn ai_farewells(&'static self) -> &'static [(&'static str, &'static [&'static str])] {
        &self.ai_farewells
    }

    fn new(loc: Locales, issues: &[Issue]) -> Self {
        let en_keys = english().files.get(&Kind::Notify).map_or(0, |s| s.text.keys().count());
        let mut langs = BTreeMap::new();
        let mut verified = Vec::new();
        let mut ai_farewells = Vec::new();
        for (code, lang) in loc.langs {
            let Some(e) = Ext::from_code(&code) else { continue };
            let at = format!("langs.{code}");
            // One per field, however many rules it broke.
            let dropped = issues
                .iter()
                .filter(|i| (i.field == at || i.field.starts_with(&format!("{at}."))) && !i.field.starts_with(&format!("{at}.console")))
                .map(|i| i.field.as_str())
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            let LocaleLang { names, status, speech, notify, .. } = lang;
            let mut phrases = speech.as_ref().and_then(|s| call_phrases(e, s));
            let notify_keys = notify.as_ref().map_or(0, |f| f.keys().count());
            let words = match &notify {
                Some(f) if phrases.is_some() && en_keys > 0 && notify_keys * 100 >= en_keys * READY_PERCENT => {
                    Some(crate::locale::words_over_english(&serde_json::to_string(f).unwrap_or_default()))
                }
                _ => None,
            };
            let stage = match (&phrases, &words) {
                (Some(_), Some(_)) => Stage::Notify,
                (Some(_), None) => Stage::Call,
                _ => Stage::None,
            };
            let closing = speech.as_ref().and_then(|s| s.closing.as_ref());
            if let (Some(p), Some(c)) = (phrases.as_mut(), closing) {
                if let Some(words) = ExtraWords::ai_farewells(e.code(), c) {
                    p.farewells = words;
                    ai_farewells.push((e.code(), words));
                }
            }
            if stage >= Stage::Call && status == REVIEWED {
                if let Some(c) = closing.filter(|c| c.verified) {
                    verified.push((e.code(), c.clone()));
                }
            }
            let zh_tw = leak(names.zh.clone());
            let zh_cn = leak(crate::zh_convert::to_cn(&names.zh));
            langs.insert(e, ExtLocale { names, status, stage, dropped, zh_tw, zh_cn, phrases, words, prompt: OnceLock::new() });
        }
        let closing = ExtraWords::from_verified(&verified);
        LocaleTable { version: loc.version, langs, closing, ai_farewells }
    }

    fn empty() -> Self {
        LocaleTable { version: 0, langs: BTreeMap::new(), closing: ExtraWords::default(), ai_farewells: Vec::new() }
    }
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// The call phrases of a language whose speech passed: its hello is required
/// (the opening line), its other lines fill the places English's do.
fn call_phrases(e: Ext, s: &Speech) -> Option<Phrases> {
    let hello = s.hello.as_deref().map(str::trim).filter(|h| !h.is_empty())?;
    if s.preview.len() != 3 {
        return None;
    }
    let en = crate::locale::phrases(Lang::En);
    let again = s.hello_again.as_deref().map(str::trim).filter(|h| !h.is_empty()).unwrap_or(hello);
    Some(Phrases {
        name_en: Lang::Ext(e).name_en(),
        stt: e.official(),
        hello: leak(hello.to_string()),
        hello_again: leak(again.to_string()),
        im_here: leak(s.im_here.clone()),
        still_there: leak(s.still_there.clone()),
        bye: leak(s.bye.clone()),
        go_ahead: leak(s.go_ahead.clone()),
        no_thanks: leak(s.no_thanks.clone()),
        think_about_it: leak(s.think_about_it.clone()),
        wait_one_more: leak(s.wait_one_more.clone()),
        no_wait: leak(s.no_wait.clone()),
        owner_fallback: en.owner_fallback,
        unavailable: leak(s.unavailable.clone()),
        preview: [leak(s.preview[0].clone()), leak(s.preview[1].clone()), leak(s.preview[2].clone())],
        // Set from the closing once its examples pass (LocaleTable::new); the
        // caller's side never reads these (unchecked words end no call).
        farewells: &[],
        wrapups: &[],
    })
}

/// Bageo's English for the notifications, the way the checks want it.
fn english() -> &'static English {
    static E: OnceLock<English> = OnceLock::new();
    E.get_or_init(|| {
        let text: EnText = serde_json::from_str(NOTIFY_EN).unwrap_or_default();
        let meta: BTreeMap<String, Meta> = serde_json::from_str(NOTIFY_META).unwrap_or_default();
        let mut files = BTreeMap::new();
        files.insert(Kind::Notify, EnSource { text, meta });
        English { files }
    })
}

/// Where a translations file comes from. The built-in one ships with the
/// program, so the "not later than tomorrow" limit on its version does not
/// apply to it: a computer whose clock is behind still gets its languages.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Builtin,
    Downloaded,
}

/// "Today" for a file with no upper limit on its date.
const NO_DATE_LIMIT: i64 = i64::MAX / 2;

/// Reads a downloaded or installed translations file the way Bageo uses it:
/// size while unpacking, format number and header, then every language
/// leniently. Returns the translations and what was dropped.
pub fn parse(gz: &[u8]) -> Result<(Locales, Vec<Issue>), Vec<Issue>> {
    parse_from(gz, Origin::Downloaded, bageo_packs::util::now_days())
}

/// Same, with the file's origin and today (days since 1970-01-01).
fn parse_from(gz: &[u8], origin: Origin, today: i64) -> Result<(Locales, Vec<Issue>), Vec<Issue>> {
    let today = if origin == Origin::Builtin { NO_DATE_LIMIT } else { today };
    let mut loc = parse_locales_at(gz, MAX_LOCALES_BYTES, today)?;
    let judge = |code: &str, c: &Closing, said: &str| crate::closing::judge(code, c, said);
    let mut dropped = sanitize_locales(&mut loc, english(), Some(&judge));
    // The console is not translated by this file yet: its absence is no loss.
    dropped.retain(|i| !(i.rule == lrule::KEY && i.field.ends_with(".console")));
    Ok((loc, dropped))
}

fn table(gz: &[u8]) -> Result<LocaleTable, Vec<Issue>> {
    let (loc, dropped) = parse(gz)?;
    Ok(LocaleTable::new(loc, &dropped))
}

fn table_from(gz: &[u8], origin: Origin, today: i64) -> Result<LocaleTable, Vec<Issue>> {
    let (loc, dropped) = parse_from(gz, origin, today)?;
    Ok(LocaleTable::new(loc, &dropped))
}

fn issue(rule: &'static str, detail: impl Into<String>) -> Vec<Issue> {
    vec![Issue::new(ISSUE_FILE, "", rule, detail)]
}

/// The built-in table. One that does not read (a build mistake, caught by the
/// tests) leaves an empty table: only the core languages are offered.
pub fn builtin() -> &'static LocaleTable {
    static T: OnceLock<&'static LocaleTable> = OnceLock::new();
    T.get_or_init(|| match table_from(BUILTIN, Origin::Builtin, 0) {
        Ok(t) => Box::leak(Box::new(t)),
        Err(issues) => {
            tracing::error!("built-in translations rejected: {}", issues.first().map(|i| i.to_string()).unwrap_or_default());
            Box::leak(Box::new(LocaleTable::empty()))
        }
    })
}

static CURRENT: RwLock<Option<&'static LocaleTable>> = RwLock::new(None);
static INSTALLS: AtomicUsize = AtomicUsize::new(0);
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

// A test's own table: installing in one test never changes what the others read.
#[cfg(test)]
thread_local! {
    static TEST_CURRENT: std::cell::Cell<Option<&'static LocaleTable>> = const { std::cell::Cell::new(None) };
}

/// The folder the installed file and its download are kept in (the local data
/// pack's folder).
pub(crate) fn installed_dir() -> PathBuf {
    crate::packs_store::installed_dir()
}

/// A new name for a download in the packs folder (.incoming-locales-<pid>-<random>.json.gz;
/// start-up clears the ones Bageo did not finish).
pub(crate) fn incoming_path(packs: &Path) -> PathBuf {
    packs.join(format!("{INCOMING_PREFIX}{}-{}{INCOMING_SUFFIX}", std::process::id(), uuid::Uuid::new_v4().simple()))
}

fn is_incoming(name: &str) -> bool {
    name.starts_with(INCOMING_PREFIX) && name.ends_with(INCOMING_SUFFIX)
}

/// A download is still in the packs folder (tests).
#[cfg(test)]
pub(crate) fn has_incoming(packs: &Path) -> bool {
    std::fs::read_dir(packs).is_ok_and(|d| d.flatten().any(|e| is_incoming(&e.file_name().to_string_lossy())))
}

/// Updates that can still be installed before Bageo has to be restarted.
pub(crate) fn installs_left() -> usize {
    MAX_INSTALLS.saturating_sub(INSTALLS.load(Ordering::SeqCst))
}

/// Calls use the built-in translations.
pub(crate) fn builtin_in_use() -> bool {
    std::ptr::eq(current(), builtin())
}

fn set_current(t: &'static LocaleTable) {
    #[cfg(test)]
    {
        TEST_CURRENT.with(|c| c.set(Some(t)));
    }
    #[cfg(not(test))]
    {
        *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(t);
    }
}

/// The table calls use now: an installed file newer than the built-in one
/// that reads back, else the built-in one. Unit tests get the built-in one
/// unless the test itself installed, restored or chose one.
pub fn current() -> &'static LocaleTable {
    #[cfg(test)]
    {
        if let Some(t) = TEST_CURRENT.with(|c| c.get()) {
            return t;
        }
    }
    if let Some(t) = *CURRENT.read().unwrap_or_else(|e| e.into_inner()) {
        return t;
    }
    let mut slot = CURRENT.write().unwrap_or_else(|e| e.into_inner());
    if let Some(t) = *slot {
        return t;
    }
    let t = first_table();
    *slot = Some(t);
    t
}

#[cfg(not(test))]
fn first_table() -> &'static LocaleTable {
    load_installed(&installed_dir(), builtin().version).unwrap_or_else(builtin)
}

#[cfg(test)]
fn first_table() -> &'static LocaleTable {
    builtin()
}

/// How far a language goes now; the core languages go all the way.
pub fn stage(l: Lang) -> Stage {
    match l {
        Lang::Ext(e) => current().stage(e),
        _ => Stage::Notify,
    }
}

/// The language a settings code stands for when the given stage is needed: a
/// core language as [Lang::parse] reads it; another official language only
/// when it goes that far now, else English (the stored code is left as it is).
pub fn lang_at(code: &str, need: Stage) -> Lang {
    match Lang::parse_any(code) {
        Some(Lang::Ext(e)) if current().stage(e) >= need => Lang::Ext(e),
        Some(Lang::Ext(_)) => Lang::En,
        _ => Lang::parse(code),
    }
}

/// The installed file in packs when it is newer than version and reads back.
/// Unfinished downloads are cleared.
fn load_installed(packs: &Path, version: u64) -> Option<&'static LocaleTable> {
    if let Ok(entries) = std::fs::read_dir(packs) {
        for e in entries.flatten() {
            if is_incoming(&e.file_name().to_string_lossy()) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let path = packs.join(FILE_NAME);
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > MAX_LOCALES_BYTES as u64 {
        tracing::warn!("installed translations too large ({} bytes); using the built-in ones", meta.len());
        return None;
    }
    let gz = std::fs::read(&path).ok()?;
    match table(&gz) {
        Ok(t) if t.version > version => {
            tracing::info!("translations {} in use ({} languages)", t.version, t.langs.len());
            Some(Box::leak(Box::new(t)))
        }
        Ok(t) => {
            tracing::info!("installed translations {} are not newer than the built-in {version}; using the built-in ones", t.version);
            None
        }
        Err(issues) => {
            tracing::warn!(
                "installed translations rejected ({} issue(s), first: {}); using the built-in ones",
                issues.len(),
                issues.first().map(|i| i.to_string()).unwrap_or_default()
            );
            None
        }
    }
}

/// Checks and installs a downloaded file, already written to staged in the
/// packs folder ([incoming_path]): it must read back, be the version the
/// release index named (want) and be newer than the table in use. It is then
/// renamed over the installed one in one step; the next call uses it. The
/// staged file never stays behind. At most [MAX_INSTALLS] per run.
pub(crate) fn install_file(staged: &Path, want: u64) -> Result<u64, Vec<Issue>> {
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = install_file_counted(&INSTALLS, &installed_dir(), staged, current().version, Some(want))?;
    set_current(t);
    Ok(t.version)
}

pub(crate) fn install_file_counted(count: &AtomicUsize, packs: &Path, staged: &Path, in_use: u64, want: Option<u64>) -> Result<&'static LocaleTable, Vec<Issue>> {
    let result = check_staged(count, staged, in_use, want).and_then(|t| {
        std::fs::rename(staged, packs.join(FILE_NAME)).map_err(|e| issue(lrule::FORMAT, format!("cannot install the translations: {e}")))?;
        count.fetch_add(1, Ordering::SeqCst);
        Ok(t)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result
}

fn check_staged(count: &AtomicUsize, staged: &Path, in_use: u64, want: Option<u64>) -> Result<&'static LocaleTable, Vec<Issue>> {
    if count.load(Ordering::SeqCst) >= MAX_INSTALLS {
        return Err(issue(lrule::FORMAT, format!("{MAX_INSTALLS} updates installed since Bageo started; restart Bageo first")));
    }
    let io = |e: std::io::Error| issue(lrule::FORMAT, format!("cannot read the translations: {e}"));
    let len = std::fs::metadata(staged).map_err(io)?.len();
    if len > MAX_LOCALES_BYTES as u64 {
        return Err(issue(lrule::SIZE, format!("{len} bytes, the limit is {MAX_LOCALES_BYTES}")));
    }
    let gz = std::fs::read(staged).map_err(io)?;
    let t = table(&gz)?;
    if want.is_some_and(|w| w != t.version) {
        return Err(issue(lrule::FORMAT, format!("version {} is not the one the index names", t.version)));
    }
    if t.version <= in_use {
        return Err(issue(lrule::FORMAT, format!("version {} is not newer than the one in use ({in_use})", t.version)));
    }
    Ok(Box::leak(Box::new(t)))
}

/// Goes back to the built-in translations: the installed file is deleted.
/// When it cannot be deleted nothing changes and the error is returned.
pub fn restore_builtin() -> std::io::Result<()> {
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    restore_in(&installed_dir())
}

fn restore_in(packs: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(packs.join(FILE_NAME)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => {
            set_current(builtin());
            Ok(())
        }
    }
}

/// The core languages' names for the menus: (language, own name, English, Chinese).
const CORE_NAMES: [(Lang, &str, &str, &str); 9] = [
    (Lang::En, "English", "English", "英文"),
    (Lang::ZhTw, "繁體中文", "Traditional Chinese", "繁體中文"),
    (Lang::ZhCn, "简体中文", "Simplified Chinese", "簡體中文"),
    (Lang::Ja, "日本語", "Japanese", "日文"),
    (Lang::Ko, "한국어", "Korean", "韓文"),
    (Lang::Es, "Español", "Spanish", "西班牙文"),
    (Lang::Fr, "Français", "French", "法文"),
    (Lang::De, "Deutsch", "German", "德文"),
    (Lang::Pt, "Português", "Portuguese", "葡萄牙文"),
];

/// The languages the console offers (settings API "languages"): the core ones
/// (stage "console"), then every other language at Call or further, with its
/// names, writing direction, stage and whether a native speaker checked it.
pub fn menu() -> Vec<Value> {
    let mut out: Vec<Value> = CORE_NAMES
        .iter()
        .map(|(l, native, en, zh)| json!({"code": l.code(), "native": native, "en": en, "zh": zh, "dir": "ltr", "stage": "console", "reviewed": true}))
        .collect();
    for (e, l) in current().langs() {
        if l.stage < Stage::Call {
            continue;
        }
        out.push(json!({
            "code": e.code(),
            "native": l.names.native,
            "en": l.names.en,
            "zh": l.names.zh,
            "dir": if Lang::Ext(e).rtl() { "rtl" } else { "ltr" },
            "stage": l.stage.code(),
            "reviewed": l.reviewed(),
        }));
    }
    out
}

/// Test translations (it, th, ar, fa, ru), only ever built in tests: the
/// names and call speech of crates/packs/tests/fixtures/locales, and made-up
/// notification text for it and ar so they reach Notify.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    const FIX: [(&str, &str, &str); 4] = [
        (
            "it",
            include_str!("../../packs/tests/fixtures/locales/languages/it/names.json"),
            include_str!("../../packs/tests/fixtures/locales/languages/it/call.json"),
        ),
        (
            "th",
            include_str!("../../packs/tests/fixtures/locales/languages/th/names.json"),
            include_str!("../../packs/tests/fixtures/locales/languages/th/call.json"),
        ),
        (
            "ar",
            include_str!("../../packs/tests/fixtures/locales/languages/ar/names.json"),
            include_str!("../../packs/tests/fixtures/locales/languages/ar/call.json"),
        ),
        (
            "fa",
            include_str!("../../packs/tests/fixtures/locales/languages/fa/names.json"),
            include_str!("../../packs/tests/fixtures/locales/languages/fa/call.json"),
        ),
    ];

    /// Keys left out of the Italian notifications: they read English.
    pub(crate) const IT_MISSING: [&str; 2] = ["n_test_body", "s_nothing"];

    fn placeholders(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            let Some(j) = rest[i..].find('}') else { break };
            out.push(&rest[i..i + j + 1]);
            rest = &rest[i + j + 1..];
        }
        out
    }

    /// A made-up translation of English text: a word of the language and the
    /// English placeholders; text without letters stays as it is.
    fn made_up(word: &str, en: &str) -> String {
        let ph = placeholders(en);
        let mut outside = en.to_string();
        for p in &ph {
            outside = outside.replace(p, "");
        }
        if ph.is_empty() && !outside.chars().any(char::is_alphabetic) {
            return en.to_string();
        }
        std::iter::once(word).chain(ph.iter().copied()).collect::<Vec<_>>().join(" ")
    }

    fn notify(word: &str, skip: &[&str]) -> Value {
        let en: Value = serde_json::from_str(NOTIFY_EN).unwrap();
        let mut strings = serde_json::Map::new();
        for (k, v) in en["strings"].as_object().unwrap() {
            if !skip.contains(&k.as_str()) {
                strings.insert(k.clone(), json!(made_up(word, v.as_str().unwrap())));
            }
        }
        let mut lists = serde_json::Map::new();
        for (k, v) in en["lists"].as_object().unwrap() {
            let items: Vec<String> = v.as_array().unwrap().iter().map(|i| made_up(word, i.as_str().unwrap())).collect();
            lists.insert(k.clone(), json!(items));
        }
        json!({"strings": strings, "lists": lists, "formats": en["formats"].clone()})
    }

    /// The test translations as JSON (edit before packing).
    pub(crate) fn value(version: u64) -> Value {
        let mut langs = serde_json::Map::new();
        for (code, names, call) in FIX {
            let mut n: Value = serde_json::from_str(names).unwrap();
            let n = n.as_object_mut().unwrap();
            n.remove("schema");
            let status = n.remove("status").unwrap();
            let call: Value = serde_json::from_str(call).unwrap();
            let mut speech = call["speech"].clone();
            speech["hello"] = call["hello"]["hello"].clone();
            speech["hello_again"] = call["hello"]["hello_again"].clone();
            let mut lang = json!({"names": n, "status": status, "speech": speech});
            match code {
                "it" => lang["notify"] = notify("Avviso", &IT_MISSING),
                "ar" => lang["notify"] = notify("إشعار", &[]),
                _ => {}
            }
            langs.insert(code.to_string(), lang);
        }
        // Names only: no call speech, so it goes nowhere.
        langs.insert(
            "ru".into(),
            json!({"names": {"code": "ru", "official": "ru-RU", "native": "Русский", "en": "Russian", "zh": "俄文"}, "status": "ai_draft"}),
        );
        json!({
            "format": 1, "version": version, "built_at": "2026-11-21T00:00:00Z", "source_commit": "test",
            "checker": "bageo-packs test", "english_source": "", "langs": langs,
        })
    }

    pub(crate) fn pack(v: &Value) -> Vec<u8> {
        let loc: Locales = serde_json::from_value(v.clone()).unwrap();
        bageo_packs::locales::pack_locales(&loc).unwrap()
    }

    /// The test translations, packed, at version (edit changes them first).
    pub(crate) fn gz(version: u64, edit: impl FnOnce(&mut Value)) -> Vec<u8> {
        let mut v = value(version);
        edit(&mut v);
        pack(&v)
    }

    /// Makes the test translations the table this test's calls read.
    pub(crate) fn use_table() -> &'static LocaleTable {
        use_gz(&gz(builtin().version + 1, |_| {}))
    }

    /// Makes a given packed file the table in use (no install).
    pub(crate) fn use_gz(gz: &[u8]) -> &'static LocaleTable {
        let t: &'static LocaleTable = Box::leak(Box::new(table(gz).unwrap()));
        set_current(t);
        t
    }

    /// Back to the built-in table (no file touched).
    pub(crate) fn use_builtin() {
        set_current(builtin());
    }

    /// A table without languages, at the built-in version: what tests read
    /// unless they choose another (the core languages only, as before any
    /// translation).
    pub(crate) fn empty() -> &'static LocaleTable {
        static T: OnceLock<&'static LocaleTable> = OnceLock::new();
        T.get_or_init(|| Box::leak(Box::new(LocaleTable { version: builtin().version, ..LocaleTable::empty() })))
    }

    /// Makes the table without languages the one in use.
    pub(crate) fn use_empty() {
        set_current(empty());
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{gz, use_empty, use_gz, use_table, IT_MISSING};
    use super::*;

    fn ext(code: &str) -> Ext {
        Ext::from_code(code).unwrap()
    }

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("bageo-locales-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn install(count: &AtomicUsize, packs: &Path, gz: &[u8], in_use: u64) -> Result<&'static LocaleTable, Vec<Issue>> {
        let staged = incoming_path(packs);
        std::fs::write(&staged, gz).unwrap();
        install_file_counted(count, packs, &staged, in_use, None)
    }

    /// The ten languages of the first translations Bageo ships.
    const SHIPPED: [&str; 10] = ["ar", "hi", "id", "it", "nl", "pl", "ru", "th", "tr", "vi"];

    #[test]
    fn builtin_translations_read_with_their_languages_and_no_test_ones() {
        let t = builtin();
        assert!(t.version >= 2026112100, "{}", t.version);
        assert!(t.get(ext("fa")).is_none(), "test languages never ship");
        let en = &english().files[&Kind::Notify];
        assert!(en.text.keys().count() >= 140);
        assert_eq!(en.meta.len(), en.text.keys().count(), "notes read");
        assert!(builtin_in_use(), "tests read the shipped file unless they choose a table");
        use_empty();
        assert!(!builtin_in_use() && current().langs().count() == 0);
    }

    #[test]
    fn the_shipped_translations_bring_ten_languages_to_notifications() {
        let (loc, dropped) = parse_from(BUILTIN, Origin::Builtin, 0).unwrap_or_else(|i| panic!("{}", i[0]));
        assert_eq!(loc.version, 2026112102);
        assert!(dropped.is_empty(), "{:?}", dropped.iter().map(|i| i.to_string()).collect::<Vec<_>>());
        let t = builtin();
        let codes: Vec<&str> = t.langs().map(|(e, _)| e.code()).collect();
        assert_eq!(codes.iter().copied().collect::<std::collections::BTreeSet<_>>(), SHIPPED.into_iter().collect());
        for (e, l) in t.langs() {
            assert!(l.stage >= Stage::Notify, "{e:?}: {:?}", l.stage);
            assert_eq!(l.dropped, 0, "{e:?}");
            assert!(!l.reviewed(), "{e:?}: AI drafts until a native speaker checks them");
            assert_eq!(l.names.code, e.code());
            let p = t.phrases(e).unwrap();
            assert!(!p.hello.is_empty() && p.stt == e.official(), "{e:?}");
        }
        // Farewells are used for the AI's goodbye only when their examples pass.
        // Vietnamese's "Thôi chào em nhé." is no closing to Bageo's classifier,
        // so its farewells are left out (the core word lists still know them).
        let left_out: Vec<&str> = t.langs().filter(|(e, _)| t.phrases(*e).is_some_and(|p| p.farewells.is_empty())).map(|(e, _)| e.code()).collect();
        assert_eq!(left_out, ["vi"]);
        assert_eq!(t.ai_farewells().len(), 9);
        testing::use_builtin();
        let menu = menu();
        assert_eq!(menu.len(), 19);
        assert!(menu[9..].iter().all(|x| x["stage"] == "notify" && x["reviewed"] == false));
    }

    #[test]
    fn a_clock_that_is_behind_keeps_the_builtin_translations_and_refuses_later_downloads() {
        // Today is 1970-01-01: the built-in file is still read...
        assert!(parse_from(BUILTIN, Origin::Builtin, 0).is_ok());
        assert_eq!(builtin().langs().count(), 10);
        // ...and the same bytes as a download are refused for their date.
        let e = parse_from(BUILTIN, Origin::Downloaded, 0).unwrap_err();
        assert!(e.iter().any(|i| i.detail.contains("later than tomorrow")), "{}", e[0]);
        // With the real clock a download dated in the future is refused too.
        let future = gz(2999010101, |_| {});
        let e = parse(&future).unwrap_err();
        assert!(e.iter().any(|i| i.detail.contains("later than tomorrow")), "{}", e[0]);
        assert!(parse_from(&future, Origin::Builtin, 0).is_ok());
        // An installed file is a download: one dated in the future is not used.
        let dir = tmp();
        std::fs::write(dir.join(FILE_NAME), &future).unwrap();
        assert!(load_installed(&dir, builtin().version).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stages_are_worked_out_from_what_passed() {
        let t = use_table();
        assert_eq!(t.stage(ext("it")), Stage::Notify);
        assert_eq!(t.stage(ext("ar")), Stage::Notify);
        assert_eq!(t.stage(ext("th")), Stage::Call);
        assert_eq!(t.stage(ext("fa")), Stage::Call);
        assert_eq!(t.stage(ext("ru")), Stage::None, "names only");
        assert_eq!(t.stage(ext("tr")), Stage::None, "not in the file");
        assert_eq!(stage(Lang::Ja), Stage::Notify, "core languages go all the way");
        // A language without a hello does not reach Call; one under the notify
        // share stays at Call.
        let t = use_gz(&gz(builtin().version + 1, |v| {
            v["langs"]["th"]["speech"].as_object_mut().unwrap().remove("hello");
            let s = v["langs"]["it"]["notify"]["strings"].as_object_mut().unwrap();
            let keys: Vec<String> = s.keys().take(10).cloned().collect();
            for k in keys {
                s.remove(&k);
            }
        }));
        assert_eq!(t.stage(ext("th")), Stage::None);
        assert_eq!(t.stage(ext("it")), Stage::Call);
        assert_eq!(t.stage(ext("ar")), Stage::Notify);
        // The status the file carries says nothing about the stage.
        let t = use_gz(&gz(builtin().version + 1, |v| v["langs"]["ru"]["status"] = "local_reviewed".into()));
        assert_eq!(t.stage(ext("ru")), Stage::None);
        assert!(t.get(ext("ru")).unwrap().reviewed());
    }

    #[test]
    fn the_menu_lists_core_languages_and_ready_ones_only() {
        use_table();
        let m = menu();
        let codes: Vec<&str> = m.iter().map(|x| x["code"].as_str().unwrap()).collect();
        assert_eq!(&codes[..9], ["en", "zh-TW", "zh-CN", "ja", "ko", "es", "fr", "de", "pt"]);
        for c in ["it", "th", "ar", "fa"] {
            assert!(codes.contains(&c), "{c}");
        }
        assert!(!codes.contains(&"ru"), "a language at no stage is not offered");
        let ar = m.iter().find(|x| x["code"] == "ar").unwrap();
        assert_eq!((ar["dir"].as_str(), ar["stage"].as_str(), ar["reviewed"].as_bool()), (Some("rtl"), Some("notify"), Some(false)));
        let th = m.iter().find(|x| x["code"] == "th").unwrap();
        assert_eq!(th["stage"].as_str(), Some("call"));
        assert!(m[0]["reviewed"].as_bool().unwrap() && m[0]["stage"] == "console");
        // A table without languages: the core languages only.
        use_empty();
        assert_eq!(menu().len(), 9);
    }

    #[test]
    fn settings_codes_fall_back_to_english_below_their_stage() {
        use_table();
        assert_eq!(lang_at("it", Stage::Notify), Lang::Ext(ext("it")));
        assert_eq!(lang_at("th", Stage::Call), Lang::Ext(ext("th")));
        assert_eq!(lang_at("th", Stage::Notify), Lang::En);
        assert_eq!(lang_at("ru", Stage::Call), Lang::En);
        assert_eq!(lang_at("ja", Stage::Notify), Lang::Ja);
        assert_eq!(lang_at("zh_tw", Stage::Call), Lang::ZhTw);
        assert_eq!(lang_at("klingon", Stage::Call), Lang::En);
        use_empty();
        assert_eq!(lang_at("it", Stage::Call), Lang::En);
        assert_eq!(Lang::parse("it"), Lang::En, "parse never gives another language");
    }

    #[test]
    fn notifications_read_the_translation_and_english_where_a_key_is_missing() {
        use_table();
        let en = crate::locale::words(Lang::En);
        let it = crate::locale::words(Lang::Ext(ext("it")));
        assert_eq!(it.n_incoming, "Avviso");
        assert_eq!(it.n_repeat_body, "Avviso {number} {count}");
        let all_en = serde_json::to_value(en).unwrap();
        let all_it = serde_json::to_value(it).unwrap();
        for k in IT_MISSING {
            assert_eq!(all_it[k], all_en[k], "{k} reads English");
        }
        assert_eq!(it.quote, en.quote);
        let ar = crate::locale::words(Lang::Ext(ext("ar")));
        assert_eq!(ar.n_summary, "إشعار");
        assert_eq!(crate::locale::fill_in(Lang::Ext(ext("ar")), &ar.n_repeat_body, &[("number", "0912"), ("count", "3")]), "إشعار 0912 3");
        // A language at Call only reads English, as before.
        assert!(std::ptr::eq(crate::locale::words(Lang::Ext(ext("th"))), en));
        assert!(std::ptr::eq(crate::locale::words(Lang::Ext(ext("fa"))), en));
    }

    #[test]
    fn a_broken_key_is_dropped_alone() {
        let t = use_gz(&gz(builtin().version + 1, |v| {
            v["langs"]["it"]["notify"]["strings"]["n_incoming"] = "Chiama <b>subito</b> https://x.example".into();
            v["langs"]["it"]["notify"]["strings"]["no_such_key"] = "Sconosciuto".into();
        }));
        let it = t.get(ext("it")).unwrap();
        assert_eq!(it.stage, Stage::Notify);
        assert_eq!(it.dropped, 1, "the broken key; an unknown key is ignored quietly");
        assert_eq!(crate::locale::words(Lang::Ext(ext("it"))).n_incoming, crate::locale::words(Lang::En).n_incoming);
        assert_eq!(crate::locale::words(Lang::Ext(ext("it"))).n_outgoing, "Avviso");
        // A broken speech line costs that language its call stage only.
        let t = use_gz(&gz(builtin().version + 1, |v| v["langs"]["th"]["speech"]["bye"] = "\u{202E}สวัสดี".into()));
        assert_eq!(t.stage(ext("th")), Stage::None);
        assert_eq!(t.stage(ext("it")), Stage::Notify);
        // A language not on the official list is left out alone.
        let t = use_gz(&gz(builtin().version + 1, |v| {
            let fa = v["langs"]["fa"].clone();
            v["langs"]["tlh"] = fa;
        }));
        assert_eq!(t.stage(ext("fa")), Stage::Call);
        assert_eq!(t.langs().count(), 5);
    }

    #[test]
    fn calls_get_the_language_name_hello_and_lines() {
        use_table();
        for (code, name, hello) in [("it", "Italian", "Pronto?"), ("th", "Thai", "ฮัลโหล"), ("ar", "Arabic", "ألو؟"), ("fa", "Persian", "الو؟")] {
            let l = Lang::Ext(ext(code));
            let p = crate::locale::phrases(l);
            assert_eq!((p.name_en, p.hello), (name, hello), "{code}");
            assert_eq!(p.stt, ext(code).official(), "{code}");
            let pack = crate::prompts::pack(l);
            assert!(pack.persona_default.contains(&format!("Speak {name}")), "{code}: {}", pack.persona_default);
            assert!(!pack.persona_default.contains("Speak English"), "{code}");
            assert!(pack.kick_open_persona.contains(hello), "{code}: {}", pack.kick_open_persona);
            assert!(pack.hello_loop_note.contains(p.im_here), "{code}: {}", pack.hello_loop_note);
            assert!(!pack.hello_loop_note.contains("I'm here, I can hear you"), "{code}");
            assert!(pack.legacy_persona_default.contains(p.no_wait), "{code}");
            assert!(!pack.legacy_persona_default.contains("台灣"), "{code}");
            assert!(std::ptr::eq(pack, crate::prompts::pack(l)), "built once per table");
        }
        // A language at no stage, or one not in the file: English, no panic.
        for code in ["ru", "tr", "sw"] {
            let l = Lang::Ext(ext(code));
            assert!(std::ptr::eq(crate::prompts::pack(l), crate::prompts::pack(Lang::En)), "{code}");
            assert_eq!(crate::locale::phrases(l).hello, "Hello?");
        }
    }

    #[test]
    fn an_update_is_used_from_the_next_call() {
        let v0 = builtin().version;
        use_table();
        let it = Lang::Ext(ext("it"));
        let before = crate::prompts::pack(it);
        assert!(before.kick_open_persona.contains("Pronto?"));
        let dir = tmp();
        let count = AtomicUsize::new(0);
        let newer = gz(v0 + 2, |v| v["langs"]["it"]["speech"]["hello"] = "Pronto, chi parla?".into());
        set_current(install(&count, &dir, &newer, current().version).unwrap());
        let after = crate::prompts::pack(it);
        assert!(after.kick_open_persona.contains("Pronto, chi parla?"), "{}", after.kick_open_persona);
        assert!(!before.kick_open_persona.contains("chi parla"), "a call going keeps its prompts");
        assert_eq!(crate::locale::phrases(it).hello, "Pronto, chi parla?");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_checks_counts_and_reads_back_at_start() {
        let v0 = builtin().version;
        let dir = tmp();
        let count = AtomicUsize::new(0);
        let t = install(&count, &dir, &gz(v0 + 1, |_| {}), v0).unwrap();
        assert_eq!(t.version, v0 + 1);
        assert!(dir.join(FILE_NAME).is_file());
        // Not newer: refused, nothing left behind.
        assert!(install(&count, &dir, &gz(v0 + 1, |_| {}), t.version).is_err());
        assert!(!has_incoming(&dir));
        // Read back at start-up; an unfinished download is cleared; the local
        // data pack's own downloads are left to it.
        let stray = dir.join(".incoming-locales-1-2.json.gz");
        let other = dir.join(".incoming-1-2.json");
        std::fs::write(&stray, b"x").unwrap();
        std::fs::write(&other, b"x").unwrap();
        assert_eq!(load_installed(&dir, v0).map(|t| t.version), Some(v0 + 1));
        assert!(!stray.exists() && other.exists());
        assert!(load_installed(&dir, v0 + 1).is_none(), "not newer than the built-in");
        // Broken on disk: not used.
        std::fs::write(dir.join(FILE_NAME), b"not gzip").unwrap();
        assert!(load_installed(&dir, v0).is_none());
        // The cap.
        let full = AtomicUsize::new(MAX_INSTALLS);
        let Err(e) = install(&full, &dir, &gz(v0 + 3, |_| {}), v0) else { panic!("installed past the cap") };
        assert!(e[0].detail.contains("restart Bageo"), "{}", e[0]);
        assert!(!has_incoming(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_goes_back_and_a_file_that_cannot_be_deleted_fails() {
        let v0 = builtin().version;
        let dir = tmp();
        let count = AtomicUsize::new(0);
        set_current(install(&count, &dir, &gz(v0 + 1, |_| {}), v0).unwrap());
        let file = dir.join(FILE_NAME);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(restore_in(&dir).is_err());
        assert_eq!(current().version, v0 + 1, "still the installed one");
        std::fs::remove_dir(&file).unwrap();
        restore_in(&dir).unwrap();
        assert!(builtin_in_use());
        restore_in(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_itself_is_checked_before_any_language() {
        let v0 = builtin().version;
        // Bytes after the gzip data.
        let mut tail = gz(v0 + 1, |_| {});
        tail.extend_from_slice(b"extra");
        assert!(parse(&tail).is_err());
        // A newer format.
        let e = parse(&gz(v0 + 1, |v| v["format"] = 2.into())).err().unwrap();
        assert!(e[0].detail.contains("update Bageo"), "{}", e[0]);
        // More than the limit once unpacked: stopped while reading.
        let mut big = b"{\"format\":1,\"langs\":{}".to_vec();
        big.resize(MAX_LOCALES_BYTES + 1024, b' ');
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut enc, &big).unwrap();
        let bomb = enc.finish().unwrap();
        assert!(bomb.len() < 1024 * 1024);
        let e = parse(&bomb).err().unwrap();
        assert!(e[0].detail.contains("stopped reading"), "{}", e[0]);
        // One language over 600 KB is left out alone.
        let loc = parse(&gz(v0 + 1, |v| v["langs"]["it"]["names"]["aliases"] = json!(vec!["x".repeat(700 * 1024)]))).unwrap().0;
        assert!(!loc.langs.contains_key("it") && loc.langs.contains_key("th"));
    }
}


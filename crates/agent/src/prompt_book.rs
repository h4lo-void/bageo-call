// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The prompt book: the words the models are told, kept as data instead of
//! in the code that uses them.
//!
//! - The words are in `data/prompts/<language>.json` (one file per language
//!   the words are written in), compiled into the program and read through
//!   [bageo_packs::prompts], which holds the file format and the rules.
//!   A text is a string, or a list of lines for a text of several
//!   paragraphs: one paragraph a line reads and diffs well.
//! - The code holds a [Text] for each one it uses (see [prompt_texts], once a
//!   module): a name, looked up the first time it is used and remembered as
//!   its place in the book. Every use after that reads the book in use and
//!   that place: two atomic loads, no lock, nothing allocated.
//! - Changing the words, or adding a language file, changes no code. A new
//!   text is one entry in the file and one line where its handle is declared.
//! - The checks below keep the two in step: every handle has its text, and
//!   no text is left in a file without a handle.
//! - "Check for updates" ([crate::pack_update]) may bring changed texts
//!   (bageo-prompts.json.gz). The book in use is then the built-in one with
//!   those laid over it, text by text: a key this Bageo does not have, a
//!   locked text (a rule of the product) or a text that fails
//!   [bageo_packs::check_text] keeps the built-in words. The next call reads
//!   the new book; "Restore" goes back to the built-in one. Each book stays
//!   in memory for the life of the program, so installs per run are capped
//!   ([MAX_INSTALLS]).
//!
//! How texts are put together into a prompt stays in the code that does it
//! ([crate::prompts] and the modules of each kind of call): the book holds
//! the words, not the rules. A sentence with values in it is a text with
//! `{name}` placeholders, filled in where it is used ([Text::fill]); which
//! sentence is used when, and what the values are, stays in the code.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};

use bageo_packs::prompts::{self as rules, PromptsPack};
use bageo_packs::{rule, Issue};
use tracing::{info, warn};

/// The format of the files this program reads.
pub const FORMAT: u64 = rules::BOOK_FORMAT;

/// The version of the built-in texts (YYYYMMDDNN). A download is used only
/// when it is newer. Raise it whenever a file in data/prompts changes (the
/// test `the_builtin_texts_match_their_version` says so).
pub const BUILTIN_VERSION: u64 = 2026112702;

/// The installed download, in the packs folder.
pub const FILE_NAME: &str = rules::PROMPTS_FILE;

/// Installs per run: every book stays in memory until Bageo restarts.
pub const MAX_INSTALLS: usize = 8;

const INCOMING_PREFIX: &str = ".incoming-prompts-";
const INCOMING_SUFFIX: &str = ".json.gz";

/// A handle not looked up yet.
const UNSEEN: usize = usize::MAX;
/// A handle whose name the book does not have.
const MISSING: usize = usize::MAX - 1;

/// One text of the book, by name (`<language>/<key>`). Reads as a `&str`
/// ([Text::get]) and writes itself in `format!`.
pub struct Text {
    name: &'static str,
    /// Its place in every book, once looked up.
    place: AtomicUsize,
}

impl Text {
    pub const fn new(name: &'static str) -> Self {
        Self { name, place: AtomicUsize::new(UNSEEN) }
    }

    /// The text in the book in use. A name the book does not have reads as
    /// nothing and is logged (the checks keep that from shipping).
    pub fn get(&self) -> &'static str {
        let mut at = self.place.load(Ordering::Relaxed);
        if at >= MISSING {
            at = self.look_up();
            if at == MISSING {
                return "";
            }
        }
        current().texts.get(at).copied().unwrap_or("")
    }

    #[cold]
    fn look_up(&self) -> usize {
        let at = self.place.load(Ordering::Relaxed);
        if at != UNSEEN {
            return at;
        }
        let at = match shelf().places.get(self.name) {
            Some(&at) => at,
            None => {
                tracing::error!("prompt book: no text named {}", self.name);
                MISSING
            }
        };
        self.place.store(at, Ordering::Relaxed);
        at
    }

    /// Its name in the book.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The text with each `{name}` replaced by its value ([fill_in]).
    pub fn fill(&self, values: &[Fill<'_>]) -> String {
        fill_in(self.get(), values)
    }
}

/// One value for [Text::fill]: the name between the braces, and what is
/// written in its place.
pub type Fill<'a> = (&'a str, &'a dyn fmt::Display);

/// `text` with each `{name}` replaced by the value of that name, in one
/// pass: what a value says is written as it is and never read for
/// placeholders again. A `{name}` with no value here, and any other brace,
/// stays as written. A name is letters, digits and `_`, the same as
/// [bageo_packs::prompts::placeholders] reads them.
pub fn fill_in(text: &str, values: &[Fill<'_>]) -> String {
    use fmt::Write as _;
    let mut out = String::with_capacity(text.len() + 32 * values.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let name_len: usize = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum();
        let value = if name_len > 0 && after[name_len..].starts_with('}') {
            values.iter().find(|(name, _)| *name == &after[..name_len])
        } else {
            None
        };
        match value {
            Some((_, value)) => {
                let _ = write!(out, "{value}");
                rest = &after[name_len + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.get())
    }
}

/// Declares the handles of a module's texts, and (for the checks) the list of
/// them all:
///
/// ```ignore
/// prompt_texts! {
///     /// What it is for.
///     ZH_KICK_COLD = "zh-TW/kick_cold";
/// }
/// ```
macro_rules! prompt_texts {
    ($( $(#[$m:meta])* $vis:vis $name:ident = $key:literal; )*) => {
        $( $(#[$m])* $vis static $name: $crate::prompt_book::Text = $crate::prompt_book::Text::new($key); )*
        /// Every handle declared above.
        #[cfg(test)]
        pub(crate) static ALL_TEXTS: &[&$crate::prompt_book::Text] = &[$(&$name),*];
    };
}
pub(crate) use prompt_texts;

/// Every text, by place: the built-in book or one with downloaded texts
/// laid over it. Kept for the life of the program once in use.
pub(crate) struct Book {
    version: u64,
    texts: Box<[&'static str]>,
    /// Texts that differ from the built-in ones.
    changed: usize,
}

/// One place of the book.
struct Entry {
    lang: &'static str,
    key: &'static str,
    locked: bool,
}

/// Where each text is, and the built-in book.
struct Shelf {
    places: HashMap<&'static str, usize>,
    entries: Vec<Entry>,
    builtin: &'static Book,
}

fn shelf() -> &'static Shelf {
    static SHELF: OnceLock<Shelf> = OnceLock::new();
    SHELF.get_or_init(|| {
        let files = rules::builtin_book();
        for e in &files.errors {
            tracing::error!("prompt book: {e}");
        }
        let mut places = HashMap::new();
        let mut entries = Vec::new();
        let mut texts = Vec::new();
        for (lang, file) in &files.langs {
            for (key, text) in &file.texts {
                // Kept for the life of the program: the files are part of it.
                let name: &'static str = Box::leak(format!("{lang}/{key}").into_boxed_str());
                places.insert(name, entries.len());
                entries.push(Entry { lang: &name[..lang.len()], key: &name[lang.len() + 1..], locked: file.locked.contains(key) });
                texts.push(text.as_str());
            }
        }
        let builtin: &'static Book = Box::leak(Box::new(Book { version: BUILTIN_VERSION, texts: texts.into_boxed_slice(), changed: 0 }));
        Shelf { places, entries, builtin }
    })
}

fn builtin() -> &'static Book {
    shelf().builtin
}

/// The book in use; null until the first text is read.
static CURRENT: AtomicPtr<Book> = AtomicPtr::new(std::ptr::null_mut());
/// Held to change the book in use or the installed file.
static LOCK: Mutex<()> = Mutex::new(());
static INSTALLS: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
thread_local! {
    /// Tests change the book of their own thread only.
    static TEST_CURRENT: std::cell::Cell<Option<&'static Book>> = const { std::cell::Cell::new(None) };
}

fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// The book calls read now.
fn current() -> &'static Book {
    #[cfg(test)]
    {
        if let Some(b) = TEST_CURRENT.with(|c| c.get()) {
            return b;
        }
    }
    let p = CURRENT.load(Ordering::Acquire);
    if p.is_null() {
        return first_book();
    }
    // SAFETY: CURRENT only ever holds books leaked for the life of the program.
    unsafe { &*p }
}

#[cold]
fn first_book() -> &'static Book {
    let held = lock();
    in_use(&held)
}

/// The book in use, with [LOCK] held: on the first read, an installed download
/// that is newer than the built-in texts and reads, else the built-in book.
/// Unit tests start from the built-in book.
fn in_use(_held: &MutexGuard<'static, ()>) -> &'static Book {
    #[cfg(test)]
    {
        if let Some(b) = TEST_CURRENT.with(|c| c.get()) {
            return b;
        }
    }
    let p = CURRENT.load(Ordering::Acquire);
    if !p.is_null() {
        // SAFETY: as in [current].
        return unsafe { &*p };
    }
    let b = if cfg!(test) { builtin() } else { load_installed(&crate::packs_store::installed_dir(), BUILTIN_VERSION).unwrap_or_else(builtin) };
    CURRENT.store(b as *const Book as *mut Book, Ordering::Release);
    b
}

fn set_current(b: &'static Book) {
    #[cfg(test)]
    {
        TEST_CURRENT.with(|c| c.set(Some(b)));
    }
    #[cfg(not(test))]
    {
        CURRENT.store(b as *const Book as *mut Book, Ordering::Release);
    }
}

/// The version of the texts in use.
pub fn current_version() -> u64 {
    current().version
}

/// Calls use the built-in texts.
pub(crate) fn builtin_in_use() -> bool {
    std::ptr::eq(current(), builtin())
}

/// Texts in use that differ from the built-in ones.
pub(crate) fn changed_texts() -> usize {
    current().changed
}

/// Downloads that can still be installed before Bageo has to be restarted.
pub(crate) fn installs_left() -> usize {
    MAX_INSTALLS.saturating_sub(INSTALLS.load(Ordering::SeqCst))
}

/// Loads an installed download now (start-up), so the first call has it.
pub(crate) fn load_at_start() {
    let v = current_version();
    if v != BUILTIN_VERSION {
        info!("prompt texts {v} in use");
    }
}

/// A new name for a download in the packs folder; start-up clears the ones
/// Bageo did not finish.
pub(crate) fn incoming_path(packs: &Path) -> PathBuf {
    packs.join(format!("{INCOMING_PREFIX}{}-{}{INCOMING_SUFFIX}", std::process::id(), uuid::Uuid::new_v4().simple()))
}

fn issue(r: &'static str, d: impl Into<String>) -> Vec<Issue> {
    vec![Issue::new(FILE_NAME, "", r, d)]
}

/// A downloaded file, refused before it is read when it is larger than a
/// download may be.
fn read_capped(path: &Path) -> Result<Vec<u8>, Vec<Issue>> {
    let len = std::fs::metadata(path).map_err(|e| issue(rule::FORMAT, format!("cannot read: {e}")))?.len();
    if len > rules::MAX_PROMPTS_PACKED as u64 {
        return Err(issue(rule::LENGTH, format!("{len} bytes packed, the limit is {}", rules::MAX_PROMPTS_PACKED)));
    }
    std::fs::read(path).map_err(|e| issue(rule::FORMAT, format!("cannot read: {e}")))
}

/// The built-in book with the texts of pack laid over it, text by text.
fn overlay(pack: &PromptsPack) -> Book {
    let sh = shelf();
    let base = builtin();
    let mut texts = base.texts.to_vec();
    let (mut changed, mut skipped, mut unknown) = (0, 0, 0);
    for (lang, keys) in &pack.langs {
        for (key, text) in keys {
            // A text this Bageo does not use: for a newer one.
            let Some(&at) = sh.places.get(format!("{lang}/{key}").as_str()) else {
                unknown += 1;
                continue;
            };
            let e = &sh.entries[at];
            if e.locked {
                warn!("prompt texts {}: {}/{} is locked; the built-in text stays", pack.version, e.lang, e.key);
                skipped += 1;
                continue;
            }
            let new = text.joined();
            if new == base.texts[at] {
                continue;
            }
            if let Err(why) = rules::check_text(base.texts[at], &new) {
                warn!("prompt texts {}: {}/{} not used ({why}); the built-in text stays", pack.version, e.lang, e.key);
                skipped += 1;
                continue;
            }
            texts[at] = Box::leak(new.into_boxed_str());
            changed += 1;
        }
    }
    info!("prompt texts {}: {changed} replaced, {skipped} skipped, {unknown} for a newer Bageo", pack.version);
    Book { version: pack.version, texts: texts.into_boxed_slice(), changed }
}

/// The download installed in packs when it reads and is newer than version;
/// unfinished downloads there are cleared.
fn load_installed(packs: &Path, version: u64) -> Option<&'static Book> {
    if let Ok(entries) = std::fs::read_dir(packs) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(INCOMING_PREFIX) && name.ends_with(INCOMING_SUFFIX) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let path = packs.join(FILE_NAME);
    if !path.is_file() {
        return None;
    }
    let pack = match read_capped(&path).and_then(|b| rules::parse_prompts(&b)) {
        Ok(p) => p,
        Err(issues) => {
            warn!("installed prompt texts rejected ({}); using the built-in ones", issues.first().map(|i| i.to_string()).unwrap_or_default());
            return None;
        }
    };
    if pack.version <= version {
        info!("installed prompt texts {} are not newer than the built-in {version}; using the built-in ones", pack.version);
        return None;
    }
    Some(Box::leak(Box::new(overlay(&pack))))
}

/// Installs a checked download (staged in the packs folder) of version want:
/// read in full again, newer than the texts in use, renamed into place in one
/// step; the next call reads it. The staged file never stays behind.
pub(crate) fn install_file(staged: &Path, want: u64) -> Result<u64, Vec<Issue>> {
    install_counted(&INSTALLS, &crate::packs_store::installed_dir(), staged, want)
}

pub(crate) fn install_counted(count: &AtomicUsize, packs: &Path, staged: &Path, want: u64) -> Result<u64, Vec<Issue>> {
    let held = lock();
    let res = (|| {
        if count.load(Ordering::SeqCst) >= MAX_INSTALLS {
            return Err(issue(rule::STATUS, format!("{MAX_INSTALLS} updates installed since Bageo started; restart Bageo first")));
        }
        let pack = rules::parse_prompts(&read_capped(staged)?)?;
        if pack.version != want {
            return Err(issue(rule::STATUS, format!("version {} is not {want}", pack.version)));
        }
        let in_use = in_use(&held).version;
        if pack.version <= in_use {
            return Err(issue(rule::STATUS, format!("version {} is not newer than {in_use}", pack.version)));
        }
        let book = overlay(&pack);
        std::fs::rename(staged, packs.join(FILE_NAME)).map_err(|e| issue(rule::FORMAT, format!("cannot install: {e}")))?;
        count.fetch_add(1, Ordering::SeqCst);
        let book: &'static Book = Box::leak(Box::new(book));
        set_current(book);
        Ok(book.version)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    res
}

/// Deletes the installed download; calls go back to the built-in texts. When
/// it cannot be deleted it stays in use.
pub fn restore_builtin() -> std::io::Result<()> {
    restore_at(&crate::packs_store::installed_dir())
}

fn restore_at(packs: &Path) -> std::io::Result<()> {
    let _held = lock();
    match std::fs::remove_file(packs.join(FILE_NAME)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    set_current(builtin());
    Ok(())
}

/// Pieces other modules' tests use.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use bageo_packs::prompts::{pack_prompts, PromptText, PROMPTS_FORMAT};

    /// The handle of a text, by name.
    pub(crate) fn handle(name: &str) -> &'static Text {
        tests::handles().into_iter().find(|t| t.name() == name).unwrap_or_else(|| panic!("no handle for {name}"))
    }

    /// A download of version carrying texts (language, key, text).
    pub(crate) fn pack(version: u64, texts: &[(&str, &str, &str)]) -> PromptsPack {
        let mut p = PromptsPack {
            format: PROMPTS_FORMAT,
            version,
            built_at: "2026-11-22T00:00:00Z".into(),
            source_commit: "0123456789abcdef0123456789abcdef01234567".into(),
            langs: Default::default(),
        };
        for (lang, key, text) in texts {
            p.langs.entry(lang.to_string()).or_default().insert(key.to_string(), PromptText::One(text.to_string()));
        }
        p
    }

    pub(crate) fn gz(version: u64, texts: &[(&str, &str, &str)]) -> Vec<u8> {
        pack_prompts(&pack(version, texts)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{gz, handle, pack};
    use super::*;

    /// The handles of every module that declares some ([prompt_texts]).
    fn modules() -> Vec<&'static [&'static Text]> {
        vec![
            crate::prompts::ALL_TEXTS,
            crate::gemini_live::ALL_TEXTS,
            crate::errands::ALL_TEXTS,
            crate::appointments::ALL_TEXTS,
            crate::contacts::ALL_TEXTS,
            crate::calendar::ALL_TEXTS,
            crate::scam_db::ALL_TEXTS,
            crate::scenario::ALL_TEXTS,
            crate::urgent_call::ALL_TEXTS,
            crate::summary::ALL_TEXTS,
            crate::stall_review::ALL_TEXTS,
            crate::stall_script::ALL_TEXTS,
            crate::preflight::ALL_TEXTS,
            crate::control::nlu::ALL_TEXTS,
            crate::extract::ALL_TEXTS,
            crate::mcp::ALL_TEXTS,
            crate::end_call::ALL_TEXTS,
            crate::place_habits::ALL_TEXTS,
            crate::voice_preview::ALL_TEXTS,
        ]
    }

    pub(super) fn handles() -> Vec<&'static Text> {
        modules().into_iter().flat_map(|m| m.iter().copied()).collect()
    }

    /// The built-in text of a name.
    fn lookup(name: &str) -> Option<&'static str> {
        shelf().places.get(name).map(|&at| builtin().texts[at])
    }

    fn parse_file(lang: &str, json: &str) -> Result<Vec<(String, String)>, String> {
        rules::parse_book_file(lang, json).map(|f| f.texts.into_iter().collect())
    }

    #[test]
    fn fill_in_writes_each_value_once_in_many_languages() {
        let n = 3;
        let cases: [(&str, &[Fill<'_>], &str); 9] = [
            ("{owner}現在不方便接電話，{n} 分鐘後回電。", &[("owner", &"王小明"), ("n", &n)], "王小明現在不方便接電話，3 分鐘後回電。"),
            ("{owner} can't take the call; back in {n} minutes.", &[("owner", &"Alex"), ("n", &n)], "Alex can't take the call; back in 3 minutes."),
            ("{owner}さんは今電話に出られません。{n}分後にかけ直します。", &[("owner", &"田中"), ("n", &n)], "田中さんは今電話に出られません。3分後にかけ直します。"),
            ("{owner} 님은 지금 전화를 받을 수 없습니다. {n}분 후에 다시 걸겠습니다.", &[("owner", &"김민수"), ("n", &n)], "김민수 님은 지금 전화를 받을 수 없습니다. 3분 후에 다시 걸겠습니다."),
            ("{owner} no puede atender; llama en {n} minutos.", &[("owner", &"María"), ("n", &n)], "María no puede atender; llama en 3 minutos."),
            ("{owner} ne peut pas répondre ; rappel dans {n} minutes.", &[("owner", &"Chloé"), ("n", &n)], "Chloé ne peut pas répondre ; rappel dans 3 minutes."),
            ("{owner} kann gerade nicht; Rückruf in {n} Minuten.", &[("owner", &"Jürgen"), ("n", &n)], "Jürgen kann gerade nicht; Rückruf in 3 Minuten."),
            ("{owner} não pode atender; retorna em {n} minutos.", &[("owner", &"João"), ("n", &n)], "João não pode atender; retorna em 3 minutos."),
            ("{owner} لا يستطيع الرد الآن؛ سيعاود الاتصال خلال {n} دقائق.", &[("owner", &"أحمد"), ("n", &n)], "أحمد لا يستطيع الرد الآن؛ سيعاود الاتصال خلال 3 دقائق."),
        ];
        for (text, values, want) in cases {
            assert_eq!(fill_in(text, values), want, "{text}");
        }
    }

    #[test]
    fn fill_in_leaves_everything_else_as_written() {
        // A value is never read for placeholders again.
        assert_eq!(fill_in("{a} {b}", &[("a", &"{b}"), ("b", &"{a}")]), "{b} {a}");
        // A name with no value, JSON braces, empty and broken braces stay.
        assert_eq!(fill_in("{x} and {y}", &[("x", &1)]), "1 and {y}");
        assert_eq!(fill_in(r#"answer {"date": "{date}"} only"#, &[("date", &"2026-01-02")]), r#"answer {"date": "2026-01-02"} only"#);
        assert_eq!(fill_in("{} {a b} {a", &[("a", &"no")]), "{} {a b} {a");
        assert_eq!(fill_in("{{a}}", &[("a", &"值")]), "{值}");
        assert_eq!(fill_in("{名前}さん、{a}", &[("名前", &"佐藤"), ("a", &"どうも")]), "佐藤さん、どうも");
        assert_eq!(fill_in("", &[("a", &"x")]), "");
        assert_eq!(fill_in("no braces", &[]), "no braces");
        // The same name twice, and the names the packs checker reads.
        let text = "{who}, {who}! {p_bye}";
        assert_eq!(fill_in(text, &[("who", &"Sam"), ("p_bye", &"bye")]), "Sam, Sam! bye");
        assert_eq!(rules::placeholders(text).into_iter().collect::<Vec<_>>(), ["p_bye", "who"]);
    }

    #[test]
    fn every_file_of_the_book_reads() {
        assert!(rules::builtin_book().errors.is_empty(), "{:?}", rules::builtin_book().errors);
        for (lang, json) in rules::BUILTIN_FILES {
            let list = parse_file(lang, json).unwrap_or_else(|e| panic!("{lang}.json: {e}"));
            assert!(!list.is_empty(), "{lang}");
            for (key, text) in &list {
                assert!(!text.is_empty(), "{lang}/{key} is empty");
                assert_eq!(lookup(&format!("{lang}/{key}")), Some(text.as_str()), "{lang}/{key}");
            }
        }
    }

    /// Every handle the code declares has its text, under one name each, and
    /// no text sits in a file without a handle: a slip in a key shows here,
    /// not on a call.
    #[test]
    fn handles_and_texts_match_one_to_one() {
        let handles = handles();
        let mut seen = std::collections::BTreeSet::new();
        for t in handles {
            assert!(lookup(t.name()).is_some(), "no text named {}", t.name());
            assert!(!t.get().is_empty(), "{}", t.name());
            assert!(seen.insert(t.name()), "two handles for {}", t.name());
        }
        for (lang, json) in rules::BUILTIN_FILES {
            for (key, _) in parse_file(lang, json).unwrap() {
                let name = format!("{lang}/{key}");
                assert!(seen.contains(name.as_str()), "{name} is in the book and no handle reads it");
            }
        }
    }

    #[test]
    fn a_text_is_looked_up_once_and_reads_the_same_ever_after() {
        let t = handles()[0];
        let first = t.get();
        assert!(std::ptr::eq(first, t.get()));
        assert_eq!(t.to_string(), first);
        assert_eq!(format!("{t}"), first);
    }

    #[test]
    fn a_text_of_several_lines_is_its_lines_joined() {
        let file = r#"{"format": 1, "lang": "xx", "texts": {"one": "a line", "many": ["", "[Head] first", "second", ""]}}"#;
        let list = parse_file("xx", file).unwrap();
        let get = |k: &str| list.iter().find(|(key, _)| key == k).map(|(_, t)| t.as_str());
        assert_eq!(get("one"), Some("a line"));
        assert_eq!(get("many"), Some("\n[Head] first\nsecond\n"));
    }

    #[test]
    fn a_file_that_is_not_the_book_is_refused() {
        for (file, why) in [
            (r#"{"format": 2, "lang": "xx", "texts": {"a": "b"}}"#, "format"),
            (r#"{"format": 1, "lang": "yy", "texts": {"a": "b"}}"#, "lang"),
            (r#"{"format": 1, "lang": "xx"}"#, "texts"),
            (r#"{"format": 1, "lang": "xx", "texts": {"Bad-Key": "b"}}"#, "key"),
            (r#"{"format": 1, "lang": "xx", "texts": {"a": 3}}"#, "neither"),
            (r#"{"format": 1, "lang": "xx", "texts": {"a": ["b", 3]}}"#, "line"),
            (r#"{"format": 1, "lang": "xx", "locked": ["c"], "texts": {"a": "b"}}"#, "locked"),
            ("not json", "JSON"),
        ] {
            let err = parse_file("xx", file).unwrap_err();
            assert!(err.contains(why), "{file}: {err}");
        }
    }

    /// The SHA-256 of the built-in texts when BUILTIN_VERSION was last raised.
    const BUILTIN_DIGEST: &str = "1e9dd997b1c2a18f9e2e45b6a201502d9f1154483d1e7abe3f9cb8803534d4b5";

    /// A download is used only when it is newer than the built-in texts, so a
    /// change to them must come with a new version.
    #[test]
    fn the_builtin_texts_match_their_version() {
        let digest = rules::builtin_book().digest();
        assert_eq!(
            digest, BUILTIN_DIGEST,
            "the built-in texts changed: set BUILTIN_VERSION to today (YYYYMMDD01) and BUILTIN_DIGEST to {digest}"
        );
    }

    /// Texts in eight languages, each a little different from the built-in one.
    const NEW: [(&str, &str, &str); 8] = [
        ("ja", "short_text_language", "言葉はふだんの話し言葉の日本語で。"),
        ("ko", "short_text_language", "말은 평소에 쓰는 구어체 한국어로 합니다."),
        ("de", "short_text_language", "Sprich ganz normales, gesprochenes Deutsch wie am Telefon."),
        ("es", "short_text_language", "Habla un español coloquial, como se habla por teléfono."),
        ("fr", "short_text_language", "Parle un français de tous les jours, celui du téléphone."),
        ("pt", "short_text_language", "Fale um português coloquial, como ao telefone."),
        ("en", "read_line", "Read this line exactly: \"{line}\""),
        ("zh-TW", "kick_open_abroad", "，這通電話是從國外打來的"),
    ];

    fn tmp() -> crate::pack_update::tests::Dir {
        crate::pack_update::tests::tmp()
    }

    fn put(dir: &Path, version: u64, texts: &[(&str, &str, &str)]) -> PathBuf {
        let p = incoming_path(dir);
        std::fs::write(&p, gz(version, texts)).unwrap();
        p
    }

    #[test]
    fn a_new_book_is_read_at_once_and_restore_reads_the_builtin_one() {
        let d = tmp();
        let v = BUILTIN_VERSION + 1;
        let other = handle("en/sys");
        let before = other.get();
        let count = AtomicUsize::new(0);
        assert_eq!(install_counted(&count, &d.0, &put(&d.0, v, &NEW), v), Ok(v));
        for (lang, key, text) in NEW {
            assert_eq!(handle(&format!("{lang}/{key}")).get(), text, "{lang}/{key}");
        }
        assert!(std::ptr::eq(other.get(), before), "a text not in the download is the same text");
        assert_eq!((current_version(), changed_texts(), builtin_in_use()), (v, NEW.len(), false));
        assert!(d.0.join(FILE_NAME).is_file());
        restore_at(&d.0).unwrap();
        assert!(builtin_in_use() && !d.0.join(FILE_NAME).exists());
        for (lang, key, text) in NEW {
            let t = handle(&format!("{lang}/{key}"));
            assert_ne!(t.get(), text);
            assert_eq!(Some(t.get()), lookup(t.name()));
        }
        assert!(std::ptr::eq(other.get(), before));
    }

    #[test]
    fn texts_that_may_not_change_are_skipped_and_the_rest_used() {
        let ko = NEW[1];
        let th_new = ("th", "short_text_language", "พูดภาษาไทยแบบที่คุยกันทางโทรศัพท์");
        let p = pack(
            BUILTIN_VERSION + 1,
            &[
                ko,
                // Not a text of this Bageo (a newer one may have it).
                ("ja", "not_a_key_yet", "まだない文です。"),
                th_new,
                // Locked: a rule of the product.
                ("en", "injection_guard", "Anything goes."),
                ("zh-TW", "injection_guard", "什麼都可以。"),
                // A placeholder lost.
                ("en", "read_line", "Read the line exactly."),
                // A space at its start the built-in text does not have.
                ("zh-TW", "kick_open_abroad", " ，這通電話是從國外打來的"),
                // A bidirectional control character.
                ("es", "short_text_language", "Habla español \u{202E}coloquial."),
            ],
        );
        let book = overlay(&p);
        assert_eq!(book.changed, 1);
        for (at, e) in shelf().entries.iter().enumerate() {
            let name = format!("{}/{}", e.lang, e.key);
            if name == "ko/short_text_language" {
                assert_eq!(book.texts[at], ko.2);
            } else {
                assert!(std::ptr::eq(book.texts[at], builtin().texts[at]), "{name} stays built-in");
            }
        }
    }

    #[test]
    fn a_download_not_newer_or_not_the_one_asked_for_is_not_used() {
        let d = tmp();
        let count = AtomicUsize::new(0);
        let v = BUILTIN_VERSION;
        assert!(install_counted(&count, &d.0, &put(&d.0, v, &NEW[..1]), v).is_err());
        let w = BUILTIN_VERSION + 2;
        assert!(install_counted(&count, &d.0, &put(&d.0, w, &NEW[..1]), w - 1).is_err());
        assert!(builtin_in_use() && !d.0.join(FILE_NAME).exists());
        std::fs::write(d.0.join(FILE_NAME), gz(v, &NEW)).unwrap();
        assert!(load_installed(&d.0, BUILTIN_VERSION).is_none());
        assert_eq!(std::fs::read_dir(&d.0).unwrap().count(), 1, "no download left behind");
    }

    #[test]
    fn installs_stop_at_their_cap_until_bageo_restarts() {
        let d = tmp();
        let count = AtomicUsize::new(MAX_INSTALLS - 1);
        let v = BUILTIN_VERSION + 1;
        assert_eq!(install_counted(&count, &d.0, &put(&d.0, v, &NEW[..2]), v), Ok(v));
        let staged = put(&d.0, v + 1, &NEW[..3]);
        let err = install_counted(&count, &d.0, &staged, v + 1).unwrap_err();
        assert!(err[0].detail.contains("restart Bageo"), "{err:?}");
        assert!(!staged.exists());
        assert_eq!(current_version(), v);
        restore_at(&d.0).unwrap();
    }

    #[test]
    fn a_broken_or_oversized_file_is_not_used_at_start() {
        let d = tmp();
        let v = BUILTIN_VERSION + 1;
        let left = incoming_path(&d.0);
        std::fs::write(&left, b"half").unwrap();
        std::fs::write(d.0.join(FILE_NAME), gz(v, &NEW)).unwrap();
        let book = load_installed(&d.0, BUILTIN_VERSION).expect("a newer download is used");
        assert_eq!((book.version, book.changed), (v, NEW.len()));
        assert!(!left.exists(), "an unfinished download is cleared");
        let mut cut = gz(v, &NEW);
        cut.truncate(cut.len() / 2);
        std::fs::write(d.0.join(FILE_NAME), cut).unwrap();
        assert!(load_installed(&d.0, BUILTIN_VERSION).is_none());
        std::fs::write(d.0.join(FILE_NAME), vec![0u8; rules::MAX_PROMPTS_PACKED + 1]).unwrap();
        assert!(load_installed(&d.0, BUILTIN_VERSION).is_none());
        std::fs::write(d.0.join(FILE_NAME), b"{\"format\": 1}").unwrap();
        assert!(load_installed(&d.0, BUILTIN_VERSION).is_none());
        assert!(builtin_in_use());
    }
}

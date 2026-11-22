// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The local data a call's prompt reads: one bundle (bageo-pack.json) built by
//! bageo-packs from the public region files. Bageo ships one
//! (data/packs/bageo-pack.json); a newer one installed later is kept in
//! {CALLS_DATA_DIR}/packs/ and used once it passes every check again.
//!
//! A table in use is never freed (Box::leak): a call holds &'static references
//! into it for its whole length, so an update never changes a call already
//! going. Only the built-in table and each installed update are kept, and
//! installs per run are capped ([MAX_INSTALLS]).
//!
//! A region whose language is not on this Bageo's list of Gemini Live
//! languages is left out of a table on its own; the rest of the bundle is used.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

use bageo_packs::data::is_official_lang;
use bageo_packs::{check_bundle_at, rule, Bundle, Issue, MultilangEntry, RegionData, FORMAT, MAX_BUNDLE_BYTES};
use flate2::read::GzDecoder;

use crate::prompts::LocalHello;

/// The bundle compiled into Bageo (`bageo-pack.json.gz`).
const BUILTIN_GZ: &[u8] = include_bytes!("../data/packs/bageo-pack.json.gz");

/// The installed bundle's file name, in {CALLS_DATA_DIR}/packs/.
pub const FILE_NAME: &str = "bageo-pack.json";

/// Most updates installed in one run; after that Bageo has to be restarted.
pub const MAX_INSTALLS: usize = 8;

const ISSUE_FILE: &str = "bageo-pack.json";

/// A bundle in use.
pub struct Table {
    /// YYYYMMDDNN; 0 for the empty table used when the built-in one is broken.
    pub version: u64,
    /// Country (ISO 3166 alpha-2, lowercase) to region code.
    pub countries: BTreeMap<String, String>,
    pub regions: BTreeMap<String, RegionData>,
    /// Regions left out because of their language.
    pub skipped: Vec<String>,
    /// Each region's way of answering the phone, where it has one.
    hellos: BTreeMap<String, LocalHello>,
    /// Secondary `(region, lang)` speech/style overrides.
    pub multilang: BTreeMap<(String, String), MultilangEntry>,
    /// Talk-style bubble sample lines per region code.
    pub bubbles: BTreeMap<String, serde_json::Value>,
    /// Business place habits for AI errands.
    pub place_habits: Vec<crate::place_habits::Region>,
    /// Raw JSON objects for `place_habits::tests`.
    pub place_habits_raw: Vec<serde_json::Value>,
}

impl Table {
    pub fn region(&self, code: &str) -> Option<&RegionData> {
        self.regions.get(code)
    }

    /// The region a country belongs to, by the bundle's index; None for a
    /// country the bundle does not list.
    pub fn region_of_country(&self, iso: &str) -> Option<&RegionData> {
        self.countries.get(iso).and_then(|code| self.regions.get(code))
    }

    /// A region's opening ([LocalHello]); None where the region has no hello.
    pub fn hello(&self, code: &str) -> Option<&LocalHello> {
        self.hellos.get(code)
    }

    fn new(bundle: Bundle, skipped: Vec<String>) -> Self {
        let mut hellos = BTreeMap::new();
        for (code, r) in &bundle.regions {
            let primary = r.lang.split('-').next().unwrap_or("");
            let Some(info) = bundle.languages.get(primary) else { continue };
            // A region in one of the nine settings languages without a hello of
            // its own (India, the Philippines: English) answers the way that
            // language does.
            let (hello, again) = match (&r.hello, settings_lang(primary)) {
                (Some(h), _) => (h.hello.clone(), h.hello_again.clone()),
                (None, Some(l)) => {
                    let ph = crate::locale::phrases(l);
                    (ph.hello.to_string(), ph.hello_again.to_string())
                }
                (None, None) => continue,
            };
            hellos.insert(
                code.clone(),
                LocalHello {
                    code: leak(primary.to_string()),
                    name_tw: leak(info.name.zh.clone()),
                    name_cn: leak(crate::zh_convert::to_cn(&info.name.zh)),
                    name_en: leak(info.name.en.clone()),
                    hello: leak(hello),
                    hello_again: leak(again),
                },
            );
        }
        let multilang = bundle
            .multilang
            .into_iter()
            .map(|e| ((e.region.clone(), e.lang.clone()), e))
            .collect();
        let place_habits_raw = bundle.place_habits;
        let place_habits = place_habits_raw
            .iter()
            .filter_map(|v| serde_json::from_value::<crate::place_habits::Region>(v.clone()).ok())
            .collect();
        Table {
            version: bundle.version,
            countries: bundle.countries,
            regions: bundle.regions,
            skipped,
            hellos,
            multilang,
            bubbles: bundle.bubbles,
            place_habits,
            place_habits_raw,
        }
    }

    fn empty() -> Self {
        Table {
            version: 0,
            countries: BTreeMap::new(),
            regions: BTreeMap::new(),
            skipped: Vec::new(),
            hellos: BTreeMap::new(),
            multilang: BTreeMap::new(),
            bubbles: BTreeMap::new(),
            place_habits: Vec::new(),
            place_habits_raw: Vec::new(),
        }
    }
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// The settings language of a language code's primary part; None for Chinese
/// (its regions answer in the setting's Chinese) and every other language.
fn settings_lang(primary: &str) -> Option<crate::locale::Lang> {
    use crate::locale::Lang;
    Some(match primary {
        "en" => Lang::En,
        "ja" => Lang::Ja,
        "ko" => Lang::Ko,
        "es" => Lang::Es,
        "fr" => Lang::Fr,
        "de" => Lang::De,
        "pt" => Lang::Pt,
        _ => return None,
    })
}

fn issue(rule: &'static str, detail: impl Into<String>) -> Vec<Issue> {
    vec![Issue::new(ISSUE_FILE, "", rule, detail)]
}

/// A region whose language, speech-to-text code or other languages are not on
/// this Bageo's list.
fn unsupported(r: &RegionData) -> bool {
    !is_official_lang(&r.lang)
        || r.stt.as_deref().is_some_and(|s| !is_official_lang(s))
        || r.other_langs.iter().any(|l| !is_official_lang(l))
}

/// Takes out the regions this Bageo cannot use, with their countries and
/// manifest entries; returns their codes.
fn prune(b: &mut Bundle) -> Vec<String> {
    let gone: Vec<String> = b.regions.values().filter(|r| unsupported(r)).map(|r| r.code.clone()).collect();
    for code in &gone {
        b.regions.remove(code);
        b.countries.retain(|_, c| c != code);
        b.manifest.retain(|e| !(e.kind == "region" && e.id == *code));
        b.multilang.retain(|m| m.region != *code);
    }
    gone
}

/// Where a bundle comes from. The built-in one ships with the program, so the
/// "not later than tomorrow" limit on dates does not apply to it: a computer
/// whose clock is behind still gets its regional data.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Builtin,
    Downloaded,
}

/// "Today" for a bundle with no upper limit on its dates.
const NO_DATE_LIMIT: i64 = i64::MAX / 2;

/// Reads a downloaded or installed bundle the way Bageo uses it: size, format
/// number and shape (fields this version does not know are ignored), the
/// regions it cannot use left out, then every check. Returns the bundle and
/// the codes left out.
pub fn parse(json: &str) -> Result<(Bundle, Vec<String>), Vec<Issue>> {
    parse_from(json, Origin::Downloaded, bageo_packs::util::now_days())
}

/// Same, with the bundle's origin and today (days since 1970-01-01).
fn parse_from(json: &str, origin: Origin, today: i64) -> Result<(Bundle, Vec<String>), Vec<Issue>> {
    let today = if origin == Origin::Builtin { NO_DATE_LIMIT } else { today };
    if json.len() > MAX_BUNDLE_BYTES {
        return Err(issue(rule::LENGTH, format!("{} bytes, the limit is {MAX_BUNDLE_BYTES}", json.len())));
    }
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| issue(rule::FORMAT, e.to_string()))?;
    match value.get("format").and_then(|f| f.as_u64()) {
        Some(n) if n > u64::from(FORMAT) => {
            return Err(issue(rule::FORMAT, format!("format {n} is newer than this Bageo reads ({FORMAT}); update Bageo first")))
        }
        Some(_) => {}
        None => return Err(issue(rule::FORMAT, "format is missing")),
    }
    let mut bundle: Bundle = serde_json::from_value(value).map_err(|e| issue(rule::FORMAT, e.to_string()))?;
    let skipped = prune(&mut bundle);
    let issues = check_bundle_at(&bundle, today);
    if issues.is_empty() {
        Ok((bundle, skipped))
    } else {
        Err(issues)
    }
}

/// Unpacks a gzipped bundle (`[0x1f, 0x8b]`) or decodes UTF-8 JSON, capped at `MAX_BUNDLE_BYTES`.
fn decode_bundle_bytes(bytes: &[u8]) -> Result<String, Vec<Issue>> {
    if bytes.len() > MAX_BUNDLE_BYTES {
        return Err(issue(rule::LENGTH, format!("{} bytes, the limit is {MAX_BUNDLE_BYTES}", bytes.len())));
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut dec = GzDecoder::new(bytes);
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = dec.read(&mut buf).map_err(|e| issue(rule::FORMAT, format!("cannot unpack: {e}")))?;
            if n == 0 {
                break;
            }
            if out.len() + n > MAX_BUNDLE_BYTES {
                return Err(issue(rule::LENGTH, format!("more than {MAX_BUNDLE_BYTES} bytes unpacked; stopped reading")));
            }
            out.extend_from_slice(&buf[..n]);
        }
        let rest = dec.into_inner();
        if !rest.is_empty() {
            return Err(issue(rule::FORMAT, format!("{} bytes after the packed data", rest.len())));
        }
        String::from_utf8(out).map_err(|_| issue(rule::FORMAT, "not UTF-8 text"))
    } else {
        std::str::from_utf8(bytes).map(str::to_string).map_err(|_| issue(rule::FORMAT, "not UTF-8 text"))
    }
}

fn builtin_str() -> &'static str {
    static S: OnceLock<String> = OnceLock::new();
    S.get_or_init(|| decode_bundle_bytes(BUILTIN_GZ).expect("built-in bageo-pack.json.gz unpacks"))
}

/// The built-in table. A built-in bundle that does not pass (a build mistake,
/// caught by the tests) leaves an empty table: calls get no regional part.
pub fn builtin() -> &'static Table {
    static T: OnceLock<&'static Table> = OnceLock::new();
    T.get_or_init(|| match parse_from(builtin_str(), Origin::Builtin, 0) {
        Ok((b, skipped)) => Box::leak(Box::new(Table::new(b, skipped))),
        Err(issues) => {
            tracing::error!("built-in local data pack rejected: {}", issues.first().map(|i| i.to_string()).unwrap_or_default());
            Box::leak(Box::new(Table::empty()))
        }
    })
}

static CURRENT: RwLock<Option<&'static Table>> = RwLock::new(None);
static INSTALLS: AtomicUsize = AtomicUsize::new(0);
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

// A test's own table in use: a test that installs or restores never changes
// what the other tests (running at the same time) read.
#[cfg(test)]
thread_local! {
    static TEST_CURRENT: std::cell::Cell<Option<&'static Table>> = const { std::cell::Cell::new(None) };
}

/// Where Bageo keeps its data (CALLS_DATA_DIR, as the rest of the agent).
#[cfg(not(test))]
pub(crate) fn data_dir() -> PathBuf {
    let default_dir = if Path::new("/data/calls").is_dir() { "/data/calls" } else { "data/calls" };
    PathBuf::from(crate::config::env_or("CALLS_DATA_DIR", default_dir))
}

/// Tests keep their files in a folder of their own, never in the working directory.
#[cfg(test)]
pub(crate) fn data_dir() -> PathBuf {
    thread_local! {
        static DIR: PathBuf = std::env::temp_dir().join(format!("bageo-agent-test-{}", uuid::Uuid::new_v4()));
    }
    DIR.with(|d| d.clone())
}

fn packs_dir(dir: &Path) -> PathBuf {
    dir.join("packs")
}

/// The folder an installed bundle and its download are kept in.
pub(crate) fn installed_dir() -> PathBuf {
    packs_dir(&data_dir())
}

/// A new name for a download in the packs folder (.incoming-<pid>-<random>.json;
/// start-up clears the ones Bageo did not finish).
pub(crate) fn incoming_path(packs: &Path) -> PathBuf {
    packs.join(format!("{INCOMING_PREFIX}{}-{}{INCOMING_SUFFIX}", std::process::id(), uuid::Uuid::new_v4().simple()))
}

const INCOMING_PREFIX: &str = ".incoming-";
const INCOMING_SUFFIX: &str = ".json";

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

/// Calls use the built-in bundle (nothing newer installed, or it was restored).
pub(crate) fn builtin_in_use() -> bool {
    std::ptr::eq(current(), builtin())
}

fn set_current(t: &'static Table) {
    #[cfg(test)]
    {
        TEST_CURRENT.with(|c| c.set(Some(t)));
    }
    #[cfg(not(test))]
    {
        *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(t);
    }
}

/// The table calls use now: an installed bundle that is newer than the built-in
/// one and passes every check, else the built-in one. Unit tests always get the
/// built-in one, unless the test itself installed or restored one.
pub fn current() -> &'static Table {
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
    let t = if cfg!(test) { builtin() } else { load_installed(&data_dir(), builtin().version).unwrap_or_else(builtin) };
    *slot = Some(t);
    t
}

/// The bundle installed in dir (CALLS_DATA_DIR) when it is newer than version
/// and passes every check. Unfinished downloads (.incoming-*.json) are cleared.
fn load_installed(dir: &Path, version: u64) -> Option<&'static Table> {
    let packs = packs_dir(dir);
    if let Ok(entries) = std::fs::read_dir(&packs) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if is_incoming(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let path = packs.join(FILE_NAME);
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > MAX_BUNDLE_BYTES as u64 {
        tracing::warn!("installed local data pack too large ({} bytes); using the built-in one", meta.len());
        return None;
    }
    let raw = std::fs::read(&path).ok()?;
    let json = decode_bundle_bytes(&raw).ok()?;
    match parse(&json) {
        Ok((b, skipped)) if b.version > version => {
            tracing::info!(
                "local data pack {} in use ({} regions, {} left out for their language)",
                b.version,
                b.regions.len(),
                skipped.len()
            );
            Some(Box::leak(Box::new(Table::new(b, skipped))))
        }
        Ok((b, _)) => {
            tracing::info!("installed local data pack {} is not newer than the built-in {version}; using the built-in one", b.version);
            None
        }
        Err(issues) => {
            tracing::warn!(
                "installed local data pack rejected ({} issue(s), first: {}); using the built-in one",
                issues.len(),
                issues.first().map(|i| i.to_string()).unwrap_or_default()
            );
            None
        }
    }
}

/// Checks and installs a downloaded bundle, already written to `staged` in the
/// packs folder ([incoming_path]): it must pass every check, be the version
/// the release index named (`want`) and be newer than the table in use. It is
/// then renamed over the installed one in one step; calls after this use it.
/// The staged file never stays behind. Returns the version. At most
/// [MAX_INSTALLS] per run.
pub(crate) fn install_file(staged: &Path, want: u64) -> Result<u64, Vec<Issue>> {
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = install_file_counted(&INSTALLS, &data_dir(), staged, current().version, Some(want))?;
    set_current(t);
    Ok(t.version)
}

pub(crate) fn install_file_counted(count: &AtomicUsize, dir: &Path, staged: &Path, in_use: u64, want: Option<u64>) -> Result<&'static Table, Vec<Issue>> {
    let result = check_staged(count, staged, in_use, want).and_then(|t| {
        std::fs::rename(staged, packs_dir(dir).join(FILE_NAME)).map_err(|e| issue(rule::FORMAT, format!("cannot install the pack: {e}")))?;
        count.fetch_add(1, Ordering::SeqCst);
        Ok(t)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(staged);
    }
    result
}

fn check_staged(count: &AtomicUsize, staged: &Path, in_use: u64, want: Option<u64>) -> Result<&'static Table, Vec<Issue>> {
    if count.load(Ordering::SeqCst) >= MAX_INSTALLS {
        return Err(issue(rule::STATUS, format!("{MAX_INSTALLS} updates installed since Bageo started; restart Bageo first")));
    }
    let io = |e: std::io::Error| issue(rule::FORMAT, format!("cannot read the pack: {e}"));
    let len = std::fs::metadata(staged).map_err(io)?.len();
    if len > MAX_BUNDLE_BYTES as u64 {
        return Err(issue(rule::LENGTH, format!("{len} bytes, the limit is {MAX_BUNDLE_BYTES}")));
    }
    let raw = std::fs::read(staged).map_err(io)?;
    let text = decode_bundle_bytes(&raw)?;
    let (bundle, skipped) = parse(&text)?;
    if want.is_some_and(|w| w != bundle.version) {
        return Err(issue(rule::STATUS, format!("version {} is not the one the index names", bundle.version)));
    }
    if bundle.version <= in_use {
        return Err(issue(rule::STATUS, format!("version {} is not newer than the one in use ({in_use})", bundle.version)));
    }
    Ok(Box::leak(Box::new(Table::new(bundle, skipped))))
}

/// Writes a bundle to the packs folder and installs it (tests).
#[cfg(test)]
fn install_counted(count: &AtomicUsize, dir: &Path, json: &[u8], in_use: u64) -> Result<&'static Table, Vec<Issue>> {
    let packs = packs_dir(dir);
    std::fs::create_dir_all(&packs).unwrap();
    let staged = incoming_path(&packs);
    std::fs::write(&staged, json).unwrap();
    install_file_counted(count, dir, &staged, in_use, None)
}

/// The built-in bundle's text, for tests that make variants of it.
#[cfg(test)]
pub(crate) fn builtin_json() -> &'static str {
    builtin_str()
}

/// Goes back to the built-in bundle: the installed one is deleted. When it
/// cannot be deleted nothing changes (it would be loaded again at the next
/// start), and the error is returned.
pub fn restore_builtin() -> std::io::Result<()> {
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    restore_in(&data_dir())
}

fn restore_in(dir: &Path) -> std::io::Result<()> {
    remove_installed(dir)?;
    set_current(builtin());
    Ok(())
}

/// Deletes the installed bundle; none there is fine.
fn remove_installed(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(packs_dir(dir).join(FILE_NAME)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!("bageo-packs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn builtin_with(edit: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut v: serde_json::Value = serde_json::from_str(builtin_json()).unwrap();
        edit(&mut v);
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn builtin_pack_passes_check() {
        let (b, skipped) = parse(builtin_json()).unwrap_or_else(|i| panic!("{}", i.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n")));
        assert!(skipped.is_empty(), "{skipped:?}");
        assert!(b.regions.len() >= 4 && b.version >= 2026112001, "{}", b.regions.len());
        assert_eq!(builtin().regions.len(), b.regions.len());
        assert!(builtin().region("tw").is_some() && builtin().region_of_country("us").is_some());
    }

    #[test]
    fn a_region_in_a_language_bageo_lacks_is_left_out_alone() {
        let json = builtin_with(|v| v["regions"]["jp"]["lang"] = "tlh".into());
        let (b, skipped) = parse(&json).unwrap();
        assert_eq!(skipped, vec!["jp".to_string()]);
        assert!(!b.regions.contains_key("jp") && !b.countries.contains_key("jp"));
        assert!(b.regions.contains_key("tw") && b.regions.contains_key("de"));
        // Unknown fields from a newer pack are ignored.
        let json = builtin_with(|v| v["regions"]["de"]["future_field"] = "x".into());
        assert!(parse(&json).is_ok());
    }

    #[test]
    fn a_bundle_that_fails_a_check_or_is_newer_in_format_is_refused() {
        let bad = builtin_with(|v| v["regions"]["us"]["habits"]["en"] = "Call me at 0912345678 or @someone".into());
        assert!(parse(&bad).is_err());
        let newer = builtin_with(|v| v["format"] = 99.into());
        let e = parse(&newer).unwrap_err();
        assert!(e[0].detail.contains("update Bageo"), "{}", e[0]);
        assert!(parse("{").is_err());
    }

    #[test]
    fn install_checks_writes_and_restores() {
        let dir = tmp();
        let count = AtomicUsize::new(0);
        let v0 = builtin().version;
        let newer = builtin_with(|v| v["version"] = (v0 + 1).into());
        let t = install_counted(&count, &dir, newer.as_bytes(), v0).unwrap();
        assert_eq!(t.version, v0 + 1);
        assert!(packs_dir(&dir).join(FILE_NAME).is_file());
        // Not newer than what is in use: refused.
        assert!(install_counted(&count, &dir, newer.as_bytes(), t.version).is_err());
        // Read back at start-up, and an unfinished download cleared.
        let stray = packs_dir(&dir).join(".incoming-1.json");
        std::fs::write(&stray, "{").unwrap();
        assert_eq!(load_installed(&dir, v0).map(|t| t.version), Some(v0 + 1));
        assert!(!stray.exists());
        assert!(load_installed(&dir, v0 + 1).is_none(), "not newer than the built-in");
        // A file broken on disk is not used.
        std::fs::write(packs_dir(&dir).join(FILE_NAME), builtin_with(|v| {
            v["version"] = (v0 + 1).into();
            v["regions"]["tw"]["fraud"]["zh"] = "沒有標記".into();
        }))
        .unwrap();
        assert!(load_installed(&dir, v0).is_none());
        remove_installed(&dir).unwrap();
        assert!(load_installed(&dir, v0).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installs_per_run_are_capped() {
        let dir = tmp();
        let count = AtomicUsize::new(MAX_INSTALLS);
        let v0 = builtin().version;
        let newer = builtin_with(|v| v["version"] = (v0 + 1).into());
        let Err(e) = install_counted(&count, &dir, newer.as_bytes(), v0) else { panic!("installed past the cap") };
        assert!(e[0].detail.contains("restart Bageo"), "{}", e[0]);
        assert!(!packs_dir(&dir).join(FILE_NAME).exists());
        assert!(!has_incoming(&packs_dir(&dir)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_restore_that_cannot_delete_the_file_fails_and_changes_nothing() {
        let dir = tmp();
        let count = AtomicUsize::new(0);
        let v0 = builtin().version;
        let newer = builtin_with(|v| v["version"] = (v0 + 1).into());
        set_current(install_counted(&count, &dir, newer.as_bytes(), v0).unwrap());
        // The installed file cannot be deleted (here: a folder is in its place).
        let file = packs_dir(&dir).join(FILE_NAME);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert!(restore_in(&dir).is_err());
        assert_eq!(current().version, v0 + 1, "still the installed one");
        assert!(!builtin_in_use());
        // Once it can be deleted the restore works; with no file it still does.
        std::fs::remove_dir(&file).unwrap();
        std::fs::write(&file, &newer).unwrap();
        restore_in(&dir).unwrap();
        assert!(builtin_in_use() && !file.exists());
        restore_in(&dir).unwrap();
        assert!(builtin_in_use());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_clock_that_is_behind_keeps_the_builtin_pack_and_refuses_later_downloads() {
        // Today is 1970-01-01: the built-in pack is still read...
        assert!(parse_from(builtin_json(), Origin::Builtin, 0).is_ok());
        assert!(builtin().region("tw").is_some());
        // ...and the same text as a download is refused for its date.
        let e = parse_from(builtin_json(), Origin::Downloaded, 0).unwrap_err();
        assert!(e.iter().any(|i| i.detail.contains("later than tomorrow")), "{}", e[0]);
        // With the real clock a download dated in the future is refused too.
        let future = builtin_with(|v| v["version"] = 2999010101u64.into());
        let e = parse(&future).unwrap_err();
        assert!(e.iter().any(|i| i.detail.contains("later than tomorrow")), "{}", e[0]);
        assert!(parse_from(&future, Origin::Builtin, 0).is_ok());
    }

    #[test]
    fn startup_clears_unfinished_downloads() {
        let dir = tmp();
        let packs = packs_dir(&dir);
        std::fs::create_dir_all(&packs).unwrap();
        let stray = [packs.join(".incoming-1.json"), packs.join(".incoming-1-2.json"), incoming_path(&packs)];
        for s in &stray {
            std::fs::write(s, "{\"format\": 1").unwrap();
        }
        let keep = packs.join("notes.txt");
        std::fs::write(&keep, "x").unwrap();
        assert!(load_installed(&dir, builtin().version).is_none(), "nothing installed");
        assert!(stray.iter().all(|s| !s.exists()), "unfinished downloads cleared");
        assert!(keep.exists(), "other files left alone");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

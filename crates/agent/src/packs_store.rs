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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

use bageo_packs::data::is_official_lang;
use bageo_packs::{check_bundle, rule, Bundle, Issue, RegionData, FORMAT, MAX_BUNDLE_BYTES};

use crate::prompts::LocalHello;

/// The bundle compiled into Bageo.
const BUILTIN: &str = include_str!("../data/packs/bageo-pack.json");

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
        Table { version: bundle.version, countries: bundle.countries, regions: bundle.regions, skipped, hellos }
    }

    fn empty() -> Self {
        Table { version: 0, countries: BTreeMap::new(), regions: BTreeMap::new(), skipped: Vec::new(), hellos: BTreeMap::new() }
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
    }
    gone
}

/// Reads a bundle the way Bageo uses it: size, format number and shape (fields
/// this version does not know are ignored), the regions it cannot use left
/// out, then every check. Returns the bundle and the codes left out.
pub fn parse(json: &str) -> Result<(Bundle, Vec<String>), Vec<Issue>> {
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
    let issues = check_bundle(&bundle);
    if issues.is_empty() {
        Ok((bundle, skipped))
    } else {
        Err(issues)
    }
}

/// The built-in table. A built-in bundle that does not pass (a build mistake,
/// caught by the tests) leaves an empty table: calls get no regional part.
pub fn builtin() -> &'static Table {
    static T: OnceLock<&'static Table> = OnceLock::new();
    T.get_or_init(|| match parse(BUILTIN) {
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

/// Where Bageo keeps its data (CALLS_DATA_DIR, as the rest of the agent).
fn data_dir() -> PathBuf {
    let default_dir = if Path::new("/data/calls").is_dir() { "/data/calls" } else { "data/calls" };
    PathBuf::from(crate::config::env_or("CALLS_DATA_DIR", default_dir))
}

fn packs_dir(dir: &Path) -> PathBuf {
    dir.join("packs")
}

/// The table calls use now: an installed bundle that is newer than the built-in
/// one and passes every check, else the built-in one. Unit tests always get the
/// built-in one.
pub fn current() -> &'static Table {
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
            if name.starts_with(".incoming-") && name.ends_with(".json") {
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
    let json = std::fs::read_to_string(&path).ok()?;
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

/// Checks and installs a downloaded bundle: it must pass every check and be
/// newer than the table in use. Written to a temporary file next to the
/// installed one, then renamed over it; calls after this use it. Returns its
/// version. At most [MAX_INSTALLS] per run.
pub fn install(json: &[u8]) -> Result<u64, Vec<Issue>> {
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let t = install_counted(&INSTALLS, &data_dir(), json, current().version)?;
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(t);
    Ok(t.version)
}

fn install_counted(count: &AtomicUsize, dir: &Path, json: &[u8], in_use: u64) -> Result<&'static Table, Vec<Issue>> {
    if count.load(Ordering::SeqCst) >= MAX_INSTALLS {
        return Err(issue(rule::STATUS, format!("{MAX_INSTALLS} updates installed since Bageo started; restart Bageo first")));
    }
    let text = std::str::from_utf8(json).map_err(|_| issue(rule::FORMAT, "not UTF-8 text"))?;
    let (bundle, skipped) = parse(text)?;
    if bundle.version <= in_use {
        return Err(issue(rule::STATUS, format!("version {} is not newer than the one in use ({in_use})", bundle.version)));
    }
    let packs = packs_dir(dir);
    let io = |e: std::io::Error| issue(rule::FORMAT, format!("cannot write the pack: {e}"));
    std::fs::create_dir_all(&packs).map_err(io)?;
    let incoming = packs.join(format!(".incoming-{}.json", std::process::id()));
    std::fs::write(&incoming, json).map_err(io)?;
    if let Err(e) = std::fs::rename(&incoming, packs.join(FILE_NAME)) {
        let _ = std::fs::remove_file(&incoming);
        return Err(io(e));
    }
    count.fetch_add(1, Ordering::SeqCst);
    Ok(Box::leak(Box::new(Table::new(bundle, skipped))))
}

/// Goes back to the built-in bundle: the installed one is deleted.
pub fn restore_builtin() {
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    remove_installed(&data_dir());
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = Some(builtin());
}

fn remove_installed(dir: &Path) {
    let _ = std::fs::remove_file(packs_dir(dir).join(FILE_NAME));
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
        let mut v: serde_json::Value = serde_json::from_str(BUILTIN).unwrap();
        edit(&mut v);
        serde_json::to_string(&v).unwrap()
    }

    #[test]
    fn builtin_pack_passes_check() {
        let (b, skipped) = parse(BUILTIN).unwrap_or_else(|i| panic!("{}", i.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("\n")));
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
        remove_installed(&dir);
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}

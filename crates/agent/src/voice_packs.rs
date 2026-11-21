// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Voice packs: the preview lines and the blocked-number announcement of a
//! language (later also of a region), downloaded only when someone wants them.
//! The installer keeps its own samples; other packs are zips in the public
//! bageo-packs releases on GitHub, listed in [INDEX_NAME] next to the data
//! pack. Without a pack nothing breaks: the sample is made by Gemini the first
//! time, as before ([crate::voice_preview]).
//!
//! A pack is `voices-<lang>[-<region>].zip` holding only
//! `<Voice>_<0|1|2|reject>.wav` for every voice its index entry lists. The index:
//! `{"format": 1, "version": N, "voices": [{"lang", "region"?, "file", "bytes", "sha256", "voices": [..]}]}`.
//! Bageo has the index it shipped with ([BUILTIN_INDEX]); a download keeps the
//! newer one it fetched next to the packs.
//!
//! The rules, each with its test below:
//! - Downloads go through [crate::pack_update]: the fixed source, every
//!   redirect checked, the size capped, SHA-256 counted as it comes in, a
//!   temporary file that any failure removes.
//! - The zip is read with [crate::backup::ZipIn] and its zip-bomb check:
//!   exactly the files the entry promises, by exact name, each at most
//!   [MAX_CLIP_BYTES] and a WAV Bageo plays.
//! - The files are unpacked into a temporary folder and renamed into
//!   `voices/<id>` in one step; a failure leaves nothing, start-up clears
//!   what a crash left ([clear_unfinished]).
//! - A zip brought in by hand must have the size and SHA-256 of a pack in the index.
//! - One download, import or removal at a time, in a task of its own; the
//!   call path only ever reads finished folders.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::calls_http::{api_error, token_ok_parts, unauthorized, CallsState, TokenQuery};
use crate::pack_update::{self, Fail, Source, Staged};

/// The list of voice packs in a bageo-packs release.
pub(crate) const INDEX_NAME: &str = "bageo-voices-index.json";
/// The list this Bageo shipped with.
const BUILTIN_INDEX: &str = include_str!("../data/voices/bageo-voices-index.json");
/// The index format this Bageo reads.
pub(crate) const FORMAT: u64 = 1;
const MAX_INDEX_BYTES: u64 = 256 * 1024;
/// A pack (about 3 MB today).
pub(crate) const MAX_PACK_BYTES: u64 = 16 << 20;
/// One recording in a pack (the longest shipped one is about 63 KB).
const MAX_CLIP_BYTES: u64 = 512 << 10;
const MAX_PACKS: usize = 1024;
/// The recordings of one voice: three preview lines and the announcement.
pub(crate) const PARTS: [&str; 4] = ["0", "1", "2", "reject"];
const TOTAL_TIMEOUT: Duration = Duration::from_secs(180);
const DISMISSED_MAX: usize = 64;
const INCOMING_PREFIX: &str = ".incoming-";
const UNPACK_PREFIX: &str = ".unpack-";
/// `.old-<random>-<id>`: a pack being replaced, until the new one is in place.
const OLD_PREFIX: &str = ".old-";
/// A deleted pack on its way out.
const GONE_PREFIX: &str = ".gone-";
/// The longest preview line and announcement a pack may hold: about twice the
/// longest ones that come with Bageo (6.3 s for a line, 7.8 s for an
/// announcement, measured on assets/voices in November 2026).
const MAX_LINE_MS: u64 = 13_000;
const MAX_REJECT_MS: u64 = 16_000;

/// One pack in the index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub lang: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub voices: Vec<String>,
}

impl Entry {
    /// `<lang>` or `<lang>-<region>`: the folder and the setting dismissed_pack_prompts use it.
    pub(crate) fn id(&self) -> String {
        folder_id(&self.lang, self.region.as_deref())
    }

    /// The files the zip holds, and nothing else.
    fn files(&self) -> HashSet<String> {
        self.voices.iter().flat_map(|v| PARTS.iter().map(move |p| format!("{v}_{p}.wav"))).collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIndex {
    #[allow(dead_code)]
    format: u64,
    version: u64,
    voices: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Index {
    pub version: u64,
    pub voices: Vec<Entry>,
}

impl Index {
    fn entry(&self, id: &str) -> Option<&Entry> {
        let id = id.to_ascii_lowercase();
        self.voices.iter().find(|e| e.id() == id)
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum IndexError {
    /// A format newer than this Bageo reads.
    TooNew(u64),
    Bad(String),
}

/// A language code: `en`, `ja`, `zh-TW`, `pt-BR`, `zh-Hant`.
fn valid_lang(s: &str) -> bool {
    let (base, rest) = s.split_once('-').map_or((s, None), |(a, b)| (a, Some(b)));
    let base_ok = (2..=3).contains(&base.len()) && base.bytes().all(|b| b.is_ascii_lowercase()) && !reserved(base);
    let rest_ok = match rest {
        None => true,
        Some(r) if r.len() == 2 => r.bytes().all(|b| b.is_ascii_uppercase()),
        Some(r) if r.len() == 4 => r.as_bytes()[0].is_ascii_uppercase() && r.bytes().skip(1).all(|b| b.is_ascii_lowercase()),
        Some(_) => false,
    };
    base_ok && rest_ok
}

/// A region code of the data pack: `tw`, `latam`, `hk-mo`.
fn valid_region(s: &str) -> bool {
    let word = |w: &str| (2..=8).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase()) && !reserved(w);
    match s.split_once('-') {
        None => word(s),
        Some((a, b)) => word(a) && word(b),
    }
}

/// A Windows device name (con, prn, aux, nul, com1–9, lpt1–9): never a folder name.
fn reserved(w: &str) -> bool {
    let w = w.to_ascii_lowercase();
    let b = w.as_bytes();
    matches!(w.as_str(), "con" | "prn" | "aux" | "nul") || (b.len() == 4 && (w.starts_with("com") || w.starts_with("lpt")) && (b'1'..=b'9').contains(&b[3]))
}

/// A pack id in any case: a language of 2–3 letters, then up to three parts of
/// 2–8 letters (script, region), none a Windows device name. Folders and the
/// setting keep it in lowercase ([Entry::id]).
pub(crate) fn valid_id(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let mut parts = id.split('-');
    let first = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    id.len() <= 40
        && (2..=3).contains(&first.len())
        && first.bytes().all(|b| b.is_ascii_lowercase())
        && !reserved(first)
        && rest.len() <= 3
        && rest.iter().all(|w| (2..=8).contains(&w.len()) && w.bytes().all(|b| b.is_ascii_lowercase()) && !reserved(w))
}

fn check_entry(e: &Entry) -> Result<(), String> {
    if !valid_lang(&e.lang) {
        return Err(format!("a pack for the language {:?}", e.lang));
    }
    if e.region.as_deref().is_some_and(|r| !valid_region(r)) {
        return Err(format!("a pack for the region {:?}", e.region));
    }
    let id = e.id();
    // Release file names keep the case they were published with.
    if !e.file.eq_ignore_ascii_case(&format!("voices-{id}.zip")) {
        return Err(format!("the pack {id} is in {:?}", e.file));
    }
    if e.bytes == 0 || e.bytes > MAX_PACK_BYTES {
        return Err(format!("the pack {id} is {} bytes", e.bytes));
    }
    if e.sha256.len() != 64 || !e.sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err(format!("the pack {id} has no SHA-256"));
    }
    if e.voices.is_empty() || e.voices.len() > crate::voice_preview::VOICES.len() {
        return Err(format!("the pack {id} lists {} voices", e.voices.len()));
    }
    let mut seen = HashSet::new();
    for v in &e.voices {
        if !crate::voice_preview::valid_voice(v) || !seen.insert(v.as_str()) {
            return Err(format!("the pack {id} lists the voice {v:?}"));
        }
    }
    Ok(())
}

/// The voice index, checked: every entry must be well formed.
pub(crate) fn parse_index(text: &str) -> Result<Index, IndexError> {
    let v: Value = serde_json::from_str(text).map_err(|e| IndexError::Bad(format!("the voice index: {e}")))?;
    match v.get("format").and_then(Value::as_u64) {
        Some(n) if n > FORMAT => return Err(IndexError::TooNew(n)),
        Some(FORMAT) => {}
        _ => return Err(IndexError::Bad(format!("the voice index is not format {FORMAT}"))),
    }
    let raw: RawIndex = serde_json::from_value(v).map_err(|e| IndexError::Bad(format!("the voice index: {e}")))?;
    if raw.voices.len() > MAX_PACKS {
        return Err(IndexError::Bad(format!("the voice index lists {} packs", raw.voices.len())));
    }
    let mut ids = HashSet::new();
    for e in &raw.voices {
        check_entry(e).map_err(IndexError::Bad)?;
        // Ids are lowercase: ("zh-TW", no region) and ("zh", "tw") are the same folder.
        if !ids.insert(e.id()) {
            return Err(IndexError::Bad(format!("the pack {} is listed twice", e.id())));
        }
    }
    Ok(Index { version: raw.version, voices: raw.voices })
}

fn builtin() -> &'static Index {
    static B: OnceLock<Index> = OnceLock::new();
    B.get_or_init(|| {
        parse_index(BUILTIN_INDEX).unwrap_or_else(|e| {
            tracing::warn!("the built-in voice index is unusable: {e:?}");
            Index { version: 0, voices: Vec::new() }
        })
    })
}

/// Where downloaded packs are kept: `<data>/voices/<id>/`.
pub(crate) fn voices_root() -> PathBuf {
    crate::packs_store::data_dir().join("voices")
}

/// The index in use: the one a download fetched when it is newer and still
/// passes the checks, else the built-in one.
pub(crate) fn known_index(root: &Path) -> Index {
    let saved = std::fs::read(root.join(INDEX_NAME))
        .ok()
        .filter(|b| b.len() as u64 <= MAX_INDEX_BYTES)
        .and_then(|b| String::from_utf8(b).ok())
        .and_then(|t| parse_index(&t).ok());
    match saved {
        Some(ix) if ix.version >= builtin().version => ix,
        _ => builtin().clone(),
    }
}

fn save_index(root: &Path, text: &str, ix: &Index) {
    if ix.version < known_index(root).version {
        return;
    }
    let tmp = root.join(format!("{INCOMING_PREFIX}{}-{}.json", std::process::id(), uuid::Uuid::new_v4().simple()));
    let done = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, root.join(INDEX_NAME)));
    if let Err(e) = done {
        let _ = std::fs::remove_file(&tmp);
        tracing::warn!("voice index not kept: {e}");
    }
}

/// A downloaded voice list is kept (and used when it is newer than the built-in one).
pub(crate) fn kept_index(root: &Path) -> bool {
    root.join(INDEX_NAME).is_file()
}

/// Goes back to the voice list Bageo came with (with "Restore the built-in
/// version" of the local data, [crate::pack_update::restore]): the downloaded
/// list is deleted; none there is fine.
pub(crate) fn restore_builtin_index(root: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(root.join(INDEX_NAME)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The region of the data pack for samples in lang: the home country's, else
/// the language's own country.
pub(crate) fn region_for(lang: crate::locale::Lang) -> Option<&'static str> {
    crate::country_profile::resolve(None, lang).map(|r| r.code())
}

/// The downloaded folders to look in for lang, best first: the region's pack,
/// then the language's.
pub(crate) fn pack_dirs(root: &Path, lang: &str, region: Option<&str>) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(2);
    if let Some(r) = region.filter(|r| valid_region(r)) {
        out.push(root.join(format!("{lang}-{r}").to_ascii_lowercase()));
    }
    out.push(root.join(lang.to_ascii_lowercase()));
    out
}

/// `<lang>` or `<lang>-<region>` in lowercase: a pack's id, its folder and its
/// file name (`voices-<id>.zip`). Lowercase: "zh-TW" and "zh" in region "tw" are
/// one folder where case does not count.
pub(crate) fn folder_id(lang: &str, region: Option<&str>) -> String {
    match region {
        Some(r) => format!("{lang}-{r}"),
        None => lang.to_string(),
    }
    .to_ascii_lowercase()
}

/// `agent pack-voices <samples dir> <out dir>`: the recordings of
/// `make-voice-samples` with VOICE_SAMPLE_REGION (comma list; default every
/// region of [crate::voice_lines]) as release files: `voices-<id>.zip` per
/// region and the index [INDEX_NAME] (an existing one in the out dir keeps its
/// other packs; its version goes up). The zips come out byte for byte the same
/// from the same recordings. Each pack is then installed into a temporary folder the
/// way a download is, so a pack Bageo would refuse is found here.
pub(crate) fn make_packs(samples: &Path, out: &Path) -> anyhow::Result<()> {
    let regions: Vec<String> = std::env::var("VOICE_SAMPLE_REGION")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    let regions = if regions.is_empty() { crate::voice_lines::codes().into_iter().map(str::to_string).collect() } else { regions };
    make_packs_for(samples, out, &regions)
}

fn make_packs_for(samples: &Path, out: &Path, regions: &[String]) -> anyhow::Result<()> {
    use anyhow::Context as _;
    std::fs::create_dir_all(out)?;
    let old = std::fs::read_to_string(out.join(INDEX_NAME)).ok().map(|t| parse_index(&t).map_err(|e| anyhow::anyhow!("{e:?}"))).transpose()?;
    let mut entries: Vec<Entry> = old.as_ref().map(|i| i.voices.clone()).unwrap_or_default();
    let at = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).and_then(|d| d.and_hms_opt(0, 0, 0)).context("a date")?;
    let mut made = Vec::new();
    for code in regions {
        let lines = crate::voice_lines::for_region(code).with_context(|| format!("no lines for the region {code}"))?;
        let lang = lines.lang().with_context(|| format!("{code}: the language {}", lines.lang))?;
        let id = folder_id(lang.code(), Some(code));
        let dir = samples.join(&id);
        let present = |v: &str| PARTS.iter().all(|p| dir.join(format!("{v}_{p}.wav")).is_file());
        let voices: Vec<String> = crate::voice_preview::VOICES.iter().map(|v| v.0).filter(|v| present(v)).map(str::to_string).collect();
        anyhow::ensure!(!voices.is_empty(), "{id}: no voice with all its recordings in {}", dir.display());
        let mut zip = crate::backup::ZipOut::new(std::io::Cursor::new(Vec::new()), at);
        for v in &voices {
            for p in PARTS {
                let name = format!("{v}_{p}.wav");
                let body = std::fs::read(dir.join(&name)).with_context(|| name.clone())?;
                zip.add(&name, &mut &body[..], body.len() as u64)?;
            }
        }
        let bytes = zip.finish()?.into_inner();
        let entry = Entry {
            lang: lang.code().to_string(),
            region: Some(code.clone()),
            file: format!("voices-{id}.zip"),
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            voices,
        };
        std::fs::write(out.join(&entry.file), &bytes)?;
        entries.retain(|e| e.id() != id);
        made.push(entry.clone());
        entries.push(entry);
    }
    entries.sort_by_key(Entry::id);
    let today: u64 = chrono::Local::now().format("%Y%m%d01").to_string().parse().unwrap_or(1);
    let version = old.map_or(today, |i| (i.version + 1).max(today));
    let text = serde_json::to_string_pretty(&json!({"format": FORMAT, "version": version, "voices": entries}))? + "\n";
    parse_index(&text).map_err(|e| anyhow::anyhow!("the index made here is refused: {e:?}"))?;
    std::fs::write(out.join(INDEX_NAME), &text)?;
    // Install each as a download does, into a folder of its own.
    let check = std::env::temp_dir().join(format!("bageo-pack-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&check);
    for e in &made {
        let n = install_zip(&check, &out.join(&e.file), e).map_err(|f| anyhow::anyhow!("{} is refused: {f:?}", e.file))?;
        tracing::info!("voice pack {}: {} voices, {} bytes of recordings, {} bytes zipped, sha256 {}", e.id(), e.voices.len(), n, e.bytes, e.sha256);
    }
    let _ = std::fs::remove_dir_all(&check);
    Ok(())
}

/// The setting dismissed_pack_prompts as stored: pack ids, each once.
pub(crate) fn clean_dismissed(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in list {
        let s = s.trim().to_ascii_lowercase();
        if valid_id(&s) && !out.contains(&s) {
            out.push(s);
        }
        if out.len() == DISMISSED_MAX {
            break;
        }
    }
    out
}

/// The packs on this computer and the bytes each takes.
pub(crate) fn installed(root: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let Ok(dir) = std::fs::read_dir(root) else { return out };
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !valid_id(&name) || name != name.to_ascii_lowercase() || !e.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let bytes = std::fs::read_dir(e.path()).map(|d| d.flatten().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
        out.push((name, bytes));
    }
    out.sort();
    out
}

/// Start-up: removes what an unfinished download, unpack or removal left, and
/// puts back a pack whose replacement never got in place (a stop between the
/// two renames of [Unpacked::put_in_place]).
pub(crate) fn clear_unfinished(root: &Path) {
    let Ok(dir) = std::fs::read_dir(root) else { return };
    let mut names: Vec<String> = dir.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    for name in names {
        let path = root.join(&name);
        if let Some(rest) = name.strip_prefix(OLD_PREFIX) {
            let id = rest.split_once('-').map(|(_, id)| id).unwrap_or("");
            let target = root.join(id);
            if valid_id(id) && id == id.to_ascii_lowercase() && std::fs::symlink_metadata(&target).is_err() {
                match std::fs::rename(&path, &target) {
                    Ok(()) => tracing::info!("voice pack {id}: put back after an unfinished replacement"),
                    Err(e) => tracing::warn!("voice pack {id}: cannot put it back yet ({e}); tried again at the next start"),
                }
                continue;
            }
        } else if ![INCOMING_PREFIX, UNPACK_PREFIX, GONE_PREFIX].iter().any(|p| name.starts_with(p)) {
            continue;
        }
        let is_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
        let _ = if is_dir { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
    }
}

fn temp_name(prefix: &str, ext: &str) -> String {
    format!("{prefix}{}-{}{ext}", std::process::id(), uuid::Uuid::new_v4().simple())
}

fn io_fail(what: &str, e: std::io::Error) -> Fail {
    Fail::Io(format!("{what}: {e}"))
}

fn zip_fail(b: crate::backup::Bad) -> Fail {
    match b {
        crate::backup::Bad::Io(e) => io_fail("cannot read the pack", e),
        other => pack_update::bad(format!("not a voice pack ({other:?})")),
    }
}

/// A WAV Bageo plays (mono, IMA ADPCM or 16-bit PCM), not too short and at most max_ms long.
fn playable(b: &[u8], max_ms: u64) -> bool {
    crate::adpcm::decode_wav(b).is_ok_and(|(rate, pcm)| rate >= 8_000 && pcm.len() >= 3_200 && (pcm.len() as u64 / 2) * 1000 / u64::from(rate) <= max_ms)
}

/// The folder files are unpacked into; removed when dropped unless it was put in place.
struct Unpacked {
    path: PathBuf,
    done: bool,
}

impl Unpacked {
    fn create(root: &Path) -> Result<Self, Fail> {
        let path = root.join(temp_name(UNPACK_PREFIX, ""));
        std::fs::create_dir(&path).map_err(|e| io_fail("cannot unpack the voices", e))?;
        Ok(Unpacked { path, done: false })
    }

    /// Renamed to root/id in one step; a pack already there is moved aside
    /// first and put back when the new one cannot take its place.
    fn put_in_place(mut self, root: &Path, id: &str) -> Result<(), Fail> {
        let target = root.join(id);
        let old = if std::fs::symlink_metadata(&target).is_ok() {
            let old = root.join(format!("{OLD_PREFIX}{}-{id}", uuid::Uuid::new_v4().simple()));
            std::fs::rename(&target, &old).map_err(|e| io_fail("cannot replace the voices", e))?;
            Some(old)
        } else {
            None
        };
        if let Err(e) = std::fs::rename(&self.path, &target) {
            if let Some(old) = &old {
                if let Err(back) = std::fs::rename(old, &target) {
                    tracing::warn!("voice pack {id}: the new voices could not be put in place ({e}) nor the old ones back ({back}); start-up puts them back");
                    return Err(io_fail("cannot put the old voices back", back));
                }
            }
            return Err(io_fail("cannot put the voices in place", e));
        }
        self.done = true;
        if let Some(old) = old {
            if let Err(e) = std::fs::remove_dir_all(&old) {
                tracing::warn!("voice pack {id}: the replaced voices go at the next start ({e})");
            }
        }
        Ok(())
    }
}

impl Drop for Unpacked {
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Checks the zip at zip_path against entry and installs it as root/<id>:
/// the bytes of the recordings.
pub(crate) fn install_zip(root: &Path, zip_path: &Path, entry: &Entry) -> Result<u64, Fail> {
    let file = std::fs::File::open(zip_path).map_err(|e| io_fail("cannot read the pack", e))?;
    let mut zip = crate::backup::ZipIn::open(file).map_err(zip_fail)?;
    let want = entry.files();
    if zip.entries.len() != want.len() {
        return Err(pack_update::bad(format!("{} files, the index says {}", zip.entries.len(), want.len())));
    }
    let mut total = 0u64;
    for (name, e) in &zip.entries {
        if !want.contains(name) {
            return Err(pack_update::bad(format!("a file that does not belong: {name:?}")));
        }
        if e.size > MAX_CLIP_BYTES {
            return Err(pack_update::bad(format!("{name} is {} bytes", e.size)));
        }
        if e.too_tight() {
            return Err(pack_update::bad(format!("{name} is packed like a zip bomb")));
        }
        total += e.size;
    }
    std::fs::create_dir_all(root).map_err(|e| io_fail("cannot make the voices folder", e))?;
    let out = Unpacked::create(root)?;
    let mut names: Vec<&String> = want.iter().collect();
    names.sort();
    for name in names {
        let body = zip.read_entry(name, MAX_CLIP_BYTES).map_err(zip_fail)?.ok_or_else(|| pack_update::bad(format!("{name} is missing")))?;
        let max_ms = if name.ends_with("_reject.wav") { MAX_REJECT_MS } else { MAX_LINE_MS };
        if !playable(&body, max_ms) {
            return Err(pack_update::bad(format!("{name} is not a recording Bageo plays, or longer than {max_ms} ms")));
        }
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(out.path.join(name)).map_err(|e| io_fail("cannot unpack the voices", e))?;
        std::io::Write::write_all(&mut f, &body).and_then(|_| f.sync_all()).map_err(|e| io_fail("cannot unpack the voices", e))?;
    }
    out.put_in_place(root, &entry.id())?;
    Ok(total)
}

/// Forgets the previews Gemini made for the pack's language and voices (in
/// cache, `<model>/<lang>/<Voice>_<part>.pcm`), so the pack's recordings are
/// played from now on. Other languages and voices stay; so does everything
/// when a pack is deleted.
fn clear_made(cache: &Path, entry: &Entry) {
    let Ok(models) = std::fs::read_dir(cache) else { return };
    for m in models.flatten() {
        let dir = m.path().join(&entry.lang);
        for v in &entry.voices {
            for p in PARTS {
                let _ = std::fs::remove_file(dir.join(format!("{v}_{p}.pcm")));
            }
        }
    }
}

/// [install_zip], then [clear_made] for the previews kept in cache.
pub(crate) fn install_and_refresh(root: &Path, cache: &Path, zip_path: &Path, entry: &Entry) -> Result<u64, Fail> {
    let n = install_zip(root, zip_path, entry)?;
    clear_made(cache, entry);
    Ok(n)
}

/// Why a download, import or removal ended; [Outcome::code] is what the console shows (vp_r_<code>).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Installed,
    Removed,
    /// Not a pack in the index (a download), not one of its files (an import),
    /// or not on this computer (a removal).
    NotListed,
    Offline,
    BadPack,
    UpdateBageo,
    Io,
    Busy,
}

impl Outcome {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Outcome::Installed => "installed",
            Outcome::Removed => "removed",
            Outcome::NotListed => "not_listed",
            Outcome::Offline => "offline",
            Outcome::BadPack => "bad_pack",
            Outcome::UpdateBageo => "update_bageo",
            Outcome::Io => "io",
            Outcome::Busy => "busy",
        }
    }
}

enum Stop {
    Fail(Fail),
    NotListed,
}

impl From<Fail> for Stop {
    fn from(f: Fail) -> Self {
        Stop::Fail(f)
    }
}

fn outcome_of(r: &Result<u64, Stop>) -> Outcome {
    match r {
        Ok(_) => Outcome::Installed,
        Err(Stop::NotListed) => Outcome::NotListed,
        Err(Stop::Fail(Fail::Offline(_))) => Outcome::Offline,
        Err(Stop::Fail(Fail::UpdateBageo(_))) => Outcome::UpdateBageo,
        Err(Stop::Fail(Fail::Io(_))) => Outcome::Io,
        Err(Stop::Fail(Fail::Bad(_) | Fail::UpToDate)) => Outcome::BadPack,
    }
}

fn log_outcome(what: &str, id: &str, r: &Result<u64, Stop>) -> Outcome {
    let out = outcome_of(r);
    match r {
        Ok(n) => tracing::info!("voice pack {id} {what}: {n} bytes; previews use it from now on"),
        Err(Stop::Fail(f)) => tracing::warn!("voice pack {id} {what}: {} ({})", out.code(), f.detail()),
        Err(Stop::NotListed) => tracing::warn!("voice pack {id} {what}: not in the voice index"),
    }
    out
}

/// The network part of a download: the index, then the pack into a temporary
/// file next to the packs. Returns the file, its entry and the index.
async fn fetch_steps(client: &reqwest::Client, src: &Source, root: &Path, id: &str) -> Result<(Staged, Entry, String, Index), Stop> {
    let (text, hops) = pack_update::fetch_index(client, src, INDEX_NAME, MAX_INDEX_BYTES).await?;
    let ix = match parse_index(&text) {
        Ok(ix) => ix,
        Err(IndexError::TooNew(n)) => return Err(Fail::UpdateBageo(format!("voice index format {n}")).into()),
        Err(IndexError::Bad(s)) => return Err(pack_update::bad(s).into()),
    };
    let entry = ix.entry(id).cloned().ok_or(Stop::NotListed)?;
    std::fs::create_dir_all(root).map_err(|e| io_fail("cannot make the voices folder", e))?;
    let url = src.release_file_url(&hops, INDEX_NAME, &entry.file).ok_or_else(|| pack_update::bad("the index names a path, not a file"))?;
    let mut staged = Staged::create(root.join(temp_name(INCOMING_PREFIX, ".zip")))?;
    pack_update::download_checked(client, src, url, entry.bytes, &entry.sha256, &mut staged).await?;
    Ok((staged, entry, text, ix))
}

/// Downloads the pack id from src into root (the download within limit) and
/// installs it with install; the index is kept once a pack from it is
/// installed. hold (the lock of the caller) is let go only when the install
/// has ended, never while it still writes.
async fn download_with<H, F>(client: &reqwest::Client, src: &Source, root: &Path, id: &str, limit: Duration, hold: H, install: F) -> Outcome
where
    H: Send + 'static,
    F: FnOnce(&Path, &Path, &Entry) -> Result<u64, Fail> + Send + 'static,
{
    let id = id.to_ascii_lowercase();
    let r = match tokio::time::timeout(limit, fetch_steps(client, src, root, &id)).await {
        Err(_) => Err(Fail::Offline(format!("no answer within {} ms", limit.as_millis())).into()),
        Ok(Err(stop)) => Err(stop),
        Ok(Ok((staged, entry, text, ix))) => {
            // Outside the time limit: once the unpack starts it runs to its end,
            // and the lock goes with it.
            let dir = root.to_path_buf();
            let done = tokio::task::spawn_blocking(move || {
                let _hold = hold;
                let r = install(&dir, staged.path(), &entry);
                drop(staged);
                if r.is_ok() {
                    save_index(&dir, &text, &ix);
                }
                r
            })
            .await;
            match done {
                Ok(Ok(n)) => Ok(n),
                Ok(Err(f)) => Err(f.into()),
                Err(_) => Err(Fail::Io("the install stopped".into()).into()),
            }
        }
    };
    log_outcome("download", &id, &r)
}

fn sha256_of_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut n = 0u64;
    loop {
        let k = f.read(&mut buf)?;
        if k == 0 {
            break;
        }
        n += k as u64;
        h.update(&buf[..k]);
    }
    Ok((n, format!("{:x}", h.finalize())))
}

/// A zip brought in by hand (no connection): installed when its size and
/// SHA-256 are those of a pack in the index. Returns the pack's id.
fn import_steps(root: &Path, cache: &Path, zip_path: &Path) -> (String, Result<u64, Stop>) {
    let (n, sha) = match sha256_of_file(zip_path) {
        Ok(v) => v,
        Err(e) => return (String::new(), Err(io_fail("cannot read the file", e).into())),
    };
    let ix = known_index(root);
    let Some(entry) = ix.voices.iter().find(|e| e.bytes == n && e.sha256 == sha) else {
        return (String::new(), Err(Stop::NotListed));
    };
    (entry.id(), install_and_refresh(root, cache, zip_path, entry).map_err(Stop::Fail))
}

fn import_bytes(root: &Path, cache: &Path, body: &[u8]) -> Outcome {
    if body.len() as u64 > MAX_PACK_BYTES {
        return Outcome::BadPack;
    }
    if let Err(e) = std::fs::create_dir_all(root) {
        tracing::warn!("voice pack import: cannot make the voices folder ({e})");
        return Outcome::Io;
    }
    let path = root.join(temp_name(INCOMING_PREFIX, ".zip"));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).and_then(|mut f| std::io::Write::write_all(&mut f, body));
    let (id, r) = match written {
        Ok(()) => import_steps(root, cache, &path),
        Err(e) => (String::new(), Err(io_fail("cannot keep the file", e).into())),
    };
    let _ = std::fs::remove_file(&path);
    log_outcome("import", if id.is_empty() { "(file)" } else { &id }, &r)
}

/// Deletes the downloaded pack id: Removed, NotListed when there is none, Io
/// when it cannot be moved away (nothing changes then).
pub(crate) fn remove(root: &Path, id: &str) -> Outcome {
    let id = id.to_ascii_lowercase();
    if !valid_id(&id) {
        return Outcome::NotListed;
    }
    let dir = root.join(&id);
    if !std::fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
        return Outcome::NotListed;
    }
    let old = root.join(temp_name(GONE_PREFIX, ""));
    if let Err(e) = std::fs::rename(&dir, &old) {
        tracing::warn!("voice pack {id}: cannot delete it ({e}); still in use");
        return Outcome::Io;
    }
    if let Err(e) = std::fs::remove_dir_all(&old) {
        tracing::warn!("voice pack {id}: removed, its files go at the next start ({e})");
    }
    tracing::info!("voice pack {id} deleted");
    Outcome::Removed
}

static RUNNING: AtomicBool = AtomicBool::new(false);

/// Holds the one place for a download, import or removal.
struct Running;

impl Running {
    fn take() -> Option<Self> {
        (!RUNNING.swap(true, Ordering::SeqCst)).then_some(Running)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

/// What the console lists: the packs of the index (with their size and
/// whether they are here; `bundled` = a language-wide pack whose language
/// came with Bageo in `bundled`) and the ones on this computer.
pub(crate) fn list_json(root: &Path, bundled: &Path, dismissed: &[String]) -> Value {
    let ix = known_index(root);
    let have = installed(root);
    let packs: Vec<Value> = ix
        .voices
        .iter()
        .map(|e| {
            let id = e.id();
            let shipped = e.region.is_none() && bundled.join(&e.lang).is_dir();
            json!({"id": id, "lang": e.lang, "region": e.region, "bytes": e.bytes, "downloaded": have.iter().any(|(h, _)| *h == id), "bundled": shipped})
        })
        .collect();
    let downloaded: Vec<Value> = have
        .iter()
        .map(|(id, bytes)| {
            // The language and region as the list writes them; a pack no longer listed shows its id.
            let e = ix.voices.iter().find(|e| e.id() == *id);
            json!({"id": id, "lang": e.map_or(id.as_str(), |e| e.lang.as_str()), "region": e.and_then(|e| e.region.clone()), "bytes": bytes})
        })
        .collect();
    json!({
        "ok": true,
        "version": ix.version,
        "packs": packs,
        "downloaded": downloaded,
        "dismissed": dismissed,
        "running": RUNNING.load(Ordering::SeqCst),
    })
}

fn listing(state: &CallsState) -> Value {
    answer(&voices_root(), &crate::config::assets_dir().join("voices"), &state.settings_store.snapshot().dismissed_pack_prompts, None)
}

/// The list and, after a change, its result.
fn answer(root: &Path, bundled: &Path, dismissed: &[String], out: Option<Outcome>) -> Value {
    let mut body = list_json(root, bundled, dismissed);
    if let Some(out) = out {
        body["result"] = json!(out.code());
    }
    body
}

#[derive(Deserialize)]
struct PackRequest {
    id: String,
}

fn with_result(state: &CallsState, out: Outcome) -> Response {
    Json(answer(&voices_root(), &crate::config::assets_dir().join("voices"), &state.settings_store.snapshot().dismissed_pack_prompts, Some(out))).into_response()
}

/// GET /api/voices: packs that can be downloaded and the ones here.
async fn list(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    Json(listing(&state)).into_response()
}

/// POST /api/voices/download {"id"}: download and install one pack.
async fn download(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !pack_update::json_request(&headers) {
        return pack_update::json_required();
    }
    let Ok(req) = serde_json::from_slice::<PackRequest>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "expected {\"id\": \"<pack>\"}");
    };
    if !valid_id(&req.id) {
        return with_result(&state, Outcome::NotListed);
    }
    let Some(running) = Running::take() else { return with_result(&state, Outcome::Busy) };
    let cache = state.previews.cache_dir().to_path_buf();
    // A task of its own: it finishes even when the console stops waiting.
    let task = tokio::spawn(async move {
        let client = match pack_update::client_with(TOTAL_TIMEOUT) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("voice pack download: no HTTP client ({e})");
                return Outcome::Offline;
            }
        };
        download_with(&client, &Source::official(), &voices_root(), &req.id, TOTAL_TIMEOUT, running, move |root: &Path, zip: &Path, entry: &Entry| {
            install_and_refresh(root, &cache, zip, entry)
        })
        .await
    });
    let out = task.await.unwrap_or(Outcome::Offline);
    with_result(&state, out)
}

/// POST /api/voices/delete {"id"}: delete a downloaded pack.
async fn delete(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !pack_update::json_request(&headers) {
        return pack_update::json_required();
    }
    let Ok(req) = serde_json::from_slice::<PackRequest>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "expected {\"id\": \"<pack>\"}");
    };
    // The answer is made after the lock is let go, so it does not say "running".
    let out = remove_now(&voices_root(), &req.id);
    with_result(&state, out)
}

/// [remove] holding the one place for a change; Busy when it is taken.
fn remove_now(root: &Path, id: &str) -> Outcome {
    let Some(_running) = Running::take() else { return Outcome::Busy };
    remove(root, id)
}

/// The body of an import is the zip itself (a form from another page cannot send this type).
fn zip_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
        .is_some_and(|v| v == "application/zip" || v == "application/octet-stream")
}

/// POST /api/voices/import (the zip as the body): install a pack brought in by hand.
async fn import(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !zip_request(&headers) {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "Content-Type must be application/zip");
    }
    let Some(running) = Running::take() else { return with_result(&state, Outcome::Busy) };
    let cache = state.previews.cache_dir().to_path_buf();
    let out = tokio::task::spawn_blocking(move || {
        let _running = running;
        import_bytes(&voices_root(), &cache, &body)
    })
    .await
    .unwrap_or(Outcome::Io);
    with_result(&state, out)
}

pub fn router() -> Router<CallsState> {
    Router::new()
        .route("/api/voices", get(list))
        .route("/api/voices/download", post(download))
        .route("/api/voices/delete", post(delete))
        .route("/api/voices/import", post(import).layer(DefaultBodyLimit::max(MAX_PACK_BYTES as usize + 4096)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Lang;
    use crate::pack_update::tests::{tmp, Reply, Server};
    use crate::voice_preview::VoicePreviews;
    use std::sync::Arc;

    const LATEST: &str = "/h4lo-void/bageo-packs/releases/latest/download/bageo-voices-index.json";
    const TAGGED: &str = "/h4lo-void/bageo-packs/releases/download/v7/";
    const TWO: [&str; 2] = ["Kore", "Charon"];

    /// A short recording (a tone), as the packs hold them.
    fn wav() -> Vec<u8> {
        let tone: Vec<u8> = (0..8_000i32).flat_map(|i| (((i % 40) - 20) as i16 * 500).to_le_bytes()).collect();
        crate::adpcm::encode_wav(&tone, 16_000)
    }

    fn zip_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
        let mut z = crate::backup::ZipOut::new(std::io::Cursor::new(Vec::new()), chrono::Local::now().naive_local());
        for (name, body) in files {
            z.add(name, &mut &body[..], body.len() as u64).unwrap();
        }
        z.finish().unwrap().into_inner()
    }

    fn files_for(voices: &[&str]) -> Vec<(String, Vec<u8>)> {
        let w = wav();
        voices.iter().flat_map(|v| PARTS.iter().map(|p| (format!("{v}_{p}.wav"), w.clone())).collect::<Vec<_>>()).collect()
    }

    fn entry(lang: &str, region: Option<&str>, voices: &[&str], zip: &[u8]) -> Entry {
        let id = region.map_or(lang.to_string(), |r| format!("{lang}-{r}"));
        Entry {
            lang: lang.into(),
            region: region.map(str::to_string),
            file: format!("voices-{id}.zip"),
            bytes: zip.len() as u64,
            sha256: format!("{:x}", Sha256::digest(zip)),
            voices: voices.iter().map(|v| v.to_string()).collect(),
        }
    }

    fn index_text(entries: &[Entry]) -> String {
        serde_json::to_string(&json!({"format": FORMAT, "version": 2026112101u64, "voices": entries})).unwrap()
    }

    /// A zip whose files are deflated (the zips Bageo writes keep recordings as they are).
    fn deflated_zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write as _;
        let (mut out, mut central) = (Vec::new(), Vec::new());
        let le16 = |v: usize| (v as u16).to_le_bytes();
        for (name, body) in files {
            let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
            enc.write_all(body).unwrap();
            let packed = enc.finish().unwrap();
            let mut crc = flate2::Crc::new();
            crc.update(body);
            let offset = out.len() as u32;
            // version, flags (UTF-8), deflate, time and date, crc, sizes
            let mut common = Vec::new();
            common.extend_from_slice(&le16(0x0800));
            common.extend_from_slice(&le16(8));
            common.extend_from_slice(&[0u8; 4]);
            common.extend_from_slice(&crc.sum().to_le_bytes());
            common.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            common.extend_from_slice(&(body.len() as u32).to_le_bytes());
            common.extend_from_slice(&le16(name.len()));
            out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
            out.extend_from_slice(&le16(20));
            out.extend_from_slice(&common);
            out.extend_from_slice(&le16(0));
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&packed);
            central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            central.extend_from_slice(&le16(20));
            central.extend_from_slice(&le16(20));
            central.extend_from_slice(&common);
            central.extend_from_slice(&[0u8; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 4]);
        out.extend_from_slice(&le16(files.len()));
        out.extend_from_slice(&le16(files.len()));
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
        out.extend_from_slice(&le16(0));
        out
    }

    /// A release: latest/ leads to the tagged one, which holds the index and the zips.
    fn release(index: String, zips: Vec<(String, Vec<u8>)>) -> Server {
        Server::start(move |path| {
            if path == LATEST {
                Reply::Redirect(format!("{TAGGED}{INDEX_NAME}"))
            } else if path == format!("{TAGGED}{INDEX_NAME}") {
                Reply::Body(index.clone().into_bytes())
            } else if let Some((_, b)) = zips.iter().find(|(f, _)| path == format!("{TAGGED}{f}")) {
                Reply::Body(b.clone())
            } else {
                Reply::NotFound
            }
        })
    }

    async fn run(src: &Source, root: &Path, id: &str) -> Outcome {
        let cache = root.join("cache");
        download_with(&pack_update::client_with(TOTAL_TIMEOUT).unwrap(), src, root, id, TOTAL_TIMEOUT, (), move |r: &Path, z: &Path, e: &Entry| {
            install_and_refresh(r, &cache, z, e)
        })
        .await
    }

    async fn wait_for(f: impl Fn() -> bool) {
        for _ in 0..500 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("waited 5 s");
    }

    /// Nothing but finished packs and the kept index: no download or unpack left.
    fn leftovers(root: &Path) -> Vec<String> {
        std::fs::read_dir(root)
            .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| n.starts_with('.')).collect())
            .unwrap_or_default()
    }

    /// A pack served whose zip holds files; the entry promises voices.
    async fn serve_and_run(lang: &str, region: Option<&str>, voices: &[&str], files: Vec<(String, Vec<u8>)>) -> (crate::pack_update::tests::Dir, Outcome) {
        serve_zip(lang, region, voices, zip_of(&files)).await
    }

    /// A pack served as this zip; the entry promises voices.
    async fn serve_zip(lang: &str, region: Option<&str>, voices: &[&str], zip: Vec<u8>) -> (crate::pack_update::tests::Dir, Outcome) {
        let d = tmp();
        let e = entry(lang, region, voices, &zip);
        let s = release(index_text(std::slice::from_ref(&e)), vec![(e.file.clone(), zip)]);
        let out = run(&s.source(), &d.0, &e.id()).await;
        (d, out)
    }

    #[test]
    fn the_builtin_index_is_empty_and_valid() {
        let ix = parse_index(BUILTIN_INDEX).expect("built-in voice index");
        assert!(ix.voices.is_empty());
        let d = tmp();
        let v = list_json(&d.0, &d.0.join("bundled"), &[]);
        assert_eq!((v["packs"].as_array().map(Vec::len), v["downloaded"].as_array().map(Vec::len)), (Some(0), Some(0)));
    }

    #[test]
    fn index_entries_are_checked_in_many_languages() {
        let z = b"zip".to_vec();
        let good = [
            entry("zh-TW", Some("tw"), &TWO, &z),
            entry("en", Some("us"), &TWO, &z),
            entry("ja", None, &TWO, &z),
            entry("es", Some("latam"), &TWO, &z),
            entry("ar", Some("eg"), &TWO, &z),
            entry("zh-CN", Some("hk-mo"), &TWO, &z),
        ];
        let ix = parse_index(&index_text(&good)).unwrap();
        assert_eq!(ix.voices.iter().map(Entry::id).collect::<Vec<_>>(), ["zh-tw-tw", "en-us", "ja", "es-latam", "ar-eg", "zh-cn-hk-mo"]);
        let bad: Vec<Box<dyn Fn(&mut Entry)>> = vec![
            Box::new(|e: &mut Entry| e.file = "../voices-ja.zip".into()),
            Box::new(|e: &mut Entry| e.file = "voices-ko.zip".into()),
            Box::new(|e: &mut Entry| e.lang = "JA".into()),
            Box::new(|e: &mut Entry| e.lang = "../x".into()),
            Box::new(|e: &mut Entry| e.region = Some("Tw".into())),
            Box::new(|e: &mut Entry| e.sha256 = "A".repeat(64)),
            Box::new(|e: &mut Entry| e.bytes = MAX_PACK_BYTES + 1),
            Box::new(|e: &mut Entry| e.bytes = 0),
            Box::new(|e: &mut Entry| e.voices = vec!["Nobody".into()]),
            Box::new(|e: &mut Entry| e.voices = vec!["Kore".into(), "Kore".into()]),
            Box::new(|e: &mut Entry| e.voices.clear()),
        ];
        for (i, edit) in bad.iter().enumerate() {
            let mut e = entry("ja", None, &TWO, &z);
            edit(&mut e);
            e.file = if i < 2 { e.file.clone() } else { format!("voices-{}.zip", e.id()) };
            assert!(matches!(parse_index(&index_text(&[e])), Err(IndexError::Bad(_))), "case {i}");
        }
        assert!(matches!(parse_index(&index_text(&[good[2].clone(), good[2].clone()])), Err(IndexError::Bad(_))), "twice");
        assert_eq!(parse_index(r#"{"format": 2, "version": 1, "voices": []}"#), Err(IndexError::TooNew(2)));
        assert!(matches!(parse_index(r#"{"format": 1, "version": 1, "voices": [], "extra": 1}"#), Err(IndexError::Bad(_))));
    }

    #[test]
    fn pack_ids_and_dismissed_prompts_are_clean() {
        for ok in ["ja", "zh-TW", "zh-TW-tw", "es-latam", "ar-eg", "en-us", "zh-CN-hk-mo"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "../x", "ja/..", "ja-", "ja tw", "x.exe", "ja-TW-", ".incoming-1"] {
            assert!(!valid_id(bad), "{bad}");
        }
        let list: Vec<String> = [" ja ", "es-latam", "../x", "ja", "zh-TW-tw", "ar", "EN"].map(String::from).to_vec();
        assert_eq!(clean_dismissed(&list), ["ja", "es-latam", "zh-tw-tw", "ar", "en"]);
        let many: Vec<String> = (0..100u8).map(|i| format!("en-{}{}", (b'a' + i / 26) as char, (b'a' + i % 26) as char)).collect();
        assert_eq!(clean_dismissed(&many).len(), DISMISSED_MAX);
    }

    #[test]
    fn ids_ignore_case_and_never_name_a_windows_device() {
        let z = b"zip".to_vec();
        // Two names for one folder where case does not count (Windows, macOS).
        let same = [entry("zh-TW", None, &TWO, &z), entry("zh", Some("tw"), &TWO, &z)];
        assert_eq!((same[0].id(), same[1].id()), ("zh-tw".to_string(), "zh-tw".to_string()));
        assert!(matches!(parse_index(&index_text(&same)), Err(IndexError::Bad(_))));
        // A device name as the language or the region.
        for (lang, region) in [("con", None), ("nul", None), ("aux", None), ("prn", None), ("es", Some("con")), ("ja", Some("aux")), ("en", Some("com1")), ("ko", Some("lpt9"))] {
            let e = entry(lang, region, &TWO, &z);
            assert!(matches!(parse_index(&index_text(&[e])), Err(IndexError::Bad(_))), "{lang} {region:?}");
        }
        for bad in ["con", "CON", "Nul", "aux-tw", "en-nul", "en-com1", "ja-LPT9", "prn-latam", "es-latam-con"] {
            assert!(!valid_id(bad), "{bad}");
            assert!(clean_dismissed(&[bad.to_string()]).is_empty(), "{bad}");
        }
        assert!(reserved("COM9") && reserved("lpt1") && !reserved("com") && !reserved("com0") && !reserved("console"));
        // Any case names the same pack; folders and the setting keep it lowercase.
        assert!(valid_id("ZH-TW-TW"));
        assert_eq!(clean_dismissed(&["ZH-TW-tw".to_string(), "zh-tw-tw".to_string()]), ["zh-tw-tw"]);
        assert_eq!(pack_dirs(Path::new("/v"), "zh-TW", Some("tw")), [PathBuf::from("/v/zh-tw-tw"), PathBuf::from("/v/zh-tw")]);
        let d = tmp();
        let zip = zip_of(&files_for(&TWO));
        let zip_path = d.0.join("in.zip");
        std::fs::write(&zip_path, &zip).unwrap();
        install_zip(&d.0, &zip_path, &entry("zh-TW", Some("tw"), &TWO, &zip)).unwrap();
        assert_eq!(installed(&d.0).iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["zh-tw-tw"]);
        assert_eq!(remove(&d.0, "ZH-TW-TW"), Outcome::Removed);
    }

    #[test]
    fn dismissed_prompts_are_saved_and_read_back() {
        let dir = std::env::temp_dir().join(format!("bageo-voices-dismissed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut base = crate::config::Settings::from_env();
        base.calls_data_dir = dir.to_string_lossy().to_string();
        let store = crate::settings_store::SettingsStore::load(base.clone());
        assert!(store.snapshot().dismissed_pack_prompts.is_empty());
        let rt = store.apply_patch(&json!({"dismissed_pack_prompts": ["ja", "es-latam", "../x", "ja", "zh-TW-tw", "ar-eg"]})).unwrap();
        assert_eq!(rt.dismissed_pack_prompts, ["ja", "es-latam", "zh-tw-tw", "ar-eg"]);
        let again = crate::settings_store::SettingsStore::load(base);
        assert_eq!(again.snapshot().dismissed_pack_prompts, ["ja", "es-latam", "zh-tw-tw", "ar-eg"]);
        assert_eq!(list_json(&dir.join("voices"), &dir.join("bundled"), &again.snapshot().dismissed_pack_prompts)["dismissed"], json!(["ja", "es-latam", "zh-tw-tw", "ar-eg"]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_pack_is_downloaded_installed_and_the_preview_uses_it() {
        let d = tmp();
        let all: Vec<&str> = crate::voice_preview::VOICES.iter().map(|(v, _, _)| *v).collect();
        let files = files_for(&all);
        assert_eq!(files.len(), 120, "30 voices, 4 recordings each");
        let zip = zip_of(&files);
        let tw = entry("zh-TW", Some("tw"), &all, &zip);
        let ar_zip = zip_of(&files_for(&TWO));
        let ar = entry("ar", Some("eg"), &TWO, &ar_zip);
        let s = release(index_text(&[tw.clone(), ar.clone()]), vec![(tw.file.clone(), zip), (ar.file.clone(), ar_zip)]);
        assert_eq!(run(&s.source(), &d.0, "zh-TW-tw").await, Outcome::Installed);
        assert_eq!(s.asked(&format!("{TAGGED}{}", tw.file)), 1, "the zip comes from the index's release");
        assert_eq!(run(&s.source(), &d.0, "AR-EG").await, Outcome::Installed, "any case");
        assert_eq!(std::fs::read_dir(d.0.join("zh-tw-tw")).unwrap().count(), 120);
        assert!(leftovers(&d.0).is_empty());
        // The list shows both, with their size; the fetched index is kept.
        let v = list_json(&d.0, &d.0.join("bundled"), &[]);
        assert_eq!(v["packs"].as_array().unwrap().len(), 2);
        assert!(v["packs"].as_array().unwrap().iter().all(|p| p["downloaded"] == json!(true)));
        assert!(v["packs"].as_array().unwrap().iter().all(|p| p["bundled"] == json!(false)));
        assert_eq!(v["downloaded"][0]["id"], json!("ar-eg"));
        assert_eq!((v["downloaded"][1]["lang"].as_str(), v["downloaded"][1]["region"].as_str()), (Some("zh-TW"), Some("tw")));
        assert!(v["downloaded"][1]["bytes"].as_u64().unwrap() > 0);
        // Previews in that region now read the pack instead of the bundled samples.
        let p = crate::voice_preview::VoicePreviews::new(d.0.join("cache"), d.0.join("bundled")).with_packs(d.0.clone());
        assert!(p.clip_in(Lang::ZhTw, Some("tw"), "Sulafat", "1", 16_000).is_some());
        assert!(p.clip_in(Lang::ZhTw, Some("tw"), "Sulafat", "reject", 16_000).is_some());
        assert!(p.clip_in(Lang::Ja, Some("jp"), "Sulafat", "1", 16_000).is_none(), "no Japanese pack");
        // Same pack again: replaced in one step.
        assert_eq!(run(&s.source(), &d.0, "zh-TW-tw").await, Outcome::Installed);
        assert!(leftovers(&d.0).is_empty());
        // Not in the index.
        assert_eq!(run(&s.source(), &d.0, "ko").await, Outcome::NotListed);
    }

    #[tokio::test]
    async fn one_changed_byte_is_refused() {
        let d = tmp();
        let zip = zip_of(&files_for(&TWO));
        let e = entry("ja", None, &TWO, &zip);
        let mut changed = zip.clone();
        let mid = changed.len() / 2;
        changed[mid] ^= 0x01;
        let s = release(index_text(std::slice::from_ref(&e)), vec![(e.file.clone(), changed)]);
        assert_eq!(run(&s.source(), &d.0, "ja").await, Outcome::BadPack);
        assert!(!d.0.join("ja").exists());
        assert!(leftovers(&d.0).is_empty());
    }

    #[tokio::test]
    async fn an_extra_or_missing_file_is_refused() {
        let mut extra = files_for(&TWO);
        extra.push(("Kore_3.wav".into(), wav()));
        let mut missing = files_for(&TWO);
        missing.pop();
        for (what, files) in [("extra", extra), ("missing", missing)] {
            let (d, out) = serve_and_run("es", Some("latam"), &TWO, files).await;
            assert_eq!(out, Outcome::BadPack, "{what}");
            assert!(!d.0.join("es-latam").exists() && leftovers(&d.0).is_empty(), "{what}");
        }
    }

    #[tokio::test]
    async fn a_file_name_out_of_the_rule_is_refused() {
        for name in ["../Kore_0.wav", "Kore_0.exe", "KORE_0.wav", "kore_0.wav", "Kore_0.WAV", "Kore _0.wav", "sub/Kore_0.wav", "Kore_3.wav"] {
            let mut files = files_for(&TWO);
            files[0].0 = name.to_string();
            let (d, out) = serve_and_run("en", Some("us"), &TWO, files).await;
            assert_eq!(out, Outcome::BadPack, "{name}");
            assert!(!d.0.join("en-us").exists() && leftovers(&d.0).is_empty(), "{name}");
            assert!(!d.0.parent().unwrap().join("Kore_0.wav").exists());
        }
    }

    #[tokio::test]
    async fn a_file_too_large_a_zip_bomb_or_not_a_wav_is_refused() {
        // Too large, and not compressible (so it is not a bomb).
        let mut x = 7u32;
        let noise: Vec<u8> = (0..MAX_CLIP_BYTES as usize + 1024)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                (x >> 16) as u8
            })
            .collect();
        for (what, body) in [("large", noise), ("text", b"not a recording".repeat(300)), ("short", wav()[..60].to_vec())] {
            let mut files = files_for(&TWO);
            files[3].1 = body;
            let (d, out) = serve_and_run("ja", None, &TWO, files).await;
            assert_eq!(out, Outcome::BadPack, "{what}");
            assert!(!d.0.join("ja").exists() && leftovers(&d.0).is_empty(), "{what}");
        }
        // Not a zip at all.
        let d = tmp();
        let body = b"PK not really".to_vec();
        let e = entry("ja", None, &TWO, &body);
        let s = release(index_text(std::slice::from_ref(&e)), vec![(e.file.clone(), body)]);
        assert_eq!(run(&s.source(), &d.0, "ja").await, Outcome::BadPack);
        assert!(leftovers(&d.0).is_empty());
    }

    #[tokio::test]
    async fn a_zip_bomb_is_refused() {
        // The same pack deflated installs: the zip itself is fine.
        let (_d, out) = serve_zip("ja", None, &TWO, deflated_zip(&files_for(&TWO))).await;
        assert_eq!(out, Outcome::Installed);
        // Eight seconds of silence: a recording Bageo plays, packed over 200 to 1.
        let silence = crate::calls::wav_from_pcm(&vec![0u8; 256_000], 16_000, 1);
        assert!(playable(&silence, MAX_LINE_MS), "it passes every other check");
        let mut files = files_for(&TWO);
        files[1].1 = silence;
        let zip = deflated_zip(&files);
        assert!(zip.len() < 64 * 1024, "{} bytes", zip.len());
        let (d, out) = serve_zip("ja", None, &TWO, zip).await;
        assert_eq!(out, Outcome::BadPack);
        assert!(!d.0.join("ja").exists() && leftovers(&d.0).is_empty());
    }

    #[tokio::test]
    async fn a_recording_longer_than_its_limit_is_refused() {
        let long = |secs: i32| {
            let pcm: Vec<u8> = (0..16_000 * secs).flat_map(|i| (((i % 40) - 20) as i16 * 500).to_le_bytes()).collect();
            crate::adpcm::encode_wav(&pcm, 16_000)
        };
        // files_for: Kore_0, Kore_1, Kore_2, Kore_reject, then Charon's.
        for (lang, at, secs, ok) in [("de", 0usize, 14, false), ("fr", 5, 14, false), ("pt", 3, 17, false), ("ko", 3, 15, true), ("es", 1, 12, true)] {
            let mut files = files_for(&TWO);
            files[at].1 = long(secs);
            let (d, out) = serve_and_run(lang, None, &TWO, files).await;
            assert_eq!(out == Outcome::Installed, ok, "{lang} {} {secs} s: {out:?}", PARTS[at % 4]);
            assert_eq!(d.0.join(lang).is_dir(), ok);
            assert!(leftovers(&d.0).is_empty());
        }
    }

    #[tokio::test]
    async fn a_redirect_to_another_site_or_plain_http_is_refused() {
        let zip = zip_of(&files_for(&TWO));
        let e = entry("zh-TW", None, &TWO, &zip);
        let index = index_text(std::slice::from_ref(&e));
        for to in [
            "https://evil.example/voices-zh-TW.zip",
            "http://objects.githubusercontent.com/voices-zh-TW.zip",
            "https://raw.githubusercontent.com/h4lo-void/bageo-packs/main/voices-zh-TW.zip",
            "https://github.com/someone/else/releases/download/v7/voices-zh-TW.zip",
        ] {
            let d = tmp();
            let (ix, to2) = (index.clone(), to.to_string());
            let s = Server::start(move |path| if path == LATEST { Reply::Body(ix.clone().into_bytes()) } else { Reply::Redirect(to2.clone()) });
            assert_eq!(run(&s.source(), &d.0, "zh-TW").await, Outcome::BadPack, "{to}");
            assert!(!d.0.join("zh-tw").exists() && leftovers(&d.0).is_empty(), "{to}");
        }
        // The index itself.
        let d = tmp();
        let s = Server::start(|path| if path == LATEST { Reply::Redirect("https://evil.example/bageo-voices-index.json".into()) } else { Reply::NotFound });
        assert_eq!(run(&s.source(), &d.0, "zh-TW").await, Outcome::BadPack);
        assert_eq!(s.hits.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_download_cut_halfway_leaves_nothing() {
        let d = tmp();
        let zip = zip_of(&files_for(&TWO));
        let e = entry("es", None, &TWO, &zip);
        let index = index_text(std::slice::from_ref(&e));
        let (half, len) = (zip[..zip.len() / 2].to_vec(), zip.len());
        let s = Server::start(move |path| if path == LATEST { Reply::Body(index.clone().into_bytes()) } else { Reply::Cut(len, half.clone()) });
        let out = run(&s.source(), &d.0, "es").await;
        assert!(matches!(out, Outcome::Offline | Outcome::BadPack), "{out:?}");
        assert!(!d.0.join("es").exists() && leftovers(&d.0).is_empty());
    }

    #[tokio::test]
    async fn no_connection_is_a_retry_and_previews_go_on() {
        let d = tmp();
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(run(&Source::local(port), &d.0, "ja").await, Outcome::Offline);
        let s = Server::start(|_| Reply::NotFound);
        assert_eq!(run(&s.source(), &d.0, "ja").await, Outcome::Offline);
        assert!(leftovers(&d.0).is_empty());
        // The bundled samples are still read.
        let bundled = d.0.join("bundled");
        std::fs::create_dir_all(bundled.join("ja")).unwrap();
        std::fs::write(bundled.join("ja").join("Kore_0.wav"), wav()).unwrap();
        let p = crate::voice_preview::VoicePreviews::new(d.0.join("cache"), bundled).with_packs(d.0.clone());
        assert!(p.clip_in(Lang::Ja, Some("jp"), "Kore", "0", 16_000).is_some());
        // And the calls' regional data.
        assert!(crate::packs_store::current().region("tw").is_some());
    }

    #[test]
    fn a_pack_is_deleted_and_a_failed_delete_says_so() {
        let d = tmp();
        let zip_path = d.0.join("in.zip");
        let zip = zip_of(&files_for(&TWO));
        std::fs::write(&zip_path, &zip).unwrap();
        let root = d.0.join("voices");
        install_zip(&root, &zip_path, &entry("ja", None, &TWO, &zip)).unwrap();
        assert_eq!(installed(&root).len(), 1);
        assert_eq!(remove(&root, "ja"), Outcome::Removed);
        assert!(installed(&root).is_empty() && leftovers(&root).is_empty());
        assert_eq!(remove(&root, "ja"), Outcome::NotListed);
        assert_eq!(remove(&root, "../voices"), Outcome::NotListed);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            install_zip(&root, &zip_path, &entry("ja", None, &TWO, &zip)).unwrap();
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
            assert_eq!(remove(&root, "ja"), Outcome::Io);
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(installed(&root).len(), 1, "still there");
        }
    }

    #[test]
    fn region_packs_are_made_from_recordings_and_installed_like_a_download() {
        let d = tmp();
        let (samples, out) = (d.0.join("samples"), d.0.join("out"));
        let w = wav();
        for (id, voices) in [("zh-tw-tw", &TWO[..]), ("en-us", &TWO[..1])] {
            let dir = samples.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            for v in voices {
                for p in PARTS {
                    std::fs::write(dir.join(format!("{v}_{p}.wav")), &w).unwrap();
                }
            }
        }
        // A voice with only some of its recordings is left out.
        std::fs::write(samples.join("en-us").join("Puck_0.wav"), &w).unwrap();
        let regions = ["tw".to_string(), "us".to_string()];
        make_packs_for(&samples, &out, &regions).unwrap();
        let text = std::fs::read_to_string(out.join(INDEX_NAME)).unwrap();
        let ix = parse_index(&text).unwrap();
        assert_eq!(ix.voices.iter().map(Entry::id).collect::<Vec<_>>(), ["en-us", "zh-tw-tw"]);
        let (us, tw) = (&ix.voices[0], &ix.voices[1]);
        assert_eq!((us.lang.as_str(), us.region.as_deref(), us.file.as_str()), ("en", Some("us"), "voices-en-us.zip"));
        assert_eq!((tw.lang.as_str(), tw.region.as_deref(), tw.file.as_str()), ("zh-TW", Some("tw"), "voices-zh-tw-tw.zip"));
        assert_eq!(us.voices, ["Kore"]);
        let mut two = tw.voices.clone();
        two.sort();
        assert_eq!(two, ["Charon", "Kore"]);
        // The zip is what the index says, and Bageo installs it from there; the
        // preview finds it where the region's folder is looked for.
        let zip = std::fs::read(out.join(&tw.file)).unwrap();
        assert_eq!((zip.len() as u64, format!("{:x}", Sha256::digest(&zip))), (tw.bytes, tw.sha256.clone()));
        let root = d.0.join("voices");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(INDEX_NAME), &text).unwrap();
        assert_eq!(import_bytes(&root, &d.0.join("cache"), &zip), Outcome::Installed);
        let dirs = pack_dirs(&root, Lang::ZhTw.code(), Some("tw"));
        assert!(dirs[0].join("Kore_reject.wav").is_file(), "{dirs:?}");
        // The same recordings give the same file; the index goes up a version.
        make_packs_for(&samples, &out, &regions).unwrap();
        let again = parse_index(&std::fs::read_to_string(out.join(INDEX_NAME)).unwrap()).unwrap();
        assert_eq!(again.voices, ix.voices);
        assert!(again.version > ix.version);
        // A region without lines or without recordings is an error, not an empty pack.
        assert!(make_packs_for(&samples, &out, &["zz".to_string()]).is_err());
        assert!(make_packs_for(&d.0.join("none"), &out, &["tw".to_string()]).is_err());
    }

    #[test]
    fn an_import_must_match_the_index() {
        let d = tmp();
        let root = d.0.join("voices");
        let cache = d.0.join("cache");
        std::fs::create_dir_all(&root).unwrap();
        let zip = zip_of(&files_for(&TWO));
        let ko = entry("ko", None, &TWO, &zip);
        std::fs::write(root.join(INDEX_NAME), index_text(std::slice::from_ref(&ko))).unwrap();
        // Not listed: one byte changed.
        let mut changed = zip.clone();
        let mid = changed.len() / 2;
        changed[mid] ^= 0x01;
        assert_eq!(import_bytes(&root, &cache, &changed), Outcome::NotListed);
        assert!(installed(&root).is_empty() && leftovers(&root).is_empty());
        // The very file the index lists.
        assert_eq!(import_bytes(&root, &cache, &zip), Outcome::Installed);
        assert_eq!(installed(&root).iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["ko"]);
        assert!(leftovers(&root).is_empty());
        // Nothing listed at all (the built-in index): refused.
        let empty = d.0.join("empty");
        assert_eq!(import_bytes(&empty, &cache, &zip), Outcome::NotListed);
    }

    /// Set while the caller's lock is held.
    struct Held(Arc<AtomicBool>);

    impl Drop for Held {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn the_lock_is_kept_until_the_unpack_has_ended() {
        let zip = zip_of(&files_for(&TWO));
        let e = entry("ja", None, &TWO, &zip);
        let s = release(index_text(std::slice::from_ref(&e)), vec![(e.file.clone(), zip)]);
        // A time limit shorter than the unpack does not cut it off.
        let d = tmp();
        let slow = |r: &Path, z: &Path, e: &Entry| {
            std::thread::sleep(Duration::from_millis(600));
            install_zip(r, z, e)
        };
        let client = pack_update::client_with(TOTAL_TIMEOUT).unwrap();
        assert_eq!(download_with(&client, &s.source(), &d.0, "ja", Duration::from_millis(400), (), slow).await, Outcome::Installed);
        // The caller stops waiting (a time-out, the console going away) while the
        // unpack runs: it goes on, and the lock is let go only when it has ended.
        let d = tmp();
        let (held, started) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)));
        let (root, port, hold, st) = (d.0.clone(), s.port, Held(held.clone()), started.clone());
        let task = tokio::spawn(async move {
            let client = pack_update::client_with(TOTAL_TIMEOUT).unwrap();
            download_with(&client, &Source::local(port), &root, "ja", TOTAL_TIMEOUT, hold, move |r: &Path, z: &Path, e: &Entry| {
                st.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(400));
                install_zip(r, z, e)
            })
            .await
        });
        wait_for(|| started.load(Ordering::SeqCst)).await;
        task.abort();
        let _ = task.await;
        assert!(held.load(Ordering::SeqCst), "held while the unpack runs");
        assert!(!d.0.join("ja").exists());
        wait_for(|| !held.load(Ordering::SeqCst)).await;
        assert!(d.0.join("ja").is_dir() && leftovers(&d.0).is_empty());
        assert!(kept_index(&d.0), "kept before the lock goes");
    }

    #[tokio::test]
    async fn the_voice_list_is_kept_after_an_install_only_and_can_be_restored() {
        let d = tmp();
        let zip = zip_of(&files_for(&TWO));
        let e = entry("fr", None, &TWO, &zip);
        let mut changed = zip.clone();
        let mid = changed.len() / 2;
        changed[mid] ^= 0x01;
        let bad = release(index_text(std::slice::from_ref(&e)), vec![(e.file.clone(), changed)]);
        assert_eq!(run(&bad.source(), &d.0, "fr").await, Outcome::BadPack);
        assert_eq!(run(&bad.source(), &d.0, "ko").await, Outcome::NotListed);
        assert!(!kept_index(&d.0) && known_index(&d.0).voices.is_empty(), "a list nothing was installed from is not kept");
        let good = release(index_text(std::slice::from_ref(&e)), vec![(e.file.clone(), zip)]);
        assert_eq!(run(&good.source(), &d.0, "fr").await, Outcome::Installed);
        assert!(kept_index(&d.0));
        assert_eq!((known_index(&d.0).version, known_index(&d.0).voices.len()), (2026112101, 1));
        // "Restore the built-in version" of the local data puts the built-in list back.
        assert_eq!(crate::pack_update::restore_all(&d.0), "restored");
        assert!(!kept_index(&d.0) && known_index(&d.0).voices.is_empty());
        assert!(d.0.join("fr").is_dir(), "the downloaded voices stay");
        restore_builtin_index(&d.0).unwrap();
    }

    #[test]
    fn installing_a_pack_replaces_the_previews_gemini_made() {
        let d = tmp();
        let (root, cache) = (d.0.join("voices"), d.0.join("cache"));
        let made = |lang: &str, file: &str| cache.join("gemini-3.8-live").join(lang).join(file);
        for (lang, file) in [("es", "Kore_0.pcm"), ("es", "Kore_1.pcm"), ("es", "Kore_2.pcm"), ("es", "Kore_reject.pcm"), ("es", "Puck_0.pcm"), ("pt", "Kore_0.pcm")] {
            let p = made(lang, file);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, vec![1u8; 9_000]).unwrap();
        }
        let previews = VoicePreviews::new(cache.clone(), d.0.join("bundled")).with_packs(root.clone());
        assert_eq!(previews.disk_lines("gemini-3.8-live", "Kore", Lang::Es, 16_000).unwrap()[0].len(), 9_000, "made before");
        let zip = zip_of(&files_for(&TWO));
        let zip_path = d.0.join("in.zip");
        std::fs::write(&zip_path, &zip).unwrap();
        install_and_refresh(&root, &cache, &zip_path, &entry("es", None, &TWO, &zip)).unwrap();
        // The pack's voice in its language: the recordings from now on.
        let lines = previews.disk_lines("gemini-3.8-live", "Kore", Lang::Es, 16_000).unwrap();
        assert_eq!(lines[0].len(), crate::adpcm::decode_wav(&wav()).unwrap().1.len());
        assert!(!made("es", "Kore_reject.pcm").exists());
        // Another voice and another language keep theirs, and a deletion leaves the cache alone.
        assert!(made("es", "Puck_0.pcm").exists() && made("pt", "Kore_0.pcm").exists());
        assert_eq!(remove(&root, "es"), Outcome::Removed);
        assert!(made("es", "Puck_0.pcm").exists() && made("pt", "Kore_0.pcm").exists());
    }

    #[test]
    fn a_delete_answers_after_letting_go_of_the_lock() {
        let d = tmp();
        let zip = zip_of(&files_for(&TWO));
        let zip_path = d.0.join("in.zip");
        std::fs::write(&zip_path, &zip).unwrap();
        install_zip(&d.0, &zip_path, &entry("ja", None, &TWO, &zip)).unwrap();
        let other = Running::take().expect("free");
        assert_eq!(remove_now(&d.0, "ja"), Outcome::Busy);
        drop(other);
        let out = remove_now(&d.0, "ja");
        assert_eq!(out, Outcome::Removed);
        let body = answer(&d.0, &d.0.join("bundled"), &[], Some(out));
        assert_eq!((body["result"].as_str(), body["running"].as_bool()), (Some("removed"), Some(false)));
    }

    #[test]
    fn a_replacement_cut_short_keeps_the_old_voices() {
        let d = tmp();
        let root = &d.0;
        // Stopped between the two renames: start-up puts the old pack back.
        std::fs::create_dir_all(root.join(".old-0a1b-ja")).unwrap();
        std::fs::write(root.join(".old-0a1b-ja").join("Kore_0.wav"), b"old").unwrap();
        // The new one got in place: the old one goes.
        std::fs::create_dir_all(root.join("ko")).unwrap();
        std::fs::create_dir_all(root.join(".old-0a1b-ko")).unwrap();
        clear_unfinished(root);
        assert_eq!(std::fs::read(root.join("ja").join("Kore_0.wav")).unwrap(), b"old");
        assert!(root.join("ko").is_dir() && leftovers(root).is_empty());
        // The new voices cannot take the place: the old ones are put back.
        let lost = Unpacked { path: root.join(".unpack-missing"), done: false };
        assert!(lost.put_in_place(root, "ja").is_err());
        assert_eq!(std::fs::read(root.join("ja").join("Kore_0.wav")).unwrap(), b"old");
        assert!(leftovers(root).is_empty());
    }

    #[test]
    fn startup_clears_unfinished_work() {
        let d = tmp();
        let root = &d.0;
        std::fs::write(root.join(".incoming-1-2.zip"), b"x").unwrap();
        std::fs::create_dir_all(root.join(".unpack-1-2")).unwrap();
        std::fs::write(root.join(".unpack-1-2").join("Kore_0.wav"), b"x").unwrap();
        std::fs::create_dir_all(root.join(".old-1-2")).unwrap();
        std::fs::create_dir_all(root.join("ja")).unwrap();
        clear_unfinished(root);
        assert!(leftovers(root).is_empty());
        assert!(root.join("ja").is_dir(), "finished packs stay");
    }
}


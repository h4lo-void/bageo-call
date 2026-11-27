// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The extra packs: what Bageo downloads on demand besides the nine built-in
//! languages. Three kinds of file, all listed in one index:
//!
//! - bageo-extras-index.json ([ExtrasIndex]): every file with its size and
//!   sha256. It has its own size limit and format number, separate from
//!   bageo-pack-index.json, so older Bageo versions never read it.
//! - lang-<code>.json.gz ([ExtraLang]): one language's names, status, notify,
//!   console and desktop text. No call speech: calls take that from
//!   bageo-locales.json.gz. The rules are the translation rules L01 to L12.
//!   It also carries the drawings of the regions whose main language it is
//!   (art: region -> the same object as that region's art file).
//! - art-<region>.json: one region's drawing for the myna mascot. The page
//!   builds SVG elements from it one by one, so [check_art] accepts only a
//!   short list of elements, attributes and colours and refuses the whole
//!   file on the first thing outside it. Still built and listed for Bageo
//!   versions that download drawings one by one; newer ones take them from
//!   the language files.
//!
//! An art file is {"format":1,"region","version","look","draw"}.
//! look is {"bow":bool,"print":name,"pal":{o1…:colour},"scenes":[[sky,[prop names]]]}.
//! draw is {"print":[node],"item":[node],"props":{name:[node]},"acc":[{"l":[levels],"nodes":[node]}]}.
//! A node is ["tag", {attributes}, [children]]; the children are optional.
//! The same checks run when the files are built and when Bageo reads them.
//!
//! Where a drawing ships follows its region's main language (lang in
//! regions/<code>.json, [art_home]): an extra language's file carries it; a
//! region of a core language, and each of [BUILTIN_ART_REGIONS], has its
//! drawing built into Bageo ([ART_BUNDLE_FILE], made by [build_art_bundle]).
//!
//! art in a language file needs no new format number: a Bageo that does not
//! know the field ignores it (unknown fields are ignored), and one that does
//! checks each drawing on its own and leaves out only the ones that fail.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::check::{push, valid_code, valid_commit, valid_min_bageo};
use crate::data::official_ext_index;
use crate::guard::Writing;
use crate::locales::{
    check_loc_file, check_names, ext_lang, lrule, pipeline_full, unpack_locales, Ctx, English, Kind, LocFile, LocaleSources, Names,
    MAX_LANG_BYTES, READY_PERCENT, REVIEW_STATES,
};
use crate::source::sorted_paths;
use crate::util::{is_sha256_hex, now_days, sha256_hex, valid_date, valid_rfc3339_utc};
use crate::Issue;

/// The extras format this build reads and writes.
pub const EXTRAS_FORMAT: u32 = 1;
/// The index's file name in a release.
pub const EXTRAS_INDEX_FILE: &str = "bageo-extras-index.json";
/// The index may not be larger than this.
pub const MAX_INDEX_BYTES: usize = 256 * 1024;
/// A language file, packed and unpacked, may not be larger than this.
pub const MAX_EXTRA_LANG_BYTES: usize = MAX_LANG_BYTES;
/// An art file may not be larger than this, may not have more nodes than
/// [MAX_ART_NODES] or be nested deeper than [MAX_ART_DEPTH].
pub const MAX_ART_BYTES: usize = 64 * 1024;
pub const MAX_ART_NODES: usize = 800;
pub const MAX_ART_DEPTH: usize = 6;
/// A language file carries at most this many region drawings.
pub const MAX_LANG_ART: usize = 16;
/// The drawings built into Bageo (crates/agent/data/packs/ in bageo-call).
pub const ART_BUNDLE_FILE: &str = "bageo-art.json.gz";
/// The built-in drawings, packed and unpacked, may not be larger than this
/// or hold more than [MAX_BUNDLE_ART] regions.
pub const MAX_ART_BUNDLE_BYTES: usize = 512 * 1024;
pub const MAX_BUNDLE_ART: usize = 64;
/// How far a language file's text goes, lowest first. basic: names and
/// whatever text there is; notify: at least [READY_PERCENT] percent of the
/// notification keys; console: notify plus console text. Only for the menu;
/// Bageo works the real level out itself.
pub const LEVELS: [&str; 3] = ["basic", "notify", "console"];
const MAX_ISSUES: usize = 20;

/// Rule codes carried by [Issue::rule] for the index and the art files
/// (language files use the L rules).
pub mod xrule {
    /// The index: unreadable, wrong shape or format number, version.
    pub const INDEX: &str = "X01-index";
    /// Language code not on the official list, or a region code of the wrong shape.
    pub const CODE: &str = "X02-code";
    /// File name with a path, or not lang-<code>.json.gz / art-<region>.json.
    pub const FILE: &str = "X03-file";
    /// Over a size limit.
    pub const SIZE: &str = "X04-size";
    /// sha256, version, minimum Bageo version, status or level.
    pub const FIELD: &str = "X05-field";
}

/// Rule codes of an art file.
pub mod arule {
    /// Unreadable, wrong shape or format number, wrong region, bad version.
    pub const FORMAT: &str = "A01-format";
    /// More than 64 KiB, 800 nodes or 6 levels.
    pub const SIZE: &str = "A02-size";
    /// An element outside g, path, ellipse, circle, rect, line, polyline, polygon.
    pub const ELEMENT: &str = "A03-element";
    /// An attribute outside the list (on* handlers, href, style, class, id …).
    pub const ATTRIBUTE: &str = "A04-attribute";
    /// A value outside what its attribute may hold: colours are #hex, none or
    /// var(--bts-xx); clip-path is one of two fixed references; no url( anywhere else.
    pub const VALUE: &str = "A05-value";
    /// A look.pal entry with an unknown key or a colour outside #hex and var(--bts-xx).
    pub const PALETTE: &str = "A06-palette";
    /// A drawing with no regions/<code>.json, whose region's main language is
    /// not on the official list, or whose language has no text to carry it.
    pub const HOME: &str = "A07-home";
}

const ELEMENTS: &[&str] = &["g", "path", "ellipse", "circle", "rect", "line", "polyline", "polygon"];
const ATTRIBUTES: &[&str] = &[
    "d", "cx", "cy", "r", "rx", "ry", "x", "y", "width", "height", "points", "x1", "y1", "x2", "y2", "transform", "opacity", "fill",
    "fill-opacity", "stroke", "fill-rule", "clip-path",
];
const CLIPS: &[&str] = &["url(#bts-clip-win)", "url(#bts-clip-body)"];
const PAL_KEYS: &[&str] = &["o1", "o1d", "o2", "o3", "o3l", "o4", "o5", "o5l", "tie"];
const SKIES: &[&str] = &["day", "dusk", "night"];

// ---- the index -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtrasLangRef {
    pub code: String,
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub version: u64,
    pub min_bageo: String,
    pub status: String,
    pub level: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtrasArtRef {
    pub region: String,
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
    pub version: u64,
    pub min_bageo: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtrasIndex {
    pub format: u32,
    /// YYYYMMDDNN of the release.
    pub version: u64,
    pub langs: Vec<ExtrasLangRef>,
    pub art: Vec<ExtrasArtRef>,
}

pub fn lang_file_name(code: &str) -> String {
    format!("lang-{code}.json.gz")
}

pub fn art_file_name(region: &str) -> String {
    format!("art-{region}.json")
}

/// YYYYMMDDNN with a real date (the clock is not consulted: a computer whose
/// clock is behind must still read a release).
fn valid_version(v: u64) -> bool {
    (1_000_000_000..10_000_000_000).contains(&v) && valid_date((v / 1_000_000) as i64, (v / 10_000 % 100) as u32, (v / 100 % 100) as u32)
}

fn short(s: &str) -> String {
    let t: String = s.chars().take(40).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

fn index_issue(field: &str, rule: &'static str, detail: impl Into<String>) -> Issue {
    Issue::new(EXTRAS_INDEX_FILE, field, rule, detail)
}

/// Checks the fields every file entry shares; Err is the first problem.
fn check_entry_common(file: &str, want: &str, bytes: u64, max: usize, sha256: &str, version: u64, min_bageo: &str) -> Result<(), (&'static str, &'static str, String)> {
    if file != want {
        return Err(("file", xrule::FILE, format!("\"{}\" must be \"{want}\" (a plain name, never a path)", short(file))));
    }
    if bytes == 0 || bytes > max as u64 {
        return Err(("bytes", xrule::SIZE, format!("{bytes} bytes, must be 1 to {max}")));
    }
    if !is_sha256_hex(sha256) {
        return Err(("sha256", xrule::FIELD, "must be 64 lower-case hex digits".to_string()));
    }
    if !valid_version(version) {
        return Err(("version", xrule::FIELD, format!("{version} is not YYYYMMDDNN")));
    }
    if !valid_min_bageo(min_bageo) {
        return Err(("min_bageo", xrule::FIELD, "must look like 1.7.0".to_string()));
    }
    Ok(())
}

fn check_lang_ref(r: &ExtrasLangRef) -> Result<(), (&'static str, &'static str, String)> {
    if let Err(e) = ext_lang(&r.code) {
        return Err(("code", xrule::CODE, e));
    }
    check_entry_common(&r.file, &lang_file_name(&r.code), r.bytes, MAX_EXTRA_LANG_BYTES, &r.sha256, r.version, &r.min_bageo)?;
    if !REVIEW_STATES.contains(&r.status.as_str()) {
        return Err(("status", xrule::FIELD, format!("must be one of {}", REVIEW_STATES.join(", "))));
    }
    if !LEVELS.contains(&r.level.as_str()) {
        return Err(("level", xrule::FIELD, format!("must be one of {}", LEVELS.join(", "))));
    }
    Ok(())
}

fn check_art_ref(r: &ExtrasArtRef) -> Result<(), (&'static str, &'static str, String)> {
    if !valid_code(&r.region) {
        return Err(("region", xrule::CODE, format!("\"{}\" is not a region code (lower-case letters and '-', 2 to 24 characters)", short(&r.region))));
    }
    check_entry_common(&r.file, &art_file_name(&r.region), r.bytes, MAX_ART_BYTES, &r.sha256, r.version, &r.min_bageo)
}

/// Reads the index. The whole file is refused when it is over 256 KiB, not
/// JSON, has no format number, or has a newer format than this build reads
/// (the message says to update Bageo first). Unknown fields are ignored. An
/// entry that breaks a rule is left out on its own and reported; the rest is
/// kept. A code or region listed twice keeps its first entry.
pub fn parse_extras_index(text: &str) -> Result<(ExtrasIndex, Vec<Issue>), Vec<Issue>> {
    let one = |field: &str, rule: &'static str, d: String| vec![index_issue(field, rule, d)];
    if text.len() > MAX_INDEX_BYTES {
        return Err(one("", xrule::SIZE, format!("{} bytes, the limit is {MAX_INDEX_BYTES}", text.len())));
    }
    let v: Value = serde_json::from_str(text).map_err(|e| one("", xrule::INDEX, e.to_string()))?;
    let Some(o) = v.as_object() else { return Err(one("", xrule::INDEX, "not a JSON object".to_string())) };
    match o.get("format").and_then(Value::as_u64) {
        None => return Err(one("format", xrule::INDEX, "missing or not a number".to_string())),
        Some(n) if n > EXTRAS_FORMAT as u64 => {
            return Err(one("format", xrule::INDEX, format!("format {n} is newer than this Bageo reads ({EXTRAS_FORMAT}); update Bageo first")))
        }
        Some(n) if n != EXTRAS_FORMAT as u64 => return Err(one("format", xrule::INDEX, format!("format {n} is not {EXTRAS_FORMAT}"))),
        Some(_) => {}
    }
    let version = match o.get("version").and_then(Value::as_u64) {
        Some(n) if valid_version(n) => n,
        _ => return Err(one("version", xrule::INDEX, "must be YYYYMMDDNN".to_string())),
    };
    let mut issues = Vec::new();
    let list = |key: &str| -> Result<Vec<Value>, Vec<Issue>> {
        match o.get(key) {
            None => Ok(Vec::new()),
            Some(Value::Array(a)) => Ok(a.clone()),
            Some(_) => Err(one(key, xrule::INDEX, "must be a list".to_string())),
        }
    };
    let (raw_langs, raw_art) = (list("langs")?, list("art")?);
    let (mut langs, mut art) = (Vec::new(), Vec::new());
    let mut seen = std::collections::BTreeSet::new();
    for (i, item) in raw_langs.into_iter().enumerate() {
        let at = format!("langs[{i}]");
        match serde_json::from_value::<ExtrasLangRef>(item) {
            Err(e) => issues.push(index_issue(&at, xrule::INDEX, format!("left out: {e}"))),
            Ok(r) => match check_lang_ref(&r) {
                Err((f, rule, d)) => issues.push(index_issue(&format!("{at}.{f}"), rule, format!("left out: {d}"))),
                Ok(()) if !seen.insert(r.code.clone()) => {
                    issues.push(index_issue(&format!("{at}.code"), xrule::CODE, format!("left out: \"{}\" is listed twice", r.code)))
                }
                Ok(()) => langs.push(r),
            },
        }
    }
    seen.clear();
    for (i, item) in raw_art.into_iter().enumerate() {
        let at = format!("art[{i}]");
        match serde_json::from_value::<ExtrasArtRef>(item) {
            Err(e) => issues.push(index_issue(&at, xrule::INDEX, format!("left out: {e}"))),
            Ok(r) => match check_art_ref(&r) {
                Err((f, rule, d)) => issues.push(index_issue(&format!("{at}.{f}"), rule, format!("left out: {d}"))),
                Ok(()) if !seen.insert(r.region.clone()) => {
                    issues.push(index_issue(&format!("{at}.region"), xrule::CODE, format!("left out: \"{}\" is listed twice", r.region)))
                }
                Ok(()) => art.push(r),
            },
        }
    }
    Ok((ExtrasIndex { format: EXTRAS_FORMAT, version, langs, art }, issues))
}

// ---- a language file --------------------------------------------------------------------------

/// lang-<code>.json (gzipped for download): one language, no call speech.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraLang {
    pub format: u32,
    pub code: String,
    /// YYYYMMDDNN.
    pub version: u64,
    pub built_at: String,
    pub source_commit: String,
    #[serde(default)]
    pub names: Names,
    /// The lowest review state of what the file carries.
    #[serde(default)]
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify: Option<LocFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<LocFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop: Option<LocFile>,
    /// The drawings of the regions whose main language this is: region -> the
    /// same object as art-<region>.json.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub art: BTreeMap<String, Value>,
}

/// Reads a downloaded language file for code: unpacks it counting as it goes
/// (600 KiB packed and unpacked), then runs the translation rules the way
/// Bageo does. A wrong or newer format, a wrong code, header or names refuse
/// the file (Err); a key that breaks a rule is dropped on its own and
/// reported, so one bad sentence never costs a language. Each region drawing
/// goes through [check_art] on its own: one that fails is left out and
/// reported, the language and the other drawings stay.
pub fn parse_lang_file(gz: &[u8], code: &str, english: &English) -> Result<(ExtraLang, Vec<Issue>), Vec<Issue>> {
    let file = lang_file_name(code);
    let rename = |v: Vec<Issue>| -> Vec<Issue> {
        v.into_iter()
            .map(|mut i| {
                i.file = file.clone();
                i
            })
            .collect()
    };
    let one = |field: &str, rule: &'static str, d: String| vec![Issue::new(&file, field, rule, d)];
    let Some(info) = ext_lang(code).ok() else { return Err(one("", lrule::CODE, format!("\"{}\" is not an extra language code", short(code)))) };
    let json = unpack_locales(gz, MAX_EXTRA_LANG_BYTES).map_err(rename)?;
    let mut v: Value = serde_json::from_slice(&json).map_err(|e| one("", lrule::FORMAT, e.to_string()))?;
    match v.get("format").and_then(Value::as_u64) {
        None => return Err(one("format", lrule::FORMAT, "missing".to_string())),
        Some(n) if n > EXTRAS_FORMAT as u64 => {
            return Err(one("format", lrule::FORMAT, format!("format {n} is newer than this Bageo reads ({EXTRAS_FORMAT}); update Bageo first")))
        }
        Some(n) if n != EXTRAS_FORMAT as u64 => return Err(one("format", lrule::FORMAT, format!("format {n} is not {EXTRAS_FORMAT}"))),
        Some(_) => {}
    }
    // Taken out before the rest is read, so a broken drawing never costs the language.
    let raw_art = v.as_object_mut().and_then(|o| o.remove("art"));
    let mut lang: ExtraLang = serde_json::from_value(v).map_err(|e| one("", lrule::FORMAT, e.to_string()))?;
    let mut bad = Vec::new();
    if lang.code != code {
        bad.push(Issue::new(&file, "code", lrule::CODE, format!("\"{}\" must be \"{code}\"", short(&lang.code))));
    }
    if !valid_version(lang.version) {
        bad.push(Issue::new(&file, "version", lrule::FORMAT, "must be YYYYMMDDNN"));
    }
    if !valid_rfc3339_utc(&lang.built_at) {
        bad.push(Issue::new(&file, "built_at", lrule::FORMAT, "must look like 2026-11-20T08:00:00Z"));
    }
    if !valid_commit(&lang.source_commit) {
        bad.push(Issue::new(&file, "source_commit", lrule::FORMAT, "must be 1 to 64 letters, digits, '.', '_' or '-'"));
    }
    check_names(&mut bad, &file, "names.", code, info, &lang.names);
    if !bad.is_empty() {
        return Err(bad);
    }
    let mut issues = Vec::new();
    if !REVIEW_STATES.contains(&lang.status.as_str()) {
        issues.push(Issue::new(&file, "status", lrule::FORMAT, format!("must be one of {}", REVIEW_STATES.join(", "))));
        lang.status = "ai_draft".to_string();
    }
    for kind in [Kind::Notify, Kind::Console, Kind::Desktop] {
        let slot = match kind {
            Kind::Notify => &mut lang.notify,
            Kind::Console => &mut lang.console,
            _ => &mut lang.desktop,
        };
        let Some(f) = slot.take() else { continue };
        let Some(en) = english.files.get(&kind) else {
            issues.push(Issue::new(&file, kind.name(), lrule::KEY, "this Bageo has no English text for it; left out"));
            continue;
        };
        let prefix = format!("{}.", kind.name());
        let cx = Ctx { file: &file, prefix: &prefix, kind, info, strict: false };
        let kept = check_loc_file(&mut issues, &cx, &f, en);
        if !kept.is_empty() {
            *slot = Some(kept);
        }
    }
    lang.art = checked_art_map(&file, raw_art, MAX_LANG_ART, &mut issues);
    Ok((lang, issues))
}

/// Region drawings inside a language file or the built-in bundle (region ->
/// an art file's object). Each goes through [check_art] on its own: one that
/// fails is left out and reported, the others are kept.
fn checked_art_map(file: &str, raw: Option<Value>, max: usize, issues: &mut Vec<Issue>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    let Some(raw) = raw else { return out };
    let Value::Object(m) = raw else {
        issues.push(Issue::new(file, "art", arule::FORMAT, "must be an object of region drawings; left out"));
        return out;
    };
    for (region, art) in m {
        let at = format!("art.{}", short(&region));
        if !valid_code(&region) {
            issues.push(Issue::new(file, &at, arule::FORMAT, "not a region code; left out"));
            continue;
        }
        if out.len() >= max {
            issues.push(Issue::new(file, &at, arule::SIZE, format!("more than {max} region drawings; left out")));
            continue;
        }
        let bytes = match serde_json::to_vec(&art) {
            Ok(b) => b,
            Err(e) => {
                issues.push(Issue::new(file, &at, arule::FORMAT, format!("left out: {e}")));
                continue;
            }
        };
        match check_art(&art_file_name(&region), &bytes, &region).first() {
            None => {
                out.insert(region, art);
            }
            Some(bad) => {
                let field = if bad.field.is_empty() { at } else { format!("{at}.{}", bad.field) };
                issues.push(Issue::new(file, &field, bad.rule, format!("left out: {}", bad.detail)));
            }
        }
    }
    out
}

// ---- where a drawing ships ---------------------------------------------------------------------

/// The regions Bageo has always shown without a download. Their drawings are
/// built in whatever the region's main language (tr speaks Turkish, gulf
/// Arabic, both extra languages).
pub const BUILTIN_ART_REGIONS: [&str; 9] = ["tw", "jp", "kr", "us", "de", "fr", "tr", "br", "gulf"];

/// Where a region's drawing ships.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtHome {
    /// One of [BUILTIN_ART_REGIONS], or the region's main language is one of
    /// the nine core languages, which have no language file: the drawing is
    /// built into Bageo ([ART_BUNDLE_FILE]).
    Builtin,
    /// The extra language (primary code: ar, nb …) whose file carries it.
    Lang(String),
}

/// Where the drawing of a region whose main language is region_lang (lang in
/// regions/<code>.json: ar-EG, nb, cmn, en-IN …) ships; the code is read the
/// way the language checks read it ([Writing::of]). None when the code is
/// not on the official list.
pub fn art_home(region_lang: &str) -> Option<ArtHome> {
    let code = Writing::of(region_lang).code;
    if code.is_empty() {
        return None;
    }
    Some(if official_ext_index(code).is_some() { ArtHome::Lang(code.to_string()) } else { ArtHome::Builtin })
}

/// Each region's main language (lang of regions/<code>.json), by region, and
/// the files whose lang cannot be read. No regions folder is no regions. Only
/// that field is read here: the region rules are check_sources'.
pub fn load_region_langs(dir: &Path) -> (BTreeMap<String, String>, Vec<Issue>) {
    let mut langs = BTreeMap::new();
    let mut out = Vec::new();
    let Ok(paths) = sorted_paths(&dir.join("regions")) else { return (langs, out) };
    for p in paths.iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json")) {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let lang = fs::read(p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("lang").and_then(Value::as_str).map(str::to_string));
        match lang {
            Some(l) => {
                langs.insert(name.trim_end_matches(".json").to_string(), l);
            }
            None => push(&mut out, &format!("regions/{name}"), "lang", arule::HOME, "cannot read the region's main language"),
        }
    }
    (langs, out)
}

/// Where each drawing of art ships: built in for [BUILTIN_ART_REGIONS],
/// else [art_home]. A drawing without a regions/<code>.json, or whose
/// region's main language is not on the official list, is an issue.
pub fn art_homes(art: &BTreeMap<String, Vec<u8>>, region_langs: &BTreeMap<String, String>) -> (BTreeMap<String, ArtHome>, Vec<Issue>) {
    let mut homes = BTreeMap::new();
    let mut out = Vec::new();
    for region in art.keys() {
        let file = format!("art/{region}.json");
        match region_langs.get(region) {
            None => push(&mut out, &file, "region", arule::HOME, format!("there is no regions/{region}.json; every drawing belongs to a region")),
            Some(lang) => match art_home(lang) {
                None => push(&mut out, &file, "region", arule::HOME, format!("regions/{region}.json: main language \"{}\" is not on the official list", short(lang))),
                Some(_) if BUILTIN_ART_REGIONS.contains(&region.as_str()) => {
                    homes.insert(region.clone(), ArtHome::Builtin);
                }
                Some(h) => {
                    homes.insert(region.clone(), h);
                }
            },
        }
    }
    (homes, out)
}

/// What packs check runs on art/: every drawing passes the art rules and
/// belongs to a region, and a region of an extra language has that
/// language's notify or console text to ride in (a language file is built
/// only then). Returns the number of drawings and the issues.
pub fn check_art_sources(dir: &Path, locales: &LocaleSources) -> (usize, Vec<Issue>) {
    let art = match load_art(dir) {
        Ok(a) => a,
        Err(issues) => return (0, issues),
    };
    if art.is_empty() {
        return (0, Vec::new());
    }
    let (region_langs, mut out) = load_region_langs(dir);
    for (region, src) in &art {
        if let Err(issues) = read_art_source(&format!("art/{region}.json"), src, region) {
            out.extend(issues);
        }
    }
    let (homes, issues) = art_homes(&art, &region_langs);
    out.extend(issues);
    for (region, home) in &homes {
        let ArtHome::Lang(code) = home else { continue };
        let has_text = locales.langs.iter().any(|l| l.code == *code && (l.files.contains_key(&Kind::Notify) || l.files.contains_key(&Kind::Console)));
        if !has_text {
            push(&mut out, &format!("art/{region}.json"), "region", arule::HOME, format!("its region speaks {code}, which has no notify or console text to carry the drawing"));
        }
    }
    (art.len(), out)
}

fn level_of(english: &English, notify: Option<&LocFile>, console: Option<&LocFile>) -> &'static str {
    let en_keys = english.files.get(&Kind::Notify).map_or(0, |e| e.text.keys().count());
    let n = notify.map_or(0, |f| f.keys().count());
    if en_keys > 0 && n * 100 >= en_keys * READY_PERCENT {
        if console.is_some() {
            "console"
        } else {
            "notify"
        }
    } else {
        "basic"
    }
}

/// JSON gzipped without a file name or time in the header, so equal input
/// gives equal bytes.
fn gzip_best(json: &[u8]) -> Result<Vec<u8>, String> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(json).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

// ---- the art files -----------------------------------------------------------------------------

struct Walk<'a> {
    file: &'a str,
    out: Vec<Issue>,
    nodes: usize,
}

impl Walk<'_> {
    fn bad(&mut self, field: &str, rule: &'static str, detail: impl Into<String>) {
        if self.out.len() < MAX_ISSUES {
            self.out.push(Issue::new(self.file, field, rule, detail));
        }
    }

    fn name_ok(s: &str) -> bool {
        (1..=32).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    }

    fn node(&mut self, path: &str, v: &Value, depth: usize) {
        self.nodes += 1;
        if self.nodes > MAX_ART_NODES {
            if self.nodes == MAX_ART_NODES + 1 {
                self.bad("", arule::SIZE, format!("more than {MAX_ART_NODES} nodes"));
            }
            return;
        }
        if depth > MAX_ART_DEPTH {
            self.bad(path, arule::SIZE, format!("nested deeper than {MAX_ART_DEPTH} levels"));
            return;
        }
        let Some(a) = v.as_array().filter(|a| (2..=3).contains(&a.len())) else {
            return self.bad(path, arule::FORMAT, "a node is [\"tag\", {attributes}, [children]]");
        };
        let Some(tag) = a[0].as_str() else { return self.bad(path, arule::FORMAT, "the tag must be a string") };
        if !ELEMENTS.contains(&tag) {
            self.bad(path, arule::ELEMENT, format!("<{}> is not an allowed element", short(tag)));
        }
        let Some(attrs) = a[1].as_object() else { return self.bad(path, arule::FORMAT, "the attributes must be an object") };
        for (k, val) in attrs {
            let at = format!("{path}.{k}");
            let known = ATTRIBUTES.contains(&k.as_str()) || k.strip_prefix("stroke-").is_some_and(|r| (1..=16).contains(&r.len()) && r.bytes().all(|b| b.is_ascii_lowercase()));
            if !known {
                self.bad(&at, arule::ATTRIBUTE, format!("attribute \"{}\" is not allowed", short(k)));
                continue;
            }
            let s = match val {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => {
                    self.bad(&at, arule::VALUE, "must be a string or a number");
                    continue;
                }
            };
            let ok = match k.as_str() {
                "fill" | "stroke" => color_ok(&s, true),
                "clip-path" => CLIPS.contains(&s.as_str()),
                _ => plain_value_ok(&s),
            };
            if !ok {
                self.bad(&at, arule::VALUE, format!("value \"{}\" is not allowed for {k}", short(&s)));
            }
        }
        if let Some(c) = a.get(2) {
            let Some(children) = c.as_array() else { return self.bad(path, arule::FORMAT, "the children must be a list") };
            for (i, child) in children.iter().enumerate() {
                self.node(&format!("{path}[{i}]"), child, depth + 1);
            }
        }
    }

    fn list(&mut self, path: &str, v: Option<&Value>) {
        match v {
            None => {}
            Some(Value::Array(a)) => {
                for (i, n) in a.iter().enumerate() {
                    self.node(&format!("{path}[{i}]"), n, 1);
                }
            }
            Some(_) => self.bad(path, arule::FORMAT, "must be a list of nodes"),
        }
    }

    fn look(&mut self, look: Option<&Value>) {
        let Some(o) = look.and_then(Value::as_object) else { return self.bad("look", arule::FORMAT, "missing or not an object") };
        if !o.get("bow").is_some_and(Value::is_boolean) {
            self.bad("look.bow", arule::FORMAT, "must be true or false");
        }
        if !o.get("print").and_then(Value::as_str).is_some_and(Self::name_ok) {
            self.bad("look.print", arule::FORMAT, "must be a name of lower-case letters, digits and '-', 1 to 32 characters");
        }
        match o.get("pal").and_then(Value::as_object) {
            None => self.bad("look.pal", arule::FORMAT, "missing or not an object"),
            Some(pal) => {
                for (k, c) in pal {
                    let at = format!("look.pal.{k}");
                    if !PAL_KEYS.contains(&k.as_str()) {
                        self.bad(&at, arule::PALETTE, format!("\"{}\" is not a palette key", short(k)));
                    } else if !c.as_str().is_some_and(|c| color_ok(c, false)) {
                        self.bad(&at, arule::PALETTE, "a colour is #hex or var(--bts-xx)");
                    }
                }
            }
        }
        match o.get("scenes").and_then(Value::as_array) {
            Some(sc) if (1..=6).contains(&sc.len()) => {
                for (i, s) in sc.iter().enumerate() {
                    let at = format!("look.scenes[{i}]");
                    let ok = s.as_array().is_some_and(|p| {
                        p.len() == 2
                            && p[0].as_str().is_some_and(|k| SKIES.contains(&k))
                            && p[1].as_array().is_some_and(|ps| ps.len() <= 12 && ps.iter().all(|x| x.as_str().is_some_and(Self::name_ok)))
                    });
                    if !ok {
                        self.bad(&at, arule::FORMAT, "a scene is [\"day\"|\"dusk\"|\"night\", [prop names]]");
                    }
                }
            }
            _ => self.bad("look.scenes", arule::FORMAT, "must be a list of 1 to 6 scenes"),
        }
    }

    fn draw(&mut self, draw: Option<&Value>) {
        let Some(o) = draw.and_then(Value::as_object) else { return self.bad("draw", arule::FORMAT, "missing or not an object") };
        self.list("draw.print", o.get("print"));
        self.list("draw.item", o.get("item"));
        match o.get("props") {
            None => {}
            Some(Value::Object(m)) if m.len() <= 40 => {
                for (k, n) in m {
                    if !Self::name_ok(k) {
                        self.bad(&format!("draw.props.{}", short(k)), arule::FORMAT, "a prop name is lower-case letters, digits and '-', 1 to 32 characters");
                        continue;
                    }
                    self.list(&format!("draw.props.{k}"), Some(n));
                }
            }
            Some(_) => self.bad("draw.props", arule::FORMAT, "must be an object of at most 40 props"),
        }
        match o.get("acc") {
            None => {}
            Some(Value::Array(a)) if a.len() <= 20 => {
                for (i, x) in a.iter().enumerate() {
                    let at = format!("draw.acc[{i}]");
                    let Some(m) = x.as_object() else {
                        self.bad(&at, arule::FORMAT, "must be {\"l\":[levels],\"nodes\":[node]}");
                        continue;
                    };
                    let levels_ok = match m.get("l") {
                        None => true,
                        Some(Value::Array(l)) => l.len() <= 5 && l.iter().all(|n| n.as_u64().is_some_and(|n| (1..=5).contains(&n))),
                        Some(_) => false,
                    };
                    if !levels_ok {
                        self.bad(&format!("{at}.l"), arule::FORMAT, "levels are numbers from 1 to 5");
                    }
                    self.list(&format!("{at}.nodes"), m.get("nodes"));
                }
            }
            Some(_) => self.bad("draw.acc", arule::FORMAT, "must be a list of at most 20 entries"),
        }
    }
}

/// #hex (3, 4, 6 or 8 digits), none (when allowed) or var(--bts-xx).
fn color_ok(s: &str, allow_none: bool) -> bool {
    if s == "none" {
        return allow_none;
    }
    if let Some(h) = s.strip_prefix('#') {
        return matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit());
    }
    s.strip_prefix("var(--bts-")
        .and_then(|r| r.strip_suffix(')'))
        .is_some_and(|n| (1..=8).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()))
}

/// Numbers, path data, points, transforms and keywords: letters, digits and
/// a few separators, never a url( or anything that could start a reference.
fn plain_value_ok(s: &str) -> bool {
    s.len() <= 4096
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b',' | b'-' | b'+' | b'(' | b')' | b'%'))
        && !s.to_ascii_lowercase().contains("url")
}

fn validate_art(file: &str, bytes: &[u8], region: &str, need_version: bool) -> Result<Value, Vec<Issue>> {
    let one = |field: &str, rule: &'static str, d: String| vec![Issue::new(file, field, rule, d)];
    if bytes.len() > MAX_ART_BYTES {
        return Err(one("", arule::SIZE, format!("{} bytes, the limit is {MAX_ART_BYTES}", bytes.len())));
    }
    let v: Value = serde_json::from_slice(bytes).map_err(|e| one("", arule::FORMAT, e.to_string()))?;
    let Some(o) = v.as_object() else { return Err(one("", arule::FORMAT, "not a JSON object".to_string())) };
    match o.get("format").and_then(Value::as_u64) {
        None => return Err(one("format", arule::FORMAT, "missing or not a number".to_string())),
        Some(n) if n > EXTRAS_FORMAT as u64 => {
            return Err(one("format", arule::FORMAT, format!("format {n} is newer than this Bageo reads ({EXTRAS_FORMAT}); update Bageo first")))
        }
        Some(n) if n != EXTRAS_FORMAT as u64 => return Err(one("format", arule::FORMAT, format!("format {n} is not {EXTRAS_FORMAT}"))),
        Some(_) => {}
    }
    let mut w = Walk { file, out: Vec::new(), nodes: 0 };
    if !valid_code(region) || o.get("region").and_then(Value::as_str) != Some(region) {
        w.bad("region", arule::FORMAT, format!("must be \"{}\", the region of the file name", short(region)));
    }
    if need_version && !o.get("version").and_then(Value::as_u64).is_some_and(valid_version) {
        w.bad("version", arule::FORMAT, "must be YYYYMMDDNN");
    }
    w.look(o.get("look"));
    w.draw(o.get("draw"));
    if w.out.is_empty() {
        Ok(v)
    } else {
        Err(w.out)
    }
}

/// The rules for a downloaded art file of region: a whole file is refused on
/// the first element, attribute or value outside the allowed lists (an empty
/// result means it passes).
pub fn check_art(file: &str, bytes: &[u8], region: &str) -> Vec<Issue> {
    validate_art(file, bytes, region, true).err().unwrap_or_default()
}

/// A source art file (no version yet) of region, as (look, draw).
pub fn read_art_source(file: &str, bytes: &[u8], region: &str) -> Result<(Value, Value), Vec<Issue>> {
    let v = validate_art(file, bytes, region, false)?;
    Ok((v["look"].clone(), v["draw"].clone()))
}

#[derive(Serialize)]
struct ArtOut {
    format: u32,
    region: String,
    version: u64,
    look: Value,
    draw: Value,
}

/// The art sources under dir/art (art/<region>.json), by region; no art
/// folder is no art. A file that is too big or wrongly named is an issue.
pub fn load_art(dir: &Path) -> Result<BTreeMap<String, Vec<u8>>, Vec<Issue>> {
    let mut art = BTreeMap::new();
    let mut out = Vec::new();
    let Ok(paths) = sorted_paths(&dir.join("art")) else { return Ok(art) };
    for p in paths {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let file = format!("art/{name}");
        let region = name.strip_suffix(".json").unwrap_or("");
        if !p.is_file() || !valid_code(region) {
            push(&mut out, &file, "", arule::FORMAT, "art files are art/<region>.json with a region code as the name");
            continue;
        }
        match fs::metadata(&p) {
            Ok(m) if m.len() as usize > MAX_ART_BYTES => push(&mut out, &file, "", arule::SIZE, format!("{} bytes, the limit is {MAX_ART_BYTES}", m.len())),
            Ok(_) => match fs::read(&p) {
                Ok(b) => {
                    art.insert(region.to_string(), b);
                }
                Err(e) => push(&mut out, &file, "", arule::FORMAT, format!("cannot read: {e}")),
            },
            Err(e) => push(&mut out, &file, "", arule::FORMAT, format!("cannot read: {e}")),
        }
    }
    if out.is_empty() {
        Ok(art)
    } else {
        Err(out)
    }
}

// ---- building ----------------------------------------------------------------------------------

/// Everything build_extras made: the files by name (language and art files),
/// the index and its JSON.
#[derive(Debug, Clone)]
pub struct ExtrasBuild {
    pub files: BTreeMap<String, Vec<u8>>,
    pub index: ExtrasIndex,
    pub index_json: String,
}

/// A source drawing as it ships, with the release version: the object and its bytes.
fn ship_art(region: &str, src: &[u8], version: u64) -> Result<(Value, Vec<u8>), Vec<Issue>> {
    let (look, draw) = read_art_source(&format!("art/{region}.json"), src, region)?;
    let file = art_file_name(region);
    let art = ArtOut { format: EXTRAS_FORMAT, region: region.to_string(), version, look, draw };
    let (value, body) = match (serde_json::to_value(&art), serde_json::to_vec(&art)) {
        (Ok(v), Ok(b)) => (v, b),
        (Err(e), _) | (_, Err(e)) => return Err(vec![Issue::new(&file, "", arule::FORMAT, e.to_string())]),
    };
    let bad = check_art(&file, &body, region);
    if bad.is_empty() {
        Ok((value, body))
    } else {
        Err(bad)
    }
}

/// Checks the sources and builds the extra packs: one language file for
/// every language outside the nine core ones that has notify or console text,
/// carrying the drawings of the regions whose main language it is
/// (region_langs: region to lang of regions/<code>.json); one art file for
/// every entry of art (region to source bytes), for Bageo versions that
/// download drawings one by one; and the index. Nothing is returned unless
/// every rule passed, every drawing of an extra language found its language
/// file, and every file reads back through the same door Bageo uses.
pub fn build_extras(
    ls: &LocaleSources,
    art: &BTreeMap<String, Vec<u8>>,
    region_langs: &BTreeMap<String, String>,
    version: u64,
    commit: &str,
    built_at: &str,
    min_bageo: &str,
) -> Result<ExtrasBuild, Vec<Issue>> {
    let mut out = Vec::new();
    let head = |field: &str, d: &str| Issue::new(EXTRAS_INDEX_FILE, field, xrule::INDEX, d.to_string());
    if !valid_version(version) {
        out.push(head("version", "must be YYYYMMDDNN"));
    }
    if !valid_commit(commit) {
        out.push(head("source_commit", "must be 1 to 64 letters, digits, '.', '_' or '-'"));
    }
    if !valid_rfc3339_utc(built_at) {
        out.push(head("built_at", "must look like 2026-11-20T08:00:00Z"));
    }
    if !valid_min_bageo(min_bageo) {
        out.push(head("min_bageo", "must look like 1.7.0"));
    }
    if !out.is_empty() {
        return Err(out);
    }

    let (checked, issues) = pipeline_full(ls, None, now_days());
    // A language that broke a rule is already reported; none of the files is built then.
    let langs_ok = issues.is_empty();
    out.extend(issues);
    let mut files = BTreeMap::new();
    let mut index = ExtrasIndex { format: EXTRAS_FORMAT, version, langs: Vec::new(), art: Vec::new() };

    let (homes, home_issues) = art_homes(art, region_langs);
    out.extend(home_issues);
    let mut drawings: BTreeMap<String, (Value, Vec<u8>)> = BTreeMap::new();
    for (region, src) in art {
        match ship_art(region, src, version) {
            Ok(d) => {
                drawings.insert(region.clone(), d);
            }
            Err(issues) => out.extend(issues),
        }
    }
    let mut carried = std::collections::BTreeSet::new();

    for (code, c) in checked {
        if !langs_ok || (c.lang.notify.is_none() && c.lang.console.is_none()) {
            continue;
        }
        let file = lang_file_name(&code);
        let level = level_of(&ls.english, c.lang.notify.as_ref(), c.lang.console.as_ref());
        let lang_art: BTreeMap<String, Value> = drawings
            .iter()
            .filter(|(r, _)| matches!(homes.get(*r), Some(ArtHome::Lang(l)) if *l == code))
            .map(|(r, (v, _))| (r.clone(), v.clone()))
            .collect();
        if lang_art.len() > MAX_LANG_ART {
            push(&mut out, &file, "art", arule::SIZE, format!("{} region drawings, the limit is {MAX_LANG_ART}", lang_art.len()));
            continue;
        }
        carried.extend(lang_art.keys().cloned());
        let lang = ExtraLang {
            format: EXTRAS_FORMAT,
            code: code.clone(),
            version,
            built_at: built_at.to_string(),
            source_commit: commit.to_string(),
            names: c.lang.names,
            status: c.text_status,
            notify: c.lang.notify,
            console: c.lang.console,
            desktop: c.desktop,
            art: lang_art,
        };
        let json = match serde_json::to_vec(&lang) {
            Ok(j) => j,
            Err(e) => {
                push(&mut out, &file, "", lrule::FORMAT, e.to_string());
                continue;
            }
        };
        if json.len() > MAX_EXTRA_LANG_BYTES {
            push(&mut out, &file, "", lrule::SIZE, format!("{} bytes unpacked, the limit is {MAX_EXTRA_LANG_BYTES}", json.len()));
            continue;
        }
        let gz = match gzip_best(&json) {
            Ok(g) => g,
            Err(e) => {
                push(&mut out, &file, "", lrule::FORMAT, e);
                continue;
            }
        };
        if gz.len() > MAX_EXTRA_LANG_BYTES {
            push(&mut out, &file, "", lrule::SIZE, format!("{} bytes packed, the limit is {MAX_EXTRA_LANG_BYTES}", gz.len()));
            continue;
        }
        match parse_lang_file(&gz, &code, &ls.english) {
            Ok((back, issues)) if issues.is_empty() && back == lang => {}
            Ok((_, issues)) => {
                out.extend(issues);
                push(&mut out, &file, "", lrule::FORMAT, "the built file does not read back the same");
                continue;
            }
            Err(issues) => {
                out.extend(issues);
                continue;
            }
        }
        index.langs.push(ExtrasLangRef {
            code: code.clone(),
            file: file.clone(),
            bytes: gz.len() as u64,
            sha256: sha256_hex(&gz),
            version,
            min_bageo: min_bageo.to_string(),
            status: lang.status.clone(),
            level: level.to_string(),
        });
        files.insert(file, gz);
    }

    if langs_ok {
        for (region, home) in &homes {
            if let ArtHome::Lang(code) = home {
                if drawings.contains_key(region) && !carried.contains(region) {
                    push(
                        &mut out,
                        &format!("art/{region}.json"),
                        "region",
                        arule::HOME,
                        format!("its region speaks {code}, which has no language file (no notify or console text) to carry the drawing"),
                    );
                }
            }
        }
    }

    for (region, (_, body)) in drawings {
        let file = art_file_name(&region);
        index.art.push(ExtrasArtRef {
            region,
            file: file.clone(),
            bytes: body.len() as u64,
            sha256: sha256_hex(&body),
            version,
            min_bageo: min_bageo.to_string(),
        });
        files.insert(file, body);
    }

    if out.is_empty() && files.is_empty() {
        push(&mut out, EXTRAS_INDEX_FILE, "", xrule::INDEX, "nothing to build: no language with notify or console text and no art");
    }
    let index_json = serde_json::to_string(&index).unwrap_or_default();
    if out.is_empty() {
        match parse_extras_index(&index_json) {
            Ok((back, issues)) if issues.is_empty() && back == index => {}
            Ok((_, issues)) => {
                out.extend(issues);
                push(&mut out, EXTRAS_INDEX_FILE, "", xrule::INDEX, "the built index does not read back the same");
            }
            Err(issues) => out.extend(issues),
        }
    }
    if out.is_empty() {
        Ok(ExtrasBuild { files, index, index_json })
    } else {
        Err(out)
    }
}

// ---- the drawings built into Bageo -----------------------------------------------------------

/// bageo-art.json.gz: the drawings of [BUILTIN_ART_REGIONS] and of the
/// regions whose main language is a core language ([ArtHome::Builtin]); no
/// language file carries them, so Bageo ships them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtBundle {
    pub format: u32,
    /// YYYYMMDDNN.
    pub version: u64,
    pub built_at: String,
    pub source_commit: String,
    /// Region -> the same object as art-<region>.json.
    #[serde(default)]
    pub art: BTreeMap<String, Value>,
}

/// Builds the built-in drawings from the sources: every drawing of art that
/// ships built in ([art_homes]). Returns the gzipped file and
/// the regions in it. Every drawing must pass and belong to a region, and
/// the file reads back through [parse_art_bundle].
pub fn build_art_bundle(
    art: &BTreeMap<String, Vec<u8>>,
    region_langs: &BTreeMap<String, String>,
    version: u64,
    commit: &str,
    built_at: &str,
) -> Result<(Vec<u8>, Vec<String>), Vec<Issue>> {
    let mut out = Vec::new();
    let head = |field: &str, d: &str| Issue::new(ART_BUNDLE_FILE, field, arule::FORMAT, d.to_string());
    if !valid_version(version) {
        out.push(head("version", "must be YYYYMMDDNN"));
    }
    if !valid_commit(commit) {
        out.push(head("source_commit", "must be 1 to 64 letters, digits, '.', '_' or '-'"));
    }
    if !valid_rfc3339_utc(built_at) {
        out.push(head("built_at", "must look like 2026-11-20T08:00:00Z"));
    }
    if !out.is_empty() {
        return Err(out);
    }
    let (homes, issues) = art_homes(art, region_langs);
    out.extend(issues);
    let mut bundle = ArtBundle { format: EXTRAS_FORMAT, version, built_at: built_at.to_string(), source_commit: commit.to_string(), art: BTreeMap::new() };
    for (region, src) in art {
        match ship_art(region, src, version) {
            Ok((value, _)) if homes.get(region) == Some(&ArtHome::Builtin) => {
                bundle.art.insert(region.clone(), value);
            }
            Ok(_) => {}
            Err(issues) => out.extend(issues),
        }
    }
    if out.is_empty() && bundle.art.is_empty() {
        out.push(head("art", "nothing to build: no drawing of a built-in region or of a region whose main language is a core language"));
    }
    if bundle.art.len() > MAX_BUNDLE_ART {
        out.push(Issue::new(ART_BUNDLE_FILE, "art", arule::SIZE, format!("{} drawings, the limit is {MAX_BUNDLE_ART}", bundle.art.len())));
    }
    if !out.is_empty() {
        return Err(out);
    }
    let json = serde_json::to_vec(&bundle).map_err(|e| vec![head("", &e.to_string())])?;
    if json.len() > MAX_ART_BUNDLE_BYTES {
        return Err(vec![Issue::new(ART_BUNDLE_FILE, "", arule::SIZE, format!("{} bytes unpacked, the limit is {MAX_ART_BUNDLE_BYTES}", json.len()))]);
    }
    let gz = gzip_best(&json).map_err(|e| vec![head("", &e)])?;
    match parse_art_bundle(&gz) {
        Ok((back, issues)) if issues.is_empty() && back == bundle => Ok((gz, bundle.art.into_keys().collect())),
        Ok((_, mut issues)) => {
            issues.push(head("", "the built file does not read back the same"));
            Err(issues)
        }
        Err(issues) => Err(issues),
    }
}

/// Reads the built-in drawings: unpacks counting as it goes
/// ([MAX_ART_BUNDLE_BYTES]); a wrong or newer format or header refuses the
/// file; each drawing goes through [check_art] on its own and one that
/// fails is left out and reported.
pub fn parse_art_bundle(gz: &[u8]) -> Result<(ArtBundle, Vec<Issue>), Vec<Issue>> {
    let one = |field: &str, d: String| vec![Issue::new(ART_BUNDLE_FILE, field, arule::FORMAT, d)];
    let json = unpack_locales(gz, MAX_ART_BUNDLE_BYTES).map_err(|v| {
        v.into_iter()
            .map(|mut i| {
                i.file = ART_BUNDLE_FILE.to_string();
                i
            })
            .collect::<Vec<_>>()
    })?;
    let mut v: Value = serde_json::from_slice(&json).map_err(|e| one("", e.to_string()))?;
    match v.get("format").and_then(Value::as_u64) {
        None => return Err(one("format", "missing".to_string())),
        Some(n) if n > EXTRAS_FORMAT as u64 => return Err(one("format", format!("format {n} is newer than this Bageo reads ({EXTRAS_FORMAT}); update Bageo first"))),
        Some(n) if n != EXTRAS_FORMAT as u64 => return Err(one("format", format!("format {n} is not {EXTRAS_FORMAT}"))),
        Some(_) => {}
    }
    let raw_art = v.as_object_mut().and_then(|o| o.remove("art"));
    let mut bundle: ArtBundle = serde_json::from_value(v).map_err(|e| one("", e.to_string()))?;
    let mut bad = Vec::new();
    if !valid_version(bundle.version) {
        bad.push(Issue::new(ART_BUNDLE_FILE, "version", arule::FORMAT, "must be YYYYMMDDNN"));
    }
    if !valid_rfc3339_utc(&bundle.built_at) {
        bad.push(Issue::new(ART_BUNDLE_FILE, "built_at", arule::FORMAT, "must look like 2026-11-20T08:00:00Z"));
    }
    if !valid_commit(&bundle.source_commit) {
        bad.push(Issue::new(ART_BUNDLE_FILE, "source_commit", arule::FORMAT, "must be 1 to 64 letters, digits, '.', '_' or '-'"));
    }
    if !bad.is_empty() {
        return Err(bad);
    }
    let mut issues = Vec::new();
    bundle.art = checked_art_map(ART_BUNDLE_FILE, raw_art, MAX_BUNDLE_ART, &mut issues);
    Ok((bundle, issues))
}

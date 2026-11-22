// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Scam database bundle (`bageo-scams.json.gz`), built from `scams/base.json`,
//! `scams/keywords.json`, and `scams/countries/*.json` in `bageo-packs`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::check::{plain_file_name, push, valid_commit, valid_min_bageo};
use crate::source::sorted_paths;
use crate::util::{is_sha256_hex, now_days, valid_date, valid_rfc3339_utc};
use crate::{rule, Index, Issue, LocalesRef};

pub const SCAMS_FORMAT: u32 = 1;
pub const SCAMS_FILE: &str = "bageo-scams.json.gz";
pub const MAX_SCAMS_BYTES: usize = 8 * 1024 * 1024;
const INDEX_FILE: &str = "bageo-pack-index.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScamBundle {
    pub format: u32,
    pub version: u64,
    pub built_at: String,
    pub source_commit: String,
    /// Combined `scam_db.json` payload (typologies, keywords, own_matter, number_rules).
    pub db: Value,
    /// Combined `country_scams.json` payload (`{"countries": { ... }}`).
    pub countries: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SrcBase {
    format: u32,
    version: u64,
    #[serde(default)]
    about: Option<String>,
    name_languages: Vec<String>,
    typologies: Vec<Value>,
    keywords: Vec<Value>,
    own_matter: Vec<Value>,
    number_rules: Vec<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SrcKeywords {
    format: u32,
    #[serde(default)]
    source_commit: Option<String>,
    keywords: Vec<Value>,
    #[serde(default)]
    own_matter: Vec<Value>,
}

pub(crate) fn valid_version(v: u64, now_day: i64) -> bool {
    if !(2_020_010_100..10_000_000_000).contains(&v) {
        return false;
    }
    let (y, m, d) = ((v / 1_000_000) as i64, (v / 10_000 % 100) as u32, (v / 100 % 100) as u32);
    if !valid_date(y, m, d) {
        return false;
    }
    let _ = now_day;
    true
}

pub fn build_scams_at(dir: &Path, version: u64, commit: &str, built_at: &str) -> Result<ScamBundle, Vec<Issue>> {
    let mut issues = Vec::new();
    if !valid_version(version, now_days()) {
        push(&mut issues, SCAMS_FILE, "version", rule::STATUS, format!("{version} is not YYYYMMDDNN"));
    }
    if !valid_commit(commit) {
        push(&mut issues, SCAMS_FILE, "source_commit", rule::FORMAT, "invalid commit string");
    }
    if !valid_rfc3339_utc(built_at) {
        push(&mut issues, SCAMS_FILE, "built_at", rule::STATUS, "must look like 2026-11-22T08:00:00Z");
    }

    let scams_dir = dir.join("scams");
    let base_path = scams_dir.join("base.json");
    let kw_path = scams_dir.join("keywords.json");
    let countries_dir = scams_dir.join("countries");

    let base: SrcBase = match fs::read(&base_path).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => {
            push(&mut issues, "scams/base.json", "", rule::FORMAT, e);
            return Err(issues);
        }
    };
    if base.format != 1 {
        push(&mut issues, "scams/base.json", "format", rule::FORMAT, format!("format {} is not 1", base.format));
    }
    if base.typologies.is_empty() {
        push(&mut issues, "scams/base.json", "typologies", rule::REQUIRED, "no typologies");
    }

    let ext_kw: SrcKeywords = match fs::read(&kw_path).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
        Ok(v) => v,
        Err(e) => {
            push(&mut issues, "scams/keywords.json", "", rule::FORMAT, e);
            return Err(issues);
        }
    };
    if ext_kw.format != 1 {
        push(&mut issues, "scams/keywords.json", "format", rule::FORMAT, format!("format {} is not 1", ext_kw.format));
    }
    let _ = ext_kw.source_commit;

    let mut seen_kw = BTreeSet::new();
    let mut all_keywords = Vec::new();
    for (src_file, list) in [("scams/base.json", &base.keywords), ("scams/keywords.json", &ext_kw.keywords)] {
        for item in list {
            let Some(lang) = item.get("lang").and_then(Value::as_str) else {
                push(&mut issues, src_file, "keywords", rule::FORMAT, "keyword group missing lang");
                continue;
            };
            if !seen_kw.insert(lang.to_string()) {
                push(&mut issues, src_file, "keywords", rule::CODE, format!("duplicate keyword language \"{lang}\""));
                continue;
            }
            all_keywords.push(item.clone());
        }
    }

    let mut seen_om = BTreeSet::new();
    let mut all_own_matter = Vec::new();
    for (src_file, list) in [("scams/base.json", &base.own_matter), ("scams/keywords.json", &ext_kw.own_matter)] {
        for item in list {
            let Some(lang) = item.get("lang").and_then(Value::as_str) else {
                push(&mut issues, src_file, "own_matter", rule::FORMAT, "own_matter group missing lang");
                continue;
            };
            if !seen_om.insert(lang.to_string()) {
                push(&mut issues, src_file, "own_matter", rule::CODE, format!("duplicate own_matter language \"{lang}\""));
                continue;
            }
            all_own_matter.push(item.clone());
        }
    }

    let mut countries_map = serde_json::Map::new();
    match sorted_paths(&countries_dir) {
        Err(e) => push(&mut issues, "scams/countries", "", rule::FORMAT, format!("cannot read folder: {e}")),
        Ok(paths) => {
            for p in paths.iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json")) {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let rel = format!("scams/countries/{stem}.json");
                if stem.len() != 2 || !stem.bytes().all(|b| b.is_ascii_lowercase()) {
                    push(&mut issues, &rel, "", rule::CODE, format!("\"{stem}\" is not a two-letter country code"));
                    continue;
                }
                match fs::read(p).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string())) {
                    Ok(v) => {
                        if v.get("scams").and_then(Value::as_array).is_none_or(Vec::is_empty) {
                            push(&mut issues, &rel, "scams", rule::REQUIRED, "must contain a non-empty scams array");
                        }
                        countries_map.insert(stem.to_ascii_uppercase(), v);
                    }
                    Err(e) => push(&mut issues, &rel, "", rule::FORMAT, e),
                }
            }
        }
    }
    if countries_map.is_empty() {
        push(&mut issues, "scams/countries", "", rule::REQUIRED, "no country scam files");
    }

    if !issues.is_empty() {
        return Err(issues);
    }

    let mut db_obj = serde_json::Map::new();
    db_obj.insert("version".into(), json!(base.version));
    if let Some(about) = base.about {
        db_obj.insert("about".into(), json!(about));
    }
    db_obj.insert("name_languages".into(), json!(base.name_languages));
    db_obj.insert("typologies".into(), Value::Array(base.typologies));
    db_obj.insert("keywords".into(), Value::Array(all_keywords));
    db_obj.insert("own_matter".into(), Value::Array(all_own_matter));
    db_obj.insert("number_rules".into(), Value::Array(base.number_rules));

    let bundle = ScamBundle {
        format: SCAMS_FORMAT,
        version,
        built_at: built_at.to_string(),
        source_commit: commit.to_string(),
        db: Value::Object(db_obj),
        countries: json!({ "countries": Value::Object(countries_map) }),
    };

    let raw_len = serde_json::to_vec(&bundle).map(|b| b.len()).unwrap_or(usize::MAX);
    if raw_len > MAX_SCAMS_BYTES {
        return Err(vec![Issue::new(
            SCAMS_FILE,
            "",
            rule::LENGTH,
            format!("{raw_len} bytes unpacked, the limit is {MAX_SCAMS_BYTES}"),
        )]);
    }
    Ok(bundle)
}

pub fn pack_scams(b: &ScamBundle) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(b).map_err(|e| e.to_string())?;
    let mut enc = GzEncoder::new(Vec::new(), Compression::best());
    enc.write_all(&json).map_err(|e| e.to_string())?;
    enc.finish().map_err(|e| e.to_string())
}

pub fn unpack_scams_bytes(gz: &[u8], max: usize) -> Result<Vec<u8>, Vec<Issue>> {
    let one = |r: &'static str, d: String| vec![Issue::new(SCAMS_FILE, "", r, d)];
    if gz.len() > max {
        return Err(one(rule::LENGTH, format!("{} bytes packed, the limit is {max}", gz.len())));
    }
    let mut dec = GzDecoder::new(gz);
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = dec.read(&mut buf).map_err(|e| one(rule::FORMAT, format!("cannot unpack: {e}")))?;
        if n == 0 {
            break;
        }
        if out.len() + n > max {
            return Err(one(rule::LENGTH, format!("more than {max} bytes unpacked; stopped reading")));
        }
        out.extend_from_slice(&buf[..n]);
    }
    let rest = dec.into_inner();
    if !rest.is_empty() {
        return Err(one(rule::FORMAT, format!("{} bytes after the packed data", rest.len())));
    }
    Ok(out)
}

pub fn parse_scams(gz: &[u8]) -> Result<ScamBundle, Vec<Issue>> {
    let raw = unpack_scams_bytes(gz, MAX_SCAMS_BYTES)?;
    let one = |r: &'static str, f: &str, d: String| vec![Issue::new(SCAMS_FILE, f, r, d)];
    let val: Value = serde_json::from_slice(&raw).map_err(|e| one(rule::FORMAT, "", e.to_string()))?;
    match val.get("format").and_then(Value::as_u64) {
        Some(n) if n > u64::from(SCAMS_FORMAT) => {
            return Err(one(rule::FORMAT, "format", format!("format {n} is newer than this Bageo reads ({SCAMS_FORMAT}); update Bageo first")));
        }
        Some(n) if n == u64::from(SCAMS_FORMAT) => {}
        Some(n) => return Err(one(rule::FORMAT, "format", format!("format {n} is not {SCAMS_FORMAT}"))),
        None => return Err(one(rule::FORMAT, "format", "missing".into())),
    }
    let bundle: ScamBundle = serde_json::from_value(val).map_err(|e| one(rule::FORMAT, "", e.to_string()))?;
    if !valid_version(bundle.version, now_days()) {
        return Err(one(rule::STATUS, "version", format!("{} is not YYYYMMDDNN", bundle.version)));
    }
    if !valid_rfc3339_utc(&bundle.built_at) {
        return Err(one(rule::STATUS, "built_at", "must look like 2026-11-22T08:00:00Z".into()));
    }
    if !valid_commit(&bundle.source_commit) {
        return Err(one(rule::FORMAT, "source_commit", "invalid commit string".into()));
    }
    Ok(bundle)
}

pub fn check_scams_ref(r: &LocalesRef) -> Vec<Issue> {
    let mut out = Vec::new();
    let f = INDEX_FILE;
    if r.format != SCAMS_FORMAT {
        push(&mut out, f, "scams.format", rule::FORMAT, format!("format {} is not {SCAMS_FORMAT}", r.format));
    }
    if !valid_version(r.version, now_days()) {
        push(&mut out, f, "scams.version", rule::STATUS, format!("{} is not YYYYMMDDNN", r.version));
    }
    if !plain_file_name(&r.file) || !r.file.ends_with(".json.gz") {
        push(&mut out, f, "scams.file", rule::RELEASE_INDEX, "must be a plain .json.gz file name");
    }
    if r.bytes == 0 || r.bytes > MAX_SCAMS_BYTES as u64 {
        push(&mut out, f, "scams.bytes", rule::LENGTH, format!("{} bytes, must be 1 to {MAX_SCAMS_BYTES}", r.bytes));
    }
    if !is_sha256_hex(&r.sha256) {
        push(&mut out, f, "scams.sha256", rule::RELEASE_INDEX, "must be 64 lower-case hex digits");
    }
    if !valid_min_bageo(&r.min_bageo) {
        push(&mut out, f, "scams.min_bageo", rule::RELEASE_INDEX, "must look like 1.6.9");
    }
    out
}

pub fn sound_scams(ix: &Index) -> (Option<LocalesRef>, Option<Issue>) {
    let Some(s) = &ix.scams else { return (None, None) };
    let bad = check_scams_ref(s);
    if bad.is_empty() {
        return (Some(s.clone()), None);
    }
    let why = bad.iter().map(|i| format!("{}: {}", i.field, i.detail)).collect::<Vec<_>>().join("; ");
    let note = Issue::new(
        INDEX_FILE,
        "scams",
        rule::RELEASE_INDEX,
        format!("the scams entry is left out ({why}); {SCAMS_FILE} is not downloaded"),
    );
    (None, Some(note))
}

/// Counts of typologies, keyword groups, and countries inside a `ScamBundle`.
pub fn scam_counts(b: &ScamBundle) -> (usize, usize, usize) {
    let typ = b.db.get("typologies").and_then(Value::as_array).map_or(0, Vec::len);
    let kw = b.db.get("keywords").and_then(Value::as_array).map_or(0, Vec::len);
    let cc = b
        .countries
        .get("countries")
        .and_then(Value::as_object)
        .map_or(0, serde_json::Map::len);
    (typ, kw, cc)
}

#[allow(dead_code)]
fn _keep_btree(_: BTreeMap<String, String>) {}

// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The source files: one JSON per region and one shared layer per language,
//! read strictly (an unknown field is an error), merged, checked and
//! packed into a [Bundle].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::check::*;
use crate::data::*;
use crate::guard::Writing;
use crate::locales::{SrcNames, SrcSpeech};
use crate::model::*;
use crate::util::*;
use crate::{checker_id, rule, Issue, FORMAT, MAX_BUNDLE_BYTES};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcText {
    pub en: String,
    pub zh: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcName {
    pub en: String,
    pub native: String,
    #[serde(default)]
    pub zh: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcHello {
    pub hello: String,
    pub hello_again: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcDeliveryKey {
    #[serde(default)]
    pub en: Option<String>,
    #[serde(default)]
    pub zh: Option<String>,
}

/// A talk-style block in a source file: any of the levels "1".."5" for each
/// of the two voices. The keys are checked by [check_sources] (a region may
/// override single levels of its layer).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcTalkStyle {
    #[serde(default)]
    pub persona: BTreeMap<String, SrcText>,
    #[serde(default)]
    pub assistant: BTreeMap<String, SrcText>,
}

/// regions/<code>.json
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcRegion {
    pub schema: u32,
    pub code: String,
    pub countries: Vec<String>,
    pub name: SrcName,
    pub lang: String,
    #[serde(default)]
    pub layer: Option<String>,
    #[serde(default)]
    pub stt: Option<String>,
    #[serde(default)]
    pub other_langs: Vec<String>,
    #[serde(default)]
    pub chinese_dialects: bool,
    #[serde(default)]
    pub hello: Option<SrcHello>,
    #[serde(default)]
    pub delivery: Option<SrcText>,
    #[serde(default)]
    pub delivery_key: Option<SrcDeliveryKey>,
    #[serde(default)]
    pub readback: Option<SrcText>,
    #[serde(default)]
    pub fraud: Option<SrcText>,
    #[serde(default)]
    pub habits: Option<SrcText>,
    #[serde(default)]
    pub stall_local: Option<SrcText>,
    #[serde(default)]
    pub talk_style: Option<SrcTalkStyle>,
    #[serde(default)]
    pub vocab: BTreeMap<String, Vec<String>>,
    pub status: String,
    #[serde(default)]
    pub reviewed_on: Option<String>,
    /// Working notes; dropped when the bundle is built.
    #[serde(default)]
    pub notes: Option<String>,
    /// The research original; dropped when the bundle is built.
    #[serde(default)]
    pub src_native: Option<serde_json::Value>,
}

/// What a language layer shares with every region that uses it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcCommon {
    #[serde(default)]
    pub delivery: Option<SrcText>,
    #[serde(default)]
    pub readback: Option<SrcText>,
    #[serde(default)]
    pub fraud: Option<SrcText>,
    #[serde(default)]
    pub habits: Option<SrcText>,
    #[serde(default)]
    pub stall_local: Option<SrcText>,
    #[serde(default)]
    pub talk_style: Option<SrcTalkStyle>,
    #[serde(default)]
    pub vocab: BTreeMap<String, Vec<String>>,
}

/// languages/<layer>/call.json. Schema 1 needs a name; schema 2 may take it
/// from the folder's names.json and may carry the call speech of a language
/// outside the nine core ones (checked with the translations).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcLayer {
    pub schema: u32,
    pub layer: String,
    pub status: String,
    #[serde(default)]
    pub reviewed_on: Option<String>,
    #[serde(default)]
    pub name: Option<SrcText>,
    #[serde(default)]
    pub stt: Option<String>,
    #[serde(default)]
    pub hello: Option<SrcHello>,
    pub common: SrcCommon,
    #[serde(default)]
    pub speech: Option<SrcSpeech>,
}

impl From<&SrcText> for Text {
    fn from(t: &SrcText) -> Text {
        Text { en: t.en.clone(), zh: t.zh.clone() }
    }
}

/// A source file as read: its name, the hash of its bytes (for the manifest)
/// and what it says.
#[derive(Debug, Clone)]
pub struct SourceFile<T> {
    /// The file name without .json (regions), or the folder name (layers).
    pub id: String,
    /// Path inside the source folder, as shown in issues.
    pub file: String,
    pub sha256: String,
    pub data: T,
}

#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub regions: Vec<SourceFile<SrcRegion>>,
    pub layers: Vec<SourceFile<SrcLayer>>,
    /// languages/<code>/names.json, where present.
    pub names: Vec<SourceFile<SrcNames>>,
    /// regions_multilang.json, where present.
    pub multilang: Vec<MultilangEntry>,
    /// bubbles.json, where present.
    pub bubbles: BTreeMap<String, serde_json::Value>,
    /// places/*.json, where present.
    pub place_habits: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SrcMultilangDoc {
    schema: u32,
    entries: Vec<MultilangEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SrcBubblesDoc {
    schema: u32,
    regions: BTreeMap<String, serde_json::Value>,
}

const MAX_SOURCE_BYTES: usize = 256 * 1024;
const MAX_AGGREGATE_SOURCE_BYTES: usize = 2 * 1024 * 1024;

pub(crate) fn sorted_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut v = fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<Vec<_>, _>>()?;
    v.sort();
    Ok(v)
}

fn read_json<T: DeserializeOwned>(path: &Path, file: &str, issues: &mut Vec<Issue>) -> Option<(String, T)> {
    read_json_limit(path, file, MAX_SOURCE_BYTES, issues)
}

fn read_json_limit<T: DeserializeOwned>(
    path: &Path,
    file: &str,
    max_bytes: usize,
    issues: &mut Vec<Issue>,
) -> Option<(String, T)> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            issues.push(Issue::new(file, "", rule::FORMAT, format!("cannot read: {e}")));
            return None;
        }
    };
    if bytes.len() > max_bytes {
        issues.push(Issue::new(
            file,
            "",
            rule::LENGTH,
            format!("{} bytes, the limit for {file} is {max_bytes}", bytes.len()),
        ));
        return None;
    }
    match serde_json::from_slice::<T>(&bytes) {
        Ok(v) => Some((sha256_hex(&bytes), v)),
        Err(e) => {
            let msg = e.to_string();
            let r = if msg.contains("unknown field") { rule::UNKNOWN_FIELD } else { rule::FORMAT };
            issues.push(Issue::new(file, "", r, msg));
            None
        }
    }
}

fn name_of(p: &Path) -> String {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string()
}

/// Reads regions/*.json, languages/*/call.json and languages/*/names.json
/// under dir (a language folder needs at least one of the two). Every file
/// that cannot be read or has a field the format does not define is reported.
pub fn load_sources(dir: &Path) -> Result<Sources, Vec<Issue>> {
    let mut issues = Vec::new();
    let mut s = Sources::default();

    match sorted_paths(&dir.join("regions")) {
        Err(e) => issues.push(Issue::new("regions", "", rule::FORMAT, format!("cannot read the folder: {e}"))),
        Ok(paths) => {
            for p in paths.iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json")) {
                let name = name_of(p);
                let file = format!("regions/{name}");
                if let Some((sha256, data)) = read_json::<SrcRegion>(p, &file, &mut issues) {
                    s.regions.push(SourceFile { id: name.trim_end_matches(".json").to_string(), file, sha256, data });
                }
            }
        }
    }

    if let Ok(paths) = sorted_paths(&dir.join("languages")) {
        for p in paths.iter().filter(|p| p.is_dir()) {
            let id = name_of(p);
            let call = p.join("call.json");
            let names = p.join("names.json");
            if !call.is_file() && !names.is_file() {
                issues.push(Issue::new(&format!("languages/{id}"), "", rule::FORMAT, "needs call.json or names.json"));
                continue;
            }
            if names.is_file() {
                let file = format!("languages/{id}/names.json");
                if let Some((sha256, data)) = read_json::<SrcNames>(&names, &file, &mut issues) {
                    s.names.push(SourceFile { id: id.clone(), file, sha256, data });
                }
            }
            if call.is_file() {
                let file = format!("languages/{id}/call.json");
                if let Some((sha256, data)) = read_json::<SrcLayer>(&call, &file, &mut issues) {
                    s.layers.push(SourceFile { id, file, sha256, data });
                }
            }
        }
    }

    let ml_path = dir.join("regions_multilang.json");
    if ml_path.is_file() {
        if let Some((_, doc)) = read_json_limit::<SrcMultilangDoc>(
            &ml_path,
            "regions_multilang.json",
            MAX_AGGREGATE_SOURCE_BYTES,
            &mut issues,
        ) {
            if doc.schema != 1 {
                issues.push(Issue::new("regions_multilang.json", "schema", rule::FORMAT, format!("schema {} is not 1", doc.schema)));
            } else {
                s.multilang = doc.entries;
            }
        }
    }

    let ml_dir = dir.join("multilang");
    if ml_dir.is_dir() {
        if let Ok(paths) = sorted_paths(&ml_dir) {
            for p in paths.iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json")) {
                let name = name_of(p);
                let file = format!("multilang/{name}");
                if let Some((_, doc)) = read_json::<SrcMultilangDoc>(p, &file, &mut issues) {
                    if doc.schema != 1 {
                        issues.push(Issue::new(&file, "schema", rule::FORMAT, format!("schema {} is not 1", doc.schema)));
                    } else {
                        s.multilang.extend(doc.entries);
                    }
                }
            }
        }
    }

    let bub_path = dir.join("bubbles.json");
    if bub_path.is_file() {
        if let Some((_, doc)) = read_json_limit::<SrcBubblesDoc>(
            &bub_path,
            "bubbles.json",
            MAX_AGGREGATE_SOURCE_BYTES,
            &mut issues,
        ) {
            if doc.schema != 1 {
                issues.push(Issue::new("bubbles.json", "schema", rule::FORMAT, format!("schema {} is not 1", doc.schema)));
            } else {
                s.bubbles = doc.regions;
            }
        }
    }

    let places_dir = dir.join("places");
    if places_dir.is_dir() {
        if let Ok(paths) = sorted_paths(&places_dir) {
            for p in paths.iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json")) {
                let name = name_of(p);
                let file = format!("places/{name}");
                let stem = name.trim_end_matches(".json");
                if let Some((_, val)) = read_json::<serde_json::Value>(p, &file, &mut issues) {
                    if val.get("region").and_then(serde_json::Value::as_str) != Some(stem) {
                        issues.push(Issue::new(&file, "region", rule::CODE, format!("must match file name \"{stem}\"")));
                    } else {
                        s.place_habits.push(val);
                    }
                }
            }
        }
    }

    if issues.is_empty() {
        Ok(s)
    } else {
        Err(issues)
    }
}

// ---- file-level rules ---------------------------------------------------------------

fn conv(t: &SrcText) -> Text {
    Text::from(t)
}

fn talk_keys(out: &mut Vec<Issue>, file: &str, field: &str, ts: &SrcTalkStyle) {
    for (who, cells) in [("persona", &ts.persona), ("assistant", &ts.assistant)] {
        for k in cells.keys() {
            if !["1", "2", "3", "4", "5"].contains(&k.as_str()) {
                push(out, file, &format!("{field}.{who}"), rule::REQUIRED, format!("unexpected level \"{k}\" (only \"1\" to \"5\")"));
            }
        }
    }
}

fn check_region_file(out: &mut Vec<Issue>, r: &SourceFile<SrcRegion>) {
    let (f, d) = (r.file.as_str(), &r.data);
    if d.schema != 1 {
        push(out, f, "schema", rule::FORMAT, format!("schema {} is not 1", d.schema));
    }
    if r.id != d.code {
        push(out, f, "code", rule::CODE, format!("the code \"{}\" must equal the file name \"{}\"", d.code, r.id));
    }
    if let Some(l) = &d.layer {
        if !valid_code(l) {
            push(out, f, "layer", rule::CODE, format!("\"{l}\" is not a valid layer code"));
        }
    }
    if let Some(n) = &d.notes {
        if n.chars().count() > NOTES_MAX {
            push(out, f, "notes", rule::LENGTH, format!("more than {NOTES_MAX} characters"));
        }
    }
    if let Some(v) = &d.src_native {
        if v.to_string().chars().count() > NOTES_MAX {
            push(out, f, "src_native", rule::LENGTH, format!("more than {NOTES_MAX} characters"));
        }
    }
    if let Some(ts) = &d.talk_style {
        talk_keys(out, f, "talk_style", ts);
    }
    // The merged status can be lower than the region's own; the region's own claim needs its date regardless.
    if d.status == "local_reviewed" && d.reviewed_on.is_none() {
        push(out, f, "reviewed_on", rule::STATUS, "required when the status is local_reviewed");
    }
}

fn check_layer_file(out: &mut Vec<Issue>, l: &SourceFile<SrcLayer>, now: i64) {
    let (f, d) = (l.file.as_str(), &l.data);
    if d.schema != 1 && d.schema != 2 {
        push(out, f, "schema", rule::FORMAT, format!("schema {} is not 1 or 2", d.schema));
    }
    if !valid_code(&d.layer) {
        push(out, f, "layer", rule::CODE, format!("\"{}\" is not a valid layer code", d.layer));
    }
    if l.id != d.layer {
        push(out, f, "layer", rule::CODE, format!("the layer \"{}\" must equal its folder name \"{}\"", d.layer, l.id));
    }
    match &d.name {
        Some(n) => text_field(out, f, "name", &conv(n), (MAX_NAME, MAX_NAME), false),
        None if d.schema == 1 => push(out, f, "name", rule::REQUIRED, "required in schema 1"),
        None => {}
    }
    if let Some(s) = &d.stt {
        if !is_official_lang(s) {
            push(out, f, "stt", rule::LANG, format!("\"{s}\" is not on the official language list"));
        }
    }
    if let Some(h) = &d.hello {
        hello_fields(out, f, "hello", &Hello { hello: h.hello.clone(), hello_again: h.hello_again.clone() }, Writing::of(&d.layer));
    }
    if d.status == "local_reviewed" && d.reviewed_on.is_none() {
        push(out, f, "reviewed_on", rule::STATUS, "required when the status is local_reviewed");
    }
    if let Some(date) = &d.reviewed_on {
        date_field(out, f, "reviewed_on", date, now);
    }

    let c = &d.common;
    if let Some(t) = &c.delivery {
        text_field(out, f, "common.delivery", &conv(t), DELIVERY_MAX, false);
    }
    if let Some(t) = &c.readback {
        text_field(out, f, "common.readback", &conv(t), READBACK_MAX, false);
    }
    if let Some(t) = &c.fraud {
        text_field(out, f, "common.fraud", &conv(t), FRAUD_MAX, false);
        fraud_marker(out, f, "common.fraud", &conv(t));
    }
    if let Some(t) = &c.habits {
        text_field(out, f, "common.habits", &conv(t), LAYER_HABITS_MAX, false);
    }
    if let Some(t) = &c.stall_local {
        text_field(out, f, "common.stall_local", &conv(t), STALL_MAX, false);
    }
    if let Some(ts) = &c.talk_style {
        talk_keys(out, f, "common.talk_style", ts);
        for (who, cells) in [("persona", &ts.persona), ("assistant", &ts.assistant)] {
            for (k, t) in cells {
                text_field(out, f, &format!("common.talk_style.{who}.{k}"), &conv(t), LAYER_CELL_MAX, true);
            }
        }
    }
    vocab_fields(out, f, "common.vocab", &c.vocab, Writing::of(&d.layer));
}

// ---- merging --------------------------------------------------------------------------

fn join(first: Option<&SrcText>, then: Option<&SrcText>) -> Option<Text> {
    match (first, then) {
        (Some(a), Some(b)) => Some(Text {
            en: format!("{} {}", a.en.trim_end(), b.en.trim_start()),
            zh: format!("{}{}", a.zh, b.zh),
        }),
        (Some(a), None) | (None, Some(a)) => Some(conv(a)),
        (None, None) => None,
    }
}

fn cell<'a>(ts: Option<&'a SrcTalkStyle>, assistant: bool, key: &str) -> Option<&'a SrcText> {
    let ts = ts?;
    (if assistant { &ts.assistant } else { &ts.persona }).get(key)
}

/// The lower of two review states; an invalid one is passed on so the status check reports it.
fn lower_status(a: &str, b: Option<&str>) -> String {
    if !valid_status(a) {
        return a.to_string();
    }
    match b {
        Some(b) if !valid_status(b) => b.to_string(),
        Some("draft") => "draft".to_string(),
        _ => a.to_string(),
    }
}

fn merge_region(r: &SourceFile<SrcRegion>, layer: Option<&SourceFile<SrcLayer>>, out: &mut Vec<Issue>) -> Option<RegionData> {
    let d = &r.data;
    let c = layer.map(|l| &l.data.common);
    let mut missing: Vec<String> = Vec::new();

    let delivery = d.delivery.as_ref().or_else(|| c.and_then(|c| c.delivery.as_ref())).map(conv);
    let fraud = d.fraud.as_ref().or_else(|| c.and_then(|c| c.fraud.as_ref())).map(conv);
    let readback = d.readback.as_ref().or_else(|| c.and_then(|c| c.readback.as_ref())).map(conv);
    let habits = join(c.and_then(|c| c.habits.as_ref()), d.habits.as_ref());
    let stall_local = join(c.and_then(|c| c.stall_local.as_ref()), d.stall_local.as_ref());
    if delivery.is_none() {
        missing.push("delivery".into());
    }
    if fraud.is_none() {
        missing.push("fraud".into());
    }
    if habits.is_none() {
        missing.push("habits".into());
    }

    let mut sets: [Vec<Text>; 2] = [Vec::new(), Vec::new()];
    for (assistant, name) in [(false, "persona"), (true, "assistant")] {
        for n in 1..=5 {
            let key = n.to_string();
            let found = cell(d.talk_style.as_ref(), assistant, &key)
                .or_else(|| cell(c.and_then(|c| c.talk_style.as_ref()), assistant, &key));
            match found {
                Some(t) => sets[assistant as usize].push(conv(t)),
                None => missing.push(format!("talk_style.{name}.{n}")),
            }
        }
    }
    for field in &missing {
        push(out, &r.file, field, rule::REQUIRED, "missing in the region and in its language layer");
    }
    if !missing.is_empty() {
        return None;
    }

    let mut vocab = d.vocab.clone();
    if let Some(c) = c {
        for (k, words) in &c.vocab {
            let entry = vocab.entry(k.clone()).or_default();
            for w in words {
                if !entry.contains(w) {
                    entry.push(w.clone());
                }
            }
        }
    }

    let [persona, assistant] = sets;
    Some(RegionData {
        code: d.code.clone(),
        countries: d.countries.clone(),
        name: Name { en: d.name.en.clone(), native: d.name.native.clone(), zh: d.name.zh.clone() },
        lang: d.lang.clone(),
        stt: d.stt.clone().or_else(|| layer.and_then(|l| l.data.stt.clone())),
        other_langs: d.other_langs.clone(),
        chinese_dialects: d.chinese_dialects,
        hello: d
            .hello
            .as_ref()
            .or_else(|| layer.and_then(|l| l.data.hello.as_ref()))
            .map(|h| Hello { hello: h.hello.clone(), hello_again: h.hello_again.clone() }),
        delivery: delivery?,
        delivery_key: d.delivery_key.as_ref().map(|k| DeliveryKey { en: k.en.clone(), zh: k.zh.clone() }),
        readback,
        fraud: fraud?,
        habits: habits?,
        stall_local,
        talk_style: TalkStyle { persona: persona.try_into().ok()?, assistant: assistant.try_into().ok()? },
        vocab,
        status: lower_status(&d.status, layer.map(|l| l.data.status.as_str())),
        reviewed_on: d.reviewed_on.clone(),
    })
}

fn pipeline(s: &Sources, version: u64, commit: &str, built_at: &str) -> (Bundle, Vec<Issue>) {
    let mut issues = Vec::new();
    let now = now_days();

    for l in &s.layers {
        check_layer_file(&mut issues, l, now);
    }
    let mut codes = BTreeSet::new();
    for r in &s.regions {
        check_region_file(&mut issues, r);
        if !codes.insert(r.data.code.as_str()) {
            push(&mut issues, &r.file, "code", rule::CODE, format!("the code \"{}\" is used by another file", r.data.code));
        }
    }

    let mut manifest = Vec::new();
    let mut languages = BTreeMap::new();
    for l in &s.layers {
        let name = match (&l.data.name, s.names.iter().find(|n| n.id == l.id)) {
            (Some(n), _) => conv(n),
            (None, Some(n)) => Text { en: n.data.en.clone(), zh: n.data.zh.clone() },
            (None, None) => {
                if l.data.schema != 1 {
                    push(&mut issues, &l.file, "name", rule::REQUIRED, "missing here and the folder has no names.json");
                }
                Text { en: String::new(), zh: String::new() }
            }
        };
        let info = LangInfo { name, stt: l.data.stt.clone() };
        if languages.insert(l.data.layer.clone(), info).is_some() {
            push(&mut issues, &l.file, "layer", rule::CODE, format!("the layer \"{}\" is defined twice", l.data.layer));
        }
        manifest.push(Entry { kind: "layer".into(), id: l.data.layer.clone(), status: l.data.status.clone(), sha256: l.sha256.clone() });
    }

    let mut regions = BTreeMap::new();
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for r in &s.regions {
        let layer_id = r.data.layer.clone().unwrap_or_else(|| primary_subtag(&r.data.lang).to_string());
        let layer = s.layers.iter().find(|l| l.data.layer == layer_id);
        if r.data.layer.is_some() && layer.is_none() {
            push(&mut issues, &r.file, "layer", rule::CODE, format!("the layer \"{layer_id}\" does not exist"));
        }
        let Some(data) = merge_region(r, layer, &mut issues) else { continue };
        if regions.contains_key(&data.code) {
            continue;
        }
        for c in &data.countries {
            match owner.get(c) {
                Some(other) if *other != data.code => {
                    push(&mut issues, &r.file, "countries", rule::COUNTRY, format!("\"{c}\" already belongs to region {other}"));
                }
                Some(_) => {}
                None => {
                    owner.insert(c.clone(), data.code.clone());
                }
            }
        }
        manifest.push(Entry { kind: "region".into(), id: data.code.clone(), status: data.status.clone(), sha256: r.sha256.clone() });
        regions.insert(data.code.clone(), data);
    }

    let bundle = Bundle {
        format: FORMAT,
        version,
        built_at: built_at.to_string(),
        source_commit: commit.to_string(),
        checker: checker_id(),
        manifest,
        countries: owner,
        languages,
        regions,
        multilang: s.multilang.clone(),
        bubbles: s.bubbles.clone(),
        place_habits: s.place_habits.clone(),
    };
    issues.extend(check_bundle_impl(&bundle, true, now));
    (bundle, issues)
}

/// Every rule on a set of source files (what CI runs): the files themselves,
/// then each region merged with its layer.
pub fn check_sources(s: &Sources) -> Vec<Issue> {
    pipeline(s, today_version(), "0000000000000000000000000000000000000000", &now_rfc3339()).1
}

/// Merges, checks and packs the sources. version is YYYYMMDDNN; commit is the
/// source commit recorded in the bundle.
pub fn build(s: &Sources, version: u64, commit: &str) -> Result<Bundle, Vec<Issue>> {
    build_at(s, version, commit, &now_rfc3339())
}

/// Same, with the build time given (for repeatable output).
pub fn build_at(s: &Sources, version: u64, commit: &str, built_at: &str) -> Result<Bundle, Vec<Issue>> {
    let (bundle, issues) = pipeline(s, version, commit, built_at);
    if !issues.is_empty() {
        return Err(issues);
    }
    // The bundle must also fit the size a download is allowed to have.
    let size = serde_json::to_string(&bundle).map(|j| j.len()).unwrap_or(usize::MAX);
    if size > MAX_BUNDLE_BYTES {
        return Err(vec![Issue::new(
            "bageo-pack.json",
            "",
            rule::LENGTH,
            format!("{size} bytes, the limit is {MAX_BUNDLE_BYTES}"),
        )]);
    }
    Ok(bundle)
}

#[cfg(test)]
mod source_limit_tests {
    use super::*;

    #[test]
    fn suriname_is_recognised_and_large_multilang_or_shards_load() {
        assert!(crate::data::is_country("sr"), "Suriname ISO code sr must be valid");
        let dir = std::env::temp_dir().join(format!("bageo-packs-ml-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("regions")).unwrap();
        fs::create_dir_all(dir.join("multilang")).unwrap();

        // Build a > 256 KiB regions_multilang.json plus a shard in multilang/*.json.
        let cell = || Text { en: "casual".into(), zh: "自然".into() };
        let five_cells = || [cell(), cell(), cell(), cell(), cell()];
        let make_entry = |r: String| MultilangEntry {
            region: r,
            lang: "en".into(),
            hello: Hello { hello: "Hello?".into(), hello_again: "Hello?".into() },
            habits: Text { en: "x".repeat(420), zh: "習慣".into() },
            talk_style: TalkStyle { persona: five_cells(), assistant: five_cells() },
        };
        let entries: Vec<MultilangEntry> = (0..450).map(|i| make_entry(format!("r{i:03}"))).collect();
        let big = serde_json::to_vec(&serde_json::json!({ "schema": 1, "entries": entries })).unwrap();
        assert!(big.len() > MAX_SOURCE_BYTES, "test fixture must exceed 256 KiB: {}", big.len());
        fs::write(dir.join("regions_multilang.json"), &big).unwrap();
        let shard_entry = make_entry("sr".into());
        fs::write(
            dir.join("multilang").join("01-extra.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "entries": [shard_entry]
            }))
            .unwrap(),
        )
        .unwrap();

        let loaded = load_sources(&dir).expect("large regions_multilang.json + shard should load");
        assert_eq!(loaded.multilang.len(), 451);
        let _ = fs::remove_dir_all(&dir);
    }
}

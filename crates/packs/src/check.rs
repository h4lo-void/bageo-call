// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The rules. Every limit is checked on the merged region (what the model
//! would read), by the same code in CI and inside Bageo.

use std::collections::{BTreeMap, BTreeSet};

use crate::data::*;
use crate::model::*;
use crate::util::*;
use crate::{rule, Issue, FORMAT, MAX_BUNDLE_BYTES};

/// (English, Chinese) character limits.
pub(crate) type Pair = (usize, usize);

pub(crate) const BUNDLE_FILE: &str = "bageo-pack.json";
pub(crate) const MAX_CODE: usize = 24;
pub(crate) const MAX_COUNTRIES: usize = 30;
pub(crate) const MAX_NAME: usize = 40;
pub(crate) const MAX_KEY: usize = 12;
pub(crate) const HELLO_MAX: Pair = (30, 60);
pub(crate) const DELIVERY_MAX: Pair = (80, 36);
pub(crate) const READBACK_MAX: Pair = (80, 36);
pub(crate) const FRAUD_MAX: Pair = (110, 50);
pub(crate) const HABITS_MAX: Pair = (420, 190);
pub(crate) const STALL_MAX: Pair = (150, 70);
pub(crate) const CELL_MAX: Pair = (200, 90);
pub(crate) const SUM_MAX: Pair = (560, 250);
pub(crate) const LAYER_HABITS_MAX: Pair = (200, 90);
pub(crate) const LAYER_CELL_MAX: Pair = (120, 55);
pub(crate) const NOTES_MAX: usize = 1000;
const MAX_OTHER_LANGS: usize = 4;
const MAX_VOCAB_SITUATIONS: usize = 8;
const MAX_VOCAB_WORDS: usize = 6;
const MAX_VOCAB_WORD_CHARS: usize = 24;
const MAX_REGIONS: usize = 400;
const MAX_LANGUAGES: usize = 200;
const MAX_MANIFEST: usize = 800;
const MAX_COUNTRY_INDEX: usize = 400;

pub(crate) fn push(out: &mut Vec<Issue>, file: &str, field: &str, rule: &'static str, detail: impl Into<String>) {
    out.push(Issue::new(file, field, rule, detail));
}

/// [a-z]{2,}(-[a-z]{2,})*, at most 24 characters.
pub(crate) fn valid_code(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_CODE && s.split('-').all(|p| p.len() >= 2 && p.bytes().all(|b| b.is_ascii_lowercase()))
}

pub(crate) fn valid_status(s: &str) -> bool {
    s == "draft" || s == "local_reviewed"
}

pub(crate) fn primary_subtag(lang: &str) -> &str {
    lang.split('-').next().unwrap_or("")
}

// ---- text rules -------------------------------------------------------------

/// Characters that hide text or break a line: control characters (line
/// breaks and tabs too), zero-width, bidirectional and tag characters.
fn bad_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{061C}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E007F}'
        )
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3040}'..='\u{30FF}' | '\u{3400}'..='\u{9FFF}' | '\u{AC00}'..='\u{D7AF}')
}

/// The text as phrase checks see it: folded, lower case, runs of white space
/// squeezed to one space; and the same without any space.
fn match_forms(s: &str) -> (String, String) {
    let folded = fold(s).to_lowercase();
    let mut spaced = String::with_capacity(folded.len());
    let mut last_space = false;
    for c in folded.chars() {
        if c.is_whitespace() {
            if !last_space {
                spaced.push(' ');
            }
            last_space = true;
        } else {
            spaced.push(c);
            last_space = false;
        }
    }
    let tight = spaced.chars().filter(|c| *c != ' ').collect();
    (spaced, tight)
}

fn find_phrase<'a>(words: &'a [&'a str], spaced: &str, tight: &str) -> Option<&'a str> {
    for w in words {
        let lw = w.to_lowercase();
        if spaced.contains(&lw) {
            return Some(w);
        }
        if lw.chars().any(is_cjk) {
            let lt: String = lw.chars().filter(|c| *c != ' ').collect();
            if tight.contains(&lt) {
                return Some(w);
            }
        }
    }
    None
}

fn has_domain(low: &str) -> bool {
    let chars: Vec<char> = low.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c != '.' || i == 0 || !chars[i - 1].is_alphanumeric() {
            continue;
        }
        let tld: String = chars[i + 1..].iter().take_while(|c| c.is_ascii_lowercase()).collect();
        let after = chars.get(i + 1 + tld.len());
        if URL_TLDS.contains(&tld.as_str()) && !after.is_some_and(|c| c.is_alphanumeric()) {
            return true;
        }
    }
    false
}

fn placeholders(out: &mut Vec<Issue>, file: &str, field: &str, folded: &str, allow_owner: bool) {
    let chars: Vec<char> = folded.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '{' => match chars[i + 1..].iter().position(|&c| c == '}') {
                Some(j) => {
                    let token: String = chars[i + 1..i + 1 + j].iter().collect();
                    if !(allow_owner && token == "owner") {
                        let why = if token == "owner" { "is not allowed here" } else { "is not an allowed placeholder" };
                        push(out, file, field, rule::PLACEHOLDER, format!("{{{token}}} {why}"));
                    }
                    i += j + 2;
                    continue;
                }
                None => push(out, file, field, rule::PLACEHOLDER, "unclosed {"),
            },
            '}' => push(out, file, field, rule::PLACEHOLDER, "stray }"),
            _ => {}
        }
        i += 1;
    }
}

/// Everything that is true of any text a pack carries.
pub(crate) fn content(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, allow_owner: bool) {
    if let Some(c) = s.chars().find(|&c| bad_char(c)) {
        push(out, file, field, rule::CHARSET, format!("contains U+{:04X} (line breaks, control, zero-width and bidirectional characters are not allowed)", c as u32));
    }
    let folded = fold(s);
    if let Some(c) = folded.chars().find(|c| matches!(c, '[' | ']' | '【' | '】' | '<' | '>' | '&')) {
        push(out, file, field, rule::CHARSET, format!("contains '{c}'"));
    }
    placeholders(out, file, field, &folded, allow_owner);

    let low = folded.to_lowercase();
    if low.contains("://") || low.contains("www.") || has_domain(&low) {
        push(out, file, field, rule::PATTERN, "contains a web address");
    }
    if low.contains('@') {
        push(out, file, field, rule::PATTERN, "contains an e-mail address or @handle");
    }
    let mut run = 0;
    for c in folded.chars() {
        if c.is_numeric() {
            run += 1;
            if run >= 6 {
                push(out, file, field, rule::PATTERN, "contains six or more digits in a row");
                break;
            }
        } else {
            run = 0;
        }
    }

    let (spaced, tight) = match_forms(s);
    if let Some(w) = find_phrase(BANNED, &spaced, &tight) {
        push(out, file, field, rule::BANNED, format!("contains the banned phrase \"{w}\""));
    }
    if let Some(w) = find_phrase(&UNSUPPORTED_OR_LIMITING, &spaced, &tight).or_else(|| find_phrase(ARABIC_ACCENTS, &spaced, &tight)) {
        push(out, file, field, rule::UNSUPPORTED, format!("names a language or accent Gemini Live does not speak, or pins the call to one language: \"{w}\""));
    }
}

/// A required, non-empty text with a length limit.
pub(crate) fn text_str(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, max: usize, allow_owner: bool) {
    if s.trim().is_empty() {
        push(out, file, field, rule::REQUIRED, "empty");
        return;
    }
    let n = s.chars().count();
    if n > max {
        push(out, file, field, rule::LENGTH, format!("{n} characters, the limit is {max}"));
    }
    content(out, file, field, s, allow_owner);
}

pub(crate) fn text_field(out: &mut Vec<Issue>, file: &str, field: &str, t: &Text, lim: Pair, allow_owner: bool) {
    text_str(out, file, &format!("{field}.en"), &t.en, lim.0, allow_owner);
    text_str(out, file, &format!("{field}.zh"), &t.zh, lim.1, allow_owner);
}

/// A fraud line also has to say it is a scam.
pub(crate) fn fraud_marker(out: &mut Vec<Issue>, file: &str, field: &str, t: &Text) {
    if !fold(&t.en).to_lowercase().contains("scam") {
        push(out, file, &format!("{field}.en"), rule::REQUIRED, "must contain \"scam\"");
    }
    if !t.zh.contains("詐騙") {
        push(out, file, &format!("{field}.zh"), rule::REQUIRED, "must contain \"詐騙\"");
    }
}

pub(crate) fn hello_fields(out: &mut Vec<Issue>, file: &str, field: &str, h: &Hello) {
    text_str(out, file, &format!("{field}.hello"), &h.hello, HELLO_MAX.0, false);
    text_str(out, file, &format!("{field}.hello_again"), &h.hello_again, HELLO_MAX.1, false);
}

pub(crate) fn vocab_fields(out: &mut Vec<Issue>, file: &str, field: &str, v: &BTreeMap<String, Vec<String>>) {
    if v.len() > MAX_VOCAB_SITUATIONS {
        push(out, file, field, rule::LENGTH, format!("{} situations, the limit is {MAX_VOCAB_SITUATIONS}", v.len()));
    }
    for (name, words) in v {
        let f = format!("{field}.{name}");
        if !(2..=16).contains(&name.len()) || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            push(out, file, &f, rule::LENGTH, "a situation name must match [a-z_]{2,16}");
        }
        if words.len() > MAX_VOCAB_WORDS {
            push(out, file, &f, rule::LENGTH, format!("{} words, the limit is {MAX_VOCAB_WORDS}", words.len()));
        }
        for (i, w) in words.iter().enumerate() {
            text_str(out, file, &format!("{f}[{i}]"), w, MAX_VOCAB_WORD_CHARS, false);
        }
    }
}

pub(crate) fn date_field(out: &mut Vec<Issue>, file: &str, field: &str, s: &str, now: i64) {
    match parse_date(s) {
        None => push(out, file, field, rule::STATUS, format!("\"{s}\" is not a date (YYYY-MM-DD)")),
        Some((y, m, d)) if days_from_civil(y, m, d) > now + 1 => push(out, file, field, rule::STATUS, "is in the future"),
        Some(_) => {}
    }
}

pub(crate) fn version_field(out: &mut Vec<Issue>, file: &str, field: &str, v: u64, now: i64) {
    if !(1_000_000_000..10_000_000_000).contains(&v) {
        push(out, file, field, rule::STATUS, format!("{v} is not YYYYMMDDNN"));
        return;
    }
    let (y, m, d) = ((v / 1_000_000) as i64, (v / 10_000 % 100) as u32, (v / 100 % 100) as u32);
    if !valid_date(y, m, d) {
        push(out, file, field, rule::STATUS, format!("{v} does not start with a real date"));
    } else if days_from_civil(y, m, d) > now + 1 {
        push(out, file, field, rule::STATUS, format!("{v} is later than tomorrow"));
    }
}

// ---- one region ---------------------------------------------------------------

/// Whether a region has to bring its own hello: its language is not one of
/// the nine, or one of its countries is not in the agent's country table
/// (English is exempt: those regions open with the English hello).
pub(crate) fn hello_required(r: &RegionData) -> bool {
    let primary = primary_subtag(&r.lang);
    if !NINE_LANG_PRIMARY.contains(&primary) {
        return true;
    }
    primary != "en" && r.countries.iter().any(|c| !NINE_LANG_COUNTRIES.contains(&c.as_str()))
}

pub(crate) fn check_region(out: &mut Vec<Issue>, file: &str, p: &str, r: &RegionData, now: i64) {
    let fld = |n: &str| format!("{p}{n}");
    if !valid_code(&r.code) {
        push(out, file, &fld("code"), rule::CODE, format!("\"{}\" must match [a-z]{{2,}}(-[a-z]{{2,}})* and be at most {MAX_CODE} characters", r.code));
    }

    if r.countries.len() > MAX_COUNTRIES {
        push(out, file, &fld("countries"), rule::LENGTH, format!("{} countries, the limit is {MAX_COUNTRIES}", r.countries.len()));
    }
    let mut seen = BTreeSet::new();
    for c in &r.countries {
        if !is_country(c) {
            push(out, file, &fld("countries"), rule::COUNTRY, format!("\"{c}\" is not a lower-case ISO 3166 country code"));
        } else if !seen.insert(c.as_str()) {
            push(out, file, &fld("countries"), rule::COUNTRY, format!("\"{c}\" is listed twice"));
        }
    }

    text_str(out, file, &fld("name.en"), &r.name.en, MAX_NAME, false);
    text_str(out, file, &fld("name.native"), &r.name.native, MAX_NAME, false);
    if let Some(z) = &r.name.zh {
        text_str(out, file, &fld("name.zh"), z, MAX_NAME, false);
    }

    if !is_official_lang(&r.lang) {
        push(out, file, &fld("lang"), rule::LANG, format!("\"{}\" is not on the official language list", r.lang));
    }
    if let Some(s) = &r.stt {
        if !is_official_lang(s) {
            push(out, file, &fld("stt"), rule::LANG, format!("\"{s}\" is not on the official language list"));
        }
    }
    if r.other_langs.len() > MAX_OTHER_LANGS {
        push(out, file, &fld("other_langs"), rule::LENGTH, format!("{} languages, the limit is {MAX_OTHER_LANGS}", r.other_langs.len()));
    }
    for l in &r.other_langs {
        if !is_official_lang(l) {
            push(out, file, &fld("other_langs"), rule::LANG, format!("\"{l}\" is not on the official language list"));
        }
    }
    if r.chinese_dialects && (r.countries.is_empty() || r.countries.iter().any(|c| !DIALECT_COUNTRIES.contains(&c.as_str()))) {
        push(out, file, &fld("chinese_dialects"), rule::DIALECTS, "only regions whose countries are all in tw, hk, mo, cn, sg, my may set it");
    }

    match &r.hello {
        Some(h) => hello_fields(out, file, &fld("hello"), h),
        None if hello_required(r) => push(out, file, &fld("hello"), rule::REQUIRED, "this region's language needs its own hello"),
        None => {}
    }

    text_field(out, file, &fld("delivery"), &r.delivery, DELIVERY_MAX, false);
    if let Some(k) = &r.delivery_key {
        for (l, v) in [("en", &k.en), ("zh", &k.zh)] {
            if let Some(v) = v {
                text_str(out, file, &fld(&format!("delivery_key.{l}")), v, MAX_KEY, false);
            }
        }
    }
    if let Some(t) = &r.readback {
        text_field(out, file, &fld("readback"), t, READBACK_MAX, false);
    }
    text_field(out, file, &fld("fraud"), &r.fraud, FRAUD_MAX, false);
    fraud_marker(out, file, &fld("fraud"), &r.fraud);
    text_field(out, file, &fld("habits"), &r.habits, HABITS_MAX, false);
    if let Some(t) = &r.stall_local {
        text_field(out, file, &fld("stall_local"), t, STALL_MAX, false);
    }
    for (who, cells) in [("persona", &r.talk_style.persona), ("assistant", &r.talk_style.assistant)] {
        for (i, t) in cells.iter().enumerate() {
            text_field(out, file, &fld(&format!("talk_style.{who}.{}", i + 1)), t, CELL_MAX, true);
        }
    }

    let parts = [Some(&r.delivery), r.readback.as_ref(), Some(&r.fraud), Some(&r.habits)];
    let en: usize = parts.iter().flatten().map(|t| t.en.chars().count()).sum();
    let zh: usize = parts.iter().flatten().map(|t| t.zh.chars().count()).sum();
    if en > SUM_MAX.0 {
        push(out, file, &fld("total.en"), rule::LENGTH, format!("delivery + readback + fraud + habits are {en} characters, the limit is {}", SUM_MAX.0));
    }
    if zh > SUM_MAX.1 {
        push(out, file, &fld("total.zh"), rule::LENGTH, format!("delivery + readback + fraud + habits are {zh} characters, the limit is {}", SUM_MAX.1));
    }

    vocab_fields(out, file, &fld("vocab"), &r.vocab);

    if !valid_status(&r.status) {
        push(out, file, &fld("status"), rule::STATUS, format!("\"{}\" must be draft or local_reviewed", r.status));
    } else if r.status == "local_reviewed" && r.reviewed_on.is_none() {
        push(out, file, &fld("reviewed_on"), rule::STATUS, "required when the status is local_reviewed");
    }
    if let Some(d) = &r.reviewed_on {
        date_field(out, file, &fld("reviewed_on"), d, now);
    }
}

// ---- the bundle -----------------------------------------------------------------

fn valid_commit(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// sources_style names a region's issues after its source file
/// (regions/<code>.json) and leaves the field path short; a bundle's issues
/// are all in bageo-pack.json with the full path (regions.<code>.<field>).
pub(crate) fn check_bundle_impl(b: &Bundle, sources_style: bool, now: i64) -> Vec<Issue> {
    let mut out = Vec::new();
    let hf = BUNDLE_FILE;

    if b.format != FORMAT {
        push(&mut out, hf, "format", rule::FORMAT, format!("format {} is not {FORMAT}", b.format));
    }
    version_field(&mut out, hf, "version", b.version, now);
    if !valid_rfc3339_utc(&b.built_at) {
        push(&mut out, hf, "built_at", rule::STATUS, "must look like 2026-11-20T08:00:00Z");
    }
    if !valid_commit(&b.source_commit) {
        push(&mut out, hf, "source_commit", rule::FORMAT, "must be 1 to 64 letters, digits, '.', '_' or '-'");
    }
    if b.checker.is_empty() || b.checker.len() > 64 || !b.checker.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
        push(&mut out, hf, "checker", rule::FORMAT, "must be 1 to 64 printable ASCII characters");
    }
    for (name, n, max) in [
        ("regions", b.regions.len(), MAX_REGIONS),
        ("languages", b.languages.len(), MAX_LANGUAGES),
        ("manifest", b.manifest.len(), MAX_MANIFEST),
        ("countries", b.countries.len(), MAX_COUNTRY_INDEX),
    ] {
        if n > max {
            push(&mut out, hf, name, rule::LENGTH, format!("{n} entries, the limit is {max}"));
        }
    }

    for (id, info) in &b.languages {
        if !valid_code(id) {
            push(&mut out, hf, &format!("languages.{id}"), rule::CODE, "a language id must match [a-z]{2,}(-[a-z]{2,})*");
        }
        text_field(&mut out, hf, &format!("languages.{id}.name"), &info.name, (MAX_NAME, MAX_NAME), false);
        if let Some(s) = &info.stt {
            if !is_official_lang(s) {
                push(&mut out, hf, &format!("languages.{id}.stt"), rule::LANG, format!("\"{s}\" is not on the official language list"));
            }
        }
    }

    // Manifest: one entry per region and per language, nothing else.
    let mut seen = BTreeSet::new();
    let mut region_entry: BTreeMap<&str, &Entry> = BTreeMap::new();
    let mut layer_entry: BTreeSet<&str> = BTreeSet::new();
    for (i, e) in b.manifest.iter().enumerate() {
        let f = format!("manifest[{i}]");
        if e.kind != "region" && e.kind != "layer" {
            push(&mut out, hf, &f, rule::INDEX, format!("kind \"{}\" must be region or layer", e.kind));
        }
        if !valid_code(&e.id) {
            push(&mut out, hf, &f, rule::INDEX, format!("id \"{}\" is not a valid code", e.id));
        }
        if !valid_status(&e.status) {
            push(&mut out, hf, &f, rule::STATUS, format!("status \"{}\" must be draft or local_reviewed", e.status));
        }
        if !is_sha256_hex(&e.sha256) {
            push(&mut out, hf, &f, rule::INDEX, "sha256 must be 64 lower-case hex digits");
        }
        if !seen.insert((e.kind.as_str(), e.id.as_str())) {
            push(&mut out, hf, &f, rule::INDEX, format!("{} {} is listed twice", e.kind, e.id));
        }
        match e.kind.as_str() {
            "region" => {
                region_entry.insert(e.id.as_str(), e);
            }
            "layer" => {
                layer_entry.insert(e.id.as_str());
            }
            _ => {}
        }
    }
    for (code, r) in &b.regions {
        match region_entry.get(code.as_str()) {
            None => push(&mut out, hf, "manifest", rule::INDEX, format!("region {code} has no manifest entry")),
            Some(e) if e.status != r.status => {
                push(&mut out, hf, "manifest", rule::INDEX, format!("region {code}: the manifest says {} but the region says {}", e.status, r.status))
            }
            Some(_) => {}
        }
    }
    for id in region_entry.keys() {
        if !b.regions.contains_key(*id) {
            push(&mut out, hf, "manifest", rule::INDEX, format!("manifest lists region {id}, which is not in the bundle"));
        }
    }
    for id in b.languages.keys() {
        if !layer_entry.contains(id.as_str()) {
            push(&mut out, hf, "manifest", rule::INDEX, format!("language {id} has no manifest entry"));
        }
    }
    for id in &layer_entry {
        if !b.languages.contains_key(*id) {
            push(&mut out, hf, "manifest", rule::INDEX, format!("manifest lists language {id}, which is not in the bundle"));
        }
    }

    // Countries index: exactly what the regions say.
    for (c, code) in &b.countries {
        if !is_country(c) {
            push(&mut out, hf, &format!("countries.{c}"), rule::INDEX, "not a country code");
        }
        match b.regions.get(code) {
            None => push(&mut out, hf, &format!("countries.{c}"), rule::INDEX, format!("points to region {code}, which is not in the bundle")),
            Some(r) if !r.countries.contains(c) => {
                push(&mut out, hf, &format!("countries.{c}"), rule::INDEX, format!("region {code} does not list this country"))
            }
            Some(_) => {}
        }
    }
    let mut claimed: BTreeMap<&str, &str> = BTreeMap::new();
    for r in b.regions.values() {
        for c in &r.countries {
            let prev = claimed.insert(c.as_str(), r.code.as_str());
            match prev {
                Some(other) if other != r.code => {
                    push(&mut out, hf, &format!("countries.{c}"), rule::COUNTRY, format!("belongs to both region {other} and region {}", r.code));
                }
                _ => match b.countries.get(c) {
                    Some(x) if *x == r.code => {}
                    Some(x) => push(&mut out, hf, &format!("countries.{c}"), rule::INDEX, format!("the index says {x}, region {} lists it", r.code)),
                    None => push(&mut out, hf, &format!("countries.{c}"), rule::INDEX, format!("region {} lists it but the index does not", r.code)),
                },
            }
        }
    }

    for (key, r) in &b.regions {
        if *key != r.code {
            push(&mut out, hf, &format!("regions.{key}.code"), rule::CODE, format!("the key {key} and the code {} differ", r.code));
        }
        let (file, prefix) = if sources_style {
            (format!("regions/{}.json", r.code), String::new())
        } else {
            (hf.to_string(), format!("regions.{key}."))
        };
        check_region(&mut out, &file, &prefix, r, now);
    }
    out
}

/// Every rule on a bundle, with today's date from the system clock.
pub fn check_bundle(b: &Bundle) -> Vec<Issue> {
    check_bundle_at(b, now_days())
}

/// Same, with today given as days since 1970-01-01 (for tests).
pub fn check_bundle_at(b: &Bundle, now: i64) -> Vec<Issue> {
    check_bundle_impl(b, false, now)
}

/// Reads a bundle's JSON: size, format number, shape, then every rule.
/// A bundle whose format is newer than this build is refused with a plain
/// message (the console tells the owner to update Bageo).
pub fn parse_bundle(json: &str) -> Result<Bundle, Vec<Issue>> {
    let one = |rule: &'static str, field: &str, detail: String| vec![Issue::new(BUNDLE_FILE, field, rule, detail)];
    if json.len() > MAX_BUNDLE_BYTES {
        return Err(one(rule::LENGTH, "", format!("{} bytes, the limit is {MAX_BUNDLE_BYTES}", json.len())));
    }
    let value: serde_json::Value = serde_json::from_str(json).map_err(|e| one(rule::FORMAT, "", e.to_string()))?;
    match value.get("format").and_then(|f| f.as_u64()) {
        Some(n) if n > FORMAT as u64 => {
            return Err(one(rule::FORMAT, "format", format!("format {n} is newer than this Bageo reads ({FORMAT}); update Bageo first")))
        }
        Some(_) => {}
        None => return Err(one(rule::FORMAT, "format", "missing".to_string())),
    }
    let bundle: Bundle = serde_json::from_value(value).map_err(|e| one(rule::FORMAT, "", e.to_string()))?;
    let issues = check_bundle(&bundle);
    if issues.is_empty() {
        Ok(bundle)
    } else {
        Err(issues)
    }
}

// ---- the release index ------------------------------------------------------------

fn valid_min_bageo(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
}

pub fn check_index(ix: &Index) -> Vec<Issue> {
    let f = "bageo-pack-index.json";
    let mut out = Vec::new();
    if ix.format != FORMAT {
        push(&mut out, f, "format", rule::FORMAT, format!("format {} is not {FORMAT}", ix.format));
    }
    version_field(&mut out, f, "version", ix.version, now_days());
    let plain_name = !ix.file.is_empty()
        && ix.file.len() <= 64
        && !ix.file.starts_with('.')
        && ix.file.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !plain_name {
        push(&mut out, f, "file", rule::RELEASE_INDEX, "must be a plain file name (letters, digits, '.', '_', '-'), not a path");
    }
    if ix.bytes == 0 || ix.bytes > MAX_BUNDLE_BYTES as u64 {
        push(&mut out, f, "bytes", rule::LENGTH, format!("{} bytes, must be 1 to {MAX_BUNDLE_BYTES}", ix.bytes));
    }
    if !is_sha256_hex(&ix.sha256) {
        push(&mut out, f, "sha256", rule::RELEASE_INDEX, "must be 64 lower-case hex digits");
    }
    if !valid_min_bageo(&ix.min_bageo) {
        push(&mut out, f, "min_bageo", rule::RELEASE_INDEX, "must look like 1.6.9");
    }
    out
}

/// Reads an index (tolerant of unknown fields) and checks it.
pub fn parse_index(json: &str) -> Result<Index, Vec<Issue>> {
    let f = "bageo-pack-index.json";
    if json.len() > 4096 {
        return Err(vec![Issue::new(f, "", rule::LENGTH, "an index is a few hundred bytes")]);
    }
    let ix: Index = serde_json::from_str(json).map_err(|e| vec![Issue::new(f, "", rule::FORMAT, e.to_string())])?;
    let issues = check_index(&ix);
    if issues.is_empty() {
        Ok(ix)
    } else {
        Err(issues)
    }
}

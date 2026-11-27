// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Extra packs: the index, the language files and the art files. The good
//! fixture (tests/fixtures/extras: it, th, ar, ru, hi and the regions it, th,
//! whose regions/*.json hold only the main language) builds and reads back;
//! every bad sample in tests/fixtures/extras_bad is refused with its rule
//! code. All sample text is invented.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use bageo_packs::extras::{arule, art_file_name, lang_file_name, xrule, MAX_ART_BYTES, MAX_INDEX_BYTES, MAX_LANG_ART};
use bageo_packs::locales::lrule;
use bageo_packs::*;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde_json::{json, Value};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const BUILT: &str = "2026-01-01T00:00:00Z";
const VERSION: u64 = 2026010101;
const LANGS: [&str; 5] = ["ar", "hi", "it", "ru", "th"];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn sources() -> LocaleSources {
    load_locales(&fixture("extras")).expect("the extras fixture loads")
}

fn art_sources() -> BTreeMap<String, Vec<u8>> {
    load_art(&fixture("extras")).expect("the art fixture loads")
}

fn region_langs() -> BTreeMap<String, String> {
    let (langs, issues) = load_region_langs(&fixture("extras"));
    assert!(issues.is_empty(), "{}", show(&issues));
    langs
}

fn built() -> ExtrasBuild {
    build_extras(&sources(), &art_sources(), &region_langs(), VERSION, COMMIT, BUILT, "1.6.9").expect("the extras fixture builds")
}

/// The fixture's drawings plus the Italian one again under other regions,
/// each with its region's main language.
fn more_art(extra: &[(&str, &str)]) -> (BTreeMap<String, Vec<u8>>, BTreeMap<String, String>) {
    let (mut art, mut langs) = (art_sources(), region_langs());
    for (region, lang) in extra {
        let mut v: Value = serde_json::from_slice(&art_sources()["it"]).unwrap();
        v["region"] = json!(region);
        art.insert(region.to_string(), serde_json::to_vec(&v).unwrap());
        langs.insert(region.to_string(), lang.to_string());
    }
    (art, langs)
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(bytes).unwrap();
    enc.finish().unwrap()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bageo-extras-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn show(issues: &[Issue]) -> String {
    issues.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("\n")
}

fn expect(issues: &[Issue], rule: &str, field: &str) {
    assert!(issues.iter().any(|i| i.rule == rule && i.field.starts_with(field)), "wanted {rule} at {field}, got:\n{}", show(issues));
}

fn unpacked(gz: &[u8]) -> Value {
    serde_json::from_slice(&unpack_locales(gz, 4 * 1024 * 1024).unwrap()).unwrap()
}

// ---- building ------------------------------------------------------------------------------

#[test]
fn the_good_fixture_passes_the_translation_rules() {
    let issues = check_locales(&sources(), None);
    assert!(issues.is_empty(), "{}", show(&issues));
}

#[test]
fn every_language_with_text_gets_a_file_and_every_region_an_art_file() {
    let b = built();
    let codes: Vec<&str> = b.index.langs.iter().map(|l| l.code.as_str()).collect();
    assert_eq!(codes, LANGS);
    let regions: Vec<&str> = b.index.art.iter().map(|a| a.region.as_str()).collect();
    assert_eq!(regions, ["it", "th"]);
    assert_eq!(b.files.len(), 7);
    for l in &b.index.langs {
        let body = &b.files[&l.file];
        assert_eq!(l.file, lang_file_name(&l.code));
        assert_eq!(l.bytes, body.len() as u64);
        assert_eq!(l.sha256, sha256_hex(body));
        assert_eq!((l.version, l.min_bageo.as_str()), (VERSION, "1.6.9"));
    }
    for a in &b.index.art {
        let body = &b.files[&a.file];
        assert_eq!(a.file, art_file_name(&a.region));
        assert_eq!(a.bytes, body.len() as u64);
        assert_eq!(a.sha256, sha256_hex(body));
        assert!(check_art(&a.file, body, &a.region).is_empty());
    }
    let (back, issues) = parse_extras_index(&b.index_json).unwrap();
    assert!(issues.is_empty());
    assert_eq!(back, b.index);
}

#[test]
fn levels_follow_how_much_text_a_language_has() {
    let b = built();
    let level = |c: &str| b.index.langs.iter().find(|l| l.code == c).unwrap().level.clone();
    assert_eq!(level("ru"), "console"); // all notify keys and console text
    assert_eq!(level("hi"), "notify"); // all notify keys, no console
    assert_eq!(level("it"), "basic"); // 6 of 10 notify keys
}

#[test]
fn a_language_file_has_names_status_notify_console_and_desktop_but_no_speech() {
    let b = built();
    let ru = unpacked(&b.files["lang-ru.json.gz"]);
    let keys: Vec<&str> = ru.as_object().unwrap().keys().map(String::as_str).collect();
    for k in ["format", "code", "version", "built_at", "source_commit", "names", "status", "notify", "console", "desktop"] {
        assert!(keys.contains(&k), "{k} missing in {keys:?}");
    }
    assert!(!keys.contains(&"speech"));
    assert_eq!(ru["names"]["native"], "Русский");
    assert_eq!(ru["notify"]["strings"]["n_incoming"], "Входящий звонок");
    assert_eq!(ru["console"]["strings"]["tab_calls"], "Звонки");
    assert_eq!(ru["desktop"]["strings"]["h_ok"], "Всё в порядке");
    assert_eq!(ru["built_at"], BUILT);
    let hi = unpacked(&b.files["lang-hi.json.gz"]);
    assert!(hi.get("console").is_none());
    assert_eq!(hi["notify"]["strings"]["n_incoming"], "आने वाली कॉल");
    // A key a reviewer marked needs_fix stays out (it: n_hidden).
    let it = unpacked(&b.files["lang-it.json.gz"]);
    assert!(it["notify"]["strings"].get("n_hidden").is_none());
    assert_eq!(it["notify"]["strings"]["n_incoming"], "Chiamata in arrivo");
}

#[test]
fn each_built_language_reads_back_without_issues() {
    let b = built();
    let english = sources().english;
    let natives = [("ar", "العربية"), ("hi", "हिन्दी"), ("it", "Italiano"), ("ru", "Русский"), ("th", "ไทย")];
    for (code, native) in natives {
        let (lang, issues) = parse_lang_file(&b.files[&lang_file_name(code)], code, &english).unwrap_or_else(|e| panic!("{code}: {}", show(&e)));
        assert!(issues.is_empty(), "{code}: {}", show(&issues));
        assert_eq!((lang.code.as_str(), lang.names.native.as_str()), (code, native));
        assert!(lang.desktop.is_some());
    }
}

#[test]
fn building_twice_gives_the_same_bytes() {
    let (a, b) = (built(), built());
    assert_eq!(a.files, b.files);
    assert_eq!(a.index_json, b.index_json);
}

#[test]
fn a_source_that_breaks_a_rule_builds_nothing() {
    let mut s = sources();
    let ru = s.langs.iter_mut().find(|l| l.code == "ru").unwrap();
    ru.files.get_mut(&Kind::Notify).unwrap().data.strings.insert("n_incoming".into(), json!("Visit https://example.invalid"));
    let err = build_extras(&s, &art_sources(), &region_langs(), VERSION, COMMIT, BUILT, "1.6.9").unwrap_err();
    expect(&err, lrule::PATTERN, "strings.n_incoming");
    // Nothing to build at all is also an error.
    let none = build_extras(&LocaleSources::default(), &BTreeMap::new(), &BTreeMap::new(), VERSION, COMMIT, BUILT, "1.6.9").unwrap_err();
    expect(&none, xrule::INDEX, "");
    // A bad header value is refused before anything else.
    assert!(build_extras(&sources(), &art_sources(), &region_langs(), 12, COMMIT, BUILT, "1.6.9").is_err());
    assert!(build_extras(&sources(), &art_sources(), &region_langs(), VERSION, "a b", BUILT, "1.6.9").is_err());
}

// ---- drawings inside the language files ---------------------------------------------------------

fn lang_art(b: &ExtrasBuild, code: &str) -> Vec<String> {
    unpacked(&b.files[&lang_file_name(code)]).get("art").and_then(Value::as_object).map_or_else(Vec::new, |m| m.keys().cloned().collect())
}

#[test]
fn each_language_file_carries_the_drawings_of_its_regions() {
    let b = built();
    assert_eq!(lang_art(&b, "it"), ["it"]);
    assert_eq!(lang_art(&b, "th"), ["th"]);
    for code in ["ar", "hi", "ru"] {
        assert!(lang_art(&b, code).is_empty(), "{code}");
    }
    // The drawing in the language file is the art file's object.
    let it = unpacked(&b.files["lang-it.json.gz"]);
    let file: Value = serde_json::from_slice(&b.files["art-it.json"]).unwrap();
    assert_eq!(it["art"]["it"], file);
    let (lang, issues) = parse_lang_file(&b.files["lang-it.json.gz"], "it", &sources().english).unwrap();
    assert!(issues.is_empty(), "{}", show(&issues));
    assert_eq!(lang.art.keys().collect::<Vec<_>>(), ["it"]);
}

#[test]
fn lang_ar_carries_eg_levant_maghreb_and_core_regions_stay_out() {
    let (art, langs) = more_art(&[("eg", "ar-EG"), ("levant", "ar-EG"), ("maghreb", "ar-EG"), ("mx", "es-US"), ("hk-mo", "cmn"), ("by", "ru-RU")]);
    let b = build_extras(&sources(), &art, &langs, VERSION, COMMIT, BUILT, "1.6.9").unwrap_or_else(|e| panic!("{}", show(&e)));
    assert_eq!(lang_art(&b, "ar"), ["eg", "levant", "maghreb"]);
    assert_eq!(lang_art(&b, "ru"), ["by"]);
    for code in LANGS {
        let carried = lang_art(&b, code);
        assert!(!carried.iter().any(|r| r == "mx" || r == "hk-mo"), "{code}: {carried:?}");
    }
    // Every drawing still gets its own file and index entry, for Bageo versions that download them one by one.
    let regions: Vec<&str> = b.index.art.iter().map(|a| a.region.as_str()).collect();
    assert_eq!(regions, ["by", "eg", "hk-mo", "it", "levant", "maghreb", "mx", "th"]);
    assert!(b.files.contains_key("art-mx.json") && b.files.contains_key("art-eg.json"));
    let (ar, issues) = parse_lang_file(&b.files["lang-ar.json.gz"], "ar", &sources().english).unwrap();
    assert!(issues.is_empty(), "{}", show(&issues));
    assert_eq!(ar.art.len(), 3);
}

#[test]
fn art_home_reads_region_languages_like_the_language_checks() {
    for (lang, code) in [("ar-EG", "ar"), ("nl-NL", "nl"), ("ru-RU", "ru"), ("bn-BD", "bn"), ("nb", "nb"), ("pl-PL", "pl"), ("th-TH", "th"), ("sq", "sq")] {
        assert_eq!(art_home(lang), Some(ArtHome::Lang(code.into())), "{lang}");
    }
    for lang in ["cmn", "en-US", "en-IN", "es-US", "pt-BR", "de-DE", "fr-FR", "ja-JP", "ko-KR", "zh-TW"] {
        assert_eq!(art_home(lang), Some(ArtHome::Builtin), "{lang}");
    }
    for lang in ["xx", "", "klingon"] {
        assert_eq!(art_home(lang), None, "{lang}");
    }
}

#[test]
fn a_drawing_needs_a_region_and_a_language_file_to_ride_in() {
    // No regions/<code>.json.
    let mut langs = region_langs();
    langs.remove("th");
    let err = build_extras(&sources(), &art_sources(), &langs, VERSION, COMMIT, BUILT, "1.6.9").unwrap_err();
    expect(&err, arule::HOME, "region");
    assert!(err.iter().any(|i| i.file == "art/th.json"), "{}", show(&err));
    // A main language not on the official list.
    let (art, langs) = more_art(&[("zz", "xx-YY")]);
    let err = build_extras(&sources(), &art, &langs, VERSION, COMMIT, BUILT, "1.6.9").unwrap_err();
    assert!(err.iter().any(|i| i.file == "art/zz.json" && i.rule == arule::HOME), "{}", show(&err));
    // An extra language with no notify or console text: no file to carry it.
    let (art, langs) = more_art(&[("vn", "vi-VN")]);
    let err = build_extras(&sources(), &art, &langs, VERSION, COMMIT, BUILT, "1.6.9").unwrap_err();
    assert!(err.iter().any(|i| i.file == "art/vn.json" && i.rule == arule::HOME && i.detail.contains("vi")), "{}", show(&err));
    // packs check finds the same.
    let d = tmp("check-art");
    copy_tree(&fixture("extras"), &d);
    let (n, issues) = check_art_sources(&d, &sources());
    assert_eq!(n, 2);
    assert!(issues.is_empty(), "{}", show(&issues));
    fs::copy(fixture("extras/art/it.json"), d.join("art/vn.json")).unwrap();
    let (_, issues) = check_art_sources(&d, &sources());
    expect(&issues, arule::FORMAT, "region"); // the copy still says "it"
    expect(&issues, arule::HOME, "region"); // and has no regions/vn.json
    fs::write(d.join("regions/vn.json"), r#"{"code": "vn", "lang": "vi-VN"}"#).unwrap();
    let (_, issues) = check_art_sources(&d, &sources());
    assert!(issues.iter().any(|i| i.file == "art/vn.json" && i.rule == arule::HOME && i.detail.contains("vi")), "{}", show(&issues));
    let _ = fs::remove_dir_all(&d);
}

/// The built Italian file with its art replaced, gzipped again.
fn it_with_art(art: Value) -> Vec<u8> {
    let mut v = unpacked(&built().files["lang-it.json.gz"]);
    v["art"] = art;
    gzip(&serde_json::to_vec(&v).unwrap())
}

#[test]
fn a_broken_drawing_in_a_language_file_is_left_out_alone() {
    let good: Value = serde_json::from_slice(&built().files["art-it.json"]).unwrap();
    let mut script = good.clone();
    script["region"] = json!("vn");
    script["draw"]["print"] = json!([["script", {}]]);
    let mut wrong_region = good.clone();
    wrong_region["region"] = json!("pl");
    let english = sources().english;
    let (lang, issues) = parse_lang_file(&it_with_art(json!({"it": good, "vn": script, "kr": wrong_region, "Bad": {}})), "it", &english).unwrap();
    assert_eq!(lang.art.keys().collect::<Vec<_>>(), ["it"]);
    expect(&issues, arule::ELEMENT, "art.vn.draw.print[0]");
    expect(&issues, arule::FORMAT, "art.kr.region");
    expect(&issues, arule::FORMAT, "art.Bad");
    assert_eq!(issues.len(), 3, "{}", show(&issues));
    // The language itself is untouched.
    assert_eq!(lang.notify.unwrap().strings["n_incoming"], "Chiamata in arrivo");
    // art that is not an object is left out, the language stays.
    let (lang, issues) = parse_lang_file(&it_with_art(json!(["it"])), "it", &english).unwrap();
    assert!(lang.art.is_empty() && lang.console.is_some());
    expect(&issues, arule::FORMAT, "art");
    // More drawings than a language file may carry: the rest is left out.
    let many: serde_json::Map<String, Value> = (0..MAX_LANG_ART + 2)
        .map(|i| {
            let region = format!("r{}", (b'a' + i as u8) as char);
            let mut d = good.clone();
            d["region"] = json!(region);
            (region, d)
        })
        .collect();
    let (lang, issues) = parse_lang_file(&it_with_art(Value::Object(many)), "it", &english).unwrap();
    assert_eq!(lang.art.len(), MAX_LANG_ART);
    expect(&issues, arule::SIZE, "art.");
}

#[test]
fn a_language_file_without_art_still_reads() {
    let mut v = unpacked(&built().files["lang-it.json.gz"]);
    assert!(v.as_object_mut().unwrap().remove("art").is_some());
    let (lang, issues) = parse_lang_file(&gzip(&serde_json::to_vec(&v).unwrap()), "it", &sources().english).unwrap();
    assert!(issues.is_empty(), "{}", show(&issues));
    assert!(lang.art.is_empty());
    // And a file written without art has no art field at all.
    let back = serde_json::to_value(&lang).unwrap();
    assert!(back.get("art").is_none());
}

// ---- the drawings built into Bageo --------------------------------------------------------------

#[test]
fn the_bundle_holds_only_core_language_regions_and_reads_back() {
    let (art, langs) = more_art(&[("eg", "ar-EG"), ("mx", "es-US"), ("hk-mo", "cmn"), ("in", "en-IN")]);
    let (gz, regions) = build_art_bundle(&art, &langs, VERSION, COMMIT, BUILT).unwrap_or_else(|e| panic!("{}", show(&e)));
    assert_eq!(regions, ["hk-mo", "in", "mx"]);
    let (bundle, issues) = parse_art_bundle(&gz).unwrap();
    assert!(issues.is_empty(), "{}", show(&issues));
    assert_eq!(bundle.art.keys().collect::<Vec<_>>(), ["hk-mo", "in", "mx"]);
    assert_eq!(bundle.version, VERSION);
    let b = build_extras(&sources(), &art, &langs, VERSION, COMMIT, BUILT, "1.6.9").unwrap();
    let file: Value = serde_json::from_slice(&b.files["art-mx.json"]).unwrap();
    assert_eq!(bundle.art["mx"], file);
    // Building twice gives the same bytes.
    assert_eq!(build_art_bundle(&art, &langs, VERSION, COMMIT, BUILT).unwrap().0, gz);
    // Only extra-language regions: nothing to build.
    let err = build_art_bundle(&art_sources(), &region_langs(), VERSION, COMMIT, BUILT).unwrap_err();
    expect(&err, arule::FORMAT, "art");
    // A broken drawing in the bundle is left out alone; a newer format refuses it.
    let mut v = unpacked(&gz);
    v["art"]["mx"]["draw"]["print"] = json!([["image", {}]]);
    let (bundle, issues) = parse_art_bundle(&gzip(&serde_json::to_vec(&v).unwrap())).unwrap();
    assert_eq!(bundle.art.keys().collect::<Vec<_>>(), ["hk-mo", "in"]);
    expect(&issues, arule::ELEMENT, "art.mx.draw.print[0]");
    v["format"] = json!(2);
    let err = parse_art_bundle(&gzip(&serde_json::to_vec(&v).unwrap())).unwrap_err();
    assert!(err[0].detail.contains("update Bageo first"), "{}", show(&err));
}

// ---- reading a language file -------------------------------------------------------------------

fn bad_lang(name: &str, code: &str) -> Result<(ExtraLang, Vec<Issue>), Vec<Issue>> {
    let bytes = fs::read(fixture("extras_bad").join(name)).unwrap();
    parse_lang_file(&gzip(&bytes), code, &sources().english)
}

#[test]
fn a_bad_key_is_dropped_alone_and_reported() {
    // Italian: a web address in a sentence (L07); the other key stays.
    let (it, issues) = bad_lang("lang-it-url.json", "it").unwrap();
    expect(&issues, lrule::PATTERN, "notify.strings.n_incoming");
    let notify = it.notify.unwrap();
    assert!(!notify.strings.contains_key("n_incoming"));
    assert_eq!(notify.strings["n_hidden"], "Numero nascosto");
    // Russian: Latin text instead of Cyrillic (L08).
    let (ru, issues) = bad_lang("lang-ru-latin.json", "ru").unwrap();
    expect(&issues, lrule::UNTRANSLATED, "notify.strings.n_incoming");
    assert!(!ru.notify.unwrap().strings.contains_key("n_incoming"));
    // Hindi: a placeholder lost (L04).
    let (hi, issues) = bad_lang("lang-hi-placeholder.json", "hi").unwrap();
    expect(&issues, lrule::PLACEHOLDER, "notify.strings.n_repeat_body");
    assert!(!hi.notify.unwrap().strings.contains_key("n_repeat_body"));
}

#[test]
fn a_newer_format_wrong_names_or_wrong_code_refuse_the_file() {
    let err = bad_lang("lang-ar-newer.json", "ar").unwrap_err();
    assert!(err.iter().any(|i| i.rule == lrule::FORMAT && i.detail.contains("update Bageo first")), "{}", show(&err));
    let err = bad_lang("lang-th-names.json", "th").unwrap_err();
    expect(&err, lrule::NAMES, "names.official");
    // A file for another language than the one asked for (L02).
    let b = built();
    let err = parse_lang_file(&b.files["lang-it.json.gz"], "ru", &sources().english).unwrap_err();
    expect(&err, lrule::CODE, "code");
    // Core languages and made-up codes are never extra languages.
    for code in ["en", "ja", "zh-TW", "xx"] {
        let err = parse_lang_file(&b.files["lang-it.json.gz"], code, &sources().english).unwrap_err();
        expect(&err, lrule::CODE, "");
    }
}

#[test]
fn a_language_file_is_cut_off_at_600_kib_packed_and_unpacked() {
    let english = sources().english;
    // Unpacked: 700 KiB of blanks packs to almost nothing; reading stops at the limit.
    let big = gzip(&vec![b' '; 700 * 1024]);
    assert!(big.len() < 10 * 1024);
    let err = parse_lang_file(&big, "it", &english).unwrap_err();
    expect(&err, lrule::SIZE, "");
    assert!(err[0].detail.contains("stopped reading"), "{}", show(&err));
    assert_eq!(err[0].file, "lang-it.json.gz");
    // Packed: more bytes than the limit before anything is unpacked.
    let err = parse_lang_file(&vec![7u8; 601 * 1024], "it", &english).unwrap_err();
    expect(&err, lrule::SIZE, "");
    // A second gzip member would go unread; the file is refused.
    let b = built();
    let mut two = b.files["lang-it.json.gz"].clone();
    two.extend_from_slice(&b.files["lang-ru.json.gz"]);
    let err = parse_lang_file(&two, "it", &english).unwrap_err();
    expect(&err, lrule::FORMAT, "");
}

// ---- the index ---------------------------------------------------------------------------------

fn lang_entry(code: &str) -> Value {
    json!({"code": code, "file": format!("lang-{code}.json.gz"), "bytes": 25000, "sha256": "ab".repeat(32), "version": VERSION,
           "min_bageo": "1.6.9", "status": "ai_checked", "level": "console", "note": "unknown fields are ignored"})
}

fn art_entry(region: &str) -> Value {
    json!({"region": region, "file": format!("art-{region}.json"), "bytes": 2500, "sha256": "cd".repeat(32), "version": VERSION, "min_bageo": "1.6.9"})
}

#[test]
fn the_index_ignores_unknown_fields_and_keeps_good_entries() {
    let text = json!({"format": 1, "version": VERSION, "future": {"x": 1},
        "langs": [lang_entry("it"), lang_entry("ru"), lang_entry("vi"), lang_entry("ar")], "art": [art_entry("hk-mo"), art_entry("th")]})
    .to_string();
    let (ix, issues) = parse_extras_index(&text).unwrap();
    assert!(issues.is_empty(), "{}", show(&issues));
    assert_eq!(ix.langs.len(), 4);
    assert_eq!(ix.art.iter().map(|a| a.region.as_str()).collect::<Vec<_>>(), ["hk-mo", "th"]);
}

#[test]
fn a_broken_index_entry_is_left_out_alone() {
    let mut core = lang_entry("ja");
    core["file"] = json!("lang-ja.json.gz");
    let mut path = lang_entry("th");
    path["file"] = json!("../lang-th.json.gz");
    let mut other = lang_entry("hi");
    other["file"] = json!("lang-ru.json.gz");
    let mut empty = lang_entry("ar");
    empty["bytes"] = json!(0);
    let mut huge = lang_entry("vi");
    huge["bytes"] = json!(601 * 1024);
    let mut hash = lang_entry("pl");
    hash["sha256"] = json!("XYZ");
    let mut level = lang_entry("sv");
    level["level"] = json!("everything");
    let mut status = lang_entry("tr");
    status["status"] = json!("great");
    let mut min = lang_entry("id");
    min["min_bageo"] = json!("1.x");
    let mut ver = lang_entry("nl");
    ver["version"] = json!(2026133301u64);
    let text = json!({"format": 1, "version": VERSION,
        "langs": [lang_entry("it"), core, path, other, empty, huge, hash, level, status, min, ver, lang_entry("it"), json!("junk"), lang_entry("ru")],
        "art": [art_entry("hk-mo"), art_entry("Bad"), art_entry("x"), art_entry("th"), art_entry("th"),
                {"region": "jp", "file": "art-kr.json", "bytes": 100, "sha256": "ab".repeat(32), "version": VERSION, "min_bageo": "1.6.9"},
                {"region": "kr", "file": "art-kr.json", "bytes": 65 * 1024, "sha256": "ab".repeat(32), "version": VERSION, "min_bageo": "1.6.9"}]})
    .to_string();
    let (ix, issues) = parse_extras_index(&text).unwrap();
    assert_eq!(ix.langs.iter().map(|l| l.code.as_str()).collect::<Vec<_>>(), ["it", "ru"]);
    assert_eq!(ix.art.iter().map(|a| a.region.as_str()).collect::<Vec<_>>(), ["hk-mo", "th"]);
    expect(&issues, xrule::CODE, "langs[1].code");
    expect(&issues, xrule::FILE, "langs[2].file");
    expect(&issues, xrule::FILE, "langs[3].file");
    expect(&issues, xrule::SIZE, "langs[4].bytes");
    expect(&issues, xrule::SIZE, "langs[5].bytes");
    expect(&issues, xrule::FIELD, "langs[6].sha256");
    expect(&issues, xrule::FIELD, "langs[7].level");
    expect(&issues, xrule::FIELD, "langs[8].status");
    expect(&issues, xrule::FIELD, "langs[9].min_bageo");
    expect(&issues, xrule::FIELD, "langs[10].version");
    expect(&issues, xrule::CODE, "langs[11].code");
    expect(&issues, xrule::INDEX, "langs[12]");
    expect(&issues, xrule::CODE, "art[1].region");
    expect(&issues, xrule::CODE, "art[2].region");
    expect(&issues, xrule::CODE, "art[4].region");
    expect(&issues, xrule::FILE, "art[5].file");
    expect(&issues, xrule::SIZE, "art[6].bytes");
    assert_eq!(issues.len(), 17);
}

#[test]
fn a_newer_format_or_a_bad_header_refuses_the_whole_index() {
    let err = parse_extras_index(&json!({"format": 2, "version": VERSION, "langs": []}).to_string()).unwrap_err();
    assert!(err[0].detail.contains("update Bageo first"), "{}", show(&err));
    for text in [
        "not json".to_string(),
        "[]".to_string(),
        json!({"version": VERSION}).to_string(),
        json!({"format": "1", "version": VERSION}).to_string(),
        json!({"format": 0, "version": VERSION}).to_string(),
        json!({"format": 1}).to_string(),
        json!({"format": 1, "version": 5}).to_string(),
        json!({"format": 1, "version": VERSION, "langs": {"a": 1}}).to_string(),
    ] {
        assert!(parse_extras_index(&text).is_err(), "{text}");
    }
    let big = format!("{{\"format\":1,\"version\":{VERSION},\"pad\":\"{}\"}}", "x".repeat(MAX_INDEX_BYTES));
    expect(&parse_extras_index(&big).unwrap_err(), xrule::SIZE, "");
}

// ---- the art files -----------------------------------------------------------------------------

fn art_it() -> Value {
    serde_json::from_slice(&art_sources()["it"]).unwrap()
}

fn art_issues(v: &Value, region: &str) -> Vec<Issue> {
    match read_art_source("art/test.json", &serde_json::to_vec(v).unwrap(), region) {
        Ok(_) => Vec::new(),
        Err(i) => i,
    }
}

fn with_node(extra: Value) -> Vec<Issue> {
    let mut v = art_it();
    v["draw"]["props"]["it-dome"].as_array_mut().unwrap().push(extra);
    art_issues(&v, "it")
}

#[test]
fn the_good_art_passes() {
    for region in ["it", "th"] {
        let src = &art_sources()[region];
        assert!(read_art_source("art/x.json", src, region).is_ok());
    }
    assert!(art_issues(&art_it(), "it").is_empty());
}

#[test]
fn the_bad_art_samples_are_refused_with_their_rule() {
    let bad = |name: &str| {
        let bytes = fs::read(fixture("extras_bad").join(name)).unwrap();
        read_art_source(name, &bytes, "it").unwrap_err()
    };
    expect(&bad("art-script.json"), arule::ELEMENT, "draw.props.it-dome[2]");
    expect(&bad("art-href.json"), arule::ATTRIBUTE, "draw.props.it-dome[2].href");
    expect(&bad("art-onload.json"), arule::ATTRIBUTE, "draw.props.it-dome[2].onload");
    expect(&bad("art-url-http.json"), arule::VALUE, "draw.props.it-dome[2].fill");
    expect(&bad("art-style-url.json"), arule::ATTRIBUTE, "draw.props.it-dome[2].style");
    expect(&bad("art-pal-url.json"), arule::PALETTE, "look.pal.o1");
}

#[test]
fn only_listed_elements_and_attributes_pass() {
    for tag in ["script", "text", "image", "use", "foreignObject", "style", "animate", "svg", "G", "a", "linearGradient"] {
        expect(&with_node(json!([tag, {}])), arule::ELEMENT, "draw.props.it-dome[2]");
    }
    for tag in ["g", "path", "ellipse", "circle", "rect", "line", "polyline", "polygon"] {
        assert!(with_node(json!([tag, {}])).is_empty(), "{tag}");
    }
    for attr in ["class", "id", "style", "href", "xlink:href", "onclick", "onload", "data-x", "filter", "mask", "font-size", "Fill", "stroke-Width", "stroke-"] {
        expect(&with_node(json!(["g", {attr: "1"}])), arule::ATTRIBUTE, "draw.props.it-dome[2]");
    }
    for attr in ["d", "cx", "cy", "r", "rx", "ry", "x", "y", "width", "height", "points", "x1", "y1", "x2", "y2", "transform", "opacity", "fill-opacity", "fill-rule", "stroke-width", "stroke-linecap", "stroke-dasharray"] {
        assert!(with_node(json!(["g", {attr: "1"}])).is_empty(), "{attr}");
    }
}

#[test]
fn colours_clip_paths_and_references_are_limited() {
    for ok in ["#fff", "#ffff", "#a1B2c3", "#a1b2c3d4", "none", "var(--bts-o1)", "var(--bts-o3l)"] {
        assert!(with_node(json!(["rect", {"fill": ok, "stroke": ok}])).is_empty(), "{ok}");
    }
    for bad in ["red", "#12", "#12345", "#ggg", "url(#a)", "url(http://example.invalid/a.png)", "var(--x)", "var(--bts-O1)", "var(--bts-)", "currentColor", "rgb(1,2,3)", "javascript:alert(1)"] {
        expect(&with_node(json!(["rect", {"fill": bad}])), arule::VALUE, "draw.props.it-dome[2].fill");
    }
    for ok in ["url(#bts-clip-win)", "url(#bts-clip-body)"] {
        assert!(with_node(json!(["g", {"clip-path": ok}])).is_empty());
    }
    for bad in ["url(#other)", "url(#bts-clip-win) ", "none", "url(http://example.invalid/c.svg#bts-clip-win)", "inset(0)"] {
        expect(&with_node(json!(["g", {"clip-path": bad}])), arule::VALUE, "draw.props.it-dome[2].clip-path");
    }
    for bad in ["translate(1 2) url(#a)", "M0 0 URL(x)", "1;2", "a:b", "<", "1\n2", "1&amp;"] {
        expect(&with_node(json!(["path", {"d": bad}])), arule::VALUE, "draw.props.it-dome[2].d");
    }
    expect(&with_node(json!(["g", {"opacity": true}])), arule::VALUE, "draw.props.it-dome[2].opacity");
    // Numbers are fine as numbers.
    assert!(with_node(json!(["circle", {"cx": 10, "cy": 12.5, "r": 3}])).is_empty());
}

#[test]
fn the_palette_takes_known_keys_and_hex_or_var_colours() {
    let set = |k: &str, c: Value| {
        let mut v = art_it();
        v["look"]["pal"][k] = c;
        art_issues(&v, "it")
    };
    assert!(set("tie", json!("#c6453a")).is_empty());
    assert!(set("o5l", json!("var(--bts-o5)")).is_empty());
    expect(&set("o1", json!("none")), arule::PALETTE, "look.pal.o1");
    expect(&set("o1", json!("red")), arule::PALETTE, "look.pal.o1");
    expect(&set("o1", json!("url(#x)")), arule::PALETTE, "look.pal.o1");
    expect(&set("o1", json!(5)), arule::PALETTE, "look.pal.o1");
    expect(&set("color", json!("#fff")), arule::PALETTE, "look.pal.color");
}

#[test]
fn size_node_and_depth_limits_refuse_the_whole_file() {
    let nest = |n: usize| -> Value {
        let mut node = json!(["g", {}]);
        for _ in 1..n {
            node = json!(["g", {}, [node]]);
        }
        node
    };
    let with_print = |nodes: Vec<Value>| {
        let mut v = art_it();
        v["draw"]["print"] = Value::Array(nodes);
        art_issues(&v, "it")
    };
    assert!(with_print(vec![nest(6)]).is_empty());
    expect(&with_print(vec![nest(7)]), arule::SIZE, "draw.print");
    // 800 nodes in all pass; one more is refused. The good file already has some.
    let have = {
        let mut v = art_it();
        v["draw"]["print"] = json!([]);
        let mut n = 0usize;
        fn count(v: &Value, n: &mut usize) {
            if let Some(a) = v.as_array() {
                if a.len() >= 2 && a[0].is_string() {
                    *n += 1;
                    if let Some(c) = a.get(2).and_then(Value::as_array) {
                        c.iter().for_each(|x| count(x, n));
                    }
                } else {
                    a.iter().for_each(|x| count(x, n));
                }
            } else if let Some(o) = v.as_object() {
                o.values().for_each(|x| count(x, n));
            }
        }
        count(&v["draw"], &mut n);
        n
    };
    let fill = |total: usize| with_print((0..total - have).map(|_| json!(["g", {}])).collect());
    assert!(fill(800).is_empty());
    expect(&fill(801), arule::SIZE, "");
    // More than 64 KiB, however it is made up.
    let mut v = art_it();
    v["draw"]["props"]["it-dome"] = json!([["path", {"d": "M0 0 ".repeat(14_000)}]]);
    let bytes = serde_json::to_vec(&v).unwrap();
    assert!(bytes.len() > MAX_ART_BYTES);
    expect(&check_art("art-it.json", &bytes, "it"), arule::SIZE, "");
    // One value over 4096 characters.
    let mut v = art_it();
    v["draw"]["props"]["it-dome"] = json!([["path", {"d": "M0 0 ".repeat(900)}]]);
    expect(&art_issues(&v, "it"), arule::VALUE, "draw.props.it-dome[0].d");
}

#[test]
fn the_shape_of_an_art_file_is_checked() {
    let mut v = art_it();
    v["format"] = json!(2);
    assert!(art_issues(&v, "it")[0].detail.contains("update Bageo first"));
    expect(&art_issues(&art_it(), "th"), arule::FORMAT, "region");
    expect(&art_issues(&art_it(), "IT"), arule::FORMAT, "region");
    for (path, value) in [
        ("look", json!(null)),
        ("draw", json!([])),
        ("look.bow", json!("yes")),
        ("look.print", json!("Has Space")),
        ("look.scenes", json!([])),
        ("look.scenes", json!([["noon", ["sun"]]])),
        ("look.scenes", json!([["day", ["Sun!"]]])),
        ("draw.print", json!({"a": 1})),
        ("draw.props", json!([])),
        ("draw.acc", json!([{"l": [9], "nodes": []}])),
        ("draw.acc", json!([{"l": [1], "nodes": "x"}])),
    ] {
        let mut v = art_it();
        let (head, tail) = path.split_once('.').map_or((path, ""), |(a, b)| (a, b));
        if tail.is_empty() {
            v[head] = value;
        } else {
            v[head][tail] = value;
        }
        let issues = art_issues(&v, "it");
        assert!(issues.iter().any(|i| i.field.starts_with(path) || i.field.starts_with(head)), "{path}: {}", show(&issues));
    }
    for node in [json!("g"), json!(["g"]), json!([1, {}]), json!(["g", []]), json!(["g", {}, "x"]), json!(["g", {}, [], 4])] {
        expect(&with_node(node), arule::FORMAT, "draw.props.it-dome[2]");
    }
    // A downloaded file also needs its version (a source file does not).
    let body = serde_json::to_vec(&art_it()).unwrap();
    expect(&check_art("art-it.json", &body, "it"), arule::FORMAT, "version");
    assert!(read_art_source("art/it.json", &body, "it").is_ok());
    // Unknown top-level fields are ignored.
    let mut v = art_it();
    v["note"] = json!("<script>");
    assert!(art_issues(&v, "it").is_empty());
}

// ---- the command line --------------------------------------------------------------------------

fn packs() -> Command {
    Command::new(env!("CARGO_BIN_EXE_packs"))
}

#[test]
fn build_extras_writes_the_files_and_an_index_that_matches_them() {
    let d = tmp("cli");
    let out = d.join("out");
    let run = |src: PathBuf| {
        packs()
            .arg("build-extras")
            .arg(src)
            .args(["--version", "2026010101", "--commit", COMMIT, "--built-at", BUILT, "--min-bageo", "1.6.9", "--out"])
            .arg(&out)
            .output()
            .unwrap()
    };
    let r = run(fixture("extras"));
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stdout));
    let mut names: Vec<String> = fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(
        names,
        ["art-it.json", "art-th.json", "bageo-extras-index.json", "lang-ar.json.gz", "lang-hi.json.gz", "lang-it.json.gz", "lang-ru.json.gz", "lang-th.json.gz"]
    );
    let (ix, issues) = parse_extras_index(&fs::read_to_string(out.join("bageo-extras-index.json")).unwrap()).unwrap();
    assert!(issues.is_empty());
    for l in &ix.langs {
        let body = fs::read(out.join(&l.file)).unwrap();
        assert_eq!((l.bytes, &l.sha256), (body.len() as u64, &sha256_hex(&body)));
    }
    for a in &ix.art {
        let body = fs::read(out.join(&a.file)).unwrap();
        assert_eq!((a.bytes, &a.sha256), (body.len() as u64, &sha256_hex(&body)));
        assert!(check_art(&a.file, &body, &a.region).is_empty());
    }
    // The same sources and time give the same files as the library builds.
    assert_eq!(fs::read(out.join("lang-ru.json.gz")).unwrap(), built().files["lang-ru.json.gz"]);

    // A bad art file stops the build with its rule code and writes nothing.
    let src = d.join("src");
    copy_tree(&fixture("extras"), &src);
    fs::copy(fixture("extras_bad").join("art-script.json"), src.join("art/jp.json")).unwrap();
    let out2 = d.join("out2");
    let r = packs()
        .arg("build-extras")
        .arg(&src)
        .args(["--version", "2026010101", "--commit", COMMIT, "--out"])
        .arg(&out2)
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(1));
    let said = String::from_utf8_lossy(&r.stdout);
    assert!(said.contains("[A01-format]") || said.contains("[A03-element]"), "{said}");
    assert!(!out2.exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn build_art_writes_the_core_language_drawings() {
    let d = tmp("cli-art");
    let src = d.join("src");
    copy_tree(&fixture("extras"), &src);
    let out = d.join("bageo-art.json.gz");
    let run = |src: &PathBuf| {
        packs()
            .arg("build-art")
            .arg(src)
            .args(["--version", "2026010101", "--commit", COMMIT, "--built-at", BUILT, "--out"])
            .arg(&out)
            .output()
            .unwrap()
    };
    // Only extra-language regions: nothing to build, nothing written.
    let r = run(&src);
    assert_eq!(r.status.code(), Some(1), "{}", String::from_utf8_lossy(&r.stdout));
    assert!(!out.exists());
    let mut mx: Value = serde_json::from_slice(&art_sources()["it"]).unwrap();
    mx["region"] = json!("mx");
    fs::write(src.join("art/mx.json"), serde_json::to_vec(&mx).unwrap()).unwrap();
    fs::write(src.join("regions/mx.json"), r#"{"code": "mx", "lang": "es-US"}"#).unwrap();
    let r = run(&src);
    assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stdout));
    let (bundle, issues) = parse_art_bundle(&fs::read(&out).unwrap()).unwrap();
    assert!(issues.is_empty());
    assert_eq!(bundle.art.keys().collect::<Vec<_>>(), ["mx"]);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn build_extras_needs_all_its_arguments() {
    let r = packs().arg("build-extras").arg(fixture("extras")).args(["--version", "2026010101"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dst = to.join(p.file_name().unwrap());
        if p.is_dir() {
            copy_tree(&p, &dst);
        } else {
            fs::copy(&p, &dst).unwrap();
        }
    }
}

#[test]
fn call_only_keeps_names_status_and_speech() {
    let d = tmp("call");
    let (full, call) = (d.join("full.json.gz"), d.join("call.json.gz"));
    let run = |out: &PathBuf, extra: &[&str]| {
        packs()
            .arg("build-locales")
            .arg(fixture("locales"))
            .args(["--version", "2026010101", "--commit", COMMIT, "--built-at", BUILT, "--out"])
            .arg(out)
            .args(extra)
            .status()
            .unwrap()
    };
    assert!(run(&full, &[]).success());
    assert!(run(&call, &["--call-only"]).success());
    let (f, c) = (unpacked(&fs::read(&full).unwrap()), unpacked(&fs::read(&call).unwrap()));
    let (fl, cl) = (f["langs"].as_object().unwrap(), c["langs"].as_object().unwrap());
    assert_eq!(fl.keys().collect::<Vec<_>>(), cl.keys().collect::<Vec<_>>());
    assert!(!cl.is_empty());
    for (code, lang) in cl {
        let mut keys: Vec<&str> = lang.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["names", "speech", "status"], "{code}");
        assert!(["needs_fix", "ai_draft", "ai_checked", "local_reviewed"].contains(&lang["status"].as_str().unwrap()));
        assert_eq!(lang["names"], fl[code]["names"]);
        assert_eq!(lang["speech"], fl[code]["speech"]);
        assert!(fl[code].get("notify").is_some());
    }
    assert!(fs::metadata(&call).unwrap().len() < fs::metadata(&full).unwrap().len());
    // The file reads back through Bageo's door.
    assert!(parse_locales(&fs::read(&call).unwrap()).is_ok());
    let _ = fs::remove_dir_all(&d);
}

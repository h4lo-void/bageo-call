// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Verifies that the phone app (APK) ships built-in string resources for all 79
//! supported languages (the 9 core languages plus all 70 extra languages from
//! [OFFICIAL_LANG_INFO]) in `android/app/src/main/res/values-*/strings.xml` and
//! lists all 79 in `xml/locales_config.xml`, while the desktop widget does not
//! bundle `desktop/ui/i18n.ext.json` (it fetches extra languages from Bageo on demand).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use bageo_packs::data::OFFICIAL_LANG_INFO;
use bageo_packs::export::{android_qualifier, CORE_APP_LOCALES, GENERATED_MARK};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn res() -> PathBuf {
    repo().join("android/app/src/main/res")
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The `values-*` folders `packs export-app` wrote: folder suffix (`it`, `in`,
/// `iw`, `b+fil`) and file text.
fn generated() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for d in fs::read_dir(res()).unwrap().flatten() {
        let name = d.file_name().to_string_lossy().to_string();
        let Some(suffix) = name.strip_prefix("values-") else { continue };
        let xml = d.path().join("strings.xml");
        if let Ok(text) = fs::read_to_string(&xml) {
            if text.contains(GENERATED_MARK) {
                out.push((suffix.to_string(), text));
            }
        }
    }
    out.sort();
    out
}

/// What a folder suffix means as a language code in `locales_config.xml`.
fn code_of(suffix: &str) -> String {
    let raw = suffix.strip_prefix("b+").unwrap_or(suffix);
    match raw {
        "in" => "id",
        "iw" => "he",
        "ji" => "yi",
        c => c,
    }
    .to_string()
}

/// `<string name="...">value</string>` entries of a `strings.xml`: name ->
/// `(formatted="false", raw text)`. `translatable="false"` is skipped.
fn strings(xml: &str) -> BTreeMap<String, (bool, String)> {
    let mut out = BTreeMap::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<string ") {
        rest = &rest[i + 8..];
        let Some(open) = rest.find('>') else { break };
        let attrs = &rest[..open];
        let Some(close) = rest.find("</string>") else { break };
        let name = attrs.strip_prefix("name=\"").and_then(|a| a.split('"').next());
        if let Some(name) = name.filter(|_| !attrs.contains("translatable=\"false\"")) {
            out.insert(name.to_string(), (attrs.contains("formatted=\"false\""), rest[open + 1..close].to_string()));
        }
        rest = &rest[close..];
    }
    out
}

/// The printf-style arguments of a string, such as `%1$s` and `%d`, sorted.
fn tokens(s: &str) -> Vec<String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] != '%' {
            i += 1;
            continue;
        }
        if c.get(i + 1) == Some(&'%') {
            i += 2;
            continue;
        }
        let mut j = i + 1;
        while c.get(j).is_some_and(|x| x.is_ascii_digit() || *x == '$') {
            j += 1;
        }
        if c.get(j).is_some_and(|x| matches!(x, 's' | 'd' | 'f')) {
            out.push(c[i..=j].iter().collect());
        }
        i = j.max(i + 1);
    }
    out.sort();
    out
}

#[test]
fn android_folders_use_the_codes_android_reads() {
    let files = generated();
    assert_eq!(files.len(), 70, "expected all 70 non-core official languages in APK");
    let suffixes: BTreeSet<&str> = files.iter().map(|(s, _)| s.as_str()).collect();
    // Indonesian and Hebrew are in and iw to Android, not id and he.
    assert!(suffixes.contains("in") && suffixes.contains("iw"), "{suffixes:?}");
    assert!(!suffixes.contains("id") && !suffixes.contains("he") && !suffixes.contains("yi"), "{suffixes:?}");
    // Three-letter codes use b+ (values-fil would be ignored by Android).
    for three in ["b+ceb", "b+fil", "b+kok", "b+mai"] {
        assert!(suffixes.contains(three), "missing {three}");
    }
    for s in &suffixes {
        let plain = s.len() == 2 && s.bytes().all(|b| b.is_ascii_lowercase());
        assert!(plain || s.starts_with("b+"), "values-{s} is a form Android does not read");
        assert_eq!(android_qualifier(&code_of(s)), format!("values-{s}"));
    }
}

#[test]
fn the_language_list_names_the_nine_and_every_exported_language() {
    let cfg = read(&res().join("xml/locales_config.xml"));
    let mut got: Vec<String> =
        cfg.lines().filter_map(|l| Some(l.trim().strip_prefix("<locale android:name=\"")?.strip_suffix("\"/>")?.to_string())).collect();
    got.sort();
    let mut want: Vec<String> = CORE_APP_LOCALES.iter().map(|s| s.to_string()).collect();
    want.extend(generated().iter().map(|(s, _)| code_of(s)));
    want.sort();
    assert_eq!(got, want);
    assert_eq!(got.len(), 79);
    let mut official_ext: Vec<String> = OFFICIAL_LANG_INFO.iter().filter(|l| !l.core).map(|l| l.code.to_string()).collect();
    official_ext.sort();
    let mut exported_ext: Vec<String> = generated().iter().map(|(s, _)| code_of(s)).collect();
    exported_ext.sort();
    assert_eq!(exported_ext, official_ext);
    assert!(read(&repo().join("android/app/src/main/AndroidManifest.xml")).contains("android:localeConfig=\"@xml/locales_config\""));
    assert!(!repo().join("desktop/ui/i18n.ext.json").exists(), "desktop/ui/i18n.ext.json must not be bundled");
}

#[test]
fn exported_strings_keep_the_english_names_and_arguments() {
    let en = strings(&read(&res().join("values/strings.xml")));
    for (suffix, text) in generated() {
        let tr = strings(&text);
        for key in en.keys().filter(|k| !tr.contains_key(*k)) {
            panic!("values-{suffix} lacks {key}: add its translation");
        }
        for (name, (literal, value)) in &tr {
            let (_, english) = en.get(name).unwrap_or_else(|| panic!("values-{suffix} has {name}, English does not"));
            if !literal {
                assert_eq!(tokens(value), tokens(english), "values-{suffix} {name}: arguments differ from English");
            }
        }
    }
}

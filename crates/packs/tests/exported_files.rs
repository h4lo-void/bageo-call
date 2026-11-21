// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The files `packs export-app` wrote into this repository (the phone app's
//! values-*/strings.xml and xml/locales_config.xml). The CI step "i18n"
//! compares them with the pinned translations; these tests check what that
//! comparison cannot see: Android silently ignores a values folder it does
//! not know, so a wrong folder name never shows up as an error.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

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

/// The pinned translation source, as tools/bageo-packs.ref says.
fn pinned() -> String {
    read(&repo().join("tools/bageo-packs.ref")).trim().to_string()
}

/// (values folder suffix, file text) of every file export-app wrote.
fn generated() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for d in fs::read_dir(res()).unwrap().flatten() {
        let name = d.file_name().to_string_lossy().to_string();
        let Some(suffix) = name.strip_prefix("values-") else { continue };
        let Ok(text) = fs::read_to_string(d.path().join("strings.xml")) else { continue };
        if text.contains(GENERATED_MARK) {
            out.push((suffix.to_string(), text));
        }
    }
    out.sort();
    out
}

/// The language a values folder stands for (the reverse of android_qualifier).
fn code_of(suffix: &str) -> String {
    match suffix {
        "in" => "id".to_string(),
        "iw" => "he".to_string(),
        "ji" => "yi".to_string(),
        s => s.strip_prefix("b+").unwrap_or(s).to_string(),
    }
}

/// name, formatted="false" and value of each <string> in a strings.xml.
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

/// The printf-style arguments of a string, such as %1$s and %d, sorted (a
/// translation may put numbered arguments in another order).
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
fn the_pin_is_one_plain_word_and_every_generated_file_names_it() {
    let pin = pinned();
    assert!(!pin.is_empty() && pin.len() <= 64 && pin.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')), "{pin}");
    let files = generated();
    assert!(!files.is_empty());
    let want = format!("{GENERATED_MARK} from bageo-packs {pin}.");
    for (suffix, text) in &files {
        assert!(text.contains(&want), "values-{suffix} was not exported from {pin}");
    }
    assert!(read(&res().join("xml/locales_config.xml")).contains(&want));
}

#[test]
fn android_folders_use_the_codes_android_reads() {
    let files = generated();
    let suffixes: BTreeSet<&str> = files.iter().map(|(s, _)| s.as_str()).collect();
    // Indonesian and Hebrew are in and iw to Android, not id and he.
    assert!(suffixes.contains("in") && suffixes.contains("iw"), "{suffixes:?}");
    assert!(!suffixes.contains("id") && !suffixes.contains("he") && !suffixes.contains("yi"), "{suffixes:?}");
    for s in &suffixes {
        // Two letters, or b+ for a three-letter code (values-fil would be ignored).
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
    assert!(read(&repo().join("android/app/src/main/AndroidManifest.xml")).contains("android:localeConfig=\"@xml/locales_config\""));
}

#[test]
fn exported_strings_keep_the_english_names_and_arguments() {
    let en = strings(&read(&res().join("values/strings.xml")));
    for (suffix, text) in generated() {
        let tr = strings(&text);
        assert!(!tr.is_empty(), "values-{suffix} is empty");
        for (name, (literal, value)) in &tr {
            let (_, english) = en.get(name).unwrap_or_else(|| panic!("values-{suffix} has {name}, English does not"));
            if !literal {
                assert_eq!(tokens(value), tokens(english), "values-{suffix} {name}: arguments differ from English");
            }
        }
    }
}

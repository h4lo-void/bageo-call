// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! packs sync-source: copies Bageo's English text and the notes for
//! translators from a bageo-call checkout into source/en/<file>.json and
//! source/meta/<file>.json of the packs repository.
//!
//! Supported today: notify (JSON with its _meta.json), the phone app
//! (Android values/strings.xml) and the desktop widget (the "en" object of
//! desktop/ui/i18n.json). The console is read from [CONSOLE_EN] once its
//! English moves out of calls.html into a data file; until then it is
//! skipped with a note.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::locales::{brace_tokens, printf_tokens, valid_key, EnText, Kind, Meta, KEY_RULE};

pub const NOTIFY_EN: &str = "crates/agent/data/i18n/notify/en.json";
pub const NOTIFY_META: &str = "crates/agent/data/i18n/notify/_meta.json";
pub const CONSOLE_EN: &str = "crates/agent/data/i18n/console/en.json";
pub const CONSOLE_META: &str = "crates/agent/data/i18n/console/_meta.json";
pub const APP_EN: &str = "android/app/src/main/res/values/strings.xml";
pub const APP_META: &str = "android/app/src/main/res/i18n_meta.json";
pub const DESKTOP_EN: &str = "desktop/ui/i18n.json";
pub const DESKTOP_META: &str = "desktop/ui/i18n_meta.json";

/// Where a file's English and notes live in bageo-call.
pub fn source_paths(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Notify => (NOTIFY_EN, NOTIFY_META),
        Kind::Console => (CONSOLE_EN, CONSOLE_META),
        Kind::App => (APP_EN, APP_META),
        Kind::Desktop => (DESKTOP_EN, DESKTOP_META),
    }
}

/// What sync-source writes: (path under the packs folder, content), and
/// notes worth telling the person running it.
#[derive(Debug, Clone, Default)]
pub struct Synced {
    pub files: Vec<(String, String)>,
    pub notes: Vec<String>,
}

fn xml_unescape(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let r = &rest[i..];
        let j = r.find(';').ok_or("a bare &")?;
        let ent = &r[1..j];
        let c = match ent {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let n = if let Some(h) = ent.strip_prefix("#x") {
                    u32::from_str_radix(h, 16).ok()
                } else if let Some(d) = ent.strip_prefix('#') {
                    d.parse().ok()
                } else {
                    None
                };
                n.and_then(char::from_u32).ok_or_else(|| format!("unknown entity &{ent};"))?
            }
        };
        out.push(c);
        rest = &r[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Android's own escapes: backslash sequences, double quotes that keep white
/// space, and white space runs outside quotes folded to one space.
fn android_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut quoted, mut space) = (false, false);
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\\' => {
                space = false;
                match it.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let hex: String = it.by_ref().take(4).collect();
                        if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            out.push(ch);
                        }
                    }
                    Some(o) => out.push(o),
                    None => {}
                }
            }
            '"' => {
                quoted = !quoted;
                space = false;
            }
            c if c.is_whitespace() && !quoted => {
                if !space {
                    out.push(' ');
                }
                space = true;
            }
            c => {
                out.push(c);
                space = false;
            }
        }
    }
    out.trim_matches(' ').to_string()
}

fn attr<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("{key}=\"");
    let mut from = 0;
    while let Some(i) = tag[from..].find(&pat) {
        let at = from + i;
        if tag[..at].ends_with(char::is_whitespace) {
            let rest = &tag[at + pat.len()..];
            return rest.find('"').map(|j| &rest[..j]);
        }
        from = at + pat.len();
    }
    None
}

/// The <string> entries of an Android strings.xml, unescaped. Strings marked
/// translatable="false" are skipped; markup, plurals and arrays are refused.
pub fn parse_android_strings(xml: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("<!--") {
            let e = r.find("-->").ok_or("an unclosed comment")?;
            rest = &r[e + 3..];
            continue;
        }
        if rest.starts_with("<plurals") || rest.starts_with("<string-array") {
            return Err("plurals and string arrays are not supported yet".into());
        }
        let is_string = rest.strip_prefix("<string").is_some_and(|r| r.starts_with(|c: char| c.is_whitespace() || c == '>'));
        if !is_string {
            rest = &rest[1..];
            continue;
        }
        let end = rest.find('>').ok_or("an unclosed <string> tag")?;
        let tag = &rest[..end];
        let name = attr(tag, "name").ok_or("a <string> without a name")?.to_string();
        let keep = attr(tag, "translatable") != Some("false");
        if tag.ends_with('/') {
            if keep {
                out.insert(name, String::new());
            }
            rest = &rest[end + 1..];
            continue;
        }
        let body = &rest[end + 1..];
        let close = body.find("</string>").ok_or_else(|| format!("{name}: no </string>"))?;
        let inner = &body[..close];
        if inner.contains('<') {
            return Err(format!("{name}: markup inside a string is not supported"));
        }
        if keep && out.insert(name.clone(), android_unescape(&xml_unescape(inner)?)).is_some() {
            return Err(format!("{name}: defined twice"));
        }
        rest = &body[close + "</string>".len()..];
    }
    Ok(out)
}

fn read_english(kind: Kind, path: &Path) -> Result<EnText, String> {
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    match kind {
        Kind::Notify | Kind::Console => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Kind::App => {
            let xml = String::from_utf8(bytes).map_err(|e| e.to_string())?;
            Ok(EnText { schema: 1, strings: parse_android_strings(&xml)?, ..EnText::default() })
        }
        Kind::Desktop => {
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let en = v.get("en").and_then(Value::as_object).ok_or("no \"en\" object")?;
            let mut strings = BTreeMap::new();
            for (k, v) in en {
                strings.insert(k.clone(), v.as_str().ok_or_else(|| format!("{k}: not a string"))?.to_string());
            }
            Ok(EnText { schema: 1, strings, ..EnText::default() })
        }
    }
}

fn tokens(s: &str) -> Vec<String> {
    let mut v = brace_tokens(s).unwrap_or_default();
    v.extend(printf_tokens(s));
    v.sort();
    v
}

fn texts(t: &EnText) -> BTreeMap<&String, Vec<&String>> {
    let mut m: BTreeMap<&String, Vec<&String>> = BTreeMap::new();
    for (k, v) in &t.strings {
        m.entry(k).or_default().push(v);
    }
    for (k, v) in &t.lists {
        m.entry(k).or_default().extend(v);
    }
    for (k, v) in &t.formats {
        m.entry(k).or_default().push(v);
    }
    m
}

fn pretty<T: serde::Serialize>(v: &T) -> Result<String, String> {
    serde_json::to_string_pretty(v).map(|s| s + "\n").map_err(|e| e.to_string())
}

/// Keeps the "same_ok": true a maintainer set by hand in the notes already in
/// the packs folder (source/meta/<file>.json): bageo-call's notes may not
/// carry it, and a fresh export would drop it. Only for keys that are still
/// there. Returns the keys kept, as "<file>.<key>".
pub fn keep_same_ok(s: &mut Synced, packs: &Path) -> Vec<String> {
    let mut kept = Vec::new();
    for (rel, body) in s.files.iter_mut() {
        let Some(kind) = rel.strip_prefix("source/meta/").and_then(|f| f.strip_suffix(".json")) else { continue };
        let Ok(old) = fs::read(packs.join(rel.as_str())) else { continue };
        let Ok(old) = serde_json::from_slice::<BTreeMap<String, Value>>(&old) else { continue };
        let Ok(mut meta) = serde_json::from_str::<BTreeMap<String, Meta>>(body) else { continue };
        let mut changed = false;
        for (k, m) in meta.iter_mut() {
            if !m.same_ok && old.get(k).and_then(|v| v.get("same_ok")).and_then(Value::as_bool) == Some(true) {
                m.same_ok = true;
                changed = true;
                kept.push(format!("{kind}.{k}"));
            }
        }
        if changed {
            if let Ok(text) = pretty(&meta) {
                *body = text;
            }
        }
    }
    kept
}

/// Reads the English and notes under root (a bageo-call checkout). commit is
/// recorded as the English files' source.
pub fn sync_source(root: &Path, commit: Option<&str>) -> Result<Synced, Vec<String>> {
    let mut errs = Vec::new();
    let mut s = Synced::default();
    for kind in Kind::ALL {
        let (en_rel, meta_rel) = source_paths(kind);
        let en_path = root.join(en_rel);
        if !en_path.is_file() {
            if kind == Kind::Console {
                s.notes.push(format!("console: skipped; its English is not a data file yet ({CONSOLE_EN})"));
            } else {
                errs.push(format!("{en_rel}: missing"));
            }
            continue;
        }
        let mut text = match read_english(kind, &en_path) {
            Ok(t) => t,
            Err(e) => {
                errs.push(format!("{en_rel}: {e}"));
                continue;
            }
        };
        let bad: Vec<String> = text.keys().filter(|k| !valid_key(k)).cloned().collect();
        if !bad.is_empty() {
            errs.extend(bad.iter().map(|k| format!("{en_rel}: key {k:?}: {KEY_RULE}")));
            continue;
        }
        let meta_path = root.join(meta_rel);
        let meta: BTreeMap<String, Meta> = if meta_path.is_file() {
            match fs::read(&meta_path).map_err(|e| e.to_string()).and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string())) {
                Ok(m) => m,
                Err(e) => {
                    errs.push(format!("{meta_rel}: {e}"));
                    continue;
                }
            }
        } else if matches!(kind, Kind::App | Kind::Desktop) {
            s.notes.push(format!("{}: no {meta_rel} yet; the notes carry only the placeholders", kind.name()));
            texts(&text)
                .into_iter()
                .map(|(k, vs)| (k.clone(), Meta { placeholders: vs.first().map(|v| tokens(v)).unwrap_or_default(), ..Meta::default() }))
                .collect()
        } else {
            errs.push(format!("{meta_rel}: missing"));
            continue;
        };
        for (k, vs) in texts(&text) {
            match meta.get(k) {
                None => errs.push(format!("{meta_rel}: no note for \"{k}\"")),
                Some(m) => {
                    let mut want = m.placeholders.clone();
                    want.sort();
                    // Date formats carry strftime codes, which are not placeholders.
                    let found = |v: &str| if text.formats.contains_key(k) { brace_tokens(v).unwrap_or_default() } else { tokens(v) };
                    if vs.iter().any(|v| found(v) != want) {
                        s.notes.push(format!("{}: \"{k}\" has placeholders other than its note lists", kind.name()));
                    }
                }
            }
        }
        text.schema = 1;
        text.source = commit.map(str::to_string);
        match (pretty(&text), pretty(&meta)) {
            (Ok(en), Ok(me)) => {
                s.files.push((format!("source/en/{}.json", kind.name()), en));
                s.files.push((format!("source/meta/{}.json", kind.name()), me));
            }
            (Err(e), _) | (_, Err(e)) => errs.push(format!("{}: {e}", kind.name())),
        }
    }
    if errs.is_empty() {
        Ok(s)
    } else {
        Err(errs)
    }
}

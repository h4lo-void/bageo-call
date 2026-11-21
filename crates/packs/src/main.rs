// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! packs check <source folder>
//! packs build <source folder> --version <YYYYMMDDNN> --commit <text> --out <bageo-pack.json>
//!             [--index <bageo-pack-index.json>] [--min-bageo <x.y.z>]
//! packs build-locales <source folder> --version <YYYYMMDDNN> --commit <text> --out <bageo-locales.json.gz>
//!             [--index <bageo-pack-index.json>] [--built-at <time>] [--min-bageo <x.y.z>] [--check]
//! packs sync-source --bageo-call <checkout> --out <packs folder> [--commit <text>]
//! packs export-app --packs <packs folder> --android <res folder> --desktop <i18n.ext.json> [--commit <text>] [--check]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use bageo_packs::export::{apply_app_export, export_app};
use bageo_packs::sync::sync_source;
use bageo_packs::util::{now_rfc3339, valid_rfc3339_utc};
use bageo_packs::{
    build, build_locales_at, check_index, check_locales, check_locales_ref, check_sources, load_locales, load_sources, pack_locales,
    parse_bundle, parse_index, parse_locales, sanitize_locales, sha256_hex, sound_locales, unpack_locales, Index, Issue, LocalesRef,
    FORMAT, LOCALES_FORMAT, MAX_LOCALES_BYTES,
};

const USAGE: &str = "usage:
  packs check <source folder>
  packs build <source folder> --version <YYYYMMDDNN> --commit <text> --out <bageo-pack.json> [--index <bageo-pack-index.json>] [--min-bageo <x.y.z>]
  packs build-locales <source folder> --version <YYYYMMDDNN> --commit <text> --out <bageo-locales.json.gz> [--index <bageo-pack-index.json>] [--built-at <YYYY-MM-DDTHH:MM:SSZ>] [--min-bageo <x.y.z>] [--check]
  packs sync-source --bageo-call <checkout> --out <packs folder> [--commit <text>]
  packs export-app --packs <packs folder> --android <res folder> --desktop <i18n.ext.json> [--commit <text>] [--check]";

fn report(issues: &[Issue]) {
    for i in issues {
        println!("{i}");
    }
    println!("{} issue(s)", issues.len());
}

fn usage() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check") => cmd_check(&args[1..]),
        Some("build") => cmd_build(&args[1..]),
        Some("build-locales") => cmd_build_locales(&args[1..]),
        Some("sync-source") => cmd_sync_source(&args[1..]),
        Some("export-app") => cmd_export_app(&args[1..]),
        _ => usage(),
    }
}

#[derive(Default)]
struct Args {
    pos: Vec<String>,
    opts: BTreeMap<String, String>,
    flags: BTreeSet<String>,
}

fn parse_args(args: &[String], opts: &[&str], flags: &[&str]) -> Option<Args> {
    let mut a = Args::default();
    let mut it = args.iter();
    while let Some(x) = it.next() {
        if opts.contains(&x.as_str()) {
            let v = it.next()?;
            if a.opts.insert(x.clone(), v.clone()).is_some() {
                return None;
            }
        } else if flags.contains(&x.as_str()) {
            a.flags.insert(x.clone());
        } else if x.starts_with("--") {
            return None;
        } else {
            a.pos.push(x.clone());
        }
    }
    Some(a)
}

/// A commit as written into files and XML comments.
fn valid_commit_arg(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && !s.contains("--") && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn cmd_check(args: &[String]) -> ExitCode {
    let [dir] = args else { return usage() };
    let sources = match load_sources(Path::new(dir)) {
        Ok(s) => s,
        Err(issues) => {
            report(&issues);
            return ExitCode::from(1);
        }
    };
    let mut issues = check_sources(&sources);
    let locales = match load_locales(Path::new(dir)) {
        Ok(l) => l,
        Err(more) => {
            issues.extend(more);
            report(&issues);
            return ExitCode::from(1);
        }
    };
    if !locales.is_empty() {
        issues.extend(check_locales(&locales, None));
    }
    if issues.is_empty() {
        println!("ok: {} region(s), {} language layer(s)", sources.regions.len(), sources.layers.len());
        if !locales.is_empty() {
            println!("ok: {} translated language(s)", locales.langs.len());
        }
        ExitCode::SUCCESS
    } else {
        report(&issues);
        ExitCode::from(1)
    }
}

fn cmd_build(args: &[String]) -> ExitCode {
    let mut dir = None;
    let (mut version, mut commit, mut out, mut index) = (None, None, None, None);
    let mut min_bageo = env!("CARGO_PKG_VERSION").to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--version" => &mut version,
            "--commit" => &mut commit,
            "--out" => &mut out,
            "--index" => &mut index,
            "--min-bageo" => {
                let Some(v) = it.next() else { return usage() };
                min_bageo = v.clone();
                continue;
            }
            s if s.starts_with("--") => return usage(),
            _ => {
                if dir.replace(a.clone()).is_some() {
                    return usage();
                }
                continue;
            }
        };
        let Some(v) = it.next() else { return usage() };
        *slot = Some(v.clone());
    }
    let (Some(dir), Some(version), Some(commit), Some(out)) = (dir, version, commit, out) else { return usage() };
    let Ok(version) = version.parse::<u64>() else { return usage() };

    let sources = match load_sources(Path::new(&dir)) {
        Ok(s) => s,
        Err(issues) => {
            report(&issues);
            return ExitCode::from(1);
        }
    };
    let bundle = match build(&sources, version, &commit) {
        Ok(b) => b,
        Err(issues) => {
            report(&issues);
            return ExitCode::from(1);
        }
    };
    let json = match serde_json::to_string(&bundle) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("cannot write the bundle: {e}");
            return ExitCode::from(1);
        }
    };
    // What was built has to read back through the same door Bageo uses.
    if let Err(issues) = parse_bundle(&json) {
        report(&issues);
        return ExitCode::from(1);
    }
    if let Err(e) = fs::write(&out, json.as_bytes()) {
        eprintln!("cannot write {out}: {e}");
        return ExitCode::from(1);
    }
    let sha256 = sha256_hex(json.as_bytes());
    println!("built {out}: {} region(s), version {version}, sha256 {}", bundle.regions.len(), &sha256[..8]);

    if let Some(path) = index {
        let file = Path::new(&out).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        // Keep the translations entry build-locales wrote into an existing
        // index, when it is sound; a broken one is left out and said so.
        let locales = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Index>(&t).ok()).and_then(|ix| {
            let (kept, note) = sound_locales(&ix);
            if let Some(n) = note {
                println!("note: {n}");
            }
            kept
        });
        let ix = Index { format: FORMAT, version, file, bytes: json.len() as u64, sha256, min_bageo, locales };
        let issues = check_index(&ix);
        if !issues.is_empty() {
            report(&issues);
            return ExitCode::from(1);
        }
        let body = match serde_json::to_string(&ix) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("cannot write the index: {e}");
                return ExitCode::from(1);
            }
        };
        if let Err(e) = fs::write(&path, body.as_bytes()) {
            eprintln!("cannot write {path}: {e}");
            return ExitCode::from(1);
        }
        println!("wrote {path}");
    }
    ExitCode::SUCCESS
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::from(1)
}

fn cmd_build_locales(args: &[String]) -> ExitCode {
    let Some(a) = parse_args(args, &["--version", "--commit", "--out", "--index", "--built-at", "--min-bageo"], &["--check"]) else {
        return usage();
    };
    let ([dir], Some(version), Some(commit), Some(out)) =
        (a.pos.as_slice(), a.opts.get("--version"), a.opts.get("--commit"), a.opts.get("--out"))
    else {
        return usage();
    };
    let Ok(version) = version.parse::<u64>() else { return usage() };
    let built_at = a.opts.get("--built-at").cloned().unwrap_or_else(now_rfc3339);
    if !valid_rfc3339_utc(&built_at) {
        return usage();
    }
    let min_bageo = a.opts.get("--min-bageo").cloned().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());

    let sources = match load_locales(Path::new(dir)) {
        Ok(s) => s,
        Err(issues) => {
            report(&issues);
            return ExitCode::from(1);
        }
    };
    let built = match build_locales_at(&sources, version, commit, &built_at, None) {
        Ok(b) => b,
        Err(issues) => {
            report(&issues);
            return ExitCode::from(1);
        }
    };
    let gz = match pack_locales(&built) {
        Ok(g) => g,
        Err(e) => return fail(format!("cannot pack the translations: {e}")),
    };
    // What was built has to read back through Bageo's door without losing a key.
    match parse_locales(&gz) {
        Err(issues) => {
            report(&issues);
            return ExitCode::from(1);
        }
        Ok(mut back) => {
            let issues = sanitize_locales(&mut back, &sources.english, None);
            if !issues.is_empty() || back != built {
                report(&issues);
                return fail("the built file does not read back the same");
            }
        }
    }

    if a.flags.contains("--check") {
        let existing = match fs::read(out) {
            Ok(b) => b,
            Err(e) => return fail(format!("cannot read {out}: {e}")),
        };
        // Byte for byte: the same sources and time always pack the same way,
        // and anything appended to the file would otherwise go unseen.
        if existing != gz {
            if let Err(issues) = unpack_locales(&existing, MAX_LOCALES_BYTES) {
                report(&issues);
            }
            return fail(format!("{out} differs from a fresh build of {dir}"));
        }
        if let Some(path) = a.opts.get("--index") {
            let ix = fs::read_to_string(path).ok().and_then(|t| parse_index(&t).ok());
            let ok = ix.and_then(|ix| ix.locales).is_some_and(|l| {
                l.sha256 == sha256_hex(&existing) && l.bytes == existing.len() as u64 && l.version == version
            });
            if !ok {
                return fail(format!("{path} does not list {out} as it is"));
            }
        }
        println!("ok: {out} matches a fresh build ({} language(s))", built.langs.len());
        return ExitCode::SUCCESS;
    }

    let sha256 = sha256_hex(&gz);
    // Everything is checked, the index entry included, before any file is written.
    let index = match a.opts.get("--index") {
        None => None,
        Some(path) => {
            let file = Path::new(out).file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let r = LocalesRef { file, bytes: gz.len() as u64, sha256: sha256.clone(), format: LOCALES_FORMAT, version, min_bageo };
            let issues = check_locales_ref(&r);
            if !issues.is_empty() {
                report(&issues);
                return ExitCode::from(1);
            }
            match locales_index_body(path, &r) {
                Ok(body) => Some((path, body)),
                Err(e) => return fail(e),
            }
        }
    };
    if let Err(e) = fs::write(out, &gz) {
        return fail(format!("cannot write {out}: {e}"));
    }
    println!("built {out}: {} language(s), version {version}, {} bytes, sha256 {}", built.langs.len(), gz.len(), &sha256[..8]);
    if let Some((path, body)) = index {
        if let Err(e) = fs::write(path, body.as_bytes()) {
            return fail(format!("cannot write {path}: {e}"));
        }
        println!("wrote {path}");
    }
    ExitCode::SUCCESS
}

/// An existing index with its locales entry added or replaced, the rest kept.
fn locales_index_body(path: &str, r: &LocalesRef) -> Result<String, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let obj = v.as_object_mut().ok_or_else(|| format!("{path}: not a JSON object"))?;
    obj.insert("locales".to_string(), serde_json::to_value(r).map_err(|e| e.to_string())?);
    let body = serde_json::to_string(&v).map_err(|e| e.to_string())?;
    match parse_index(&body) {
        Ok(ix) if ix.locales.as_ref() == Some(r) => {}
        Ok(_) => return Err(format!("{path}: the locales entry does not read back")),
        Err(issues) => return Err(issues.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("\n")),
    }
    Ok(body)
}

fn cmd_sync_source(args: &[String]) -> ExitCode {
    let Some(a) = parse_args(args, &["--bageo-call", "--out", "--commit"], &[]) else { return usage() };
    let (true, Some(root), Some(out)) = (a.pos.is_empty(), a.opts.get("--bageo-call"), a.opts.get("--out")) else { return usage() };
    let commit = a.opts.get("--commit").map(String::as_str);
    if commit.is_some_and(|c| !valid_commit_arg(c)) {
        return usage();
    }
    let synced = match sync_source(Path::new(root), commit) {
        Ok(s) => s,
        Err(errs) => {
            for e in &errs {
                println!("{e}");
            }
            return fail(format!("{} problem(s); nothing written", errs.len()));
        }
    };
    for (rel, body) in &synced.files {
        let p = Path::new(out).join(rel);
        if let Some(d) = p.parent() {
            if let Err(e) = fs::create_dir_all(d) {
                return fail(format!("cannot create {}: {e}", d.display()));
            }
        }
        if let Err(e) = fs::write(&p, body.as_bytes()) {
            return fail(format!("cannot write {}: {e}", p.display()));
        }
        println!("wrote {rel}");
    }
    for n in &synced.notes {
        println!("note: {n}");
    }
    ExitCode::SUCCESS
}

fn cmd_export_app(args: &[String]) -> ExitCode {
    let Some(a) = parse_args(args, &["--packs", "--android", "--desktop", "--commit"], &["--check"]) else { return usage() };
    let (true, Some(packs), Some(res), Some(desktop)) =
        (a.pos.is_empty(), a.opts.get("--packs"), a.opts.get("--android"), a.opts.get("--desktop"))
    else {
        return usage();
    };
    let commit = a.opts.get("--commit").map(String::as_str);
    if commit.is_some_and(|c| !valid_commit_arg(c)) {
        return usage();
    }
    let sources = match load_locales(Path::new(packs)) {
        Ok(s) => s,
        Err(issues) => {
            report(&issues);
            return ExitCode::from(1);
        }
    };
    let e = export_app(&sources, commit);
    for i in &e.issues {
        println!("left out: {i}");
    }
    let check = a.flags.contains("--check");
    let changed = match apply_app_export(&e, Path::new(res), Path::new(desktop), check) {
        Ok(c) => c,
        Err(err) => return fail(format!("cannot write the app files: {err}")),
    };
    if check {
        if changed.is_empty() {
            println!("ok: app and desktop files match ({} language(s))", e.langs.len());
            return ExitCode::SUCCESS;
        }
        for c in &changed {
            println!("differs: {c}");
        }
        return fail(format!("{} file(s) differ; run packs export-app without --check", changed.len()));
    }
    for c in &changed {
        println!("wrote {c}");
    }
    println!("exported {} language(s)", e.langs.len());
    ExitCode::SUCCESS
}

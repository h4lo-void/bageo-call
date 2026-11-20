// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! packs check <source folder>
//! packs build <source folder> --version <YYYYMMDDNN> --commit <text> --out <bageo-pack.json>
//!             [--index <bageo-pack-index.json>] [--min-bageo <x.y.z>]

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use bageo_packs::{
    build, check_index, check_sources, load_sources, parse_bundle, sha256_hex, Index, Issue, FORMAT,
};

const USAGE: &str = "usage:
  packs check <source folder>
  packs build <source folder> --version <YYYYMMDDNN> --commit <text> --out <bageo-pack.json> [--index <bageo-pack-index.json>] [--min-bageo <x.y.z>]";

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
        _ => usage(),
    }
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
    let issues = check_sources(&sources);
    if issues.is_empty() {
        println!("ok: {} region(s), {} language layer(s)", sources.regions.len(), sources.layers.len());
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
        let ix = Index { format: FORMAT, version, file, bytes: json.len() as u64, sha256, min_bageo };
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

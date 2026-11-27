// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! `packs check` on the ambience/ folder and `packs build-ambience`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use bageo_packs::{sha256_hex, AMBIENCE_KINDS};

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
const BUILT: &str = "2026-11-27T00:00:00Z";

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bageo-ambience-cli-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap().flatten() {
        let target = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &target);
        } else {
            fs::copy(e.path(), target).unwrap();
        }
    }
}

/// A header and a little silence.
fn wav(extra: usize) -> Vec<u8> {
    let data = 2 * (8 + extra);
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&((36 + data) as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&[1, 0, 1, 0]);
    b.extend_from_slice(&16_000u32.to_le_bytes());
    b.extend_from_slice(&32_000u32.to_le_bytes());
    b.extend_from_slice(&[2, 0, 16, 0]);
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data as u32).to_le_bytes());
    b.resize(b.len() + data, 0);
    b
}

/// A valid source folder (the fixture) with the twelve beds in it.
fn source(name: &str, with_beds: bool) -> PathBuf {
    let d = tmp(name);
    copy_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/valid"), &d);
    if with_beds {
        fs::create_dir_all(d.join("ambience")).unwrap();
        for (i, k) in AMBIENCE_KINDS.iter().enumerate() {
            fs::write(d.join(format!("ambience/ambience_{k}.wav")), wav(i)).unwrap();
        }
    }
    d
}

fn packs() -> Command {
    Command::new(env!("CARGO_BIN_EXE_packs"))
}

fn run(args: &[&str]) -> (bool, String) {
    let out = packs().args(args).output().unwrap();
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
}

#[test]
fn check_passes_with_the_beds_and_without_the_folder() {
    let with = source("check-with", true);
    let (ok, text) = run(&["check", with.to_str().unwrap()]);
    assert!(ok, "{text}");
    let without = source("check-without", false);
    let (ok, text) = run(&["check", without.to_str().unwrap()]);
    assert!(ok, "{text}");
    let _ = fs::remove_dir_all(with);
    let _ = fs::remove_dir_all(without);
}

#[test]
fn check_refuses_a_missing_extra_empty_or_not_wav_file() {
    for (name, damage) in [
        ("missing", 0),
        ("extra", 1),
        ("empty", 2),
        ("notwav", 3),
    ] {
        let d = source(&format!("check-{name}"), true);
        let beds = d.join("ambience");
        match damage {
            0 => fs::remove_file(beds.join("ambience_office.wav")).unwrap(),
            1 => fs::write(beds.join("ambience_cafe.wav"), wav(1)).unwrap(),
            2 => fs::write(beds.join("ambience_office.wav"), b"").unwrap(),
            _ => fs::write(beds.join("ambience_office.wav"), vec![1u8; 100]).unwrap(),
        }
        let (ok, text) = run(&["check", d.to_str().unwrap()]);
        assert!(!ok, "{name}: {text}");
        assert!(text.contains("ambience/ambience_"), "{name}: {text}");
        let _ = fs::remove_dir_all(d);
    }
}

#[test]
fn build_ambience_writes_the_files_and_an_index_that_matches_them() {
    let d = source("build", true);
    let out = tmp("build-out");
    let (ok, text) = run(&[
        "build-ambience",
        d.to_str().unwrap(),
        "--version",
        "2026112701",
        "--commit",
        COMMIT,
        "--out",
        out.to_str().unwrap(),
        "--built-at",
        BUILT,
    ]);
    assert!(ok, "{text}");
    assert!(text.contains("built 12 background sound(s)"), "{text}");
    let index: serde_json::Value = serde_json::from_slice(&fs::read(out.join("bageo-ambience-index.json")).unwrap()).unwrap();
    assert_eq!(index["format"], 1);
    assert_eq!(index["version"], "2026112701");
    assert_eq!(index["commit"], COMMIT);
    assert_eq!(index["built_at"], BUILT);
    let beds = index["beds"].as_array().unwrap();
    assert_eq!(beds.len(), 12);
    for (i, k) in AMBIENCE_KINDS.iter().enumerate() {
        let file = format!("ambience_{k}.wav");
        let body = fs::read(out.join(&file)).unwrap();
        assert_eq!(body, wav(i));
        assert_eq!(beds[i]["kind"], *k);
        assert_eq!(beds[i]["file"], file);
        assert_eq!(beds[i]["bytes"], body.len());
        assert_eq!(beds[i]["sha256"], sha256_hex(&body));
    }
    assert_eq!(fs::read_dir(&out).unwrap().count(), 13);
    let _ = fs::remove_dir_all(d);
    let _ = fs::remove_dir_all(out);
}

#[test]
fn build_ambience_writes_nothing_when_a_bed_is_bad_or_the_options_are_wrong() {
    let d = source("build-bad", true);
    fs::write(d.join("ambience/ambience_car.wav"), b"text").unwrap();
    let out = tmp("build-bad-out");
    let o = out.to_str().unwrap();
    let (ok, text) = run(&["build-ambience", d.to_str().unwrap(), "--version", "2026112701", "--commit", COMMIT, "--out", o]);
    assert!(!ok && text.contains("ambience_car.wav"), "{text}");
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    fs::write(d.join("ambience/ambience_car.wav"), wav(3)).unwrap();
    for args in [
        vec!["build-ambience", d.to_str().unwrap(), "--commit", COMMIT, "--out", o],
        vec!["build-ambience", d.to_str().unwrap(), "--version", "12", "--commit", COMMIT, "--out", o],
        vec!["build-ambience", d.to_str().unwrap(), "--version", "2026112701", "--commit", "bad commit", "--out", o],
        vec!["build-ambience", d.to_str().unwrap(), "--version", "2026112701", "--commit", COMMIT, "--out", o, "--built-at", "yesterday"],
    ] {
        let (ok, text) = run(&args);
        assert!(!ok, "{args:?}: {text}");
    }
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    let _ = fs::remove_dir_all(d);
    let _ = fs::remove_dir_all(out);
}

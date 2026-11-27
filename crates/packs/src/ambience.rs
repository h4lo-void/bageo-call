// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! The background sounds ("beds"): twelve WAV files in `ambience/` of the
//! source folder, `ambience_<kind>.wav`. They are not built into Bageo; a
//! release carries them next to an index, `bageo-ambience-index.json`, and
//! Bageo downloads one when its owner picks or previews it. The kinds are the
//! same as `KINDS` in the agent's `ambience.rs`.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::util::sha256_hex;
use crate::{rule, Issue};

/// The folder in the source tree.
pub const AMBIENCE_DIR: &str = "ambience";
/// The list of beds in a release.
pub const AMBIENCE_INDEX_FILE: &str = "bageo-ambience-index.json";
/// The index format.
pub const AMBIENCE_FORMAT: u64 = 1;
/// One bed file is refused above this size.
pub const MAX_BED_BYTES: u64 = 8 << 20;
/// The beds, in the order the index lists them.
pub const AMBIENCE_KINDS: [&str; 12] = [
    "office",
    "home",
    "street",
    "hospital",
    "ambulance",
    "typhoon",
    "storm",
    "construction",
    "mrt",
    "nightmarket",
    "firecrackers",
    "car",
];

/// The file name of a bed.
pub fn bed_file(kind: &str) -> String {
    format!("ambience_{kind}.wav")
}

#[derive(Debug, Clone, Serialize)]
pub struct BedRef {
    pub kind: String,
    pub file: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AmbienceIndex {
    pub format: u64,
    pub version: String,
    pub commit: String,
    pub built_at: String,
    pub beds: Vec<BedRef>,
}

/// What `build-ambience` writes: the bed files (name, bytes) and the index.
pub struct AmbienceBuild {
    pub files: Vec<(String, Vec<u8>)>,
    pub index: AmbienceIndex,
    pub index_json: String,
}

/// A RIFF/WAVE file: the header and room for at least one chunk.
fn is_wav(b: &[u8]) -> bool {
    b.len() >= 44 && &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE"
}

fn check_bed_bytes(shown: &str, b: &[u8], issues: &mut Vec<Issue>) {
    if b.is_empty() {
        issues.push(Issue::new(shown, "", rule::LENGTH, "the file is empty"));
    } else if b.len() as u64 > MAX_BED_BYTES {
        issues.push(Issue::new(shown, "", rule::LENGTH, format!("{} bytes is more than the {MAX_BED_BYTES} allowed", b.len())));
    } else if !is_wav(b) {
        issues.push(Issue::new(shown, "", rule::FORMAT, "not a RIFF/WAVE file"));
    }
}

/// Reads `<source>/ambience/`: every bed present and valid, nothing else in
/// the folder. Returns the beds read (in kind order) and what was wrong.
fn read_beds(source: &Path) -> (Vec<(&'static str, Vec<u8>)>, Vec<Issue>) {
    let dir = source.join(AMBIENCE_DIR);
    let mut issues = Vec::new();
    let mut beds = Vec::new();
    let expected: BTreeSet<String> = AMBIENCE_KINDS.iter().map(|k| bed_file(k)).collect();
    match fs::read_dir(&dir) {
        Err(e) => issues.push(Issue::new(AMBIENCE_DIR, "", rule::FORMAT, format!("cannot read the folder: {e}"))),
        Ok(entries) => {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !expected.contains(&name) {
                    issues.push(Issue::new(&format!("{AMBIENCE_DIR}/{name}"), "", rule::CODE, "not one of the background sounds"));
                }
            }
        }
    }
    for kind in AMBIENCE_KINDS {
        let name = bed_file(kind);
        let shown = format!("{AMBIENCE_DIR}/{name}");
        let path = dir.join(&name);
        match fs::symlink_metadata(&path) {
            Err(_) => issues.push(Issue::new(&shown, "", rule::REQUIRED, "missing")),
            Ok(m) if !m.is_file() => issues.push(Issue::new(&shown, "", rule::FORMAT, "not a regular file")),
            Ok(m) if m.len() > MAX_BED_BYTES => {
                issues.push(Issue::new(&shown, "", rule::LENGTH, format!("{} bytes is more than the {MAX_BED_BYTES} allowed", m.len())))
            }
            Ok(_) => match fs::read(&path) {
                Err(e) => issues.push(Issue::new(&shown, "", rule::FORMAT, format!("cannot read: {e}"))),
                Ok(b) => {
                    let before = issues.len();
                    check_bed_bytes(&shown, &b, &mut issues);
                    if issues.len() == before {
                        beds.push((kind, b));
                    }
                }
            },
        }
    }
    (beds, issues)
}

/// The checks of `packs check` for `<source>/ambience/`. A source without that
/// folder (an older bageo-packs) passes.
pub fn check_ambience(source: &Path) -> Vec<Issue> {
    if !source.join(AMBIENCE_DIR).exists() {
        return Vec::new();
    }
    read_beds(source).1
}

fn valid_version(v: &str) -> bool {
    v.len() == 10 && v.bytes().all(|b| b.is_ascii_digit())
}

/// The beds of `<source>/ambience/` and their index, checked and ready to write.
pub fn build_ambience(source: &Path, version: &str, commit: &str, built_at: &str) -> Result<AmbienceBuild, Vec<Issue>> {
    if !valid_version(version) {
        return Err(vec![Issue::new(AMBIENCE_INDEX_FILE, "version", rule::STATUS, "must be YYYYMMDDNN")]);
    }
    let (beds, issues) = read_beds(source);
    if !issues.is_empty() {
        return Err(issues);
    }
    let mut refs = Vec::new();
    let mut files = Vec::new();
    for (kind, bytes) in beds {
        let file = bed_file(kind);
        refs.push(BedRef { kind: kind.to_string(), file: file.clone(), bytes: bytes.len() as u64, sha256: sha256_hex(&bytes) });
        files.push((file, bytes));
    }
    let index = AmbienceIndex {
        format: AMBIENCE_FORMAT,
        version: version.to_string(),
        commit: commit.to_string(),
        built_at: built_at.to_string(),
        beds: refs,
    };
    let mut index_json = serde_json::to_string_pretty(&index).map_err(|e| vec![Issue::new(AMBIENCE_INDEX_FILE, "", rule::FORMAT, e.to_string())])?;
    index_json.push('\n');
    Ok(AmbienceBuild { files, index, index_json })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest valid WAV: a header and a few samples of silence.
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

    fn source(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("bageo-ambience-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join(AMBIENCE_DIR)).unwrap();
        for (i, k) in AMBIENCE_KINDS.iter().enumerate() {
            fs::write(d.join(AMBIENCE_DIR).join(bed_file(k)), wav(i)).unwrap();
        }
        d
    }

    const AT: &str = "2026-11-27T00:00:00Z";

    #[test]
    fn a_full_folder_builds_an_index_that_matches_its_files() {
        let d = source("full");
        assert!(check_ambience(&d).is_empty());
        let b = build_ambience(&d, "2026112701", "abc123", AT).unwrap();
        assert_eq!(b.files.len(), 12);
        let v: serde_json::Value = serde_json::from_str(&b.index_json).unwrap();
        assert_eq!(v["format"], 1);
        assert_eq!(v["version"], "2026112701");
        assert_eq!(v["commit"], "abc123");
        assert_eq!(v["built_at"], AT);
        let beds = v["beds"].as_array().unwrap();
        assert_eq!(beds.len(), 12);
        for (i, k) in AMBIENCE_KINDS.iter().enumerate() {
            let body = &b.files[i].1;
            assert_eq!(beds[i]["kind"], *k);
            assert_eq!(beds[i]["file"], format!("ambience_{k}.wav"));
            assert_eq!(beds[i]["bytes"], body.len());
            assert_eq!(beds[i]["sha256"], sha256_hex(body));
        }
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_source_without_the_folder_passes_but_cannot_build() {
        let d = std::env::temp_dir().join(format!("bageo-ambience-{}-none", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        assert!(check_ambience(&d).is_empty());
        assert!(build_ambience(&d, "2026112701", "abc", AT).is_err());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_missing_empty_long_unknown_or_not_wav_file_is_refused() {
        let d = source("bad");
        let dir = d.join(AMBIENCE_DIR);
        fs::remove_file(dir.join("ambience_home.wav")).unwrap();
        fs::write(dir.join("ambience_car.wav"), b"").unwrap();
        fs::write(dir.join("ambience_mrt.wav"), vec![b'x'; 200]).unwrap();
        fs::write(dir.join("ambience_storm.wav"), vec![0u8; MAX_BED_BYTES as usize + 1]).unwrap();
        fs::write(dir.join("extra.wav"), wav(1)).unwrap();
        fs::write(dir.join("ambience_cafe.wav"), wav(1)).unwrap();
        let issues = check_ambience(&d);
        let text: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        for needle in ["ambience_home.wav: [R05-required]", "ambience_car.wav: [R06-length]", "ambience_mrt.wav: [R01-format]", "ambience_storm.wav: [R06-length]", "extra.wav: [R02-code]", "ambience_cafe.wav: [R02-code]"] {
            assert!(text.iter().any(|t| t.contains(needle)), "{needle} in {text:?}");
        }
        assert_eq!(issues.len(), 6, "{text:?}");
        assert!(build_ambience(&d, "2026112701", "abc", AT).is_err());
        let _ = fs::remove_dir_all(d);
    }

    #[test]
    fn a_bad_version_is_refused() {
        let d = source("version");
        for v in ["", "1", "20261127", "202611270a", "20261127011"] {
            assert!(build_ambience(&d, v, "abc", AT).is_err(), "{v}");
        }
        let _ = fs::remove_dir_all(d);
    }
}

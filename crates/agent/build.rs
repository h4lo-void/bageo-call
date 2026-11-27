// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Puts a build id into the program (`BAGEO_BUILD`), so `/health` and the
//! console can tell which build is running when the version number is the same:
//! the short git commit (`-dirty` when the files differ from it); without git
//! (a Docker build, a source download) the `BAGEO_BUILD_ID` environment
//! variable; else `unknown`.

use std::path::Path;
use std::process::Command;

use serde_json::{Map, Value};

/// The longest id kept, so a stray value cannot grow the health answer.
const MAX_LEN: usize = 32;

const CONSOLE_META_REL: &str = "static/i18n/_meta.json";
const NOTIFY_META_REL: &str = "data/i18n/notify/_meta.json";

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let dir = Path::new(&dir);
    let id = resolve(git_build_id(dir), std::env::var("BAGEO_BUILD_ID").ok());
    println!("cargo:rustc-env=BAGEO_BUILD={id}");
    println!("cargo:rerun-if-env-changed=BAGEO_BUILD_ID");
    // Edits to the program's own files change the `-dirty` mark; a commit or a
    // staging change moves the git files.
    for p in ["build.rs", "Cargo.toml", "src", "static", "data", CONSOLE_META_REL, NOTIFY_META_REL] {
        println!("cargo:rerun-if-changed={p}");
    }
    for name in ["HEAD", "index"] {
        if let Some(path) = git(dir, &["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={}", dir.join(path).display());
        }
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR must be set");
    let out_dir = Path::new(&out_dir);
    write_slim_meta(&dir.join(CONSOLE_META_REL), &out_dir.join("console_meta_slim.json"));
    write_slim_meta(&dir.join(NOTIFY_META_REL), &out_dir.join("notify_meta_slim.json"));
}

fn write_slim_meta(src: &Path, dst: &Path) {
    let raw = std::fs::read_to_string(src)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", src.display()));
    let slim = slim_meta_json(&raw)
        .unwrap_or_else(|e| panic!("failed to slim {}: {e}", src.display()));
    if std::fs::read_to_string(dst).ok().as_deref() != Some(slim.as_str()) {
        std::fs::write(dst, slim.as_bytes())
            .unwrap_or_else(|e| panic!("failed to write {}: {e}", dst.display()));
    }
}

/// Strip translator-only fields (`where`, `who`) from a `_meta.json` file,
/// keeping only runtime validation fields (`placeholders`, `max`, `same_ok`)
/// in compact JSON. Empty `placeholders`, null `max`, and `same_ok: false`
/// are omitted since `bageo_packs::locales::Meta` defaults them on read.
fn slim_meta_json(raw: &str) -> Result<String, String> {
    let root: Value = serde_json::from_str(raw).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = root.as_object().ok_or_else(|| "meta root must be a JSON object".to_string())?;
    let mut out = Map::with_capacity(obj.len());
    for (key, entry) in obj {
        let entry_obj = entry
            .as_object()
            .ok_or_else(|| format!("entry '{key}' must be a JSON object"))?;
        let mut slim = Map::new();
        if let Some(ph) = entry_obj.get("placeholders") {
            let arr = ph
                .as_array()
                .ok_or_else(|| format!("entry '{key}' placeholders must be an array"))?;
            if !arr.is_empty() {
                slim.insert("placeholders".to_string(), Value::Array(arr.clone()));
            }
        }
        if let Some(max) = entry_obj.get("max").filter(|v| !v.is_null()) {
            slim.insert("max".to_string(), max.clone());
        }
        if entry_obj.get("same_ok").and_then(Value::as_bool) == Some(true) {
            slim.insert("same_ok".to_string(), Value::Bool(true));
        }
        out.insert(key.clone(), Value::Object(slim));
    }
    serde_json::to_string(&Value::Object(out)).map_err(|e| format!("failed to serialize slim meta: {e}"))
}

/// The id to use: git's when there is one, else the environment variable's.
fn resolve(from_git: Option<String>, from_env: Option<String>) -> String {
    from_git
        .or_else(|| from_env.as_deref().and_then(clean_env_id))
        .unwrap_or_else(|| "unknown".to_string())
}

/// A value given by hand or by a workflow: only letters, digits, `.`, `_` and
/// `-`; a full 40-character commit hash is shortened to 7 like git's own.
fn clean_env_id(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() == 40 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(raw[..7].to_ascii_lowercase());
    }
    let id: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(MAX_LEN)
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// The short commit of the repository this program's source sits in, with
/// `-dirty` after it when tracked files have uncommitted changes. `None` when
/// git is missing, this is no checkout, or the checkout belongs to a folder
/// above (a source download unpacked inside some other repository).
fn git_build_id(dir: &Path) -> Option<String> {
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    let here = dir.canonicalize().ok()?;
    if Path::new(&top).join("crates").join("agent").canonicalize().ok()? != here {
        return None;
    }
    let sha = git(dir, &["rev-parse", "--short=7", "HEAD"])?;
    if sha.is_empty() || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let dirty = git(dir, &["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    Some(if dirty { format!("{sha}-dirty") } else { sha })
}

/// Run git in `dir`; its trimmed output when it succeeds.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).current_dir(dir).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// The agent's own tests compile this file too (see `main.rs`), so the rules
// above are tested with `cargo test -p agent`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_wins_over_the_variable() {
        assert_eq!(resolve(Some("a1b2c3d".into()), Some("from-env".into())), "a1b2c3d");
        assert_eq!(resolve(Some("a1b2c3d-dirty".into()), None), "a1b2c3d-dirty");
    }

    #[test]
    fn without_git_the_variable_is_used() {
        assert_eq!(resolve(None, Some("release-2026.11_a".into())), "release-2026.11_a");
        assert_eq!(resolve(None, Some("  a1b2c3d \n".into())), "a1b2c3d");
        // A workflow hands over the whole commit hash; it is shortened like git does.
        let full = "0123456789ABCDEF0123456789abcdef01234567";
        assert_eq!(resolve(None, Some(full.into())), "0123456");
    }

    #[test]
    fn the_variable_cannot_smuggle_odd_text_or_length() {
        assert_eq!(resolve(None, Some("v1 \"x\"/<b>".into())), "v1xb");
        assert_eq!(resolve(None, Some("a".repeat(100))).len(), MAX_LEN);
    }

    #[test]
    fn nothing_known_is_unknown() {
        assert_eq!(resolve(None, None), "unknown");
        assert_eq!(resolve(None, Some("".into())), "unknown");
        assert_eq!(resolve(None, Some("  /// ".into())), "unknown");
    }

    #[test]
    fn a_folder_outside_any_checkout_has_no_git_id() {
        let dir = std::env::temp_dir().join(format!("bageo-build-id-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(git_build_id(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_checkout_id_is_a_short_commit_with_an_optional_dirty_mark() {
        // Not a checkout (a source download): nothing to check.
        let Some(id) = git_build_id(Path::new(env!("CARGO_MANIFEST_DIR"))) else { return };
        let sha = id.strip_suffix("-dirty").unwrap_or(&id);
        assert_eq!(sha.len(), 7, "{id}");
        assert!(sha.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    }

    #[test]
    fn slim_meta_strips_translator_notes_and_keeps_validation_fields() {
        let sample = r#"{
            "a_plain": {"where": "long description here", "who": "the owner", "placeholders": []},
            "b_ph": {"where": "with ph", "who": "the caller", "placeholders": ["{number}", "{count}"]},
            "c_max": {"where": "button", "who": "the owner", "placeholders": [], "max": 18},
            "d_same": {"where": "code", "who": "the owner", "placeholders": ["{x}"], "same_ok": true}
        }"#;
        let slim = slim_meta_json(sample).unwrap();
        assert!(!slim.contains("long description here"));
        assert!(!slim.contains("the owner"));
        assert!(!slim.contains("the caller"));
        assert!(!slim.contains("\"where\""));
        assert!(!slim.contains("\"who\""));
        let parsed: Value = serde_json::from_str(&slim).unwrap();
        assert_eq!(parsed["a_plain"], serde_json::json!({}));
        assert_eq!(parsed["b_ph"], serde_json::json!({"placeholders": ["{number}", "{count}"]}));
        assert_eq!(parsed["c_max"], serde_json::json!({"max": 18}));
        assert_eq!(parsed["d_same"], serde_json::json!({"placeholders": ["{x}"], "same_ok": true}));
    }

    #[test]
    fn slim_meta_on_repo_files_preserves_all_keys_and_shrinks_dramatically() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        for rel in [CONSOLE_META_REL, NOTIFY_META_REL] {
            let raw = std::fs::read_to_string(dir.join(rel)).unwrap();
            let slim = slim_meta_json(&raw).unwrap();
            assert!(slim.len() * 4 < raw.len(), "{rel}: {} vs {}", slim.len(), raw.len());
            let full_val: Value = serde_json::from_str(&raw).unwrap();
            let slim_val: Value = serde_json::from_str(&slim).unwrap();
            let full_obj = full_val.as_object().unwrap();
            let slim_obj = slim_val.as_object().unwrap();
            assert_eq!(slim_obj.len(), full_obj.len(), "{rel}: key count");
            for (k, full_entry) in full_obj {
                let slim_entry = slim_obj.get(k).unwrap_or_else(|| panic!("{rel}: missing key {k}"));
                let full_ph = full_entry.get("placeholders").and_then(Value::as_array).cloned().unwrap_or_default();
                let slim_ph = slim_entry.get("placeholders").and_then(Value::as_array).cloned().unwrap_or_default();
                assert_eq!(slim_ph, full_ph, "{rel}: placeholders mismatch on {k}");
                assert_eq!(slim_entry.get("max"), full_entry.get("max"), "{rel}: max mismatch on {k}");
                let full_same = full_entry.get("same_ok").and_then(Value::as_bool).unwrap_or(false);
                let slim_same = slim_entry.get("same_ok").and_then(Value::as_bool).unwrap_or(false);
                assert_eq!(slim_same, full_same, "{rel}: same_ok mismatch on {k}");
                assert!(slim_entry.get("where").is_none() && slim_entry.get("who").is_none(), "{rel}: notes leaked on {k}");
            }
        }
    }

    #[test]
    fn slim_meta_rejects_invalid_inputs() {
        assert!(slim_meta_json("not json").is_err());
        assert!(slim_meta_json("[]").is_err());
        assert!(slim_meta_json(r#"{"k": 123}"#).is_err());
        assert!(slim_meta_json(r#"{"k": {"placeholders": "bad"}}"#).is_err());
    }
}

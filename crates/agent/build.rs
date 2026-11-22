// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Puts a build id into the program (`BAGEO_BUILD`), so `/health` and the
//! console can tell which build is running when the version number is the same:
//! the short git commit (`-dirty` when the files differ from it); without git
//! (a Docker build, a source download) the `BAGEO_BUILD_ID` environment
//! variable; else `unknown`.

use std::path::Path;
use std::process::Command;

/// The longest id kept, so a stray value cannot grow the health answer.
const MAX_LEN: usize = 32;

fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let dir = Path::new(&dir);
    let id = resolve(git_build_id(dir), std::env::var("BAGEO_BUILD_ID").ok());
    println!("cargo:rustc-env=BAGEO_BUILD={id}");
    println!("cargo:rerun-if-env-changed=BAGEO_BUILD_ID");
    // Edits to the program's own files change the `-dirty` mark; a commit or a
    // staging change moves the git files.
    for p in ["build.rs", "Cargo.toml", "src", "static", "data"] {
        println!("cargo:rerun-if-changed={p}");
    }
    for name in ["HEAD", "index"] {
        if let Some(path) = git(dir, &["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={}", dir.join(path).display());
        }
    }
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
}

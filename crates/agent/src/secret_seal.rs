// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Local at-rest encryption for credentials stored in `config/settings.json`.
//!
//! Each non-empty secret value is sealed with `XChaCha20-Poly1305` (24-byte random
//! nonce, 16-byte authentication tag) and bound to its setting field name via AEAD
//! associated data (`"bageo-setting-v1:<field>"`), so ciphertext cannot be copied
//! across fields.
//!
//! Key material:
//! - When `BAGEO_MASTER_KEY` is set in the environment (non-empty), the 32-byte key
//!   is derived deterministically with SHA-256 so headless / container deployments
//!   need not keep any key file on disk.
//! - Otherwise a 32-byte random key is generated once and stored next to
//!   `settings.json` as `.settings.key` (`0600` on Unix; dot-prefixed scratch name
//!   so backups never export it). Backups receive unsealed values in memory from
//!   `SettingsStore::overrides_json` and apply their own `.bageo` encryption.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XNonce, XChaCha20Poly1305};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "enc:v1:";
const KEY_FILE_NAME: &str = ".settings.key";
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// True when `s` carries the sealed prefix written by [`seal_str`].
pub fn is_sealed(s: &str) -> bool {
    s.starts_with(PREFIX)
}

fn key_path_for(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(KEY_FILE_NAME)
}

fn env_master_key() -> Option<[u8; KEY_LEN]> {
    let raw = std::env::var("BAGEO_MASTER_KEY").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut h = Sha256::new();
    h.update(b"bageo-settings-master-key-v1:");
    h.update(trimmed.as_bytes());
    let digest = h.finalize();
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(&digest);
    Some(out)
}

fn read_key_file(path: &Path) -> Option<[u8; KEY_LEN]> {
    let raw = std::fs::read_to_string(path).ok()?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw.trim())
        .ok()?;
    if decoded.len() != KEY_LEN {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    let mut out = [0u8; KEY_LEN];
    out.copy_from_slice(&decoded);
    Some(out)
}

fn write_key_file(path: &Path, key: &[u8; KEY_LEN]) -> Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create {}", dir.display()))?;
    }
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
    let tmp = path.with_extension("key.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("open {}", tmp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(encoded.as_bytes())?;
        f.sync_all()?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename to {}", path.display()));
    }
    Ok(())
}

/// Load the local sealing key for `settings_path`, creating `.settings.key` when
/// `create` is true and none exists yet.
pub fn load_or_create_key(settings_path: &Path, create: bool) -> Result<Option<[u8; KEY_LEN]>> {
    if let Some(k) = env_master_key() {
        return Ok(Some(k));
    }
    let kpath = key_path_for(settings_path);
    if let Some(k) = read_key_file(&kpath) {
        return Ok(Some(k));
    }
    if !create {
        return Ok(None);
    }
    let mut key = [0u8; KEY_LEN];
    if getrandom::getrandom(&mut key).is_err() {
        let mut h = Sha256::new();
        for _ in 0..4 {
            h.update(uuid::Uuid::new_v4().as_bytes());
        }
        key.copy_from_slice(&h.finalize());
    }
    write_key_file(&kpath, &key)?;
    Ok(Some(key))
}

fn aad_for(field: &str) -> Vec<u8> {
    format!("bageo-setting-v1:{field}").into_bytes()
}

/// Seal a non-empty secret value under `key` and `field` AAD. Empty strings stay empty.
pub fn seal_str(key: &[u8; KEY_LEN], field: &str, plain: &str) -> Result<String> {
    if plain.is_empty() {
        return Ok(String::new());
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    if getrandom::getrandom(&mut nonce).is_err() {
        let mut h = Sha256::new();
        h.update(uuid::Uuid::new_v4().as_bytes());
        h.update(uuid::Uuid::new_v4().as_bytes());
        nonce.copy_from_slice(&h.finalize()[..NONCE_LEN]);
    }
    let aad = aad_for(field);
    let ct = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plain.as_bytes(),
                aad: &aad,
            },
        )
        .map_err(|_| anyhow::anyhow!("failed to seal {field}"))?;
    let mut combined = Vec::with_capacity(NONCE_LEN + ct.len());
    combined.extend_from_slice(&nonce);
    combined.extend_from_slice(&ct);
    Ok(format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(combined)
    ))
}

/// Unseal a value produced by [`seal_str`]. Non-prefixed legacy strings are returned
/// verbatim with `was_plain = true` (when non-empty) so the caller can upgrade on disk.
pub fn unseal_str(
    key: Option<&[u8; KEY_LEN]>,
    field: &str,
    stored: &str,
) -> Result<(String, bool)> {
    let Some(rest) = stored.strip_prefix(PREFIX) else {
        let was_plain = !stored.trim().is_empty();
        return Ok((stored.to_string(), was_plain));
    };
    let key = key.ok_or_else(|| anyhow::anyhow!("missing settings key file for sealed {field}"))?;
    let combined = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(rest.trim())
        .with_context(|| format!("invalid base64 in sealed {field}"))?;
    if combined.len() < NONCE_LEN + 16 {
        anyhow::bail!("truncated sealed value for {field}");
    }
    let (nonce, ct) = combined.split_at(NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let aad = aad_for(field);
    let pt = cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload { msg: ct, aad: &aad },
        )
        .map_err(|_| anyhow::anyhow!("authentication failed unsealing {field}"))?;
    let s = String::from_utf8(pt).with_context(|| format!("non-utf8 plaintext in {field}"))?;
    Ok((s, false))
}

/// Seal every non-empty credential field and channel token in `v` (an `Overrides` JSON object)
/// before writing `settings.json` to disk.
pub fn seal_overrides_json(settings_path: &Path, v: &mut Value, secret_fields: &[&str]) -> Result<()> {
    let Some(obj) = v.as_object_mut() else {
        return Ok(());
    };
    let has_any = secret_fields.iter().any(|f| {
        obj.get(*f)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && !is_sealed(s))
    }) || obj
        .get("notify_channels")
        .and_then(Value::as_array)
        .is_some_and(|arr| {
            arr.iter().any(|ch| {
                ["token", "app_token"].iter().any(|k| {
                    ch.get(*k)
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty() && !is_sealed(s))
                })
            })
        });
    if !has_any {
        return Ok(());
    }
    let Some(key) = load_or_create_key(settings_path, true)? else {
        return Ok(());
    };
    for &field in secret_fields {
        if let Some(val) = obj.get_mut(field) {
            if let Some(s) = val.as_str() {
                if !s.is_empty() && !is_sealed(s) {
                    *val = Value::String(seal_str(&key, field, s)?);
                }
            }
        }
    }
    if let Some(arr) = obj.get_mut("notify_channels").and_then(Value::as_array_mut) {
        for ch in arr.iter_mut() {
            if let Some(cobj) = ch.as_object_mut() {
                for sub in ["token", "app_token"] {
                    let aad_name = format!("notify_channels.{sub}");
                    if let Some(val) = cobj.get_mut(sub) {
                        if let Some(s) = val.as_str() {
                            if !s.is_empty() && !is_sealed(s) {
                                *val = Value::String(seal_str(&key, &aad_name, s)?);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Unseal any `enc:v1:` fields in `v` (loaded from `settings_path`). Returns `true`
/// if at least one non-empty secret was still stored in legacy plaintext on disk.
pub fn unseal_overrides_json(settings_path: Option<&Path>, v: &mut Value, secret_fields: &[&str]) -> bool {
    let Some(obj) = v.as_object_mut() else {
        return false;
    };
    let key = env_master_key()
        .or_else(|| settings_path.and_then(|p| load_or_create_key(p, false).ok().flatten()));
    let mut saw_plain = false;
    for &field in secret_fields {
        if let Some(val) = obj.get_mut(field) {
            if let Some(s) = val.as_str() {
                match unseal_str(key.as_ref(), field, s) {
                    Ok((plain, was_plain)) => {
                        if was_plain {
                            saw_plain = true;
                        }
                        *val = Value::String(plain);
                    }
                    Err(e) => {
                        tracing::warn!("settings: could not unseal {field} ({e}); clearing unreadable value");
                        *val = Value::String(String::new());
                    }
                }
            }
        }
    }
    if let Some(arr) = obj.get_mut("notify_channels").and_then(Value::as_array_mut) {
        for ch in arr.iter_mut() {
            if let Some(cobj) = ch.as_object_mut() {
                for sub in ["token", "app_token"] {
                    let aad_name = format!("notify_channels.{sub}");
                    if let Some(val) = cobj.get_mut(sub) {
                        if let Some(s) = val.as_str() {
                            match unseal_str(key.as_ref(), &aad_name, s) {
                                Ok((plain, was_plain)) => {
                                    if was_plain {
                                        saw_plain = true;
                                    }
                                    *val = Value::String(plain);
                                }
                                Err(e) => {
                                    tracing::warn!("settings: could not unseal {aad_name} ({e}); clearing unreadable value");
                                    *val = Value::String(String::new());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    saw_plain
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn seal_roundtrip_and_aad_domain_separation() {
        let dir = std::env::temp_dir().join(format!("bageo-seal-test-{}", uuid::Uuid::new_v4()));
        let settings_path = dir.join("config").join("settings.json");
        let key = load_or_create_key(&settings_path, true).unwrap().unwrap();
        let sealed = seal_str(&key, "gemini_api_key", "test-secret-key-12345").unwrap();
        assert!(is_sealed(&sealed));
        assert!(!sealed.contains("test-secret-key"));

        let (plain, was_plain) = unseal_str(Some(&key), "gemini_api_key", &sealed).unwrap();
        assert_eq!(plain, "test-secret-key-12345");
        assert!(!was_plain);

        // Transplanting ciphertext to another field must fail authentication.
        assert!(unseal_str(Some(&key), "ui_password", &sealed).is_err());

        // Legacy plaintext is accepted and flagged for upgrade.
        let (legacy, was_plain) = unseal_str(Some(&key), "gemini_api_key", "plain-legacy").unwrap();
        assert_eq!(legacy, "plain-legacy");
        assert!(was_plain);

        // Backup item classification ignores `.settings.key`.
        assert_eq!(crate::backup::item_of("config/.settings.key"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overrides_json_seals_credentials_and_channel_tokens() {
        let dir = std::env::temp_dir().join(format!("bageo-seal-json-{}", uuid::Uuid::new_v4()));
        let settings_path = dir.join("config").join("settings.json");
        let mut v = json!({
            "owner_name": "Alex",
            "gemini_api_key": "fake-gemini-secret",
            "notify_channels": [
                { "id": "1", "kind": "telegram", "token": "fake-bot-token", "app_token": "fake-app-token", "target": "123" }
            ]
        });
        seal_overrides_json(&settings_path, &mut v, &["gemini_api_key", "ui_password"]).unwrap();
        let raw = v.to_string();
        assert!(!raw.contains("fake-gemini-secret") && !raw.contains("fake-bot-token") && !raw.contains("fake-app-token"));
        assert!(raw.contains("Alex"));

        let saw_plain = unseal_overrides_json(Some(&settings_path), &mut v, &["gemini_api_key", "ui_password"]);
        assert!(!saw_plain);
        assert_eq!(v["gemini_api_key"], "fake-gemini-secret");
        assert_eq!(v["notify_channels"][0]["token"], "fake-bot-token");
        assert_eq!(v["notify_channels"][0]["app_token"], "fake-app-token");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

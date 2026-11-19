// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Settings file and password storage.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

const KEYCHAIN_SERVICE: &str = "bageo-status";
const KEYCHAIN_USER: &str = "console";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Empty means "find Bageo automatically".
    pub address: String,
    pub autostart: bool,
    /// Show a desktop notification when a call comes in.
    pub notify_calls: bool,
    /// Show a desktop notification when a text message arrives.
    pub notify_sms: bool,
    /// Show a desktop notification when the summary of a finished call is ready.
    /// On by default, also for settings files written before this existed.
    pub notify_summary: bool,
    /// The console was opened once after Bageo first answered on this computer.
    pub opened_console: bool,
    /// Where the Bageo program was last found. macOS can run a downloaded app
    /// from a temporary copy, away from the folder Bageo is in.
    pub agent_path: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            address: String::new(),
            autostart: false,
            notify_calls: false,
            notify_sms: false,
            notify_summary: true,
            opened_console: false,
            agent_path: String::new(),
        }
    }
}

fn path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|d| d.join("config.json"))
}

/// Returns the saved settings and whether the file already existed.
pub fn load(app: &AppHandle) -> (Config, bool) {
    let Some(p) = path(app) else {
        return (Config::default(), true);
    };
    match fs::read_to_string(&p) {
        Ok(text) => (serde_json::from_str(&text).unwrap_or_default(), true),
        Err(_) => (Config::default(), p.exists()),
    }
}

pub fn save(app: &AppHandle, cfg: &Config) -> Result<(), String> {
    let p = path(app).ok_or_else(|| "config".to_string())?;
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|_| "config".to_string())?;
    }
    let text = serde_json::to_string_pretty(cfg).map_err(|_| "config".to_string())?;
    fs::write(&p, text).map_err(|_| "config".to_string())
}

fn entry() -> Option<keyring::Entry> {
    keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).ok()
}

pub fn read_password() -> Option<String> {
    entry()?.get_password().ok().filter(|p| !p.is_empty())
}

pub fn store_password(password: &str) -> Result<(), String> {
    entry()
        .ok_or_else(|| "keychain".to_string())?
        .set_password(password)
        .map_err(|_| "keychain".to_string())
}

pub fn delete_password() -> Result<(), String> {
    match entry().map(|e| e.delete_credential()) {
        None | Some(Ok(())) | Some(Err(keyring::Error::NoEntry)) => Ok(()),
        Some(Err(_)) => Err("keychain".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_reads_partial_and_empty_json() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert_eq!(c, Config::default());
        let c: Config = serde_json::from_str(r#"{"address":"http://192.168.1.20:8787/"}"#).unwrap();
        assert_eq!(c.address, "http://192.168.1.20:8787/");
        assert!(!c.autostart);
        assert!(!c.notify_calls && !c.notify_sms);
    }

    #[test]
    fn old_config_file_without_notify_fields_still_loads() {
        let c: Config = serde_json::from_str(r#"{"address": "", "autostart": true}"#).unwrap();
        assert!(c.autostart);
        assert!(!c.notify_calls);
        assert!(!c.notify_sms);
        let c: Config = serde_json::from_str(r#"{"notify_calls": true, "notify_sms": true}"#).unwrap();
        assert!(c.notify_calls && c.notify_sms);
    }

    #[test]
    fn old_config_file_without_opened_console_still_loads() {
        let old = r#"{"address": "", "autostart": false, "notify_calls": true, "notify_sms": false}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        assert!(c.notify_calls);
        assert!(!c.opened_console);
        let c: Config = serde_json::from_str(r#"{"opened_console": true}"#).unwrap();
        assert!(c.opened_console);
        assert!(serde_json::to_string(&c).unwrap().contains("\"opened_console\":true"));
    }

    #[test]
    fn call_summary_notice_is_on_unless_the_file_says_otherwise() {
        assert!(Config::default().notify_summary);
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.notify_summary);
        let old = r#"{"address": "", "autostart": true, "notify_calls": true, "notify_sms": false}"#;
        let c: Config = serde_json::from_str(old).unwrap();
        assert!(c.notify_summary && c.notify_calls && !c.notify_sms);
        let c: Config = serde_json::from_str(r#"{"notify_summary": false}"#).unwrap();
        assert!(!c.notify_summary);
    }
}

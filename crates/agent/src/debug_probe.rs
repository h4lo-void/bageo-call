// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Session NDJSON debug probe (debug mode). Written under CALLS_DATA_DIR.
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("AGENT_DEBUG_PROBE").as_deref().map(str::trim),
            Ok("1") | Ok("true") | Ok("yes")
        )
    })
}

/// Append one probe line to the debug NDJSON file (no PII secrets).
///
/// Off unless `AGENT_DEBUG_PROBE=1`: it does synchronous file IO from the call
/// path (hello / every TTS burst / disconnect) and the file grew without bound.
pub fn agent_dbg(location: &str, message: &str, hypothesis_id: &str, data: Value) {
    if !enabled() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = json!({
        "sessionId": "agent",
        "timestamp": ts,
        "location": location,
        "message": message,
        "hypothesisId": hypothesis_id,
        "runId": "live",
        "data": data,
    });
    // Only the data folder: if it cannot be written, nothing is written.
    let path = crate::config::data_dir().join("debug-probe.ndjson");
    write_line(&path, &line.to_string());
}

fn write_line(path: &std::path::Path, line: &str) {
    if let Ok(mut f) = crate::private_file::append_file(path) {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_written_when_the_folder_is_missing() {
        let dir = std::env::temp_dir().join(format!("bageo-probe-{}", uuid::Uuid::new_v4().simple()));
        let path = dir.join("debug-probe.ndjson");
        write_line(&path, "{}");
        assert!(!path.exists() && !dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("bageo-probe-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("debug-probe.ndjson");
        write_line(&path, "{\"a\":1}");
        write_line(&path, "{\"a\":2}");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

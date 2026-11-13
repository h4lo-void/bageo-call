//! Session NDJSON debug probe (debug mode). Written under CALLS_DATA_DIR.
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
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
    let paths = [
        PathBuf::from(crate::config::env_or("CALLS_DATA_DIR", "data/calls")).join("debug-probe.ndjson"),
        std::env::temp_dir().join("bageo-call-debug-probe.ndjson"),
    ];
    for p in &paths {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{line}");
            break;
        }
    }
}

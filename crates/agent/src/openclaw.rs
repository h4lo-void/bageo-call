//! OpenClaw gateway handoff for assistant tasks.
//!
//! # Endpoint convention
//!
//! We POST to **`{openclaw_url}/hooks/agent`** (trailing slashes on the base URL are
//! stripped). This webhook-style path is used instead of `/v1/tasks` because myna-call
//! hands off work to an external agent hook rather than managing task CRUD locally.
//!
//! Request body:
//! ```json
//! { "source": "myna-call", "task": <task_json> }
//! ```
//!
//! When `openclaw_token` is non-empty, sends `Authorization: Bearer <token>`.
//! HTTP client timeouts are configured on the shared `reqwest::Client` caller.

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};
use uuid::Uuid;

/// Result of handing a task to OpenClaw.
pub struct OpenClawHandoff {
    pub external_id: String,
    pub raw: Value,
}

/// True when `openclaw_url` is set (non-empty after trim).
pub fn configured(url: &str) -> bool {
    !url.trim().is_empty()
}

fn hook_url(base: &str) -> String {
    format!("{}/hooks/agent", base.trim_end_matches('/'))
}

fn non_empty(s: &str) -> Option<&str> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Best-effort: top-level `id`, `task_id`, or `external_id` (string fields only).
fn extract_external_id(raw: &Value) -> Option<String> {
    for key in ["id", "task_id", "external_id"] {
        if let Some(s) = raw.get(key).and_then(|v| v.as_str()) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    None
}

/// POST the task to OpenClaw and return the gateway's external id when present.
pub async fn handoff_task(
    http: &Client,
    url: &str,
    token: &str,
    task_json: &Value,
) -> Result<OpenClawHandoff> {
    let endpoint = hook_url(url);
    let body = json!({
        "source": "myna-call",
        "task": task_json,
    });

    info!("openclaw handoff POST {endpoint}");

    let mut req = http.post(&endpoint).json(&body);
    if let Some(t) = non_empty(token) {
        req = req.bearer_auth(t);
    }

    let resp = req.send().await.context("openclaw handoff HTTP")?;
    let status = resp.status();
    let raw = match resp.json::<Value>().await {
        Ok(v) => v,
        Err(e) => {
            warn!("openclaw handoff: response not JSON ({e})");
            Value::Null
        }
    };

    if !status.is_success() {
        anyhow::bail!("openclaw handoff HTTP {status}: {raw}");
    }

    let external_id = extract_external_id(&raw).unwrap_or_else(|| {
        let id = Uuid::new_v4().to_string();
        warn!("openclaw handoff: no id in response, using local uuid={id}");
        id
    });

    info!("openclaw handoff ok external_id={external_id}");
    Ok(OpenClawHandoff { external_id, raw })
}

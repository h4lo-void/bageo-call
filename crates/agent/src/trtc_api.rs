//! Tencent Cloud TRTC **control-plane** REST (TC3-HMAC-SHA256).
//!
//! Official Actions only (API version `2019-07-22`), verified against
//! [StartAIConversation](https://cloud.tencent.com/document/product/647/108514),
//! [StopAIConversation](https://cloud.tencent.com/document/product/647/108513),
//! [ControlAIConversation](https://cloud.tencent.com/document/product/647/109408):
//! - [`TrtcApiClient::start_ai_conversation`] → `StartAIConversation`
//! - [`TrtcApiClient::stop_ai_conversation`] → `StopAIConversation`
//! - [`TrtcApiClient::control_server_push_text`] → `ControlAIConversation` (`ServerPushText`)
//!
//! There is **no** Cloud REST `JoinRoom` for media. Room media join is SDK
//! `enterRoom` in the TRTC media sidecar (`trtc-sdk-v5`) only.
//!
//! Credentials: `TRTC_SECRET_ID` + `TRTC_SECRET_KEY` (SecretString). Never log
//! secrets, UserSig, or LLM/TTS JSON bodies.

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::config::SecretString;

type HmacSha256 = Hmac<Sha256>;

const API_VERSION: &str = "2019-07-22";
const SERVICE: &str = "trtc";
/// String room id (`call_{serial}`) — official docs use RoomIdType=1 for string.
const ROOM_ID_TYPE_STRING: u32 = 1;

/// TC3 + endpoint settings for TRTC AIConversation Actions.
#[derive(Clone)]
pub struct TrtcApiClient {
    secret_id: SecretString,
    secret_key: SecretString,
    /// e.g. `ap-guangzhou`
    region: String,
    /// Host only, e.g. `trtc.tencentcloudapi.com` or `trtc.intl.tencentcloudapi.com`
    host: String,
    http: Client,
}

impl std::fmt::Debug for TrtcApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrtcApiClient")
            .field(
                "secret_id",
                &if self.secret_id.as_str().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field(
                "secret_key",
                &if self.secret_key.as_str().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field("region", &self.region)
            .field("host", &self.host)
            .finish()
    }
}

impl TrtcApiClient {
    pub fn new(
        secret_id: SecretString,
        secret_key: SecretString,
        region: String,
        host: String,
        http: Client,
    ) -> Self {
        Self {
            secret_id,
            secret_key,
            region,
            host,
            http,
        }
    }

    /// Cloud API SecretId present (AKID…) — required for TC3; distinct from UserSig.
    pub fn configured(&self) -> bool {
        !self.secret_id.as_str().trim().is_empty() && !self.secret_key.as_str().trim().is_empty()
    }

    /// `StartAIConversation` — AI robot joins `room_id` targeting `bridge_*`.
    ///
    /// Required (official): `SdkAppId`, `RoomId`, `AgentConfig.{UserId,UserSig,TargetUserId}`.
    /// `LLMConfig` / `TTSConfig` are opaque JSON **strings** from env (API marks them
    /// optional in the schema table but describes them as required; we require both).
    /// `RoomIdType=1` for string rooms (`call_{serial}`). Never log configs / UserSig.
    pub async fn start_ai_conversation(
        &self,
        sdk_app_id: u32,
        room_id: &str,
        agent_user_id: &str,
        agent_user_sig: &str,
        target_user_id: &str,
        max_idle_secs: u32,
        stt_language: &str,
        llm_config: &str,
        tts_config: &str,
    ) -> Result<StartAiConversationResult> {
        if !self.configured() {
            bail!("TRTC Cloud API not configured (need TRTC_SECRET_ID + TRTC_SECRET_KEY)");
        }
        if llm_config.trim().is_empty() || tts_config.trim().is_empty() {
            bail!("TRTC_LLM_CONFIG / TRTC_TTS_CONFIG required for StartAIConversation");
        }
        let body = start_ai_conversation_body(
            sdk_app_id,
            room_id,
            agent_user_id,
            agent_user_sig,
            target_user_id,
            max_idle_secs,
            stt_language,
            llm_config,
            tts_config,
        );
        let resp = self.call_action("StartAIConversation", &body).await?;
        let task_id = resp
            .pointer("/Response/TaskId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if task_id.is_empty() {
            let err = resp
                .pointer("/Response/Error/Message")
                .and_then(|v| v.as_str())
                .unwrap_or("missing TaskId");
            bail!("StartAIConversation failed: {err}");
        }
        info!(
            "TRTC StartAIConversation ok room_id={room_id} agent={agent_user_id} target={target_user_id} task_id_len={}",
            task_id.len()
        );
        Ok(StartAiConversationResult { task_id })
    }

    /// `StopAIConversation` — required field: `TaskId`.
    pub async fn stop_ai_conversation(&self, task_id: &str) -> Result<()> {
        if task_id.trim().is_empty() {
            bail!("StopAIConversation: empty TaskId");
        }
        let body = stop_ai_conversation_body(task_id);
        let resp = self.call_action("StopAIConversation", &body).await?;
        if let Some(msg) = resp
            .pointer("/Response/Error/Message")
            .and_then(|v| v.as_str())
        {
            bail!("StopAIConversation failed: {msg}");
        }
        info!("TRTC StopAIConversation ok task_id_len={}", task_id.len());
        Ok(())
    }

    /// `ControlAIConversation` with `Command: ServerPushText` (menu / barge-in prompts).
    ///
    /// Required: `TaskId`, `Command`; when Command is ServerPushText, `ServerPushText` is required.
    pub async fn control_server_push_text(
        &self,
        task_id: &str,
        text: &str,
        interrupt: bool,
    ) -> Result<()> {
        if task_id.trim().is_empty() {
            bail!("ControlAIConversation: empty TaskId");
        }
        let body = control_server_push_text_body(task_id, text, interrupt);
        let resp = self.call_action("ControlAIConversation", &body).await?;
        if let Some(msg) = resp
            .pointer("/Response/Error/Message")
            .and_then(|v| v.as_str())
        {
            bail!("ControlAIConversation ServerPushText failed: {msg}");
        }
        info!(
            "TRTC ControlAIConversation ServerPushText ok task_id_len={} text_chars={}",
            task_id.len(),
            text.chars().count()
        );
        Ok(())
    }

    async fn call_action(&self, action: &str, payload: &Value) -> Result<Value> {
        let payload_str = serde_json::to_string(payload).context("serialize TC3 body")?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("clock")?
            .as_secs();
        let authorization = sign_tc3(
            self.secret_id.as_str().trim(),
            self.secret_key.as_str().trim(),
            &self.host,
            SERVICE,
            action,
            timestamp,
            &payload_str,
        )?;

        let url = format!("https://{}/", self.host);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", authorization)
            .header("Content-Type", "application/json; charset=utf-8")
            .header("Host", &self.host)
            .header("X-TC-Action", action)
            .header("X-TC-Timestamp", timestamp.to_string())
            .header("X-TC-Version", API_VERSION)
            .header("X-TC-Region", &self.region)
            .body(payload_str)
            .send()
            .await
            .with_context(|| format!("TRTC REST {action} HTTP"))?;

        let status = resp.status();
        let text = resp.text().await.context("TRTC REST body")?;
        let value: Value = serde_json::from_str(&text)
            .unwrap_or_else(|_| json!({ "Response": { "Error": { "Message": text } } }));
        if !status.is_success() {
            warn!("TRTC REST {action} HTTP status={status}");
        }
        Ok(value)
    }
}

#[derive(Debug, Clone)]
pub struct StartAiConversationResult {
    pub task_id: String,
}

/// Request body for `StartAIConversation` (field names must match Cloud API).
fn start_ai_conversation_body(
    sdk_app_id: u32,
    room_id: &str,
    agent_user_id: &str,
    agent_user_sig: &str,
    target_user_id: &str,
    max_idle_secs: u32,
    stt_language: &str,
    llm_config: &str,
    tts_config: &str,
) -> Value {
    json!({
        "SdkAppId": sdk_app_id,
        "RoomId": room_id,
        // 0 = digit room, 1 = string room (official). We use `call_{serial}` strings.
        "RoomIdType": ROOM_ID_TYPE_STRING,
        "AgentConfig": {
            "UserId": agent_user_id,
            "UserSig": agent_user_sig,
            "TargetUserId": target_user_id,
            // Official default 60s; floor at 30 to avoid instant idle-stop.
            "MaxIdleTime": max_idle_secs.max(30),
        },
        "STTConfig": {
            "Language": stt_language,
        },
        // API expects JSON *strings* (not nested objects) for LLM/TTS configs.
        "LLMConfig": llm_config,
        "TTSConfig": tts_config,
    })
}

fn stop_ai_conversation_body(task_id: &str) -> Value {
    json!({ "TaskId": task_id })
}

fn control_server_push_text_body(task_id: &str, text: &str, interrupt: bool) -> Value {
    json!({
        "TaskId": task_id,
        "Command": "ServerPushText",
        "ServerPushText": {
            "Text": text,
            "Interrupt": interrupt,
        }
    })
}

/// Build Authorization header value for TC3-HMAC-SHA256 (POST JSON).
pub fn sign_tc3(
    secret_id: &str,
    secret_key: &str,
    host: &str,
    service: &str,
    _action: &str,
    timestamp: u64,
    payload: &str,
) -> Result<String> {
    if secret_id.is_empty() || secret_key.is_empty() {
        bail!("empty TC3 credentials");
    }
    let content_type = "application/json; charset=utf-8";
    let canonical_headers = format!(
        "content-type:{content_type}\nhost:{}\n",
        host.to_ascii_lowercase()
    );
    let signed_headers = "content-type;host";
    let hashed_payload = sha256_hex(payload.as_bytes());
    let canonical_request =
        format!("POST\n/\n\n{canonical_headers}\n{signed_headers}\n{hashed_payload}");
    let hashed_canonical = sha256_hex(canonical_request.as_bytes());
    let date = utc_date(timestamp);
    let credential_scope = format!("{date}/{service}/tc3_request");
    let string_to_sign =
        format!("TC3-HMAC-SHA256\n{timestamp}\n{credential_scope}\n{hashed_canonical}");

    let secret_date = hmac_sha256(format!("TC3{secret_key}").as_bytes(), date.as_bytes())?;
    let secret_service = hmac_sha256(&secret_date, service.as_bytes())?;
    let secret_signing = hmac_sha256(&secret_service, b"tc3_request")?;
    let signature = hex_encode(&hmac_sha256(&secret_signing, string_to_sign.as_bytes())?);

    Ok(format!(
        "TC3-HMAC-SHA256 Credential={secret_id}/{credential_scope}, SignedHeaders={signed_headers}, Signature={signature}"
    ))
}

fn utc_date(timestamp: u64) -> String {
    // YYYY-MM-DD in UTC from unix seconds (no chrono dep required for signing).
    let days = timestamp / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant civil_from_days (UTC calendar date from days since 1970-01-01).
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

fn sha256_hex(data: &[u8]) -> String {
    hex_encode(&Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key).context("hmac key")?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Resolve API host from env-style flags (domestic vs intl).
pub fn resolve_host(explicit: &str, use_intl: bool) -> String {
    let t = explicit.trim();
    if !t.is_empty() {
        return t
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string();
    }
    if use_intl {
        "trtc.intl.tencentcloudapi.com".into()
    } else {
        "trtc.tencentcloudapi.com".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_tc3_is_stable_shape() {
        let auth = sign_tc3(
            "AKIDtestSecretIdOnly",
            "testSecretKeyOnly",
            "trtc.tencentcloudapi.com",
            "trtc",
            "StartAIConversation",
            1_551_113_065,
            r#"{"SdkAppId":1}"#,
        )
        .expect("sign");
        assert!(auth.starts_with("TC3-HMAC-SHA256 Credential=AKIDtestSecretIdOnly/"));
        assert!(auth.contains("/trtc/tc3_request"));
        assert!(auth.contains("SignedHeaders=content-type;host"));
        assert!(auth.contains("Signature="));
        assert!(!auth.contains("testSecretKeyOnly"));
    }

    #[test]
    fn utc_date_known() {
        // 2019-02-25 00:00:00 UTC
        assert_eq!(utc_date(1_551_052_800), "2019-02-25");
    }

    #[test]
    fn resolve_host_defaults() {
        assert_eq!(resolve_host("", false), "trtc.tencentcloudapi.com");
        assert_eq!(resolve_host("", true), "trtc.intl.tencentcloudapi.com");
        assert_eq!(
            resolve_host("https://trtc.tencentcloudapi.com/", false),
            "trtc.tencentcloudapi.com"
        );
    }

    #[test]
    fn debug_redacts_secrets() {
        let c = TrtcApiClient::new(
            SecretString::from("AKIDleaked"),
            SecretString::from("secret-leaked"),
            "ap-guangzhou".into(),
            "trtc.tencentcloudapi.com".into(),
            Client::new(),
        );
        let s = format!("{c:?}");
        assert!(s.contains("***"));
        assert!(!s.contains("leaked"));
    }

    #[test]
    fn start_body_matches_official_required_fields() {
        let llm = r#"{"LLMType":"openai","Model":"x","APIKey":"k","APIUrl":"https://example/v1/chat/completions","Streaming":true}"#;
        let tts = r#"{"TTSType":"flow","VoiceId":"v-female-R2s4N9qJ","Model":"flow_02_turbo"}"#;
        let body = start_ai_conversation_body(
            14_000_000,
            "call_S1",
            "ai_S1",
            "sig-placeholder",
            "bridge_S1",
            120,
            "zh",
            llm,
            tts,
        );
        assert_eq!(body["SdkAppId"], 14_000_000);
        assert_eq!(body["RoomId"], "call_S1");
        assert_eq!(body["RoomIdType"], ROOM_ID_TYPE_STRING);
        assert_eq!(body["AgentConfig"]["UserId"], "ai_S1");
        assert_eq!(body["AgentConfig"]["UserSig"], "sig-placeholder");
        assert_eq!(body["AgentConfig"]["TargetUserId"], "bridge_S1");
        assert_eq!(body["AgentConfig"]["MaxIdleTime"], 120);
        assert_eq!(body["STTConfig"]["Language"], "zh");
        // LLM/TTS must remain JSON *strings*, not objects.
        assert_eq!(body["LLMConfig"].as_str(), Some(llm));
        assert_eq!(body["TTSConfig"].as_str(), Some(tts));
    }

    #[test]
    fn stop_and_control_bodies_match_official() {
        let stop = stop_ai_conversation_body("task-abc");
        assert_eq!(stop["TaskId"], "task-abc");
        assert!(stop.get("Command").is_none());

        let ctrl = control_server_push_text_body("task-abc", "按1 AI通話", true);
        assert_eq!(ctrl["TaskId"], "task-abc");
        assert_eq!(ctrl["Command"], "ServerPushText");
        assert_eq!(ctrl["ServerPushText"]["Text"], "按1 AI通話");
        assert_eq!(ctrl["ServerPushText"]["Interrupt"], true);
    }

    #[test]
    fn max_idle_floors_at_30() {
        let body =
            start_ai_conversation_body(1, "call_x", "ai_x", "sig", "bridge_x", 5, "zh", "{}", "{}");
        assert_eq!(body["AgentConfig"]["MaxIdleTime"], 30);
    }
}

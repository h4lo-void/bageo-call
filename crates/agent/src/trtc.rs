//! Tencent Cloud TRTC helpers (UserSig + config).
//!
//! Credentials come from env defaults + optional runtime overrides
//! (`SettingsStore` / `/calls` settings → `effective()`). Never log or return the
//! secret key.
//!
//! Product direction: short menu + DTMF `1` (AI call) use TRTC voice via
//! [`crate::trtc_bridge`]. Soft scaffold (UserSig + silence) until the
//! PCM↔TRTC bridge ships. Cloud AI control is [`crate::trtc_api`]
//! (`StartAIConversation` / `Stop` / `Control`) — **not** a media JoinRoom.
//!
//! Failure degrade (`!transfer_ready` **or** UserSig fail): brief local goodbye
//! + hangup — **not** long hold music. DTMF `2` voicemail stays local.
//!
//! Call site: `start_trtc_ai_after_choice` in `main.rs` must match on
//! [`TrtcSettings::arm_ai_after_dtmf1`] → Scaffold (silence) or Degrade (goodbye).

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::SecretString;
use crate::trtc_api::{resolve_host, TrtcApiClient};

type HmacSha256 = Hmac<Sha256>;

/// Env-backed TRTC settings (no secret in Display/Debug).
#[derive(Clone)]
pub struct TrtcSettings {
    pub sdk_app_id: u32,
    /// SDK secret key for TLSSigAPIv2 UserSig — [`SecretString`]; never log plaintext.
    pub secret_key: SecretString,
    /// Cloud API / CAM SecretId (AKID…) for TC3 REST — paired with [`Self::cloud_secret_key`].
    pub secret_id: SecretString,
    /// Cloud API / CAM SecretKey for TC3 signing — **not** the SDK UserSig key.
    pub cloud_secret_key: SecretString,
    /// When true and credentials present, menu DTMF 1 prefers TRTC AI path.
    pub ai_enabled: bool,
    /// Default UserSig TTL (seconds).
    pub usersig_expire_secs: u64,
    /// REST region (`X-TC-Region`), e.g. `ap-guangzhou`.
    pub api_region: String,
    /// Host override; empty → domestic or intl via `api_use_intl`.
    pub api_host: String,
    pub api_use_intl: bool,
    /// Opaque JSON string for StartAIConversation LLMConfig (never log).
    pub llm_config: SecretString,
    /// Opaque JSON string for StartAIConversation TTSConfig (never log).
    pub tts_config: SecretString,
    pub stt_language: String,
    pub ai_max_idle_secs: u32,
    /// Master switch for sidecar media path (`TRTC_BRIDGE_ENABLED`).
    pub bridge_enabled: bool,
    /// Sidecar base URL, e.g. `http://127.0.0.1:8791` (`TRTC_BRIDGE_URL`).
    pub bridge_url: String,
}

impl TrtcSettings {
    pub fn from_parts(
        sdk_app_id: u32,
        secret_key: SecretString,
        ai_enabled: bool,
        usersig_expire_secs: u64,
    ) -> Self {
        Self {
            sdk_app_id,
            secret_key,
            secret_id: SecretString::from(""),
            cloud_secret_key: SecretString::from(""),
            ai_enabled,
            usersig_expire_secs,
            api_region: "ap-guangzhou".into(),
            api_host: String::new(),
            api_use_intl: false,
            llm_config: SecretString::from(""),
            tts_config: SecretString::from(""),
            stt_language: "zh".into(),
            ai_max_idle_secs: 120,
            bridge_enabled: false,
            bridge_url: String::new(),
        }
    }

    pub fn configured(&self) -> bool {
        self.sdk_app_id > 0 && !self.secret_key.as_str().trim().is_empty()
    }

    /// Ready to prefer TRTC for menu + AI (credentials + switch).
    pub fn transfer_ready(&self) -> bool {
        self.ai_enabled && self.configured()
    }

    /// TC3 Cloud API ready (CAM SecretId + CAM SecretKey + LLM/TTS JSON).
    pub fn cloud_api_ready(&self) -> bool {
        !self.secret_id.as_str().trim().is_empty()
            && !self.cloud_secret_key.as_str().trim().is_empty()
            && !self.llm_config.as_str().trim().is_empty()
            && !self.tts_config.as_str().trim().is_empty()
    }

    pub fn api_host_resolved(&self) -> String {
        resolve_host(&self.api_host, self.api_use_intl)
    }

    pub fn bridge_base_url(&self) -> Option<&str> {
        let u = self.bridge_url.trim();
        if u.is_empty() {
            None
        } else {
            Some(u.trim_end_matches('/'))
        }
    }

    /// Build a TC3 client when CAM Cloud API credentials exist (LLM/TTS may still be empty).
    pub fn api_client(&self, http: reqwest::Client) -> Option<TrtcApiClient> {
        if self.secret_id.as_str().trim().is_empty()
            || self.cloud_secret_key.as_str().trim().is_empty()
        {
            return None;
        }
        Some(TrtcApiClient::new(
            self.secret_id.clone(),
            self.cloud_secret_key.clone(),
            self.api_region.clone(),
            self.api_host_resolved(),
            http,
        ))
    }

    pub fn gen_user_sig(&self, user_id: &str) -> Result<String> {
        gen_user_sig(
            self.sdk_app_id,
            self.secret_key.as_str().trim(),
            user_id,
            self.usersig_expire_secs,
        )
    }

    /// DTMF-1 AI path decision (crypto/config only — no media I/O).
    ///
    /// **Wire in `main.rs` → `start_trtc_ai_after_choice`:**
    /// - [`TrtcAiArm::Scaffold`] → set `IvrPhase::TrtcAi` + soft silence scaffold
    /// - [`TrtcAiArm::Degrade`] → `start_trtc_degrade_hangup` (brief goodbye WAV /
    ///   short Piper + hangup). Reasons: `trtc_not_ready` | `trtc_usersig_fail`.
    /// - Never hold-music theater; DTMF-2 VM unaffected.
    pub fn arm_ai_after_dtmf1(&self, serial: &str) -> TrtcAiArm {
        if !self.transfer_ready() {
            return TrtcAiArm::Degrade {
                reason: "trtc_not_ready",
                detail: None,
                sdk_app_id: self.sdk_app_id,
                ai_enabled: self.ai_enabled,
            };
        }
        match self.gen_user_sig(&format!("bridge_{serial}")) {
            Ok(_) => TrtcAiArm::Scaffold {
                sdk_app_id: self.sdk_app_id,
            },
            Err(e) => TrtcAiArm::Degrade {
                reason: "trtc_usersig_fail",
                detail: Some(format!("{e:#}")),
                sdk_app_id: self.sdk_app_id,
                ai_enabled: self.ai_enabled,
            },
        }
    }
}

/// Result of arming TRTC AI after menu DTMF `1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrtcAiArm {
    /// UserSig ok — enter soft silence scaffold until PCM↔TRTC bridge.
    Scaffold { sdk_app_id: u32 },
    /// Brief local goodbye + hangup. `reason`: `trtc_not_ready` | `trtc_usersig_fail`.
    Degrade {
        reason: &'static str,
        /// UserSig error text only (never the secret).
        detail: Option<String>,
        sdk_app_id: u32,
        ai_enabled: bool,
    },
}

impl std::fmt::Debug for TrtcSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrtcSettings")
            .field("sdk_app_id", &self.sdk_app_id)
            .field(
                "secret_key",
                &if self.secret_key.as_str().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field(
                "secret_id",
                &if self.secret_id.as_str().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field(
                "cloud_secret_key",
                &if self.cloud_secret_key.as_str().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field("ai_enabled", &self.ai_enabled)
            .field("usersig_expire_secs", &self.usersig_expire_secs)
            .field("api_region", &self.api_region)
            .field("bridge_enabled", &self.bridge_enabled)
            .field(
                "bridge_url",
                &if self.bridge_url.is_empty() {
                    ""
                } else {
                    "(set)"
                },
            )
            .field(
                "llm_config",
                &if self.llm_config.as_str().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field(
                "tts_config",
                &if self.tts_config.as_str().is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .field("configured", &self.configured())
            .field("cloud_api_ready", &self.cloud_api_ready())
            .finish()
    }
}

/// TLSSigAPIv2 UserSig (zlib + URL-safe base64 with Tencent's `*`-`-`_` alphabet).
pub fn gen_user_sig(
    sdk_app_id: u32,
    secret_key: &str,
    user_id: &str,
    expire_secs: u64,
) -> Result<String> {
    if sdk_app_id == 0 {
        bail!("TRTC sdk_app_id is 0");
    }
    if secret_key.is_empty() {
        bail!("TRTC secret_key empty");
    }
    let identifier = user_id.trim();
    if identifier.is_empty() {
        bail!("TRTC user_id empty");
    }
    let curr_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock")?
        .as_secs();
    let expire = expire_secs.max(60);

    let content = format!(
        "TLS.identifier:{identifier}\nTLS.sdkappid:{sdk_app_id}\nTLS.time:{curr_time}\nTLS.expire:{expire}\n"
    );
    let mut mac = HmacSha256::new_from_slice(secret_key.as_bytes()).context("hmac key")?;
    mac.update(content.as_bytes());
    let sig_b64 = B64.encode(mac.finalize().into_bytes());

    let doc = json!({
        "TLS.ver": "2.0",
        "TLS.identifier": identifier,
        "TLS.sdkappid": sdk_app_id,
        "TLS.expire": expire,
        "TLS.time": curr_time,
        "TLS.sig": sig_b64,
    });
    let raw = serde_json::to_vec(&doc).context("usersig json")?;
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&raw).context("zlib write")?;
    let compressed = enc.finish().context("zlib finish")?;
    Ok(base64url_tencent(&compressed))
}

fn base64url_tencent(data: &[u8]) -> String {
    B64.encode(data)
        .replace('+', "*")
        .replace('/', "-")
        .replace('=', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usersig_is_nonempty_urlsafe() {
        let sig = gen_user_sig(
            1_400_000_000,
            "unit-test-secret-key-not-real",
            "agent_bridge",
            600,
        )
        .expect("sig");
        assert!(!sig.is_empty());
        assert!(!sig.contains('+'));
        assert!(!sig.contains('/'));
        assert!(!sig.contains('='));
    }

    #[test]
    fn max_idle_default_meets_tencent_range() {
        let t = TrtcSettings::from_parts(1, SecretString::from("k"), true, 600);
        assert!((60..=259_200).contains(&t.ai_max_idle_secs));
    }

    #[test]
    fn transfer_ready_needs_both() {
        let mut t = TrtcSettings::from_parts(1, SecretString::from("k"), false, 600);
        assert!(!t.transfer_ready());
        t.ai_enabled = true;
        assert!(t.transfer_ready());
        t.secret_key = SecretString::from("");
        assert!(!t.transfer_ready());
    }

    #[test]
    fn debug_redacts_secret() {
        let t = TrtcSettings::from_parts(
            1,
            SecretString::from("unit-test-trtc-secret-KEY-do-not-leak"),
            true,
            600,
        );
        let s = format!("{t:?}");
        assert!(s.contains("***"));
        assert!(!s.contains("do-not-leak"));
    }

    #[test]
    fn arm_ai_degrades_when_not_ready() {
        let t = TrtcSettings::from_parts(0, SecretString::from(""), true, 600);
        match t.arm_ai_after_dtmf1("abc") {
            TrtcAiArm::Degrade {
                reason: "trtc_not_ready",
                ..
            } => {}
            other => panic!("expected trtc_not_ready degrade, got {other:?}"),
        }
    }

    #[test]
    fn arm_ai_scaffolds_when_ready() {
        let t = TrtcSettings::from_parts(
            1_400_000_000,
            SecretString::from("unit-test-secret-key-not-real"),
            true,
            600,
        );
        match t.arm_ai_after_dtmf1("xyz") {
            TrtcAiArm::Scaffold { sdk_app_id } => assert_eq!(sdk_app_id, 1_400_000_000),
            other => panic!("expected scaffold, got {other:?}"),
        }
    }
}

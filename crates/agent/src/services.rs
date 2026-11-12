use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::config::{AgentMode, LlmProvider, Settings};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub async fn transcribe_pcm16(
    client: &Client,
    settings: &Settings,
    pcm: &[u8],
    sample_rate: u32,
) -> Result<String> {
    let min_bytes = (sample_rate as usize) * 2 / 5; // ~0.2s
    if pcm.len() < min_bytes {
        return Ok(String::new());
    }
    let url = format!(
        "{}/v1/transcribe?sample_rate={}&language=zh",
        settings.whisper_url.trim_end_matches('/'),
        sample_rate
    );
    let resp = client
        .post(&url)
        .header("Content-Type", "application/octet-stream")
        .body(pcm.to_vec())
        .send()
        .await
        .context("whisper HTTP")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("whisper HTTP {status}: {body}");
    }
    let data: Value = resp.json().await.context("whisper json")?;
    // Whisper `zh` emits Simplified; the owner reads Traditional Chinese (Taiwan) everywhere.
    Ok(crate::zh_tw::to_tw(
        data.get("text").and_then(|v| v.as_str()).unwrap_or("").trim(),
    ))
}

fn build_messages(settings: &Settings, user_text: &str, history: &[ChatMessage]) -> Vec<Value> {
    let mut messages = vec![json!({
        "role": "system",
        "content": settings.system_prompt,
    })];
    let start = history.len().saturating_sub(8);
    for m in &history[start..] {
        messages.push(json!({"role": m.role, "content": m.content}));
    }
    messages.push(json!({"role": "user", "content": user_text}));
    messages
}

/// Chat via configured LLM provider (`LLM_PROVIDER=ollama` default, or `openrouter`).
pub async fn chat(
    client: &Client,
    settings: &Settings,
    user_text: &str,
    history: &[ChatMessage],
) -> Result<String> {
    match settings.llm_provider {
        LlmProvider::Ollama => chat_ollama(client, settings, user_text, history).await,
        LlmProvider::OpenRouter => chat_openrouter(client, settings, user_text, history).await,
    }
}

pub async fn chat_ollama(
    client: &Client,
    settings: &Settings,
    user_text: &str,
    history: &[ChatMessage],
) -> Result<String> {
    let messages = build_messages(settings, user_text, history);
    let payload = json!({
        "model": settings.ollama_model,
        "messages": messages,
        "stream": false,
        "options": {"temperature": 0.4, "num_predict": 128},
    });
    let url = format!("{}/api/chat", settings.ollama_url.trim_end_matches('/'));
    let resp = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .context("ollama chat")?;
    resp.error_for_status_ref()
        .with_context(|| format!("ollama status {}", resp.status()))?;
    let data: Value = resp.json().await.context("ollama json")?;
    Ok(data
        .pointer("/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string())
}

pub async fn chat_openrouter(
    client: &Client,
    settings: &Settings,
    user_text: &str,
    history: &[ChatMessage],
) -> Result<String> {
    let key = settings.openrouter_api_key.trim();
    if key.is_empty() {
        anyhow::bail!(
            "LLM_PROVIDER=openrouter but OPENROUTER_API_KEY is empty; set the key or switch back to LLM_PROVIDER=ollama"
        );
    }
    let messages = build_messages(settings, user_text, history);
    let payload = json!({
        "model": settings.openrouter_model,
        "messages": messages,
        "stream": false,
        "temperature": 0.4,
        "max_tokens": 128,
    });
    let url = format!(
        "{}/chat/completions",
        settings.openrouter_base_url.trim_end_matches('/')
    );
    let resp = client
        .post(&url)
        .bearer_auth(key)
        .header("HTTP-Referer", &settings.openrouter_http_referer)
        .header("X-Title", &settings.openrouter_app_title)
        .json(&payload)
        .send()
        .await
        .context("openrouter chat")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("openrouter status {status}: {body}");
    }
    let data: Value = resp.json().await.context("openrouter json")?;
    Ok(data
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string())
}

/// **Piper is hard-disabled.** Product audio is clone WAVs / Gemini / TRTC only.
/// Never synthesize the default female Piper voice (zh_CN-huayan).
pub async fn synthesize_piper(_client: &Client, _settings: &Settings, text: &str) -> Vec<u8> {
    let preview: String = text.chars().take(48).collect();
    warn!("Piper TTS disabled (clone-WAV only); refusing synth text={preview:?}");
    Vec::new()
}

fn decode_tts_bytes(bytes: &[u8], dst_rate: u32) -> Option<Vec<u8>> {
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" {
        return parse_wav_pcm(bytes, dst_rate);
    }
    Some(bytes.to_vec())
}

fn parse_wav_pcm(bytes: &[u8], dst_rate: u32) -> Option<Vec<u8>> {
    // Minimal PCM WAV parser
    if bytes.len() < 44 || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12usize;
    let mut rate = 22050u32;
    let mut channels = 1u16;
    let mut bits = 16u16;
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().ok()?) as usize;
        let start = pos + 8;
        let end = start.saturating_add(size);
        if end > bytes.len() {
            break;
        }
        if id == b"fmt " && size >= 16 {
            channels = u16::from_le_bytes(bytes[start + 2..start + 4].try_into().ok()?);
            rate = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().ok()?);
            bits = u16::from_le_bytes(bytes[start + 14..start + 16].try_into().ok()?);
        } else if id == b"data" {
            data = Some(&bytes[start..end]);
        }
        pos = end + (size % 2); // word align
    }
    let pcm = data?;
    if bits != 16 {
        warn!("WAV bits={bits} not supported");
        return None;
    }
    Some(resample_pcm16(pcm, rate, dst_rate, channels))
}

pub fn resample_pcm16(pcm: &[u8], src_rate: u32, dst_rate: u32, channels: u16) -> Vec<u8> {
    if pcm.is_empty() || src_rate == 0 {
        return Vec::new();
    }
    if src_rate == dst_rate && channels <= 1 {
        return pcm.to_vec();
    }
    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let mono: Vec<i16> = if channels > 1 {
        samples
            .chunks_exact(channels as usize)
            .map(|frame| {
                let sum: i32 = frame.iter().map(|&s| s as i32).sum();
                (sum / channels as i32) as i16
            })
            .collect()
    } else {
        samples
    };
    if src_rate == dst_rate {
        return i16_to_le(&mono);
    }
    let duration = mono.len() as f64 / src_rate as f64;
    let new_len = ((duration * dst_rate as f64) as usize).max(1);
    let mut out = Vec::with_capacity(new_len);
    for i in 0..new_len {
        let t = i as f64 / new_len as f64 * (mono.len() as f64);
        let i0 = t.floor() as usize;
        let i1 = (i0 + 1).min(mono.len().saturating_sub(1));
        let frac = t - i0 as f64;
        let s0 = mono.get(i0).copied().unwrap_or(0) as f64;
        let s1 = mono.get(i1).copied().unwrap_or(0) as f64;
        out.push((s0 + (s1 - s0) * frac) as i16);
    }
    i16_to_le(&out)
}

fn i16_to_le(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

pub async fn ensure_ollama_model(client: &Client, settings: &Settings) {
    if settings.agent_mode != AgentMode::Llm {
        info!(
            "AGENT_MODE={}; skipping Ollama model ensure",
            settings.agent_mode.as_str()
        );
        return;
    }
    if settings.llm_provider != LlmProvider::Ollama {
        info!(
            "LLM provider is {}; skipping Ollama model ensure",
            settings.llm_provider.as_str()
        );
        return;
    }
    let tags_url = format!("{}/api/tags", settings.ollama_url.trim_end_matches('/'));
    match client.get(&tags_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(data) = resp.json::<Value>().await {
                let names: Vec<String> = data
                    .get("models")
                    .and_then(|m| m.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| {
                                m.get("name").and_then(|n| n.as_str()).map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let model = &settings.ollama_model;
                let prefix = model.split(':').next().unwrap_or(model);
                if names.iter().any(|n| n == model || n.starts_with(prefix)) {
                    return;
                }
                info!("Pulling Ollama model {model} (may take a while)");
                let pull_url = format!("{}/api/pull", settings.ollama_url.trim_end_matches('/'));
                if let Err(e) = client
                    .post(&pull_url)
                    .json(&json!({"name": model}))
                    .send()
                    .await
                {
                    warn!("Ollama pull failed: {e:#}");
                }
            }
        }
        Ok(resp) => warn!("Ollama not ready yet: status {}", resp.status()),
        Err(e) => warn!("Ollama not ready yet: {e:#}"),
    }
}

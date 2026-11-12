//! Minimal Wyoming TTS client for linuxserver/piper (port 10200).

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::{error, info};
use url::Url;

#[derive(Debug, Clone)]
pub struct PcmMeta {
    pub pcm: Vec<u8>,
    pub rate: u32,
    pub width: u16,
    pub channels: u16,
}

pub fn parse_host_port(piper_url: &str) -> Result<(String, u16)> {
    let raw = if piper_url.contains("://") {
        piper_url.to_string()
    } else {
        format!("tcp://{piper_url}")
    };
    let parsed = Url::parse(&raw).context("parse piper url")?;
    let host = parsed.host_str().unwrap_or("piper").to_string();
    let port = parsed.port().unwrap_or(10200);
    Ok((host, port))
}

pub async fn synthesize_wyoming(text: &str, piper_url: &str) -> Result<PcmMeta> {
    let (host, port) = parse_host_port(piper_url)?;
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    .context("piper connect timeout")?
    .with_context(|| format!("connect piper {host}:{port}"))?;

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    write_event(&mut writer, "synthesize", &json!({"text": text}), None).await?;

    let mut pcm_parts: Vec<u8> = Vec::new();
    let mut sample_rate = 22050u32;
    let mut sample_width = 2u16;
    let mut channels = 1u16;

    loop {
        let event =
            match tokio::time::timeout(std::time::Duration::from_secs(60), read_event(&mut reader))
                .await
            {
                Ok(Ok(Some(e))) => e,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(e),
                Err(_) => bail!("piper read timeout"),
            };

        match event.etype.as_str() {
            "audio-start" => {
                sample_rate = event
                    .data
                    .get("rate")
                    .or_else(|| event.data.get("sample_rate"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(sample_rate as u64) as u32;
                sample_width = event
                    .data
                    .get("width")
                    .or_else(|| event.data.get("sample_width"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(sample_width as u64) as u16;
                channels = event
                    .data
                    .get("channels")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(channels as u64) as u16;
            }
            "audio-chunk" => {
                if !event.payload.is_empty() {
                    pcm_parts.extend_from_slice(&event.payload);
                }
            }
            "audio-stop" | "synthesized" => break,
            "error" => {
                error!("Piper wyoming error: {}", event.data);
                break;
            }
            _ => {}
        }
    }

    let raw = pcm_parts;
    info!(
        "Piper synthesized {} bytes rate={} width={} ch={}",
        raw.len(),
        sample_rate,
        sample_width,
        channels
    );
    Ok(PcmMeta {
        pcm: raw,
        rate: sample_rate,
        width: sample_width,
        channels,
    })
}

struct Event {
    etype: String,
    data: Value,
    payload: Vec<u8>,
}

async fn write_event(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    etype: &str,
    data: &Value,
    payload: Option<&[u8]>,
) -> Result<()> {
    let data_bytes = serde_json::to_vec(data)?;
    let payload = payload.unwrap_or(&[]);
    let header = json!({
        "type": etype,
        "data_length": data_bytes.len(),
        "payload_length": payload.len(),
    });
    let mut header_bytes = serde_json::to_vec(&header)?;
    header_bytes.push(b'\n');
    writer.write_all(&header_bytes).await?;
    if !data_bytes.is_empty() {
        writer.write_all(&data_bytes).await?;
    }
    if !payload.is_empty() {
        writer.write_all(payload).await?;
    }
    writer.flush().await?;
    Ok(())
}

async fn read_event(
    reader: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
) -> Result<Option<Event>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let header: Value = serde_json::from_str(line.trim()).context("wyoming header json")?;
    let data_length = header
        .get("data_length")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let payload_length = header
        .get("payload_length")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let etype = header
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let mut data = json!({});
    if data_length > 0 {
        let mut buf = vec![0u8; data_length];
        reader.read_exact(&mut buf).await?;
        data = serde_json::from_slice(&buf).context("wyoming data json")?;
    }
    let mut payload = vec![0u8; payload_length];
    if payload_length > 0 {
        reader.read_exact(&mut payload).await?;
    }
    Ok(Some(Event {
        etype,
        data,
        payload,
    }))
}

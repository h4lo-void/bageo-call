//! Decode hold-music / IVR audio (mp3/wav/…) to 16 kHz s16le mono PCM.
//! Prefers a tiny native PCM-WAV path (no ffmpeg) so 48 kHz menu WAVs resample cleanly;
//! falls back to ffmpeg for mp3 / odd containers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

/// Decode `path` to PCM s16le mono at `sample_rate`.
pub fn decode_audio_to_pcm16(path: &Path, sample_rate: u32) -> Result<Vec<u8>> {
    if !path.is_file() {
        bail!("audio file missing: {}", path.display());
    }
    // Native path first for standard PCM WAV (our IVR assets are 48 kHz s16le mono).
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("wav"))
        .unwrap_or(false)
    {
        match decode_wav_pcm16_native(path, sample_rate) {
            Ok(pcm) => return Ok(pcm),
            Err(e) => {
                warn!(
                    "native WAV decode failed path={} ({e:#}); trying ffmpeg",
                    path.display()
                );
            }
        }
    }
    decode_audio_to_pcm16_ffmpeg(path, sample_rate)
}

/// Minimal RIFF/WAVE PCM reader + linear resample to `target_rate` mono s16le.
fn decode_wav_pcm16_native(path: &Path, target_rate: u32) -> Result<Vec<u8>> {
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if data.len() < 44 {
        bail!("WAV too small");
    }
    if &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        bail!("not RIFF/WAVE");
    }
    let mut offset = 12usize;
    let mut fmt_channels = 0u16;
    let mut fmt_rate = 0u32;
    let mut pcm = Vec::new();
    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let size = u32::from_le_bytes(data[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let start = offset + 8;
        let end = start.saturating_add(size);
        if end > data.len() {
            bail!("WAV chunk overruns file");
        }
        if id == b"fmt " {
            if size < 16 {
                bail!("fmt chunk too small");
            }
            let audio_format = u16::from_le_bytes(data[start..start + 2].try_into().unwrap());
            fmt_channels = u16::from_le_bytes(data[start + 2..start + 4].try_into().unwrap());
            fmt_rate = u32::from_le_bytes(data[start + 4..start + 8].try_into().unwrap());
            let fmt_bits = u16::from_le_bytes(data[start + 14..start + 16].try_into().unwrap());
            if audio_format != 1 {
                bail!("unsupported WAV format tag={audio_format} (need PCM)");
            }
            if fmt_bits != 16 {
                bail!("unsupported bits={fmt_bits} (need 16)");
            }
            if fmt_channels == 0 {
                bail!("WAV channels=0");
            }
        } else if id == b"data" {
            pcm = data[start..end].to_vec();
        }
        offset = end + (size % 2); // word align
    }
    if fmt_rate == 0 || pcm.is_empty() {
        bail!("WAV missing fmt/data");
    }
    // Downmix multi-channel → mono
    if fmt_channels > 1 {
        let ch = fmt_channels as usize;
        let frames = pcm.len() / (2 * ch);
        let mut mono = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let mut acc = 0i32;
            for c in 0..ch {
                let o = (i * ch + c) * 2;
                acc += i16::from_le_bytes([pcm[o], pcm[o + 1]]) as i32;
            }
            let s = (acc / ch as i32) as i16;
            mono.extend_from_slice(&s.to_le_bytes());
        }
        pcm = mono;
    }
    if fmt_rate != target_rate {
        pcm = resample_s16le_mono(&pcm, fmt_rate, target_rate);
    }
    if pcm.is_empty() {
        bail!("decoded empty PCM");
    }
    Ok(pcm)
}

/// Public for Gemini Live 24 kHz → phone rate resampling.
pub fn resample_s16le_mono_pub(input: &[u8], src_rate: u32, dst_rate: u32) -> Vec<u8> {
    resample_s16le_mono(input, src_rate, dst_rate)
}

fn resample_s16le_mono(input: &[u8], src_rate: u32, dst_rate: u32) -> Vec<u8> {
    if src_rate == 0 || dst_rate == 0 || input.len() < 2 {
        return Vec::new();
    }
    let src_frames = input.len() / 2;
    let dst_frames = (src_frames as u64 * dst_rate as u64 / src_rate as u64) as usize;
    let mut out = Vec::with_capacity(dst_frames * 2);
    if src_frames == 0 || dst_frames == 0 {
        return out;
    }
    for i in 0..dst_frames {
        let src_pos = i as f64 * (src_rate as f64 / dst_rate as f64);
        let i0 = src_pos.floor() as usize;
        let i1 = (i0 + 1).min(src_frames - 1);
        let frac = src_pos - i0 as f64;
        let s0 = i16::from_le_bytes([input[i0 * 2], input[i0 * 2 + 1]]) as f64;
        let s1 = i16::from_le_bytes([input[i1 * 2], input[i1 * 2 + 1]]) as f64;
        let s = (s0 + (s1 - s0) * frac)
            .round()
            .clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn decode_audio_to_pcm16_ffmpeg(path: &Path, sample_rate: u32) -> Result<Vec<u8>> {
    decode_audio_to_pcm16_ffmpeg_with_af(path, sample_rate, None)
}

/// ffmpeg decode; optional `af` graph (hold music mp3 fallback: single stage).
fn decode_audio_to_pcm16_ffmpeg_with_af(
    path: &Path,
    sample_rate: u32,
    af: Option<&str>,
) -> Result<Vec<u8>> {
    let sr = sample_rate.to_string();
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("audio path is not UTF-8"))?;
    let mut args = vec!["-hide_banner", "-loglevel", "error", "-vn", "-i", path_str];
    if let Some(filter) = af {
        args.extend_from_slice(&["-af", filter]);
    }
    args.extend_from_slice(&[
        "-f",
        "s16le",
        "-acodec",
        "pcm_s16le",
        "-ac",
        "1",
        "-ar",
        &sr,
        "pipe:1",
    ]);
    let output = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("spawn ffmpeg for audio decode")?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        bail!("ffmpeg failed ({}): {err}", output.status);
    }
    if output.stdout.is_empty() {
        bail!("ffmpeg produced empty PCM for {}", path.display());
    }
    Ok(output.stdout)
}

/// Peak-normalize s16le mono toward `target_peak` (e.g. 0.85 of full scale).
#[allow(dead_code)]
fn peak_normalize_s16le(pcm: &mut [u8], target_peak: f32) {
    if pcm.len() < 2 || !(0.1..=0.99).contains(&target_peak) {
        return;
    }
    let mut peak = 0i32;
    for pair in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]) as i32;
        peak = peak.max(s.abs());
    }
    if peak < 256 {
        return;
    }
    let target = (target_peak * i16::MAX as f32) as i32;
    let gain = target as f64 / peak as f64;
    // Always aim at target (boost quiet masters; cut hot ones). Soft-limit follows.
    if (gain - 1.0).abs() < 0.01 {
        return;
    }
    for pair in pcm.chunks_exact_mut(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]) as f64 * gain;
        let out = s.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        let b = out.to_le_bytes();
        pair[0] = b[0];
        pair[1] = b[1];
    }
}

/// Whole-buffer RMS normalize — keeps perceived level steady (unlike peak-norm
/// which makes transient high notes dominate).
fn rms_normalize_s16le(pcm: &mut [u8], target_rms: f32) {
    if pcm.len() < 2 || !(0.02..=0.35).contains(&target_rms) {
        return;
    }
    let mut acc = 0.0f64;
    let mut n = 0usize;
    for pair in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]) as f64 / i16::MAX as f64;
        acc += s * s;
        n += 1;
    }
    if n == 0 {
        return;
    }
    let rms = (acc / n as f64).sqrt();
    if rms < 1e-4 {
        return;
    }
    let gain = (target_rms as f64 / rms).clamp(0.2, 5.0);
    if (gain - 1.0).abs() < 0.02 {
        return;
    }
    for pair in pcm.chunks_exact_mut(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]) as f64 * gain;
        let out = s.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        let b = out.to_le_bytes();
        pair[0] = b[0];
        pair[1] = b[1];
    }
}

// NOTE: the former per-frame `HoldLeveler` runtime AGC was removed on purpose:
// stacked on the asset compressor and the carrier's VoLTE AGC it rode gain
// 0.71–1.40 (~6 dB) with 35–130 ms time constants, adding ±2.9 dB envelope
// swing → audible hold-music pumping (louder / quieter).
// Playback is constant-gain; loudness is fixed offline on the asset once.

/// Soft-knee ceiling so VoLTE uplink never hard-clips (crackling on highs).
/// `ceiling` is linear full-scale fraction (e.g. 0.79 ≈ −2 dBFS).
pub fn soft_limit_s16le(pcm: &mut [u8], ceiling: f32) {
    if pcm.len() < 2 || !(0.2..=0.98).contains(&ceiling) {
        return;
    }
    let ceil = (ceiling as f64) * i16::MAX as f64;
    let knee = ceil * 0.85;
    for pair in pcm.chunks_exact_mut(2) {
        let s = i16::from_le_bytes([pair[0], pair[1]]) as f64;
        let a = s.abs();
        let out = if a <= knee {
            s
        } else {
            // Smooth approach to ceiling (no hard clip).
            let t = ((a - knee) / (ceil - knee + 1.0)).clamp(0.0, 4.0);
            let shaped = knee + (ceil - knee) * (t / (1.0 + t));
            s.signum() * shaped.min(ceil)
        };
        let b = (out.round().clamp(i16::MIN as f64, i16::MAX as f64) as i16).to_le_bytes();
        pair[0] = b[0];
        pair[1] = b[1];
    }
}

/// Backward-compatible alias used by older call sites / docs.
pub fn decode_mp3_to_pcm16(path: &Path, sample_rate: u32) -> Result<Vec<u8>> {
    decode_audio_to_pcm16(path, sample_rate)
}

/// Fixed-loudness decode graph (mp3 fallback path only; the pre-rendered WAV
/// is preferred). ONE gentle slow-release compressor stage + true-peak ceiling:
/// slow release (350 ms) stays below the audible pumping band, ratio 3 only
/// tames macro dynamics. No dynaudnorm, no stacked compressors, no runtime AGC.
const HOLD_MUSIC_FFMPEG_AF: &str = concat!(
    "highpass=f=60,",
    "equalizer=f=6500:t=q:w=1.2:g=-2,",
    "acompressor=threshold=0.06:ratio=3:attack=15:release=350:makeup=1.6,",
    "alimiter=limit=0.85:attack=3:release=60:level=0"
);

/// Load hold music when enabled; returns `None` to fall back to Piper.
/// Prefers a sibling `.wav` when `path` is `.mp3` (pre-normalized asset).
pub fn load_hold_music_pcm(path: &str, sample_rate: u32, enabled: bool) -> Option<Vec<u8>> {
    if !enabled {
        info!("TRANSFER_USE_HOLD_MUSIC=false; Piper transfer TTS remains active");
        return None;
    }
    let primary = PathBuf::from(path);
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        // Prefer clean 16 kHz WAV when env still points at legacy .mp3.
        if primary
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.eq_ignore_ascii_case("mp3"))
            .unwrap_or(false)
        {
            v.push(primary.with_extension("wav"));
        }
        v.push(primary);
        v
    };
    for p in &candidates {
        match load_hold_one(p, sample_rate) {
            Ok(pcm) => {
                let secs = pcm.len() as f64 / (sample_rate as f64 * 2.0);
                info!(
                    "hold music loaded path={} pcm_bytes={} (~{secs:.1}s @ {} Hz)",
                    p.display(),
                    pcm.len(),
                    sample_rate
                );
                return Some(pcm);
            }
            Err(e) => debug!("hold music try path={} ({e:#})", p.display()),
        }
    }
    debug!("hold music unavailable; falling back to Piper transfer TTS");
    None
}

fn load_hold_one(path: &Path, sample_rate: u32) -> Result<Vec<u8>> {
    if !path.is_file() {
        bail!("audio file missing: {}", path.display());
    }
    let is_wav = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("wav"))
        .unwrap_or(false);
    let mut pcm = if is_wav {
        decode_audio_to_pcm16(path, sample_rate)?
    } else {
        // Compressed sources: mild shelf + compress/limit (keep VoLTE clarity).
        match decode_audio_to_pcm16_ffmpeg_with_af(path, sample_rate, Some(HOLD_MUSIC_FFMPEG_AF)) {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    "hold ffmpeg af failed ({e:#}); plain decode path={}",
                    path.display()
                );
                decode_audio_to_pcm16_ffmpeg(path, sample_rate)?
            }
        }
    };
    // Fixed mid loudness (~−17 dBFS RMS); soft-limit after for VoLTE headroom.
    rms_normalize_s16le(&mut pcm, 0.135);
    soft_limit_s16le(&mut pcm, 0.82);
    if pcm.is_empty() {
        bail!("decoded empty PCM");
    }
    Ok(pcm)
}

/// Parse `IVR_PROMPT_WAVS` (comma/semicolon/whitespace-separated) into paths.
pub fn parse_wav_list(spec: &str) -> Vec<PathBuf> {
    spec.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// One pre-decoded IVR menu WAV segment (kept separate for sequential realtime play).
#[derive(Debug, Clone)]
pub struct IvrSegment {
    pub label: String,
    pub path: PathBuf,
    pub pcm: Vec<u8>,
}

/// Load each menu WAV as its own PCM buffer (order preserved). Missing files are skipped.
/// Returns `None` if nothing decoded (caller falls back to Piper).
pub fn load_ivr_segments(paths: &[PathBuf], sample_rate: u32) -> Option<Vec<IvrSegment>> {
    if paths.is_empty() {
        return None;
    }
    let mut segs = Vec::new();
    for p in paths {
        match decode_audio_to_pcm16(p, sample_rate) {
            Ok(pcm) => {
                let label = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("ivr_segment")
                    .to_string();
                let secs = pcm.len() as f64 / (sample_rate as f64 * 2.0);
                info!(
                    "IVR segment loaded label={label} path={} pcm_bytes={} (~{secs:.1}s @ {} Hz)",
                    p.display(),
                    pcm.len(),
                    sample_rate
                );
                segs.push(IvrSegment {
                    label,
                    path: p.clone(),
                    pcm,
                });
            }
            Err(e) => warn!("IVR wav skip path={} ({e:#})", p.display()),
        }
    }
    if segs.is_empty() {
        warn!("IVR prompt WAVs unavailable; falling back to Piper menu TTS");
        return None;
    }
    let total: usize = segs.iter().map(|s| s.pcm.len()).sum();
    let secs = total as f64 / (sample_rate as f64 * 2.0);
    info!(
        "IVR prompt WAV menu loaded segments={}/{} total_pcm_bytes={} (~{secs:.1}s @ {} Hz) order={:?}",
        segs.len(),
        paths.len(),
        total,
        sample_rate,
        segs.iter().map(|s| s.label.as_str()).collect::<Vec<_>>()
    );
    Some(segs)
}

/// Concatenate decoded PCM from each path (legacy helper / tests).
/// Prefer [`load_ivr_segments`] for IVR menu playback.
pub fn load_concat_audio_pcm(paths: &[PathBuf], sample_rate: u32) -> Option<Vec<u8>> {
    let segs = load_ivr_segments(paths, sample_rate)?;
    let mut out = Vec::new();
    for s in segs {
        out.extend_from_slice(&s.pcm);
    }
    Some(out)
}

/// Load a single fail / voicemail clip; `None` → Piper stub.
pub fn load_fail_wav_pcm(path: &str, sample_rate: u32) -> Option<Vec<u8>> {
    let p = Path::new(path);
    match decode_audio_to_pcm16(p, sample_rate) {
        Ok(pcm) => {
            info!(
                "IVR fail WAV loaded path={} pcm_bytes={}",
                p.display(),
                pcm.len()
            );
            Some(pcm)
        }
        Err(e) => {
            debug!("optional sound not loaded ({e:#})");
            None
        }
    }
}

/// Drop leading near-silence so first system audio is hearable sooner after answer.
/// Windowed RMS; keeps a tiny pad so wave onsets are not clipped.
pub fn trim_leading_silence_s16le(
    pcm: &[u8],
    sample_rate: u32,
    rms_threshold: f32,
    win_ms: u32,
) -> (Vec<u8>, u32) {
    if pcm.len() < 4 || sample_rate < 8000 {
        return (pcm.to_vec(), 0);
    }
    let win_ms = win_ms.clamp(10, 50);
    let win = ((sample_rate as usize) * win_ms as usize / 1000).max(1) * 2;
    let thr = rms_threshold.max(1.0);
    let mut pos = 0usize;
    let mut trimmed_ms = 0u32;
    while pos + win <= pcm.len() {
        let chunk = &pcm[pos..pos + win];
        let n = chunk.len() / 2;
        let mut sum = 0.0f64;
        for i in 0..n {
            let s = i16::from_le_bytes([chunk[i * 2], chunk[i * 2 + 1]]) as f64;
            sum += s * s;
        }
        let rms = (sum / n as f64).sqrt() as f32;
        if rms >= thr {
            break;
        }
        pos += win;
        trimmed_ms = trimmed_ms.saturating_add(win_ms);
    }
    // Keep ≤40 ms of pre-roll when we chopped silence (avoids hard consonant cut).
    let pad = ((sample_rate as usize) * 40 / 1000) * 2;
    let start = pos.saturating_sub(pad);
    // Cap trim so we never delete almost the whole clip (bad threshold).
    let min_keep = ((sample_rate as usize) * 400 / 1000) * 2; // 400 ms
    if pcm.len().saturating_sub(start) < min_keep {
        return (pcm.to_vec(), 0);
    }
    if start == 0 {
        return (pcm.to_vec(), 0);
    }
    (pcm[start..].to_vec(), trimmed_ms)
}

//! Preview on the settings page: a few phone lines in a Gemini voice with the
//! chosen background sound under them — what a caller hears. A voice's lines come from
//! one short Live session the first time and are kept on disk after that.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::ambience::AmbienceMixer;

/// Gemini Live prebuilt voices: (name, character, male). Male voices first;
/// the settings page translates the character words.
pub const VOICES: &[(&str, &str, bool)] = &[
    ("Charon", "informative", true),
    ("Puck", "upbeat", true),
    ("Fenrir", "excitable", true),
    ("Orus", "firm", true),
    ("Enceladus", "breathy", true),
    ("Iapetus", "clear", true),
    ("Umbriel", "easy-going", true),
    ("Algieba", "smooth", true),
    ("Algenib", "gravelly", true),
    ("Rasalgethi", "informative", true),
    ("Alnilam", "firm", true),
    ("Schedar", "even", true),
    ("Achird", "friendly", true),
    ("Zubenelgenubi", "casual", true),
    ("Sadachbia", "lively", true),
    ("Sadaltager", "knowledgeable", true),
    ("Zephyr", "bright", false),
    ("Kore", "firm", false),
    ("Leda", "youthful", false),
    ("Aoede", "breezy", false),
    ("Callirrhoe", "easy-going", false),
    ("Autonoe", "bright", false),
    ("Despina", "smooth", false),
    ("Erinome", "clear", false),
    ("Laomedeia", "upbeat", false),
    ("Achernar", "soft", false),
    // Measured on gemini-3.8-live (median pitch ~110 / ~144 Hz): lower than the
    // other female voices (~200–255 Hz), so the list says so.
    ("Gacrux", "mature, very low", false),
    ("Pulcherrima", "forward, low", false),
    ("Vindemiatrix", "gentle", false),
    ("Sulafat", "warm", false),
];

pub fn valid_voice(voice: &str) -> bool {
    VOICES.iter().any(|(name, _, _)| *name == voice)
}

/// What the AI says in the sample, in the agent's language; the gaps between
/// lines are the caller talking.
pub fn sample_lines() -> [&'static str; 3] {
    crate::locale::phrases(crate::locale::current()).preview
}
/// Lines in a sample.
pub const LINES: usize = 3;
const LEAD_MS: usize = 700;
const GAP_MS: usize = 1_800;
const TAIL_MS: usize = 1_000;
/// Background sound only: long enough for a bed's signature sound to come by.
const BED_ONLY_MS: usize = 20_000;
/// Cached lines older than this are made again (model voices get updated).
const CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 24 * 3600);

pub struct VoicePreviews {
    dir: PathBuf,
    /// One Live session at a time.
    gen_lock: tokio::sync::Mutex<()>,
}

/// Model name as a directory name.
fn model_dir(model: &str) -> String {
    let m: String = model
        .trim()
        .trim_start_matches("models/")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
        .collect();
    if m.is_empty() { "default".into() } else { m }
}

fn read_lines(paths: &[PathBuf]) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let fresh = std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age < CACHE_MAX_AGE);
        let pcm = std::fs::read(p).ok().filter(|b| fresh && b.len() >= 3_200)?;
        out.push(pcm);
    }
    Some(out)
}

impl VoicePreviews {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            gen_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn paths(&self, model: &str, voice: &str) -> Vec<PathBuf> {
        // Per language: the lines are spoken in the agent's language.
        let dir = self
            .dir
            .join(model_dir(model))
            .join(crate::locale::current().code());
        (0..LINES)
            .map(|i| dir.join(format!("{voice}_{i}.pcm")))
            .collect()
    }

    /// The voice's lines (s16le at `sample_rate`): from disk, else a fresh Live
    /// session (`regen` forces one). `voice` must be [`valid_voice`].
    pub async fn lines(
        &self,
        api_key: &str,
        model: &str,
        thinking_level: &str,
        voice: &str,
        sample_rate: u32,
        regen: bool,
    ) -> Result<Vec<Vec<u8>>> {
        anyhow::ensure!(valid_voice(voice), "unknown voice {voice}");
        let paths = self.paths(model, voice);
        if !regen {
            if let Some(lines) = read_lines(&paths) {
                return Ok(lines);
            }
        }
        let _one = self.gen_lock.lock().await;
        if !regen {
            // Made by another request while this one waited.
            if let Some(lines) = read_lines(&paths) {
                return Ok(lines);
            }
        }
        let t0 = std::time::Instant::now();
        // A session per line, all at once: ~3 s instead of ~10 s one after another.
        let sample = sample_lines();
        let per_line = futures_util::future::try_join_all(sample.iter().map(|line| {
            crate::gemini_live::speak_lines(api_key, model, thinking_level, voice, std::slice::from_ref(line), sample_rate)
        }))
        .await?;
        let lines: Vec<Vec<u8>> = per_line.into_iter().flatten().collect();
        anyhow::ensure!(lines.len() == LINES, "Gemini left out some of the lines");
        info!(
            "voice preview made voice={voice} model={model} took_ms={} audio_ms={}",
            t0.elapsed().as_millis(),
            lines.iter().map(|l| l.len()).sum::<usize>() * 1000 / (sample_rate as usize * 2).max(1)
        );
        if let Err(e) = write_lines(&paths, &lines) {
            warn!("voice preview cache write failed voice={voice}: {e:#}");
        }
        Ok(lines)
    }

    fn reject_path(&self, model: &str, voice: &str) -> PathBuf {
        self.dir
            .join(model_dir(model))
            .join(crate::locale::current().code())
            .join(format!("{}_reject.pcm", file_voice(voice)))
    }

    /// The blocked-number announcement in `voice`, if it was made already. The call
    /// path only ever reads this — it never calls Gemini.
    pub fn cached_reject_clip(&self, model: &str, voice: &str) -> Option<Vec<u8>> {
        std::fs::read(self.reject_path(model, voice))
            .ok()
            .filter(|b| b.len() >= 3_200)
    }

    /// Make the announcement once (one short Live session, ~2 s) and keep it on
    /// disk; `Ok(false)` when it was there already. Cached for good: it only has
    /// to be made again for another voice or model.
    pub async fn ensure_reject_clip(
        &self,
        api_key: &str,
        model: &str,
        thinking_level: &str,
        voice: &str,
        sample_rate: u32,
    ) -> Result<bool> {
        if self.cached_reject_clip(model, voice).is_some() {
            return Ok(false);
        }
        let _one = self.gen_lock.lock().await;
        if self.cached_reject_clip(model, voice).is_some() {
            return Ok(false);
        }
        let t0 = std::time::Instant::now();
        let pcm = crate::gemini_live::speak_lines_as(
            &crate::prompts::current().announce_system,
            api_key,
            model,
            thinking_level,
            file_voice(voice),
            &[reject_announce_text()],
            sample_rate,
        )
        .await?
        .into_iter()
        .next()
        .unwrap_or_default();
        anyhow::ensure!(pcm.len() >= 3_200, "Gemini did not speak the announcement");
        let path = self.reject_path(model, voice);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_atomic(&path, &pcm)?;
        info!(
            "reject announcement made voice={voice} model={model} took_ms={} audio_ms={}",
            t0.elapsed().as_millis(),
            pcm.len() * 1000 / (sample_rate as usize * 2).max(1)
        );
        Ok(true)
    }
}

/// Blocked numbers: what the caller hears before the line drops, in the
/// agent's language.
pub fn reject_announce_text() -> &'static str {
    crate::locale::phrases(crate::locale::current()).unavailable
}

/// Voice of the blocked-number announcement: the AI's own voice setting.
pub fn reject_voice(rt: &crate::settings_store::RuntimeSettings) -> &str {
    match rt.gemini_voice_name.trim() {
        "" => crate::gemini_live::default_voice_name(),
        v => v,
    }
}

/// Have the announcement ready for the current voice and model while it can be
/// needed: there are blocked numbers, or hidden numbers are not answered (one
/// Gemini call the first time only). `true` = nothing left to do.
pub async fn prepare_reject_clip(
    previews: &VoicePreviews,
    rt: &crate::settings_store::RuntimeSettings,
    sample_rate: u32,
) -> bool {
    let needed = !rt.reject_numbers.is_empty() || !rt.answer_unknown_callers;
    if !needed || rt.gemini_api_key.trim().is_empty() {
        return true;
    }
    let voice = reject_voice(rt);
    match previews
        .ensure_reject_clip(
            &rt.gemini_api_key,
            &rt.gemini_live_model,
            &rt.gemini_thinking_level,
            voice,
            sample_rate,
        )
        .await
    {
        Ok(_) => true,
        Err(e) => {
            warn!("reject announcement not made voice={voice}: {e:#}");
            false
        }
    }
}

/// A voice name safe in a file name (the setting is free text): letters only,
/// else the default voice.
fn file_voice(voice: &str) -> &str {
    let v = voice.trim();
    if !v.is_empty() && v.len() <= 32 && v.chars().all(|c| c.is_ascii_alphabetic()) {
        v
    } else {
        crate::gemini_live::default_voice_name()
    }
}

fn write_lines(paths: &[PathBuf], lines: &[Vec<u8>]) -> Result<()> {
    if let Some(dir) = paths.first().and_then(|p| p.parent()) {
        std::fs::create_dir_all(dir)?;
    }
    for (p, pcm) in paths.iter().zip(lines) {
        write_atomic(p, pcm)?;
    }
    Ok(())
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The sample as a caller hears it: the lines with the caller's gaps between them
/// and the room under all of it (the always-on background sound of an echo-free phone).
/// `bed` = (samples, gain, second of the bed to start at — the same stretch
/// every time). No lines = the room alone, [`BED_ONLY_MS`] long.
pub fn assemble(lines: &[Vec<u8>], bed: Option<(Arc<Vec<i16>>, f32, usize)>, sample_rate: u32) -> Vec<u8> {
    let ms = |m: usize| (sample_rate as usize * m / 1000) * 2;
    let mut pcm: Vec<u8> = Vec::new();
    if lines.is_empty() {
        pcm.resize(ms(BED_ONLY_MS), 0);
    } else {
        pcm.resize(ms(LEAD_MS), 0);
        for (i, line) in lines.iter().enumerate() {
            if i > 0 {
                pcm.resize(pcm.len() + ms(GAP_MS), 0);
            }
            pcm.extend_from_slice(&line[..line.len() & !1]);
        }
        pcm.resize(pcm.len() + ms(TAIL_MS), 0);
    }
    if let Some((bed, gain, start_s)) = bed {
        let mut mixer = AmbienceMixer::new(bed, gain, start_s * sample_rate as usize, sample_rate);
        for frame in pcm.chunks_mut(ms(40).max(2)) {
            mixer.mix_under(frame, true);
        }
    }
    pcm
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(pcm: &[u8]) -> f32 {
        let n = (pcm.len() / 2).max(1) as f64;
        let s: f64 = pcm
            .chunks_exact(2)
            .map(|b| f64::from(i16::from_le_bytes([b[0], b[1]])).powi(2))
            .sum();
        (s / n).sqrt() as f32
    }

    #[test]
    fn voices_are_unique_and_male_first() {
        let names: std::collections::HashSet<_> = VOICES.iter().map(|v| v.0).collect();
        assert_eq!(names.len(), VOICES.len());
        assert!(valid_voice("Charon") && valid_voice(crate::gemini_live::default_voice_name()));
        assert!(!valid_voice("charon") && !valid_voice("../x") && !valid_voice(""));
        let first_female = VOICES.iter().position(|v| !v.2).unwrap();
        assert!(VOICES[first_female..].iter().all(|v| !v.2));
        assert_eq!(model_dir("models/gemini-3.8-live"), "gemini-3.8-live");
        assert_eq!(model_dir("a/../b"), "a_.._b");
    }

    #[test]
    fn sample_has_the_callers_gaps_and_the_room_under_everything() {
        let sr = 16_000u32;
        let line = |secs: usize| -> Vec<u8> {
            (0..sr as usize * secs).flat_map(|i| (if i % 2 == 0 { 8000i16 } else { -8000 }).to_le_bytes()).collect()
        };
        let lines = vec![line(1), line(2), line(1)];
        let dry = assemble(&lines, None, sr);
        let ms = |m: usize| (sr as usize * m / 1000) * 2;
        assert_eq!(dry.len(), ms(LEAD_MS + 2 * GAP_MS + TAIL_MS + 4_000));
        // The caller's turn (gap after line 1) is silent without a room…
        let gap = &dry[ms(LEAD_MS + 1_000 + 200)..ms(LEAD_MS + 1_000 + 1_600)];
        assert_eq!(rms(gap), 0.0);
        // …and carries the room with one.
        let bed = Arc::new((0..sr as usize * 30).map(|i| if i % 3 == 0 { 3000i16 } else { -1500 }).collect::<Vec<_>>());
        let wet = assemble(&lines, Some((bed.clone(), 0.2, 20)), sr);
        assert!(rms(&wet[ms(LEAD_MS + 1_000 + 200)..ms(LEAD_MS + 1_000 + 1_600)]) > 300.0);
        // The room alone, when no voice is asked for.
        let room = assemble(&[], Some((bed, 0.2, 20)), sr);
        assert!(room.len() == ms(BED_ONLY_MS) && rms(&room[ms(1_000)..]) > 300.0);
    }

    #[test]
    fn bed_preview_starts_where_the_bed_says() {
        // A bed that is loud only from 10 s on: starting there is loud at once.
        let sr = 16_000u32;
        let bed = Arc::new((0..sr as usize * 30).map(|i| if i < sr as usize * 10 { 0i16 } else if i % 2 == 0 { 4000 } else { -4000 }).collect::<Vec<_>>());
        let ms = |m: usize| (sr as usize * m / 1000) * 2;
        let late = assemble(&[], Some((bed.clone(), 1.0, 10)), sr);
        let early = assemble(&[], Some((bed, 1.0, 0)), sr);
        assert!(rms(&late[ms(1_000)..ms(3_000)]) > 1000.0);
        assert!(rms(&early[ms(1_000)..ms(3_000)]) < 10.0);
        assert!(crate::ambience::preview_start_s("hospital") != crate::ambience::preview_start_s("home"));
    }

    #[test]
    fn reject_clip_is_read_from_disk_only() {
        let dir = std::env::temp_dir().join(format!("vp-reject-{}", std::process::id()));
        let p = VoicePreviews::new(dir.clone());
        assert!(p.cached_reject_clip("gemini-3.8-live", "Rasalgethi").is_none());
        let path = p.reject_path("models/gemini-3.8-live", "Rasalgethi");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![7u8; 8_000]).unwrap();
        assert_eq!(p.cached_reject_clip("gemini-3.8-live", "Rasalgethi").unwrap().len(), 8_000);
        // Per voice: another voice has none yet.
        assert!(p.cached_reject_clip("gemini-3.8-live", "Charon").is_none());
        // A free-text voice setting never escapes the cache directory.
        assert_eq!(file_voice("../../etc"), crate::gemini_live::default_voice_name());
        assert_eq!(file_voice(" Rasalgethi "), "Rasalgethi");
        assert!(reject_announce_text().contains("無法接通"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_needs_every_line() {
        let dir = std::env::temp_dir().join(format!("vp-test-{}", std::process::id()));
        let p = VoicePreviews::new(dir.clone());
        let paths = p.paths("gemini-3.8-live", "Charon");
        assert!(read_lines(&paths).is_none());
        write_lines(&paths, &[vec![1u8; 4_000], vec![2u8; 4_000], vec![3u8; 4_000]]).unwrap();
        assert_eq!(read_lines(&paths).unwrap()[1][0], 2);
        std::fs::remove_file(&paths[2]).unwrap();
        assert!(read_lines(&paths).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

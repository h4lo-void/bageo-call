//! Preview on the settings page: a few phone lines in a Gemini voice with the
//! chosen background sound under them — what a caller hears. A voice's lines come from
//! the samples that come with the program (every voice in every language, recorded
//! with `agent make-voice-samples`), else from one short Live session the first
//! time, kept on disk after that.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::ambience::AmbienceMixer;
use crate::locale::Lang;

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

/// What the AI says in the sample, in `lang`; the gaps between lines are the
/// caller talking.
pub fn sample_lines(lang: Lang) -> [&'static str; 3] {
    crate::locale::phrases(lang).preview
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
    /// Samples that come with the program: `<lang>/<Voice>_<0|1|2|reject>.wav`
    /// (IMA ADPCM, see [`make_bundle`]). Used before anything is asked of Gemini.
    bundled: PathBuf,
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
    pub fn new(dir: PathBuf, bundled: PathBuf) -> Self {
        Self {
            dir,
            bundled,
            gen_lock: tokio::sync::Mutex::new(()),
        }
    }

    fn paths(&self, model: &str, voice: &str, lang: Lang) -> Vec<PathBuf> {
        // Per language: the lines are spoken in it.
        let dir = self.dir.join(model_dir(model)).join(lang.code());
        (0..LINES)
            .map(|i| dir.join(format!("{voice}_{i}.pcm")))
            .collect()
    }

    /// A bundled sample (`part` = line number or `reject`) as s16le, when there is
    /// one recorded at `sample_rate`.
    fn bundled_clip(&self, lang: Lang, voice: &str, part: &str, sample_rate: u32) -> Option<Vec<u8>> {
        let path = self.bundled.join(lang.code()).join(format!("{}_{part}.wav", file_voice(voice)));
        let (rate, pcm) = crate::adpcm::decode_wav(&std::fs::read(path).ok()?).ok()?;
        (rate == sample_rate && pcm.len() >= 3_200).then_some(pcm)
    }

    fn bundled_lines(&self, lang: Lang, voice: &str, sample_rate: u32) -> Option<Vec<Vec<u8>>> {
        (0..LINES)
            .map(|i| self.bundled_clip(lang, voice, &i.to_string(), sample_rate))
            .collect()
    }

    /// The voice's lines in `lang` (s16le at `sample_rate`): made here before, else
    /// the bundled ones, else a fresh Live session (`regen` forces one). `voice`
    /// must be [`valid_voice`].
    #[allow(clippy::too_many_arguments)]
    pub async fn lines(
        &self,
        api_key: &str,
        model: &str,
        thinking_level: &str,
        voice: &str,
        lang: Lang,
        sample_rate: u32,
        regen: bool,
    ) -> Result<Vec<Vec<u8>>> {
        anyhow::ensure!(valid_voice(voice), "unknown voice {voice}");
        let paths = self.paths(model, voice, lang);
        if !regen {
            if let Some(lines) = read_lines(&paths).or_else(|| self.bundled_lines(lang, voice, sample_rate)) {
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
        let sample = sample_lines(lang);
        let per_line = futures_util::future::try_join_all(sample.iter().map(|line| {
            crate::gemini_live::speak_lines(lang, api_key, model, thinking_level, voice, std::slice::from_ref(line), sample_rate)
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

    fn reject_path(&self, model: &str, voice: &str, lang: Lang) -> PathBuf {
        self.dir
            .join(model_dir(model))
            .join(lang.code())
            .join(format!("{}_reject.pcm", file_voice(voice)))
    }

    /// The blocked-number announcement in `voice` and `lang`: made here before, else
    /// the bundled one. The call path only ever reads this — it never calls Gemini.
    pub fn cached_reject_clip(&self, model: &str, voice: &str, lang: Lang, sample_rate: u32) -> Option<Vec<u8>> {
        std::fs::read(self.reject_path(model, voice, lang))
            .ok()
            .filter(|b| b.len() >= 3_200)
            .or_else(|| self.bundled_clip(lang, voice, "reject", sample_rate))
    }

    /// Make the announcement once (one short Live session, ~2 s) and keep it on
    /// disk; `Ok(false)` when it was there already (bundled or made before).
    #[allow(clippy::too_many_arguments)]
    pub async fn ensure_reject_clip(
        &self,
        api_key: &str,
        model: &str,
        thinking_level: &str,
        voice: &str,
        lang: Lang,
        sample_rate: u32,
    ) -> Result<bool> {
        if self.cached_reject_clip(model, voice, lang, sample_rate).is_some() {
            return Ok(false);
        }
        let _one = self.gen_lock.lock().await;
        if self.cached_reject_clip(model, voice, lang, sample_rate).is_some() {
            return Ok(false);
        }
        let t0 = std::time::Instant::now();
        let pcm = crate::gemini_live::speak_lines_as(
            lang,
            &crate::prompts::pack(lang).announce_system,
            api_key,
            model,
            thinking_level,
            file_voice(voice),
            &[reject_announce_text_in(lang)],
            sample_rate,
        )
        .await?
        .into_iter()
        .next()
        .unwrap_or_default();
        anyhow::ensure!(pcm.len() >= 3_200, "Gemini did not speak the announcement");
        let path = self.reject_path(model, voice, lang);
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
    reject_announce_text_in(crate::locale::current())
}

pub fn reject_announce_text_in(lang: Lang) -> &'static str {
    crate::locale::phrases(lang).unavailable
}

/// Voice of the blocked-number announcement: its own setting, else the AI's voice.
pub fn reject_voice(rt: &crate::settings_store::RuntimeSettings) -> &str {
    match (rt.reject_voice_name.trim(), rt.gemini_voice_name.trim()) {
        ("", "") => crate::gemini_live::default_voice_name(),
        ("", ai) => ai,
        (own, _) => own,
    }
}

/// Have the announcement ready for the current voice, language and model while it
/// can be needed: there are blocked numbers, or hidden numbers are not answered
/// (bundled: no Gemini call; else one, the first time only). `true` = nothing
/// left to do.
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
            rt.lang(),
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

/// `agent make-voice-samples [dir]`: record every voice's preview lines and
/// blocked-number announcement in every language into `dir` (default
/// `assets/voices`) — the samples the program ships with, so nobody spends their
/// own Gemini quota on a preview. Needs `GEMINI_API_KEY`; optional
/// `GEMINI_LIVE_MODEL`, `SAMPLE_RATE` (16000), `VOICE_SAMPLE_JOBS` (voices at once, 4),
/// `VOICE_SAMPLE_LANGS` / `VOICE_SAMPLE_VOICES` (comma lists). Files already there are
/// kept, so a stopped run carries on where it was. Every recording is checked
/// against Gemini's own transcript of it and made again when it strays.
pub async fn make_bundle(out: &Path) -> Result<()> {
    use futures_util::StreamExt;
    let key = std::env::var("GEMINI_API_KEY").unwrap_or_default();
    anyhow::ensure!(!key.trim().is_empty(), "set GEMINI_API_KEY");
    let model = std::env::var("GEMINI_LIVE_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| crate::gemini_live::default_model().to_string());
    let num = |var: &str, default: u32| std::env::var(var).ok().and_then(|v| v.trim().parse().ok()).unwrap_or(default);
    let sr = num("SAMPLE_RATE", 16_000);
    let jobs = num("VOICE_SAMPLE_JOBS", 4).clamp(1, 16) as usize;
    let list = |var: &str| -> Vec<String> {
        std::env::var(var)
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let (langs, voices) = (list("VOICE_SAMPLE_LANGS"), list("VOICE_SAMPLE_VOICES"));
    let todo: Vec<(Lang, &str)> = crate::locale::ALL
        .iter()
        .filter(|l| langs.is_empty() || langs.iter().any(|c| c == l.code()))
        .flat_map(|l| VOICES.iter().map(move |v| (*l, v.0)))
        .filter(|(_, v)| voices.is_empty() || voices.iter().any(|x| x == v))
        .collect();
    info!("voice samples: {} voice×language to check, model={model} sr={sr} jobs={jobs} → {}", todo.len(), out.display());
    let (key, model) = (&key, &model);
    let failed: Vec<String> = futures_util::stream::iter(todo)
        .map(|(lang, voice)| async move {
            let mut last = String::new();
            for attempt in 0..5u64 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(4 * attempt)).await;
                }
                match make_voice(out, key, model, lang, voice, sr).await {
                    Ok(()) => return None,
                    Err(e) => {
                        last = format!("{}/{voice}: {e:#}", lang.code());
                        warn!("voice sample try {} failed {last}", attempt + 1);
                    }
                }
            }
            Some(last)
        })
        .buffer_unordered(jobs)
        .filter_map(|r| async move { r })
        .collect()
        .await;
    anyhow::ensure!(failed.is_empty(), "{} not made:\n{}", failed.len(), failed.join("\n"));
    info!("voice samples: all there");
    Ok(())
}

/// The missing samples of one voice in one language.
async fn make_voice(out: &Path, key: &str, model: &str, lang: Lang, voice: &str, sr: u32) -> Result<()> {
    let dir = out.join(lang.code());
    std::fs::create_dir_all(&dir)?;
    let (phrases, pack) = (crate::locale::phrases(lang), crate::prompts::pack(lang));
    let parts = phrases
        .preview
        .iter()
        .enumerate()
        .map(|(i, line)| (i.to_string(), pack.preview_system.as_str(), *line))
        .chain(std::iter::once(("reject".to_string(), pack.announce_system.as_str(), phrases.unavailable)));
    for (part, system, line) in parts {
        let path = dir.join(format!("{voice}_{part}.wav"));
        if path.exists() {
            continue;
        }
        let (pcm, heard) = crate::gemini_live::speak_lines_heard(lang, system, key, model, "", voice, &[line], sr, true)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        let secs = pcm.len() as f32 / (sr as f32 * 2.0);
        // Gemini often sends no transcript; then the length is the check (anything
        // added makes it longer). Too short = cut off, with or without one.
        let (shortest, longest) = reading_time(line);
        anyhow::ensure!(secs >= shortest, "{part}: only {secs:.1} s for {line:?}");
        if heard.trim().is_empty() {
            anyhow::ensure!(secs <= longest, "{part}: no transcript and {secs:.1} s for {line:?}");
        } else {
            anyhow::ensure!(says_the_line(line, &heard), "{part}: asked for {line:?}, Gemini said {heard:?}");
        }
        anyhow::ensure!((0.4..=14.0).contains(&secs), "{part}: {secs:.1} s for {line:?}");
        write_atomic(&path, &crate::adpcm::encode_wav(&pcm, sr))?;
        info!("voice sample {}/{voice}_{part} {secs:.1}s heard={heard:?}", lang.code());
    }
    Ok(())
}

/// How long a plain reading of `line` takes at the fastest and the slowest:
/// CJK characters, kana and hangul 0.12–0.4 s, other letters and digits
/// 0.04–0.11 s, plus up to 1.2 s for the pauses.
fn reading_time(line: &str) -> (f32, f32) {
    let (wide, narrow) = line
        .chars()
        .filter(|c| c.is_alphanumeric())
        .fold((0u32, 0u32), |(w, n), c| if c >= '\u{2E80}' { (w + 1, n) } else { (w, n + 1) });
    let (wide, narrow) = (wide as f32, narrow as f32);
    ((0.12 * wide + 0.04 * narrow).max(0.4), 1.2 + 0.4 * wide + 0.11 * narrow)
}

/// Gemini's transcript of a recording against the line it was asked to read: most
/// of the line is there and not much else (it sometimes answers the line or adds
/// to it). Lenient on spelling — transcripts write numbers, kana or Chinese script
/// their own way.
fn says_the_line(line: &str, heard: &str) -> bool {
    let norm = |s: &str| -> Vec<char> {
        zhconv::zhconv(s, zhconv::Variant::ZhCN)
            .to_lowercase()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect()
    };
    let (a, b) = (norm(line), norm(heard));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    // Longest common subsequence.
    let mut prev = vec![0usize; b.len() + 1];
    for x in &a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y { prev[j] + 1 } else { cur[j].max(prev[j + 1]) };
        }
        prev = cur;
    }
    let common = prev[b.len()];
    common * 10 >= a.len() * 6 && b.len() <= a.len() + (a.len() * 2 / 5).max(4)
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
        let p = VoicePreviews::new(dir.clone(), dir.join("bundled"));
        let zh = Lang::ZhTw;
        assert!(p.cached_reject_clip("gemini-3.8-live", "Rasalgethi", zh, 16_000).is_none());
        let path = p.reject_path("models/gemini-3.8-live", "Rasalgethi", zh);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![7u8; 8_000]).unwrap();
        assert_eq!(p.cached_reject_clip("gemini-3.8-live", "Rasalgethi", zh, 16_000).unwrap().len(), 8_000);
        // Per voice: another voice has none yet.
        assert!(p.cached_reject_clip("gemini-3.8-live", "Charon", zh, 16_000).is_none());
        // Per language too.
        assert!(p.cached_reject_clip("gemini-3.8-live", "Rasalgethi", Lang::Ja, 16_000).is_none());
        // A free-text voice setting never escapes the cache directory.
        assert_eq!(file_voice("../../etc"), crate::gemini_live::default_voice_name());
        assert_eq!(file_voice(" Rasalgethi "), "Rasalgethi");
        assert!(reject_announce_text().contains("無法接通"));
        assert!(reject_announce_text_in(Lang::En).contains("not available"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_samples_are_used_per_language_and_rate() {
        let dir = std::env::temp_dir().join(format!("vp-bundled-{}", std::process::id()));
        let bundled = dir.join("bundled");
        let p = VoicePreviews::new(dir.join("cache"), bundled.clone());
        let tone: Vec<u8> = (0..16_000i32).flat_map(|i| (((i % 40) - 20) as i16 * 500).to_le_bytes()).collect();
        std::fs::create_dir_all(bundled.join("ja")).unwrap();
        for part in ["0", "1", "2", "reject"] {
            std::fs::write(bundled.join("ja").join(format!("Kore_{part}.wav")), crate::adpcm::encode_wav(&tone, 16_000)).unwrap();
        }
        let lines = p.bundled_lines(Lang::Ja, "Kore", 16_000).unwrap();
        assert_eq!((lines.len(), lines[0].len()), (LINES, tone.len()));
        assert!(p.bundled_lines(Lang::Ko, "Kore", 16_000).is_none());
        assert!(p.bundled_lines(Lang::Ja, "Charon", 16_000).is_none());
        // Recorded at another rate than the agent runs at: not used.
        assert!(p.bundled_lines(Lang::Ja, "Kore", 8_000).is_none());
        assert!(p.cached_reject_clip("gemini-3.8-live", "Kore", Lang::Ja, 16_000).is_some());
        assert!(p.cached_reject_clip("gemini-3.8-live", "Kore", Lang::ZhTw, 16_000).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recordings_are_checked_against_the_line() {
        assert!(says_the_line("喂？你好。", "喂，你好"));
        assert!(says_the_line("此號碼目前無法接通，請稍後再撥。", "此号码目前无法接通，请稍后再拨"));
        assert!(says_the_line("Hello?", "Hello."));
        // Answered the line instead of reading it, or went on talking.
        assert!(!says_the_line("好，掰掰。", "好的，請問還有什麼需要幫忙的嗎？我可以幫你查一下包裹的狀況"));
        assert!(!says_the_line("Hello?", ""));
        assert!(!says_the_line("Okay, bye.", "Sure! Is there anything else I can help you with today? Have a great one"));
        // Read the line, then went on.
        assert!(!says_the_line("Okay, bye.", "Okay, bye! Is there anything else I can help you with today"));
        assert!(!says_the_line("喂？你好。", "喂？你好。請問有什麼事嗎？"));
        // Kana written for a word the line has in kanji: a few more characters are fine.
        assert!(says_the_line("おかけになった電話は、現在おつなぎできません。", "おかけになった電話はげんざいおつなぎできません"));
        // No transcript: a short word may not run on into a sentence.
        assert!((2.5..3.0).contains(&reading_time("もしもし？").1));
        assert!(reading_time("Okay, bye.").1 < 2.5);
        assert!(reading_time("此號碼目前無法接通，請稍後再撥。").1 > 6.0);
        // Cut off: the announcement cannot be said in 0.7 s.
        assert!(reading_time("此號碼目前無法接通，請稍後再撥。").0 > 1.5);
        assert!(reading_time("Hello?").0 <= 0.5);
    }

    #[test]
    fn cache_needs_every_line() {
        let dir = std::env::temp_dir().join(format!("vp-test-{}", std::process::id()));
        let p = VoicePreviews::new(dir.clone(), dir.join("bundled"));
        let paths = p.paths("gemini-3.8-live", "Charon", Lang::ZhTw);
        assert!(read_lines(&paths).is_none());
        write_lines(&paths, &[vec![1u8; 4_000], vec![2u8; 4_000], vec![3u8; 4_000]]).unwrap();
        assert_eq!(read_lines(&paths).unwrap()[1][0], 2);
        std::fs::remove_file(&paths[2]).unwrap();
        assert!(read_lines(&paths).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

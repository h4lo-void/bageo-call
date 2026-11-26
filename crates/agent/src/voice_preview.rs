// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Preview on the settings page: a few phone lines in a Gemini voice with the
//! chosen background sound under them — what a caller hears. A voice's lines come from
//! what was made here before, else a downloaded voice pack for the region, else
//! one for the language or the samples that come with the program (recorded with
//! `agent make-voice-samples`; see [crate::voice_packs]), else from one short
//! Live session the first time, kept on disk after that.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result};
use tracing::{info, warn};

use crate::ambience::AmbienceMixer;
use crate::locale::Lang;

// The words are in the prompt book (data/prompts/<language>.json); these are their handles.
crate::prompt_book::prompt_texts! {
    EN_VOICE_PREVIEW_REGION_SYSTEM = "en/voice_preview_region_system";
}

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
    /// Downloaded voice packs, `<lang>[-<region>]/<Voice>_<part>.wav`
    /// ([crate::voice_packs]); looked at before the bundled samples.
    packs: Option<PathBuf>,
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

/// Whether `lang` speaks its own words: a core language, or an extended one
/// whose language pack has call speech ([crate::locale::phrases]). Any other
/// extended language reads English, so what is said and recorded for it is English.
fn own_words(lang: Lang) -> bool {
    match lang {
        Lang::Ext(e) => crate::locales_store::current().phrases(e).is_some(),
        _ => true,
    }
}

/// Start of the names of the previews made here. An extended language without
/// words keeps its English ones apart, so they are never taken for its own
/// once its language pack arrives.
fn made_prefix(lang: Lang) -> &'static str {
    if own_words(lang) { "" } else { "en_" }
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
            packs: None,
            gen_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Also read the voice packs downloaded into `dir`.
    pub fn with_packs(mut self, dir: PathBuf) -> Self {
        self.packs = Some(dir);
        self
    }

    /// Where the previews made with Gemini are kept (`<model>/<lang>/<Voice>_<part>.pcm`).
    pub(crate) fn cache_dir(&self) -> &Path {
        &self.dir
    }

    fn paths(&self, model: &str, voice: &str, lang: Lang) -> Vec<PathBuf> {
        // Per language, extended ones included: the lines are spoken in it.
        let dir = self.dir.join(model_dir(model)).join(lang.code());
        let prefix = made_prefix(lang);
        (0..LINES)
            .map(|i| dir.join(format!("{prefix}{voice}_{i}.pcm")))
            .collect()
    }

    /// A recorded sample (`part` = line number or `reject`) as s16le, when there is
    /// one recorded at `sample_rate`: from the pack of the language's region
    /// ([crate::voice_packs::region_for]), the language's pack or the bundled ones.
    fn bundled_clip(&self, lang: Lang, voice: &str, part: &str, sample_rate: u32) -> Option<Vec<u8>> {
        self.clip_in(lang, crate::voice_packs::region_for(lang), voice, part, sample_rate)
    }

    /// [Self::bundled_clip] for `region`: the region's pack, then the language's
    /// pack, then the bundled samples (a language that reads English: English's);
    /// the first that has the clip.
    pub(crate) fn clip_in(&self, lang: Lang, region: Option<&str>, voice: &str, part: &str, sample_rate: u32) -> Option<Vec<u8>> {
        let name = format!("{}_{part}.wav", file_voice(voice));
        let mut dirs = self.packs.as_deref().map(|p| crate::voice_packs::pack_dirs(p, lang.code(), region)).unwrap_or_default();
        // A language with words of its own never plays English's samples as its own.
        dirs.push(self.bundled.join((if own_words(lang) { lang } else { lang.base() }).code()));
        dirs.into_iter().find_map(|d| {
            let (rate, pcm) = crate::adpcm::decode_wav(&std::fs::read(d.join(&name)).ok()?).ok()?;
            (rate == sample_rate && pcm.len() >= 3_200).then_some(pcm)
        })
    }

    fn bundled_lines(&self, lang: Lang, voice: &str, sample_rate: u32) -> Option<Vec<Vec<u8>>> {
        (0..LINES)
            .map(|i| self.bundled_clip(lang, voice, &i.to_string(), sample_rate))
            .collect()
    }

    /// The voice's lines on disk: made here before, else recorded ones ([Self::bundled_clip]).
    pub(crate) fn disk_lines(&self, model: &str, voice: &str, lang: Lang, sample_rate: u32) -> Option<Vec<Vec<u8>>> {
        read_lines(&self.paths(model, voice, lang)).or_else(|| self.bundled_lines(lang, voice, sample_rate))
    }

    /// The voice's lines in `lang` (s16le at `sample_rate`): made here before, else
    /// a voice pack or the bundled ones, else a fresh Live session (`regen` forces
    /// one). `voice` must be [`valid_voice`].
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
            if let Some(lines) = self.disk_lines(model, voice, lang, sample_rate) {
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
            .join(format!("{}{}_reject.pcm", made_prefix(lang), file_voice(voice)))
    }

    /// The announcement as `lang` itself says it: made here before, else a voice
    /// pack's or the bundled one.
    fn own_reject_clip(&self, model: &str, voice: &str, lang: Lang, sample_rate: u32) -> Option<Vec<u8>> {
        std::fs::read(self.reject_path(model, voice, lang))
            .ok()
            .filter(|b| b.len() >= 3_200)
            .or_else(|| self.bundled_clip(lang, voice, "reject", sample_rate))
    }

    /// The blocked-number announcement in `voice` and `lang`: [Self::own_reject_clip],
    /// else (a language with words of its own that has none yet) the bundled
    /// English one, better than silence. The call path only ever reads this — it never calls Gemini.
    pub fn cached_reject_clip(&self, model: &str, voice: &str, lang: Lang, sample_rate: u32) -> Option<Vec<u8>> {
        self.own_reject_clip(model, voice, lang, sample_rate).or_else(|| {
            if own_words(lang) { self.bundled_clip(Lang::En, voice, "reject", sample_rate) } else { None }
        })
    }

    /// Make the announcement once (one short Live session, ~2 s) and keep it on
    /// disk; `Ok(false)` when the language's own was there already (bundled or
    /// made before; the English stand-in does not count).
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
        if self.own_reject_clip(model, voice, lang, sample_rate).is_some() {
            return Ok(false);
        }
        let _one = self.gen_lock.lock().await;
        if self.own_reject_clip(model, voice, lang, sample_rate).is_some() {
            return Ok(false);
        }
        // Where it goes is settled with the words, before the call: a language
        // pack that comes or goes meanwhile must not rename what was spoken.
        let path = self.reject_path(model, voice, lang);
        let words = reject_announce_text_in(lang);
        let t0 = std::time::Instant::now();
        let pcm = crate::gemini_live::speak_lines_as(
            lang,
            &crate::prompts::pack(lang).announce_system,
            api_key,
            model,
            thinking_level,
            file_voice(voice),
            &[words],
            sample_rate,
        )
        .await?
        .into_iter()
        .next()
        .unwrap_or_default();
        anyhow::ensure!(pcm.len() >= 3_200, "Gemini did not speak the announcement");
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
/// can be needed: someone is on the Blacklist (or blocked numbers wait to be
/// moved there), or hidden numbers are not answered (bundled: no Gemini call;
/// else one, the first time only). `true` = nothing left to do.
pub async fn prepare_reject_clip(
    previews: &VoicePreviews,
    rt: &crate::settings_store::RuntimeSettings,
    sample_rate: u32,
) -> bool {
    let needed = crate::contacts::blacklist_len() > 0 || !rt.pending_rejects().is_empty() || !rt.answer_unknown_callers;
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
/// own Gemini quota on a preview. Needs `GEMINI_API_KEY` (or `GEMINI_API_KEYS`,
/// a comma list used in turn; see [KeyRing]); optional
/// `GEMINI_LIVE_MODEL`, `SAMPLE_RATE` (16000), `VOICE_SAMPLE_JOBS` (voices at once, 4),
/// `VOICE_SAMPLE_LANGS` / `VOICE_SAMPLE_VOICES` (comma lists). Files already there are
/// kept, so a stopped run carries on where it was. Every recording is checked
/// against Gemini's own transcript of it and made again when it strays.
///
/// `VOICE_SAMPLE_REGION` (comma list of region codes, e.g. `tw,us`) records the
/// lines of those regions instead ([crate::voice_lines]), told the region's local
/// ways of talking and the middle talk style ([region_system]), into
/// `dir/<lang>-<region>/` — what `agent pack-voices` zips. Without it nothing changes.
pub async fn make_bundle(out: &Path) -> Result<()> {
    use futures_util::StreamExt;
    let ring = KeyRing::from_env();
    anyhow::ensure!(ring.live() > 0, "set GEMINI_API_KEY");
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
    let (langs, voices, regions) = (list("VOICE_SAMPLE_LANGS"), list("VOICE_SAMPLE_VOICES"), list("VOICE_SAMPLE_REGION"));
    // What to record: (language, region's code or none).
    let mut targets: Vec<(Lang, Option<String>)> = Vec::new();
    if regions.is_empty() {
        targets.extend(crate::locale::ALL.iter().map(|l| (*l, None)));
    } else {
        for code in &regions {
            let lines = crate::voice_lines::for_region(code).with_context(|| format!("no lines for the region {code} in voices/lines.json"))?;
            let lang = lines.lang().with_context(|| format!("{code}: the language {}", lines.lang))?;
            anyhow::ensure!(crate::country_profile::Region::of_code(code).is_some(), "{code} is not a region of the data pack");
            targets.push((lang, Some(code.trim().to_ascii_lowercase())));
        }
    }
    let todo: Vec<(Lang, Option<String>, &str)> = targets
        .into_iter()
        .filter(|(l, _)| langs.is_empty() || langs.iter().any(|c| c == l.code()))
        .flat_map(|(l, r)| VOICES.iter().map(move |v| (l, r.clone(), v.0)))
        .filter(|(_, _, v)| voices.is_empty() || voices.iter().any(|x| x == v))
        .collect();
    info!(
        "voice samples: {} voice×language to check, model={model} sr={sr} jobs={jobs} keys={} → {}",
        todo.len(),
        ring.live(),
        out.display()
    );
    let (ring, model) = (&ring, &model);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let stop = &stop;
    let failed: Vec<String> = futures_util::stream::iter(todo)
        .map(|(lang, region, voice)| async move {
            let mut last = String::new();
            for attempt in 0..5u64 {
                if attempt > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(4 * attempt)).await;
                }
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return Some(format!("{}/{voice}: stopped, every key is used up", lang.code()));
                }
                let Some((slot, key)) = ring.pick() else {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    return Some(format!("{}/{voice}: every key is used up", lang.code()));
                };
                match make_voice(out, key, model, lang, region.as_deref(), voice, sr).await {
                    Ok(()) => {
                        ring.worked(slot);
                        return None;
                    }
                    Err(e) => {
                        let at = region.as_deref().map(|r| format!("-{r}")).unwrap_or_default();
                        last = ring.mask(&format!("{}{at}/{voice}: {e:#}", lang.code()));
                        if quota_error(&last) {
                            ring.refused(slot);
                        }
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
    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        anyhow::bail!("every Gemini key is used up; {} not made", failed.len());
    }
    anyhow::ensure!(failed.is_empty(), "{} not made:\n{}", failed.len(), failed.join("\n"));
    info!("voice samples: all there");
    Ok(())
}

/// The keys of a recording run, used in turn. A key Gemini refuses
/// [QUOTA_STRIKES] times in a row with its limit ([quota_error]) is laid aside;
/// with none left the run stops (no other key is ever tried). Keys are never
/// printed: [Self::mask] takes them out of any text.
struct KeyRing {
    keys: Vec<String>,
    strikes: Vec<std::sync::atomic::AtomicU32>,
    next: std::sync::atomic::AtomicUsize,
}

const QUOTA_STRIKES: u32 = 3;

impl KeyRing {
    /// `GEMINI_API_KEYS` (comma, space or line separated), then `GEMINI_API_KEY`; each once.
    fn from_env() -> Self {
        let mut keys: Vec<String> = Vec::new();
        for var in ["GEMINI_API_KEYS", "GEMINI_API_KEY"] {
            for k in std::env::var(var).unwrap_or_default().split(|c: char| c == ',' || c.is_whitespace()) {
                if !k.is_empty() && !keys.iter().any(|x| x == k) {
                    keys.push(k.to_string());
                }
            }
        }
        Self::new(keys)
    }

    fn new(keys: Vec<String>) -> Self {
        let strikes = keys.iter().map(|_| std::sync::atomic::AtomicU32::new(0)).collect();
        Self { keys, strikes, next: std::sync::atomic::AtomicUsize::new(0) }
    }

    fn laid_aside(&self, i: usize) -> bool {
        self.strikes[i].load(std::sync::atomic::Ordering::Relaxed) >= QUOTA_STRIKES
    }

    /// How many keys can still be used.
    fn live(&self) -> usize {
        (0..self.keys.len()).filter(|i| !self.laid_aside(*i)).count()
    }

    /// The next key in turn that is not laid aside, with its place; None when all are.
    fn pick(&self) -> Option<(usize, &str)> {
        for _ in 0..self.keys.len() {
            let i = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.keys.len();
            if !self.laid_aside(i) {
                return Some((i, self.keys[i].as_str()));
            }
        }
        None
    }

    /// The key at `i` did its work: its refusals so far do not count.
    fn worked(&self, i: usize) {
        self.strikes[i].store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Gemini refused the key at `i` with its limit.
    fn refused(&self, i: usize) {
        self.strikes[i].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// `text` without any of the keys.
    fn mask(&self, text: &str) -> String {
        self.keys.iter().fold(text.to_string(), |t, k| t.replace(k.as_str(), "***"))
    }
}

/// Why Gemini could not be used, told from the text of its error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiFailKind {
    /// The limit is reached: quota, rate, or the project's spending cap.
    Quota,
    /// The key is not valid, expired or has no permission.
    Key,
    /// Anything else (network, timeout, a model that did not answer).
    Other,
}

impl AiFailKind {
    /// The call-record event for it; `Other` has none.
    pub fn event(self) -> Option<&'static str> {
        match self {
            AiFailKind::Quota => Some("ai_fail_quota"),
            AiFailKind::Key => Some("ai_fail_key"),
            AiFailKind::Other => None,
        }
    }

    /// The kind a call-record event names.
    pub fn from_event(event: &str) -> Option<AiFailKind> {
        match event {
            "ai_fail_quota" => Some(AiFailKind::Quota),
            "ai_fail_key" => Some(AiFailKind::Key),
            _ => None,
        }
    }
}

/// `code` as an HTTP status code: a number of its own (not a piece of `4290` or of an
/// address) that follows a word saying it is a status (`"code":429`, `status 429`,
/// `HTTP/1.1 403`, `error 429`). A bare `429 ms` or `10.0.0.403` is not one.
fn has_status_code(text: &str, code: &str) -> bool {
    let b = text.as_bytes();
    let word_ends_at = |end: usize, words: &[&str]| {
        words.iter().any(|w| {
            end >= w.len()
                && text.is_char_boundary(end - w.len())
                && &text[end - w.len()..end] == *w
                && (end == w.len() || !b[end - w.len() - 1].is_ascii_alphanumeric())
        })
    };
    let skip_back = |mut end: usize, keep: fn(u8) -> bool| {
        while end > 0 && keep(b[end - 1]) {
            end -= 1;
        }
        end
    };
    text.match_indices(code).any(|(i, _)| {
        let end = i + code.len();
        if !((i == 0 || !b[i - 1].is_ascii_digit()) && (end >= b.len() || !b[end].is_ascii_digit())) {
            return false;
        }
        // Up to a few separators (`":`, `: `, a space) between the word and the number.
        let sep = skip_back(i, |c| !c.is_ascii_alphanumeric());
        if i - sep > 4 {
            return false;
        }
        if word_ends_at(sep, &["code", "status", "http", "https", "error"]) {
            return true;
        }
        // `HTTP/1.1 403`: a protocol version sits between the word and the number.
        if sep > 0 && b[sep - 1].is_ascii_digit() {
            let ver = skip_back(sep, |c| c.is_ascii_digit() || c == b'.');
            let before = skip_back(ver, |c| !c.is_ascii_alphanumeric());
            return ver - before <= 2 && word_ends_at(before, &["http", "https"]);
        }
        false
    })
}

/// The kind of failure an error of Gemini's (a close reason, an error body, a
/// connection error) tells: a reached limit, a key that does not work, or neither.
pub fn ai_fail_kind(text: &str) -> AiFailKind {
    let t = text.to_ascii_lowercase();
    let any = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if any(&[
        "spending cap",
        "quota",
        "resource_exhausted",
        "resource exhausted",
        "rate limit",
        "too many requests",
        "billing",
    ]) || has_status_code(&t, "429")
    {
        return AiFailKind::Quota;
    }
    if any(&[
        "api key not valid",
        "api_key_invalid",
        "api key expired",
        "permission_denied",
        "permission denied",
        "unauthenticated",
    ]) || has_status_code(&t, "401")
        || has_status_code(&t, "403")
    {
        return AiFailKind::Key;
    }
    AiFailKind::Other
}

/// An error that says Gemini's limit (rate, quota) is reached.
fn quota_error(text: &str) -> bool {
    ai_fail_kind(text) == AiFailKind::Quota
}

/// The language code Gemini is given when a recording is transcribed: the
/// official Gemini Live code of the language (the settings' own speech codes
/// `zh-TW`, `cmn-CN` and `es-ES` are not on the list; Chinese is `cmn`).
fn official_stt(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw | Lang::ZhCn => "cmn",
        Lang::En => "en-US",
        Lang::Ja => "ja-JP",
        Lang::Ko => "ko-KR",
        Lang::Es => "es-US",
        Lang::Fr => "fr-FR",
        Lang::De => "de-DE",
        Lang::Pt => "pt-BR",
        Lang::Ext(e) => e.official(),
    }
}

/// The instruction a region's preview lines are recorded with: the language's own
/// ([crate::prompts::Pack::preview_system]), then the region's local ways of talking
/// and the middle cell of the talk-style slider, the same texts a call's prompt
/// takes from the data pack. Situation and tone only; the line stays as given.
fn region_system(base: &str, region: crate::country_profile::Region, lang: Lang) -> String {
    const NOTE_TW: &str = "以上只影響語氣、節奏和客氣程度，台詞本身一個字都不改。";
    let owner = crate::prompts::owner_label(lang, "");
    let ways = crate::country_profile::habits_block(region, lang);
    let style = crate::country_profile::style_line(region, lang, false, 3, &owner);
    if ways.is_empty() && style.is_empty() {
        return base.to_string();
    }
    let note = match lang {
        Lang::ZhTw => NOTE_TW.to_string(),
        Lang::ZhCn => crate::zh_convert::to_cn(NOTE_TW),
        _ => EN_VOICE_PREVIEW_REGION_SYSTEM.get().to_string(),
    };
    format!("{base}{ways}{style}\n{note}")
}

/// The missing samples of one voice in one language (of one region's lines when `region` is given).
async fn make_voice(out: &Path, key: &str, model: &str, lang: Lang, region: Option<&str>, voice: &str, sr: u32) -> Result<()> {
    let pack = crate::prompts::pack(lang);
    let (dir, previews, preview_system, reject, code_in) = match region {
        None => {
            let phrases = crate::locale::phrases(lang);
            let previews: Vec<String> = phrases.preview.iter().map(|l| l.to_string()).collect();
            (out.join(lang.base().code()), previews, pack.preview_system.clone(), phrases.unavailable.to_string(), official_stt(lang).to_string())
        }
        Some(code) => {
            let lines = crate::voice_lines::for_region(code).with_context(|| format!("no lines for the region {code}"))?;
            let r = crate::country_profile::Region::of_code(code).with_context(|| format!("{code} is not a region of the data pack"))?;
            (
                out.join(crate::voice_packs::folder_id(lang.code(), Some(code))),
                lines.preview.clone(),
                region_system(&pack.preview_system, r, lang),
                lines.reject.clone(),
                lines.gemini_lang.clone(),
            )
        }
    };
    std::fs::create_dir_all(&dir)?;
    let parts = previews
        .iter()
        .enumerate()
        .map(|(i, line)| (i.to_string(), preview_system.as_str(), line.as_str()))
        .chain(std::iter::once(("reject".to_string(), pack.announce_system.as_str(), reject.as_str())));
    for (part, system, line) in parts {
        let path = dir.join(format!("{voice}_{part}.wav"));
        if path.exists() {
            continue;
        }
        let (pcm, heard) = crate::gemini_live::speak_lines_coded(lang, system, key, model, "", voice, &[line], sr, Some(code_in.as_str()))
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
        info!("voice sample {}/{voice}_{part} {secs:.1}s heard={heard:?}", dir.file_name().and_then(|n| n.to_str()).unwrap_or(""));
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
        crate::zh_convert::to_cn(s)
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

    fn ext(code: &str) -> Lang {
        Lang::Ext(crate::locale::Ext::from_code(code).unwrap_or_else(|| panic!("{code}")))
    }

    /// Recordings that come with the program, English only: `bundled/en/Kore_<part>.wav`.
    fn bundle_english(bundled: &Path, samples: usize) -> usize {
        let tone: Vec<u8> = (0..samples as i32).flat_map(|i| (((i % 40) - 20) as i16 * 500).to_le_bytes()).collect();
        std::fs::create_dir_all(bundled.join("en")).unwrap();
        for part in ["0", "1", "2", "reject"] {
            std::fs::write(bundled.join("en").join(format!("Kore_{part}.wav")), crate::adpcm::encode_wav(&tone, 16_000)).unwrap();
        }
        tone.len()
    }

    #[test]
    fn extended_languages_keep_their_previews_in_their_own_folders() {
        for with_words in [true, false] {
            if with_words {
                crate::locales_store::testing::use_table();
            } else {
                crate::locales_store::testing::use_empty();
            }
            let p = VoicePreviews::new(std::path::PathBuf::from("cache"), std::path::PathBuf::from("bundled"));
            let model = "models/gemini-3.8-live";
            let en = p.paths(model, "Kore", Lang::En);
            let en_reject = p.reject_path(model, "Kore", Lang::En);
            for code in ["it", "th", "vi", "ar", "hi"] {
                let l = ext(code);
                let own = p.paths(model, "Kore", l);
                let reject = p.reject_path(model, "Kore", l);
                assert_ne!(own, en, "{code} words={with_words}");
                assert_ne!(reject, en_reject, "{code} words={with_words}");
                for path in own.iter().chain([&reject]) {
                    assert_eq!(path.parent().unwrap().file_name().unwrap(), code, "{code} words={with_words}");
                    assert!(!path.starts_with(PathBuf::from("cache").join("gemini-3.8-live").join("en")), "{code}");
                }
            }
            // Core languages keep the places their made previews have always been in.
            for (l, code) in [(Lang::En, "en"), (Lang::Ja, "ja"), (Lang::De, "de"), (Lang::ZhTw, "zh-TW")] {
                let dir = PathBuf::from("cache").join("gemini-3.8-live").join(code);
                assert_eq!(p.paths(model, "Kore", l), (0..LINES).map(|i| dir.join(format!("Kore_{i}.pcm"))).collect::<Vec<_>>(), "{code}");
                assert_eq!(p.reject_path(model, "Kore", l), dir.join("Kore_reject.pcm"), "{code}");
            }
        }
        crate::locales_store::testing::use_builtin();
    }

    #[test]
    fn announcement_and_sample_say_the_translation_when_there_is_one() {
        crate::locales_store::testing::use_table();
        // Test translations: it, th, ar, fa have call speech; vi, hi, ru have none.
        for code in ["it", "th", "ar", "fa"] {
            let l = ext(code);
            assert_eq!(reject_announce_text_in(l), crate::locale::phrases(l).unavailable, "{code}");
            assert_ne!(reject_announce_text_in(l), reject_announce_text_in(Lang::En), "{code}");
            assert_ne!(sample_lines(l), sample_lines(Lang::En), "{code}");
            assert!(own_words(l), "{code}");
        }
        assert!(reject_announce_text_in(ext("it")).contains("richiamo"));
        for code in ["vi", "hi", "ru"] {
            let l = ext(code);
            assert_eq!(reject_announce_text_in(l), reject_announce_text_in(Lang::En), "{code}");
            assert_eq!(sample_lines(l), sample_lines(Lang::En), "{code}");
            assert!(!own_words(l), "{code}");
        }
        for l in [Lang::En, Lang::Ja, Lang::Ko, Lang::De, Lang::ZhTw] {
            assert!(own_words(l));
        }
        crate::locales_store::testing::use_empty();
        assert_eq!(reject_announce_text_in(ext("it")), reject_announce_text_in(Lang::En));
        assert!(!own_words(ext("it")));
        crate::locales_store::testing::use_builtin();
    }

    #[tokio::test]
    async fn english_is_never_taken_for_the_announcement_of_a_language_with_words() {
        crate::locales_store::testing::use_table();
        let dir = std::env::temp_dir().join(format!("vp-ext-words-{}", uuid::Uuid::new_v4()));
        let p = VoicePreviews::new(dir.join("cache"), dir.join("bundled"));
        let model = "gemini-3.8-live";
        let english = bundle_english(&dir.join("bundled"), 6_000);
        // An English owner made the announcement here before.
        let en_made = p.reject_path(model, "Kore", Lang::En);
        std::fs::create_dir_all(en_made.parent().unwrap()).unwrap();
        std::fs::write(&en_made, vec![7u8; 9_000]).unwrap();
        for code in ["it", "th", "ar"] {
            let l = ext(code);
            // Not its own: neither the English cache nor the bundled English sample.
            assert!(p.own_reject_clip(model, "Kore", l, 16_000).is_none(), "{code}");
            assert!(p.disk_lines(model, "Kore", l, 16_000).is_none(), "{code}");
            // On a call, until its own is made, the bundled English one beats silence.
            assert_eq!(p.cached_reject_clip(model, "Kore", l, 16_000).unwrap().len(), english, "{code}");
            // Made for it: its own folder only, and the English owner is unaffected.
            let own = p.reject_path(model, "Kore", l);
            std::fs::create_dir_all(own.parent().unwrap()).unwrap();
            std::fs::write(&own, vec![3u8; 5_000]).unwrap();
            assert_eq!(p.own_reject_clip(model, "Kore", l, 16_000).unwrap().len(), 5_000, "{code}");
            assert_eq!(p.cached_reject_clip(model, "Kore", l, 16_000).unwrap().len(), 5_000, "{code}");
            // Nothing left to make: no Gemini session is started.
            assert!(!p.ensure_reject_clip("", model, "", "Kore", l, 16_000).await.unwrap(), "{code}");
        }
        assert_eq!(std::fs::read(&en_made).unwrap().len(), 9_000);
        assert_eq!(p.cached_reject_clip(model, "Kore", Lang::En, 16_000).unwrap().len(), 9_000);
        let _ = std::fs::remove_dir_all(&dir);
        crate::locales_store::testing::use_builtin();
    }

    #[tokio::test]
    async fn a_language_without_words_keeps_reading_english_but_never_from_another_languages_folder() {
        crate::locales_store::testing::use_table();
        let dir = std::env::temp_dir().join(format!("vp-ext-nowords-{}", uuid::Uuid::new_v4()));
        let p = VoicePreviews::new(dir.join("cache"), dir.join("bundled"));
        let model = "gemini-3.8-live";
        let en_made = p.reject_path(model, "Kore", Lang::En);
        std::fs::create_dir_all(en_made.parent().unwrap()).unwrap();
        std::fs::write(&en_made, vec![7u8; 9_000]).unwrap();
        for code in ["vi", "hi", "ru"] {
            let l = ext(code);
            // The English owner's cache is not read for it.
            assert!(p.cached_reject_clip(model, "Kore", l, 16_000).is_none(), "{code}");
        }
        // With the bundled English sample, the announcement is English, as before, and counts as there.
        let english = bundle_english(&dir.join("bundled"), 6_000);
        for code in ["vi", "hi", "ru"] {
            let l = ext(code);
            assert_eq!(p.own_reject_clip(model, "Kore", l, 16_000).unwrap().len(), english, "{code}");
            assert_eq!(p.cached_reject_clip(model, "Kore", l, 16_000).unwrap().len(), english, "{code}");
            assert!(!p.ensure_reject_clip("", model, "", "Kore", l, 16_000).await.unwrap(), "{code}");
            assert_eq!(p.disk_lines(model, "Kore", l, 16_000).unwrap()[0].len(), english, "{code}");
            // What gets made for it (English words) is kept apart from its own, future, recordings.
            let made = p.reject_path(model, "Kore", l);
            assert_eq!(made.file_name().unwrap(), "en_Kore_reject.pcm", "{code}");
            assert_eq!(made.parent().unwrap().file_name().unwrap(), code, "{code}");
            std::fs::create_dir_all(made.parent().unwrap()).unwrap();
            std::fs::write(&made, vec![5u8; 4_000]).unwrap();
            assert_eq!(p.cached_reject_clip(model, "Kore", l, 16_000).unwrap().len(), 4_000, "{code}");
            assert_eq!(p.paths(model, "Kore", l)[0].file_name().unwrap(), "en_Kore_0.pcm", "{code}");
        }
        // Once the language pack arrives its own announcement is still to be made.
        crate::locales_store::testing::use_table();
        assert_eq!(p.reject_path(model, "Kore", ext("it")).file_name().unwrap(), "Kore_reject.pcm");
        let _ = std::fs::remove_dir_all(&dir);
        crate::locales_store::testing::use_builtin();
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
    fn samples_are_looked_for_in_the_cache_region_pack_language_pack_then_bundled() {
        let dir = std::env::temp_dir().join(format!("vp-order-{}", uuid::Uuid::new_v4()));
        let (packs, bundled) = (dir.join("packs"), dir.join("bundled"));
        let p = VoicePreviews::new(dir.join("cache"), bundled.clone()).with_packs(packs.clone());
        let tone = |n: i32| -> Vec<u8> { (0..n).flat_map(|i| (((i % 40) - 20) as i16 * 500).to_le_bytes()).collect() };
        // Each place holds recordings of its own length, so the test can tell where one came from.
        let put = |d: &Path, n: i32| -> usize {
            std::fs::create_dir_all(d).unwrap();
            let wav = crate::adpcm::encode_wav(&tone(n), 16_000);
            for part in ["0", "1", "2", "reject"] {
                std::fs::write(d.join(format!("Kore_{part}.wav")), &wav).unwrap();
            }
            crate::adpcm::decode_wav(&wav).unwrap().1.len()
        };
        let len = |lang: Lang, region: &str| p.clip_in(lang, Some(region), "Kore", "1", 16_000).map(|c| c.len());
        for (lang, region) in [(Lang::ZhTw, "tw"), (Lang::En, "us"), (Lang::Ja, "jp"), (Lang::Es, "latam")] {
            // Nothing recorded: the preview is made by Gemini.
            assert_eq!(len(lang, region), None, "{lang:?}");
            let b = put(&bundled.join(lang.code()), 4_000);
            assert_eq!(len(lang, region), Some(b), "{lang:?} bundled");
            // Pack folders are lowercase (voice_packs::Entry::id).
            let l = put(&packs.join(lang.code().to_ascii_lowercase()), 5_000);
            assert_eq!(len(lang, region), Some(l), "{lang:?} language pack");
            let r = put(&packs.join(format!("{}-{region}", lang.code()).to_ascii_lowercase()), 6_000);
            assert_eq!(len(lang, region), Some(r), "{lang:?} region pack");
            // Another region's pack is not this region's.
            assert_eq!(len(lang, "zz"), Some(l), "{lang:?} other region");
            assert!(b != l && l != r);
        }
        // Made here before comes first.
        let made = vec![1u8; 9_000];
        for path in p.paths("gemini-3.8-live", "Kore", Lang::Ja) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, &made).unwrap();
        }
        assert_eq!(p.disk_lines("gemini-3.8-live", "Kore", Lang::Ja, 16_000).unwrap()[0].len(), 9_000);
        // Without a packs folder (older installs, tests): the bundled samples, as before.
        let plain = VoicePreviews::new(dir.join("cache2"), bundled.clone());
        let bundled_len = crate::adpcm::decode_wav(&std::fs::read(bundled.join("ja").join("Kore_1.wav")).unwrap()).unwrap().1.len();
        assert_eq!(plain.clip_in(Lang::Ja, Some("jp"), "Kore", "1", 16_000).map(|c| c.len()), Some(bundled_len));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_used_in_turn_and_laid_aside_after_their_limit() {
        for l in crate::locale::ALL {
            assert!(bageo_packs::data::is_official_lang(official_stt(l)), "{l:?}");
        }
        assert_eq!((official_stt(Lang::ZhTw), official_stt(Lang::Es)), ("cmn", "es-US"));
        let ring = KeyRing::new(vec!["k-one".into(), "k-two".into(), "k-three".into()]);
        let order: Vec<usize> = (0..4).map(|_| ring.pick().unwrap().0).collect();
        assert_eq!(order, [0, 1, 2, 0]);
        // Two refusals in a row are not enough, and a good recording clears them.
        ring.refused(1);
        ring.refused(1);
        assert_eq!(ring.live(), 3);
        ring.worked(1);
        ring.refused(1);
        ring.refused(1);
        assert_eq!(ring.live(), 3);
        ring.refused(1);
        assert_eq!(ring.live(), 2);
        for _ in 0..6 {
            assert_ne!(ring.pick().unwrap().0, 1);
        }
        for i in [0, 2] {
            for _ in 0..QUOTA_STRIKES {
                ring.refused(i);
            }
        }
        assert!(ring.pick().is_none());
        assert_eq!(ring.live(), 0);
    }

    #[test]
    fn keys_never_show_in_a_message_and_a_limit_is_told_from_other_failures() {
        let ring = KeyRing::new(vec!["AIzaFakeKeyOne".into(), "AQ.FakeKeyTwo".into()]);
        let m = ring.mask("cannot reach wss://x?key=AIzaFakeKeyOne and AQ.FakeKeyTwo: 429");
        assert!(!m.contains("Fake") && m.contains("429"), "{m}");
        assert!(quota_error("HTTP error: 429 Too Many Requests"));
        assert!(quota_error("Gemini setup error: {\"status\":\"RESOURCE_EXHAUSTED\"}"));
        assert!(quota_error("You exceeded your current quota"));
        // A recording that strays, in any language, is not a limit.
        assert!(!quota_error("1: asked for \"もしもし？\", Gemini said \"はい、どちらさまですか\""));
        assert!(!quota_error("0: asked for \"Hola, buenas tardes.\", Gemini said \"Hola\""));
        assert!(!quota_error("reject: only 0.3 s for \"您撥的電話目前無法接聽，請稍後再撥，謝謝。\""));
        assert!(!quota_error("connecting to Gemini timed out"));
    }

    #[test]
    fn a_failure_of_gemini_is_told_a_limit_from_a_key_from_the_rest() {
        use AiFailKind::{Key, Other, Quota};
        let cases: [(&str, AiFailKind); 25] = [
            (
                "Your project has exceeded its monthly spending cap. Please go to AI Studio at https://ai.studio/spend to manage your project spend cap.",
                Quota,
            ),
            ("gemini ws closed before setup: Some(CloseFrame { code: Policy, reason: \"Your project has exceeded its monthly spending cap.\" })", Quota),
            ("gemini setup error: {\"code\":429,\"status\":\"RESOURCE_EXHAUSTED\"}", Quota),
            ("HTTP error: 429 Too Many Requests", Quota),
            ("You exceeded your current quota, please check your plan and billing details", Quota),
            ("Resource exhausted (rate limit)", Quota),
            ("gemini setup error: {\"message\":\"API key not valid. Please pass a valid API key.\",\"status\":\"INVALID_ARGUMENT\",\"reason\":\"API_KEY_INVALID\"}", Key),
            ("API key expired. Please renew the API key.", Key),
            ("gemini server error: {\"code\":403,\"status\":\"PERMISSION_DENIED\"}", Key),
            ("HTTP error: 401 Unauthorized (UNAUTHENTICATED)", Key),
            ("Permission denied on resource project", Key),
            ("cannot reach Gemini: dns error: failed to lookup address information", Other),
            ("connecting to Gemini timed out", Other),
            ("Gemini setup timed out", Other),
            ("gemini ws closed (none) before setup", Other),
            // A number that only holds a status code's digits is not that code.
            ("Gemini produced no audio in 14290 ms (frames 24031)", Other),
            // A bare number is not a status code either.
            ("Gemini produced no audio in 429 ms (frames 24)", Other),
            ("gemini setup timed out after 401 ms", Other),
            ("dial tcp 10.0.0.403:443 refused", Other),
            // A status code is told by the word before it.
            ("{\"code\":429}", Quota),
            ("status: 401", Key),
            ("HTTP 403 Forbidden", Key),
            ("HTTP/1.1 403", Key),
            ("error 429", Quota),
            ("Status Code: 401", Key),
        ];
        for (text, want) in cases {
            assert_eq!(ai_fail_kind(text), want, "{text}");
        }
        assert_eq!(ai_fail_kind(""), Other);
        assert_eq!((Quota.event(), Key.event(), Other.event()), (Some("ai_fail_quota"), Some("ai_fail_key"), None));
        assert_eq!(AiFailKind::from_event("ai_fail_key"), Some(Key));
        assert_eq!(AiFailKind::from_event("persona_ai_fail_hangup"), None);
    }

    /// The announcement of an AI that cannot answer is the blocked-number one:
    /// the language's own recording, else the bundled English one, else none.
    #[test]
    fn the_unavailable_announcement_is_the_languages_own_or_the_english_one() {
        let dir = std::env::temp_dir().join(format!("vp-aifail-{}", uuid::Uuid::new_v4()));
        let bundled = dir.join("bundled");
        let p = VoicePreviews::new(dir.join("cache"), bundled.clone());
        let model = "gemini-3.8-live";
        let langs = [Lang::En, Lang::ZhTw, Lang::ZhCn, Lang::Ja, Lang::Ko, Lang::Es, Lang::Fr, Lang::De, Lang::Pt];
        // Every language records its own length, so the test can tell whose it was.
        let put = |lang: Lang, samples: i32| -> usize {
            let tone: Vec<u8> = (0..samples).flat_map(|i| (((i % 40) - 20) as i16 * 500).to_le_bytes()).collect();
            let d = bundled.join(lang.code());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("Kore_reject.wav"), crate::adpcm::encode_wav(&tone, 16_000)).unwrap();
            tone.len()
        };
        let lens: Vec<usize> = langs.iter().enumerate().map(|(i, l)| put(*l, 4_000 + 500 * i as i32)).collect();
        for (l, want) in langs.iter().zip(&lens) {
            assert_eq!(p.cached_reject_clip(model, "Kore", *l, 16_000).map(|c| c.len()), Some(*want), "{l:?}");
        }
        // Another voice or another rate: nothing recorded.
        assert!(p.cached_reject_clip(model, "Charon", Lang::Ja, 16_000).is_none());
        assert!(p.cached_reject_clip(model, "Kore", Lang::Ja, 8_000).is_none());
        // A language with no recording of its own plays English's, as a blocked number does.
        for l in &langs[1..] {
            std::fs::remove_file(bundled.join(l.code()).join("Kore_reject.wav")).unwrap();
            assert_eq!(p.cached_reject_clip(model, "Kore", *l, 16_000).map(|c| c.len()), Some(lens[0]), "{l:?}");
        }
        // A language beyond the nine that has no words here reads English: the bundled English one.
        let vi = Lang::Ext(crate::locale::Ext::from_code("vi").expect("vi"));
        assert_eq!(p.cached_reject_clip(model, "Kore", vi, 16_000).map(|c| c.len()), Some(lens[0]));
        // Nothing recorded at all: nothing to play (and nothing is made: the call path never asks Gemini).
        std::fs::remove_file(bundled.join("en").join("Kore_reject.wav")).unwrap();
        for l in langs.into_iter().chain([vi]) {
            assert!(p.cached_reject_clip(model, "Kore", l, 16_000).is_none(), "{l:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_regions_recording_instruction_has_its_ways_and_talk_style_and_keeps_the_line_fixed() {
        use crate::country_profile::{habits_block, style_line, Region};
        let cases = [
            ("tw", Lang::ZhTw),
            ("cn", Lang::ZhCn),
            ("us", Lang::En),
            ("jp", Lang::Ja),
            ("kr", Lang::Ko),
            ("latam", Lang::Es),
            ("br", Lang::Pt),
            ("fr", Lang::Fr),
            ("de", Lang::De),
        ];
        for (code, lang) in cases {
            let r = Region::of_code(code).unwrap_or_else(|| panic!("region {code}"));
            let base = crate::prompts::pack(lang).preview_system.clone();
            let s = region_system(&base, r, lang);
            assert!(s.starts_with(&base), "{code}");
            let owner = crate::prompts::owner_label(lang, "");
            for part in [habits_block(r, lang), style_line(r, lang, false, 3, &owner)] {
                assert!(s.contains(part.trim()), "{code}: {part:?}");
            }
            assert!(!s.contains('{') && !s.contains('}'), "{code}");
            // Only tone: the last word is that the line is not changed.
            assert!(s.ends_with(if lang.is_zh() { "不改。" } else { "word for word." }) || s.ends_with("一个字都不改。"), "{code}: {s}");
        }
        // Chinese of the two scripts.
        let r = Region::of_code("tw").unwrap();
        assert!(region_system("x", r, Lang::ZhTw).contains("一個字"));
        assert!(region_system("x", r, Lang::ZhCn).contains("一个字"));
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

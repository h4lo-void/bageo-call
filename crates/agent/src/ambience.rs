// SPDX-License-Identifier: AGPL-3.0-only OR LicenseRef-Bageo-Commercial
// SPDX-FileCopyrightText: h4lo-void <https://github.com/h4lo-void/bageo-call>

//! Background sound (ambience): a looped room ambience — office, home or
//! street — under the AI's voice, so the caller hears a place, not a studio.
//!
//! Phones whose capture returns our own playback (Pixel 7/7a) only get the bed
//! while the AI talks (the uplink is masked then) and it fades out after each
//! phrase, like a phone's noise suppression; otherwise Gemini would hear the
//! bed as the caller. Echo-free phones (Pixel 9, per the call's echo check)
//! keep it running through the pauses.

//! The bed files are not in the installers. They live in bageo-packs and are
//! downloaded once, when the owner picks one or plays a preview of it (never
//! at start-up, never in the background, never during a call). Beds found in
//! the assets folder (older installs) or in the download folder load at start-up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

use crate::calls_http::{api_error, token_ok_parts, unauthorized, CallsState, TokenQuery};
use crate::pack_update::{self, Fail, Source, Staged};

/// The list of beds in a bageo-packs release.
pub(crate) const INDEX_NAME: &str = "bageo-ambience-index.json";
/// The index format this Bageo reads.
const INDEX_FORMAT: u64 = 1;
const MAX_INDEX_BYTES: u64 = 256 * 1024;
/// One bed file (the longest shipped one is about 2.5 MB).
pub(crate) const MAX_BED_BYTES: u64 = 8 << 20;
/// A bed shorter than this is refused.
const MIN_BED_SECS: usize = 5;
const TOTAL_TIMEOUT: Duration = Duration::from_secs(180);
const INCOMING_PREFIX: &str = ".incoming-";

/// Bed files `ambience_<kind>.wav` in the assets dir: 150 Hz–7 kHz, end
/// crossfaded into the start, RMS normalized to [`BED_REF_DBFS`]. The first
/// three are everyday places; the rest are more unusual ones.
pub const KINDS: [&str; 12] = [
    "office",
    "home",
    "street",
    "hospital",
    "ambulance",
    "typhoon",
    "storm",
    "construction",
    "mrt",
    "nightmarket",
    "firecrackers",
    "car",
];
const BED_REF_DBFS: f32 = -30.0;
/// A pause in the AI's voice for the voice-following bed (≈ −47 dBFS).
const GATE_RMS: f32 = 150.0;
/// Voice-following bed falls over this after the voice stops.
const GATE_RELEASE_MS: f32 = 250.0;
/// The always-on bed fades in over this when it starts.
const FADE_IN_MS: f32 = 400.0;

pub fn valid_kind(kind: &str) -> bool {
    KINDS.contains(&kind)
}

/// Where the settings page's preview starts in a bed: just before its signature
/// sound (the loop drops the first 2 s).
pub fn preview_start_s(kind: &str) -> usize {
    match kind {
        "hospital" => 46,     // "ding-dong" chime at ~50 s
        "ambulance" => 14,    // the close pass peaks at ~22 s
        "firecrackers" => 59, // a string goes off at ~62 s
        "car" => 14,          // indicator from ~16 s
        _ => 20,
    }
}

/// Bed RMS for the level setting 1 low / 2 medium / 3 high (AI voice ≈ −16 dBFS).
pub fn level_dbfs(level: u32) -> f32 {
    match level {
        1 => -50.0,
        3 => -38.0,
        _ => -44.0,
    }
}

pub fn level_gain(level: u32) -> f32 {
    10f32.powf((level_dbfs(level) - BED_REF_DBFS) / 20.0)
}

/// Why a bed could not be downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmbienceDownloadError {
    /// Not one of [`KINDS`].
    UnknownKind,
    /// The release has no bed of that kind.
    NotListed,
    /// No connection, a time-out, or GitHub did not answer.
    Offline(String),
    /// The download was refused (address, size, checksum or content).
    Bad(String),
    /// The bed could not be saved on this computer.
    Io(String),
}

impl std::fmt::Display for AmbienceDownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKind => write!(f, "unknown background sound"),
            Self::NotListed => write!(f, "the background sound is not in the release"),
            Self::Offline(s) => write!(f, "could not reach the download: {s}"),
            Self::Bad(s) => write!(f, "the download was refused: {s}"),
            Self::Io(s) => write!(f, "could not save the background sound: {s}"),
        }
    }
}

impl std::error::Error for AmbienceDownloadError {}

impl From<Fail> for AmbienceDownloadError {
    fn from(f: Fail) -> Self {
        match f {
            Fail::Offline(s) => Self::Offline(s),
            Fail::Io(s) => Self::Io(s),
            other => Self::Bad(other.detail().to_string()),
        }
    }
}

/// Beds decoded in memory (16 kHz mono samples). Found on disk at start-up;
/// more arrive through [`AmbienceLibrary::ensure_bed`] when the owner asks.
pub struct AmbienceLibrary {
    beds: RwLock<HashMap<&'static str, Arc<Vec<i16>>>>,
    sample_rate: u32,
    download_dir: Option<PathBuf>,
    /// One download at a time; a second request for the same bed finds it done.
    busy: tokio::sync::Mutex<()>,
}

#[derive(Deserialize)]
struct RawIndex {
    format: u64,
    #[serde(default)]
    beds: Vec<RawBed>,
}

#[derive(Deserialize)]
struct RawBed {
    kind: String,
    file: String,
    bytes: u64,
    sha256: String,
}

fn entry_for(text: &str, kind: &str) -> Result<RawBed, AmbienceDownloadError> {
    let bad = |s: &str| AmbienceDownloadError::Bad(s.to_string());
    let ix: RawIndex = serde_json::from_str(text).map_err(|e| AmbienceDownloadError::Bad(format!("the index is not readable: {e}")))?;
    if ix.format != INDEX_FORMAT {
        return Err(AmbienceDownloadError::Bad(format!("index format {}", ix.format)));
    }
    let e = ix.beds.into_iter().find(|b| b.kind == kind).ok_or(AmbienceDownloadError::NotListed)?;
    if e.file != format!("ambience_{kind}.wav") {
        return Err(bad("the index names another file"));
    }
    if e.bytes == 0 || e.bytes > MAX_BED_BYTES {
        return Err(bad("the index gives a size out of range"));
    }
    if e.sha256.len() != 64 || !e.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(bad("the index gives a bad checksum"));
    }
    Ok(e)
}

/// A bed file as 16 kHz-style samples at `sample_rate`; RIFF/WAVE only.
fn decode_bed(path: &Path, sample_rate: u32) -> Result<Vec<i16>> {
    use std::io::Read as _;
    let mut head = [0u8; 12];
    std::fs::File::open(path)?.read_exact(&mut head)?;
    anyhow::ensure!(&head[0..4] == b"RIFF" && &head[8..12] == b"WAVE", "not a WAV file");
    let pcm = read_bed(path, sample_rate)?;
    anyhow::ensure!(pcm.len() >= sample_rate as usize * 2 * MIN_BED_SECS, "shorter than {MIN_BED_SECS} s");
    Ok(pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect())
}

impl AmbienceLibrary {
    /// Beds found in `dir` (older installs keep them in the assets folder).
    pub fn load(dir: &Path, sample_rate: u32) -> Self {
        let lib = Self {
            beds: RwLock::new(HashMap::new()),
            sample_rate,
            download_dir: None,
            busy: tokio::sync::Mutex::new(()),
        };
        lib.load_dir(dir);
        lib
    }

    /// Also use `dir` for beds downloaded later: the ones already there load
    /// now (they win over the assets folder), and leftovers of a download that
    /// was cut off are removed. Never touches the network.
    pub fn with_download_dir(mut self, dir: PathBuf) -> Self {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with(INCOMING_PREFIX) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.load_dir(&dir);
        self.download_dir = Some(dir);
        self
    }

    fn load_dir(&self, dir: &Path) {
        for kind in KINDS {
            let path = dir.join(format!("ambience_{kind}.wav"));
            if !path.is_file() {
                continue;
            }
            match decode_bed(&path, self.sample_rate) {
                Ok(samples) => {
                    info!("ambience bed loaded kind={kind} secs={}", samples.len() / self.sample_rate.max(1) as usize);
                    self.put(kind, samples);
                }
                Err(e) => warn!("ambience bed unavailable kind={kind} path={}: {e:#}", path.display()),
            }
        }
    }

    fn put(&self, kind: &'static str, samples: Vec<i16>) -> Arc<Vec<i16>> {
        let bed = Arc::new(samples);
        self.beds.write().unwrap_or_else(|e| e.into_inner()).insert(kind, bed.clone());
        bed
    }

    pub fn get(&self, kind: &str) -> Option<Arc<Vec<i16>>> {
        self.beds.read().unwrap_or_else(|e| e.into_inner()).get(kind).cloned()
    }

    pub fn loaded(&self) -> Vec<&'static str> {
        let beds = self.beds.read().unwrap_or_else(|e| e.into_inner());
        KINDS.iter().copied().filter(|k| beds.contains_key(k)).collect()
    }

    /// The bed `kind`, downloaded from the latest bageo-packs release when it
    /// is not here yet. Called only when the owner asks (a preview, picking a
    /// bed, the download button); it takes effect at once, no restart.
    pub async fn ensure_bed(&self, client: &reqwest::Client, src: &Source, kind: &str) -> Result<Arc<Vec<i16>>, AmbienceDownloadError> {
        let kind: &'static str = KINDS.iter().copied().find(|k| *k == kind).ok_or(AmbienceDownloadError::UnknownKind)?;
        if let Some(b) = self.get(kind) {
            return Ok(b);
        }
        let dir = self.download_dir.clone().ok_or_else(|| AmbienceDownloadError::Io("no folder for downloads".into()))?;
        let _one = self.busy.lock().await;
        if let Some(b) = self.get(kind) {
            return Ok(b);
        }
        let r = match tokio::time::timeout(TOTAL_TIMEOUT, self.fetch(client, src, &dir, kind)).await {
            Err(_) => Err(AmbienceDownloadError::Offline(format!("no answer within {} s", TOTAL_TIMEOUT.as_secs()))),
            Ok(r) => r,
        };
        match r {
            Ok(samples) => {
                info!("ambience bed downloaded kind={kind} secs={}", samples.len() / self.sample_rate.max(1) as usize);
                Ok(self.put(kind, samples))
            }
            Err(e) => {
                warn!("ambience bed download failed kind={kind}: {e}");
                Err(e)
            }
        }
    }

    /// [`Self::ensure_bed`] from the official release.
    pub async fn ensure_official(&self, kind: &str) -> Result<Arc<Vec<i16>>, AmbienceDownloadError> {
        let client = pack_update::client_with(TOTAL_TIMEOUT).map_err(|e| AmbienceDownloadError::Offline(e.to_string()))?;
        self.ensure_bed(&client, &Source::official(), kind).await
    }

    async fn fetch(&self, client: &reqwest::Client, src: &Source, dir: &Path, kind: &'static str) -> Result<Vec<i16>, AmbienceDownloadError> {
        let (text, hops) = pack_update::fetch_index(client, src, INDEX_NAME, MAX_INDEX_BYTES).await?;
        let entry = entry_for(&text, kind)?;
        std::fs::create_dir_all(dir).map_err(|e| AmbienceDownloadError::Io(format!("cannot make the folder: {e}")))?;
        let url = src
            .release_file_url(&hops, INDEX_NAME, &entry.file)
            .ok_or_else(|| AmbienceDownloadError::Bad("the index names a path, not a file".into()))?;
        // Removed when dropped, whatever the outcome (unless renamed away first).
        let mut staged = Staged::create(dir.join(format!("{INCOMING_PREFIX}{}-{kind}.wav", uuid::Uuid::new_v4().simple())))?;
        pack_update::download_checked(client, src, url, entry.bytes, &entry.sha256, &mut staged).await?;
        let (path, rate) = (staged.path().to_path_buf(), self.sample_rate);
        let samples = tokio::task::spawn_blocking(move || decode_bed(&path, rate))
            .await
            .map_err(|_| AmbienceDownloadError::Io("the check stopped".into()))?
            .map_err(|e| AmbienceDownloadError::Bad(format!("{e:#}")))?;
        std::fs::rename(staged.path(), dir.join(&entry.file)).map_err(|e| AmbienceDownloadError::Io(format!("cannot save: {e}")))?;
        Ok(samples)
    }
}

#[derive(Deserialize)]
struct BedRequest {
    kind: String,
}

pub(crate) fn download_error(e: &AmbienceDownloadError) -> Response {
    let (status, code) = match e {
        AmbienceDownloadError::UnknownKind => (StatusCode::BAD_REQUEST, "unknown_background"),
        AmbienceDownloadError::NotListed => (StatusCode::NOT_FOUND, "background_not_listed"),
        AmbienceDownloadError::Offline(_) => (StatusCode::BAD_GATEWAY, "background_offline"),
        AmbienceDownloadError::Bad(_) => (StatusCode::BAD_GATEWAY, "background_refused"),
        AmbienceDownloadError::Io(_) => (StatusCode::INTERNAL_SERVER_ERROR, "background_not_saved"),
    };
    api_error(status, code, e.to_string())
}

/// POST /api/ambience/download {"kind"}: download one background sound now.
async fn download(State(state): State<CallsState>, headers: HeaderMap, Query(q): Query<TokenQuery>, body: Bytes) -> Response {
    if !token_ok_parts(&state.settings, &headers, q.token.as_deref()) {
        return unauthorized();
    }
    if !pack_update::json_request(&headers) {
        return pack_update::json_required();
    }
    let Ok(req) = serde_json::from_slice::<BedRequest>(&body) else {
        return api_error(StatusCode::BAD_REQUEST, "bad_request", "expected {\"kind\": \"<background>\"}");
    };
    let lib = state.ambience.clone();
    // A task of its own: it finishes even when the console stops waiting.
    let done = tokio::spawn(async move { lib.ensure_official(&req.kind).await.map(|_| req.kind) }).await;
    match done {
        Ok(Ok(kind)) => Json(json!({"ok": true, "kind": kind, "beds": state.ambience.loaded()})).into_response(),
        Ok(Err(e)) => download_error(&e),
        Err(_) => api_error(StatusCode::INTERNAL_SERVER_ERROR, "background_not_saved", "the download stopped"),
    }
}

pub fn router() -> Router<CallsState> {
    Router::new().route("/api/ambience/download", post(download))
}

/// One bed file as s16le mono at `sample_rate`. Beds ship as IMA ADPCM WAV;
/// plain 16-bit PCM WAV still works. Anything else goes to the generic decoder.
fn read_bed(path: &Path, sample_rate: u32) -> Result<Vec<u8>> {
    if let Ok((rate, pcm)) = std::fs::read(path)
        .map_err(anyhow::Error::from)
        .and_then(|file| crate::adpcm::decode_wav(&file))
    {
        return Ok(if rate == sample_rate {
            pcm
        } else {
            crate::hold_music::resample_s16le_mono_pub(&pcm, rate, sample_rate)
        });
    }
    crate::hold_music::decode_audio_to_pcm16(path, sample_rate)
}

fn rms_s16le(pcm: &[u8]) -> f32 {
    let n = pcm.len() / 2;
    if n == 0 {
        return 0.0;
    }
    let sum: f64 = pcm
        .chunks_exact(2)
        .map(|b| {
            let s = f64::from(i16::from_le_bytes([b[0], b[1]]));
            s * s
        })
        .sum();
    (sum / n as f64).sqrt() as f32
}

/// One call's bed: loop position and envelope carry across frames, so the room
/// sounds continuous under speech, in pauses and across segments.
pub struct AmbienceMixer {
    bed: Arc<Vec<i16>>,
    pos: usize,
    gain: f32,
    env: f32,
    sample_rate: u32,
}

impl AmbienceMixer {
    pub fn new(bed: Arc<Vec<i16>>, gain: f32, start: usize, sample_rate: u32) -> Self {
        let pos = if bed.is_empty() { 0 } else { start % bed.len() };
        Self {
            bed,
            pos,
            gain,
            env: 0.0,
            sample_rate: sample_rate.max(8000),
        }
    }

    pub fn env(&self) -> f32 {
        self.env
    }

    fn frame_ms(&self, bytes: usize) -> f32 {
        (bytes / 2) as f32 * 1000.0 / self.sample_rate as f32
    }

    /// Envelope at the end of a `ms` frame heading for `target` (rise over `rise_ms`).
    fn step(&self, target: f32, ms: f32, rise_ms: f32) -> f32 {
        if target > self.env {
            (self.env + ms / rise_ms.max(1.0)).min(target)
        } else {
            (self.env - ms / GATE_RELEASE_MS).max(target)
        }
    }

    /// Add the bed into s16le `pcm`, envelope ramping from its current value to `to`.
    fn add(&mut self, pcm: &mut [u8], to: f32) {
        let n = pcm.len() / 2;
        if n == 0 || self.bed.is_empty() {
            self.env = to;
            return;
        }
        let from = self.env;
        for i in 0..n {
            let e = from + (to - from) * (i as f32 + 1.0) / n as f32;
            let b = f32::from(self.bed[self.pos]) * self.gain * e;
            self.pos += 1;
            if self.pos >= self.bed.len() {
                self.pos = 0;
            }
            let s = f32::from(i16::from_le_bytes([pcm[2 * i], pcm[2 * i + 1]])) + b;
            let v = s.round().clamp(f32::from(i16::MIN), f32::from(i16::MAX)) as i16;
            pcm[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        self.env = to;
    }

    /// Bed under one frame of the AI's voice. `continuous` (echo-free phone): the
    /// bed stays at full level; otherwise it follows the voice — up within the
    /// frame when the AI speaks, down over ~250 ms in its pauses.
    pub fn mix_under(&mut self, pcm: &mut [u8], continuous: bool) {
        let ms = self.frame_ms(pcm.len());
        let (target, rise) = if continuous {
            (1.0, FADE_IN_MS)
        } else if rms_s16le(pcm) >= GATE_RMS {
            (1.0, ms)
        } else {
            (0.0, ms)
        };
        let to = self.step(target, ms, rise);
        self.add(pcm, to);
    }

    /// Bed-only frame of `bytes` (no voice): 1.0 keeps the room going in a pause,
    /// 0.0 fades it out.
    pub fn bed_frame(&mut self, bytes: usize, target: f32) -> Vec<u8> {
        let mut pcm = vec![0u8; bytes & !1];
        let ms = self.frame_ms(pcm.len());
        let to = self.step(target, ms, FADE_IN_MS);
        self.add(&mut pcm, to);
        pcm
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack_update::tests::{tmp, Reply, Server};

    fn tone(amp: i16, samples: usize) -> Vec<u8> {
        (0..samples)
            .flat_map(|i| (if i % 2 == 0 { amp } else { -amp }).to_le_bytes())
            .collect()
    }

    fn bed(amp: i16) -> Arc<Vec<i16>> {
        Arc::new((0..16_000).map(|i| if i % 2 == 0 { amp } else { -amp }).collect())
    }

    #[test]
    fn levels_sit_well_under_the_voice() {
        assert!((level_gain(2) - 0.2).abs() < 0.01); // -44 dBFS from the -30 reference
        assert!(level_gain(1) < level_gain(2) && level_gain(2) < level_gain(3));
        assert_eq!(level_dbfs(0), level_dbfs(2));
        assert!(valid_kind("office") && !valid_kind("cafe"));
        assert!(valid_kind("hospital") && valid_kind("ambulance") && !valid_kind("../office"));
    }

    fn bed_wav(secs: usize) -> Vec<u8> {
        let pcm: Vec<u8> = noisy_bed(16_000 * secs).iter().flat_map(|s| s.to_le_bytes()).collect();
        crate::calls::wav_from_pcm(&pcm, 16_000, 1)
    }

    fn sha_hex(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        format!("{:x}", Sha256::digest(b))
    }

    fn index_with(kind: &str, file: &str, body: &[u8]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "format": 1, "version": "2026112701", "commit": "test", "built_at": "2026-11-27T00:00:00Z",
            "beds": [{"kind": kind, "file": file, "bytes": body.len(), "sha256": sha_hex(body)}],
        }))
        .unwrap()
    }

    const LATEST: &str = "/h4lo-void/bageo-packs/releases/latest/download/";

    fn release(index: Vec<u8>, file: Vec<u8>) -> Server {
        Server::start(move |p| {
            if p.ends_with(&format!("/{INDEX_NAME}")) {
                Reply::Body(index.clone())
            } else if p.starts_with(LATEST) && p.ends_with(".wav") {
                Reply::Body(file.clone())
            } else {
                Reply::NotFound
            }
        })
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn beds_load_from_the_download_folder_over_the_assets_folder() {
        let (assets, dl) = (tmp(), tmp());
        std::fs::write(assets.0.join("ambience_office.wav"), bed_wav(6)).unwrap();
        std::fs::write(assets.0.join("ambience_home.wav"), bed_wav(6)).unwrap();
        std::fs::write(dl.0.join("ambience_home.wav"), bed_wav(9)).unwrap();
        std::fs::write(dl.0.join("ambience_street.wav"), bed_wav(7)).unwrap();
        std::fs::write(dl.0.join("ambience_mrt.wav"), b"not a wav at all, just text").unwrap();
        std::fs::write(dl.0.join("ambience_car.wav"), bed_wav(2)).unwrap(); // too short
        std::fs::write(dl.0.join(".incoming-1-office.wav"), b"half").unwrap();
        let lib = AmbienceLibrary::load(&assets.0, 16_000).with_download_dir(dl.0.clone());
        assert_eq!(lib.loaded(), vec!["office", "home", "street"]);
        assert_eq!(lib.get("home").unwrap().len(), 16_000 * 9, "the downloaded bed wins");
        assert_eq!(lib.get("office").unwrap().len(), 16_000 * 6, "the assets folder is the fallback");
        assert!(lib.get("mrt").is_none() && lib.get("car").is_none());
        assert!(!dl.0.join(".incoming-1-office.wav").exists(), "a cut-off download is cleaned up");
    }

    #[tokio::test]
    async fn a_bed_is_downloaded_checked_and_usable_at_once() {
        let dl = tmp();
        let wav = bed_wav(6);
        let server = release(index_with("home", "ambience_home.wav", &wav), wav.clone());
        let lib = AmbienceLibrary::load(&dl.0.join("none"), 16_000).with_download_dir(dl.0.clone());
        assert!(lib.get("home").is_none());
        let client = pack_update::client_with(Duration::from_secs(10)).unwrap();
        let bed = lib.ensure_bed(&client, &server.source(), "home").await.unwrap();
        assert_eq!(bed.len(), 16_000 * 6);
        assert_eq!(lib.loaded(), vec!["home"]);
        assert_eq!(std::fs::read(dl.0.join("ambience_home.wav")).unwrap(), wav);
        assert_eq!(leftovers(&dl.0), vec!["ambience_home.wav".to_string()]);
        // The next ask costs nothing.
        let hits = server.hits.lock().unwrap().len();
        lib.ensure_bed(&client, &server.source(), "home").await.unwrap();
        assert_eq!(server.hits.lock().unwrap().len(), hits);
        // And a restart finds it on disk without the network.
        let again = AmbienceLibrary::load(&dl.0.join("none"), 16_000).with_download_dir(dl.0.clone());
        assert_eq!(again.loaded(), vec!["home"]);
    }

    #[tokio::test]
    async fn bad_beds_and_names_are_refused_and_leave_nothing() {
        let wav = bed_wav(6);
        let client = pack_update::client_with(Duration::from_secs(10)).unwrap();
        let try_it = |index: Vec<u8>, file: Vec<u8>, kind: &'static str| {
            let client = client.clone();
            async move {
                let dl = tmp();
                let server = release(index, file);
                let lib = AmbienceLibrary::load(&dl.0.join("none"), 16_000).with_download_dir(dl.0.clone());
                let r = lib.ensure_bed(&client, &server.source(), kind).await;
                assert!(lib.get(kind).is_none(), "{kind}");
                assert!(leftovers(&dl.0).is_empty(), "{kind}: {:?}", leftovers(&dl.0));
                let hits = server.hits.lock().unwrap().len();
                (r.err(), hits)
            }
        };
        // One changed byte.
        let mut flipped = wav.clone();
        *flipped.last_mut().unwrap() ^= 1;
        let (e, _) = try_it(index_with("home", "ambience_home.wav", &wav), flipped, "home").await;
        assert!(matches!(e, Some(AmbienceDownloadError::Bad(_))), "{e:?}");
        // A file longer than the index says.
        let mut longer = wav.clone();
        longer.extend_from_slice(&[0; 64]);
        let (e, _) = try_it(index_with("home", "ambience_home.wav", &wav), longer, "home").await;
        assert!(matches!(e, Some(AmbienceDownloadError::Bad(_))), "{e:?}");
        // An index that says more than the limit.
        let mut huge = serde_json::from_slice::<serde_json::Value>(&index_with("home", "ambience_home.wav", &wav)).unwrap();
        huge["beds"][0]["bytes"] = (MAX_BED_BYTES + 1).into();
        let (e, _) = try_it(serde_json::to_vec(&huge).unwrap(), wav.clone(), "home").await;
        assert!(matches!(e, Some(AmbienceDownloadError::Bad(_))), "{e:?}");
        // A file that is not a WAV (checksum right).
        let junk = vec![7u8; 4096];
        let (e, _) = try_it(index_with("home", "ambience_home.wav", &junk), junk.clone(), "home").await;
        assert!(matches!(e, Some(AmbienceDownloadError::Bad(_))), "{e:?}");
        // The index names a path.
        for file in ["../ambience_home.wav", "sub/ambience_home.wav", "ambience_office.wav", "ambience_home.wav?x=1"] {
            let (e, _) = try_it(index_with("home", file, &wav), wav.clone(), "home").await;
            assert!(matches!(e, Some(AmbienceDownloadError::Bad(_))), "{file}: {e:?}");
        }
        // A checksum that is not 64 lowercase hex digits.
        let mut upper = serde_json::from_slice::<serde_json::Value>(&index_with("home", "ambience_home.wav", &wav)).unwrap();
        let up = upper["beds"][0]["sha256"].as_str().unwrap().to_uppercase();
        upper["beds"][0]["sha256"] = up.into();
        let (e, _) = try_it(serde_json::to_vec(&upper).unwrap(), wav.clone(), "home").await;
        assert!(matches!(e, Some(AmbienceDownloadError::Bad(_))), "{e:?}");
        // The release does not list the kind.
        let (e, _) = try_it(index_with("office", "ambience_office.wav", &wav), wav.clone(), "home").await;
        assert_eq!(e, Some(AmbienceDownloadError::NotListed));
        // Names that are not a kind never reach the network.
        for kind in ["../home", "home/../office", "cafe", "", "HOME", "home.wav"] {
            let (e, hits) = try_it(index_with("home", "ambience_home.wav", &wav), wav.clone(), kind).await;
            assert_eq!(e, Some(AmbienceDownloadError::UnknownKind), "{kind}");
            assert_eq!(hits, 0, "{kind}");
        }
    }

    #[tokio::test]
    async fn a_download_cut_halfway_or_with_no_answer_leaves_nothing() {
        let dl = tmp();
        let wav = bed_wav(6);
        let index = index_with("home", "ambience_home.wav", &wav);
        let half = wav[..wav.len() / 2].to_vec();
        let server = Server::start(move |p| {
            if p.ends_with(INDEX_NAME) {
                Reply::Body(index.clone())
            } else {
                Reply::Cut(wav.len(), half.clone())
            }
        });
        let lib = AmbienceLibrary::load(&dl.0.join("none"), 16_000).with_download_dir(dl.0.clone());
        let client = pack_update::client_with(Duration::from_secs(10)).unwrap();
        assert!(lib.ensure_bed(&client, &server.source(), "home").await.is_err());
        assert!(lib.loaded().is_empty() && leftovers(&dl.0).is_empty());
        // Without a download folder nothing is tried at all.
        let none = AmbienceLibrary::load(&dl.0.join("none"), 16_000);
        let before = server.hits.lock().unwrap().len();
        assert!(matches!(none.ensure_bed(&client, &server.source(), "home").await, Err(AmbienceDownloadError::Io(_))));
        assert_eq!(server.hits.lock().unwrap().len(), before);
    }

    #[test]
    fn voice_following_bed_opens_with_speech_and_fades_in_pauses() {
        let mut m = AmbienceMixer::new(bed(1000), 1.0, 0, 16_000);
        let mut speech = tone(5000, 640);
        m.mix_under(&mut speech, false);
        assert_eq!(m.env(), 1.0);
        assert!(rms_s16le(&speech) > 5000.0);
        // Pause: down over ~250 ms (6–7 frames of 40 ms), not at once.
        let mut quiet = vec![0u8; 1280];
        m.mix_under(&mut quiet, false);
        assert!(m.env() > 0.8 && m.env() < 1.0);
        assert!(rms_s16le(&quiet) > 100.0);
        for _ in 0..7 {
            let mut q = vec![0u8; 1280];
            m.mix_under(&mut q, false);
        }
        assert_eq!(m.env(), 0.0);
        let mut q = vec![0u8; 1280];
        m.mix_under(&mut q, false);
        assert_eq!(rms_s16le(&q), 0.0);
    }

    #[test]
    fn continuous_bed_fades_in_and_keeps_running_in_pauses() {
        let mut m = AmbienceMixer::new(bed(1000), 0.5, 123, 16_000);
        let first = m.bed_frame(1280, 1.0);
        assert!(m.env() > 0.0 && m.env() < 0.2);
        assert!(rms_s16le(&first) < 60.0);
        for _ in 0..10 {
            m.bed_frame(1280, 1.0);
        }
        assert_eq!(m.env(), 1.0);
        let steady = m.bed_frame(1280, 1.0);
        assert!((rms_s16le(&steady) - 500.0).abs() < 5.0);
        // Under speech it stays at full level whatever the voice does.
        let mut quiet = vec![0u8; 1280];
        m.mix_under(&mut quiet, true);
        assert_eq!(m.env(), 1.0);
        // Loops past the end of the bed without a gap.
        for _ in 0..30 {
            assert!(rms_s16le(&m.bed_frame(1280, 1.0)) > 400.0);
        }
    }

    /// Room-tone-like test signal: low-passed pseudo-random noise with a slow swell.
    fn noisy_bed(samples: usize) -> Vec<i16> {
        let (mut seed, mut lp) = (12_345u32, 0f64);
        (0..samples)
            .map(|i| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let white = f64::from((seed >> 16) as i16);
                lp += 0.3 * (white - lp);
                let swell = 0.6 + 0.4 * (i as f64 / 16_000.0 * 0.7).sin();
                (lp * 0.2 * swell) as i16
            })
            .collect()
    }

    fn snr_db(src: &[i16], back: &[i16]) -> f64 {
        let (sig, err) = src.iter().zip(back).fold((0f64, 0f64), |(s, e), (a, b)| {
            (s + f64::from(*a).powi(2), e + (f64::from(*a) - f64::from(*b)).powi(2))
        });
        10.0 * (sig / err.max(1.0)).log10()
    }

    fn s16(pcm: &[u8]) -> Vec<i16> {
        pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
    }

    #[test]
    fn beds_load_from_adpcm_and_from_plain_pcm() {
        let src = noisy_bed(16_000 * 8 + 123); // not a whole number of ADPCM blocks
        let pcm: Vec<u8> = src.iter().flat_map(|s| s.to_le_bytes()).collect();
        let dir = std::env::temp_dir().join(format!("bageo-ambience-fmt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (plain, packed) = (dir.join("ambience_office.wav"), dir.join("ambience_home.wav"));
        std::fs::write(&plain, crate::calls::wav_from_pcm(&pcm, 16_000, 1)).unwrap();
        let adpcm = crate::adpcm::encode_wav(&pcm, 16_000);
        std::fs::write(&packed, &adpcm).unwrap();
        let lib = AmbienceLibrary::load(&dir, 16_000);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(lib.loaded(), vec!["office", "home"]);
        let (a, b) = (lib.get("office").unwrap(), lib.get("home").unwrap());
        assert_eq!(a.len(), src.len());
        assert_eq!(b.len(), src.len(), "same number of samples after ADPCM");
        assert_eq!(*a, src, "plain PCM is read back untouched");
        let snr = snr_db(&src, &b);
        println!("ADPCM bed: {} -> {} bytes, SNR {snr:.1} dB", pcm.len(), adpcm.len());
        assert!(snr > 20.0, "SNR {snr:.1} dB");
        assert!(adpcm.len() * 3 < pcm.len());
    }

    /// One-off converter, run by hand: turns the plain-PCM beds in `BED_SRC_DIR`
    /// into IMA ADPCM WAV files of the same names in `BED_OUT_DIR`.
    /// `cargo test -p agent convert_beds_to_adpcm -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn convert_beds_to_adpcm() {
        let src_dir = std::path::PathBuf::from(std::env::var("BED_SRC_DIR").expect("BED_SRC_DIR"));
        let out_dir = std::path::PathBuf::from(std::env::var("BED_OUT_DIR").expect("BED_OUT_DIR"));
        std::fs::create_dir_all(&out_dir).unwrap();
        let (mut before, mut after) = (0usize, 0usize);
        for kind in KINDS {
            let name = format!("ambience_{kind}.wav");
            let file = std::fs::read(src_dir.join(&name)).unwrap();
            let (rate, pcm) = crate::adpcm::decode_wav(&file).unwrap();
            assert_eq!(rate, 16_000, "{name}");
            let packed = crate::adpcm::encode_wav(&pcm, rate);
            let (rate2, back) = crate::adpcm::decode_wav(&packed).unwrap();
            assert_eq!((rate2, back.len()), (rate, pcm.len()), "{name}: same sample count");
            let (src, back) = (s16(&pcm), s16(&back));
            let rms_err = (src.iter().zip(&back).map(|(a, b)| (f64::from(*a) - f64::from(*b)).powi(2)).sum::<f64>()
                / src.len() as f64)
                .sqrt();
            println!(
                "{name}: {} -> {} bytes, {} samples, SNR {:.1} dB, RMS error {rms_err:.1}",
                file.len(),
                packed.len(),
                src.len(),
                snr_db(&src, &back)
            );
            std::fs::write(out_dir.join(&name), &packed).unwrap();
            before += file.len();
            after += packed.len();
        }
        println!("total: {before} -> {after} bytes");
    }
}

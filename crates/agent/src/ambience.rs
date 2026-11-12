//! Background sound (ambience): a looped room ambience — office, home or
//! street — under the AI's voice, so the caller hears a place, not a studio.
//!
//! Phones whose capture returns our own playback (Pixel 7/7a) only get the bed
//! while the AI talks (the uplink is masked then) and it fades out after each
//! phrase, like a phone's noise suppression; otherwise Gemini would hear the
//! bed as the caller. Echo-free phones (Pixel 9, per the call's echo check)
//! keep it running through the pauses.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tracing::{info, warn};

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

/// Beds decoded once at start-up (16 kHz mono samples).
#[derive(Default)]
pub struct AmbienceLibrary {
    beds: HashMap<&'static str, Arc<Vec<i16>>>,
}

impl AmbienceLibrary {
    pub fn load(dir: &Path, sample_rate: u32) -> Self {
        let mut beds = HashMap::new();
        for kind in KINDS {
            let path = dir.join(format!("ambience_{kind}.wav"));
            if !path.is_file() {
                continue;
            }
            match crate::hold_music::decode_audio_to_pcm16(&path, sample_rate) {
                Ok(pcm) if pcm.len() >= sample_rate as usize * 2 * 5 => {
                    let samples: Vec<i16> = pcm
                        .chunks_exact(2)
                        .map(|b| i16::from_le_bytes([b[0], b[1]]))
                        .collect();
                    info!(
                        "ambience bed loaded kind={kind} secs={}",
                        samples.len() / sample_rate.max(1) as usize
                    );
                    beds.insert(kind, Arc::new(samples));
                }
                Ok(_) => warn!("ambience bed too short kind={kind} path={}", path.display()),
                Err(e) => warn!("ambience bed unavailable kind={kind}: {e:#}"),
            }
        }
        Self { beds }
    }

    pub fn get(&self, kind: &str) -> Option<Arc<Vec<i16>>> {
        self.beds.get(kind).cloned()
    }

    pub fn loaded(&self) -> Vec<&'static str> {
        KINDS
            .iter()
            .copied()
            .filter(|k| self.beds.contains_key(k))
            .collect()
    }
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

    #[test]
    fn every_kind_has_its_bed_in_the_repo() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets");
        for kind in KINDS {
            assert!(dir.join(format!("ambience_{kind}.wav")).is_file(), "{kind}");
        }
        let lib = AmbienceLibrary::load(&dir, 16_000);
        assert_eq!(lib.loaded(), KINDS.to_vec());
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
}

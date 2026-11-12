/// Simple energy VAD for utterance segmentation (PCM s16le mono).

#[derive(Debug)]
pub struct EnergyVad {
    sample_rate: u32,
    threshold: f32,
    silence_ms: u32,
    min_utterance_ms: u32,
    max_utterance_ms: u32,
    buf: Vec<u8>,
    in_speech: bool,
    silence_samples: usize,
    speech_samples: usize,
}

impl EnergyVad {
    pub fn new(
        sample_rate: u32,
        threshold: f32,
        silence_ms: u32,
        min_utterance_ms: u32,
        max_utterance_ms: u32,
    ) -> Self {
        Self {
            sample_rate,
            threshold,
            silence_ms,
            min_utterance_ms,
            max_utterance_ms,
            buf: Vec::new(),
            in_speech: false,
            silence_samples: 0,
            speech_samples: 0,
        }
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.in_speech = false;
        self.silence_samples = 0;
        self.speech_samples = 0;
    }

    pub fn has_buffered_audio(&self) -> bool {
        !self.buf.is_empty()
    }

    pub fn flush(&mut self) -> Vec<u8> {
        let out = std::mem::take(&mut self.buf);
        self.reset();
        out
    }

    /// Feed PCM s16le mono. Returns a completed utterance or None.
    pub fn feed(&mut self, pcm_chunk: &[u8]) -> Option<Vec<u8>> {
        if pcm_chunk.is_empty() {
            return None;
        }
        let samples = samples_i16(pcm_chunk);
        let rms = rms_i16(&samples);
        let speaking = rms >= self.threshold;

        if speaking {
            if !self.in_speech {
                self.in_speech = true;
                self.speech_samples = 0;
                self.buf.clear();
            }
            self.buf.extend_from_slice(pcm_chunk);
            self.speech_samples += samples.len();
            self.silence_samples = 0;
            let max_samples =
                (self.sample_rate as u64 * self.max_utterance_ms as u64 / 1000) as usize;
            if self.speech_samples >= max_samples {
                return Some(self.flush());
            }
            return None;
        }

        if self.in_speech {
            self.buf.extend_from_slice(pcm_chunk);
            self.silence_samples += samples.len();
            let need = (self.sample_rate as u64 * self.silence_ms as u64 / 1000) as usize;
            let min_samples =
                (self.sample_rate as u64 * self.min_utterance_ms as u64 / 1000) as usize;
            if self.silence_samples >= need && self.speech_samples >= min_samples {
                return Some(self.flush());
            }
        }
        None
    }
}

fn samples_i16(pcm: &[u8]) -> Vec<i16> {
    pcm.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn rms_i16(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| {
            let f = s as f64;
            f * f
        })
        .sum();
    ((sum / samples.len() as f64).sqrt()) as f32
}

/// One-shot gate: fire once after remote (downlink) energy stays above threshold
/// for `need_ms` contiguous speech (default ~300ms). Used by transfer mode.
#[derive(Debug)]
pub struct RemoteVoiceGate {
    sample_rate: u32,
    threshold: f32,
    need_ms: u32,
    speech_samples: usize,
    fired: bool,
    bytes_received: u64,
    chunks: u64,
    max_rms: f32,
    last_rms: f32,
}

impl RemoteVoiceGate {
    pub fn new(sample_rate: u32, threshold: f32, need_ms: u32) -> Self {
        Self {
            sample_rate,
            threshold,
            need_ms: need_ms.max(50),
            speech_samples: 0,
            fired: false,
            bytes_received: 0,
            chunks: 0,
            max_rms: 0.0,
            last_rms: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.speech_samples = 0;
        self.fired = false;
        self.bytes_received = 0;
        self.chunks = 0;
        self.max_rms = 0.0;
        self.last_rms = 0.0;
    }

    pub fn has_fired(&self) -> bool {
        self.fired
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    pub fn max_rms(&self) -> f32 {
        self.max_rms
    }

    pub fn last_rms(&self) -> f32 {
        self.last_rms
    }

    pub fn chunks(&self) -> u64 {
        self.chunks
    }

    pub fn threshold(&self) -> f32 {
        self.threshold
    }

    /// Why VAD has not fired yet (for timeout / disconnect logs).
    pub fn miss_reason(&self) -> String {
        if self.fired {
            return "already_fired".into();
        }
        if self.bytes_received == 0 {
            return format!(
                "no_downlink_pcm bytes=0 chunks=0 threshold={} need_ms={}",
                self.threshold, self.need_ms
            );
        }
        format!(
            "energy_below_threshold bytes={} chunks={} max_rms={:.1} last_rms={:.1} threshold={} need_ms={} speech_samples={}",
            self.bytes_received,
            self.chunks,
            self.max_rms,
            self.last_rms,
            self.threshold,
            self.need_ms,
            self.speech_samples
        )
    }

    /// Feed downlink PCM. Returns true exactly once when remote voice is detected.
    pub fn feed(&mut self, pcm_chunk: &[u8]) -> bool {
        if self.fired || pcm_chunk.is_empty() {
            return false;
        }
        self.bytes_received += pcm_chunk.len() as u64;
        self.chunks += 1;
        let samples = samples_i16(pcm_chunk);
        let rms = rms_i16(&samples);
        self.last_rms = rms;
        if rms > self.max_rms {
            self.max_rms = rms;
        }
        if rms >= self.threshold {
            self.speech_samples += samples.len();
            let need = (self.sample_rate as u64 * self.need_ms as u64 / 1000) as usize;
            if self.speech_samples >= need {
                self.fired = true;
                return true;
            }
        } else {
            // Require contiguous energy; reset on silence gaps.
            self.speech_samples = 0;
        }
        false
    }
}

//! Goertzel DTMF decoder for 16-bit mono PCM (16 kHz uplink from remote party).
//!
//! Product: **1** = AI, **2** = voicemail. End call = hang up the phone (no 0/#).
//!
//! Approach follows classic telephony Goertzel practice used across open-source
//! detectors (same family as bert2/DtmfDetection, embedded Goertzel DTMF, etc.):
//! - 8 DTMF frequencies, dual-tone peak + second-peak margin
//! - twist limit between low/high groups
//! - short blocks (~25 ms); **two consecutive same digits** = one physical press
//!   (more reliable on VoLTE than ultra-aggressive single-block emit)
//!
//! Banks usually decode DTMF on the PBX/SBC (signaling), not in an app on PCM.
//! On cellular PCM we only have audio — this is the standard open-source path.

use std::collections::VecDeque;

const LOW: [f32; 4] = [697.0, 770.0, 852.0, 941.0];
const HIGH: [f32; 4] = [1209.0, 1336.0, 1477.0, 1633.0];
const KEYS: [[char; 4]; 4] = [
    ['1', '2', '3', 'A'],
    ['4', '5', '6', 'B'],
    ['7', '8', '9', 'C'],
    ['*', '0', '#', 'D'],
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DtmfHit {
    pub digit: char,
    pub confidence: f32,
    pub purity: f32,
    pub high_margin: f32,
}

impl DtmfHit {
    pub fn is_menu_key(&self) -> bool {
        matches!(
            self.digit.to_ascii_uppercase(),
            '0' | '1' | '2' | '3' | '4' | '5' | '6' | '7' | '8' | '9' | '*' | '#'
        )
    }

    /// Keys we act on for the product IVR.
    pub fn is_product_key(&self) -> bool {
        matches!(self.digit, '1' | '2')
    }

    /// Accept for menu / wait routing after classic detector emit.
    pub fn menu_barge_in_ok(&self) -> bool {
        if !self.is_product_key() {
            return false;
        }
        // Digit 1 must beat digit 2 on high band (1209 vs 1336).
        // Speech residual ("press 1") often lights both bands with low margin — require clear 1.
        if self.digit == '1' && self.high_margin < 1.70 {
            return false;
        }
        if self.digit == '2' && self.high_margin < 1.55 {
            return false;
        }
        self.purity >= 0.50 && self.confidence >= 0.52
    }
}

pub struct DtmfDecoder {
    sample_rate: u32,
    block_samples: usize,
    pending: Vec<i16>,
    votes_needed: u8,
    vote_digit: Option<char>,
    vote_count: u8,
    vote_conf: f32,
    vote_purity: f32,
    vote_high_margin: f32,
    miss_grace_left: u8,
    cooldown_blocks: u8,
    cooldown_left: u8,
    mag_threshold: f32,
    /// Min ratio peak/second in a band (~6 dB ≈ 2.0).
    peak_ratio: f32,
    /// Max |low-high| power ratio (twist).
    max_twist: f32,
    recent: VecDeque<char>,
}

const MIN_EMIT_CONF: f32 = 0.50;
const MISS_GRACE: u8 = 2;

impl DtmfDecoder {
    pub fn new(sample_rate: u32) -> Self {
        // ~25 ms blocks @ 16 kHz — common in open-source / embedded DTMF.
        let block_samples = ((sample_rate as usize) * 25 / 1000).max(160);
        Self {
            sample_rate,
            block_samples,
            pending: Vec::with_capacity(block_samples * 2),
            // Two same blocks ≈ 50 ms — one normal phone key press (ITU-ish).
            votes_needed: 2,
            vote_digit: None,
            vote_count: 0,
            vote_conf: 0.0,
            vote_purity: 0.0,
            vote_high_margin: 0.0,
            miss_grace_left: 0,
            cooldown_blocks: 6,
            cooldown_left: 0,
            // Raise mag floor so speech harmonics less often look like dual-tone.
            mag_threshold: 2.5e6,
            peak_ratio: 2.0,
            max_twist: 2.5,
            recent: VecDeque::with_capacity(8),
        }
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.vote_digit = None;
        self.vote_count = 0;
        self.vote_conf = 0.0;
        self.vote_purity = 0.0;
        self.vote_high_margin = 0.0;
        self.miss_grace_left = 0;
        self.cooldown_left = 0;
        self.recent.clear();
    }

    pub fn set_sample_rate(&mut self, sample_rate: u32) {
        if self.sample_rate != sample_rate {
            self.sample_rate = sample_rate;
            self.block_samples = ((sample_rate as usize) * 25 / 1000).max(160);
            self.reset();
        }
    }

    pub fn feed(&mut self, pcm: &[u8]) -> Option<DtmfHit> {
        let mut out = None;
        let mut i = 0;
        while i + 1 < pcm.len() {
            let s = i16::from_le_bytes([pcm[i], pcm[i + 1]]);
            self.pending.push(s);
            i += 2;
            while self.pending.len() >= self.block_samples {
                let block: Vec<i16> = self.pending.drain(..self.block_samples).collect();
                if let Some(hit) = self.process_block(&block) {
                    out = Some(hit);
                }
            }
        }
        out
    }

    fn process_block(&mut self, samples: &[i16]) -> Option<DtmfHit> {
        if self.cooldown_left > 0 {
            self.cooldown_left -= 1;
            self.vote_digit = None;
            self.vote_count = 0;
            self.miss_grace_left = 0;
            return None;
        }

        let Some((digit, conf, purity, high_margin)) = detect_block(
            samples,
            self.sample_rate,
            self.mag_threshold,
            self.peak_ratio,
            self.max_twist,
        ) else {
            if self.vote_digit.is_some() && self.miss_grace_left > 0 {
                self.miss_grace_left -= 1;
                return None;
            }
            self.vote_digit = None;
            self.vote_count = 0;
            self.vote_conf = 0.0;
            self.vote_purity = 0.0;
            self.vote_high_margin = 0.0;
            self.miss_grace_left = 0;
            return None;
        };

        match self.vote_digit {
            Some(d) if d == digit => {
                self.vote_count = self.vote_count.saturating_add(1);
                self.vote_conf = self.vote_conf.max(conf);
                self.vote_purity = self.vote_purity.max(purity);
                self.vote_high_margin = self.vote_high_margin.max(high_margin);
                self.miss_grace_left = MISS_GRACE;
            }
            Some(_) => {
                // Mid-press flip: keep stronger candidate.
                if conf + 0.08 < self.vote_conf {
                    self.miss_grace_left = MISS_GRACE;
                    return None;
                }
                self.vote_digit = Some(digit);
                self.vote_count = 1;
                self.vote_conf = conf;
                self.vote_purity = purity;
                self.vote_high_margin = high_margin;
                self.miss_grace_left = MISS_GRACE;
            }
            None => {
                self.vote_digit = Some(digit);
                self.vote_count = 1;
                self.vote_conf = conf;
                self.vote_purity = purity;
                self.vote_high_margin = high_margin;
                self.miss_grace_left = MISS_GRACE;
            }
        }

        if self.vote_conf < MIN_EMIT_CONF {
            return None;
        }

        // Very clean dual-tone → one block is enough (short key press).
        // Keep this stricter than menu_barge_in so speech rarely single-vote emits "1".
        let solid = matches!(digit, '1' | '2')
            && self.vote_purity >= 0.62
            && self.vote_conf >= 0.62
            && (digit != '1' || self.vote_high_margin >= 1.80);
        let need = if solid { 1 } else { self.votes_needed };

        if self.vote_count >= need {
            let hit = DtmfHit {
                digit,
                confidence: self.vote_conf,
                purity: self.vote_purity,
                high_margin: self.vote_high_margin,
            };
            self.vote_digit = None;
            self.vote_count = 0;
            self.vote_conf = 0.0;
            self.vote_purity = 0.0;
            self.vote_high_margin = 0.0;
            self.miss_grace_left = 0;
            self.cooldown_left = self.cooldown_blocks;
            self.recent.push_back(digit);
            while self.recent.len() > 8 {
                self.recent.pop_front();
            }
            return Some(hit);
        }
        None
    }
}

fn goertzel_power(samples: &[i16], sample_rate: u32, freq: f32) -> f32 {
    let n = samples.len() as f32;
    if n < 2.0 {
        return 0.0;
    }
    let k = (0.5 + n * freq / sample_rate as f32).floor();
    let omega = std::f32::consts::TAU * k / n;
    let coeff = 2.0 * omega.cos();
    let mut s0 = 0.0f32;
    let mut s1 = 0.0f32;
    let mut s2 = 0.0f32;
    for &x in samples {
        s0 = x as f32 + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - coeff * s1 * s2
}

/// Classic dual-tone peak detector (open-source Goertzel DTMF style).
fn detect_block(
    samples: &[i16],
    sample_rate: u32,
    mag_threshold: f32,
    peak_ratio: f32,
    max_twist: f32,
) -> Option<(char, f32, f32, f32)> {
    let mut low_p = [0.0f32; 4];
    let mut high_p = [0.0f32; 4];
    for (i, f) in LOW.iter().enumerate() {
        low_p[i] = goertzel_power(samples, sample_rate, *f);
    }
    for (i, f) in HIGH.iter().enumerate() {
        high_p[i] = goertzel_power(samples, sample_rate, *f);
    }

    let (li, lmax) = argmax4(&low_p);
    let (hi, hmax) = argmax4(&high_p);

    let mut e = 0.0f32;
    for &s in samples {
        let x = s as f32;
        e += x * x;
    }
    // Adaptive floor so quiet cellular DTMF still passes.
    let floor = (e * 6.0).max(mag_threshold * 0.12).min(mag_threshold);
    if lmax < floor || hmax < floor {
        return None;
    }

    let l2 = second_max(&low_p, li);
    let h2 = second_max(&high_p, hi);
    // Peak vs second in row/col (~6 dB).
    if l2 > 1.0 && lmax < l2 * peak_ratio {
        return None;
    }
    if h2 > 1.0 && hmax < h2 * peak_ratio {
        return None;
    }

    // Twist: both tones present with similar energy.
    let lo = lmax.min(hmax);
    let hi_e = lmax.max(hmax);
    if lo < 1.0 || hi_e / lo > max_twist {
        return None;
    }

    let high_margin = if h2 > 1.0 { hmax / h2 } else { 10.0 };

    let dual = lmax + hmax;
    let all: f32 = low_p.iter().sum::<f32>() + high_p.iter().sum::<f32>();
    let purity = if all > 1.0 {
        (dual / all).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // Classic detectors reject speech with too much energy outside the two peaks.
    if purity < 0.40 {
        return None;
    }

    let digit = KEYS[li][hi];
    let conf_l = if l2 > 1.0 { lmax / l2 } else { 10.0 };
    let conf_h = if h2 > 1.0 { hmax / h2 } else { 10.0 };
    let conf = ((conf_l.min(conf_h) / peak_ratio) * purity).clamp(0.0, 1.0);

    Some((digit, conf, purity, high_margin))
}

fn argmax4(v: &[f32; 4]) -> (usize, f32) {
    let mut best_i = 0;
    let mut best = v[0];
    for (i, &x) in v.iter().enumerate().skip(1) {
        if x > best {
            best = x;
            best_i = i;
        }
    }
    (best_i, best)
}

fn second_max(v: &[f32; 4], skip: usize) -> f32 {
    let mut best = 0.0f32;
    for (i, &x) in v.iter().enumerate() {
        if i != skip && x > best {
            best = x;
        }
    }
    best
}

pub fn synthesize_digit(
    digit: char,
    sample_rate: u32,
    duration_ms: u32,
    amplitude: i16,
) -> Vec<u8> {
    let (lf, hf) = match digit {
        '1' => (697.0, 1209.0),
        '2' => (697.0, 1336.0),
        '3' => (697.0, 1477.0),
        '4' => (770.0, 1209.0),
        '5' => (770.0, 1336.0),
        '6' => (770.0, 1477.0),
        '7' => (852.0, 1209.0),
        '8' => (852.0, 1336.0),
        '9' => (852.0, 1477.0),
        '*' => (941.0, 1209.0),
        '0' => (941.0, 1336.0),
        '#' => (941.0, 1477.0),
        'A' => (697.0, 1633.0),
        'B' => (770.0, 1633.0),
        'C' => (852.0, 1633.0),
        'D' => (941.0, 1633.0),
        _ => return Vec::new(),
    };
    let n = (sample_rate as u64 * u64::from(duration_ms) / 1000) as usize;
    let amp = f32::from(amplitude);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / sample_rate as f32;
        let s = (amp
            * 0.5
            * ((std::f32::consts::TAU * lf * t).sin() + (std::f32::consts::TAU * hf * t).sin()))
        .round()
        .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(d: &mut DtmfDecoder, pcm: &[u8]) -> Option<DtmfHit> {
        let mut hit = None;
        for chunk in pcm.chunks(320) {
            if let Some(h) = d.feed(chunk) {
                hit = Some(h);
            }
        }
        hit
    }

    #[test]
    fn single_press_digit_1() {
        let pcm = synthesize_digit('1', 16000, 50, 8000);
        let mut d = DtmfDecoder::new(16000);
        let h = feed_all(&mut d, &pcm).expect("digit 1");
        assert_eq!(h.digit, '1');
        assert!(h.menu_barge_in_ok());
    }

    #[test]
    fn single_press_digit_2() {
        let pcm = synthesize_digit('2', 16000, 50, 8000);
        let mut d = DtmfDecoder::new(16000);
        let h = feed_all(&mut d, &pcm).expect("digit 2");
        assert_eq!(h.digit, '2');
        assert!(h.menu_barge_in_ok());
    }

    #[test]
    fn digit_1_not_confused_as_2_clean() {
        let pcm = synthesize_digit('1', 16000, 80, 10000);
        let mut d = DtmfDecoder::new(16000);
        let h = feed_all(&mut d, &pcm).expect("digit 1");
        assert_eq!(h.digit, '1');
        assert!(h.high_margin >= 1.5);
    }
}

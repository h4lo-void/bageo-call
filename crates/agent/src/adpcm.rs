//! IMA ADPCM WAV (format 0x11, mono): the bundled voice samples are kept in it,
//! a quarter of the size of plain 16-bit WAV and still playable in any player.
//! Also reads plain 16-bit PCM WAV.

use anyhow::{bail, ensure, Result};

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const INDEX_STEP: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];
/// Bytes per block: a 4-byte header (first sample + step index), then 2 samples a byte.
const BLOCK_ALIGN: usize = 512;
const SAMPLES_PER_BLOCK: usize = (BLOCK_ALIGN - 4) * 2 + 1;

/// One decoder step: the next sample from `nibble`. The encoder runs the same
/// step, so both sides stay in lock-step.
fn step(pred: &mut i32, index: &mut i32, nibble: u8) -> i16 {
    let s = STEPS[*index as usize];
    let mut diff = s >> 3;
    if nibble & 4 != 0 {
        diff += s;
    }
    if nibble & 2 != 0 {
        diff += s >> 1;
    }
    if nibble & 1 != 0 {
        diff += s >> 2;
    }
    *pred = if nibble & 8 != 0 { *pred - diff } else { *pred + diff }.clamp(-32768, 32767);
    *index = (*index + INDEX_STEP[(nibble & 7) as usize]).clamp(0, 88);
    *pred as i16
}

fn encode_one(pred: &mut i32, index: &mut i32, sample: i16) -> u8 {
    let s = STEPS[*index as usize];
    let mut diff = i32::from(sample) - *pred;
    let mut nibble = 0u8;
    if diff < 0 {
        nibble = 8;
        diff = -diff;
    }
    if diff >= s {
        nibble |= 4;
        diff -= s;
    }
    if diff >= s >> 1 {
        nibble |= 2;
        diff -= s >> 1;
    }
    if diff >= s >> 2 {
        nibble |= 1;
    }
    step(pred, index, nibble);
    nibble
}

/// s16le mono PCM → IMA ADPCM WAV file bytes.
pub fn encode_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let samples: Vec<i16> = pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    let mut data = Vec::with_capacity(samples.len() / 2 + BLOCK_ALIGN);
    let mut index = 0i32;
    for block in samples.chunks(SAMPLES_PER_BLOCK) {
        let first = block[0];
        let mut pred = i32::from(first);
        data.extend_from_slice(&first.to_le_bytes());
        data.push(index as u8);
        data.push(0);
        // The rest of the block, padded with the last sample so every block is whole.
        let last = *block.last().unwrap_or(&first);
        let mut rest = block[1..].iter().copied().chain(std::iter::repeat(last)).take(SAMPLES_PER_BLOCK - 1);
        while let Some(a) = rest.next() {
            let lo = encode_one(&mut pred, &mut index, a);
            let hi = rest.next().map(|b| encode_one(&mut pred, &mut index, b)).unwrap_or(0);
            data.push(lo | (hi << 4));
        }
    }
    let byte_rate = (u64::from(sample_rate) * BLOCK_ALIGN as u64 / SAMPLES_PER_BLOCK as u64) as u32;
    let mut out = Vec::with_capacity(data.len() + 60);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((4 + 8 + 20 + 8 + 4 + 8 + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&20u32.to_le_bytes());
    out.extend_from_slice(&0x11u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&(BLOCK_ALIGN as u16).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&(SAMPLES_PER_BLOCK as u16).to_le_bytes());
    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

/// A mono WAV (IMA ADPCM or 16-bit PCM) → (sample rate, s16le PCM).
pub fn decode_wav(file: &[u8]) -> Result<(u32, Vec<u8>)> {
    ensure!(file.len() >= 12 && &file[..4] == b"RIFF" && &file[8..12] == b"WAVE", "not a WAV file");
    let u16_at = |b: &[u8], i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |b: &[u8], i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (mut fmt, mut data, mut total) = (None, None, None);
    let mut at = 12;
    while at + 8 <= file.len() {
        let len = u32_at(file, at + 4) as usize;
        let body = &file[at + 8..(at + 8).saturating_add(len).min(file.len())];
        match &file[at..at + 4] {
            b"fmt " => fmt = Some(body),
            b"data" => data = Some(body),
            b"fact" if body.len() >= 4 => total = Some(u32_at(body, 0) as usize),
            _ => {}
        }
        at = at.saturating_add(8 + len + (len & 1));
    }
    let (Some(fmt), Some(data)) = (fmt, data) else { bail!("WAV without fmt or data") };
    ensure!(fmt.len() >= 16, "short fmt chunk");
    let (format, channels, rate) = (u16_at(fmt, 0), u16_at(fmt, 2), u32_at(fmt, 4));
    ensure!(channels == 1, "only mono WAV");
    match format {
        1 => {
            ensure!(u16_at(fmt, 14) == 16, "only 16-bit PCM WAV");
            Ok((rate, data[..data.len() & !1].to_vec()))
        }
        0x11 => {
            let align = usize::from(u16_at(fmt, 12));
            ensure!(align > 4, "bad ADPCM block size");
            let mut pcm = Vec::with_capacity(data.len() * 4);
            for block in data.chunks(align).filter(|b| b.len() > 4) {
                let mut pred = i32::from(i16::from_le_bytes([block[0], block[1]]));
                let mut index = i32::from(block[2]).clamp(0, 88);
                pcm.extend_from_slice(&(pred as i16).to_le_bytes());
                for &byte in &block[4..] {
                    for nibble in [byte & 0x0f, byte >> 4] {
                        pcm.extend_from_slice(&step(&mut pred, &mut index, nibble).to_le_bytes());
                    }
                }
            }
            if let Some(n) = total {
                pcm.truncate(n * 2);
            }
            Ok((rate, pcm))
        }
        f => bail!("WAV format {f} is not supported"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_the_sound() {
        let sr = 16_000u32;
        // A falling tone with a quiet stretch, 1.3 s: more than one block, a partial last one.
        let n = (sr as usize * 13) / 10;
        let src: Vec<i16> = (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(sr);
                let amp = if (0.5..0.7).contains(&t) { 300.0 } else { 9_000.0 };
                (amp * (2.0 * std::f64::consts::PI * (600.0 - 200.0 * t) * t).sin()) as i16
            })
            .collect();
        let pcm: Vec<u8> = src.iter().flat_map(|s| s.to_le_bytes()).collect();
        let wav = encode_wav(&pcm, sr);
        assert!(wav.len() < pcm.len() / 3, "a quarter of the size: {} vs {}", wav.len(), pcm.len());
        let (rate, back) = decode_wav(&wav).unwrap();
        assert_eq!((rate, back.len()), (sr, pcm.len()));
        let back: Vec<i16> = back.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        let (sig, err) = src.iter().zip(&back).fold((0f64, 0f64), |(s, e), (a, b)| {
            (s + f64::from(*a).powi(2), e + (f64::from(*a) - f64::from(*b)).powi(2))
        });
        let snr_db = 10.0 * (sig / err.max(1.0)).log10();
        assert!(snr_db > 20.0, "SNR {snr_db:.1} dB");
    }

    #[test]
    fn reads_plain_pcm_and_refuses_junk() {
        let pcm: Vec<u8> = (0..800i16).flat_map(|s| (s * 10).to_le_bytes()).collect();
        let wav = crate::calls::wav_from_pcm(&pcm, 8_000, 1);
        assert_eq!(decode_wav(&wav).unwrap(), (8_000, pcm));
        assert!(decode_wav(b"RIFF\0\0\0\0WAVE").is_err());
        assert!(decode_wav(b"not audio at all").is_err());
    }
}

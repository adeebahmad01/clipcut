//! Sample-level helpers: energy envelope, silence trimming, loudness,
//! resampling and WAV I/O.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Envelope hop (and resolution of every timing derived from it).
pub const HOP_S: f32 = 0.010;
const WIN_S: f32 = 0.020;

/// RMS energy every [`HOP_S`] seconds over a [`WIN_S`] window, centred on the hop.
pub fn rms_envelope(samples: &[f32], rate: u32) -> Vec<f32> {
    let hop = ((rate as f32 * HOP_S) as usize).max(1);
    let half = ((rate as f32 * WIN_S) as usize / 2).max(1);
    let frames = samples.len().div_ceil(hop);
    // Prefix sums of squares for O(1) windows.
    let mut prefix = Vec::with_capacity(samples.len() + 1);
    prefix.push(0f64);
    let mut acc = 0f64;
    for s in samples {
        acc += (*s as f64) * (*s as f64);
        prefix.push(acc);
    }
    (0..frames)
        .map(|f| {
            let centre = f * hop + hop / 2;
            let a = centre.saturating_sub(half);
            let b = (centre + half).min(samples.len());
            if b <= a {
                return 0.0;
            }
            ((prefix[b] - prefix[a]) / (b - a) as f64).sqrt() as f32
        })
        .collect()
}

/// Silence threshold for an envelope: 40 dB under its loud level, with a floor.
pub fn silence_threshold(env: &[f32]) -> f32 {
    let mut sorted: Vec<f32> = env
        .iter()
        .copied()
        .filter(|v| v.is_finite() && *v > 0.0)
        .collect();
    if sorted.is_empty() {
        return 1e-4;
    }
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let loud = sorted[((sorted.len() as f32 * 0.95) as usize).min(sorted.len() - 1)];
    (loud * 0.01).max(2e-4)
}

/// Reject empty, non-finite, silent, DC-only, or click-only engine output.
///
/// This is an acoustic sanity check, not speech recognition: it cannot prove
/// that a user command said the requested words (or distinguish music from
/// speech). Real speech engines must still be checked with a listening/ASR
/// smoke test when their models or phonemizer change.
pub fn validate_speech(samples: &[f32], rate: u32) -> anyhow::Result<()> {
    anyhow::ensure!(rate > 0, "TTS audio has a zero sample rate");
    anyhow::ensure!(!samples.is_empty(), "TTS engine returned empty audio");
    anyhow::ensure!(
        samples.iter().all(|s| s.is_finite()),
        "TTS engine returned non-finite audio"
    );
    let duration = samples.len() as f32 / rate as f32;
    anyhow::ensure!(
        duration >= 0.05,
        "TTS engine returned only {duration:.3}s of audio"
    );
    let mean = samples.iter().map(|s| *s as f64).sum::<f64>() / samples.len() as f64;
    let variance = samples
        .iter()
        .map(|s| (*s as f64 - mean).powi(2))
        .sum::<f64>()
        / samples.len() as f64;
    anyhow::ensure!(
        variance.sqrt() >= 2e-4,
        "TTS engine returned silent or DC-only audio"
    );
    let env = rms_envelope(samples, rate);
    let th = silence_threshold(&env);
    let active_s = env.iter().filter(|v| **v > th).count() as f32 * HOP_S;
    anyhow::ensure!(
        active_s >= 0.04,
        "TTS engine returned no sustained speech activity ({active_s:.3}s)"
    );
    Ok(())
}

/// Cut leading/trailing silence, keeping a little padding so onsets and
/// releases are not clipped.
pub fn trim_silence(samples: &[f32], rate: u32) -> Vec<f32> {
    let env = rms_envelope(samples, rate);
    let th = silence_threshold(&env);
    let Some(first) = env.iter().position(|v| *v > th) else {
        return Vec::new();
    };
    let last = env.iter().rposition(|v| *v > th).unwrap_or(first);
    let hop = (rate as f32 * HOP_S) as usize;
    let start = (first * hop).saturating_sub((rate as f32 * 0.015) as usize);
    let end = ((last + 1) * hop + (rate as f32 * 0.030) as usize).min(samples.len());
    let mut out = samples[start..end].to_vec();
    fade(&mut out, rate, 0.004);
    out
}

/// Short linear fade in/out to avoid clicks at cut points.
pub fn fade(samples: &mut [f32], rate: u32, secs: f32) {
    let n = ((rate as f32 * secs) as usize).min(samples.len() / 2);
    let len = samples.len();
    for i in 0..n {
        let g = i as f32 / n as f32;
        samples[i] *= g;
        samples[len - 1 - i] *= g;
    }
}

/// Bring active speech to roughly -16 LUFS (RMS of voiced frames about
/// -18 dBFS) while keeping peaks at or below -1 dBFS.
pub fn normalize_loudness(samples: &mut [f32], rate: u32) {
    let env = rms_envelope(samples, rate);
    let th = silence_threshold(&env);
    let active: Vec<f32> = env.iter().copied().filter(|v| *v > th * 3.0).collect();
    if active.is_empty() {
        return;
    }
    let mean_sq = active.iter().map(|v| v * v).sum::<f32>() / active.len() as f32;
    let rms = mean_sq.sqrt().max(1e-6);
    let target_rms = 10f32.powf(-18.0 / 20.0);
    let peak_limit = 10f32.powf(-1.0 / 20.0);
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs())).max(1e-6);
    let gain = (target_rms / rms).min(peak_limit / peak).min(30.0);
    for s in samples.iter_mut() {
        *s *= gain;
    }
}

/// Windowed-sinc resampler (Hann, 16 zero crossings) — enough for speech.
pub fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == 0 || to == 0 {
        return Vec::new();
    }
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = ((samples.len() as f64) * ratio).round() as usize;
    let cutoff = ratio.min(1.0) * 0.95;
    let half_taps = 16.0 / cutoff;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let t = i as f64 / ratio;
        let lo = (t - half_taps).floor().max(0.0) as usize;
        let hi = ((t + half_taps).ceil() as usize).min(samples.len() - 1);
        let mut acc = 0.0;
        let mut norm = 0.0;
        for (j, s) in samples.iter().enumerate().take(hi + 1).skip(lo) {
            let x = j as f64 - t;
            let w = 0.5 + 0.5 * (std::f64::consts::PI * x / half_taps).cos();
            if x.abs() > half_taps {
                continue;
            }
            let arg = std::f64::consts::PI * x * cutoff;
            let sinc = if arg.abs() < 1e-9 {
                1.0
            } else {
                arg.sin() / arg
            };
            let k = sinc * w * cutoff;
            acc += *s as f64 * k;
            norm += k;
        }
        out.push(if norm.abs() > 1e-9 {
            (acc / norm) as f32
        } else {
            0.0
        });
    }
    out
}

/// Read any PCM/float WAV as mono f32.
pub fn read_wav(path: &Path) -> anyhow::Result<(Vec<f32>, u32)> {
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let spec = reader.spec();
    anyhow::ensure!(
        spec.sample_rate > 0 && spec.channels > 0,
        "invalid WAV sample rate/channels in {}",
        path.display()
    );
    anyhow::ensure!(
        (1..=32).contains(&spec.bits_per_sample),
        "unsupported WAV bit depth {}",
        spec.bits_per_sample
    );
    let channels = spec.channels as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    anyhow::ensure!(
        interleaved.len() % channels == 0,
        "incomplete WAV frame in {}",
        path.display()
    );
    anyhow::ensure!(
        interleaved.iter().all(|s| s.is_finite()),
        "non-finite WAV samples in {}",
        path.display()
    );
    let mono = interleaved
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

/// Write mono 16-bit PCM.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32) -> anyhow::Result<()> {
    anyhow::ensure!(rate > 0, "cannot write WAV with a zero sample rate");
    anyhow::ensure!(
        samples.iter().all(|s| s.is_finite()),
        "cannot write non-finite WAV samples"
    );
    if let Some(dir) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "wav.{}.{}.tmp",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> anyhow::Result<()> {
        let mut w = hound::WavWriter::create(&tmp, spec)?;
        for s in samples {
            w.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
        }
        w.finalize()?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, rate: u32, amp: f32) -> Vec<f32> {
        (0..(secs * rate as f32) as usize)
            .map(|i| amp * (i as f32 * 0.07).sin())
            .collect()
    }

    #[test]
    fn trims_and_normalizes() {
        let rate = 24_000;
        let mut s = vec![0.0; 12_000];
        s.extend(tone(1.0, rate, 0.05));
        s.extend(vec![0.0; 24_000]);
        let t = trim_silence(&s, rate);
        let secs = t.len() as f32 / rate as f32;
        assert!((1.0..1.1).contains(&secs), "{secs}");
        let mut t = t;
        normalize_loudness(&mut t, rate);
        let peak = t.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak <= 0.892 && peak > 0.1, "{peak}");
        assert!(trim_silence(&vec![0.0; 1000], rate).is_empty());
    }

    #[test]
    fn resamples() {
        let s = tone(0.5, 22_050, 0.5);
        let r = resample(&s, 22_050, 24_000);
        assert_eq!(r.len(), 12_000);
        let rms_in = (s.iter().map(|v| v * v).sum::<f32>() / s.len() as f32).sqrt();
        let rms_out = (r.iter().map(|v| v * v).sum::<f32>() / r.len() as f32).sqrt();
        assert!((rms_in - rms_out).abs() < 0.02, "{rms_in} {rms_out}");
        assert!(resample(&s, 0, 24_000).is_empty());
    }

    #[test]
    fn rejects_speechless_and_invalid_audio() {
        assert!(validate_speech(&[], 24_000).is_err());
        assert!(validate_speech(&vec![0.0; 24_000], 24_000).is_err());
        assert!(validate_speech(&vec![0.5; 24_000], 24_000).is_err());
        assert!(validate_speech(&vec![f32::NAN; 24_000], 24_000).is_err());
        assert!(validate_speech(&tone(0.3, 24_000, 0.1), 0).is_err());
        let mut click = vec![0.0; 24_000];
        click[12_000] = 0.5;
        assert!(validate_speech(&click, 24_000).is_err());
        assert!(validate_speech(&tone(0.3, 24_000, 0.1), 24_000).is_ok());
    }

    #[test]
    fn wav_roundtrip() {
        let dir = std::env::temp_dir().join(format!("clipcut-tts-wav-{}", std::process::id()));
        let p = dir.join("a.wav");
        let s = tone(0.1, 24_000, 0.5);
        write_wav(&p, &s, 24_000).unwrap();
        let (r, rate) = read_wav(&p).unwrap();
        assert_eq!(rate, 24_000);
        assert_eq!(r.len(), s.len());
        assert!((r[100] - s[100]).abs() < 1e-3);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

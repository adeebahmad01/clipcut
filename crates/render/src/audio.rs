//! Audio: procedural SFX synthesis, user SFX overrides, and the final mix
//! (voice + SFX + ducked music + clip audio) with a limiter, written as WAV.

use std::collections::HashMap;
use std::f32::consts::{PI, TAU};
use std::path::{Path, PathBuf};

use anyhow::Context;

use clipcut_core::timeline::{SfxKind, Timeline};

use crate::control::Control;
use crate::ffmpeg::decode_audio;
use crate::util::{Rng, fnv1a};

pub const SR: u32 = 48_000;
const SRF: f32 = SR as f32;

/// Stereo interleaved buffer.
pub type Stereo = Vec<f32>;

// ---------------------------------------------------------------------------
// DSP building blocks

/// RBJ biquad, direct form I.
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

#[derive(Clone, Copy)]
enum Band {
    Low,
    High,
    Pass,
}

impl Biquad {
    fn new(kind: Band, freq: f32, q: f32) -> Self {
        let mut b = Biquad {
            b0: 0.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        };
        b.set(kind, freq, q);
        b
    }
    fn set(&mut self, kind: Band, freq: f32, q: f32) {
        let w0 = TAU * freq.clamp(10.0, SRF * 0.45) / SRF;
        let (s, c) = w0.sin_cos();
        let alpha = s / (2.0 * q);
        let (b0, b1, b2) = match kind {
            Band::Low => ((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0),
            Band::High => ((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0),
            Band::Pass => (alpha, 0.0, -alpha),
        };
        let a0 = 1.0 + alpha;
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = -2.0 * c / a0;
        self.a2 = (1.0 - alpha) / a0;
    }
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

fn n_samples(sec: f32) -> usize {
    (sec * SRF) as usize
}

fn noise(rng: &mut Rng) -> f32 {
    rng.f32() * 2.0 - 1.0
}

/// Mono -> stereo with a pan curve (-1 left .. 1 right) per sample position.
fn to_stereo(mono: &[f32], pan: impl Fn(f32) -> f32) -> Stereo {
    let n = mono.len().max(1) as f32;
    let mut out = Vec::with_capacity(mono.len() * 2);
    for (i, &s) in mono.iter().enumerate() {
        let p = pan(i as f32 / n).clamp(-1.0, 1.0);
        let a = (p + 1.0) * PI / 4.0;
        out.push(s * a.cos() * std::f32::consts::SQRT_2);
        out.push(s * a.sin() * std::f32::consts::SQRT_2);
    }
    out
}

fn normalize(buf: &mut [f32], peak: f32) {
    let m = buf.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if m > 1e-6 {
        let g = peak / m;
        buf.iter_mut().for_each(|s| *s *= g);
    }
}

/// Short fades at both ends to avoid clicks.
fn edge_fade(buf: &mut [f32], fade_in: usize, fade_out: usize) {
    let n = buf.len();
    for i in 0..fade_in.min(n) {
        buf[i] *= i as f32 / fade_in as f32;
    }
    for i in 0..fade_out.min(n) {
        buf[n - 1 - i] *= i as f32 / fade_out as f32;
    }
}

// ---------------------------------------------------------------------------
// Synthesized SFX (all stereo, peak-normalized to ~0.9)

fn whoosh(seed: u64, dur: f32, f_lo: f32, f_hi: f32, peak_at: f32) -> Stereo {
    let mut rng = Rng::new(seed);
    let n = n_samples(dur);
    let mut bp = Biquad::new(Band::Pass, f_lo, 1.1);
    let mut bp2 = Biquad::new(Band::Pass, f_lo, 1.1);
    let mut lp = Biquad::new(Band::Low, 6000.0, 0.7);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / n as f32;
        // Frequency sweeps up to the peak, then settles.
        let f = if t < peak_at {
            f_lo * (f_hi / f_lo).powf((t / peak_at).powf(1.3))
        } else {
            f_hi * ((f_lo * 2.2) / f_hi).powf((t - peak_at) / (1.0 - peak_at))
        };
        if i % 32 == 0 {
            bp.set(Band::Pass, f, 1.1);
            bp2.set(Band::Pass, f * 1.5, 1.6);
        }
        let env = if t < peak_at {
            (t / peak_at).powf(2.2)
        } else {
            (1.0 - (t - peak_at) / (1.0 - peak_at)).powf(2.6)
        };
        let x = noise(&mut rng);
        out.push(lp.run(bp.run(x) * 0.8 + bp2.run(x) * 0.45) * env);
    }
    normalize(&mut out, 0.9);
    edge_fade(&mut out, 64, 256);
    to_stereo(&out, |p| -0.6 + 1.2 * p)
}

fn pop(seed: u64) -> Stereo {
    let mut rng = Rng::new(seed);
    let n = n_samples(0.13);
    let mut out = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    let mut hp = Biquad::new(Band::High, 1500.0, 0.7);
    for i in 0..n {
        let t = i as f32 / SRF;
        let f = 180.0 + 900.0 * (-t * 45.0).exp();
        phase += TAU * f / SRF;
        let env = (t / 0.003).min(1.0) * (-t * 38.0).exp();
        let click = hp.run(noise(&mut rng)) * (-t * 400.0).exp() * 0.35;
        out.push(phase.sin() * env + click);
    }
    normalize(&mut out, 0.9);
    edge_fade(&mut out, 16, 128);
    to_stereo(&out, |_| 0.0)
}

fn click(seed: u64, level: f32, len: f32) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let n = n_samples(len);
    let mut hp = Biquad::new(Band::High, 2200.0, 0.8);
    let mut bp = Biquad::new(Band::Pass, 3200.0 + rng.range(-400.0, 400.0), 4.0);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / SRF;
        let x = noise(&mut rng);
        let env = (-t * 260.0).exp();
        let body = (TAU * 1700.0 * t).sin() * (-t * 120.0).exp() * 0.3;
        out.push((hp.run(x) * 0.7 + bp.run(x) * 2.0) * env * level + body * level);
    }
    out
}

fn click_sfx(seed: u64) -> Stereo {
    let mut out = click(seed, 1.0, 0.05);
    normalize(&mut out, 0.85);
    edge_fade(&mut out, 4, 64);
    to_stereo(&out, |_| 0.0)
}

fn ding() -> Stereo {
    let n = n_samples(1.4);
    let base = 1318.5;
    let partials = [
        (1.0, 1.0, 3.2),
        (2.0, 0.45, 5.0),
        (2.76, 0.3, 7.0),
        (5.4, 0.12, 11.0),
        (0.5, 0.15, 4.0),
    ];
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / SRF;
        let attack = (t / 0.002).min(1.0);
        let s: f32 = partials
            .iter()
            .map(|&(m, a, d)| (TAU * base * m * t).sin() * a * (-t * d).exp())
            .sum();
        out.push(s * attack);
    }
    normalize(&mut out, 0.85);
    edge_fade(&mut out, 8, 2000);
    to_stereo(&out, |_| 0.0)
}

fn riser(seed: u64) -> Stereo {
    let mut rng = Rng::new(seed);
    let dur = 1.2;
    let n = n_samples(dur);
    let mut bp = Biquad::new(Band::Pass, 400.0, 0.9);
    let mut out = Vec::with_capacity(n);
    let (mut p1, mut p2) = (0.0f32, 0.0f32);
    for i in 0..n {
        let t = i as f32 / n as f32;
        let f = 300.0 * (12.0f32).powf(t * t);
        if i % 32 == 0 {
            bp.set(Band::Pass, f * 2.0, 0.9);
        }
        p1 += TAU * f / SRF;
        p2 += TAU * f * 1.503 / SRF;
        let env = t.powf(2.2) * if t > 0.96 { (1.0 - t) / 0.04 } else { 1.0 };
        let tone = (p1.sin() * 0.5 + p2.sin() * 0.25) * (1.0 + 0.3 * (TAU * 9.0 * t * dur).sin());
        out.push((bp.run(noise(&mut rng)) * 1.4 + tone * 0.35) * env);
    }
    normalize(&mut out, 0.85);
    to_stereo(&out, |p| (TAU * p * 2.0).sin() * 0.35)
}

fn hit(seed: u64) -> Stereo {
    let mut rng = Rng::new(seed);
    let n = n_samples(0.9);
    let mut lp = Biquad::new(Band::Low, 2400.0, 0.7);
    let mut out = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / SRF;
        let f = 45.0 + 85.0 * (-t * 22.0).exp();
        phase += TAU * f / SRF;
        let kick = phase.sin() * (-t * 5.5).exp() * (t / 0.002).min(1.0);
        let crack = lp.run(noise(&mut rng)) * (-t * 28.0).exp() * 0.7;
        let s = (kick * 1.3 + crack).tanh();
        out.push(s);
    }
    normalize(&mut out, 0.95);
    edge_fade(&mut out, 8, 4000);
    to_stereo(&out, |_| 0.0)
}

fn typewriter(seed: u64) -> Stereo {
    let mut rng = Rng::new(seed);
    let keys = 7;
    let mut out = vec![0.0f32; n_samples(0.75)];
    let mut at = 0usize;
    for k in 0..keys {
        let c = click(
            seed.wrapping_add(k as u64 * 7919),
            rng.range(0.7, 1.0),
            0.04,
        );
        for (i, s) in c.iter().enumerate() {
            if at + i < out.len() {
                out[at + i] += s;
            }
        }
        at += n_samples(rng.range(0.065, 0.095));
    }
    normalize(&mut out, 0.85);
    edge_fade(&mut out, 4, 256);
    let mut r2 = Rng::new(seed ^ 77);
    let pans: Vec<f32> = (0..keys).map(|_| r2.range(-0.25, 0.25)).collect();
    to_stereo(&out, move |p| {
        pans[((p * keys as f32) as usize).min(keys - 1)]
    })
}

fn glitch(seed: u64) -> Stereo {
    let mut rng = Rng::new(seed);
    let n = n_samples(0.4);
    let mut out = vec![0.0f32; n];
    let mut i = 0;
    while i < n {
        let seg = n_samples(rng.range(0.015, 0.05)).max(1);
        let kind = rng.below(3);
        let f = rng.range(150.0, 1800.0);
        let crush = rng.range(4.0, 16.0);
        let gain = rng.range(0.4, 1.0);
        for j in 0..seg.min(n - i) {
            let t = j as f32 / SRF;
            let s = match kind {
                0 => ((TAU * f * t).sin() * crush).round() / crush,
                1 => {
                    if (t * f).fract() < 0.5 {
                        1.0
                    } else {
                        -1.0
                    }
                }
                _ => noise(&mut rng),
            };
            out[i + j] = s * gain * 0.6;
        }
        i += seg + if rng.chance(0.3) { n_samples(0.012) } else { 0 };
    }
    normalize(&mut out, 0.75);
    edge_fade(&mut out, 32, 256);
    let mut r2 = Rng::new(seed ^ 5);
    let flips: Vec<f32> = (0..8).map(|_| r2.range(-0.5, 0.5)).collect();
    to_stereo(&out, move |p| flips[((p * 8.0) as usize).min(7)])
}

fn camera(seed: u64) -> Stereo {
    let mut rng = Rng::new(seed);
    let n = n_samples(0.32);
    let mut out = vec![0.0f32; n];
    let mut res = Biquad::new(Band::Pass, 950.0, 6.0);
    let mut hp = Biquad::new(Band::High, 1200.0, 0.7);
    for (start, level) in [(0.0f32, 1.0f32), (0.085, 0.75)] {
        let s0 = n_samples(start);
        for j in 0..n_samples(0.09) {
            if s0 + j >= n {
                break;
            }
            let t = j as f32 / SRF;
            let x = noise(&mut rng);
            let env = (-t * 90.0).exp();
            out[s0 + j] += (hp.run(x) * 0.8 + res.run(x) * 3.0) * env * level;
        }
    }
    // Mechanical rattle of the shutter curtain.
    for j in n_samples(0.01)..n_samples(0.08) {
        let t = j as f32 / SRF;
        out[j] += noise(&mut rng) * 0.12 * (-(t - 0.01) * 40.0).exp();
    }
    normalize(&mut out, 0.85);
    edge_fade(&mut out, 4, 512);
    to_stereo(&out, |_| 0.0)
}

fn record_scratch(seed: u64) -> Stereo {
    let mut rng = Rng::new(seed);
    let n = n_samples(0.55);
    let mut bp = Biquad::new(Band::Pass, 800.0, 2.0);
    let mut out = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    for i in 0..n {
        let t = i as f32 / SRF;
        // Back-and-forth platter speed: a fast forward then backward drag.
        let speed = (TAU * 3.5 * t).sin() * (1.0 - t / 0.55) + 0.25 * (TAU * 11.0 * t).sin();
        let f = 260.0 + 1400.0 * speed.abs();
        phase += TAU * f / SRF;
        if i % 32 == 0 {
            bp.set(Band::Pass, 500.0 + 2500.0 * speed.abs(), 2.0);
        }
        let saw = (phase / TAU).fract() * 2.0 - 1.0;
        let env =
            (t / 0.01).min(1.0) * (1.0 - t / 0.55).max(0.0).powf(0.6) * (0.35 + 0.65 * speed.abs());
        out.push((saw * 0.45 + bp.run(noise(&mut rng)) * 1.6) * env);
    }
    normalize(&mut out, 0.85);
    edge_fade(&mut out, 16, 512);
    to_stereo(&out, |p| (p * TAU * 3.5).sin() * 0.3)
}

/// Synthesize a variant of `kind`; `variant` changes the noise seed / pitch.
pub fn synth(kind: SfxKind, variant: u64) -> Stereo {
    let seed = 0xC11F_C07 ^ variant.wrapping_mul(0x9E37_79B9);
    match kind {
        SfxKind::Whoosh => whoosh(seed, 0.5, 280.0, 3200.0, 0.32),
        SfxKind::Swoosh => whoosh(seed, 0.3, 700.0, 5200.0, 0.4),
        SfxKind::Pop => pop(seed),
        SfxKind::Click => click_sfx(seed),
        SfxKind::Ding => ding(),
        SfxKind::Riser => riser(seed),
        SfxKind::Hit => hit(seed),
        SfxKind::Typewriter => typewriter(seed),
        SfxKind::Glitch => glitch(seed),
        SfxKind::Camera => camera(seed),
        SfxKind::RecordScratch => record_scratch(seed),
    }
}

/// Where an SFX's "moment" sits inside its sample, so cues land on it.
/// E.g. a whoosh peaks ~0.16 s in; a riser climaxes at its end.
pub fn sfx_anchor(kind: SfxKind) -> f32 {
    match kind {
        SfxKind::Whoosh => 0.16,
        SfxKind::Swoosh => 0.12,
        SfxKind::Riser => 1.2,
        _ => 0.0,
    }
}

// ---------------------------------------------------------------------------
// User overrides

/// `<sfx dir>/<snake_kind>*.wav|mp3`, sorted for deterministic picks.
pub fn find_overrides(dir: &Path) -> HashMap<SfxKind, Vec<PathBuf>> {
    let mut map: HashMap<SfxKind, Vec<PathBuf>> = HashMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return map;
    };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    files.sort();
    for f in files {
        let ext = f
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "wav" | "mp3" | "ogg" | "flac") {
            continue;
        }
        let stem = f
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        // Longest matching prefix wins ("record_scratch" before e.g. "re...").
        let kind = SfxKind::ALL
            .iter()
            .filter(|k| stem.starts_with(k.snake_name()))
            .max_by_key(|k| k.snake_name().len());
        if let Some(k) = kind {
            map.entry(*k).or_default().push(f);
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Mixer

fn add_at(dst: &mut [f32], src: &[f32], at_s: f32, gain: f32) {
    let start = (at_s.max(0.0) * SRF) as usize * 2;
    let skip = if at_s < 0.0 {
        ((-at_s) * SRF) as usize * 2
    } else {
        0
    };
    if skip >= src.len() {
        return;
    }
    for (i, s) in src[skip..].iter().enumerate() {
        let j = start + i;
        if j >= dst.len() {
            break;
        }
        dst[j] += s * gain;
    }
}

/// Envelope follower on voice RMS -> per-sample music gain.
fn duck_curve(voice: &[f32], frames: usize, duck_to: f32) -> Vec<f32> {
    let win = (SRF * 0.01) as usize;
    let mut gains = vec![1.0f32; frames];
    let duck_to = if duck_to.is_finite() {
        duck_to.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if duck_to >= 0.999 || voice.is_empty() || frames == 0 {
        return gains;
    }
    let windows = frames.div_ceil(win);
    let mut target = Vec::with_capacity(windows);
    for w in 0..windows {
        let (a, b) = (w * win, ((w + 1) * win).min(frames));
        let mut sum = 0.0;
        for f in a..b {
            let l = voice.get(f * 2).copied().unwrap_or(0.0);
            let r = voice.get(f * 2 + 1).copied().unwrap_or(0.0);
            // Energy, not a mono sum: opposite-phase speech must still duck.
            sum += (l * l + r * r) * 0.5;
        }
        let rms = (sum / (b - a).max(1) as f32).sqrt();
        target.push(if rms > 0.012 { duck_to } else { 1.0 });
    }
    // Hold speech state across short pauses between words (~200 ms).
    let hold = 20;
    let mut held = target.clone();
    let mut last_speech: Option<usize> = None;
    for (w, &t) in target.iter().enumerate() {
        if t < 1.0 {
            last_speech = Some(w);
        } else if last_speech.is_some_and(|l| w - l <= hold) {
            held[w] = duck_to;
        }
    }
    let attack = (-1.0 / (0.06 * SRF)).exp();
    let release = (-1.0 / (0.35 * SRF)).exp();
    let mut g = 1.0f32;
    for (f, out) in gains.iter_mut().enumerate() {
        let t = held[(f / win).min(held.len() - 1)];
        let c = if t < g { attack } else { release };
        g = t + (g - t) * c;
        *out = g;
    }
    gains
}

/// Look-ahead peak limiter (ceiling ~-1 dBFS) followed by a gentle soft clip.
pub fn limit(buf: &mut [f32]) {
    let frames = buf.len() / 2;
    if frames == 0 {
        return;
    }
    let ceiling = 0.89f32;
    let mut g: Vec<f32> = (0..frames)
        .map(|f| {
            let p = buf[f * 2].abs().max(buf[f * 2 + 1].abs());
            if p > ceiling { ceiling / p } else { 1.0 }
        })
        .collect();
    // Backward pass: ramp down ahead of peaks (5 ms attack).
    let att = (SRF * 0.005) as f32;
    for f in (0..frames - 1).rev() {
        let lim = g[f + 1] + (1.0 - g[f + 1]) / att;
        g[f] = g[f].min(lim);
    }
    // Forward pass: smooth release (80 ms).
    let rel = (-1.0 / (0.08 * SRF)).exp();
    let mut cur = 1.0f32;
    for gf in g.iter_mut() {
        cur = if *gf < cur {
            *gf
        } else {
            *gf + (cur - *gf) * rel
        };
        *gf = cur;
    }
    for f in 0..frames {
        for c in 0..2 {
            let s = buf[f * 2 + c] * g[f];
            // Soft knee above 0.8.
            buf[f * 2 + c] = if s.abs() <= 0.8 {
                s
            } else {
                s.signum() * (0.8 + 0.2 * ((s.abs() - 0.8) / 0.2).tanh())
            };
        }
    }
}

pub struct MixReport {
    pub warnings: Vec<String>,
}

/// Mix the whole audio plan into an interleaved stereo 48 kHz buffer.
pub fn mix(
    tl: &Timeline,
    ffmpeg: &Path,
    sfx_dir: Option<&Path>,
    control: &Control,
) -> anyhow::Result<(Stereo, MixReport)> {
    control.check()?;
    // Match the video frame count, including the final partial frame.
    let duration = (tl.duration_s * tl.fps as f32).ceil() / tl.fps as f32;
    let frames = (duration * SRF).round() as usize;
    let mut out = vec![0.0f32; frames * 2];
    let mut warnings = Vec::new();
    let plan = &tl.audio;

    // Voice.
    let mut voice: Vec<f32> = Vec::new();
    if let Some(vp) = &plan.voice_path {
        match decode_audio(
            ffmpeg,
            Path::new(vp),
            2,
            0.0,
            Some(duration),
            false,
            control,
        ) {
            Ok(mut v) => {
                normalize(&mut v, 0.89);
                for s in &mut v {
                    *s *= plan.voice_gain;
                }
                add_at(&mut out, &v, 0.0, 1.0);
                voice = v;
            }
            Err(e) => warnings.push(format!("voice track skipped: {e:#}")),
        }
    }

    control.check()?;

    // SFX.
    let overrides = sfx_dir.map(find_overrides).unwrap_or_default();
    // Keep the actual anchor alongside the buffer. A broken user override
    // falls back to synthesis, including its peak offset, not anchor zero.
    let mut cache: HashMap<(SfxKind, u64), (Stereo, f32)> = HashMap::new();
    for (ci, cue) in plan.sfx.iter().enumerate() {
        control.check()?;
        let variant = fnv1a(format!("{ci}-{:.2}", cue.at_s).as_bytes()) % 4;
        let key = match overrides.get(&cue.kind) {
            Some(files) if !files.is_empty() => (cue.kind, 1000 + (variant % files.len() as u64)),
            _ => (cue.kind, variant),
        };
        if let std::collections::hash_map::Entry::Vacant(entry) = cache.entry(key) {
            let mut decoded = None;
            if key.1 >= 1000 {
                let f = &overrides[&cue.kind][(key.1 - 1000) as usize];
                match decode_audio(ffmpeg, f, 2, 0.0, Some(6.0), false, control) {
                    Ok(mut b) if !b.is_empty() => {
                        normalize(&mut b, 0.9);
                        decoded = Some((b, 0.0));
                    }
                    Ok(_) => warnings.push(format!(
                        "empty SFX override {}; using synthesis",
                        f.display()
                    )),
                    Err(e) => {
                        control.check()?;
                        warnings.push(format!(
                            "SFX override {} unusable; using synthesis: {e:#}",
                            f.display()
                        ));
                    }
                }
            }
            entry.insert(
                decoded.unwrap_or_else(|| (synth(cue.kind, variant), sfx_anchor(cue.kind))),
            );
        }
        let (buf, anchor) = &cache[&key];
        add_at(&mut out, buf, cue.at_s - anchor, cue.gain);
    }

    // Clip audio.
    for ca in &plan.clip_audio {
        control.check()?;
        match decode_audio(
            ffmpeg,
            Path::new(&ca.path),
            2,
            ca.trim_start_s,
            Some(ca.duration_s),
            false,
            control,
        ) {
            Ok(mut b) => {
                let f = n_samples(0.03) * 2;
                edge_fade(&mut b, f, f);
                add_at(&mut out, &b, ca.at_s, ca.gain);
            }
            Err(e) => warnings.push(format!("clip audio skipped: {e:#}")),
        }
    }

    control.check()?;

    // Music bed, looped and ducked under the voice.
    if let Some(m) = &plan.music {
        match decode_audio(
            ffmpeg,
            Path::new(&m.path),
            2,
            0.0,
            Some(duration),
            true,
            control,
        ) {
            Ok(mut b) => {
                normalize(&mut b, 0.8);
                let gains = duck_curve(&voice, frames, m.duck_to);
                let fade_in = SRF * 0.3;
                let fade_out = SRF * 1.5;
                let n = (b.len() / 2).min(frames);
                for f in 0..n {
                    if f % 16_384 == 0 {
                        control.check()?;
                    }
                    let mut g = m.gain * gains[f];
                    g *= (f as f32 / fade_in).min(1.0);
                    g *= ((frames - f) as f32 / fade_out).min(1.0);
                    out[f * 2] += b[f * 2] * g;
                    out[f * 2 + 1] += b[f * 2 + 1] * g;
                }
            }
            Err(e) => warnings.push(format!("music skipped: {e:#}")),
        }
    }

    control.check()?;
    limit(&mut out);
    control.check()?;
    Ok((out, MixReport { warnings }))
}

pub fn write_wav(path: &Path, buf: &[f32]) -> anyhow::Result<()> {
    write_wav_controlled(path, buf, None)
}

pub fn write_wav_controlled(
    path: &Path,
    buf: &[f32],
    control: Option<&Control>,
) -> anyhow::Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SR,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)
        .with_context(|| format!("cannot write {}", path.display()))?;
    for (i, s) in buf.iter().enumerate() {
        if i % 16_384 == 0
            && let Some(c) = control
        {
            c.check()?;
        }
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
    }
    w.finalize()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_all_kinds_are_sane() {
        for k in SfxKind::ALL {
            let b = synth(k, 0);
            assert!(b.len() > 1000, "{k:?} too short");
            assert!(b.len() % 2 == 0);
            let peak = b.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.3 && peak <= 1.3, "{k:?} peak {peak}");
            assert!(b.iter().all(|s| s.is_finite()), "{k:?} non-finite");
        }
    }

    #[test]
    fn limiter_caps_peaks() {
        let mut b: Vec<f32> = (0..96_000).map(|i| (i as f32 * 0.05).sin() * 2.5).collect();
        limit(&mut b);
        assert!(b.iter().all(|s| s.abs() <= 1.0));
    }

    #[test]
    fn ducking_detects_right_only_and_opposite_phase_voice_then_recovers() {
        let frames = SR as usize * 2;
        for (left, right) in [(0.0, 0.3), (0.3, -0.3)] {
            let mut voice = vec![0.0; frames * 2];
            for f in 0..SR as usize / 2 {
                voice[f * 2] = left;
                voice[f * 2 + 1] = right;
            }
            let gains = duck_curve(&voice, frames, 0.35);
            assert!(gains[SR as usize / 3] < 0.36);
            // Short gaps retain the speech envelope; long silence recovers.
            assert!(gains[(SR as f32 * 0.65) as usize] < 0.37);
            assert!(gains[frames - 1] > 0.97);
            assert!(
                gains
                    .iter()
                    .all(|g| g.is_finite() && (0.35..=1.0).contains(g))
            );
        }
        assert!(duck_curve(&[], frames, 0.35).iter().all(|g| *g == 1.0));
        assert!(duck_curve(&[1.0; 100], 50, 1.0).iter().all(|g| *g == 1.0));
        assert!(duck_curve(&[], 0, 0.35).is_empty());
    }

    #[test]
    fn negative_cue_crops_source_instead_of_delaying_its_anchor() {
        let src = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let mut out = [0.0; 6];
        add_at(&mut out, &src, -1.0 / SRF, 0.5);
        assert_eq!(out, [1.5, 2.0, 2.5, 3.0, 0.0, 0.0]);
        for kind in SfxKind::ALL {
            assert_eq!(synth(kind, 2), synth(kind, 2));
            assert!(sfx_anchor(kind) * SRF <= synth(kind, 2).len() as f32 / 2.0 + 1.0);
        }
    }
}

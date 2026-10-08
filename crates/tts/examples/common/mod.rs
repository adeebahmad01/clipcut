use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use clipcut_core::{Config, Paths, Tools, VoiceTrack, WordTiming};
use clipcut_tts::{Tts, audio, normalize::normalize_words};
use serde::Serialize;

pub const NARRATION: &str = "Small steps build lasting habits. Keep moving forward!";
pub const ALIGNMENT: &str =
    "approximate phoneme-weight / energy-envelope timing; NOT forced alignment";

pub struct Options {
    pub config: Config,
    pub voice: Option<String>,
    pub out: PathBuf,
    pub text: String,
    pub expected_engine: Option<String>,
    pub home: Option<PathBuf>,
}

impl Options {
    pub fn parse(file_name: &str) -> anyhow::Result<Self> {
        let mut options = Self {
            config: Config::default(),
            voice: None,
            out: PathBuf::from("/tmp/clipcut-tts-smoke").join(file_name),
            text: NARRATION.into(),
            expected_engine: None,
            home: None,
        };
        let mut args = std::env::args().skip(1);
        let value = |args: &mut std::iter::Skip<std::env::Args>, flag: &str| {
            args.next()
                .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))
        };
        let mut text = Vec::new();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--engine" => options.config.tts.engine = value(&mut args, &arg)?,
                "--voice" => options.voice = Some(value(&mut args, &arg)?),
                "--speed" => {
                    options.config.tts.speed =
                        value(&mut args, &arg)?.parse().context("invalid --speed")?
                }
                "--command" => options.config.tts.command = value(&mut args, &arg)?,
                "--out" => options.out = PathBuf::from(value(&mut args, &arg)?),
                "--out-dir" => options.out = PathBuf::from(value(&mut args, &arg)?).join(file_name),
                "--home" => options.home = Some(PathBuf::from(value(&mut args, &arg)?)),
                "--expect-engine" => options.expected_engine = Some(value(&mut args, &arg)?),
                "--" => {
                    text.extend(args);
                    break;
                }
                "--help" | "-h" => {
                    println!(
                        "Options: --engine kokoro|espeak|command --voice ID --speed FLOAT \
                         --out FILE.wav --out-dir DIR --home DIR --expect-engine ENGINE \
                         --command TEMPLATE [\"narration\"]\n\
                         Writes a WAV and checked .json sidecar. Alignment is approximate, not forced."
                    );
                    std::process::exit(0);
                }
                flag if flag.starts_with("--") => anyhow::bail!("unknown option {flag}"),
                _ => text.push(arg),
            }
        }
        if !text.is_empty() {
            options.text = text.join(" ");
        }
        ensure!(options.out.file_name().is_some(), "--out must name a file");
        Ok(options)
    }

    pub fn tts(&self) -> Tts {
        let paths = Paths::resolve(self.home.clone());
        let tools = Tools::detect(&self.config, &paths);
        Tts::new(self.config.clone(), paths, tools)
    }

    pub fn directory(&self) -> &Path {
        self.out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
    }
}

#[derive(Debug, Serialize)]
pub struct Validation {
    pub alignment_method: &'static str,
    pub sample_count: usize,
    pub duration_s: f32,
    pub peak: f32,
    pub rms: f64,
    pub active_s: f32,
    pub shortest_word_s: f32,
    pub longest_word_s: f32,
    pub word_count: usize,
    pub sentence_count: usize,
    pub timings_monotonic_bounded_and_complete: bool,
    pub transcript_recognition_performed: bool,
}

/// Check the actual file, not just a successful return or a printed duration.
/// Acoustic activity does not certify pronunciation or transcript accuracy.
pub fn verify_and_save(
    track: &VoiceTrack,
    sentences: &[String],
    wav: &Path,
    expected_engine: Option<&str>,
) -> anyhow::Result<Validation> {
    if let Some(engine) = expected_engine {
        ensure!(
            track.engine == engine,
            "expected {engine}, got {} (possible fallback)",
            track.engine
        );
    }
    let spec = hound::WavReader::open(wav)?.spec();
    ensure!(
        spec.channels == 1 && spec.bits_per_sample == 16,
        "expected mono PCM16 WAV"
    );
    let (samples, rate) = audio::read_wav(wav)?;
    audio::validate_speech(&samples, rate)?;
    ensure!(
        rate == 24_000 && rate == track.sample_rate,
        "unexpected sample rate {rate}"
    );
    let duration_s = samples.len() as f32 / rate as f32;
    ensure!(
        (duration_s - track.duration_s).abs() <= 1.0 / rate as f32,
        "track/WAV duration mismatch"
    );
    ensure!(
        track.sentences.len() == sentences.len(),
        "missing sentence timings"
    );
    let expected_words: Vec<_> = sentences.iter().flat_map(|s| normalize_words(s)).collect();
    ensure!(
        expected_words.len() == track.words.len() && !expected_words.is_empty(),
        "missing word timings"
    );
    for (expected, actual) in expected_words.iter().zip(&track.words) {
        ensure!(expected.original == actual.text, "word text mismatch");
    }
    check_timings(&track.words, duration_s)?;
    check_timings(&track.sentences, duration_s)?;
    let mut word_index = 0;
    for (text, sentence) in sentences.iter().zip(&track.sentences) {
        ensure!(sentence.text == *text, "sentence text mismatch");
        let count = normalize_words(text).len();
        let words = &track.words[word_index..word_index + count];
        ensure!(count > 0, "empty sentence timing");
        ensure!(
            words[0].start_s == sentence.start_s && words[count - 1].end_s == sentence.end_s,
            "sentence does not contain its words"
        );
        ensure!(
            words
                .iter()
                .all(|w| w.start_s >= sentence.start_s && w.end_s <= sentence.end_s),
            "word crosses a sentence boundary"
        );
        word_index += count;
    }
    let env = audio::rms_envelope(&samples, rate);
    let threshold = audio::silence_threshold(&env);
    let durations: Vec<f32> = track.words.iter().map(|w| w.end_s - w.start_s).collect();
    let validation = Validation {
        alignment_method: ALIGNMENT,
        sample_count: samples.len(),
        duration_s,
        peak: samples.iter().copied().map(f32::abs).fold(0.0, f32::max),
        rms: (samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / samples.len() as f64)
            .sqrt(),
        active_s: env.iter().filter(|e| **e > threshold).count() as f32 * audio::HOP_S,
        shortest_word_s: durations.iter().copied().fold(f32::INFINITY, f32::min),
        longest_word_s: durations.iter().copied().fold(0.0, f32::max),
        word_count: track.words.len(),
        sentence_count: track.sentences.len(),
        timings_monotonic_bounded_and_complete: true,
        transcript_recognition_performed: false,
    };
    let evidence = serde_json::json!({ "track": track, "validation": validation });
    std::fs::write(
        wav.with_extension("json"),
        serde_json::to_vec_pretty(&evidence)?,
    )?;
    Ok(validation)
}

fn check_timings(timings: &[WordTiming], duration: f32) -> anyhow::Result<()> {
    ensure!(!timings.is_empty(), "no speech timings");
    let mut previous_end = 0.0;
    for timing in timings {
        ensure!(
            timing.start_s.is_finite()
                && timing.end_s.is_finite()
                && timing.start_s >= previous_end
                && timing.end_s > timing.start_s
                && timing.end_s <= duration,
            "invalid timing {timing:?}"
        );
        previous_end = timing.end_s;
    }
    Ok(())
}

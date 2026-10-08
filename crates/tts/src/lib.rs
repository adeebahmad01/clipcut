//! Free local voiceover: Kokoro-82M through ONNX Runtime with espeak-ng
//! phonemes, an espeak-ng fallback, and a user command engine. Returns
//! approximate, audio-anchored word timings so beats and captions track the
//! real audio. These timings are not transcript-based forced alignment.
//!
//! PUBLIC API CONTRACT — the app crate codes against these signatures.

use std::path::Path;

use clipcut_core::{Config, Paths, Progress, Tools, VoiceTrack};
use serde::Serialize;

pub mod align;
pub mod audio;
pub mod kokoro;
pub mod models;
pub mod normalize;
pub mod phonemize;
mod synth;
pub mod voices;

pub use models::{DEFAULT_KOKORO_VARIANT, KOKORO_VARIANT_ENV, KokoroVariant, ModelFiles};
pub use normalize::normalize;

/// Voice used when neither the request nor the config names a valid one.
pub const DEFAULT_VOICE: &str = "am_michael";
/// Default espeak-ng fallback voice.
pub const DEFAULT_ESPEAK_VOICE: &str = "en-us+m3";

#[derive(Clone)]
pub struct Tts {
    pub config: Config,
    pub paths: Paths,
    pub tools: Tools,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceInfo {
    pub id: String,
    pub label: String,
    pub engine: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TtsHealth {
    pub engine: String,
    pub ready: bool,
    pub detail: String,
}

impl Tts {
    pub fn new(config: Config, paths: Paths, tools: Tools) -> Self {
        Self {
            config,
            paths,
            tools,
        }
    }

    /// Download the Kokoro model + voices into `paths.models()` if missing.
    pub async fn ensure_models(&self, progress: &Progress) -> anyhow::Result<()> {
        let files = models::ensure(&self.paths.models(), progress).await?;
        progress.info(format!(
            "Kokoro model ready ({}, {})",
            files.variant.name(),
            files.model.display()
        ));
        Ok(())
    }

    /// Located Kokoro files, if complete on disk.
    pub fn model_files(&self) -> Option<ModelFiles> {
        models::locate(&self.paths.models())
    }

    pub fn health(&self) -> TtsHealth {
        let engine = self.config.tts.engine.trim().to_ascii_lowercase();
        let espeak = self.tools.espeak_ng.as_ref();
        let (ready, detail) = match engine.as_str() {
            "espeak" | "espeak-ng" => match espeak {
                Some(p) => (true, format!("espeak-ng at {}", p.display())),
                None => (false, "espeak-ng is not installed".to_string()),
            },
            "command" => {
                if let Err(e) = validate_command(&self.config.tts.command) {
                    (false, e.to_string())
                } else {
                    (true, format!("command: {}", self.config.tts.command.trim()))
                }
            }
            "" | "kokoro" => match (self.model_files(), espeak) {
                (_, None) => (
                    false,
                    "Kokoro needs espeak-ng for phonemes, and it is not installed".to_string(),
                ),
                (Some(files), Some(_)) => {
                    match voices::load_voice(&files.voices, self.config.tts.voice.trim()) {
                        Ok(_) => (
                            true,
                            format!(
                                "Kokoro-82M {} ({}), voice {}; approximate audio-anchored timings",
                                files.variant.name(),
                                files.model.display(),
                                self.config.tts.voice
                            ),
                        ),
                        Err(e) => (
                            false,
                            format!(
                                "Kokoro voice pack unavailable ({e:#}); synthesis can fall back to espeak-ng"
                            ),
                        ),
                    }
                }
                (None, Some(_)) => {
                    let v = KokoroVariant::configured();
                    (
                        false,
                        format!(
                            "Kokoro model not downloaded yet ({} MB, {}); it downloads on first use, espeak-ng is used if that fails",
                            (v.size() + models::VOICES_SIZE) / 1_000_000,
                            v.name()
                        ),
                    )
                }
            },
            _ => (
                false,
                format!("unknown tts.engine {engine:?}; choose kokoro, espeak, or command"),
            ),
        };
        TtsHealth {
            engine: if engine.is_empty() {
                "kokoro".into()
            } else {
                engine
            },
            ready,
            detail,
        }
    }

    pub fn voices(&self) -> Vec<VoiceInfo> {
        let mut out: Vec<VoiceInfo> = voices::KOKORO_VOICES
            .iter()
            .map(|(id, _)| VoiceInfo {
                id: id.to_string(),
                label: voices::kokoro_label(id),
                engine: "kokoro".into(),
            })
            .collect();
        out.extend(voices::ESPEAK_VOICES.iter().map(|(id, label)| VoiceInfo {
            id: id.to_string(),
            label: label.to_string(),
            engine: "espeak".into(),
        }));
        out
    }

    /// Speak `sentences` in order into `out_wav` (mono). `voice` overrides the
    /// configured voice. The returned track's `path` is `out_wav` relative to
    /// `project_dir`, with per-word and per-sentence timings.
    pub async fn synthesize(
        &self,
        sentences: &[String],
        voice: Option<&str>,
        project_dir: &Path,
        out_wav: &Path,
        progress: &Progress,
    ) -> anyhow::Result<VoiceTrack> {
        // Reject speechless input before choosing/downloading an engine.
        synth::validate_input(sentences)?;
        let voice = voice
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(self.config.tts.voice.trim());
        let (engine, voice_id) = self.engine_for(voice, progress).await?;
        progress.info(format!(
            "Voicing {} sentences with {} ({voice_id})",
            sentences.len(),
            engine.name()
        ));
        progress.info(
            "Word timings use approximate phoneme weights and audio energy, not forced alignment",
        );
        let espeak = self.tools.espeak_ng.as_deref();
        let speed = self.config.tts.speed;
        let rendered = match synth::render(&engine, espeak, sentences, speed, progress).await {
            Ok(r) => r,
            Err(e) if matches!(engine, synth::Engine::Kokoro { .. }) && espeak.is_some() => {
                progress.warn(format!("Kokoro failed ({e:#}); falling back to espeak-ng"));
                let fallback = self.espeak_engine(voices::espeak_voice_for(&voice_id))?;
                let r = synth::render(&fallback, espeak, sentences, speed, progress).await?;
                return self.finish(
                    r,
                    &fallback,
                    voices::espeak_voice_for(&voice_id),
                    project_dir,
                    out_wav,
                );
            }
            Err(e) => return Err(e),
        };
        self.finish(rendered, &engine, &voice_id, project_dir, out_wav)
    }

    /// Short sample for the settings page.
    pub async fn preview(&self, text: &str, voice: &str, out_wav: &Path) -> anyhow::Result<()> {
        let sentences = split_sentences(text);
        anyhow::ensure!(!sentences.is_empty(), "nothing to say");
        let progress = Progress::log_only("tts-preview");
        let dir = out_wav.parent().unwrap_or(Path::new("."));
        self.synthesize(&sentences, Some(voice), dir, out_wav, &progress)
            .await?;
        Ok(())
    }

    fn finish(
        &self,
        rendered: synth::Rendered,
        engine: &synth::Engine,
        voice_id: &str,
        project_dir: &Path,
        out_wav: &Path,
    ) -> anyhow::Result<VoiceTrack> {
        audio::validate_speech(&rendered.samples, kokoro::SAMPLE_RATE)?;
        audio::write_wav(out_wav, &rendered.samples, kokoro::SAMPLE_RATE)?;
        let path = match out_wav.strip_prefix(project_dir) {
            Ok(rel) => rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
            Err(_) => out_wav.to_string_lossy().into_owned(),
        };
        Ok(VoiceTrack {
            path,
            duration_s: rendered.samples.len() as f32 / kokoro::SAMPLE_RATE as f32,
            sample_rate: kokoro::SAMPLE_RATE,
            engine: engine.name().to_string(),
            voice: voice_id.to_string(),
            words: rendered.words,
            sentences: rendered.sentences,
        })
    }

    fn espeak_engine(&self, voice: &str) -> anyhow::Result<synth::Engine> {
        let exe = self.tools.espeak_ng.clone().ok_or_else(|| {
            anyhow::anyhow!("espeak-ng is not installed (needed for the espeak voice)")
        })?;
        Ok(synth::Engine::Espeak {
            exe,
            voice: voice.to_string(),
        })
    }

    /// Pick the engine and the concrete voice id for a request.
    async fn engine_for(
        &self,
        voice: &str,
        progress: &Progress,
    ) -> anyhow::Result<(synth::Engine, String)> {
        let configured = self.config.tts.engine.trim().to_ascii_lowercase();
        if configured == "command" {
            let template = self.config.tts.command.trim().to_string();
            validate_command(&template)?;
            return Ok((
                synth::Engine::Command {
                    template,
                    ffmpeg: self.tools.ffmpeg.clone(),
                },
                "command".into(),
            ));
        }
        anyhow::ensure!(
            matches!(configured.as_str(), "" | "kokoro" | "espeak" | "espeak-ng"),
            "unknown tts.engine {configured:?}; choose kokoro, espeak, or command"
        );
        if voices::ESPEAK_VOICES.iter().any(|(v, _)| *v == voice) || voice.starts_with("en-") {
            return Ok((self.espeak_engine(voice)?, voice.to_string()));
        }
        if configured == "espeak" || configured == "espeak-ng" {
            let v = voices::espeak_voice_for(voice);
            return Ok((self.espeak_engine(v)?, v.to_string()));
        }

        // Kokoro.
        anyhow::ensure!(
            self.tools.espeak_ng.is_some(),
            "Kokoro needs espeak-ng for phonemes; install espeak-ng"
        );
        let models_dir = self.paths.models();
        let files = match models::locate(&models_dir) {
            Some(f) => Some(f),
            None => match models::ensure(&models_dir, progress).await {
                Ok(f) => Some(f),
                Err(e) => {
                    progress.warn(format!("Kokoro model unavailable ({e:#}); using espeak-ng"));
                    None
                }
            },
        };
        let Some(files) = files else {
            let v = voices::espeak_voice_for(voice);
            return Ok((self.espeak_engine(v)?, v.to_string()));
        };
        let mut candidates = vec![voice.to_string()];
        for c in [self.config.tts.voice.trim(), DEFAULT_VOICE] {
            if !candidates.iter().any(|v| v == c) && !c.is_empty() {
                candidates.push(c.to_string());
            }
        }
        let mut last_err = None;
        for (i, cand) in candidates.iter().enumerate() {
            let vf = files.voices.clone();
            let c = cand.clone();
            match tokio::task::spawn_blocking(move || voices::load_voice(&vf, &c)).await? {
                Ok(pack) => {
                    if i > 0 {
                        progress.warn(format!("Unknown Kokoro voice {voice:?}; using {cand}"));
                    }
                    let british = phonemize::espeak_lang_for_voice(cand) == "en-gb";
                    return Ok((
                        synth::Engine::Kokoro {
                            model: files.model.clone(),
                            voice: pack,
                            british,
                        },
                        cand.clone(),
                    ));
                }
                Err(e) => last_err = Some(e),
            }
        }
        let err = last_err.unwrap_or_else(|| anyhow::anyhow!("no usable Kokoro voice"));
        progress.warn(format!(
            "Kokoro voices unavailable ({err:#}); falling back to espeak-ng"
        ));
        let v = voices::espeak_voice_for(voice);
        Ok((self.espeak_engine(v)?, v.to_string()))
    }
}

fn validate_command(template: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        template.contains("{text_file}") && template.contains("{out_wav}"),
        "tts.command must use both {{text_file}} and {{out_wav}} placeholders"
    );
    Ok(())
}

/// Split at sentence punctuation followed by whitespace, keeping common
/// abbreviations, decimal points, and closing quotes with their sentence.
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        cur.push(*c);
        let body = cur.trim_end_matches(['"', '\'', '”', '’', ')', ']', '}']);
        let terminal = body.chars().last();
        let word = body.split_whitespace().last().unwrap_or("");
        let at_end = matches!(terminal, Some('.' | '!' | '?' | '…'))
            && chars.get(i + 1).is_none_or(|n| n.is_whitespace())
            && !(terminal == Some('.') && normalize::is_abbreviation(word));
        if at_end {
            out.push(cur.trim().to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sentences() {
        assert_eq!(
            split_sentences("Hi there. How are you? Fine"),
            vec!["Hi there.", "How are you?", "Fine"]
        );
        assert_eq!(
            split_sentences("It costs $3.50 today."),
            vec!["It costs $3.50 today."]
        );
        assert_eq!(split_sentences("Hi! Go."), vec!["Hi!", "Go."]);
        assert_eq!(
            split_sentences("Meet Dr. Adams. \"Go now!\" Fine."),
            vec!["Meet Dr. Adams.", "\"Go now!\"", "Fine."]
        );
    }

    #[test]
    fn voice_catalog() {
        let paths = Paths::resolve(Some("/nonexistent/clipcut".into()));
        let mut tools = Tools::detect(&Config::default(), &paths);
        tools.espeak_ng = None;
        let tts = Tts::new(Config::default(), paths, tools);
        let v = tts.voices();
        assert!(
            v.iter()
                .any(|v| v.id == "am_michael" && v.label == "Michael (US male)")
        );
        assert!(v.iter().any(|v| v.engine == "espeak"));
        assert!(v.len() >= 28);
        assert!(!tts.health().ready);
    }

    #[test]
    fn rejects_unknown_engine_and_incomplete_command() {
        let paths = Paths::resolve(Some("/nonexistent/clipcut".into()));
        let mut config = Config::default();
        config.tts.engine = "typo".into();
        let tools = Tools::detect(&config, &paths);
        assert!(
            !Tts::new(config.clone(), paths.clone(), tools.clone())
                .health()
                .ready
        );
        config.tts.engine = "command".into();
        config.tts.command = "echo nothing".into();
        assert!(!Tts::new(config, paths, tools).health().ready);
    }

    #[tokio::test]
    async fn speechless_public_requests_do_not_create_files_or_install_models() {
        let dir =
            std::env::temp_dir().join(format!("clipcut-tts-empty-public-{}", std::process::id()));
        let paths = Paths::resolve(Some(dir.join("home")));
        let tools = Tools::detect(&Config::default(), &paths);
        let tts = Tts::new(Config::default(), paths.clone(), tools);
        let out = dir.join("output/voice.wav");
        for sentences in [vec![], vec![String::new()], vec!["... 🚀".to_string()]] {
            assert!(
                tts.synthesize(
                    &sentences,
                    None,
                    &dir,
                    &out,
                    &Progress::log_only("empty-public-test")
                )
                .await
                .is_err()
            );
            assert!(!out.exists());
            assert!(!paths.home.exists());
        }
        assert!(!dir.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cancelling_command_synthesis_kills_child_and_leaves_no_artifact() {
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("clipcut-tts-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sentinel = dir.join("started");
        let out = dir.join("cancelled.wav");
        let mut config = Config::default();
        config.tts.engine = "command".into();
        config.tts.command = format!(
            "printf '%s\\n%s\\n' \"$$\" {{text_file}} > '{}'; exec sleep 20 # {{out_wav}}",
            sentinel.to_string_lossy().replace('\'', "'\\''")
        );
        let paths = Paths::resolve(Some(dir.join("home")));
        let mut tools = Tools::detect(&config, &paths);
        tools.espeak_ng = None;
        let tts = Tts::new(config, paths, tools);
        let project = dir.clone();
        let output = out.clone();
        let task = tokio::spawn(async move {
            tts.synthesize(
                &["Cancellation check.".into()],
                None,
                &project,
                &output,
                &Progress::log_only("cancel-test"),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while !sentinel.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("command must start promptly");
        let started = std::fs::read_to_string(&sentinel).unwrap();
        let mut lines = started.lines();
        let pid: u32 = lines.next().unwrap().parse().unwrap();
        let scratch = Path::new(lines.next().unwrap()).parent().unwrap();
        assert!(scratch.exists());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(3), async {
            while Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled command child must exit");
        assert!(!out.exists());
        assert!(!scratch.exists());
        assert!(!dir.join("home").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

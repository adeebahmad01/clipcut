//! Native renderer. The director turns a `Project` into a `Timeline`; the
//! compositor draws each frame with tiny-skia (clips decoded by ffmpeg,
//! text, icons, captions, cards, animations) and pipes raw frames into
//! ffmpeg/x264. Audio (voice, procedural SFX, ducked music) is mixed in Rust.
//!
//! PUBLIC API CONTRACT — the app crate codes against these signatures.

mod audio;
mod control;
mod director;
mod ffmpeg;
mod pipeline;
mod scene;
mod text;
mod thumbnail;
mod util;

use std::path::Path;

use clipcut_core::timeline::{SfxKind, Timeline};
use clipcut_core::{Config, Paths, Progress, Project, Tools};

#[derive(Clone)]
pub struct Renderer {
    pub config: Config,
    pub paths: Paths,
    pub tools: Tools,
}

/// Inputs the director needs that are not in `project.json`.
#[derive(Debug, Clone, Default)]
pub struct DirectorInputs {
    /// Absolute SVG path per beat id (Iconify), resolved by the app.
    pub icons: std::collections::HashMap<String, std::path::PathBuf>,
    /// Absolute path of the music bed, if any.
    pub music: Option<std::path::PathBuf>,
}

impl Renderer {
    pub fn new(config: Config, paths: Paths, tools: Tools) -> Self {
        Self {
            config,
            paths,
            tools,
        }
    }

    /// Build the edit: layout, animations, overlays, captions, SFX cues.
    /// Deterministic for a given project (seeded by project id).
    pub fn direct(
        &self,
        project: &Project,
        project_dir: &Path,
        inputs: &DirectorInputs,
    ) -> anyhow::Result<Timeline> {
        director::direct(
            &self.config,
            self.tools.ffprobe.as_deref(),
            inputs.music.as_deref(),
            &inputs.icons,
            project,
            project_dir,
        )
    }

    /// Render a timeline to an H.264/AAC mp4.
    pub async fn render(
        &self,
        timeline: &Timeline,
        out_mp4: &Path,
        progress: &Progress,
    ) -> anyhow::Result<()> {
        let ffmpeg = Tools::require(&self.tools.ffmpeg, "ffmpeg")?.to_path_buf();
        let sfx_dir = self.paths.sfx();
        let crf = self.config.video.crf;
        let control = std::sync::Arc::new(control::Control::default());
        let _cancel = control::CancelOnDrop(control.clone());
        let (tl, out, progress) = (timeline.clone(), out_mp4.to_path_buf(), progress.clone());
        let stats = tokio::task::spawn_blocking(move || {
            pipeline::render(&tl, &ffmpeg, Some(&sfx_dir), crf, &out, &progress, &control)
        })
        .await??;
        tracing::info!(
            "rendered {} frames in {:.1}s ({:.2}x real time)",
            stats.frames,
            stats.seconds,
            stats.seconds / timeline.duration_s.max(0.01)
        );
        Ok(())
    }

    /// Render a single frame to PNG (UI thumbnails / scrubbing).
    pub async fn still(
        &self,
        timeline: &Timeline,
        at_s: f32,
        out_png: &Path,
    ) -> anyhow::Result<()> {
        let ffmpeg = self.tools.ffmpeg.clone();
        let control = std::sync::Arc::new(control::Control::default());
        let _cancel = control::CancelOnDrop(control.clone());
        let (tl, out) = (timeline.clone(), out_png.to_path_buf());
        tokio::task::spawn_blocking(move || {
            pipeline::still(&tl, ffmpeg.as_deref(), at_s, &out, &control)
        })
        .await?
    }

    /// Designed cover: best beat visual, focus-cropped, hook in the accent
    /// color. `out_png` is the video's size; `out_jpg` is 1280x720 for upload.
    pub async fn thumbnail(
        &self,
        project: &Project,
        project_dir: &Path,
        timeline: &Timeline,
        out_png: &Path,
        out_jpg: &Path,
    ) -> anyhow::Result<()> {
        let (ffmpeg, ffprobe) = (self.tools.ffmpeg.clone(), self.tools.ffprobe.clone());
        let control = std::sync::Arc::new(control::Control::default());
        let _cancel = control::CancelOnDrop(control.clone());
        let (project, dir) = (project.clone(), project_dir.to_path_buf());
        let (accent, size) = (timeline.caption_style.accent, (timeline.width, timeline.height));
        let (png, jpg) = (out_png.to_path_buf(), out_jpg.to_path_buf());
        tokio::task::spawn_blocking(move || {
            thumbnail::render(
                &project,
                &dir,
                ffmpeg.as_deref(),
                ffprobe.as_deref(),
                accent,
                size,
                &png,
                &jpg,
                &control,
            )
        })
        .await?
    }
}

/// Synthesize one of the built-in sound effects as 48 kHz stereo 16-bit WAV
/// (for previews, or to seed the user's SFX folder).
pub fn write_sfx_wav(kind: SfxKind, variant: u64, out: &Path) -> anyhow::Result<()> {
    audio::write_wav(out, &audio::synth(kind, variant))
}

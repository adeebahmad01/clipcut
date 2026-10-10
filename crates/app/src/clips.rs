//! "Clip from a long video": captions → the LLM picks 3-5 self-contained
//! moments → one project per moment. Each clip's single beat is the source
//! footage itself and its voice is the source audio, so the pipeline only
//! downloads, extracts audio and renders it vertical with captions and SFX.

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use clipcut_core::config::Format;
use clipcut_core::model::{ClipSource, ProjectKind};
use clipcut_core::paths::ProjectPaths;
use clipcut_core::timeline::CaptionPreset;
use clipcut_core::{
    Beat, BeatStatus, CaptionCue, HookOption, Progress, Project, Script, Stage, Status, Tools,
    VisualKind, VisualPlan, VoiceTrack, WordTiming, align_beats, media, tools,
};
use clipcut_sources::VideoInfo;
use serde::Deserialize;

use crate::pipeline::{self, Outcome};
use crate::state::{AppState, Services};

/// Most moments taken from one video.
pub const MAX_CLIPS: usize = 5;

#[derive(Debug, Clone, Deserialize)]
pub struct ClipRequest {
    pub url: String,
    #[serde(default = "default_count")]
    pub count: usize,
    #[serde(default = "default_min")]
    pub min_s: f32,
    #[serde(default = "default_max")]
    pub max_s: f32,
    #[serde(default)]
    pub format: Option<Format>,
    #[serde(default)]
    pub caption_preset: Option<CaptionPreset>,
    #[serde(default)]
    pub extra_formats: Vec<Format>,
    /// Queue the clips for rendering right away.
    #[serde(default = "default_true")]
    pub start: bool,
}

fn default_count() -> usize {
    3
}
fn default_min() -> f32 {
    30.0
}
fn default_max() -> f32 {
    60.0
}
fn default_true() -> bool {
    true
}

impl ClipRequest {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            count: default_count(),
            min_s: default_min(),
            max_s: default_max(),
            format: None,
            caption_preset: None,
            extra_formats: Vec::new(),
            start: true,
        }
    }
}

/// Find the best moments of `req.url` and save one project per moment.
pub async fn create(
    state: &AppState,
    req: &ClipRequest,
    progress: &Progress,
) -> anyhow::Result<Vec<Project>> {
    let url = req.url.trim();
    anyhow::ensure!(
        url.starts_with("http://") || url.starts_with("https://"),
        "paste a YouTube link"
    );
    let count = req.count.clamp(1, MAX_CLIPS);
    let min_s = req.min_s.clamp(5.0, 180.0);
    let max_s = req.max_s.clamp(min_s, 180.0);
    let svc = state.services();

    progress.info("Reading the video's details");
    let info = svc.sources.youtube_info(url).await?;
    anyhow::ensure!(
        info.duration_s > min_s + 5.0,
        "this video is only {} long; pick something longer than the clips",
        clock(info.duration_s)
    );
    progress.info(format!("Fetching captions of \"{}\"", info.title));
    let cues = svc.sources.youtube_captions(&info.url).await?;
    anyhow::ensure!(
        !cues.is_empty(),
        "this video has no English captions, so its moments can't be found"
    );

    progress.info(format!("Picking the best {count} moments"));
    let mut picks = match svc
        .llm
        .pick_highlights(
            &info.title,
            &info.channel,
            &cues,
            count,
            min_s,
            max_s,
            progress,
        )
        .await
    {
        Ok(picks) if !picks.is_empty() => picks,
        Ok(_) => Vec::new(),
        Err(e) => {
            progress.warn(format!("Moment picker failed ({e:#}); using caption density"));
            Vec::new()
        }
    };
    if picks.is_empty() {
        picks = clipcut_llm::heuristic_highlights(&cues, count, min_s, max_s);
    }
    anyhow::ensure!(!picks.is_empty(), "no moment of the right length was found");

    let format = req.format.unwrap_or(svc.config.video.format);
    let extra = crate::server::clean_extra_formats(format, req.extra_formats.clone());
    let mut out = Vec::new();
    for h in picks.into_iter().take(count) {
        let end = h.end_s.min(info.duration_s);
        let start = h.start_s.clamp(0.0, (end - 5.0).max(0.0));
        let project = build_project(
            &info,
            &cues,
            ClipMoment {
                start_s: start,
                end_s: end,
                title: h.title,
                hook: h.hook,
                reason: h.reason,
            },
            format,
            extra.clone(),
            req.caption_preset,
        );
        out.push(state.store.create(project).await?);
    }
    progress.info(format!("Made {} clips from \"{}\"", out.len(), info.title));
    Ok(out)
}

pub struct ClipMoment {
    pub start_s: f32,
    pub end_s: f32,
    pub title: String,
    pub hook: String,
    pub reason: String,
}

pub fn build_project(
    info: &VideoInfo,
    cues: &[CaptionCue],
    moment: ClipMoment,
    format: Format,
    extra_formats: Vec<Format>,
    caption_preset: Option<CaptionPreset>,
) -> Project {
    let (start, end) = (moment.start_s, moment.end_s);
    let len = end - start;
    let title = if moment.title.trim().is_empty() {
        info.title.clone()
    } else {
        moment.title.trim().to_string()
    };
    let hook = moment.hook.trim().to_string();
    let window = window_cues(cues, start, end);
    let mut p = Project::new(title.clone(), "", format, len.round().max(1.0) as u32);
    p.kind = ProjectKind::Clip;
    p.autopilot = true;
    p.caption_preset = caption_preset;
    p.extra_formats = extra_formats;
    let sentences: Vec<String> = window.iter().map(|c| c.text.clone()).collect();
    p.script = Some(Script {
        text: sentences.join(" "),
        sentences,
        draft: String::new(),
        title: title.chars().take(100).collect(),
        description: format!(
            "Clip from {} by {}: {}",
            info.title, info.channel, info.url
        ),
        hashtags: clip_hashtags(&info.channel),
    });
    if !hook.is_empty() {
        p.hooks = vec![HookOption {
            text: hook.clone(),
            angle: "clip".into(),
            score: 1.0,
            why: moment.reason.clone(),
        }];
        p.chosen_hook = Some(0);
    }
    p.beats = vec![Beat {
        id: "b01".into(),
        text: p.script.as_ref().map(|s| s.text.clone()).unwrap_or_default(),
        visual: VisualPlan {
            kind: VisualKind::Youtube,
            query: info.url.clone(),
            alt_queries: Vec::new(),
            on_screen_text: (!hook.is_empty()).then(|| hook.clone()),
            icon: None,
            meme: None,
            quote: None,
            sfx: None,
        },
        candidates: Vec::new(),
        chosen: None,
        asset: None,
        status: BeatStatus::Pending,
        error: None,
        locked: false,
        start_s: 0.0,
        end_s: 0.0,
    }];
    for s in &mut p.stages {
        match s.stage {
            Stage::Research | Stage::Hooks | Stage::Script => {
                s.status = Status::Skipped;
                s.message = "From the source video".into();
            }
            Stage::Beats => {
                s.status = Status::Done;
                s.progress = 1.0;
                s.message = "Done".into();
            }
            _ => {}
        }
    }
    p.clip = Some(ClipSource {
        url: info.url.clone(),
        video_id: info.id.clone(),
        title: info.title.clone(),
        channel: info.channel.clone(),
        duration_s: info.duration_s,
        start_s: start,
        end_s: end,
        hook,
        reason: moment.reason,
        cues: window,
    });
    p
}

fn clip_hashtags(channel: &str) -> Vec<String> {
    let mut tags = vec!["#shorts".to_string(), "#clips".to_string()];
    let slug: String = channel
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    if !slug.is_empty() {
        tags.push(format!("#{slug}"));
    }
    tags
}

/// Cues overlapping `start..end`, shifted to start at 0 and clipped to the moment.
pub fn window_cues(cues: &[CaptionCue], start: f32, end: f32) -> Vec<CaptionCue> {
    cues.iter()
        .filter(|c| c.end_s > start && c.start_s < end && !c.text.trim().is_empty())
        .map(|c| CaptionCue {
            start_s: (c.start_s - start).max(0.0),
            end_s: (c.end_s.min(end) - start).max(0.0),
            text: c.text.trim().to_string(),
        })
        .filter(|c| c.end_s > c.start_s)
        .collect()
}

/// Assets stage of a clip: download the moment with its audio.
pub async fn download(
    state: &AppState,
    svc: &Services,
    project: &Project,
    dir: &Path,
    progress: &Progress,
) -> anyhow::Result<()> {
    let clip = project
        .clip
        .as_ref()
        .context("this clip has lost its source moment")?;
    let beat = project.beats.first().context("this clip has no beat")?;
    if let Some(a) = &beat.asset
        && dir.join(&a.path).exists()
    {
        progress.info("Clip already downloaded");
        return Ok(());
    }
    let info = VideoInfo {
        id: clip.video_id.clone(),
        url: clip.url.clone(),
        title: clip.title.clone(),
        channel: clip.channel.clone(),
        duration_s: clip.duration_s,
    };
    let assets = ProjectPaths(dir).assets();
    tokio::fs::create_dir_all(&assets).await?;
    let stem = format!("{}-{}", beat.id, uuid::Uuid::new_v4().simple());
    progress.beat(
        &beat.id,
        format!(
            "Downloading {}-{} of \"{}\"",
            clock(clip.start_s),
            clock(clip.end_s),
            clip.title
        ),
    );
    let mut asset = svc
        .sources
        .fetch_youtube_segment(&info, clip.start_s, clip.end_s, &assets, &stem)
        .await?;
    if let Ok(rel) = Path::new(&asset.path).strip_prefix(dir) {
        asset.path = rel.to_string_lossy().into_owned();
    } else if !dir.join(&asset.path).exists() && assets.join(&asset.path).exists() {
        asset.path = format!("assets/{}", asset.path);
    }
    pipeline::save_outcome(
        state,
        &project.id,
        &beat.id,
        Outcome {
            visual: beat.visual.clone(),
            candidates: Vec::new(),
            chosen: None,
            asset: Some(asset),
            status: BeatStatus::Ready,
            error: None,
        },
        true,
    )
    .await?;
    Ok(())
}

/// Voice stage of a clip: the source audio, timed by its own captions.
pub async fn extract_voice(
    state: &AppState,
    svc: &Services,
    project: &Project,
    dir: &Path,
    progress: &Progress,
) -> anyhow::Result<()> {
    let clip = project
        .clip
        .as_ref()
        .context("this clip has lost its source moment")?;
    let asset = project
        .beats
        .first()
        .and_then(|b| b.asset.as_ref())
        .context("download the clip first")?;
    let ffmpeg = Tools::require(&svc.tools.ffmpeg, "ffmpeg")?;
    let ffprobe = Tools::require(&svc.tools.ffprobe, "ffprobe")?;
    let source = dir.join(&asset.path);
    let probe = media::probe(ffprobe, &source).await?;
    anyhow::ensure!(probe.has_audio, "the downloaded clip has no audio track");

    progress.info("Extracting the clip's audio");
    let voice_dir = ProjectPaths(dir).voice();
    tokio::fs::create_dir_all(&voice_dir).await?;
    let out = voice_dir.join("voice.wav");
    let (src, dst) = (source.to_string_lossy(), out.to_string_lossy());
    tools::run(
        ffmpeg,
        &[
            "-nostdin", "-y", "-v", "error", "-i", &src, "-vn", "-ac", "2", "-ar", "48000", "-c:a",
            "pcm_s16le", &dst,
        ],
        None,
        Duration::from_secs(120),
    )
    .await?;
    let duration_s = match media::probe(ffprobe, &out).await {
        Ok(p) if p.duration_s > 0.0 => p.duration_s,
        _ => clip.end_s - clip.start_s,
    };
    let track = VoiceTrack {
        path: "voice/voice.wav".into(),
        duration_s,
        sample_rate: 48_000,
        engine: "source".into(),
        voice: clip.channel.clone(),
        words: words_from_cues(&clip.cues, duration_s),
        sentences: clip
            .cues
            .iter()
            .filter(|c| c.start_s < duration_s)
            .map(|c| WordTiming {
                text: c.text.clone(),
                start_s: c.start_s,
                end_s: c.end_s.min(duration_s),
            })
            .collect(),
    };
    if track.words.is_empty() {
        progress.warn("The moment has no captions; the clip will render without them");
    }
    state
        .store
        .update(&project.id, |p| {
            align_beats(&mut p.beats, &track.words, track.duration_s);
            p.voice_track = Some(track);
            Ok(())
        })
        .await?;
    Ok(())
}

/// Word timings estimated from caption cues: each cue's span is shared by its
/// words in proportion to their length.
pub fn words_from_cues(cues: &[CaptionCue], duration_s: f32) -> Vec<WordTiming> {
    let mut out = Vec::new();
    for c in cues {
        let start = c.start_s.max(0.0);
        let end = c.end_s.min(duration_s);
        if end <= start {
            continue;
        }
        let words: Vec<&str> = c.text.split_whitespace().collect();
        let total: usize = words.iter().map(|w| w.chars().count()).sum();
        if total == 0 {
            continue;
        }
        let mut t = start;
        for w in words {
            let len = (end - start) * w.chars().count() as f32 / total as f32;
            out.push(WordTiming {
                text: w.to_string(),
                start_s: t,
                end_s: (t + len).min(end),
            });
            t += len;
        }
    }
    out
}

/// `m:ss`, or `h:mm:ss` past an hour.
pub fn clock(s: f32) -> String {
    let total = s.max(0.0).round() as u64;
    let (h, m, sec) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start_s: f32, end_s: f32, text: &str) -> CaptionCue {
        CaptionCue {
            start_s,
            end_s,
            text: text.into(),
        }
    }

    #[test]
    fn clock_formats_minutes_and_hours() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(65.4), "1:05");
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(-3.0), "0:00");
    }

    #[test]
    fn words_share_their_cue_by_length() {
        let words = words_from_cues(&[cue(1.0, 3.0, "ab abcd ab"), cue(5.0, 9.0, "late")], 6.0);
        let texts: Vec<_> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["ab", "abcd", "ab", "late"]);
        assert!((words[0].start_s - 1.0).abs() < 1e-4);
        assert!((words[0].end_s - 1.5).abs() < 1e-4);
        assert!((words[1].end_s - 2.5).abs() < 1e-4);
        assert!((words[2].end_s - 3.0).abs() < 1e-4);
        // Clipped to the audio's length.
        assert!((words[3].end_s - 6.0).abs() < 1e-4);
    }

    #[test]
    fn window_cues_shift_and_clip() {
        let cues = [cue(5.0, 9.0, "before"), cue(9.0, 12.0, "inside"), cue(19.0, 25.0, "edge")];
        let w = window_cues(&cues, 10.0, 20.0);
        assert_eq!(w.len(), 2);
        assert_eq!((w[0].start_s, w[0].end_s), (0.0, 2.0));
        assert_eq!((w[1].start_s, w[1].end_s), (9.0, 10.0));
    }

    #[test]
    fn built_clip_skips_writing_stages() {
        let info = VideoInfo {
            id: "abc".into(),
            url: "https://www.youtube.com/watch?v=abc".into(),
            title: "Long talk".into(),
            channel: "Some Channel".into(),
            duration_s: 600.0,
        };
        let cues = [cue(100.0, 104.0, "first line"), cue(104.0, 108.0, "second line")];
        let p = build_project(
            &info,
            &cues,
            ClipMoment {
                start_s: 100.0,
                end_s: 140.0,
                title: "".into(),
                hook: "Wait for it".into(),
                reason: "payoff".into(),
            },
            Format::Vertical,
            vec![Format::Square],
            Some(CaptionPreset::Hormozi),
        );
        assert_eq!(p.kind, ProjectKind::Clip);
        assert_eq!(p.topic, "Long talk");
        assert_eq!(p.target_seconds, 40);
        assert_eq!(p.beats.len(), 1);
        assert_eq!(p.beats[0].visual.on_screen_text.as_deref(), Some("Wait for it"));
        assert_eq!(p.chosen_hook_text(), Some("Wait for it"));
        assert_eq!(
            pipeline::first_unfinished(&p),
            Some(Stage::Assets),
            "research, hooks, script and beats come from the source"
        );
        let clip = p.clip.unwrap();
        assert_eq!(clip.cues.len(), 2);
        assert_eq!(clip.cues[1].start_s, 4.0);
        assert_eq!(p.script.unwrap().hashtags, ["#shorts", "#clips", "#somechannel"]);
    }
}

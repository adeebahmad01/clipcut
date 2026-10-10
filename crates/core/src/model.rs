//! The persisted project: one `project.json` per video. Every stage writes its
//! output here so any stage can be re-run or edited from the web UI.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::Format;
pub use crate::timeline::{CaptionPreset, RectF, SfxKind};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub topic: String,
    /// Optional brief from the user: angle, audience, must-mention facts.
    #[serde(default)]
    pub notes: String,
    pub format: Format,
    pub target_seconds: u32,
    /// Overrides the configured TTS voice for this project.
    #[serde(default)]
    pub voice: Option<String>,
    /// Run every stage without pausing for review.
    #[serde(default = "yes")]
    pub autopilot: bool,
    #[serde(default)]
    pub stages: Vec<StageState>,
    #[serde(default)]
    pub research: Option<Research>,
    #[serde(default)]
    pub hooks: Vec<HookOption>,
    #[serde(default)]
    pub chosen_hook: Option<usize>,
    #[serde(default)]
    pub script: Option<Script>,
    #[serde(default)]
    pub beats: Vec<Beat>,
    #[serde(default)]
    pub voice_track: Option<VoiceTrack>,
    #[serde(default)]
    pub render: Option<RenderOutput>,
    /// Caption look; `None` uses `video.caption_preset`.
    #[serde(default)]
    pub caption_preset: Option<CaptionPreset>,
    /// More aspect ratios rendered from the same edit, besides `format`.
    #[serde(default)]
    pub extra_formats: Vec<Format>,
    #[serde(default)]
    pub published: Vec<PublishRecord>,
    #[serde(default)]
    pub kind: ProjectKind,
    /// The long video a clip project was cut from.
    #[serde(default)]
    pub clip: Option<ClipSource>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectKind {
    /// Researched, scripted and narrated from a topic.
    #[default]
    Story,
    /// One moment cut from a long video, with its own audio.
    Clip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipSource {
    /// Watch URL of the long video.
    pub url: String,
    pub video_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub channel: String,
    /// Length of the whole source video.
    #[serde(default)]
    pub duration_s: f32,
    /// The moment, in source seconds.
    pub start_s: f32,
    pub end_s: f32,
    /// Overlay shown at the start of the clip.
    #[serde(default)]
    pub hook: String,
    /// Why the moment was picked.
    #[serde(default)]
    pub reason: String,
    /// The source's caption cues inside the moment, relative to `start_s`.
    #[serde(default)]
    pub cues: Vec<CaptionCue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishRecord {
    /// `youtube`.
    pub platform: String,
    pub format: Format,
    pub video_id: String,
    pub url: String,
    pub privacy: String,
    /// Scheduled release time, when the upload is private until then.
    #[serde(default)]
    pub publish_at: Option<DateTime<Utc>>,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Research,
    Hooks,
    Script,
    Beats,
    Assets,
    Voice,
    Render,
}

impl Stage {
    pub const ALL: [Stage; 7] =
        [Stage::Research, Stage::Hooks, Stage::Script, Stage::Beats, Stage::Assets, Stage::Voice, Stage::Render];

    pub fn label(self) -> &'static str {
        match self {
            Stage::Research => "Research",
            Stage::Hooks => "Hook",
            Stage::Script => "Script",
            Stage::Beats => "Beat sheet",
            Stage::Assets => "Visuals",
            Stage::Voice => "Voiceover",
            Stage::Render => "Render",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageState {
    pub stage: Stage,
    pub status: Status,
    #[serde(default)]
    pub message: String,
    /// 0.0 ..= 1.0
    #[serde(default)]
    pub progress: f32,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Research {
    /// 3-6 sentence briefing in plain words.
    pub summary: String,
    pub facts: Vec<Fact>,
    pub sources: Vec<SourceRef>,
    /// Surprising angles worth hooking on.
    #[serde(default)]
    pub angles: Vec<String>,
    /// Search keywords (people, products, events) for visuals.
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fact {
    pub text: String,
    #[serde(default)]
    pub source_url: Option<String>,
    #[serde(default)]
    pub source_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRef {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookOption {
    pub text: String,
    #[serde(default)]
    pub angle: String,
    /// 0-10 self-assessed scroll-stopping power.
    #[serde(default)]
    pub score: f32,
    #[serde(default)]
    pub why: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Script {
    /// Final narration (hook included), after the unslop pass.
    pub text: String,
    /// Narration split into sentences, in order. Joined they equal `text`.
    pub sentences: Vec<String>,
    /// Pre-unslop draft, kept for comparison.
    #[serde(default)]
    pub draft: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub hashtags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualKind {
    /// 1-3s cut from a YouTube video, found via captions.
    Youtube,
    /// Screenshot of a news/article page, highlighted quote, scroll motion.
    Article,
    /// Meme template with our own caption; used for jokes.
    Meme,
    /// Reaction GIF (GIPHY/Tenor, rendered as mp4).
    Gif,
    /// Photo of a person/product/place from image search.
    Image,
    /// Stock footage + on-screen text; the rare last resort.
    Stock,
    /// Kinetic typography + icon on a background. Always succeeds.
    Text,
}

impl VisualKind {
    /// Default fallback chain when the planned kind finds nothing.
    pub fn fallbacks(self) -> &'static [VisualKind] {
        use VisualKind::*;
        match self {
            Youtube => &[Gif, Image, Stock, Text],
            Article => &[Image, Text],
            Meme => &[Gif, Text],
            Gif => &[Meme, Youtube, Text],
            Image => &[Youtube, Stock, Text],
            Stock => &[Image, Text],
            Text => &[],
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemePlan {
    /// Imgflip template name the planner wants, e.g. "Drake Hotline Bling".
    pub template: String,
    /// Modern caption-above-image text (preferred).
    #[serde(default)]
    pub caption: String,
    #[serde(default)]
    pub top_text: String,
    #[serde(default)]
    pub bottom_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualPlan {
    pub kind: VisualKind,
    /// Primary search query for the kind's provider.
    pub query: String,
    #[serde(default)]
    pub alt_queries: Vec<String>,
    /// Short punchy overlay (<= 5 words). None = captions only.
    #[serde(default)]
    pub on_screen_text: Option<String>,
    /// Iconify id ("mdi:fire") or a plain search word ("fire").
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub meme: Option<MemePlan>,
    /// Exact phrase to find: spoken line in a YouTube video's captions, or the
    /// sentence to highlight on an article page.
    #[serde(default)]
    pub quote: Option<String>,
    /// Override the default sound for this cut.
    #[serde(default)]
    pub sfx: Option<SfxKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BeatStatus {
    #[default]
    Pending,
    Searching,
    Ready,
    /// Planned kind failed; a fallback kind was used.
    Fallback,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Beat {
    pub id: String,
    /// Exact narration words this visual covers. Beats in order, joined with
    /// spaces, reproduce the script.
    pub text: String,
    pub visual: VisualPlan,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub chosen: Option<usize>,
    #[serde(default)]
    pub asset: Option<Asset>,
    #[serde(default)]
    pub status: BeatStatus,
    #[serde(default)]
    pub error: Option<String>,
    /// The user picked this asset by hand; automation must not replace it.
    #[serde(default)]
    pub locked: bool,
    /// Seconds in the voice track; filled by the voice stage.
    #[serde(default)]
    pub start_s: f32,
    #[serde(default)]
    pub end_s: f32,
}

/// A search hit that can become an asset. Cheap to list, fetched on demand.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candidate {
    pub kind: VisualKind,
    /// e.g. "youtube", "giphy", "tenor", "pexels", "pixabay", "searxng",
    /// "wikimedia", "openverse", "imgflip", "article", "text".
    pub provider: String,
    pub title: String,
    /// Human page (YouTube watch URL, article URL, GIPHY page...).
    pub page_url: String,
    /// Direct media URL when the provider gives one.
    #[serde(default)]
    pub media_url: Option<String>,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub duration_s: Option<f32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    /// Chosen moment inside a long video (YouTube).
    #[serde(default)]
    pub start_s: Option<f32>,
    #[serde(default)]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Video,
    Image,
    Svg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub kind: AssetKind,
    /// Relative to the project directory, e.g. `assets/b03.mp4`.
    pub path: String,
    pub provider: String,
    pub source_url: String,
    /// Line for credits.txt.
    pub credit: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub duration_s: Option<f32>,
    /// Where playback starts inside `path`.
    #[serde(default)]
    pub trim_start_s: f32,
    /// Region of interest, normalized to the asset (article quote highlight,
    /// a face). The renderer pans/zooms toward it.
    #[serde(default)]
    pub focus: Option<RectF>,
}

/// One caption cue from a video's subtitles (used to find the exact moment).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptionCue {
    pub start_s: f32,
    pub end_s: f32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordTiming {
    pub text: String,
    pub start_s: f32,
    pub end_s: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceTrack {
    /// Relative to the project dir, e.g. `voice/voice.wav`.
    pub path: String,
    pub duration_s: f32,
    pub sample_rate: u32,
    pub engine: String,
    pub voice: String,
    pub words: Vec<WordTiming>,
    pub sentences: Vec<WordTiming>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderOutput {
    /// Relative to the project dir, e.g. `render/final.mp4`.
    pub path: String,
    pub credits_path: String,
    pub duration_s: f32,
    pub width: u32,
    pub height: u32,
    pub rendered_at: DateTime<Utc>,
    /// The same edit in the project's `extra_formats`.
    #[serde(default)]
    pub variants: Vec<RenderVariant>,
    #[serde(default)]
    pub subtitles: Option<Subtitles>,
    /// Relative path of the YouTube-sized thumbnail (1280x720 JPEG).
    #[serde(default)]
    pub thumbnail_jpg: Option<String>,
}

impl RenderOutput {
    /// The rendered file for `format`, if any.
    pub fn file_for(&self, main: Format, format: Format) -> Option<(&str, u32, u32)> {
        if format == main {
            return Some((&self.path, self.width, self.height));
        }
        self.variants.iter().find(|v| v.format == format).map(|v| (v.path.as_str(), v.width, v.height))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderVariant {
    pub format: Format,
    /// Relative to the project dir, e.g. `render/final-square.mp4`.
    pub path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtitles {
    /// Relative paths, e.g. `render/captions.srt`.
    pub srt: String,
    pub vtt: String,
}

impl Project {
    pub fn new(topic: impl Into<String>, notes: impl Into<String>, format: Format, target_seconds: u32) -> Self {
        let now = Utc::now();
        let id = format!("{}-{}", now.format("%Y%m%d-%H%M%S"), &uuid::Uuid::new_v4().simple().to_string()[..6]);
        Self {
            id,
            created_at: now,
            updated_at: now,
            topic: topic.into(),
            notes: notes.into(),
            format,
            target_seconds,
            voice: None,
            autopilot: true,
            stages: Stage::ALL
                .iter()
                .map(|&stage| StageState {
                    stage,
                    status: Status::Pending,
                    message: String::new(),
                    progress: 0.0,
                    error: None,
                    started_at: None,
                    finished_at: None,
                })
                .collect(),
            research: None,
            hooks: Vec::new(),
            chosen_hook: None,
            script: None,
            beats: Vec::new(),
            voice_track: None,
            render: None,
            caption_preset: None,
            extra_formats: Vec::new(),
            published: Vec::new(),
            kind: ProjectKind::Story,
            clip: None,
        }
    }

    /// Every format this project renders, main first, without duplicates.
    pub fn formats(&self) -> Vec<Format> {
        let mut out = vec![self.format];
        for &f in &self.extra_formats {
            if !out.contains(&f) {
                out.push(f);
            }
        }
        out
    }

    pub fn stage_mut(&mut self, stage: Stage) -> &mut StageState {
        if let Some(i) = self.stages.iter().position(|s| s.stage == stage) {
            return &mut self.stages[i];
        }
        self.stages.push(StageState {
            stage,
            status: Status::Pending,
            message: String::new(),
            progress: 0.0,
            error: None,
            started_at: None,
            finished_at: None,
        });
        self.stages.last_mut().unwrap()
    }

    pub fn chosen_hook_text(&self) -> Option<&str> {
        self.chosen_hook.and_then(|i| self.hooks.get(i)).map(|h| h.text.as_str())
    }

    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(dir.join("project.json"))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&mut self, dir: &Path) -> anyhow::Result<()> {
        self.updated_at = Utc::now();
        std::fs::create_dir_all(dir)?;
        write_atomic(&dir.join("project.json"), serde_json::to_string_pretty(self)?.as_bytes())
    }
}

/// Write via a temp file + rename so a crash never leaves half a JSON file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    crate::atomic::write(path, bytes, false)
}

/// `write_atomic` for secrets: the file is readable by its owner only.
pub fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    crate::atomic::write(path, bytes, true)
}

fn normalize_word(word: &str) -> String {
    word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Map each beat's text onto the voice track's word timings, in order, and
/// set `start_s`/`end_s`. Beats are contiguous: each starts where the
/// previous ended, so the picture never goes blank between cuts. Unmatched
/// text falls back to sharing the remaining time. Every bound is finite and
/// inside the duration; invalid/nonpositive durations produce zero bounds.
pub fn align_beats(beats: &mut [Beat], words: &[WordTiming], total_s: f32) {
    let total_s = if total_s.is_finite() && total_s > 0.0 { total_s } else { 0.0 };
    if beats.is_empty() {
        return;
    }
    let spoken: Vec<_> = words
        .iter()
        .filter(|w| w.start_s.is_finite() && w.end_s.is_finite() && w.end_s >= w.start_s)
        .filter_map(|w| {
            let text = normalize_word(&w.text);
            (!text.is_empty()).then_some((text, w))
        })
        .collect();
    let mut cursor = 0usize;
    let mut prev_end = 0.0f32;
    let count = beats.len();
    let duration = f64::from(total_s);
    // Preserve a useful cut length when possible, but never demand 0.2s from
    // every beat when the entire voice track is shorter than that budget.
    let min_span = (duration / count as f64).min(0.2);
    for (i, beat) in beats.iter_mut().enumerate() {
        let beat_words: Vec<String> =
            beat.text.split_whitespace().map(normalize_word).filter(|w| !w.is_empty()).collect();
        // Advance one spoken word per beat word, re-syncing on exact matches
        // within a small window to survive TTS number/abbreviation expansion.
        let mut end_idx = cursor;
        let mut matched = false;
        for bw in &beat_words {
            let window_end = end_idx.saturating_add(4).min(spoken.len());
            if let Some(off) = spoken[end_idx..window_end].iter().position(|(s, _)| s == bw) {
                end_idx += off + 1;
                matched = true;
            } else if end_idx < spoken.len() {
                end_idx += 1;
            }
        }
        beat.start_s = prev_end;
        beat.end_s = if i + 1 == count {
            total_s
        } else {
            let remaining = (count - i) as f64;
            let fallback = f64::from(prev_end) + (duration - f64::from(prev_end)) / remaining;
            // Cut halfway into the pause after the beat's last word.
            let candidate = if matched && end_idx > cursor {
                let last = end_idx - 1;
                let this_end = f64::from(spoken[last].1.end_s);
                let next_start = spoken.get(last + 1).map_or(this_end, |(_, w)| f64::from(w.start_s));
                let midpoint = (this_end + next_start) / 2.0;
                if midpoint > f64::from(prev_end) { midpoint } else { fallback }
            } else {
                fallback
            };
            let upper = (duration - min_span * (remaining - 1.0)).max(f64::from(prev_end));
            let lower = (f64::from(prev_end) + min_span).min(upper);
            // f64 arithmetic avoids midpoint overflow and accumulated drift.
            // Clamp again after rounding to f32 to preserve exact contiguity.
            (candidate.clamp(lower, upper) as f32).clamp(prev_end, total_s)
        };
        prev_end = beat.end_s;
        cursor = end_idx;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beat(text: &str) -> Beat {
        Beat {
            id: text.into(),
            text: text.into(),
            visual: VisualPlan {
                kind: VisualKind::Text,
                query: String::new(),
                alt_queries: vec![],
                on_screen_text: None,
                icon: None,
                meme: None,
                quote: None,
                sfx: None,
            },
            candidates: vec![],
            chosen: None,
            asset: None,
            status: BeatStatus::Pending,
            error: None,
            locked: false,
            start_s: 0.0,
            end_s: 0.0,
        }
    }

    fn assert_bounds(beats: &[Beat], duration: f32) {
        let mut end = 0.0;
        for beat in beats {
            assert!(beat.start_s.is_finite());
            assert!(beat.end_s.is_finite());
            assert_eq!(beat.start_s, end);
            assert!(beat.start_s >= 0.0);
            assert!(beat.end_s >= beat.start_s, "{beat:?}");
            assert!(beat.end_s <= duration, "{beat:?}");
            end = beat.end_s;
        }
        if !beats.is_empty() {
            assert_eq!(end, duration);
        }
    }

    fn word(text: &str, start_s: f32, end_s: f32) -> WordTiming {
        WordTiming { text: text.into(), start_s, end_s }
    }

    #[test]
    fn aligns_contiguously() {
        let words: Vec<WordTiming> = ["Coffee", "is", "a", "fruit.", "Seriously,", "a", "berry."]
            .iter()
            .enumerate()
            .map(|(i, w)| WordTiming { text: w.to_string(), start_s: i as f32 * 0.4, end_s: i as f32 * 0.4 + 0.3 })
            .collect();
        let mut beats = vec![beat("Coffee is a fruit."), beat("Seriously, a berry.")];
        align_beats(&mut beats, &words, 3.0);
        assert_eq!(beats[0].start_s, 0.0);
        // "fruit." ends at 1.5 and "Seriously," starts at 1.6: cut mid-pause.
        assert!((beats[0].end_s - 1.55).abs() < 0.01, "{}", beats[0].end_s);
        assert_eq!(beats[1].start_s, beats[0].end_s);
        assert_eq!(beats[1].end_s, 3.0);
        assert_bounds(&beats, 3.0);
    }

    #[test]
    fn missing_words_share_remaining_duration() {
        let mut beats = vec![beat("one"), beat("two"), beat("three")];
        align_beats(&mut beats, &[], 4.5);
        assert_bounds(&beats, 4.5);
        assert_eq!(beats.iter().map(|b| b.end_s).collect::<Vec<_>>(), [1.5, 3.0, 4.5]);
    }

    #[test]
    fn unrelated_word_timings_use_fallback_without_stalling() {
        let mut beats = vec![beat("one"), beat("two"), beat("three"), beat("four")];
        let words = [word("different", 0.0, 0.1), word("narration", 0.1, 0.2)];
        align_beats(&mut beats, &words, 2.0);
        assert_bounds(&beats, 2.0);
        assert_eq!(beats.iter().map(|b| b.end_s).collect::<Vec<_>>(), [0.5, 1.0, 1.5, 2.0]);
    }

    #[test]
    fn resyncs_after_number_expansion() {
        let words = [
            word("I", 0.0, 0.3),
            word("paid", 0.4, 0.7),
            word("forty", 0.8, 1.1),
            word("two", 1.2, 1.5),
            word("dollars", 1.6, 1.8),
            word("today", 2.0, 2.3),
        ];
        let mut beats = vec![beat("I paid 42 dollars."), beat("Today.")];
        align_beats(&mut beats, &words, 2.5);
        assert_bounds(&beats, 2.5);
        assert!((beats[0].end_s - 1.9).abs() < 0.001);
    }

    #[test]
    fn short_audio_and_many_beats_never_overshoot_or_reverse() {
        for duration in [0.001, 0.03, 0.1, 1.0] {
            let mut beats: Vec<_> = (0..100).map(|_| beat("one")).collect();
            align_beats(&mut beats, &[word("one", 0.0, 10.0)], duration);
            assert_bounds(&beats, duration);
            assert!(beats.iter().all(|b| b.end_s > b.start_s));
        }
    }

    #[test]
    fn exhausted_word_timings_leave_time_for_every_remaining_beat() {
        let mut beats: Vec<_> = (0..12).map(|_| beat("one")).collect();
        align_beats(&mut beats, &[word("one", 0.0, 4.0)], 4.0);
        assert_bounds(&beats, 4.0);
        assert!(beats.iter().all(|b| b.end_s > b.start_s));
    }

    #[test]
    fn empty_and_punctuation_only_beats_stay_contiguous() {
        let mut beats = vec![beat(""), beat("..."), beat("one"), beat(""), beat("two")];
        let words = [word("...", 0.0, 0.1), word("one", 0.1, 0.5), word("two", 0.7, 1.0)];
        align_beats(&mut beats, &words, 1.0);
        assert_bounds(&beats, 1.0);
        assert!(beats.iter().all(|b| b.end_s > b.start_s));
    }

    #[test]
    fn malformed_timings_cannot_escape_voice_duration() {
        let words = [
            word("one", f32::NAN, 0.5),
            word("two", 0.1, f32::INFINITY),
            word("three", 1.0, -1.0),
            word("four", -3.0, -2.0),
            word("five", 1.0, 100.0),
            word("six", 0.0, 0.1),
            word("seven", f32::INFINITY, f32::INFINITY),
        ];
        let mut beats = vec![beat("one two"), beat("three four"), beat("five"), beat("six seven")];
        align_beats(&mut beats, &words, 2.0);
        assert_bounds(&beats, 2.0);
        assert!(beats.iter().all(|b| b.end_s > b.start_s));
    }

    #[test]
    fn nonpositive_and_nonfinite_durations_produce_zero_bounds() {
        for duration in [0.0, -0.0, -1.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut beats = vec![beat("one"), beat("two")];
            align_beats(&mut beats, &[word("one", 0.0, 0.3)], duration);
            assert_bounds(&beats, 0.0);
        }
    }

    #[test]
    fn single_or_missing_beats_are_safe() {
        align_beats(&mut [], &[], f32::NAN);
        let mut beats = vec![beat("all narration")];
        align_beats(&mut beats, &[], 0.01);
        assert_bounds(&beats, 0.01);
    }

    #[test]
    fn very_large_finite_timings_do_not_overflow_midpoints() {
        let words = [word("one", f32::MAX / 2.0, f32::MAX * 0.7), word("two", f32::MAX * 0.8, f32::MAX)];
        let mut beats = vec![beat("one"), beat("two")];
        align_beats(&mut beats, &words, f32::MAX);
        assert_bounds(&beats, f32::MAX);
        assert!(beats[0].end_s < f32::MAX);
    }

    #[test]
    fn repeated_alignment_is_deterministic() {
        let words = [word("one", 0.0, 0.4), word("two", 0.5, 0.8)];
        let mut beats: Vec<_> = (0..30).map(|_| beat("missing")).collect();
        align_beats(&mut beats, &words, 0.3);
        let original: Vec<_> = beats.iter().map(|b| (b.start_s, b.end_s)).collect();
        align_beats(&mut beats, &words, 0.3);
        assert_bounds(&beats, 0.3);
        assert_eq!(beats.iter().map(|b| (b.start_s, b.end_s)).collect::<Vec<_>>(), original);
    }
}

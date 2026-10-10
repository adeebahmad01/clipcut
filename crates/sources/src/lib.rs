//! Finding and fetching visuals: SearXNG/DuckDuckGo web search, YouTube via
//! yt-dlp (captions + exact 1-3 s sections, 5-180 s clips with audio), article
//! capture with headless Chromium, GIPHY/Tenor GIFs, Imgflip meme templates,
//! Pexels/Pixabay stock, Wikimedia/Openverse images and Iconify icons.
//! Topic ideas come from Google Trends RSS or news search.
//!
//! PUBLIC API CONTRACT — the app crate codes against these signatures.

use std::path::{Path, PathBuf};

use clipcut_core::{Asset, CaptionCue, Candidate, Config, KeyPool, Paths, Tools, VisualKind, VisualPlan};
use serde::{Deserialize, Serialize};

mod article;
mod candidates;
mod captions;
mod health;
mod icons;
mod ideas;
mod media;
mod providers;
mod provenance;
mod search;
mod svg;
pub mod setup;
mod util;
mod youtube;

pub use article::shutdown_browser;
pub use captions::{best_moment, parse_vtt};
pub use ideas::{Idea, IdeaSource, parse_trends_rss};
pub use providers::rank_memes;
pub use search::{parse_ddg_html, unwrap_ddg_url};
pub use youtube::video_id;

#[derive(Clone)]
pub struct Sources {
    pub config: Config,
    pub keys: KeyPool,
    pub paths: Paths,
    pub tools: Tools,
    pub http: reqwest::Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchCategory {
    General,
    News,
    Images,
    Videos,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
    /// Full image (image results).
    #[serde(default)]
    pub image_url: Option<String>,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default)]
    pub published: Option<String>,
    /// Which backend answered: searxng, duckduckgo, brave, tavily, ...
    pub engine: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemeTemplate {
    pub id: String,
    pub name: String,
    pub url: String,
    pub width: u32,
    pub height: u32,
    pub box_count: u32,
}

/// Metadata of one YouTube video (`Sources::youtube_info`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoInfo {
    pub id: String,
    /// https://www.youtube.com/watch?v=ID
    pub url: String,
    pub title: String,
    pub channel: String,
    pub duration_s: f32,
}

/// Health of each backend for the doctor page.
#[derive(Debug, Clone, Serialize)]
pub struct SourceHealth {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

impl Sources {
    pub fn new(config: Config, keys: KeyPool, paths: Paths, tools: Tools) -> Self {
        Self { config, keys, paths, tools, http: clipcut_core::http::client() }
    }

    /// Free-first search: SearXNG, DuckDuckGo, then optional keyed fallbacks.
    pub async fn web_search(&self, query: &str, category: SearchCategory, limit: usize) -> anyhow::Result<Vec<SearchHit>> {
        self.web_search_impl(query, category, limit).await
    }

    /// List candidates for one beat of the given kind (no downloads).
    /// `Text` returns an empty list. Never errors just because a provider
    /// is missing a key — that provider is skipped.
    pub async fn candidates(&self, kind: VisualKind, plan: &VisualPlan, limit: usize) -> anyhow::Result<Vec<Candidate>> {
        self.candidates_impl(kind, plan, limit).await
    }

    /// English captions of a YouTube video as cues (auto-subs accepted).
    pub async fn youtube_captions(&self, video_url: &str) -> anyhow::Result<Vec<CaptionCue>> {
        self.youtube_captions_impl(video_url).await
    }

    /// Cheap keyword-overlap moment finder, used when the LLM pick is skipped.
    pub fn best_moment_heuristic(&self, cues: &[CaptionCue], beat_text: &str, quote: Option<&str>, clip_seconds: f32) -> Option<(f32, f32)> {
        captions::best_moment(cues, beat_text, quote, clip_seconds)
    }

    /// Download a candidate into `dest_dir` as `<stem>.<ext>`, normalizing:
    /// GIFs/webm → H.264 mp4, YouTube → only the `window` section (start,end s),
    /// articles → viewport PNG screenshot (NOT a scrolling recording) with
    /// `quote` highlighted and `Asset::focus` set when found.
    /// A `<stem>.source.json` sidecar retains provenance and rights warnings.
    pub async fn fetch(
        &self,
        candidate: &Candidate,
        dest_dir: &Path,
        stem: &str,
        window: Option<(f32, f32)>,
        quote: Option<&str>,
    ) -> anyhow::Result<Asset> {
        util::validate_stem(stem)?;
        anyhow::ensure!(util::http_url(&candidate.page_url), "candidate source must be an HTTP(S) URL");
        let result = match candidate.kind {
            VisualKind::Youtube => self.fetch_youtube(candidate, dest_dir, stem, window).await,
            VisualKind::Article => self.fetch_article(candidate, dest_dir, stem, quote).await,
            VisualKind::Gif => self.fetch_gif(candidate, dest_dir, stem).await,
            VisualKind::Stock => self.fetch_stock(candidate, dest_dir, stem).await,
            VisualKind::Image | VisualKind::Meme => self.fetch_image(candidate, dest_dir, stem).await,
            VisualKind::Text => anyhow::bail!("text beats have nothing to fetch"),
        };
        if result.is_err() { util::clear_stem(dest_dir, stem); }
        result
    }

    /// Title, channel and duration of a YouTube video (no download). Rejects
    /// live/upcoming streams and videos over 4 h.
    pub async fn youtube_info(&self, url: &str) -> anyhow::Result<VideoInfo> {
        self.youtube_info_impl(url).await
    }

    /// "Clip from a long video": download `start_s..end_s` (5-180 s) of `info`
    /// WITH its audio as an H.264/AAC mp4 at `dest_dir/<stem>.mp4`, plus the
    /// `<stem>.source.json` provenance sidecar.
    pub async fn fetch_youtube_segment(
        &self,
        info: &VideoInfo,
        start_s: f32,
        end_s: f32,
        dest_dir: &Path,
        stem: &str,
    ) -> anyhow::Result<Asset> {
        util::validate_stem(stem)?;
        let result = self.fetch_youtube_segment_impl(info, start_s, end_s, dest_dir, stem).await;
        if result.is_err() { util::clear_stem(dest_dir, stem); }
        result
    }

    /// Topic ideas: Google Trends daily searches for `geo` (2-letter country,
    /// default US; cached 30 min) or news headlines for `query` (default
    /// "top news today"). `limit` is clamped to 1-50.
    pub async fn ideas(&self, source: IdeaSource, query: Option<&str>, geo: &str, limit: usize) -> anyhow::Result<Vec<Idea>> {
        self.ideas_impl(source, query, geo, limit).await
    }

    /// Imgflip's popular templates (cached for a day).
    pub async fn meme_templates(&self) -> anyhow::Result<Vec<MemeTemplate>> {
        self.meme_templates_impl().await
    }

    /// Resolve an Iconify id ("mdi:fire") or search word ("fire") to a cached
    /// SVG file. None when nothing matches.
    pub async fn icon_svg(&self, name_or_query: &str) -> anyhow::Result<Option<PathBuf>> {
        self.icon_svg_impl(name_or_query).await
    }

    /// Probe each backend (SearXNG reachable, keys present, yt-dlp works...).
    pub async fn health(&self) -> Vec<SourceHealth> {
        self.health_impl().await
    }
}

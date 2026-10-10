//! YouTube through yt-dlp: flat search, captions, exact section downloads.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clipcut_core::{Asset, AssetKind, CaptionCue, Candidate, Tools, VisualKind};
use serde_json::{Value, json};

use crate::{Sources, VideoInfo};
use crate::captions::parse_vtt;
use crate::util::{clean_text, clear_stem, http_url, read_fresh, relative_to_project, run, with_ext};
use crate::media::{MAX_SEGMENT_SECONDS, credit_for};

/// Extract the 11-char video id from any YouTube URL form.
pub fn video_id(url: &str) -> Option<String> {
    let valid = |s: &str| s.len() == 11 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid(url) {
        return Some(url.to_string());
    }
    let parsed = url::Url::parse(url).ok()?;
    if !http_url(url) { return None; }
    let host = parsed.host_str()?.trim_start_matches("www.").trim_start_matches("m.");
    if host == "youtu.be" {
        return parsed.path_segments()?.next().filter(|s| valid(s)).map(str::to_string);
    }
    if !["youtube.com", "youtube-nocookie.com"].iter().any(|h| host == *h || host.ends_with(&format!(".{h}"))) {
        return None;
    }
    if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "v") {
        return valid(&v).then(|| v.into_owned());
    }
    let segs: Vec<&str> = parsed.path_segments()?.collect();
    match segs.as_slice() {
        ["shorts" | "embed" | "live" | "v", id, ..] if valid(id) => Some(id.to_string()),
        _ => None,
    }
}

impl Sources {
    pub(crate) fn yt_dlp(&self) -> anyhow::Result<&Path> {
        Tools::require(&self.tools.yt_dlp, "yt-dlp")
    }

    /// Flags every yt-dlp call needs: JS runtime for YouTube's player
    /// challenges (deno is yt-dlp's default; node must be enabled), cookies.
    pub(crate) fn yt_common_args(&self) -> Vec<String> {
        let mut args: Vec<String> = ["--no-warnings", "--ignore-config", "--no-plugin-dirs",
            "--socket-timeout", "10", "--retries", "1", "--fragment-retries", "1",
            "--extractor-retries", "1", "--file-access-retries", "1", "--no-progress"]
            .map(str::to_string).into();
        if self.tools.deno.is_none() {
            if let Some(node) = &self.tools.node {
                args.push("--js-runtimes".into());
                args.push(format!("node:{}", node.display()));
            }
        }
        if let Some(browser) = self.config.youtube.cookies_from_browser.as_deref().filter(|b| !b.trim().is_empty()) {
            args.push("--cookies-from-browser".into());
            args.push(browser.trim().to_string());
        }
        args
    }

    pub(crate) async fn youtube_search(&self, query: &str, n: usize) -> anyhow::Result<Vec<Candidate>> {
        if n == 0 || query.trim().is_empty() { return Ok(Vec::new()); }
        let yt = self.yt_dlp()?;
        let search = format!("ytsearch{}:{}", n.clamp(1, 30), query);
        let mut args = self.yt_common_args();
        args.extend(["--flat-playlist".into(), "--dump-json".into(), search]);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(yt, &argv, None, Duration::from_secs(60)).await?;
        let max_s = self.config.youtube.max_video_minutes as f32 * 60.0;
        let block: Vec<String> = self.config.youtube.channel_blocklist.iter().map(|c| c.trim().to_lowercase()).collect();
        let mut out_c = Vec::new();
        for line in out.stdout.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let Some(id) = v["id"].as_str() else { continue };
            if video_id(id).as_deref() != Some(id) { continue; }
            let live = v["live_status"].as_str().is_some_and(|s| s == "is_live" || s == "is_upcoming" || s == "post_live")
                || v["is_live"].as_bool() == Some(true);
            let duration = v["duration"].as_f64().map(|d| d as f32);
            if live || duration.is_none_or(|d| !d.is_finite() || d <= 0.0 || (max_s > 0.0 && d > max_s)) {
                continue;
            }
            let channel = v["channel"].as_str().or(v["uploader"].as_str()).unwrap_or_default().to_string();
            let channel_id = v["channel_id"].as_str().unwrap_or_default().to_string();
            if block.iter().any(|b| !b.is_empty() && (*b == channel.to_lowercase() || *b == channel_id.to_lowercase())) {
                continue;
            }
            let thumb = v["thumbnails"]
                .as_array()
                .and_then(|t| t.iter().max_by_key(|t| t["width"].as_u64().unwrap_or(0)))
                .and_then(|t| t["url"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg"));
            let is_short = v["url"].as_str().is_some_and(|u| u.contains("/shorts/"));
            out_c.push(Candidate {
                kind: VisualKind::Youtube,
                provider: "youtube".into(),
                title: v["title"].as_str().unwrap_or_default().to_string(),
                page_url: format!("https://www.youtube.com/watch?v={id}"),
                media_url: None,
                thumbnail_url: Some(thumb),
                duration_s: duration,
                width: None,
                height: None,
                start_s: None,
                extra: serde_json::json!({
                    "id": id,
                    "channel": channel,
                    "channel_id": channel_id,
                    "view_count": v["view_count"],
                    "is_short": is_short,
                    // Flat search does not establish the video's license.
                    "license_status": "unknown_requires_review",
                }),
            });
        }
        Ok(out_c)
    }

    pub(crate) async fn youtube_captions_impl(&self, video_url: &str) -> anyhow::Result<Vec<CaptionCue>> {
        let id = video_id(video_url).ok_or_else(|| anyhow::anyhow!("not a YouTube URL: {video_url}"))?;
        let dir = self.paths.cache().join("captions").join(&id);
        let cache = dir.join("cues.json");
        if let Some(text) = read_fresh(&cache, Duration::from_secs(7 * 24 * 3600)) {
            if let Ok(cues) = serde_json::from_str::<Vec<CaptionCue>>(&text) {
                if !cues.is_empty() && cues.iter().all(valid_cue) { return Ok(cues); }
            }
        }
        let yt = self.yt_dlp()?;
        tokio::fs::create_dir_all(&dir).await?;
        // Isolated per attempt: stale files / concurrent requests cannot be
        // mistaken for freshly fetched captions.
        let work = dir.join(format!("attempt-{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&work).await?;
        let _cleanup = CaptionWork(work.clone());
        let template = work.join("%(id)s.%(ext)s").to_string_lossy().to_string();
        let url = format!("https://www.youtube.com/watch?v={id}");
        let mut args = self.yt_common_args();
        args.extend(
            [
                "--skip-download",
                "--write-subs",
                "--write-auto-subs",
                "--sub-langs",
                "en,en-orig",
                "--sub-format",
                "vtt/best",
                "--convert-subs",
                "vtt",
                "--no-playlist",
                "-o",
                &template,
                &url,
            ]
            .map(String::from),
        );
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        run(yt, &argv, None, Duration::from_secs(60)).await?;
        let mut files: Vec<PathBuf> = std::fs::read_dir(&work)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "vtt"))
            .collect();
        // Prefer plain "en", then regional English, then the ASR original.
        let rank = |p: &PathBuf| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let lang = name.trim_end_matches(".vtt").rsplit('.').next().unwrap_or_default().to_string();
            match lang.as_str() {
                "en" => 0,
                "en-US" | "en-GB" | "en-CA" | "en-AU" => 1,
                "en-orig" => 2,
                _ => 3,
            }
        };
        files.sort_by_key(rank);
        let mut cues = Vec::new();
        for f in &files {
            if std::fs::metadata(f)?.len() > 4 * 1024 * 1024 { continue; }
            let text = std::fs::read_to_string(f).unwrap_or_default();
            cues = parse_vtt(&text);
            if !cues.is_empty() {
                break;
            }
        }
        for f in files {
            let _ = std::fs::remove_file(f);
        }
        if !cues.is_empty() {
            clipcut_core::model::write_atomic(&cache, serde_json::to_string(&cues)?.as_bytes())?;
        }
        Ok(cues)
    }

    pub(crate) async fn fetch_youtube(
        &self,
        c: &Candidate,
        dest_dir: &Path,
        stem: &str,
        window: Option<(f32, f32)>,
    ) -> anyhow::Result<Asset> {
        self.yt_dlp()?;
        let id = video_id(&c.page_url).ok_or_else(|| anyhow::anyhow!("not a YouTube URL: {}", c.page_url))?;
        let (a, b) = window.unwrap_or_else(|| {
            let s = c.start_s.unwrap_or(0.0);
            let d = self.config.video.max_clip_seconds;
            (s, s + if d.is_finite() { d.clamp(1.0, 3.0) } else { 3.0 })
        });
        validate_window(a, b)?;
        if let Some(d) = c.duration_s.filter(|d| d.is_finite() && *d > 0.0) {
            anyhow::ensure!(b <= d + 0.001, "window {a}-{b} exceeds source duration {d}");
        }
        let ffmpeg = Tools::require(&self.tools.ffmpeg, "ffmpeg")?;
        Tools::require(&self.tools.ffprobe, "ffprobe")?;
        let url = format!("https://www.youtube.com/watch?v={id}");
        let path = self.download_section(&id, (a, b), dest_dir, stem, 40, Duration::from_secs(120)).await?;
        let downloaded = self.validate_video(&path).await?;
        let duration = b - a;
        // Never accept a full download, padding or a truncated near-end section.
        anyhow::ensure!((downloaded.duration_s - duration).abs() <= 0.25,
            "yt-dlp produced {}s instead of requested {duration}s section", downloaded.duration_s);
        let final_path = with_ext(dest_dir, stem, "mp4");
        let length = format!("{duration:.6}");
        run(ffmpeg, &["-nostdin", "-y", "-v", "error", "-xerror", "-i", &path.to_string_lossy(),
            "-map", "0:v:0", "-map", "0:a:0?", "-t", &length, "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p", "-r", "30",
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-threads", "2",
            "-c:a", "aac", "-movflags", "+faststart", &final_path.to_string_lossy()],
            None, Duration::from_secs(30)).await?;
        let probe = self.validate_video(&final_path).await?;
        anyhow::ensure!((probe.duration_s - duration).abs() <= 0.045, "section length is not frame-accurate");
        let _ = tokio::fs::remove_file(&path).await;
        let asset = Asset {
            kind: AssetKind::Video,
            path: relative_to_project(dest_dir, &final_path),
            provider: "youtube".into(),
            source_url: format!("{url}&t={}s", a.floor() as u32),
            credit: credit_for(c),
            width: probe.width,
            height: probe.height,
            duration_s: Some(probe.duration_s),
            trim_start_s: 0.0,
            focus: None,
        };
        crate::provenance::record(c, &asset, &final_path, None, serde_json::json!({
            "mode": "youtube_exact_section", "start_s": a, "end_s": b,
            "padding_s": 0, "duration_tolerance_s": 0.045,
        }))?;
        Ok(asset)
    }

    /// yt-dlp download of exactly `*a-b` into `dest_dir/<stem>.yt.<ext>`, at most
    /// `max_mib` MiB. Clears earlier `<stem>.*` files first.
    async fn download_section(
        &self, id: &str, (a, b): (f32, f32), dest_dir: &Path, stem: &str, max_mib: u64, timeout: Duration,
    ) -> anyhow::Result<PathBuf> {
        let yt = self.yt_dlp()?;
        tokio::fs::create_dir_all(dest_dir).await?;
        clear_stem(dest_dir, stem);
        let template = dest_dir.join(format!("{stem}.yt.%(ext)s")).to_string_lossy().to_string();
        let section = format!("*{a:.3}-{b:.3}");
        let url = format!("https://www.youtube.com/watch?v={id}");
        let max_bytes = max_mib * 1024 * 1024;
        let output_args = format!("ffmpeg_o:-fs {max_bytes} -threads 2");
        let mut args = self.yt_common_args();
        if let Some(ffmpeg) = &self.tools.ffmpeg {
            args.push("--ffmpeg-location".into());
            args.push(ffmpeg.to_string_lossy().to_string());
        }
        args.extend(
            [
                "--download-sections",
                &section,
                "--force-keyframes-at-cuts",
                "--format",
                "bv*[height<=1080][width<=1920][vcodec^=avc1]+ba[ext=m4a]/bv*[height<=1080][width<=1920]+ba/b[height<=1080][width<=1920]",
                "--downloader-args",
                "ffmpeg_i:-rw_timeout 10000000",
                "--downloader-args",
                &output_args,
                "--match-filter",
                "!is_live",
                "--merge-output-format",
                "mp4",
                "--print",
                "after_move:filepath",
                "--no-playlist",
                "--no-part",
                "-o",
                &template,
                &url,
            ]
            .map(String::from),
        );
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(yt, &argv, None, timeout).await?;
        let canonical_dir = dest_dir.canonicalize()?;
        let valid_output = |p: &Path| p.canonicalize().ok().is_some_and(|p| {
            p.parent() == Some(canonical_dir.as_path())
                && p.file_stem().is_some_and(|s| s == format!("{stem}.yt").as_str())
                && p.extension().is_some_and(|e| ["mp4", "webm", "mkv", "mov"].iter().any(|s| e == *s))
        });
        let path = out
            .stdout
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty() && valid_output(Path::new(l)))
            .map(PathBuf::from)
            .or_else(|| {
                std::fs::read_dir(dest_dir).ok()?.flatten().map(|e| e.path()).find(|p| {
                    valid_output(p)
                })
            })
            .ok_or_else(|| anyhow::anyhow!("yt-dlp produced no file: {}", out.stderr.trim()))?;
        anyhow::ensure!(std::fs::metadata(&path)?.len() <= max_bytes, "YouTube section exceeded {max_mib} MiB");
        Ok(path)
    }

    pub(crate) async fn youtube_info_impl(&self, url: &str) -> anyhow::Result<VideoInfo> {
        let id = video_id(url).ok_or_else(|| anyhow::anyhow!("not a YouTube URL: {}", clean_text(url, 200)))?;
        let yt = self.yt_dlp()?;
        let watch = format!("https://www.youtube.com/watch?v={id}");
        let mut args = self.yt_common_args();
        args.extend(["--skip-download", "--no-playlist", "--print", INFO_TEMPLATE, &watch].map(String::from));
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(yt, &argv, None, Duration::from_secs(60)).await?;
        let info = parse_video_info(&out.stdout)?;
        anyhow::ensure!(info.id == id, "yt-dlp described video {} instead of {id}", info.id);
        Ok(info)
    }

    /// 5-180 s section WITH audio ("clip from a long video"), H.264/AAC mp4.
    pub(crate) async fn fetch_youtube_segment_impl(
        &self, info: &VideoInfo, a: f32, b: f32, dest_dir: &Path, stem: &str,
    ) -> anyhow::Result<Asset> {
        self.yt_dlp()?;
        let id = video_id(&info.id).filter(|id| *id == info.id && video_id(&info.url).as_ref() == Some(id))
            .ok_or_else(|| anyhow::anyhow!("not a YouTube video: {} ({})", clean_text(&info.id, 40), clean_text(&info.url, 200)))?;
        validate_segment(a, b, info.duration_s)?;
        let ffmpeg = Tools::require(&self.tools.ffmpeg, "ffmpeg")?;
        Tools::require(&self.tools.ffprobe, "ffprobe")?;
        let url = format!("https://www.youtube.com/watch?v={id}");
        let path = self.download_section(&id, (a, b), dest_dir, stem, 300, Duration::from_secs(600)).await?;
        let length = b - a;
        let downloaded = self.validate_video_within(&path, MAX_SEGMENT_SECONDS).await?;
        anyhow::ensure!(downloaded.has_audio, "the section has no audio");
        anyhow::ensure!((downloaded.duration_s - length).abs() <= 0.35,
            "yt-dlp produced {}s instead of the requested {length}s section", downloaded.duration_s);
        let final_path = with_ext(dest_dir, stem, "mp4");
        run(ffmpeg, &["-nostdin", "-y", "-v", "error", "-xerror", "-i", &path.to_string_lossy(),
            "-map", "0:v:0", "-map", "0:a:0", "-t", &format!("{length:.6}"), "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p", "-r", "30",
            "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-threads", "2",
            "-c:a", "aac", "-b:a", "160k", "-ar", "48000", "-ac", "2", "-movflags", "+faststart",
            &final_path.to_string_lossy()],
            None, Duration::from_secs(600)).await?;
        let probe = self.validate_video_within(&final_path, MAX_SEGMENT_SECONDS).await?;
        anyhow::ensure!(probe.has_audio, "the section has no audio");
        anyhow::ensure!((probe.duration_s - length).abs() <= 0.1, "clip is {}s instead of the requested {length}s", probe.duration_s);
        let _ = tokio::fs::remove_file(&path).await;
        let c = Candidate {
            kind: VisualKind::Youtube,
            provider: "youtube".into(),
            title: clean_text(&info.title, 300),
            page_url: url.clone(),
            media_url: None,
            thumbnail_url: None,
            duration_s: Some(info.duration_s),
            width: None,
            height: None,
            start_s: Some(a),
            extra: json!({"id": id, "channel": clean_text(&info.channel, 300), "license_status": "unknown_requires_review"}),
        };
        let asset = Asset {
            kind: AssetKind::Video,
            path: relative_to_project(dest_dir, &final_path),
            provider: "youtube".into(),
            source_url: format!("{url}&t={}s", a.floor() as u32),
            credit: credit_for(&c),
            width: probe.width,
            height: probe.height,
            duration_s: Some(probe.duration_s),
            trim_start_s: 0.0,
            focus: None,
        };
        crate::provenance::record(&c, &asset, &final_path, None, json!({
            "mode": "youtube_clip_section", "start_s": a, "end_s": b, "duration_tolerance_s": 0.1,
        }))?;
        Ok(asset)
    }
}

/// `--print` template for `youtube_info`: one JSON object line.
const INFO_TEMPLATE: &str = "%(.{id,title,channel,uploader,duration,live_status,is_live})j";
/// Longest video `youtube_info` accepts (4 h).
const MAX_INFO_SECONDS: f32 = 14_400.0;

/// Last JSON object line of yt-dlp's `INFO_TEMPLATE` output.
fn parse_video_info(stdout: &str) -> anyhow::Result<VideoInfo> {
    let v = stdout.lines().rev().map(str::trim).filter(|l| !l.is_empty())
        .find_map(|l| serde_json::from_str::<Value>(l).ok().filter(Value::is_object))
        .ok_or_else(|| anyhow::anyhow!("yt-dlp printed no video metadata"))?;
    let id = v["id"].as_str().filter(|id| video_id(id).as_deref() == Some(*id))
        .ok_or_else(|| anyhow::anyhow!("yt-dlp metadata has no valid video id"))?;
    let live = v["live_status"].as_str().is_some_and(|s| matches!(s, "is_live" | "is_upcoming" | "post_live"))
        || v["is_live"].as_bool() == Some(true);
    anyhow::ensure!(!live, "live streams can't be clipped; wait for the replay");
    let duration = v["duration"].as_f64().map(|d| d as f32).filter(|d| d.is_finite() && *d > 0.0)
        .ok_or_else(|| anyhow::anyhow!("video has no known duration"))?;
    anyhow::ensure!(duration <= MAX_INFO_SECONDS, "video is {:.1} h long; the limit is 4 h", duration / 3600.0);
    let text = |k: &str| v[k].as_str().map(|s| clean_text(s, 300)).filter(|s| !s.is_empty());
    Ok(VideoInfo {
        id: id.to_string(),
        url: format!("https://www.youtube.com/watch?v={id}"),
        title: text("title").unwrap_or_else(|| format!("YouTube video {id}")),
        channel: text("channel").or_else(|| text("uploader")).unwrap_or_default(),
        duration_s: duration,
    })
}

/// Long-video clip window: finite, inside the video and 5-180 s long
/// (1 ms slack for f32 rounding).
fn validate_segment(a: f32, b: f32, video_s: f32) -> anyhow::Result<()> {
    anyhow::ensure!(a.is_finite() && b.is_finite() && a >= 0.0 && b > a,
        "clip window must be finite, nonnegative and increasing; got {a}-{b}");
    let len = b - a;
    anyhow::ensure!((5.0 - 0.001..=180.0 + 0.001).contains(&len), "clip must be 5-180s long; got {len:.2}s");
    anyhow::ensure!(video_s.is_finite() && video_s > 0.0 && b <= video_s + 0.001,
        "clip window {a}-{b} exceeds the video's {video_s}s");
    Ok(())
}

fn validate_window(a: f32, b: f32) -> anyhow::Result<()> {
    anyhow::ensure!(a.is_finite() && b.is_finite() && a >= 0.0 && b > a
        && (1.0..=3.0).contains(&(b - a)), "YouTube window must be finite, nonnegative and exactly 1-3s; got {a}-{b}");
    Ok(())
}
fn valid_cue(c: &CaptionCue) -> bool {
    c.start_s.is_finite() && c.end_s.is_finite() && c.start_s >= 0.0 && c.end_s > c.start_s
        && !c.text.trim().is_empty() && c.text.len() <= 16_384
}
struct CaptionWork(PathBuf);
impl Drop for CaptionWork {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[cfg(test)]
mod tests {
    use super::video_id;

    #[test]
    fn ids_from_urls() {
        assert_eq!(video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=5").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id("https://youtu.be/dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id("https://www.youtube.com/shorts/dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(video_id("https://example.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(video_id("https://evilyoutube.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(video_id("file://youtube.com/watch?v=dQw4w9WgXcQ"), None);
    }

    #[test]
    fn exact_windows_only() {
        for (a, b) in [(0.0, 1.0), (20.5, 23.5)] { assert!(super::validate_window(a, b).is_ok()); }
        for (a, b) in [(-1.0, 1.0), (0.0, 0.9), (0.0, 3.1), (0.0, 6.0), (0.0, f32::NAN)] {
            assert!(super::validate_window(a, b).is_err());
        }
    }

    #[test]
    fn segment_windows() {
        for (a, b) in [(0.0, 5.0), (10.5, 190.5), (420.0, 600.0), (595.0, 600.0), (0.3, 5.3)] {
            assert!(super::validate_segment(a, b, 600.0).is_ok(), "{a}-{b}");
        }
        for (a, b, d) in [(-1.0, 10.0, 600.0), (0.0, 4.9, 600.0), (0.0, 180.5, 600.0), (595.0, 601.0, 600.0),
            (f32::NAN, 10.0, 600.0), (10.0, 5.0, 600.0), (0.0, f32::INFINITY, 600.0), (0.0, 10.0, f32::NAN), (0.0, 10.0, 0.0)] {
            assert!(super::validate_segment(a, b, d).is_err(), "{a}-{b} of {d}");
        }
    }

    #[test]
    fn parses_video_info() {
        use serde_json::json;
        let line = |v: serde_json::Value| format!("noise from a plugin\n{v}\n\n");
        let ok = json!({"id": "dQw4w9WgXcQ", "title": "  Never\n Gonna\tGive  ", "channel": null, "uploader": "Rick",
            "duration": 213.5, "live_status": "not_live", "is_live": false});
        let i = super::parse_video_info(&line(ok)).unwrap();
        assert_eq!((i.id.as_str(), i.url.as_str()), ("dQw4w9WgXcQ", "https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert_eq!((i.title.as_str(), i.channel.as_str(), i.duration_s), ("Never Gonna Give", "Rick", 213.5));
        let long = json!({"id": "dQw4w9WgXcQ", "title": "é".repeat(400), "channel": "C", "duration": 60});
        let i = super::parse_video_info(&line(long)).unwrap();
        assert_eq!((i.title.chars().count(), i.channel.as_str()), (300, "C"));
        for status in ["is_live", "is_upcoming", "post_live"] {
            let v = json!({"id": "dQw4w9WgXcQ", "title": "t", "duration": 60, "live_status": status});
            assert!(super::parse_video_info(&line(v)).unwrap_err().to_string().contains("wait for the replay"));
        }
        let live = json!({"id": "dQw4w9WgXcQ", "title": "t", "duration": 60, "is_live": true});
        assert!(super::parse_video_info(&line(live)).is_err());
        for d in [json!(null), json!(0), json!(-5), json!("NA"), json!(14_401)] {
            assert!(super::parse_video_info(&line(json!({"id": "dQw4w9WgXcQ", "title": "t", "duration": d}))).is_err(), "{d}");
        }
        assert!(super::parse_video_info(&line(json!({"id": "../etc", "title": "t", "duration": 60}))).is_err());
        assert!(super::parse_video_info("").is_err());
        assert!(super::parse_video_info("{malformed\n").is_err());
    }
}

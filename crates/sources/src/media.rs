//! Downloading and normalizing media: images -> PNG/JPEG, GIF/webm -> H.264 mp4.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clipcut_core::media::Probe;
use clipcut_core::{Asset, AssetKind, Candidate, Tools};
use serde_json::json;

use crate::Sources;
use crate::util::{clear_stem, download_to, relative_to_project, run, with_ext};

const MAX_IMAGE_BYTES: u64 = 30 * 1024 * 1024;
const MAX_GIF_BYTES: u64 = 40 * 1024 * 1024;
const MAX_STOCK_BYTES: u64 = 40 * 1024 * 1024;
/// Longest image side kept; bigger photos are downscaled.
const MAX_IMAGE_SIDE: u32 = 2560;
const MAX_VIDEO_SECONDS: f32 = 120.0;
/// Long-video sections (`fetch_youtube_segment`): up to 180 s plus slack.
pub(crate) const MAX_SEGMENT_SECONDS: f32 = 200.0;
const MAX_IMAGE_PIXELS: u64 = 40_000_000;

struct VideoStream {
    codec: String,
    pix_fmt: String,
    width: u32,
    height: u32,
    frames: u64,
}

impl Sources {
    async fn probe(&self, file: &Path, max_s: f32) -> anyhow::Result<Probe> {
        let ffprobe = Tools::require(&self.tools.ffprobe, "ffprobe")?;
        let path = file.to_string_lossy();
        let out = run(ffprobe, &["-v", "error", "-show_entries",
            "stream=codec_type,width,height,avg_frame_rate:format=duration", "-of", "json", &path],
            None, Duration::from_secs(15)).await?;
        let v: serde_json::Value = serde_json::from_str(&out.stdout)?;
        let mut probe = Probe::default();
        probe.duration_s = v["format"]["duration"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        for s in v["streams"].as_array().into_iter().flatten() {
            if s["codec_type"].as_str() == Some("video") && !probe.has_video {
                probe.has_video = true;
                probe.width = s["width"].as_u64().and_then(|n| n.try_into().ok()).unwrap_or(0);
                probe.height = s["height"].as_u64().and_then(|n| n.try_into().ok()).unwrap_or(0);
                if let Some((a, b)) = s["avg_frame_rate"].as_str().and_then(|r| r.split_once('/')) {
                    let (a, b) = (a.parse::<f32>().unwrap_or(0.0), b.parse::<f32>().unwrap_or(0.0));
                    if b > 0.0 { probe.fps = a / b; }
                }
            }
            if s["codec_type"].as_str() == Some("audio") { probe.has_audio = true; }
        }
        validate_video_probe(&probe, max_s)?;
        Ok(probe)
    }

    async fn video_stream(&self, file: &Path) -> anyhow::Result<VideoStream> {
        let ffprobe = Tools::require(&self.tools.ffprobe, "ffprobe")?;
        let path = file.to_string_lossy();
        let out = run(
            ffprobe,
            &["-v", "error", "-select_streams", "v:0", "-read_intervals", "%+2", "-count_frames",
                "-show_entries", "stream=codec_name,pix_fmt,width,height,nb_read_frames", "-of", "json", &path],
            None,
            Duration::from_secs(15),
        )
        .await?;
        let v: serde_json::Value = serde_json::from_str(&out.stdout)?;
        let s = &v["streams"][0];
        anyhow::ensure!(s.is_object(), "{} has no video stream", file.display());
        Ok(VideoStream {
            codec: s["codec_name"].as_str().unwrap_or_default().to_string(),
            pix_fmt: s["pix_fmt"].as_str().unwrap_or_default().to_string(),
            width: s["width"].as_u64().unwrap_or(0) as u32,
            height: s["height"].as_u64().unwrap_or(0) as u32,
            frames: s["nb_read_frames"].as_str().and_then(|n| n.parse().ok()).unwrap_or(0),
        })
    }

    pub(crate) async fn validate_video(&self, file: &Path) -> anyhow::Result<Probe> {
        self.validate_video_within(file, MAX_VIDEO_SECONDS).await
    }

    /// `validate_video` with an explicit duration cap (seconds).
    pub(crate) async fn validate_video_within(&self, file: &Path, max_s: f32) -> anyhow::Result<Probe> {
        let p = self.probe(file, max_s).await?;
        let s = self.video_stream(file).await?;
        anyhow::ensure!(s.frames >= 2, "media is a still image / single-frame video, not animation");
        let ffmpeg = Tools::require(&self.tools.ffmpeg, "ffmpeg")?;
        run(ffmpeg, &["-nostdin", "-v", "error", "-xerror", "-i", &file.to_string_lossy(),
            "-map", "0:v:0", "-frames:v", "2", "-an", "-f", "null", "-"],
            None, Duration::from_secs(20)).await?;
        Ok(p)
    }

    /// Make `input` an H.264/yuv420p mp4 with even dimensions at
    /// `dest_dir/<stem>.mp4`. Already-compliant mp4s are kept as they are.
    pub(crate) async fn ensure_h264_mp4(
        &self,
        input: &Path,
        dest_dir: &Path,
        stem: &str,
        keep_audio: bool,
    ) -> anyhow::Result<PathBuf> {
        let target = with_ext(dest_dir, stem, "mp4");
        let p = self.validate_video(input).await?;
        let stream = self.video_stream(input).await?;
        let is_mp4 = input.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4"));
        let compliant = stream.codec == "h264"
            && stream.pix_fmt == "yuv420p"
            && stream.width % 2 == 0
            && stream.height % 2 == 0
            && stream.width.max(stream.height) <= 1920;
        if compliant && is_mp4 && (keep_audio || !p.has_audio) {
            if input != target {
                tokio::fs::rename(input, &target).await?;
            }
            return Ok(target);
        }
        let ffmpeg = Tools::require(&self.tools.ffmpeg, "ffmpeg")?;
        let tmp = with_ext(dest_dir, stem, "tmp.mp4");
        let input_s = input.to_string_lossy().to_string();
        let tmp_s = tmp.to_string_lossy().to_string();
        let mut args: Vec<&str> = vec!["-nostdin", "-y", "-v", "error", "-xerror", "-i", &input_s, "-map", "0:v:0"];
        if keep_audio {
            args.extend(["-map", "0:a:0?", "-c:a", "aac", "-b:a", "160k"]);
        } else {
            args.push("-an");
        }
        args.extend([
            "-vf",
            "scale=w='trunc(iw*min(1,1920/max(iw,ih))/2)*2':h='trunc(ih*min(1,1920/max(iw,ih))/2)*2',format=yuv420p",
            "-threads",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "18",
            "-movflags",
            "+faststart",
            &tmp_s,
        ]);
        run(ffmpeg, &args, None, Duration::from_secs(90)).await?;
        self.validate_video(&tmp).await?;
        if input != target {
            let _ = tokio::fs::remove_file(input).await;
        }
        tokio::fs::rename(&tmp, &target).await?;
        Ok(target)
    }

    /// Download + verify an image; keep PNG/JPEG, convert everything else.
    pub(crate) async fn fetch_image_file(
        &self,
        url: &str,
        dest_dir: &Path,
        stem: &str,
        referer: Option<&str>,
    ) -> anyhow::Result<(PathBuf, u32, u32, AssetKind)> {
        tokio::fs::create_dir_all(dest_dir).await?;
        clear_stem(dest_dir, stem);
        let raw = with_ext(dest_dir, stem, "download");
        download_to(&self.http, url, &raw, MAX_IMAGE_BYTES, Duration::from_secs(90), referer).await?;
        let bytes = tokio::fs::read(&raw).await?;
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_lowercase();
        if head.contains("<svg") {
            let svg = std::str::from_utf8(&bytes)?;
            let (w, h) = crate::svg::dimensions(svg)?;
            let out = with_ext(dest_dir, stem, "svg");
            tokio::fs::rename(&raw, &out).await?;
            return Ok((out, w, h, AssetKind::Svg));
        }
        anyhow::ensure!(!head.trim_start().starts_with("<!doctype") && !head.contains("<html"), "{url} returned an HTML page, not an image");
        let result = tokio::task::spawn_blocking({
            let raw = raw.clone();
            let dest_dir = dest_dir.to_path_buf();
            let stem = stem.to_string();
            move || normalize_image(&raw, &bytes, &dest_dir, &stem)
        })
        .await?;
        match result {
            Ok((path, w, h)) => {
                if path != raw {
                    let _ = std::fs::remove_file(&raw);
                }
                Ok((path, w, h, AssetKind::Image))
            }
            // Do not reinterpret arbitrary audio/video/HTML as a first-frame image.
            Err(e) => { let _ = std::fs::remove_file(&raw); Err(e) }
        }
    }

    pub(crate) async fn fetch_image(&self, c: &Candidate, dest_dir: &Path, stem: &str) -> anyhow::Result<Asset> {
        let url = c.media_url.as_deref().unwrap_or(&c.page_url);
        let referer = (c.page_url != url).then_some(c.page_url.as_str());
        let ((path, w, h, kind), actual_url) = match self.fetch_image_file(url, dest_dir, stem, referer).await {
            Ok(r) => (r, url),
            // Hotlink-protected originals: fall back to the search thumbnail.
            Err(e) => match c.thumbnail_url.as_deref().filter(|t| *t != url) {
                Some(thumb) => (self.fetch_image_file(thumb, dest_dir, stem, referer).await.map_err(|_| e)?, thumb),
                None => return Err(e),
            },
        };
        let asset = Asset {
            kind,
            path: relative_to_project(dest_dir, &path),
            provider: c.provider.clone(),
            source_url: c.page_url.clone(),
            credit: credit_for(c),
            width: w,
            height: h,
            duration_s: None,
            trim_start_s: 0.0,
            focus: None,
        };
        crate::provenance::record(c, &asset, &path, Some(actual_url), json!({
            "mode": if kind == AssetKind::Svg { "static_svg" } else { "decoded_image" },
            "thumbnail_fallback": actual_url != url,
        }))?;
        Ok(asset)
    }

    pub(crate) async fn fetch_video_file(&self, c: &Candidate, dest_dir: &Path, stem: &str, max: u64) -> anyhow::Result<Asset> {
        let url = c.media_url.as_deref().ok_or_else(|| anyhow::anyhow!("{} candidate has no media URL", c.provider))?;
        tokio::fs::create_dir_all(dest_dir).await?;
        clear_stem(dest_dir, stem);
        let ext = url
            .split(['?', '#'])
            .next()
            .and_then(|u| u.rsplit('.').next())
            .filter(|e| matches!(e.to_ascii_lowercase().as_str(), "mp4" | "gif" | "webm" | "mov" | "ogv" | "m4v"))
            .unwrap_or("bin")
            .to_ascii_lowercase();
        let raw = with_ext(dest_dir, stem, &format!("src.{ext}"));
        download_to(&self.http, url, &raw, max, Duration::from_secs(45), None).await?;
        let keep_audio = c.kind == clipcut_core::VisualKind::Stock;
        let path = self.ensure_h264_mp4(&raw, dest_dir, stem, keep_audio).await?;
        let probe = self.validate_video(&path).await?;
        let asset = Asset {
            kind: AssetKind::Video,
            path: relative_to_project(dest_dir, &path),
            provider: c.provider.clone(),
            source_url: c.page_url.clone(),
            credit: credit_for(c),
            width: probe.width,
            height: probe.height,
            duration_s: Some(probe.duration_s),
            trim_start_s: 0.0,
            focus: None,
        };
        crate::provenance::record(c, &asset, &path, Some(url), json!({
            "mode": "bounded_video_download", "normalized": "h264/yuv420p/mp4",
        }))?;
        Ok(asset)
    }

    pub(crate) async fn fetch_gif(&self, c: &Candidate, dest_dir: &Path, stem: &str) -> anyhow::Result<Asset> {
        self.fetch_video_file(c, dest_dir, stem, MAX_GIF_BYTES).await
    }

    pub(crate) async fn fetch_stock(&self, c: &Candidate, dest_dir: &Path, stem: &str) -> anyhow::Result<Asset> {
        self.fetch_video_file(c, dest_dir, stem, MAX_STOCK_BYTES).await
    }
}

/// Decode to verify; keep PNG/JPEG bytes, re-encode other formats (and
/// oversized photos). Returns the final path and its dimensions.
fn normalize_image(raw: &Path, bytes: &[u8], dest_dir: &Path, stem: &str) -> anyhow::Result<(PathBuf, u32, u32)> {
    use image::ImageFormat;
    let format = image::guess_format(bytes)?;
    let img = decode_image(bytes)?;
    let (w, h) = (img.width(), img.height());
    anyhow::ensure!(w >= 16 && h >= 16, "image is only {w}x{h}");
    let too_big = w.max(h) > MAX_IMAGE_SIDE;
    match format {
        ImageFormat::Jpeg | ImageFormat::Png if !too_big => {
            let ext = if format == ImageFormat::Png { "png" } else { "jpg" };
            let out = with_ext(dest_dir, stem, ext);
            std::fs::rename(raw, &out)?;
            Ok((out, w, h))
        }
        _ => {
            let img = if too_big { img.resize(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE, image::imageops::FilterType::Triangle) } else { img };
            let (w, h) = (img.width(), img.height());
            if img.color().has_alpha() {
                let out = with_ext(dest_dir, stem, "png");
                img.save_with_format(&out, ImageFormat::Png)?;
                Ok((out, w, h))
            } else {
                let out = with_ext(dest_dir, stem, "jpg");
                let file = std::fs::File::create(&out)?;
                let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 92);
                enc.encode_image(&img.to_rgb8())?;
                Ok((out, w, h))
            }
        }
    }
}

/// One credits.txt line per candidate.
pub(crate) fn credit_for(c: &Candidate) -> String {
    let e = &c.extra;
    let s = |k: &str| e[k].as_str().unwrap_or_default().trim().to_string();
    let source = match c.provider.as_str() {
        "giphy" => format!("GIPHY: {}", c.page_url),
        "tenor" => format!("Tenor: {}", c.page_url),
        "imgflip" => format!("Meme template \"{}\" via Imgflip ({})", c.title, c.page_url),
        "wikimedia" => {
            let mut parts = vec![c.title.trim_start_matches("File:").to_string()];
            for k in ["artist", "license"] {
                let v = s(k);
                if !v.is_empty() {
                    parts.push(v);
                }
            }
            format!("Wikimedia Commons: {} ({})", parts.join(", "), c.page_url)
        }
        "openverse" => {
            let mut parts = vec![c.title.clone()];
            for k in ["creator", "license"] {
                let v = s(k);
                if !v.is_empty() {
                    parts.push(v);
                }
            }
            format!("Openverse: {} ({})", parts.join(", "), c.page_url)
        }
        "pexels" => format!("Pexels: {} by {} ({})", c.title, s("author"), c.page_url),
        "pixabay" => format!("Pixabay: {} by {} ({})", c.title, s("author"), c.page_url),
        "unsplash" => format!("Unsplash: photo by {} ({})", s("author"), c.page_url),
        other => {
            let label = match other {
                "searxng" | "duckduckgo" | "brave" | "tavily" => "Image",
                o => o,
            };
            format!("{label}: {} ({})", c.title, c.page_url)
        }
    };
    let license = crate::provenance::license_info(c);
    if license["status"].as_str() == Some("unknown_requires_review") {
        format!("{source} — license unknown; rights review required (not cleared)")
    } else {
        format!("{source} — source-declared license: {}{}; verify conditions and third-party rights (not cleared)",
            license["name"].as_str().unwrap_or_default(),
            license["url"].as_str().map(|u| format!(" <{u}>")).unwrap_or_default())
    }
}

pub(crate) fn decode_image(bytes: &[u8]) -> anyhow::Result<image::DynamicImage> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(192 * 1024 * 1024);
    reader.limits(limits);
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?.into_dimensions()?;
    anyhow::ensure!(w >= 16 && h >= 16 && u64::from(w) * u64::from(h) <= MAX_IMAGE_PIXELS,
        "image dimensions {w}x{h} are outside decode limits");
    Ok(reader.decode()?)
}

fn validate_video_probe(p: &Probe, max_s: f32) -> anyhow::Result<()> {
    anyhow::ensure!(p.has_video && p.width >= 16 && p.height >= 16
        && p.width.max(p.height) <= 4096 && u64::from(p.width) * u64::from(p.height) <= 8_847_360,
        "missing video / unsafe dimensions {}x{}", p.width, p.height);
    anyhow::ensure!(p.duration_s.is_finite() && p.duration_s > 0.0 && p.duration_s <= max_s,
        "video duration must be finite and within 0-{max_s}s, got {}", p.duration_s);
    anyhow::ensure!(p.fps.is_finite() && p.fps > 0.0 && p.fps <= 120.0, "unsupported video frame rate {}", p.fps);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_forged_images_and_unsafe_video_metadata() {
        assert!(decode_image(b"<html>fake PNG</html>").is_err());
        assert!(decode_image(b"").is_err());
        let mut p = Probe { has_video: true, width: 640, height: 360, duration_s: 2.0, fps: 25.0, ..Default::default() };
        assert!(validate_video_probe(&p, MAX_VIDEO_SECONDS).is_ok());
        p.duration_s = 180.0;
        assert!(validate_video_probe(&p, MAX_SEGMENT_SECONDS).is_ok());
        for d in [0.0, -1.0, f32::NAN, f32::INFINITY, 121.0] {
            p.duration_s = d;
            assert!(validate_video_probe(&p, MAX_VIDEO_SECONDS).is_err());
        }
        p.duration_s = 201.0;
        assert!(validate_video_probe(&p, MAX_SEGMENT_SECONDS).is_err());
    }
}

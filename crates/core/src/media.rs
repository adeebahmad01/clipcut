use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::tools::run;

#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub duration_s: f32,
    pub fps: f32,
    pub has_audio: bool,
    pub has_video: bool,
}

/// `ffprobe` a media file (video, image, gif or audio).
pub async fn probe(ffprobe: &Path, file: &Path) -> anyhow::Result<Probe> {
    let path = file.to_string_lossy();
    let out = run(
        ffprobe,
        &["-v", "error", "-show_entries", "stream=codec_type,width,height,avg_frame_rate:format=duration", "-of", "json", &path],
        None,
        Duration::from_secs(30),
    )
    .await?;
    let v: serde_json::Value = serde_json::from_str(&out.stdout)?;
    let mut probe = Probe::default();
    probe.duration_s = v["format"]["duration"].as_str().and_then(|d| d.parse().ok()).unwrap_or(0.0);
    for s in v["streams"].as_array().into_iter().flatten() {
        match s["codec_type"].as_str() {
            Some("video") if !probe.has_video => {
                probe.has_video = true;
                probe.width = s["width"].as_u64().unwrap_or(0) as u32;
                probe.height = s["height"].as_u64().unwrap_or(0) as u32;
                if let Some((n, d)) = s["avg_frame_rate"].as_str().and_then(|r| r.split_once('/')) {
                    let (n, d): (f32, f32) = (n.parse().unwrap_or(0.0), d.parse().unwrap_or(1.0));
                    if d > 0.0 {
                        probe.fps = n / d;
                    }
                }
            }
            Some("audio") => probe.has_audio = true,
            _ => {}
        }
    }
    Ok(probe)
}

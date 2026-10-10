//! Designed thumbnail: the strongest beat image (or a frame of a beat clip),
//! focus-cropped and graded, with the hook set in Anton and its key word in
//! the accent color. Falls back to a gradient card when no visual qualifies.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use image::RgbaImage;
use image::imageops::{self, FilterType};
use resvg::tiny_skia::{self as sk, Pixmap, Transform};

use clipcut_core::model::{AssetKind, Project, ProjectKind, VisualKind};
use clipcut_core::timeline::{Align, FontKind, RectF, Rgba};

use crate::control::{Control, StagedFile};
use crate::director::key_word;
use crate::ffmpeg::{VideoSpec, grab_frame, probe};
use crate::scene::load_image;
use crate::text::{self, DrawOpts, norm};
use crate::util::{Rng, color_alpha, paint, pixmap_from_rgba, unpremultiply};

/// Longest edge a source frame is decoded at.
const MAX_SOURCE_EDGE: u32 = 1920;
/// Words of the headline kept on the thumbnail.
const MAX_WORDS: usize = 7;

const BACKDROPS: [(Rgba, Rgba); 4] = [
    (Rgba(28, 18, 70, 255), Rgba(120, 30, 110, 255)),
    (Rgba(8, 38, 72, 255), Rgba(12, 120, 128, 255)),
    (Rgba(70, 10, 24, 255), Rgba(170, 60, 30, 255)),
    (Rgba(40, 14, 80, 255), Rgba(20, 90, 160, 255)),
];

/// A visual worth putting on the cover.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    pub video: bool,
    /// Seconds into a video to grab.
    pub at_s: f32,
    pub focus: Option<RectF>,
    pub width: u32,
    pub height: u32,
}

/// Rank beat visuals: real photos/footage only (no memes, article shots or
/// text cards), preferring larger sources, a known focus and earlier beats.
pub fn pick_source(project: &Project, project_dir: &Path, ffprobe: Option<&Path>) -> Option<Source> {
    let n = project.beats.len().max(1) as f32;
    let mut best: Option<(f32, Source)> = None;
    let mut seen = std::collections::HashSet::new();
    for (i, beat) in project.beats.iter().enumerate() {
        let Some(asset) = &beat.asset else { continue };
        let kind = beat.chosen.and_then(|c| beat.candidates.get(c)).map(|c| c.kind).unwrap_or(beat.visual.kind);
        if asset.kind == AssetKind::Svg || matches!(kind, VisualKind::Meme | VisualKind::Article | VisualKind::Text) {
            continue;
        }
        let path = project_dir.join(&asset.path);
        if !path.is_file() || !seen.insert((asset.path.clone(), (asset.trim_start_s * 10.0) as i64)) {
            continue;
        }
        let video = asset.kind == AssetKind::Video;
        let (mut w, mut h, mut dur) = (asset.width, asset.height, asset.duration_s.unwrap_or(0.0));
        if (w == 0 || h == 0)
            && video
            && let Some((pw, ph, pd)) = ffprobe.and_then(|fp| probe(fp, &path))
        {
            (w, h, dur) = (pw, ph, if dur > 0.0 { dur } else { pd });
        }
        if w == 0 || h == 0 {
            if video {
                continue;
            }
            // Images are decoded later anyway; score them as mid-sized.
            (w, h) = (1000, 1000);
        }
        let pixels = ((w as f32 * h as f32) / 2_000_000.0).min(1.0);
        let score = pixels + if asset.focus.is_some() { 0.5 } else { 0.0 } + 0.6 * (1.0 - i as f32 / n)
            + if kind == VisualKind::Image { 0.15 } else { 0.0 };
        // Past the cut point: a beat clip's opening frame is often a fade.
        let mut at_s = asset.trim_start_s + 0.4;
        if dur > 0.0 && at_s >= dur {
            at_s = asset.trim_start_s.min(dur * 0.5);
        }
        let source = Source { path, video, at_s, focus: asset.focus, width: w, height: h };
        if best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, source));
        }
    }
    best.map(|(_, s)| s)
}

/// Headline for the cover: the chosen hook, else the title, else the topic.
pub fn headline(project: &Project) -> String {
    let text = match (&project.kind, &project.clip) {
        (ProjectKind::Clip, Some(clip)) if !clip.hook.trim().is_empty() => clip.hook.clone(),
        _ => project
            .chosen_hook_text()
            .map(str::to_string)
            .or_else(|| project.script.as_ref().map(|s| s.title.clone()).filter(|t| !t.trim().is_empty()))
            .unwrap_or_else(|| project.topic.clone()),
    };
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() <= MAX_WORDS {
        return words.join(" ");
    }
    // Prefer ending on a clause ("X hit records, but…" → "X hit records").
    let clause = (3..=MAX_WORDS).rev().find(|&n| words[n - 1].ends_with([',', '.', ':', ';', '?', '!', '—']));
    let trim = |s: String| s.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '?' && c != '!').to_string();
    match clause {
        Some(n) => trim(words[..n].join(" ")),
        None => trim(words[..MAX_WORDS].join(" ")) + "…",
    }
}

/// Decode the source at (at most) `MAX_SOURCE_EDGE`.
pub fn load_source(src: &Source, ffmpeg: Option<&Path>, control: &Arc<Control>) -> anyhow::Result<RgbaImage> {
    if !src.video {
        return load_image(&src.path).with_context(|| format!("cannot decode {}", src.path.display()));
    }
    let ff = ffmpeg.context("ffmpeg is required to grab a video frame")?;
    let scale = (MAX_SOURCE_EDGE as f32 / src.width.max(src.height) as f32).min(1.0);
    let even = |v: u32| ((v as f32 * scale) as u32 / 2 * 2).max(2);
    let (w, h) = (even(src.width), even(src.height));
    let spec = VideoSpec {
        layer: 0,
        path: src.path.to_string_lossy().into_owned(),
        trim_start_s: src.at_s,
        looped: false,
        width: w,
        height: h,
        filter: format!("scale={w}:{h}:flags=bicubic,setsar=1"),
        start_s: 0.0,
        end_s: 1.0,
    };
    let px = grab_frame(ff, &spec, 0, control)?;
    RgbaImage::from_raw(w, h, unpremultiply(px.data().to_vec())).context("frame buffer")
}

/// Normalized crop with the target's aspect, zoomed slightly, holding the
/// focus center at `anchor_x` across the crop (subject right of the text).
pub fn focus_crop(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32, focus: Option<RectF>, anchor_x: f32) -> RectF {
    let (src_ar, dst_ar) = (src_w as f32 / src_h.max(1) as f32, dst_w as f32 / dst_h.max(1) as f32);
    let (mut cw, mut ch) = if src_ar > dst_ar { (dst_ar / src_ar, 1.0) } else { (1.0, src_ar / dst_ar) };
    cw /= 1.08;
    ch /= 1.08;
    let (fx, fy) = focus.map(|f| (f.x + f.w / 2.0, f.y + f.h / 2.0)).unwrap_or((0.5, 0.42));
    let x = (fx - cw * anchor_x).clamp(0.0, 1.0 - cw);
    let y = (fy - ch * 0.45).clamp(0.0, 1.0 - ch);
    RectF::new(x, y, cw, ch)
}

/// Contrast and saturation lift; thumbnails are seen small.
fn grade(img: &mut RgbaImage) {
    for p in img.pixels_mut() {
        let [r, g, b, _] = p.0;
        let (r, g, b) = (r as f32, g as f32, b as f32);
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        let f = |c: f32| {
            let c = l + (c - l) * 1.2;
            ((c / 255.0 - 0.5) * 1.12 + 0.52).clamp(0.0, 1.0) * 255.0
        };
        p.0 = [f(r) as u8, f(g) as u8, f(b) as u8, 255];
    }
}

fn backdrop(seed: &str, w: u32, h: u32) -> anyhow::Result<Pixmap> {
    let mut px = Pixmap::new(w, h).context("thumbnail size")?;
    let (from, to) = *Rng::from_str(seed).pick(&BACKDROPS);
    let shader = sk::LinearGradient::new(
        sk::Point::from_xy(0.0, 0.0),
        sk::Point::from_xy(w as f32, h as f32),
        vec![sk::GradientStop::new(0.0, color_alpha(from, 1.0)), sk::GradientStop::new(1.0, color_alpha(to, 1.0))],
        sk::SpreadMode::Pad,
        Transform::identity(),
    )
    .context("backdrop gradient")?;
    let paint = sk::Paint { shader, anti_alias: true, ..sk::Paint::default() };
    px.fill_rect(sk::Rect::from_xywh(0.0, 0.0, w as f32, h as f32).context("rect")?, &paint, Transform::identity(), None);
    Ok(px)
}

/// Darken toward the text so white type always reads.
fn shade(px: &mut Pixmap, landscape: bool) {
    let (w, h) = (px.width() as f32, px.height() as f32);
    let mut band = |start: sk::Point, end: sk::Point, alpha: f32| {
        let stops = vec![
            sk::GradientStop::new(0.0, color_alpha(Rgba(0, 0, 0, 255), 0.0)),
            sk::GradientStop::new(1.0, color_alpha(Rgba(0, 0, 0, 255), alpha)),
        ];
        if let (Some(shader), Some(rect)) = (
            sk::LinearGradient::new(start, end, stops, sk::SpreadMode::Pad, Transform::identity()),
            sk::Rect::from_xywh(0.0, 0.0, w, h),
        ) {
            let paint = sk::Paint { shader, anti_alias: true, ..sk::Paint::default() };
            px.fill_rect(rect, &paint, Transform::identity(), None);
        }
    };
    if landscape {
        band(sk::Point::from_xy(w * 0.72, 0.0), sk::Point::from_xy(0.0, 0.0), 0.78);
        band(sk::Point::from_xy(0.0, h * 0.55), sk::Point::from_xy(0.0, h), 0.45);
    } else {
        band(sk::Point::from_xy(0.0, h * 0.38), sk::Point::from_xy(0.0, h), 0.86);
    }
}

/// Compose one cover at `w`x`h` from an already-decoded source.
pub fn compose(
    source: Option<(&RgbaImage, Option<RectF>)>,
    headline: &str,
    accent: Rgba,
    seed: &str,
    w: u32,
    h: u32,
) -> anyhow::Result<Pixmap> {
    let landscape = w > h;
    let mut px = match source {
        Some((img, focus)) => {
            let c = focus_crop(img.width(), img.height(), w, h, focus, if landscape { 0.66 } else { 0.5 });
            let (iw, ih) = (img.width() as f32, img.height() as f32);
            let (x, y) = ((c.x * iw) as u32, (c.y * ih) as u32);
            let cw = ((c.w * iw) as u32).clamp(1, img.width() - x.min(img.width() - 1));
            let ch = ((c.h * ih) as u32).clamp(1, img.height() - y.min(img.height() - 1));
            let mut out = imageops::resize(&imageops::crop_imm(img, x, y, cw, ch).to_image(), w, h, FilterType::CatmullRom);
            grade(&mut out);
            pixmap_from_rgba(w, h, out.into_raw()).context("thumbnail pixels")?
        }
        None => backdrop(seed, w, h)?,
    };
    shade(&mut px, landscape);
    if headline.trim().is_empty() {
        return Ok(px);
    }
    let (fw, fh) = (w as f32, h as f32);
    let (max_w, max_h, bottom) =
        if landscape { (fw * 0.58, fh * 0.62, fh * 0.9) } else { (fw * 0.88, fh * if w == h { 0.42 } else { 0.3 }, fh * 0.8) };
    // Measure the real cap height: layouts can under-report it, and the cover
    // must not push type off its top edge.
    let face = text::face(FontKind::Display);
    let cap = face.path("H", 100.0, 0.0, 0.0).map(|p| -p.bounds().top() / 100.0).unwrap_or(0.75);
    let size = fw.min(fh) * 0.2;
    let lay = text::layout(FontKind::Display, headline, size, max_w, max_h - (cap - face.cap).max(0.0) * size, true);
    let top = bottom - (lay.lines.len().max(1) - 1) as f32 * lay.line_h - cap * lay.size;
    let key = key_word(headline).map(|k| norm(&k));
    let hot: Vec<bool> = lay
        .words
        .iter()
        .map(|wd| key.as_deref() == Some(norm(&wd.text).as_str()) || wd.text.chars().any(|c| c.is_ascii_digit()))
        .collect();
    let color_of = |wi: usize| if hot.get(wi).copied().unwrap_or(false) { accent } else { Rgba::WHITE };
    let (x, box_w, align) = if landscape { (fw * 0.06, max_w, Align::Left) } else { ((fw - max_w) / 2.0, max_w, Align::Center) };
    // An accent bar anchors left-aligned type.
    if landscape {
        let bar = cap * lay.size;
        if let Some(r) = sk::Rect::from_xywh(x, top - bar * 0.4, fw * 0.12, (bar * 0.12).max(6.0)) {
            px.fill_rect(r, &paint(color_alpha(accent, 1.0)), Transform::identity(), None);
        }
    }
    let opts = DrawOpts {
        align,
        stroke: Some((Rgba(0, 0, 0, 255), lay.size * 0.06)),
        shadow: true,
        pill: None,
        color_of: &color_of,
        reveal_chars: None,
        opacity: 1.0,
    };
    text::draw_layout(&mut px, &lay, x, bottom - lay.height, box_w, &opts, Transform::identity());
    Ok(px)
}

fn save(px: Pixmap, out: &Path, jpeg: bool, control: &Arc<Control>) -> anyhow::Result<()> {
    let (w, h) = (px.width(), px.height());
    let img = RgbaImage::from_raw(w, h, unpremultiply(px.take())).context("frame buffer")?;
    let staged = StagedFile::new(out, if jpeg { "jpg" } else { "png" })?;
    if jpeg {
        let file = std::fs::File::create(&staged.path)?;
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 90);
        enc.encode_image(&image::DynamicImage::ImageRgba8(img).to_rgb8())
            .with_context(|| format!("cannot write {}", out.display()))?;
    } else {
        img.save(&staged.path).with_context(|| format!("cannot write {}", out.display()))?;
    }
    control.publish(&staged.path, out)
}

/// Write the cover at the video's size (PNG) and at 1280x720 (JPEG, for upload).
#[allow(clippy::too_many_arguments)]
pub fn render(
    project: &Project,
    project_dir: &Path,
    ffmpeg: Option<&Path>,
    ffprobe: Option<&Path>,
    accent: Rgba,
    (w, h): (u32, u32),
    out_png: &Path,
    out_jpg: &Path,
    control: &Arc<Control>,
) -> anyhow::Result<()> {
    control.check()?;
    let picked = pick_source(project, project_dir, ffprobe);
    let decoded = match &picked {
        Some(src) => match load_source(src, ffmpeg, control) {
            Ok(img) => Some((img, src.focus)),
            Err(e) => {
                control.check()?;
                tracing::warn!("thumbnail source {} unusable: {e:#}", src.path.display());
                None
            }
        },
        None => None,
    };
    let source = decoded.as_ref().map(|(img, focus)| (img, *focus));
    let line = headline(project);
    control.check()?;
    save(compose(source, &line, accent, &project.id, w, h)?, out_png, false, control)?;
    save(compose(source, &line, accent, &project.id, 1280, 720)?, out_jpg, true, control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_keeps_aspect_and_stays_inside() {
        let c = focus_crop(1920, 1080, 1080, 1920, Some(RectF::new(0.85, 0.1, 0.1, 0.2)), 0.5);
        assert!((c.w * 1920.0 / (c.h * 1080.0) - 1080.0 / 1920.0).abs() < 0.01);
        assert!(c.x >= 0.0 && c.x + c.w <= 1.0 + 1e-6 && c.y >= 0.0 && c.y + c.h <= 1.0 + 1e-6);
        // Pushed as far right as the frame allows, toward the focus.
        assert!((c.x + c.w - 1.0).abs() < 1e-4);
    }

    #[test]
    fn covers_compose_with_and_without_a_source() {
        let mut img = RgbaImage::new(640, 360);
        for (x, _, p) in img.enumerate_pixels_mut() {
            p.0 = [(x % 255) as u8, 90, 160, 255];
        }
        let line = "The 3 rules nobody tells you about money";
        for (w, h) in [(1080, 1920), (1280, 720), (1080, 1080)] {
            let with = compose(Some((&img, None)), line, Rgba(255, 214, 10, 255), "p1", w, h).unwrap();
            let without = compose(None, line, Rgba(255, 214, 10, 255), "p1", w, h).unwrap();
            assert_eq!((with.width(), with.height()), (w, h));
            assert_eq!((without.width(), without.height()), (w, h));
            // The accent-colored digit lands somewhere on the cover.
            let yellow = with.pixels().iter().filter(|p| p.red() > 230 && p.green() > 190 && p.blue() < 60).count();
            assert!(yellow > 200, "{w}x{h}: {yellow}");
        }
    }

    #[test]
    fn headline_prefers_the_hook_and_stays_short() {
        let mut p = Project::new("octopus brains", "", clipcut_core::config::Format::Vertical, 45);
        assert_eq!(headline(&p), "octopus brains");
        p.hooks = vec![clipcut_core::HookOption {
            text: "One two three four five six seven eight, nine!".into(),
            angle: String::new(),
            score: 8.0,
            why: String::new(),
        }];
        p.chosen_hook = Some(0);
        assert_eq!(headline(&p), "One two three four five six seven…");
        p.hooks[0].text = "X says downloads hit records, but its comeback story has one problem.".into();
        assert_eq!(headline(&p), "X says downloads hit records");
        p.hooks[0].text = "Is X really back? Downloads say yes, users say no".into();
        assert_eq!(headline(&p), "Is X really back? Downloads say yes");
    }

    #[test]
    fn meme_and_text_beats_never_become_the_cover() {
        let dir = std::env::temp_dir().join(format!("clipcut-thumb-pick-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        for f in ["meme.png", "photo.jpg"] {
            std::fs::write(dir.join("assets").join(f), b"x").unwrap();
        }
        let beat = |id: &str, kind: &str, path: &str| -> clipcut_core::Beat {
            serde_json::from_value(serde_json::json!({
                "id": id, "text": "t", "visual": {"kind": kind, "query": "q"},
                "asset": {"kind": "image", "path": path, "provider": "x", "source_url": "", "credit": "",
                          "width": 1920, "height": 1080}
            }))
            .unwrap()
        };
        let mut p = Project::new("t", "", clipcut_core::config::Format::Vertical, 45);
        p.beats = vec![beat("b1", "meme", "assets/meme.png"), beat("b2", "image", "assets/photo.jpg")];
        assert_eq!(pick_source(&p, &dir, None).unwrap().path, dir.join("assets/photo.jpg"));
        p.beats.truncate(1);
        assert!(pick_source(&p, &dir, None).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}

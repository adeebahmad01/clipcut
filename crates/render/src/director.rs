//! The director: turns a `Project` (beats, assets, voice timings) into a
//! `Timeline` of layers, captions and SFX cues. Deterministic per project id.

use std::path::{Path, PathBuf};

use clipcut_core::Config;
use clipcut_core::model::{AssetKind, Beat, Project, ProjectKind, VisualKind, WordTiming};
use clipcut_core::timeline::{
    Align, Anim, AnimKind, AudioPlan, CaptionStyle, CaptionWord, CardStyle, Fit, FontKind, Layer,
    LayerContent, Motion, MusicBed, Pan, RectF, Rgba, SfxCue, SfxKind, TextStyle, Timeline,
};

use crate::scene::meme_band_height;
use crate::text::norm;
use crate::util::Rng;

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "at", "for", "with", "is", "are",
    "was", "were", "be", "been", "it", "its", "this", "that", "these", "those", "you", "your",
    "we", "our", "they", "their", "he", "she", "his", "her", "i", "my", "me", "so", "as", "by",
    "from", "just", "than", "then", "not", "no", "do", "does", "did", "has", "have", "had", "can",
    "will", "would", "could", "should", "what", "which", "who", "how", "why", "when", "where",
    "there", "here", "all", "more", "most", "very", "into", "about", "up", "out", "if", "also",
    "only", "one", "get", "got", "like", "really", "even", "every",
];

/// (accent, secondary) per video.
const PALETTES: [(Rgba, Rgba); 5] = [
    (Rgba(255, 214, 10, 255), Rgba(120, 225, 255, 255)),
    (Rgba(170, 255, 60, 255), Rgba(255, 214, 10, 255)),
    (Rgba(60, 220, 255, 255), Rgba(255, 214, 10, 255)),
    (Rgba(255, 145, 30, 255), Rgba(255, 232, 130, 255)),
    (Rgba(255, 80, 170, 255), Rgba(255, 214, 10, 255)),
];

const GRADIENTS: [(Rgba, Rgba); 6] = [
    (Rgba(28, 18, 70, 255), Rgba(120, 30, 110, 255)),
    (Rgba(8, 38, 72, 255), Rgba(12, 120, 128, 255)),
    (Rgba(70, 10, 24, 255), Rgba(170, 60, 30, 255)),
    (Rgba(16, 16, 24, 255), Rgba(60, 60, 92, 255)),
    (Rgba(10, 60, 40, 255), Rgba(30, 140, 90, 255)),
    (Rgba(40, 14, 80, 255), Rgba(20, 90, 160, 255)),
];

fn is_stop(w: &str) -> bool {
    STOPWORDS.contains(&w)
}

/// The word to highlight: one with a digit, else the longest non-stopword.
pub fn key_word(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if let Some(w) = words.iter().find(|w| w.chars().any(|c| c.is_ascii_digit())) {
        return Some(w.to_string());
    }
    words
        .iter()
        .filter(|w| !is_stop(&norm(w)))
        .max_by_key(|w| norm(w).chars().count())
        .map(|w| w.to_string())
}

fn keywords(text: &str, n: usize) -> String {
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|w| !is_stop(&norm(w)) && !norm(w).is_empty())
        .collect();
    words
        .iter()
        .take(n)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '%' && c != '$'))
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate_words(text: &str, n: usize) -> String {
    text.split_whitespace()
        .take(n)
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Treatment {
    Clip,
    Article,
    Meme,
    ImageCard,
    FullImage,
    FullVideo,
    Stock,
    Kinetic,
}

#[derive(Clone, Debug)]
struct Media {
    path: String,
    video: bool,
    w: u32,
    h: u32,
    dur: Option<f32>,
    trim: f32,
    focus: Option<RectF>,
}

impl Media {
    fn aspect(&self) -> f32 {
        if self.w > 0 && self.h > 0 {
            self.w as f32 / self.h as f32
        } else if self.video {
            16.0 / 9.0
        } else {
            1.0
        }
    }
}

fn resolve_media(beat: &Beat, project_dir: &Path, ffprobe: Option<&Path>) -> Option<Media> {
    let a = beat.asset.as_ref()?;
    let p = Path::new(&a.path);
    let path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_dir.join(p)
    };
    if !path.is_file() {
        tracing::warn!("beat {}: asset {} missing", beat.id, path.display());
        return None;
    }
    let path_s = path.to_string_lossy().to_string();
    let (mut w, mut h, mut dur) = (a.width, a.height, a.duration_s);
    // A real GIF remains moving media even if an importer labels it Image.
    let video = a.kind == AssetKind::Video
        || path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gif"));
    if a.kind == AssetKind::Svg {
        return None;
    }
    if video {
        if (w == 0 || h == 0 || dur.is_none())
            && let Some(fp) = ffprobe
            && let Some((pw, ph, pd)) = crate::ffmpeg::probe(fp, &path)
        {
            if w == 0 || h == 0 {
                (w, h) = (pw, ph);
            }
            if dur.is_none() && pd.is_finite() && pd > 0.0 {
                dur = Some(pd);
            }
        }
    } else if (w == 0 || h == 0)
        && let Ok((iw, ih)) = image::image_dimensions(&path)
    {
        (w, h) = (iw, ih);
    }

    Some(Media {
        path: path_s,
        video,
        w,
        h,
        dur,
        trim: a.trim_start_s.max(0.0),
        focus: a.focus,
    })
}

/// Normalized geometry for the output format.
struct Geo {
    w: f32,
    h: f32,
    vertical: bool,
    square: bool,
}

impl Geo {
    fn region(&self) -> RectF {
        if self.square {
            RectF::new(0.1, 0.25, 0.8, 0.46)
        } else if self.vertical {
            RectF::new(0.05, 0.25, 0.9, 0.37)
        } else {
            RectF::new(0.04, 0.1, 0.56, 0.7)
        }
    }
    fn text_rect(&self, hook: bool) -> RectF {
        match (self.square, self.vertical, hook) {
            (true, _, false) => RectF::new(0.06, 0.05, 0.88, 0.16),
            (true, _, true) => RectF::new(0.05, 0.04, 0.9, 0.19),
            (false, true, false) => RectF::new(0.06, 0.105, 0.88, 0.135),
            (false, true, true) => RectF::new(0.05, 0.09, 0.9, 0.155),
            (false, false, _) => RectF::new(0.62, 0.2, 0.35, 0.45),
        }
    }
    fn text_size(&self, hook: bool) -> f32 {
        match (self.square, self.vertical, hook) {
            (true, _, false) => 92.0,
            (true, _, true) => 112.0,
            (false, true, false) => 118.0,
            (false, true, true) => 148.0,
            (false, false, false) => 104.0,
            (false, false, true) => 124.0,
        }
    }
    fn caption_y(&self) -> f32 {
        if self.square {
            0.83
        } else if self.vertical {
            0.72
        } else {
            0.875
        }
    }
    /// Card rect fitting `aspect` inside `region` (cover-cropped below `min_frac` width).
    fn card_rect(&self, aspect: f32, region: RectF, min_frac: f32) -> RectF {
        let (rw, rh) = (region.w * self.w, region.h * self.h);
        let mut cw = rw;
        let mut ch = cw / aspect.max(0.1);
        if ch > rh {
            ch = rh;
            cw = ch * aspect;
        }
        cw = cw.max(rw * min_frac).min(rw);
        let x = region.x * self.w + (rw - cw) / 2.0;
        let y = region.y * self.h + (rh - ch) * 0.3;
        RectF::new(x / self.w, y / self.h, cw / self.w, ch / self.h)
    }
    /// Square-in-pixels rect of side `side_w` (fraction of width) centered at (cx, cy).
    fn square(&self, cx: f32, cy: f32, side_w: f32) -> RectF {
        let hn = side_w * self.w / self.h;
        RectF::new(cx - side_w / 2.0, cy - hn / 2.0, side_w, hn)
    }
}

/// Shot limits are not just suggestions: no configured maximum can exceed
/// three seconds, and invalid config values fall back to the default bounds.
fn clip_bounds(min: f32, max: f32) -> (f32, f32) {
    let max = if max.is_finite() {
        max.clamp(1.0, 3.0)
    } else {
        3.0
    };
    let min = if min.is_finite() {
        min.clamp(1.0, max)
    } else {
        1.0
    };
    (min, max)
}

/// Split [s, e), choosing word onsets only inside the legal cut interval.
/// Balance the remaining pieces so jitter/snapping cannot create an overlong
/// final shot. A sub-second beat (or infeasible user minimum) keeps its timing;
/// the maximum always takes priority rather than extending narration.
fn split_pieces(
    s: f32,
    e: f32,
    min: f32,
    max: f32,
    words: &[WordTiming],
    rng: &mut Rng,
) -> Vec<(f32, f32)> {
    let len = (e as f64) - (s as f64);
    if len <= max as f64 {
        return vec![(s, e)];
    }
    let n = (len / max as f64).ceil() as usize;
    let base = len / n as f64;
    let min = (min as f64).min(base);
    let mut cuts = vec![s];
    for k in 1..n {
        let prev = *cuts.last().unwrap() as f64;
        let remaining = (n - k) as f64;
        let lo = (prev + min).max(e as f64 - remaining * max as f64);
        let hi = (prev + max as f64).min(e as f64 - remaining * min).max(lo);
        let target =
            (s as f64 + base * k as f64 + rng.range(-0.15, 0.15) as f64 * base).clamp(lo, hi);
        let cut = words
            .iter()
            .filter(|w| {
                w.start_s.is_finite()
                    && w.start_s as f64 >= lo
                    && w.start_s as f64 <= hi
                    && (w.start_s as f64 - target).abs() <= 0.25
            })
            .min_by(|a, b| {
                (a.start_s as f64 - target)
                    .abs()
                    .total_cmp(&(b.start_s as f64 - target).abs())
            })
            .map(|w| w.start_s as f64)
            .unwrap_or(target);
        cuts.push(cut as f32);
    }
    cuts.push(e);
    cuts.windows(2).map(|p| (p[0], p[1])).collect()
}

/// Source-normalized crop of 1/zoom around a focus point.
fn punch_crop(focus: Option<RectF>, zoom: f32) -> RectF {
    let (cx, cy) = focus
        .map(|f| (f.x + f.w / 2.0, f.y + f.h / 2.0))
        .unwrap_or((0.5, 0.42));
    let s = 1.0 / zoom;
    RectF::new(
        (cx - s / 2.0).clamp(0.0, 1.0 - s),
        (cy - s / 2.0).clamp(0.0, 1.0 - s),
        s,
        s,
    )
}

fn svg_is_mono(path: &Path) -> bool {
    match std::fs::read_to_string(path) {
        Ok(s) => s.contains("currentColor") || !(s.contains('#') || s.contains("rgb(")),
        Err(_) => false,
    }
}

struct Director<'a> {
    geo: Geo,
    rng: Rng,
    accent: Rgba,
    second: Rgba,
    pill_mode: bool,
    sfx_volume: f32,
    min_clip: f32,
    max_clip: f32,
    words: &'a [WordTiming],
    layers: Vec<Layer>,
    sfx: Vec<SfxCue>,
    card_anims: Vec<(AnimKind, f32)>,
    card_anim_idx: usize,
    icon_side: bool,
    gradient_idx: usize,
}

fn base_gain(k: SfxKind) -> f32 {
    match k {
        SfxKind::Whoosh => 0.55,
        SfxKind::Swoosh => 0.5,
        SfxKind::Pop => 0.5,
        SfxKind::Click => 0.45,
        SfxKind::Ding => 0.4,
        SfxKind::Riser => 0.45,
        SfxKind::Hit => 0.85,
        SfxKind::Typewriter => 0.4,
        SfxKind::Glitch => 0.45,
        SfxKind::Camera => 0.6,
        SfxKind::RecordScratch => 0.55,
    }
}

impl Director<'_> {
    fn cue(&mut self, kind: SfxKind, at_s: f32, rel: f32) {
        let gain = base_gain(kind) * rel * self.sfx_volume;
        let big = matches!(kind, SfxKind::Hit | SfxKind::Riser);
        if !big
            && self
                .sfx
                .iter()
                .any(|c| (c.at_s - at_s).abs() < 0.1 && !matches!(c.kind, SfxKind::Riser))
        {
            return;
        }
        self.sfx.push(SfxCue {
            kind,
            at_s: at_s.max(0.0),
            gain,
        });
    }

    fn next_card_anim(&mut self) -> Anim {
        if self.card_anim_idx >= self.card_anims.len() {
            let last = self.card_anims.last().map(|a| a.0);
            self.rng.shuffle(&mut self.card_anims);
            if self.card_anims.first().map(|a| a.0) == last {
                self.card_anims.rotate_left(1);
            }
            self.card_anim_idx = 0;
        }
        let (k, d) = self.card_anims[self.card_anim_idx];
        self.card_anim_idx += 1;
        Anim::new(k, d)
    }

    fn card_style(&self) -> CardStyle {
        CardStyle {
            radius: self.geo.w.min(self.geo.h) * 0.035,
            shadow: true,
            border: Some((Rgba(255, 255, 255, 46), 3.0)),
        }
    }

    fn text_style(&self, size: f32, key: Option<&str>, pill: bool) -> TextStyle {
        TextStyle {
            font: FontKind::Display,
            size_px: size,
            color: Rgba::WHITE,
            stroke: (!pill).then_some((Rgba(8, 8, 10, 255), size * 0.075)),
            background: pill.then_some(Rgba(14, 14, 18, 235)),
            align: Align::Center,
            uppercase: true,
            highlight_words: key.map(|k| vec![k.to_string()]).unwrap_or_default(),
            highlight_color: Some(self.accent),
            shadow: true,
        }
    }

    /// When the key word is spoken inside the beat (or shortly after its start).
    fn text_start(&self, s: f32, e: f32, key: Option<&str>) -> f32 {
        let fallback = s + 0.05;
        let Some(k) = key.map(norm) else {
            return fallback;
        };
        self.words
            .iter()
            .find(|w| w.start_s >= s - 0.05 && w.start_s < e && norm(&w.text) == k)
            .map(|w| w.start_s)
            .filter(|&t| t <= (s + 1.2).min(e - 0.8))
            .map(|t| t.max(fallback))
            .unwrap_or(fallback)
    }

    /// On-screen text overlay; returns its start time.
    #[allow(clippy::too_many_arguments)]
    fn add_text(
        &mut self,
        text: &str,
        s: f32,
        e: f32,
        rect: RectF,
        size: f32,
        hook: bool,
        pill: bool,
    ) -> f32 {
        let text = truncate_words(text, 5);
        let key = key_word(&text);
        let start = (if hook {
            s + 0.05
        } else {
            self.text_start(s, e, key.as_deref())
        })
        .min(e - 0.001)
        .max(s);
        let style = self.text_style(size, key.as_deref(), pill);
        let mut l = Layer::new(
            start,
            e,
            20,
            LayerContent::Text {
                text: text.clone(),
                style,
            },
            rect,
        );
        let roll = self.rng.f32();
        let chars = text.chars().count() as f32;
        let (anim, sfx) = if hook || roll < 0.45 {
            (Anim::new(AnimKind::Pop, 0.32), Some((SfxKind::Pop, 0.6)))
        } else if roll < 0.7 && text.split_whitespace().count() <= 4 {
            (
                Anim::new(AnimKind::Typewriter, (chars * 0.045).clamp(0.3, 0.8)),
                Some((SfxKind::Typewriter, 0.8)),
            )
        } else {
            (
                Anim::new(AnimKind::SlideUp, 0.32),
                self.rng.chance(0.5).then_some((SfxKind::Swoosh, 0.45)),
            )
        };
        l.enter = anim;
        l.motion = if hook {
            Motion::Shake { intensity: 0.6 }
        } else {
            Motion::PushIn { amount: 0.04 }
        };
        self.layers.push(l);
        if let Some((k, g)) = sfx
            && !hook
        {
            self.cue(k, start, g);
        }
        start
    }

    fn add_icon(&mut self, path: &Path, rect: RectF, start: f32, e: f32, white: bool) {
        if e - start < 0.5 {
            return;
        }
        let tint = svg_is_mono(path).then_some(if white { Rgba::WHITE } else { self.accent });
        let mut l = Layer::new(
            start,
            e,
            25,
            LayerContent::Icon {
                path: path.to_string_lossy().to_string(),
                tint,
            },
            rect,
        );
        l.fit = Fit::Contain;
        l.enter = Anim::new(
            if self.rng.chance(0.75) {
                AnimKind::Pop
            } else {
                AnimKind::Spin
            },
            0.4,
        );
        l.motion = Motion::Float;
        self.layers.push(l);
        if self.rng.chance(0.6) {
            self.cue(SfxKind::Pop, start, 0.55);
        }
    }

    /// Icon at a top corner of a card (alternating sides).
    fn icon_for_card(&mut self, card: RectF) -> RectF {
        let side = if self.geo.vertical { 0.15 } else { 0.085 };
        self.icon_side = !self.icon_side;
        let hn = side * self.geo.w / self.geo.h;
        let x = if self.icon_side {
            card.x + card.w - side * 0.72
        } else {
            card.x - side * 0.28
        };
        let y = card.y - hn * 0.5;
        RectF::new(x.clamp(0.01, 0.99 - side), y.max(0.01), side, hn)
    }

    fn backdrop(&mut self, m: &Media, s: f32, e: f32) {
        let content = if m.video {
            LayerContent::BlurredVideo {
                path: m.path.clone(),
                trim_start_s: m.trim,
                looped: true,
            }
        } else {
            LayerContent::BlurredImage {
                path: m.path.clone(),
            }
        };
        let mut l = Layer::new(s, e, 0, content, RectF::FULL);
        l.motion = Motion::PushIn { amount: 0.05 };
        self.layers.push(l);
    }

    fn full_bleed_overlays(&mut self, s: f32, e: f32) {
        let top = LayerContent::Gradient {
            from: Rgba(0, 0, 0, 175),
            to: Rgba(0, 0, 0, 0),
            angle_deg: 90.0,
        };
        self.layers
            .push(Layer::new(s, e, 1, top, RectF::new(0.0, 0.0, 1.0, 0.36)));
        let bottom = LayerContent::Gradient {
            from: Rgba(0, 0, 0, 0),
            to: Rgba(0, 0, 0, 165),
            angle_deg: 90.0,
        };
        self.layers.push(Layer::new(
            s,
            e,
            1,
            bottom,
            RectF::new(0.0, 0.58, 1.0, 0.42),
        ));
    }

    fn flash(&mut self, at: f32) {
        let mut l = Layer::new(
            at,
            at + 0.14,
            15,
            LayerContent::Solid {
                color: Rgba(255, 255, 255, 255),
            },
            RectF::FULL,
        );
        l.exit = Anim::new(AnimKind::Fade, 0.14);
        l.opacity = 0.55;
        self.layers.push(l);
    }

    /// Video layer content for sub-cut `j` (jump ahead or punch-in).
    fn video_piece(
        &mut self,
        m: &Media,
        gif: bool,
        s: f32,
        ps: f32,
        pe: f32,
        j: usize,
        jump: &mut f32,
    ) -> (LayerContent, Option<RectF>) {
        let len = pe - ps;
        let mut crop = None;
        if j > 0 && (gif || j % 2 == 1 || m.dur.is_none()) {
            crop = Some(punch_crop(m.focus, self.rng.range(1.25, 1.4)));
        } else if j > 0 {
            *jump += self.rng.range(0.8, 2.0);
        }
        let mut trim = m.trim + (ps - s) + *jump;
        let mut looped = gif;
        if let Some(d) = m.dur
            && !gif
            && trim + len > d - 0.05
        {
            if m.trim + len <= d {
                trim = (d - len - 0.05).max(m.trim);
            } else {
                trim = m.trim.min(d * 0.5);
                looped = true;
            }
        }
        if gif {
            trim = m.trim;
        }
        (
            LayerContent::Video {
                path: m.path.clone(),
                trim_start_s: trim,
                looped,
            },
            crop,
        )
    }
}

struct BeatCtx<'b> {
    beat: &'b Beat,
    s: f32,
    e: f32,
    hook: bool,
    media: Option<Media>,
    icon: Option<PathBuf>,
}

impl Director<'_> {
    fn treatment(&mut self, b: &BeatCtx) -> Treatment {
        let Some(m) = &b.media else {
            return Treatment::Kinetic;
        };
        let portrait = m.aspect() < 0.8 && self.geo.vertical;
        match b.beat.visual.kind {
            VisualKind::Text => Treatment::Kinetic,
            VisualKind::Stock => Treatment::Stock,
            VisualKind::Meme => Treatment::Meme,
            VisualKind::Article if !m.video => Treatment::Article,
            _ if m.video && portrait => Treatment::FullVideo,
            _ if m.video => Treatment::Clip,
            _ if portrait || self.rng.chance(0.3) => Treatment::FullImage,
            _ => Treatment::ImageCard,
        }
    }

    fn cut_sfx(&mut self, b: &BeatCtx, t: Treatment, card_anim: Option<Anim>) {
        if b.hook {
            self.cue(SfxKind::Hit, b.s, 1.0);
            return;
        }
        if let Some(k) = b.beat.visual.sfx {
            self.cue(k, b.s, 1.0);
            return;
        }
        let whip = card_anim.is_some_and(|a| a.kind == AnimKind::Whip);
        match t {
            Treatment::Article | Treatment::ImageCard | Treatment::FullImage
                if !whip && self.rng.chance(0.7) =>
            {
                self.cue(SfxKind::Camera, b.s, 1.0);
                if self.rng.chance(0.5) {
                    self.flash(b.s);
                }
            }
            Treatment::Meme => {
                let k = if self.rng.chance(0.5) {
                    SfxKind::RecordScratch
                } else {
                    SfxKind::Ding
                };
                self.cue(k, b.s, 1.0);
            }
            Treatment::Kinetic if self.rng.chance(0.3) => self.cue(SfxKind::Glitch, b.s, 0.8),
            _ => {
                if whip || self.rng.chance(0.75) {
                    let k = if self.rng.chance(0.6) {
                        SfxKind::Whoosh
                    } else {
                        SfxKind::Swoosh
                    };
                    self.cue(k, b.s, 1.0);
                }
            }
        }
    }

    fn sub_cut_sfx(&mut self, at: f32) {
        if self.rng.chance(0.35) {
            let k = if self.rng.chance(0.6) {
                SfxKind::Swoosh
            } else {
                SfxKind::Click
            };
            self.cue(k, at, 0.6);
        }
    }

    fn piece_motion(&mut self, j: usize) -> Motion {
        match j % 3 {
            0 => Motion::PushIn { amount: 0.07 },
            1 => Motion::PullOut { amount: 0.1 },
            _ => Motion::Pan {
                dx: if self.rng.chance(0.5) { 0.07 } else { -0.07 },
                dy: 0.0,
            },
        }
    }

    fn beat(&mut self, b: BeatCtx) {
        let t = self.treatment(&b);
        let pieces = split_pieces(
            b.s,
            b.e,
            self.min_clip,
            self.max_clip,
            self.words,
            &mut self.rng,
        );
        for p in pieces.iter().skip(1) {
            self.sub_cut_sfx(p.0);
        }
        let ost = b
            .beat
            .visual
            .on_screen_text
            .clone()
            .filter(|t| !t.trim().is_empty());
        match t {
            Treatment::Kinetic => self.kinetic(&b, &pieces, ost),
            Treatment::Stock => self.stock(&b, &pieces, ost),
            Treatment::FullImage | Treatment::FullVideo => self.full_bleed(&b, &pieces, ost),
            Treatment::Meme => self.meme(&b, &pieces, ost),
            Treatment::Clip | Treatment::ImageCard | Treatment::Article => {
                self.card(&b, t, &pieces, ost)
            }
        }
    }

    fn card(&mut self, b: &BeatCtx, t: Treatment, pieces: &[(f32, f32)], ost: Option<String>) {
        let m = b.media.clone().unwrap();
        let region = self.geo.region();
        let aspect = if t == Treatment::Article {
            region.w * self.geo.w / (region.h * self.geo.h) * 0.9
        } else {
            m.aspect()
        };
        let rect = self.geo.card_rect(aspect, region, 0.62);
        let anim = if b.hook {
            Anim::new(
                if self.rng.chance(0.5) {
                    AnimKind::ZoomIn
                } else {
                    AnimKind::Pop
                },
                0.35,
            )
        } else {
            self.next_card_anim()
        };
        for &(ps, pe) in pieces {
            let mut background = m.clone();
            if background.video {
                let offset = ps - b.s;
                background.trim += match m.dur.filter(|d| *d > m.trim) {
                    Some(d) => offset % (d - m.trim),
                    None => offset,
                };
            }
            self.backdrop(&background, ps, pe);
        }
        self.cut_sfx(b, t, Some(anim));
        let gif = b.beat.visual.kind == VisualKind::Gif;
        let (iw, ih) = (m.w.max(1) as f32, m.h.max(1) as f32);
        let card_aspect = rect.w * self.geo.w / (rect.h * self.geo.h);
        let focus = m.focus.unwrap_or(RectF::new(0.08, 0.12, 0.84, 0.18));
        // Article views: (source-normalized) crops with the card's aspect.
        let view = |width_px: f32, center: (f32, f32)| -> RectF {
            let w = width_px.min(iw).min(ih * card_aspect);
            let h = w / card_aspect;
            let x = (center.0 * iw - w / 2.0).clamp(0.0, iw - w);
            let y = (center.1 * ih - h / 2.0).clamp(0.0, ih - h);
            RectF::new(x / iw, y / ih, w / iw, h / ih)
        };
        let fc = (focus.x + focus.w / 2.0, focus.y + focus.h / 2.0);
        let mut jump = 0.0;
        let mut article_view = view(f32::MAX, (0.5, fc.1.min(0.35)));
        for (j, &(ps, pe)) in pieces.iter().enumerate() {
            let (content, crop, motion) = match t {
                Treatment::Clip => {
                    let (c, crop) = self.video_piece(&m, gif, b.s, ps, pe, j, &mut jump);
                    let mo = if j == 0 {
                        Motion::PushIn { amount: 0.06 }
                    } else {
                        self.piece_motion(j)
                    };
                    (c, crop, mo)
                }
                Treatment::Article => {
                    let tighter = (focus.w * iw * 1.2).max(article_view.w * iw * 0.55);
                    let to = view(
                        if j == 0 {
                            (focus.w * iw * 1.3).max(article_view.w * iw * 0.6)
                        } else {
                            tighter
                        },
                        fc,
                    );
                    let pan = Pan {
                        from: article_view,
                        to,
                    };
                    article_view = to;
                    (
                        LayerContent::Image {
                            path: m.path.clone(),
                            pan: Some(pan),
                        },
                        None,
                        Motion::None,
                    )
                }
                _ => {
                    let crop = (j > 0).then(|| punch_crop(m.focus, self.rng.range(1.2, 1.35)));
                    let mo = if j == 0 {
                        Motion::PushIn { amount: 0.09 }
                    } else {
                        self.piece_motion(j)
                    };
                    (
                        LayerContent::Image {
                            path: m.path.clone(),
                            pan: None,
                        },
                        crop,
                        mo,
                    )
                }
            };
            let mut l = Layer::new(ps, pe, 10, content, rect);
            l.card = Some(self.card_style());
            l.crop = crop;
            l.motion = motion;
            if j == 0 {
                l.enter = anim;
                if b.hook {
                    l.motion = Motion::Shake { intensity: 0.65 };
                }
            }
            self.layers.push(l);
        }
        let mut text_start = b.s + 0.25;
        if let Some(text) = &ost {
            let tr = self.geo.text_rect(b.hook);
            let pill = self.pill_mode && !b.hook;
            text_start =
                self.add_text(text, b.s, b.e, tr, self.geo.text_size(b.hook), b.hook, pill);
        }
        if let Some(icon) = b.icon.clone() {
            let r = self.icon_for_card(rect);
            self.add_icon(&icon, r, text_start.max(b.s + 0.2) + 0.15, b.e, false);
        }
    }

    fn full_bleed(&mut self, b: &BeatCtx, pieces: &[(f32, f32)], ost: Option<String>) {
        let m = b.media.clone().unwrap();
        self.cut_sfx(
            b,
            if m.video {
                Treatment::FullVideo
            } else {
                Treatment::FullImage
            },
            None,
        );
        let mut jump = 0.0;
        for (j, &(ps, pe)) in pieces.iter().enumerate() {
            let (content, crop) = if m.video {
                self.video_piece(
                    &m,
                    b.beat.visual.kind == VisualKind::Gif,
                    b.s,
                    ps,
                    pe,
                    j,
                    &mut jump,
                )
            } else {
                let crop = (j > 0).then(|| punch_crop(m.focus, self.rng.range(1.15, 1.3)));
                (
                    LayerContent::Image {
                        path: m.path.clone(),
                        pan: None,
                    },
                    crop,
                )
            };
            let mut l = Layer::new(ps, pe, 0, content, RectF::FULL);
            l.crop = crop;
            l.motion = if j == 0 {
                if b.hook {
                    Motion::Shake { intensity: 0.65 }
                } else {
                    Motion::PushIn { amount: 0.1 }
                }
            } else {
                self.piece_motion(j)
            };
            if j == 0 && !b.hook {
                l.enter = Anim::new(AnimKind::ZoomOut, 0.3);
            }
            self.layers.push(l);
        }
        self.full_bleed_overlays(b.s, b.e);
        let mut text_start = b.s + 0.25;
        if let Some(text) = &ost {
            let tr = self.geo.text_rect(b.hook);
            text_start = self.add_text(
                text,
                b.s,
                b.e,
                tr,
                self.geo.text_size(b.hook),
                b.hook,
                false,
            );
        }
        if let Some(icon) = b.icon.clone() {
            let tr = self.geo.text_rect(b.hook);
            let side = if self.geo.vertical { 0.14 } else { 0.08 };
            let r = self.geo.square(
                0.5,
                tr.y + tr.h + side * self.geo.w / self.geo.h * 0.75,
                side,
            );
            self.add_icon(&icon, r, text_start + 0.2, b.e, false);
        }
    }

    fn stock(&mut self, b: &BeatCtx, pieces: &[(f32, f32)], ost: Option<String>) {
        let m = b.media.clone().unwrap();
        self.cut_sfx(b, Treatment::Stock, None);
        let mut jump = 0.0;
        for (j, &(ps, pe)) in pieces.iter().enumerate() {
            let (content, crop) = if m.video {
                self.video_piece(&m, false, b.s, ps, pe, j, &mut jump)
            } else {
                (
                    LayerContent::Image {
                        path: m.path.clone(),
                        pan: None,
                    },
                    (j > 0).then(|| punch_crop(m.focus, 1.25)),
                )
            };
            let mut l = Layer::new(ps, pe, 0, content, RectF::FULL);
            l.crop = crop;
            l.motion = if j % 2 == 0 {
                Motion::PushIn { amount: 0.08 }
            } else {
                Motion::PullOut { amount: 0.08 }
            };
            self.layers.push(l);
        }
        self.layers.push(Layer::new(
            b.s,
            b.e,
            1,
            LayerContent::Solid {
                color: Rgba(0, 0, 0, 105),
            },
            RectF::FULL,
        ));
        let text = ost.unwrap_or_else(|| keywords(&b.beat.text, 3));
        if !text.is_empty() {
            let (rect, size) = if self.geo.vertical {
                (RectF::new(0.06, 0.36, 0.88, 0.2), 150.0)
            } else {
                (RectF::new(0.1, 0.3, 0.8, 0.3), 140.0)
            };
            let start = self.add_text(&text, b.s, b.e, rect, size, b.hook, false);
            if let Some(icon) = b.icon.clone() {
                let side = if self.geo.vertical { 0.16 } else { 0.09 };
                let r = self
                    .geo
                    .square(0.5, rect.y - side * self.geo.w / self.geo.h * 0.6, side);
                self.add_icon(&icon, r, start + 0.2, b.e, false);
            }
        }
    }

    fn meme(&mut self, b: &BeatCtx, pieces: &[(f32, f32)], ost: Option<String>) {
        let m = b.media.clone().unwrap();
        let plan = b.beat.visual.meme.clone().unwrap_or_default();
        let (mut caption, top, bottom) = (
            plan.caption.trim().to_string(),
            plan.top_text,
            plan.bottom_text,
        );
        if caption.is_empty() && top.trim().is_empty() && bottom.trim().is_empty() {
            caption = ost.clone().unwrap_or_default();
        }
        let has_caption = !caption.is_empty();
        let region = if !has_caption {
            self.geo.region()
        } else if self.geo.vertical {
            RectF::new(0.05, 0.12, 0.9, 0.52)
        } else {
            RectF::new(0.04, 0.06, 0.56, 0.78)
        };
        let (rw, rh) = (region.w * self.geo.w, region.h * self.geo.h);
        let mut cw = rw;
        let total = |cw: f32| meme_band_height(&caption, cw) + cw / m.aspect();
        while total(cw) > rh && cw > rw * 0.6 {
            cw *= 0.96;
        }
        let ch = total(cw).min(rh);
        let rect = RectF::new(
            (region.x * self.geo.w + (rw - cw) / 2.0) / self.geo.w,
            (region.y * self.geo.h + (rh - ch) * 0.3) / self.geo.h,
            cw / self.geo.w,
            ch / self.geo.h,
        );
        let anim = if b.hook {
            Anim::new(AnimKind::Pop, 0.35)
        } else {
            self.next_card_anim()
        };
        for &(ps, pe) in pieces {
            let mut background = m.clone();
            if background.video {
                let offset = ps - b.s;
                background.trim += match m.dur.filter(|d| *d > m.trim) {
                    Some(d) => offset % (d - m.trim),
                    None => offset,
                };
            }
            self.backdrop(&background, ps, pe);
        }
        self.cut_sfx(b, Treatment::Meme, Some(anim));
        for (j, &(ps, pe)) in pieces.iter().enumerate() {
            let content = LayerContent::Meme {
                path: m.path.clone(),
                video: m.video,
                trim_start_s: m.trim
                    + match m.dur.filter(|d| *d > m.trim) {
                        Some(d) => (ps - b.s) % (d - m.trim),
                        None => ps - b.s,
                    },
                caption: caption.clone(),
                top_text: top.clone(),
                bottom_text: bottom.clone(),
            };
            let mut l = Layer::new(ps, pe, 10, content, rect);
            l.card = Some(CardStyle {
                border: None,
                ..self.card_style()
            });
            if j == 0 {
                l.enter = anim;
                l.motion = if b.hook {
                    Motion::Shake { intensity: 0.6 }
                } else {
                    Motion::PushIn { amount: 0.05 }
                };
            } else {
                l.motion = if j % 2 == 1 {
                    Motion::PullOut { amount: 0.12 }
                } else {
                    Motion::PushIn { amount: 0.08 }
                };
            }
            self.layers.push(l);
        }
        let mut text_start = b.s + 0.25;
        if !has_caption && let Some(text) = &ost {
            let tr = self.geo.text_rect(b.hook);
            let pill = self.pill_mode && !b.hook;
            text_start =
                self.add_text(text, b.s, b.e, tr, self.geo.text_size(b.hook), b.hook, pill);
        }
        if let Some(icon) = b.icon.clone() {
            let r = self.icon_for_card(rect);
            self.add_icon(&icon, r, text_start + 0.15, b.e, false);
        }
    }

    fn kinetic(&mut self, b: &BeatCtx, pieces: &[(f32, f32)], ost: Option<String>) {
        self.cut_sfx(b, Treatment::Kinetic, None);
        let text = ost.unwrap_or_else(|| keywords(&b.beat.text, 3));
        let text = truncate_words(&text, 5).to_uppercase();
        let key = key_word(&text);
        // Lines of <= 2 words / ~10 chars.
        let mut lines: Vec<String> = Vec::new();
        for w in text.split_whitespace() {
            match lines.last_mut() {
                Some(l)
                    if l.split_whitespace().count() < 2
                        && l.chars().count() + 1 + w.chars().count() <= 10 =>
                {
                    l.push(' ');
                    l.push_str(w);
                }
                _ => lines.push(w.to_string()),
            }
        }
        lines.truncate(4);
        let icon = b.icon.clone();
        // Square frames have the room of a landscape one for stacked words.
        let vertical = self.geo.vertical && !self.geo.square;
        let (size, line_step, side) = if vertical {
            (176.0, 0.1, 0.3)
        } else {
            (150.0, 0.16, 0.16)
        };
        let n = lines.len().max(1) as f32;
        let center = if icon.is_some() {
            if vertical { 0.49 } else { 0.58 }
        } else if vertical {
            0.42
        } else {
            0.48
        };
        let y0 = center - n * line_step / 2.0;
        for (j, &(ps, pe)) in pieces.iter().enumerate() {
            let (from, to) = GRADIENTS[self.gradient_idx % GRADIENTS.len()];
            self.gradient_idx += 1 + self.rng.below(2);
            let mut g = Layer::new(
                ps,
                pe,
                0,
                LayerContent::Gradient {
                    from,
                    to,
                    angle_deg: self.rng.range(0.0, 360.0),
                },
                RectF::FULL,
            );
            g.motion = Motion::Rotate { deg_per_s: 25.0 };
            self.layers.push(g);
            for (li, line) in lines.iter().enumerate() {
                let rect = RectF::new(0.05, y0 + li as f32 * line_step, 0.9, line_step * 1.02);
                let mut style = self.text_style(size, key.as_deref(), false);
                style.stroke = Some((Rgba(8, 8, 10, 255), size * 0.06));
                let start = (if j == 0 {
                    ps + 0.04 + li as f32 * 0.12
                } else {
                    ps + li as f32 * 0.04
                })
                .min(pe - 0.001)
                .max(ps);
                let mut l = Layer::new(
                    start,
                    pe,
                    20,
                    LayerContent::Text {
                        text: line.clone(),
                        style,
                    },
                    rect,
                );
                l.enter = if j == 0 {
                    Anim::new(
                        if li % 2 == 0 {
                            AnimKind::SlideUp
                        } else {
                            AnimKind::Pop
                        },
                        0.3,
                    )
                } else {
                    Anim::new(AnimKind::Pop, 0.22)
                };
                l.motion = if b.hook && j == 0 {
                    Motion::Shake { intensity: 0.6 }
                } else {
                    Motion::PushIn { amount: 0.05 }
                };
                self.layers.push(l);
                if j == 0 && li < 3 {
                    self.cue(
                        if li % 2 == 0 {
                            SfxKind::Swoosh
                        } else {
                            SfxKind::Pop
                        },
                        start,
                        0.5,
                    );
                }
            }
            if let Some(icon) = &icon {
                let r = if vertical {
                    self.geo.square(0.5, 0.25, side)
                } else {
                    self.geo.square(0.5, 0.24, side)
                };
                let start = if j == 0 {
                    ps + 0.1 + n * 0.12
                } else {
                    ps + 0.05
                };
                self.add_icon(icon, r, start, pe, true);
            }
        }
    }
}

/// Where a clip's source sits: (top, height) of a full-width band, or `None`
/// for full-bleed. Wide sources in a vertical frame are cropped toward 1:1.
fn clip_band(geo: &Geo, source_aspect: f32) -> Option<(f32, f32)> {
    let frame = geo.w / geo.h;
    let band_aspect = source_aspect.clamp(frame, frame.max(1.0));
    let band_h = frame / band_aspect;
    (band_h < 0.92).then(|| (((1.0 - band_h) * 0.42).max(0.0), band_h))
}

/// Punch-in cuts for a clip: on sentence starts at least 4 s apart (any word
/// after 7 s without one), never in the last 2.5 s.
fn clip_cuts(s: f32, e: f32, sentences: &[WordTiming], words: &[WordTiming]) -> Vec<(f32, f32)> {
    let mut cuts = vec![s];
    if words.is_empty() {
        let n = ((e - s) / 6.0).floor() as usize;
        cuts.extend((1..n).map(|k| s + (e - s) * k as f32 / n as f32));
    }
    for w in words {
        let (t, last) = (w.start_s, *cuts.last().unwrap());
        if t <= last || t > e - 2.5 {
            continue;
        }
        let sentence = sentences.iter().any(|x| (x.start_s - t).abs() < 0.05);
        if (sentence && t - last >= 4.0) || t - last >= 7.0 {
            cuts.push(t);
        }
    }
    cuts.push(e);
    cuts.windows(2).map(|p| (p[0], p[1])).collect()
}

impl Director<'_> {
    /// Clip-from-long-video edit: the source moment plays straight through
    /// (its own audio is the voice track) as a band over a blurred copy of
    /// itself, punching in on sentence starts, with the hook on top. Returns
    /// the caption center line.
    fn clip(&mut self, b: &BeatCtx, hook: Option<String>, sentences: &[WordTiming]) -> f32 {
        let Some(m) = b.media.clone().filter(|m| m.video) else {
            let pieces =
                split_pieces(b.s, b.e, self.min_clip, self.max_clip, self.words, &mut self.rng);
            self.kinetic(b, &pieces, hook);
            return self.geo.caption_y();
        };
        let band = clip_band(&self.geo, m.aspect());
        let rect = band.map_or(RectF::FULL, |(top, h)| RectF::new(0.0, top, 1.0, h));
        if band.is_some() {
            self.backdrop(&m, b.s, b.e);
            let mut dim = Layer::new(
                b.s,
                b.e,
                1,
                LayerContent::Solid {
                    color: Rgba(0, 0, 0, 255),
                },
                RectF::FULL,
            );
            dim.opacity = 0.35;
            self.layers.push(dim);
        } else {
            self.full_bleed_overlays(b.s, b.e);
        }
        for (j, (ps, pe)) in clip_cuts(b.s, b.e, sentences, self.words)
            .into_iter()
            .enumerate()
        {
            let content = LayerContent::Video {
                path: m.path.clone(),
                trim_start_s: m.trim + (ps - b.s),
                looped: false,
            };
            let mut l = Layer::new(ps, pe, if band.is_some() { 10 } else { 0 }, content, rect);
            if j % 2 == 1 {
                l.crop = Some(punch_crop(m.focus, self.rng.range(1.15, 1.22)));
                let k = if self.rng.chance(0.5) {
                    SfxKind::Whoosh
                } else {
                    SfxKind::Swoosh
                };
                self.cue(k, ps, 0.45);
            } else {
                l.motion = Motion::PushIn { amount: 0.03 };
                if j > 0 {
                    self.cue(SfxKind::Click, ps, 0.4);
                }
            }
            self.layers.push(l);
        }
        if let Some(text) = hook.filter(|t| !t.trim().is_empty()) {
            let text = truncate_words(&text, 8);
            let (tr, end) = match band {
                // A header above the band for the whole clip.
                Some((top, _)) => (
                    RectF::new(0.05, 0.03, 0.9, (top - 0.045).max(0.07)),
                    b.e,
                ),
                None => (self.geo.text_rect(true), (b.s + 3.5).min(b.e)),
            };
            let key = key_word(&text);
            let style = self.text_style(self.geo.text_size(true), key.as_deref(), false);
            let mut l = Layer::new(b.s + 0.05, end, 20, LayerContent::Text { text, style }, tr);
            l.enter = Anim::new(AnimKind::Pop, 0.32);
            if band.is_none() {
                l.exit = Anim::new(AnimKind::Fade, 0.25);
            }
            self.layers.push(l);
            self.cue(SfxKind::Pop, b.s + 0.05, 0.6);
        }
        match band {
            Some((top, h)) if 1.0 - (top + h) >= 0.13 => top + h + 0.075,
            Some((top, h)) => top + h - 0.09,
            None => self.geo.caption_y(),
        }
    }
}

/// Build the timeline for a project.
pub fn direct(
    config: &Config,
    ffprobe: Option<&Path>,
    music: Option<&Path>,
    icons: &std::collections::HashMap<String, PathBuf>,
    project: &Project,
    project_dir: &Path,
) -> anyhow::Result<Timeline> {
    let (w, h) = project.format.size();
    // Square posts share the vertical layout with their own text and card bands.
    let geo = Geo {
        w: w as f32,
        h: h as f32,
        vertical: h >= w,
        square: h == w,
    };
    let mut rng = Rng::from_str(&project.id);
    let (accent, second) = *rng.pick(&PALETTES);
    let pill_mode = rng.chance(0.3);
    let voice = project.voice_track.as_ref();
    let mut clean_words: Vec<WordTiming> = voice
        .map(|v| {
            v.words
                .iter()
                .filter(|w| {
                    w.start_s.is_finite()
                        && w.end_s.is_finite()
                        && w.end_s > w.start_s
                        && w.end_s > 0.0
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    clean_words.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
    let words = clean_words.as_slice();

    let beats = &project.beats;
    let last_end = beats.last().map(|b| b.end_s).unwrap_or(0.0);
    let total = voice
        .map(|v| v.duration_s)
        .filter(|d| d.is_finite() && *d > 0.5)
        .or(Some(last_end).filter(|d| d.is_finite() && *d > 0.5))
        .unwrap_or(project.target_seconds.max(5) as f32);
    let aligned = !beats.is_empty()
        && beats
            .iter()
            .all(|b| b.start_s.is_finite() && b.end_s.is_finite() && b.end_s > b.start_s);
    let mut spans: Vec<(f32, f32)> = Vec::with_capacity(beats.len());
    for (i, b) in beats.iter().enumerate() {
        let s = spans.last().map(|p| p.1).unwrap_or(0.0);
        let e = if i + 1 == beats.len() {
            total.max(s + 0.3)
        } else if aligned {
            b.end_s.max(s + 0.3)
        } else {
            total * (i + 1) as f32 / beats.len() as f32
        };
        spans.push((s, e));
    }
    let duration = spans.last().map(|p| p.1).unwrap_or(total).max(total);

    let mut anims = vec![
        (AnimKind::Pop, 0.38),
        (AnimKind::Whip, 0.3),
        (AnimKind::SlideUp, 0.32),
        (AnimKind::ZoomIn, 0.35),
        (AnimKind::Drop, 0.5),
    ];
    rng.shuffle(&mut anims);
    let (min_clip, max_clip) =
        clip_bounds(config.video.min_clip_seconds, config.video.max_clip_seconds);
    let mut d = Director {
        geo,
        rng,
        accent,
        second,
        pill_mode,
        sfx_volume: config.audio.sfx_volume,
        min_clip,
        max_clip,
        words,
        layers: Vec::new(),
        sfx: Vec::new(),
        card_anims: anims,
        card_anim_idx: 0,
        icon_side: false,
        gradient_idx: 0,
    };
    d.gradient_idx = d.rng.below(GRADIENTS.len());
    let clip = project.kind == ProjectKind::Clip && !beats.is_empty();
    let mut caption_y = d.geo.caption_y();

    if beats.is_empty() {
        for (i, (s, e)) in split_pieces(0.0, duration, d.min_clip, d.max_clip, words, &mut d.rng)
            .into_iter()
            .enumerate()
        {
            let (from, to) = GRADIENTS[(d.gradient_idx + i) % GRADIENTS.len()];
            let mut layer = Layer::new(
                s,
                e,
                0,
                LayerContent::Gradient {
                    from,
                    to,
                    angle_deg: 90.0,
                },
                RectF::FULL,
            );
            layer.motion = Motion::Rotate { deg_per_s: 15.0 };
            d.layers.push(layer);
        }
    }
    for (i, b) in beats.iter().enumerate() {
        let (s, e) = spans[i];
        let media = resolve_media(b, project_dir, ffprobe);
        let svg_asset = b
            .asset
            .as_ref()
            .filter(|a| a.kind == AssetKind::Svg)
            .map(|a| {
                if Path::new(&a.path).is_absolute() {
                    PathBuf::from(&a.path)
                } else {
                    project_dir.join(&a.path)
                }
            });
        let icon = icons
            .get(&b.id)
            .cloned()
            .or(svg_asset)
            .filter(|p| p.is_file());
        let ctx = BeatCtx {
            beat: b,
            s,
            e,
            hook: i == 0,
            media,
            icon,
        };
        if clip {
            // One beat per clip; a stray extra beat is not drawn.
            if i == 0 {
                let hook = project
                    .clip
                    .as_ref()
                    .map(|c| c.hook.clone())
                    .or_else(|| b.visual.on_screen_text.clone());
                let sentences = voice.map(|v| v.sentences.as_slice()).unwrap_or_default();
                caption_y = d.clip(&BeatCtx { e: duration, ..ctx }, hook, sentences);
            }
            continue;
        }
        d.beat(ctx);
    }
    // Riser into the first cut.
    if !clip && spans.len() >= 2 && spans[0].1 - spans[0].0 >= 1.4 {
        d.cue(SfxKind::Riser, spans[0].1, 1.0);
    }

    // Captions: emphasis on numbers and on words from the beat's on-screen text.
    let captions: Vec<CaptionWord> = words
        .iter()
        .filter(|w| w.start_s < duration)
        .map(|wt| {
            let n = norm(&wt.text);
            let beat_idx = spans
                .iter()
                .position(|&(s, e)| wt.start_s >= s && wt.start_s < e);
            let in_ost = beat_idx
                .and_then(|i| beats[i].visual.on_screen_text.as_ref())
                .is_some_and(|t| !is_stop(&n) && t.split_whitespace().any(|w| norm(w) == n));
            CaptionWord {
                text: wt.text.clone(),
                start_s: wt.start_s.max(0.0),
                end_s: wt.end_s.min(duration),
                emphasis: wt.text.chars().any(|c| c.is_ascii_digit()) || in_ost,
            }
        })
        .collect();
    let preset = project.caption_preset.unwrap_or(config.video.caption_preset);
    let (font, scale, max_words, uppercase) = preset.shape();
    let caption_style = CaptionStyle {
        enabled: config.video.captions,
        font,
        size_px: (if geo_vertical(w, h) { 80.0 } else { 64.0 }) * scale,
        color: Rgba::WHITE,
        accent: d.accent,
        stroke: Rgba(0, 0, 0, 255),
        y: caption_y,
        max_words,
        uppercase,
        emphasis_color: Some(d.second),
        preset,
    };

    let voice_path = voice.map(|v| {
        let p = Path::new(&v.path);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            project_dir.join(p)
        }
    });
    let voice_path = voice_path
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().to_string());
    // A clip keeps its own soundtrack; a music bed would fight it.
    let music = music
        .filter(|_| config.audio.use_music && !clip)
        .filter(|p| p.is_file())
        .map(|p| MusicBed {
            path: p.to_string_lossy().to_string(),
            gain: config.audio.music_volume,
            duck_to: if config.audio.duck_music { 0.35 } else { 1.0 },
        });
    let mut sfx = d.sfx;
    sfx.sort_by(|a, b| a.at_s.total_cmp(&b.at_s));

    Ok(Timeline {
        width: w,
        height: h,
        fps: config.video.fps.clamp(12, 60),
        duration_s: duration,
        background: Rgba(10, 10, 12, 255),
        layers: d.layers,
        captions,
        caption_style,
        audio: AudioPlan {
            voice_path,
            voice_gain: config.audio.voice_volume,
            sfx,
            music,
            clip_audio: Vec::new(),
        },
    })
}

fn geo_vertical(w: u32, h: u32) -> bool {
    h > w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_config_is_bounded_even_when_invalid() {
        assert_eq!(clip_bounds(0.1, 9.0), (1.0, 3.0));
        assert_eq!(clip_bounds(4.0, 2.0), (2.0, 2.0));
        assert_eq!(clip_bounds(f32::NAN, f32::INFINITY), (1.0, 3.0));
    }

    fn geo(w: f32, h: f32) -> Geo {
        Geo {
            w,
            h,
            vertical: h >= w,
            square: h == w,
        }
    }

    #[test]
    fn clip_band_crops_wide_sources_toward_square_in_vertical_frames() {
        let (top, h) = clip_band(&geo(1080.0, 1920.0), 16.0 / 9.0).unwrap();
        assert!((h - 0.5625).abs() < 1e-4 && top > 0.15 && top + h < 0.8, "{top} {h}");
        // Portrait sources and matching frames fill the screen.
        assert!(clip_band(&geo(1080.0, 1920.0), 9.0 / 16.0).is_none());
        assert!(clip_band(&geo(1920.0, 1080.0), 16.0 / 9.0).is_none());
        assert!(clip_band(&geo(1080.0, 1080.0), 16.0 / 9.0).is_none());
    }

    #[test]
    fn clip_cuts_land_on_sentences_and_cover_the_clip() {
        let wt = |t: f32| WordTiming {
            text: "w".into(),
            start_s: t,
            end_s: t + 0.2,
        };
        let words: Vec<_> = (0..150).map(|i| wt(i as f32 * 0.3)).collect();
        let sentences = vec![wt(0.0), wt(3.0), wt(5.1), wt(15.0), wt(42.0)];
        let cuts = clip_cuts(0.0, 45.0, &sentences, &words);
        assert_eq!(cuts.first().unwrap().0, 0.0);
        assert_eq!(cuts.last().unwrap().1, 45.0);
        assert!((cuts[1].0 - 5.1).abs() < 0.01, "{cuts:?}");
        for (i, &(s, e)) in cuts.iter().enumerate() {
            assert!(e > s && e - s <= 7.31, "{cuts:?}");
            if i > 0 {
                assert_eq!(cuts[i - 1].1, s);
            }
        }
        assert!(cuts.iter().all(|c| c.0 <= 42.5), "no cut in the last 2.5 s: {cuts:?}");
        let even = clip_cuts(0.0, 30.0, &[], &[]);
        assert_eq!(even.len(), 5);
    }

    #[test]
    fn cuts_are_contiguous_bounded_and_deterministic_with_word_snapping() {
        for seed in 0..12 {
            for tick in 1..=360 {
                let start = 13.17;
                let end = start + tick as f32 / 10.0;
                let words: Vec<_> = (0..200)
                    .map(|i| WordTiming {
                        text: "word".into(),
                        start_s: start + i as f32 * 0.193,
                        end_s: start + i as f32 * 0.193 + 0.1,
                    })
                    .collect();
                let pieces = split_pieces(start, end, 1.0, 3.0, &words, &mut Rng::new(seed));
                assert_eq!(
                    pieces,
                    split_pieces(start, end, 1.0, 3.0, &words, &mut Rng::new(seed))
                );
                assert_eq!(pieces.first().unwrap().0, start);
                assert_eq!(pieces.last().unwrap().1, end);
                for (i, &(s, e)) in pieces.iter().enumerate() {
                    assert!(e > s && e - s <= 3.000_01, "{seed}/{tick}: {pieces:?}");
                    if end - start >= 1.0 {
                        assert!(e - s >= 0.999_99, "{seed}/{tick}: {pieces:?}");
                    }
                    if i > 0 {
                        assert_eq!(pieces[i - 1].1, s);
                    }
                }
            }
        }
    }
}

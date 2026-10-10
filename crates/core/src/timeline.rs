//! The render contract: a flat list of timed layers plus an audio plan.
//! `clipcut-render`'s director builds it from a `Project`; its compositor
//! turns it into frames. All times are seconds, all rects normalized 0..1.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub const FULL: RectF = RectF { x: 0.0, y: 0.0, w: 1.0, h: 1.0 };
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
}

/// RGBA, 0-255.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const WHITE: Rgba = Rgba(255, 255, 255, 255);
    pub const BLACK: Rgba = Rgba(0, 0, 0, 255);
    pub const TRANSPARENT: Rgba = Rgba(0, 0, 0, 0);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SfxKind {
    Whoosh,
    Swoosh,
    Pop,
    Click,
    Ding,
    Riser,
    Hit,
    Typewriter,
    Glitch,
    Camera,
    RecordScratch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timeline {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub duration_s: f32,
    pub background: Rgba,
    /// Drawn in ascending `z`, then list order.
    pub layers: Vec<Layer>,
    pub captions: Vec<CaptionWord>,
    pub caption_style: CaptionStyle,
    pub audio: AudioPlan,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub start_s: f32,
    pub end_s: f32,
    pub z: i32,
    pub content: LayerContent,
    /// Box the content is laid out in (normalized to the frame).
    pub rect: RectF,
    pub fit: Fit,
    pub enter: Anim,
    pub exit: Anim,
    /// Continuous motion while on screen.
    pub motion: Motion,
    /// Rounded card with shadow (used for clips, memes, articles).
    #[serde(default)]
    pub card: Option<CardStyle>,
    #[serde(default = "one")]
    pub opacity: f32,
    /// Static crop of the source media (source-normalized) applied before
    /// fitting: a punch-in / re-framed cut of the same clip. Video, image and
    /// meme content only.
    #[serde(default)]
    pub crop: Option<RectF>,
}

fn one() -> f32 {
    1.0
}

impl Layer {
    /// A layer with no animation, motion, card or crop.
    pub fn new(start_s: f32, end_s: f32, z: i32, content: LayerContent, rect: RectF) -> Self {
        Self {
            start_s,
            end_s,
            z,
            content,
            rect,
            fit: Fit::Cover,
            enter: Anim::NONE,
            exit: Anim::NONE,
            motion: Motion::None,
            card: None,
            opacity: 1.0,
            crop: None,
        }
    }

    pub fn is_active(&self, t: f32) -> bool {
        t >= self.start_s && t < self.end_s
    }
}

impl SfxKind {
    pub const ALL: [SfxKind; 11] = [
        SfxKind::Whoosh,
        SfxKind::Swoosh,
        SfxKind::Pop,
        SfxKind::Click,
        SfxKind::Ding,
        SfxKind::Riser,
        SfxKind::Hit,
        SfxKind::Typewriter,
        SfxKind::Glitch,
        SfxKind::Camera,
        SfxKind::RecordScratch,
    ];

    /// snake_case name, as used in serde and for `<kind>*.wav` overrides.
    pub fn snake_name(self) -> &'static str {
        match self {
            SfxKind::Whoosh => "whoosh",
            SfxKind::Swoosh => "swoosh",
            SfxKind::Pop => "pop",
            SfxKind::Click => "click",
            SfxKind::Ding => "ding",
            SfxKind::Riser => "riser",
            SfxKind::Hit => "hit",
            SfxKind::Typewriter => "typewriter",
            SfxKind::Glitch => "glitch",
            SfxKind::Camera => "camera",
            SfxKind::RecordScratch => "record_scratch",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayerContent {
    /// Absolute path. Played from `trim_start_s`, looped if shorter than the layer.
    Video { path: String, trim_start_s: f32, #[serde(default)] looped: bool },
    /// Absolute path. `pan` animates the visible crop (source-normalized) over the layer's life.
    Image { path: String, #[serde(default)] pan: Option<Pan> },
    Text { text: String, style: TextStyle },
    /// Absolute path to an SVG (Iconify).
    Icon { path: String, #[serde(default)] tint: Option<Rgba> },
    Solid { color: Rgba },
    Gradient { from: Rgba, to: Rgba, #[serde(default)] angle_deg: f32 },
    /// Same video as a blurred, darkened full-frame backdrop.
    BlurredVideo { path: String, trim_start_s: f32, #[serde(default)] looped: bool },
    BlurredImage { path: String },
    /// Meme card: media (image, or a video when `video`) with a modern
    /// caption band above it (black text on white) and/or classic
    /// impact-style `top_text`/`bottom_text` drawn over the media.
    Meme {
        path: String,
        #[serde(default)]
        video: bool,
        #[serde(default)]
        trim_start_s: f32,
        #[serde(default)]
        caption: String,
        #[serde(default)]
        top_text: String,
        #[serde(default)]
        bottom_text: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Pan {
    pub from: RectF,
    pub to: RectF,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    #[default]
    Cover,
    Contain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FontKind {
    /// Condensed headline face (Anton).
    #[default]
    Display,
    /// Heavy grotesk (Archivo Black) for captions and memes.
    Heavy,
    /// Readable UI sans (Inter).
    Body,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextStyle {
    pub font: FontKind,
    /// Pixel size at the output resolution.
    pub size_px: f32,
    pub color: Rgba,
    #[serde(default)]
    pub stroke: Option<(Rgba, f32)>,
    /// Rounded pill/box behind the text.
    #[serde(default)]
    pub background: Option<Rgba>,
    #[serde(default)]
    pub align: Align,
    #[serde(default)]
    pub uppercase: bool,
    /// Words (case-insensitive) drawn in `highlight_color`.
    #[serde(default)]
    pub highlight_words: Vec<String>,
    #[serde(default)]
    pub highlight_color: Option<Rgba>,
    #[serde(default)]
    pub shadow: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CardStyle {
    /// Corner radius in output pixels.
    pub radius: f32,
    pub shadow: bool,
    #[serde(default)]
    pub border: Option<(Rgba, f32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnimKind {
    #[default]
    None,
    Fade,
    SlideUp,
    SlideDown,
    SlideLeft,
    SlideRight,
    /// Scale from 0 with overshoot.
    Pop,
    ZoomIn,
    ZoomOut,
    /// Fast horizontal slide with motion smear.
    Whip,
    /// Falls from above and bounces.
    Drop,
    Spin,
    /// Characters/words appear one by one (text only).
    Typewriter,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Anim {
    pub kind: AnimKind,
    pub duration_s: f32,
}

impl Anim {
    pub const NONE: Anim = Anim { kind: AnimKind::None, duration_s: 0.0 };
    pub fn new(kind: AnimKind, duration_s: f32) -> Self {
        Self { kind, duration_s }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Motion {
    #[default]
    None,
    /// Slow scale-up by `amount` (0.08 = 8%) over the layer's life.
    PushIn { amount: f32 },
    PullOut { amount: f32 },
    /// Horizontal drift as a fraction of the box width.
    Pan { dx: f32, dy: f32 },
    /// Gentle bob for icons/stickers.
    Float,
    /// Short punchy shake at the start (impact moments).
    Shake { intensity: f32 },
    /// Continuous slow rotation (degrees per second).
    Rotate { deg_per_s: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptionWord {
    pub text: String,
    pub start_s: f32,
    pub end_s: f32,
    /// Emphasized words get the accent color and a little pop.
    #[serde(default)]
    pub emphasis: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptionStyle {
    pub enabled: bool,
    pub font: FontKind,
    pub size_px: f32,
    pub color: Rgba,
    pub accent: Rgba,
    pub stroke: Rgba,
    /// Vertical center of the caption line, normalized.
    pub y: f32,
    /// Words shown together (2-4 reads best for shorts).
    pub max_words: usize,
    pub uppercase: bool,
    /// Color for emphasized words that are not currently spoken
    /// (defaults to `color`).
    #[serde(default)]
    pub emphasis_color: Option<Rgba>,
    #[serde(default)]
    pub preset: CaptionPreset,
}

/// How captions look and move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaptionPreset {
    /// Heavy uppercase, the spoken word pops in the accent color.
    #[default]
    Pop,
    /// Condensed uppercase, a bright box behind the spoken word.
    Hormozi,
    /// Two huge words at a time, thick outline, bouncing colors.
    Mrbeast,
    /// Small lowercase sans with a soft shadow, no motion.
    Minimal,
    /// A longer line that fills in word by word as it is spoken.
    Karaoke,
}

impl CaptionPreset {
    pub const ALL: [CaptionPreset; 5] =
        [CaptionPreset::Pop, CaptionPreset::Hormozi, CaptionPreset::Mrbeast, CaptionPreset::Minimal, CaptionPreset::Karaoke];

    pub fn slug(self) -> &'static str {
        match self {
            CaptionPreset::Pop => "pop",
            CaptionPreset::Hormozi => "hormozi",
            CaptionPreset::Mrbeast => "mrbeast",
            CaptionPreset::Minimal => "minimal",
            CaptionPreset::Karaoke => "karaoke",
        }
    }

    /// Font, size multiplier, words per group and uppercase for this look.
    pub fn shape(self) -> (FontKind, f32, usize, bool) {
        match self {
            CaptionPreset::Pop => (FontKind::Heavy, 1.0, 3, true),
            CaptionPreset::Hormozi => (FontKind::Display, 1.1, 3, true),
            CaptionPreset::Mrbeast => (FontKind::Display, 1.3, 2, true),
            CaptionPreset::Minimal => (FontKind::Body, 0.7, 5, false),
            CaptionPreset::Karaoke => (FontKind::Heavy, 0.8, 5, true),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPlan {
    /// Absolute path to the narration WAV.
    pub voice_path: Option<String>,
    pub voice_gain: f32,
    pub sfx: Vec<SfxCue>,
    pub music: Option<MusicBed>,
    /// Original audio of clips that should be heard (rare; e.g. a famous line).
    #[serde(default)]
    pub clip_audio: Vec<ClipAudio>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SfxCue {
    pub kind: SfxKind,
    pub at_s: f32,
    pub gain: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MusicBed {
    pub path: String,
    pub gain: f32,
    /// Gain multiplier applied while narration is speaking.
    pub duck_to: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipAudio {
    pub path: String,
    pub trim_start_s: f32,
    pub at_s: f32,
    pub duration_s: f32,
    pub gain: f32,
}

//! Text: embedded fonts, glyph outlines -> tiny-skia paths, word wrapping
//! with auto-shrink, and drawing with fill/stroke/shadow/pill/highlights.

use std::sync::LazyLock;

use ab_glyph::{Font, FontRef, GlyphId, OutlineCurve, VariableFont};
use resvg::tiny_skia::{self as sk, LineCap, LineJoin, Path, PathBuilder, Pixmap, Stroke, Transform};

use clipcut_core::timeline::{Align, FontKind, Rgba};

use crate::util::{color, color_alpha, paint, rounded_rect};

static ANTON: &[u8] = include_bytes!("../../../assets/fonts/Anton-Regular.ttf");
static ARCHIVO: &[u8] = include_bytes!("../../../assets/fonts/ArchivoBlack-Regular.ttf");
static INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");

pub struct FontFace {
    pub font: FontRef<'static>,
    /// Pixels per font unit at size 1.
    upem: f32,
    /// Cap height / em.
    pub cap: f32,
    /// Line advance / em.
    pub line: f32,
    /// Extra tracking / em.
    pub tracking: f32,
}

impl FontFace {
    fn new(data: &'static [u8], line: f32, tracking: f32, wght: Option<f32>) -> Self {
        let mut font = FontRef::try_from_slice(data).expect("embedded font is valid");
        if let Some(w) = wght {
            font.set_variation(b"wght", w);
        }
        let upem = font.units_per_em().unwrap_or(1000.0);
        let cap = font
            .outline(font.glyph_id('H'))
            .map(|o| o.bounds.max.y / upem)
            .unwrap_or(0.7);
        Self { font, upem, cap, line, tracking }
    }

    pub fn scale(&self, size: f32) -> f32 {
        size / self.upem
    }

    /// Advance width of `text` at `size` (with kerning and tracking).
    pub fn measure(&self, text: &str, size: f32) -> f32 {
        let s = self.scale(size);
        let mut x = 0.0;
        let mut prev: Option<GlyphId> = None;
        for c in text.chars() {
            let id = self.font.glyph_id(c);
            if let Some(p) = prev {
                x += self.font.kern_unscaled(p, id) * s;
            }
            x += self.font.h_advance_unscaled(id) * s + self.tracking * size;
            prev = Some(id);
        }
        x - if text.is_empty() { 0.0 } else { self.tracking * size }
    }

    /// Append glyph outlines of `text` to `pb` with the pen at (x, baseline).
    pub fn append_path(&self, pb: &mut PathBuilder, text: &str, size: f32, x: f32, baseline: f32) {
        let s = self.scale(size);
        let mut pen = x;
        let mut prev: Option<GlyphId> = None;
        for c in text.chars() {
            let id = self.font.glyph_id(c);
            if let Some(p) = prev {
                pen += self.font.kern_unscaled(p, id) * s;
            }
            if let Some(outline) = self.font.outline(id) {
                let map = |p: ab_glyph::Point| (pen + p.x * s, baseline - p.y * s);
                let mut last: Option<(f32, f32)> = None;
                let mut open = false;
                for curve in &outline.curves {
                    let (start, end) = match curve {
                        OutlineCurve::Line(a, b) => (map(*a), map(*b)),
                        OutlineCurve::Quad(a, _, b) => (map(*a), map(*b)),
                        OutlineCurve::Cubic(a, _, _, b) => (map(*a), map(*b)),
                    };
                    let joined = last.is_some_and(|l| (l.0 - start.0).abs() < 1e-3 && (l.1 - start.1).abs() < 1e-3);
                    if !joined {
                        if open {
                            pb.close();
                        }
                        pb.move_to(start.0, start.1);
                        open = true;
                    }
                    match curve {
                        OutlineCurve::Line(_, _) => pb.line_to(end.0, end.1),
                        OutlineCurve::Quad(_, c1, _) => {
                            let c = map(*c1);
                            pb.quad_to(c.0, c.1, end.0, end.1)
                        }
                        OutlineCurve::Cubic(_, c1, c2, _) => {
                            let (a, b) = (map(*c1), map(*c2));
                            pb.cubic_to(a.0, a.1, b.0, b.1, end.0, end.1)
                        }
                    }
                    last = Some(end);
                }
                if open {
                    pb.close();
                }
            }
            pen += self.font.h_advance_unscaled(id) * s + self.tracking * size;
            prev = Some(id);
        }
    }

    pub fn path(&self, text: &str, size: f32, x: f32, baseline: f32) -> Option<Path> {
        let mut pb = PathBuilder::new();
        self.append_path(&mut pb, text, size, x, baseline);
        pb.finish()
    }
}

pub struct Fonts {
    pub display: FontFace,
    pub heavy: FontFace,
    pub body: FontFace,
}

pub static FONTS: LazyLock<Fonts> = LazyLock::new(|| Fonts {
    display: FontFace::new(ANTON, 1.04, 0.01, None),
    heavy: FontFace::new(ARCHIVO, 1.14, 0.0, None),
    body: FontFace::new(INTER, 1.22, 0.0, Some(750.0)),
});

pub fn face(kind: FontKind) -> &'static FontFace {
    match kind {
        FontKind::Display => &FONTS.display,
        FontKind::Heavy => &FONTS.heavy,
        FontKind::Body => &FONTS.body,
    }
}

/// Lowercased alphanumerics, for matching highlight words.
pub fn norm(word: &str) -> String {
    word.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

#[derive(Debug, Clone)]
pub struct LWord {
    pub text: String,
    pub width: f32,
}

#[derive(Debug, Clone)]
pub struct LLine {
    /// (word index, x offset from line start)
    pub words: Vec<(usize, f32)>,
    pub width: f32,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub kind: FontKind,
    pub size: f32,
    pub words: Vec<LWord>,
    pub lines: Vec<LLine>,
    pub line_h: f32,
    pub cap_h: f32,
    /// Visual block height: cap height of the first line + line advances.
    pub height: f32,
    pub width: f32,
}

fn wrap(face: &FontFace, words: &[String], size: f32, max_w: f32) -> (Vec<LWord>, Vec<LLine>) {
    let space = face.measure(" ", size).max(size * 0.22);
    let lw: Vec<LWord> = words.iter().map(|w| LWord { text: w.clone(), width: face.measure(w, size) }).collect();
    let mut lines: Vec<LLine> = Vec::new();
    let mut cur = LLine { words: vec![], width: 0.0 };
    for (i, w) in lw.iter().enumerate() {
        let add = if cur.words.is_empty() { w.width } else { space + w.width };
        if !cur.words.is_empty() && cur.width + add > max_w {
            lines.push(std::mem::replace(&mut cur, LLine { words: vec![], width: 0.0 }));
            cur.words.push((i, 0.0));
            cur.width = w.width;
        } else {
            let x = if cur.words.is_empty() { 0.0 } else { cur.width + space };
            cur.words.push((i, x));
            cur.width += add;
        }
    }
    if !cur.words.is_empty() {
        lines.push(cur);
    }
    (lw, lines)
}

/// Lay out `text` inside `max_w` x `max_h`, shrinking from `size` until it
/// fits. Balances 2-line layouts so a lone word doesn't dangle.
pub fn layout(kind: FontKind, text: &str, size: f32, max_w: f32, max_h: f32, uppercase: bool) -> Layout {
    let face = face(kind);
    let text = if uppercase { text.to_uppercase() } else { text.to_string() };
    let words: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    let mut size = size.max(4.0);
    loop {
        let (lw, mut lines) = wrap(face, &words, size, max_w);
        // Balance: try a narrower width so lines end up more even.
        if lines.len() == 2 {
            let total: f32 = lines.iter().map(|l| l.width).sum();
            let (_, balanced) = wrap(face, &words, size, (total / 2.0 + size * 1.2).min(max_w));
            if balanced.len() == 2 {
                lines = balanced;
            }
        }
        let line_h = size * face.line;
        let cap_h = size * face.cap;
        let n = lines.len().max(1) as f32;
        let height = cap_h + (n - 1.0) * line_h;
        let width = lines.iter().map(|l| l.width).fold(0.0, f32::max);
        let widest_word = lw.iter().map(|w| w.width).fold(0.0, f32::max);
        if (height <= max_h && widest_word <= max_w && width <= max_w) || size <= 8.0 {
            return Layout { kind, size, words: lw, lines, line_h, cap_h, height, width };
        }
        size *= 0.93;
    }
}

pub struct DrawOpts<'a> {
    pub align: Align,
    pub stroke: Option<(Rgba, f32)>,
    pub shadow: bool,
    pub pill: Option<Rgba>,
    /// Per-word color.
    pub color_of: &'a dyn Fn(usize) -> Rgba,
    /// Draw only the first N characters (typewriter).
    pub reveal_chars: Option<usize>,
    pub opacity: f32,
}

/// Draw a layout with its block's top-left at (x, y), aligned inside `box_w`.
pub fn draw_layout(px: &mut Pixmap, lay: &Layout, x: f32, y: f32, box_w: f32, opts: &DrawOpts, transform: Transform) {
    let face = face(lay.kind);
    let mut budget = opts.reveal_chars.unwrap_or(usize::MAX);
    // (path, word index)
    let mut glyphs: Vec<(Path, usize)> = Vec::new();
    let mut pills: Vec<Path> = Vec::new();
    for (li, line) in lay.lines.iter().enumerate() {
        let lx = match opts.align {
            Align::Left => x,
            Align::Center => x + (box_w - line.width) / 2.0,
            Align::Right => x + box_w - line.width,
        };
        let baseline = y + lay.cap_h + li as f32 * lay.line_h;
        if opts.pill.is_some() && budget > 0 {
            let pad_x = lay.size * 0.32;
            let pad_y = lay.size * 0.2;
            let top = baseline - lay.cap_h - pad_y;
            let h = lay.cap_h + pad_y * 2.0;
            if let Some(p) = rounded_rect(lx - pad_x, top, line.width + pad_x * 2.0, h, lay.size * 0.2) {
                pills.push(p);
            }
        }
        for &(wi, wx) in &line.words {
            if budget == 0 {
                break;
            }
            let word = &lay.words[wi].text;
            let n = word.chars().count();
            let shown: String = if budget >= n { word.clone() } else { word.chars().take(budget).collect() };
            budget = budget.saturating_sub(n + 1);
            if let Some(path) = face.path(&shown, lay.size, lx + wx, baseline) {
                glyphs.push((path, wi));
            }
        }
    }
    let op = opts.opacity;
    if let Some(bg) = opts.pill {
        for p in &pills {
            if opts.shadow {
                let t = transform.pre_translate(0.0, lay.size * 0.06);
                px.fill_path(p, &paint(color_alpha(Rgba(0, 0, 0, 110), op)), sk::FillRule::Winding, t, None);
            }
            px.fill_path(p, &paint(color_alpha(bg, op)), sk::FillRule::Winding, transform, None);
        }
    }
    let stroke_w = opts.stroke.map(|(_, w)| w).unwrap_or(0.0);
    if opts.shadow {
        let off = (lay.size * 0.055).max(2.0);
        let t = transform.pre_translate(off * 0.3, off);
        let sp = paint(color_alpha(Rgba(0, 0, 0, 120), op));
        for (p, _) in &glyphs {
            px.fill_path(p, &sp, sk::FillRule::Winding, t, None);
            if stroke_w > 0.0 {
                px.stroke_path(p, &sp, &stroke(stroke_w), t, None);
            }
        }
    }
    if let Some((sc, w)) = opts.stroke {
        let sp = paint(color_alpha(sc, op));
        for (p, _) in &glyphs {
            px.stroke_path(p, &sp, &stroke(w), transform, None);
        }
    }
    for (p, wi) in &glyphs {
        let c = (opts.color_of)(*wi);
        px.fill_path(p, &paint(color_alpha(c, op)), sk::FillRule::Winding, transform, None);
    }
}

pub fn stroke(width: f32) -> Stroke {
    Stroke { width: width * 2.0, line_join: LineJoin::Round, line_cap: LineCap::Round, ..Stroke::default() }
}

/// Total characters drawn by a typewriter for this layout (spaces count once).
pub fn char_count(lay: &Layout) -> usize {
    lay.words.iter().map(|w| w.text.chars().count() + 1).sum::<usize>().saturating_sub(1)
}

pub fn solid(c: Rgba) -> sk::Color {
    color(c)
}

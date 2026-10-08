//! The compositor: prepares static resources for a timeline (decoded images,
//! pre-rendered text/icons/memes, card masks and shadows) and draws frames.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use resvg::tiny_skia::{
    self as sk, FilterQuality, GradientStop, LinearGradient, Mask, MaskType, Pixmap, PixmapPaint,
    Point, SpreadMode, Transform,
};
use resvg::usvg;

use clipcut_core::timeline::{
    Align, Anim, AnimKind, CardStyle, Fit, FontKind, Layer, LayerContent, Motion, RectF, Rgba,
    TextStyle, Timeline,
};

use crate::control::Control;
use crate::ffmpeg::{VideoSpec, video_filter};
use crate::text::{self, DrawOpts, Layout, norm};
use crate::util::{
    self, blur_pixmap, bounded_crop, clamp01, color, color_alpha, darken, ease, lerp, noise1,
    paint, rounded_rect, unpremultiply,
};

/// Backdrop videos are decoded at 1/BACKDROP_DIV resolution and upscaled.
const BACKDROP_DIV: u32 = 8;

/// A pre-rendered bitmap positioned relative to its layer box: drawn at
/// box-local (-pad, -pad).
pub struct Sprite {
    pub px: Pixmap,
    pub pad: f32,
}

enum Prep {
    None,
    Image(Arc<Pixmap>),
    Backdrop(Arc<Pixmap>),
    Sprite(Arc<Sprite>),
    Typewriter(Layout),
    Meme(Arc<MemeLayout>, Option<Arc<Pixmap>>),
}

struct CardRes {
    mask: Mask,
    shadow: Option<(Pixmap, f32)>,
}

pub struct CapGroup {
    pub words: Vec<usize>,
    pub start: f32,
    pub end: f32,
}

pub struct Scene {
    pub tl: Timeline,
    pub w: u32,
    pub h: u32,
    order: Vec<usize>,
    prep: Vec<Prep>,
    cards: Vec<Option<Arc<CardRes>>>,
    pub specs: Vec<VideoSpec>,
    groups: Vec<CapGroup>,
    typewriter_cache: Mutex<HashMap<(usize, usize), Arc<Sprite>>>,
}

fn box_px(r: RectF, w: u32, h: u32) -> (f32, f32, u32, u32) {
    let bw = (r.w * w as f32).round().max(1.0) as u32;
    let bh = (r.h * h as f32).round().max(1.0) as u32;
    ((r.x * w as f32).round(), (r.y * h as f32).round(), bw, bh)
}

fn is_media(c: &LayerContent) -> bool {
    matches!(
        c,
        LayerContent::Video { .. }
            | LayerContent::Image { .. }
            | LayerContent::BlurredVideo { .. }
            | LayerContent::BlurredImage { .. }
            | LayerContent::Meme { .. }
    )
}

impl Scene {
    /// Prepare every layer (or only those active at `only_at`, for stills).
    pub fn new(tl: &Timeline, only_at: Option<f32>, control: &Control) -> anyhow::Result<Self> {
        control.check()?;
        let tl = tl.clone();
        let (w, h) = (tl.width.max(2), tl.height.max(2));
        let mut order: Vec<usize> = (0..tl.layers.len()).collect();
        order.sort_by_key(|&i| tl.layers[i].z);
        let wanted = |l: &Layer| only_at.is_none_or(|t| l.is_active(t));

        // Decode each distinct image once.
        let mut img_paths: Vec<String> = Vec::new();
        for l in tl.layers.iter().filter(|l| wanted(l)) {
            match &l.content {
                LayerContent::Image { path, .. } | LayerContent::BlurredImage { path } => {
                    img_paths.push(path.clone())
                }
                LayerContent::Meme {
                    path, video: false, ..
                } => img_paths.push(path.clone()),
                _ => {}
            }
        }
        img_paths.sort();
        img_paths.dedup();
        let images: HashMap<String, Option<Arc<image::RgbaImage>>> = img_paths
            .par_iter()
            .map(|p| {
                control.check()?;
                let image = load_image(Path::new(p)).map(Arc::new);
                control.check()?;
                anyhow::ensure!(image.is_some(), "cannot load image {p}");
                Ok((p.clone(), image))
            })
            .collect::<anyhow::Result<_>>()?;

        // Blurred backdrops are shared per path.
        let backdrop_paths: Vec<String> = tl
            .layers
            .iter()
            .filter(|l| wanted(l))
            .filter_map(|l| match &l.content {
                LayerContent::BlurredImage { path } => Some(path.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let backdrops: HashMap<String, Arc<Pixmap>> = backdrop_paths
            .par_iter()
            .map(|p| {
                control.check()?;
                let img = images[p].as_ref().expect("validated image");
                let backdrop = Arc::new(blurred_backdrop(img, w, h));
                control.check()?;
                Ok((p.clone(), backdrop))
            })
            .collect::<anyhow::Result<_>>()?;

        let prep: Vec<Prep> = tl
            .layers
            .par_iter()
            .map(|l| {
                control.check()?;
                let prepared = if wanted(l) {
                    prepare_layer(l, w, h, &images, &backdrops)
                } else {
                    Prep::None
                };
                control.check()?;
                if wanted(l)
                    && matches!(l.content, LayerContent::Icon { .. })
                    && matches!(prepared, Prep::None)
                {
                    anyhow::bail!("cannot load SVG icon");
                }
                Ok(prepared)
            })
            .collect::<anyhow::Result<_>>()?;

        // Card masks/shadows per distinct (w, h, radius).
        let mut card_cache: HashMap<(u32, u32, u32, bool), Arc<CardRes>> = HashMap::new();
        let mut cards = Vec::with_capacity(tl.layers.len());
        for l in &tl.layers {
            control.check()?;
            let res = l.card.filter(|_| wanted(l)).map(|c| {
                let (_, _, bw, bh) = box_px(l.rect, w, h);
                card_cache
                    .entry((bw, bh, c.radius.round() as u32, c.shadow))
                    .or_insert_with(|| Arc::new(card_res(bw, bh, c)))
                    .clone()
            });
            cards.push(res);
        }

        let fps = tl.fps.max(1);
        let mut specs = Vec::new();
        for (i, l) in tl.layers.iter().enumerate() {
            if !wanted(l) {
                continue;
            }
            let (_, _, bw, bh) = box_px(l.rect, w, h);
            let spec = match &l.content {
                LayerContent::Video {
                    path,
                    trim_start_s,
                    looped,
                } => Some(VideoSpec {
                    layer: i,
                    path: path.clone(),
                    trim_start_s: *trim_start_s,
                    looped: *looped,
                    width: bw,
                    height: bh,
                    filter: video_filter(bw, bh, fps, l.crop, l.fit == Fit::Contain, false),
                    start_s: l.start_s,
                    end_s: l.end_s,
                }),
                LayerContent::BlurredVideo {
                    path,
                    trim_start_s,
                    looped,
                } => {
                    let (sw, sh) = ((bw / BACKDROP_DIV).max(8), (bh / BACKDROP_DIV).max(8));
                    Some(VideoSpec {
                        layer: i,
                        path: path.clone(),
                        trim_start_s: *trim_start_s,
                        looped: *looped,
                        width: sw,
                        height: sh,
                        filter: video_filter(sw, sh, fps, l.crop, false, true),
                        start_s: l.start_s,
                        end_s: l.end_s,
                    })
                }
                LayerContent::Meme {
                    path,
                    video: true,
                    trim_start_s,
                    ..
                } => {
                    let Prep::Meme(m, _) = &prep[i] else { continue };
                    let (mw, mh) = (
                        m.img.2.round().max(2.0) as u32,
                        m.img.3.round().max(2.0) as u32,
                    );
                    Some(VideoSpec {
                        layer: i,
                        path: path.clone(),
                        trim_start_s: *trim_start_s,
                        looped: true,
                        width: mw,
                        height: mh,
                        filter: video_filter(mw, mh, fps, l.crop, false, false),
                        start_s: l.start_s,
                        end_s: l.end_s,
                    })
                }
                _ => None,
            };
            specs.extend(spec);
        }

        let groups = group_captions(&tl);
        control.check()?;
        Ok(Scene {
            tl,
            w,
            h,
            order,
            prep,
            cards,
            specs,
            groups,
            typewriter_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn frame_count(&self) -> usize {
        (self.tl.duration_s * self.tl.fps.max(1) as f32)
            .ceil()
            .max(1.0) as usize
    }

    /// Draw the frame at `t`. `video` holds the current decoded frame per layer index.
    pub fn draw(&self, t: f32, video: &HashMap<usize, Arc<Pixmap>>) -> Pixmap {
        let mut fb = Pixmap::new(self.w, self.h).expect("frame size");
        fb.fill(color(self.tl.background));
        for &i in &self.order {
            let l = &self.tl.layers[i];
            if !l.is_active(t) {
                continue;
            }
            self.draw_layer(&mut fb, i, l, t, video);
        }
        if self.tl.caption_style.enabled {
            self.draw_captions(&mut fb, t);
        }
        fb
    }

    fn draw_layer(
        &self,
        fb: &mut Pixmap,
        i: usize,
        l: &Layer,
        t: f32,
        video: &HashMap<usize, Arc<Pixmap>>,
    ) {
        let (w, h) = (self.w as f32, self.h as f32);
        let local = t - l.start_s;
        let life = (l.end_s - l.start_s).max(1e-3);
        let st = anim_state(l, local, w, h);
        let media = is_media(&l.content);
        let mo = motion_state(&l.motion, local, life, i as u32, media, w, h);
        let opacity = (l.opacity * st.opacity).clamp(0.0, 1.0);
        if opacity <= 0.002 {
            return;
        }
        let (bx, by, bw, bh) = box_px(l.rect, self.w, self.h);
        match &l.content {
            LayerContent::Solid { color: c } => {
                let t = box_transform(bx, by, bw as f32, bh as f32, &st, &mo);
                if let Some(r) = sk::Rect::from_xywh(0.0, 0.0, bw as f32, bh as f32) {
                    fb.fill_rect(r, &paint(color_alpha(*c, opacity)), t, None);
                }
            }
            LayerContent::Gradient {
                from,
                to,
                angle_deg,
            } => {
                let angle = (angle_deg + mo.rot).to_radians();
                let (cx, cy) = (bw as f32 / 2.0, bh as f32 / 2.0);
                let half = (bw as f32 * angle.cos().abs() + bh as f32 * angle.sin().abs()) / 2.0;
                let (dx, dy) = (angle.cos() * half, angle.sin() * half);
                let stops = vec![
                    GradientStop::new(0.0, color_alpha(*from, opacity)),
                    GradientStop::new(1.0, color_alpha(*to, opacity)),
                ];
                if let Some(shader) = LinearGradient::new(
                    Point::from_xy(cx - dx, cy - dy),
                    Point::from_xy(cx + dx, cy + dy),
                    stops,
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    let mut p = sk::Paint::default();
                    p.shader = shader;
                    let mut st2 = st;
                    st2.rot = 0.0;
                    let mut mo2 = mo;
                    mo2.rot = 0.0;
                    let t = box_transform(bx, by, bw as f32, bh as f32, &st2, &mo2);
                    if let Some(r) = sk::Rect::from_xywh(0.0, 0.0, bw as f32, bh as f32) {
                        fb.fill_rect(r, &p, t, None);
                    }
                }
            }
            LayerContent::Video { .. } | LayerContent::BlurredVideo { .. } => {
                let Some(frame) = video.get(&i) else {
                    self.placeholder(fb, l, bx, by, bw, bh, &st, &mo, opacity, i);
                    return;
                };
                let src =
                    sk::Rect::from_xywh(0.0, 0.0, frame.width() as f32, frame.height() as f32)
                        .unwrap();
                self.draw_media(fb, i, l, frame, src, bx, by, bw, bh, &st, &mo, opacity);
            }
            LayerContent::Image { pan, .. } => {
                let Prep::Image(img) = &self.prep[i] else {
                    self.placeholder(fb, l, bx, by, bw, bh, &st, &mo, opacity, i);
                    return;
                };
                let (iw, ih) = (img.width() as f32, img.height() as f32);
                let view = match pan {
                    Some(p) => {
                        let u = ease::in_out_cubic(clamp01(local / life));
                        RectF::new(
                            lerp(p.from.x, p.to.x, u),
                            lerp(p.from.y, p.to.y, u),
                            lerp(p.from.w, p.to.w, u),
                            lerp(p.from.h, p.to.h, u),
                        )
                    }
                    None => l.crop.unwrap_or(RectF::FULL),
                };
                let view = bounded_crop(view);
                let src = sk::Rect::from_xywh(
                    view.x * iw,
                    view.y * ih,
                    (view.w * iw).max(1.0),
                    (view.h * ih).max(1.0),
                )
                .unwrap_or(sk::Rect::from_xywh(0.0, 0.0, iw, ih).unwrap());
                self.draw_media(fb, i, l, img, src, bx, by, bw, bh, &st, &mo, opacity);
            }
            LayerContent::BlurredImage { .. } => {
                let Prep::Backdrop(img) = &self.prep[i] else {
                    return;
                };
                let src =
                    sk::Rect::from_xywh(0.0, 0.0, img.width() as f32, img.height() as f32).unwrap();
                self.draw_media(fb, i, l, img, src, bx, by, bw, bh, &st, &mo, opacity);
            }
            LayerContent::Text { .. } | LayerContent::Icon { .. } => {
                let sprite = match &self.prep[i] {
                    Prep::Sprite(s) => s.clone(),
                    Prep::Typewriter(lay) => self.typewriter_sprite(i, l, lay, local),
                    _ => return,
                };
                let t = box_transform(bx, by, bw as f32, bh as f32, &st, &mo)
                    .pre_translate(-sprite.pad, -sprite.pad);
                draw_sprite(fb, &sprite.px, opacity, t, &st);
            }
            LayerContent::Meme { .. } => {
                let Prep::Meme(m, img) = &self.prep[i] else {
                    return;
                };
                let media = video.get(&i).cloned().or_else(|| img.clone());
                let mut boxpx = Pixmap::new(bw, bh).unwrap();
                // Video crops were applied by ffmpeg; image crops are applied here.
                let crop = if video.contains_key(&i) { None } else { l.crop };
                render_meme(&mut boxpx, m, media.as_deref(), crop, l.fit, &mo);
                if let Some(card) = &self.cards[i] {
                    boxpx.apply_mask(&card.mask);
                }
                self.composite_box(fb, i, l, &boxpx, bx, by, bw, bh, &st, &mo, opacity);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn placeholder(
        &self,
        fb: &mut Pixmap,
        l: &Layer,
        bx: f32,
        by: f32,
        bw: u32,
        bh: u32,
        st: &AnimState,
        mo: &MotionState,
        opacity: f32,
        i: usize,
    ) {
        if matches!(
            l.content,
            LayerContent::BlurredVideo { .. } | LayerContent::BlurredImage { .. }
        ) {
            return;
        }
        let mut boxpx = Pixmap::new(bw, bh).unwrap();
        boxpx.fill(sk::Color::from_rgba8(28, 28, 34, 255));
        if let Some(card) = &self.cards[i] {
            boxpx.apply_mask(&card.mask);
        }
        self.composite_box(fb, i, l, &boxpx, bx, by, bw, bh, st, mo, opacity);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_media(
        &self,
        fb: &mut Pixmap,
        i: usize,
        l: &Layer,
        src: &Pixmap,
        view: sk::Rect,
        bx: f32,
        by: f32,
        bw: u32,
        bh: u32,
        st: &AnimState,
        mo: &MotionState,
        opacity: f32,
    ) {
        let (fw, fh) = (bw as f32, bh as f32);
        let base = match l.fit {
            Fit::Cover => (fw / view.width()).max(fh / view.height()),
            Fit::Contain => (fw / view.width()).min(fh / view.height()),
        };
        let s = base * mo.zoom;
        let content = Transform::from_translate(fw / 2.0 + mo.pan_x * fw, fh / 2.0 + mo.pan_y * fh)
            .pre_scale(s, s)
            .pre_translate(
                -(view.x() + view.width() / 2.0),
                -(view.y() + view.height() / 2.0),
            );
        let quality = if (s - 1.0).abs() < 1e-3 && is_integer(content.tx) && is_integer(content.ty)
        {
            FilterQuality::Nearest
        } else {
            FilterQuality::Bilinear
        };
        let full = bx <= 0.5 && by <= 0.5 && bw >= self.w && bh >= self.h;
        let plain = st.scale == 1.0
            && st.rot == 0.0
            && st.dx == 0.0
            && st.dy == 0.0
            && mo.dx == 0.0
            && mo.dy == 0.0
            && mo.rot == 0.0
            && mo.box_scale == 1.0;
        if full && l.card.is_none() && plain && l.fit == Fit::Cover {
            // Full-bleed: draw straight into the frame.
            let paint = PixmapPaint {
                opacity,
                quality,
                ..PixmapPaint::default()
            };
            fb.draw_pixmap(
                0,
                0,
                src.as_ref(),
                &paint,
                content.post_translate(bx, by),
                None,
            );
            return;
        }
        let mut boxpx = Pixmap::new(bw, bh).unwrap();
        if l.fit == Fit::Contain || l.card.is_some() {
            boxpx.fill(sk::Color::from_rgba8(12, 12, 14, 255));
        }
        let paint = PixmapPaint {
            opacity: 1.0,
            quality,
            ..PixmapPaint::default()
        };
        boxpx.draw_pixmap(0, 0, src.as_ref(), &paint, content, None);
        if let Some(card) = &self.cards[i] {
            boxpx.apply_mask(&card.mask);
            if let Some(cs) = l.card
                && let Some((c, bwid)) = cs.border
                && let Some(path) =
                    rounded_rect(bwid / 2.0, bwid / 2.0, fw - bwid, fh - bwid, cs.radius)
            {
                let stroke = sk::Stroke {
                    width: bwid,
                    ..sk::Stroke::default()
                };
                boxpx.stroke_path(&path, &paint_c(c), &stroke, Transform::identity(), None);
            }
        }
        self.composite_box(fb, i, l, &boxpx, bx, by, bw, bh, st, mo, opacity);
    }

    #[allow(clippy::too_many_arguments)]
    fn composite_box(
        &self,
        fb: &mut Pixmap,
        i: usize,
        _l: &Layer,
        boxpx: &Pixmap,
        bx: f32,
        by: f32,
        bw: u32,
        bh: u32,
        st: &AnimState,
        mo: &MotionState,
        opacity: f32,
    ) {
        let t = box_transform(bx, by, bw as f32, bh as f32, st, mo);
        if let Some(card) = &self.cards[i]
            && let Some((shadow, pad)) = &card.shadow
        {
            let off = (bh as f32 * 0.025).clamp(6.0, 22.0);
            let paint = PixmapPaint {
                opacity: opacity * 0.75,
                quality: FilterQuality::Bilinear,
                ..PixmapPaint::default()
            };
            fb.draw_pixmap(
                0,
                0,
                shadow.as_ref(),
                &paint,
                t.pre_translate(-pad, -pad + off),
                None,
            );
        }
        draw_sprite(fb, boxpx, opacity, t, st);
    }

    fn typewriter_sprite(&self, i: usize, l: &Layer, lay: &Layout, local: f32) -> Arc<Sprite> {
        let total = text::char_count(lay);
        let dur = l.enter.duration_s.max(0.05);
        let n = ((local / dur).clamp(0.0, 1.0) * total as f32).ceil() as usize;
        let key = (i, n);
        if let Some(s) = self.typewriter_cache.lock().unwrap().get(&key) {
            return s.clone();
        }
        let LayerContent::Text { style, .. } = &l.content else {
            unreachable!()
        };
        let (_, _, bw, bh) = box_px(l.rect, self.w, self.h);
        let s = Arc::new(render_text_sprite(lay, style, bw, bh, Some(n)));
        let mut cache = self.typewriter_cache.lock().unwrap();
        // Reuse within adjacent frames without retaining every reveal sprite
        // for an entire long-form render (each 1080p sprite can be megabytes).
        let bytes: usize = cache.values().map(|v| v.px.data().len()).sum();
        if bytes + s.px.data().len() > 32 * 1024 * 1024 {
            cache.clear();
        }
        cache.insert(key, s.clone());
        s
    }

    fn draw_captions(&self, fb: &mut Pixmap, t: f32) {
        let Some(g) = self.groups.iter().find(|g| t >= g.start && t < g.end) else {
            return;
        };
        let style = &self.tl.caption_style;
        let face = text::face(style.font);
        let words: Vec<String> = g
            .words
            .iter()
            .map(|&wi| {
                let w = caption_text(&self.tl.captions[wi].text);
                if style.uppercase { w.to_uppercase() } else { w }
            })
            .collect();
        let mut size = style.size_px;
        let max_w = self.w as f32 * 0.86;
        let space = |size: f32| size * 0.26;
        let mut widths: Vec<f32> = words.iter().map(|w| face.measure(w, size)).collect();
        let total =
            widths.iter().sum::<f32>() + space(size) * (words.len().saturating_sub(1)) as f32;
        if total > max_w {
            let k = max_w / total;
            size *= k;
            widths.iter_mut().for_each(|w| *w *= k);
        }
        let total =
            widths.iter().sum::<f32>() + space(size) * (words.len().saturating_sub(1)) as f32;
        let cy = style.y * self.h as f32;
        let baseline = cy + size * face.cap / 2.0;
        let cx = self.w as f32 / 2.0;
        let gp = clamp01((t - g.start) / 0.13);
        let gs = 0.8 + 0.2 * ease::out_back(gp);
        let group_t = Transform::from_translate(cx, cy)
            .pre_scale(gs, gs)
            .pre_translate(-cx, -cy);
        let active = g.words.iter().rposition(|&wi| {
            let word = &self.tl.captions[wi];
            word.start_s <= t && t < word.end_s
        });
        let stroke_w = size * 0.12;
        let mut x = cx - total / 2.0;
        let mut items = Vec::new();
        for (k, &wi) in g.words.iter().enumerate() {
            let cw = &self.tl.captions[wi];
            let wcx = x + widths[k] / 2.0;
            let is_active = Some(k) == active;
            let mut ws = if cw.emphasis { 1.06 } else { 1.0 };
            if is_active {
                let lt = t - cw.start_s;
                ws *= if lt < 0.07 {
                    lerp(1.0, 1.17, lt / 0.07)
                } else {
                    lerp(1.17, 1.1, clamp01((lt - 0.07) / 0.1))
                };
            }
            let col = if is_active {
                style.accent
            } else if cw.emphasis {
                style.emphasis_color.unwrap_or(style.color)
            } else {
                style.color
            };
            let wt = group_t
                .pre_translate(wcx, cy)
                .pre_scale(ws, ws)
                .pre_translate(-wcx, -cy);
            if let Some(path) = face.path(&words[k], size, x, baseline) {
                items.push((is_active, path, wt, col));
            }
            x += widths[k] + space(size);
        }
        // Inactive words first, the active (scaled) word on top.
        items.sort_by_key(|i| i.0);
        let shadow = paint(sk::Color::from_rgba8(0, 0, 0, 120));
        let stroke_paint = paint(color(style.stroke));
        for (_, path, wt, col) in &items {
            let st = wt.pre_translate(0.0, size * 0.07);
            fb.stroke_path(path, &shadow, &text::stroke(stroke_w), st, None);
            fb.stroke_path(path, &stroke_paint, &text::stroke(stroke_w), *wt, None);
            fb.fill_path(path, &paint(color(*col)), sk::FillRule::Winding, *wt, None);
        }
    }
}

fn paint_c(c: Rgba) -> sk::Paint<'static> {
    paint(color(c))
}

fn is_integer(v: f32) -> bool {
    (v - v.round()).abs() < 1e-3
}

/// Clean a spoken word for display: drop surrounding punctuation except ?/!.
pub fn caption_text(w: &str) -> String {
    let t = w.trim_matches(|c: char| {
        !c.is_alphanumeric() && c != '?' && c != '!' && c != '%' && c != '$' && c != '\''
    });
    let t = t.trim_start_matches(['?', '!', '\'']);
    t.trim_end_matches('\'').to_string()
}

fn draw_sprite(fb: &mut Pixmap, px: &Pixmap, opacity: f32, t: Transform, st: &AnimState) {
    let exact = (t.sx - 1.0).abs() < 1e-4
        && (t.sy - 1.0).abs() < 1e-4
        && t.kx.abs() < 1e-4
        && t.ky.abs() < 1e-4;
    let quality = if exact && is_integer(t.tx) && is_integer(t.ty) {
        FilterQuality::Nearest
    } else {
        FilterQuality::Bilinear
    };
    if st.smear > 0.01 {
        // Motion smear: trailing ghost copies along the travel direction.
        for k in 1..=4 {
            let off = st.smear_dir * st.smear * 70.0 * k as f32;
            let paint = PixmapPaint {
                opacity: opacity * 0.22 * st.smear.min(1.0) / k as f32 * 2.0,
                quality: FilterQuality::Bilinear,
                ..PixmapPaint::default()
            };
            fb.draw_pixmap(0, 0, px.as_ref(), &paint, t.post_translate(off, 0.0), None);
        }
    }
    let paint = PixmapPaint {
        opacity,
        quality,
        ..PixmapPaint::default()
    };
    fb.draw_pixmap(0, 0, px.as_ref(), &paint, t, None);
}

/// Box-local -> frame transform including enter/exit animation and box motion
/// (scale/rotate about the box center).
fn box_transform(
    bx: f32,
    by: f32,
    bw: f32,
    bh: f32,
    st: &AnimState,
    mo: &MotionState,
) -> Transform {
    let cx = bx + bw / 2.0 + st.dx + mo.dx;
    let cy = by + bh / 2.0 + st.dy + mo.dy;
    let s = st.scale * mo.box_scale;
    let rot = st.rot + mo.rot;
    let mut t = Transform::from_translate(cx, cy);
    if rot.abs() > 1e-3 {
        t = t.pre_rotate(rot);
    }
    t.pre_scale(s, s).pre_translate(-bw / 2.0, -bh / 2.0)
}

#[derive(Debug, Clone, Copy)]
pub struct AnimState {
    pub scale: f32,
    pub dx: f32,
    pub dy: f32,
    pub rot: f32,
    pub opacity: f32,
    pub smear: f32,
    pub smear_dir: f32,
}

impl AnimState {
    const IDENTITY: AnimState = AnimState {
        scale: 1.0,
        dx: 0.0,
        dy: 0.0,
        rot: 0.0,
        opacity: 1.0,
        smear: 0.0,
        smear_dir: 1.0,
    };
}

pub fn anim_state(l: &Layer, local: f32, w: f32, h: f32) -> AnimState {
    let mut s = AnimState::IDENTITY;
    if l.enter.kind != AnimKind::None && l.enter.duration_s > 0.0 && local < l.enter.duration_s {
        apply_anim(
            l.enter,
            clamp01(local / l.enter.duration_s),
            &mut s,
            w,
            h,
            true,
        );
    }
    let remaining = l.end_s - l.start_s - local;
    if l.exit.kind != AnimKind::None && l.exit.duration_s > 0.0 && remaining < l.exit.duration_s {
        apply_anim(
            l.exit,
            clamp01(remaining / l.exit.duration_s),
            &mut s,
            w,
            h,
            false,
        );
    }
    s
}

fn apply_anim(a: Anim, p: f32, s: &mut AnimState, w: f32, h: f32, entering: bool) {
    let fade = |k: f32| clamp01(p * k);
    match a.kind {
        AnimKind::None => {}
        AnimKind::Fade => s.opacity *= ease::out_quad(p),
        AnimKind::SlideUp => {
            s.dy += (1.0 - ease::out_cubic(p)) * h * 0.09;
            s.opacity *= fade(2.5);
        }
        AnimKind::SlideDown => {
            s.dy -= (1.0 - ease::out_cubic(p)) * h * 0.09;
            s.opacity *= fade(2.5);
        }
        AnimKind::SlideLeft => {
            s.dx += (1.0 - ease::out_cubic(p)) * w * 0.3;
            s.opacity *= fade(2.5);
        }
        AnimKind::SlideRight => {
            s.dx -= (1.0 - ease::out_cubic(p)) * w * 0.3;
            s.opacity *= fade(2.5);
        }
        AnimKind::Pop => {
            s.scale *= 0.3 + 0.7 * ease::out_back(p);
            s.opacity *= fade(5.0);
        }
        AnimKind::ZoomIn => {
            s.scale *= 0.6 + 0.4 * ease::out_cubic(p);
            s.opacity *= fade(3.0);
        }
        AnimKind::ZoomOut => {
            s.scale *= 1.0 + 0.45 * (1.0 - ease::out_cubic(p));
            s.opacity *= fade(3.0);
        }
        AnimKind::Whip => {
            let dir = if entering { 1.0 } else { -1.0 };
            s.dx += (1.0 - ease::out_expo(p)) * w * 1.05 * dir;
            s.smear = (1.0 - p).powf(1.5);
            s.smear_dir = dir;
        }
        AnimKind::Drop => {
            s.dy -= (1.0 - ease::out_bounce(p)) * h * 0.3;
            s.opacity *= fade(6.0);
        }
        AnimKind::Spin => {
            s.rot -= (1.0 - ease::out_cubic(p)) * 200.0;
            s.scale *= 0.2 + 0.8 * ease::out_back(p);
            s.opacity *= fade(4.0);
        }
        // Text reveals per character (see typewriter_sprite); others fade.
        AnimKind::Typewriter => s.opacity *= fade(8.0),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MotionState {
    /// Content zoom inside the box (media).
    pub zoom: f32,
    /// Content offset, fraction of the box.
    pub pan_x: f32,
    pub pan_y: f32,
    /// Whole-box scale (non-media).
    pub box_scale: f32,
    pub dx: f32,
    pub dy: f32,
    pub rot: f32,
}

pub fn motion_state(
    m: &Motion,
    local: f32,
    life: f32,
    seed: u32,
    media: bool,
    w: f32,
    h: f32,
) -> MotionState {
    let mut s = MotionState {
        zoom: 1.0,
        pan_x: 0.0,
        pan_y: 0.0,
        box_scale: 1.0,
        dx: 0.0,
        dy: 0.0,
        rot: 0.0,
    };
    let u = clamp01(local / life);
    match *m {
        Motion::None => {}
        Motion::PushIn { amount } => {
            // Slight ease-out so the push starts moving immediately but never stalls.
            let z = 1.0 + amount * (0.6 * u + 0.4 * ease::out_quad(u));
            if media { s.zoom = z } else { s.box_scale = z }
        }
        Motion::PullOut { amount } => {
            if media {
                s.zoom = 1.0 + amount * (1.0 - u)
            } else {
                s.box_scale = 1.0 + amount * (1.0 - u)
            }
        }
        Motion::Pan { dx, dy } => {
            if media {
                s.zoom = 1.0 + dx.abs().max(dy.abs()) + 0.01;
                s.pan_x = (0.5 - u) * dx;
                s.pan_y = (0.5 - u) * dy;
            } else {
                s.dx = (u - 0.5) * dx * w;
                s.dy = (u - 0.5) * dy * h;
            }
        }
        Motion::Float => {
            s.dy = (local * std::f32::consts::TAU * 0.55).sin() * h * 0.006;
            s.rot = (local * std::f32::consts::TAU * 0.33 + seed as f32).sin() * 5.0;
        }
        Motion::Shake { intensity } => {
            let dur = 0.45;
            if local < dur {
                let amp = intensity * w * 0.022 * (1.0 - local / dur).powi(2);
                s.dx = amp * noise1(local * 40.0, seed);
                s.dy = amp * noise1(local * 40.0, seed.wrapping_add(17));
                s.rot = amp * 0.05 * noise1(local * 30.0, seed.wrapping_add(31));
                if media {
                    s.zoom = 1.0 + intensity * 0.05 * (1.0 - local / dur).powi(2);
                }
            }
        }
        Motion::Rotate { deg_per_s } => s.rot = deg_per_s * local,
    }
    s
}

fn load_image(path: &Path) -> Option<image::RgbaImage> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "svg" {
        let data = std::fs::read(path).ok()?;
        let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
        let size = tree.size();
        let scale = 1024.0 / size.width().max(size.height());
        let (w, h) = (
            (size.width() * scale).ceil() as u32,
            (size.height() * scale).ceil() as u32,
        );
        let mut px = Pixmap::new(w.max(1), h.max(1))?;
        resvg::render(&tree, Transform::from_scale(scale, scale), &mut px.as_mut());
        return image::RgbaImage::from_raw(px.width(), px.height(), unpremultiply(px.take()));
    }
    match image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map(|r| r.decode())
    {
        Ok(Ok(img)) => Some(img.to_rgba8()),
        Ok(Err(e)) => {
            tracing::warn!("cannot decode image {}: {e}", path.display());
            None
        }
        Err(e) => {
            tracing::warn!("cannot open image {}: {e}", path.display());
            None
        }
    }
}

fn to_pixmap(img: &image::RgbaImage) -> Pixmap {
    util::pixmap_from_rgba(img.width(), img.height(), img.as_raw().clone())
        .expect("non-empty image")
}

/// Tiny, blurred, darkened, then upscaled once to the full frame.
fn blurred_backdrop(img: &image::RgbaImage, w: u32, h: u32) -> Pixmap {
    let (sw, sh) = ((w / BACKDROP_DIV).max(8), (h / BACKDROP_DIV).max(8));
    let scale = (sw as f32 / img.width() as f32).max(sh as f32 / img.height() as f32);
    let (rw, rh) = (
        ((img.width() as f32 * scale).ceil() as u32).max(sw),
        ((img.height() as f32 * scale).ceil() as u32).max(sh),
    );
    let small = image::imageops::resize(img, rw, rh, image::imageops::FilterType::Triangle);
    let cropped =
        image::imageops::crop_imm(&small, (rw - sw) / 2, (rh - sh) / 2, sw, sh).to_image();
    let mut px = to_pixmap(&cropped);
    blur_pixmap(&mut px, 2, 3);
    darken(&mut px, 0.55, 1.3);
    let mut full = Pixmap::new(w, h).unwrap();
    let paint = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..PixmapPaint::default()
    };
    full.draw_pixmap(
        0,
        0,
        px.as_ref(),
        &paint,
        Transform::from_scale(w as f32 / sw as f32, h as f32 / sh as f32),
        None,
    );
    full
}

fn prepare_layer(
    l: &Layer,
    w: u32,
    h: u32,
    images: &HashMap<String, Option<Arc<image::RgbaImage>>>,
    backdrops: &HashMap<String, Arc<Pixmap>>,
) -> Prep {
    let (_, _, bw, bh) = box_px(l.rect, w, h);
    match &l.content {
        LayerContent::Image { path, pan } => {
            let Some(Some(img)) = images.get(path) else {
                return Prep::None;
            };
            let (iw, ih) = (img.width() as f32, img.height() as f32);
            let mut views = vec![l.crop.unwrap_or(RectF::FULL)];
            if let Some(p) = pan {
                views.push(p.from);
                views.push(p.to);
            }
            let zoom_max = match l.motion {
                Motion::PushIn { amount } | Motion::PullOut { amount } => 1.0 + amount,
                Motion::Pan { dx, dy } => 1.0 + dx.abs().max(dy.abs()),
                _ => 1.05,
            };
            let need = views
                .iter()
                .map(|v| (bw as f32 / (v.w.max(0.02) * iw)).max(bh as f32 / (v.h.max(0.02) * ih)))
                .fold(0.0, f32::max)
                * zoom_max
                * 1.1;
            if need < 0.85 {
                let (nw, nh) = (
                    (iw * need).round().max(1.0) as u32,
                    (ih * need).round().max(1.0) as u32,
                );
                let small = image::imageops::resize(
                    img.as_ref(),
                    nw,
                    nh,
                    image::imageops::FilterType::Triangle,
                );
                Prep::Image(Arc::new(to_pixmap(&small)))
            } else {
                Prep::Image(Arc::new(to_pixmap(img)))
            }
        }
        LayerContent::BlurredImage { path } => match backdrops.get(path) {
            Some(b) => Prep::Backdrop(b.clone()),
            None => Prep::None,
        },
        LayerContent::Text { text, style } => {
            let lay = text_layout(text, style, bw, bh);
            if l.enter.kind == AnimKind::Typewriter {
                Prep::Typewriter(lay)
            } else {
                Prep::Sprite(Arc::new(render_text_sprite(&lay, style, bw, bh, None)))
            }
        }
        LayerContent::Icon { path, tint } => match render_icon(Path::new(path), bw, bh, *tint) {
            Some(s) => Prep::Sprite(Arc::new(s)),
            None => Prep::None,
        },
        LayerContent::Meme {
            path,
            video,
            caption,
            top_text,
            bottom_text,
            ..
        } => {
            let aspect = if *video {
                16.0 / 9.0
            } else {
                images
                    .get(path)
                    .and_then(|i| i.as_ref())
                    .map(|i| i.width() as f32 / i.height() as f32)
                    .unwrap_or(1.0)
            };
            let m = meme_layout(caption, top_text, bottom_text, bw as f32, bh as f32, aspect);
            let media = if *video {
                None
            } else {
                images.get(path).and_then(|i| i.as_ref()).map(|img| {
                    // Pre-scale to the image area (cover) so per-frame drawing is cheap.
                    let (iw, ih) = (img.width() as f32, img.height() as f32);
                    let s = (m.img.2 / iw).max(m.img.3 / ih) * 1.08;
                    if s < 0.9 {
                        let small = image::imageops::resize(
                            img.as_ref(),
                            (iw * s).round().max(1.0) as u32,
                            (ih * s).round().max(1.0) as u32,
                            image::imageops::FilterType::Triangle,
                        );
                        Arc::new(to_pixmap(&small))
                    } else {
                        Arc::new(to_pixmap(img))
                    }
                })
            };
            Prep::Meme(Arc::new(m), media)
        }
        _ => Prep::None,
    }
}

fn pill_pad(style: &TextStyle, size: f32) -> f32 {
    if style.background.is_some() {
        size * 0.34
    } else {
        style.stroke.map(|s| s.1).unwrap_or(0.0)
    }
}

fn text_layout(text: &str, style: &TextStyle, bw: u32, bh: u32) -> Layout {
    let pad = pill_pad(style, style.size_px);
    let vpad = if style.background.is_some() {
        style.size_px * 0.2
    } else {
        pad
    };
    text::layout(
        style.font,
        text,
        style.size_px,
        bw as f32 - pad * 2.0,
        bh as f32 - vpad * 2.0,
        style.uppercase,
    )
}

/// Text block vertically centered in the box, on a padded sprite.
fn render_text_sprite(
    lay: &Layout,
    style: &TextStyle,
    bw: u32,
    bh: u32,
    reveal: Option<usize>,
) -> Sprite {
    let stroke = style.stroke.map(|s| s.1).unwrap_or(0.0);
    let pad = (stroke * 2.0 + lay.size * 0.45).ceil();
    let mut px = Pixmap::new(bw + 2 * pad as u32, bh + 2 * pad as u32).unwrap();
    let inset = pill_pad(style, lay.size);
    let y = pad + (bh as f32 - lay.height) / 2.0;
    let hl: Vec<String> = style.highlight_words.iter().map(|w| norm(w)).collect();
    let hc = style.highlight_color.unwrap_or(Rgba(255, 214, 10, 255));
    let color_of = |wi: usize| {
        if hl
            .iter()
            .any(|h| !h.is_empty() && *h == norm(&lay.words[wi].text))
        {
            hc
        } else {
            style.color
        }
    };
    let opts = DrawOpts {
        align: style.align,
        stroke: style.stroke,
        shadow: style.shadow,
        pill: style.background,
        color_of: &color_of,
        reveal_chars: reveal,
        opacity: 1.0,
    };
    text::draw_layout(
        &mut px,
        lay,
        pad + inset,
        y,
        bw as f32 - inset * 2.0,
        &opts,
        Transform::identity(),
    );
    Sprite { px, pad }
}

fn render_icon(path: &Path, bw: u32, bh: u32, tint: Option<Rgba>) -> Option<Sprite> {
    let data = std::fs::read(path).ok()?;
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let pad = (bw.min(bh) as f32 * 0.18).ceil();
    let (pw, ph) = (bw + 2 * pad as u32, bh + 2 * pad as u32);
    let mut icon = Pixmap::new(pw, ph)?;
    let s = (bw as f32 / size.width()).min(bh as f32 / size.height());
    let (ox, oy) = (
        pad + (bw as f32 - size.width() * s) / 2.0,
        pad + (bh as f32 - size.height() * s) / 2.0,
    );
    resvg::render(
        &tree,
        Transform::from_translate(ox, oy).pre_scale(s, s),
        &mut icon.as_mut(),
    );
    if let Some(c) = tint {
        for p in icon.data_mut().chunks_exact_mut(4) {
            let a = (p[3] as u32 * c.3 as u32 + 127) / 255;
            p[3] = a as u8;
            p[0] = ((c.0 as u32 * a + 127) / 255) as u8;
            p[1] = ((c.1 as u32 * a + 127) / 255) as u8;
            p[2] = ((c.2 as u32 * a + 127) / 255) as u8;
        }
    }
    // Soft drop shadow under the icon.
    let mut out = Pixmap::new(pw, ph)?;
    let mut shadow = icon.clone();
    for p in shadow.data_mut().chunks_exact_mut(4) {
        let a = (p[3] as f32 * 0.6) as u8;
        p.copy_from_slice(&[0, 0, 0, a]);
    }
    blur_pixmap(&mut shadow, (bw as f32 * 0.03).ceil() as usize, 2);
    let off = (bh as f32 * 0.04).round();
    out.draw_pixmap(
        0,
        off as i32,
        shadow.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    out.draw_pixmap(
        0,
        0,
        icon.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    Some(Sprite { px: out, pad })
}

fn card_res(bw: u32, bh: u32, c: CardStyle) -> CardRes {
    let mut mask = Mask::new(bw, bh).unwrap();
    if let Some(path) = rounded_rect(0.0, 0.0, bw as f32, bh as f32, c.radius) {
        let mut tmp = Pixmap::new(bw, bh).unwrap();
        tmp.fill_path(
            &path,
            &paint(sk::Color::WHITE),
            sk::FillRule::Winding,
            Transform::identity(),
            None,
        );
        mask = Mask::from_pixmap(tmp.as_ref(), MaskType::Alpha);
    }
    let shadow = c.shadow.then(|| {
        let blur = (bw.min(bh) as f32 * 0.035).clamp(8.0, 28.0);
        let pad = (blur * 3.0).ceil();
        let mut px = Pixmap::new(bw + 2 * pad as u32, bh + 2 * pad as u32).unwrap();
        if let Some(path) = rounded_rect(pad, pad, bw as f32, bh as f32, c.radius) {
            px.fill_path(
                &path,
                &paint(sk::Color::from_rgba8(0, 0, 0, 200)),
                sk::FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        // Blur at quarter resolution, then scale back up (cheap and smooth).
        let q = 4u32;
        let (sw, sh) = ((px.width() / q).max(1), (px.height() / q).max(1));
        let mut small = Pixmap::new(sw, sh).unwrap();
        let p = PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        };
        small.draw_pixmap(
            0,
            0,
            px.as_ref(),
            &p,
            Transform::from_scale(1.0 / q as f32, 1.0 / q as f32),
            None,
        );
        blur_pixmap(&mut small, (blur / q as f32).ceil() as usize, 3);
        let mut out = Pixmap::new(px.width(), px.height()).unwrap();
        out.draw_pixmap(
            0,
            0,
            small.as_ref(),
            &p,
            Transform::from_scale(
                px.width() as f32 / sw as f32,
                px.height() as f32 / sh as f32,
            ),
            None,
        );
        (out, pad)
    });
    CardRes { mask, shadow }
}

/// Layout of a meme card: caption band on top, media below; rects box-local.
pub struct MemeLayout {
    pub band_h: f32,
    pub caption: Option<(Layout, f32, f32)>,
    pub top: Option<Layout>,
    pub bottom: Option<Layout>,
    /// (x, y, w, h) of the media area.
    pub img: (f32, f32, f32, f32),
    pub box_w: f32,
}

pub fn meme_caption_size(box_w: f32) -> f32 {
    (box_w * 0.058).max(20.0)
}

/// Band height for a caption at card width `box_w` (also used by the director).
pub fn meme_band_height(caption: &str, box_w: f32) -> f32 {
    if caption.trim().is_empty() {
        return 0.0;
    }
    let size = meme_caption_size(box_w);
    let pad = box_w * 0.05;
    let lay = text::layout(
        FontKind::Heavy,
        caption,
        size,
        box_w - pad * 2.0,
        size * 1.14 * 5.0,
        false,
    );
    lay.height + lay.size * 0.35 + pad * 1.5
}

pub fn meme_layout(
    caption: &str,
    top: &str,
    bottom: &str,
    bw: f32,
    bh: f32,
    _aspect: f32,
) -> MemeLayout {
    let mut band_h = meme_band_height(caption, bw).min(bh * 0.5);
    let caption = (!caption.trim().is_empty()).then(|| {
        let size = meme_caption_size(bw);
        let pad = bw * 0.05;
        let lay = text::layout(
            FontKind::Heavy,
            caption,
            size,
            bw - pad * 2.0,
            band_h - pad * 1.2,
            false,
        );
        band_h = band_h.max(lay.height + pad * 1.5);
        let y = (band_h - lay.height) / 2.0;
        (lay, pad, y)
    });
    let img = (0.0, band_h, bw, (bh - band_h).max(10.0));
    let classic = |t: &str| {
        (!t.trim().is_empty()).then(|| {
            text::layout(
                FontKind::Display,
                t,
                img.2 * 0.115,
                img.2 * 0.9,
                img.3 * 0.3,
                true,
            )
        })
    };
    MemeLayout {
        band_h,
        top: classic(top),
        bottom: classic(bottom),
        caption,
        img,
        box_w: bw,
    }
}

fn render_meme(
    px: &mut Pixmap,
    m: &MemeLayout,
    media: Option<&Pixmap>,
    crop: Option<RectF>,
    fit: Fit,
    motion: &MotionState,
) {
    px.fill(sk::Color::WHITE);
    let (ix, iy, iw, ih) = m.img;
    if let Some(src) = media {
        let crop = bounded_crop(crop.unwrap_or(RectF::FULL));
        let (sw, sh) = (src.width() as f32, src.height() as f32);
        let (vw, vh) = (crop.w * sw, crop.h * sh);
        let base = match fit {
            Fit::Cover => (iw / vw).max(ih / vh),
            Fit::Contain => (iw / vw).min(ih / vh),
        };
        let t = Transform::from_translate(
            ix + iw / 2.0 + motion.pan_x * iw,
            iy + ih / 2.0 + motion.pan_y * ih,
        )
        .pre_scale(base * motion.zoom, base * motion.zoom)
        .pre_translate(-(crop.x * sw + vw / 2.0), -(crop.y * sh + vh / 2.0));
        let clip = sk::Rect::from_xywh(ix, iy, iw, ih).map(|r| {
            let mut mask = Mask::new(px.width(), px.height()).unwrap();
            mask.fill_path(
                &sk::PathBuilder::from_rect(r),
                sk::FillRule::Winding,
                false,
                Transform::identity(),
            );
            mask
        });
        let paint = PixmapPaint {
            quality: FilterQuality::Bilinear,
            ..PixmapPaint::default()
        };
        px.draw_pixmap(0, 0, src.as_ref(), &paint, t, clip.as_ref());
    } else {
        if let Some(r) = sk::Rect::from_xywh(ix, iy, iw, ih) {
            px.fill_rect(
                r,
                &paint(sk::Color::from_rgba8(30, 30, 36, 255)),
                Transform::identity(),
                None,
            );
        }
    }
    if let Some((lay, pad, y)) = &m.caption {
        let black = |_| Rgba(17, 17, 17, 255);
        let opts = DrawOpts {
            align: Align::Left,
            stroke: None,
            shadow: false,
            pill: None,
            color_of: &black,
            reveal_chars: None,
            opacity: 1.0,
        };
        text::draw_layout(
            px,
            lay,
            *pad,
            *y,
            m.box_w - pad * 2.0,
            &opts,
            Transform::identity(),
        );
    }
    let white = |_| Rgba::WHITE;
    for (lay, top) in [(&m.top, true), (&m.bottom, false)] {
        if let Some(lay) = lay {
            let stroke = (lay.size * 0.07).max(2.0);
            let opts = DrawOpts {
                align: Align::Center,
                stroke: Some((Rgba::BLACK, stroke)),
                shadow: false,
                pill: None,
                color_of: &white,
                reveal_chars: None,
                opacity: 1.0,
            };
            let margin = ih * 0.04 + stroke;
            let y = if top {
                iy + margin
            } else {
                iy + ih - margin - lay.height
            };
            text::draw_layout(
                px,
                lay,
                ix + iw * 0.05,
                y,
                iw * 0.9,
                &opts,
                Transform::identity(),
            );
        }
    }
}

/// Group caption words into short karaoke lines.
pub fn group_captions(tl: &Timeline) -> Vec<CapGroup> {
    let words = &tl.captions;
    let max_words = tl.caption_style.max_words.clamp(1, 6);
    let mut groups: Vec<CapGroup> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut chars = 0usize;
    let mut indices: Vec<usize> = (0..words.len()).collect();
    indices.sort_by(|&a, &b| words[a].start_s.total_cmp(&words[b].start_s));
    for i in indices {
        let w = &words[i];
        let clean = caption_text(&w.text);
        if clean.is_empty()
            || !w.start_s.is_finite()
            || !w.end_s.is_finite()
            || w.end_s <= w.start_s
        {
            continue;
        }
        if let Some(&last) = cur.last() {
            let gap = w.start_s - words[last].end_s;
            if cur.len() >= max_words || chars + clean.chars().count() + 1 > 17 || gap > 0.45 {
                groups.push(CapGroup {
                    words: std::mem::take(&mut cur),
                    start: 0.0,
                    end: 0.0,
                });
                chars = 0;
            }
        }
        cur.push(i);
        chars += clean.chars().count() + 1;
        let t = w.text.trim_end();
        if t.ends_with(['.', '?', '!', ',', ';', ':']) {
            groups.push(CapGroup {
                words: std::mem::take(&mut cur),
                start: 0.0,
                end: 0.0,
            });
            chars = 0;
        }
    }
    if !cur.is_empty() {
        groups.push(CapGroup {
            words: cur,
            start: 0.0,
            end: 0.0,
        });
    }
    let n = groups.len();
    for gi in 0..n {
        let first = groups[gi].words[0];
        let last = *groups[gi].words.last().unwrap();
        let start = words[first].start_s;
        let last_end = words[last].end_s;
        let next_start = if gi + 1 < n {
            words[groups[gi + 1].words[0]].start_s
        } else {
            f32::MAX
        };
        let end = if next_start - last_end < 0.7 {
            next_start
        } else {
            last_end + 0.35
        };
        groups[gi].start = start;
        groups[gi].end = end.min(tl.duration_s).max(start + 0.05);
    }
    groups
}

//! Small shared helpers: deterministic RNG, easing curves, shapes, blur.

use resvg::tiny_skia::{self as sk, Path, PathBuilder, Pixmap};

use clipcut_core::timeline::{RectF, Rgba};

/// xorshift64* — tiny, deterministic, good enough for editing choices.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1) ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn from_str(s: &str) -> Self {
        Self::new(fnv1a(s.as_bytes()))
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [0, 1).
    pub fn f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f32()
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
    pub fn chance(&mut self, p: f32) -> bool {
        self.f32() < p
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Smooth deterministic noise in [-1, 1] (for shake).
pub fn noise1(x: f32, seed: u32) -> f32 {
    let i = x.floor();
    let f = x - i;
    let h = |n: i32| -> f32 {
        let v = fnv1a(
            &[
                (n as u32 ^ seed.wrapping_mul(0x9E37)).to_le_bytes(),
                seed.to_le_bytes(),
            ]
            .concat(),
        );
        (v >> 40) as f32 / (1u64 << 23) as f32 - 1.0
    };
    let a = h(i as i32);
    let b = h(i as i32 + 1);
    let s = f * f * (3.0 - 2.0 * f);
    a + (b - a) * s
}

pub fn clamp01(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub mod ease {
    use std::f32::consts::PI;

    pub fn out_cubic(t: f32) -> f32 {
        1.0 - (1.0 - t).powi(3)
    }
    pub fn out_quad(t: f32) -> f32 {
        1.0 - (1.0 - t) * (1.0 - t)
    }
    pub fn in_out_cubic(t: f32) -> f32 {
        if t < 0.5 {
            4.0 * t * t * t
        } else {
            1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
        }
    }
    pub fn in_out_sine(t: f32) -> f32 {
        -((PI * t).cos() - 1.0) / 2.0
    }
    pub fn out_expo(t: f32) -> f32 {
        if t >= 1.0 {
            1.0
        } else {
            1.0 - 2f32.powf(-10.0 * t)
        }
    }
    pub fn out_back(t: f32) -> f32 {
        let c1 = 1.70158;
        let c3 = c1 + 1.0;
        1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
    }
    pub fn out_bounce(t: f32) -> f32 {
        let n1 = 7.5625;
        let d1 = 2.75;
        if t < 1.0 / d1 {
            n1 * t * t
        } else if t < 2.0 / d1 {
            let t = t - 1.5 / d1;
            n1 * t * t + 0.75
        } else if t < 2.5 / d1 {
            let t = t - 2.25 / d1;
            n1 * t * t + 0.9375
        } else {
            let t = t - 2.625 / d1;
            n1 * t * t + 0.984375
        }
    }
}

pub fn color(c: Rgba) -> sk::Color {
    sk::Color::from_rgba8(c.0, c.1, c.2, c.3)
}

pub fn color_alpha(c: Rgba, alpha: f32) -> sk::Color {
    let a = (c.3 as f32 * alpha.clamp(0.0, 1.0)).round() as u8;
    sk::Color::from_rgba8(c.0, c.1, c.2, a)
}

pub fn paint(c: sk::Color) -> sk::Paint<'static> {
    let mut p = sk::Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

/// Rounded rectangle path (circular corners via cubic approximations).
pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    if r <= 0.01 {
        return Some(PathBuilder::from_rect(sk::Rect::from_xywh(x, y, w, h)?));
    }
    let k = 0.552_284_8 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Separable box blur on a premultiplied RGBA pixmap, `passes` times
/// (3 passes ≈ gaussian).
pub fn blur_pixmap(px: &mut Pixmap, radius: usize, passes: usize) {
    if radius == 0 {
        return;
    }
    let w = px.width() as usize;
    let h = px.height() as usize;
    let data = px.data_mut();
    let mut tmp = vec![0u8; data.len()];
    for _ in 0..passes {
        box_blur_h(data, &mut tmp, w, h, radius);
        box_blur_v(&tmp, data, w, h, radius);
    }
}

fn box_blur_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let div = (2 * r + 1) as u32;
    for y in 0..h {
        let row = y * w * 4;
        for c in 0..4 {
            let at = |x: isize| -> u32 {
                src[row + (x.clamp(0, w as isize - 1) as usize) * 4 + c] as u32
            };
            let mut acc: u32 = 0;
            for i in -(r as isize)..=(r as isize) {
                acc += at(i);
            }
            for x in 0..w {
                dst[row + x * 4 + c] = (acc / div) as u8;
                acc += at(x as isize + r as isize + 1);
                acc -= at(x as isize - r as isize);
            }
        }
    }
}

fn box_blur_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let div = (2 * r + 1) as u32;
    for x in 0..w {
        for c in 0..4 {
            let at = |y: isize| -> u32 {
                src[(y.clamp(0, h as isize - 1) as usize) * w * 4 + x * 4 + c] as u32
            };
            let mut acc: u32 = 0;
            for i in -(r as isize)..=(r as isize) {
                acc += at(i);
            }
            for y in 0..h {
                dst[y * w * 4 + x * 4 + c] = (acc / div) as u8;
                acc += at(y as isize + r as isize + 1);
                acc -= at(y as isize - r as isize);
            }
        }
    }
}

/// Straight-alpha RGBA bytes -> premultiplied pixmap.
pub fn pixmap_from_rgba(w: u32, h: u32, mut data: Vec<u8>) -> Option<Pixmap> {
    for p in data.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a < 255 {
            p[0] = ((p[0] as u32 * a + 127) / 255) as u8;
            p[1] = ((p[1] as u32 * a + 127) / 255) as u8;
            p[2] = ((p[2] as u32 * a + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(data, sk::IntSize::from_wh(w, h)?)
}

/// Premultiplied tiny-skia pixels -> straight-alpha PNG pixels.
pub fn unpremultiply(mut data: Vec<u8>) -> Vec<u8> {
    for p in data.chunks_exact_mut(4) {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut p[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    data
}

/// Clamp size and origin jointly, keeping a crop wholly inside the source.
pub fn bounded_crop(c: RectF) -> RectF {
    let w = if c.w.is_finite() {
        c.w.clamp(0.01, 1.0)
    } else {
        1.0
    };
    let h = if c.h.is_finite() {
        c.h.clamp(0.01, 1.0)
    } else {
        1.0
    };
    let x = if c.x.is_finite() {
        c.x.clamp(0.0, 1.0 - w)
    } else {
        0.0
    };
    let y = if c.y.is_finite() {
        c.y.clamp(0.0, 1.0 - h)
    } else {
        0.0
    };
    RectF::new(x, y, w, h)
}

/// Multiply RGB of a premultiplied pixmap (darken backdrops).
pub fn darken(px: &mut Pixmap, factor: f32, saturation: f32) {
    let f = factor.clamp(0.0, 1.0);
    for p in px.data_mut().chunks_exact_mut(4) {
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        let l = 0.299 * r + 0.587 * g + 0.114 * b;
        let a = p[3] as f32;
        let s = |c: f32| ((l + (c - l) * saturation) * f).clamp(0.0, a) as u8;
        p[0] = s(r);
        p[1] = s(g);
        p[2] = s(b);
    }
}

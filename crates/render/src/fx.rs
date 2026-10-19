//! Raster effects on premultiplied RGBA8 pixmaps: blur (3× box ≈ Gaussian), dilate/erode (spread),
//! alpha extraction and tinting.

use tiny_skia::Pixmap;

/// Box radii for three passes approximating a Gaussian of `sigma` (Kovesi).
fn box_sizes(sigma: f64) -> [usize; 3] {
    let n = 3.0;
    let w_ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i64;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - n * (wl * wl) as f64 - 4.0 * n * wl as f64 - 3.0 * n) / (-4.0 * wl as f64 - 4.0);
    let m = m_ideal.round() as i64;
    let r = |i: i64| -> usize {
        let w = if i < m { wl } else { wu };
        ((w.max(1) - 1) / 2) as usize
    };
    [r(0), r(1), r(2)]
}

/// One horizontal box pass of radius `r` over `w` × `h` pixels of `C` channels.
fn box_blur_h<const C: usize>(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    if r == 0 {
        dst.copy_from_slice(src);
        return;
    }
    let div = (2 * r + 1) as u32;
    for y in 0..h {
        let row = y * w * C;
        let mut acc = [0u32; C];
        // Edge: transparent outside.
        for x in 0..=r.min(w.saturating_sub(1)) {
            for c in 0..C {
                acc[c] += u32::from(src.get(row + x * C + c).copied().unwrap_or(0));
            }
        }
        for x in 0..w {
            for c in 0..C {
                if let Some(d) = dst.get_mut(row + x * C + c) {
                    *d = (acc[c] / div) as u8;
                }
            }
            let add = x + r + 1;
            if add < w {
                for c in 0..C {
                    acc[c] += u32::from(src.get(row + add * C + c).copied().unwrap_or(0));
                }
            }
            if x >= r {
                let sub = x - r;
                for c in 0..C {
                    acc[c] = acc[c].saturating_sub(u32::from(src.get(row + sub * C + c).copied().unwrap_or(0)));
                }
            }
        }
    }
}

fn transpose<const C: usize>(src: &[u8], dst: &mut [u8], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let s = (y * w + x) * C;
            let d = (x * h + y) * C;
            if let (Some(sp), Some(dp)) = (src.get(s..s + C), dst.get_mut(d..d + C)) {
                dp.copy_from_slice(sp);
            }
        }
    }
}

/// The largest blur standard deviation drawn (pixels): far wider than any layer (at most 16,384
/// pixels a side), so wider blurs look the same (everything averages out to nothing). The box
/// passes cost the same at any radius, so the documented blur radii (up to 10,000 units) draw
/// in full at any zoom.
pub const MAX_SIGMA: f64 = 1_000_000.0;

/// Gaussian-like blur of the whole pixmap with standard deviation `sigma` (pixels).
pub fn blur(p: &mut Pixmap, sigma: f64) {
    if !(sigma.is_finite() && sigma > 0.3) {
        return;
    }
    let (w, h) = (p.width() as usize, p.height() as usize);
    let b = blur_channels::<4>(p.data().to_vec(), w, h, sigma);
    p.data_mut().copy_from_slice(&b);
}

/// [`blur`] of the alpha alone, which the colour channels then copy (shadows, which are tinted
/// afterwards and only keep the alpha: a quarter of the work).
pub fn blur_alpha(p: &mut Pixmap, sigma: f64) {
    if !(sigma.is_finite() && sigma > 0.3) {
        return;
    }
    let (w, h) = (p.width() as usize, p.height() as usize);
    let alpha: Vec<u8> = p.data().as_chunks::<4>().0.iter().map(|c| c[3]).collect();
    let alpha = blur_channels::<1>(alpha, w, h, sigma);
    for (px, a) in p.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
        px.copy_from_slice(&[a, a, a, a]);
    }
}

/// `a` (`w` × `h` pixels of `C` channels) blurred by `sigma`: three box passes each way.
fn blur_channels<const C: usize>(mut a: Vec<u8>, w: usize, h: usize, sigma: f64) -> Vec<u8> {
    let radii = box_sizes(sigma.min(MAX_SIGMA));
    let mut b = vec![0u8; a.len()];
    for r in radii {
        box_blur_h::<C>(&a, &mut b, w, h, r);
        std::mem::swap(&mut a, &mut b);
    }
    transpose::<C>(&a, &mut b, w, h);
    std::mem::swap(&mut a, &mut b);
    for r in radii {
        box_blur_h::<C>(&a, &mut b, h, w, r);
        std::mem::swap(&mut a, &mut b);
    }
    transpose::<C>(&a, &mut b, h, w);
    b
}

/// Grow (`r > 0`) or shrink (`r < 0`) the alpha of a pixmap by `r` pixels (square structuring
/// element, separable max/min; outside the pixmap counts as empty). Colour channels follow alpha
/// (tinted afterwards anyway). Each pass costs a few steps per pixel whatever `r` (van Herk /
/// Gil-Werman): a shadow's spread of hundreds of pixels on a large layer took seconds pixel by
/// pixel.
pub fn morph(p: &mut Pixmap, r: i32) {
    if r == 0 {
        return;
    }
    let grow = r > 0;
    let r = r.unsigned_abs().min(400) as usize;
    let (w, h) = (p.width() as usize, p.height() as usize);
    let mut alpha: Vec<u8> = p.data().as_chunks::<4>().0.iter().map(|c| c.get(3).copied().unwrap_or(0)).collect();
    let mut bufs = Lines::default();
    for row in alpha.chunks_exact_mut(w.max(1)) {
        bufs.run(row, r, grow);
    }
    let mut col = vec![0u8; h];
    for x in 0..w {
        for (y, v) in col.iter_mut().enumerate() {
            *v = alpha.get(y * w + x).copied().unwrap_or(0);
        }
        bufs.run(&mut col, r, grow);
        for (y, v) in col.iter().enumerate() {
            if let Some(d) = alpha.get_mut(y * w + x) {
                *d = *v;
            }
        }
    }
    for (px, a) in p.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
        px.copy_from_slice(&[a, a, a, a]);
    }
}

/// Buffers for [`morph`]'s passes, reused line to line.
#[derive(Default)]
struct Lines {
    padded: Vec<u8>,
    ahead: Vec<u8>,
    behind: Vec<u8>,
}

impl Lines {
    /// Each value of `line` becomes the max (`grow`) or min of the values within `r` of it, those
    /// past the ends counting as 0: running max/min over blocks of the window's length, from the
    /// block's start (`ahead`) and to its end (`behind`); a window spans at most two blocks.
    fn run(&mut self, line: &mut [u8], r: usize, grow: bool) {
        let op = |a: u8, b: u8| if grow { a.max(b) } else { a.min(b) };
        let k = 2 * r + 1;
        self.padded.clear();
        self.padded.resize(r, 0);
        self.padded.extend_from_slice(line);
        self.padded.resize(line.len() + 2 * r, 0);
        let m = self.padded.len();
        self.ahead.clear();
        let mut acc = 0;
        for (i, v) in self.padded.iter().enumerate() {
            acc = if i % k == 0 { *v } else { op(acc, *v) };
            self.ahead.push(acc);
        }
        self.behind.clear();
        self.behind.resize(m, 0);
        for (i, v) in self.padded.iter().enumerate().rev() {
            acc = if i + 1 == m || (i + 1) % k == 0 { *v } else { op(acc, *v) };
            if let Some(b) = self.behind.get_mut(i) {
                *b = acc;
            }
        }
        for ((out, b), a) in line.iter_mut().zip(&self.behind).zip(self.ahead.iter().skip(k - 1)) {
            *out = op(*b, *a);
        }
    }
}

/// Replace every pixel with `color` (non-premultiplied RGBA 0..1) at the pixel's alpha × color alpha.
pub fn tint(p: &mut Pixmap, color: [f32; 4]) {
    let [r, g, b, a] = color;
    for px in p.data_mut().as_chunks_mut::<4>().0 {
        let alpha = f32::from(px.get(3).copied().unwrap_or(0)) / 255.0 * a;
        let q = |c: f32| (c * alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        px.copy_from_slice(&[q(r), q(g), q(b), (alpha * 255.0).round().clamp(0.0, 255.0) as u8]);
    }
}

/// Invert the alpha (premultiplied white with alpha 1-a): used for inner shadows.
pub fn invert_alpha(p: &mut Pixmap) {
    for px in p.data_mut().as_chunks_mut::<4>().0 {
        let a = 255 - px.get(3).copied().unwrap_or(0);
        px.copy_from_slice(&[a, a, a, a]);
    }
}

/// Multiply every pixel of `p` by the alpha of `m` (same size); `invert` uses 1-alpha.
pub fn mask_by(p: &mut Pixmap, m: &Pixmap, invert: bool) {
    for (px, mp) in p.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(m.data().chunks_exact(4)) {
        let ma = u32::from(mp.get(3).copied().unwrap_or(0));
        let ma = if invert { 255 - ma } else { ma };
        for c in px.iter_mut() {
            *c = ((u32::from(*c) * ma + 127) / 255) as u8;
        }
    }
}

/// [`mask_by`] with `m`'s pixel `(x + at.0, y + at.1)` for `p`'s pixel `(x, y)`; past `m`'s edges
/// the mask is empty.
/// The alpha of `small` scaled up `k` times (bilinear, pixel centres matching), as a `w` × `h`
/// pixmap whose top-left lies `at` scaled-up pixels into it, with the alpha in every channel.
/// One channel and two taps each way: a shadow cast from a small copy came back up at a few
/// pixels per microsecond through the general resampler.
pub fn upscale_alpha(small: &Pixmap, k: u32, at: (u32, u32), w: u32, h: u32) -> Option<Pixmap> {
    let mut out = Pixmap::new(w, h)?;
    let (sw, sh) = (small.width() as usize, small.height() as usize);
    if sw == 0 || sh == 0 || k == 0 {
        return Some(out);
    }
    let a: Vec<u8> = small.data().as_chunks::<4>().0.iter().map(|c| c[3]).collect();
    // For each output column or row: the two source ones and the second's weight (of 256).
    let taps = |n: u32, off: u32, len: usize| -> Vec<(usize, usize, u32)> {
        (0..n)
            .map(|i| {
                let u = (f64::from(i + off) + 0.5) / f64::from(k) - 0.5;
                let u0 = u.floor();
                let f = ((u - u0) * 256.0).round() as u32;
                let c = |v: f64| (v.max(0.0) as usize).min(len - 1);
                (c(u0), c(u0 + 1.0), f.min(256))
            })
            .collect()
    };
    let (xs, ys) = (taps(w, at.0, sw), taps(h, at.1, sh));
    // The source rows scaled across (×256), each once, as the output rows reach them.
    let mut rows: Vec<Option<Vec<u32>>> = vec![None; sh];
    let across = |y: usize, rows: &mut Vec<Option<Vec<u32>>>| {
        if rows.get(y).is_some_and(Option::is_none) {
            let src = a.get(y * sw..(y + 1) * sw).unwrap_or(&[]);
            let v = xs
                .iter()
                .map(|&(x0, x1, f)| {
                    let (p, q) = (u32::from(src.get(x0).copied().unwrap_or(0)), u32::from(src.get(x1).copied().unwrap_or(0)));
                    p * (256 - f) + q * f
                })
                .collect();
            if let Some(r) = rows.get_mut(y) {
                *r = Some(v);
            }
        }
    };
    let wu = w as usize;
    for (oy, &(y0, y1, f)) in ys.iter().enumerate() {
        across(y0, &mut rows);
        across(y1, &mut rows);
        let (Some(Some(r0)), Some(Some(r1))) = (rows.get(y0), rows.get(y1)) else { continue };
        let line = out.data_mut().get_mut(oy * wu * 4..(oy + 1) * wu * 4).unwrap_or(&mut []);
        for ((px, p), q) in line.as_chunks_mut::<4>().0.iter_mut().zip(r0).zip(r1) {
            let v = ((p * (256 - f) + q * f + 32_768) >> 16).min(255) as u8;
            *px = [v; 4];
        }
        // Rows above this one aren't read again.
        if let Some(done) = y0.checked_sub(1).and_then(|y| rows.get_mut(y)) {
            *done = None;
        }
    }
    Some(out)
}

/// `p` at half its size (rounded up): each pixel the mean of the 2 × 2 it covers, with what lies
/// past the edge counted as transparent.
pub fn half(p: &Pixmap) -> Option<Pixmap> {
    let (w, h) = (p.width(), p.height());
    let (hw, hh) = (w.div_ceil(2), h.div_ceil(2));
    let mut out = Pixmap::new(hw, hh)?;
    let src = p.data();
    let at = |x: u32, y: u32, c: usize| if x < w && y < h { u32::from(src.get(((y * w + x) * 4) as usize + c).copied().unwrap_or(0)) } else { 0 };
    for (i, px) in out.data_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i as u32 % hw) * 2, (i as u32 / hw) * 2);
        for (c, v) in px.iter_mut().enumerate() {
            *v = ((at(x, y, c) + at(x + 1, y, c) + at(x, y + 1, c) + at(x + 1, y + 1, c) + 2) / 4) as u8;
        }
    }
    Some(out)
}

pub fn mask_by_at(p: &mut Pixmap, m: &Pixmap, at: (i32, i32), invert: bool) {
    let (pw, mw, mh) = (p.width() as i64, i64::from(m.width()), i64::from(m.height()));
    for (i, px) in p.data_mut().as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i as i64 % pw.max(1) + i64::from(at.0), i as i64 / pw.max(1) + i64::from(at.1));
        let ma =
            if x >= 0 && y >= 0 && x < mw && y < mh { u32::from(m.data().get(((y * mw + x) * 4 + 3) as usize).copied().unwrap_or(0)) } else { 0 };
        let ma = if invert { 255 - ma } else { ma };
        for c in px.iter_mut() {
            *c = ((u32::from(*c) * ma + 127) / 255) as u8;
        }
    }
}

/// Call `f(dst_row, src_row, row, first_col)` for each row where `src`, with its top-left at `at`
/// on `dst`, overlaps it (premultiplied RGBA bytes of the overlapping columns; `row` and
/// `first_col` in `src`'s pixels). Nothing past either edge is visited.
pub(crate) fn overlap_rows(dst: &mut Pixmap, at: (i32, i32), src: &Pixmap, mut f: impl FnMut(&mut [u8], &[u8], i64, i64)) {
    let (dw, dh) = (i64::from(dst.width()), i64::from(dst.height()));
    let (sw, sh) = (i64::from(src.width()), i64::from(src.height()));
    let (ax, ay) = (i64::from(at.0), i64::from(at.1));
    let (c0, c1) = ((-ax).max(0), sw.min(dw - ax));
    let (r0, r1) = ((-ay).max(0), sh.min(dh - ay));
    if c1 <= c0 || r1 <= r0 {
        return;
    }
    let len = (c1 - c0) as usize * 4;
    let s = src.data();
    let d = dst.data_mut();
    for r in r0..r1 {
        let (si, di) = ((r * sw + c0) as usize * 4, ((r + ay) * dw + ax + c0) as usize * 4);
        if let (Some(sp), Some(dp)) = (s.get(si..si + len), d.get_mut(di..di + len)) {
            f(dp, sp, r, c0);
        }
    }
}

/// Composite `src` at `opacity` onto `dst` (top-left at `at`) with plus-darker (linear burn:
/// `max(0, backdrop + source - 1)`, through the W3C general compositing formula), which
/// tiny-skia lacks.
pub fn plus_darker(dst: &mut Pixmap, at: (i32, i32), src: &Pixmap, opacity: f32) {
    let o = if opacity.is_finite() { opacity.clamp(0.0, 1.0) } else { 0.0 };
    overlap_rows(dst, at, src, |dp, sp, _, _| {
        for (d, s) in dp.chunks_exact_mut(4).zip(sp.chunks_exact(4)) {
            let f = |v: u8| f32::from(v) / 255.0;
            let sa = f(s[3]) * o;
            if sa <= 0.0 {
                continue;
            }
            let da = f(d[3]);
            for c in 0..3 {
                let (sc, dc) = (f(s[c]) * o, f(d[c]));
                let v = sc * (1.0 - da) + dc * (1.0 - sa) + (sa * dc + da * sc - sa * da).max(0.0);
                d[c] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            d[3] = ((sa + da - sa * da).clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_spreads_and_conserves() {
        let mut p = Pixmap::new(64, 64).unwrap();
        let i = (32 * 64 + 32) * 4;
        p.data_mut()[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        let before: u32 = p.data().iter().map(|v| u32::from(*v)).sum();
        blur(&mut p, 3.0);
        let after: u32 = p.data().iter().map(|v| u32::from(*v)).sum();
        assert!(p.data()[i + 3] < 255);
        // Integer rounding loses a little.
        assert!(after <= before && after > 0, "{before} {after}");
    }

    #[test]
    fn morph_grows() {
        let mut p = Pixmap::new(20, 20).unwrap();
        let i = (10 * 20 + 10) * 4;
        p.data_mut()[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        morph(&mut p, 2);
        let count = p.data().as_chunks::<4>().0.iter().filter(|c| c[3] == 255).count();
        assert_eq!(count, 25);
        morph(&mut p, -2);
        let count = p.data().as_chunks::<4>().0.iter().filter(|c| c[3] == 255).count();
        assert_eq!(count, 1);
    }

    /// The square max/min, pixel by pixel (what `morph` did before it was fast).
    fn morph_by_hand(alpha: &[u8], w: usize, h: usize, r: usize, grow: bool) -> Vec<u8> {
        let at = |x: i64, y: i64| if x < 0 || y < 0 || x >= w as i64 || y >= h as i64 { 0 } else { alpha[y as usize * w + x as usize] };
        let mut out = vec![0; w * h];
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                let r = r as i64;
                let mut v = if grow { 0 } else { 255 };
                for dy in -r..=r {
                    for dx in -r..=r {
                        let s = at(x + dx, y + dy);
                        v = if grow { v.max(s) } else { v.min(s) };
                    }
                }
                out[y as usize * w + x as usize] = v;
            }
        }
        out
    }

    #[test]
    fn morph_equals_the_square_max_and_min_by_hand() {
        let (w, h) = (23, 17);
        let alpha: Vec<u8> = (0..w * h).map(|i| ((i * 7919 + 13) % 251) as u8).map(|v| if v > 180 { v } else { v / 4 }).collect();
        for r in [1, 2, 3, 5, 8, 11, 30] {
            for grow in [true, false] {
                let mut p = Pixmap::new(w as u32, h as u32).unwrap();
                for (px, a) in p.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(&alpha) {
                    px.copy_from_slice(&[*a, *a, *a, *a]);
                }
                morph(&mut p, if grow { r as i32 } else { -(r as i32) });
                let got: Vec<u8> = p.data().as_chunks::<4>().0.iter().map(|c| c[3]).collect();
                assert_eq!(got, morph_by_hand(&alpha, w, h, r, grow), "r {r}, grow {grow}");
            }
        }
    }

    #[test]
    fn a_wide_spread_on_a_large_layer_is_quick() {
        let mut p = Pixmap::new(2400, 1800).unwrap();
        p.fill_rect(
            tiny_skia::Rect::from_xywh(900.0, 700.0, 600.0, 400.0).unwrap(),
            &tiny_skia::Paint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
        let t = std::time::Instant::now();
        morph(&mut p, 300);
        // Pixel by pixel this took seconds; a debug build of the fast passes takes well under one.
        assert!(t.elapsed().as_secs_f64() < 3.0, "{:?}", t.elapsed());
        assert_eq!(p.pixel(600, 400).map(|c| c.alpha()), Some(255));
        assert_eq!(p.pixel(599, 399).map(|c| c.alpha()), Some(0));
    }

    #[test]
    fn box_sizes_sane() {
        for s in [0.5, 1.0, 4.0, 50.0] {
            let b = box_sizes(s);
            assert!(b.iter().all(|r| *r < 1000));
        }
    }
}

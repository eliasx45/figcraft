//! Audio clips on layers: a waveform strip from the clip's peak envelope, a time ruler counting
//! frames along the top and the markers (a line, a flag and the label). Drawn over the layer's
//! fills, under its children, inside its box.

use figcraft_color::Color;
use figcraft_doc::{AudioClip, Node, TextData};
use figcraft_geom::{Affine, BezPath, Rect, Shape};
use tiny_skia::{FillRule, Pixmap};

use crate::draw::{ts_path, ts_transform};

/// The most waveform columns drawn for one layer (a very wide strip at a high zoom).
const MAX_COLUMNS: usize = 16_384;
/// The most ruler ticks drawn.
const MAX_TICKS: usize = 4_000;
/// The most marker labels laid out per draw.
const MAX_LABELS: usize = 300;
/// The most a quiet clip's waveform is scaled up.
const MAX_GAIN: f64 = 8.0;
/// Ruler height and label size (layer units) for a strip at least 64 tall.
const RULER: f64 = 16.0;
const LABEL_SIZE: f64 = 9.0;
const MARKER_SIZE: f64 = 10.0;

/// One solid fill of a strip: `path` (in `at`'s space, which maps into the layer) in `color` at
/// `alpha`, filled non-zero. The CPU and GPU renderers both fill these, in order.
#[derive(Clone, Debug)]
pub struct Piece {
    pub path: BezPath,
    pub color: Color,
    pub alpha: f32,
    pub at: Affine,
}

/// Draw `n`'s clip `a` on `pix` (layer → pixels through `xf`, `scale` device pixels per unit).
pub fn draw(pix: &mut Pixmap, n: &Node, a: &AudioClip, xf: Affine, scale: f64) {
    for p in pieces(n, a, scale) {
        if let Some(tp) = ts_path(&p.path) {
            pix.fill_path(&tp, &paint(p.color, p.alpha), FillRule::Winding, ts_transform(xf * p.at), None);
        }
    }
}

/// What `n`'s clip `a` draws at `scale` device pixels per layer unit, in drawing order.
pub fn pieces(n: &Node, a: &AudioClip, scale: f64) -> Vec<Piece> {
    let mut out = Vec::new();
    let (w, h) = (n.width, n.height);
    if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
        return out;
    }
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let ruler = if a.ruler { RULER.min(h * 0.3) } else { 0.0 };
    let (top, bottom) = (ruler, h);
    wave(&mut out, a, w, top, bottom, scale);
    if ruler > 0.0 {
        draw_ruler(&mut out, a, w, ruler, scale);
    }
    markers(&mut out, a, w, h, ruler, scale);
    out
}

fn paint(c: Color, alpha: f32) -> tiny_skia::Paint<'static> {
    let mut p = tiny_skia::Paint { anti_alias: true, ..Default::default() };
    let [r, g, b, a] = c.to_rgba8();
    p.set_color_rgba8(r, g, b, (f32::from(a) * alpha.clamp(0.0, 1.0)).round() as u8);
    p
}

fn fill(out: &mut Vec<Piece>, path: BezPath, color: Color, alpha: f32, at: Affine) {
    if !path.elements().is_empty() {
        out.push(Piece { path, color, alpha, at });
    }
}

fn rect(path: &mut BezPath, x0: f64, y0: f64, x1: f64, y1: f64) {
    path.extend(Rect::new(x0, y0, x1, y1).path_elements(0.1));
}

/// The waveform between `top` and `bottom`: one bar per device pixel column, as tall as the
/// loudest sample under it, mirrored about the middle.
fn wave(out: &mut Vec<Piece>, a: &AudioClip, w: f64, top: f64, bottom: f64, scale: f64) {
    let mid = (top + bottom) / 2.0;
    let half = ((bottom - top) / 2.0 - 1.0).max(0.5);
    let cols = ((w * scale).ceil().max(1.0) as usize).min(MAX_COLUMNS);
    let step = w / cols as f64;
    // Quiet recordings are scaled up to the loudest peak shown (at most 8×), so speech reads.
    let loudest = f64::from(a.peak_over(a.start, a.end));
    let gain = if loudest > 0.0 { (1.0 / loudest).min(MAX_GAIN) } else { 1.0 };
    let mut path = BezPath::new();
    for i in 0..cols {
        let (x0, x1) = (i as f64 * step, (i + 1) as f64 * step);
        let p = (f64::from(a.peak_over(a.time_at(x0, w), a.time_at(x1, w))) * gain).min(1.0);
        // At least a hairline, so silence still reads as a strip.
        let hh = (p * half).max(0.5 / scale);
        rect(&mut path, x0, mid - hh, x1 + step * 0.05, mid + hh);
    }
    fill(out, path, a.color, 1.0, Affine::IDENTITY);
    // The centre line.
    let mut line = BezPath::new();
    rect(&mut line, 0.0, mid - 0.5 / scale, w, mid + 0.5 / scale);
    fill(out, line, a.color, 0.5, Affine::IDENTITY);
}

/// Frames between ruler ticks so they stand at least `min_px` device pixels apart.
fn tick_step(px_per_frame: f64, min_px: f64, fps: f64) -> f64 {
    let second = fps.round().max(1.0);
    let steps = [1.0, 2.0, 5.0, 10.0, second / 2.0, second, second * 2.0, second * 5.0, second * 10.0, second * 30.0, second * 60.0, second * 300.0];
    let mut sorted = steps.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted.into_iter().find(|s| *s > 0.0 && s * px_per_frame >= min_px).unwrap_or(second * 600.0)
}

/// The ruler: a band along the top with ticks every few frames and a frame number at the major
/// ones.
fn draw_ruler(out: &mut Vec<Piece>, a: &AudioClip, w: f64, ruler: f64, scale: f64) {
    let mut band = BezPath::new();
    rect(&mut band, 0.0, 0.0, w, ruler);
    fill(out, band, Color::BLACK, 0.25, Affine::IDENTITY);
    let fps = a.fps.max(0.01);
    let px_per_frame = w / (a.span() * fps) * scale;
    if !(px_per_frame.is_finite() && px_per_frame > 0.0) {
        return;
    }
    let minor = tick_step(px_per_frame, 6.0, fps);
    let major = tick_step(px_per_frame, 56.0, fps).max(minor);
    // Timeline frames shown: offset included.
    let f0 = ((a.start + a.offset) * fps / minor).floor() * minor;
    let f1 = (a.end + a.offset) * fps;
    let mut ticks = BezPath::new();
    let mut labels: Vec<(f64, String)> = Vec::new();
    let mut f = f0;
    let mut n = 0;
    while f <= f1 && n < MAX_TICKS {
        let x = a.x_of(a.time_of_frame(f), w);
        if (0.0..=w).contains(&x) {
            let is_major = (f / major).round() * major == f;
            let th = if is_major { ruler * 0.55 } else { ruler * 0.25 };
            rect(&mut ticks, x - 0.5 / scale, ruler - th, x + 0.5 / scale, ruler);
            if is_major && labels.len() < MAX_LABELS {
                labels.push((x, format!("{}", f as i64)));
            }
        }
        f += minor;
        n += 1;
    }
    fill(out, ticks, Color::WHITE, 0.7, Affine::IDENTITY);
    let size = LABEL_SIZE.min(ruler * 0.6);
    for (x, s) in labels {
        text(out, &s, size, Color::WHITE, 0.85, x + 2.0 / scale, 1.0, w);
    }
}

/// Markers: a line down the strip, a flag under the ruler and the label beside it.
fn markers(out: &mut Vec<Piece>, a: &AudioClip, w: f64, h: f64, ruler: f64, scale: f64) {
    let size = MARKER_SIZE.min((h - ruler) * 0.3).max(4.0);
    let mut drawn = 0;
    for m in &a.markers {
        let x = a.x_of(m.time, w);
        if !(-0.5..=w + 0.5).contains(&x) {
            continue;
        }
        let c = m.color.unwrap_or(a.marker_color);
        let mut line = BezPath::new();
        rect(&mut line, x - 0.75 / scale, ruler, x + 0.75 / scale, h);
        fill(out, line, c, 1.0, Affine::IDENTITY);
        let mut flag = BezPath::new();
        let fs = size * 0.6;
        flag.move_to((x, ruler));
        flag.line_to((x + fs, ruler));
        flag.line_to((x, ruler + fs));
        flag.close_path();
        fill(out, flag, c, 1.0, Affine::IDENTITY);
        if !m.label.is_empty() && drawn < MAX_LABELS {
            text(out, &m.label, size, c, 1.0, x + fs + 1.0, ruler + 1.0, w);
            drawn += 1;
        }
    }
}

/// One line of `s` at (`x`, `y`) (its top left), kept inside 0..`w` (moved left when it would run
/// past the right edge).
#[allow(clippy::too_many_arguments)]
fn text(out: &mut Vec<Piece>, s: &str, size: f64, c: Color, alpha: f32, x: f64, y: f64, w: f64) {
    let mut t = TextData::new(s);
    t.style.font_size = size;
    let layout = figcraft_text::layout_cached(&t, None, None);
    let x = if x + layout.width > w { (w - layout.width - 1.0).max(0.0) } else { x };
    let mut path = BezPath::new();
    for g in &layout.glyphs {
        path.extend(g.path().elements().iter().copied());
    }
    fill(out, path, c, alpha, Affine::translate((x, y)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_steps_grow_with_zoom_out() {
        // 10 px per frame: every frame; 0.1 px per frame: a tick per 2 s at 30 fps (60 frames = 6 px).
        assert_eq!(tick_step(10.0, 6.0, 30.0), 1.0);
        assert_eq!(tick_step(0.1, 6.0, 30.0), 60.0);
        assert_eq!(tick_step(1e-9, 6.0, 30.0), 18_000.0);
        assert!(tick_step(0.5, 56.0, 29.97) >= 112.0);
    }
}

//! The scene graph as a vello scene, in page coordinates. Mirrors `figcraft_render::draw` for
//! the subset the GPU draws; anything else is a [`Fallback`] and the frame renders on the CPU.

use std::collections::HashMap;
use std::sync::Arc;

use figcraft_color::{BlendMode, Color, Gradient, GradientKind, Paint, PaintKind};
use figcraft_doc::{Document, Node, NodeId, NodeKind, StrokeAlign};
use figcraft_geom::{Affine, BezPath, Rect, Shape};
use figcraft_render::paint::{Prepared, prepare_at};
use figcraft_render::{FillRule, Pixmap};
use vello::Scene;
use vello::peniko::{self, Blob, Compose, Extend, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality, ImageSampler, Mix};

/// Why a frame falls back to the CPU renderer.
pub type Fallback = &'static str;

/// Decoded images handed to vello, one blob per pixmap so its atlas keeps them across frames.
#[derive(Default)]
pub struct Images(HashMap<usize, (Arc<Pixmap>, ImageData)>);

struct PixBytes(Arc<Pixmap>);

impl AsRef<[u8]> for PixBytes {
    fn as_ref(&self) -> &[u8] {
        self.0.data()
    }
}

impl Images {
    fn get(&mut self, p: &Arc<Pixmap>) -> ImageData {
        // The map holds the Arc, so its address can't be reused while the entry lives.
        let key = Arc::as_ptr(p) as usize;
        if let Some((_, d)) = self.0.get(&key) {
            return d.clone();
        }
        let d = ImageData {
            data: Blob::new(Arc::new(PixBytes(p.clone()))),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: p.width(),
            height: p.height(),
        };
        if self.0.len() > 512 {
            self.0.clear();
        }
        self.0.insert(key, (p.clone(), d.clone()));
        d
    }
}

/// Builds the scene of one document.
pub struct Builder<'a> {
    pub doc: &'a Document,
    /// Layers not to draw (text being edited).
    pub skip: &'a [NodeId],
    pub images: &'a mut Images,
    /// Device pixels per page unit the scene is drawn at, for paints the CPU renderer turns into
    /// pixels first (images at the matching level of their pyramid, diamond gradients).
    pub scale: f64,
    /// Whether a paint was turned into pixels at `scale` (the scene then belongs to that scale).
    pub sampled: bool,
    /// Approximate what's too small to read at `scale`, as the CPU's canvas previews do
    /// (`figcraft_render::lod`: text greeked into bars).
    pub lod: bool,
}

fn pc(c: Color) -> peniko::Color {
    peniko::Color::new([c.r, c.g, c.b, c.a])
}

fn fill_rule(r: FillRule) -> Fill {
    match r {
        FillRule::EvenOdd => Fill::EvenOdd,
        FillRule::Winding => Fill::NonZero,
    }
}

/// The CPU renderer's blend modes (`figcraft_render::paint::ts_blend`), plus darker included.
fn mode(b: BlendMode) -> peniko::BlendMode {
    let m = match b {
        BlendMode::PassThrough | BlendMode::Normal => Mix::Normal,
        BlendMode::Darken => Mix::Darken,
        BlendMode::Multiply | BlendMode::PlusDarker => Mix::Multiply,
        BlendMode::ColorBurn => Mix::ColorBurn,
        BlendMode::Lighten => Mix::Lighten,
        BlendMode::Screen => Mix::Screen,
        BlendMode::PlusLighter => return peniko::BlendMode::new(Mix::Normal, Compose::Plus),
        BlendMode::ColorDodge => Mix::ColorDodge,
        BlendMode::Overlay => Mix::Overlay,
        BlendMode::SoftLight => Mix::SoftLight,
        BlendMode::HardLight => Mix::HardLight,
        BlendMode::Difference => Mix::Difference,
        BlendMode::Exclusion => Mix::Exclusion,
        BlendMode::Hue => Mix::Hue,
        BlendMode::Saturation => Mix::Saturation,
        BlendMode::Color => Mix::Color,
        BlendMode::Luminosity => Mix::Luminosity,
    };
    m.into()
}

fn normal(b: BlendMode) -> bool {
    matches!(b, BlendMode::Normal | BlendMode::PassThrough)
}

/// A gradient's handles: the unit box → local mapping (as `figcraft_render::paint`).
fn handle_frame(g: &Gradient, w: f64, h: f64) -> Affine {
    let f = (g.from.0 * w, g.from.1 * h);
    let t = (g.to.0 * w, g.to.1 * h);
    let k = (g.width.0 * w, g.width.1 * h);
    Affine::new([t.0 - f.0, t.1 - f.1, k.0 - f.0, k.1 - f.1, f.0, f.1])
}

fn ts_affine(t: tiny_skia::Transform) -> Affine {
    Affine::new([f64::from(t.sx), f64::from(t.ky), f64::from(t.kx), f64::from(t.sy), f64::from(t.tx), f64::from(t.ty)])
}

impl Builder<'_> {
    /// A paint as the CPU renderer's pixels at `scale` (the scene now belongs to it).
    fn sample(&mut self, p: &Paint, w: f64, h: f64, scale: f64) -> Option<Prepared> {
        self.sampled = true;
        prepare_at(p, w, h, scale, self.doc, 0.0)
    }

    /// The page's layers (or only `solo`, where it is).
    pub fn page(&mut self, sc: &mut Scene, page: NodeId, solo: Option<NodeId>) -> Result<(), Fallback> {
        let doc = self.doc;
        if let Some(id) = solo.filter(|id| *id != page && doc.page_of(*id) == Some(page)) {
            let parent = doc.node(id).and_then(|n| n.parent).map(|p| doc.world_transform(p)).unwrap_or(Affine::IDENTITY);
            return self.node(sc, id, parent, 0);
        }
        for c in doc.node(page).map(|p| p.children.as_slice()).unwrap_or_default() {
            self.node(sc, *c, Affine::IDENTITY, 0)?;
        }
        Ok(())
    }

    /// Node `id`, where `parent_xf` maps its parent's space to the page.
    pub fn node(&mut self, sc: &mut Scene, id: NodeId, parent_xf: Affine, depth: usize) -> Result<(), Fallback> {
        if depth > 256 || self.skip.contains(&id) {
            return Ok(());
        }
        let doc = self.doc;
        let Some(n) = doc.node(id) else { return Ok(()) };
        if !n.visible || n.opacity <= 0.0 {
            return Ok(());
        }
        if n.is_mask {
            return Err("masks");
        }
        if n.effects.iter().any(|e| e.visible) {
            return Err("effects (shadows, blurs, glass, noise, texture)");
        }
        let xf = parent_xf * n.transform;
        let layer = n.opacity < 0.999 || !normal(n.blend);
        if layer {
            // An isolated group over what the node paints (page coordinates).
            let b = figcraft_render::paint_bounds(doc, id).unwrap_or(Rect::ZERO).inflate(1.0, 1.0);
            sc.push_layer(Fill::NonZero, mode(n.blend), n.opacity.clamp(0.0, 1.0), Affine::IDENTITY, &b);
        }
        self.content(sc, n, xf, depth)?;
        if layer {
            sc.pop_layer();
        }
        Ok(())
    }

    fn children(&mut self, sc: &mut Scene, n: &Node, xf: Affine, depth: usize) -> Result<(), Fallback> {
        for c in &n.children {
            self.node(sc, *c, xf, depth + 1)?;
        }
        Ok(())
    }

    /// Fills, children (clipped), strokes and text of a node (`draw::draw_content`).
    fn content(&mut self, sc: &mut Scene, n: &Node, xf: Affine, depth: usize) -> Result<(), Fallback> {
        let doc = self.doc;
        let (w, h) = (n.width, n.height);
        match &n.kind {
            NodeKind::Page { .. } | NodeKind::Group => self.children(sc, n, xf, depth),
            NodeKind::Text(_) => self.text(sc, n, xf),
            _ => {
                let closed = figcraft_doc::geometry::is_closed(n) || matches!(n.kind, NodeKind::BooleanOp { .. });
                let clips = n.clip && n.kind.is_frame_like() && !n.children.is_empty();
                let outline = figcraft_render::node_outline(doc, n);
                if closed && let Some((path, rule)) = &outline {
                    self.fills(sc, path, *rule, &n.fills, w, h, xf)?;
                }
                if !matches!(n.kind, NodeKind::BooleanOp { .. }) && !n.children.is_empty() && !n.hides_contents() {
                    if clips {
                        // The CPU draws clipped children into a layer of their own: an isolated group.
                        match &outline {
                            Some((p, _)) if n.corners.uniform() != Some(0.0) => sc.push_layer(Fill::NonZero, Mix::Normal, 1.0, xf, p),
                            _ => sc.push_layer(Fill::NonZero, Mix::Normal, 1.0, xf, &n.local_rect()),
                        }
                        self.children(sc, n, xf, depth)?;
                        sc.pop_layer();
                    } else {
                        self.children(sc, n, xf, depth)?;
                    }
                }
                if let Some((area, rule)) = figcraft_render::stroke_fill(doc, n) {
                    self.fills(sc, &area, rule, &n.strokes, w, h, xf)?;
                }
                Ok(())
            }
        }
    }

    /// Text as glyph outlines, grouped by style (as the CPU renderer does).
    fn text(&mut self, sc: &mut Scene, n: &Node, xf: Affine) -> Result<(), Fallback> {
        if self.lod {
            // How text looks depends on the scale: the scene belongs to it.
            self.sampled = true;
            let scale = xf.determinant().abs().sqrt() * self.scale;
            if figcraft_render::lod::greeked(n, scale) {
                for (bar, c) in figcraft_render::lod::greek_bars(n, figcraft_render::draw::text_box_bounds(n)) {
                    sc.fill(Fill::NonZero, xf, peniko::Color::new(c), None, &bar);
                }
                return Ok(());
            }
        }
        let Some((layout, dy)) = figcraft_text::layout_node(n) else { return Ok(()) };
        let gxf = xf * Affine::translate((0.0, dy));
        let mut by_style: HashMap<usize, BezPath> = HashMap::new();
        for g in &layout.glyphs {
            by_style.entry(g.style).or_default().extend(g.path().elements().iter().copied());
        }
        for d in &layout.decorations {
            by_style.entry(d.style).or_default().extend(d.rect.to_path(0.1).elements().iter().copied());
        }
        let mut keys: Vec<usize> = by_style.keys().copied().collect();
        keys.sort_unstable();
        for k in &keys {
            let Some(path) = by_style.get(k) else { continue };
            let fills = layout.styles.get(*k).and_then(|s| s.fills.as_ref()).unwrap_or(&n.fills);
            self.fills(sc, path, FillRule::Winding, fills, n.width, n.height, gxf)?;
        }
        if n.has_visible_stroke() {
            let props = figcraft_doc::StrokeProps { align: StrokeAlign::Center, ..n.stroke.clone() };
            for path in keys.iter().filter_map(|k| by_style.get(k)) {
                let s = figcraft_pathops::stroke_center(path, props.weight, &props);
                self.fills(sc, &s, FillRule::Winding, &n.strokes, n.width, n.height, gxf)?;
            }
        }
        Ok(())
    }

    fn fills(&mut self, sc: &mut Scene, path: &BezPath, rule: FillRule, paints: &[Paint], w: f64, h: f64, xf: Affine) -> Result<(), Fallback> {
        let scale = xf.determinant().abs().sqrt() * self.scale;
        let style = fill_rule(rule);
        for p in paints {
            let Some((brush, brush_xf)) = self.brush(p, w, h, scale, xf)? else { continue };
            let blend = !normal(p.blend);
            if blend {
                sc.push_layer(style, mode(p.blend), 1.0, xf, path);
            }
            sc.fill(style, xf, &brush, brush_xf, path);
            if blend {
                sc.pop_layer();
            }
        }
        Ok(())
    }

    /// A paint as a vello brush for a `w`×`h` local box that `xf` maps to the page (`None`: draws
    /// nothing).
    fn brush(&mut self, p: &Paint, w: f64, h: f64, scale: f64, xf: Affine) -> Result<Option<(peniko::Brush, Option<Affine>)>, Fallback> {
        if !p.visible || p.opacity <= 0.0 {
            return Ok(None);
        }
        let op = p.opacity.clamp(0.0, 1.0);
        match &p.kind {
            PaintKind::Solid { color } => {
                let c = color.with_alpha(color.a * op);
                Ok((c.a > 0.0).then(|| (peniko::Brush::Solid(pc(c)), None)))
            }
            PaintKind::Gradient(g) if g.kind != GradientKind::Diamond => {
                let frame = handle_frame(g, w.max(1e-6), h.max(1e-6));
                let det = frame.determinant().abs();
                if det.is_nan() || det <= 1e-12 {
                    return Ok(None);
                }
                let stops: Vec<peniko::ColorStop> =
                    g.sorted_stops().into_iter().map(|s| (s.position, pc(s.color.with_alpha(s.color.a * op))).into()).collect();
                let (grad, brush_xf) = match g.kind {
                    GradientKind::Linear => {
                        // vello projects linear gradients onto the end points' line after the
                        // transform, which loses a skewed handle frame: give it the line in page
                        // space instead (the view only scales and moves, keeping angles).
                        let m = xf * frame;
                        let det = m.determinant();
                        if !det.is_finite() || det.abs() <= 1e-12 || xf.determinant().abs() <= 1e-12 {
                            return Ok(None);
                        }
                        let c = m.inverse().as_coeffs();
                        let (nx, ny) = (c[0], c[2]);
                        let p0 = m * figcraft_geom::Point::ZERO;
                        let k = 1.0 / (nx * nx + ny * ny);
                        (peniko::Gradient::new_linear(p0, (p0.x + nx * k, p0.y + ny * k)), xf.inverse())
                    }
                    GradientKind::Radial => (peniko::Gradient::new_radial((0.0, 0.0), 1.0), frame),
                    _ => (peniko::Gradient::new_sweep((0.0, 0.0), 0.0, std::f32::consts::TAU), frame),
                };
                Ok(Some((peniko::Brush::Gradient(grad.with_stops(stops.as_slice())), Some(brush_xf))))
            }
            // Pattern cells are drawn at the zoom they show at: not in a zoom-independent scene.
            PaintKind::Pattern(_) => Err("pattern fills"),
            // Images and diamond gradients: the CPU renderer's pixmap, sampled on the GPU.
            _ => match self.sample(p, w, h, scale) {
                Some(Prepared::Pattern { pixmap, mode, quality, opacity, transform }) => {
                    let extend = match mode {
                        tiny_skia::SpreadMode::Pad => Extend::Pad,
                        tiny_skia::SpreadMode::Repeat => Extend::Repeat,
                        tiny_skia::SpreadMode::Reflect => Extend::Reflect,
                    };
                    let quality = match quality {
                        tiny_skia::FilterQuality::Nearest => ImageQuality::Low,
                        tiny_skia::FilterQuality::Bilinear => ImageQuality::Medium,
                        tiny_skia::FilterQuality::Bicubic => ImageQuality::High,
                    };
                    let sampler = ImageSampler { x_extend: extend, y_extend: extend, quality, alpha: opacity };
                    let image = ImageBrush { image: self.images.get(&pixmap), sampler };
                    Ok(Some((peniko::Brush::Image(image), Some(ts_affine(transform)))))
                }
                Some(Prepared::Shader(_)) => Err("paint"),
                None => Ok(None),
            },
        }
    }
}

//! The scene graph as a vello scene, in page coordinates. Mirrors `figcraft_render::draw` for
//! the subset the GPU draws; anything else is a [`Fallback`] and the frame renders on the CPU.
//! Drop shadows, layer blurs and background blurs are images the scene draws, made on the GPU
//! before it renders ([`crate::effects`]).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use figcraft_color::{BlendMode, Color, Gradient, GradientKind, Paint, PaintKind};
use figcraft_doc::{Document, EffectKind, Node, NodeId, NodeKind, Shadow, StrokeAlign};
use figcraft_geom::{Affine, BezPath, Rect, Shape};
use figcraft_render::paint::{Prepared, prepare_at};
use figcraft_render::{FillRule, Pixmap};
use vello::Scene;
use vello::peniko::{self, Blob, Compose, Extend, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality, ImageSampler, Mix};

use crate::effects::{self, FxNode, Kind, Pass, Source};

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
    /// Take in the images `other` made (built on another thread).
    pub fn absorb(&mut self, other: Images) {
        if self.0.len() + other.0.len() > 512 {
            self.0.clear();
        }
        self.0.extend(other.0);
    }

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
    /// The top-level layer being built.
    pub top: NodeId,
    /// The layers with effects built so far, each after those inside it.
    pub fx: Vec<Arc<FxNode>>,
    /// Layers pushed and not yet popped in the top-level layer's scene.
    pub open: usize,
    /// How deep inside the content of layers with effects the build is (`0`: in the top-level
    /// layer's scene).
    pub in_fx: usize,
    /// Inside a layer with effects whose content holds a background blur: what lies under that
    /// content in its top-level layer (page coordinates).
    pub under: Option<Scene>,
    /// Where images are drawn (`None`: everywhere); those outside it are left out.
    pub area: Option<Rect>,
    /// Whether an image was left out (the scene then shows only `area` as it is).
    pub partial: bool,
    /// Where each image is drawn (by blob, page coordinates): a render leaves out those it doesn't
    /// show (vello's atlas takes every image of a scene, and too many draw as nothing).
    pub drawn: HashMap<u64, Rect>,
}

/// Why the GPU can't draw top-level layer `top`, found without building it: what the build would
/// stop at, through the layers it draws (hidden ones and a boolean's operands aren't).
pub fn cpu_only(doc: &Document, top: NodeId) -> Option<Fallback> {
    let mut stack = vec![(top, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let Some(n) = doc.node(id) else { continue };
        if depth > 256 || !n.visible || n.opacity <= 0.0 {
            continue;
        }
        // A mask among the page's own layers masks other top-level layers (built apart).
        if n.is_mask && id == top {
            return Some("masks");
        }
        if n.fills.iter().chain(&n.strokes).any(|p| p.visible && matches!(p.kind, PaintKind::Pattern(_))) {
            return Some("pattern fills");
        }
        if n.audio.is_some() {
            return Some("audio waveforms");
        }
        if n.effects.iter().any(|e| e.visible && matches!(e.kind, EffectKind::Texture(_) | EffectKind::Glass(_))) {
            return Some("effects (texture, glass)");
        }
        let kids = match n.kind {
            NodeKind::Page { .. } | NodeKind::Group => true,
            NodeKind::Text(_) | NodeKind::BooleanOp { .. } => false,
            _ => !n.hides_contents(),
        };
        if kids {
            stack.extend(n.children.iter().map(|c| (*c, depth + 1)));
        }
    }
    None
}

/// `sc` as drawn so far, with its `open` layers closed.
fn closed(sc: &Scene, open: usize) -> Scene {
    let mut c = sc.clone();
    for _ in 0..open {
        c.pop_layer();
    }
    c
}

/// Bytes an effect's image stands for until its texture replaces them: zeros (transparent),
/// shared by every image, so a scene drawn before its effects were made shows none of them.
struct Zeros(usize);

impl AsRef<[u8]> for Zeros {
    fn as_ref(&self) -> &[u8] {
        static Z: OnceLock<Vec<u8>> = OnceLock::new();
        let z = Z.get_or_init(|| vec![0; effects::MAX_SIDE as usize * effects::MAX_SIDE as usize * 4]);
        z.get(..self.0).unwrap_or_default()
    }
}

/// A new image of `w`×`h` an effect's texture will replace.
pub(crate) fn placeholder(w: u32, h: u32, premultiplied: bool) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new(Zeros(w as usize * h as usize * 4))),
        format: ImageFormat::Rgba8,
        alpha_type: if premultiplied { ImageAlphaType::AlphaPremultiplied } else { ImageAlphaType::Alpha },
        width: w,
        height: h,
    }
}

/// `outlines` (local, through `xf` to the page) grown or shrunk by `spread` local units, in white:
/// their fill, and a band twice the spread wide along them with mitred joins drawn over it (to
/// grow) or cut out of it (to shrink), as `figcraft_render::shadow::spread_alpha` does.
fn spread_shape(outlines: &[figcraft_render::shadow::Outline], spread: f64, xf: Affine, region: Rect) -> Scene {
    let white = peniko::Color::new([1.0, 1.0, 1.0, 1.0]);
    let mut sc = Scene::new();
    for (path, rule) in outlines {
        sc.fill(fill_rule(*rule), xf, white, None, path);
    }
    let band = vello::kurbo::Stroke::new(spread.abs() * 2.0).with_join(vello::kurbo::Join::Miter).with_miter_limit(figcraft_doc::SPREAD_MITER_LIMIT);
    let shrink = spread < 0.0;
    if shrink {
        sc.push_layer(Fill::NonZero, peniko::BlendMode::new(Mix::Normal, Compose::DestOut), 1.0, Affine::IDENTITY, &region);
    }
    for (path, _) in outlines {
        sc.stroke(&band, xf, white, None, path);
    }
    if shrink {
        sc.pop_layer();
    }
    sc
}

/// An effect drawn over a node's fills through its outline (an inner shadow).
struct OverFill<'a> {
    pass: &'a Pass,
    path: &'a BezPath,
    rule: FillRule,
    /// Local → page.
    xf: Affine,
    scale: f64,
}

impl OverFill<'_> {
    fn draw(&self, sc: &mut Scene) {
        let (brush, bxf) = fx_brush(self.scale, self.pass);
        sc.push_layer(fill_rule(self.rule), Mix::Normal, 1.0, self.xf, self.path);
        sc.fill(Fill::NonZero, Affine::IDENTITY, &brush, Some(bxf), &self.pass.region);
        sc.pop_layer();
    }
}

/// Device pixels of a layer's textures past which they cover only what a build needs.
const CROP_PX: f64 = 1024.0 * 1024.0;

/// How far (page units) a layer's effects reach: what its textures need around what they show.
fn reach(shadows: &[&Shadow], inners: &[&Shadow], layer_var: f64, back: Option<f64>, scale: f64) -> f64 {
    let finite = |v: f64| if v.is_finite() { v.abs() } else { 0.0 };
    let mut sigma = layer_var.max(0.0).sqrt();
    let mut reach = 3.0 * sigma;
    for s in shadows.iter().chain(inners) {
        let sg = ((finite(s.radius) / 2.0).powi(2) + layer_var.max(0.0)).sqrt();
        sigma = sigma.max(sg);
        reach = reach.max(3.0 * sg + finite(s.spread) + finite(s.offset.0).max(finite(s.offset.1)));
    }
    if let Some(b) = back {
        sigma = sigma.max(finite(b));
        reach = reach.max(3.0 * finite(b));
    }
    // A texture drawn smaller is off by up to two of its pixels at an edge.
    reach + (2.0 * f64::from(effects::factor(sigma * scale)) + 2.0) / scale
}

/// The brush that draws a pass's image over its page area (image pixels → page).
fn fx_brush(scale: f64, p: &Pass) -> (peniko::Brush, Affine) {
    let sampler = ImageSampler { x_extend: Extend::Pad, y_extend: Extend::Pad, quality: ImageQuality::Medium, alpha: 1.0 };
    let xf = Affine::translate((p.region.x0, p.region.y0)) * Affine::scale(f64::from(p.out.max(1)) / scale);
    (peniko::Brush::Image(ImageBrush { image: p.image.clone(), sampler }), xf)
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
            self.top = *c;
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
        let xf = parent_xf * n.transform;
        if n.effects.iter().any(|e| e.visible) {
            return self.with_effects(sc, n, xf, depth);
        }
        let layer = n.opacity < 0.999 || !normal(n.blend);
        if layer {
            // An isolated group over what the node paints (page coordinates).
            let b = figcraft_render::paint_bounds(doc, id).unwrap_or(Rect::ZERO).inflate(1.0, 1.0);
            sc.push_layer(Fill::NonZero, mode(n.blend), n.opacity.clamp(0.0, 1.0), Affine::IDENTITY, &b);
            self.open += 1;
        }
        let r = self.content(sc, n, xf, depth, &[]);
        if layer {
            sc.pop_layer();
            self.open -= 1;
        }
        r
    }

    /// A node with effects (`draw::draw_layer`): its drop shadows under it, then in its layer
    /// the blurred backdrop through its shape, its content (inner shadows over its fills; blurred,
    /// with a layer blur) and its grains.
    fn with_effects(&mut self, sc: &mut Scene, n: &Node, xf: Affine, depth: usize) -> Result<(), Fallback> {
        let doc = self.doc;
        let mut shadows: Vec<&Shadow> = Vec::new();
        let mut inners: Vec<&Shadow> = Vec::new();
        let mut noises: Vec<&figcraft_doc::Noise> = Vec::new();
        // Blurs one after another blur as one Gaussian (their variances add).
        let mut layer_var = 0.0;
        let mut back = None;
        for e in n.effects.iter().filter(|e| e.visible) {
            match &e.kind {
                EffectKind::DropShadow(s) => shadows.push(s),
                EffectKind::InnerShadow(s) => inners.push(s),
                // The CPU blurs grains drawn before a layer blur.
                EffectKind::LayerBlur(b) if !b.is_progressive() && noises.is_empty() => layer_var += (b.sanitized().radius / 2.0).powi(2),
                EffectKind::BackgroundBlur(b) if !b.is_progressive() && back.is_none() => back = Some(b.sanitized().radius / 2.0),
                EffectKind::Noise(nz) => noises.push(nz),
                _ => return Err("effects (texture, glass, progressive or repeated background blurs, noise under a layer blur)"),
            }
        }
        let back = back.filter(|s| *s > 0.0);
        if back.is_some() && layer_var > 0.0 {
            return Err("a background blur with a layer blur");
        }
        let scale = self.scale;
        if !(scale.is_finite() && scale > 0.0) {
            return Err("scale");
        }
        // Everything the node and its effects paint, in page units, out to whole pixels (grains
        // land on the pixels the CPU draws them on).
        let Some(r) = figcraft_render::paint_bounds(doc, n.id).map(|r| r.inflate(1.0, 1.0)) else { return Ok(()) };
        let snap = |r: Rect| {
            Rect::new((r.x0 * scale).floor() / scale, (r.y0 * scale).floor() / scale, (r.x1 * scale).ceil() / scale, (r.y1 * scale).ceil() / scale)
        };
        let full = snap(r);
        // A large layer's textures cover only what the build needs (its area, and as far as each
        // effect reaches into it): whole, the textures of layers with effects inside one another
        // stack more pixels on a spot than vello's image atlas holds. The scene then shows only
        // its area as it is, and is built again past it.
        let area = self.area.filter(|_| full.width() * full.height() * scale * scale > CROP_PX);
        let crop = |reach: f64| match area {
            Some(a) => snap(full.intersect(a.inflate(reach, reach))).intersect(full),
            None => full,
        };
        let region = crop(reach(&shadows, &inners, layer_var, back, scale));
        if !(region.width() > 0.0 && region.height() > 0.0) {
            self.partial = true;
            return Ok(());
        }
        let cropped = region != full;
        if cropped {
            self.partial = true;
        }
        // The scene's pixels (effects drawn smaller) belong to its scale.
        self.sampled = true;
        // `reach`: how far (page units) the effect reaches, its texture drawn smaller aside.
        let pass = |source, kind_of: &dyn Fn(f64) -> Kind, sigma_page: f64, reach: f64, premultiplied: bool, shape: Option<Scene>| {
            let mut k = effects::factor(sigma_page * scale);
            loop {
                // Off by up to two of its pixels at an edge, drawn smaller.
                let r = crop(reach + (2.0 * f64::from(k) + 2.0) / scale);
                let r = if r.width() > 0.0 && r.height() > 0.0 { r } else { region };
                if let Some(src) = effects::texture_size(r, scale, k) {
                    let px = scale / f64::from(k);
                    let kind = kind_of(px);
                    // A blur or shadow made smaller is scaled up to the scene's pixels (dithered
                    // there), as far as the atlas allows.
                    let mut out = if matches!(kind, Kind::Blur { .. } | Kind::Shadow { .. }) { 1 } else { k };
                    let mut size = effects::texture_size(r, scale, out);
                    while out < k && size.is_none_or(|(w, h)| u64::from(w) * u64::from(h) > effects::MAX_UP_PX) {
                        out *= 2;
                        size = effects::texture_size(r, scale, out);
                    }
                    let (w, h) = size.unwrap_or(src);
                    return Ok(Pass { source, kind, k, out, src, shape, image: placeholder(w, h, premultiplied), region: r });
                }
                if k >= 64 {
                    return Err("an effect too large for the GPU");
                }
                k *= 2;
            }
        };
        let finite = |v: f64| if v.is_finite() { v.abs() } else { 0.0 };
        let shadow_reach = |s: &Shadow, sigma: f64| 3.0 * sigma + finite(s.spread) + finite(s.offset.0).max(finite(s.offset.1));
        let shape = figcraft_render::node_outline(doc, n);
        let is_group = matches!(n.kind, NodeKind::Group);
        // Inner shadows of a layer that is one outline: what lies outside it (shrunk by the spread)
        // at the offset, blurred, over the fills and through the outline.
        let mut inner_passes = Vec::new();
        if !is_group && let Some((path, rule)) = &shape {
            for s in &inners {
                let sigma = s.radius.max(0.0) / 2.0;
                let spread = if s.spread.is_finite() { s.spread } else { 0.0 };
                let at = Affine::translate(s.offset) * xf;
                let cast = spread_shape(&[(path.clone(), *rule)], -spread, at, region);
                let color = s.color;
                inner_passes.push(pass(
                    Source::Content,
                    &|px| Kind::Shadow { sigma: sigma * px, spread: 0.0, color, inside: true },
                    sigma,
                    shadow_reach(s, sigma),
                    false,
                    Some(cast),
                )?);
            }
        }
        let over: Vec<OverFill> =
            inner_passes.iter().filter_map(|p| shape.as_ref().map(|(path, rule)| OverFill { pass: p, path, rule: *rule, xf, scale })).collect();
        // The content on its own (layers with effects inside it are made first, and drawn as
        // their images in it).
        let mut content = Scene::new();
        // A background blur inside sees what lies under this layer, then the content before it.
        // (Its drop shadows aren't made yet then: they are missing from what it blurs.)
        let nested_back = doc.subtree(n.id).iter().skip(1).any(|c| doc.node(*c).is_some_and(figcraft_render::backdrop::has_effects));
        let under = nested_back.then(|| {
            let mut u = Scene::new();
            if let Some(o) = &self.under {
                u.append(o, None);
            }
            u.append(&closed(sc, self.open), None);
            u
        });
        let under = std::mem::replace(&mut self.under, under);
        let open = std::mem::replace(&mut self.open, 0);
        // A cropped layer's content is needed over its textures.
        let outer = self.area;
        if cropped {
            self.area = Some(region);
        }
        let fx_before = self.fx.len();
        self.in_fx += 1;
        let built = self.content(&mut content, n, xf, depth, &over);
        self.in_fx -= 1;
        let nested = self.fx.len() > fx_before;
        self.area = outer;
        self.open = open;
        self.under = under;
        built?;
        drop(over);
        // A group's inner shadows: cast from around what its layers draw together, over them.
        let mut group_inner = Vec::new();
        if is_group {
            for s in &inners {
                let sigma = s.radius.max(0.0) / 2.0;
                let spread = if s.spread.is_finite() { s.spread } else { 0.0 };
                let mut cast = Scene::new();
                cast.append(&content, Some(Affine::translate(s.offset)));
                let color = s.color;
                group_inner.push(pass(
                    Source::Content,
                    &|px| Kind::Shadow { sigma: sigma * px, spread: -spread * px, color, inside: true },
                    sigma,
                    shadow_reach(s, sigma),
                    false,
                    Some(cast),
                )?);
            }
        }

        // A spread moves the outline of a layer that is one (`figcraft_render::shadow`); other
        // layers spread their pixels.
        let silhouette = if shadows.iter().any(|s| s.spread != 0.0) { figcraft_render::shadow::silhouette(doc, n) } else { None };
        let mut shadow_passes = Vec::new();
        for s in &shadows {
            // A shadow cast from a blurred layer: the two blurs as one.
            let sigma = ((s.radius.max(0.0) / 2.0).powi(2) + layer_var).sqrt();
            let color = s.color;
            let cast = silhouette.as_ref().filter(|_| s.spread != 0.0 && s.spread.is_finite()).map(|o| spread_shape(o, s.spread, xf, region));
            let spread = if cast.is_some() { 0.0 } else { s.spread };
            shadow_passes.push(pass(
                Source::Content,
                &|px| Kind::Shadow { sigma: sigma * px, spread: spread * px, color, inside: false },
                sigma,
                shadow_reach(s, sigma),
                false,
                cast,
            )?);
        }
        let blurred = if layer_var > 0.0 {
            let sigma = layer_var.sqrt();
            Some(pass(Source::Content, &|px| Kind::Blur { sigma: sigma * px }, sigma, 3.0 * sigma, true, None)?)
        } else {
            None
        };
        let backdrop = match back {
            Some(sigma) => Some(pass(Source::Backdrop, &|px| Kind::Blur { sigma: sigma * px }, sigma, 3.0 * sigma, true, None)?),
            None => None,
        };
        // A layer holding layers with effects draws its content once, into a texture of its own:
        // drawn as it is, each render would hold the textures of every layer nested in it at once
        // (more than vello's image atlas holds, on a page of sections of cards with shadows).
        let flat = match &blurred {
            None if nested => pass(Source::Content, &|_| Kind::Flat, 0.0, 0.0, false, None).ok().filter(|p| p.k == 1),
            _ => None,
        };
        let mut noise_passes = Vec::new();
        for nz in &noises {
            let density = if nz.density.is_finite() { nz.density.clamp(0.0, 1.0) as f32 } else { 0.0 };
            if density <= 0.0 {
                continue;
            }
            let (gx, gy) = nz.grain();
            let (gx, gy) = (gx * 2.0, gy * 2.0);
            let nr = crop(2.0 / scale);
            let nr = if nr.width() > 0.0 && nr.height() > 0.0 { nr } else { region };
            let tex = Affine::scale(scale) * Affine::translate((-nr.x0, -nr.y0)) * xf;
            let det = tex.determinant();
            if !(det.is_finite() && det.abs() > 1e-12) {
                continue;
            }
            let cells = (Affine::scale_non_uniform(1.0 / gx, 1.0 / gy) * tex.inverse()).as_coeffs();
            let g = effects::Grains {
                cells,
                seed: n.id.0 as u32,
                mode: match nz.mode {
                    figcraft_doc::NoiseMode::Mono => 0,
                    figcraft_doc::NoiseMode::Duo => 1,
                    figcraft_doc::NoiseMode::Multi => 2,
                },
                density,
                c1: nz.color.sanitized(),
                c2: nz.secondary.sanitized(),
                multi_alpha: if nz.opacity.is_finite() { nz.opacity.clamp(0.0, 1.0) } else { 0.0 },
            };
            let (w, h) = effects::texture_size(nr, scale, 1).ok_or("noise too large for the GPU")?;
            let image = placeholder(w, h, true);
            noise_passes
                .push((Pass { source: Source::Content, kind: Kind::Noise(g), k: 1, out: 1, src: (w, h), shape: None, image, region: nr }, nz.blend));
        }

        // The content, and what the layer's pixels are (blurred, with a layer blur): hide grains
        // and group inner shadows outside them.
        let inline = |sc: &mut Scene| match &flat {
            Some(f) => {
                let (brush, bxf) = fx_brush(scale, f);
                sc.fill(Fill::NonZero, Affine::IDENTITY, &brush, Some(bxf), &f.region);
            }
            None => sc.append(&content, None),
        };
        let pixels = |sc: &mut Scene| match &blurred {
            Some(b) => {
                let (brush, bxf) = fx_brush(scale, b);
                sc.fill(Fill::NonZero, Affine::IDENTITY, &brush, Some(bxf), &b.region);
            }
            None => inline(sc),
        };
        let op = n.opacity.clamp(0.0, 1.0);
        let cover = figcraft_render::shadow::hiding_cover(doc, n);
        for (s, p) in shadows.iter().zip(&shadow_passes) {
            let at = Affine::translate(s.offset);
            let (brush, bxf) = fx_brush(scale, p);
            sc.push_layer(Fill::NonZero, mode(s.blend), op, at, &region);
            sc.fill(Fill::NonZero, at, &brush, Some(bxf), &p.region);
            if !s.show_behind {
                // Hidden behind what the layer's paints cover (or its pixels).
                let moved = Rect::new(region.x0 + s.offset.0, region.y0 + s.offset.1, region.x1 + s.offset.0, region.y1 + s.offset.1);
                sc.push_layer(Fill::NonZero, peniko::BlendMode::new(Mix::Normal, Compose::DestOut), 1.0, Affine::IDENTITY, &region.union(moved));
                match (&cover, &blurred) {
                    (Some(outlines), _) => {
                        for (path, rule) in outlines {
                            sc.fill(fill_rule(*rule), xf, peniko::Color::new([1.0, 1.0, 1.0, 1.0]), None, path);
                        }
                    }
                    (None, _) => pixels(sc),
                }
                sc.pop_layer();
            }
            sc.pop_layer();
        }
        let layer = op < 0.999 || !normal(n.blend);
        if layer {
            sc.push_layer(Fill::NonZero, mode(n.blend), op, Affine::IDENTITY, &region);
            self.open += 1;
        }
        let mut prefix = None;
        if let Some(p) = &backdrop {
            // What lies under it in its top-level layer: everything drawn so far, layers closed.
            let mut pre = Scene::new();
            if let Some(u) = &self.under {
                pre.append(u, None);
            }
            pre.append(&closed(sc, self.open), None);
            prefix = Some(pre);
            if let Some((path, rule)) = figcraft_render::backdrop::outline(doc, n) {
                let (brush, bxf) = fx_brush(scale, p);
                sc.push_layer(fill_rule(rule), Mix::Normal, 1.0, xf, &path);
                sc.fill(Fill::NonZero, Affine::IDENTITY, &brush, Some(bxf), &p.region);
                sc.pop_layer();
            }
        }
        pixels(sc);
        let dest_in = peniko::BlendMode::new(Mix::Normal, Compose::DestIn);
        for p in &group_inner {
            let (brush, bxf) = fx_brush(scale, p);
            sc.push_layer(Fill::NonZero, Mix::Normal, 1.0, Affine::IDENTITY, &region);
            sc.fill(Fill::NonZero, Affine::IDENTITY, &brush, Some(bxf), &p.region);
            sc.push_layer(Fill::NonZero, dest_in, 1.0, Affine::IDENTITY, &region);
            inline(sc);
            sc.pop_layer();
            sc.pop_layer();
        }
        for (p, blend) in &noise_passes {
            // Grains at the alpha of the pixels under them, blended with the noise's mode.
            let (brush, bxf) = fx_brush(scale, p);
            let brush = match brush {
                peniko::Brush::Image(mut im) => {
                    im.sampler.quality = ImageQuality::Low;
                    peniko::Brush::Image(im)
                }
                b => b,
            };
            sc.push_layer(Fill::NonZero, mode(*blend), 1.0, Affine::IDENTITY, &region);
            sc.fill(Fill::NonZero, Affine::IDENTITY, &brush, Some(bxf), &p.region);
            sc.push_layer(Fill::NonZero, dest_in, 1.0, Affine::IDENTITY, &region);
            pixels(sc);
            sc.pop_layer();
            sc.pop_layer();
        }
        if layer {
            sc.pop_layer();
            self.open -= 1;
        }
        // Made in this order: inner shadows are in the content the others are made from.
        let mut passes = inner_passes;
        passes.extend(group_inner);
        passes.extend(shadow_passes);
        passes.extend(blurred);
        passes.extend(flat);
        passes.extend(backdrop);
        passes.extend(noise_passes.into_iter().map(|(p, _)| p));
        // Its textures draw over its region (the content of a layer with effects around it holds them).
        for p in &passes {
            self.drawn.insert(p.image.data.id(), region);
        }
        self.fx.push(Arc::new(FxNode {
            id: n.id,
            top: self.top,
            content,
            prefix,
            region,
            scale,
            passes,
            made: Mutex::new(None),
            failed: OnceLock::new(),
        }));
        Ok(())
    }

    /// A node's children in paint order (`draw::draw_children`): a visible mask and the layers
    /// over it up to the next mask in a group of their own, clipped by the mask.
    fn children(&mut self, sc: &mut Scene, n: &Node, xf: Affine, depth: usize) -> Result<(), Fallback> {
        let doc = self.doc;
        let kids: Vec<Option<&Node>> = n.children.iter().map(|c| doc.node(*c)).collect();
        for run in Document::paint_runs_of(&kids, n.layout.as_ref().is_some_and(|l| l.reverse_z)) {
            let Some(&c) = n.children.get(run.start) else { continue };
            match kids.get(run.start).copied().flatten() {
                Some(m) if m.is_mask && m.visible => self.masked(sc, m, n.children.get(run.start + 1..run.end).unwrap_or(&[]), xf, depth)?,
                _ => self.node(sc, c, xf, depth + 1)?,
            }
        }
        Ok(())
    }

    /// `content` clipped by mask `m` (`draw::draw_masked`): drawn in a group, then the mask as if
    /// it weren't one (a vector mask: its outline, opaque) keeping the group's pixels by its alpha or
    /// brightness.
    fn masked(&mut self, sc: &mut Scene, m: &Node, content: &[NodeId], xf: Affine, depth: usize) -> Result<(), Fallback> {
        let doc = self.doc;
        let Some(b) = content.iter().filter_map(|c| figcraft_render::paint_bounds(doc, *c)).reduce(|a, b| a.union(b)) else { return Ok(()) };
        let b = b.inflate(1.0, 1.0);
        let mut m2 = m.clone();
        m2.is_mask = false;
        let mxf = xf * m2.transform;
        sc.push_layer(Fill::NonZero, Mix::Normal, 1.0, Affine::IDENTITY, &b);
        self.open += 1;
        for c in content {
            self.node(sc, *c, xf, depth + 1)?;
        }
        match m.mask_type {
            figcraft_doc::MaskType::Vector => {
                sc.push_layer(Fill::NonZero, peniko::BlendMode::new(Mix::Normal, Compose::DestIn), 1.0, Affine::IDENTITY, &b);
                if let Some((outline, rule)) = figcraft_render::vector_mask_outline(doc, &m2, depth + 1) {
                    sc.fill(fill_rule(rule), mxf, peniko::Color::new([1.0, 1.0, 1.0, 1.0]), None, &outline);
                }
            }
            kind => {
                if kind == figcraft_doc::MaskType::Luminance {
                    sc.push_luminance_mask_layer(Fill::NonZero, 1.0, Affine::IDENTITY, &b);
                } else {
                    sc.push_layer(Fill::NonZero, peniko::BlendMode::new(Mix::Normal, Compose::DestIn), 1.0, Affine::IDENTITY, &b);
                }
                let faded = m2.opacity < 0.999;
                if faded {
                    sc.push_layer(Fill::NonZero, Mix::Normal, m2.opacity.clamp(0.0, 1.0), Affine::IDENTITY, &b);
                }
                let r = self.content(sc, &m2, mxf, depth + 1, &[]);
                if faded {
                    sc.pop_layer();
                }
                r?;
            }
        }
        sc.pop_layer();
        sc.pop_layer();
        self.open -= 1;
        Ok(())
    }

    /// Fills, `over` them, children (clipped), strokes and text of a node (`draw::draw_content`).
    fn content(&mut self, sc: &mut Scene, n: &Node, xf: Affine, depth: usize, over: &[OverFill]) -> Result<(), Fallback> {
        let doc = self.doc;
        let (w, h) = (n.width, n.height);
        match &n.kind {
            NodeKind::Page { .. } | NodeKind::Group => self.children(sc, n, xf, depth),
            NodeKind::Text(_) => {
                self.text(sc, n, xf)?;
                over.iter().for_each(|o| o.draw(sc));
                Ok(())
            }
            // Waveforms, rulers and markers are drawn by the CPU renderer.
            _ if n.audio.is_some() => Err("audio waveforms"),
            _ => {
                let closed = figcraft_doc::geometry::is_closed(n) || matches!(n.kind, NodeKind::BooleanOp { .. });
                let clips = n.clip && n.kind.is_frame_like() && !n.children.is_empty();
                let outline = figcraft_render::node_outline(doc, n);
                if closed && let Some((path, rule)) = &outline {
                    self.fills(sc, path, *rule, &n.fills, w, h, xf)?;
                }
                over.iter().for_each(|o| o.draw(sc));
                if !matches!(n.kind, NodeKind::BooleanOp { .. }) && !n.children.is_empty() && !n.hides_contents() {
                    if clips {
                        // The CPU draws clipped children into a layer of their own: an isolated group.
                        match &outline {
                            Some((p, _)) if n.corners.uniform() != Some(0.0) => sc.push_layer(Fill::NonZero, Mix::Normal, 1.0, xf, p),
                            _ => sc.push_layer(Fill::NonZero, Mix::Normal, 1.0, xf, &n.local_rect()),
                        }
                        self.open += 1;
                        let r = self.children(sc, n, xf, depth);
                        sc.pop_layer();
                        self.open -= 1;
                        r?;
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
            if let peniko::Brush::Image(im) = &brush {
                let at = figcraft_geom::transform_rect_bbox(xf, path.bounding_box());
                self.drawn.entry(im.image.data.id()).and_modify(|r| *r = r.union(at)).or_insert(at);
            }
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
            // An image far from what is shown waits (turning it into pixels is the costly part of
            // a build): the scene covers only `area` until it is built again.
            PaintKind::Image(_)
                if self.area.is_some_and(|a| {
                    let b = figcraft_geom::transform_rect_bbox(xf, Rect::new(0.0, 0.0, w, h));
                    !(b.x0 < a.x1 && a.x0 < b.x1 && b.y0 < a.y1 && a.y0 < b.y1)
                }) =>
            {
                self.partial = true;
                Ok(None)
            }
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

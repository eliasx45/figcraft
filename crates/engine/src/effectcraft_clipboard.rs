//! One-time native clipboard export. This is a snapshot, not a link to either document.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use figcraft_color::{BlendMode, Color, ColorProfile, GradientKind, Paint, PaintKind};
use figcraft_doc::*;
use figcraft_geom::Affine;
use serde_json::{Value, json};

use crate::{Result, Session, bad};

const CMD: &str = "edit.copyForEffectCraft";
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
const MAX_NODES: usize = 20_000;
const MAX_DEPTH: usize = 64;

/// Counts encoded bytes without allocating a second copy, and stops at the clipboard budget.
struct Budget(usize);
impl Write for Budget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| io::Error::other("selection exceeds the 2 MiB clipboard limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = crate::params::req_targets(CMD, s, p)?;
    export(s.doc()?, &ids)
}

fn export(doc: &Document, ids: &[NodeId]) -> Result<Value> {
    let mut roots = crate::cmd::edit::roots(doc, ids);
    let mut unique = BTreeSet::new();
    roots.retain(|id| unique.insert(*id));
    if roots.is_empty() {
        return Err(bad(CMD, "select layers to copy"));
    }
    // Validate the entire source first, including boolean operands, before computing geometry.
    let mut stack: Vec<_> = roots.iter().map(|id| (*id, 0)).collect();
    let mut seen = BTreeSet::new();
    let mut budget = Budget(0);
    while let Some((id, depth)) = stack.pop() {
        if depth > MAX_DEPTH || seen.len() >= MAX_NODES || !seen.insert(id) {
            return Err(bad(CMD, "selection exceeds 64 levels / 20,000 layers, or contains a cycle"));
        }
        let n = doc.get(id)?;
        serde_json::to_writer(&mut budget, n).map_err(|e| bad(CMD, e.to_string()))?;
        if !n.transform.as_coeffs().iter().all(|x| x.is_finite()) || !n.width.is_finite() || !n.height.is_finite() {
            return Err(bad(CMD, "selection contains non-finite geometry"));
        }
        stack.extend(n.children.iter().map(|id| (*id, depth + 1)));
    }
    let mut ranks: BTreeMap<NodeId, BTreeMap<NodeId, usize>> = BTreeMap::new();
    roots.sort_by_cached_key(|id| {
        let mut chain = doc.ancestors(*id);
        chain.reverse();
        chain.push(*id);
        let mut key = vec![chain.first().and_then(|p| doc.pages.iter().position(|v| v == p)).unwrap_or(0)];
        for pair in chain.windows(2) {
            if let [parent, child] = pair {
                let order = ranks.entry(*parent).or_insert_with(|| {
                    doc.node(*parent).map(|n| paint_children(doc, n).into_iter().enumerate().map(|(i, id)| (id, i)).collect()).unwrap_or_default()
                });
                key.push(order.get(child).copied().unwrap_or(0));
            }
        }
        key
    });
    let mut cx = Export { doc, warnings: Vec::new() };
    if !doc.color_profile.is_srgb() {
        cx.warn("Colours converted to sRGB; out-of-gamut values may be clipped".into());
    }
    let nodes = roots.into_iter().map(|id| cx.node(id)).collect::<Result<Vec<_>>>()?;
    let payload = json!({"format": "figcraft.clipboard", "version": 1, "nodes": nodes, "warnings": cx.warnings});
    if has_null(&payload) {
        return Err(bad(CMD, "selection contains non-finite properties"));
    }
    serde_json::to_writer(Budget(0), &payload).map_err(|e| bad(CMD, e.to_string()))?;
    Ok(payload)
}

fn has_null(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Array(a) => a.iter().any(has_null),
        Value::Object(o) => o.values().any(has_null),
        _ => false,
    }
}

struct Export<'a> {
    doc: &'a Document,
    warnings: Vec<String>,
}
impl Export<'_> {
    fn warn(&mut self, message: String) {
        if self.warnings.len() < 200 && !self.warnings.contains(&message) {
            self.warnings.push(message);
        } else if self.warnings.len() == 200 {
            self.warnings.push("Further unsupported-feature warnings omitted".into());
        }
    }
    fn note(&mut self, n: &Node, message: &str) {
        self.warn(format!("{} ({}): {message}", n.name.chars().take(80).collect::<String>(), n.id));
    }
    fn color(&self, c: Color) -> Value {
        let c = self.doc.color_profile.conversion_to(ColorProfile::Srgb).apply(c);
        json!({"r": c.r, "g": c.g, "b": c.b, "a": c.a})
    }
    fn paints(&mut self, n: &Node, paints: &[Paint]) -> Vec<Value> {
        paints.iter().filter_map(|p| {
            let mut v = match &p.kind {
                PaintKind::Solid { color } => json!({"type": "SOLID", "color": self.color(*color)}),
                PaintKind::Gradient(g) => {
                    if g.kind != GradientKind::Linear { self.note(n, "non-linear gradients use EffectCraft's radial approximation"); }
                    json!({"type": match g.kind { GradientKind::Linear => "GRADIENT_LINEAR", GradientKind::Radial => "GRADIENT_RADIAL", GradientKind::Angular => "GRADIENT_ANGULAR", GradientKind::Diamond => "GRADIENT_DIAMOND" },
                        "gradientHandlePositions": [point(g.from), point(g.to), point(g.width)],
                        "gradientStops": g.stops.iter().map(|s| json!({"position": s.position, "color": self.color(s.color)})).collect::<Vec<_>>()})
                }
                PaintKind::Image(_) | PaintKind::Pattern(_) => { self.note(n, "image/pattern paint omitted; no raster fallback"); return None; }
            };
            if p.blend != BlendMode::Normal { self.note(n, "paint blend mode is not transferred"); }
            v["opacity"] = json!(p.opacity);
            v["visible"] = json!(p.visible);
            Some(v)
        }).collect()
    }
    fn effects(&mut self, n: &Node) -> Vec<Value> {
        n.effects.iter().filter_map(|e| {
            let mut v = match &e.kind {
                EffectKind::DropShadow(s) | EffectKind::InnerShadow(s) => {
                    if s.blend != BlendMode::Normal || s.show_behind { self.note(n, "shadow blend/show-behind settings are not transferred"); }
                    json!({"type": if matches!(&e.kind, EffectKind::DropShadow(_)) { "DROP_SHADOW" } else { "INNER_SHADOW" }, "color": self.color(s.color), "offset": point(s.offset), "radius": s.radius, "spread": s.spread})
                }
                EffectKind::LayerBlur(b) => {
                    if b.blur_type != BlurType::Uniform { self.note(n, "progressive blur becomes uniform"); }
                    json!({"type": "LAYER_BLUR", "radius": b.radius})
                }
                _ => { self.note(n, "background blur, noise, texture or glass omitted; no raster fallback"); return None; }
            };
            v["visible"] = json!(e.visible);
            Some(v)
        }).collect()
    }
    fn node(&mut self, id: NodeId) -> Result<Value> {
        let n = self.doc.get(id)?;
        let world = self.doc.world_transform(id);
        if !world.as_coeffs().iter().all(|v| v.is_finite()) || world.determinant().abs() < 1e-12 {
            return Err(bad(CMD, "selection has a non-finite or singular transform"));
        }
        let ty = match &n.kind {
            NodeKind::Page { .. } => "CANVAS",
            NodeKind::Frame => "FRAME",
            NodeKind::Group => "GROUP",
            NodeKind::Section { .. } => "SECTION",
            NodeKind::Rectangle => "RECTANGLE",
            NodeKind::Ellipse { .. } => "ELLIPSE",
            NodeKind::Line => "LINE",
            NodeKind::Text(_) => "TEXT",
            NodeKind::Slice => "SLICE",
            NodeKind::Component { .. } => "COMPONENT",
            NodeKind::ComponentSet { .. } => "COMPONENT_SET",
            NodeKind::Instance { .. } => "INSTANCE",
            _ => "VECTOR",
        };
        let bounds = self.doc.world_bounds(id).unwrap_or_default();
        let render_bounds = self.doc.world_render_bounds(id).unwrap_or(bounds);
        let mut v = json!({"id": id.to_string(), "name": n.name, "type": ty, "width": n.width, "height": n.height,
            "size": {"x": n.width, "y": n.height}, "absoluteTransform": matrix(world),
            "absoluteBoundingBox": {"x": bounds.x0, "y": bounds.y0, "width": bounds.width(), "height": bounds.height()},
            "absoluteRenderBounds": {"x": render_bounds.x0, "y": render_bounds.y0, "width": render_bounds.width(), "height": render_bounds.height()},
            "visible": n.visible, "opacity": n.opacity, "blendMode": blend(n.blend),
            "fills": self.paints(n, &n.fills), "strokes": self.paints(n, &n.strokes), "effects": self.effects(n),
            "strokeWeight": n.stroke.weight, "strokeAlign": match n.stroke.align { StrokeAlign::Inside => "INSIDE", StrokeAlign::Center => "CENTER", StrokeAlign::Outside => "OUTSIDE" },
            "strokeCap": match n.stroke.cap_start { StrokeCap::Round => "ROUND", StrokeCap::Square => "SQUARE", _ => "NONE" },
            "strokeJoin": match n.stroke.join { StrokeJoin::Round => "ROUND", StrokeJoin::Bevel => "BEVEL", StrokeJoin::Miter => "MITER" },
            "strokeMiterLimit": n.stroke.miter_limit, "strokeDashes": n.stroke.dashes,
            "rectangleCornerRadii": n.corners.radii, "clipsContent": n.clip,
            "isMask": n.is_mask, "maskType": if n.mask_type == MaskType::Luminance { "LUMINANCE" } else { "ALPHA" }});
        if n.is_mask && n.mask_type == MaskType::Vector {
            self.note(n, "vector mask uses alpha masking");
        }
        if n.stroke.sides.is_some() || n.stroke.profile().is_some() || n.stroke.brush.is_some() {
            self.note(n, "per-side, variable-width and brush strokes become uniform strokes");
        }
        if n.stroke.cap_start != n.stroke.cap_end || n.stroke.cap_start.is_arrow() {
            self.note(n, "different stroke ends and arrowheads are not transferred");
        }
        if n.stroke.dashes.len() > 2 {
            self.note(n, "only the first dash and gap are transferred");
        }
        if n.corners.smoothing > 0.0 && ty != "VECTOR" {
            self.note(n, "corner smoothing is not transferred");
        }
        if matches!(&n.kind, NodeKind::Slice) {
            self.note(n, "slice omitted by EffectCraft");
        }
        if matches!(&n.kind, NodeKind::Component { .. } | NodeKind::ComponentSet { .. } | NodeKind::Instance { .. }) {
            self.note(n, "component relationships become ordinary editable layers");
        }
        if !n.bound.is_empty() || !n.styles.is_empty() || n.layout.is_some() {
            self.note(n, "styles, variables and auto layout use their current resolved values");
        }
        if !n.interactions.is_empty() {
            self.note(n, "prototype interactions are not transferred");
        }
        if let NodeKind::Ellipse { start, sweep, inner } = &n.kind {
            v["arcData"] = json!({"startingAngle": start, "endingAngle": start + sweep, "innerRadius": inner});
        }
        let geometry = ty == "VECTOR"
            || matches!(&n.kind, NodeKind::Ellipse { start: _, sweep, inner } if sweep.abs() < std::f64::consts::TAU - 1e-9 || *inner > 0.0);
        if geometry {
            let (path, evenodd) = if matches!(&n.kind, NodeKind::BooleanOp { .. }) {
                self.note(n, "boolean result becomes editable paths; boolean operands are not transferred");
                let (p, rule) = figcraft_render::node_outline(self.doc, n).ok_or_else(|| bad(CMD, "cannot export boolean geometry"))?;
                (p, rule == figcraft_render::FillRule::EvenOdd)
            } else {
                (
                    figcraft_doc::geometry::shape_path(n).ok_or_else(|| bad(CMD, "cannot export shape geometry"))?,
                    matches!(&n.kind, NodeKind::Vector { network } if network.regions.iter().any(|r| r.winding == WindingRule::EvenOdd)),
                )
            };
            if path.elements().len() > 100_000 || path.elements().iter().any(|e| !e.is_finite()) {
                return Err(bad(CMD, "invalid or oversized vector path"));
            }
            v["vectorPaths"] = json!([{"data": path.to_svg(), "windingRule": if evenodd { "EVENODD" } else { "NONZERO" }}]);
            if let NodeKind::Vector { network } = &n.kind
                && !n.strokes.is_empty()
                && network.stroke_path() != path
            {
                self.note(n, "vector stroke follows its exported fill outline");
            }
        }
        if let NodeKind::Text(t) = &n.kind {
            self.text(n, t, &mut v)?;
        }
        if !matches!(&n.kind, NodeKind::BooleanOp { .. }) {
            v["children"] = Value::Array(paint_children(self.doc, n).into_iter().map(|c| self.node(c)).collect::<Result<Vec<_>>>()?);
        }
        Ok(v)
    }
    fn style(&mut self, n: &Node, t: &TextStyle) -> Value {
        if !t.variations.is_empty() || t.decoration_look.is_some() {
            self.note(n, "variable-font axes and decoration styling are not transferred");
        }
        let mut v = json!({"fontFamily": t.font_family, "fontStyle": t.font_style, "fontSize": t.font_size,
            "letterSpacing": t.letter_spacing.resolve(t.font_size),
            "textCase": match t.case { TextCase::Original => "ORIGINAL", TextCase::Upper => "UPPER", TextCase::Lower => "LOWER", TextCase::Title => "TITLE", TextCase::SmallCaps => "SMALL_CAPS" },
            "textDecoration": match t.decoration { TextDecoration::None => "NONE", TextDecoration::Underline => "UNDERLINE", TextDecoration::Strikethrough => "STRIKETHROUGH" }});
        if t.line_height != LineHeight::Auto {
            v["lineHeightPx"] = json!(t.line_height.resolve(t.font_size, 1.21));
        }
        if let Some(fills) = &t.fills {
            v["fills"] = json!(self.paints(n, fills));
        }
        v
    }
    fn text(&mut self, n: &Node, t: &TextData, v: &mut Value) -> Result<()> {
        if t.characters.len() > MAX_BYTES / 4 {
            return Err(bad(CMD, "text exceeds clipboard budget"));
        }
        self.note(n, "text stays editable; font availability and line layout may differ in EffectCraft");
        if !n.strokes.is_empty() {
            self.note(n, "text strokes are not transferred");
        }
        if t.path.is_some()
            || t.list != ListStyle::None
            || !t.paragraphs.is_empty()
            || !t.features.is_empty()
            || t.auto_resize == TextAutoResize::Truncate
            || t.leading_trim != LeadingTrim::None
            || t.hanging_punctuation
            || t.hanging_list
            || t.paragraph_indent != 0.0
            || t.list_spacing != 0.0
        {
            self.note(n, "text paths, lists, truncation, trim and advanced paragraph/OpenType settings are not transferred");
        }
        if n.fills.len() > 1 || n.fills.iter().any(|p| !matches!(p.kind, PaintKind::Solid { .. })) {
            self.note(n, "text uses its top fill colour; gradients use their first stop");
        }
        let mut style = self.style(n, &t.style);
        style["textAlignHorizontal"] = json!(match t.align_h {
            TextAlignH::Left => "LEFT",
            TextAlignH::Center => "CENTER",
            TextAlignH::Right => "RIGHT",
            TextAlignH::Justified => "JUSTIFIED",
        });
        style["textAlignVertical"] = json!(match t.align_v {
            TextAlignV::Top => "TOP",
            TextAlignV::Middle => "CENTER",
            TextAlignV::Bottom => "BOTTOM",
        });
        style["paragraphSpacing"] = json!(t.paragraph_spacing);
        v["style"] = style;
        v["characters"] = json!(t.characters);
        v["textAutoResize"] = json!(match t.auto_resize {
            TextAutoResize::WidthAndHeight => "WIDTH_AND_HEIGHT",
            TextAutoResize::Height => "HEIGHT",
            _ => "NONE",
        });
        if !t.runs.is_empty() {
            let mut table = serde_json::Map::new();
            let mut style_budget = Budget(0);
            let mut overrides = Vec::new();
            let chars: Vec<_> = t.characters.chars().collect();
            for (index, (start, end, st)) in t.spans().iter().enumerate() {
                let sid = index + 1;
                let style = self.style(n, st);
                serde_json::to_writer(&mut style_budget, &style).map_err(|e| bad(CMD, e.to_string()))?;
                table.insert(sid.to_string(), style);
                for ch in chars.get(*start..*end).ok_or_else(|| bad(CMD, "invalid text style range"))? {
                    overrides.extend(std::iter::repeat_n(sid, ch.len_utf16()));
                }
            }
            v["styleOverrideTable"] = Value::Object(table);
            v["characterStyleOverrides"] = json!(overrides);
        }
        Ok(())
    }
}
fn paint_children(doc: &Document, n: &Node) -> Vec<NodeId> {
    doc.paint_runs(n).into_iter().flat_map(|run| n.children.get(run).unwrap_or(&[]).iter().copied()).collect()
}
fn point(p: (f64, f64)) -> Value {
    json!({"x": p.0, "y": p.1})
}
fn matrix(a: Affine) -> [[f64; 3]; 2] {
    let c = a.as_coeffs();
    [[c[0], c[2], c[4]], [c[1], c[3], c[5]]]
}
fn blend(b: BlendMode) -> &'static str {
    match b {
        BlendMode::PassThrough => "PASS_THROUGH",
        BlendMode::Normal => "NORMAL",
        BlendMode::Darken => "DARKEN",
        BlendMode::Multiply => "MULTIPLY",
        BlendMode::PlusDarker => "LINEAR_BURN",
        BlendMode::ColorBurn => "COLOR_BURN",
        BlendMode::Lighten => "LIGHTEN",
        BlendMode::Screen => "SCREEN",
        BlendMode::PlusLighter => "LINEAR_DODGE",
        BlendMode::ColorDodge => "COLOR_DODGE",
        BlendMode::Overlay => "OVERLAY",
        BlendMode::SoftLight => "SOFT_LIGHT",
        BlendMode::HardLight => "HARD_LIGHT",
        BlendMode::Difference => "DIFFERENCE",
        BlendMode::Exclusion => "EXCLUSION",
        BlendMode::Hue => "HUE",
        BlendMode::Saturation => "SATURATION",
        BlendMode::Color => "COLOR",
        BlendMode::Luminosity => "LUMINOSITY",
    }
}

#[cfg(test)]
mod tests;

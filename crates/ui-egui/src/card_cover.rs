//! Layers above a live card stay above its picture. EffectCraft and video cards paint their live
//! frame after the canvas image (no tile re-renders while they play), which would hide every layer
//! that sits over the card in z-order: board stamps, stickies, marker strokes. So each card keeps a
//! "cover": the layers painted after it that overlap it, rendered with a transparent background
//! over the part of the card they touch, and drawn right after the frame (before the card's own
//! hover chips and buttons).
//!
//! The cover only renders when the document or the zoom step changes, so playback costs one extra
//! textured quad per covered card. Cards with nothing over them cost nothing.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Color32, pos2};
use figcraft_doc::{Document, NodeId};
use figcraft_geom::Rect;
use figcraft_render::Pixmap;

/// Most pixels one cover holds (past it the cover renders at a lower scale).
const MAX_PIXELS: f64 = 4_000_000.0;
/// Siblings looked at, at most, per level (a page with a huge number of layers).
const MAX_SIBLINGS: usize = 100_000;

/// The layers painted over `card` that overlap it, bottom to top, and the page area they cover on
/// it. Each is the root of a subtree drawn after the card: the card's own children, then the later
/// siblings of the card and of each of its ancestors up to the page.
pub(crate) fn above(doc: &Document, card: NodeId) -> (Vec<NodeId>, Option<Rect>) {
    let Some(bounds) = doc.world_bounds(card) else { return (Vec::new(), None) };
    let mut roots: Vec<NodeId> = doc.node(card).map(|n| n.children.iter().take(MAX_SIBLINGS).copied().collect()).unwrap_or_default();
    let mut cur = card;
    for parent in doc.ancestors(card) {
        let Some(p) = doc.node(parent) else { break };
        if let Some(at) = p.children.iter().position(|c| *c == cur) {
            roots.extend(p.children.iter().skip(at.saturating_add(1)).take(MAX_SIBLINGS));
        }
        if p.is_page() {
            break;
        }
        cur = parent;
    }
    let mut out = Vec::new();
    let mut area: Option<Rect> = None;
    for id in roots {
        let Some(n) = doc.node(id) else { continue };
        if !n.visible || n.opacity <= 0.0 {
            continue;
        }
        let Some(b) = doc.world_render_bounds(id) else { continue };
        let hit = b.intersect(bounds);
        if !(hit.width() > 0.0 && hit.height() > 0.0) {
            continue;
        }
        out.push(id);
        area = Some(area.map_or(hit, |a| a.union(hit)));
    }
    (out, area)
}

/// The scale a cover renders at for a card shown at `want` device pixels per page unit: the next
/// power of two up (so zooming re-renders only per doubling), lowered so `area` stays within
/// `MAX_PIXELS`.
pub(crate) fn cover_scale(area: Rect, want: f64) -> f64 {
    let s = step(want);
    let px = area.width() * area.height() * s * s;
    if px.is_finite() && px > MAX_PIXELS { s * (MAX_PIXELS / px).sqrt() } else { s }
}

/// `want` rounded up to a power of two, within 1/64..=64.
fn step(want: f64) -> f64 {
    let want = if want.is_finite() && want > 0.0 { want } else { 1.0 };
    2f64.powf(want.log2().ceil()).clamp(1.0 / 64.0, 64.0)
}

/// The layers over `card` rendered over the part of it they cover (transparent elsewhere), the
/// page area the picture starts at and its scale. `None` when nothing covers the card.
pub(crate) fn render(doc: &Document, card: NodeId, want: f64) -> Option<(Pixmap, Rect, f64)> {
    let (ids, area) = above(doc, card);
    let area = area?;
    if ids.is_empty() {
        return None;
    }
    let scale = cover_scale(area, want);
    let pix = figcraft_render::render_area(doc, &ids, area, scale)?;
    // The picture's sides round up to whole pixels: the page area it spans.
    let spans = Rect::new(area.x0, area.y0, area.x0 + f64::from(pix.width()) / scale, area.y0 + f64::from(pix.height()) / scale);
    Some((pix, spans, scale))
}

struct Slot {
    doc: Arc<Document>,
    step: f64,
    /// The cover's texture and the page area it spans (None: nothing over the card).
    tex: Option<(egui::TextureHandle, Rect)>,
}

/// Each card's cover, kept until the document or zoom step changes.
#[derive(Default)]
pub(crate) struct Covers {
    slots: HashMap<NodeId, Slot>,
}

impl Covers {
    /// Forget the covers of cards that aren't among `cards` any more.
    pub(crate) fn keep_only(&mut self, cards: &[NodeId]) {
        self.slots.retain(|id, _| cards.contains(id));
    }

    /// Draw `card`'s cover (rendering it first if the document or zoom step changed). `want` is
    /// device pixels per page unit; `to_screen` maps page points to the screen.
    pub(crate) fn draw(&mut self, painter: &egui::Painter, doc: &Arc<Document>, card: NodeId, want: f64, to_screen: impl Fn(f64, f64) -> egui::Pos2) {
        let s = step(want);
        let fresh = self.slots.get(&card).is_some_and(|slot| Arc::ptr_eq(&slot.doc, doc) && slot.step.to_bits() == s.to_bits());
        if !fresh {
            let tex = render(doc, card, want).map(|(pix, spans, _)| {
                let image = crate::render_worker::screen_image(&pix, doc.color_profile);
                (painter.ctx().load_texture("card-cover", image, egui::TextureOptions::LINEAR), spans)
            });
            self.slots.insert(card, Slot { doc: Arc::clone(doc), step: s, tex });
        }
        let Some((tex, spans)) = self.slots.get(&card).and_then(|slot| slot.tex.as_ref()) else { return };
        let r = egui::Rect::from_min_max(to_screen(spans.x0, spans.y0), to_screen(spans.x1, spans.y1));
        if r.is_finite() && r.intersects(painter.clip_rect()) {
            painter.image(tex.id(), r, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    }
}

//! Fork-only (`effectcraft-inproc`): grab a layer of the comp right on its EffectCraft card, as
//! in EffectCraft's Composition viewer, while the Timeline edits that card.
//!
//! - **When.** The Timeline is open for the card and may edit (not read-only), the card is
//!   not playing, and the Move tool is on. Otherwise the card behaves as ever.
//! - **What.** The selected layers show their box and eight handles. A press on a layer selects
//!   it (⇧ adds or removes it; the Timeline follows) and dragging moves the selection
//!   (⇧: along one axis). Dragging a handle scales (corners both axes, edges one; ⇧ on a corner
//!   keeps the proportions). A layer covering the whole comp (a background) is grabbed by its
//!   handles only, so a press on it still moves the card. A press on no layer is FigCraft's.
//! - **How.** The maths is EffectCraft's own (`effectcraft_engine::viewer_handles`: the layer's
//!   content box through its transform, the viewer's pick and drag rules); edits are its
//!   `prop.set` (keyed at the current time when animated), one undo step per drag. A press on
//!   the card gives the panel the keyboard, so ⌘Z undoes it in EffectCraft, and the card
//!   re-renders from the session like any panel edit.

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use egui::{CornerRadius, CursorIcon, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};
use figcraft_doc::{EcComp, NodeId};
use figcraft_ec_inproc::effectcraft_engine::Session;
use figcraft_ec_inproc::effectcraft_engine::project::LayerId;
use figcraft_ec_inproc::effectcraft_engine::render::EvalCtx;
use figcraft_ec_inproc::effectcraft_engine::viewer_handles as vh;
use serde_json::json;

use crate::FigApp;
use crate::effectcraft_panel::EcHost;

/// Screen points from a handle's centre that still grab it.
const GRAB: f32 = 6.0;
/// A handle's side (screen points).
const SIZE: f32 = 7.0;
/// Screen points the pointer moves before a press becomes a drag (a click only selects).
const SLOP: f32 = 2.0;
/// Cards smaller than this on screen (points) show and take no handles.
const MIN_CARD: f32 = 60.0;

/// The drag under way, if any, and how many there were (each is its own undo step).
#[derive(Default)]
pub(crate) struct State {
    drag: Option<Drag>,
    count: u64,
}

impl State {
    pub(crate) fn dragging(&self) -> bool {
        self.drag.is_some()
    }
}

struct Drag {
    card: NodeId,
    kind: Kind,
    press: Pos2,
    moved: bool,
    merge: String,
    map: CardMap,
}

enum Kind {
    Move(vh::MoveDrag),
    Scale(vh::ScaleDrag),
}

/// Comp pixels ↔ screen points on a card showing a `size` comp in `pic`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CardMap {
    pub pic: Rect,
    pub size: [f64; 2],
}

impl CardMap {
    pub(crate) fn new(pic: Rect, width: u32, height: u32) -> Option<CardMap> {
        (pic.is_finite() && pic.width() > 0.0 && pic.height() > 0.0 && width > 0 && height > 0)
            .then(|| CardMap { pic, size: [f64::from(width), f64::from(height)] })
    }

    pub(crate) fn to_screen(&self, p: [f64; 2]) -> Pos2 {
        let x = self.pic.min.x + (p[0] / self.size[0]) as f32 * self.pic.width();
        let y = self.pic.min.y + (p[1] / self.size[1]) as f32 * self.pic.height();
        pos2(x, y)
    }

    pub(crate) fn to_comp(&self, p: Pos2) -> [f64; 2] {
        let u = f64::from((p.x - self.pic.min.x) / self.pic.width());
        let v = f64::from((p.y - self.pic.min.y) / self.pic.height());
        [u * self.size[0], v * self.size[1]]
    }
}

/// What a press at a screen point grabs on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hit {
    /// A handle (numbered as in `vh::HANDLES`) of a selected layer's box.
    Handle(LayerId, usize),
    /// The layer's body.
    Layer(LayerId),
}

/// `f` with the session's active comp at its current time, as the viewer evaluates it.
fn with_ctx<R>(s: &Session, f: impl FnOnce(&EvalCtx) -> R) -> Option<R> {
    let cid = s.active_comp_id()?;
    let comp = s.active_comp()?;
    let ctx = EvalCtx { footage: Some(s.footage.as_ref()), expr: s.expr.as_deref(), ..EvalCtx::new(&s.project, cid, comp, s.time()) };
    Some(f(&ctx))
}

/// Whether the session's active comp is the one card `c` shows (the Timeline may have opened
/// a precomp: then the card shows something else and takes no handles).
pub(crate) fn shows_active_comp(s: &Session, c: &EcComp) -> bool {
    let Some(id) = s.active_comp_id() else { return false };
    if c.comp_id > 0 {
        return id.0 == c.comp_id;
    }
    s.project.item(id).is_some_and(|i| i.name == c.comp)
}

/// Whether box `b` covers the whole `size` comp (a background: grabbed by its handles only).
pub(crate) fn covers_comp(b: &vh::LayerBox, size: [f64; 2]) -> bool {
    [[0.0, 0.0], [size[0], 0.0], [size[0], size[1]], [0.0, size[1]]].iter().all(|p| vh::point_in_quad(*p, &b.quad))
}

/// The boxes of the selected layers that have one.
fn selected_boxes(ctx: &EvalCtx, selected: &[LayerId]) -> Vec<vh::LayerBox> {
    selected.iter().filter_map(|id| ctx.comp.layer(*id)).filter(|l| !l.switches.locked).filter_map(|l| vh::layer_box(ctx, l)).collect()
}

/// The handle within [`GRAB`] of screen point `pos` among `boxes`, the nearest first.
pub(crate) fn handle_at(boxes: &[vh::LayerBox], map: &CardMap, pos: Pos2) -> Option<(LayerId, usize)> {
    boxes
        .iter()
        .flat_map(|b| b.handles().into_iter().enumerate().map(move |(i, h)| (b.layer, i, map.to_screen(h).distance(pos))))
        .filter(|(_, _, d)| d.is_finite() && *d <= GRAB)
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(l, i, _)| (l, i))
}

/// What a press at screen point `pos` grabs: a selected layer's handle, else (as the viewer
/// picks) a selected layer under it, else the topmost layer, backgrounds left out.
pub(crate) fn hit(s: &Session, map: &CardMap, pos: Pos2) -> Option<Hit> {
    let selected = s.state.selected_layers.clone();
    with_ctx(s, |ctx| {
        if let Some((l, i)) = handle_at(&selected_boxes(ctx, &selected), map, pos) {
            return Some(Hit::Handle(l, i));
        }
        let p = map.to_comp(pos);
        let hits: Vec<LayerId> = vh::layers_at(ctx, p)
            .into_iter()
            .filter(|id| ctx.comp.layer(*id).and_then(|l| vh::layer_box(ctx, l)).is_some_and(|b| !covers_comp(&b, map.size)))
            .collect();
        hits.iter().find(|id| selected.contains(id)).or(hits.first()).map(|id| Hit::Layer(*id))
    })
    .flatten()
}

/// The resize cursor over handle `handle` (corners diagonal, edges across them).
pub(crate) fn handle_cursor(handle: usize) -> CursorIcon {
    match handle {
        0 | 2 => CursorIcon::ResizeNwSe,
        1 | 3 => CursorIcon::ResizeNeSw,
        4 | 6 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeHorizontal,
    }
}

/// The Timeline's host, when it edits card `card` and handles apply (see the module doc).
fn editing<'a>(app: &'a FigApp, card: NodeId, c: &EcComp) -> Option<&'a EcHost> {
    let p = app.effectcraft.panel.as_ref().filter(|p| p.open && p.card == card)?;
    let ec = &app.session.effectcraft;
    let playing = ec.playing() && ec.node == Some(card);
    (p.host.read_only().is_none() && !playing && shows_active_comp(p.host.session(), c)).then_some(&p.host)
}

/// The Move tool is on (other tools draw on the card as on any layer).
fn move_tool(app: &FigApp) -> bool {
    matches!(app.session.tool.tool, figcraft_engine::tools::ToolId::Move)
}

/// Draw the selected layers' boxes and handles on card `card` (showing in `pic`), and a thin
/// box round the layer under the pointer. Runs in the card overlay, after the frame.
pub(crate) fn draw(app: &FigApp, painter: &egui::Painter, card: NodeId, c: &EcComp, pic: Rect, hover: Option<Pos2>) {
    if pic.width() < MIN_CARD || pic.height() < MIN_CARD / 2.0 || !move_tool(app) {
        return;
    }
    let Some(host) = editing(app, card, c) else { return };
    let s = host.session();
    let Some(comp) = s.active_comp() else { return };
    let Some(map) = CardMap::new(pic, comp.width, comp.height) else { return };
    let accent = crate::theme::canvas_accent(painter.ctx());
    let t = crate::theme::Tokens::get(painter.ctx());
    let selected = s.state.selected_layers.clone();
    let hovered = hover.filter(|h| pic.contains(*h) && !app.effectcraft.handles.dragging()).and_then(|h| hit(s, &map, h));
    let painter = painter.with_clip_rect(pic.expand(SIZE).intersect(painter.clip_rect()));
    let _ = with_ctx(s, |ctx| {
        if let Some(Hit::Layer(id)) = hovered
            && !selected.contains(&id)
            && let Some(b) = ctx.comp.layer(id).and_then(|l| vh::layer_box(ctx, l))
        {
            let pts: Vec<Pos2> = b.quad.iter().map(|p| map.to_screen(*p)).collect();
            painter.add(egui::Shape::closed_line(pts, Stroke::new(1.0, accent.gamma_multiply(0.6))));
        }
        for b in selected_boxes(ctx, &selected) {
            let pts: Vec<Pos2> = b.quad.iter().map(|p| map.to_screen(*p)).collect();
            painter.add(egui::Shape::closed_line(pts, Stroke::new(1.0, accent)));
            for h in b.handles() {
                let r = Rect::from_center_size(map.to_screen(h), vec2(SIZE, SIZE));
                painter.rect_filled(r, CornerRadius::ZERO, t.canvas);
                painter.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0, accent), StrokeKind::Inside);
            }
        }
    });
}

/// The pointer on card `target` (the card under it and its picture, when presses there may
/// grab a layer: not on its buttons or slider): hover cursors, a press that grabs a layer or
/// handle, and the drag that follows (wherever the pointer goes). Returns whether the pointer
/// was used (the canvas then leaves it alone and shows this cursor).
pub(crate) fn input(app: &mut FigApp, ctx: &egui::Context, target: Option<(NodeId, Rect)>, pos: Pos2, hovered: bool) -> bool {
    let (down, pressed, shift) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.button_pressed(egui::PointerButton::Primary), i.modifiers.shift));
    if app.effectcraft.handles.drag.is_some() {
        if down {
            drag_to(app, ctx, pos, shift);
        } else {
            app.effectcraft.handles.drag = None;
        }
        return true;
    }
    if !hovered || !move_tool(app) {
        return false;
    }
    let Some((card, pic)) = target.filter(|(_, pic)| pic.width() >= MIN_CARD && pic.height() >= MIN_CARD / 2.0 && pic.contains(pos)) else {
        return false;
    };
    let Ok(doc) = app.session.doc() else { return false };
    let Some(c) = doc.node(card).and_then(|n| n.effectcraft.as_deref()).cloned() else { return false };
    let Some(host) = editing(app, card, &c) else { return false };
    let s = host.session();
    let Some(map) = s.active_comp().and_then(|comp| CardMap::new(pic, comp.width, comp.height)) else { return false };
    let Some(h) = hit(s, &map, pos) else { return false };
    if !pressed {
        ctx.set_cursor_icon(match h {
            Hit::Handle(_, i) => handle_cursor(i),
            Hit::Layer(_) => CursorIcon::Default,
        });
        return true;
    }
    press(app, ctx, card, map, h, pos, shift);
    true
}

/// A press grabbed `h` on card `card`: select, give the panel the keyboard, begin the drag.
fn press(app: &mut FigApp, ctx: &egui::Context, card: NodeId, map: CardMap, h: Hit, pos: Pos2, shift: bool) {
    let link = &mut app.effectcraft;
    let Some(p) = link.panel.as_mut() else { return };
    // ⌘Z now undoes in EffectCraft, keys go to its shortcuts, until a click elsewhere.
    p.focused = true;
    p.host.set_focus(effectcraft_ui_egui::dock::PanelKind::Timeline);
    let cpt = map.to_comp(pos);
    let kind = match h {
        Hit::Handle(layer, handle) => {
            let s = p.host.session();
            let others = s.state.selected_layers.clone();
            with_ctx(s, |c| vh::ScaleDrag::begin(c, layer, handle, cpt, &others)).flatten().map(Kind::Scale)
        }
        Hit::Layer(layer) => {
            let selected = p.host.session().state.selected_layers.contains(&layer);
            let params = if shift {
                json!({"layers": [layer.0], "toggle": true})
            } else if selected {
                json!(null)
            } else {
                json!({"layers": [layer.0]})
            };
            if !params.is_null()
                && let Err(e) = p.host.run_engine(ctx, "layer.select", params)
            {
                log::debug!("EffectCraft card: {e}");
            }
            let s = p.host.session();
            let now = s.state.selected_layers.clone();
            // ⇧-click that took the layer out of the selection: nothing to drag.
            if now.contains(&layer) { with_ctx(s, |c| vh::MoveDrag::begin(c, &now, cpt)).flatten().map(Kind::Move) } else { None }
        }
    };
    if let Some(kind) = kind {
        link.handles.count += 1;
        let merge = format!("figcraft-card-drag-{}", link.handles.count);
        link.handles.drag = Some(Drag { card, kind, press: pos, moved: false, merge, map });
    }
    ctx.request_repaint();
}

/// The drag with the pointer at `pos`: the values it writes (none until it moved past
/// [`SLOP`], so a click only selects).
fn drag_to(app: &mut FigApp, ctx: &egui::Context, pos: Pos2, shift: bool) {
    let link = &mut app.effectcraft;
    let Some(d) = link.handles.drag.as_mut() else { return };
    ctx.set_cursor_icon(match &d.kind {
        Kind::Move(_) => CursorIcon::Grabbing,
        Kind::Scale(s) => handle_cursor(match s.axes {
            [true, false] => 5,
            [false, true] => 4,
            _ => 0,
        }),
    });
    d.moved |= d.press.distance(pos) > SLOP;
    let Some(p) = link.panel.as_mut().filter(|p| p.open && p.card == d.card && p.host.read_only().is_none()) else {
        link.handles.drag = None;
        return;
    };
    if !d.moved {
        return;
    }
    let cpt = d.map.to_comp(pos);
    let (path, values) = match &d.kind {
        Kind::Move(m) => ("transform/position", m.values(cpt, shift)),
        Kind::Scale(s) => ("transform/scale", s.values(cpt, shift)),
    };
    for (layer, v) in values {
        if v.iter().any(|x| !x.is_finite()) {
            continue;
        }
        let params = json!({"layer": layer.0, "path": path, "value": v, "merge": d.merge});
        if let Err(e) = p.host.run_engine(ctx, "prop.set", params) {
            log::debug!("EffectCraft card: {e}");
        }
    }
    ctx.request_repaint();
}

#[cfg(test)]
#[path = "effectcraft_handles_tests.rs"]
mod tests;

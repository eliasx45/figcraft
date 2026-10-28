//! Fork-only (`effectcraft-inproc`): agents on the MAIN board, pen.dev style (Elias, 28 Oct 2026:
//! "if an agent is working on its comp, its comp is glowing like this and has this system that Pen
//! Dev has. include that too"). The state is the engine's `canvas.presence.*`
//! ([`figcraft_engine::ecboard::presence`]); this draws it and reports the pointer on it:
//!
//! - the agent's card gets a wash of its colour (stronger at the bottom) and a solid outline: it
//!   pulses softly ([`PULSE`]) while the agent works and holds steady when done (the "changed"
//!   glow); idle is a faint outline. Agents on the same card stack their outlines outward;
//! - an arrow cursor with a name tag ("A2 · S1 06") sits at the centre of the touched layer's box
//!   (comp pixels mapped onto the card; no box: the card's top-left corner) and glides to each new
//!   place ([`GLIDE`], ease-out);
//! - a thin box in its colour round the touched box fades out over [`TOUCH`].
//!
//! Nothing moves for show: every position is what the host passed. With Reduce Motion (egui's
//! animation time 0, or FigCraft's preference) there is no pulse and no glide.
//!
//! Hovering a cursor or a card's glow (the band round its outline) is a `presence.hover {agent}`
//! event in `board.events` (`{agent: null}` when the pointer leaves it); a click is
//! `presence.open {agent}`.

use std::collections::HashMap;

use egui::{Color32, CornerRadius, Mesh, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};
use figcraft_doc::{Document, NodeId};
use figcraft_engine::ecboard::presence::{Agent, Status};
use serde_json::json;

use crate::FigApp;

/// One working pulse (seconds).
pub(crate) const PULSE: f64 = 1.6;
/// The cursor's glide to a new place (seconds).
pub(crate) const GLIDE: f64 = 0.25;
/// How long the touch box takes to fade (seconds).
pub(crate) const TOUCH: f64 = 1.2;
/// The glow band round a card that takes the pointer (points beyond the outline).
const BAND: f32 = 8.0;
/// Outlines of agents on the same card step outward by this (points).
const STACK: f32 = 3.0;

/// A place on a card: its picture's fraction across and down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Spot {
    pub card: NodeId,
    pub u: f32,
    pub v: f32,
}

/// What the UI remembers of an agent between frames.
#[derive(Clone, Debug, PartialEq)]
struct Seen {
    serial: u64,
    to: Spot,
    /// Gliding from here since `since`.
    from: Option<Spot>,
    since: f64,
    /// When its touch box was last shown.
    touched: Option<f64>,
}

#[derive(Default)]
pub(crate) struct View {
    seen: HashMap<String, Seen>,
    /// The agent under the pointer (reported once per change).
    hovered: Option<String>,
}

/// Where agent `a`'s cursor sits on its card (`w`×`h` comp pixels): the centre of its box, else the
/// card's corner.
pub(crate) fn spot_of(a: &Agent, w: u32, h: u32) -> Spot {
    let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
    let (u, v) = match a.bbox {
        Some([x, y, bw, bh]) => (((x + bw / 2.0) / w).clamp(0.0, 1.0), ((y + bh / 2.0) / h).clamp(0.0, 1.0)),
        None => (0.0, 0.0),
    };
    Spot { card: a.card, u: u as f32, v: v as f32 }
}

/// Ease-out (cubic) of `t` in 0..1.
pub(crate) fn ease_out(t: f64) -> f64 {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 1.0 };
    1.0 - (1.0 - t).powi(3)
}

/// The glow's strength `now` (0..1): a soft pulse while working (steady at 0.6 without motion),
/// steady when done, faint when idle.
pub(crate) fn glow(status: Status, now: f64, motion: bool) -> f32 {
    match status {
        Status::Working if motion && now.is_finite() => (0.5 - 0.5 * (std::f64::consts::TAU * now / PULSE).cos()) as f32,
        Status::Working => 0.6,
        Status::Done => 1.0,
        Status::Idle => 0.0,
    }
}

/// The touch box's opacity `dt` seconds after it showed (`None` once gone).
pub(crate) fn touch_alpha(dt: f64) -> Option<f32> {
    (dt.is_finite() && (0.0..TOUCH).contains(&dt)).then(|| (1.0 - dt / TOUCH).powi(2) as f32)
}

/// Whether reduce motion is on.
fn motion(app: &FigApp, ctx: &egui::Context) -> bool {
    ctx.global_style().animation_time > 0.0 && !app.session.prefs.reduce_motion
}

fn rgb(a: &Agent) -> Color32 {
    Color32::from_rgb(a.color[0], a.color[1], a.color[2])
}

/// Dark text on light colours, white on dark ones.
pub(crate) fn ink_on(c: Color32) -> Color32 {
    let l = 0.2126 * f32::from(c.r()) + 0.7152 * f32::from(c.g()) + 0.0722 * f32::from(c.b());
    if l > 150.0 { Color32::from_rgb(0x14, 0x14, 0x16) } else { Color32::WHITE }
}

/// Every frame: follow the agents set (a new serial glides the cursor and shows the touch box),
/// report hovering, keep the pulse going.
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let motion = motion(app, ctx);
    let Ok(ds) = app.session.ds() else { return };
    let doc = &ds.doc;
    let agents = &app.session.ecboard.presence.list;
    let view = &mut app.effectcraft.board.presence;
    view.seen.retain(|k, _| agents.iter().any(|a| a.agent == *k));
    let mut busy = false;
    for a in agents {
        let Some(c) = doc.node(a.card).and_then(|n| n.effectcraft.as_deref()) else { continue };
        let to = spot_of(a, c.width, c.height);
        let touched = a.bbox.map(|_| now);
        match view.seen.get_mut(&a.agent) {
            Some(s) if s.serial == a.serial => {}
            Some(s) => {
                s.from = (motion && s.to != to).then_some(s.to);
                s.to = to;
                s.since = now;
                s.serial = a.serial;
                s.touched = touched;
            }
            None => {
                view.seen.insert(a.agent.clone(), Seen { serial: a.serial, to, from: None, since: now, touched });
            }
        }
        busy |= a.status == Status::Working && motion;
    }
    let mut moving = false;
    for s in view.seen.values_mut() {
        if s.from.is_some() && now - s.since >= GLIDE {
            s.from = None;
        }
        if s.touched.is_some_and(|t| touch_alpha(now - t).is_none()) {
            s.touched = None;
        }
        moving |= s.from.is_some() || s.touched.is_some();
    }
    if moving {
        ctx.request_repaint();
    } else if busy {
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
    // Hover: only over the canvas; held while a button is down (a click or a drag going on).
    let (down, at) = ctx.input(|i| (i.pointer.any_down(), i.pointer.hover_pos()));
    if down {
        return;
    }
    let hit = at.filter(|p| app.canvas.rect.contains(*p)).and_then(|p| hit(app, p)).map(|(agent, _)| agent);
    if hit != app.effectcraft.board.presence.hovered {
        app.effectcraft.board.presence.hovered = hit.clone();
        app.session.ecboard.push("presence.hover", json!({"agent": hit}));
    }
}

/// An agent drawn on the active page: its card's picture, its cursor, the stack step.
struct Shown<'a> {
    agent: &'a Agent,
    pic: Rect,
    cursor: Pos2,
    step: f32,
}

/// The agents on the active page, in the order set (later ones on top).
fn shown<'a>(app: &'a FigApp, doc: &Document, page: NodeId, now: f64, motion: bool) -> Vec<Shown<'a>> {
    let view = &app.effectcraft.board.presence;
    let mut per_card: HashMap<NodeId, f32> = HashMap::new();
    let mut out = Vec::new();
    for a in &app.session.ecboard.presence.list {
        if doc.page_of(a.card) != Some(page) {
            continue;
        }
        let Some((pic, c)) = super::board::pic_of(app, doc, a.card) else { continue };
        let at = |s: Spot| -> Option<Pos2> {
            let r = if s.card == a.card { pic } else { super::board::pic_of(app, doc, s.card)?.0 };
            Some(pos2(r.min.x + s.u * r.width(), r.min.y + s.v * r.height()))
        };
        let seen = view.seen.get(&a.agent);
        let to = seen.map_or_else(|| spot_of(a, c.width, c.height), |s| s.to);
        let Some(target) = at(to) else { continue };
        let cursor = match seen.and_then(|s| s.from.map(|f| (f, s.since))) {
            Some((from, since)) if motion => match at(from) {
                Some(f) => f.lerp(target, ease_out((now - since) / GLIDE) as f32),
                None => target,
            },
            _ => target,
        };
        let n = per_card.entry(a.card).or_insert(0.0);
        let step = *n;
        *n += 1.0;
        out.push(Shown { agent: a, pic, cursor, step });
    }
    out
}

/// The arrow (tip at `p`) and the name tag's box.
fn cursor_rects(painter_ctx: &egui::Context, a: &Agent, p: Pos2) -> (Rect, Rect) {
    let arrow = Rect::from_min_size(p, vec2(13.0, 13.0));
    let w = painter_ctx.fonts_mut(|f| f.layout_no_wrap(a.tag(), tag_font(), Color32::WHITE).size().x);
    let tag = Rect::from_min_size(p + vec2(11.0, 13.0), vec2(w + 10.0, 16.0));
    (arrow, tag)
}

fn tag_font() -> egui::FontId {
    egui::FontId::proportional(10.5)
}

/// The agent under screen point `p` (top-most first: cursors, then glow bands) and whether it was
/// the cursor.
fn hit(app: &FigApp, p: Pos2) -> Option<(String, bool)> {
    let ds = app.session.ds().ok()?;
    let ctx = app.ui_context.as_ref()?;
    let now = ctx.input(|i| i.time);
    let list = shown(app, &ds.doc, ds.page, now, motion(app, ctx));
    for s in list.iter().rev() {
        let (arrow, tag) = cursor_rects(ctx, s.agent, s.cursor);
        if arrow.expand(2.0).contains(p) || tag.contains(p) {
            return Some((s.agent.agent.clone(), true));
        }
    }
    for s in list.iter().rev() {
        let edge = s.pic.expand(1.5 + s.step * STACK);
        if edge.expand(BAND).contains(p) && !s.pic.shrink(2.0).contains(p) && s.agent.status != Status::Idle {
            return Some((s.agent.agent.clone(), false));
        }
    }
    None
}

/// A press on an agent's cursor or glow: `presence.open {agent}`. Returns whether it took the
/// press (a selected card's own edge stays the card's).
pub(crate) fn input(app: &mut FigApp, ctx: &egui::Context, pos: Pos2, hovered: bool) -> bool {
    if !hovered || !ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary)) {
        return false;
    }
    let Some((agent, on_cursor)) = hit(app, pos) else { return false };
    let card = app.session.ecboard.presence.of(&agent).map(|a| a.card);
    if !on_cursor && card.is_some_and(|c| app.session.selection().contains(&c)) {
        return false;
    }
    app.session.ecboard.push("presence.open", json!({"agent": agent, "card": card}));
    true
}

/// A wash of `c` over `r`, `top`→`bottom` opacity.
fn wash(painter: &egui::Painter, r: Rect, c: Color32, top: f32, bottom: f32) {
    let mut m = Mesh::default();
    let (a, b) = (c.gamma_multiply(top), c.gamma_multiply(bottom));
    m.colored_vertex(r.left_top(), a);
    m.colored_vertex(r.right_top(), a);
    m.colored_vertex(r.right_bottom(), b);
    m.colored_vertex(r.left_bottom(), b);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(m));
}

/// The arrow cursor, tip at `p`.
fn arrow(painter: &egui::Painter, p: Pos2, c: Color32) {
    let pts = [p, p + vec2(12.5, 4.8), p + vec2(6.2, 6.2), p + vec2(4.8, 12.5)];
    let mut m = Mesh::default();
    for q in pts {
        m.colored_vertex(q, c);
    }
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(m));
    painter.add(Shape::closed_line(pts.to_vec(), Stroke::new(1.0, Color32::WHITE.gamma_multiply(0.9))));
}

/// Draw every agent on the active page: glows, touch boxes, then cursors on top. Runs after the
/// cards' frames and the board's marks.
pub(crate) fn overlay(app: &FigApp, painter: &egui::Painter) {
    let Ok(ds) = app.session.ds() else { return };
    let ctx = painter.ctx().clone();
    let now = ctx.input(|i| i.time);
    let motion = motion(app, &ctx);
    let list = shown(app, &ds.doc, ds.page, now, motion);
    let hovered = app.effectcraft.board.presence.hovered.as_deref();
    for s in &list {
        let c = rgb(s.agent);
        let k = glow(s.agent.status, now, motion);
        let lift = if hovered == Some(s.agent.agent.as_str()) { 0.15 } else { 0.0 };
        let edge = s.pic.expand(1.5 + s.step * STACK);
        match s.agent.status {
            Status::Idle => {
                painter.rect_stroke(edge, CornerRadius::same(4), Stroke::new(1.0, c.gamma_multiply(0.35 + lift)), StrokeKind::Outside);
            }
            Status::Working | Status::Done => {
                if s.step == 0.0 {
                    wash(painter, s.pic, c, 0.03 + 0.05 * k, 0.12 + 0.12 * k);
                }
                for (i, a) in [(1.0_f32, 0.22_f32), (2.0, 0.12), (3.0, 0.06)] {
                    painter.rect_stroke(
                        edge.expand(2.5 * i),
                        CornerRadius::same(5),
                        Stroke::new(2.5, c.gamma_multiply((a * (0.4 + 0.6 * k) + lift * 0.5).min(1.0))),
                        StrokeKind::Outside,
                    );
                }
                painter.rect_stroke(
                    edge,
                    CornerRadius::same(4),
                    Stroke::new(2.0, c.gamma_multiply((0.7 + 0.3 * k + lift).min(1.0))),
                    StrokeKind::Outside,
                );
            }
        }
        // The touch box, in the comp's pixels on the card.
        let seen = app.effectcraft.board.presence.seen.get(&s.agent.agent);
        if let (Some([x, y, w, h]), Some(alpha)) = (s.agent.bbox, seen.and_then(|v| v.touched).and_then(|t| touch_alpha(now - t)))
            && let Some(comp) = ds.doc.node(s.agent.card).and_then(|n| n.effectcraft.as_deref())
        {
            let (cw, ch) = (f64::from(comp.width.max(1)), f64::from(comp.height.max(1)));
            let at = |px: f64, py: f64| pos2(s.pic.min.x + (px / cw) as f32 * s.pic.width(), s.pic.min.y + (py / ch) as f32 * s.pic.height());
            let r = Rect::from_min_max(at(x, y), at(x + w, y + h));
            let p = painter.with_clip_rect(s.pic.expand(2.0).intersect(painter.clip_rect()));
            p.rect_stroke(r, CornerRadius::same(2), Stroke::new(1.5, c.gamma_multiply(alpha)), StrokeKind::Middle);
        }
    }
    for s in &list {
        let c = rgb(s.agent);
        arrow(painter, s.cursor, c);
        let (_, tag) = cursor_rects(&ctx, s.agent, s.cursor);
        painter.rect_filled(tag, CornerRadius::same(3), c);
        painter.text(pos2(tag.min.x + 5.0, tag.center().y), egui::Align2::LEFT_CENTER, s.agent.tag(), tag_font(), ink_on(c));
    }
}

#[cfg(test)]
#[path = "effectcraft_presence_tests.rs"]
mod tests;

//! Fork-only (`effectcraft-inproc`): the MAIN board in the app — Ghostex's EDITING canvas (Elias,
//! 28 Oct 2026: "perfect i love it as a start. go ahead"; the mock is EffectCraft's
//! `plan/figcraft-canvas-playground.html`). The board itself is engine commands
//! (`figcraft_engine::ecboard`); this is what needs the window:
//!
//! - **The host.** The engine's [`EcHost`] is installed on the first frame: projects opened in
//!   process from their saved `.ecproj` (`figcraft_ec_inproc`, read-only).
//! - **Fly.** `canvas.flyTo` glides there with the cards' 0.35 s glide (`cards::fly`).
//! - **Selection.** Every frame tells the engine the selection (`ecboard::observe`): a `select`
//!   event for the host when the selected card changes.
//! - **Review marks** (`canvas.annotate.*`), as EffectCraft's PN1/PN2 viewer marks: with a tool
//!   armed (or P / C on a page with a MAIN board; Esc closes the box, then disarms), Pen drags a
//!   smooth stroke on a card (EffectCraft's ink filter, every pointer sample), Comment clicks a
//!   point; the box pops from the mark (EffectCraft's own `mark_box`: Enter adds, Shift+Enter a new
//!   line, Esc cancels) and Enter runs `canvas.annotate.add`. Layer (or ⌃⌥-click any time) picks
//!   the layer under the pointer at once. Marks waiting for the host are drawn faintly on their
//!   card.
//! - **Flashes.** `card.flash` makes the cards flash (three 0.45 s pulses) and puts a "Before /
//!   After" badge on them: held, the card shows its frame from the revision before the edit (the
//!   store keeps those frames when newer ones replace them).
//! - **Agents** (`canvas.presence.*`): their cards glow and their cursors sit at what they touched
//!   ([`super::presence`]).

use std::collections::HashMap;
use std::path::Path;

use effectcraft_ui_egui::automation::Registry;
use effectcraft_ui_egui::panels::viewer_annotate as va;
use effectcraft_ui_egui::theme::{ThemeKind, Tokens};
use egui::{Color32, CornerRadius, Key, Modifiers, Pos2, Rect, Stroke, pos2, vec2};
use figcraft_doc::{Document, EcComp, NodeId};
use figcraft_ec_inproc::effectcraft_engine::commands::annotate::Tool as EcTool;
use figcraft_ec_inproc::effectcraft_engine::commands::annotate::ink::{self, InkFilter};
use figcraft_engine::ecboard::{self, EcHost, marks::Tool};
use serde_json::{Value, json};

use crate::FigApp;

/// One flash pulse (seconds) and how many.
const FLASH: f64 = 0.45;
const PULSES: f64 = 3.0;
/// How long a layer pick's box shows (seconds).
const PICK_SHOWN: f64 = 0.9;
/// The most samples one live stroke keeps.
const MAX_LIVE: usize = 20_000;
const BLUE: Color32 = Color32::from_rgb(0x2D, 0x8C, 0xFF);
const YELLOW: Color32 = Color32::from_rgb(0xFF, 0xCF, 0x3A);

/// The board's EffectCraft: each project opened in process from its saved file (read-only).
#[derive(Default)]
pub(crate) struct Host {
    pool: figcraft_ec_inproc::Pool,
}

impl EcHost for Host {
    fn call(&mut self, project: &str, method: &str, params: &Value) -> Result<Value, String> {
        if project.trim().is_empty() {
            return Err("no EffectCraft project".into());
        }
        let path = Path::new(project);
        if !path.is_file() {
            return Err(format!("{project} isn't there (save it in EffectCraft first)"));
        }
        self.pool.get(path)?.call(method, params)
    }
}

/// A Pen stroke while the button is down: filtered samples (comp pixels), the newest raw one.
struct Live {
    card: NodeId,
    time: f64,
    stroke: Vec<[f64; 2]>,
    filter: InkFilter,
    tip: Option<[f64; 2]>,
    /// Comp pixels per screen point when it started.
    px: f64,
}

/// A mark's box waiting for its note.
struct Pending {
    card: NodeId,
    draft: va::Draft,
}

#[derive(Default)]
pub(crate) struct State {
    installed: bool,
    live: Option<Live>,
    draft: Option<Pending>,
    /// Cards flashing since (UI time), and flashes asked for since last frame.
    flashing: HashMap<NodeId, f64>,
    new_flashes: Vec<NodeId>,
    /// The card whose Before / After badge is held.
    held: Option<NodeId>,
    /// The last layer pick: card, screen point, when.
    pick: Option<(NodeId, Pos2, f64)>,
    auto: Registry,
    /// Agents shown on the board.
    pub(crate) presence: super::presence::View,
}

/// Card `id`'s picture on screen and its comp.
pub(crate) fn pic_of(app: &FigApp, doc: &Document, id: NodeId) -> Option<(Rect, EcComp)> {
    let c = doc.node(id)?.effectcraft.as_deref()?.clone();
    let r = super::screen_rect(app, doc, id)?;
    Some((super::fit(r, c.width, c.height), c))
}

fn to_comp(pic: Rect, c: &EcComp, p: Pos2) -> [f64; 2] {
    let (w, h) = (pic.width().max(1e-3), pic.height().max(1e-3));
    [f64::from((p.x - pic.min.x) / w) * f64::from(c.width), f64::from((p.y - pic.min.y) / h) * f64::from(c.height)]
}

fn to_screen(pic: Rect, c: &EcComp, q: [f64; 2]) -> Pos2 {
    let (w, h) = (f64::from(c.width.max(1)), f64::from(c.height.max(1)));
    pos2(pic.min.x + (q[0] / w) as f32 * pic.width(), pic.min.y + (q[1] / h) as f32 * pic.height())
}

/// The Before / After badge of a card's picture (none on small cards).
pub(crate) fn badge_rect(pic: Rect) -> Option<Rect> {
    (pic.width() >= 160.0 && pic.height() >= 60.0).then(|| Rect::from_min_size(pos2(pic.max.x - 8.0 - 104.0, pic.min.y + 6.0), vec2(104.0, 20.0)))
}

/// Pulse strength (1 → 0 each pulse) `dt` seconds into a flash; `None` once it's over.
pub(crate) fn pulse(dt: f64) -> Option<f32> {
    if !(0.0..FLASH * PULSES).contains(&dt) {
        return None;
    }
    let u = (dt / FLASH).fract();
    Some(((1.0 - u) * (1.0 - u)) as f32)
}

fn ids(v: &Value) -> Vec<NodeId> {
    v.as_array().into_iter().flatten().filter_map(figcraft_engine::params::id_of).take(256).collect()
}

/// UI requests: `canvas.flyTo {from, to}`, `card.flash {cards}`, `card.flashDismiss {cards}`.
pub(crate) fn request(app: &mut FigApp, kind: &str, data: &Value) {
    match kind {
        "canvas.flyTo" => {
            let view = |k: &str| -> Option<(f64, f64, f64)> {
                let a = data.get(k)?.as_array()?;
                Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_f64()?))
            };
            if let (Some(from), Some(to)) = (view("from"), view("to")) {
                let motion = !app.session.prefs.reduce_motion;
                super::cards::fly(app, from, to, motion);
            }
        }
        "card.flash" => {
            let new = ids(&data["cards"]);
            app.effectcraft.board.new_flashes.extend(new);
        }
        "card.flashDismiss" => {
            for id in ids(&data["cards"]) {
                app.effectcraft.board.flashing.remove(&id);
                if app.effectcraft.board.held == Some(id) {
                    app.effectcraft.board.held = None;
                }
            }
        }
        _ => {}
    }
}

/// Every frame: the host, the selection, flashes, keys, the mark's box.
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context) {
    if !app.effectcraft.board.installed {
        app.effectcraft.board.installed = true;
        if app.session.ecboard.host.is_none() {
            app.session.ecboard.host = Some(Box::new(Host::default()));
        }
    }
    ecboard::observe(&mut app.session);
    let now = ctx.input(|i| i.time);
    let st = &mut app.effectcraft.board;
    for id in std::mem::take(&mut st.new_flashes) {
        st.flashing.insert(id, now);
    }
    st.flashing.retain(|_, t| pulse(now - *t).is_some());
    if !st.flashing.is_empty() || st.pick.is_some_and(|p| now - p.2 < PICK_SHOWN) {
        ctx.request_repaint();
    }
    if st.pick.is_some_and(|p| now - p.2 >= PICK_SHOWN) {
        st.pick = None;
    }
    // Disarmed elsewhere (a command, the host): a stroke under way goes.
    if app.session.ecboard.marks.armed.is_none() {
        app.effectcraft.board.live = None;
    }
    keys(app, ctx, now);
    draft_box(app, ctx, now);
    super::presence::frame(app, ctx);
}

/// Whether the active page holds a MAIN board.
fn on_board(app: &FigApp) -> bool {
    ecboard::board_of(&app.session, "").is_ok_and(|b| !b.cards.is_empty())
}

fn arm(app: &mut FigApp, tool: Tool) {
    let id = if app.session.ecboard.marks.armed == Some(tool) { "canvas.annotate.stop" } else { "canvas.annotate.start" };
    app.run(id, json!({"tool": tool.id()}));
    if id == "canvas.annotate.start" {
        app.toast(match tool {
            Tool::Pen => "Pen: draw on a card. Esc stops.",
            Tool::Comment => "Comment: click a card. Esc stops.",
            Tool::Layer => "Layer: click a layer on a card (⌃⌥-click does it any time).",
        });
    }
}

/// P / C arm Pen / Comment on a page with a MAIN board (the canvas has the keys); Esc closes the
/// box first, then disarms.
fn keys(app: &mut FigApp, ctx: &egui::Context, now: f64) {
    if ctx.input(|i| i.key_pressed(Key::Escape)) {
        if let Some(p) = app.effectcraft.board.draft.as_mut().filter(|p| p.draft.closing.is_none()) {
            p.draft.closing = Some(now);
            ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            return;
        }
        if app.session.ecboard.marks.armed.is_some() {
            ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
            app.effectcraft.board.live = None;
            app.run("canvas.annotate.stop", json!({}));
            return;
        }
    }
    let panel = app.effectcraft.panel.as_ref().is_some_and(|p| p.open && p.focused);
    let editing = app.session.ds().is_ok_and(|d| d.text_edit.is_some() || d.vector_edit.is_some());
    if ctx.egui_wants_keyboard_input() || panel || editing || !on_board(app) {
        return;
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::P)) {
        arm(app, Tool::Pen);
    } else if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::C)) {
        arm(app, Tool::Comment);
    }
}

/// The open box: EffectCraft's own Pen bar / Comment card, popping from the mark.
fn draft_box(app: &mut FigApp, ctx: &egui::Context, now: f64) {
    let Some(card) = app.effectcraft.board.draft.as_ref().map(|p| p.card) else { return };
    let Some(doc) = app.session.ds().ok().map(|d| d.doc.clone()) else { return };
    let Some((pic, c)) = pic_of(app, &doc, card) else {
        app.effectcraft.board.draft = None;
        return;
    };
    let area = app.canvas.rect;
    let motion = ctx.global_style().animation_time > 0.0;
    let tokens = Tokens::for_kind(ThemeKind::Dark);
    let (drop, add) = {
        let st = &mut app.effectcraft.board;
        let (Some(p), auto) = (st.draft.as_mut(), &mut st.auto) else { return };
        let d = &mut p.draft;
        let pts: Vec<Pos2> = d.points.iter().map(|q| to_screen(pic, &c, *q)).collect();
        let mark = pts.iter().fold(Rect::NOTHING, |r, p| r.union(Rect::from_center_size(*p, vec2(1.0, 1.0)))).expand(4.0);
        let (alpha, scale, moving) = va::pop(now, d.opened, d.closing, motion);
        if moving {
            ctx.request_repaint();
        }
        let done = if d.closing.is_some() && !moving { Some(false) } else { va::mark_box(ctx, &tokens, auto, d, mark, area, alpha, scale) };
        let mut add = None;
        let mut drop = false;
        match done {
            Some(true) if d.closing.is_none() => {
                add = Some(json!({"card": card, "time": d.time, "tool": d.tool.id(), "points": d.points, "note": d.text.trim(), "source": "canvas"}));
                d.closing = Some(now);
                ctx.request_repaint();
            }
            Some(false) if d.closing.is_some_and(|t| !motion || now - t >= va::POP_OUT) => drop = true,
            Some(_) => {
                if d.closing.is_none() {
                    d.closing = Some(now);
                }
                ctx.request_repaint();
            }
            None => {}
        }
        (drop, add)
    };
    if drop {
        app.effectcraft.board.draft = None;
    }
    if let Some(params) = add
        && app.run("canvas.annotate.add", params).is_some()
    {
        app.toast("Queued for Claude");
    }
}

/// The cards with a Before / After badge on the active page.
fn badged(app: &FigApp, doc: &Document, page: NodeId) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = app.session.ecboard.flash.list.iter().filter(|f| f.before).flat_map(|f| f.cards.iter().copied()).collect();
    out.sort();
    out.dedup();
    out.retain(|id| doc.page_of(*id) == Some(page));
    out
}

/// Draw flashes, badges (and the frame before while one is held), waiting marks, the live stroke,
/// the open box's mark and the last layer pick. Runs after the cards' frames.
pub(crate) fn overlay(app: &FigApp, painter: &egui::Painter) {
    let Ok(ds) = app.session.ds() else { return };
    let (doc, page) = (&ds.doc, ds.page);
    let st = &app.effectcraft.board;
    let now = painter.ctx().input(|i| i.time);
    let font = egui::FontId::proportional(11.0);
    // Flashes.
    for (id, t0) in &st.flashing {
        if doc.page_of(*id) != Some(page) {
            continue;
        }
        let (Some((pic, _)), Some(k)) = (pic_of(app, doc, *id), pulse(now - t0)) else { continue };
        painter.rect_stroke(pic.expand(2.0), CornerRadius::same(6), Stroke::new(4.0, BLUE.gamma_multiply(k)), egui::StrokeKind::Outside);
        painter.rect_filled(pic, CornerRadius::ZERO, Color32::WHITE.gamma_multiply(0.18 * k));
    }
    // Before / After.
    for id in badged(app, doc, page) {
        let Some((pic, c)) = pic_of(app, doc, id) else { continue };
        let Some(badge) = badge_rect(pic) else { continue };
        let held = st.held == Some(id);
        if held {
            let f = super::frame_at(&c, super::card_time(app, doc, id, &c, None));
            match app.effectcraft.before_near(&c, f) {
                Some(tex) => {
                    painter.rect_filled(pic, CornerRadius::ZERO, Color32::from_rgb(0x0C, 0x0C, 0x0E));
                    painter.image(tex.id(), pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    painter.rect_filled(pic, CornerRadius::ZERO, Color32::from_black_alpha(200));
                    painter.text(
                        pic.center(),
                        egui::Align2::CENTER_CENTER,
                        "No frame from before the edit here",
                        font.clone(),
                        Color32::from_gray(200),
                    );
                }
            }
        }
        painter.rect_filled(badge, CornerRadius::same(10), if held { YELLOW } else { Color32::WHITE });
        painter.text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            "Before / After",
            egui::FontId::proportional(10.5),
            Color32::from_rgb(0x11, 0x11, 0x11),
        );
        let tag = if held { "BEFORE" } else { "AFTER" };
        let at = pos2(pic.max.x - 6.0, pic.max.y - 6.0);
        let g = painter.layout_no_wrap(tag.into(), egui::FontId::monospace(10.0), Color32::from_rgb(0xCF, 0xE3, 0xFF));
        let r = Rect::from_min_size(pos2(at.x - g.size().x - 8.0, at.y - g.size().y - 2.0), g.size() + vec2(8.0, 2.0));
        painter.rect_filled(r, CornerRadius::same(3), Color32::from_black_alpha(160));
        painter.galley(r.min + vec2(4.0, 1.0), g, Color32::from_rgb(0xCF, 0xE3, 0xFF));
    }
    // Marks waiting for the host, faint on their card.
    for m in &app.session.ecboard.marks.list {
        if doc.page_of(m.card) != Some(page) {
            continue;
        }
        let Some((pic, c)) = pic_of(app, doc, m.card) else { continue };
        let pts: Vec<Pos2> = m.points.iter().map(|q| to_screen(pic, &c, *q)).collect();
        let p = painter.with_clip_rect(pic.intersect(painter.clip_rect()));
        match m.tool {
            Tool::Pen => va::draw_screen_mark(&p, EcTool::Pen, &pts, 0.55),
            Tool::Comment => va::draw_screen_mark(&p, EcTool::Comment, &pts, 0.55),
            Tool::Layer => {
                if let Some(at) = pts.first() {
                    p.rect_stroke(
                        Rect::from_center_size(*at, vec2(10.0, 10.0)),
                        CornerRadius::same(2),
                        Stroke::new(2.0, YELLOW.gamma_multiply(0.55)),
                        egui::StrokeKind::Middle,
                    );
                }
            }
        }
    }
    // The live stroke and the open box's mark.
    if let Some(l) = &st.live
        && let Some((pic, c)) = pic_of(app, doc, l.card)
    {
        let mut shown = l.stroke.clone();
        if let Some(tip) = l.tip
            && shown.last().is_none_or(|x| *x != tip)
        {
            shown.push(tip);
        }
        let pts: Vec<Pos2> = shown.iter().map(|q| to_screen(pic, &c, *q)).collect();
        if pts.len() > 1 {
            va::draw_screen_mark(painter, EcTool::Pen, &pts, 1.0);
        }
        painter.ctx().request_repaint();
    }
    if let Some(p) = &st.draft
        && let Some((pic, c)) = pic_of(app, doc, p.card)
    {
        let pts: Vec<Pos2> = p.draft.points.iter().map(|q| to_screen(pic, &c, *q)).collect();
        va::draw_screen_mark(painter, p.draft.tool, &pts, 1.0);
    }
    // The last layer pick.
    if let Some((_, at, t0)) = st.pick
        && now - t0 < PICK_SHOWN
    {
        let k = (1.0 - (now - t0) / PICK_SHOWN).clamp(0.0, 1.0) as f32;
        painter.rect_stroke(
            Rect::from_center_size(at, vec2(80.0, 44.0)),
            CornerRadius::same(3),
            Stroke::new(2.0, YELLOW.gamma_multiply(k)),
            egui::StrokeKind::Middle,
        );
    }
    // Agents, on top.
    super::presence::overlay(app, painter);
}

/// Feed this frame's pointer samples to the Pen stroke; on release, open its box.
fn pen_step(app: &mut FigApp, ctx: &egui::Context) {
    let Some(card) = app.effectcraft.board.live.as_ref().map(|l| l.card) else { return };
    let Some(doc) = app.session.ds().ok().map(|d| d.doc.clone()) else { return };
    let Some((pic, c)) = pic_of(app, &doc, card) else {
        app.effectcraft.board.live = None;
        return;
    };
    let (down, samples, now) = ctx.input(|i| (i.pointer.primary_down(), va::drag_samples(&i.events), i.time));
    let Some(l) = app.effectcraft.board.live.as_mut() else { return };
    for s in samples {
        let q = to_comp(pic, &c, s);
        l.tip = Some(q);
        if l.stroke.len() < MAX_LIVE {
            l.stroke.extend(l.filter.push(q));
        }
    }
    ctx.request_repaint();
    if down {
        return;
    }
    let Some(mut l) = app.effectcraft.board.live.take() else { return };
    let mut points = std::mem::take(&mut l.stroke);
    if let Some(tip) = l.tip
        && points.last().is_some_and(|x| *x != tip)
    {
        points.push(tip);
    }
    let points = ink::finish(&points, va::KEEP_TOLERANCE * l.px);
    if points.len() >= 2 {
        let tc = format!("{} · {}", code(&c.comp), ecboard::timecode(l.time, c.fps));
        app.effectcraft.board.draft = Some(Pending { card, draft: va::Draft::new(EcTool::Pen, c.comp_id, l.time, tc, points, now) });
    }
}

/// "S1 06" of "S1 06 Old vs AI".
fn code(name: &str) -> String {
    name.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

/// The pointer for review marks: a held Before / After badge, the Pen's stroke, an agent's cursor
/// or glow, an armed tool or ⌃⌥ on a card. Returns whether it took the pointer.
pub(crate) fn input(app: &mut FigApp, ctx: &egui::Context, pos: Pos2, hovered: bool) -> bool {
    let (pressed, down, mods, now) =
        ctx.input(|i| (i.pointer.button_pressed(egui::PointerButton::Primary), i.pointer.primary_down(), i.modifiers, i.time));
    if app.effectcraft.board.held.is_some() {
        if !down {
            app.effectcraft.board.held = None;
        }
        return true;
    }
    if app.effectcraft.board.live.is_some() {
        pen_step(app, ctx);
        return true;
    }
    if !hovered {
        return false;
    }
    // A click on an agent's cursor or glow (no tool armed): the host opens that agent.
    if app.session.ecboard.marks.armed.is_none() && super::presence::input(app, ctx, pos, hovered) {
        return true;
    }
    let Ok(ds) = app.session.ds() else { return false };
    let (doc, page) = (ds.doc.clone(), ds.page);
    if pressed {
        for id in badged(app, &doc, page) {
            if pic_of(app, &doc, id).and_then(|(pic, _)| badge_rect(pic)).is_some_and(|b| b.contains(pos)) {
                if mods.alt {
                    app.run("card.flashDismiss", json!({"card": id}));
                } else {
                    app.effectcraft.board.held = Some(id);
                }
                return true;
            }
        }
    }
    let armed = app.session.ecboard.marks.armed;
    let pick = mods.ctrl && mods.alt;
    if armed.is_none() && !pick {
        return false;
    }
    let Some((id, pic)) = super::card_at(app, &doc, page, pos) else { return false };
    let Some(c) = doc.node(id).and_then(|n| n.effectcraft.as_deref()).cloned() else { return false };
    ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    // A new mark drops a box still open.
    if pressed && let Some(p) = app.effectcraft.board.draft.as_mut().filter(|p| p.draft.closing.is_none()) {
        p.draft.closing = Some(now);
    }
    let t = super::card_time(app, &doc, id, &c, None);
    let q = to_comp(pic, &c, pos);
    if pick || armed == Some(Tool::Layer) {
        if pressed {
            let r = app.run("canvas.annotate.add", json!({"card": id, "time": t, "tool": "layer", "x": q[0], "y": q[1], "source": "canvas"}));
            if let Some(text) = r.as_ref().and_then(|r| r["copyRef"].as_str()) {
                app.toast(format!("Chip added: {text}"));
            }
            app.effectcraft.board.pick = Some((id, pos, now));
        }
        return true;
    }
    match armed {
        Some(Tool::Pen) => {
            if pressed {
                let px = f64::from(c.width) / f64::from(pic.width().max(1e-3));
                let mut filter = InkFilter::new(px);
                let origin = ctx.input(|i| i.pointer.press_origin()).unwrap_or(pos);
                let first = to_comp(pic, &c, origin);
                let stroke: Vec<[f64; 2]> = filter.push(first).into_iter().collect();
                app.effectcraft.board.live = Some(Live { card: id, time: t, stroke, filter, tip: Some(first), px });
                pen_step(app, ctx);
            }
            true
        }
        Some(Tool::Comment) => {
            if pressed {
                let tc = format!("{} · {}", code(&c.comp), ecboard::timecode(t, c.fps));
                app.effectcraft.board.draft = Some(Pending { card: id, draft: va::Draft::new(EcTool::Comment, c.comp_id, t, tc, vec![q], now) });
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "effectcraft_board_tests.rs"]
mod tests;

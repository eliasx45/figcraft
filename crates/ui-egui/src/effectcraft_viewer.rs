//! Fork-only (`effectcraft-inproc`): EffectCraft's Composition viewer tools on the card the open
//! Timeline edits, behaving as in EffectCraft.
//!
//! - **Which.** Selection (V), Rotation (W), Pan Behind (Y), the shape tools (Q: rectangle,
//!   rounded rectangle, ellipse, polygon, star; a mask on a selected footage or solid layer, else a
//!   shape), the Pen (G: masks and shape paths; Add / Delete / Convert Vertex) and the Type tools
//!   (⌘T: click for text, drag for a paragraph box, click into a text layer and type). With the
//!   Selection tool, FigCraft's own layer handles move and scale ([`super::handles`]); path
//!   vertices and tangents of the selected layer drag, and a double-click on a text layer edits
//!   it in place.
//! - **Hosted, not rewritten.** The tools are EffectCraft's own viewer code
//!   (`effectcraft_ui_egui::panels::viewer_hosted`, EMBED8): the same gestures, snapping, undo
//!   steps and text editing as its Composition panel, running on the Timeline's session
//!   ([`crate::effectcraft_panel::EcHost`]). They draw and take the pointer in an egui area over
//!   the card's picture, mapped by a `ViewerMap` from the card's place on screen, so they follow
//!   the canvas's zoom and pan.
//! - **When.** The Timeline is open for the card and may edit, the card isn't playing, it shows
//!   the Timeline's comp, FigCraft's Move tool is on and the card is big enough on screen; Space
//!   held pans the canvas instead. The keys (EffectCraft's, Elias's own keymap: `tool.*`) pick the
//!   tool while the Timeline has the keyboard (a click in it or on the card); V returns to the
//!   Selection tool. The tool row in the Timeline's header shows and picks them too.
//! - **The rest of the viewer** (EMBED14): title/action safe, the grid, the proportional grid,
//!   the comp's guides and the 3D reference axes, each only while turned on (EffectCraft's keys
//!   ' ⌘' ⌘; ⌘R in the Timeline, or the grid button at the end of the tool row: EffectCraft's
//!   "Choose grid and guide options"); the rulers outside the picture's top and left edges, guides
//!   dragged out of them and along the picture; the selected layer's motion path, its keys and
//!   tangents dragged (the Motion Path setting decides how many keys show); and the Mask Feather
//!   tool (the Pen's slot: feather points on masks, their feather boundary while it is on).
//!   Guides and grids stay on the card while the tools sit out (another FigCraft tool, read-only).
//! - **Nothing over the picture** but what was turned on (guides, grids), the selected layers'
//!   paths, motion path, vertices and anchor point, and the tool's own feedback (shape outline,
//!   Pen rubber band, text caret and selection, feather boundary).

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use effectcraft_ui_egui::panels::viewer::ViewerMap;
use effectcraft_ui_egui::panels::viewer_hosted as vh;
use effectcraft_ui_egui::state::Tool;
use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, pos2, vec2};
use figcraft_doc::NodeId;
use serde_json::json;

use crate::FigApp;
use crate::effectcraft_panel::{EcHost, Panel};

/// Cards smaller than this on screen (points) take no tools (as [`super::handles`]).
const MIN_CARD: f32 = 60.0;

/// The tool row: (what its button picks, the tool command, its name).
pub(crate) const ROW: [(Slot, &str, &str); 6] = [
    (Slot::Selection, "tool.selection", "Selection Tool"),
    (Slot::Rotate, "tool.rotate", "Rotation Tool"),
    (Slot::PanBehind, "tool.panBehind", "Pan Behind (Anchor Point) Tool"),
    (Slot::Shape, "tool.shape", "Shape Tools: masks and shapes"),
    (Slot::Pen, "tool.pen", "Pen Tool: masks and shape paths"),
    (Slot::Type, "tool.type", "Type Tool"),
];

/// A button of the tool row: one tool, or a slot of several (as EffectCraft's Tools panel).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    Selection,
    Rotate,
    PanBehind,
    Shape,
    Pen,
    Type,
}

impl Slot {
    /// Whether `tool` is this button's (lit).
    pub(crate) fn has(self, tool: Tool) -> bool {
        match self {
            Slot::Selection => tool == Tool::Selection,
            Slot::Rotate => tool == Tool::Rotate,
            Slot::PanBehind => tool == Tool::PanBehind,
            Slot::Shape => tool.is_shape(),
            Slot::Pen => matches!(tool, Tool::Pen | Tool::PenAdd | Tool::PenDelete | Tool::PenConvert | Tool::MaskFeather),
            Slot::Type => matches!(tool, Tool::Type | Tool::TypeVertical),
        }
    }
}

/// The comp's picture on screen and its map, for card `card` (the Timeline edits it: `editing`).
pub(crate) fn picture(app: &FigApp, card: NodeId, host: &EcHost) -> Option<(Rect, ViewerMap)> {
    let doc = app.session.doc().ok()?;
    let c = doc.node(card).and_then(|n| n.effectcraft.as_deref())?;
    let r = super::screen_rect(app, doc, card)?;
    let pic = super::fit(r, c.width, c.height);
    if pic.width() < MIN_CARD || pic.height() < MIN_CARD / 2.0 {
        return None;
    }
    let comp = host.session().active_comp()?;
    // The comp's own size: the card may show it at another (frames are asked for scaled).
    let map = vh::map_for(pic, [comp.width, comp.height])?;
    Some((pic, map))
}

/// Whether the card shows the Timeline's comp, still (not playing): guides and grids that are
/// on show over it.
fn shows_overlays(app: &FigApp, p: &Panel) -> bool {
    if !p.open {
        return false;
    }
    let Ok(doc) = app.session.doc() else { return false };
    let Some(c) = doc.node(p.card).and_then(|n| n.effectcraft.as_deref()) else { return false };
    let ec = &app.session.effectcraft;
    let playing = ec.playing() && ec.node == Some(p.card);
    !playing && super::handles::shows_active_comp(p.host.session(), c)
}

/// Whether the viewer tools act on card `card` now (see the module doc).
fn active(app: &FigApp, p: &Panel) -> bool {
    if !p.open || app.effectcraft.picking || app.effectcraft.scrub.is_some() || app.effectcraft.handles.dragging() {
        return false;
    }
    if !matches!(app.session.tool.tool, figcraft_engine::tools::ToolId::Move) {
        return false;
    }
    p.host.read_only().is_none() && shows_overlays(app, p)
}

/// One frame of the viewer tools on the card the Timeline edits (`p`, taken out of the app while
/// the panel draws). Runs after the canvas, over it. Never panics into FigCraft.
pub(crate) fn card_tools(app: &mut FigApp, ctx: &egui::Context, p: &mut Panel) {
    // A click elsewhere took the keyboard from the Timeline: a text edit on the card ends.
    if !p.focused && p.host.session().state.text_edit.is_some() && p.host.read_only().is_none() {
        let _ = p.host.run_engine(ctx, "text.endEdit", json!({}));
    }
    let tools_on = active(app, p);
    if !tools_on && !shows_overlays(app, p) {
        return;
    }
    let Some((pic, map)) = picture(app, p.card, &p.host) else { return };
    // Room for the rulers above and left of the picture (they never cover it).
    let room = if p.host.ui().viewer.rulers { vh::RULER + 2.0 } else { 0.0 };
    let clip = app.canvas.rect.intersect(Rect::from_min_max(pic.min - vec2(room, room), pic.max).expand(12.0));
    if !clip.is_positive() {
        return;
    }
    if !tools_on {
        // The tools sit out: only what was turned on (guides, grids) stays on the card.
        let id = egui::Id::new("effectcraft_card_overlays");
        egui::Area::new(id).order(egui::Order::Middle).fixed_pos(pic.min).constrain(false).movable(false).interactable(false).show(ctx, |ui| {
            ui.set_clip_rect(clip);
            let host = &mut p.host;
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vh::view_overlays(host, ui, map))).is_err() {
                log::warn!("EffectCraft card guides: an internal error");
            }
        });
        return;
    }
    let opts = vh::Options { id: egui::Id::new(("effectcraft-card-tools", p.card)), space_pan: app.canvas.space_pan };
    let id = egui::Id::new("effectcraft_card_tools");
    let mut frame = vh::Frame::default();
    // Over the canvas (Middle): what the tools take never reaches it. The area itself senses
    // nothing, so where they take nothing the canvas still gets the pointer.
    egui::Area::new(id).order(egui::Order::Middle).fixed_pos(pic.min).constrain(false).movable(false).sense(egui::Sense::hover()).show(ctx, |ui| {
        ui.set_clip_rect(clip);
        let host = &mut p.host;
        frame = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vh::tools(host, ui, map, opts))).unwrap_or_else(|_| {
            log::warn!("EffectCraft card tools: an internal error");
            vh::Frame::default()
        });
    });
    // A press there is an EffectCraft edit: the Timeline has the keyboard (⌘Z undoes it there).
    if frame.owns && ctx.input(|i| i.pointer.any_pressed()) {
        p.focused = true;
        p.host.set_focus(effectcraft_ui_egui::dock::PanelKind::Timeline);
    }
}

/// A double-click on card `card` with the Selection tool: edit the text layer under `pos` in
/// place, as EffectCraft's viewer does. Returns whether one was.
pub(crate) fn double_click(app: &mut FigApp, ctx: &egui::Context, card: NodeId, pos: Pos2) -> bool {
    let Some(p) = app.effectcraft.panel.as_ref().filter(|p| p.card == card) else { return false };
    if !active(app, p) || vh::active(&p.host) != Tool::Selection {
        return false;
    }
    let Some((_, map)) = picture(app, card, &p.host) else { return false };
    let Some(p) = app.effectcraft.panel.as_mut() else { return false };
    let host = &mut p.host;
    let edited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vh::edit_text_at(host, ctx, map, pos))).unwrap_or(false);
    if edited {
        p.focused = true;
        p.host.set_focus(effectcraft_ui_egui::dock::PanelKind::Timeline);
        ctx.request_repaint();
    }
    edited
}

/// The tool row in the Timeline's header: a button per tool (lit: the current one), hovering
/// names it with its key from EffectCraft's keymap. A click picks it as its key would.
pub(crate) fn tool_row(ui: &mut egui::Ui, host: &mut EcHost) {
    let side = 18.0;
    let tool = vh::active(host);
    let accent = crate::theme::canvas_accent(ui.ctx());
    let t = crate::theme::Tokens::get(ui.ctx());
    for (slot, id, name) in ROW {
        let (r, resp) = ui.allocate_exact_size(vec2(side, side), egui::Sense::click());
        let on = slot.has(tool);
        let p = ui.painter();
        if on {
            p.rect_filled(r, CornerRadius::same(3), accent.gamma_multiply(0.35));
        } else if resp.hovered() {
            p.rect_filled(r, CornerRadius::same(3), t.hover);
        }
        let shown = if on { tool } else { slot_tool(host, slot) };
        paint_icon(p, r.shrink(3.0), shown, if on { Color32::WHITE } else { t.icon });
        let key = host.session().shortcuts().shortcut_of(id, &json!({})).map(str::to_string);
        let tip = match key {
            Some(k) if !k.is_empty() => format!("{name} ({k})"),
            _ => name.to_string(),
        };
        if resp.on_hover_text(tip).clicked()
            && !on
            && let Err(e) = host.invoke(ui.ctx(), id, json!({}))
        {
            host.ui_mut().status = e;
        }
    }
    ui.add_space(4.0);
    grid_button(ui, host);
}

fn grid_popup_id() -> egui::Id {
    egui::Id::new("effectcraft_card_grid_options")
}

/// EffectCraft's "Choose grid and guide options" button (lit while one of them shows): a menu of
/// Title/Action Safe, Proportional Grid, Grid, Guides, Rulers and 3D Reference Axes, ticked when
/// on; picking one turns it on or off as in EffectCraft's viewer.
pub(crate) fn grid_button(ui: &mut egui::Ui, host: &mut EcHost) {
    let side = 18.0;
    let accent = crate::theme::canvas_accent(ui.ctx());
    let t = crate::theme::Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(vec2(side, side), egui::Sense::click());
    let on = vh::grid_options_on(host);
    let id = grid_popup_id();
    let open = egui::Popup::is_id_open(ui.ctx(), id);
    let p = ui.painter();
    if on || open {
        p.rect_filled(r, CornerRadius::same(3), accent.gamma_multiply(0.35));
    } else if resp.hovered() {
        p.rect_filled(r, CornerRadius::same(3), t.hover);
    }
    paint_grid_icon(p, r.shrink(3.0), if on { Color32::WHITE } else { t.icon });
    if resp.on_hover_text("Choose grid and guide options").clicked() {
        if open {
            egui::Popup::close_id(ui.ctx(), id);
        } else {
            egui::Popup::open_id(ui.ctx(), id);
        }
    }
    if !egui::Popup::is_id_open(ui.ctx(), id) {
        return;
    }
    // Under the button, in window coordinates (the Timeline's layer is scaled with the canvas).
    let to_window = ui.ctx().layer_transform_to_global(ui.layer_id()).unwrap_or_default();
    let at = to_window * r.left_bottom();
    if !at.x.is_finite() || !at.y.is_finite() {
        egui::Popup::close_id(ui.ctx(), id);
        return;
    }
    let items = vh::grid_options(host);
    let mut picked = None;
    egui::Popup::new(id, ui.ctx().clone(), egui::PopupAnchor::Position(at), egui::LayerId::new(egui::Order::Foreground, id))
        .kind(egui::PopupKind::Menu)
        .layout(egui::Layout::top_down_justified(egui::Align::Min))
        .style(crate::right::menu_style)
        .open_memory(None)
        .show(|ui| {
            for (i, (name, ticked)) in items.iter().enumerate() {
                if name == "-" {
                    ui.separator();
                    continue;
                }
                let mut v = *ticked;
                if ui.checkbox(&mut v, name.as_str()).clicked() {
                    picked = Some(i);
                }
            }
        });
    if let Some(i) = picked {
        vh::toggle_grid_option(host, i);
        egui::Popup::close_id(ui.ctx(), id);
    }
}

/// The grid button's icon: a square cut in three by two lines each way (original, drawn in code).
pub(crate) fn paint_grid_icon(p: &egui::Painter, r: Rect, col: Color32) {
    if !r.is_finite() || r.width() <= 0.0 || r.height() <= 0.0 {
        return;
    }
    let k = r.width().min(r.height()) / 16.0;
    let at = |x: f32, y: f32| pos2(r.min.x + x * k, r.min.y + y * k);
    let s = Stroke::new((1.1 * k).max(0.7), col);
    p.rect_stroke(Rect::from_min_max(at(2.0, 2.0), at(14.0, 14.0)), CornerRadius::ZERO, s, StrokeKind::Middle);
    for v in [6.0, 10.0] {
        p.line_segment([at(v, 2.0), at(v, 14.0)], s);
        p.line_segment([at(2.0, v), at(14.0, v)], s);
    }
}

/// The tool a slot's button shows when it isn't the current one: the slot's last used.
fn slot_tool(host: &EcHost, slot: Slot) -> Tool {
    let ui = host.ui();
    let last = |i: usize, d: Tool| ui.slot_tools.get(i).copied().unwrap_or(d);
    match slot {
        Slot::Selection => Tool::Selection,
        Slot::Rotate => Tool::Rotate,
        Slot::PanBehind => Tool::PanBehind,
        Slot::Shape => last(8, Tool::Rectangle),
        Slot::Pen => last(9, Tool::Pen),
        Slot::Type => last(10, Tool::Type),
    }
}

/// A tool's icon, drawn in code from lines and simple shapes in a 16-unit box filling `r`
/// (original, FigCraft's own).
pub(crate) fn paint_icon(p: &egui::Painter, r: Rect, tool: Tool, col: Color32) {
    if !r.is_finite() || r.width() <= 0.0 {
        return;
    }
    let k = r.width().min(r.height()) / 16.0;
    let at = |x: f32, y: f32| pos2(r.min.x + x * k, r.min.y + y * k);
    let s = Stroke::new((1.3 * k).max(0.8), col);
    match tool {
        Tool::Selection => {
            let pts = vec![at(4.0, 2.0), at(4.0, 13.5), at(7.0, 10.6), at(9.2, 15.0), at(11.0, 14.1), at(8.9, 9.8), at(12.6, 9.8)];
            p.add(egui::Shape::convex_polygon(pts, col, Stroke::NONE));
        }
        Tool::Rotate => {
            // Three quarters of a circle with an arrowhead at its open end.
            let c = at(8.0, 8.5);
            let rad = 5.0 * k;
            let arc: Vec<Pos2> = (0..=18)
                .map(|i| {
                    let a = (-80.0 + 270.0 * i as f32 / 18.0).to_radians();
                    c + vec2(a.cos(), a.sin()) * rad
                })
                .collect();
            p.add(egui::Shape::line(arc, s));
            let tip = c + vec2((-80.0f32).to_radians().cos(), (-80.0f32).to_radians().sin()) * rad;
            p.add(egui::Shape::convex_polygon(
                vec![tip + vec2(2.6, 0.0) * k, tip + vec2(-1.6, -2.4) * k, tip + vec2(-1.2, 2.6) * k],
                col,
                Stroke::NONE,
            ));
        }
        Tool::PanBehind => {
            let c = at(8.0, 8.0);
            p.circle_stroke(c, 3.6 * k, s);
            p.line_segment([at(8.0, 1.5), at(8.0, 14.5)], s);
            p.line_segment([at(1.5, 8.0), at(14.5, 8.0)], s);
        }
        Tool::Ellipse => {
            p.add(egui::Shape::ellipse_stroke(at(8.0, 8.0), vec2(6.0, 4.6) * k, s));
        }
        Tool::Polygon | Tool::Star => {
            let n = if tool == Tool::Star { 10 } else { 5 };
            let pts: Vec<Pos2> = (0..n)
                .map(|i| {
                    let a = (-90.0 + 360.0 * i as f32 / n as f32).to_radians();
                    let rad = if tool == Tool::Star && i % 2 == 1 { 2.8 } else { 6.2 };
                    at(8.0, 8.4) + vec2(a.cos(), a.sin()) * rad * k
                })
                .collect();
            p.add(egui::Shape::closed_line(pts, s));
        }
        Tool::Rectangle | Tool::RoundedRect => {
            let round = if tool == Tool::RoundedRect { (3.0 * k) as u8 } else { 0 };
            p.rect_stroke(Rect::from_min_max(at(2.0, 3.5), at(14.0, 12.5)), CornerRadius::same(round), s, StrokeKind::Middle);
        }
        Tool::Type | Tool::TypeVertical => {
            p.line_segment([at(3.0, 3.0), at(13.0, 3.0)], s);
            p.line_segment([at(8.0, 3.0), at(8.0, 14.0)], s);
            p.line_segment([at(6.0, 14.0), at(10.0, 14.0)], s);
            if tool == Tool::TypeVertical {
                p.line_segment([at(13.0, 8.0), at(13.0, 14.0)], s);
            }
        }
        // The Pen and its vertex tools: a nib (a point, a slit and its hole), the vertex tools
        // with a plus, a minus or an angle beside it.
        _ => {
            let nib = vec![at(6.5, 2.0), at(10.5, 8.0), at(8.5, 13.0), at(4.5, 13.0), at(2.5, 8.0)];
            p.add(egui::Shape::closed_line(nib, s));
            p.line_segment([at(6.5, 2.0), at(6.5, 7.5)], s);
            p.circle_filled(at(6.5, 8.6), 1.0 * k, col);
            match tool {
                Tool::PenAdd => {
                    p.line_segment([at(12.0, 3.0), at(12.0, 8.0)], s);
                    p.line_segment([at(9.5, 5.5), at(14.5, 5.5)], s);
                }
                Tool::PenDelete => {
                    p.line_segment([at(9.5, 5.5), at(14.5, 5.5)], s);
                }
                Tool::PenConvert => {
                    p.add(egui::Shape::line(vec![at(10.5, 2.5), at(14.0, 6.0), at(10.5, 9.5)], s));
                }
                Tool::MaskFeather => {
                    // A soft edge: a dashed arc beside the nib.
                    let c = at(11.0, 9.0);
                    for i in 0..4 {
                        let a0 = (-80.0 + 40.0 * i as f32).to_radians();
                        let a1 = a0 + 22.0f32.to_radians();
                        let rad = 4.0 * k;
                        p.line_segment([c + vec2(a0.cos(), a0.sin()) * rad, c + vec2(a1.cos(), a1.sin()) * rad], s);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "effectcraft_viewer_tests.rs"]
mod tests;

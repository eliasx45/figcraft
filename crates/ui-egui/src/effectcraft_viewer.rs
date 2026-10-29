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
//! - **EffectCraft's own row and bar.** The Timeline header's tool row is EffectCraft's Tools bar
//!   (`effectcraft_ui_egui::panels::toolbar_hosted`, EMBED16: every slot, flyout, tooltip and the
//!   owner-local icons, at the header's size) and EffectCraft's Composition viewer bar sits under
//!   the card's picture (`viewer_tools::hosted_bar`: magnification, resolution, transparency grid,
//!   mask visibility, region of interest, grid and guide options, channel, exposure, snapshots,
//!   Fast Previews, 3D renderer and view, the time), both on the Timeline's session, so they stay
//!   what EffectCraft draws as it changes. FigCraft draws neither.
//! - **The rest of the viewer** (EMBED14): title/action safe, the grid, the proportional grid,
//!   the comp's guides and the 3D reference axes, each only while turned on (EffectCraft's keys
//!   ' ⌘' ⌘; ⌘R in the Timeline, or the grid button in the viewer bar under the card: EffectCraft's
//!   "Choose grid and guide options"); the rulers outside the picture's top and left edges, guides
//!   dragged out of them and along the picture; the selected layer's motion path, its keys and
//!   tangents dragged (the Motion Path setting decides how many keys show); and the Mask Feather
//!   tool (the Pen's slot: feather points on masks, their feather boundary while it is on).
//!   Guides and grids stay on the card while the tools sit out (another FigCraft tool, read-only).
//! - **Nothing over the picture** but what was turned on (guides, grids), the selected layers'
//!   paths, motion path, vertices and anchor point, and the tool's own feedback (shape outline,
//!   Pen rubber band, text caret and selection, feather boundary).

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use effectcraft_ui_egui::panels::toolbar_hosted as tb;
use effectcraft_ui_egui::panels::viewer::ViewerMap;
use effectcraft_ui_egui::panels::viewer_hosted as vh;
use effectcraft_ui_egui::state::Tool;
use egui::{Pos2, Rect, vec2};
use figcraft_doc::NodeId;
use serde_json::json;

use crate::FigApp;
use crate::effectcraft_panel::{EcHost, Panel};

/// Cards smaller than this on screen (points) take no tools (as [`super::handles`]).
const MIN_CARD: f32 = 60.0;

/// The tools a card can run: EffectCraft's hosted viewer tools, and Hand and Zoom, which act on
/// the canvas (see [`view_tool`]).
///
/// CDXC:Editing 2026-10-29 WHY: the card's view is the canvas's (FigCraft zooms and pans every
/// card at once), so Hand pans the canvas and Zoom zooms it about the click (Alt: out), and the
/// viewer bar's magnification sets the canvas zoom so the card shows at that size (Fit flies to the
/// card): a card-only zoom would fight the canvas's. The camera tools (they move a 3D camera
/// through EffectCraft's full Composition viewer), Brush / Clone / Eraser, Roto Brush / Refine
/// Edge / Object Matte and the Puppet pins (strokes, propagation, meshes that only that viewer
/// runs) are not hosted: their slots stay in the row, dimmed, and say to use them in EffectCraft.
pub(crate) fn card_can(tool: Tool) -> bool {
    vh::hosted(tool) || matches!(tool, Tool::Hand | Tool::Zoom)
}

/// What a tool the card can't run says (its tooltip and the status line).
pub(crate) const ELSEWHERE: &str = "Not on a canvas card: open the comp in EffectCraft's Composition panel to use it.";

/// The tool row's slot side (panel units: the Timeline header is [`super::strips::HEADER`]).
const SLOT: f32 = 18.0;

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
    if matches!(p.host.ui().tool, Tool::Hand | Tool::Zoom) {
        view_tool(app, ctx, p, pic, clip);
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
    if !active(app, p) || p.host.ui().tool != Tool::Selection {
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

/// The tool row in the Timeline's header: EffectCraft's own Tools bar slots (EMBED16), at the
/// header's size, on the Timeline's session. Tools the card can't run are dimmed ([`card_can`]).
pub(crate) fn tool_row(ui: &mut egui::Ui, host: &mut EcHost) {
    let (r, _) = ui.allocate_exact_size(vec2(tb::width(SLOT), SLOT), egui::Sense::hover());
    let works: &dyn Fn(Tool) -> bool = &card_can;
    let opts = tb::Options { side: SLOT, id_scope: Some(egui::Id::new("effectcraft-card-tool-row")), works: Some(works), elsewhere: ELSEWHERE };
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tb::slots(host, ui, r.min.x, r.center().y, &opts)));
    if ran.is_err() {
        log::warn!("EffectCraft tool row: an internal error");
    }
}

/// EffectCraft's Composition viewer bar in `bar` (panel units, under the card's picture) on the
/// Timeline's session; `shown` is the card's magnification on screen (screen points per comp
/// pixel). Returns the magnification picked from its menu (`None` in it: Fit), for
/// [`set_magnification`].
pub(crate) fn viewer_bar(ui: &mut egui::Ui, host: &mut EcHost, bar: Rect, shown: f32) -> Option<Option<f32>> {
    let shown = if shown.is_finite() && shown > 0.0 { shown } else { 1.0 };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| effectcraft_ui_egui::panels::viewer_tools::hosted_bar(host, ui, bar, shown)))
        .unwrap_or_else(|_| {
            log::warn!("EffectCraft viewer bar: an internal error");
            None
        })
        .map(|(z, _)| z)
}

/// The card's magnification on screen for card `card` (screen points per comp pixel of the
/// Timeline's comp), 0 when it isn't known.
pub(crate) fn shown_magnification(app: &FigApp, card: NodeId, host: &EcHost) -> f32 {
    let Ok(doc) = app.session.doc() else { return 0.0 };
    let Some(c) = doc.node(card).and_then(|n| n.effectcraft.as_deref()) else { return 0.0 };
    let Some(r) = super::screen_rect(app, doc, card) else { return 0.0 };
    let pic = super::fit(r, c.width, c.height);
    let Some(comp) = host.session().active_comp() else { return 0.0 };
    let m = pic.width() / comp.width.max(1) as f32;
    if m.is_finite() { m } else { 0.0 }
}

/// The viewer bar's magnification asked for card `card`: the canvas zooms so the card's picture
/// shows at `want` (screen points per comp pixel; it shows at `shown` now), about the card's
/// centre; `None` (Fit) flies to the card. See [`card_can`] for why the canvas, not the card.
pub(crate) fn set_magnification(app: &mut FigApp, card: NodeId, want: Option<f32>, shown: f32) {
    let Some(want) = want else {
        app.run("canvas.flyTo", json!({"card": card}));
        return;
    };
    let Ok(ds) = app.session.ds() else { return };
    let Some(b) = ds.doc.world_bounds(card) else { return };
    let zoom = ds.view.zoom * f64::from(want) / f64::from(shown);
    if !zoom.is_finite() || zoom <= 0.0 || !shown.is_finite() || shown <= 0.0 {
        return;
    }
    let zoom = zoom.clamp(figcraft_engine::ViewState::MIN_ZOOM, figcraft_engine::ViewState::MAX_ZOOM);
    let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
    let _ = app.session.execute("view.set", &json!({"cx": cx, "cy": cy, "zoom": zoom}));
}

/// The zoom of one Zoom tool click: in ×2, out (Alt) ×½.
const ZOOM_STEP: f64 = 2.0;

/// The canvas view after zooming by `factor` about page point (`px`, `py`) from view (`cx`, `cy`,
/// `zoom`): the point stays under the pointer.
pub(crate) fn zoom_about(view: (f64, f64, f64), at: (f64, f64), factor: f64) -> Option<(f64, f64, f64)> {
    let (cx, cy, zoom) = view;
    let z = (zoom * factor).clamp(figcraft_engine::ViewState::MIN_ZOOM, figcraft_engine::ViewState::MAX_ZOOM);
    let k = zoom / z;
    let out = (at.0 - (at.0 - cx) * k, at.1 - (at.1 - cy) * k, z);
    [out.0, out.1, out.2].iter().all(|v| v.is_finite()).then_some(out)
}

/// The Hand and Zoom tools over the card's picture `pic` (see [`card_can`]): Hand drags pan the
/// canvas, a Zoom click zooms it in about the click (Alt: out).
fn view_tool(app: &mut FigApp, ctx: &egui::Context, p: &mut Panel, pic: Rect, clip: Rect) {
    let tool = p.host.ui().tool;
    let id = egui::Id::new(("effectcraft-card-view-tool", p.card));
    let mut resp = None;
    egui::Area::new(egui::Id::new("effectcraft_card_view_tool")).order(egui::Order::Middle).fixed_pos(pic.min).constrain(false).movable(false).show(
        ctx,
        |ui| {
            ui.set_clip_rect(clip);
            resp = Some(ui.interact(pic, id, egui::Sense::click_and_drag()));
        },
    );
    let Some(resp) = resp else { return };
    let Ok(ds) = app.session.ds() else { return };
    let view = (ds.view.cx, ds.view.cy, ds.view.zoom);
    let alt = ctx.input(|i| i.modifiers.alt);
    if resp.hovered() || resp.dragged() {
        ctx.set_cursor_icon(match tool {
            Tool::Hand if resp.dragged() => egui::CursorIcon::Grabbing,
            Tool::Hand => egui::CursorIcon::Grab,
            _ if alt => egui::CursorIcon::ZoomOut,
            _ => egui::CursorIcon::ZoomIn,
        });
    }
    let to = match tool {
        Tool::Hand if resp.dragged() => {
            let d = resp.drag_delta();
            let z = view.2.max(1e-9);
            Some((view.0 - f64::from(d.x) / z, view.1 - f64::from(d.y) / z, view.2))
        }
        Tool::Zoom if resp.clicked() => resp.interact_pointer_pos().and_then(|at| {
            let page = app.to_page(at);
            zoom_about(view, (page.x, page.y), if alt { 1.0 / ZOOM_STEP } else { ZOOM_STEP })
        }),
        _ => None,
    };
    if let Some((cx, cy, zoom)) = to.filter(|v| [v.0, v.1, v.2].iter().all(|x| x.is_finite())) {
        let _ = app.session.execute("view.set", &json!({"cx": cx, "cy": cy, "zoom": zoom}));
    }
    if resp.is_pointer_button_down_on() {
        p.focused = true;
    }
}

#[cfg(test)]
#[path = "effectcraft_viewer_tests.rs"]
mod tests;

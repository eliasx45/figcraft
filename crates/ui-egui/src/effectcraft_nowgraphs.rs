//! Fork-only (`effectcraft-inproc`): the "Now graphs" shelf (Elias, 29 Oct 2026, approved after
//! the mock `effectcraft/plan/pause-graphs-playground.html`: "go ahead ... should work on main").
//!
//! - **When.** The card the Timeline edits is paused (Space or its pause button) and shows the
//!   Timeline's comp: the shelf docks to the right of the card, top-aligned with its picture, at a
//!   fixed screen size. It floats over the canvas (never over the viewer, never moving another
//!   panel or card). Playing hides it; its × tucks it away until the next pause (a small
//!   "Now graphs" tab brings it back).
//! - **What.** Every keyframed property whose segment touches the playhead ± 1 s, through every
//!   precomp the card's comp shows, all at once (EffectCraft's `nowGraphs.list`): per property one
//!   line "Layer · Prop", the segment's speed graph with its two influence handles (out of the
//!   first key, in of the second) and an ease chip ▾ listing Elias's saved Graphs (his approved
//!   curves in EffectCraft's taste profile: Expo In-Out, Expo Out 1, the entrance fade…) and
//!   Linear. The path (MAIN › precomp › layer › prop) and the numbers are a faint line on hover.
//! - **Which is which.** Each animated object has its own colour (no purple, [`PALETTE`]): while
//!   the shelf shows, its box is outlined on the picture in that colour (1.5 px, no fill, no
//!   label; an object off the frame gets a dot on the picture's edge), its cards carry a stripe
//!   of it and a crop of the object from the card's current frame. Hovering a card brightens its
//!   outline (the others dim); hovering an object on the picture lights its cards.
//! - **Edits** are EffectCraft commands on the Timeline's session (`nowGraphs.setInfluence`, one
//!   undo step per drag; `nowGraphs.applyEase`, one per pick), so they mirror into the app, ⌘Z
//!   undoes them in EffectCraft and the card re-renders as for any Timeline edit.
//! - **Agents.** Widgets register automation ids (`nowgraphs-shelf`, `nowgraphs-card-<n>`,
//!   `nowgraphs-out-<n>`, `nowgraphs-in-<n>`, `nowgraphs-ease-<n>`) in the Timeline's registry and
//!   in [`State::widgets`]; the control channel's `effectcraft.nowGraphs` reads the shelf (its
//!   serde [`View`], cards, objects, rects) and `effectcraft.nowGraphs.setInfluence` /
//!   `.applyEase` / `.set` drive it ([`control`]).

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use effectcraft_ui_egui::panels::viewer_hosted as vh;
use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FigApp;
use crate::effectcraft_panel::{EcHost, Panel};

/// The shelf's width and its gap from the card (screen points).
pub(crate) const SHELF_W: f32 = 300.0;
pub(crate) const GAP: f32 = 12.0;
/// Shortest shelf (screen points), when the card's picture is smaller.
const MIN_SHELF_H: f32 = 180.0;
/// One graph card's height and the header's.
pub(crate) const CARD_H: f32 = 96.0;
const HEAD_H: f32 = 24.0;
/// Cards smaller than this on screen (points) get no shelf.
const MIN_CARD: f32 = 60.0;
/// The outline's width, and while its card is hovered.
const OUTLINE: f32 = 1.5;
const OUTLINE_HOT: f32 = 2.5;
/// Screen points either side of a handle that grab it.
const GRAB: f32 = 7.0;
/// The thumbnail's size.
const THUMB: egui::Vec2 = vec2(30.0, 18.0);

const PANEL: Color32 = Color32::from_rgb(0x1d, 0x1d, 0x20);
const CARD: Color32 = Color32::from_rgb(0x19, 0x19, 0x1c);
const LINE: Color32 = Color32::from_rgb(0x34, 0x34, 0x3a);
const EDGE: Color32 = Color32::from_rgb(0x3c, 0x5c, 0x8c);
const FG: Color32 = Color32::from_rgb(0xe6, 0xe6, 0xea);
const MUTED: Color32 = Color32::from_rgb(0x8e, 0x8e, 0x98);
const HOT: Color32 = Color32::from_rgb(0x2d, 0x8c, 0xff);
const KEY: Color32 = Color32::from_rgb(0xe8, 0xc3, 0x4a);

/// The objects' colours, in the order objects are found (no purple, no magenta).
pub(crate) const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x2d, 0x8c, 0xff), // blue
    Color32::from_rgb(0xff, 0x8a, 0x3d), // orange
    Color32::from_rgb(0x2f, 0xb3, 0x6f), // green
    Color32::from_rgb(0xe8, 0xc3, 0x4a), // yellow
    Color32::from_rgb(0x22, 0xc3, 0xe6), // cyan
    Color32::from_rgb(0xff, 0x6b, 0x5a), // coral
    Color32::from_rgb(0xa3, 0xd9, 0x4a), // lime
    Color32::from_rgb(0xc8, 0xa2, 0x7a), // tan
];

/// Object `i`'s colour.
pub(crate) fn colour(i: usize) -> Color32 {
    PALETTE.get(i % PALETTE.len()).copied().unwrap_or(HOT)
}

// ---------------------------------------------------------------- the listing

/// One side's ease: normalised speed, influence %.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Side {
    #[serde(default)]
    pub speed: f64,
    #[serde(default)]
    pub influence: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Ease {
    #[serde(default)]
    pub out: Side,
    #[serde(default, rename = "in")]
    pub inn: Side,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Interp {
    #[serde(default)]
    pub out: String,
    #[serde(default, rename = "in")]
    pub inn: String,
}

/// One graph of `nowGraphs.list`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Graph {
    pub comp: u64,
    pub layer: u64,
    pub layer_name: String,
    pub prop: u64,
    pub path: String,
    pub property: String,
    pub trail: Vec<String>,
    pub key: u64,
    pub start: f64,
    pub end: f64,
    pub interp: Interp,
    pub ease: Option<Ease>,
    pub speed: Vec<f64>,
    pub object: usize,
    pub curve: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Object {
    pub object: usize,
    pub comp: u64,
    pub layer: u64,
    pub name: String,
    pub quad: Option<Vec<[f64; 2]>>,
    pub off_frame: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Curve {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Listing {
    pub comp: u64,
    pub time: f64,
    pub width: u32,
    pub height: u32,
    pub graphs: Vec<Graph>,
    pub objects: Vec<Object>,
    pub curves: Vec<Curve>,
}

impl Listing {
    pub(crate) fn parse(v: &Value) -> Listing {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    fn curve_name(&self, id: &str) -> Option<&str> {
        self.curves.iter().find(|c| c.id == id).map(|c| c.name.as_str())
    }
}

/// What the ease chip says for `g`: Linear, the saved curve it matches, or Custom.
pub(crate) fn ease_label(l: &Listing, g: &Graph) -> String {
    if g.interp.out == "linear" && g.interp.inn == "linear" {
        return "Linear".into();
    }
    match g.curve.as_deref() {
        Some(id) => l.curve_name(id).unwrap_or(id).to_string(),
        None => "Custom".into(),
    }
}

/// `nowGraphs.list` on the Timeline's session for its comp at its time. Never panics.
pub(crate) fn list(host: &mut EcHost) -> Listing {
    let (comp, time) = (host.comp(), host.time());
    let r =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.session_mut().execute("nowGraphs.list", json!({"comp": comp, "time": time}))));
    match r {
        Ok(Ok(v)) => Listing::parse(&v),
        Ok(Err(e)) => {
            log::debug!("Now graphs: {e}");
            Listing::default()
        }
        Err(_) => {
            log::warn!("Now graphs: an internal error listing the graphs");
            Listing::default()
        }
    }
}

/// The `nowGraphs.setInfluence` call for graph `g` (`out`/`in` %; `merge`: the drag's key).
pub(crate) fn influence_call(g: &Graph, out: Option<f64>, inn: Option<f64>, merge: &str) -> Value {
    let mut p = json!({"comp": g.comp, "layer": g.layer, "prop": g.prop, "key": g.key, "merge": merge});
    if let Some(v) = out.filter(|v| v.is_finite()) {
        p["out"] = json!(v);
    }
    if let Some(v) = inn.filter(|v| v.is_finite()) {
        p["in"] = json!(v);
    }
    p
}

/// The `nowGraphs.applyEase` call that puts curve `curve` on graph `g`.
pub(crate) fn ease_call(g: &Graph, curve: &str) -> Value {
    json!({"comp": g.comp, "layer": g.layer, "prop": g.prop, "key": g.key, "curve": curve})
}

// ---------------------------------------------------------------- geometry

/// Where the shelf docks: right of the card `card`, top-aligned with its picture `pic`, at least
/// [`MIN_SHELF_H`] tall. Never over the picture.
pub(crate) fn shelf_rect(card: Rect, pic: Rect) -> Rect {
    let h = pic.height().max(MIN_SHELF_H);
    Rect::from_min_size(pos2(card.max.x.max(pic.max.x) + GAP, pic.min.y), vec2(SHELF_W, h))
}

/// A handle's x in graph area `g`: out handles from the left edge, in handles from the right.
pub(crate) fn handle_x(g: Rect, out: bool, influence: f64) -> f32 {
    let k = (influence.clamp(0.0, 100.0) / 100.0) as f32;
    if out { g.min.x + g.width() * k } else { g.max.x - g.width() * k }
}

/// The influence (%) a handle dragged to `x` in graph area `g` asks for (0.1–100).
pub(crate) fn influence_at(g: Rect, out: bool, x: f32) -> f64 {
    let w = g.width().max(1.0);
    let k = if out { (x - g.min.x) / w } else { (g.max.x - x) / w };
    let v = f64::from(k) * 100.0;
    if v.is_finite() { v.clamp(0.1, 100.0) } else { 0.1 }
}

/// The speed curve's points in graph area `g` (samples 0..1 of the fastest, bottom to top).
pub(crate) fn curve_points(g: Rect, speed: &[f64]) -> Vec<Pos2> {
    let n = speed.len();
    if n < 2 {
        return vec![];
    }
    speed
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let x = g.min.x + g.width() * i as f32 / (n - 1) as f32;
            let s = if s.is_finite() { s.clamp(0.0, 1.0) as f32 } else { 0.0 };
            pos2(x, g.max.y - g.height() * 0.9 * s)
        })
        .collect()
}

/// Comp-pixel quad `q` on the picture.
fn on_screen(map: &effectcraft_ui_egui::panels::viewer::ViewerMap, q: &[[f64; 2]]) -> Vec<Pos2> {
    q.iter().map(|p| map.to_screen(*p)).filter(|p| p.x.is_finite() && p.y.is_finite()).collect()
}

/// Whether `p` is inside the convex polygon `q`.
pub(crate) fn in_quad(p: Pos2, q: &[Pos2]) -> bool {
    if q.len() < 3 {
        return false;
    }
    let mut sign = 0.0_f32;
    for i in 0..q.len() {
        let (Some(a), Some(b)) = (q.get(i), q.get((i + 1) % q.len())) else { return false };
        let c = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    true
}

/// The dot on the picture's edge for an object off the frame: its centre, clamped into `pic`.
pub(crate) fn edge_marker(pic: Rect, q: &[Pos2]) -> Option<Pos2> {
    if q.is_empty() || !pic.is_positive() {
        return None;
    }
    let n = q.len() as f32;
    let c = pos2(q.iter().map(|p| p.x).sum::<f32>() / n, q.iter().map(|p| p.y).sum::<f32>() / n);
    let inner = pic.shrink(4.0);
    (c.x.is_finite() && c.y.is_finite()).then(|| pos2(c.x.clamp(inner.min.x, inner.max.x), c.y.clamp(inner.min.y, inner.max.y)))
}

/// The part of a `w` × `h` frame object quad `q` covers, as a texture's uv rect (`None`: none).
pub(crate) fn crop_uv(q: &[[f64; 2]], w: u32, h: u32) -> Option<Rect> {
    if q.is_empty() || w == 0 || h == 0 {
        return None;
    }
    let (x0, x1) = q.iter().map(|p| p[0]).fold((f64::MAX, f64::MIN), |(a, b), x| (a.min(x), b.max(x)));
    let (y0, y1) = q.iter().map(|p| p[1]).fold((f64::MAX, f64::MIN), |(a, b), y| (a.min(y), b.max(y)));
    let (fw, fh) = (f64::from(w), f64::from(h));
    let u = |x: f64| (x / fw).clamp(0.0, 1.0) as f32;
    let v = |y: f64| (y / fh).clamp(0.0, 1.0) as f32;
    let r = Rect::from_min_max(pos2(u(x0), v(y0)), pos2(u(x1), v(y1)));
    (r.is_finite() && r.width() > 1e-4 && r.height() > 1e-4).then_some(r)
}

/// `uv` of a `w` × `h` frame fitted into `slot`, keeping its proportions.
fn fit_crop(slot: Rect, uv: Rect, w: u32, h: u32) -> Rect {
    let (cw, ch) = (uv.width() * w as f32, uv.height() * h as f32);
    if cw <= 0.0 || ch <= 0.0 {
        return slot;
    }
    let k = (slot.width() / cw).min(slot.height() / ch);
    Rect::from_center_size(slot.center(), vec2(cw * k, ch * k))
}

// ---------------------------------------------------------------- state

/// The shelf's state agents read and set (`effectcraft.nowGraphs`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct View {
    /// The shelf is showing (the card is paused).
    pub shown: bool,
    /// Tucked away with its ×, until the next pause.
    pub closed: bool,
    /// The graph card hovered (index in the listing).
    pub hover_card: Option<usize>,
    /// The object hovered on the picture.
    pub hover_object: Option<usize>,
}

/// A handle drag under way: graph, side, the drag's undo key.
#[derive(Clone, Debug, PartialEq)]
struct Drag {
    graph: usize,
    out: bool,
    merge: String,
    last: f64,
}

#[derive(Default)]
pub(crate) struct State {
    pub view: View,
    listing: Listing,
    /// What `listing` was made from: session revision, comp, time (ms).
    made_from: Option<(u64, u64, i64)>,
    drag: Option<Drag>,
    /// Drags so far (each its own undo key).
    gestures: u64,
    /// The card is playing (last frame).
    was_playing: bool,
    /// Where the shelf was drawn (screen), and its widgets this frame.
    pub shelf: Option<Rect>,
    pub widgets: Vec<(String, Rect)>,
}

/// The listing, made again when the session, its comp or its time changed.
fn refresh(st: &mut State, host: &mut EcHost) {
    let key = (host.session().revision, host.comp(), (host.time() * 1000.0).round() as i64);
    if st.made_from != Some(key) {
        st.listing = list(host);
        st.made_from = Some(key);
    }
}

// ---------------------------------------------------------------- one frame

/// One frame of the shelf and the outlines for the card the Timeline edits (`p`, taken out of
/// the app). Returns the screen rect it took (the canvas leaves the pointer there).
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context, p: &mut Panel) -> Option<Rect> {
    let mut st = std::mem::take(&mut app.effectcraft.nowgraphs);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frame_inner(app, ctx, p, &mut st))).unwrap_or_else(|_| {
        log::warn!("Now graphs: an internal error");
        None
    });
    st.shelf = r;
    app.effectcraft.nowgraphs = st;
    r
}

fn frame_inner(app: &mut FigApp, ctx: &egui::Context, p: &mut Panel, st: &mut State) -> Option<Rect> {
    st.widgets.clear();
    let ec = &app.session.effectcraft;
    let playing = ec.playing() && ec.node == Some(p.card);
    if playing || !p.open {
        // Playing hides it; the next pause shows it again.
        if playing && !st.was_playing {
            st.view.closed = false;
        }
        st.was_playing = playing;
        st.view.shown = false;
        st.drag = None;
        return None;
    }
    st.was_playing = false;
    let doc = app.session.doc().ok()?;
    let c = doc.node(p.card).and_then(|n| n.effectcraft.as_deref())?.clone();
    if !super::handles::shows_active_comp(p.host.session(), &c) {
        st.view.shown = false;
        return None;
    }
    let r = super::screen_rect(app, doc, p.card)?;
    let pic = super::fit(r, c.width, c.height);
    if pic.width() < MIN_CARD || pic.height() < MIN_CARD / 2.0 {
        st.view.shown = false;
        return None;
    }
    let canvas = app.canvas.rect;
    refresh(st, &mut p.host);
    let (cw, ch) = p.host.session().active_comp().map(|c| (c.width, c.height)).unwrap_or((c.width, c.height));
    let map = vh::map_for(pic, [cw, ch]);
    // The card's frame for the thumbnails.
    let tex = {
        let t = super::card_time(app, doc, p.card, &c, None);
        let f = super::frame_at(&c, t);
        app.effectcraft.texture_near(&c, f, super::cards::chosen_side(app, p.card, &c)).map(|t| t.id())
    };
    let hover = ctx.input(|i| i.pointer.hover_pos());
    let shelf = shelf_rect(r, pic);
    let on_shelf = hover.is_some_and(|h| shelf.contains(h)) && !st.view.closed;
    // The objects on the picture, and which one the pointer is over (the smallest).
    let quads: Vec<Option<Vec<Pos2>>> =
        st.listing.objects.iter().map(|o| o.quad.as_ref().zip(map.as_ref()).map(|(q, m)| on_screen(m, q)).filter(|q| q.len() == 4)).collect();
    st.view.hover_object = if st.view.closed || on_shelf {
        None
    } else {
        hover.filter(|h| pic.contains(*h)).and_then(|h| {
            quads
                .iter()
                .enumerate()
                .filter_map(|(i, q)| q.as_ref().filter(|q| in_quad(h, q)).map(|q| (i, area(q))))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        })
    };
    st.view.shown = true;
    if st.view.closed {
        return reopen_tab(ctx, st, shelf, canvas);
    }
    // The outlines (over the picture, under nothing of the shelf's).
    let lit = st.view.hover_card.and_then(|n| st.listing.graphs.get(n)).map(|g| g.object).or(st.view.hover_object);
    let painter = ctx
        .layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("nowgraphs_outlines")))
        .with_clip_rect(canvas.intersect(pic.expand(2.0)));
    for (i, q) in quads.iter().enumerate() {
        let o = st.listing.objects.get(i);
        let col = colour(i);
        let (w, col) = match lit {
            Some(k) if k == i => (OUTLINE_HOT, col),
            Some(_) => (OUTLINE, col.gamma_multiply(0.35)),
            None => (OUTLINE, col.gamma_multiply(0.85)),
        };
        match q {
            Some(q) if o.is_some_and(|o| !o.off_frame) => {
                painter.add(Shape::closed_line(q.clone(), Stroke::new(w, col)));
            }
            Some(q) => {
                if let Some(m) = edge_marker(pic, q) {
                    painter.circle_filled(m, if lit == Some(i) { 5.0 } else { 3.5 }, col);
                }
            }
            None => {}
        }
    }
    // The shelf.
    let clip = canvas.intersect(shelf.expand(1.0));
    if !clip.is_positive() {
        return None;
    }
    let mut actions: Vec<(String, Value)> = vec![];
    let mut close = false;
    let mut hover_card: Option<usize> = None;
    let mut grabbed = false;
    let listing = st.listing.clone();
    egui::Area::new(egui::Id::new("nowgraphs_shelf")).order(egui::Order::Middle).fixed_pos(shelf.min).constrain(false).movable(false).show(
        ctx,
        |ui| {
            ui.set_clip_rect(clip);
            let painter = ui.painter().clone();
            painter.rect_filled(shelf, CornerRadius::same(8), PANEL);
            painter.rect_stroke(shelf, CornerRadius::same(8), Stroke::new(1.0, EDGE), StrokeKind::Inside);
            st.widgets.push(("nowgraphs-shelf".into(), shelf));
            // Header: "Now graphs · n" and ×.
            let head = Rect::from_min_size(shelf.min, vec2(shelf.width(), HEAD_H));
            painter.text(
                head.left_center() + vec2(10.0, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("Now graphs · {}", listing.graphs.len()),
                FontId::proportional(11.5),
                FG,
            );
            let x = Rect::from_center_size(head.right_center() - vec2(14.0, 0.0), vec2(18.0, 18.0));
            let xr = ui
                .interact(x, egui::Id::new("nowgraphs-close"), Sense::click())
                .on_hover_text("Tuck the graphs away (they come back on the next pause)");
            painter.text(x.center(), egui::Align2::CENTER_CENTER, "×", FontId::proportional(14.0), if xr.hovered() { FG } else { MUTED });
            st.widgets.push(("nowgraphs-close".into(), x));
            close = xr.clicked();
            painter.hline(head.x_range(), head.max.y, Stroke::new(1.0, LINE));
            let body = Rect::from_min_max(pos2(shelf.min.x + 8.0, head.max.y + 6.0), shelf.max - vec2(8.0, 6.0));
            if listing.graphs.is_empty() {
                painter.text(
                    body.center_top() + vec2(0.0, 18.0),
                    egui::Align2::CENTER_CENTER,
                    "Nothing keyframed within 1 s of the playhead",
                    FontId::proportional(11.0),
                    MUTED,
                );
                return;
            }
            ui.scope_builder(egui::UiBuilder::new().max_rect(body), |ui| {
                egui::ScrollArea::vertical().id_salt("nowgraphs-scroll").auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 6.0;
                    for (n, g) in listing.graphs.iter().enumerate() {
                        let out = card(ui, ctx, &listing, n, g, CardCtx { tex, frame: (cw, ch), lit, drag: st.drag.as_ref() });
                        if out.hovered {
                            hover_card = Some(n);
                        }
                        st.widgets.extend(out.widgets);
                        if let Some(pick) = out.pick {
                            actions.push(("nowGraphs.applyEase".into(), ease_call(g, &pick)));
                        }
                        if let Some((side_out, start)) = out.grab {
                            grabbed = true;
                            if start {
                                st.gestures = st.gestures.saturating_add(1);
                                let base = g.ease.map(|e| if side_out { e.out.influence } else { e.inn.influence }).unwrap_or(0.0);
                                st.drag = Some(Drag { graph: n, out: side_out, merge: format!("nowgraphs-drag-{}", st.gestures), last: base });
                            }
                        }
                        if let (Some(v), Some(d)) = (out.dragged_to, st.drag.as_mut())
                            && d.graph == n
                            && (v - d.last).abs() > 0.05
                        {
                            d.last = v;
                            let (o, i) = if d.out { (Some(v), None) } else { (None, Some(v)) };
                            actions.push(("nowGraphs.setInfluence".into(), influence_call(g, o, i, &d.merge)));
                        }
                    }
                });
            });
        },
    );
    if !grabbed && !ctx.input(|i| i.pointer.any_down()) {
        st.drag = None;
    }
    st.view.hover_card = hover_card;
    if close {
        st.view.closed = true;
        st.drag = None;
    }
    // The widgets are the Timeline's too (agents find them in its registry).
    for (id, rect) in &st.widgets {
        p.host.auto_mut().add(id, *rect, "Now graphs");
    }
    if !actions.is_empty() {
        // EffectCraft edits: the Timeline has the keyboard (⌘Z undoes them there).
        p.focused = true;
        p.host.set_focus(effectcraft_ui_egui::dock::PanelKind::Timeline);
    }
    for (cmd, params) in actions {
        if let Err(e) = p.host.run_engine(ctx, &cmd, params) {
            app.toast(e);
        }
    }
    Some(shelf)
}

fn area(q: &[Pos2]) -> f32 {
    let n = q.len();
    (0..n)
        .map(|i| {
            let (a, b) = (q.get(i).copied().unwrap_or(Pos2::ZERO), q.get((i + 1) % n).copied().unwrap_or(Pos2::ZERO));
            a.x * b.y - b.x * a.y
        })
        .sum::<f32>()
        .abs()
        / 2.0
}

/// The small "Now graphs" tab where the tucked-away shelf docks: a click brings it back.
fn reopen_tab(ctx: &egui::Context, st: &mut State, shelf: Rect, canvas: Rect) -> Option<Rect> {
    let tab = Rect::from_min_size(shelf.min, vec2(96.0, 22.0));
    let clip = canvas.intersect(tab.expand(1.0));
    if !clip.is_positive() {
        return None;
    }
    let mut open = false;
    egui::Area::new(egui::Id::new("nowgraphs_tab")).order(egui::Order::Middle).fixed_pos(tab.min).constrain(false).movable(false).show(ctx, |ui| {
        ui.set_clip_rect(clip);
        let r = ui.interact(tab, egui::Id::new("nowgraphs-reopen"), Sense::click()).on_hover_text("Show the graphs under the playhead");
        let p = ui.painter();
        p.rect_filled(tab, CornerRadius::same(11), if r.hovered() { CARD } else { PANEL });
        p.rect_stroke(tab, CornerRadius::same(11), Stroke::new(1.0, EDGE), StrokeKind::Inside);
        p.text(tab.center(), egui::Align2::CENTER_CENTER, "Now graphs ▸", FontId::proportional(11.0), FG);
        open = r.clicked();
    });
    st.widgets.push(("nowgraphs-reopen".into(), tab));
    if open {
        st.view.closed = false;
    }
    Some(tab)
}

struct CardCtx<'a> {
    tex: Option<egui::TextureId>,
    frame: (u32, u32),
    lit: Option<usize>,
    drag: Option<&'a Drag>,
}

#[derive(Default)]
struct CardOut {
    hovered: bool,
    widgets: Vec<(String, Rect)>,
    /// A curve picked from the chip.
    pick: Option<String>,
    /// A handle pressed or held: (out side, the drag starts now).
    grab: Option<(bool, bool)>,
    /// The influence the held handle is dragged to.
    dragged_to: Option<f64>,
}

/// Graph card `n` (`g`) in the shelf.
fn card(ui: &mut egui::Ui, ctx: &egui::Context, l: &Listing, n: usize, g: &Graph, cc: CardCtx<'_>) -> CardOut {
    let mut out = CardOut::default();
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, CARD_H), Sense::hover());
    out.hovered = resp.hovered() || cc.drag.is_some_and(|d| d.graph == n);
    out.widgets.push((format!("nowgraphs-card-{n}"), rect));
    let col = colour(g.object);
    let lit = cc.lit == Some(g.object);
    let painter = ui.painter().clone();
    painter.rect_filled(rect, CornerRadius::same(6), CARD);
    painter.rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, if out.hovered || lit { HOT } else { LINE }), StrokeKind::Inside);
    // The object's stripe.
    let stripe = Rect::from_min_size(rect.min + vec2(0.0, 6.0), vec2(3.0, rect.height() - 12.0));
    painter.rect_filled(stripe, CornerRadius::same(1), col);
    // Top line: thumbnail, "Layer · Prop", the ease chip.
    let top = Rect::from_min_size(rect.min + vec2(9.0, 4.0), vec2(rect.width() - 15.0, 20.0));
    let thumb = Rect::from_min_size(pos2(top.min.x, top.center().y - THUMB.y / 2.0), THUMB);
    painter.rect_filled(thumb, CornerRadius::same(2), Color32::from_rgb(0x0c, 0x0c, 0x0e));
    let quad = l.objects.get(g.object).and_then(|o| o.quad.as_deref());
    match (cc.tex, quad.and_then(|q| crop_uv(q, cc.frame.0, cc.frame.1))) {
        (Some(tex), Some(uv)) => {
            painter.with_clip_rect(thumb.intersect(painter.clip_rect())).image(tex, fit_crop(thumb, uv, cc.frame.0, cc.frame.1), uv, Color32::WHITE);
        }
        _ => {
            painter.rect_filled(thumb.shrink(5.0), CornerRadius::same(1), col.gamma_multiply(0.6));
        }
    }
    painter.rect_stroke(thumb, CornerRadius::same(2), Stroke::new(1.0, col.gamma_multiply(0.7)), StrokeKind::Inside);
    let chip = Rect::from_min_max(pos2(top.max.x - 112.0, top.min.y + 1.0), pos2(top.max.x, top.max.y - 1.0));
    let label = Rect::from_min_max(pos2(thumb.max.x + 6.0, top.min.y), pos2(chip.min.x - 4.0, top.max.y));
    painter.with_clip_rect(label.intersect(painter.clip_rect())).text(
        label.left_center(),
        egui::Align2::LEFT_CENTER,
        format!("{} · {}", g.layer_name, g.property),
        FontId::proportional(11.5),
        FG,
    );
    // The ease chip ▾: his saved Graphs, then Linear.
    let chip_id = format!("nowgraphs-ease-{n}");
    let cr = ui.interact(chip, egui::Id::new(&chip_id), Sense::click()).on_hover_text("Put one of your saved Graphs on this segment");
    painter.rect_filled(chip, CornerRadius::same(4), if cr.hovered() { LINE } else { Color32::from_rgb(0x25, 0x25, 0x2a) });
    let name = ease_label(l, g);
    painter.with_clip_rect(chip.shrink2(vec2(6.0, 0.0)).intersect(painter.clip_rect())).text(
        chip.left_center() + vec2(7.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &name,
        FontId::proportional(10.5),
        FG,
    );
    painter.text(chip.right_center() - vec2(8.0, 0.0), egui::Align2::CENTER_CENTER, "▾", FontId::proportional(10.0), MUTED);
    out.widgets.push((chip_id, chip));
    egui::Popup::menu(&cr).show(|ui| {
        for c in &l.curves {
            let on = g.curve.as_deref() == Some(c.id.as_str()) || (c.id == "linear" && name == "Linear");
            let text = if on { format!("✓ {}", c.name) } else { c.name.clone() };
            if ui.button(text).clicked() {
                out.pick = Some(c.id.clone());
                ui.close();
            }
        }
    });
    // The speed graph and its two influence handles.
    let ga = Rect::from_min_max(pos2(rect.min.x + 12.0, top.max.y + 6.0), pos2(rect.max.x - 10.0, rect.max.y - 16.0));
    painter.hline(ga.x_range(), ga.max.y, Stroke::new(1.0, LINE));
    let pts = curve_points(ga, &g.speed);
    if pts.len() >= 2 {
        painter.add(Shape::line(pts.clone(), Stroke::new(1.5, col)));
    }
    if g.start <= l.time && l.time <= g.end && g.end > g.start {
        let x = ga.min.x + ga.width() * ((l.time - g.start) / (g.end - g.start)) as f32;
        painter.vline(x, ga.y_range(), Stroke::new(1.0, Color32::from_rgba_unmultiplied(0x2d, 0x8c, 0xff, 140)));
    }
    let ease = g.ease.unwrap_or_default();
    for side_out in [true, false] {
        let (inf, y) = if side_out { (ease.out.influence, pts.first().map(|p| p.y)) } else { (ease.inn.influence, pts.last().map(|p| p.y)) };
        let y = y.unwrap_or(ga.max.y);
        let key_x = if side_out { ga.min.x } else { ga.max.x };
        let shown = cc.drag.filter(|d| d.graph == n && d.out == side_out).map_or(inf, |d| d.last);
        let hx = handle_x(ga, side_out, shown);
        let id = format!("nowgraphs-{}-{n}", if side_out { "out" } else { "in" });
        let grab = Rect::from_center_size(pos2(hx, y), vec2(GRAB * 2.0, GRAB * 2.0));
        let hr = ui.interact(grab, egui::Id::new(&id), Sense::drag());
        let hot = hr.hovered() || hr.dragged();
        painter.line_segment([pos2(key_x, y), pos2(hx, y)], Stroke::new(1.0, KEY.gamma_multiply(0.8)));
        painter.circle_filled(pos2(key_x, y), 2.5, KEY);
        painter.circle_filled(pos2(hx, y), if hot { 4.5 } else { 3.5 }, if hot { FG } else { KEY });
        if hot {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if hr.drag_started() || hr.dragged() {
            out.grab = Some((side_out, hr.drag_started()));
            if let Some(pos) = hr.interact_pointer_pos() {
                out.dragged_to = Some(influence_at(ga, side_out, pos.x));
            }
        }
        out.widgets.push((id, grab));
    }
    // On hover: the path and the numbers, faint.
    if out.hovered {
        let line = format!("{}   out {:.1} · in {:.1} · {:.2}–{:.2} s", g.trail.join(" › "), ease.out.influence, ease.inn.influence, g.start, g.end);
        let r = Rect::from_min_max(pos2(rect.min.x + 9.0, rect.max.y - 14.0), pos2(rect.max.x - 6.0, rect.max.y - 2.0));
        painter.with_clip_rect(r.intersect(painter.clip_rect())).text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            line,
            FontId::proportional(9.5),
            MUTED.gamma_multiply(0.8),
        );
    }
    out
}

// ---------------------------------------------------------------- the control channel

/// `effectcraft.nowGraphs` (read the shelf), `.setInfluence {card, out?, in?}`, `.applyEase
/// {card, curve}` (edit card `card`'s segment, as the shelf does) and `.set {closed?,
/// hoverCard?, hoverObject?}`.
pub(crate) fn control(app: &mut FigApp, ctx: &egui::Context, method: &str, p: &Value) -> Result<Value, String> {
    match method {
        "effectcraft.nowGraphs" => Ok(inspect(app)),
        "effectcraft.nowGraphs.set" => {
            let v = &mut app.effectcraft.nowgraphs.view;
            if let Some(b) = p.get("closed").and_then(Value::as_bool) {
                v.closed = b;
            }
            if p.get("hoverCard").is_some() {
                v.hover_card = p.get("hoverCard").and_then(Value::as_u64).map(|n| n as usize);
            }
            if p.get("hoverObject").is_some() {
                v.hover_object = p.get("hoverObject").and_then(Value::as_u64).map(|n| n as usize);
            }
            ctx.request_repaint();
            Ok(inspect(app))
        }
        "effectcraft.nowGraphs.setInfluence" | "effectcraft.nowGraphs.applyEase" => {
            let n = p.get("card").and_then(Value::as_u64).map(|n| n as usize).ok_or("`card` (the graph card's index) required")?;
            let g = app
                .effectcraft
                .nowgraphs
                .listing
                .graphs
                .get(n)
                .cloned()
                .ok_or_else(|| format!("no graph card {n} (effectcraft.nowGraphs lists them)"))?;
            let (cmd, params) = if method.ends_with("applyEase") {
                let curve = p.get("curve").and_then(Value::as_str).ok_or("`curve` (an id from effectcraft.nowGraphs's curves) required")?;
                ("nowGraphs.applyEase", ease_call(&g, curve))
            } else {
                let st = &mut app.effectcraft.nowgraphs;
                st.gestures = st.gestures.saturating_add(1);
                let merge = format!("nowgraphs-agent-{}", st.gestures);
                ("nowGraphs.setInfluence", influence_call(&g, p.get("out").and_then(Value::as_f64), p.get("in").and_then(Value::as_f64), &merge))
            };
            let panel = app.effectcraft.panel.as_mut().filter(|x| x.open).ok_or("no open EffectCraft Timeline on the canvas")?;
            panel.focused = true;
            panel.host.set_focus(effectcraft_ui_egui::dock::PanelKind::Timeline);
            let r = panel.host.run_engine(ctx, cmd, params);
            ctx.request_repaint();
            r
        }
        other => Err(format!("unknown method `{other}`")),
    }
}

/// The shelf for agents: its view, the cards (index, label, path, colour, ease, rect) and objects.
pub(crate) fn inspect(app: &FigApp) -> Value {
    let st = &app.effectcraft.nowgraphs;
    let l = &st.listing;
    let rect = |r: &Rect| json!([r.min.x, r.min.y, r.width(), r.height()]);
    let widget = |id: &str| st.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| rect(r));
    let hex = |c: Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
    json!({
        "view": serde_json::to_value(&st.view).unwrap_or(Value::Null),
        "card": app.effectcraft.panel.as_ref().map(|p| json!(p.card)),
        "shelf": st.shelf.as_ref().map(rect),
        "comp": l.comp,
        "time": l.time,
        "cards": l.graphs.iter().enumerate().map(|(n, g)| json!({
            "card": n, "id": format!("nowgraphs-card-{n}"), "label": format!("{} · {}", g.layer_name, g.property),
            "trail": g.trail, "comp": g.comp, "layer": g.layer, "path": g.path, "key": g.key,
            "start": g.start, "end": g.end, "object": g.object, "colour": hex(colour(g.object)),
            "ease": ease_label(l, g), "curve": g.curve, "influence": g.ease.map(|e| json!({"out": e.out.influence, "in": e.inn.influence})),
            "rect": widget(&format!("nowgraphs-card-{n}")), "easeChip": widget(&format!("nowgraphs-ease-{n}")),
            "outHandle": widget(&format!("nowgraphs-out-{n}")), "inHandle": widget(&format!("nowgraphs-in-{n}")),
        })).collect::<Vec<_>>(),
        "objects": l.objects.iter().map(|o| json!({"object": o.object, "name": o.name, "colour": hex(colour(o.object)), "quad": o.quad, "offFrame": o.off_frame})).collect::<Vec<_>>(),
        "curves": l.curves,
        "widgets": st.widgets.iter().map(|(id, r)| json!({"id": id, "rect": rect(r)})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[path = "effectcraft_nowgraphs_tests.rs"]
mod tests;

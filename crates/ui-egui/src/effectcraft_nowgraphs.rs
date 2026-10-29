//! Fork-only (`effectcraft-inproc`): the "Now graphs" shelf (Elias, 29 Oct 2026, approved after
//! the mock `effectcraft/plan/pause-graphs-playground.html`: "go ahead ... should work on main";
//! fixed and polished the same day on his live use, NG2).
//!
//! - **When.** The card the Timeline edits is paused (Space or its pause button) and shows the
//!   Timeline's comp: the shelf docks to the right of the card, top-aligned with its picture.
//!   It is part of the canvas like the Timeline and Effect Controls ([`super::strips`]): sized in
//!   panel units, drawn in a layer egui transforms like the canvas, so it zooms and pans with its
//!   card, text and all. Below [`MIN_SCALE`] (unreadable) it isn't drawn at all. It is as tall as
//!   its graphs need, up to the card's picture plus its Timeline (when the Timeline doesn't reach
//!   under it); more graphs scroll (wheel or trackpad over the shelf scrolls it, never the
//!   canvas). Playing hides it; its × tucks it away until the next pause (a small "Now graphs ▸"
//!   tab brings it back).
//! - **What.** Every keyframed property whose segment touches the playhead ± 1 s, through every
//!   precomp the card's comp shows (EffectCraft's `nowGraphs.list`), those containing the playhead
//!   first, then the nearest: per property a header (thumbnail, "Layer · Prop", the ease chip ▾
//!   listing Elias's saved Graphs and Linear), the segment's speed graph with its two influence
//!   handles, and its two keys on a lane under the graph, draggable in time (snapped to the comp's
//!   frames) to lengthen or shorten the move. The path and the numbers are a faint line on hover.
//! - **Which is which.** Each card has a crop of its object from the card's current frame.
//!   Hovering a card outlines its object on the picture (1.5 px, its own colour, [`PALETTE`], no
//!   purple; an object off the frame gets a dot on the picture's edge); nothing is drawn on the
//!   picture otherwise. Hovering an object on the picture lights its cards.
//! - **Edits** are EffectCraft commands on the Timeline's session (`nowGraphs.setInfluence` and
//!   `nowGraphs.moveKey`, one undo step per drag; `nowGraphs.applyEase`, one per pick), so they
//!   mirror into the app, ⌘Z undoes them in EffectCraft and the card re-renders as for any
//!   Timeline edit.
//! - **Agents.** Widgets register automation ids (`nowgraphs-shelf`, `nowgraphs-card-<n>`,
//!   `nowgraphs-out-<n>`, `nowgraphs-in-<n>`, `nowgraphs-key0-<n>`, `nowgraphs-key1-<n>`,
//!   `nowgraphs-ease-<n>`; screen rects) in the Timeline's registry and in [`State::widgets`]; the
//!   control channel's `effectcraft.nowGraphs` reads the shelf (its serde [`View`], cards, objects,
//!   rects) and `effectcraft.nowGraphs.setInfluence` / `.applyEase` / `.moveKey` / `.set` drive it
//!   ([`control`]).

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use effectcraft_ui_egui::panels::viewer_hosted as vh;
use egui::{Color32, CornerRadius, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FigApp;
use crate::effectcraft_panel::{EcHost, Panel};

// Sizes are panel units (page units / the card's scale, as the Timeline's), unless noted.
/// The shelf's width and its gap from the card.
pub(crate) const SHELF_W: f32 = 300.0;
pub(crate) const GAP: f32 = 12.0;
/// Shortest shelf.
pub(crate) const MIN_SHELF_H: f32 = 140.0;
/// One graph card's height, the header's and the inner padding.
pub(crate) const CARD_H: f32 = 116.0;
pub(crate) const HEAD_H: f32 = 30.0;
const PAD: f32 = 12.0;
/// Below this many screen points per panel unit the shelf would be unreadable: not drawn.
pub(crate) const MIN_SCALE: f32 = 0.45;
/// The outline's width (screen points).
const OUTLINE: f32 = 1.5;
/// Half the grab zone of a handle or key (panel units; at least [`MIN_GRAB_PX`] screen points).
const GRAB: f32 = 7.0;
const MIN_GRAB_PX: f32 = 5.0;
/// The thumbnail's size.
const THUMB: egui::Vec2 = vec2(28.0, 16.0);
/// Corner radius of the shelf (as the Timeline's panels) and of a hovered card.
const RADIUS: u8 = 4;
const CARD_RADIUS: u8 = 6;
/// Space around a segment on its graph's time axis (share of its length), so its keys can move.
const AXIS_PAD: f64 = 0.25;

/// His blue (10-10's P01 blues), the one accent; greys for everything else.
const ACCENT: Color32 = Color32::from_rgb(0x46, 0x97, 0xf3);
const KEY_GREY: Color32 = Color32::from_rgb(0xb8, 0xb8, 0xc0);

/// The objects' colours, in the order objects are listed (no purple, no magenta). Only an
/// outline on the picture shows them, while its card is hovered.
pub(crate) const PALETTE: [Color32; 8] = [
    Color32::from_rgb(0x46, 0x97, 0xf3), // blue
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
    PALETTE.get(i % PALETTE.len()).copied().unwrap_or(ACCENT)
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
    /// The two keys' times (`key`, then `key + 1`) in the listed comp's seconds.
    pub key_times: Option<[f64; 2]>,
    /// The precomp layers from the listed comp down to `comp` (`[{comp, layer}]`).
    pub via: Vec<Value>,
    pub contains_playhead: bool,
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
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Listing {
    pub comp: u64,
    pub time: f64,
    pub frame_rate: f64,
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

    /// The graph of property (`comp`, `layer`, `prop`), wherever the listing put it now.
    pub(crate) fn find(&self, who: Who) -> Option<(usize, &Graph)> {
        self.graphs.iter().enumerate().find(|(_, g)| (g.comp, g.layer, g.prop) == who)
    }

    /// Seconds per frame of the listed comp (25 fps when not given).
    pub(crate) fn frame(&self) -> f64 {
        if self.frame_rate.is_finite() && self.frame_rate > 0.5 { 1.0 / self.frame_rate } else { 1.0 / 25.0 }
    }
}

/// A graph's property: comp, layer, prop uid (it keeps this while the listing reorders).
pub(crate) type Who = (u64, u64, u64);

impl Graph {
    pub(crate) fn who(&self) -> Who {
        (self.comp, self.layer, self.prop)
    }

    /// Its two keys' times in the listed comp's seconds (`key`, then `key + 1`).
    pub(crate) fn keys(&self) -> [f64; 2] {
        self.key_times.filter(|k| k.iter().all(|t| t.is_finite())).unwrap_or([self.start, self.end])
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

/// The `nowGraphs.moveKey` call that drags graph `g`'s key `which` (0: its first, 1: its second)
/// to `time` (seconds of the listed comp `top`; `merge`: the drag's key).
pub(crate) fn move_call(top: u64, g: &Graph, which: usize, time: f64, merge: &str) -> Value {
    let key = g.key.saturating_add(u64::from(which > 0));
    json!({"comp": g.comp, "layer": g.layer, "prop": g.prop, "key": key, "time": time, "top": top, "via": g.via, "merge": merge})
}

/// `t` on the nearest frame of a comp with `frame` seconds per frame.
pub(crate) fn snap(t: f64, frame: f64) -> f64 {
    if !t.is_finite() || !frame.is_finite() || frame <= 0.0 {
        return t;
    }
    (t / frame).round() * frame
}

/// The `nowGraphs.applyEase` call that puts curve `curve` on graph `g`.
pub(crate) fn ease_call(g: &Graph, curve: &str) -> Value {
    json!({"comp": g.comp, "layer": g.layer, "prop": g.prop, "key": g.key, "curve": curve})
}

// ---------------------------------------------------------------- geometry

/// Where the shelf docks (panel units): right of the card `card`, top-aligned with its picture
/// `pic`, as tall as `content` needs, at least [`MIN_SHELF_H`], at most down to the bottom of the
/// Timeline `strip` (or of the viewer bar `footer` / the picture, when the Timeline reaches under
/// the shelf). Never over the picture.
pub(crate) fn shelf_rect(card: Rect, pic: Rect, strip: Option<Rect>, footer: Option<Rect>, content: f32) -> Rect {
    let x0 = card.max.x.max(pic.max.x) + GAP;
    let reaches_under = strip.is_some_and(|s| s.max.x > x0 - GAP / 2.0);
    let bottom = match (strip, footer) {
        (Some(s), _) if !reaches_under => s.max.y,
        (_, Some(f)) => f.max.y.max(pic.max.y),
        _ => pic.max.y,
    };
    let most = (bottom - pic.min.y).max(MIN_SHELF_H);
    let want = if content.is_finite() { content } else { MIN_SHELF_H };
    Rect::from_min_size(pos2(x0, pic.min.y), vec2(SHELF_W, want.clamp(MIN_SHELF_H, most)))
}

/// The height the shelf's content needs for `n` graphs.
pub(crate) fn content_height(n: usize) -> f32 {
    HEAD_H + (n.max(1) as f32) * CARD_H + 4.0
}

/// The scroll offset kept in range for content `content` tall in a viewport `view` tall.
pub(crate) fn clamp_scroll(scroll: f32, content: f32, view: f32) -> f32 {
    let most = (content - view).max(0.0);
    if scroll.is_finite() { scroll.clamp(0.0, most) } else { 0.0 }
}

/// A graph's time axis: its segment `[a, b]` and room either side, so its keys can move.
pub(crate) fn axis(a: f64, b: f64, frame: f64) -> (f64, f64) {
    let (lo, hi) = (a.min(b), a.max(b));
    let pad = ((hi - lo) * AXIS_PAD).max(frame * 2.0).max(1e-3);
    if lo.is_finite() && hi.is_finite() { (lo - pad, hi + pad) } else { (0.0, 1.0) }
}

/// Time `t` on axis `ax` as an x in graph area `g`, and back.
pub(crate) fn time_x(g: Rect, ax: (f64, f64), t: f64) -> f32 {
    let span = ax.1 - ax.0;
    let k = if span.abs() > 1e-9 && t.is_finite() { ((t - ax.0) / span) as f32 } else { 0.5 };
    g.min.x + g.width() * k.clamp(-1.0, 2.0)
}

pub(crate) fn x_time(g: Rect, ax: (f64, f64), x: f32) -> f64 {
    let w = g.width().max(1.0);
    let k = f64::from(((x - g.min.x) / w).clamp(-1.0, 2.0));
    let t = ax.0 + (ax.1 - ax.0) * k;
    if t.is_finite() { t } else { ax.0 }
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
    /// The shelf is showing (the card is paused and big enough on screen to read it).
    pub shown: bool,
    /// Tucked away with its ×, until the next pause.
    pub closed: bool,
    /// The graph card hovered (index in the listing).
    pub hover_card: Option<usize>,
    /// The object hovered on the picture.
    pub hover_object: Option<usize>,
    /// How far the cards are scrolled (panel units).
    pub scroll: f32,
}

/// What a drag holds: an influence handle (out side or in side) or a key (0: the segment's first,
/// 1: its second).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Grip {
    Influence { out: bool },
    Key { which: usize },
}

/// A drag under way: the property (it keeps it while the listing reorders), what it holds, the
/// drag's undo key, the last value sent (influence %, or the key's time in the listed comp's
/// seconds) and, for a key, the graph's time axis when it began (it doesn't rescale mid-drag).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Drag {
    pub who: Who,
    pub grip: Grip,
    pub merge: String,
    pub last: f64,
    pub axis: (f64, f64),
}

#[derive(Default)]
pub(crate) struct State {
    pub view: View,
    pub(crate) listing: Listing,
    /// What `listing` was made from: session revision, comp, time (ms).
    made_from: Option<(u64, u64, i64)>,
    drag: Option<Drag>,
    /// Drags so far (each its own undo key).
    gestures: u64,
    /// The card is playing (last frame).
    was_playing: bool,
    /// Where the shelf was drawn (screen), and its widgets this frame (screen).
    pub shelf: Option<Rect>,
    pub widgets: Vec<(String, Rect)>,
}

/// The listing, made again when the session, its comp or its time changed (a new comp or time
/// scrolls back to the top; an edit keeps the scroll).
fn refresh(st: &mut State, host: &mut EcHost) {
    let key = (host.session().revision, host.comp(), (host.time() * 1000.0).round() as i64);
    if st.made_from != Some(key) {
        if st.made_from.is_none_or(|m| (m.1, m.2) != (key.1, key.2)) {
            st.view.scroll = 0.0;
        }
        st.listing = list(host);
        st.made_from = Some(key);
    }
}

// ---------------------------------------------------------------- one frame

/// One frame of the shelf and the outline for the card the Timeline edits (`p`, taken out of
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

/// Where the shelf sits this frame.
struct Place {
    /// Panel units to screen points (the canvas's zoom and pan, the card's scale).
    xf: egui::emath::TSTransform,
    /// The shelf (panel units).
    shelf: Rect,
}

fn hide(st: &mut State) -> Option<Rect> {
    st.view.shown = false;
    st.view.hover_card = None;
    st.view.hover_object = None;
    None
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
        st.drag = None;
        return hide(st);
    }
    st.was_playing = false;
    let doc = app.session.doc().ok()?;
    let c = doc.node(p.card).and_then(|n| n.effectcraft.as_deref())?.clone();
    if !super::handles::shows_active_comp(p.host.session(), &c) {
        return hide(st);
    }
    let r = super::screen_rect(app, doc, p.card)?;
    let pic = super::fit(r, c.width, c.height);
    // In canvas space, like the Timeline: the card's page geometry in panel units.
    let Some(g) = super::strips::geom_of(doc, p.card, true, 0) else { return hide(st) };
    let Some(xf) = super::strips::layer_transform(app, g.k) else { return hide(st) };
    if !(xf.scaling.is_finite() && xf.scaling >= MIN_SCALE) {
        // Too small to read: nothing at all (no oversized shelf over the other cards).
        st.drag = None;
        return hide(st);
    }
    refresh(st, &mut p.host);
    let card_p = super::strips::to_panel(g.card, g.k);
    let (cw, ch) = p.host.session().active_comp().map(|c| (c.width, c.height)).unwrap_or((c.width, c.height));
    let pic_p = super::fit(card_p, c.width, c.height);
    let shelf = shelf_rect(
        card_p,
        pic_p,
        Some(super::strips::to_panel(g.strip, g.k)),
        g.footer.map(|f| super::strips::to_panel(f, g.k)),
        content_height(st.listing.graphs.len()),
    );
    let place = Place { xf, shelf };
    let canvas = app.canvas.rect;
    let map = vh::map_for(pic, [cw, ch]);
    // The card's frame for the thumbnails.
    let tex = {
        let t = super::card_time(app, doc, p.card, &c, None);
        let f = super::frame_at(&c, t);
        app.effectcraft.texture_near(&c, f, super::cards::chosen_side(app, p.card, &c)).map(|t| t.id())
    };
    let hover = ctx.input(|i| i.pointer.hover_pos());
    let shelf_s = xf * shelf;
    let on_shelf = hover.is_some_and(|h| shelf_s.contains(h)) && !st.view.closed;
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
    let zooming = ctx.input(|i| (i.zoom_delta() - 1.0).abs() > 1e-4);
    let interactable = !app.canvas.space_pan && !zooming;
    if st.view.closed {
        return reopen_tab(ctx, st, &place, canvas, interactable);
    }
    // The wheel over the shelf scrolls its cards (the canvas leaves it the pointer there).
    let view_h = (shelf.height() - HEAD_H).max(0.0);
    let content = content_height(st.listing.graphs.len()) - HEAD_H;
    if on_shelf && interactable {
        let dy = ctx.input(|i| i.smooth_scroll_delta.y);
        if dy.abs() > 1e-3 {
            st.view.scroll -= dy / xf.scaling;
            ctx.input_mut(|i| {
                i.smooth_scroll_delta = egui::Vec2::ZERO;
            });
        }
    }
    st.view.scroll = clamp_scroll(st.view.scroll, content, view_h);
    // The outline: only the hovered card's object (or the dragged one's), in its colour.
    let lit_card = st.drag.as_ref().and_then(|d| st.listing.find(d.who)).map(|(n, _)| n).or(st.view.hover_card);
    let outlined = lit_card.and_then(|n| st.listing.graphs.get(n)).map(|g| g.object);
    if let Some(i) = outlined {
        let painter = ctx
            .layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("nowgraphs_outlines")))
            .with_clip_rect(canvas.intersect(pic.expand(2.0)));
        let col = colour(i);
        match (quads.get(i).and_then(Option::as_ref), st.listing.objects.get(i)) {
            (Some(q), Some(o)) if !o.off_frame => {
                painter.add(Shape::closed_line(q.clone(), Stroke::new(OUTLINE, col)));
            }
            (Some(q), _) => {
                if let Some(m) = edge_marker(pic, q) {
                    painter.circle_filled(m, 4.0, col);
                }
            }
            _ => {}
        }
    }
    // The shelf, in a layer egui scales and moves like the canvas.
    let clip_s = canvas.intersect(shelf_s.expand(2.0));
    if !clip_s.is_positive() {
        return None;
    }
    let layer_id = egui::Id::new("nowgraphs_shelf");
    ctx.set_transform_layer(egui::LayerId::new(egui::Order::Middle, layer_id), xf);
    let inv = xf.inverse();
    let clip = Rect::from_two_pos(inv * clip_s.min, inv * clip_s.max);
    let t = crate::theme::Tokens::get(ctx);
    let mut actions: Vec<(String, Value)> = vec![];
    let mut close = false;
    let mut hover_card: Option<usize> = None;
    let mut held = false;
    let listing = st.listing.clone();
    let px = 1.0 / xf.scaling;
    let grab = GRAB.max(MIN_GRAB_PX * px);
    let mut widgets: Vec<(String, Rect)> = vec![];
    egui::Area::new(layer_id).order(egui::Order::Middle).fixed_pos(shelf.min).constrain(false).movable(false).interactable(interactable).show(
        ctx,
        |ui| {
            ui.set_clip_rect(clip);
            let painter = ui.painter().clone();
            // Swallow clicks on the shelf's empty parts (never the canvas under it).
            let _ = ui.interact(shelf, egui::Id::new("nowgraphs-shelf"), Sense::click_and_drag());
            painter.rect_filled(shelf, CornerRadius::same(RADIUS), t.panel);
            painter.rect_stroke(shelf, CornerRadius::same(RADIUS), Stroke::new(px, t.border), StrokeKind::Outside);
            widgets.push(("nowgraphs-shelf".into(), shelf));
            // Header: "Now graphs · n", quiet, and ×.
            let head = Rect::from_min_size(shelf.min, vec2(shelf.width(), HEAD_H));
            painter.text(
                head.left_center() + vec2(PAD, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("Now graphs · {}", listing.graphs.len()),
                FontId::proportional(11.0),
                t.text_secondary,
            );
            let x = Rect::from_center_size(head.right_center() - vec2(PAD + 4.0, 0.0), vec2(18.0, 18.0));
            let xr = ui
                .interact(x, egui::Id::new("nowgraphs-close"), Sense::click())
                .on_hover_text("Tuck the graphs away (they come back on the next pause)");
            if xr.hovered() {
                painter.rect_filled(x, CornerRadius::same(4), t.hover);
            }
            painter.text(
                x.center(),
                egui::Align2::CENTER_CENTER,
                "×",
                FontId::proportional(13.0),
                if xr.hovered() { t.text } else { t.text_secondary },
            );
            widgets.push(("nowgraphs-close".into(), x));
            close = xr.clicked();
            painter.hline(head.x_range(), head.max.y, Stroke::new(px, t.border.gamma_multiply(0.7)));
            let body = Rect::from_min_max(pos2(shelf.min.x, head.max.y), shelf.max);
            if listing.graphs.is_empty() {
                painter.text(
                    body.center_top() + vec2(0.0, 22.0),
                    egui::Align2::CENTER_CENTER,
                    "Nothing keyframed within 1 s of the playhead",
                    FontId::proportional(11.0),
                    t.text_secondary,
                );
                return;
            }
            let body_clip = body.intersect(clip);
            let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(body));
            cui.set_clip_rect(body_clip);
            let n_all = listing.graphs.len();
            for (n, g) in listing.graphs.iter().enumerate() {
                let rect = Rect::from_min_size(pos2(body.min.x, body.min.y - st.view.scroll + n as f32 * CARD_H), vec2(body.width(), CARD_H));
                if !rect.intersects(body_clip) {
                    continue;
                }
                let cc = CardCtx {
                    rect,
                    clip: body_clip,
                    tex,
                    frame: (cw, ch),
                    lit: st.view.hover_object == Some(g.object),
                    drag: st.drag.as_ref().filter(|d| d.who == g.who()),
                    last: n + 1 == n_all,
                    px,
                    grab,
                    t: &t,
                };
                let out = card(&mut cui, ctx, &listing, n, g, cc);
                if out.hovered {
                    hover_card = Some(n);
                }
                widgets.extend(out.widgets);
                if let Some(pick) = out.pick {
                    actions.push(("nowGraphs.applyEase".into(), ease_call(g, &pick)));
                }
                if let Some((grip, starts)) = out.grab {
                    held = true;
                    if starts {
                        st.gestures = st.gestures.saturating_add(1);
                        let (last, ax) = match grip {
                            Grip::Influence { out } => {
                                (g.ease.map(|e| if out { e.out.influence } else { e.inn.influence }).unwrap_or(0.0), (0.0, 1.0))
                            }
                            Grip::Key { which } => {
                                let k = g.keys();
                                (k.get(which).copied().unwrap_or(g.start), axis(g.start, g.end, listing.frame()))
                            }
                        };
                        st.drag = Some(Drag { who: g.who(), grip, merge: format!("nowgraphs-drag-{}", st.gestures), last, axis: ax });
                    }
                }
                if let (Some(v), Some(d)) = (out.dragged_to, st.drag.as_mut())
                    && d.who == g.who()
                {
                    match d.grip {
                        Grip::Influence { out } if (v - d.last).abs() > 0.05 => {
                            d.last = v;
                            let (o, i) = if out { (Some(v), None) } else { (None, Some(v)) };
                            actions.push(("nowGraphs.setInfluence".into(), influence_call(g, o, i, &d.merge)));
                        }
                        Grip::Key { which } if (v - d.last).abs() > listing.frame() * 0.25 => {
                            d.last = v;
                            actions.push(("nowGraphs.moveKey".into(), move_call(listing.comp, g, which, v, &d.merge)));
                        }
                        _ => {}
                    }
                }
            }
            // A quiet scroll bar when there is more than fits.
            if content > view_h + 0.5 && view_h > 0.0 {
                let track = Rect::from_min_max(pos2(body.max.x - 5.0, body.min.y + 4.0), pos2(body.max.x - 2.0, body.max.y - 4.0));
                let k = (view_h / content).clamp(0.08, 1.0);
                let h = track.height() * k;
                let y = track.min.y + (track.height() - h) * (st.view.scroll / (content - view_h)).clamp(0.0, 1.0);
                let thumb = Rect::from_min_size(pos2(track.min.x, y), vec2(track.width(), h));
                painter.rect_filled(thumb, CornerRadius::same(2), t.text_secondary.gamma_multiply(if on_shelf { 0.45 } else { 0.25 }));
            }
        },
    );
    if !held && !ctx.input(|i| i.pointer.any_down()) {
        st.drag = None;
    }
    st.view.hover_card = hover_card;
    if close {
        st.view.closed = true;
        st.drag = None;
    }
    // Agents get screen rects; the widgets are the Timeline's too (agents find them in its
    // registry).
    st.widgets = widgets.into_iter().map(|(id, r)| (id, xf * r)).collect();
    for (id, rect) in &st.widgets {
        if rect.intersects(canvas) {
            p.host.auto_mut().add(id, *rect, "Now graphs");
        }
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
    Some(shelf_s)
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

/// The small "Now graphs ▸" tab where the tucked-away shelf docks (canvas space too): a click
/// brings it back.
fn reopen_tab(ctx: &egui::Context, st: &mut State, place: &Place, canvas: Rect, interactable: bool) -> Option<Rect> {
    let tab = Rect::from_min_size(place.shelf.min, vec2(104.0, 24.0));
    let tab_s = place.xf * tab;
    let clip_s = canvas.intersect(tab_s.expand(2.0));
    if !clip_s.is_positive() {
        return None;
    }
    let id = egui::Id::new("nowgraphs_tab");
    ctx.set_transform_layer(egui::LayerId::new(egui::Order::Middle, id), place.xf);
    let inv = place.xf.inverse();
    let clip = Rect::from_two_pos(inv * clip_s.min, inv * clip_s.max);
    let t = crate::theme::Tokens::get(ctx);
    let px = 1.0 / place.xf.scaling;
    let mut open = false;
    egui::Area::new(id).order(egui::Order::Middle).fixed_pos(tab.min).constrain(false).movable(false).interactable(interactable).show(ctx, |ui| {
        ui.set_clip_rect(clip);
        let r = ui.interact(tab, egui::Id::new("nowgraphs-reopen"), Sense::click()).on_hover_text("Show the graphs under the playhead");
        let p = ui.painter();
        p.rect_filled(tab, CornerRadius::same(12), if r.hovered() { t.hover } else { t.panel });
        p.rect_stroke(tab, CornerRadius::same(12), Stroke::new(px, t.border), StrokeKind::Outside);
        p.text(
            tab.center(),
            egui::Align2::CENTER_CENTER,
            "Now graphs ▸",
            FontId::proportional(11.0),
            if r.hovered() { t.text } else { t.text_secondary },
        );
        open = r.clicked();
    });
    st.widgets.push(("nowgraphs-reopen".into(), tab_s));
    if open {
        st.view.closed = false;
    }
    Some(tab_s)
}

struct CardCtx<'a> {
    /// The card (panel units) and what of it shows.
    rect: Rect,
    clip: Rect,
    tex: Option<egui::TextureId>,
    frame: (u32, u32),
    /// Its object is hovered on the picture.
    lit: bool,
    /// The drag on this card's property, if any.
    drag: Option<&'a Drag>,
    /// The last card (no separator under it).
    last: bool,
    /// One screen point, and half a grab zone (panel units).
    px: f32,
    grab: f32,
    t: &'a crate::theme::Tokens,
}

#[derive(Default)]
struct CardOut {
    hovered: bool,
    widgets: Vec<(String, Rect)>,
    /// A curve picked from the chip.
    pick: Option<String>,
    /// A handle or key pressed or held: what, and whether the drag starts now.
    grab: Option<(Grip, bool)>,
    /// Where the held grip is dragged to: an influence (%), or a key's time (listed comp seconds,
    /// on a frame).
    dragged_to: Option<f64>,
}

/// Graph card `n` (`g`) in the shelf: header (thumbnail, "Layer · Prop", ease chip), the speed
/// graph with its influence handles, the key lane under it, the faint path line on hover.
fn card(ui: &mut egui::Ui, ctx: &egui::Context, l: &Listing, n: usize, g: &Graph, cc: CardCtx<'_>) -> CardOut {
    let mut out = CardOut::default();
    let t = cc.t;
    let rect = cc.rect;
    out.hovered = ui.rect_contains_pointer(rect.intersect(cc.clip)) || cc.drag.is_some();
    out.widgets.push((format!("nowgraphs-card-{n}"), rect));
    let painter = ui.painter().with_clip_rect(cc.clip);
    if out.hovered || cc.lit {
        painter.rect_filled(
            rect.shrink2(vec2(4.0, 2.0)),
            CornerRadius::same(CARD_RADIUS),
            t.hover.gamma_multiply(if out.hovered { 0.9 } else { 0.6 }),
        );
    }
    if !cc.last {
        painter.hline((rect.min.x + PAD)..=(rect.max.x - PAD), rect.max.y, Stroke::new(cc.px, t.border.gamma_multiply(0.55)));
    }
    let inner = rect.shrink2(vec2(PAD, 0.0));
    // Header: thumbnail, "Layer · Prop", the ease chip.
    let top = Rect::from_min_size(pos2(inner.min.x, rect.min.y + 10.0), vec2(inner.width(), 18.0));
    let thumb = Rect::from_min_size(pos2(top.min.x, top.center().y - THUMB.y / 2.0), THUMB);
    painter.rect_filled(thumb, CornerRadius::same(3), Color32::from_rgb(0x0e, 0x0e, 0x10));
    let quad = l.objects.get(g.object).and_then(|o| o.quad.as_deref());
    match (cc.tex, quad.and_then(|q| crop_uv(q, cc.frame.0, cc.frame.1))) {
        (Some(tex), Some(uv)) => {
            painter.with_clip_rect(thumb.intersect(cc.clip)).image(tex, fit_crop(thumb, uv, cc.frame.0, cc.frame.1), uv, Color32::WHITE);
        }
        _ => {
            painter.circle_filled(thumb.center(), 3.0, colour(g.object).gamma_multiply(0.8));
        }
    }
    // The ease chip ▾: his saved Graphs, then Linear.
    let name = ease_label(l, g);
    let chip_font = FontId::proportional(10.5);
    let text_w = ui.fonts_mut(|f| f.layout_no_wrap(name.clone(), chip_font.clone(), t.text).size().x);
    let chip_w = (text_w + 28.0).clamp(64.0, 132.0);
    let chip = Rect::from_min_max(pos2(top.max.x - chip_w, top.min.y), top.max);
    let chip_id = format!("nowgraphs-ease-{n}");
    let cr =
        ui.interact(chip, egui::Id::new(("nowgraphs-ease", g.who())), Sense::click()).on_hover_text("Put one of your saved Graphs on this segment");
    painter.rect_filled(chip, CornerRadius::same(9), if cr.hovered() { t.border } else { t.field });
    painter.with_clip_rect(chip.shrink2(vec2(9.0, 0.0)).intersect(cc.clip)).text(
        chip.left_center() + vec2(10.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &name,
        chip_font,
        t.text,
    );
    painter.text(chip.right_center() - vec2(10.0, 0.0), egui::Align2::CENTER_CENTER, "▾", FontId::proportional(9.5), t.text_secondary);
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
    // "Layer · Prop": the layer in the text colour, the property quieter.
    let label = Rect::from_min_max(pos2(thumb.max.x + 8.0, top.min.y), pos2(chip.min.x - 8.0, top.max.y));
    let lp = painter.with_clip_rect(label.intersect(cc.clip));
    let font = FontId::proportional(11.5);
    let lw = ui.fonts_mut(|f| f.layout_no_wrap(g.layer_name.clone(), font.clone(), t.text).size().x);
    lp.text(label.left_center(), egui::Align2::LEFT_CENTER, &g.layer_name, font.clone(), t.text);
    lp.text(label.left_center() + vec2(lw, 0.0), egui::Align2::LEFT_CENTER, format!("  ·  {}", g.property), font, t.text_secondary);
    // The time axis: the segment and room either side (held still while a key is dragged).
    let frame = l.frame();
    let ax = cc.drag.filter(|d| matches!(d.grip, Grip::Key { .. })).map_or_else(|| axis(g.start, g.end, frame), |d| d.axis);
    let mut keys = g.keys();
    if let Some(Drag { grip: Grip::Key { which }, last, .. }) = cc.drag
        && let Some(k) = keys.get_mut(*which)
    {
        *k = *last;
    }
    let ga = Rect::from_min_max(pos2(inner.min.x, top.max.y + 10.0), pos2(inner.max.x, top.max.y + 52.0));
    let (x0, x1) = (time_x(ga, ax, keys[0].min(keys[1])), time_x(ga, ax, keys[0].max(keys[1])));
    let seg = Rect::from_min_max(pos2(x0, ga.min.y), pos2(x1.max(x0 + 1.0), ga.max.y));
    // Baseline, the keys' guides, the playhead.
    painter.hline(ga.x_range(), ga.max.y, Stroke::new(cc.px, t.border));
    for x in [seg.min.x, seg.max.x] {
        painter.vline(x, ga.y_range(), Stroke::new(cc.px, t.border.gamma_multiply(0.5)));
    }
    if ax.0 <= l.time && l.time <= ax.1 {
        let x = time_x(ga, ax, l.time);
        painter.vline(x, (ga.min.y - 4.0)..=(ga.max.y + 14.0), Stroke::new(cc.px, ACCENT.gamma_multiply(0.55)));
    }
    // The speed curve: a crisp line with a faint fill under it.
    let pts = curve_points(seg, &g.speed);
    if pts.len() >= 2 {
        let mut mesh = egui::Mesh::default();
        let top_c = ACCENT.gamma_multiply(0.16);
        for (i, p) in pts.iter().enumerate() {
            mesh.colored_vertex(*p, top_c);
            mesh.colored_vertex(pos2(p.x, seg.max.y), Color32::TRANSPARENT);
            if i > 0 {
                let k = (i as u32) * 2;
                mesh.add_triangle(k - 2, k - 1, k);
                mesh.add_triangle(k - 1, k, k + 1);
            }
        }
        painter.add(Shape::mesh(mesh));
        painter.add(Shape::line(pts.clone(), Stroke::new(1.25 * cc.px, ACCENT)));
    }
    // The influence handles, from the curve's ends.
    let ease = g.ease.unwrap_or_default();
    let key_drag = cc.drag.is_some_and(|d| matches!(d.grip, Grip::Key { .. }));
    for side_out in [true, false] {
        let (inf, y) = if side_out { (ease.out.influence, pts.first().map(|p| p.y)) } else { (ease.inn.influence, pts.last().map(|p| p.y)) };
        let y = y.unwrap_or(seg.max.y);
        let key_x = if side_out { seg.min.x } else { seg.max.x };
        let shown = cc.drag.filter(|d| d.grip == Grip::Influence { out: side_out }).map_or(inf, |d| d.last);
        let hx = handle_x(seg, side_out, shown);
        let id = format!("nowgraphs-{}-{n}", if side_out { "out" } else { "in" });
        let zone = Rect::from_center_size(pos2(hx, y), vec2(cc.grab * 2.0, cc.grab * 2.0));
        // Ids follow the property, not the card's place: a drag survives the listing reordering.
        let hr = ui.interact(zone.intersect(cc.clip), egui::Id::new(("nowgraphs-influence", side_out, g.who())), Sense::drag());
        let hot = !key_drag && (hr.hovered() || hr.dragged());
        painter.line_segment([pos2(key_x, y), pos2(hx, y)], Stroke::new(cc.px, KEY_GREY.gamma_multiply(if hot { 0.9 } else { 0.5 })));
        painter.circle_filled(pos2(key_x, y), 2.0, KEY_GREY);
        if hot {
            painter.circle_filled(pos2(hx, y), 6.5, ACCENT.gamma_multiply(0.22));
        }
        painter.circle_filled(pos2(hx, y), if hot { 4.0 } else { 3.25 }, if hot { ACCENT } else { KEY_GREY });
        if hot {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if !key_drag && (hr.drag_started() || hr.dragged()) {
            out.grab = Some((Grip::Influence { out: side_out }, hr.drag_started()));
            if let Some(pos) = hr.interact_pointer_pos() {
                out.dragged_to = Some(influence_at(seg, side_out, pos.x));
            }
        }
        out.widgets.push((id, zone));
    }
    // The key lane: the segment's two keys, dragged in time on the comp's frames.
    let lane_y = ga.max.y + 10.0;
    let infl_drag = cc.drag.is_some_and(|d| matches!(d.grip, Grip::Influence { .. }));
    for (which, kt) in keys.iter().enumerate() {
        let x = time_x(ga, ax, *kt);
        let id = format!("nowgraphs-key{which}-{n}");
        let zone = Rect::from_center_size(pos2(x, lane_y), vec2(cc.grab * 2.0, cc.grab * 2.0));
        let kr = ui
            .interact(zone.intersect(cc.clip), egui::Id::new(("nowgraphs-key", which, g.who())), Sense::drag())
            .on_hover_text("Drag to retime this key (frames)");
        let held = cc.drag.is_some_and(|d| d.grip == Grip::Key { which });
        let hot = !infl_drag && (kr.hovered() || kr.dragged() || held);
        let r = if hot { 4.5 } else { 3.75 };
        let c = pos2(x, lane_y);
        let diamond = vec![c - vec2(0.0, r), c + vec2(r, 0.0), c + vec2(0.0, r), c - vec2(r, 0.0)];
        if hot {
            painter.circle_filled(c, 7.0, ACCENT.gamma_multiply(0.22));
        }
        painter.add(Shape::convex_polygon(diamond, if hot { ACCENT } else { KEY_GREY }, Stroke::NONE));
        if hot {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if !infl_drag && (kr.drag_started() || kr.dragged()) {
            out.grab = Some((Grip::Key { which }, kr.drag_started()));
            if let Some(pos) = kr.interact_pointer_pos() {
                out.dragged_to = Some(snap(x_time(ga, ax, pos.x), frame));
            }
        }
        out.widgets.push((id, zone));
    }
    // While a key is dragged: the move's new length, in frames.
    if key_drag {
        let frames = ((keys[1] - keys[0]).abs() / frame).round();
        painter.text(pos2(seg.center().x, lane_y), egui::Align2::CENTER_CENTER, format!("{frames:.0} f"), FontId::proportional(9.5), t.text);
    }
    // On hover: the path and the numbers, faint.
    if out.hovered && !key_drag {
        let line = format!(
            "{}   out {:.1} · in {:.1} · {:.0} f",
            g.trail.join(" › "),
            ease.out.influence,
            ease.inn.influence,
            ((keys[1] - keys[0]).abs() / frame).round()
        );
        let r = Rect::from_min_max(pos2(inner.min.x, rect.max.y - 18.0), pos2(inner.max.x, rect.max.y - 6.0));
        painter.with_clip_rect(r.intersect(cc.clip)).text(
            r.left_center(),
            egui::Align2::LEFT_CENTER,
            line,
            FontId::proportional(9.5),
            t.text_secondary.gamma_multiply(0.8),
        );
    }
    out
}

// ---------------------------------------------------------------- the control channel

/// `effectcraft.nowGraphs` (read the shelf), `.setInfluence {card, out?, in?}`, `.applyEase
/// {card, curve}`, `.moveKey {card, which (0: the segment's first key, 1: its second), time (the
/// listed comp's seconds)}` (edit card `card`'s segment, as the shelf does) and `.set {closed?,
/// hoverCard?, hoverObject?, scroll?}`.
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
            if let Some(y) = p.get("scroll").and_then(Value::as_f64).filter(|y| y.is_finite()) {
                v.scroll = y.max(0.0) as f32;
            }
            ctx.request_repaint();
            Ok(inspect(app))
        }
        "effectcraft.nowGraphs.setInfluence" | "effectcraft.nowGraphs.applyEase" | "effectcraft.nowGraphs.moveKey" => {
            let n = p.get("card").and_then(Value::as_u64).map(|n| n as usize).ok_or("`card` (the graph card's index) required")?;
            let g = app
                .effectcraft
                .nowgraphs
                .listing
                .graphs
                .get(n)
                .cloned()
                .ok_or_else(|| format!("no graph card {n} (effectcraft.nowGraphs lists them)"))?;
            let top = app.effectcraft.nowgraphs.listing.comp;
            let (cmd, params) = if method.ends_with("moveKey") {
                let which = p
                    .get("which")
                    .and_then(Value::as_u64)
                    .filter(|w| *w <= 1)
                    .ok_or("`which` (0: the segment's first key, 1: its second) required")?;
                let time = p.get("time").and_then(Value::as_f64).filter(|t| t.is_finite()).ok_or("`time` (seconds of the listed comp) required")?;
                let st = &mut app.effectcraft.nowgraphs;
                st.gestures = st.gestures.saturating_add(1);
                let merge = format!("nowgraphs-agent-{}", st.gestures);
                ("nowGraphs.moveKey", move_call(top, &g, which as usize, time, &merge))
            } else if method.ends_with("applyEase") {
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
        "frameRate": l.frame_rate,
        "cards": l.graphs.iter().enumerate().map(|(n, g)| json!({
            "card": n, "id": format!("nowgraphs-card-{n}"), "label": format!("{} · {}", g.layer_name, g.property),
            "trail": g.trail, "comp": g.comp, "layer": g.layer, "path": g.path, "key": g.key,
            "start": g.start, "end": g.end, "keyTimes": g.keys(), "containsPlayhead": g.contains_playhead,
            "object": g.object, "colour": hex(colour(g.object)),
            "ease": ease_label(l, g), "curve": g.curve, "influence": g.ease.map(|e| json!({"out": e.out.influence, "in": e.inn.influence})),
            "rect": widget(&format!("nowgraphs-card-{n}")), "easeChip": widget(&format!("nowgraphs-ease-{n}")),
            "outHandle": widget(&format!("nowgraphs-out-{n}")), "inHandle": widget(&format!("nowgraphs-in-{n}")),
            "firstKey": widget(&format!("nowgraphs-key0-{n}")), "secondKey": widget(&format!("nowgraphs-key1-{n}")),
        })).collect::<Vec<_>>(),
        "objects": l.objects.iter().map(|o| json!({"object": o.object, "name": o.name, "colour": hex(colour(o.object)), "quad": o.quad, "offFrame": o.off_frame})).collect::<Vec<_>>(),
        "curves": l.curves,
        "widgets": st.widgets.iter().map(|(id, r)| json!({"id": id, "rect": rect(r)})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
#[path = "effectcraft_nowgraphs_tests.rs"]
mod tests;

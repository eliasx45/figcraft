//! Fork-only (`effectcraft-inproc`): EffectCraft Timelines on the canvas, under their cards
//! (Elias's "option A", `plan/canvas-timeline-playground.src.html`).
//!
//! - **Where.** A card's Timeline hangs under it (a small gap), from the card's left edge. It is
//!   part of the canvas: everything is sized in page units and scales and moves with the canvas's
//!   zoom and pan, text and all; no minimum size on screen. Its height is 0.6× the card's longest
//!   side by default, its width its own: at least the card's width and 1.6× that default height (a
//!   9:16 card still gets a wide, horizontal Timeline), the card's longest side by default; both
//!   as dragged at its edges.
//! - **One open.** The card being edited ([`crate::effectcraft_panel::Panel`]) shows EffectCraft's
//!   real Timeline there, Effect Controls to its right; it is drawn in an egui area whose layer
//!   egui transforms like the canvas ([`layer_transform`]), so its input lands right too
//!   (EffectCraft's raw pointer reads go through its `layer_pointer::Lp`). Every other card shows a
//!   thin closed strip: a header (▸ comp · N layers) and a bar per layer over the comp's duration
//!   ([`Loader`] reads them from the saved project on a thread); a click opens that card's
//!   Timeline.
//! - **Bigger pushes.** Dragging the open Timeline's right edge widens it, its bottom edge makes it
//!   (and Effect Controls) taller, its bottom-right corner does both. What the bigger Timeline and
//!   its Effect Controls would cover moves away to keep a small gap: to the right of the
//!   card-and-Timeline column in its vertical band moves right, below it in its x-range moves
//!   down, and what they push pushes what is behind it. Real node moves, one undo step with the
//!   new size for the whole drag ([`resize`]); shrinking again leaves them where they went. The
//!   size is kept on the card (plugin data [`WIDTH_KEY`], [`HEIGHT_KEY`]); the height is at least
//!   the header, ruler and a few rows. Effect Controls' right edge widens them the same way (wide
//!   enough by default for names, values, checkbox labels and pop-ups; plugin data
//!   [`FX_WIDTH_KEY`]); their height follows the Timeline's. The edges are easy to grab: zones
//!   [`BAND`] screen points either side of each edge and [`CORNER`] round each bottom-right
//!   corner, at any zoom, win over the panels' content ([`edge_at`]); hovered or dragged, the
//!   edge shows its resize cursor and an accent line (a grip at a corner), and a drag stays
//!   captured until the button is let go.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, pos2, vec2};
use figcraft_doc::{Document, EcComp, NodeId};
use figcraft_geom::Rect as PRect;
use serde_json::json;

use crate::FigApp;

/// The card's plugin data holding its Timeline's width (page units).
pub(crate) const WIDTH_KEY: &str = "effectcraft.timelineWidth";
/// The card's plugin data holding its Timeline's height (page units).
pub(crate) const HEIGHT_KEY: &str = "effectcraft.timelineHeight";
/// The card's plugin data holding its Effect Controls' width (page units).
pub(crate) const FX_WIDTH_KEY: &str = "effectcraft.effectControlsWidth";
/// The card size everything below is designed for (its longest side, page units): a bigger card
/// gets a proportionally bigger Timeline.
const REF_SIDE: f64 = 480.0;
/// Gap between the card and its strip, and the strip and Effect Controls (page units at
/// [`REF_SIDE`]).
const GAP: f64 = 8.0;
/// The open Timeline's height, as a share of the card's longest side.
const HEIGHT_SHARE: f64 = 0.6;
/// The open Timeline is never narrower than this many times its height.
const MIN_ASPECT: f64 = 1.6;
/// Effect Controls' width by default, the least and the most (panel units: names, values,
/// checkbox labels and pop-ups fit at the default).
const FX_WIDTH: f64 = 360.0;
const FX_MIN: f64 = 200.0;
const FX_MAX: f64 = 2000.0;
/// The widest a Timeline gets (times the card's longest side).
const MAX_WIDTH: f64 = 12.0;
/// The shortest a Timeline gets (panel units: its header, the ruler and a few rows), and the
/// tallest (times the card's longest side).
const MIN_HEIGHT: f64 = 150.0;
const MAX_HEIGHT: f64 = 8.0;
/// The open Timeline's header (the panel's own units: page units at [`REF_SIDE`]).
pub(crate) const HEADER: f32 = 22.0;
/// The closed strip's header and layer bars (page units at [`REF_SIDE`]).
const CLOSED_HEADER: f64 = 16.0;
const BAR: f64 = 5.0;
/// Most layer bars a closed strip shows.
const MAX_BARS: usize = 24;
/// The resize zones (screen points, whatever the zoom): this far either side of an edge, and this
/// far either way from a bottom-right corner.
pub(crate) const BAND: f32 = 8.0;
pub(crate) const CORNER: f32 = 14.0;
/// Seconds between checks of the strips' project files (a save refreshes their bars).
const RECHECK: f64 = 2.0;

/// The card's scale: its longest side over [`REF_SIDE`]. One panel unit is this many page units.
pub(crate) fn card_scale(w: f64, h: f64) -> f64 {
    let k = w.max(h) / REF_SIDE;
    if k.is_finite() { k.clamp(0.05, 50.0) } else { 1.0 }
}

/// The open Timeline's page size for a card of page size `w`×`h` and the width and height asked
/// for (dragged at its edges).
pub(crate) fn open_size(w: f64, h: f64, asked: Option<f64>, asked_h: Option<f64>) -> (f64, f64) {
    let side = w.max(h).max(1.0);
    let least = least_width(w, h);
    let width = match asked.filter(|a| a.is_finite() && *a > 0.0) {
        Some(a) => a.clamp(least, least.max(side * MAX_WIDTH)),
        None => least.max(side),
    };
    let (lo, hi) = height_range(w, h);
    let height = match asked_h.filter(|a| a.is_finite() && *a > 0.0) {
        Some(a) => a.clamp(lo, hi),
        None => side * HEIGHT_SHARE,
    };
    (width, height)
}

/// The least width a card's Timeline may be dragged to (page units): the card's width, and 1.6×
/// its default height.
pub(crate) fn least_width(w: f64, h: f64) -> f64 {
    w.max(w.max(h).max(1.0) * HEIGHT_SHARE * MIN_ASPECT)
}

/// The heights a card's Timeline may be dragged to (page units).
pub(crate) fn height_range(w: f64, h: f64) -> (f64, f64) {
    let side = w.max(h).max(1.0);
    let lo = (MIN_HEIGHT * card_scale(w, h)).min(side * HEIGHT_SHARE);
    (lo, lo.max(side * MAX_HEIGHT))
}

/// Where a card's strip sits on the page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Geom {
    /// Page units per panel unit ([`card_scale`]).
    pub k: f64,
    pub card: PRect,
    /// The open Timeline (header included) or the closed strip.
    pub strip: PRect,
    /// Effect Controls, beside the open Timeline.
    pub fx: Option<PRect>,
}

impl Geom {
    /// Everything it covers on the page.
    pub(crate) fn whole(&self) -> PRect {
        self.fx.map_or(self.strip, |f| self.strip.union(f))
    }
}

/// The sizes dragged for a card's Timeline (page units; `None`: the default, or unchanged).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Size {
    pub width: Option<f64>,
    pub height: Option<f64>,
    /// Effect Controls' width.
    pub fx: Option<f64>,
}

/// Effect Controls' widths for a card of page size `w`×`h`: (least, default, most).
pub(crate) fn fx_range(w: f64, h: f64) -> (f64, f64, f64) {
    let k = card_scale(w, h);
    (FX_MIN * k, FX_WIDTH * k, FX_MAX * k)
}

/// Effect Controls' page width for the width asked for.
pub(crate) fn fx_width(w: f64, h: f64, asked: Option<f64>) -> f64 {
    let (lo, def, hi) = fx_range(w, h);
    asked.filter(|a| a.is_finite() && *a > 0.0).map_or(def, |a| a.clamp(lo, hi))
}

/// The strip of a card at page box `card`: the open Timeline (`open`) or a closed strip with
/// `bars` layer bars; `asked`, the sizes dragged (the closed strip takes the Timeline's width).
pub(crate) fn geom(card: PRect, asked: Size, open: bool, bars: usize) -> Geom {
    let (w, h) = (card.width(), card.height());
    let k = card_scale(w, h);
    let (tw, th) = open_size(w, h, asked.width, asked.height);
    let y0 = card.y1 + GAP * k;
    if open {
        let strip = PRect::new(card.x0, y0, card.x0 + tw, y0 + th);
        let fx0 = strip.x1 + GAP * k;
        let fx = PRect::new(fx0, y0, fx0 + fx_width(w, h, asked.fx), y0 + th);
        Geom { k, card, strip, fx: Some(fx) }
    } else {
        let sh = (CLOSED_HEADER + bars.min(MAX_BARS) as f64 * BAR + 2.0) * k;
        Geom { k, card, strip: PRect::new(card.x0, y0, card.x0 + tw, y0 + sh), fx: None }
    }
}

/// The width dragged for card `id` (its plugin data).
pub(crate) fn asked_width(doc: &Document, id: NodeId) -> Option<f64> {
    asked(doc, id, WIDTH_KEY)
}

/// The height dragged for card `id` (its plugin data).
pub(crate) fn asked_height(doc: &Document, id: NodeId) -> Option<f64> {
    asked(doc, id, HEIGHT_KEY)
}

/// Everything dragged for card `id`.
pub(crate) fn asked_size(doc: &Document, id: NodeId) -> Size {
    Size { width: asked_width(doc, id), height: asked_height(doc, id), fx: asked(doc, id, FX_WIDTH_KEY) }
}

fn asked(doc: &Document, id: NodeId, key: &str) -> Option<f64> {
    doc.node(id)?.plugin_data.get(key)?.parse::<f64>().ok().filter(|w| w.is_finite() && *w > 0.0)
}

/// Card `id`'s strip as it stands (`open`: its Timeline is open; `bars`: its closed strip's).
pub(crate) fn geom_of(doc: &Document, id: NodeId, open: bool, bars: usize) -> Option<Geom> {
    let b = doc.world_bounds(id)?;
    (b.width() > 0.0 && b.height() > 0.0 && b.x0.is_finite() && b.y0.is_finite()).then(|| geom(b, asked_size(doc, id), open, bars))
}

/// A page box on screen.
pub(crate) fn on_screen(app: &FigApp, r: PRect) -> Rect {
    Rect::from_two_pos(app.to_screen(r.x0, r.y0), app.to_screen(r.x1, r.y1))
}

/// The transform of the open Timeline's layer: panel units (page units / `k`) to screen points.
pub(crate) fn layer_transform(app: &FigApp, k: f64) -> Option<egui::emath::TSTransform> {
    let (a, b) = (app.to_screen(0.0, 0.0), app.to_screen(1.0, 0.0));
    let scale = (b.x - a.x) * k as f32;
    (scale.is_finite() && scale > 0.0 && a.x.is_finite() && a.y.is_finite()).then(|| egui::emath::TSTransform::new(a.to_vec2(), scale))
}

/// A page box in panel units.
pub(crate) fn to_panel(r: PRect, k: f64) -> Rect {
    Rect::from_min_max(pos2((r.x0 / k) as f32, (r.y0 / k) as f32), pos2((r.x1 / k) as f32, (r.y1 / k) as f32))
}

/// A comp's layers as bars: when each starts and ends (comp seconds) and its label colour; and
/// its work area (comp seconds), for the card's preview rules.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Bars {
    pub duration: f64,
    pub count: usize,
    pub bars: Vec<(f64, f64, Color32)>,
    pub work: (f64, f64),
}

type Preset = figcraft_ec_inproc::effectcraft_engine::preview::PreviewPreset;

/// FigCraft's preview rules for a card ([`figcraft_engine::effectcraft::Preview`]) from
/// EffectCraft's Space preview options `p` and the comp's work area and duration (comp seconds).
/// Applied: range (work area, extended by the current time, entire duration, around the current
/// time), play from, loop, frame rate and skip. Not: resolution, cache before playback, full
/// screen, audio/video only, moving the current time to the preview time.
pub(crate) fn preview_of(p: &Preset, work: (f64, f64), duration: f64) -> figcraft_engine::effectcraft::Preview {
    use figcraft_ec_inproc::effectcraft_engine::preview::{PlayFrom, PreviewRange as R};
    use figcraft_engine::effectcraft::{Preview, PreviewRange};
    let range = match p.range {
        R::WorkArea => PreviewRange::WorkArea,
        R::WorkAreaExtended => PreviewRange::WorkAreaExtended,
        R::EntireDuration => PreviewRange::Entire,
        R::AroundCurrentTime => PreviewRange::Around { pre: p.pre_roll, post: p.post_roll },
    };
    Preview { work, duration, range, looping: p.loop_, from_start: p.play_from == PlayFrom::RangeStart, fps: p.frame_rate, skip: p.skip }
}

type BarsKey = (PathBuf, String);
type Job = (BarsKey, PathBuf, EcComp);
type Done = (BarsKey, Option<Arc<Bars>>);

fn bars_key(c: &EcComp) -> Option<BarsKey> {
    let path = Path::new(&c.project);
    if c.project.is_empty() || !path.is_file() {
        return None;
    }
    let comp = if c.comp_id > 0 { format!("#{}", c.comp_id) } else { c.comp.clone() };
    Some((figcraft_ec_inproc::key(path), comp))
}

fn file_seconds(path: &Path) -> u64 {
    std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// The closed strips' layer bars, read from the cards' saved projects on a thread (one project
/// open at a time there), read again when a file changes.
#[derive(Default)]
pub(crate) struct Loader {
    got: HashMap<BarsKey, Option<Arc<Bars>>>,
    /// The file times the bars were read at.
    stamps: HashMap<PathBuf, u64>,
    asked: HashSet<BarsKey>,
    checked: f64,
    /// Elias's EffectCraft Space preview options (re-read every [`RECHECK`]).
    preset: Option<(f64, Preset)>,
    chan: Option<(Sender<Job>, Receiver<Done>)>,
}

impl Loader {
    /// EffectCraft's Space preview options: the person's own (EffectCraft's settings folder, read
    /// again every [`RECHECK`] seconds), else EffectCraft's defaults.
    pub(crate) fn space_preset(&mut self, now: f64) -> Preset {
        match &self.preset {
            Some((at, p)) if now - at <= RECHECK => p.clone(),
            _ => {
                let p = effectcraft_ui_egui::panels::timeline::hosted::user_preview().unwrap_or_default().spacebar;
                self.preset = Some((now, p.clone()));
                p
            }
        }
    }

    /// The bars of card comp `c`, when read.
    pub(crate) fn peek(&self, c: &EcComp) -> Option<&Bars> {
        self.got.get(&bars_key(c)?)?.as_deref()
    }

    /// Ask for the bars of `comps` not read yet, take what the thread read, and forget what a
    /// save made stale.
    pub(crate) fn feed(&mut self, ctx: &egui::Context, comps: &[EcComp], now: f64) {
        if let Some((_, rx)) = &self.chan {
            while let Ok((key, bars)) = rx.try_recv() {
                self.asked.remove(&key);
                self.got.insert(key, bars);
            }
        }
        if now - self.checked > RECHECK {
            self.checked = now;
            let stale: Vec<PathBuf> = self.stamps.iter().filter(|(p, s)| file_seconds(p) != **s).map(|(p, _)| p.clone()).collect();
            for p in stale {
                self.stamps.remove(&p);
                self.got.retain(|(k, _), _| *k != p);
            }
        }
        for c in comps {
            let Some(key) = bars_key(c) else { continue };
            if self.got.contains_key(&key) || self.asked.contains(&key) {
                continue;
            }
            if self.chan.is_none() {
                self.chan = start(ctx.clone());
            }
            let Some((tx, _)) = &self.chan else { return };
            self.stamps.entry(key.0.clone()).or_insert_with(|| file_seconds(&key.0));
            if tx.send((key.clone(), key.0.clone(), c.clone())).is_ok() {
                self.asked.insert(key);
            } else {
                self.chan = None;
            }
        }
    }
}

/// The bars thread: opens each asked project (keeping the last one) and reads the comp's layers.
fn start(ctx: egui::Context) -> Option<(Sender<Job>, Receiver<Done>)> {
    let (jtx, jrx) = channel::<Job>();
    let (dtx, drx) = channel::<Done>();
    std::thread::Builder::new()
        .name("effectcraft-strips".into())
        .spawn(move || {
            let mut open: Option<(PathBuf, u64, figcraft_ec_inproc::InProc)> = None;
            while let Ok((key, path, comp)) = jrx.recv() {
                let bars = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let stamp = file_seconds(&path);
                    let fresh = open.as_ref().is_some_and(|(p, s, _)| *p == path && *s == stamp);
                    if !fresh {
                        open = figcraft_ec_inproc::InProc::open(&path).ok().map(|ec| (path.clone(), stamp, ec));
                    }
                    open.as_ref().and_then(|(_, _, ec)| read_bars(ec, &comp)).map(Arc::new)
                }))
                .unwrap_or(None);
                if dtx.send((key, bars)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        })
        .ok()?;
    Some((jtx, drx))
}

fn read_bars(ec: &figcraft_ec_inproc::InProc, c: &EcComp) -> Option<Bars> {
    use figcraft_ec_inproc::effectcraft_engine::project::ItemId;
    let comp_ref = if c.comp_id > 0 { json!(c.comp_id) } else { json!(c.comp) };
    let id = ec.comp_id(&comp_ref).ok()?;
    let comp = ec.session().project.comp(ItemId(id))?;
    let tokens = effectcraft_ui_egui::theme::Tokens::for_kind(effectcraft_ui_egui::theme::ThemeKind::Dark);
    let bars = comp.layers.iter().take(MAX_BARS).map(|l| (l.in_point.seconds(), l.out_point.seconds(), tokens.label(l.label))).collect();
    Some(Bars { duration: comp.duration.seconds(), count: comp.layers.len(), bars, work: (comp.work_area.0.seconds(), comp.work_area.1.seconds()) })
}

/// The cards with a closed strip on the current page (every card but the one whose Timeline is
/// open) and where each strip is.
pub(crate) fn closed(app: &FigApp, doc: &Document, page: NodeId) -> Vec<(NodeId, Geom)> {
    let open = app.effectcraft.panel.as_ref().filter(|p| p.open).map(|p| p.card);
    super::cards(doc, page)
        .into_iter()
        .filter(|id| Some(*id) != open)
        .filter_map(|id| {
            let c = doc.node(id)?.effectcraft.as_deref()?;
            let bars = app.effectcraft.strips.peek(c).map_or(1, |b| b.bars.len().max(1));
            Some((id, geom_of(doc, id, false, bars)?))
        })
        .collect()
}

/// The closed strip under screen point `pos`.
pub(crate) fn closed_at(app: &FigApp, doc: &Document, page: NodeId, pos: Pos2) -> Option<NodeId> {
    closed(app, doc, page).into_iter().rev().find(|(_, g)| on_screen(app, g.strip).contains(pos)).map(|(id, _)| id)
}

/// Draw the closed strips (in the card overlay, over the canvas image).
pub(crate) fn draw_closed(app: &FigApp, painter: &egui::Painter, doc: &Document, page: NodeId, hover: Option<Pos2>) {
    let t = crate::theme::Tokens::get(painter.ctx());
    for (id, g) in closed(app, doc, page) {
        let r = on_screen(app, g.strip);
        if !r.intersects(painter.clip_rect()) || r.width() < 2.0 {
            continue;
        }
        let Some(c) = doc.node(id).and_then(|n| n.effectcraft.as_deref()) else { continue };
        let s = r.width() / g.strip.width().max(1e-9) as f32 * g.k as f32;
        let hovered = hover.is_some_and(|h| r.contains(h));
        painter.rect_filled(r, CornerRadius::same((3.0 * s).clamp(0.0, 6.0) as u8), t.panel);
        painter.rect_stroke(
            r,
            CornerRadius::same((3.0 * s).clamp(0.0, 6.0) as u8),
            Stroke::new(1.0, if hovered { t.accent } else { t.border }),
            egui::StrokeKind::Inside,
        );
        let head = Rect::from_min_size(r.min, vec2(r.width(), CLOSED_HEADER as f32 * s));
        let bars = app.effectcraft.strips.peek(c);
        let font = 10.0 * s;
        if font >= 4.0 {
            let n = bars.map_or(String::new(), |b| format!(" · {} layers", b.count));
            let text = format!("▸ {}{n}", c.comp);
            let painter = painter.with_clip_rect(head.intersect(painter.clip_rect()));
            painter.text(head.left_center() + vec2(6.0 * s, 0.0), egui::Align2::LEFT_CENTER, text, egui::FontId::proportional(font), t.text);
        }
        let track = Rect::from_min_max(pos2(r.min.x + 2.0 * s, head.max.y), pos2(r.max.x - 2.0 * s, r.max.y - 1.0 * s));
        let (span, list): (f64, Vec<(f64, f64, Color32)>) = match bars {
            Some(b) if !b.bars.is_empty() => (b.duration, b.bars.clone()),
            _ => (c.end, vec![(c.start, c.end, t.text_secondary)]),
        };
        let span = if span.is_finite() && span > 0.0 { span } else { 1.0 };
        let x_of = |sec: f64| track.min.x + track.width() * (sec / span).clamp(0.0, 1.0) as f32;
        for (i, (a, b, col)) in list.iter().enumerate() {
            let y = track.min.y + i as f32 * BAR as f32 * s;
            let bar = Rect::from_min_max(pos2(x_of(*a), y + 1.0 * s), pos2(x_of(*b).max(x_of(*a) + 1.0), y + (BAR as f32 - 1.0) * s));
            painter.rect_filled(bar, CornerRadius::same((s).clamp(0.0, 2.0) as u8), col.gamma_multiply(0.85));
        }
        if let Some(at) = app.effectcraft.shown.get(&id) {
            let x = x_of(*at);
            painter.vline(x, track.y_range(), Stroke::new(1.0, t.accent));
        }
    }
}

/// An edge that sizes the open Timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Edge {
    /// The Timeline's right edge: its width.
    Right,
    /// The bottom edge (Timeline and Effect Controls): their height.
    Bottom,
    /// The Timeline's bottom-right corner: both.
    Corner,
    /// Effect Controls' right edge: its width.
    FxRight,
    /// Effect Controls' bottom-right corner: their width and the height.
    FxCorner,
}

impl Edge {
    fn label(self) -> &'static str {
        match self {
            Edge::Right => "Widen Timeline",
            Edge::Bottom => "Heighten Timeline",
            Edge::Corner => "Resize Timeline",
            Edge::FxRight => "Widen Effect Controls",
            Edge::FxCorner => "Resize Effect Controls",
        }
    }

    pub(crate) fn cursor(self) -> egui::CursorIcon {
        match self {
            Edge::Right | Edge::FxRight => egui::CursorIcon::ResizeHorizontal,
            Edge::Bottom => egui::CursorIcon::ResizeVertical,
            Edge::Corner | Edge::FxCorner => egui::CursorIcon::ResizeNwSe,
        }
    }

    /// The sizes a drag to page point (`x`, `y`) asks for, for a Timeline at `g`.
    pub(crate) fn size(self, g: &Geom, x: f64, y: f64) -> Size {
        let width = Some(x - g.strip.x0);
        let height = Some(y - g.strip.y0);
        match self {
            Edge::Right => Size { width, ..Size::default() },
            Edge::Bottom => Size { height, ..Size::default() },
            Edge::Corner => Size { width, height, fx: None },
            Edge::FxRight => Size { fx: g.fx.map(|f| x - f.x0), ..Size::default() },
            Edge::FxCorner => Size { height, fx: g.fx.map(|f| x - f.x0), width: None },
        }
    }

    /// Where the edge is on the page for a Timeline at `g` (the point a drag moves).
    fn at(self, g: &Geom) -> (f64, f64) {
        let fx = g.fx.unwrap_or(g.strip);
        match self {
            Edge::Right | Edge::Corner => (g.strip.x1, g.strip.y1),
            Edge::Bottom => (g.strip.x1, g.strip.y1),
            Edge::FxRight | Edge::FxCorner => (fx.x1, fx.y1),
        }
    }
}

/// The edge whose resize zone holds screen point `p`, for a Timeline at `strip` and Effect
/// Controls at `fx` (screen rectangles). Zones are [`BAND`] points either side of an edge (inside
/// and out) and [`CORNER`] points either way from a bottom-right corner, at any zoom; corners win
/// over edges, and edges over the panels' content under them.
pub(crate) fn edge_at(strip: Rect, fx: Option<Rect>, p: Pos2) -> Option<Edge> {
    let near = |c: Pos2| (p.x - c.x).abs() <= CORNER && (p.y - c.y).abs() <= CORNER;
    if near(strip.max) {
        return Some(Edge::Corner);
    }
    if let Some(f) = fx
        && near(f.max)
    {
        return Some(Edge::FxCorner);
    }
    let across = |x: f32| (p.x - x).abs() <= BAND;
    let along = |r: Rect| p.y >= r.min.y - BAND && p.y <= r.max.y + BAND;
    if across(strip.max.x) && along(strip) {
        return Some(Edge::Right);
    }
    if let Some(f) = fx
        && across(f.max.x)
        && along(f)
    {
        return Some(Edge::FxRight);
    }
    let right = fx.map_or(strip.max.x, |f| f.max.x);
    if (p.y - strip.max.y).abs() <= BAND && p.x >= strip.min.x - BAND && p.x <= right + BAND {
        return Some(Edge::Bottom);
    }
    None
}

/// The screen rectangles the resize zones reach (the panels grown by [`BAND`], and the corners):
/// the canvas leaves the pointer there to them.
pub(crate) fn zones(strip: Rect, fx: Option<Rect>) -> Vec<Rect> {
    let mut out: Vec<Rect> = [Some(strip), fx].into_iter().flatten().map(|r| r.expand(BAND)).collect();
    out.extend([Some(strip), fx].into_iter().flatten().map(|r| Rect::from_center_size(r.max, egui::Vec2::splat(CORNER * 2.0))));
    out
}

/// Draw edge `edge`'s affordance (hovered or dragged): an accent line along it and, at a corner,
/// a grip mark. `strip`, `fx`: screen rectangles.
pub(crate) fn draw_edge(painter: &egui::Painter, strip: Rect, fx: Option<Rect>, edge: Edge) {
    let t = crate::theme::Tokens::get(painter.ctx());
    let line = Stroke::new(2.0, t.accent);
    let f = fx.unwrap_or(strip);
    let right = f.max.x.max(strip.max.x);
    let grip = |c: Pos2| {
        for d in [4.0, 8.0, 12.0] {
            painter.line_segment([c + vec2(-d, -1.5), c + vec2(-1.5, -d)], Stroke::new(1.5, t.accent));
        }
    };
    match edge {
        Edge::Right => {
            painter.vline(strip.max.x, strip.y_range(), line);
        }
        Edge::FxRight => {
            painter.vline(f.max.x, f.y_range(), line);
        }
        Edge::Bottom => {
            painter.hline(strip.min.x..=right, strip.max.y, line);
        }
        Edge::Corner => {
            painter.vline(strip.max.x, strip.y_range(), line);
            painter.hline(strip.min.x..=right, strip.max.y, line);
            grip(strip.max);
        }
        Edge::FxCorner => {
            painter.vline(f.max.x, f.y_range(), line);
            painter.hline(strip.min.x..=right, strip.max.y, line);
            grip(f.max);
        }
    }
}

/// An edge of the open Timeline being dragged: the card, the edge, where it was grabbed relative
/// to the edge (page units: the edge doesn't jump to the pointer), whether the drag has started
/// (past egui's drag threshold), and the nodes it pushed and how far (x, y).
pub(crate) struct Widen {
    pub card: NodeId,
    pub edge: Edge,
    pub grab: (f64, f64),
    pub started: bool,
    pub pushed: HashMap<NodeId, (f64, f64)>,
}

/// The top-level node (a child of `page`) holding `id`.
fn top_of(doc: &Document, page: NodeId, id: NodeId) -> NodeId {
    let mut at = id;
    for _ in 0..256 {
        match doc.node(at).and_then(|n| n.parent) {
            Some(p) if p != page => at = p,
            _ => break,
        }
    }
    at
}

/// Resize card `card`'s open Timeline in `doc` (the gesture's snapshot) to `size` (page units;
/// `None`: as it is; `fx`: Effect Controls' width): its plugin data, and the page's top-level nodes the
/// Timeline and its Effect Controls would now cover moved away, each at least as far as before in
/// this drag (`pushed`, x and y). Wider pushes what is to the right in the card-and-Timeline
/// band right; taller pushes what is below in its columns down; pushed nodes push the ones behind
/// them, a small gap kept. `strips`: the other cards and their closed strips' bar counts; a strip
/// counts as part of the node holding it.
pub(crate) fn resize(
    doc: &mut Document,
    page: NodeId,
    card: NodeId,
    size: Size,
    strips: &[(NodeId, usize)],
    pushed: &mut HashMap<NodeId, (f64, f64)>,
) -> Option<()> {
    let b = doc.world_bounds(card)?;
    let least = least_width(b.width(), b.height());
    let (lo, hi) = height_range(b.width(), b.height());
    if !least.is_finite() || !lo.is_finite() || doc.node(card)?.effectcraft.is_none() {
        return None;
    }
    let side = b.width().max(b.height());
    let width = size.width.map(|w| if w.is_finite() { w.clamp(least, least.max(side * MAX_WIDTH)) } else { least });
    let height = size.height.map(|h| if h.is_finite() { h.clamp(lo, hi) } else { lo });
    let (fx_lo, _, fx_hi) = fx_range(b.width(), b.height());
    let fx = size.fx.map(|w| if w.is_finite() { w.clamp(fx_lo, fx_hi) } else { fx_lo });
    {
        let data = &mut doc.get_mut(card).ok()?.plugin_data;
        for (key, v) in [(WIDTH_KEY, width), (HEIGHT_KEY, height), (FX_WIDTH_KEY, fx)] {
            if let Some(v) = v {
                data.insert(key.into(), format!("{v:.1}"));
            }
        }
    }
    let g = geom_of(doc, card, true, 0)?;
    let column = PRect::new(b.x0, b.y0, g.whole().x1, g.whole().y1);
    let gap = GAP * g.k;
    let own = top_of(doc, page, card);
    // The page's other top-level nodes and what each covers (its cards' strips included).
    let boxes = |doc: &Document| -> Vec<(NodeId, PRect)> {
        let Some(n) = doc.node(page) else { return Vec::new() };
        n.children
            .iter()
            .copied()
            .filter(|id| *id != own)
            .filter_map(|id| {
                if !doc.node(id)?.visible {
                    return None;
                }
                let mut r = doc.world_bounds(id)?;
                for (sc, bars) in strips {
                    if top_of(doc, page, *sc) == id
                        && let Some(sg) = geom_of(doc, *sc, false, *bars)
                    {
                        r = r.union(sg.strip);
                    }
                }
                (r.x0.is_finite() && r.y0.is_finite()).then_some((id, r))
            })
            .collect()
    };
    let mut moves: Vec<(NodeId, f64, f64)> = Vec::new();
    if width.is_some() || fx.is_some() {
        // To the column's right in its band, left to right.
        let mut others: Vec<(NodeId, PRect)> =
            boxes(doc).into_iter().filter(|(_, r)| r.y0 < column.y1 && r.y1 > column.y0 && r.x0 >= b.x0 + 1e-6).collect();
        others.sort_by(|a, b| a.1.x0.total_cmp(&b.1.x0));
        let mut frontier = column.x1 + gap;
        for (id, r) in others {
            let (before, _) = pushed_by(pushed, id);
            let dx = before.max(frontier - r.x0).max(0.0);
            if dx > 0.0 {
                moves.push((id, dx, 0.0));
                frontier = frontier.max(r.x1 + dx + gap);
            }
        }
    }
    apply(doc, pushed, &moves);
    if height.is_some() {
        // Below the card in the column's x-range, top to bottom.
        let mut moves: Vec<(NodeId, f64, f64)> = Vec::new();
        let mut others: Vec<(NodeId, PRect)> =
            boxes(doc).into_iter().filter(|(_, r)| r.x0 < column.x1 && r.x1 > column.x0 && r.y0 >= b.y1 - 1e-6).collect();
        others.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
        let mut frontier = column.y1 + gap;
        for (id, r) in others {
            let (_, before) = pushed_by(pushed, id);
            let dy = before.max(frontier - r.y0).max(0.0);
            if dy > 0.0 {
                moves.push((id, 0.0, dy));
                frontier = frontier.max(r.y1 + dy + gap);
            }
        }
        apply(doc, pushed, &moves);
    }
    Some(())
}

fn pushed_by(pushed: &HashMap<NodeId, (f64, f64)>, id: NodeId) -> (f64, f64) {
    pushed.get(&id).copied().unwrap_or((0.0, 0.0))
}

/// Move each node by its (dx, dy) and remember how far it went on each axis.
fn apply(doc: &mut Document, pushed: &mut HashMap<NodeId, (f64, f64)>, moves: &[(NodeId, f64, f64)]) {
    for (id, dx, dy) in moves {
        let w = doc.world_transform(*id);
        let _ = doc.set_world_transform(*id, figcraft_geom::Affine::translate((*dx, *dy)) * w);
        let e = pushed.entry(*id).or_insert((0.0, 0.0));
        if *dx > 0.0 {
            e.0 = *dx;
        }
        if *dy > 0.0 {
            e.1 = *dy;
        }
    }
}

/// The resize edges of card `card`'s open Timeline (`g`; `strip`, `fx`: on screen), from the raw
/// pointer, before the panels draw: a press in a zone grabs that edge, and the drag stays its
/// until the button is let go wherever the pointer goes; it sizes only once past egui's drag
/// threshold. Returns the edge hovered or dragged (its affordance shows, and the panels sit this
/// frame out so the edge wins over what is under it).
pub(crate) fn edges_input(app: &mut FigApp, ctx: &egui::Context, card: NodeId, g: &Geom, strip: Rect, fx: Option<Rect>) -> Option<Edge> {
    let (pos, pressed, down, past) =
        ctx.input(|i| (i.pointer.interact_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.is_decidedly_dragging()));
    if app.effectcraft.widen.as_ref().is_some_and(|w| w.card != card) {
        // Another card's Timeline opened mid-drag: that drag is over.
        if app.effectcraft.widen.take().is_some_and(|w| w.started) {
            let _ = app.session.commit_interaction();
        }
    }
    if app.effectcraft.widen.is_none() {
        let over = pos.filter(|p| app.canvas.rect.contains(*p)).and_then(|p| edge_at(strip, fx, p));
        let Some(edge) = over else { return None };
        if pressed && !egui::Popup::is_any_open(ctx) {
            let (ex, ey) = edge.at(g);
            let p = pos.map(|p| app.to_page(p)).unwrap_or_default();
            app.effectcraft.widen = Some(Widen { card, edge, grab: (p.x - ex, p.y - ey), started: false, pushed: HashMap::new() });
        }
        return Some(edge);
    }
    let Some(edge) = app.effectcraft.widen.as_ref().map(|w| w.edge) else { return None };
    if !down {
        if app.effectcraft.widen.take().is_some_and(|w| w.started) {
            let _ = app.session.commit_interaction();
        }
        return Some(edge);
    }
    let Some(p) = pos.map(|p| app.to_page(p)) else { return Some(edge) };
    if !past {
        return Some(edge);
    }
    let (doc, page) = match app.session.ds() {
        Ok(ds) => (ds.doc.clone(), ds.page),
        Err(_) => return Some(edge),
    };
    let strips: Vec<(NodeId, usize)> = closed(app, &doc, page)
        .into_iter()
        .filter(|(id, _)| *id != card)
        .filter_map(|(id, _)| Some((id, app.effectcraft.strips.peek(doc.node(id)?.effectcraft.as_deref()?).map_or(1, |b| b.bars.len().max(1)))))
        .collect();
    let Some(w) = app.effectcraft.widen.as_mut() else { return Some(edge) };
    if !w.started {
        w.started = true;
        let _ = app.session.begin_interaction(edge.label());
    }
    let Some(w) = app.effectcraft.widen.as_mut() else { return Some(edge) };
    let mut pushed = std::mem::take(&mut w.pushed);
    let size = edge.size(g, p.x - w.grab.0, p.y - w.grab.1);
    let _ = app.session.preview(|d, _| {
        resize(d, page, card, size, &strips, &mut pushed);
        Ok(())
    });
    if let Some(w) = app.effectcraft.widen.as_mut() {
        w.pushed = pushed;
    }
    Some(edge)
}

#[cfg(test)]
#[path = "effectcraft_strips_tests.rs"]
mod tests;

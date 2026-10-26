//! Fork-only (`effectcraft-inproc`): what the EffectCraft cards need around them, beside the
//! Timeline ([`crate::effectcraft_panel`]).
//!
//! - **Unsaved EffectCraft edits are never lost.** The open Timeline's in-process session keeps
//!   edits only FigCraft has when the EffectCraft app doesn't have the project open (a mirror's
//!   edits are the app's: nothing to ask). Quitting, closing the window, closing or leaving the
//!   FigCraft file the card is in, or deleting the card asks first, as FigCraft does for its own
//!   files: Save (the Timeline's ⌘S: EffectCraft's own save of the `.ecproj`), Don't Save (the
//!   edits go, the cards show the file again) or Cancel (nothing closes; leaving the file goes
//!   back to it; a deleted card's edits are kept, ⌘Z brings the card back). A file already closed
//!   has no Cancel. [`hold_close`] is the window's check, [`confirm`] draws the question.
//! - **New cards come into view.** A card made by Pre-compose (Open New Composition), New
//!   EffectCraft Comp, a comp opened from the Project tab or explode is shown with its open
//!   Timeline ([`reveal`]): the canvas glides there at the same zoom, zooming out only when it
//!   doesn't fit. Already in view: nothing moves. A pan or zoom by hand stops the glide.
//! - **⌘N in the Timeline** (EffectCraft's New Composition, `comp.new` without settings) opens
//!   EffectCraft's New Composition dialog as `effectcraft.newComp` does; OK makes the comp in the
//!   Timeline's project and places it as a card with its Timeline open ([`intercept`]).
//! - **Preview resolution and Cache Before Playback.** Frames for a card are rendered at the comp
//!   viewer's resolution (its Resolution in the project: Full, Half, Third, Quarter, Custom; Auto
//!   is what the card's size on screen needs, as before) and, while the card plays, at the Space
//!   preview preset's resolution (Auto: the viewer's) ([`shape_wanted`]). With Cache Before
//!   Playback the card renders its play range first ("Caching n%" on it), then plays; play pressed
//!   again meanwhile plays what is cached ("If caching, play cached frames"), else stops
//!   ([`cache_step`]). A range bigger than the frame budget caches what fits.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use egui::{Color32, pos2};
use figcraft_doc::{Document, EcComp, NodeId};
use figcraft_geom::Rect as PRect;
use serde_json::{Value, json};

use crate::FigApp;

/// Seconds a glide to a new card takes.
const GLIDE: f64 = 0.35;
/// Screen points kept free round a card brought into view.
const MARGIN: f64 = 48.0;
/// Seconds caching may go without a new frame before the card plays what it has.
const STALL: f64 = 10.0;
/// Share of the frame budget one cached range may take.
const CACHE_SHARE: f64 = 0.6;

/// What happens once the unsaved-edits question is answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Then {
    /// The window (or the app) closes.
    Close,
    /// The file the card is in was left for another: Cancel goes back to it.
    Left { back: u64 },
    /// The card or its file went away: nothing to go back to (a card: ⌘Z brings it back).
    Gone { file_open: bool },
}

/// The unsaved-edits question while it shows.
#[derive(Clone, Debug)]
pub(crate) struct Ask {
    pub(crate) then: Then,
    /// The window it shows in.
    window: u64,
}

/// An answer to the question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    Save,
    DontSave,
    Cancel,
}

struct Glide {
    from: (f64, f64, f64),
    to: (f64, f64, f64),
    since: Option<f64>,
    /// The view as last set (a different one: moved by hand, the glide stops).
    last: Option<(f64, f64, f64)>,
}

/// A card rendering its play range before it plays.
#[derive(Clone, Debug)]
pub(crate) struct Caching {
    pub(crate) card: NodeId,
    comp: EcComp,
    /// Where it plays from.
    from: f64,
    pub(crate) frames: Vec<i64>,
    /// The most frames held so far, and when that went up.
    best: (usize, f64),
}

#[derive(Default)]
pub(crate) struct State {
    /// The open Timeline's project and the FigCraft file (uid) its card is in.
    owner: Option<(PathBuf, u64)>,
    pub(crate) ask: Option<Ask>,
    /// A deleted card's edits the person chose to keep (asked once).
    quiet: Option<(u64, NodeId)>,
    /// Cards to bring into view.
    reveal: Vec<NodeId>,
    glide: Option<Glide>,
    pub(crate) cache: Option<Caching>,
    /// The transport last frame: playing, on which card.
    was: (bool, Option<NodeId>),
    /// The Space preset's resolution factor (`None`: Auto) and whether it caches.
    preview_factor: Option<u32>,
    /// Each visible card's frame size for its size on screen (Auto).
    auto: HashMap<NodeId, u32>,
}

// ------------------------------------------------------------------------------------------
// Unsaved edits

/// The open Timeline has edits only FigCraft has (not a mirror's, not read-only).
pub(crate) fn dirty(app: &FigApp) -> bool {
    app.effectcraft.panel.as_ref().is_some_and(|p| p.host.is_dirty())
}

/// The `.ecproj` the open Timeline edits (its file name).
fn project_name(app: &FigApp) -> String {
    app.effectcraft
        .panel
        .as_ref()
        .and_then(|p| p.host.path().file_name().map(|f| f.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "the EffectCraft project".to_string())
}

/// The window is asked to close (`windows::request_close`): `true` when the Timeline's unsaved
/// edits are to be asked about first (the question shows; the close goes on once answered).
pub(crate) fn hold_close(app: &mut FigApp) -> bool {
    if !dirty(app) || app.effectcraft.cards.ask.is_some() {
        return app.effectcraft.cards.ask.as_ref().is_some_and(|a| a.then == Then::Close);
    }
    // Another window closing that doesn't hold the card's file: nothing of the edits goes with it.
    let last = app.windows.current == 0 && app.windows.list.is_empty();
    let holds = app.effectcraft.cards.owner.as_ref().is_none_or(|(_, uid)| app.shows(*uid));
    if !last && !holds {
        return false;
    }
    app.effectcraft.cards.ask = Some(Ask { then: Then::Close, window: app.windows.current });
    true
}

impl Then {
    fn is_close(self) -> bool {
        self == Then::Close
    }
}

/// Once a frame: who holds the Timeline's edits, and the question when their card or file is
/// left, closed or deleted.
fn watch(app: &mut FigApp) {
    let Some(p) = app.effectcraft.panel.as_ref() else {
        app.effectcraft.cards.owner = None;
        app.effectcraft.cards.quiet = None;
        if app.effectcraft.cards.ask.as_ref().is_some_and(|a| !a.then.is_close()) {
            app.effectcraft.cards.ask = None;
        }
        return;
    };
    let (path, card, dirty) = (p.path(), p.card, p.host.is_dirty());
    let active = app.session.ds().ok().map(|d| (d.uid, d.doc.node(card).is_some_and(|n| n.effectcraft.is_some())));
    let st = &mut app.effectcraft.cards;
    let fresh = st.owner.as_ref().is_none_or(|(p, _)| *p != path);
    // Without edits the Timeline goes with the file showing its card.
    if let Some((uid, has)) = active
        && (fresh || (!dirty && has))
    {
        st.owner = Some((path, uid));
    }
    if !dirty {
        st.quiet = None;
        if st.ask.as_ref().is_some_and(|a| !a.then.is_close()) {
            st.ask = None;
        }
        return;
    }
    if st.ask.is_some() || app.ui.confirm_close || app.ui.confirm_close_tab.is_some() {
        return;
    }
    let Some((_, owner)) = st.owner.clone() else { return };
    let then = if !app.session.docs.iter().any(|d| d.uid == owner) {
        Some(Then::Gone { file_open: false })
    } else if active.is_some_and(|(uid, _)| uid != owner) {
        Some(Then::Left { back: owner })
    } else if active.is_some_and(|(_, has)| !has) && st.quiet != Some((owner, card)) {
        Some(Then::Gone { file_open: true })
    } else {
        None
    };
    if let Some(then) = then {
        st.ask = Some(Ask { then, window: app.windows.current });
    }
}

/// Answer the question: Save saves the Timeline's project (it stays asked when that fails),
/// Don't Save drops the edits, Cancel keeps them; then what was going on goes on.
pub(crate) fn answer(app: &mut FigApp, ctx: &egui::Context, a: Answer) {
    let Some(ask) = app.effectcraft.cards.ask.take() else { return };
    match a {
        Answer::Save => {
            if let Some(mut p) = app.effectcraft.panel.take() {
                app.effectcraft_save(&mut p);
                let still = p.host.is_dirty();
                app.effectcraft.panel = Some(p);
                if still {
                    // Not saved (the app opened the project meanwhile…): the toast says why.
                    return;
                }
            }
        }
        Answer::DontSave => discard(app),
        Answer::Cancel => {
            match ask.then {
                Then::Left { back } => {
                    if let Some(i) = app.session.docs.iter().position(|d| d.uid == back) {
                        app.run("doc.switch", json!({"index": i}));
                        app.ui.home = false;
                    }
                }
                Then::Gone { file_open: true } => {
                    app.effectcraft.cards.quiet =
                        app.effectcraft.cards.owner.as_ref().map(|(_, u)| *u).zip(app.effectcraft.panel.as_ref().map(|p| p.card));
                }
                Then::Gone { file_open: false } | Then::Close => {}
            }
            return;
        }
    }
    if ask.then.is_close() {
        crate::windows::request_close(app, ctx);
    }
}

/// Drop the Timeline and its unsaved edits: the cards show the saved file again.
fn discard(app: &mut FigApp) {
    let Some(p) = app.effectcraft.panel.take() else { return };
    let path = p.path();
    app.effectcraft_want(&path, None);
    app.effectcraft_send(super::io::Job::Override { path, snapshot: None });
    app.effectcraft.panel_rects = None;
    app.effectcraft.cards.owner = None;
}

/// The unsaved-edits question (in the window it was asked in).
pub(crate) fn confirm(app: &mut FigApp, ctx: &egui::Context) {
    let Some(ask) = app.effectcraft.cards.ask.clone() else { return };
    if ask.window != app.windows.current {
        return;
    }
    if !dirty(app) {
        app.effectcraft.cards.ask = None;
        if ask.then.is_close() {
            crate::windows::request_close(app, ctx);
        }
        return;
    }
    let name = project_name(app);
    let text = match ask.then {
        Then::Close => format!("Save the EffectCraft edits to {name} before closing?"),
        Then::Left { .. } => format!("Save the EffectCraft edits to {name} before leaving its file?"),
        Then::Gone { file_open: true } => format!("The card is gone. Save its EffectCraft edits to {name}?"),
        Then::Gone { file_open: false } => format!("Its file closed. Save the EffectCraft edits to {name}?"),
    };
    let cancel_ok = ask.then != Then::Gone { file_open: false };
    let t = crate::theme::Tokens::get(ctx);
    let (mut open, mut pick) = (true, None);
    crate::dialogs::window("Unsaved EffectCraft edits", ctx).order(egui::Order::Foreground).open(&mut open).show(ctx, |ui| {
        ui.set_width(360.0);
        crate::widgets::label(ui, &text, t.text);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if crate::widgets::button(ui, "Save", true, true).clicked() {
                pick = Some(Answer::Save);
            }
            if crate::widgets::button(ui, "Don't save", false, true).clicked() {
                pick = Some(Answer::DontSave);
            }
            if cancel_ok && crate::widgets::button(ui, "Cancel", false, true).clicked() {
                pick = Some(Answer::Cancel);
            }
        });
    });
    // Closed with its ✕: Cancel (a closed file's question stays until answered).
    if !open && pick.is_none() && cancel_ok {
        pick = Some(Answer::Cancel);
    }
    if let Some(a) = pick {
        answer(app, ctx, a);
    }
}

// ------------------------------------------------------------------------------------------
// New cards come into view

/// Bring card `card` (and its open Timeline) into view from the next frame.
pub(crate) fn reveal(app: &mut FigApp, card: NodeId) {
    let r = &mut app.effectcraft.cards.reveal;
    if !r.contains(&card) && r.len() < 64 {
        r.push(card);
    }
}

/// Bring into view the cards on the page that aren't in `before` (explode).
pub(crate) fn reveal_new(app: &mut FigApp, before: &[NodeId]) {
    let Ok(ds) = app.session.ds() else { return };
    let new: Vec<NodeId> = super::cards(&ds.doc, ds.page).into_iter().filter(|id| !before.contains(id)).collect();
    for id in new {
        reveal(app, id);
    }
}

/// The cards on the active page (explode diffs them).
pub(crate) fn on_page(app: &FigApp) -> Vec<NodeId> {
    app.session.ds().map(|d| super::cards(&d.doc, d.page)).unwrap_or_default()
}

/// What a card shows on the page: its box, with its open Timeline and Effect Controls.
fn shown_box(doc: &Document, card: NodeId, open: bool) -> Option<PRect> {
    let b = doc.world_bounds(card)?;
    Some(super::strips::geom_of(doc, card, open, 0).map_or(b, |g| g.whole().union(b)))
}

/// The view (centre x, centre y, zoom) that shows page box `r` in a `vw`×`vh` viewport now at
/// `(cx, cy, zoom)`: the least pan at the same zoom when it fits, else zoomed out to fit and
/// centred. `None`: it is in view already (or nothing sensible to do).
pub(crate) fn view_to_show(view: (f64, f64, f64), viewport: (f64, f64), r: PRect) -> Option<(f64, f64, f64)> {
    let (cx, cy, zoom) = view;
    let (vw, vh) = viewport;
    let finite = [cx, cy, zoom, vw, vh, r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite());
    if !finite || zoom <= 0.0 || vw <= 1.0 || vh <= 1.0 || r.width() < 0.0 || r.height() < 0.0 {
        return None;
    }
    let m = MARGIN.min(vw / 4.0).min(vh / 4.0);
    let (hw, hh) = ((vw / 2.0 - m) / zoom, (vh / 2.0 - m) / zoom);
    if r.width() <= hw * 2.0 && r.height() <= hh * 2.0 {
        // The least move that puts it inside (the margin kept).
        let shift = |c: f64, lo: f64, hi: f64, h: f64| {
            if lo < c - h {
                lo + h
            } else if hi > c + h {
                hi - h
            } else {
                c
            }
        };
        let (nx, ny) = (shift(cx, r.x0, r.x1, hw), shift(cy, r.y0, r.y1, hh));
        if (nx - cx).abs() < 1e-6 && (ny - cy).abs() < 1e-6 {
            return None;
        }
        return Some((nx, ny, zoom));
    }
    let z = ((vw - 2.0 * m) / r.width().max(1e-6)).min((vh - 2.0 * m) / r.height().max(1e-6));
    let z = z.clamp(figcraft_engine::ViewState::MIN_ZOOM, zoom);
    Some((r.center().x, r.center().y, z))
}

/// Eased progress (ease-out cubic) at `u` (0..1).
pub(crate) fn ease(u: f64) -> f64 {
    let u = if u.is_finite() { u.clamp(0.0, 1.0) } else { 1.0 };
    1.0 - (1.0 - u).powi(3)
}

/// Start a glide to the cards waiting to be shown, and move the one under way.
fn glide(app: &mut FigApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let pending = std::mem::take(&mut app.effectcraft.cards.reveal);
    if !pending.is_empty()
        && let Ok(ds) = app.session.ds()
    {
        let open = app.effectcraft.panel.as_ref().filter(|p| p.open).map(|p| p.card);
        let boxes: Vec<PRect> =
            pending.iter().filter(|id| ds.doc.page_of(**id) == Some(ds.page)).filter_map(|id| shown_box(&ds.doc, *id, open == Some(*id))).collect();
        let v = &ds.view;
        if let Some(r) = boxes.into_iter().reduce(|a, b| a.union(b))
            && let Some(to) = view_to_show((v.cx, v.cy, v.zoom), (v.vw, v.vh), r)
        {
            app.effectcraft.cards.glide = Some(Glide { from: (v.cx, v.cy, v.zoom), to, since: None, last: None });
        }
    }
    let Some(g) = app.effectcraft.cards.glide.as_mut() else { return };
    let Ok(ds) = app.session.ds() else {
        app.effectcraft.cards.glide = None;
        return;
    };
    let now_view = (ds.view.cx, ds.view.cy, ds.view.zoom);
    if let Some(last) = g.last
        && ((last.0 - now_view.0).abs() > 1e-6 || (last.1 - now_view.1).abs() > 1e-6 || (last.2 - now_view.2).abs() > 1e-9)
    {
        // Moved by hand meanwhile.
        app.effectcraft.cards.glide = None;
        return;
    }
    let since = *g.since.get_or_insert(now);
    let u = ease((now - since) / GLIDE);
    let (a, b) = (g.from, g.to);
    let lz = |x: f64| x.max(1e-9).ln();
    let zoom = (lz(a.2) + (lz(b.2) - lz(a.2)) * u).exp();
    let at = (a.0 + (b.0 - a.0) * u, a.1 + (b.1 - a.1) * u, if zoom.is_finite() { zoom } else { b.2 });
    let done = u >= 1.0;
    let set = if done { b } else { at };
    let _ = app.session.execute("view.set", &json!({"cx": set.0, "cy": set.1, "zoom": set.2}));
    let after = app.session.ds().map(|d| (d.view.cx, d.view.cy, d.view.zoom)).unwrap_or(set);
    if done {
        app.effectcraft.cards.glide = None;
    } else {
        g.last = Some(after);
        ctx.request_repaint();
    }
}

// ------------------------------------------------------------------------------------------
// ⌘N in the Timeline

fn new_comp_id() -> egui::Id {
    egui::Id::new("figcraft-ec-timeline-new-comp")
}

/// `EcHost::invoke`: EffectCraft's New Composition (`comp.new` without a size, ⌘N) opens its
/// dialog as `effectcraft.newComp` does (OK places the comp as a card); `None`: run as it is.
pub(crate) fn intercept(host: &crate::effectcraft_panel::EcHost, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "comp.new" || host.read_only().is_some() {
        return None;
    }
    let sized = params.as_object().is_some_and(|o| o.contains_key("width") || o.contains_key("height"));
    if sized {
        return None;
    }
    ctx.data_mut(|d| d.insert_temp(new_comp_id(), host.path().to_path_buf()));
    Some(Ok(json!({"dialog": "comp.new"})))
}

/// The New Composition dialog the Timeline asked for.
fn new_comp(app: &mut FigApp, ctx: &egui::Context) {
    let Some(path) = ctx.data_mut(|d| d.remove_temp::<PathBuf>(new_comp_id())) else { return };
    let from = app.effectcraft.panel.as_ref().filter(|p| p.host.path() == path).map(|p| p.card);
    let to = super::project::Target { project: Some(path), from, ..Default::default() };
    super::project::open_dialog(app, ctx, to);
}

// ------------------------------------------------------------------------------------------
// Preview resolution and Cache Before Playback

/// The comp viewer's resolution factor for card comp `c` (`None`: Auto): from the open Timeline
/// when it has the card's project, else the saved project.
fn viewer_factor(app: &FigApp, c: &EcComp) -> Option<u32> {
    use figcraft_ec_inproc::effectcraft_engine::project::ItemId;
    if c.project.is_empty() {
        return None;
    }
    let key = figcraft_ec_inproc::key(Path::new(&c.project));
    if let Some(p) = app.effectcraft.panel.as_ref().filter(|p| p.host.path() == key) {
        let comp = TimelineHost::session(&p.host).project.comp(ItemId(c.comp_id))?;
        return comp.resolution.factor().map(u32::from);
    }
    app.effectcraft.strips.peek(c).and_then(|b| b.resolution).map(u32::from)
}

/// The frame size (longest side, pixels) for comp `c` at resolution factor `k`.
pub(crate) fn side_at(c: &EcComp, k: u32) -> u32 {
    let k = k.clamp(1, 40);
    (c.width.max(c.height) / k).clamp(16, 8192)
}

/// The frame size for card `id` (`auto`: what its size on screen needs; `previewing`: it plays or
/// caches, so the Space preset's resolution, Auto meaning the viewer's).
fn side_of(app: &FigApp, id: NodeId, c: &EcComp, auto: u32) -> u32 {
    let st = &app.effectcraft.cards;
    let t = &app.session.effectcraft;
    let previewing = (t.playing() && t.node == Some(id)) || st.cache.as_ref().is_some_and(|k| k.card == id);
    let factor = if previewing { st.preview_factor.or_else(|| viewer_factor(app, c)) } else { viewer_factor(app, c) };
    factor.map_or(auto, |k| side_at(c, k))
}

/// The frames of `c` held at frame size `side`, of `frames`.
fn held(app: &FigApp, c: &EcComp, side: u32, frames: &[i64]) -> (usize, Option<i64>) {
    let Some(s) = app.effectcraft.stores.get(&super::StoreKey::of(c, side)).filter(|s| !s.stale()) else {
        return (0, frames.first().copied());
    };
    let n = frames.iter().filter(|f| s.frames.contains_key(f)).count();
    (n, frames.iter().copied().find(|f| !s.frames.contains_key(f)))
}

/// `effectcraft_sync`: each wanted card's frame size by the resolution rules, and a caching card
/// asks for its first frame not held yet.
pub(crate) fn shape_wanted(app: &mut FigApp, wanted: &mut [(NodeId, EcComp, u32, i64)]) {
    app.effectcraft.cards.auto.clear();
    for w in wanted.iter_mut() {
        app.effectcraft.cards.auto.insert(w.0, w.2);
        let side = side_of(app, w.0, &w.1, w.2);
        w.2 = side;
        if let Some(k) = app.effectcraft.cards.cache.as_ref().filter(|k| k.card == w.0)
            && let (_, Some(f)) = held(app, &w.1, side, &k.frames)
        {
            w.3 = f;
        }
    }
}

/// The frames a card plays over comp seconds `range`, every `step`-th, as many as the budget
/// holds at frame size `side`.
pub(crate) fn range_frames(c: &EcComp, range: (f64, f64), step: u32, side: u32) -> Vec<i64> {
    let r = c.frames();
    let fps = if c.fps.is_finite() && c.fps > 0.0 { c.fps } else { 30.0 };
    let (a, b) = (range.0.min(range.1), range.0.max(range.1));
    if !a.is_finite() || !b.is_finite() {
        return Vec::new();
    }
    let first = c.frame_of(a).clamp(*r.start(), *r.end());
    let last = c.frame_of((b - 0.5 / fps).max(a)).clamp(first, *r.end());
    let (w, h) = (f64::from(c.width.max(1)), f64::from(c.height.max(1)));
    let s = f64::from(side.max(1));
    let bytes = (s * s * (w.min(h) / w.max(h)) * 4.0).max(1.0);
    let cap = ((super::BUDGET as f64 * CACHE_SHARE / bytes) as usize).clamp(1, 100_000);
    (first..=last).step_by(step.clamp(1, 100) as usize).take(cap).collect()
}

/// `effectcraft_sync`: the Space preset's resolution for playing cards, and Cache Before
/// Playback: a card that starts playing holds still until its play range is rendered.
pub(crate) fn cache_step(app: &mut FigApp, ctx: &egui::Context, preset: &Preset, now: f64) {
    app.effectcraft.cards.preview_factor = preset.resolution.factor(preset.custom_resolution);
    caching(app, ctx, preset, now);
    // As left (its own pause and play aren't presses).
    let t = &app.session.effectcraft;
    app.effectcraft.cards.was = (t.playing(), t.node);
}

type Preset = figcraft_ec_inproc::effectcraft_engine::preview::PreviewPreset;

fn caching(app: &mut FigApp, ctx: &egui::Context, preset: &Preset, now: f64) {
    let t = &app.session.effectcraft;
    let (playing, node) = (t.playing(), t.node);
    let prev = app.effectcraft.cards.was;
    let started = playing && (!prev.0 || prev.1 != node);
    let comp_of = |app: &FigApp, id: NodeId| app.session.doc().ok().and_then(|d| d.node(id)?.effectcraft.as_deref().cloned());
    if let Some(k) = app.effectcraft.cards.cache.clone() {
        if started && node == Some(k.card) {
            // Pressed again while caching: what is cached plays, or it stops.
            app.effectcraft.cards.cache = None;
            if !preset.play_cached_frames {
                app.session.effectcraft.pause();
            }
            return;
        }
        if (playing && node != Some(k.card)) || comp_of(app, k.card).is_none() {
            app.effectcraft.cards.cache = None;
            return;
        }
        let auto = app.effectcraft.cards.auto.get(&k.card).copied().unwrap_or(super::SIDES[1]);
        let side = side_of(app, k.card, &k.comp, auto);
        let (n, missing) = held(app, &k.comp, side, &k.frames);
        if let Some(c) = app.effectcraft.cards.cache.as_mut()
            && n > c.best.0
        {
            c.best = (n, now);
        }
        if missing.is_none() || now - k.best.1 > STALL {
            app.effectcraft.cards.cache = None;
            app.session.effectcraft.play(k.card, &k.comp, Some(k.from));
        }
        ctx.request_repaint();
        return;
    }
    if !(started && preset.cache_before_playback) {
        return;
    }
    let Some(id) = node else { return };
    let Some(c) = comp_of(app, id) else { return };
    let t = &app.session.effectcraft;
    let from = t.time();
    let pv = t.preview(id).copied();
    let range = figcraft_engine::effectcraft::play_range((c.start, c.end), pv.as_ref(), from);
    let step = pv.map_or(0, |p| p.skip).saturating_add(1);
    let auto = app.effectcraft.cards.auto.get(&id).copied().unwrap_or(super::SIDES[1]);
    app.effectcraft.cards.cache = Some(Caching { card: id, comp: c.clone(), from, frames: Vec::new(), best: (0, now) });
    let side = side_of(app, id, &c, auto);
    let frames = range_frames(&c, range, step, side);
    let (n, missing) = held(app, &c, side, &frames);
    if missing.is_none() || frames.is_empty() {
        app.effectcraft.cards.cache = None;
        return;
    }
    app.session.effectcraft.pause();
    app.effectcraft.cards.cache = Some(Caching { card: id, comp: c, from, frames, best: (n, now) });
    ctx.request_repaint();
}

/// "Caching n%" on the card rendering its play range.
pub(crate) fn draw(app: &FigApp, painter: &egui::Painter) {
    let Some(k) = app.effectcraft.cards.cache.as_ref() else { return };
    let Ok(doc) = app.session.doc() else { return };
    let Some(r) = super::screen_rect(app, doc, k.card) else { return };
    let pic = super::fit(r, k.comp.width, k.comp.height);
    let auto = app.effectcraft.cards.auto.get(&k.card).copied().unwrap_or(super::SIDES[1]);
    let side = side_of(app, k.card, &k.comp, auto);
    let (n, _) = held(app, &k.comp, side, &k.frames);
    let pct = (n * 100).checked_div(k.frames.len()).unwrap_or(100);
    super::chip(
        painter,
        pos2(pic.min.x + 6.0, pic.min.y + 6.0),
        format!("Caching {pct}%"),
        egui::FontId::proportional(11.0),
        Color32::from_black_alpha(190),
    );
}

// ------------------------------------------------------------------------------------------

/// Once a frame, before the Timeline draws: the edits' owner and question, the glide to new
/// cards, the Timeline's ⌘N.
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context) {
    watch(app);
    new_comp(app, ctx);
    glide(app, ctx);
    // Cards whose node went away leave no caching or sizes behind.
    if let Ok(doc) = app.session.doc() {
        let alive: HashSet<NodeId> = app.effectcraft.cards.auto.keys().copied().filter(|id| doc.node(*id).is_some()).collect();
        app.effectcraft.cards.auto.retain(|id, _| alive.contains(id));
    }
}

#[cfg(test)]
#[path = "effectcraft_cards_tests.rs"]
mod tests;

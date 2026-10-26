//! EffectCraft cards on the canvas ([`figcraft_doc::effectcraft`]): the desktop app asks the
//! running EffectCraft (its control channel, 127.0.0.1:9877) for each card's frames and draws the
//! one at the playhead over the card, like Dynamic Link. EffectCraft answers from its own preview
//! cache and queues the frames it lacks behind its own viewer's work, so nothing renders twice
//! and its preview never waits for FigCraft; frames show up here as they become ready.
//!
//! What this side does: a background thread holds the connection (frames arrive as JPEG and are
//! turned into textures there); the canvas paints the texture nearest the playhead after the
//! canvas image, so no tile is re-rendered; the card's poster frame goes into the document for
//! when EffectCraft isn't running. A card plays in a loop with the comp's own sound; cards and
//! audio strips covering the same edit time share one playhead. An EffectCraft without the
//! `figcraft.*` methods still works, slower, through `render.frame`.
//!
//! Fork-only, with the `effectcraft-inproc` feature (desktop): a card whose saved `.ecproj` is on
//! disk renders through EffectCraft's engine linked into FigCraft (`figcraft-ec-inproc`, the same
//! `figcraft.*` methods answered in-process) whenever the EffectCraft app is
//! closed or has another project open; when the app has that project open its live edits win and
//! the card asks the app as before (`io::choose`). A save of the file refreshes those frames.
//! Those frames render on their own worker (`io::worker`), one at a time, the newest request per
//! card winning; on EffectCraft's GPU compositor sharing the window's wgpu device when the app
//! shared it ([`set_native_frames`]), kept as GPU textures egui draws directly, else on the CPU.
//! Double-clicking a card opens EffectCraft's own Timeline on the canvas right under it, scaling
//! with the canvas; every other card shows a closed strip there ([`strips`],
//! [`crate::effectcraft_panel`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use egui::{Color32, CornerRadius, Pos2, Rect, Stroke, pos2, vec2};
use figcraft_doc::{Document, EcComp, NodeId};
use figcraft_engine::cmd::effectcraft::EXPLODED_FROM;
use figcraft_geom::Point;
use serde_json::{Value, json};

use crate::FigApp;

/// Fork: grab the comp's layers on the card the Timeline edits.
#[cfg(feature = "effectcraft-inproc")]
#[path = "effectcraft_handles.rs"]
pub(crate) mod handles;

/// Fork: EffectCraft's viewer tools (rotate, anchor, masks, pen, type) on the card the Timeline
/// edits.
#[cfg(feature = "effectcraft-inproc")]
#[path = "effectcraft_viewer.rs"]
pub(crate) mod viewer;

/// Fork: the EffectCraft Timelines on the canvas, under their cards.
#[cfg(feature = "effectcraft-inproc")]
#[path = "effectcraft_strips.rs"]
pub(crate) mod strips;

/// Fork: files dropped on cards, EffectCraft's Project panel beside the Timeline, new comps.
#[cfg(feature = "effectcraft-inproc")]
#[path = "effectcraft_project.rs"]
pub(crate) mod project;

/// Fork: EffectCraft's Pre-compose, Interpret Footage and Replace Footage dialogs for a card.
#[cfg(feature = "effectcraft-inproc")]
#[path = "effectcraft_dialogs.rs"]
pub(crate) mod dialogs;

/// Fork: the right-click menu of the card the Timeline edits is EffectCraft's viewer menu.
#[cfg(feature = "effectcraft-inproc")]
#[path = "effectcraft_menu.rs"]
pub(crate) mod menu;

/// Fork: unsaved Timeline edits asked about, new cards brought into view, ⌘N, preview
/// resolution and Cache Before Playback for cards.
#[cfg(feature = "effectcraft-inproc")]
#[path = "effectcraft_cards.rs"]
pub(crate) mod cards;

/// Play / explode button radius (screen points).
const BUTTON: f32 = 10.0;
/// Texture memory kept for card frames (bytes).
const BUDGET: usize = 1 << 30;
/// Frames asked for in one request, and requests out at once.
const BATCH: usize = 24;
const IN_FLIGHT: usize = 2;
/// Seconds before a frame EffectCraft is still rendering is asked for again.
const REPOLL: f64 = 0.2;
/// Seconds between checks of EffectCraft's project revision (edits there refresh the cards).
const REVISION_EVERY: f64 = 0.5;
/// Seconds before a frame that failed is asked for again.
pub(crate) const FAILED_RETRY: f64 = 10.0;
/// Seconds between attempts to reach an EffectCraft that wasn't running.
const RETRY_DOWN: f64 = 2.0;
/// Frames either side of the playhead kept for long cards.
const WINDOW: i64 = 300;
/// Seconds from the playhead a frame may be and still stand in for it.
const NEAR: f64 = 2.0;
/// Most cards one explode places.
const MAX_EXPLODE: usize = 16;
/// Frame sizes asked for (longest side, pixels): the smallest one at least as big as the card
/// on screen, at most half the comp.
const SIDES: [u32; 3] = [240, 480, 960];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct StoreKey {
    comp: String,
    comp_id: u64,
    /// The card's project: setting another one is another store (its frames are asked afresh).
    project: String,
    side: u32,
}

impl StoreKey {
    pub(crate) fn of(c: &EcComp, side: u32) -> StoreKey {
        StoreKey { comp: c.comp.clone(), comp_id: c.comp_id, project: c.project.clone(), side }
    }
    pub(crate) fn same_comp(&self, c: &EcComp) -> bool {
        self.comp == c.comp && self.comp_id == c.comp_id && self.project == c.project
    }
}

/// A card frame's texture: decoded on the CPU, or EffectCraft's GPU texture registered with egui
/// (in-process frames on the shared device: no copy through the CPU).
pub(crate) enum FrameTex {
    Cpu(egui::TextureHandle),
    #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
    Native(NativeFrame),
}

impl FrameTex {
    pub(crate) fn id(&self) -> egui::TextureId {
        match self {
            FrameTex::Cpu(t) => t.id(),
            FrameTex::Native(n) => n.id,
        }
    }

    pub(crate) fn size(&self) -> [usize; 2] {
        match self {
            FrameTex::Cpu(t) => t.size(),
            FrameTex::Native(n) => n.size,
        }
    }
}

/// The app's way to show EffectCraft's GPU frames (a `wgpu::Texture` on the window's device,
/// passed as `Any` so this crate needs no wgpu) as egui textures.
pub trait NativeFrames: Send + Sync {
    /// The egui texture showing `texture`; `None` when it isn't one this app can show.
    fn register(&self, texture: &(dyn std::any::Any + Send + Sync)) -> Option<egui::TextureId>;
    fn free(&self, id: egui::TextureId);
}

static NATIVE_FRAMES: std::sync::OnceLock<Box<dyn NativeFrames>> = std::sync::OnceLock::new();

/// Show in-process EffectCraft GPU frames through `frames` (once per process).
pub fn set_native_frames(frames: Box<dyn NativeFrames>) {
    let _ = NATIVE_FRAMES.set(frames);
}

#[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
pub(crate) fn native_frames() -> Option<&'static dyn NativeFrames> {
    NATIVE_FRAMES.get().map(|b| b.as_ref())
}

/// A registered GPU frame. Dropped, its texture id is freed two UI passes later (the pass that
/// dropped it may still draw it).
pub(crate) struct NativeFrame {
    id: egui::TextureId,
    size: [usize; 2],
}

impl NativeFrame {
    #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
    pub(crate) fn new(id: egui::TextureId, size: [usize; 2]) -> NativeFrame {
        NativeFrame { id, size }
    }
}

/// Ids of dropped GPU frames: (dropped this pass, dropped the pass before).
static RETIRED: std::sync::Mutex<(Vec<egui::TextureId>, Vec<egui::TextureId>)> = std::sync::Mutex::new((Vec::new(), Vec::new()));

impl Drop for NativeFrame {
    fn drop(&mut self) {
        RETIRED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0.push(self.id);
    }
}

/// Free the GPU frames dropped two passes ago (once per UI pass).
fn retire_native_frames() {
    let old = {
        let mut r = RETIRED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let newer = std::mem::take(&mut r.0);
        std::mem::replace(&mut r.1, newer)
    };
    if let Some(f) = NATIVE_FRAMES.get() {
        for id in old {
            f.free(id);
        }
    }
}

struct Slot {
    tex: FrameTex,
    /// EffectCraft's project revision the frame was rendered at.
    rev: u64,
    bytes: usize,
}

#[derive(Default)]
pub(crate) struct Store {
    frames: BTreeMap<i64, Slot>,
    /// Frames asked for (or still rendering in EffectCraft) and when.
    pending: HashMap<i64, f64>,
    /// Frames that failed and when: asked for again after [`FAILED_RETRY`] (the app may have
    /// opened the project, the file may be there now).
    failed: HashMap<i64, f64>,
    /// The frame the store was last wanted around (eviction keeps what is near it).
    focus: i64,
    /// Its frames were rendered in-process from the saved project, at this file revision (they
    /// stay fresh whatever the app's revision; a later save replaces them).
    file: Option<u64>,
    /// The revision of the Timeline's unsaved edits these frames must show: frames of another
    /// revision are asked for again (each edit re-renders the card; a later reply replaces them).
    want: Option<u64>,
}

/// Comp, comp id, start and end (milliseconds).
type SoundKey = (String, u64, i64, i64);

enum SoundState {
    Loading,
    Ready(Arc<figcraft_audio::Sound>),
    Silent,
}

/// The link to EffectCraft and the frames it sent.
#[derive(Default)]
pub struct Link {
    #[cfg(not(target_arch = "wasm32"))]
    io: Option<io::Io>,
    stores: HashMap<StoreKey, Store>,
    in_flight: usize,
    /// EffectCraft's project revision (0: not known yet).
    revision: u64,
    revision_asked: f64,
    revision_out: bool,
    /// Why EffectCraft can't be reached, and since when.
    down: Option<(String, f64)>,
    sounds: HashMap<SoundKey, SoundState>,
    /// Cards whose poster was asked for this session.
    posters: HashSet<NodeId>,
    /// Cards without a project, and when the app was last asked for its project.
    project_asked: HashMap<NodeId, f64>,
    /// The comp time each card showed last (it stays there when nothing moves it; explode takes
    /// the comps showing there).
    shown: HashMap<NodeId, f64>,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    out: Option<crate::audio_ui::out::Output>,
    /// The transport generation, card and sound piece the output was started for.
    out_for: Option<(u64, NodeId, SoundKey)>,
    last_time: f64,
    /// Whether the audio strip and the card transport played last frame.
    was: (bool, bool),
    /// The card whose slider is being dragged.
    scrub: Option<NodeId>,
    /// EffectCraft is closed and frames come from the saved project (`effectcraft-cli`), and
    /// that file's modification time (seconds) when they were rendered.
    headless: bool,
    file_stamp: Option<u64>,
    bytes: usize,
    /// The layers over each card, drawn over its frame ([`crate::card_cover`]).
    covers: std::cell::RefCell<crate::card_cover::Covers>,
    /// EffectCraft's Timeline on the canvas under the card it edits (fork-only, [`strips`]; its
    /// fonts: `theme::install_fonts`).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) panel: Option<crate::effectcraft_panel::Panel>,
    /// The Timeline's colour eyedropper is armed: canvas clicks take a colour.
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) picking: bool,
    /// Where the open Timeline and its Effect Controls were drawn on screen, and in which pass (the
    /// canvas leaves the pointer over them alone: [`panel_takes_pointer`]).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) panel_rects: Option<(u64, Vec<Rect>)>,
    /// The closed strips' layer bars ([`strips::Loader`]).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) strips: strips::Loader,
    /// An edge of the open Timeline being dragged ([`strips::edges_input`]).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) widen: Option<strips::Widen>,
    /// Space pressed in the Timeline: it plays the card when let go, unless the pointer was pressed
    /// meanwhile (Space held and a drag pans the canvas). `true`: it was.
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) space_tap: Option<bool>,
    /// Layer handles on the card the Timeline edits ([`handles`]).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) handles: handles::State,
    /// Unsaved-edits question, new cards into view, caching ([`cards`]).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) cards: cards::State,
}

impl Store {
    /// Its frames show another revision of the Timeline's edits than the one wanted.
    pub(crate) fn stale(&self) -> bool {
        self.want.is_some() && self.file != self.want
    }

    #[cfg(test)]
    pub(crate) fn with_revisions(file: Option<u64>, want: Option<u64>) -> Store {
        Store { file, want, ..Store::default() }
    }

    /// Frame `f` failed at `now`.
    pub(crate) fn fail(&mut self, f: i64, now: f64) {
        self.pending.remove(&f);
        self.failed.insert(f, now);
    }
}

/// Whether frame `f` of store `s` is to be asked for at `now`: not held fresh (`rev`: the app's
/// revision; `headless`: frames come from the saved file), not asked for in the last [`REPOLL`],
/// and not failed in the last [`FAILED_RETRY`].
pub(crate) fn wants(s: &Store, f: i64, now: f64, rev: u64, headless: bool) -> bool {
    let file = s.file.is_some();
    let stale = s.stale();
    let fresh = |x: &Slot| !stale && (x.rev >= rev || ((headless || file) && x.rev == 0));
    !(s.failed.get(&f).is_some_and(|t| now - t < FAILED_RETRY)
        || s.frames.get(&f).is_some_and(fresh)
        || s.pending.get(&f).is_some_and(|t| now - t < REPOLL))
}

impl Link {
    /// Whether EffectCraft couldn't be reached last time.
    pub fn down(&self) -> Option<&str> {
        self.down.as_ref().map(|(e, _)| e.as_str())
    }

    /// The texture of comp frame `f` of `c`, or the nearest one there is within the card's
    /// stretch and `NEAR` of `f` (else the card shows its poster, not some other moment).
    fn texture_near(&self, c: &EcComp, f: i64) -> Option<&FrameTex> {
        let mut best: Option<(i64, u32, &FrameTex)> = None;
        let r = c.frames();
        let reach = (c.fps * NEAR).round().max(1.0) as i64;
        let (lo, hi) = ((f - reach).max(*r.start()), (f + reach).min(*r.end()));
        if lo > hi {
            return None;
        }
        let f = f.clamp(lo, hi);
        for (k, s) in self.stores.iter().filter(|(k, _)| k.same_comp(c)) {
            let near = [s.frames.range(lo..=f).next_back(), s.frames.range(f..=hi).next()];
            for (g, slot) in near.into_iter().flatten() {
                let d = (g - f).abs();
                if best.is_none_or(|(bd, bs, _)| d < bd || (d == bd && k.side > bs)) {
                    best = Some((d, k.side, &slot.tex));
                }
            }
        }
        best.map(|(_, _, t)| t)
    }

    /// Frames of `c` held (any size) and frames shown.
    fn coverage(&self, c: &EcComp) -> (usize, usize) {
        let r = c.frames();
        let total = usize::try_from(r.end() - r.start() + 1).unwrap_or(0);
        let mut have: HashSet<i64> = HashSet::new();
        for (_, s) in self.stores.iter().filter(|(k, _)| k.same_comp(c)) {
            have.extend(s.frames.range(r.clone()).map(|(f, _)| *f));
        }
        (have.len().min(total), total)
    }
}

/// The cards on `page`, in paint order (the last is on top).
fn cards(doc: &Document, page: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![(page, 0usize)];
    while let Some((id, depth)) = stack.pop() {
        let Some(n) = doc.node(id) else { continue };
        if depth > 256 || !n.visible {
            continue;
        }
        if n.effectcraft.is_some() {
            out.push(id);
        }
        stack.extend(n.children.iter().rev().map(|c| (*c, depth + 1)));
    }
    out
}

/// The card's box on screen (the bounds of its four corners).
pub(crate) fn screen_rect(app: &FigApp, doc: &Document, id: NodeId) -> Option<Rect> {
    let n = doc.node(id)?;
    let w = doc.world_transform(id);
    let pts = [Point::new(0.0, 0.0), Point::new(n.width, 0.0), Point::new(0.0, n.height), Point::new(n.width, n.height)].map(|p| {
        let q = w * p;
        app.to_screen(q.x, q.y)
    });
    let r = Rect::from_points(&pts);
    (r.is_finite() && r.width() > 1.0 && r.height() > 1.0).then_some(r)
}

/// The largest `w`×`h` box centred in `r`.
pub(crate) fn fit(r: Rect, w: u32, h: u32) -> Rect {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let s = (r.width() / w).min(r.height() / h);
    Rect::from_center_size(r.center(), vec2(w * s, h * s))
}

/// The frame size to ask for a card `px` pixels wide on screen.
pub(crate) fn side_for(px: f32, c: &EcComp) -> u32 {
    let cap = (c.width.max(c.height) / 2).max(SIDES[0]);
    let px = if px.is_finite() { px.max(0.0) } else { 0.0 };
    SIDES.into_iter().filter(|s| *s <= cap).find(|s| *s as f32 >= px).unwrap_or_else(|| SIDES.into_iter().rfind(|s| *s <= cap).unwrap_or(SIDES[0]))
}

/// The frames of `c` to fetch, nearest the playhead frame `at` first (on to the end, then from
/// the start: the way it plays), within `WINDOW` of it.
pub(crate) fn order(c: &EcComp, at: i64) -> Vec<i64> {
    let r = c.frames();
    let (a, b) = (*r.start(), *r.end());
    let at = at.clamp(a, b);
    let (lo, hi) = ((at - WINDOW).max(a), (at + WINDOW).min(b));
    let mut out: Vec<i64> = (at..=hi).collect();
    if hi == b && lo == a {
        out.extend(a..at);
    } else {
        out.extend((lo..at).rev());
    }
    out
}

/// Seconds of a comp time as milliseconds (sound keys).
fn ms(t: f64) -> i64 {
    let v = (t * 1000.0).round();
    if v.is_finite() { v.clamp(-1e15, 1e15) as i64 } else { 0 }
}

/// Seconds of sound fetched at once (EffectCraft sends at most 300 s per request, and a long
/// card's whole mix would be a huge reply), and the step those pieces start on.
const SOUND_WINDOW: f64 = 60.0;
const SOUND_STEP: f64 = 30.0;

/// The stretch of `c`'s sound (comp seconds) fetched for playhead `t`: all of it for a short
/// card, else the 60 s piece starting on the 30 s step at or before `t` (so the next piece is
/// there before the playhead leaves this one).
pub(crate) fn sound_window(c: &EcComp, t: f64) -> (f64, f64) {
    if c.span() <= SOUND_WINDOW * 1.5 {
        return (c.start, c.end);
    }
    let t = if t.is_finite() { t.clamp(c.start, c.end) } else { c.start };
    let k = ((t - c.start) / SOUND_STEP).floor().max(0.0);
    let a = c.start + k * SOUND_STEP;
    (a, (a + SOUND_WINDOW).min(c.end))
}

fn sound_key(c: &EcComp, w: (f64, f64)) -> SoundKey {
    (c.comp.clone(), c.comp_id, ms(w.0), ms(w.1))
}

/// The edit time (timeline seconds) the shared playhead is at: the playing card's, else the
/// audio strip's, else the paused card's.
fn timeline_now(app: &FigApp, doc: &Document) -> Option<f64> {
    let ec = &app.session.effectcraft;
    let card_t = ec.node.and_then(|n| doc.node(n)?.effectcraft.as_deref().map(|c| c.timeline_of(ec.time())));
    if ec.playing() {
        return card_t;
    }
    let au = &app.session.audio;
    let strip_t = au.node.and_then(|n| doc.node(n)?.audio.as_deref().map(|a| a.offset + au.time()));
    if au.playing() {
        return strip_t;
    }
    card_t.or(strip_t)
}

/// Comp time under the pointer `h` on card `id` (hover scrubbing), unless that card plays.
fn hover_time(app: &FigApp, doc: &Document, id: NodeId, c: &EcComp, h: Option<Pos2>) -> Option<f64> {
    let h = h?;
    let ec = &app.session.effectcraft;
    if ec.playing() && ec.node == Some(id) {
        return None;
    }
    let pic = fit(screen_rect(app, doc, id)?, c.width, c.height);
    if !pic.contains(h) {
        return None;
    }
    let u = f64::from(((h.x - pic.min.x) / pic.width()).clamp(0.0, 0.999));
    Some(c.start + u * c.span())
}

/// The comp time card `id` shows now.
fn card_time(app: &FigApp, doc: &Document, id: NodeId, c: &EcComp, hover: Option<Pos2>) -> f64 {
    let ec = &app.session.effectcraft;
    // The card the EffectCraft Timeline edits shows the one playhead they share (no skimming
    // under the pointer, no audio strip's time).
    #[cfg(feature = "effectcraft-inproc")]
    if let Some(p) = app.effectcraft.panel.as_ref().filter(|p| p.open && p.card == id) {
        return if ec.node == Some(id) { ec.time() } else { p.host.time() };
    }
    if ec.playing() && ec.node == Some(id) {
        return ec.time();
    }
    if let Some(t) = hover_time(app, doc, id, c, hover) {
        return t;
    }
    if let Some(t) = timeline_now(app, doc).filter(|t| c.covers_timeline(*t)) {
        return c.comp_time_of(t);
    }
    if ec.node == Some(id) {
        return ec.time();
    }
    app.effectcraft.shown.get(&id).copied().unwrap_or(c.start)
}

fn frame_at(c: &EcComp, t: f64) -> i64 {
    let r = c.frames();
    c.frame_of(t).clamp(*r.start(), *r.end())
}

/// Height kept free above a card for its title (the frame's name label).
const TITLE: f32 = 20.0;
/// Width kept for the frame readout left of a card's buttons.
const READOUT: f32 = 96.0;

/// A card's controls, in a strip above its title so nothing covers the picture (EffectCraft and
/// video cards share it): the scrub track along the left, the readout, then the play button and
/// the second button (explode, play all) at the right end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CardBar {
    /// The strip, and the stretch hovering counts as the card's (the strip and the title).
    pub row: Rect,
    pub zone: Rect,
    /// Where the scrub track runs (its whole height grabs it); `None` on a narrow card.
    pub track: Option<Rect>,
    /// The readout's right edge, and the buttons' centres.
    pub readout_right: f32,
    pub play: Pos2,
    pub second: Pos2,
}

/// The controls of a card whose picture shows in `pic` (`None` when it shows too small).
pub(crate) fn card_bar(pic: Rect) -> Option<CardBar> {
    if !(pic.width() > BUTTON * 6.0 && pic.height() > BUTTON * 3.0) {
        return None;
    }
    let cy = pic.min.y - TITLE - BUTTON - 2.0;
    let row = Rect::from_min_max(pos2(pic.min.x, cy - BUTTON - 2.0), pos2(pic.max.x, cy + BUTTON + 2.0));
    let second = pos2(pic.max.x - BUTTON, cy);
    let play = pos2(second.x - BUTTON * 2.0 - 4.0, cy);
    let readout_right = play.x - BUTTON - 6.0;
    let track_end = readout_right - READOUT - 8.0;
    let track = (track_end - pic.min.x >= 40.0).then(|| Rect::from_min_max(pos2(pic.min.x, row.min.y), pos2(track_end, row.max.y)));
    Some(CardBar { row, zone: Rect::from_min_max(row.min, pos2(pic.max.x, pic.min.y)), track, readout_right, play, second })
}

/// Whether screen point `p` is on a card bar's button centred at `b`.
pub(crate) fn on_button(b: Pos2, p: Pos2) -> bool {
    b.distance(p) <= BUTTON + 3.0
}

/// Whether screen point `p` is on the scrub track of a card showing in `pic`.
pub(crate) fn on_slider(pic: Rect, p: Pos2) -> bool {
    card_bar(pic).and_then(|b| b.track).is_some_and(|t| t.contains(p))
}

/// Draw a card bar's scrub track: `cached` of it held (a fraction; EffectCraft's cache bar) and
/// the playhead at fraction `at` (`grip`: grabbed or hovered, drawn thicker).
pub(crate) fn draw_track(painter: &egui::Painter, track: Rect, cached: Option<f32>, at: Option<f32>, grip: bool, active: bool) {
    let th = if grip {
        5.0
    } else if active {
        3.0
    } else {
        2.0
    };
    let bar = Rect::from_center_size(track.center(), vec2(track.width(), th));
    painter.rect_filled(bar, CornerRadius::same(1), Color32::from_white_alpha(28));
    if let Some(u) = cached {
        let x = bar.min.x + bar.width() * u.clamp(0.0, 1.0);
        painter.rect_filled(
            Rect::from_min_max(bar.min, pos2(x, bar.max.y)),
            CornerRadius::same(1),
            Color32::from_rgba_unmultiplied(0x2F, 0xB3, 0x6F, 170),
        );
    }
    if let Some(u) = at {
        let x = bar.min.x + bar.width() * u.clamp(0.0, 1.0);
        painter.rect_filled(Rect::from_min_max(bar.min, pos2(x, bar.max.y)), CornerRadius::same(1), Color32::from_white_alpha(70));
        painter.circle_filled(pos2(x, bar.center().y), if grip { 6.0 } else { 4.0 }, Color32::WHITE);
    }
}

/// Draw `text` as a readout chip ending at `right`, centred on `cy`.
pub(crate) fn readout(painter: &egui::Painter, right: f32, cy: f32, text: String, font: egui::FontId) {
    let g = painter.layout_no_wrap(text, font, Color32::WHITE);
    let bg = Rect::from_min_size(pos2(right - g.size().x - 8.0, cy - (g.size().y + 4.0) / 2.0), g.size() + vec2(8.0, 4.0));
    painter.rect_filled(bg, CornerRadius::same(3), Color32::from_black_alpha(170));
    painter.galley(bg.min + vec2(4.0, 2.0), g, Color32::WHITE);
}

/// The comp time at screen x `x` on a scrub track running along `track`.
pub(crate) fn slider_time(c: &EcComp, track: Rect, x: f32) -> f64 {
    let u = if track.width() > 0.0 { f64::from(((x - track.min.x) / track.width()).clamp(0.0, 1.0)) } else { 0.0 };
    c.start + u * c.span()
}

/// Whether card `id` has cards exploded out of it (its explode button then closes them).
fn exploded(doc: &Document, id: NodeId) -> bool {
    let key = id.to_string();
    doc.nodes().any(|n| n.effectcraft.is_some() && n.plugin_data.get(EXPLODED_FROM) == Some(&key))
}

pub(crate) fn pointer(painter: &egui::Painter) -> Option<Pos2> {
    painter.ctx().input(|i| i.pointer.hover_pos()).filter(|h| painter.clip_rect().contains(*h))
}

/// Draw each card's frame at the playhead, the frames still coming, and on hover its buttons.
/// Runs right after the canvas image, so selection outlines draw over it.
pub fn overlay(app: &FigApp, painter: &egui::Painter) {
    let Ok(ds) = app.session.ds() else { return };
    let doc = &ds.doc;
    let hover = pointer(painter);
    let ec = &app.session.effectcraft;
    let font = egui::FontId::proportional(11.0);
    let ids = cards(doc, ds.page);
    let mut covers = app.effectcraft.covers.try_borrow_mut().ok();
    if let Some(cv) = covers.as_mut() {
        cv.keep_only(&ids);
    }
    let want = ds.view.zoom * f64::from(painter.ctx().pixels_per_point());
    for id in ids {
        let Some(c) = doc.node(id).and_then(|n| n.effectcraft.as_deref()) else { continue };
        let Some(r) = screen_rect(app, doc, id) else { continue };
        if !r.intersects(painter.clip_rect()) {
            continue;
        }
        let pic = fit(r, c.width, c.height);
        let t = card_time(app, doc, id, c, hover);
        let f = frame_at(c, t);
        let drawn = match app.effectcraft.texture_near(c, f) {
            Some(tex) => {
                painter.rect_filled(r, CornerRadius::ZERO, Color32::from_rgb(0x0C, 0x0C, 0x0E));
                painter.image(tex.id(), pic, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                // Layers above the card in z-order stay above its frame.
                if let Some(cv) = covers.as_mut() {
                    cv.draw(painter, &ds.doc, id, want, |x, y| app.to_screen(x, y));
                }
                true
            }
            None => false,
        };
        // Fork: the selected layers' boxes and handles while the Timeline edits this card.
        #[cfg(feature = "effectcraft-inproc")]
        handles::draw(app, painter, id, c, pic, hover);
        let bar = card_bar(pic);
        let hovered = hover.is_some_and(|h| r.contains(h) || bar.is_some_and(|b| b.zone.contains(h)));
        let playing = ec.playing() && ec.node == Some(id);
        let scrubbing = app.effectcraft.scrub == Some(id);
        // The scrub track above the card: the frames held (EffectCraft's cache bar) and, on
        // hover, the playhead knob.
        let (have, total) = app.effectcraft.coverage(c);
        let active = hovered || playing || scrubbing;
        if let Some(track) = bar.and_then(|b| b.track)
            && (active || (total > 0 && have < total))
        {
            let grip = scrubbing || hover.is_some_and(|h| track.contains(h));
            let cached = (total > 0).then(|| have as f32 / total as f32);
            let at = (active && c.span() > 0.0).then(|| ((t - c.start) / c.span()) as f32);
            draw_track(painter, track, cached, at, grip, active);
        }
        if !drawn && let Some(why) = app.effectcraft.down() {
            let project = std::path::Path::new(&c.project).file_name().map(|f| f.to_string_lossy().into_owned()).filter(|f| !f.is_empty());
            let msg = match project {
                Some(p) => format!("{why} · open {p} in EffectCraft to play"),
                None => format!("{why} · open the project in EffectCraft to play"),
            };
            chip(painter, pos2(pic.min.x + 6.0, pic.max.y - 22.0), msg, font.clone(), Color32::from_black_alpha(190));
        }
        if hovered || playing {
            let from = if app.effectcraft.headless && app.effectcraft.down().is_some() { "  · saved file" } else { "" };
            if let Some(bar) = bar {
                let label = format!("f{}  {}{from}", f, crate::audio_ui::timecode(c.timeline_of(t)));
                readout(painter, bar.readout_right, bar.play.y, label, font.clone());
            }
            if let Some((play, explode)) = bar.map(|b| (b.play, b.second)) {
                round_button(painter, play);
                let fg = Color32::WHITE;
                if playing {
                    for dx in [-3.5, 1.5] {
                        painter.rect_filled(Rect::from_min_size(play + vec2(dx, -4.5), vec2(2.5, 9.0)), CornerRadius::ZERO, fg);
                    }
                } else {
                    painter.add(egui::Shape::convex_polygon(
                        vec![play + vec2(-3.0, -5.0), play + vec2(5.0, 0.0), play + vec2(-3.0, 5.0)],
                        fg,
                        Stroke::NONE,
                    ));
                }
                if hovered {
                    // Explode: four small boxes, the comps inside; lit while they are out (a
                    // press closes them).
                    let open = exploded(doc, id);
                    round_button(painter, explode);
                    if open {
                        painter.circle_filled(explode, BUTTON - 1.0, crate::theme::canvas_accent(painter.ctx()));
                    }
                    for (dx, dy) in [(-4.5, -4.5), (0.5, -4.5), (-4.5, 0.5), (0.5, 0.5)] {
                        let r = Rect::from_min_size(explode + vec2(dx, dy), vec2(4.0, 4.0));
                        if open {
                            painter.rect_filled(r, CornerRadius::same(1), fg);
                        } else {
                            painter.rect_stroke(r, CornerRadius::same(1), Stroke::new(1.0, fg), egui::StrokeKind::Inside);
                        }
                    }
                }
            }
        }
    }
    // Fork: the closed Timeline strips under the cards whose Timeline isn't open.
    #[cfg(feature = "effectcraft-inproc")]
    strips::draw_closed(app, painter, doc, ds.page, hover);
    // Fork: "Caching n%" on a card rendering its play range.
    #[cfg(feature = "effectcraft-inproc")]
    cards::draw(app, painter);
}

pub(crate) fn round_button(painter: &egui::Painter, c: Pos2) {
    painter.circle_filled(c, BUTTON, Color32::from_black_alpha(170));
    painter.circle_stroke(c, BUTTON, Stroke::new(1.0, Color32::from_white_alpha(90)));
}

pub(crate) fn chip(painter: &egui::Painter, at: Pos2, text: String, font: egui::FontId, bg: Color32) {
    let g = painter.layout_no_wrap(text, font, Color32::WHITE);
    let r = Rect::from_min_size(at, g.size() + vec2(8.0, 4.0));
    painter.rect_filled(r, CornerRadius::same(3), bg);
    painter.galley(r.min + vec2(4.0, 2.0), g, Color32::WHITE);
}

/// The topmost card under screen point `pos`.
fn card_at(app: &FigApp, doc: &Document, page: NodeId, pos: Pos2) -> Option<(NodeId, Rect)> {
    cards(doc, page).into_iter().rev().find_map(|id| {
        let c = doc.node(id)?.effectcraft.as_deref()?;
        let r = screen_rect(app, doc, id)?;
        r.contains(pos).then(|| (id, fit(r, c.width, c.height)))
    })
}

/// The play (`true`) or explode button of the topmost card under screen point `pos` (the buttons
/// sit above their card).
fn button_at(app: &FigApp, doc: &Document, page: NodeId, pos: Pos2) -> Option<(NodeId, bool)> {
    cards(doc, page).into_iter().rev().find_map(|id| {
        let c = doc.node(id)?.effectcraft.as_deref()?;
        let bar = card_bar(fit(screen_rect(app, doc, id)?, c.width, c.height))?;
        if on_button(bar.play, pos) { Some((id, true)) } else { on_button(bar.second, pos).then_some((id, false)) }
    })
}

/// The topmost card whose scrub track (above it) is under screen point `pos`.
fn track_at(app: &FigApp, doc: &Document, page: NodeId, pos: Pos2) -> Option<NodeId> {
    cards(doc, page).into_iter().rev().find(|id| {
        let pic = doc.node(*id).and_then(|n| n.effectcraft.as_deref()).and_then(|c| Some(fit(screen_rect(app, doc, *id)?, c.width, c.height)));
        pic.is_some_and(|pic| on_slider(pic, pos))
    })
}

/// Whether the pointer belongs to the open EffectCraft Timeline (`panel`: its rectangles on
/// screen, Effect Controls included, as last drawn): over it, or pressed in it and still held (a
/// drag out of the Timeline). A press that began on the canvas (`canvas_drag`) keeps its moves and
/// release wherever they are.
#[cfg(feature = "effectcraft-inproc")]
pub(crate) fn panel_has_pointer(panel: &[Rect], hover: Option<Pos2>, held_from: Option<Pos2>, canvas_drag: bool) -> bool {
    let over = |p: Pos2| panel.iter().any(|r| r.contains(p));
    !canvas_drag && (hover.is_some_and(over) || held_from.is_some_and(over))
}

/// The canvas leaves this frame's pointer (clicks, drags, scroll) to the open Timeline on it:
/// only the Timeline acts on what happens over it (the canvas still zooms on ⌘-scroll and
/// pinch, [`crate::canvas`], and pans with Space held).
#[cfg(feature = "effectcraft-inproc")]
pub(crate) fn panel_takes_pointer(app: &FigApp, ctx: &egui::Context, canvas_drag: bool) -> bool {
    if app.canvas.space_pan || app.effectcraft.panel.as_ref().is_none_or(|p| !p.open) {
        return false;
    }
    // Drawn after the canvas: last pass's rectangles are the ones on screen now.
    let pass = ctx.cumulative_pass_nr();
    let rects = app.effectcraft.panel_rects.as_ref().filter(|(p, _)| p.saturating_add(1) >= pass).map_or(&[][..], |(_, r)| r.as_slice());
    let (hover, held_from) = ctx.input(|i| (i.pointer.hover_pos(), i.pointer.press_origin().filter(|_| i.pointer.any_down())));
    panel_has_pointer(rects, hover, held_from, canvas_drag)
}

/// Fork: the pointer on the layers of the card the Timeline edits ([`handles::input`]): a
/// press on a layer or handle there (not on the card's buttons or slider) and the drag that
/// follows. Returns whether it has the pointer (its own cursor shows).
#[cfg(feature = "effectcraft-inproc")]
pub fn layer_handles_input(app: &mut FigApp, ctx: &egui::Context, pos: Pos2, hovered: bool) -> bool {
    if app.effectcraft.picking || app.effectcraft.scrub.is_some() {
        return false;
    }
    let target = if app.effectcraft.handles.dragging() {
        None
    } else {
        let Some(card) = app.effectcraft.panel.as_ref().filter(|p| p.open).map(|p| p.card) else { return false };
        let Ok(ds) = app.session.ds() else { return false };
        let (doc, page) = (ds.doc.clone(), ds.page);
        let free = |pic: Rect| !card_bar(pic).is_some_and(|b| b.zone.contains(pos));
        card_at(app, &doc, page, pos).filter(|(id, pic)| *id == card && free(*pic))
    };
    handles::input(app, ctx, target, pos, hovered)
}

/// A press on a card's play or explode button or its slider (dragged, it scrubs), or a
/// double-click on a card (open its comp in EffectCraft at the frame shown). Returns whether the
/// pointer was used.
pub fn input(app: &mut FigApp, ctx: &egui::Context, pos: Pos2, hovered: bool, double: bool) -> bool {
    if let Some(id) = app.effectcraft.scrub {
        if ctx.input(|i| i.pointer.primary_down()) {
            scrub_to(app, id, pos);
        } else {
            app.effectcraft.scrub = None;
        }
        return true;
    }
    if !hovered {
        return false;
    }
    // The EffectCraft Timeline's eyedropper takes this click ([`FigApp::effectcraft_panel_ui`]).
    #[cfg(feature = "effectcraft-inproc")]
    if app.effectcraft.picking {
        return true;
    }
    let pressed = ctx.input(|i| i.pointer.button_pressed(egui::PointerButton::Primary));
    if !pressed && !double {
        return false;
    }
    let Ok(ds) = app.session.ds() else { return false };
    let (doc, page) = (ds.doc.clone(), ds.page);
    // Fork: a press on a closed Timeline strip opens that card's Timeline (another one closes).
    #[cfg(feature = "effectcraft-inproc")]
    if pressed && let Some(id) = strips::closed_at(app, &doc, page, pos) {
        if !app.effectcraft_open_panel(id) {
            let t = doc.node(id).and_then(|n| n.effectcraft.as_deref()).map(|c| app.effectcraft.shown.get(&id).copied().unwrap_or(c.start));
            app.run("effectcraft.reveal", json!({"id": id, "time": t}));
        }
        return true;
    }
    if pressed && let Some((id, play)) = button_at(app, &doc, page, pos) {
        app.run(if play { "effectcraft.togglePlay" } else { "effectcraft.explode" }, json!({"id": id}));
        return true;
    }
    if pressed && let Some(id) = track_at(app, &doc, page, pos) {
        app.effectcraft.scrub = Some(id);
        scrub_to(app, id, pos);
        return true;
    }
    let Some((id, _)) = card_at(app, &doc, page, pos) else { return false };
    if double {
        // Fork: EffectCraft's Timeline under the card, editing the card's comp.
        #[cfg(feature = "effectcraft-inproc")]
        if app.effectcraft_open_panel(id) {
            return true;
        }
        let t = doc.node(id).and_then(|n| n.effectcraft.as_deref()).map(|c| card_time(app, &doc, id, c, Some(pos)));
        app.run("effectcraft.reveal", json!({"id": id, "time": t}));
        return true;
    }
    false
}

/// Put card `id`'s playhead at the scrub-track point under `pos` (it plays on from there when it was
/// playing).
fn scrub_to(app: &mut FigApp, id: NodeId, pos: Pos2) {
    let Ok(doc) = app.session.doc() else { return };
    let Some(c) = doc.node(id).and_then(|n| n.effectcraft.as_deref()) else { return };
    let Some(r) = screen_rect(app, doc, id) else { return };
    let Some(track) = card_bar(fit(r, c.width, c.height)).and_then(|b| b.track) else { return };
    let t = slider_time(c, track, pos.x);
    app.effectcraft.shown.insert(id, t);
    app.run("effectcraft.seek", json!({"id": id, "time": t}));
}

/// The `effectcraft.place` steps for the comps nested in card `card` (`c`, its page box
/// `bounds` = x, y, width, height), from EffectCraft's `figcraft.tree` answer: one column per
/// level to the card's right, each card on the same edit time as the card it is in and marked as
/// out of `card` (pressing its explode button again closes them).
pub(crate) fn explode_steps(card: NodeId, c: &EcComp, bounds: (f64, f64, f64, f64), tree: &Value) -> Vec<(String, Value)> {
    let (x, y, w, _) = bounds;
    let cw = (w * 0.6).max(64.0);
    let gap = (w * 0.08).max(16.0);
    let mut rows: Vec<f64> = Vec::new();
    let mut out = Vec::new();
    walk(c, tree.get("children"), 1, &mut |child: &EcComp, depth: usize, name: String| {
        let ch = cw * child.height as f64 / child.width.max(1) as f64;
        let col = depth.saturating_sub(1);
        if rows.len() <= col {
            rows.resize(col + 1, y);
        }
        let cy = rows.get(col).copied().unwrap_or(y);
        if let Some(r) = rows.get_mut(col) {
            *r = cy + ch + gap;
        }
        let cx = x + w + gap + col as f64 * (cw + gap);
        out.push((
            "effectcraft.place".to_string(),
            json!({"comp": child.comp, "compId": child.comp_id, "project": child.project, "start": child.start, "end": child.end, "fps": child.fps,
                "width": child.width, "height": child.height, "offset": child.offset, "rate": child.rate, "sound": false, "name": name,
                "x": cx, "y": cy, "cardWidth": cw, "explodedFrom": card}),
        ));
    });
    out
}

/// Visit the comps in `children` (a `figcraft.tree` level) nested in `parent`, mapped onto its
/// edit time, depth first.
fn walk(parent: &EcComp, children: Option<&Value>, depth: usize, f: &mut dyn FnMut(&EcComp, usize, String)) {
    let Some(Value::Array(list)) = children else { return };
    if depth > 4 {
        return;
    }
    for ch in list.iter().take(64) {
        let num = |v: Option<&Value>| v.and_then(Value::as_f64).filter(|x| x.is_finite());
        let comp = ch.get("comp");
        let Some(name) = comp.and_then(|c| c.get("name")).and_then(Value::as_str) else { continue };
        let (ls, stretch) = (num(ch.get("start")).unwrap_or(0.0), num(ch.get("stretch")).unwrap_or(100.0));
        let (lin, lout) = (num(ch.get("in")).unwrap_or(parent.start), num(ch.get("out")).unwrap_or(parent.end));
        if stretch.abs() < 1e-6 {
            continue;
        }
        // The part of the layer the parent card shows, in parent comp seconds.
        let (a, b) = (parent.start.max(lin), parent.end.min(lout));
        if b <= a {
            continue;
        }
        let k = 100.0 / stretch;
        let (s, e) = ((a - ls) * k, (b - ls) * k);
        let fps = num(comp.and_then(|c| c.get("frameRate"))).filter(|f| *f > 0.0).unwrap_or(parent.fps);
        let mut child = EcComp::new(name, s.min(e), s.max(e), fps);
        child.comp_id = comp.and_then(|c| c.get("id")).and_then(Value::as_u64).unwrap_or(0);
        child.project = parent.project.clone();
        child.width = comp.and_then(|c| c.get("width")).and_then(Value::as_u64).map_or(parent.width, |v| v.clamp(1, 32_768) as u32);
        child.height = comp.and_then(|c| c.get("height")).and_then(Value::as_u64).map_or(parent.height, |v| v.clamp(1, 32_768) as u32);
        child.offset = parent.offset + ls / parent.rate;
        child.rate = parent.rate * k.abs();
        child.sound = false;
        child.sanitize();
        let remapped = ch.get("timeRemap").and_then(Value::as_bool).unwrap_or(false);
        let label = format!("{name} · in {}{}", parent.comp, if remapped { " (time remapped)" } else { "" });
        f(&child, depth, label);
        walk(&child, ch.get("children"), depth + 1, f);
    }
}

impl FigApp {
    /// Keep the cards fed: fetch the frames around each visible card's playhead, refresh them
    /// after edits in EffectCraft, sound the playing card, and keep one playhead with the audio
    /// strips. Runs every frame after the UI.
    pub(crate) fn effectcraft_sync(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        retire_native_frames();
        // The playhead's card went away (deleted, undone, another file).
        if let Some(n) = self.session.effectcraft.node
            && !self.session.doc().is_ok_and(|d| d.node(n).is_some_and(|n| n.effectcraft.is_some()))
        {
            self.session.effectcraft.clear();
        }
        // One sound at a time: what started playing last wins.
        let (au, ec) = (self.session.audio.playing(), self.session.effectcraft.playing());
        if au && ec {
            if !self.effectcraft.was.0 {
                self.session.effectcraft.pause();
            } else {
                self.session.audio.pause();
            }
        }
        self.effectcraft.was = (self.session.audio.playing(), self.session.effectcraft.playing());
        #[cfg(not(target_arch = "wasm32"))]
        self.effectcraft_replies(ctx, now);
        self.effectcraft_sound();
        self.effectcraft_follow();
        let Ok(ds) = self.session.ds() else { return };
        let doc = ds.doc.clone();
        let ids = cards(&doc, ds.page);
        if ids.is_empty() {
            return;
        }
        // Fork: the closed strips' layer bars.
        #[cfg(feature = "effectcraft-inproc")]
        {
            let cards: Vec<(NodeId, EcComp)> = ids.iter().filter_map(|id| Some((*id, doc.node(*id)?.effectcraft.as_deref().cloned()?))).collect();
            let comps: Vec<EcComp> = cards.iter().map(|(_, c)| c.clone()).collect();
            self.effectcraft.strips.feed(ctx, &comps, now);
            // Cards play by EffectCraft's preview rules: the comp's work area (live from the open
            // Timeline, else the saved project) and the Space preview options.
            let preset = self.effectcraft.strips.space_preset(now);
            for (id, c) in &cards {
                let open = self.effectcraft.panel.as_ref().filter(|p| p.card == *id).and_then(|p| p.host.work_area());
                let wa = open.or_else(|| self.effectcraft.strips.peek(c).map(|b| (b.work, b.duration)));
                let pv = wa.map(|(w, d)| strips::preview_of(&preset, w, d));
                self.session.effectcraft.set_preview(*id, pv);
            }
            // Cache Before Playback and the preview resolution.
            cards::cache_step(self, ctx, &preset, now);
        }
        let ppp = ctx.pixels_per_point();
        let hover = ctx.input(|i| i.pointer.hover_pos()).filter(|h| self.canvas.rect.contains(*h));
        let mut wanted: Vec<(NodeId, EcComp, u32, i64)> = Vec::new();
        for id in ids {
            let Some(c) = doc.node(id).and_then(|n| n.effectcraft.as_deref()) else { continue };
            let Some(r) = screen_rect(self, &doc, id) else { continue };
            let t = card_time(self, &doc, id, c, hover);
            self.effectcraft.shown.insert(id, t);
            if !r.intersects(self.canvas.rect) {
                continue;
            }
            let pic = fit(r, c.width, c.height);
            wanted.push((id, c.clone(), side_for(pic.width().max(pic.height()) * ppp, c), frame_at(c, t)));
        }
        // Fork: the comp viewer's / Space preview's resolution; a caching card's range.
        #[cfg(feature = "effectcraft-inproc")]
        cards::shape_wanted(self, &mut wanted);
        if self.session.effectcraft.playing() || self.effectcraft.in_flight > 0 {
            ctx.request_repaint();
        }
        #[cfg(not(target_arch = "wasm32"))]
        self.effectcraft_request(ctx, now, &doc, &wanted);
        #[cfg(target_arch = "wasm32")]
        let _ = (now, wanted);
    }

    /// Play the playing card's sound (macOS and Windows), restarting it on a seek, a new play, a
    /// loop round or the next piece of a long card's sound.
    fn effectcraft_sound(&mut self) {
        let t = &self.session.effectcraft;
        let (playing, generation, node, time) = (t.playing(), t.generation, t.node, t.time());
        let wrapped = time + 0.05 < self.effectcraft.last_time;
        self.effectcraft.last_time = time;
        let card = node.filter(|_| playing).and_then(|n| Some((n, self.session.doc().ok()?.node(n)?.effectcraft.as_deref().cloned()?)));
        let Some((node, c)) = card.filter(|(_, c)| c.sound) else {
            self.effectcraft.out_for = None;
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                self.effectcraft.out = None;
            }
            return;
        };
        let w = sound_window(&c, time);
        let want = (generation, node, sound_key(&c, w));
        if self.effectcraft.out_for.as_ref() == Some(&want) && !wrapped {
            return;
        }
        let Some(SoundState::Ready(sound)) = self.effectcraft.sounds.get(&want.2) else {
            // Not here yet (effectcraft_request asks for it): quiet until it is.
            self.effectcraft.out_for = None;
            #[cfg(any(target_os = "macos", target_os = "windows"))]
            {
                self.effectcraft.out = None;
            }
            return;
        };
        let sound = sound.clone();
        self.effectcraft.out_for = Some(want);
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            self.effectcraft.out = None;
            match crate::audio_ui::out::Output::start(&sound, (time - w.0).max(0.0), w.1 - w.0) {
                Ok(o) => self.effectcraft.out = Some(o),
                Err(e) => log::warn!("EffectCraft card sound: {e}"),
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let _ = sound;
    }

    /// While a card plays, the audio strip covering the same edit time follows it (quietly: the
    /// card's own mix is what sounds).
    fn effectcraft_follow(&mut self) {
        let t = &self.session.effectcraft;
        if !t.playing() || self.session.audio.playing() {
            return;
        }
        let Ok(doc) = self.session.doc() else { return };
        let Some(c) = t.node.and_then(|n| doc.node(n)?.effectcraft.as_deref()) else { return };
        let at = c.timeline_of(t.time());
        let Ok(page) = self.session.page() else { return };
        let strip = strips(doc, page).into_iter().find_map(|id| {
            let a = doc.node(id)?.audio.as_deref()?;
            let m = at - a.offset;
            (m >= a.start && m < a.end).then(|| (id, a.clone(), m))
        });
        if let Some((id, a, m)) = strip {
            self.session.audio.seek(id, &a, m);
        }
    }

    /// The UI's requests: explode a card's precomps, open a card's comp in EffectCraft.
    pub(crate) fn effectcraft_request_ui(&mut self, kind: &str, data: &Value) {
        let Some(id) = data.get("id").and_then(figcraft_engine::params::id_of) else { return };
        let Some(c) = self.session.doc().ok().and_then(|d| d.node(id)?.effectcraft.as_deref().cloned()) else { return };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let job = match kind {
                // The comps showing at the frame the card shows, not every precomp of its stretch
                // (a 10-minute MAIN holds dozens).
                "effectcraft.explode" => io::Job::Tree { card: id, comp: c.clone(), at: self.effectcraft.shown.get(&id).copied().unwrap_or(c.start) },
                _ => io::Job::Reveal { comp: c.clone(), time: data.get("time").and_then(Value::as_f64).unwrap_or(c.start) },
            };
            if !self.effectcraft_send(job) {
                self.toast("EffectCraft can't be reached");
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (kind, c);
            self.toast("EffectCraft comps play in the desktop app");
        }
    }
}

/// Fork-only: EffectCraft's Timeline on the canvas, under the card it edits
/// ([`crate::effectcraft_panel`], [`strips`]).
#[cfg(feature = "effectcraft-inproc")]
impl FigApp {
    /// Open the Timeline on card `id`'s comp. `false` when it can't (no saved project, the comp
    /// isn't in it): the caller opens the comp in the EffectCraft app instead.
    pub(crate) fn effectcraft_open_panel(&mut self, id: NodeId) -> bool {
        let Some(c) = self.session.doc().ok().and_then(|d| d.node(id)?.effectcraft.as_deref().cloned()) else { return false };
        let path = std::path::PathBuf::from(&c.project);
        if c.project.is_empty() || !path.is_file() {
            return false;
        }
        let key = figcraft_ec_inproc::key(&path);
        if let Some(p) = self.effectcraft.panel.as_mut()
            && p.host.path() == key
            && p.card == id
            && !p.host.stale()
        {
            p.open = true;
            p.focused = true;
            return true;
        }
        if let Some(p) = &self.effectcraft.panel
            && p.host.is_dirty()
        {
            self.toast("Save the EffectCraft edits first (⌘S in its Timeline)");
            if let Some(p) = self.effectcraft.panel.as_mut() {
                p.open = true;
            }
            return true;
        }
        // The app has the project open: the panel mirrors its project (edits land there).
        let opened = match io::app_state_now(&key) {
            io::AppState::Owns => crate::effectcraft_panel::EcHost::open_mirror_tcp(&key, &c, self.ui_context.clone()),
            state => crate::effectcraft_panel::EcHost::open(&key, &c, state == io::AppState::Unknown),
        };
        match opened {
            Ok(host) => {
                if let Some(old) = self.effectcraft.panel.take() {
                    self.effectcraft_send(io::Job::Override { path: old.path(), snapshot: None });
                }
                self.effectcraft.panel = Some(crate::effectcraft_panel::Panel { host, card: id, open: true, focused: true, synced: None });
                true
            }
            Err(e) => {
                log::warn!("EffectCraft Timeline: {e}");
                false
            }
        }
    }

    /// What the canvas shows under the pointer, for an armed eyedropper: over a card of the
    /// panel's project, its comp frame there as the panel's session renders it; elsewhere the
    /// document's pixel. Sampled only on the click that takes it (the colour is unused before).
    fn effectcraft_canvas_color(
        &self,
        ctx: &egui::Context,
        p: &crate::effectcraft_panel::Panel,
        panel: &[Rect],
    ) -> Option<effectcraft_ui_egui::panels::timeline::PointerColor> {
        use effectcraft_ui_egui::panels::timeline::PointerColor;
        let pos = ctx.input(|i| i.pointer.hover_pos())?;
        if !self.canvas.rect.contains(pos) || panel.iter().any(|r| r.contains(pos)) {
            return None;
        }
        let clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary));
        if !clicked {
            return Some(PointerColor { rgb: [0.0; 3], clicked: false });
        }
        let ds = self.session.ds().ok()?;
        let (doc, page) = (ds.doc.clone(), ds.page);
        if let Some((id, pic)) = card_at(self, &doc, page, pos)
            && let Some(c) = doc.node(id).and_then(|n| n.effectcraft.as_deref())
            && pic.contains(pos)
            && figcraft_ec_inproc::key(std::path::Path::new(&c.project)).as_path() == p.host.path()
        {
            let (u, v) = (f64::from((pos.x - pic.min.x) / pic.width().max(1.0)), f64::from((pos.y - pic.min.y) / pic.height().max(1.0)));
            let comp = if c.comp_id > 0 { json!(c.comp_id) } else { json!(c.comp) };
            let f = frame_at(c, card_time(self, &doc, id, c, Some(pos)));
            let rgb = p.host.comp_color(&comp, f, u * f64::from(c.width), v * f64::from(c.height))?;
            return Some(PointerColor { rgb, clicked: true });
        }
        let at = self.to_page(pos);
        let ids = doc.node(page).map(|n| n.children.clone()).unwrap_or_default();
        let pm = figcraft_render::render_area(&doc, &ids, figcraft_geom::Rect::new(at.x, at.y, at.x + 1.0, at.y + 1.0), 1.0)?;
        let px = pm.pixel(0, 0)?.demultiply();
        // Nothing drawn there: the canvas colour.
        let rgb = if px.alpha() == 0 {
            let c = crate::theme::Tokens::get(ctx).canvas;
            [f32::from(c.r()) / 255.0, f32::from(c.g()) / 255.0, f32::from(c.b()) / 255.0]
        } else {
            [f32::from(px.red()) / 255.0, f32::from(px.green()) / 255.0, f32::from(px.blue()) / 255.0]
        };
        Some(PointerColor { rgb, clicked: true })
    }

    /// One playhead for the card and its Timeline: whichever moved since they were last
    /// made the same drags the other (the Timeline wins a tie: it was scrubbed by hand).
    fn effectcraft_sync_playhead(&mut self, p: &mut crate::effectcraft_panel::Panel) {
        let Some(c) = self.session.doc().ok().and_then(|d| d.node(p.card)?.effectcraft.as_deref().cloned()) else { return };
        let t = &self.session.effectcraft;
        let card_t = if t.node == Some(p.card) { t.time() } else { self.effectcraft.shown.get(&p.card).copied().unwrap_or(c.start) };
        let ec_t = p.host.time();
        let half = 0.5 / c.fps.max(1.0);
        let (card_t, ec_t) = match p.synced {
            Some((_, last_ec)) if (ec_t - last_ec).abs() > half => {
                // Scrubbed in the Timeline: the card follows.
                self.run("effectcraft.seek", json!({"id": p.card, "time": ec_t}));
                self.effectcraft.shown.insert(p.card, ec_t);
                (ec_t, ec_t)
            }
            Some((last_card, _)) if (card_t - last_card).abs() <= half => return,
            // The card moved (played, scrubbed on the canvas) or first sync: the Timeline follows.
            _ => {
                p.host.set_time(card_t);
                (card_t, p.host.time())
            }
        };
        p.synced = Some((card_t, ec_t));
        if self.session.effectcraft.playing()
            && let Some(ctx) = self.ui_context.clone()
        {
            ctx.request_repaint();
        }
    }

    /// The open Timeline's keys while it has the keyboard (the last click was in it), before the
    /// canvas and FigCraft's shortcuts see them: a tap of Space plays the card (held with a drag,
    /// it pans the canvas), ⌘S saves, the rest is EffectCraft's
    /// ([`crate::effectcraft_panel::EcHost::keys`]).
    pub(crate) fn effectcraft_panel_keys(&mut self, ctx: &egui::Context) {
        let Some(mut p) = self.effectcraft.panel.take() else { return };
        if let Some(dragged) = self.effectcraft.space_tap {
            let (down, held) = ctx.input(|i| (i.key_down(egui::Key::Space), i.pointer.any_down()));
            if held {
                self.effectcraft.space_tap = Some(true);
            } else if !down {
                self.effectcraft.space_tap = None;
                if !dragged && p.open {
                    self.run("effectcraft.togglePlay", json!({"id": p.card}));
                }
            }
        }
        if p.open && p.focused {
            let mut save = false;
            for a in p.host.keys(ctx) {
                match a {
                    crate::effectcraft_panel::PanelAction::Play => {
                        self.effectcraft.space_tap.get_or_insert(false);
                    }
                    crate::effectcraft_panel::PanelAction::Save => save = true,
                    crate::effectcraft_panel::PanelAction::Ran(_) => {}
                }
            }
            if save {
                self.effectcraft_save(&mut p);
            }
        }
        self.effectcraft.panel = Some(p);
    }

    /// Save the Timeline's project (asking the app first: it may have opened it since).
    fn effectcraft_save(&mut self, p: &mut crate::effectcraft_panel::Panel) {
        if p.host.mirrored() {
            match p.host.save(true) {
                Ok(()) => self.toast("Saving in EffectCraft"),
                Err(e) => self.toast(e),
            }
            return;
        }
        let owns = !matches!(io::app_state_now(p.host.path()), io::AppState::Closed | io::AppState::Other);
        match p.host.save(owns) {
            Ok(()) => self.toast("Saved the EffectCraft project"),
            Err(e) => self.toast(e),
        }
    }

    /// The open Timeline on the canvas, under its card ([`strips`]); drawn after the canvas, so
    /// it sits over it where the card is this frame. Its header (comp, "● unsaved" and Save, or
    /// why it's read-only; ✕ closes it), EffectCraft's Timeline, Effect Controls to its right, the
    /// right and bottom edges (their corner, Effect Controls' right edge) that size them, the keyboard focus, and the card following its edits.
    pub(crate) fn effectcraft_panel_ui(&mut self, ctx: &egui::Context) {
        cards::frame(self, ctx);
        let Some(mut p) = self.effectcraft.panel.take() else {
            self.effectcraft.panel_rects = None;
            return;
        };
        // A mirror of the app's project: its edits out, the app's in. The card's frames come from
        // the app: ask for its revision now.
        if p.host.mirrored() {
            if p.host.sync() {
                self.effectcraft.revision_asked = f64::NEG_INFINITY;
            }
            if p.open {
                ctx.request_repaint_after(crate::effectcraft_panel::mirror::PULL_EVERY);
            }
        }
        if !p.open {
            self.effectcraft.panel_rects = None;
            self.effectcraft.panel = Some(p);
            return;
        }
        let t = crate::theme::Tokens::get(ctx);
        let (geom, comp) = match self.session.doc() {
            Ok(d) => (
                strips::geom_of(d, p.card, true, 0),
                d.node(p.card).and_then(|n| n.effectcraft.as_deref()).map(|c| c.comp.clone()).unwrap_or_default(),
            ),
            Err(_) => (None, String::new()),
        };
        let mut close = false;
        let mut save = false;
        let mut screen: Vec<Rect> = Vec::new();
        let mut fx_screen: Option<Rect> = None;
        let mut zones: Vec<Rect> = Vec::new();
        if let Some((g, xf)) = geom.and_then(|g| Some((g, strips::layer_transform(self, g.k)?))) {
            // One area in a layer egui scales and moves like the canvas: painting and input.
            let id = egui::Id::new("effectcraft_canvas_timeline");
            ctx.set_transform_layer(egui::LayerId::new(egui::Order::Middle, id), xf);
            let strip = strips::to_panel(g.strip, g.k);
            let fx = g.fx.map(|f| strips::to_panel(f, g.k));
            let whole = fx.map_or(strip, |f| strip.union(f));
            let canvas = self.canvas.rect;
            let inv = xf.inverse();
            let clip = Rect::from_two_pos(inv * canvas.min, inv * canvas.max);
            let visible = |r: Rect| Some((xf * r).intersect(canvas)).filter(|r| r.is_positive());
            screen = [Some(strip), fx].into_iter().flatten().filter_map(visible).collect();
            fx_screen = fx.and_then(visible);
            // Space held pans the canvas and a pinch or ⌘-scroll zooms it: the Timeline sits out.
            let zooming = ctx.input(|i| (i.zoom_delta() - 1.0).abs() > 1e-4);
            // The resize edges first, on the raw pointer: hovered or dragged, they win over the
            // panels under them (which sit the frame out).
            let (s_strip, s_fx) = (xf * strip, fx.map(|f| xf * f));
            let hot = strips::edges_input(self, ctx, p.card, &g, s_strip, s_fx);
            zones = strips::zones(s_strip, s_fx).into_iter().map(|r| r.intersect(canvas)).filter(|r| r.is_positive()).collect();
            let interactable = !self.canvas.space_pan && !zooming && hot.is_none();
            egui::Area::new(id).order(egui::Order::Middle).fixed_pos(whole.min).constrain(false).movable(false).interactable(interactable).show(
                ctx,
                |ui| {
                    ui.set_clip_rect(clip);
                    let painter = ui.painter().clone();
                    for r in [Some(strip), fx].into_iter().flatten() {
                        painter.rect_filled(r, CornerRadius::same(4), t.panel);
                        painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, t.border), egui::StrokeKind::Outside);
                    }
                    let head = Rect::from_min_size(strip.min, vec2(strip.width(), strips::HEADER));
                    painter.hline(head.x_range(), head.max.y, Stroke::new(1.0, t.border));
                    let bar = egui::UiBuilder::new().max_rect(head.shrink2(vec2(6.0, 1.0))).layout(egui::Layout::left_to_right(egui::Align::Center));
                    ui.scope_builder(bar, |ui| {
                        ui.label("▾");
                        ui.strong(&comp);
                        viewer::tool_row(ui, &mut p.host);
                        if let Some(why) = p.host.read_only() {
                            ui.label(egui::RichText::new(why).color(t.text_secondary));
                        } else if p.host.is_dirty() {
                            ui.label("● unsaved");
                            save = ui.button("Save").clicked();
                        } else if p.host.mirrored() {
                            ui.label(egui::RichText::new("⇄ live in EffectCraft").color(t.text_secondary))
                                .on_hover_text("Edits here are EffectCraft's: its undo, its unsaved changes, its ⌘S");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close = ui.button("✕").on_hover_text("Close the Timeline").clicked();
                        });
                    });
                    let body = Rect::from_min_max(pos2(strip.min.x, head.max.y + 1.0), strip.max);
                    p.host.show(ui, body, fx);
                    ui.advance_cursor_after_rect(whole);
                },
            );
            // The edge hovered or dragged: its cursor, an accent line along it, a corner's grip.
            if let Some(edge) = hot {
                ctx.set_cursor_icon(edge.cursor());
                let layer = egui::LayerId::new(egui::Order::Foreground, id.with("edges"));
                strips::draw_edge(&ctx.layer_painter(layer).with_clip_rect(canvas), s_strip, s_fx, edge);
            }
        }
        // The canvas leaves the pointer over the panels and their resize zones to them.
        let owned: Vec<Rect> = screen.iter().copied().chain(zones).collect();
        self.effectcraft.panel_rects = (!owned.is_empty()).then(|| (ctx.cumulative_pass_nr(), owned));
        // EffectCraft's viewer tools on the card (over the canvas, under nothing of the panel's).
        viewer::card_tools(self, ctx, &mut p);
        // The Timeline has the keyboard from a click in it, or on its card (moving or scaling a
        // layer there is an EffectCraft edit: ⌘Z undoes it), until a click elsewhere (not one in
        // a menu it opened).
        let card = self.session.doc().ok().and_then(|d| screen_rect(self, d, p.card));
        if !egui::Popup::is_any_open(ctx)
            && let Some(press) = ctx.input(|i| if i.pointer.any_pressed() { i.pointer.interact_pos() } else { None })
        {
            p.focused = screen.iter().chain(card.as_ref()).any(|r| r.contains(press));
            if p.focused {
                let fx = fx_screen.is_some_and(|r| r.contains(press));
                p.host.set_focus(if fx {
                    // Effect Controls or the Project tab beside them.
                    project::side_panel(ctx, &p.host)
                } else {
                    effectcraft_ui_egui::dock::PanelKind::Timeline
                });
            }
        }
        if save {
            self.effectcraft_save(&mut p);
        }
        // Edits re-render the card from this session (pictures, sound and precomps).
        let rev = effectcraft_ui_egui::panels::timeline::TimelineHost::session(&p.host).revision;
        if p.host.renders_here() && rev != p.host.shown_revision {
            p.host.shown_revision = rev;
            let snapshot = p.host.snapshot();
            self.effectcraft_want(&p.path(), Some(snapshot.revision));
            self.effectcraft_send(io::Job::Override { path: p.path(), snapshot: Some(Box::new(snapshot)) });
            self.effectcraft.sounds.retain(|_, s| matches!(s, SoundState::Loading));
        }
        self.effectcraft_sync_playhead(&mut p);
        // The colour eyedropper samples FigCraft's canvas under the pointer.
        let pointer = if p.host.picking() { self.effectcraft_canvas_color(ctx, &p, &screen) } else { None };
        self.effectcraft.picking = p.host.pick(ctx, pointer);
        if close {
            p.open = false;
            self.effectcraft.panel_rects = None;
            if !p.host.is_dirty() {
                self.effectcraft_send(io::Job::Override { path: p.path(), snapshot: None });
                return;
            }
            self.toast("The EffectCraft edits are kept: click its strip to save them");
        }
        self.effectcraft.panel = Some(p);
    }
}

/// The audio strips on `page` (the follow above looks for one under the playhead).
fn strips(doc: &Document, page: NodeId) -> Vec<NodeId> {
    doc.nodes().filter(|n| n.audio.is_some() && n.visible && doc.page_of(n.id) == Some(page)).map(|n| n.id).collect()
}

#[cfg(not(target_arch = "wasm32"))]
impl FigApp {
    /// The cards of the project at `path` show the panel's edits at `revision` (None: the file's).
    #[cfg(feature = "effectcraft-inproc")]
    fn effectcraft_want(&mut self, path: &std::path::Path, revision: Option<u64>) {
        for (k, s) in self.effectcraft.stores.iter_mut() {
            if !k.project.is_empty() && figcraft_ec_inproc::key(std::path::Path::new(&k.project)).as_path() == path {
                s.want = revision;
            }
        }
    }

    fn effectcraft_send(&mut self, job: io::Job) -> bool {
        if self.effectcraft.io.is_none()
            && let Some(ctx) = self.ui_context.clone()
        {
            self.effectcraft.io = io::Io::start(ctx);
        }
        self.effectcraft.io.as_ref().is_some_and(|io| io.tx.send(job).is_ok())
    }

    /// Take what the link thread sent: frames, sounds, the revision, trees, failures.
    fn effectcraft_replies(&mut self, ctx: &egui::Context, now: f64) {
        let Some(io) = self.effectcraft.io.as_ref() else { return };
        let replies: Vec<io::Reply> = io.rx.try_iter().collect();
        for r in replies {
            match r {
                io::Reply::Frames { store, revision, got, pending, failed, poster, headless, file } => {
                    self.effectcraft.in_flight = self.effectcraft.in_flight.saturating_sub(1);
                    match headless {
                        // Rendered from the saved file: a newer save makes the earlier ones stale.
                        Some(stamp) => {
                            if self.effectcraft.file_stamp.is_some_and(|s| s != stamp) {
                                for s in self.effectcraft.stores.values_mut() {
                                    s.frames.retain(|_, x| x.rev != 0);
                                }
                                self.effectcraft.bytes = self.effectcraft.stores.values().flat_map(|s| s.frames.values()).map(|x| x.bytes).sum();
                            }
                            self.effectcraft.file_stamp = Some(stamp);
                            self.effectcraft.headless = true;
                        }
                        None => {
                            self.effectcraft.down = None;
                            self.effectcraft.headless = false;
                            if revision > self.effectcraft.revision {
                                self.effectcraft.revision = revision;
                            }
                        }
                    }
                    let s = self.effectcraft.stores.entry(store).or_default();
                    // In-process frames of an older save of the file go.
                    if file.is_some() && s.file.is_some() && s.file != file {
                        let dropped: usize = s.frames.values().map(|x| x.bytes).sum();
                        s.frames.clear();
                        self.effectcraft.bytes = self.effectcraft.bytes.saturating_sub(dropped);
                    }
                    s.file = file;
                    for (f, tex) in got {
                        let [w, h] = tex.size();
                        let bytes = w * h * 4;
                        s.pending.remove(&f);
                        s.failed.remove(&f);
                        if let Some(old) = s.frames.insert(f, Slot { tex, rev: revision, bytes }) {
                            self.effectcraft.bytes = self.effectcraft.bytes.saturating_sub(old.bytes);
                        }
                        self.effectcraft.bytes += bytes;
                    }
                    for f in pending {
                        s.pending.insert(f, now);
                    }
                    for f in failed {
                        s.fail(f, now);
                    }
                    if let Some((id, bytes)) = poster
                        && self.session.doc().is_ok_and(|d| d.node(id).is_some_and(|n| n.effectcraft.is_some()))
                    {
                        use base64::Engine as _;
                        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                        let _ = self.session.execute("effectcraft.setPoster", &json!({"id": id, "data": data}));
                    }
                }
                io::Reply::FramesFailed { store, frames, error } => {
                    self.effectcraft.in_flight = self.effectcraft.in_flight.saturating_sub(1);
                    let s = self.effectcraft.stores.entry(store).or_default();
                    for f in frames {
                        s.fail(f, now);
                    }
                    log::warn!("EffectCraft frames: {error}");
                }
                io::Reply::Sound { key, sound } => {
                    let state = match sound {
                        Some(s) => SoundState::Ready(s),
                        None => SoundState::Silent,
                    };
                    // Sound that arrives while its card plays starts on the next frame.
                    self.effectcraft.sounds.insert(key, state);
                }
                io::Reply::Revision(r) => {
                    self.effectcraft.revision_out = false;
                    self.effectcraft.down = None;
                    if r != self.effectcraft.revision {
                        self.effectcraft.revision = r;
                        // Edited in EffectCraft: the sound is asked for again too.
                        self.effectcraft.sounds.retain(|_, s| matches!(s, SoundState::Loading));
                    }
                }
                io::Reply::Tree { card, tree } => self.effectcraft_explode(card, &tree),
                io::Reply::Revealed(comp) => self.toast(format!("{comp} is open in EffectCraft")),
                io::Reply::AppProject { card, path } => {
                    // Only a card still without a project (the user may have set one meanwhile).
                    let empty =
                        self.session.doc().ok().and_then(|d| d.node(card)?.effectcraft.as_deref().map(|c| c.project.is_empty())).unwrap_or(false);
                    if let Some(path) = path.filter(|_| empty) {
                        log::info!("EffectCraft card {card:?}: project {path} (open in EffectCraft)");
                        let _ = self.session.execute("effectcraft.set", &json!({"ids": [card], "project": path}));
                    }
                }
                io::Reply::Error(e) => self.toast(e),
                io::Reply::Down { error, kind } => {
                    match kind {
                        io::Kind::Frames => self.effectcraft.in_flight = self.effectcraft.in_flight.saturating_sub(1),
                        io::Kind::Revision => self.effectcraft.revision_out = false,
                        io::Kind::Sound(key) => {
                            self.effectcraft.sounds.remove(&key);
                        }
                        io::Kind::Other => self.toast(error.clone()),
                        // (an ask the user never made: no outage)
                        io::Kind::Quiet => continue,
                    }
                    for s in self.effectcraft.stores.values_mut() {
                        s.pending.clear();
                    }
                    self.effectcraft.down = Some((error, now));
                }
            }
        }
        if self.effectcraft.in_flight > 0 || self.effectcraft.stores.values().any(|s| !s.pending.is_empty()) {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(REPOLL));
        }
    }

    /// Ask for the frames the visible cards (`wanted`: card, comp, frame size, playhead frame)
    /// lack, the revision, the sounds and the posters.
    fn effectcraft_request(&mut self, ctx: &egui::Context, now: f64, doc: &Document, wanted: &[(NodeId, EcComp, u32, i64)]) {
        if let Some((_, since)) = self.effectcraft.down {
            if now - since < RETRY_DOWN {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(RETRY_DOWN));
                return;
            }
            self.effectcraft.down = None;
        }
        if wanted.is_empty() {
            return;
        }
        if !self.effectcraft.revision_out && now - self.effectcraft.revision_asked > REVISION_EVERY {
            self.effectcraft.revision_asked = now;
            self.effectcraft.revision_out = self.effectcraft_send(io::Job::Revision);
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(REVISION_EVERY));
        }
        let mut keep: HashSet<SoundKey> = HashSet::new();
        for (id, c, side, at) in wanted {
            if c.sound {
                // The piece of sound at the playhead, and the next one while the card plays.
                let w = sound_window(c, *at as f64 / c.fps);
                let mut pieces = vec![w];
                let ec = &self.session.effectcraft;
                if ec.playing() && ec.node == Some(*id) {
                    let next = sound_window(c, w.0 + SOUND_STEP);
                    if next != w {
                        pieces.push(next);
                    }
                }
                for p in pieces {
                    let key = sound_key(c, p);
                    keep.insert(key.clone());
                    if !self.effectcraft.sounds.contains_key(&key) {
                        self.effectcraft.sounds.insert(key.clone(), SoundState::Loading);
                        self.effectcraft_send(io::Job::Sound { key, comp: c.clone(), start: p.0, end: p.1 });
                    }
                }
            }
            // A card without a project takes the app's, when the app has its comp open.
            if c.project.is_empty() && self.effectcraft.project_asked.get(id).is_none_or(|t| now - t > FAILED_RETRY) {
                self.effectcraft.project_asked.insert(*id, now);
                self.effectcraft_send(io::Job::AppProject { card: *id, comp: c.clone() });
            }
            if self.effectcraft.in_flight >= IN_FLIGHT {
                continue;
            }
            let key = StoreKey::of(c, *side);
            let rev = self.effectcraft.revision;
            // While EffectCraft is closed any frame held is as new as the saved file allows.
            let headless = self.effectcraft.headless;
            let s = self.effectcraft.stores.entry(key.clone()).or_default();
            s.focus = *at;
            let mut frames = Vec::new();
            for f in order(c, *at) {
                if !wants(s, f, now, rev, headless) {
                    continue;
                }
                frames.push(f);
                if frames.len() >= BATCH {
                    break;
                }
            }
            if frames.is_empty() {
                continue;
            }
            for f in &frames {
                s.pending.insert(*f, now);
            }
            // The poster: the first frame fetched (the one at the playhead), once a session, for a
            // card without one.
            let first = frames.first().copied().unwrap_or(*at);
            let has_poster = doc.node(*id).is_some_and(|n| n.fills.iter().any(|p| matches!(p.kind, figcraft_color::PaintKind::Image(_))));
            let poster = (!has_poster && !self.effectcraft.posters.contains(id)).then_some(*id);
            if let Some(p) = poster {
                self.effectcraft.posters.insert(p);
            }
            if self.effectcraft_send(io::Job::Frames { store: key, comp: c.clone(), frames, side: *side, poster: poster.map(|p| (p, first)) }) {
                self.effectcraft.in_flight += 1;
            }
        }
        // Sound pieces nobody is near any more go (a long card's would pile up).
        if self.effectcraft.sounds.len() > 12 {
            self.effectcraft.sounds.retain(|k, s| matches!(s, SoundState::Loading) || keep.contains(k));
        }
        self.effectcraft_evict();
    }

    /// Keep the frame textures within the budget, dropping those farthest from where each card
    /// was last wanted.
    fn effectcraft_evict(&mut self) {
        while self.effectcraft.bytes > BUDGET {
            let far = self
                .effectcraft
                .stores
                .iter()
                .filter_map(|(k, s)| {
                    let (a, b) = (s.frames.keys().next()?, s.frames.keys().next_back()?);
                    let (da, db) = ((a - s.focus).abs(), (b - s.focus).abs());
                    Some(if da >= db { (da, k.clone(), *a) } else { (db, k.clone(), *b) })
                })
                .max_by_key(|(d, _, _)| *d);
            let Some((_, k, f)) = far else { break };
            let Some(slot) = self.effectcraft.stores.get_mut(&k).and_then(|s| s.frames.remove(&f)) else { break };
            self.effectcraft.bytes = self.effectcraft.bytes.saturating_sub(slot.bytes);
        }
    }

    /// Place a card for each comp nested in card `card`, from EffectCraft's tree, as one undo
    /// step.
    fn effectcraft_explode(&mut self, card: NodeId, tree: &Value) {
        let Ok(doc) = self.session.doc() else { return };
        let (Some(c), Some(b)) = (doc.node(card).and_then(|n| n.effectcraft.as_deref()).cloned(), doc.world_bounds(card)) else { return };
        let mut steps = explode_steps(card, &c, (b.x0, b.y0, b.x1 - b.x0, b.y1 - b.y0), tree);
        if steps.is_empty() {
            self.toast(format!("No comps are nested in {} at this frame", c.comp));
            return;
        }
        let total = steps.len();
        steps.truncate(MAX_EXPLODE);
        let n = steps.len();
        let more = if total > n { format!(" ({} more left out)", total - n) } else { String::new() };
        #[cfg(feature = "effectcraft-inproc")]
        let before = cards::on_page(self);
        match self.session.batch("Explode precomps", &steps) {
            Ok(_) => self.toast(format!("{n} nested comp{} placed next to {}{more}. ⌘Z takes them away", if n == 1 { "" } else { "s" }, c.comp)),
            Err(e) => self.toast(e.to_string()),
        }
        // Fork: the placed cards come into view.
        #[cfg(feature = "effectcraft-inproc")]
        cards::reveal_new(self, &before);
        self.handle_ui_requests();
    }
}

/// The connection to EffectCraft: one thread, one request at a time (EffectCraft answers each
/// connection in order), frames decoded into textures here, off the UI thread.
#[cfg(not(target_arch = "wasm32"))]
mod io {
    use std::io::{BufRead, BufReader, Write};
    use std::net::{SocketAddr, TcpStream};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use std::time::{Duration, Instant, UNIX_EPOCH};

    use figcraft_doc::{EcComp, NodeId};
    use serde_json::{Value, json};

    use super::{SoundKey, StoreKey};

    /// Frames asked for in one `render.frame` round (an EffectCraft without `figcraft.frames`
    /// renders each from scratch).
    const LEGACY_BATCH: usize = 3;
    /// Biggest decoded frame accepted (pixels).
    const MAX_PIXELS: usize = 4096 * 4096;

    #[derive(Clone)]
    pub enum Job {
        Frames {
            store: StoreKey,
            comp: EcComp,
            frames: Vec<i64>,
            side: u32,
            poster: Option<(NodeId, i64)>,
        },
        Sound {
            key: SoundKey,
            comp: EcComp,
            start: f64,
            end: f64,
        },
        Revision,
        Tree {
            card: NodeId,
            comp: EcComp,
            at: f64,
        },
        Reveal {
            comp: EcComp,
            time: f64,
        },
        /// The EffectCraft app's open project, when it has card `card`'s comp (a card placed
        /// without a project gets it).
        AppProject {
            card: NodeId,
            comp: EcComp,
        },
        /// Frames of the project at `path` render from this snapshot (a hosted EffectCraft panel's
        /// unsaved edits) instead of the saved file; `None` goes back to the file.
        #[cfg(feature = "effectcraft-inproc")]
        Override {
            path: PathBuf,
            snapshot: Option<Box<figcraft_ec_inproc::Snapshot>>,
        },
    }

    /// What a failed job was, so the UI can stop waiting for it.
    pub enum Kind {
        Frames,
        Revision,
        Sound(SoundKey),
        /// Asked quietly (nothing to tell the user when the app can't be reached).
        Quiet,
        Other,
    }

    pub enum Reply {
        /// `headless`: rendered from the saved project (EffectCraft closed), whose modification
        /// time (seconds) it carries.
        Frames {
            store: StoreKey,
            revision: u64,
            got: Vec<(i64, super::FrameTex)>,
            pending: Vec<i64>,
            failed: Vec<i64>,
            poster: Option<(NodeId, Vec<u8>)>,
            headless: Option<u64>,
            /// Rendered in-process from the card's saved project (`effectcraft-inproc`): that
            /// file's revision (a later save replaces this store's frames).
            file: Option<u64>,
        },
        FramesFailed {
            store: StoreKey,
            frames: Vec<i64>,
            error: String,
        },
        Sound {
            key: SoundKey,
            sound: Option<Arc<figcraft_audio::Sound>>,
        },
        Revision(u64),
        Tree {
            card: NodeId,
            tree: Value,
        },
        Revealed(String),
        /// `path`: the app's open project, which has the card's comp.
        AppProject {
            card: NodeId,
            path: Option<String>,
        },
        Error(String),
        Down {
            error: String,
            kind: Kind,
        },
    }

    pub struct Io {
        pub tx: Sender<Job>,
        pub rx: Receiver<Reply>,
    }

    #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
    impl Job {
        /// The card's comp, for the jobs that render or read it (not a reveal: that opens the
        /// comp in the EffectCraft app).
        fn comp(&self) -> Option<&EcComp> {
            match self {
                Job::Frames { comp, .. } | Job::Sound { comp, .. } | Job::Tree { comp, .. } => Some(comp),
                Job::Revision | Job::Reveal { .. } | Job::AppProject { .. } => None,
                #[cfg(feature = "effectcraft-inproc")]
                Job::Override { .. } => None,
            }
        }
    }

    /// Where a card's requests go.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Route {
        /// The running EffectCraft app's control channel (or, when it's closed, `effectcraft-cli`).
        Tcp,
        /// EffectCraft's engine inside FigCraft, on the card's saved project.
        InProc,
    }

    /// What the EffectCraft app has to do with a card's project.
    #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum AppState {
        /// Not running (nothing answers on its port).
        Closed,
        /// Running with the card's project open: it has the live, unsaved edits.
        Owns,
        /// Running with another project (or an untitled one) open.
        Other,
        /// Running, but it didn't say which project it has open (an older EffectCraft).
        Unknown,
    }

    /// What the app said it has open, as last asked.
    #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub(crate) enum Seen {
        Closed,
        /// The open project (canonical path; `None` when untitled).
        Open(Option<PathBuf>),
        Unknown,
    }

    /// The app's relation to `project` (a canonical path) given what it said it has open.
    #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
    pub(crate) fn state_of(seen: &Seen, project: &Path) -> AppState {
        match seen {
            Seen::Closed => AppState::Closed,
            Seen::Unknown => AppState::Unknown,
            Seen::Open(Some(p)) if p == project => AppState::Owns,
            Seen::Open(_) => AppState::Other,
        }
    }

    /// Where a card's requests go: in-process when that's built in and the card's saved project
    /// is there, unless the EffectCraft app has that project open (its live edits win) or can't
    /// say what it has open; otherwise the app, as before.
    #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
    pub(crate) fn choose(in_process: bool, project_is_file: bool, app: AppState) -> Route {
        if !in_process || !project_is_file {
            return Route::Tcp;
        }
        match app {
            AppState::Closed | AppState::Other => Route::InProc,
            AppState::Owns | AppState::Unknown => Route::Tcp,
        }
    }

    /// Seconds between asks of which project the app has open.
    #[cfg(feature = "effectcraft-inproc")]
    const PROBE_EVERY: Duration = Duration::from_secs(2);

    /// Ask the EffectCraft app now (its own connection) what it has to do with `project`.
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) fn app_state_now(project: &Path) -> AppState {
        let seen = match Conn::open(port()) {
            Err(_) => Seen::Closed,
            Ok(mut c) => match c.call("engine.execute", summary()) {
                Ok(v) => seen_of(&v),
                Err(CallError::Remote(_)) => Seen::Unknown,
                Err(CallError::Io(_)) => Seen::Closed,
            },
        };
        state_of(&seen, &figcraft_ec_inproc::key(project))
    }

    impl Io {
        pub fn start(ctx: egui::Context) -> Option<Io> {
            let (jtx, jrx) = channel::<Job>();
            let (rtx, rrx) = channel::<Reply>();
            std::thread::Builder::new()
                .name("effectcraft-link".into())
                .spawn(move || {
                    let mut link = Thread::new(ctx, port(), rtx.clone());
                    while let Ok(job) = jrx.recv() {
                        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| link.run(job)));
                        // `None`: handed to the in-process frame worker, which answers itself.
                        let Some(reply) = r.unwrap_or_else(|_| Some(Reply::Error("EffectCraft link: an internal error".into()))) else { continue };
                        if rtx.send(reply).is_err() {
                            break;
                        }
                        link.ctx.request_repaint();
                    }
                })
                .ok()?;
            Some(Io { tx: jtx, rx: rrx })
        }
    }

    fn port() -> u16 {
        std::env::var("EFFECTCRAFT_CONTROL_PORT").ok().and_then(|p| p.trim().parse().ok()).unwrap_or(9877)
    }

    enum CallError {
        Io(String),
        Remote(String),
    }

    struct Conn {
        w: TcpStream,
        r: BufReader<TcpStream>,
        next: u64,
    }

    impl Conn {
        fn open(port: u16) -> Result<Conn, String> {
            let addr = SocketAddr::from(([127, 0, 0, 1], port));
            let s = TcpStream::connect_timeout(&addr, Duration::from_millis(400)).map_err(|e| match e.kind() {
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut => "EffectCraft isn't open".to_string(),
                _ => format!("EffectCraft: {e}"),
            })?;
            let _ = s.set_read_timeout(Some(Duration::from_secs(70)));
            let _ = s.set_nodelay(true);
            let r = BufReader::new(s.try_clone().map_err(|e| format!("EffectCraft: {e}"))?);
            Ok(Conn { w: s, r, next: 1 })
        }

        fn call(&mut self, method: &str, params: Value) -> Result<Value, CallError> {
            let id = self.next;
            self.next += 1;
            let mut line = json!({"id": id, "method": method, "params": params}).to_string();
            line.push('\n');
            self.w.write_all(line.as_bytes()).map_err(|e| CallError::Io(format!("EffectCraft: {e}")))?;
            let mut buf = String::new();
            loop {
                buf.clear();
                let n = self.r.read_line(&mut buf).map_err(|e| CallError::Io(format!("EffectCraft: {e}")))?;
                if n == 0 {
                    return Err(CallError::Io("EffectCraft closed the connection".into()));
                }
                let Ok(v) = serde_json::from_str::<Value>(&buf) else { continue };
                if v.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if v.get("ok").and_then(Value::as_bool) == Some(true) {
                    return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                }
                let e = v.get("error").and_then(Value::as_str).unwrap_or("EffectCraft refused the request");
                return Err(CallError::Remote(e.to_string()));
            }
        }
    }

    /// What the app's `project.summary` says it has open (`path: null`: nothing, or untitled).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) fn seen_of(summary: &Value) -> Seen {
        Seen::Open(summary.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()).map(|p| figcraft_ec_inproc::key(Path::new(p))))
    }

    /// The app's error says it doesn't have the comp (so a saved project can render it here).
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) fn not_in_app(error: &str) -> bool {
        let e = error.to_ascii_lowercase();
        e.contains("no composition") || e.contains("no such comp") || e.contains("not found")
    }

    /// What the app has open: `project.summary` is an engine command, reached through the control
    /// channel's `engine.execute` (`{path: null}` with nothing open).
    fn summary() -> Value {
        json!({"command": "project.summary", "params": {}})
    }

    /// How the comp is named to EffectCraft: its id when known, else its name.
    fn comp_ref(c: &EcComp) -> Value {
        if c.comp_id > 0 { json!(c.comp_id) } else { json!(c.comp) }
    }

    fn unknown(e: &str) -> bool {
        e.contains("unknown method")
    }

    fn decode(ctx: &egui::Context, name: String, bytes: &[u8]) -> Option<egui::TextureHandle> {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        if w == 0 || h == 0 || w * h > MAX_PIXELS {
            return None;
        }
        let ci = egui::ColorImage::from_rgba_unmultiplied([w, h], img.as_raw());
        Some(ctx.load_texture(name, ci, egui::TextureOptions::LINEAR))
    }

    fn b64(v: Option<&Value>) -> Option<Vec<u8>> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(v?.as_str()?.trim()).ok()
    }

    pub(crate) struct Thread {
        conn: Option<Conn>,
        /// EffectCraft has no `figcraft.*` methods: frames come from `render.frame`.
        legacy: bool,
        ctx: egui::Context,
        /// The app's control port.
        port: u16,
        /// EffectCraft sessions inside FigCraft, one per card project.
        #[cfg(feature = "effectcraft-inproc")]
        inproc: figcraft_ec_inproc::Pool,
        /// The project this job's calls go to in-process (`None`: over TCP).
        #[cfg(feature = "effectcraft-inproc")]
        via: Option<PathBuf>,
        /// What the app last said it has open, and when.
        #[cfg(feature = "effectcraft-inproc")]
        seen: Option<(Instant, Seen)>,
        /// Where replies go (the in-process frame worker answers there itself).
        #[cfg_attr(not(feature = "effectcraft-inproc"), allow(dead_code))]
        out: Sender<Reply>,
        /// The in-process frame worker (started with the first in-process frames).
        #[cfg(feature = "effectcraft-inproc")]
        worker: Option<Sender<worker::Work>>,
        /// Projects whose frames render from a hosted panel's session instead of the file.
        #[cfg(feature = "effectcraft-inproc")]
        overrides: std::collections::HashMap<PathBuf, figcraft_ec_inproc::Snapshot>,
        /// The route each card project took last (logged when it changes).
        #[cfg(feature = "effectcraft-inproc")]
        routes: std::collections::HashMap<PathBuf, Route>,
    }

    impl Thread {
        pub(crate) fn new(ctx: egui::Context, port: u16, out: Sender<Reply>) -> Thread {
            Thread {
                conn: None,
                legacy: false,
                ctx,
                port,
                #[cfg(feature = "effectcraft-inproc")]
                inproc: figcraft_ec_inproc::Pool::default(),
                #[cfg(feature = "effectcraft-inproc")]
                via: None,
                #[cfg(feature = "effectcraft-inproc")]
                seen: None,
                out,
                #[cfg(feature = "effectcraft-inproc")]
                worker: None,
                #[cfg(feature = "effectcraft-inproc")]
                overrides: Default::default(),
                #[cfg(feature = "effectcraft-inproc")]
                routes: Default::default(),
            }
        }

        /// Answer `job`: `None` when the in-process frame worker took it (it answers on `out`).
        pub(crate) fn run(&mut self, job: Job) -> Option<Reply> {
            #[cfg(feature = "effectcraft-inproc")]
            let job = match job {
                Job::Override { path, snapshot } => {
                    match snapshot {
                        Some(s) => self.overrides.insert(figcraft_ec_inproc::key(&path), *s),
                        None => self.overrides.remove(&figcraft_ec_inproc::key(&path)),
                    };
                    return None;
                }
                job => job,
            };
            #[cfg(feature = "effectcraft-inproc")]
            if let Some(path) = self.in_process_route(&job) {
                return match job {
                    Job::Frames { .. } => self.in_process_frames(job, path),
                    _ => Some(self.in_process(job, path)),
                };
            }
            // The app answered but doesn't have the comp (another or no project open) while the
            // card's project is on disk: render it here after all.
            #[cfg(feature = "effectcraft-inproc")]
            let fallback = match &job {
                Job::Frames { comp, .. } if !comp.project.is_empty() && Path::new(&comp.project).is_file() => Some(job.clone()),
                _ => None,
            };
            let reply = self.over_tcp(job);
            #[cfg(feature = "effectcraft-inproc")]
            if let (Reply::FramesFailed { error, .. }, Some(job)) = (&reply, fallback)
                && not_in_app(error)
                && let Some(project) = job.comp().map(|c| figcraft_ec_inproc::key(Path::new(&c.project)))
            {
                log::warn!(
                    "EffectCraft card {:?}: the app answered \"{error}\"; rendering {} in FigCraft",
                    job.comp().map(|c| c.comp.as_str()).unwrap_or(""),
                    project.display()
                );
                self.routes.insert(project.clone(), Route::InProc);
                return self.in_process_frames(job, project);
            }
            Some(reply)
        }

        fn over_tcp(&mut self, job: Job) -> Reply {
            let kind = match &job {
                Job::Frames { .. } => Kind::Frames,
                Job::Revision => Kind::Revision,
                Job::Sound { key, .. } => Kind::Sound(key.clone()),
                Job::AppProject { .. } => Kind::Quiet,
                _ => Kind::Other,
            };
            if self.conn.is_none() {
                match Conn::open(self.port) {
                    Ok(c) => self.conn = Some(c),
                    Err(error) => {
                        if let Job::AppProject { card, .. } = job {
                            return Reply::AppProject { card, path: None };
                        }
                        // Cards render in-process meanwhile: the app being closed isn't an outage.
                        #[cfg(feature = "effectcraft-inproc")]
                        if matches!(job, Job::Revision) {
                            return Reply::Revision(0);
                        }
                        // Closed: frames still come, rendered from the saved project.
                        if let Job::Frames { store, comp, frames, side, poster } = job
                            && let Some(r) = headless(&self.ctx, store, &comp, frames, side, poster)
                        {
                            return r;
                        }
                        return Reply::Down { error, kind };
                    }
                }
            }
            match self.job(job) {
                Ok(r) => r,
                Err(error) => {
                    self.conn = None;
                    Reply::Down { error, kind }
                }
            }
        }

        fn call(&mut self, method: &str, params: Value) -> Result<Result<Value, String>, String> {
            // In-process: errors are the request's (a project that won't open, a bad comp), never
            // a lost connection.
            #[cfg(feature = "effectcraft-inproc")]
            if let Some(path) = self.via.clone() {
                // A hosted panel's unsaved edits: their sound and precomps, not the file's.
                if let Some(snap) = self.overrides.get(&path)
                    && matches!(method, "figcraft.tree" | "figcraft.audio")
                {
                    return Ok(snap.call(method, &params));
                }
                let r = self.inproc.get(&path).and_then(|ec| ec.call(method, &params));
                return Ok(r);
            }
            let Some(c) = self.conn.as_mut() else { return Err("EffectCraft isn't open".into()) };
            match c.call(method, params) {
                Ok(v) => Ok(Ok(v)),
                Err(CallError::Remote(e)) => Ok(Err(e)),
                Err(CallError::Io(e)) => Err(e),
            }
        }

        /// Run `job`: `Err` is a lost connection.
        fn job(&mut self, job: Job) -> Result<Reply, String> {
            match job {
                #[cfg(feature = "effectcraft-inproc")]
                Job::Override { .. } => Ok(Reply::Error("EffectCraft: an override is not a request".into())),
                Job::AppProject { card, comp } => {
                    let path = match self.call("engine.execute", summary())? {
                        Ok(v) => v.get("path").and_then(Value::as_str).filter(|p| !p.is_empty()).map(str::to_string),
                        Err(_) => None,
                    };
                    let Some(path) = path else { return Ok(Reply::AppProject { card, path: None }) };
                    let p = json!({"comp": comp_ref(&comp), "start": comp.start, "end": comp.start + 1.0 / comp.fps.max(1.0), "depth": 0});
                    let has = self.call("figcraft.tree", p)?.is_ok();
                    Ok(Reply::AppProject { card, path: has.then_some(path) })
                }
                Job::Revision => Ok(match self.call("app.revision", json!({}))? {
                    Ok(v) => Reply::Revision(v.get("revision").and_then(Value::as_u64).unwrap_or(0)),
                    Err(_) => Reply::Revision(0),
                }),
                Job::Frames { store, comp, frames, side, poster } => self.frames(store, &comp, frames, side, poster),
                Job::Sound { key, comp, start, end } => {
                    let p = json!({"comp": comp_ref(&comp), "start": start, "end": end, "rate": 48_000});
                    let sound = match self.call("figcraft.audio", p)? {
                        Ok(v) if v.get("silent").and_then(Value::as_bool) != Some(true) => b64(v.get("wav"))
                            .and_then(|bytes| figcraft_audio::open(&format!("{}.wav", comp.comp), Arc::from(bytes.as_slice())).ok())
                            .map(Arc::new),
                        _ => None,
                    };
                    Ok(Reply::Sound { key, sound })
                }
                Job::Tree { card, comp, at } => {
                    let p = json!({"comp": comp_ref(&comp), "start": at, "end": at + 1.0 / comp.fps, "depth": 3});
                    Ok(match self.call("figcraft.tree", p)? {
                        Ok(tree) => Reply::Tree { card, tree },
                        Err(e) if unknown(&e) => Reply::Error("This EffectCraft can't list its precomps yet: update it and restart it".into()),
                        Err(e) => Reply::Error(format!("EffectCraft: {e}")),
                    })
                }
                Job::Reveal { comp, time } => {
                    let p = json!({"comp": comp_ref(&comp), "time": time});
                    Ok(match self.call("figcraft.reveal", p)? {
                        Ok(_) => Reply::Revealed(comp.comp),
                        Err(e) if unknown(&e) => Reply::Error("This EffectCraft can't open comps from FigCraft yet: update it and restart it".into()),
                        Err(e) => Reply::Error(format!("EffectCraft: {e}")),
                    })
                }
            }
        }

        fn frames(&mut self, store: StoreKey, comp: &EcComp, frames: Vec<i64>, side: u32, poster: Option<(NodeId, i64)>) -> Result<Reply, String> {
            let (mut got, mut pending, mut failed, mut poster_bytes) = (Vec::new(), Vec::new(), Vec::new(), None);
            let name = |f: i64| format!("effectcraft-{}-{}-{side}-{f}", comp.comp_id, comp.comp);
            if !self.legacy {
                let p = json!({"comp": comp_ref(comp), "frames": frames, "maxSide": side});
                match self.call("figcraft.frames", p)? {
                    Ok(v) => {
                        let revision = v.get("revision").and_then(Value::as_u64).unwrap_or(0);
                        for e in v.get("frames").and_then(Value::as_array).into_iter().flatten() {
                            let Some(f) = e.get("frame").and_then(Value::as_i64) else { continue };
                            if e.get("pending").and_then(Value::as_bool) == Some(true) {
                                pending.push(f);
                            } else if let Some(bytes) = b64(e.get("jpeg").or_else(|| e.get("png"))) {
                                match decode(&self.ctx, name(f), &bytes) {
                                    Some(tex) => got.push((f, super::FrameTex::Cpu(tex))),
                                    None => failed.push(f),
                                }
                                if poster.is_some_and(|(_, pf)| pf == f) {
                                    poster_bytes = poster.map(|(id, _)| (id, bytes));
                                }
                            } else {
                                failed.push(f);
                            }
                        }
                        return Ok(Reply::Frames { store, revision, got, pending, failed, poster: poster_bytes, headless: None, file: None });
                    }
                    Err(e) if unknown(&e) => self.legacy = true,
                    Err(error) => return Ok(Reply::FramesFailed { store, frames, error }),
                }
            }
            // An older EffectCraft: a few frames per round through `render.frame` (it renders
            // each from scratch); the rest are asked for again.
            let revision = match self.call("app.revision", json!({}))? {
                Ok(v) => v.get("revision").and_then(Value::as_u64).unwrap_or(0),
                Err(_) => 0,
            };
            for (i, f) in frames.iter().copied().enumerate() {
                if i >= LEGACY_BATCH {
                    pending.push(f);
                    continue;
                }
                let p = json!({"comp": comp_ref(comp), "time": (f as f64 + 0.001) / comp.fps, "max_side": side, "base64": true});
                match self.call("render.frame", p)? {
                    Ok(v) => match b64(v.get("png")) {
                        Some(bytes) => {
                            match decode(&self.ctx, name(f), &bytes) {
                                Some(tex) => got.push((f, super::FrameTex::Cpu(tex))),
                                None => failed.push(f),
                            }
                            if poster.is_some_and(|(_, pf)| pf == f) {
                                poster_bytes = poster.map(|(id, _)| (id, bytes));
                            }
                        }
                        None => failed.push(f),
                    },
                    Err(_) => failed.push(f),
                }
            }
            Ok(Reply::Frames { store, revision, got, pending, failed, poster: poster_bytes, headless: None, file: None })
        }
    }

    #[cfg(feature = "effectcraft-inproc")]
    impl Thread {
        /// What the app has open, asked at most every [`PROBE_EVERY`].
        fn seen(&mut self) -> Seen {
            if let Some((at, seen)) = &self.seen
                && at.elapsed() < PROBE_EVERY
            {
                return seen.clone();
            }
            let seen = self.ask_app();
            self.seen = Some((Instant::now(), seen.clone()));
            seen
        }

        fn ask_app(&mut self) -> Seen {
            if self.conn.is_none() {
                match Conn::open(self.port) {
                    Ok(c) => self.conn = Some(c),
                    Err(_) => return Seen::Closed,
                }
            }
            match self.call("engine.execute", summary()) {
                Ok(Ok(v)) => seen_of(&v),
                Ok(Err(_)) => Seen::Unknown,
                Err(_) => {
                    self.conn = None;
                    Seen::Closed
                }
            }
        }

        /// The card project `job` runs on in-process when [`choose`] says so (`None`: the app).
        pub(crate) fn in_process_route(&mut self, job: &Job) -> Option<PathBuf> {
            let project = job.comp().map(|c| c.project.as_str()).filter(|p| !p.is_empty())?;
            let path = figcraft_ec_inproc::key(Path::new(project));
            let is_file = path.is_file();
            // The app is asked only for cards that could render here.
            let app = if is_file { state_of(&self.seen(), &path) } else { AppState::Closed };
            let route = choose(true, is_file, app);
            // Said once per project and route (why a card renders where it does).
            if self.routes.insert(path.clone(), route) != Some(route) {
                // (WARN for now, so it shows in the app's log)
                log::warn!(
                    "EffectCraft card {:?} of {}: {route:?} (file there: {is_file}, app: {app:?})",
                    job.comp().map(|c| c.comp.as_str()).unwrap_or(""),
                    path.display()
                );
            }
            (route == Route::InProc).then_some(path)
        }

        /// Run `job` on the in-process session of the project at `path`.
        fn in_process(&mut self, job: Job, path: PathBuf) -> Reply {
            self.via = Some(path.clone());
            let r = self.job(job);
            self.via = None;
            let file = self.inproc.get(&path).ok().map(|ec| ec.revision());
            match r {
                // Frames from the file: as new as the file (`file`), never behind the app's revision.
                Ok(Reply::Frames { store, got, pending, failed, poster, .. }) => {
                    Reply::Frames { store, revision: 0, got, pending, failed, poster, headless: None, file: file.or(Some(1)) }
                }
                Ok(other) => other,
                // (in-process calls don't lose connections; kept for safety)
                Err(error) => Reply::Error(format!("EffectCraft (in FigCraft): {error}")),
            }
        }

        /// Hand frames of the project at `path` to the frame worker (`None`: it answers), or
        /// fail them at once (a project that won't open, an unknown comp, no worker).
        fn in_process_frames(&mut self, job: Job, path: PathBuf) -> Option<Reply> {
            let Job::Frames { store, comp, frames, side, poster } = job else {
                return Some(Reply::Error("EffectCraft: not a frames request".into()));
            };
            let from = match self.overrides.get(&path) {
                Some(snap) => snap.comp_id(&comp_ref(&comp)).map(|id| (id, snap.clone())),
                None => self.inproc.get(&path).and_then(|ec| Ok((ec.comp_id(&comp_ref(&comp))?, ec.snapshot()))),
            };
            let work = match from {
                Ok((comp_id, snapshot)) => worker::Work { store, comp: comp.comp, comp_id, frames, side, poster, snapshot },
                Err(error) => {
                    log::warn!("EffectCraft in FigCraft: {} in {}: {error}", comp.comp, path.display());
                    return Some(Reply::FramesFailed { store, frames, error: format!("in FigCraft: {error}") });
                }
            };
            if self.worker.is_none() {
                self.worker = worker::start(self.ctx.clone(), self.out.clone());
            }
            let Some(tx) = self.worker.as_ref() else {
                return Some(Reply::FramesFailed { store: work.store, frames: work.frames, error: "EffectCraft frame worker didn't start".into() });
            };
            match tx.send(work) {
                Ok(()) => None,
                Err(std::sync::mpsc::SendError(w)) => {
                    self.worker = None;
                    Some(Reply::FramesFailed { store: w.store, frames: w.frames, error: "EffectCraft frame worker stopped".into() })
                }
            }
        }
    }

    /// In-process frames, rendered one at a time off the link thread (sound and tree requests
    /// never wait behind them): on the GPU sharing FigCraft's device when the app shared it and
    /// can show its textures, else on the CPU. At most one request per card is queued: a newer
    /// one replaces it (its frames come back `pending`, asked for again if still wanted), and a
    /// request being rendered stops early when a newer one for its card arrives.
    #[cfg(feature = "effectcraft-inproc")]
    pub(crate) mod worker {
        use std::sync::mpsc::{Receiver, Sender, channel};

        use figcraft_doc::NodeId;
        use figcraft_ec_inproc::{Compositor, FRAMES_PER_CALL, Picture, Snapshot};

        use super::{Reply, StoreKey, decode};
        use crate::effectcraft_ui::{FrameTex, NativeFrame, native_frames};

        pub struct Work {
            pub store: StoreKey,
            /// The comp's name (texture names) and id in the project.
            pub comp: String,
            pub comp_id: u64,
            pub frames: Vec<i64>,
            pub side: u32,
            pub poster: Option<(NodeId, i64)>,
            pub snapshot: Snapshot,
        }

        /// Which compositor renders a frame: the CPU for the poster (its bytes go into the
        /// document) and whenever the GPU frame couldn't be shown.
        pub(crate) fn compositor(poster: bool, gpu_shared: bool, can_show: bool) -> Compositor {
            if !poster && gpu_shared && can_show { Compositor::Gpu } else { Compositor::Cpu }
        }

        pub fn start(ctx: egui::Context, out: Sender<Reply>) -> Option<Sender<Work>> {
            let (tx, rx) = channel::<Work>();
            std::thread::Builder::new().name("effectcraft-frames".into()).spawn(move || run(&ctx, &rx, &out)).ok()?;
            Some(tx)
        }

        fn run(ctx: &egui::Context, rx: &Receiver<Work>, out: &Sender<Reply>) {
            let mut queue: Vec<Work> = Vec::new();
            loop {
                if queue.is_empty() {
                    match rx.recv() {
                        Ok(w) => enqueue(&mut queue, w, out),
                        Err(_) => return,
                    }
                }
                while let Ok(w) = rx.try_recv() {
                    enqueue(&mut queue, w, out);
                }
                if queue.is_empty() {
                    continue;
                }
                let w = queue.remove(0);
                let (store, frames) = (w.store.clone(), w.frames.clone());
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render(ctx, w, rx, &mut queue, out)));
                let reply = r.unwrap_or_else(|_| Reply::FramesFailed { store, frames, error: "EffectCraft frame worker: an internal error".into() });
                if out.send(reply).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        }

        /// Queue `w`, replacing (and answering `pending`) a queued request for the same card.
        pub(crate) fn enqueue(queue: &mut Vec<Work>, mut w: Work, out: &Sender<Reply>) {
            if let Some(i) = queue.iter().position(|q| q.store == w.store) {
                let old = queue.remove(i);
                w.poster = w.poster.or(old.poster);
                let _ = out.send(superseded(old));
            }
            queue.push(w);
        }

        fn superseded(w: Work) -> Reply {
            Reply::Frames {
                store: w.store,
                revision: 0,
                got: Vec::new(),
                pending: w.frames,
                failed: Vec::new(),
                poster: None,
                headless: None,
                file: Some(w.snapshot.revision),
            }
        }

        pub(crate) fn render(ctx: &egui::Context, w: Work, rx: &Receiver<Work>, queue: &mut Vec<Work>, out: &Sender<Reply>) -> Reply {
            let (mut got, mut pending, mut failed, mut poster_bytes) = (Vec::new(), Vec::new(), Vec::new(), None);
            let shared = figcraft_ec_inproc::gpu_shared();
            for (i, &f) in w.frames.iter().enumerate() {
                if i > 0 {
                    while let Ok(n) = rx.try_recv() {
                        enqueue(queue, n, out);
                    }
                }
                // A newer request for this card wins: the rest wait.
                if i >= FRAMES_PER_CALL || queue.iter().any(|q| q.store == w.store) {
                    pending.push(f);
                    continue;
                }
                let is_poster = w.poster.is_some_and(|(_, pf)| pf == f);
                let shower = native_frames();
                let with = compositor(is_poster, shared, shower.is_some());
                match w.snapshot.frame(w.comp_id, f, w.side, with) {
                    Ok(Picture::Png(bytes)) => {
                        match decode(ctx, format!("effectcraft-inproc-{}-{}-{}-{f}", w.comp_id, w.comp, w.side), &bytes) {
                            Some(tex) => got.push((f, FrameTex::Cpu(tex))),
                            None => failed.push(f),
                        }
                        if is_poster {
                            poster_bytes = w.poster.map(|(id, _)| (id, bytes));
                        }
                    }
                    Ok(Picture::Native { texture, width, height }) => match shower.and_then(|s| s.register(texture.as_ref())) {
                        Some(id) => got.push((f, FrameTex::Native(NativeFrame::new(id, [width as usize, height as usize])))),
                        None => {
                            log::warn!("EffectCraft frame {f} of {}: the GPU frame couldn't be shown; on the CPU", w.comp);
                            let cpu = w.snapshot.frame(w.comp_id, f, w.side, Compositor::Cpu).ok().and_then(|p| match p {
                                Picture::Png(bytes) => decode(ctx, format!("effectcraft-inproc-{}-{}-{}-{f}", w.comp_id, w.comp, w.side), &bytes),
                                Picture::Native { .. } => None,
                            });
                            match cpu {
                                Some(tex) => got.push((f, FrameTex::Cpu(tex))),
                                None => failed.push(f),
                            }
                        }
                    },
                    Err(e) => {
                        log::warn!("EffectCraft frame {f} of {}: {e}", w.comp);
                        failed.push(f);
                    }
                }
            }
            Reply::Frames { store: w.store, revision: 0, got, pending, failed, poster: poster_bytes, headless: None, file: Some(w.snapshot.revision) }
        }
    }

    // (src/effectcraft_ui/io/tests.rs)
    #[cfg(test)]
    mod tests;

    /// Frames rendered at once from the saved project, and how long one run may take (seconds).
    const HEADLESS_BATCH: usize = 12;
    const HEADLESS_TIMEOUT: u64 = 90;

    /// `effectcraft-cli`: `EFFECTCRAFT_CLI`, the local EffectCraft build, or the one on `PATH`.
    fn cli() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("EFFECTCRAFT_CLI").map(PathBuf::from).filter(|p| p.is_file()) {
            return Some(p);
        }
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from);
        let exe = if cfg!(windows) { "effectcraft-cli.exe" } else { "effectcraft-cli" };
        if let Some(p) = home.map(|h| h.join("Developer/effectcraft/target/local/local").join(exe)).filter(|p| p.is_file()) {
            return Some(p);
        }
        std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(exe)).find(|p| p.is_file())
    }

    /// EffectCraft is closed: render the first `HEADLESS_BATCH` of `frames` from the card's saved
    /// project with `effectcraft-cli` (one process, PNGs in a temp folder). `None` when there is no
    /// project file or no CLI.
    fn headless(ctx: &egui::Context, store: StoreKey, comp: &EcComp, frames: Vec<i64>, side: u32, poster: Option<(NodeId, i64)>) -> Option<Reply> {
        let project = Path::new(&comp.project);
        if comp.project.is_empty() || !project.is_file() {
            return None;
        }
        let cli = cli()?;
        let stamp =
            std::fs::metadata(project).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
        static RUN: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!("figcraft-effectcraft-{}-{}", std::process::id(), RUN.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).ok()?;
        let take: Vec<i64> = frames.iter().copied().take(HEADLESS_BATCH).collect();
        let pending: Vec<i64> = frames.iter().copied().skip(HEADLESS_BATCH).collect();
        let file = |f: i64| dir.join(format!("{f}.png"));
        let mut cmd = Command::new(cli);
        cmd.arg("run");
        for f in &take {
            let p = json!({"comp": comp_ref(comp), "time": (*f as f64 + 0.001) / comp.fps, "max_side": side, "path": file(*f)});
            cmd.arg("render.frame").arg(p.to_string());
        }
        cmd.arg("--project").arg(project).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        let finished = match cmd.spawn() {
            Ok(mut child) => {
                let start = Instant::now();
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break true,
                        Ok(None) if start.elapsed() < Duration::from_secs(HEADLESS_TIMEOUT) => std::thread::sleep(Duration::from_millis(30)),
                        _ => {
                            let _ = child.kill();
                            let _ = child.wait();
                            break false;
                        }
                    }
                }
            }
            Err(_) => false,
        };
        let (mut got, mut failed, mut poster_bytes) = (Vec::new(), Vec::new(), None);
        for f in take {
            // Frames the run didn't get to (it stopped on an error) count as failed.
            let bytes = std::fs::read(file(f)).ok();
            match bytes.as_deref().and_then(|b| decode(ctx, format!("effectcraft-file-{}-{}-{side}-{f}", comp.comp_id, comp.comp), b)) {
                Some(tex) => got.push((f, super::FrameTex::Cpu(tex))),
                None => failed.push(f),
            }
            if let Some(b) = bytes
                && poster.is_some_and(|(_, pf)| pf == f)
            {
                poster_bytes = poster.map(|(id, _)| (id, b));
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        if !finished && got.is_empty() {
            return Some(Reply::FramesFailed { store, frames: failed, error: "effectcraft-cli couldn't render the saved project".into() });
        }
        Some(Reply::Frames { store, revision: 0, got, pending, failed, poster: poster_bytes, headless: Some(stamp), file: None })
    }
}

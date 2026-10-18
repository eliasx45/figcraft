//! Canvas rendering off the UI thread (desktop) or inline (web). The latest request wins (queued
//! older ones are skipped); the UI shows each finished image, re-projected to the current view,
//! until the next one arrives.
//! The renderer keeps its last image ([`figcraft_render::Retained`]): a pan renders only the
//! uncovered strips and an edit only the areas of the layers that changed.
//! A new zoom can't reuse that image, and the old one, re-projected, covers little of the view
//! after a zoom out (an eighth of the width from 800% to 100%). When full renders are slow, the
//! whole view is first rendered coarsely ([`PREVIEW_FACTOR`] times fewer pixels each way, kept
//! apart from the full image so pans and edits still build on it), then refined unless a newer
//! request is already waiting.
//! Between canvas renders the thread paints canvas tiles ([`figcraft_render::tiles`]) for
//! [`crate::tiles`]: a few at a time, so a canvas render never waits behind more than a few.

use std::sync::Arc;

use figcraft_color::ColorProfile;
use figcraft_doc::{Document, NodeId};
use figcraft_render::tiles::{CpuTilePainter, Painted, TileKey, TilePainter};
use figcraft_render::{Pixmap, Retained, View, Work};
// A monotonic clock in the browser too: preview decisions rest on how long renders take there.
use web_time::Instant;

/// A preview has this many times fewer pixels each way than the view it stands in for.
pub const PREVIEW_FACTOR: f64 = 4.0;
/// Full renders quicker than this (about a frame) show without a preview first.
pub const PREVIEW_AFTER_MS: f64 = 12.0;

#[derive(Clone)]
pub struct Job {
    pub doc: Arc<Document>,
    pub page: NodeId,
    pub view: View,
    pub outline: bool,
    /// Outline mode's options (hidden layers, object bounds).
    pub outline_extras: figcraft_render::Outline,
    /// Layers drawn as outlines (outlines for the selection).
    pub outlined: Vec<NodeId>,
    pub skip: Vec<NodeId>,
    /// Only this layer (the focus view).
    pub solo: Option<NodeId>,
    /// Soft-proof sRGB: stripe the colours sRGB can't show.
    pub proof: bool,
    /// The document's [`figcraft_engine::DocState::uid`]: images of other files are never shown.
    pub uid: u64,
    /// A coarse preview may come first when the zoom changes (not in pixel preview, whose pixels
    /// are the point).
    pub zoom_preview: bool,
    /// When the UI asked for it: how long the image took to reach the screen.
    pub at: Instant,
}

pub struct Done {
    /// The image, ready for a texture (converted off the UI thread).
    pub image: egui::ColorImage,
    /// The view the image shows (the requested one snapped outwards to whole device pixels).
    pub view: View,
    pub ms: f64,
    pub work: Work,
    /// What it is an image of ([`Job::uid`], [`Job::page`]).
    pub uid: u64,
    pub page: NodeId,
    /// A coarse preview, refined by a later image unless a newer request replaced it.
    pub coarse: bool,
    /// [`Job::at`].
    pub at: Instant,
}

/// What tiles are rendered with: tiles of other options aren't shown.
#[derive(Clone, Debug, PartialEq)]
pub struct TileOpts {
    pub outline: bool,
    pub outline_extras: figcraft_render::Outline,
    pub outlined: Vec<NodeId>,
    pub solo: Option<NodeId>,
    pub proof: bool,
    /// [`figcraft_text::layout_generation`]: new fonts change text everywhere.
    pub fonts: u64,
}

/// Tiles to paint, most useful first; a newer batch replaces the one being painted.
#[derive(Clone)]
pub struct TileBatch {
    /// Set by the worker it's submitted to.
    pub seq: u64,
    pub doc: Arc<Document>,
    pub page: NodeId,
    /// [`crate::tiles::TileCache`]'s generation: tiles of an older one are dropped.
    pub generation: u64,
    pub opts: TileOpts,
    pub keys: Vec<TileKey>,
}

pub struct TileDone {
    pub key: TileKey,
    /// `None`: only the page's background.
    pub image: Option<egui::ColorImage>,
    /// The document it shows (it may have changed since).
    pub doc: Arc<Document>,
    pub generation: u64,
}

/// Tiles a canvas render waits behind at most: one per core.
fn tile_chunk() -> usize {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get).clamp(1, 16)
    }
    #[cfg(target_arch = "wasm32")]
    {
        2
    }
}

/// A render of a document in `profile` as a texture image for the screen (sRGB).
pub fn screen_image(pix: &Pixmap, profile: ColorProfile) -> egui::ColorImage {
    let size = [pix.width() as usize, pix.height() as usize];
    if profile.is_srgb() {
        return egui::ColorImage::from_rgba_premultiplied(size, pix.data());
    }
    let mut data = pix.data().to_vec();
    profile.conversion_to(ColorProfile::Srgb).apply_pixels(&mut data);
    egui::ColorImage::from_rgba_premultiplied(size, &data)
}

/// Soft proof: grey diagonal stripes over the pixels whose colours sRGB can't show.
fn mark_outside_srgb(image: &mut egui::ColorImage, pix: &Pixmap, profile: ColorProfile) {
    let w = image.size[0].max(1);
    let clipped = profile.conversion_to(ColorProfile::Srgb).clipped_pixels(pix.data());
    for (i, (px, out)) in image.pixels.iter_mut().zip(clipped).enumerate() {
        if out && ((i % w) + (i / w)) % 8 < 4 {
            *px = egui::Color32::from_gray(128);
        }
    }
}

/// Render `view` of the job (its own view, or the coarse one standing in for it).
fn run(job: &Job, view: &View, retained: &mut Retained, coarse: bool) -> Option<Done> {
    let t0 = Instant::now();
    // The canvas is a preview: what's too small to see in detail at this zoom is approximated.
    let opts = figcraft_render::Options {
        outline_mode: job.outline,
        outline: job.outline_extras,
        outlined: job.outlined.clone(),
        skip: job.skip.clone(),
        solo: job.solo,
        lod: true,
        ..Default::default()
    };
    let (view, pix, work) = retained.render(&job.doc, job.page, view, &opts)?;
    let mut image = screen_image(pix, job.doc.color_profile);
    if job.proof {
        mark_outside_srgb(&mut image, pix, job.doc.color_profile);
    }
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    Some(Done { image, view, ms, work, uid: job.uid, page: job.page, coarse, at: job.at })
}

/// `view` with [`PREVIEW_FACTOR`] times fewer pixels each way, showing at least the same area.
pub fn coarse_view(view: &View) -> View {
    let shrink = |n: u32| (f64::from(n) / PREVIEW_FACTOR).ceil().max(1.0) as u32;
    View { origin: view.origin, scale: view.scale / PREVIEW_FACTOR, width: shrink(view.width), height: shrink(view.height) }
}

/// Makes a tile painter for each render thread (the app's GPU painter); `None` from it: the CPU's.
type PainterFactory = Box<dyn Fn() -> Option<Box<dyn TilePainter>> + Send + Sync>;

static TILE_PAINTER: std::sync::OnceLock<PainterFactory> = std::sync::OnceLock::new();

/// Paint canvas tiles with what `factory` makes (once per process, before the first window
/// renders; later calls are ignored).
pub fn set_tile_painter(factory: PainterFactory) {
    let _ = TILE_PAINTER.set(factory);
}

fn new_tile_painter() -> Box<dyn TilePainter> {
    TILE_PAINTER.get().and_then(|f| f()).unwrap_or_else(|| Box::new(CpuTilePainter))
}

/// How tiles have been painted (`ui.inspect` canvas.tiles.painting).
#[derive(Clone, Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TileStats {
    /// The painter and how its last chunk went.
    pub painter: String,
    /// Tiles in the last chunk and how long they took.
    pub last_tiles: usize,
    pub last_ms: f64,
    /// Since the app started.
    pub tiles: u64,
    pub ms: f64,
}

static TILE_STATS: std::sync::Mutex<TileStats> =
    std::sync::Mutex::new(TileStats { painter: String::new(), last_tiles: 0, last_ms: 0.0, tiles: 0, ms: 0.0 });

pub fn tile_stats() -> TileStats {
    TILE_STATS.lock().map(|s| s.clone()).unwrap_or_default()
}

/// What the render thread keeps between jobs: the canvas image, the previews standing in for it
/// while the zoom changes, and what the last full render drew and took.
pub struct Renders {
    full: Retained,
    preview: Retained,
    /// The last full render's document, page and scale.
    last: Option<(u64, NodeId, f64)>,
    /// How long the last render from scratch took.
    full_ms: f64,
    /// Previews come first only when renders from scratch take at least this long.
    preview_after_ms: f64,
    /// Paints canvas tiles: on the CPU, or the GPU where the app installed a painter for it
    /// ([`set_tile_painter`]); made for the first batch (a GPU one compiles its shaders then).
    painter: Option<Box<dyn TilePainter>>,
}

impl Default for Renders {
    fn default() -> Self {
        Self::with_threshold(PREVIEW_AFTER_MS)
    }
}

impl Renders {
    /// Previews when renders from scratch take at least `ms` (tests: 0, always).
    pub fn with_threshold(ms: f64) -> Self {
        Self {
            full: Retained::default(),
            preview: Retained::default(),
            last: None,
            full_ms: 0.0,
            preview_after_ms: ms,
            painter: None,
        }
    }

    /// Whether `job` gets a preview first: the kept image is of another zoom, page or file (so the
    /// full render starts from scratch) and those have been slow. The first render has no
    /// measure yet and comes without one.
    fn wants_preview(&self, job: &Job) -> bool {
        job.zoom_preview
            && self.full_ms >= self.preview_after_ms
            && self.last.is_some_and(|(uid, page, scale)| uid != job.uid || page != job.page || scale.to_bits() != job.view.scale.to_bits())
    }

    /// The coarse preview of the whole of `job`'s view, when one should come first.
    pub fn preview(&mut self, job: &Job) -> Option<Done> {
        if !self.wants_preview(job) {
            return None;
        }
        let view = coarse_view(&job.view);
        guarded(&mut self.preview, |r| run(job, &view, r, true))
    }

    /// The image of `job`'s view, built on the kept one where it can be.
    pub fn refine(&mut self, job: &Job) -> Option<Done> {
        let done = guarded(&mut self.full, |r| run(job, &job.view, r, false));
        if let Some(d) = &done {
            self.last = Some((job.uid, job.page, job.view.scale));
            if d.work == Work::Full {
                self.full_ms = d.ms;
            }
        }
        done
    }
}

impl Renders {
    /// Paint `keys` of `batch`, ready for textures, and the keys `stop` left for later (a canvas
    /// render is waiting). A panic paints none of them and leaves none.
    pub fn tiles(&mut self, batch: &TileBatch, keys: &[TileKey], stop: &(dyn Fn() -> bool + Sync)) -> (Vec<TileDone>, Vec<TileKey>) {
        let o = &batch.opts;
        let opts = figcraft_render::Options {
            outline_mode: o.outline,
            outline: o.outline_extras,
            outlined: o.outlined.clone(),
            solo: o.solo,
            lod: true,
            ..Default::default()
        };
        let painter = self.painter.get_or_insert_with(new_tile_painter);
        let t0 = web_time::Instant::now();
        let Ok(painted) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| painter.paint(&batch.doc, batch.page, keys, &opts, stop))) else {
            return (vec![], vec![]);
        };
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if let Ok(mut s) = TILE_STATS.lock() {
            s.painter = painter.report();
            s.last_tiles = painted.len();
            s.last_ms = ms;
            s.tiles += painted.len() as u64;
            s.ms += ms;
        }
        let started: std::collections::HashSet<TileKey> = painted.iter().map(|(k, _)| *k).collect();
        let later = keys.iter().filter(|k| !started.contains(k)).copied().collect();
        let done = painted
            .into_iter()
            .filter_map(|(key, p)| {
                let image = match p? {
                    Painted::Empty => None,
                    Painted::Pixels(pix) => {
                        let mut image = screen_image(&pix, batch.doc.color_profile);
                        if o.proof {
                            mark_outside_srgb(&mut image, &pix, batch.doc.color_profile);
                        }
                        Some(image)
                    }
                };
                Some(TileDone { key, image, doc: batch.doc.clone(), generation: batch.generation })
            })
            .collect();
        (done, later)
    }
}

/// Run `f` on `retained`; after a panic, forget the image it may have left half drawn.
fn guarded(retained: &mut Retained, f: impl FnOnce(&mut Retained) -> Option<Done>) -> Option<Done> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(retained))) {
        Ok(d) => d,
        Err(_) => {
            retained.clear();
            None
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
enum Msg {
    Job(u64, Job),
    Tiles(TileBatch),
}

/// The batch being painted and how far it got.
pub(crate) type Painting = Option<(TileBatch, usize)>;

/// Take what's queued: the newest job (if any) and the newest tile batch.
#[cfg(not(target_arch = "wasm32"))]
fn drain(jobs: &std::sync::mpsc::Receiver<Msg>, tiles: &mut Painting) -> Option<(u64, Job)> {
    let mut newest = None;
    while let Ok(m) = jobs.try_recv() {
        match m {
            Msg::Job(seq, job) => newest = Some((seq, job)),
            Msg::Tiles(b) => *tiles = Some((b, 0)),
        }
    }
    newest
}

/// Paint the next chunk of the batch; `None` when it has none left. Tiles `stop` left out go
/// back to the front of the batch.
pub(crate) fn paint_next(renders: &mut Renders, tiles: &mut Painting, stop: &(dyn Fn() -> bool + Sync)) -> Option<Vec<TileDone>> {
    let (batch, at) = tiles.as_mut()?;
    let keys = batch.keys.get(*at..)?.iter().take(tile_chunk()).copied().collect::<Vec<_>>();
    if keys.is_empty() {
        *tiles = None;
        return None;
    }
    *at += keys.len();
    let (done, later) = renders.tiles(batch, &keys, stop);
    if !later.is_empty() {
        let rest = batch.keys.get(*at..).unwrap_or_default();
        batch.keys = later.into_iter().chain(rest.iter().copied()).collect();
        *at = 0;
    }
    Some(done)
}

fn tiles_left(tiles: &Painting) -> bool {
    tiles.as_ref().is_some_and(|(b, at)| *at < b.keys.len())
}

#[cfg(not(target_arch = "wasm32"))]
pub struct Worker {
    tx: std::sync::mpsc::Sender<Msg>,
    rx: std::sync::mpsc::Receiver<Done>,
    tile_rx: std::sync::mpsc::Receiver<TileDone>,
    /// Jobs submitted so far (shared: tile painting stops for a newer one).
    submitted: Arc<std::sync::atomic::AtomicU64>,
    /// The newest job the thread is done with: rendered (or skipped for a newer one) and the UI
    /// woken for it.
    finished: Arc<std::sync::atomic::AtomicU64>,
    /// Tile batches submitted so far, and the newest one painted (older ones it replaced count).
    tiles_submitted: u64,
    tiles_finished: Arc<std::sync::atomic::AtomicU64>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Worker {
    pub fn new(ctx: egui::Context) -> Self {
        use std::sync::mpsc::TryRecvError;
        let (tx, jobs) = std::sync::mpsc::channel::<Msg>();
        let (done_tx, rx) = std::sync::mpsc::channel::<Done>();
        let (tile_tx, tile_rx) = std::sync::mpsc::channel::<TileDone>();
        let finished = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let done_with = finished.clone();
        let tiles_finished = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let tiles_done_with = tiles_finished.clone();
        let submitted = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let queued = submitted.clone();
        // The window that renders with it (each window has its own worker).
        let viewport = ctx.viewport_id();
        let spawned = std::thread::Builder::new().name("figcraft-render".into()).spawn(move || {
            let mut renders = Renders::default();
            let mut next = None;
            let mut tiles: Painting = None;
            loop {
                let first = match next.take() {
                    Some(j) => Some(j),
                    // Wait for work only when no tiles are left to paint.
                    None if tiles_left(&tiles) => match jobs.try_recv() {
                        Ok(Msg::Job(seq, job)) => Some((seq, job)),
                        Ok(Msg::Tiles(b)) => {
                            tiles = Some((b, 0));
                            continue;
                        }
                        Err(TryRecvError::Empty) => None,
                        Err(TryRecvError::Disconnected) => break,
                    },
                    None => match jobs.recv() {
                        Ok(Msg::Job(seq, job)) => Some((seq, job)),
                        Ok(Msg::Tiles(b)) => {
                            tiles = Some((b, 0));
                            continue;
                        }
                        Err(_) => break,
                    },
                };
                let Some((mut seq, mut job)) = first else {
                    // Idle: a few tiles, then look for a canvas render again.
                    let seq = tiles.as_ref().map_or(0, |(b, _)| b.seq);
                    // A canvas render asked for meanwhile stops the tiles not started yet.
                    let waiting = || queued.load(std::sync::atomic::Ordering::Acquire) > done_with.load(std::sync::atomic::Ordering::Acquire);
                    if let Some(done) = paint_next(&mut renders, &mut tiles, &waiting)
                        && !done.is_empty()
                    {
                        for d in done {
                            if tile_tx.send(d).is_err() {
                                return;
                            }
                        }
                        ctx.request_repaint_of(viewport);
                    }
                    if !tiles_left(&tiles) {
                        tiles = None;
                        tiles_done_with.store(seq, std::sync::atomic::Ordering::Release);
                    }
                    continue;
                };
                // Skip to the newest queued job.
                if let Some(newer) = drain(&jobs, &mut tiles) {
                    (seq, job) = newer;
                }
                if let Some(d) = renders.preview(&job) {
                    if done_tx.send(d).is_err() {
                        break;
                    }
                    ctx.request_repaint_of(viewport);
                    // A newer request makes refining this one moot: it gets its own preview.
                    if let Some(newer) = drain(&jobs, &mut tiles) {
                        next = Some(newer);
                        continue;
                    }
                }
                if let Some(d) = renders.refine(&job)
                    && done_tx.send(d).is_err()
                {
                    break;
                }
                ctx.request_repaint_of(viewport);
                done_with.store(seq, std::sync::atomic::Ordering::Release);
            }
        });
        if let Err(e) = spawned {
            log::error!("render thread: {e}");
        }
        Self { tx, rx, tile_rx, submitted, finished, tiles_submitted: 0, tiles_finished }
    }

    pub fn submit(&mut self, job: Job) {
        use std::sync::atomic::Ordering;
        let seq = self.submitted.load(Ordering::Acquire) + 1;
        // Counted before it's sent: the thread may already be painting tiles it should stop.
        self.submitted.store(seq, Ordering::Release);
        if self.tx.send(Msg::Job(seq, job)).is_err() {
            self.submitted.store(seq - 1, Ordering::Release);
        }
    }

    /// Paint these tiles when there's nothing else to render (replacing the tiles asked for
    /// before).
    pub fn submit_tiles(&mut self, mut batch: TileBatch) {
        batch.seq = self.tiles_submitted + 1;
        if self.tx.send(Msg::Tiles(batch)).is_ok() {
            self.tiles_submitted += 1;
        }
    }

    /// [`Self::in_flight`], or tiles still being painted (tests waiting for a still UI).
    pub fn busy(&self) -> bool {
        self.in_flight() || self.tiles_finished.load(std::sync::atomic::Ordering::Acquire) < self.tiles_submitted
    }

    /// Whether a submitted job is still rendering or hasn't woken the UI yet (a finished image
    /// may still wait for [`Self::poll`]). Tiles don't count.
    pub fn in_flight(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire) < self.submitted.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn poll(&mut self) -> Option<Done> {
        let mut last = None;
        while let Ok(d) = self.rx.try_recv() {
            last = Some(d);
        }
        last
    }

    /// The tiles painted since the last call.
    pub fn poll_tiles(&mut self) -> Vec<TileDone> {
        self.tile_rx.try_iter().collect()
    }
}

/// The browser's worker: renders inline, one image a frame: a preview first when one is due, the
/// refined image in the next frame unless a newer request replaced the job. Built everywhere so
/// tests run it.
pub struct Inline {
    ctx: egui::Context,
    pending: Option<Job>,
    previewed: bool,
    renders: Renders,
    tiles: Painting,
}

#[cfg(target_arch = "wasm32")]
pub type Worker = Inline;

impl Inline {
    pub fn new(ctx: egui::Context) -> Self {
        Self { ctx, pending: None, previewed: false, renders: Renders::default(), tiles: None }
    }
    pub fn submit_tiles(&mut self, batch: TileBatch) {
        self.tiles = Some((batch, 0));
    }
    /// A few tiles a frame while no canvas render is pending.
    pub fn poll_tiles(&mut self) -> Vec<TileDone> {
        if self.pending.is_some() {
            return vec![];
        }
        let done = paint_next(&mut self.renders, &mut self.tiles, &|| false).unwrap_or_default();
        if tiles_left(&self.tiles) {
            self.ctx.request_repaint();
        }
        done
    }
    pub fn submit(&mut self, job: Job) {
        self.pending = Some(job);
        self.previewed = false;
    }
    pub fn in_flight(&self) -> bool {
        self.pending.is_some()
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some() || tiles_left(&self.tiles)
    }
    pub fn poll(&mut self) -> Option<Done> {
        if !self.previewed {
            self.previewed = true;
            if let Some(d) = self.pending.as_ref().and_then(|job| self.renders.preview(job)) {
                self.ctx.request_repaint();
                return Some(d);
            }
        }
        let job = self.pending.take()?;
        self.renders.refine(&job)
    }
}

/// Render synchronously from scratch (tests, screenshots).
pub fn render_now(job: &Job) -> Option<Done> {
    run(job, &job.view, &mut Retained::default(), false)
}

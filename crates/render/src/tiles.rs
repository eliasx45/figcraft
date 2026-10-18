//! Canvas tiles: the page cut into squares of [`TILE_PX`] device pixels at power-of-two zoom
//! levels, so a screen can keep what it has rendered and reuse it at other zooms.
//!
//! - **Levels.** Level `k` renders `2^k` device pixels per page unit. Tile `(k, x, y)` covers the
//!   page square at `(x, y) · TILE_PX / 2^k`, so the four tiles of level `k + 1` inside it share
//!   its edges exactly and every tile of a level sits on one pixel grid (no seams between them).
//! - **Painting** goes through [`TilePainter`]: [`CpuTilePainter`] renders with tiny-skia, the
//!   same renderer as everything else. A GPU painter can take its place without touching the
//!   tile grid, the cache or the compositing.
//! - **Planning** ([`wanted`]): after a view settles, the tiles that make zooming out instant: a
//!   coarser level around the view, wider at each coarser level, and one that shows the whole page.
//! - **Compositing** ([`compose`]): which source draws each part of the screen, the exact canvas
//!   image or the sharpest tile there is, so every pixel comes from exactly one image.

use std::sync::Arc;

use figcraft_doc::{Document, NodeId};
use figcraft_geom::Rect;
use tiny_skia::{IntRect, Pixmap};

use crate::{Options, View, render_view};

/// A tile's side in device pixels.
pub const TILE_PX: u32 = 512;
/// The finest level: 2^10 device pixels per page unit.
pub const MAX_LEVEL: i32 = 10;
/// The coarsest level.
pub const MIN_LEVEL: i32 = -30;
/// Levels [`wanted`] fills below the view's own (each twice as coarse, covering twice as far).
pub const LEVELS_OUT: i32 = 6;
/// The most tiles [`wanted`] asks for at one level, nearest the view's centre first.
pub const MAX_PER_LEVEL: usize = 48;
/// [`compose`] gives up on tiles (exact image only) beyond this many screen cells.
const MAX_CELLS: usize = 4096;
/// How many coarser levels [`compose`] looks through for a tile under a cell.
const LOOK_UP: i32 = 16;
/// Backdrop margins beyond this many device pixels are capped.
const MAX_MARGIN_PX: f64 = 1024.0;

/// One tile: level `level`, column `x`, row `y`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub level: i32,
    pub x: i64,
    pub y: i64,
}

/// Device pixels per page unit at `level`.
pub fn level_scale(level: i32) -> f64 {
    2f64.powi(level.clamp(MIN_LEVEL, MAX_LEVEL))
}

/// The level whose scale is at most `scale` (the sharpest one that is never magnified more than
/// twice to show `scale`): `floor(log2 scale)`, clamped.
pub fn level_at_most(scale: f64) -> Option<i32> {
    finite_log2(scale).map(|l| (l.floor() as i32).clamp(MIN_LEVEL, MAX_LEVEL))
}

/// The level whose scale is at least `scale` (shown at `scale` it is never magnified):
/// `ceil(log2 scale)`, clamped.
pub fn level_at_least(scale: f64) -> Option<i32> {
    finite_log2(scale).map(|l| (l.ceil() as i32).clamp(MIN_LEVEL, MAX_LEVEL))
}

fn finite_log2(scale: f64) -> Option<f64> {
    (scale.is_finite() && scale > 0.0).then(|| scale.log2()).filter(|l| l.is_finite())
}

/// Page units a tile of `level` covers each way.
fn tile_units(level: i32) -> f64 {
    f64::from(TILE_PX) / level_scale(level)
}

impl TileKey {
    pub fn scale(&self) -> f64 {
        level_scale(self.level)
    }

    /// The page square the tile covers.
    pub fn rect(&self) -> Rect {
        let u = tile_units(self.level);
        let (x0, y0) = (self.x as f64 * u, self.y as f64 * u);
        Rect::new(x0, y0, x0 + u, y0 + u)
    }

    /// The tile's view: [`TILE_PX`] square at its level.
    pub fn view(&self) -> View {
        let u = tile_units(self.level);
        View { origin: (self.x as f64 * u, self.y as f64 * u), scale: self.scale(), width: TILE_PX, height: TILE_PX }
    }

    /// The tile `levels` coarser that contains this one.
    pub fn ancestor(&self, levels: i32) -> TileKey {
        let up = levels.clamp(0, 62) as u32;
        TileKey { level: self.level - levels, x: self.x.div_euclid(1i64 << up), y: self.y.div_euclid(1i64 << up) }
    }

    /// The part of this tile that `r` (inside it) covers, as texture coordinates `[0, 1]²`.
    pub fn uv(&self, r: Rect) -> Rect {
        let t = self.rect();
        let (w, h) = (t.width().max(1e-300), t.height().max(1e-300));
        Rect::new((r.x0 - t.x0) / w, (r.y0 - t.y0) / h, (r.x1 - t.x0) / w, (r.y1 - t.y0) / h)
    }
}

/// The tiles of `level` that `r` touches, row by row; `None` when there are more than `max`.
pub fn tiles_in(r: Rect, level: i32, max: usize) -> Option<Vec<TileKey>> {
    let u = tile_units(level);
    let idx = |v: f64| -> Option<i64> {
        let i = (v / u).floor();
        (i.is_finite() && i.abs() < 1e15).then_some(i as i64)
    };
    // The last tile is the one holding the right (bottom) edge, not the one starting there.
    let last = |v: f64| -> Option<i64> {
        let i = (v / u).ceil() - 1.0;
        (i.is_finite() && i.abs() < 1e15).then_some(i as i64)
    };
    let (x0, y0) = (idx(r.x0)?, idx(r.y0)?);
    let (x1, y1) = (last(r.x1)?.max(x0), last(r.y1)?.max(y0));
    let n = (x1 - x0 + 1).checked_mul(y1 - y0 + 1)?;
    if n < 0 || n as u64 > max as u64 {
        return None;
    }
    let mut out = Vec::with_capacity(n as usize);
    for y in y0..=y1 {
        for x in x0..=x1 {
            out.push(TileKey { level, x, y });
        }
    }
    Some(out)
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

fn intersect(a: &Rect, b: &Rect) -> Option<Rect> {
    let r = Rect::new(a.x0.max(b.x0), a.y0.max(b.y0), a.x1.min(b.x1), a.y1.min(b.y1));
    (r.x1 > r.x0 && r.y1 > r.y0).then_some(r)
}

/// `a` minus `b`: up to four rectangles (bands above and below, then left and right).
fn subtract(a: &Rect, b: &Rect) -> Vec<Rect> {
    let Some(i) = intersect(a, b) else { return vec![*a] };
    let mut out = Vec::with_capacity(4);
    let mut push = |r: Rect| {
        if r.x1 > r.x0 && r.y1 > r.y0 {
            out.push(r);
        }
    };
    push(Rect::new(a.x0, a.y0, a.x1, i.y0));
    push(Rect::new(a.x0, i.y1, a.x1, a.y1));
    push(Rect::new(a.x0, i.y0, i.x0, i.y1));
    push(Rect::new(i.x1, i.y0, a.x1, i.y1));
    out
}

/// The part of the page with something on it: the paint bounds of the page's layers.
pub fn content_bounds(doc: &Document, page: NodeId) -> Option<Rect> {
    let p = doc.node(page)?;
    p.children
        .iter()
        .filter(|id| doc.node(**id).is_some_and(|n| n.visible))
        .filter_map(|id| crate::draw::paint_bounds(doc, *id))
        .reduce(|a, b| a.union(b))
}

/// The tiles worth having after the view `visible` (page rect) settles at `scale` device pixels per
/// unit, most useful first, given what's on the page (`content`):
///
/// 1. the level below the view's (`floor(log2 scale) - 1`) over the view seen twice as far out;
/// 2. a level that shows all of `content` in a few tiles (zooming out never shows nothing);
/// 3. coarser levels, each over twice the area of the one before, up to [`LEVELS_OUT`].
///
/// At every level a tile costs about as much as the visible area did, whatever the level.
pub fn wanted(visible: Rect, scale: f64, content: Option<Rect>) -> Vec<TileKey> {
    let (Some(content), Some(top)) = (content, level_at_most(scale)) else { return vec![] };
    if !(visible.width() > 0.0 && visible.height() > 0.0 && content.width() >= 0.0 && content.height() >= 0.0) {
        return vec![];
    }
    let side = content.width().max(content.height()).max(1e-9);
    // Two tiles each way show the whole content at this level.
    let overview = level_at_most(2.0 * f64::from(TILE_PX) / side).unwrap_or(MIN_LEVEL).min(top - 1);
    let (cx, cy) = (visible.center().x, visible.center().y);
    let around = |level: i32| -> Vec<TileKey> {
        let grow = level_scale(top - level).min(1e12);
        let (hw, hh) = (visible.width() * grow / 2.0, visible.height() * grow / 2.0);
        let area = Rect::new(cx - hw, cy - hh, cx + hw, cy + hh);
        let Some(r) = intersect(&area, &content.inflate(1.0 / level_scale(level), 1.0 / level_scale(level))) else { return vec![] };
        let mut keys = tiles_in(r, level, 64 * MAX_PER_LEVEL).unwrap_or_default();
        let dist = |k: &TileKey| {
            let c = k.rect().center();
            (c.x - cx).hypot(c.y - cy)
        };
        keys.sort_by(|a, b| dist(a).total_cmp(&dist(b)));
        keys.truncate(MAX_PER_LEVEL);
        keys
    };
    let mut levels = vec![top - 1];
    if overview < top - 1 {
        levels.push(overview);
    }
    for l in (top - LEVELS_OUT..top - 1).rev() {
        if l > overview {
            levels.push(l);
        }
    }
    let mut out = Vec::new();
    for l in levels {
        if (MIN_LEVEL..=MAX_LEVEL).contains(&l) {
            let mut keys = if l == overview { tiles_in(content, l, MAX_PER_LEVEL).unwrap_or_else(|| around(l)) } else { around(l) };
            out.append(&mut keys);
        }
    }
    out
}

/// What a cache has for a tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Have {
    /// Rendered from an older document: an edit touched it since.
    pub stale: bool,
}

/// The exact canvas image: the page rect it shows and its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Exact {
    pub rect: Rect,
    pub scale: f64,
}

/// Where a part of the screen comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Exact,
    Tile(TileKey),
}

/// A part of the screen (page rect) and the image that draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Piece {
    pub rect: Rect,
    pub pick: Pick,
}

/// How well an image of `source` device pixels per unit shows `scale`: 1 when it has at least
/// that many (it is shrunk or shown 1:1), else the share it has (it is magnified, blurry).
fn quality(source: f64, scale: f64) -> f64 {
    if source >= scale * (1.0 - 1e-9) { 1.0 } else { (source / scale).max(0.0) }
}

/// Which image draws each part of `visible` (page rect) at `scale` device pixels per unit: the
/// exact image or the sharpest tile `have` has (stale tiles count half). Parts nothing covers
/// are left out (the page's background shows). The exact image wins ties: it is the newest.
pub fn compose(visible: Rect, scale: f64, exact: Option<Exact>, have: impl Fn(TileKey) -> Option<Have>) -> Vec<Piece> {
    let qe = exact.map(|e| quality(e.scale, scale));
    // At rest the exact image covers the view: draw it whole.
    if let (Some(e), Some(q)) = (exact, qe)
        && q >= 1.0
        && e.rect.x0 <= visible.x0
        && e.rect.y0 <= visible.y0
        && e.rect.x1 >= visible.x1
        && e.rect.y1 >= visible.y1
    {
        return vec![Piece { rect: visible, pick: Pick::Exact }];
    }
    let exact_only = || exact.and_then(|e| intersect(&visible, &e.rect)).map(|r| vec![Piece { rect: r, pick: Pick::Exact }]).unwrap_or_default();
    let Some(cell_level) = level_at_least(scale) else { return exact_only() };
    let Some(cells) = tiles_in(visible, cell_level, MAX_CELLS) else { return exact_only() };
    // The sharpest tile over a cell: the cell's own tile or the nearest coarser one it has.
    let best_tile = |cell: &TileKey| -> Option<(TileKey, f64)> {
        let mut best: Option<(TileKey, f64)> = None;
        for up in 0..=LOOK_UP {
            let k = cell.ancestor(up);
            if k.level < MIN_LEVEL {
                break;
            }
            if let Some(h) = have(k) {
                let q = quality(k.scale(), scale) * if h.stale { 0.5 } else { 1.0 };
                if best.is_none_or(|(_, b)| q > b) {
                    best = Some((k, q));
                }
                if !h.stale {
                    break;
                }
            }
        }
        best
    };
    let mut out = Vec::new();
    for cell in &cells {
        let Some(c) = intersect(&cell.rect(), &visible) else { continue };
        let tile = best_tile(cell);
        match (exact, qe) {
            (Some(e), Some(q)) if overlaps(&c, &e.rect) => {
                let inside = intersect(&c, &e.rect);
                let pick_inside = match tile {
                    Some((k, qt)) if qt > q => Pick::Tile(k),
                    _ => Pick::Exact,
                };
                if let Some(r) = inside {
                    out.push(Piece { rect: r, pick: pick_inside });
                }
                if let Some((k, _)) = tile {
                    for r in subtract(&c, &e.rect) {
                        out.push(Piece { rect: r, pick: Pick::Tile(k) });
                    }
                }
            }
            _ => {
                if let Some((k, _)) = tile {
                    out.push(Piece { rect: c, pick: Pick::Tile(k) });
                }
            }
        }
    }
    out
}

/// A painted tile.
#[derive(Clone, Debug)]
pub enum Painted {
    /// Only the page's background: nothing to keep but the fact.
    Empty,
    Pixels(Pixmap),
}

/// Paints tiles: [`CpuTilePainter`], or a GPU painter behind the same calls.
pub trait TilePainter: Send {
    /// Paint `tiles` of `page`; `None` for a tile that couldn't be painted. Once `stop` says so
    /// (a canvas render is waiting), tiles not started yet are left out of the result: the caller
    /// paints them later.
    fn paint(
        &mut self,
        doc: &Arc<Document>,
        page: NodeId,
        tiles: &[TileKey],
        opts: &Options,
        stop: &(dyn Fn() -> bool + Sync),
    ) -> Vec<(TileKey, Option<Painted>)>;

    /// What paints and how the last batch went (`ui.inspect`).
    fn report(&self) -> String {
        "cpu".into()
    }
}

/// The CPU painter: tiny-skia, tiles in parallel on desktop.
#[derive(Default)]
pub struct CpuTilePainter;

impl TilePainter for CpuTilePainter {
    fn paint(
        &mut self,
        doc: &Arc<Document>,
        page: NodeId,
        tiles: &[TileKey],
        opts: &Options,
        stop: &(dyn Fn() -> bool + Sync),
    ) -> Vec<(TileKey, Option<Painted>)> {
        let content = content_bounds(doc, page);
        let one = |k: &TileKey| -> Option<(TileKey, Option<Painted>)> {
            if stop() {
                return None;
            }
            // A tile off everything on the page is only background.
            let off = content.is_none_or(|c| !overlaps(&c.inflate(2.0 / k.scale(), 2.0 / k.scale()), &k.rect()));
            if off && opts.solo.is_none() {
                return Some((*k, Some(Painted::Empty)));
            }
            Some((*k, render_tile(doc, page, k, opts)))
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            use rayon::prelude::*;
            tiles.par_iter().filter_map(one).collect()
        }
        #[cfg(target_arch = "wasm32")]
        {
            tiles.iter().filter_map(one).collect()
        }
    }
}

/// Render one tile: its square, with what background blurs read around it rendered too and cut
/// off, so a blur at a tile's edge looks as it does across the canvas.
pub fn render_tile(doc: &Document, page: NodeId, key: &TileKey, opts: &Options) -> Option<Painted> {
    let s = key.scale();
    let reach = crate::backdrop_reach(crate::max_backdrop_blur(doc), s).ceil();
    let margin = if reach.is_finite() { reach.clamp(0.0, MAX_MARGIN_PX) as u32 + 1 } else { 1 };
    let u = tile_units(key.level);
    let px = (key.x as f64 * f64::from(TILE_PX), key.y as f64 * f64::from(TILE_PX));
    let m = f64::from(margin);
    let view = View { origin: ((px.0 - m) / s, (px.1 - m) / s), scale: s, width: TILE_PX + 2 * margin, height: TILE_PX + 2 * margin };
    if !(view.origin.0.is_finite() && view.origin.1.is_finite() && u.is_finite()) {
        return None;
    }
    let full = render_view(doc, page, &view, opts)?;
    let pix = full.clone_rect(IntRect::from_xywh(i32::try_from(margin).ok()?, i32::try_from(margin).ok()?, TILE_PX, TILE_PX)?)?;
    let first = pix.pixels().first().copied();
    let bg_only = first.is_some_and(|f| pix.pixels().iter().all(|p| *p == f)) && first.is_some_and(|f| is_page_background(doc, page, opts, f));
    Some(if bg_only { Painted::Empty } else { Painted::Pixels(pix) })
}

/// Whether `px` is what the page's background paints (nothing else on the tile).
fn is_page_background(doc: &Document, page: NodeId, opts: &Options, px: tiny_skia::PremultipliedColorU8) -> bool {
    let bg = opts.background.or_else(|| match doc.node(page).map(|p| &p.kind) {
        Some(figcraft_doc::NodeKind::Page { background, .. }) => Some(*background),
        _ => None,
    });
    let Some(bg) = bg else { return px.alpha() == 0 };
    let mut one = match Pixmap::new(1, 1) {
        Some(p) => p,
        None => return false,
    };
    one.fill(crate::paint::ts_color(bg));
    one.pixels().first().is_some_and(|b| *b == px)
}

/// Page areas that differ between `old` and `new` on `page` (where layers were and are, with what
/// paints from them); `None` when everything may have (the page itself, a pattern source, too many
/// changes or another colour profile).
pub fn changed(old: &Arc<Document>, new: &Arc<Document>, page: NodeId) -> Option<Vec<Rect>> {
    if Arc::ptr_eq(old, new) {
        return Some(vec![]);
    }
    crate::retained::changed_page_areas(old, new, page)
}

/// Whether a change in `areas` shows on tile `key`, given the largest background blur radius
/// `backdrop` (a blur reads that far around it).
pub fn touched(key: &TileKey, areas: &[Rect], backdrop: f64) -> bool {
    let s = key.scale();
    let reach = (crate::backdrop_reach(backdrop, s) + 2.0) / s;
    let r = key.rect().inflate(reach, reach);
    areas.iter().any(|a| overlaps(a, &r) || (a.width() == 0.0 && a.height() == 0.0 && r.contains(a.origin())))
}

/// The largest background blur radius in `doc` (see [`touched`]).
pub fn backdrop_radius(doc: &Document) -> f64 {
    crate::max_backdrop_blur(doc)
}

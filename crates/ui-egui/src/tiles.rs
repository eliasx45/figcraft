//! The canvas's tile cache: tiles ([`figcraft_render::tiles`]) the render thread painted, kept as
//! GPU textures so the canvas can show something sharp, or at least something, the moment the
//! zoom changes, before the exact image for the new zoom arrives.
//!
//! - Tiles are kept for one file, page and set of render options; another one clears them.
//! - An edit marks the tiles it touches stale (still shown when nothing better is there, and
//!   painted again); tiles painted from an older document than the current one count as stale
//!   only when a change since touched them.
//! - Tiles are asked for when the canvas render is done and the document didn't change in the
//!   frame (so never while a drag edits every frame); the cache never asks for frames of its own.
//!   Up to [`budget`] textures are kept (least recently drawn go first).

use std::collections::HashMap;
use std::sync::Arc;

use figcraft_doc::{Document, NodeId};
use figcraft_geom::Rect;
use figcraft_render::tiles::{Have, TILE_PX, TileKey, changed, content_bounds, level_at_most, touched, wanted};

use crate::render_worker::{TileBatch, TileDone, TileOpts};

/// Most tiles kept that have pixels (a megabyte of GPU memory each).
pub fn budget() -> usize {
    if cfg!(target_arch = "wasm32") { 96 } else { 256 }
}
/// Most tiles kept in all (empty ones have no texture).
const MAX_ENTRIES: usize = 8192;

struct Entry {
    tex: Option<egui::TextureHandle>,
    stale: bool,
    /// The frame it was last drawn in.
    used: u64,
}

/// What a plan was made for: asking again for the same is pointless.
#[derive(Clone, PartialEq)]
struct Asked {
    generation: u64,
    doc: usize,
    level: i32,
    cell: (i64, i64),
}

#[derive(Default)]
pub struct TileCache {
    /// Bumped whenever the tiles are dropped: tiles painted before are thrown away.
    generation: u64,
    of: Option<(u64, NodeId, TileOpts)>,
    doc: Option<Arc<Document>>,
    /// The document the tiles' staleness was last worked out for, when `doc` has moved on since
    /// (worked out when tiles are needed: a drag edits every frame).
    pending: Option<Arc<Document>>,
    /// The largest background blur radius in `doc` (what an edit's reach grows by).
    backdrop: f64,
    tiles: HashMap<TileKey, Entry>,
    /// Changes from older documents to `doc`, by document address (tiles painted from them).
    changes: HashMap<usize, (Arc<Document>, Option<Vec<Rect>>)>,
    content: Option<(usize, Option<Rect>)>,
    asked: Option<Asked>,
    frame: u64,
    /// Tiles drawn in the last frame (`ui.inspect`).
    pub drawn: usize,
}

impl TileCache {
    pub fn clear(&mut self) {
        self.tiles.clear();
        self.pending = None;
        self.changes.clear();
        self.asked = None;
        self.generation += 1;
    }

    /// Keep the tiles of file `uid`, `page` and `opts` showing `doc`: other ones drop them, a newer
    /// document marks what it changed stale. Returns whether the document is another one.
    pub fn sync(&mut self, uid: u64, page: NodeId, opts: &TileOpts, doc: &Arc<Document>) -> bool {
        if self.of.as_ref().is_none_or(|(u, p, o)| *u != uid || *p != page || o != opts) {
            self.clear();
            self.of = Some((uid, page, opts.clone()));
            self.doc = Some(doc.clone());
            self.backdrop = figcraft_render::tiles::backdrop_radius(doc);
            return true;
        }
        let Some(old) = self.doc.clone() else {
            self.doc = Some(doc.clone());
            return true;
        };
        if Arc::ptr_eq(&old, doc) {
            return false;
        }
        if self.pending.is_none() && !self.tiles.is_empty() {
            self.pending = Some(old);
        }
        self.changes.clear();
        self.doc = Some(doc.clone());
        true
    }

    /// Mark stale what changed since the tiles' staleness was last worked out.
    pub fn resolve(&mut self) {
        let (Some(old), Some(doc), Some((_, page, _))) = (self.pending.take(), self.doc.clone(), self.of.as_ref()) else { return };
        let page = *page;
        let backdrop = figcraft_render::tiles::backdrop_radius(&doc);
        let reach = backdrop.max(self.backdrop);
        match changed(&old, &doc, page) {
            Some(areas) => {
                for (k, e) in &mut self.tiles {
                    if !e.stale && touched(k, &areas, reach) {
                        e.stale = true;
                    }
                }
            }
            None => self.tiles.values_mut().for_each(|e| e.stale = true),
        }
        self.backdrop = backdrop;
    }

    /// Whether a tile painted from `from` still shows the current document.
    fn fresh(&mut self, from: &Arc<Document>, key: &TileKey) -> bool {
        let (Some(doc), Some((_, page, _))) = (self.doc.clone(), self.of.as_ref()) else { return false };
        if Arc::ptr_eq(from, &doc) {
            return true;
        }
        let page = *page;
        let addr = Arc::as_ptr(from) as usize;
        let (_, areas) = self.changes.entry(addr).or_insert_with(|| (from.clone(), changed(from, &doc, page)));
        areas.as_ref().is_some_and(|a| !touched(key, a, self.backdrop))
    }

    /// Keep a painted tile.
    pub fn insert(&mut self, ctx: &egui::Context, d: TileDone) {
        if d.generation != self.generation {
            return;
        }
        self.resolve();
        let stale = !self.fresh(&d.doc, &d.key);
        let tex = d.image.map(|image| {
            let name = format!("figcraft-tile-{}-{}-{}", d.key.level, d.key.x, d.key.y);
            match self.tiles.remove(&d.key).and_then(|e| e.tex) {
                Some(mut t) => {
                    t.set(image, egui::TextureOptions::LINEAR);
                    t
                }
                None => ctx.load_texture(name, image, egui::TextureOptions::LINEAR),
            }
        });
        self.tiles.insert(d.key, Entry { tex, stale, used: self.frame });
        self.evict();
    }

    /// What the cache has for `key`.
    pub fn have(&self, key: TileKey) -> Option<Have> {
        self.tiles.get(&key).map(|e| Have { stale: e.stale })
    }

    /// The texture of `key` (none for a tile with only background), counted as drawn this frame.
    pub fn texture(&mut self, key: TileKey) -> Option<egui::TextureId> {
        let frame = self.frame;
        let e = self.tiles.get_mut(&key)?;
        e.used = frame;
        e.tex.as_ref().map(egui::TextureHandle::id)
    }

    /// A new frame: tiles drawn from now on count as used in it.
    pub fn begin_frame(&mut self) {
        self.frame += 1;
        self.drawn = 0;
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    pub fn textures(&self) -> usize {
        self.tiles.values().filter(|e| e.tex.is_some()).count()
    }

    /// Tiles known to be stale (an edit not yet worked out isn't counted: see [`Self::resolve`]).
    pub fn stale(&self) -> usize {
        self.tiles.values().filter(|e| e.stale).count()
    }

    /// Drop the least recently drawn tiles beyond the budget (never ones drawn this frame).
    fn evict(&mut self) {
        let textured = self.textures();
        let over_tex = textured.saturating_sub(budget());
        let over_all = self.tiles.len().saturating_sub(MAX_ENTRIES);
        if over_tex == 0 && over_all == 0 {
            return;
        }
        let frame = self.frame;
        let mut old: Vec<(u64, bool, TileKey)> =
            self.tiles.iter().filter(|(_, e)| e.used < frame).map(|(k, e)| (e.used, e.tex.is_some(), *k)).collect();
        old.sort_by_key(|(used, ..)| *used);
        let (mut tex_left, mut all_left) = (over_tex, over_all);
        for (_, has_tex, k) in old {
            if tex_left == 0 && all_left == 0 {
                break;
            }
            if has_tex && tex_left > 0 {
                tex_left -= 1;
                all_left = all_left.saturating_sub(1);
                self.tiles.remove(&k);
            } else if all_left > 0 {
                all_left -= 1;
                self.tiles.remove(&k);
            }
        }
    }

    /// The tiles to paint for `visible` (page rect) at `scale` device pixels per unit, unless they
    /// have been asked for already: missing and stale tiles of [`wanted`], as a batch for the
    /// render thread.
    pub fn plan(&mut self, visible: Rect, scale: f64) -> Option<TileBatch> {
        let (Some((_, page, opts)), Some(doc)) = (self.of.clone(), self.doc.clone()) else { return None };
        self.resolve();
        let top = level_at_most(scale)?;
        // Ask again once the view has moved by a tile of the coarsest level asked for at its size.
        let unit = f64::from(TILE_PX) / figcraft_render::tiles::level_scale(top - 1);
        let c = visible.center();
        let asked = Asked {
            generation: self.generation,
            doc: Arc::as_ptr(&doc) as usize,
            level: top,
            cell: ((c.x / unit).floor() as i64, (c.y / unit).floor() as i64),
        };
        if self.asked.as_ref() == Some(&asked) {
            return None;
        }
        self.asked = Some(asked);
        let addr = Arc::as_ptr(&doc) as usize;
        let content = match self.content {
            Some((a, c)) if a == addr => c,
            _ => {
                let c = content_bounds(&doc, page);
                self.content = Some((addr, c));
                c
            }
        };
        let keys: Vec<TileKey> = wanted(visible, scale, content).into_iter().filter(|k| self.tiles.get(k).is_none_or(|e| e.stale)).collect();
        if keys.is_empty() {
            return None;
        }
        Some(TileBatch { seq: 0, doc, page, generation: self.generation, opts, keys })
    }
}

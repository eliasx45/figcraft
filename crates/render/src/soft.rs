//! Soft drop shadows kept between renders. A node's shadows blurred by many pixels are cast from
//! a copy of it drawn smaller (`draw::SoftCaster`); at one zoom that copy and those casts are the
//! same whichever band, tile or pan of the view asks for them, so they are worked out once over
//! the node's whole footprint and kept here. Each render only scales up the part it shows: the
//! canvas's bands and tiles, and the strips a pan uncovers, each cast the shadows again over
//! themselves and the shadows' reach (on a page of large frames, ten times the work and more).
//!
//! An entry stands for the node's subtree as it was (the nodes are kept, and compared by
//! pointer: an edit anywhere below replaces one) at one scale and transform, level of detail and
//! font generation (still renders only: a playing GIF's frames are drawn once each). Entries are
//! dropped least recently used first past a byte budget.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use figcraft_doc::{Document, Node, Shadow};
use figcraft_geom::Affine;
use tiny_skia::{IntRect, Pixmap};

/// A node's soft shadows at one zoom: what they are cast from and the casts so far.
pub(crate) struct Entry {
    /// The device pixels the copies cover, in the pixels of the transform `xf` maps to.
    pub region: IntRect,
    /// The node's local → device pixels transform the copies were drawn for.
    pub xf: Affine,
    /// The copies so far, each half the size of the one before (`k`, `2k`, …), with their local →
    /// copy pixels transforms.
    pub copies: Vec<(Pixmap, Affine)>,
    /// Each shadow's alpha cast so far ([`shadow_key`]) and the factor it was cast smaller by.
    pub casts: Vec<(u64, Arc<Pixmap>, u32)>,
}

impl Entry {
    pub fn new(region: IntRect, xf: Affine) -> Self {
        Entry { region, xf, copies: Vec::new(), casts: Vec::new() }
    }

    /// The bytes its pixmaps hold.
    pub fn bytes(&self) -> u64 {
        let px = |p: &Pixmap| u64::from(p.width()) * u64::from(p.height()) * 4;
        self.copies.iter().map(|(p, _)| px(p)).sum::<u64>() + self.casts.iter().map(|(_, p, _)| px(p)).sum::<u64>()
    }
}

/// What tells two casts of a shadow apart: its blur, spread and offset (its colour, blend mode
/// and whether it shows behind the layer are applied afterwards, to each render's pixels).
pub(crate) fn shadow_key(s: &Shadow) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (s.radius.to_bits(), s.spread.to_bits(), s.offset.0.to_bits(), s.offset.1.to_bits()).hash(&mut h);
    h.finish()
}

/// What the entry of node `n` is kept for, besides its subtree.
pub(crate) struct Key {
    pub scale: f64,
    /// The local → device transform's linear part (its translation only moves the shadows).
    pub linear: [f64; 4],
    /// The factor the first copy is drawn smaller by.
    pub k: u32,
    pub lod: bool,
    /// Which cell of a large node's footprint (`None`: all of it).
    pub cell: Option<(i32, i32)>,
}

/// The kept pixmaps past which the least recently used entries go.
const BUDGET: u64 = 256 << 20;
/// The most entries kept (small layers' entries cost little but each is scanned when one goes).
const MAX_ENTRIES: usize = 4096;
/// What each kept layer counts for besides its pixmaps (it is kept alive with the entry).
const NODE_BYTES: u64 = 512;

/// What the entry was drawn from, kept alive so the pointers it is compared by can't be reused:
/// the subtree's nodes and the bytes of the images they paint (a missing image relinked under its
/// own hash changes those and no layer).
#[derive(Default)]
struct Drawn {
    nodes: Vec<Arc<Node>>,
    images: Vec<Option<Arc<Vec<u8>>>>,
}

impl Drawn {
    fn of(doc: &Document, n: &Node) -> Self {
        let mut d = Drawn::default();
        for id in doc.subtree(n.id) {
            let Some(node) = doc.node_arc(id) else { continue };
            for p in node.fills.iter().chain(&node.strokes) {
                if let figcraft_color::PaintKind::Image(img) = &p.kind {
                    d.images.push(doc.images.get(&img.image).map(|i| i.bytes.clone()));
                }
            }
            d.nodes.push(node.clone());
        }
        d
    }

    fn same(&self, other: &Drawn) -> bool {
        let img = |a: &Option<Arc<Vec<u8>>>, b: &Option<Arc<Vec<u8>>>| match (a, b) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        self.nodes.len() == other.nodes.len()
            && self.images.len() == other.images.len()
            && self.nodes.iter().zip(&other.nodes).all(|(a, b)| Arc::ptr_eq(a, b))
            && self.images.iter().zip(&other.images).all(|(a, b)| img(a, b))
    }
}

struct Slot {
    drawn: Drawn,
    entry: Arc<Mutex<Entry>>,
    /// Told apart from an entry it replaced, whose late size reports are then ignored.
    serial: u64,
    bytes: u64,
    used: u64,
}

#[derive(Default)]
struct Cache {
    /// By key, a few entries apart (another window's document, or another version of this one,
    /// can have a layer of the same id).
    map: HashMap<u64, Vec<Slot>>,
    entries: usize,
    bytes: u64,
    clock: u64,
}

/// The most entries one key holds.
const PER_KEY: usize = 4;

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache::default()))
}

/// The blur radius of each shadow cast so far (tests tell theirs apart by an unusual radius).
#[cfg(test)]
pub(crate) static CASTS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// Which kept entry a [`note`] is about.
#[derive(Clone, Copy)]
pub(crate) struct Ticket {
    id: u64,
    serial: u64,
}

/// The kept entry of node `n` of `doc` for `key`, made by `make` when there is none or what it
/// was drawn from changed since; with the ticket to report its size by ([`note`]).
pub(crate) fn shared(doc: &Document, n: &Node, key: &Key, make: impl FnOnce() -> Entry) -> (Arc<Mutex<Entry>>, Ticket) {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    n.id.hash(&mut h);
    (key.scale.to_bits(), key.linear.map(f64::to_bits), key.k, key.lod, key.cell, figcraft_text::layout_generation()).hash(&mut h);
    let id = h.finish();
    let drawn = Drawn::of(doc, n);
    let mut c = cache().lock().unwrap_or_else(PoisonError::into_inner);
    c.clock += 1;
    let now = c.clock;
    if let Some(slot) = c.map.get_mut(&id).and_then(|v| v.iter_mut().find(|s| s.drawn.same(&drawn))) {
        slot.used = now;
        return (slot.entry.clone(), Ticket { id, serial: slot.serial });
    }
    let entry = Arc::new(Mutex::new(make()));
    let bytes = drawn.nodes.len() as u64 * NODE_BYTES;
    let slots = c.map.entry(id).or_default();
    let mut freed = 0;
    let mut dropped = 0;
    if slots.len() >= PER_KEY
        && let Some(i) = slots.iter().enumerate().min_by_key(|(_, s)| s.used).map(|(i, _)| i)
    {
        freed = slots.swap_remove(i).bytes;
        dropped = 1;
    }
    slots.push(Slot { drawn, entry: entry.clone(), serial: now, bytes, used: now });
    c.entries = c.entries + 1 - dropped;
    c.bytes = c.bytes.saturating_sub(freed) + bytes;
    evict(&mut c, now);
    (entry, Ticket { id, serial: now })
}

/// The entry `t` is about now holds `bytes` of pixmaps.
pub(crate) fn note(t: Ticket, bytes: u64) {
    let mut c = cache().lock().unwrap_or_else(PoisonError::into_inner);
    let Some(slot) = c.map.get_mut(&t.id).and_then(|v| v.iter_mut().find(|s| s.serial == t.serial)) else { return };
    let bytes = bytes + slot.drawn.nodes.len() as u64 * NODE_BYTES;
    let before = std::mem::replace(&mut slot.bytes, bytes);
    c.bytes = c.bytes.saturating_sub(before) + bytes;
    evict(&mut c, t.serial);
}

/// Past the budget or the count, drop the least recently used entries but the one made at `keep`.
fn evict(c: &mut Cache, keep: u64) {
    while (c.bytes > BUDGET || c.entries > MAX_ENTRIES) && c.entries > 1 {
        let oldest = c
            .map
            .iter()
            .flat_map(|(k, v)| v.iter().map(move |s| (*k, s.serial, s.used)))
            .filter(|(_, serial, _)| *serial != keep)
            .min_by_key(|(_, _, used)| *used);
        let Some((key, serial, _)) = oldest else { break };
        let Some(slots) = c.map.get_mut(&key) else { break };
        let Some(i) = slots.iter().position(|s| s.serial == serial) else { break };
        let gone = slots.swap_remove(i);
        if slots.is_empty() {
            c.map.remove(&key);
        }
        c.entries = c.entries.saturating_sub(1);
        c.bytes = c.bytes.saturating_sub(gone.bytes);
    }
}

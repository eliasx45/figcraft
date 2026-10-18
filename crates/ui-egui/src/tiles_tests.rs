//! The canvas's tile cache: tiles painted by the worker are kept, an edit marks only the tiles it
//! touches stale, other options drop them, the budget holds, and the same tiles aren't asked for
//! twice.

use std::sync::Arc;

use figcraft_engine::Session;
use figcraft_engine::params::id_of;
use figcraft_geom::Rect;
use figcraft_render::tiles::TileKey;
use serde_json::json;

use crate::render_worker::{Inline, TileBatch, TileDone, TileOpts};
use crate::tiles::{TileCache, budget};

fn opts() -> TileOpts {
    TileOpts { outline: false, outline_extras: figcraft_render::Outline::default(), outlined: vec![], solo: None, proof: false, fonts: 0 }
}

/// A rectangle near the origin and one far to the right.
fn session() -> (Session, figcraft_doc::NodeId) {
    let mut s = Session::new();
    let near = id_of(&s.execute("shape.rectangle", &json!({"x": 40, "y": 40, "width": 200, "height": 120})).unwrap()["id"]).unwrap();
    s.execute("shape.rectangle", &json!({"x": 5000, "y": 40, "width": 200, "height": 120})).unwrap();
    (s, near)
}

fn paint(ctx: &egui::Context, batch: TileBatch) -> Vec<TileDone> {
    let mut w = Inline::new(ctx.clone());
    w.submit_tiles(batch);
    let mut out = Vec::new();
    for _ in 0..100 {
        let got = w.poll_tiles();
        if got.is_empty() {
            break;
        }
        out.extend(got);
    }
    out
}

#[test]
fn painted_tiles_are_kept_and_an_edit_stales_only_what_it_touches() {
    let ctx = egui::Context::default();
    let (mut s, near) = session();
    let (uid, page, doc) = {
        let ds = s.ds().unwrap();
        (ds.uid, ds.page, ds.doc.clone())
    };
    let mut cache = TileCache::default();
    cache.sync(uid, page, &opts(), &doc);
    let batch = cache.plan(Rect::new(0.0, 0.0, 1200.0, 800.0), 2.0).expect("tiles to paint");
    assert!(cache.plan(Rect::new(0.0, 0.0, 1200.0, 800.0), 2.0).is_none(), "asked once");
    let keys = batch.keys.clone();
    let done = paint(&ctx, batch);
    assert_eq!(done.len(), keys.len(), "every tile painted");
    assert!(done.iter().any(|d| d.image.is_some()), "some with pixels");
    for d in done {
        cache.insert(&ctx, d);
    }
    assert_eq!(cache.len(), keys.len());
    assert_eq!(cache.stale(), 0);
    // Move the near rectangle a little: tiles around it go stale, the far one's don't.
    s.execute("transform.move", &json!({"ids": [near], "dx": 30, "dy": 0})).unwrap();
    let doc2 = s.ds().unwrap().doc.clone();
    assert!(!Arc::ptr_eq(&doc, &doc2));
    cache.sync(uid, page, &opts(), &doc2);
    assert_eq!(cache.stale(), 0, "worked out only when needed");
    cache.resolve();
    let stale: Vec<TileKey> = keys.iter().copied().filter(|k| cache.have(*k).is_some_and(|h| h.stale)).collect();
    assert!(!stale.is_empty(), "the tiles under the edit");
    assert!(stale.len() < keys.len(), "not all of them");
    for k in &stale {
        assert!(k.rect().intersect(Rect::new(0.0, 0.0, 300.0, 200.0)).area() > 0.0, "{k:?} is nowhere near the edit");
    }
    // Asked again for the new document: only the stale ones.
    let again = cache.plan(Rect::new(0.0, 0.0, 1200.0, 800.0), 2.0).expect("the stale tiles");
    assert!(again.keys.iter().all(|k| stale.contains(k)));
}

#[test]
fn tiles_painted_from_an_older_document_are_fresh_unless_a_change_touched_them() {
    let ctx = egui::Context::default();
    let (mut s, near) = session();
    let (uid, page, doc) = {
        let ds = s.ds().unwrap();
        (ds.uid, ds.page, ds.doc.clone())
    };
    let mut cache = TileCache::default();
    cache.sync(uid, page, &opts(), &doc);
    let batch = cache.plan(Rect::new(0.0, 0.0, 1200.0, 800.0), 2.0).expect("tiles to paint");
    let done = paint(&ctx, batch);
    // The document changes while the tiles were being painted.
    s.execute("transform.move", &json!({"ids": [near], "dx": 30, "dy": 0})).unwrap();
    cache.sync(uid, page, &opts(), &s.ds().unwrap().doc.clone());
    let n = done.len();
    for d in done {
        cache.insert(&ctx, d);
    }
    assert!(cache.stale() > 0 && cache.stale() < n, "{} of {n} stale", cache.stale());
}

#[test]
fn other_options_or_pages_drop_the_tiles_and_their_late_arrivals() {
    let ctx = egui::Context::default();
    let (s, _) = session();
    let ds = s.ds().unwrap();
    let mut cache = TileCache::default();
    cache.sync(ds.uid, ds.page, &opts(), &ds.doc);
    let batch = cache.plan(Rect::new(0.0, 0.0, 1200.0, 800.0), 2.0).expect("tiles to paint");
    let done = paint(&ctx, batch);
    // Outline mode: the tiles show something else now.
    cache.sync(ds.uid, ds.page, &TileOpts { outline: true, ..opts() }, &ds.doc);
    for d in done {
        cache.insert(&ctx, d);
    }
    assert_eq!(cache.len(), 0, "tiles of the old options never land");
}

#[test]
fn the_texture_budget_holds() {
    let ctx = egui::Context::default();
    let (s, _) = session();
    let ds = s.ds().unwrap();
    let mut cache = TileCache::default();
    cache.sync(ds.uid, ds.page, &opts(), &ds.doc);
    let image = egui::ColorImage::new([4, 4], vec![egui::Color32::RED; 16]);
    // Generation 1: the one `sync` started.
    for i in 0..(budget() as i64 + 40) {
        cache.begin_frame();
        cache.insert(&ctx, TileDone { key: TileKey { level: 0, x: i, y: 0 }, image: Some(image.clone()), doc: ds.doc.clone(), generation: 1 });
    }
    assert_eq!(cache.textures(), budget());
    // The newest stay.
    assert!(cache.have(TileKey { level: 0, x: budget() as i64 + 39, y: 0 }).is_some());
    assert!(cache.have(TileKey { level: 0, x: 0, y: 0 }).is_none());
}

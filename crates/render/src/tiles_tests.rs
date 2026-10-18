//! Canvas tiles: tiles stitch into exactly the image a render of the same area gives, plans stay
//! bounded, compositing draws every part of the screen once, and edits find the tiles they touch.

use std::sync::Arc;

use figcraft_color::{Color, Paint};
use figcraft_doc::{Blur, Document, Effect, EffectKind, Node, NodeId, NodeKind};
use figcraft_geom::Rect;

use crate::tiles::{
    CpuTilePainter, Exact, Have, MAX_PER_LEVEL, Painted, Pick, TILE_PX, TileKey, TilePainter, changed, compose, content_bounds, level_at_least,
    level_at_most, render_tile, tiles_in, touched, wanted,
};
use crate::{Options, Pixmap, View, render_view};

fn solid(n: &mut Node, hex: &str) {
    n.fills = vec![Paint::solid(Color::from_hex(hex).unwrap())];
}

/// Shapes across the tile edges of levels 0 and below (512 page units at level 0).
fn scene() -> (Arc<Document>, NodeId, NodeId) {
    let mut d = Document::new("tiles");
    let page = d.first_page().unwrap();
    let a = d.create(page, NodeKind::Rectangle, "A", 400.0, 380.0, 260.0, 300.0).unwrap();
    solid(d.get_mut(a).unwrap(), "E03030");
    let e = d.create(page, NodeKind::Ellipse { start: 0.0, sweep: std::f64::consts::TAU, inner: 0.0 }, "E", 480.0, 470.0, 90.0, 90.0).unwrap();
    solid(d.get_mut(e).unwrap(), "3070E0");
    let f = d.create(page, NodeKind::Frame, "F", -300.0, -200.0, 900.0, 140.0).unwrap();
    solid(d.get_mut(f).unwrap(), "20A060");
    for i in 0..40 {
        let r = d.create(page, NodeKind::Rectangle, "dot", -400.0 + f64::from(i) * 37.0, 900.0 + f64::from(i % 5) * 11.0, 3.0, 3.0).unwrap();
        solid(d.get_mut(r).unwrap(), "F0B020");
    }
    (Arc::new(d), page, a)
}

fn pixels(p: &Painted) -> &Pixmap {
    match p {
        Painted::Pixels(p) => p,
        Painted::Empty => panic!("expected pixels"),
    }
}

/// The tiles `(x0..=x1, y0..=y1)` of `level` side by side, compared with one render of their area
/// (equal up to anti-aliasing, as partial renders are: see `retained_tests::assert_same`).
fn stitched_matches_render(doc: &Document, page: NodeId, level: i32, (x0, x1): (i64, i64), (y0, y1): (i64, i64), opts: &Options) {
    let s = TileKey { level, x: 0, y: 0 }.scale();
    let t = TILE_PX;
    let (cols, rows) = ((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    let origin = TileKey { level, x: x0, y: y0 }.rect();
    let view = View { origin: (origin.x0, origin.y0), scale: s, width: cols * t, height: rows * t };
    let whole = render_view(doc, page, &view, opts).unwrap();
    let mut stitched = Pixmap::new(cols * t, rows * t).unwrap();
    let Some(figcraft_doc::NodeKind::Page { background, .. }) = doc.node(page).map(|p| &p.kind) else { panic!("no page") };
    stitched.fill(crate::paint::ts_color(*background));
    let mut empty = 0;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let key = TileKey { level, x, y };
            match render_tile(doc, page, &key, opts).unwrap() {
                Painted::Empty => empty += 1,
                Painted::Pixels(p) => stitched.draw_pixmap(
                    ((x - x0) as u32 * t) as i32,
                    ((y - y0) as u32 * t) as i32,
                    p.as_ref(),
                    &tiny_skia::PixmapPaint { blend_mode: tiny_skia::BlendMode::Source, ..Default::default() },
                    tiny_skia::Transform::identity(),
                    None,
                ),
            }
        }
    }
    assert!(empty < (cols * rows) as usize, "something rendered");
    crate::retained_tests::assert_same(&stitched, &whole, &format!("tiles of level {level}"));
}

#[test]
fn tiles_stitch_into_the_render_of_their_area() {
    let (doc, page, _) = scene();
    stitched_matches_render(&doc, page, 0, (0, 1), (0, 1), &Options::default());
    // Zoomed out, with levels of detail: the small dots are splatted, still on one pixel grid.
    stitched_matches_render(&doc, page, -2, (-1, 0), (-1, 0), &Options { lod: true, ..Default::default() });
}

#[test]
fn background_blurs_at_a_tile_edge_read_across_it() {
    let (doc, page, a) = scene();
    let mut d = (*doc).clone();
    let glass = d.create(page, NodeKind::Rectangle, "Blur", 470.0, 400.0, 100.0, 200.0).unwrap();
    d.get_mut(glass).unwrap().fills = vec![Paint::solid(Color::from_hex("FFFFFF80").unwrap())];
    d.get_mut(glass).unwrap().effects.push(Effect { kind: EffectKind::BackgroundBlur(Blur { radius: 12.0, ..Blur::default() }), visible: true });
    let _ = a;
    stitched_matches_render(&d, page, 0, (0, 1), (0, 1), &Options::default());
}

#[test]
fn tiles_off_the_content_are_empty_without_rendering() {
    let (doc, page, _) = scene();
    let far = TileKey { level: 0, x: 40, y: -30 };
    let painted = CpuTilePainter.paint(&doc, page, &[far, TileKey { level: 0, x: 0, y: 0 }], &Options::default(), &|| false);
    assert!(matches!(painted[0], (k, Some(Painted::Empty)) if k == far));
    assert!(matches!(painted[1].1, Some(Painted::Pixels(_))));
    // Rendered and found blank: empty too.
    assert!(matches!(render_tile(&doc, page, &far, &Options::default()), Some(Painted::Empty)));
    assert_eq!(pixels(painted[1].1.as_ref().unwrap()).width(), TILE_PX);
}

#[test]
fn levels_nest_and_edges_belong_to_one_tile() {
    let k = TileKey { level: 2, x: -3, y: 5 };
    let parent = k.ancestor(1);
    assert_eq!(parent, TileKey { level: 1, x: -2, y: 2 });
    let (pr, kr) = (parent.rect(), k.rect());
    assert!(pr.x0 <= kr.x0 && pr.y0 <= kr.y0 && pr.x1 >= kr.x1 && pr.y1 >= kr.y1);
    assert_eq!(pr.width(), 2.0 * kr.width());
    // A rect ending on a tile edge doesn't touch the next tile.
    let r = Rect::new(0.0, 0.0, 512.0, 512.0);
    assert_eq!(tiles_in(r, 0, 100).unwrap(), vec![TileKey { level: 0, x: 0, y: 0 }]);
    assert_eq!(tiles_in(Rect::new(-1.0, 0.0, 513.0, 10.0), 0, 100).unwrap().len(), 3);
    assert!(tiles_in(Rect::new(0.0, 0.0, 1e6, 1e6), 0, 100).is_none(), "too many tiles: none");
    assert_eq!(level_at_most(3.0), Some(1));
    assert_eq!(level_at_least(3.0), Some(2));
    assert_eq!(level_at_most(1.0), Some(0));
    assert_eq!(level_at_most(0.0), None);
    assert_eq!(level_at_most(f64::NAN), None);
    assert_eq!(level_at_least(f64::INFINITY), None);
}

#[test]
fn the_plan_goes_coarser_and_wider_and_always_shows_the_whole_page() {
    let (doc, page, _) = scene();
    let content = content_bounds(&doc, page).unwrap();
    // 200% on a 2x screen (4 device pixels per unit), looking at the red rectangle.
    let visible = Rect::new(400.0, 380.0, 760.0, 605.0);
    let plan = wanted(visible, 4.0, Some(content));
    assert!(!plan.is_empty());
    assert_eq!(plan[0].level, 1, "first the level below the view's own (4 → level 2 → 1)");
    let levels: std::collections::BTreeSet<i32> = plan.iter().map(|k| k.level).collect();
    assert!(levels.len() >= 3);
    for l in &levels {
        let n = plan.iter().filter(|k| k.level == *l).count();
        assert!(n <= MAX_PER_LEVEL, "level {l}: {n} tiles");
    }
    // Some level's tiles cover all of the content.
    let covered = levels.iter().any(|l| {
        let tiles: Vec<_> = plan.iter().filter(|k| k.level == *l).collect();
        tiles_in(content, *l, 1000).unwrap().iter().all(|k| tiles.contains(&k))
    });
    assert!(covered, "an overview level");
    // No duplicates.
    let unique: std::collections::BTreeSet<_> = plan.iter().collect();
    assert_eq!(unique.len(), plan.len());
    assert!(wanted(visible, 4.0, None).is_empty(), "an empty page wants nothing");
    assert!(wanted(visible, f64::NAN, Some(content)).is_empty());
    assert!(wanted(Rect::new(0.0, 0.0, 0.0, 0.0), 1.0, Some(content)).is_empty());
}

#[test]
fn a_huge_page_far_zoomed_out_stays_bounded() {
    let content = Rect::new(-5e6, -5e6, 5e6, 5e6);
    for scale in [1e-4, 0.01, 0.3, 2.0, 64.0, 1e4] {
        let w = 2880.0 / scale;
        let plan = wanted(Rect::new(-w / 2.0, -w / 3.0, w / 2.0, w / 3.0), scale, Some(content));
        assert!(plan.len() <= MAX_PER_LEVEL * 8, "scale {scale}: {} tiles", plan.len());
    }
}

fn area(pieces: &[crate::tiles::Piece]) -> f64 {
    pieces.iter().map(|p| p.rect.area()).sum()
}

fn disjoint(pieces: &[crate::tiles::Piece]) -> bool {
    pieces.iter().enumerate().all(|(i, a)| pieces.iter().skip(i + 1).all(|b| a.rect.intersect(b.rect).area() <= 1e-9))
}

#[test]
fn at_rest_the_exact_image_draws_everything() {
    let visible = Rect::new(0.0, 0.0, 720.0, 450.0);
    let exact = Exact { rect: Rect::new(-50.0, -50.0, 770.0, 500.0), scale: 2.0 };
    let pieces = compose(visible, 2.0, Some(exact), |_| Some(Have { stale: false }));
    assert_eq!(pieces.len(), 1);
    assert_eq!(pieces[0].pick, Pick::Exact);
    assert_eq!(pieces[0].rect, visible);
}

#[test]
fn zooming_out_fills_around_the_old_image_with_tiles() {
    // Was at 2 device px/unit over (0,0)-(720,450); now at 0.5 the view is four times as wide.
    let exact = Exact { rect: Rect::new(0.0, 0.0, 720.0, 450.0), scale: 2.0 };
    let visible = Rect::new(-1080.0, -675.0, 1800.0, 1125.0);
    // Only level -2 tiles are cached (0.25 px/unit: magnified twice).
    let have = |k: TileKey| (k.level == -2).then_some(Have { stale: false });
    let pieces = compose(visible, 0.5, Some(exact), have);
    assert!(disjoint(&pieces), "every part of the screen is drawn once");
    assert!((area(&pieces) - visible.area()).abs() < 1e-6 * visible.area(), "the whole view is covered");
    // The old image is sharper than the tiles where it is.
    let exact_area: f64 = pieces.iter().filter(|p| p.pick == Pick::Exact).map(|p| p.rect.area()).sum();
    assert!((exact_area - exact.rect.area()).abs() < 1e-6);
    assert!(pieces.iter().any(|p| matches!(p.pick, Pick::Tile(k) if k.level == -2)));
}

#[test]
fn a_sharp_tile_beats_a_magnified_exact_image_and_stale_tiles_lose_to_it() {
    // Zoomed in from 1 to 2 px/unit: the old image is magnified, a level-1 tile is sharp.
    let exact = Exact { rect: Rect::new(0.0, 0.0, 1000.0, 1000.0), scale: 1.0 };
    let visible = Rect::new(100.0, 100.0, 400.0, 300.0);
    let sharp = compose(visible, 2.0, Some(exact), |k| (k.level == 1).then_some(Have { stale: false }));
    assert!(sharp.iter().all(|p| matches!(p.pick, Pick::Tile(k) if k.level == 1)));
    assert!(disjoint(&sharp));
    let stale = compose(visible, 2.0, Some(exact), |k| (k.level == 1).then_some(Have { stale: true }));
    assert!(stale.iter().all(|p| p.pick == Pick::Exact), "a magnified fresh image beats a stale tile");
    // Nothing at all: nothing drawn, the page background shows.
    assert!(compose(visible, 2.0, None, |_| None).is_empty());
    assert!(compose(visible, f64::NAN, None, |_| Some(Have { stale: false })).is_empty());
}

#[test]
fn the_nearest_coarser_tile_stands_in_for_a_missing_one() {
    let visible = Rect::new(0.0, 0.0, 300.0, 200.0);
    let pieces = compose(visible, 1.0, None, |k| (k.level == -3).then_some(Have { stale: false }));
    assert!(!pieces.is_empty());
    assert!(pieces.iter().all(|p| matches!(p.pick, Pick::Tile(k) if k.level == -3)));
    assert!((area(&pieces) - visible.area()).abs() < 1e-6);
}

#[test]
fn an_edit_touches_the_tiles_where_the_layer_was_and_is() {
    let (doc, page, a) = scene();
    let mut d = (*doc).clone();
    d.get_mut(a).unwrap().transform = figcraft_geom::Affine::translate((3000.0, 0.0));
    let new = Arc::new(d);
    let areas = changed(&doc, &new, page).unwrap();
    assert!(!areas.is_empty());
    let at = |x: f64, y: f64| TileKey { level: 0, x: (x / 512.0).floor() as i64, y: (y / 512.0).floor() as i64 };
    assert!(touched(&at(450.0, 400.0), &areas, 0.0), "where it was");
    assert!(touched(&at(3450.0, 400.0), &areas, 0.0), "where it is");
    assert!(!touched(&at(-2000.0, 3000.0), &areas, 0.0), "far from both");
    // Coarse tiles over both places are touched too.
    assert!(touched(&TileKey { level: -4, x: 0, y: 0 }, &areas, 0.0));
    assert_eq!(changed(&doc, &doc, page), Some(vec![]));
}

#[test]
fn a_stop_leaves_the_tiles_not_started_for_later() {
    let (doc, page, _) = scene();
    let keys: Vec<TileKey> = (0..6).map(|x| TileKey { level: 0, x, y: 0 }).collect();
    assert!(CpuTilePainter.paint(&doc, page, &keys, &Options::default(), &|| true).is_empty(), "stopped before any");
    // Stop after the first tile starts: the rest are left out, none is reported as failed.
    let started = std::sync::atomic::AtomicUsize::new(0);
    let stop = || started.fetch_add(1, std::sync::atomic::Ordering::SeqCst) >= 1;
    let painted = CpuTilePainter.paint(&doc, page, &keys, &Options::default(), &stop);
    assert!(!painted.is_empty() && painted.len() < keys.len(), "{} of {}", painted.len(), keys.len());
    assert!(painted.iter().all(|(_, p)| p.is_some()));
}

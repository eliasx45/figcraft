use std::collections::HashMap;

use figcraft_geom::Rect as PRect;
use serde_json::json;

use super::*;

/// A 16:9 card's Timeline is as wide as the card and 0.6× as tall; a 9:16 card's stays wide and
/// horizontal (never narrower than 1.6× its height, at least the card's width).
#[test]
fn timelines_stay_wide_under_any_card() {
    let land = geom(PRect::new(0.0, 0.0, 480.0, 270.0), None, true, 0);
    assert_eq!((land.strip.width(), land.strip.height()), (480.0, 288.0));
    assert_eq!(land.strip.x0, 0.0);
    assert!(land.strip.y0 > 270.0, "under the card, with a gap");
    let fx = land.fx.expect("Effect Controls beside it");
    assert!(fx.x0 > land.strip.x1 && fx.y0 == land.strip.y0);

    let tall = geom(PRect::new(100.0, 0.0, 370.0, 480.0), None, true, 0);
    assert!(tall.strip.width() >= tall.strip.height() * 1.6 - 1e-9, "horizontal under a 9:16 card");
    assert!(tall.strip.width() >= 270.0);
    assert_eq!(tall.strip.x0, 100.0, "anchored at the card's left edge");
    // Dragged narrower than that, it stops at the least width; wider, it is as dragged.
    let least = least_width(270.0, 480.0);
    assert_eq!(open_size(270.0, 480.0, Some(10.0)).0, least);
    assert_eq!(open_size(270.0, 480.0, Some(900.0)).0, 900.0);
    // Junk widths are ignored.
    assert_eq!(open_size(480.0, 270.0, Some(f64::NAN)), open_size(480.0, 270.0, None));
}

/// Everything scales with the card (a card twice as big has a Timeline twice as big), and a
/// closed strip is a header and a bar per layer.
#[test]
fn strips_scale_with_their_card() {
    let a = geom(PRect::new(0.0, 0.0, 480.0, 270.0), None, true, 0);
    let b = geom(PRect::new(0.0, 0.0, 960.0, 540.0), None, true, 0);
    assert_eq!(b.k, 2.0 * a.k);
    assert_eq!(b.strip.height(), 2.0 * a.strip.height());
    let closed3 = geom(PRect::new(0.0, 0.0, 480.0, 270.0), None, false, 3);
    let closed6 = geom(PRect::new(0.0, 0.0, 480.0, 270.0), None, false, 6);
    assert!(closed3.fx.is_none());
    assert!(closed6.strip.height() > closed3.strip.height() && closed3.strip.height() < a.strip.height());
    let r = to_panel(b.strip, b.k);
    assert_eq!((r.width(), r.height()), (a.strip.width() as f32, a.strip.height() as f32), "the same panel at any card size");
}

fn place(s: &mut figcraft_engine::Session, x: f64, y: f64) -> NodeId {
    let v = s.execute("effectcraft.place", &json!({"comp": "C", "start": 0, "end": 2, "x": x, "y": y, "cardWidth": 480})).unwrap();
    figcraft_engine::params::id_of(&v["id"]).unwrap()
}

/// Widening a Timeline pushes what it would cover to its right in its band (cascading, with a
/// gap), never what is above or below that band or to its left, and never pulls back; the width
/// is kept on the card.
#[test]
fn widening_pushes_the_neighbours_right() {
    let mut s = figcraft_engine::Session::new();
    let card = place(&mut s, 0.0, 0.0);
    let b = s.doc().unwrap().world_bounds(card).unwrap();
    let right = place(&mut s, b.x1 + 400.0, b.y0);
    let far = s
        .execute("shape.rectangle", &json!({"x": b.x1 + 400.0 + 480.0 + 20.0, "y": b.y0, "width": 50, "height": 50}))
        .ok()
        .and_then(|v| figcraft_engine::params::id_of(&v["id"]));
    let below = s
        .execute("shape.rectangle", &json!({"x": b.x1 + 100.0, "y": b.y1 + 5000.0, "width": 50, "height": 50}))
        .ok()
        .and_then(|v| figcraft_engine::params::id_of(&v["id"]));
    let page = s.ds().unwrap().page;
    let snap = s.doc().unwrap().clone();
    let x_of = |d: &Document, id: NodeId| d.world_bounds(id).unwrap().x0;
    let mut pushed = HashMap::new();
    let mut d = snap.clone();
    widen(&mut d, page, card, 1600.0, &[(right, 3)], &mut pushed).unwrap();
    assert_eq!(asked_width(&d, card), Some(1600.0));
    let g = geom_of(&d, card, true, 0).unwrap();
    let moved = x_of(&d, right);
    assert!(moved > g.whole().x1, "the card on the right moved past the Timeline and its Effect Controls");
    assert!(x_of(&d, card) == x_of(&snap, card), "the card itself stays");
    if let Some(far) = far {
        assert!(x_of(&d, far) > d.world_bounds(right).unwrap().x1, "pushed on by the card it was behind");
    }
    if let Some(below) = below {
        assert_eq!(x_of(&d, below), x_of(&snap, below), "out of the band: stays");
    }
    // Narrower again (from the same snapshot, as a gesture's preview is): nothing comes back.
    let mut d2 = snap.clone();
    widen(&mut d2, page, card, 500.0, &[(right, 3)], &mut pushed).unwrap();
    assert_eq!(x_of(&d2, right), moved, "shrinking doesn't pull them back");
}

/// Junk never panics: a card without bounds, a width that isn't a number.
#[test]
fn junk_widths_never_panic() {
    let mut s = figcraft_engine::Session::new();
    let card = place(&mut s, 0.0, 0.0);
    let page = s.ds().unwrap().page;
    let mut d = s.doc().unwrap().clone();
    let mut pushed = HashMap::new();
    for w in [f64::NAN, f64::INFINITY, -5.0, 0.0, 1e300] {
        let _ = widen(&mut d, page, card, w, &[], &mut pushed);
    }
    let _ = widen(&mut d, page, page, 100.0, &[(page, 2)], &mut pushed);
    assert!(geom(PRect::new(0.0, 0.0, 0.0, 0.0), Some(f64::NAN), true, 0).strip.width().is_finite());
}

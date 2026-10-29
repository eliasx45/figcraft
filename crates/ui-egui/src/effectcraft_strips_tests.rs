use figcraft_geom::Rect as PRect;
use serde_json::json;

use super::*;

/// A 16:9 card's Timeline is as wide as the card and 0.6× as tall; a 9:16 card's stays wide and
/// horizontal (never narrower than 1.6× its height, at least the card's width).
#[test]
fn timelines_stay_wide_under_any_card() {
    let land = geom(PRect::new(0.0, 0.0, 480.0, 270.0), Size::default(), true, 0);
    // Effect Controls left of the picture, as tall as it and top-aligned (AE's Standard workspace).
    let fx = land.fx.expect("Effect Controls beside the picture");
    assert!(fx.x1 < 0.0 && fx.y0 == 0.0 && fx.y1 == 270.0, "{fx:?}");
    // The viewer bar under the picture, as wide as it.
    let foot = land.footer.expect("the viewer bar");
    assert!(foot.y0 > 270.0 && foot.x0 == 0.0 && foot.x1 == 480.0);
    // The Timeline under both, from Effect Controls' left edge to at least the picture's right.
    assert_eq!(land.strip.height(), 288.0);
    assert_eq!(land.strip.x0, fx.x0);
    assert!(land.strip.x1 >= 480.0 && land.strip.y0 > foot.y1, "under the bar, with a gap");
    assert_eq!(land.whole(), land.strip.union(fx).union(foot));

    let tall = geom(PRect::new(100.0, 0.0, 370.0, 480.0), Size::default(), true, 0);
    assert!(tall.strip.width() >= tall.strip.height() * 1.6 - 1e-9, "horizontal under a 9:16 card");
    assert!(tall.strip.width() >= 270.0);
    assert_eq!(tall.strip.x0, tall.fx.unwrap().x0, "anchored at Effect Controls' left edge");
    // Dragged narrower than that, it stops at the least width; wider, it is as dragged.
    let least = least_width(270.0, 480.0);
    assert_eq!(open_size(270.0, 480.0, Some(10.0), None).0, least);
    assert_eq!(open_size(270.0, 480.0, Some(900.0), None).0, 900.0);
    // Junk widths are ignored.
    assert_eq!(open_size(480.0, 270.0, Some(f64::NAN), Some(f64::NAN)), open_size(480.0, 270.0, None, None));
}

/// Everything scales with the card (a card twice as big has a Timeline twice as big), and a
/// closed strip is a header and a bar per layer.
#[test]
fn strips_scale_with_their_card() {
    let a = geom(PRect::new(0.0, 0.0, 480.0, 270.0), Size::default(), true, 0);
    let b = geom(PRect::new(0.0, 0.0, 960.0, 540.0), Size::default(), true, 0);
    assert_eq!(b.k, 2.0 * a.k);
    assert_eq!(b.strip.height(), 2.0 * a.strip.height());
    let closed3 = geom(PRect::new(0.0, 0.0, 480.0, 270.0), Size::default(), false, 3);
    let closed6 = geom(PRect::new(0.0, 0.0, 480.0, 270.0), Size::default(), false, 6);
    assert!(closed3.fx.is_none());
    assert!(closed6.strip.height() > closed3.strip.height() && closed3.strip.height() < a.strip.height());
    let r = to_panel(b.strip, b.k);
    assert_eq!((r.width(), r.height()), (a.strip.width() as f32, a.strip.height() as f32), "the same panel at any card size");
}

fn place(s: &mut figcraft_engine::Session, x: f64, y: f64) -> NodeId {
    let v = s.execute("effectcraft.place", &json!({"comp": "C", "start": 0, "end": 2, "x": x, "y": y, "cardWidth": 480})).unwrap();
    figcraft_engine::params::id_of(&v["id"]).unwrap()
}

/// Widening a Timeline slides the card's row right as one piece (with a gap), never what is
/// below that row out of reach; narrower again, the row follows back, and putting back returns
/// it where it was; the width is kept on the card.
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
    let mut d = snap.clone();
    resize(&mut d, page, card, Size { width: Some(1600.0), ..Size::default() }, &[(right, 3)]).unwrap();
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
    // Narrower again (on the widened page, as a later drag is): the row follows back.
    let mut d2 = d.clone();
    resize(&mut d2, page, card, Size { width: Some(500.0), ..Size::default() }, &[(right, 3)]).unwrap();
    assert!(x_of(&d2, right) < moved, "the room follows the size");
    put_back(&mut d2, card);
    assert_eq!(x_of(&d2, right), x_of(&snap, right), "put back where it was");
}

/// Junk never panics: a card without bounds, a width that isn't a number.
#[test]
fn junk_widths_never_panic() {
    let mut s = figcraft_engine::Session::new();
    let card = place(&mut s, 0.0, 0.0);
    let page = s.ds().unwrap().page;
    let mut d = s.doc().unwrap().clone();
    for w in [f64::NAN, f64::INFINITY, -5.0, 0.0, 1e300] {
        let _ = resize(&mut d, page, card, Size { width: Some(w), height: Some(w), fx: Some(w) }, &[]);
    }
    let _ = resize(&mut d, page, page, Size { width: Some(100.0), ..Size::default() }, &[(page, 2)]);
    assert!(
        geom(PRect::new(0.0, 0.0, 0.0, 0.0), Size { width: Some(f64::NAN), height: Some(f64::NAN), fx: Some(f64::NAN) }, true, 0)
            .strip
            .width()
            .is_finite()
    );
}

/// Taller slides everything below the card's row down as one piece (rows keep their shape), Effect
/// Controls keep the picture's height, the Timeline's sizes are kept on the card and Effect
/// Controls' width on the board (its page).
#[test]
fn taller_pushes_down_and_effect_controls_follow() {
    let mut s = figcraft_engine::Session::new();
    let card = place(&mut s, 0.0, 0.0);
    let b = s.doc().unwrap().world_bounds(card).unwrap();
    let page = s.ds().unwrap().page;
    let g0 = geom_of(s.doc().unwrap(), card, true, 0).unwrap();
    let below = s
        .execute("shape.rectangle", &json!({"x": b.x0 + 10.0, "y": g0.strip.y1 + 40.0, "width": 50, "height": 50}))
        .ok()
        .and_then(|v| figcraft_engine::params::id_of(&v["id"]));
    let beside = s
        .execute("shape.rectangle", &json!({"x": g0.whole().x1 + 500.0, "y": g0.strip.y1 + 40.0, "width": 50, "height": 50}))
        .ok()
        .and_then(|v| figcraft_engine::params::id_of(&v["id"]));
    // Another card of the board, out of the way (above and left of everything).
    let other = place(&mut s, -5000.0, -5000.0);
    let snap = s.doc().unwrap().clone();
    let y_of = |d: &Document, id: NodeId| d.world_bounds(id).unwrap().y0;
    let mut d = snap.clone();
    let tall = g0.strip.height() + 600.0;
    resize(&mut d, page, card, Size { height: Some(tall), ..Size::default() }, &[]).unwrap();
    assert_eq!(asked_height(&d, card), Some(tall));
    let g = geom_of(&d, card, true, 0).unwrap();
    assert_eq!(g.fx.unwrap().height(), b.height(), "Effect Controls keep the picture's height");
    if let Some(below) = below {
        assert!(y_of(&d, below) > g.strip.y1, "pushed down under the taller Timeline");
    }
    if let (Some(below), Some(beside)) = (below, beside) {
        assert_eq!(y_of(&d, beside) - y_of(&snap, beside), y_of(&d, below) - y_of(&snap, below), "the row below moves as one");
    }
    // Never shorter than a header and a few rows.
    let mut d2 = snap.clone();
    resize(&mut d2, page, card, Size { height: Some(1.0), ..Size::default() }, &[]).unwrap();
    assert!(geom_of(&d2, card, true, 0).unwrap().strip.height() >= height_range(b.width(), b.height()).0);
    // Effect Controls: wide enough by default, wider as dragged, kept on the board.
    let (lo, def, _) = fx_range(b.width(), b.height());
    assert!(g0.fx.unwrap().width() == def && def > lo);
    let mut d3 = snap.clone();
    resize(&mut d3, page, card, Size { fx: Some(def * 2.0), ..Size::default() }, &[]).unwrap();
    assert_eq!(geom_of(&d3, card, true, 0).unwrap().fx.unwrap().width(), def * 2.0);
    let board = d3.page_of(card).unwrap();
    assert_eq!(d3.node(board).unwrap().plugin_data.get(FX_WIDTH_KEY).map(String::as_str), Some(format!("{:.1}", def * 2.0).as_str()));
    assert!(!d3.node(card).unwrap().plugin_data.contains_key(FX_WIDTH_KEY), "not the card's own");
    // Another card of the board opens with that width.
    assert_eq!(geom_of(&d3, other, true, 0).unwrap().fx.unwrap().width(), def * 2.0);
}

/// Effect Controls left of the picture make room on the left: what is left of the card in its
/// row (a neighbour card, the row's header) slides left as one piece, and back when put back.
#[test]
fn effect_controls_push_the_left_neighbours_left() {
    let mut s = figcraft_engine::Session::new();
    let card = place(&mut s, 2000.0, 0.0);
    let b = s.doc().unwrap().world_bounds(card).unwrap();
    let left = place(&mut s, b.x0 - 600.0, b.y0);
    let header = s
        .execute("shape.rectangle", &json!({"x": b.x0 - 900.0, "y": b.y0, "width": 100, "height": 50}))
        .ok()
        .and_then(|v| figcraft_engine::params::id_of(&v["id"]));
    let page = s.ds().unwrap().page;
    let snap = s.doc().unwrap().clone();
    let x_of = |d: &Document, id: NodeId| d.world_bounds(id).unwrap().x0;
    let mut d = snap.clone();
    make_room(&mut d, page, card, &[(left, 3)]).unwrap();
    let g = geom_of(&d, card, true, 0).unwrap();
    let fx = g.fx.unwrap();
    let moved = d.world_bounds(left).unwrap();
    assert!(moved.x1 < fx.x0, "the left card is clear of Effect Controls");
    assert_eq!(x_of(&d, card), x_of(&snap, card), "the card itself stays");
    if let Some(header) = header {
        assert_eq!(x_of(&snap, left) - x_of(&d, left), x_of(&snap, header) - x_of(&d, header), "the row's left part moves as one");
    }
    put_back(&mut d, card);
    assert_eq!(x_of(&d, left), x_of(&snap, left), "put back where it was");
}

/// The resize zones are 8 screen points either side of each edge at any zoom (just outside the
/// edge works as well as just inside), the Timeline's corner wins over edges, and edges over the
/// content under them (a point the Timeline's scroll bar would take is the right edge's).
/// Effect Controls (left of the picture, over the Timeline) size at their left edge.
#[test]
fn edges_are_easy_to_grab_at_any_zoom() {
    use egui::{Rect, pos2, vec2};
    for zoom in [0.1_f32, 0.25, 1.0, 3.0] {
        // A 360×270 Effect Controls, and under it (and the picture right of it) an 848×288
        // Timeline from the same left edge, on screen.
        let fx = Rect::from_min_size(pos2(100.0, 100.0), vec2(360.0, 270.0) * zoom);
        let strip = Rect::from_min_size(pos2(fx.min.x, fx.max.y + 46.0 * zoom), vec2(848.0, 288.0) * zoom);
        let mid_y = strip.center().y;
        // The Timeline's right edge: just outside, on it, just inside (where its scroll bar is).
        for dx in [-7.5, -2.0, 0.0, 2.0] {
            assert_eq!(edge_at(strip, Some(fx), pos2(strip.max.x + dx, mid_y)), Some(Edge::Right), "zoom {zoom}, dx {dx}");
        }
        // Effect Controls' left edge, both sides.
        for dx in [-7.5, 0.0, 7.5] {
            assert_eq!(edge_at(strip, Some(fx), pos2(fx.min.x + dx, fx.center().y)), Some(Edge::FxLeft), "zoom {zoom}, dx {dx}");
        }
        // Effect Controls' right edge (beside the picture) sizes nothing.
        assert_ne!(edge_at(strip, Some(fx), pos2(fx.max.x, fx.center().y)), Some(Edge::FxLeft));
        // The Timeline's bottom edge, both sides.
        for dy in [-7.5, 0.0, 7.5] {
            assert_eq!(edge_at(strip, Some(fx), pos2(strip.center().x, strip.max.y + dy)), Some(Edge::Bottom), "zoom {zoom}, dy {dy}");
        }
        // Past the band: nothing (the content's, or the canvas's).
        assert_eq!(edge_at(strip, Some(fx), pos2(strip.max.x + 9.0 + 8.0 * zoom, mid_y)).filter(|e| *e == Edge::Right), None);
        assert_eq!(edge_at(strip, Some(fx), pos2(strip.center().x, strip.max.y + 9.0)), None, "below the band");
        if strip.width() > 60.0 {
            assert_eq!(edge_at(strip, Some(fx), pos2(strip.min.x + 30.0, strip.min.y + 20.0).min(strip.center())), None, "the content's");
        }
        // The corner wins over the edges that meet there, 14 points either way.
        assert_eq!(edge_at(strip, Some(fx), strip.max + vec2(-6.0, -6.0)), Some(Edge::Corner));
        assert_eq!(edge_at(strip, Some(fx), strip.max + vec2(12.0, 12.0)), Some(Edge::Corner), "just outside the corner");
        assert_eq!(edge_at(strip, None, strip.max + vec2(5.0, 5.0)), Some(Edge::Corner), "no Effect Controls");
    }
    // The zones the canvas leaves to the edges reach past the panels.
    let strip = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 50.0));
    let z = zones(strip, None);
    assert!(z.iter().any(|r| r.contains(pos2(107.0, 25.0))) && z.iter().any(|r| r.contains(pos2(112.0, 62.0))));
}

/// Every edge sizes what it should, from where it was grabbed.
#[test]
fn each_edge_sizes_its_own_panel() {
    let g = geom(PRect::new(0.0, 0.0, 480.0, 270.0), Size::default(), true, 0);
    let fx = g.fx.unwrap();
    assert_eq!(Edge::Right.size(&g, g.strip.x0 + 700.0, 0.0), Size { width: Some(700.0), ..Size::default() });
    assert_eq!(Edge::Bottom.size(&g, 0.0, g.strip.y0 + 500.0), Size { height: Some(500.0), ..Size::default() });
    assert_eq!(Edge::FxLeft.size(&g, fx.x1 - 400.0, 0.0), Size { fx: Some(400.0), ..Size::default() });
    let c = Edge::Corner.size(&g, g.strip.x0 + 900.0, g.strip.y0 + 300.0);
    assert_eq!((c.width, c.height, c.fx), (Some(900.0), Some(300.0), None));
}

//! Live cards (EffectCraft, video) paint their frame over the canvas image: the layers above a card
//! in z-order (board stamps, stickies, marker strokes) must still show over it.
#![allow(clippy::unwrap_used)]

use figcraft_color::{Color, Paint};
use figcraft_doc::{Document, EcComp, NodeKind};
use figcraft_geom::Rect;

use crate::card_cover::{above, cover_scale, render};

/// A page with a layer under the card, the card (an EffectCraft card, 400×225 at 100,100), a stamp
/// over its corner, and a sticky elsewhere on the page.
fn board() -> (Document, [figcraft_doc::NodeId; 4]) {
    let mut d = Document::new("board");
    let p = d.first_page().unwrap();
    let under = d.create(p, NodeKind::Rectangle, "Under", 80.0, 80.0, 500.0, 300.0).unwrap();
    let card = d.create(p, NodeKind::Rectangle, "Card", 100.0, 100.0, 400.0, 225.0).unwrap();
    d.get_mut(card).unwrap().effectcraft = Some(Box::new(EcComp::new("MAIN", 0.0, 5.0, 30.0)));
    let stamp = d.create(p, NodeKind::Ellipse { start: 0.0, sweep: std::f64::consts::TAU, inner: 0.0 }, "Stamp", 460.0, 90.0, 60.0, 60.0).unwrap();
    d.get_mut(stamp).unwrap().fills = vec![Paint::solid(Color::from_hex("2FB36F").unwrap())];
    let sticky = d.create(p, NodeKind::Rectangle, "Sticky", 900.0, 100.0, 200.0, 200.0).unwrap();
    (d, [under, card, stamp, sticky])
}

#[test]
fn a_stamp_on_a_live_card_stays_on_top_of_its_frame() {
    let (d, [_under, card, stamp, _sticky]) = board();
    let (ids, area) = above(&d, card);
    assert_eq!(ids, vec![stamp], "only the layer above the card that overlaps it");
    // The stamp's part on the card: x 460..500, y 100..150.
    let area = area.unwrap();
    assert_eq!((area.x0, area.y0, area.x1, area.y1), (460.0, 100.0, 500.0, 150.0));

    // The cover drawn over the frame holds the stamp, and is clear where it isn't.
    let (pix, spans, scale) = render(&d, card, 1.0).unwrap();
    assert_eq!(scale, 1.0);
    assert_eq!((spans.x0, spans.y0), (460.0, 100.0));
    let at = |x: f64, y: f64| pix.pixel(((x - spans.x0) * scale) as u32, ((y - spans.y0) * scale) as u32).unwrap();
    let inside = at(490.0, 120.0);
    assert_eq!(inside.alpha(), 255, "the stamp shows over the frame");
    assert!(inside.green() > inside.red() && inside.green() > inside.blue(), "it is the stamp's green: {inside:?}");
    assert_eq!(at(461.0, 101.0).alpha(), 0, "outside the round stamp the live frame shows (no box)");
}

#[test]
fn a_card_with_nothing_over_it_draws_no_cover() {
    let (mut d, [_under, card, stamp, _sticky]) = board();
    d.get_mut(stamp).unwrap().visible = false;
    assert!(above(&d, card).0.is_empty());
    assert!(render(&d, card, 2.0).is_none());
}

#[test]
fn covers_render_per_zoom_doubling_within_a_pixel_budget() {
    let small = Rect::new(0.0, 0.0, 100.0, 100.0);
    assert_eq!(cover_scale(small, 1.0), 1.0);
    assert_eq!(cover_scale(small, 1.3), 2.0);
    assert_eq!(cover_scale(small, 3.0), 4.0);
    assert_eq!(cover_scale(small, f64::NAN), 1.0);
    let huge = Rect::new(0.0, 0.0, 4000.0, 4000.0);
    let s = cover_scale(huge, 8.0);
    assert!(4000.0 * 4000.0 * s * s <= 4_000_001.0, "{s}");
}

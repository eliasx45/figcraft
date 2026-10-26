//! Layer handles on EffectCraft cards: comp ↔ card mapping, what a press grabs (handles first,
//! backgrounds by their handles only), and a drag being one EffectCraft undo step.
#![allow(clippy::unwrap_used)]

use egui::{CursorIcon, Rect, pos2, vec2};
use figcraft_doc::EcComp;
use figcraft_ec_inproc::effectcraft_engine::Session;
use figcraft_ec_inproc::effectcraft_engine::project::LayerId;
use serde_json::json;

use super::{CardMap, Hit, covers_comp, handle_cursor, hit, shows_active_comp, vh, with_ctx};

/// A 640×360 comp "Main" with a comp-sized background and a 100×100 layer A in the middle
/// (box 270,130 – 370,230), shown on a card at (100, 100) at half size.
fn scene() -> (Session, LayerId, LayerId, CardMap) {
    let mut s = Session::default();
    s.execute("comp.new", json!({"name": "Main", "width": 640, "height": 360, "frameRate": 30, "duration": 10})).unwrap();
    let id = |v: serde_json::Value| LayerId(v["layer"].as_u64().unwrap());
    let bg = id(s.execute("layer.newSolid", json!({"name": "BG", "width": 640, "height": 360})).unwrap());
    let a = id(s.execute("layer.newSolid", json!({"name": "A", "width": 100, "height": 100})).unwrap());
    s.execute("edit.deselectAll", json!({})).unwrap();
    let map = CardMap::new(Rect::from_min_size(pos2(100.0, 100.0), vec2(320.0, 180.0)), 640, 360).unwrap();
    (s, bg, a, map)
}

#[test]
fn comp_pixels_map_onto_the_card_and_back() {
    let (_, _, _, map) = scene();
    assert_eq!(map.to_screen([320.0, 180.0]), pos2(260.0, 190.0));
    assert_eq!(map.to_comp(pos2(260.0, 190.0)), [320.0, 180.0]);
    assert!(CardMap::new(Rect::NOTHING, 640, 360).is_none());
    assert!(CardMap::new(Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 10.0)), 0, 360).is_none());
}

#[test]
fn a_press_grabs_the_layer_under_it_but_never_a_background_body() {
    let (s, _, a, map) = scene();
    assert_eq!(hit(&s, &map, map.to_screen([320.0, 180.0])), Some(Hit::Layer(a)));
    // Only the background there: the press is FigCraft's (it moves the card).
    assert_eq!(hit(&s, &map, map.to_screen([50.0, 50.0])), None);
}

#[test]
fn a_selected_layers_handles_come_first() {
    let (mut s, bg, a, map) = scene();
    s.execute("layer.select", json!({"layers": [a.0]})).unwrap();
    assert_eq!(hit(&s, &map, map.to_screen([370.0, 230.0]) + vec2(2.0, -2.0)), Some(Hit::Handle(a, 2)));
    assert_eq!(hit(&s, &map, map.to_screen([370.0, 180.0])), Some(Hit::Handle(a, 5)));
    // A selected background: its handles grab, its body doesn't.
    s.execute("layer.select", json!({"layers": [bg.0]})).unwrap();
    assert_eq!(hit(&s, &map, map.to_screen([0.0, 0.0])), Some(Hit::Handle(bg, 0)));
    assert_eq!(hit(&s, &map, map.to_screen([50.0, 50.0])), None);
}

#[test]
fn backgrounds_are_layers_covering_the_whole_comp() {
    let (s, bg, a, _) = scene();
    with_ctx(&s, |ctx| {
        let b = |id: LayerId| vh::layer_box(ctx, ctx.comp.layer(id).unwrap()).unwrap();
        assert!(covers_comp(&b(bg), [640.0, 360.0]));
        assert!(!covers_comp(&b(a), [640.0, 360.0]));
    })
    .unwrap();
}

#[test]
fn handles_apply_only_to_the_comp_the_card_shows() {
    let (s, _, _, _) = scene();
    let by_name = EcComp::new("Main", 0.0, 2.0, 30.0);
    assert!(shows_active_comp(&s, &by_name));
    let mut other = EcComp::new("Other", 0.0, 2.0, 30.0);
    assert!(!shows_active_comp(&s, &other));
    other.comp_id = s.active_comp_id().unwrap().0;
    assert!(shows_active_comp(&s, &other));
    other.comp_id += 1;
    assert!(!shows_active_comp(&s, &other));
}

#[test]
fn handle_cursors_point_the_way_they_scale() {
    assert_eq!(handle_cursor(0), CursorIcon::ResizeNwSe);
    assert_eq!(handle_cursor(1), CursorIcon::ResizeNeSw);
    assert_eq!(handle_cursor(4), CursorIcon::ResizeVertical);
    assert_eq!(handle_cursor(7), CursorIcon::ResizeHorizontal);
}

/// Every frame of a drag writes with the drag's `merge`: one ⌘Z takes all of it back.
#[test]
fn a_drag_is_one_effectcraft_undo_step() {
    let (mut s, _, a, map) = scene();
    let position = |s: &Session| with_ctx(s, |ctx| vh::MoveDrag::begin(ctx, &[a], [0.0, 0.0]).unwrap().layers[0].1).unwrap();
    assert_eq!(position(&s), [320.0, 180.0, 0.0]);
    let drag = with_ctx(&s, |ctx| vh::MoveDrag::begin(ctx, &[a], map.to_comp(pos2(260.0, 190.0)))).flatten().unwrap();
    for x in [265.0, 270.0, 280.0] {
        for (id, v) in drag.values(map.to_comp(pos2(x, 190.0)), false) {
            s.execute("prop.set", json!({"layer": id.0, "path": "transform/position", "value": v, "merge": "figcraft-card-drag-1"})).unwrap();
        }
    }
    assert_eq!(position(&s), [360.0, 180.0, 0.0]);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(position(&s), [320.0, 180.0, 0.0]);
}

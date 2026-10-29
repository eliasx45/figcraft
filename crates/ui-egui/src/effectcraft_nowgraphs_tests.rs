//! The Now graphs shelf against FigCraft's own host (`EcHost`, the Timeline's session): the
//! listing under the playhead, a handle drag being one EffectCraft undo step, a saved ease
//! picked, where the shelf docks, the handle maths, colours and junk input.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use egui::{Rect, pos2, vec2};
use figcraft_doc::EcComp;
use serde_json::json;

use super::*;
use crate::effectcraft_panel::EcHost;

/// The sample project (a 64×36 comp "C", 2 s at 25 fps, a full-size red solid) plus a 16×16 Box
/// whose opacity fades 0 → 100 over 0.4 → 1.2 s; the playhead at 0.8 s.
fn host(name: &str) -> (EcHost, u64) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-nowgraphs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p: PathBuf = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    let mut c = EcComp::new("C", 0.0, 2.0, 25.0);
    c.comp_id = cid;
    c.project = p.to_str().unwrap().to_string();
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    let bx =
        h.invoke(&ctx, "layer.newSolid", json!({"name": "Box", "color": "#00ff00", "width": 16, "height": 16})).unwrap()["layer"].as_u64().unwrap();
    for (t, v) in [(0.4, 0.0), (1.2, 100.0)] {
        h.invoke(&ctx, "prop.addKey", json!({"layer": bx, "path": "transform/opacity", "time": t, "value": v})).unwrap();
    }
    h.set_time(0.8);
    (h, bx)
}

#[test]
fn the_listing_has_the_fade_under_the_playhead_with_his_curves() {
    let (mut h, bx) = host("list");
    let l = list(&mut h);
    assert_eq!(l.graphs.len(), 1, "{l:#?}");
    let g = &l.graphs[0];
    assert_eq!(g.layer, bx);
    assert_eq!(g.property, "Opacity");
    assert_eq!(g.speed.len(), 49);
    assert!(g.trail.first().is_some_and(|t| t == "C"));
    assert_eq!(l.objects.len(), 1);
    assert!(l.objects[0].quad.as_ref().is_some_and(|q| q.len() == 4));
    assert!(l.curves.iter().any(|c| c.id == "expo-in-out-elias"));
    assert_eq!(l.curves.last().map(|c| c.id.as_str()), Some("linear"));
    // A linear fade says so on its chip.
    assert_eq!(ease_label(&l, g), "Linear");
    // Far from the keys: nothing.
    h.set_time(1.96);
    let mut st = State::default();
    refresh(&mut st, &mut h);
    assert!(st.listing.graphs.iter().all(|g| g.end + 1.0 >= 1.96));
}

#[test]
fn a_handle_drag_is_one_undo_step_and_a_pick_is_another() {
    let (mut h, _) = host("drag");
    let ctx = egui::Context::default();
    let l = list(&mut h);
    let g = l.graphs[0].clone();
    let before = h.session().revision;
    for v in [20.0, 40.0, 66.0] {
        h.run_engine(&ctx, "nowGraphs.setInfluence", influence_call(&g, Some(v), None, "nowgraphs-drag-1")).unwrap();
    }
    let l2 = list(&mut h);
    let e = l2.graphs[0].ease.unwrap();
    assert!((e.out.influence - 66.0).abs() < 0.01, "{e:?}");
    assert_ne!(h.session().revision, before);
    h.run_engine(&ctx, "nowGraphs.applyEase", ease_call(&g, "expo-out-1")).unwrap();
    let l3 = list(&mut h);
    assert_eq!(l3.graphs[0].curve.as_deref(), Some("expo-out-1"));
    assert_eq!(ease_label(&l3, &l3.graphs[0]), "Expo Out 1 (Elias)");
    // Undo: the pick, then the whole drag.
    h.invoke(&ctx, "edit.undo", json!({})).unwrap();
    let e = list(&mut h).graphs[0].ease.unwrap();
    assert!((e.out.influence - 66.0).abs() < 0.01);
    h.invoke(&ctx, "edit.undo", json!({})).unwrap();
    assert_eq!(ease_label(&list(&mut h), &g), "Linear");
}

#[test]
fn the_shelf_docks_right_of_the_card_never_over_the_picture() {
    let card = Rect::from_min_size(pos2(100.0, 100.0), vec2(320.0, 200.0));
    let pic = Rect::from_min_size(pos2(100.0, 110.0), vec2(320.0, 180.0));
    let s = shelf_rect(card, pic);
    assert!(s.min.x >= card.max.x + GAP - 0.01);
    assert!(!s.intersects(pic));
    assert_eq!(s.min.y, pic.min.y);
    assert_eq!(s.width(), SHELF_W);
    // A tiny picture still gets a usable shelf.
    let small = Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 45.0));
    assert!(shelf_rect(small, small).height() >= 180.0);
}

#[test]
fn handles_map_to_influences_and_back() {
    let g = Rect::from_min_size(pos2(10.0, 0.0), vec2(200.0, 50.0));
    for v in [0.1, 25.0, 50.0, 81.0, 100.0] {
        assert!((influence_at(g, true, handle_x(g, true, v)) - v).abs() < 0.01);
        assert!((influence_at(g, false, handle_x(g, false, v)) - v).abs() < 0.01);
    }
    assert_eq!(influence_at(g, true, -500.0), 0.1);
    assert_eq!(influence_at(g, false, -500.0), 100.0);
    assert_eq!(influence_at(g, true, f32::NAN), 0.1);
}

#[test]
fn objects_get_their_own_colours_and_none_is_purple() {
    for (i, c) in PALETTE.iter().enumerate() {
        assert!(PALETTE.iter().skip(i + 1).all(|d| d != c), "colour {i} repeats");
        // Purple / magenta: red and blue both well above green.
        let (r, g, b) = (i32::from(c.r()), i32::from(c.g()), i32::from(c.b()));
        assert!(!(r > g + 40 && b > g + 40), "colour {i} is purple-ish: {c:?}");
    }
    assert_eq!(colour(PALETTE.len()), colour(0));
}

#[test]
fn outlines_markers_and_crops_never_panic_on_junk() {
    let pic = Rect::from_min_size(pos2(0.0, 0.0), vec2(160.0, 90.0));
    let q = [pos2(-50.0, 10.0), pos2(-20.0, 10.0), pos2(-20.0, 30.0), pos2(-50.0, 30.0)];
    let m = edge_marker(pic, &q).unwrap();
    assert!(pic.contains(m) && m.x < 10.0, "on the left edge: {m:?}");
    assert!(edge_marker(pic, &[]).is_none());
    assert!(edge_marker(Rect::NOTHING, &q).is_none());
    assert!(in_quad(pos2(-30.0, 20.0), &q));
    assert!(!in_quad(pos2(30.0, 20.0), &q));
    assert!(!in_quad(pos2(0.0, 0.0), &q[..2]));
    assert_eq!(crop_uv(&[[0.0, 0.0], [32.0, 0.0], [32.0, 18.0], [0.0, 18.0]], 64, 36), Some(Rect::from_min_max(pos2(0.0, 0.0), pos2(0.5, 0.5))));
    assert!(crop_uv(&[[0.0, 0.0]], 64, 36).is_none());
    let _ = crop_uv(&[[f64::NAN, 0.0], [1.0, 1.0]], 64, 36);
    assert!(crop_uv(&[[0.0, 0.0], [10.0, 10.0]], 0, 36).is_none());
    assert!(curve_points(pic, &[0.5]).is_empty());
    assert_eq!(curve_points(pic, &[0.0, f64::NAN, 2.0]).len(), 3);
    assert_eq!(Listing::parse(&json!("junk")), Listing::default());
    assert_eq!(Listing::parse(&json!({"graphs": [{"layer": 3}]})).graphs.len(), 1);
}

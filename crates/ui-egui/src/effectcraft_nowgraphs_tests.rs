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
fn the_shelf_docks_right_of_the_card_in_panel_units_and_grows_down_the_timeline() {
    // A 480×270 card (panel units), its viewer bar and a Timeline under it, ending left of the shelf.
    let card = Rect::from_min_size(pos2(0.0, 0.0), vec2(480.0, 270.0));
    let footer = Rect::from_min_size(pos2(0.0, 278.0), vec2(480.0, 30.0));
    let strip = Rect::from_min_max(pos2(-368.0, 316.0), pos2(480.0, 604.0));
    let s = shelf_rect(card, card, Some(strip), Some(footer), content_height(1));
    assert!(s.min.x >= card.max.x + GAP - 0.01);
    assert!(!s.intersects(card));
    assert_eq!(s.min.y, card.min.y);
    assert_eq!(s.width(), SHELF_W);
    assert!(s.height() >= MIN_SHELF_H && s.height() <= content_height(1).max(MIN_SHELF_H) + 0.01, "{s:?}");
    // Ten graphs: as tall as picture + Timeline, no taller (the rest scrolls).
    let tall = shelf_rect(card, card, Some(strip), Some(footer), content_height(10));
    assert!((tall.max.y - strip.max.y).abs() < 0.01, "{tall:?}");
    // A Timeline reaching under the shelf: it stops at the viewer bar.
    let wide = Rect::from_min_max(pos2(-368.0, 316.0), pos2(1200.0, 604.0));
    let s = shelf_rect(card, card, Some(wide), Some(footer), content_height(10));
    assert!((s.max.y - footer.max.y).abs() < 0.01 && !s.intersects(wide), "{s:?}");
    // Junk content never panics.
    let _ = shelf_rect(card, card, None, None, f32::NAN);
}

#[test]
fn scrolling_reaches_the_last_graph_and_stays_in_range() {
    let view = 300.0;
    let content = content_height(5) - HEAD_H;
    assert_eq!(clamp_scroll(-40.0, content, view), 0.0);
    let most = clamp_scroll(1e9, content, view);
    assert!((most - (content - view)).abs() < 0.01, "the last card's bottom reaches the view's");
    assert!(most + view >= 5.0 * CARD_H - 0.01);
    assert_eq!(clamp_scroll(50.0, 100.0, view), 0.0, "everything fits: no scroll");
    assert_eq!(clamp_scroll(f32::NAN, content, view), 0.0);
}

#[test]
fn key_times_map_to_the_axis_and_snap_to_frames() {
    let g = Rect::from_min_size(pos2(10.0, 0.0), vec2(200.0, 40.0));
    let ax = axis(1.0, 2.0, 0.04);
    assert!(ax.0 < 1.0 && ax.1 > 2.0, "room either side: {ax:?}");
    for t in [1.0, 1.5, 2.0] {
        assert!((x_time(g, ax, time_x(g, ax, t)) - t).abs() < 1e-4);
    }
    assert!((snap(1.013, 0.04) - 1.0).abs() < 1e-9);
    assert!((snap(1.03, 0.04) - 1.04).abs() < 1e-9);
    assert!(snap(f64::NAN, 0.04).is_nan());
    assert_eq!(snap(1.3, 0.0), 1.3);
    let _ = time_x(g, (1.0, 1.0), 1.0);
    let _ = x_time(Rect::NOTHING, ax, f32::NAN);
    let l = Listing { frame_rate: 0.0, ..Listing::default() };
    assert!((l.frame() - 0.04).abs() < 1e-9);
}

#[test]
fn a_key_drag_is_one_undo_step_and_the_listing_follows_the_move() {
    let (mut h, _) = host("key");
    let ctx = egui::Context::default();
    let l = list(&mut h);
    let g = l.graphs[0].clone();
    assert_eq!(g.keys(), [0.4, 1.2]);
    // Drag the second key from 1.2 s to 1.5 s, frame by frame, one undo key.
    for t in [1.28, 1.36, 1.5] {
        h.run_engine(&ctx, "nowGraphs.moveKey", move_call(l.comp, &g, 1, t, "nowgraphs-drag-7")).unwrap();
    }
    let g2 = list(&mut h).graphs[0].clone();
    assert!((g2.end - 1.5).abs() < 0.01, "{g2:?}");
    assert!((g2.start - 0.4).abs() < 0.01);
    h.invoke(&ctx, "edit.undo", json!({})).unwrap();
    let g3 = list(&mut h).graphs[0].clone();
    assert!((g3.end - 1.2).abs() < 0.01, "one undo takes the whole drag back: {g3:?}");
}

#[test]
fn graphs_under_the_playhead_come_first() {
    let (mut h, bx) = host("order");
    let ctx = egui::Context::default();
    // A second layer whose fade ended 0.3 s before the playhead (0.8 s): listed after the Box.
    let early =
        h.invoke(&ctx, "layer.newSolid", json!({"name": "Early", "color": "#0000ff", "width": 8, "height": 8})).unwrap()["layer"].as_u64().unwrap();
    for (t, v) in [(0.1, 0.0), (0.5, 100.0)] {
        h.invoke(&ctx, "prop.addKey", json!({"layer": early, "path": "transform/opacity", "time": t, "value": v})).unwrap();
    }
    h.set_time(0.8);
    let l = list(&mut h);
    assert_eq!(l.graphs.len(), 2, "{l:#?}");
    assert_eq!(l.graphs[0].layer, bx);
    assert!(l.graphs[0].contains_playhead && !l.graphs[1].contains_playhead);
    assert_eq!(l.graphs[0].object, 0);
    assert!(l.find(l.graphs[1].who()).is_some_and(|(n, _)| n == 1));
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

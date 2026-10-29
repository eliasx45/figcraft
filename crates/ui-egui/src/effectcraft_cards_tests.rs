//! Unsaved EffectCraft edits are asked about (quit, close, leaving or deleting the card), new
//! cards glide into view, ⌘N in the Timeline opens New Composition, cards render at EffectCraft's
//! preview resolution and cache their range before playing; junk never panics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use egui::{Event, Key, Modifiers};
use figcraft_doc::{EcComp, NodeId};
use figcraft_ec_inproc::effectcraft_engine::preview::{PreviewPreset, PreviewResolution};
use figcraft_ec_inproc::effectcraft_engine::project::{ItemId, Resolution};
use figcraft_geom::Rect as PRect;
use serde_json::json;

use super::{Answer, Then};
use crate::layers_drag_tests::Ui;

/// A fresh folder with EffectCraft's sample project (`C`: 64×36, 2 s, 25 fps, one red solid).
fn sample(name: &str) -> (PathBuf, PathBuf, u64) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-cards-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    (d, p, cid)
}

/// The app with the sample comp as a card at 0, 0, `width` page units wide.
fn app_with_card(p: &Path, cid: u64, width: f64) -> (Ui, NodeId) {
    let mut s = figcraft_engine::Session::new();
    let r = s
        .execute(
            "effectcraft.place",
            &json!({"comp": "C", "compId": cid, "project": p.to_str().unwrap(), "start": 0, "end": 2, "fps": 25, "x": 0, "y": 0, "cardWidth": width}),
        )
        .unwrap();
    let card = figcraft_engine::params::id_of(&r["id"]).unwrap();
    (Ui::new(s), card)
}

/// The card's Timeline open with one unsaved edit (a solid).
fn edited(name: &str) -> (PathBuf, PathBuf, u64, Ui, NodeId) {
    let (d, p, cid) = sample(name);
    let (mut u, card) = app_with_card(&p, cid, 640.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    let ctx = u.ctx.clone();
    let h = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
    h.invoke(&ctx, "layer.newSolid", json!({"color": "#00ff00", "width": 8, "height": 8})).unwrap();
    assert!(h.is_dirty());
    u.frame(vec![]);
    (d, p, cid, u, card)
}

fn saved_layers(p: &Path, cid: u64) -> usize {
    let ec = figcraft_ec_inproc::InProc::open(&figcraft_ec_inproc::key(p)).unwrap();
    ec.session().project.comp(ItemId(cid)).map_or(0, |c| c.layers.len())
}

fn then(u: &Ui) -> Option<Then> {
    u.app.effectcraft.cards.ask.as_ref().map(|a| a.then)
}

#[test]
fn closing_with_unsaved_timeline_edits_asks_and_save_saves_the_ecproj() {
    let (d, p, cid, mut u, _) = edited("close-save");
    let before = saved_layers(&p, cid);
    let ctx = u.ctx.clone();
    crate::windows::request_close(&mut u.app, &ctx);
    assert_eq!(then(&u), Some(Then::Close), "the window waits for the answer");
    assert!(!u.app.ui.quit);
    u.frame(vec![]);
    assert!(u.ctx.memory(|m| m.area_rect(egui::Id::new(Some("Unsaved EffectCraft edits"))).is_some()), "the question is on screen");
    super::answer(&mut u.app, &ctx, Answer::Save);
    assert!(then(&u).is_none());
    assert!(!super::dirty(&u.app), "saved");
    assert_eq!(saved_layers(&p, cid), before + 1, "the .ecproj on disk has the edit");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn dont_save_drops_the_edits_and_cancel_keeps_them() {
    let (d, p, cid, mut u, _) = edited("close-discard");
    let before = saved_layers(&p, cid);
    let ctx = u.ctx.clone();
    crate::windows::request_close(&mut u.app, &ctx);
    super::answer(&mut u.app, &ctx, Answer::Cancel);
    assert!(then(&u).is_none());
    assert!(super::dirty(&u.app), "Cancel: the edits stay");
    crate::windows::request_close(&mut u.app, &ctx);
    super::answer(&mut u.app, &ctx, Answer::DontSave);
    assert!(u.app.effectcraft.panel.is_none(), "the Timeline and its edits went");
    assert_eq!(saved_layers(&p, cid), before, "the file is as it was");
    // Nothing left to ask: the close goes on.
    assert!(!super::hold_close(&mut u.app));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_timeline_without_edits_never_asks() {
    let (d, p, cid) = sample("clean");
    let (mut u, card) = app_with_card(&p, cid, 640.0);
    assert!(!super::hold_close(&mut u.app), "no Timeline");
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    assert!(!super::hold_close(&mut u.app), "open, nothing edited");
    u.app.run("file.new", json!({}));
    u.frame(vec![]);
    assert!(then(&u).is_none(), "leaving its file without edits asks nothing");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn leaving_the_cards_file_asks_and_cancel_goes_back() {
    let (d, _, _, mut u, _) = edited("leave");
    let mine = u.app.session.ds().unwrap().uid;
    u.app.run("file.new", json!({}));
    u.frame(vec![]);
    assert_eq!(then(&u), Some(Then::Left { back: mine }));
    let ctx = u.ctx.clone();
    super::answer(&mut u.app, &ctx, Answer::Cancel);
    assert_eq!(u.app.session.ds().unwrap().uid, mine, "back in the card's file");
    u.frame(vec![]);
    assert!(then(&u).is_none(), "and nothing more to ask there");
    assert!(super::dirty(&u.app));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn deleting_the_card_asks_and_cancel_keeps_the_edits_quietly() {
    let (d, _, _, mut u, card) = edited("delete");
    u.app.run("edit.delete", json!({"ids": [card]}));
    u.frame(vec![]);
    assert_eq!(then(&u), Some(Then::Gone { file_open: true }));
    let ctx = u.ctx.clone();
    super::answer(&mut u.app, &ctx, Answer::Cancel);
    for _ in 0..3 {
        u.frame(vec![]);
    }
    assert!(then(&u).is_none(), "asked once");
    assert!(super::dirty(&u.app), "kept: ⌘Z brings the card back with them");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_closed_file_offers_save_or_dont_save() {
    let (d, p, cid, mut u, _) = edited("file-closed");
    let i = u.app.session.active;
    u.app.run("file.new", json!({}));
    u.app.run("file.close", json!({"index": i, "force": true}));
    u.frame(vec![]);
    assert_eq!(then(&u), Some(Then::Gone { file_open: false }));
    let before = saved_layers(&p, cid);
    let ctx = u.ctx.clone();
    super::answer(&mut u.app, &ctx, Answer::Save);
    assert_eq!(saved_layers(&p, cid), before + 1);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn the_view_moves_only_as_much_as_it_must() {
    let v = (500.0, 400.0, 1.0);
    let vp = (1000.0, 800.0);
    assert_eq!(super::view_to_show(v, vp, PRect::new(100.0, 100.0, 300.0, 300.0)), None, "in view: nothing moves");
    let (cx, cy, z) = super::view_to_show(v, vp, PRect::new(1200.0, 100.0, 1400.0, 300.0)).unwrap();
    assert_eq!(z, 1.0, "fits: the zoom stays");
    assert_eq!(cy, 400.0, "no vertical move needed");
    assert!((cx + 500.0 - 48.0 - 1400.0).abs() < 1e-6, "its right edge just inside the margin: {cx}");
    let (cx, cy, z) = super::view_to_show(v, vp, PRect::new(0.0, 0.0, 4000.0, 1000.0)).unwrap();
    assert!(z < 1.0 && z > 0.0, "too big: zoomed out to fit");
    assert_eq!((cx, cy), (2000.0, 500.0));
    // Never zooms in.
    let (_, _, z) = super::view_to_show((0.0, 0.0, 0.1), vp, PRect::new(9000.0, 9000.0, 9010.0, 9010.0)).unwrap();
    assert_eq!(z, 0.1);
}

#[test]
fn a_new_card_glides_into_view_with_its_timeline() {
    let (d, p, cid) = sample("reveal");
    let (mut u, card) = app_with_card(&p, cid, 900.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    let zoom = u.app.session.ds().unwrap().view.zoom;
    let ctx = u.ctx.clone();
    let to = crate::effectcraft_ui::project::Target { project: Some(p.clone()), from: Some(card), ..Default::default() };
    let new = crate::effectcraft_ui::project::create(&mut u.app, &ctx, to, json!({"name": "Far", "width": 64, "height": 36, "duration": 1})).unwrap();
    assert_ne!(new, card);
    let doc = u.app.session.doc().unwrap();
    let r = crate::effectcraft_ui::screen_rect(&u.app, doc, new).unwrap();
    assert!(!u.app.canvas.rect.contains_rect(r), "placed out of view to start with");
    let mut views = Vec::new();
    for _ in 0..40 {
        u.frame(vec![]);
        let v = &u.app.session.ds().unwrap().view;
        views.push(v.cx);
    }
    let doc = u.app.session.doc().unwrap();
    let r = crate::effectcraft_ui::screen_rect(&u.app, doc, new).unwrap();
    assert!(u.app.canvas.rect.expand(1.0).contains_rect(r), "the new card is in view: {r:?} in {:?}", u.app.canvas.rect);
    assert!(views.windows(2).filter(|w| (w[1] - w[0]).abs() > 1e-6).count() > 2, "it glided over several frames");
    assert!(u.app.session.ds().unwrap().view.zoom <= zoom, "never zoomed in");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn cmd_n_in_the_timeline_opens_new_composition_and_places_the_comp() {
    let (d, p, cid) = sample("cmd-n");
    let (mut u, card) = app_with_card(&p, cid, 640.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    let comps = |u: &Ui| TimelineHost::session(&u.app.effectcraft.panel.as_ref().unwrap().host).project.comps().count();
    let n = comps(&u);
    let ctx = u.ctx.clone();
    let r = u.app.effectcraft.panel.as_mut().unwrap().host.invoke(&ctx, "comp.new", json!({})).unwrap();
    assert_eq!(r["dialog"], json!("comp.new"));
    assert_eq!(comps(&u), n, "no comp before OK");
    u.frame(vec![]);
    assert!(crate::effectcraft_ui::project::dialog_open(&u.ctx), "EffectCraft's New Composition shows");
    // The dialog's OK: the sized command makes the comp and its card.
    let to = crate::effectcraft_ui::project::Target { project: Some(p.clone()), ..Default::default() };
    let new = crate::effectcraft_ui::project::create(&mut u.app, &ctx, to, json!({"name": "N", "width": 32, "height": 32, "duration": 1})).unwrap();
    assert_eq!(comps(&u), n + 1);
    let panel = u.app.effectcraft.panel.as_ref().unwrap();
    assert_eq!(panel.card, new, "the Timeline is on the new card");
    // The key itself, with the Timeline focused.
    u.frame(vec![Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
    u.app.effectcraft.panel.as_mut().unwrap().focused = true;
    u.frame(vec![Event::Key { key: Key::N, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND }]);
    u.frame(vec![]);
    assert!(crate::effectcraft_ui::project::dialog_open(&u.ctx), "⌘N opened it");
    assert_eq!(comps(&u), n + 1, "and made nothing by itself");
    let _ = std::fs::remove_dir_all(d);
}

fn comp(cid: u64, p: &Path) -> EcComp {
    serde_json::from_value(json!({"comp": "C", "compId": cid, "project": p.to_string_lossy(), "start": 0.0, "end": 2.0, "fps": 25.0,
        "width": 1920, "height": 1080}))
    .unwrap()
}

#[test]
fn cards_render_at_the_viewer_and_preview_resolution() {
    let (d, p, cid) = sample("resolution");
    let (mut u, card) = app_with_card(&p, cid, 640.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    let c = comp(cid, &p);
    let mut w = vec![(card, c.clone(), 240u32, 0i64)];
    super::shape_wanted(&mut u.app, &mut w);
    assert_eq!(w[0].2, 240, "Auto: the card's size on screen");
    let h = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
    TimelineHost::session_mut(h).set_resolution(ItemId(cid), Resolution::Half).unwrap();
    let mut w = vec![(card, c.clone(), 240u32, 0i64)];
    super::shape_wanted(&mut u.app, &mut w);
    assert_eq!(w[0].2, 960, "the viewer's Half of 1920");
    // Playing: the Space preset's Quarter.
    let ctx = u.ctx.clone();
    let preset = PreviewPreset { resolution: PreviewResolution::Quarter, ..Default::default() };
    super::cache_step(&mut u.app, &ctx, &preset, 0.0);
    u.app.session.effectcraft.play(card, &c, Some(0.0));
    let mut w = vec![(card, c.clone(), 240u32, 0i64)];
    super::shape_wanted(&mut u.app, &mut w);
    assert_eq!(w[0].2, 480, "Quarter while it plays");
    assert_eq!(super::side_at(&c, 1), 1920);
    assert_eq!(super::side_at(&c, 0), 1920, "junk factor is Full");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn cache_before_playback_holds_play_until_the_range_is_rendered() {
    let (d, p, cid) = sample("cache");
    let (mut u, card) = app_with_card(&p, cid, 640.0);
    let c = u.app.session.doc().unwrap().node(card).unwrap().effectcraft.as_deref().unwrap().clone();
    let ctx = u.ctx.clone();
    let preset = PreviewPreset { cache_before_playback: true, ..Default::default() };
    super::cache_step(&mut u.app, &ctx, &preset, 0.0);
    u.app.session.effectcraft.play(card, &c, Some(0.0));
    super::cache_step(&mut u.app, &ctx, &preset, 0.0);
    let k = u.app.effectcraft.cards.cache.clone().expect("caching");
    assert_eq!(k.card, card);
    assert_eq!(k.frames.len(), 50, "the whole 2 s at 25 fps");
    assert!(!u.app.session.effectcraft.playing(), "it holds still while caching");
    // Play again meanwhile: what is cached plays.
    u.app.session.effectcraft.play(card, &c, Some(0.0));
    super::cache_step(&mut u.app, &ctx, &preset, 0.1);
    assert!(u.app.effectcraft.cards.cache.is_none());
    assert!(u.app.session.effectcraft.playing());
    // Nothing arrives for a while (EffectCraft unreachable): it plays what it has.
    u.app.session.effectcraft.pause();
    super::cache_step(&mut u.app, &ctx, &preset, 1.0);
    u.app.session.effectcraft.play(card, &c, Some(0.0));
    super::cache_step(&mut u.app, &ctx, &preset, 1.0);
    assert!(u.app.effectcraft.cards.cache.is_some());
    super::cache_step(&mut u.app, &ctx, &preset, 12.0);
    assert!(u.app.effectcraft.cards.cache.is_none());
    assert!(u.app.session.effectcraft.playing());
    super::cache_step(&mut u.app, &ctx, &preset, 12.1);
    assert!(u.app.effectcraft.cards.cache.is_none(), "its own play isn't a new press");
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn range_frames_follow_skip_and_the_budget() {
    let p = PathBuf::from("/nowhere.ecproj");
    let c = comp(1, &p);
    assert_eq!(super::range_frames(&c, (0.0, 1.0), 1, 240).len(), 25);
    assert_eq!(super::range_frames(&c, (0.0, 1.0), 2, 240), (0..25).step_by(2).collect::<Vec<_>>());
    let big = EcComp { end: 600.0, ..c.clone() };
    let n = super::range_frames(&big, (0.0, 600.0), 1, 1920).len();
    assert!((1..15_000).contains(&n), "full-size frames: only what the budget holds ({n})");
}

#[test]
fn junk_never_panics() {
    let p = PathBuf::from("");
    let c = comp(0, &p);
    for r in [(f64::NAN, 1.0), (f64::INFINITY, f64::NEG_INFINITY), (5.0, -5.0), (-1e300, 1e300)] {
        let _ = super::range_frames(&c, r, 0, 0);
        let _ = super::range_frames(&EcComp { fps: 0.0, width: 0, height: 0, ..c.clone() }, r, u32::MAX, u32::MAX);
    }
    for v in [(f64::NAN, 0.0, 1.0), (0.0, 0.0, 0.0), (0.0, 0.0, -1.0), (1e308, -1e308, 1e-300)] {
        for vp in [(0.0, 0.0), (f64::NAN, 10.0), (1e9, 1e9), (-5.0, 3.0)] {
            for r in [PRect::new(0.0, 0.0, 1.0, 1.0), PRect::new(f64::NAN, 0.0, 1.0, f64::INFINITY), PRect::new(5.0, 5.0, -5.0, -5.0)] {
                let _ = super::view_to_show(v, vp, r);
            }
        }
    }
    for u in [f64::NAN, -1.0, 2.0, f64::INFINITY] {
        let e = super::ease(u);
        assert!((0.0..=1.0).contains(&e));
    }
    let mut ui = Ui::new(figcraft_engine::Session::new());
    let ctx = ui.ctx.clone();
    assert!(!super::hold_close(&mut ui.app));
    for a in [Answer::Save, Answer::DontSave, Answer::Cancel] {
        super::answer(&mut ui.app, &ctx, a);
    }
    super::reveal(&mut ui.app, NodeId(u64::MAX));
    super::reveal_new(&mut ui.app, &[]);
    let mut w = vec![(NodeId(u64::MAX), c.clone(), 0u32, i64::MIN)];
    super::shape_wanted(&mut ui.app, &mut w);
    let preset = PreviewPreset { cache_before_playback: true, custom_resolution: 0, resolution: PreviewResolution::Custom, ..Default::default() };
    super::cache_step(&mut ui.app, &ctx, &preset, f64::NAN);
    ui.app.session.effectcraft.play(NodeId(u64::MAX), &c, Some(f64::NAN));
    super::cache_step(&mut ui.app, &ctx, &preset, f64::INFINITY);
    for _ in 0..3 {
        ui.frame(vec![]);
    }
    // The Timeline's New Composition with junk settings.
    let (d, sp, cid) = sample("junk");
    let (mut u, card) = app_with_card(&sp, cid, 640.0);
    assert!(u.app.effectcraft_open_panel(card));
    let ctx = u.ctx.clone();
    for params in [json!(null), json!([]), json!("x"), json!({"width": "wide"}), json!({"height": -1})] {
        let h = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
        let _ = h.invoke(&ctx, "comp.new", params);
        u.frame(vec![]);
    }
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn a_card_placed_by_comp_name_follows_the_viewer_resolution_too() {
    let (d, p, cid) = sample("resolution-by-name");
    let (mut u, card) = app_with_card(&p, cid, 640.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    // Cards placed from the app before comp ids carry the name only (compId 0).
    let mut c = comp(cid, &p);
    c.comp_id = 0;
    assert_eq!(super::chosen_side(&u.app, card, &c), None, "Auto");
    let h = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
    TimelineHost::session_mut(h).set_resolution(ItemId(cid), Resolution::Quarter).unwrap();
    assert_eq!(super::chosen_side(&u.app, card, &c), Some(480), "the viewer's Quarter of 1920");
    let _ = std::fs::remove_dir_all(d);
}

/// A card's own resolution (its title row's chip) decides its frame size over the comp's viewer
/// Resolution, is kept on the card (one undo step) and reads back; junk values are none.
#[test]
fn a_cards_own_resolution_wins_and_is_kept_on_the_card() {
    let (d, p, cid) = sample("resolution-per-card");
    let (mut u, card) = app_with_card(&p, cid, 640.0);
    let c = comp(cid, &p);
    assert_eq!(super::chosen_side(&u.app, card, &c), None, "Auto: what its size on screen needs");
    let value = super::res_value(Resolution::Half);
    u.app.run("node.set", json!({"ids": [card], "props": {"pluginData": {super::RES_KEY: value}}}));
    assert_eq!(super::card_resolution(u.app.session.doc().unwrap(), card), Some(Resolution::Half));
    assert_eq!(super::chosen_side(&u.app, card, &c), Some(960), "Half of 1920");
    u.app.run("edit.undo", json!({}));
    assert_eq!(super::chosen_side(&u.app, card, &c), None, "undo takes it back");
    for r in [Resolution::Auto, Resolution::Full, Resolution::Third, Resolution::Quarter, Resolution::Custom(7)] {
        assert_eq!(super::parse_res(&super::res_value(r)), Some(r));
    }
    for junk in ["", "custom:", "custom:x", "huge", "custom:-3"] {
        assert_eq!(super::parse_res(junk), None, "{junk}");
    }
    u.frame(vec![]);
    let _ = std::fs::remove_dir_all(d);
}

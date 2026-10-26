//! The card's right-click menu: EffectCraft's viewer menu over a layer and over an empty part of
//! the card, an entry doing EffectCraft's edit as one undo step, Composition Settings, FigCraft's
//! own menu when the Timeline is closed or with ⌥, and junk.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use egui::{Event, PointerButton, Pos2, pos2};
use figcraft_doc::NodeId;
use serde_json::json;

use super::{gate, open_menu, picture_of, run};
use crate::effectcraft_panel::EcHost;
use crate::layers_drag_tests::Ui;

/// EffectCraft's sample project (`C`: 64×36, 2 s, 25 fps, one red solid filling it).
fn sample(name: &str) -> (PathBuf, PathBuf, u64) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-menu-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    (d, p, cid)
}

/// The app with the sample comp as a big card at 0, 0.
fn app_with_card(p: &Path, cid: u64) -> (Ui, NodeId) {
    let mut s = figcraft_engine::Session::new();
    let r = s
        .execute(
            "effectcraft.place",
            &json!({"comp": "C", "compId": cid, "project": p.to_str().unwrap(), "start": 0, "end": 2, "fps": 25, "x": 0, "y": 0, "cardWidth": 1400}),
        )
        .unwrap();
    let card = figcraft_engine::params::id_of(&r["id"]).unwrap();
    (Ui::new(s), card)
}

/// The app with the card's Timeline open (and drawn).
fn with_timeline(name: &str) -> (PathBuf, Ui, NodeId) {
    let (dir, p, cid) = sample(name);
    let (mut u, card) = app_with_card(&p, cid);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    u.frame(vec![]);
    (dir, u, card)
}

fn host(u: &Ui) -> &EcHost {
    &u.app.effectcraft.panel.as_ref().unwrap().host
}

fn host_mut(u: &mut Ui) -> &mut EcHost {
    &mut u.app.effectcraft.panel.as_mut().unwrap().host
}

/// A right-click at `at` (press and release), `mods` held.
fn right_click(u: &mut Ui, at: Pos2, mods: egui::Modifiers) {
    u.frame_with(vec![Event::PointerMoved(at)], mods);
    u.frame_with(vec![Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed: true, modifiers: mods }], mods);
    u.frame_with(vec![Event::PointerButton { pos: at, button: PointerButton::Secondary, pressed: false, modifiers: mods }], mods);
    u.frame(vec![]);
}

/// The solid's layer id.
fn solid(u: &Ui) -> u64 {
    host(u).session().active_comp().unwrap().layers[0].id.0
}

#[test]
fn a_right_click_over_a_layer_opens_effectcrafts_menu_for_it() {
    let (dir, mut u, card) = with_timeline("over-layer");
    let l = solid(&u);
    let ctx = u.ctx.clone();
    host_mut(&mut u).invoke(&ctx, "edit.deselectAll", json!({})).unwrap();
    let pic = picture_of(&u.app).expect("the card's picture is on screen");
    right_click(&mut u, pic.center(), egui::Modifiers::NONE);
    let open = open_menu(&u.ctx).expect("EffectCraft's menu opened");
    assert_eq!(open.card, card);
    assert_eq!(open.target.hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![l], "Select ▸ names the layer under the pointer");
    assert_eq!(host(&u).session().state.selected_layers.iter().map(|i| i.0).collect::<Vec<_>>(), vec![l], "the layer is picked, as in the viewer");
    assert!(egui::Popup::is_id_open(&u.ctx, super::popup_id()));
    assert!(u.app.ui.context_menu.is_none(), "FigCraft's own menu stays shut");
    assert!(u.app.effectcraft.panel.as_ref().unwrap().focused, "the Timeline has the keyboard (⌘Z undoes the menu's edits)");
    // Escape closes it.
    u.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    u.frame(vec![]);
    assert!(open_menu(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn an_entry_does_effectcrafts_edit_as_one_undo_step() {
    let (dir, mut u, card) = with_timeline("edit");
    let l = solid(&u);
    let ctx = u.ctx.clone();
    // What EffectCraft's menu does: Transform ▸ Flip Horizontal on the layer, straight on a session.
    let (_, p2, cid2) = sample("edit-reference");
    let mut comp = figcraft_doc::EcComp::new("C", 0.0, 2.0, 25.0);
    comp.comp_id = cid2;
    comp.project = p2.to_string_lossy().into_owned();
    let mut reference = EcHost::open(&p2, &comp, false).unwrap();
    let rl = reference.session().active_comp().unwrap().layers[0].id.0;
    reference.session_mut().execute("layer.transform", json!({"layers": [rl], "op": "flipH"})).unwrap();
    let scale = |h: &EcHost, id: u64| {
        let c = h.session().active_comp().unwrap();
        let layer = c.layers.iter().find(|x| x.id.0 == id).unwrap();
        layer.props.prop("transform/scale").unwrap().value_at(layer.layer_time(h.session().time())).as_vec3()
    };
    let before = scale(host(&u), l);
    // The entry's actions, as the menu pushes them: select its layers, then the command.
    run(&mut u.app, &ctx, card, "layer.select", json!({"layers": [l]})).unwrap();
    let undo_before = host(&u).session().history.undo.len();
    run(&mut u.app, &ctx, card, "layer.transform", json!({"layers": [l], "op": "flipH"})).unwrap();
    assert_eq!(scale(host(&u), l), scale(&reference, rl), "the same edit as EffectCraft's");
    assert_eq!(host(&u).session().history.undo.len(), undo_before + 1, "one undo step");
    assert!(host(&u).is_dirty(), "an unsaved edit of the card's project (⌘S saves it)");
    host_mut(&mut u).invoke(&ctx, "edit.undo", json!({})).unwrap();
    assert_eq!(scale(host(&u), l), before, "⌘Z takes it back in one go");
    // New ▸ Text puts the caret on the card.
    run(&mut u.app, &ctx, card, "layer.newText", json!({})).unwrap();
    assert!(host(&u).session().state.text_edit.is_some(), "typing goes into the new text");
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(p2.parent().unwrap());
}

#[test]
fn a_right_click_on_an_empty_part_of_the_card_shows_the_comp_entries() {
    let (dir, mut u, card) = with_timeline("empty");
    let l = solid(&u);
    let ctx = u.ctx.clone();
    host_mut(&mut u).invoke(&ctx, "edit.clear", json!({"layers": [l]})).unwrap();
    let pic = picture_of(&u.app).unwrap();
    right_click(&mut u, pic.center(), egui::Modifiers::NONE);
    let open = open_menu(&u.ctx).expect("EffectCraft's menu opened on the empty comp");
    assert!(open.target.hits.is_empty(), "no layer under the pointer");
    assert!(host(&u).session().state.selected_layers.is_empty());
    assert_eq!(open.target.comp, host(&u).comp());
    // Composition Settings… opens EffectCraft's dialog; its OK is one undo step.
    run(&mut u.app, &ctx, card, "comp.settings", json!({"comp": open.target.comp})).unwrap();
    assert!(super::dialog_open(&ctx));
    let mut d = effectcraft_ui_egui::panels::comp_settings::draft_of(
        &host(&u).session().project,
        figcraft_ec_inproc::effectcraft_engine::project::ItemId(open.target.comp),
    )
    .unwrap();
    d.width = 100;
    let params = effectcraft_ui_egui::panels::comp_settings::params(&d);
    super::apply_comp_settings(&mut u.app, &ctx, card, open.target.comp, params).unwrap();
    assert_eq!(host(&u).session().active_comp().unwrap().width, 100);
    host_mut(&mut u).invoke(&ctx, "edit.undo", json!({})).unwrap();
    assert_eq!(host(&u).session().active_comp().unwrap().width, 64);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn figcrafts_menu_shows_when_the_timeline_is_closed_or_with_alt() {
    let (dir, p, cid) = sample("figcraft-menu");
    let (mut u, card) = app_with_card(&p, cid);
    let r = crate::effectcraft_ui::screen_rect(&u.app, u.app.session.doc().unwrap(), card).unwrap();
    right_click(&mut u, r.center(), egui::Modifiers::NONE);
    let (_, ids) = u.app.ui.context_menu.clone().expect("FigCraft's menu for the card");
    assert_eq!(ids, vec![card]);
    assert!(open_menu(&u.ctx).is_none());
    u.app.ui.context_menu = None;
    // Timeline open, ⌥ held: FigCraft's again.
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    u.frame(vec![]);
    let pic = picture_of(&u.app).unwrap();
    right_click(&mut u, pic.center(), egui::Modifiers::ALT);
    assert!(u.app.ui.context_menu.is_some(), "⌥ right-click: FigCraft's menu");
    assert!(open_menu(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn junk_never_panics() {
    let (dir, mut u, card) = with_timeline("junk");
    let ctx = u.ctx.clone();
    for (cmd, params) in [
        ("", json!(null)),
        ("nope.nothing", json!({})),
        ("layer.transform", json!({"layers": [999_999], "op": "nope"})),
        ("layer.setBlendMode", json!({"layers": "x", "mode": 5})),
        ("effect.apply", json!({"effect": "\u{0}", "layers": [-1]})),
        ("comp.settings", json!({"comp": 424_242})),
        ("comp.settings", json!({"comp": "x"})),
        ("comp.copyFrame", json!({"comp": 424_242, "scale": f64::MAX})),
        ("layer.precompose", json!({})),
        ("app.solidSettings", json!({})),
        ("layer.newText", json!([1, 2])),
    ] {
        let _ = run(&mut u.app, &ctx, card, cmd, params.clone());
        let _ = run(&mut u.app, &ctx, NodeId(999_999), cmd, params);
    }
    assert!(gate("app.solidSettings").is_some() && gate("layer.transform").is_none() && gate("").is_none());
    let _ = super::apply_comp_settings(&mut u.app, &ctx, card, 424_242, json!({"width": -1}));
    let _ = super::apply_comp_settings(&mut u.app, &ctx, card, 1, json!(null));
    for at in [pos2(f32::NAN, 1.0), pos2(1e30, -1e30), pos2(-5.0, -5.0), pos2(1439.0, 899.0)] {
        assert!(!super::claims(&u.app, at, egui::Modifiers::NONE) || at.x.is_finite());
        right_click(&mut u, at, egui::Modifiers::NONE);
    }
    // A menu left open for a card that's gone closes.
    if let Some(p) = u.app.effectcraft.panel.as_mut() {
        p.open = false;
    }
    u.frame(vec![]);
    assert!(open_menu(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

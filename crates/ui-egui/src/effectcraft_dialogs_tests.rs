//! EffectCraft's Pre-compose, Interpret Footage and Replace Footage dialogs on a card's Timeline:
//! they open from the keys' and menus' requests (never from programmatic calls), OK does what
//! EffectCraft does through the Timeline's session (one undo step), Cancel changes nothing, junk
//! never panics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use effectcraft_ui_egui::panels::forms::FieldKind;
use effectcraft_ui_egui::panels::timeline::TimelineHost;
use figcraft_doc::NodeId;
use figcraft_ec_inproc::effectcraft_engine::project::{ItemId, ItemKind, LayerId, LayerSource};
use serde_json::{Value, json};

use super::{Which, shown};
use crate::effectcraft_panel::EcHost;
use crate::layers_drag_tests::Ui;

/// A fresh folder with EffectCraft's sample project (`C`: 64×36, 2 s, 25 fps, one red solid).
fn sample(name: &str) -> (PathBuf, PathBuf, u64) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-dialogs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    (d, p, cid)
}

/// A synthetic still (16×8, one colour).
fn still(dir: &Path, name: &str, rgb: [u8; 3]) -> PathBuf {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(16, 8, image::Rgba([rgb[0], rgb[1], rgb[2], 255])).save(&path).unwrap();
    path
}

/// The app with the sample comp as a card and its Timeline open.
fn app_with_timeline(p: &Path, cid: u64) -> (Ui, NodeId) {
    let mut s = figcraft_engine::Session::new();
    let r = s
        .execute(
            "effectcraft.place",
            &json!({"comp": "C", "compId": cid, "project": p.to_str().unwrap(), "start": 0, "end": 2, "fps": 25, "x": 0, "y": 0, "cardWidth": 1400}),
        )
        .unwrap();
    let card = figcraft_engine::params::id_of(&r["id"]).unwrap();
    let mut u = Ui::new(s);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    (u, card)
}

fn host(u: &mut Ui) -> &mut EcHost {
    &mut u.app.effectcraft.panel.as_mut().unwrap().host
}

fn invoke(u: &mut Ui, id: &str, params: Value) -> Result<Value, String> {
    let ctx = u.ctx.clone();
    host(u).invoke(&ctx, id, params)
}

fn select_layers(u: &mut Ui, layers: &[u64]) {
    invoke(u, "layer.select", json!({"layers": layers})).unwrap();
}

fn comp_names(h: &EcHost) -> Vec<String> {
    h.session().project.comps().filter_map(|(id, _)| h.session().project.item(*id).map(|i| i.name.clone())).collect()
}

fn escape(u: &mut Ui) {
    u.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
}

#[test]
fn precompose_opens_from_the_key_request_and_ok_makes_the_precomp_named_as_typed() {
    let (dir, p, cid) = sample("precompose");
    let (mut u, card) = app_with_timeline(&p, cid);
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    let rev = host(&mut u).session().revision;
    // ⌘⇧C / the Timeline's menu run layer.precompose without a name: the dialog, nothing done yet.
    let r = invoke(&mut u, "layer.precompose", json!({})).unwrap();
    assert_eq!(r["dialog"], json!("layer.precompose"));
    assert_eq!(host(&mut u).session().revision, rev, "nothing changes until OK");
    u.frame(vec![]);
    u.frame(vec![]);
    let mut s = shown(&u.ctx).expect("the Pre-compose dialog shows");
    let d = s.precompose_draft().unwrap();
    assert_eq!(d.layers, vec![solid]);
    assert!(d.can_leave && d.open, "EffectCraft's defaults");
    d.name = "Glow".into();
    let ctx = u.ctx.clone();
    super::finish(&mut u.app, &ctx, &s).unwrap();
    let h = host(&mut u);
    assert!(comp_names(h).contains(&"Glow".to_string()), "named as typed: {:?}", comp_names(h));
    let glow =
        h.session().project.comps().find(|(id, _)| h.session().project.item(**id).is_some_and(|i| i.name == "Glow")).map(|(id, _)| id.0).unwrap();
    // Open New Composition: a card of it, right of the card, with the Timeline on it.
    let panel = u.app.effectcraft.panel.as_ref().unwrap();
    assert_eq!(panel.host.comp(), glow);
    assert_ne!(panel.card, card);
    let doc = u.app.session.doc().unwrap();
    assert!(doc.nodes().any(|n| n.effectcraft.as_deref().is_some_and(|c| c.comp == "Glow" && c.comp_id == glow)));
    assert!(panel.host.is_dirty(), "an unsaved edit (⌘S saves it)");
    // One undo step in the Timeline's session.
    invoke(&mut u, "edit.undo", json!({})).unwrap();
    assert!(!comp_names(host(&mut u)).contains(&"Glow".to_string()));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn precompose_unticked_keeps_the_timeline_and_two_precomps_get_their_own_names() {
    let (dir, p, cid) = sample("precompose-stay");
    let (mut u, card) = app_with_timeline(&p, cid);
    for (i, name) in ["Pre A", "Pre B"].iter().enumerate() {
        let layer = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
        select_layers(&mut u, &[layer]);
        invoke(&mut u, "layer.precompose", json!({"layers": [layer]})).unwrap();
        let mut s = shown(&u.ctx).unwrap();
        let d = s.precompose_draft().unwrap();
        d.name = (*name).into();
        d.open = false;
        let ctx = u.ctx.clone();
        let path = host(&mut u).path().to_path_buf();
        super::precompose(&mut u.app, &ctx, &path, d).unwrap();
        let panel = u.app.effectcraft.panel.as_ref().unwrap();
        assert_eq!((panel.card, panel.host.comp()), (card, cid), "the Timeline stays on its comp ({i})");
    }
    let names = comp_names(host(&mut u));
    assert!(names.contains(&"Pre A".to_string()) && names.contains(&"Pre B".to_string()), "{names:?}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn cancel_changes_nothing() {
    let (dir, p, cid) = sample("cancel");
    let (mut u, _) = app_with_timeline(&p, cid);
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    let rev = host(&mut u).session().revision;
    let comps = comp_names(host(&mut u));
    invoke(&mut u, "layer.precompose", json!({})).unwrap();
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_some());
    escape(&mut u);
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none(), "Escape cancels");
    assert_eq!(host(&mut u).session().revision, rev);
    assert_eq!(comp_names(host(&mut u)), comps);
    // Interpret Footage too.
    let item = invoke(&mut u, "file.importPlaceholder", json!({"name": "Shot"})).unwrap()["item"].as_u64().unwrap();
    let rev = host(&mut u).session().revision;
    invoke(&mut u, "file.interpretFootage", json!({"item": item})).unwrap();
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_some());
    escape(&mut u);
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none());
    assert_eq!(host(&mut u).session().revision, rev);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn interpret_footage_opens_for_the_project_item_or_the_footage_layer_and_ok_changes_it() {
    let (dir, p, cid) = sample("interpret");
    let (mut u, _) = app_with_timeline(&p, cid);
    let item = invoke(&mut u, "file.importPlaceholder", json!({"name": "Shot", "width": 64, "height": 36})).unwrap()["item"].as_u64().unwrap();
    let layer = invoke(&mut u, "layer.addItem", json!({"item": item})).unwrap()["layer"].as_u64().unwrap();
    // The footage layer selected in the Timeline (which has the keyboard): ⌘⌥G asks for its footage.
    host(&mut u).set_focus(effectcraft_ui_egui::dock::PanelKind::Timeline);
    host(&mut u).session_mut().state.project_selection.clear();
    select_layers(&mut u, &[layer]);
    invoke(&mut u, "file.interpretFootage", json!({})).unwrap();
    u.frame(vec![]);
    let mut s = shown(&u.ctx).expect("Interpret Footage shows");
    let f = s.form().unwrap();
    assert_eq!(f.title, "Interpret Footage");
    assert_eq!(f.base["items"], json!([item]), "for the layer's footage");
    for fl in &mut f.fields {
        match (fl.key.as_str(), &mut fl.kind) {
            ("frameRate", FieldKind::Number { value, .. }) => *value = 12.0,
            ("loop", FieldKind::Number { value, .. }) => *value = 3.0,
            _ => {}
        }
    }
    let ctx = u.ctx.clone();
    super::finish(&mut u.app, &ctx, &s).unwrap();
    let h = host(&mut u);
    let Some(ItemKind::Footage(ft)) = h.session().project.item(ItemId(item)).map(|i| &i.kind) else { panic!("footage") };
    assert!((ft.frame_rate.as_f64() - 12.0).abs() < 1e-6, "interpreted at 12 fps");
    assert_eq!(ft.loop_count, 3);
    // With its settings given (a script, the control channel), it just runs.
    assert!(super::intercept(h, &ctx, "file.interpretFootage", &json!({"frameRate": 24})).is_none());
    // From the Project tab: its selected footage.
    h.set_focus(effectcraft_ui_egui::dock::PanelKind::Project);
    h.session_mut().state.project_selection = vec![ItemId(item)];
    h.session_mut().state.selected_layers.clear();
    super::request(&mut u.app, None, Which::InterpretFootage).unwrap();
    assert!(shown(&u.ctx).unwrap().form().is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn replace_footage_picks_a_file_and_every_layer_using_the_item_shows_it() {
    let (dir, p, cid) = sample("replace");
    let red = still(&dir, "red.png", [255, 0, 0]);
    let blue = still(&dir, "blue.png", [0, 0, 255]);
    let (mut u, _) = app_with_timeline(&p, cid);
    let item = invoke(&mut u, "file.import", json!({"paths": [red.to_str().unwrap()]})).unwrap()["items"][0].as_u64().unwrap();
    let a = invoke(&mut u, "layer.addItem", json!({"item": item})).unwrap()["layer"].as_u64().unwrap();
    let b = invoke(&mut u, "layer.addItem", json!({"item": item})).unwrap()["layer"].as_u64().unwrap();
    select_layers(&mut u, &[a]);
    // ⌘H: the file dialog for the selected layer's footage (FigCraft's picker starts next frame).
    invoke(&mut u, "file.replaceFootage", json!({})).unwrap();
    assert_eq!(shown(&u.ctx).unwrap().replace_item(), Some(item));
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none(), "handed to the file dialog");
    // The picked file: EffectCraft's replace.
    let ctx = u.ctx.clone();
    let path = host(&mut u).path().to_path_buf();
    super::replace(&mut u.app, &ctx, &path, item, &blue).unwrap();
    let h = host(&mut u);
    let it = h.session().project.item(ItemId(item)).unwrap();
    let ItemKind::Footage(f) = &it.kind else { panic!("footage") };
    assert!(f.path.ends_with("blue.png"), "{}", f.path);
    let c = h.session().active_comp().unwrap();
    for l in [a, b] {
        assert_eq!(c.layer(LayerId(l)).unwrap().source, LayerSource::Footage { item: ItemId(item) }, "every layer uses the replaced item");
    }
    assert!(h.is_dirty());
    invoke(&mut u, "edit.undo", json!({})).unwrap();
    let ItemKind::Footage(f) = &host(&mut u).session().project.item(ItemId(item)).unwrap().kind else { panic!() };
    assert!(f.path.ends_with("red.png"), "one undo step");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_card_menu_entry_point_opens_the_cards_timeline_and_its_dialog() {
    let (dir, p, cid) = sample("request");
    let (mut u, card) = app_with_timeline(&p, cid);
    u.app.effectcraft.panel.as_mut().unwrap().open = false;
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    super::request(&mut u.app, Some(card), Which::Precompose).unwrap();
    assert!(u.app.effectcraft.panel.as_ref().unwrap().open, "the card's Timeline opened");
    assert!(shown(&u.ctx).is_some());
    escape(&mut u);
    // Nothing selected: an error to show, no dialog.
    host(&mut u).session_mut().state.selected_layers.clear();
    assert!(super::request(&mut u.app, Some(card), Which::Precompose).is_err());
    assert!(super::request(&mut u.app, None, Which::ReplaceFootage).is_err(), "no footage selected");
    assert!(shown(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn never_from_programmatic_calls_or_the_control_channel() {
    let (dir, p, cid) = sample("programmatic");
    let (mut u, _) = app_with_timeline(&p, cid);
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    // With their parameters EffectCraft's commands run at once.
    let r = invoke(&mut u, "layer.precompose", json!({"layers": [solid], "name": "Scripted"})).unwrap();
    assert!(r.get("comp").is_some());
    assert!(shown(&u.ctx).is_none());
    // FigCraft's control channel has no route to them.
    for id in ["layer.precompose", "file.interpretFootage", "file.replaceFootage"] {
        let _ = u.app.run(id, json!({}));
    }
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none());
    // A read-only Timeline (the app has the project, can't mirror) says so instead.
    let mut c = figcraft_doc::EcComp::new("C", 0.0, 2.0, 25.0);
    c.comp_id = cid;
    c.project = p.to_str().unwrap().to_string();
    let ro = EcHost::open(&p, &c, true).unwrap();
    let ctx = egui::Context::default();
    assert!(matches!(super::intercept(&ro, &ctx, "layer.precompose", &json!({"layers": [solid]})), Some(Err(_))));
    assert!(super::shown(&ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn junk_never_panics() {
    let (dir, p, cid) = sample("junk");
    let (mut u, card) = app_with_timeline(&p, cid);
    let ctx = u.ctx.clone();
    for id in ["layer.precompose", "file.interpretFootage", "file.interpretProxy", "file.replaceFootage"] {
        for params in [
            json!(null),
            json!([1]),
            json!({"layers": "x"}),
            json!({"layers": [999_999]}),
            json!({"item": -1}),
            json!({"items": [u64::MAX]}),
            json!({"item": "x"}),
        ] {
            let _ = super::intercept(host(&mut u), &ctx, id, &params);
            u.frame(vec![]);
            escape(&mut u);
        }
    }
    for which in [Which::Precompose, Which::InterpretFootage, Which::ReplaceFootage] {
        let _ = super::request(&mut u.app, Some(NodeId(999_999)), which);
        let _ = super::request(&mut u.app, Some(card), which);
        let _ = super::request(&mut u.app, None, which);
    }
    let path = host(&mut u).path().to_path_buf();
    let _ = super::replace(&mut u.app, &ctx, &path, 999_999, Path::new("/nope/x.png"));
    let _ = super::replace(&mut u.app, &ctx, Path::new("/other.ecproj"), 1, Path::new(""));
    let mut d = effectcraft_ui_egui::panels::precomp::PrecomposeDraft { layers: vec![u64::MAX], name: "\u{0}".into(), ..Default::default() };
    let _ = super::precompose(&mut u.app, &ctx, &path, &d);
    d.open = true;
    let _ = super::precompose(&mut u.app, &ctx, Path::new("/nope"), &d);
    // The Timeline gone while a dialog is up: it closes quietly.
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    let _ = invoke(&mut u, "layer.precompose", json!({}));
    u.app.effectcraft.panel = None;
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

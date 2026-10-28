//! EffectCraft's Pre-compose, Interpret Footage, Replace Footage and layer dialogs (Solid, Light,
//! Camera, Layer Settings, Time Stretch, Layer Style; EMBED12) on a card's Timeline:
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

use effectcraft_ui_egui::panels::hosted_dialogs::Hosted;

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
    let mut ro = EcHost::open(&p, &c, true).unwrap();
    let ctx = egui::Context::default();
    assert!(matches!(super::intercept(&mut ro, &ctx, "layer.precompose", &json!({"layers": [solid]})), Some(Err(_))));
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

// ---- EMBED12: EffectCraft's layer dialogs.

/// The dialog shown now, as one of EffectCraft's layer dialogs.
fn layer_dialog(u: &Ui) -> Hosted {
    let mut s = shown(&u.ctx).expect("a dialog shows");
    s.layer().expect("one of EffectCraft's layer dialogs").clone()
}

/// OK in the dialog shown, with its state set to `h`.
fn ok_with(u: &mut Ui, h: Hosted) {
    let mut s = shown(&u.ctx).unwrap();
    *s.layer().unwrap() = h;
    let ctx = u.ctx.clone();
    super::finish(&mut u.app, &ctx, &s).unwrap();
    super::set_shown(&ctx, None);
}

fn layer_count(u: &mut Ui) -> usize {
    host(u).session().active_comp().unwrap().layers.len()
}

fn undo_steps(u: &mut Ui) -> usize {
    host(u).session().history.undo.len()
}

/// The card's right-click menu runs the entry's command through the Timeline's host.
fn menu_entry(u: &mut Ui, card: NodeId, cmd: &str) -> Result<(), String> {
    let ctx = u.ctx.clone();
    crate::effectcraft_ui::menu::run(&mut u.app, &ctx, card, cmd, json!({}))
}

#[test]
fn new_solid_opens_from_the_menu_and_ok_makes_the_typed_solid_in_one_undo_step() {
    let (dir, p, cid) = sample("solid");
    let (mut u, card) = app_with_timeline(&p, cid);
    let (n, steps, rev) = (layer_count(&mut u), undo_steps(&mut u), host(&mut u).session().revision);
    // New ▸ Solid… on the card: EffectCraft's Solid Settings with the comp's size, nothing made yet.
    menu_entry(&mut u, card, "app.solidSettings").unwrap();
    u.frame(vec![]);
    let Hosted::Solid(mut d) = layer_dialog(&u) else { panic!("Solid Settings") };
    assert_eq!((d.size, d.layer), ([64, 36], None), "Make Comp Size is the default");
    assert_eq!(host(&mut u).session().revision, rev, "nothing changes until OK");
    d.name = "Backdrop".into();
    d.size = [20, 10];
    d.color = [0.0, 1.0, 0.0];
    ok_with(&mut u, Hosted::Solid(d));
    assert_eq!(layer_count(&mut u), n + 1);
    assert_eq!(undo_steps(&mut u), steps + 1, "one undo step");
    let h = host(&mut u);
    let c = h.session().active_comp().unwrap();
    let l = c.layers.iter().find(|l| l.name == "Backdrop").expect("named as typed");
    let LayerSource::Solid { item } = l.source else { panic!("a solid") };
    let Some(ItemKind::Solid(so)) = h.session().project.item(item).map(|i| &i.kind) else { panic!("its solid") };
    assert_eq!((so.width, so.height, so.color), (20, 10, [0.0, 1.0, 0.0]));
    assert!(h.is_dirty(), "an unsaved edit (⌘S saves it)");
    // The next New Solid starts from that colour, as in EffectCraft.
    invoke(&mut u, "layer.newSolid", json!({})).unwrap();
    let Hosted::Solid(d) = layer_dialog(&u) else { panic!() };
    assert_eq!(d.color, [0.0, 1.0, 0.0]);
    u.frame(vec![]);
    escape(&mut u);
    u.frame(vec![]);
    invoke(&mut u, "edit.undo", json!({})).unwrap();
    assert_eq!(layer_count(&mut u), n);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_timelines_keys_open_the_dialogs_from_effectcrafts_keymap() {
    let (dir, p, cid) = sample("keys");
    let (mut u, _) = app_with_timeline(&p, cid);
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    let mut tried = 0;
    for id in ["layer.newSolid", "layer.newLight", "layer.newCamera", "layer.settings", "layer.timeStretch"] {
        // Elias's own keys when he has a keymap (⇧Y for New Solid), else EffectCraft's (⌘Y, ⌥⌘⇧L, ⌥⌘⇧C, ⌘⇧Y).
        let Some((mods, key)) = host(&mut u).session().shortcuts().shortcut_of(id, &json!({})).and_then(effectcraft_ui_egui::menus::parse_shortcut)
        else {
            continue;
        };
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods }],
            ..Default::default()
        };
        let rev = host(&mut u).session().revision;
        ctx.run_ui(input, |ui| {
            let _ = host(&mut u).keys(ui.ctx());
        })
        .textures_delta
        .clear();
        let s = shown(&ctx);
        assert!(s.is_some(), "{id}: its dialog");
        assert_eq!(host(&mut u).session().revision, rev, "{id}: nothing made yet");
        tried += 1;
    }
    assert!(tried > 0, "no key bound at all");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn light_and_camera_of_the_chosen_type_and_their_layer_settings() {
    let (dir, p, cid) = sample("3d");
    let (mut u, card) = app_with_timeline(&p, cid);
    // New ▸ Light…: a Point light.
    menu_entry(&mut u, card, "layer.newLight").unwrap();
    let Hosted::Light(mut d) = layer_dialog(&u) else { panic!("Light Settings") };
    assert_eq!(d.layer, None);
    d.name = "Key".into();
    d.kind = 2;
    let steps = undo_steps(&mut u);
    ok_with(&mut u, Hosted::Light(d));
    assert_eq!(undo_steps(&mut u), steps + 1, "one undo step");
    let light = {
        let c = host(&mut u).session().active_comp().unwrap().clone();
        let l = c.layers.iter().find(|l| l.name == "Key").expect("the light");
        assert!(matches!(l.source, LayerSource::Light { kind } if kind.label() == "Point"), "{:?}", l.source);
        l.id.0
    };
    // New ▸ Camera…: one-node.
    menu_entry(&mut u, card, "layer.newCamera").unwrap();
    let Hosted::Camera(mut d) = layer_dialog(&u) else { panic!("Camera Settings") };
    d.name = "Cam".into();
    d.two_node = false;
    ok_with(&mut u, Hosted::Camera(d));
    let cam = host(&mut u).session().active_comp().unwrap().layers.iter().find(|l| l.name == "Cam").map(|l| l.id.0).expect("the camera");
    // Layer Settings… on them: Light / Camera Settings with their values.
    select_layers(&mut u, &[light]);
    menu_entry(&mut u, card, "layer.settings").unwrap();
    let Hosted::Light(mut d) = layer_dialog(&u) else { panic!("Light Settings for a light") };
    assert_eq!((d.layer, d.kind), (Some(light), 2));
    d.name = "Fill".into();
    ok_with(&mut u, Hosted::Light(d));
    assert_eq!(host(&mut u).session().active_comp().unwrap().layer(LayerId(light)).unwrap().name, "Fill");
    select_layers(&mut u, &[cam]);
    super::request(&mut u.app, Some(card), Which::LayerSettings).unwrap();
    let Hosted::Camera(d) = layer_dialog(&u) else { panic!("Camera Settings for a camera") };
    assert_eq!((d.layer, d.two_node), (Some(cam), false));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layer_settings_on_a_solid_changes_it_and_on_a_null_its_name() {
    let (dir, p, cid) = sample("settings");
    let (mut u, card) = app_with_timeline(&p, cid);
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    let steps = undo_steps(&mut u);
    invoke(&mut u, "layer.settings", json!({})).unwrap();
    let Hosted::Solid(mut d) = layer_dialog(&u) else { panic!("Solid Settings for a solid") };
    assert_eq!(d.layer, Some(solid));
    d.color = [0.0, 0.0, 1.0];
    d.size = [32, 18];
    ok_with(&mut u, Hosted::Solid(d));
    assert_eq!(undo_steps(&mut u), steps + 1, "one undo step");
    let h = host(&mut u);
    let LayerSource::Solid { item } = h.session().active_comp().unwrap().layer(LayerId(solid)).unwrap().source else { panic!() };
    let Some(ItemKind::Solid(so)) = h.session().project.item(item).map(|i| &i.kind) else { panic!() };
    assert_eq!((so.width, so.height, so.color), (32, 18, [0.0, 0.0, 1.0]));
    // A null: Null Settings (its name).
    let null = invoke(&mut u, "layer.newNull", json!({})).unwrap()["layer"].as_u64().unwrap();
    select_layers(&mut u, &[null]);
    menu_entry(&mut u, card, "layer.settings").unwrap();
    let Hosted::Form(mut f) = layer_dialog(&u) else { panic!("Null Settings") };
    assert_eq!(f.title, "Null Settings");
    for fl in &mut f.fields {
        if let FieldKind::Text(t) = &mut fl.kind {
            *t = "Rig".into();
        }
    }
    ok_with(&mut u, Hosted::Form(f));
    assert_eq!(host(&mut u).session().active_comp().unwrap().layer(LayerId(null)).unwrap().name, "Rig");
    // A text layer has none, as in EffectCraft: no dialog, an error to show.
    let text = invoke(&mut u, "layer.newText", json!({"text": "Hi"})).unwrap()["layer"].as_u64().unwrap();
    select_layers(&mut u, &[text]);
    assert!(super::request(&mut u.app, Some(card), Which::LayerSettings).is_err());
    assert!(shown(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn time_stretch_applies_in_one_undo_step() {
    let (dir, p, cid) = sample("stretch");
    let (mut u, card) = app_with_timeline(&p, cid);
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    menu_entry(&mut u, card, "layer.timeStretch").unwrap();
    let Hosted::TimeStretch(mut d) = layer_dialog(&u) else { panic!("Time Stretch") };
    assert!((d.percent - 100.0).abs() < 1e-9);
    d.percent = 200.0;
    let steps = undo_steps(&mut u);
    ok_with(&mut u, Hosted::TimeStretch(d));
    assert_eq!(undo_steps(&mut u), steps + 1, "one undo step");
    let stretch = host(&mut u).session().active_comp().unwrap().layer(LayerId(solid)).unwrap().stretch;
    assert!((stretch - 200.0).abs() < 1e-6, "{stretch}");
    invoke(&mut u, "edit.undo", json!({})).unwrap();
    assert!((host(&mut u).session().active_comp().unwrap().layer(LayerId(solid)).unwrap().stretch - 100.0).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layer_style_options_edit_live_ok_keeps_and_cancel_takes_back() {
    let (dir, p, cid) = sample("styles");
    let (mut u, card) = app_with_timeline(&p, cid);
    let solid = host(&mut u).session().active_comp().unwrap().layers[0].id.0;
    select_layers(&mut u, &[solid]);
    let styles = |u: &mut Ui| {
        host(u).session().active_comp().unwrap().layer(LayerId(solid)).unwrap().layer_styles().is_some_and(|s| s.sub("dropShadow").is_some())
    };
    // Layer Styles ▸ Drop Shadow from the card's menu: the style is added and the dialog shows it.
    menu_entry(&mut u, card, "layer.style.dropShadow").unwrap();
    assert!(styles(&mut u));
    let Hosted::LayerStyle(st) = layer_dialog(&u) else { panic!("the Layer Style dialog") };
    assert_eq!((st.layer, st.selected.as_str()), (solid, "dropShadow"));
    // Cancel: the style goes again, nothing left changed.
    u.frame(vec![]);
    escape(&mut u);
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none());
    assert!(!styles(&mut u), "Cancel took the style back");
    // Layer Style Options…, a style ticked live, OK keeps it as one undo step.
    let steps = undo_steps(&mut u);
    menu_entry(&mut u, card, "layer.style.options").unwrap();
    u.frame(vec![]);
    let ctx = u.ctx.clone();
    let r = host(&mut u).run_engine(&ctx, "layer.style.add", json!({"layers": [solid], "style": "dropShadow"}));
    assert!(r.is_ok());
    let s = shown(&u.ctx).unwrap();
    super::finish(&mut u.app, &ctx, &s).unwrap();
    super::set_shown(&ctx, None);
    assert!(styles(&mut u), "OK keeps the edit");
    assert_eq!(undo_steps(&mut u), steps + 1, "one undo step");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layer_dialogs_never_open_from_programmatic_calls_or_read_only() {
    let (dir, p, cid) = sample("layer-programmatic");
    let (mut u, _) = app_with_timeline(&p, cid);
    // With their settings, they run at once.
    let n = layer_count(&mut u);
    invoke(&mut u, "layer.newSolid", json!({"color": "#ff0000", "width": 4, "height": 4})).unwrap();
    invoke(&mut u, "layer.newLight", json!({"kind": "Point"})).unwrap();
    invoke(&mut u, "layer.newCamera", json!({"type": "oneNode"})).unwrap();
    assert_eq!(layer_count(&mut u), n + 3);
    assert!(shown(&u.ctx).is_none());
    // FigCraft's control channel has no route to them.
    for id in ["app.solidSettings", "layer.newSolid", "layer.settings", "layer.timeStretch", "layer.style.options"] {
        let _ = u.app.run(id, json!({}));
    }
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none());
    // Read-only: no dialog (the edit is refused as usual).
    let mut c = figcraft_doc::EcComp::new("C", 0.0, 2.0, 25.0);
    c.comp_id = cid;
    c.project = p.to_str().unwrap().to_string();
    let mut ro = EcHost::open(&p, &c, true).unwrap();
    let ctx = egui::Context::default();
    assert!(super::intercept(&mut ro, &ctx, "layer.newSolid", &json!({})).is_none());
    assert!(super::shown(&ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn layer_dialog_junk_never_panics() {
    let (dir, p, cid) = sample("layer-junk");
    let (mut u, card) = app_with_timeline(&p, cid);
    let ctx = u.ctx.clone();
    let ids = [
        "app.solidSettings",
        "layer.newSolid",
        "layer.newLight",
        "layer.newCamera",
        "layer.settings",
        "layer.cameraSettings",
        "layer.lightSettings",
        "layer.timeStretch",
        "layer.style.options",
        "layer.style.dropShadow",
        "layer.style.nope",
    ];
    for id in ids {
        for params in [json!(null), json!([1]), json!({"layer": "x"}), json!({"layer": 999_999}), json!({"layer": -1}), json!({"style": "\u{0}"})] {
            let _ = super::intercept(host(&mut u), &ctx, id, &params);
            u.frame(vec![]);
            escape(&mut u);
            u.frame(vec![]);
        }
    }
    host(&mut u).session_mut().state.selected_layers.clear();
    for which in [Which::Solid, Which::Light, Which::Camera, Which::LayerSettings, Which::TimeStretch, Which::LayerStyleOptions] {
        let _ = super::request(&mut u.app, Some(NodeId(999_999)), which);
        let _ = super::request(&mut u.app, Some(card), which);
        let _ = super::request(&mut u.app, None, which);
        u.frame(vec![]);
        escape(&mut u);
    }
    // The Timeline gone while a layer dialog is up: it closes quietly.
    let _ = invoke(&mut u, "layer.newSolid", json!({}));
    u.app.effectcraft.panel = None;
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

// ---- Keyframe dialogs (Keyframe Velocity, Keyframe Interpolation, Edit Value).

/// Two opacity keys on the sample's solid, the first selected. Returns (layer, prop uid).
fn two_keys(u: &mut Ui) -> (u64, u64) {
    let l = host(u).session().active_comp().unwrap().layers[0].id.0;
    let uid = host(u).session().active_comp().unwrap().layer(LayerId(l)).unwrap().props.prop("transform/opacity").unwrap().uid;
    for (t, v) in [(0.0, 0.0), (1.0, 100.0)] {
        invoke(u, "prop.addKey", json!({"layer": l, "prop": uid, "time": t, "value": v})).unwrap();
    }
    invoke(u, "keys.select", json!({"keys": [{"layer": l, "prop": uid, "time": 0.0}]})).unwrap();
    (l, uid)
}

fn first_key_info(u: &mut Ui) -> Value {
    host(u).session_mut().execute("keys.info", json!({})).unwrap()[0].clone()
}

#[test]
fn keyframe_velocity_opens_from_the_keyframe_menu_and_ok_sets_it_in_one_undo_step() {
    let (dir, p, cid) = sample("velocity");
    let (mut u, _) = app_with_timeline(&p, cid);
    two_keys(&mut u);
    let rev = host(&mut u).session().revision;
    // The keyframe menu's Keyframe Velocity… runs keys.velocity without settings: the dialog.
    let r = invoke(&mut u, "keys.velocity", json!({})).unwrap();
    assert_eq!(r["dialog"], json!("keys.velocity"));
    assert_eq!(host(&mut u).session().revision, rev, "nothing changes until OK");
    u.frame(vec![]);
    let Hosted::Velocity(mut d) = layer_dialog(&u) else { panic!("Keyframe Velocity") };
    assert!(d.has_out && !d.has_in, "the first key has only an outgoing side");
    d.out_inf = vec![75.0];
    let steps = undo_steps(&mut u);
    ok_with(&mut u, Hosted::Velocity(d));
    assert_eq!(undo_steps(&mut u), steps + 1, "one undo step");
    let k = first_key_info(&mut u);
    assert!((k["outEase"][0]["influence"].as_f64().unwrap() - 75.0).abs() < 1e-6, "{k}");
    // Cancel changes nothing.
    invoke(&mut u, "keys.velocity", json!({})).unwrap();
    let rev = host(&mut u).session().revision;
    u.frame(vec![]);
    escape(&mut u);
    u.frame(vec![]);
    assert!(shown(&u.ctx).is_none());
    assert_eq!(host(&mut u).session().revision, rev);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn keyframe_interpolation_opens_from_its_key_and_ok_changes_the_keys() {
    let (dir, p, cid) = sample("interp");
    let (mut u, _) = app_with_timeline(&p, cid);
    two_keys(&mut u);
    let mut tried = 0;
    for id in ["keys.interpolation", "keys.velocity"] {
        // ⌥⌘K / ⇧⌘K, or Elias's own keys for them.
        let Some((mods, key)) = host(&mut u).session().shortcuts().shortcut_of(id, &json!({})).and_then(effectcraft_ui_egui::menus::parse_shortcut)
        else {
            continue;
        };
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events: vec![egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let _ = host(&mut u).keys(ui.ctx());
        })
        .textures_delta
        .clear();
        assert!(shown(&ctx).is_some(), "{id}: its dialog");
        tried += 1;
    }
    assert!(tried > 0, "no key bound at all");
    // From the card's entry point: Hold for the selected key, one undo step.
    super::request(&mut u.app, None, Which::KeyInterpolation).unwrap();
    let Hosted::Interpolation(mut d) = layer_dialog(&u) else { panic!("Keyframe Interpolation") };
    assert_eq!(d.count, 1);
    d.temporal = 5;
    let steps = undo_steps(&mut u);
    ok_with(&mut u, Hosted::Interpolation(d));
    assert_eq!(undo_steps(&mut u), steps + 1);
    let k = first_key_info(&mut u);
    assert!(k.to_string().contains("hold"), "{k}");
    // Current Settings everywhere: OK changes nothing.
    super::request(&mut u.app, None, Which::KeyInterpolation).unwrap();
    let rev = host(&mut u).session().revision;
    let Hosted::Interpolation(d) = layer_dialog(&u) else { panic!() };
    ok_with(&mut u, Hosted::Interpolation(d));
    assert_eq!(host(&mut u).session().revision, rev);
    // No keys selected: an error, no dialog.
    host(&mut u).session_mut().state.selected_keys.clear();
    assert!(super::request(&mut u.app, None, Which::KeyVelocity).is_err());
    assert!(shown(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn edit_value_on_a_keyframe_is_effectcrafts_form_and_with_a_value_runs_at_once() {
    let (dir, p, cid) = sample("edit-value");
    let (mut u, _) = app_with_timeline(&p, cid);
    let (l, uid) = two_keys(&mut u);
    invoke(&mut u, "keys.set", json!({"layer": l, "prop": uid, "time": 1.0})).unwrap();
    assert!(matches!(layer_dialog(&u), Hosted::Form(_)), "the value form");
    super::set_shown(&u.ctx, None);
    // With a value (the graph's drags, scripts): no dialog.
    invoke(&mut u, "keys.set", json!({"layer": l, "prop": uid, "time": 1.0, "value": 40.0})).unwrap();
    let _ = invoke(&mut u, "keys.velocity", json!({"outInfluence": [50.0]}));
    assert!(shown(&u.ctx).is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn keyframe_dialog_junk_never_panics() {
    let (dir, p, cid) = sample("key-junk");
    let (mut u, card) = app_with_timeline(&p, cid);
    let ctx = u.ctx.clone();
    for id in ["keys.velocity", "keys.interpolation", "keys.set", "prop.set", "layer.sequence"] {
        for params in [json!(null), json!([1]), json!({"layer": "x", "prop": -1, "time": f64::MAX}), json!({"layer": 999_999, "prop": 1, "time": 0})]
        {
            let _ = super::intercept(host(&mut u), &ctx, id, &params);
            u.frame(vec![]);
            escape(&mut u);
            u.frame(vec![]);
        }
    }
    two_keys(&mut u);
    for which in [Which::KeyVelocity, Which::KeyInterpolation] {
        let _ = super::request(&mut u.app, Some(NodeId(999_999)), which);
        let _ = super::request(&mut u.app, Some(card), which);
        u.frame(vec![]);
        escape(&mut u);
    }
    // A draft with lists shorter than its dimensions draws without indexing past them.
    let d = effectcraft_ui_egui::panels::key_dialogs::VelocityDraft { dims: 3, in_speed: vec![1.0], ..Default::default() };
    super::set_shown(&ctx, Some(super::Shown::layer_for_tests(host(&mut u).path(), Hosted::Velocity(d))));
    u.frame(vec![]);
    escape(&mut u);
    let _ = std::fs::remove_dir_all(dir);
}

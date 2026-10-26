//! EffectCraft's Character / Paragraph, Markers and Motion Library panels as tabs beside a card's
//! Timeline: each draws headless against the Timeline's host, a text change and a marker land as
//! in EffectCraft (one undo step each), the Marker dialog edits through the host, the Motion
//! Library's commands and its plug's requests reach the Timeline's session, junk never panics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use effectcraft_ui_egui::panels::markers_ui::MarkerRef;
use effectcraft_ui_egui::panels::timeline::{DialogRequest, TimelineHost};
use egui::{Event, pos2};
use figcraft_doc::NodeId;
use serde_json::{Value, json};

use super::super::{Side, set_side};
use crate::effectcraft_panel::EcHost;
use crate::layers_drag_tests::Ui;

/// A fresh folder with EffectCraft's sample project (`C`: 64×36, 2 s, 25 fps, one red solid).
fn sample(name: &str) -> (PathBuf, PathBuf, u64) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-tabs-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    (d, p, cid)
}

fn host(p: &Path, cid: u64) -> EcHost {
    let mut c = figcraft_doc::EcComp::new("C", 0.0, 2.0, 25.0);
    c.comp_id = cid;
    c.project = p.to_str().unwrap().to_string();
    EcHost::open(p, &c, false).unwrap()
}

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 600.0);

fn input(events: Vec<Event>) -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)), events, ..Default::default() }
}

/// One frame of the Timeline with its column (the tabs at x 1000…1400).
fn show(ctx: &egui::Context, h: &mut EcHost, events: Vec<Event>) {
    let _ = ctx.run_ui(input(events), |ui| {
        let tl = egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(1000.0, SCREEN.y));
        let fx = egui::Rect::from_min_size(pos2(1000.0, 0.0), egui::vec2(400.0, SCREEN.y));
        h.show(ui, tl, Some(fx));
    });
}

/// Click automation target `id` (drawn last frame).
fn click(ctx: &egui::Context, h: &mut EcHost, id: &str) {
    let [x, y, w, hh] = h.auto().find_current(id).map(|e| e.rect).unwrap_or_else(|| panic!("{id} drew"));
    let at = pos2(x + w / 2.0, y + hh / 2.0);
    show(
        ctx,
        h,
        vec![
            Event::PointerMoved(at),
            Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
        ],
    );
    show(ctx, h, vec![Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }]);
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    effectcraft_ui_egui::theme::install_panel_fonts(&ctx);
    ctx
}

fn undo_len(h: &EcHost) -> usize {
    h.session().history.undo.len()
}

fn comp_markers(h: &EcHost) -> Vec<String> {
    h.session().active_comp().map_or_else(Vec::new, |c| c.markers.iter().map(|m| m.comment.clone()).collect())
}

#[test]
fn each_tab_draws_effectcrafts_panel_headless() {
    let (dir, p, cid) = sample("draw");
    let mut h = host(&p, cid);
    let ctx = ctx();
    for (side, panel, drew) in [
        (Side::Text, effectcraft_ui_egui::dock::PanelKind::Character, "character.font"),
        (Side::Markers, effectcraft_ui_egui::dock::PanelKind::Markers, "figcraft.markers.addComp"),
        (Side::Library, effectcraft_ui_egui::dock::PanelKind::MotionLibrary, "motionLibrary."),
    ] {
        set_side(&ctx, &h, side);
        show(&ctx, &mut h, vec![]);
        show(&ctx, &mut h, vec![]);
        assert!(!h.auto().query_current(drew).is_empty(), "{side:?}: {drew} drew");
        assert_eq!(super::super::side_panel(&ctx, &h), panel, "a click there gives EffectCraft's {panel:?} keys");
    }
    set_side(&ctx, &h, Side::Text);
    show(&ctx, &mut h, vec![]);
    assert!(h.auto().find_current("paragraph.scroll").is_some() || !h.auto().query_current("paragraph.").is_empty(), "Paragraph under Character");
    for key in ["effectControls", "project", "text", "markers", "library"] {
        assert!(h.auto().find_current(&format!("figcraft.side.{key}")).is_some(), "the {key} tab");
    }
    // A narrow column still shows every tab (short labels).
    let _ = ctx.run_ui(input(vec![]), |ui| {
        let tl = egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(1000.0, SCREEN.y));
        let fx = egui::Rect::from_min_size(pos2(1000.0, 0.0), egui::vec2(230.0, SCREEN.y));
        h.show(ui, tl, Some(fx));
    });
    assert!(h.auto().find_current("figcraft.side.library").is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_character_change_on_the_selected_text_layer_is_one_undo_step() {
    let (dir, p, cid) = sample("character");
    let mut h = host(&p, cid);
    let ctx = ctx();
    let r = h.invoke(&ctx, "layer.newText", json!({"text": "Hello"})).unwrap();
    let layer = r["layer"].as_u64().unwrap();
    h.invoke(&ctx, "layer.select", json!({"layers": [layer]})).unwrap();
    set_side(&ctx, &h, Side::Text);
    show(&ctx, &mut h, vec![]);
    let before = undo_len(&h);
    let bold = |h: &EcHost| effectcraft_ui_egui::panels::text_panels::text_target(h).map(|t| t.doc.faux_bold);
    assert_eq!(bold(&h), Some(false));
    click(&ctx, &mut h, "character.fauxBold");
    assert_eq!(bold(&h), Some(true), "Faux Bold landed on the layer");
    assert_eq!(undo_len(&h), before + 1, "one undo step");
    assert!(h.is_dirty(), "an unsaved edit of the card's project");
    h.invoke(&ctx, "edit.undo", json!({})).unwrap();
    assert_eq!(bold(&h), Some(false), "undone");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn add_comp_and_layer_markers_from_the_markers_tab() {
    let (dir, p, cid) = sample("markers");
    let mut h = host(&p, cid);
    let ctx = ctx();
    set_side(&ctx, &h, Side::Markers);
    show(&ctx, &mut h, vec![]);
    let before = undo_len(&h);
    click(&ctx, &mut h, "figcraft.markers.addComp");
    assert_eq!(comp_markers(&h).len(), 1, "a comp marker at the current time");
    assert_eq!(undo_len(&h), before + 1, "one undo step");
    show(&ctx, &mut h, vec![]);
    assert!(h.auto().find_current("markers.0").is_some(), "EffectCraft's list shows it");
    // Add Layer Marker needs a selected layer; with one, the layer gets it.
    h.session_mut().state.selected_layers.clear();
    click(&ctx, &mut h, "figcraft.markers.addLayer");
    assert!(!h.ui().status.is_empty(), "says to select layers");
    let solid = h.session().active_comp().unwrap().layers.iter().next().unwrap().id.0;
    h.invoke(&ctx, "layer.select", json!({"layers": [solid]})).unwrap();
    click(&ctx, &mut h, "figcraft.markers.addLayer");
    let layer_markers = h.session().active_comp().unwrap().layers.iter().next().unwrap().markers.len();
    assert_eq!(layer_markers, 1, "the selected layer got a marker");
    let _ = std::fs::remove_dir_all(dir);
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

/// The Marker dialog (one test: it is one dialog per FigCraft process).
#[test]
fn the_marker_dialog_edits_and_deletes_through_the_timeline() {
    let (dir, p, cid) = sample("marker-dialog");
    let (mut u, _card) = app_with_timeline(&p, cid);
    let ctx = u.ctx.clone();
    let host = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
    host.invoke(&ctx, "comp.addMarker", json!({"comment": "Beat", "time": 0.5})).unwrap();
    // A double-click on the marker (Timeline or Markers tab) asks the host for the dialog.
    assert!(host.request_dialog(DialogRequest::Marker(MarkerRef { layer: None, index: 7 })).is_err(), "no such marker");
    host.request_dialog(DialogRequest::Marker(MarkerRef { layer: None, index: 0 })).unwrap();
    let mut shown = super::marker_shown().expect("the dialog shows");
    assert_eq!(shown.draft.comment, "Beat");
    assert_eq!(shown.draft.title, "Composition Marker");
    let before = undo_len(host);
    shown.draft.comment = "Hit".into();
    super::set_marker(Some(shown));
    u.frame(vec![]);
    u.frame(vec![Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }]);
    assert!(super::marker_shown().is_none(), "OK closed it");
    let host = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
    assert!(comp_markers(host).first().is_some_and(|c| c.starts_with("Hit")), "{:?}", comp_markers(host));
    assert_eq!(undo_len(host), before + 1, "one undo step");
    // Delete (Cmd+Backspace) removes it.
    host.request_dialog(DialogRequest::Marker(MarkerRef { layer: None, index: 0 })).unwrap();
    u.frame(vec![]);
    u.frame(vec![Event::Key { key: egui::Key::Backspace, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND }]);
    let host = &u.app.effectcraft.panel.as_ref().unwrap().host;
    assert!(comp_markers(host).is_empty(), "deleted");
    // Escape changes nothing.
    let host = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
    host.invoke(&ctx, "comp.addMarker", json!({"comment": "Keep"})).unwrap();
    host.request_dialog(DialogRequest::Marker(MarkerRef { layer: None, index: 0 })).unwrap();
    let rev = host.session().revision;
    u.frame(vec![]);
    u.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert!(super::marker_shown().is_none());
    assert_eq!(u.app.effectcraft.panel.as_ref().unwrap().host.session().revision, rev, "cancel changed nothing");
    super::set_marker(None);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn motion_library_commands_and_its_plug_reach_the_timelines_session() {
    let (dir, p, cid) = sample("library");
    let (mut u, _card) = app_with_timeline(&p, cid);
    let ctx = u.ctx.clone();
    // The panel's own commands run on the host (EffectCraft's menus run them with its app).
    let host = &mut u.app.effectcraft.panel.as_mut().unwrap().host;
    host.invoke(&ctx, "motionLibrary.view", json!({"query": "ease"})).unwrap();
    assert_eq!(host.ui().motion_library.query, "ease");
    assert_eq!(host.library_port(), super::bridge_port(), "the plug's port is FigCraft's");
    // The plug's engine.execute lands in the Timeline's session.
    let port = super::bridge_port().expect("the plug's port opened");
    let plug = std::thread::spawn(move || {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut answers = Vec::new();
        let mut r = BufReader::new(s.try_clone().unwrap());
        for req in [
            json!({"id": 1, "method": "engine.execute", "params": {"command": "comp.addMarker", "params": {"comment": "plug"}}}),
            json!({"id": 2, "method": "app.quit", "params": {}}),
            json!({"id": 3, "method": "engine.execute", "params": {"params": {}}}),
        ] {
            writeln!(s, "{req}").unwrap();
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            answers.push(serde_json::from_str::<Value>(&line).unwrap());
        }
        writeln!(s, "not json").unwrap();
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        answers.push(serde_json::from_str::<Value>(&line).unwrap());
        answers
    });
    let mut answers = None;
    for _ in 0..2000 {
        u.frame(vec![]);
        if plug.is_finished() {
            answers = Some(plug.join().unwrap());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let answers = answers.expect("the plug got its answers");
    assert_eq!(answers[0]["ok"], json!(true), "{answers:?}");
    assert_eq!(answers[1]["ok"], json!(false), "only engine.execute");
    assert_eq!(answers[2]["ok"], json!(false), "a command is needed");
    assert_eq!(answers[3]["ok"], json!(false), "junk answered, never a panic");
    let host = &u.app.effectcraft.panel.as_ref().unwrap().host;
    assert_eq!(comp_markers(host), vec!["plug".to_string()], "the plug's edit is the Timeline's");
    assert!(host.is_dirty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn junk_never_panics() {
    let (dir, p, cid) = sample("junk");
    let mut h = host(&p, cid);
    let ctx = ctx();
    let junk =
        [json!(null), json!(1), json!("x"), json!([1, {}]), json!({"op": 5, "id": [], "query": {}}), json!({"id": "../../etc", "op": "delete"})];
    for id in [
        "motionLibrary.view",
        "motionLibrary.apply",
        "motionLibrary.copyName",
        "motionLibrary.card",
        "motionLibrary.folder",
        "motionLibrary.group",
        "motionLibrary.curve",
        "motionLibrary.tool",
        "motionLibrary.nothing",
        "markers.set",
        "markers.delete",
        "layer.addMarker",
    ] {
        for j in &junk {
            let _ = h.invoke(&ctx, id, j.clone());
            let _ = super::run_job(&mut h, &ctx, id, j.clone());
        }
    }
    assert!(super::intercept(&mut h, &ctx, "layer.select", &json!({})).is_none(), "not the library's");
    for m in [MarkerRef { layer: None, index: usize::MAX }, MarkerRef { layer: Some(u64::MAX), index: 0 }] {
        assert!(h.request_dialog(DialogRequest::Marker(m)).is_err());
    }
    // Every tab draws in a tiny or empty column.
    for side in [Side::Text, Side::Markers, Side::Library] {
        set_side(&ctx, &h, side);
        for w in [0.0, 3.0, 40.0] {
            let _ = ctx.run_ui(input(vec![]), |ui| {
                let tl = egui::Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(1000.0, 30.0));
                let fx = egui::Rect::from_min_size(pos2(1000.0, 0.0), egui::vec2(w, 30.0));
                h.show(ui, tl, Some(fx));
            });
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

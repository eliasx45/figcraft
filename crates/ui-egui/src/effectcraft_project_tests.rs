//! New material and new comps without going to EffectCraft: files dropped on a card or its
//! Timeline become layers where (and when) they land, the Project tab is EffectCraft's own panel
//! answering through FigCraft, new comps become cards with their Timeline open, junk never panics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use egui::{Event, pos2};
use figcraft_doc::NodeId;
use serde_json::json;

use super::{Ask, DropTo, FileDrop, ProjectHost, Side, Target};
use crate::effectcraft_panel::EcHost;
use crate::layers_drag_tests::Ui;

/// A fresh folder with EffectCraft's sample project (`C`: 64×36, 2 s, 25 fps, one red solid).
fn sample(name: &str) -> (PathBuf, PathBuf, u64) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-project-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    (d, p, cid)
}

/// A synthetic still to import (a green 16×8 PNG).
fn still(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(16, 8, image::Rgba([0, 200, 80, 255])).save(&path).unwrap();
    path
}

/// The app with the sample comp placed as a card at 0, 0 (wide enough to hit), its playhead at
/// `time`.
fn app_with_card(p: &Path, cid: u64, time: f64) -> (Ui, NodeId) {
    let mut s = figcraft_engine::Session::new();
    let r = s
        .execute(
            "effectcraft.place",
            &json!({"comp": "C", "compId": cid, "project": p.to_str().unwrap(), "start": 0, "end": 2, "fps": 25, "x": 0, "y": 0, "cardWidth": 1400}),
        )
        .unwrap();
    let card = figcraft_engine::params::id_of(&r["id"]).unwrap();
    s.execute("effectcraft.seek", &json!({"id": card, "time": time})).unwrap();
    (Ui::new(s), card)
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

/// Drop `path` from the Finder at screen point `at` (one frame).
fn drop_at(u: &mut Ui, path: &Path, at: egui::Pos2) {
    let file: egui::DroppedFileHandle = std::sync::Arc::new(Dropped(path.to_path_buf()));
    u.frame_raw(egui::RawInput { events: vec![Event::PointerMoved(at)], dropped_files: vec![file], ..Default::default() });
}

fn comp_layers(h: &EcHost) -> Vec<(String, f64)> {
    h.session().active_comp().map_or_else(Vec::new, |c| c.layers.iter().map(|l| (l.name.clone(), l.in_point.seconds())).collect())
}

#[test]
fn a_file_dropped_on_a_card_becomes_a_layer_where_and_when_it_lands() {
    let (dir, p, cid) = sample("card-drop");
    let png = still(&dir, "leaf.png");
    let (mut u, card) = app_with_card(&p, cid, 1.0);
    let nodes = u.app.session.doc().unwrap().nodes().count();
    let r = crate::effectcraft_ui::screen_rect(&u.app, u.app.session.doc().unwrap(), card).unwrap();
    drop_at(&mut u, &png, r.center());
    u.frame(vec![]);
    let panel = u.app.effectcraft.panel.as_ref().expect("the card's Timeline opened");
    assert_eq!(panel.card, card);
    let layers = comp_layers(&panel.host);
    assert_eq!(layers.len(), 2, "the solid and the dropped still: {layers:?}");
    let (name, at) = layers[0].clone();
    assert_eq!(name, "leaf.png", "the new layer goes on top");
    assert!((at - 1.0).abs() < 0.05, "it starts at the card's time (1 s), not {at}");
    assert!(panel.host.session().project.items.values().any(|i| i.name == "leaf.png"), "imported into the card's project");
    assert!(panel.host.is_dirty(), "an unsaved edit of the card's project (⌘S saves it)");
    assert_eq!(u.app.session.doc().unwrap().nodes().count(), nodes, "nothing landed on FigCraft's canvas");
    assert!(u.app.ui.pending_opens.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn files_dropped_on_the_timeline_go_into_its_comp() {
    let (dir, p, cid) = sample("timeline-drop");
    let png = still(&dir, "dot.png");
    let (mut u, card) = app_with_card(&p, cid, 0.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    u.frame(vec![]);
    let (_, rects) = u.app.effectcraft.panel_rects.clone().expect("the Timeline drew");
    let timeline = rects[0];
    let n = comp_layers(&u.app.effectcraft.panel.as_ref().unwrap().host).len();
    // Over the lower rows (the layer outline, left of the time graph).
    drop_at(&mut u, &png, pos2(timeline.min.x + 40.0, timeline.max.y - 20.0));
    u.frame(vec![]);
    let h = &u.app.effectcraft.panel.as_ref().unwrap().host;
    assert_eq!(comp_layers(h).len(), n + 1, "a layer of the dropped still");
    assert!(comp_layers(h).iter().any(|(name, _)| name == "dot.png"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn the_project_tab_is_effectcrafts_panel_answering_through_figcraft() {
    let (dir, p, cid) = sample("project-tab");
    let mut c = figcraft_doc::EcComp::new("C", 0.0, 2.0, 25.0);
    c.comp_id = cid;
    c.project = p.to_str().unwrap().to_string();
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    effectcraft_ui_egui::theme::install_panel_fonts(&ctx);
    super::set_side(&ctx, &h, Side::Project);
    let input = || egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 400.0))), ..Default::default() };
    for _ in 0..2 {
        ctx.run_ui(input(), |ui| {
            let tl = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 400.0));
            let fx = egui::Rect::from_min_size(egui::pos2(1000.0, 0.0), egui::vec2(400.0, 400.0));
            h.show(ui, tl, Some(fx));
        })
        .textures_delta
        .clear();
    }
    assert!(h.auto().find_current("figcraft.side.project").is_some(), "the tabs drew");
    assert!(h.auto().find_current("project.newComp").is_some(), "EffectCraft's Project panel drew");
    assert!(!h.auto().query_current("project.item.").is_empty(), "with the project's items");
    assert_eq!(super::side_panel(&ctx, &h), effectcraft_ui_egui::dock::PanelKind::Project);
    // What EffectCraft's app does in its own windows comes back to FigCraft as asks.
    let mut ph = ProjectHost { host: &mut h, asked: Vec::new() };
    ph.invoke(&ctx, "comp.new", json!({})).unwrap();
    ph.invoke(&ctx, "comp.open", json!({"comp": cid})).unwrap();
    ph.invoke(&ctx, "file.import", json!({})).unwrap();
    assert!(ph.invoke(&ctx, "comp.open", json!({"comp": 999_999})).is_err());
    assert_eq!(ph.asked, vec![Ask::NewComp, Ask::Card(cid), Ask::Import]);
    // Everything else is an edit of the Timeline's session (undoable, unsaved).
    let folders = |h: &EcHost| h.session().project.items.values().filter(|i| i.is_folder()).count();
    let before = folders(ph.host);
    ph.invoke(&ctx, "project.newFolder", json!({})).unwrap();
    assert_eq!(folders(ph.host), before + 1);
    assert!(h.is_dirty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_new_comp_in_the_cards_project_becomes_a_card_with_its_timeline_open() {
    let (dir, p, cid) = sample("new-in-project");
    let (mut u, card) = app_with_card(&p, cid, 0.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    u.app.run("select.set", json!({"ids": [card]}));
    let r = u.app.run("effectcraft.newComp", json!({"name": "Lower third", "width": 1080, "height": 1920, "frameRate": 30, "duration": 4})).unwrap();
    assert_eq!(r["dialog"], json!(false));
    u.frame(vec![]);
    u.frame(vec![]);
    let doc = u.app.session.doc().unwrap();
    let new = doc.nodes().find(|n| n.effectcraft.as_deref().is_some_and(|c| c.comp == "Lower third")).expect("a card of the new comp");
    let c = new.effectcraft.as_deref().unwrap().clone();
    assert_eq!((c.width, c.height), (1080, 1920));
    assert_eq!(figcraft_ec_inproc::key(Path::new(&c.project)), figcraft_ec_inproc::key(&p), "in the card's project");
    let (b, b_new) = (doc.world_bounds(card).unwrap(), doc.world_bounds(new.id).unwrap());
    assert!(b_new.x0 > b.x1, "placed right of the card");
    let panel = u.app.effectcraft.panel.as_ref().unwrap();
    assert_eq!(panel.card, new.id, "the Timeline moved to the new card");
    assert!(panel.open);
    assert_eq!(panel.host.comp(), c.comp_id);
    assert!(panel.host.is_dirty(), "an unsaved edit until ⌘S");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_new_comp_without_a_card_is_a_new_project_next_to_the_file() {
    let d = std::env::temp_dir().join(format!("figcraft-ec-project-{}-new-file", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let mut u = Ui::new(figcraft_engine::Session::new());
    u.app.session.ds_mut().unwrap().path = Some(d.join("Board.figcraft").to_string_lossy().into_owned());
    // Unsized: EffectCraft's New Composition dialog opens first.
    let r = u.app.run("effectcraft.newComp", json!({})).unwrap();
    assert_eq!(r["dialog"], json!(true));
    u.frame(vec![]);
    assert!(super::dialog_open(&u.ctx), "the New Composition dialog shows");
    u.frame(vec![Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
    // Its OK, as the control channel would ask it: the settings given.
    u.app.run("effectcraft.newComp", json!({"name": "Intro", "width": 640, "height": 360, "frameRate": 25, "duration": 3}));
    u.frame(vec![]);
    let made = d.join("Board.ecproj");
    assert!(made.is_file(), "saved next to the FigCraft file");
    let doc = u.app.session.doc().unwrap();
    let card = doc.nodes().find(|n| n.effectcraft.as_deref().is_some_and(|c| c.comp == "Intro")).expect("a card of the new comp");
    let c = card.effectcraft.as_deref().unwrap();
    assert_eq!(figcraft_ec_inproc::key(Path::new(&c.project)), figcraft_ec_inproc::key(&made));
    let panel = u.app.effectcraft.panel.as_ref().expect("its Timeline opened");
    assert_eq!(panel.card, card.id);
    assert!(!panel.host.is_dirty(), "a new project is saved at once");
    // The next one takes the next free name.
    assert_eq!(super::new_project_path(&d.join("Board.figcraft")).unwrap(), d.join("Board 2.ecproj"));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn junk_never_panics() {
    let (dir, p, cid) = sample("junk");
    let (mut u, card) = app_with_card(&p, cid, 0.5);
    let ctx = u.ctx.clone();
    // Drops of files that aren't there, aren't media, or aren't paths at all.
    for paths in [vec![], vec!["/nope/x.png".to_string()], vec!["\u{0}".to_string()], vec![dir.to_string_lossy().into_owned()], vec!["".into()]] {
        for to in [DropTo::Panel, DropTo::Card(card), DropTo::Card(NodeId(999_999))] {
            for at in [pos2(f32::NAN, 1.0), pos2(1e30, -1e30), pos2(10.0, 10.0)] {
                let d = FileDrop { paths: paths.clone(), at, shift: true, alt: true, to, pass: 0 };
                let _ = super::drop_files(&mut u.app, &ctx, d);
            }
        }
    }
    assert!(super::drop_target(&u.app, pos2(f32::NAN, f32::INFINITY)).is_none());
    // New comps with junk settings and places.
    for (to, params) in [
        (Target::default(), json!(null)),
        (Target { project: Some(PathBuf::from("/nope/p.ecproj")), ..Target::default() }, json!({"width": -5})),
        (Target { project: Some(p.clone()), ..Target::default() }, json!({"width": 1e308, "height": "x", "frameRate": -1, "duration": [1]})),
        (Target { next_to: Some(PathBuf::from("/")), ..Target::default() }, json!([1, 2])),
        (
            Target { from: Some(NodeId(424_242)), place: serde_json::from_value(json!({"x": "NaN", "cardWidth": -1})).unwrap(), ..Target::default() },
            json!({}),
        ),
    ] {
        let _ = super::create(&mut u.app, &ctx, to, params);
    }
    for data in [json!(null), json!({"project": 5, "nextTo": [1], "from": "x", "place": 7, "comp": "y"}), json!({"project": "\u{0}"})] {
        super::request(&mut u.app, "effectcraft.newComp", &data);
        super::request(&mut u.app, "dialog.effectcraftNewComp", &data);
        let _ = Target::of(&data);
    }
    for path in ["", "/", "relative.figcraft", "\u{0}/x"] {
        let _ = super::new_project_path(Path::new(path));
    }
    assert!(super::show_comp(&mut u.app, 999_999).is_err(), "no such comp");
    for _ in 0..3 {
        u.frame(vec![]);
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A new card (a precomp, a new comp) never lands on a card already right of its source: it moves
/// past it and that card's strip (regression: "BG Precomp" covered "FigCraft New Comp").
#[test]
fn a_new_card_lands_in_a_free_spot() {
    let (_d, p, cid) = sample("free-spot");
    let (mut u, card) = app_with_card(&p, cid, 0.0);
    let other = u
        .app
        .session
        .execute(
            "effectcraft.place",
            &json!({"comp": "C", "compId": cid, "project": p.to_str().unwrap(), "start": 0, "end": 2, "fps": 25, "x": 1600, "y": 0, "cardWidth": 400}),
        )
        .unwrap();
    let other = figcraft_engine::params::id_of(&other["id"]).unwrap();
    let doc = u.app.session.doc().unwrap();
    let b = doc.world_bounds(other).unwrap();
    let x = super::free_x(doc, card, None, 1500.0, 0.0, 400.0, 225.0, 40.0);
    assert!(x >= b.x1, "{x} lands past the card in the way (ends at {})", b.x1);
    let clear = super::free_x(doc, card, None, 5000.0, 0.0, 400.0, 225.0, 40.0);
    assert_eq!(clear, 5000.0, "a free spot is kept");
}

#[test]
fn another_card_of_the_project_takes_the_timeline_with_its_unsaved_edits() {
    let (dir, p, cid) = sample("switch-cards");
    let (mut u, card) = app_with_card(&p, cid, 0.0);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    u.app.run("select.set", json!({"ids": [card]}));
    u.app.run("effectcraft.newComp", json!({"name": "Second", "width": 64, "height": 36, "frameRate": 25, "duration": 2})).unwrap();
    u.frame(vec![]);
    u.frame(vec![]);
    let second = u.app.effectcraft.panel.as_ref().unwrap().card;
    assert_ne!(second, card);
    assert!(u.app.effectcraft.panel.as_ref().unwrap().host.is_dirty());
    // Back to the first card: no "save first".
    assert!(u.app.effectcraft_open_panel(card));
    let panel = u.app.effectcraft.panel.as_ref().unwrap();
    assert_eq!(panel.card, card, "the Timeline moved back");
    assert_eq!(panel.host.comp(), cid);
    assert!(panel.host.is_dirty(), "the new comp is still there, unsaved");
    assert!(panel.host.session().project.items.values().any(|i| i.name == "Second"));
    let _ = std::fs::remove_dir_all(dir);
}

//! The docked EffectCraft Timeline: it draws headless against FigCraft's host, is read-only when
//! the EffectCraft app owns the project, saves round-trip, and never panics on junk.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use figcraft_doc::EcComp;
use serde_json::json;

use super::{EcHost, PanelAction, READ_ONLY};

fn sample(name: &str) -> (PathBuf, EcComp) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-panel-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    let mut c = EcComp::new("C", 0.0, 2.0, 25.0);
    c.comp_id = cid;
    c.project = p.to_str().unwrap().to_string();
    (p, c)
}

fn layers(h: &EcHost) -> usize {
    h.session().active_comp().map_or(0, |c| c.layers.len())
}

fn frame(ctx: &egui::Context, h: &mut EcHost, input: egui::RawInput) {
    let _ = ctx.run_ui(input, |ui| {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 300.0));
        h.show(ui, rect);
    });
}

fn input() -> egui::RawInput {
    egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 300.0))), ..Default::default() }
}

#[test]
fn the_timeline_draws_headless_in_figcrafts_host() {
    let (p, c) = sample("draw");
    let mut h = EcHost::open(&p, &c, false).unwrap();
    assert_eq!(h.session().active_comp_id().map(|i| i.0), Some(c.comp_id));
    let ctx = egui::Context::default();
    effectcraft_ui_egui::theme::install_panel_fonts(&ctx);
    frame(&ctx, &mut h, input());
    frame(&ctx, &mut h, input());
    assert!(h.auto().find_current("timeline.timecode").is_some(), "the header drew");
    assert!(!h.auto().query_current("timeline.layer.").is_empty(), "the layer rows drew");
}

#[test]
fn read_only_when_the_app_owns_the_project() {
    let (p, c) = sample("ro");
    let mut h = EcHost::open(&p, &c, true).unwrap();
    assert_eq!(h.read_only(), Some(READ_ONLY));
    let n = layers(&h);
    let ctx = egui::Context::default();
    let r = h.invoke(&ctx, "layer.newSolid", json!({"color": "#00ff00", "width": 8, "height": 8}));
    assert!(r.is_err());
    assert_eq!(layers(&h), n, "the edit was taken back");
    assert!(!h.is_dirty());
    assert!(h.save(false).is_err(), "a read-only panel never saves");
}

#[test]
fn edits_save_round_trip() {
    let (p, c) = sample("save");
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let n = layers(&h);
    let ctx = egui::Context::default();
    h.invoke(&ctx, "layer.newSolid", json!({"color": "#00ff00", "width": 8, "height": 8})).unwrap();
    assert!(h.is_dirty());
    let snap_rev = h.snapshot().revision;
    // The app opened the project meanwhile: no save, and the panel turns read-only.
    let mut other = EcHost::open(&p, &c, false).unwrap();
    other.invoke(&ctx, "layer.newSolid", json!({"color": "#0000ff", "width": 8, "height": 8})).unwrap();
    assert!(other.save(true).is_err());
    assert!(other.read_only().is_some());
    h.save(false).unwrap();
    assert!(!h.is_dirty());
    let again = EcHost::open(&p, &c, false).unwrap();
    assert_eq!(layers(&again), n + 1);
    assert_ne!(again.snapshot().revision, 0);
    assert_ne!(snap_rev, 0);
}

#[test]
fn junk_never_panics() {
    let (p, c) = sample("junk");
    let d = p.parent().unwrap();
    // No file, a junk file, a comp that isn't there.
    assert!(EcHost::open(&d.join("gone.ecproj"), &c, false).is_err());
    let junk = d.join("junk.ecproj");
    std::fs::write(&junk, b"{ nope").unwrap();
    assert!(EcHost::open(&junk, &c, false).is_err());
    let mut bad = c.clone();
    bad.comp_id = 987_654;
    assert!(EcHost::open(&p, &bad, false).is_err());
    let mut bad = c.clone();
    bad.comp_id = 0;
    bad.comp = "Nope".into();
    assert!(EcHost::open(&p, &bad, false).is_err());
    // Junk commands and keys.
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    for (id, params) in [("no.such", json!({})), ("layer.newSolid", json!(null)), ("prop.set", json!([1, 2])), ("", json!("x"))] {
        assert!(h.invoke(&ctx, id, params).is_err(), "{id}");
    }
    let mut keys = input();
    keys.events = vec![
        egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Key { key: egui::Key::S, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
        egui::Event::Key { key: egui::Key::F13, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
    ];
    let mut actions = vec![];
    let _ = ctx.run_ui(keys, |_| actions = h.keys(&ctx));
    assert_eq!(actions.get(..2), Some(&[PanelAction::Play, PanelAction::Save][..]));
}

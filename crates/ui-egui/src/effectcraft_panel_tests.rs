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

fn key(k: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers }
}

/// ⌘Z with the panel focused undoes EffectCraft's edit and is taken (FigCraft's undo never sees
/// it); ⇧⌘Z redoes it.
#[test]
fn undo_stays_in_effectcraft_while_the_panel_has_focus() {
    let (p, c) = sample("undo");
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    let n = layers(&h);
    h.invoke(&ctx, "layer.newSolid", json!({"color": "#00ff00", "width": 8, "height": 8})).unwrap();
    assert_eq!(layers(&h), n + 1);
    let mut undo = input();
    undo.events = vec![key(egui::Key::Z, egui::Modifiers::COMMAND)];
    let mut left = 1;
    let _ = ctx.run_ui(undo, |_| {
        let _ = h.keys(&ctx);
        left = ctx.input(|i| i.events.iter().filter(|e| matches!(e, egui::Event::Key { key: egui::Key::Z, .. })).count());
    });
    assert_eq!(layers(&h), n, "EffectCraft's undo ran");
    assert_eq!(left, 0, "the key was taken from FigCraft");
    let mut redo = input();
    redo.events = vec![key(egui::Key::Z, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)];
    let _ = ctx.run_ui(redo, |_| {
        let _ = h.keys(&ctx);
    });
    assert_eq!(layers(&h), n + 1, "and its redo");
}

/// The Graph Editor in the docked panel: Shift+F3 (the panel focused) opens it, it draws the
/// selected layer's keys, F fits its graphs; the switch is the panel's state, not an edit (no
/// unsaved change, no card re-render), and works read-only too.
#[test]
fn the_graph_editor_opens_and_draws_in_the_docked_panel() {
    use effectcraft_ui_egui::panels::timeline::hosted;
    let (p, c) = sample("graph");
    let mut h = EcHost::open(&p, &c, false).unwrap();
    assert!(hosted::installed(h.session()), "the Graph Editor's keys are bound in the panel's session");
    let ctx = egui::Context::default();
    effectcraft_ui_egui::theme::install_panel_fonts(&ctx);
    let comp = h.session().active_comp().unwrap().clone();
    let layer = comp.layers.iter().next().unwrap();
    let (lid, uid) = (layer.id.0, layer.props.prop("transform/opacity").unwrap().uid);
    for (t, v) in [(0.0, 0.0), (1.0, 100.0)] {
        h.invoke(&ctx, "prop.addKey", json!({"layer": lid, "prop": uid, "time": t, "value": v})).unwrap();
    }
    h.invoke(&ctx, "layer.select", json!({"layers": [lid]})).unwrap();
    let mut press = input();
    press.events = vec![key(egui::Key::F3, egui::Modifiers::SHIFT)];
    let mut actions = vec![];
    let _ = ctx.run_ui(press, |_| actions = h.keys(&ctx));
    assert!(actions.contains(&PanelAction::Ran("timeline.graphEditor".into())), "{actions:?}");
    assert!(h.ui().timeline.graph_editor, "Shift+F3 opened the Graph Editor");
    for _ in 0..3 {
        frame(&ctx, &mut h, input());
    }
    assert!(h.auto().find_current("timeline.graph").is_some(), "the graph plot drew");
    assert!(!h.auto().query_current("timeline.graph.key.").is_empty(), "the opacity keys drew in the graph");
    let mut press = input();
    press.events = vec![key(egui::Key::F, egui::Modifiers::NONE)];
    let _ = ctx.run_ui(press, |_| actions = h.keys(&ctx));
    assert!(actions.contains(&PanelAction::Ran("timeline.graph.fit".into())), "{actions:?}");
    frame(&ctx, &mut h, input());
    let rev = h.session().revision;
    h.invoke(&ctx, "timeline.graphEditor", json!({})).unwrap();
    assert!(!h.ui().timeline.graph_editor);
    assert_eq!(h.session().revision, rev, "the switch is no edit");
    assert!(h.invoke(&ctx, "timeline.graph.fit", json!({})).is_err(), "nothing to fit while it is closed");
    // Read-only: the Graph Editor still opens (it only shows the keys).
    let mut ro = EcHost::open(&p, &c, true).unwrap();
    ro.invoke(&ctx, "timeline.graphEditor", json!({"value": true})).unwrap();
    assert!(ro.ui().timeline.graph_editor);
    assert!(!ro.is_dirty());
}

/// EffectCraft's Timeline keys that its app runs itself work in the docked panel too: P reveals
/// Position (taken from FigCraft's Pen), Collapse All clears.
#[test]
fn timeline_reveal_keys_work_in_the_docked_panel() {
    let (p, c) = sample("reveal");
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    let mut press = input();
    press.events = vec![key(egui::Key::P, egui::Modifiers::NONE)];
    let mut actions = vec![];
    let mut left = 1;
    let _ = ctx.run_ui(press, |_| {
        actions = h.keys(&ctx);
        left = ctx.input(|i| i.events.iter().filter(|e| matches!(e, egui::Event::Key { key: egui::Key::P, .. })).count());
    });
    assert!(actions.contains(&PanelAction::Ran("timeline.reveal.position".into())), "{actions:?}");
    assert_eq!(left, 0, "FigCraft's P (Pen) never saw it");
    assert_eq!(h.ui().timeline.reveal, ["position"]);
    h.invoke(&ctx, "timeline.collapseAll", json!({})).unwrap();
    assert!(h.ui().timeline.reveal.is_empty());
}

/// An edit while a precomp row expanded in the Timeline has the focus never lands in the
/// parent comp: here the focused instance is gone, so the edit is refused.
#[test]
fn edits_of_an_expanded_precomp_never_reach_the_parent() {
    let (p, c) = sample("inline");
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    let n = layers(&h);
    let rev = h.session().revision;
    h.ui_mut().timeline.inline_focus = Some(vec![c.comp_id, 987_654]);
    assert!(h.invoke(&ctx, "layer.newSolid", json!({"color": "#00ff00", "width": 8, "height": 8})).is_err());
    assert!(h.invoke(&ctx, "keys.easyEase", json!({})).is_err());
    assert_eq!(layers(&h), n);
    assert_eq!(h.session().revision, rev, "the parent comp is untouched");
    assert_eq!(h.session().active_comp_id().map(|i| i.0), Some(c.comp_id));
}

/// Speed curves in the property rows are on in the docked Timeline (footer ∿).
#[test]
fn speed_curves_show_in_the_docked_rows_by_default() {
    let (p, c) = sample("speed");
    let h = EcHost::open(&p, &c, false).unwrap();
    assert!(h.ui().timeline.speed_rows);
}

/// The Timeline's time is the card's: set and read back in comp seconds, snapped to frames and
/// kept inside the comp; junk times are ignored.
#[test]
fn the_timeline_time_follows_the_card() {
    let (p, c) = sample("time");
    let mut h = EcHost::open(&p, &c, false).unwrap();
    h.set_time(1.0);
    assert!((h.time() - 1.0).abs() < 1e-6);
    h.set_time(1.013);
    assert!((h.time() - 1.0).abs() < 1e-6, "snapped to 25 fps: {}", h.time());
    h.set_time(99.0);
    assert!(h.time() < 2.0, "kept in the 2 s comp");
    let before = h.time();
    h.set_time(f64::NAN);
    assert_eq!(h.time(), before);
    let rev = h.session().revision;
    h.set_time(0.5);
    assert_eq!(h.session().revision, rev, "moving the time is no edit (no re-render)");
}

/// Whose pointer it is: over the docked panel or pressed in it and still held, the panel's;
/// a press that began on the canvas keeps its release over the panel; no panel, the canvas's.
#[test]
fn the_panel_owns_the_pointer_over_it() {
    use crate::effectcraft_ui::panel_has_pointer;
    let panel = Some(egui::Rect::from_min_max(egui::pos2(0.0, 600.0), egui::pos2(1000.0, 900.0)));
    let (inside, canvas) = (Some(egui::pos2(500.0, 800.0)), Some(egui::pos2(500.0, 300.0)));
    assert!(panel_has_pointer(panel, inside, None, false), "hovering a Timeline row");
    assert!(panel_has_pointer(panel, canvas, inside, false), "dragged out of the Timeline onto the canvas");
    assert!(!panel_has_pointer(panel, inside, canvas, true), "a canvas drag over the panel stays the canvas's");
    assert!(!panel_has_pointer(panel, canvas, None, false), "over the canvas");
    assert!(!panel_has_pointer(None, inside, inside, false), "no panel drawn this frame");
}

/// Regression: a click on a layer row in the docked Timeline zoomed the canvas out (162 % →
/// 25 %) and played the card. Clicks, drags, scroll and pinch over the panel reach only the
/// panel, which takes the keyboard.
#[test]
fn pointer_input_over_the_panel_never_reaches_the_canvas() {
    use egui::{Event, PointerButton, pos2, vec2};
    let (_p, c) = sample("route");
    let mut s = figcraft_engine::Session::new();
    s.execute(
        "effectcraft.place",
        &json!({"comp": c.comp, "compId": c.comp_id, "project": c.project, "start": 0, "end": 2, "fps": 25, "x": 0, "y": 0, "cardWidth": 1400}),
    )
    .unwrap();
    let card = s.doc().unwrap().nodes().find(|n| n.effectcraft.is_some()).unwrap().id;
    let mut u = crate::layers_drag_tests::Ui::new(s);
    assert!(u.app.effectcraft_open_panel(card));
    u.frame(vec![]);
    u.frame(vec![]);
    let (_, rect) = u.app.effectcraft.panel_rect.expect("the panel drew");
    assert!(!u.app.canvas.rect.intersects(rect.shrink(1.0)), "the canvas is above the panel");
    // Click away first: the panel gives up the keyboard.
    let canvas_spot = pos2(u.app.canvas.rect.center().x, u.app.canvas.rect.max.y - 20.0);
    u.button(canvas_spot, true);
    u.button(canvas_spot, false);
    assert!(!u.app.effectcraft.panel.as_ref().unwrap().focused);
    for _ in 0..40 {
        u.frame(vec![]);
    }
    let state = |u: &crate::layers_drag_tests::Ui| {
        let ds = u.app.session.ds().unwrap();
        (ds.view.zoom, ds.view.cx, ds.view.cy, ds.selection.clone(), u.app.session.effectcraft.playing())
    };
    let before = state(&u);
    let row = pos2(rect.center().x, rect.max.y - 60.0);
    // A click (press and release in one frame, as the control channel sends it) and a slow one.
    u.frame(vec![
        Event::PointerMoved(row),
        Event::PointerButton { pos: row, button: PointerButton::Primary, pressed: true, modifiers: Default::default() },
        Event::PointerButton { pos: row, button: PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ]);
    u.frame(vec![]);
    assert!(u.app.effectcraft.panel.as_ref().unwrap().focused, "the click gave the panel the keyboard");
    u.button(row, true);
    u.button(row, false);
    // Scroll, ⌘-scroll and a pinch over it.
    u.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -120.0),
        phase: egui::TouchPhase::Move,
        modifiers: Default::default(),
    }]);
    u.frame_with(
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -120.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::COMMAND,
        }],
        egui::Modifiers::COMMAND,
    );
    u.frame(vec![Event::Zoom(0.5)]);
    // A drag out of the Timeline, let go over the canvas.
    u.drag_to(row, canvas_spot);
    u.button(canvas_spot, false);
    u.frame(vec![]);
    assert_eq!(state(&u), before, "the canvas's view, selection, and card transport are untouched");
}

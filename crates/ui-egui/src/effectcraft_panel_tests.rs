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
    ctx.run_ui(input, |ui| {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 300.0));
        h.show(ui, rect, None);
    })
    .textures_delta
    .clear();
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
    // Junk commands and keys. (layer.newSolid with null params makes a default solid now, so
    // the junk is a comp that isn't there; night run 29 Oct.)
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    for (id, params) in [("no.such", json!({})), ("layer.newSolid", json!({"comp": 987_654})), ("prop.set", json!([1, 2])), ("", json!("x"))] {
        assert!(h.invoke(&ctx, id, params).is_err(), "{id}");
    }
    let mut keys = input();
    keys.events = vec![
        egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
        egui::Event::Key { key: egui::Key::S, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND },
        egui::Event::Key { key: egui::Key::F13, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE },
    ];
    let mut actions = vec![];
    ctx.run_ui(keys, |_| actions = h.keys(&ctx)).textures_delta.clear();
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
    ctx.run_ui(undo, |_| {
        let _ = h.keys(&ctx);
        left = ctx.input(|i| i.events.iter().filter(|e| matches!(e, egui::Event::Key { key: egui::Key::Z, .. })).count());
    })
    .textures_delta
    .clear();
    assert_eq!(layers(&h), n, "EffectCraft's undo ran");
    assert_eq!(left, 0, "the key was taken from FigCraft");
    let mut redo = input();
    redo.events = vec![key(egui::Key::Z, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)];
    ctx.run_ui(redo, |_| {
        let _ = h.keys(&ctx);
    })
    .textures_delta
    .clear();
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
    ctx.run_ui(press, |_| actions = h.keys(&ctx)).textures_delta.clear();
    assert!(actions.contains(&PanelAction::Ran("timeline.graphEditor".into())), "{actions:?}");
    assert!(h.ui().timeline.graph_editor, "Shift+F3 opened the Graph Editor");
    for _ in 0..3 {
        frame(&ctx, &mut h, input());
    }
    assert!(h.auto().find_current("timeline.graph").is_some(), "the graph plot drew");
    assert!(!h.auto().query_current("timeline.graph.key.").is_empty(), "the opacity keys drew in the graph");
    let mut press = input();
    press.events = vec![key(egui::Key::F, egui::Modifiers::NONE)];
    ctx.run_ui(press, |_| actions = h.keys(&ctx)).textures_delta.clear();
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
    ctx.run_ui(press, |_| {
        actions = h.keys(&ctx);
        left = ctx.input(|i| i.events.iter().filter(|e| matches!(e, egui::Event::Key { key: egui::Key::P, .. })).count());
    })
    .textures_delta
    .clear();
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

/// Whose pointer it is: over the Timeline or its Effect Controls, or pressed in them and still
/// held, the panel's; a press that began on the canvas keeps its release over the panel; no
/// panel, the canvas's.
#[test]
fn the_panel_owns_the_pointer_over_it() {
    use crate::effectcraft_ui::panel_has_pointer;
    let timeline = egui::Rect::from_min_max(egui::pos2(0.0, 600.0), egui::pos2(600.0, 900.0));
    let fx = egui::Rect::from_min_max(egui::pos2(610.0, 600.0), egui::pos2(900.0, 900.0));
    let panel = [timeline, fx];
    let (inside, canvas) = (Some(egui::pos2(500.0, 800.0)), Some(egui::pos2(500.0, 300.0)));
    assert!(panel_has_pointer(&panel, inside, None, false), "hovering a Timeline row");
    assert!(panel_has_pointer(&panel, Some(egui::pos2(700.0, 700.0)), None, false), "hovering Effect Controls");
    assert!(!panel_has_pointer(&panel, Some(egui::pos2(605.0, 700.0)), None, false), "the gap between them is canvas");
    assert!(panel_has_pointer(&panel, canvas, inside, false), "dragged out of the Timeline onto the canvas");
    assert!(!panel_has_pointer(&panel, inside, canvas, true), "a canvas drag over the panel stays the canvas's");
    assert!(!panel_has_pointer(&panel, canvas, None, false), "over the canvas");
    assert!(!panel_has_pointer(&[], inside, inside, false), "no panel drawn");
}

/// Regression: a click on a layer row in the Timeline zoomed the canvas out (162 % → 25 %) and
/// played the card. Clicks, drags and scroll over the Timeline on the canvas reach only it, which
/// takes the keyboard; ⌘-scroll and pinch there zoom the canvas (and nothing else).
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
    let (_, rects) = u.app.effectcraft.panel_rects.clone().expect("the Timeline drew");
    let rect = rects[0];
    let card_r = crate::effectcraft_ui::screen_rect(&u.app, u.app.session.doc().unwrap(), card).unwrap();
    assert!(rect.min.y > card_r.max.y && (rect.min.x - card_r.min.x).abs() < 1.0, "the Timeline hangs under its card");
    // Click away first (on the canvas beside the card): the panel gives up the keyboard.
    let canvas_spot = pos2(u.app.canvas.rect.min.x + 5.0, u.app.canvas.rect.min.y + 5.0);
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
    // A scroll over it.
    u.frame(vec![Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -120.0),
        phase: egui::TouchPhase::Move,
        modifiers: Default::default(),
    }]);
    // A drag out of the Timeline, let go over the canvas.
    u.drag_to(row, canvas_spot);
    u.button(canvas_spot, false);
    u.frame(vec![]);
    assert_eq!(state(&u), before, "the canvas's view, selection, and card transport are untouched");
    // A pinch over it zooms the canvas, and the Timeline follows its card.
    u.frame(vec![Event::PointerMoved(row), Event::Zoom(0.5)]);
    u.frame(vec![]);
    u.frame(vec![]);
    let zoom = u.app.session.ds().unwrap().view.zoom;
    assert!((zoom - before.0 * 0.5).abs() < 1e-6, "pinch zooms the canvas over the Timeline: {zoom}");
    let (_, rects) = u.app.effectcraft.panel_rects.clone().expect("still drawn");
    assert!((rects[0].width() - rect.width() * 0.5).abs() < 2.0 || rects[0].width() < rect.width(), "the Timeline shrinks with the canvas");
}

/// Moving a layer on the card is an EffectCraft edit: a press on the card keeps (or gives) the
/// panel the keyboard, so ⌘Z right after undoes it in EffectCraft (it went to FigCraft's undo).
#[test]
fn a_press_on_the_card_keeps_undo_in_effectcraft() {
    use egui::pos2;
    let (_p, c) = sample("card-focus");
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
    let canvas_spot = pos2(u.app.canvas.rect.min.x + 5.0, u.app.canvas.rect.min.y + 5.0);
    u.button(canvas_spot, true);
    u.button(canvas_spot, false);
    assert!(!u.app.effectcraft.panel.as_ref().unwrap().focused, "a click beside the card gives the keyboard back");
    let card_r = crate::effectcraft_ui::screen_rect(&u.app, u.app.session.doc().unwrap(), card).unwrap();
    u.button(card_r.center(), true);
    u.button(card_r.center(), false);
    assert!(u.app.effectcraft.panel.as_ref().unwrap().focused, "a press on the card gives the panel the keyboard");
}

// The panel as a mirror of the EffectCraft app's project (`mirror`). The app is an in-process
// EffectCraft session on the same file, reached synchronously (`mirror::Direct`).

mod mirrored {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use effectcraft_ui_egui::panels::timeline::TimelineHost;
    use figcraft_doc::EcComp;
    use figcraft_ec_inproc::InProc;
    use figcraft_ec_inproc::effectcraft_engine::project::ItemId;
    use serde_json::{Value, json};

    use super::super::mirror::{self, DOWN, DROPPED, Direct, ELSEWHERE, Link, LinkError};
    use super::super::{EcHost, READ_ONLY};
    use super::{input, key, layers, sample};

    /// The EffectCraft app: its engine on the project, reachable unless `down`; `mirrors`: it has
    /// `project.mirror` (an older one doesn't).
    struct App {
        app: Rc<RefCell<InProc>>,
        down: Rc<Cell<bool>>,
        mirrors: bool,
    }

    impl Link for App {
        fn call(&mut self, command: &str, params: Value) -> Result<Value, LinkError> {
            if self.down.get() {
                return Err(LinkError::Down("EffectCraft isn't open".into()));
            }
            if !self.mirrors && command.starts_with("project.") {
                return Err(LinkError::Refused(format!("unknown command `{command}`")));
            }
            self.app.borrow_mut().session_mut().execute(command, params).map_err(|e| LinkError::Refused(e.to_string()))
        }
    }

    struct Rig {
        h: EcHost,
        app: Rc<RefCell<InProc>>,
        down: Rc<Cell<bool>>,
        c: EcComp,
    }

    /// The app has `app_path` open (the card's project unless given); the panel mirrors it.
    fn rig(name: &str, mirrors: bool, app_path: Option<&std::path::Path>) -> Rig {
        let (p, c) = sample(name);
        let app = Rc::new(RefCell::new(InProc::open(app_path.unwrap_or(&p)).unwrap()));
        // Bound first: an `if let` scrutinee's borrow lives through its block (edition 2021),
        // so borrow_mut inside it panicked "RefCell already borrowed" (night run 29 Oct).
        let id = app.borrow().comp_id(&json!(c.comp));
        if let Ok(id) = id {
            app.borrow_mut().session_mut().open_comp(ItemId(id));
        }
        let down = Rc::new(Cell::new(false));
        let link = App { app: app.clone(), down: down.clone(), mirrors };
        let mut h = EcHost::open_mirror(&p, &c, Box::new(Direct::new(Box::new(link)))).unwrap();
        if let Some(m) = h.mirror.as_mut() {
            m.pull_every = std::time::Duration::ZERO;
            m.retry_every = std::time::Duration::ZERO;
        }
        let mut r = Rig { h, app, down, c };
        r.settle();
        r
    }

    impl Rig {
        /// A few frames: what's out comes back, what's next goes out.
        fn settle(&mut self) {
            for _ in 0..6 {
                self.h.sync();
            }
        }
        fn app_layers(&self) -> usize {
            self.app.borrow().session().project.comp(ItemId(self.c.comp_id)).map_or(0, |c| c.layers.len())
        }
        fn app_exec(&self, id: &str, params: Value) -> Value {
            self.app.borrow_mut().session_mut().execute(id, params).unwrap()
        }
        fn same(&self) -> bool {
            self.h.session().project.same_content(&self.app.borrow().session().project)
        }
    }

    fn solid() -> Value {
        json!({"color": "#00ff00", "width": 8, "height": 8})
    }

    /// An edit in the panel lands in the app: one undo step there, labelled, unsaved there (never
    /// here), and both hold the same project.
    #[test]
    fn panel_edits_land_in_the_app() {
        let mut r = rig("m-push", true, None);
        assert!(r.h.mirrored());
        assert_eq!(r.h.read_only(), None, "in step with the app: editable");
        let n = layers(&r.h);
        let steps = r.app.borrow().session().history.undo.len();
        let ctx = egui::Context::default();
        r.h.invoke(&ctx, "layer.newSolid", solid()).unwrap();
        r.settle();
        assert_eq!(r.app_layers(), n + 1, "the app has the panel's layer");
        assert_eq!(layers(&r.h), n + 1);
        assert!(r.same(), "one project");
        assert!(r.app.borrow().session().is_dirty(), "unsaved in the app");
        assert!(!r.h.is_dirty(), "the panel has nothing of its own to save");
        let app = r.app.borrow();
        assert_eq!(app.session().history.undo.len(), steps + 1);
        let label = app.session().history.undo.last().map(|(l, _)| l.clone()).unwrap_or_default();
        assert!(!label.is_empty() && label != "Edit in FigCraft", "the panel's own undo label: {label}");
    }

    /// The app's edits show in the panel (pulled), and nothing goes back the other way.
    #[test]
    fn app_edits_show_in_the_panel() {
        let mut r = rig("m-pull", true, None);
        let n = layers(&r.h);
        r.app_exec("layer.newSolid", solid());
        r.app_exec("layer.newSolid", solid());
        let rev = r.app.borrow().session().revision;
        r.settle();
        assert_eq!(layers(&r.h), n + 2);
        assert!(r.same());
        assert_eq!(r.app.borrow().session().revision, rev, "pulling changed nothing in the app");
    }

    /// ⌘Z / ⇧⌘Z in the panel undo and redo in the app's history.
    #[test]
    fn undo_in_the_panel_is_the_apps() {
        let mut r = rig("m-undo", true, None);
        let ctx = egui::Context::default();
        let n = layers(&r.h);
        r.h.invoke(&ctx, "layer.newSolid", solid()).unwrap();
        r.settle();
        assert_eq!(r.app_layers(), n + 1);
        let mut undo = input();
        undo.events = vec![key(egui::Key::Z, egui::Modifiers::COMMAND)];
        ctx.run_ui(undo, |_| {
            let _ = r.h.keys(&ctx);
        })
        .textures_delta
        .clear();
        r.settle();
        assert_eq!(r.app_layers(), n, "undone in the app");
        assert_eq!(layers(&r.h), n, "and shown here");
        let mut redo = input();
        redo.events = vec![key(egui::Key::Z, egui::Modifiers::COMMAND | egui::Modifiers::SHIFT)];
        ctx.run_ui(redo, |_| {
            let _ = r.h.keys(&ctx);
        })
        .textures_delta
        .clear();
        r.settle();
        assert_eq!(r.app_layers(), n + 1, "redone in the app");
        assert!(r.same());
        // An undo of the app's own edit, from the panel: one history.
        r.app_exec("layer.newSolid", solid());
        r.settle();
        assert!(r.h.invoke(&ctx, "edit.undo", json!({})).is_ok());
        r.settle();
        assert_eq!(r.app_layers(), n + 1);
        assert_eq!(layers(&r.h), n + 1);
    }

    /// A drag (consecutive edits merged by the panel) is one undo step in the app, however many
    /// pushes it took; a new press starts another.
    #[test]
    fn a_drag_is_one_undo_step_in_the_app() {
        let mut r = rig("m-drag", true, None);
        let steps = r.app.borrow().session().history.undo.len();
        let wa = |r: &Rig| r.app.borrow().session().project.comp(ItemId(r.c.comp_id)).map(|c| c.work_area.0.seconds()).unwrap_or(-1.0);
        let start = wa(&r);
        for s in [0.2, 0.4, 0.6] {
            r.h.session_mut().execute("comp.workArea", json!({"start": s, "end": 1.6, "merge": "wa-drag"})).unwrap();
            r.settle();
        }
        assert!((wa(&r) - 0.6).abs() < 1e-6, "the drag's end is in the app: {}", wa(&r));
        assert_eq!(r.app.borrow().session().history.undo.len(), steps + 1, "one step");
        // The next gesture (a press resets the merge key) is a step of its own.
        r.h.session_mut().history.merge_key = None;
        r.h.session_mut().execute("comp.workArea", json!({"start": 0.8, "end": 1.6, "merge": "wa-drag"})).unwrap();
        r.settle();
        assert_eq!(r.app.borrow().session().history.undo.len(), steps + 2);
        // (The app's own ⌘Z: any author's step, as the user's undo.)
        r.app_exec("edit.undo", json!({"any": true}));
        r.app_exec("edit.undo", json!({"any": true}));
        assert!((wa(&r) - start).abs() < 1e-6, "two undos take both gestures back");
        r.settle();
        assert!(r.same());
    }

    /// Both edit at once: the app wins, the panel's edit is dropped (said so) and the panel shows
    /// the app's project; nothing forks.
    #[test]
    fn the_app_wins_when_both_edit() {
        let mut r = rig("m-race", true, None);
        let ctx = egui::Context::default();
        let n = layers(&r.h);
        // The panel's edit, not sent yet, and two of the app's.
        r.h.invoke(&ctx, "prop.set", json!({"layer": "#1", "path": "transform/opacity", "value": 20})).unwrap();
        r.app_exec("layer.newSolid", solid());
        r.app_exec("layer.newSolid", solid());
        r.settle();
        assert_eq!(r.app_layers(), n + 2);
        assert_eq!(layers(&r.h), n + 2, "the app's project");
        assert!(r.same(), "one project, the app's");
        assert_eq!(r.h.ui().status, DROPPED);
        // Editing goes on from there.
        r.h.invoke(&ctx, "layer.newSolid", solid()).unwrap();
        r.settle();
        assert_eq!(r.app_layers(), n + 3);
        assert!(r.same());
    }

    /// The app stops answering: read-only (edits taken back, the app untouched), until it
    /// answers again; then edits land again.
    #[test]
    fn an_unreachable_app_makes_the_panel_read_only_until_it_answers() {
        let mut r = rig("m-down", true, None);
        let ctx = egui::Context::default();
        let n = layers(&r.h);
        r.down.set(true);
        r.settle();
        assert_eq!(r.h.read_only(), Some(DOWN));
        assert!(r.h.stale(), "opening the Timeline again starts afresh");
        assert!(r.h.invoke(&ctx, "layer.newSolid", solid()).is_err());
        assert_eq!(layers(&r.h), n, "taken back");
        assert!(r.h.save(false).is_err(), "nothing to save it with");
        // The app edits meanwhile (e.g. it was busy), then answers again.
        r.down.set(false);
        r.app_exec("layer.newSolid", solid());
        r.settle();
        assert_eq!(r.h.read_only(), None);
        assert_eq!(layers(&r.h), n + 1, "the app's project");
        r.h.invoke(&ctx, "layer.newSolid", solid()).unwrap();
        r.settle();
        assert_eq!(r.app_layers(), n + 2);
        assert!(r.same());
    }

    /// An EffectCraft without `project.mirror`, or with another project open: read-only, the app
    /// untouched.
    #[test]
    fn an_older_app_or_another_project_stays_read_only() {
        let ctx = egui::Context::default();
        let mut old = rig("m-old", false, None);
        assert_eq!(old.h.read_only(), Some(READ_ONLY));
        assert!(old.h.invoke(&ctx, "layer.newSolid", solid()).is_err());
        let (other, _) = sample("m-other-file");
        let mut r = rig("m-other", true, Some(&other));
        assert_eq!(r.h.read_only(), Some(ELSEWHERE));
        assert!(r.h.stale());
        let n = r.app_layers();
        assert!(r.h.invoke(&ctx, "layer.newSolid", solid()).is_err());
        r.settle();
        assert_eq!(r.app_layers(), n, "the other project is untouched");
    }

    /// ⌘S in a mirror saves in the app (its file, its unsaved mark); the panel never writes the
    /// file itself.
    #[test]
    fn save_is_the_apps() {
        let mut r = rig("m-save", true, None);
        let ctx = egui::Context::default();
        r.h.invoke(&ctx, "layer.newSolid", solid()).unwrap();
        r.settle();
        assert!(r.app.borrow().session().is_dirty());
        r.h.save(false).unwrap();
        r.settle();
        assert!(!r.app.borrow().session().is_dirty(), "saved by the app");
        let again = InProc::open(r.h.path()).unwrap();
        assert_eq!(again.session().project.comp(ItemId(r.c.comp_id)).map(|c| c.layers.len()), Some(r.app_layers()));
    }

    /// Junk from the app never panics and never edits: the panel asks for the whole project again.
    #[test]
    fn junk_from_the_app_never_panics() {
        struct Junk(u32);
        impl Link for Junk {
            fn call(&mut self, _: &str, _: Value) -> Result<Value, LinkError> {
                self.0 += 1;
                Ok(match self.0 % 5 {
                    0 => json!(null),
                    1 => json!({"revision": 3, "patch": {"fromRev": 1, "toRev": 3, "ops": [{"op": "remove", "id": 99}]}}),
                    2 => json!({"revision": 4, "project": "{ nope"}),
                    3 => json!({"revision": "x"}),
                    _ => json!({"revision": 5, "unchanged": true}),
                })
            }
        }
        let (p, c) = sample("m-junk");
        let mut h = EcHost::open_mirror(&p, &c, Box::new(Direct::new(Box::new(Junk(0))))).unwrap();
        if let Some(m) = h.mirror.as_mut() {
            m.pull_every = std::time::Duration::ZERO;
        }
        let n = layers(&h);
        for _ in 0..40 {
            h.sync();
        }
        assert!(h.read_only().is_some(), "never in step with junk");
        assert_eq!(layers(&h), n);
        let _ = mirror::CLIENT;
    }
}

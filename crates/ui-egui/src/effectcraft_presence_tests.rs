//! Agents on the board, pen.dev style (Elias: "if an agent is working on its comp, its comp is
//! glowing like this and has this system that Pen Dev has. include that too"). Driven without the
//! app's frame (no card link, so no EffectCraft port is ever reached): only this module's frame,
//! overlay and input.

use egui::{Event, PointerButton, RawInput, Rect, pos2, vec2};
use figcraft_doc::NodeId;
use figcraft_engine::Session;
use figcraft_engine::ecboard::presence::{Agent, Status};
use serde_json::{Value, json};

use super::*;
use crate::{FigApp, NoServices};

fn agent(bbox: Option<[f64; 4]>) -> Agent {
    Agent {
        agent: "A2".into(),
        name: "A2".into(),
        color: [0xFF, 0xB0, 0x20],
        card: NodeId(1),
        comp: "S1 06 Old vs AI".into(),
        comp_id: 7,
        layer: None,
        bbox,
        status: Status::Working,
        serial: 1,
    }
}

#[test]
fn the_cursor_sits_at_the_touched_box_else_the_cards_corner() {
    let s = spot_of(&agent(Some([100.0, 200.0, 300.0, 80.0])), 1920, 1080);
    assert!((s.u - 250.0 / 1920.0).abs() < 1e-6 && (s.v - 240.0 / 1080.0).abs() < 1e-6, "{s:?}");
    let s = spot_of(&agent(None), 1920, 1080);
    assert_eq!((s.u, s.v), (0.0, 0.0));
    let s = spot_of(&agent(Some([5000.0, -900.0, 10.0, 10.0])), 1920, 1080);
    assert_eq!((s.u, s.v), (1.0, 0.0), "kept on the card");
    let s = spot_of(&agent(Some([0.0, 0.0, 10.0, 10.0])), 0, 0);
    assert!(s.u.is_finite() && s.v.is_finite());
}

#[test]
fn glow_pulses_while_working_and_holds_when_done() {
    assert!(glow(Status::Working, 0.0, true) < 0.01);
    assert!(glow(Status::Working, PULSE / 2.0, true) > 0.99);
    assert!(glow(Status::Working, PULSE, true) < 0.01, "one pulse every 1.6 s");
    assert_eq!(glow(Status::Working, 0.3, false), glow(Status::Working, 1.1, false), "reduce motion: no pulse");
    assert_eq!(glow(Status::Done, 0.3, true), 1.0);
    assert_eq!(glow(Status::Done, 0.9, false), 1.0);
    assert_eq!(glow(Status::Idle, 0.9, true), 0.0);
    assert!(glow(Status::Working, f64::NAN, true).is_finite());
}

#[test]
fn glides_ease_out_and_touch_boxes_fade() {
    assert_eq!(ease_out(0.0), 0.0);
    assert_eq!(ease_out(1.0), 1.0);
    assert!(ease_out(0.5) > 0.8, "fast start, slow end");
    assert_eq!(ease_out(f64::NAN), 1.0);
    assert_eq!(touch_alpha(0.0), Some(1.0));
    assert!(touch_alpha(TOUCH * 0.5).is_some_and(|a| a < 0.5));
    assert_eq!(touch_alpha(TOUCH), None);
    assert_eq!(touch_alpha(-0.1), None);
    assert_eq!(touch_alpha(f64::NAN), None);
    assert_eq!(ink_on(egui::Color32::from_rgb(0xFF, 0xD6, 0x0A)), egui::Color32::from_rgb(0x14, 0x14, 0x16));
    assert_eq!(ink_on(egui::Color32::from_rgb(0x2D, 0x4C, 0xAF)), egui::Color32::WHITE);
}

/// An app with one card (480×270 page units at 0, 0, comp 1920×1080) shown at zoom 1 in a
/// 1440×900 canvas, and agent A2 on it.
fn app_with_agent() -> (FigApp, egui::Context, NodeId) {
    let mut s = Session::new();
    let r = s
        .execute(
            "effectcraft.place",
            &json!({"comp": "S1 06 Old vs AI", "compId": 7, "project": "/nonexistent/26-10.ecproj", "start": 0, "end": 2, "x": 0, "y": 0, "cardWidth": 480}),
        )
        .unwrap();
    let card = figcraft_engine::params::id_of(&r["id"]).unwrap();
    s.execute("canvas.presence.set", &json!({"agent": "A2", "card": card, "color": "#ffb020", "bbox": [960, 540, 0, 0]})).unwrap();
    let mut app = FigApp::new(s, Box::new(NoServices));
    app.canvas.rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
    if let Ok(ds) = app.session.ds_mut() {
        (ds.view.vw, ds.view.vh, ds.view.zoom) = (1440.0, 900.0, 1.0);
        let b = ds.doc.world_bounds(card).unwrap();
        (ds.view.cx, ds.view.cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
    }
    let ctx = egui::Context::default();
    app.ui_context = Some(ctx.clone());
    (app, ctx, card)
}

/// One pass at `time` with `events`, running only the presence frame (and `input` at `pos`).
fn pass(app: &mut FigApp, ctx: &egui::Context, time: f64, events: Vec<Event>, pos: Option<egui::Pos2>) -> bool {
    let raw =
        RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))), time: Some(time), events, ..Default::default() };
    let mut took = false;
    let _ = ctx.run_ui(raw, |ui| {
        let ctx = ui.ctx().clone();
        frame(app, &ctx);
        if let Some(p) = pos {
            took = input(app, &ctx, p, true);
        }
        overlay(app, &ui.painter().clone());
    });
    took
}

fn events_of(app: &FigApp, kind: &str) -> Vec<Value> {
    app.session.ecboard.events_since(0).filter(|e| e.kind == kind).map(|e| e.data.clone()).collect()
}

fn cursor(app: &FigApp, ctx: &egui::Context, now: f64) -> egui::Pos2 {
    let ds = app.session.ds().unwrap();
    let list = shown(app, &ds.doc, ds.page, now, motion(app, ctx));
    list.first().unwrap().cursor
}

#[test]
fn the_cursor_maps_comp_pixels_onto_the_card_and_glides_to_a_new_place() {
    let (mut app, ctx, card) = app_with_agent();
    pass(&mut app, &ctx, 1.0, vec![], None);
    let (pic, _) = super::super::board::pic_of(&app, &app.session.ds().unwrap().doc, card).unwrap();
    let at = cursor(&app, &ctx, 1.0);
    assert!(at.distance(pic.center()) < 0.5, "comp centre = card centre: {at:?} vs {pic:?}");

    // Moved to the top-left quarter: it glides there (ease-out, 0.25 s), then sits there.
    app.session.execute("canvas.presence.set", &json!({"agent": "A2", "card": card, "bbox": [0, 0, 960, 540]})).unwrap();
    pass(&mut app, &ctx, 2.0, vec![], None);
    let to = pos2(pic.min.x + pic.width() / 4.0, pic.min.y + pic.height() / 4.0);
    let mid = cursor(&app, &ctx, 2.0 + GLIDE / 2.0);
    assert!(mid.distance(to) > 1.0 && mid.distance(at) > mid.distance(to), "on its way, past half: {mid:?}");
    pass(&mut app, &ctx, 2.0 + GLIDE + 0.01, vec![], None);
    assert!(cursor(&app, &ctx, 2.0 + GLIDE + 0.01).distance(to) < 0.5);
}

#[test]
fn hovering_and_clicking_an_agent_are_board_events() {
    let (mut app, ctx, card) = app_with_agent();
    pass(&mut app, &ctx, 1.0, vec![], None);
    let tip = cursor(&app, &ctx, 1.0);
    let over = tip + vec2(4.0, 6.0);
    pass(&mut app, &ctx, 1.1, vec![Event::PointerMoved(over)], None);
    assert_eq!(events_of(&app, "presence.hover"), vec![json!({"agent": "A2"})]);
    // Still over it: no new event.
    pass(&mut app, &ctx, 1.2, vec![Event::PointerMoved(over + vec2(1.0, 0.0))], None);
    assert_eq!(events_of(&app, "presence.hover").len(), 1);
    // A click on the cursor opens the agent and takes the press.
    let press = Event::PointerButton { pos: over, button: PointerButton::Primary, pressed: true, modifiers: Default::default() };
    assert!(pass(&mut app, &ctx, 1.3, vec![press], Some(over)));
    assert_eq!(events_of(&app, "presence.open"), vec![json!({"agent": "A2", "card": card})]);
    let release = Event::PointerButton { pos: over, button: PointerButton::Primary, pressed: false, modifiers: Default::default() };
    pass(&mut app, &ctx, 1.35, vec![release], None);
    // Away from the cursor and the glow: the hover ends.
    pass(&mut app, &ctx, 1.4, vec![Event::PointerMoved(pos2(1400.0, 880.0))], None);
    assert_eq!(events_of(&app, "presence.hover").last(), Some(&json!({"agent": null})));
    // The glow band round the card counts as the agent too.
    let (pic, _) = super::super::board::pic_of(&app, &app.session.ds().unwrap().doc, card).unwrap();
    pass(&mut app, &ctx, 1.5, vec![Event::PointerMoved(pos2(pic.center().x, pic.max.y + 4.0))], None);
    assert_eq!(events_of(&app, "presence.hover").last(), Some(&json!({"agent": "A2"})));
    // The middle of the card stays the card's.
    assert!(!pass(
        &mut app,
        &ctx,
        1.6,
        vec![Event::PointerButton {
            pos: pic.center() + vec2(100.0, 60.0),
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Default::default()
        }],
        Some(pic.center() + vec2(100.0, 60.0))
    ));
    let up =
        Event::PointerButton { pos: pic.center() + vec2(100.0, 60.0), button: PointerButton::Primary, pressed: false, modifiers: Default::default() };
    pass(&mut app, &ctx, 1.65, vec![up], None);
    // Cleared: nothing to hover, nothing drawn, nothing panics.
    app.session.execute("canvas.presence.clear", &json!({})).unwrap();
    pass(&mut app, &ctx, 1.7, vec![Event::PointerMoved(pos2(pic.center().x, pic.max.y + 4.0))], None);
    assert_eq!(events_of(&app, "presence.hover").last(), Some(&json!({"agent": null})));
}

#[test]
fn several_agents_and_a_deleted_card_never_panic() {
    let (mut app, ctx, card) = app_with_agent();
    for (i, st) in ["working", "done", "idle"].iter().enumerate() {
        app.session
            .execute("canvas.presence.set", &json!({"agent": format!("B{i}"), "card": card, "status": st, "bbox": [i * 100, 0, 50, 50]}))
            .unwrap();
    }
    pass(&mut app, &ctx, 1.0, vec![Event::PointerMoved(pos2(720.0, 450.0))], Some(pos2(720.0, 450.0)));
    let ds = app.session.ds().unwrap();
    let list = shown(&app, &ds.doc, ds.page, 1.0, true);
    assert_eq!(list.len(), 4);
    assert_eq!(list.iter().map(|s| s.step).collect::<Vec<_>>(), vec![0.0, 1.0, 2.0, 3.0], "outlines stack outward");
    app.session.execute("edit.delete", &json!({"ids": [card]})).unwrap();
    assert!(app.session.doc().unwrap().node(card).is_none());
    pass(&mut app, &ctx, 2.0, vec![Event::PointerMoved(pos2(10.0, 10.0))], Some(pos2(10.0, 10.0)));
}

//! EffectCraft's viewer tools on a card, against FigCraft's own host (`EcHost`, the Timeline's
//! session): tool keys and the tool row, masks, rotation, the anchor point, the Pen and typed
//! text landing in the comp, the card's pointer mapping at several canvas zooms, and junk input;
//! (EMBED14) guides and grids drawn only while on and the grid button, motion-path keys and
//! tangents dragged on the card, the Mask Feather tool.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use effectcraft_ui_egui::panels::timeline::TimelineHost;
use effectcraft_ui_egui::panels::viewer::ViewerMap;
use effectcraft_ui_egui::panels::viewer_hosted as vh;
use effectcraft_ui_egui::state::Tool;
use egui::{Event, PointerButton, Pos2, Rect, pos2, vec2};
use figcraft_doc::EcComp;
use figcraft_ec_inproc::effectcraft_engine::project::{LayerId, LayerSource};
use serde_json::json;

use super::{ROW, Slot, grid_button, paint_grid_icon, paint_icon, tool_row};
use crate::effectcraft_panel::EcHost;

/// The sample project (a 64×36 comp "C" with a full-size red solid) plus a 16×16 Box in the
/// middle (box 24,10 – 40,26); snapping off, nothing selected.
fn host(name: &str) -> (EcHost, LayerId, LayerId) {
    let d = std::env::temp_dir().join(format!("figcraft-ec-viewer-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let p: PathBuf = d.join("p.ecproj");
    let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
    let mut c = EcComp::new("C", 0.0, 2.0, 25.0);
    c.comp_id = cid;
    c.project = p.to_str().unwrap().to_string();
    let mut h = EcHost::open(&p, &c, false).unwrap();
    let ctx = egui::Context::default();
    let bx = LayerId(
        h.invoke(&ctx, "layer.newSolid", json!({"name": "Box", "color": "#00ff00", "width": 16, "height": 16})).unwrap()["layer"].as_u64().unwrap(),
    );
    let comp = h.session().active_comp().unwrap();
    let plate = comp.layers.iter().find(|l| l.id != bx).unwrap().id;
    h.invoke(&ctx, "edit.deselectAll", json!({})).unwrap();
    h.session_mut().state.snapping = false;
    (h, plate, bx)
}

/// The card's picture at canvas zoom `k`: the 64×36 comp at (100, 50), `k` points per pixel.
fn card(k: f32) -> ViewerMap {
    vh::map_for(Rect::from_min_size(pos2(100.0, 50.0), vec2(64.0 * k, 36.0 * k)), [64, 36]).unwrap()
}

fn frame(ctx: &egui::Context, h: &mut EcHost, map: ViewerMap, events: Vec<Event>) -> vh::Frame {
    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), events, ..Default::default() };
    let mut out = vh::Frame::default();
    ctx.run_ui(input, |ui| {
        out = vh::tools(h, ui, map, vh::Options { id: egui::Id::new("card"), space_pan: false });
    })
    .textures_delta
    .clear();
    out
}

fn button(at: Pos2, pressed: bool) -> Event {
    Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }
}

/// Hover, press at the first comp point, move through the rest, release (screen points from
/// comp pixels through `map`).
fn drag(ctx: &egui::Context, h: &mut EcHost, map: ViewerMap, comp: &[[f64; 2]]) {
    let at: Vec<Pos2> = comp.iter().map(|c| map.to_screen(*c)).collect();
    frame(ctx, h, map, vec![Event::PointerMoved(at[0])]);
    frame(ctx, h, map, vec![button(at[0], true)]);
    for p in &at[1..] {
        frame(ctx, h, map, vec![Event::PointerMoved(*p)]);
    }
    let last = *at.last().unwrap();
    frame(ctx, h, map, vec![button(last, false)]);
    frame(ctx, h, map, vec![]);
}

fn click(ctx: &egui::Context, h: &mut EcHost, map: ViewerMap, comp: [f64; 2]) {
    let at = map.to_screen(comp);
    frame(ctx, h, map, vec![Event::PointerMoved(at)]);
    frame(ctx, h, map, vec![button(at, true)]);
    frame(ctx, h, map, vec![button(at, false)]);
    frame(ctx, h, map, vec![]);
}

fn masks(h: &EcHost, l: LayerId) -> usize {
    h.session().active_comp().unwrap().layer(l).unwrap().masks().map_or(0, |m| m.groups().count())
}

fn value(h: &EcHost, l: LayerId, path: &str) -> [f64; 3] {
    let c = h.session().active_comp().unwrap();
    let layer = c.layer(l).unwrap();
    layer.props.prop(path).unwrap().value_at(layer.layer_time(h.session().time())).as_vec3()
}

fn scalar(h: &EcHost, l: LayerId, path: &str) -> f64 {
    let c = h.session().active_comp().unwrap();
    let layer = c.layer(l).unwrap();
    layer.props.prop(path).unwrap().value_at(layer.layer_time(h.session().time())).as_f64()
}

/// The tools' keys (from Elias's EffectCraft keymap when he has one, else EffectCraft's) pick
/// them while the Timeline has the keyboard, and the key for the Selection tool comes back to it.
#[test]
fn tool_keys_switch_the_card_tools() {
    let (mut h, _, _) = host("keys");
    let ctx = egui::Context::default();
    let mut tried = 0;
    for (id, want) in
        [("tool.rotate", Tool::Rotate), ("tool.panBehind", Tool::PanBehind), ("tool.pen", Tool::Pen), ("tool.selection", Tool::Selection)]
    {
        let Some((mods, key)) = h.session().shortcuts().shortcut_of(id, &json!({})).and_then(effectcraft_ui_egui::menus::parse_shortcut) else {
            continue;
        };
        let input = egui::RawInput {
            events: vec![Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: mods }],
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let _ = h.keys(ui.ctx());
        })
        .textures_delta
        .clear();
        assert_eq!(h.ui().tool, want, "{id}");
        tried += 1;
    }
    assert!(tried > 0, "no tool key bound at all");
    // By command too (the tool row): a slot picks its last used tool, and junk is refused.
    h.invoke(&ctx, "tool.shape", json!({})).unwrap();
    assert!(h.ui().tool.is_shape());
    assert!(h.invoke(&ctx, "tool.nothing", json!({})).is_err());
}

#[test]
fn the_tool_row_draws_and_lights_the_current_tool() {
    let (mut h, _, _) = host("row");
    let ctx = egui::Context::default();
    h.ui_mut().tool = Tool::Ellipse;
    ctx.run_ui(egui::RawInput::default(), |ui| {
        ui.horizontal(|ui| tool_row(ui, &mut h));
    })
    .textures_delta
    .clear();
    let lit: Vec<Slot> = ROW.iter().map(|(s, ..)| *s).filter(|s| s.has(Tool::Ellipse)).collect();
    assert_eq!(lit, vec![Slot::Shape]);
    assert!(Slot::Pen.has(Tool::PenConvert) && Slot::Type.has(Tool::TypeVertical) && !Slot::Selection.has(Tool::Rotate));
}

/// The card maps the pointer to comp pixels at any canvas zoom.
#[test]
fn the_card_maps_the_pointer_at_every_canvas_zoom() {
    for k in [0.25_f32, 0.5, 1.0, 2.0, 8.0] {
        let m = card(k);
        let c = vh::comp_point(&m, pos2(100.0 + 32.0 * k, 50.0 + 18.0 * k)).unwrap();
        assert!((c[0] - 32.0).abs() < 1e-3 && (c[1] - 18.0).abs() < 1e-3, "zoom {k}: {c:?}");
        let s = m.to_screen([64.0, 36.0]);
        assert!((s.x - (100.0 + 64.0 * k)).abs() < 1e-3 && (s.y - (50.0 + 36.0 * k)).abs() < 1e-3);
    }
    assert!(vh::map_for(Rect::NOTHING, [64, 36]).is_none());
    assert!(vh::map_for(Rect::from_min_size(pos2(f32::INFINITY, 0.0), vec2(10.0, 10.0)), [64, 36]).is_none());
}

/// A shape tool drag on the card (canvas at 4×) lands as a mask on the selected layer, one undo
/// step in EffectCraft.
#[test]
fn a_mask_drawn_on_the_card_lands_in_the_comp() {
    let (mut h, plate, _) = host("mask");
    let ctx = egui::Context::default();
    h.invoke(&ctx, "layer.select", json!({"layers": [plate.0]})).unwrap();
    h.ui_mut().tool = Tool::Ellipse;
    drag(&ctx, &mut h, card(4.0), &[[4.0, 4.0], [10.0, 8.0], [20.0, 14.0]]);
    assert_eq!(masks(&h, plate), 1);
    assert!(h.is_dirty());
    h.invoke(&ctx, "edit.undo", json!({})).unwrap();
    assert_eq!(masks(&h, plate), 0);
}

#[test]
fn the_rotation_tool_turns_a_layer_on_the_card() {
    let (mut h, _, bx) = host("rotate");
    let ctx = egui::Context::default();
    h.ui_mut().tool = Tool::Rotate;
    // About the Box's anchor (32, 18), at canvas zoom 4: right of it, then below it: +90°.
    drag(&ctx, &mut h, card(4.0), &[[37.0, 18.0], [40.0, 18.0], [32.0, 26.0]]);
    assert_eq!(h.session().state.selected_layers, vec![bx]);
    let r = scalar(&h, bx, "transform/rotation");
    assert!((r - 90.0).abs() < 0.5, "rotation {r}");
}

#[test]
fn pan_behind_moves_the_anchor_on_the_card() {
    let (mut h, _, bx) = host("anchor");
    let ctx = egui::Context::default();
    h.ui_mut().tool = Tool::PanBehind;
    drag(&ctx, &mut h, card(4.0), &[[32.0, 18.0], [34.0, 18.0], [36.0, 20.0]]);
    let a = value(&h, bx, "transform/anchor");
    let p = value(&h, bx, "transform/position");
    assert!((a[0] - 12.0).abs() < 0.3 && (a[1] - 10.0).abs() < 0.3, "anchor {a:?}");
    assert!((p[0] - 36.0).abs() < 0.3 && (p[1] - 20.0).abs() < 0.3, "position {p:?}: the layer stays put");
}

#[test]
fn the_pen_draws_a_closed_mask_on_the_card() {
    let (mut h, plate, _) = host("pen");
    let ctx = egui::Context::default();
    h.invoke(&ctx, "layer.select", json!({"layers": [plate.0]})).unwrap();
    h.ui_mut().tool = Tool::Pen;
    for p in [[6.0, 6.0], [30.0, 6.0], [18.0, 30.0], [6.0, 6.0]] {
        click(&ctx, &mut h, card(4.0), p);
    }
    assert_eq!(masks(&h, plate), 1);
}

#[test]
fn text_typed_on_the_card_lands_in_the_comp() {
    let (mut h, _, _) = host("type");
    let ctx = egui::Context::default();
    h.ui_mut().tool = Tool::Type;
    let map = card(4.0);
    click(&ctx, &mut h, map, [8.0, 30.0]);
    assert!(h.session().state.text_edit.is_some(), "a text edit began");
    frame(&ctx, &mut h, map, vec![Event::Text("Hi".into())]);
    frame(&ctx, &mut h, map, vec![]);
    let comp = h.session().active_comp().unwrap();
    let text = comp.layers.iter().find(|l| matches!(l.source, LayerSource::Text)).map(|l| l.id).unwrap();
    let doc = figcraft_ec_inproc::effectcraft_engine::commands::text_edit::layer_doc(h.session(), text).unwrap();
    assert_eq!(doc.text, "Hi");
}

/// Shapes one frame paints over the card at zoom 4 (`with`: the tools run).
fn painted(ctx: &egui::Context, h: &mut EcHost, with: bool) -> usize {
    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
    ctx.run_ui(input, |ui| {
        if with {
            let _ = vh::tools(h, ui, card(4.0), vh::Options { id: egui::Id::new("card"), space_pan: false });
        }
    })
    .shapes
    .len()
}

/// Nothing goes over the card until a guide or grid is turned on: EffectCraft's keys (through
/// the Timeline's host) and the grid button's menu turn them on and off.
#[test]
fn guides_and_grids_show_on_the_card_only_while_on() {
    let (mut h, _, _) = host("grids");
    let ctx = egui::Context::default();
    h.invoke(&ctx, "view.addGuide", json!({"orientation": "vertical", "position": 20})).unwrap();
    h.ui_mut().viewer.guides = false;
    let none = painted(&ctx, &mut h, false);
    assert_eq!(painted(&ctx, &mut h, true), none, "nothing over the picture");
    for id in ["view.grid", "view.guides", "view.safeMargins"] {
        h.invoke(&ctx, id, json!({})).unwrap();
    }
    assert!(h.ui().viewer.grid && h.ui().viewer.guides && h.ui().viewer.safe_margins);
    assert!(painted(&ctx, &mut h, true) > none);
    for id in ["view.grid", "view.guides", "view.safeMargins"] {
        h.invoke(&ctx, id, json!({"value": false})).unwrap();
    }
    assert_eq!(painted(&ctx, &mut h, true), none);
    // Overlays alone (the tools sitting out) draw the same things.
    vh::toggle_grid_option(&mut h, 2);
    let input = egui::RawInput::default();
    let shapes = ctx.run_ui(input, |ui| vh::view_overlays(&mut h, ui, card(4.0))).shapes.len();
    assert!(shapes > 0, "the grid while the tools sit out");
    vh::toggle_grid_option(&mut h, 2);
    assert!(!vh::grid_options_on(&h));
    // The grid button and its open menu draw (lit while one is on).
    egui::Popup::open_id(&ctx, super::grid_popup_id());
    for _ in 0..2 {
        ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.horizontal(|ui| grid_button(ui, &mut h));
        })
        .textures_delta
        .clear();
    }
}

/// A Position keyframe of the selected layer drags on the card at several canvas zooms, one
/// undo step each; its tangent handle drags the tangent.
#[test]
fn a_motion_path_key_drags_on_the_card() {
    let (mut h, _, bx) = host("motion");
    let ctx = egui::Context::default();
    h.invoke(&ctx, "prop.addKey", json!({"layer": bx.0, "path": "transform/position", "time": 0.0, "value": [32.0, 18.0, 0.0]})).unwrap();
    h.invoke(&ctx, "prop.addKey", json!({"layer": bx.0, "path": "transform/position", "time": 1.0, "value": [48.0, 18.0, 0.0]})).unwrap();
    h.invoke(&ctx, "layer.select", json!({"layers": [bx.0]})).unwrap();
    let key0 = |h: &EcHost| {
        let c = h.session().active_comp().unwrap();
        c.layer(bx).unwrap().props.prop("transform/position").unwrap().keys[0].clone()
    };
    for (k, to) in [(4.0_f32, [36.0, 22.0]), (0.5, [40.0, 26.0])] {
        let from = key0(&h).value.as_vec3();
        frame(&ctx, &mut h, card(k), vec![]);
        drag(&ctx, &mut h, card(k), &[[from[0], from[1]], [from[0] + 1.0, from[1] + 1.0], to]);
        let v = key0(&h).value.as_vec3();
        // At 50 % a screen point is two comp pixels: within one of the target.
        let tol = if k < 1.0 { 1.5 } else { 0.3 };
        assert!((v[0] - to[0]).abs() < tol && (v[1] - to[1]).abs() < tol, "zoom {k}: {v:?}");
        h.invoke(&ctx, "edit.undo", json!({})).unwrap();
        assert_eq!(key0(&h).value.as_vec3(), from, "zoom {k}: one undo step");
    }
    // The out tangent handle, where EffectCraft draws it.
    let keys = h.session().active_comp().unwrap().layer(bx).unwrap().props.prop("transform/position").unwrap().keys.clone();
    let (_, out) = if keys[0].spatial_auto {
        figcraft_ec_inproc::effectcraft_engine::keyframe::spatial_handles(&keys, 0)
    } else {
        figcraft_ec_inproc::effectcraft_engine::keyframe::spatial_tangents(&keys, 0)
    };
    assert!(out[0].abs() > 0.5, "an out handle: {out:?}");
    let at = [32.0 + out[0], 18.0 + out[1]];
    drag(&ctx, &mut h, card(4.0), &[at, [at[0], at[1] + 1.0], [at[0], at[1] + 6.0]]);
    assert!((key0(&h).spatial_out[1] - 6.0).abs() < 0.3, "{:?}", key0(&h).spatial_out);
}

/// The Mask Feather tool sits in the Pen's slot (lit there) and adds a feather point dragged out
/// from a mask's edge on the card.
#[test]
fn the_mask_feather_tool_works_on_the_card() {
    let (mut h, plate, _) = host("feather");
    let ctx = egui::Context::default();
    h.invoke(&ctx, "mask.new", json!({"layer": plate.0, "vertices": [[8, 8], [40, 8], [40, 28], [8, 28]], "closed": true})).unwrap();
    h.invoke(&ctx, "layer.select", json!({"layers": [plate.0]})).unwrap();
    h.ui_mut().tool = Tool::MaskFeather;
    assert!(Slot::Pen.has(Tool::MaskFeather) && vh::active(&h) == Tool::MaskFeather);
    drag(&ctx, &mut h, card(4.0), &[[24.0, 8.0], [24.0, 7.0], [24.0, 4.0]]);
    let c = h.session().active_comp().unwrap();
    let l = c.layer(plate).unwrap();
    let g = l.masks().unwrap().groups().next().unwrap();
    let sp = l.props.find_group(g.uid).unwrap().get("path").unwrap().value_at(l.layer_time(h.session().time())).as_path().cloned().unwrap();
    assert_eq!(sp.feather.len(), 1);
    assert!((sp.feather[0].radius - 4.0).abs() < 0.3, "radius {}", sp.feather[0].radius);
}

/// Junk maps, pointer positions and icon boxes never panic.
#[test]
fn junk_never_panics() {
    let (mut h, _, _) = host("junk");
    let ctx = egui::Context::default();
    let maps = [
        ViewerMap { origin: pos2(f32::NAN, 0.0), zoom: 1.0, comp: [64.0, 36.0], area: Rect::from_min_size(Pos2::ZERO, vec2(64.0, 36.0)) },
        ViewerMap { origin: Pos2::ZERO, zoom: -1.0, comp: [64.0, 36.0], area: Rect::from_min_size(Pos2::ZERO, vec2(64.0, 36.0)) },
        ViewerMap { origin: Pos2::ZERO, zoom: 1e-30, comp: [0.0, 0.0], area: Rect::NOTHING },
        card(1e6),
    ];
    h.ui_mut().viewer.grid = true;
    h.ui_mut().viewer.rulers = true;
    h.ui_mut().viewer.safe_margins = true;
    h.ui_mut().viewer.proportional_grid = true;
    for tool in [Tool::Selection, Tool::Rotate, Tool::PanBehind, Tool::Star, Tool::Pen, Tool::PenAdd, Tool::MaskFeather, Tool::Type] {
        h.ui_mut().tool = tool;
        for m in maps {
            ctx.run_ui(egui::RawInput::default(), |ui| vh::view_overlays(&mut h, ui, m)).textures_delta.clear();
            for at in [pos2(f32::NAN, 0.0), pos2(-1e30, 1e30), pos2(110.0, 60.0)] {
                frame(&ctx, &mut h, m, vec![Event::PointerMoved(at), button(at, true)]);
                frame(&ctx, &mut h, m, vec![Event::PointerMoved(at + vec2(30.0, 30.0)), button(at, false)]);
                let _ = vh::edit_text_at(&mut h, &ctx, m, at);
            }
        }
    }
    ctx.run_ui(egui::RawInput::default(), |ui| {
        for tool in Tool::ALL {
            for r in [
                Rect::NOTHING,
                Rect::from_min_size(Pos2::ZERO, vec2(0.0, 0.0)),
                Rect::from_min_size(pos2(f32::NAN, 0.0), vec2(16.0, 16.0)),
                Rect::from_min_size(Pos2::ZERO, vec2(16.0, 16.0)),
            ] {
                paint_icon(ui.painter(), r, tool, egui::Color32::WHITE);
                paint_grid_icon(ui.painter(), r, egui::Color32::WHITE);
            }
        }
    })
    .textures_delta
    .clear();
}

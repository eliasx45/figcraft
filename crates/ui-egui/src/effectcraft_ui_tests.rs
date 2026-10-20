//! EffectCraft cards: the picture letterboxed in the card, the frame size asked for, the order
//! frames are fetched in, and exploded precomps mapped onto the same edit time.
#![allow(clippy::unwrap_used)]

use egui::{Rect, pos2};
use figcraft_doc::EcComp;
use serde_json::json;

use crate::effectcraft_ui::{explode_steps, fit, order, side_for};

fn main_card() -> EcComp {
    EcComp::new("MAIN", 1.4, 5.1, 29.97)
}

#[test]
fn the_picture_keeps_the_comps_shape() {
    let r = fit(Rect::from_min_max(pos2(0.0, 0.0), pos2(400.0, 400.0)), 1920, 1080);
    assert_eq!((r.width(), r.height()), (400.0, 225.0));
    assert_eq!(r.center(), pos2(200.0, 200.0));
}

#[test]
fn frames_are_asked_for_at_the_size_shown_up_to_half_the_comp() {
    let c = main_card();
    assert_eq!(side_for(100.0, &c), 240);
    assert_eq!(side_for(300.0, &c), 480);
    assert_eq!(side_for(700.0, &c), 960);
    assert_eq!(side_for(4000.0, &c), 960, "half of 1920");
    let mut small = main_card();
    (small.width, small.height) = (369, 151);
    assert_eq!(side_for(800.0, &small), 240, "never more than the comp has");
}

#[test]
fn frames_come_from_the_playhead_on_the_way_it_plays() {
    let c = main_card(); // frames 41..=152
    let o = order(&c, 100);
    assert_eq!(&o[..3], &[100, 101, 102]);
    assert_eq!(o.len(), 112, "every frame once");
    assert_eq!(o[52], 152);
    assert_eq!(o[53], 41, "then round from the start");
}

#[test]
fn exploded_precomps_share_the_edit_time() {
    let c = main_card();
    let tree = json!({"children": [{
        "layer": {"id": 66446, "name": "Document page"},
        "comp": {"id": 221, "name": "Document page", "width": 1080, "height": 1350, "frameRate": 29.97},
        "start": 1.4, "stretch": 100.0, "in": 1.4, "out": 5.14, "timeRemap": false,
        "children": [{
            "layer": {"id": 1, "name": "Document sheet"},
            "comp": {"id": 80211, "name": "Document sheet", "width": 1080, "height": 1350, "frameRate": 29.97},
            "start": 0.0, "stretch": 200.0, "in": 0.0, "out": 600.0, "timeRemap": false, "children": []
        }]
    }]});
    let steps = explode_steps(&c, (0.0, 0.0, 480.0, 270.0), &tree);
    assert_eq!(steps.len(), 2);
    let (page, sheet) = (&steps[0].1, &steps[1].1);
    assert_eq!(page["comp"], json!("Document page"));
    // MAIN 1.4–5.1 → Document page 0–3.7, starting 1.4 s into the edit.
    assert!((page["start"].as_f64().unwrap()).abs() < 1e-9 && (page["end"].as_f64().unwrap() - 3.7).abs() < 1e-9, "{page}");
    assert!((page["offset"].as_f64().unwrap() - 1.4).abs() < 1e-9);
    // Stretched to 200 %: half speed.
    assert!((sheet["end"].as_f64().unwrap() - 1.85).abs() < 1e-9, "{sheet}");
    assert!((sheet["rate"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(sheet["sound"], json!(false), "only the card itself sounds");
    assert!(sheet["x"].as_f64().unwrap() > page["x"].as_f64().unwrap(), "a column per level");
}

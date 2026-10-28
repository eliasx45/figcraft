//! EffectCraft cards: the picture letterboxed in the card, the frame size asked for, the order
//! frames are fetched in, and exploded precomps mapped onto the same edit time.
#![allow(clippy::unwrap_used)]

use egui::{Rect, pos2};
use figcraft_doc::{EcComp, NodeId};
use serde_json::json;

use crate::effectcraft_ui::{card_bar, explode_steps, fit, on_button, on_slider, order, side_for, slider_time};

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
    let steps = explode_steps(NodeId(12), &c, (0.0, 0.0, 480.0, 270.0), &tree);
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
    assert!(steps.iter().all(|(_, p)| p["explodedFrom"] == json!(NodeId(12))), "explode closes them again");
}

#[test]
fn the_slider_runs_over_the_cards_stretch() {
    let c = main_card();
    let pic = Rect::from_min_max(pos2(100.0, 0.0), pos2(500.0, 225.0));
    assert!((slider_time(&c, pic, 100.0) - 1.4).abs() < 1e-9);
    assert!((slider_time(&c, pic, 300.0) - 3.25).abs() < 1e-9);
    assert!((slider_time(&c, pic, 900.0) - 5.1).abs() < 1e-9, "past the end stays on it");
    assert!((slider_time(&c, pic, -50.0) - 1.4).abs() < 1e-9);
}

/// Regression (live test 25 Oct): a card whose frames failed (comp not in the app's project) and
/// then got a project stayed black and asked nothing: failed frames were never asked again and
/// the project wasn't part of the store. Now a failed frame is asked again after FAILED_RETRY,
/// and another project is another store.
#[test]
fn failed_frames_are_asked_again_and_a_new_project_is_a_new_store() {
    use crate::effectcraft_ui::{FAILED_RETRY, Store, StoreKey, wants};
    let mut s = Store::default();
    assert!(wants(&s, 10, 0.0, 0, false));
    s.fail(10, 1.0);
    assert!(!wants(&s, 10, 1.0 + FAILED_RETRY / 2.0, 0, false), "not hammered right away");
    assert!(wants(&s, 10, 1.0 + FAILED_RETRY + 0.1, 0, false), "asked again later");
    let mut c = main_card();
    let before = StoreKey::of(&c, 240);
    c.project = "/x/p.ecproj".into();
    let after = StoreKey::of(&c, 240);
    assert_ne!(before, after);
    assert!(after.same_comp(&c) && !before.same_comp(&c));
}

/// Regression (live test 26 Oct): a layer moved with the card's handles (or any docked-panel
/// edit) moved its outline but the card kept showing the frames rendered before the edit: frames
/// of the saved file counted as fresh forever. Now the panel's edit revision is wanted, and frames
/// of another revision are asked for again.
#[test]
fn frames_of_an_older_panel_edit_are_asked_again() {
    use crate::effectcraft_ui::Store;
    assert!(!Store::with_revisions(Some(1), None).stale(), "no panel edits: the file's frames stay");
    assert!(Store::with_revisions(Some(1), Some(2)).stale(), "an edit makes them stale");
    assert!(!Store::with_revisions(Some(2), Some(2)).stale(), "frames of the edit are fresh");
}

#[test]
fn nothing_of_the_card_bar_covers_the_picture() {
    // EffectCraft and video cards share the bar: buttons, readout and scrub track sit above the
    // card's title, so the picture stays as clean as EffectCraft's Composition panel.
    for pic in [Rect::from_min_size(pos2(100.0, 200.0), egui::vec2(480.0, 270.0)), Rect::from_min_size(pos2(-50.0, 0.0), egui::vec2(160.0, 284.0))] {
        let bar = card_bar(pic).unwrap();
        assert!(bar.zone.max.y <= pic.min.y && bar.row.max.y < pic.min.y - 16.0, "the strip clears the picture and its title");
        for b in [bar.play, bar.second] {
            assert!(b.y + 10.0 < pic.min.y, "{b:?} covers the comp");
            assert!(on_button(b, b + egui::vec2(12.0, 0.0)), "a generous hit zone");
        }
        assert!(bar.play.x < bar.second.x && bar.readout_right < bar.play.x);
        if let Some(track) = bar.track {
            assert!(track.max.y < pic.min.y && track.max.x < bar.readout_right);
            assert!(on_slider(pic, track.center()) && !on_slider(pic, pic.center()), "scrubbed above the card, not on it");
        }
    }
    assert!(card_bar(Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(40.0, 30.0))).is_none(), "too small: no bar");
}

/// Ghostex runs its EffectCraft on its own port (9878 in its sandbox): a Dock launch has no
/// `EFFECTCRAFT_CONTROL_PORT`, so the cards went to Elias's standalone app on 9877. The host's
/// port now wins over the environment's, which wins over 9877.
#[test]
fn a_host_given_effectcraft_port_wins_over_the_environment_and_9877() {
    use crate::effectcraft_ui::{DEFAULT_CONTROL_PORT, resolve_control_port};
    assert_eq!(resolve_control_port(0, None), DEFAULT_CONTROL_PORT);
    assert_eq!(DEFAULT_CONTROL_PORT, 9877);
    assert_eq!(resolve_control_port(0, Some(" 9900 ")), 9900);
    assert_eq!(resolve_control_port(9878, Some("9900")), 9878, "the host wins");
    assert_eq!(resolve_control_port(9878, None), 9878);
    for junk in ["", "abc", "70000", "-1", "0"] {
        assert_eq!(resolve_control_port(0, Some(junk)), DEFAULT_CONTROL_PORT, "{junk:?}");
    }
    // (The setter itself is global state: not set here, so no other test's card reaches a real
    // EffectCraft port through it.)
}

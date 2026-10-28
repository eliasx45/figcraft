use egui::{Rect, pos2, vec2};
use figcraft_doc::EcComp;

use super::*;

#[test]
fn card_pixels_round_trip_through_the_picture() {
    let c = EcComp::new("S1 06 Old vs AI", 0.0, 9.0, 29.97);
    let pic = Rect::from_min_size(pos2(100.0, 50.0), vec2(480.0, 270.0));
    let q = to_comp(pic, &c, pos2(340.0, 185.0));
    assert!((q[0] - 960.0).abs() < 1e-3 && (q[1] - 540.0).abs() < 1e-3, "{q:?}");
    let back = to_screen(pic, &c, q);
    assert!((back.x - 340.0).abs() < 1e-3 && (back.y - 185.0).abs() < 1e-3);
}

#[test]
fn flashes_pulse_three_times_then_stop() {
    assert_eq!(pulse(0.0), Some(1.0));
    assert!(pulse(FLASH * 0.5).is_some_and(|k| k < 0.5));
    assert!(pulse(FLASH * 1.01).is_some_and(|k| k > 0.9), "the second pulse starts bright");
    assert_eq!(pulse(FLASH * PULSES + 0.01), None);
    assert_eq!(pulse(f64::NAN), None);
    assert_eq!(pulse(-1.0), None);
}

#[test]
fn small_cards_get_no_badge() {
    assert!(badge_rect(Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 56.0))).is_none());
    let pic = Rect::from_min_size(pos2(0.0, 0.0), vec2(480.0, 270.0));
    let b = badge_rect(pic).unwrap();
    assert!(pic.contains_rect(b) && b.min.x > pic.center().x, "top right, inside: {b:?}");
}

#[test]
fn a_host_without_a_project_says_so() {
    let mut h = Host::default();
    assert!(h.call("", "figcraft.board", &serde_json::json!({})).is_err());
    assert!(h.call("/nope/never.ecproj", "figcraft.board", &serde_json::json!({})).is_err());
}

#[test]
fn scene_codes() {
    assert_eq!(code("S1 06 Old vs AI"), "S1 06");
    assert_eq!(code("MAIN"), "MAIN");
}

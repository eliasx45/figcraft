//! Video cards: the frame size decoded, the slider, a card's own clock (looping or stopping at
//! its end), where its poster comes from, ⌘-scrubbing, the volume control and frame steps.
#![allow(clippy::unwrap_used)]

use egui::{Rect, pos2};
use figcraft_doc::VideoClip;

use crate::video_ui::{clock, cmd_scrub_time, poster_time, scrolled_volume, side_for, slider_time, step_time, toggle_mute, video_bar, volume_at};

fn clip() -> VideoClip {
    let mut v = VideoClip::new("/clips/a.mp4", 18.0, 1280, 720, 30.0);
    (v.start, v.end) = (2.0, 12.0);
    v
}

#[test]
fn frames_are_decoded_at_the_size_shown_up_to_the_files() {
    let v = clip();
    assert_eq!(side_for(100.0, &v), 240);
    assert_eq!(side_for(300.0, &v), 480);
    assert_eq!(side_for(5000.0, &v), 960);
    let mut small = clip();
    (small.width, small.height) = (320, 180);
    assert_eq!(side_for(900.0, &small), 240, "never more than the file has");
}

#[test]
fn the_slider_runs_over_the_stretch() {
    let v = clip();
    let pic = Rect::from_min_max(pos2(100.0, 0.0), pos2(500.0, 225.0));
    assert_eq!(slider_time(&v, pic, 100.0), 2.0);
    assert_eq!(slider_time(&v, pic, 300.0), 7.0);
    assert_eq!(slider_time(&v, pic, 9000.0), 12.0);
}

#[test]
fn a_cards_own_clock_loops_or_stops_at_the_end() {
    let mut v = clip();
    assert_eq!(clock(&v, 2.0, 3.0), (5.0, 0));
    let (t, k) = clock(&v, 2.0, 13.0);
    assert!((t - 5.0).abs() < 1e-9 && k == 1, "{t} {k}");
    v.looping = false;
    assert_eq!(clock(&v, 2.0, 13.0), (12.0, 0));
    assert_eq!(clock(&v, 2.0, -5.0), (2.0, 0));
}

#[test]
fn posters_come_from_a_little_way_in() {
    let v = clip();
    assert_eq!(poster_time(&v), 3.0);
    let mut short = clip();
    short.end = 3.0;
    assert!((poster_time(&short) - 2.1).abs() < 1e-9);
}

#[test]
fn hovering_alone_never_moves_the_playhead_command_held_scrubs() {
    let v = clip();
    let pic = Rect::from_min_max(pos2(100.0, 100.0), pos2(500.0, 325.0));
    let mid = pos2(300.0, 200.0);
    assert_eq!(cmd_scrub_time(&v, pic, mid, false, false), None, "plain hover");
    assert_eq!(cmd_scrub_time(&v, pic, mid, true, false), Some(7.0), "⌘ held: the time under the pointer");
    assert_eq!(cmd_scrub_time(&v, pic, mid, true, true), None, "⌘-drag is the canvas's");
    assert_eq!(cmd_scrub_time(&v, pic, pos2(50.0, 200.0), true, false), None, "off the picture");
    assert_eq!(cmd_scrub_time(&v, pic, pos2(f32::NAN, 200.0), true, false), None);
}

#[test]
fn the_volume_slider_sits_right_of_the_scrub_track_when_the_file_has_sound() {
    let pic = Rect::from_min_max(pos2(0.0, 100.0), pos2(480.0, 370.0));
    let silent = video_bar(pic, false).unwrap();
    assert!(silent.speaker.is_none() && silent.volume.is_none());
    assert_eq!(silent.track, silent.bar.track);
    let b = video_bar(pic, true).unwrap();
    let (track, sp, vol, full) = (b.track.unwrap(), b.speaker.unwrap(), b.volume.unwrap(), b.bar.track.unwrap());
    assert!(track.max.x < sp.x && sp.x < vol.min.x && vol.max.x == full.max.x, "{track:?} {sp:?} {vol:?}");
    assert!(track.width() >= 40.0);
    assert!(vol.max.y < pic.min.y, "the controls are above the picture, never on it");
    // A narrow card keeps its scrub track and has no volume control.
    let narrow = video_bar(Rect::from_min_max(pos2(0.0, 100.0), pos2(200.0, 212.0)), true).unwrap();
    assert!(narrow.volume.is_none() && narrow.track == narrow.bar.track);
}

#[test]
fn volume_mute_and_wheel() {
    let r = Rect::from_min_max(pos2(100.0, 0.0), pos2(200.0, 10.0));
    assert_eq!(volume_at(r, 150.0), 0.5);
    assert_eq!(volume_at(r, -10.0), 0.0);
    assert_eq!(volume_at(r, 900.0), 1.0);
    assert_eq!(volume_at(r, f32::NAN), 1.0);
    assert_eq!(toggle_mute(0.4, Some(0.4)), 0.0, "mutes");
    assert_eq!(toggle_mute(0.0, Some(0.4)), 0.4, "back to the last level");
    assert_eq!(toggle_mute(0.0, None), 1.0, "full when there was none");
    assert_eq!(toggle_mute(0.0, Some(f32::NAN)), 1.0);
    assert!(scrolled_volume(0.5, 50.0) > 0.5 && scrolled_volume(0.5, -50.0) < 0.5);
    assert_eq!(scrolled_volume(0.9, 1000.0), 1.0);
    assert_eq!(scrolled_volume(0.1, -1000.0), 0.0);
    assert_eq!(scrolled_volume(0.3, f32::INFINITY), 0.3);
}

#[test]
fn arrows_step_one_frame_within_the_stretch() {
    let v = clip();
    assert_eq!(v.frame_of(step_time(&v, 5.0, 1)), 151);
    assert_eq!(v.frame_of(step_time(&v, 5.0, -1)), 149);
    assert_eq!(step_time(&v, 2.0, -1), 2.0, "not before the start");
    assert_eq!(step_time(&v, 12.0, 1), 12.0, "not past the end");
}

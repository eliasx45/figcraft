//! Video cards: the frame size decoded, the slider, a card's own clock (looping or stopping at
//! its end) and where its poster comes from.
#![allow(clippy::unwrap_used)]

use egui::{Rect, pos2};
use figcraft_doc::VideoClip;

use crate::video_ui::{clock, poster_time, side_for, slider_time};

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

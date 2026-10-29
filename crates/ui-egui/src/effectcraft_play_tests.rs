//! A playing card plays as EffectCraft previews: frames asked for ahead only, on the Skip grid,
//! from where the playhead will be; the newest ready frame held, never black.
#![allow(clippy::unwrap_used)]

use figcraft_doc::{EcComp, NodeId};
use figcraft_engine::effectcraft::Span;

use super::{Meter, lead_frames, order, pick};

/// A 25 fps card over comp seconds 0..40 (frames 0..=999).
fn card() -> EcComp {
    EcComp::new("MAIN", 0.0, 40.0, 25.0)
}

fn span(step_frames: f64, looping: bool) -> Span {
    Span { start: 0.0, end: 40.0, step: step_frames / 25.0, looping, speed: 1.0 }
}

/// The frames asked for are all ahead of the playhead (the lead past it), never behind it: the
/// 29 Oct black frames came from asking for half of them behind, already played.
#[test]
fn only_frames_ahead_of_the_playhead_are_asked_for() {
    let c = card();
    let got = order(&c, &span(0.0, true), 100, 6, 4);
    assert_eq!(got, vec![106, 107, 108, 109]);
    assert!(order(&c, &span(0.0, true), 100, 0, 32).iter().all(|f| *f >= 100));
}

/// Skip: only the frames the preview shows, on its grid from the range start.
#[test]
fn skip_asks_only_for_the_frames_shown() {
    let c = card();
    let got = order(&c, &span(3.0, true), 100, 1, 4);
    assert_eq!(got, vec![102, 105, 108, 111], "every third frame, from frame 0");
}

/// Near the end a looping card goes on from the range start (never past where it started); a
/// card that plays once stops at its end.
#[test]
fn the_end_wraps_when_looping_and_stops_when_not() {
    let c = card();
    assert_eq!(order(&c, &span(0.0, true), 997, 0, 5), vec![997, 998, 999, 0, 1]);
    assert_eq!(order(&c, &span(0.0, false), 997, 0, 5), vec![997, 998, 999]);
    // A tiny range is asked for once, whole.
    let short = Span { start: 0.0, end: 0.12, step: 0.0, looping: true, speed: 1.0 };
    assert_eq!(order(&c, &short, 1, 0, 50), vec![1, 2, 0]);
}

/// The playhead's frame when held; else the newest held before it, never one older than the
/// frame on screen; else the frame on screen stays (not black). After a loop round the frame on
/// screen is ahead of the playhead and no longer holds anything back.
#[test]
fn the_newest_ready_frame_is_held() {
    assert_eq!(pick(&[90, 95, 100], 100, Some(95)), Some(100));
    assert_eq!(pick(&[90, 95, 98], 100, Some(95)), Some(98), "the newest ready one");
    assert_eq!(pick(&[90, 95], 100, Some(95)), Some(95), "the frame on screen stays");
    assert_eq!(pick(&[90], 100, Some(95)), None, "older than the frame on screen (and that one gone): not shown");
    assert_eq!(pick(&[2, 3], 4, Some(990)), Some(3), "after a loop round");
    assert_eq!(pick(&[], 4, None), None, "nothing held yet");
    assert_eq!(pick(&[120], 100, None), None, "a frame ahead is not 'ready' for the playhead");
}

/// The lead is the time from asking to having a frame, in comp frames, capped; junk is no lead.
#[test]
fn the_lead_follows_the_measured_latency() {
    assert_eq!(lead_frames(0.2, 25.0), 5);
    assert_eq!(lead_frames(100.0, 25.0), 100, "at most four seconds ahead");
    assert_eq!(lead_frames(f64::NAN, 25.0), 0);
    assert_eq!(lead_frames(0.2, f64::NAN), 0);
    let mut m = Meter::default();
    m.follow(NodeId(7), 1, 25.0, 1);
    let first = m.lead(25.0);
    m.ask(&[10, 11], 1.0);
    m.got(&[10, 11], 1.4);
    assert_eq!(m.lead(25.0), 10, "0.4 s at 25 fps (was the first guess, {first})");
    m.got(&[99], 9.0);
    assert_eq!(m.lead(25.0), 10, "a frame never asked for doesn't count");
}

/// EffectCraft's meter: frames shown at the rate are real time, fewer are not; a new play starts
/// it over.
#[test]
fn the_meter_says_whether_the_card_keeps_real_time() {
    let mut m = Meter::default();
    m.follow(NodeId(7), 1, 25.0, 1);
    for i in 0..26 {
        m.shown(i, f64::from(i as i32) / 25.0);
    }
    let (label, rt) = m.rate().unwrap();
    assert!(rt, "{label}");
    m.follow(NodeId(7), 2, 25.0, 1);
    assert!(m.rate().is_none() && m.last().is_none(), "a new play starts over");
    for i in 0..6 {
        m.shown(i * 5, f64::from(i as i32) / 5.0);
    }
    let (label, rt) = m.rate().unwrap();
    assert!(!rt && label.contains("not real-time"), "{label}");
    m.stop();
    assert!(m.card().is_none());
}

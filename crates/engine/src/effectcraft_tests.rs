//! EffectCraft cards: placed (on their own, or filling an empty frame), read back with their
//! frames, set, played in a loop and sought, given a poster, and the requests the desktop app
//! carries out (explode, reveal).

use figcraft_doc::{NodeId, NodeKind};
use serde_json::{Value, json};

use crate::Session;
use crate::params::id_of;

fn ok(s: &mut Session, cmd: &str, p: Value) -> Value {
    s.execute(cmd, &p).unwrap_or_else(|e| panic!("{cmd} {p}: {e}"))
}

fn card(s: &mut Session, extra: Value) -> NodeId {
    let mut p = json!({"comp": "MAIN", "compId": 282, "start": 1.4, "end": 5.1, "fps": 29.97, "width": 1920, "height": 1080, "x": 0, "y": 0});
    if let (Some(o), Value::Object(e)) = (p.as_object_mut(), extra) {
        o.extend(e);
    }
    id_of(&ok(s, "effectcraft.place", p)["id"]).unwrap()
}

#[test]
fn placing_a_comp_makes_a_card_with_its_frames() {
    let mut s = Session::new();
    let id = card(&mut s, json!({}));
    let r = ok(&mut s, "effectcraft.info", json!({"ids": [id]}));
    let c = &r["cards"][0];
    assert_eq!((c["comp"].clone(), c["compId"].clone()), (json!("MAIN"), json!(282)), "{r}");
    assert_eq!((c["startFrame"].clone(), c["endFrame"].clone()), (json!(41), json!(152)), "{r}");
    assert_eq!((c["cardWidth"].clone(), c["cardHeight"].clone()), (json!(480.0), json!(270.0)), "16:9 at the default width");
    assert_eq!(s.doc().unwrap().node(id).unwrap().name, "MAIN · 0:01–0:05");
    assert!(s.undo().unwrap(), "placing is one undo step");
    assert!(s.doc().unwrap().node(id).is_none());
}

#[test]
fn a_card_fills_the_selected_empty_frame() {
    let mut s = Session::new();
    let f = id_of(&ok(&mut s, "node.create", json!({"type": "frame", "x": 100, "y": 50, "width": 640, "height": 360}))["id"]).unwrap();
    ok(&mut s, "select.set", json!({"ids": [f]}));
    let id = id_of(&ok(&mut s, "effectcraft.place", json!({"comp": "Document page", "start": 0, "end": 3.7}))["id"]).unwrap();
    let doc = s.doc().unwrap();
    let n = doc.node(id).unwrap();
    assert_eq!(n.parent, Some(f));
    assert_eq!((n.width, n.height), (640.0, 360.0));
    assert!(matches!(doc.node(f).unwrap().kind, NodeKind::Frame));
}

#[test]
fn bad_ranges_and_values_are_refused() {
    let mut s = Session::new();
    for p in [
        json!({"comp": "MAIN", "start": 5, "end": 2}),
        json!({"comp": "", "start": 0, "end": 2}),
        json!({"comp": "MAIN", "fps": 0}),
        json!({"comp": "MAIN", "rate": -1}),
        json!({"comp": "MAIN", "width": 0}),
        json!({"comp": "MAIN", "start": 0, "end": 100000}),
        json!({"start": 0, "end": 2}),
    ] {
        assert!(s.execute("effectcraft.place", &p).is_err(), "{p}");
    }
    let id = card(&mut s, json!({}));
    assert!(s.execute("effectcraft.set", &json!({"ids": [id], "start": 9, "end": 1})).is_err());
    let r = ok(&mut s, "effectcraft.set", json!({"ids": [id], "start": 2, "end": 4, "offset": 1.4, "sound": false}));
    assert_eq!((r["cards"][0]["start"].clone(), r["cards"][0]["sound"].clone()), (json!(2.0), json!(false)), "{r}");
}

#[test]
fn the_transport_plays_seeks_and_pauses_a_card() {
    let mut s = Session::new();
    let id = card(&mut s, json!({}));
    let t = ok(&mut s, "effectcraft.seek", json!({"id": id, "frame": 85}));
    assert_eq!(t["frame"], json!(85), "{t}");
    let t = ok(&mut s, "effectcraft.play", json!({"id": id}));
    assert_eq!(t["playing"], json!(true));
    let t = ok(&mut s, "effectcraft.pause", json!({}));
    assert_eq!(t["playing"], json!(false));
    let at = t["time"].as_f64().unwrap();
    assert!((2.83..3.0).contains(&at), "resumes where it was sought: {t}");
    let t = ok(&mut s, "effectcraft.seek", json!({"id": id, "x": 0}));
    assert_eq!(t["time"], json!(1.4), "the left edge is the start");
}

#[test]
fn a_card_keeps_a_poster_frame_without_an_undo_step() {
    let mut s = Session::new();
    let id = card(&mut s, json!({}));
    let png = {
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([200, 30, 40, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    };
    use base64::Engine as _;
    let steps = s.ds().unwrap().history.undo.len();
    let r = ok(&mut s, "effectcraft.setPoster", json!({"id": id, "data": base64::engine::general_purpose::STANDARD.encode(png)}));
    assert!(r["image"].is_string());
    assert_eq!(s.ds().unwrap().history.undo.len(), steps);
    let info = ok(&mut s, "effectcraft.info", json!({"ids": [id]}));
    assert_eq!(info["cards"][0]["poster"], json!(true));
    assert!(s.execute("effectcraft.setPoster", &json!({"id": id, "data": "not base64!"})).is_err());
}

#[test]
fn explode_and_reveal_go_to_the_desktop_app() {
    let mut s = Session::new();
    let id = card(&mut s, json!({}));
    ok(&mut s, "effectcraft.explode", json!({"id": id}));
    ok(&mut s, "effectcraft.reveal", json!({"id": id, "time": 3.0}));
    let kinds: Vec<&str> = s.ui_requests.iter().map(|r| r.kind.as_str()).collect();
    assert_eq!(kinds, ["effectcraft.explode", "effectcraft.reveal"]);
    assert_eq!(s.ui_requests[1].data["comp"], json!("MAIN"));
}

#[test]
fn explode_again_closes_the_cards_it_opened() {
    let mut s = Session::new();
    let main = card(&mut s, json!({}));
    let other = card(&mut s, json!({"x": 0, "y": 900}));
    let page = card(&mut s, json!({"comp": "Document page", "explodedFrom": main, "x": 600}));
    let sheet = card(&mut s, json!({"comp": "Document sheet", "explodedFrom": page, "x": 900}));
    let r = ok(&mut s, "effectcraft.explode", json!({"id": main}));
    assert_eq!(r["collapsed"], json!(2), "{r}");
    let doc = s.doc().unwrap();
    assert!(doc.node(page).is_none() && doc.node(sheet).is_none(), "and the ones exploded out of those");
    assert!(doc.node(main).is_some() && doc.node(other).is_some());
    assert!(s.ui_requests.is_empty(), "closing asks EffectCraft for nothing");
    assert!(s.undo().unwrap(), "closing is one undo step");
    assert!(s.doc().unwrap().node(sheet).is_some());
    // `open: true` asks again even while they are out.
    ok(&mut s, "effectcraft.explode", json!({"id": main, "open": true}));
    assert_eq!(s.ui_requests.len(), 1);
}

/// Regression (Elias, 26 Oct): a card played past its comp's work area (0–2.9 s) instead of
/// looping in it. The loop range is the preview range within the card's stretch; play starts at
/// the current time when it is inside it, else at its start; a card that plays once stops.
#[test]
fn cards_loop_by_effectcrafts_preview_rules() {
    use crate::effectcraft::{Preview, PreviewRange, play_from, play_range, wrap};
    let card = (0.0, 7.0);
    let work = Preview { work: (0.0, 2.9), duration: 7.0, range: PreviewRange::WorkArea, ..Preview::default() };
    // Work area ∩ card.
    assert_eq!(play_range(card, Some(&work), 1.0), (0.0, 2.9));
    assert_eq!(play_range((1.4, 5.1), Some(&work), 2.0), (1.4, 2.9), "the card's stretch cuts the work area");
    assert_eq!(play_range((3.0, 5.0), Some(&work), 3.5), (3.0, 5.0), "no overlap: the card's stretch");
    assert_eq!(play_range(card, None, 1.0), card, "no rules: the card's stretch");
    // Extended by the current time (EffectCraft's default), whole comp, around the current time.
    let ext = Preview { range: PreviewRange::WorkAreaExtended, ..work };
    assert_eq!(play_range(card, Some(&ext), 4.0), (0.0, 4.0));
    assert_eq!(play_range(card, Some(&Preview { range: PreviewRange::Entire, ..work }), 4.0), (0.0, 7.0));
    assert_eq!(play_range(card, Some(&Preview { range: PreviewRange::Around { pre: 1.0, post: 2.0 }, ..work }), 4.0), (3.0, 6.0));
    // Starts: inside the range from the current time; outside it (the playhead at 3:13) from
    // its start; always from the start when the options say so.
    assert_eq!(play_from((0.0, 2.9), Some(&work), Some(1.5)), 1.5);
    assert_eq!(play_from((0.0, 2.9), Some(&work), Some(3.43)), 0.0);
    assert_eq!(play_from((0.0, 2.9), Some(&Preview { from_start: true, ..work }), Some(1.5)), 0.0);
    // Loops in the range; plays once and stops.
    assert_eq!(wrap(2.0, (0.0, 2.9), true), Some(2.0));
    assert!((wrap(3.4, (0.0, 2.9), true).unwrap() - 0.5).abs() < 1e-9);
    assert!((wrap(6.3, (0.5, 2.5), true).unwrap() - 0.5 - (6.3 - 0.5) % 2.0).abs() < 1e-9);
    assert_eq!(wrap(3.4, (0.0, 2.9), false), None);
    // Junk never panics.
    for t in [f64::NAN, f64::INFINITY, -1.0] {
        let r = play_range(card, Some(&Preview { work: (f64::NAN, f64::INFINITY), duration: f64::NAN, ..work }), t);
        assert!(r.0 <= r.1);
        let _ = play_from(r, Some(&work), Some(t));
        let _ = wrap(t, r, true);
    }
}

/// The transport plays the card by its rules: from the work-area start when the playhead is
/// past it, and a work area changed mid-play is the loop's at once.
#[test]
fn the_transport_follows_the_work_area() {
    use crate::effectcraft::{Preview, PreviewRange};
    let mut s = Session::new();
    let id = card(&mut s, json!({"start": 0, "end": 7}));
    let c = s.doc().unwrap().node(id).unwrap().effectcraft.as_deref().cloned().unwrap();
    let rules = Preview { work: (0.0, 2.9), duration: 7.0, range: PreviewRange::WorkArea, ..Preview::default() };
    s.effectcraft.set_preview(id, Some(rules));
    s.effectcraft.play(id, &c, Some(3.43));
    assert!(s.effectcraft.time() < 0.5, "started at the work area: {}", s.effectcraft.time());
    s.effectcraft.set_preview(id, Some(Preview { work: (1.0, 2.0), ..rules }));
    let t = s.effectcraft.time();
    assert!((1.0..2.0).contains(&t), "the new work area at once: {t}");
    assert!(s.effectcraft.playing());
}

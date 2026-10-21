//! Video cards: placed (from what the file holds, or given sizes where it can't be read), read
//! back, set, played in a loop and sought, and "play all visible".

use figcraft_doc::NodeId;
use serde_json::{Value, json};

use crate::Session;
use crate::params::id_of;

fn ok(s: &mut Session, cmd: &str, p: Value) -> Value {
    s.execute(cmd, &p).unwrap_or_else(|e| panic!("{cmd} {p}: {e}"))
}

/// A card for a file that isn't there, with its sizes given (as on a system without video).
fn card(s: &mut Session) -> NodeId {
    let p = json!({"path": "/nowhere/clip.mp4", "duration": 18.0, "width": 1280, "height": 720, "fps": 30, "x": 0, "y": 0});
    id_of(&ok(s, "video.place", p)["id"]).unwrap()
}

#[test]
fn placing_a_video_makes_a_card_the_files_shape() {
    let mut s = Session::new();
    let id = card(&mut s);
    let c = &ok(&mut s, "video.info", json!({"ids": [id]}))["cards"][0];
    assert_eq!((c["file"].clone(), c["start"].clone(), c["end"].clone()), (json!("clip.mp4"), json!(0.0), json!(18.0)), "{c}");
    assert_eq!((c["cardWidth"].clone(), c["cardHeight"].clone()), (json!(480.0), json!(270.0)));
    assert_eq!(s.doc().unwrap().node(id).unwrap().name, "clip");
    assert!(s.undo().unwrap(), "placing is one undo step");
    assert!(s.doc().unwrap().node(id).is_none());
}

#[test]
fn a_file_that_cant_be_read_needs_its_sizes() {
    let mut s = Session::new();
    assert!(s.execute("video.place", &json!({"path": "/nowhere/clip.mp4"})).is_err());
    assert!(s.execute("video.place", &json!({})).is_err());
}

#[test]
fn the_stretch_stays_within_the_file() {
    let mut s = Session::new();
    let id = card(&mut s);
    let r = ok(&mut s, "video.set", json!({"ids": [id], "start": 2, "end": 99, "loop": false, "sound": false}));
    let c = &r["cards"][0];
    assert_eq!(
        (c["start"].clone(), c["end"].clone(), c["loop"].clone(), c["sound"].clone()),
        (json!(2.0), json!(18.0), json!(false), json!(false)),
        "{c}"
    );
    for bad in [json!({"start": 5, "end": 1}), json!({"start": 30}), json!({"start": -1})] {
        let mut p = bad.clone();
        p["ids"] = json!([id]);
        assert!(s.execute("video.set", &p).is_err(), "{bad}");
    }
}

#[test]
fn the_transport_plays_seeks_and_pauses_a_card() {
    let mut s = Session::new();
    let id = card(&mut s);
    let t = ok(&mut s, "video.seek", json!({"id": id, "time": 4.5}));
    assert_eq!((t["time"].clone(), t["frame"].clone()), (json!(4.5), json!(135)), "{t}");
    assert_eq!(ok(&mut s, "video.togglePlay", json!({"id": id}))["playing"], json!(true));
    let t = ok(&mut s, "video.pause", json!({}));
    let at = t["time"].as_f64().unwrap();
    assert!((4.5..4.8).contains(&at), "resumes where it was sought: {t}");
    assert_eq!(ok(&mut s, "video.seek", json!({"id": id, "x": 240}))["time"], json!(9.0), "the middle of the card is the middle of the clip");
    assert_eq!(ok(&mut s, "video.playAll", json!({}))["playAll"], json!(true));
    assert_eq!(ok(&mut s, "video.playAll", json!({"on": false}))["playAll"], json!(false));
}

#[test]
fn importing_a_missing_video_is_an_error_not_a_crash() {
    let mut s = Session::new();
    assert!(s.execute("file.import", &json!({"path": "/nowhere/clip.mp4"})).is_err());
}

#[test]
fn a_card_plays_once_unless_it_loops() {
    let mut s = Session::new();
    let id = card(&mut s);
    assert_eq!(ok(&mut s, "video.info", json!({"ids": [id]}))["cards"][0]["loop"], json!(false));
    let end = ok(&mut s, "video.info", json!({"ids": [id]}))["cards"][0]["end"].as_f64().unwrap();
    ok(&mut s, "video.play", json!({"id": id, "time": end - 0.001}));
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(s.video.ended() && !s.video.playing(), "it stops at its end");
    let t = ok(&mut s, "video.togglePlay", json!({"id": id}));
    assert_eq!((t["playing"].clone(), t["time"].as_f64().unwrap() < 1.0), (json!(true), true), "Space plays it over again: {t}");
}

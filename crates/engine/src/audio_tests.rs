//! Audio layers: placed from WAV bytes or a file, read back with their frames, range and markers
//! set (one undo step each), and the transport played, sought and paused.

use base64::Engine as _;
use figcraft_doc::NodeId;
use serde_json::{Value, json};

use crate::Session;
use crate::params::id_of;

fn ok(s: &mut Session, cmd: &str, p: Value) -> Value {
    s.execute(cmd, &p).unwrap_or_else(|e| panic!("{cmd} {p}: {e}"))
}

fn wav(seconds: f64) -> String {
    base64::engine::general_purpose::STANDARD.encode(figcraft_audio::tone_wav(seconds, 440.0, 48_000, 0.8))
}

fn place(s: &mut Session, extra: Value) -> (NodeId, Value) {
    let mut p = json!({"data": wav(2.0), "name": "voice.wav", "x": 10, "y": 20});
    if let (Some(o), Value::Object(e)) = (p.as_object_mut(), extra) {
        o.extend(e);
    }
    let r = ok(s, "audio.place", p);
    (id_of(&r["id"]).unwrap(), r)
}

#[test]
fn placing_a_sound_makes_a_waveform_strip() {
    let mut s = Session::new();
    let (id, r) = place(&mut s, json!({}));
    assert!((r["duration"].as_f64().unwrap() - 2.0).abs() < 1e-3, "{r}");
    assert_eq!(
        (r["sampleRate"].clone(), r["channels"].clone(), r["width"].clone(), r["height"].clone()),
        (json!(48_000), json!(1), json!(200.0), json!(96.0))
    );
    let doc = s.doc().unwrap();
    let n = doc.node(id).unwrap();
    let a = n.audio.as_deref().unwrap();
    assert_eq!(a.peaks.0.len(), 400, "200 peaks a second");
    assert!(doc.audio.contains_key(&a.asset));
    assert_eq!(s.selection(), vec![id]);
    // The layer inspector shows the clip without its envelope.
    let v = crate::inspect::node_json(s.doc().unwrap(), id, 0);
    assert_eq!(v["audio"]["file"], "voice.wav");
    assert!(v["audio"].get("peaks").is_none());
}

#[test]
fn frames_count_from_the_offset_at_the_ruler_rate() {
    let mut s = Session::new();
    // A clip that starts 42 s into the edit at 29.97 fps: media 0 is frame 1259.
    let (id, _) = place(&mut s, json!({"fps": 29.97, "offset": 42.0, "markers": [{"frame": 1281, "label": "CON"}, {"time": 0.25}]}));
    let c = &ok(&mut s, "audio.info", json!({"ids": [id]}))["clips"][0];
    assert_eq!((c["startFrame"].clone(), c["endFrame"].clone()), (json!(1259), json!(1319)));
    assert_eq!(c["markers"][0]["frame"], 1266, "sorted by time: 0.25 s first");
    assert_eq!(c["markers"][1]["label"], "CON");
    assert_eq!(c["markers"][1]["frame"], 1281);
    assert!((c["markers"][1]["time"].as_f64().unwrap() - (1281.0 / 29.97 - 42.0)).abs() < 1e-6);
}

#[test]
fn markers_are_added_replaced_removed_and_undone() {
    let mut s = Session::new();
    let (id, _) = place(&mut s, json!({}));
    let m = ok(&mut s, "audio.addMarker", json!({"ids": [id], "time": 1.5, "label": "peak", "color": "#FF0000"}));
    assert_eq!(m["markers"][0]["index"], 0);
    ok(&mut s, "audio.addMarker", json!({"time": 0.5, "label": "k"}));
    let labels = |s: &mut Session| -> Vec<String> {
        ok(s, "audio.info", json!({"ids": [id]}))["clips"][0]["markers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["label"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(labels(&mut s), ["k", "peak"]);
    ok(&mut s, "edit.undo", json!({}));
    assert_eq!(labels(&mut s), ["peak"]);
    ok(&mut s, "audio.setMarkers", json!({"markers": [{"time": 0.1, "label": "a"}, {"time": 0.2, "label": "b"}, {"time": 0.3, "label": "a"}]}));
    assert_eq!(ok(&mut s, "audio.removeMarker", json!({"label": "a"}))["removed"], 2);
    assert_eq!(ok(&mut s, "audio.removeMarker", json!({"index": 0}))["removed"], 1);
    assert!(labels(&mut s).is_empty());
    // A marker needs a time (or the playhead on the clip).
    assert!(s.execute("audio.addMarker", &json!({"label": "x"})).is_err());
    ok(&mut s, "audio.seek", json!({"time": 1.25}));
    let m = ok(&mut s, "audio.addMarker", json!({"label": "here"}));
    assert_eq!(m["markers"][0]["time"], 1.25);
}

#[test]
fn the_range_and_width_are_set() {
    let mut s = Session::new();
    let (id, _) = place(&mut s, json!({"fps": 30}));
    let c = &ok(&mut s, "audio.set", json!({"startFrame": 15, "end": 1.5, "pxPerSecond": 400}))["clips"][0];
    assert_eq!((c["start"].clone(), c["end"].clone(), c["width"].clone()), (json!(0.5), json!(1.5), json!(400.0)));
    assert!(s.execute("audio.set", &json!({"start": 1.0, "end": 0.5})).is_err());
    assert!(s.execute("audio.set", &json!({"fps": 0})).is_err());
    // Not an audio layer.
    let r = id_of(&ok(&mut s, "shape.rectangle", json!({"x": 0, "y": 0, "width": 10, "height": 10}))["id"]).unwrap();
    assert!(s.execute("audio.set", &json!({"ids": [r], "start": 0})).is_err());
    let _ = id;
}

#[test]
fn the_transport_plays_seeks_and_pauses() {
    let mut s = Session::new();
    let (id, _) = place(&mut s, json!({}));
    let t = ok(&mut s, "audio.play", json!({"id": id, "time": 0.5}));
    assert_eq!(t["playing"], true);
    assert!(t["time"].as_f64().unwrap() >= 0.5);
    let t = ok(&mut s, "audio.pause", json!({}));
    assert_eq!(t["playing"], false);
    // x 150 of a 200-wide strip of 2 s = 1.5 s.
    let t = ok(&mut s, "audio.seek", json!({"x": 150}));
    assert_eq!((t["time"].clone(), t["id"].clone()), (json!(1.5), json!(id)));
    assert_eq!(ok(&mut s, "audio.togglePlay", json!({}))["playing"], true);
    assert_eq!(ok(&mut s, "audio.togglePlay", json!({}))["playing"], false);
    // Out of range seeks clamp to the clip.
    assert_eq!(ok(&mut s, "audio.seek", json!({"time": 99}))["time"], 2.0);
}

#[test]
fn importing_a_sound_file_places_it() {
    let dir = std::env::temp_dir().join(format!("figcraft-audio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Take 1.wav");
    std::fs::write(&path, figcraft_audio::tone_wav(1.0, 220.0, 44_100, 0.5)).unwrap();
    let mut s = Session::new();
    let r = ok(&mut s, "file.import", json!({"path": path.to_string_lossy(), "x": 0, "y": 0}));
    assert_eq!(r["file"], "Take 1.wav");
    let id = id_of(&r["id"]).unwrap();
    assert!(s.doc().unwrap().node(id).unwrap().audio.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn junk_sounds_are_refused() {
    let mut s = Session::new();
    let junk = base64::engine::general_purpose::STANDARD.encode(b"not a sound at all");
    assert!(s.execute("audio.place", &json!({"data": junk, "name": "x.mp3"})).is_err());
    assert!(s.execute("audio.place", &json!({})).is_err());
    assert!(s.execute("audio.info", &json!({})).is_err(), "no audio layers");
}

#[test]
fn a_speech_map_marks_the_stressed_syllables() {
    let mut s = Session::new();
    // The sound starts 42 s into the edit; the map is in timeline seconds.
    let (id, _) = place(&mut s, json!({"fps": 29.97, "offset": 42.0}));
    let map = json!([
        {"word": "content", "start": 42.665, "end": 43.1, "syllables": [
            {"t": 42.67, "vowel": 42.78, "stress": 1}, {"t": 42.9, "vowel": 42.95, "stress": 0}]},
        {"word": "pattern", "start": 43.1, "end": 43.5, "syllables": [{"t": 43.1, "vowel": 43.2, "stress": 2}]}
    ]);
    assert_eq!(ok(&mut s, "audio.importSpeechMap", json!({"map": map}))["count"], 1);
    let c = &ok(&mut s, "audio.info", json!({"ids": [id]}))["clips"][0];
    assert_eq!((c["markers"][0]["label"].clone(), c["markers"][0]["frame"].clone()), (json!("content"), json!(1279)));
    // The peak: a frame before the vowel (frame 1281 for "CON-tent"); secondary stress too.
    assert_eq!(ok(&mut s, "audio.importSpeechMap", json!({"map": {"words": map}, "at": "peak", "stress": 2}))["count"], 2);
    let c = &ok(&mut s, "audio.info", json!({"ids": [id]}))["clips"][0];
    assert_eq!(c["markers"][0]["frame"], 1281);
    assert!(s.execute("audio.importSpeechMap", &json!({"map": 5})).is_err());
    assert!(s.execute("audio.importSpeechMap", &json!({"map": [], "at": "nowhere"})).is_err());
}

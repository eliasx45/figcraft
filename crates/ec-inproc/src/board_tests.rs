use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;
use crate::InProc;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("figcraft-ec-board-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn new_comp(s: &mut Session, name: &str, dur: f64) -> u64 {
    s.execute("comp.new", json!({"name": name, "width": 64, "height": 36, "duration": dur, "frameRate": 25})).unwrap();
    let id = s.active_comp_id().unwrap().0;
    s.execute("layer.newSolid", json!({"comp": id, "color": "#ff0000", "width": 64, "height": 36})).unwrap();
    id
}

/// MAIN (20 s, chapters HOOK at 0 and IDEA at 10) holding scene A at 2 s, scene B at 12 s and a
/// part comp P (15 s) at 5 s that holds scene C at 1 s.
fn project(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("p.ecproj");
    let mut s = crate::new_session();
    let a = new_comp(&mut s, "S1 01 Alpha", 3.0);
    let b = new_comp(&mut s, "S1 02 Beta", 4.0);
    let c = new_comp(&mut s, "S1 03 Gamma", 2.0);
    let p = new_comp(&mut s, "26-10 P1", 15.0);
    s.execute("layer.addItem", json!({"comp": p, "item": c, "time": 1.0})).unwrap();
    let main = new_comp(&mut s, "MAIN", 20.0);
    s.execute("layer.addItem", json!({"comp": main, "item": a, "time": 2.0})).unwrap();
    s.execute("layer.addItem", json!({"comp": main, "item": b, "time": 12.0})).unwrap();
    s.execute("layer.addItem", json!({"comp": main, "item": p, "time": 5.0})).unwrap();
    s.execute("comp.addMarker", json!({"comp": main, "time": 0.0, "comment": "HOOK"})).unwrap();
    s.execute("comp.addMarker", json!({"comp": main, "time": 10.0, "comment": "IDEA · faceless with AI"})).unwrap();
    s.execute("file.saveAs", json!({"path": path.to_str().unwrap()})).unwrap();
    path
}

#[test]
fn the_board_lists_mains_scenes_through_part_comps_with_their_main_times() {
    let dir = temp("scenes");
    let path = project(&dir);
    std::fs::write(dir.join("board.json"), r#"[{"name": "HOOK", "beats": [{"start": 2.5, "end": 3, "text": "sixty days", "stress": "SIX"}]}]"#)
        .unwrap();
    let mut ec = InProc::open(&path).unwrap();
    let b = ec.call("figcraft.board", &json!({})).unwrap();
    assert_eq!(b["main"]["name"], "MAIN");
    let names: Vec<&str> = b["scenes"].as_array().unwrap().iter().map(|s| s["compName"].as_str().unwrap()).collect();
    assert_eq!(names, ["S1 01 Alpha", "S1 03 Gamma", "S1 02 Beta"], "{b}");
    let gamma = &b["scenes"][1];
    assert_eq!(gamma["parent"], "26-10 P1");
    assert!((gamma["mainStart"].as_f64().unwrap() - 6.0).abs() < 1e-3, "{gamma}");
    assert!((gamma["mainEnd"].as_f64().unwrap() - 8.0).abs() < 1e-3, "{gamma}");
    assert!((gamma["offset"].as_f64().unwrap() - 6.0).abs() < 1e-3, "comp second 0 shows at MAIN 6 s: {gamma}");
    assert!((gamma["compStart"].as_f64().unwrap()).abs() < 1e-3);
    let ch = b["chapters"].as_array().unwrap();
    assert_eq!(ch.len(), 2);
    assert_eq!(ch[1]["name"], "IDEA · faceless with AI");
    assert!((ch[0]["end"].as_f64().unwrap() - 10.0).abs() < 1e-3);
    assert_eq!(b["beats"][0]["stress"], "SIX");
}

#[test]
fn without_a_main_comp_the_longest_comp_is_main() {
    let dir = temp("longest");
    let path = dir.join("p.ecproj");
    let mut s = crate::new_session();
    new_comp(&mut s, "short", 2.0);
    new_comp(&mut s, "Long one", 9.0);
    s.execute("file.saveAs", json!({"path": path.to_str().unwrap()})).unwrap();
    let mut ec = InProc::open(&path).unwrap();
    let b = ec.call("figcraft.board", &json!({})).unwrap();
    assert_eq!(b["main"]["name"], "Long one");
    assert_eq!(b["chapters"].as_array().unwrap().len(), 1, "one chapter without markers: {b}");
}

#[test]
fn marks_and_layer_picks_measure_through_effectcrafts_annotate() {
    let dir = temp("marks");
    let path = project(&dir);
    let mut ec = InProc::open(&path).unwrap();
    let main = ec.call("figcraft.board", &json!({})).unwrap()["main"]["id"].as_u64().unwrap();
    let alpha = ec.call("figcraft.board", &json!({})).unwrap()["scenes"][0]["comp"].as_u64().unwrap();
    let m = ec.call("figcraft.mark", &json!({"comp": alpha, "time": 0.5, "tool": "comment", "point": [32, 18], "note": "bigger"})).unwrap();
    assert_eq!(m["mark"]["note"], "bigger");
    assert_eq!(m["mark"]["frame"], 12);
    assert!(m["mark"]["copyRef"].as_str().unwrap().contains("S1 01 Alpha"));
    // The mark left EffectCraft's own queue.
    let left = ec.session_mut().execute("annotate.list", json!({})).unwrap();
    assert!(left["marks"].as_array().unwrap().is_empty());
    let hit = ec.call("figcraft.layerAt", &json!({"comp": main, "time": 2.5, "x": 10, "y": 10})).unwrap();
    assert!(hit["layer"]["path"].as_array().is_some_and(|p| !p.is_empty()), "{hit}");
    let info = ec.call("figcraft.comp", &json!({"comp": alpha, "layers": 1})).unwrap();
    assert_eq!(info["parents"][0], "MAIN");
    assert_eq!(info["layers"].as_array().unwrap().len(), 1);
    assert!(ec.call("figcraft.moves", &json!({"comp": alpha})).unwrap()["moves"].is_array());
    let words = ec.call("figcraft.speech", &json!({})).unwrap();
    assert_eq!(words["missing"], true, "no speech map yet: {words}");
}

#[test]
fn junk_never_panics() {
    let dir = temp("junk");
    let path = project(&dir);
    let mut ec = InProc::open(&path).unwrap();
    let junk = [
        json!(null),
        json!([1]),
        json!({"comp": "nope"}),
        json!({"comp": 1e308, "x": "a", "points": [[1]], "time": "NaN"}),
        json!({"comp": -1, "layers": -5}),
    ];
    for m in ["figcraft.board", "figcraft.speech", "figcraft.mark", "figcraft.layerAt", "figcraft.comp", "figcraft.moves"] {
        for j in &junk {
            let _ = ec.call(m, j);
        }
    }
    assert!(beats_of(&json!({"sections": [{"beats": [{"start": "x"}, {"start": 1, "end": -5}]}]})).len() == 1);
    assert!(beats_of(&Value::Null).is_empty());
}

#[test]
fn maps_compose_through_stretch() {
    let m = Map::MAIN.child(5.0, 200.0).unwrap();
    assert!((m.main(2.0) - 9.0).abs() < 1e-9, "a 200 % stretch shows comp second 2 at MAIN 5 + 4");
    assert!((m.comp(9.0) - 2.0).abs() < 1e-9);
    assert!(Map::MAIN.child(0.0, 0.0).is_none());
}

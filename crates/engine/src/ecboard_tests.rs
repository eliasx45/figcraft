//! The MAIN board commands (`ecboard`) against a stand-in EffectCraft host.

use serde_json::{Value, json};

use crate::ecboard::{self, EcHost};
use crate::{EngineError, Session};

const PROJECT: &str = "/tmp/26-10.ecproj";

/// Answers as `figcraft_ec_inproc` does, from fixed data: MAIN (60 s, chapters HOOK 0–20 and
/// IDEA 20–60) with three scenes; `scenes` can be changed between calls.
struct Fake {
    scenes: Vec<Value>,
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

fn scene(id: u64, name: &str, a: f64, b: f64) -> Value {
    json!({"comp": id, "compName": name, "layer": name, "parent": "26-10 P1", "mainStart": a, "mainEnd": b,
        "compStart": 0.0, "compEnd": b - a, "offset": a, "rate": 1.0, "width": 1920, "height": 1080, "fps": 29.97, "duration": b - a})
}

impl EcHost for Fake {
    fn call(&mut self, project: &str, method: &str, p: &Value) -> Result<Value, String> {
        if let Ok(mut c) = self.calls.lock() {
            c.push(method.to_string());
        }
        if project != PROJECT {
            return Err(format!("no project {project}"));
        }
        match method {
            "figcraft.board" => Ok(json!({
                "project": PROJECT,
                "main": {"id": 1, "name": "MAIN", "width": 1920, "height": 1080, "fps": 29.97, "duration": 60.0},
                "chapters": [{"time": 0.0, "end": 20.0, "name": "HOOK"}, {"time": 20.0, "end": 60.0, "name": "IDEA · faceless with AI"}],
                "scenes": self.scenes,
                "beats": [{"time": 25.5, "end": 26.0, "text": "sixty days", "stress": "SIX", "section": "IDEA"}],
            })),
            "figcraft.speech" => Ok(
                json!({"comp": 1, "words": [[2.0, 2.4, "in", false], [2.4, 3.0, "content", true], [25.4, 25.9, "sixty", true], [26.0, 26.3, "days", false]]}),
            ),
            "figcraft.mark" => Ok(json!({"mark": {"id": 9, "frame": (p["time"].as_f64().unwrap_or(0.0) * 29.97).floor(), "timecode": "0:00:01:00",
                "layers": [{"id": 4, "name": "S1 06 N A1 W1 T", "comp": 38382, "compName": "S1 06 Old vs AI", "top": 4, "path": ["S1 06 Old vs AI", "S1 06 N A1 W1 T"]}],
                "compSize": [1920, 1080], "copyRef": format!("{} · 0:00:01:00 (f30) · 💬 comment", p["comp"])}})),
            "figcraft.layerAt" => Ok(
                json!({"layer": {"id": 5, "name": "Pill", "comp": 2, "compName": "Inner", "top": 3, "path": ["S1 06 Old vs AI", "Row 2", "Pill"]}}),
            ),
            "figcraft.comp" => Ok(
                json!({"id": p["comp"], "name": "S1 06 Old vs AI", "parents": ["26-10 P1"], "width": 1920, "height": 1080, "fps": 29.97, "duration": 9.31,
                "layerCount": 40, "layers": [{"name": "S1 06 CTRL", "type": "Null", "in": 0, "out": 9.31, "keys": 0, "visible": false, "label": "Red"}], "moreLayers": 39}),
            ),
            "figcraft.moves" => Ok(
                json!({"comp": p["comp"], "moves": [{"layer": "Card", "property": "Position", "from": {"time": 0.1, "frame": 3}, "to": {"time": 0.6, "frame": 18}, "curve": "Fast In"}]}),
            ),
            other => Err(format!("unknown method {other}")),
        }
    }
}

fn sess() -> (Session, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut s = Session::new();
    s.ecboard.host = Some(Box::new(Fake {
        scenes: vec![scene(10, "HV1 01 Calendar", 0.5, 2.7), scene(11, "S1 05 Spotlight how", 21.0, 26.5), scene(12, "S1 06 Old vs AI", 26.5, 35.0)],
        calls: calls.clone(),
    }));
    (s, calls)
}

fn auto(s: &mut Session) -> Value {
    s.execute("board.autoMain", &json!({"ecproj": PROJECT, "x": 0, "y": 0})).unwrap()
}

#[test]
fn auto_main_lays_out_one_row_per_chapter_and_a_card_per_scene() {
    let (mut s, _) = sess();
    let r = auto(&mut s);
    assert_eq!(r["rows"].as_array().unwrap().len(), 2);
    assert_eq!(r["rows"][0]["cards"].as_array().unwrap().len(), 1);
    assert_eq!(r["rows"][1]["cards"].as_array().unwrap().len(), 2);
    let st = s.execute("board.state", &json!({})).unwrap();
    let idea = &st["rows"][1];
    assert_eq!(idea["name"], "IDEA · faceless with AI");
    assert_eq!(idea["cards"][1]["label"], "S1 06 Old vs AI · 0:26");
    // Cards go left to right in time order, on one line.
    let (a, b) = (&idea["cards"][0]["bounds"], &idea["cards"][1]["bounds"]);
    assert!(a[0].as_f64().unwrap() < b[0].as_f64().unwrap());
    assert_eq!(a[1], b[1]);
    // A card plays its comp's stretch and maps it onto MAIN.
    let id = idea["cards"][1]["card"].as_u64().unwrap();
    let info = s.execute("effectcraft.info", &json!({"ids": [id]})).unwrap();
    assert_eq!(info["cards"][0]["compId"], 12);
    assert!((info["cards"][0]["offset"].as_f64().unwrap() - 26.5).abs() < 1e-6);
    // One undo step takes the whole board away.
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.execute("board.state", &json!({})).unwrap()["cards"], 0);
}

#[test]
fn running_auto_main_again_updates_in_place() {
    let (mut s, _) = sess();
    let first = auto(&mut s);
    let kept = first["rows"][1]["cards"][0].clone();
    // A scene went, another came.
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    s.ecboard.host =
        Some(Box::new(Fake { scenes: vec![scene(11, "S1 05 Spotlight how", 21.0, 26.5), scene(13, "S1 07 Avatar", 36.0, 44.0)], calls }));
    let again = s.execute("board.autoMain", &json!({"ecproj": PROJECT})).unwrap();
    assert_eq!(again["removed"], 2, "{again}");
    assert_eq!(again["added"].as_array().unwrap().len(), 1);
    assert_eq!(again["rows"][1]["cards"][0], kept, "the kept card keeps its id");
    let st = s.execute("board.state", &json!({})).unwrap();
    assert_eq!(st["cards"], 2, "never duplicated");
    let titles = s.doc().unwrap().nodes().filter(|n| n.plugin_data.get(ecboard::KIND).is_some_and(|k| k == "title")).count();
    assert_eq!(titles, 1);
}

#[test]
fn fly_to_a_comp_a_chapter_or_a_main_time() {
    let (mut s, _) = sess();
    auto(&mut s);
    let r = s.execute("canvas.flyTo", &json!({"comp": "S1 06 Old vs AI"})).unwrap();
    let card = r["card"].as_u64().unwrap();
    let ds = s.ds().unwrap();
    assert!((ds.view.cx - r["to"]["cx"].as_f64().unwrap()).abs() < 1e-9, "the view is set at once");
    assert_eq!(ds.selection.first().map(|n| n.0), Some(card));
    assert!(s.ui_requests.iter().any(|u| u.kind == "canvas.flyTo"), "the app glides there");
    let t = s.execute("canvas.flyTo", &json!({"time": 22.0})).unwrap();
    let st = s.execute("board.state", &json!({})).unwrap();
    assert_eq!(t["card"], st["rows"][1]["cards"][0]["card"], "MAIN 0:22 is S1 05");
    assert!(s.execute("canvas.flyTo", &json!({"chapter": "idea"})).is_ok());
    assert!(s.execute("canvas.flyTo", &json!({"time": 59.0})).is_err(), "no scene there");
    assert!(s.execute("canvas.flyTo", &json!({})).is_err());
}

#[test]
fn selecting_a_card_is_an_event_the_host_polls() {
    let (mut s, _) = sess();
    auto(&mut s);
    let since = s.execute("board.events", &json!({})).unwrap()["last"].as_u64().unwrap();
    let st = s.execute("board.state", &json!({})).unwrap();
    let card = st["rows"][0]["cards"][0]["card"].clone();
    s.execute("select.set", &json!({"ids": [card]})).ok();
    if s.selection().is_empty() {
        s.ds_mut().unwrap().selection = vec![crate::params::id_of(&card).unwrap()];
    }
    let ev = s.execute("board.events", &json!({"since": since})).unwrap();
    let sel = ev["events"].as_array().unwrap().iter().find(|e| e["kind"] == "select").cloned().unwrap();
    assert_eq!(sel["data"]["card"], card);
    assert_eq!(sel["data"]["comp"], "HV1 01 Calendar");
    let st = s.execute("board.state", &json!({})).unwrap();
    assert_eq!(st["selection"]["card"], card);
}

#[test]
fn marks_measure_through_effectcraft_and_queue_in_session_state() {
    let (mut s, _) = sess();
    auto(&mut s);
    let st = s.execute("board.state", &json!({})).unwrap();
    let card = st["rows"][1]["cards"][1]["card"].clone();
    let rev = s.ds().unwrap().revision;
    s.execute("canvas.annotate.start", &json!({"tool": "comment"})).unwrap();
    assert_eq!(s.execute("board.state", &json!({})).unwrap()["armed"], "comment");
    let m = s.execute("canvas.annotate.add", &json!({"card": card, "time": 1.0, "point": [960, 540], "note": "bigger"})).unwrap();
    let mark = &m["mark"];
    assert_eq!(mark["tool"], "comment");
    assert_eq!(mark["note"], "bigger");
    assert_eq!(mark["layers"][0]["name"], "S1 06 N A1 W1 T");
    assert!((mark["mainTime"].as_f64().unwrap() - 27.5).abs() < 0.05, "{mark}");
    assert!(mark["copyRef"].as_str().unwrap().contains("MAIN 0:27"), "{mark}");
    // Layer pick.
    let l = s.execute("canvas.annotate.add", &json!({"card": card, "time": 1.0, "tool": "layer", "x": 10, "y": 10})).unwrap();
    assert!(l["mark"]["copyRef"].as_str().unwrap().contains("Row 2 › Pill"), "{l}");
    let list = s.execute("canvas.annotate.list", &json!({"since": mark["id"]})).unwrap();
    assert_eq!(list["marks"].as_array().unwrap().len(), 1);
    // Never the document.
    assert_eq!(s.ds().unwrap().revision, rev);
    let agent = s.execute("board.agentView", &json!({"card": card})).unwrap();
    assert_eq!(agent["marks"].as_array().unwrap().len(), 2);
    s.execute("canvas.annotate.remove", &json!({"id": mark["id"]})).unwrap();
    assert_eq!(s.execute("canvas.annotate.clear", &json!({})).unwrap()["removed"], 1);
    s.execute("canvas.annotate.stop", &json!({})).unwrap();
    assert_eq!(s.execute("board.state", &json!({})).unwrap()["armed"], Value::Null);
}

#[test]
fn agent_view_reads_the_card_for_claude() {
    let (mut s, _) = sess();
    auto(&mut s);
    let st = s.execute("board.state", &json!({})).unwrap();
    let card = st["rows"][1]["cards"][0]["card"].clone();
    let a = s.execute("board.agentView", &json!({"card": card})).unwrap();
    assert_eq!(a["chapter"], "IDEA · faceless with AI");
    assert_eq!(a["comp"]["parent"], "26-10 P1");
    assert_eq!(a["main"]["frames"][0], (21.0_f64 * 29.97).round());
    assert_eq!(a["stress"]["word"], "six", "the board.json beat wins: {a}");
    assert_eq!(a["stress"]["from"], "board.json beat");
    assert_eq!(a["speech"], "sixty days");
    assert_eq!(a["layers"][0]["name"], "S1 06 CTRL");
}

#[test]
fn card_flash_names_the_comps_cards() {
    let (mut s, _) = sess();
    auto(&mut s);
    let r = s.execute("card.flash", &json!({"comp": "HV1 01 Calendar"})).unwrap();
    assert_eq!(r["cards"].as_array().unwrap().len(), 1);
    assert_eq!(r["before"], true);
    assert!(s.ui_requests.iter().any(|u| u.kind == "card.flash"));
    assert!(s.execute("card.flash", &json!({"comp": "nope"})).is_err());
    let id = crate::params::id_of(&r["cards"][0]).unwrap();
    assert!(s.ecboard.flash.of(id).is_some());
    s.execute("card.flashDismiss", &json!({"card": r["cards"][0]})).unwrap();
    assert!(s.ecboard.flash.of(id).is_none());
}

#[test]
fn storyboard_and_review_boards_go_beside_the_board() {
    let (mut s, _) = sess();
    auto(&mut s);
    let sb = s.execute("board.storyboard", &json!({"row": "IDEA"})).unwrap();
    assert_eq!(sb["shots"].as_array().unwrap().len(), 2);
    assert_eq!(sb["shots"][0]["stress"]["word"], "six");
    assert_eq!(sb["shots"][0]["note"], "", "no fake notes");
    // Again: replaced, not stacked.
    s.execute("board.storyboard", &json!({"row": 1})).unwrap();
    let boards = |s: &Session, k: &str| s.doc().unwrap().nodes().filter(|n| n.plugin_data.get(ecboard::KIND).is_some_and(|x| x == k)).count();
    assert_eq!(boards(&s, "storyboard"), 1);
    let card = s.execute("board.state", &json!({})).unwrap()["rows"][1]["cards"][0]["card"].clone();
    let rv = s.execute("board.review", &json!({"card": card})).unwrap();
    assert_eq!(rv["existing"].as_array().unwrap().len(), 6);
    assert_eq!(rv["proposed"].as_array().unwrap().len(), 4);
    assert_eq!(rv["beats"][0]["ease"], "Fast In");
    let sbb = &sb["bounds"];
    assert!(rv["bounds"][1].as_f64().unwrap() > sbb[1].as_f64().unwrap(), "under the row's storyboard");
    // The frames are live cards (one frame each), not pictures.
    assert!(boards(&s, "still") >= 6 + 2);
    assert_eq!(boards(&s, "review"), 1);
}

#[test]
fn without_effectcraft_the_board_says_what_it_needs() {
    let mut s = Session::new();
    match s.execute("board.autoMain", &json!({"ecproj": PROJECT})) {
        Err(EngineError::Disabled(_, why)) => assert!(why.contains("EffectCraft"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(s.execute("board.state", &json!({})).is_ok());
    assert!(s.execute("board.events", &json!({"since": 0})).is_ok());
    assert!(s.execute("canvas.annotate.start", &json!({"tool": "pen"})).is_ok());
}

#[test]
fn junk_never_panics() {
    let junk = [
        json!(null),
        json!({"ecproj": [1], "comp": {"x": 1}, "x": "NaN", "y": 1e308}),
        json!({"card": "nope", "time": 1e308, "points": [[1], [1e308, -1e308]], "tool": 7, "note": [1]}),
        json!({"card": -1, "id": 1e308, "row": {"x": 1}, "chapter": [], "since": -5, "maxZoom": -1}),
        json!({"comp": "", "before": "yes", "ids": ["x"], "point": {"x": "a"}, "frame": "NaN"}),
        json!({"ecproj": "\u{0}", "chapter": "\u{1F4A5}".repeat(300), "time": -1e308}),
    ];
    let ids: Vec<&str> = crate::all_commands()
        .iter()
        .map(|c| c.id)
        .filter(|i| {
            ["board.autoMain", "board.state", "board.events", "board.agentView", "board.storyboard", "board.review"].contains(i)
                || i.starts_with("canvas.annotate.")
                || i.starts_with("canvas.flyTo")
                || i.starts_with("card.flash")
        })
        .collect();
    assert!(ids.len() >= 17, "{ids:?}");
    for with_board in [false, true] {
        for cmd in &ids {
            for j in &junk {
                let (mut s, _) = sess();
                if with_board {
                    auto(&mut s);
                }
                if let Err(EngineError::Internal { cmd, msg }) = s.execute(cmd, j) {
                    panic!("{cmd} panicked on {j}: {msg}");
                }
            }
        }
    }
}

#[test]
fn stress_and_lines_from_words() {
    let words = vec![
        (1.0, 1.4, "the".to_string(), false),
        (1.4, 2.0, "sixty".into(), true),
        (2.0, 2.1, "-day".into(), false),
        (2.1, 3.0, "content".into(), true),
    ];
    assert_eq!(ecboard::line_of(&words, 0.0, 4.0), "the sixty-day content");
    let st = ecboard::stress_of(&[], &words, 0.0, 4.0).unwrap();
    assert_eq!((st.0.as_str(), st.2), ("content", "longest stressed word"));
    assert_eq!(ecboard::timecode(62.5, 29.97), "1:02:14");
    assert_eq!(ecboard::clock(62.9), "1:02");
}

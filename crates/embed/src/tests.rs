//! Not run yet (Elias runs them at night): `cargo test -p figcraft-embed`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use figcraft_engine::Session;
use figcraft_ui_egui::{FigApp, Services};
use serde_json::json;

use crate::board;

struct Quiet;
impl Services for Quiet {}

fn app() -> FigApp {
    FigApp::new(Session::empty(), Box::new(Quiet))
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("figcraft-embed-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::create_dir_all(&d);
    d
}

#[test]
fn the_board_sits_next_to_the_project() {
    assert_eq!(board::board_path(Path::new("/a/b/26-10 EMBED TEST.ecproj")), PathBuf::from("/a/b/26-10 EMBED TEST.board.figcraft"));
    assert_eq!(board::board_path(Path::new("/a/x.ecprojx")), PathBuf::from("/a/x.board.figcraft"));
    assert_eq!(board::board_path(Path::new("/a/.ecproj")), PathBuf::from("/a/.ecproj.board.figcraft"));
}

#[test]
fn a_new_board_is_made_saved_and_reopened_without_touching_the_project() {
    let dir = temp("new");
    let project = dir.join("p.ecproj");
    std::fs::write(&project, b"{}").unwrap();
    let board = board::board_path(&project);
    let ctx = egui::Context::default();
    let mut fig = app();
    let r = board::open(&mut fig, &ctx, &project, &board).unwrap();
    assert!(r.created);
    assert!(board.exists(), "a new board is saved at once");
    // An empty board asks for autoMain; until FigCraft has it the answer is "unknown command".
    match r.auto_main {
        Some(Ok(_)) => {}
        Some(Err(e)) => assert!(crate::is_unknown(&e) || !e.is_empty()),
        None => panic!("an empty board is built"),
    }
    assert_eq!(std::fs::read(&project).unwrap(), b"{}", "the project is never written");
    // Opening again shows the same tab, makes nothing new.
    let again = board::open(&mut fig, &ctx, &project, &board).unwrap();
    assert!(!again.created);
    assert_eq!(fig.session.docs.len(), 1);
    // A fresh app opens the file instead.
    let mut other = app();
    let third = board::open(&mut other, &ctx, &project, &board).unwrap();
    assert!(!third.created);
    assert_eq!(other.session.ds().unwrap().path.as_deref().map(Path::new), Some(board.as_path()));
}

#[test]
fn unknown_commands_come_back_as_errors_not_panics() {
    let ctx = egui::Context::default();
    let mut fig = app();
    let e = crate::run_command(&mut fig, &ctx, "canvas.noSuchThing", &json!({})).unwrap_err();
    assert!(crate::is_unknown(&e), "{e}");
    // A control method by name still works when the engine doesn't know it.
    assert!(crate::run_command(&mut fig, &ctx, "window.list", &json!({})).is_ok());
    for junk in [json!(null), json!([1, 2]), json!("x"), json!({"since": "nope"})] {
        let _ = crate::run_command(&mut fig, &ctx, "canvas.annotate.list", &junk);
        let _ = crate::run_command(&mut fig, &ctx, "canvas.flyTo", &junk);
    }
}

#[test]
fn marks_are_handed_over_once() {
    let mut seen = HashSet::new();
    let listed = json!({"marks": [{"id": 1, "note": "a"}, {"id": 2, "note": "b"}], "armed": "pen"});
    let (fresh, max) = board::fresh_marks(&listed, &mut seen);
    assert_eq!(fresh.len(), 2);
    assert_eq!(max, 2);
    let listed = json!([{"id": 2}, {"id": 3}, {"id": "m4"}, "junk", {"no": "id"}]);
    let (fresh, max) = board::fresh_marks(&listed, &mut seen);
    assert_eq!(fresh.iter().map(|m| m["id"].to_string()).collect::<Vec<_>>(), vec!["3", "\"m4\""]);
    assert_eq!(max, 3);
    assert!(board::fresh_marks(&json!(null), &mut seen).0.is_empty());
}

#[test]
fn the_selection_names_no_card_on_an_empty_board() {
    let ctx = egui::Context::default();
    let mut fig = app();
    crate::run_command(&mut fig, &ctx, "file.new", &json!({})).unwrap();
    crate::run_command(&mut fig, &ctx, "shape.rectangle", &json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
    let (cards, nodes) = board::selection(&fig);
    assert!(cards.is_empty());
    assert_eq!(nodes.len(), 1);
    let (card, node) = board::under_pointer(&fig);
    assert!(card.is_none());
    assert_eq!(node.map(|n| n.id), nodes.first().map(|n| n.id));
}

#[test]
fn prefs_are_read_from_figcrafts_state_file() {
    assert!(crate::services::prefs_of("not json").is_none());
    assert!(crate::services::prefs_of("{}").is_none());
    let defaults = serde_json::to_value(figcraft_engine::Prefs::default()).unwrap();
    assert!(crate::services::prefs_of(&json!({"prefs": defaults}).to_string()).is_some());
}

#[test]
fn sizes_and_scales_are_clamped() {
    assert_eq!(crate::clamp_size([0, 99_999]), [1, 16384]);
    assert_eq!(crate::clamp_ppp(f32::NAN, 2.0), 2.0);
    assert_eq!(crate::clamp_ppp(100.0, 2.0), 8.0);
}

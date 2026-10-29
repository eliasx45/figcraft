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

#[test]
fn the_card_at_a_time_is_the_shortest_that_holds_it() {
    let card = |node, start: f64, end: f64, offset: f64| board::CardRef {
        node,
        name: format!("c{node}"),
        comp: format!("c{node}"),
        comp_id: 0,
        project: String::new(),
        start,
        end,
        offset,
    };
    let cards = vec![card(1, 0.0, 545.0, 0.0), card(2, 0.0, 12.0, 0.13), card(3, 2.0, 6.0, 12.4)];
    assert_eq!(board::card_at_time(&cards, 5.0).map(|c| c.node), Some(2));
    assert_eq!(board::card_at_time(&cards, 15.0).map(|c| c.node), Some(3), "comp 2..6 at 12.4 holds 14.4..18.4");
    assert_eq!(board::card_at_time(&cards, 100.0).map(|c| c.node), Some(1));
    assert_eq!(board::card_at_time(&cards, -1.0), None);
    assert_eq!(board::card_at_time(&cards, f64::NAN), None);
}

/// Night run 29 Oct: Ghostex opens the board before FigCraft's first frame; the EffectCraft host
/// was only installed by that frame, so `board.autoMain` answered "needs FigCraft's EffectCraft
/// build" and the EDITING canvas stayed empty. `open` installs the host first.
#[test]
fn opening_before_the_first_frame_still_reaches_the_effectcraft_host() {
    let dir = temp("prehost");
    let project = dir.join("p.ecproj");
    std::fs::write(&project, b"{}").unwrap();
    let board = board::board_path(&project);
    let ctx = egui::Context::default();
    let mut fig = app();
    assert!(fig.session.ecboard.host.is_none(), "no frame has run yet");
    let r = board::open(&mut fig, &ctx, &project, &board).unwrap();
    assert!(fig.session.ecboard.host.is_some(), "open installs the host");
    if let Some(Err(e)) = r.auto_main {
        assert!(!e.contains("needs FigCraft's EffectCraft build"), "{e}");
    }
}

/// Night run 29 Oct: the sidebar's chapter "STEP 2" starts at 316.577 s (its marker), its first
/// scene "S3 03" at 316.583 s, and the previous chapter's "S3 02" ends at 316.583 s: the chapter
/// must fly to S3 03, not S3 02.
#[test]
fn a_chapter_a_hair_before_its_first_card_picks_that_card() {
    let card = |node, start: f64, end: f64| board::CardRef {
        node,
        name: format!("c{node}"),
        comp: format!("c{node}"),
        comp_id: 0,
        project: String::new(),
        start: 0.0,
        end: end - start,
        offset: start,
    };
    let cards = vec![card(44, 296.463, 316.583), card(45, 316.583, 322.889)];
    assert_eq!(board::card_at_time(&cards, 316.577).map(|c| c.node), Some(45));
    assert_eq!(board::card_at_time(&cards, 316.0).map(|c| c.node), Some(44));
    assert_eq!(board::card_at_time(&cards, 322.88).map(|c| c.node), Some(45), "the last card's end still finds it");
}

/// Ghostex hang, 29 Oct: Ghostex calls set_visible before every frame, and each call asked for an
/// immediate frame, so the canvas drew at the display rate for ever on an idle board.
#[test]
fn set_visible_asks_for_a_frame_only_when_the_view_comes_back() {
    assert!(crate::shows_again(false, true));
    assert!(!crate::shows_again(true, true), "already shown: no frame of its own");
    assert!(!crate::shows_again(true, false));
    assert!(!crate::shows_again(false, false));
}

#[test]
fn the_hosts_save_writes_the_board_tab_and_keeps_the_front_tab() {
    // Ghostex's one ⌘S saves the board (whatever tab is in front) and the EffectCraft project.
    let dir = temp("save");
    let project = dir.join("p.ecproj");
    std::fs::write(&project, b"{}").unwrap();
    let board = board::board_path(&project);
    let ctx = egui::Context::default();
    let mut fig = app();
    board::open(&mut fig, &ctx, &project, &board).unwrap();
    let i = fig.session.active;
    fig.session.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 5, "height": 5})).unwrap();
    assert!(fig.session.docs[i].is_dirty());
    let before = std::fs::metadata(&board).unwrap().len();
    // Another tab in front: the board is still the one saved, and the front tab stays in front.
    crate::run_command(&mut fig, &ctx, "file.new", &json!({"name": "other"})).unwrap();
    let front = fig.session.active;
    assert_ne!(front, i);
    board::save(&mut fig, &board).unwrap();
    assert!(!fig.session.docs[i].is_dirty(), "the board is saved");
    assert_ne!(std::fs::metadata(&board).unwrap().len(), before, "the rectangle is in the file");
    assert_eq!(fig.session.active, front);
    assert_eq!(std::fs::read(&project).unwrap(), b"{}", "the project is never written");
    // No board open: an error, no dialog, no panic.
    let e = board::save(&mut app(), &board).unwrap_err();
    assert!(e.contains("isn't open"), "{e}");
}

#[test]
fn claimed_commands_go_to_the_host_and_are_answered_once() {
    use figcraft_ui_egui::ControlRequest;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, mpsc};
    let (tx, rx) = mpsc::channel();
    let req = |id: u64, method: &str, params: serde_json::Value| ControlRequest {
        id: json!(id),
        method: method.into(),
        params,
        reply: tx.clone(),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let mut queue = vec![
        req(1, "engine.execute", json!({"command": "editing.saveAll"})),
        req(2, "engine.execute", json!({"command": "file.save", "params": {}})),
        req(3, "editing.saveAll", json!(null)),
        req(4, "window.list", json!({})),
    ];
    let claimed = vec!["editing.saveAll".to_string()];
    let taken = crate::host_commands::take(&mut queue, &claimed);
    assert_eq!(taken.iter().map(|t| t.command.as_str()).collect::<Vec<_>>(), ["editing.saveAll", "editing.saveAll"]);
    assert_eq!(taken[0].params, json!({}));
    assert_eq!(queue.iter().map(|r| r.id.as_u64().unwrap()).collect::<Vec<_>>(), [2, 4], "the rest stay, in order");
    let mut taken = taken.into_iter();
    taken.next().unwrap().reply(Ok(json!({"saved": true})));
    drop(taken); // the second, unanswered: the caller still hears back
    let replies: Vec<_> = rx.try_iter().collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0].result, Ok(json!({"saved": true})));
    assert!(replies[1].result.as_ref().unwrap_err().contains("wasn't answered"));
    assert!(crate::host_commands::take(&mut queue, &[]).is_empty());
}

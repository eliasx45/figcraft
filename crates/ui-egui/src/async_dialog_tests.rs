use std::path::PathBuf;

use figcraft_engine::Session;

use crate::{ContentsFuture, DialogFuture, FigApp, Services};

struct OpenFile(PathBuf);

impl Services for OpenFile {
    fn pick_open(&mut self, _title: &str, _exts: &[&str]) -> DialogFuture {
        Box::pin(std::future::ready(Some(self.0.clone())))
    }
}

#[test]
fn open_dialog_finishes_after_the_async_picker_returns() {
    let dir = std::env::temp_dir().join(format!("figcraft-async-open-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("opened.figcraft");
    let doc = figcraft_doc::Document::new("Opened");
    std::fs::write(&path, figcraft_format::save_bytes(&doc, None).unwrap()).unwrap();

    let mut app = FigApp::new(Session::empty(), Box::new(OpenFile(path.clone())));
    app.ui.home = true;
    app.open_dialog();
    assert!(app.session.docs.is_empty(), "opening waits for the picker result");

    app.poll_dialog(&egui::Context::default());
    assert_eq!(app.session.docs.len(), 1);
    assert_eq!(app.session.docs[0].path.as_deref(), Some(path.to_string_lossy().as_ref()));
    assert!(!app.ui.home);
    let _ = std::fs::remove_dir_all(dir);
}

/// Every picker answers `self.0`.
struct Picks(PathBuf);

impl Services for Picks {
    fn pick_open(&mut self, _title: &str, _exts: &[&str]) -> DialogFuture {
        Box::pin(std::future::ready(Some(self.0.clone())))
    }
    fn pick_save(&mut self, _title: &str, _name: &str, _exts: &[&str]) -> DialogFuture {
        Box::pin(std::future::ready(Some(self.0.clone())))
    }
}

/// Two files, A (the first) in front, and where the picker saves.
fn two_files(tag: &str) -> (FigApp, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("figcraft-async-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("a.figcraft");
    let mut app = FigApp::new(Session::empty(), Box::new(Picks(path.clone())));
    for _ in 0..2 {
        app.session.execute("file.new", &serde_json::json!({})).unwrap();
    }
    app.session.active = 0;
    (app, dir, path)
}

#[test]
fn save_as_saves_the_file_it_was_opened_for_after_another_tab_comes_to_the_front() {
    let (mut app, dir, path) = two_files("pin");
    app.save_as_dialog();
    // The user switches tabs while the dialog is open.
    app.session.active = 1;
    // Another window draws first: the result waits for the window holding the file.
    app.windows.current = 7;
    app.poll_dialog(&egui::Context::default());
    assert!(app.session.docs.iter().all(|d| d.path.is_none()));
    app.windows.current = 0;
    app.poll_dialog(&egui::Context::default());
    let p = path.to_string_lossy();
    assert_eq!(app.session.docs[0].path.as_deref(), Some(p.as_ref()), "A is saved where the user picked");
    assert_eq!(app.session.docs[1].path, None, "B is untouched");
    assert_eq!(app.session.active, 1, "the tab the user switched to stays in front");
    assert!(path.exists());
    let _ = std::fs::remove_dir_all(dir);
}

/// The browser: the picker hands over a file's name and contents, never a path.
struct Contents(&'static str, Vec<u8>);

impl Services for Contents {
    fn pick_open(&mut self, _title: &str, _exts: &[&str]) -> DialogFuture {
        panic!("a platform that picks contents is never asked for a path")
    }
    fn pick_open_contents(&mut self, _title: &str, _exts: &[&str]) -> Option<ContentsFuture> {
        Some(Box::pin(std::future::ready(Some(Ok((self.0.to_string(), self.1.clone()))))))
    }
}

/// The browser, with a picked file it won't read.
struct Unreadable(String);

impl Services for Unreadable {
    fn pick_open_contents(&mut self, _title: &str, _exts: &[&str]) -> Option<ContentsFuture> {
        Some(Box::pin(std::future::ready(Some(Err(self.0.clone())))))
    }
}

#[test]
fn a_picked_file_that_cannot_be_read_says_why_and_opens_nothing() {
    let why = crate::browser_can_read("Huge.fig", 600.0 * 1024.0 * 1024.0).unwrap_err();
    let mut app = FigApp::new(Session::empty(), Box::new(Unreadable(why.clone())));
    app.open_dialog();
    app.poll_dialog(&egui::Context::default());
    assert!(app.session.docs.is_empty());
    assert_eq!(app.ui.last_error.as_deref(), Some(why.as_str()));
    assert!(why.contains("Huge.fig"), "{why}");
}

#[test]
fn the_browser_reads_files_up_to_512_mib() {
    let max = (512u64 << 20) as f64;
    assert!(crate::browser_can_read("a", 0.0).is_ok());
    assert!(crate::browser_can_read("a", max).is_ok());
    assert!(crate::browser_can_read("a", max + 1.0).is_err());
    assert!(crate::browser_can_read("a", f64::NAN).is_err());
    assert!(crate::browser_can_read("a", f64::INFINITY).is_err());
}

#[test]
fn open_dialog_opens_picked_contents_as_a_new_unsaved_document() {
    let bytes = figcraft_format::save_bytes(&figcraft_doc::Document::new("Picked"), None).unwrap();
    let mut app = FigApp::new(Session::empty(), Box::new(Contents("Picked.figcraft", bytes)));
    app.ui.home = true;
    app.open_dialog();
    assert!(app.session.docs.is_empty(), "opening waits for the picker result");
    app.poll_dialog(&egui::Context::default());
    assert_eq!(app.session.docs.len(), 1);
    assert_eq!(app.session.docs[0].path, None, "no path to save over");
    assert_eq!(app.session.docs[0].doc.name, "Picked");
    assert!(!app.ui.home, "and the editor shows it");
}

#[test]
fn import_dialog_imports_picked_contents_into_the_file_in_front() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20" fill="red"/></svg>"#.to_vec();
    let mut app = FigApp::new(Session::new(), Box::new(Contents("Logo.svg", svg)));
    let rectangles = |app: &FigApp| app.session.doc().unwrap().nodes().filter(|n| matches!(n.kind, figcraft_doc::NodeKind::Rectangle)).count();
    assert_eq!(rectangles(&app), 0);
    app.import_dialog(false);
    app.poll_dialog(&egui::Context::default());
    assert_eq!(app.session.docs.len(), 1, "imported, not opened");
    assert_eq!(rectangles(&app), 1, "the SVG's content");
}

#[test]
fn a_dialog_whose_file_closed_meanwhile_does_nothing() {
    let (mut app, dir, path) = two_files("closed");
    app.save_as_dialog();
    app.session.execute("file.close", &serde_json::json!({"index": 0, "force": true})).unwrap();
    app.poll_dialog(&egui::Context::default());
    assert!(app.session.docs.iter().all(|d| d.path.is_none()), "B isn't saved in A's place");
    assert!(!path.exists());
    assert!(app.pending_dialog.is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn closing_an_unsaved_tab_asks_and_save_closes_it_once_saved() {
    let (mut app, dir, path) = two_files("close");
    app.session.execute("node.create", &serde_json::json!({"type": "frame", "width": 10, "height": 10})).unwrap();
    let uid = app.session.docs[0].uid;
    // File ▸ Close (and the tab's close button) asks instead of doing nothing.
    crate::menus::invoke(&mut app, "file.close");
    assert_eq!(app.ui.confirm_close_tab, Some(uid));
    assert_eq!(app.session.docs.len(), 2, "nothing closes before the answer");
    // Save: the never-saved file asks where, saves there, then closes.
    app.ui.confirm_close_tab = None;
    crate::tabs::finish_close_tab(&mut app, uid, true);
    assert_eq!(app.session.docs.len(), 2, "still open while the save dialog is up");
    app.poll_dialog(&egui::Context::default());
    assert!(path.exists(), "saved where the user picked");
    assert!(app.session.docs.iter().all(|d| d.uid != uid), "closed once saved");
    assert_eq!(app.session.docs.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn dont_save_closes_the_tab_and_drops_its_changes() {
    let (mut app, dir, _) = two_files("discard");
    app.session.execute("node.create", &serde_json::json!({"type": "frame", "width": 10, "height": 10})).unwrap();
    let uid = app.session.docs[0].uid;
    crate::tabs::finish_close_tab(&mut app, uid, false);
    assert!(app.session.docs.iter().all(|d| d.uid != uid));
    assert_eq!(app.session.docs.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// A picker that hasn't answered yet.
struct Open;

impl Services for Open {
    fn pick_save(&mut self, _title: &str, _name: &str, _exts: &[&str]) -> DialogFuture {
        Box::pin(std::future::pending())
    }
}

#[test]
fn an_open_dialog_keeps_looking_without_input() {
    // The system's wake can be lost on macOS: a frame must come back on its own to see the answer.
    let mut app = FigApp::new(Session::new(), Box::new(Open));
    app.save_as_dialog();
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.poll_dialog(ui.ctx()));
    out.textures_delta.clear();
    let delay = out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.repaint_delay);
    assert!(delay.is_some_and(|d| d <= std::time::Duration::from_secs(1)), "repaint after {delay:?}");
    assert!(app.pending_dialog.is_some());
}

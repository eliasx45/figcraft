//! Recovery copies: a run that ends without closing leaves copies of its unsaved documents that the
//! next start opens again (a running app's are left alone); a normal quit leaves nothing.

use figcraft_engine::Session;
use serde_json::json;

use crate::recovery::{Recovery, recover};

fn root(name: &str) -> std::path::PathBuf {
    let r = std::env::temp_dir().join(format!("figcraft-recovery-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&r);
    r
}

fn edited() -> Session {
    let mut s = Session::new();
    s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 10, "height": 10})).unwrap();
    s
}

#[test]
fn a_crashed_runs_unsaved_documents_come_back() {
    let root = root("crash");
    let s = edited();
    let mut r = Recovery::start(&root).unwrap();
    r.write(&s, true);
    // While it runs, another start leaves its copies alone.
    let mut other = Session::empty();
    assert_eq!(recover(&root, &mut other), 0);
    // It ends without closing (the lock goes with the process).
    drop(r);
    let mut next = Session::empty();
    assert_eq!(recover(&root, &mut next), 1);
    let ds = next.docs.first().unwrap();
    assert!(ds.is_dirty() && ds.path.is_none());
    assert_eq!(next.doc().unwrap().node(next.doc().unwrap().pages[0]).map(|p| p.children.len()), Some(1));
    assert!(std::fs::read_dir(&root).unwrap().next().is_none(), "the folder is gone");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn copies_follow_saves_and_a_normal_quit_leaves_nothing() {
    let root = root("quit");
    let mut s = edited();
    let mut r = Recovery::start(&root).unwrap();
    r.write(&s, true);
    let dir = std::fs::read_dir(&root).unwrap().flatten().next().unwrap().path();
    let copies =
        |d: &std::path::Path| std::fs::read_dir(d).unwrap().flatten().filter(|f| f.path().extension().is_some_and(|e| e == "figcraft")).count();
    assert_eq!(copies(&dir), 1);
    // Saved: the copy goes.
    s.ds_mut().unwrap().mark_saved();
    r.write(&s, true);
    assert_eq!(copies(&dir), 0);
    // Changes pending while the interval runs: it asks to be woken for them.
    s.execute("shape.rectangle", &json!({"x": 0, "y": 0, "width": 4, "height": 4})).unwrap();
    assert!(r.tick(&s, 0.0).is_none());
    assert!(r.tick(&s, 1.0).is_none(), "copied at 0 s: nothing new");
    s.execute("shape.rectangle", &json!({"x": 9, "y": 0, "width": 4, "height": 4})).unwrap();
    assert!(r.tick(&s, 1.0).is_some_and(|w| w > 0.0));
    r.finish();
    assert!(!dir.exists(), "a normal quit removes the copies");
    let _ = std::fs::remove_dir_all(&root);
}

/// The files named `*.figcraft` in the run folder under `root`.
fn copy_count(root: &std::path::Path) -> usize {
    let Some(dir) = std::fs::read_dir(root).unwrap().flatten().next().map(|e| e.path()) else { return 0 };
    std::fs::read_dir(dir).unwrap().flatten().filter(|f| f.path().extension().is_some_and(|e| e == "figcraft")).count()
}

/// A folder at the path of `s`'s first document's copy in the run folder under `root`.
fn blocked_copy(root: &std::path::Path, s: &Session) -> std::path::PathBuf {
    let dir = std::fs::read_dir(root).unwrap().flatten().next().unwrap().path();
    let blocker = dir.join(format!("{}.figcraft", s.docs[0].uid));
    std::fs::create_dir(&blocker).unwrap();
    blocker
}

#[test]
fn a_copy_that_failed_is_tried_again_without_another_edit() {
    let root = root("retry");
    let s = edited();
    let mut r = Recovery::start(&root).unwrap();
    // A folder where the copy goes makes its write fail on every platform (renaming the run folder
    // away fails on Windows, where the run's lock file keeps it open).
    let blocker = blocked_copy(&root, &s);
    r.write(&s, true);
    std::fs::remove_dir(&blocker).unwrap();
    // Nothing was edited since: the same revision still has to be copied.
    r.write(&s, true);
    assert_eq!(copy_count(&root), 1, "the failed revision was copied again");
    r.finish();
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_background_copy_that_failed_is_tried_again_without_another_edit() {
    let root = root("retry-bg");
    let s = edited();
    let mut r = Recovery::start(&root).unwrap();
    let blocker = blocked_copy(&root, &s);
    r.write(&s, false);
    // Let the failing write end before the way is clear again.
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::fs::remove_dir(&blocker).unwrap();
    for _ in 0..50 {
        r.write(&s, false);
        if copy_count(&root) == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(copy_count(&root), 1, "the failed revision was copied again");
    r.finish();
    let _ = std::fs::remove_dir_all(&root);
}

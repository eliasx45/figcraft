//! Which transport an EffectCraft card's requests take (the running app over TCP, or EffectCraft's
//! engine in-process on the card's saved project) and how the in-process one fails: never a
//! panic, never a stalled link.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use super::{AppState, Route, Seen, choose, state_of};

#[test]
fn without_the_feature_or_a_saved_project_cards_ask_the_app() {
    for app in [AppState::Closed, AppState::Owns, AppState::Other, AppState::Unknown] {
        assert_eq!(choose(false, true, app), Route::Tcp, "{app:?}");
        assert_eq!(choose(true, false, app), Route::Tcp, "{app:?}");
    }
}

#[test]
fn in_process_unless_the_app_has_the_project_open() {
    assert_eq!(choose(true, true, AppState::Closed), Route::InProc);
    assert_eq!(choose(true, true, AppState::Other), Route::InProc);
    // The app's live (unsaved) edits win.
    assert_eq!(choose(true, true, AppState::Owns), Route::Tcp);
    // An app that can't say what it has open might have it: ask it, as before.
    assert_eq!(choose(true, true, AppState::Unknown), Route::Tcp);
}

#[test]
fn what_the_app_has_open_decides_ownership() {
    let p = PathBuf::from("/x/a.ecproj");
    assert_eq!(state_of(&Seen::Closed, &p), AppState::Closed);
    assert_eq!(state_of(&Seen::Unknown, &p), AppState::Unknown);
    assert_eq!(state_of(&Seen::Open(Some(p.clone())), &p), AppState::Owns);
    assert_eq!(state_of(&Seen::Open(Some(PathBuf::from("/x/b.ecproj"))), &p), AppState::Other);
    // Untitled.
    assert_eq!(state_of(&Seen::Open(None), Path::new("/x/a.ecproj")), AppState::Other);
}

/// The link thread with the in-process engine, its app port one nothing listens on.
#[cfg(feature = "effectcraft-inproc")]
mod in_process {
    use std::path::PathBuf;

    use figcraft_doc::{EcComp, NodeId};

    use super::super::{Job, Kind, Reply, StoreKey, Thread};

    fn closed_port() -> u16 {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
        // (dropped: connections to it are refused)
    }

    fn thread() -> Thread {
        Thread::new(egui::Context::default(), closed_port())
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("figcraft-ec-transport-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn card(project: &str, comp: &str, comp_id: u64) -> EcComp {
        let mut c = EcComp::new(comp, 0.0, 2.0, 25.0);
        c.comp_id = comp_id;
        c.project = project.to_string();
        c
    }

    fn frames(c: &EcComp, frames: Vec<i64>) -> Job {
        Job::Frames { store: StoreKey::of(c, 240), comp: c.clone(), frames, side: 240, poster: None }
    }

    #[test]
    fn a_saved_project_renders_with_the_app_closed() {
        let d = dir("ok");
        let p = d.join("p.ecproj");
        let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
        let c = card(p.to_str().unwrap(), "C", cid);
        let mut t = thread();
        assert!(t.in_process_route(&frames(&c, vec![0])).is_some());
        match t.run(frames(&c, vec![0, 1, 2, 3, 4, 5])) {
            Reply::Frames { got, pending, failed, headless, file, revision, .. } => {
                assert_eq!(got.len(), figcraft_ec_inproc::FRAMES_PER_CALL);
                assert_eq!(pending.len(), 6 - figcraft_ec_inproc::FRAMES_PER_CALL);
                assert!(failed.is_empty());
                assert_eq!((headless, revision), (None, 0));
                assert!(file.is_some_and(|f| f > 0));
            }
            _ => panic!("expected frames"),
        }
        // A tree and a (silent) sound too.
        assert!(matches!(t.run(Job::Tree { card: NodeId(1), comp: c.clone(), at: 0.0 }), Reply::Tree { .. }));
        let key = (c.comp.clone(), c.comp_id, 0, 1000);
        assert!(matches!(t.run(Job::Sound { key, comp: c.clone(), start: 0.0, end: 1.0 }), Reply::Sound { sound: None, .. }));
        // The app being closed isn't an outage while cards render here.
        assert!(matches!(t.run(Job::Revision), Reply::Revision(0)));
    }

    #[test]
    fn a_bad_comp_fails_its_frames_not_the_link() {
        let d = dir("badcomp");
        let p = d.join("p.ecproj");
        figcraft_ec_inproc::sample_project(&p).unwrap();
        let mut t = thread();
        for c in [card(p.to_str().unwrap(), "Nope", 0), card(p.to_str().unwrap(), "C", 987_654)] {
            match t.run(frames(&c, vec![0, 1])) {
                Reply::FramesFailed { frames, error, .. } => {
                    assert_eq!(frames, vec![0, 1]);
                    assert!(!error.is_empty());
                }
                _ => panic!("expected FramesFailed for {}", c.comp),
            }
            assert!(matches!(t.run(Job::Tree { card: NodeId(1), comp: c.clone(), at: 0.0 }), Reply::Error(_)));
            let key = (c.comp.clone(), c.comp_id, 0, 1000);
            assert!(matches!(t.run(Job::Sound { key, comp: c.clone(), start: 0.0, end: 1.0 }), Reply::Sound { sound: None, .. }));
        }
    }

    #[test]
    fn a_junk_project_fails_its_frames() {
        let d = dir("junk");
        let p = d.join("p.ecproj");
        std::fs::write(&p, b"{ not a project").unwrap();
        let c = card(p.to_str().unwrap(), "C", 1);
        let mut t = thread();
        assert!(matches!(t.run(frames(&c, vec![0])), Reply::FramesFailed { .. }));
    }

    #[test]
    fn a_missing_project_goes_to_the_app_which_is_closed() {
        let d = dir("missing");
        let c = card(d.join("gone.ecproj").to_str().unwrap(), "C", 1);
        let mut t = thread();
        assert!(t.in_process_route(&frames(&c, vec![0])).is_none());
        // No project file, no app: the link is down (the CLI fallback needs the file too).
        assert!(matches!(t.run(frames(&c, vec![0])), Reply::Down { kind: Kind::Frames, .. }));
        // A card without a project at all.
        let c = card("", "C", 1);
        assert!(t.in_process_route(&frames(&c, vec![0])).is_none());
        // Opening a comp in EffectCraft always needs the app.
        assert!(matches!(t.run(Job::Reveal { comp: c, time: 0.0 }), Reply::Down { kind: Kind::Other, .. }));
    }
}

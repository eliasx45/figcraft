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
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    use figcraft_doc::{EcComp, NodeId};
    use figcraft_ec_inproc::{Compositor, FRAMES_PER_CALL, InProc};

    use super::super::worker::{self, Work, compositor, enqueue};
    use super::super::{Job, Kind, Reply, StoreKey, Thread};

    fn closed_port() -> u16 {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        l.local_addr().unwrap().port()
        // (dropped: connections to it are refused)
    }

    /// The thread and where the frame worker answers.
    fn thread() -> (Thread, Receiver<Reply>) {
        let (tx, rx) = channel();
        (Thread::new(egui::Context::default(), closed_port(), tx), rx)
    }

    fn reply(rx: &Receiver<Reply>) -> Reply {
        rx.recv_timeout(Duration::from_secs(120)).unwrap()
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

    fn sample(name: &str) -> (PathBuf, u64) {
        let p = dir(name).join("p.ecproj");
        let cid = figcraft_ec_inproc::sample_project(&p).unwrap();
        (p, cid)
    }

    #[test]
    fn the_gpu_only_when_shared_showable_and_not_the_poster() {
        assert_eq!(compositor(false, true, true), Compositor::Gpu);
        assert_eq!(compositor(true, true, true), Compositor::Cpu);
        assert_eq!(compositor(false, false, true), Compositor::Cpu);
        assert_eq!(compositor(false, true, false), Compositor::Cpu);
    }

    #[test]
    fn a_saved_project_renders_on_the_worker_with_the_app_closed() {
        let (p, cid) = sample("ok");
        let c = card(p.to_str().unwrap(), "C", cid);
        let (mut t, rx) = thread();
        assert!(t.in_process_route(&frames(&c, vec![0])).is_some());
        // Handed to the worker: no reply from the link thread.
        assert!(t.run(frames(&c, vec![0, 1, 2, 3, 4, 5])).is_none());
        // Sound and trees answer meanwhile (they don't wait for frames).
        assert!(matches!(t.run(Job::Tree { card: NodeId(1), comp: c.clone(), at: 0.0 }), Some(Reply::Tree { .. })));
        let key = (c.comp.clone(), c.comp_id, 0, 1000);
        assert!(matches!(t.run(Job::Sound { key, comp: c.clone(), start: 0.0, end: 1.0 }), Some(Reply::Sound { sound: None, .. })));
        // The app being closed isn't an outage while cards render here.
        assert!(matches!(t.run(Job::Revision), Some(Reply::Revision(0))));
        match reply(&rx) {
            Reply::Frames { got, pending, failed, headless, file, revision, .. } => {
                // No shared device in tests: CPU frames.
                assert_eq!(got.len(), FRAMES_PER_CALL);
                assert!(got.iter().all(|(_, t)| matches!(t, crate::effectcraft_ui::FrameTex::Cpu(_))));
                assert_eq!(pending.len(), 6 - FRAMES_PER_CALL);
                assert!(failed.is_empty());
                assert_eq!((headless, revision), (None, 0));
                assert!(file.is_some_and(|f| f > 0));
            }
            _ => panic!("expected frames"),
        }
    }

    #[test]
    fn a_newer_request_for_a_card_replaces_the_queued_one() {
        let (p, cid) = sample("newest");
        let ec = InProc::open(&p).unwrap();
        let c = card(p.to_str().unwrap(), "C", cid);
        let work = |frames: Vec<i64>, poster: Option<(NodeId, i64)>| Work {
            store: StoreKey::of(&c, 240),
            comp: "C".into(),
            comp_id: cid,
            frames,
            side: 240,
            poster,
            snapshot: ec.snapshot(),
        };
        let (tx, rx) = channel();
        let mut queue = Vec::new();
        enqueue(&mut queue, work(vec![0, 1], Some((NodeId(7), 0))), &tx);
        enqueue(&mut queue, work(vec![9], None), &tx);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].frames, vec![9]);
        // The poster request carries over to the newer request.
        assert_eq!(queue[0].poster, Some((NodeId(7), 0)));
        match rx.try_recv().unwrap() {
            Reply::Frames { got, pending, .. } => assert!(got.is_empty() && pending == vec![0, 1]),
            _ => panic!("expected the replaced request back as pending"),
        }
        // A request being rendered stops when a newer one for its card is waiting.
        let (jtx, jrx) = channel();
        jtx.send(work(vec![20], None)).unwrap();
        let mut queue = Vec::new();
        match worker::render(&egui::Context::default(), work(vec![10, 11, 12], None), &jrx, &mut queue, &tx) {
            Reply::Frames { got, pending, .. } => {
                assert_eq!(got.len(), 1);
                assert_eq!(pending, vec![11, 12]);
            }
            _ => panic!("expected frames"),
        }
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn a_bad_comp_fails_its_frames_not_the_link() {
        let (p, _) = sample("badcomp");
        let (mut t, _rx) = thread();
        for c in [card(p.to_str().unwrap(), "Nope", 0), card(p.to_str().unwrap(), "C", 987_654)] {
            match t.run(frames(&c, vec![0, 1])) {
                Some(Reply::FramesFailed { frames, error, .. }) => {
                    assert_eq!(frames, vec![0, 1]);
                    assert!(!error.is_empty());
                }
                _ => panic!("expected FramesFailed for {}", c.comp),
            }
            assert!(matches!(t.run(Job::Tree { card: NodeId(1), comp: c.clone(), at: 0.0 }), Some(Reply::Error(_))));
            let key = (c.comp.clone(), c.comp_id, 0, 1000);
            assert!(matches!(t.run(Job::Sound { key, comp: c.clone(), start: 0.0, end: 1.0 }), Some(Reply::Sound { sound: None, .. })));
        }
    }

    #[test]
    fn a_junk_project_fails_its_frames() {
        let p = dir("junk").join("p.ecproj");
        std::fs::write(&p, b"{ not a project").unwrap();
        let c = card(p.to_str().unwrap(), "C", 1);
        let (mut t, _rx) = thread();
        assert!(matches!(t.run(frames(&c, vec![0])), Some(Reply::FramesFailed { .. })));
    }

    #[test]
    fn a_missing_project_goes_to_the_app_which_is_closed() {
        let d = dir("missing");
        let c = card(d.join("gone.ecproj").to_str().unwrap(), "C", 1);
        let (mut t, _rx) = thread();
        assert!(t.in_process_route(&frames(&c, vec![0])).is_none());
        // No project file, no app: the link is down (the CLI fallback needs the file too).
        assert!(matches!(t.run(frames(&c, vec![0])), Some(Reply::Down { kind: Kind::Frames, .. })));
        // A card without a project at all.
        let c = card("", "C", 1);
        assert!(t.in_process_route(&frames(&c, vec![0])).is_none());
        // Opening a comp in EffectCraft always needs the app.
        assert!(matches!(t.run(Job::Reveal { comp: c, time: 0.0 }), Some(Reply::Down { kind: Kind::Other, .. })));
    }
}

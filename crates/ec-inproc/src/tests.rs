use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("figcraft-ec-inproc-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A saved project with one 64×36 comp `C` (id returned) holding a red solid.
fn project(dir: &Path) -> (PathBuf, u64) {
    let path = dir.join("p.ecproj");
    let cid = sample_project(&path).unwrap();
    (path, cid)
}

#[test]
fn a_missing_project_is_an_error_not_a_panic() {
    let dir = temp("missing");
    assert!(InProc::open(&dir.join("nope.ecproj")).is_err());
    // A folder isn't a project.
    assert!(InProc::open(&dir).is_err());
    let mut pool = Pool::default();
    assert!(pool.get(&dir.join("nope.ecproj")).is_err());
    assert!(pool.is_empty());
}

#[test]
fn a_junk_project_is_an_error() {
    let dir = temp("junk");
    for (i, bytes) in [&b""[..], b"{", b"not json at all", &[0xff, 0xfe, 0x00][..], b"{\"items\": 5}"].iter().enumerate() {
        let p = dir.join(format!("{i}.ecproj"));
        std::fs::write(&p, bytes).unwrap();
        assert!(InProc::open(&p).is_err(), "{i}");
    }
}

#[test]
fn frames_tree_and_sound_of_a_saved_project() {
    let dir = temp("roundtrip");
    let (path, cid) = project(&dir);
    let mut ec = InProc::open(&path).unwrap();
    assert!(ec.revision() > 0);
    let v = ec.call("figcraft.frames", &json!({"comp": cid, "frames": [0, 1, 2, 3, 4, 5], "maxSide": 32})).unwrap();
    let frames = v["frames"].as_array().unwrap();
    assert_eq!(frames.len(), 6);
    assert_eq!(frames.iter().filter(|f| f["png"].is_string()).count(), FRAMES_PER_CALL);
    assert_eq!(frames.iter().filter(|f| f["pending"] == json!(true)).count(), 6 - FRAMES_PER_CALL);
    use base64::Engine as _;
    let png = base64::engine::general_purpose::STANDARD.decode(frames[0]["png"].as_str().unwrap()).unwrap();
    assert_eq!(png.get(1..4), Some(&b"PNG"[..]));
    // By name too.
    assert!(ec.call("figcraft.frames", &json!({"comp": "C", "frames": [0], "maxSide": 32})).unwrap()["frames"][0]["png"].is_string());
    let tree = ec.call("figcraft.tree", &json!({"comp": cid, "start": 0.0, "end": 1.0})).unwrap();
    assert_eq!(tree["comp"]["name"], json!("C"));
    assert_eq!(ec.call("figcraft.audio", &json!({"comp": cid, "start": 0.0, "end": 1.0})).unwrap()["silent"], json!(true));
    assert_eq!(ec.call("app.revision", &json!({})).unwrap()["revision"], json!(ec.revision()));
}

#[test]
fn a_bad_comp_or_bad_params_are_errors() {
    let dir = temp("bad");
    let (path, cid) = project(&dir);
    let mut ec = InProc::open(&path).unwrap();
    for p in [
        json!({"comp": 999_999, "frames": [0]}),
        json!({"comp": "No such comp", "frames": [0]}),
        json!({"comp": null, "frames": [0]}),
        json!({"frames": [0]}),
        json!({"comp": cid}),
        json!({"comp": cid, "frames": "0"}),
        json!({"comp": cid, "frames": vec![0; MAX_FRAMES + 1]}),
        json!([]),
        Value::Null,
    ] {
        assert!(ec.call("figcraft.frames", &p).is_err(), "{p}");
    }
    // Junk frame numbers are reported per frame.
    let v = ec.call("figcraft.frames", &json!({"comp": cid, "frames": [-1, "x", 1e300, i64::MAX]})).unwrap();
    assert!(v["frames"].as_array().unwrap().iter().all(|f| f["error"].is_string()), "{v}");
    for p in [json!({"comp": cid}), json!({"comp": cid, "start": 1.0, "end": 0.0}), json!({"comp": cid, "start": f64::MAX, "end": 1.0}), json!({"comp": 0, "start": 0, "end": 1})]
    {
        assert!(ec.call("figcraft.tree", &p).is_err(), "{p}");
        assert!(ec.call("figcraft.audio", &p).is_err(), "{p}");
    }
    assert!(ec.call("figcraft.audio", &json!({"comp": cid, "start": 0, "end": 1, "rate": 1})).is_err());
    assert!(ec.call("figcraft.reveal", &json!({"comp": cid, "time": 0})).is_err());
    assert!(ec.call("no.such.method", &json!({})).unwrap_err().contains("unknown method"));
    assert!(ec.call("render.frame", &json!(7)).is_err());
}

#[test]
fn a_save_reloads_the_session_and_changes_the_revision() {
    let dir = temp("reload");
    let (path, cid) = project(&dir);
    let mut pool = Pool::default();
    let r0 = pool.get(&path).unwrap().revision();
    // Saved again (longer file, later time): the next call sees the new project.
    std::thread::sleep(std::time::Duration::from_millis(20));
    let mut s = new_session();
    s.execute("file.open", json!({"path": path.to_str().unwrap()})).unwrap();
    s.execute("comp.new", json!({"name": "D", "width": 32, "height": 32, "duration": 1})).unwrap();
    s.execute("file.save", json!({"force": true})).unwrap();
    let ec = pool.get(&path).unwrap();
    assert_ne!(ec.revision(), r0);
    assert!(ec.call("figcraft.tree", &json!({"comp": "D", "start": 0, "end": 1})).is_ok());
    assert!(ec.call("figcraft.tree", &json!({"comp": cid, "start": 0, "end": 1})).is_ok());
    // Deleted: an error, and the session is let go.
    std::fs::remove_file(&path).unwrap();
    assert!(pool.get(&path).is_err());
    assert!(pool.is_empty());
}

#[test]
fn the_pool_keeps_a_few_projects() {
    let dirs: Vec<PathBuf> = (0..POOL + 1).map(|i| temp(&format!("pool{i}"))).collect();
    let mut pool = Pool::default();
    for d in &dirs {
        let (p, _) = project(d);
        assert!(pool.get(&p).is_ok());
    }
    assert_eq!(pool.len(), POOL);
}

#[test]
fn wav_header_is_16_bit_stereo_pcm() {
    let w = wav16(&[0.0, 1.0, -1.0, f32::NAN], 48_000);
    assert_eq!(&w[..4], b"RIFF");
    assert_eq!(&w[8..16], b"WAVEfmt ");
    assert_eq!(w.len(), 44 + 8);
    assert_eq!(i16::from_le_bytes([w[46], w[47]]), 32767);
    assert_eq!(i16::from_le_bytes([w[48], w[49]]), -32767);
    assert_eq!(i16::from_le_bytes([w[50], w[51]]), 0);
}

#[test]
fn a_snapshot_answers_tree_and_sound_like_the_file() {
    let dir = temp("snapcall");
    let (path, cid) = project(&dir);
    let mut ec = InProc::open(&path).unwrap();
    let snap = ec.snapshot();
    let p = json!({"comp": cid, "start": 0.0, "end": 1.0});
    assert_eq!(snap.call("figcraft.tree", &p).unwrap(), ec.call("figcraft.tree", &p).unwrap());
    assert_eq!(snap.call("figcraft.audio", &p).unwrap()["silent"], json!(true));
    // An edit in the session shows in a new snapshot, not the file.
    ec.session_mut().execute("comp.new", json!({"name": "E", "width": 16, "height": 16, "duration": 1})).unwrap();
    assert!(ec.snapshot().call("figcraft.tree", &json!({"comp": "E", "start": 0, "end": 1})).is_ok());
    assert!(snap.call("figcraft.tree", &json!({"comp": "E", "start": 0, "end": 1})).is_err());
    for bad in [json!({}), json!({"comp": 999_999, "start": 0, "end": 1}), json!({"comp": cid}), Value::Null] {
        assert!(snap.call("figcraft.tree", &bad).is_err(), "{bad}");
        assert!(snap.call("figcraft.audio", &bad).is_err(), "{bad}");
    }
    assert!(snap.call("figcraft.frames", &p).is_err());
}

use super::*;

#[test]
fn video_names() {
    assert!(is_video_name("clip.MP4"));
    assert!(is_video_name("/a/b/edit.mov"));
    assert!(!is_video_name("vo.m4a"));
    assert!(!is_video_name("mp4"));
}

#[test]
fn frames_are_decoded_at_the_card_size_with_even_sides() {
    assert_eq!(fit(1280, 720, 480), (480, 270));
    assert_eq!(fit(1080, 1920, 480), (270, 480));
    assert_eq!(fit(640, 360, 960), (640, 360), "never larger than the file");
    assert_eq!(fit(1001, 563, 480), (480, 270));
    assert_eq!(fit(0, 0, 0), (2, 2));
    assert_eq!(fit(8000, 8000, 99_999).0, MAX_SIDE);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn other_systems_say_unsupported() {
    assert_eq!(probe(Path::new("x.mp4")), Err(Error::Unsupported));
}

#[test]
fn a_missing_file_is_an_error_not_a_crash() {
    let p = std::env::temp_dir().join("figcraft-no-such-video.mp4");
    assert!(probe(&p).is_err());
    assert!(frame_at(&p, 0.0, 240).is_err());
}

/// `FIGCRAFT_VIDEO_SAMPLE=/path/clip.mp4 cargo test -p figcraft-video`: probes and decodes a real
/// file (skipped without one; CI has none).
#[test]
fn decodes_a_real_file_when_given() {
    let Some(p) = std::env::var_os("FIGCRAFT_VIDEO_SAMPLE") else { return };
    let p = std::path::PathBuf::from(p);
    let info = probe(&p).unwrap();
    assert!(info.duration > 0.0 && info.width > 0, "{info:?}");
    let mut r = Reader::open(&p, 1.0, 2.0, 480).unwrap();
    let mut n = 0;
    let mut last = -1.0;
    while let Some(f) = r.next() {
        assert!(f.width <= 480 && f.height <= 480 && f.rgba.len() == (f.width * f.height * 4) as usize);
        assert!(f.time > last);
        last = f.time;
        n += 1;
    }
    assert!(n >= 10, "about a second of frames, got {n}");
    let poster = frame_at(&p, 0.5, 240).unwrap();
    assert_eq!(poster.width.max(poster.height), 240);
    eprintln!("{info:?}: {n} frames 1–2 s, last at {last:.3}");
}

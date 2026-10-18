//! Sounds in a native file: saved once per hash with the layers that play them, read back, and
//! reported when missing.

use figcraft_doc::{AudioClip, AudioMarker, Document, NodeKind, Peaks};

use super::*;

fn doc_with_sound() -> (Document, String) {
    let mut d = Document::new("File");
    let p = d.first_page().unwrap();
    let hash = add_audio(&mut d, b"RIFF....WAVEfake".to_vec());
    for x in [0.0, 300.0] {
        let id = d.create(p, NodeKind::Frame, "Audio", x, 0.0, 200.0, 80.0).unwrap();
        let mut clip = AudioClip::new(hash.clone(), "voice.wav", 2.0, 48_000, 1);
        clip.peak_rate = 200;
        clip.peaks = Peaks(Arc::new(vec![0, 50, 255, 7]));
        clip.markers.push(AudioMarker { time: 0.5, label: "CON".into(), color: None });
        d.get_mut(id).unwrap().audio = Some(Box::new(clip));
    }
    (d, hash)
}

#[test]
fn sounds_and_clips_round_trip() {
    let (d, hash) = doc_with_sound();
    let bytes = save_bytes(&d, None).unwrap();
    let z = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    assert_eq!(z.file_names().filter(|n| n.starts_with("audio/")).count(), 1, "one member per sound");
    let (loaded, issues) = load_bytes(&bytes).unwrap();
    assert!(issues.is_empty(), "{issues:?}");
    assert_eq!(loaded.audio.get(&hash).map(|a| a.bytes.as_slice()), Some(&b"RIFF....WAVEfake"[..]));
    let clips: Vec<&AudioClip> = loaded.nodes().filter_map(|n| n.audio.as_deref()).collect();
    assert_eq!(clips.len(), 2);
    assert_eq!(clips[0].peaks.0.as_slice(), &[0, 50, 255, 7]);
    assert_eq!(clips[0].markers[0].label, "CON");
    assert_eq!(clips[0].file_name, "voice.wav");
}

#[test]
fn an_unused_sound_is_left_out() {
    let (mut d, _) = doc_with_sound();
    let unused = add_audio(&mut d, b"other".to_vec());
    let (loaded, _) = load_bytes(&save_bytes(&d, None).unwrap()).unwrap();
    assert!(!loaded.audio.contains_key(&unused));
}

#[test]
fn a_missing_sound_is_reported_and_the_clip_kept() {
    let (d, hash) = doc_with_sound();
    let bytes = save_bytes(&d, None).unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(&bytes)).unwrap();
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut w = zip::ZipWriter::new(&mut buf);
    let names: Vec<String> = z.file_names().map(str::to_string).collect();
    for n in names.iter().filter(|n| !n.starts_with("audio/")) {
        let data = read_member(&mut z, n, u64::MAX).unwrap();
        w.start_file(n.as_str(), zip::write::SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut w, &data).unwrap();
    }
    w.finish().unwrap();
    let (loaded, issues) = load_bytes(&buf.into_inner()).unwrap();
    assert!(issues.iter().any(|i| i.contains(&hash) && i.contains("missing")), "{issues:?}");
    assert_eq!(loaded.nodes().filter(|n| n.audio.is_some()).count(), 2);
}

#[test]
fn damaged_peaks_load_as_silence() {
    let (d, _) = doc_with_sound();
    let text = String::from_utf8(
        save_bytes(&d, None)
            .map(|b| {
                let mut z = zip::ZipArchive::new(std::io::Cursor::new(b)).unwrap();
                read_member(&mut z, "document.json", u64::MAX).unwrap()
            })
            .unwrap(),
    )
    .unwrap();
    let broken = text.replace("ADL/Bw==", "!!not base64!!");
    assert_ne!(broken, text, "the peaks are saved as base64");
    let (loaded, _) = load_bytes(broken.as_bytes()).unwrap();
    assert!(loaded.nodes().filter_map(|n| n.audio.as_deref()).any(|a| a.peaks.0.is_empty()));
}

use super::*;

fn tone() -> Sound {
    open("tone.wav", tone_wav(2.0, 440.0, 48_000, 0.8).into()).unwrap()
}

#[test]
fn opens_a_wav_and_reads_its_length() {
    let s = tone();
    assert_eq!(s.info().sample_rate, 48_000);
    assert_eq!(s.info().channels, 1);
    assert!((s.info().duration - 2.0).abs() < 1e-3, "{}", s.info().duration);
}

#[test]
fn a_name_without_extension_is_sniffed() {
    let s = open("voice", tone_wav(0.5, 220.0, 44_100, 0.5).into()).unwrap();
    assert_eq!(s.info().sample_rate, 44_100);
}

#[test]
fn peaks_follow_the_envelope() {
    let p = tone().peaks(PEAK_RATE);
    assert_eq!(p.len(), 400);
    // Silent at the ends, loudest in the middle (0.8 of full scale ≈ 204).
    assert!(p[0] < 10, "{}", p[0]);
    assert!(p[399] < 10, "{}", p[399]);
    assert!((195..=210).contains(&p[200]), "{}", p[200]);
    assert!(p[100] > 100 && p[100] < p[200]);
}

#[test]
fn stereo_reads_a_range_at_another_rate() {
    let s = tone();
    let v = s.stereo(0.5, 1.5, 44_100);
    assert_eq!(v.len(), 44_100 * 2);
    // Mono goes to both sides.
    assert!(v.chunks(2).all(|c| c[0] == c[1]));
    assert!(v.iter().any(|x| x.abs() > 0.5));
    // Past the end: silence, never a panic.
    let tail = s.stereo(1.9, 2.5, 48_000);
    assert_eq!(tail.len(), (0.6f64 * 48_000.0).round() as usize * 2);
    assert!(tail[tail.len() - 2..].iter().all(|x| *x == 0.0));
    // Before the start: silence first.
    let head = s.stereo(-0.1, 0.1, 48_000);
    assert_eq!(head.len(), 9600 * 2);
    assert!(head[..100].iter().all(|x| *x == 0.0));
}

#[test]
fn junk_and_empty_ranges_never_panic() {
    assert!(open("x.wav", Arc::from(vec![1u8, 2, 3])).is_err());
    assert!(open("x.mp3", Arc::from(Vec::new())).is_err());
    assert!(open("x.m4a", Arc::from(b"\0\0\0\x18ftypM4A junkjunkjunk".to_vec())).is_err());
    let s = tone();
    assert!(s.stereo(1.0, 0.5, 48_000).is_empty());
    assert!(s.stereo(f64::NAN, 1.0, 48_000).is_empty());
    assert!(s.stereo(0.0, 1.0, 0).is_empty());
    assert!(s.peaks(0).is_empty());
}

#[test]
fn extensions() {
    assert!(is_audio_name("a/b/Voice Over.M4A"));
    assert!(is_audio_name("x.wav"));
    assert!(!is_audio_name("x.png"));
    assert!(!is_audio_name("wav"));
}

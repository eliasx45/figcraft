//! Audio clips draw as a waveform strip with a ruler and markers.

use std::sync::Arc;

use figcraft_color::{Color, Paint};
use figcraft_doc::{AudioClip, AudioMarker, Document, NodeKind, Peaks};

use crate::render_node;

/// A 400 × 100 strip of a 4 s clip: silent first half, full scale second half, a red marker at 1 s.
fn strip(ruler: bool) -> (Document, figcraft_doc::NodeId) {
    let mut d = Document::new("F");
    let p = d.first_page().unwrap();
    let id = d.create(p, NodeKind::Frame, "Audio", 0.0, 0.0, 400.0, 100.0).unwrap();
    let mut a = AudioClip::new("h", "x.wav", 4.0, 48_000, 1);
    a.peak_rate = 10;
    a.peaks = Peaks(Arc::new((0..40).map(|i| if i < 20 { 0 } else { 255 }).collect()));
    a.color = Color::from_rgb8_const(0, 255, 0);
    a.ruler = ruler;
    a.markers.push(AudioMarker { time: 1.0, label: String::new(), color: Some(Color::from_rgb8_const(255, 0, 0)) });
    let n = d.get_mut(id).unwrap();
    n.fills = vec![Paint::solid(Color::BLACK)];
    n.audio = Some(Box::new(a));
    (d, id)
}

fn px(p: &tiny_skia::Pixmap, x: u32, y: u32) -> [u8; 3] {
    let c = p.pixel(x, y).unwrap();
    [c.red(), c.green(), c.blue()]
}

#[test]
fn the_wave_follows_the_peaks() {
    let (d, id) = strip(false);
    let p = render_node(&d, id, 1.0).unwrap();
    // Loud half: green near the top and bottom; silent half: black there, a hairline in the middle.
    assert_eq!(px(&p, 300, 10), [0, 255, 0]);
    assert_eq!(px(&p, 300, 90), [0, 255, 0]);
    assert_eq!(px(&p, 150, 10), [0, 0, 0]);
    assert!(px(&p, 150, 50)[1] > 0, "{:?}", px(&p, 150, 50));
}

#[test]
fn markers_and_ruler_draw() {
    let (d, id) = strip(true);
    let p = render_node(&d, id, 1.0).unwrap();
    // The marker at 1 s = x 100, below the ruler.
    let m = px(&p, 100, 60);
    assert!(m[0] > 150 && m[1] < 120, "{m:?}");
    // The ruler band darkens the top; its ticks are white.
    let row: Vec<[u8; 3]> = (0..400).map(|x| px(&p, x, 14)).collect();
    assert!(row.iter().any(|c| c[0] > 100 && c[1] > 100 && c[2] > 100), "ticks");
}

#[test]
fn junk_clips_draw_without_panicking() {
    let (mut d, id) = strip(true);
    let n = d.get_mut(id).unwrap();
    if let Some(a) = n.audio.as_mut() {
        a.start = f64::NAN;
        a.end = f64::INFINITY;
        a.fps = 0.0;
        a.peak_rate = u32::MAX;
        a.markers.push(AudioMarker { time: f64::NAN, label: "x".repeat(5000), color: None });
    }
    let _ = render_node(&d, id, 1.0);
    let n = d.get_mut(id).unwrap();
    n.width = 0.5;
    n.height = 1e-9;
    let _ = render_node(&d, id, 64.0);
}

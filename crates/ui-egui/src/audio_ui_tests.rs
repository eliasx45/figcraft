//! Hovering an audio strip finds it however deeply it's nested.
#![allow(clippy::unwrap_used)]

use figcraft_doc::{AudioClip, Document, NodeKind};
use figcraft_geom::Point;

use crate::audio_ui::audio_at;

#[test]
fn hover_finds_an_audio_layer_nested_in_auto_layout_frames() {
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let board = d.create(page, NodeKind::Frame, "Board", 100.0, 100.0, 600.0, 300.0).unwrap();
    let card = d.create(board, NodeKind::Frame, "Card", 20.0, 20.0, 520.0, 200.0).unwrap();
    let strip = d.create(card, NodeKind::Frame, "Audio · S1", 20.0, 100.0, 480.0, 70.0).unwrap();
    d.get_mut(strip).unwrap().audio = Some(Box::new(AudioClip::new("h", "x.wav", 1.0, 48_000, 1)));
    // Page point inside the strip: 100 + 20 + 20 = 140 across, 100 + 20 + 100 = 220 down.
    assert_eq!(audio_at(&d, page, Point::new(150.0, 230.0)), Some(strip));
    // On the card but off the strip: nothing.
    assert_eq!(audio_at(&d, page, Point::new(150.0, 150.0)), None);
    // Hidden strips don't take the hover.
    d.get_mut(card).unwrap().visible = false;
    assert_eq!(audio_at(&d, page, Point::new(150.0, 230.0)), None);
}

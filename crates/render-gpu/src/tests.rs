//! What needs no GPU: which tiles fall back and why.
#![allow(clippy::unwrap_used)]

use figcraft_color::{Color, Paint};
use figcraft_doc::{Document, NodeKind};

#[test]
fn an_edit_rebuilds_only_the_top_level_layers_it_touched() {
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let a = d.create(page, NodeKind::Rectangle, "A", 0.0, 0.0, 10.0, 10.0).unwrap();
    let f = d.create(page, NodeKind::Frame, "F", 50.0, 0.0, 100.0, 100.0).unwrap();
    let inner = d.create(f, NodeKind::Rectangle, "I", 10.0, 10.0, 10.0, 10.0).unwrap();
    let old = d.clone();
    d.get_mut(inner).unwrap().fills = vec![Paint::solid(Color::from_hex("FF0000").unwrap())];
    let dirty = super::dirty_tops(&old, &d, page).unwrap();
    assert!(dirty.contains(&f) && !dirty.contains(&a));
}

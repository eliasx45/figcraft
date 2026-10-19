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

#[test]
fn layers_the_gpu_cant_draw_are_found_without_building_them() {
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let f = d.create(page, NodeKind::Frame, "F", 0.0, 0.0, 100.0, 100.0).unwrap();
    let m = d.create(f, NodeKind::Rectangle, "M", 10.0, 10.0, 10.0, 10.0).unwrap();
    // Masks inside a layer are drawn on the GPU; one among the page's own layers isn't.
    d.get_mut(m).unwrap().is_mask = true;
    assert_eq!(crate::scene::cpu_only(&d, f), None);
    assert_eq!(crate::scene::cpu_only(&d, m), Some("masks"));
    d.get_mut(m).unwrap().fills = vec![Paint::pattern(1)];
    assert_eq!(crate::scene::cpu_only(&d, f), Some("pattern fills"));
    // A hidden layer draws nothing.
    d.get_mut(m).unwrap().visible = false;
    assert_eq!(crate::scene::cpu_only(&d, f), None);
}

#[test]
fn blurs_are_made_smaller_as_the_cpu_casts_soft_shadows() {
    use crate::effects::{factor, texture_size};
    assert_eq!(factor(4.0), 1);
    assert_eq!(factor(15.9), 1);
    assert_eq!(factor(16.0), 2);
    assert_eq!(factor(100.0), 8);
    assert_eq!(factor(1e9), 64);
    assert_eq!(factor(f64::NAN), 1);
    let r = figcraft_geom::Rect::new(0.0, 0.0, 1000.0, 500.0);
    assert_eq!(texture_size(r, 2.0, 4), Some((500, 250)));
    // Too large for one texture: the layer falls back.
    assert_eq!(texture_size(r, 20.0, 1), None);
}

#[test]
fn images_far_from_the_view_are_left_out_until_they_are_near() {
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let mut pix = figcraft_render::Pixmap::new(4, 4).unwrap();
    pix.fill(tiny_skia::Color::from_rgba8(10, 20, 30, 255));
    let png = figcraft_render::encode_png(&pix).unwrap();
    d.images.insert("gpu-tests-far".into(), figcraft_doc::ImageData { bytes: std::sync::Arc::new(png), width: 4, height: 4 });
    let f = d.create(page, NodeKind::Frame, "F", 0.0, 0.0, 10_000.0, 100.0).unwrap();
    let far = d.create(f, NodeKind::Rectangle, "Far", 9_000.0, 0.0, 50.0, 50.0).unwrap();
    d.get_mut(far).unwrap().fills = vec![Paint::image("gpu-tests-far", figcraft_color::ImageScaleMode::Fill)];
    let build = |area| {
        let mut images = crate::Images::default();
        let mut b = crate::Builder {
            doc: &d,
            skip: &[],
            images: &mut images,
            scale: 1.0,
            sampled: false,
            lod: false,
            top: f,
            fx: Vec::new(),
            open: 0,
            in_fx: 0,
            under: None,
            area,
            partial: false,
        };
        let mut sc = vello::Scene::new();
        b.node(&mut sc, f, figcraft_geom::Affine::IDENTITY, 0).unwrap();
        (b.partial, b.sampled)
    };
    assert_eq!(build(Some(figcraft_geom::Rect::new(0.0, 0.0, 500.0, 100.0))), (true, false));
    assert_eq!(build(Some(figcraft_geom::Rect::new(8_000.0, 0.0, 9_500.0, 100.0))), (false, true));
    assert_eq!(build(None), (false, true));
}

#[test]
fn a_layer_the_build_never_reaches_doesnt_send_its_top_level_layer_to_the_cpu() {
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let f = d.create(page, NodeKind::Frame, "F", 0.0, 0.0, 100.0, 100.0).unwrap();
    let g = d.create(f, NodeKind::Group, "G", 0.0, 0.0, 10.0, 10.0).unwrap();
    let r = d.create(g, NodeKind::Rectangle, "P", 10.0, 10.0, 10.0, 10.0).unwrap();
    d.get_mut(r).unwrap().fills = vec![Paint::pattern(1)];
    assert_eq!(crate::scene::cpu_only(&d, f), Some("pattern fills"));
    // Inside a hidden group.
    d.get_mut(g).unwrap().visible = false;
    assert_eq!(crate::scene::cpu_only(&d, f), None);
}

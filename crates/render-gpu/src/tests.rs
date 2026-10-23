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
            drawn: std::collections::HashMap::new(),
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

#[test]
fn the_atlas_mirror_places_images_as_vello_does() {
    // Scenes of new and earlier images of all sizes, through vello's own resolver: it uploads a
    // new image exactly when the mirror gives it a place, its atlas grows as the mirror's, and
    // images get left out once it is full.
    use vello::{kurbo, peniko};
    let mut resolver = vello::low_level::vello_encoding::Resolver::new();
    let mut mirror = super::Mirror::new();
    let mut seed = 7u64;
    let mut rand = move |n: u64| {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % n.max(1)
    };
    let mut seen: Vec<peniko::ImageData> = Vec::new();
    let (mut placed, mut dropped) = (0, 0);
    let mut packed = Vec::new();
    for round in 0..80 {
        let mut sc = vello::Scene::new();
        super::keep_atlas(&mut sc);
        let mut fresh = Vec::new();
        for _ in 0..rand(10) {
            let im = match seen.get(rand(seen.len() as u64) as usize) {
                Some(im) if rand(3) == 0 => im.clone(),
                _ => {
                    let im = super::scene::placeholder(1 + rand(3500) as u32, 1 + rand(3500) as u32, true);
                    fresh.push(im.data.id());
                    seen.push(im.clone());
                    im
                }
            };
            let brush = peniko::ImageBrush { image: im, sampler: peniko::ImageSampler::default() };
            sc.fill(peniko::Fill::NonZero, kurbo::Affine::IDENTITY, &brush, None, &kurbo::Rect::new(0.0, 0.0, 1.0, 1.0));
        }
        // Now and then a render without images (vello then evicts sooner).
        if rand(5) == 0 {
            sc = vello::Scene::new();
            super::keep_atlas(&mut sc);
            fresh.clear();
        }
        mirror.resolve(&super::scene_images(&sc));
        let (_, _, up) = resolver.resolve(sc.encoding(), &mut packed);
        let size = mirror.atlas.size();
        assert_eq!((up.width, up.height), (size.width as u32, size.height as u32), "round {round}");
        let uploaded: std::collections::HashSet<u64> = up.images.iter().map(|(im, _, _)| im.data.id()).collect();
        for id in &fresh {
            assert_eq!(uploaded.contains(id), mirror.map.contains_key(id), "round {round}");
            if mirror.map.contains_key(id) {
                placed += 1;
            } else {
                dropped += 1;
            }
        }
    }
    assert!(placed > 50 && dropped > 5, "{placed} placed, {dropped} left out");
}

#[test]
fn a_render_after_two_without_images_has_the_whole_atlas() {
    // vello evicts an image only once two renders didn't use it.
    let mut m = super::Mirror::new();
    let quarter = |id| (id, 4096, 4096);
    assert!(m.resolve(&[quarter(1), quarter(2), quarter(3), quarter(4)]));
    assert!(!m.resolve(&[quarter(5)]), "the four of the last render are kept");
    let mut m2 = super::Mirror::new();
    m2.resolve(&[quarter(1), quarter(2), quarter(3), quarter(4)]);
    m2.resolve(&[]);
    m2.resolve(&[]);
    assert!(m2.resolve(&[quarter(5), quarter(6), quarter(7), quarter(8)]));
}

#[test]
fn audio_strips_nested_in_frames_draw_their_waveform_on_the_cpu() {
    // The GPU scene has no waveforms: a board holding an audio strip (selected or not) must fall back,
    // or the strip shows as its bare fill.
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let board = d.create(page, NodeKind::Frame, "Board", 0.0, 0.0, 600.0, 200.0).unwrap();
    let card = d.create(board, NodeKind::Frame, "Card", 10.0, 10.0, 500.0, 100.0).unwrap();
    let strip = d.create(card, NodeKind::Frame, "Audio · S1", 10.0, 10.0, 480.0, 70.0).unwrap();
    assert_eq!(crate::scene::cpu_only(&d, board), None);
    d.get_mut(strip).unwrap().audio = Some(Box::new(figcraft_doc::AudioClip::new("h", "x.wav", 1.0, 48_000, 1)));
    assert_eq!(crate::scene::cpu_only(&d, board), Some("audio waveforms"));
}

#[test]
fn soft_effects_are_dithered_at_the_scenes_pixels() {
    // A large, faint glow is blurred in a texture many times smaller than the screen. Dithered
    // there, the image's own sampling smooths the dither away when it is drawn larger, and each
    // 8-bit level shows as a ring many pixels wide (banding): its last pass scales it up to the
    // scene's pixels and dithers there.
    let src = include_str!("fx.wgsl");
    for entry in ["fn up_tint", "fn up_rgba"] {
        let body = src.split(entry).nth(1).and_then(|b| b.split("\n}\n").next()).unwrap();
        assert!(body.contains("bilinear(c)") && body.contains("dither(c)"), "{entry} doesn't scale up and dither");
    }
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let glow =
        d.create(page, NodeKind::Ellipse { start: 0.0, sweep: std::f64::consts::TAU, inner: 0.0 }, "BG Glow", 0.0, 0.0, 1100.0, 800.0).unwrap();
    let mut blur = figcraft_doc::Effect::layer_blur();
    if let figcraft_doc::EffectKind::LayerBlur(b) = &mut blur.kind {
        b.radius = 260.0;
    }
    let n = d.get_mut(glow).unwrap();
    n.fills = vec![Paint::solid(Color::from_hex("7B2CB0").unwrap())];
    n.opacity = 0.32;
    n.effects = vec![blur];
    let mut images = crate::Images::default();
    let mut b = crate::Builder {
        doc: &d,
        skip: &[],
        images: &mut images,
        scale: 1.0,
        sampled: false,
        lod: false,
        top: glow,
        fx: Vec::new(),
        drawn: std::collections::HashMap::new(),
        open: 0,
        in_fx: 0,
        under: None,
        area: None,
        partial: false,
    };
    let mut sc = vello::Scene::new();
    b.node(&mut sc, glow, figcraft_geom::Affine::IDENTITY, 0).unwrap();
    let blurs: Vec<_> = b.fx.iter().flat_map(|f| &f.passes).filter(|p| matches!(p.kind, crate::effects::Kind::Blur { .. })).collect();
    assert_eq!(blurs.len(), 1);
    let p = blurs[0];
    // Made 16 times smaller, drawn at (nearly) the scene's pixels: at most the atlas's share.
    assert!(p.k >= 8 && p.out * 4 <= p.k, "made at 1/{}, drawn at 1/{}", p.k, p.out);
    let (w, h) = crate::effects::texture_size(p.region, 1.0, p.out).unwrap();
    assert_eq!((p.image.width, p.image.height), (w, h));
    assert!(u64::from(w) * u64::from(h) <= crate::effects::MAX_UP_PX);
    assert!(
        p.out == 1
            || crate::effects::texture_size(p.region, 1.0, p.out / 2).is_none_or(|(w, h)| u64::from(w) * u64::from(h) > crate::effects::MAX_UP_PX)
    );
    assert_eq!(p.src, crate::effects::texture_size(p.region, 1.0, p.k).unwrap());
}

#[test]
fn gradients_resolve_in_16_bits() {
    // A dark card's fill (0.09 to 0.05 grey) spans ten 8-bit levels: ramps in 8 bits draw it as
    // ten flat bands. The patched vello (third_party/) keeps 16 bits, as two rows of 8.
    use vello::{kurbo, peniko};
    let mut sc = vello::Scene::new();
    let g = peniko::Gradient::new_linear((0.0, 0.0), (0.0, 586.0))
        .with_stops([peniko::Color::new([0.09, 0.09, 0.11, 1.0]), peniko::Color::new([0.05, 0.05, 0.067, 1.0])]);
    sc.fill(peniko::Fill::NonZero, kurbo::Affine::IDENTITY, &g, None, &kurbo::Rect::new(0.0, 0.0, 330.0, 586.0));
    let mut resolver = vello::low_level::vello_encoding::Resolver::new();
    let mut packed = Vec::new();
    let (_, ramps, _) = resolver.resolve(sc.encoding(), &mut packed);
    assert_eq!(ramps.height, 2, "one ramp, two rows");
    let w = ramps.width as usize;
    let red = |i: usize| {
        let (hi, lo) = (ramps.data[i].to_ne_bytes()[0], ramps.data[w + i].to_ne_bytes()[0]);
        f64::from(u16::from(hi) << 8 | u16::from(lo)) / 65535.0
    };
    for i in [0, w / 2, w - 1] {
        let want = 0.09 + (0.05 - 0.09) * i as f64 / (w - 1) as f64;
        assert!((red(i) - want).abs() < 2.0 / 65535.0, "sample {i}: {} for {want}", red(i));
    }
    let levels: std::collections::BTreeSet<u16> = (0..w).map(|i| (red(i) * 65535.0).round() as u16).collect();
    assert!(levels.len() > 100, "{} levels", levels.len());
}

#[test]
#[ignore = "needs a GPU"]
fn a_clipped_frames_own_gradient_is_dithered() {
    // vello keeps what lies under a clip layer in an 8-bit blend stack: rounded exactly there, a
    // card's own dark gradient fill (under its clipped children) showed flat bands tens of pixels
    // tall, while the children drawn inside the clip were dithered.
    use figcraft_color::{ColorStop, Gradient, GradientKind};
    use figcraft_render::tiles::{Painted, TilePainter, tiles_in};
    let mut d = Document::new("t");
    let page = d.first_page().unwrap();
    let card = d.create(page, NodeKind::Frame, "Card", 0.0, 0.0, 330.0, 586.0).unwrap();
    let inner = d.create(card, NodeKind::Rectangle, "Inner", 100.0, 100.0, 50.0, 50.0).unwrap();
    let stop = |position, hex| ColorStop { position, color: Color::from_hex(hex).unwrap() };
    let n = d.get_mut(card).unwrap();
    n.clip = true;
    n.fills = vec![Paint::gradient(Gradient {
        kind: GradientKind::Linear,
        stops: vec![stop(0.0, "17171C"), stop(1.0, "0D0D11")],
        from: (0.5, 0.0),
        to: (0.5, 1.0),
        width: (0.0, 0.0),
    })];
    d.get_mut(inner).unwrap().fills = vec![Paint::solid(Color::from_hex("808080").unwrap())];
    let Ok(gpu) = crate::Gpu::new() else { return };
    let mut p = crate::GpuTilePainter::new(gpu);
    p.keep_on_gpu(false);
    let doc = std::sync::Arc::new(d);
    let r = figcraft_geom::Rect::new(160.0, 0.0, 330.0, 586.0);
    let out = p.paint(&doc, page, &tiles_in(r, 1, 64).unwrap(), &figcraft_render::Options::default(), &|| false);
    // 16×16 blocks of the card's fill (right of the inner square) all of one colour.
    let (mut flat, mut blocks) = (0, 0);
    for (k, painted) in out {
        let Some(Painted::Pixels(px)) = painted else { continue };
        let (t, s, w) = (k.rect(), k.scale(), px.width() as usize);
        for by in 0..px.height() as usize / 16 {
            for bx in 0..w / 16 {
                let (x, y) = (t.x0 + (bx * 16) as f64 / s, t.y0 + (by * 16) as f64 / s);
                if x < 170.0 || x + 16.0 / s > 320.0 || y < 10.0 || y + 16.0 / s > 576.0 {
                    continue;
                }
                let first = &px.data()[(by * 16 * w + bx * 16) * 4..][..3];
                let same = (0..16).all(|yy| (0..16).all(|xx| &px.data()[((by * 16 + yy) * w + bx * 16 + xx) * 4..][..3] == first));
                blocks += 1;
                flat += usize::from(same);
            }
        }
    }
    assert!(blocks > 100, "{blocks} blocks");
    assert!(flat * 20 < blocks, "{flat} of {blocks} blocks flat (banded)");
}

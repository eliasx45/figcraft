//! Drop shadows blurred by many pixels are cast from a copy of the layer drawn smaller
//! (`draw::SoftCaster`): they come out as the full-size ones, and the layer itself only covers the
//! view and the other effects' margin, not the shadows' (a large page zoomed in was slow).

use figcraft_color::{Color, Paint};
use figcraft_doc::{Document, Effect, EffectKind, NodeId, NodeKind, Shadow};

use super::*;

fn shadow(radius: f64, offset: (f64, f64), spread: f64, show_behind: bool) -> EffectKind {
    let color = Color { a: 0.6, ..Color::BLACK };
    EffectKind::DropShadow(Shadow { color, offset, radius, spread, show_behind, ..Default::default() })
}

/// A page with a white `w` × `h` rectangle at `(x, y)` with `effects`.
fn page(x: f64, y: f64, w: f64, h: f64, effects: Vec<EffectKind>) -> (Document, NodeId) {
    let mut d = Document::new("soft");
    let p = d.first_page().unwrap();
    let n = d.create(p, NodeKind::Rectangle, "R", x, y, w, h).unwrap();
    let node = d.get_mut(n).unwrap();
    node.fills = vec![Paint::solid(Color::WHITE)];
    node.effects = effects.into_iter().map(|kind| Effect { kind, visible: true }).collect();
    (d, p)
}

fn render(d: &Document, p: NodeId, v: &View, exact: bool) -> Pixmap {
    draw::EXACT_SHADOWS.with(|e| e.set(exact));
    let pix = render_view_single(d, p, v, &Options::default()).unwrap();
    draw::EXACT_SHADOWS.with(|e| e.set(false));
    pix
}

/// The mean and largest difference of two renders, per channel (0–255).
fn diff(a: &Pixmap, b: &Pixmap) -> (f64, u8) {
    let (mut sum, mut max) = (0u64, 0u8);
    for (x, y) in a.data().iter().zip(b.data()) {
        let d = x.abs_diff(*y);
        sum += u64::from(d);
        max = max.max(d);
    }
    (sum as f64 / a.data().len() as f64, max)
}

fn alpha(p: &Pixmap, x: u32, y: u32) -> u8 {
    p.data()[((y * p.width() + x) * 4 + 3) as usize]
}

#[test]
fn a_soft_shadow_matches_the_full_size_one() {
    // Blurred by 100 and 50 device pixels: cast 8 and 4 times smaller. One hides behind the
    // rectangle, one spreads.
    let effects = vec![shadow(400.0, (80.0, 120.0), 0.0, false), shadow(200.0, (-40.0, 30.0), 60.0, true)];
    let (d, p) = page(600.0, 400.0, 1000.0, 700.0, effects);
    let v = View { origin: (0.0, 0.0), scale: 0.5, width: 1100, height: 800 };
    assert!(draw::soft_factor_of(d.nodes().find(|n| n.name == "R").unwrap(), 0.5) == Some(4));
    let (soft, exact) = (render(&d, p, &v, false), render(&d, p, &v, true));
    let (mean, max) = diff(&soft, &exact);
    assert!(mean < 0.5 && max <= 6, "mean {mean:.2}, max {max}");
    // Inside the rectangle it is the rectangle (the shadow hides behind it, the other shows
    // through nothing: the fill is opaque).
    assert_eq!(alpha(&soft, 550, 400), 255);
    assert_eq!(&soft.data()[(400 * 1100 + 550) * 4..][..4], &exact.data()[(400 * 1100 + 550) * 4..][..4]);
}

#[test]
fn a_soft_shadow_reaches_in_from_a_layer_off_the_view() {
    // The rectangle ends 100 left of the view; its shadow, offset 300 right, reaches well in.
    let (d, p) = page(-1100.0, 0.0, 1000.0, 600.0, vec![shadow(300.0, (300.0, 0.0), 0.0, false)]);
    let v = View { origin: (0.0, 0.0), scale: 1.0, width: 800, height: 600 };
    let (soft, exact) = (render(&d, p, &v, false), render(&d, p, &v, true));
    assert!(alpha(&exact, 50, 300) > 40, "the shadow reaches in");
    let (mean, max) = diff(&soft, &exact);
    assert!(mean < 0.5 && max <= 6, "mean {mean:.2}, max {max}");
}

#[test]
fn a_huge_layer_with_soft_shadows_renders_quickly_zoomed_in() {
    // 22000 × 5600 with five shadows blurred by up to ~800 at 36 %: the layer covers the view
    // (and the other effects' margin), not the shadows' 2048 pixels around it, in every band.
    let effects = (0..5).map(|i| shadow(266.0 + 128.0 * f64::from(i), (0.0, 40.0), 0.0, true)).collect();
    let (d, p) = page(0.0, 0.0, 22_000.0, 5600.0, effects);
    let v = View { origin: (8000.0, 2000.0), scale: 0.36, width: 2400, height: 1800 };
    let t = std::time::Instant::now();
    let pix = render_view(&d, p, &v, &Options::default()).unwrap();
    assert!(t.elapsed().as_secs_f64() < 3.0, "{:?}", t.elapsed());
    assert_eq!(alpha(&pix, 1200, 900), 255);
}

#[test]
fn a_soft_shadow_of_a_blurred_layer_matches_the_full_size_one() {
    // Cut by the view's edge, the blurred shape still casts from what lies beyond it.
    let blur = EffectKind::LayerBlur(figcraft_doc::Blur { radius: 120.0, ..Default::default() });
    let (d, p) = page(-300.0, 100.0, 900.0, 500.0, vec![blur, shadow(300.0, (60.0, 60.0), 0.0, true)]);
    let v = View { origin: (0.0, 0.0), scale: 0.5, width: 600, height: 400 };
    let (soft, exact) = (render(&d, p, &v, false), render(&d, p, &v, true));
    let (mean, max) = diff(&soft, &exact);
    // The blur drawn smaller rounds a little differently (half a level on average).
    assert!(mean < 1.0 && max <= 6, "mean {mean:.2}, max {max}");
}

#[test]
fn upscaling_keeps_flat_alpha_flat_and_ramps_between_pixel_centres() {
    // Two pixels: 0 then 255, scaled up 4 times from 2 pixels in: 0 up to the first centre
    // (scaled-up x 2), 255 from the second (x 6), a straight ramp between.
    let mut small = Pixmap::new(2, 1).unwrap();
    small.data_mut()[4..8].copy_from_slice(&[255; 4]);
    let up = crate::fx::upscale_alpha(&small, 4, (0, 0), 8, 1).unwrap();
    let a: Vec<u8> = up.data().chunks(4).map(|c| c[3]).collect();
    assert_eq!(a, [0, 0, 32, 96, 159, 223, 255, 255]);
    let mut flat = Pixmap::new(3, 3).unwrap();
    flat.data_mut().fill(128);
    let up = crate::fx::upscale_alpha(&flat, 8, (5, 3), 13, 17).unwrap();
    assert!(up.data().iter().all(|v| *v == 128));
}

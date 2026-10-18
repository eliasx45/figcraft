//! GPU tiles (vello) vs CPU tiles (tiny-skia, the oracle) on the render test scenes, through the
//! painters the canvas uses. Needs a GPU adapter, so it's ignored by default:
//! `cargo test -p figcraft-render-gpu --release -- --ignored --nocapture`
//! (`FIGCRAFT_PARITY_DIR=dir` keeps both images of every tile that differs).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use figcraft_color::{BlendMode, Color, ColorStop, Gradient, GradientKind, ImageScaleMode, Paint};
use figcraft_doc::{BoolOp, Document, ImageData, NodeId, NodeKind, StrokeAlign, TextData};
use figcraft_geom::Affine;
use figcraft_render::tiles::{CpuTilePainter, Painted, TILE_PX, TileKey, TilePainter, content_bounds, level_at_most, tiles_in};
use figcraft_render::{Options, Pixmap, View};
use figcraft_render_gpu::{Gpu, GpuTilePainter};

fn rect(d: &mut Document, parent: NodeId, x: f64, y: f64, w: f64, h: f64, c: &str) -> NodeId {
    let r = d.create(parent, NodeKind::Rectangle, "R", x, y, w, h).unwrap();
    d.get_mut(r).unwrap().fills = vec![Paint::solid(Color::from_hex(c).unwrap())];
    r
}

fn ellipse() -> NodeKind {
    NodeKind::Ellipse { start: 0.0, sweep: std::f64::consts::TAU, inner: 0.0 }
}

fn new_doc() -> (Document, NodeId) {
    let d = Document::new("parity");
    let p = d.first_page().unwrap();
    (d, p)
}

fn gradient(kind: GradientKind) -> Paint {
    let g = Gradient {
        kind,
        stops: vec![
            ColorStop { position: 0.0, color: Color::from_hex("FF0000").unwrap() },
            ColorStop { position: 0.5, color: Color::from_hex("00CC66").unwrap() },
            ColorStop { position: 1.0, color: Color::from_hex("0000FF").unwrap() },
        ],
        from: (0.5, 0.5),
        to: (1.0, 0.5),
        width: (0.5, 1.0),
    };
    Paint::gradient(if kind == GradientKind::Linear { Gradient { from: (0.0, 0.0), to: (1.0, 1.0), width: (0.0, 1.0), ..g } } else { g })
}

fn photo() -> Vec<u8> {
    let mut p = Pixmap::new(64, 64).unwrap();
    for (i, px) in p.data_mut().chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i % 64) as u8, (i / 64) as u8);
        px.copy_from_slice(&[x * 4, y * 4, 255 - x * 2, 255]);
    }
    figcraft_render::encode_png(&p).unwrap()
}

/// (name, document, page, view) for every scene.
fn scenes() -> Vec<(String, Document, NodeId, View)> {
    let v = |w, h, s| View { origin: (0.0, 0.0), scale: s, width: w, height: h };
    let mut out = Vec::new();
    {
        let (mut d, p) = new_doc();
        rect(&mut d, p, 10.0, 10.0, 50.0, 30.0, "FF0000");
        out.push(("solid rect".into(), d.clone(), p, v(100, 100, 1.0)));
        out.push(("solid rect @2x".into(), d, p, v(200, 200, 2.0)));
    }
    {
        let (mut d, p) = new_doc();
        let a = rect(&mut d, p, 10.0, 10.0, 60.0, 60.0, "000000");
        d.get_mut(a).unwrap().opacity = 0.5;
        let b = rect(&mut d, p, 40.0, 40.0, 60.0, 60.0, "FF8800");
        d.get_mut(b).unwrap().blend = BlendMode::Multiply;
        out.push(("opacity + multiply".into(), d, p, v(120, 120, 1.0)));
    }
    for kind in [GradientKind::Linear, GradientKind::Radial, GradientKind::Angular, GradientKind::Diamond] {
        let (mut d, p) = new_doc();
        let r = rect(&mut d, p, 10.0, 10.0, 160.0, 100.0, "000000");
        d.get_mut(r).unwrap().fills = vec![gradient(kind)];
        out.push((format!("{kind:?} gradient"), d, p, v(180, 120, 1.0)));
    }
    {
        let (mut d, p) = new_doc();
        let f = d.create(p, NodeKind::Frame, "F", 10.0, 10.0, 80.0, 80.0).unwrap();
        rect(&mut d, f, 40.0, 40.0, 80.0, 80.0, "222222");
        let g = d.create(p, NodeKind::Frame, "G", 110.0, 10.0, 80.0, 80.0).unwrap();
        d.get_mut(g).unwrap().corners.radii = [20.0; 4];
        let r = rect(&mut d, g, -20.0, 10.0, 120.0, 40.0, "3366FF");
        let n = d.get_mut(r).unwrap();
        n.transform = Affine::rotate(0.4) * n.transform;
        out.push(("frame clips (square, rounded)".into(), d, p, v(200, 100, 1.0)));
    }
    {
        let (mut d, p) = new_doc();
        for (i, align) in [StrokeAlign::Inside, StrokeAlign::Center, StrokeAlign::Outside].into_iter().enumerate() {
            let e = d.create(p, ellipse(), "E", 10.0 + i as f64 * 70.0, 10.0, 50.0, 40.0).unwrap();
            let n = d.get_mut(e).unwrap();
            n.fills = vec![Paint::solid(Color::WHITE)];
            n.strokes = vec![Paint::solid(Color::BLACK)];
            n.stroke.weight = 5.0;
            n.stroke.align = align;
        }
        let r = rect(&mut d, p, 10.0, 70.0, 190.0, 20.0, "FFFFFF");
        let n = d.get_mut(r).unwrap();
        n.strokes = vec![Paint::solid(Color::from_hex("CC0000").unwrap())];
        n.stroke.weight = 3.0;
        n.stroke.dashes = vec![8.0, 4.0];
        out.push(("strokes (inside, center, outside, dashed)".into(), d, p, v(220, 100, 1.0)));
    }
    {
        let (mut d, p) = new_doc();
        let b = d.create(p, NodeKind::BooleanOp { op: BoolOp::Subtract }, "B", 0.0, 0.0, 100.0, 100.0).unwrap();
        d.get_mut(b).unwrap().fills = vec![Paint::solid(Color::BLACK)];
        d.create(b, NodeKind::Rectangle, "A", 0.0, 0.0, 100.0, 100.0).unwrap();
        d.create(b, ellipse(), "Hole", 25.0, 25.0, 50.0, 50.0).unwrap();
        out.push(("boolean subtract".into(), d, p, v(100, 100, 1.0)));
    }
    {
        let (mut d, p) = new_doc();
        let mut td = TextData::new("Hello, GPU canvas");
        td.style.font_size = 28.0;
        d.create(p, NodeKind::Text(td), "T", 4.0, 4.0, 300.0, 40.0).unwrap();
        out.push(("text".into(), d.clone(), p, v(300, 48, 1.0)));
        out.push(("text @2x".into(), d, p, v(600, 96, 2.0)));
    }
    {
        let (mut d, p) = new_doc();
        d.images.insert("parity-photo".into(), ImageData { bytes: Arc::new(photo()), width: 64, height: 64 });
        let e = d.create(p, ellipse(), "E", 10.0, 10.0, 120.0, 90.0).unwrap();
        d.get_mut(e).unwrap().fills = vec![Paint::image("parity-photo", ImageScaleMode::Fill)];
        let r = rect(&mut d, p, 140.0, 10.0, 90.0, 90.0, "000000");
        d.get_mut(r).unwrap().fills = vec![Paint::image("parity-photo", ImageScaleMode::Tile)];
        out.push(("image fills (fill, tile)".into(), d, p, v(240, 110, 1.0)));
    }
    {
        let (mut d, p) = new_doc();
        let g = d.create(p, NodeKind::Group, "G", 0.0, 0.0, 10.0, 10.0).unwrap();
        for i in 0..5 {
            let e = d.create(g, ellipse(), "E", 20.0 + f64::from(i) * 15.0, 20.0, 40.0, 40.0).unwrap();
            d.get_mut(e).unwrap().fills = vec![Paint::solid(Color::new(f64::from(i) as f32 / 5.0, 0.3, 0.8, 0.7))];
        }
        let n = d.get_mut(g).unwrap();
        n.transform = Affine::translate((100.0, 0.0)) * Affine::rotate(0.5) * Affine::scale_non_uniform(1.3, 0.8);
        out.push(("rotated, scaled group".into(), d, p, v(200, 160, 1.0)));
    }
    {
        let (d, p) = figcraft_engine::synthetic::large_document(1000).unwrap();
        let b = d.page_content_bounds(p).unwrap();
        out.push(("synthetic 1k @100%".into(), d.clone(), p, View { origin: (0.0, 0.0), scale: 1.0, width: 1440, height: 900 }));
        let s = (1440.0 / b.width()).min(900.0 / b.height());
        out.push(("synthetic 1k fit".into(), d, p, View { origin: (b.x0, b.y0), scale: s, width: 1440, height: 900 }));
    }
    out
}

/// (mean |difference| per channel, max, share of pixels off by more than 16 in any channel).
fn diff(a: &Pixmap, b: &Pixmap) -> (f64, u8, f64) {
    let (mut sum, mut max, mut off) = (0u64, 0u8, 0u64);
    for (pa, pb) in a.data().chunks_exact(4).zip(b.data().chunks_exact(4)) {
        let d = pa.iter().zip(pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
        sum += pa.iter().zip(pb).map(|(x, y)| u64::from(x.abs_diff(*y))).sum::<u64>();
        max = max.max(d);
        off += u64::from(d > 16);
    }
    let px = (a.data().len() / 4).max(1) as f64;
    (sum as f64 / (px * 4.0), max, off as f64 / px)
}

/// A tile's pixels; an empty tile is the page's background.
fn pixels(p: Option<Painted>, doc: &Document, page: NodeId) -> Pixmap {
    match p.expect("painted") {
        Painted::Pixels(p) => p,
        Painted::Native(_) => panic!("asked for pixels"),
        Painted::Empty => {
            let mut pix = Pixmap::new(TILE_PX, TILE_PX).unwrap();
            if let Some(NodeKind::Page { background, .. }) = doc.node(page).map(|n| &n.kind) {
                let c = background;
                pix.fill(tiny_skia::Color::from_rgba(c.r, c.g, c.b, c.a).unwrap());
            }
            pix
        }
    }
}

#[test]
#[ignore = "needs a GPU adapter"]
fn gpu_tiles_match_cpu_tiles() {
    figcraft_text::scan_system_fonts();
    let gpu = match Gpu::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("no GPU adapter ({e}): skipped");
            return;
        }
    };
    let mut gp = GpuTilePainter::new(gpu);
    eprintln!("adapter: {}", gp.adapter());
    let dir = std::env::var_os("FIGCRAFT_PARITY_DIR").map(std::path::PathBuf::from);
    println!("| scene | tiles | mean diff | max diff | pixels off > 16 |\n|---|---:|---:|---:|---:|");
    let mut worst = Vec::new();
    for (name, doc, page, view) in scenes() {
        let doc = Arc::new(doc);
        let level = level_at_most(view.scale).unwrap();
        let Some(keys) = content_bounds(&doc, page).and_then(|b| tiles_in(b, level, 64)) else { continue };
        let opts = Options { lod: false, ..Options::default() };
        let cpu = CpuTilePainter.paint(&doc, page, &keys, &opts, &|| false);
        let gpu = gp.paint(&doc, page, &keys, &opts, &|| false);
        assert_eq!(gp.counts.cpu, 0, "{name}: fell back: {:?}", gp.last_fallback);
        let find = |v: &[(TileKey, Option<Painted>)], k: TileKey| v.iter().find(|(x, _)| *x == k).and_then(|(_, p)| p.clone());
        let (mut mean, mut max, mut off) = (0.0f64, 0u8, 0.0f64);
        for k in &keys {
            let c = pixels(find(&cpu, *k), &doc, page);
            let g = pixels(find(&gpu, *k), &doc, page);
            let (m, x, o) = diff(&c, &g);
            if let Some(dir) = dir.as_ref().filter(|_| o > 0.0) {
                let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
                let at = format!("{slug}-{}-{}-{}", k.level, k.x, k.y);
                std::fs::write(dir.join(format!("{at}-cpu.png")), figcraft_render::encode_png(&c).unwrap()).unwrap();
                std::fs::write(dir.join(format!("{at}-gpu.png")), figcraft_render::encode_png(&g).unwrap()).unwrap();
            }
            mean = mean.max(m);
            max = max.max(x);
            off = off.max(o);
        }
        println!("| {name} | {} | {mean:.3} | {max} | {:.2}% |", keys.len(), off * 100.0);
        // Anti-aliasing and gradient interpolation differ in the low bits; layout must not.
        if !(mean < 2.0 && off < 0.05) {
            worst.push(format!("{name}: mean {mean:.3}, {:.2}% off", off * 100.0));
        }
    }
    assert!(worst.is_empty(), "{worst:#?}");
}

#[test]
#[ignore = "needs a GPU adapter"]
fn effects_fall_back_only_where_they_paint() {
    let Ok(gpu) = Gpu::new() else { return };
    let mut gp = GpuTilePainter::new(gpu);
    let (mut d, p) = new_doc();
    rect(&mut d, p, 10.0, 10.0, 50.0, 50.0, "FF0000");
    let far = rect(&mut d, p, 3000.0, 10.0, 50.0, 50.0, "00FF00");
    d.get_mut(far).unwrap().effects.push(figcraft_doc::Effect {
        kind: figcraft_doc::EffectKind::LayerBlur(figcraft_doc::Blur { radius: 4.0, ..Default::default() }),
        visible: true,
    });
    let doc = Arc::new(d);
    let keys = [TileKey { level: 0, x: 0, y: 0 }, TileKey { level: 0, x: 5, y: 0 }];
    let out = gp.paint(&doc, p, &keys, &Options::default(), &|| false);
    assert_eq!(out.len(), 2);
    assert_eq!((gp.counts.gpu, gp.counts.cpu), (1, 1), "{:?}", gp.counts);
}

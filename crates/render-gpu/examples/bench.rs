//! CPU vs GPU tile painting on a zoomed-out canvas: the tiles a Retina window (2880×1800 device
//! pixels) needs to show a whole synthetic page, painted by `CpuTilePainter` (every core) and by
//! `GpuTilePainter` (vello), cold (the GPU builds its scene) and warm.
//!
//! `cargo run -p figcraft-render-gpu --release --example bench -- [layers] [images]`
//! (`images`: that many 3000×2000 generated photos on the page as well).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use std::sync::Arc;
use std::time::Instant;

use figcraft_color::{ImageScaleMode, Paint};
use figcraft_doc::{Document, ImageData, NodeId, NodeKind};
use figcraft_geom::Rect;
use figcraft_render::Options;
use figcraft_render::tiles::{CpuTilePainter, Painted, TileKey, TilePainter, content_bounds, level_at_most, tiles_in};
use figcraft_render_gpu::{Gpu, GpuTilePainter};

/// A generated photo (smooth colour waves: nothing to compress away), PNG-encoded.
fn photo(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut p = figcraft_render::Pixmap::new(w, h).unwrap();
    for (i, px) in p.data_mut().chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i as u32 % w) as f32, (i as u32 / w) as f32);
        let s = seed as f32;
        let r = ((x * 0.011 + s).sin() * 0.5 + 0.5) * 255.0;
        let g = ((y * 0.017 + s * 1.7).cos() * 0.5 + 0.5) * 255.0;
        let b = (((x + y) * 0.007 + s * 0.3).sin() * 0.5 + 0.5) * 255.0;
        px.copy_from_slice(&[r as u8, g as u8, b as u8, 255]);
    }
    figcraft_render::encode_png(&p).unwrap()
}

fn with_images(d: &mut Document, page: NodeId, n: usize) {
    let b = content_bounds(d, page).unwrap_or(Rect::new(0.0, 0.0, 4000.0, 3000.0));
    for i in 0..n {
        let hash = format!("bench-photo-{i}");
        d.images.insert(hash.clone(), ImageData { bytes: Arc::new(photo(3000, 2000, i as u32)), width: 3000, height: 2000 });
        let (col, row) = ((i % 6) as f64, (i / 6) as f64);
        let r = d.create(page, NodeKind::Rectangle, "Photo", b.x0 + col * 1600.0, b.y1 + 200.0 + row * 1100.0, 1500.0, 1000.0).unwrap();
        d.get_mut(r).unwrap().fills = vec![Paint::image(hash, ImageScaleMode::Fill)];
    }
}

fn time(p: &mut dyn TilePainter, doc: &Arc<Document>, page: NodeId, keys: &[TileKey]) -> (f64, usize) {
    let opts = Options { lod: true, ..Options::default() };
    let t = Instant::now();
    let out = p.paint(doc, page, keys, &opts, &|| false);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    (ms, out.iter().filter(|(_, p)| matches!(p, Some(Painted::Pixels(_)))).count())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let layers: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(10_000);
    let images: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(0);
    figcraft_text::scan_system_fonts();
    let (mut d, page) = figcraft_engine::synthetic::large_document(layers).unwrap();
    with_images(&mut d, page, images);
    let doc = Arc::new(d);
    let b = content_bounds(&doc, page).unwrap();
    let (w, h) = (2880.0, 1800.0);
    let scale = (w / b.width()).min(h / b.height());
    let mut gpu = GpuTilePainter::new(Gpu::new().expect("a GPU adapter"));
    println!("{layers} layers, {images} photos, page {:.0}×{:.0}, fit at {:.3}× on {}", b.width(), b.height(), scale, gpu.adapter());
    for (label, s) in [("fit", scale), ("fit ×4 (zooming in)", scale * 4.0)] {
        let level = level_at_most(s).unwrap();
        let view = Rect::new(b.x0, b.y0, b.x0 + w / s, b.y0 + h / s);
        let keys = tiles_in(view, level, 4096).unwrap();
        // The image pyramids are made once per document: warm them for both painters.
        let _ = time(&mut CpuTilePainter, &doc, page, &keys[..1.min(keys.len())]);
        let (cpu_cold, n) = time(&mut CpuTilePainter, &doc, page, &keys);
        let (cpu, _) = time(&mut CpuTilePainter, &doc, page, &keys);
        let mut fresh = GpuTilePainter::new(Gpu::new().unwrap());
        let (cold, _) = time(&mut fresh, &doc, page, &keys);
        let (warm, _) = time(&mut gpu, &doc, page, &keys);
        let (warm2, _) = time(&mut gpu, &doc, page, &keys);
        // Left on the GPU (what the app shows): until the GPU has finished them.
        gpu.keep_on_gpu(true);
        let mut kept = f64::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let _ = gpu.paint(&doc, page, &keys, &Options { lod: true, ..Options::default() }, &|| false);
            gpu.wait();
            kept = kept.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        let kept_counts = gpu.counts;
        gpu.keep_on_gpu(false);
        println!(
            "{label}: {} tiles ({n} with pixels) | CPU cold {cpu_cold:.1} ms, warm {cpu:.1} ms | GPU cold {cold:.1} ms, warm {:.1} ms, left on the GPU {kept:.1} ms | {:?} | kept {:?} {}",
            keys.len(),
            warm.min(warm2),
            gpu.counts,
            kept_counts,
            gpu.last_fallback.unwrap_or("")
        );
    }
}

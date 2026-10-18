//! GPU canvas tiles: the scene graph as a [vello] scene, rasterised by compute shaders on wgpu,
//! painting the canvas's tiles ([`figcraft_render::tiles`]).
//!
//! The CPU renderer (`figcraft-render`) stays the reference and the fallback: a tile that shows
//! something this doesn't draw yet (effects, masks, pattern fills: see [`Fallback`]) is painted on
//! the CPU, and so is a whole batch in an outline view. Exports, the CLI and MCP never come here.
//!
//! The page scene is kept per top-level layer in page coordinates: a tile is the layers over it
//! through the tile's transform, and an edit rebuilds only the top-level layers it touched.

pub mod scene;

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;

use figcraft_doc::{Document, NodeId, NodeKind};
use figcraft_geom::Rect;
use figcraft_render::tiles::{CpuTilePainter, Painted, TILE_PX, TileKey, TilePainter};
use figcraft_render::{Options, Pixmap, View};
pub use scene::{Builder, Fallback, Images};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};

/// More changed layers than this rebuild the whole scene.
const MAX_CHANGED: usize = 4096;
/// Tiles per side of a block: the tiles of a block render in one GPU pass (the scene goes to
/// the GPU once for all of them, not once per tile).
const BLOCK: i64 = 4;
/// The render target's side, device pixels.
const TARGET_PX: u32 = TILE_PX * BLOCK as u32;

/// A wgpu device (the window's, or one of its own) with a vello renderer.
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    target: Option<Target>,
    /// The adapter's name and backend (for reports).
    pub adapter: String,
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Rows padded to wgpu's copy alignment.
    readback: wgpu::Buffer,
}

/// The bytes of a target's row, padded as wgpu copies want.
const ROW: u32 = (TARGET_PX * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

impl Gpu {
    /// A device of its own on the default (high-performance) adapter (tests, benchmarks).
    pub fn new() -> Result<Self, String> {
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
        )
        .map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("figcraft-gpu-tiles"), ..Default::default() }))
                .map_err(|e| e.to_string())?;
        // wgpu panics on device errors by default; a tile must not take the app down.
        device.on_uncaptured_error(Arc::new(|e| log::error!("gpu tiles: {e}")));
        Self::shared(device, queue, format!("{} ({:?})", info.name, info.backend))
    }

    /// On a device the window already has (its textures can then take the tiles without a copy
    /// through the CPU later). Fails where the device can't run vello's compute shaders.
    pub fn shared(device: wgpu::Device, queue: wgpu::Queue, adapter: String) -> Result<Self, String> {
        let renderer = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { device, queue, renderer, target: None, adapter })
    }

    fn target(&mut self) -> &Target {
        self.target.get_or_insert_with(|| {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("figcraft-gpu-tiles"),
                size: wgpu::Extent3d { width: TARGET_PX, height: TARGET_PX, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("figcraft-gpu-tiles-readback"),
                size: u64::from(ROW) * u64::from(TARGET_PX),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            Target { texture, view, readback }
        })
    }

    /// Render `scene` (in device pixels) over `base` as `cols`×`rows` tiles, and read them back
    /// as premultiplied pixmaps, row by row (waits for the GPU).
    fn render(&mut self, scene: &Scene, cols: u32, rows: u32, base: figcraft_color::Color, counts: &mut Counts) -> Result<Vec<Pixmap>, Fallback> {
        let started = std::time::Instant::now();
        let (cols, rows) = (cols.clamp(1, BLOCK as u32), rows.clamp(1, BLOCK as u32));
        let (width, height) = (cols * TILE_PX, rows * TILE_PX);
        let params = RenderParams {
            base_color: vello::peniko::Color::new([base.r, base.g, base.b, base.a]),
            width,
            height,
            antialiasing_method: AaConfig::Area,
        };
        self.target();
        let Some(t) = &self.target else { return Err("no render target") };
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let r = self.renderer.render_to_texture(&self.device, &self.queue, scene, &t.view, &params);
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("figcraft-gpu-tiles-copy") });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo {
                buffer: &t.readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(ROW), rows_per_image: None },
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("gpu tiles: {e}");
            return Err("GPU error");
        }
        r.map_err(|e| {
            log::warn!("gpu tiles: {e}");
            "GPU render failed"
        })?;
        counts.submit_ms += ms(started);
        let t0 = std::time::Instant::now();
        let slice = t.readback.slice(..u64::from(ROW) * u64::from(height));
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| _ = tx.send(r.is_ok()));
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        if rx.try_recv() != Ok(true) {
            return Err("GPU readback failed");
        }
        counts.wait_ms += ms(t0);
        let t0 = std::time::Instant::now();
        let tiles = {
            let Ok(data) = slice.get_mapped_range() else {
                t.readback.unmap();
                return Err("GPU readback failed");
            };
            (0..rows).flat_map(|r| (0..cols).map(move |c| (r, c))).filter_map(|(r, c)| cut(&data, c, r)).collect()
        };
        t.readback.unmap();
        counts.cut_ms += ms(t0);
        Ok(tiles)
    }
}

/// Tile (`col`, `row`) of mapped target rows as a premultiplied pixmap (vello writes straight
/// alpha).
fn cut(data: &[u8], col: u32, row: u32) -> Option<Pixmap> {
    let mut pix = Pixmap::new(TILE_PX, TILE_PX)?;
    let w = TILE_PX as usize * 4;
    let x0 = col as usize * w;
    let y0 = row as usize * TILE_PX as usize;
    for (y, dst) in pix.data_mut().chunks_exact_mut(w).enumerate() {
        let at = (y0 + y) * ROW as usize + x0;
        let src = data.get(at..at + w)?;
        for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            let a = u32::from(s.get(3).copied().unwrap_or(0));
            if a == 255 {
                d.copy_from_slice(s);
            } else {
                for (dc, sc) in d.iter_mut().zip(s).take(3) {
                    *dc = ((u32::from(*sc) * a + 127) / 255).min(a) as u8;
                }
                if let Some(da) = d.get_mut(3) {
                    *da = a as u8;
                }
            }
        }
    }
    Some(pix)
}

/// A top-level layer's scene and the page area it paints (`None`: anywhere).
type Part = Result<(Scene, Option<Rect>), Fallback>;

/// What the kept scene was built from.
struct Kept {
    doc: Arc<Document>,
    page: NodeId,
    skip: Vec<NodeId>,
    fonts: u64,
    /// Each top-level layer's scene (or why it can't be drawn), in page order.
    parts: Vec<(NodeId, Part)>,
}

/// How the tiles of the last batch were painted (`ui.inspect`, benchmarks).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Counts {
    pub gpu: usize,
    pub cpu: usize,
    pub empty: usize,
    /// Milliseconds: bringing the page scene up to date, building the blocks' scenes, vello's
    /// encoding and submission, waiting for the GPU, and cutting the pixels into tiles.
    pub scene_ms: f64,
    pub compose_ms: f64,
    pub submit_ms: f64,
    pub wait_ms: f64,
    pub cut_ms: f64,
}

/// Paints canvas tiles on the GPU, and on the CPU where it can't.
pub struct GpuTilePainter {
    gpu: Gpu,
    images: Images,
    kept: Option<Kept>,
    frame: Scene,
    /// Why the last tile painted on the CPU was (`None`: none was).
    pub last_fallback: Option<Fallback>,
    pub counts: Counts,
}

impl GpuTilePainter {
    pub fn new(gpu: Gpu) -> Self {
        Self { gpu, images: Images::default(), kept: None, frame: Scene::new(), last_fallback: None, counts: Counts::default() }
    }

    pub fn adapter(&self) -> &str {
        &self.gpu.adapter
    }

    /// The kept page scene brought up to date with `doc`.
    fn update(&mut self, doc: &Arc<Document>, page: NodeId, skip: &[NodeId]) {
        let fonts = figcraft_text::layout_generation();
        let old = self.kept.take().filter(|k| k.page == page && k.skip == skip && k.fonts == fonts && k.doc.color_profile == doc.color_profile);
        let kept = match old {
            Some(k) if Arc::ptr_eq(&k.doc, doc) => k,
            old => {
                // Top-level layers to rebuild; `None`: all of them.
                let dirty = old.as_ref().and_then(|k| dirty_tops(&k.doc, doc, page));
                let mut reuse: HashMap<NodeId, Part> = match (old, &dirty) {
                    (Some(k), Some(_)) => k.parts.into_iter().collect(),
                    _ => HashMap::new(),
                };
                let mut b = Builder { doc, skip, images: &mut self.images };
                let kids = doc.node(page).map(|p| p.children.as_slice()).unwrap_or_default();
                let parts = kids
                    .iter()
                    .map(|c| {
                        let cached = if dirty.as_ref().is_some_and(|d| !d.contains(c)) { reuse.remove(c) } else { None };
                        let part = cached.unwrap_or_else(|| {
                            let mut sc = Scene::new();
                            b.node(&mut sc, *c, figcraft_geom::Affine::IDENTITY, 0)
                                // A page unit of margin for anti-aliasing.
                                .map(|()| (sc, figcraft_render::paint_bounds(doc, *c).map(|r| r.inflate(1.0, 1.0))))
                        });
                        (*c, part)
                    })
                    .collect();
                Kept { doc: doc.clone(), page, skip: skip.to_vec(), fonts, parts }
            }
        };
        self.kept = Some(kept);
    }

    /// What tile `key` needs: `Ok(false)` nothing is over it (only the background), `Ok(true)`
    /// the GPU can paint it, `Err` a layer over it needs the CPU (layers the GPU can't draw make
    /// only the tiles they paint on fall back).
    fn classify(&self, doc: &Document, key: &TileKey) -> Result<bool, Fallback> {
        let shown = key.rect();
        let mut any = false;
        for (id, part) in self.kept.iter().flat_map(|k| k.parts.iter()) {
            match part {
                Ok((_, at)) => any |= at.is_none_or(|r| overlaps(&r, &shown)),
                Err(why) => {
                    let reach = figcraft_render::paint_bounds(doc, *id).map(|r| r.inflate(2.0 / key.scale(), 2.0 / key.scale()));
                    if reach.is_none_or(|r| overlaps(&r, &shown)) {
                        return Err(why);
                    }
                }
            }
        }
        Ok(any)
    }

    /// The scene of the page area `shown` through `view` in `self.frame` (layers the GPU can
    /// draw that are over it).
    fn compose(&mut self, view: &View) {
        self.frame.reset();
        let xf = view.transform();
        let shown = view.page_rect();
        for (_, part) in self.kept.iter().flat_map(|k| k.parts.iter()) {
            if let Ok((sc, at)) = part
                && at.is_none_or(|r| overlaps(&r, &shown))
            {
                self.frame.append(sc, Some(xf));
            }
        }
    }

    /// Paint the tiles of one block (`level`, block column and row) on the GPU; `None` when the
    /// GPU failed (they go to the CPU).
    fn block(&mut self, level: i32, keys: &[TileKey], base: figcraft_color::Color) -> Option<Vec<(TileKey, Pixmap)>> {
        let x0 = keys.iter().map(|k| k.x).min()?;
        let y0 = keys.iter().map(|k| k.y).min()?;
        let cols = (keys.iter().map(|k| k.x).max()? - x0 + 1).clamp(1, BLOCK) as u32;
        let rows = (keys.iter().map(|k| k.y).max()? - y0 + 1).clamp(1, BLOCK) as u32;
        let first = TileKey { level, x: x0, y: y0 }.view();
        let view = View { width: cols * TILE_PX, height: rows * TILE_PX, ..first };
        let t = std::time::Instant::now();
        self.compose(&view);
        self.counts.compose_ms += ms(t);
        let frame = std::mem::take(&mut self.frame);
        let r = self.gpu.render(&frame, cols, rows, base, &mut self.counts);
        self.frame = frame;
        let pix = match r {
            Ok(p) => p,
            Err(why) => {
                self.last_fallback = Some(why);
                return None;
            }
        };
        let at = |k: &TileKey| ((k.y - y0) as usize) * cols as usize + (k.x - x0) as usize;
        let mut pix: Vec<Option<Pixmap>> = pix.into_iter().map(Some).collect();
        Some(keys.iter().filter_map(|k| Some((*k, pix.get_mut(at(k))?.take()?))).collect())
    }

    fn base(doc: &Document, page: NodeId, opts: &Options) -> figcraft_color::Color {
        opts.background
            .or_else(|| match doc.node(page).map(|p| &p.kind) {
                Some(NodeKind::Page { background, .. }) => Some(*background),
                _ => None,
            })
            .unwrap_or(figcraft_color::Color::TRANSPARENT)
    }
}

fn ms(t: std::time::Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x0 < b.x1 && b.x0 < a.x1 && a.y0 < b.y1 && b.y0 < a.y1
}

impl TilePainter for GpuTilePainter {
    fn paint(
        &mut self,
        doc: &Arc<Document>,
        page: NodeId,
        tiles: &[TileKey],
        opts: &Options,
        stop: &(dyn Fn() -> bool + Sync),
    ) -> Vec<(TileKey, Option<Painted>)> {
        self.counts = Counts::default();
        // Views the GPU doesn't draw: every tile on the CPU.
        let cpu_only = opts.outline_mode || !opts.outlined.is_empty() || opts.solo.is_some();
        if cpu_only {
            self.last_fallback = Some("outline or focus view");
        } else {
            let t = std::time::Instant::now();
            self.update(doc, page, &opts.skip);
            self.counts.scene_ms = ms(t);
        }
        let base = Self::base(doc, page, opts);
        let mut out = Vec::with_capacity(tiles.len());
        let mut cpu = Vec::new();
        // The GPU's tiles by block, in the order they were asked for.
        let mut blocks: Vec<((i32, i64, i64), Vec<TileKey>)> = Vec::new();
        for key in tiles {
            match if cpu_only { Err("outline or focus view") } else { self.classify(doc, key) } {
                Ok(false) => {
                    self.counts.empty += 1;
                    out.push((*key, Some(Painted::Empty)));
                }
                Ok(true) => {
                    let b = (key.level, key.x.div_euclid(BLOCK), key.y.div_euclid(BLOCK));
                    match blocks.iter_mut().find(|(at, _)| *at == b) {
                        Some((_, keys)) => keys.push(*key),
                        None => blocks.push((b, vec![*key])),
                    }
                }
                Err(why) => {
                    self.last_fallback = Some(why);
                    cpu.push(*key);
                }
            }
        }
        for ((level, ..), keys) in blocks {
            if stop() {
                return out;
            }
            match self.block(level, &keys, base) {
                Some(done) => {
                    self.counts.gpu += done.len();
                    let painted: HashSet<TileKey> = done.iter().map(|(k, _)| *k).collect();
                    out.extend(done.into_iter().map(|(k, p)| (k, Some(Painted::Pixels(p)))));
                    cpu.extend(keys.into_iter().filter(|k| !painted.contains(k)));
                }
                None => cpu.extend(keys),
            }
        }
        // The rest on every core, as the CPU painter does.
        let rest = CpuTilePainter.paint(doc, page, &cpu, opts, stop);
        self.counts.cpu += rest.len();
        out.extend(rest);
        out
    }

    fn report(&self) -> String {
        let c = self.counts;
        let why = self.last_fallback.map(|w| format!(", last CPU: {w}")).unwrap_or_default();
        format!("gpu {}: {} on the GPU, {} on the CPU, {} empty{why}", self.gpu.adapter, c.gpu, c.cpu, c.empty)
    }
}

/// The top-level layers of `page` an edit from `old` to `new` touched; `None` when the whole page
/// scene has to be rebuilt (the page itself changed, or too much did).
fn dirty_tops(old: &Document, new: &Document, page: NodeId) -> Option<HashSet<NodeId>> {
    let mut out = HashSet::new();
    for id in new.changed_ids(old, MAX_CHANGED)? {
        if id == page {
            return None;
        }
        for d in [old, new] {
            if d.page_of(id) != Some(page) {
                continue;
            }
            // The ancestor right under the page.
            let mut top = id;
            for a in d.ancestors(id) {
                if a == page {
                    break;
                }
                top = a;
            }
            out.insert(top);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests;

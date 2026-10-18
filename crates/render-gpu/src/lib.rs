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
use std::sync::Arc;

use figcraft_doc::{Document, NodeId, NodeKind};
use figcraft_geom::Rect;
use figcraft_render::tiles::{CpuTilePainter, NativeTile, Painted, TILE_PX, TileKey, TilePainter};
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
                // Every core compiles shaders (on its own thread: see `shared_in_background`).
                num_init_threads: None,
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

impl Gpu {
    /// Render `scene` (in device pixels) over `base` into a `width`×`height` texture of its own that
    /// stays on the GPU. Waits for the GPU to finish it: one canvas image at a time, so a zoom that
    /// asks for a new one every frame never queues up work ahead of the window's own frames.
    fn render_view(
        &mut self,
        scene: &Scene,
        width: u32,
        height: u32,
        base: figcraft_color::Color,
        counts: &mut Counts,
    ) -> Result<wgpu::Texture, Fallback> {
        let started = std::time::Instant::now();
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("figcraft-gpu-canvas"),
            size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let params = RenderParams {
            base_color: vello::peniko::Color::new([base.r, base.g, base.b, base.a]),
            width,
            height,
            antialiasing_method: AaConfig::Area,
        };
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let r = self.renderer.render_to_texture(&self.device, &self.queue, scene, &view, &params);
        counts.submit_ms += ms(started);
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("gpu canvas: {e}");
            return Err("GPU error");
        }
        let waited = std::time::Instant::now();
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
        counts.wait_ms += ms(waited);
        r.map_err(|e| {
            log::warn!("gpu canvas: {e}");
            "GPU render failed"
        })?;
        Ok(texture)
    }

    /// Render `scene` (in device pixels) over `base` as `cols`×`rows` tiles, each copied into a
    /// texture of its own that stays on the GPU (row by row; not waited for: the queue keeps the
    /// order, so whatever samples them later sees them painted).
    fn render_native(
        &mut self,
        scene: &Scene,
        cols: u32,
        rows: u32,
        base: figcraft_color::Color,
        counts: &mut Counts,
    ) -> Result<Vec<wgpu::Texture>, Fallback> {
        let started = std::time::Instant::now();
        let (cols, rows) = (cols.clamp(1, BLOCK as u32), rows.clamp(1, BLOCK as u32));
        let params = RenderParams {
            base_color: vello::peniko::Color::new([base.r, base.g, base.b, base.a]),
            width: cols * TILE_PX,
            height: rows * TILE_PX,
            antialiasing_method: AaConfig::Area,
        };
        self.target();
        let Some(t) = &self.target else { return Err("no render target") };
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let r = self.renderer.render_to_texture(&self.device, &self.queue, scene, &t.view, &params);
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("figcraft-gpu-tiles-split") });
        let mut out = Vec::with_capacity((cols * rows) as usize);
        for row in 0..rows {
            for col in 0..cols {
                let tile = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("figcraft-gpu-tile"),
                    size: wgpu::Extent3d { width: TILE_PX, height: TILE_PX, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    // egui samples Rgba8Unorm textures as gamma-encoded colours, which is what vello writes.
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                });
                enc.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &t.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: col * TILE_PX, y: row * TILE_PX, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo { texture: &tile, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    wgpu::Extent3d { width: TILE_PX, height: TILE_PX, depth_or_array_layers: 1 },
                );
                out.push(tile);
            }
        }
        self.queue.submit([enc.finish()]);
        counts.submit_ms += ms(started);
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("gpu tiles: {e}");
            return Err("GPU error");
        }
        r.map_err(|e| {
            log::warn!("gpu tiles: {e}");
            "GPU render failed"
        })?;
        Ok(out)
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

/// A top-level layer's scene, the page area it paints (`None`: anywhere) and whether it holds
/// pixels made for the scale it was built at ([`Builder::sampled`]).
type Part = Result<(Scene, Option<Rect>, bool), Fallback>;

/// What the kept scene was built from.
struct Kept {
    doc: Arc<Document>,
    page: NodeId,
    skip: Vec<NodeId>,
    fonts: u64,
    /// The scale (a tile level's) the sampled parts were built at.
    scale: f64,
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
    gpu: SharedGpu,
    images: Images,
    kept: Option<Kept>,
    /// The scene of the canvas's whole-view renders ([`TilePainter::paint_view`]), kept apart:
    /// its pixels are made for the view's scale, the tiles' for theirs.
    view_kept: Option<Kept>,
    frame: Scene,
    /// Why the last tile painted on the CPU was (`None`: none was).
    pub last_fallback: Option<Fallback>,
    pub counts: Counts,
    /// Tiles stay on the GPU where they can ([`Painted::Native`]).
    native: bool,
}

/// A GPU every window's painter shares; `None` until its shaders are ready (painters paint on
/// the CPU meanwhile).
pub type SharedGpu = Arc<std::sync::Mutex<Option<Gpu>>>;

/// A [`SharedGpu`] on `device`, made on a thread of its own: vello compiles its shaders then,
/// which takes seconds on some backends (Direct3D 12), and no render waits for that.
pub fn shared_in_background(device: wgpu::Device, queue: wgpu::Queue, adapter: String) -> SharedGpu {
    let slot: SharedGpu = Arc::new(std::sync::Mutex::new(None));
    let fill = slot.clone();
    let spawned = std::thread::Builder::new().name("figcraft-gpu-shaders".into()).spawn(move || {
        let t = std::time::Instant::now();
        match Gpu::shared(device, queue, adapter) {
            Ok(gpu) => {
                log::info!("gpu canvas: {} ready in {:.0} ms", gpu.adapter, ms(t));
                if let Ok(mut s) = fill.lock() {
                    *s = Some(gpu);
                }
            }
            Err(e) => log::warn!("gpu canvas: {e}; painting on the CPU"),
        }
    });
    if let Err(e) = spawned {
        log::warn!("gpu canvas: {e}");
    }
    slot
}

impl GpuTilePainter {
    pub fn new(gpu: Gpu) -> Self {
        Self::with_shared(Arc::new(std::sync::Mutex::new(Some(gpu))))
    }

    pub fn with_shared(gpu: SharedGpu) -> Self {
        Self {
            gpu,
            images: Images::default(),
            kept: None,
            view_kept: None,
            frame: Scene::new(),
            last_fallback: None,
            counts: Counts::default(),
            native: false,
        }
    }

    /// The adapter's name and backend (empty while the shaders aren't ready).
    pub fn adapter(&self) -> String {
        self.gpu.lock().ok().and_then(|g| g.as_ref().map(|g| g.adapter.clone())).unwrap_or_default()
    }

    fn ready(&self) -> bool {
        self.gpu.lock().is_ok_and(|g| g.is_some())
    }

    /// Block until the GPU has done what was submitted (benchmarks).
    pub fn wait(&self) {
        if let Ok(g) = self.gpu.lock()
            && let Some(g) = g.as_ref()
        {
            let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
        }
    }

    /// The kept page scene brought up to date with `doc`, its sampled parts drawn for `scale`.
    fn update(&mut self, doc: &Arc<Document>, page: NodeId, skip: &[NodeId], scale: f64, lod: bool) {
        let fonts = figcraft_text::layout_generation();
        let old = self.kept.take().filter(|k| k.page == page && k.skip == skip && k.fonts == fonts && k.doc.color_profile == doc.color_profile);
        if let Some(k) = old.as_ref()
            && Arc::ptr_eq(&k.doc, doc)
            && k.scale.to_bits() == scale.to_bits()
        {
            self.kept = old;
            return;
        }
        // Top-level layers to rebuild; `None`: all of them.
        let dirty = old.as_ref().and_then(|k| if Arc::ptr_eq(&k.doc, doc) { Some(HashSet::new()) } else { dirty_tops(&k.doc, doc, page) });
        let rescaled = old.as_ref().is_some_and(|k| k.scale.to_bits() != scale.to_bits());
        let mut reuse: HashMap<NodeId, Part> = match (old, &dirty) {
            (Some(k), Some(_)) => k.parts.into_iter().collect(),
            _ => HashMap::new(),
        };
        let images = &mut self.images;
        // What's too small to read is approximated only in zoomed-out views.
        let lod = lod && scale <= figcraft_render::lod::MAX_SCALE;
        let kids = doc.node(page).map(|p| p.children.as_slice()).unwrap_or_default();
        let parts = kids
            .iter()
            .map(|c| {
                let cached = if dirty.as_ref().is_some_and(|d| !d.contains(c)) { reuse.remove(c) } else { None };
                // Pixels made for another scale are made again.
                let cached = cached.filter(|p| !(rescaled && matches!(p, Ok((_, _, true)))));
                let part = cached.unwrap_or_else(|| {
                    let mut b = Builder { doc, skip, images, scale, sampled: false, lod };
                    let mut sc = Scene::new();
                    b.node(&mut sc, *c, figcraft_geom::Affine::IDENTITY, 0)
                        // A page unit of margin for anti-aliasing.
                        .map(|()| (sc, figcraft_render::paint_bounds(doc, *c).map(|r| r.inflate(1.0, 1.0)), b.sampled))
                });
                (*c, part)
            })
            .collect();
        self.kept = Some(Kept { doc: doc.clone(), page, skip: skip.to_vec(), fonts, scale, parts });
    }

    /// What tile `key` needs: `Ok(false)` nothing is over it (only the background), `Ok(true)`
    /// the GPU can paint it, `Err` a layer over it needs the CPU (layers the GPU can't draw make
    /// only the tiles they paint on fall back).
    fn classify(&self, doc: &Document, key: &TileKey) -> Result<bool, Fallback> {
        let shown = key.rect();
        let mut any = false;
        for (id, part) in self.kept.iter().flat_map(|k| k.parts.iter()) {
            match part {
                Ok((_, at, _)) => any |= at.is_none_or(|r| overlaps(&r, &shown)),
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
            if let Ok((sc, at, _)) = part
                && at.is_none_or(|r| overlaps(&r, &shown))
            {
                self.frame.append(sc, Some(xf));
            }
        }
    }

    /// Paint the tiles of one block (`level`, block column and row) on the GPU; `None` when the
    /// GPU failed (they go to the CPU).
    fn block(&mut self, level: i32, keys: &[TileKey], base: figcraft_color::Color, native: bool) -> Option<Vec<(TileKey, Painted)>> {
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
        let r: Result<Vec<Painted>, Fallback> = match self.gpu.lock().as_deref_mut() {
            Ok(Some(gpu)) if native => gpu
                .render_native(&frame, cols, rows, base, &mut self.counts)
                .map(|t| t.into_iter().map(|t| Painted::Native(NativeTile(Arc::new(t)))).collect()),
            Ok(Some(gpu)) => gpu.render(&frame, cols, rows, base, &mut self.counts).map(|p| p.into_iter().map(Painted::Pixels).collect()),
            _ => Err("GPU not ready"),
        };
        self.frame = frame;
        let tiles = match r {
            Ok(p) => p,
            Err(why) => {
                self.last_fallback = Some(why);
                return None;
            }
        };
        let at = |k: &TileKey| ((k.y - y0) as usize) * cols as usize + (k.x - x0) as usize;
        let mut tiles: Vec<Option<Painted>> = tiles.into_iter().map(Some).collect();
        Some(keys.iter().filter_map(|k| Some((*k, tiles.get_mut(at(k))?.take()?))).collect())
    }

    /// [`TilePainter::paint_view`] on the scene in `self.kept`.
    fn view(&mut self, doc: &Arc<Document>, page: NodeId, view: &View, opts: &Options) -> Option<Painted> {
        let base = Self::base(doc, page, opts);
        if !self.native || opts.outline_mode || !opts.outlined.is_empty() || opts.solo.is_some() || base.a < 1.0 || !doc.color_profile.is_srgb() {
            return None;
        }
        let max = self.gpu.lock().ok().and_then(|g| g.as_ref().map(|g| g.device.limits().max_texture_dimension_2d))?;
        if view.width == 0 || view.height == 0 || view.width > max || view.height > max {
            return None;
        }
        // Pixels the scene makes (images, greeked text) for the tile level at or above the view's
        // scale: zooming within a level keeps the scene.
        let scale = figcraft_render::tiles::level_at_least(view.scale).map_or(view.scale, figcraft_render::tiles::level_scale);
        let t = std::time::Instant::now();
        self.update(doc, page, &opts.skip, scale, opts.lod);
        self.counts.scene_ms = ms(t);
        let shown = view.page_rect();
        for (id, part) in self.kept.iter().flat_map(|k| k.parts.iter()) {
            if let Err(why) = part {
                let reach = figcraft_render::paint_bounds(doc, *id).map(|r| r.inflate(2.0 / view.scale, 2.0 / view.scale));
                if reach.is_none_or(|r| overlaps(&r, &shown)) {
                    self.last_fallback = Some(why);
                    return None;
                }
            }
        }
        let t = std::time::Instant::now();
        self.compose(view);
        self.counts.compose_ms = ms(t);
        let frame = std::mem::take(&mut self.frame);
        let r = match self.gpu.lock().as_deref_mut() {
            Ok(Some(gpu)) => gpu.render_view(&frame, view.width, view.height, base, &mut self.counts),
            _ => Err("GPU not ready"),
        };
        self.frame = frame;
        match r {
            Ok(tex) => Some(Painted::Native(NativeTile(Arc::new(tex)))),
            Err(why) => {
                self.last_fallback = Some(why);
                None
            }
        }
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
        if !self.ready() {
            self.last_fallback = Some("GPU not ready");
            return CpuTilePainter.paint(doc, page, tiles, opts, stop);
        }
        // Views the GPU doesn't draw: every tile on the CPU.
        let cpu_only = opts.outline_mode || !opts.outlined.is_empty() || opts.solo.is_some();
        if cpu_only {
            self.last_fallback = Some("outline or focus view");
        } else {
            // Bounds don't depend on the scale: keep the one the scene has.
            let scale = self.kept.as_ref().map(|k| k.scale).or(tiles.first().map(TileKey::scale)).unwrap_or(1.0);
            let t = std::time::Instant::now();
            self.update(doc, page, &opts.skip, scale, opts.lod);
            self.counts.scene_ms = ms(t);
        }
        let base = Self::base(doc, page, opts);
        // On the GPU as they are: when the app shows them so, the page is opaque (vello writes
        // straight alpha, the window wants it premultiplied) and in sRGB (the screen's).
        let native = self.native && base.a >= 1.0 && doc.color_profile.is_srgb();
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
        // A level at a time (the scene's pixels are made for one), finest first.
        blocks.sort_by_key(|((level, ..), _)| std::cmp::Reverse(*level));
        for ((level, ..), keys) in blocks {
            if stop() {
                return out;
            }
            let t = std::time::Instant::now();
            self.update(doc, page, &opts.skip, figcraft_render::tiles::level_scale(level), opts.lod);
            self.counts.scene_ms += ms(t);
            match self.block(level, &keys, base, native) {
                Some(done) => {
                    self.counts.gpu += done.len();
                    let painted: HashSet<TileKey> = done.iter().map(|(k, _)| *k).collect();
                    out.extend(done.into_iter().map(|(k, p)| (k, Some(p))));
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

    fn keep_on_gpu(&mut self, on: bool) {
        self.native = on;
    }

    fn paint_view(&mut self, doc: &Arc<Document>, page: NodeId, view: &View, opts: &Options) -> Option<Painted> {
        std::mem::swap(&mut self.kept, &mut self.view_kept);
        let out = self.view(doc, page, view, opts);
        std::mem::swap(&mut self.kept, &mut self.view_kept);
        out
    }

    fn report(&self) -> String {
        let c = self.counts;
        let why = self.last_fallback.map(|w| format!(", last CPU: {w}")).unwrap_or_default();
        format!(
            "gpu {}: {} on the GPU, {} on the CPU, {} empty{why}; scene {:.1} ms, compose {:.1} ms, submit {:.1} ms",
            self.adapter(),
            c.gpu,
            c.cpu,
            c.empty,
            c.scene_ms,
            c.compose_ms,
            c.submit_ms
        )
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

//! GPU canvas tiles: the scene graph as a [vello] scene, rasterised by compute shaders on wgpu,
//! painting the canvas's tiles ([`figcraft_render::tiles`]).
//!
//! The CPU renderer (`figcraft-render`) stays the reference and the fallback: a tile that shows
//! something this doesn't draw yet (masks, pattern fills, the rarer effects: see [`Fallback`]) is
//! painted on the CPU, and so is a whole batch in an outline view. Exports, the CLI and MCP never
//! come here. Drop shadows, layer blurs and background blurs are made on the GPU ([`effects`]).
//!
//! The page scene is kept per top-level layer in page coordinates: a tile is the layers over it
//! through the tile's transform, and an edit rebuilds only the top-level layers it touched.

pub mod effects;
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
/// vello's image atlas: its side at first and at most, and how many renders an image stays after
/// its last one (vello_encoding 0.11's `ImageCache`).
const ATLAS_START: i32 = 1024;
const ATLAS_SIDE: i32 = 8192;
const ATLAS_KEEP: u64 = 2;
/// Why a render whose images don't fit vello's atlas even when it is empty paints on the CPU.
const ATLAS_FULL: Fallback = "too many image pixels for the GPU's atlas";

/// A wgpu device (the window's, or one of its own) with a vello renderer.
pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
    target: Option<Target>,
    /// The effects' compute passes (`None`: they failed to build; layers with effects fall back).
    fx: Option<effects::Pipelines>,
    atlas: Atlas,
    /// The adapter's name and backend (for reports).
    pub adapter: String,
}

/// vello's image atlas as the renders left it, and what makes room in it.
struct Atlas {
    mirror: Mirror,
    /// A 1×1 target for renders without images.
    idle: Option<wgpu::TextureView>,
    /// What an image a render leaves out draws.
    blank: vello::peniko::ImageData,
}

impl Default for Atlas {
    fn default() -> Self {
        Self { mirror: Mirror::new(), idle: None, blank: scene::placeholder(1, 1, true) }
    }
}

impl Atlas {
    /// Make sure every image of `scene` gets a place in vello's atlas (one that doesn't draws as
    /// nothing): when they don't all fit next to those of the last renders, two renders without
    /// images let vello evict those first. Fails when they don't fit even so.
    fn fit(&mut self, renderer: &mut Renderer, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene) -> Result<(), Fallback> {
        let patches = &scene.encoding().resources.patches;
        if patches.is_empty() {
            // vello doesn't touch its atlas then.
            return Ok(());
        }
        let images = scene_images(scene);
        let mut next = self.mirror.clone();
        if next.resolve(&images) {
            self.mirror = next;
            return Ok(());
        }
        // Two renders without images first: vello then evicts those of the renders before.
        let mut next = self.mirror.clone();
        next.resolve(&[]);
        next.resolve(&[]);
        let mut strip = None;
        if !next.resolve_copy(&images) {
            // Evicted only once one doesn't fit, those images left gaps the scene's don't fit
            // between: a third with a strip as wide as the atlas (which doesn't fit then) evicts
            // them all first, and the scene's images go into an atlas holding only the strip.
            for h in [1, 64, 1024] {
                let im = scene::placeholder(ATLAS_SIDE as u32, h, true);
                let mut m = next.clone();
                m.resolve(&[(im.data.id(), ATLAS_SIDE, h as i32)]);
                if m.map.len() == 1 {
                    if m.resolve_copy(&images) {
                        next = m;
                        strip = Some(im);
                    }
                    break;
                }
            }
            if strip.is_none() {
                return Err(ATLAS_FULL);
            }
        }
        next.resolve(&images);
        // vello resolves a scene's images before anything can fail: the atlas moves on either way.
        self.mirror = next;
        self.idle(renderer, device, queue, None)?;
        self.idle(renderer, device, queue, None)?;
        if let Some(im) = strip {
            self.idle(renderer, device, queue, Some(im))?;
        }
        Ok(())
    }

    /// A render of nothing (or of `image`, drawn nowhere) into a pixel of its own.
    fn idle(
        &mut self,
        renderer: &mut Renderer,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: Option<vello::peniko::ImageData>,
    ) -> Result<(), Fallback> {
        let view = self.idle.get_or_insert_with(|| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("figcraft-gpu-idle"),
                    size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::STORAGE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        });
        let mut sc = Scene::new();
        keep_atlas(&mut sc);
        if let Some(image) = image {
            let brush = vello::peniko::ImageBrush { image, sampler: vello::peniko::ImageSampler::default() };
            sc.fill(vello::peniko::Fill::NonZero, vello::kurbo::Affine::IDENTITY, &brush, None, &vello::kurbo::Rect::new(0.0, 0.0, 1.0, 1.0));
        }
        let params = RenderParams { base_color: vello::peniko::Color::TRANSPARENT, width: 1, height: 1, antialiasing_method: AaConfig::Area };
        renderer.render_to_texture(device, queue, &sc, view, &params).map_err(|e| {
            log::warn!("gpu canvas: {e}");
            "GPU render failed"
        })
    }
}

/// `scene`'s images in the order vello places them: blob, width, height.
fn scene_images(scene: &Scene) -> Vec<(u64, i32, i32)> {
    let side = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    scene
        .encoding()
        .resources
        .patches
        .iter()
        .filter_map(|p| match p {
            vello::low_level::vello_encoding::Patch::Image { image, .. } => Some((image.data.id(), side(image.width), side(image.height))),
            _ => None,
        })
        .collect()
}

/// What vello's atlas holds, kept by the same rules on the same allocator (vello_encoding 0.11's
/// `ImageCache` and its `Resolver::resolve_pending_images`), to know before a render whether it
/// leaves an image out.
#[derive(Clone)]
struct Mirror {
    atlas: guillotiere::AtlasAllocator,
    /// The renders so far.
    generation: u64,
    /// Whether this render evicted images already (once per render).
    evicted: bool,
    /// Image blob → its place, size and the render that last used it.
    map: HashMap<u64, (guillotiere::AllocId, i32, i32, u64)>,
}

impl Mirror {
    fn new() -> Self {
        Self {
            atlas: guillotiere::AtlasAllocator::new(guillotiere::size2(ATLAS_START, ATLAS_START)),
            generation: 0,
            evicted: false,
            map: HashMap::new(),
        }
    }

    /// Whether a render of `images` would give each a place (this atlas left as it is).
    fn resolve_copy(&self, images: &[(u64, i32, i32)]) -> bool {
        self.clone().resolve(images)
    }

    /// A render of `images` (blob, width, height, in the scene's order): whether each got a place.
    fn resolve(&mut self, images: &[(u64, i32, i32)]) -> bool {
        self.generation = self.generation.wrapping_add(1);
        self.evicted = false;
        'pass: loop {
            // A pass after the atlas grew starts over.
            let previous = self.generation.wrapping_sub(1);
            for e in self.map.values_mut() {
                if e.3 == self.generation {
                    e.3 = previous;
                }
            }
            let mut all = true;
            for &(id, w, h) in images {
                loop {
                    if self.place(id, w, h) {
                        break;
                    }
                    let size = self.atlas.size();
                    if w <= size.width && h <= size.height && self.evict() {
                        continue;
                    }
                    if self.grow() {
                        continue 'pass;
                    }
                    all = false;
                    break;
                }
            }
            return all;
        }
    }

    fn place(&mut self, id: u64, w: i32, h: i32) -> bool {
        if let Some(e) = self.map.get_mut(&id) {
            e.3 = self.generation;
            return true;
        }
        match self.atlas.allocate(guillotiere::size2(w, h)) {
            Some(a) => {
                self.map.insert(id, (a.id, w, h, self.generation));
                true
            }
            None => false,
        }
    }

    /// Free the images the last renders didn't use.
    fn evict(&mut self) -> bool {
        if self.evicted {
            return false;
        }
        let Some(before) = self.generation.checked_sub(ATLAS_KEEP) else { return false };
        let stale: Vec<u64> = self.map.iter().filter(|(_, e)| e.3 < before).map(|(id, _)| *id).collect();
        for id in &stale {
            if let Some(e) = self.map.remove(id) {
                self.atlas.deallocate(e.0);
            }
        }
        self.evicted = !stale.is_empty();
        self.evicted
    }

    /// A larger atlas with every image packed again in blob order.
    fn grow(&mut self) -> bool {
        let mut side = self.atlas.size().width.saturating_mul(2);
        while side <= ATLAS_SIDE {
            let mut atlas = guillotiere::AtlasAllocator::new(guillotiere::size2(side, side));
            let mut ids: Vec<u64> = self.map.keys().copied().collect();
            ids.sort_unstable();
            let mut map = HashMap::with_capacity(ids.len());
            let fits = ids.iter().all(|id| {
                let Some(&(_, w, h, last)) = self.map.get(id) else { return true };
                atlas.allocate(guillotiere::size2(w, h)).map(|a| map.insert(*id, (a.id, w, h, last))).is_some()
            });
            if fits {
                self.atlas = atlas;
                self.map = map;
                return true;
            }
            side = side.saturating_mul(2);
        }
        false
    }
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
        log_device_errors(&device);
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
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let fx = effects::Pipelines::new(&device);
        let fx = match pollster::block_on(scope.pop()) {
            Some(e) => {
                log::warn!("gpu canvas: effects: {e}; layers with effects paint on the CPU");
                None
            }
            None => Some(fx),
        };
        Ok(Self { device, queue, renderer, target: None, fx, atlas: Atlas::default(), adapter })
    }

    /// Make the textures of `fx`'s passes (`backdrop`: what lies under it, in page coordinates, over
    /// `base`) and have the scene's images draw them.
    /// `drawn`: where the images are drawn ([`Builder::drawn`]).
    fn make(
        &mut self,
        fx: &effects::FxNode,
        backdrop: Option<&Scene>,
        base: figcraft_color::Color,
        drawn: &HashMap<u64, Rect>,
    ) -> Result<(), Fallback> {
        let Some(pipes) = &self.fx else { return Err("GPU effects unavailable") };
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        // Each source drawn once per factor.
        let mut sources: Vec<((effects::Source, u32), wgpu::Texture)> = Vec::new();
        let mut made = Vec::with_capacity(fx.passes.len());
        for p in &fx.passes {
            let key = (p.source, p.k);
            let shared = sources.iter().find(|(k, _)| *k == key).filter(|_| p.shape.is_none());
            let src = match shared {
                _ if matches!(p.kind, effects::Kind::Noise(_)) => None,
                Some((_, t)) => Some(t.clone()),
                None => {
                    let (scene, under) = match (&p.shape, p.source) {
                        (Some(shape), _) => (shape, figcraft_color::Color::TRANSPARENT),
                        (None, effects::Source::Content) => (&fx.content, figcraft_color::Color::TRANSPARENT),
                        (None, effects::Source::Backdrop) => (backdrop.ok_or("no backdrop")?, base),
                    };
                    let (width, height) = (p.image.width, p.image.height);
                    let t = self.device.create_texture(&wgpu::TextureDescriptor {
                        label: Some("figcraft-fx-source"),
                        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                        view_formats: &[],
                    });
                    let under = vello::peniko::Color::new([under.r, under.g, under.b, under.a]);
                    let to = Dest { texture: &t, width, height, base: under };
                    pieces(&mut self.renderer, &self.device, &self.queue, &mut self.atlas, scene, fx.to_texture(p.k), drawn, &to)?;
                    if p.shape.is_none() {
                        sources.push((key, t.clone()));
                    }
                    Some(t)
                }
            };
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("figcraft-fx") });
            let t = pipes.run(&self.device, &self.queue, &mut enc, &p.kind, src.as_ref(), (p.image.width, p.image.height)).ok_or("no source")?;
            // Submitted before the next source is drawn: the content shows the inner shadows made
            // first (their images are in it).
            self.queue.submit([enc.finish()]);
            let at =
                wgpu::TexelCopyTextureInfoBase { texture: t.clone(), mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All };
            self.renderer.override_image(&p.image, Some(at));
            made.push(t);
        }
        if let Some(e) = pollster::block_on(scope.pop()) {
            log::warn!("gpu canvas: effects: {e}");
            return Err("GPU error");
        }
        if let Ok(mut m) = fx.made.lock() {
            *m = Some(made);
        }
        Ok(())
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
        self.atlas.fit(&mut self.renderer, &self.device, &self.queue, scene)?;
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
        self.atlas.fit(&mut self.renderer, &self.device, &self.queue, scene)?;
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
        self.atlas.fit(&mut self.renderer, &self.device, &self.queue, scene)?;
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

/// A top-level layer's scene, or why the GPU can't draw it.
type Part = Result<Built, Fallback>;

/// A top-level layer of the kept scene: built only once something shows it (building one turns
/// its images into pixels, and a page can hold hundreds that don't fit in memory together).
enum Slot {
    Built(Box<Part>),
    /// Not built yet; the page area it paints (`None`: anywhere).
    Pending(Option<Rect>),
}

impl Slot {
    fn built(&self) -> Option<&Part> {
        match self {
            Slot::Built(p) => Some(p.as_ref()),
            Slot::Pending(_) => None,
        }
    }
}

struct Built {
    scene: Scene,
    /// The page area it paints (`None`: anywhere).
    at: Option<Rect>,
    /// Whether it holds pixels made for the scale it was built at ([`Builder::sampled`]).
    sampled: bool,
    /// The page area it shows as it is (`None`: all of it); images elsewhere are left out.
    covers: Option<Rect>,
    /// Its layers with effects, each after those inside it.
    fx: Vec<Arc<effects::FxNode>>,
    /// Where each of its images is drawn ([`Builder::drawn`]).
    drawn: HashMap<u64, Rect>,
}

/// What the kept scene was built from.
struct Kept {
    doc: Arc<Document>,
    page: NodeId,
    skip: Vec<NodeId>,
    fonts: u64,
    /// The scale (a tile level's) the sampled parts were built at.
    scale: f64,
    /// Whether too small to read is approximated.
    lod: bool,
    /// Each top-level layer's scene (or why it can't be drawn), in page order.
    parts: Vec<(NodeId, Slot)>,
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
    /// Making the effects' textures.
    pub fx_ms: f64,
    pub compose_ms: f64,
    pub submit_ms: f64,
    pub wait_ms: f64,
    pub cut_ms: f64,
}

/// Every scene rendered draws an invisible gradient. vello 0.11 shrinks its image atlas to 1×1
/// for a scene without images, gradients or glyphs, and the next scene's images that it still
/// takes for uploaded then draw as nothing (the atlas texture was made again, empty).
fn keep_atlas(sc: &mut Scene) {
    use vello::{kurbo, peniko};
    let clear = peniko::Color::new([0.0, 0.0, 0.0, 0.0]);
    let stops = [peniko::ColorStop::from((0.0, clear)), peniko::ColorStop::from((1.0, clear))];
    let grad = peniko::Gradient::new_linear((0.0, 0.0), (1.0, 0.0)).with_stops(stops.as_slice());
    sc.fill(peniko::Fill::NonZero, kurbo::Affine::IDENTITY, &grad, None, &kurbo::Rect::new(0.0, 0.0, 1.0, 1.0));
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
    /// What an image off the view draws ([`Self::compose`]).
    blank: vello::peniko::ImageData,
    /// Why the last tile painted on the CPU was (`None`: none was).
    pub last_fallback: Option<Fallback>,
    pub counts: Counts,
    /// Tiles stay on the GPU where they can ([`Painted::Native`]).
    native: bool,
    /// The effect images whose textures the renderer draws, until their layer goes.
    overrides: Vec<(std::sync::Weak<effects::FxNode>, Vec<vello::peniko::ImageData>)>,
}

/// A GPU every window's painter shares; `None` until its shaders are ready (painters paint on
/// the CPU meanwhile).
pub type SharedGpu = Arc<std::sync::Mutex<Option<Gpu>>>;

/// Log `device`'s errors instead of panicking (wgpu's default). Call it on the window's device
/// when the painter shares it: the painter submits from its own thread, and a window resize that
/// reconfigures the surface while it does fails ("Failed to wait for GPU to come idle"), which
/// took the app down; the surface is configured again on the next frame.
pub fn log_device_errors(device: &wgpu::Device) {
    device.on_uncaptured_error(Arc::new(|e| log::error!("gpu: {e}")));
}

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
            blank: scene::placeholder(1, 1, true),
            last_fallback: None,
            counts: Counts::default(),
            native: false,
            overrides: Vec::new(),
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
            (Some(k), Some(_)) => k.parts.into_iter().filter_map(|(id, s)| if let Slot::Built(p) = s { Some((id, *p)) } else { None }).collect(),
            _ => HashMap::new(),
        };
        // What's too small to read is approximated only in zoomed-out views.
        let lod = lod && scale <= figcraft_render::lod::MAX_SCALE;
        let kids = doc.node(page).map(|p| p.children.as_slice()).unwrap_or_default();
        // A mask among the page's own layers clips the top-level layers over it, which are built
        // apart: those views go to the CPU.
        let looked: Vec<Option<&figcraft_doc::Node>> = kids.iter().map(|c| doc.node(*c)).collect();
        let masked: HashSet<NodeId> = Document::paint_runs_of(&looked, false)
            .into_iter()
            .filter(|r| looked.get(r.start).copied().flatten().is_some_and(|m| m.is_mask && m.visible))
            .flat_map(|r| kids.get(r).unwrap_or_default().iter().copied())
            .collect();
        let mut rebuilt = false;
        let parts = kids
            .iter()
            .map(|c| {
                let cached = if dirty.as_ref().is_some_and(|d| !d.contains(c)) { reuse.remove(c) } else { None };
                // Pixels made for another scale are made again.
                let cached = cached.filter(|p| !(rescaled && matches!(p, Ok(Built { sampled: true, .. }))));
                // A background blur sees the top-level layers before its own: once one of them is
                // built again, it is made again.
                if masked.contains(c) {
                    return (*c, Slot::Built(Box::new(Err("masks on the page"))));
                }
                match cached {
                    Some(part) => {
                        if rebuilt && let Ok(b) = &part {
                            for fx in b.fx.iter().filter(|f| f.prefix.is_some()) {
                                if let Ok(mut m) = fx.made.lock() {
                                    *m = None;
                                }
                            }
                        }
                        (*c, Slot::Built(Box::new(part)))
                    }
                    None => {
                        rebuilt = true;
                        // A page unit of margin for anti-aliasing.
                        (*c, Slot::Pending(figcraft_render::paint_bounds(doc, *c).map(|r| r.inflate(1.0, 1.0))))
                    }
                }
            })
            .collect();
        self.kept = Some(Kept { doc: doc.clone(), page, skip: skip.to_vec(), fonts, scale, lod, parts });
    }

    /// Build the kept scene's top-level layers over `shown` (or build them again where they left
    /// out images there), and those under the background blurs there (what they blur). `whole`:
    /// for one image of all of `shown`, which a layer the GPU can't draw sends to the CPU anyway,
    /// so none is built then; why is returned.
    fn build_over(&mut self, shown: &[Rect], whole: bool) -> Option<Fallback> {
        let first = shown.first()?;
        let mut rects = shown.to_vec();
        let all = shown.iter().fold(*first, |a, r| a.union(*r));
        // Images within half a view of what is shown are drawn too, so a pan doesn't build again.
        let area = all.inflate(all.width() / 2.0, all.height() / 2.0);
        let inside = |c: &Rect, r: &Rect| c.x0 <= r.x0 && c.y0 <= r.y0 && c.x1 >= r.x1 && c.y1 >= r.y1;
        // A layer built for a backdrop can hold another background blur (a few rounds at most).
        for _ in 0..4 {
            use rayon::prelude::*;
            let k = self.kept.as_mut()?;
            let (doc, scale, lod) = (k.doc.clone(), k.scale, k.lod);
            let todo: Vec<(usize, NodeId, Option<Rect>)> = k
                .parts
                .iter()
                .enumerate()
                .filter_map(|(i, (c, slot))| {
                    let (at, covers) = match slot {
                        Slot::Pending(at) => (*at, None),
                        Slot::Built(p) => match p.as_ref() {
                            Ok(b) if b.covers.is_some() => (b.at, b.covers),
                            _ => return None,
                        },
                    };
                    let wanted: Vec<&Rect> = rects.iter().filter(|x| at.is_none_or(|r| overlaps(&r, x))).collect();
                    let missing = match covers {
                        None => matches!(slot, Slot::Pending(_)) && !wanted.is_empty(),
                        Some(c) => wanted.iter().any(|x| !inside(&c, &x.intersect(at.unwrap_or(**x)))),
                    };
                    missing.then_some((i, *c, at))
                })
                .collect();
            if todo.is_empty() {
                return None;
            }
            // Layers the GPU can't draw are found without building them.
            let mut cpu = None;
            let todo: Vec<(usize, NodeId, Option<Rect>)> = todo
                .into_iter()
                .filter(|(i, c, at)| match scene::cpu_only(&doc, *c) {
                    Some(why) => {
                        if let Some((_, slot)) = k.parts.get_mut(*i) {
                            *slot = Slot::Built(Box::new(Err(why)));
                        }
                        if at.is_none_or(|r| shown.iter().any(|x| overlaps(&r, x))) {
                            cpu = Some(why);
                        }
                        false
                    }
                    None => true,
                })
                .collect();
            if whole && cpu.is_some() {
                return cpu;
            }
            // On every core: their images turn into pixels first (a layer is built on one thread,
            // and one can hold dozens of large images), then the layers are built.
            let near: Vec<(&figcraft_color::Paint, f64, f64, f64)> = todo
                .iter()
                .flat_map(|(_, c, _)| doc.subtree(*c))
                .filter_map(|id| doc.node(id))
                .filter(|n| n.visible && doc.world_bounds(n.id).is_some_and(|b| overlaps(&b, &area)))
                .flat_map(|n| {
                    let s = doc.world_transform(n.id).determinant().abs().sqrt() * scale;
                    n.fills
                        .iter()
                        .chain(&n.strokes)
                        .filter(|p| p.visible && matches!(p.kind, figcraft_color::PaintKind::Image(_)))
                        .map(move |p| (p, n.width, n.height, s))
                })
                .collect();
            near.par_iter().for_each(|(p, w, h, s)| {
                let _ = figcraft_render::paint::prepare_at(p, *w, *h, *s, &doc, 0.0);
            });
            let skip = &k.skip;
            let built: Vec<(usize, Part, Images)> = todo
                .into_par_iter()
                .map(|(i, c, at)| {
                    let mut images = Images::default();
                    let mut b = Builder {
                        doc: &doc,
                        skip,
                        images: &mut images,
                        scale,
                        sampled: false,
                        lod,
                        top: c,
                        fx: Vec::new(),
                        open: 0,
                        in_fx: 0,
                        under: None,
                        area: Some(area),
                        partial: false,
                        drawn: HashMap::new(),
                    };
                    let mut sc = Scene::new();
                    let part = b.node(&mut sc, c, figcraft_geom::Affine::IDENTITY, 0).map(|()| Built {
                        scene: sc,
                        at,
                        sampled: b.sampled,
                        covers: b.partial.then_some(area),
                        fx: b.fx,
                        drawn: b.drawn,
                    });
                    (i, part, images)
                })
                .collect();
            // A layer built again (with more of its images) changes what the background blurs of
            // the layers after it see.
            let mut again = usize::MAX;
            for (i, part, images) in built {
                if let Some((_, slot)) = k.parts.get_mut(i) {
                    if matches!(slot, Slot::Built(_)) {
                        again = again.min(i);
                    }
                    *slot = Slot::Built(Box::new(part));
                }
                self.images.absorb(images);
            }
            for (_, slot) in k.parts.iter().skip(again.saturating_add(1)) {
                for fx in slot.built().and_then(|p| p.as_ref().ok()).map(|b| b.fx.as_slice()).unwrap_or_default() {
                    if fx.prefix.is_some()
                        && let Ok(mut m) = fx.made.lock()
                    {
                        *m = None;
                    }
                }
            }
            let more: Vec<Rect> = k
                .parts
                .iter()
                .filter_map(|(_, s)| s.built()?.as_ref().ok())
                .flat_map(|b| b.fx.iter())
                .filter(|f| f.prefix.is_some() && shown.iter().any(|x| overlaps(&f.region, x)) && !rects.contains(&f.region))
                .map(|f| f.region)
                .collect();
            if more.is_empty() {
                return None;
            }
            rects.extend(more);
        }
        None
    }

    /// What tile `key` needs: `Ok(false)` nothing is over it (only the background), `Ok(true)`
    /// the GPU can paint it, `Err` a layer over it needs the CPU (layers the GPU can't draw make
    /// only the tiles they paint on fall back).
    fn classify(&self, doc: &Document, key: &TileKey) -> Result<bool, Fallback> {
        let shown = key.rect();
        let mut any = false;
        for (id, slot) in self.kept.iter().flat_map(|k| k.parts.iter()) {
            let Slot::Built(part) = slot else {
                // Built before its tiles are classified ([`Self::build_over`]): elsewhere.
                continue;
            };
            match &**part {
                Ok(b) => any |= b.at.is_none_or(|r| overlaps(&r, &shown)),
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
        keep_atlas(&mut self.frame);
        let xf = view.transform();
        let shown = view.page_rect();
        let mut drawn: HashMap<u64, Rect> = HashMap::new();
        for (_, slot) in self.kept.iter().flat_map(|k| k.parts.iter()) {
            if let Some(Ok(b)) = slot.built()
                && b.at.is_none_or(|r| overlaps(&r, &shown))
            {
                self.frame.append(&b.scene, Some(xf));
                merge(&mut drawn, &b.drawn);
            }
        }
        // Images drawn only off the view (a layer's scene holds those near it too, and vello's
        // atlas takes every image of a scene) draw a blank pixel there instead.
        let near = shown.inflate(2.0 / view.scale, 2.0 / view.scale);
        blank_out(&mut self.frame, &drawn, &near, &self.blank);
    }

    /// Make the effect textures the layers over `shown` draw, and those of the layers under the
    /// background blurs there (what they blur), in page order; forget those of layers gone.
    fn prepare(&mut self, shown: &Rect, base: figcraft_color::Color) -> Result<(), Fallback> {
        let Some(kept) = &self.kept else { return Ok(()) };
        let Ok(mut gpu) = self.gpu.lock() else { return Err("GPU not ready") };
        let Some(gpu) = gpu.as_mut() else { return Err("GPU not ready") };
        self.overrides.retain(|(fx, images)| {
            let alive = fx.strong_count() > 0;
            if !alive {
                for im in images {
                    gpu.renderer.override_image(im, None);
                }
            }
            alive
        });
        // Every layer with effects in page order (each after those inside it), with its top-level
        // layer's place.
        let all: Vec<(usize, &Arc<effects::FxNode>)> = kept
            .parts
            .iter()
            .enumerate()
            .filter_map(|(i, (_, slot))| Some((i, slot.built()?.as_ref().ok()?)))
            .flat_map(|(i, b)| b.fx.iter().map(move |f| (i, f)))
            .collect();
        // Those over `shown`, and what they are made from first: the layers with effects inside
        // them (drawn in their content) and, for a background blur, those under it.
        let mut wanted: Vec<bool> = all.iter().map(|(_, f)| overlaps(&f.region, shown)).collect();
        for i in (0..all.len()).rev() {
            if !wanted.get(i).copied().unwrap_or(false) {
                continue;
            }
            let Some(&(part, f)) = all.get(i) else { continue };
            let inside: HashSet<NodeId> = kept.doc.subtree(f.id).into_iter().collect();
            for (j, (p, g)) in all.iter().enumerate().take(i) {
                if ((*p == part && inside.contains(&g.id)) || (f.prefix.is_some() && overlaps(&g.region, &f.region)))
                    && let Some(w) = wanted.get_mut(j)
                {
                    *w = true;
                }
            }
        }
        let mut drawn = HashMap::new();
        for b in kept.parts.iter().filter_map(|(_, s)| s.built()?.as_ref().ok()) {
            merge(&mut drawn, &b.drawn);
        }
        for ((i, fx), want) in all.iter().zip(&wanted) {
            if !want {
                continue;
            }
            let (i, fx) = (*i, *fx);
            if let Ok(m) = fx.made.lock()
                && let Some(textures) = m.as_ref()
            {
                // Made: copied into the renderer's image atlas again (an entry there can be
                // stale: written from another render's textures before these were made).
                for (p, t) in fx.passes.iter().zip(textures) {
                    let at = wgpu::TexelCopyTextureInfoBase {
                        texture: t.clone(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    };
                    gpu.renderer.override_image(&p.image, Some(at));
                }
                continue;
            }
            if let Some(why) = fx.failed.get() {
                return Err(why);
            }
            let backdrop = match &fx.prefix {
                Some(prefix) => {
                    let mut sc = Scene::new();
                    for (id, earlier) in kept.parts.iter().take(i) {
                        match earlier.built() {
                            Some(Ok(e)) if e.at.is_none_or(|r| overlaps(&r, &fx.region)) => sc.append(&e.scene, None),
                            // What it blurs the GPU can't draw: the view goes to the CPU.
                            Some(Err(why)) if figcraft_render::paint_bounds(&kept.doc, *id).is_none_or(|r| overlaps(&r, &fx.region)) => {
                                return Err(why);
                            }
                            _ => {}
                        }
                    }
                    sc.append(prefix, None);
                    Some(sc)
                }
                None => None,
            };
            if let Err(why) = gpu.make(fx, backdrop.as_ref(), base, &drawn) {
                // It won't fit later either: not tried again for every tile.
                if why == ATLAS_FULL {
                    let _ = fx.failed.set(why);
                }
                return Err(why);
            }
            if !self.overrides.iter().any(|(f, _)| std::ptr::eq(f.as_ptr(), Arc::as_ptr(fx))) {
                self.overrides.push((Arc::downgrade(fx), fx.passes.iter().map(|p| p.image.clone()).collect()));
            }
        }
        Ok(())
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
        self.build_over(&[view.page_rect()], false);
        if let Err(why) = self.prepare(&view.page_rect(), base) {
            self.last_fallback = Some(why);
            return None;
        }
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
        let shown = view.page_rect();
        if let Some(why) = self.build_over(&[shown], true) {
            self.counts.scene_ms = ms(t);
            self.last_fallback = Some(why);
            return None;
        }
        self.counts.scene_ms = ms(t);
        for (id, slot) in self.kept.iter().flat_map(|k| k.parts.iter()) {
            if let Some(Err(why)) = slot.built() {
                let reach = figcraft_render::paint_bounds(doc, *id).map(|r| r.inflate(2.0 / view.scale, 2.0 / view.scale));
                if reach.is_none_or(|r| overlaps(&r, &shown)) {
                    self.last_fallback = Some(why);
                    return None;
                }
            }
        }
        let t = std::time::Instant::now();
        if let Err(why) = self.prepare(&shown, base) {
            self.last_fallback = Some(why);
            return None;
        }
        self.counts.fx_ms = ms(t);
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

/// Add `from`'s places of images to `into`'s.
fn merge(into: &mut HashMap<u64, Rect>, from: &HashMap<u64, Rect>) {
    for (id, r) in from {
        into.entry(*id).and_modify(|d| *d = d.union(*r)).or_insert(*r);
    }
}

/// Have `sc`'s images that `drawn` places off `near` (in `sc`'s coordinates) draw `blank`.
fn blank_out(sc: &mut Scene, drawn: &HashMap<u64, Rect>, near: &Rect, blank: &vello::peniko::ImageData) {
    for p in &mut sc.encoding_mut().resources.patches {
        if let vello::low_level::vello_encoding::Patch::Image { image, .. } = p
            && drawn.get(&image.data.id()).is_some_and(|r| !overlaps(r, near))
        {
            *image = blank.clone();
        }
    }
}

/// A texture to render into, and what it is cleared to.
struct Dest<'a> {
    texture: &'a wgpu::Texture,
    width: u32,
    height: u32,
    base: vello::peniko::Color,
}

/// Render `scene` (page coordinates, its images placed by `drawn`) through `to_px` into `to`; in
/// pieces when its images don't fit vello's atlas together, each without the images it doesn't
/// show (down to 256 pixels a side).
#[allow(clippy::too_many_arguments)]
fn pieces(
    renderer: &mut Renderer,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &mut Atlas,
    scene: &Scene,
    to_px: figcraft_geom::Affine,
    drawn: &HashMap<u64, Rect>,
    to: &Dest,
) -> Result<(), Fallback> {
    // Where its images are drawn, in pixels.
    let places: HashMap<u64, Rect> = drawn.iter().map(|(id, r)| (*id, figcraft_geom::transform_rect_bbox(to_px, *r))).collect();
    let whole = Rect::new(0.0, 0.0, f64::from(to.width), f64::from(to.height));
    let mut todo = vec![whole];
    while let Some(piece) = todo.pop() {
        let (w, h) = (piece.width(), piece.height());
        let mut sc = Scene::new();
        keep_atlas(&mut sc);
        sc.append(scene, Some(figcraft_geom::Affine::translate((-piece.x0, -piece.y0)) * to_px));
        let (pw, ph) = (w as u32, h as u32);
        let one = piece == whole;
        if !one {
            let near = Rect::new(-2.0, -2.0, w + 2.0, h + 2.0);
            let moved: HashMap<u64, Rect> = places.iter().map(|(id, at)| (*id, *at - figcraft_geom::Vec2::new(piece.x0, piece.y0))).collect();
            blank_out(&mut sc, &moved, &near, &atlas.blank);
        }
        if let Err(why) = atlas.fit(renderer, device, queue, &sc) {
            if w.max(h) <= 256.0 {
                return Err(why);
            }
            // In halves across its longer side, on whole pixels.
            let (a, b) = if w >= h {
                let x = (piece.x0 + w / 2.0).floor();
                (Rect::new(piece.x0, piece.y0, x, piece.y1), Rect::new(x, piece.y0, piece.x1, piece.y1))
            } else {
                let y = (piece.y0 + h / 2.0).floor();
                (Rect::new(piece.x0, piece.y0, piece.x1, y), Rect::new(piece.x0, y, piece.x1, piece.y1))
            };
            todo.extend([a, b]);
            continue;
        }
        let params = RenderParams { base_color: to.base, width: pw, height: ph, antialiasing_method: AaConfig::Area };
        let failed = |e: vello::Error| {
            log::warn!("gpu canvas: {e}");
            "GPU render failed"
        };
        if one {
            let view = to.texture.create_view(&wgpu::TextureViewDescriptor::default());
            renderer.render_to_texture(device, queue, &sc, &view, &params).map_err(failed)?;
            continue;
        }
        let t = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("figcraft-gpu-piece"),
            size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        renderer.render_to_texture(device, queue, &sc, &t.create_view(&wgpu::TextureViewDescriptor::default()), &params).map_err(failed)?;
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("figcraft-gpu-piece") });
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyTextureInfo {
                texture: to.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: piece.x0 as u32, y: piece.y0 as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
    }
    Ok(())
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
            let rects: Vec<Rect> = tiles.iter().map(TileKey::rect).collect();
            self.build_over(&rects, false);
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

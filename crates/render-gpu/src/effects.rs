//! Drop shadows, inner shadows, layer blurs, background blurs and noise on the GPU (the CPU's
//! `figcraft_render::draw` is the reference). A layer with effects is drawn on its own into a
//! texture, smaller by the factor its blur allows (as the CPU's soft shadows: a blur of 16+ pixels
//! hides the detail an eighth of it would show), and compute passes ([`fx.wgsl`](fx.wgsl)) blur
//! that; noise is made by its own pass ([`grain.wgsl`](grain.wgsl)) from the CPU's hash. The page
//! scene then draws the results as images, scaled back up, where the CPU would composite them:
//! drop shadows under the layer, the blurred backdrop inside its shape, inner shadows over its
//! fills, a blurred layer in its place, grains over it.
//!
//! The results belong to the scene's scale (a tile level's), not to a view: zooming within a level
//! and panning reuse them, and an edit makes them again with the layer's part of the scene.

use std::sync::Mutex;

use figcraft_color::Color;
use figcraft_doc::NodeId;
use figcraft_geom::{Affine, Rect};
use vello::Scene;
use vello::peniko::ImageData;
use vello::wgpu;

/// The longest side of an effect's texture; a layer that needs more falls back to the CPU.
pub const MAX_SIDE: u32 = 4096;
/// Most taps each side of a blur pass, and the widest spread, in the texture's pixels.
const MAX_REACH: i32 = 512;

/// What one effect draws from (`Content`: the layer alone, `Backdrop`: everything under it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Content,
    Backdrop,
}

/// What a pass makes of its source.
#[derive(Clone, Debug)]
pub enum Kind {
    /// Coverage spread by `spread` (texture pixels, out when positive), blurred by `sigma`,
    /// tinted with `color` (straight alpha out). `inside`: an inner shadow, cast by what lies
    /// outside the coverage.
    Shadow { sigma: f64, spread: f64, color: Color, inside: bool },
    /// Colours blurred by `sigma` (premultiplied out).
    Blur { sigma: f64 },
    /// Grains (premultiplied out), from nothing.
    Noise(Grains),
}

/// A noise effect's grains in a pass's texture.
#[derive(Clone, Debug)]
pub struct Grains {
    /// Texture pixels → grain cells (`a b c d e f`, as `Affine`).
    pub cells: [f64; 6],
    pub seed: u32,
    /// 0 Mono, 1 Duo, 2 Multi.
    pub mode: u32,
    pub density: f32,
    pub c1: Color,
    pub c2: Color,
    pub multi_alpha: f32,
}

/// One effect of a layer: a texture the page scene draws through `image`.
pub struct Pass {
    pub source: Source,
    pub kind: Kind,
    /// How many times smaller than the scene's scale the texture is.
    pub k: u32,
    /// What it is cast from instead of the content (a shadow's spread outline, page coordinates).
    pub shape: Option<Scene>,
    /// The image the scene draws, whose pixels this pass makes (its size is the texture's).
    pub image: ImageData,
}

/// A layer with effects in the page scene: what its passes are made from.
pub struct FxNode {
    pub id: NodeId,
    /// The top-level layer it is in: the backdrop is what the top-level layers before it draw, then
    /// `prefix`.
    pub top: NodeId,
    /// The layer's content, its effects aside, in page coordinates.
    pub content: Scene,
    /// What its own top-level layer draws before it (backdrop passes only).
    pub prefix: Option<Scene>,
    /// The page area the textures cover.
    pub region: Rect,
    /// Device pixels per page unit the scene (and its textures) are made for.
    pub scale: f64,
    pub passes: Vec<Pass>,
    /// The textures made so far, in `passes` order (`None`: not yet; a noise pass's has no source).
    pub made: Mutex<Option<Vec<wgpu::Texture>>>,
}

impl FxNode {
    /// Page units → texture pixels of a pass drawn `k` times smaller.
    pub fn to_texture(&self, k: u32) -> Affine {
        let s = self.scale / f64::from(k.max(1));
        Affine::scale(s) * Affine::translate((-self.region.x0, -self.region.y0))
    }
}

/// The texture size of `region` at `scale / k` (`None`: too large or empty).
pub fn texture_size(region: Rect, scale: f64, k: u32) -> Option<(u32, u32)> {
    let s = scale / f64::from(k.max(1));
    let (w, h) = ((region.width() * s).ceil(), (region.height() * s).ceil());
    let ok = |v: f64| v.is_finite() && v >= 1.0 && v <= f64::from(MAX_SIDE);
    (ok(w) && ok(h)).then_some((w as u32, h as u32))
}

/// The factor a blur of `sigma` device pixels is drawn smaller by (as the CPU's soft shadows):
/// 1 under 16 pixels, else the power of two at most an eighth of it (at most 64).
pub fn factor(sigma: f64) -> u32 {
    if !(sigma.is_finite() && sigma >= 16.0) {
        return 1;
    }
    1 << ((sigma / 8.0).min(64.0).log2().floor().max(0.0) as u32)
}

#[derive(Clone, Copy)]
enum Out {
    /// One channel of coverage between passes.
    R,
    /// A tinted shadow.
    Shadow,
    /// Premultiplied colour between passes.
    Wide,
    /// Premultiplied colour out.
    Rgba,
}

impl Out {
    fn binding(self) -> u32 {
        match self {
            Out::R => 1,
            Out::Shadow => 3,
            Out::Wide => 4,
            Out::Rgba => 5,
        }
    }

    fn format(self) -> wgpu::TextureFormat {
        match self {
            Out::R => wgpu::TextureFormat::R32Float,
            Out::Shadow | Out::Rgba => wgpu::TextureFormat::Rgba8Unorm,
            Out::Wide => wgpu::TextureFormat::Rgba16Float,
        }
    }
}

/// The compute pipelines of [`fx.wgsl`](fx.wgsl) and [`grain.wgsl`](grain.wgsl).
pub struct Pipelines {
    noise: wgpu::ComputePipeline,
    grain_layout: wgpu::BindGroupLayout,
    extract: wgpu::ComputePipeline,
    morph_h: wgpu::ComputePipeline,
    morph_v: wgpu::ComputePipeline,
    alpha_h: wgpu::ComputePipeline,
    alpha_v_tint: wgpu::ComputePipeline,
    rgba_h: wgpu::ComputePipeline,
    rgba_v: wgpu::ComputePipeline,
    layouts: [wgpu::BindGroupLayout; 4],
}

fn layout(device: &wgpu::Device, out: Out) -> wgpu::BindGroupLayout {
    let entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None };
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("figcraft-fx"),
        entries: &[
            entry(
                0,
                wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
            ),
            entry(
                out.binding(),
                wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: out.format(),
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
            ),
            entry(2, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
        ],
    })
}

/// A pass's uniforms, laid out as `Params` in the shader.
fn params(sigma: f64, reach: i32, grow: bool, outside: bool, color: Color) -> [u8; 32] {
    let mut b = [0u8; 32];
    let words: [[u8; 4]; 8] = [
        (sigma as f32).to_le_bytes(),
        reach.to_le_bytes(),
        i32::from(grow).to_le_bytes(),
        i32::from(outside).to_le_bytes(),
        color.r.to_le_bytes(),
        color.g.to_le_bytes(),
        color.b.to_le_bytes(),
        color.a.to_le_bytes(),
    ];
    for (dst, w) in b.chunks_exact_mut(4).zip(words) {
        dst.copy_from_slice(&w);
    }
    b
}

/// Taps each side of a Gaussian of `sigma` pixels.
fn reach_of(sigma: f64) -> i32 {
    if sigma.is_finite() && sigma > 0.0 { (sigma * 3.0).ceil().clamp(1.0, f64::from(MAX_REACH)) as i32 } else { 0 }
}

impl Pipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("figcraft-fx"),
            source: wgpu::ShaderSource::Wgsl(include_str!("fx.wgsl").into()),
        });
        let layouts = [layout(device, Out::R), layout(device, Out::Shadow), layout(device, Out::Wide), layout(device, Out::Rgba)];
        let pipe = |entry: &str, l: &wgpu::BindGroupLayout| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("figcraft-fx"),
                bind_group_layouts: &[Some(l)],
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            })
        };
        let [r, shadow, wide, rgba] = &layouts;
        let grain_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("figcraft-grain"),
            source: wgpu::ShaderSource::Wgsl(include_str!("grain.wgsl").into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::COMPUTE, ty, count: None };
        let grain_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("figcraft-grain"),
            entries: &[
                entry(0, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
                entry(
                    1,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let gl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("figcraft-grain"),
            bind_group_layouts: &[Some(&grain_layout)],
            immediate_size: 0,
        });
        let noise = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("noise"),
            layout: Some(&gl),
            module: &grain_module,
            entry_point: Some("noise"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Pipelines {
            noise,
            grain_layout,
            extract: pipe("extract", r),
            morph_h: pipe("morph_h", r),
            morph_v: pipe("morph_v", r),
            alpha_h: pipe("alpha_h", r),
            alpha_v_tint: pipe("alpha_v_tint", shadow),
            rgba_h: pipe("rgba_h", wide),
            rgba_v: pipe("rgba_v", rgba),
            layouts: layouts.clone(),
        }
    }

    fn layout(&self, out: Out) -> &wgpu::BindGroupLayout {
        let [r, shadow, wide, rgba] = &self.layouts;
        match out {
            Out::R => r,
            Out::Shadow => shadow,
            Out::Wide => wide,
            Out::Rgba => rgba,
        }
    }

    /// One pass of `pipe` from `src` into a new texture of `out`'s format and `src`'s size.
    #[allow(clippy::too_many_arguments)]
    fn step(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        pipe: &wgpu::ComputePipeline,
        out: Out,
        src: &wgpu::Texture,
        p: [u8; 32],
    ) -> wgpu::Texture {
        let (w, h) = (src.width(), src.height());
        let mut usage = wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING;
        if matches!(out, Out::Shadow | Out::Rgba) {
            // The scene's image atlas copies it.
            usage |= wgpu::TextureUsages::COPY_SRC;
        }
        let dst = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("figcraft-fx"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: out.format(),
            usage,
            view_formats: &[],
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("figcraft-fx"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniforms, 0, &p);
        let src_view = src.create_view(&wgpu::TextureViewDescriptor::default());
        let dst_view = dst.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("figcraft-fx"),
            layout: self.layout(out),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&src_view) },
                wgpu::BindGroupEntry { binding: out.binding(), resource: wgpu::BindingResource::TextureView(&dst_view) },
                wgpu::BindGroupEntry { binding: 2, resource: uniforms.as_entire_binding() },
            ],
        });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("figcraft-fx"), timestamp_writes: None });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        drop(pass);
        dst
    }

    /// `kind` made from `src` (a layer or backdrop as vello draws it: straight alpha; noise has
    /// none and is `size`).
    pub fn run(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        enc: &mut wgpu::CommandEncoder,
        kind: &Kind,
        src: Option<&wgpu::Texture>,
        size: (u32, u32),
    ) -> Option<wgpu::Texture> {
        let none = Color::TRANSPARENT;
        match kind {
            Kind::Shadow { sigma, spread, color, inside } => {
                let o = *inside;
                let mut cov = self.step(device, queue, enc, &self.extract, Out::R, src?, params(0.0, 0, false, o, none));
                let r = if spread.is_finite() { spread.abs().round().min(f64::from(MAX_REACH)) as i32 } else { 0 };
                if r > 0 {
                    // Inverted, growing the coverage shrinks what lies outside it.
                    let grow = (*spread > 0.0) != o;
                    cov = self.step(device, queue, enc, &self.morph_h, Out::R, &cov, params(0.0, r, grow, o, none));
                    cov = self.step(device, queue, enc, &self.morph_v, Out::R, &cov, params(0.0, r, grow, o, none));
                }
                let n = reach_of(*sigma);
                let cov = self.step(device, queue, enc, &self.alpha_h, Out::R, &cov, params(*sigma, n, false, o, none));
                Some(self.step(device, queue, enc, &self.alpha_v_tint, Out::Shadow, &cov, params(*sigma, n, false, o, *color)))
            }
            Kind::Blur { sigma } => {
                let n = reach_of(*sigma);
                let wide = self.step(device, queue, enc, &self.rgba_h, Out::Wide, src?, params(*sigma, n, false, false, none));
                Some(self.step(device, queue, enc, &self.rgba_v, Out::Rgba, &wide, params(*sigma, n, false, false, none)))
            }
            Kind::Noise(g) => Some(self.grains(device, queue, enc, g, size)),
        }
    }

    fn grains(&self, device: &wgpu::Device, queue: &wgpu::Queue, enc: &mut wgpu::CommandEncoder, g: &Grains, (w, h): (u32, u32)) -> wgpu::Texture {
        let dst = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("figcraft-grain"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let [a, b, c, d, e, f] = g.cells.map(|v| v as f32);
        let floats = [a, b, c, d, e, f, 0.0, 0.0, g.c1.r, g.c1.g, g.c1.b, g.c1.a, g.c2.r, g.c2.g, g.c2.b, g.c2.a];
        let mut bytes = Vec::with_capacity(80);
        for v in floats {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&g.seed.to_le_bytes());
        bytes.extend_from_slice(&g.mode.to_le_bytes());
        bytes.extend_from_slice(&g.density.to_le_bytes());
        bytes.extend_from_slice(&g.multi_alpha.to_le_bytes());
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("figcraft-grain"),
            size: bytes.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniforms, 0, &bytes);
        let view = dst.create_view(&wgpu::TextureViewDescriptor::default());
        let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("figcraft-grain"),
            layout: &self.grain_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
            ],
        });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("figcraft-grain"), timestamp_writes: None });
        pass.set_pipeline(&self.noise);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(w.div_ceil(8), h.div_ceil(8), 1);
        drop(pass);
        dst
    }
}

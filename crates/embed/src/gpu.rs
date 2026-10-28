//! The view's wgpu device: its setup, the canvas tiles painted on it (vello) and EffectCraft's
//! compositor on it for the cards, as the desktop app installs them (`apps/figcraft/src/gpu_tiles.rs`).

use std::sync::Arc;

use eframe::{egui_wgpu, wgpu};
use figcraft_render::tiles::{NativeTile, TilePainter};
use figcraft_ui_egui::render_worker::NativeTextures;

/// The device setup: Metal on macOS, the adapter's own limits (textures capped at 16384 px), no
/// indirect-call validation (as EffectCraft's embedded view).
pub(crate) fn setup() -> egui_wgpu::WgpuSetupCreateNew {
    let mut setup = egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    #[cfg(target_os = "macos")]
    {
        setup.instance_descriptor.backends = wgpu::Backends::METAL;
    }
    setup.instance_descriptor.flags = setup.instance_descriptor.flags.difference(wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL).with_env();
    setup.device_descriptor = Arc::new(|adapter| wgpu::DeviceDescriptor {
        label: Some("FigCraft device (embedded)"),
        required_limits: wgpu::Limits { max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d.min(16384), ..adapter.limits() },
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    });
    setup
}

/// GPU tiles shown as the view's textures: they never come back through the CPU.
struct Textures {
    device: wgpu::Device,
    renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
}

impl NativeTextures for Textures {
    fn register(&self, tile: &NativeTile) -> Option<egui::TextureId> {
        let texture = tile.0.downcast_ref::<wgpu::Texture>()?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some(self.renderer.write().register_native_texture(&self.device, &view, wgpu::FilterMode::Linear))
    }

    fn free(&self, id: egui::TextureId) {
        self.renderer.write().free_texture(&id);
    }
}

/// The canvas painted on the GPU unless the preference (Draw the canvas on the GPU) is off.
pub(crate) fn install_tiles(rs: &egui_wgpu::RenderState, pref: bool) {
    if !pref {
        log::info!("embedded FigCraft: GPU canvas off, the CPU draws the canvas");
        return;
    }
    let (device, queue) = (rs.device.clone(), rs.queue.clone());
    figcraft_render_gpu::log_device_errors(&device);
    figcraft_ui_egui::render_worker::set_native_textures(Box::new(Textures { device: device.clone(), renderer: rs.renderer.clone() }));
    let info = rs.adapter.get_info();
    let adapter = format!("{} ({:?})", info.name, info.backend);
    let gpu = figcraft_render_gpu::shared_in_background(device, queue, adapter);
    figcraft_ui_egui::render_worker::set_tile_painter(Box::new(move || -> Option<Box<dyn TilePainter>> {
        Some(Box::new(figcraft_render_gpu::GpuTilePainter::with_shared(gpu.clone())))
    }));
}

/// In-process EffectCraft frames as the view's textures.
struct EffectCraftFrames {
    device: wgpu::Device,
    renderer: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
}

impl figcraft_ui_egui::effectcraft_ui::NativeFrames for EffectCraftFrames {
    fn register(&self, texture: &(dyn std::any::Any + Send + Sync)) -> Option<egui::TextureId> {
        let texture = texture.downcast_ref::<wgpu::Texture>()?;
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Some(self.renderer.write().register_native_texture(&self.device, &view, wgpu::FilterMode::Linear))
    }

    fn free(&self, id: egui::TextureId) {
        self.renderer.write().free_texture(&id);
    }
}

/// EffectCraft cards rendered in-process composite on EffectCraft's GPU compositor on this view's
/// device (started off the UI thread; the CPU renders until then).
pub(crate) fn install_effectcraft(rs: &egui_wgpu::RenderState) {
    let (adapter, device, queue) = (rs.adapter.clone(), rs.device.clone(), rs.queue.clone());
    let started =
        std::thread::Builder::new().name("effectcraft-gpu".into()).spawn(move || match figcraft_ec_inproc::share_device(&adapter, device, queue) {
            Ok(()) => log::info!("embedded FigCraft: EffectCraft cards composite on the view's GPU"),
            Err(e) => log::warn!("embedded FigCraft: EffectCraft's GPU compositor didn't start ({e}); cards render on the CPU"),
        });
    if started.is_ok() {
        figcraft_ui_egui::effectcraft_ui::set_native_frames(Box::new(EffectCraftFrames { device: rs.device.clone(), renderer: rs.renderer.clone() }));
    }
}

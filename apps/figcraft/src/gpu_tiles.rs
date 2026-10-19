//! Canvas tiles painted on the GPU (`figcraft-render-gpu`, vello) on the window's own wgpu device:
//! on unless Preferences ▸ Draw the canvas on the GPU is off (`pref.gpuCanvas`);
//! `FIGCRAFT_GPU_TILES=0` or `=1` overrides it for one run. The CPU paints whatever the GPU can't,
//! and everything when the device can't run vello's compute shaders (an OpenGL fallback, say).

use std::sync::Arc;

use eframe::egui_wgpu;
use eframe::wgpu;
use figcraft_render::tiles::{NativeTile, TilePainter};
use figcraft_ui_egui::render_worker::NativeTextures;

/// GPU tiles shown as the window's textures: they never come back through the CPU.
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

/// Whether a switch says yes.
fn wanted(v: Option<&str>) -> bool {
    v.is_some_and(|v| matches!(v.trim(), "1" | "true" | "on" | "yes"))
}

/// Whether the GPU paints: `FIGCRAFT_GPU_TILES` when it's a yes or a no, else the preference.
fn on(env: Option<&str>, pref: bool) -> bool {
    match env.map(str::trim) {
        Some("0" | "false" | "off" | "no") => false,
        Some(v) if wanted(Some(v)) => true,
        _ => pref,
    }
}

/// Install the GPU tile painter when it's on and the window draws with wgpu.
pub fn install(cc: &eframe::CreationContext<'_>, pref: bool) {
    if !on(std::env::var("FIGCRAFT_GPU_TILES").ok().as_deref(), pref) {
        log::info!("GPU canvas off: the CPU draws the canvas");
        return;
    }
    let Some(rs) = &cc.wgpu_render_state else {
        log::warn!("GPU tiles: the window doesn't draw with wgpu");
        return;
    };
    let (device, queue) = (rs.device.clone(), rs.queue.clone());
    // `FIGCRAFT_GPU_TILES_COPY=1` brings the pixels back through the CPU instead (for measuring).
    if !wanted(std::env::var("FIGCRAFT_GPU_TILES_COPY").ok().as_deref()) {
        figcraft_ui_egui::render_worker::set_native_textures(Box::new(Textures { device: device.clone(), renderer: rs.renderer.clone() }));
    }
    let info = rs.adapter.get_info();
    let adapter = format!("{} ({:?})", info.name, info.backend);
    // Shaders compile now, in the background; the CPU paints until they're ready.
    let gpu = figcraft_render_gpu::shared_in_background(device, queue, adapter);
    figcraft_ui_egui::render_worker::set_tile_painter(Box::new(move || -> Option<Box<dyn TilePainter>> {
        Some(Box::new(figcraft_render_gpu::GpuTilePainter::with_shared(gpu.clone())))
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_yes_turns_gpu_tiles_on() {
        assert!(super::wanted(Some("1")) && super::wanted(Some(" on ")));
        assert!(!super::wanted(None) && !super::wanted(Some("0")) && !super::wanted(Some("")));
    }

    #[test]
    fn the_environment_overrides_the_preference_only_when_it_says_yes_or_no() {
        use super::on;
        assert!(on(None, true) && !on(None, false));
        assert!(!on(Some("0"), true) && !on(Some(" off"), true));
        assert!(on(Some("1"), false));
        assert!(on(Some(""), true) && !on(Some("maybe"), false));
    }
}

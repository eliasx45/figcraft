//! Canvas tiles painted on the GPU (`figcraft-render-gpu`, vello) on the window's own wgpu device,
//! when asked for with `FIGCRAFT_GPU_TILES=1`. Off by default until it measures better than the
//! CPU painter (`docs/gaps.md`, gap 7); the CPU paints whatever the GPU can't, and everything
//! when the device can't run vello's compute shaders (an OpenGL fallback, say).

use figcraft_render::tiles::TilePainter;

/// Whether `FIGCRAFT_GPU_TILES` asks for GPU tiles.
fn wanted(v: Option<&str>) -> bool {
    v.is_some_and(|v| matches!(v.trim(), "1" | "true" | "on" | "yes"))
}

/// Install the GPU tile painter when it's asked for and the window draws with wgpu.
pub fn install(cc: &eframe::CreationContext<'_>) {
    if !wanted(std::env::var("FIGCRAFT_GPU_TILES").ok().as_deref()) {
        return;
    }
    let Some(rs) = &cc.wgpu_render_state else {
        log::warn!("GPU tiles: the window doesn't draw with wgpu");
        return;
    };
    let (device, queue) = (rs.device.clone(), rs.queue.clone());
    let info = rs.adapter.get_info();
    let adapter = format!("{} ({:?})", info.name, info.backend);
    figcraft_ui_egui::render_worker::set_tile_painter(Box::new(move || -> Option<Box<dyn TilePainter>> {
        match figcraft_render_gpu::Gpu::shared(device.clone(), queue.clone(), adapter.clone()) {
            Ok(gpu) => Some(Box::new(figcraft_render_gpu::GpuTilePainter::new(gpu))),
            Err(e) => {
                log::warn!("GPU tiles: {e}; painting on the CPU");
                None
            }
        }
    }));
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_yes_turns_gpu_tiles_on() {
        assert!(super::wanted(Some("1")) && super::wanted(Some(" on ")));
        assert!(!super::wanted(None) && !super::wanted(Some("0")) && !super::wanted(Some("")));
    }
}

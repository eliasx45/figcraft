//! Frames off the link thread: a [`Snapshot`] of an in-process session (what a render reads, all
//! shared `Arc`s) renders one frame at a time on a worker, on EffectCraft's GPU compositor running
//! on FigCraft's own wgpu device when the app shared it ([`share_device`]), else on the CPU.
//!
//! A GPU frame stays on the GPU: [`Picture::Native`] carries the `wgpu::Texture` (premultiplied
//! RGBA8 with mips, egui-wgpu's native texture format) for the app to register with egui. The CPU
//! renders a frame the GPU can't (no shared device, an effect without a GPU path, a device fault)
//! and the poster frame (its bytes go into the document).

use std::any::Any;
use std::sync::{Arc, OnceLock};

use effectcraft_engine::Session;
use effectcraft_engine::project::ItemId;
use effectcraft_engine::render::{Accelerator, RenderOpts, Renderer};
use serde_json::{Value, json};

use crate::{InProc, guarded};

/// The compositor on FigCraft's device, once the app shared it.
static GPU: OnceLock<effectcraft_gpu::Gpu> = OnceLock::new();

/// Share FigCraft's wgpu device (eframe's render state) with EffectCraft's GPU compositor. Call
/// once, off the UI thread (it builds the compositor's context); later calls are ignored.
pub fn share_device(adapter: &wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Result<(), String> {
    if GPU.get().is_some() {
        return Ok(());
    }
    let gpu = guarded("gpu", || effectcraft_gpu::Gpu::new(adapter, device, queue))?;
    let _ = GPU.set(gpu);
    Ok(())
}

/// Whether frames can render on the GPU (the app shared its device).
pub fn gpu_shared() -> bool {
    GPU.get().is_some()
}

/// A rendered frame.
pub enum Picture {
    /// A PNG (the CPU's frame).
    Png(Vec<u8>),
    /// A `wgpu::Texture` on FigCraft's device (`Box<dyn Any>` so the UI crate needs no wgpu).
    Native { texture: Box<dyn Any + Send + Sync>, width: u32, height: u32 },
}

/// Which compositor a frame may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compositor {
    /// The GPU on the shared device, falling back to the CPU.
    Gpu,
    Cpu,
}

/// What rendering a comp of one project reads, detached from its session (cheap: shared `Arc`s).
#[derive(Clone)]
pub struct Snapshot {
    project: Arc<effectcraft_engine::project::Project>,
    services: Arc<dyn effectcraft_engine::Services>,
    footage: Arc<dyn effectcraft_engine::render::FootageSource>,
    expr: Option<Arc<dyn effectcraft_engine::render::ExprHost>>,
    layer_cache: Arc<effectcraft_engine::render::LayerCache>,
    prefs: effectcraft_engine::prefs::Prefs,
    state: effectcraft_engine::EditorState,
    /// The project file's revision ([`InProc::revision`]).
    pub revision: u64,
}

impl InProc {
    /// What a frame worker renders from.
    pub fn snapshot(&self) -> Snapshot {
        let s = &self.session;
        Snapshot {
            project: s.project.clone(),
            services: s.services.clone(),
            footage: s.footage.clone(),
            expr: s.expr.clone(),
            layer_cache: s.layer_cache.clone(),
            prefs: s.prefs.clone(),
            state: s.state.clone(),
            revision: self.revision(),
        }
    }

    /// The id of the comp `comp` names (an id or a name).
    pub fn comp_id(&self, comp: &Value) -> Result<u64, String> {
        self.comp(&json!({"comp": comp})).map(|c| c.0)
    }
}

impl Snapshot {
    /// The id of the comp `comp` names (its id, or its name when that is unambiguous).
    pub fn comp_id(&self, comp: &Value) -> Result<u64, String> {
        match comp {
            Value::Number(n) => n.as_u64().filter(|id| self.project.comp(ItemId(*id)).is_some()).ok_or_else(|| format!("no composition {n}")),
            Value::String(name) => {
                let mut found = self.project.items.values().filter(|i| &i.name == name && i.as_comp().is_some()).map(|i| i.id.0);
                match (found.next(), found.next()) {
                    (Some(id), None) => Ok(id),
                    (Some(_), Some(_)) => Err(format!("more than one composition is called {name:?}")),
                    _ => Err(format!("no composition {name:?}")),
                }
            }
            _ => Err("not a composition id or name".into()),
        }
    }

    /// `figcraft.tree` and `figcraft.audio` from this snapshot (a hosted panel's unsaved
    /// edits), the same answers [`InProc::call`] gives from the saved file.
    pub fn call(&self, method: &str, params: &Value) -> Result<Value, String> {
        guarded(method, || {
            let cid = ItemId(self.comp_id(params.get("comp").unwrap_or(&Value::Null))?);
            match method {
                "figcraft.tree" => crate::tree_in(&self.project, cid, params),
                "figcraft.audio" => crate::audio_in(&self.project, self.footage.as_ref(), self.expr.as_deref(), cid, params),
                other => Err(format!("unknown method {other} (EffectCraft snapshot)")),
            }
        })
    }

    /// A session over this snapshot (`cache`: share the layer cache; a CPU retry after a failed
    /// GPU attempt doesn't, as EffectCraft's viewer does).
    fn session(&self, cache: bool) -> Session {
        let mut s = Session {
            project: self.project.clone(),
            services: self.services.clone(),
            footage: self.footage.clone(),
            expr: self.expr.clone(),
            prefs: self.prefs.clone(),
            state: self.state.clone(),
            ..Session::default()
        };
        if cache {
            s.layer_cache = self.layer_cache.clone();
        }
        s
    }

    /// Render frame `frame` of comp `comp` with its longest side at most `max_side`.
    pub fn frame(&self, comp: u64, frame: i64, max_side: u32, with: Compositor) -> Result<Picture, String> {
        if !(0..=crate::MAX_FRAME).contains(&frame) {
            return Err("not a frame number".into());
        }
        let side = u64::from(max_side).clamp(16, crate::MAX_SIDE) as u32;
        if with == Compositor::Gpu
            && let Some(gpu) = GPU.get()
        {
            match guarded("gpu frame", || Ok(self.on_gpu(gpu, ItemId(comp), frame, side))) {
                Ok(Some(p)) => return Ok(p),
                Ok(None) => {}
                Err(e) => log::warn!("EffectCraft GPU frame: {e}; on the CPU"),
            }
        }
        self.on_cpu(comp, frame, side, with == Compositor::Cpu)
    }

    /// `None`: the GPU can't (the CPU renders it).
    fn on_gpu(&self, gpu: &effectcraft_gpu::Gpu, cid: ItemId, frame: i64, side: u32) -> Option<Picture> {
        let comp = self.project.comp(cid)?;
        let long = f64::from(comp.width.max(comp.height).max(1));
        let t = comp.frame_rate.tick_of(frame);
        let opts = RenderOpts {
            scale: (f64::from(side) / long).min(1.0),
            nested_switches: self.prefs.general.switches_affect_nested_comps,
            draft_shadows: self.prefs.three_d.realtime_shadows,
            ..Default::default()
        };
        let mut r = Renderer::new(&self.project, self.footage.as_ref(), opts);
        r.expr = self.expr.as_deref();
        r.cache = Some(self.layer_cache.as_ref());
        r.accel = Some(gpu as &dyn Accelerator);
        let f = gpu.render_display(&r, cid, t)?;
        Some(Picture::Native { width: f.width, height: f.height, texture: Box::new(f.texture) })
    }

    fn on_cpu(&self, comp: u64, frame: i64, side: u32, cache: bool) -> Result<Picture, String> {
        guarded("cpu frame", || {
            let mut s = self.session(cache);
            let v = s.execute("render.frame", json!({"comp": comp, "frame": frame, "max_side": side, "base64": true})).map_err(|e| e.to_string())?;
            let png = v.get("png").and_then(Value::as_str).ok_or("no picture")?;
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.decode(png.trim()).map(Picture::Png).map_err(|e| e.to_string())
        })
    }
}

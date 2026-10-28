//! EffectCraft's engine inside FigCraft (fork-only, desktop-only): an EffectCraft card renders
//! from its saved `.ecproj` without the EffectCraft app running.
//!
//! [`InProc`] is one EffectCraft session (engine + footage decoding + expressions, no UI, no
//! scripting, no export) that opens a project read-only and answers the same methods the
//! EffectCraft app's control channel answers for FigCraft (`figcraft.frames`, `figcraft.tree`,
//! `figcraft.audio`, `app.revision`, `render.frame`), with the same JSON shapes, so the FigCraft
//! side swaps transports without changing what it asks. Differences from the app:
//!
//! - frames are rendered here, on the CPU, a few per call ([`FRAMES_PER_CALL`]); the rest of a
//!   request come back `pending` and are asked for again. Each frame is a PNG (`png`, not `jpeg`).
//! - the revision is the project file's modification stamp: a save in EffectCraft reloads the
//!   session on the next call and changes it.
//! - `figcraft.reveal` needs the app (it opens the comp in EffectCraft's UI): refused here.
//!
//! Frames for the canvas render off the link thread from a [`Snapshot`], on EffectCraft's GPU
//! compositor sharing FigCraft's wgpu device when the app shared it ([`share_device`]), as GPU
//! textures egui draws without a copy ([`Picture::Native`]); else on the CPU.
//!
//! The session never saves, never announces itself as an EffectCraft host and writes no
//! EffectCraft settings (it has no config directory). [`Pool`] keeps a few sessions, one per
//! project.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// What FigCraft's MAIN board asks of a project (`figcraft.board`, `figcraft.speech`, …).
pub mod board;
mod frames;
/// EffectCraft's engine, for hosts of its panels (the Timeline names its `Session`).
pub use effectcraft_engine;
pub use frames::{Compositor, Picture, Snapshot, gpu_shared, share_device};

use effectcraft_engine::Session;
use effectcraft_engine::project::ItemId;
use effectcraft_engine::time::Tick;
use serde_json::{Value, json};

/// Frames rendered in one `figcraft.frames` call (the rest are reported `pending`).
pub const FRAMES_PER_CALL: usize = 4;
/// Frames one `figcraft.frames` request may name.
const MAX_FRAMES: usize = 256;
/// Largest frame side rendered (pixels).
const MAX_SIDE: u64 = 4096;
/// Highest frame number accepted (about 4.6 days at 25 fps).
const MAX_FRAME: i64 = 10_000_000;
/// Longest piece of sound mixed at once (seconds), and the sample rates accepted.
const MAX_SOUND: f64 = 120.0;
const RATES: std::ops::RangeInclusive<u64> = 8_000..=192_000;
/// Biggest project file opened (bytes).
const MAX_PROJECT: u64 = 512 << 20;
/// Sessions a [`Pool`] keeps (each holds a whole project and its decoded footage).
pub const POOL: usize = 2;

/// What identifies one saved state of a project file: its length and modification time (ns).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: u128,
}

fn stamp(path: &Path) -> Result<Stamp, String> {
    let m = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !m.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let modified = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    Ok(Stamp { len: m.len(), modified })
}

/// The project path as sessions are keyed (absolute when it exists).
pub fn key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// One EffectCraft session holding one saved project.
pub struct InProc {
    path: PathBuf,
    stamp: Stamp,
    session: Session,
}

/// Footage import through EffectCraft's media layer (as `effectcraft_host` wires it up), so a
/// hosted panel's File ▸ Import, a Project panel import and files dropped on a card probe and
/// add footage exactly as EffectCraft does.
struct MediaImporter;

impl effectcraft_engine::Importer for MediaImporter {
    fn probe(&self, path: &str) -> Result<effectcraft_engine::project::Footage, String> {
        effectcraft_media::probe(path).map_err(|e| e.to_string())
    }
    fn hardware_refuses(&self, footage: &effectcraft_engine::project::Footage) -> bool {
        effectcraft_media::hw::refuses(&footage.codec, footage.width, footage.height)
    }
}

/// A session with what rendering and importing need: footage decoding and import, expressions
/// (like `effectcraft_host::session()` without scripting, plug-ins and export).
fn new_session() -> Session {
    Session {
        importer: Some(std::sync::Arc::new(MediaImporter)),
        footage: std::sync::Arc::new(effectcraft_media::MediaPool::new()),
        expr: Some(std::sync::Arc::new(effectcraft_expr::Expressions)),
        expr_check: Some(effectcraft_expr::check_syntax),
        ..Default::default()
    }
}

/// Run `f`, turning a panic inside EffectCraft's engine into an error.
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err(format!("EffectCraft engine: an internal error ({what})")))
}

impl InProc {
    /// Open the project at `path` (an `.ecproj`, or the XML copy EffectCraft writes).
    pub fn open(path: &Path) -> Result<InProc, String> {
        let path = key(path);
        let stamp = stamp(&path)?;
        if stamp.len > MAX_PROJECT {
            return Err(format!("{} is too big to open here", path.display()));
        }
        let text = path.to_str().ok_or_else(|| format!("{} is not a UTF-8 path", path.display()))?.to_string();
        let session = guarded("open", || {
            let mut s = new_session();
            s.execute("file.open", json!({"path": text})).map_err(|e| e.to_string())?;
            Ok(s)
        })?;
        Ok(InProc { path, stamp, session })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The project file's modification time in whole seconds (what FigCraft compares).
    pub fn file_seconds(&self) -> u64 {
        u64::try_from(self.stamp.modified / 1_000_000_000).unwrap_or(u64::MAX)
    }

    /// The revision `figcraft.frames` / `app.revision` report: changes with every save, and
    /// with every edit made in this session (a hosted panel editing it).
    pub fn revision(&self) -> u64 {
        // Never 0 (FigCraft reads 0 as "not known").
        let mixed = (self.stamp.modified as u64) ^ self.stamp.len.rotate_left(32) ^ self.session.revision.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        mixed.max(1)
    }

    /// The session (a hosted EffectCraft panel draws and edits it).
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    /// Edits not saved yet.
    pub fn is_dirty(&self) -> bool {
        self.session.is_dirty()
    }

    /// Save the project over its file with EffectCraft's own save (atomic; refused when the file
    /// changed on disk since it was opened or saved here).
    pub fn save(&mut self) -> Result<(), String> {
        let path = self.path.to_str().ok_or("not a UTF-8 path")?.to_string();
        guarded("save", || self.session.execute("file.save", json!({"path": path})).map(|_| ()).map_err(|e| e.to_string()))?;
        // What's on disk is now this session: no reload for it.
        self.stamp = stamp(&self.path)?;
        Ok(())
    }

    /// Throw away this session's edits: the project as saved.
    pub fn reload(&mut self) -> Result<(), String> {
        *self = InProc::open(&self.path)?;
        Ok(())
    }

    /// Reopen the project when its file changed since it was opened (saved in EffectCraft).
    pub fn refresh(&mut self) -> Result<bool, String> {
        let now = stamp(&self.path)?;
        if now == self.stamp {
            return Ok(false);
        }
        let fresh = InProc::open(&self.path)?;
        *self = fresh;
        Ok(true)
    }

    /// Answer one control-channel request. `Err` is the error the app would have answered.
    pub fn call(&mut self, method: &str, params: &Value) -> Result<Value, String> {
        guarded(method, || match method {
            "app.revision" => Ok(json!({"revision": self.revision()})),
            "figcraft.frames" => self.frames(params),
            "figcraft.tree" => self.tree(params),
            "figcraft.audio" => self.audio(params),
            "figcraft.reveal" => Err("opening a comp needs the EffectCraft app: open it first".into()),
            // FigCraft's MAIN board (`figcraft_engine::ecboard`).
            "figcraft.board" => board::board(&self.session, &self.path, params),
            "figcraft.speech" => board::speech(&mut self.session, params),
            "figcraft.mark" => board::mark(&mut self.session, params),
            "figcraft.layerAt" => board::layer_at(&mut self.session, params),
            "figcraft.comp" => board::comp(&self.session, params),
            "figcraft.moves" => board::moves(&mut self.session, params),
            "render.frame" => {
                let mut p = params.clone();
                if let Some(o) = p.as_object_mut() {
                    // Never a file: the picture comes back inline.
                    o.remove("path");
                    o.remove("wait");
                    o.insert("base64".into(), json!(true));
                } else {
                    return Err("render.frame: parameters must be an object".into());
                }
                self.session.execute("render.frame", p).map_err(|e| e.to_string())
            }
            other => Err(format!("unknown method {other} (in-process EffectCraft)")),
        })
    }

    fn comp(&self, params: &Value) -> Result<ItemId, String> {
        match params.get("comp") {
            Some(v @ (Value::Number(_) | Value::String(_))) => self.session.resolve_comp(Some(v)).map_err(|e| e.to_string()),
            _ => Err("`comp` (a composition id or name) is required".into()),
        }
    }

    /// `figcraft.frames {comp, frames, maxSide}` → `{revision, frames: [{frame, w, h, png} |
    /// {frame, pending: true} | {frame, error}]}`.
    fn frames(&mut self, p: &Value) -> Result<Value, String> {
        let cid = self.comp(p)?;
        let list = p.get("frames").and_then(Value::as_array).ok_or("`frames` must be a list of frame numbers")?;
        if list.len() > MAX_FRAMES {
            return Err(format!("at most {MAX_FRAMES} frames at once"));
        }
        let side = p.get("maxSide").or_else(|| p.get("max_side")).and_then(Value::as_u64).unwrap_or(960).clamp(16, MAX_SIDE);
        let mut out = Vec::with_capacity(list.len());
        let mut rendered = 0;
        for v in list {
            let Some(f) = v.as_i64().filter(|f| (0..=MAX_FRAME).contains(f)) else {
                out.push(json!({"frame": v, "error": "not a frame number"}));
                continue;
            };
            if rendered >= FRAMES_PER_CALL {
                out.push(json!({"frame": f, "pending": true}));
                continue;
            }
            rendered += 1;
            let r = self.session.execute("render.frame", json!({"comp": cid.0, "frame": f, "max_side": side, "base64": true}));
            match r {
                Ok(v) => match v.get("png").and_then(Value::as_str) {
                    Some(png) => out.push(json!({"frame": f, "w": v.get("width"), "h": v.get("height"), "png": png})),
                    None => out.push(json!({"frame": f, "error": "no picture"})),
                },
                Err(e) => out.push(json!({"frame": f, "error": e.to_string()})),
            }
        }
        Ok(json!({"revision": self.revision(), "frames": out}))
    }

    /// `figcraft.tree {comp, start, end, depth?}` (comp seconds) → the app's tree.
    fn tree(&self, p: &Value) -> Result<Value, String> {
        tree_in(&self.session.project, self.comp(p)?, p)
    }

    /// `figcraft.audio {comp, start, end, rate?}` → `{wav}` (16-bit stereo) or `{silent: true}`.
    fn audio(&self, p: &Value) -> Result<Value, String> {
        let s = &self.session;
        audio_in(&s.project, s.footage.as_ref(), s.expr.as_deref(), self.comp(p)?, p)
    }
}

/// `figcraft.tree` of comp `cid` of `project`.
fn tree_in(project: &effectcraft_engine::project::Project, cid: ItemId, p: &Value) -> Result<Value, String> {
    let (start, end) = span(p)?;
    let depth = p.get("depth").and_then(Value::as_u64).unwrap_or(3).min(8) as u32;
    effectcraft_engine::commands::figcraft_link::tree(project, cid, Tick::from_seconds_f64(start), Tick::from_seconds_f64(end), depth)
        .ok_or_else(|| "no such composition".into())
}

/// `figcraft.audio` of comp `cid` of `project`.
fn audio_in(
    project: &effectcraft_engine::project::Project,
    footage: &dyn effectcraft_engine::render::FootageSource,
    expr: Option<&dyn effectcraft_engine::render::ExprHost>,
    cid: ItemId,
    p: &Value,
) -> Result<Value, String> {
    let (start, end) = span(p)?;
    let rate = p.get("rate").and_then(Value::as_u64).unwrap_or(48_000);
    if !RATES.contains(&rate) {
        return Err(format!("`rate` must be {} to {} Hz", RATES.start(), RATES.end()));
    }
    let rate = rate as u32;
    if !effectcraft_engine::render::audio::comp_has_audio(project, cid) {
        return Ok(json!({"silent": true}));
    }
    let frames = ((end - start).min(MAX_SOUND) * f64::from(rate)).round().max(0.0) as usize;
    let mix = effectcraft_engine::render::audio::mix_comp(project, footage, expr, cid, Tick::from_seconds_f64(start), frames, rate);
    if mix.iter().all(|x| x.abs() < 1e-6) {
        return Ok(json!({"silent": true}));
    }
    use base64::Engine as _;
    Ok(json!({"wav": base64::engine::general_purpose::STANDARD.encode(wav16(&mix, rate)), "rate": rate}))
}

/// `start` and `end` (seconds, finite, start ≤ end).
fn span(p: &Value) -> Result<(f64, f64), String> {
    let get = |k: &str| p.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && x.abs() < 1e7);
    let start = get("start").ok_or("`start` must be seconds")?;
    let end = get("end").ok_or("`end` must be seconds")?;
    if end < start {
        return Err("`end` comes before `start`".into());
    }
    Ok((start, end))
}

/// Interleaved stereo float samples as a 16-bit PCM WAV file.
pub fn wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data = (samples.len() * 2).min(u32::MAX as usize - 44) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate.saturating_mul(4)).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in samples.iter().take(data as usize / 2) {
        let v = if s.is_finite() { (s.clamp(-1.0, 1.0) * 32767.0).round() as i16 } else { 0 };
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// A few in-process sessions, one per project, the least recently used dropped first.
#[derive(Default)]
pub struct Pool {
    sessions: Vec<InProc>,
}

impl Pool {
    /// The session of the project at `path`, opened (or reopened after a save) as needed.
    pub fn get(&mut self, path: &Path) -> Result<&mut InProc, String> {
        let k = key(path);
        let at = match self.sessions.iter().position(|s| s.path == k) {
            Some(i) => {
                let mut s = self.sessions.remove(i);
                match s.refresh() {
                    Ok(true) => log::info!("EffectCraft in FigCraft: reloaded {} (saved since)", k.display()),
                    Ok(false) => {}
                    Err(e) => {
                        // Gone or unreadable now: forget it.
                        log::warn!("EffectCraft in FigCraft: {} went away: {e}", k.display());
                        return Err(e);
                    }
                }
                self.sessions.push(s);
                self.sessions.len() - 1
            }
            None => {
                let t0 = std::time::Instant::now();
                let s = InProc::open(&k).map_err(|e| {
                    log::warn!("EffectCraft in FigCraft: can't open {}: {e}", k.display());
                    e
                })?;
                log::info!("EffectCraft in FigCraft: opened {} in {:.1} s", k.display(), t0.elapsed().as_secs_f64());
                if self.sessions.len() >= POOL {
                    self.sessions.remove(0);
                }
                self.sessions.push(s);
                self.sessions.len() - 1
            }
        };
        self.sessions.get_mut(at).ok_or_else(|| "EffectCraft session lost".into())
    }

    /// Forget the session of `path` (after it failed).
    pub fn drop_project(&mut self, path: &Path) {
        let k = key(path);
        self.sessions.retain(|s| s.path != k);
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }
}

/// Make a new project at `path` (never over an existing file) holding one comp made by
/// EffectCraft's `comp.new` with `params` (its New Composition settings), saved with EffectCraft's
/// own save. Returns the comp's id.
pub fn create_project(path: &Path, params: &Value) -> Result<u64, String> {
    if path.exists() {
        return Err(format!("{} already exists", path.display()));
    }
    let text = path.to_str().ok_or("not a UTF-8 path")?.to_string();
    guarded("create", || {
        let mut s = new_session();
        let e = |e: effectcraft_engine::EngineError| e.to_string();
        s.execute("comp.new", params.clone()).map_err(e)?;
        let cid = s.active_comp_id().ok_or("no comp")?.0;
        s.execute("file.saveAs", json!({"path": text})).map_err(e)?;
        Ok(cid)
    })
}

/// Save a small project at `path` for tests: a 64×36, 2-second, 25 fps comp `C` holding a red
/// solid. Returns the comp's id.
#[doc(hidden)]
pub fn sample_project(path: &Path) -> Result<u64, String> {
    let text = path.to_str().ok_or("not a UTF-8 path")?.to_string();
    guarded("sample", || {
        let mut s = new_session();
        let e = |e: effectcraft_engine::EngineError| e.to_string();
        s.execute("comp.new", json!({"name": "C", "width": 64, "height": 36, "duration": 2, "frameRate": 25})).map_err(e)?;
        let cid = s.active_comp_id().ok_or("no comp")?.0;
        s.execute("layer.newSolid", json!({"color": "#ff0000", "width": 64, "height": 36})).map_err(e)?;
        s.execute("file.saveAs", json!({"path": text})).map_err(e)?;
        Ok(cid)
    })
}

#[cfg(test)]
mod tests;

//! Fork-only (`effectcraft-inproc`): three more of EffectCraft's panels as tabs in the column
//! right of a card's Timeline, beside Effect Controls and Project (`effectcraft_project::side`
//! draws the tab row; the column keeps its width edge, `effectcraft_strips`).
//!
//! - **Text.** EffectCraft's Character panel over its Paragraph panel
//!   (`panels::text_panels::{character, paragraph}`, hostable since EffectCraft's EMBED13), with a
//!   divider to drag between them. They act on the text being edited on the card (the Type tool's
//!   selection, `effectcraft_viewer`: style runs for the selected characters) or else on the
//!   selected text layers, exactly as in EffectCraft: every change is EffectCraft's `layer.setText`
//!   through the Timeline's host, one undo step (a drag of a number is one), ⌘Z/⌘S in the
//!   Timeline, the app's history when the panel mirrors the EffectCraft app. The fill / stroke
//!   eyedroppers sample the card (`EcHost::pick`).
//! - **Markers.** EffectCraft's Markers panel (`panels::misc::markers`: the comp's markers, a click
//!   moves the playhead there, a double-click opens the marker's Composition Marker dialog) under
//!   two buttons, Add Comp Marker (EffectCraft's `comp.addMarker`, Num* there) and Add Layer Marker
//!   (`layer.addMarker` on the selected layers). EffectCraft's Composition / Layer Marker dialog
//!   (`markers_ui::{draft_for, draw_dialog}`) shows in a FigCraft window for a double-click here or
//!   on a marker in the Timeline ([`request_dialog`], `EcHost::request_dialog`); OK runs
//!   `markers.set`, Delete `markers.delete`, through the host (one undo step each).
//! - **Library.** EffectCraft's Motion Library panel (`panels::motion_library::show`): Elias's
//!   cards, curves, presets and tools, read from his library as EffectCraft reads it. The session
//!   gets EffectCraft's settings folder read only (`motion_library::hosted_setup`: the store
//!   refuses writes) and his library folders. Applying a card, a curve or a preset, saving to the
//!   library and the tools run EffectCraft's own code; the library's EffectCraft plug
//!   (`effectcraft_plug.py`), which works on the open project through a control port, reaches
//!   this Timeline's session through [`bridge_port`]: a local port FigCraft opens the first time
//!   the library asks for it, answering the plug's `engine.execute` with the Timeline's host (so a
//!   curve applied to the selected keys is an edit here, one undo step per batch the plug sends).
//!   The panel's own commands (`motionLibrary.*`) run on the host (`EcHost::invoke`, [`intercept`]),
//!   so a card dragged onto the Timeline applies there.
//! - **Keys.** A click in a tab gives it the keyboard: EffectCraft's shortcuts for that panel
//!   (Character, Markers, Motion Library) from Elias's own keymap, as for the Timeline.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use effectcraft_ui_egui::automation::Registry;
use effectcraft_ui_egui::panels::markers_ui::{self, MarkerDraft};
use effectcraft_ui_egui::panels::timeline::{DialogRequest, TimelineHost};
use effectcraft_ui_egui::panels::{misc, motion_library, text_panels};
use effectcraft_ui_egui::theme::{ThemeKind, Tokens};
use effectcraft_ui_egui::widgets;
use egui::{Rect, Sense, Stroke, pos2, vec2};
use serde_json::{Value, json};

use crate::FigApp;
use crate::effectcraft_panel::EcHost;

/// The Markers tab's button row (panel units).
const BAR: f32 = 28.0;
/// The Text tab's divider (panel units) and the share Character starts with.
const DIVIDER: f32 = 6.0;
const CHARACTER_SHARE: f32 = 0.62;
/// Smallest height either text panel keeps (panel units).
const MIN_PART: f32 = 60.0;
/// Longest line the plug bridge reads, plug requests run in one frame, and how long the plug
/// waits for one.
const MAX_LINE: usize = 8 << 20;
const PER_FRAME: usize = 64;
const PLUG_WAIT: Duration = Duration::from_secs(60);

// ------------------------------------------------------------------------------------------
// The tabs

/// Character over Paragraph in `rect`, with a divider between them.
pub(crate) fn text(host: &mut EcHost, ui: &mut egui::Ui, rect: Rect) {
    let ctx = ui.ctx().clone();
    let t = host.tokens();
    let id = egui::Id::new(("figcraft-ec-text-split", host.path()));
    let share = ctx.data(|d| d.get_temp::<f32>(id)).filter(|s| s.is_finite()).unwrap_or(CHARACTER_SHARE);
    let usable = (rect.height() - DIVIDER).max(0.0);
    let top_h = if usable >= 2.0 * MIN_PART { (usable * share).clamp(MIN_PART, usable - MIN_PART) } else { usable * 0.5 };
    let character = Rect::from_min_size(rect.min, vec2(rect.width(), top_h));
    let divider = Rect::from_min_size(pos2(rect.min.x, character.max.y), vec2(rect.width(), DIVIDER));
    let paragraph = Rect::from_min_max(pos2(rect.min.x, divider.max.y), rect.max);
    let resp = ui.interact(divider, id.with("edge"), Sense::click_and_drag()).on_hover_cursor(egui::CursorIcon::ResizeVertical);
    host.auto_mut().add_response("figcraft.text.divider", &resp, "Resize Character and Paragraph");
    if resp.dragged() && usable > 0.0 {
        let next = ((top_h + resp.drag_delta().y) / usable).clamp(0.0, 1.0);
        ctx.data_mut(|d| d.insert_temp(id, next));
    } else if resp.double_clicked() {
        ctx.data_mut(|d| d.insert_temp(id, CHARACTER_SHARE));
    }
    let line = if resp.hovered() || resp.dragged() { Stroke::new(2.0, t.accent) } else { Stroke::new(1.0, t.separator) };
    ui.painter().hline(divider.x_range(), divider.center().y, line);
    // Each in its own clipped child, as EffectCraft docks them: their rows scroll and must not
    // spill over the other panel.
    if character.height() > 4.0 {
        clipped(ui, character, |ui| text_panels::character(host, ui, character));
    }
    if paragraph.height() > 4.0 {
        clipped(ui, paragraph, |ui| text_panels::paragraph(host, ui, paragraph));
    }
}

/// `draw` in a child of `ui` that paints and hits only inside `rect`.
fn clipped(ui: &mut egui::Ui, rect: Rect, draw: impl FnOnce(&mut egui::Ui)) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    draw(&mut child);
}

/// EffectCraft's Markers panel under Add Comp Marker / Add Layer Marker.
pub(crate) fn markers(host: &mut EcHost, ui: &mut egui::Ui, rect: Rect) {
    let ctx = ui.ctx().clone();
    let t = host.tokens();
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), BAR));
    let list = Rect::from_min_max(pos2(rect.min.x, bar.max.y + 1.0), rect.max);
    ui.painter().hline(bar.x_range(), bar.max.y, Stroke::new(1.0, t.separator));
    let has_comp = host.session().active_comp().is_some();
    let selected = host.session().state.selected_layers.clone();
    let w = ((rect.width() - 18.0) / 2.0).clamp(40.0, 150.0);
    let comp_r = Rect::from_min_size(pos2(rect.min.x + 6.0, rect.min.y + 4.0), vec2(w, BAR - 8.0));
    let layer_r = Rect::from_min_size(pos2(comp_r.max.x + 6.0, comp_r.min.y), comp_r.size());
    let mut run = None;
    if widgets::text_button(ui, comp_r, "Add Comp Marker", false, &t, egui::Id::new(("figcraft-ec-add-marker", "comp"))).clicked() && has_comp {
        run = Some(("comp.addMarker", json!({})));
    }
    host.auto_mut().add("figcraft.markers.addComp", comp_r, "Add Comp Marker");
    let layer_resp = widgets::text_button(ui, layer_r, "Add Layer Marker", false, &t, egui::Id::new(("figcraft-ec-add-marker", "layer")));
    host.auto_mut().add("figcraft.markers.addLayer", layer_r, "Add Layer Marker");
    if layer_resp.clicked() {
        if selected.is_empty() {
            host.ui_mut().status = "Select the layers to mark in the Timeline".into();
        } else {
            let layers: Vec<u64> = selected.iter().map(|l| l.0).collect();
            run = Some(("layer.addMarker", json!({"layers": layers})));
        }
    }
    if let Some((id, params)) = run
        && let Err(e) = host.invoke(&ctx, id, params)
    {
        host.ui_mut().status = e;
    }
    if list.height() > 4.0 {
        misc::markers(host, ui, list);
    }
}

/// EffectCraft's Motion Library panel, its session set up to read Elias's library first.
pub(crate) fn library(host: &mut EcHost, ui: &mut egui::Ui, rect: Rect) {
    let dir = if cfg!(test) { None } else { motion_library::user_settings_dir() };
    motion_library::hosted_setup(host.session_mut(), dir);
    // The plug's requests repaint this context.
    if let Some(b) = BRIDGE.get().and_then(Option::as_ref)
        && let Ok(mut c) = b.ctx.lock()
    {
        *c = Some(ui.ctx().clone());
    }
    motion_library::show(host, ui, rect);
}

/// The Motion Library's own commands (`motionLibrary.*`, `ml.saveTextPreset`) on the Timeline's
/// host, as EffectCraft's menus run them with its app (`EcHost::invoke`). `None` for other ids.
pub(crate) fn intercept(host: &mut EcHost, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !(id.starts_with("motionLibrary.") || id == "ml.saveTextPreset") {
        return None;
    }
    motion_library::hosted_command(host, ctx, id, params)
}

// ------------------------------------------------------------------------------------------
// EffectCraft's Marker dialog

/// The Marker dialog asked for, and the project of the Timeline it edits.
#[derive(Clone, Debug)]
pub(crate) struct MarkerShown {
    pub(crate) path: PathBuf,
    pub(crate) draft: MarkerDraft,
}

/// The dialog shown (one at a time; `TimelineHost::request_dialog` has no egui context to keep
/// it in).
static MARKER: Mutex<Option<MarkerShown>> = Mutex::new(None);

pub(crate) fn marker_shown() -> Option<MarkerShown> {
    MARKER.lock().ok().and_then(|m| m.clone())
}

fn set_marker(m: Option<MarkerShown>) {
    if let Ok(mut g) = MARKER.lock() {
        *g = m;
    }
}

/// A dialog EffectCraft's panels ask the Timeline's host for (`EcHost::request_dialog`): the
/// Composition / Layer Marker dialog for a marker (a double-click on it in the Timeline or the
/// Markers tab), from the session alone; it shows from the next frame on ([`frame`]). The others
/// aren't hosted here.
pub(crate) fn request_dialog(host: &mut EcHost, request: DialogRequest) -> Result<Value, String> {
    match request {
        DialogRequest::Marker(m) => {
            if let Some(why) = host.read_only() {
                return Err(why.to_string());
            }
            let draft = markers_ui::draft_for(host.session_mut(), m)?;
            set_marker(Some(MarkerShown { path: host.path().to_path_buf(), draft }));
            Ok(json!({"dialog": "marker"}))
        }
        _ => Err("This dialog is not available here".into()),
    }
}

/// Once a frame, after the panels drew: the plug's requests, and the Marker dialog with its OK,
/// Delete and Cancel.
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context) {
    serve(app, ctx);
    let Some(mut shown) = marker_shown() else { return };
    let Some(p) = app.effectcraft.panel.as_mut().filter(|p| p.host.path() == shown.path) else {
        // The Timeline it was opened for has gone.
        set_marker(None);
        return;
    };
    let t = Tokens::for_kind(ThemeKind::Dark);
    let prefs = p.host.session().prefs.clone();
    let mut auto = Registry::default();
    let mut a = markers_ui::DialogAnswer::default();
    let title = shown.draft.title.clone();
    let frame = egui::Frame::window(&ctx.style_of(ctx.theme())).fill(t.panel_bg).inner_margin(egui::Margin::same(18));
    let modal = egui::Modal::new(egui::Id::new("figcraft-ec-marker-dialog")).frame(frame).show(ctx, |ui| {
        ui.set_max_width(520.0);
        ui.label(egui::RichText::new(&title).strong().size(14.0));
        ui.add_space(6.0);
        a = markers_ui::draw_dialog(ui, &mut shown.draft, &prefs, &mut auto, &t);
    });
    a.cancel |= modal.should_close();
    let a = markers_ui::dialog_keys(ctx, a);
    let run = if a.ok {
        Some(("markers.set", markers_ui::draft_params(&shown.draft)))
    } else if a.delete {
        shown.draft.target.map(|m| ("markers.delete", m.params()))
    } else {
        None
    };
    if let Some((id, params)) = run
        && let Err(e) = p.host.invoke(ctx, id, params)
    {
        app.toast(e);
    }
    set_marker(if a.ok || a.delete || a.cancel { None } else { Some(shown) });
}

// ------------------------------------------------------------------------------------------
// The Motion Library plug's way in

/// One `engine.execute` the plug sent, and where its answer goes.
struct Job {
    command: String,
    params: Value,
    reply: Sender<Result<Value, String>>,
}

/// A local port speaking the part of EffectCraft's control protocol the Motion Library's plug
/// uses (`engine.execute`), answered by the open Timeline's host each frame ([`serve`]).
struct Bridge {
    port: u16,
    jobs: Mutex<VecDeque<Job>>,
    ctx: Mutex<Option<egui::Context>>,
}

static BRIDGE: OnceLock<Option<Arc<Bridge>>> = OnceLock::new();

/// The port the Motion Library's plug reaches the open Timeline's session through
/// (`TimelineHost::library_port`); opened the first time it's asked (127.0.0.1 only).
pub(crate) fn bridge_port() -> Option<u16> {
    BRIDGE.get_or_init(start).as_ref().map(|b| b.port)
}

fn start() -> Option<Arc<Bridge>> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let bridge = Arc::new(Bridge { port, jobs: Mutex::new(VecDeque::new()), ctx: Mutex::new(None) });
    let b = bridge.clone();
    std::thread::Builder::new()
        .name("figcraft-ec-library-plug".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let b = b.clone();
                let _ = std::thread::Builder::new().name("figcraft-ec-library-client".into()).spawn(move || client(&b, stream));
            }
        })
        .ok()?;
    Some(bridge)
}

/// One plug connection: a JSON request per line, a JSON answer per line
/// (`{id, ok, result | error}`, as EffectCraft's control channel answers).
fn client(b: &Bridge, stream: TcpStream) {
    let Ok(mut out) = stream.try_clone() else { return };
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Vec::new();
        match reader.by_ref().take(MAX_LINE as u64 + 1).read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) if line.len() > MAX_LINE => return,
            Ok(_) => {}
        }
        let req = serde_json::from_slice::<Value>(&line).unwrap_or(Value::Null);
        let answer = match answer(b, &req) {
            Ok(result) => json!({"id": req.get("id").cloned().unwrap_or(Value::Null), "ok": true, "result": result}),
            Err(e) => json!({"id": req.get("id").cloned().unwrap_or(Value::Null), "ok": false, "error": e}),
        };
        if writeln!(out, "{answer}").is_err() {
            return;
        }
    }
}

/// Queue `engine.execute {command, params}` for the next frame and wait for its answer.
fn answer(b: &Bridge, req: &Value) -> Result<Value, String> {
    if req.get("method").and_then(Value::as_str) != Some("engine.execute") {
        return Err("FigCraft's EffectCraft Timeline answers engine.execute only".into());
    }
    let p = req.get("params").ok_or("missing `params`")?;
    let command = p.get("command").and_then(Value::as_str).filter(|c| !c.is_empty()).ok_or("missing `command`")?.to_string();
    let params = p.get("params").cloned().unwrap_or_else(|| json!({}));
    let (tx, rx) = channel();
    b.jobs.lock().map_err(|_| "FigCraft is busy")?.push_back(Job { command, params, reply: tx });
    if let Ok(c) = b.ctx.lock()
        && let Some(c) = c.as_ref()
    {
        c.request_repaint();
    }
    rx.recv_timeout(PLUG_WAIT).map_err(|_| "FigCraft did not answer in time".to_string())?
}

/// Run the plug's queued requests on the open Timeline's host (its undo, read-only guard, mirror
/// and card refresh, as any edit there).
pub(crate) fn serve(app: &mut FigApp, ctx: &egui::Context) {
    let Some(b) = BRIDGE.get().and_then(Option::as_ref) else { return };
    let jobs: Vec<Job> = match b.jobs.lock() {
        Ok(mut q) => {
            let n = q.len().min(PER_FRAME);
            q.drain(..n).collect()
        }
        Err(_) => return,
    };
    let more = b.jobs.lock().is_ok_and(|q| !q.is_empty());
    for job in jobs {
        let r = match app.effectcraft.panel.as_mut() {
            Some(p) => run_job(&mut p.host, ctx, &job.command, job.params),
            None => Err("Open an EffectCraft card's Timeline in FigCraft".into()),
        };
        let _ = job.reply.send(r);
    }
    if more {
        ctx.request_repaint();
    }
}

/// One plug request on `host`. Never panics into FigCraft.
pub(crate) fn run_job(host: &mut EcHost, ctx: &egui::Context, command: &str, params: Value) -> Result<Value, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.invoke(ctx, command, params)))
        .unwrap_or_else(|_| Err(format!("EffectCraft: {command} failed (an internal error)")))
}

#[cfg(test)]
#[path = "effectcraft_tabs_tests.rs"]
mod tests;

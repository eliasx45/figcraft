//! `canvas.annotate.*`: Pen / Comment / Layer review marks on a MAIN board card, as EffectCraft's
//! PN1 `annotate.*` does on its Composition viewer (the model: same tools, same mark shape).
//!
//! A host (Ghostex's Pen · Comment strip, Claude, the control channel) arms a tool with
//! `canvas.annotate.start {tool}`, or P / C on the canvas; the desktop app then takes the pointer
//! on cards: Pen drags a smooth stroke (EffectCraft PN2's ink), Comment clicks a point, both in the
//! card's comp pixels; a box pops from the mark (HyperFrames-style: Enter adds, Esc cancels). Layer
//! (or ⌃⌥-click at any time) picks the layer under the pointer, through precomps. Adding runs
//! `canvas.annotate.add`, which has EffectCraft measure the mark (`figcraft.mark`: frame-snapped
//! time, timecode, box, the layers under it, backgrounds skipped) and adds the card's MAIN time.
//!
//! Marks are session state, never the document or its undo history: they wait here until the host
//! takes them (`canvas.annotate.list {since}`) and removes them (`canvas.annotate.remove`).

use figcraft_doc::{EcComp, NodeId};
use serde_json::{Value, json};

use super::{host_call, text_param, timecode};
use crate::cmd::CommandSpec;
use crate::params::{get, num, opt_id, string};
use crate::{EngineError, Result, Session, bad, spec};

/// The most marks kept (oldest dropped).
pub const MAX_MARKS: usize = 64;
/// The longest stroke kept (points; longer ones are thinned evenly).
pub const MAX_POINTS: usize = 2000;
/// Longest note (characters).
const MAX_NOTE: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Comment,
    Layer,
}

impl Tool {
    pub fn from_name(s: &str) -> Option<Tool> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pen" | "draw" | "stroke" => Some(Tool::Pen),
            "comment" | "pin" | "point" => Some(Tool::Comment),
            "layer" | "pick" => Some(Tool::Layer),
            _ => None,
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Tool::Pen => "pen",
            Tool::Comment => "comment",
            Tool::Layer => "layer",
        }
    }
}

/// One mark: EffectCraft's `annotate.add` mark plus where it sits on the board.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    pub id: u64,
    pub tool: Tool,
    pub card: NodeId,
    pub comp: String,
    pub comp_id: u64,
    pub project: String,
    /// Comp seconds of the frame it was drawn on, that frame and its timecode.
    pub time: f64,
    pub frame: i64,
    pub timecode: String,
    /// The same moment in MAIN.
    pub main_time: f64,
    pub main_timecode: String,
    /// The stroke (Pen) or the point (Comment, Layer), comp pixels.
    pub points: Vec<[f64; 2]>,
    pub bbox: [f64; 4],
    pub comp_size: [u32; 2],
    pub note: String,
    /// The layers under it, most likely first (`{id, name, comp, compName, top, path}`).
    pub layers: Vec<Value>,
    pub source: String,
    pub copy_ref: String,
}

#[derive(Clone, Debug, Default)]
pub struct Marks {
    pub armed: Option<Tool>,
    /// Bumped on every arm: the app pauses playback when it changes.
    pub arm_serial: u64,
    pub list: Vec<Mark>,
    next_id: u64,
    /// Bumped on every change.
    pub revision: u64,
    /// Armed for one mark only (`canvas.annotate.start {once: true}`, the host's ⌃⌥-click Layer
    /// pick): the mark it adds disarms the tool.
    pub once: bool,
}

impl Marks {
    pub fn arm(&mut self, tool: Option<Tool>) {
        if tool.is_some() {
            self.arm_serial = self.arm_serial.wrapping_add(1);
        }
        self.armed = tool;
        self.once = false;
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn push(&mut self, mut m: Mark) -> u64 {
        self.next_id = self.next_id.saturating_add(1);
        m.id = self.next_id;
        self.list.push(m);
        if self.list.len() > MAX_MARKS {
            let extra = self.list.len() - MAX_MARKS;
            self.list.drain(..extra);
        }
        self.revision = self.revision.wrapping_add(1);
        self.next_id
    }

    pub fn remove(&mut self, ids: &[u64]) -> usize {
        let before = self.list.len();
        self.list.retain(|m| !ids.contains(&m.id));
        let n = before - self.list.len();
        if n > 0 {
            self.revision = self.revision.wrapping_add(1);
        }
        n
    }

    pub fn get(&self, id: u64) -> Option<&Mark> {
        self.list.iter().find(|m| m.id == id)
    }

    pub fn last_id(&self) -> u64 {
        self.list.last().map_or(0, |m| m.id)
    }
}

/// `[x, y, w, h]` round `points`.
pub fn bbox(points: &[[f64; 2]]) -> [f64; 4] {
    let Some(first) = points.first() else { return [0.0; 4] };
    let mut b = [first[0], first[1], first[0], first[1]];
    for p in points {
        b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
    }
    [b[0], b[1], b[2] - b[0], b[3] - b[1]]
}

/// At most [`MAX_POINTS`] finite points, thinned evenly, each within ±1e6.
pub fn tidy(points: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    let pts: Vec<[f64; 2]> =
        points.into_iter().filter(|p| p[0].is_finite() && p[1].is_finite()).map(|p| [p[0].clamp(-1e6, 1e6), p[1].clamp(-1e6, 1e6)]).collect();
    if pts.len() <= MAX_POINTS {
        return pts;
    }
    let step = pts.len() as f64 / MAX_POINTS as f64;
    (0..MAX_POINTS).filter_map(|i| pts.get((i as f64 * step) as usize).copied()).collect()
}

fn points_p(cmd: &str, p: &Value) -> Result<Vec<[f64; 2]>> {
    let pair = |v: &Value| -> Option<[f64; 2]> {
        match v {
            Value::Array(a) => Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?]),
            Value::Object(_) => Some([v.get("x")?.as_f64()?, v.get("y")?.as_f64()?]),
            _ => None,
        }
    };
    if let Some(a) = get(p, "points").and_then(Value::as_array) {
        return a.iter().take(100_000).map(|v| pair(v).ok_or_else(|| bad(cmd, "each of `points` is [x, y] (comp pixels)"))).collect();
    }
    if let Some(v) = get(p, "point") {
        return Ok(vec![pair(v).ok_or_else(|| bad(cmd, "`point` is [x, y] (comp pixels)"))?]);
    }
    if let (Some(x), Some(y)) = (num(p, "x"), num(p, "y")) {
        return Ok(vec![[x, y]]);
    }
    Err(bad(cmd, "give `points` ([[x, y], …] in the card's comp pixels), or `point` / `x`, `y`"))
}

fn state_json(s: &Session) -> Value {
    let m = &s.ecboard.marks;
    json!({"armed": m.armed.map(Tool::id), "revision": m.revision, "count": m.list.len(), "last": m.last_id(), "armSerial": m.arm_serial})
}

pub fn mark_json(m: &Mark) -> Value {
    json!({
        "id": m.id, "tool": m.tool.id(), "card": m.card, "comp": m.comp_id, "compName": m.comp, "project": m.project,
        "time": m.time, "frame": m.frame, "timecode": m.timecode, "mainTime": m.main_time, "mainTimecode": m.main_timecode,
        "points": m.points, "bbox": m.bbox, "compSize": m.comp_size, "note": m.note, "layers": m.layers, "source": m.source,
        "copyRef": m.copy_ref,
    })
}

fn start(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "canvas.annotate.start";
    let tool = match string(p, "tool") {
        None => Tool::Pen,
        Some(t) => Tool::from_name(&t).ok_or_else(|| bad(cmd, format!("`tool` is pen, comment or layer, got {t:?}")))?,
    };
    s.ecboard.marks.arm(Some(tool));
    s.ecboard.marks.once = p.get("once").and_then(Value::as_bool).unwrap_or(false);
    // Playback pauses while marking (the frame marked is the one shown).
    s.effectcraft.pause();
    s.ecboard.push("armed", json!({"armed": tool.id()}));
    Ok(state_json(s))
}

fn stop(s: &mut Session, _: &Value) -> Result<Value> {
    if s.ecboard.marks.armed.is_some() {
        s.ecboard.marks.arm(None);
        s.ecboard.push("armed", json!({"armed": null}));
    }
    Ok(state_json(s))
}

/// What a layer pick names, as a mark layer.
fn layer_ref(l: &Value) -> String {
    let path: Vec<&str> = l.get("path").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    if path.len() > 2 { path[1..].join(" › ") } else { l.get("name").and_then(Value::as_str).unwrap_or("").to_string() }
}

/// The comp second a mark on card `c` (layer `id`) goes at: `time` / `frame`, else the card's
/// playhead, else its first frame. Snapped to the frame.
fn time_p(cmd: &str, s: &Session, id: NodeId, c: &EcComp, p: &Value) -> Result<f64> {
    let t = match (num(p, "time"), num(p, "frame")) {
        (Some(t), _) => t,
        (None, Some(f)) => f / c.fps,
        (None, None) if s.effectcraft.node == Some(id) => s.effectcraft.time(),
        (None, None) => c.start,
    };
    if !t.is_finite() {
        return Err(bad(cmd, "`time` / `frame` must be finite"));
    }
    Ok((c.frame_of(t.clamp(-1e7, 1e7)) as f64) / c.fps)
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "canvas.annotate.add";
    let id = super::card_param(cmd, s, p)?;
    let c = s.doc()?.node(id).and_then(|n| n.effectcraft.as_deref()).cloned().ok_or_else(|| bad(cmd, "not an EffectCraft card"))?;
    let points = tidy(points_p(cmd, p)?);
    if points.is_empty() {
        return Err(bad(cmd, "the mark has no points"));
    }
    let tool = match string(p, "tool") {
        Some(t) => Tool::from_name(&t).ok_or_else(|| bad(cmd, format!("`tool` is pen, comment or layer, got {t:?}")))?,
        None if points.len() == 1 => Tool::Comment,
        None => Tool::Pen,
    };
    let points: Vec<[f64; 2]> = if tool == Tool::Pen { points } else { points.into_iter().take(1).collect() };
    let t = time_p(cmd, s, id, &c, p)?;
    let note = text_param(p, "note", MAX_NOTE);
    let source = string(p, "source").map(|x| x.chars().take(64).collect()).unwrap_or_else(|| "canvas".into());
    let main_time = c.offset + t / c.rate.max(1e-9);
    let fps = super::board_of(s, &c.project).map(|b| super::main_fps(&b)).unwrap_or(c.fps);
    let main_tc = timecode(main_time, fps);
    // EffectCraft measures it (when there is one to ask).
    let comp_ref = if c.comp_id > 0 { json!(c.comp_id) } else { json!(c.comp) };
    let (mut timecode_s, mut frame, mut layers, mut copy, mut size) =
        (timecode(t, c.fps), c.frame_of(t), Vec::new(), String::new(), [c.width, c.height]);
    if s.ecboard.host.is_some() {
        match tool {
            Tool::Pen | Tool::Comment => {
                let q = json!({"comp": comp_ref, "time": t, "tool": tool.id(), "points": points, "note": note});
                let r = host_call(s, cmd, &c.project, "figcraft.mark", &q)?;
                let m = &r["mark"];
                if let Some(tc) = m["timecode"].as_str() {
                    timecode_s = tc.to_string();
                }
                frame = m["frame"].as_i64().unwrap_or(frame);
                layers = m["layers"].as_array().cloned().unwrap_or_default();
                copy = m["copyRef"].as_str().unwrap_or_default().to_string();
                if let Some(a) = m["compSize"].as_array() {
                    size = [a.first().and_then(Value::as_u64).unwrap_or(1) as u32, a.get(1).and_then(Value::as_u64).unwrap_or(1) as u32];
                }
            }
            Tool::Layer => {
                let at = points.first().copied().unwrap_or([0.0, 0.0]);
                let r = host_call(s, cmd, &c.project, "figcraft.layerAt", &json!({"comp": comp_ref, "time": t, "x": at[0], "y": at[1]}))?;
                if r["layer"].is_object() {
                    layers.push(r["layer"].clone());
                }
            }
        }
    }
    if copy.is_empty() {
        copy = match tool {
            Tool::Layer => {
                let what = layers.first().map(layer_ref).filter(|x| !x.is_empty()).unwrap_or_else(|| "(no layer there)".into());
                format!("◆ {} › {what} · {timecode_s} (f{frame})", c.comp)
            }
            Tool::Pen => {
                let [x, y, w, h] = bbox(&points);
                format!("✏️ {} · {timecode_s} (f{frame}) · drawn around x {x:.0}–{:.0}, y {y:.0}–{:.0}", c.comp, x + w, y + h)
            }
            Tool::Comment => {
                let q = points.first().copied().unwrap_or([0.0, 0.0]);
                format!("💬 {} · {timecode_s} (f{frame}) · comment at ({:.0}, {:.0})", c.comp, q[0], q[1])
            }
        };
        if tool != Tool::Layer {
            copy.push_str(&if note.is_empty() { " · (no note)".to_string() } else { format!(" — \"{note}\"") });
        }
    }
    copy.push_str(&format!(" · MAIN {main_tc}"));
    let mark = Mark {
        id: 0,
        tool,
        card: id,
        comp: c.comp.clone(),
        comp_id: c.comp_id,
        project: c.project.clone(),
        time: t,
        frame,
        timecode: timecode_s,
        main_time,
        main_timecode: main_tc,
        bbox: bbox(&points),
        points,
        comp_size: size,
        note,
        layers,
        source,
        copy_ref: copy,
    };
    let mid = s.ecboard.marks.push(mark);
    let m = s.ecboard.marks.get(mid).cloned().ok_or_else(|| bad(cmd, "the mark was dropped"))?;
    s.ecboard.push("mark", json!({"id": mid, "tool": m.tool.id(), "card": id, "comp": m.comp}));
    // A one-shot arm (⌃⌥-click in Ghostex) ends with its mark. Night run 29 Oct: `once` was
    // ignored, so Layer stayed armed and every later click on a card picked a layer.
    if s.ecboard.marks.once && s.ecboard.marks.armed == Some(tool) {
        s.ecboard.marks.arm(None);
        s.ecboard.push("armed", json!({"armed": null}));
    }
    Ok(json!({"mark": mark_json(&m), "copyRef": m.copy_ref, "state": state_json(s)}))
}

fn list(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "canvas.annotate.list";
    let since = num(p, "since").filter(|v| v.is_finite() && *v >= 0.0).map_or(0, |v| v.min(u64::MAX as f64) as u64);
    let card = opt_id(cmd, p, "card")?;
    let marks: Vec<Value> = s.ecboard.marks.list.iter().filter(|m| m.id > since && card.is_none_or(|c| m.card == c)).map(mark_json).collect();
    let st = state_json(s);
    Ok(json!({"armed": st["armed"], "revision": st["revision"], "last": st["last"], "marks": marks}))
}

fn get_cmd(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "canvas.annotate.get";
    let id = num(p, "id").filter(|v| v.is_finite() && *v >= 0.0).ok_or_else(|| bad(cmd, "give `id` (canvas.annotate.list lists them)"))? as u64;
    s.ecboard.marks.get(id).map(mark_json).ok_or_else(|| bad(cmd, format!("no mark {id}")))
}

fn ids_p(p: &Value) -> Vec<u64> {
    let mut ids: Vec<u64> = get(p, "ids").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    ids.extend(get(p, "id").and_then(Value::as_u64));
    ids
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = ids_p(p);
    if ids.is_empty() {
        return Err(bad("canvas.annotate.remove", "give `id` or `ids`"));
    }
    let removed = s.ecboard.marks.remove(&ids);
    if removed > 0 {
        s.ecboard.push("marksRemoved", json!({"ids": ids}));
    }
    Ok(json!({"removed": removed, "state": state_json(s)}))
}

fn clear(s: &mut Session, p: &Value) -> Result<Value> {
    let card = opt_id("canvas.annotate.clear", p, "card")?;
    let ids: Vec<u64> = s.ecboard.marks.list.iter().filter(|m| card.is_none_or(|c| m.card == c)).map(|m| m.id).collect();
    let removed = s.ecboard.marks.remove(&ids);
    if removed > 0 {
        s.ecboard.push("marksRemoved", json!({"ids": ids}));
    }
    Ok(json!({"removed": removed, "state": state_json(s)}))
}

fn state_cmd(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(state_json(s))
}

/// Whether a tool is armed (the canvas takes the pointer on cards).
pub fn armed(s: &Session) -> Option<Tool> {
    s.ecboard.marks.armed
}

/// Refuse when there's nothing to mark on (no card).
pub fn needs_card(s: &Session, cmd: &str) -> Result<()> {
    let doc = s.doc()?;
    if doc.nodes().any(|n| n.effectcraft.is_some()) {
        Ok(())
    } else {
        Err(EngineError::Disabled(cmd.into(), "no EffectCraft card in this file".into()))
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "canvas.annotate.start",
            "Pen / Comment on Cards",
            "{tool?: pen (default)|comment (alias pin)|layer}  arms review marks on the EffectCraft cards: Pen drags a stroke, Comment clicks a point (a box asks what should change: Enter adds, Esc cancels), Layer clicks a layer (⌃⌥-click does it any time). Pauses playback; again switches the tool → {armed, revision, count, last, armSerial}",
            false,
            start
        ),
        spec!(
            "canvas.annotate.stop",
            "Stop Pen / Comment",
            "{}  the canvas takes the pointer back (Esc on the canvas does the same) → {armed: null, …}",
            false,
            stop
        ),
        spec!(
            "canvas.annotate.add",
            "Add Review Mark",
            "{card? (default the selected card), time? (comp seconds) | frame? (default the card's playhead), tool?: pen|comment|layer (default comment for one point), points?: [[x, y], …] in the card's comp pixels, point?: [x, y], x?, y?, note?, source?}  what the canvas's Enter runs; EffectCraft measures it (the time snaps to the frame) → {mark: {id, tool, card, comp, compName, project, time, frame, timecode, mainTime, mainTimecode, points, bbox: [x, y, w, h], compSize, note, layers: [{id, name, comp, compName, top, path}], source, copyRef}, copyRef, state}",
            false,
            add
        ),
        spec!(
            "canvas.annotate.list",
            "List Review Marks",
            "{since?: mark id (only newer ones), card?}  the marks waiting for the host → {armed, revision, last, marks: [mark…]}",
            false,
            list
        ),
        spec!("canvas.annotate.get", "Get Review Mark", "{id} → mark", false, get_cmd),
        spec!("canvas.annotate.remove", "Remove Review Mark", "{id | ids}  the host took them → {removed, state}", false, remove),
        spec!("canvas.annotate.clear", "Clear Review Marks", "{card?}  every mark (on one card) → {removed, state}", false, clear),
        spec!("canvas.annotate.state", "Review Mark State", "{} → {armed, revision, count, last, armSerial}", false, state_cmd),
    ]
}

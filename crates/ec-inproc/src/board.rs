//! What FigCraft's MAIN board (`board.*`, `canvas.annotate.*` in `figcraft_engine::ecboard`)
//! asks of an EffectCraft project, answered in process through [`InProc::call`](crate::InProc::call):
//!
//! - `figcraft.board {comp?}`: the MAIN comp (the item named MAIN, else the longest comp), its
//!   chapters (MAIN's markers) and its scenes: every precomp layer showing in MAIN, looked through
//!   "part" comps (P1…P5: precomps at least half as long as MAIN), the rule Elias's
//!   `editing-spread` scout uses. Each scene carries its MAIN range and how its comp time maps onto
//!   MAIN (`offset`: MAIN seconds of comp second 0, `rate`: comp seconds per MAIN second), and the
//!   board.json beats next to the project (`storyboard/board.json`, else `board.json`).
//! - `figcraft.speech {comp?, start?, end?}`: MAIN's speech map as `[start, end, word, stressed]`.
//! - `figcraft.mark {comp, time, tool, points | point, note}`: EffectCraft's own `annotate.add`
//!   measures the mark (frame-snapped time, timecode, box, the layers under it through precomps,
//!   backgrounds skipped); the mark is taken out of this session again at once (FigCraft keeps
//!   the queue).
//! - `figcraft.layerAt {comp, time, x, y}`: the deepest layer drawn at a comp pixel (`layer.at
//!   {deep}`), in the shape of a mark's layer.
//! - `figcraft.comp {comp, layers?}`: a comp's size, length, the comps it is used in and its first
//!   layers (name, type, in, out, keys, visible, label).
//! - `figcraft.moves {comp, start?, end?}`: its keyframed moves with the ease graph each matches
//!   (`taste.check`).

use std::path::Path;

use effectcraft_engine::Session;
use effectcraft_engine::project::{ItemId, LayerSource, Project};
use serde_json::{Value, json};

/// Deepest precomp nesting walked for scenes.
const MAX_DEPTH: usize = 12;
/// The most scenes one board holds.
const MAX_SCENES: usize = 512;
/// The most chapters.
const MAX_CHAPTERS: usize = 128;
/// The most layers `figcraft.comp` lists.
const MAX_LAYERS: usize = 200;
/// The most words returned.
const MAX_WORDS: usize = 20_000;
/// Biggest board.json read (bytes).
const MAX_BOARD_JSON: u64 = 8 << 20;
/// A precomp at least this share of MAIN's length is a part comp: its scenes are looked inside.
const PART_SHARE: f64 = 0.5;

fn r3(v: f64) -> f64 {
    if v.is_finite() { (v * 1000.0).round() / 1000.0 } else { 0.0 }
}

fn name_of(project: &Project, id: ItemId) -> String {
    project.item(id).map(|i| i.name.clone()).unwrap_or_default()
}

/// The comp `p` names (`comp`: id or name), else MAIN: the comp named "MAIN" (any case), else the
/// longest comp.
pub fn main_comp(s: &Session, p: &Value) -> Result<ItemId, String> {
    if let Some(v @ (Value::Number(_) | Value::String(_))) = p.get("comp") {
        return s.resolve_comp(Some(v)).map_err(|e| e.to_string());
    }
    let project = &s.project;
    let named = project.comps().find(|(id, _)| name_of(project, **id).trim().eq_ignore_ascii_case("main")).map(|(id, _)| *id);
    if let Some(id) = named {
        return Ok(id);
    }
    project
        .comps()
        .max_by(|a, b| a.1.duration.seconds().partial_cmp(&b.1.duration.seconds()).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(id, _)| *id)
        .ok_or_else(|| "the project has no compositions".into())
}

/// How a comp's time maps onto MAIN: comp second `t` shows at MAIN second `offset + t / rate`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Map {
    pub offset: f64,
    pub rate: f64,
}

impl Map {
    pub const MAIN: Map = Map { offset: 0.0, rate: 1.0 };
    /// The map of a precomp placed in this comp at `start` (comp seconds) with `stretch` (percent).
    pub fn child(self, start: f64, stretch: f64) -> Option<Map> {
        if !(stretch.is_finite() && stretch.abs() > 1e-6 && self.rate.abs() > 1e-9) {
            return None;
        }
        Some(Map { offset: self.offset + start / self.rate, rate: self.rate * 100.0 / stretch })
    }
    pub fn main(self, t: f64) -> f64 {
        self.offset + t / self.rate
    }
    pub fn comp(self, main: f64) -> f64 {
        (main - self.offset) * self.rate
    }
}

/// One scene of MAIN.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub comp: ItemId,
    pub layer: String,
    /// The comp that holds its layer.
    pub parent: String,
    pub chain: Vec<String>,
    pub main: (f64, f64),
    pub map: Map,
}

/// MAIN's scenes, in MAIN time order.
pub fn scenes(project: &Project, main: ItemId) -> Vec<Scene> {
    let Some(root) = project.comp(main) else { return Vec::new() };
    let dur = root.duration.seconds();
    let mut out = Vec::new();
    walk(project, main, Map::MAIN, (0.0, dur), vec![name_of(project, main)], dur, 0, &mut out);
    out.sort_by(|a, b| a.main.0.partial_cmp(&b.main.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

#[allow(clippy::too_many_arguments)]
fn walk(project: &Project, comp: ItemId, map: Map, (lo, hi): (f64, f64), chain: Vec<String>, root_dur: f64, depth: usize, out: &mut Vec<Scene>) {
    if depth > MAX_DEPTH || out.len() >= MAX_SCENES {
        return;
    }
    let Some(c) = project.comp(comp) else { return };
    for l in c.layers.iter() {
        let LayerSource::Comp { item } = &l.source else { continue };
        let item = *item;
        if item == comp || !l.switches.video || l.switches.guide {
            continue;
        }
        let Some(sub) = project.comp(item) else { continue };
        let Some(child) = map.child(l.start_time.seconds(), l.stretch) else { continue };
        // The part of the layer that shows, in MAIN seconds.
        let (a, b) = (map.main(l.in_point.seconds()), map.main(l.out_point.seconds()));
        let (a, b) = (a.min(b).max(lo), a.max(b).min(hi));
        if b <= a + 1e-6 || b.is_nan() || a.is_nan() {
            continue;
        }
        let sub_dur = sub.duration.seconds();
        let name = name_of(project, item);
        if root_dur > 0.0 && sub_dur >= PART_SHARE * root_dur {
            let mut next = chain.clone();
            next.push(name);
            walk(project, item, child, (a, b), next, root_dur, depth + 1, out);
            continue;
        }
        if out.len() >= MAX_SCENES {
            return;
        }
        out.push(Scene {
            comp: item,
            layer: l.name.clone(),
            parent: chain.last().cloned().unwrap_or_default(),
            chain: chain.clone(),
            main: (a, b),
            map: child,
        });
    }
}

/// MAIN's markers as chapters `(start, end, name)`, in time order (the comment, else the chapter
/// field); one "MAIN" chapter without markers.
pub fn chapters(project: &Project, main: ItemId) -> Vec<(f64, f64, String)> {
    let Some(c) = project.comp(main) else { return Vec::new() };
    let dur = c.duration.seconds();
    let mut m: Vec<(f64, String)> = c
        .markers
        .iter()
        .map(|k| {
            let n = if k.comment.trim().is_empty() { k.chapter.trim() } else { k.comment.trim() };
            (k.time.seconds(), n.lines().next().unwrap_or("").to_string())
        })
        .filter(|(t, _)| t.is_finite())
        .collect();
    m.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    m.truncate(MAX_CHAPTERS);
    if m.is_empty() {
        return vec![(0.0, dur, name_of(project, main))];
    }
    let mut out = Vec::with_capacity(m.len());
    for (i, (t, n)) in m.iter().enumerate() {
        let end = m.get(i + 1).map_or(dur, |x| x.0).max(*t);
        let n = if n.is_empty() { format!("Chapter {}", i + 1) } else { n.clone() };
        out.push((*t, end, n));
    }
    out
}

/// The board.json beats next to `project_path`: `[{time, end, text, stress, shot, section}]`.
pub fn beats(project_path: &Path) -> Vec<Value> {
    let Some(dir) = project_path.parent() else { return Vec::new() };
    for p in [dir.join("storyboard").join("board.json"), dir.join("board.json")] {
        let Ok(meta) = std::fs::metadata(&p) else { continue };
        if !meta.is_file() || meta.len() > MAX_BOARD_JSON {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
        return beats_of(&v);
    }
    Vec::new()
}

/// The beats of a board.json value (a list of sections, or `{sections}` / `{cards}`).
pub fn beats_of(v: &Value) -> Vec<Value> {
    let sections = match v {
        Value::Array(a) => a.as_slice(),
        Value::Object(o) => o.get("sections").or_else(|| o.get("cards")).and_then(Value::as_array).map_or(&[][..], |a| a.as_slice()),
        _ => &[][..],
    };
    let num = |b: &Value, k: &str| b.get(k).and_then(|x| x.as_f64().or_else(|| x.as_str()?.trim().parse().ok())).filter(|x: &f64| x.is_finite());
    let mut out = Vec::new();
    for sec in sections.iter().take(1000) {
        let name = sec.get("name").and_then(Value::as_str).unwrap_or_default();
        for b in sec.get("beats").and_then(Value::as_array).into_iter().flatten().take(1000) {
            let Some(start) = num(b, "start") else { continue };
            let end = num(b, "end").unwrap_or(start);
            let s = |k: &str| b.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
            out.push(json!({"time": r3(start), "end": r3(end.max(start)), "text": s("text"), "stress": s("stress"), "shot": b.get("shot").cloned().unwrap_or(Value::Null), "section": name}));
            if out.len() >= 10_000 {
                return out;
            }
        }
    }
    out.sort_by(|a, b| a["time"].as_f64().partial_cmp(&b["time"].as_f64()).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// `figcraft.board`.
pub fn board(s: &Session, project_path: &Path, p: &Value) -> Result<Value, String> {
    let main = main_comp(s, p)?;
    let project = &s.project;
    let c = project.comp(main).ok_or("no such composition")?;
    let fps = c.frame_rate.as_f64();
    let list: Vec<Value> = scenes(project, main)
        .iter()
        .filter_map(|sc| {
            let sub = project.comp(sc.comp)?;
            let (ca, cb) = (sc.map.comp(sc.main.0), sc.map.comp(sc.main.1));
            Some(json!({
                "comp": sc.comp.0, "compName": name_of(project, sc.comp), "layer": sc.layer, "parent": sc.parent, "chain": sc.chain,
                "mainStart": r3(sc.main.0), "mainEnd": r3(sc.main.1),
                "compStart": r3(ca.min(cb)), "compEnd": r3(ca.max(cb)),
                "offset": sc.map.offset, "rate": sc.map.rate,
                "width": sub.width, "height": sub.height, "fps": sub.frame_rate.as_f64(), "duration": r3(sub.duration.seconds()),
            }))
        })
        .collect();
    let chapters: Vec<Value> = chapters(project, main).into_iter().map(|(a, b, n)| json!({"time": r3(a), "end": r3(b), "name": n})).collect();
    Ok(json!({
        "project": project_path.to_string_lossy(),
        "main": {"id": main.0, "name": name_of(project, main), "width": c.width, "height": c.height, "fps": fps, "duration": r3(c.duration.seconds())},
        "chapters": chapters,
        "scenes": list,
        "beats": beats(project_path),
    }))
}

/// `figcraft.speech`: MAIN's words between `start` and `end` (MAIN seconds) as
/// `[start, end, word, stressed]`; `{words: [], missing: true}` when MAIN has no speech map.
pub fn speech(s: &mut Session, p: &Value) -> Result<Value, String> {
    let main = main_comp(s, p)?;
    let mut q = json!({"comp": main.0});
    for k in ["start", "end"] {
        if let Some(v) = p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()) {
            q[k] = json!(v);
        }
    }
    let r = match s.execute("speech.get", q) {
        Ok(r) => r,
        Err(e) => return Ok(json!({"comp": main.0, "words": [], "missing": true, "why": e.to_string()})),
    };
    let words: Vec<Value> = r["list"]
        .as_array()
        .into_iter()
        .flatten()
        .take(MAX_WORDS)
        .filter_map(|w| {
            let stressed = w["syllables"]
                .as_array()
                .is_some_and(|s| s.iter().any(|x| x["stress"].as_bool().unwrap_or(false) || x["stress"].as_f64().is_some_and(|v| v > 0.0)));
            Some(json!([w["start"].as_f64()?, w["end"].as_f64()?, w["word"].as_str()?, stressed]))
        })
        .collect();
    Ok(json!({"comp": main.0, "words": words}))
}

/// `figcraft.mark`: EffectCraft's `annotate.add` on this session, then the mark leaves it again.
pub fn mark(s: &mut Session, p: &Value) -> Result<Value, String> {
    let mut q = p.clone();
    let Some(o) = q.as_object_mut() else { return Err("parameters must be an object".into()) };
    o.insert("source".into(), json!("figcraft"));
    let r = s.execute("annotate.add", q).map_err(|e| e.to_string())?;
    let mark = r.get("mark").cloned().ok_or("no mark")?;
    if let Some(id) = mark["id"].as_u64() {
        let _ = s.execute("annotate.remove", json!({"id": id}));
    }
    Ok(json!({"mark": mark}))
}

/// `figcraft.layerAt`: the deepest layer drawn at comp pixel `x`, `y` at comp second `time`.
pub fn layer_at(s: &mut Session, p: &Value) -> Result<Value, String> {
    let cid = s.resolve_comp(p.get("comp")).map_err(|e| e.to_string())?;
    let f = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
    let (Some(x), Some(y)) = (f("x"), f("y")) else { return Err("`x` and `y` are comp pixels".into()) };
    let hit =
        s.execute("layer.at", json!({"comp": cid.0, "time": f("time").unwrap_or(0.0), "x": x, "y": y, "deep": true})).map_err(|e| e.to_string())?;
    let Some(top) = hit["layer"]["id"].as_u64() else { return Ok(json!({"layer": null})) };
    let levels = hit["path"].as_array().cloned().unwrap_or_default();
    let deepest = levels.iter().rev().find(|l| l["layer"]["id"].as_u64().is_some());
    let comp_name = hit["compName"].as_str().unwrap_or_default().to_string();
    let (layer, comp, holder) = match deepest {
        Some(l) => (l["layer"].clone(), l["comp"].as_u64().unwrap_or(cid.0), l["compName"].as_str().unwrap_or_default().to_string()),
        None => (hit["layer"].clone(), cid.0, comp_name.clone()),
    };
    let mut path = vec![comp_name];
    path.extend(levels.iter().filter_map(|l| l["layer"]["name"].as_str().map(str::to_string)));
    Ok(json!({"layer": {"id": layer["id"], "name": layer["name"], "comp": comp, "compName": holder, "top": top, "path": path}}))
}

/// `figcraft.comp`: a comp's facts and its first `layers` (default 8) layers.
pub fn comp(s: &Session, p: &Value) -> Result<Value, String> {
    let cid = s.resolve_comp(p.get("comp")).map_err(|e| e.to_string())?;
    let project = &s.project;
    let c = project.comp(cid).ok_or("no such composition")?;
    let n = p.get("layers").and_then(Value::as_u64).unwrap_or(8).min(MAX_LAYERS as u64) as usize;
    let parents: Vec<String> = project
        .comps()
        .filter(|(_, pc)| pc.layers.iter().any(|l| matches!(&l.source, LayerSource::Comp { item } if *item == cid)))
        .map(|(id, _)| name_of(project, *id))
        .take(16)
        .collect();
    let layers: Vec<Value> = c
        .layers
        .iter()
        .take(n)
        .map(|l| {
            let mut keys = 0usize;
            l.props.walk("", &mut |_, pr| keys += pr.keys.len());
            json!({"name": l.name, "type": l.source.type_name(), "in": r3(l.in_point.seconds()), "out": r3(l.out_point.seconds()),
                "keys": keys, "visible": l.switches.video, "label": l.label.name()})
        })
        .collect();
    Ok(json!({
        "id": cid.0, "name": name_of(project, cid), "parents": parents, "width": c.width, "height": c.height,
        "fps": c.frame_rate.as_f64(), "duration": r3(c.duration.seconds()), "layerCount": c.layers.len(),
        "layers": layers, "moreLayers": c.layers.len().saturating_sub(n),
    }))
}

/// `figcraft.moves`: the comp's keyframed moves between `start` and `end` (comp seconds), each
/// with the ease graph it matches (`curve`) or comes nearest (`nearest`).
pub fn moves(s: &mut Session, p: &Value) -> Result<Value, String> {
    let cid = s.resolve_comp(p.get("comp")).map_err(|e| e.to_string())?;
    let mut q = json!({"comp": cid.0});
    for k in ["start", "end"] {
        if let Some(v) = p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()) {
            q[k] = json!(v);
        }
    }
    let r = s.execute("taste.check", q).map_err(|e| e.to_string())?;
    let moves: Vec<Value> = r["moves"]
        .as_array()
        .into_iter()
        .flatten()
        .take(256)
        .map(|m| {
            json!({"layer": m["layerName"], "property": m["property"], "path": m["path"], "from": m["from"], "to": m["to"], "frames": m["frames"],
                "moveType": m["moveType"], "curve": m["curve"], "nearest": m["nearest"]["name"], "verdict": m["verdict"]})
        })
        .collect();
    Ok(json!({"comp": cid.0, "moves": moves}))
}

#[cfg(test)]
#[path = "board_tests.rs"]
mod tests;

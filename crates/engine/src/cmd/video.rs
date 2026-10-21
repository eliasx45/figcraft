//! Video files on the canvas: place a video card (a dropped `.mp4` / `.mov` / `.m4v` makes one),
//! read and set the stretch it plays, and play it. The desktop app decodes the pictures on the
//! Apple media engine (`figcraft-video`); the file stays where it is (linked, not embedded).

use figcraft_doc::{Document, Node, NodeId, NodeKind, VideoClip};
use serde_json::{Value, json};

use super::CommandSpec;
use crate::params::*;
use crate::{EngineError, Result, Session, bad, spec};

/// Default card width (layer units).
const CARD_WIDTH: f64 = 480.0;
/// Gap between cards placed in a row (layer units).
const GAP: f64 = 24.0;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "video.place",
            "Place video",
            "{path (a video file: .mp4, .mov, .m4v), x?, y? (default the centre of the view), cardWidth? (default 480), name?, start?, end? (media seconds), sound? (default: the file has sound), loop? (default true), parent?, duration?, width?, height?, fps? (only where the file can't be read here)}  a card that plays the file (linked, not embedded) → {id, ...card}",
            true,
            place
        ),
        spec!(
            "video.info",
            "Video info",
            "{ids?}  the video cards among `ids` (default the selection, else every one on the page) → {cards: [{id, name, path, file, duration, width, height, fps, start, end, sound, loop, x, y, cardWidth, cardHeight}]}",
            false,
            info
        ),
        spec!("video.set", "Set video", "{ids?, path?, start?, end? (media seconds), sound?, loop?}  → {cards} as video.info", true, set),
        spec!(
            "video.play",
            "Play video",
            "{id?, time? (media seconds)}  plays the card with its sound (looping over its stretch) → the transport",
            false,
            play
        ),
        spec!("video.pause", "Pause video", "{}  → the transport", false, |s, _| {
            s.video.pause();
            Ok(transport_json(s))
        }),
        spec!("video.togglePlay", "Play/pause video", "{id?}  → the transport", false, |s, p| {
            let cmd = "video.togglePlay";
            let (id, _) = target(cmd, s, p)?;
            if s.video.playing() && s.video.node == Some(id) {
                s.video.pause();
                Ok(transport_json(s))
            } else {
                play(s, &json!({"id": id}))
            }
        }),
        spec!("video.seek", "Seek video", "{id?, time? (media seconds) | x? (layer units from the card's left edge)}  → the transport", false, seek),
        spec!("video.transport", "Video transport", "{}  → {id, playing, time, frame, playAll}", false, |s, _| Ok(transport_json(s))),
        spec!(
            "video.playAll",
            "Play all visible videos",
            "{on? (bool; default: toggle)}  every video card on screen plays, muted (the one started with play keeps its sound) → the transport",
            false,
            |s, p| {
                s.video.play_all = boolean(p, "on").unwrap_or(!s.video.play_all);
                Ok(transport_json(s))
            }
        ),
    ]
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// The video cards a command acts on: `ids`/`id`, else the selection, else the playhead's card,
/// else (with `all_if_none`) every card on the page.
fn targets_of(cmd: &str, s: &Session, p: &Value, all_if_none: bool) -> Result<Vec<NodeId>> {
    let named = get(p, "ids").is_some_and(|v| !v.is_null()) || get(p, "id").is_some_and(|v| !v.is_null());
    let doc = s.doc()?;
    let has = |i: &NodeId| doc.node(*i).is_some_and(|n| n.video.is_some());
    let ids: Vec<NodeId> = if named { req_targets(cmd, s, p)? } else { s.selection() };
    let mut out: Vec<NodeId> = ids.iter().copied().filter(has).collect();
    if named && out.len() < ids.len() {
        return Err(bad(cmd, "not a video card (place one with video.place)"));
    }
    if out.is_empty() && !named {
        if let Some(n) = s.video.node.filter(has) {
            out.push(n);
        } else if all_if_none {
            let page = s.page()?;
            out = doc.nodes().filter(|n| n.video.is_some() && doc.page_of(n.id) == Some(page)).map(|n| n.id).collect();
        }
    }
    if out.is_empty() {
        return Err(EngineError::Disabled(cmd.to_string(), "no video card selected (pass `ids`)".into()));
    }
    Ok(out)
}

/// The one card a transport command acts on.
fn target(cmd: &str, s: &Session, p: &Value) -> Result<(NodeId, VideoClip)> {
    let id = match opt_id(cmd, p, "id")? {
        Some(i) => i,
        None => *targets_of(cmd, s, p, false)?.first().ok_or_else(|| bad(cmd, "no video card"))?,
    };
    let v = s.doc()?.node(id).and_then(|n| n.video.as_deref()).cloned().ok_or_else(|| bad(cmd, "not a video card"))?;
    Ok((id, v))
}

pub fn card_json(doc: &Document, id: NodeId) -> Value {
    let Some(n) = doc.node(id) else { return Value::Null };
    let Some(v) = n.video.as_deref() else { return Value::Null };
    let b = doc.world_bounds(id);
    json!({"id": id, "name": n.name, "path": v.path, "file": v.file_name(), "duration": round6(v.duration), "width": v.width, "height": v.height,
        "fps": v.fps, "start": round6(v.start), "end": round6(v.end), "sound": v.sound, "loop": v.looping,
        "x": b.map(|b| b.x0), "y": b.map(|b| b.y0), "cardWidth": n.width, "cardHeight": n.height})
}

/// Set `v` from `p` (start/end within the file, sound, loop).
fn apply(cmd: &str, v: &mut VideoClip, p: &Value) -> Result<()> {
    let (start, end) = (num(p, "start").unwrap_or(v.start), num(p, "end").unwrap_or(v.end));
    if !(start.is_finite() && end.is_finite()) || start < 0.0 || end <= start {
        return Err(bad(cmd, "`start` must be ≥ 0 and before `end`"));
    }
    if start >= v.duration {
        return Err(bad(cmd, format!("`start` is past the end of the video ({:.2} s)", v.duration)));
    }
    (v.start, v.end) = (start, end.min(v.duration));
    if let Some(b) = boolean(p, "sound") {
        v.sound = b;
    }
    if let Some(b) = boolean(p, "loop") {
        v.looping = b;
    }
    v.sanitize();
    Ok(())
}

/// What the file holds: read here (macOS), else from the params.
fn probe(cmd: &str, path: &str, p: &Value) -> Result<(f64, u32, u32, f64, bool)> {
    match figcraft_video::probe(std::path::Path::new(path)) {
        Ok(i) => Ok((i.duration, i.width, i.height, i.fps, i.sound)),
        Err(e) => match (num(p, "duration"), num(p, "width"), num(p, "height")) {
            (Some(d), Some(w), Some(h)) if d.is_finite() && d > 0.0 && w >= 1.0 && h >= 1.0 => {
                Ok((d, w.min(16_384.0) as u32, h.min(16_384.0) as u32, num(p, "fps").unwrap_or(30.0), true))
            }
            _ => Err(bad(cmd, e.to_string())),
        },
    }
}

/// [`video.place`](specs); `file.import` of a video file runs it too.
pub(crate) fn place(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "video.place";
    let path = req_string(cmd, p, "path")?;
    let (duration, width, height, fps, has_sound) = probe(cmd, &path, p)?;
    let mut v = VideoClip::new(path, duration, width, height, fps);
    v.sound = has_sound;
    apply(cmd, &mut v, p)?;
    let page = s.page()?;
    let parent = opt_id(cmd, p, "parent")?.unwrap_or(page);
    let ds = s.ds_mut()?;
    let (vx, vy) = (ds.view.cx, ds.view.cy);
    let doc = ds.doc_mut();
    let w = num(p, "cardWidth").filter(|w| w.is_finite() && *w > 0.0).unwrap_or(CARD_WIDTH).clamp(16.0, 100_000.0);
    let h = (w * f64::from(v.height) / f64::from(v.width.max(1))).clamp(9.0, 100_000.0);
    let x = num(p, "x").filter(|x| x.is_finite()).unwrap_or(vx - w / 2.0);
    let y = num(p, "y").filter(|y| y.is_finite()).unwrap_or(vy - h / 2.0);
    let nid = doc.alloc_id();
    let stem = std::path::Path::new(&v.path).file_stem().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let name = string(p, "name").unwrap_or(stem);
    let mut node = Node::new(nid, NodeKind::Frame, name, w, h);
    node.transform = figcraft_geom::Affine::translate((x, y));
    node.fills = vec![figcraft_color::Paint::solid(figcraft_color::Color::from_rgb8_const(0x0C, 0x0C, 0x0E))];
    node.clip = true;
    node.video = Some(Box::new(v));
    doc.insert(node, parent, None)?;
    ds.selection = vec![nid];
    let mut out = card_json(&ds.doc, nid);
    if let Some(o) = out.as_object_mut() {
        o.insert("gap".into(), json!(GAP));
    }
    Ok(out)
}

fn info(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = targets_of("video.info", s, p, true)?;
    let doc = s.doc()?;
    Ok(json!({"cards": ids.iter().map(|i| card_json(doc, *i)).collect::<Vec<_>>()}))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "video.set";
    let ids = targets_of(cmd, s, p, false)?;
    // A new file is read before anything changes.
    let relinked = match string(p, "path") {
        Some(path) => Some((probe(cmd, &path, p)?, path)),
        None => None,
    };
    let doc = s.doc_mut()?;
    for id in &ids {
        let n = doc.get_mut(*id)?;
        let Some(mut v) = n.video.take() else { continue };
        if let Some(((d, w, h, fps, sound), path)) = &relinked {
            let mut fresh = VideoClip::new(path.clone(), *d, *w, *h, *fps);
            fresh.sound = *sound;
            fresh.looping = v.looping;
            v = Box::new(fresh);
        }
        let r = apply(cmd, &mut v, p);
        n.video = Some(v);
        r?;
    }
    let doc = s.doc()?;
    Ok(json!({"cards": ids.iter().map(|i| card_json(doc, *i)).collect::<Vec<_>>()}))
}

fn play(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "video.play";
    let (id, v) = target(cmd, s, p)?;
    let from = match num(p, "time") {
        Some(t) if t.is_finite() => Some(t),
        Some(_) => return Err(bad(cmd, "`time` must be finite")),
        None => (s.video.node == Some(id)).then(|| s.video.time()),
    };
    s.video.play(id, &v, from);
    Ok(transport_json(s))
}

fn seek(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "video.seek";
    let (id, v) = target(cmd, s, p)?;
    let w = s.doc()?.node(id).map_or(0.0, |n| n.width);
    let t = match (num(p, "time"), num(p, "x")) {
        (Some(t), _) if t.is_finite() => t,
        (None, Some(x)) if x.is_finite() && w > 0.0 => v.start + (x / w).clamp(0.0, 1.0) * v.span(),
        _ => return Err(bad(cmd, "pass `time` or `x`")),
    };
    s.video.seek(id, &v, t);
    Ok(transport_json(s))
}

pub fn transport_json(s: &Session) -> Value {
    let t = &s.video;
    let card = t.node.and_then(|n| s.doc().ok()?.node(n)?.video.as_deref().cloned());
    let time = t.time();
    json!({"id": t.node, "playing": t.playing(), "time": round6(time), "frame": card.as_ref().map(|v| v.frame_of(time)), "playAll": t.play_all})
}

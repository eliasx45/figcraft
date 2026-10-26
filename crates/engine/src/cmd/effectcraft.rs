//! EffectCraft comps on the canvas: place a live comp card, read and set its stretch, play it,
//! its poster frame, and the requests the desktop app carries out over its link to EffectCraft
//! (explode the precomps into cards, open the comp in EffectCraft).

use figcraft_doc::{Document, EcComp, Node, NodeId, NodeKind};
use serde_json::{Value, json};

use super::CommandSpec;
use crate::params::*;
use crate::{EngineError, Result, Session, bad, spec};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "effectcraft.place",
            "Place EffectCraft comp",
            "{comp (name), compId?, project? (.ecproj path), start? (comp seconds, default 0), end? (default start + 5), fps? (default 29.97), width?, height? (the comp's pixel size, default 1920×1080), offset? (timeline seconds of comp time 0), rate? (comp seconds per timeline second), sound? (default true), name?, explodedFrom? (the card it came out of: explode closes it again), into? (a frame to fill; default the selected empty frame), parent?, x?, y? (default the centre of the view), cardWidth? (default 480)}  a card that shows and plays the comp from the running EffectCraft → {id, ...card}",
            true,
            place
        ),
        spec!(
            "effectcraft.info",
            "EffectCraft comp info",
            "{ids?}  the EffectCraft cards among `ids` (default the selection, else every card on the page) → {cards: [{id, name, comp, compId, project, start, end, fps, offset, rate, startFrame, endFrame, width, height, x, y, cardWidth, cardHeight, sound, poster}]}",
            false,
            info
        ),
        spec!(
            "effectcraft.set",
            "Set EffectCraft comp",
            "{ids?, comp?, compId?, start?, end? (comp seconds), fps?, offset?, rate?, sound? (bool)}  → {cards} as effectcraft.info",
            true,
            set
        ),
        spec!(
            "effectcraft.setPoster",
            "Set EffectCraft poster",
            "{id, data (base64 PNG/JPEG)}  the frame the card shows when EffectCraft isn't running (the desktop app sets it from the first frame it gets) → {id, image}",
            false,
            set_poster
        ),
        spec!(
            "effectcraft.play",
            "Play EffectCraft comp",
            "{id?, time? (comp seconds) | frame?}  plays the card (the selected or playhead's one) in a loop → transport",
            false,
            play
        ),
        spec!("effectcraft.pause", "Pause EffectCraft comp", "{}  → transport", false, |s, _| {
            s.effectcraft.pause();
            Ok(transport_json(s))
        }),
        spec!("effectcraft.togglePlay", "Play/pause EffectCraft comp", "{id?}  → transport", false, |s, p| {
            if s.effectcraft.playing() {
                s.effectcraft.pause();
                return Ok(transport_json(s));
            }
            play(s, p)
        }),
        spec!(
            "effectcraft.seek",
            "Move EffectCraft playhead",
            "{id?, time? (comp seconds) | frame? (comp frame) | x? (layer units from the card's left edge)}  → transport",
            false,
            seek
        ),
        spec!("effectcraft.transport", "EffectCraft transport", "{} → {id, playing, time, frame}", false, |s, _| Ok(transport_json(s))),
        spec!(
            "effectcraft.explode",
            "Explode EffectCraft precomps",
            "{id?, open? (bool; default: toggle)}  open: the desktop app asks EffectCraft for the comps nested in the card at the frame it shows and places a card for each (at most 16), to the right of it, all on the same playhead (one undo step) → {requested}; close (when the card is exploded): removes the cards that explode placed, and theirs → {collapsed}",
            true,
            explode
        ),
        spec!(
            "effectcraft.reveal",
            "Open in EffectCraft",
            "{id?, time? (comp seconds; default the card's playhead)}  the desktop app opens the card's comp in EffectCraft at that time → {requested}",
            false,
            |s, p| {
                let cmd = "effectcraft.reveal";
                let (id, c) = target(cmd, s, p)?;
                let t = match num(p, "time") {
                    Some(t) if t.is_finite() => t,
                    Some(_) => return Err(bad(cmd, "`time` must be finite")),
                    None if s.effectcraft.node == Some(id) => s.effectcraft.time(),
                    None => c.start,
                };
                s.request_ui("effectcraft.reveal", json!({"id": id, "comp": c.comp, "compId": c.comp_id, "time": t}));
                Ok(json!({"requested": true, "id": id, "time": round6(t)}))
            }
        ),
        spec!(
            "effectcraft.newComp",
            "New EffectCraft comp",
            "{project? (.ecproj path; default the selected card's project, else a new project saved next to this file), name?, width?, height? (pixels, 1..30000; both skip the dialog), frameRate? (0.01..999), duration? (seconds, up to 3 h), background? (#hex), x?, y?, cardWidth?}  the desktop app (fork, effectcraft-inproc) makes the comp with EffectCraft's own comp.new in the project's in-process session (unsaved until ⌘S in its Timeline; a new project is saved at once), places it as a card (right of the selected card, else at x/y or the view's centre) and opens its Timeline. Without width and height it first shows EffectCraft's New Composition dialog (a dialog request: never from the control channel) → {requested, project, dialog}",
            false,
            new_comp
        ),
    ]
}

/// The longest new comp (seconds) and its largest side (pixels), as EffectCraft allows.
const NEW_COMP_MAX_SECONDS: f64 = 3.0 * 3600.0;
const NEW_COMP_MAX_SIDE: f64 = 30_000.0;

/// Ask the desktop app for a new EffectCraft comp (see the spec). The engine knows no EffectCraft:
/// it checks the parameters and says where the comp goes; the app makes it.
fn new_comp(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "effectcraft.newComp";
    let project = match get(p, "project") {
        None => None,
        Some(Value::String(x)) if !x.trim().is_empty() && !x.contains('\0') && x.len() <= 4096 => Some(x.clone()),
        Some(_) => return Err(bad(cmd, "`project` must be the path of an .ecproj")),
    };
    // The selected card: its project by default, and the new card goes to its right.
    let from = targets_of(cmd, s, &json!({}), false).ok().and_then(|ids| ids.first().copied());
    let from_project = from.and_then(|id| s.doc().ok()?.node(id)?.effectcraft.as_deref().map(|c| c.project.clone())).filter(|x| !x.trim().is_empty());
    let project = project.or(from_project);
    let mut comp = serde_json::Map::new();
    if let Some(name) = get(p, "name") {
        match name.as_str().map(str::trim) {
            Some(n) if !n.is_empty() && n.chars().count() <= 255 => comp.insert("name".into(), json!(n)),
            _ => return Err(bad(cmd, "`name`: 1 to 255 characters")),
        };
    }
    for (k, lo, hi) in
        [("width", 1.0, NEW_COMP_MAX_SIDE), ("height", 1.0, NEW_COMP_MAX_SIDE), ("frameRate", 0.01, 999.0), ("duration", 0.001, NEW_COMP_MAX_SECONDS)]
    {
        let Some(v) = get(p, k) else { continue };
        match f64_of(v) {
            Some(x) if (lo..=hi).contains(&x) => {
                comp.insert(k.into(), if matches!(k, "width" | "height") { json!(x.round() as u64) } else { json!(x) })
            }
            _ => return Err(bad(cmd, format!("`{k}`: {lo}..{hi}"))),
        };
    }
    if let Some(bg) = get(p, "background") {
        if !bg.is_string() || color_of(bg).is_none() {
            return Err(bad(cmd, "`background` must be a colour (#rrggbb)"));
        }
        comp.insert("background".into(), bg.clone());
    }
    let mut place = serde_json::Map::new();
    for k in ["x", "y", "cardWidth"] {
        let Some(v) = get(p, k) else { continue };
        match f64_of(v) {
            Some(x) if x.abs() <= 1e9 && (k != "cardWidth" || x >= 1.0) => place.insert(k.into(), json!(x)),
            _ => return Err(bad(cmd, format!("`{k}` must be a finite number"))),
        };
    }
    let next_to = s.ds()?.path.clone().filter(|x| !x.trim().is_empty());
    if project.is_none() && next_to.is_none() {
        return Err(EngineError::Disabled(cmd.to_string(), "save this file first: the new EffectCraft project is saved next to it".into()));
    }
    let sized = comp.contains_key("width") && comp.contains_key("height");
    let data = json!({"project": project, "nextTo": next_to, "comp": comp, "place": place, "from": from});
    s.request_ui(if sized { "effectcraft.newComp" } else { "dialog.effectcraftNewComp" }, data);
    Ok(json!({"requested": true, "project": project, "dialog": !sized}))
}

/// Default card width (layer units).
const CARD_WIDTH: f64 = 480.0;
/// The `plugin_data` key of a card that explode placed: the id of the card it came out of.
pub const EXPLODED_FROM: &str = "effectcraft.explodedFrom";

/// The cards explode placed out of card `id`, and the ones placed out of those.
pub fn exploded_from(doc: &Document, id: NodeId) -> Vec<NodeId> {
    let mut out: Vec<NodeId> = Vec::new();
    let mut todo = vec![(id, 0usize)];
    while let Some((from, depth)) = todo.pop() {
        if depth > 8 || out.len() > 4096 {
            break;
        }
        let key = from.to_string();
        for n in doc.nodes().filter(|n| n.effectcraft.is_some() && n.plugin_data.get(EXPLODED_FROM) == Some(&key)) {
            if !out.contains(&n.id) && n.id != id {
                out.push(n.id);
                todo.push((n.id, depth + 1));
            }
        }
    }
    out
}

fn explode(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "effectcraft.explode";
    let (id, _) = target(cmd, s, p)?;
    let open = boolean(p, "open");
    let kids = exploded_from(s.doc()?, id);
    if open != Some(true) && !kids.is_empty() {
        let doc = s.doc_mut()?;
        for k in &kids {
            if doc.node(*k).is_some() {
                doc.remove(*k)?;
            }
        }
        return Ok(json!({"collapsed": kids.len(), "id": id}));
    }
    if open == Some(false) {
        return Ok(json!({"collapsed": 0, "id": id}));
    }
    s.request_ui("effectcraft.explode", json!({"id": id}));
    Ok(json!({"requested": true, "id": id}))
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

/// The comp settings of `p` on `c`.
fn apply(cmd: &str, c: &mut EcComp, p: &Value) -> Result<()> {
    if let Some(name) = string(p, "comp") {
        if name.trim().is_empty() {
            return Err(bad(cmd, "`comp` is empty"));
        }
        c.comp = name;
    }
    if let Some(v) = num(p, "compId") {
        if !(v.is_finite() && v >= 0.0) {
            return Err(bad(cmd, "`compId` must be a non-negative id"));
        }
        c.comp_id = v.min(u64::MAX as f64) as u64;
    }
    if let Some(v) = string(p, "project") {
        c.project = v;
    }
    let finite = |k: &str| -> Result<Option<f64>> {
        match num(p, k) {
            Some(v) if !v.is_finite() => Err(bad(cmd, format!("`{k}` must be finite"))),
            v => Ok(v),
        }
    };
    if let Some(f) = finite("fps")? {
        if !(0.01..=1000.0).contains(&f) {
            return Err(bad(cmd, "`fps`: 0.01..1000"));
        }
        c.fps = f;
    }
    let (start, end) = (finite("start")?.unwrap_or(c.start), finite("end")?.unwrap_or(c.end));
    if end <= start {
        return Err(bad(cmd, format!("the range must end after it starts (start {start}, end {end})")));
    }
    if end - start > figcraft_doc::effectcraft::MAX_SPAN {
        return Err(bad(cmd, format!("at most {} s of comp time", figcraft_doc::effectcraft::MAX_SPAN)));
    }
    (c.start, c.end) = (start, end);
    if let Some(o) = finite("offset")? {
        c.offset = o;
    }
    if let Some(r) = finite("rate")? {
        if r <= 0.0 {
            return Err(bad(cmd, "`rate` must be positive"));
        }
        c.rate = r;
    }
    for (k, slot) in [("width", &mut c.width), ("height", &mut c.height)] {
        if let Some(v) = finite(k)? {
            if !(1.0..=32_768.0).contains(&v) {
                return Err(bad(cmd, format!("`{k}`: 1..32768 pixels")));
            }
            *slot = v.round() as u32;
        }
    }
    if let Some(b) = boolean(p, "sound") {
        c.sound = b;
    }
    c.sanitize();
    Ok(())
}

/// `m:ss` of `t` seconds.
fn clock(t: f64) -> String {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let s = t.round() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// An empty frame to fill: no children, not itself a card or a strip.
fn fillable(doc: &Document, id: NodeId) -> bool {
    doc.node(id).is_some_and(|n| matches!(n.kind, NodeKind::Frame) && n.children.is_empty() && n.audio.is_none() && n.effectcraft.is_none())
}

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "effectcraft.place";
    let comp = req_string(cmd, p, "comp")?;
    // Five seconds from `start` unless `end` says otherwise.
    let start = num(p, "start").filter(|v| v.is_finite()).unwrap_or(0.0);
    let mut c = EcComp::new(comp, start, start + 5.0, 29.97);
    apply(cmd, &mut c, p)?;
    let page = s.page()?;
    let exploded = opt_id(cmd, p, "explodedFrom")?;
    let sel = s.selection();
    let into = match opt_id(cmd, p, "into")? {
        Some(f) => {
            if !fillable(s.doc()?, f) {
                return Err(bad(cmd, "`into` must be an empty frame"));
            }
            Some(f)
        }
        None if opt_id(cmd, p, "parent")?.is_none() && num(p, "x").is_none() && sel.len() == 1 => {
            let doc = s.doc()?;
            sel.first().copied().filter(|f| fillable(doc, *f))
        }
        None => None,
    };
    let ds = s.ds_mut()?;
    let (vx, vy) = (ds.view.cx, ds.view.cy);
    let doc = ds.doc_mut();
    let aspect = c.height as f64 / c.width.max(1) as f64;
    let (parent, x, y, w, h) = match into {
        Some(f) => {
            let n = doc.node(f).ok_or_else(|| bad(cmd, "`into`: no such layer"))?;
            (f, 0.0, 0.0, n.width, n.height)
        }
        None => {
            let parent = opt_id(cmd, p, "parent")?.unwrap_or(page);
            let w = num(p, "cardWidth").filter(|w| w.is_finite() && *w > 0.0).unwrap_or(CARD_WIDTH).clamp(16.0, 100_000.0);
            let h = (w * aspect).clamp(9.0, 100_000.0);
            let x = num(p, "x").filter(|v| v.is_finite()).unwrap_or(vx - w / 2.0);
            let y = num(p, "y").filter(|v| v.is_finite()).unwrap_or(vy - h / 2.0);
            (parent, x, y, w, h)
        }
    };
    let nid = doc.alloc_id();
    let name = string(p, "name").unwrap_or_else(|| format!("{} · {}–{}", c.comp, clock(c.start), clock(c.end)));
    let mut node = Node::new(nid, NodeKind::Frame, name, w, h);
    node.transform = figcraft_geom::Affine::translate((x, y));
    node.fills = vec![figcraft_color::Paint::solid(figcraft_color::Color::from_rgb8_const(0x0C, 0x0C, 0x0E))];
    node.clip = true;
    node.effectcraft = Some(Box::new(c));
    if let Some(from) = exploded {
        node.plugin_data.insert(EXPLODED_FROM.into(), from.to_string());
    }
    doc.insert(node, parent, None)?;
    ds.selection = vec![nid];
    let mut out = card_json(&ds.doc, nid);
    if let Some(o) = out.as_object_mut() {
        o.insert("into".into(), json!(into));
    }
    Ok(out)
}

/// The cards `p` names: `ids`/`id`, else the selected cards, else the playhead's card, else every
/// card on the page (`all_if_none`).
fn targets_of(cmd: &str, s: &Session, p: &Value, all_if_none: bool) -> Result<Vec<NodeId>> {
    let named = get(p, "ids").is_some_and(|v| !v.is_null()) || get(p, "id").is_some_and(|v| !v.is_null());
    let doc = s.doc()?;
    let has = |i: &NodeId| doc.node(*i).is_some_and(|n| n.effectcraft.is_some());
    let ids: Vec<NodeId> = if named { req_targets(cmd, s, p)? } else { s.selection() };
    let mut out: Vec<NodeId> = ids.iter().copied().filter(has).collect();
    if named && out.len() < ids.len() {
        return Err(bad(cmd, "not an EffectCraft card (place one with effectcraft.place)"));
    }
    if out.is_empty() && !named {
        if let Some(n) = s.effectcraft.node.filter(has) {
            out.push(n);
        } else if all_if_none {
            let page = s.page()?;
            out = doc.nodes().filter(|n| n.effectcraft.is_some() && doc.page_of(n.id) == Some(page)).map(|n| n.id).collect();
        }
    }
    if out.is_empty() {
        return Err(EngineError::Disabled(cmd.to_string(), "no EffectCraft card selected (pass `ids`)".into()));
    }
    Ok(out)
}

/// The one card a transport command acts on: `id`, else the selected card, else the playhead's.
fn target(cmd: &str, s: &Session, p: &Value) -> Result<(NodeId, EcComp)> {
    let id = match opt_id(cmd, p, "id")? {
        Some(i) => i,
        None => *targets_of(cmd, s, p, false)?.first().ok_or_else(|| bad(cmd, "no EffectCraft card"))?,
    };
    let c = s.doc()?.node(id).and_then(|n| n.effectcraft.as_deref()).cloned().ok_or_else(|| bad(cmd, "not an EffectCraft card"))?;
    Ok((id, c))
}

pub fn card_json(doc: &Document, id: NodeId) -> Value {
    let Some(n) = doc.node(id) else { return Value::Null };
    let Some(c) = n.effectcraft.as_deref() else { return Value::Null };
    let b = doc.world_bounds(id);
    let poster = n.fills.iter().any(|f| matches!(f.kind, figcraft_color::PaintKind::Image(_)));
    json!({"id": id, "name": n.name, "comp": c.comp, "compId": c.comp_id, "project": c.project, "start": round6(c.start), "end": round6(c.end),
        "fps": c.fps, "offset": round6(c.offset), "rate": c.rate, "startFrame": c.frames().start(), "endFrame": c.frames().end(),
        "width": c.width, "height": c.height, "x": b.map(|b| b.x0), "y": b.map(|b| b.y0), "cardWidth": n.width, "cardHeight": n.height,
        "sound": c.sound, "poster": poster})
}

fn info(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = targets_of("effectcraft.info", s, p, true)?;
    let doc = s.doc()?;
    Ok(json!({"cards": ids.iter().map(|i| card_json(doc, *i)).collect::<Vec<_>>()}))
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "effectcraft.set";
    let ids = targets_of(cmd, s, p, false)?;
    let doc = s.doc_mut()?;
    for id in &ids {
        let n = doc.get_mut(*id)?;
        let Some(mut c) = n.effectcraft.take() else { continue };
        let r = apply(cmd, &mut c, p);
        n.effectcraft = Some(c);
        r?;
    }
    let doc = s.doc()?;
    Ok(json!({"cards": ids.iter().map(|i| card_json(doc, *i)).collect::<Vec<_>>()}))
}

fn set_poster(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "effectcraft.setPoster";
    let id = req_id(cmd, p, "id")?;
    let data = req_string(cmd, p, "data")?;
    use base64::Engine as _;
    let data = data.split_once(',').map(|(_, b)| b.to_string()).unwrap_or(data);
    let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|e| bad(cmd, format!("`data`: {e}")))?;
    if bytes.len() > 16 << 20 {
        return Err(bad(cmd, "`data`: at most 16 MB"));
    }
    let doc = s.doc_mut()?;
    if !doc.node(id).is_some_and(|n| n.effectcraft.is_some()) {
        return Err(bad(cmd, "not an EffectCraft card"));
    }
    let (hash, _, _) = figcraft_format::add_image(doc, bytes).map_err(|e| bad(cmd, e.to_string()))?;
    let n = doc.get_mut(id)?;
    n.fills = vec![
        figcraft_color::Paint::solid(figcraft_color::Color::from_rgb8_const(0x0C, 0x0C, 0x0E)),
        figcraft_color::Paint::image(hash.clone(), figcraft_color::ImageScaleMode::Fit),
    ];
    Ok(json!({"id": id, "image": hash}))
}

/// A comp time from `p`: `time` (seconds) or `frame` (comp frame).
fn time_of(cmd: &str, c: &EcComp, p: &Value) -> Result<Option<f64>> {
    let t = match (num(p, "time"), num(p, "frame")) {
        (Some(t), _) => t,
        (None, Some(f)) => f / c.fps,
        (None, None) => return Ok(None),
    };
    if !t.is_finite() {
        return Err(bad(cmd, "`time` / `frame` must be finite"));
    }
    Ok(Some(t))
}

fn play(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "effectcraft.play";
    let (id, c) = target(cmd, s, p)?;
    let from = match time_of(cmd, &c, p)? {
        Some(t) => Some(t),
        None => (s.effectcraft.node == Some(id)).then(|| s.effectcraft.time()),
    };
    s.effectcraft.play(id, &c, from);
    Ok(transport_json(s))
}

fn seek(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "effectcraft.seek";
    let (id, c) = target(cmd, s, p)?;
    let w = s.doc()?.node(id).map_or(0.0, |n| n.width);
    let t = match (time_of(cmd, &c, p)?, num(p, "x")) {
        (Some(t), _) => t,
        (None, Some(x)) if x.is_finite() && w > 0.0 => c.start + (x / w).clamp(0.0, 1.0) * c.span(),
        _ => return Err(bad(cmd, "pass `time`, `frame` or `x`")),
    };
    s.effectcraft.seek(id, &c, t);
    Ok(transport_json(s))
}

pub fn transport_json(s: &Session) -> Value {
    let t = &s.effectcraft;
    let card = t.node.and_then(|n| s.doc().ok()?.node(n)?.effectcraft.as_deref().cloned());
    let time = t.time();
    json!({"id": t.node, "playing": t.playing(), "time": round6(time), "frame": card.as_ref().map(|c| c.frame_of(time))})
}

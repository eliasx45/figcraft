//! Audio on the canvas: place a sound as a waveform strip, read and set its range and ruler,
//! markers, and the transport (play, pause, seek).

use figcraft_doc::{AudioClip, AudioMarker, Document, Node, NodeId, NodeKind, Peaks};
use serde_json::{Value, json};
use std::sync::Arc;

use super::CommandSpec;
use crate::params::*;
use crate::{EngineError, Result, Session, bad, spec};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "audio.place",
            "Place audio",
            "{path | data (base64) + name, x?, y?, parent?, name?, width? (default 100 px per second shown, 160..4000), height? (default 96), start?, end? (media seconds shown; default the whole sound), fps? (ruler frame rate, default 29.97), offset? (timeline seconds of the sound's first sample: ruler and marker frames count from it), color?, markers? ([{time | frame, label?, color?}])}  WAV, MP3, AAC, M4A/MP4/MOV, FLAC, Ogg, AIFF; a frame with the sound as a waveform strip → {id, audio, file, duration, sampleRate, channels, codec, start, end, fps, width, height}",
            true,
            place
        ),
        spec!(
            "audio.info",
            "Audio info",
            "{ids?}  the audio layers among `ids` (default the selection, else every audio layer on the page) → {clips: [{id, name, file, duration, sampleRate, channels, start, end, fps, offset, startFrame, endFrame, x, y, width, height, markers: [{index, time, frame, label, color?}]}]}",
            false,
            info
        ),
        spec!(
            "audio.set",
            "Set audio clip",
            "{ids?, start? | startFrame?, end? | endFrame? (the range shown, media seconds or ruler frames), fps?, offset?, color?, markerColor?, ruler? (bool), pxPerSecond? (resize the layer to that many units per second shown)}  → {clips} as audio.info",
            true,
            set
        ),
        spec!(
            "audio.addMarker",
            "Add audio marker",
            "{ids?, time? (media seconds) | frame? (ruler frame), label?, color?}  default: the playhead → {markers: [{id, index, time, frame}]}",
            true,
            add_marker
        ),
        spec!(
            "audio.setMarkers",
            "Set audio markers",
            "{ids?, markers: [{time | frame, label?, color?}]}  replaces every marker → {count}",
            true,
            set_markers
        ),
        spec!("audio.removeMarker", "Remove audio marker", "{ids?, index? | label? | all? (bool)}  → {removed}", true, remove_marker),
        spec!(
            "audio.importSpeechMap",
            "Import speech map",
            "{ids?, path | map ([{word, start, end, syllables: [{t, vowel, stress}]}], or {words: [...]}), timeBase? (timeline (default: times are timeline seconds, `offset` is taken off) | media), stress? (1: primary stress only (default), 2: secondary too, 0: every syllable), at? (onset (default) | vowel | peak: one ruler frame before the vowel), replace? (default true), color?}  a marker per stressed syllable, labelled with its word → {count}",
            true,
            import_speech_map
        ),
        spec!(
            "audio.play",
            "Play audio",
            "{id?, time? | frame?}  plays the clip (the selected or playhead's one) from the playhead or the given time → transport",
            false,
            play
        ),
        spec!("audio.pause", "Pause audio", "{}  → transport", false, |s, _| {
            s.audio.pause();
            Ok(transport_json(s))
        }),
        spec!("audio.togglePlay", "Play/pause audio", "{id?}  → transport", false, |s, p| {
            if s.audio.playing() {
                s.audio.pause();
                return Ok(transport_json(s));
            }
            play(s, p)
        }),
        spec!(
            "audio.seek",
            "Move audio playhead",
            "{id?, time? (media seconds) | frame? (ruler frame) | x? (layer units from the clip's left edge)}  → transport",
            false,
            seek
        ),
        spec!("audio.transport", "Audio transport", "{} → {id, playing, time, frame, until}", false, |s, _| Ok(transport_json(s))),
    ]
}

/// Default strip height and width per second shown.
const HEIGHT: f64 = 96.0;
const PX_PER_SECOND: f64 = 100.0;

#[cfg(not(target_arch = "wasm32"))]
fn read_path(path: &str) -> Result<Vec<u8>> {
    crate::file_access::read(path)
}

#[cfg(target_arch = "wasm32")]
fn read_path(path: &str) -> Result<Vec<u8>> {
    Err(EngineError::Other(format!("{path}: files can't be read in the browser")))
}

/// The bytes and file name `p` names (`path`, or base64 `data` with `name`).
fn source(cmd: &str, p: &Value) -> Result<(Vec<u8>, String)> {
    if let Some(path) = string(p, "path") {
        let name = std::path::Path::new(&path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        return Ok((read_path(&path)?, name));
    }
    let Some(d) = string(p, "data") else { return Err(bad(cmd, "pass `path` or `data` (base64) with `name`")) };
    use base64::Engine as _;
    let d = d.split_once(',').map(|(_, b)| b.to_string()).unwrap_or(d);
    let bytes = base64::engine::general_purpose::STANDARD.decode(d.trim()).map_err(|e| bad(cmd, format!("`data`: {e}")))?;
    Ok((bytes, string(p, "name").unwrap_or_else(|| "audio.wav".into())))
}

/// A clip for the sound `bytes` (decoded once for its length and peaks), its hash registered
/// with `doc`.
pub fn clip_for(cmd: &str, doc: &mut Document, bytes: Vec<u8>, file_name: &str) -> Result<(AudioClip, String)> {
    let shared: Arc<[u8]> = Arc::from(bytes.as_slice());
    let sound = figcraft_audio::open(file_name, shared).map_err(|e| bad(cmd, e.to_string()))?;
    let i = sound.info().clone();
    let mut clip = AudioClip::new(String::new(), file_name, i.duration, i.sample_rate, i.channels);
    clip.peak_rate = figcraft_audio::PEAK_RATE;
    clip.peaks = Peaks(Arc::new(sound.peaks(figcraft_audio::PEAK_RATE)));
    clip.asset = figcraft_format::add_audio(doc, bytes);
    Ok((clip, i.codec))
}

fn place(s: &mut Session, p: &Value) -> Result<Value> {
    let (bytes, file) = source("audio.place", p)?;
    place_bytes(s, bytes, &file, p)
}

/// [`audio.place`](specs) with the file already read: `file` is its name.
pub fn place_bytes(s: &mut Session, bytes: Vec<u8>, file: &str, p: &Value) -> Result<Value> {
    let cmd = "audio.place";
    let page = s.page()?;
    let parent = opt_id(cmd, p, "parent")?.unwrap_or(page);
    let ds = s.ds_mut()?;
    let doc = ds.doc_mut();
    let (mut clip, codec) = clip_for(cmd, doc, bytes, file)?;
    apply_settings(cmd, &mut clip, p)?;
    if let Some(Value::Array(ms)) = get(p, "markers") {
        clip.markers = markers_of(cmd, &clip, ms)?;
    }
    clip.sanitize();
    let w = num(p, "width").filter(|w| w.is_finite() && *w > 0.0).unwrap_or((clip.span() * PX_PER_SECOND).clamp(160.0, 4000.0));
    let h = num(p, "height").filter(|h| h.is_finite() && *h > 0.0).unwrap_or(HEIGHT);
    let nid = doc.alloc_id();
    let name = string(p, "name").unwrap_or_else(|| format!("Audio · {file}"));
    let mut node = Node::new(nid, NodeKind::Frame, name, w.min(100_000.0), h.min(100_000.0));
    node.transform = figcraft_geom::Affine::translate((num_or(p, "x", 0.0), num_or(p, "y", 0.0)));
    node.fills = vec![figcraft_color::Paint::solid(figcraft_color::Color::from_rgb8_const(0x14, 0x1A, 0x2E))];
    node.corners.radii = [6.0; 4];
    let out = json!({"id": nid, "audio": clip.asset, "file": file, "duration": clip.duration, "sampleRate": clip.sample_rate,
        "channels": clip.channels, "codec": codec, "start": clip.start, "end": clip.end, "fps": clip.fps, "width": node.width, "height": node.height});
    node.audio = Some(Box::new(clip));
    doc.insert(node, parent, None)?;
    ds.selection = vec![nid];
    Ok(out)
}

/// A media time from `p`: `time` (seconds) or `frame` (ruler frame, `offset` included).
fn time_of(cmd: &str, a: &AudioClip, p: &Value, time: &str, frame: &str) -> Result<Option<f64>> {
    let t = match (num(p, time), num(p, frame)) {
        (Some(t), _) => t,
        (None, Some(f)) => a.time_of_frame(f),
        (None, None) => return Ok(None),
    };
    if !t.is_finite() {
        return Err(bad(cmd, format!("`{time}` / `{frame}` must be finite")));
    }
    Ok(Some(t))
}

/// Range, ruler and colour settings of `p` on `a`.
fn apply_settings(cmd: &str, a: &mut AudioClip, p: &Value) -> Result<()> {
    if let Some(f) = num(p, "fps") {
        if !(f.is_finite() && (0.01..=1000.0).contains(&f)) {
            return Err(bad(cmd, "`fps`: 0.01..1000"));
        }
        a.fps = f;
    }
    if let Some(o) = num(p, "offset") {
        if !o.is_finite() {
            return Err(bad(cmd, "`offset` must be finite"));
        }
        a.offset = o;
    }
    let start = time_of(cmd, a, p, "start", "startFrame")?;
    let end = time_of(cmd, a, p, "end", "endFrame")?;
    let (s0, e0) = (start.unwrap_or(a.start), end.unwrap_or(a.end));
    if e0 <= s0 {
        return Err(bad(cmd, format!("the range must end after it starts (start {s0}, end {e0})")));
    }
    (a.start, a.end) = (s0, e0);
    if let Some(c) = color(p, "color") {
        a.color = c;
    }
    if let Some(c) = color(p, "markerColor") {
        a.marker_color = c;
    }
    if let Some(r) = boolean(p, "ruler") {
        a.ruler = r;
    }
    Ok(())
}

fn markers_of(cmd: &str, a: &AudioClip, ms: &[Value]) -> Result<Vec<AudioMarker>> {
    if ms.len() > figcraft_doc::audio::MAX_MARKERS {
        return Err(bad(cmd, format!("at most {} markers", figcraft_doc::audio::MAX_MARKERS)));
    }
    ms.iter()
        .enumerate()
        .map(|(i, m)| {
            let time = time_of(cmd, a, m, "time", "frame")?.ok_or_else(|| bad(cmd, format!("markers[{i}]: pass `time` or `frame`")))?;
            let mut label = string(m, "label").unwrap_or_default();
            figcraft_doc::audio::truncate_chars(&mut label, figcraft_doc::audio::MAX_LABEL);
            Ok(AudioMarker { time, label, color: color(m, "color") })
        })
        .collect()
}

/// The audio layers `p` names: `ids`/`id`, else the selected audio layers, else every audio layer
/// on the page (`all_if_none`).
fn audio_targets(cmd: &str, s: &Session, p: &Value, all_if_none: bool) -> Result<Vec<NodeId>> {
    let named = get(p, "ids").is_some_and(|v| !v.is_null()) || get(p, "id").is_some_and(|v| !v.is_null());
    let doc = s.doc()?;
    let has = |i: &NodeId| doc.node(*i).is_some_and(|n| n.audio.is_some());
    let ids: Vec<NodeId> = if named { req_targets(cmd, s, p)? } else { s.selection() };
    let mut out: Vec<NodeId> = ids.iter().copied().filter(has).collect();
    if named && out.len() < ids.len() {
        return Err(bad(cmd, "not an audio layer (place one with audio.place)"));
    }
    if out.is_empty() && !named {
        if let Some(n) = s.audio.node.filter(has) {
            out.push(n);
        } else if all_if_none {
            let page = s.page()?;
            out = doc.nodes().filter(|n| n.audio.is_some() && doc.page_of(n.id) == Some(page)).map(|n| n.id).collect();
        }
    }
    if out.is_empty() {
        return Err(EngineError::Disabled(cmd.to_string(), "no audio layer selected (pass `ids`)".into()));
    }
    Ok(out)
}

fn clip_json(doc: &Document, id: NodeId) -> Value {
    let Some(n) = doc.node(id) else { return Value::Null };
    let Some(a) = n.audio.as_deref() else { return Value::Null };
    let b = doc.world_bounds(id);
    let markers: Vec<Value> = a
        .markers
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut v = json!({"index": i, "time": round6(m.time), "frame": a.frame_of(m.time), "label": m.label});
            if let (Some(c), Some(o)) = (m.color, v.as_object_mut()) {
                o.insert("color".into(), json!(c.to_hex()));
            }
            v
        })
        .collect();
    json!({"id": id, "name": n.name, "file": a.file_name, "duration": round6(a.duration), "sampleRate": a.sample_rate, "channels": a.channels,
        "start": round6(a.start), "end": round6(a.end), "fps": a.fps, "offset": a.offset, "startFrame": a.frame_of(a.start), "endFrame": a.frame_of(a.end),
        "x": b.map(|b| b.x0), "y": b.map(|b| b.y0), "width": n.width, "height": n.height, "playable": doc.audio.contains_key(&a.asset), "markers": markers})
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn info(s: &mut Session, p: &Value) -> Result<Value> {
    let ids = audio_targets("audio.info", s, p, true)?;
    let doc = s.doc()?;
    Ok(json!({"clips": ids.iter().map(|i| clip_json(doc, *i)).collect::<Vec<_>>()}))
}

/// Edit the clip of each of `ids` with `f`.
fn edit(s: &mut Session, ids: &[NodeId], mut f: impl FnMut(&mut Node, &mut AudioClip) -> Result<()>) -> Result<()> {
    let doc = s.doc_mut()?;
    for id in ids {
        let n = doc.get_mut(*id)?;
        let Some(mut a) = n.audio.take() else { continue };
        let r = f(n, &mut a);
        a.sanitize();
        n.audio = Some(a);
        r?;
    }
    Ok(())
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.set";
    let ids = audio_targets(cmd, s, p, false)?;
    let pps = num(p, "pxPerSecond");
    if pps.is_some_and(|v| !(v.is_finite() && v > 0.0)) {
        return Err(bad(cmd, "`pxPerSecond` must be positive"));
    }
    edit(s, &ids, |n, a| {
        apply_settings(cmd, a, p)?;
        if let Some(v) = pps {
            n.width = (a.span() * v).clamp(1.0, 100_000.0);
        }
        Ok(())
    })?;
    let doc = s.doc()?;
    Ok(json!({"clips": ids.iter().map(|i| clip_json(doc, *i)).collect::<Vec<_>>()}))
}

fn add_marker(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.addMarker";
    let ids = audio_targets(cmd, s, p, false)?;
    let playhead = s.audio.node.map(|n| (n, s.audio.time()));
    let mut out = Vec::new();
    edit(s, &ids, |n, a| {
        if a.markers.len() >= figcraft_doc::audio::MAX_MARKERS {
            return Err(bad(cmd, format!("at most {} markers", figcraft_doc::audio::MAX_MARKERS)));
        }
        let time = match time_of(cmd, a, p, "time", "frame")? {
            Some(t) => t,
            None => match playhead {
                Some((pn, t)) if pn == n.id => t,
                _ => return Err(bad(cmd, "pass `time` or `frame` (or put the playhead on the clip)")),
            },
        };
        let mut label = string(p, "label").unwrap_or_default();
        figcraft_doc::audio::truncate_chars(&mut label, figcraft_doc::audio::MAX_LABEL);
        a.markers.push(AudioMarker { time, label, color: color(p, "color") });
        a.sort_markers();
        let index = a.markers.iter().rposition(|m| m.time == time).unwrap_or(0);
        out.push(json!({"id": n.id, "index": index, "time": round6(time), "frame": a.frame_of(time)}));
        Ok(())
    })?;
    Ok(json!({"markers": out}))
}

fn set_markers(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.setMarkers";
    let Some(Value::Array(ms)) = get(p, "markers") else { return Err(bad(cmd, "`markers`: an array of {time | frame, label?, color?}")) };
    let ids = audio_targets(cmd, s, p, false)?;
    let mut count = 0;
    edit(s, &ids, |_, a| {
        a.markers = markers_of(cmd, a, ms)?;
        count = a.markers.len();
        Ok(())
    })?;
    Ok(json!({"count": count}))
}

fn remove_marker(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.removeMarker";
    let ids = audio_targets(cmd, s, p, false)?;
    let (index, label, all) = (opt_index(cmd, p, "index")?, string(p, "label"), boolean(p, "all").unwrap_or(false));
    if index.is_none() && label.is_none() && !all {
        return Err(bad(cmd, "pass `index`, `label` or `all`"));
    }
    let mut removed = 0;
    edit(s, &ids, |_, a| {
        let before = a.markers.len();
        if all {
            a.markers.clear();
        } else if let Some(i) = index {
            if i < a.markers.len() {
                a.markers.remove(i);
            }
        } else if let Some(l) = &label {
            a.markers.retain(|m| &m.label != l);
        }
        removed += before - a.markers.len();
        Ok(())
    })?;
    Ok(json!({"removed": removed}))
}

/// The most speech map entries read.
const MAX_WORDS: usize = 100_000;

fn import_speech_map(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.importSpeechMap";
    let map: Value = match (get(p, "map"), string(p, "path")) {
        (Some(m), _) => m.clone(),
        (None, Some(path)) => serde_json::from_slice(&read_path(&path)?).map_err(|e| bad(cmd, format!("{path}: {e}")))?,
        (None, None) => return Err(bad(cmd, "pass `map` or `path`")),
    };
    let words = match &map {
        Value::Array(a) => a.clone(),
        Value::Object(o) => match o.get("words") {
            Some(Value::Array(a)) => a.clone(),
            _ => return Err(bad(cmd, "`map`: an array of words or {words: [...]}")),
        },
        _ => return Err(bad(cmd, "`map`: an array of words or {words: [...]}")),
    };
    let media = match string(p, "timeBase").as_deref() {
        None | Some("timeline") | Some("comp") => false,
        Some("media") => true,
        Some(_) => return Err(bad(cmd, "`timeBase`: timeline | media")),
    };
    let level = num(p, "stress").unwrap_or(1.0);
    let at = string(p, "at").unwrap_or_else(|| "onset".into());
    if !matches!(at.as_str(), "onset" | "vowel" | "peak") {
        return Err(bad(cmd, "`at`: onset | vowel | peak"));
    }
    let replace = boolean(p, "replace").unwrap_or(true);
    let col = color(p, "color");
    let ids = audio_targets(cmd, s, p, false)?;
    let mut count = 0;
    edit(s, &ids, |_, a| {
        let mut out: Vec<AudioMarker> = if replace { Vec::new() } else { std::mem::take(&mut a.markers) };
        for w in words.iter().take(MAX_WORDS) {
            let label = string(w, "word").unwrap_or_default();
            let Some(Value::Array(syls)) = get(w, "syllables") else { continue };
            for sy in syls {
                let stress = num(sy, "stress").unwrap_or(0.0);
                let wanted = if level <= 0.0 {
                    true
                } else if level >= 2.0 {
                    stress >= 1.0
                } else {
                    stress == 1.0
                };
                if !wanted {
                    continue;
                }
                let (onset, vowel) = (num(sy, "t"), num(sy, "vowel"));
                let t = match at.as_str() {
                    "vowel" => vowel.or(onset),
                    "peak" => vowel.map(|v| v - 1.0 / a.fps).or(onset),
                    _ => onset.or(vowel),
                };
                let Some(t) = t.filter(|t| t.is_finite()) else { continue };
                let t = if media { t } else { t - a.offset };
                if out.len() >= figcraft_doc::audio::MAX_MARKERS {
                    break;
                }
                let mut label = label.clone();
                figcraft_doc::audio::truncate_chars(&mut label, figcraft_doc::audio::MAX_LABEL);
                out.push(AudioMarker { time: t, label, color: col });
            }
        }
        a.markers = out;
        count = a.markers.len();
        Ok(())
    })?;
    Ok(json!({"count": count}))
}

/// The clip the transport commands act on: `id`, else the selected audio layer, else the
/// playhead's.
fn transport_target(cmd: &str, s: &Session, p: &Value) -> Result<(NodeId, AudioClip)> {
    let id = match opt_id(cmd, p, "id")? {
        Some(i) => i,
        None => *audio_targets(cmd, s, p, true)?.first().ok_or_else(|| bad(cmd, "no audio layer"))?,
    };
    let a = s.doc()?.node(id).and_then(|n| n.audio.as_deref()).cloned().ok_or_else(|| bad(cmd, "not an audio layer"))?;
    Ok((id, a))
}

fn play(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.play";
    let (id, a) = transport_target(cmd, s, p)?;
    let from = match time_of(cmd, &a, p, "time", "frame")? {
        Some(t) => Some(t),
        // Resume where the playhead is (from the start once it reached the end).
        None => (s.audio.node == Some(id) && s.audio.time() < a.end - 1e-3).then(|| s.audio.time()),
    };
    s.audio.play(id, &a, from);
    Ok(transport_json(s))
}

fn seek(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "audio.seek";
    let (id, a) = transport_target(cmd, s, p)?;
    let w = s.doc()?.node(id).map_or(0.0, |n| n.width);
    let t = match (time_of(cmd, &a, p, "time", "frame")?, num(p, "x")) {
        (Some(t), _) => t,
        (None, Some(x)) if x.is_finite() => a.time_at(x, w),
        _ => return Err(bad(cmd, "pass `time`, `frame` or `x`")),
    };
    s.audio.seek(id, &a, t);
    Ok(transport_json(s))
}

pub fn transport_json(s: &Session) -> Value {
    let t = &s.audio;
    let clip = t.node.and_then(|n| s.doc().ok()?.node(n)?.audio.as_deref().cloned());
    let time = t.time();
    json!({"id": t.node, "playing": t.playing(), "time": round6(time), "frame": clip.as_ref().map(|a| a.frame_of(time)), "until": round6(t.until())})
}

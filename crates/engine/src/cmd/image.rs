//! Image paints: adjustment presets, built in and saved in the file.

use figcraft_color::{ImageFilters, PaintKind};
use figcraft_doc::{Document, ImagePreset};
use serde_json::{Value, json};

use super::{CommandSpec, ok};
use crate::params::*;
use crate::{Result, Session, bad, spec};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "image.adjust",
            "Adjust image",
            "{ids?, target? (fill|stroke, default fill), index? (default every image paint), preset? (vivid|muted|warm|cool|blackAndWhite|fade|reset, or the name of a preset saved in the file), filters? ({exposure, contrast, saturation, temperature, tint, highlights, shadows}: each -1..1, applied over the preset)}  sets the adjustments of the layers' image paints; returns {changed}",
            true,
            adjust
        ),
        spec!(
            "image.savePreset",
            "Save image preset",
            "{name, filters? (default the adjustments of the first image paint of `ids`/the selection), ids?, target?, index?}  saved in the file; a preset with the same name is replaced",
            true,
            save_preset
        ),
        spec!("image.deletePreset", "Delete image preset", "{name}", true, |s, p| {
            let name = req_string("image.deletePreset", p, "name")?;
            let doc = s.doc_mut()?;
            let before = doc.image_presets.len();
            doc.image_presets.retain(|x| x.name != name);
            if doc.image_presets.len() == before {
                return Err(bad("image.deletePreset", format!("no preset named `{name}`")));
            }
            ok()
        }),
        spec!(
            "image.relinkMissing",
            "Relink missing images",
            "{path? (a .fig file, or a folder of image files named by their hash or matching it), data? (base64 of a .fig file), images? ({hash: base64 image bytes})} → {relinked, missing}  fills in images a paste or an import couldn't load (shown as missing-image placeholders); the others stay as they are",
            true,
            relink_missing
        ),
        spec!("image.presets", "Image presets", "{} → {builtIn: [{id, label, filters}], saved: [{name, filters}]}", false, |s, _| {
            let built_in: Vec<Value> = ImageFilters::PRESETS.iter().map(|(id, label, f)| json!({"id": id, "label": label, "filters": f})).collect();
            Ok(json!({"builtIn": built_in, "saved": s.doc()?.image_presets}))
        }),
    ]
}

/// The longest name a saved preset may have.
const MAX_NAME: usize = 100;
/// The most presets a file keeps.
const MAX_PRESETS: usize = 500;

/// Whether `p` asks for strokes (`target: stroke`) rather than fills.
fn strokes(cmd: &str, p: &Value) -> Result<bool> {
    match string(p, "target").as_deref() {
        None | Some("fill") | Some("fills") => Ok(false),
        Some("stroke") | Some("strokes") => Ok(true),
        Some(_) => Err(bad(cmd, "target: fill|stroke")),
    }
}

/// The preset `name`: one saved in the file under that exact name, else a built-in one.
pub fn find_preset(doc: &Document, name: &str) -> Option<ImageFilters> {
    doc.image_presets.iter().find(|x| x.name == name).map(|x| x.filters).or_else(|| ImageFilters::preset(name))
}

/// `base` with the adjustments `v` names (an object of -1..1 numbers) set.
fn with_values(cmd: &str, base: ImageFilters, v: &Value) -> Result<ImageFilters> {
    let Value::Object(m) = v else { return Err(bad(cmd, "filters: an object of adjustments")) };
    let mut f = base;
    for (k, val) in m {
        let x = f64_of(val).ok_or_else(|| bad(cmd, format!("filters.{k}: a number")))? as f32;
        let slot = match k.as_str() {
            "exposure" => &mut f.exposure,
            "contrast" => &mut f.contrast,
            "saturation" => &mut f.saturation,
            "temperature" => &mut f.temperature,
            "tint" => &mut f.tint,
            "highlights" => &mut f.highlights,
            "shadows" => &mut f.shadows,
            _ => return Err(bad(cmd, format!("unknown adjustment `{k}`"))),
        };
        *slot = x;
    }
    Ok(f.sanitized())
}

fn adjust(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "image.adjust";
    let ids = req_targets(cmd, s, p)?;
    let stroke = strokes(cmd, p)?;
    let index = match get(p, "index") {
        None | Some(Value::Null) => None,
        Some(_) => Some(req_index(cmd, p, "index")?),
    };
    let preset = match string(p, "preset") {
        Some(name) => Some(find_preset(s.doc()?, &name).ok_or_else(|| bad(cmd, format!("no preset named `{name}`")))?),
        None => None,
    };
    let values = get(p, "filters").filter(|v| !v.is_null());
    if preset.is_none() && values.is_none() {
        return Err(bad(cmd, "pass `preset` or `filters`"));
    }
    let doc = s.doc_mut()?;
    let mut changed = 0;
    for id in ids {
        let n = doc.get(id)?;
        if !if stroke { &n.strokes } else { &n.fills }.iter().any(|x| matches!(x.kind, PaintKind::Image(_))) {
            continue;
        }
        let n = doc.get_mut(id)?;
        let list = if stroke { &mut n.strokes } else { &mut n.fills };
        let mut touched = false;
        for (i, paint) in list.iter_mut().enumerate() {
            let PaintKind::Image(img) = &mut paint.kind else { continue };
            if index.is_some_and(|k| k != i) {
                continue;
            }
            let base = preset.unwrap_or(img.filters);
            img.filters = match values {
                Some(v) => with_values(cmd, base, v)?,
                None => base,
            };
            touched = true;
            changed += 1;
        }
        if touched {
            // Like any paint edit, the layer leaves its paint style.
            if stroke {
                n.styles.stroke = None;
            } else {
                n.styles.fill = None;
            }
        }
    }
    if changed == 0 {
        return Err(bad(cmd, if index.is_some() { "no image paint at that index" } else { "the layers have no image paints" }));
    }
    Ok(json!({"changed": changed}))
}

fn save_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "image.savePreset";
    let name: String = req_string(cmd, p, "name")?.trim().chars().filter(|c| !c.is_control()).take(MAX_NAME).collect();
    if name.is_empty() {
        return Err(bad(cmd, "`name` is empty"));
    }
    let filters = match get(p, "filters").filter(|v| !v.is_null()) {
        Some(v) => with_values(cmd, ImageFilters::NONE, v)?,
        None => {
            let stroke = strokes(cmd, p)?;
            let index = num(p, "index").filter(|k| k.is_finite() && *k >= 0.0).map(|k| k as usize);
            let doc = s.doc()?;
            targets(s, p)
                .iter()
                .filter_map(|id| doc.node(*id))
                .flat_map(|n| if stroke { n.strokes.iter() } else { n.fills.iter() }.enumerate())
                .find_map(|(i, x)| match &x.kind {
                    PaintKind::Image(img) if index.is_none_or(|k| k == i) => Some(img.filters),
                    _ => None,
                })
                .ok_or_else(|| bad(cmd, "pass `filters`, or select a layer with an image paint"))?
        }
    };
    let doc = s.doc_mut()?;
    let preset = ImagePreset { name: name.clone(), filters };
    match doc.image_presets.iter().position(|x| x.name == name) {
        Some(i) => {
            if let Some(x) = doc.image_presets.get_mut(i) {
                *x = preset;
            }
        }
        None if doc.image_presets.len() >= MAX_PRESETS => return Err(bad(cmd, format!("a file keeps at most {MAX_PRESETS} presets"))),
        None => doc.image_presets.push(preset),
    }
    Ok(json!({"name": name, "filters": filters}))
}

/// A `.fig` file (or a folder's image) read for relinking: as big as a file may open.
const MAX_RELINK_FILE: u64 = 1024 * 1024 * 1024;
/// One image file in a folder.
const MAX_RELINK_IMAGE: u64 = 64 * 1024 * 1024;
/// Files looked at in a folder.
const MAX_RELINK_FILES: usize = 5_000;

/// Images of `.fig` bytes or of the files in folder `path` that some of `missing` (hashes) name.
fn relink_sources(p: &Value, missing: &[String]) -> Result<std::collections::BTreeMap<String, Vec<u8>>> {
    const CMD: &str = "image.relinkMissing";
    let fig = |bytes: &[u8]| figcraft_format::fig::archive_images(bytes, missing.iter().map(String::as_str)).map_err(|e| bad(CMD, e.to_string()));
    // Image bytes by hash, as a paste's downloads deliver them.
    if let Some(images) = p.get("images").and_then(Value::as_object) {
        use base64::Engine as _;
        let mut found = std::collections::BTreeMap::new();
        for (hash, b64) in images.iter().take(figcraft_format::fig::clipboard::MAX_IMAGES) {
            let Some(h) = missing.iter().find(|m| m.eq_ignore_ascii_case(hash)) else { continue };
            let bytes = b64.as_str().and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok());
            if let Some(bytes) = bytes {
                found.insert(h.clone(), bytes);
            }
        }
        return Ok(found);
    }
    if let Some(data) = string(p, "data") {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(data.trim()).map_err(|_| bad(CMD, "`data` is not base64"))?;
        return fig(&bytes);
    }
    let path = string(p, "path").ok_or_else(|| bad(CMD, "pass `path` (a .fig file or a folder), `data` or `images`"))?;
    if !crate::file_access::is_dir(&path) {
        return fig(&crate::file_access::read_capped(&path, MAX_RELINK_FILE)?);
    }
    let mut found = std::collections::BTreeMap::new();
    for file in crate::file_access::read_dir(&path)?.into_iter().take(MAX_RELINK_FILES) {
        if found.len() == missing.len() {
            break;
        }
        let stem = file.file_stem().and_then(|s| s.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        let named = missing.iter().find(|h| **h == stem && !found.contains_key(*h)).cloned();
        let Ok(bytes) = crate::file_access::read_capped(&file, MAX_RELINK_IMAGE) else { continue };
        // A file named by its hash, or one whose bytes have a missing image's hash (a `.fig`
        // file's SHA-1, or the SHA-256 FigCraft's own files use).
        let hash = named
            .or_else(|| [figcraft_format::fig::image_hash(&bytes), figcraft_format::image_hash(&bytes)].into_iter().find(|h| missing.contains(h)));
        if let Some(h) = hash {
            found.insert(h, bytes);
        }
    }
    Ok(found)
}

fn relink_missing(s: &mut Session, p: &Value) -> Result<Value> {
    let doc = s.doc()?;
    let missing: Vec<String> =
        doc.images.iter().filter(|(_, img)| figcraft_format::rest::is_missing_image(&img.bytes)).map(|(k, _)| k.clone()).collect();
    if missing.is_empty() {
        return Ok(json!({"relinked": 0, "missing": 0}));
    }
    let found = relink_sources(p, &missing)?;
    let doc = s.doc_mut()?;
    let mut renamed = std::collections::BTreeMap::new();
    for (old, bytes) in found {
        // Bytes that don't decode as an image stay missing.
        if let Ok((new, _, _)) = figcraft_format::add_image(doc, bytes) {
            renamed.insert(old, new);
        }
    }
    let rename = |paints: &mut [figcraft_color::Paint]| {
        for paint in paints {
            if let PaintKind::Image(img) = &mut paint.kind
                && let Some(new) = renamed.get(&img.image)
            {
                img.image = new.clone();
            }
        }
    };
    let ids: Vec<_> = doc.nodes().map(|n| n.id).collect();
    for id in ids {
        if let Some(n) = doc.node_mut(id) {
            rename(&mut n.fills);
            rename(&mut n.strokes);
            if let figcraft_doc::NodeKind::Text(t) = &mut n.kind {
                if let Some(f) = t.style.fills.as_mut() {
                    rename(f);
                }
                for r in &mut t.runs {
                    if let Some(f) = r.style.fills.as_mut() {
                        rename(f);
                    }
                }
            }
        }
    }
    for style in &mut doc.styles {
        if let figcraft_doc::StyleKind::Paint { paints } = &mut style.kind {
            rename(paints);
        }
    }
    for (old, new) in &renamed {
        if old == new {
            // Same key, new bytes: the decoded placeholder must not be drawn again.
            figcraft_render::images::forget(old);
        } else {
            doc.images.remove(old);
        }
    }
    Ok(json!({"relinked": renamed.len(), "missing": missing.len() - renamed.len()}))
}

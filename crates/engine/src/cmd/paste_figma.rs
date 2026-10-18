//! Selection import uses the `.fig`/REST mapping and normal Paste placement, not file-open's
//! append-pages behavior. All resource and node IDs are local to this undoable import.
use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use figcraft_doc::*;
use figcraft_geom::Affine;
use serde_json::{Value, json};

use crate::{EngineError, Result, Session, bad};

const CMD: &str = "edit.pasteFigma";
/// All images of one paste (the platform's download cap); images past it are left out, not refused.
const MAX_IMAGE_BYTES: usize = 1024 * 1024 * 1024;
/// Missing images listed by hash in the result.
const MAX_LISTED: usize = 200;

#[cfg(test)]
#[path = "paste_figma_tests.rs"]
mod tests;

pub(super) fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let html = p.get("html").and_then(Value::as_str).ok_or_else(|| bad(CMD, "html is required"))?;
    let contents = figcraft_format::fig::clipboard::decode(html).map_err(|e| bad(CMD, e.to_string()))?;
    let mut images = BTreeMap::new();
    let mut budget = MAX_IMAGE_BYTES;
    let mut over_budget = 0usize;
    if let Some(entries) = p.get("images").and_then(Value::as_object) {
        if entries.len() > figcraft_format::fig::clipboard::MAX_IMAGES {
            return Err(bad(CMD, "too many images"));
        }
        for (hash, value) in entries {
            let value = value.as_str().ok_or_else(|| bad(CMD, "image values must be base64"))?;
            if value.len() > budget.saturating_mul(4).div_ceil(3) + 4 {
                over_budget += 1;
                continue;
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(value).map_err(|_| bad(CMD, "invalid image base64"))?;
            match budget.checked_sub(bytes.len()) {
                Some(left) => budget = left,
                None => {
                    over_budget += 1;
                    continue;
                }
            }
            images.insert(hash.clone(), bytes);
        }
    }
    // Why the platform couldn't load an image, by hash (shown with the missing images).
    let failures: BTreeMap<String, String> = p
        .get("imageFailures")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .take(figcraft_format::fig::clipboard::MAX_IMAGES)
        .filter_map(|(h, why)| Some((h.chars().take(64).collect(), why.as_str()?.chars().take(200).collect())))
        .collect();
    let (mut other, mut warnings) = figcraft_format::fig::import_with_images(&contents.canvas, "Pasted Figma layers", &|h| {
        images.get(h).cloned().or_else(|| s.doc().ok()?.images.get(h).map(|i| i.bytes.as_ref().clone()))
    })
    .map_err(|e| bad(CMD, e.to_string()))?;
    crate::cmd::file::link_instances(&mut other);
    let roots: Vec<_> = other.pages.iter().filter_map(|p| other.node(*p)).flat_map(|p| p.children.iter().copied()).collect();
    if roots.is_empty() {
        return Err(bad(CMD, "the clipboard contains no supported layers"));
    }
    let bounds = other.bounds_of(&roots);
    let (parent, at) = super::paste_destination(s, p, bounds)?;
    let shift = match (at, bounds) {
        (Some((x, y)), Some(b)) => Affine::translate((x - b.x0, y - b.y0)),
        _ => Affine::IDENTITY,
    };
    let mut missing = BTreeSet::new();
    let ids = insert(s.doc_mut()?, &mut other, &roots, parent, shift, &mut warnings, &mut missing)?;
    s.set_selection(ids.clone());
    let layers: usize = ids.iter().map(|id| s.doc().map_or(0, |d| d.subtree(*id).len())).sum();
    let used = image_keys(s.doc()?, &ids);
    // Only the images the pasted layers show (the mains brought along may hold others).
    missing.retain(|h| used.contains(h));
    let loaded = used.len().saturating_sub(missing.len());
    // The platform downloads the images after the paste and fills them in (`image.relinkMissing`).
    let pending = p.get("imagesPending").and_then(Value::as_bool).unwrap_or(false);
    let missing_list: Vec<Value> = missing
        .iter()
        .take(MAX_LISTED)
        .map(|h| json!({"hash": h, "reason": failures.get(h).map_or("not on the clipboard", String::as_str)}))
        .collect();
    // The headline goes first: how much of the copy came across.
    let mut notes = Vec::new();
    if !missing.is_empty() && !pending {
        let mut why: BTreeMap<&str, usize> = BTreeMap::new();
        for h in &missing {
            *why.entry(failures.get(h).map_or("not on the clipboard", String::as_str)).or_default() += 1;
        }
        let why: Vec<String> = why.into_iter().map(|(r, n)| format!("{n} {r}")).collect();
        notes.push(format!(
            "{} of {} image(s) couldn't be loaded ({}); they show a missing-image placeholder. File ▸ Relink missing images… fills them in from a .fig copy of the file or a folder of the images",
            missing.len(),
            used.len(),
            why.join(", ")
        ));
    }
    if contents.dropped > 0 {
        notes.push(format!(
            "{} image links on the clipboard were left out (more than {} images, or not readable)",
            contents.dropped,
            figcraft_format::fig::clipboard::MAX_IMAGES
        ));
    }
    if over_budget > 0 {
        notes.push(format!("{over_budget} images were left out: the copy's images pass 1 GB"));
    }
    // The headline already covers the importer's own note about missing images.
    if !missing.is_empty() {
        warnings.retain(|w| !w.contains("missing-image placeholder"));
    }
    match warnings.len() {
        0 => {}
        1 => notes.push("1 layer or property was approximated or left out (listed below)".into()),
        n => notes.push(format!("{n} layers or properties were approximated or left out (listed below)")),
    }
    notes.append(&mut warnings);
    Ok(json!({
        "ids": ids,
        "warnings": notes,
        "missingImages": missing_list,
        "summary": {"name": "Pasted Figma layers", "layers": layers, "images": loaded},
    }))
}

/// Image keys the paints of `roots` and their layers use.
fn image_keys(doc: &Document, roots: &[NodeId]) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for n in roots.iter().flat_map(|r| doc.subtree(*r)).filter_map(|id| doc.node(id)) {
        figcraft_doc::node_images(n, &mut keys);
    }
    keys
}

fn warn(warnings: &mut Vec<String>, message: String) {
    if warnings.len() < 200 {
        warnings.push(message);
    }
}

fn alias(value: &mut VariableValue, prefix: &str) {
    if let VariableValue::Alias(id) = value {
        *id = format!("{prefix}{id}");
    }
}

fn expression(expr: &mut proto::Expr, prefix: &str, depth: usize) {
    if depth > proto::MAX_EXPR_DEPTH {
        return;
    }
    match expr {
        proto::Expr::Value(value) => alias(value, prefix),
        proto::Expr::Mode { collection } => *collection = format!("{prefix}{collection}"),
        proto::Expr::InMode { variable, .. } => *variable = format!("{prefix}{variable}"),
        proto::Expr::Op { args, .. } => args.iter_mut().for_each(|e| expression(e, prefix, depth + 1)),
    }
}

fn bindings(bound: &mut BTreeMap<String, String>, prefix: &str) {
    for id in bound.values_mut() {
        *id = format!("{prefix}{id}");
    }
}

fn image_paints(paints: &mut [figcraft_color::Paint], images: &BTreeMap<String, String>) {
    for paint in paints {
        if let figcraft_color::PaintKind::Image(image) = &mut paint.kind
            && let Some(id) = images.get(&image.image)
        {
            image.image = id.clone();
        }
    }
}

/// Point `n` at the pasted copies of the copy's styles, variables and images.
fn resource_refs(n: &mut Node, prefix: &str, images: &BTreeMap<String, String>) {
    image_paints(&mut n.fills, images);
    image_paints(&mut n.strokes, images);
    for r in [&mut n.styles.fill, &mut n.styles.stroke, &mut n.styles.text, &mut n.styles.grid, &mut n.styles.effect] {
        *r = r.take().map(|id| format!("{prefix}{id}"));
    }
    if let NodeKind::Text(text) = &mut n.kind {
        text.map_text_styles(|id| Some(format!("{prefix}{id}")));
    }
    bindings(&mut n.bound, prefix);
    n.var_modes = std::mem::take(&mut n.var_modes).into_iter().map(|(c, m)| (format!("{prefix}{c}"), m)).collect();
}

fn insert(
    doc: &mut Document,
    src: &mut Document,
    roots: &[NodeId],
    parent: NodeId,
    shift: Affine,
    warnings: &mut Vec<String>,
    missing: &mut BTreeSet<String>,
) -> Result<Vec<NodeId>> {
    // A reserved node id gives this import a collision-free namespace, including references whose
    // definitions were absent from the clipboard. Such references cannot bind to unrelated data.
    let used: BTreeSet<String> = doc
        .styles
        .iter()
        .map(|s| &s.id)
        .chain(doc.variables.collections.iter().map(|c| &c.id))
        .chain(doc.variables.variables.iter().map(|v| &v.id))
        .filter_map(|id| id.split_once(':').map(|(prefix, _)| prefix.to_string()))
        .collect();
    let prefix = (0..=used.len())
        .map(|_| format!("figma-paste-{}", doc.alloc_id().0))
        .find(|p| !used.contains(p))
        .ok_or_else(|| bad(CMD, "cannot allocate imported resource IDs"))?
        + ":";
    let mut images = BTreeMap::new();
    for (old, image) in &src.images {
        // A missing image keeps its own hash (not the placeholder's), so it can be relinked later.
        if figcraft_format::rest::is_missing_image(&image.bytes) {
            doc.images.entry(old.clone()).or_insert_with(|| image.clone());
            missing.insert(old.clone());
            continue;
        }
        let (hash, _, _) = figcraft_format::add_image(doc, image.bytes.as_ref().clone()).map_err(|e| EngineError::Other(e.to_string()))?;
        images.insert(old.clone(), hash);
    }
    for style in &mut src.styles {
        style.id = format!("{prefix}{}", style.id);
        bindings(&mut style.bound, &prefix);
        if let StyleKind::Paint { paints } = &mut style.kind {
            image_paints(paints, &images);
        }
    }
    for coll in &mut src.variables.collections {
        coll.id = format!("{prefix}{}", coll.id);
        coll.extends = coll.extends.take().map(|c| format!("{prefix}{c}"));
    }
    for var in &mut src.variables.variables {
        var.id = format!("{prefix}{}", var.id);
        var.collection = format!("{prefix}{}", var.collection);
        var.extends = var.extends.take().map(|v| format!("{prefix}{v}"));
        for value in var.values.values_mut() {
            alias(value, &prefix);
        }
    }
    doc.styles.extend(src.styles.clone());
    doc.variables.collections.extend(src.variables.collections.clone());
    doc.variables.variables.extend(src.variables.variables.clone());

    // One map across every selected root preserves links between copied siblings.
    let mut map = BTreeMap::new();
    for root in roots {
        for id in src.subtree(*root) {
            map.entry(id).or_insert_with(|| doc.alloc_id());
        }
    }
    // The copy's library copies (the mains its instances use, on the copy's hidden page) refer to
    // its styles, variables and images like the layers do.
    if let Some(store) = src.library_store() {
        for id in src.subtree(store) {
            if let Ok(n) = src.get_mut(id) {
                resource_refs(n, &prefix, &images);
            }
        }
    }
    let mut library_instances = BTreeSet::new();
    let page = doc.page_of(parent).unwrap_or(parent);
    let mut result = Vec::new();
    for root in roots {
        let mut nodes: Vec<_> = src.subtree(*root).into_iter().filter_map(|id| src.node(id).cloned()).collect();
        let section = nodes.first().is_some_and(|n| matches!(n.kind, NodeKind::Section { .. }));
        let into = if section && !doc.takes_sections(parent) { page } else { parent };
        for n in &mut nodes {
            n.source = n.source.and_then(|id| map.get(&id).copied());
            resource_refs(n, &prefix, &images);
            match &mut n.kind {
                // A main the copy carries as a library copy comes along after the layers.
                NodeKind::Instance { component, .. } if !map.contains_key(component) && src.is_remote(*component) => {
                    library_instances.insert(n.id);
                }
                NodeKind::Instance { component, .. } if !map.contains_key(component) => {
                    n.kind = NodeKind::Frame;
                    warn(warnings, format!("{}: main component not copied; kept as an editable frame", n.name));
                }
                NodeKind::Instance { component, props } => {
                    if let Some(NodeKind::Component { props: defs, .. }) = src.node(*component).map(|n| &n.kind) {
                        for (key, def) in defs {
                            if matches!(def, PropDef::InstanceSwap { .. })
                                && let Some(value) = props.get_mut(key)
                            {
                                *value = value.as_u64().and_then(|i| map.get(&NodeId(i))).map_or(Value::Null, |i| json!(i));
                            }
                        }
                    }
                }
                NodeKind::Component { props, .. } => {
                    props.retain(|_, def| !matches!(def, PropDef::InstanceSwap { default, .. } if !map.contains_key(default)));
                    for def in props.values_mut() {
                        match def {
                            PropDef::InstanceSwap { default, preferred } => {
                                if let Some(id) = map.get(default) {
                                    *default = *id;
                                }
                                *preferred = preferred.iter().filter_map(|i| map.get(i).copied()).collect();
                            }
                            PropDef::Slot { preferred } => *preferred = preferred.iter().filter_map(|i| map.get(i).copied()).collect(),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            for paint in n.fills.iter_mut().chain(n.strokes.iter_mut()) {
                if let figcraft_color::PaintKind::Pattern(p) = &mut paint.kind
                    && !map.contains_key(&NodeId(p.source))
                {
                    p.source = doc.alloc_id().0;
                    warn(warnings, format!("{}: pattern source was not copied", n.name));
                }
            }
            for it in &mut n.interactions {
                let before = proto::walk(&it.actions).count();
                proto::retain_deep(&mut it.actions, &mut |action| {
                    use proto::Action::*;
                    match action {
                        Navigate { dest, .. } | ScrollTo { dest, .. } | OpenOverlay { dest, .. } | SwapOverlay { dest } | ChangeTo { dest } => {
                            map.contains_key(dest)
                        }
                        _ => true,
                    }
                });
                if proto::walk(&it.actions).count() != before {
                    warn(warnings, format!("{}: prototype destinations outside the copy were omitted", n.name));
                }
                proto::walk_mut(&mut it.actions, &mut |action| match action {
                    proto::Action::SetVariable { variable, expr, .. } => {
                        *variable = format!("{prefix}{variable}");
                        if let Some(e) = expr {
                            expression(e, &prefix, 0);
                        }
                    }
                    proto::Action::SetVariableMode { collection, .. } => *collection = format!("{prefix}{collection}"),
                    proto::Action::Conditional { blocks } => {
                        for b in blocks {
                            if let Some(e) = &mut b.condition {
                                expression(e, &prefix, 0);
                            }
                        }
                    }
                    _ => {}
                });
            }
        }
        if let Some(n) = nodes.first_mut() {
            n.transform = doc.world_transform(into).inverse() * shift * src.world_transform(*root);
        }
        result.push(doc.paste_nodes(&nodes, &map, *root, into, None)?);
    }
    if !library_instances.is_empty() {
        let adopt: BTreeMap<NodeId, NodeId> = map.iter().filter(|(theirs, _)| library_instances.contains(*theirs)).map(|(a, b)| (*a, *b)).collect();
        crate::library::adopt_pasted(doc, src, None, &adopt)?;
    }
    Ok(result)
}

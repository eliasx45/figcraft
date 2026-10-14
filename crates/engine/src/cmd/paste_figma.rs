//! Selection import uses the `.fig`/REST mapping and normal Paste placement, not file-open's
//! append-pages behavior. All resource and node IDs are local to this undoable import.
use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use figcraft_doc::*;
use figcraft_geom::Affine;
use serde_json::{Value, json};

use crate::{EngineError, Result, Session, bad};

const CMD: &str = "edit.pasteFigma";
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

#[cfg(test)]
#[path = "paste_figma_tests.rs"]
mod tests;

pub(super) fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let html = p.get("html").and_then(Value::as_str).ok_or_else(|| bad(CMD, "html is required"))?;
    let contents = figcraft_format::fig::clipboard::decode(html).map_err(|e| bad(CMD, e.to_string()))?;
    let mut images = BTreeMap::new();
    let mut budget = MAX_IMAGE_BYTES;
    if let Some(entries) = p.get("images").and_then(Value::as_object) {
        if entries.len() > 256 {
            return Err(bad(CMD, "too many images"));
        }
        for (hash, value) in entries {
            let value = value.as_str().ok_or_else(|| bad(CMD, "image values must be base64"))?;
            if value.len() > budget.saturating_mul(4).div_ceil(3) + 4 {
                return Err(bad(CMD, "clipboard images exceed 64 MiB"));
            }
            let bytes = base64::engine::general_purpose::STANDARD.decode(value).map_err(|_| bad(CMD, "invalid image base64"))?;
            budget = budget.checked_sub(bytes.len()).ok_or_else(|| bad(CMD, "clipboard images exceed 64 MiB"))?;
            images.insert(hash.clone(), bytes);
        }
    }
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
    let ids = insert(s.doc_mut()?, &mut other, &roots, parent, shift, &mut warnings)?;
    s.set_selection(ids.clone());
    Ok(json!({"ids": ids, "warnings": warnings}))
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

fn insert(
    doc: &mut Document,
    src: &mut Document,
    roots: &[NodeId],
    parent: NodeId,
    shift: Affine,
    warnings: &mut Vec<String>,
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
    let page = doc.page_of(parent).unwrap_or(parent);
    let mut result = Vec::new();
    for root in roots {
        let mut nodes: Vec<_> = src.subtree(*root).into_iter().filter_map(|id| src.node(id).cloned()).collect();
        let section = nodes.first().is_some_and(|n| matches!(n.kind, NodeKind::Section { .. }));
        let into = if section && !doc.takes_sections(parent) { page } else { parent };
        for n in &mut nodes {
            n.source = n.source.and_then(|id| map.get(&id).copied());
            image_paints(&mut n.fills, &images);
            image_paints(&mut n.strokes, &images);
            for r in [&mut n.styles.fill, &mut n.styles.stroke, &mut n.styles.text, &mut n.styles.grid, &mut n.styles.effect] {
                *r = r.take().map(|id| format!("{prefix}{id}"));
            }
            if let NodeKind::Text(text) = &mut n.kind {
                text.map_text_styles(|id| Some(format!("{prefix}{id}")));
            }
            bindings(&mut n.bound, &prefix);
            n.var_modes = std::mem::take(&mut n.var_modes).into_iter().map(|(c, m)| (format!("{prefix}{c}"), m)).collect();
            match &mut n.kind {
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
    Ok(result)
}

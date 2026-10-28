//! The project's board: where it lives, opening or making it, and reading cards and selection.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use figcraft_engine::doc::{Document, NodeId};
use figcraft_ui_egui::FigApp;
use serde::Serialize;
use serde_json::{Value, json};

use crate::OpenReport;

/// FigCraft's native file extension.
pub const BOARD_EXT: &str = "figcraft";
/// FigCraft's left rail (the bare canvas lays it out left of the drawable).
pub(crate) const RAIL_WIDTH: f32 = 56.0;
/// How far up from a selected layer its card is looked for.
const MAX_DEPTH: usize = 64;

/// An EffectCraft card on the board.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardRef {
    /// The card's node id on the board.
    pub node: u64,
    /// The card's layer name.
    pub name: String,
    /// The comp it shows (its name in EffectCraft) and its item id (0: unknown).
    pub comp: String,
    pub comp_id: u64,
    /// The `.ecproj` it comes from.
    pub project: String,
    /// The stretch shown, comp seconds, and where comp time 0 sits on the edit's timeline.
    pub start: f64,
    pub end: f64,
    pub offset: f64,
}

/// Any node on the board.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeRef {
    pub id: u64,
    pub name: String,
    /// FigCraft's type name (`FRAME`, `SECTION`, `TEXT`, …).
    pub kind: String,
}

/// `<project stem>.board.figcraft` in the project's folder.
pub fn board_path(ecproj: &Path) -> PathBuf {
    let stem = ecproj.file_stem().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "project".into());
    ecproj.with_file_name(format!("{stem}.board.{BOARD_EXT}"))
}

/// Show the board: the open tab, else the file, else a new file; build it when it has no card.
pub(crate) fn open(fig: &mut FigApp, ctx: &egui::Context, ecproj: &Path, board: &Path) -> Result<OpenReport, String> {
    let board_s = board.to_string_lossy().to_string();
    let mut created = false;
    if let Some(i) = open_index(fig, board) {
        fig.session.active = i;
    } else if board.exists() {
        exec(fig, ctx, "file.open", json!({"path": board_s}))?;
        if let Some(i) = open_index(fig, board) {
            fig.session.active = i;
        }
    } else {
        let stem = ecproj.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        exec(fig, ctx, "file.new", json!({"name": format!("{stem} board")}))?;
        created = true;
    }
    fig.ui.home = false;
    let empty = fig.session.doc().map(|d| !has_cards(d)).unwrap_or(true);
    let auto_main = empty.then(|| exec(fig, ctx, "board.autoMain", json!({"ecproj": ecproj.to_string_lossy()})));
    if let Some(Err(e)) = &auto_main {
        log::warn!("embedded FigCraft: board.autoMain: {e}");
    }
    if created {
        // Saved now, so it has its path and FigCraft's auto-save keeps it from here.
        exec(fig, ctx, "file.save", json!({"path": board_s}))?;
    }
    Ok(OpenReport { board: board.to_path_buf(), created, auto_main })
}

fn exec(fig: &mut FigApp, ctx: &egui::Context, command: &str, params: Value) -> Result<Value, String> {
    crate::run_command(fig, ctx, command, &params)
}

/// The open document saved at `board`.
fn open_index(fig: &FigApp, board: &Path) -> Option<usize> {
    fig.session.docs.iter().position(|d| d.path.as_deref().is_some_and(|p| Path::new(p) == board))
}

/// Whether the board holds an EffectCraft card.
pub(crate) fn has_cards(doc: &Document) -> bool {
    doc.nodes().any(|n| n.effectcraft.is_some())
}

/// The card `id` is, or is inside.
pub(crate) fn card_of(doc: &Document, id: NodeId) -> Option<CardRef> {
    let mut at = Some(id);
    for _ in 0..MAX_DEPTH {
        let node = doc.node(at?)?;
        if let Some(c) = node.effectcraft.as_deref() {
            return Some(CardRef {
                node: node.id.0,
                name: node.name.clone(),
                comp: c.comp.clone(),
                comp_id: c.comp_id,
                project: c.project.clone(),
                start: c.start,
                end: c.end,
                offset: c.offset,
            });
        }
        at = node.parent;
    }
    None
}

pub(crate) fn node_ref(doc: &Document, id: NodeId) -> Option<NodeRef> {
    let n = doc.node(id)?;
    Some(NodeRef { id: n.id.0, name: n.name.clone(), kind: n.kind.type_name().to_string() })
}

/// The cards in the selection (each once, in selection order) and the selected nodes.
pub(crate) fn selection(fig: &FigApp) -> (Vec<CardRef>, Vec<NodeRef>) {
    let Ok(ds) = fig.session.ds() else { return (Vec::new(), Vec::new()) };
    let mut cards: Vec<CardRef> = Vec::new();
    for id in &ds.selection {
        if let Some(c) = card_of(&ds.doc, *id)
            && !cards.iter().any(|k| k.node == c.node)
        {
            cards.push(c);
        }
    }
    let nodes = ds.selection.iter().filter_map(|id| node_ref(&ds.doc, *id)).collect();
    (cards, nodes)
}

/// The card and node under the pointer (the canvas's hover), else the selection's first.
pub(crate) fn under_pointer(fig: &FigApp) -> (Option<CardRef>, Option<NodeRef>) {
    let Ok(ds) = fig.session.ds() else { return (None, None) };
    let Some(id) = ds.hover.or_else(|| ds.selection.first().copied()) else { return (None, None) };
    (card_of(&ds.doc, id), node_ref(&ds.doc, id))
}

/// Save every open file that has a path and unsaved changes.
pub(crate) fn save_all(fig: &mut FigApp) {
    let active = fig.session.active;
    for i in 0..fig.session.docs.len() {
        let dirty = fig.session.docs.get(i).is_some_and(|d| d.path.is_some() && d.is_dirty());
        if dirty {
            fig.session.active = i;
            if let Err(e) = fig.session.execute("file.save", &json!({})) {
                log::warn!("embedded FigCraft: saving the board: {e}");
            }
        }
    }
    fig.session.active = active.min(fig.session.docs.len().saturating_sub(1));
}

/// The marks of a `canvas.annotate.list` answer (`{marks: [...]}` or a bare list) not handed over
/// yet, oldest first, and the highest numeric id among them.
pub(crate) fn fresh_marks(listed: &Value, seen: &mut HashSet<String>) -> (Vec<Value>, u64) {
    let marks = listed.get("marks").and_then(Value::as_array).or_else(|| listed.as_array());
    let mut fresh = Vec::new();
    let mut max = 0;
    for m in marks.into_iter().flatten() {
        if !m.is_object() {
            continue;
        }
        let id = match m.get("id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) => n.to_string(),
            _ => continue,
        };
        if let Some(n) = m.get("id").and_then(Value::as_u64) {
            max = max.max(n);
        }
        if seen.insert(id) {
            fresh.push(m.clone());
        }
    }
    (fresh, max)
}

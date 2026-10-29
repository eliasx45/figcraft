//! Fork: the MAIN board — Ghostex's EDITING canvas (approved by Elias 28 Oct 2026: "perfect i love
//! it as a start. go ahead"; the clickable mock is EffectCraft's `plan/figcraft-canvas-playground.html`).
//!
//! An EffectCraft project's MAIN comp laid out on the page as a board: one row per chapter (MAIN's
//! markers), one live EffectCraft card per scene comp MAIN shows in that stretch, in time order,
//! labelled "S1 06 Old vs AI · 1:02". Around it, commands for the host (Ghostex), Claude over MCP
//! and the UI alike:
//!
//! - `board.autoMain` builds or updates the board ([`auto_main`]); `board.state` and
//!   `board.events {since}` are what a host polls (rows, cards, selection, view, armed tool; a
//!   `select` event whenever the selected card changes); `board.agentView {card}` is what Claude
//!   reads for a card; `canvas.flyTo` glides the view to a card, a comp, a chapter or the MAIN
//!   scene at a time.
//! - `canvas.annotate.*` ([`marks`]): Pen / Comment / Layer review marks drawn on a card, in its
//!   comp's pixels, queued here (session state, never the document) for the host to take.
//! - `card.flash` ([`flash`]): a card flashes when Claude edits its comp, with a Before / After badge.
//! - `board.storyboard` / `board.review` ([`boards`]): boards beside a row or a card.
//! - `canvas.presence.*` ([`presence`]): agents shown on the cards they work on, pen.dev style.
//!
//! The engine knows no EffectCraft: what a board needs from the project comes from an
//! [`EcHost`] the desktop app installs (FigCraft's in-process EffectCraft, `figcraft_ec_inproc`'s
//! `figcraft.board`, `figcraft.speech`, `figcraft.mark`, `figcraft.layerAt`, `figcraft.comp`,
//! `figcraft.moves`). Without one, commands that need the project say so.
//!
//! Board layers are ordinary layers on the page tagged in `plugin_data` ([`KIND`], [`PROJECT`],
//! [`KEY`], [`INFO`]), so a saved file keeps its board and `board.autoMain` finds it again.

#[path = "ecboard_boards.rs"]
pub mod boards;
#[path = "ecboard_flash.rs"]
pub mod flash;
#[path = "ecboard_marks.rs"]
pub mod marks;
#[path = "ecboard_presence.rs"]
pub mod presence;

use std::collections::VecDeque;

use figcraft_color::{Color, Paint};
use figcraft_doc::{Document, EcComp, Node, NodeId, NodeKind, TextAlignH, TextAutoResize, TextData};
use figcraft_geom::{Affine, Rect};
use serde_json::{Value, json};

use crate::cmd::CommandSpec;
use crate::params::{boolean, get, id_of, num, opt_id, string};
use crate::{EngineError, Result, Session, bad, spec};

/// What the board asks of EffectCraft (see the module doc). `project` is the `.ecproj` path.
pub trait EcHost: Send {
    fn call(&mut self, project: &str, method: &str, params: &Value) -> std::result::Result<Value, String>;
}

/// `plugin_data` keys of board layers.
pub const KIND: &str = "ecboard.kind";
pub const PROJECT: &str = "ecboard.project";
pub const KEY: &str = "ecboard.key";
pub const INFO: &str = "ecboard.info";

/// Card width (page units) and the room round the cards.
pub const CARD_W: f64 = 480.0;
const GAP: f64 = 44.0;
/// The row header's column, left of the cards.
const HEAD_W: f64 = 420.0;
/// Room under a row's cards (their names, the next row's gap).
const ROW_PAD: f64 = 170.0;
/// The board's title above the first row.
const TITLE_H: f64 = 120.0;
/// Room between other content on the page and a new board.
const AWAY: f64 = 400.0;
/// The most events kept for `board.events`.
const MAX_EVENTS: usize = 256;
/// The view's largest zoom after a fly (as the mock's 1.6).
const FLY_MAX_ZOOM: f64 = 1.6;

const TITLE_INK: Color = Color::from_rgb8_const(0xEE, 0xEE, 0xEE);
const HEAD_INK: Color = Color::from_rgb8_const(0xD8, 0xDC, 0xE4);

/// One thing a host may want to know about.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub seq: u64,
    /// `select`, `board`, `mark`, `marksRemoved`, `armed`, `flash`, `storyboard`, `review`,
    /// `presence.hover`, `presence.open`.
    pub kind: String,
    pub data: Value,
}

/// The board's session state (never in the document).
#[derive(Default)]
pub struct State {
    /// Set by the desktop app (FigCraft's EffectCraft build).
    pub host: Option<Box<dyn EcHost>>,
    pub marks: marks::Marks,
    pub flash: flash::Flashes,
    /// Agents shown on the board (`canvas.presence.*`).
    pub presence: presence::Presence,
    events: VecDeque<Event>,
    next_seq: u64,
    /// Bumped with every event.
    pub revision: u64,
    /// The selection last seen (a change is a `select` event).
    seen_selection: Option<(u64, Vec<NodeId>)>,
}

impl State {
    pub fn push(&mut self, kind: &str, data: Value) {
        self.next_seq = self.next_seq.saturating_add(1);
        self.revision = self.revision.wrapping_add(1);
        self.events.push_back(Event { seq: self.next_seq, kind: kind.to_string(), data });
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    pub fn last_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn events_since(&self, since: u64) -> impl Iterator<Item = &Event> {
        self.events.iter().filter(move |e| e.seq > since)
    }
}

/// Call the host for `project`.
pub fn host_call(s: &mut Session, cmd: &str, project: &str, method: &str, params: &Value) -> Result<Value> {
    let host =
        s.ecboard.host.as_mut().ok_or_else(|| EngineError::Disabled(cmd.into(), "needs FigCraft's EffectCraft build (effectcraft-inproc)".into()))?;
    host.call(project, method, params).map_err(|e| EngineError::Disabled(cmd.into(), e))
}

// ------------------------------------------------------------------------------------------
// Reading the board back from the page

fn kind_of(n: &Node) -> Option<&str> {
    n.plugin_data.get(KIND).map(String::as_str)
}

/// A board card: its layer, comp and where it sits in MAIN.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: NodeId,
    pub comp: EcComp,
    pub row: usize,
    pub main: (f64, f64),
    pub parent: String,
    pub label: String,
}

impl Card {
    /// MAIN seconds of comp second `t`.
    pub fn main_of(&self, t: f64) -> f64 {
        self.comp.offset + t / self.comp.rate.max(1e-9)
    }
    /// Comp seconds of MAIN second `m`.
    pub fn comp_of(&self, m: f64) -> f64 {
        (m - self.comp.offset) * self.comp.rate
    }
}

/// One row (chapter).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub index: usize,
    pub id: NodeId,
    pub name: String,
    pub start: f64,
    pub end: f64,
}

/// The board on a page, as its layers say.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Board {
    pub title: Option<NodeId>,
    pub project: String,
    /// MAIN as `figcraft.board` described it.
    pub main: Value,
    pub rows: Vec<Row>,
    pub cards: Vec<Card>,
}

fn info(n: &Node) -> Value {
    n.plugin_data.get(INFO).and_then(|t| serde_json::from_str(t).ok()).unwrap_or(Value::Null)
}

fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite())
}

/// The board of `project` on `page` (any project's when `project` is empty: the first found).
pub fn read(doc: &Document, page: NodeId, project: &str) -> Board {
    let mut b = Board::default();
    let Some(p) = doc.node(page) else { return b };
    let want = |n: &Node| project.is_empty() || n.plugin_data.get(PROJECT).is_some_and(|x| x == project);
    let mut project = project.to_string();
    for id in &p.children {
        let Some(n) = doc.node(*id) else { continue };
        let Some(kind) = kind_of(n) else { continue };
        if !want(n) && !(project.is_empty() && kind == "title") {
            continue;
        }
        if project.is_empty() {
            project = n.plugin_data.get(PROJECT).cloned().unwrap_or_default();
        }
        if n.plugin_data.get(PROJECT).map(String::as_str) != Some(project.as_str()) {
            continue;
        }
        let i = info(n);
        match kind {
            "title" => {
                b.title = Some(*id);
                b.main = i;
            }
            "row" => b.rows.push(Row {
                index: i.get("index").and_then(Value::as_u64).unwrap_or(0) as usize,
                id: *id,
                name: i.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                start: f(&i, "start").unwrap_or(0.0),
                end: f(&i, "end").unwrap_or(0.0),
            }),
            "card" => {
                let Some(c) = n.effectcraft.as_deref() else { continue };
                b.cards.push(Card {
                    id: *id,
                    comp: c.clone(),
                    row: i.get("row").and_then(Value::as_u64).unwrap_or(0) as usize,
                    main: (f(&i, "mainStart").unwrap_or(0.0), f(&i, "mainEnd").unwrap_or(0.0)),
                    parent: i.get("parent").and_then(Value::as_str).unwrap_or_default().to_string(),
                    label: n.name.clone(),
                });
            }
            _ => {}
        }
    }
    b.project = project;
    b.rows.sort_by_key(|r| r.index);
    b.cards.sort_by(|a, c| a.row.cmp(&c.row).then(a.main.0.partial_cmp(&c.main.0).unwrap_or(std::cmp::Ordering::Equal)));
    b
}

/// The active page's board (of `project`, or the first one).
pub fn board_of(s: &Session, project: &str) -> Result<Board> {
    let ds = s.ds()?;
    Ok(read(&ds.doc, ds.page, project))
}

/// A board card by layer id.
pub fn card_by_id(s: &Session, id: NodeId) -> Option<Card> {
    let doc = s.doc().ok()?;
    let page = doc.page_of(id)?;
    let project = doc.node(id)?.plugin_data.get(PROJECT)?.clone();
    read(doc, page, &project).cards.into_iter().find(|c| c.id == id)
}

/// `m:ss` of `t` seconds.
pub fn clock(t: f64) -> String {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let s = t.floor() as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `m:ss:ff` of `t` seconds at `fps`.
pub fn timecode(t: f64, fps: f64) -> String {
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 30.0 };
    let s = t.floor() as i64;
    let fr = ((t - t.floor()) * fps).floor() as i64;
    format!("{}:{:02}:{:02}", s / 60, s % 60, fr)
}

/// MAIN's frame rate on this board (29.97 when not known).
pub fn main_fps(b: &Board) -> f64 {
    f(&b.main, "fps").filter(|x| *x > 0.0).unwrap_or(29.97)
}

/// The EffectCraft project a command means: `ecproj`/`project`, else the selected card's, else
/// the page's board's, else the first card's on the page.
pub fn project_param(cmd: &str, s: &Session, p: &Value) -> Result<String> {
    for k in ["ecproj", "project"] {
        match get(p, k) {
            Some(Value::String(x)) if !x.trim().is_empty() && !x.contains('\0') && x.len() <= 4096 => return Ok(x.clone()),
            Some(Value::Null) | None => {}
            Some(_) => return Err(bad(cmd, format!("`{k}` must be the path of an .ecproj"))),
        }
    }
    let doc = s.doc()?;
    let of = |id: NodeId| doc.node(id).and_then(|n| n.effectcraft.as_deref()).map(|c| c.project.clone()).filter(|x| !x.trim().is_empty());
    if let Some(x) = s.selection().into_iter().find_map(of) {
        return Ok(x);
    }
    let page = s.page()?;
    let b = read(doc, page, "");
    if !b.project.is_empty() {
        return Ok(b.project);
    }
    let first = doc.nodes().filter(|n| doc.page_of(n.id) == Some(page)).find_map(|n| of(n.id));
    first.ok_or_else(|| bad(cmd, "pass `ecproj` (the EffectCraft project): no card on this page names one"))
}

// ------------------------------------------------------------------------------------------
// board.autoMain

/// A scene as `figcraft.board` gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub comp: u64,
    pub name: String,
    pub parent: String,
    pub main: (f64, f64),
    pub comp_range: (f64, f64),
    pub offset: f64,
    pub rate: f64,
    pub size: (u32, u32),
    pub fps: f64,
}

impl Scene {
    pub fn key(&self) -> String {
        format!("{}@{:.3}", self.comp, self.main.0)
    }
}

/// The scenes of a `figcraft.board` answer.
pub fn scenes_of(v: &Value) -> Vec<Scene> {
    v.get("scenes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(1024)
        .filter_map(|s| {
            let (a, b) = (f(s, "mainStart")?, f(s, "mainEnd")?);
            let (ca, cb) = (f(s, "compStart")?, f(s, "compEnd")?);
            if !(b > a && cb > ca) {
                return None;
            }
            let dim = |k: &str| s.get(k).and_then(Value::as_u64).map_or(1920, |x| x.clamp(1, 32_768) as u32);
            Some(Scene {
                comp: s.get("comp").and_then(Value::as_u64)?,
                name: s.get("compName").and_then(Value::as_str)?.to_string(),
                parent: s.get("parent").and_then(Value::as_str).unwrap_or_default().to_string(),
                main: (a, b),
                comp_range: (ca, cb),
                offset: f(s, "offset").unwrap_or(a - ca),
                rate: f(s, "rate").filter(|r| *r > 0.0).unwrap_or(1.0),
                size: (dim("width"), dim("height")),
                fps: f(s, "fps").filter(|x| *x > 0.0).unwrap_or(29.97),
            })
        })
        .collect()
}

/// The chapters of a `figcraft.board` answer: `(start, end, name)`; one row for all of MAIN
/// without any.
pub fn chapters_of(v: &Value) -> Vec<(f64, f64, String)> {
    let mut out: Vec<(f64, f64, String)> = v
        .get("chapters")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(128)
        .filter_map(|c| Some((f(c, "time")?, f(c, "end")?, c.get("name").and_then(Value::as_str).unwrap_or("Chapter").to_string())))
        .collect();
    if out.is_empty() {
        let dur = v.get("main").and_then(|m| f(m, "duration")).unwrap_or(f64::MAX);
        out.push((0.0, dur, v.get("main").and_then(|m| m.get("name")).and_then(Value::as_str).unwrap_or("MAIN").to_string()));
    }
    out
}

/// The row a scene goes in: the chapter holding its middle (before the first: the first; after
/// the last: the last).
pub fn row_of(chapters: &[(f64, f64, String)], scene: &Scene) -> usize {
    let mid = (scene.main.0 + scene.main.1) / 2.0;
    chapters
        .iter()
        .position(|(a, b, _)| mid >= *a && mid < *b)
        .unwrap_or_else(|| if chapters.first().is_some_and(|c| mid < c.0) { 0 } else { chapters.len().saturating_sub(1) })
}

/// "INTRO", "what this video covers" of "INTRO · what this video covers".
fn split_name(n: &str) -> (&str, &str) {
    match n.split_once(" · ") {
        Some((a, b)) => (a.trim(), b.trim()),
        None => (n.trim(), ""),
    }
}

fn text_node(doc: &mut Document, name: &str, text: &str, size: f64, color: Color, width: f64) -> Node {
    let mut td = TextData { characters: text.to_string(), auto_resize: TextAutoResize::Height, align_h: TextAlignH::Left, ..TextData::default() };
    td.style.font_size = size;
    let mut n = Node::new(doc.alloc_id(), NodeKind::Text(td), name, width.max(1.0), size * 1.4);
    n.fills = vec![Paint::solid(color)];
    n
}

/// Fit a text layer's height to its text.
pub(crate) fn fit_text(doc: &mut Document, id: NodeId) {
    if let Some(n) = doc.node(id)
        && let Some((w, h)) = figcraft_text::auto_size(n)
        && let Some(n) = doc.node_mut(id)
    {
        (n.width, n.height) = (w, h);
    }
}

fn tag(n: &mut Node, kind: &str, project: &str, key: &str, info: &Value) {
    n.plugin_data.insert(KIND.into(), kind.into());
    n.plugin_data.insert(PROJECT.into(), project.into());
    n.plugin_data.insert(KEY.into(), key.into());
    n.plugin_data.insert(INFO.into(), info.to_string());
}

/// The page box of what is on `page` (none: `None`).
pub(crate) fn content_bounds(doc: &Document, page: NodeId, skip: &dyn Fn(&Node) -> bool) -> Option<Rect> {
    let p = doc.node(page)?;
    p.children.iter().filter(|c| doc.node(**c).is_some_and(|n| !skip(n))).filter_map(|c| doc.world_bounds(*c)).reduce(|a, b| a.union(b))
}

fn auto_main(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.autoMain";
    let project = project_param(cmd, s, p)?;
    let mut q = json!({});
    if let Some(c) = get(p, "comp").filter(|v| v.is_string() || v.is_number()) {
        q["comp"] = c.clone();
    }
    let answer = host_call(s, cmd, &project, "figcraft.board", &q)?;
    let main = answer.get("main").cloned().unwrap_or(Value::Null);
    if !main.is_object() {
        return Err(bad(cmd, "EffectCraft didn't describe MAIN"));
    }
    let chapters = chapters_of(&answer);
    let scenes = scenes_of(&answer);
    // Each card's still: its scene's peak (a board.json beat, else the longest stressed word),
    // never its first frame (often black). No speech map: board.json beats only.
    let beats = answer.get("beats").and_then(Value::as_array).cloned().unwrap_or_default();
    let words = host_call(s, cmd, &project, "figcraft.speech", &q).ok().map(|v| words_of(&v)).unwrap_or_default();
    let page = s.page()?;
    let before = board_of(s, &project)?;
    let (vx, vy) = s.ds().map(|d| (d.view.cx, d.view.cy))?;
    let doc = s.doc_mut()?;
    // Where the board starts: where it is, else x / y, else right of what's on the page.
    let origin = match before.title.and_then(|t| doc.world_bounds(t)) {
        Some(r) => (r.x0, r.y0),
        None => match (num(p, "x").filter(|v| v.is_finite()), num(p, "y").filter(|v| v.is_finite())) {
            (Some(x), Some(y)) => (x, y),
            _ => match content_bounds(doc, page, &|_| false) {
                Some(r) => (r.x1 + AWAY, r.y0),
                None => (vx - 800.0, vy - 400.0),
            },
        },
    };
    let (ox, oy) = (origin.0.clamp(-1e8, 1e8), origin.1.clamp(-1e8, 1e8));
    let aspect = main.get("height").and_then(Value::as_f64).unwrap_or(1080.0) / main.get("width").and_then(Value::as_f64).unwrap_or(1920.0).max(1.0);
    let card_h = (CARD_W * if aspect.is_finite() && aspect > 0.0 { aspect } else { 9.0 / 16.0 }).clamp(40.0, 4.0 * CARD_W);
    let row_h = card_h + ROW_PAD;
    // Rows and their scenes in time order.
    let mut rows: Vec<Vec<&Scene>> = vec![Vec::new(); chapters.len()];
    for sc in &scenes {
        if let Some(r) = rows.get_mut(row_of(&chapters, sc)) {
            r.push(sc);
        }
    }
    // The title.
    let project_name = std::path::Path::new(&project).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    let main_name = main.get("name").and_then(Value::as_str).unwrap_or("MAIN").to_string();
    let title_text = format!("{main_name} board · {project_name}");
    let title = match before.title.filter(|t| doc.node(*t).is_some()) {
        Some(t) => {
            if let Some(n) = doc.node_mut(t)
                && let NodeKind::Text(td) = &mut n.kind
            {
                td.characters = title_text.clone();
            }
            t
        }
        None => {
            let mut n = text_node(doc, "MAIN board", &title_text, 40.0, TITLE_INK, 1600.0);
            n.transform = Affine::translate((ox, oy));
            let id = n.id;
            doc.insert(n, page, None)?;
            id
        }
    };
    if let Some(n) = doc.node_mut(title) {
        n.transform = Affine::translate((ox, oy));
        tag(n, "title", &project, "title", &main);
    }
    fit_text(doc, title);
    // Rows.
    let mut out_rows = Vec::new();
    let mut kept_rows: Vec<NodeId> = Vec::new();
    for (i, (a, b, name)) in chapters.iter().enumerate() {
        let y = oy + TITLE_H + i as f64 * row_h;
        let (head, sub) = split_name(name);
        let count = rows.get(i).map_or(0, Vec::len);
        let text =
            format!("{head}\n{sub}{}{} · {count} scene{}", if sub.is_empty() { "" } else { "\n" }, clock(*a), if count == 1 { "" } else { "s" });
        let info = json!({"index": i, "name": name, "start": a, "end": b});
        let existing = before.rows.iter().find(|r| r.index == i && doc.node(r.id).is_some()).map(|r| r.id);
        let id = match existing {
            Some(id) => {
                if let Some(n) = doc.node_mut(id)
                    && let NodeKind::Text(td) = &mut n.kind
                {
                    td.characters = text.clone();
                }
                id
            }
            None => {
                let n = text_node(doc, head, &text, 26.0, HEAD_INK, HEAD_W - 60.0);
                let id = n.id;
                doc.insert(n, page, None)?;
                id
            }
        };
        if let Some(n) = doc.node_mut(id) {
            n.name = head.chars().take(64).collect();
            n.transform = Affine::translate((ox, y + 20.0));
            tag(n, "row", &project, &format!("row:{i}"), &info);
        }
        fit_text(doc, id);
        kept_rows.push(id);
        out_rows.push((i, id, name.clone(), *a, *b));
    }
    for r in &before.rows {
        if !kept_rows.contains(&r.id) && doc.node(r.id).is_some() {
            doc.remove(r.id)?;
        }
    }
    // Cards.
    let mut added = Vec::new();
    let mut kept: Vec<NodeId> = Vec::new();
    let mut row_cards: Vec<Vec<NodeId>> = vec![Vec::new(); chapters.len()];
    for (ri, list) in rows.iter().enumerate() {
        let y = oy + TITLE_H + ri as f64 * row_h;
        for (k, sc) in list.iter().enumerate() {
            let x = ox + HEAD_W + k as f64 * (CARD_W + GAP);
            let mut c = EcComp::new(sc.name.clone(), sc.comp_range.0, sc.comp_range.1, sc.fps);
            c.comp_id = sc.comp;
            c.project = project.clone();
            (c.width, c.height) = sc.size;
            c.offset = sc.offset;
            c.rate = sc.rate;
            c.peak = stress_of(&beats, &words, sc.main.0, sc.main.1).map(|(_, t, _)| (t - sc.offset) * sc.rate);
            c.sanitize();
            let h = (CARD_W * f64::from(c.height) / f64::from(c.width.max(1))).clamp(9.0, 4.0 * CARD_W);
            let label = format!("{} · {}", sc.name, clock(sc.main.0));
            let info = json!({"row": ri, "mainStart": sc.main.0, "mainEnd": sc.main.1, "parent": sc.parent, "chapter": chapters.get(ri).map(|c| c.2.clone())});
            let existing = before
                .cards
                .iter()
                .find(|bc| doc.node(bc.id).and_then(|n| n.plugin_data.get(KEY)).is_some_and(|key| *key == sc.key()))
                .map(|bc| bc.id);
            let id = match existing.filter(|id| !kept.contains(id)) {
                Some(id) => {
                    if let Some(n) = doc.node_mut(id) {
                        // The person's sound choice stays.
                        c.sound = n.effectcraft.as_deref().is_none_or(|old| old.sound);
                        // A still that moved: the saved poster (another moment) goes, so the
                        // app takes it again at the new still.
                        if n.effectcraft.as_deref().is_none_or(|old| old.still() != c.still()) {
                            n.fills.retain(|f| !matches!(f.kind, figcraft_color::PaintKind::Image(_)));
                        }
                        n.effectcraft = Some(Box::new(c));
                    }
                    id
                }
                None => {
                    let nid = doc.alloc_id();
                    let n = crate::cmd::effectcraft::card_node(nid, c, label.clone(), CARD_W, h);
                    doc.insert(n, page, None)?;
                    added.push(nid);
                    nid
                }
            };
            if let Some(n) = doc.node_mut(id) {
                n.name = label;
                n.transform = Affine::translate((x, y));
                (n.width, n.height) = (CARD_W, h);
                tag(n, "card", &project, &sc.key(), &info);
            }
            kept.push(id);
            if let Some(r) = row_cards.get_mut(ri) {
                r.push(id);
            }
        }
    }
    let mut removed = 0usize;
    for bc in &before.cards {
        if !kept.contains(&bc.id) && doc.node(bc.id).is_some() {
            doc.remove(bc.id)?;
            removed += 1;
        }
    }
    let rows_json: Vec<Value> = out_rows
        .iter()
        .map(|(i, id, name, a, b)| json!({"index": i, "id": id, "name": name, "start": a, "end": b, "cards": row_cards.get(*i).cloned().unwrap_or_default()}))
        .collect();
    let result = json!({
        "project": project, "main": main, "title": title, "rows": rows_json,
        "cards": kept.len(), "added": added, "removed": removed, "kept": kept.len() - added.len(),
    });
    s.ecboard.push("board", json!({"project": project, "cards": kept.len(), "added": added.len(), "removed": removed}));
    Ok(result)
}

// ------------------------------------------------------------------------------------------
// board.state / board.events

/// Note a change of the selected cards (a `select` event). Commands and the UI call it.
pub fn observe(s: &mut Session) {
    let Ok(ds) = s.ds() else { return };
    let now = (ds.uid, ds.selection.clone());
    if s.ecboard.seen_selection.as_ref() == Some(&now) {
        return;
    }
    let is_card = |id: &NodeId| ds.doc.node(*id).is_some_and(|n| n.effectcraft.is_some());
    let card = now.1.iter().copied().find(is_card);
    let first = s.ecboard.seen_selection.is_none();
    let had_card = s.ecboard.seen_selection.as_ref().is_some_and(|(_, sel)| sel.iter().any(is_card));
    s.ecboard.seen_selection = Some(now);
    if card.is_none() && (first || !had_card) {
        return;
    }
    let data = match card {
        Some(id) => match card_by_id(s, id) {
            Some(c) => card_json(&c, id),
            None => match s.doc().ok().and_then(|d| d.node(id)).and_then(|n| n.effectcraft.as_deref()) {
                Some(c) => json!({"card": id, "comp": c.comp, "compId": c.comp_id, "project": c.project}),
                None => json!({"card": null}),
            },
        },
        None => json!({"card": null}),
    };
    s.ecboard.push("select", data);
}

fn card_json(c: &Card, id: NodeId) -> Value {
    json!({"card": id, "label": c.label, "comp": c.comp.comp, "compId": c.comp.comp_id, "project": c.comp.project,
        "row": c.row, "mainStart": c.main.0, "mainEnd": c.main.1, "parent": c.parent})
}

fn state(s: &mut Session, p: &Value) -> Result<Value> {
    observe(s);
    let project = match get(p, "ecproj").or_else(|| get(p, "project")) {
        Some(Value::String(x)) => x.clone(),
        _ => String::new(),
    };
    let b = board_of(s, &project)?;
    let doc = s.doc()?;
    let rows: Vec<Value> = b
        .rows
        .iter()
        .map(|r| {
            let cards: Vec<Value> = b
                .cards
                .iter()
                .filter(|c| c.row == r.index)
                .map(|c| {
                    let bx = doc.world_bounds(c.id);
                    let mut j = card_json(c, c.id);
                    j["bounds"] = json!(bx.map(|r| [r.x0, r.y0, r.width(), r.height()]));
                    j
                })
                .collect();
            json!({"index": r.index, "id": r.id, "name": r.name, "start": r.start, "end": r.end, "cards": cards})
        })
        .collect();
    let sel = s.selection();
    let card = sel.iter().copied().find(|id| doc.node(*id).is_some_and(|n| n.effectcraft.is_some()));
    let selected = card.and_then(|id| b.cards.iter().find(|c| c.id == id).map(|c| card_json(c, id)));
    let ds = s.ds()?;
    let v = &ds.view;
    Ok(json!({
        "revision": s.ecboard.revision,
        "lastEvent": s.ecboard.last_seq(),
        "project": b.project,
        "main": b.main,
        "title": b.title,
        "rows": rows,
        "cards": b.cards.len(),
        "selection": {"ids": sel, "card": card, "board": selected},
        "view": {"cx": v.cx, "cy": v.cy, "zoom": v.zoom, "width": v.vw, "height": v.vh},
        "armed": s.ecboard.marks.armed.map(marks::Tool::id),
        "marks": {"count": s.ecboard.marks.list.len(), "last": s.ecboard.marks.last_id()},
        "playhead": crate::cmd::effectcraft::transport_json(s),
    }))
}

fn events(s: &mut Session, p: &Value) -> Result<Value> {
    observe(s);
    let since = num(p, "since").filter(|v| v.is_finite() && *v >= 0.0).map_or(0, |v| v.min(u64::MAX as f64) as u64);
    let list: Vec<Value> = s.ecboard.events_since(since).map(|e| json!({"seq": e.seq, "kind": e.kind, "data": e.data})).collect();
    Ok(json!({"last": s.ecboard.last_seq(), "revision": s.ecboard.revision, "events": list}))
}

// ------------------------------------------------------------------------------------------
// canvas.flyTo

/// The view `(cx, cy, zoom)` that shows page box `r` in a `vw`×`vh` viewport, zoomed to fit with
/// a margin, never past `max_zoom`.
pub fn view_for(r: Rect, viewport: (f64, f64), max_zoom: f64) -> Option<(f64, f64, f64)> {
    let (vw, vh) = viewport;
    let (vw, vh) = (if vw.is_finite() && vw > 1.0 { vw } else { 1600.0 }, if vh.is_finite() && vh > 1.0 { vh } else { 1000.0 });
    if ![r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()) {
        return None;
    }
    let pad = 40.0_f64.min(vw / 4.0).min(vh / 4.0);
    let z = ((vw - 2.0 * pad) / r.width().max(1.0)).min((vh - 2.0 * pad) / r.height().max(1.0));
    let max = if max_zoom.is_finite() && max_zoom > 0.0 { max_zoom } else { FLY_MAX_ZOOM };
    let z = z.min(max).clamp(crate::ViewState::MIN_ZOOM, crate::ViewState::MAX_ZOOM);
    let c = r.center();
    Some((c.x, c.y, z))
}

/// The board card showing MAIN second `t` (the latest-starting one when scenes overlap).
pub fn card_at_time(b: &Board, t: f64) -> Option<&Card> {
    b.cards.iter().filter(|c| t >= c.main.0 && t < c.main.1).max_by(|a, c| a.main.0.partial_cmp(&c.main.0).unwrap_or(std::cmp::Ordering::Equal))
}

fn fly_to(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "canvas.flyTo";
    let b = board_of(s, "")?;
    let doc = s.doc()?;
    let card_box = |id: NodeId| doc.world_bounds(id).map(|r| Rect::new(r.x0 - 20.0, r.y0 - 20.0, r.x1 + 20.0, r.y1 + 60.0));
    let (rect, card, what) = if let Some(id) = opt_id(cmd, p, "card")?.or(opt_id(cmd, p, "id")?) {
        (card_box(id).ok_or_else(|| bad(cmd, "no such card"))?, Some(id), json!({"card": id}))
    } else if let Some(c) = get(p, "comp").filter(|v| !v.is_null()) {
        let (name, cid) = (c.as_str().map(str::to_string), c.as_u64());
        let hit = b
            .cards
            .iter()
            .find(|k| cid == Some(k.comp.comp_id) || name.as_deref().is_some_and(|n| n == k.comp.comp))
            .map(|k| k.id)
            .or_else(|| {
                doc.nodes()
                    .find(|n| n.effectcraft.as_deref().is_some_and(|k| cid == Some(k.comp_id) || name.as_deref().is_some_and(|x| x == k.comp)))
                    .map(|n| n.id)
            })
            .ok_or_else(|| bad(cmd, "no card shows that comp"))?;
        (card_box(hit).ok_or_else(|| bad(cmd, "no such card"))?, Some(hit), json!({"comp": c}))
    } else if let Some(ch) = get(p, "chapter").filter(|v| !v.is_null()) {
        let row = match ch {
            Value::Number(n) => n.as_u64().and_then(|i| b.rows.iter().find(|r| r.index as u64 == i)),
            Value::String(q) => {
                let q = q.trim().to_lowercase();
                b.rows
                    .iter()
                    .find(|r| r.name.to_lowercase() == q)
                    .or_else(|| b.rows.iter().find(|r| !q.is_empty() && r.name.to_lowercase().contains(&q)))
            }
            _ => None,
        }
        .ok_or_else(|| bad(cmd, "no such chapter on the board"))?;
        let mut r = doc.world_bounds(row.id).ok_or_else(|| bad(cmd, "the chapter's row is gone"))?;
        for c in b.cards.iter().filter(|c| c.row == row.index) {
            if let Some(cb) = doc.world_bounds(c.id) {
                r = r.union(cb);
            }
        }
        let first = b.cards.iter().find(|c| c.row == row.index).map(|c| c.id);
        (Rect::new(r.x0 - 20.0, r.y0 - 20.0, r.x1 + 20.0, r.y1 + 60.0), first, json!({"chapter": row.name}))
    } else if let Some(t) = num(p, "time") {
        if !t.is_finite() {
            return Err(bad(cmd, "`time` must be MAIN seconds"));
        }
        let c = card_at_time(&b, t).ok_or_else(|| bad(cmd, format!("no MAIN scene at {}", clock(t))))?;
        (card_box(c.id).ok_or_else(|| bad(cmd, "no such card"))?, Some(c.id), json!({"time": t, "comp": c.comp.comp}))
    } else {
        return Err(bad(cmd, "pass `card`, `comp`, `chapter` or `time` (MAIN seconds)"));
    };
    let ds = s.ds()?;
    let from = (ds.view.cx, ds.view.cy, ds.view.zoom);
    let max = num(p, "maxZoom").unwrap_or(FLY_MAX_ZOOM);
    let to = view_for(rect, (ds.view.vw, ds.view.vh), max).ok_or_else(|| bad(cmd, "nothing to show"))?;
    let select = boolean(p, "select").unwrap_or(true);
    let ds = s.ds_mut()?;
    ds.view.cx = to.0;
    ds.view.cy = to.1;
    ds.view.zoom = to.2;
    if select && let Some(c) = card {
        ds.selection = vec![c];
    }
    // The view is set already (headless callers see it); the desktop app glides there from `from`.
    s.request_ui("canvas.flyTo", json!({"from": [from.0, from.1, from.2], "to": [to.0, to.1, to.2]}));
    observe(s);
    Ok(json!({"to": {"cx": to.0, "cy": to.1, "zoom": to.2}, "rect": [rect.x0, rect.y0, rect.width(), rect.height()], "card": card, "target": what}))
}

// ------------------------------------------------------------------------------------------
// board.agentView

/// The stressed word of MAIN seconds `a..b`: a board.json beat there, else the longest stressed
/// word. `(word, MAIN time, from)`.
pub fn stress_of(beats: &[Value], words: &[(f64, f64, String, bool)], a: f64, b: f64) -> Option<(String, f64, &'static str)> {
    let beat = beats.iter().find(|x| f(x, "time").is_some_and(|t| t >= a && t < b));
    if let Some(bt) = beat {
        let word = [bt.get("stress"), bt.get("text")]
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|x| !x.trim().is_empty())
            .and_then(|x| x.split_whitespace().next())
            .unwrap_or_default()
            .to_lowercase();
        return Some((word, f(bt, "time").unwrap_or(a), "board.json beat"));
    }
    words
        .iter()
        .filter(|w| w.3 && w.1 > a + 0.05 && w.0 < b)
        .max_by_key(|w| w.2.chars().count())
        .map(|w| (w.2.trim_start_matches('-').to_string(), w.0, "longest stressed word"))
}

/// MAIN's words: `(start, end, word, stressed)`.
pub type Words = Vec<(f64, f64, String, bool)>;

/// The words of a `figcraft.speech` answer.
pub fn words_of(v: &Value) -> Words {
    v.get("words")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let a = w.as_array()?;
            Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2)?.as_str()?.to_string(), a.get(3).and_then(Value::as_bool).unwrap_or(false)))
        })
        .collect()
}

/// Words of MAIN seconds `a..b` as a line ("sixty-day" joins hyphen pieces).
pub fn line_of(words: &[(f64, f64, String, bool)], a: f64, b: f64) -> String {
    words.iter().filter(|w| w.1 > a + 0.05 && w.0 < b).map(|w| w.2.as_str()).collect::<Vec<_>>().join(" ").replace(" -", "-")
}

/// The card a command means: `card`/`id`, else the selected card.
pub fn card_param(cmd: &str, s: &Session, p: &Value) -> Result<NodeId> {
    if let Some(id) = opt_id(cmd, p, "card")?.or(opt_id(cmd, p, "id")?) {
        return Ok(id);
    }
    let doc = s.doc()?;
    s.selection()
        .into_iter()
        .find(|id| doc.node(*id).is_some_and(|n| n.effectcraft.is_some()))
        .ok_or_else(|| EngineError::Disabled(cmd.into(), "no card selected (pass `card`)".into()))
}

fn agent_view(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.agentView";
    let id = card_param(cmd, s, p)?;
    let comp = s.doc()?.node(id).and_then(|n| n.effectcraft.as_deref()).cloned().ok_or_else(|| bad(cmd, "not an EffectCraft card"))?;
    let card = card_by_id(s, id);
    let b = board_of(s, &comp.project)?;
    let fps = main_fps(&b);
    let (a, e) = card.as_ref().map_or((comp.offset + comp.start / comp.rate, comp.offset + comp.end / comp.rate), |c| c.main);
    let n = num(p, "layers").filter(|v| v.is_finite() && *v >= 0.0).map_or(8, |v| v.min(200.0) as usize);
    let showing_comp = if s.effectcraft.node == Some(id) { s.effectcraft.time() } else { comp.start };
    let showing_main = comp.offset + showing_comp / comp.rate.max(1e-9);
    // What EffectCraft knows (none without the host: the board's own facts only).
    let has_host = s.ecboard.host.is_some();
    let (info, words, beats) = if has_host {
        let info = host_call(
            s,
            cmd,
            &comp.project,
            "figcraft.comp",
            &json!({"comp": if comp.comp_id > 0 { json!(comp.comp_id) } else { json!(comp.comp) }, "layers": n}),
        )
        .ok();
        let speech = host_call(s, cmd, &comp.project, "figcraft.speech", &json!({"start": a, "end": e})).ok();
        let beats =
            host_call(s, cmd, &comp.project, "figcraft.board", &json!({})).ok().and_then(|v| v.get("beats").and_then(Value::as_array).cloned());
        (info, speech.as_ref().map(words_of).unwrap_or_default(), beats.unwrap_or_default())
    } else {
        (None, Vec::new(), Vec::new())
    };
    let stress = stress_of(&beats, &words, a, e).map(|(w, t, from)| json!({"word": w, "frame": (t * fps).round(), "time": t, "from": from}));
    let marks: Vec<Value> = s
        .ecboard
        .marks
        .list
        .iter()
        .filter(|m| m.card == id)
        .map(|m| json!({"id": m.id, "kind": m.tool.id(), "at": m.timecode, "main": m.main_timecode, "note": if m.note.is_empty() { m.layers.first().and_then(|l| l.get("name")).cloned().unwrap_or(json!("")) } else { json!(m.note) }}))
        .collect();
    let chapter = card.as_ref().and_then(|c| b.rows.iter().find(|r| r.index == c.row)).map(|r| r.name.clone());
    let comp_json = match &info {
        Some(i) => {
            json!({"id": i["id"], "name": i["name"], "parent": card.as_ref().map(|c| c.parent.clone()).filter(|x| !x.is_empty()).map_or(i["parents"][0].clone(), |x| json!(x)),
            "size": [i["width"], i["height"]], "duration": i["duration"], "fps": i["fps"]})
        }
        None => json!({"id": comp.comp_id, "name": comp.comp, "parent": card.as_ref().map(|c| c.parent.clone()), "size": [comp.width, comp.height]}),
    };
    let layers = info.as_ref().and_then(|i| i.get("layers").cloned()).unwrap_or(json!([]));
    Ok(json!({
        "card": doc_name(s, id), "id": id, "chapter": chapter,
        "comp": comp_json,
        "main": {"start": timecode(a, fps), "end": timecode(e, fps), "frames": [(a * fps).round(), (e * fps).round()], "seconds": [a, e]},
        "showing": timecode(showing_main, fps),
        "stress": stress,
        "speech": line_of(&words, a, e),
        "layers": layers,
        "moreLayers": info.as_ref().and_then(|i| i.get("moreLayers").cloned()).unwrap_or(json!(0)),
        "marks": marks,
        "reads": [format!("figcraft.comp({}) → layers, keys", comp.comp_id), format!("speech.get(MAIN, {a:.2}–{e:.2} s) → words"), "canvas.annotate.list(since) → marks on this card".to_string()],
        "effectcraft": has_host,
    }))
}

fn doc_name(s: &Session, id: NodeId) -> String {
    s.doc().ok().and_then(|d| d.node(id)).map(|n| n.name.clone()).unwrap_or_default()
}

/// Parse an id that may be a string or number.
pub fn node_id(v: &Value) -> Option<NodeId> {
    id_of(v)
}

/// Read a string parameter, trimmed, at most `max` characters.
pub fn text_param(p: &Value, key: &str, max: usize) -> String {
    string(p, key).map(|s| s.trim().chars().take(max).collect()).unwrap_or_default()
}

pub fn specs() -> Vec<CommandSpec> {
    let mut v = vec![
        spec!(
            "board.autoMain",
            "MAIN Board",
            "{ecproj? (the .ecproj; default the selected card's, the page's board's or the first card's), comp? (MAIN's id or name; default the comp named MAIN, else the longest), x?, y? (where a new board starts; default right of the page's content)}  lays the EffectCraft project's MAIN out on the page: one row per chapter (MAIN's markers), one live card per scene comp MAIN shows in that stretch (looked through part comps), in time order, labelled \"S1 06 Old vs AI · 1:02\". Run again, it updates the board in place (scenes added or gone; cards keep their ids) → {project, main, title, rows: [{index, id, name, start, end, cards}], cards, added, removed, kept}",
            true,
            auto_main
        ),
        spec!(
            "board.state",
            "MAIN Board State",
            "{ecproj?}  → {revision (bumped with every event), lastEvent, project, main, title, rows: [{index, id, name, start, end, cards: [{card, label, comp, compId, project, row, mainStart, mainEnd, parent, bounds}]}], cards, selection: {ids, card, board}, view: {cx, cy, zoom, width, height}, armed (pen|comment|layer|null), marks: {count, last}, playhead}",
            false,
            state
        ),
        spec!(
            "board.events",
            "MAIN Board Events",
            "{since? (the last seq seen)}  → {last, revision, events: [{seq, kind: select|board|mark|marksRemoved|armed|flash|storyboard|review|presence.hover|presence.open, data}]}: what changed since `since` (the newest 256 kept); `select` carries the selected card ({card, label, comp, compId, project, row, mainStart, mainEnd, parent}; card null when none)",
            false,
            events
        ),
        spec!(
            "canvas.flyTo",
            "Fly To",
            "{card? | comp? (name or id) | chapter? (name, part of one, or row index) | time? (MAIN seconds: the scene showing then), maxZoom? (default 1.6), select? (default true)}  glides the view there (0.35 s in the app; the view is set at once) and selects the card → {to: {cx, cy, zoom}, rect, card, target}",
            false,
            fly_to
        ),
        spec!(
            "board.agentView",
            "Card for Claude",
            "{card? (default the selected card), layers? (default 8)}  what Claude reads for a card → {card, id, chapter, comp: {id, name, parent, size, duration, fps}, main: {start, end, frames, seconds}, showing, stress: {word, frame, time, from}, speech, layers: [{name, type, in, out, keys, visible, label}], moreLayers, marks: [{id, kind, at, main, note}], reads, effectcraft}",
            false,
            agent_view
        ),
    ];
    v.extend(marks::specs());
    v.extend(flash::specs());
    v.extend(presence::specs());
    v.extend(boards::specs());
    v
}

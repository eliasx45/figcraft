//! `board.storyboard` and `board.review`: boards beside a MAIN board row or card, in Devin
//! Jatho's storyboard style (the `speech-storyboard` skill's card: script panel, then per scene a
//! transcript pill → frame → note → PEAK / STRESS tags → audio strip), as the mock shows them.
//!
//! Words are MAIN's speech map (`figcraft.speech`), stresses are board.json beats there, else the
//! longest stressed word; the frames are live EffectCraft cards of one frame (they render like any
//! card). Notes come from a beat's `animation` / `note` only: none, the note stays empty (Claude
//! or Elias fills it). The review board's Existing filmstrip is cards of MAIN across the card's
//! range, its Proposed slots are empty frames for Claude to fill, and its Beats table lists the
//! comp's real keyframed moves (`figcraft.moves`) with the ease graph each matches.

use figcraft_color::{Color, Paint};
use figcraft_doc::{Document, EcComp, Node, NodeId, NodeKind, StrokeAlign, StyleRun, TextAlignH, TextAutoResize, TextData, VectorNetwork};
use figcraft_geom::{Affine, BezPath};
use serde_json::{Value, json};

use super::{
    Board, Card, KEY, KIND, PROJECT, Words, board_of, card_by_id, card_param, clock, fit_text, host_call, line_of, main_fps, stress_of, words_of,
};
use crate::cmd::CommandSpec;
use crate::params::{get, opt_id};
use crate::{EngineError, Result, Session, bad, spec};

const PAD: f64 = 60.0;
const COL_GAP: f64 = 60.0;
const SHOT_W: f64 = 480.0;
const SCRIPT_W: f64 = 500.0;
const ITEM_GAP: f64 = 18.0;
/// The most shots on one storyboard.
const MAX_SHOTS: usize = 24;
/// Frames in the review's Existing strip, and its Proposed slots.
const FILM: usize = 6;
const SLOTS: usize = 4;
const CELL_W: f64 = 300.0;

const BG: Color = Color::from_rgb8_const(0x16, 0x16, 0x16);
const PANEL: Color = Color::from_rgb8_const(0x1F, 0x1F, 0x1F);
const PAPER: Color = Color::from_rgb8_const(0xFA, 0xFA, 0xFA);
const DARK_INK: Color = Color::from_rgb8_const(0x22, 0x22, 0x22);
const WHITE: Color = Color::from_rgb8_const(0xFF, 0xFF, 0xFF);
const MUTED: Color = Color::from_rgb8_const(0x8A, 0x8A, 0x8A);
const PILL: Color = Color::from_rgb8_const(0x26, 0x26, 0x26);
const AMBER: Color = Color::from_rgb8_const(0xFF, 0xB0, 0x20);
const ORANGE: Color = Color::from_rgb8_const(0xFF, 0x7A, 0x1A);
const RED: Color = Color::from_rgb8_const(0xFF, 0x4A, 0x4A);
const STRESS_BG: Color = Color::from_rgb8_const(0x3A, 0x3A, 0x3A);
const STRESS_INK: Color = Color::from_rgb8_const(0xFF, 0xD0, 0x8A);
const BLUE: Color = Color::from_rgb8_const(0x2D, 0x8C, 0xFF);
const WAVE: Color = Color::from_rgb8_const(0x3F, 0xD0, 0xA8);
const STRIP_BG: Color = Color::from_rgb8_const(0x10, 0x18, 0x20);
const SLOT_BG: Color = Color::from_rgb8_const(0x2A, 0x2A, 0x2A);
const GRAPH_BG: Color = Color::from_rgb8_const(0x2A, 0x2A, 0x2A);
const GRAPH_INK: Color = Color::from_rgb8_const(0x7F, 0xB6, 0xFF);

/// Devin's five ease graphs as small speed-curve icons (34×20), by the names EffectCraft's
/// `taste.check` matches.
const GRAPHS: [(&str, &str); 5] = [
    ("Fast In", "M1,18 C3,1 6,1 9,10 S20,17 33,18"),
    ("Fast out", "M1,18 C14,17 24,15 28,8 S31,1 33,18"),
    ("Quick launch · soft landing", "M1,18 C4,2 9,2 14,9 S26,17 33,18"),
    ("Slow launch · quick finish", "M1,18 C10,17 18,12 22,6 S28,2 33,18"),
    ("Smooth emphasis", "M1,18 C9,18 12,2 17,2 S25,18 33,18"),
];

/// Layers under construction: everything goes in `parent` at `(x, y)` relative to it.
struct Builder<'a> {
    doc: &'a mut Document,
}

impl Builder<'_> {
    fn insert(&mut self, mut n: Node, parent: NodeId, at: (f64, f64)) -> Result<NodeId> {
        n.transform = Affine::translate(at);
        let id = n.id;
        self.doc.insert(n, parent, None)?;
        Ok(id)
    }

    fn frame(&mut self, parent: NodeId, name: &str, at: (f64, f64), size: (f64, f64), fill: Color, radius: f64) -> Result<NodeId> {
        let mut n = Node::new(self.doc.alloc_id(), NodeKind::Frame, name, size.0.max(1.0), size.1.max(1.0));
        n.fills = vec![Paint::solid(fill)];
        n.corners.radii = [radius; 4];
        n.clip = false;
        self.insert(n, parent, at)
    }

    fn rect(&mut self, parent: NodeId, at: (f64, f64), size: (f64, f64), fill: Color) -> Result<NodeId> {
        let mut n = Node::new(self.doc.alloc_id(), NodeKind::Rectangle, "Bar", size.0.max(0.5), size.1.max(0.5));
        n.fills = vec![Paint::solid(fill)];
        self.insert(n, parent, at)
    }

    /// A text layer `width` wide (its height fits the text); `bold`: char ranges in `accent`.
    #[allow(clippy::too_many_arguments)]
    fn text(
        &mut self,
        parent: NodeId,
        at: (f64, f64),
        width: f64,
        text: &str,
        size: f64,
        ink: Color,
        align: TextAlignH,
        bold: &[(usize, usize)],
        accent: Color,
        style: &str,
    ) -> Result<(NodeId, f64)> {
        let mut td = TextData { characters: text.to_string(), auto_resize: TextAutoResize::Height, align_h: align, ..TextData::default() };
        td.style.font_size = size;
        if !style.is_empty() {
            td.style.font_style = style.to_string();
        }
        let n_chars = text.chars().count();
        for (a, b) in bold {
            let (a, b) = ((*a).min(n_chars), (*b).min(n_chars));
            if b > a {
                let mut st = td.style.clone();
                st.fills = Some(vec![Paint::solid(accent)]);
                st.font_style = "Bold".into();
                td.runs.push(StyleRun { start: a, end: b, style: st });
            }
        }
        td.runs.sort_by_key(|r| r.start);
        let name: String = text.lines().next().unwrap_or("Text").chars().take(48).collect();
        let mut n = Node::new(
            self.doc.alloc_id(),
            NodeKind::Text(td),
            if name.trim().is_empty() { "Text".to_string() } else { name },
            width.max(1.0),
            size * 1.4,
        );
        n.fills = vec![Paint::solid(ink)];
        let id = self.insert(n, parent, at)?;
        fit_text(self.doc, id);
        let h = self.doc.node(id).map_or(size * 1.4, |n| n.height);
        if let Some(n) = self.doc.node_mut(id) {
            n.width = width.max(1.0);
        }
        Ok((id, h))
    }

    /// A rounded pill holding `text`; returns its size.
    fn pill(&mut self, parent: NodeId, at: (f64, f64), text: &str, fill: Color, ink: Color, size: f64) -> Result<(f64, f64)> {
        let w = (text.chars().count() as f64 * size * 0.6 + 2.0 * size).clamp(40.0, SHOT_W);
        let h = size * 1.9;
        let f = self.frame(parent, text, at, (w, h), fill, h / 2.0)?;
        self.text(f, (0.0, size * 0.25), w, text, size, ink, TextAlignH::Center, &[], ink, "Bold")?;
        Ok((w, h))
    }

    /// Devin's ease graph icon `name` (34×20, scaled `k`), at `at` in `parent`.
    fn graph(&mut self, parent: NodeId, at: (f64, f64), name: &str, k: f64) -> Result<()> {
        let bg = self.frame(parent, "Ease graph", at, (34.0 * k, 20.0 * k), GRAPH_BG, 3.0 * k)?;
        let Some((_, d)) = GRAPHS.iter().find(|(n, _)| *n == name) else { return Ok(()) };
        let Ok(mut path) = BezPath::from_svg(d) else { return Ok(()) };
        path.apply_affine(Affine::scale(k));
        let mut n = Node::new(self.doc.alloc_id(), NodeKind::Vector { network: VectorNetwork::from_path(&path) }, name, 34.0 * k, 20.0 * k);
        n.strokes = vec![Paint::solid(GRAPH_INK)];
        n.stroke.weight = 1.5 * k;
        n.stroke.align = StrokeAlign::Center;
        if let NodeKind::Vector { network } = &mut n.kind {
            network.regions.clear();
        }
        self.insert(n, bg, (0.0, 0.0))?;
        Ok(())
    }

    /// A one-frame live card of `c` at comp second `t`.
    fn still(&mut self, parent: NodeId, at: (f64, f64), w: f64, c: &EcComp, t: f64, name: String) -> Result<NodeId> {
        let mut k = c.clone();
        let t = t.clamp(c.start, (c.end - 1.0 / c.fps).max(c.start));
        let f = k.frame_of(t) as f64 / k.fps;
        (k.start, k.end) = (f, f + 1.0 / k.fps);
        k.sound = false;
        k.sanitize();
        let h = (w * f64::from(k.height) / f64::from(k.width.max(1))).clamp(9.0, 4.0 * w);
        let n = crate::cmd::effectcraft::card_node(self.doc.alloc_id(), k, name, w, h);
        self.insert(n, parent, at)
    }
}

/// Char ranges of `word` (whole words, any case) in `line`.
fn ranges_of(line: &str, word: &str) -> Vec<(usize, usize)> {
    let w = word.trim().to_lowercase();
    if w.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars.get(i).is_some_and(|c| c.is_alphanumeric()) {
            i += 1;
            continue;
        }
        let start = i;
        while chars.get(i).is_some_and(|c| c.is_alphanumeric() || *c == '\'') {
            i += 1;
        }
        let token: String = chars.get(start..i).map(|s| s.iter().collect::<String>()).unwrap_or_default().to_lowercase();
        if token == w {
            out.push((start, i));
            if out.len() >= 4 {
                break;
            }
        }
    }
    out
}

/// What a scene's column shows.
struct Shot {
    card: Card,
    line: String,
    stress: Option<(String, f64, &'static str)>,
    note: String,
}

fn note_of(beats: &[Value], a: f64, b: f64) -> String {
    beats
        .iter()
        .filter(|x| x.get("time").and_then(Value::as_f64).is_some_and(|t| t >= a && t < b))
        .find_map(|x| ["animation", "note", "attr"].iter().find_map(|k| x.get(*k).and_then(Value::as_str).filter(|v| !v.trim().is_empty())))
        .unwrap_or_default()
        .to_string()
}

/// The audio strip of MAIN seconds `a..b`: word bars (stressed ones taller), a frame ruler, amber
/// marks on long stressed words, the stress in red.
fn audio_strip(
    bd: &mut Builder,
    parent: NodeId,
    at: (f64, f64),
    words: &[(f64, f64, String, bool)],
    (a, b): (f64, f64),
    fps: f64,
    stress: Option<&(String, f64, &str)>,
) -> Result<f64> {
    let (w, h) = (SHOT_W, 104.0);
    let strip = bd.frame(parent, "Audio", at, (w, h), STRIP_BG, 10.0)?;
    let span = (b - a).max(1e-3);
    let x = |t: f64| ((t - a) / span * w).clamp(0.0, w);
    for wd in words.iter().filter(|wd| wd.1 > a && wd.0 < b).take(400) {
        let bh = if wd.3 { 56.0 } else { 28.0 };
        bd.rect(strip, (x(wd.0), 44.0 - bh / 2.0), ((x(wd.1) - x(wd.0) - 1.0).max(1.5), bh), WAVE)?;
        if wd.3 && wd.2.chars().count() > 5 {
            bd.rect(strip, (x(wd.0), 4.0), (2.0, 76.0), AMBER.with_alpha(0.75))?;
        }
    }
    let (f0, f1) = ((a * fps).ceil() as i64, (b * fps).floor() as i64);
    let step = if f1 - f0 > 300 { 60 } else { 30 };
    let mut fr = (f0 + step - 1).div_euclid(step) * step;
    let mut n = 0;
    while fr <= f1 && n < 64 {
        let tx = x(fr as f64 / fps);
        bd.rect(strip, (tx, 80.0), (1.5, 8.0), MUTED)?;
        bd.text(strip, (tx + 3.0, 86.0), 80.0, &format!("f{fr}"), 13.0, MUTED, TextAlignH::Left, &[], MUTED, "")?;
        fr += step;
        n += 1;
    }
    if let Some((word, t, _)) = stress
        && *t >= a
        && *t <= b
    {
        bd.rect(strip, (x(*t), 0.0), (4.0, 80.0), RED)?;
        let label = format!("▲ {}", word.to_uppercase());
        bd.text(strip, ((x(*t) + 6.0).min(w - 90.0), 2.0), 160.0, &label, 15.0, RED, TextAlignH::Left, &[], RED, "Bold")?;
    }
    Ok(h)
}

/// One scene's column at `(x, y)` in `parent`; returns its height.
#[allow(clippy::too_many_arguments)]
fn shot_column(
    bd: &mut Builder,
    parent: NodeId,
    (x, y): (f64, f64),
    k: usize,
    sh: &Shot,
    words: &[(f64, f64, String, bool)],
    fps: f64,
    extra: &[(String, Color, Color)],
) -> Result<f64> {
    let col = bd.frame(parent, &format!("S{} · {}", k + 1, sh.card.comp.comp), (x, y), (SHOT_W, 10.0), BG.with_alpha(0.0), 0.0)?;
    let mut cy = 0.0;
    // Transcript pill.
    let line = if sh.line.is_empty() { "(no speech)".to_string() } else { sh.line.clone() };
    let bold = sh.stress.as_ref().map(|s| ranges_of(&line, &s.0)).unwrap_or_default();
    let pill = bd.frame(col, "Transcript", (0.0, cy), (SHOT_W, 40.0), PILL, 24.0)?;
    let (_, th) = bd.text(pill, (20.0, 12.0), SHOT_W - 40.0, &line, 22.0, WHITE.with_alpha(0.55), TextAlignH::Center, &bold, AMBER, "")?;
    if let Some(n) = bd.doc.node_mut(pill) {
        n.height = th + 24.0;
    }
    cy += th + 24.0 + ITEM_GAP;
    // The frame: a live one-frame card at the stress (else the middle).
    let (a, b) = sh.card.main;
    let at_main = sh.stress.as_ref().map_or((a + b) / 2.0, |s| s.1);
    let code: String = sh.card.comp.comp.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    let frame = bd.still(col, (0.0, cy), SHOT_W, &sh.card.comp, sh.card.comp_of(at_main), format!("S{} · {code}", k + 1))?;
    let fh = bd.doc.node(frame).map_or(270.0, |n| n.height);
    if let Some(n) = bd.doc.node_mut(frame) {
        n.plugin_data.insert(KIND.into(), "still".into());
    }
    bd.text(col, (12.0, cy + 8.0), 300.0, &format!("S{} · {code}", k + 1), 20.0, WHITE, TextAlignH::Left, &[], WHITE, "Bold")?;
    cy += fh + ITEM_GAP;
    // The note (Devin's direction): empty unless one is known.
    let (_, nh) = bd.text(col, (0.0, cy), SHOT_W, &sh.note, 26.0, WHITE, TextAlignH::Center, &[], WHITE, "Heavy Italic")?;
    cy += if sh.note.is_empty() { 0.0 } else { nh + ITEM_GAP };
    // Tags.
    let mut tags: Vec<(String, Color, Color)> = Vec::new();
    if let Some((w, t, from)) = &sh.stress {
        if *from == "board.json beat" {
            tags.push((format!("PEAK f{} · {w}", (t * fps).round()), RED, WHITE));
        } else {
            tags.push((format!("STRESS · {w}"), STRESS_BG, STRESS_INK));
        }
    }
    tags.extend(extra.iter().cloned());
    let mut tx = 0.0;
    let mut ty = cy;
    let mut row_h = 0.0_f64;
    for (t, fill, ink) in &tags {
        let est = (t.chars().count() as f64 * 18.0 * 0.6 + 36.0).clamp(40.0, SHOT_W);
        if tx > 0.0 && tx + est > SHOT_W {
            tx = 0.0;
            ty += row_h + 8.0;
            row_h = 0.0;
        }
        let (w, h) = bd.pill(col, (tx, ty), t, *fill, *ink, 18.0)?;
        tx += w + 8.0;
        row_h = row_h.max(h);
    }
    cy = if tags.is_empty() { cy } else { ty + row_h + ITEM_GAP };
    // Audio ±0.25 s.
    let ah = audio_strip(bd, col, (0.0, cy), words, ((a - 0.25).max(0.0), b + 0.25), fps, sh.stress.as_ref())?;
    cy += ah;
    if let Some(n) = bd.doc.node_mut(col) {
        n.height = cy;
    }
    Ok(cy)
}

/// MAIN's words and board.json beats over `a..b` (the host says).
fn speech(s: &mut Session, cmd: &str, project: &str, a: f64, b: f64) -> Result<(Words, Vec<Value>)> {
    let sp = host_call(s, cmd, project, "figcraft.speech", &json!({"start": (a - 0.5).max(0.0), "end": b + 0.5}))?;
    let beats = host_call(s, cmd, project, "figcraft.board", &json!({}))?.get("beats").and_then(Value::as_array).cloned().unwrap_or_default();
    Ok((words_of(&sp), beats))
}

fn shot_of(card: &Card, words: &[(f64, f64, String, bool)], beats: &[Value]) -> Shot {
    let (a, b) = card.main;
    Shot { card: card.clone(), line: line_of(words, a, b), stress: stress_of(beats, words, a, b), note: note_of(beats, a, b) }
}

/// Where boards go: right of the board's cards.
fn boards_x(doc: &Document, b: &Board) -> f64 {
    b.cards.iter().filter_map(|c| doc.world_bounds(c.id)).map(|r| r.x1).fold(f64::MIN, f64::max).max(0.0) + 160.0
}

/// The page y of row `index`'s cards.
fn row_y(doc: &Document, b: &Board, index: usize) -> f64 {
    b.cards
        .iter()
        .filter(|c| c.row == index)
        .filter_map(|c| doc.world_bounds(c.id))
        .map(|r| r.y0)
        .fold(f64::MAX, f64::min)
        .min(b.rows.iter().find(|r| r.index == index).and_then(|r| doc.world_bounds(r.id)).map_or(f64::MAX, |r| r.y0 - 20.0))
}

/// Remove the boards of `kind` keyed `key` (an earlier storyboard or review of the same thing).
fn remove_old(doc: &mut Document, page: NodeId, project: &str, kind: &str, key: &str) -> Result<usize> {
    let old: Vec<NodeId> = doc
        .node(page)
        .map(|p| p.children.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|id| {
            doc.node(*id).is_some_and(|n| {
                n.plugin_data.get(KIND).map(String::as_str) == Some(kind)
                    && n.plugin_data.get(PROJECT).map(String::as_str) == Some(project)
                    && n.plugin_data.get(KEY).map(String::as_str) == Some(key)
            })
        })
        .collect();
    for id in &old {
        doc.remove(*id)?;
    }
    Ok(old.len())
}

fn root_frame(bd: &mut Builder, page: NodeId, name: &str, at: (f64, f64), project: &str, kind: &str, key: &str, info: Value) -> Result<NodeId> {
    let root = bd.frame(page, name, at, (100.0, 100.0), BG.with_alpha(0.96), 68.0)?;
    if let Some(n) = bd.doc.node_mut(root) {
        n.plugin_data.insert(KIND.into(), kind.into());
        n.plugin_data.insert(PROJECT.into(), project.into());
        n.plugin_data.insert(KEY.into(), key.into());
        n.plugin_data.insert(super::INFO.into(), info.to_string());
    }
    Ok(root)
}

/// The row `p` names: `row` (index or chapter name), else `card`'s row, else the selected card's.
fn row_param(cmd: &str, s: &Session, b: &Board, p: &Value) -> Result<usize> {
    match get(p, "row").filter(|v| !v.is_null()) {
        Some(Value::Number(n)) => {
            let i = n.as_u64().ok_or_else(|| bad(cmd, "`row` is a row index or chapter name"))? as usize;
            b.rows.iter().find(|r| r.index == i).map(|r| r.index).ok_or_else(|| bad(cmd, format!("no row {i}")))
        }
        Some(Value::String(q)) => {
            let q = q.trim().to_lowercase();
            b.rows
                .iter()
                .find(|r| r.name.to_lowercase() == q)
                .or_else(|| b.rows.iter().find(|r| !q.is_empty() && r.name.to_lowercase().contains(&q)))
                .map(|r| r.index)
                .ok_or_else(|| bad(cmd, "no such chapter on the board"))
        }
        Some(_) => Err(bad(cmd, "`row` is a row index or chapter name")),
        None => {
            let id = card_param(cmd, s, p)?;
            b.cards.iter().find(|c| c.id == id).map(|c| c.row).ok_or_else(|| bad(cmd, "that card isn't on the MAIN board (board.autoMain)"))
        }
    }
}

fn storyboard(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.storyboard";
    let project = match opt_id(cmd, p, "card")? {
        Some(id) => card_by_id(s, id).map(|c| c.comp.project).unwrap_or_default(),
        None => String::new(),
    };
    let b = board_of(s, &project)?;
    if b.cards.is_empty() {
        return Err(EngineError::Disabled(cmd.into(), "no MAIN board on this page (board.autoMain)".into()));
    }
    let row = row_param(cmd, s, &b, p)?;
    let cards: Vec<Card> = b.cards.iter().filter(|c| c.row == row).take(MAX_SHOTS).cloned().collect();
    let (a, e) = cards.iter().fold((f64::MAX, f64::MIN), |(a, e), c| (a.min(c.main.0), e.max(c.main.1)));
    let (words, beats) = speech(s, cmd, &b.project, a, e)?;
    let fps = main_fps(&b);
    let shots: Vec<Shot> = cards.iter().map(|c| shot_of(c, &words, &beats)).collect();
    let name = b.rows.iter().find(|r| r.index == row).map(|r| r.name.clone()).unwrap_or_default();
    let page = s.page()?;
    let doc = s.doc_mut()?;
    let key = format!("row:{row}");
    remove_old(doc, page, &b.project, "storyboard", &key)?;
    let at = (boards_x(doc, &b), row_y(doc, &b, row) + 10.0);
    let mut bd = Builder { doc };
    let root =
        root_frame(&mut bd, page, &format!("Storyboard · {name}"), at, &b.project, "storyboard", &key, json!({"row": row, "shots": shots.len()}))?;
    bd.text(
        root,
        (PAD, PAD - 10.0),
        1600.0,
        &format!("Storyboard · {name} · from the speech map + board.json"),
        26.0,
        MUTED,
        TextAlignH::Left,
        &[(0, 13 + name.chars().count())],
        WHITE,
        "",
    )?;
    let top = PAD + 50.0;
    // The script panel: every line, its stressed word orange.
    let script = bd.frame(root, "Script", (PAD, top), (SCRIPT_W, 100.0), PAPER, 32.0)?;
    let (_, hh) = bd.text(
        script,
        (36.0, 32.0),
        SCRIPT_W - 72.0,
        "SCRIPT",
        56.0,
        Color::from_rgb8_const(0x11, 0x11, 0x11),
        TextAlignH::Left,
        &[],
        DARK_INK,
        "Heavy Italic",
    )?;
    let mut sy = 32.0 + hh + 12.0;
    for (k, sh) in shots.iter().enumerate() {
        let tag = format!("S{}  ", k + 1);
        let text = format!("{tag}{}", if sh.line.is_empty() { "(no speech)" } else { &sh.line });
        let off = tag.chars().count();
        let mut bold: Vec<(usize, usize)> =
            sh.stress.as_ref().map(|st| ranges_of(&sh.line, &st.0)).unwrap_or_default().into_iter().map(|(x, y)| (x + off, y + off)).collect();
        bold.truncate(1);
        let (_, h) = bd.text(script, (36.0, sy), SCRIPT_W - 72.0, &text, 22.0, DARK_INK, TextAlignH::Left, &bold, ORANGE, "")?;
        sy += h + 12.0;
    }
    if let Some(n) = bd.doc.node_mut(script) {
        n.height = sy + 24.0;
    }
    let mut tallest = sy + 24.0;
    for (k, sh) in shots.iter().enumerate() {
        let x = PAD + SCRIPT_W + COL_GAP + k as f64 * (SHOT_W + COL_GAP);
        tallest = tallest.max(shot_column(&mut bd, root, (x, top), k, sh, &words, fps, &[])?);
    }
    let width = PAD * 2.0 + SCRIPT_W + shots.len() as f64 * (SHOT_W + COL_GAP);
    if let Some(n) = bd.doc.node_mut(root) {
        (n.width, n.height) = (width, top + tallest + PAD);
    }
    let out = json!({"id": root, "row": row, "chapter": name, "shots": shots.iter().map(|sh| json!({"card": sh.card.id, "comp": sh.card.comp.comp, "line": sh.line,
        "stress": sh.stress.as_ref().map(|(w, t, from)| json!({"word": w, "time": t, "frame": (t * fps).round(), "from": from})), "note": sh.note})).collect::<Vec<_>>(),
        "bounds": [at.0, at.1, width, top + tallest + PAD]});
    s.ecboard.push("storyboard", json!({"id": root, "row": row}));
    Ok(out)
}

fn review(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "board.review";
    let id = card_param(cmd, s, p)?;
    let card = card_by_id(s, id).ok_or_else(|| bad(cmd, "that card isn't on the MAIN board (board.autoMain)"))?;
    let b = board_of(s, &card.comp.project)?;
    let fps = main_fps(&b);
    let (a, e) = card.main;
    let (words, beats) = speech(s, cmd, &b.project, a, e)?;
    let moves = host_call(s, cmd, &b.project, "figcraft.moves", &json!({"comp": card.comp.comp_id, "start": card.comp.start, "end": card.comp.end}))?;
    let moves: Vec<Value> = moves.get("moves").and_then(Value::as_array).cloned().unwrap_or_default();
    let shot = shot_of(&card, &words, &beats);
    let row_name = b.rows.iter().find(|r| r.index == card.row).map(|r| r.name.clone()).unwrap_or_default();
    let main_card = b.main.get("id").and_then(Value::as_u64).map(|mid| {
        let mut m = EcComp::new(b.main.get("name").and_then(Value::as_str).unwrap_or("MAIN"), a, e.max(a + 1.0 / fps), fps);
        m.comp_id = mid;
        m.project = b.project.clone();
        m.width = b.main.get("width").and_then(Value::as_u64).map_or(1920, |v| v.clamp(1, 32_768) as u32);
        m.height = b.main.get("height").and_then(Value::as_u64).map_or(1080, |v| v.clamp(1, 32_768) as u32);
        m.sanitize();
        m
    });
    let next = b
        .cards
        .iter()
        .position(|c| c.id == id)
        .and_then(|i| b.cards.get(i + 1))
        .map(|c| c.comp.comp.split_whitespace().take(2).collect::<Vec<_>>().join(" "));
    let page = s.page()?;
    let doc = s.doc_mut()?;
    let key = format!("card:{id}");
    remove_old(doc, page, &b.project, "review", &key)?;
    // Under this row's storyboard when there is one, else at the row.
    let story = doc.node(page).map(|p| p.children.clone()).unwrap_or_default().into_iter().find(|n| {
        doc.node(*n).is_some_and(|n| {
            n.plugin_data.get(KIND).map(String::as_str) == Some("storyboard") && n.plugin_data.get(KEY) == Some(&format!("row:{}", card.row))
        })
    });
    let x = boards_x(doc, &b);
    let y = match story.and_then(|n| doc.world_bounds(n)) {
        Some(r) => r.y1 + 120.0,
        None => row_y(doc, &b, card.row) + 10.0,
    };
    let mut bd = Builder { doc };
    let root = root_frame(&mut bd, page, &format!("Review · {}", card.comp.comp), (x, y), &b.project, "review", &key, json!({"card": id}))?;
    let head = format!("Review · {} · {} row · read-only", card.comp.comp, row_name.split(" · ").next().unwrap_or(""));
    bd.text(root, (PAD, PAD - 10.0), 1600.0, &head, 26.0, MUTED, TextAlignH::Left, &[(0, 9 + card.comp.comp.chars().count())], WHITE, "")?;
    let top = PAD + 50.0;
    let mut extra = vec![("VERDICT · —".to_string(), BLUE, WHITE), ("SCENE · as built".to_string(), WHITE.with_alpha(0.1), WHITE)];
    if let Some(n) = next {
        extra.push((format!("INTERRUPT OK · next {n}"), Color::from_rgb8_const(0x26, 0x44, 0x6E), Color::from_rgb8_const(0xCF, 0xE0, 0xFF)));
    }
    let col_h = shot_column(&mut bd, root, (PAD, top), 0, &shot, &words, fps, &extra)?;
    // The right side: Existing, Proposed, Beats.
    let rx = PAD + SHOT_W + COL_GAP;
    let strip_w = 32.0 * 2.0 + FILM as f64 * (CELL_W + 16.0);
    let mut ry = top;
    // Existing: MAIN across the card's range.
    let ex = bd.frame(root, "Existing", (rx, ry), (strip_w, 100.0), PANEL, 36.0)?;
    bd.text(
        ex,
        (32.0, 24.0),
        strip_w - 64.0,
        &format!("EXISTING · {} in MAIN (rendered)", card.comp.comp),
        30.0,
        WHITE,
        TextAlignH::Left,
        &[],
        WHITE,
        "Heavy Italic",
    )?;
    let mut film = Vec::new();
    let cell_h = CELL_W * 9.0 / 16.0;
    if let Some(m) = &main_card {
        for i in 0..FILM {
            let t = a + (e - a) * (i as f64 + 0.5) / FILM as f64;
            let label = format!("f{} · {:.1} s", (t * fps).round(), t);
            let cid = bd.still(ex, (32.0 + i as f64 * (CELL_W + 16.0), 80.0), CELL_W, m, t, format!("MAIN · {}", clock(t)))?;
            if let Some(n) = bd.doc.node_mut(cid) {
                n.plugin_data.insert(KIND.into(), "still".into());
            }
            bd.text(ex, (32.0 + i as f64 * (CELL_W + 16.0), 80.0 + cell_h + 6.0), CELL_W, &label, 18.0, MUTED, TextAlignH::Center, &[], MUTED, "")?;
            film.push(json!({"card": cid, "time": t, "frame": (t * fps).round()}));
        }
    }
    let eh = 80.0 + cell_h + 44.0;
    if let Some(n) = bd.doc.node_mut(ex) {
        n.height = eh;
    }
    ry += eh + 32.0;
    // Proposed: empty slots for Claude.
    let pr = bd.frame(root, "Proposed", (rx, ry), (strip_w, eh), PANEL, 36.0)?;
    bd.text(pr, (32.0, 24.0), strip_w - 64.0, "PROPOSED · empty slots for Claude", 30.0, WHITE, TextAlignH::Left, &[(0, 8)], BLUE, "Heavy Italic")?;
    let mut slots = Vec::new();
    for i in 0..SLOTS {
        let sx = 32.0 + i as f64 * (CELL_W + 16.0);
        let sid = bd.frame(pr, &format!("Slot {}", i + 1), (sx, 80.0), (CELL_W, cell_h), SLOT_BG, 8.0)?;
        if let Some(n) = bd.doc.node_mut(sid) {
            n.plugin_data.insert(KIND.into(), "slot".into());
            n.plugin_data.insert(KEY.into(), format!("slot:{}", i + 1));
        }
        bd.text(pr, (sx, 80.0 + cell_h + 6.0), CELL_W, &format!("slot {}", i + 1), 18.0, MUTED, TextAlignH::Center, &[], MUTED, "")?;
        slots.push(sid);
    }
    ry += eh + 32.0;
    // Beats: the comp's real moves and the ease graph each matches.
    let bt = bd.frame(root, "Beats", (rx, ry), (strip_w, 100.0), PANEL, 36.0)?;
    bd.text(bt, (32.0, 24.0), strip_w - 64.0, "BEATS · \"now\" from the comp's keys", 30.0, WHITE, TextAlignH::Left, &[], WHITE, "Heavy Italic")?;
    let cols = [32.0, 520.0, 900.0, 1240.0];
    let mut by = 84.0;
    for (i, h) in ["MOVE", "NOW", "PROPOSED", "EASE GRAPH"].iter().enumerate() {
        bd.text(bt, (cols[i], by), 360.0, h, 16.0, MUTED, TextAlignH::Left, &[], MUTED, "")?;
    }
    by += 34.0;
    let peak_t = shot.stress.as_ref().map(|st| card.comp_of(st.1));
    let mut rows = Vec::new();
    for m in moves.iter().take(16) {
        let (t0, t1) = (m["from"]["time"].as_f64().unwrap_or(0.0), m["to"]["time"].as_f64().unwrap_or(0.0));
        let what = format!("{} · {}", m["layer"].as_str().unwrap_or("?"), m["property"].as_str().unwrap_or(""));
        let now = format!("f{}–{} · {:.2}→{:.2} s", m["from"]["frame"].as_f64().unwrap_or(0.0), m["to"]["frame"].as_f64().unwrap_or(0.0), t0, t1);
        let ease = m["curve"].as_str().or_else(|| m["nearest"].as_str()).unwrap_or("");
        let peak = peak_t.is_some_and(|pt| pt >= t0 - 0.05 && pt <= t1 + 0.05);
        let ink = if peak { Color::from_rgb8_const(0xFF, 0x62, 0x62) } else { WHITE };
        bd.text(bt, (cols[0], by), 470.0, &format!("{what}{}", if peak { " ★" } else { "" }), 18.0, ink, TextAlignH::Left, &[], ink, "Bold")?;
        bd.text(bt, (cols[1], by), 360.0, &now, 18.0, Color::from_rgb8_const(0xBB, 0xBB, 0xBB), TextAlignH::Left, &[], WHITE, "")?;
        bd.graph(bt, (cols[3], by - 6.0), ease, 1.6)?;
        bd.text(
            bt,
            (cols[3] + 64.0, by),
            420.0,
            if ease.is_empty() { "linear / no graph" } else { ease },
            18.0,
            Color::from_rgb8_const(0xBB, 0xBB, 0xBB),
            TextAlignH::Left,
            &[],
            WHITE,
            "",
        )?;
        rows.push(json!({"move": what, "now": now, "ease": ease, "peak": peak, "from": t0, "to": t1}));
        by += 42.0;
    }
    if moves.is_empty() {
        bd.text(bt, (cols[0], by), 900.0, "no keyframed moves in this range", 18.0, MUTED, TextAlignH::Left, &[], MUTED, "")?;
        by += 42.0;
    }
    if let Some(n) = bd.doc.node_mut(bt) {
        n.height = by + 24.0;
    }
    ry += by + 24.0;
    let width = rx + strip_w + PAD;
    let height = top + col_h.max(ry - top) + PAD;
    if let Some(n) = bd.doc.node_mut(root) {
        (n.width, n.height) = (width, height);
    }
    s.ecboard.push("review", json!({"id": root, "card": id}));
    Ok(json!({"id": root, "card": id, "comp": card.comp.comp, "verdict": null, "existing": film, "proposed": slots, "beats": rows,
        "stress": shot.stress.as_ref().map(|(w, t, from)| json!({"word": w, "time": t, "frame": (t * fps).round(), "from": from})), "bounds": [x, y, width, height]}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        spec!(
            "board.storyboard",
            "Storyboard Row",
            "{row? (index or chapter name) | card? (its row; default the selected card's)}  a storyboard beside the row (Devin style): the SCRIPT panel, then per scene a transcript pill (stressed word amber) → a live frame at the stress → the note (empty unless board.json has one) → PEAK / STRESS tags → the audio strip (words, frame ruler, stress); replaces the row's earlier one → {id, row, chapter, shots: [{card, comp, line, stress, note}], bounds}",
            true,
            storyboard
        ),
        spec!(
            "board.review",
            "Review Card",
            "{card? (default the selected card)}  a review board beside the card: its scene column with VERDICT / SCENE / INTERRUPT tags, EXISTING (live frames of MAIN across its range), PROPOSED (empty slots for Claude), BEATS (the comp's keyframed moves, now, and the ease graph each matches); under the row's storyboard when there is one → {id, card, comp, verdict, existing: [{card, time, frame}], proposed: [slot ids], beats: [{move, now, ease, peak, from, to}], stress, bounds}",
            true,
            review
        ),
    ]
}

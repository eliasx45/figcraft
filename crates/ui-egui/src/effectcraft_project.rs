//! Fork-only (`effectcraft-inproc`): new material into an EffectCraft card's comp, and new comps,
//! without going to EffectCraft. Every edit goes through the in-process session the card's
//! Timeline uses ([`crate::effectcraft_panel::EcHost`]): same undo, ⌘S save, read-only guard and
//! live card refresh.
//!
//! - **Project tab.** The column right of the open Timeline has two tabs, Effect Controls and
//!   Project. Project is EffectCraft's real Project panel (`panels::project::show`, hostable since
//!   EffectCraft's EMBED9) drawn against [`ProjectHost`], a thin wrapper over the panel's host:
//!   items, folders, labels, renames, search, Import (double-click the empty list: a file
//!   dialog), New Folder, and dragging items into the Timeline (EffectCraft's own drag payload) or
//!   onto the card (a layer centred where it lands). What EffectCraft's app would do in its own
//!   windows becomes FigCraft's: Create a new Composition opens EffectCraft's New Composition
//!   settings (`comp_settings::show`) in a FigCraft window and the comp becomes a card; opening a
//!   comp (double-click, "used in" ▾, Send to FigCraft) shows it as a card (placed right of the
//!   Timeline's card when it has none yet) with the Timeline on it.
//! - **Files from the Finder.** Dropped on a card (whose project is saved), they are imported
//!   into its project with EffectCraft's `file.import` (drag import, so proxies, sequences and
//!   interpretation work as in EffectCraft) and become layers centred where they land, at the
//!   card's time, like a drop on EffectCraft's Composition panel; the card's Timeline opens.
//!   Dropped on the open Timeline, they go where its insertion line shows, starting at the drop
//!   time over the time graph (Shift: at the current time), as EffectCraft's Timeline places them
//!   (`os_drop::TimelineDropMap`); on Effect Controls or the Project tab, into the project only.
//!   Alt imports a folder's numbered stills one by one.
//! - **New EffectCraft comp** (File menu, Actions palette, `effectcraft.newComp`): in the selected
//!   card's project, else in a new `.ecproj` saved next to the FigCraft file; EffectCraft's New
//!   Composition dialog first unless the size is given. In a project the open Timeline edits, the
//!   comp is an unsaved edit of that session (⌘S saves it) and the Timeline switches to the new
//!   card; a new project, or one no card has open, is saved at once.

use std::path::{Path, PathBuf};

use effectcraft_ui_egui::automation::Registry;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::DragPayload;
use effectcraft_ui_egui::panels::comp_settings::{self, CompDraft};
use effectcraft_ui_egui::panels::timeline::{PointerColor, TimelineHost, TimelineParts};
use effectcraft_ui_egui::state::UiState;
use effectcraft_ui_egui::theme::{ThemeKind, Tokens};
use egui::{Pos2, Rect, Sense, Stroke, pos2, vec2};
use figcraft_doc::NodeId;
use figcraft_ec_inproc::effectcraft_engine::Session;
use figcraft_ec_inproc::effectcraft_engine::project::{ItemId, ItemKind};
use serde_json::{Map, Value, json};

use crate::FigApp;
use crate::effectcraft_panel::EcHost;

/// The tab row over Effect Controls / Project (panel units).
pub(crate) const TABS: f32 = 22.0;
/// Most files one drop imports.
const MAX_DROP: usize = 256;
/// What Import… offers (EffectCraft imports these; anything else it reports).
const MEDIA: &[&str] = &[
    "mov", "mp4", "m4v", "mkv", "webm", "avi", "mxf", "mts", "gif", "png", "jpg", "jpeg", "webp", "tif", "tiff", "exr", "dpx", "tga", "bmp", "psd",
    "psb", "ai", "pdf", "eps", "svg", "wav", "mp3", "m4a", "aac", "aif", "aiff", "flac", "ogg", "json", "csv", "tsv", "glb", "gltf", "obj",
];

/// Which panel the column right of the Timeline shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Side {
    #[default]
    EffectControls,
    Project,
}

fn side_id(host: &EcHost) -> egui::Id {
    egui::Id::new(("figcraft-ec-side", host.path()))
}

/// The tab the column of `host`'s Timeline shows.
pub(crate) fn side_of(ctx: &egui::Context, host: &EcHost) -> Side {
    ctx.data(|d| d.get_temp::<Side>(side_id(host))).unwrap_or_default()
}

pub(crate) fn set_side(ctx: &egui::Context, host: &EcHost, side: Side) {
    ctx.data_mut(|d| d.insert_temp(side_id(host), side));
}

/// The panel a press in the column gives the keyboard (EffectCraft's shortcuts for it).
pub(crate) fn side_panel(ctx: &egui::Context, host: &EcHost) -> PanelKind {
    match side_of(ctx, host) {
        Side::EffectControls => PanelKind::EffectControls,
        Side::Project => PanelKind::Project,
    }
}

/// What the Project tab asked FigCraft to do (answered by [`frame`], after the panels drew).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ask {
    /// Create a new Composition: the settings dialog, then a card.
    NewComp,
    /// Show comp `0` of the project as a card with the Timeline on it.
    Card(u64),
    /// Import… : a file dialog, then `file.import`.
    Import,
    /// Reveal this footage file in the Finder.
    Reveal(String),
}

fn asks_id() -> egui::Id {
    egui::Id::new("figcraft-ec-project-asks")
}

fn push_asks(ctx: &egui::Context, asks: Vec<Ask>) {
    if asks.is_empty() {
        return;
    }
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<Ask>>(asks_id()).extend(asks));
}

fn take_asks(ctx: &egui::Context) -> Vec<Ask> {
    ctx.data_mut(|d| d.remove_temp::<Vec<Ask>>(asks_id())).unwrap_or_default()
}

/// Effect Controls or EffectCraft's Project panel in `rect` (the column right of the Timeline),
/// under a tab row. Drawn inside [`EcHost::show`] (its panic guard and read-only check).
pub(crate) fn side(host: &mut EcHost, ui: &mut egui::Ui, rect: Rect) {
    let ctx = ui.ctx().clone();
    let t = host.tokens();
    let tabs = Rect::from_min_size(rect.min, vec2(rect.width(), TABS));
    let body = Rect::from_min_max(pos2(rect.min.x, tabs.max.y + 1.0), rect.max);
    let painter = ui.painter().clone();
    painter.hline(tabs.x_range(), tabs.max.y, Stroke::new(1.0, t.separator));
    let mut side = side_of(&ctx, host);
    let mut x = tabs.min.x + 6.0;
    for (s, label, key) in [(Side::EffectControls, "Effect Controls", "effectControls"), (Side::Project, "Project", "project")] {
        let on = side == s;
        let font = if on { Tokens::semibold(11.5) } else { Tokens::ui(11.5) };
        let g = painter.layout_no_wrap(label.to_string(), font, if on { t.text } else { t.text_dim });
        let r = Rect::from_min_size(pos2(x, tabs.min.y), vec2(g.size().x + 16.0, TABS));
        let resp = ui.interact(r.intersect(tabs), egui::Id::new(("figcraft-ec-side-tab", key, host.path())), Sense::click());
        painter.galley(pos2(r.min.x + 8.0, r.center().y - g.size().y / 2.0), g, t.text);
        if on {
            painter.hline(r.x_range(), tabs.max.y - 1.0, Stroke::new(2.0, t.accent));
        } else if resp.hovered() {
            painter.hline(r.x_range(), tabs.max.y - 1.0, Stroke::new(1.0, t.text_dim));
        }
        host.auto_mut().add(&format!("figcraft.side.{key}"), r, label);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, label));
        if resp.clicked() {
            side = s;
        }
        x = r.max.x + 2.0;
    }
    set_side(&ctx, host, side);
    if body.height() <= 4.0 {
        return;
    }
    match side {
        Side::EffectControls => effectcraft_ui_egui::panels::effect_controls::show(host, ui, body),
        Side::Project => {
            let mut h = ProjectHost { host, asked: Vec::new() };
            effectcraft_ui_egui::panels::project::show(&mut h, ui, body);
            let asked = std::mem::take(&mut h.asked);
            push_asks(&ctx, asked);
        }
    }
}

/// The Project panel's host: the Timeline's host, with what EffectCraft's app does in its own
/// windows turned into [`Ask`]s for FigCraft.
pub(crate) struct ProjectHost<'a> {
    pub(crate) host: &'a mut EcHost,
    pub(crate) asked: Vec<Ask>,
}

/// `params` is `{}` or nothing.
fn empty(params: &Value) -> bool {
    params.is_null() || params.as_object().is_some_and(Map::is_empty)
}

impl TimelineHost for ProjectHost<'_> {
    fn session(&self) -> &Session {
        self.host.session()
    }
    fn session_mut(&mut self) -> &mut Session {
        self.host.session_mut()
    }
    fn ui(&self) -> &UiState {
        self.host.ui()
    }
    fn ui_mut(&mut self) -> &mut UiState {
        self.host.ui_mut()
    }
    fn auto(&self) -> &Registry {
        self.host.auto()
    }
    fn auto_mut(&mut self) -> &mut Registry {
        self.host.auto_mut()
    }
    fn tokens(&self) -> Tokens {
        self.host.tokens()
    }
    fn parts(&mut self) -> TimelineParts<'_> {
        self.host.parts()
    }
    fn id_scope(&self) -> Option<egui::Id> {
        self.host.id_scope()
    }
    fn invoke(&mut self, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
        match id {
            "comp.new" if empty(&params) => {
                self.asked.push(Ask::NewComp);
                Ok(json!({"dialog": id}))
            }
            "comp.open" | "comp.sendToFigCraft" => {
                let comp = params.get("comp").and_then(Value::as_u64).ok_or("no composition")?;
                if self.host.session().project.comp(ItemId(comp)).is_none() {
                    return Err("not a composition".into());
                }
                self.asked.push(Ask::Card(comp));
                Ok(json!({"card": comp}))
            }
            "file.import" if params.get("paths").or(params.get("path")).is_none() => {
                self.asked.push(Ask::Import);
                Ok(json!({"dialog": id}))
            }
            "file.revealInFinder" => {
                let s = self.host.session();
                let path = s.state.project_selection.iter().find_map(|i| match &s.project.item(*i)?.kind {
                    ItemKind::Footage(f) => Some(f.path.clone()),
                    _ => None,
                });
                let path = path.ok_or("select a footage item")?;
                self.asked.push(Ask::Reveal(path));
                Ok(Value::Null)
            }
            "footage.open" => Err("The Footage panel is EffectCraft's: open the project there to see the footage".into()),
            _ => self.host.invoke(ctx, id, params),
        }
    }
    fn run_engine(&mut self, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
        self.host.run_engine(ctx, id, params)
    }
    fn pointer_color(&mut self, ctx: &egui::Context) -> Option<PointerColor> {
        self.host.pointer_color(ctx)
    }
    /// The footage commands FigCraft can run (Interpret / Replace Footage: EffectCraft's dialogs,
    /// `effectcraft_dialogs`).
    fn menu_entry(&self, ui: &mut egui::Ui, command: &str, clicked: &mut Option<(String, Value)>) {
        let (label, keys) = match command {
            "file.interpretFootage" => ("Interpret Footage…", "⌥⌘G"),
            "file.replaceFootage" => ("Replace Footage…", "⌘H"),
            "file.reloadFootage" => ("Reload Footage", ""),
            "file.revealInFinder" => ("Reveal in Finder", ""),
            _ => return,
        };
        if ui.add(egui::Button::new(label).shortcut_text(keys)).clicked() {
            *clicked = Some((command.to_string(), json!({})));
            ui.close();
        }
    }
}

// ------------------------------------------------------------------------------------------
// Files dropped from the Finder

/// Where dropped files go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DropTo {
    /// The open Timeline or the column beside it.
    Panel,
    /// The card under the pointer.
    Card(NodeId),
}

/// Files the canvas left to a card or its Timeline this pass.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileDrop {
    pub(crate) paths: Vec<String>,
    pub(crate) at: Pos2,
    pub(crate) shift: bool,
    pub(crate) alt: bool,
    pub(crate) to: DropTo,
    /// The pass it was dropped in (a drop the Timeline didn't see drawn is dropped, not kept).
    pub(crate) pass: u64,
}

fn drop_id() -> egui::Id {
    egui::Id::new("figcraft-ec-file-drop")
}

/// Where files dropped (or dragged) at screen point `at` go, if a card or its Timeline takes them.
pub(crate) fn drop_target(app: &FigApp, at: Pos2) -> Option<DropTo> {
    if !at.is_finite() {
        return None;
    }
    let open = app.effectcraft.panel.as_ref().is_some_and(|p| p.open);
    if open && app.effectcraft.panel_rects.as_ref().is_some_and(|(_, rs)| rs.iter().any(|r| r.contains(at))) {
        return Some(DropTo::Panel);
    }
    let ds = app.session.ds().ok()?;
    let (doc, page) = (ds.doc.clone(), ds.page);
    let (id, _) = super::card_at(app, &doc, page, at)?;
    let c = doc.node(id)?.effectcraft.as_deref()?;
    (!c.project.is_empty() && Path::new(&c.project).is_file()).then_some(DropTo::Card(id))
}

/// The canvas's file-drop hook: files dropped on a card or its Timeline are theirs (imported into
/// the card's EffectCraft project by [`frame`] once the Timeline has drawn), not FigCraft's.
/// Returns the paths taken, which the canvas leaves alone.
pub(crate) fn claim_drops(app: &FigApp, ctx: &egui::Context) -> Vec<PathBuf> {
    let (files, at, shift, alt) = ctx.input(|i| {
        let at = i.pointer.hover_pos().or(i.pointer.latest_pos());
        (i.raw.dropped_files.clone(), at, i.modifiers.shift, i.modifiers.alt)
    });
    if files.is_empty() {
        return Vec::new();
    }
    let Some(to) = at.and_then(|a| drop_target(app, a)) else { return Vec::new() };
    let taken: Vec<PathBuf> = files.iter().map(|f| f.path().to_path_buf()).filter(|p| p.is_absolute()).take(MAX_DROP).collect();
    let paths: Vec<String> = taken.iter().filter_map(|p| p.to_str().map(str::to_string)).collect();
    if paths.is_empty() {
        return Vec::new();
    }
    let at = at.unwrap_or_default();
    ctx.data_mut(|d| d.insert_temp(drop_id(), FileDrop { paths, at, shift, alt, to, pass: ctx.cumulative_pass_nr() }));
    taken
}

/// The Timeline's layer (panel units → screen), as `effectcraft_panel_ui` draws it.
fn panel_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Middle, egui::Id::new("effectcraft_canvas_timeline"))
}

/// Screen point `p` in the Timeline's own coordinates.
fn panel_point(ctx: &egui::Context, p: Pos2) -> Pos2 {
    ctx.layer_transform_to_global(panel_layer()).map_or(p, |xf| xf.inverse() * p)
}

/// Import `params` (`file.import`'s) through the Timeline's host: errors the import lists come
/// back as one message.
fn import(host: &mut EcHost, ctx: &egui::Context, params: Value) -> Result<Value, String> {
    let r = host.invoke(ctx, "file.import", params)?;
    let errors: Vec<&str> = r.get("errors").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    if !errors.is_empty() && r.get("items").and_then(Value::as_array).is_none_or(Vec::is_empty) {
        return Err(errors.join("; "));
    }
    Ok(r)
}

/// Import a drop into the card's project: as layers of its comp where they landed.
pub(crate) fn drop_files(app: &mut FigApp, ctx: &egui::Context, d: FileDrop) -> Result<Value, String> {
    let mut params = json!({"paths": d.paths, "drag": true});
    if d.alt {
        params["sequence"] = json!(false);
    }
    match d.to {
        DropTo::Panel => {
            let p = app.effectcraft.panel.as_mut().filter(|p| p.open).ok_or("The EffectCraft Timeline closed")?;
            // Over the Timeline: where its line shows (EffectCraft's own placing); over Effect
            // Controls or the Project tab: into the project only.
            let map = effectcraft_ui_egui::os_drop::timeline_map(ctx).filter(|m| Some(m.comp) == p.host.session().active_comp_id());
            if let Some(t) = map.and_then(|m| m.target(panel_point(ctx, d.at), d.shift)) {
                params["addToComp"] = json!(true);
                params["index"] = json!(t.index);
                params["time"] = json!(t.time.map(|t| t.seconds()));
            }
            import(&mut p.host, ctx, params)
        }
        DropTo::Card(id) => {
            let doc = app.session.ds().map_err(|e| e.to_string())?.doc.clone();
            let c = doc.node(id).and_then(|n| n.effectcraft.as_deref()).cloned().ok_or("not an EffectCraft card")?;
            let pic = super::screen_rect(app, &doc, id).map(|r| super::fit(r, c.width, c.height)).ok_or("the card isn't on screen")?;
            let time = super::card_time(app, &doc, id, &c, None);
            let u = f64::from((d.at.x - pic.min.x) / pic.width().max(1.0)).clamp(0.0, 1.0);
            let v = f64::from((d.at.y - pic.min.y) / pic.height().max(1.0)).clamp(0.0, 1.0);
            if !app.effectcraft_open_panel(id) {
                return Err("This card's EffectCraft project can't be opened here".into());
            }
            let p = app.effectcraft.panel.as_mut().ok_or("no EffectCraft Timeline")?;
            if p.card != id {
                return Err("Save the EffectCraft edits first (⌘S in its Timeline)".into());
            }
            p.host.set_time(time);
            params["addToComp"] = json!(true);
            params["position"] = json!([u * f64::from(c.width), v * f64::from(c.height)]);
            import(&mut p.host, ctx, params)
        }
    }
}

/// While files are dragged over a card or the Timeline: an outline round what takes them.
fn drop_preview(app: &FigApp, ctx: &egui::Context) {
    let (hovering, at) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.pointer.hover_pos().or(i.pointer.latest_pos())));
    if !hovering {
        return;
    }
    let Some(at) = at else { return };
    let accent = crate::theme::Tokens::get(ctx).accent;
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("figcraft-ec-drop"))).with_clip_rect(app.canvas.rect);
    match drop_target(app, at) {
        Some(DropTo::Card(id)) => {
            if let Some(r) = app.session.doc().ok().and_then(|d| super::screen_rect(app, d, id)) {
                painter.rect_stroke(r.expand(2.0), 3.0, Stroke::new(2.0, accent), egui::StrokeKind::Outside);
            }
        }
        Some(DropTo::Panel) => {
            let xf = ctx.layer_transform_to_global(panel_layer()).unwrap_or_default();
            if let Some(map) = effectcraft_ui_egui::os_drop::timeline_map(ctx)
                && let Some(t) = map.target(panel_point(ctx, at), ctx.input(|i| i.modifiers.shift))
            {
                let r = map.rows_rect;
                painter.line_segment([xf * pos2(r.min.x, t.line_y), xf * pos2(r.max.x, t.line_y)], Stroke::new(2.0, accent));
                if let Some(x) = t.marker_x {
                    painter.line_segment([xf * pos2(x, r.min.y), xf * pos2(x, r.max.y)], Stroke::new(1.0, accent));
                }
            } else if let Some((_, rs)) = &app.effectcraft.panel_rects
                && let Some(r) = rs.iter().find(|r| r.contains(at))
            {
                painter.rect_stroke(*r, 3.0, Stroke::new(2.0, accent), egui::StrokeKind::Inside);
            }
        }
        None => return,
    }
    ctx.request_repaint();
}

/// A Project item dragged from the Project tab and let go over the Timeline's card: a layer
/// centred where it lands, at the card's time (EffectCraft's Composition panel drop).
fn item_dropped_on_card(app: &mut FigApp, ctx: &egui::Context) -> Result<(), String> {
    let Some(item) = egui::DragAndDrop::payload::<DragPayload>(ctx).and_then(|p| match *p {
        DragPayload::Item(i) => Some(i),
        _ => None,
    }) else {
        return Ok(());
    };
    let (released, at) = ctx.input(|i| (i.pointer.any_released(), i.pointer.hover_pos().or(i.pointer.latest_pos())));
    let Some(at) = at.filter(|_| released) else { return Ok(()) };
    let Some(card) = app.effectcraft.panel.as_ref().filter(|p| p.open).map(|p| p.card) else { return Ok(()) };
    if drop_target(app, at) != Some(DropTo::Card(card)) {
        return Ok(());
    }
    egui::DragAndDrop::clear_payload(ctx);
    let doc = app.session.ds().map_err(|e| e.to_string())?.doc.clone();
    let c = doc.node(card).and_then(|n| n.effectcraft.as_deref()).cloned().ok_or("not an EffectCraft card")?;
    let pic = super::screen_rect(app, &doc, card).map(|r| super::fit(r, c.width, c.height)).ok_or("the card isn't on screen")?;
    let time = super::card_time(app, &doc, card, &c, None);
    let u = f64::from((at.x - pic.min.x) / pic.width().max(1.0)).clamp(0.0, 1.0);
    let v = f64::from((at.y - pic.min.y) / pic.height().max(1.0)).clamp(0.0, 1.0);
    let p = app.effectcraft.panel.as_mut().ok_or("no EffectCraft Timeline")?;
    p.host.set_time(time);
    // A selected item carries the selection (folders and data files aren't layers).
    let s = p.host.session();
    let items: Vec<u64> = if s.state.project_selection.iter().any(|i| i.0 == item) {
        s.state.project_selection.iter().filter(|i| s.project.item(**i).is_some_and(|it| !it.is_folder())).map(|i| i.0).collect()
    } else {
        vec![item]
    };
    let position = json!([u * f64::from(c.width), v * f64::from(c.height)]);
    for i in items.into_iter().rev() {
        p.host.invoke(ctx, "layer.addItem", json!({"item": i, "position": position}))?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// New comps and comps as cards

/// Where a new comp goes: a project (`None`: a new one next to `next_to`, the FigCraft file), the
/// card it is placed beside, and `effectcraft.place` overrides (x, y, cardWidth).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Target {
    pub(crate) project: Option<PathBuf>,
    pub(crate) next_to: Option<PathBuf>,
    pub(crate) from: Option<NodeId>,
    pub(crate) place: Map<String, Value>,
}

impl Target {
    /// From an `effectcraft.newComp` request.
    pub(crate) fn of(data: &Value) -> Target {
        let path = |k: &str| data.get(k).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(PathBuf::from);
        Target {
            project: path("project"),
            next_to: path("nextTo"),
            from: data.get("from").and_then(figcraft_engine::params::id_of),
            place: data.get("place").and_then(Value::as_object).cloned().unwrap_or_default(),
        }
    }
}

/// The New Composition dialog while it is open.
#[derive(Clone, Debug)]
struct Dialog {
    draft: CompDraft,
    to: Target,
}

fn dialog_id() -> egui::Id {
    egui::Id::new("figcraft-ec-new-comp")
}

fn last_draft_id() -> egui::Id {
    egui::Id::new("figcraft-ec-new-comp-last")
}

/// Open EffectCraft's New Composition settings for a comp going to `to` (the last settings, as
/// EffectCraft keeps them, with the next free "Comp N" name).
pub(crate) fn open_dialog(app: &FigApp, ctx: &egui::Context, to: Target) {
    let comps = to
        .project
        .as_ref()
        .and_then(|p| app.effectcraft.panel.as_ref().filter(|q| q.host.path() == figcraft_ec_inproc::key(p)))
        .map_or(0, |q| q.host.session().project.comps().count());
    let last: CompDraft = ctx.data(|d| d.get_temp(last_draft_id())).unwrap_or_default();
    let draft = CompDraft { name: format!("Comp {}", comps.saturating_add(1)), start_tc: None, dur_tc: None, preset_name: None, ..last };
    ctx.data_mut(|d| d.insert_temp(dialog_id(), Dialog { draft, to }));
}

/// Whether the New Composition dialog is open.
#[cfg(test)]
pub(crate) fn dialog_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<Dialog>(dialog_id())).is_some()
}

/// One frame of the New Composition dialog (EffectCraft's own settings, `comp_settings::show`).
fn new_comp_dialog(app: &mut FigApp, ctx: &egui::Context) {
    let Some(mut dlg) = ctx.data(|d| d.get_temp::<Dialog>(dialog_id())) else { return };
    let t = Tokens::for_kind(ThemeKind::Dark);
    let mut auto = Registry::default();
    // The project's own presets when its Timeline is open (saved ones go to that session).
    let key = dlg.to.project.as_deref().map(figcraft_ec_inproc::key);
    let custom = app
        .effectcraft
        .panel
        .as_ref()
        .filter(|p| key.as_deref() == Some(p.host.path()))
        .map(|p| p.host.session().prefs.comp_presets.clone())
        .unwrap_or_default();
    let mut cmds: Vec<(&'static str, Value)> = Vec::new();
    let mut result = None;
    let frame = egui::Frame::window(&ctx.style_of(ctx.theme())).fill(t.panel_bg).inner_margin(egui::Margin::same(18));
    let shown = egui::Modal::new(dialog_id().with("modal")).frame(frame).show(ctx, |ui| {
        ui.set_max_width(620.0);
        ui.label(egui::RichText::new("New Composition").strong().size(14.0));
        ui.add_space(6.0);
        result = comp_settings::show(ui, &mut dlg.draft, &t, &mut auto, &custom, &mut cmds);
    });
    // Escape or a click beside it cancels, as Cancel does.
    if result.is_none() && dlg.draft.preset_name.is_none() && shown.should_close() {
        result = Some(Value::Null);
    }
    if dlg.draft.preset_name.is_some() {
        result = None;
        egui::Modal::new(dialog_id().with("name")).frame(frame).show(ctx, |ui| {
            ui.set_max_width(360.0);
            comp_settings::choose_name(ui, &mut dlg.draft, &t, &mut auto, &custom, &mut cmds);
        });
    }
    let preset_busy = !cmds.is_empty() || dlg.draft.preset_name.is_some();
    for (id, params) in cmds {
        if let Some(p) = app.effectcraft.panel.as_mut().filter(|p| key.as_deref() == Some(p.host.path())) {
            let _ = p.host.session_mut().execute(id, params);
        }
    }
    if result.is_none() && !preset_busy && !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
        result = Some(comp_settings::params(&dlg.draft));
    }
    match result {
        None => ctx.data_mut(|d| {
            d.insert_temp(dialog_id(), dlg);
        }),
        Some(Value::Null) => ctx.data_mut(|d| {
            d.remove::<Dialog>(dialog_id());
        }),
        Some(params) => {
            ctx.data_mut(|d| {
                d.remove::<Dialog>(dialog_id());
                d.insert_temp(last_draft_id(), dlg.draft.clone());
            });
            if let Err(e) = create(app, ctx, dlg.to, params) {
                app.toast(e);
            }
        }
    }
}

/// A comp's card: its name, size, frame rate and duration, from `session`.
pub(crate) fn card_params(session: &Session, project: &Path, comp: u64) -> Option<Value> {
    let c = session.project.comp(ItemId(comp))?;
    let name = session.project.item(ItemId(comp))?.name.clone();
    let fps = c.frame_rate.as_f64();
    let fps = if fps.is_finite() && fps > 0.0 { fps } else { 29.97 };
    let dur = c.duration.seconds();
    let end = if dur.is_finite() && dur > 0.0 { dur.min(figcraft_doc::effectcraft::MAX_SPAN) } else { 1.0 / fps };
    Some(json!({"comp": name, "compId": comp, "project": project.to_string_lossy(), "start": 0.0, "end": end, "fps": fps,
        "width": c.width.max(1), "height": c.height.max(1)}))
}

/// Place a card from `params` ([`card_params`]): at the `place` overrides, else right of card
/// `from` (and its Timeline), else where `effectcraft.place` puts it. Returns the card.
fn place_card(app: &mut FigApp, mut params: Value, from: Option<NodeId>, place: &Map<String, Value>) -> Result<NodeId, String> {
    if let (Some(from), Ok(doc), None) = (from, app.session.doc(), place.get("x")) {
        let open = app.effectcraft.panel.as_ref().is_some_and(|p| p.open && p.card == from);
        if let Some(b) = doc.world_bounds(from) {
            let right = super::strips::geom_of(doc, from, open, 0).map_or(b.x1, |g| g.whole().x1.max(b.x1));
            let gap = (b.width() * 0.08).max(16.0);
            let cw = b.width().max(64.0);
            let (w, h) = (params.get("width").and_then(Value::as_f64).unwrap_or(16.0), params.get("height").and_then(Value::as_f64).unwrap_or(9.0));
            let ch = if w > 0.0 && h > 0.0 && (h / w).is_finite() { cw * h / w } else { cw * 9.0 / 16.0 };
            let open_card = app.effectcraft.panel.as_ref().filter(|p| p.open).map(|p| p.card);
            params["x"] = json!(free_x(doc, from, open_card, right + gap, b.y0, cw, ch, gap));
            params["y"] = json!(b.y0);
            params["cardWidth"] = json!(cw);
        }
    }
    if let Some(o) = params.as_object_mut() {
        o.extend(place.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let r = app.run("effectcraft.place", params).ok_or("The card couldn't be placed")?;
    let card = r.get("id").and_then(figcraft_engine::params::id_of).ok_or_else(|| "The card couldn't be placed".to_string())?;
    // The new card and its Timeline come into view.
    super::cards::reveal(app, card);
    Ok(card)
}

/// The first x from `x` rightwards where a new card (on the page of node `near`) at (x, `y`), `w` × `h` page units, with its
/// Timeline and Effect Controls open, covers nothing on its page (cards with their Timelines or
/// strips, other layers), keeping `gap` (regression: a precomp's card landed on the card already
/// right of its source).
pub(crate) fn free_x(doc: &figcraft_doc::Document, near: NodeId, open: Option<NodeId>, mut x: f64, y: f64, w: f64, h: f64, gap: f64) -> f64 {
    use figcraft_geom::Rect as PRect;
    let Some(page) = doc.page_of(near).and_then(|p| doc.node(p)) else { return x };
    let taken: Vec<PRect> = page
        .children
        .iter()
        .filter_map(|id| {
            let b = doc.world_bounds(*id)?;
            let strip = doc.node(*id)?.effectcraft.as_ref().and_then(|_| super::strips::geom_of(doc, *id, open == Some(*id), 3)).map(|g| g.whole());
            Some(strip.map_or(b, |s| s.union(b)))
        })
        .collect();
    for _ in 0..256 {
        let me = super::strips::geom(PRect::new(x, y, x + w, y + h), super::strips::Size::default(), true, 0).whole().union(PRect::new(
            x,
            y,
            x + w,
            y + h,
        ));
        let hit = taken.iter().filter(|t| t.intersect(me).area() > 0.0).map(|t| t.x1).fold(f64::NAN, f64::max);
        if !hit.is_finite() {
            return x;
        }
        x = hit + gap;
    }
    x
}

/// The open Timeline now edits comp `comp` on card `card` (same project: same session, unsaved
/// edits and all).
fn switch(app: &mut FigApp, card: NodeId, comp: u64) {
    if let Some(p) = app.effectcraft.panel.as_mut() {
        p.host.set_comp(comp);
        p.card = card;
        p.open = true;
        p.focused = true;
        p.synced = None;
    }
    super::cards::reveal(app, card);
}

/// A card on the page showing comp `comp` of project `key` (`comp` 0: any comp of it).
fn card_of(app: &FigApp, key: &Path, comp: u64) -> Option<NodeId> {
    let ds = app.session.ds().ok()?;
    let page = ds.page;
    super::cards(&ds.doc, page).into_iter().find(|id| {
        ds.doc
            .node(*id)
            .and_then(|n| n.effectcraft.as_deref())
            .is_some_and(|c| !c.project.is_empty() && figcraft_ec_inproc::key(Path::new(&c.project)) == key && (comp == 0 || c.comp_id == comp))
    })
}

/// Comp `comp` of the Timeline's project as a card with the Timeline on it: its card on the page,
/// else a new one right of the Timeline's card.
pub(crate) fn show_comp(app: &mut FigApp, comp: u64) -> Result<NodeId, String> {
    let p = app.effectcraft.panel.as_ref().ok_or("no EffectCraft Timeline")?;
    let (path, from) = (p.path(), p.card);
    if p.host.comp() == comp && p.open {
        return Ok(from);
    }
    let card = match card_of(app, &path, comp) {
        Some(c) => c,
        None => {
            let params = card_params(p.host.session(), &path, comp).ok_or("no such composition")?;
            place_card(app, params, Some(from), &Map::new())?
        }
    };
    switch(app, card, comp);
    Ok(card)
}

/// A file name free next to the FigCraft file `next_to`: `<name>.ecproj`, `<name> 2.ecproj`…
pub(crate) fn new_project_path(next_to: &Path) -> Result<PathBuf, String> {
    let dir = next_to.parent().filter(|d| d.is_dir()).ok_or("save this file first: the new EffectCraft project goes next to it")?;
    let stem = next_to.file_stem().and_then(|s| s.to_str()).filter(|s| !s.is_empty()).unwrap_or("Untitled");
    (1..1000)
        .map(|n| if n == 1 { dir.join(format!("{stem}.ecproj")) } else { dir.join(format!("{stem} {n}.ecproj")) })
        .find(|p| !p.exists())
        .ok_or_else(|| "no free name for the new EffectCraft project".to_string())
}

/// Make a comp with EffectCraft's `comp.new` `params` where `to` says, place it as a card and
/// open its Timeline. Returns the card.
pub(crate) fn create(app: &mut FigApp, ctx: &egui::Context, to: Target, params: Value) -> Result<NodeId, String> {
    let mut params = if params.is_object() { params } else { json!({}) };
    // EffectCraft reads the frame size as whole pixels.
    for k in ["width", "height"] {
        if let Some(x) = params.get(k).and_then(Value::as_f64).filter(|x| x.is_finite() && *x >= 1.0) {
            params[k] = json!(x.round().min(30_000.0) as u64);
        }
    }
    let project = match &to.project {
        Some(p) => p.clone(),
        None => new_project_path(to.next_to.as_deref().ok_or("save this file first: the new EffectCraft project goes next to it")?)?,
    };
    let key = figcraft_ec_inproc::key(&project);
    // The Timeline isn't on that project yet: open it on a card of it, when the page has one.
    if app.effectcraft.panel.as_ref().is_none_or(|p| p.host.path() != key)
        && let Some(card) = card_of(app, &key, 0)
    {
        app.effectcraft_open_panel(card);
    }
    if let Some(p) = app.effectcraft.panel.as_mut().filter(|p| p.host.path() == key) {
        // An edit of the Timeline's session (unsaved until ⌘S, undone with ⌘Z there).
        let r = p.host.invoke(ctx, "comp.new", params)?;
        let comp = r.get("comp").and_then(Value::as_u64).ok_or("EffectCraft made no composition")?;
        let card_p = card_params(p.host.session(), &key, comp).ok_or("EffectCraft made no composition")?;
        let from = to.from.or(Some(p.card));
        let card = place_card(app, card_p, from, &to.place)?;
        switch(app, card, comp);
        return Ok(card);
    }
    // Nobody edits it here: a comp saved in the project at once (a new project, or one with no
    // card on this page), as long as the EffectCraft app doesn't have it open.
    let comp = if project.exists() {
        if !matches!(super::io::app_state_now(&key), super::io::AppState::Closed | super::io::AppState::Other) {
            return Err("EffectCraft has this project open: make the comp there".into());
        }
        let mut ec = figcraft_ec_inproc::InProc::open(&key)?;
        let r = ec.session_mut().execute("comp.new", params).map_err(|e| e.to_string())?;
        let comp = r.get("comp").and_then(Value::as_u64).ok_or("EffectCraft made no composition")?;
        ec.save()?;
        comp
    } else {
        figcraft_ec_inproc::create_project(&project, &params)?
    };
    let key = figcraft_ec_inproc::key(&project);
    let ec = figcraft_ec_inproc::InProc::open(&key)?;
    let card_p = card_params(ec.session(), &key, comp).ok_or("EffectCraft made no composition")?;
    let card = place_card(app, card_p, to.from, &to.place)?;
    app.effectcraft_open_panel(card);
    Ok(card)
}

/// An `effectcraft.newComp` request: the dialog first (`dialog.effectcraftNewComp`), or the comp
/// at once with its settings.
pub(crate) fn request(app: &mut FigApp, kind: &str, data: &Value) {
    let ctx = app.ui_context.clone().unwrap_or_default();
    let to = Target::of(data);
    if kind.starts_with("dialog.") {
        open_dialog(app, &ctx, to);
        return;
    }
    let params = data.get("comp").cloned().unwrap_or_else(|| json!({}));
    if let Err(e) = create(app, &ctx, to, params) {
        app.toast(e);
    }
}

/// Answer one of the Project tab's asks.
fn answer(app: &mut FigApp, ctx: &egui::Context, ask: Ask) -> Result<(), String> {
    match ask {
        Ask::NewComp => {
            let p = app.effectcraft.panel.as_ref().ok_or("no EffectCraft Timeline")?;
            let to = Target { project: Some(p.path()), from: Some(p.card), ..Target::default() };
            open_dialog(app, ctx, to);
        }
        Ask::Card(comp) => {
            show_comp(app, comp)?;
        }
        Ask::Import => {
            let path = app.effectcraft.panel.as_ref().map(|p| p.path()).ok_or("no EffectCraft Timeline")?;
            app.pick_open("Import into EffectCraft", MEDIA, move |app, file| {
                let ctx = app.ui_context.clone().unwrap_or_default();
                let Some(p) = app.effectcraft.panel.as_mut().filter(|p| p.path() == path) else { return };
                let Some(f) = file.to_str().map(str::to_string) else { return };
                if let Err(e) = import(&mut p.host, &ctx, json!({"paths": [f]})) {
                    app.toast(e);
                }
            });
        }
        Ask::Reveal(path) => reveal(&path)?,
    }
    Ok(())
}

/// Show a file in the Finder.
fn reveal(path: &str) -> Result<(), String> {
    if !Path::new(path).exists() {
        return Err(format!("{path} isn't there any more"));
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg("-R").arg(path).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let dir = Path::new(path).parent().ok_or("no folder")?;
        std::process::Command::new("xdg-open").arg(dir).spawn().map(|_| ()).map_err(|e| e.to_string())
    }
}

/// Once a frame, after the Timeline drew: files dropped on a card or the Timeline, the Project
/// tab's asks, a Project item let go over the card, and the New Composition dialog.
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context) {
    drop_preview(app, ctx);
    if let Some(d) = ctx
        .data_mut(|d| {
            let taken = d.get_temp::<FileDrop>(drop_id());
            d.remove::<FileDrop>(drop_id());
            taken
        })
        .filter(|d| d.pass == ctx.cumulative_pass_nr())
        && let Err(e) = drop_files(app, ctx, d)
    {
        app.toast(e);
    }
    for ask in take_asks(ctx) {
        if let Err(e) = answer(app, ctx, ask) {
            app.toast(e);
        }
    }
    if let Err(e) = item_dropped_on_card(app, ctx) {
        app.toast(e);
    }
    new_comp_dialog(app, ctx);
    // Pre-compose, Interpret Footage, Replace Footage.
    super::dialogs::frame(app, ctx);
}

#[cfg(test)]
#[path = "effectcraft_project_tests.rs"]
mod tests;

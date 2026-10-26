//! Fork-only (`effectcraft-inproc`): EffectCraft's real Timeline on FigCraft's canvas, right under
//! the EffectCraft card that was double-clicked (or whose closed strip was clicked), editing its
//! comp. Where it sits and how it scales with the canvas: `effectcraft_ui::strips`.
//!
//! The panel is `effectcraft_ui_egui::panels::timeline::show` drawn against [`EcHost`], a
//! [`TimelineHost`] over an in-process EffectCraft session of the card's saved `.ecproj`.
//!
//! - **Who edits.** Asked once, when the panel opens:
//!   - The EffectCraft app doesn't have the project open: the session edits the file's project.
//!     Edits mark it unsaved, the card re-renders live (its frames render from this session's
//!     snapshot, `io::Job::Override`), and ⌘S while the panel has focus saves the `.ecproj` with
//!     EffectCraft's own (atomic) save.
//!   - The app has it open: the session is a **mirror** of the app's project
//!     ([`mirror`], decision and protocol there). Every edit here lands in the app as an undo
//!     step of its own (unsaved there, drawn there at once), the app's edits show here live,
//!     ⌘Z / ⇧⌘Z undo and redo in the app's history and ⌘S saves in the app. The app wins any
//!     disagreement: the mirror takes the app's project again. Until it is in step with the app,
//!     while the app doesn't answer, or when it has another project open, the panel is
//!     read-only (it says why).
//!   - The app can't say what it has open, or can't mirror (an older EffectCraft): read-only,
//!     "open in EffectCraft — edit there". Read-only means anything that changes the project is
//!     undone at once.
//! - **Pointer.** Clicks, drags and scroll over the Timeline and its Effect Controls are theirs
//!   (the canvas leaves them alone); ⌘-scroll and pinch still zoom the canvas, and Space held
//!   pans it.
//! - **Keys.** While the panel has focus (the last click was in it) its keys go to EffectCraft's
//!   own shortcut dispatch (`panels::timeline::keys::forward_shortcuts`) with Elias's own
//!   EffectCraft shortcuts (read from EffectCraft's settings folder, never written; EffectCraft's
//!   defaults without them); Space plays the card and ⌘S saves. The Timeline commands EffectCraft's
//!   app runs itself (reveal keys P S R T…, U, zoom - =, D, B N, [ ], Graph Editor…) run through
//!   `panels::timeline::hosted`. They are taken before FigCraft's shortcuts, so P, R, T… work on
//!   the canvas again as soon as it is clicked.
//! - **Expanded precomps.** With a precomp's row expanded in the Timeline focused, edits go to that
//!   precomp (`comp.inlineEdit`, as in EffectCraft); edits that can't be redirected are refused
//!   with a status message, never applied to the card's comp.
//! - **One playhead.** The card's playhead and the Timeline's current time follow each other:
//!   scrubbing either moves both, and Space plays the card with the Timeline following.
//! - **Undo.** ⌘Z / ⇧⌘Z (⌘Y) undo and redo EffectCraft's edits while the panel has focus (the
//!   app's history when it has the project open) and FigCraft's otherwise; neither reaches the
//!   other's history. Each press on the panel starts a new undo step (a drag is one step), as in
//!   EffectCraft.
//! - Effect Controls (the selected layer's effects) sit to the right of the Timeline, on the
//!   canvas too.
//! - **Graph Editor and speed curves.** The header's Graph button or Shift+F3 shows EffectCraft's
//!   Graph Editor in the Timeline's place (F fits its graphs); the footer's ∿ button shows speed
//!   curves in the property rows (on by default). Both edit through this host like any Timeline
//!   edit: same undo, read-only guard and card refresh.
//! - What the Timeline still needs EffectCraft's whole app for (suggestions overlay, RAM chip,
//!   EffectCraft's own dialogs…, `docs/embedding-panels.md` in EffectCraft) is left out by the
//!   panel itself.

use std::path::{Path, PathBuf};

/// Fork: the panel's session as a mirror of the EffectCraft app's project.
#[path = "effectcraft_mirror.rs"]
pub(crate) mod mirror;

use effectcraft_ui_egui::automation::Registry;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::timeline::{PointerColor, TimelineHost, TimelineParts, hosted};
use effectcraft_ui_egui::state::UiState;
use effectcraft_ui_egui::theme::{ThemeKind, Tokens};
use figcraft_doc::{EcComp, NodeId};
use figcraft_ec_inproc::InProc;
use figcraft_ec_inproc::effectcraft_engine::Session;
use figcraft_ec_inproc::effectcraft_engine::project::ItemId;
use serde_json::{Value, json};

/// What read-only panels say.
pub const READ_ONLY: &str = "Open in EffectCraft — edit there";

/// The Timeline's host: the session, the frontend state the panel keeps, and whether it may edit.
pub struct EcHost {
    ec: InProc,
    ui: UiState,
    auto: Registry,
    tokens: Tokens,
    /// `Some(why)`: nothing may change the project.
    read_only: Option<String>,
    comp: u64,
    /// The session revision the card's frames were last pointed at.
    pub(crate) shown_revision: u64,
    /// The panel with the keyboard (Timeline or Effect Controls).
    focus: PanelKind,
    /// What FigCraft's canvas shows under the pointer this frame, for an armed eyedropper.
    pub(crate) pointer: Option<PointerColor>,
    /// The app has the project open: the session mirrors it ([`mirror`]).
    pub(crate) mirror: Option<mirror::Mirror>,
}

impl EcHost {
    /// Open the card's project and comp. `app_owns`: the EffectCraft app has the project open
    /// (or can't say), so the panel is read-only.
    pub fn open(path: &Path, comp: &EcComp, app_owns: bool) -> Result<EcHost, String> {
        let mut ec = InProc::open(path)?;
        bind_keys(ec.session_mut());
        let comp_ref = if comp.comp_id > 0 { json!(comp.comp_id) } else { json!(comp.comp) };
        let id = ec.comp_id(&comp_ref)?;
        ec.session_mut().open_comp(ItemId(id));
        let ui = UiState { focused: PanelKind::Timeline, ..UiState::default() };
        let shown_revision = ec.session().revision;
        Ok(EcHost {
            ec,
            ui,
            auto: Registry::default(),
            tokens: Tokens::for_kind(ThemeKind::Dark),
            read_only: app_owns.then(|| READ_ONLY.to_string()),
            comp: id,
            shown_revision,
            focus: PanelKind::Timeline,
            pointer: None,
            mirror: None,
        })
    }

    /// Open the card's comp as a mirror of the EffectCraft app's open project, reached through
    /// `transport` ([`mirror::Threaded`] over [`mirror::Tcp`] in the app). Read-only until the
    /// first pull arrives ([`EcHost::sync`]).
    pub fn open_mirror(path: &Path, comp: &EcComp, transport: Box<dyn mirror::Transport>) -> Result<EcHost, String> {
        let mut h = EcHost::open(path, comp, false)?;
        let m = mirror::Mirror::new(h.ec.path(), transport);
        h.read_only = m.read_only();
        h.mirror = Some(m);
        Ok(h)
    }

    /// [`EcHost::open_mirror`] over the app's control channel, its requests on a thread of their own.
    pub fn open_mirror_tcp(path: &Path, comp: &EcComp, ctx: Option<egui::Context>) -> Result<EcHost, String> {
        let t = mirror::Threaded::start(Box::new(mirror::Tcp::new()), ctx).ok_or("EffectCraft mirror: couldn't start its thread")?;
        EcHost::open_mirror(path, comp, Box::new(t))
    }

    /// Mirroring the app's project: one step of it, each frame (see [`mirror::Mirror::sync`]).
    /// `true`: the app's project changed (its frames are new). Standalone: nothing.
    pub fn sync(&mut self) -> bool {
        let Some(m) = self.mirror.as_mut() else { return false };
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| m.sync(self.ec.session_mut()))).unwrap_or_else(|_| {
            log::warn!("EffectCraft mirror: an internal error");
            mirror::Outcome::default()
        });
        self.read_only = m.read_only();
        if let Some(st) = out.status {
            self.ui.status = st;
        }
        out.app_changed
    }

    /// The session mirrors the app's project.
    pub fn mirrored(&self) -> bool {
        self.mirror.is_some()
    }

    /// A mirror that lost its app (it answers no more, or has another project open): opening
    /// the Timeline again starts afresh.
    pub fn stale(&self) -> bool {
        self.mirror.as_ref().is_some_and(|m| matches!(m.state, mirror::State::Down(_) | mirror::State::Elsewhere))
    }

    /// The card's frames render from this session (its own edits); a mirror's come from the app.
    pub fn renders_here(&self) -> bool {
        self.read_only.is_none() && self.mirror.is_none()
    }

    pub fn read_only(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    pub fn path(&self) -> &Path {
        self.ec.path()
    }

    /// Unsaved edits only this panel has (a mirror's are the app's to save).
    pub fn is_dirty(&self) -> bool {
        self.read_only.is_none() && self.mirror.is_none() && self.ec.is_dirty()
    }

    /// What the card's frames render from while this host has edits.
    pub fn snapshot(&self) -> figcraft_ec_inproc::Snapshot {
        self.ec.snapshot()
    }

    /// Save over the `.ecproj`. `app_owns_now`: asked again just before (the app may have opened it
    /// since the panel did).
    pub fn save(&mut self, app_owns_now: bool) -> Result<(), String> {
        // A mirror: the app saves its project.
        if let Some(m) = self.mirror.as_mut() {
            return m.forward("file.save", json!({}));
        }
        if let Some(why) = &self.read_only {
            return Err(why.clone());
        }
        if app_owns_now {
            self.read_only = Some(READ_ONLY.to_string());
            return Err("EffectCraft has this project open now: edit and save it there".into());
        }
        self.ec.save()
    }

    /// Read-only: undo whatever changed the project (it stays as saved).
    fn keep_read_only(&mut self, before: u64) {
        if self.read_only.is_none() || self.ec.session().revision == before {
            return;
        }
        // A mirror: the app's project as last seen.
        if let Some(m) = self.mirror.as_mut()
            && m.revert(self.ec.session_mut())
        {
            self.ui.status = self.read_only.clone().unwrap_or_default();
            return;
        }
        let _ = self.ec.session_mut().execute("edit.undo", json!({}));
        if self.ec.session().revision != before && self.ec.session().is_dirty() {
            // Undo couldn't take it back: the project as saved.
            if self.ec.reload().is_ok() {
                bind_keys(self.ec.session_mut());
                self.ec.session_mut().open_comp(ItemId(self.comp));
            }
        }
        self.ui.status = READ_ONLY.to_string();
    }

    /// Run engine command `id` like a menu item would, read-only kept. While an edit of a precomp
    /// expanded in the Timeline runs ([`hosted::inline`]), it goes to that precomp
    /// (`comp.inlineEdit`, [`hosted::engine_command`]) as in EffectCraft.
    fn guarded_run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        // A mirror: undo, redo and save run in the app (its history, its file).
        if let Some(m) = self.mirror.as_mut()
            && mirror::FORWARDED.contains(&id)
        {
            return m.forward(id, params).map(|()| json!({"forwarded": true}));
        }
        let before = self.ec.session().revision;
        let (cmd, params) = hosted::engine_command(self, id, params);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.ec.session_mut().execute(&cmd, params).map_err(|e| e.to_string())))
            .unwrap_or_else(|_| Err(format!("EffectCraft: {id} failed (an internal error)")));
        self.keep_read_only(before);
        // U's revealed properties, toasts (the rest of the session's events are app chrome).
        hosted::handle_events(self);
        if self.read_only.is_some() && self.ec.session().revision != before {
            return Err(READ_ONLY.to_string());
        }
        r
    }

    /// One frame of the panels: the Timeline in `timeline`, Effect Controls in `fx` (when given).
    /// Never panics into FigCraft.
    pub fn show(&mut self, ui: &mut egui::Ui, timeline: egui::Rect, fx: Option<egui::Rect>) {
        let before = self.ec.session().revision;
        self.auto.begin_frame();
        // A press starts a new undo step (EffectCraft's app does the same with each press), so
        // two drags of one property are two steps, here and in the app a mirror pushes to.
        if ui.input(|i| i.pointer.any_pressed())
            && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| timeline.contains(p) || fx.is_some_and(|f| f.contains(p)))
        {
            self.ec.session_mut().history.merge_key = None;
        }
        let drew = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(f) = fx {
                // Effect Controls, or EffectCraft's Project panel on its tab beside them.
                crate::effectcraft_ui::project::side(self, ui, f);
            }
            effectcraft_ui_egui::panels::timeline::show(self, ui, timeline);
        }));
        if drew.is_err() {
            log::warn!("EffectCraft Timeline: an internal error drawing the panel");
            self.ui.status = "The EffectCraft Timeline hit an internal error".into();
        }
        self.keep_read_only(before);
    }

    /// The panel's keys this frame (it has focus): Space plays the card and ⌘S saves (FigCraft
    /// does those), everything else goes through EffectCraft's own shortcut dispatch
    /// (`forward_shortcuts`: its keymap, nothing while a text field, menu or dialog has the
    /// keyboard). The keys are taken, so FigCraft's shortcuts don't see them.
    pub fn keys(&mut self, ctx: &egui::Context) -> Vec<PanelAction> {
        let mut out = Vec::new();
        if !ctx.egui_wants_keyboard_input() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space)) {
                out.push(PanelAction::Play);
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
                out.push(PanelAction::Save);
            }
            // Undo is EffectCraft's here, whatever its keymap says: ⌘Z never reaches FigCraft's
            // undo while the panel has focus (and FigCraft's never reaches this session).
            let redo_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
            for (m, k, id) in [
                (redo_shift, egui::Key::Z, "edit.redo"),
                (egui::Modifiers::COMMAND, egui::Key::Y, "edit.redo"),
                (egui::Modifiers::COMMAND, egui::Key::Z, "edit.undo"),
            ] {
                // ⌘Y is EffectCraft's New Solid (its default keymap): redo only when its keymap
                // leaves ⌘Y free.
                if k == egui::Key::Y
                    && (!ctx.input(|i| i.key_pressed(k))
                        || self.ec.session().shortcuts().bindings().iter().any(|(s, b)| *s == "Cmd+Y" && b.command != "edit.redo"))
                {
                    continue;
                }
                if ctx.input_mut(|i| i.consume_key(m, k)) {
                    if let Err(e) = self.guarded_run(id, json!({})) {
                        self.ui.status = e;
                    }
                    out.push(PanelAction::Ran(id.to_string()));
                }
            }
        }
        // A session opened again (read-only taken back) needs the Timeline's keys again.
        if !hosted::installed(self.ec.session()) {
            bind_keys(self.ec.session_mut());
        }
        let focus = self.focus;
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            effectcraft_ui_egui::panels::timeline::keys::forward_shortcuts(self, ctx, focus)
        }))
        .unwrap_or_default();
        out.extend(ran.into_iter().map(PanelAction::Ran));
        out
    }

    /// The comp's work area and duration (comp seconds), as edited here (B/N, the work-area bar).
    pub fn work_area(&self) -> Option<((f64, f64), f64)> {
        let c = self.ec.session().project.comp(ItemId(self.comp))?;
        Some(((c.work_area.0.seconds(), c.work_area.1.seconds()), c.duration.seconds()))
    }

    /// The comp the Timeline edits (its id in the project).
    pub fn comp(&self) -> u64 {
        self.comp
    }

    /// Edit comp `id` of this project from now on (a comp made or opened from the Project tab
    /// becomes a card): same session, unsaved edits and all.
    pub fn set_comp(&mut self, id: u64) {
        if self.ec.session().project.comp(ItemId(id)).is_some() {
            self.ec.session_mut().open_comp(ItemId(id));
            self.comp = id;
        }
    }

    /// The Timeline's current time (comp seconds).
    pub fn time(&self) -> f64 {
        self.ec.session().time().seconds()
    }

    /// Move the Timeline's current time to comp second `t` (snapped to a frame, kept in the comp).
    pub fn set_time(&mut self, t: f64) {
        if t.is_finite() {
            self.ec.session_mut().set_time(figcraft_ec_inproc::effectcraft_engine::time::Tick::from_seconds_f64(t));
        }
    }

    /// A colour eyedropper (Effect Controls or text colour) is armed: the next canvas click takes
    /// a colour instead of acting on the canvas.
    pub fn picking(&self) -> bool {
        self.ui.fx_pick.as_ref().is_some_and(|p| p.kind == "color") || self.ui.text_color_pick.is_some()
    }

    /// Run the armed eyedropper with `pointer` (the canvas colour under the pointer; `clicked`
    /// takes it). Returns whether one is still armed.
    pub fn pick(&mut self, ctx: &egui::Context, pointer: Option<PointerColor>) -> bool {
        self.pointer = pointer;
        let before = self.ec.session().revision;
        let armed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| effectcraft_ui_egui::panels::effect_controls::hosted_pick(self, ctx)))
            .unwrap_or(false);
        self.pointer = None;
        self.keep_read_only(before);
        armed
    }

    /// The colour of comp frame `frame` at comp pixel (`x`, `y`) of comp `comp` (id or name) as
    /// this session renders it (the card shows the same frame), 0–1 per channel.
    pub fn comp_color(&self, comp: &Value, frame: i64, x: f64, y: f64) -> Option<[f32; 3]> {
        let snap = self.ec.snapshot();
        let id = snap.comp_id(comp).ok()?;
        let c = self.ec.session().project.comp(ItemId(id))?;
        let (w, h) = (c.width.max(1), c.height.max(1));
        let side = w.max(h).min(4096);
        let figcraft_ec_inproc::Picture::Png(png) = snap.frame(id, frame, side, figcraft_ec_inproc::Compositor::Cpu).ok()? else { return None };
        let img = image::load_from_memory(&png).ok()?.to_rgb8();
        let px = ((x / f64::from(w)) * f64::from(img.width())).floor().clamp(0.0, f64::from(img.width().saturating_sub(1))) as u32;
        let py = ((y / f64::from(h)) * f64::from(img.height())).floor().clamp(0.0, f64::from(img.height().saturating_sub(1))) as u32;
        let p = img.get_pixel_checked(px, py)?;
        Some([f32::from(p[0]) / 255.0, f32::from(p[1]) / 255.0, f32::from(p[2]) / 255.0])
    }

    /// Which of the panels has the keyboard.
    pub fn set_focus(&mut self, panel: PanelKind) {
        self.focus = panel;
        self.ui.focused = panel;
    }
}

/// The Timeline's own commands bound in `session`, with Elias's EffectCraft shortcuts (read from
/// EffectCraft's settings folder, never written) when he has any, else EffectCraft's defaults.
fn bind_keys(session: &mut Session) {
    hosted::install(session);
    if !hosted::use_user_shortcuts(session) {
        log::info!("EffectCraft Timeline: no EffectCraft shortcuts found, using the default keys");
    }
}

/// What a panel key asks FigCraft to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PanelAction {
    Play,
    Save,
    /// An EffectCraft command ran.
    Ran(String),
}

impl TimelineHost for EcHost {
    fn session(&self) -> &Session {
        self.ec.session()
    }
    fn session_mut(&mut self) -> &mut Session {
        self.ec.session_mut()
    }
    fn ui(&self) -> &UiState {
        &self.ui
    }
    fn ui_mut(&mut self) -> &mut UiState {
        &mut self.ui
    }
    fn auto(&self) -> &Registry {
        &self.auto
    }
    fn auto_mut(&mut self) -> &mut Registry {
        &mut self.auto
    }
    fn tokens(&self) -> Tokens {
        self.tokens
    }
    fn parts(&mut self) -> TimelineParts<'_> {
        TimelineParts { session: self.ec.session_mut(), ui: &mut self.ui, auto: &mut self.auto, tokens: self.tokens }
    }
    fn id_scope(&self) -> Option<egui::Id> {
        Some(egui::Id::new(("figcraft-effectcraft-timeline", self.ec.path())))
    }
    /// As EffectCraft's menus: edits in an expanded precomp go to it ([`hosted::inline`]), the
    /// Timeline's UI commands (Graph Editor, zoom, reveal keys…) run on the panel
    /// ([`hosted::run`]), everything else on the session, read-only kept.
    fn invoke(&mut self, ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
        if let Some(r) = hosted::inline(self, ctx, id, &params) {
            return r;
        }
        // ⌘N (New Composition without settings): EffectCraft's dialog, the comp becomes a card.
        if let Some(r) = crate::effectcraft_ui::cards::intercept(self, ctx, id, &params) {
            return r;
        }
        // Pre-compose, Interpret / Replace Footage without their settings: EffectCraft's dialogs.
        if let Some(r) = crate::effectcraft_ui::dialogs::intercept(self, ctx, id, &params) {
            return r;
        }
        if let Some(r) = hosted::run(self, ctx, id, &params) {
            return r;
        }
        self.guarded_run(id, params)
    }
    /// The colour FigCraft's canvas shows under the pointer ([`EcHost::pointer`], set by the app
    /// each frame while a colour eyedropper is armed).
    fn pointer_color(&mut self, _ctx: &egui::Context) -> Option<PointerColor> {
        self.pointer
    }
    fn run_engine(&mut self, _ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
        let r = self.guarded_run(id, params);
        if let Err(e) = &r {
            self.ui.status = e.clone();
        }
        r
    }
}

/// The open Timeline: its host (kept while closed, unsaved edits and all), the card it edits,
/// whether it is showing and has the keyboard.
pub struct Panel {
    pub host: EcHost,
    pub card: NodeId,
    pub open: bool,
    pub focused: bool,
    /// The card's and the Timeline's times as last made the same (comp seconds).
    pub synced: Option<(f64, f64)>,
}

impl Panel {
    pub fn path(&self) -> PathBuf {
        self.host.path().to_path_buf()
    }
}

#[cfg(test)]
#[path = "effectcraft_panel_tests.rs"]
mod tests;

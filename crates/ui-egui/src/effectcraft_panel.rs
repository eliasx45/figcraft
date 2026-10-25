//! Fork-only (`effectcraft-inproc`): EffectCraft's real Timeline panel docked at the bottom of
//! FigCraft, editing the comp of the EffectCraft card that was double-clicked.
//!
//! The panel is `effectcraft_ui_egui::panels::timeline::show` drawn against [`EcHost`], a
//! [`TimelineHost`] over an in-process EffectCraft session of the card's saved `.ecproj`.
//!
//! - **Who edits.** The session may edit only when the EffectCraft app doesn't have that project
//!   open (asked once, when the panel opens). Then edits mark the project unsaved, the card
//!   re-renders live (its frames render from this session's snapshot, `io::Job::Override`), and
//!   ⌘S while the panel has focus saves the `.ecproj` with EffectCraft's own (atomic) save. When
//!   the app has it open, or can't say what it has open, the panel is read-only: it says "open in
//!   EffectCraft — edit there", and anything that changes the project is undone at once.
//! - **Keys.** While the panel has focus (the last click was in it) its keys go to EffectCraft's
//!   own shortcut dispatch (`panels::timeline::keys::forward_shortcuts`); Space plays the card and
//!   ⌘S saves. They are taken before FigCraft's shortcuts, so P, R, T… work on the canvas again
//!   as soon as it is clicked.
//! - Effect Controls (the selected layer's effects) dock left of the Timeline when there is room.
//! - What the Timeline still needs EffectCraft's whole app for (Graph Editor, markers, dialogs…,
//!   `docs/embedding-panels.md` in EffectCraft) is left out by the panel itself.

use std::path::{Path, PathBuf};

use effectcraft_ui_egui::automation::Registry;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::timeline::{TimelineHost, TimelineParts};
use effectcraft_ui_egui::state::UiState;
use effectcraft_ui_egui::theme::{ThemeKind, Tokens};
use figcraft_doc::{EcComp, NodeId};
use figcraft_ec_inproc::InProc;
use figcraft_ec_inproc::effectcraft_engine::Session;
use figcraft_ec_inproc::effectcraft_engine::project::ItemId;
use serde_json::{Value, json};

/// Effect Controls' width beside the Timeline, and the panel width it needs to show.
const EFFECT_CONTROLS_WIDTH: f32 = 300.0;
const EFFECT_CONTROLS_MIN_ROOM: f32 = 900.0;

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
    /// The docked panel with the keyboard (Timeline or Effect Controls).
    focus: PanelKind,
}

impl EcHost {
    /// Open the card's project and comp. `app_owns`: the EffectCraft app has the project open
    /// (or can't say), so the panel is read-only.
    pub fn open(path: &Path, comp: &EcComp, app_owns: bool) -> Result<EcHost, String> {
        let mut ec = InProc::open(path)?;
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
        })
    }

    pub fn read_only(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    pub fn path(&self) -> &Path {
        self.ec.path()
    }

    pub fn is_dirty(&self) -> bool {
        self.read_only.is_none() && self.ec.is_dirty()
    }

    /// What the card's frames render from while this host has edits.
    pub fn snapshot(&self) -> figcraft_ec_inproc::Snapshot {
        self.ec.snapshot()
    }

    /// Save over the `.ecproj`. `app_owns_now`: asked again just before (the app may have opened it
    /// since the panel did).
    pub fn save(&mut self, app_owns_now: bool) -> Result<(), String> {
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
        let _ = self.ec.session_mut().execute("edit.undo", json!({}));
        if self.ec.session().revision != before && self.ec.session().is_dirty() {
            // Undo couldn't take it back: the project as saved.
            if self.ec.reload().is_ok() {
                self.ec.session_mut().open_comp(ItemId(self.comp));
            }
        }
        self.ui.status = READ_ONLY.to_string();
    }

    /// Run `id` like a menu item would, read-only kept.
    fn guarded_run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        let before = self.ec.session().revision;
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.ec.session_mut().execute(id, params).map_err(|e| e.to_string())))
            .unwrap_or_else(|_| Err(format!("EffectCraft: {id} failed (an internal error)")));
        self.keep_read_only(before);
        if self.read_only.is_some() && self.ec.session().revision != before {
            return Err(READ_ONLY.to_string());
        }
        r
    }

    /// One frame of the panels in `rect`: Effect Controls on the left (when there is room), the
    /// Timeline in the rest. Returns the Effect Controls' rectangle. Never panics into FigCraft.
    pub fn show(&mut self, ui: &mut egui::Ui, rect: egui::Rect) -> Option<egui::Rect> {
        let before = self.ec.session().revision;
        self.auto.begin_frame();
        let fx = (rect.width() >= EFFECT_CONTROLS_MIN_ROOM).then(|| egui::Rect::from_min_size(rect.min, egui::vec2(EFFECT_CONTROLS_WIDTH, rect.height())));
        let timeline = match fx {
            Some(f) => egui::Rect::from_min_max(egui::pos2(f.max.x + 1.0, rect.min.y), rect.max),
            None => rect,
        };
        let drew = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(f) = fx {
                effectcraft_ui_egui::panels::effect_controls::show(self, ui, f);
            }
            effectcraft_ui_egui::panels::timeline::show(self, ui, timeline);
        }));
        if drew.is_err() {
            log::warn!("EffectCraft Timeline: an internal error drawing the panel");
            self.ui.status = "The EffectCraft Timeline hit an internal error".into();
        }
        self.keep_read_only(before);
        fx
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
        }
        let focus = self.focus;
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| effectcraft_ui_egui::panels::timeline::keys::forward_shortcuts(self, ctx, focus)))
        .unwrap_or_default();
        out.extend(ran.into_iter().map(PanelAction::Ran));
        out
    }

    /// Which of the docked panels has the keyboard.
    pub fn set_focus(&mut self, panel: PanelKind) {
        self.focus = panel;
        self.ui.focused = panel;
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
    fn invoke(&mut self, _ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
        self.guarded_run(id, params)
    }
    fn run_engine(&mut self, _ctx: &egui::Context, id: &str, params: Value) -> Result<Value, String> {
        let r = self.guarded_run(id, params);
        if let Err(e) = &r {
            self.ui.status = e.clone();
        }
        r
    }
}

/// The docked panel: its host (kept while hidden, unsaved edits and all), the card it edits,
/// whether it is showing and has the keyboard.
pub struct Panel {
    pub host: EcHost,
    pub card: NodeId,
    pub open: bool,
    pub focused: bool,
    pub height: f32,
}

impl Panel {
    pub fn path(&self) -> PathBuf {
        self.host.path().to_path_buf()
    }
}

#[cfg(test)]
#[path = "effectcraft_panel_tests.rs"]
mod tests;

//! Fork-only (`effectcraft-inproc`): EffectCraft's own Pre-compose, Interpret Footage and Replace
//! Footage dialogs for the comp a card's Timeline edits, shown in FigCraft windows.
//!
//! **Entry point (the card's right-click menu, keys, the Timeline's and Project tab's menus):**
//!
//! ```ignore
//! crate::effectcraft_ui::dialogs::request(app: &mut FigApp, card: Option<NodeId>, which: Which)
//!     -> Result<(), String>
//! // Which::{Precompose, InterpretFootage, ReplaceFootage}
//! ```
//!
//! `card`: the card the menu was opened on; its Timeline opens first when it isn't the open one
//! (`None`: the open Timeline). The dialog acts on that Timeline's selection (layers for
//! Pre-compose; for the footage dialogs the footage of the first selected footage layer when the
//! Timeline has the keyboard, else the Project tab's selected footage, else that layer's). Errors
//! are for a toast ("select the layers to pre-compose"). The dialog shows from the next frame on.
//!
//! The same dialogs come up when EffectCraft's own commands run in the hosted panels without their
//! parameters, as in EffectCraft's app ([`intercept`], called by `EcHost::invoke`):
//! `layer.precompose` without `name` (⌘⇧C, the Timeline's context menu), `file.interpretFootage`
//! / `file.interpretProxy` without settings (⌘⌥G, the Project tab's menu and Interpret button),
//! `file.replaceFootage` without `path` (⌘H, the Project tab's menu). With their parameters they
//! run at once, so programmatic calls never open a dialog; FigCraft's control channel has no
//! route to them at all.
//!
//! The dialog code is EffectCraft's (EMBED10: `panels::precomp::{draft_for, draw}`,
//! `panels::forms::{build_form, draw_form}`); OK runs the command through the Timeline's host, so
//! it is one undo step there (⌘Z / ⌘S in the Timeline, the app's history when the panel mirrors
//! the EffectCraft app), read-only is respected and the card re-renders. After Pre-compose with
//! Open New Composition ticked, the new comp becomes a card right of the Timeline's card with the
//! Timeline on it (as `effectcraft.newComp` does); unticked, the Timeline stays on its comp and the
//! precomp is there for the card's explode. Replace Footage asks for a file with FigCraft's file
//! dialog, then runs EffectCraft's `file.replaceFootage` on the item: every layer using it shows
//! the new file.

use std::path::{Path, PathBuf};

use effectcraft_ui_egui::automation::Registry;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::forms::{self, Built, Form};
use effectcraft_ui_egui::panels::precomp::{self, PrecomposeDraft};
use effectcraft_ui_egui::panels::timeline::TimelineHost;
use effectcraft_ui_egui::theme::{ThemeKind, Tokens};
use figcraft_doc::NodeId;
use figcraft_ec_inproc::effectcraft_engine::project::{ItemId, ItemKind};
use serde_json::{Value, json};

use crate::FigApp;
use crate::effectcraft_panel::EcHost;

/// What Replace Footage offers (EffectCraft imports these).
const MEDIA: &[&str] = &[
    "mov", "mp4", "m4v", "mkv", "webm", "avi", "mxf", "mts", "gif", "png", "jpg", "jpeg", "webp", "tif", "tiff", "exr", "dpx", "tga", "bmp", "psd",
    "psb", "ai", "pdf", "eps", "svg", "wav", "mp3", "m4a", "aac", "aif", "aiff", "flac", "ogg",
];

/// One of the hosted EffectCraft dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Which {
    /// Layer ▸ Pre-compose… (⌘⇧C).
    Precompose,
    /// File ▸ Interpret Footage ▸ Main… (⌘⌥G).
    InterpretFootage,
    /// File ▸ Replace Footage ▸ File… (⌘H): a file dialog, then EffectCraft's replace.
    ReplaceFootage,
}

/// The dialog open (or the file dialog to start) for the Timeline of project `path`.
#[derive(Clone, Debug)]
pub(crate) struct Shown {
    path: PathBuf,
    kind: Kind,
}

#[derive(Clone, Debug)]
enum Kind {
    Precompose(PrecomposeDraft),
    /// Interpret Footage (or Proxy): EffectCraft's form, run with its command.
    Form(Form),
    /// Replace Footage of this item: FigCraft's file dialog first.
    Replace(u64),
}

fn shown_id() -> egui::Id {
    egui::Id::new("figcraft-ec-dialog")
}

/// The dialog open, if any.
pub(crate) fn shown(ctx: &egui::Context) -> Option<Shown> {
    ctx.data(|d| d.get_temp::<Shown>(shown_id()))
}

fn set_shown(ctx: &egui::Context, s: Option<Shown>) {
    ctx.data_mut(|d| match s {
        Some(s) => {
            d.insert_temp(shown_id(), s);
        }
        None => {
            d.remove::<Shown>(shown_id());
        }
    });
}

/// Which dialog EffectCraft command `id` with `params` raises (none when its parameters are given).
fn which_of(id: &str, params: &Value) -> Option<Which> {
    match id {
        "layer.precompose" if params.get("name").is_none() => Some(Which::Precompose),
        "file.interpretFootage" | "file.interpretProxy" => Some(Which::InterpretFootage),
        "file.replaceFootage" if ["path", "paths"].iter().all(|k| params.get(*k).is_none()) => Some(Which::ReplaceFootage),
        _ => None,
    }
}

/// The footage item the footage dialogs act on: the one `params` names (`item` / `items`), else
/// the first selected footage layer's (when the Timeline has the keyboard) or the Project tab's
/// selected footage, then the other.
fn footage_item(host: &EcHost, params: &Value) -> Option<ItemId> {
    let s = host.session();
    let footage = |i: ItemId| matches!(s.project.item(i).map(|x| &x.kind), Some(ItemKind::Footage(_)));
    let named = match params.get("items").or(params.get("item")) {
        Some(Value::Array(a)) => a.first().and_then(Value::as_u64),
        Some(v) => v.as_u64(),
        None => None,
    };
    if let Some(i) = named {
        return Some(ItemId(i)).filter(|i| footage(*i));
    }
    let layer = || {
        let c = s.active_comp()?;
        s.state.selected_layers.iter().find_map(|l| c.layer(*l)?.source.item().filter(|i| footage(*i)))
    };
    let project = || s.state.project_selection.iter().copied().find(|i| footage(*i));
    if host.ui().focused == PanelKind::Project { project().or_else(layer) } else { layer().or_else(project) }
}

/// Open dialog `which` for `host`'s comp, as EffectCraft command `id` with `params` asked
/// (`params` without the dialog's settings). Errors say why not (nothing selected, read-only).
fn open(host: &EcHost, ctx: &egui::Context, which: Which, id: &str, params: &Value) -> Result<Value, String> {
    if let Some(why) = host.read_only() {
        return Err(why.to_string());
    }
    let kind = match which {
        Which::Precompose => Kind::Precompose(precomp::draft_for(host.session(), params)?),
        Which::InterpretFootage => {
            let item = footage_item(host, params).ok_or("Interpret Footage: select a footage item")?;
            let mut p = if params.is_object() { params.clone() } else { json!({}) };
            p["items"] = json!([item.0]);
            if let Some(o) = p.as_object_mut() {
                o.remove("item");
            }
            match forms::build_form(host.session(), id, &p) {
                Some(Built::Form(f)) => Kind::Form(f),
                Some(Built::Info(_, body)) => return Err(body),
                None => return Err("Interpret Footage: this item has nothing to interpret".into()),
            }
        }
        Which::ReplaceFootage => Kind::Replace(footage_item(host, params).ok_or("Replace Footage: select a footage item")?.0),
    };
    set_shown(ctx, Some(Shown { path: host.path().to_path_buf(), kind }));
    Ok(json!({"dialog": id}))
}

/// `EcHost::invoke`: an EffectCraft command that opens one of these dialogs in EffectCraft's app
/// opens it here (`None`: run it as it is).
pub(crate) fn intercept(host: &EcHost, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let which = which_of(id, params)?;
    // Interpret Footage with its settings given runs as it is.
    if which == Which::InterpretFootage && params.as_object().is_some_and(|o| o.keys().any(|k| k != "item" && k != "items")) {
        return None;
    }
    Some(open(host, ctx, which, id, params))
}

/// Open dialog `which` for card `card`'s Timeline (the open one when `None`). The card's
/// right-click menu calls this; the dialog shows from the next frame on.
pub(crate) fn request(app: &mut FigApp, card: Option<NodeId>, which: Which) -> Result<(), String> {
    if let Some(card) = card
        && app.effectcraft.panel.as_ref().is_none_or(|p| p.card != card || !p.open)
        && !app.effectcraft_open_panel(card)
    {
        return Err("Save the EffectCraft project first: this card has no project to edit".into());
    }
    let ctx = app.ui_context.clone().unwrap_or_default();
    let p = app.effectcraft.panel.as_ref().ok_or("Open the card's EffectCraft Timeline first")?;
    let id = match which {
        Which::Precompose => "layer.precompose",
        Which::InterpretFootage => "file.interpretFootage",
        Which::ReplaceFootage => "file.replaceFootage",
    };
    open(&p.host, &ctx, which, id, &json!({})).map(|_| ())
}

/// Run EffectCraft command `id` on the Timeline of project `path` (its host: undo, read-only,
/// mirror and card refresh as any Timeline edit).
fn run(app: &mut FigApp, ctx: &egui::Context, path: &Path, id: &str, params: Value) -> Result<Value, String> {
    let p = app.effectcraft.panel.as_mut().filter(|p| p.host.path() == path).ok_or("The EffectCraft Timeline closed")?;
    p.host.invoke(ctx, id, params)
}

/// OK in Pre-compose: EffectCraft's `layer.precompose` with the dialog's settings; with Open New
/// Composition the precomp becomes a card with the Timeline on it. Returns the new comp.
pub(crate) fn precompose(app: &mut FigApp, ctx: &egui::Context, path: &Path, d: &PrecomposeDraft) -> Result<u64, String> {
    let keep = app.effectcraft.panel.as_ref().map(|p| p.host.comp());
    let r = run(app, ctx, path, "layer.precompose", precomp::draft_params(d))?;
    let comp = r.get("comp").and_then(Value::as_u64).ok_or("EffectCraft made no composition")?;
    if d.open {
        crate::effectcraft_ui::project::show_comp(app, comp)?;
    } else if let (Some(keep), Some(p)) = (keep, app.effectcraft.panel.as_mut()) {
        // The Timeline stays on its comp.
        p.host.set_comp(keep);
    }
    Ok(comp)
}

/// Replace footage `item` of the Timeline of project `path` with `file` (EffectCraft's
/// `file.replaceFootage`: every layer using the item shows the new file).
pub(crate) fn replace(app: &mut FigApp, ctx: &egui::Context, path: &Path, item: u64, file: &Path) -> Result<Value, String> {
    let f = file.to_str().ok_or("That file name can't be read")?;
    run(app, ctx, path, "file.replaceFootage", json!({"item": item, "path": f}))
}

/// What one frame of a dialog decided.
enum Outcome {
    Open,
    Cancel,
    Ok,
}

/// The window the dialogs show in (as FigCraft's New Composition dialog).
fn modal(ctx: &egui::Context, salt: &str, title: &str, t: &Tokens, width: f32, body: impl FnOnce(&mut egui::Ui)) -> bool {
    let frame = egui::Frame::window(&ctx.style_of(ctx.theme())).fill(t.panel_bg).inner_margin(egui::Margin::same(18));
    let shown = egui::Modal::new(shown_id().with(salt)).frame(frame).show(ctx, |ui| {
        ui.set_max_width(width);
        ui.label(egui::RichText::new(title).strong().size(14.0));
        ui.add_space(6.0);
        body(ui);
    });
    shown.should_close()
}

fn enter(ctx: &egui::Context) -> bool {
    !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Enter))
}

/// Once a frame (after the Timeline drew): the dialog open, its OK and Cancel.
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context) {
    let Some(mut s) = shown(ctx) else { return };
    // The Timeline it was opened for has gone (closed for another project): nothing to do.
    if app.effectcraft.panel.as_ref().is_none_or(|p| p.host.path() != s.path) {
        set_shown(ctx, None);
        return;
    }
    let t = Tokens::for_kind(ThemeKind::Dark);
    let mut auto = Registry::default();
    let outcome = match &mut s.kind {
        Kind::Replace(item) => {
            let (item, path) = (*item, s.path.clone());
            set_shown(ctx, None);
            app.pick_open("Replace Footage", MEDIA, move |app, file| {
                let ctx = app.ui_context.clone().unwrap_or_default();
                if let Err(e) = replace(app, &ctx, &path, item, &file) {
                    app.toast(e);
                }
            });
            return;
        }
        Kind::Precompose(d) => {
            let mut a = precomp::Answer::default();
            let close = modal(ctx, "precompose", "Pre-compose", &t, 520.0, |ui| a = precomp::draw(ui, d, &t, &mut auto));
            decide(a.ok || enter(ctx), a.cancel || close)
        }
        Kind::Form(f) => {
            let (mut ok, mut cancel) = (false, false);
            let (mut regs, mut browse) = (Vec::new(), None);
            let title = f.title.clone();
            let max_h = (ctx.content_rect().height() - 200.0).max(80.0);
            let close = modal(ctx, "form", &title, &t, 480.0, |ui| (ok, cancel) = forms::draw_form(ui, f, &t, max_h, &mut regs, &mut browse));
            decide(ok || enter(ctx), cancel || close)
        }
    };
    match outcome {
        Outcome::Open => set_shown(ctx, Some(s)),
        Outcome::Cancel => set_shown(ctx, None),
        Outcome::Ok => {
            set_shown(ctx, None);
            if let Err(e) = finish(app, ctx, &s) {
                app.toast(e);
            }
        }
    }
}

fn decide(ok: bool, cancel: bool) -> Outcome {
    if ok {
        Outcome::Ok
    } else if cancel {
        Outcome::Cancel
    } else {
        Outcome::Open
    }
}

/// OK: run what dialog `s` says.
pub(crate) fn finish(app: &mut FigApp, ctx: &egui::Context, s: &Shown) -> Result<(), String> {
    match &s.kind {
        Kind::Precompose(d) => precompose(app, ctx, &s.path, d).map(|_| ()),
        Kind::Form(f) => run(app, ctx, &s.path, &f.command, f.params()).map(|_| ()),
        Kind::Replace(_) => Ok(()),
    }
}

#[cfg(test)]
impl Shown {
    pub(crate) fn precompose_draft(&mut self) -> Option<&mut PrecomposeDraft> {
        match &mut self.kind {
            Kind::Precompose(d) => Some(d),
            _ => None,
        }
    }
    pub(crate) fn form(&mut self) -> Option<&mut Form> {
        match &mut self.kind {
            Kind::Form(f) => Some(f),
            _ => None,
        }
    }
    pub(crate) fn replace_item(&self) -> Option<u64> {
        match self.kind {
            Kind::Replace(i) => Some(i),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "effectcraft_dialogs_tests.rs"]
mod tests;

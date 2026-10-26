//! Fork-only (`effectcraft-inproc`): the right-click menu of the card the EffectCraft Timeline
//! edits is EffectCraft's own Composition viewer menu.
//!
//! - **Which menu.** A right-click on the picture of the card whose Timeline is open (and may
//!   edit) picks the layer under the pointer as EffectCraft's viewer does (one already selected
//!   keeps the group) and opens EffectCraft's viewer context menu: Select ▸ (the layers under the
//!   pointer), the layer entries (New ▸, Masks ▸, Transform ▸, Time ▸, Markers ▸, Blending Mode ▸,
//!   Layer Styles ▸, Effects ▸, Arrange ▸, Create Shapes/Masks from Text or Vector, Pre-compose…,
//!   Duplicate, Split Layer, Delete) and the comp's (Copy Frame to Clipboard, Composition
//!   Settings…). On an empty part of the card the layer part says "No unlocked layer selected".
//! - **Hosted, not rewritten.** The entries are EffectCraft's own menu code
//!   (`effectcraft_ui_egui::panels::viewer_menu`, EMBED11, shared with its Composition panel).
//!   What they choose runs through the Timeline's host ([`crate::effectcraft_panel::EcHost`]):
//!   the same command, one undo step (⌘Z / ⌘S in the Timeline; the app's history when it mirrors
//!   the EffectCraft app), the card re-renders.
//! - **Dialogs.** Pre-compose… is EffectCraft's dialog through [`super::dialogs`] (DIALOGS);
//!   Composition Settings… is EffectCraft's own (`comp_settings::show`) in a FigCraft window, as
//!   New Composition is ([`super::project`]). Entries whose dialog isn't hosted yet (New ▸
//!   Solid…, Light…, Camera…, Layer Settings…, Time Stretch…, Layer Style Options…) show disabled
//!   with the reason on hover ([`gate`]); Send to FigCraft too (the comp is already here). New ▸
//!   Text puts the caret on the card, as EffectCraft's menu does in its viewer.
//! - **FigCraft's own menu** stays reachable: on any card whose Timeline isn't open, with the
//!   Timeline read-only, or with ⌥ (Alt) held, a right-click shows FigCraft's canvas menu for the
//!   card as before.
//! - Nothing is drawn over the picture but the menu itself, while it is open.

use effectcraft_ui_egui::automation::Registry;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::comp_settings::{self, CompDraft};
use effectcraft_ui_egui::panels::timeline::{TimelineHost, TimelineParts};
use effectcraft_ui_egui::panels::viewer::ViewerMap;
use effectcraft_ui_egui::panels::viewer_hosted as vh;
use effectcraft_ui_egui::panels::viewer_menu as vm;
use effectcraft_ui_egui::theme::{ThemeKind, Tokens};
use egui::Pos2;
use figcraft_doc::NodeId;
use figcraft_ec_inproc::effectcraft_engine::Event as EcEvent;
use figcraft_ec_inproc::effectcraft_engine::project::ItemId;
use serde_json::{Value, json};

use crate::FigApp;
use crate::effectcraft_panel::EcHost;

/// The open menu: where, on which card, and what the right-click found.
#[derive(Clone, Debug)]
pub(crate) struct Open {
    pub(crate) pos: Pos2,
    pub(crate) card: NodeId,
    pub(crate) target: vm::Target,
}

/// Composition Settings while it is open: the card, the comp it edits and the dialog's draft.
#[derive(Clone, Debug)]
struct CompDialog {
    card: NodeId,
    comp: u64,
    draft: CompDraft,
}

pub(crate) fn popup_id() -> egui::Id {
    egui::Id::new("figcraft-ec-card-menu")
}

fn open_id() -> egui::Id {
    popup_id().with("open")
}

fn dialog_id() -> egui::Id {
    egui::Id::new("figcraft-ec-comp-settings")
}

/// The menu open on a card, if any.
pub(crate) fn open_menu(ctx: &egui::Context) -> Option<Open> {
    ctx.data(|d| d.get_temp::<Open>(open_id()))
}

/// Whether Composition Settings is open.
#[cfg(test)]
pub(crate) fn dialog_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<CompDialog>(dialog_id())).is_some()
}

fn close(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Open>(open_id()));
    egui::Popup::close_id(ctx, popup_id());
}

/// Entries whose EffectCraft dialog FigCraft doesn't host yet: `Some(why)` shows them disabled.
pub(crate) fn gate(cmd: &str) -> Option<String> {
    let why = match cmd {
        "app.solidSettings" => "EffectCraft's Solid Settings isn't on the card yet: add the solid in EffectCraft",
        "layer.settings" => "EffectCraft's Layer Settings isn't on the card yet: open it in EffectCraft",
        "layer.newLight" | "layer.newCamera" => "Its settings dialog isn't on the card yet: add it in EffectCraft",
        "layer.timeStretch" => "EffectCraft's Time Stretch isn't on the card yet: use it in EffectCraft",
        "layer.style.options" => "EffectCraft's Layer Style options aren't on the card yet: open them in EffectCraft",
        "comp.sendToFigCraft" => "This comp is already on FigCraft's canvas",
        _ => return None,
    };
    Some(why.to_string())
}

/// The card whose Timeline is open and may edit, when screen point `pos` is on its picture (and
/// not on the Timeline or its side panels), with the picture's map.
fn over_card(app: &FigApp, pos: Pos2) -> Option<(NodeId, ViewerMap)> {
    if !pos.x.is_finite() || !pos.y.is_finite() || !app.canvas.rect.contains(pos) {
        return None;
    }
    let p = app.effectcraft.panel.as_ref().filter(|p| p.open)?;
    if p.host.read_only().is_some() || app.effectcraft.picking || app.effectcraft.scrub.is_some() {
        return None;
    }
    if app.effectcraft.panel_rects.as_ref().is_some_and(|(_, rs)| rs.iter().any(|r| r.contains(pos))) {
        return None;
    }
    let doc = app.session.doc().ok()?;
    let c = doc.node(p.card).and_then(|n| n.effectcraft.as_deref())?;
    if !super::handles::shows_active_comp(p.host.session(), c) {
        return None;
    }
    let (pic, map) = super::viewer::picture(app, p.card, &p.host)?;
    pic.contains(pos).then_some((p.card, map))
}

/// Whether a right-click at `pos` (with `mods` held) is EffectCraft's: the canvas then leaves it
/// alone (FigCraft's own menu doesn't open).
pub(crate) fn claims(app: &FigApp, pos: Pos2, mods: egui::Modifiers) -> bool {
    !mods.alt && over_card(app, pos).is_some()
}

/// What is under `pos` is the canvas (or the card's viewer tools over it), not a panel or a
/// window over the canvas.
fn on_canvas_layer(ctx: &egui::Context, pos: Pos2) -> bool {
    ctx.layer_id_at(pos).is_none_or(|l| l.order == egui::Order::Background || l.id == egui::Id::new("effectcraft_card_tools"))
}

/// One frame: a right-click on the card opens the menu, the open menu draws and runs what was
/// chosen, Composition Settings draws. After the Timeline's panel. Never panics into FigCraft.
pub(crate) fn frame(app: &mut FigApp, ctx: &egui::Context) {
    open_on_right_click(app, ctx);
    show_menu(app, ctx);
    comp_settings_dialog(app, ctx);
}

fn open_on_right_click(app: &mut FigApp, ctx: &egui::Context) {
    let (clicked, pos, mods) = ctx.input(|i| (i.pointer.secondary_clicked(), i.pointer.interact_pos(), i.modifiers));
    let Some(pos) = pos.filter(|_| clicked) else { return };
    if !on_canvas_layer(ctx, pos) {
        return;
    }
    let Some((card, map)) = over_card(app, pos) else { return };
    if mods.alt {
        // FigCraft's own menu for the card (the canvas opens it, unless a viewer tool held the
        // pointer there).
        if app.ui.context_menu.is_none() {
            crate::canvas::open_context_menu(app, pos, mods.command);
        }
        return;
    }
    let Some(cpt) = vh::comp_point(&map, pos) else { return };
    let Some(p) = app.effectcraft.panel.as_mut() else { return };
    let host = &mut p.host;
    let target = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| vm::press(host, cpt))).ok().flatten();
    let Some(target) = target else { return };
    // The menu's edits are EffectCraft's: the Timeline has the keyboard (⌘Z undoes them there).
    p.focused = true;
    p.host.set_focus(PanelKind::Timeline);
    app.ui.context_menu = None;
    ctx.data_mut(|d| d.insert_temp(open_id(), Open { pos, card, target }));
    egui::Popup::open_id(ctx, popup_id());
}

fn show_menu(app: &mut FigApp, ctx: &egui::Context) {
    let Some(open) = open_menu(ctx) else { return };
    let id = popup_id();
    let Some(p) = app.effectcraft.panel.as_mut().filter(|p| p.open && p.card == open.card) else {
        close(ctx);
        return;
    };
    if !egui::Popup::is_id_open(ctx, id) {
        close(ctx);
        return;
    }
    let mut actions: Vec<(String, Value)> = Vec::new();
    let TimelineParts { session, auto, .. } = p.host.parts();
    let project = &session.project;
    let Some(comp) = vm::comp_of(project, &open.target) else {
        close(ctx);
        return;
    };
    let layers = vm::context_layers(comp, &session.state.selected_layers);
    let labels = vm::Labels::default();
    let drew = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        egui::Popup::new(id, ctx.clone(), egui::PopupAnchor::Position(open.pos), egui::LayerId::new(egui::Order::Foreground, id))
            .kind(egui::PopupKind::Menu)
            .layout(egui::Layout::top_down_justified(egui::Align::Min))
            .style(effectcraft_ui_egui::widgets::menu_style)
            .gap(0.0)
            .open_memory(None)
            .show(|ui| vm::contents(ui, project, comp, &open.target, &layers, &labels, auto, &gate, &mut actions));
    }));
    if drew.is_err() {
        log::warn!("EffectCraft card menu: an internal error");
        close(ctx);
        return;
    }
    if !egui::Popup::is_id_open(ctx, id) || !actions.is_empty() {
        close(ctx);
    }
    for (cmd, params) in actions {
        if let Err(e) = run(app, ctx, open.card, &cmd, params) {
            app.toast(e);
        }
    }
}

/// Run menu entry `cmd` for card `card` as EffectCraft's viewer does: through the Timeline's
/// host (one undo step), its dialogs through FigCraft's windows.
pub(crate) fn run(app: &mut FigApp, ctx: &egui::Context, card: NodeId, cmd: &str, params: Value) -> Result<(), String> {
    if let Some(why) = gate(cmd) {
        return Err(why);
    }
    if app.effectcraft.panel.as_ref().is_none_or(|p| !p.open || p.card != card) {
        return Err("The EffectCraft Timeline closed".into());
    }
    match cmd {
        "layer.precompose" if params.get("name").is_none() => return super::dialogs::request(app, Some(card), super::dialogs::Which::Precompose),
        "comp.settings" => return open_comp_settings(app, ctx, card, params.get("comp").and_then(Value::as_u64)),
        _ => {}
    }
    let p = app.effectcraft.panel.as_mut().ok_or("The EffectCraft Timeline closed")?;
    if cmd == "comp.copyFrame" {
        return copy_frame(&mut p.host, ctx, params);
    }
    // New ▸ Text puts the caret in the comp, so typing goes into the text (EffectCraft's menus).
    let params =
        if cmd == "layer.newText" && params.as_object().is_some_and(serde_json::Map::is_empty) { json!({"text": "", "edit": true}) } else { params };
    // Each entry is an undo step of its own.
    p.host.session_mut().history.merge_key = None;
    let host = &mut p.host;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.invoke(ctx, cmd, params)))
        .unwrap_or_else(|_| Err(format!("EffectCraft: {cmd} failed (an internal error)")))
        .map(|_| ())
}

/// Copy Frame to Clipboard: the frame EffectCraft renders, onto the system clipboard.
fn copy_frame(host: &mut EcHost, ctx: &egui::Context, params: Value) -> Result<(), String> {
    host.session_mut().execute("comp.copyFrame", params).map_err(|e| e.to_string())?;
    let mut copied = false;
    for ev in host.session_mut().drain_events() {
        match ev {
            EcEvent::CopyImage { width, height, rgba } => {
                let (w, h) = (width as usize, height as usize);
                if w > 0 && h > 0 && w.checked_mul(h).and_then(|n| n.checked_mul(4)) == Some(rgba.len()) {
                    ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba));
                    copied = true;
                }
            }
            EcEvent::Toast { message, .. } => host.ui_mut().status = message,
            _ => {}
        }
    }
    if copied { Ok(()) } else { Err("EffectCraft couldn't copy the frame".into()) }
}

/// Composition Settings… for comp `comp` (the Timeline's when `None`): EffectCraft's dialog with
/// the comp's settings.
fn open_comp_settings(app: &FigApp, ctx: &egui::Context, card: NodeId, comp: Option<u64>) -> Result<(), String> {
    let p = app.effectcraft.panel.as_ref().filter(|p| p.card == card).ok_or("The EffectCraft Timeline closed")?;
    let comp = comp.unwrap_or_else(|| p.host.comp());
    let draft = comp_settings::draft_of(&p.host.session().project, ItemId(comp)).ok_or("That composition is gone")?;
    ctx.data_mut(|d| d.insert_temp(dialog_id(), CompDialog { card, comp, draft }));
    Ok(())
}

/// Composition Settings' OK: `params` (`comp_settings::params`) applied to comp `comp` through
/// the Timeline's host, one undo step.
pub(crate) fn apply_comp_settings(app: &mut FigApp, ctx: &egui::Context, card: NodeId, comp: u64, mut params: Value) -> Result<(), String> {
    let p = app.effectcraft.panel.as_mut().filter(|p| p.card == card).ok_or("The EffectCraft Timeline closed")?;
    let o = params.as_object_mut().ok_or("Composition Settings: bad settings")?;
    o.insert("comp".into(), json!(comp));
    p.host.session_mut().history.merge_key = None;
    let host = &mut p.host;
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.invoke(ctx, "comp.settings", params)))
        .unwrap_or_else(|_| Err("EffectCraft: Composition Settings failed (an internal error)".into()))
        .map(|_| ())
}

/// One frame of Composition Settings (EffectCraft's own, `comp_settings::show`).
fn comp_settings_dialog(app: &mut FigApp, ctx: &egui::Context) {
    let Some(mut dlg) = ctx.data(|d| d.get_temp::<CompDialog>(dialog_id())) else { return };
    let Some(p) = app.effectcraft.panel.as_ref().filter(|p| p.card == dlg.card) else {
        ctx.data_mut(|d| d.remove::<CompDialog>(dialog_id()));
        return;
    };
    let custom = p.host.session().prefs.comp_presets.clone();
    let t = Tokens::for_kind(ThemeKind::Dark);
    let mut auto = Registry::default();
    let mut cmds: Vec<(&'static str, Value)> = Vec::new();
    let mut result = None;
    let frame = egui::Frame::window(&ctx.style_of(ctx.theme())).fill(t.panel_bg).inner_margin(egui::Margin::same(18));
    let shown = egui::Modal::new(dialog_id().with("modal")).frame(frame).show(ctx, |ui| {
        ui.set_max_width(620.0);
        ui.label(egui::RichText::new("Composition Settings").strong().size(14.0));
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
    if let Some(p) = app.effectcraft.panel.as_mut() {
        for (id, params) in cmds {
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
            d.remove::<CompDialog>(dialog_id());
        }),
        Some(params) => {
            ctx.data_mut(|d| d.remove::<CompDialog>(dialog_id()));
            if let Err(e) = apply_comp_settings(app, ctx, dlg.card, dlg.comp, params) {
                app.toast(e);
            }
        }
    }
}

/// The card's picture on screen (tests).
#[cfg(test)]
pub(crate) fn picture_of(app: &FigApp) -> Option<egui::Rect> {
    let p = app.effectcraft.panel.as_ref()?;
    super::viewer::picture(app, p.card, &p.host).map(|(r, _)| r)
}

#[cfg(test)]
#[path = "effectcraft_menu_tests.rs"]
mod tests;

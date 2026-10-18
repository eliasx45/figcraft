//! The floating bottom toolbar (UI3): tool groups with flyouts, comments, actions.

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use figcraft_engine::ToolId;
use figcraft_engine::vectoredit::VTool;
use serde_json::json;

use crate::FigApp;
use crate::a11y::{self, Nav, Region, Tip};
use crate::i18n::{tr, tr_noop, trf};
use crate::icons::{self, Icon};
use crate::theme::{ACCENT, Tokens, regular};

struct Group {
    key: &'static str,
    /// The flyout button's name for screen readers.
    name: &'static str,
    tools: &'static [(ToolId, Icon)],
}

const GROUPS: &[Group] = &[
    Group {
        key: "move",
        name: tr_noop("Move tools"),
        tools: &[(ToolId::Move, Icon::Move), (ToolId::Hand, Icon::Hand), (ToolId::Scale, Icon::Scale)],
    },
    Group {
        key: "frame",
        name: tr_noop("Frame tools"),
        tools: &[(ToolId::Frame, Icon::Frame), (ToolId::Section, Icon::Section), (ToolId::Slice, Icon::Slice)],
    },
    Group {
        key: "shape",
        name: tr_noop("Shape tools"),
        tools: &[
            (ToolId::Rectangle, Icon::Rectangle),
            (ToolId::Line, Icon::Line),
            (ToolId::Arrow, Icon::Arrow),
            (ToolId::Ellipse, Icon::Ellipse),
            (ToolId::Polygon, Icon::Polygon),
            (ToolId::Star, Icon::Star),
        ],
    },
    Group {
        key: "pen",
        name: tr_noop("Drawing tools"),
        tools: &[(ToolId::Pen, Icon::Pen), (ToolId::Pencil, Icon::Pencil), (ToolId::ShapeBuilder, Icon::ShapeBuilder)],
    },
    Group { key: "text", name: tr_noop("Text"), tools: &[(ToolId::Text, Icon::Text)] },
    Group { key: "comment", name: tr_noop("Comment"), tools: &[(ToolId::Comment, Icon::Comment)] },
];

const DROPDOWN_W: f32 = 16.0;

fn remembered(ui: &Ui, key: &str, default: ToolId) -> ToolId {
    ui.data(|d| d.get_temp::<u8>(egui::Id::new(("tb_last", key)))).and_then(|i| ToolId::ALL.get(i as usize).copied()).unwrap_or(default)
}

pub fn show(app: &mut FigApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let canvas = app.canvas.rect;
    app.ui.actions_rect = None;
    // A file you can't edit now (view only, or on its way to a server): moving around only.
    if let Some(why) = app.session.active_read_only() {
        view_only_toolbar(app, ui, why);
        return;
    }
    // Vector edit mode swaps the tools for the edit tools.
    if let Some(tool) = app.session.ds().ok().and_then(|d| d.vector_edit.as_ref()).map(|v| v.tool) {
        edit_toolbar(app, ui, tool);
        return;
    }
    // So does the shape builder, for its merge/remove modes.
    if app.session.tool.tool == ToolId::ShapeBuilder {
        builder_toolbar(app, ui);
        return;
    }
    main_toolbar(app, ui, canvas, &t);
}

/// The main toolbar pill's width: the Actions palette matches it so the two read as one bar.
pub(crate) fn main_width() -> f32 {
    let (cell, gap) = (32.0, 4.0);
    GROUPS.len() as f32 * (cell + gap)
        + GROUPS.iter().filter(|g| g.tools.len() > 1).count() as f32 * DROPDOWN_W
        + 2.0 * (cell + gap)
        + 8.0
        + (4.0 * 34.0 + 4.0)
        + 16.0
}

fn main_toolbar(app: &mut FigApp, ui: &mut Ui, canvas: Rect, t: &Tokens) {
    let (cell, gap) = (32.0, 4.0);
    let width = main_width();
    let rect = pill(ui, &mut app.ui.nav, canvas, width, t);
    let current = app.session.tool.tool;
    let mut x = rect.min.x + 8.0;
    let cy = rect.center().y;
    for g in GROUPS {
        let in_group = g.tools.iter().any(|(tid, _)| *tid == current);
        let shown = if in_group { current } else { remembered(ui, g.key, g.tools.first().map(|x| x.0).unwrap_or(ToolId::Move)) };
        let icon = g.tools.iter().find(|(tid, _)| *tid == shown).map(|(_, i)| *i).unwrap_or(Icon::Move);
        let r = Rect::from_center_size(pos2(x + cell / 2.0, cy), vec2(cell, cell));
        let resp = tool_cell(ui, t, r, ("tool", g.key), icon, in_group, &format!("{}  {}", tr(shown.label()), pretty_shortcut(shown.shortcut())));
        if in_group {
            a11y::prefer(&mut app.ui.nav, Region::Toolbar, resp.id);
        }
        if resp.clicked() {
            app.ui.tool_flyout = None;
            // Enter (a keyboard click) on the shape or frame tool already chosen places one; the
            // canvas takes the keyboard.
            if in_group && shown.draws_shape() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                app.run("tool.place", json!({}));
                app.ui.nav.go(Region::Canvas);
            } else {
                app.run("tool.set", json!({"tool": shown.id()}));
            }
        }
        x += cell;
        // Flyout chevron.
        if g.tools.len() > 1 {
            let cr = Rect::from_center_size(pos2(x + DROPDOWN_W / 2.0, cy), vec2(DROPDOWN_W, cell));
            let cresp = ui.interact(cr, ui.id().with(("flyout", g.key)), Sense::click());
            let open = app.ui.tool_flyout == Some(g.key);
            a11y::menu_button(&cresp, tr(g.name), open);
            if open {
                a11y::popup(&mut app.ui.nav, cresp.id, flyout_id(g.key));
            }
            if cresp.hovered() {
                ui.painter().rect_filled(cr, CornerRadius::same(4), t.hover);
            }
            icons::paint(ui.painter(), Rect::from_center_size(cr.center(), vec2(12.0, 12.0)), Icon::Chevron, t.icon_secondary);
            if cresp.clicked() {
                app.ui.tool_flyout = if app.ui.tool_flyout == Some(g.key) { None } else { Some(g.key) };
            }
            x += DROPDOWN_W;
        }
        let anchor = Rect::from_min_max(r.min, pos2(x, r.max.y));
        x += gap;
        if app.ui.tool_flyout == Some(g.key) {
            flyout(app, ui, g, anchor, rect.min.y - 8.0);
        }
    }
    // Resources (components, plugins) opens the Assets panel; Actions palette.
    for (icon, tip, id) in [
        (Icon::Assets, crate::shortcuts::key_tip(tr("Resources"), "Shift+I"), "resources"),
        (Icon::Actions, crate::shortcuts::tip(tr("Actions"), "app.actions"), "actions"),
    ] {
        let r = Rect::from_center_size(pos2(x + cell / 2.0, cy), vec2(cell, cell));
        let resp = ui.interact(r, ui.id().with(id), Sense::click()).tip(tip);
        if id == "actions" {
            app.ui.actions_rect = Some(r);
        }
        // The Actions button reads on while its panel is open; clicking it again closes it.
        let actions_open = id == "actions" && app.ui.palette.is_some();
        if actions_open {
            ui.painter().rect_filled(r, CornerRadius::same(5), ACCENT);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(5), t.hover);
        }
        icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(20.0, 20.0)), icon, if actions_open { Color32::WHITE } else { t.icon });
        if resp.clicked() {
            if id == "actions" {
                crate::menus::toggle_palette(&mut app.ui);
            } else {
                app.run("view.assets", json!({}));
            }
        }
        x += cell + 4.0;
    }
    divider(ui, t, x, cy);
    x += 8.0;
    // Mode switcher: Draw · Design · Prototype · Inspect (code).
    let modes = [(Icon::Pencil, tr("Draw")), (Icon::Frame, tr("Design")), (Icon::Prototype, tr("Prototype")), (Icon::Code, tr("Inspect"))];
    let active = match app.ui.right_tab {
        crate::RightTab::Design if app.session.tool.tool == ToolId::Pencil => 0,
        crate::RightTab::Design => 1,
        crate::RightTab::Prototype => 2,
        crate::RightTab::Inspect => 3,
    };
    let seg = Rect::from_min_size(pos2(x, cy - 18.0), vec2(4.0 * 34.0 + 4.0, 36.0));
    ui.painter().rect_filled(seg, CornerRadius::same(8), t.field);
    for (i, (icon, tip)) in modes.iter().enumerate() {
        let r = Rect::from_min_size(pos2(seg.min.x + 2.0 + i as f32 * 34.0, seg.min.y + 2.0), vec2(34.0, 32.0));
        let resp = ui.interact(r, ui.id().with(("mode", i)), Sense::click()).on_hover_text(*tip);
        a11y::toggle(&resp, tip, i == active);
        if i == active {
            ui.painter().rect_filled(r, CornerRadius::same(6), t.panel);
            ui.painter().rect_stroke(r, CornerRadius::same(6), Stroke::new(1.0, t.border), StrokeKind::Inside);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(6), t.hover);
        }
        icons::paint(
            ui.painter(),
            Rect::from_center_size(r.center(), vec2(18.0, 18.0)),
            *icon,
            if i == active { t.accent_text } else { t.icon_secondary },
        );
        if resp.clicked() {
            match i {
                0 => {
                    app.ui.right_tab = crate::RightTab::Design;
                    app.run("tool.set", json!({"tool": "pencil"}));
                }
                1 => {
                    app.ui.right_tab = crate::RightTab::Design;
                    if app.session.tool.tool == ToolId::Pencil {
                        app.run("tool.set", json!({"tool": "move"}));
                    }
                }
                2 => app.ui.right_tab = crate::RightTab::Prototype,
                _ => app.ui.right_tab = crate::RightTab::Inspect,
            }
        }
    }
    // Floating help button at the bottom right of the canvas.
    let hr = Rect::from_center_size(pos2(canvas.max.x - 32.0, canvas.max.y - 32.0), vec2(32.0, 32.0));
    let hresp = ui.interact(hr, ui.id().with("help"), Sense::click()).tip(tr("Help and resources"));
    ui.painter().circle_filled(hr.center() + vec2(0.0, 1.0), 17.0, Color32::from_black_alpha(20));
    ui.painter().circle_filled(hr.center(), 16.0, t.panel);
    ui.painter().text(hr.center(), Align2::CENTER_CENTER, "?", crate::theme::regular(15.0), t.text);
    crate::right::menu_popup(&hresp).show(|ui| {
        for (l, c) in [
            (tr("Getting started"), "help.gettingStarted"),
            (tr("Keyboard shortcuts"), "help.shortcuts"),
            (tr("Help page"), "help.page"),
            (tr("About FigCraft"), "help.about"),
        ] {
            if crate::right::menu_item(ui, l, crate::shortcuts::shortcut(c).unwrap_or("")) {
                app.run(c, json!({}));
                ui.close();
            }
        }
    });
}

/// The pill shown for a file you can't edit now: Move and Hand, and a badge saying why ("View
/// only", or "Publishing…" until the file is on the server).
fn view_only_toolbar(app: &mut FigApp, ui: &mut Ui, why: &'static str) {
    let t = Tokens::get(ui.ctx());
    // The editing tools are off: the tool falls back to moving around.
    if !matches!(app.session.tool.tool, ToolId::Move | ToolId::Hand) {
        app.run("tool.set", json!({"tool": "move"}));
    }
    let label = if why == figcraft_engine::live::PUBLISHING { tr("Publishing…") } else { tr("View only") };
    let cell = 32.0;
    let text = ui.painter().layout_no_wrap(label.to_string(), crate::theme::semibold(11.5), t.text);
    let badge_w = 12.0 + 14.0 + 6.0 + text.size().x + 12.0;
    let width = 2.0 * (cell + 4.0) + 8.0 + 12.0 + badge_w + 8.0;
    let rect = pill(ui, &mut app.ui.nav, app.canvas.rect, width, &t);
    let cy = rect.center().y;
    let mut x = rect.min.x + 8.0;
    for (tool, icon) in [(ToolId::Move, Icon::Move), (ToolId::Hand, Icon::Hand)] {
        let r = Rect::from_center_size(pos2(x + cell / 2.0, cy), vec2(cell, cell));
        let active = app.session.tool.tool == tool;
        let resp = tool_cell(ui, &t, r, ("tool", tool.id()), icon, active, &format!("{}  {}", tr(tool.label()), pretty_shortcut(tool.shortcut())));
        if active {
            a11y::prefer(&mut app.ui.nav, Region::Toolbar, resp.id);
        }
        if resp.clicked() {
            app.run("tool.set", json!({"tool": tool.id()}));
        }
        x += cell + 4.0;
    }
    divider(ui, &t, x, cy);
    let badge = Rect::from_min_size(pos2(x + 12.0, cy - 13.0), vec2(badge_w, 26.0));
    let resp = ui.interact(badge, ui.id().with("view_only"), Sense::hover()).on_hover_text(why);
    a11y::status(&resp, label, why);
    ui.painter().rect_filled(badge, CornerRadius::same(13), t.field);
    icons::paint(ui.painter(), Rect::from_center_size(pos2(badge.min.x + 12.0 + 7.0, cy), vec2(14.0, 14.0)), Icon::Eye, t.icon_secondary);
    ui.painter().galley(pos2(badge.min.x + 12.0 + 14.0 + 6.0, cy - text.size().y / 2.0), text, t.text);
}

/// The pill shown in vector edit mode: the edit tools and Done.
fn edit_toolbar(app: &mut FigApp, ui: &mut Ui, current: VTool) {
    let t = Tokens::get(ui.ctx());
    let canvas = app.canvas.rect;
    let cell = 32.0;
    let width = VTool::ALL.len() as f32 * (cell + 4.0) + 8.0 + 12.0 + DONE_W + 8.0;
    let rect = pill(ui, &mut app.ui.nav, canvas, width, &t);
    let cy = rect.center().y;
    let mut x = rect.min.x + 8.0;
    // Holding Cmd with the Move tool bends: show it.
    let held = current == VTool::Move && ui.input(|i| i.modifiers.command);
    for tool in VTool::ALL {
        let r = Rect::from_center_size(pos2(x + cell / 2.0, cy), vec2(cell, cell));
        let tip = if tool == VTool::Bend {
            trf("{tool}  hold {key}", &[("tool", &tr(tool.label())), ("key", &pretty_shortcut(tool.shortcut()))])
        } else {
            format!("{}  {}", tr(tool.label()), pretty_shortcut(tool.shortcut()))
        };
        let active = tool == current && !held || held && tool == VTool::Bend;
        let icon = match tool {
            VTool::Move => Icon::Move,
            VTool::Pen => Icon::Pen,
            VTool::Bend => Icon::Bend,
            VTool::Paint => Icon::PaintBucket,
            VTool::Lasso => Icon::Lasso,
            VTool::Cut => Icon::Cut,
            VTool::Eraser => Icon::Eraser,
            VTool::Width => Icon::VariableWidth,
        };
        let resp = tool_cell(ui, &t, r, ("vtool", tool.id()), icon, active, &tip);
        if active {
            a11y::prefer(&mut app.ui.nav, Region::Toolbar, resp.id);
        }
        if resp.clicked() {
            app.run("vector.edit.tool", json!({"tool": tool.id()}));
        }
        x += cell + 4.0;
    }
    if done_button(ui, &t, x, cy, &format!("{}  Esc", tr("Done editing"))) {
        app.run("vector.edit.end", json!({}));
    }
}

/// The pill shown while the shape builder is active: Merge and Remove modes, a hint and Done.
fn builder_toolbar(app: &mut FigApp, ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let cell = 32.0;
    let remove = app.session.tool.builder_remove;
    let has_regions = figcraft_engine::shapebuilder::current(&app.session).is_some_and(|a| !a.regions.is_empty());
    let alt = pretty_shortcut("Alt");
    let hint = if !has_regions {
        tr("Select overlapping shapes").to_string()
    } else if remove {
        trf("Click or drag to remove · {alt} merges", &[("alt", &alt)])
    } else {
        trf("Click or drag to merge · {alt} removes", &[("alt", &alt)])
    };
    let galley = ui.painter().layout_no_wrap(hint, regular(12.0), t.text_secondary);
    let width = 2.0 * (cell + 4.0) + 8.0 + 12.0 + galley.size().x + 12.0 + 12.0 + DONE_W + 8.0;
    let rect = pill(ui, &mut app.ui.nav, app.canvas.rect, width, &t);
    let cy = rect.center().y;
    let mut x = rect.min.x + 8.0;
    for (mode, icon, tip) in [("merge", Icon::ShapeBuilder, tr("Merge regions")), ("remove", Icon::ShapeBuilderRemove, tr("Remove regions"))] {
        let r = Rect::from_center_size(pos2(x + cell / 2.0, cy), vec2(cell, cell));
        let active = (mode == "remove") == remove;
        let resp = tool_cell(ui, &t, r, ("sbmode", mode), icon, active, tip);
        if active {
            a11y::prefer(&mut app.ui.nav, Region::Toolbar, resp.id);
        }
        if resp.clicked() {
            app.run("shapeBuilder.mode", json!({"mode": mode}));
        }
        x += cell + 4.0;
    }
    divider(ui, &t, x, cy);
    x += 12.0;
    let w = galley.size().x;
    ui.painter().galley(pos2(x, cy - galley.size().y / 2.0), galley, t.text_secondary);
    x += w + 12.0;
    if done_button(ui, &t, x, cy, &format!("{}  {}", tr("Done"), pretty_shortcut("Enter"))) {
        app.run("tool.set", json!({"tool": "move"}));
    }
}

/// Where a toolbar pill `width` wide sits: centred 12 px above the canvas's bottom edge.
pub(crate) fn pill_rect(canvas: Rect, width: f32) -> Rect {
    Rect::from_center_size(pos2(canvas.center().x, canvas.max.y - 12.0 - 24.0), vec2(width, 48.0))
}

/// The floating pill (shadow, panel, border) `width` wide at the bottom centre of the canvas: the
/// toolbar region keyboard navigation moves through.
fn pill(ui: &Ui, nav: &mut Nav, canvas: Rect, width: f32, t: &Tokens) -> Rect {
    let rect = pill_rect(canvas, width);
    // The pill takes the press and the drag over it: its buttons sense clicks only, and egui gives
    // a drag the canvas under them would sense to the canvas, so the pen placed an anchor there.
    // Not focusable: the keyboard walks the buttons, not the pill.
    ui.interact(rect, ui.id().with("pill"), Sense::CLICK | Sense::DRAG);
    a11y::area(nav, Region::Toolbar, rect);
    a11y::container_bounds(ui, rect);
    let painter = ui.painter();
    for (i, a) in [(6.0, 10u8), (3.0, 16), (1.0, 24)] {
        painter.rect_filled(rect.expand(i).translate(vec2(0.0, i * 0.5)), CornerRadius::same((13.0 + i) as u8), Color32::from_black_alpha(a));
    }
    painter.rect_filled(rect, CornerRadius::same(13), t.panel);
    painter.rect_stroke(rect, CornerRadius::same(13), Stroke::new(1.0, t.border), StrokeKind::Inside);
    rect
}

/// A square tool button: accent when active, tinted on hover; `tip` is its tooltip and name.
fn tool_cell(ui: &mut Ui, t: &Tokens, r: Rect, id: impl egui::AsIdSalt, icon: Icon, active: bool, tip: &str) -> egui::Response {
    let resp = ui.interact(r, ui.id().with(id), Sense::click()).on_hover_ui(|ui| {
        ui.label(tip);
    });
    a11y::toggle(&resp, tip, active);
    let bg = if active {
        ACCENT
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, CornerRadius::same(5), bg);
    icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(20.0, 20.0)), icon, if active { Color32::WHITE } else { t.icon });
    resp
}

/// A vertical separator just after `x`.
fn divider(ui: &Ui, t: &Tokens, x: f32, cy: f32) {
    ui.painter().line_segment([pos2(x + 2.0, cy - 14.0), pos2(x + 2.0, cy + 14.0)], Stroke::new(1.0, t.border));
}

const DONE_W: f32 = 56.0;

/// A divider at `x` and the accent Done button after it; true when clicked.
fn done_button(ui: &mut Ui, t: &Tokens, x: f32, cy: f32, tip: &str) -> bool {
    divider(ui, t, x, cy);
    let done = Rect::from_min_size(pos2(x + 12.0, cy - 14.0), vec2(DONE_W, 28.0));
    let resp = ui.interact(done, ui.id().with("tool_done"), Sense::click()).tip(tip);
    ui.painter().rect_filled(done, CornerRadius::same(6), if resp.hovered() { t.accent_hover } else { ACCENT });
    ui.painter().text(done.center(), Align2::CENTER_CENTER, tr("Done"), crate::theme::semibold(11.5), Color32::WHITE);
    resp.clicked()
}

fn flyout(app: &mut FigApp, ui: &mut Ui, g: &Group, anchor: Rect, bottom: f32) {
    let t = Tokens::get(ui.ctx());
    let item_h = 32.0;
    let screen = ui.ctx().content_rect().shrink(8.0);
    let width = g.tools.iter().fold(220.0_f32, |w, (tid, _)| {
        let label = ui.painter().layout_no_wrap(tr(tid.label()).to_string(), regular(12.0), t.menu_text);
        let key = ui.painter().layout_no_wrap(pretty_shortcut(tid.shortcut()), regular(12.0), t.menu_dim);
        w.max(label.size().x + key.size().x + 96.0)
    });
    let w = width.min(screen.width().max(1.0));
    let h = item_h * g.tools.len() as f32 + 16.0;
    let pos = pos2(
        anchor.min.x.clamp(screen.min.x, (screen.max.x - w).max(screen.min.x)),
        (bottom - h).clamp(screen.min.y, (screen.max.y - h).max(screen.min.y)),
    );
    let mut chosen = None;
    let popup = egui::Area::new(flyout_id(g.key)).fixed_pos(pos).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
        ui.painter()
            .add(egui::Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(80) }.as_shape(rect, CornerRadius::same(13)));
        ui.painter().rect_filled(rect, CornerRadius::same(13), t.menu);
        ui.painter().rect_stroke(rect, CornerRadius::same(13), Stroke::new(1.0, t.border), StrokeKind::Inside);
        let mut y = rect.min.y + 8.0;
        for (tid, icon) in g.tools {
            let r = Rect::from_min_size(pos2(rect.min.x + 8.0, y), vec2(w - 16.0, item_h));
            let resp = ui.interact(r, ui.id().with(("fly", tid.id())), Sense::click());
            a11y::menu_item(&resp, tr(tid.label()), tid.shortcut(), Some(*tid == app.session.tool.tool));
            // The keyboard's item is highlighted like the hovered one.
            a11y::own_focus(&resp);
            if resp.hovered() || resp.has_focus() {
                ui.painter().rect_filled(r, CornerRadius::same(5), t.menu_hover);
            }
            if *tid == app.session.tool.tool {
                icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 10.0, r.center().y), vec2(12.0, 12.0)), Icon::Check, t.menu_text);
            }
            icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 32.0, r.center().y), vec2(18.0, 18.0)), *icon, t.menu_text);
            let key = pretty_shortcut(tid.shortcut());
            let key_width = ui.painter().layout_no_wrap(key.clone(), regular(12.0), t.menu_dim).size().x;
            let label = crate::widgets::elide_font(ui, tr(tid.label()), (r.width() - key_width - 80.0).max(1.0), regular(12.0));
            ui.painter().text(pos2(r.min.x + 48.0, r.center().y), Align2::LEFT_CENTER, label, regular(12.0), t.menu_text);
            ui.painter().text(pos2(r.max.x - 8.0, r.center().y), Align2::RIGHT_CENTER, key, regular(12.0), t.menu_dim);
            if resp.clicked() {
                chosen = Some(*tid);
            }
            y += item_h;
        }
    });
    if ui.input(|i| {
        i.key_pressed(egui::Key::Escape)
            || (i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !popup.response.rect.contains(p) && !anchor.contains(p)))
    }) {
        app.ui.tool_flyout = None;
    }
    if let Some(tid) = chosen {
        if let Some(i) = ToolId::ALL.iter().position(|x| *x == tid) {
            ui.data_mut(|d| d.insert_temp(egui::Id::new(("tb_last", g.key)), i as u8));
        }
        app.run("tool.set", json!({"tool": tid.id()}));
        app.ui.tool_flyout = None;
    }
}

/// A tool group's flyout (an egui area).
fn flyout_id(key: &str) -> egui::Id {
    egui::Id::new(("flyout_area", key))
}

/// Platform-specific, readable key names shared by every shortcut surface.
pub fn pretty_shortcut(s: &str) -> String {
    figcraft_engine::keymap::display(s, crate::shortcuts::platform())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layers_drag_tests::Ui as Harness;

    #[test]
    fn mouse_opens_tool_menu_and_a_panel_click_dismisses_it() {
        let mut h = Harness::new(figcraft_engine::Session::new());
        h.ctx.enable_accesskit();
        h.frame(vec![]);
        let out = h.frame_output(vec![]);
        let tree = out.platform_output.accesskit_update.unwrap();
        let button = tree.nodes.iter().map(|(_, n)| n).find(|n| n.label() == Some("Move tools")).unwrap();
        let b = button.bounds().unwrap();
        let at = pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
        h.button(at, true);
        h.button(at, false);
        h.frame(vec![]);
        assert_eq!(h.app.ui.tool_flyout, Some("move"), "the opening click must not dismiss its menu");
        h.button(pos2(1410.0, 780.0), true);
        h.button(pos2(1410.0, 780.0), false);
        h.frame(vec![]);
        assert_eq!(h.app.ui.tool_flyout, None, "a click outside the canvas also dismisses the menu");
        assert_eq!(h.app.session.tool.tool, ToolId::Move);
    }

    #[test]
    fn tool_menu_stays_inside_small_and_large_windows() {
        for size in [vec2(800.0, 500.0), vec2(1440.0, 900.0)] {
            let ctx = egui::Context::default();
            let mut app = FigApp::new(figcraft_engine::Session::new(), Box::new(crate::NoServices));
            app.ui.tool_flyout = Some("shape");
            for i in 0..4 {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                        time: Some(i as f64 / 60.0),
                        ..Default::default()
                    },
                    |ui| app.ui(ui),
                )
                .textures_delta
                .clear();
            }
            let menu = ctx.memory(|m| m.area_rect(flyout_id("shape"))).unwrap();
            assert!(ctx.content_rect().contains_rect(menu), "menu escapes window: {menu:?} at {size:?}");
        }
    }
}
